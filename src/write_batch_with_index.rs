use crate::db::DBInner;
use crate::ffi_util::raw_data_and_free;
use crate::write_batch::get_ts_size_callback;
use crate::{
    AsColumnFamilyRef, Comparator, DBAccess, DBCommon, DBPinnableSlice,
    DBRawIteratorWithThreadMode, Error, Options, ReadOptions, ThreadMode, ffi,
};
use libc::{c_char, c_uchar, c_void, size_t};
use std::sync::Arc;

/// A write batch that can also be read from, and that can be layered on top of
/// a database iterator.
///
/// There is deliberately no vectored write here, unlike
/// [`WriteBatch::put_vectored`](crate::WriteBatch::put_vectored).
/// `WriteBatchWithIndex` does not override the `SliceParts` overloads, so
/// `rocksdb_writebatch_wi_putv` and friends fall through to
/// `WriteBatchBase`, which concatenates the parts into a temporary
/// `std::string` and calls the single-slice path anyway. Joining the parts in
/// Rust costs the same copy and lets the caller reuse the buffer.
///
/// Values read out of the batch are copied, but iterators and pinned slices
/// borrow, so the borrow checker is what keeps them from outliving their owner.
///
/// An iterator built with [`Self::iterator_with_base`] reads directly out of the
/// batch's internal skip-list, so it cannot outlive the batch:
///
/// ```compile_fail,E0597
/// use rust_rocksdb::{DB, WriteBatchWithIndex};
///
/// let db = DB::open_default("foo").unwrap();
/// let mut iter = {
///     let mut wbwi = WriteBatchWithIndex::new(0, true);
///     wbwi.put(b"k", b"v");
///     wbwi.iterator_with_base(db.raw_iterator())
/// };
/// iter.seek_to_first();
/// ```
///
/// A slice from [`Self::get_pinned_from_batch_and_db`] pins a block in the
/// database's block cache, so it cannot outlive the database:
///
/// ```compile_fail,E0597
/// use rust_rocksdb::{DB, ReadOptions, WriteBatchWithIndex};
///
/// let wbwi = WriteBatchWithIndex::new(0, true);
/// let readopts = ReadOptions::default();
/// let _value = {
///     let db = DB::open_default("foo").unwrap();
///     wbwi.get_pinned_from_batch_and_db(&db, b"k", &readopts).unwrap()
/// };
/// ```
pub struct WriteBatchWithIndex {
    pub(crate) inner: *mut ffi::rocksdb_writebatch_wi_t,
    /// RocksDB stores the comparator by pointer and never takes ownership, so
    /// the batch has to keep it alive for as long as its index exists.
    _comparator: Option<Arc<Comparator>>,
}

/// How a batch built by [`WriteBatchWithIndex::builder`] is indexed and bounded.
///
/// Every field is optional. The defaults match
/// [`WriteBatchWithIndex::new`] with `overwrite_key` set to false.
pub struct WriteBatchWithIndexBuilder {
    comparator: Option<Arc<Comparator>>,
    reserved_bytes: usize,
    overwrite_key: bool,
    max_bytes: usize,
    protection_bytes_per_key: usize,
}

impl WriteBatchWithIndexBuilder {
    /// Orders the index by this comparator instead of by bytewise order.
    ///
    /// This must be the comparator of the column family the batch is read
    /// against, otherwise reads through the batch return the wrong entries. The
    /// batch keeps a reference, so the comparator cannot be dropped early.
    pub fn comparator(mut self, comparator: Arc<Comparator>) -> Self {
        self.comparator = Some(comparator);
        self
    }

    /// Preallocates this many bytes for the batch's serialized form.
    pub fn reserved_bytes(mut self, reserved_bytes: usize) -> Self {
        self.reserved_bytes = reserved_bytes;
        self
    }

    /// Makes a later write to a key replace the earlier one in the index.
    ///
    /// With this off the index keeps every version, iteration sees all of them,
    /// and reads return the newest. With it on the batch holds one entry per
    /// key, which is what a transaction wants. Merge operands are still kept in
    /// full either way.
    pub fn overwrite_key(mut self, overwrite_key: bool) -> Self {
        self.overwrite_key = overwrite_key;
        self
    }

    /// Fails writes once the batch's serialized size would exceed this many
    /// bytes. Zero means no limit.
    pub fn max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Stores this many bytes of per-key checksum alongside each entry so
    /// RocksDB can detect memory corruption in the batch before it is written.
    ///
    /// Only 0 and 8 are supported. Zero disables the protection.
    pub fn protection_bytes_per_key(mut self, protection_bytes_per_key: usize) -> Self {
        self.protection_bytes_per_key = protection_bytes_per_key;
        self
    }

    /// Creates the batch.
    pub fn build(self) -> WriteBatchWithIndex {
        let comparator_ptr = self
            .comparator
            .as_ref()
            .map_or(std::ptr::null_mut(), |cmp| cmp.inner.as_ptr());
        WriteBatchWithIndex {
            inner: unsafe {
                ffi::rocksdb_writebatch_wi_create_with_params(
                    comparator_ptr,
                    self.reserved_bytes,
                    c_uchar::from(self.overwrite_key),
                    self.max_bytes,
                    self.protection_bytes_per_key,
                )
            },
            _comparator: self.comparator,
        }
    }
}

impl WriteBatchWithIndex {
    pub fn new(reserved_bytes: usize, overwrite_key: bool) -> Self {
        Self {
            inner: unsafe {
                ffi::rocksdb_writebatch_wi_create(
                    reserved_bytes as size_t,
                    c_uchar::from(overwrite_key),
                )
            },
            _comparator: None,
        }
    }

    /// Starts building a batch with a custom comparator, size cap, or per-key
    /// checksums.
    ///
    /// ```
    /// use rust_rocksdb::WriteBatchWithIndex;
    ///
    /// let mut batch = WriteBatchWithIndex::builder()
    ///     .overwrite_key(true)
    ///     .max_bytes(1 << 20)
    ///     .protection_bytes_per_key(8)
    ///     .build();
    /// batch.put(b"k", b"v");
    /// assert_eq!(batch.len(), 1);
    /// ```
    pub fn builder() -> WriteBatchWithIndexBuilder {
        WriteBatchWithIndexBuilder {
            comparator: None,
            reserved_bytes: 0,
            overwrite_key: false,
            max_bytes: 0,
            protection_bytes_per_key: 0,
        }
    }

    pub fn len(&self) -> usize {
        unsafe { ffi::rocksdb_writebatch_wi_count(self.inner) as usize }
    }

    /// Return WriteBatch serialized size (in bytes).
    pub fn size_in_bytes(&self) -> usize {
        unsafe {
            let mut batch_size: size_t = 0;
            ffi::rocksdb_writebatch_wi_data(self.inner, &raw mut batch_size);
            batch_size
        }
    }

    /// Return a reference to a byte array which represents a serialized version of the batch.
    pub fn data(&self) -> &[u8] {
        unsafe {
            let mut batch_size: size_t = 0;
            let batch_data = ffi::rocksdb_writebatch_wi_data(self.inner, &raw mut batch_size);
            std::slice::from_raw_parts(batch_data as _, batch_size)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get_from_batch<K>(&self, key: K, options: &Options) -> Result<Option<Vec<u8>>, Error>
    where
        K: AsRef<[u8]>,
    {
        let key = key.as_ref();
        unsafe {
            let mut value_size: size_t = 0;
            let value_data = ffi_try!(ffi::rocksdb_writebatch_wi_get_from_batch(
                self.inner,
                options.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                &raw mut value_size
            ));

            // `value_data` was allocated by `malloc` on the C++ side; copy it
            // out and release it with `rocksdb_free`.
            Ok(raw_data_and_free(value_data, value_size))
        }
    }

    pub fn get_from_batch_cf<K>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        options: &Options,
    ) -> Result<Option<Vec<u8>>, Error>
    where
        K: AsRef<[u8]>,
    {
        let key = key.as_ref();
        unsafe {
            let mut value_size: size_t = 0;
            let value_data = ffi_try!(ffi::rocksdb_writebatch_wi_get_from_batch_cf(
                self.inner,
                options.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                &raw mut value_size
            ));

            // `value_data` was allocated by `malloc` on the C++ side; copy it
            // out and release it with `rocksdb_free`.
            Ok(raw_data_and_free(value_data, value_size))
        }
    }

    pub fn get_from_batch_and_db<T, I, K>(
        &self,
        db: &DBCommon<T, I>,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<Vec<u8>>, Error>
    where
        T: ThreadMode,
        I: DBInner,
        K: AsRef<[u8]>,
    {
        if readopts.inner.is_null() {
            return Err(Error::new(
                "Unable to create RocksDB read options. This is a fairly trivial call, and its \
                 failure may be indicative of a mis-compiled or mis-loaded RocksDB library."
                    .to_owned(),
            ));
        }

        let key = key.as_ref();
        unsafe {
            let mut value_size: size_t = 0;
            let value_data = ffi_try!(ffi::rocksdb_writebatch_wi_get_from_batch_and_db(
                self.inner,
                db.inner.inner(),
                readopts.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                &raw mut value_size
            ));

            // `value_data` was allocated by `malloc` on the C++ side; copy it
            // out and release it with `rocksdb_free`.
            Ok(raw_data_and_free(value_data, value_size))
        }
    }

    /// The returned slice pins a block inside `db`'s block cache, so its
    /// lifetime is tied to `db` rather than to `self`. Letting lifetime elision
    /// pick `&self` here would allow the slice to outlive the database and
    /// release a cache handle into a destroyed cache.
    pub fn get_pinned_from_batch_and_db<'db, T, I, K>(
        &self,
        db: &'db DBCommon<T, I>,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<DBPinnableSlice<'db>>, Error>
    where
        T: ThreadMode,
        I: DBInner,
        K: AsRef<[u8]>,
    {
        if readopts.inner.is_null() {
            return Err(Error::new(
                "Unable to create RocksDB read options. This is a fairly trivial call, and its \
                 failure may be indicative of a mis-compiled or mis-loaded RocksDB library."
                    .to_owned(),
            ));
        }

        let key = key.as_ref();
        unsafe {
            let value_data = ffi_try!(ffi::rocksdb_writebatch_wi_get_pinned_from_batch_and_db(
                self.inner,
                db.inner.inner(),
                readopts.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
            ));

            if value_data.is_null() {
                Ok(None)
            } else {
                Ok(Some(DBPinnableSlice::from_c(value_data)))
            }
        }
    }

    pub fn get_from_batch_and_db_cf<T, I, K>(
        &self,
        db: &DBCommon<T, I>,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<Vec<u8>>, Error>
    where
        T: ThreadMode,
        I: DBInner,
        K: AsRef<[u8]>,
    {
        if readopts.inner.is_null() {
            return Err(Error::new(
                "Unable to create RocksDB read options. This is a fairly trivial call, and its \
                 failure may be indicative of a mis-compiled or mis-loaded RocksDB library."
                    .to_owned(),
            ));
        }

        let key = key.as_ref();
        unsafe {
            let mut value_size: size_t = 0;
            let value_data = ffi_try!(ffi::rocksdb_writebatch_wi_get_from_batch_and_db_cf(
                self.inner,
                db.inner.inner(),
                readopts.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                &raw mut value_size
            ));

            // `value_data` was allocated by `malloc` on the C++ side; copy it
            // out and release it with `rocksdb_free`.
            Ok(raw_data_and_free(value_data, value_size))
        }
    }

    /// The returned slice pins a block inside `db`'s block cache, so its
    /// lifetime is tied to `db` rather than to `self`. See
    /// [`Self::get_pinned_from_batch_and_db`].
    pub fn get_pinned_from_batch_and_db_cf<'db, T, I, K>(
        &self,
        db: &'db DBCommon<T, I>,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<DBPinnableSlice<'db>>, Error>
    where
        T: ThreadMode,
        I: DBInner,
        K: AsRef<[u8]>,
    {
        if readopts.inner.is_null() {
            return Err(Error::new(
                "Unable to create RocksDB read options. This is a fairly trivial call, and its \
                 failure may be indicative of a mis-compiled or mis-loaded RocksDB library."
                    .to_owned(),
            ));
        }

        let key = key.as_ref();
        unsafe {
            let value_data = ffi_try!(ffi::rocksdb_writebatch_wi_get_pinned_from_batch_and_db_cf(
                self.inner,
                db.inner.inner(),
                readopts.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
            ));

            if value_data.is_null() {
                Ok(None)
            } else {
                Ok(Some(DBPinnableSlice::from_c(value_data)))
            }
        }
    }

    /// Insert a value into the database under the given key.
    pub fn put<K, V>(&mut self, key: K, value: V)
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let value = value.as_ref();

        unsafe {
            ffi::rocksdb_writebatch_wi_put(
                self.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                value.as_ptr() as *const c_char,
                value.len() as size_t,
            );
        }
    }

    pub fn put_cf<K, V>(&mut self, cf: &impl AsColumnFamilyRef, key: K, value: V)
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let value = value.as_ref();

        unsafe {
            ffi::rocksdb_writebatch_wi_put_cf(
                self.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                value.as_ptr() as *const c_char,
                value.len() as size_t,
            );
        }
    }

    pub fn merge<K, V>(&mut self, key: K, value: V)
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let value = value.as_ref();

        unsafe {
            ffi::rocksdb_writebatch_wi_merge(
                self.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                value.as_ptr() as *const c_char,
                value.len() as size_t,
            );
        }
    }

    pub fn merge_cf<K, V>(&mut self, cf: &impl AsColumnFamilyRef, key: K, value: V)
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let value = value.as_ref();

        unsafe {
            ffi::rocksdb_writebatch_wi_merge_cf(
                self.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                value.as_ptr() as *const c_char,
                value.len() as size_t,
            );
        }
    }

    /// Removes the database entry for key. Does nothing if the key was not found.
    pub fn delete<K: AsRef<[u8]>>(&mut self, key: K) {
        let key = key.as_ref();

        unsafe {
            ffi::rocksdb_writebatch_wi_delete(
                self.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
            );
        }
    }

    pub fn delete_cf<K: AsRef<[u8]>>(&mut self, cf: &impl AsColumnFamilyRef, key: K) {
        let key = key.as_ref();

        unsafe {
            ffi::rocksdb_writebatch_wi_delete_cf(
                self.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
            );
        }
    }

    /// Removes the database entry for a key that was written exactly once.
    ///
    /// This is a cheaper delete than [`delete`](Self::delete), but it is only
    /// correct when the key has had at most one `put` and no `merge` since the
    /// last delete of that key. Using it on a key that was written more than
    /// once leaves an older version visible, and RocksDB does not report that
    /// as an error.
    pub fn single_delete<K: AsRef<[u8]>>(&mut self, key: K) {
        let key = key.as_ref();

        unsafe {
            ffi::rocksdb_writebatch_wi_singledelete(
                self.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
            );
        }
    }

    /// Removes the entry for a write-once key in the given column family.
    ///
    /// See [`single_delete`](Self::single_delete) for when this is safe to use.
    pub fn single_delete_cf<K: AsRef<[u8]>>(&mut self, cf: &impl AsColumnFamilyRef, key: K) {
        let key = key.as_ref();

        unsafe {
            ffi::rocksdb_writebatch_wi_singledelete_cf(
                self.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
            );
        }
    }

    /// Removes entries in the range `[from, to)`.
    ///
    /// Range deletes are recorded in the batch but they are not indexed. Reads
    /// and iterators that go through the batch, such as
    /// [`get_from_batch`](Self::get_from_batch) and
    /// [`iterator_with_base`](Self::iterator_with_base), do not see them. They
    /// only take effect once the batch is written to the database.
    pub fn delete_range<K: AsRef<[u8]>>(&mut self, from: K, to: K) {
        let (start_key, end_key) = (from.as_ref(), to.as_ref());

        unsafe {
            ffi::rocksdb_writebatch_wi_delete_range(
                self.inner,
                start_key.as_ptr() as *const c_char,
                start_key.len() as size_t,
                end_key.as_ptr() as *const c_char,
                end_key.len() as size_t,
            );
        }
    }

    /// Removes entries in the range `[from, to)` of one column family.
    ///
    /// See [`delete_range`](Self::delete_range) for why these are invisible to
    /// reads through the batch.
    pub fn delete_range_cf<K: AsRef<[u8]>>(&mut self, cf: &impl AsColumnFamilyRef, from: K, to: K) {
        let (start_key, end_key) = (from.as_ref(), to.as_ref());

        unsafe {
            ffi::rocksdb_writebatch_wi_delete_range_cf(
                self.inner,
                cf.inner(),
                start_key.as_ptr() as *const c_char,
                start_key.len() as size_t,
                end_key.as_ptr() as *const c_char,
                end_key.len() as size_t,
            );
        }
    }

    /// Append a blob of arbitrary size to the records in this batch.
    ///
    /// The blob goes to the write-ahead log but never to an SST file, and it
    /// consumes no sequence number and does not change [`len`](Self::len).
    pub fn put_log_data<V: AsRef<[u8]>>(&mut self, log_data: V) {
        let log_data = log_data.as_ref();

        unsafe {
            ffi::rocksdb_writebatch_wi_put_log_data(
                self.inner,
                log_data.as_ptr() as *const c_char,
                log_data.len() as size_t,
            );
        }
    }

    /// Record the current state of the batch so it can be undone later.
    ///
    /// Save points nest, so each call pushes onto a stack that
    /// [`rollback_to_save_point`](Self::rollback_to_save_point) pops from.
    pub fn set_save_point(&mut self) {
        unsafe {
            ffi::rocksdb_writebatch_wi_set_save_point(self.inner);
        }
    }

    /// Undo every operation recorded since the most recent save point, and pop
    /// that save point.
    ///
    /// Returns an error if there is no save point to roll back to.
    pub fn rollback_to_save_point(&mut self) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_writebatch_wi_rollback_to_save_point(
                self.inner
            ));
        }
        Ok(())
    }

    /// Overwrite the user-defined timestamp on every entry in the batch.
    ///
    /// `get_ts_size` is called with each column family id the batch touches and
    /// must return the timestamp width configured for that column family, or 0
    /// if it does not use timestamps. `ts` must be exactly as wide as every
    /// non-zero size it returns.
    ///
    /// # Safety
    ///
    /// Every key already recorded for a column family whose `get_ts_size`
    /// returns a non-zero width must be at least that many bytes long, which in
    /// practice means it was written through one of the `_with_ts` methods and
    /// already carries a timestamp suffix of exactly that width. RocksDB
    /// overwrites the last `width` bytes of each key without checking that the
    /// key is that long, so a shorter key makes it write in front of the key and
    /// corrupt the heap. Mixing plain [`put`](Self::put) with a non-zero width
    /// for the same column family is what usually triggers this.
    ///
    /// # Errors
    ///
    /// Returns an error if `ts` is empty, if its length differs from a non-zero
    /// width returned by `get_ts_size`, or if `get_ts_size` reports that it
    /// could not find the width for a column family.
    pub unsafe fn update_timestamps<S, F>(&mut self, ts: S, mut get_ts_size: F) -> Result<(), Error>
    where
        S: AsRef<[u8]>,
        F: FnMut(u32) -> usize,
    {
        let ts = ts.as_ref();
        let state = std::ptr::from_mut(&mut get_ts_size).cast::<c_void>();
        unsafe {
            ffi_try!(ffi::rocksdb_writebatch_wi_update_timestamps(
                self.inner,
                ts.as_ptr() as *const c_char,
                ts.len() as size_t,
                state,
                Some(get_ts_size_callback::<F>),
            ));
        }
        Ok(())
    }

    /// Clear all updates buffered in this batch.
    pub fn clear(&mut self) {
        unsafe {
            ffi::rocksdb_writebatch_wi_clear(self.inner);
        }
    }

    /// The returned iterator reads directly out of this batch's internal
    /// skip-list and write buffer, so it must not outlive the batch. Binding
    /// `&self` to the same lifetime as the base iterator is what enforces that;
    /// with an independent lifetime on `&self` the iterator could outlive the
    /// batch and read freed memory.
    pub fn iterator_with_base<'a, D>(
        &'a self,
        base_iterator: DBRawIteratorWithThreadMode<'a, D>,
    ) -> DBRawIteratorWithThreadMode<'a, D>
    where
        D: DBAccess,
    {
        let (base_iterator_inner, readopts) = base_iterator.into_inner();

        let iterator = unsafe {
            ffi::rocksdb_writebatch_wi_create_iterator_with_base_readopts(
                self.inner,
                base_iterator_inner.as_ptr(),
                readopts.as_ptr(),
            )
        };

        // The delta iterator keeps its own raw pointers to the iterate bounds
        // in these options, so it has to hold the same object the base
        // iterator was built from, not an equivalent copy.
        DBRawIteratorWithThreadMode::from_inner(iterator, readopts)
    }

    /// The returned iterator reads directly out of this batch, so it must not
    /// outlive the batch. See [`Self::iterator_with_base`].
    pub fn iterator_with_base_cf<'a, D>(
        &'a self,
        base_iterator: DBRawIteratorWithThreadMode<'a, D>,
        cf: &impl AsColumnFamilyRef,
    ) -> DBRawIteratorWithThreadMode<'a, D>
    where
        D: DBAccess,
    {
        let (base_iterator_inner, readopts) = base_iterator.into_inner();

        let iterator = unsafe {
            ffi::rocksdb_writebatch_wi_create_iterator_with_base_cf_readopts(
                self.inner,
                base_iterator_inner.as_ptr(),
                cf.inner(),
                readopts.as_ptr(),
            )
        };

        DBRawIteratorWithThreadMode::from_inner(iterator, readopts)
    }
}

impl Drop for WriteBatchWithIndex {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_writebatch_wi_destroy(self.inner);
        }
    }
}

unsafe impl Send for WriteBatchWithIndex {}

use crate::db::DBInner;
use crate::ffi_util::CSlice;
use crate::{
    AsColumnFamilyRef, DBAccess, DBCommon, DBPinnableSlice, DBRawIteratorWithThreadMode, Error,
    Options, ReadOptions, ThreadMode, ffi,
};
use libc::{c_char, c_uchar, size_t};

pub struct WriteBatchWithIndex {
    pub(crate) inner: *mut ffi::rocksdb_writebatch_wi_t,
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

    /// Retrieves a value from the batch, allocating a new `Vec<u8>` to hold the data.
    ///
    /// For a zero-allocation alternative, see [`WriteBatchWithIndex::get_from_batch_with`].
    #[inline]
    pub fn get_from_batch<K>(&self, key: K, options: &Options) -> Result<Option<Vec<u8>>, Error>
    where
        K: AsRef<[u8]>,
    {
        self.get_from_batch_with(key, options, <[u8]>::to_vec)
    }

    /// Retrieves a value from the batch and passes it to a closure.
    ///
    /// This provides a zero-copy and zero-allocation way to read data. The `&[u8]` slice
    /// is directly mapped to RocksDB's C-allocated memory and is safely freed after the closure returns.
    pub fn get_from_batch_with<K, F, R>(
        &self,
        key: K,
        options: &Options,
        f: F,
    ) -> Result<Option<R>, Error>
    where
        K: AsRef<[u8]>,
        F: FnOnce(&[u8]) -> R,
    {
        let key = key.as_ref();
        let mut value_size: size_t = 0;
        let value_data = unsafe {
            ffi_try!(ffi::rocksdb_writebatch_wi_get_from_batch(
                self.inner,
                options.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                &raw mut value_size
            ))
        };

        if value_data.is_null() {
            Ok(None)
        } else {
            // SAFETY: CSlice takes ownership and frees the memory on drop.
            let c_slice = unsafe { CSlice::from_raw_parts(value_data.cast_const(), value_size) };
            Ok(Some(f(c_slice.as_ref())))
        }
    }

    /// Retrieves a value from a column family in the batch, allocating a new `Vec<u8>` to hold the data.
    ///
    /// For a zero-allocation alternative, see [`WriteBatchWithIndex::get_from_batch_cf_with`].
    #[inline]
    pub fn get_from_batch_cf<K>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        options: &Options,
    ) -> Result<Option<Vec<u8>>, Error>
    where
        K: AsRef<[u8]>,
    {
        self.get_from_batch_cf_with(cf, key, options, <[u8]>::to_vec)
    }

    /// Retrieves a value from a column family in the batch and passes it to a closure.
    ///
    /// This provides a zero-copy and zero-allocation way to read data. The `&[u8]` slice
    /// is directly mapped to RocksDB's C-allocated memory and is safely freed after the closure returns.
    pub fn get_from_batch_cf_with<K, F, R>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        options: &Options,
        f: F,
    ) -> Result<Option<R>, Error>
    where
        K: AsRef<[u8]>,
        F: FnOnce(&[u8]) -> R,
    {
        let key = key.as_ref();
        let mut value_size: size_t = 0;
        let value_data = unsafe {
            ffi_try!(ffi::rocksdb_writebatch_wi_get_from_batch_cf(
                self.inner,
                options.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                &raw mut value_size
            ))
        };

        if value_data.is_null() {
            Ok(None)
        } else {
            // SAFETY: CSlice takes ownership and frees the memory on drop.
            let c_slice = unsafe { CSlice::from_raw_parts(value_data.cast_const(), value_size) };
            Ok(Some(f(c_slice.as_ref())))
        }
    }

    /// Retrieves a value from the batch or the database, allocating a new `Vec<u8>` to hold the data.
    ///
    /// For a zero-allocation alternative, see [`WriteBatchWithIndex::get_from_batch_and_db_with`].
    #[inline]
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
        self.get_from_batch_and_db_with(db, key, readopts, <[u8]>::to_vec)
    }

    /// Retrieves a value from the batch or the database and passes it to a closure.
    ///
    /// This provides a zero-copy and zero-allocation way to read data. The `&[u8]` slice
    /// is directly mapped to RocksDB's C-allocated memory and is safely freed after the closure returns.
    pub fn get_from_batch_and_db_with<T, I, K, F, R>(
        &self,
        db: &DBCommon<T, I>,
        key: K,
        readopts: &ReadOptions,
        f: F,
    ) -> Result<Option<R>, Error>
    where
        T: ThreadMode,
        I: DBInner,
        K: AsRef<[u8]>,
        F: FnOnce(&[u8]) -> R,
    {
        if readopts.inner.is_null() {
            return Err(Error::new(
                "Unable to create RocksDB read options. This is a fairly trivial call, and its \
                 failure may be indicative of a mis-compiled or mis-loaded RocksDB library."
                    .to_owned(),
            ));
        }

        let key = key.as_ref();
        let mut value_size: size_t = 0;
        let value_data = unsafe {
            ffi_try!(ffi::rocksdb_writebatch_wi_get_from_batch_and_db(
                self.inner,
                db.inner.inner(),
                readopts.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                &raw mut value_size
            ))
        };

        if value_data.is_null() {
            Ok(None)
        } else {
            // SAFETY: CSlice takes ownership and frees the memory on drop.
            let c_slice = unsafe { CSlice::from_raw_parts(value_data.cast_const(), value_size) };
            Ok(Some(f(c_slice.as_ref())))
        }
    }

    pub fn get_pinned_from_batch_and_db<T, I, K>(
        &'_ self,
        db: &DBCommon<T, I>,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<DBPinnableSlice<'_>>, Error>
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

    /// Retrieves a value from a column family in the batch or the database, allocating a new `Vec<u8>` to hold the data.
    ///
    /// For a zero-allocation alternative, see [`WriteBatchWithIndex::get_from_batch_and_db_cf_with`].
    #[inline]
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
        self.get_from_batch_and_db_cf_with(db, cf, key, readopts, <[u8]>::to_vec)
    }

    /// Retrieves a value from a column family in the batch or the database and passes it to a closure.
    ///
    /// This provides a zero-copy and zero-allocation way to read data. The `&[u8]` slice
    /// is directly mapped to RocksDB's C-allocated memory and is safely freed after the closure returns.
    pub fn get_from_batch_and_db_cf_with<T, I, K, F, R>(
        &self,
        db: &DBCommon<T, I>,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: &ReadOptions,
        f: F,
    ) -> Result<Option<R>, Error>
    where
        T: ThreadMode,
        I: DBInner,
        K: AsRef<[u8]>,
        F: FnOnce(&[u8]) -> R,
    {
        if readopts.inner.is_null() {
            return Err(Error::new(
                "Unable to create RocksDB read options. This is a fairly trivial call, and its \
                 failure may be indicative of a mis-compiled or mis-loaded RocksDB library."
                    .to_owned(),
            ));
        }

        let key = key.as_ref();
        let mut value_size: size_t = 0;
        let value_data = unsafe {
            ffi_try!(ffi::rocksdb_writebatch_wi_get_from_batch_and_db_cf(
                self.inner,
                db.inner.inner(),
                readopts.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                &raw mut value_size
            ))
        };

        if value_data.is_null() {
            Ok(None)
        } else {
            // SAFETY: CSlice takes ownership and frees the memory on drop.
            let c_slice = unsafe { CSlice::from_raw_parts(value_data.cast_const(), value_size) };
            Ok(Some(f(c_slice.as_ref())))
        }
    }

    pub fn get_pinned_from_batch_and_db_cf<T, I, K>(
        &'_ self,
        db: &DBCommon<T, I>,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<DBPinnableSlice<'_>>, Error>
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

    /// Clear all updates buffered in this batch.
    pub fn clear(&mut self) {
        unsafe {
            ffi::rocksdb_writebatch_wi_clear(self.inner);
        }
    }

    pub fn iterator_with_base<'a, D>(
        &self,
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
                readopts.inner,
            )
        };

        DBRawIteratorWithThreadMode::from_inner(iterator, readopts)
    }

    pub fn iterator_with_base_cf<'a, D>(
        &self,
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
                readopts.inner,
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

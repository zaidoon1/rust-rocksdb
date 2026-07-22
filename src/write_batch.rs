// Copyright 2020 Tyler Neely
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::{AsColumnFamilyRef, ffi};
use libc::{c_char, c_int, c_void, size_t};
use std::{io::IoSlice, slice};

const INLINE_WRITE_BATCH_PARTS: usize = 4;

enum WriteBatchPartStorage {
    Inline([ffi::rocksdb_slice_t; INLINE_WRITE_BATCH_PARTS]),
    Heap(Vec<ffi::rocksdb_slice_t>),
}

struct WriteBatchParts {
    storage: WriteBatchPartStorage,
    count: c_int,
}

impl WriteBatchParts {
    fn new(parts: &[IoSlice<'_>], name: &str) -> Result<Self, crate::Error> {
        let count = c_int::try_from(parts.len()).map_err(|_| {
            crate::Error::new(format!(
                "{name} has {} parts; expected at most {}",
                parts.len(),
                c_int::MAX
            ))
        })?;
        let storage = if parts.len() <= INLINE_WRITE_BATCH_PARTS {
            Self::inline(parts)
        } else {
            Self::heap(parts)
        };
        Ok(Self { storage, count })
    }

    fn inline(parts: &[IoSlice<'_>]) -> WriteBatchPartStorage {
        let mut slices = std::array::from_fn(|_| ffi::rocksdb_slice_t {
            data: std::ptr::null(),
            size: 0,
        });
        for (index, part) in parts.iter().enumerate() {
            slices[index] = ffi::rocksdb_slice_t {
                data: part.as_ptr().cast(),
                size: part.len(),
            };
        }
        WriteBatchPartStorage::Inline(slices)
    }

    fn heap(parts: &[IoSlice<'_>]) -> WriteBatchPartStorage {
        WriteBatchPartStorage::Heap(
            parts
                .iter()
                .map(|part| ffi::rocksdb_slice_t {
                    data: part.as_ptr().cast(),
                    size: part.len(),
                })
                .collect(),
        )
    }

    fn as_ptr(&self) -> *const ffi::rocksdb_slice_t {
        match &self.storage {
            WriteBatchPartStorage::Inline(slices) => slices.as_ptr(),
            WriteBatchPartStorage::Heap(slices) => slices.as_ptr(),
        }
    }
}

/// A type alias to keep compatibility. See [`WriteBatchWithTransaction`] for details
pub type WriteBatch = WriteBatchWithTransaction<false>;

/// An atomic batch of write operations.
///
/// [`delete_range`](#method.delete_range) is not supported in [`Transaction`].
///
/// Making an atomic commit of several writes:
///
/// ```
/// use rust_rocksdb::{DB, Options, WriteBatchWithTransaction};
///
/// let tempdir = tempfile::Builder::new()
///     .prefix("_path_for_rocksdb_storage1")
///     .tempdir()
///     .expect("Failed to create temporary path for the _path_for_rocksdb_storage1");
/// let path = tempdir.path();
/// {
///     let db = DB::open_default(path).unwrap();
///     let mut batch = WriteBatchWithTransaction::<false>::default();
///     batch.put(b"my key", b"my value");
///     batch.put(b"key2", b"value2");
///     batch.put(b"key3", b"value3");
///
///     // delete_range is supported when use without transaction
///     batch.delete_range(b"key2", b"key3");
///
///     db.write(&batch); // Atomically commits the batch
/// }
/// let _ = DB::destroy(&Options::default(), path);
/// ```
///
/// [`Transaction`]: crate::Transaction
pub struct WriteBatchWithTransaction<const TRANSACTION: bool> {
    pub(crate) inner: *mut ffi::rocksdb_writebatch_t,
}

/// Receives the puts and deletes of a write batch.
///
/// The application must provide an implementation of this trait when
/// iterating the operations within a `WriteBatch`
pub trait WriteBatchIterator {
    /// Called with a key and value that were `put` into the batch.
    fn put(&mut self, key: &[u8], value: &[u8]);
    /// Called with a key that was `delete`d from the batch.
    fn delete(&mut self, key: &[u8]);
}

/// Receives the puts, deletes, and merges of a write batch with column family
/// information.
///
/// This trait extends write batch iteration to support column family-specific
/// operations. The application must implement this trait when iterating
/// operations within a WriteBatch that contains column family-aware writes.
///
/// Note that for the default column family "default", the column family ID is 0.
pub trait WriteBatchIteratorCf {
    /// Called with a column family ID, key, and value that were put into
    /// the specific column family of the batch.
    fn put_cf(&mut self, cf_id: u32, key: &[u8], value: &[u8]);
    /// Called with a column family ID and key that were `delete`d from the
    /// specific column family of the batch.
    fn delete_cf(&mut self, cf_id: u32, key: &[u8]);
    /// Called with a column family ID, key, and value that were `merge`d into
    /// the specific column family of the batch.
    /// Merge operations combine the provided value with the existing value at
    /// the key using a database-defined merge operator.
    fn merge_cf(&mut self, cf_id: u32, key: &[u8], value: &[u8]);
}

unsafe extern "C" fn writebatch_put_callback<T: WriteBatchIterator>(
    state: *mut c_void,
    k: *const c_char,
    klen: usize,
    v: *const c_char,
    vlen: usize,
) {
    unsafe {
        let callbacks = &mut *(state as *mut T);
        let key = slice::from_raw_parts(k.cast::<u8>(), klen);
        let value = slice::from_raw_parts(v.cast::<u8>(), vlen);
        callbacks.put(key, value);
    }
}

unsafe extern "C" fn writebatch_delete_callback<T: WriteBatchIterator>(
    state: *mut c_void,
    k: *const c_char,
    klen: usize,
) {
    unsafe {
        let callbacks = &mut *(state as *mut T);
        let key = slice::from_raw_parts(k.cast::<u8>(), klen);
        callbacks.delete(key);
    }
}

unsafe extern "C" fn writebatch_put_cf_callback<T: WriteBatchIteratorCf>(
    state: *mut c_void,
    cfid: u32,
    k: *const c_char,
    klen: usize,
    v: *const c_char,
    vlen: usize,
) {
    unsafe {
        let callbacks = &mut *(state as *mut T);
        let key = slice::from_raw_parts(k.cast::<u8>(), klen);
        let value = slice::from_raw_parts(v.cast::<u8>(), vlen);
        callbacks.put_cf(cfid, key, value);
    }
}

unsafe extern "C" fn writebatch_delete_cf_callback<T: WriteBatchIteratorCf>(
    state: *mut c_void,
    cfid: u32,
    k: *const c_char,
    klen: usize,
) {
    unsafe {
        let callbacks = &mut *(state as *mut T);
        let key = slice::from_raw_parts(k.cast::<u8>(), klen);
        callbacks.delete_cf(cfid, key);
    }
}

unsafe extern "C" fn writebatch_merge_cf_callback<T: WriteBatchIteratorCf>(
    state: *mut c_void,
    cfid: u32,
    k: *const c_char,
    klen: usize,
    v: *const c_char,
    vlen: usize,
) {
    unsafe {
        let callbacks = &mut *(state as *mut T);
        let key = slice::from_raw_parts(k.cast::<u8>(), klen);
        let value = slice::from_raw_parts(v.cast::<u8>(), vlen);
        callbacks.merge_cf(cfid, key, value);
    }
}

impl<const TRANSACTION: bool> WriteBatchWithTransaction<TRANSACTION> {
    /// Create a new `WriteBatch` without allocating memory.
    pub fn new() -> Self {
        Self {
            inner: unsafe { ffi::rocksdb_writebatch_create() },
        }
    }

    /// Creates `WriteBatch` with the specified `capacity` in bytes. Allocates immediately.
    pub fn with_capacity_bytes(capacity_bytes: usize) -> Self {
        Self {
            // zeroes from default constructor
            // https://github.com/facebook/rocksdb/blob/0f35db55d86ea8699ea936c9e2a4e34c82458d6b/include/rocksdb/write_batch.h#L66
            inner: unsafe { ffi::rocksdb_writebatch_create_with_params(capacity_bytes, 0, 0, 0) },
        }
    }

    /// Construct with a reference to a byte array serialized by [`WriteBatch`].
    pub fn from_data(data: &[u8]) -> Self {
        unsafe {
            let ptr = data.as_ptr();
            let len = data.len();
            Self {
                inner: ffi::rocksdb_writebatch_create_from(
                    ptr as *const libc::c_char,
                    len as size_t,
                ),
            }
        }
    }

    pub fn len(&self) -> usize {
        unsafe { ffi::rocksdb_writebatch_count(self.inner) as usize }
    }

    /// Return WriteBatch serialized size (in bytes).
    pub fn size_in_bytes(&self) -> usize {
        unsafe {
            let mut batch_size: size_t = 0;
            ffi::rocksdb_writebatch_data(self.inner, &raw mut batch_size);
            batch_size
        }
    }

    /// Return a reference to a byte array which represents a serialized version of the batch.
    pub fn data(&self) -> &[u8] {
        unsafe {
            let mut batch_size: size_t = 0;
            let batch_data = ffi::rocksdb_writebatch_data(self.inner, &raw mut batch_size);
            std::slice::from_raw_parts(batch_data as _, batch_size)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate the put and delete operations within this write batch. Note that
    /// this does _not_ return an `Iterator` but instead will invoke the `put()`
    /// and `delete()` member functions of the provided `WriteBatchIterator`
    /// trait implementation.
    pub fn iterate<T: WriteBatchIterator>(&self, callbacks: &mut T) {
        let state = std::ptr::from_mut::<T>(callbacks) as *mut c_void;
        unsafe {
            ffi::rocksdb_writebatch_iterate(
                self.inner,
                state,
                Some(writebatch_put_callback::<T>),
                Some(writebatch_delete_callback::<T>),
            );
        }
    }

    /// Iterate the put, delete, and merge operations within this write batch with column family
    /// information. Note that this does _not_ return an `Iterator` but instead will invoke the
    /// `put_cf()`, `delete_cf()`, and `merge_cf()` member functions of the provided
    /// `WriteBatchIteratorCf` trait implementation.
    ///
    /// # Notes
    /// - For operations on the default column family ("default"), the `cf_id` parameter passed to
    ///   the callbacks will be 0
    pub fn iterate_cf<T: WriteBatchIteratorCf>(&self, callbacks: &mut T) {
        let state = std::ptr::from_mut::<T>(callbacks) as *mut c_void;
        unsafe {
            ffi::rocksdb_writebatch_iterate_cf(
                self.inner,
                state,
                Some(writebatch_put_cf_callback::<T>),
                Some(writebatch_delete_cf_callback::<T>),
                Some(writebatch_merge_cf_callback::<T>),
            );
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
            ffi::rocksdb_writebatch_put(
                self.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                value.as_ptr() as *const c_char,
                value.len() as size_t,
            );
        }
    }

    /// Inserts one key and value assembled from multiple byte slices.
    ///
    /// This avoids concatenating the parts in Rust. RocksDB copies the key and
    /// value parts into the write batch during this call, so the slices do not
    /// need to outlive the method.
    pub fn put_vectored(
        &mut self,
        key: &[IoSlice<'_>],
        value: &[IoSlice<'_>],
    ) -> Result<(), crate::Error> {
        let key = WriteBatchParts::new(key, "key")?;
        let value = WriteBatchParts::new(value, "value")?;
        unsafe {
            ffi_try!(ffi::rust_rocksdb_writebatch_put_slices(
                self.inner,
                key.count,
                key.as_ptr(),
                value.count,
                value.as_ptr(),
            ));
        }
        Ok(())
    }

    /// Insert a value into the specific column family of the database under the given key.
    pub fn put_cf<K, V>(&mut self, cf: &impl AsColumnFamilyRef, key: K, value: V)
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let value = value.as_ref();

        unsafe {
            ffi::rocksdb_writebatch_put_cf(
                self.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                value.as_ptr() as *const c_char,
                value.len() as size_t,
            );
        }
    }

    /// Inserts one key and value assembled from multiple byte slices into a column family.
    ///
    /// This avoids concatenating the parts in Rust. RocksDB copies the key and
    /// value parts into the write batch during this call, so the slices do not
    /// need to outlive the method.
    pub fn put_cf_vectored(
        &mut self,
        cf: &impl AsColumnFamilyRef,
        key: &[IoSlice<'_>],
        value: &[IoSlice<'_>],
    ) -> Result<(), crate::Error> {
        let key = WriteBatchParts::new(key, "key")?;
        let value = WriteBatchParts::new(value, "value")?;
        unsafe {
            ffi_try!(ffi::rust_rocksdb_writebatch_put_slices_cf(
                self.inner,
                cf.inner(),
                key.count,
                key.as_ptr(),
                value.count,
                value.as_ptr(),
            ));
        }
        Ok(())
    }

    /// Insert a value into the specific column family of the database
    /// under the given key with timestamp.
    pub fn put_cf_with_ts<K, V, S>(&mut self, cf: &impl AsColumnFamilyRef, key: K, ts: S, value: V)
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
        S: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let value = value.as_ref();
        let ts = ts.as_ref();
        unsafe {
            ffi::rocksdb_writebatch_put_cf_with_ts(
                self.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                ts.as_ptr() as *const c_char,
                ts.len() as size_t,
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
            ffi::rocksdb_writebatch_merge(
                self.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                value.as_ptr() as *const c_char,
                value.len() as size_t,
            );
        }
    }

    /// Merges one key and value assembled from multiple byte slices.
    ///
    /// This avoids concatenating the parts in Rust. RocksDB copies the key and
    /// value parts into the write batch during this call, so the slices do not
    /// need to outlive the method.
    pub fn merge_vectored(
        &mut self,
        key: &[IoSlice<'_>],
        value: &[IoSlice<'_>],
    ) -> Result<(), crate::Error> {
        let key = WriteBatchParts::new(key, "key")?;
        let value = WriteBatchParts::new(value, "value")?;
        unsafe {
            ffi_try!(ffi::rust_rocksdb_writebatch_merge_slices(
                self.inner,
                key.count,
                key.as_ptr(),
                value.count,
                value.as_ptr(),
            ));
        }
        Ok(())
    }

    pub fn merge_cf<K, V>(&mut self, cf: &impl AsColumnFamilyRef, key: K, value: V)
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let value = value.as_ref();

        unsafe {
            ffi::rocksdb_writebatch_merge_cf(
                self.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                value.as_ptr() as *const c_char,
                value.len() as size_t,
            );
        }
    }

    /// Merges one key and value assembled from multiple byte slices in a column family.
    ///
    /// This avoids concatenating the parts in Rust. RocksDB copies the key and
    /// value parts into the write batch during this call, so the slices do not
    /// need to outlive the method.
    pub fn merge_cf_vectored(
        &mut self,
        cf: &impl AsColumnFamilyRef,
        key: &[IoSlice<'_>],
        value: &[IoSlice<'_>],
    ) -> Result<(), crate::Error> {
        let key = WriteBatchParts::new(key, "key")?;
        let value = WriteBatchParts::new(value, "value")?;
        unsafe {
            ffi_try!(ffi::rust_rocksdb_writebatch_merge_slices_cf(
                self.inner,
                cf.inner(),
                key.count,
                key.as_ptr(),
                value.count,
                value.as_ptr(),
            ));
        }
        Ok(())
    }

    /// Removes the database entry for key. Does nothing if the key was not found.
    pub fn delete<K: AsRef<[u8]>>(&mut self, key: K) {
        let key = key.as_ref();

        unsafe {
            ffi::rocksdb_writebatch_delete(
                self.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
            );
        }
    }

    /// Removes the entry for one key assembled from multiple byte slices.
    ///
    /// This avoids concatenating the parts in Rust. RocksDB copies the key
    /// parts into the write batch during this call, so the slices do not need
    /// to outlive the method.
    pub fn delete_vectored(&mut self, key: &[IoSlice<'_>]) -> Result<(), crate::Error> {
        let key = WriteBatchParts::new(key, "key")?;
        unsafe {
            ffi_try!(ffi::rust_rocksdb_writebatch_delete_slices(
                self.inner,
                key.count,
                key.as_ptr(),
            ));
        }
        Ok(())
    }

    /// Removes the database entry in the specific column family for key.
    /// Does nothing if the key was not found.
    pub fn delete_cf<K: AsRef<[u8]>>(&mut self, cf: &impl AsColumnFamilyRef, key: K) {
        let key = key.as_ref();

        unsafe {
            ffi::rocksdb_writebatch_delete_cf(
                self.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
            );
        }
    }

    /// Removes the entry for one key assembled from multiple byte slices in a column family.
    ///
    /// This avoids concatenating the parts in Rust. RocksDB copies the key
    /// parts into the write batch during this call, so the slices do not need
    /// to outlive the method.
    pub fn delete_cf_vectored(
        &mut self,
        cf: &impl AsColumnFamilyRef,
        key: &[IoSlice<'_>],
    ) -> Result<(), crate::Error> {
        let key = WriteBatchParts::new(key, "key")?;
        unsafe {
            ffi_try!(ffi::rust_rocksdb_writebatch_delete_slices_cf(
                self.inner,
                cf.inner(),
                key.count,
                key.as_ptr(),
            ));
        }
        Ok(())
    }

    /// Removes the database entry in the specific column family with timestamp for key.
    /// Does nothing if the key was not found.
    pub fn delete_cf_with_ts<K: AsRef<[u8]>, S: AsRef<[u8]>>(
        &mut self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        ts: S,
    ) {
        let key = key.as_ref();
        let ts = ts.as_ref();
        unsafe {
            ffi::rocksdb_writebatch_delete_cf_with_ts(
                self.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                ts.as_ptr() as *const c_char,
                ts.len() as size_t,
            );
        }
    }

    // Append a blob of arbitrary size to the records in this batch. The blob will
    // be stored in the transaction log but not in any other file. In particular,
    // it will not be persisted to the SST files. When iterating over this
    // WriteBatch, WriteBatch::Handler::LogData will be called with the contents
    // of the blob as it is encountered. Blobs, puts, deletes, and merges will be
    // encountered in the same order in which they were inserted. The blob will
    // NOT consume sequence number(s) and will NOT increase the count of the batch
    //
    // Example application: add timestamps to the transaction log for use in
    // replication.
    pub fn put_log_data<V: AsRef<[u8]>>(&mut self, log_data: V) {
        let log_data = log_data.as_ref();

        unsafe {
            ffi::rocksdb_writebatch_put_log_data(
                self.inner,
                log_data.as_ptr() as *const c_char,
                log_data.len() as size_t,
            );
        }
    }

    /// Clear all updates buffered in this batch.
    pub fn clear(&mut self) {
        unsafe {
            ffi::rocksdb_writebatch_clear(self.inner);
        }
    }
}

impl WriteBatchWithTransaction<false> {
    /// Remove database entries from start key to end key.
    ///
    /// Removes the database entries in the range ["begin_key", "end_key"), i.e.,
    /// including "begin_key" and excluding "end_key". It is not an error if no
    /// keys exist in the range ["begin_key", "end_key").
    pub fn delete_range<K: AsRef<[u8]>>(&mut self, from: K, to: K) {
        let (start_key, end_key) = (from.as_ref(), to.as_ref());

        unsafe {
            ffi::rocksdb_writebatch_delete_range(
                self.inner,
                start_key.as_ptr() as *const c_char,
                start_key.len() as size_t,
                end_key.as_ptr() as *const c_char,
                end_key.len() as size_t,
            );
        }
    }

    /// Removes entries in a range whose bounds are assembled from byte slices.
    ///
    /// The range includes `from` and excludes `to`. The two bounds must have
    /// RocksDB copies both bounds into the write batch during this call, so the
    /// slices do not need to outlive the method.
    pub fn delete_range_vectored(
        &mut self,
        from: &[IoSlice<'_>],
        to: &[IoSlice<'_>],
    ) -> Result<(), crate::Error> {
        if from.len() != to.len() {
            return Err(crate::Error::new(format!(
                "range start has {} parts but range end has {} parts; expected equal counts",
                from.len(),
                to.len()
            )));
        }
        let from = WriteBatchParts::new(from, "range start")?;
        let to = WriteBatchParts::new(to, "range end")?;
        unsafe {
            ffi_try!(ffi::rust_rocksdb_writebatch_delete_range_slices(
                self.inner,
                from.count,
                from.as_ptr(),
                to.count,
                to.as_ptr(),
            ));
        }
        Ok(())
    }

    /// Remove database entries in column family from start key to end key.
    ///
    /// Removes the database entries in the range ["begin_key", "end_key"), i.e.,
    /// including "begin_key" and excluding "end_key". It is not an error if no
    /// keys exist in the range ["begin_key", "end_key").
    pub fn delete_range_cf<K: AsRef<[u8]>>(&mut self, cf: &impl AsColumnFamilyRef, from: K, to: K) {
        let (start_key, end_key) = (from.as_ref(), to.as_ref());

        unsafe {
            ffi::rocksdb_writebatch_delete_range_cf(
                self.inner,
                cf.inner(),
                start_key.as_ptr() as *const c_char,
                start_key.len() as size_t,
                end_key.as_ptr() as *const c_char,
                end_key.len() as size_t,
            );
        }
    }

    /// Removes entries in a column family range whose bounds are assembled from byte slices.
    ///
    /// The range includes `from` and excludes `to`. The two bounds must have
    /// RocksDB copies both bounds into the write batch during this call, so the
    /// slices do not need to outlive the method.
    pub fn delete_range_cf_vectored(
        &mut self,
        cf: &impl AsColumnFamilyRef,
        from: &[IoSlice<'_>],
        to: &[IoSlice<'_>],
    ) -> Result<(), crate::Error> {
        if from.len() != to.len() {
            return Err(crate::Error::new(format!(
                "range start has {} parts but range end has {} parts; expected equal counts",
                from.len(),
                to.len()
            )));
        }
        let from = WriteBatchParts::new(from, "range start")?;
        let to = WriteBatchParts::new(to, "range end")?;
        unsafe {
            ffi_try!(ffi::rust_rocksdb_writebatch_delete_range_slices_cf(
                self.inner,
                cf.inner(),
                from.count,
                from.as_ptr(),
                to.count,
                to.as_ptr(),
            ));
        }
        Ok(())
    }
}

impl<const TRANSACTION: bool> Default for WriteBatchWithTransaction<TRANSACTION> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const TRANSACTION: bool> Drop for WriteBatchWithTransaction<TRANSACTION> {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_writebatch_destroy(self.inner);
        }
    }
}

unsafe impl<const TRANSACTION: bool> Send for WriteBatchWithTransaction<TRANSACTION> {}

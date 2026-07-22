use crate::{Error, ffi};
use std::{
    marker::PhantomData,
    ptr::{self, NonNull},
    slice,
};

/// Owns all values returned by one native pinned MultiGet operation.
///
/// Values are borrowed directly from RocksDB and remain valid until this batch
/// is dropped. Vendored builds store successful values in one native owner
/// instead of allocating one wrapper per key. System builds use upstream C API
/// handles internally to avoid depending on RocksDB's private C++ ABI.
pub struct DBPinnableBatch<'db> {
    inner: NonNull<ffi::rust_rocksdb_pinnable_batch_t>,
    len: usize,
    db: PhantomData<&'db ()>,
}

/// Iterator over a [`DBPinnableBatch`].
pub struct DBPinnableBatchIter<'batch, 'db> {
    batch: &'batch DBPinnableBatch<'db>,
    index: usize,
}

unsafe impl Send for DBPinnableBatch<'_> {}
unsafe impl Sync for DBPinnableBatch<'_> {}

impl<'db> DBPinnableBatch<'db> {
    /// Returns the number of results in the batch.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns whether the batch contains no results.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns one result by input index.
    pub fn get(&self, index: usize) -> Option<Result<Option<&[u8]>, Error>> {
        if index >= self.len() {
            return None;
        }

        let mut value = ptr::null();
        let mut value_len = 0;
        let mut error = ptr::null();
        let mut error_len = 0;
        let state = unsafe {
            ffi::rust_rocksdb_pinnable_batch_get(
                self.inner.as_ptr(),
                index,
                &raw mut value,
                &raw mut value_len,
                &raw mut error,
                &raw mut error_len,
            )
        };

        Some(match state {
            state if state == ffi::rust_rocksdb_pinnable_batch_not_found as u8 => Ok(None),
            state if state == ffi::rust_rocksdb_pinnable_batch_found as u8 => {
                let value = if value_len == 0 {
                    &[]
                } else {
                    // SAFETY: RocksDB owns `value` until this batch is dropped.
                    unsafe { slice::from_raw_parts(value.cast::<u8>(), value_len) }
                };
                Ok(Some(value))
            }
            state if state == ffi::rust_rocksdb_pinnable_batch_error as u8 => {
                let message = if error_len == 0 {
                    String::new()
                } else {
                    // SAFETY: The batch owns the error bytes until it is dropped.
                    let bytes = unsafe { slice::from_raw_parts(error.cast::<u8>(), error_len) };
                    String::from_utf8_lossy(bytes).into_owned()
                };
                Err(Error::new(message))
            }
            unexpected => unreachable!("unexpected pinned batch result state {unexpected}"),
        })
    }

    /// Iterates over results in input order.
    pub fn iter(&self) -> DBPinnableBatchIter<'_, 'db> {
        DBPinnableBatchIter {
            batch: self,
            index: 0,
        }
    }

    pub(crate) unsafe fn from_c(inner: *mut ffi::rust_rocksdb_pinnable_batch_t) -> Self {
        let inner = NonNull::new(inner).expect("RocksDB returned a null pinned batch");
        Self {
            len: unsafe { ffi::rust_rocksdb_pinnable_batch_len(inner.as_ptr()) },
            inner,
            db: PhantomData,
        }
    }
}

impl Drop for DBPinnableBatch<'_> {
    fn drop(&mut self) {
        unsafe {
            ffi::rust_rocksdb_pinnable_batch_destroy(self.inner.as_ptr());
        }
    }
}

impl<'batch> IntoIterator for &'batch DBPinnableBatch<'_> {
    type Item = Result<Option<&'batch [u8]>, Error>;
    type IntoIter = DBPinnableBatchIter<'batch, 'batch>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'batch> Iterator for DBPinnableBatchIter<'batch, '_> {
    type Item = Result<Option<&'batch [u8]>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let result = self.batch.get(self.index)?;
        self.index += 1;
        Some(result)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.batch.len() - self.index;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for DBPinnableBatchIter<'_, '_> {}
impl std::iter::FusedIterator for DBPinnableBatchIter<'_, '_> {}

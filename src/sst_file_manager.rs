use std::ptr::NonNull;
use std::sync::Arc;

use crate::env::Env;
use crate::ffi;

pub(crate) struct SstFileManagerWrapper {
    pub(crate) inner: NonNull<ffi::rocksdb_sst_file_manager_t>,
}

unsafe impl Send for SstFileManagerWrapper {}
unsafe impl Sync for SstFileManagerWrapper {}

impl Drop for SstFileManagerWrapper {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_sst_file_manager_destroy(self.inner.as_ptr());
        }
    }
}

#[derive(Clone)]
pub struct SstFileManager(pub(crate) Arc<SstFileManagerWrapper>);

impl SstFileManager {
    /// Creates a new `SstFileManager` bound to the provided `Env`.
    ///
    /// SstFileManager tracks and controls total SST file space usage, enabling
    /// applications to cap disk utilization and throttle deletions.
    pub fn new(env: &Env) -> Self {
        let inner = NonNull::new(unsafe { ffi::rocksdb_sst_file_manager_create(env.0.inner) })
            .expect("Could not create RocksDB sst file manager");
        SstFileManager(Arc::new(SstFileManagerWrapper { inner }))
    }

    /// Sets the maximum allowed total SST file size in bytes.
    pub fn set_max_allowed_space_usage(&self, bytes: u64) {
        unsafe {
            ffi::rocksdb_sst_file_manager_set_max_allowed_space_usage(self.0.inner.as_ptr(), bytes);
        }
    }

    /// Sets the compaction buffer size in bytes used by the manager for space accounting.
    pub fn set_compaction_buffer_size(&self, bytes: u64) {
        unsafe {
            ffi::rocksdb_sst_file_manager_set_compaction_buffer_size(self.0.inner.as_ptr(), bytes);
        }
    }

    /// Returns true if the total SST file size has reached or exceeded the configured limit.
    pub fn is_max_allowed_space_reached(&self) -> bool {
        unsafe { ffi::rocksdb_sst_file_manager_is_max_allowed_space_reached(self.0.inner.as_ptr()) }
    }

    /// Returns true if the space limit is reached, including compaction output under accounting.
    pub fn is_max_allowed_space_reached_including_compactions(&self) -> bool {
        unsafe {
            ffi::rocksdb_sst_file_manager_is_max_allowed_space_reached_including_compactions(
                self.0.inner.as_ptr(),
            )
        }
    }

    /// Returns the total size of SST files tracked by this manager in bytes.
    pub fn get_total_size(&self) -> u64 {
        unsafe { ffi::rocksdb_sst_file_manager_get_total_size(self.0.inner.as_ptr()) }
    }

    /// Returns the configured file deletion rate in bytes per second. Negative means unlimited.
    pub fn get_delete_rate_bytes_per_second(&self) -> i64 {
        unsafe {
            ffi::rocksdb_sst_file_manager_get_delete_rate_bytes_per_second(self.0.inner.as_ptr())
        }
    }

    /// Sets the file deletion rate in bytes per second. Use a negative value to disable limiting.
    pub fn set_delete_rate_bytes_per_second(&self, rate: i64) {
        unsafe {
            ffi::rocksdb_sst_file_manager_set_delete_rate_bytes_per_second(
                self.0.inner.as_ptr(),
                rate,
            );
        }
    }

    /// Returns the maximum trash-to-DB size ratio used for trash space control.
    pub fn get_max_trash_db_ratio(&self) -> f64 {
        unsafe { ffi::rocksdb_sst_file_manager_get_max_trash_db_ratio(self.0.inner.as_ptr()) }
    }

    /// Sets the maximum trash-to-DB size ratio used for trash space control.
    pub fn set_max_trash_db_ratio(&self, ratio: f64) {
        unsafe {
            ffi::rocksdb_sst_file_manager_set_max_trash_db_ratio(self.0.inner.as_ptr(), ratio);
        }
    }

    /// Returns the total trash size tracked by this manager in bytes.
    pub fn get_total_trash_size(&self) -> u64 {
        unsafe { ffi::rocksdb_sst_file_manager_get_total_trash_size(self.0.inner.as_ptr()) }
    }
}

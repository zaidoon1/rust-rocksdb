use std::{ptr::NonNull, sync::Arc};

use crate::{ffi, Env};

pub(crate) struct SstFileManagerWrapper {
    pub(crate) inner: NonNull<ffi::rocksdb_sst_file_manager_t>,
    _outlive: Env,
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

/// SstFileManager is used to track SST and blob files in the DB and control
/// their deletion rate. All SstFileManager public functions are thread-safe.
pub struct SstFileManager(pub(crate) Arc<SstFileManagerWrapper>);

impl SstFileManager {
    /// Initializes SstFileManager with the specified RocksDB Env.
    pub fn new_sst_file_manager(env: &Env) -> Self {
        let inner =
            NonNull::new(unsafe { ffi::rocksdb_sst_file_manager_create(env.0.inner) }).unwrap();

        SstFileManager(Arc::new(SstFileManagerWrapper {
            inner,
            _outlive: env.clone(),
        }))
    }

    /// returns the total size of all tracked files
    pub fn get_total_size(&self) -> u64 {
        unsafe { ffi::rocksdb_sst_file_manager_get_total_size(self.0.inner.as_ptr()) }
    }

    /// returns true if the total size of SST  and blob files exceeded the
    /// maximum allowed space usage.
    pub fn is_max_allowed_space_reached(&self) -> bool {
        unsafe { ffi::rocksdb_sst_file_manager_is_max_allowed_space_reached(self.0.inner.as_ptr()) }
    }

    /// returns true if the total size of SST and blob files as well as
    /// estimated size of ongoing compactions exceeds the maximums allowed space
    /// usage.
    pub fn is_max_allowed_space_reached_including_compactions(&self) -> bool {
        unsafe {
            ffi::rocksdb_sst_file_manager_is_max_allowed_space_reached_including_compactions(
                self.0.inner.as_ptr(),
            )
        }
    }

    /// update trash/DB size ratio where new files will be deleted immediately
    pub fn set_max_trash_db_ratio(&self, ratio: f64) {
        unsafe {
            ffi::rocksdb_sst_file_manager_set_max_trash_db_ratio(self.0.inner.as_ptr(), ratio)
        }
    }

    /// returns trash/DB size ratio where new files will be deleted immediately
    pub fn get_max_trash_db_ratio(&self) -> f64 {
        unsafe { ffi::rocksdb_sst_file_manager_get_max_trash_db_ratio(self.0.inner.as_ptr()) }
    }

    /// update the maximum allowed space that should be used by RocksDB, if
    /// the total size of the SST and blob files exceeds max_allowed_space,
    /// writes to RocksDB will fail.
    ///
    /// setting max_allowed_space to 0 will disable this feature; maximum
    /// allowed space will be infinite (Default value).
    pub fn set_max_allowed_space_usage(&self, max_allowed_space: u64) {
        unsafe {
            ffi::rocksdb_sst_file_manager_set_max_allowed_space_usage(
                self.0.inner.as_ptr(),
                max_allowed_space,
            )
        }
    }

    /// sets the amount of buffer room each compaction should be able to leave.
    /// In other words, at its maximum disk space consumption, the compaction
    /// should still leave compaction_buffer_size available on the disk so that
    /// other background functions may continue, such as logging and flushing.
    pub fn set_compaction_buffer_size(&self, compaction_buffer_size: u64) {
        unsafe {
            ffi::rocksdb_sst_file_manager_set_compaction_buffer_size(
                self.0.inner.as_ptr(),
                compaction_buffer_size,
            )
        }
    }

    /// update the delete rate limit in bytes per second. zero means disable
    /// delete rate limiting and delete files immediately
    pub fn set_delete_rate_bytes_per_second(&self, delete_rate: i64) {
        unsafe {
            ffi::rocksdb_sst_file_manager_set_delete_rate_bytes_per_second(
                self.0.inner.as_ptr(),
                delete_rate,
            )
        }
    }

    /// returns delete rate limit in bytes per second
    pub fn get_delete_rate_bytes_per_second(&self) -> i64 {
        unsafe {
            ffi::rocksdb_sst_file_manager_get_delete_rate_bytes_per_second(self.0.inner.as_ptr())
        }
    }
}

impl Drop for SstFileManager {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_sst_file_manager_destroy(self.0.inner.as_ptr());
        }
    }
}

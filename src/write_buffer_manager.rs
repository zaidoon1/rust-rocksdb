use crate::cache::Cache;
use crate::ffi;
use libc::size_t;
use std::ptr::NonNull;
use std::sync::Arc;

pub(crate) struct WriteBufferManagerWrapper {
    pub(crate) inner: NonNull<ffi::rocksdb_write_buffer_manager_t>,
}

unsafe impl Send for WriteBufferManagerWrapper {}
unsafe impl Sync for WriteBufferManagerWrapper {}

impl Drop for WriteBufferManagerWrapper {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_write_buffer_manager_destroy(self.inner.as_ptr());
        }
    }
}

#[derive(Clone)]
pub struct WriteBufferManager(pub(crate) Arc<WriteBufferManagerWrapper>);

impl WriteBufferManager {
    /// <https://github.com/facebook/rocksdb/wiki/Write-Buffer-Manager>
    /// Write buffer manager helps users control the total memory used by memtables across multiple column families and/or DB instances.
    /// Users can enable this control by 2 ways:
    ///
    /// 1- Limit the total memtable usage across multiple column families and DBs under a threshold.
    /// 2- Cost the memtable memory usage to block cache so that memory of RocksDB can be capped by the single limit.
    /// The usage of a write buffer manager is similar to rate_limiter and sst_file_manager.
    /// Users can create one write buffer manager object and pass it to all the options of column families or DBs whose memtable size they want to be controlled by this object.
    ///
    /// A memory limit is given when creating the write buffer manager object. RocksDB will try to limit the total memory to under this limit.
    ///
    /// a flush will be triggered on one column family of the DB you are inserting to,
    ///
    /// If mutable memtable size exceeds about 90% of the limit,
    /// If the total memory is over the limit, more aggressive flush may also be triggered only if the mutable memtable size also exceeds 50% of the limit.
    /// Both checks are needed because if already more than half memory is being flushed, triggering more flush may not help.
    ///
    /// The total memory is counted as total memory allocated in the arena, even if some of that may not yet be used by memtable.
    ///
    /// buffer_size: the memory limit in bytes.
    /// allow_stall: If set true, it will enable stalling of all writers when memory usage exceeds buffer_size (soft limit).
    ///             It will wait for flush to complete and memory usage to drop down
    pub fn new_write_buffer_manager(buffer_size: size_t, allow_stall: bool) -> Self {
        let inner = NonNull::new(unsafe {
            ffi::rocksdb_write_buffer_manager_create(buffer_size, allow_stall)
        })
        .unwrap();
        WriteBufferManager(Arc::new(WriteBufferManagerWrapper { inner }))
    }

    /// Users can set up RocksDB to cost memory used by memtables to block cache.
    /// This can happen no matter whether you enable memtable memory limit or not.
    /// This option is added to manage memory (memtables + block cache) under a single limit.
    ///
    /// buffer_size: the memory limit in bytes.
    /// allow_stall: If set true, it will enable stalling of all writers when memory usage exceeds buffer_size (soft limit).
    ///             It will wait for flush to complete and memory usage to drop down
    /// cache: the block cache instance
    pub fn new_write_buffer_manager_with_cache(
        buffer_size: size_t,
        allow_stall: bool,
        cache: Cache,
    ) -> Self {
        let inner = NonNull::new(unsafe {
            ffi::rocksdb_write_buffer_manager_create_with_cache(
                buffer_size,
                cache.0.inner.as_ptr(),
                allow_stall,
            )
        })
        .unwrap();
        WriteBufferManager(Arc::new(WriteBufferManagerWrapper { inner }))
    }

    /// Returns the WriteBufferManager memory usage in bytes.
    pub fn get_usage(&self) -> usize {
        unsafe { ffi::rocksdb_write_buffer_manager_memory_usage(self.0.inner.as_ptr()) }
    }

    /// Returns the current buffer size in bytes.
    pub fn get_buffer_size(&self) -> usize {
        unsafe { ffi::rocksdb_write_buffer_manager_buffer_size(self.0.inner.as_ptr()) }
    }

    /// Set the buffer size in bytes.
    pub fn set_buffer_size(&self, new_size: usize) {
        unsafe {
            ffi::rocksdb_write_buffer_manager_set_buffer_size(self.0.inner.as_ptr(), new_size);
        }
    }

    /// Returns if WriteBufferManager is enabled.
    pub fn enabled(&self) -> bool {
        unsafe { ffi::rocksdb_write_buffer_manager_enabled(self.0.inner.as_ptr()) }
    }

    /// set the allow_stall flag.
    pub fn set_allow_stall(&self, allow_stall: bool) {
        unsafe {
            ffi::rocksdb_write_buffer_manager_set_allow_stall(self.0.inner.as_ptr(), allow_stall);
        }
    }
}

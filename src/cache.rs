use crate::{ffi, LruCacheOptions};
use libc::size_t;
use std::ptr::NonNull;
use std::sync::Arc;

pub(crate) struct CacheWrapper {
    pub(crate) inner: NonNull<ffi::rocksdb_cache_t>,
}

unsafe impl Send for CacheWrapper {}
unsafe impl Sync for CacheWrapper {}

impl Drop for CacheWrapper {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_cache_destroy(self.inner.as_ptr());
        }
    }
}

#[derive(Clone)]
pub struct Cache(pub(crate) Arc<CacheWrapper>);

impl Cache {
    /// Creates an LRU cache with capacity in bytes.
    pub fn new_lru_cache(capacity: size_t) -> Cache {
        let inner = NonNull::new(unsafe { ffi::rocksdb_cache_create_lru(capacity) }).unwrap();
        Cache(Arc::new(CacheWrapper { inner }))
    }

    /// Creates an LRU cache with custom options.
    pub fn new_lru_cache_opts(opts: &LruCacheOptions) -> Cache {
        let inner =
            NonNull::new(unsafe { ffi::rocksdb_cache_create_lru_opts(opts.inner) }).unwrap();
        Cache(Arc::new(CacheWrapper { inner }))
    }

    /// Creates a HyperClockCache with capacity in bytes.
    ///
    /// `estimated_entry_charge` is an important tuning parameter. The optimal
    /// choice at any given time is
    /// `(cache.get_usage() - 64 * cache.get_table_address_count()) /
    /// cache.get_occupancy_count()`, or approximately `cache.get_usage() /
    /// cache.get_occupancy_count()`.
    ///
    /// However, the value cannot be changed dynamically, so as the cache
    /// composition changes at runtime, the following tradeoffs apply:
    ///
    /// * If the estimate is substantially too high (e.g., 25% higher),
    ///   the cache may have to evict entries to prevent load factors that
    ///   would dramatically affect lookup times.
    /// * If the estimate is substantially too low (e.g., less than half),
    ///   then meta data space overhead is substantially higher.
    ///
    /// The latter is generally preferable, and picking the larger of
    /// block size and meta data block size is a reasonable choice that
    /// errs towards this side.
    pub fn new_hyper_clock_cache(capacity: size_t, estimated_entry_charge: size_t) -> Cache {
        Cache(Arc::new(CacheWrapper {
            inner: NonNull::new(unsafe {
                ffi::rocksdb_cache_create_hyper_clock(capacity, estimated_entry_charge)
            })
            .unwrap(),
        }))
    }

    /// Returns the cache memory usage in bytes.
    pub fn get_usage(&self) -> usize {
        unsafe { ffi::rocksdb_cache_get_usage(self.0.inner.as_ptr()) }
    }

    /// Returns the pinned memory usage in bytes.
    pub fn get_pinned_usage(&self) -> usize {
        unsafe { ffi::rocksdb_cache_get_pinned_usage(self.0.inner.as_ptr()) }
    }

    /// Sets cache capacity in bytes.
    pub fn set_capacity(&mut self, capacity: size_t) {
        unsafe {
            ffi::rocksdb_cache_set_capacity(self.0.inner.as_ptr(), capacity);
        }
    }
}

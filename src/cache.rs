use crate::{LruCacheOptions, ffi};
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

    /// Creates a HyperClockCache with `capacity` in bytes.
    ///
    /// HyperClockCache is now generally recommended over LRUCache. See RocksDB's
    /// [HyperClockCacheOptions in cache.h](https://github.com/facebook/rocksdb/blob/main/include/rocksdb/cache.h)
    /// for details.
    ///
    /// `estimated_entry_charge` is an optional parameter. When not provided
    /// (== 0, recommended and default), an HCC variant with a
    /// dynamically-growing table and generally good performance is used. This
    /// variant depends on anonymous mmaps so might not be available on all
    /// platforms.
    ///
    /// If the average "charge" (uncompressed block size) of block cache entries
    /// is reasonably predicted and provided here, the most efficient variant of
    /// HCC is used. Performance is degraded if the prediction is inaccurate.
    /// Prediction could be difficult or impossible with cache-charging features
    /// such as WriteBufferManager. The best parameter choice based on a cache
    /// in use is roughly given by `cache.get_usage() / cache.get_occupancy_count()`,
    /// though it is better to estimate toward the lower side than the higher
    /// side when the ratio might vary.
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

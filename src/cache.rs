use crate::{Error, LruCacheOptions, ffi, ffi_util::convert_rocksdb_error};
use libc::{c_char, c_int, size_t};
use std::ptr::{self, NonNull};
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

    /// Creates an LRU cache with capacity in bytes that refuses to exceed it.
    ///
    /// A normal LRU cache treats the capacity as a target. When an insert
    /// arrives and nothing more can be evicted, because the remaining entries
    /// are pinned, it inserts anyway and lets usage go over capacity. With the
    /// strict limit the insert fails with `MemoryLimit` instead, so the cache
    /// is a hard memory bound.
    ///
    /// That failure is not confined to the cache API. RocksDB inserts into the
    /// block cache while reading, so operations that need a block the cache
    /// cannot hold surface the error to the caller. Opening a table file can
    /// fail the same way when
    /// [`BlockBasedOptions::set_cache_index_and_filter_blocks`] charges the
    /// reader against a full cache. Size the cache with headroom before
    /// turning this on.
    ///
    /// [`BlockBasedOptions::set_cache_index_and_filter_blocks`]: crate::BlockBasedOptions::set_cache_index_and_filter_blocks
    pub fn new_lru_cache_with_strict_capacity_limit(capacity: size_t) -> Cache {
        let inner = NonNull::new(unsafe {
            ffi::rocksdb_cache_create_lru_with_strict_capacity_limit(capacity)
        })
        .unwrap();
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

    /// Creates a HyperClockCache with custom options.
    ///
    /// Use this over [`Self::new_hyper_clock_cache`] to set the shard count or
    /// a memory allocator. See [`HyperClockCacheOptions`].
    pub fn new_hyper_clock_cache_opts(opts: &HyperClockCacheOptions) -> Cache {
        let inner = NonNull::new(unsafe { ffi::rocksdb_cache_create_hyper_clock_opts(opts.inner) })
            .unwrap();
        Cache(Arc::new(CacheWrapper { inner }))
    }

    /// Returns the cache memory usage in bytes.
    pub fn get_usage(&self) -> usize {
        unsafe { ffi::rocksdb_cache_get_usage(self.0.inner.as_ptr()) }
    }

    /// Returns the pinned memory usage in bytes.
    pub fn get_pinned_usage(&self) -> usize {
        unsafe { ffi::rocksdb_cache_get_pinned_usage(self.0.inner.as_ptr()) }
    }

    /// Returns the configured cache capacity in bytes.
    pub fn get_capacity(&self) -> usize {
        unsafe { ffi::rocksdb_cache_get_capacity(self.0.inner.as_ptr()) }
    }

    /// Returns the number of entries currently occupying the cache hash tables.
    pub fn get_occupancy_count(&self) -> usize {
        unsafe { ffi::rocksdb_cache_get_occupancy_count(self.0.inner.as_ptr()) }
    }

    /// Returns the total number of cache hash table addresses.
    pub fn get_table_address_count(&self) -> usize {
        unsafe { ffi::rocksdb_cache_get_table_address_count(self.0.inner.as_ptr()) }
    }

    /// Sets cache capacity in bytes.
    pub fn set_capacity(&mut self, capacity: size_t) {
        unsafe {
            ffi::rocksdb_cache_set_capacity(self.0.inner.as_ptr(), capacity);
        }
    }

    /// Give up the cached data instead of freeing it when the cache goes away.
    ///
    /// This is RocksDB's `Cache::DisownData`. It only changes what happens at
    /// destruction: the last drop stops running the shard destructors, so the
    /// cached entries are never reclaimed. It exists to make process exit
    /// faster on a large cache, where walking and freeing every entry costs
    /// real time and nothing is going to reuse the memory anyway.
    ///
    /// [`Cache`] is a reference-counted handle, so this affects the shared cache
    /// and every other clone of it, not just this handle.
    ///
    /// Under ASAN or valgrind RocksDB ignores the request and frees the entries
    /// as usual, so the leak is not reported there.
    ///
    /// # Safety
    ///
    /// Every database and every other user of this cache must be dropped
    /// before this call — RocksDB's contract is: "Always delete the DB object
    /// before calling this method!" (`advanced_cache.h`). Nothing may read
    /// from or write to the cache after this call; RocksDB documents any use
    /// after disowning as unsupported. Anything that would have reused the
    /// memory later cannot, because it is leaked for the remaining lifetime of
    /// the process, which makes this call worth doing only shortly before the
    /// process exits.
    pub unsafe fn disown_data(&mut self) {
        unsafe {
            ffi::rocksdb_cache_disown_data(self.0.inner.as_ptr());
        }
    }
}

pub(crate) struct MemoryAllocatorWrapper {
    pub(crate) inner: NonNull<ffi::rocksdb_memory_allocator_t>,
}

unsafe impl Send for MemoryAllocatorWrapper {}
unsafe impl Sync for MemoryAllocatorWrapper {}

impl Drop for MemoryAllocatorWrapper {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_memory_allocator_destroy(self.inner.as_ptr());
        }
    }
}

/// An allocator RocksDB uses for cache block memory instead of the system one.
///
/// Pass it to `set_memory_allocator` on [`LruCacheOptions`] or
/// [`HyperClockCacheOptions`] before building the cache. One allocator can
/// back several caches. Cloning is cheap and hands out another reference to
/// the same allocator.
///
/// The allocator is ignored for compression libraries that allocate internally
/// (currently only XPRESS).
#[derive(Clone)]
pub struct MemoryAllocator(pub(crate) Arc<MemoryAllocatorWrapper>);

impl MemoryAllocator {
    /// Creates a jemalloc allocator that keeps its memory out of core dumps.
    ///
    /// The allocator serves every request from one dedicated jemalloc arena
    /// and marks that arena `MADV_DONTDUMP`. A block cache built on it
    /// therefore does not land in a core dump, which for a multi-gigabyte
    /// cache is the difference between a usable dump and an unusable one.
    /// Using a single arena also cuts jemalloc metadata, and jemalloc's
    /// thread-local cache is left on to keep the arena's mutex from becoming
    /// a bottleneck.
    ///
    /// # Errors
    ///
    /// Fails with `Not implemented: Not compiled with JEMALLOC` when the
    /// linked librocksdb has no jemalloc support. That depends on how
    /// librocksdb itself was built, which this crate's `jemalloc` feature
    /// controls only for a vendored build. An externally supplied librocksdb
    /// can have jemalloc with the feature off, or lack it with the feature on,
    /// so check the result rather than the feature.
    pub fn new_jemalloc_nodump() -> Result<Self, Error> {
        // `rocksdb_jemalloc_nodump_allocator_create` allocates the handle
        // before it tries to build the allocator and returns it either way,
        // so the error path has to destroy it. `ffi_try!` would return first
        // and leak it.
        let mut err: *mut c_char = ptr::null_mut();
        let allocator = unsafe { ffi::rocksdb_jemalloc_nodump_allocator_create(&raw mut err) };
        if !err.is_null() {
            if !allocator.is_null() {
                unsafe { ffi::rocksdb_memory_allocator_destroy(allocator) };
            }
            return Err(convert_rocksdb_error(err));
        }

        let inner = NonNull::new(allocator).ok_or_else(|| {
            Error::new("Could not create RocksDB jemalloc nodump allocator".to_owned())
        })?;
        Ok(MemoryAllocator(Arc::new(MemoryAllocatorWrapper { inner })))
    }

    /// Returns the raw pointer the C API passes this allocator by.
    pub(crate) fn as_ptr(&self) -> *mut ffi::rocksdb_memory_allocator_t {
        self.0.inner.as_ptr()
    }
}

/// Configuration for a HyperClockCache, RocksDB's block cache of choice.
///
/// HCC is lock free, so it holds up far better than LRU under parallel load
/// and contention, and it needs sharding only for a modest gain, so its shards
/// can be much larger and are much less prone to thrashing. Upstream
/// recommends it over LRU for the block cache.
///
/// The caveats, from `rocksdb/include/rocksdb/cache.h`:
///
/// * It only works as [`BlockBasedOptions::set_block_cache`]. It is not a
///   general cache, so it is not usable as a row cache.
/// * Cache priorities are enforced less aggressively, so a long range scan can
///   dilute the cache unless it reads with `fill_cache` off.
/// * Some configurations need anonymous mmap support.
/// * The bounded counting-CLOCK eviction gives a hit rate slightly different
///   from LRU's, in either direction.
///
/// Build a cache from these with [`Cache::new_hyper_clock_cache_opts`].
///
/// [`BlockBasedOptions::set_block_cache`]: crate::BlockBasedOptions::set_block_cache
pub struct HyperClockCacheOptions {
    pub(crate) inner: *mut ffi::rocksdb_hyper_clock_cache_options_t,
}

// Same reasoning as `LruCacheOptions` in `db_options.rs`: the inner pointer is
// never aliased, and every mutation goes through `&mut self`.
unsafe impl Send for HyperClockCacheOptions {}
unsafe impl Sync for HyperClockCacheOptions {}

impl Drop for HyperClockCacheOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_hyper_clock_cache_options_destroy(self.inner);
        }
    }
}

impl HyperClockCacheOptions {
    /// Creates options for a HyperClockCache of `capacity` bytes.
    ///
    /// There is no `Default`, because a cache with no capacity is not a useful
    /// starting point. See [`Self::set_estimated_entry_charge`] for what to
    /// pass as `estimated_entry_charge`, and pass 0 if unsure.
    pub fn new(capacity: size_t, estimated_entry_charge: size_t) -> Self {
        let inner = unsafe {
            ffi::rocksdb_hyper_clock_cache_options_create(capacity, estimated_entry_charge)
        };
        assert!(
            !inner.is_null(),
            "Could not create RocksDB hyper clock cache options"
        );

        Self { inner }
    }

    /// Capacity of the cache, in the same units as the `charge` of each entry,
    /// which is bytes unless entries are charged some other way.
    pub fn set_capacity(&mut self, capacity: size_t) {
        unsafe {
            ffi::rocksdb_hyper_clock_cache_options_set_capacity(self.inner, capacity);
        }
    }

    /// The estimated average `charge` of a cache entry, which selects the HCC
    /// variant.
    ///
    /// 0, the recommended value, picks the variant with a dynamically growing
    /// table and generally good performance. That variant depends on anonymous
    /// mmap, so it is not available everywhere.
    ///
    /// A non-zero value picks the most efficient variant, but only pays off if
    /// the estimate is close. A bad estimate degrades performance. Estimating
    /// is hard or impossible with cache-charging features such as
    /// [`WriteBufferManager`] in play. For a cache already running, roughly
    /// `cache.get_usage() / cache.get_occupancy_count()` is the right number,
    /// and when the ratio moves around it is better to guess low than high.
    ///
    /// [`WriteBufferManager`]: crate::WriteBufferManager
    pub fn set_estimated_entry_charge(&mut self, estimated_entry_charge: size_t) {
        unsafe {
            ffi::rocksdb_hyper_clock_cache_options_set_estimated_entry_charge(
                self.inner,
                estimated_entry_charge,
            );
        }
    }

    /// Cache is sharded into 2^`num_shard_bits` shards, by hash of key.
    /// If < 0, a good default is chosen based on the capacity and the
    /// implementation.
    ///
    /// HCC uses sharding only for a modest performance boost, so it does not
    /// need as many shards as LRU does to scale.
    pub fn set_num_shard_bits(&mut self, num_shard_bits: c_int) {
        unsafe {
            ffi::rocksdb_hyper_clock_cache_options_set_num_shard_bits(self.inner, num_shard_bits);
        }
    }

    /// Allocates cache block memory through `allocator` instead of the system
    /// allocator.
    ///
    /// These options do not borrow the allocator. The C setter copies the
    /// `shared_ptr<MemoryAllocator>` out of the handle
    /// (`opts->rep.memory_allocator = memory_allocator->rep` in `db/c.cc`), so
    /// the allocator stays alive through the options and the caches built from
    /// them even after the [`MemoryAllocator`] here is dropped.
    pub fn set_memory_allocator(&mut self, allocator: &MemoryAllocator) {
        unsafe {
            ffi::rocksdb_hyper_clock_cache_options_set_memory_allocator(
                self.inner,
                allocator.as_ptr(),
            );
        }
    }
}

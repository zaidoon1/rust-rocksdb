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

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::ptr::{NonNull, null_mut};
use std::slice;
use std::sync::Arc;

use libc::{self, c_char, c_double, c_int, c_uchar, c_uint, c_void, size_t};

use crate::cache::Cache;
use crate::column_family::ColumnFamilyTtl;
use crate::event_listener::{EventListener, new_event_listener};
use crate::ffi_util::from_cstr_and_free;
use crate::sst_file_manager::SstFileManager;
use crate::statistics::{Histogram, HistogramData, StatsLevel};
use crate::write_buffer_manager::WriteBufferManager;
use crate::{
    ColumnFamilyDescriptor, Error, SnapshotWithThreadMode,
    compaction_filter::{self, CompactionFilterCallback, CompactionFilterFn},
    compaction_filter_factory::{self, CompactionFilterFactory},
    comparator::{
        ComparatorCallback, ComparatorWithTsCallback, CompareFn, CompareTsFn, CompareWithoutTsFn,
    },
    db::DBAccess,
    env::Env,
    ffi,
    ffi_util::{CStrLike, to_cpath},
    merge_operator::{
        self, MergeFn, MergeOperatorCallback, full_merge_callback, partial_merge_callback,
    },
    slice_transform::SliceTransform,
    statistics::Ticker,
};

// must be Send and Sync because it will be called by RocksDB from different threads
type LogCallbackFn = dyn Fn(LogLevel, &str) + 'static + Send + Sync;

/// Type for log callbacks used by [`Options::set_info_logger`]. Use Box to pass a thin pointer to
/// the C callback.
type LoggerCallback = Box<dyn Fn(LogLevel, &str) + Sync + Send>;

// Holds a log callback to ensure it outlives any Options and DBs that use it.
struct LogCallback {
    callback: Box<LogCallbackFn>,
}

/// Options that must outlive the DB, and may be shared between DBs. This is cloned and stored
/// with every DB that is created from the options.
#[derive(Default)]
pub(crate) struct OptionsMustOutliveDB {
    env: Option<Env>,
    row_cache: Option<Cache>,
    blob_cache: Option<Cache>,
    block_based: Option<BlockBasedOptionsMustOutliveDB>,
    write_buffer_manager: Option<WriteBufferManager>,
    sst_file_manager: Option<SstFileManager>,
    log_callback: Option<Arc<LogCallback>>,
    comparator: Option<Arc<OwnedComparator>>,
    compaction_filter: Option<Arc<OwnedCompactionFilter>>,
    logger_callback: Option<Arc<LoggerCallback>>,
}

impl OptionsMustOutliveDB {
    pub(crate) fn clone(&self) -> Self {
        Self {
            env: self.env.clone(),
            row_cache: self.row_cache.clone(),
            blob_cache: self.blob_cache.clone(),
            block_based: self
                .block_based
                .as_ref()
                .map(BlockBasedOptionsMustOutliveDB::clone),
            write_buffer_manager: self.write_buffer_manager.clone(),
            sst_file_manager: self.sst_file_manager.clone(),
            log_callback: self.log_callback.clone(),
            comparator: self.comparator.clone(),
            compaction_filter: self.compaction_filter.clone(),
            logger_callback: self.logger_callback.clone(),
        }
    }
}

/// Stores a `rocksdb_comparator_t` and destroys it when dropped.
///
/// This has an unsafe implementation of Send and Sync because it wraps a RocksDB pointer that
/// is safe to share between threads.
struct OwnedComparator {
    inner: NonNull<ffi::rocksdb_comparator_t>,
}

impl OwnedComparator {
    fn new(inner: NonNull<ffi::rocksdb_comparator_t>) -> Self {
        Self { inner }
    }
}

impl Drop for OwnedComparator {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_comparator_destroy(self.inner.as_ptr());
        }
    }
}

/// Stores a `rocksdb_compactionfilter_t` and destroys it when dropped.
///
/// This has an unsafe implementation of Send and Sync because it wraps a RocksDB pointer that
/// is safe to share between threads.
struct OwnedCompactionFilter {
    inner: NonNull<ffi::rocksdb_compactionfilter_t>,
}

impl OwnedCompactionFilter {
    fn new(inner: NonNull<ffi::rocksdb_compactionfilter_t>) -> Self {
        Self { inner }
    }
}

impl Drop for OwnedCompactionFilter {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_compactionfilter_destroy(self.inner.as_ptr());
        }
    }
}

#[derive(Default)]
struct BlockBasedOptionsMustOutliveDB {
    block_cache: Option<Cache>,
}

impl BlockBasedOptionsMustOutliveDB {
    fn clone(&self) -> Self {
        Self {
            block_cache: self.block_cache.clone(),
        }
    }
}

/// Database-wide options around performance and behavior.
///
/// Please read the official tuning [guide](https://github.com/facebook/rocksdb/wiki/RocksDB-Tuning-Guide)
/// and most importantly, measure performance under realistic workloads with realistic hardware.
///
/// # Examples
///
/// ```
/// use rust_rocksdb::{Options, DB};
/// use rust_rocksdb::DBCompactionStyle;
///
/// fn badly_tuned_for_somebody_elses_disk() -> DB {
///    let path = "path/for/rocksdb/storageX";
///    let mut opts = Options::default();
///    opts.create_if_missing(true);
///    opts.set_max_open_files(10000);
///    opts.set_use_fsync(false);
///    opts.set_bytes_per_sync(8388608);
///    opts.optimize_for_point_lookup(1024);
///    opts.set_table_cache_num_shard_bits(6);
///    opts.set_max_write_buffer_number(32);
///    opts.set_write_buffer_size(536870912);
///    opts.set_target_file_size_base(1073741824);
///    opts.set_min_write_buffer_number_to_merge(4);
///    opts.set_level_zero_stop_writes_trigger(2000);
///    opts.set_level_zero_slowdown_writes_trigger(0);
///    opts.set_compaction_style(DBCompactionStyle::Universal);
///    opts.set_disable_auto_compactions(true);
///
///    DB::open(&opts, path).unwrap()
/// }
/// ```
pub struct Options {
    pub(crate) inner: *mut ffi::rocksdb_options_t,
    pub(crate) outlive: OptionsMustOutliveDB,
}

/// Optionally disable WAL or sync for this write.
///
/// # Examples
///
/// Making an unsafe write of a batch:
///
/// ```
/// use rust_rocksdb::{DB, Options, WriteBatch, WriteOptions};
///
/// let tempdir = tempfile::Builder::new()
///     .prefix("_path_for_rocksdb_storageY1")
///     .tempdir()
///     .expect("Failed to create temporary path for the _path_for_rocksdb_storageY1");
/// let path = tempdir.path();
/// {
///     let db = DB::open_default(path).unwrap();
///     let mut batch = WriteBatch::default();
///     batch.put(b"my key", b"my value");
///     batch.put(b"key2", b"value2");
///     batch.put(b"key3", b"value3");
///
///     let mut write_options = WriteOptions::default();
///     write_options.set_sync(false);
///     write_options.disable_wal(true);
///
///     db.write_opt(&batch, &write_options);
/// }
/// let _ = DB::destroy(&Options::default(), path);
/// ```
pub struct WriteOptions {
    pub(crate) inner: *mut ffi::rocksdb_writeoptions_t,
}

pub struct LruCacheOptions {
    pub(crate) inner: *mut ffi::rocksdb_lru_cache_options_t,
}

/// Optionally wait for the memtable flush to be performed.
///
/// # Examples
///
/// Manually flushing the memtable:
///
/// ```
/// use rust_rocksdb::{DB, Options, FlushOptions};
///
/// let tempdir = tempfile::Builder::new()
///     .prefix("_path_for_rocksdb_storageY2")
///     .tempdir()
///     .expect("Failed to create temporary path for the _path_for_rocksdb_storageY2");
/// let path = tempdir.path();
/// {
///     let db = DB::open_default(path).unwrap();
///
///     let mut flush_options = FlushOptions::default();
///     flush_options.set_wait(true);
///
///     db.flush_opt(&flush_options);
/// }
/// let _ = DB::destroy(&Options::default(), path);
/// ```
pub struct FlushOptions {
    pub(crate) inner: *mut ffi::rocksdb_flushoptions_t,
}

/// For configuring block-based file storage.
pub struct BlockBasedOptions {
    pub(crate) inner: *mut ffi::rocksdb_block_based_table_options_t,
    outlive: BlockBasedOptionsMustOutliveDB,
}

pub struct ReadOptions {
    pub(crate) inner: *mut ffi::rocksdb_readoptions_t,
    // The `ReadOptions` owns a copy of the timestamp and iteration bounds.
    // This is necessary to ensure the pointers we pass over the FFI live as
    // long as the `ReadOptions`. This way, when performing the read operation,
    // the pointers are guaranteed to be valid.
    timestamp: Option<Vec<u8>>,
    iter_start_ts: Option<Vec<u8>>,
    iterate_upper_bound: Option<Vec<u8>>,
    iterate_lower_bound: Option<Vec<u8>>,
}

/// Configuration of cuckoo-based storage.
pub struct CuckooTableOptions {
    pub(crate) inner: *mut ffi::rocksdb_cuckoo_table_options_t,
}

/// For configuring external files ingestion.
///
/// # Examples
///
/// Move files instead of copying them:
///
/// ```
/// use rust_rocksdb::{DB, IngestExternalFileOptions, SstFileWriter, Options};
///
/// let writer_opts = Options::default();
/// let mut writer = SstFileWriter::create(&writer_opts);
/// let tempdir = tempfile::Builder::new()
///     .tempdir()
///     .expect("Failed to create temporary folder for the _path_for_sst_file");
/// let path1 = tempdir.path().join("_path_for_sst_file");
/// writer.open(path1.clone()).unwrap();
/// writer.put(b"k1", b"v1").unwrap();
/// writer.finish().unwrap();
///
/// let tempdir2 = tempfile::Builder::new()
///     .prefix("_path_for_rocksdb_storageY3")
///     .tempdir()
///     .expect("Failed to create temporary path for the _path_for_rocksdb_storageY3");
/// let path2 = tempdir2.path();
/// {
///   let db = DB::open_default(&path2).unwrap();
///   let mut ingest_opts = IngestExternalFileOptions::default();
///   ingest_opts.set_move_files(true);
///   db.ingest_external_file_opts(&ingest_opts, vec![path1]).unwrap();
/// }
/// let _ = DB::destroy(&Options::default(), path2);
/// ```
pub struct IngestExternalFileOptions {
    pub(crate) inner: *mut ffi::rocksdb_ingestexternalfileoptions_t,
}

// Safety note: auto-implementing Send on most db-related types is prevented by the inner FFI
// pointer. In most cases, however, this pointer is Send-safe because it is never aliased and
// rocksdb internally does not rely on thread-local information for its user-exposed types.
unsafe impl Send for Options {}
unsafe impl Send for WriteOptions {}
unsafe impl Send for LruCacheOptions {}
unsafe impl Send for FlushOptions {}
unsafe impl Send for BlockBasedOptions {}
unsafe impl Send for CuckooTableOptions {}
unsafe impl Send for ReadOptions {}
unsafe impl Send for IngestExternalFileOptions {}
unsafe impl Send for CompactOptions {}
unsafe impl Send for ImportColumnFamilyOptions {}
unsafe impl Send for OwnedComparator {}
unsafe impl Send for OwnedCompactionFilter {}

// Sync is similarly safe for many types because they do not expose interior mutability, and their
// use within the rocksdb library is generally behind a const reference
unsafe impl Sync for Options {}
unsafe impl Sync for WriteOptions {}
unsafe impl Sync for LruCacheOptions {}
unsafe impl Sync for FlushOptions {}
unsafe impl Sync for BlockBasedOptions {}
unsafe impl Sync for CuckooTableOptions {}
unsafe impl Sync for ReadOptions {}
unsafe impl Sync for IngestExternalFileOptions {}
unsafe impl Sync for CompactOptions {}
unsafe impl Sync for ImportColumnFamilyOptions {}
unsafe impl Sync for OwnedComparator {}
unsafe impl Sync for OwnedCompactionFilter {}

impl Drop for Options {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_options_destroy(self.inner);
        }
    }
}

impl Clone for Options {
    fn clone(&self) -> Self {
        let inner = unsafe { ffi::rocksdb_options_create_copy(self.inner) };
        assert!(!inner.is_null(), "Could not copy RocksDB options");

        Self {
            inner,
            outlive: self.outlive.clone(),
        }
    }
}

impl Drop for BlockBasedOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_block_based_options_destroy(self.inner);
        }
    }
}

impl Drop for CuckooTableOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_cuckoo_options_destroy(self.inner);
        }
    }
}

impl Drop for FlushOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_flushoptions_destroy(self.inner);
        }
    }
}

impl Drop for WriteOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_writeoptions_destroy(self.inner);
        }
    }
}

impl Drop for LruCacheOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_lru_cache_options_destroy(self.inner);
        }
    }
}

impl Drop for ReadOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_readoptions_destroy(self.inner);
        }
    }
}

impl Drop for IngestExternalFileOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_destroy(self.inner);
        }
    }
}

impl BlockBasedOptions {
    /// Approximate size of user data packed per block. Note that the
    /// block size specified here corresponds to uncompressed data. The
    /// actual size of the unit read from disk may be smaller if
    /// compression is enabled. This parameter can be changed dynamically.
    pub fn set_block_size(&mut self, size: usize) {
        unsafe {
            ffi::rocksdb_block_based_options_set_block_size(self.inner, size);
        }
    }

    /// Block size for partitioned metadata. Currently applied to indexes when
    /// kTwoLevelIndexSearch is used and to filters when partition_filters is used.
    /// Note: Since in the current implementation the filters and index partitions
    /// are aligned, an index/filter block is created when either index or filter
    /// block size reaches the specified limit.
    ///
    /// Note: this limit is currently applied to only index blocks; a filter
    /// partition is cut right after an index block is cut.
    pub fn set_metadata_block_size(&mut self, size: usize) {
        unsafe {
            ffi::rocksdb_block_based_options_set_metadata_block_size(self.inner, size as u64);
        }
    }

    /// Note: currently this option requires kTwoLevelIndexSearch to be set as
    /// well.
    ///
    /// Use partitioned full filters for each SST file. This option is
    /// incompatible with block-based filters.
    pub fn set_partition_filters(&mut self, size: bool) {
        unsafe {
            ffi::rocksdb_block_based_options_set_partition_filters(self.inner, c_uchar::from(size));
        }
    }

    /// Sets global cache for blocks (user data is stored in a set of blocks, and
    /// a block is the unit of reading from disk).
    ///
    /// If set, use the specified cache for blocks.
    /// By default, rocksdb will automatically create and use an 8MB internal cache.
    pub fn set_block_cache(&mut self, cache: &Cache) {
        unsafe {
            ffi::rocksdb_block_based_options_set_block_cache(self.inner, cache.0.inner.as_ptr());
        }
        self.outlive.block_cache = Some(cache.clone());
    }

    /// Disable block cache
    pub fn disable_cache(&mut self) {
        unsafe {
            ffi::rocksdb_block_based_options_set_no_block_cache(self.inner, c_uchar::from(true));
        }
    }

    /// Sets a [Bloom filter](https://github.com/facebook/rocksdb/wiki/RocksDB-Bloom-Filter)
    /// policy to reduce disk reads.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::BlockBasedOptions;
    ///
    /// let mut opts = BlockBasedOptions::default();
    /// opts.set_bloom_filter(10.0, true);
    /// ```
    pub fn set_bloom_filter(&mut self, bits_per_key: c_double, block_based: bool) {
        unsafe {
            let bloom = if block_based {
                ffi::rocksdb_filterpolicy_create_bloom(bits_per_key as _)
            } else {
                ffi::rocksdb_filterpolicy_create_bloom_full(bits_per_key as _)
            };

            ffi::rocksdb_block_based_options_set_filter_policy(self.inner, bloom);
        }
    }

    /// Sets a [Ribbon filter](http://rocksdb.org/blog/2021/12/29/ribbon-filter.html)
    /// policy to reduce disk reads.
    ///
    /// Ribbon filters use less memory in exchange for slightly more CPU usage
    /// compared to an equivalent bloom filter.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::BlockBasedOptions;
    ///
    /// let mut opts = BlockBasedOptions::default();
    /// opts.set_ribbon_filter(10.0);
    /// ```
    pub fn set_ribbon_filter(&mut self, bloom_equivalent_bits_per_key: c_double) {
        unsafe {
            let ribbon = ffi::rocksdb_filterpolicy_create_ribbon(bloom_equivalent_bits_per_key);
            ffi::rocksdb_block_based_options_set_filter_policy(self.inner, ribbon);
        }
    }

    /// Sets a hybrid [Ribbon filter](http://rocksdb.org/blog/2021/12/29/ribbon-filter.html)
    /// policy to reduce disk reads.
    ///
    /// Uses Bloom filters before the given level, and Ribbon filters for all
    /// other levels. This combines the memory savings from Ribbon filters
    /// with the lower CPU usage of Bloom filters.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::BlockBasedOptions;
    ///
    /// let mut opts = BlockBasedOptions::default();
    /// opts.set_hybrid_ribbon_filter(10.0, 2);
    /// ```
    pub fn set_hybrid_ribbon_filter(
        &mut self,
        bloom_equivalent_bits_per_key: c_double,
        bloom_before_level: c_int,
    ) {
        unsafe {
            let ribbon = ffi::rocksdb_filterpolicy_create_ribbon_hybrid(
                bloom_equivalent_bits_per_key,
                bloom_before_level,
            );
            ffi::rocksdb_block_based_options_set_filter_policy(self.inner, ribbon);
        }
    }

    /// Whether to put index/filter blocks in the block cache. When false,
    /// each "table reader" object will pre-load index/filter blocks during
    /// table initialization. Index and filter partition blocks always use
    /// block cache regardless of this option.
    ///
    /// Default: false
    pub fn set_cache_index_and_filter_blocks(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_block_based_options_set_cache_index_and_filter_blocks(
                self.inner,
                c_uchar::from(v),
            );
        }
    }

    /// If `cache_index_and_filter_blocks` is enabled, cache index and filter
    /// blocks with high priority. Depending on the block cache implementation,
    /// index, filter, and other metadata blocks may be less likely to be
    /// evicted than data blocks when this is set to true.
    ///
    /// Default: true.
    pub fn set_cache_index_and_filter_blocks_with_high_priority(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_block_based_options_set_cache_index_and_filter_blocks_with_high_priority(
                self.inner,
                c_uchar::from(v),
            );
        }
    }

    /// Defines the index type to be used for SS-table lookups.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{BlockBasedOptions, BlockBasedIndexType, Options};
    ///
    /// let mut opts = Options::default();
    /// let mut block_opts = BlockBasedOptions::default();
    /// block_opts.set_index_type(BlockBasedIndexType::HashSearch);
    /// ```
    pub fn set_index_type(&mut self, index_type: BlockBasedIndexType) {
        let index = index_type as i32;
        unsafe {
            ffi::rocksdb_block_based_options_set_index_type(self.inner, index);
        }
    }

    /// Selects the search algorithm used inside each index block at lookup
    /// time.
    ///
    /// Use [`IndexBlockSearchType::Interpolation`] when keys in index blocks
    /// are known to be uniformly distributed and the byte-wise comparator is
    /// in use, or [`IndexBlockSearchType::Auto`] to let RocksDB choose per
    /// block. `Auto` requires the corresponding write-path threshold to be
    /// set via [`Self::set_uniform_cv_threshold`]; otherwise it falls back to
    /// binary search.
    ///
    /// Default: `IndexBlockSearchType::Binary`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{BlockBasedOptions, IndexBlockSearchType};
    ///
    /// let mut block_opts = BlockBasedOptions::default();
    /// block_opts.set_index_block_search_type(IndexBlockSearchType::Auto);
    /// block_opts.set_uniform_cv_threshold(0.2);
    /// ```
    pub fn set_index_block_search_type(&mut self, search_type: IndexBlockSearchType) {
        unsafe {
            ffi::rocksdb_block_based_options_set_index_block_search_type(
                self.inner,
                search_type as c_int,
            );
        }
    }

    /// Coefficient of variation (CV) threshold used on the write path to
    /// decide whether an index block's keys are "uniform" enough to benefit
    /// from interpolation search at read time. When the CV of key gaps within
    /// an index block is below this threshold, the per-block "is_uniform"
    /// footer bit is set, which
    /// [`IndexBlockSearchType::Auto`](Self::set_index_block_search_type)
    /// consults at lookup time.
    ///
    /// Any negative value disables the feature; the magnitude is ignored.
    /// With the default disabled value, [`IndexBlockSearchType::Auto`]
    /// degenerates to binary search at read time because the per-block
    /// "is_uniform" bit is never written. The recommended enabled range is
    /// `0.0..=1.0`; a typical value is `0.2`.
    ///
    /// Note: currently only index blocks honour this; the value has no effect
    /// on data blocks today.
    ///
    /// Default: `-1.0` (disabled)
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::BlockBasedOptions;
    ///
    /// let mut block_opts = BlockBasedOptions::default();
    /// block_opts.set_uniform_cv_threshold(0.2);
    /// ```
    pub fn set_uniform_cv_threshold(&mut self, threshold: f64) {
        unsafe {
            ffi::rocksdb_block_based_options_set_uniform_cv_threshold(self.inner, threshold);
        }
    }

    /// If cache_index_and_filter_blocks is true and the below is true, then
    /// filter and index blocks are stored in the cache, but a reference is
    /// held in the "table reader" object so the blocks are pinned and only
    /// evicted from cache when the table reader is freed.
    ///
    /// Default: false.
    pub fn set_pin_l0_filter_and_index_blocks_in_cache(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_block_based_options_set_pin_l0_filter_and_index_blocks_in_cache(
                self.inner,
                c_uchar::from(v),
            );
        }
    }

    /// If cache_index_and_filter_blocks is true and the below is true, then
    /// the top-level index of partitioned filter and index blocks are stored in
    /// the cache, but a reference is held in the "table reader" object so the
    /// blocks are pinned and only evicted from cache when the table reader is
    /// freed. This is not limited to l0 in LSM tree.
    ///
    /// Default: true.
    pub fn set_pin_top_level_index_and_filter(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_block_based_options_set_pin_top_level_index_and_filter(
                self.inner,
                c_uchar::from(v),
            );
        }
    }

    /// Format version, reserved for backward compatibility.
    ///
    /// See full [list](https://github.com/facebook/rocksdb/blob/v11.8.1/include/rocksdb/table.h#L702-L731)
    /// of the supported versions.
    ///
    /// Default: 7, which needs RocksDB 10.4.0 or newer to read. Lower it if
    /// older readers have to open the files.
    pub fn set_format_version(&mut self, version: i32) {
        unsafe {
            ffi::rocksdb_block_based_options_set_format_version(self.inner, version);
        }
    }

    /// Use delta encoding to compress keys in blocks.
    /// ReadOptions::pin_data requires this option to be disabled.
    ///
    /// Default: true
    pub fn set_use_delta_encoding(&mut self, enable: bool) {
        unsafe {
            ffi::rocksdb_block_based_options_set_use_delta_encoding(
                self.inner,
                c_uchar::from(enable),
            );
        }
    }

    /// Number of keys between restart points for delta encoding of keys.
    /// This parameter can be changed dynamically. Most clients should
    /// leave this parameter alone. The minimum value allowed is 1. Any smaller
    /// value will be silently overwritten with 1.
    ///
    /// Default: 16.
    pub fn set_block_restart_interval(&mut self, interval: i32) {
        unsafe {
            ffi::rocksdb_block_based_options_set_block_restart_interval(self.inner, interval);
        }
    }

    /// Same as block_restart_interval but used for the index block.
    /// If you don't plan to run RocksDB before version 5.16 and you are
    /// using `index_block_restart_interval` > 1, you should
    /// probably set the `format_version` to >= 4 as it would reduce the index size.
    ///
    /// Default: 1.
    pub fn set_index_block_restart_interval(&mut self, interval: i32) {
        unsafe {
            ffi::rocksdb_block_based_options_set_index_block_restart_interval(self.inner, interval);
        }
    }

    /// Set the data block index type for point lookups:
    ///  `DataBlockIndexType::BinarySearch` to use binary search within the data block.
    ///  `DataBlockIndexType::BinaryAndHash` to use the data block hash index in combination with
    ///  the normal binary search.
    ///
    /// The hash table utilization ratio is adjustable using [`set_data_block_hash_ratio`](#method.set_data_block_hash_ratio), which is
    /// valid only when using `DataBlockIndexType::BinaryAndHash`.
    ///
    /// Default: `BinarySearch`
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{BlockBasedOptions, DataBlockIndexType, Options};
    ///
    /// let mut opts = Options::default();
    /// let mut block_opts = BlockBasedOptions::default();
    /// block_opts.set_data_block_index_type(DataBlockIndexType::BinaryAndHash);
    /// block_opts.set_data_block_hash_ratio(0.85);
    /// ```
    pub fn set_data_block_index_type(&mut self, index_type: DataBlockIndexType) {
        let index_t = index_type as i32;
        unsafe {
            ffi::rocksdb_block_based_options_set_data_block_index_type(self.inner, index_t);
        }
    }

    /// Set the data block hash index utilization ratio.
    ///
    /// The smaller the utilization ratio, the less hash collisions happen, and so reduce the risk for a
    /// point lookup to fall back to binary search due to the collisions. A small ratio means faster
    /// lookup at the price of more space overhead.
    ///
    /// Default: 0.75
    pub fn set_data_block_hash_ratio(&mut self, ratio: f64) {
        unsafe {
            ffi::rocksdb_block_based_options_set_data_block_hash_ratio(self.inner, ratio);
        }
    }

    /// If false, place only prefixes in the filter, not whole keys.
    ///
    /// Defaults to true.
    pub fn set_whole_key_filtering(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_block_based_options_set_whole_key_filtering(self.inner, c_uchar::from(v));
        }
    }

    /// Use the specified checksum type.
    /// Newly created table files will be protected with this checksum type.
    /// Old table files will still be readable, even though they have different checksum type.
    pub fn set_checksum_type(&mut self, checksum_type: ChecksumType) {
        unsafe {
            ffi::rocksdb_block_based_options_set_checksum(self.inner, checksum_type as c_char);
        }
    }

    /// If true, generate Bloom/Ribbon filters that minimize memory internal
    /// fragmentation.
    /// See official [wiki](
    /// https://github.com/facebook/rocksdb/wiki/RocksDB-Bloom-Filter#reducing-internal-fragmentation)
    /// for more information.
    ///
    /// Default: true.
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::BlockBasedOptions;
    ///
    /// let mut opts = BlockBasedOptions::default();
    /// opts.set_bloom_filter(10.0, true);
    /// opts.set_optimize_filters_for_memory(true);
    /// ```
    pub fn set_optimize_filters_for_memory(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_block_based_options_set_optimize_filters_for_memory(
                self.inner,
                c_uchar::from(v),
            );
        }
    }

    /// The tier of block-based tables whose top-level index into metadata
    /// partitions will be pinned. Currently indexes and filters may be
    /// partitioned.
    ///
    /// Note `cache_index_and_filter_blocks` must be true for this option to have
    /// any effect. Otherwise any top-level index into metadata partitions would be
    /// held in table reader memory, outside the block cache.
    ///
    /// Default: `BlockBasedPinningTier:Fallback`
    ///
    /// # Example
    ///
    /// ```
    /// use rust_rocksdb::{BlockBasedOptions, BlockBasedPinningTier, Options};
    ///
    /// let mut opts = Options::default();
    /// let mut block_opts = BlockBasedOptions::default();
    /// block_opts.set_top_level_index_pinning_tier(BlockBasedPinningTier::FlushAndSimilar);
    /// ```
    pub fn set_top_level_index_pinning_tier(&mut self, tier: BlockBasedPinningTier) {
        unsafe {
            ffi::rocksdb_block_based_options_set_top_level_index_pinning_tier(
                self.inner,
                tier as c_int,
            );
        }
    }

    /// The tier of block-based tables whose metadata partitions will be pinned.
    /// Currently indexes and filters may be partitioned.
    ///
    /// Default: `BlockBasedPinningTier:Fallback`
    ///
    /// # Example
    ///
    /// ```
    /// use rust_rocksdb::{BlockBasedOptions, BlockBasedPinningTier, Options};
    ///
    /// let mut opts = Options::default();
    /// let mut block_opts = BlockBasedOptions::default();
    /// block_opts.set_partition_pinning_tier(BlockBasedPinningTier::FlushAndSimilar);
    /// ```
    pub fn set_partition_pinning_tier(&mut self, tier: BlockBasedPinningTier) {
        unsafe {
            ffi::rocksdb_block_based_options_set_partition_pinning_tier(self.inner, tier as c_int);
        }
    }

    /// The tier of block-based tables whose unpartitioned metadata blocks will be
    /// pinned.
    ///
    /// Note `cache_index_and_filter_blocks` must be true for this option to have
    /// any effect. Otherwise the unpartitioned meta-blocks would be held in table
    /// reader memory, outside the block cache.
    ///
    /// Default: `BlockBasedPinningTier:Fallback`
    ///
    /// # Example
    ///
    /// ```
    /// use rust_rocksdb::{BlockBasedOptions, BlockBasedPinningTier, Options};
    ///
    /// let mut opts = Options::default();
    /// let mut block_opts = BlockBasedOptions::default();
    /// block_opts.set_unpartitioned_pinning_tier(BlockBasedPinningTier::FlushAndSimilar);
    /// ```
    pub fn set_unpartitioned_pinning_tier(&mut self, tier: BlockBasedPinningTier) {
        unsafe {
            ffi::rocksdb_block_based_options_set_unpartitioned_pinning_tier(
                self.inner,
                tier as c_int,
            );
        }
    }

    /// Align data blocks on lesser of page size and block size
    pub fn get_block_align(&self) -> bool {
        unsafe { ffi::rocksdb_block_based_options_get_block_align(self.inner) != 0 }
    }

    /// Number of keys between restart points for delta encoding of keys. This parameter can
    /// be changed dynamically.  Most clients should leave this parameter alone.  The minimum
    /// value allowed is 1.  Any smaller value will be silently overwritten with 1.
    pub fn get_block_restart_interval(&self) -> c_int {
        unsafe { ffi::rocksdb_block_based_options_get_block_restart_interval(self.inner) }
    }

    /// Approximate size of user data packed per block.  Note that the block size specified
    /// here corresponds to uncompressed data.  The actual size of the unit read from disk may
    /// be smaller if compression is enabled.  This parameter can be changed dynamically.
    pub fn get_block_size(&self) -> u64 {
        unsafe { ffi::rocksdb_block_based_options_get_block_size(self.inner) }
    }

    /// This is used to close a block before it reaches the configured 'block_size'. If the
    /// percentage of free space in the current block is less than this specified number and
    /// adding a new record to the block will exceed the configured block size, then this
    /// block will be closed and the new record will be written to the next block.
    pub fn get_block_size_deviation(&self) -> c_int {
        unsafe { ffi::rocksdb_block_based_options_get_block_size_deviation(self.inner) }
    }

    /// TODO(kailiu) Temporarily disable this feature by making the default value to be false.
    ///
    /// TODO(ajkr) we need to update names of variables controlling meta-block caching as they
    /// should now apply to range tombstone and compression dictionary meta-blocks, in
    /// addition to index and filter meta-blocks.
    ///
    /// Whether to put index/filter blocks in the block cache. When false, each "table reader"
    /// object will pre-load index/filter blocks during table initialization. Index and filter
    /// partition blocks always use block cache regardless of this option.
    pub fn get_cache_index_and_filter_blocks(&self) -> bool {
        unsafe {
            ffi::rocksdb_block_based_options_get_cache_index_and_filter_blocks(self.inner) != 0
        }
    }

    /// If cache_index_and_filter_blocks is enabled, cache index and filter blocks with high
    /// priority. If set to true, depending on implementation of block cache, index, filter,
    /// and other metadata blocks may be less likely to be evicted than data blocks.
    pub fn get_cache_index_and_filter_blocks_with_high_priority(&self) -> bool {
        unsafe {
            ffi::rocksdb_block_based_options_get_cache_index_and_filter_blocks_with_high_priority(
                self.inner,
            ) != 0
        }
    }

    /// Use the specified checksum type. Newly created table files will be protected with this
    /// checksum type. Old table files will still be readable, even though they have different
    /// checksum type.
    pub fn get_checksum(&self) -> c_int {
        unsafe { ffi::rocksdb_block_based_options_get_checksum(self.inner) }
    }

    /// #entries/#buckets. It is valid only when data_block_hash_index_type is
    /// kDataBlockBinaryAndHash.
    pub fn set_data_block_hash_table_util_ratio(&mut self, val: f64) {
        unsafe {
            ffi::rocksdb_block_based_options_set_data_block_hash_table_util_ratio(self.inner, val);
        }
    }

    /// Returns the value of the `data_block_hash_table_util_ratio` option.
    pub fn get_data_block_hash_table_util_ratio(&self) -> f64 {
        unsafe { ffi::rocksdb_block_based_options_get_data_block_hash_table_util_ratio(self.inner) }
    }

    /// Returns the value of the `data_block_index_type` option.
    pub fn get_data_block_index_type(&self) -> c_int {
        unsafe { ffi::rocksdb_block_based_options_get_data_block_index_type(self.inner) }
    }

    /// When both partitioned indexes and partitioned filters are enabled, this enables
    /// independent partitioning boundaries between the two. Most notably, this enables these
    /// metadata blocks to hit their target size much more accurately, as there is often a
    /// disparity between index sizes and filter sizes. This should reduce fragmentation and
    /// metadata overheads in the block cache, as well as treat blocks more fairly for cache
    /// eviction purposes.
    ///
    /// There are no SST format compatibility issues with this option. (All versions of
    /// RocksDB able to read partitioned filters are able to read decoupled partitioned
    /// filters.)
    ///
    /// decouple_partitioned_filters = true is the new default. This option is now DEPRECATED
    /// and might be ignored and/or removed in a future release.
    ///
    /// NOTE: decouple_partitioned_filters = false with partition_filters = true disables
    /// parallel compression (CompressionOptions::parallel_threads sanitized to 1).
    pub fn set_decouple_partitioned_filters(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_block_based_options_set_decouple_partitioned_filters(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `decouple_partitioned_filters` option.
    pub fn get_decouple_partitioned_filters(&self) -> bool {
        unsafe {
            ffi::rocksdb_block_based_options_get_decouple_partitioned_filters(self.inner) != 0
        }
    }

    /// If true, detect corruption during Bloom Filter (format_version >= 5) and Ribbon Filter
    /// construction.
    ///
    /// This is an extra check that is only useful in detecting software bugs or CPU+memory
    /// malfunction. Turning on this feature increases filter construction time by 30%.
    ///
    /// TODO: optimize this performance
    pub fn set_detect_filter_construct_corruption(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_block_based_options_set_detect_filter_construct_corruption(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `detect_filter_construct_corruption` option.
    pub fn get_detect_filter_construct_corruption(&self) -> bool {
        unsafe {
            ffi::rocksdb_block_based_options_get_detect_filter_construct_corruption(self.inner) != 0
        }
    }

    /// Store index blocks on disk in compressed format. Changing this option to false  will
    /// avoid the overhead of decompression if index blocks are evicted and read back
    pub fn set_enable_index_compression(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_block_based_options_set_enable_index_compression(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `enable_index_compression` option.
    pub fn get_enable_index_compression(&self) -> bool {
        unsafe { ffi::rocksdb_block_based_options_get_enable_index_compression(self.inner) != 0 }
    }

    /// EXPERIMENTAL
    ///
    /// Return an error Status if a user_defined_index_factory is configured, but there's no
    /// corresponding UDI block in the SST file being opened. When use_udi_as_primary_index is
    /// true, this check is automatically enforced (a missing UDI block is always an error in
    /// primary mode).
    pub fn set_fail_if_no_udi_on_open(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_block_based_options_set_fail_if_no_udi_on_open(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `fail_if_no_udi_on_open` option.
    pub fn get_fail_if_no_udi_on_open(&self) -> bool {
        unsafe { ffi::rocksdb_block_based_options_get_fail_if_no_udi_on_open(self.inner) != 0 }
    }

    /// We currently have these format versions: 0 - 1 -- No longer supported. Attempting to
    /// read files with these format versions will return an error. To upgrade, load the data
    /// with RocksDB >= 4.6.0 and < 11.0.0, then run a full compaction.
    /// - Can be read by RocksDB's versions since 3.10. Changes the way we encode compressed
    ///   blocks with LZ4, BZip2 and Zlib compression. If you don't plan to run RocksDB
    ///   before version 3.10, you should probably use this.
    /// - Can be read by RocksDB's versions since 5.15. Changes the way we encode the keys
    ///   in index blocks. If you don't plan to run RocksDB before version 5.15, you should
    ///   probably use this. This option only affects newly written tables. When reading
    ///   existing tables, the information about version is read from the footer.
    /// - Can be read by RocksDB's versions since 5.16. Changes the way we encode the values
    ///   in index blocks. If you don't plan to run RocksDB before version 5.16 and you are
    ///   using index_block_restart_interval > 1, you should probably use this as it would
    ///   reduce the index size. This option only affects newly written tables. When reading
    ///   existing tables, the information about version is read from the footer.
    /// - Can be read by RocksDB's versions since 6.6.0. Full and partitioned filters use a
    ///   generally faster and more accurate Bloom filter implementation, with a different
    ///   schema.
    /// - Modified the file footer and checksum matching so that SST data misplaced within
    ///   or between files is as likely to fail checksum verification as random corruption.
    ///   Also checksum-protects SST footer. Can be read by RocksDB versions >= 8.6.0.
    /// - Support for custom compression algorithms with a CompressionManager using a
    ///   non-built-in CompatibilityName(). See `compression_manager` in
    ///   ColumnFamilyOptions. Also changes the format of TableProperties field
    ///   `compression_name`. Can be read by RocksDB versions >= 10.4.0.
    ///
    /// Using the default setting of format_version is strongly recommended, so that available
    /// enhancements are adopted eventually and automatically. The default setting will only
    /// update to the latest after thorough production validation and sufficient time and
    /// number of releases have elapsed (6 months recommended) to ensure a clean
    /// downgrade/revert path for users who might only upgrade a few times per year.
    pub fn get_format_version(&self) -> u32 {
        unsafe { ffi::rocksdb_block_based_options_get_format_version(self.inner) }
    }

    /// Same as block_restart_interval but used for the index block.
    pub fn get_index_block_restart_interval(&self) -> c_int {
        unsafe { ffi::rocksdb_block_based_options_get_index_block_restart_interval(self.inner) }
    }

    /// Returns the value of the `index_block_search_type` option.
    pub fn get_index_block_search_type(&self) -> c_int {
        unsafe { ffi::rocksdb_block_based_options_get_index_block_search_type(self.inner) }
    }

    /// Sets the `index_shortening` option.
    pub fn set_index_shortening(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_block_based_options_set_index_shortening(self.inner, val);
        }
    }

    /// Returns the value of the `index_shortening` option.
    pub fn get_index_shortening(&self) -> c_int {
        unsafe { ffi::rocksdb_block_based_options_get_index_shortening(self.inner) }
    }

    /// Returns the value of the `index_type` option.
    pub fn get_index_type(&self) -> c_int {
        unsafe { ffi::rocksdb_block_based_options_get_index_type(self.inner) }
    }

    /// RocksDB does auto-readahead for iterators on noticing more than two reads for a table
    /// file if user doesn't provide readahead_size. The readahead size starts at
    /// initial_auto_readahead_size and doubles on every additional read upto
    /// BlockBasedTableOptions.max_auto_readahead_size. max_auto_readahead_size can also be
    /// configured.
    ///
    /// Scenarios:
    /// - If initial_auto_readahead_size is set 0 then it will disabled the implicit auto
    ///   prefetching irrespective of max_auto_readahead_size.
    /// - If max_auto_readahead_size is set 0, it will disable the internal prefetching
    ///   irrespective of initial_auto_readahead_size.
    /// - If initial_auto_readahead_size > max_auto_readahead_size, then RocksDB will
    ///   sanitize the value of initial_auto_readahead_size to max_auto_readahead_size and
    ///   readahead_size will be max_auto_readahead_size.
    ///
    /// Value should be provided along with KB i.e. 8 * 1024 as it will prefetch the blocks.
    ///
    /// Default: 8 KB (8 * 1024).
    pub fn set_initial_auto_readahead_size(&mut self, val: usize) {
        unsafe {
            ffi::rocksdb_block_based_options_set_initial_auto_readahead_size(self.inner, val);
        }
    }

    /// Returns the value of the `initial_auto_readahead_size` option.
    pub fn get_initial_auto_readahead_size(&self) -> usize {
        unsafe { ffi::rocksdb_block_based_options_get_initial_auto_readahead_size(self.inner) }
    }

    /// RocksDB does auto-readahead for iterators on noticing more than two reads for a table
    /// file if user doesn't provide readahead_size. The readahead starts at
    /// BlockBasedTableOptions.initial_auto_readahead_size (default: 8KB) and doubles on every
    /// additional read upto max_auto_readahead_size and max_auto_readahead_size can be
    /// configured.
    ///
    /// Special Value: 0 - If max_auto_readahead_size is set 0 then it will disable the
    /// implicit auto prefetching. If max_auto_readahead_size provided is less than
    /// initial_auto_readahead_size, then RocksDB will sanitize the
    /// initial_auto_readahead_size and set it to max_auto_readahead_size.
    ///
    /// Value should be provided along with KB i.e. 256 * 1024 as it will prefetch the blocks.
    ///
    /// Found that 256 KB readahead size provides the best performance, based on experiments,
    /// for auto readahead. Experiment data is in PR #3282.
    ///
    /// Default: 256 KB (256 * 1024).
    pub fn set_max_auto_readahead_size(&mut self, val: usize) {
        unsafe {
            ffi::rocksdb_block_based_options_set_max_auto_readahead_size(self.inner, val);
        }
    }

    /// Returns the value of the `max_auto_readahead_size` option.
    pub fn get_max_auto_readahead_size(&self) -> usize {
        unsafe { ffi::rocksdb_block_based_options_get_max_auto_readahead_size(self.inner) }
    }

    /// Target block size for partitioned metadata. Currently applied to indexes when
    /// kTwoLevelIndexSearch is used and to filters when partition_filters is used. When
    /// decouple_partitioned_filters=false (original behavior), there is much more deviation
    /// from this target size. See the comment on decouple_partitioned_filters.
    pub fn get_metadata_block_size(&self) -> u64 {
        unsafe { ffi::rocksdb_block_based_options_get_metadata_block_size(self.inner) }
    }

    /// Disable block cache. If this is set to true, then no block cache will be configured
    /// (block_cache reset to nullptr).
    ///
    /// This option should not be used with SetOptions.
    pub fn get_no_block_cache(&self) -> bool {
        unsafe { ffi::rocksdb_block_based_options_get_no_block_cache(self.inner) != 0 }
    }

    /// RocksDB does auto-readahead for iterators on noticing more than two reads for a table
    /// file if user doesn't provide readahead_size and reads are sequential.
    /// num_file_reads_for_auto_readahead indicates after how many sequential reads internal
    /// auto prefetching should be start.
    ///
    /// For example, if value is 2 then after reading 2 sequential data blocks on third data
    /// block prefetching will start. If set 0, it will start prefetching from the first read.
    ///
    /// This parameter can be changed dynamically by
    /// DB::SetOptions({{"block_based_table_factory",
    /// "{num_file_reads_for_auto_readahead=0;}"}}));
    ///
    /// Changing the value dynamically will only affect files opened after the change.
    ///
    /// Default: 2
    pub fn set_num_file_reads_for_auto_readahead(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_block_based_options_set_num_file_reads_for_auto_readahead(self.inner, val);
        }
    }

    /// Returns the value of the `num_file_reads_for_auto_readahead` option.
    pub fn get_num_file_reads_for_auto_readahead(&self) -> u64 {
        unsafe {
            ffi::rocksdb_block_based_options_get_num_file_reads_for_auto_readahead(self.inner)
        }
    }

    /// Option to generate Bloom/Ribbon filters that minimize memory internal fragmentation.
    ///
    /// When false, malloc_usable_size is not available, or format_version < 5, filters are
    /// generated without regard to internal fragmentation when loaded into memory (historical
    /// behavior). When true (and malloc_usable_size is available and format_version >= 5),
    /// then filters are generated to "round up" and "round down" their sizes to minimize
    /// internal fragmentation when loaded into memory, assuming the reading DB has the same
    /// memory allocation characteristics as the generating DB. This option does not break
    /// forward or backward compatibility.
    ///
    /// While individual filters will vary in bits/key and false positive rate when setting is
    /// true, the implementation attempts to maintain a weighted average FP rate for filters
    /// consistent with this option set to false.
    ///
    /// With Jemalloc for example, this setting is expected to save about 10% of the memory
    /// footprint and block cache charge of filters, while increasing disk usage of filters by
    /// about 1-2% due to encoding efficiency losses with variance in bits/key.
    ///
    /// NOTE: Because some memory counted by block cache might be unmapped pages within
    /// internal fragmentation, this option can increase observed RSS memory usage. With
    /// cache_index_and_filter_blocks=true, this option makes the block cache better at using
    /// space it is allowed. (These issues should not arise with partitioned filters.)
    ///
    /// NOTE: Set to false if you do not trust malloc_usable_size. When set to true, RocksDB
    /// might access an allocated memory object beyond its original size if malloc_usable_size
    /// says it is safe to do so. While this can be considered bad practice, it should not
    /// produce undefined behavior unless malloc_usable_size is buggy or broken.
    pub fn get_optimize_filters_for_memory(&self) -> bool {
        unsafe { ffi::rocksdb_block_based_options_get_optimize_filters_for_memory(self.inner) != 0 }
    }

    /// Note: currently this option requires kTwoLevelIndexSearch to be set as well.
    /// TODO(myabandeh): remove the note above once the limitation is lifted Use partitioned
    /// full filters for each SST file. This option is incompatible with block-based filters.
    /// Filter partition blocks use block cache even when cache_index_and_filter_blocks=false.
    pub fn get_partition_filters(&self) -> bool {
        unsafe { ffi::rocksdb_block_based_options_get_partition_filters(self.inner) != 0 }
    }

    /// DEPRECATED: This option will be removed in a future version. For now, this option
    /// still takes effect by updating each of the following variables that has the default
    /// value, `PinningTier::kFallback`:
    ///
    /// - `MetadataCacheOptions::partition_pinning`
    /// - `MetadataCacheOptions::unpartitioned_pinning`
    ///
    /// The updated value is chosen as follows:
    ///
    /// - `pin_l0_filter_and_index_blocks_in_cache == false` -> `PinningTier::kNone`
    /// - `pin_l0_filter_and_index_blocks_in_cache == true` ->
    ///   `PinningTier::kFlushedAndSimilar`
    ///
    /// To migrate away from this flag, explicitly configure `MetadataCacheOptions` as
    /// described above.
    ///
    /// if cache_index_and_filter_blocks is true and the below is true, then filter and index
    /// blocks are stored in the cache, but a reference is held in the "table reader" object
    /// so the blocks are pinned and only evicted from cache when the table reader is freed.
    pub fn get_pin_l0_filter_and_index_blocks_in_cache(&self) -> bool {
        unsafe {
            ffi::rocksdb_block_based_options_get_pin_l0_filter_and_index_blocks_in_cache(self.inner)
                != 0
        }
    }

    /// DEPRECATED: This option will be removed in a future version. For now, this option
    /// still takes effect by updating `MetadataCacheOptions::top_level_index_pinning` when it
    /// has the default value, `PinningTier::kFallback`.
    ///
    /// The updated value is chosen as follows:
    ///
    /// - `pin_top_level_index_and_filter == false` -> `PinningTier::kNone`
    /// - `pin_top_level_index_and_filter == true` -> `PinningTier::kAll`
    ///
    /// To migrate away from this flag, explicitly configure `MetadataCacheOptions` as
    /// described above.
    ///
    /// If cache_index_and_filter_blocks is true and the below is true, then the top-level
    /// index of partitioned filter and index blocks are stored in the cache, but a reference
    /// is held in the "table reader" object so the blocks are pinned and only evicted from
    /// cache when the table reader is freed. This is not limited to l0 in LSM tree.
    pub fn get_pin_top_level_index_and_filter(&self) -> bool {
        unsafe {
            ffi::rocksdb_block_based_options_get_pin_top_level_index_and_filter(self.inner) != 0
        }
    }

    /// Sets the `prepopulate_block_cache` option.
    pub fn set_prepopulate_block_cache(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_block_based_options_set_prepopulate_block_cache(self.inner, val);
        }
    }

    /// Returns the value of the `prepopulate_block_cache` option.
    pub fn get_prepopulate_block_cache(&self) -> c_int {
        unsafe { ffi::rocksdb_block_based_options_get_prepopulate_block_cache(self.inner) }
    }

    /// If used, For every data block we load into memory, we will create a bitmap of size
    /// ((block_size / `read_amp_bytes_per_bit`) / 8) bytes. This bitmap will be used to
    /// figure out the percentage we actually read of the blocks.
    ///
    /// When this feature is used Tickers::READ_AMP_ESTIMATE_USEFUL_BYTES and
    /// Tickers::READ_AMP_TOTAL_READ_BYTES can be used to calculate the read amplification
    /// using this formula (READ_AMP_TOTAL_READ_BYTES / READ_AMP_ESTIMATE_USEFUL_BYTES)
    ///
    /// value  =>  memory usage (percentage of loaded blocks memory) 1      =>  12.50 % 2
    /// =>  06.25 % 4      =>  03.12 % 8      =>  01.56 % 16     =>  00.78 %
    ///
    /// Note: This number must be a power of 2, if not it will be sanitized to be the next
    /// lowest power of 2, for example a value of 7 will be treated as 4, a value of 19 will
    /// be treated as 16.
    ///
    /// Default: 0 (disabled)
    pub fn set_read_amp_bytes_per_bit(&mut self, val: u32) {
        unsafe {
            ffi::rocksdb_block_based_options_set_read_amp_bytes_per_bit(self.inner, val);
        }
    }

    /// Returns the value of the `read_amp_bytes_per_bit` option.
    pub fn get_read_amp_bytes_per_bit(&self) -> u32 {
        unsafe { ffi::rocksdb_block_based_options_get_read_amp_bytes_per_bit(self.inner) }
    }

    /// When true, data blocks store keys and values separately. Keys are stored at the
    /// beginning of the block, followed by values at the end. This can improve read
    /// performance at a cost of a varint per restart interval (~1 bit per key by default), in
    /// addition to improving compression. Small values or low block_restart_interval may
    /// prefer to set this as false.
    ///
    /// Default: false
    pub fn get_separate_key_value_in_data_block(&self) -> bool {
        unsafe {
            ffi::rocksdb_block_based_options_get_separate_key_value_in_data_block(self.inner) != 0
        }
    }

    /// Align data blocks on super block alignment. Avoid a data block split across super
    /// block boundaries. Works with/without compression.
    ///
    /// Here a "super block" refers to an aligned unit of underlying Filesystem storage for
    /// which there is an extra cost when a random read involves two such super blocks instead
    /// of just one. Configuring that size here suggests inserting padding in the SST file to
    /// avoid a single SST block splitting across two super blocks. Only power-of-two sizes
    /// are supported. See also super_block_alignment_space_overhead_ratio. Default to 0,
    /// which means super block alignment is disabled.
    ///
    /// Super block alignment size. Default to 0, which means super block alignment is
    /// disabled. If it is enabled, it needs to be a power of 2 and higher than block size.
    pub fn set_super_block_alignment_size(&mut self, val: usize) {
        unsafe {
            ffi::rocksdb_block_based_options_set_super_block_alignment_size(self.inner, val);
        }
    }

    /// Returns the value of the `super_block_alignment_size` option.
    pub fn get_super_block_alignment_size(&self) -> usize {
        unsafe { ffi::rocksdb_block_based_options_get_super_block_alignment_size(self.inner) }
    }

    /// This option constrols the storage space overhead of super block alignment. It is used
    /// to calculate the max padding size allowed for super block alignment. It is calculated
    /// in this way. If super_block_alignment_size is 2MB, and
    /// super_block_alignment_overhead_ratio is 128, then the max padding size allowed for
    /// super block alignment is 2MB / 128 = 16KB. Note that, when it is set to 0, super block
    /// alignment is disabled.
    pub fn set_super_block_alignment_space_overhead_ratio(&mut self, val: usize) {
        unsafe {
            ffi::rocksdb_block_based_options_set_super_block_alignment_space_overhead_ratio(
                self.inner, val,
            );
        }
    }

    /// Returns the value of the `super_block_alignment_space_overhead_ratio` option.
    pub fn get_super_block_alignment_space_overhead_ratio(&self) -> usize {
        unsafe {
            ffi::rocksdb_block_based_options_get_super_block_alignment_space_overhead_ratio(
                self.inner,
            )
        }
    }

    /// Coefficient of variation (CV) threshold used to determine if keys in an index block
    /// are uniformly distributed. Lower CV means more "uniform", and the more likely
    /// interpolation search will outperform binary search.
    ///
    /// On the write path, if the CV of key gaps in an index block is less than this
    /// threshold, the "is_uniform" hint is set in that block's footer. To disable (i.e.
    /// always have "is_uniform=false"), set value to -1.
    ///
    /// On the read path, if `BlockSearchType::kAuto` is set, then it will use the is_uniform
    /// hint to select an appropriate search algorithm for the block.
    ///
    /// NOTE: Currently only supports index blocks. May update to include data blocks in the
    /// future.
    pub fn get_uniform_cv_threshold(&self) -> f64 {
        unsafe { ffi::rocksdb_block_based_options_get_uniform_cv_threshold(self.inner) }
    }

    /// Use delta encoding to compress keys in blocks. ReadOptions::pin_data requires this
    /// option to be disabled.
    ///
    /// Default: true
    pub fn get_use_delta_encoding(&self) -> bool {
        unsafe { ffi::rocksdb_block_based_options_get_use_delta_encoding(self.inner) != 0 }
    }

    /// EXPERIMENTAL
    ///
    /// When true and user_defined_index_factory is set, the UDI becomes the primary index for
    /// reads. All reads (including internal operations like compaction and VerifyChecksum)
    /// automatically route through the UDI without needing ReadOptions::table_index_factory.
    ///
    /// Both the standard binary search index and the UDI are always fully built. The standard
    /// index serves as a safety fallback (e.g., for backup/restore or rollback to a non-UDI
    /// configuration). A future refactor will extract the index abstraction to allow skipping
    /// the standard index build when the UDI is primary.
    ///
    /// When the UDI is primary:
    /// - All reads automatically use the UDI (ReadOptions::table_index_factory does not
    ///   need to be set)
    /// - Partitioned index (kTwoLevelIndexSearch) and partitioned filters are incompatible
    ///   with this option
    /// - fail_if_no_udi_on_open is automatically enforced to prevent silent data loss if
    ///   these SSTs are opened without UDI support
    ///
    /// Recommended migration path:
    ///
    /// - Deploy with user_defined_index_factory set but use_udi_as_primary_index=false
    ///   (secondary mode). New SSTs are written with both indexes. Reads use the standard
    ///   index by default.
    ///
    /// - Validate reads through the UDI by setting ReadOptions::table_index_factory on a
    ///   subset of reads.
    ///
    /// - Compact the entire DB to rewrite all pre-existing SSTs with both indexes. All SSTs
    ///   must have a UDI block before proceeding.
    ///
    /// - Enable use_udi_as_primary_index=true. All reads use the UDI.
    ///
    /// Rollback: set use_udi_as_primary_index=false. Since the standard index is always fully
    /// populated, SSTs are immediately readable through the standard index. No compaction is
    /// required. All reads immediately revert to the standard index path.
    ///
    /// Backup/restore: the user_defined_index_factory is a shared_ptr that cannot survive
    /// Options serialization (e.g., GetStringFromDBOptions). Since the standard index is
    /// always fully populated, a restored DB can be opened and read without the factory
    /// (reads fall back to the standard index). Set the factory when opening the restored DB
    /// to resume using the UDI.
    ///
    /// Default: false (UDI is built alongside the standard index as a secondary)
    pub fn set_use_udi_as_primary_index(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_block_based_options_set_use_udi_as_primary_index(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `use_udi_as_primary_index` option.
    pub fn get_use_udi_as_primary_index(&self) -> bool {
        unsafe { ffi::rocksdb_block_based_options_get_use_udi_as_primary_index(self.inner) != 0 }
    }

    /// Verify that decompressing the compressed block gives back the input. This is a
    /// verification mode that we use to detect bugs in compression algorithms.
    pub fn set_verify_compression(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_block_based_options_set_verify_compression(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `verify_compression` option.
    pub fn get_verify_compression(&self) -> bool {
        unsafe { ffi::rocksdb_block_based_options_get_verify_compression(self.inner) != 0 }
    }

    /// If true, place whole keys in the filter (not just prefixes). This must generally be
    /// true for gets to be efficient.
    pub fn get_whole_key_filtering(&self) -> bool {
        unsafe { ffi::rocksdb_block_based_options_get_whole_key_filtering(self.inner) != 0 }
    }
}

impl Default for BlockBasedOptions {
    fn default() -> Self {
        let block_opts = unsafe { ffi::rocksdb_block_based_options_create() };
        assert!(
            !block_opts.is_null(),
            "Could not create RocksDB block based options"
        );

        Self {
            inner: block_opts,
            outlive: BlockBasedOptionsMustOutliveDB::default(),
        }
    }
}

impl CuckooTableOptions {
    /// Determines the utilization of hash tables. Smaller values
    /// result in larger hash tables with fewer collisions.
    /// Default: 0.9
    pub fn set_hash_ratio(&mut self, ratio: f64) {
        unsafe {
            ffi::rocksdb_cuckoo_options_set_hash_ratio(self.inner, ratio);
        }
    }

    /// A property used by builder to determine the depth to go to
    /// to search for a path to displace elements in case of
    /// collision. See Builder.MakeSpaceForKey method. Higher
    /// values result in more efficient hash tables with fewer
    /// lookups but take more time to build.
    /// Default: 100
    pub fn set_max_search_depth(&mut self, depth: u32) {
        unsafe {
            ffi::rocksdb_cuckoo_options_set_max_search_depth(self.inner, depth);
        }
    }

    /// In case of collision while inserting, the builder
    /// attempts to insert in the next cuckoo_block_size
    /// locations before skipping over to the next Cuckoo hash
    /// function. This makes lookups more cache friendly in case
    /// of collisions.
    /// Default: 5
    pub fn set_cuckoo_block_size(&mut self, size: u32) {
        unsafe {
            ffi::rocksdb_cuckoo_options_set_cuckoo_block_size(self.inner, size);
        }
    }

    /// If this option is enabled, user key is treated as uint64_t and its value
    /// is used as hash value directly. This option changes builder's behavior.
    /// Reader ignore this option and behave according to what specified in
    /// table property.
    /// Default: false
    pub fn set_identity_as_first_hash(&mut self, flag: bool) {
        unsafe {
            ffi::rocksdb_cuckoo_options_set_identity_as_first_hash(self.inner, c_uchar::from(flag));
        }
    }

    /// If this option is set to true, module is used during hash calculation.
    /// This often yields better space efficiency at the cost of performance.
    /// If this option is set to false, # of entries in table is constrained to
    /// be power of two, and bit and is used to calculate hash, which is faster in general.
    /// Default: true
    pub fn set_use_module_hash(&mut self, flag: bool) {
        unsafe {
            ffi::rocksdb_cuckoo_options_set_use_module_hash(self.inner, c_uchar::from(flag));
        }
    }

    /// In case of collision while inserting, the builder attempts to insert in the next
    /// cuckoo_block_size locations before skipping over to the next Cuckoo hash function.
    /// This makes lookups more cache friendly in case of collisions.
    pub fn get_cuckoo_block_size(&self) -> u32 {
        unsafe { ffi::rocksdb_cuckoo_options_get_cuckoo_block_size(self.inner) }
    }

    /// @hash_table_ratio: the desired utilization of the hash table used for prefix hashing.
    /// hash_table_ratio = number of prefixes / #buckets in the hash table
    pub fn set_hash_table_ratio(&mut self, val: f64) {
        unsafe {
            ffi::rocksdb_cuckoo_options_set_hash_table_ratio(self.inner, val);
        }
    }

    /// Returns the value of the `hash_table_ratio` option.
    pub fn get_hash_table_ratio(&self) -> f64 {
        unsafe { ffi::rocksdb_cuckoo_options_get_hash_table_ratio(self.inner) }
    }

    /// If this option is enabled, user key is treated as uint64_t and its value is used as
    /// hash value directly. This option changes builder's behavior. Reader ignore this option
    /// and behave according to what specified in table property.
    pub fn get_identity_as_first_hash(&self) -> bool {
        unsafe { ffi::rocksdb_cuckoo_options_get_identity_as_first_hash(self.inner) != 0 }
    }

    /// A property used by builder to determine the depth to go to to search for a path to
    /// displace elements in case of collision. See Builder.MakeSpaceForKey method. Higher
    /// values result in more efficient hash tables with fewer lookups but take more time to
    /// build.
    pub fn get_max_search_depth(&self) -> u32 {
        unsafe { ffi::rocksdb_cuckoo_options_get_max_search_depth(self.inner) }
    }

    /// If this option is set to true, module is used during hash calculation. This often
    /// yields better space efficiency at the cost of performance. If this option is set to
    /// false, # of entries in table is constrained to be power of two, and bit and is used to
    /// calculate hash, which is faster in general.
    pub fn get_use_module_hash(&self) -> bool {
        unsafe { ffi::rocksdb_cuckoo_options_get_use_module_hash(self.inner) != 0 }
    }
}

impl Default for CuckooTableOptions {
    fn default() -> Self {
        let opts = unsafe { ffi::rocksdb_cuckoo_options_create() };
        assert!(!opts.is_null(), "Could not create RocksDB cuckoo options");

        Self { inner: opts }
    }
}

// Verbosity of the LOG.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(i32)]
pub enum LogLevel {
    Debug = 0,
    Info,
    Warn,
    Error,
    Fatal,
    Header,
}

impl LogLevel {
    pub(crate) fn try_from_raw(raw: i32) -> Option<Self> {
        match raw {
            n if n == LogLevel::Debug as i32 => Some(LogLevel::Debug),
            n if n == LogLevel::Info as i32 => Some(LogLevel::Info),
            n if n == LogLevel::Warn as i32 => Some(LogLevel::Warn),
            n if n == LogLevel::Error as i32 => Some(LogLevel::Error),
            n if n == LogLevel::Fatal as i32 => Some(LogLevel::Fatal),
            n if n == LogLevel::Header as i32 => Some(LogLevel::Header),
            _ => None,
        }
    }
}

impl Options {
    /// Constructs the DBOptions and ColumnFamilyDescriptors by loading the
    /// latest RocksDB options file stored in the specified rocksdb database.
    ///
    /// *IMPORTANT*:
    /// ROCKSDB DOES NOT STORE cf ttl in the options file. If you have set it via
    /// [`ColumnFamilyDescriptor::new_with_ttl`] then you need to set it again after loading the options file.
    /// Tll will be set to [`ColumnFamilyTtl::Disabled`] for all column families for your safety.
    pub fn load_latest<P: AsRef<Path>>(
        path: P,
        env: Env,
        ignore_unknown_options: bool,
        cache: Cache,
    ) -> Result<(Options, Vec<ColumnFamilyDescriptor>), Error> {
        let path = to_cpath(path)?;
        let mut db_options: *mut ffi::rocksdb_options_t = null_mut();
        let mut num_column_families: usize = 0;
        let mut column_family_names: *mut *mut c_char = null_mut();
        let mut column_family_options: *mut *mut ffi::rocksdb_options_t = null_mut();
        unsafe {
            ffi_try!(ffi::rocksdb_load_latest_options(
                path.as_ptr(),
                env.0.inner,
                ignore_unknown_options,
                cache.0.inner.as_ptr(),
                &raw mut db_options,
                &raw mut num_column_families,
                &raw mut column_family_names,
                &raw mut column_family_options,
            ));
        }
        let options = Options {
            inner: db_options,
            outlive: OptionsMustOutliveDB::default(),
        };
        // read_column_descriptors frees column_family_names and the column_family_options array.
        // We can't call rocksdb_load_latest_options_destroy because it also frees options, and
        // the individual `column_family_options` pointers. We want to return them.
        let column_families = unsafe {
            Options::read_column_descriptors(
                num_column_families,
                column_family_names,
                column_family_options,
            )
        };
        Ok((options, column_families))
    }

    /// Constructs a new `DBOptions` from `self` and a string `opts_str` with the syntax detailed in the blogpost
    /// [Reading RocksDB options from a file](https://rocksdb.org/blog/2015/02/24/reading-rocksdb-options-from-a-file.html)
    pub fn get_options_from_string<S: AsRef<str>>(
        &mut self,
        opts_str: S,
    ) -> Result<Options, Error> {
        // create the rocksdb_options_t and immediately wrap it so we don't forget to free it
        let options = Options {
            inner: unsafe { ffi::rocksdb_options_create() },
            outlive: OptionsMustOutliveDB::default(),
        };

        let opts_cstr = opts_str.as_ref().into_c_string().map_err(|e| {
            Error::new(format!(
                "options string must not contain NUL (0x00) bytes: {e}"
            ))
        })?;
        unsafe {
            ffi_try!(ffi::rocksdb_get_options_from_string(
                self.inner.cast_const(),
                opts_cstr.as_ptr(),
                options.inner,
            ));
        }
        Ok(options)
    }

    /// Reads column descriptors from C pointers. This frees the `column_family_names` and
    /// `column_family_options` arrays, and the strings contained in `column_family_names`. It does
    /// *not* free the `rocksdb_options_t*` pointers contained in `column_family_options`.
    #[inline]
    unsafe fn read_column_descriptors(
        num_column_families: usize,
        column_family_names: *mut *mut c_char,
        column_family_options: *mut *mut ffi::rocksdb_options_t,
    ) -> Vec<ColumnFamilyDescriptor> {
        let column_family_names_iter = unsafe {
            slice::from_raw_parts(column_family_names, num_column_families)
                .iter()
                .map(|ptr| from_cstr_and_free(*ptr))
        };
        let column_family_options_iter = unsafe {
            slice::from_raw_parts(column_family_options, num_column_families)
                .iter()
                .map(|ptr| Options {
                    inner: *ptr,
                    outlive: OptionsMustOutliveDB::default(),
                })
        };
        let column_descriptors = column_family_names_iter
            .zip(column_family_options_iter)
            .map(|(name, options)| ColumnFamilyDescriptor {
                name,
                options,
                ttl: ColumnFamilyTtl::Disabled,
            })
            .collect::<Vec<_>>();

        // free the arrays
        unsafe {
            // we freed each string in the column_family_names array using from_cstr_and_free
            ffi::rocksdb_free(column_family_names as *mut c_void);
            // we don't want to free the contents of this array because we return it
            ffi::rocksdb_free(column_family_options as *mut c_void);
            column_descriptors
        }
    }

    /// By default, RocksDB uses only one background thread for flush and
    /// compaction. Calling this function will set it up such that total of
    /// `total_threads` is used. Good value for `total_threads` is the number of
    /// cores. You almost definitely want to call this function if your system is
    /// bottlenecked by RocksDB.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.increase_parallelism(3);
    /// ```
    pub fn increase_parallelism(&mut self, parallelism: i32) {
        unsafe {
            ffi::rocksdb_options_increase_parallelism(self.inner, parallelism);
        }
    }

    /// Optimize level style compaction.
    ///
    /// Default values for some parameters in `Options` are not optimized for heavy
    /// workloads and big datasets, which means you might observe write stalls under
    /// some conditions.
    ///
    /// This can be used as one of the starting points for tuning RocksDB options in
    /// such cases.
    ///
    /// Internally, it sets `write_buffer_size`, `min_write_buffer_number_to_merge`,
    /// `max_write_buffer_number`, `level0_file_num_compaction_trigger`,
    /// `target_file_size_base`, `max_bytes_for_level_base`, so it can override if those
    /// parameters were set before.
    ///
    /// It sets buffer sizes so that memory consumption would be constrained by
    /// `memtable_memory_budget`.
    pub fn optimize_level_style_compaction(&mut self, memtable_memory_budget: usize) {
        unsafe {
            ffi::rocksdb_options_optimize_level_style_compaction(
                self.inner,
                memtable_memory_budget as u64,
            );
        }
    }

    /// Optimize universal style compaction.
    ///
    /// Default values for some parameters in `Options` are not optimized for heavy
    /// workloads and big datasets, which means you might observe write stalls under
    /// some conditions.
    ///
    /// This can be used as one of the starting points for tuning RocksDB options in
    /// such cases.
    ///
    /// Internally, it sets `write_buffer_size`, `min_write_buffer_number_to_merge`,
    /// `max_write_buffer_number`, `level0_file_num_compaction_trigger`,
    /// `target_file_size_base`, `max_bytes_for_level_base`, so it can override if those
    /// parameters were set before.
    ///
    /// It sets buffer sizes so that memory consumption would be constrained by
    /// `memtable_memory_budget`.
    pub fn optimize_universal_style_compaction(&mut self, memtable_memory_budget: usize) {
        unsafe {
            ffi::rocksdb_options_optimize_universal_style_compaction(
                self.inner,
                memtable_memory_budget as u64,
            );
        }
    }

    /// If true, the database will be created if it is missing.
    ///
    /// Default: `false`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.create_if_missing(true);
    /// ```
    pub fn create_if_missing(&mut self, create_if_missing: bool) {
        unsafe {
            ffi::rocksdb_options_set_create_if_missing(
                self.inner,
                c_uchar::from(create_if_missing),
            );
        }
    }

    /// If true, any column families that didn't exist when opening the database
    /// will be created.
    ///
    /// Default: `false`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.create_missing_column_families(true);
    /// ```
    pub fn create_missing_column_families(&mut self, create_missing_cfs: bool) {
        unsafe {
            ffi::rocksdb_options_set_create_missing_column_families(
                self.inner,
                c_uchar::from(create_missing_cfs),
            );
        }
    }

    /// Specifies whether an error should be raised if the database already exists.
    ///
    /// Default: false
    pub fn set_error_if_exists(&mut self, enabled: bool) {
        unsafe {
            ffi::rocksdb_options_set_error_if_exists(self.inner, c_uchar::from(enabled));
        }
    }

    /// Enable/disable paranoid checks.
    ///
    /// If true, the implementation will do aggressive checking of the
    /// data it is processing and will stop early if it detects any
    /// errors. This may have unforeseen ramifications: for example, a
    /// corruption of one DB entry may cause a large number of entries to
    /// become unreadable or for the entire DB to become unopenable.
    /// If any of the  writes to the database fails (Put, Delete, Merge, Write),
    /// the database will switch to read-only mode and fail all other
    /// Write operations.
    ///
    /// Default: true
    pub fn set_paranoid_checks(&mut self, enabled: bool) {
        unsafe {
            ffi::rocksdb_options_set_paranoid_checks(self.inner, c_uchar::from(enabled));
        }
    }

    /// A list of paths where SST files can be put into, with its target size.
    /// Newer data is placed into paths specified earlier in the vector while
    /// older data gradually moves to paths specified later in the vector.
    ///
    /// For example, you have a flash device with 10GB allocated for the DB,
    /// as well as a hard drive of 2TB, you should config it to be:
    ///   [{"/flash_path", 10GB}, {"/hard_drive", 2TB}]
    ///
    /// The system will try to guarantee data under each path is close to but
    /// not larger than the target size. But current and future file sizes used
    /// by determining where to place a file are based on best-effort estimation,
    /// which means there is a chance that the actual size under the directory
    /// is slightly more than target size under some workloads. User should give
    /// some buffer room for those cases.
    ///
    /// If none of the paths has sufficient room to place a file, the file will
    /// be placed to the last path anyway, despite to the target size.
    ///
    /// Placing newer data to earlier paths is also best-efforts. User should
    /// expect user files to be placed in higher levels in some extreme cases.
    ///
    /// If left empty, only one path will be used, which is `path` passed when
    /// opening the DB.
    ///
    /// Default: empty
    pub fn set_db_paths(&mut self, paths: &[DBPath]) {
        let mut paths: Vec<_> = paths.iter().map(|path| path.inner.cast_const()).collect();
        let num_paths = paths.len();
        unsafe {
            ffi::rocksdb_options_set_db_paths(self.inner, paths.as_mut_ptr(), num_paths);
        }
    }

    /// Use the specified object to interact with the environment,
    /// e.g. to read/write files, schedule background work, etc. In the near
    /// future, support for doing storage operations such as read/write files
    /// through env will be deprecated in favor of file_system.
    ///
    /// Default: Env::default()
    pub fn set_env(&mut self, env: &Env) {
        unsafe {
            ffi::rocksdb_options_set_env(self.inner, env.0.inner);
        }
        self.outlive.env = Some(env.clone());
    }

    /// Sets the compression algorithm that will be used for compressing blocks.
    ///
    /// Default: `DBCompressionType::Lz4`, falling back to
    /// `DBCompressionType::Snappy` and then `DBCompressionType::None` when the
    /// preceding one is not compiled in. RocksDB 11.5.0 changed this from
    /// Snappy; it affects only column families that never set `compression`,
    /// and only newly written SST files. Existing data stays readable, since
    /// the decompressor is selected per block.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{Options, DBCompressionType};
    ///
    /// let mut opts = Options::default();
    /// opts.set_compression_type(DBCompressionType::Snappy);
    /// ```
    pub fn set_compression_type(&mut self, t: DBCompressionType) {
        unsafe {
            ffi::rocksdb_options_set_compression(self.inner, t as c_int);
        }
    }

    /// Number of threads for parallel compression.
    /// Parallel compression is enabled only if threads > 1.
    /// THE FEATURE IS STILL EXPERIMENTAL
    ///
    /// See [code](https://github.com/facebook/rocksdb/blob/v8.6.7/include/rocksdb/advanced_options.h#L116-L127)
    /// for more information.
    ///
    /// Default: 1
    ///
    /// Examples
    ///
    /// ```
    /// use rust_rocksdb::{Options, DBCompressionType};
    ///
    /// let mut opts = Options::default();
    /// opts.set_compression_type(DBCompressionType::Zstd);
    /// opts.set_compression_options_parallel_threads(3);
    /// ```
    pub fn set_compression_options_parallel_threads(&mut self, num: i32) {
        unsafe {
            ffi::rocksdb_options_set_compression_options_parallel_threads(self.inner, num);
        }
    }

    /// Sets the compression algorithm that will be used for compressing WAL.
    ///
    /// At present, only ZSTD compression is supported!
    ///
    /// Default: `DBCompressionType::None`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{Options, DBCompressionType};
    ///
    /// let mut opts = Options::default();
    /// opts.set_wal_compression_type(DBCompressionType::Zstd);
    /// // Or None to disable it
    /// opts.set_wal_compression_type(DBCompressionType::None);
    /// ```
    pub fn set_wal_compression_type(&mut self, t: DBCompressionType) {
        match t {
            DBCompressionType::None | DBCompressionType::Zstd => unsafe {
                ffi::rocksdb_options_set_wal_compression(self.inner, t as c_int);
            },
            other => unimplemented!("{:?} is not supported for WAL compression", other),
        }
    }

    /// Sets the bottom-most compression algorithm that will be used for
    /// compressing blocks at the bottom-most level.
    ///
    /// Note that to actually enable bottom-most compression configuration after
    /// setting the compression type, it needs to be enabled by calling
    /// [`set_bottommost_compression_options`](#method.set_bottommost_compression_options) or
    /// [`set_bottommost_zstd_max_train_bytes`](#method.set_bottommost_zstd_max_train_bytes) method with `enabled` argument
    /// set to `true`.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{Options, DBCompressionType};
    ///
    /// let mut opts = Options::default();
    /// opts.set_bottommost_compression_type(DBCompressionType::Zstd);
    /// opts.set_bottommost_zstd_max_train_bytes(0, true);
    /// ```
    pub fn set_bottommost_compression_type(&mut self, t: DBCompressionType) {
        unsafe {
            ffi::rocksdb_options_set_bottommost_compression(self.inner, t as c_int);
        }
    }

    /// Different levels can have different compression policies. There
    /// are cases where most lower levels would like to use quick compression
    /// algorithms while the higher levels (which have more data) use
    /// compression algorithms that have better compression but could
    /// be slower. This array, if non-empty, should have an entry for
    /// each level of the database; these override the value specified in
    /// the previous field 'compression'.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{Options, DBCompressionType};
    ///
    /// let mut opts = Options::default();
    /// opts.set_compression_per_level(&[
    ///     DBCompressionType::None,
    ///     DBCompressionType::None,
    ///     DBCompressionType::Snappy,
    ///     DBCompressionType::Snappy,
    ///     DBCompressionType::Snappy
    /// ]);
    /// ```
    pub fn set_compression_per_level(&mut self, level_types: &[DBCompressionType]) {
        unsafe {
            let mut level_types: Vec<_> = level_types.iter().map(|&t| t as c_int).collect();
            ffi::rocksdb_options_set_compression_per_level(
                self.inner,
                level_types.as_mut_ptr(),
                level_types.len() as size_t,
            );
        }
    }

    /// Maximum size of dictionaries used to prime the compression library.
    /// Enabling dictionary can improve compression ratios when there are
    /// repetitions across data blocks.
    ///
    /// The dictionary is created by sampling the SST file data. If
    /// `zstd_max_train_bytes` is nonzero, the samples are passed through zstd's
    /// dictionary generator. Otherwise, the random samples are used directly as
    /// the dictionary.
    ///
    /// When compression dictionary is disabled, we compress and write each block
    /// before buffering data for the next one. When compression dictionary is
    /// enabled, we buffer all SST file data in-memory so we can sample it, as data
    /// can only be compressed and written after the dictionary has been finalized.
    /// So users of this feature may see increased memory usage.
    ///
    /// Default: `0`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_compression_options(4, 5, 6, 7);
    /// ```
    pub fn set_compression_options(
        &mut self,
        w_bits: c_int,
        level: c_int,
        strategy: c_int,
        max_dict_bytes: c_int,
    ) {
        unsafe {
            ffi::rocksdb_options_set_compression_options(
                self.inner,
                w_bits,
                level,
                strategy,
                max_dict_bytes,
            );
        }
    }

    /// Sets compression options for blocks at the bottom-most level.  Meaning
    /// of all settings is the same as in [`set_compression_options`](#method.set_compression_options) method but
    /// affect only the bottom-most compression which is set using
    /// [`set_bottommost_compression_type`](#method.set_bottommost_compression_type) method.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{Options, DBCompressionType};
    ///
    /// let mut opts = Options::default();
    /// opts.set_bottommost_compression_type(DBCompressionType::Zstd);
    /// opts.set_bottommost_compression_options(4, 5, 6, 7, true);
    /// ```
    pub fn set_bottommost_compression_options(
        &mut self,
        w_bits: c_int,
        level: c_int,
        strategy: c_int,
        max_dict_bytes: c_int,
        enabled: bool,
    ) {
        unsafe {
            ffi::rocksdb_options_set_bottommost_compression_options(
                self.inner,
                w_bits,
                level,
                strategy,
                max_dict_bytes,
                c_uchar::from(enabled),
            );
        }
    }

    /// Sets maximum size of training data passed to zstd's dictionary trainer. Using zstd's
    /// dictionary trainer can achieve even better compression ratio improvements than using
    /// `max_dict_bytes` alone.
    ///
    /// The training data will be used to generate a dictionary of max_dict_bytes.
    ///
    /// Default: 0.
    pub fn set_zstd_max_train_bytes(&mut self, value: c_int) {
        unsafe {
            ffi::rocksdb_options_set_compression_options_zstd_max_train_bytes(self.inner, value);
        }
    }

    /// Sets maximum size of training data passed to zstd's dictionary trainer
    /// when compressing the bottom-most level. Using zstd's dictionary trainer
    /// can achieve even better compression ratio improvements than using
    /// `max_dict_bytes` alone.
    ///
    /// The training data will be used to generate a dictionary of
    /// `max_dict_bytes`.
    ///
    /// Default: 0.
    pub fn set_bottommost_zstd_max_train_bytes(&mut self, value: c_int, enabled: bool) {
        unsafe {
            ffi::rocksdb_options_set_bottommost_compression_options_zstd_max_train_bytes(
                self.inner,
                value,
                c_uchar::from(enabled),
            );
        }
    }

    /// If non-zero, we perform bigger reads when doing compaction. If you're
    /// running RocksDB on spinning disks, you should set this to at least 2MB.
    /// That way RocksDB's compaction is doing sequential instead of random reads.
    ///
    /// Default: 2 * 1024 * 1024 (2 MB)
    pub fn set_compaction_readahead_size(&mut self, compaction_readahead_size: usize) {
        unsafe {
            ffi::rocksdb_options_compaction_readahead_size(self.inner, compaction_readahead_size);
        }
    }

    /// Allow RocksDB to pick dynamic base of bytes for levels.
    /// With this feature turned on, RocksDB will automatically adjust max bytes for each level.
    /// The goal of this feature is to have lower bound on size amplification.
    ///
    /// Default: true.
    pub fn set_level_compaction_dynamic_level_bytes(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_options_set_level_compaction_dynamic_level_bytes(
                self.inner,
                c_uchar::from(v),
            );
        }
    }

    /// This option has different meanings for different compaction styles:
    ///
    /// Leveled: files older than `periodic_compaction_seconds` will be picked up
    /// for compaction and will be re-written to the same level as they were
    /// before if level_compaction_dynamic_level_bytes is disabled. Otherwise,
    /// it will rewrite files to the next level except for the last level files
    /// to the same level.
    ///
    /// FIFO: not supported. Setting this option has no effect for FIFO compaction.
    ///
    /// Universal: when there are files older than `periodic_compaction_seconds`,
    /// rocksdb will try to do as large a compaction as possible including the
    /// last level. Such compaction is only skipped if only last level is to
    /// be compacted and no file in last level is older than
    /// `periodic_compaction_seconds`. See more in
    /// UniversalCompactionBuilder::PickPeriodicCompaction().
    /// For backward compatibility, the effective value of this option takes
    /// into account the value of option `ttl`. The logic is as follows:
    ///
    /// - both options are set to 30 days if they have the default value.
    /// - if both options are zero, zero is picked. Otherwise, we take the min
    ///   value among non-zero options values (i.e. takes the stricter limit).
    ///
    /// One main use of the feature is to make sure a file goes through compaction
    /// filters periodically. Users can also use the feature to clear up SST
    /// files using old format.
    ///
    /// A file's age is computed by looking at file_creation_time or creation_time
    /// table properties in order, if they have valid non-zero values; if not, the
    /// age is based on the file's last modified time (given by the underlying
    /// Env).
    ///
    /// This option only supports block based table format for any compaction
    /// style.
    ///
    /// unit: seconds. Ex: 7 days = 7 * 24 * 60 * 60
    ///
    /// Values:
    /// 0: Turn off Periodic compactions.
    /// UINT64_MAX - 1 (0xfffffffffffffffe) is special flag to allow RocksDB to
    /// pick default.
    ///
    /// Default: 30 days if using block based table format + compaction filter +
    /// leveled compaction or block based table format + universal compaction.
    /// 0 (disabled) otherwise.
    ///
    pub fn set_periodic_compaction_seconds(&mut self, secs: u64) {
        unsafe {
            ffi::rocksdb_options_set_periodic_compaction_seconds(self.inner, secs);
        }
    }

    /// When an iterator scans this number of invisible entries (tombstones or
    /// hidden puts) from the active memtable during a single iterator operation,
    /// we will attempt to flush the memtable. Currently only forward scans are
    /// supported (SeekToFirst(), Seek() and Next()).
    /// This option helps to reduce the overhead of scanning through a
    /// large number of entries in memtable.
    /// Users should consider enable deletion-triggered-compaction (see
    /// CompactOnDeletionCollectorFactory) together with this option to compact
    /// away tombstones after the memtable is flushed.
    ///
    /// Default: 0 (disabled)
    /// Dynamically changeable through the SetOptions() API.
    pub fn set_memtable_op_scan_flush_trigger(&mut self, num: u32) {
        unsafe {
            ffi::rocksdb_options_set_memtable_op_scan_flush_trigger(self.inner, num);
        }
    }

    /// Similar to `memtable_op_scan_flush_trigger`, but this option applies to
    /// Next() calls between Seeks or until iterator destruction. If the average
    /// of the number of invisible entries scanned from the active memtable, the
    /// memtable will be marked for flush.
    /// Note that to avoid the case where the window between Seeks is too small,
    /// the option only takes effect if the total number of hidden entries scanned
    /// within a window is at least `memtable_op_scan_flush_trigger`. So this
    /// option is only effective when `memtable_op_scan_flush_trigger` is set.
    ///
    /// This option should be set to a lower value than
    /// `memtable_op_scan_flush_trigger`. It covers the case where an iterator
    /// scans through an expensive key range with many invisible entries from the
    /// active memtable, but the number of invisible entries per operation does not
    /// exceed `memtable_op_scan_flush_trigger`.
    ///
    /// Default: 0 (disabled)
    /// Dynamically changeable through the SetOptions() API.
    pub fn set_memtable_avg_op_scan_flush_trigger(&mut self, num: u32) {
        unsafe {
            ffi::rocksdb_options_set_memtable_avg_op_scan_flush_trigger(self.inner, num);
        }
    }

    /// This option has different meanings for different compaction styles:
    ///
    /// Leveled: Non-bottom-level files with all keys older than TTL will go
    ///    through the compaction process. This usually happens in a cascading
    ///    way so that those entries will be compacted to bottommost level/file.
    ///    The feature is used to remove stale entries that have been deleted or
    ///    updated from the file system.
    ///
    /// FIFO: Files with all keys older than TTL will be deleted. TTL is only
    ///    supported if option max_open_files is set to -1.
    ///
    /// Universal: users should only set the option `periodic_compaction_seconds`
    ///    instead. For backward compatibility, this option has the same
    ///    meaning as `periodic_compaction_seconds`. See more in comments for
    ///    `periodic_compaction_seconds` on the interaction between these two
    ///    options.
    ///
    /// This option only supports block based table format for any compaction
    /// style.
    ///
    /// unit: seconds. Ex: 1 day = 1 * 24 * 60 * 60
    /// 0 means disabling.
    /// UINT64_MAX - 1 (0xfffffffffffffffe) is special flag to allow RocksDB to
    /// pick default.
    ///
    /// Default: 30 days if using block based table. 0 (disable) otherwise.
    ///
    /// Dynamically changeable
    /// Note that dynamically changing this option only works for leveled and FIFO
    /// compaction. For universal compaction, dynamically changing this option has
    /// no effect, users should dynamically change `periodic_compaction_seconds`
    /// instead.
    pub fn set_ttl(&mut self, secs: u64) {
        unsafe {
            ffi::rocksdb_options_set_ttl(self.inner, secs);
        }
    }

    pub fn set_merge_operator_associative<F: MergeFn + Clone>(
        &mut self,
        name: impl CStrLike,
        full_merge_fn: F,
    ) {
        let cb = Box::new(MergeOperatorCallback {
            name: name.into_c_string().unwrap(),
            full_merge_fn: full_merge_fn.clone(),
            partial_merge_fn: full_merge_fn,
        });

        unsafe {
            let mo = ffi::rocksdb_mergeoperator_create(
                Box::into_raw(cb).cast::<c_void>(),
                Some(merge_operator::destructor_callback::<F, F>),
                Some(full_merge_callback::<F, F>),
                Some(partial_merge_callback::<F, F>),
                Some(merge_operator::delete_callback),
                Some(merge_operator::name_callback::<F, F>),
            );
            ffi::rocksdb_options_set_merge_operator(self.inner, mo);
        }
    }

    pub fn set_merge_operator<F: MergeFn, PF: MergeFn>(
        &mut self,
        name: impl CStrLike,
        full_merge_fn: F,
        partial_merge_fn: PF,
    ) {
        let cb = Box::new(MergeOperatorCallback {
            name: name.into_c_string().unwrap(),
            full_merge_fn,
            partial_merge_fn,
        });

        unsafe {
            let mo = ffi::rocksdb_mergeoperator_create(
                Box::into_raw(cb).cast::<c_void>(),
                Some(merge_operator::destructor_callback::<F, PF>),
                Some(full_merge_callback::<F, PF>),
                Some(partial_merge_callback::<F, PF>),
                Some(merge_operator::delete_callback),
                Some(merge_operator::name_callback::<F, PF>),
            );
            ffi::rocksdb_options_set_merge_operator(self.inner, mo);
        }
    }

    #[deprecated(
        since = "0.5.0",
        note = "add_merge_operator has been renamed to set_merge_operator"
    )]
    pub fn add_merge_operator<F: MergeFn + Clone>(&mut self, name: &str, merge_fn: F) {
        self.set_merge_operator_associative(name, merge_fn);
    }

    /// Sets a compaction filter used to determine if entries should be kept, changed,
    /// or removed during compaction.
    ///
    /// An example use case is to remove entries with an expired TTL.
    ///
    /// If you take a snapshot of the database, only values written since the last
    /// snapshot will be passed through the compaction filter.
    ///
    /// If multi-threaded compaction is used, `filter_fn` may be called multiple times
    /// simultaneously.
    pub fn set_compaction_filter<F>(&mut self, name: impl CStrLike, filter_fn: F)
    where
        F: CompactionFilterFn + Send + 'static,
    {
        let cb = Box::new(CompactionFilterCallback {
            name: name.into_c_string().unwrap(),
            filter_fn,
        });

        let filter = unsafe {
            let cf = ffi::rocksdb_compactionfilter_create(
                Box::into_raw(cb).cast::<c_void>(),
                Some(compaction_filter::destructor_callback::<CompactionFilterCallback<F>>),
                Some(compaction_filter::filter_callback::<CompactionFilterCallback<F>>),
                Some(compaction_filter::name_callback::<CompactionFilterCallback<F>>),
            );
            ffi::rocksdb_options_set_compaction_filter(self.inner, cf);

            OwnedCompactionFilter::new(NonNull::new(cf).unwrap())
        };
        self.outlive.compaction_filter = Some(Arc::new(filter));
    }

    pub fn add_event_listener<L: EventListener>(&mut self, l: L) {
        let handle = new_event_listener(l);
        unsafe { ffi::rust_rocksdb_options_add_eventlistener(self.inner, handle.inner) }
    }

    /// This is a factory that provides compaction filter objects which allow
    /// an application to modify/delete a key-value during background compaction.
    ///
    /// A new filter will be created on each compaction run.  If multithreaded
    /// compaction is being used, each created CompactionFilter will only be used
    /// from a single thread and so does not need to be thread-safe.
    ///
    /// Default: nullptr
    pub fn set_compaction_filter_factory<F>(&mut self, factory: F)
    where
        F: CompactionFilterFactory + 'static,
    {
        let factory = Box::new(factory);

        unsafe {
            let cff = ffi::rocksdb_compactionfilterfactory_create(
                Box::into_raw(factory).cast::<c_void>(),
                Some(compaction_filter_factory::destructor_callback::<F>),
                Some(compaction_filter_factory::create_compaction_filter_callback::<F>),
                Some(compaction_filter_factory::name_callback::<F>),
            );

            ffi::rocksdb_options_set_compaction_filter_factory(self.inner, cff);
        }
    }

    /// Sets the comparator used to define the order of keys in the table.
    /// Default: a comparator that uses lexicographic byte-wise ordering
    ///
    /// The client must ensure that the comparator supplied here has the same
    /// name and orders keys *exactly* the same as the comparator provided to
    /// previous open calls on the same DB.
    pub fn set_comparator(&mut self, name: impl CStrLike, compare_fn: Box<CompareFn>) {
        let cb = Box::new(ComparatorCallback {
            name: name.into_c_string().unwrap(),
            compare_fn,
        });

        let cmp = unsafe {
            let cmp = ffi::rocksdb_comparator_create(
                Box::into_raw(cb).cast::<c_void>(),
                Some(ComparatorCallback::destructor_callback),
                Some(ComparatorCallback::compare_callback),
                Some(ComparatorCallback::name_callback),
            );
            ffi::rocksdb_options_set_comparator(self.inner, cmp);
            OwnedComparator::new(NonNull::new(cmp).unwrap())
        };
        self.outlive.comparator = Some(Arc::new(cmp));
    }

    /// Sets the comparator that are timestamp-aware, used to define the order of keys in the table,
    /// taking timestamp into consideration.
    /// Find more information on timestamp-aware comparator on [here](https://github.com/facebook/rocksdb/wiki/User-defined-Timestamp)
    ///
    /// The client must ensure that the comparator supplied here has the same
    /// name and orders keys *exactly* the same as the comparator provided to
    /// previous open calls on the same DB.
    pub fn set_comparator_with_ts(
        &mut self,
        name: impl CStrLike,
        timestamp_size: usize,
        compare_fn: Box<CompareFn>,
        compare_ts_fn: Box<CompareTsFn>,
        compare_without_ts_fn: Box<CompareWithoutTsFn>,
    ) {
        let cb = Box::new(ComparatorWithTsCallback {
            name: name.into_c_string().unwrap(),
            compare_fn,
            compare_ts_fn,
            compare_without_ts_fn,
        });

        let cmp = unsafe {
            let cmp = ffi::rocksdb_comparator_with_ts_create(
                Box::into_raw(cb).cast::<c_void>(),
                Some(ComparatorWithTsCallback::destructor_callback),
                Some(ComparatorWithTsCallback::compare_callback),
                Some(ComparatorWithTsCallback::compare_ts_callback),
                Some(ComparatorWithTsCallback::compare_without_ts_callback),
                Some(ComparatorWithTsCallback::name_callback),
                timestamp_size,
            );
            ffi::rocksdb_options_set_comparator(self.inner, cmp);
            OwnedComparator::new(NonNull::new(cmp).unwrap())
        };
        self.outlive.comparator = Some(Arc::new(cmp));
    }

    pub fn set_prefix_extractor(&mut self, prefix_extractor: SliceTransform) {
        unsafe {
            ffi::rocksdb_options_set_prefix_extractor(self.inner, prefix_extractor.inner);
        }
    }

    // Use this if you don't need to keep the data sorted, i.e. you'll never use
    // an iterator, only Put() and Get() API calls
    //
    pub fn optimize_for_point_lookup(&mut self, block_cache_size_mb: u64) {
        unsafe {
            ffi::rocksdb_options_optimize_for_point_lookup(self.inner, block_cache_size_mb);
        }
    }

    /// Sets the optimize_filters_for_hits flag
    ///
    /// Default: `false`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_optimize_filters_for_hits(true);
    /// ```
    pub fn set_optimize_filters_for_hits(&mut self, optimize_for_hits: bool) {
        unsafe {
            ffi::rocksdb_options_set_optimize_filters_for_hits(
                self.inner,
                c_int::from(optimize_for_hits),
            );
        }
    }

    /// Sets the periodicity when obsolete files get deleted.
    ///
    /// The files that get out of scope by compaction
    /// process will still get automatically delete on every compaction,
    /// regardless of this setting.
    ///
    /// Default: 6 hours
    pub fn set_delete_obsolete_files_period_micros(&mut self, micros: u64) {
        unsafe {
            ffi::rocksdb_options_set_delete_obsolete_files_period_micros(self.inner, micros);
        }
    }

    /// Prepare the DB for bulk loading.
    ///
    /// All data will be in level 0 without any automatic compaction.
    /// It's recommended to manually call CompactRange(NULL, NULL) before reading
    /// from the database, because otherwise the read can be very slow.
    pub fn prepare_for_bulk_load(&mut self) {
        unsafe {
            ffi::rocksdb_options_prepare_for_bulk_load(self.inner);
        }
    }

    /// Sets the number of open files that can be used by the DB. You may need to
    /// increase this if your database has a large working set. Value `-1` means
    /// files opened are always kept open. You can estimate number of files based
    /// on target_file_size_base and target_file_size_multiplier for level-based
    /// compaction. For universal-style compaction, you can usually set it to `-1`.
    ///
    /// Default: `-1`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_max_open_files(10);
    /// ```
    pub fn set_max_open_files(&mut self, nfiles: c_int) {
        unsafe {
            ffi::rocksdb_options_set_max_open_files(self.inner, nfiles);
        }
    }

    /// If max_open_files is -1, DB will open all files on DB::Open(). You can
    /// use this option to increase the number of threads used to open the files.
    /// Default: 16
    pub fn set_max_file_opening_threads(&mut self, nthreads: c_int) {
        unsafe {
            ffi::rocksdb_options_set_max_file_opening_threads(self.inner, nthreads);
        }
    }

    /// By default, writes to stable storage use fdatasync (on platforms
    /// where this function is available). If this option is true,
    /// fsync is used instead.
    ///
    /// fsync and fdatasync are equally safe for our purposes and fdatasync is
    /// faster, so it is rarely necessary to set this option. It is provided
    /// as a workaround for kernel/filesystem bugs, such as one that affected
    /// fdatasync with ext4 in kernel versions prior to 3.7.
    ///
    /// Default: `false`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_use_fsync(true);
    /// ```
    pub fn set_use_fsync(&mut self, useit: bool) {
        unsafe {
            ffi::rocksdb_options_set_use_fsync(self.inner, c_int::from(useit));
        }
    }

    /// Returns the value of the `use_fsync` option.
    pub fn get_use_fsync(&self) -> bool {
        let val = unsafe { ffi::rocksdb_options_get_use_fsync(self.inner) };
        val != 0
    }

    /// Specifies the absolute info LOG dir.
    ///
    /// If it is empty, the log files will be in the same dir as data.
    /// If it is non empty, the log files will be in the specified dir,
    /// and the db data dir's absolute path will be used as the log file
    /// name's prefix.
    ///
    /// Default: empty
    pub fn set_db_log_dir<P: AsRef<Path>>(&mut self, path: P) {
        let p = to_cpath(path).unwrap();
        unsafe {
            ffi::rocksdb_options_set_db_log_dir(self.inner, p.as_ptr());
        }
    }

    /// Specifies the log level.
    /// Consider the `LogLevel` enum for a list of possible levels.
    ///
    /// Default: Info
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{Options, LogLevel};
    ///
    /// let mut opts = Options::default();
    /// opts.set_log_level(LogLevel::Warn);
    /// ```
    pub fn set_log_level(&mut self, level: LogLevel) {
        unsafe {
            ffi::rocksdb_options_set_info_log_level(self.inner, level as c_int);
        }
    }

    /// Allows OS to incrementally sync files to disk while they are being
    /// written, asynchronously, in the background. This operation can be used
    /// to smooth out write I/Os over time. Users shouldn't rely on it for
    /// persistency guarantee.
    /// Issue one request for every bytes_per_sync written. `0` turns it off.
    ///
    /// Default: `0`
    ///
    /// You may consider using rate_limiter to regulate write rate to device.
    /// When rate limiter is enabled, it automatically enables bytes_per_sync
    /// to 1MB.
    ///
    /// This option applies to table files
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_bytes_per_sync(1024 * 1024);
    /// ```
    pub fn set_bytes_per_sync(&mut self, nbytes: u64) {
        unsafe {
            ffi::rocksdb_options_set_bytes_per_sync(self.inner, nbytes);
        }
    }

    /// Same as bytes_per_sync, but applies to WAL files.
    ///
    /// Default: 0, turned off
    ///
    /// Dynamically changeable through SetDBOptions() API.
    pub fn set_wal_bytes_per_sync(&mut self, nbytes: u64) {
        unsafe {
            ffi::rocksdb_options_set_wal_bytes_per_sync(self.inner, nbytes);
        }
    }

    /// Sets the maximum buffer size that is used by WritableFileWriter.
    ///
    /// On Windows, we need to maintain an aligned buffer for writes.
    /// We allow the buffer to grow until it's size hits the limit in buffered
    /// IO and fix the buffer size when using direct IO to ensure alignment of
    /// write requests if the logical sector size is unusual
    ///
    /// Default: 1024 * 1024 (1 MB)
    ///
    /// Dynamically changeable through SetDBOptions() API.
    pub fn set_writable_file_max_buffer_size(&mut self, nbytes: u64) {
        unsafe {
            ffi::rocksdb_options_set_writable_file_max_buffer_size(self.inner, nbytes);
        }
    }

    /// If true, allow multi-writers to update mem tables in parallel.
    /// Only some memtable_factory-s support concurrent writes; currently it
    /// is implemented only for SkipListFactory.  Concurrent memtable writes
    /// are not compatible with inplace_update_support or filter_deletes.
    /// It is strongly recommended to set enable_write_thread_adaptive_yield
    /// if you are going to use this feature.
    ///
    /// Default: true
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_allow_concurrent_memtable_write(false);
    /// ```
    pub fn set_allow_concurrent_memtable_write(&mut self, allow: bool) {
        unsafe {
            ffi::rocksdb_options_set_allow_concurrent_memtable_write(
                self.inner,
                c_uchar::from(allow),
            );
        }
    }

    /// If true, threads synchronizing with the write batch group leader will wait for up to
    /// write_thread_max_yield_usec before blocking on a mutex. This can substantially improve
    /// throughput for concurrent workloads, regardless of whether allow_concurrent_memtable_write
    /// is enabled.
    ///
    /// Default: true
    pub fn set_enable_write_thread_adaptive_yield(&mut self, enabled: bool) {
        unsafe {
            ffi::rocksdb_options_set_enable_write_thread_adaptive_yield(
                self.inner,
                c_uchar::from(enabled),
            );
        }
    }

    /// Specifies whether an iteration->Next() sequentially skips over keys with the same user-key or not.
    ///
    /// This number specifies the number of keys (with the same userkey)
    /// that will be sequentially skipped before a reseek is issued.
    ///
    /// Default: 8
    pub fn set_max_sequential_skip_in_iterations(&mut self, num: u64) {
        unsafe {
            ffi::rocksdb_options_set_max_sequential_skip_in_iterations(self.inner, num);
        }
    }

    /// Enable direct I/O mode for reading
    /// they may or may not improve performance depending on the use case
    ///
    /// Files will be opened in "direct I/O" mode
    /// which means that data read from the disk will not be cached or
    /// buffered. The hardware buffer of the devices may however still
    /// be used. Memory mapped files are not impacted by these parameters.
    ///
    /// Default: false
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_use_direct_reads(true);
    /// ```
    pub fn set_use_direct_reads(&mut self, enabled: bool) {
        unsafe {
            ffi::rocksdb_options_set_use_direct_reads(self.inner, c_uchar::from(enabled));
        }
    }

    /// Enable direct I/O mode for flush and compaction
    ///
    /// Files will be opened in "direct I/O" mode
    /// which means that data written to the disk will not be cached or
    /// buffered. The hardware buffer of the devices may however still
    /// be used. Memory mapped files are not impacted by these parameters.
    /// they may or may not improve performance depending on the use case
    ///
    /// Default: false
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_use_direct_io_for_flush_and_compaction(true);
    /// ```
    pub fn set_use_direct_io_for_flush_and_compaction(&mut self, enabled: bool) {
        unsafe {
            ffi::rocksdb_options_set_use_direct_io_for_flush_and_compaction(
                self.inner,
                c_uchar::from(enabled),
            );
        }
    }

    /// Enable/disable child process inherit open files.
    ///
    /// Default: true
    pub fn set_is_fd_close_on_exec(&mut self, enabled: bool) {
        unsafe {
            ffi::rocksdb_options_set_is_fd_close_on_exec(self.inner, c_uchar::from(enabled));
        }
    }

    /// Hints to the OS that it should not buffer disk I/O. Enabling this
    /// parameter may improve performance but increases pressure on the
    /// system cache.
    ///
    /// The exact behavior of this parameter is platform dependent.
    ///
    /// On POSIX systems, after RocksDB reads data from disk it will
    /// mark the pages as "unneeded". The operating system may or may not
    /// evict these pages from memory, reducing pressure on the system
    /// cache. If the disk block is requested again this can result in
    /// additional disk I/O.
    ///
    /// On WINDOWS systems, files will be opened in "unbuffered I/O" mode
    /// which means that data read from the disk will not be cached or
    /// bufferized. The hardware buffer of the devices may however still
    /// be used. Memory mapped files are not impacted by this parameter.
    ///
    /// Default: true
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// #[allow(deprecated)]
    /// opts.set_allow_os_buffer(false);
    /// ```
    #[deprecated(
        since = "0.7.0",
        note = "replaced with set_use_direct_reads/set_use_direct_io_for_flush_and_compaction methods"
    )]
    pub fn set_allow_os_buffer(&mut self, is_allow: bool) {
        self.set_use_direct_reads(!is_allow);
        self.set_use_direct_io_for_flush_and_compaction(!is_allow);
    }

    /// Sets the number of shards used for table cache.
    ///
    /// Default: `6`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_table_cache_num_shard_bits(4);
    /// ```
    pub fn set_table_cache_num_shard_bits(&mut self, nbits: c_int) {
        unsafe {
            ffi::rocksdb_options_set_table_cache_numshardbits(self.inner, nbits);
        }
    }

    /// By default target_file_size_multiplier is 1, which means
    /// by default files in different levels will have similar size.
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_target_file_size_multiplier(&mut self, multiplier: i32) {
        unsafe {
            ffi::rocksdb_options_set_target_file_size_multiplier(self.inner, multiplier as c_int);
        }
    }

    /// Sets the minimum number of write buffers that will be merged
    /// before writing to storage.  If set to `1`, then
    /// all write buffers are flushed to L0 as individual files and this increases
    /// read amplification because a get request has to check in all of these
    /// files. Also, an in-memory merge may result in writing lesser
    /// data to storage if there are duplicate records in each of these
    /// individual write buffers.
    ///
    /// Default: `1`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_min_write_buffer_number(2);
    /// ```
    pub fn set_min_write_buffer_number(&mut self, nbuf: c_int) {
        unsafe {
            ffi::rocksdb_options_set_min_write_buffer_number_to_merge(self.inner, nbuf);
        }
    }

    /// Sets the maximum number of write buffers that are built up in memory.
    /// The default and the minimum number is 2, so that when 1 write buffer
    /// is being flushed to storage, new writes can continue to the other
    /// write buffer.
    /// If max_write_buffer_number > 3, writing will be slowed down to
    /// options.delayed_write_rate if we are writing to the last write buffer
    /// allowed.
    ///
    /// Default: `2`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_max_write_buffer_number(4);
    /// ```
    pub fn set_max_write_buffer_number(&mut self, nbuf: c_int) {
        unsafe {
            ffi::rocksdb_options_set_max_write_buffer_number(self.inner, nbuf);
        }
    }

    /// Sets the amount of data to build up in memory (backed by an unsorted log
    /// on disk) before converting to a sorted on-disk file.
    ///
    /// Larger values increase performance, especially during bulk loads.
    /// Up to max_write_buffer_number write buffers may be held in memory
    /// at the same time,
    /// so you may wish to adjust this parameter to control memory usage.
    /// Also, a larger write buffer will result in a longer recovery time
    /// the next time the database is opened.
    ///
    /// Note that write_buffer_size is enforced per column family.
    /// See db_write_buffer_size for sharing memory across column families.
    ///
    /// Default: `0x4000000` (64MiB)
    ///
    /// Dynamically changeable through SetOptions() API
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_write_buffer_size(128 * 1024 * 1024);
    /// ```
    pub fn set_write_buffer_size(&mut self, size: usize) {
        unsafe {
            ffi::rocksdb_options_set_write_buffer_size(self.inner, size);
        }
    }

    /// Amount of data to build up in memtables across all column
    /// families before writing to disk.
    ///
    /// This is distinct from write_buffer_size, which enforces a limit
    /// for a single memtable.
    ///
    /// This feature is disabled by default. Specify a non-zero value
    /// to enable it.
    ///
    /// Default: 0 (disabled)
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_db_write_buffer_size(128 * 1024 * 1024);
    /// ```
    pub fn set_db_write_buffer_size(&mut self, size: usize) {
        unsafe {
            ffi::rocksdb_options_set_db_write_buffer_size(self.inner, size);
        }
    }

    /// Control maximum total data size for a level.
    /// max_bytes_for_level_base is the max total for level-1.
    /// Maximum number of bytes for level L can be calculated as
    /// (max_bytes_for_level_base) * (max_bytes_for_level_multiplier ^ (L-1))
    /// For example, if max_bytes_for_level_base is 200MB, and if
    /// max_bytes_for_level_multiplier is 10, total data size for level-1
    /// will be 200MB, total file size for level-2 will be 2GB,
    /// and total file size for level-3 will be 20GB.
    ///
    /// Default: `0x10000000` (256MiB).
    ///
    /// Dynamically changeable through SetOptions() API
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_max_bytes_for_level_base(512 * 1024 * 1024);
    /// ```
    pub fn set_max_bytes_for_level_base(&mut self, size: u64) {
        unsafe {
            ffi::rocksdb_options_set_max_bytes_for_level_base(self.inner, size);
        }
    }

    /// Default: `10`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_max_bytes_for_level_multiplier(4.0);
    /// ```
    pub fn set_max_bytes_for_level_multiplier(&mut self, mul: f64) {
        unsafe {
            ffi::rocksdb_options_set_max_bytes_for_level_multiplier(self.inner, mul);
        }
    }

    /// Sets a lower bound on the auto-tuned MANIFEST size limit. The MANIFEST
    /// is rolled over on reaching the limit and the older one is deleted.
    ///
    /// This used to be a hard limit. RocksDB now auto-tunes the real limit and
    /// treats this as a minimum, so setting it small does not keep the MANIFEST
    /// small. Batches written in the foreground get a 25% higher limit.
    ///
    /// Default: 1 GiB.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_max_manifest_file_size(20 * 1024 * 1024);
    /// ```
    pub fn set_max_manifest_file_size(&mut self, size: usize) {
        unsafe {
            ffi::rocksdb_options_set_max_manifest_file_size(self.inner, size);
        }
    }

    /// Sets the target file size for compaction.
    /// target_file_size_base is per-file size for level-1.
    /// Target file size for level L can be calculated by
    /// target_file_size_base * (target_file_size_multiplier ^ (L-1))
    /// For example, if target_file_size_base is 2MB and
    /// target_file_size_multiplier is 10, then each file on level-1 will
    /// be 2MB, and each file on level 2 will be 20MB,
    /// and each file on level-3 will be 200MB.
    ///
    /// Default: `0x4000000` (64MiB)
    ///
    /// Dynamically changeable through SetOptions() API
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_target_file_size_base(128 * 1024 * 1024);
    /// ```
    pub fn set_target_file_size_base(&mut self, size: u64) {
        unsafe {
            ffi::rocksdb_options_set_target_file_size_base(self.inner, size);
        }
    }

    /// Sets the minimum number of write buffers that will be merged together
    /// before writing to storage.  If set to `1`, then
    /// all write buffers are flushed to L0 as individual files and this increases
    /// read amplification because a get request has to check in all of these
    /// files. Also, an in-memory merge may result in writing lesser
    /// data to storage if there are duplicate records in each of these
    /// individual write buffers.
    ///
    /// Default: `1`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_min_write_buffer_number_to_merge(2);
    /// ```
    pub fn set_min_write_buffer_number_to_merge(&mut self, to_merge: c_int) {
        unsafe {
            ffi::rocksdb_options_set_min_write_buffer_number_to_merge(self.inner, to_merge);
        }
    }

    /// Sets the number of files to trigger level-0 compaction. A value < `0` means that
    /// level-0 compaction will not be triggered by number of files at all.
    ///
    /// Default: `4`
    ///
    /// Dynamically changeable through SetOptions() API
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_level_zero_file_num_compaction_trigger(8);
    /// ```
    pub fn set_level_zero_file_num_compaction_trigger(&mut self, n: c_int) {
        unsafe {
            ffi::rocksdb_options_set_level0_file_num_compaction_trigger(self.inner, n);
        }
    }

    /// Sets the soft limit on number of level-0 files. We start slowing down writes at this
    /// point. A value < `0` means that no writing slowdown will be triggered by
    /// number of files in level-0.
    ///
    /// Default: `20`
    ///
    /// Dynamically changeable through SetOptions() API
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_level_zero_slowdown_writes_trigger(10);
    /// ```
    pub fn set_level_zero_slowdown_writes_trigger(&mut self, n: c_int) {
        unsafe {
            ffi::rocksdb_options_set_level0_slowdown_writes_trigger(self.inner, n);
        }
    }

    /// Sets the maximum number of level-0 files.  We stop writes at this point.
    ///
    /// Default: `36`
    ///
    /// Dynamically changeable through SetOptions() API
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_level_zero_stop_writes_trigger(48);
    /// ```
    pub fn set_level_zero_stop_writes_trigger(&mut self, n: c_int) {
        unsafe {
            ffi::rocksdb_options_set_level0_stop_writes_trigger(self.inner, n);
        }
    }

    /// Sets the compaction style.
    ///
    /// Default: DBCompactionStyle::Level
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{Options, DBCompactionStyle};
    ///
    /// let mut opts = Options::default();
    /// opts.set_compaction_style(DBCompactionStyle::Universal);
    /// ```
    pub fn set_compaction_style(&mut self, style: DBCompactionStyle) {
        unsafe {
            ffi::rocksdb_options_set_compaction_style(self.inner, style as c_int);
        }
    }

    /// Sets the options needed to support Universal Style compactions.
    pub fn set_universal_compaction_options(&mut self, uco: &UniversalCompactOptions) {
        unsafe {
            ffi::rocksdb_options_set_universal_compaction_options(self.inner, uco.inner);
        }
    }

    /// Sets the options for FIFO compaction style.
    pub fn set_fifo_compaction_options(&mut self, fco: &FifoCompactOptions) {
        unsafe {
            ffi::rocksdb_options_set_fifo_compaction_options(self.inner, fco.inner);
        }
    }

    /// Sets unordered_write to true trades higher write throughput with
    /// relaxing the immutability guarantee of snapshots. This violates the
    /// repeatability one expects from ::Get from a snapshot, as well as
    /// ::MultiGet and Iterator's consistent-point-in-time view property.
    /// If the application cannot tolerate the relaxed guarantees, it can implement
    /// its own mechanisms to work around that and yet benefit from the higher
    /// throughput. Using TransactionDB with WRITE_PREPARED write policy and
    /// two_write_queues=true is one way to achieve immutable snapshots despite
    /// unordered_write.
    ///
    /// By default, i.e., when it is false, rocksdb does not advance the sequence
    /// number for new snapshots unless all the writes with lower sequence numbers
    /// are already finished. This provides the immutability that we expect from
    /// snapshots. Moreover, since Iterator and MultiGet internally depend on
    /// snapshots, the snapshot immutability results into Iterator and MultiGet
    /// offering consistent-point-in-time view. If set to true, although
    /// Read-Your-Own-Write property is still provided, the snapshot immutability
    /// property is relaxed: the writes issued after the snapshot is obtained (with
    /// larger sequence numbers) will be still not visible to the reads from that
    /// snapshot, however, there still might be pending writes (with lower sequence
    /// number) that will change the state visible to the snapshot after they are
    /// landed to the memtable.
    ///
    /// Default: false
    pub fn set_unordered_write(&mut self, unordered: bool) {
        unsafe {
            ffi::rocksdb_options_set_unordered_write(self.inner, c_uchar::from(unordered));
        }
    }

    /// Sets maximum number of threads that will
    /// concurrently perform a compaction job by breaking it into multiple,
    /// smaller ones that are run simultaneously.
    ///
    /// Default: 1 (i.e. no subcompactions)
    pub fn set_max_subcompactions(&mut self, num: u32) {
        unsafe {
            ffi::rocksdb_options_set_max_subcompactions(self.inner, num);
        }
    }

    /// Sets maximum number of concurrent background jobs
    /// (compactions and flushes).
    ///
    /// Default: 2
    ///
    /// Dynamically changeable through SetDBOptions() API.
    pub fn set_max_background_jobs(&mut self, jobs: c_int) {
        unsafe {
            ffi::rocksdb_options_set_max_background_jobs(self.inner, jobs);
        }
    }

    /// Sets the maximum number of concurrent background compaction jobs, submitted to
    /// the default LOW priority thread pool.
    /// We first try to schedule compactions based on
    /// `base_background_compactions`. If the compaction cannot catch up , we
    /// will increase number of compaction threads up to
    /// `max_background_compactions`.
    ///
    /// If you're increasing this, also consider increasing number of threads in
    /// LOW priority thread pool. For more information, see
    /// Env::SetBackgroundThreads
    ///
    /// Default: `-1`, meaning RocksDB derives it from `max_background_jobs`.
    /// Setting either this or `max_background_flushes` opts into the old
    /// behaviour, where the unset one of the pair counts as `1`.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// #[allow(deprecated)]
    /// opts.set_max_background_compactions(2);
    /// ```
    #[deprecated(
        since = "0.15.0",
        note = "RocksDB automatically decides this based on the value of max_background_jobs"
    )]
    pub fn set_max_background_compactions(&mut self, n: c_int) {
        unsafe {
            ffi::rocksdb_options_set_max_background_compactions(self.inner, n);
        }
    }

    /// Sets the maximum number of concurrent background memtable flush jobs, submitted to
    /// the HIGH priority thread pool.
    ///
    /// By default, all background jobs (major compaction and memtable flush) go
    /// to the LOW priority pool. If this option is set to a positive number,
    /// memtable flush jobs will be submitted to the HIGH priority pool.
    /// It is important when the same Env is shared by multiple db instances.
    /// Without a separate pool, long running major compaction jobs could
    /// potentially block memtable flush jobs of other db instances, leading to
    /// unnecessary Put stalls.
    ///
    /// If you're increasing this, also consider increasing number of threads in
    /// HIGH priority thread pool. For more information, see
    /// Env::SetBackgroundThreads
    ///
    /// Default: `-1`, meaning RocksDB derives it from `max_background_jobs`.
    /// Setting either this or `max_background_compactions` opts into the old
    /// behaviour, where the unset one of the pair counts as `1`.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// #[allow(deprecated)]
    /// opts.set_max_background_flushes(2);
    /// ```
    #[deprecated(
        since = "0.15.0",
        note = "RocksDB automatically decides this based on the value of max_background_jobs"
    )]
    pub fn set_max_background_flushes(&mut self, n: c_int) {
        unsafe {
            ffi::rocksdb_options_set_max_background_flushes(self.inner, n);
        }
    }

    /// Disables automatic compactions. Manual compactions can still
    /// be issued on this column family
    ///
    /// Default: `false`
    ///
    /// Dynamically changeable through SetOptions() API
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_disable_auto_compactions(true);
    /// ```
    pub fn set_disable_auto_compactions(&mut self, disable: bool) {
        unsafe {
            ffi::rocksdb_options_set_disable_auto_compactions(self.inner, c_int::from(disable));
        }
    }

    /// SetMemtableHugePageSize sets the page size for huge page for
    /// arena used by the memtable.
    /// If <=0, it won't allocate from huge page but from malloc.
    /// Users are responsible to reserve huge pages for it to be allocated. For
    /// example:
    ///      sysctl -w vm.nr_hugepages=20
    /// See linux doc Documentation/vm/hugetlbpage.txt
    /// If there isn't enough free huge page available, it will fall back to
    /// malloc.
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_memtable_huge_page_size(&mut self, size: size_t) {
        unsafe {
            ffi::rocksdb_options_set_memtable_huge_page_size(self.inner, size);
        }
    }

    /// Enables the skip-list memtable's batch-lookup optimization for
    /// `MultiGet`.
    ///
    /// When enabled, the search path is cached between consecutive keys in a
    /// `MultiGet`, reducing per-key cost from `O(log N)` to `O(log d)` where
    /// `d` is the distance between consecutive keys. The optimization
    /// exploits the fact that `MultiGet` keys are sorted.
    ///
    /// Applies only to the default skip-list memtable (the one used when no
    /// memtable factory is set via [`Self::set_memtable_factory`]). The
    /// `MemtableFactory::Vector`, `HashSkipList`, and `HashLinkList` variants
    /// all fall back to per-key lookups regardless of this flag.
    ///
    /// This option is immutable on the C++ side: it must be set before the
    /// column family is opened and cannot be changed via `SetOptions`.
    ///
    /// Default: `false`
    pub fn set_memtable_batch_lookup_optimization(&mut self, enable: bool) {
        unsafe {
            ffi::rocksdb_options_set_memtable_batch_lookup_optimization(
                self.inner,
                c_uchar::from(enable),
            );
        }
    }

    /// Returns the current value of
    /// [`Self::set_memtable_batch_lookup_optimization`].
    ///
    /// Provided primarily for tests that want to confirm the setter is wired
    /// through to the underlying C++ `AdvancedColumnFamilyOptions`.
    pub fn get_memtable_batch_lookup_optimization(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_memtable_batch_lookup_optimization(self.inner) != 0 }
    }

    /// Sets the maximum number of successive merge operations on a key in the memtable.
    ///
    /// When a merge operation is added to the memtable and the maximum number of
    /// successive merges is reached, the value of the key will be calculated and
    /// inserted into the memtable instead of the merge operation. This will
    /// ensure that there are never more than max_successive_merges merge
    /// operations in the memtable.
    ///
    /// Default: 0 (disabled)
    pub fn set_max_successive_merges(&mut self, num: usize) {
        unsafe {
            ffi::rocksdb_options_set_max_successive_merges(self.inner, num);
        }
    }

    /// Control locality of bloom filter probes to improve cache miss rate.
    /// This option only applies to memtable prefix bloom and plaintable
    /// prefix bloom. It essentially limits the max number of cache lines each
    /// bloom filter check can touch.
    ///
    /// This optimization is turned off when set to 0. The number should never
    /// be greater than number of probes. This option can boost performance
    /// for in-memory workload but should use with care since it can cause
    /// higher false positive rate.
    ///
    /// Default: 0
    pub fn set_bloom_locality(&mut self, v: u32) {
        unsafe {
            ffi::rocksdb_options_set_bloom_locality(self.inner, v);
        }
    }

    /// Enable/disable thread-safe inplace updates.
    ///
    /// Requires updates if
    /// * key exists in current memtable
    /// * new sizeof(new_value) <= sizeof(old_value)
    /// * old_value for that key is a put i.e. kTypeValue
    ///
    /// Default: false.
    pub fn set_inplace_update_support(&mut self, enabled: bool) {
        unsafe {
            ffi::rocksdb_options_set_inplace_update_support(self.inner, c_uchar::from(enabled));
        }
    }

    /// Sets the number of locks used for inplace update.
    ///
    /// Default: 10000 when inplace_update_support = true, otherwise 0.
    pub fn set_inplace_update_locks(&mut self, num: usize) {
        unsafe {
            ffi::rocksdb_options_set_inplace_update_num_locks(self.inner, num);
        }
    }

    /// Different max-size multipliers for different levels.
    /// These are multiplied by max_bytes_for_level_multiplier to arrive
    /// at the max-size of each level.
    ///
    /// Default: 1
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_max_bytes_for_level_multiplier_additional(&mut self, level_values: &[i32]) {
        let count = level_values.len();
        unsafe {
            ffi::rocksdb_options_set_max_bytes_for_level_multiplier_additional(
                self.inner,
                level_values.as_ptr().cast_mut(),
                count,
            );
        }
    }

    /// The total maximum size(bytes) of write buffers to maintain in memory
    /// including copies of buffers that have already been flushed. This parameter
    /// only affects trimming of flushed buffers and does not affect flushing.
    /// This controls the maximum amount of write history that will be available
    /// in memory for conflict checking when Transactions are used. The actual
    /// size of write history (flushed Memtables) might be higher than this limit
    /// if further trimming will reduce write history total size below this
    /// limit. For example, if max_write_buffer_size_to_maintain is set to 64MB,
    /// and there are three flushed Memtables, with sizes of 32MB, 20MB, 20MB.
    /// Because trimming the next Memtable of size 20MB will reduce total memory
    /// usage to 52MB which is below the limit, RocksDB will stop trimming.
    ///
    /// When using an OptimisticTransactionDB:
    /// If this value is too low, some transactions may fail at commit time due
    /// to not being able to determine whether there were any write conflicts.
    ///
    /// When using a TransactionDB:
    /// If Transaction::SetSnapshot is used, TransactionDB will read either
    /// in-memory write buffers or SST files to do write-conflict checking.
    /// Increasing this value can reduce the number of reads to SST files
    /// done for conflict detection.
    ///
    /// Setting this value to 0 will cause write buffers to be freed immediately
    /// after they are flushed. If this value is set to -1,
    /// 'max_write_buffer_number * write_buffer_size' will be used.
    ///
    /// Default:
    /// If using a TransactionDB/OptimisticTransactionDB, the default value will
    /// be set to the value of 'max_write_buffer_number * write_buffer_size'
    /// if it is not explicitly set by the user.  Otherwise, the default is 0.
    pub fn set_max_write_buffer_size_to_maintain(&mut self, size: i64) {
        unsafe {
            ffi::rocksdb_options_set_max_write_buffer_size_to_maintain(self.inner, size);
        }
    }

    /// By default, a single write thread queue is maintained. The thread gets
    /// to the head of the queue becomes write batch group leader and responsible
    /// for writing to WAL and memtable for the batch group.
    ///
    /// If enable_pipelined_write is true, separate write thread queue is
    /// maintained for WAL write and memtable write. A write thread first enter WAL
    /// writer queue and then memtable writer queue. Pending thread on the WAL
    /// writer queue thus only have to wait for previous writers to finish their
    /// WAL writing but not the memtable writing. Enabling the feature may improve
    /// write throughput and reduce latency of the prepare phase of two-phase
    /// commit.
    ///
    /// Default: false
    pub fn set_enable_pipelined_write(&mut self, value: bool) {
        unsafe {
            ffi::rocksdb_options_set_enable_pipelined_write(self.inner, c_uchar::from(value));
        }
    }

    /// Defines the underlying memtable implementation.
    /// See official [wiki](https://github.com/facebook/rocksdb/wiki/MemTable) for more information.
    /// Defaults to using a skiplist.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{Options, MemtableFactory};
    /// let mut opts = Options::default();
    /// let factory = MemtableFactory::HashSkipList {
    ///     bucket_count: 1_000_000,
    ///     height: 4,
    ///     branching_factor: 4,
    /// };
    ///
    /// opts.set_allow_concurrent_memtable_write(false);
    /// opts.set_memtable_factory(factory);
    /// ```
    pub fn set_memtable_factory(&mut self, factory: MemtableFactory) {
        match factory {
            MemtableFactory::Vector => unsafe {
                ffi::rocksdb_options_set_memtable_vector_rep(self.inner);
            },
            MemtableFactory::HashSkipList {
                bucket_count,
                height,
                branching_factor,
            } => unsafe {
                ffi::rocksdb_options_set_hash_skip_list_rep(
                    self.inner,
                    bucket_count,
                    height,
                    branching_factor,
                );
            },
            MemtableFactory::HashLinkList { bucket_count } => unsafe {
                ffi::rocksdb_options_set_hash_link_list_rep(self.inner, bucket_count);
            },
        }
    }

    pub fn set_block_based_table_factory(&mut self, factory: &BlockBasedOptions) {
        unsafe {
            ffi::rocksdb_options_set_block_based_table_factory(self.inner, factory.inner);
        }
        self.outlive.block_based = Some(factory.outlive.clone());
    }

    /// Sets the table factory to a CuckooTableFactory (the default table
    /// factory is a block-based table factory that provides a default
    /// implementation of TableBuilder and TableReader with default
    /// BlockBasedTableOptions).
    /// See official [wiki](https://github.com/facebook/rocksdb/wiki/CuckooTable-Format) for more information on this table format.
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{Options, CuckooTableOptions};
    ///
    /// let mut opts = Options::default();
    /// let mut factory_opts = CuckooTableOptions::default();
    /// factory_opts.set_hash_ratio(0.8);
    /// factory_opts.set_max_search_depth(20);
    /// factory_opts.set_cuckoo_block_size(10);
    /// factory_opts.set_identity_as_first_hash(true);
    /// factory_opts.set_use_module_hash(false);
    ///
    /// opts.set_cuckoo_table_factory(&factory_opts);
    /// ```
    pub fn set_cuckoo_table_factory(&mut self, factory: &CuckooTableOptions) {
        unsafe {
            ffi::rocksdb_options_set_cuckoo_table_factory(self.inner, factory.inner);
        }
    }

    // This is a factory that provides TableFactory objects.
    // Default: a block-based table factory that provides a default
    // implementation of TableBuilder and TableReader with default
    // BlockBasedTableOptions.
    /// Sets the factory as plain table.
    /// See official [wiki](https://github.com/facebook/rocksdb/wiki/PlainTable-Format) for more
    /// information.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{KeyEncodingType, Options, PlainTableFactoryOptions};
    ///
    /// let mut opts = Options::default();
    /// let factory_opts = PlainTableFactoryOptions {
    ///   user_key_length: 0,
    ///   bloom_bits_per_key: 20,
    ///   hash_table_ratio: 0.75,
    ///   index_sparseness: 16,
    ///   huge_page_tlb_size: 0,
    ///   encoding_type: KeyEncodingType::Plain,
    ///   full_scan_mode: false,
    ///   store_index_in_file: false,
    /// };
    ///
    /// opts.set_plain_table_factory(&factory_opts);
    /// ```
    pub fn set_plain_table_factory(&mut self, options: &PlainTableFactoryOptions) {
        unsafe {
            ffi::rocksdb_options_set_plain_table_factory(
                self.inner,
                options.user_key_length,
                options.bloom_bits_per_key,
                options.hash_table_ratio,
                options.index_sparseness,
                options.huge_page_tlb_size,
                options.encoding_type as c_char,
                c_uchar::from(options.full_scan_mode),
                c_uchar::from(options.store_index_in_file),
            );
        }
    }

    /// Sets the start level to use compression.
    pub fn set_min_level_to_compress(&mut self, lvl: c_int) {
        unsafe {
            ffi::rocksdb_options_set_min_level_to_compress(self.inner, lvl);
        }
    }

    /// Measure IO stats in compactions and flushes, if `true`.
    ///
    /// Default: `false`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_report_bg_io_stats(true);
    /// ```
    pub fn set_report_bg_io_stats(&mut self, enable: bool) {
        unsafe {
            ffi::rocksdb_options_set_report_bg_io_stats(self.inner, c_int::from(enable));
        }
    }

    /// Once write-ahead logs exceed this size, we will start forcing the flush of
    /// column families whose memtables are backed by the oldest live WAL file
    /// (i.e. the ones that are causing all the space amplification).
    ///
    /// Default: `0`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// // Set max total wal size to 1G.
    /// opts.set_max_total_wal_size(1 << 30);
    /// ```
    pub fn set_max_total_wal_size(&mut self, size: u64) {
        unsafe {
            ffi::rocksdb_options_set_max_total_wal_size(self.inner, size);
        }
    }

    /// Recovery mode to control the consistency while replaying WAL.
    ///
    /// Default: DBRecoveryMode::PointInTime
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{Options, DBRecoveryMode};
    ///
    /// let mut opts = Options::default();
    /// opts.set_wal_recovery_mode(DBRecoveryMode::AbsoluteConsistency);
    /// ```
    pub fn set_wal_recovery_mode(&mut self, mode: DBRecoveryMode) {
        unsafe {
            ffi::rocksdb_options_set_wal_recovery_mode(self.inner, mode as c_int);
        }
    }

    /// Enables recording RocksDB statistics.
    ///
    /// The statistics in this Options object are shared between all DB instances.
    /// See [`get_statistics`](Self::get_statistics), [`get_ticker_count`](Self::get_ticker_count),
    /// and [`get_histogram_data`](Self::get_histogram_data).
    pub fn enable_statistics(&mut self) {
        unsafe {
            ffi::rocksdb_options_enable_statistics(self.inner);
        }
    }

    /// Returns a string containing RocksDB statistics if enabled using
    /// [`enable_statistics`](Self::enable_statistics).
    pub fn get_statistics(&self) -> Option<String> {
        unsafe {
            let value = ffi::rocksdb_options_statistics_get_string(self.inner);
            if value.is_null() {
                return None;
            }

            // Must have valid UTF-8 format.
            Some(from_cstr_and_free(value))
        }
    }

    /// StatsLevel can be used to reduce statistics overhead by skipping certain
    /// types of stats in the stats collection process.
    ///
    /// Only takes effect if stats are enabled first using
    /// [`enable_statistics`](Self::enable_statistics).
    pub fn set_statistics_level(&self, level: StatsLevel) {
        unsafe { ffi::rocksdb_options_set_statistics_level(self.inner, level as c_int) }
    }

    /// Returns a counter if statistics are enabled using
    /// [`enable_statistics`](Self::enable_statistics).
    pub fn get_ticker_count(&self, ticker: Ticker) -> u64 {
        unsafe { ffi::rocksdb_options_statistics_get_ticker_count(self.inner, ticker as u32) }
    }

    /// Returns a histogram if statistics are enabled using
    /// [`enable_statistics`](Self::enable_statistics).
    pub fn get_histogram_data(&self, histogram: Histogram) -> HistogramData {
        unsafe {
            let data = HistogramData::default();
            ffi::rocksdb_options_statistics_get_histogram_data(
                self.inner,
                histogram as u32,
                data.inner,
            );
            data
        }
    }

    /// If not zero, dump `rocksdb.stats` to LOG every `stats_dump_period_sec`.
    ///
    /// Default: `600` (10 mins)
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_stats_dump_period_sec(300);
    /// ```
    pub fn set_stats_dump_period_sec(&mut self, period: c_uint) {
        unsafe {
            ffi::rocksdb_options_set_stats_dump_period_sec(self.inner, period);
        }
    }

    /// If not zero, dump rocksdb.stats to RocksDB to LOG every `stats_persist_period_sec`.
    ///
    /// Default: `600` (10 mins)
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_stats_persist_period_sec(5);
    /// ```
    pub fn set_stats_persist_period_sec(&mut self, period: c_uint) {
        unsafe {
            ffi::rocksdb_options_set_stats_persist_period_sec(self.inner, period);
        }
    }

    /// When set to true, reading SST files will opt out of the filesystem's
    /// readahead. Setting this to false may improve sequential iteration
    /// performance.
    ///
    /// Default: `true`
    pub fn set_advise_random_on_open(&mut self, advise: bool) {
        unsafe {
            ffi::rocksdb_options_set_advise_random_on_open(self.inner, c_uchar::from(advise));
        }
    }

    /// Enable/disable adaptive mutex, which spins in the user space before resorting to kernel.
    ///
    /// This could reduce context switch when the mutex is not
    /// heavily contended. However, if the mutex is hot, we could end up
    /// wasting spin time.
    ///
    /// Default: false
    pub fn set_use_adaptive_mutex(&mut self, enabled: bool) {
        unsafe {
            ffi::rocksdb_options_set_use_adaptive_mutex(self.inner, c_uchar::from(enabled));
        }
    }

    /// Sets the number of levels for this database.
    pub fn set_num_levels(&mut self, n: c_int) {
        unsafe {
            ffi::rocksdb_options_set_num_levels(self.inner, n);
        }
    }

    /// When a `prefix_extractor` is defined through `opts.set_prefix_extractor` this
    /// creates a prefix bloom filter for each memtable with the size of
    /// `write_buffer_size * memtable_prefix_bloom_ratio` (capped at 0.25).
    ///
    /// Default: `0`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{Options, SliceTransform};
    ///
    /// let mut opts = Options::default();
    /// let transform = SliceTransform::create_fixed_prefix(10);
    /// opts.set_prefix_extractor(transform);
    /// opts.set_memtable_prefix_bloom_ratio(0.2);
    /// ```
    pub fn set_memtable_prefix_bloom_ratio(&mut self, ratio: f64) {
        unsafe {
            ffi::rocksdb_options_set_memtable_prefix_bloom_size_ratio(self.inner, ratio);
        }
    }

    /// Sets the maximum number of bytes in all compacted files.
    /// We try to limit number of bytes in one compaction to be lower than this
    /// threshold. But it's not guaranteed.
    ///
    /// Value 0 will be sanitized.
    ///
    /// Default: target_file_size_base * 25
    pub fn set_max_compaction_bytes(&mut self, nbytes: u64) {
        unsafe {
            ffi::rocksdb_options_set_max_compaction_bytes(self.inner, nbytes);
        }
    }

    /// Specifies the absolute path of the directory the
    /// write-ahead log (WAL) should be written to.
    ///
    /// Default: same directory as the database
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut opts = Options::default();
    /// opts.set_wal_dir("/path/to/dir");
    /// ```
    pub fn set_wal_dir<P: AsRef<Path>>(&mut self, path: P) {
        let p = to_cpath(path).unwrap();
        unsafe {
            ffi::rocksdb_options_set_wal_dir(self.inner, p.as_ptr());
        }
    }

    /// Sets the WAL ttl in seconds.
    ///
    /// The following two options affect how archived logs will be deleted.
    /// 1. If both set to 0, logs will be deleted asap and will not get into
    ///    the archive.
    /// 2. If wal_ttl_seconds is 0 and wal_size_limit_mb is not 0,
    ///    WAL files will be checked every 10 min and if total size is greater
    ///    then wal_size_limit_mb, they will be deleted starting with the
    ///    earliest until size_limit is met. All empty files will be deleted.
    /// 3. If wal_ttl_seconds is not 0 and wall_size_limit_mb is 0, then
    ///    WAL files will be checked every wal_ttl_seconds / 2 and those that
    ///    are older than wal_ttl_seconds will be deleted.
    /// 4. If both are not 0, WAL files will be checked every 10 min and both
    ///    checks will be performed with ttl being first.
    ///
    /// Default: 0
    pub fn set_wal_ttl_seconds(&mut self, secs: u64) {
        unsafe {
            ffi::rocksdb_options_set_WAL_ttl_seconds(self.inner, secs);
        }
    }

    /// Sets the WAL size limit in MB.
    ///
    /// If total size of WAL files is greater then wal_size_limit_mb,
    /// they will be deleted starting with the earliest until size_limit is met.
    ///
    /// Default: 0
    pub fn set_wal_size_limit_mb(&mut self, size: u64) {
        unsafe {
            ffi::rocksdb_options_set_WAL_size_limit_MB(self.inner, size);
        }
    }

    /// Sets the number of bytes to preallocate (via fallocate) the manifest files.
    ///
    /// Default is 4MB, which is reasonable to reduce random IO
    /// as well as prevent overallocation for mounts that preallocate
    /// large amounts of data (such as xfs's allocsize option).
    pub fn set_manifest_preallocation_size(&mut self, size: usize) {
        unsafe {
            ffi::rocksdb_options_set_manifest_preallocation_size(self.inner, size);
        }
    }

    /// If true, then DB::Open() will not update the statistics used to optimize
    /// compaction decision by loading table properties from many files.
    /// Turning off this feature will improve DBOpen time especially in disk environment.
    ///
    /// Default: false
    pub fn set_skip_stats_update_on_db_open(&mut self, skip: bool) {
        unsafe {
            ffi::rocksdb_options_set_skip_stats_update_on_db_open(self.inner, c_uchar::from(skip));
        }
    }

    /// Controls whether RocksDB opens and validates SST files in the background after open.
    ///
    /// Enabling this can reduce open latency for databases with many SST files
    /// or high latency storage. It is mostly useful with
    /// [`Options::set_max_open_files`] set to `-1`.
    ///
    /// This option is not compatible with FIFO compaction and requires
    /// [`Options::set_skip_stats_update_on_db_open`] to be `true`. SST open
    /// errors are no longer returned by `DB::open`; they can instead surface as
    /// background errors or from operations that access the affected file.
    ///
    /// Default: `false`
    pub fn set_open_files_async(&mut self, enabled: bool) -> Result<(), Error> {
        let supported = unsafe {
            ffi::rust_rocksdb_options_set_open_files_async(self.inner, c_uchar::from(enabled)) != 0
        };
        if !supported {
            return Err(Error::new(
                "open_files_async requires RocksDB 11.1 or newer".to_owned(),
            ));
        }
        Ok(())
    }

    /// Returns whether SST files are opened and validated in the background after open.
    pub fn get_open_files_async(&self) -> bool {
        unsafe { ffi::rust_rocksdb_options_get_open_files_async(self.inner) != 0 }
    }

    /// Returns whether the linked RocksDB supports `open_files_async`.
    pub fn supports_open_files_async() -> bool {
        unsafe { ffi::rust_rocksdb_options_open_files_async_supported() != 0 }
    }

    /// Specify the maximal number of info log files to be kept.
    ///
    /// Default: 1000
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut options = Options::default();
    /// options.set_keep_log_file_num(100);
    /// ```
    pub fn set_keep_log_file_num(&mut self, nfiles: usize) {
        unsafe {
            ffi::rocksdb_options_set_keep_log_file_num(self.inner, nfiles);
        }
    }

    /// Allow the OS to mmap file for writing.
    ///
    /// Default: false
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut options = Options::default();
    /// options.set_allow_mmap_writes(true);
    /// ```
    pub fn set_allow_mmap_writes(&mut self, is_enabled: bool) {
        unsafe {
            ffi::rocksdb_options_set_allow_mmap_writes(self.inner, c_uchar::from(is_enabled));
        }
    }

    /// Allow the OS to mmap file for reading sst tables.
    ///
    /// Default: false
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut options = Options::default();
    /// options.set_allow_mmap_reads(true);
    /// ```
    pub fn set_allow_mmap_reads(&mut self, is_enabled: bool) {
        unsafe {
            ffi::rocksdb_options_set_allow_mmap_reads(self.inner, c_uchar::from(is_enabled));
        }
    }

    /// If enabled, WAL is not flushed automatically after each write. Instead it
    /// relies on manual invocation of `DB::flush_wal()` to write the WAL buffer
    /// to its file.
    ///
    /// Default: false
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut options = Options::default();
    /// options.set_manual_wal_flush(true);
    /// ```
    pub fn set_manual_wal_flush(&mut self, is_enabled: bool) {
        unsafe {
            ffi::rocksdb_options_set_manual_wal_flush(self.inner, c_uchar::from(is_enabled));
        }
    }

    /// Guarantee that all column families are flushed together atomically.
    /// This option applies to both manual flushes (`db.flush()`) and automatic
    /// background flushes caused when memtables are filled.
    ///
    /// Note that this is only useful when the WAL is disabled. When using the
    /// WAL, writes are always consistent across column families.
    ///
    /// Default: false
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut options = Options::default();
    /// options.set_atomic_flush(true);
    /// ```
    pub fn set_atomic_flush(&mut self, atomic_flush: bool) {
        unsafe {
            ffi::rocksdb_options_set_atomic_flush(self.inner, c_uchar::from(atomic_flush));
        }
    }

    /// Sets global cache for table-level rows.
    ///
    /// Default: null (disabled)
    /// Not supported in ROCKSDB_LITE mode!
    pub fn set_row_cache(&mut self, cache: &Cache) {
        unsafe {
            ffi::rocksdb_options_set_row_cache(self.inner, cache.0.inner.as_ptr());
        }
        self.outlive.row_cache = Some(cache.clone());
    }

    /// Use to control write rate of flush and compaction. Flush has higher
    /// priority than compaction.
    /// If rate limiter is enabled, bytes_per_sync is set to 1MB by default.
    ///
    /// Default: disable
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut options = Options::default();
    /// options.set_ratelimiter(1024 * 1024, 100 * 1000, 10);
    /// ```
    pub fn set_ratelimiter(
        &mut self,
        rate_bytes_per_sec: i64,
        refill_period_us: i64,
        fairness: i32,
    ) {
        unsafe {
            let ratelimiter =
                ffi::rocksdb_ratelimiter_create(rate_bytes_per_sec, refill_period_us, fairness);
            ffi::rocksdb_options_set_ratelimiter(self.inner, ratelimiter);
            ffi::rocksdb_ratelimiter_destroy(ratelimiter);
        }
    }

    /// Use to control write rate of flush and compaction. Flush has higher
    /// priority than compaction.
    /// If rate limiter is enabled, bytes_per_sync is set to 1MB by default.
    ///
    /// Default: disable
    pub fn set_auto_tuned_ratelimiter(
        &mut self,
        rate_bytes_per_sec: i64,
        refill_period_us: i64,
        fairness: i32,
    ) {
        unsafe {
            let ratelimiter = ffi::rocksdb_ratelimiter_create_auto_tuned(
                rate_bytes_per_sec,
                refill_period_us,
                fairness,
            );
            ffi::rocksdb_options_set_ratelimiter(self.inner, ratelimiter);
            ffi::rocksdb_ratelimiter_destroy(ratelimiter);
        }
    }

    /// Create a RateLimiter object, which can be shared among RocksDB instances to
    /// control write rate of flush and compaction.
    ///
    /// rate_bytes_per_sec: this is the only parameter you want to set most of the
    /// time. It controls the total write rate of compaction and flush in bytes per
    /// second. Currently, RocksDB does not enforce rate limit for anything other
    /// than flush and compaction, e.g. write to WAL.
    ///
    /// refill_period_us: this controls how often tokens are refilled. For example,
    /// when rate_bytes_per_sec is set to 10MB/s and refill_period_us is set to
    /// 100ms, then 1MB is refilled every 100ms internally. Larger value can lead to
    /// burstier writes while smaller value introduces more CPU overhead.
    /// The default should work for most cases.
    ///
    /// fairness: RateLimiter accepts high-pri requests and low-pri requests.
    /// A low-pri request is usually blocked in favor of hi-pri request. Currently,
    /// RocksDB assigns low-pri to request from compaction and high-pri to request
    /// from flush. Low-pri requests can get blocked if flush requests come in
    /// continuously. This fairness parameter grants low-pri requests permission by
    /// 1/fairness chance even though high-pri requests exist to avoid starvation.
    /// You should be good by leaving it at default 10.
    ///
    /// mode: Mode indicates which types of operations count against the limit.
    ///
    /// auto_tuned: Enables dynamic adjustment of rate limit within the range
    ///              `[rate_bytes_per_sec / 20, rate_bytes_per_sec]`, according to
    ///              the recent demand for background I/O.
    pub fn set_ratelimiter_with_mode(
        &mut self,
        rate_bytes_per_sec: i64,
        refill_period_us: i64,
        fairness: i32,
        mode: RateLimiterMode,
        auto_tuned: bool,
    ) {
        unsafe {
            let ratelimiter = ffi::rocksdb_ratelimiter_create_with_mode(
                rate_bytes_per_sec,
                refill_period_us,
                fairness,
                mode as c_int,
                auto_tuned,
            );
            ffi::rocksdb_options_set_ratelimiter(self.inner, ratelimiter);
            ffi::rocksdb_ratelimiter_destroy(ratelimiter);
        }
    }

    /// Sets the maximal size of the info log file.
    ///
    /// If the log file is larger than `max_log_file_size`, a new info log file
    /// will be created. If `max_log_file_size` is equal to zero, all logs will
    /// be written to one log file.
    ///
    /// Default: 0
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut options = Options::default();
    /// options.set_max_log_file_size(0);
    /// ```
    pub fn set_max_log_file_size(&mut self, size: usize) {
        unsafe {
            ffi::rocksdb_options_set_max_log_file_size(self.inner, size);
        }
    }

    /// Sets the time for the info log file to roll (in seconds).
    ///
    /// If specified with non-zero value, log file will be rolled
    /// if it has been active longer than `log_file_time_to_roll`.
    /// Default: 0 (disabled)
    pub fn set_log_file_time_to_roll(&mut self, secs: usize) {
        unsafe {
            ffi::rocksdb_options_set_log_file_time_to_roll(self.inner, secs);
        }
    }

    /// Controls the recycling of log files.
    ///
    /// If non-zero, previously written log files will be reused for new logs,
    /// overwriting the old data. The value indicates how many such files we will
    /// keep around at any point in time for later use. This is more efficient
    /// because the blocks are already allocated and fdatasync does not need to
    /// update the inode after each write.
    ///
    /// Default: 0
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::Options;
    ///
    /// let mut options = Options::default();
    /// options.set_recycle_log_file_num(5);
    /// ```
    pub fn set_recycle_log_file_num(&mut self, num: usize) {
        unsafe {
            ffi::rocksdb_options_set_recycle_log_file_num(self.inner, num);
        }
    }

    /// Prints logs to stderr for faster debugging
    /// See official [wiki](https://github.com/facebook/rocksdb/wiki/Logger) for more information.
    pub fn set_stderr_logger(&mut self, log_level: LogLevel, prefix: impl CStrLike) {
        let p = prefix.into_c_string().unwrap();

        unsafe {
            let logger = ffi::rocksdb_logger_create_stderr_logger(log_level as c_int, p.as_ptr());
            ffi::rocksdb_options_set_info_log(self.inner, logger);
            ffi::rocksdb_logger_destroy(logger);
        }
    }

    /// Invokes `callback` with RocksDB log messages with level >= `log_level`.
    ///
    /// The callback can be called concurrently by multiple RocksDB threads.
    ///
    /// # Examples
    /// ```
    /// use rust_rocksdb::{LogLevel, Options};
    ///
    /// let mut options = Options::default();
    /// options.set_callback_logger(LogLevel::Debug, move |level, msg| println!("{level:?} {msg}"));
    /// ```
    pub fn set_callback_logger(
        &mut self,
        log_level: LogLevel,
        callback: impl Fn(LogLevel, &str) + 'static + Send + Sync,
    ) {
        // store the closure in an Arc so it can be shared across multiple Option/DBs
        let holder = Arc::new(LogCallback {
            callback: Box::new(callback),
        });
        let holder_ptr = std::ptr::from_ref::<LogCallback>(holder.as_ref());
        let holder_cvoid = holder_ptr.cast::<c_void>().cast_mut();

        unsafe {
            let logger = ffi::rocksdb_logger_create_callback_logger(
                log_level as c_int,
                Some(Self::logger_callback),
                holder_cvoid,
            );
            ffi::rocksdb_options_set_info_log(self.inner, logger);
            ffi::rocksdb_logger_destroy(logger);
        }

        self.outlive.log_callback = Some(holder);
    }

    extern "C" fn logger_callback(func: *mut c_void, level: u32, msg: *mut c_char, len: usize) {
        use std::process;

        // Neither argument can be trusted:
        //
        //  * `LogLevel` is `#[repr(i32)]`, and `level` is whatever
        //    `InfoLogLevel` the C layer cast to an unsigned, so transmuting it
        //    could materialise an invalid discriminant.
        //  * `msg` is raw `vsnprintf` output. Log lines routinely embed
        //    filesystem paths and `Status::ToString()` text, and paths reach
        //    RocksDB via `OsStr::as_bytes()`, which is not UTF-8 validated, so
        //    `from_utf8_unchecked` was unsound.
        //
        // `from_utf8_lossy` returns `Cow::Borrowed` for valid UTF-8, so the
        // common path still does not allocate.
        let level = LogLevel::try_from_raw(level as i32).unwrap_or(LogLevel::Info);
        let slice = if len == 0 {
            &[][..]
        } else {
            unsafe { slice::from_raw_parts(msg.cast_const().cast::<u8>(), len) }
        };
        let msg = String::from_utf8_lossy(slice);

        // Shared reference, not `&mut`: RocksDB logs from several background
        // threads at once, so a `&mut` here would alias. `LogCallbackFn` is a
        // `dyn Fn`, so a shared reference is all it needs.
        let holder = unsafe { &*func.cast::<LogCallback>() };
        let callback_in_catch_unwind = AssertUnwindSafe(&holder.callback);
        if catch_unwind(move || callback_in_catch_unwind(level, &msg)).is_err() {
            process::abort();
        }
    }

    /// Sets the threshold at which all writes will be slowed down to at least delayed_write_rate if estimated
    /// bytes needed to be compaction exceed this threshold.
    ///
    /// Default: 64GB
    pub fn set_soft_pending_compaction_bytes_limit(&mut self, limit: usize) {
        unsafe {
            ffi::rocksdb_options_set_soft_pending_compaction_bytes_limit(self.inner, limit);
        }
    }

    /// Sets the bytes threshold at which all writes are stopped if estimated bytes needed to be compaction exceed
    /// this threshold.
    ///
    /// Default: 256GB
    pub fn set_hard_pending_compaction_bytes_limit(&mut self, limit: usize) {
        unsafe {
            ffi::rocksdb_options_set_hard_pending_compaction_bytes_limit(self.inner, limit);
        }
    }

    /// Sets the size of one block in arena memory allocation.
    ///
    /// If <= 0, a proper value is automatically calculated (usually 1/10 of
    /// writer_buffer_size).
    ///
    /// Default: 0
    pub fn set_arena_block_size(&mut self, size: usize) {
        unsafe {
            ffi::rocksdb_options_set_arena_block_size(self.inner, size);
        }
    }

    /// If true, then print malloc stats together with rocksdb.stats when printing to LOG.
    ///
    /// Default: false
    pub fn set_dump_malloc_stats(&mut self, enabled: bool) {
        unsafe {
            ffi::rocksdb_options_set_dump_malloc_stats(self.inner, c_uchar::from(enabled));
        }
    }

    /// Enable whole key bloom filter in memtable. Note this will only take effect
    /// if memtable_prefix_bloom_size_ratio is not 0. Enabling whole key filtering
    /// can potentially reduce CPU usage for point-look-ups.
    ///
    /// Default: false (disable)
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_memtable_whole_key_filtering(&mut self, whole_key_filter: bool) {
        unsafe {
            ffi::rocksdb_options_set_memtable_whole_key_filtering(
                self.inner,
                c_uchar::from(whole_key_filter),
            );
        }
    }

    /// Enable the use of key-value separation.
    ///
    /// More details can be found here: [Integrated BlobDB](http://rocksdb.org/blog/2021/05/26/integrated-blob-db.html).
    ///
    /// Default: false (disable)
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_enable_blob_files(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_enable_blob_files(self.inner, u8::from(val));
        }
    }

    /// Sets the minimum threshold value at or above which will be written
    /// to blob files during flush or compaction.
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_min_blob_size(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_min_blob_size(self.inner, val);
        }
    }

    /// Sets the size limit for blob files.
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_blob_file_size(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_blob_file_size(self.inner, val);
        }
    }

    /// Sets the blob compression type. All blob files use the same
    /// compression type.
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_blob_compression_type(&mut self, val: DBCompressionType) {
        unsafe {
            ffi::rocksdb_options_set_blob_compression_type(self.inner, val as _);
        }
    }

    /// If this is set to true RocksDB will actively relocate valid blobs from the oldest blob files
    /// as they are encountered during compaction.
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_enable_blob_gc(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_enable_blob_gc(self.inner, u8::from(val));
        }
    }

    /// Sets the threshold that the GC logic uses to determine which blob files should be considered “old.”
    ///
    /// For example, the default value of 0.25 signals to RocksDB that blobs residing in the
    /// oldest 25% of blob files should be relocated by GC. This parameter can be tuned to adjust
    /// the trade-off between write amplification and space amplification.
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_blob_gc_age_cutoff(&mut self, val: c_double) {
        unsafe {
            ffi::rocksdb_options_set_blob_gc_age_cutoff(self.inner, val);
        }
    }

    /// Sets the blob GC force threshold.
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_blob_gc_force_threshold(&mut self, val: c_double) {
        unsafe {
            ffi::rocksdb_options_set_blob_gc_force_threshold(self.inner, val);
        }
    }

    /// Sets the blob compaction read ahead size.
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_blob_compaction_readahead_size(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_blob_compaction_readahead_size(self.inner, val);
        }
    }

    /// Sets the blob cache.
    ///
    /// Using a dedicated object for blobs and using the same object for the block and blob caches
    /// are both supported. In the latter case, note that blobs are less valuable from a caching
    /// perspective than SST blocks, and some cache implementations have configuration options that
    /// can be used to prioritize items accordingly (see Cache::Priority and
    /// LRUCacheOptions::{high,low}_pri_pool_ratio).
    ///
    /// Default: disabled
    pub fn set_blob_cache(&mut self, cache: &Cache) {
        unsafe {
            ffi::rocksdb_options_set_blob_cache(self.inner, cache.0.inner.as_ptr());
        }
        self.outlive.blob_cache = Some(cache.clone());
    }

    /// Set this option to true during creation of database if you want
    /// to be able to ingest behind (call IngestExternalFile() skipping keys
    /// that already exist, rather than overwriting matching keys).
    /// Setting this option to true has the following effects:
    ///
    /// 1. Disable some internal optimizations around SST file compression.
    /// 2. Reserve the last level for ingested files only.
    /// 3. Compaction will not include any file from the last level.
    ///
    /// Note that only Universal Compaction supports allow_ingest_behind.
    /// `num_levels` should be >= 3 if this option is turned on.
    ///
    /// DEFAULT: false
    /// Immutable.
    pub fn set_allow_ingest_behind(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_allow_ingest_behind(self.inner, c_uchar::from(val));
        }
    }

    // A factory of a table property collector that marks an SST
    // file as need-compaction when it observe at least "D" deletion
    // entries in any "N" consecutive entries, or the ratio of tombstone
    // entries >= deletion_ratio.
    //
    // `window_size`: is the sliding window size "N"
    // `num_dels_trigger`: is the deletion trigger "D"
    // `deletion_ratio`: if <= 0 or > 1, disable triggering compaction based on
    // deletion ratio.
    pub fn add_compact_on_deletion_collector_factory(
        &mut self,
        window_size: size_t,
        num_dels_trigger: size_t,
        deletion_ratio: f64,
    ) {
        unsafe {
            ffi::rocksdb_options_add_compact_on_deletion_collector_factory_del_ratio(
                self.inner,
                window_size,
                num_dels_trigger,
                deletion_ratio,
            );
        }
    }

    /// Like [`Self::add_compact_on_deletion_collector_factory`], but only triggers
    /// compaction if the SST file size is at least `min_file_size` bytes.
    pub fn add_compact_on_deletion_collector_factory_min_file_size(
        &mut self,
        window_size: size_t,
        num_dels_trigger: size_t,
        deletion_ratio: f64,
        min_file_size: u64,
    ) {
        unsafe {
            ffi::rocksdb_options_add_compact_on_deletion_collector_factory_min_file_size(
                self.inner,
                window_size,
                num_dels_trigger,
                deletion_ratio,
                min_file_size,
            );
        }
    }

    /// <https://github.com/facebook/rocksdb/wiki/Write-Buffer-Manager>
    /// Write buffer manager helps users control the total memory used by memtables across multiple column families and/or DB instances.
    /// Users can enable this control by 2 ways:
    ///
    /// 1- Limit the total memtable usage across multiple column families and DBs under a threshold.
    /// 2- Cost the memtable memory usage to block cache so that memory of RocksDB can be capped by the single limit.
    /// The usage of a write buffer manager is similar to rate_limiter and sst_file_manager.
    /// Users can create one write buffer manager object and pass it to all the options of column families or DBs whose memtable size they want to be controlled by this object.
    pub fn set_write_buffer_manager(&mut self, write_buffer_manager: &WriteBufferManager) {
        unsafe {
            ffi::rocksdb_options_set_write_buffer_manager(
                self.inner,
                write_buffer_manager.0.inner.as_ptr(),
            );
        }
        self.outlive.write_buffer_manager = Some(write_buffer_manager.clone());
    }

    /// Sets an `SstFileManager` for this `Options`.
    ///
    /// SstFileManager tracks and controls total SST file space usage, enabling
    /// applications to cap disk utilization and throttle deletions.
    pub fn set_sst_file_manager(&mut self, sst_file_manager: &SstFileManager) {
        unsafe {
            ffi::rocksdb_options_set_sst_file_manager(
                self.inner,
                sst_file_manager.0.inner.as_ptr(),
            );
        }
        self.outlive.sst_file_manager = Some(sst_file_manager.clone());
    }

    /// If true, working thread may avoid doing unnecessary and long-latency
    /// operation (such as deleting obsolete files directly or deleting memtable)
    /// and will instead schedule a background job to do it.
    ///
    /// Use it if you're latency-sensitive.
    ///
    /// Default: false (disabled)
    pub fn set_avoid_unnecessary_blocking_io(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_avoid_unnecessary_blocking_io(self.inner, u8::from(val));
        }
    }

    /// Activates the experimental Mempurge memtable garbage collection feature.
    ///
    /// See the upstream RocksDB option documentation:
    /// <https://github.com/facebook/rocksdb/blob/v10.7.5/include/rocksdb/advanced_options.h#L259-L274>
    ///
    /// At every flush, RocksDB estimates the useful payload ratio of the memtable
    /// and compares it with this threshold. If the ratio is below the threshold,
    /// RocksDB replaces the regular flush with a mempurge operation.
    ///
    /// Threshold values:
    ///
    /// * `0.0`: mempurge deactivated.
    /// * `1.0`: recommended threshold value.
    /// * `> 1.0`: aggressive mempurge.
    /// * `0.0 < threshold < 1.0`: mempurge only for very low useful payload ratios.
    ///
    /// Default: 0.0
    pub fn set_experimental_mempurge_threshold(&mut self, threshold: f64) {
        unsafe {
            ffi::rocksdb_options_set_experimental_mempurge_threshold(self.inner, threshold);
        }
    }

    /// Sets the compaction priority.
    ///
    /// If level compaction_style =
    /// kCompactionStyleLevel, for each level, which files are prioritized to be
    /// picked to compact.
    ///
    /// Default: `DBCompactionPri::MinOverlappingRatio`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{Options, DBCompactionPri};
    ///
    /// let mut opts = Options::default();
    /// opts.set_compaction_pri(DBCompactionPri::RoundRobin);
    /// ```
    pub fn set_compaction_pri(&mut self, pri: DBCompactionPri) {
        unsafe {
            ffi::rocksdb_options_set_compaction_pri(self.inner, pri as c_int);
        }
    }

    /// If true, the log numbers and sizes of the synced WALs are tracked
    /// in MANIFEST. During DB recovery, if a synced WAL is missing
    /// from disk, or the WAL's size does not match the recorded size in
    /// MANIFEST, an error will be reported and the recovery will be aborted.
    ///
    /// This is one additional protection against WAL corruption besides the
    /// per-WAL-entry checksum.
    ///
    /// Note that this option does not work with secondary instance.
    /// Currently, only syncing closed WALs are tracked. Calling `DB::SyncWAL()`,
    /// etc. or writing with `WriteOptions::sync=true` to sync the live WAL is not
    /// tracked for performance/efficiency reasons.
    ///
    /// See: <https://github.com/facebook/rocksdb/wiki/Track-WAL-in-MANIFEST>
    ///
    /// Default: false (disabled)
    pub fn set_track_and_verify_wals_in_manifest(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_track_and_verify_wals_in_manifest(self.inner, u8::from(val));
        }
    }

    /// Returns the value of the `track_and_verify_wals_in_manifest` option.
    pub fn get_track_and_verify_wals_in_manifest(&self) -> bool {
        let val_u8 =
            unsafe { ffi::rocksdb_options_get_track_and_verify_wals_in_manifest(self.inner) };
        val_u8 != 0
    }

    /// The DB unique ID can be saved in the DB manifest (preferred, this option)
    /// or an IDENTITY file (historical, deprecated), or both. If this option is
    /// set to false (old behavior), then `write_identity_file` must be set to true.
    /// The manifest is preferred because
    ///
    /// 1. The IDENTITY file is not checksummed, so it is not as safe against
    ///    corruption.
    /// 2. The IDENTITY file may or may not be copied with the DB (e.g. not
    ///    copied by BackupEngine), so is not reliable for the provenance of a DB.
    ///
    /// This option might eventually be obsolete and removed as Identity files
    /// are phased out.
    ///
    /// Default: true (enabled)
    pub fn set_write_dbid_to_manifest(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_write_dbid_to_manifest(self.inner, u8::from(val));
        }
    }

    /// Returns the value of the `write_dbid_to_manifest` option.
    pub fn get_write_dbid_to_manifest(&self) -> bool {
        let val_u8 = unsafe { ffi::rocksdb_options_get_write_dbid_to_manifest(self.inner) };
        val_u8 != 0
    }

    /// Sets the logger to use.
    ///
    /// By default `rocksdb` writes its internal logs to a file in the database
    /// directory; this can be changed to a custom callback with the
    /// [`InfoLogger::new_callback_logger`] constructor.
    pub fn set_info_logger(&mut self, mut logger: InfoLogger) {
        // Move the callback so it can be shared across database instances
        self.outlive.logger_callback = logger.callback.take();
        unsafe {
            ffi::rocksdb_options_set_info_log(self.inner, logger.inner);
        }
    }

    /// Returns a reference to the currently configured logger.
    pub fn get_info_logger(&self) -> InfoLogger {
        let raw = unsafe { ffi::rocksdb_options_get_info_log(self.inner) };
        InfoLogger {
            inner: raw,
            callback: self.outlive.logger_callback.clone(),
        }
    }

    /// Sets the `add` option.
    pub fn set_add(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_options_calculate_sst_write_lifetime_hint_set_add(self.inner, val);
        }
    }

    /// if set to false then recovery will fail when a prepared transaction is encountered in
    /// the WAL
    pub fn set_allow_2pc(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_allow_2pc(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `allow_2pc` option.
    pub fn get_allow_2pc(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_allow_2pc(self.inner) != 0 }
    }

    /// It allows user to opt-in to get error messages containing corrupted keys/values.
    /// Corrupt keys, values will be logged in the messages/logs/status that will help users
    /// with the useful information regarding affected data. By default value is set false to
    /// prevent users data to be exposed in the logs/messages etc.
    ///
    /// Default: false
    pub fn set_allow_data_in_errors(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_allow_data_in_errors(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `allow_data_in_errors` option.
    pub fn get_allow_data_in_errors(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_allow_data_in_errors(self.inner) != 0 }
    }

    /// If false, fallocate() calls are bypassed, which disables file preallocation. The file
    /// space preallocation is used to increase the file write/append performance. By default,
    /// RocksDB preallocates space for WAL, SST, Manifest files, the extra space is truncated
    /// when the file is written. Warning: if you're using btrfs, we would recommend setting
    /// `allow_fallocate=false` to disable preallocation. As on btrfs, the extra allocated
    /// space cannot be freed, which could be significant if you have lots of files. More
    /// details about this limitation:
    /// <https://github.com/btrfs/btrfs-dev-docs/blob/471c5699336e043114d4bca02adcd57d9dab9c44/data-extent-reference-counts.md>
    pub fn set_allow_fallocate(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_allow_fallocate(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `allow_fallocate` option.
    pub fn get_allow_fallocate(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_allow_fallocate(self.inner) != 0 }
    }

    /// EXPERIMENTAL: If true, RocksDB asynchronously precreates the next WAL file so
    /// foreground memtable switching can usually avoid the filesystem latency of creating a
    /// new WAL. The precreated file is only reserved empty storage; it does not become a
    /// logical WAL and is not added to WAL tracking until it is consumed by a foreground WAL
    /// rotation.
    ///
    /// The option is sanitized to false when recycle_log_file_num is non-zero.
    ///
    /// Default: false
    pub fn set_async_wal_precreate(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_async_wal_precreate(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `async_wal_precreate` option.
    pub fn get_async_wal_precreate(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_async_wal_precreate(self.inner) != 0 }
    }

    /// By default RocksDB replay WAL logs and flush them on DB open, which may create very
    /// small SST files. If this option is enabled, RocksDB will try to avoid (but not
    /// guarantee not to) flush during recovery. Also, existing WAL logs will be kept, so that
    /// if crash happened before flush, we still have logs to recover from.
    ///
    /// Note: when `enforce_write_buffer_manager_during_recovery` is also enabled, flushes may
    /// still occur during recovery to respect the WriteBufferManager's global memory limit,
    /// even if this option is true. Once any such WBM-triggered flush happens, all remaining
    /// memtables will also be flushed at the end of recovery (similar to the behavior when
    /// this option is false).
    ///
    /// DEFAULT: false
    pub fn set_avoid_flush_during_recovery(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_avoid_flush_during_recovery(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `avoid_flush_during_recovery` option.
    pub fn get_avoid_flush_during_recovery(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_avoid_flush_during_recovery(self.inner) != 0 }
    }

    /// By default RocksDB will flush all memtables on DB close if there are unpersisted data
    /// (i.e. with WAL disabled) The flush can be skip to speedup DB close. Unpersisted data
    /// WILL BE LOST.
    ///
    /// DEFAULT: false
    ///
    /// Dynamically changeable through SetDBOptions() API.
    pub fn set_avoid_flush_during_shutdown(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_avoid_flush_during_shutdown(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `avoid_flush_during_shutdown` option.
    pub fn get_avoid_flush_during_shutdown(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_avoid_flush_during_shutdown(self.inner) != 0 }
    }

    /// Set to true to re-instate an old behavior of keeping complete, synced WAL files open
    /// for write until they are collected for deletion by a background thread. This should
    /// not be needed unless there is a performance issue with file Close(), but setting it to
    /// true means that Checkpoint might call LinkFile on a WAL still open for write, which
    /// might be unsupported on some FileSystem implementations. As this is intended as a
    /// temporary kill switch, it is already DEPRECATED.
    pub fn set_background_close_inactive_wals(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_background_close_inactive_wals(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `background_close_inactive_wals` option.
    pub fn get_background_close_inactive_wals(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_background_close_inactive_wals(self.inner) != 0 }
    }

    /// By default, RocksDB will attempt to detect any data losses or corruptions in DB files
    /// and return an error to the user, either at DB::Open time or later during DB operation.
    /// The exception to this policy is the WAL file, whose recovery is controlled by the
    /// wal_recovery_mode option.
    ///
    /// Best-efforts recovery (this option set to true) signals a preference for opening the
    /// DB to any point-in-time valid state for each column family, including the empty/new
    /// state, versus the default of returning non-WAL data losses to the user as errors. In
    /// terms of RocksDB user data, this is like applying
    /// WALRecoveryMode::kPointInTimeRecovery to each column family rather than just the WAL.
    ///
    /// The behavior changes in the presence of "AtomicGroup"s in the MANIFEST, which is
    /// currently only the case when `atomic_flush == true`. In that case, all pre-existing
    /// CFs must recover the atomic group in order for that group to be applied in an
    /// all-or-nothing manner. This means that unused/inactive CF(s) with invalid filesystem
    /// state can block recovery of all other CFs at an atomic group.
    ///
    /// Best-efforts recovery (BER) is specifically designed to recover a DB with files that
    /// are missing or truncated to some smaller size, such as the result of an incomplete DB
    /// "physical" (FileSystem) copy. BER can also detect when an SST file has been replaced
    /// with a different one of the same size (assuming SST unique IDs are tracked in DB
    /// manifest). BER is not yet designed to produce a usable DB from other corruptions to DB
    /// files (which should generally be detectable by DB::VerifyChecksum()), and BER does not
    /// yet attempt to recover any WAL files.
    ///
    /// For example, if an SST or blob file referenced by the MANIFEST is missing, BER might
    /// be able to find a set of files corresponding to an old "point in time" version of the
    /// column family, possibly from an older MANIFEST file. Besides complete "point in time"
    /// version, an incomplete version with only a suffix of L0 files missing can also be
    /// recovered to if the versioning history doesn't include an atomic flush.  From the
    /// users' perspective, missing a suffix of L0 files means missing the user's most
    /// recently written data. So the remaining available files still presents a valid point
    /// in time view, although for some previous time. It's not done for atomic flush because
    /// that guarantees a consistent view across column families. We cannot guarantee that if
    /// recovering an incomplete version. Some other kinds of DB files (e.g. CURRENT, LOCK,
    /// IDENTITY) are either ignored or replaced with BER, or quietly fixed regardless of BER
    /// setting. BER does require at least one valid MANIFEST to recover to a non-trivial DB
    /// state, unlike `ldb repair`.
    ///
    /// Default: false
    pub fn set_best_efforts_recovery(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_best_efforts_recovery(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `best_efforts_recovery` option.
    pub fn get_best_efforts_recovery(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_best_efforts_recovery(self.inner) != 0 }
    }

    /// If max_bgerror_resume_count is >= 2, db resume is called multiple times. This option
    /// decides how long to wait to retry the next resume if the previous resume fails and
    /// satisfy redo resume conditions.
    ///
    /// Default: 1000000 (microseconds).
    pub fn set_bgerror_resume_retry_interval(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_bgerror_resume_retry_interval(self.inner, val);
        }
    }

    /// Returns the value of the `bgerror_resume_retry_interval` option.
    pub fn get_bgerror_resume_retry_interval(&self) -> u64 {
        unsafe { ffi::rocksdb_options_get_bgerror_resume_retry_interval(self.inner) }
    }

    /// Number of direct-write blob partitions for this column family. Requires
    /// enable_blob_direct_write = true.
    ///
    /// If blob_direct_write_partition_strategy is null, partition selection uses the default
    /// round-robin strategy.
    ///
    /// Default: 1
    ///
    /// Not dynamically changeable through the SetOptions() API.
    pub fn set_blob_direct_write_partitions(&mut self, val: u32) {
        unsafe {
            ffi::rocksdb_options_set_blob_direct_write_partitions(self.inner, val);
        }
    }

    /// Returns the value of the `blob_direct_write_partitions` option.
    pub fn get_blob_direct_write_partitions(&self) -> u32 {
        unsafe { ffi::rocksdb_options_get_blob_direct_write_partitions(self.inner) }
    }

    /// Enable/disable per key-value checksum protection for in memory blocks.
    ///
    /// Checksum is constructed when a block is loaded into memory and verification is done
    /// for each key read from the block. This is useful for detecting in-memory data
    /// corruption. Note that this feature has a non-trivial negative impact on read
    /// performance. Different values of the option have similar performance impact, but
    /// different memory cost and corruption detection probability (e.g. 1 byte gives 255/256
    /// chance for detecting a corruption).
    ///
    /// Default: 0 (no protection) Supported values: 0, 1, 2, 4, 8. Dynamically changeable
    /// through the SetOptions() API.
    pub fn set_block_protection_bytes_per_key(&mut self, val: u8) {
        unsafe {
            ffi::rocksdb_options_set_block_protection_bytes_per_key(self.inner, val);
        }
    }

    /// Returns the value of the `block_protection_bytes_per_key` option.
    pub fn get_block_protection_bytes_per_key(&self) -> u8 {
        unsafe { ffi::rocksdb_options_get_block_protection_bytes_per_key(self.inner) }
    }

    /// For leveled compaction, RocksDB may compact a file at the bottommost level if it can
    /// compact away data that were protected by some snapshot. The compaction reason in LOG
    /// for this kind of compactions is "BottommostFiles". Usually such compaction can happen
    /// as soon as a relevant snapshot is released. This option allows user to delay such
    /// compactions. A file is qualified for "BottommostFiles" compaction if it is at least
    /// "bottommost_file_compaction_delay" seconds old.
    ///
    /// Default: 0 (no delay) Dynamically changeable through the SetOptions() API.
    pub fn set_bottommost_file_compaction_delay(&mut self, val: u32) {
        unsafe {
            ffi::rocksdb_options_set_bottommost_file_compaction_delay(self.inner, val);
        }
    }

    /// Returns the value of the `bottommost_file_compaction_delay` option.
    pub fn get_bottommost_file_compaction_delay(&self) -> u32 {
        unsafe { ffi::rocksdb_options_get_bottommost_file_compaction_delay(self.inner) }
    }

    /// If either DBOptions::allow_ingest_behind or this option is set to true, this column
    /// family will prepare for ingesting files to the last level (IngestExternalFiles() with
    /// ingest_behind=true). Users should set only this option since
    /// DBOptions::allow_ingest_behind is deprecated.
    ///
    /// Specifically, preparing a column family for ingesting files to the last level has the
    /// following effects:
    /// - Disables some internal optimizations around SST file compression.
    /// - Reserves the last level for ingested files only.
    /// - Compaction will not include any file from the last level.
    /// - Compaction will preserve necessary tombstones that can apply on top of ingested
    ///   files.
    ///
    /// Note that only Universal Compaction supports cf_allow_ingest_behind. `num_levels`
    /// should be >= 3 if this option is turned on.
    ///
    /// Note that this option needs to be set to true before any write to the CF. It's
    /// recommended to set the option to true since CF creation. Otherwise, ingestion with
    /// ingest_behind = true might fail. Once file ingestions are done, the option should be
    /// flipped to false. Flipping this option to false allows the CF to disable the behavior
    /// changes detailed above and resume more efficient operation.
    ///
    /// Default: false Immutable.
    pub fn set_cf_allow_ingest_behind(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_cf_allow_ingest_behind(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `cf_allow_ingest_behind` option.
    pub fn get_cf_allow_ingest_behind(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_cf_allow_ingest_behind(self.inner) != 0 }
    }

    /// DEPRECATED: This option might be removed in a future release.
    ///
    /// If true, during compaction, RocksDB will count the number of entries read and compare
    /// it against the number of entries in the compaction input files. This is intended to
    /// add protection against corruption during compaction. Note that
    /// - this verification is not done for compactions during which a compaction filter
    ///   returns kRemoveAndSkipUntil, and
    /// - the number of range deletions is not verified.
    ///
    /// The option is here to turn the feature off in case this new validation feature has a
    /// bug. The option may be removed in the future once the feature is stable.
    ///
    /// Default: true
    pub fn set_compaction_verify_record_count(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_compaction_verify_record_count(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `compaction_verify_record_count` option.
    pub fn get_compaction_verify_record_count(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_compaction_verify_record_count(self.inner) != 0 }
    }

    /// EXPERIMENTAL When this field is set, all SST files without an explicitly set
    /// temperature will be treated as if they have this temperature for file reading
    /// accounting purpose, such as io statistics, io perf context.
    ///
    /// Not dynamically changeable; change requires DB restart.
    pub fn set_default_temperature(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_options_set_default_temperature(self.inner, val);
        }
    }

    /// Returns the value of the `default_temperature` option.
    pub fn get_default_temperature(&self) -> c_int {
        unsafe { ffi::rocksdb_options_get_default_temperature(self.inner) }
    }

    /// EXPERIMENTAL When no other option such as last_level_temperature determines the
    /// temperature of a new SST file, it will be written with this temperature, which can be
    /// set differently for each column family.
    ///
    /// Dynamically changeable through the SetOptions() API
    pub fn set_default_write_temperature(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_options_set_default_write_temperature(self.inner, val);
        }
    }

    /// Returns the value of the `default_write_temperature` option.
    pub fn get_default_write_temperature(&self) -> c_int {
        unsafe { ffi::rocksdb_options_get_default_write_temperature(self.inner) }
    }

    /// The limited write rate to DB if soft_pending_compaction_bytes_limit or
    /// level0_slowdown_writes_trigger is triggered, or we are writing to the last mem table
    /// allowed and we allow more than 3 mem tables. It is calculated using size of user write
    /// requests before compression. RocksDB may decide to slow down more if the compaction
    /// still gets behind further. If the value is 0, we will infer a value from
    /// `rater_limiter` value if it is not empty, or 16MB if `rater_limiter` is empty. Note
    /// that if users change the rate in `rate_limiter` after DB is opened,
    /// `delayed_write_rate` won't be adjusted.
    ///
    /// Unit: byte per second.
    ///
    /// Default: 0
    ///
    /// Dynamically changeable through SetDBOptions() API.
    pub fn set_delayed_write_rate(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_delayed_write_rate(self.inner, val);
        }
    }

    /// Returns the value of the `delayed_write_rate` option.
    pub fn get_delayed_write_rate(&self) -> u64 {
        unsafe { ffi::rocksdb_options_get_delayed_write_rate(self.inner) }
    }

    /// Setting this option to true disallows ordinary writes to the column family and it can
    /// only be populated through import and ingestion. It is intended to protect "ingestion
    /// only" column families. This option is not currently supported on the default column
    /// family because of error handling challenges analogous to
    /// <https://github.com/facebook/rocksdb/issues/13429>
    ///
    /// This option is not mutable with SetOptions(). It can be changed between DB::Open()
    /// calls, but open will fail if recovering WAL writes to a CF with this option set.
    pub fn set_disallow_memtable_writes(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_disallow_memtable_writes(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `disallow_memtable_writes` option.
    pub fn get_disallow_memtable_writes(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_disallow_memtable_writes(self.inner) != 0 }
    }

    /// If true, then print malloc stats together with rocksdb.stats when printing to LOG.
    /// DEFAULT: false
    pub fn get_dump_malloc_stats(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_dump_malloc_stats(self.inner) != 0 }
    }

    /// When enabled, values >= min_blob_size are written directly to blob files during the
    /// write path and replaced in WAL and memtable with BlobIndex references.
    ///
    /// Requires enable_blob_files = true. Experimental reduced-scope v1 restrictions. These
    /// limitations keep the v1 implementation intentionally small; follow-up PRs are expected
    /// to improve feature compatibility over time:
    /// - only supports the ordered single-memtable-writer path; unordered, pipelined,
    ///   two_write_queues, and allow_concurrent_memtable_write are not supported.
    /// - crash recovery only supports blob files that were already made manifest-visible by
    ///   flush/SST creation; WAL replay of active direct-write blob files is not currently
    ///   supported.
    /// - checkpoint/backup/live-files enumeration must flush pending direct-write state
    ///   first; APIs that intentionally skip the flush, or run while WAL is locked, can
    ///   return NotSupported.
    /// - not compatible with MemPurge or user-defined timestamps.
    /// - DB::IngestWriteBatchWithIndex() is not supported while any live column family
    ///   enables this option.
    /// - read-only and secondary opens can read flushed/manifest-visible blob files, but do
    ///   not resolve still-active direct-write blob files.
    ///
    /// Default: false
    ///
    /// Not dynamically changeable through the SetOptions() API.
    pub fn set_enable_blob_direct_write(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_enable_blob_direct_write(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `enable_blob_direct_write` option.
    pub fn get_enable_blob_direct_write(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_enable_blob_direct_write(self.inner) != 0 }
    }

    /// If true, then the status of the threads involved in this DB will be tracked and
    /// available via GetThreadList() API.
    ///
    /// Default: false
    pub fn set_enable_thread_tracking(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_enable_thread_tracking(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `enable_thread_tracking` option.
    pub fn get_enable_thread_tracking(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_enable_thread_tracking(self.inner) != 0 }
    }

    /// DEPRECATED: This option might be removed in a future release.
    ///
    /// If set to false, when compaction or flush sees a SingleDelete followed by a Delete for
    /// the same user key, compaction job will not fail. Otherwise, compaction job will fail.
    /// This is a temporary option to help existing use cases migrate, and will be removed in
    /// a future release. Warning: do not set to false unless you are trying to migrate
    /// existing data in which the contract of single delete
    /// (<https://github.com/facebook/rocksdb/wiki/Single-Delete>) is not enforced, thus has
    /// Delete mixed with SingleDelete for the same user key. Violation of the contract leads
    /// to undefined behaviors with high possibility of data inconsistency, e.g. deleted old
    /// data become visible again, etc.
    pub fn set_enforce_single_del_contracts(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_enforce_single_del_contracts(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `enforce_single_del_contracts` option.
    pub fn get_enforce_single_del_contracts(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_enforce_single_del_contracts(self.inner) != 0 }
    }

    /// If true and a WriteBufferManager is configured, RocksDB will check
    /// WriteBufferManager::ShouldFlush() during WAL recovery and schedule flushes when
    /// needed. This prevents OOM when multiple RocksDB instances share a WriteBufferManager
    /// and one instance is recovering from WAL.
    ///
    /// When triggered, all column families with non-empty memtables are scheduled for flush,
    /// which may produce smaller L0 files in some column families. This also overrides
    /// `avoid_flush_during_recovery`: once a WBM-triggered flush occurs mid-recovery, all
    /// remaining non-empty memtables will be flushed at the end of recovery as well.
    ///
    /// DEFAULT: true
    pub fn set_enforce_write_buffer_manager_during_recovery(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_enforce_write_buffer_manager_during_recovery(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `enforce_write_buffer_manager_during_recovery` option.
    pub fn get_enforce_write_buffer_manager_during_recovery(&self) -> bool {
        unsafe {
            ffi::rocksdb_options_get_enforce_write_buffer_manager_during_recovery(self.inner) != 0
        }
    }

    /// EXPERIMENTAL When this is true, save file system metadata (if supported by the FS) for
    /// SST files added to the DB in the MANIFEST, and use it to accelerate re-opening of
    /// those files on DB open. This will help cut down DB open latency on remote storage
    /// systems.
    pub fn set_fast_sst_open(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_fast_sst_open(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `fast_sst_open` option.
    pub fn get_fast_sst_open(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_fast_sst_open(self.inner) != 0 }
    }

    /// DEPRECATED: This option might be removed in a future release.
    ///
    /// If true, during memtable flush, RocksDB will validate total entries read in flush,
    /// total entries written in the SST and compare them with counter of keys added.
    ///
    /// The option is here to turn the feature off in case this new validation feature has a
    /// bug. The option may be removed in the future once the feature is stable.
    ///
    /// Default: true
    pub fn set_flush_verify_memtable_count(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_flush_verify_memtable_count(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `flush_verify_memtable_count` option.
    pub fn get_flush_verify_memtable_count(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_flush_verify_memtable_count(self.inner) != 0 }
    }

    /// For a given catch up attempt, this option specifies the number of times to tail the
    /// MANIFEST and try to install a new, consistent  version before giving up. Though it
    /// should be extremely rare, the catch up may fail if the leader is mutating the LSM at a
    /// very high rate and the follower is unable to get a consistent view. Default to 10
    /// attempts
    pub fn set_follower_catchup_retry_count(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_follower_catchup_retry_count(self.inner, val);
        }
    }

    /// Returns the value of the `follower_catchup_retry_count` option.
    pub fn get_follower_catchup_retry_count(&self) -> u64 {
        unsafe { ffi::rocksdb_options_get_follower_catchup_retry_count(self.inner) }
    }

    /// Time to wait between consecutive catch up attempts Default 100ms
    pub fn set_follower_catchup_retry_wait_ms(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_follower_catchup_retry_wait_ms(self.inner, val);
        }
    }

    /// Returns the value of the `follower_catchup_retry_wait_ms` option.
    pub fn get_follower_catchup_retry_wait_ms(&self) -> u64 {
        unsafe { ffi::rocksdb_options_get_follower_catchup_retry_wait_ms(self.inner) }
    }

    /// When a RocksDB database is opened in follower mode, this option is set by the user to
    /// request the frequency of the follower attempting to refresh its view of the leader.
    /// RocksDB may choose to trigger catch ups more frequently if it detects any changes in
    /// the database state. Default every 10s.
    pub fn set_follower_refresh_catchup_period_ms(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_follower_refresh_catchup_period_ms(self.inner, val);
        }
    }

    /// Returns the value of the `follower_refresh_catchup_period_ms` option.
    pub fn get_follower_refresh_catchup_period_ms(&self) -> u64 {
        unsafe { ffi::rocksdb_options_get_follower_refresh_catchup_period_ms(self.inner) }
    }

    /// In debug mode, RocksDB runs consistency checks on the LSM every time the LSM changes
    /// (Flush, Compaction, AddFile). When this option is true, these checks are also enabled
    /// in release mode. These checks were historically disabled in release mode, but are now
    /// enabled by default for proactive corruption detection. The CPU overhead is negligible
    /// for normal mixed operations but can slow down saturated writing. See
    /// Options::DisableExtraChecks(). Default: true
    pub fn set_force_consistency_checks(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_force_consistency_checks(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `force_consistency_checks` option.
    pub fn get_force_consistency_checks(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_force_consistency_checks(self.inner) != 0 }
    }

    /// EXPERIMENTAL If this option is set, when creating the last level files, pass this
    /// temperature to FileSystem used. Should be no-op for default FileSystem and users need
    /// to plug in their own FileSystem to take advantage of it. Currently only compatible
    /// with universal compaction.
    ///
    /// Dynamically changeable through the SetOptions() API
    pub fn set_last_level_temperature(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_options_set_last_level_temperature(self.inner, val);
        }
    }

    /// Returns the value of the `last_level_temperature` option.
    pub fn get_last_level_temperature(&self) -> c_int {
        unsafe { ffi::rocksdb_options_get_last_level_temperature(self.inner) }
    }

    /// The number of bytes to prefetch when reading the DB manifest and WAL files during
    /// DB::Open (and variants). This is mostly useful for reading a remotely located log, as
    /// it can save the number of round-trips. If 0, then the prefetching is disabled.
    ///
    /// Default: 0
    pub fn set_log_readahead_size(&mut self, val: usize) {
        unsafe {
            ffi::rocksdb_options_set_log_readahead_size(self.inner, val);
        }
    }

    /// Returns the value of the `log_readahead_size` option.
    pub fn get_log_readahead_size(&self) -> usize {
        unsafe { ffi::rocksdb_options_get_log_readahead_size(self.inner) }
    }

    /// It indicates, which lowest cache tier we want to use for a certain DB. Currently we
    /// support volatile_tier and non_volatile_tier. They are layered. By setting it to
    /// kVolatileTier, only the block cache (current implemented volatile_tier) is used. So
    /// cache entries will not spill to secondary cache (current implemented
    /// non_volatile_tier), and block cache lookup misses will not lookup in the secondary
    /// cache. When kNonVolatileBlockTier is used, we use both block cache and secondary
    /// cache.
    ///
    /// Default: kNonVolatileBlockTier
    pub fn set_lowest_used_cache_tier(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_options_set_lowest_used_cache_tier(self.inner, val);
        }
    }

    /// Returns the value of the `lowest_used_cache_tier` option.
    pub fn get_lowest_used_cache_tier(&self) -> c_int {
        unsafe { ffi::rocksdb_options_get_lowest_used_cache_tier(self.inner) }
    }

    /// It defines how many times DB::Resume() is called by a separate thread when background
    /// retryable IO Error happens. When background retryable IO Error happens, SetBGError is
    /// called to deal with the error. If the error can be auto-recovered (e.g., retryable IO
    /// Error during Flush or WAL write), then db resume is called in background to recover
    /// from the error. If this value is 0 or negative, DB::Resume() will not be called
    /// automatically.
    ///
    /// Default: INT_MAX
    pub fn set_max_bgerror_resume_count(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_options_set_max_bgerror_resume_count(self.inner, val);
        }
    }

    /// Returns the value of the `max_bgerror_resume_count` option.
    pub fn get_max_bgerror_resume_count(&self) -> c_int {
        unsafe { ffi::rocksdb_options_get_max_bgerror_resume_count(self.inner) }
    }

    /// Maximum interval in seconds between periodic compaction trigger checks. The periodic
    /// trigger re-evaluates compaction scores for all column families, which is necessary for
    /// features like read-triggered compaction and time-based compaction to work on a "quiet"
    /// DB with no writes.
    ///
    /// This is an upper bound: the actual check interval may be reduced to align with
    /// stats_dump_period_sec, stats_persist_period_sec, or per-CF time-based compaction
    /// intervals (periodic_compaction_seconds, ttl, etc.).
    ///
    /// Note: this option controls how often RocksDB *checks* whether compaction is needed. It
    /// is different from the CF option `periodic_compaction_seconds` which controls the *age
    /// threshold* at which SST files become eligible for periodic compaction.
    ///
    /// The minimum effective period is 1 second (values below 1 are clamped to 1). Setting
    /// this to 0 results in the most aggressive 1-second polling.
    ///
    /// Default: 43200 (12 hours)
    ///
    /// Dynamically changeable through SetDBOptions() API.
    pub fn set_max_compaction_trigger_wakeup_seconds(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_max_compaction_trigger_wakeup_seconds(self.inner, val);
        }
    }

    /// Returns the value of the `max_compaction_trigger_wakeup_seconds` option.
    pub fn get_max_compaction_trigger_wakeup_seconds(&self) -> u64 {
        unsafe { ffi::rocksdb_options_get_max_compaction_trigger_wakeup_seconds(self.inner) }
    }

    /// This option mostly replaces max_manifest_file_size to control an auto-tuned balance of
    /// manifest write amplification and space amplification. A new manifest file is created
    /// with the "compacted" contents of the old one when current_manifest_size >
    /// max(max_manifest_file_size, est_compacted_manifest_size * (1 +
    /// max_manifest_space_amp_pct/100))
    ///
    /// where est_compacted_manifest_size is an estimate of how big a new compacted version of
    /// the current manifest would be. Currently, the estimate used is the last newly-written
    /// manifest, in its "compacted" form.
    ///
    /// Space amplification in the manifest file might be less of a concern for primary
    /// storage space and more of a concern for DB recover time and size of backup files that
    /// aren't incremental between backups. To minimize manifest churn on initial DB
    /// population, setting max_manifest_file_size to something not too small, like 1MB,
    /// should suffice. Similarly, write amp on the manifest file is likely not a direct
    /// concern but completed compactions and flushes cannot (currently) be committed while
    /// the (relatively small) manifest file is being compacted. Manifest compactions should
    /// not interfere with user write latency or throughput unless the DB is chronically
    /// stalling or close to stalling writes already.
    ///
    /// For this option to have a meaningful effect, it is recommended to set
    /// max_manifest_file_size to something modest like 1MB. Then we can interpret values for
    /// this option as follows, starting with minimum space amp and maximum write amp:
    /// - 0 - Every manifest write (flush, compaction, etc.) generates a whole new manifest.
    ///   Only useful for testing.
    /// - very small - Doesn't take many manifest writes to generate a whole new manifest.
    /// - 100 - In a DB with pretty consistent number of SST files, etc., achieves about 1.0
    ///   write amp (writing about 2x the theoretical minimum) and a max of about 1.0 space
    ///   amp (manifest up to 2x the compacted size).
    /// - 500 - Recommended and default: 0.2 write amp and up to roughly 5.0 space amp.
    /// - 10000 - 0.01 write amp and up to 100 space amp on the manifest.
    ///
    /// This option is mutable with SetDBOptions(), taking effect on the next manifest write
    /// (e.g. completed DB compaction or flush).
    pub fn set_max_manifest_space_amp_pct(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_options_set_max_manifest_space_amp_pct(self.inner, val);
        }
    }

    /// Returns the value of the `max_manifest_space_amp_pct` option.
    pub fn get_max_manifest_space_amp_pct(&self) -> c_int {
        unsafe { ffi::rocksdb_options_get_max_manifest_space_amp_pct(self.inner) }
    }

    /// The maximum limit of number of bytes that are written in a single batch of WAL or
    /// memtable write. It is followed when the leader write size is larger than 1/8 of this
    /// limit.
    ///
    /// Default: 1 MB
    pub fn set_max_write_batch_group_size_bytes(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_max_write_batch_group_size_bytes(self.inner, val);
        }
    }

    /// Returns the value of the `max_write_batch_group_size_bytes` option.
    pub fn get_max_write_batch_group_size_bytes(&self) -> u64 {
        unsafe { ffi::rocksdb_options_get_max_write_batch_group_size_bytes(self.inner) }
    }

    /// RocksDB will try to flush the current memtable after the number of range deletions is
    /// \>= this limit. For workloads with many range deletions, limiting the number of range
    /// deletions in memtable can help prevent performance degradation and/or OOM caused by
    /// too many range tombstones in a single memtable.
    ///
    /// Default: 0 (disabled)
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_memtable_max_range_deletions(&mut self, val: u32) {
        unsafe {
            ffi::rocksdb_options_set_memtable_max_range_deletions(self.inner, val);
        }
    }

    /// Returns the value of the `memtable_max_range_deletions` option.
    pub fn get_memtable_max_range_deletions(&self) -> u32 {
        unsafe { ffi::rocksdb_options_get_memtable_max_range_deletions(self.inner) }
    }

    /// Enable memtable per key-value checksum protection.
    ///
    /// Each entry in memtable will be suffixed by a per key-value checksum. This options
    /// determines the size of such checksums.
    ///
    /// It is suggested to turn on write batch per key-value checksum protection together with
    /// this option, so that the checksum computation is done outside of writer threads
    /// (memtable kv checksum can be computed from write batch checksum) See
    /// WriteOptions::protection_bytes_per_key for more detail.
    ///
    /// Default: 0 (no protection) Supported values: 0, 1, 2, 4, 8. Dynamically changeable
    /// through the SetOptions() API.
    pub fn set_memtable_protection_bytes_per_key(&mut self, val: u32) {
        unsafe {
            ffi::rocksdb_options_set_memtable_protection_bytes_per_key(self.inner, val);
        }
    }

    /// Returns the value of the `memtable_protection_bytes_per_key` option.
    pub fn get_memtable_protection_bytes_per_key(&self) -> u32 {
        unsafe { ffi::rocksdb_options_get_memtable_protection_bytes_per_key(self.inner) }
    }

    /// Enables additional integrity checks during seek. Specifically, for skiplist-based
    /// memtables, key checksum validation could be enabled during seek optionally. This is
    /// helpful to detect corrupted memtable keys during reads. Enabling this feature incurs a
    /// performance overhead due to additional key checksum validation during memtable seek
    /// operation. This option depends on memtable_protection_bytes_per_key to be non zero. If
    /// memtable_protection_bytes_per_key is zero, no validation is performed.
    pub fn set_memtable_verify_per_key_checksum_on_seek(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_memtable_verify_per_key_checksum_on_seek(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `memtable_verify_per_key_checksum_on_seek` option.
    pub fn get_memtable_verify_per_key_checksum_on_seek(&self) -> bool {
        unsafe {
            ffi::rocksdb_options_get_memtable_verify_per_key_checksum_on_seek(self.inner) != 0
        }
    }

    /// Enable whole key bloom filter in memtable. Note this will only take effect if
    /// memtable_prefix_bloom_size_ratio is not 0. Enabling whole key filtering can
    /// potentially reduce CPU usage for point-look-ups.
    ///
    /// Default: false (disabled)
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn get_memtable_whole_key_filtering(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_memtable_whole_key_filtering(self.inner) != 0 }
    }

    /// When DB files other than SST, blob and WAL files are created, use this filesystem
    /// temperature. (See also `wal_write_temperature` and various `*_temperature` CF
    /// options.) When not `kUnknown`, this overrides any temperature set by
    /// OptimizeForManifestWrite functions.
    pub fn set_metadata_write_temperature(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_options_set_metadata_write_temperature(self.inner, val);
        }
    }

    /// Returns the value of the `metadata_write_temperature` option.
    pub fn get_metadata_write_temperature(&self) -> c_int {
        unsafe { ffi::rocksdb_options_get_metadata_write_temperature(self.inner) }
    }

    /// EXPERIMENTAL
    ///
    /// During forward or reverse iteration, when this many or more strictly contiguous point
    /// tombstones (kTypeDeletion, kTypeDeletionWithTimestamp, kTypeSingleDeletion) are
    /// encountered with no live keys between them, a range tombstone [first_tombstone_key,
    /// next_live_key) is inserted into the current mutable memtable (only if memtable is not
    /// empty). This is a logically redundant entry that does not change any data, but
    /// optimizes future iterators by potentially skipping a large number of tombstone scans.
    ///
    /// This optimization is best-effort and is currently disabled for iterator configurations
    /// that may not expose all interior live keys, including:
    /// - user-defined timestamp reads without full visibility (for example,
    ///   ReadOptions::iter_start_ts or a non-max ReadOptions::timestamp)
    /// - prefix extractor reads that are neither total-order (ReadOptions::total_order_seek
    ///   / ReadOptions::auto_prefix_mode) nor bounded by ReadOptions::prefix_same_as_start
    ///
    /// Even if the above restrictions are met, there are still scenarios where a converted
    /// range tombstone may be discarded:
    /// - The snapshot's active mutable memtable has already become immutable.
    /// - The iterator's snapshot seq is below the active memtable's earliest sequence
    ///   number.
    /// - A range tombstone covering [first_tombstone_key, next_live_key) is already present
    ///   in the memtable.
    /// - A WritePrepared/WriteUnprepared transaction read callback is in use and the
    ///   snapshot seq is at or above its min uncommitted seq.
    /// - An IngestExternalFile call is currently in flight on this column family OR the
    ///   inserted range tombstone seqno would be lower than the ingested file seqno.
    ///
    /// Read-write iterators using ReadOptions::table_filter are rejected while this option is
    /// enabled, see more details in ReadOptions::table_filter comments.
    ///
    /// Set to 0 to disable.
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_min_tombstones_for_range_conversion(&mut self, val: u32) {
        unsafe {
            ffi::rocksdb_options_set_min_tombstones_for_range_conversion(self.inner, val);
        }
    }

    /// Returns the value of the `min_tombstones_for_range_conversion` option.
    pub fn get_min_tombstones_for_range_conversion(&self) -> u32 {
        unsafe { ffi::rocksdb_options_get_min_tombstones_for_range_conversion(self.inner) }
    }

    /// EXPERIMENTAL: If true, RocksDB can reduce recovery work after a clean shutdown, which
    /// may reduce DB::Open latency on warm reopens, especially on storage where metadata
    /// appends are expensive.
    ///
    /// Best-effort optimization: if it is disabled or unavailable, RocksDB falls back to the
    /// standard recovery path.
    ///
    /// Temporary rollout / kill switch for an optimization that is intended to be correct and
    /// eventually always enabled. Mutable via SetDBOptions().
    pub fn set_optimize_manifest_for_recovery(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_optimize_manifest_for_recovery(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `optimize_manifest_for_recovery` option.
    pub fn get_optimize_manifest_for_recovery(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_optimize_manifest_for_recovery(self.inner) != 0 }
    }

    /// After writing every SST file, reopen it and read all the keys. Checks the hash of all
    /// of the keys and values written versus the keys in the file and signals a corruption if
    /// they do not match
    ///
    /// Default: false
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_paranoid_file_checks(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_paranoid_file_checks(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `paranoid_file_checks` option.
    pub fn get_paranoid_file_checks(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_paranoid_file_checks(self.inner) != 0 }
    }

    /// Enables additional integrity checks during reads/scans. Specifically, for
    /// skiplist-based memtables, key ordering validation could be enabled optionally. This is
    /// helpful to detect corrupted memtable keys during reads. Enabling this feature incurs a
    /// performance overhead due to additional comparison during memtable lookup.
    pub fn set_paranoid_memory_checks(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_paranoid_memory_checks(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `paranoid_memory_checks` option.
    pub fn get_paranoid_memory_checks(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_paranoid_memory_checks(self.inner) != 0 }
    }

    /// If true, automatically persist stats to a hidden column family (column family name:
    /// ___rocksdb_stats_history___) every stats_persist_period_sec seconds; otherwise, write
    /// to an in-memory struct. User can query through `GetStatsHistory` API. If user attempts
    /// to create a column family with the same name on a DB which have previously set
    /// persist_stats_to_disk to true, the column family creation will fail, but the hidden
    /// column family will survive, as well as the previously persisted statistics. When
    /// peristing stats to disk, the stat name will be limited at 100 bytes. Default: false
    pub fn set_persist_stats_to_disk(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_persist_stats_to_disk(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `persist_stats_to_disk` option.
    pub fn get_persist_stats_to_disk(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_persist_stats_to_disk(self.inner) != 0 }
    }

    /// UNDER CONSTRUCTION -- DO NOT USE When the user-defined timestamp feature is enabled,
    /// this flag controls whether the user-defined timestamps will be persisted.
    ///
    /// When it's false, the user-defined timestamps will be removed from the user keys when
    /// data is flushed from memtables to SST files. Other places that user keys can be
    /// persisted like file boundaries in file metadata and blob files go through a similar
    /// process. There are two major motivations for this flag:
    /// - backward compatibility: if the user later decides to disable the user-defined
    ///   timestamp feature for the column family, these SST files can be handled by a user
    ///   comparator that is not aware of user-defined timestamps.
    /// - enable user-defined timestamp feature for an existing column family while set this
    ///   flag to be `false`: user keys in the newly generated SST files are of the same
    ///   format as the existing SST files.
    ///
    /// Currently only user comparator that formats user-defined timesamps as uint64_t via
    /// using one of the RocksDB provided comparator `ComparatorWithU64TsImpl` are supported.
    ///
    /// When setting this flag to `false`, users should also call
    /// `DB::IncreaseFullHistoryTsLow` to set a cutoff timestamp for flush. RocksDB refrains
    /// from flushing a memtable with data still above the cutoff timestamp with best effort.
    /// One limitation of this best effort is that when `max_write_buffer_number` is equal to
    /// or smaller than 2, RocksDB will not attempt to retain user-defined timestamps, all
    /// flush jobs continue normally.
    ///
    /// Users can do user-defined multi-versioned read above the cutoff timestamp. When users
    /// try to read below the cutoff timestamp, an error will be returned.
    ///
    /// Note that if WAL is enabled, unlike SST files, user-defined timestamps are persisted
    /// to WAL even if this flag is set to `false`. The benefit of this is that user-defined
    /// timestamps can be recovered with the caveat that users should flush all memtables so
    /// there is no active WAL files before doing a downgrade. In order to use WAL to recover
    /// user-defined timestamps, users of this feature would want to set both
    /// `avoid_flush_during_shutdown` and `avoid_flush_during_recovery` to be true.
    ///
    /// Note that setting this flag to false is not supported in combination with atomic
    /// flush, or concurrent memtable write enabled by `allow_concurrent_memtable_write`.
    ///
    /// Default: true (user-defined timestamps are persisted) Not dynamically changeable,
    /// change it requires db restart and only compatible changes are allowed.
    pub fn set_persist_user_defined_timestamps(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_persist_user_defined_timestamps(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `persist_user_defined_timestamps` option.
    pub fn get_persist_user_defined_timestamps(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_persist_user_defined_timestamps(self.inner) != 0 }
    }

    /// EXPERIMENTAL The feature is still in development and is incomplete. If this option is
    /// set, when data insert time is within this time range, it will be precluded from the
    /// last level. 0 means no key will be precluded from the last level.
    ///
    /// Note: when enabled, universal size amplification (controlled by option
    /// `compaction_options_universal.max_size_amplification_percent`) calculation will
    /// exclude the last level. As the feature is designed for tiered storage and a typical
    /// setting is the last level is cold tier which is likely not size constrained, the size
    /// amp is going to be only for non-last levels.
    ///
    /// Default: 0 (disable the feature)
    ///
    /// Dynamically changeable through the SetOptions() API
    pub fn set_preclude_last_level_data_seconds(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_preclude_last_level_data_seconds(self.inner, val);
        }
    }

    /// Returns the value of the `preclude_last_level_data_seconds` option.
    pub fn get_preclude_last_level_data_seconds(&self) -> u64 {
        unsafe { ffi::rocksdb_options_get_preclude_last_level_data_seconds(self.inner) }
    }

    /// Historically, when prefix_extractor != nullptr, iterators have an unfortunate default
    /// semantics of *possibly* only returning data within the same prefix. To avoid "spooky
    /// action at a distance," iterator bounds should come from the instantiation or seeking
    /// of the iterator, not from a mutable column family option.
    ///
    /// When set to true, it is as if every iterator is created with total_order_seek=true and
    /// only auto_prefix_mode=true and prefix_same_as_start=true can take advantage of prefix
    /// seek optimizations.
    pub fn set_prefix_seek_opt_in_only(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_prefix_seek_opt_in_only(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `prefix_seek_opt_in_only` option.
    pub fn get_prefix_seek_opt_in_only(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_prefix_seek_opt_in_only(self.inner) != 0 }
    }

    /// EXPERIMENTAL If this option is set, it will preserve the internal time information
    /// about the data until it's older than the specified time here. Internally the time
    /// information is a map between sequence number and time, which is the same as
    /// `preclude_last_level_data_seconds`. But it won't preclude the data from the last level
    /// and the data in the last level won't have the sequence number zeroed out. Internally,
    /// rocksdb would sample the sequence number to time pair and store that in SST property
    /// "rocksdb.seqno.time.map". The information is currently only used for tiered storage
    /// compaction (option `preclude_last_level_data_seconds`).
    ///
    /// Note: if both `preclude_last_level_data_seconds` and this option is set, it will
    /// preserve the max time of the 2 options and compaction still preclude the data based on
    /// `preclude_last_level_data_seconds`. The higher the preserve_time is, the less the
    /// sampling frequency will be ( which means less accuracy of the time estimation).
    ///
    /// Default: 0 (disable the feature)
    ///
    /// Dynamically changeable through the SetOptions() API
    pub fn set_preserve_internal_time_seconds(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_preserve_internal_time_seconds(self.inner, val);
        }
    }

    /// Returns the value of the `preserve_internal_time_seconds` option.
    pub fn get_preserve_internal_time_seconds(&self) -> u64 {
        unsafe { ffi::rocksdb_options_get_preserve_internal_time_seconds(self.inner) }
    }

    /// Requested maximum number of threads in the shared read I/O executor. A DB open can
    /// increase the executor to this size but cannot reduce it. Used exclusively for
    /// asynchronous read requests (e.g. GetAsync, MultiGetAsync).
    pub fn set_read_io_executor_threads(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_options_set_read_io_executor_threads(self.inner, val);
        }
    }

    /// Returns the value of the `read_io_executor_threads` option.
    pub fn get_read_io_executor_threads(&self) -> c_int {
        unsafe { ffi::rocksdb_options_get_read_io_executor_threads(self.inner) }
    }

    /// When set to a positive value, enables read-triggered compaction. An SST file is marked
    /// for compaction when its estimated read frequency (estimated_reads / file_size) exceeds
    /// this threshold. This helps reduce read amplification for hot keys by compacting
    /// frequently-read files.
    ///
    /// Only "collapsible" reads are counted -- lookups that return NotFound (bloom filter
    /// false positive), Delete/SingleDeletion (tombstone), or Merge (partial result). These
    /// are reads where the file contributed no final value and compaction would eliminate the
    /// wasted work.
    ///
    /// Choosing a value: the threshold balances read IO saved against the write amplification
    /// (WA) of an extra compaction. This assumes the block-based table format is being used,
    ///
    /// Break-even derivation (no block cache): Let r = estimated_reads / file_size  (the
    /// threshold) S = file_size B = block_size             (typically 4 KB) F = level fanout
    /// (typically ~10)
    ///
    /// Each collapsible read wastes one data-block read = B bytes of IO. Total wasted read IO
    /// for a file = r * S * B.
    ///
    /// Compaction cost: one level-L file overlaps ~F files in level L+1, so we read (1 + F)
    /// files and write (1 + F) files. Total compaction IO = 2 * (1 + F) * S.
    ///
    /// Break-even when wasted read IO equals compaction IO: r * S * B = 2 * (1 + F) * S r = 2
    /// * (1 + F) / B
    ///
    /// With F = 10, B = 4096:  r = 22 / 4096 ~= 0.005.
    ///
    /// With a block-cache hit rate h (0 <= h < 1), each collapsible read only costs (1 - h) *
    /// B bytes of actual disk IO, so: r = 2 * (1 + F) / ((1 - h) * B)
    ///
    /// h = 0   -> r ~= 0.005 h = 0.5 -> r ~= 0.01 h = 0.9 -> r ~= 0.05
    ///
    /// A recommended starting point is 0.01, which avoids triggering compactions that cost
    /// more IO than they save for most cache-friendly workloads, while still being responsive
    /// enough to compact files with significant wasted reads.
    ///
    /// For this feature to take effect on a "quiet" DB (no writes), the DB-level option
    /// `max_compaction_trigger_wakeup_seconds` must also be set to a non-zero value so the
    /// periodic background job can re-evaluate files.
    ///
    /// Valid range: >= 0.0 (must be finite). Use 0.0 to disable.
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_read_triggered_compaction_threshold(&mut self, val: f64) {
        unsafe {
            ffi::rocksdb_options_set_read_triggered_compaction_threshold(self.inner, val);
        }
    }

    /// Returns the value of the `read_triggered_compaction_threshold` option.
    pub fn get_read_triggered_compaction_threshold(&self) -> f64 {
        unsafe { ffi::rocksdb_options_get_read_triggered_compaction_threshold(self.inner) }
    }

    /// Sets the `remove` option.
    pub fn set_remove(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_options_calculate_sst_write_lifetime_hint_set_remove(self.inner, val);
        }
    }

    /// EXPERIMENTAL: If true, DB::Open can try to reuse the existing MANIFEST for the first
    /// post-open metadata update instead of creating a fresh one. This can reduce warm-open
    /// latency for DBs whose MANIFEST is expensive to rebuild.
    ///
    /// Best-effort optimization: even when enabled, RocksDB may still create a fresh MANIFEST
    /// if the FileSystem does not support reopening the existing MANIFEST for append, or if
    /// RocksDB decides reuse is unsafe. That fallback is normal behavior.
    ///
    /// With very small `max_manifest_file_size` settings, the reused MANIFEST can still
    /// rotate earlier than expected after open, because RocksDB may keep a conservative
    /// auto-tuned rotation threshold until it later refreshes its compacted-size estimate.
    ///
    /// Temporary rollout / kill switch while this optimization is being validated.
    pub fn set_reuse_manifest_on_open(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_reuse_manifest_on_open(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `reuse_manifest_on_open` option.
    pub fn get_reuse_manifest_on_open(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_reuse_manifest_on_open(self.inner) != 0 }
    }

    /// If this option is set then 1 in N blocks are compressed using a fast (lz4) and slow
    /// (zstd) compression algorithm. The compressibility is reported as stats and the stored
    /// data is left uncompressed (unless compression is also requested).
    pub fn set_sample_for_compression(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_sample_for_compression(self.inner, val);
        }
    }

    /// Returns the value of the `sample_for_compression` option.
    pub fn get_sample_for_compression(&self) -> u64 {
        unsafe { ffi::rocksdb_options_get_sample_for_compression(self.inner) }
    }

    /// if not zero, periodically take stats snapshots and store in memory, the memory size
    /// for stats snapshots is capped at stats_history_buffer_size Default: 1MB
    pub fn set_stats_history_buffer_size(&mut self, val: usize) {
        unsafe {
            ffi::rocksdb_options_set_stats_history_buffer_size(self.inner, val);
        }
    }

    /// Returns the value of the `stats_history_buffer_size` option.
    pub fn get_stats_history_buffer_size(&self) -> usize {
        unsafe { ffi::rocksdb_options_get_stats_history_buffer_size(self.inner) }
    }

    /// When true, guarantees WAL files have at most `wal_bytes_per_sync` bytes submitted for
    /// writeback at any given time, and SST files have at most `bytes_per_sync` bytes pending
    /// writeback at any given time. This can be used to handle cases where processing speed
    /// exceeds I/O speed during file generation, which can lead to a huge sync when the file
    /// is finished, even with `bytes_per_sync` / `wal_bytes_per_sync` properly configured.
    ///
    /// - If `sync_file_range` is supported it achieves this by waiting for any prior
    ///   `sync_file_range`s to finish before proceeding. In this way, processing
    ///   (compression, etc.) can proceed uninhibited in the gap between `sync_file_range`s,
    ///   and we block only when I/O falls behind.
    /// - Otherwise the `WritableFile::Sync` method is used. Note this mechanism always
    ///   blocks, thus preventing the interleaving of I/O and processing.
    ///
    /// Note: Enabling this option does not provide any additional persistence guarantees, as
    /// it may use `sync_file_range`, which does not write out metadata.
    ///
    /// Default: false
    pub fn set_strict_bytes_per_sync(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_strict_bytes_per_sync(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `strict_bytes_per_sync` option.
    pub fn get_strict_bytes_per_sync(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_strict_bytes_per_sync(self.inner) != 0 }
    }

    /// Whether to allow filesystem reads to stay under the `max_successive_merges` limit.
    /// When true, this can lead to merge writes blocking the write path waiting on filesystem
    /// reads.
    ///
    /// This option is temporary in case the recent change to disallow filesystem reads during
    /// merge writes has a problem and users need to undo it quickly.
    ///
    /// Default: false
    pub fn set_strict_max_successive_merges(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_strict_max_successive_merges(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `strict_max_successive_merges` option.
    pub fn get_strict_max_successive_merges(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_strict_max_successive_merges(self.inner) != 0 }
    }

    /// If true, RocksDB will consider the estimated tail size (filter + index + meta blocks)
    /// when deciding whether to cut a compaction output file. This helps prevent output files
    /// from exceeding the target_file_size_base due to large tail blocks. When disabled, only
    /// the data block size is considered, which may result in SST files exceeding the
    /// target_file_size_base.
    ///
    /// Default: false
    ///
    /// Dynamically changeable through SetOptions() API
    pub fn set_target_file_size_is_upper_bound(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_target_file_size_is_upper_bound(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `target_file_size_is_upper_bound` option.
    pub fn get_target_file_size_is_upper_bound(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_target_file_size_is_upper_bound(self.inner) != 0 }
    }

    /// EXPERIMENTAL
    ///
    /// If true, each new WAL will record various information about its predecessor WAL for
    /// verification on the predecessor WAL during WAL recovery.
    ///
    /// It verifies the following:
    /// - There exists at least some WAL in the DB
    /// - It's not compatible with `RepairDB()` since this option imposes a stricter
    ///   requirement on WAL than the DB went through `RepariDB()` can normally meet
    /// - There exists no WAL hole where new WAL data presents while some old WAL data not
    ///   yet obsolete is missing. The DB manifest indicates which WALs are obsolete.
    ///
    /// This is intended to be a better replacement to `track_and_verify_wals_in_manifest`.
    ///
    /// Default: false
    pub fn set_track_and_verify_wals(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_track_and_verify_wals(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `track_and_verify_wals` option.
    pub fn get_track_and_verify_wals(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_track_and_verify_wals(self.inner) != 0 }
    }

    /// If enabled it uses two queues for writes, one for the ones with disable_memtable and
    /// one for the ones that also write to memtable. This allows the memtable writes not to
    /// lag behind other writes. It can be used to optimize MySQL 2PC in which only the
    /// commits, which are serial, write to memtable.
    pub fn set_two_write_queues(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_two_write_queues(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `two_write_queues` option.
    pub fn get_two_write_queues(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_two_write_queues(self.inner) != 0 }
    }

    /// EXPERIMENTAL When > 0, RocksDB attempts to erase some block cache entries for files
    /// that have become obsolete, which means they are about to be deleted. To avoid
    /// excessive tracking, this "uncaching" process is iterative and speculative, meaning it
    /// could incur extra background CPU effort if the file's blocks are generally not cached.
    /// A larger number indicates more willingness to spend CPU time to maximize block cache
    /// hit rates by erasing known-obsolete entries.
    ///
    /// When uncache_aggressiveness=1, block cache entries for an obsolete file are only
    /// erased until any attempted erase operation fails because the block is not cached. Then
    /// no further attempts are made to erase cached blocks for that file.
    ///
    /// For larger values, erasure is attempted until evidence incidates that the chance of
    /// success is < 0.99^(a-1), where a = uncache_aggressiveness. For example: 2 -> Attempt
    /// only while expecting >= 99% successful/useful erasure 11 -> 90% 69 -> 50% 110 -> 33%
    /// 230 -> 10% 460 -> 1% 690 -> 0.1% 1000 -> 1 in 23000 10000 -> Always (for all practical
    /// purposes) NOTE: UINT32_MAX and nearby values could take additional special meanings in
    /// the future.
    ///
    /// Pinned cache entries (guaranteed present) are always erased if uncache_aggressiveness
    /// \> 0, but are not used in predicting the chances of successful erasure of non-pinned
    /// entries.
    ///
    /// NOTE: In the case of copied DBs (such as Checkpoints) sharing a block cache, it is
    /// possible that a file becoming obsolete doesn't mean its block cache entries (shared
    /// among copies) are obsolete. Such a scenerio is the best case for
    /// uncache_aggressiveness = 0.
    ///
    /// When using allow_mmap_reads=true, this option is ignored (no un-caching).
    ///
    /// Once validated in production, the default will likely change to something around 300.
    pub fn set_uncache_aggressiveness(&mut self, val: u32) {
        unsafe {
            ffi::rocksdb_options_set_uncache_aggressiveness(self.inner, val);
        }
    }

    /// Returns the value of the `uncache_aggressiveness` option.
    pub fn get_uncache_aggressiveness(&self) -> u32 {
        unsafe { ffi::rocksdb_options_get_uncache_aggressiveness(self.inner) }
    }

    /// Use O_DIRECT for compaction-input SST reads only, leaving user reads buffered. Useful
    /// when sequential compaction reads would otherwise evict the hot user-read working set
    /// from the OS page cache. When this is true and use_direct_reads is false, compaction
    /// opens short-lived O_DIRECT readers for its input files instead of reusing the buffered
    /// readers cached for user reads. This is the read-side analogue of
    /// use_direct_io_for_flush_and_compaction, and the two are often paired on write-heavy
    /// workloads.
    ///
    /// Scope and limits:
    /// - DBOption scope (applies to all column families); no per-CF setting.
    /// - Covers compaction inputs only. Blob-file reads and compaction-output verification
    ///   (paranoid_file_checks) still use the buffered path.
    /// - The ephemeral readers bypass the TableCache and are not counted against
    ///   max_open_files. Non-L0 levels keep one reader open at a time; L0 opens all of a
    ///   subcompaction's overlapping inputs at once, so with large L0 fan-in and many
    ///   subcompactions, watch RLIMIT_NOFILE.
    /// - Every input file is reopened per compaction, so NO_FILE_OPENS and
    ///   TABLE_OPEN_IO_MICROS rise while this is enabled.
    ///
    /// The same SST can be open through both a buffered handle (user reads) and an O_DIRECT
    /// handle (the compaction scan) at once; modern Linux handles this fine. The flag is
    /// neutral or slightly negative for in-memory DBs or uniform random reads, so measure
    /// before enabling.
    ///
    /// Has no effect when use_direct_reads is true (all reads are already O_DIRECT). Rejected
    /// at DB::Open when allow_mmap_reads is set.
    ///
    /// On a filesystem without O_DIRECT support (e.g. tmpfs), DB::Open fails: it probes by
    /// opening the MANIFEST with O_DIRECT. The probe only checks the filesystem holding the
    /// DB directory, so if SST files live elsewhere (via db_paths/cf_paths) without O_DIRECT,
    /// Open succeeds and the first compaction fails instead.
    ///
    /// Default: false
    pub fn set_use_direct_io_for_compaction_reads(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_use_direct_io_for_compaction_reads(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `use_direct_io_for_compaction_reads` option.
    pub fn get_use_direct_io_for_compaction_reads(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_use_direct_io_for_compaction_reads(self.inner) != 0 }
    }

    /// If true, on DB close, read back the entire MANIFEST file and validate CRC checksums
    /// and logical record content. If corruption is detected, a fresh MANIFEST is written
    /// from in-memory state before closing.
    ///
    /// This option is mutable with SetDBOptions().
    pub fn set_verify_manifest_content_on_close(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_verify_manifest_content_on_close(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `verify_manifest_content_on_close` option.
    pub fn get_verify_manifest_content_on_close(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_verify_manifest_content_on_close(self.inner) != 0 }
    }

    /// Bitmask enum for output verification option.
    ///
    /// Default: 0 (kVerifyNone)
    ///
    /// Dynamically changeable (as a uint32_t) through SetOptions() API.
    pub fn set_verify_output_flags(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_options_set_verify_output_flags(self.inner, val);
        }
    }

    /// Returns the value of the `verify_output_flags` option.
    pub fn get_verify_output_flags(&self) -> c_int {
        unsafe { ffi::rocksdb_options_get_verify_output_flags(self.inner) }
    }

    /// If true, verifies the SST unique id between MANIFEST and actual file each time an SST
    /// file is opened. This check ensures an SST file is not overwritten or misplaced. A
    /// corruption error will be reported if mismatch detected, but only when MANIFEST tracks
    /// the unique id, which starts from RocksDB version 7.3. Although the tracked internal
    /// unique id is related to the one returned by GetUniqueIdFromTableProperties, that is
    /// subject to change. NOTE: verification is currently only done on SST files using
    /// block-based table format.
    ///
    /// Setting to false should only be needed in case of unexpected problems.
    ///
    /// Although an early version of this option opened all SST files for verification on
    /// DB::Open, that is no longer guaranteed. However, as documented in an above option, if
    /// max_open_files is -1, DB will open all files on DB::Open().
    ///
    /// Default: true
    pub fn set_verify_sst_unique_id_in_manifest(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_options_set_verify_sst_unique_id_in_manifest(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `verify_sst_unique_id_in_manifest` option.
    pub fn get_verify_sst_unique_id_in_manifest(&self) -> bool {
        unsafe { ffi::rocksdb_options_get_verify_sst_unique_id_in_manifest(self.inner) != 0 }
    }

    /// Use this filesystem temperature when creating WAL files. When not `kUnknown`, this
    /// overrides any temperature set by OptimizeForLogWrite functions.
    pub fn set_wal_write_temperature(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_options_set_wal_write_temperature(self.inner, val);
        }
    }

    /// Returns the value of the `wal_write_temperature` option.
    pub fn get_wal_write_temperature(&self) -> c_int {
        unsafe { ffi::rocksdb_options_get_wal_write_temperature(self.inner) }
    }

    /// The maximum number of microseconds that a write operation will use a yielding spin
    /// loop to coordinate with other write threads before blocking on a mutex.  (Assuming
    /// write_thread_slow_yield_usec is set properly) increasing this value is likely to
    /// increase RocksDB throughput at the expense of increased CPU usage.
    ///
    /// Default: 100
    pub fn set_write_thread_max_yield_usec(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_write_thread_max_yield_usec(self.inner, val);
        }
    }

    /// Returns the value of the `write_thread_max_yield_usec` option.
    pub fn get_write_thread_max_yield_usec(&self) -> u64 {
        unsafe { ffi::rocksdb_options_get_write_thread_max_yield_usec(self.inner) }
    }

    /// The latency in microseconds after which a std::this_thread::yield call (sched_yield on
    /// Linux) is considered to be a signal that other processes or threads would like to use
    /// the current core. Increasing this makes writer threads more likely to take CPU by
    /// spinning, which will show up as an increase in the number of involuntary context
    /// switches.
    ///
    /// Default: 3
    pub fn set_write_thread_slow_yield_usec(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_options_set_write_thread_slow_yield_usec(self.inner, val);
        }
    }

    /// Returns the value of the `write_thread_slow_yield_usec` option.
    pub fn get_write_thread_slow_yield_usec(&self) -> u64 {
        unsafe { ffi::rocksdb_options_get_write_thread_slow_yield_usec(self.inner) }
    }
}

impl Default for Options {
    fn default() -> Self {
        unsafe {
            let opts = ffi::rocksdb_options_create();
            assert!(!opts.is_null(), "Could not create RocksDB options");

            Self {
                inner: opts,
                outlive: OptionsMustOutliveDB::default(),
            }
        }
    }
}

impl FlushOptions {
    pub fn new() -> FlushOptions {
        FlushOptions::default()
    }

    /// Waits until the flush is done.
    ///
    /// Default: true
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::FlushOptions;
    ///
    /// let mut options = FlushOptions::default();
    /// options.set_wait(false);
    /// ```
    pub fn set_wait(&mut self, wait: bool) {
        unsafe {
            ffi::rocksdb_flushoptions_set_wait(self.inner, c_uchar::from(wait));
        }
    }

    /// If true, the flush would proceed immediately even it means writes will stall for the
    /// duration of the flush; if false the operation will wait until it's possible to do
    /// flush w/o causing stall or until required flush is performed by someone else
    /// (foreground call or background thread). Default: false
    pub fn set_allow_write_stall(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_flushoptions_set_allow_write_stall(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `allow_write_stall` option.
    pub fn get_allow_write_stall(&self) -> bool {
        unsafe { ffi::rocksdb_flushoptions_get_allow_write_stall(self.inner) != 0 }
    }

    /// If true, use atomic flush to flush all column families atomically, regardless of the
    /// DBOptions::atomic_flush setting. When used with DB::Flush() or internally via
    /// GetLiveFilesStorageInfo(), this forces all column families to be flushed in a single
    /// atomic operation. Default: false (uses DBOptions::atomic_flush setting).
    pub fn set_force_atomic_flush(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_flushoptions_set_force_atomic_flush(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `force_atomic_flush` option.
    pub fn get_force_atomic_flush(&self) -> bool {
        unsafe { ffi::rocksdb_flushoptions_get_force_atomic_flush(self.inner) != 0 }
    }

    /// If true (and `wait` is also true), Flush() will not return until the registered
    /// EventListener::OnFlushCompleted callbacks for the flushed memtables have finished
    /// running. By default (false), Flush(wait=true) may return as soon as the flush result
    /// is committed, which can be before (or while) the OnFlushCompleted callbacks execute on
    /// the background flush thread. Set this to true when the caller needs to observe the
    /// effects of its OnFlushCompleted listener(s) immediately after Flush() returns. Has no
    /// effect when `wait == false`. Default: false
    pub fn set_listener_wait(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_flushoptions_set_listener_wait(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `listener_wait` option.
    pub fn get_listener_wait(&self) -> bool {
        unsafe { ffi::rocksdb_flushoptions_get_listener_wait(self.inner) != 0 }
    }
}

impl Default for FlushOptions {
    fn default() -> Self {
        let flush_opts = unsafe { ffi::rocksdb_flushoptions_create() };
        assert!(
            !flush_opts.is_null(),
            "Could not create RocksDB flush options"
        );

        Self { inner: flush_opts }
    }
}

impl WriteOptions {
    pub fn new() -> WriteOptions {
        WriteOptions::default()
    }

    /// Sets the sync mode. If true, the write will be flushed
    /// from the operating system buffer cache before the write is considered complete.
    /// If this flag is true, writes will be slower.
    ///
    /// Default: false
    pub fn set_sync(&mut self, sync: bool) {
        unsafe {
            ffi::rocksdb_writeoptions_set_sync(self.inner, c_uchar::from(sync));
        }
    }

    /// Sets whether WAL should be active or not.
    /// If true, writes will not first go to the write ahead log,
    /// and the write may got lost after a crash.
    ///
    /// Default: false
    pub fn disable_wal(&mut self, disable: bool) {
        unsafe {
            ffi::rocksdb_writeoptions_disable_WAL(self.inner, c_int::from(disable));
        }
    }

    /// If true and if user is trying to write to column families that don't exist (they were dropped),
    /// ignore the write (don't return an error). If there are multiple writes in a WriteBatch,
    /// other writes will succeed.
    ///
    /// Default: false
    pub fn set_ignore_missing_column_families(&mut self, ignore: bool) {
        unsafe {
            ffi::rocksdb_writeoptions_set_ignore_missing_column_families(
                self.inner,
                c_uchar::from(ignore),
            );
        }
    }

    /// If true and we need to wait or sleep for the write request, fails
    /// immediately with Status::Incomplete().
    ///
    /// Default: false
    pub fn set_no_slowdown(&mut self, no_slowdown: bool) {
        unsafe {
            ffi::rocksdb_writeoptions_set_no_slowdown(self.inner, c_uchar::from(no_slowdown));
        }
    }

    /// If true, this write request is of lower priority if compaction is
    /// behind. In this case, no_slowdown = true, the request will be cancelled
    /// immediately with Status::Incomplete() returned. Otherwise, it will be
    /// slowed down. The slowdown value is determined by RocksDB to guarantee
    /// it introduces minimum impacts to high priority writes.
    ///
    /// Default: false
    pub fn set_low_pri(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_writeoptions_set_low_pri(self.inner, c_uchar::from(v));
        }
    }

    /// If true, writebatch will maintain the last insert positions of each
    /// memtable as hints in concurrent write. It can improve write performance
    /// in concurrent writes if keys in one writebatch are sequential. In
    /// non-concurrent writes (when concurrent_memtable_writes is false) this
    /// option will be ignored.
    ///
    /// Default: false
    pub fn set_memtable_insert_hint_per_batch(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_writeoptions_set_memtable_insert_hint_per_batch(
                self.inner,
                c_uchar::from(v),
            );
        }
    }

    /// EXPERIMENTAL
    pub fn set_io_activity(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_writeoptions_set_io_activity(self.inner, val);
        }
    }

    /// Returns the value of the `io_activity` option.
    pub fn get_io_activity(&self) -> c_int {
        unsafe { ffi::rocksdb_writeoptions_get_io_activity(self.inner) }
    }

    /// `protection_bytes_per_key` is the number of bytes used to store protection information
    /// for each key entry. Currently supported values are zero (disabled) and eight.
    ///
    /// Default: zero (disabled).
    pub fn set_protection_bytes_per_key(&mut self, val: usize) {
        unsafe {
            ffi::rocksdb_writeoptions_set_protection_bytes_per_key(self.inner, val);
        }
    }

    /// Returns the value of the `protection_bytes_per_key` option.
    pub fn get_protection_bytes_per_key(&self) -> usize {
        unsafe { ffi::rocksdb_writeoptions_get_protection_bytes_per_key(self.inner) }
    }

    /// For file reads associated with this option, charge the internal rate limiter (see
    /// `DBOptions::rate_limiter`) at the specified priority. The special value
    /// `Env::IO_TOTAL` disables charging the rate limiter.
    ///
    /// The rate limiting is bypassed no matter this option's value for file reads on plain
    /// tables (these can exist when `ColumnFamilyOptions::table_factory` is a
    /// `PlainTableFactory`) and cuckoo tables (these can exist when
    /// `ColumnFamilyOptions::table_factory` is a `CuckooTableFactory`).
    ///
    /// The bytes charged to rate limiter may not exactly match the file read bytes since
    /// there are some seemingly insignificant reads, like for file headers/footers, that we
    /// currently do not charge to rate limiter.
    pub fn set_rate_limiter_priority(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_writeoptions_set_rate_limiter_priority(self.inner, val);
        }
    }

    /// Returns the value of the `rate_limiter_priority` option.
    pub fn get_rate_limiter_priority(&self) -> c_int {
        unsafe { ffi::rocksdb_writeoptions_get_rate_limiter_priority(self.inner) }
    }
}

impl Default for WriteOptions {
    fn default() -> Self {
        let write_opts = unsafe { ffi::rocksdb_writeoptions_create() };
        assert!(
            !write_opts.is_null(),
            "Could not create RocksDB write options"
        );

        Self { inner: write_opts }
    }
}

impl LruCacheOptions {
    /// Capacity of the cache, in the same units as the `charge` of each entry.
    /// This is typically measured in bytes, but can be a different unit if using
    /// kDontChargeCacheMetadata.
    pub fn set_capacity(&mut self, cap: usize) {
        unsafe {
            ffi::rocksdb_lru_cache_options_set_capacity(self.inner, cap);
        }
    }

    /// Cache is sharded into 2^num_shard_bits shards, by hash of key.
    /// If < 0, a good default is chosen based on the capacity and the
    /// implementation. (Mutex-based implementations are much more reliant
    /// on many shards for parallel scalability.)
    pub fn set_num_shard_bits(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_lru_cache_options_set_num_shard_bits(self.inner, val);
        }
    }
}

impl Default for LruCacheOptions {
    fn default() -> Self {
        let inner = unsafe { ffi::rocksdb_lru_cache_options_create() };
        assert!(
            !inner.is_null(),
            "Could not create RocksDB LRU cache options"
        );

        Self { inner }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
#[repr(i32)]
pub enum ReadTier {
    /// Reads data in memtable, block cache, OS cache or storage.
    All = 0,
    /// Reads data in memtable or block cache.
    BlockCache,
    /// Reads persisted data. When WAL is disabled, this option will skip data in memtable.
    Persisted,
    /// Reads data in memtable. Used for memtable only iterators.
    Memtable,
}

impl ReadOptions {
    // TODO add snapshot setting here
    // TODO add snapshot wrapper structs with proper destructors;
    // that struct needs an "iterator" impl too.

    /// Specify whether the "data block"/"index block"/"filter block"
    /// read for this iteration should be cached in memory?
    /// Callers may wish to set this field to false for bulk scans.
    ///
    /// Default: true
    pub fn fill_cache(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_fill_cache(self.inner, c_uchar::from(v));
        }
    }

    /// Sets the snapshot which should be used for the read.
    /// The snapshot must belong to the DB that is being read and must
    /// not have been released.
    pub fn set_snapshot<D: DBAccess>(&mut self, snapshot: &SnapshotWithThreadMode<D>) {
        unsafe {
            ffi::rocksdb_readoptions_set_snapshot(self.inner, snapshot.inner);
        }
    }

    /// Sets the lower bound for an iterator.
    pub fn set_iterate_lower_bound<K: Into<Vec<u8>>>(&mut self, key: K) {
        self.set_lower_bound_impl(Some(key.into()));
    }

    /// Sets the upper bound for an iterator.
    /// The upper bound itself is not included on the iteration result.
    pub fn set_iterate_upper_bound<K: Into<Vec<u8>>>(&mut self, key: K) {
        self.set_upper_bound_impl(Some(key.into()));
    }

    /// Sets lower and upper bounds based on the provided range.  This is
    /// similar to setting lower and upper bounds separately except that it also
    /// allows either bound to be reset.
    ///
    /// The argument can be a regular Rust range, e.g. `lower..upper`.  However,
    /// since RocksDB upper bound is always excluded (i.e. range can never be
    /// fully closed) inclusive ranges (`lower..=upper` and `..=upper`) are not
    /// supported.  For example:
    ///
    /// ```
    /// let mut options = rust_rocksdb::ReadOptions::default();
    /// options.set_iterate_range("xy".as_bytes().."xz".as_bytes());
    /// ```
    ///
    /// In addition, [`crate::PrefixRange`] can be used to specify a range of
    /// keys with a given prefix.  In particular, the above example is
    /// equivalent to:
    ///
    /// ```
    /// let mut options = rust_rocksdb::ReadOptions::default();
    /// options.set_iterate_range(rust_rocksdb::PrefixRange("xy".as_bytes()));
    /// ```
    ///
    /// Note that setting range using this method is separate to using prefix
    /// iterators.  Prefix iterators use prefix extractor configured for
    /// a column family.  Setting bounds via [`crate::PrefixRange`] is more akin
    /// to using manual prefix.
    ///
    /// Using this method clears any previously set bounds.  In other words, the
    /// bounds can be reset by setting the range to `..` as in:
    ///
    /// ```
    /// let mut options = rust_rocksdb::ReadOptions::default();
    /// options.set_iterate_range(..);
    /// ```
    pub fn set_iterate_range(&mut self, range: impl crate::IterateBounds) {
        let (lower, upper) = range.into_bounds();
        self.set_lower_bound_impl(lower);
        self.set_upper_bound_impl(upper);
    }

    /// Equivalent to `set_iterate_range(PrefixRange(prefix))`, but writes into
    /// the already-allocated bound buffers instead of building two fresh
    /// `Vec<u8>`s and dropping the old ones.
    ///
    /// `set_iterate_range` has to allocate because `IterateBounds::into_bounds`
    /// hands back owned `Vec`s. That is fine for one-off configuration, but the
    /// hot prefix-probe path reuses a cached `ReadOptions` specifically to avoid
    /// per-call allocation, and then threw that away by reallocating both bounds
    /// on every call. Reusing the buffers makes the steady state allocation-free.
    pub(crate) fn set_prefix_range_in_place(&mut self, prefix: &[u8]) {
        // An empty prefix covers the full keyspace, i.e. no bounds at all.
        if prefix.is_empty() {
            self.set_lower_bound_impl(None);
            self.set_upper_bound_impl(None);
            return;
        }

        // Lower bound is the prefix itself. The buffer can be reallocated by
        // `extend_from_slice`, so the pointer has to be handed to RocksDB again
        // even when the bound was already set.
        let (ptr, len) = {
            let lower = self.iterate_lower_bound.get_or_insert_with(Vec::new);
            lower.clear();
            lower.extend_from_slice(prefix);
            (lower.as_ptr() as *const c_char, lower.len())
        };
        unsafe {
            ffi::rocksdb_readoptions_set_iterate_lower_bound(self.inner, ptr, len);
        }

        // Upper bound is the successor of the prefix: strip trailing 0xff bytes,
        // then increment the last remaining one. A prefix that is entirely 0xff
        // has no successor, so it is an unbounded scan. This mirrors
        // `iter_range::next_prefix`.
        let ffs = prefix
            .iter()
            .rev()
            .take_while(|&&byte| byte == u8::MAX)
            .count();
        let head = &prefix[..prefix.len() - ffs];
        if head.is_empty() {
            self.set_upper_bound_impl(None);
            return;
        }
        let (ptr, len) = {
            let upper = self.iterate_upper_bound.get_or_insert_with(Vec::new);
            upper.clear();
            upper.extend_from_slice(head);
            // `head` is non-empty and its last byte is not 0xff, so this cannot
            // overflow.
            *upper.last_mut().unwrap() += 1;
            (upper.as_ptr() as *const c_char, upper.len())
        };
        unsafe {
            ffi::rocksdb_readoptions_set_iterate_upper_bound(self.inner, ptr, len);
        }
    }

    fn set_lower_bound_impl(&mut self, bound: Option<Vec<u8>>) {
        let (ptr, len) = if let Some(ref bound) = bound {
            (bound.as_ptr() as *const c_char, bound.len())
        } else if self.iterate_lower_bound.is_some() {
            (std::ptr::null(), 0)
        } else {
            return;
        };
        self.iterate_lower_bound = bound;
        unsafe {
            ffi::rocksdb_readoptions_set_iterate_lower_bound(self.inner, ptr, len);
        }
    }

    fn set_upper_bound_impl(&mut self, bound: Option<Vec<u8>>) {
        let (ptr, len) = if let Some(ref bound) = bound {
            (bound.as_ptr() as *const c_char, bound.len())
        } else if self.iterate_upper_bound.is_some() {
            (std::ptr::null(), 0)
        } else {
            return;
        };
        self.iterate_upper_bound = bound;
        unsafe {
            ffi::rocksdb_readoptions_set_iterate_upper_bound(self.inner, ptr, len);
        }
    }

    /// Specify if this read request should process data that ALREADY
    /// resides on a particular cache. If the required data is not
    /// found at the specified cache, then Status::Incomplete is returned.
    ///
    /// Default: ::All
    pub fn set_read_tier(&mut self, tier: ReadTier) {
        unsafe {
            ffi::rocksdb_readoptions_set_read_tier(self.inner, tier as c_int);
        }
    }

    /// Enforce that the iterator only iterates over the same
    /// prefix as the seek.
    /// This option is effective only for prefix seeks, i.e. prefix_extractor is
    /// non-null for the column family and total_order_seek is false.  Unlike
    /// iterate_upper_bound, prefix_same_as_start only works within a prefix
    /// but in both directions.
    ///
    /// Default: false
    pub fn set_prefix_same_as_start(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_prefix_same_as_start(self.inner, c_uchar::from(v));
        }
    }

    /// Enable a total order seek regardless of index format (e.g. hash index)
    /// used in the table. Some table format (e.g. plain table) may not support
    /// this option.
    ///
    /// If true when calling Get(), we also skip prefix bloom when reading from
    /// block based table. It provides a way to read existing data after
    /// changing implementation of prefix extractor.
    pub fn set_total_order_seek(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_total_order_seek(self.inner, c_uchar::from(v));
        }
    }

    /// Sets a threshold for the number of keys that can be skipped
    /// before failing an iterator seek as incomplete. The default value of 0 should be used to
    /// never fail a request as incomplete, even on skipping too many keys.
    ///
    /// Default: 0
    pub fn set_max_skippable_internal_keys(&mut self, num: u64) {
        unsafe {
            ffi::rocksdb_readoptions_set_max_skippable_internal_keys(self.inner, num);
        }
    }

    /// If true, when PurgeObsoleteFile is called in CleanupIteratorState, we schedule a background job
    /// in the flush job queue and delete obsolete files in background.
    ///
    /// Default: false
    pub fn set_background_purge_on_iterator_cleanup(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_background_purge_on_iterator_cleanup(
                self.inner,
                c_uchar::from(v),
            );
        }
    }

    /// If true, keys deleted using the DeleteRange() API will be visible to
    /// readers until they are naturally deleted during compaction.
    ///
    /// Default: false
    #[deprecated(
        note = "deprecated in RocksDB 10.2.1: no performance impact if DeleteRange is not used"
    )]
    pub fn set_ignore_range_deletions(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_ignore_range_deletions(self.inner, c_uchar::from(v));
        }
    }

    /// If true, all data read from underlying storage will be
    /// verified against corresponding checksums.
    ///
    /// Default: true
    pub fn set_verify_checksums(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_verify_checksums(self.inner, c_uchar::from(v));
        }
    }

    /// If non-zero, an iterator will create a new table reader which
    /// performs reads of the given size. Using a large size (> 2MB) can
    /// improve the performance of forward iteration on spinning disks.
    /// Default: 0
    ///
    /// ```
    /// use rust_rocksdb::{ReadOptions};
    ///
    /// let mut opts = ReadOptions::default();
    /// opts.set_readahead_size(4_194_304); // 4mb
    /// ```
    pub fn set_readahead_size(&mut self, v: usize) {
        unsafe {
            ffi::rocksdb_readoptions_set_readahead_size(self.inner, v as size_t);
        }
    }

    /// If auto_readahead_size is set to true, it will auto tune the readahead_size
    /// during scans internally.
    /// For this feature to be enabled, iterate_upper_bound must also be specified.
    ///
    /// NOTE: - Recommended for forward Scans only.
    ///       - If there is a backward scans, this option will be
    ///         disabled internally and won't be enabled again if the forward scan
    ///         is issued again.
    ///
    /// Default: true
    pub fn set_auto_readahead_size(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_auto_readahead_size(self.inner, c_uchar::from(v));
        }
    }

    /// If true, create a tailing iterator. Note that tailing iterators
    /// only support moving in the forward direction. Iterating in reverse
    /// or seek_to_last are not supported.
    pub fn set_tailing(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_tailing(self.inner, c_uchar::from(v));
        }
    }

    /// Specifies the value of "pin_data". If true, it keeps the blocks
    /// loaded by the iterator pinned in memory as long as the iterator is not deleted,
    /// If used when reading from tables created with
    /// BlockBasedTableOptions::use_delta_encoding = false,
    /// Iterator's property "rocksdb.iterator.is-key-pinned" is guaranteed to
    /// return 1.
    ///
    /// Default: false
    pub fn set_pin_data(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_pin_data(self.inner, c_uchar::from(v));
        }
    }

    /// Asynchronously prefetch some data.
    ///
    /// Used for sequential reads and internal automatic prefetching.
    ///
    /// Default: `false`
    pub fn set_async_io(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_async_io(self.inner, c_uchar::from(v));
        }
    }

    /// Selects the multi-level vs single-level parallel `MultiGet` path when
    /// the library is built with `USE_COROUTINES` (the `coroutines` cargo
    /// feature) and `set_async_io(true)` has been called.
    ///
    /// When `true` (the C++ default), `MultiGet` parallelises reads across
    /// LSM levels, giving the lowest latency at the cost of higher CPU and
    /// coroutine scheduling overhead. When `false`, parallelism is limited
    /// to within a single level, trading some latency for CPU savings.
    ///
    /// Has no effect outside of `USE_COROUTINES` builds with `async_io=true`.
    /// With either condition unmet, both code paths in `db/version_set.cc`
    /// fall through to the synchronous per-file lookup regardless of this
    /// flag's value.
    ///
    /// See the RocksDB ["Asynchronous IO in RocksDB" blog
    /// post](https://rocksdb.org/blog/2022/10/07/asynchronous-io-in-rocksdb.html)
    /// for the qualitative tradeoff: `optimize_multiget_for_io=true`
    /// (multi-level) is the lowest-latency configuration but costs the most
    /// CPU; `optimize_multiget_for_io=false` (single-level, with `async_io`
    /// still on) retains most of the latency win at meaningfully lower CPU.
    ///
    /// Default: `true`
    pub fn set_optimize_multiget_for_io(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_optimize_multiget_for_io(self.inner, c_uchar::from(v));
        }
    }

    /// Returns the current value of [`Self::set_optimize_multiget_for_io`].
    ///
    /// Provided primarily for tests that want to confirm the setter is wired
    /// through to the underlying C++ `ReadOptions`. Reads through to the C
    /// API getter without exposing the underlying `c_uchar` representation.
    pub fn get_optimize_multiget_for_io(&self) -> bool {
        unsafe { ffi::rocksdb_readoptions_get_optimize_multiget_for_io(self.inner) != 0 }
    }

    /// Deadline for completing an API call (Get/MultiGet/Seek/Next for now)
    /// in microseconds.
    /// It should be set to microseconds since epoch, i.e, gettimeofday or
    /// equivalent plus allowed duration in microseconds.
    /// This is best effort. The call may exceed the deadline if there is IO
    /// involved and the file system doesn't support deadlines, or due to
    /// checking for deadline periodically rather than for every key if
    /// processing a batch
    pub fn set_deadline(&mut self, microseconds: u64) {
        unsafe {
            ffi::rocksdb_readoptions_set_deadline(self.inner, microseconds);
        }
    }

    /// A timeout in microseconds to be passed to the underlying FileSystem for
    /// reads. As opposed to deadline, this determines the timeout for each
    /// individual file read request. If a MultiGet/Get/Seek/Next etc call
    /// results in multiple reads, each read can last up to io_timeout us.
    pub fn set_io_timeout(&mut self, microseconds: u64) {
        unsafe {
            ffi::rocksdb_readoptions_set_io_timeout(self.inner, microseconds);
        }
    }

    /// Timestamp of operation. Read should return the latest data visible to the
    /// specified timestamp. All timestamps of the same database must be of the
    /// same length and format. The user is responsible for providing a customized
    /// compare function via Comparator to order <key, timestamp> tuples.
    /// For iterator, iter_start_ts is the lower bound (older) and timestamp
    /// serves as the upper bound. Versions of the same record that fall in
    /// the timestamp range will be returned. If iter_start_ts is nullptr,
    /// only the most recent version visible to timestamp is returned.
    /// The user-specified timestamp feature is still under active development,
    /// and the API is subject to change.
    pub fn set_timestamp<S: Into<Vec<u8>>>(&mut self, ts: S) {
        self.set_timestamp_impl(Some(ts.into()));
    }

    fn set_timestamp_impl(&mut self, ts: Option<Vec<u8>>) {
        let (ptr, len) = if let Some(ref ts) = ts {
            (ts.as_ptr() as *const c_char, ts.len())
        } else if self.timestamp.is_some() {
            // The stored timestamp is a `Some` but we're updating it to a `None`.
            // This means to cancel a previously set timestamp.
            // To do this, use a null pointer and zero length.
            (std::ptr::null(), 0)
        } else {
            return;
        };
        self.timestamp = ts;
        unsafe {
            ffi::rocksdb_readoptions_set_timestamp(self.inner, ptr, len);
        }
    }

    /// See `set_timestamp`
    pub fn set_iter_start_ts<S: Into<Vec<u8>>>(&mut self, ts: S) {
        self.set_iter_start_ts_impl(Some(ts.into()));
    }

    fn set_iter_start_ts_impl(&mut self, ts: Option<Vec<u8>>) {
        let (ptr, len) = if let Some(ref ts) = ts {
            (ts.as_ptr() as *const c_char, ts.len())
        } else if self.timestamp.is_some() {
            (std::ptr::null(), 0)
        } else {
            return;
        };
        self.iter_start_ts = ts;
        unsafe {
            ffi::rocksdb_readoptions_set_iter_start_ts(self.inner, ptr, len);
        }
    }

    /// For iterators, RocksDB does auto-readahead on noticing more than two sequential reads
    /// for a table file if user doesn't provide readahead_size. The readahead starts at 8KB
    /// and doubles on every additional read upto max_auto_readahead_size only when reads are
    /// sequential. However at each level, if iterator moves over next file, readahead_size
    /// starts again from 8KB.
    ///
    /// By enabling this option, RocksDB will do some enhancements for prefetching the data.
    pub fn set_adaptive_readahead(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_adaptive_readahead(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `adaptive_readahead` option.
    pub fn get_adaptive_readahead(&self) -> bool {
        unsafe { ffi::rocksdb_readoptions_get_adaptive_readahead(self.inner) != 0 }
    }

    /// When set, the iterator may defer loading and/or preparing the value when moving to a
    /// different entry (i.e. during SeekToFirst/SeekToLast/Seek/ SeekForPrev/Next/Prev
    /// operations). This can be used to save on I/O and/or CPU when the values associated
    /// with certain keys may not be used by the application. See also
    /// IteratorBase::PrepareValue().
    ///
    /// Note: this option currently only applies to 1) large values stored in blob files using
    /// BlobDB and 2) multi-column-family iterators (CoalescingIterator and
    /// AttributeGroupIterator). Otherwise, it has no effect.
    ///
    /// Default: false
    pub fn set_allow_unprepared_value(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_allow_unprepared_value(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `allow_unprepared_value` option.
    pub fn get_allow_unprepared_value(&self) -> bool {
        unsafe { ffi::rocksdb_readoptions_get_allow_unprepared_value(self.inner) != 0 }
    }

    /// When true, by default use total_order_seek = true, and RocksDB can selectively enable
    /// prefix seek mode if won't generate a different result from total_order_seek, based on
    /// seek key, and iterator upper bound. BUG: Using
    /// Comparator::IsSameLengthImmediateSuccessor and SliceTransform::FullLengthEnabled to
    /// enable prefix mode in cases where prefix of upper bound differs from prefix of seek
    /// key has a flaw. If present in the DB, "short keys" (shorter than "full length" prefix)
    /// can be omitted from auto_prefix_mode iteration when they would be present in
    /// total_order_seek iteration, regardless of whether the short keys are "in domain" of
    /// the prefix extractor. This is not an issue if no short keys are added to DB or are not
    /// expected to be returned by such iterators. (We are also assuming the new condition on
    /// IsSameLengthImmediateSuccessor is satisfied; see its BUG section). A bug example is in
    /// DBTest2::AutoPrefixMode1, search for "BUG".
    pub fn set_auto_prefix_mode(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_auto_prefix_mode(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `auto_prefix_mode` option.
    pub fn get_auto_prefix_mode(&self) -> bool {
        unsafe { ffi::rocksdb_readoptions_get_auto_prefix_mode(self.inner) != 0 }
    }

    /// If auto_readahead_size is set to true, it will auto tune the readahead_size during
    /// scans internally based on block cache data when block cache is enabled, iteration
    /// upper bound when `iterate_upper_bound != nullptr` and prefix when
    /// `prefix_same_as_start == true`
    ///
    /// Besides enabling block cache, it also requires `iterate_upper_bound != nullptr` or
    /// `prefix_same_as_start == true` for this option to take effect
    ///
    /// To be specific, it does the following: (1) When `iterate_upper_bound` is specified,
    /// trim the readahead so the readahead does not exceed iteration upper bound (2) When
    /// `prefix_same_as_start` is set to true, trim the readahead so data blocks containing
    /// keys that are not in the same prefix as the seek key in `Seek()` are not prefetched
    /// - Limition: `Seek(key)` instead of `SeekToFirst()` needs to be called in order for
    ///   this trimming to take effect
    ///
    /// NOTE: - Used for forward Scans only.
    /// - If there is a backward scans, this option will be disabled internally and won't be
    ///   enabled again if the forward scan is issued again.
    ///
    /// Default: true
    pub fn get_auto_readahead_size(&self) -> bool {
        unsafe { ffi::rocksdb_readoptions_get_auto_readahead_size(self.inner) != 0 }
    }

    /// EXPERIMENTAL
    ///
    /// Long-running iterators are holding onto memory and storage resources long after they
    /// are obsolete. This setting (when enabled) will fix that problem for as long as
    /// iterator periodically makes some progress and its supplied `read_options` was
    /// configured with non-nullptr `snapshot` value. The feature is engineered so that the
    /// performance impact should be negligible. We expect the default value to be true some
    /// time in the future.
    ///
    /// NOTE 1: Does not have effect on TransactionDB with WRITE_PREPARED or WRITE_UNPREPARED
    /// policies (currently incompatible).
    ///
    /// NOTE 2: True is not recommended if using user-defined timestamp with
    /// persist_user_defined_timestamps=false and non-nullptr ReadOptions::timestamp or
    /// ReadOptions::iter_start_ts, because auto-refreshing iterator will not prevent user
    /// timestamp information from being dropped during iteration. Auto-refresh might be
    /// disabled for this combination in the future.
    ///
    /// Default: false
    pub fn set_auto_refresh_iterator_with_snapshot(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_readoptions_set_auto_refresh_iterator_with_snapshot(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `auto_refresh_iterator_with_snapshot` option.
    pub fn get_auto_refresh_iterator_with_snapshot(&self) -> bool {
        unsafe { ffi::rocksdb_readoptions_get_auto_refresh_iterator_with_snapshot(self.inner) != 0 }
    }

    /// EXPERIMENTAL
    pub fn set_io_activity(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_readoptions_set_io_activity(self.inner, val);
        }
    }

    /// Returns the value of the `io_activity` option.
    pub fn get_io_activity(&self) -> c_int {
        unsafe { ffi::rocksdb_readoptions_get_io_activity(self.inner) }
    }

    /// When the number of merge operands applied exceeds this threshold during a successful
    /// query, the operation will return a special OK Status with subcode
    /// kMergeOperandThresholdExceeded. Currently only applies to point lookups and is
    /// disabled by default.
    pub fn set_merge_operand_count_threshold(&mut self, val: usize) {
        unsafe {
            ffi::rocksdb_readoptions_set_merge_operand_count_threshold(self.inner, val);
        }
    }

    /// Returns the value of the `merge_operand_count_threshold` option.
    pub fn get_merge_operand_count_threshold(&self) -> usize {
        unsafe { ffi::rocksdb_readoptions_get_merge_operand_count_threshold(self.inner) }
    }

    /// For file reads associated with this option, charge the internal rate limiter (see
    /// `DBOptions::rate_limiter`) at the specified priority. The special value
    /// `Env::IO_TOTAL` disables charging the rate limiter.
    ///
    /// The rate limiting is bypassed no matter this option's value for file reads on plain
    /// tables (these can exist when `ColumnFamilyOptions::table_factory` is a
    /// `PlainTableFactory`) and cuckoo tables (these can exist when
    /// `ColumnFamilyOptions::table_factory` is a `CuckooTableFactory`).
    ///
    /// The bytes charged to rate limiter may not exactly match the file read bytes since
    /// there are some seemingly insignificant reads, like for file headers/footers, that we
    /// currently do not charge to rate limiter.
    pub fn set_rate_limiter_priority(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_readoptions_set_rate_limiter_priority(self.inner, val);
        }
    }

    /// Returns the value of the `rate_limiter_priority` option.
    pub fn get_rate_limiter_priority(&self) -> c_int {
        unsafe { ffi::rocksdb_readoptions_get_rate_limiter_priority(self.inner) }
    }

    /// Soft limit on the cumulative value size read by a single MultiGet, to bound how much
    /// it buffers. It always makes progress: at least one key is read even if its value alone
    /// exceeds the limit. Once the returned size exceeds the limit, subsequent keys get
    /// status Aborted (so a caller can retry them, and cannot loop forever on a single value
    /// that by itself exceeds the limit).
    pub fn set_value_size_soft_limit(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_readoptions_set_value_size_soft_limit(self.inner, val);
        }
    }

    /// Returns the value of the `value_size_soft_limit` option.
    pub fn get_value_size_soft_limit(&self) -> u64 {
        unsafe { ffi::rocksdb_readoptions_get_value_size_soft_limit(self.inner) }
    }
}

impl Default for ReadOptions {
    fn default() -> Self {
        unsafe {
            Self {
                inner: ffi::rocksdb_readoptions_create(),
                timestamp: None,
                iter_start_ts: None,
                iterate_upper_bound: None,
                iterate_lower_bound: None,
            }
        }
    }
}

impl IngestExternalFileOptions {
    /// Can be set to true to move the files instead of copying them.
    pub fn set_move_files(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_move_files(self.inner, c_uchar::from(v));
        }
    }

    /// If set to false, an ingested file keys could appear in existing snapshots
    /// that where created before the file was ingested.
    pub fn set_snapshot_consistency(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_snapshot_consistency(
                self.inner,
                c_uchar::from(v),
            );
        }
    }

    /// If set to false, IngestExternalFile() will fail if the file key range
    /// overlaps with existing keys or tombstones in the DB.
    pub fn set_allow_global_seqno(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_allow_global_seqno(
                self.inner,
                c_uchar::from(v),
            );
        }
    }

    /// If set to false and the file key range overlaps with the memtable key range
    /// (memtable flush required), IngestExternalFile will fail.
    pub fn set_allow_blocking_flush(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_allow_blocking_flush(
                self.inner,
                c_uchar::from(v),
            );
        }
    }

    /// Set to true if you would like duplicate keys in the file being ingested
    /// to be skipped rather than overwriting existing data under that key.
    /// Usecase: back-fill of some historical data in the database without
    /// over-writing existing newer version of data.
    /// This option could only be used if the DB has been running
    /// with allow_ingest_behind=true since the dawn of time.
    /// All files will be ingested at the bottommost level with seqno=0.
    pub fn set_ingest_behind(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_ingest_behind(self.inner, c_uchar::from(v));
        }
    }

    /// Normally (true), IngestExternalFile() will trigger and block for flushing memtable(s)
    /// if there is overlap between ingested files and memtable(s). If allow_blocking_flush is
    /// set to false, IngestExternalFile() will fail if the file key range overlaps with the
    /// memtable key range (memtable flush required).
    pub fn get_allow_blocking_flush(&self) -> bool {
        unsafe { ffi::rocksdb_ingestexternalfileoptions_get_allow_blocking_flush(self.inner) != 0 }
    }

    /// EXPERIMENTAL, SUBJECT TO CHANGE
    ///
    /// Enables special mode of ingestion that allows files generated by a live DB, instead of
    /// SstFileWriter. When true:
    /// - Allows files to be ingested when their cf_id doesn't match the CF they are being
    ///   ingested into.
    /// - Allows files with any sequence numbers to be ingested.
    /// - Original sequence numbers are preserved (no reassignment).
    ///
    /// REQUIREMENTS:
    /// - Ingested files must NOT overlap with any existing data in the DB. Since no
    ///   sequence number reassignment is performed on db generated files. Ingestion will
    ///   fail if any overlap is detected. However, input files are allowed to overlap with
    ///   each other when this option is enabled. This is useful when ingesting multiple
    ///   levels of files from a CF, where levels naturally overlap with each other.
    /// - CAUTION: If input files overlap with each other, then for any given user key
    ///   appearing in multiple files, earlier files MUST have smaller sequence numbers than
    ///   later files. Later files will be placed at a higher level (smaller level number).
    ///   This is to ensure the LSM invariant where for the same key, recent updates are in
    ///   higher levels. This means that if you are ingesting files from multiple levels of
    ///   a CF, you should put files from lower levels first, and files from higher levels
    ///   later. Example for getting files from a CF for ingestion:
    ///
    /// ColumnFamilyMetaData cf_meta; from_db->GetColumnFamilyMetaData(from_cf, &cf_meta); //
    /// iterate in reverse to start from lowest level for (auto level_meta =
    /// cf_meta.levels.rbegin(); level_meta != cf_meta.levels.rend(); ++level_meta) { // L0
    /// files need to be added in reverse order so we iterate in reverse // within a level too
    /// for (auto file_meta = level_meta->files.rbegin(); file_meta !=
    /// level_meta->files.rend(); ++file_meta) { // Add file for ingestion } }
    ///
    /// WARNING: Violating the sequence number ordering requirement will cause LSM invariant
    /// violations and may lead to incorrect reads or data corruption.
    /// - If you would like to enforce that the ingested files do not overlap with each
    ///   other, you can set `fail_if_not_bottommost_level` to true. If ingested files
    ///   overlap with each other, some file will be placed above Lmax, failing the
    ///   ingestion if the option is set.
    /// - `write_global_seqno` must be false (sequence numbers cannot be reassigned).
    pub fn set_allow_db_generated_files(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_allow_db_generated_files(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `allow_db_generated_files` option.
    pub fn get_allow_db_generated_files(&self) -> bool {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_get_allow_db_generated_files(self.inner) != 0
        }
    }

    /// Enables assiging a global sequence number to each ingested file, i.e., all keys in the
    /// ingested file will be treated as having this seqno. If set to false, we will use the
    /// sequence numbers in the ingested file as is, and IngestExternalFile() will fail if the
    /// ingested key range overlaps with existing keys or tombstones or output of ongoing
    /// compaction in the CF (the conditions under which a global seqno must be assigned to
    /// the ingested file). If the ingested files overlap with each other, we need to assign
    /// global sequence to the ingested files and this option needs to be enabled. One
    /// exception to this is when ingesting DB generated SST files (see option
    /// allow_db_generated_files below). DB generated files do not support global seqno
    /// assignment and can be ingested even if they overlap with each other. This option has
    /// no effect when allow_db_generated_files is enabled.
    pub fn get_allow_global_seqno(&self) -> bool {
        unsafe { ffi::rocksdb_ingestexternalfileoptions_get_allow_global_seqno(self.inner) != 0 }
    }

    /// Set to TRUE if user wants file to be ingested to the last level. An error of
    /// Status::TryAgain() will be returned if a file cannot fit in the last level when
    /// calling DB::IngestExternalFile()/DB::IngestExternalFiles(). The user should clear the
    /// last level in the overlapping range before re-attempt.
    ///
    /// ingest_behind takes precedence over fail_if_not_bottommost_level.
    ///
    /// XXX: "bottommost" is obsolete/confusing terminology to refer to last level
    pub fn get_fail_if_not_bottommost_level(&self) -> bool {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_get_fail_if_not_bottommost_level(self.inner) != 0
        }
    }

    /// If set to true, ingestion falls back to copy when hard linking fails. This applies to
    /// both `move_files` and `link_files`.
    pub fn set_failed_move_fall_back_to_copy(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_failed_move_fall_back_to_copy(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `failed_move_fall_back_to_copy` option.
    pub fn get_failed_move_fall_back_to_copy(&self) -> bool {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_get_failed_move_fall_back_to_copy(self.inner)
                != 0
        }
    }

    /// Maximum number of threads used to open table readers for the files being ingested
    /// during commit, can speed up ingestion performance, when ingesting multiple files at
    /// once.
    pub fn set_file_opening_threads(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_file_opening_threads(self.inner, val);
        }
    }

    /// Returns the value of the `file_opening_threads` option.
    pub fn get_file_opening_threads(&self) -> c_int {
        unsafe { ffi::rocksdb_ingestexternalfileoptions_get_file_opening_threads(self.inner) }
    }

    /// Should the "data block"/"index block" read for this iteration be placed in block
    /// cache? Callers may wish to set this field to false for bulk scans. This would help not
    /// to the change eviction order of existing items in the block cache.
    pub fn set_fill_cache(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_fill_cache(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `fill_cache` option.
    pub fn get_fill_cache(&self) -> bool {
        unsafe { ffi::rocksdb_ingestexternalfileoptions_get_fill_cache(self.inner) != 0 }
    }

    /// Set to true if you would like duplicate keys in the file being ingested to be skipped
    /// rather than overwriting existing data under that key. Use case: back-fill of some
    /// historical data in the database without over-writing existing newer version of data.
    /// This option could only be used if the CF has been running with
    /// cf_allow_ingest_behind=true since CF creation (or before any write). All files will be
    /// ingested at the bottommost level with seqno=0.
    pub fn get_ingest_behind(&self) -> bool {
        unsafe { ffi::rocksdb_ingestexternalfileoptions_get_ingest_behind(self.inner) != 0 }
    }

    /// Same as move_files except that input files will NOT be unlinked. Only one of
    /// `move_files` and `link_files` can be set at the same time.
    pub fn set_link_files(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_link_files(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the value of the `link_files` option.
    pub fn get_link_files(&self) -> bool {
        unsafe { ffi::rocksdb_ingestexternalfileoptions_get_link_files(self.inner) != 0 }
    }

    /// Can be set to true to move the files instead of copying them. The input files will be
    /// unlinked after successful ingestion. The implementation depends on hard links
    /// (LinkFile) instead of traditional move (RenameFile) to maximize the chances to restore
    /// to the original state upon failure.
    pub fn get_move_files(&self) -> bool {
        unsafe { ffi::rocksdb_ingestexternalfileoptions_get_move_files(self.inner) != 0 }
    }

    /// Controls whether external file ingestion should prefetch index and filter blocks while
    /// opening table readers during commit. Setting this to false can reduce commit latency
    /// for bulk loads into Lmax when
    /// (BlockBasedTableOptions::cache_index_and_filter_blocks=true or partitioned
    /// filters/indexes are enabled).
    pub fn set_prefetch_lmax_index_and_filter_blocks(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_prefetch_lmax_index_and_filter_blocks(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `prefetch_lmax_index_and_filter_blocks` option.
    pub fn get_prefetch_lmax_index_and_filter_blocks(&self) -> bool {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_get_prefetch_lmax_index_and_filter_blocks(
                self.inner,
            ) != 0
        }
    }

    /// If set to false, an ingested file keys could appear in existing snapshots that where
    /// created before the file was ingested.
    pub fn get_snapshot_consistency(&self) -> bool {
        unsafe { ffi::rocksdb_ingestexternalfileoptions_get_snapshot_consistency(self.inner) != 0 }
    }

    /// Set to true if you would like to verify the checksums of each block of the external
    /// SST file before ingestion. Warning: setting this to true causes slowdown in file
    /// ingestion because the external SST file has to be read.
    pub fn set_verify_checksums_before_ingest(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_verify_checksums_before_ingest(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `verify_checksums_before_ingest` option.
    pub fn get_verify_checksums_before_ingest(&self) -> bool {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_get_verify_checksums_before_ingest(self.inner)
                != 0
        }
    }

    /// When verify_checksums_before_ingest = true, RocksDB uses default readahead setting to
    /// scan the file while verifying checksums before ingestion. Users can override the
    /// default value using this option. Using a large readahead size (> 2MB) can typically
    /// improve the performance of forward iteration on spinning disks.
    pub fn set_verify_checksums_readahead_size(&mut self, val: usize) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_verify_checksums_readahead_size(
                self.inner, val,
            );
        }
    }

    /// Returns the value of the `verify_checksums_readahead_size` option.
    pub fn get_verify_checksums_readahead_size(&self) -> usize {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_get_verify_checksums_readahead_size(self.inner)
        }
    }

    /// Set to TRUE if user wants to verify the sst file checksum of ingested files. The DB
    /// checksum function will generate the checksum of each ingested file (if
    /// file_checksum_gen_factory is set) and compare the checksum function name and checksum
    /// with the ingested checksum information.
    ///
    /// If this option is set to True: 1) if DB does not enable checksum
    /// (file_checksum_gen_factory == nullptr), the ingested checksum information will be
    /// ignored; 2) If DB enable the checksum function, we calculate the sst file checksum
    /// after the file is moved or copied and compare the checksum and checksum name. If
    /// checksum or checksum function name does not match, ingestion will be failed. If the
    /// verification is successful, checksum and checksum function name will be stored in
    /// Manifest. If this option is set to FALSE, 1) if DB does not enable checksum, the
    /// ingested checksum information will be ignored; 2) if DB enable the checksum, we only
    /// verify the ingested checksum function name and we trust the ingested checksum. If the
    /// checksum function name matches, we store the checksum in Manifest. DB does not
    /// calculate the checksum during ingestion. However, if no checksum information is
    /// provided with the ingested files, DB will generate the checksum and store in the
    /// Manifest.
    pub fn set_verify_file_checksum(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_verify_file_checksum(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `verify_file_checksum` option.
    pub fn get_verify_file_checksum(&self) -> bool {
        unsafe { ffi::rocksdb_ingestexternalfileoptions_get_verify_file_checksum(self.inner) != 0 }
    }

    /// DEPRECATED - Set to true if you would like to write global_seqno to the external SST
    /// file on ingestion for backward compatibility before RocksDB 5.16.0. Such old versions
    /// of RocksDB expect any global_seqno to be written to the SST file rather than recorded
    /// in the DB manifest. This functionality was deprecated because (a) random writes might
    /// be costly or unsupported on some FileSystems, and (b) the file checksum changes with
    /// such a write.
    pub fn set_write_global_seqno(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_ingestexternalfileoptions_set_write_global_seqno(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `write_global_seqno` option.
    pub fn get_write_global_seqno(&self) -> bool {
        unsafe { ffi::rocksdb_ingestexternalfileoptions_get_write_global_seqno(self.inner) != 0 }
    }
}

impl Default for IngestExternalFileOptions {
    fn default() -> Self {
        unsafe {
            Self {
                inner: ffi::rocksdb_ingestexternalfileoptions_create(),
            }
        }
    }
}

/// Used by BlockBasedOptions::set_index_type.
pub enum BlockBasedIndexType {
    /// A space efficient index block that is optimized for
    /// binary-search-based index.
    BinarySearch,

    /// The hash index, if enabled, will perform a hash lookup if
    /// a prefix extractor has been provided through Options::set_prefix_extractor.
    HashSearch,

    /// A two-level index implementation. Both levels are binary search indexes.
    TwoLevelIndexSearch,
}

/// Used by BlockBasedOptions::set_data_block_index_type.
#[repr(C)]
pub enum DataBlockIndexType {
    /// Use binary search when performing point lookup for keys in data blocks.
    /// This is the default.
    BinarySearch = 0,

    /// Appends a compact hash table to the end of the data block for efficient indexing. Backwards
    /// compatible with databases created without this feature. Once turned on, existing data will
    /// be gradually converted to the hash index format.
    BinaryAndHash = 1,
}

/// Defines the underlying memtable implementation.
/// See official [wiki](https://github.com/facebook/rocksdb/wiki/MemTable) for more information.
pub enum MemtableFactory {
    Vector,
    HashSkipList {
        bucket_count: usize,
        height: i32,
        branching_factor: i32,
    },
    HashLinkList {
        bucket_count: usize,
    },
}

/// Used by BlockBasedOptions::set_checksum_type.
pub enum ChecksumType {
    NoChecksum = 0,
    CRC32c = 1,
    XXHash = 2,
    XXHash64 = 3,
    XXH3 = 4, // Supported since RocksDB 6.27
}

/// Used in [`PlainTableFactoryOptions`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub enum KeyEncodingType {
    /// Always write full keys.
    #[default]
    Plain = 0,
    /// Find opportunities to write the same prefix for multiple rows.
    Prefix = 1,
}

/// Used with DBOptions::set_plain_table_factory.
/// See official [wiki](https://github.com/facebook/rocksdb/wiki/PlainTable-Format) for more
/// information.
///
/// Defaults:
///  user_key_length: 0 (variable length)
///  bloom_bits_per_key: 10
///  hash_table_ratio: 0.75
///  index_sparseness: 16
///  huge_page_tlb_size: 0
///  encoding_type: KeyEncodingType::Plain
///  full_scan_mode: false
///  store_index_in_file: false
pub struct PlainTableFactoryOptions {
    pub user_key_length: u32,
    pub bloom_bits_per_key: i32,
    pub hash_table_ratio: f64,
    pub index_sparseness: usize,
    pub huge_page_tlb_size: usize,
    pub encoding_type: KeyEncodingType,
    pub full_scan_mode: bool,
    pub store_index_in_file: bool,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub enum DBCompressionType {
    None = ffi::rocksdb_no_compression as isize,
    Snappy = ffi::rocksdb_snappy_compression as isize,
    Zlib = ffi::rocksdb_zlib_compression as isize,
    Bz2 = ffi::rocksdb_bz2_compression as isize,
    Lz4 = ffi::rocksdb_lz4_compression as isize,
    Lz4hc = ffi::rocksdb_lz4hc_compression as isize,
    Zstd = ffi::rocksdb_zstd_compression as isize,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub enum DBCompactionStyle {
    Level = ffi::rocksdb_level_compaction as isize,
    Universal = ffi::rocksdb_universal_compaction as isize,
    Fifo = ffi::rocksdb_fifo_compaction as isize,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub enum DBRecoveryMode {
    TolerateCorruptedTailRecords = ffi::rocksdb_tolerate_corrupted_tail_records_recovery as isize,
    AbsoluteConsistency = ffi::rocksdb_absolute_consistency_recovery as isize,
    PointInTime = ffi::rocksdb_point_in_time_recovery as isize,
    SkipAnyCorruptedRecord = ffi::rocksdb_skip_any_corrupted_records_recovery as isize,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(i32)]
pub enum RateLimiterMode {
    KReadsOnly = 0,
    KWritesOnly = 1,
    KAllIo = 2,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub enum DBCompactionPri {
    ByCompensatedSize = ffi::rocksdb_k_by_compensated_size_compaction_pri as isize,
    OldestLargestSeqFirst = ffi::rocksdb_k_oldest_largest_seq_first_compaction_pri as isize,
    OldestSmallestSeqFirst = ffi::rocksdb_k_oldest_smallest_seq_first_compaction_pri as isize,
    MinOverlappingRatio = ffi::rocksdb_k_min_overlapping_ratio_compaction_pri as isize,
    RoundRobin = ffi::rocksdb_k_round_robin_compaction_pri as isize,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub enum BlockBasedPinningTier {
    Fallback = ffi::rocksdb_block_based_k_fallback_pinning_tier as isize,
    None = ffi::rocksdb_block_based_k_none_pinning_tier as isize,
    FlushAndSimilar = ffi::rocksdb_block_based_k_flush_and_similar_pinning_tier as isize,
    All = ffi::rocksdb_block_based_k_all_pinning_tier as isize,
}

/// Index-block search algorithm selected by
/// [`BlockBasedOptions::set_index_block_search_type`].
///
/// `Auto` is only meaningful in combination with
/// [`BlockBasedOptions::set_uniform_cv_threshold`]: the threshold gates whether
/// the per-block "is_uniform" footer bit is set on the write path, and `Auto`
/// reads that bit at lookup time to choose between binary and interpolation
/// search per index block. Without setting the threshold to a non-negative
/// value, `Auto` degenerates to binary search.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub enum IndexBlockSearchType {
    /// Standard binary search. The default and safest choice.
    Binary = ffi::rocksdb_block_based_table_index_block_search_type_binary as isize,
    /// Interpolation search. Faster than binary search for index blocks whose
    /// keys are uniformly distributed; significantly slower when they are not.
    ///
    /// Only applicable when the byte-wise comparator is in use; with any
    /// other comparator the C++ code falls back to binary search regardless.
    ///
    /// Performance is significantly degraded when
    /// `IndexShorteningMode::kShortenSeparatorsAndSuccessor` is also set,
    /// because the shortened successor skews end-keys away from the uniform
    /// distribution that interpolation search relies on. Avoid combining the
    /// two.
    Interpolation = ffi::rocksdb_block_based_table_index_block_search_type_interpolation as isize,
    /// Per-block adaptive selection between binary and interpolation search,
    /// based on the per-block "is_uniform" footer bit. Requires
    /// `uniform_cv_threshold >= 0` on the write path; see
    /// [`BlockBasedOptions::set_uniform_cv_threshold`].
    Auto = ffi::rocksdb_block_based_table_index_block_search_type_auto as isize,
}

pub struct FifoCompactOptions {
    pub(crate) inner: *mut ffi::rocksdb_fifo_compaction_options_t,
}

impl Default for FifoCompactOptions {
    fn default() -> Self {
        let opts = unsafe { ffi::rocksdb_fifo_compaction_options_create() };
        assert!(
            !opts.is_null(),
            "Could not create RocksDB Fifo Compaction Options"
        );

        Self { inner: opts }
    }
}

impl Drop for FifoCompactOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_fifo_compaction_options_destroy(self.inner);
        }
    }
}

impl FifoCompactOptions {
    /// Sets the max table file size.
    ///
    /// Once the total sum of table files reaches this, we will delete the oldest
    /// table file
    ///
    /// Default: 1GB
    pub fn set_max_table_files_size(&mut self, nbytes: u64) {
        unsafe {
            ffi::rocksdb_fifo_compaction_options_set_max_table_files_size(self.inner, nbytes);
        }
    }

    /// DEPRECATED When not 0, if the data in the file is older than this threshold, RocksDB
    /// will soon move the file to warm temperature.
    pub fn set_age_for_warm(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_fifo_compaction_options_set_age_for_warm(self.inner, val);
        }
    }

    /// Returns the value of the `age_for_warm` option.
    pub fn get_age_for_warm(&self) -> u64 {
        unsafe { ffi::rocksdb_fifo_compaction_options_get_age_for_warm(self.inner) }
    }

    /// EXPERIMENTAL If true, when compaction is picked for kChangeTemperature reason, allow
    /// the trivia copy of the sst file from source FileSystem to destination FileSystem. If
    /// false, the changeTemperature will be the non-trivial copy by iterating/appending
    /// blocks by blocks of the sst file.
    pub fn set_allow_trivial_copy_when_change_temperature(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_fifo_compaction_options_set_allow_trivial_copy_when_change_temperature(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `allow_trivial_copy_when_change_temperature` option.
    pub fn get_allow_trivial_copy_when_change_temperature(&self) -> bool {
        unsafe {
            ffi::rocksdb_fifo_compaction_options_get_allow_trivial_copy_when_change_temperature(
                self.inner,
            ) != 0
        }
    }

    /// EXPERIMENTAL If 'allow_trivia_copy_op_when_change_temperature=true', the tmp buffer
    /// size to copy the file from the source FileSystem to the destnation FileSystem. If
    /// 'allow_trivia_copy_op_when_change_temperature=false', this field will not be used. The
    /// minmum buffer size must be at least 4KiB
    pub fn set_trivial_copy_buffer_size(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_fifo_compaction_options_set_trivial_copy_buffer_size(self.inner, val);
        }
    }

    /// Returns the value of the `trivial_copy_buffer_size` option.
    pub fn get_trivial_copy_buffer_size(&self) -> u64 {
        unsafe { ffi::rocksdb_fifo_compaction_options_get_trivial_copy_buffer_size(self.inner) }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub enum UniversalCompactionStopStyle {
    Similar = ffi::rocksdb_similar_size_compaction_stop_style as isize,
    Total = ffi::rocksdb_total_size_compaction_stop_style as isize,
}

pub struct UniversalCompactOptions {
    pub(crate) inner: *mut ffi::rocksdb_universal_compaction_options_t,
}

impl Default for UniversalCompactOptions {
    fn default() -> Self {
        let opts = unsafe { ffi::rocksdb_universal_compaction_options_create() };
        assert!(
            !opts.is_null(),
            "Could not create RocksDB Universal Compaction Options"
        );

        Self { inner: opts }
    }
}

impl Drop for UniversalCompactOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_universal_compaction_options_destroy(self.inner);
        }
    }
}

impl UniversalCompactOptions {
    /// Sets the percentage flexibility while comparing file size.
    /// If the candidate file(s) size is 1% smaller than the next file's size,
    /// then include next file into this candidate set.
    ///
    /// Default: 1
    pub fn set_size_ratio(&mut self, ratio: c_int) {
        unsafe {
            ffi::rocksdb_universal_compaction_options_set_size_ratio(self.inner, ratio);
        }
    }

    /// Sets the minimum number of files in a single compaction run.
    ///
    /// Default: 2
    pub fn set_min_merge_width(&mut self, num: c_int) {
        unsafe {
            ffi::rocksdb_universal_compaction_options_set_min_merge_width(self.inner, num);
        }
    }

    /// Sets the maximum number of files in a single compaction run.
    ///
    /// Default: UINT_MAX
    pub fn set_max_merge_width(&mut self, num: c_int) {
        unsafe {
            ffi::rocksdb_universal_compaction_options_set_max_merge_width(self.inner, num);
        }
    }

    /// sets the size amplification.
    ///
    /// It is defined as the amount (in percentage) of
    /// additional storage needed to store a single byte of data in the database.
    /// For example, a size amplification of 2% means that a database that
    /// contains 100 bytes of user-data may occupy upto 102 bytes of
    /// physical storage. By this definition, a fully compacted database has
    /// a size amplification of 0%. Rocksdb uses the following heuristic
    /// to calculate size amplification: it assumes that all files excluding
    /// the earliest file contribute to the size amplification.
    ///
    /// Default: 200, which means that a 100 byte database could require upto 300 bytes of storage.
    pub fn set_max_size_amplification_percent(&mut self, v: c_int) {
        unsafe {
            ffi::rocksdb_universal_compaction_options_set_max_size_amplification_percent(
                self.inner, v,
            );
        }
    }

    /// Sets the percentage of compression size.
    ///
    /// If this option is set to be -1, all the output files
    /// will follow compression type specified.
    ///
    /// If this option is not negative, we will try to make sure compressed
    /// size is just above this value. In normal cases, at least this percentage
    /// of data will be compressed.
    /// When we are compacting to a new file, here is the criteria whether
    /// it needs to be compressed: assuming here are the list of files sorted
    /// by generation time:
    ///    A1...An B1...Bm C1...Ct
    /// where A1 is the newest and Ct is the oldest, and we are going to compact
    /// B1...Bm, we calculate the total size of all the files as total_size, as
    /// well as  the total size of C1...Ct as total_C, the compaction output file
    /// will be compressed iff
    ///   total_C / total_size < this percentage
    ///
    /// Default: -1
    pub fn set_compression_size_percent(&mut self, v: c_int) {
        unsafe {
            ffi::rocksdb_universal_compaction_options_set_compression_size_percent(self.inner, v);
        }
    }

    /// Sets the algorithm used to stop picking files into a single compaction run.
    ///
    /// Default: ::Total
    pub fn set_stop_style(&mut self, style: UniversalCompactionStopStyle) {
        unsafe {
            ffi::rocksdb_universal_compaction_options_set_stop_style(self.inner, style as c_int);
        }
    }

    /// Option to optimize the manual compaction by enabling trivial move for non overlapping
    /// files. Default: false
    pub fn set_allow_trivial_move(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_universal_compaction_options_set_allow_trivial_move(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `allow_trivial_move` option.
    pub fn get_allow_trivial_move(&self) -> bool {
        unsafe { ffi::rocksdb_universal_compaction_options_get_allow_trivial_move(self.inner) != 0 }
    }

    /// EXPERIMENTAL If true, try to limit compaction size under max_compaction_bytes. This
    /// might cause higher write amplification, but can prevent some problem caused by large
    /// compactions. Default: false
    pub fn set_incremental(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_universal_compaction_options_set_incremental(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `incremental` option.
    pub fn get_incremental(&self) -> bool {
        unsafe { ffi::rocksdb_universal_compaction_options_get_incremental(self.inner) != 0 }
    }

    /// The limit on the number of sorted runs. RocksDB will try to keep the number of sorted
    /// runs at most this number. While compactions are running, the number of sorted runs may
    /// be temporarily higher than this number.
    ///
    /// Since universal compaction checks if there is compaction to do when the number of
    /// sorted runs is at least level0_file_num_compaction_trigger, it is suggested to set
    /// level0_file_num_compaction_trigger to be no larger than max_read_amp.
    ///
    /// Values: -1: special flag to let RocksDB pick default. Currently, RocksDB will fall
    /// back to the behavior before this option is introduced, which is to use
    /// level0_file_num_compaction_trigger as the limit. This may change in the future to
    /// behave as 0 below. 0: Let RocksDB auto-tune. Currently, we determine the max number of
    /// sorted runs based on the current DB size, size_ratio and write_buffer_size. Note that
    /// this is only supported for the default stop_style kCompactionStopStyleTotalSize. For
    /// kCompactionStopStyleSimilarSize, this behaves as if -1 is configured. N > 0: limit the
    /// number of sorted runs to be at most N. N should be at least the compaction trigger
    /// specified by level0_file_num_compaction_trigger. If 0 < max_read_amp <
    /// level0_file_num_compaction_trigger, Status::NotSupported() will be returned during DB
    /// open. N < -1: Status::NotSupported() will be returned during DB open.
    ///
    /// Default: -1
    pub fn set_max_read_amp(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_universal_compaction_options_set_max_read_amp(self.inner, val);
        }
    }

    /// Returns the value of the `max_read_amp` option.
    pub fn get_max_read_amp(&self) -> c_int {
        unsafe { ffi::rocksdb_universal_compaction_options_get_max_read_amp(self.inner) }
    }

    /// If true, auto universal compaction picking will adjust to minimize locking of input
    /// files when bottom priority compactions are waiting to run. This can increase the
    /// likelihood of existing L0s being selected for compaction, thereby improving write
    /// stall and reducing read regression. It may increase the overrall write amplification
    /// and compaction load on low priority threads.
    ///
    /// Default: true (enabled)
    ///
    /// This options does not apply to manual compactions.
    ///
    /// This option is temporary in case turning on this feature causes problems and users
    /// need to undo it quickly. This option is planned for removal in the near future with
    /// default value set to true.
    ///
    /// Dynamically changeable through the SetOptions() API.
    pub fn set_reduce_file_locking(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_universal_compaction_options_set_reduce_file_locking(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `reduce_file_locking` option.
    pub fn get_reduce_file_locking(&self) -> bool {
        unsafe {
            ffi::rocksdb_universal_compaction_options_get_reduce_file_locking(self.inner) != 0
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum BottommostLevelCompaction {
    /// Skip bottommost level compaction
    Skip = 0,
    /// Only compact bottommost level if there is a compaction filter
    /// This is the default option
    IfHaveCompactionFilter,
    /// Always compact bottommost level
    Force,
    /// Always compact bottommost level but in bottommost level avoid
    /// double-compacting files created in the same compaction
    ForceOptimized,
}

pub struct CompactOptions {
    pub(crate) inner: *mut ffi::rocksdb_compactoptions_t,
    full_history_ts_low: Option<Vec<u8>>,
}

impl Default for CompactOptions {
    fn default() -> Self {
        let opts = unsafe { ffi::rocksdb_compactoptions_create() };
        assert!(!opts.is_null(), "Could not create RocksDB Compact Options");

        Self {
            inner: opts,
            full_history_ts_low: None,
        }
    }
}

impl Drop for CompactOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_compactoptions_destroy(self.inner);
        }
    }
}

impl CompactOptions {
    /// If more than one thread calls manual compaction,
    /// only one will actually schedule it while the other threads will simply wait
    /// for the scheduled manual compaction to complete. If exclusive_manual_compaction
    /// is set to true, the call will disable scheduling of automatic compaction jobs
    /// and wait for existing automatic compaction jobs to finish.
    pub fn set_exclusive_manual_compaction(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_compactoptions_set_exclusive_manual_compaction(
                self.inner,
                c_uchar::from(v),
            );
        }
    }

    /// Sets bottommost level compaction.
    pub fn set_bottommost_level_compaction(&mut self, lvl: BottommostLevelCompaction) {
        unsafe {
            ffi::rocksdb_compactoptions_set_bottommost_level_compaction(self.inner, lvl as c_uchar);
        }
    }

    /// If true, compacted files will be moved to the minimum level capable
    /// of holding the data or given level (specified non-negative target_level).
    pub fn set_change_level(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_compactoptions_set_change_level(self.inner, c_uchar::from(v));
        }
    }

    /// If change_level is true and target_level have non-negative value, compacted
    /// files will be moved to target_level.
    pub fn set_target_level(&mut self, lvl: c_int) {
        unsafe {
            ffi::rocksdb_compactoptions_set_target_level(self.inner, lvl);
        }
    }

    /// Set user-defined timestamp low bound, the data with older timestamp than
    /// low bound maybe GCed by compaction. Default: nullptr
    pub fn set_full_history_ts_low<S: Into<Vec<u8>>>(&mut self, ts: S) {
        self.set_full_history_ts_low_impl(Some(ts.into()));
    }

    fn set_full_history_ts_low_impl(&mut self, ts: Option<Vec<u8>>) {
        let (ptr, len) = if let Some(ref ts) = ts {
            (ts.as_ptr().cast_mut().cast::<c_char>(), ts.len())
        } else if self.full_history_ts_low.is_some() {
            (std::ptr::null::<Vec<u8>>() as *mut c_char, 0)
        } else {
            return;
        };
        self.full_history_ts_low = ts;
        unsafe {
            ffi::rocksdb_compactoptions_set_full_history_ts_low(self.inner, ptr, len);
        }
    }

    /// Override `CompactRangeOptions::blob_garbage_collection_age_cutoff` for a
    /// single manual compaction.
    ///
    /// If set to `< 0` or `> 1`, RocksDB leaves the
    /// `blob_garbage_collection_age_cutoff` from `ColumnFamilyOptions` in
    /// effect (this is the default, `-1`). Otherwise, it overrides the
    /// user-provided setting for the duration of this compaction. This
    /// enables callers to selectively override the age cutoff per
    /// `compact_range` call.
    ///
    /// See [`Options::set_blob_gc_age_cutoff`] for the CF-level setter that
    /// this value overrides.
    pub fn set_blob_garbage_collection_age_cutoff(&mut self, v: c_double) {
        unsafe {
            ffi::rocksdb_compactoptions_set_blob_garbage_collection_age_cutoff(self.inner, v);
        }
    }

    /// If set to < 0 or > 1, RocksDB leaves blob_garbage_collection_age_cutoff from
    /// ColumnFamilyOptions in effect. Otherwise, it will override the user-provided setting.
    /// This enables customers to selectively override the age cutoff.
    pub fn get_blob_garbage_collection_age_cutoff(&self) -> f64 {
        unsafe { ffi::rocksdb_compactoptions_get_blob_garbage_collection_age_cutoff(self.inner) }
    }

    /// If set to kForce, RocksDB will override enable_blob_file_garbage_collection to true;
    /// if set to kDisable, RocksDB will override it to false, and kUseDefault leaves the
    /// setting in effect. This enables customers to both force-enable and force-disable GC
    /// when calling CompactRange.
    pub fn set_blob_garbage_collection_policy(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_compactoptions_set_blob_garbage_collection_policy(self.inner, val);
        }
    }

    /// Returns the value of the `blob_garbage_collection_policy` option.
    pub fn get_blob_garbage_collection_policy(&self) -> c_int {
        unsafe { ffi::rocksdb_compactoptions_get_blob_garbage_collection_policy(self.inner) }
    }
}

pub struct WaitForCompactOptions {
    pub(crate) inner: *mut ffi::rocksdb_wait_for_compact_options_t,
}

impl Default for WaitForCompactOptions {
    fn default() -> Self {
        let opts = unsafe { ffi::rocksdb_wait_for_compact_options_create() };
        assert!(
            !opts.is_null(),
            "Could not create RocksDB Wait For Compact Options"
        );

        Self { inner: opts }
    }
}

impl Drop for WaitForCompactOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_wait_for_compact_options_destroy(self.inner);
        }
    }
}

impl WaitForCompactOptions {
    /// If true, abort waiting if background jobs are paused. If false,
    /// ContinueBackgroundWork() must be called to resume the background jobs.
    /// Otherwise, jobs that were queued, but not scheduled yet may never finish
    /// and WaitForCompact() may wait indefinitely (if timeout is set, it will
    /// abort after the timeout).
    ///
    /// Default: false
    pub fn set_abort_on_pause(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_wait_for_compact_options_set_abort_on_pause(self.inner, c_uchar::from(v));
        }
    }

    /// If true, flush all column families before starting to wait.
    ///
    /// Default: false
    pub fn set_flush(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_wait_for_compact_options_set_flush(self.inner, c_uchar::from(v));
        }
    }

    /// Timeout in microseconds for waiting for compaction to complete.
    /// when timeout == 0, WaitForCompact() will wait as long as there's background
    /// work to finish.
    ///
    /// Default: 0
    pub fn set_timeout(&mut self, microseconds: u64) {
        unsafe {
            ffi::rocksdb_wait_for_compact_options_set_timeout(self.inner, microseconds);
        }
    }

    /// A boolean to wait for purge to complete
    pub fn set_wait_for_purge(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_wait_for_compact_options_set_wait_for_purge(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `wait_for_purge` option.
    pub fn get_wait_for_purge(&self) -> bool {
        unsafe { ffi::rocksdb_wait_for_compact_options_get_wait_for_purge(self.inner) != 0 }
    }
}

/// Represents a path where sst files can be put into
pub struct DBPath {
    pub(crate) inner: *mut ffi::rocksdb_dbpath_t,
}

impl DBPath {
    /// Create a new path
    pub fn new<P: AsRef<Path>>(path: P, target_size: u64) -> Result<Self, Error> {
        let p = to_cpath(path.as_ref()).unwrap();
        let dbpath = unsafe { ffi::rocksdb_dbpath_create(p.as_ptr(), target_size) };
        if dbpath.is_null() {
            Err(Error::new(format!(
                "Could not create path for storing sst files at location: {}",
                path.as_ref().display()
            )))
        } else {
            Ok(DBPath { inner: dbpath })
        }
    }
}

impl Drop for DBPath {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_dbpath_destroy(self.inner);
        }
    }
}

pub struct InfoLogger {
    pub(crate) inner: *mut ffi::rocksdb_logger_t,
    callback: Option<Arc<LoggerCallback>>,
}

impl InfoLogger {
    /// Creates a new logger that redirects logs to `STDERR` with an optional
    /// prefix.
    pub fn new_stderr_logger<S: AsRef<str>>(log_level: LogLevel, prefix: Option<S>) -> Self {
        let prefix = prefix.map(|s| {
            s.as_ref()
                .into_c_string()
                .expect("cannot have NULL in prefix")
        });
        let prefix_ptr = match prefix.as_ref() {
            Some(s) => s.as_ptr(),
            None => std::ptr::null(),
        };
        let inner =
            unsafe { ffi::rocksdb_logger_create_stderr_logger(log_level as i32, prefix_ptr) };
        Self {
            inner,
            // no Rust callback: RocksDB implements this
            callback: None,
        }
    }

    /// Creates a new logger that redirects logs to a custom callback.
    pub fn new_callback_logger<F: Fn(LogLevel, &str) + Sync + Send + 'static>(
        level: LogLevel,
        cb: F,
    ) -> Self {
        // use an Arc<Box<...>> so we can reference count, and still pass a thin pointer to C
        let arc_cb: Arc<LoggerCallback> = Arc::new(Box::new(cb));
        let raw_cb: LoggerCallbackPtr = Arc::as_ptr(&arc_cb);
        let inner = unsafe {
            ffi::rocksdb_logger_create_callback_logger(
                level as i32,
                Some(logger_callback),
                raw_cb as *mut c_void,
            )
        };
        Self {
            inner,
            callback: Some(arc_cb),
        }
    }
}

impl Drop for InfoLogger {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_logger_destroy(self.inner);
        }
    }
}

/// Options for importing column families. See
/// [DB::create_column_family_with_import](crate::DB::create_column_family_with_import).
pub struct ImportColumnFamilyOptions {
    pub(crate) inner: *mut ffi::rocksdb_import_column_family_options_t,
}

impl ImportColumnFamilyOptions {
    pub fn new() -> Self {
        let inner = unsafe { ffi::rocksdb_import_column_family_options_create() };
        ImportColumnFamilyOptions { inner }
    }

    /// Determines whether to move the provided set of files on import. The default
    /// behavior is to copy the external files on import. Setting `move_files` to `true`
    /// will move the files instead of copying them. See
    /// [DB::create_column_family_with_import](crate::DB::create_column_family_with_import)
    /// for more information.
    pub fn set_move_files(&mut self, move_files: bool) {
        unsafe {
            ffi::rocksdb_import_column_family_options_set_move_files(
                self.inner,
                c_uchar::from(move_files),
            );
        }
    }
}

impl Default for ImportColumnFamilyOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ImportColumnFamilyOptions {
    fn drop(&mut self) {
        unsafe { ffi::rocksdb_import_column_family_options_destroy(self.inner) }
    }
}

/// Ensures the unsafe casts use the same type.
type LoggerCallbackPtr = *const LoggerCallback;

unsafe extern "C" fn logger_callback(
    raw_cb: *mut c_void,
    level: c_uint,
    msg: *mut c_char,
    len: size_t,
) {
    let rust_callback: &LoggerCallback = unsafe { &*(raw_cb as LoggerCallbackPtr) };
    let raw_msg = if len == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(msg.cast_const().cast::<u8>(), len) }
    };
    let msg = String::from_utf8_lossy(raw_msg);
    // Don't panic on an unexpected level: this runs in an `extern "C"` frame,
    // where unwinding aborts the process. Losing the exact level of one log
    // line is not worth taking the process down for.
    let level = LogLevel::try_from_raw(level as i32).unwrap_or(LogLevel::Info);
    (rust_callback)(level, &msg);
}

#[cfg(test)]
mod tests {
    use crate::cache::Cache;
    use crate::db_options::{DBCompactionPri, InfoLogger, WriteBufferManager};
    use crate::{MemtableFactory, Options};

    /// `set_prefix_range_in_place` is an allocation-free reimplementation of
    /// `set_iterate_range(PrefixRange(..))`. It has to produce byte-identical
    /// bounds, including for the awkward cases: empty prefixes, trailing 0xff
    /// bytes, and all-0xff prefixes (which have no successor).
    #[test]
    fn prefix_range_in_place_matches_prefix_range() {
        let cases: &[&[u8]] = &[
            b"",
            b"a",
            b"foo",
            b"\x00",
            b"\xff",
            b"\xff\xff",
            b"a\xff",
            b"a\xff\xff",
            b"\xfe\xff",
            b"prefix\x00\xff",
        ];

        for prefix in cases {
            let mut expected = crate::ReadOptions::default();
            expected.set_iterate_range(crate::PrefixRange(*prefix));

            let mut actual = crate::ReadOptions::default();
            actual.set_prefix_range_in_place(prefix);

            assert_eq!(
                actual.iterate_lower_bound, expected.iterate_lower_bound,
                "lower bound mismatch for prefix {prefix:?}"
            );
            assert_eq!(
                actual.iterate_upper_bound, expected.iterate_upper_bound,
                "upper bound mismatch for prefix {prefix:?}"
            );
        }
    }

    /// The whole point of the in-place setter is that a reused `ReadOptions`
    /// stops reallocating, so overwriting the bounds repeatedly must keep the
    /// results correct rather than leaving stale bytes behind.
    #[test]
    fn prefix_range_in_place_is_reusable() {
        let mut opts = crate::ReadOptions::default();

        opts.set_prefix_range_in_place(b"aaaa");
        assert_eq!(opts.iterate_lower_bound.as_deref(), Some(&b"aaaa"[..]));
        assert_eq!(opts.iterate_upper_bound.as_deref(), Some(&b"aaab"[..]));

        // Shorter prefix must truncate, not leave the tail of the previous one.
        opts.set_prefix_range_in_place(b"b");
        assert_eq!(opts.iterate_lower_bound.as_deref(), Some(&b"b"[..]));
        assert_eq!(opts.iterate_upper_bound.as_deref(), Some(&b"c"[..]));

        // An all-0xff prefix has no successor: the upper bound must be cleared.
        opts.set_prefix_range_in_place(b"\xff");
        assert_eq!(opts.iterate_lower_bound.as_deref(), Some(&b"\xff"[..]));
        assert_eq!(opts.iterate_upper_bound, None);

        // An empty prefix is the full range: both bounds cleared.
        opts.set_prefix_range_in_place(b"");
        assert_eq!(opts.iterate_lower_bound, None);
        assert_eq!(opts.iterate_upper_bound, None);
    }

    #[test]
    fn test_enable_statistics() {
        let mut opts = Options::default();
        assert_eq!(None, opts.get_statistics());
        opts.enable_statistics();
        opts.set_stats_dump_period_sec(60);
        assert!(opts.get_statistics().is_some());

        let opts = Options::default();
        assert!(opts.get_statistics().is_none());
    }

    #[test]
    fn test_set_memtable_factory() {
        let mut opts = Options::default();
        opts.set_memtable_factory(MemtableFactory::Vector);
        opts.set_memtable_factory(MemtableFactory::HashLinkList { bucket_count: 100 });
        opts.set_memtable_factory(MemtableFactory::HashSkipList {
            bucket_count: 100,
            height: 4,
            branching_factor: 4,
        });
    }

    #[test]
    fn test_use_fsync() {
        let mut opts = Options::default();
        assert!(!opts.get_use_fsync());
        opts.set_use_fsync(true);
        assert!(opts.get_use_fsync());
    }

    #[test]
    fn test_set_stats_persist_period_sec() {
        let mut opts = Options::default();
        opts.enable_statistics();
        opts.set_stats_persist_period_sec(5);
        assert!(opts.get_statistics().is_some());

        let opts = Options::default();
        assert!(opts.get_statistics().is_none());
    }

    #[test]
    fn test_set_write_buffer_manager() {
        let mut opts = Options::default();
        let lrucache = Cache::new_lru_cache(100);
        let write_buffer_manager =
            WriteBufferManager::new_write_buffer_manager_with_cache(100, false, lrucache);
        assert_eq!(write_buffer_manager.get_buffer_size(), 100);
        assert_eq!(write_buffer_manager.get_usage(), 0);
        assert!(write_buffer_manager.enabled());

        opts.set_write_buffer_manager(&write_buffer_manager);
        drop(opts);

        // WriteBufferManager outlives options
        assert!(write_buffer_manager.enabled());
    }

    #[test]
    fn compaction_pri() {
        let mut opts = Options::default();
        opts.set_compaction_pri(DBCompactionPri::RoundRobin);
        opts.create_if_missing(true);
        let tmp = tempfile::tempdir().unwrap();
        let _db = crate::DB::open(&opts, tmp.path()).unwrap();

        let options = std::fs::read_dir(tmp.path())
            .unwrap()
            .find_map(|x| {
                let x = x.ok()?;
                x.file_name()
                    .into_string()
                    .unwrap()
                    .contains("OPTIONS")
                    .then_some(x.path())
            })
            .map(std::fs::read_to_string)
            .unwrap()
            .unwrap();

        assert!(options.contains("compaction_pri=kRoundRobin"));
    }

    #[test]
    fn test_callback_logger() {
        let (log_snd, log_rcv) = std::sync::mpsc::channel();
        let callback = move |level, msg: &str| {
            log_snd.send((level, msg.to_string())).ok();
        };

        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.set_info_logger(InfoLogger::new_callback_logger(
            super::LogLevel::Debug,
            callback,
        ));

        // create 2 DBs with the options then drop the options to ensure it is reference counted
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::DB::open(&opts, tmp.path()).unwrap();
        db.put(b"testkey", b"testvalue").unwrap();
        db.flush().unwrap();
        db.delete(b"testkey").unwrap();
        db.flush().unwrap();
        db.compact_range(Some(b"a"), Some(b"z"));
        assert!(log_rcv.try_recv().is_ok());
        drop(db);

        let tmp2 = tempfile::tempdir().unwrap();
        let db2 = crate::DB::open(&opts, tmp2.path()).unwrap();

        // get the configured logger before dropping the options
        let logger = opts.get_info_logger();
        drop(opts);

        // clear the logs and make sure the callback is called by db2
        while log_rcv.try_recv().is_ok() {}
        assert!(log_rcv.try_recv().is_err());

        db2.put(b"testkey2", b"testvalue2").unwrap();
        db2.flush().unwrap();
        db2.delete(b"testkey2").unwrap();
        db2.flush().unwrap();
        db2.compact_range(Some(b"a"), Some(b"z"));

        drop(db2);
        assert!(log_rcv.try_recv().is_ok());

        // clear the logs
        while log_rcv.try_recv().is_ok() {}
        assert!(log_rcv.try_recv().is_err());

        // create a db with the copied logger to check lifetimes
        let tmp3 = tempfile::tempdir().unwrap();
        let mut opts2 = Options::default();
        opts2.create_if_missing(true);
        opts2.set_info_logger(logger);
        let db3 = crate::DB::open(&opts2, tmp3.path()).unwrap();
        drop(opts2);
        db3.put(b"testkey3", b"testvalue3").unwrap();
        db3.flush().unwrap();
        db3.delete(b"testkey3").unwrap();
        db3.flush().unwrap();
        db3.compact_range(Some(b"a"), Some(b"z"));
        assert!(log_rcv.try_recv().is_ok());
        drop(db3);
    }
}

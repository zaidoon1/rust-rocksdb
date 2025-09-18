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

use crate::comparator::{
    ComparatorCallback, ComparatorWithTsCallback, CompareFn, CompareTsFn, CompareWithoutTsFn,
};
use crate::db_options::BlockBasedOptions;
use crate::db_options::{
    CuckooTableOptions, DBCompactionPri, DBCompactionStyle, DBCompressionType, FifoCompactOptions,
    MemtableFactory, OptionsMustOutliveDB, PlainTableFactoryOptions, UniversalCompactOptions,
};
use crate::ffi;
use crate::{
    compaction_filter::{self, CompactionFilterCallback, CompactionFilterFn},
    compaction_filter_factory::{self, CompactionFilterFactory},
    ffi_util::CStrLike,
    merge_operator::{
        self, full_merge_callback, partial_merge_callback, MergeFn, MergeOperatorCallback,
    },
    slice_transform::SliceTransform,
};
use libc::{c_char, c_int, c_uchar, c_void, size_t};

/// Column Family-level options.
pub struct ColumnFamilyOptions {
    pub(crate) inner: *mut ffi::rocksdb_options_t,
    pub(crate) outlive: OptionsMustOutliveDB,
}

impl Default for ColumnFamilyOptions {
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

impl Clone for ColumnFamilyOptions {
    fn clone(&self) -> Self {
        let inner = unsafe { ffi::rocksdb_options_create_copy(self.inner) };
        assert!(!inner.is_null(), "Could not copy RocksDB options");

        Self {
            inner,
            outlive: self.outlive.clone(),
        }
    }
}

impl Drop for ColumnFamilyOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_options_destroy(self.inner);
        }
    }
}

unsafe impl Send for ColumnFamilyOptions {}
unsafe impl Sync for ColumnFamilyOptions {}

impl ColumnFamilyOptions {
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

        unsafe {
            let cmp = ffi::rocksdb_comparator_create(
                Box::into_raw(cb).cast::<c_void>(),
                Some(ComparatorCallback::destructor_callback),
                Some(ComparatorCallback::compare_callback),
                Some(ComparatorCallback::name_callback),
            );
            ffi::rocksdb_options_set_comparator(self.inner, cmp);
        }
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

        unsafe {
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
        }
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

        unsafe {
            let cf = ffi::rocksdb_compactionfilter_create(
                Box::into_raw(cb).cast::<c_void>(),
                Some(compaction_filter::destructor_callback::<CompactionFilterCallback<F>>),
                Some(compaction_filter::filter_callback::<CompactionFilterCallback<F>>),
                Some(compaction_filter::name_callback::<CompactionFilterCallback<F>>),
            );
            ffi::rocksdb_options_set_compaction_filter(self.inner, cf);
        }
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
    /// use rust_rocksdb::ColumnFamilyOptions;
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_min_write_buffer_number(2);
    /// ```
    pub fn set_min_write_buffer_number(&mut self, nbuf: c_int) {
        unsafe {
            ffi::rocksdb_options_set_min_write_buffer_number_to_merge(self.inner, nbuf);
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
    /// use rust_rocksdb::ColumnFamilyOptions;
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_min_write_buffer_number_to_merge(2);
    /// ```
    pub fn set_min_write_buffer_number_to_merge(&mut self, to_merge: c_int) {
        unsafe {
            ffi::rocksdb_options_set_min_write_buffer_number_to_merge(self.inner, to_merge);
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
    /// use rust_rocksdb::ColumnFamilyOptions;
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_max_write_buffer_number(4);
    /// ```
    pub fn set_max_write_buffer_number(&mut self, nbuf: c_int) {
        unsafe {
            ffi::rocksdb_options_set_max_write_buffer_number(self.inner, nbuf);
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
    /// use rust_rocksdb::ColumnFamilyOptions;
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_level_zero_slowdown_writes_trigger(10);
    /// ```
    pub fn set_level_zero_slowdown_writes_trigger(&mut self, n: c_int) {
        unsafe {
            ffi::rocksdb_options_set_level0_slowdown_writes_trigger(self.inner, n);
        }
    }

    /// Sets the maximum number of level-0 files.  We stop writes at this point.
    ///
    /// Default: `24`
    ///
    /// Dynamically changeable through SetOptions() API
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::ColumnFamilyOptions;
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_level_zero_stop_writes_trigger(48);
    /// ```
    pub fn set_level_zero_stop_writes_trigger(&mut self, n: c_int) {
        unsafe {
            ffi::rocksdb_options_set_level0_stop_writes_trigger(self.inner, n);
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
    /// use rust_rocksdb::ColumnFamilyOptions;
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_level_zero_file_num_compaction_trigger(8);
    /// ```
    pub fn set_level_zero_file_num_compaction_trigger(&mut self, n: c_int) {
        unsafe {
            ffi::rocksdb_options_set_level0_file_num_compaction_trigger(self.inner, n);
        }
    }

    /// Sets the prefix extractor for this column family.
    ///
    /// Enables building prefix-based bloom filters and prefix-aware queries. The prefix extractor
    /// defines how to derive a prefix from keys. Must be consistent across DB opens.
    pub fn set_prefix_extractor(&mut self, prefix_extractor: SliceTransform) {
        unsafe {
            ffi::rocksdb_options_set_prefix_extractor(self.inner, prefix_extractor.inner);
        }
    }

    /// Sets an associative (only full-merge) merge operator for this column family.
    ///
    /// Use when the merge operation is associative (e.g., string append, numeric sum) so that
    /// partial merges can be treated as full merges. The merge operator is invoked during reads
    /// and compactions to combine multiple updates for the same key.
    pub fn set_merge_operator_associative<F: MergeFn + Clone>(
        &mut self,
        name: impl CStrLike,
        full_merge_fn: F,
    ) {
        let cb = Box::new(MergeOperatorCallback::<F, F> {
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

    /// Sets a merge operator supporting both full and partial merge for this column family.
    ///
    /// Provide both full-merge and partial-merge callbacks. RocksDB may call partial merges to
    /// combine successive operands without needing the current value. Full merge gets the existing
    /// value when available and all operands.
    pub fn set_merge_operator<F: MergeFn, PF: MergeFn>(
        &mut self,
        name: impl CStrLike,
        full_merge_fn: F,
        partial_merge_fn: PF,
    ) {
        let cb = Box::new(MergeOperatorCallback::<F, PF> {
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

    /// Sets the compression algorithm that will be used for compressing blocks.
    ///
    /// Default: `DBCompressionType::Snappy` (`DBCompressionType::None` if
    /// snappy feature is not enabled).
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{ColumnFamilyOptions, DBCompressionType};
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_compression_type(DBCompressionType::Snappy);
    /// ```
    pub fn set_compression_type(&mut self, t: DBCompressionType) {
        unsafe {
            ffi::rocksdb_options_set_compression(self.inner, t as c_int);
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
    /// use rust_rocksdb::{ColumnFamilyOptions, DBCompressionType};
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_bottommost_compression_type(DBCompressionType::Zstd);
    /// opts.set_bottommost_zstd_max_train_bytes(0, true);
    /// ```
    pub fn set_bottommost_compression_type(&mut self, t: DBCompressionType) {
        unsafe {
            ffi::rocksdb_options_set_bottommost_compression(self.inner, t as c_int);
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
    /// use rust_rocksdb::{ColumnFamilyOptions, DBCompressionType};
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_compression_type(DBCompressionType::Zstd);
    /// opts.set_compression_options_parallel_threads(3);
    /// ```
    pub fn set_compression_options_parallel_threads(&mut self, num: i32) {
        unsafe {
            ffi::rocksdb_options_set_compression_options_parallel_threads(self.inner, num);
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
    /// use rust_rocksdb::{ColumnFamilyOptions, DBCompressionType};
    ///
    /// let mut opts = ColumnFamilyOptions::default();
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
    /// use rust_rocksdb::ColumnFamilyOptions;
    ///
    /// let mut opts = ColumnFamilyOptions::default();
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
    /// use rust_rocksdb::{ColumnFamilyOptions, DBCompressionType};
    ///
    /// let mut opts = ColumnFamilyOptions::default();
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

    /// Sets the compaction style.
    ///
    /// Default: DBCompactionStyle::Level
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{ColumnFamilyOptions, DBCompactionStyle};
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_compaction_style(DBCompactionStyle::Universal);
    /// ```
    pub fn set_compaction_style(&mut self, style: DBCompactionStyle) {
        unsafe {
            ffi::rocksdb_options_set_compaction_style(self.inner, style as c_int);
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
    /// use rust_rocksdb::{ColumnFamilyOptions, DBCompactionPri};
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_compaction_pri(DBCompactionPri::RoundRobin);
    /// ```
    pub fn set_compaction_pri(&mut self, pri: DBCompactionPri) {
        unsafe {
            ffi::rocksdb_options_set_compaction_pri(self.inner, pri as c_int);
        }
    }

    /// Sets the options needed to support Universal style compactions for this column family.
    ///
    /// See RocksDB docs on Universal compaction for details. These options are only used
    /// when `compaction_style` is set to `DBCompactionStyle::Universal`.
    pub fn set_universal_compaction_options(&mut self, uco: &UniversalCompactOptions) {
        unsafe {
            ffi::rocksdb_options_set_universal_compaction_options(self.inner, uco.inner);
        }
    }

    /// Sets the options for FIFO compaction style for this column family.
    ///
    /// See RocksDB docs on FIFO compaction for details. These options are only used
    /// when `compaction_style` is set to `DBCompactionStyle::Fifo`.
    pub fn set_fifo_compaction_options(&mut self, fco: &FifoCompactOptions) {
        unsafe {
            ffi::rocksdb_options_set_fifo_compaction_options(self.inner, fco.inner);
        }
    }

    /// If true, RocksDB will pick target size of each level dynamically.
    /// We will pick a base level b >= 1. L0 will be directly merged into level b,
    /// instead of always into level 1. Level 1 to b-1 need to be empty.
    /// We try to pick b and its target size so that
    /// 1. target size is in the range of
    ///    (max_bytes_for_level_base / max_bytes_for_level_multiplier,
    ///    max_bytes_for_level_base]
    /// 2. target size of the last level (level num_levels-1) equals to the max
    ///    size of a level in the LSM (typically the last level).
    ///
    /// At the same time max_bytes_for_level_multiplier is still satisfied.
    /// Note that max_bytes_for_level_multiplier_additional is ignored with this
    /// flag on.
    ///
    /// With this option on, from an empty DB, we make last level the base level,
    /// which means merging L0 data into the last level, until it exceeds
    /// max_bytes_for_level_base. And then we make the second last level to be
    /// base level, to start to merge L0 data to second last level, with its
    /// target size to be 1/max_bytes_for_level_multiplier of the last level's
    /// extra size. After the data accumulates more so that we need to move the
    /// base level to the third last one, and so on.
    ///
    /// For example, assume max_bytes_for_level_multiplier=10, num_levels=6,
    /// and max_bytes_for_level_base=10MB.
    /// Target sizes of level 1 to 5 starts with:
    /// [- - - - 10MB]
    /// with base level is level 5. Target sizes of level 1 to 4 are not applicable
    /// because they will not be used.
    /// Until the size of Level 5 grows to more than 10MB, say 11MB, we make
    /// base target to level 4 and now the targets looks like:
    /// [- - - 1.1MB 11MB]
    /// While data are accumulated, size targets are tuned based on actual data
    /// of level 5. When level 5 has 50MB of data, the target is like:
    /// [- - - 5MB 50MB]
    /// Until level 5's actual size is more than 100MB, say 101MB. Now if we keep
    /// level 4 to be the base level, its target size needs to be 10.1MB, which
    /// doesn't satisfy the target size range. So now we make level 3 the target
    /// size and the target sizes of the levels look like:
    /// [- - 1.01MB 10.1MB 101MB]
    /// In the same way, while level 5 further grows, all levels' targets grow,
    /// like
    /// [- - 5MB 50MB 500MB]
    /// Until level 5 exceeds 1000MB and becomes 1001MB, we make level 2 the
    /// base level and make levels' target sizes like this:
    /// [- 1.001MB 10.01MB 100.1MB 1001MB]
    /// and go on...
    ///
    /// By doing it, we give max_bytes_for_level_multiplier a priority against
    /// max_bytes_for_level_base, for a more predictable LSM tree shape. It is
    /// useful to limit worse case space amplification.
    /// If `allow_ingest_behind=true` or `preclude_last_level_data_seconds > 0`,
    /// then the last level is reserved, and we will start filling LSM from the
    /// second last level.
    ///
    /// With this option on, compaction is more adaptive to write traffic:
    /// Compaction priority will take into account estimated bytes to be compacted
    /// down to a level and favors compacting lower levels when there is a write
    /// traffic spike (and hence more compaction debt). Refer to
    /// <https://github.com/facebook/rocksdb/wiki/Leveled-Compactio#option-level_compaction_dynamic_level_bytes-and-levels-target-size>
    /// for more detailed description. See more implementation detail in:
    /// VersionStorageInfo::ComputeCompactionScore().
    ///
    /// With this option on, unneeded levels will be drained automatically:
    /// Note that there may be excessive levels (where target level size is 0 when
    /// computed based on this feature) in the LSM. This can happen after a user
    /// migrates to turn this feature on or deletes a lot of data. This is
    /// especially likely when a user migrates from leveled compaction with a
    /// smaller multiplier or from universal compaction. RocksDB will gradually
    /// drain these unnecessary levels by compacting files down the LSM. Smaller
    /// number of levels should help to reduce read amplification.
    ///
    /// Default: true
    pub fn set_level_compaction_dynamic_level_bytes(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_options_set_level_compaction_dynamic_level_bytes(
                self.inner,
                c_uchar::from(v),
            );
        }
    }

    /// Sets the number of levels for this column family.
    ///
    /// Default: 7
    pub fn set_num_levels(&mut self, n: c_int) {
        unsafe {
            ffi::rocksdb_options_set_num_levels(self.inner, n);
        }
    }

    /// The manifest file is rolled over on reaching this limit.
    /// The older manifest file be deleted.
    /// The default value is MAX_INT so that roll-over does not take place.
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
    /// use rust_rocksdb::ColumnFamilyOptions;
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_target_file_size_base(128 * 1024 * 1024);
    /// ```
    pub fn set_target_file_size_base(&mut self, size: u64) {
        unsafe {
            ffi::rocksdb_options_set_target_file_size_base(self.inner, size);
        }
    }

    /// Sets the target file size multiplier across levels.
    /// By default, files in different levels will have similar size.
    ///
    /// Dynamically changeable via SetOptions.
    pub fn set_target_file_size_multiplier(&mut self, multiplier: i32) {
        unsafe {
            ffi::rocksdb_options_set_target_file_size_multiplier(self.inner, multiplier as c_int);
        }
    }

    /// Sets the maximum number of bytes in all compacted files for a single compaction run.
    /// We try to limit number of bytes in one compaction to be lower than this threshold.
    ///
    /// Value 0 will be sanitized.
    /// Default: `target_file_size_base * 25`.
    pub fn set_max_compaction_bytes(&mut self, nbytes: u64) {
        unsafe {
            ffi::rocksdb_options_set_max_compaction_bytes(self.inner, nbytes);
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
    /// use rust_rocksdb::ColumnFamilyOptions;
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_write_buffer_size(128 * 1024 * 1024);
    /// ```
    pub fn set_write_buffer_size(&mut self, size: usize) {
        unsafe {
            ffi::rocksdb_options_set_write_buffer_size(self.inner, size);
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
    /// use rust_rocksdb::ColumnFamilyOptions;
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_max_bytes_for_level_base(512 * 1024 * 1024);
    /// ```
    pub fn set_max_bytes_for_level_base(&mut self, size: u64) {
        unsafe {
            ffi::rocksdb_options_set_max_bytes_for_level_base(self.inner, size);
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
    /// use rust_rocksdb::ColumnFamilyOptions;
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_disable_auto_compactions(true);
    /// ```
    pub fn set_disable_auto_compactions(&mut self, disable: bool) {
        unsafe {
            ffi::rocksdb_options_set_disable_auto_compactions(self.inner, c_int::from(disable));
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
    /// use rust_rocksdb::{ColumnFamilyOptions, SliceTransform};
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// let transform = SliceTransform::create_fixed_prefix(10);
    /// opts.set_prefix_extractor(transform);
    /// opts.set_memtable_prefix_bloom_ratio(0.2);
    /// ```
    pub fn set_memtable_prefix_bloom_ratio(&mut self, ratio: f64) {
        unsafe {
            ffi::rocksdb_options_set_memtable_prefix_bloom_size_ratio(self.inner, ratio);
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

    /// Defines the underlying memtable implementation.
    /// See official [wiki](https://github.com/facebook/rocksdb/wiki/MemTable) for more information.
    /// Defaults to using a skiplist.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{ColumnFamilyOptions, MemtableFactory};
    /// let mut opts = ColumnFamilyOptions::default();
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
            MemtableFactory::HashLinkList { bucket_count } => unsafe {
                ffi::rocksdb_options_set_hash_link_list_rep(self.inner, bucket_count);
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
    /// Default: 10000 when `inplace_update_support = true`, otherwise 0.
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

    /// Default: `10`
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::ColumnFamilyOptions;
    ///
    /// let mut opts = ColumnFamilyOptions::default();
    /// opts.set_max_bytes_for_level_multiplier(4.0);
    /// ```
    pub fn set_max_bytes_for_level_multiplier(&mut self, mul: f64) {
        unsafe {
            ffi::rocksdb_options_set_max_bytes_for_level_multiplier(self.inner, mul);
        }
    }

    /// Sets the start level to use compression.
    pub fn set_min_level_to_compress(&mut self, lvl: c_int) {
        unsafe {
            ffi::rocksdb_options_set_min_level_to_compress(self.inner, lvl);
        }
    }

    /// Sets the table factory to a BlockBasedTableFactory with provided `BlockBasedOptions`.
    pub fn set_block_based_table_factory(&mut self, factory: &BlockBasedOptions) {
        unsafe {
            ffi::rocksdb_options_set_block_based_table_factory(self.inner, factory.inner);
        }
        // Note: we intentionally do not update `self.outlive.block_based` here because
        // BlockBasedOptions::outlive is private to the db_options module. DB-level
        // method updates outlive to keep caches alive.
    }

    /// Sets the table factory to a CuckooTableFactory (the default table
    /// factory is a block-based table factory that provides a default
    /// implementation of TableBuilder and TableReader with default
    /// BlockBasedTableOptions).
    /// See official [wiki](https://github.com/facebook/rocksdb/wiki/CuckooTable-Format) for more information on this table format.
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{ColumnFamilyOptions, CuckooTableOptions};
    ///
    /// let mut opts = ColumnFamilyOptions::default();
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

    /// This is a factory that provides TableFactory objects.
    /// Default: a block-based table factory that provides a default
    /// implementation of TableBuilder and TableReader with default
    /// BlockBasedTableOptions.
    /// Sets the factory as plain table.
    /// See official [wiki](https://github.com/facebook/rocksdb/wiki/PlainTable-Format) for more
    /// information.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_rocksdb::{KeyEncodingType, ColumnFamilyOptions, PlainTableFactoryOptions};
    ///
    /// let mut opts = ColumnFamilyOptions::default();
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

    /// A factory of a table property collector that marks an SST
    /// file as need-compaction when it observe at least "D" deletion
    /// entries in any "N" consecutive entries, or the ratio of tombstone
    /// entries >= deletion_ratio.
    ///
    /// `window_size`: is the sliding window size "N"
    /// `num_dels_trigger`: is the deletion trigger "D"
    /// `deletion_ratio`: if <= 0 or > 1, disable triggering compaction based on
    /// deletion ratio.
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
}

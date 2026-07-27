// Copyright 2020 Tran Tuan Linh
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

use libc::{c_int, c_uchar};
use std::{
    cell::{Cell, RefCell},
    marker::PhantomData,
};

use crate::cache::Cache;
use crate::ffi_util::from_cstr_and_free;
use crate::{DB, DBCommon, ThreadMode, TransactionDB};
use crate::{Error, db::DBInner, ffi};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(i32)]
pub enum PerfStatsLevel {
    /// Unknown settings
    Uninitialized = 0,
    /// Disable perf stats
    Disable = 1,
    /// Enables only count stats
    EnableCount = 2,
    /// Enables count stats and time spent waiting for RocksDB work
    EnableWait = 3,
    /// Count stats and enable time stats except for mutexes
    EnableTimeExceptForMutex = 4,
    /// Other than time, also measure CPU time counters. Still don't measure
    /// time (neither wall time nor CPU time) for mutexes
    EnableTimeAndCPUTimeExceptForMutex = 5,
    /// Enables count and time stats
    EnableTime = 6,
    /// N.B must always be the last value!
    OutOfBound = 7,
}

// Include the generated PerfMetric enum from perf_enum.rs
include!("perf_enum.rs");

/// Sets the perf stats level for current thread.
pub fn set_perf_stats(lvl: PerfStatsLevel) {
    unsafe {
        ffi::rocksdb_set_perf_level(lvl as c_int);
    }
}

/// Thread local context for gathering performance counter efficiently
/// and transparently.
pub struct PerfContext {
    pub(crate) inner: *mut ffi::rocksdb_perfcontext_t,
    reusable: bool,
}

thread_local! {
    static ACTIVE_MANUAL_PERF_CONTEXTS: Cell<usize> = const { Cell::new(0) };
    static REUSABLE_PERF_CONTEXT: RefCell<PerfContext> = RefCell::new({
        let inner = unsafe { ffi::rocksdb_perfcontext_create() };
        assert!(!inner.is_null(), "Could not create Perf Context");
        PerfContext {
            inner,
            reusable: true,
        }
    });
}

impl Default for PerfContext {
    fn default() -> Self {
        let ctx = unsafe { ffi::rocksdb_perfcontext_create() };
        assert!(!ctx.is_null(), "Could not create Perf Context");
        ACTIVE_MANUAL_PERF_CONTEXTS.with(|count| count.set(count.get() + 1));

        Self {
            inner: ctx,
            reusable: false,
        }
    }
}

impl Drop for PerfContext {
    fn drop(&mut self) {
        if !self.reusable {
            ACTIVE_MANUAL_PERF_CONTEXTS.with(|count| count.set(count.get() - 1));
        }
        unsafe {
            ffi::rocksdb_perfcontext_destroy(self.inner);
        }
    }
}

impl PerfContext {
    /// Reset context
    pub fn reset(&mut self) {
        unsafe {
            ffi::rocksdb_perfcontext_reset(self.inner);
        }
    }

    /// Get the report on perf
    pub fn report(&self, exclude_zero_counters: bool) -> String {
        unsafe {
            let ptr =
                ffi::rocksdb_perfcontext_report(self.inner, c_uchar::from(exclude_zero_counters));
            from_cstr_and_free(ptr)
        }
    }

    /// Returns value of a metric
    pub fn metric(&self, id: PerfMetric) -> u64 {
        unsafe { ffi::rocksdb_perfcontext_metric(self.inner, id as c_int) }
    }
}

/// Runs `f` with a reusable thread-local [`PerfContext`].
///
/// The context is reset before each call. This avoids allocating and destroying
/// the C wrapper for every measurement.
///
/// # Panics
///
/// Panics if called reentrantly or while a manually created [`PerfContext`] is
/// alive on the same thread. RocksDB returns the same underlying context to all
/// wrappers on a thread, so resetting it would discard the manual measurement.
pub fn with_thread_local<F, R>(f: F) -> R
where
    F: FnOnce(&mut PerfContext) -> R,
{
    ACTIVE_MANUAL_PERF_CONTEXTS.with(|count| {
        assert_eq!(
            count.get(),
            0,
            "with_thread_local cannot run while a manual PerfContext is alive on the same thread"
        );
    });
    REUSABLE_PERF_CONTEXT.with(|ctx| {
        let mut ctx = ctx.try_borrow_mut().unwrap_or_else(|_| {
            panic!("with_thread_local cannot be called reentrantly on the same thread")
        });
        ctx.reset();
        f(&mut ctx)
    })
}

/// Memory usage stats
pub struct MemoryUsageStats {
    /// Approximate memory usage of all the mem-tables
    pub mem_table_total: u64,
    /// Approximate memory usage of un-flushed mem-tables
    pub mem_table_unflushed: u64,
    /// Approximate memory usage of all the table readers
    pub mem_table_readers_total: u64,
    /// Approximate memory usage by cache
    pub cache_total: u64,
}

/// Wrap over memory_usage_t. Hold current memory usage of the specified DB instances and caches
pub struct MemoryUsage {
    inner: *mut ffi::rocksdb_memory_usage_t,
}

impl Drop for MemoryUsage {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_approximate_memory_usage_destroy(self.inner);
        }
    }
}

impl MemoryUsage {
    /// Approximate memory usage of all the mem-tables
    pub fn approximate_mem_table_total(&self) -> u64 {
        unsafe { ffi::rocksdb_approximate_memory_usage_get_mem_table_total(self.inner) }
    }

    /// Approximate memory usage of un-flushed mem-tables
    pub fn approximate_mem_table_unflushed(&self) -> u64 {
        unsafe { ffi::rocksdb_approximate_memory_usage_get_mem_table_unflushed(self.inner) }
    }

    /// Approximate memory usage of all the table readers
    pub fn approximate_mem_table_readers_total(&self) -> u64 {
        unsafe { ffi::rocksdb_approximate_memory_usage_get_mem_table_readers_total(self.inner) }
    }

    /// Approximate memory usage by cache
    pub fn approximate_cache_total(&self) -> u64 {
        unsafe { ffi::rocksdb_approximate_memory_usage_get_cache_total(self.inner) }
    }
}

/// Creates [`MemoryUsage`] from DBs and caches.
///
/// Most users should call [`get_memory_usage_stats`] instead.
///
/// A `MemoryUsageBuilder` must not outlive the `DB`s added to it:
///
/// ```compile_fail,E0597
/// use rust_rocksdb::{perf::MemoryUsageBuilder, DB};
///
/// let mut builder = MemoryUsageBuilder::new().unwrap();
/// {
///     let db = DB::open_default("foo").unwrap();
///     builder.add_db(&db);
/// }
/// let _memory_usage = builder.build().unwrap();
/// ```
pub struct MemoryUsageBuilder<'a> {
    inner: *mut ffi::rocksdb_memory_consumers_t,
    base_dbs: Vec<*mut ffi::rocksdb_t>,
    // must not outlive the DBs/caches that are added
    _marker: PhantomData<&'a ()>,
}

impl Drop for MemoryUsageBuilder<'_> {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_memory_consumers_destroy(self.inner);
        }
        for base_db in &self.base_dbs {
            unsafe {
                ffi::rocksdb_transactiondb_close_base_db(*base_db);
            }
        }
    }
}

impl<'a> MemoryUsageBuilder<'a> {
    /// Create new instance
    pub fn new() -> Result<Self, Error> {
        let mc = unsafe { ffi::rocksdb_memory_consumers_create() };
        if mc.is_null() {
            Err(Error::new(
                "Could not create MemoryUsage builder".to_owned(),
            ))
        } else {
            Ok(Self {
                inner: mc,
                base_dbs: Vec::new(),
                _marker: PhantomData,
            })
        }
    }

    /// Add a DB instance to collect memory usage from it and add up in total stats
    pub fn add_tx_db<T: ThreadMode>(&mut self, db: &'a TransactionDB<T>) {
        unsafe {
            let base_db = ffi::rocksdb_transactiondb_get_base_db(db.inner);
            ffi::rocksdb_memory_consumers_add_db(self.inner, base_db);
            // rocksdb_transactiondb_get_base_db allocates a struct that must be freed
            self.base_dbs.push(base_db);
        }
    }

    /// Add a DB instance to collect memory usage from it and add up in total stats
    pub fn add_db<T: ThreadMode, D: DBInner>(&mut self, db: &'a DBCommon<T, D>) {
        unsafe {
            ffi::rocksdb_memory_consumers_add_db(self.inner, db.inner.inner());
        }
    }

    /// Add a cache to collect memory usage from it and add up in total stats
    pub fn add_cache(&mut self, cache: &'a Cache) {
        unsafe {
            ffi::rocksdb_memory_consumers_add_cache(self.inner, cache.0.inner.as_ptr());
        }
    }

    /// Build up MemoryUsage
    pub fn build(&self) -> Result<MemoryUsage, Error> {
        unsafe {
            let mu = ffi_try!(ffi::rocksdb_approximate_memory_usage_create(self.inner));
            Ok(MemoryUsage { inner: mu })
        }
    }
}

/// Get memory usage stats from DB instances and Cache instances
pub fn get_memory_usage_stats(
    dbs: Option<&[&DB]>,
    caches: Option<&[&Cache]>,
) -> Result<MemoryUsageStats, Error> {
    let mut builder = MemoryUsageBuilder::new()?;
    if let Some(dbs_) = dbs {
        for db in dbs_ {
            builder.add_db(db);
        }
    }
    if let Some(caches_) = caches {
        for cache in caches_ {
            builder.add_cache(cache);
        }
    }

    let mu = builder.build()?;
    Ok(MemoryUsageStats {
        mem_table_total: mu.approximate_mem_table_total(),
        mem_table_unflushed: mu.approximate_mem_table_unflushed(),
        mem_table_readers_total: mu.approximate_mem_table_readers_total(),
        cache_total: mu.approximate_cache_total(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DB, Options};
    use std::panic;
    use tempfile::TempDir;

    #[test]
    fn perf_stats_level_matches_rocksdb_11_1_2() {
        assert_eq!(PerfStatsLevel::Uninitialized as i32, 0);
        assert_eq!(PerfStatsLevel::Disable as i32, 1);
        assert_eq!(PerfStatsLevel::EnableCount as i32, 2);
        assert_eq!(PerfStatsLevel::EnableWait as i32, 3);
        assert_eq!(PerfStatsLevel::EnableTimeExceptForMutex as i32, 4);
        assert_eq!(PerfStatsLevel::EnableTimeAndCPUTimeExceptForMutex as i32, 5);
        assert_eq!(PerfStatsLevel::EnableTime as i32, 6);
        assert_eq!(PerfStatsLevel::OutOfBound as i32, 7);
    }

    #[test]
    fn with_thread_local_reuses_context_after_unwind() {
        let first = with_thread_local(|ctx| ctx.inner);
        let panic_result = panic::catch_unwind(|| {
            with_thread_local(|_| panic!("test panic"));
        });
        assert!(panic_result.is_err());
        let second = with_thread_local(|ctx| ctx.inner);

        assert_eq!(first, second);
    }

    #[test]
    fn with_thread_local_resets_context_before_use() {
        let temp_dir = TempDir::new().unwrap();
        let mut opts = Options::default();
        opts.create_if_missing(true);
        let db = DB::open(&opts, temp_dir.path()).unwrap();
        db.put(b"key", b"value").unwrap();

        set_perf_stats(PerfStatsLevel::EnableCount);
        let comparison_count = with_thread_local(|ctx| {
            db.get(b"key").unwrap();
            ctx.metric(PerfMetric::UserKeyComparisonCount)
        });
        assert!(comparison_count > 0);
        assert_eq!(
            with_thread_local(|ctx| ctx.metric(PerfMetric::UserKeyComparisonCount)),
            0
        );
        set_perf_stats(PerfStatsLevel::Disable);
    }

    #[test]
    #[should_panic(expected = "with_thread_local cannot be called reentrantly on the same thread")]
    fn with_thread_local_rejects_reentrant_use() {
        with_thread_local(|_| with_thread_local(|_| ()));
    }

    #[test]
    #[should_panic(
        expected = "with_thread_local cannot run while a manual PerfContext is alive on the same thread"
    )]
    fn with_thread_local_rejects_active_manual_context() {
        let _manual = PerfContext::default();
        with_thread_local(|_| ());
    }

    #[test]
    fn test_perf_context_with_db_operations() {
        let temp_dir = TempDir::new().unwrap();
        let mut opts = Options::default();
        opts.create_if_missing(true);
        let db = DB::open(&opts, temp_dir.path()).unwrap();

        // Insert data with deletions to test internal key/delete skipping
        let n = 10;
        for i in 0..n {
            let k = vec![i as u8];
            db.put(&k, &k).unwrap();
            if i % 2 == 0 {
                db.delete(&k).unwrap();
            }
        }

        set_perf_stats(PerfStatsLevel::EnableCount);
        let mut ctx = PerfContext::default();

        // Use iterator with explicit seek to trigger metrics
        let mut iter = db.raw_iterator();
        iter.seek_to_first();
        let mut valid_count = 0;
        while iter.valid() {
            valid_count += 1;
            iter.next();
        }

        // Check counts - should have 5 valid entries (odd numbers: 1,3,5,7,9)
        assert_eq!(
            valid_count, 5,
            "Iterator should find 5 valid entries (odd numbers)"
        );

        // Check internal skip metrics
        let internal_key_skipped = ctx.metric(PerfMetric::InternalKeySkippedCount);
        let internal_delete_skipped = ctx.metric(PerfMetric::InternalDeleteSkippedCount);

        // In RocksDB, when iterating over deleted keys in SST files:
        // - We should skip the deletion markers (n/2 = 5 deletes)
        // - Total internal keys skipped should be >= number of deletions
        assert!(
            internal_key_skipped >= (n / 2) as u64,
            "internal_key_skipped ({}) should be >= {} (deletions)",
            internal_key_skipped,
            n / 2
        );
        assert_eq!(
            internal_delete_skipped,
            (n / 2) as u64,
            "internal_delete_skipped ({internal_delete_skipped}) should equal {} (deleted entries)",
            n / 2
        );
        assert_eq!(
            ctx.metric(PerfMetric::SeekInternalSeekTime),
            0,
            "Time metrics should be 0 with EnableCount"
        );

        // Test reset
        ctx.reset();
        assert_eq!(ctx.metric(PerfMetric::InternalKeySkippedCount), 0);
        assert_eq!(ctx.metric(PerfMetric::InternalDeleteSkippedCount), 0);

        // Change perf level to EnableTime
        set_perf_stats(PerfStatsLevel::EnableTime);

        // Iterate backwards
        let mut iter = db.raw_iterator();
        iter.seek_to_last();
        let mut backward_count = 0;
        while iter.valid() {
            backward_count += 1;
            iter.prev();
        }
        assert_eq!(
            backward_count, 5,
            "Backward iteration should also find 5 valid entries"
        );

        // Check accumulated metrics after second iteration
        let key_skipped_after = ctx.metric(PerfMetric::InternalKeySkippedCount);
        let delete_skipped_after = ctx.metric(PerfMetric::InternalDeleteSkippedCount);

        // After both iterations, we should have accumulated more skipped keys
        assert!(
            key_skipped_after >= internal_key_skipped,
            "After second iteration, internal_key_skipped ({key_skipped_after}) should be >= first iteration ({internal_key_skipped})",
        );
        assert_eq!(
            delete_skipped_after,
            (n / 2) as u64,
            "internal_delete_skipped should still be {} after second iteration",
            n / 2
        );

        // Disable perf stats
        set_perf_stats(PerfStatsLevel::Disable);
    }
}

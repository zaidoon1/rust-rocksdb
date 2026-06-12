// Copyright 2026 Tyler Neely
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
//
//! Smoke tests for the async-IO `MultiGet` path.
//!
//! These tests run in both feature configurations. When the `coroutines`
//! feature is disabled, `async_io=true` parallelizes only within a single
//! LSM level; when enabled, the multi-level coroutine path activates. The
//! tests verify that results are *correct* in both configurations - they do
//! not assert which dispatch branch ran.
//!
//! The multi-level test explicitly populates several LSM levels and asserts
//! the layout via `rocksdb.num-files-at-levelN` before running the
//! MultiGet, so failures from a "MultiGet across multiple levels" bug land
//! in a test that actually has data in multiple levels.

mod util;

use pretty_assertions::assert_eq;
use rust_rocksdb::{
    CompactOptions, DB, Options, ReadOptions, WaitForCompactOptions, WriteOptions,
    properties::num_files_at_level,
};

use util::DBPath;

/// Counts non-empty levels by querying `rocksdb.num-files-at-level{N}` for
/// each level. Returns `(non_empty_count, [files_per_level; max_levels])`.
fn level_layout(db: &DB, max_levels: usize) -> (usize, Vec<u64>) {
    let counts: Vec<u64> = (0..max_levels)
        .map(|lvl| {
            db.property_int_value(num_files_at_level(lvl))
                .unwrap()
                .unwrap_or(0)
        })
        .collect();
    let non_empty = counts.iter().filter(|&&n| n > 0).count();
    (non_empty, counts)
}

/// Verifies that `built_with_coroutines()` agrees with the crate's `coroutines`
/// cargo feature.
///
/// This is intentionally tautological at the source level - the function is
/// implemented as `cfg!(feature = "coroutines")` - but it's not pointless. If
/// the function is ever refactored to read its answer from a runtime symbol
/// (for example, after the upstream PR adding `rocksdb_compiled_with_coroutines`
/// is merged and we bump the submodule), this test would catch a wiring bug
/// where the runtime value disagrees with the build feature.
#[test]
fn built_with_coroutines_matches_feature_flag() {
    assert_eq!(
        rust_rocksdb::built_with_coroutines(),
        cfg!(feature = "coroutines"),
        "built_with_coroutines() must match the cargo feature flag"
    );
}

/// Verifies that `multi_get` with `async_io=true` returns correct results
/// when keys are actually spread across multiple LSM levels. Without an
/// explicit level-layout check, a regression that breaks the multi-level
/// dispatch path could pass silently because the test setup never produced
/// such a layout.
#[test]
fn multi_get_async_io_matches_serial_get_across_levels() {
    let path = DBPath::new("_rust_rocksdb_multi_get_async_io_multi_level");
    {
        // Configure tiny memtables and SSTs so a moderate number of writes
        // forces L0->L1->L2 compactions. Disable auto compactions so the
        // test drives the layout deterministically via `compact_range_opt`.
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.set_write_buffer_size(4 * 1024);
        opts.set_target_file_size_base(4 * 1024);
        opts.set_max_bytes_for_level_base(16 * 1024);
        opts.set_max_bytes_for_level_multiplier(2.0);
        opts.set_num_levels(5);
        opts.set_disable_auto_compactions(true);
        opts.set_level_zero_file_num_compaction_trigger(64); // effectively disabled

        let db = DB::open(&opts, &path).unwrap();
        let mut wo = WriteOptions::default();
        wo.set_sync(false);

        // Three batches, each flushed to its own L0 file, then compacted
        // down one level at a time. After the dance there should be data
        // in at least L0 and one deeper level.
        let value = vec![b'v'; 200];

        // 30-second timeout for each compaction wait. With
        // disable_auto_compactions=true and a high L0 trigger, no background
        // work runs concurrently, but we still call wait_for_compact after
        // each manual compaction as defense-in-depth so the level layout is
        // settled before we measure it.
        let make_wait_opts = || {
            let mut o = WaitForCompactOptions::default();
            o.set_flush(true);
            o.set_timeout(30 * 1_000_000); // 30s in microseconds
            o
        };

        let put_batch = |start: u32, end: u32| {
            for i in start..end {
                let key = format!("key-{i:08}");
                db.put_opt(key.as_bytes(), &value, &wo).unwrap();
            }
            db.flush().unwrap();
        };

        // Batch 1 -> L0 -> compact to L2.
        put_batch(0, 200);
        let mut co = CompactOptions::default();
        co.set_change_level(true);
        co.set_target_level(2);
        db.compact_range_opt::<&[u8], &[u8]>(None, None, &co);
        db.wait_for_compact(&make_wait_opts()).unwrap();

        // Batch 2 -> L0 -> compact to L1.
        put_batch(200, 400);
        let mut co = CompactOptions::default();
        co.set_change_level(true);
        co.set_target_level(1);
        db.compact_range_opt::<&[u8], &[u8]>(None, None, &co);
        db.wait_for_compact(&make_wait_opts()).unwrap();

        // Batch 3 -> stays in L0 (no further compaction).
        put_batch(400, 600);
        db.wait_for_compact(&make_wait_opts()).unwrap();

        let (non_empty, counts) = level_layout(&db, 5);
        assert!(
            non_empty >= 2,
            "test setup failed to spread data across multiple LSM levels: \
             non_empty={non_empty}, counts={counts:?}; \
             coroutines feature: {}",
            cfg!(feature = "coroutines")
        );

        // Build a key batch spanning all three insertion batches plus some
        // misses. Each batch landed in a different level, so a MultiGet
        // across this set forces multi-level lookup.
        let mut keys: Vec<Vec<u8>> = (0..600u32)
            .step_by(37)
            .map(|i| format!("key-{i:08}").into_bytes())
            .collect();
        keys.push(b"key-99999999".to_vec()); // miss
        keys.push(b"key-00001234".to_vec()); // miss

        let reference: Vec<Option<Vec<u8>>> = keys.iter().map(|k| db.get(k).unwrap()).collect();

        let mut ro = ReadOptions::default();
        ro.set_async_io(true);
        let key_refs: Vec<&[u8]> = keys.iter().map(Vec::as_slice).collect();
        let async_results: Vec<Option<Vec<u8>>> = db
            .multi_get_opt(key_refs, &ro)
            .into_iter()
            .map(Result::unwrap)
            .collect();

        assert_eq!(
            reference,
            async_results,
            "async_io MultiGet results must match a serial Get loop \
             (coroutines feature: {}, level counts: {counts:?})",
            cfg!(feature = "coroutines")
        );
    }
}

/// Reads a batch of keys that all live in a single SST after a flush.
/// Exercises the same-level fast path even when the coroutines feature is
/// enabled, since version_set.cc's `MultiGetFromSST` short-circuits the
/// coroutine dispatch when only one file is involved.
#[test]
fn multi_get_async_io_matches_serial_get_single_level() {
    let path = DBPath::new("_rust_rocksdb_multi_get_async_io_single_level");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);

        let db = DB::open(&opts, &path).unwrap();
        for i in 0..32u32 {
            let key = format!("key-{i:04}");
            db.put(key.as_bytes(), format!("value-{i}").as_bytes())
                .unwrap();
        }
        db.flush().unwrap();

        let keys: Vec<Vec<u8>> = (0..32u32)
            .map(|i| format!("key-{i:04}").into_bytes())
            .collect();

        let reference: Vec<Option<Vec<u8>>> = keys.iter().map(|k| db.get(k).unwrap()).collect();

        let mut ro = ReadOptions::default();
        ro.set_async_io(true);
        let key_refs: Vec<&[u8]> = keys.iter().map(Vec::as_slice).collect();
        let async_results: Vec<Option<Vec<u8>>> = db
            .multi_get_opt(key_refs, &ro)
            .into_iter()
            .map(Result::unwrap)
            .collect();

        assert_eq!(reference, async_results);
    }
}

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
//! feature is disabled, `async_io=true` only parallelizes within a single
//! LSM level; when enabled, the multi-level coroutine path activates. The
//! tests don't assert anything about *which* path was taken - only that the
//! results are correct in both cases. That's the property that matters from
//! the user's perspective.

mod util;

use pretty_assertions::assert_eq;
use rust_rocksdb::{DB, Options, ReadOptions, WaitForCompactOptions, WriteOptions};

use util::DBPath;

#[test]
fn built_with_coroutines_matches_feature_flag() {
    assert_eq!(
        rust_rocksdb::built_with_coroutines(),
        cfg!(feature = "coroutines")
    );
}

/// Builds a DB whose keyspace is spread across multiple LSM levels, then
/// verifies that `multi_get` with `async_io=true` returns identical results
/// to a loop of single `get`s. Both branches of `version_set.cc`'s
/// `MultiGetFromSST{,Coroutine}` dispatch must produce correct results.
#[test]
fn multi_get_async_io_matches_serial_get() {
    let path = DBPath::new("_rust_rocksdb_multi_get_async_io");
    {
        // Force many small SST files spread across several levels.
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.set_write_buffer_size(64 * 1024);
        opts.set_target_file_size_base(64 * 1024);
        opts.set_max_bytes_for_level_base(256 * 1024);
        opts.set_level_zero_file_num_compaction_trigger(2);
        opts.set_num_levels(4);
        opts.set_disable_auto_compactions(false);

        let db = DB::open(&opts, &path).unwrap();

        let mut wo = WriteOptions::default();
        wo.set_sync(false);

        // Insert enough data to span multiple non-empty levels after
        // compactions settle.
        let n: u32 = 4_000;
        let value = vec![b'v'; 200];
        for i in 0..n {
            let key = format!("key-{i:08}");
            db.put_opt(key.as_bytes(), &value, &wo).unwrap();
        }
        db.flush().unwrap();

        // Trigger a full compaction and wait for it to finish so the
        // resulting LSM has a stable level layout.
        db.compact_range::<&[u8], &[u8]>(None, None);
        let mut wait_opts = WaitForCompactOptions::default();
        wait_opts.set_flush(true);
        wait_opts.set_timeout(30 * 1_000_000); // 30s in microseconds
        db.wait_for_compact(&wait_opts).unwrap();

        // Build a batch of keys mixing hits and misses, sparsely spread
        // across the keyspace so they're likely to land in different SSTs
        // and different levels.
        let mut keys: Vec<Vec<u8>> = (0..n)
            .step_by(53)
            .map(|i| format!("key-{i:08}").into_bytes())
            .collect();
        keys.push(b"key-99999999".to_vec()); // miss
        keys.push(b"a-missing-key".to_vec()); // miss
        keys.push(b"key-00000017".to_vec()); // hit, small key

        // Reference: a loop of single Gets.
        let reference: Vec<Option<Vec<u8>>> = keys.iter().map(|k| db.get(k).unwrap()).collect();

        // Test path: multi_get_opt with async_io=true.
        let mut ro = ReadOptions::default();
        ro.set_async_io(true);
        let key_refs: Vec<&[u8]> = keys.iter().map(Vec::as_slice).collect();
        let async_results: Vec<Option<Vec<u8>>> = db
            .multi_get_opt(key_refs, &ro)
            .into_iter()
            .map(Result::unwrap)
            .collect();

        assert_eq!(reference.len(), async_results.len());
        assert_eq!(
            reference,
            async_results,
            "async_io MultiGet results must match a serial Get loop \
             (coroutines feature: {})",
            cfg!(feature = "coroutines")
        );
    }
}

/// Same as above, but reads a batch of keys that are guaranteed to all hit
/// the same single SST/level after compaction. Stresses the single-level
/// async path even when coroutines are enabled (which forces a single-file
/// fast-path in version_set.cc's `MultiGetFromSST`).
#[test]
fn multi_get_async_io_same_level_matches_serial_get() {
    let path = DBPath::new("_rust_rocksdb_multi_get_async_io_same_level");
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

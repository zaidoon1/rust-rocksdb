// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

//! Reads that report the user-defined timestamp a value was written with.
//!
//! The plain getters throw the timestamp away, so these calls are the only way to see
//! it. Every DB here uses `util::U64Comparator`, an 8 byte little endian timestamp,
//! because a timestamped read is only legal on a column family whose comparator
//! declares a timestamp size.

mod util;

use pretty_assertions::assert_eq;
use rust_rocksdb::{DB, Options, ReadOptions};
use util::DBPath;

/// Options for a DB whose default column family carries 8 byte timestamps.
fn ts_options() -> Options {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.set_comparator_with_ts(
        util::U64Comparator::NAME,
        std::mem::size_of::<u64>(),
        Box::new(util::U64Comparator::compare),
        Box::new(util::U64Comparator::compare_ts),
        Box::new(util::U64Comparator::compare_without_ts),
    );
    opts
}

/// Read options that read as of `ts`.
fn read_at(ts: u64) -> ReadOptions {
    let mut ro = ReadOptions::default();
    ro.set_timestamp(ts.to_le_bytes());
    ro
}

#[test]
fn get_with_ts_reports_the_timestamp_the_value_was_written_with() {
    let path = DBPath::new("_rust_rocksdb_get_with_ts_basic");
    {
        let db = DB::open(&ts_options(), &path).unwrap();
        db.put_with_ts(b"k", 7_u64.to_le_bytes(), b"v").unwrap();

        let got = db
            .get_with_ts(b"k", &read_at(7))
            .unwrap()
            .expect("the key was just written");

        assert_eq!(got.value.as_ref(), b"v");
        assert_eq!(got.timestamp.as_ref(), &7_u64.to_le_bytes());
    }
}

/// Reading as of an older timestamp sees the older value and reports that timestamp.
#[test]
fn get_with_ts_reads_the_version_the_read_timestamp_selects() {
    let path = DBPath::new("_rust_rocksdb_get_with_ts_versions");
    {
        let db = DB::open(&ts_options(), &path).unwrap();
        db.put_with_ts(b"k", 1_u64.to_le_bytes(), b"first").unwrap();
        db.put_with_ts(b"k", 5_u64.to_le_bytes(), b"second")
            .unwrap();

        let old = db.get_with_ts(b"k", &read_at(1)).unwrap().unwrap();
        assert_eq!(old.value.as_ref(), b"first");
        assert_eq!(old.timestamp.as_ref(), &1_u64.to_le_bytes());

        let new = db.get_with_ts(b"k", &read_at(5)).unwrap().unwrap();
        assert_eq!(new.value.as_ref(), b"second");
        assert_eq!(new.timestamp.as_ref(), &5_u64.to_le_bytes());

        // A read before anything was written sees nothing rather than erroring.
        assert!(db.get_with_ts(b"k", &read_at(0)).unwrap().is_none());
    }
}

/// A missing key is `Ok(None)`.
///
/// This is the path where RocksDB leaves the timestamp out-param untouched, so it also
/// covers the wrapper initialising it rather than reading whatever was on the stack.
#[test]
fn get_with_ts_reports_a_missing_key_as_none() {
    let path = DBPath::new("_rust_rocksdb_get_with_ts_missing");
    {
        let db = DB::open(&ts_options(), &path).unwrap();
        db.put_with_ts(b"present", 1_u64.to_le_bytes(), b"v")
            .unwrap();

        assert!(db.get_with_ts(b"absent", &read_at(1)).unwrap().is_none());
        // Repeated so a stale pointer left by the previous miss would show up.
        assert!(db.get_with_ts(b"absent", &read_at(1)).unwrap().is_none());
    }
}

/// A zero length value round trips, timestamp included.
#[test]
fn get_with_ts_handles_an_empty_value() {
    let path = DBPath::new("_rust_rocksdb_get_with_ts_empty");
    {
        let db = DB::open(&ts_options(), &path).unwrap();
        db.put_with_ts(b"k", 3_u64.to_le_bytes(), b"").unwrap();

        let got = db.get_with_ts(b"k", &read_at(3)).unwrap().unwrap();
        assert_eq!(got.value.as_ref(), b"");
        assert_eq!(got.timestamp.as_ref(), &3_u64.to_le_bytes());
    }
}

/// Reading without a timestamp on the read options is an error, not a panic.
#[test]
fn get_with_ts_without_a_read_timestamp_is_an_error() {
    let path = DBPath::new("_rust_rocksdb_get_with_ts_no_read_ts");
    {
        let db = DB::open(&ts_options(), &path).unwrap();
        db.put_with_ts(b"k", 1_u64.to_le_bytes(), b"v").unwrap();

        let err = db
            .get_with_ts(b"k", &ReadOptions::default())
            .unwrap_err()
            .into_string();
        assert!(
            err.contains("timestamp"),
            "the error should name the missing timestamp, got: {err}"
        );
    }
}

#[test]
fn get_cf_with_ts_reads_from_the_named_column_family() {
    let path = DBPath::new("_rust_rocksdb_get_cf_with_ts");
    {
        let mut opts = ts_options();
        opts.create_missing_column_families(true);
        let db = DB::open_cf_with_opts(&opts, &path, [("ts_cf", opts.clone())]).unwrap();
        let cf = db.cf_handle("ts_cf").unwrap();

        db.put_cf_with_ts(&cf, b"k", 9_u64.to_le_bytes(), b"in_cf")
            .unwrap();

        let got = db.get_cf_with_ts(&cf, b"k", &read_at(9)).unwrap().unwrap();
        assert_eq!(got.value.as_ref(), b"in_cf");
        assert_eq!(got.timestamp.as_ref(), &9_u64.to_le_bytes());

        // Only the extra column family was given the timestamped comparator, and a
        // timestamped read against one without it is rejected rather than reported as a
        // miss.
        let err = db.get_with_ts(b"k", &read_at(9)).unwrap_err().into_string();
        assert!(
            err.contains("does not enable timestamp"),
            "expected the default column family to reject the read, got: {err}"
        );
    }
}

/// The batch keeps input order and reports hits, misses and timestamps per key.
#[test]
fn multi_get_with_ts_reports_each_key_in_order() {
    let path = DBPath::new("_rust_rocksdb_multi_get_with_ts");
    {
        let db = DB::open(&ts_options(), &path).unwrap();
        db.put_with_ts(b"a", 2_u64.to_le_bytes(), b"va").unwrap();
        db.put_with_ts(b"c", 2_u64.to_le_bytes(), b"vc").unwrap();

        let results = db.multi_get_with_ts([b"a", b"b", b"c"], &read_at(2));
        assert_eq!(results.len(), 3);

        let a = results[0].as_ref().unwrap().as_ref().unwrap();
        assert_eq!(a.value.as_ref(), b"va");
        assert_eq!(a.timestamp.as_ref(), &2_u64.to_le_bytes());

        assert!(
            results[1].as_ref().unwrap().is_none(),
            "b was never written"
        );

        let c = results[2].as_ref().unwrap().as_ref().unwrap();
        assert_eq!(c.value.as_ref(), b"vc");
        assert_eq!(c.timestamp.as_ref(), &2_u64.to_le_bytes());
    }
}

/// An empty key list is an empty result list, not a panic on the zero length arrays.
#[test]
fn multi_get_with_ts_accepts_no_keys() {
    let path = DBPath::new("_rust_rocksdb_multi_get_with_ts_empty");
    {
        let db = DB::open(&ts_options(), &path).unwrap();
        let results = db.multi_get_with_ts(Vec::<&[u8]>::new(), &read_at(1));
        assert!(results.is_empty());
    }
}

#[test]
fn multi_get_cf_with_ts_reads_each_key_from_its_own_column_family() {
    let path = DBPath::new("_rust_rocksdb_multi_get_cf_with_ts");
    {
        let mut opts = ts_options();
        opts.create_missing_column_families(true);
        let db =
            DB::open_cf_with_opts(&opts, &path, [("one", opts.clone()), ("two", opts.clone())])
                .unwrap();
        let one = db.cf_handle("one").unwrap();
        let two = db.cf_handle("two").unwrap();

        db.put_cf_with_ts(&one, b"k", 4_u64.to_le_bytes(), b"from_one")
            .unwrap();
        db.put_cf_with_ts(&two, b"k", 4_u64.to_le_bytes(), b"from_two")
            .unwrap();

        let results =
            db.multi_get_cf_with_ts([(&one, b"k".to_vec()), (&two, b"k".to_vec())], &read_at(4));
        assert_eq!(results.len(), 2);

        let first = results[0].as_ref().unwrap().as_ref().unwrap();
        assert_eq!(first.value.as_ref(), b"from_one");
        assert_eq!(first.timestamp.as_ref(), &4_u64.to_le_bytes());

        let second = results[1].as_ref().unwrap().as_ref().unwrap();
        assert_eq!(second.value.as_ref(), b"from_two");
        assert_eq!(second.timestamp.as_ref(), &4_u64.to_le_bytes());
    }
}

/// Every value read stays valid after the batch it came from is dropped.
///
/// The pointers are RocksDB allocations the results own outright, so nothing about
/// them depends on the `Vec` that delivered them.
#[test]
fn multi_get_with_ts_values_outlive_the_result_vec() {
    let path = DBPath::new("_rust_rocksdb_multi_get_with_ts_outlive");
    {
        let db = DB::open(&ts_options(), &path).unwrap();
        db.put_with_ts(b"k", 1_u64.to_le_bytes(), b"v").unwrap();

        let kept = {
            let mut results = db.multi_get_with_ts([b"k"], &read_at(1));
            results.remove(0).unwrap().unwrap()
        };

        assert_eq!(kept.value.as_ref(), b"v");
        assert_eq!(kept.timestamp.as_ref(), &1_u64.to_le_bytes());
    }
}

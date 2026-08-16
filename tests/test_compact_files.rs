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

//! Coverage for `compact_files`, `compact_files_cf` and `CompactionOptions`.
//!
//! The interesting part is ownership. `rocksdb_compact_files` hands back a
//! `char**` that the crate has to read and then free with
//! `rocksdb_compact_files_output_file_names_destroy`, and it leaves both
//! out-parameters untouched when it fails. Every test here either reads that
//! array or forces a failure path that must not read it, so a leak or a read of
//! freed memory shows up under a sanitizer.

mod util;

/// The job info a successful `compact_files` reports.
///
/// `CompactFilesResult::job_info` is only `None` when `allow_trivial_move` is enabled, and no
/// test here enables it, so a missing job info is a bug rather than a case to handle.
fn job_info(result: &CompactFilesResult) -> &CompactionJobInfo {
    result
        .job_info
        .as_ref()
        .expect("job info is collected unless allow_trivial_move is enabled")
        .info()
}

use std::path::Path;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use rust_rocksdb::event_listener::{CompactionJobInfo, DBCompactionReason};
use rust_rocksdb::{
    CompactFilesResult, CompactionCancellationToken, CompactionOptions, DB, DBCompressionType,
    DEFAULT_COLUMN_FAMILY_NAME, ErrorKind, LiveFile, Options, Temperature,
};
use util::DBPath;

/// Options that hold the LSM tree still, so a test's own `compact_files` call
/// is the only thing that moves a file between levels.
fn quiet_options() -> Options {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);
    opts.set_disable_auto_compactions(true);
    // Belt and braces. These tests create a handful of files and never come
    // close to the trigger, so nothing is scheduled even if auto compactions
    // were somehow enabled.
    opts.set_level_zero_file_num_compaction_trigger(1024);
    opts
}

fn live_files_at_level(db: &DB, cf_name: &str, level: i32) -> Vec<LiveFile> {
    db.live_files()
        .unwrap()
        .into_iter()
        .filter(|f| f.column_family_name == cf_name && f.level == level)
        .collect()
}

/// The names `compact_files` wants, which are the ones RocksDB's own metadata
/// reports rather than anything read off the filesystem.
///
/// `CStrLike` covers `&str` but not `String`, so these are borrowed from the
/// listing rather than cloned out of it.
fn names_of(files: &[LiveFile]) -> Vec<&str> {
    files.iter().map(|f| f.name.as_str()).collect()
}

/// Writes two batches, flushing after each, leaving two SST files in L0.
fn fill_two_l0_files(db: &DB) {
    db.put(b"k1", b"v1").unwrap();
    db.put(b"k2", b"v2").unwrap();
    db.flush().unwrap();
    db.put(b"k3", b"v3").unwrap();
    db.put(b"k4", b"v4").unwrap();
    db.flush().unwrap();
}

/// Checks that every reported output name is a real live SST file.
///
/// `CompactFilesImpl` fills these in with `TableFileName(cf_paths, number,
/// path_id)`, so each one is an absolute path ending in a zero padded file
/// number. `LiveFile` splits the same path into `directory` and a `name` that
/// keeps its leading separator, so joining the two back together is an exact
/// comparison rather than a suffix match.
#[track_caller]
fn assert_output_names_are_live_ssts(db: &DB, output_files: &[String]) {
    let live: Vec<String> = db
        .live_files()
        .unwrap()
        .iter()
        .map(|f| format!("{}{}", f.directory, f.name))
        .collect();

    for name in output_files {
        assert!(
            !name.is_empty(),
            "compact_files reported an empty output name"
        );
        assert!(name.ends_with(".sst"), "not an SST file name: {name}");

        let stem = Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_else(|| panic!("output name has no file stem: {name}"));
        assert!(
            !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_digit()),
            "output file stem is not a file number: {name}"
        );

        assert!(
            Path::new(name).is_file(),
            "output file is not on disk: {name}"
        );
        assert!(
            live.contains(name),
            "output file is not in the live file list: {name}\nlive: {live:?}"
        );
    }
}

/// Compacting two L0 files into L1 empties L0, writes at least one output file
/// and leaves every key readable.
#[test]
fn test_compact_files_moves_l0_into_l1() {
    let path = DBPath::new("_rust_rocksdb_compact_files_l0_to_l1");
    {
        let db = DB::open(&quiet_options(), &path).unwrap();
        fill_two_l0_files(&db);

        let inputs = live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0);
        assert_eq!(inputs.len(), 2, "expected one SST per flush");

        let opts = CompactionOptions::default();
        let result = db.compact_files(&opts, names_of(&inputs), 1).unwrap();

        assert!(
            !result.output_files.is_empty(),
            "a compaction of live keys must write at least one file"
        );
        assert_output_names_are_live_ssts(&db, &result.output_files);

        // `SanitizeCompactionInputFilesForAllLevels` pulls in every L0 file when
        // the output level is above 0, so L0 always drains completely here.
        assert_eq!(
            live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0).len(),
            0,
            "L0 should be empty after compacting all of it upwards"
        );
        assert_eq!(
            live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 1).len(),
            result.output_files.len(),
            "L1 should hold exactly the files the compaction reported"
        );

        for (key, value) in [
            (b"k1", b"v1"),
            (b"k2", b"v2"),
            (b"k3", b"v3"),
            (b"k4", b"v4"),
        ] {
            assert_eq!(db.get(key).unwrap().as_deref(), Some(value.as_slice()));
        }
    }
}

/// The job info out-parameter describes the compaction that just ran.
///
/// Only fields `BuildCompactionJobInfo` assigns are read. `num_l0_files` is left to
/// `test_compact_files_job_info_has_no_num_l0_files`, which covers it reporting
/// nothing on this path.
#[test]
fn test_compact_files_reports_job_info() {
    let path = DBPath::new("_rust_rocksdb_compact_files_job_info");
    {
        let db = DB::open(&quiet_options(), &path).unwrap();
        fill_two_l0_files(&db);

        let inputs = live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0);
        let opts = CompactionOptions::default();
        let result = db.compact_files(&opts, names_of(&inputs), 1).unwrap();
        let info = job_info(&result);

        info.status().unwrap();
        assert!(!info.aborted());
        assert_eq!(
            info.cf_name().as_deref(),
            Some(DEFAULT_COLUMN_FAMILY_NAME.as_bytes())
        );
        assert_eq!(info.cf_id(), 0, "the default column family is always id 0");
        assert_eq!(
            info.compaction_reason(),
            DBCompactionReason::KManualCompaction
        );
        assert_eq!(info.base_input_level(), 0, "the inputs all came from L0");
        assert_eq!(info.output_level(), 1);

        assert_eq!(info.input_file_count(), inputs.len());
        assert_eq!(info.input_file_infos_count(), inputs.len());
        assert_eq!(info.output_file_count(), result.output_files.len());
        assert_eq!(info.output_file_infos_count(), result.output_files.len());
        assert_eq!(
            info.num_input_files_at_output_level(),
            0,
            "L1 was empty, so nothing was pulled in from the output level"
        );

        // Four distinct keys in, four out, since none of them shadow each other.
        assert_eq!(info.input_records(), 4);
        assert_eq!(info.output_records(), 4);
        assert_eq!(info.num_corrupt_keys(), 0);
        assert!(info.total_input_bytes() > 0);
        assert!(info.total_output_bytes() > 0);

        // The names in the job info are the same absolute paths the result
        // carries, just reached through a different accessor.
        let job_outputs: Vec<String> = (0..info.output_file_count())
            .map(|i| String::from_utf8(info.output_file_at(i).unwrap().to_vec()).unwrap())
            .collect();
        assert_eq!(job_outputs, result.output_files);

        let mut got_inputs: Vec<String> = (0..info.input_file_count())
            .map(|i| String::from_utf8(info.input_file_at(i).unwrap().to_vec()).unwrap())
            .collect();
        let mut want_inputs: Vec<String> = inputs
            .iter()
            .map(|f| format!("{}{}", f.directory, f.name))
            .collect();
        got_inputs.sort();
        want_inputs.sort();
        assert_eq!(got_inputs, want_inputs);

        // Past the end of either list is `None` rather than a panic.
        assert_eq!(info.input_file_at(info.input_file_count()), None);
        assert_eq!(info.output_file_at(info.output_file_count()), None);

        let stats = info.stats();
        assert!(stats.is_manual_compaction());
        assert_eq!(stats.num_input_files(), inputs.len());
        assert_eq!(stats.num_output_files(), result.output_files.len());
        assert_eq!(stats.num_input_records(), 4);
        assert_eq!(stats.num_output_records(), 4);
        assert_eq!(stats.num_corrupt_keys(), 0);
        assert!(stats.total_input_bytes() > 0);
        assert!(stats.total_output_bytes() > 0);
    }
}

/// A compaction that drops every key succeeds and reports no output files.
///
/// This is the case that makes the output array come back empty, which
/// `collect_and_free_output_names` short circuits before it frees anything.
#[test]
fn test_compact_files_with_all_keys_deleted_reports_no_outputs() {
    let path = DBPath::new("_rust_rocksdb_compact_files_all_deleted");
    {
        let db = DB::open(&quiet_options(), &path).unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.put(b"k2", b"v2").unwrap();
        db.flush().unwrap();
        db.delete(b"k1").unwrap();
        db.delete(b"k2").unwrap();
        db.flush().unwrap();

        let inputs = live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0);
        assert_eq!(inputs.len(), 2);

        // The last level, where a tombstone with nothing below it and no
        // snapshot holding it can be dropped instead of written out.
        let bottom_level = 6;
        let opts = CompactionOptions::default();
        let result = db
            .compact_files(&opts, names_of(&inputs), bottom_level)
            .unwrap();

        job_info(&result).status().unwrap();
        assert!(
            result.output_files.is_empty(),
            "compacting nothing but tombstones to the last level should write \
             no file, got {:?}",
            result.output_files
        );
        assert_eq!(job_info(&result).output_file_count(), 0);
        assert_eq!(job_info(&result).output_records(), 0);
        assert_eq!(db.get(b"k1").unwrap(), None);
        assert_eq!(db.get(b"k2").unwrap(), None);
    }
}

/// An input file RocksDB does not know about is rejected before anything runs.
#[test]
fn test_compact_files_rejects_unknown_input_file() {
    let path = DBPath::new("_rust_rocksdb_compact_files_unknown_input");
    {
        let db = DB::open(&quiet_options(), &path).unwrap();
        fill_two_l0_files(&db);
        let before_files = live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0);
        let before = names_of(&before_files);

        let opts = CompactionOptions::default();
        let err = db.compact_files(&opts, ["/999999.sst"], 1).unwrap_err();

        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert!(
            err.to_string().contains("does not exist"),
            "unexpected error message: {err}"
        );
        // The docs promise nothing is compacted when the request is rejected.
        let after_files = live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0);
        assert_eq!(names_of(&after_files), before);
        assert_eq!(
            live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 1).len(),
            0
        );
    }
}

/// An empty input list is rejected. This is a failure path where RocksDB
/// returns before writing either out-parameter, so the crate must not read the
/// null pointer it passed in.
#[test]
fn test_compact_files_rejects_empty_input_list() {
    let path = DBPath::new("_rust_rocksdb_compact_files_empty_input");
    {
        let db = DB::open(&quiet_options(), &path).unwrap();
        fill_two_l0_files(&db);

        let opts = CompactionOptions::default();
        let err = db.compact_files(&opts, Vec::<&str>::new(), 1).unwrap_err();

        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert_eq!(
            live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0).len(),
            2
        );
    }
}

/// An output level past the last one is rejected, the other early return that
/// leaves both out-parameters untouched.
#[test]
fn test_compact_files_rejects_out_of_range_output_level() {
    let path = DBPath::new("_rust_rocksdb_compact_files_bad_output_level");
    {
        let db = DB::open(&quiet_options(), &path).unwrap();
        fill_two_l0_files(&db);
        let inputs = live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0);

        // `num_levels` defaults to 7, so the levels are 0 through 6.
        let opts = CompactionOptions::default();
        let err = db.compact_files(&opts, names_of(&inputs), 7).unwrap_err();

        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert_eq!(
            live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0).len(),
            2
        );
    }
}

/// `compact_files_cf` targets one column family and leaves the others alone.
#[test]
fn test_compact_files_cf_targets_one_column_family() {
    let path = DBPath::new("_rust_rocksdb_compact_files_cf");
    {
        // Every setting in `quiet_options` is a column family option, and
        // `open_cf` gives each family `Options::default()` instead, including
        // the default family it adds for you. That leaves auto compactions on,
        // so a background job can move the output below level 1 after
        // compact_files_cf returns. Both families need the options explicitly.
        let db = DB::open_cf_with_opts(
            &quiet_options(),
            &path,
            [
                ("cf1", quiet_options()),
                (DEFAULT_COLUMN_FAMILY_NAME, quiet_options()),
            ],
        )
        .unwrap();
        let cf = db.cf_handle("cf1").unwrap();

        // Both column families get two L0 files, so a compaction that leaked
        // across them would show up in the default one.
        fill_two_l0_files(&db);
        db.put_cf(&cf, b"c1", b"v1").unwrap();
        db.flush_cf(&cf).unwrap();
        db.put_cf(&cf, b"c2", b"v2").unwrap();
        db.flush_cf(&cf).unwrap();

        let inputs = live_files_at_level(&db, "cf1", 0);
        assert_eq!(inputs.len(), 2);

        let opts = CompactionOptions::default();
        let result = db
            .compact_files_cf(&cf, &opts, names_of(&inputs), 1)
            .unwrap();

        job_info(&result).status().unwrap();
        assert_eq!(job_info(&result).cf_name().as_deref(), Some(&b"cf1"[..]));
        assert_ne!(
            job_info(&result).cf_id(),
            0,
            "cf1 is not the default column family"
        );
        assert!(!result.output_files.is_empty());
        assert_output_names_are_live_ssts(&db, &result.output_files);

        assert_eq!(live_files_at_level(&db, "cf1", 0).len(), 0);
        assert_eq!(
            live_files_at_level(&db, "cf1", 1).len(),
            result.output_files.len()
        );
        assert_eq!(
            live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0).len(),
            2,
            "the default column family should not have been touched"
        );

        assert_eq!(db.get_cf(&cf, b"c1").unwrap().as_deref(), Some(&b"v1"[..]));
        assert_eq!(db.get_cf(&cf, b"c2").unwrap().as_deref(), Some(&b"v2"[..]));
    }
}

/// `output_file_size_limit` splits the output across several files, which is
/// the case where the output name array holds more than one entry.
#[test]
fn test_compact_files_honours_output_file_size_limit() {
    let path = DBPath::new("_rust_rocksdb_compact_files_size_limit");
    {
        let db = DB::open(&quiet_options(), &path).unwrap();
        let value = vec![b'x'; 4096];
        for i in 0..64u32 {
            db.put(format!("key{i:04}").as_bytes(), &value).unwrap();
        }
        db.flush().unwrap();

        let inputs = live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0);
        assert_eq!(inputs.len(), 1, "one memtable flush makes one SST");

        let mut opts = CompactionOptions::default();
        // Without this the 256 KiB of repeated bytes compresses down to well
        // under one output file and the cap never bites.
        opts.set_compression(DBCompressionType::None);
        opts.set_output_file_size_limit(16 * 1024);
        let result = db.compact_files(&opts, names_of(&inputs), 1).unwrap();

        job_info(&result).status().unwrap();
        assert!(
            result.output_files.len() > 1,
            "256 KiB under a 16 KiB cap should span several files, got {:?}",
            result.output_files
        );
        assert_output_names_are_live_ssts(&db, &result.output_files);

        // Every name is distinct, which is what shows the `char**` was walked
        // entry by entry rather than one pointer being read repeatedly.
        let mut deduped = result.output_files.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(deduped.len(), result.output_files.len());

        for i in 0..64u32 {
            assert_eq!(
                db.get(format!("key{i:04}").as_bytes()).unwrap().as_deref(),
                Some(value.as_slice())
            );
        }
    }
}

/// A token that is already cancelled stops the compaction before it starts, and
/// detaching it lets the same options succeed.
#[test]
fn test_compact_files_with_cancelled_token() {
    let path = DBPath::new("_rust_rocksdb_compact_files_cancelled");
    {
        let db = DB::open(&quiet_options(), &path).unwrap();
        fill_two_l0_files(&db);
        let inputs = live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0);

        let token = Arc::new(CompactionCancellationToken::new());
        token.cancel();

        let mut opts = CompactionOptions::default();
        opts.set_canceled(Arc::clone(&token));
        assert!(opts.canceled().is_some());

        // `CompactFilesImpl` reads the flag before it does any work, so a token
        // cancelled up front fails the same way every run. Cancelling from
        // another thread mid-compaction would be a race, so it is not tested.
        let err = db.compact_files(&opts, names_of(&inputs), 1).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Incomplete);
        assert_eq!(
            live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0).len(),
            2,
            "a cancelled compaction must not move anything"
        );

        // Detaching releases these options' share of the token. The token stays
        // cancelled, since there is no way to un-cancel one.
        opts.clear_canceled();
        assert!(opts.canceled().is_none());

        let result = db.compact_files(&opts, names_of(&inputs), 1).unwrap();
        job_info(&result).status().unwrap();
        assert!(!result.output_files.is_empty());
        assert_eq!(
            live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0).len(),
            0
        );
    }
}

/// The options keep the token alive on their own, so dropping the caller's last
/// handle before the compaction runs is still safe.
#[test]
fn test_compaction_options_keep_token_alive() {
    let path = DBPath::new("_rust_rocksdb_compact_files_token_lifetime");
    {
        let db = DB::open(&quiet_options(), &path).unwrap();
        fill_two_l0_files(&db);
        let inputs = live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0);

        let mut opts = CompactionOptions::default();
        {
            let token = Arc::new(CompactionCancellationToken::new());
            token.cancel();
            opts.set_canceled(token);
        }
        assert_eq!(Arc::strong_count(opts.canceled().unwrap()), 1);

        // The C struct points at a flag owned by the `Arc` inside `opts`.
        // Reading it here is only sound because that handle is still held.
        let err = db.compact_files(&opts, names_of(&inputs), 1).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Incomplete);
    }
}

/// Replacing an attached token releases the old one and points the C struct at
/// the new flag.
#[test]
fn test_compaction_options_replace_token() {
    let first = Arc::new(CompactionCancellationToken::new());
    let second = Arc::new(CompactionCancellationToken::new());

    let mut opts = CompactionOptions::default();
    opts.set_canceled(Arc::clone(&first));
    assert_eq!(Arc::strong_count(&first), 2);

    opts.set_canceled(Arc::clone(&second));
    assert_eq!(Arc::strong_count(&first), 1, "the old token was released");
    assert_eq!(Arc::strong_count(&second), 2);

    opts.clear_canceled();
    assert_eq!(Arc::strong_count(&second), 1);
    assert!(opts.canceled().is_none());

    // Clearing again is a no-op rather than a second release.
    opts.clear_canceled();
    assert!(opts.canceled().is_none());
}

/// Every `CompactionOptions` setter reads back what was written, and the
/// defaults match RocksDB's.
#[test]
fn test_compaction_options_round_trip() {
    let mut opts = CompactionOptions::default();

    // `CompactionOptions::compression` defaults to `kDisableCompressionOption`,
    // which is not a compression type, so it reads back as `None`. That is a
    // different thing from explicitly asking for no compression.
    assert_eq!(opts.get_compression(), None);
    opts.set_compression(DBCompressionType::None);
    assert_eq!(opts.get_compression(), Some(DBCompressionType::None));
    for t in [
        DBCompressionType::Snappy,
        DBCompressionType::Zlib,
        DBCompressionType::Bz2,
        DBCompressionType::Lz4,
        DBCompressionType::Lz4hc,
        DBCompressionType::Zstd,
    ] {
        opts.set_compression(t);
        assert_eq!(opts.get_compression(), Some(t));
    }
    opts.unset_compression();
    assert_eq!(opts.get_compression(), None);

    assert_eq!(opts.get_output_file_size_limit(), u64::MAX);
    opts.set_output_file_size_limit(64 * 1024 * 1024);
    assert_eq!(opts.get_output_file_size_limit(), 64 * 1024 * 1024);
    opts.set_output_file_size_limit(0);
    assert_eq!(opts.get_output_file_size_limit(), 0);

    assert_eq!(opts.get_max_subcompactions(), 0);
    opts.set_max_subcompactions(4);
    assert_eq!(opts.get_max_subcompactions(), 4);

    assert!(!opts.get_allow_trivial_move());
    opts.set_allow_trivial_move(true);
    assert!(opts.get_allow_trivial_move());
    opts.set_allow_trivial_move(false);
    assert!(!opts.get_allow_trivial_move());

    assert_eq!(opts.get_output_temperature_override(), Temperature::Unknown);
    for t in [
        Temperature::Hot,
        Temperature::Warm,
        Temperature::Cool,
        Temperature::Cold,
        Temperature::Ice,
    ] {
        opts.set_output_temperature_override(t);
        assert_eq!(opts.get_output_temperature_override(), t);
    }

    assert!(opts.canceled().is_none());
}

/// The result outlives the DB it came from, since both halves own their data.
#[test]
fn test_compact_files_result_outlives_db() {
    let path = DBPath::new("_rust_rocksdb_compact_files_result_lifetime");
    {
        let result = {
            let db = DB::open(&quiet_options(), &path).unwrap();
            fill_two_l0_files(&db);
            let inputs = live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0);
            let opts = CompactionOptions::default();
            db.compact_files(&opts, names_of(&inputs), 1).unwrap()
        };

        // The DB is closed. The output names were copied out of the `char**`
        // before it was freed and the job info owns its own C++ struct, so both
        // are still readable.
        assert!(!result.output_files.is_empty());
        assert!(result.output_files.iter().all(|n| n.ends_with(".sst")));
        let info = job_info(&result);
        info.status().unwrap();
        assert_eq!(info.output_level(), 1);
        assert_eq!(
            info.cf_name().as_deref(),
            Some(DEFAULT_COLUMN_FAMILY_NAME.as_bytes())
        );
    }
}

/// A compaction that can be satisfied by a trivial move reports no job info.
///
/// `CompactFilesImpl` returns as soon as it has moved the files, before
/// `BuildCompactionJobInfo` would have run, so a job info allocated for that call
/// would come back holding uninitialised scalars while the call still reported
/// success. Nothing observable separates that from a real compaction afterwards,
/// so enabling the option has to suppress the job info entirely.
///
/// The single input file makes the move trivial: there is nothing at level 1 to
/// merge with, so RocksDB can relink the file instead of rewriting it.
#[test]
fn test_compact_files_with_trivial_move_reports_no_job_info() {
    let path = DBPath::new("_rust_rocksdb_compact_files_trivial_move");
    {
        let db = DB::open(&quiet_options(), &path).unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.put(b"k2", b"v2").unwrap();
        db.flush().unwrap();

        let inputs = live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0);
        assert_eq!(inputs.len(), 1, "one flush, one file to move");

        let mut opts = CompactionOptions::default();
        opts.set_allow_trivial_move(true);
        assert!(opts.get_allow_trivial_move());

        let result = db.compact_files(&opts, names_of(&inputs), 1).unwrap();
        assert!(
            result.job_info.is_none(),
            "allow_trivial_move suppresses the job info"
        );

        // The compaction still happened and still reports its output files, which
        // RocksDB fills in on the trivial move path too.
        assert!(!result.output_files.is_empty());
        assert!(result.output_files.iter().all(|n| n.ends_with(".sst")));
        assert_eq!(db.get(b"k1").unwrap().as_deref(), Some(b"v1".as_slice()));
        assert_eq!(db.get(b"k2").unwrap().as_deref(), Some(b"v2".as_slice()));
    }
}

/// `num_l0_files` is absent on the `CompactFiles` path.
///
/// `BuildCompactionJobInfo` never assigns it and the field has no in-class
/// initialiser, so there is no value to report. The listener path, which
/// `test_event_listener` covers, does set it.
#[test]
fn test_compact_files_job_info_has_no_num_l0_files() {
    let path = DBPath::new("_rust_rocksdb_compact_files_no_num_l0");
    {
        let db = DB::open(&quiet_options(), &path).unwrap();
        fill_two_l0_files(&db);

        let inputs = live_files_at_level(&db, DEFAULT_COLUMN_FAMILY_NAME, 0);
        let opts = CompactionOptions::default();
        let result = db.compact_files(&opts, names_of(&inputs), 1).unwrap();

        assert_eq!(job_info(&result).num_l0_files(), None);
    }
}

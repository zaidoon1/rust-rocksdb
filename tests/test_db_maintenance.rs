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

//! Runtime maintenance operations: checksum verification, background work control,
//! compaction hints, runtime DB option changes and WAL flushing with options.

mod util;

use pretty_assertions::assert_eq;
use rust_rocksdb::{
    AsColumnFamilyRef, DB, FlushWalOptions, IoPriority, Options, ReadOptions,
    checkpoint::Checkpoint, file_checksum::FileChecksumGenFactory,
};
use util::DBPath;

fn filled_db(path: &DBPath) -> DB {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = DB::open(&opts, path).unwrap();
    for i in 0..200_u32 {
        db.put(format!("key{i:04}"), format!("value{i}")).unwrap();
    }
    db.flush().unwrap();
    db
}

#[test]
fn verify_checksum_accepts_a_healthy_db() {
    let path = DBPath::new("_rust_rocksdb_verify_checksum");
    {
        let db = filled_db(&path);
        db.verify_checksum().unwrap();
        db.verify_checksum_opt(&ReadOptions::default()).unwrap();
    }
}

/// `verify_file_checksums` needs a checksum generator to have been configured.
///
/// Without one RocksDB recorded no whole file checksums, and rather than treating
/// that as nothing to check it refuses the request.
#[test]
fn verify_file_checksums_requires_a_generator() {
    let path = DBPath::new("_rust_rocksdb_verify_file_checksums_no_gen");
    {
        let db = filled_db(&path);

        for err in [
            db.verify_file_checksums().unwrap_err(),
            db.verify_file_checksums_opt(&ReadOptions::default())
                .unwrap_err(),
        ] {
            let err = err.into_string();
            assert!(
                err.contains("file_checksum_gen_factory"),
                "the error should name the missing factory, got: {err}"
            );
        }
    }
}

/// With a generator configured, verification reads the files and agrees with them.
#[test]
fn verify_file_checksums_accepts_a_db_written_with_a_generator() {
    let path = DBPath::new("_rust_rocksdb_verify_file_checksums_crc32c");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.set_file_checksum_gen_factory(&FileChecksumGenFactory::crc32c());
        let db = DB::open(&opts, &path).unwrap();
        for i in 0..100_u32 {
            db.put(format!("key{i:04}"), format!("value{i}")).unwrap();
        }
        db.flush().unwrap();

        db.verify_file_checksums().unwrap();
        db.verify_file_checksums_opt(&ReadOptions::default())
            .unwrap();
    }
}

/// Corrupting an SST on disk makes checksum verification fail.
///
/// Without this the passing cases above would look the same as a verification that
/// never actually reads anything.
#[test]
fn verify_checksum_rejects_a_corrupted_sst() {
    let path = DBPath::new("_rust_rocksdb_verify_checksum_corrupt");
    {
        let sst = {
            let db = filled_db(&path);
            let files = db.live_files().unwrap();
            assert!(!files.is_empty(), "the flush should have written a file");
            let path_ref = &path;
            let dir: &std::path::Path = path_ref.as_ref();
            dir.join(&files[0].name[1..])
        };

        // Overwrite bytes in the middle of the data blocks, well past the header and
        // well before the footer, so the file still opens and the damage shows up as a
        // block checksum mismatch.
        let mut bytes = std::fs::read(&sst).unwrap();
        let middle = bytes.len() / 2;
        for byte in &mut bytes[middle..middle + 32] {
            *byte ^= 0xff;
        }
        std::fs::write(&sst, &bytes).unwrap();

        let db = DB::open(&Options::default(), &path).unwrap();
        assert!(
            db.verify_checksum().is_err(),
            "verification should notice the flipped bytes"
        );
    }
}

/// Background work stops and restarts, and the calls nest.
#[test]
fn background_work_pauses_and_continues() {
    let path = DBPath::new("_rust_rocksdb_pause_background_work");
    {
        let db = filled_db(&path);

        db.pause_background_work().unwrap();
        // Writes still land while background work is paused.
        db.put(b"during_pause", b"v").unwrap();
        db.continue_background_work().unwrap();

        // Nested: two pauses need two continues.
        db.pause_background_work().unwrap();
        db.pause_background_work().unwrap();
        db.continue_background_work().unwrap();
        db.continue_background_work().unwrap();

        assert_eq!(
            db.get(b"during_pause").unwrap().as_deref(),
            Some(b"v".as_slice())
        );
        // The DB still works normally afterwards.
        db.compact_range(None::<&[u8]>, None::<&[u8]>);
    }
}

/// One continue too many is an error rather than a silent underflow.
#[test]
fn continue_background_work_without_a_pause_is_an_error() {
    let path = DBPath::new("_rust_rocksdb_continue_without_pause");
    {
        let db = filled_db(&path);
        assert!(db.continue_background_work().is_err());
    }
}

/// Disabling manual compaction makes `compact_range` return without doing the work.
#[test]
fn manual_compaction_can_be_disabled_and_re_enabled() {
    let path = DBPath::new("_rust_rocksdb_manual_compaction_toggle");
    {
        let db = filled_db(&path);

        db.disable_manual_compaction();
        // Returns immediately instead of compacting. Nothing to assert about timing,
        // only that it is not an error and the data is untouched.
        db.compact_range(None::<&[u8]>, None::<&[u8]>);
        assert_eq!(
            db.get(b"key0000").unwrap().as_deref(),
            Some(b"value0".as_slice())
        );

        db.enable_manual_compaction();
        db.compact_range(None::<&[u8]>, None::<&[u8]>);
        assert_eq!(
            db.get(b"key0000").unwrap().as_deref(),
            Some(b"value0".as_slice())
        );
    }
}

/// Marking a range for compaction succeeds for bounded and unbounded ranges alike.
#[test]
fn suggest_compact_range_marks_bounded_and_unbounded_ranges() {
    let path = DBPath::new("_rust_rocksdb_suggest_compact_range");
    {
        let db = filled_db(&path);

        db.suggest_compact_range(Some(b"key0000"), Some(b"key0100"))
            .unwrap();
        // Either bound may be left out, which means unbounded in that direction.
        db.suggest_compact_range(Some(b"key0050"), None::<&[u8]>)
            .unwrap();
        db.suggest_compact_range(None::<&[u8]>, Some(b"key0050"))
            .unwrap();
        db.suggest_compact_range(None::<&[u8]>, None::<&[u8]>)
            .unwrap();

        // The data survives being marked.
        assert_eq!(
            db.get(b"key0000").unwrap().as_deref(),
            Some(b"value0".as_slice())
        );
    }
}

#[test]
fn suggest_compact_range_cf_targets_one_column_family() {
    let path = DBPath::new("_rust_rocksdb_suggest_compact_range_cf");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        let db = DB::open_cf(&opts, &path, ["other"]).unwrap();
        let cf = db.cf_handle("other").unwrap();

        db.put_cf(&cf, b"k", b"v").unwrap();
        db.flush_cf(&cf).unwrap();

        db.suggest_compact_range_cf(&cf, Some(b"a"), Some(b"z"))
            .unwrap();
        db.suggest_compact_range_cf(&cf, None::<&[u8]>, None::<&[u8]>)
            .unwrap();

        assert_eq!(
            db.get_cf(&cf, b"k").unwrap().as_deref(),
            Some(b"v".as_slice())
        );
    }
}

/// A mutable DB level option can be changed at runtime and read back.
#[test]
fn set_db_options_changes_a_mutable_option() {
    let path = DBPath::new("_rust_rocksdb_set_db_options");
    {
        let db = filled_db(&path);

        db.set_db_options(&[("max_background_jobs", "5")]).unwrap();
        // Several at once, since the C API takes them as parallel arrays.
        db.set_db_options(&[("max_background_jobs", "3"), ("bytes_per_sync", "1048576")])
            .unwrap();
        // RocksDB rejects a call that asks for no changes.
        assert!(db.set_db_options(&[]).is_err());

        db.put(b"after", b"v").unwrap();
        assert_eq!(db.get(b"after").unwrap().as_deref(), Some(b"v".as_slice()));
    }
}

#[test]
fn set_db_options_rejects_bad_names_and_values() {
    let path = DBPath::new("_rust_rocksdb_set_db_options_bad");
    {
        let db = filled_db(&path);

        assert!(db.set_db_options(&[("not_a_real_option", "1")]).is_err());
        // A column family option is not settable through the DB level call.
        assert!(
            db.set_db_options(&[("write_buffer_size", "65536")])
                .is_err()
        );
        // An interior NUL is rejected before RocksDB is reached.
        assert!(db.set_db_options(&[("max_background\0jobs", "1")]).is_err());

        // Deliberately not tested: a malformed value such as
        // ("max_background_jobs", "not_a_number"). RocksDB throws
        // std::invalid_argument out of std::stoi and the exception unwinds into Rust,
        // which aborts the process, so there is no failure here to assert on. See the
        // Aborts section on set_db_options.
    }
}

/// `flush_wal_with_options` covers what `flush_wal` does, plus the priority.
#[test]
fn flush_wal_with_options_syncs_the_wal() {
    let path = DBPath::new("_rust_rocksdb_flush_wal_with_options");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        let db = DB::open(&opts, &path).unwrap();
        db.put(b"k", b"v").unwrap();

        let mut wal_opts = FlushWalOptions::default();
        wal_opts.set_sync(true);
        wal_opts.set_rate_limiter_priority(IoPriority::High);
        assert!(wal_opts.get_sync());
        assert_eq!(wal_opts.get_rate_limiter_priority(), Some(IoPriority::High));

        db.flush_wal_with_options(&wal_opts).unwrap();

        // Without sync as well, which is the cheaper path.
        let mut no_sync = FlushWalOptions::default();
        no_sync.set_sync(false);
        db.flush_wal_with_options(&no_sync).unwrap();

        // The write is readable through a checkpoint, so the WAL really was flushed.
        let cp_path = DBPath::new("_rust_rocksdb_flush_wal_with_options_cp");
        Checkpoint::new(&db)
            .unwrap()
            .create_checkpoint(&cp_path)
            .unwrap();
        let copy = DB::open_for_read_only(&Options::default(), &cp_path, false).unwrap();
        assert_eq!(copy.get(b"k").unwrap().as_deref(), Some(b"v".as_slice()));
    }
}

/// A column family handle reports the id and name RocksDB knows it by.
#[test]
fn column_family_handles_report_their_id_and_name() {
    let path = DBPath::new("_rust_rocksdb_cf_handle_id_and_name");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        let db = DB::open_cf(&opts, &path, ["first", "second"]).unwrap();

        let first = db.cf_handle("first").unwrap();
        let second = db.cf_handle("second").unwrap();

        assert_eq!(first.name(), b"first".to_vec());
        assert_eq!(second.name(), b"second".to_vec());

        // Ids are assigned in creation order and the default family is always 0, so
        // the two extra families are distinct and non-zero.
        assert_ne!(first.id(), second.id());
        assert_ne!(first.id(), 0);
        assert_ne!(second.id(), 0);

        // Reading twice gives the same answer, so nothing is consumed or freed early.
        assert_eq!(first.name(), b"first".to_vec());
        assert_eq!(first.id(), first.id());
    }
}

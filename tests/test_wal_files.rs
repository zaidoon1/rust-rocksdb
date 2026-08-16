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

//! Coverage for `get_sorted_wal_files`, `get_current_wal_file` and the types
//! they hand back.
//!
//! `WalFiles` owns a `std::vector<rocksdb_wal_file_t>` and `WalFile` is a
//! borrowed view pointing straight into it, while `OwnedWalFile` owns a single
//! one of its own. `db/c.cc` snapshots every field by value when it builds
//! them, so both should stay readable after the DB is closed, and the tests
//! below hold them across the drop to prove it.
//!
//! Nothing here asserts that a particular WAL file was deleted or archived.
//! With the default options an obsolete WAL is deleted by a background job, so
//! when it disappears is not something a test can pin down.

mod util;

use pretty_assertions::assert_eq;
use rust_rocksdb::{DB, Options, OwnedWalFile, WalFile, WalFileType, WalFiles, WalReadOptions};
use util::DBPath;

fn db_options() -> Options {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);
    opts
}

/// An alive WAL is reported as `/NNNNNN.log`, relative to the DB directory.
///
/// `WalFileImpl::PathName` builds it with `LogFileName("", number)`, so the
/// leading separator is always there and the number is zero padded to at least
/// six digits.
#[track_caller]
fn assert_alive_wal_path(file: WalFile<'_>) {
    let name = file.path_name_lossy();
    assert!(name.starts_with('/'), "WAL path is not rooted: {name}");
    assert!(name.ends_with(".log"), "WAL path is not a log file: {name}");

    let digits = name
        .trim_start_matches('/')
        .strip_suffix(".log")
        .expect("checked above");
    assert!(
        digits.len() >= 6 && digits.bytes().all(|b| b.is_ascii_digit()),
        "WAL path does not hold a zero padded log number: {name}"
    );
    assert_eq!(
        digits.parse::<u64>().unwrap(),
        file.log_number(),
        "the path and the log number disagree"
    );
    assert_eq!(file.path_name(), name.as_bytes());
}

fn log_numbers(files: &WalFiles) -> Vec<u64> {
    files.iter().map(WalFile::log_number).collect()
}

/// The listing is sorted oldest first by log number, with no repeats.
#[track_caller]
fn assert_strictly_ascending(numbers: &[u64]) {
    for pair in numbers.windows(2) {
        assert!(
            pair[0] < pair[1],
            "WAL files are not strictly ascending by log number: {numbers:?}"
        );
    }
}

/// A DB that has never been written to lists no WAL files.
///
/// `GetSortedWalsOfType` asks for sequence numbers, and it skips any WAL whose
/// first record sequence is 0, which is how it spots an empty file. The WAL a
/// fresh DB opens with is exactly that.
#[test]
fn test_sorted_wal_files_is_empty_before_any_write() {
    let path = DBPath::new("_rust_rocksdb_wal_empty");
    {
        let db = DB::open(&db_options(), &path).unwrap();

        let files = db.get_sorted_wal_files().unwrap();
        assert_eq!(files.len(), 0);
        assert!(files.is_empty());
        assert!(files.get(0).is_none());
        assert_eq!(files.iter().count(), 0);

        // The current WAL exists even though the listing skipped it.
        let current = db.get_current_wal_file().unwrap();
        assert!(current.log_number() > 0);
        assert_eq!(current.file_type(), WalFileType::AliveLogFile);
    }
}

/// Once there is something to record, the WAL shows up with real metadata.
#[test]
fn test_sorted_wal_files_after_a_write() {
    let path = DBPath::new("_rust_rocksdb_wal_after_write");
    {
        let db = DB::open(&db_options(), &path).unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.put(b"k2", b"v2").unwrap();

        let files = db.get_sorted_wal_files().unwrap();
        assert!(!files.is_empty());
        assert_strictly_ascending(&log_numbers(&files));

        let file = files.get(files.len() - 1).unwrap();
        assert_eq!(file.file_type(), WalFileType::AliveLogFile);
        assert!(file.log_number() > 0);
        assert_alive_wal_path(file);
        assert!(
            file.size_file_bytes() > 0,
            "a WAL holding two writes cannot be empty on disk"
        );
        // A WAL only makes the listing when its first record has a real
        // sequence number, so this is non-zero by construction.
        assert!(file.start_sequence() > 0);
    }
}

/// The current WAL is the newest entry in the listing.
#[test]
fn test_current_wal_file_is_in_the_sorted_listing() {
    let path = DBPath::new("_rust_rocksdb_wal_current");
    {
        let db = DB::open(&db_options(), &path).unwrap();
        db.put(b"k1", b"v1").unwrap();

        let current = db.get_current_wal_file().unwrap();
        assert_eq!(current.file_type(), WalFileType::AliveLogFile);
        assert_alive_wal_path(current.as_wal_file());
        assert_eq!(current.path_name(), current.as_wal_file().path_name());
        assert_eq!(current.log_number(), current.as_wal_file().log_number());

        // Nothing has been flushed, so the WAL being written to is the only one
        // and therefore the newest.
        let files = db.get_sorted_wal_files().unwrap();
        let numbers = log_numbers(&files);
        assert!(
            numbers.contains(&current.log_number()),
            "the current WAL {} is missing from {numbers:?}",
            current.log_number()
        );
        assert_eq!(numbers.last().copied(), Some(current.log_number()));
    }
}

/// Flushing seals the WAL and starts a new one with a higher log number.
#[test]
fn test_flush_starts_a_new_wal() {
    let path = DBPath::new("_rust_rocksdb_wal_flush_rolls");
    {
        let db = DB::open(&db_options(), &path).unwrap();
        db.put(b"k1", b"v1").unwrap();
        let before = db.get_current_wal_file().unwrap().log_number();

        // `SwitchMemtable` takes a fresh file number under the DB mutex before
        // the flush is scheduled, and `flush` waits for the flush, so the roll
        // has definitely happened by the time this returns.
        db.flush().unwrap();
        let after = db.get_current_wal_file().unwrap().log_number();
        assert!(
            after > before,
            "the log number should have advanced past {before}, got {after}"
        );

        // The new WAL joins the listing as soon as it has a record in it.
        db.put(b"k2", b"v2").unwrap();
        let numbers = log_numbers(&db.get_sorted_wal_files().unwrap());
        assert!(
            numbers.contains(&after),
            "the new WAL {after} is missing from {numbers:?}"
        );
        assert_strictly_ascending(&numbers);
    }
}

/// A WAL still holding unflushed data for another column family stays in the
/// listing alongside the new one, which is the case with more than one entry.
#[test]
fn test_sorted_wal_files_lists_several_wals() {
    let path = DBPath::new("_rust_rocksdb_wal_several");
    {
        let db = DB::open_cf(&db_options(), &path, ["cf1"]).unwrap();
        let cf = db.cf_handle("cf1").unwrap();

        db.put(b"default_key", b"v").unwrap();
        db.put_cf(&cf, b"cf_key", b"v").unwrap();
        let first = db.get_current_wal_file().unwrap().log_number();

        // Flushing cf1 rolls the WAL, but the default column family still has
        // an unflushed memtable recorded in the first one, so RocksDB has to
        // keep it.
        db.flush_cf(&cf).unwrap();
        let second = db.get_current_wal_file().unwrap().log_number();
        assert!(second > first);

        // Give the new WAL a record so it is not skipped as empty.
        db.put(b"default_key2", b"v").unwrap();

        let files = db.get_sorted_wal_files().unwrap();
        let numbers = log_numbers(&files);
        assert!(
            numbers.len() >= 2,
            "expected the old and new WAL, got {numbers:?}"
        );
        assert!(numbers.contains(&first), "old WAL missing from {numbers:?}");
        assert!(
            numbers.contains(&second),
            "new WAL missing from {numbers:?}"
        );
        assert_strictly_ascending(&numbers);

        for file in &files {
            assert_alive_wal_path(file);
            assert!(file.start_sequence() > 0);
        }
    }
}

/// The listing is a snapshot of plain values, so it outlives the DB.
#[test]
fn test_wal_files_outlive_the_db() {
    let path = DBPath::new("_rust_rocksdb_wal_outlives_db");
    {
        let (files, current): (WalFiles, OwnedWalFile) = {
            let db = DB::open(&db_options(), &path).unwrap();
            db.put(b"k1", b"v1").unwrap();
            (
                db.get_sorted_wal_files().unwrap(),
                db.get_current_wal_file().unwrap(),
            )
        };

        // The DB is closed. `CaptureWalFile` copied the path string and the
        // four integers into the C struct, so nothing here points back at it.
        assert!(!files.is_empty());
        let file = files.get(0).unwrap();
        assert!(file.log_number() > 0);
        assert!(!file.path_name().is_empty());
        assert_eq!(file.file_type(), WalFileType::AliveLogFile);

        assert_eq!(current.log_number(), file.log_number());
        assert_eq!(current.path_name(), file.path_name());
        assert_eq!(current.file_type(), WalFileType::AliveLogFile);
        assert_eq!(current.size_file_bytes(), file.size_file_bytes());

        // Both render without reaching back into the DB either.
        assert!(!format!("{files:?}").is_empty());
        assert!(!format!("{current:?}").is_empty());
    }
}

/// Walking the listing agrees with indexing it, in both directions.
#[test]
fn test_wal_files_iteration() {
    let path = DBPath::new("_rust_rocksdb_wal_iteration");
    {
        let db = DB::open(&db_options(), &path).unwrap();
        db.put(b"k1", b"v1").unwrap();

        let files = db.get_sorted_wal_files().unwrap();
        let len = files.len();
        assert!(len > 0);

        let by_index: Vec<u64> = (0..len)
            .map(|i| files.get(i).unwrap().log_number())
            .collect();
        assert_eq!(log_numbers(&files), by_index);

        // `&WalFiles` iterates too, and the iterator knows its own length.
        let mut by_ref = Vec::new();
        for file in &files {
            by_ref.push(file.log_number());
        }
        assert_eq!(by_ref, by_index);
        assert_eq!(files.iter().len(), len);
        assert_eq!(files.iter().size_hint(), (len, Some(len)));

        let mut reversed: Vec<u64> = files.iter().rev().map(WalFile::log_number).collect();
        reversed.reverse();
        assert_eq!(reversed, by_index);

        // Past the end is `None`, and the iterator stays exhausted.
        assert!(files.get(len).is_none());
        assert!(files.get(usize::MAX).is_none());
        let mut iter = files.iter();
        for _ in 0..len {
            assert!(iter.next().is_some());
        }
        assert!(iter.next().is_none());
        assert!(iter.next().is_none());
    }
}

/// `WalFileType` decodes the values RocksDB defines and nothing else.
#[test]
fn test_wal_file_type_conversion() {
    assert_eq!(WalFileType::from(0), WalFileType::ArchivedLogFile);
    assert_eq!(WalFileType::from(1), WalFileType::AliveLogFile);
    // Anything RocksDB might add later, and anything nonsensical, decodes as
    // unknown rather than panicking or aliasing a real variant.
    assert_eq!(WalFileType::from(2), WalFileType::Unknown);
    assert_eq!(WalFileType::from(-1), WalFileType::Unknown);
    assert_eq!(WalFileType::from(i32::MAX), WalFileType::Unknown);

    assert_eq!(WalFileType::ArchivedLogFile.as_str(), "ArchivedLogFile");
    assert_eq!(WalFileType::AliveLogFile.as_str(), "AliveLogFile");
    assert_eq!(WalFileType::Unknown.as_str(), "Unknown");
}

/// `WalReadOptions` reads back what was written.
#[test]
fn test_wal_read_options_round_trip() {
    let mut opts = WalReadOptions::default();
    assert!(
        opts.get_verify_checksums(),
        "RocksDB verifies WAL checksums by default"
    );

    opts.set_verify_checksums(false);
    assert!(!opts.get_verify_checksums());

    opts.set_verify_checksums(true);
    assert!(opts.get_verify_checksums());
}

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

mod util;

use pretty_assertions::assert_eq;
use std::path::Path;

use rust_rocksdb::checkpoint::{Checkpoint, TransactionDBCheckpoint};
use rust_rocksdb::{
    DB, DBWithThreadMode, ExportImportFilesMetaData, ImportColumnFamilyOptions, IteratorMode,
    MultiThreaded, OptimisticTransactionDB, Options, TransactionDB, TransactionDBOptions,
};
use std::fs;
use util::DBPath;

#[test]
pub fn test_single_checkpoint() {
    const PATH_PREFIX: &str = "_rust_rocksdb_cp_single_";

    // Create DB with some data
    let db_path = DBPath::new(&format!("{PATH_PREFIX}db1"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = DB::open(&opts, &db_path).unwrap();

    db.put(b"k1", b"v1").unwrap();
    db.put(b"k2", b"v2").unwrap();
    db.put(b"k3", b"v3").unwrap();
    db.put(b"k4", b"v4").unwrap();

    // Create checkpoint
    let cp1 = Checkpoint::new(&db).unwrap();
    let cp1_path = DBPath::new(&format!("{PATH_PREFIX}cp1"));
    cp1.create_checkpoint(&cp1_path).unwrap();

    // Verify checkpoint
    let cp = DB::open_default(&cp1_path).unwrap();

    assert_eq!(cp.get(b"k1").unwrap().unwrap(), b"v1");
    assert_eq!(cp.get(b"k2").unwrap().unwrap(), b"v2");
    assert_eq!(cp.get(b"k3").unwrap().unwrap(), b"v3");
    assert_eq!(cp.get(b"k4").unwrap().unwrap(), b"v4");
}

#[test]
pub fn test_multi_checkpoints() {
    const PATH_PREFIX: &str = "_rust_rocksdb_cp_multi_";

    // Create DB with some data
    let db_path = DBPath::new(&format!("{PATH_PREFIX}db1"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = DB::open(&opts, &db_path).unwrap();

    db.put(b"k1", b"v1").unwrap();
    db.put(b"k2", b"v2").unwrap();
    db.put(b"k3", b"v3").unwrap();
    db.put(b"k4", b"v4").unwrap();

    // Create first checkpoint
    let cp1 = Checkpoint::new(&db).unwrap();
    let cp1_path = DBPath::new(&format!("{PATH_PREFIX}cp1"));
    cp1.create_checkpoint(&cp1_path).unwrap();

    // Verify checkpoint
    let cp = DB::open_default(&cp1_path).unwrap();

    assert_eq!(cp.get(b"k1").unwrap().unwrap(), b"v1");
    assert_eq!(cp.get(b"k2").unwrap().unwrap(), b"v2");
    assert_eq!(cp.get(b"k3").unwrap().unwrap(), b"v3");
    assert_eq!(cp.get(b"k4").unwrap().unwrap(), b"v4");

    // Change some existing keys
    db.put(b"k1", b"modified").unwrap();
    db.put(b"k2", b"changed").unwrap();

    // Add some new keys
    db.put(b"k5", b"v5").unwrap();
    db.put(b"k6", b"v6").unwrap();

    // Create another checkpoint
    let cp2 = Checkpoint::new(&db).unwrap();
    let cp2_path = DBPath::new(&format!("{PATH_PREFIX}cp2"));
    cp2.create_checkpoint(&cp2_path).unwrap();

    // Verify second checkpoint
    let cp = DB::open_default(&cp2_path).unwrap();

    assert_eq!(cp.get(b"k1").unwrap().unwrap(), b"modified");
    assert_eq!(cp.get(b"k2").unwrap().unwrap(), b"changed");
    assert_eq!(cp.get(b"k5").unwrap().unwrap(), b"v5");
    assert_eq!(cp.get(b"k6").unwrap().unwrap(), b"v6");
}

/// Test `create_checkpoint_with_log_size` with log_size_for_flush = 0.
/// A value of 0 forces RocksDB to flush memtables before creating the checkpoint,
/// ensuring all recent writes are included.
#[test]
pub fn test_checkpoint_with_log_size_zero_forces_flush() {
    const PATH_PREFIX: &str = "_rust_rocksdb_cp_log_size_zero_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = DB::open(&opts, &db_path).unwrap();

    // Write some initial data and flush it explicitly to ensure we have
    // some materialized state in SST files
    db.put(b"flushed_key", b"flushed_value").unwrap();
    db.flush().unwrap();

    // Write additional data that will remain in the memtable (not flushed to SST)
    db.put(b"memtable_key", b"memtable_value").unwrap();

    // Create checkpoint with log_size_for_flush = 0 (forces flush)
    let cp = Checkpoint::new(&db).unwrap();
    let cp_path = DBPath::new(&format!("{PATH_PREFIX}cp"));
    cp.create_checkpoint_with_log_size(&cp_path, 0).unwrap();

    // Verify there is exactly one WAL file and it is empty (data was flushed to SST)
    let wal_files: Vec<_> = fs::read_dir((&cp_path).as_ref())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "log"))
        .collect();
    assert_eq!(
        wal_files.len(),
        1,
        "Checkpoint should contain exactly one WAL file"
    );
    let wal_metadata = wal_files[0].metadata().unwrap();
    assert_eq!(
        wal_metadata.len(),
        0,
        "WAL file should be empty when flush is forced"
    );

    // Verify checkpoint contains all data (flush was forced, so data is in SST files)
    let cp_db = DB::open_default(&cp_path).unwrap();

    assert_eq!(
        cp_db.get(b"flushed_key").unwrap().unwrap(),
        b"flushed_value"
    );
    assert_eq!(
        cp_db.get(b"memtable_key").unwrap().unwrap(),
        b"memtable_value"
    );
}

/// Test `create_checkpoint_with_log_size` with a large log_size_for_flush value.
/// A non-zero value means RocksDB skips flushing memtables if the WAL is smaller
/// than the threshold. However, the checkpoint still includes WAL files, so when
/// the checkpoint is opened, the WAL is replayed and memtable data becomes available.
#[test]
pub fn test_checkpoint_with_large_log_size_skips_flush() {
    const PATH_PREFIX: &str = "_rust_rocksdb_cp_log_size_large_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = DB::open(&opts, &db_path).unwrap();

    // Write some initial data and flush it explicitly to ensure we have
    // some materialized state in SST files
    db.put(b"flushed_key", b"flushed_value").unwrap();
    db.flush().unwrap();

    // Write additional data that will remain in the memtable (not flushed to SST)
    db.put(b"memtable_key", b"memtable_value").unwrap();

    // Create checkpoint with a very large log_size_for_flush.
    // This tells RocksDB not to force a flush unless WAL exceeds this size.
    // Since we've written very little data, the WAL should be well under this
    // threshold, so no flush should be forced.
    let cp = Checkpoint::new(&db).unwrap();
    let cp_path = DBPath::new(&format!("{PATH_PREFIX}cp"));
    let large_log_size = u64::MAX;
    cp.create_checkpoint_with_log_size(&cp_path, large_log_size)
        .unwrap();

    // Verify there is exactly one WAL file and it is not empty
    let wal_files: Vec<_> = fs::read_dir((&cp_path).as_ref())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "log"))
        .collect();
    assert_eq!(
        wal_files.len(),
        1,
        "Checkpoint should contain exactly one WAL file"
    );
    let wal_metadata = wal_files[0].metadata().unwrap();
    assert!(wal_metadata.len() > 0, "WAL file should not be empty");

    // Verify the checkpoint can be opened and contains the flushed data
    let cp_db = DB::open_default(&cp_path).unwrap();

    // The flushed key should definitely be present (it was in an SST file)
    assert_eq!(
        cp_db.get(b"flushed_key").unwrap().unwrap(),
        b"flushed_value"
    );

    // The memtable_key IS present even though no flush was forced, because
    // the checkpoint includes WAL files. When the checkpoint DB is opened,
    // the WAL is replayed, restoring the memtable data.
    assert_eq!(
        cp_db.get(b"memtable_key").unwrap().unwrap(),
        b"memtable_value"
    );
}

/// Test `create_checkpoint_with_log_size` on OptimisticTransactionDB with log_size_for_flush = 0.
/// A value of 0 forces RocksDB to flush memtables before creating the checkpoint,
/// ensuring all recent writes are included.
#[test]
pub fn test_optimistic_transaction_db_checkpoint_with_log_size_zero_forces_flush() {
    const PATH_PREFIX: &str = "_rust_rocksdb_otxn_cp_log_size_zero_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db: OptimisticTransactionDB = OptimisticTransactionDB::open(&opts, &db_path).unwrap();

    // Write some initial data and flush it explicitly to ensure we have
    // some materialized state in SST files
    db.put(b"flushed_key", b"flushed_value").unwrap();
    db.flush().unwrap();

    // Write additional data that will remain in the memtable (not flushed to SST)
    db.put(b"memtable_key", b"memtable_value").unwrap();

    // Create checkpoint with log_size_for_flush = 0 (forces flush)
    let cp = Checkpoint::new(&db).unwrap();
    let cp_path = DBPath::new(&format!("{PATH_PREFIX}cp"));
    cp.create_checkpoint_with_log_size(&cp_path, 0).unwrap();

    // Verify there is exactly one WAL file and it is empty (data was flushed to SST)
    let wal_files: Vec<_> = fs::read_dir((&cp_path).as_ref())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "log"))
        .collect();
    assert_eq!(
        wal_files.len(),
        1,
        "Checkpoint should contain exactly one WAL file"
    );
    let wal_metadata = wal_files[0].metadata().unwrap();
    assert_eq!(
        wal_metadata.len(),
        0,
        "WAL file should be empty when flush is forced"
    );

    // Verify checkpoint contains all data (flush was forced, so data is in SST files)
    let cp_db: OptimisticTransactionDB = OptimisticTransactionDB::open_default(&cp_path).unwrap();

    assert_eq!(
        cp_db.get(b"flushed_key").unwrap().unwrap(),
        b"flushed_value"
    );
    assert_eq!(
        cp_db.get(b"memtable_key").unwrap().unwrap(),
        b"memtable_value"
    );
}

/// Test `create_checkpoint` on TransactionDB flushes memtables before creating the checkpoint.
///
/// TransactionDB enables two-phase commit internally, so RocksDB flushes even when
/// the checkpoint API's log size setting would otherwise allow WAL replay.
#[test]
pub fn test_transaction_db_checkpoint_create_checkpoint_forces_flush() {
    const PATH_PREFIX: &str = "_rust_rocksdb_txn_cp_flush_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db: TransactionDB =
        TransactionDB::open(&opts, &TransactionDBOptions::default(), &db_path).unwrap();

    // Write some initial data and flush it explicitly to ensure we have
    // some materialized state in SST files.
    db.put(b"flushed_key", b"flushed_value").unwrap();
    db.flush().unwrap();

    // Write additional data that will remain in the memtable unless the
    // checkpoint creation flushes it.
    db.put(b"memtable_key", b"memtable_value").unwrap();

    let cp = TransactionDBCheckpoint::new(&db).unwrap();
    let cp_path = DBPath::new(&format!("{PATH_PREFIX}cp"));
    cp.create_checkpoint(&cp_path).unwrap();

    // Verify there is exactly one WAL file and it is empty, proving the
    // memtable data was flushed to SST instead of relying on WAL replay.
    let wal_files: Vec<_> = fs::read_dir((&cp_path).as_ref())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "log"))
        .collect();
    assert_eq!(
        wal_files.len(),
        1,
        "Checkpoint should contain exactly one WAL file"
    );
    let wal_metadata = wal_files[0].metadata().unwrap();
    assert_eq!(
        wal_metadata.len(),
        0,
        "WAL file should be empty when TransactionDB checkpoint creation flushes"
    );

    let cp_db: TransactionDB = TransactionDB::open_default(&cp_path).unwrap();

    assert_eq!(
        cp_db.get(b"flushed_key").unwrap().unwrap(),
        b"flushed_value"
    );
    assert_eq!(
        cp_db.get(b"memtable_key").unwrap().unwrap(),
        b"memtable_value"
    );
}

/// Test `create_checkpoint_with_log_size` on OptimisticTransactionDB with a large log_size_for_flush value.
/// A non-zero value means RocksDB skips flushing memtables if the WAL is smaller
/// than the threshold. However, the checkpoint still includes WAL files, so when
/// the checkpoint is opened, the WAL is replayed and memtable data becomes available.
#[test]
pub fn test_optimistic_transaction_db_checkpoint_with_large_log_size_skips_flush() {
    const PATH_PREFIX: &str = "_rust_rocksdb_otxn_cp_log_size_large_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db: OptimisticTransactionDB = OptimisticTransactionDB::open(&opts, &db_path).unwrap();

    // Write some initial data and flush it explicitly to ensure we have
    // some materialized state in SST files
    db.put(b"flushed_key", b"flushed_value").unwrap();
    db.flush().unwrap();

    // Write additional data that will remain in the memtable (not flushed to SST)
    db.put(b"memtable_key", b"memtable_value").unwrap();

    // Create checkpoint with a very large log_size_for_flush.
    // This tells RocksDB not to force a flush unless WAL exceeds this size.
    // Since we've written very little data, the WAL should be well under this
    // threshold, so no flush should be forced.
    let cp = Checkpoint::new(&db).unwrap();
    let cp_path = DBPath::new(&format!("{PATH_PREFIX}cp"));
    let large_log_size = u64::MAX;
    cp.create_checkpoint_with_log_size(&cp_path, large_log_size)
        .unwrap();

    // Verify there is exactly one WAL file and it is not empty
    let wal_files: Vec<_> = fs::read_dir((&cp_path).as_ref())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "log"))
        .collect();
    assert_eq!(
        wal_files.len(),
        1,
        "Checkpoint should contain exactly one WAL file"
    );
    let wal_metadata = wal_files[0].metadata().unwrap();
    assert!(wal_metadata.len() > 0, "WAL file should not be empty");

    // Verify the checkpoint can be opened and contains the flushed data
    let cp_db: OptimisticTransactionDB = OptimisticTransactionDB::open_default(&cp_path).unwrap();

    // The flushed key should definitely be present (it was in an SST file)
    assert_eq!(
        cp_db.get(b"flushed_key").unwrap().unwrap(),
        b"flushed_value"
    );

    // The memtable_key IS present even though no flush was forced, because
    // the checkpoint includes WAL files. When the checkpoint DB is opened,
    // the WAL is replayed, restoring the memtable data.
    assert_eq!(
        cp_db.get(b"memtable_key").unwrap().unwrap(),
        b"memtable_value"
    );
}

/// Test that TransactionDB checkpoints include both direct writes and committed transaction writes.
#[test]
pub fn test_transaction_db_checkpoint_includes_direct_and_committed_writes() {
    const PATH_PREFIX: &str = "_rust_rocksdb_txn_cp_basic_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db"));
    let db: TransactionDB = TransactionDB::open_default(&db_path).unwrap();

    db.put(b"direct_key", b"direct_value").unwrap();

    let txn = db.transaction();
    txn.put(b"txn_key", b"txn_value").unwrap();
    txn.commit().unwrap();

    let cp = TransactionDBCheckpoint::new(&db).unwrap();
    let cp_path = DBPath::new(&format!("{PATH_PREFIX}cp"));
    cp.create_checkpoint(&cp_path).unwrap();

    let cp_db: TransactionDB = TransactionDB::open_default(&cp_path).unwrap();

    assert_eq!(cp_db.get(b"direct_key").unwrap().unwrap(), b"direct_value");
    assert_eq!(cp_db.get(b"txn_key").unwrap().unwrap(), b"txn_value");
}

/// Test that TransactionDB checkpoints do not include uncommitted transaction writes.
#[test]
pub fn test_transaction_db_checkpoint_excludes_uncommitted_writes() {
    const PATH_PREFIX: &str = "_rust_rocksdb_txn_cp_uncommitted_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db"));
    let db: TransactionDB = TransactionDB::open_default(&db_path).unwrap();

    db.put(b"committed_key", b"committed_value").unwrap();

    let txn = db.transaction();
    txn.put(b"uncommitted_key", b"uncommitted_value").unwrap();

    let cp = TransactionDBCheckpoint::new(&db).unwrap();
    let cp_path = DBPath::new(&format!("{PATH_PREFIX}cp"));
    cp.create_checkpoint(&cp_path).unwrap();

    let cp_db: TransactionDB = TransactionDB::open_default(&cp_path).unwrap();

    assert_eq!(
        cp_db.get(b"committed_key").unwrap().unwrap(),
        b"committed_value"
    );
    assert!(cp_db.get(b"uncommitted_key").unwrap().is_none());

    txn.rollback().unwrap();
}

/// Test that the same TransactionDB checkpoint object can create multiple checkpoints.
#[test]
pub fn test_transaction_db_checkpoint_object_can_create_multiple_checkpoints() {
    const PATH_PREFIX: &str = "_rust_rocksdb_txn_cp_multi_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db"));
    let db: TransactionDB = TransactionDB::open_default(&db_path).unwrap();

    db.put(b"k1", b"v1").unwrap();

    let cp = TransactionDBCheckpoint::new(&db).unwrap();
    let cp1_path = DBPath::new(&format!("{PATH_PREFIX}cp1"));
    cp.create_checkpoint(&cp1_path).unwrap();

    db.put(b"k2", b"v2").unwrap();

    let cp2_path = DBPath::new(&format!("{PATH_PREFIX}cp2"));
    cp.create_checkpoint(&cp2_path).unwrap();

    let cp1_db: TransactionDB = TransactionDB::open_default(&cp1_path).unwrap();
    assert_eq!(cp1_db.get(b"k1").unwrap().unwrap(), b"v1");
    assert!(cp1_db.get(b"k2").unwrap().is_none());

    let cp2_db: TransactionDB = TransactionDB::open_default(&cp2_path).unwrap();
    assert_eq!(cp2_db.get(b"k1").unwrap().unwrap(), b"v1");
    assert_eq!(cp2_db.get(b"k2").unwrap().unwrap(), b"v2");
}

/// Test that TransactionDB checkpoints preserve column family data.
#[test]
pub fn test_transaction_db_checkpoint_preserves_column_families() {
    const PATH_PREFIX: &str = "_rust_rocksdb_txn_cp_cf_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db"));
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);
    let db: TransactionDB = TransactionDB::open_cf(
        &opts,
        &TransactionDBOptions::default(),
        &db_path,
        ["cf1", "cf2"],
    )
    .unwrap();

    let cf1 = db.cf_handle("cf1").unwrap();
    let cf2 = db.cf_handle("cf2").unwrap();

    db.put(b"default_key", b"default_value").unwrap();
    db.put_cf(&cf1, b"cf1_key", b"cf1_value").unwrap();
    db.put_cf(&cf2, b"cf2_key", b"cf2_value").unwrap();

    let cp = TransactionDBCheckpoint::new(&db).unwrap();
    let cp_path = DBPath::new(&format!("{PATH_PREFIX}cp"));
    cp.create_checkpoint(&cp_path).unwrap();

    let cp_db: TransactionDB = TransactionDB::open_cf(
        &Options::default(),
        &TransactionDBOptions::default(),
        &cp_path,
        ["cf1", "cf2"],
    )
    .unwrap();

    let cp_cf1 = cp_db.cf_handle("cf1").unwrap();
    let cp_cf2 = cp_db.cf_handle("cf2").unwrap();

    assert_eq!(
        cp_db.get(b"default_key").unwrap().unwrap(),
        b"default_value"
    );
    assert_eq!(
        cp_db.get_cf(&cp_cf1, b"cf1_key").unwrap().unwrap(),
        b"cf1_value"
    );
    assert_eq!(
        cp_db.get_cf(&cp_cf2, b"cf2_key").unwrap().unwrap(),
        b"cf2_value"
    );
}

/// Test that TransactionDB checkpoint creation returns an error if the destination exists.
#[test]
pub fn test_transaction_db_checkpoint_fails_if_path_exists() {
    const PATH_PREFIX: &str = "_rust_rocksdb_txn_cp_existing_path_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db"));
    let db: TransactionDB = TransactionDB::open_default(&db_path).unwrap();

    db.put(b"k1", b"v1").unwrap();

    let cp_path = DBPath::new(&format!("{PATH_PREFIX}cp"));
    fs::create_dir_all((&cp_path).as_ref()).unwrap();

    let cp = TransactionDBCheckpoint::new(&db).unwrap();
    let err = cp.create_checkpoint(&cp_path).unwrap_err();

    assert!(
        !err.to_string().is_empty(),
        "checkpoint error should include RocksDB failure details"
    );
}

/// Test that proves memtable data in a checkpoint is only available via WAL replay.
/// We create two checkpoints with large log_size_for_flush (no flush forced), then
/// truncate the WAL in one checkpoint. The checkpoint with intact WAL has the
/// memtable_key, while the checkpoint with truncated WAL does not.
#[test]
pub fn test_checkpoint_wal_truncation_loses_memtable_data() {
    const PATH_PREFIX: &str = "_rust_rocksdb_cp_wal_truncate_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = DB::open(&opts, &db_path).unwrap();

    // Write some initial data and flush it explicitly to ensure we have
    // some materialized state in SST files
    db.put(b"flushed_key", b"flushed_value").unwrap();
    db.flush().unwrap();

    // Write additional data that will remain in the memtable (not flushed to SST)
    db.put(b"memtable_key", b"memtable_value").unwrap();

    // Create two checkpoints with large log_size_for_flush (no flush forced)
    let cp = Checkpoint::new(&db).unwrap();
    let large_log_size = u64::MAX;

    let cp_intact_path = DBPath::new(&format!("{PATH_PREFIX}cp_intact"));
    cp.create_checkpoint_with_log_size(&cp_intact_path, large_log_size)
        .unwrap();

    let cp_truncated_path = DBPath::new(&format!("{PATH_PREFIX}cp_truncated"));
    cp.create_checkpoint_with_log_size(&cp_truncated_path, large_log_size)
        .unwrap();

    // Truncate the WAL in the second checkpoint
    let wal_files: Vec<_> = fs::read_dir((&cp_truncated_path).as_ref())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "log"))
        .map(|entry| entry.path())
        .collect();
    for wal_file in &wal_files {
        fs::write(wal_file, b"").unwrap();
    }

    // Open the checkpoint with intact WAL - both keys should be present
    let cp_db_intact = DB::open_default(&cp_intact_path).unwrap();
    assert_eq!(
        cp_db_intact.get(b"flushed_key").unwrap().unwrap(),
        b"flushed_value"
    );
    assert_eq!(
        cp_db_intact.get(b"memtable_key").unwrap().unwrap(),
        b"memtable_value",
        "memtable_key should be present when WAL is intact"
    );

    // Open the checkpoint with truncated WAL - only flushed_key should be present
    let cp_db_truncated = DB::open_default(&cp_truncated_path).unwrap();
    assert_eq!(
        cp_db_truncated.get(b"flushed_key").unwrap().unwrap(),
        b"flushed_value"
    );
    assert!(
        cp_db_truncated.get(b"memtable_key").unwrap().is_none(),
        "memtable_key should be absent when WAL is truncated"
    );
}

/// Test that checkpoint with WAL over 50MB threshold triggers a flush.
/// When WAL exceeds the threshold at checkpoint creation time, RocksDB
/// flushes memtables, resulting in an empty WAL in the checkpoint.
///
/// Note: This test carefully writes data to get WAL just over 50 MiB
/// (52,428,800 bytes) but under the 64MB memtable auto-flush limit.
///
/// IGNORED: There is a bug in RocksDB where `log_size_for_flush` is completely
/// non-functional when set to a non-zero value. The issue is in
/// `WalManager::GetSortedWalFiles()` which returns early without populating
/// the WAL files list when `include_archived=false`, causing
/// `GetLiveFilesStorageInfo()` to calculate WAL size as 0. This means the
/// condition `0 < log_size_for_flush` is always true, skipping the flush.
///
/// See: https://github.com/facebook/rocksdb/pull/14193
#[test]
#[ignore]
fn test_checkpoint_wal_over_threshold_is_flushed() {
    const PATH_PREFIX: &str = "_rust_rocksdb_cp_wal_threshold_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = DB::open(&opts, &db_path).unwrap();

    // Write enough data to exceed 50 MiB WAL threshold but stay under 64MB memtable limit
    // 50 MiB = 52,428,800 bytes
    let threshold = 50 * 1024 * 1024_u64; // 50 MiB
    let value = vec![b'x'; 1024]; // 1KB values
    let mut i = 0;
    loop {
        let key = format!("key_{:08}", i);
        db.put(key.as_bytes(), &value).unwrap();
        i += 1;

        // Check WAL size periodically
        if i % 1000 == 0 {
            let wal_size: u64 = fs::read_dir((&db_path).as_ref())
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "log"))
                .map(|e| e.metadata().unwrap().len())
                .sum();
            if wal_size > threshold {
                break;
            }
        }
    }

    // Create checkpoint with 50 MiB threshold - since WAL exceeds this, flush should trigger
    let cp = Checkpoint::new(&db).unwrap();
    let cp_path = DBPath::new(&format!("{PATH_PREFIX}cp"));
    cp.create_checkpoint_with_log_size(&cp_path, threshold)
        .unwrap();

    // Verify checkpoint has empty WAL (data was flushed to SST)
    let cp_wal_size: u64 = fs::read_dir((&cp_path).as_ref())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "log"))
        .map(|e| e.metadata().unwrap().len())
        .sum();

    assert_eq!(
        cp_wal_size, 0,
        "Checkpoint WAL should be empty when WAL size exceeds log_size_for_flush threshold"
    );

    // Verify checkpoint has SST files
    let cp_sst_count = fs::read_dir((&cp_path).as_ref())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "sst"))
        .count();

    assert!(
        cp_sst_count > 0,
        "Checkpoint should contain SST files when flush is triggered"
    );

    // Verify data is accessible in checkpoint
    let cp_db = DB::open_default(&cp_path).unwrap();
    assert!(cp_db.get(b"key_00000000").unwrap().is_some());
}

#[test]
pub fn test_export_checkpoint_column_family() {
    const PATH_PREFIX: &str = "_rust_rocksdb_cf_export_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db-src"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = DBWithThreadMode::<MultiThreaded>::open(&opts, &db_path).unwrap();

    let opts = Options::default();
    db.create_cf("cf1", &opts).unwrap();
    db.create_cf("cf2", &opts).unwrap();

    let cf1 = db.cf_handle("cf1").unwrap();
    db.put_cf(&cf1, b"k1", b"v1").unwrap();
    db.put_cf(&cf1, b"k2", b"v2").unwrap();

    let cf2 = db.cf_handle("cf2").unwrap();
    db.put_cf(&cf2, b"k1", b"v1_cf2").unwrap();
    db.put_cf(&cf2, b"k2", b"v2_cf2").unwrap();

    // The CF will be checkpointed at the time of export, not when the struct is created
    let cp = Checkpoint::new(&db).unwrap();

    db.flush_cf(&cf1).expect("flush succeeds"); // Create an additonal SST to export
    db.delete_cf(&cf1, b"k2").unwrap();
    db.put_cf(&cf1, b"k3", b"v3").unwrap();

    let cf1_export_path = DBPath::new(&format!("{PATH_PREFIX}cf1-export"));
    let export_metadata = cp.export_column_family(&cf1, &cf1_export_path).unwrap();

    // Modify the column family after export - these changes will NOT be observable
    db.put_cf(&cf1, b"k4", b"v4").unwrap();
    db.delete_cf(&cf1, b"k1").unwrap();

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db-dest"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db_new = DBWithThreadMode::<MultiThreaded>::open(&opts, &db_path).unwrap();

    // Prepopulate some data in the destination DB - this should remain intact after import
    {
        db_new.create_cf("cf0", &opts).unwrap();
        let cf0 = db_new.cf_handle("cf0").unwrap();
        db_new.put_cf(&cf0, b"k1", b"v0").unwrap();
        db_new.put_cf(&cf0, b"k5", b"v5").unwrap();
    }

    let export_files = export_metadata.get_files();
    assert_eq!(export_files.len(), 2);

    export_files.iter().for_each(|export_file| {
        assert!(export_file.column_family_name.is_empty()); // CF export does not have the CF name
        assert!(!export_file.name.is_empty());
        assert!(!export_file.directory.is_empty());
    });

    let mut import_metadata = ExportImportFilesMetaData::default();
    import_metadata.set_db_comparator_name(&export_metadata.get_db_comparator_name());
    import_metadata.set_files(&export_files.to_vec()).unwrap();

    let cf_opts = Options::default();
    let mut import_opts = ImportColumnFamilyOptions::default();
    import_opts.set_move_files(true);
    db_new
        .create_column_family_with_import(&cf_opts, "cf1-new", &import_opts, &import_metadata)
        .unwrap();

    assert!(export_files.iter().all(|export_file| {
        !Path::new(&export_file.directory)
            .join(&export_file.name)
            .exists()
    }));

    let cf1_new = db_new.cf_handle("cf1-new").unwrap();
    let imported_data: Vec<_> = db_new
        .iterator_cf(&cf1_new, IteratorMode::Start)
        .map(Result::unwrap)
        .map(|(k, v)| {
            (
                String::from_utf8_lossy(&k).into_owned(),
                String::from_utf8_lossy(&v).into_owned(),
            )
        })
        .collect();
    assert_eq!(
        vec![
            ("k1".to_string(), "v1".to_string()),
            ("k3".to_string(), "v3".to_string()),
        ],
        imported_data,
    );

    let cf0 = db_new.cf_handle("cf0").unwrap();
    let original_data: Vec<_> = db_new
        .iterator_cf(&cf0, IteratorMode::Start)
        .map(Result::unwrap)
        .map(|(k, v)| {
            (
                String::from_utf8_lossy(&k).into_owned(),
                String::from_utf8_lossy(&v).into_owned(),
            )
        })
        .collect();
    assert_eq!(
        vec![
            ("k1".to_string(), "v0".to_string()),
            ("k5".to_string(), "v5".to_string()),
        ],
        original_data,
    );
}

#[test]
pub fn test_export_transaction_db_checkpoint_column_family() {
    const PATH_PREFIX: &str = "_rust_rocksdb_txn_cf_export_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db-src"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);
    let db: TransactionDB = TransactionDB::open_cf(
        &opts,
        &TransactionDBOptions::default(),
        &db_path,
        ["cf1", "cf2"],
    )
    .unwrap();

    let cf1 = db.cf_handle("cf1").unwrap();
    db.put_cf(&cf1, b"k1", b"v1").unwrap();
    db.put_cf(&cf1, b"k2", b"v2").unwrap();

    let cf2 = db.cf_handle("cf2").unwrap();
    db.put_cf(&cf2, b"k1", b"v1_cf2").unwrap();

    let cp = TransactionDBCheckpoint::new(&db).unwrap();

    db.flush_cf(&cf1).expect("flush succeeds");
    db.delete_cf(&cf1, b"k2").unwrap();
    db.put_cf(&cf1, b"k3", b"v3").unwrap();

    let cf1_export_path = DBPath::new(&format!("{PATH_PREFIX}cf1-export"));
    let export_metadata = cp.export_column_family(&cf1, &cf1_export_path).unwrap();

    db.put_cf(&cf1, b"k4", b"v4").unwrap();
    db.delete_cf(&cf1, b"k1").unwrap();

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db-dest"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db_new = DBWithThreadMode::<MultiThreaded>::open(&opts, &db_path).unwrap();

    // Prepopulate some data in the destination DB - this should remain intact after import
    {
        db_new.create_cf("cf0", &opts).unwrap();
        let cf0 = db_new.cf_handle("cf0").unwrap();
        db_new.put_cf(&cf0, b"k1", b"v0").unwrap();
        db_new.put_cf(&cf0, b"k5", b"v5").unwrap();
    }

    let export_files = export_metadata.get_files();
    assert!(!export_files.is_empty());

    export_files.iter().for_each(|export_file| {
        assert!(export_file.column_family_name.is_empty());
        assert!(!export_file.name.is_empty());
        assert!(!export_file.directory.is_empty());
    });

    let mut import_metadata = ExportImportFilesMetaData::default();
    import_metadata.set_db_comparator_name(&export_metadata.get_db_comparator_name());
    import_metadata.set_files(&export_files.to_vec()).unwrap();

    let cf_opts = Options::default();
    let mut import_opts = ImportColumnFamilyOptions::default();
    import_opts.set_move_files(true);
    db_new
        .create_column_family_with_import(&cf_opts, "cf1-new", &import_opts, &import_metadata)
        .unwrap();

    assert!(export_files.iter().all(|export_file| {
        !Path::new(&export_file.directory)
            .join(&export_file.name)
            .exists()
    }));

    let cf1_new = db_new.cf_handle("cf1-new").unwrap();
    let imported_data: Vec<_> = db_new
        .iterator_cf(&cf1_new, IteratorMode::Start)
        .map(Result::unwrap)
        .map(|(k, v)| {
            (
                String::from_utf8_lossy(&k).into_owned(),
                String::from_utf8_lossy(&v).into_owned(),
            )
        })
        .collect();
    assert_eq!(
        vec![
            ("k1".to_string(), "v1".to_string()),
            ("k3".to_string(), "v3".to_string()),
        ],
        imported_data,
    );

    let cf0 = db_new.cf_handle("cf0").unwrap();
    let original_data: Vec<_> = db_new
        .iterator_cf(&cf0, IteratorMode::Start)
        .map(Result::unwrap)
        .map(|(k, v)| {
            (
                String::from_utf8_lossy(&k).into_owned(),
                String::from_utf8_lossy(&v).into_owned(),
            )
        })
        .collect();
    assert_eq!(
        vec![
            ("k1".to_string(), "v0".to_string()),
            ("k5".to_string(), "v5".to_string()),
        ],
        original_data,
    );
}

// Copyright 2021 Yiyuan Liu
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

use std::ptr::NonNull;
use std::sync::Arc;

use libc::c_int;

use crate::ffi;

pub struct TransactionOptions {
    pub(crate) inner: *mut ffi::rocksdb_transaction_options_t,
}

unsafe impl Send for TransactionOptions {}
unsafe impl Sync for TransactionOptions {}

impl Default for TransactionOptions {
    fn default() -> Self {
        let txn_opts = unsafe { ffi::rocksdb_transaction_options_create() };
        assert!(
            !txn_opts.is_null(),
            "Could not create RocksDB transaction options"
        );
        Self { inner: txn_opts }
    }
}

impl TransactionOptions {
    pub fn new() -> TransactionOptions {
        TransactionOptions::default()
    }

    pub fn set_skip_prepare(&mut self, skip_prepare: bool) {
        unsafe {
            ffi::rocksdb_transaction_options_set_skip_prepare(self.inner, u8::from(skip_prepare));
        }
    }

    /// Specifies use snapshot or not.
    ///
    /// Default: false.
    ///
    /// If a transaction has a snapshot set, the transaction will ensure that
    /// any keys successfully written(or fetched via `get_for_update`) have not
    /// been modified outside this transaction since the time the snapshot was
    /// set.
    /// If a snapshot has not been set, the transaction guarantees that keys have
    /// not been modified since the time each key was first written (or fetched via
    /// `get_for_update`).
    ///
    /// Using snapshot will provide stricter isolation guarantees at the
    /// expense of potentially more transaction failures due to conflicts with
    /// other writes.
    ///
    /// Calling `set_snapshot` will not affect the version of Data returned by `get`
    /// methods.
    pub fn set_snapshot(&mut self, snapshot: bool) {
        unsafe {
            ffi::rocksdb_transaction_options_set_set_snapshot(self.inner, u8::from(snapshot));
        }
    }

    /// Specifies whether detect deadlock or not.
    ///
    /// Setting to true means that before acquiring locks, this transaction will
    /// check if doing so will cause a deadlock. If so, it will return with
    /// Status::Busy.  The user should retry their transaction.
    ///
    /// Default: false.
    pub fn set_deadlock_detect(&mut self, deadlock_detect: bool) {
        unsafe {
            ffi::rocksdb_transaction_options_set_deadlock_detect(
                self.inner,
                u8::from(deadlock_detect),
            );
        }
    }

    /// Specifies the wait timeout in milliseconds when a transaction attempts to lock a key.
    ///
    /// If 0, no waiting is done if a lock cannot instantly be acquired.
    /// If negative, transaction lock timeout in `TransactionDBOptions` will be used.
    ///
    /// Default: -1.
    pub fn set_lock_timeout(&mut self, lock_timeout: i64) {
        unsafe {
            ffi::rocksdb_transaction_options_set_lock_timeout(self.inner, lock_timeout);
        }
    }

    /// Specifies expiration duration in milliseconds.
    ///
    /// If non-negative, transactions that last longer than this many milliseconds will fail to commit.
    /// If not set, a forgotten transaction that is never committed, rolled back, or deleted
    /// will never relinquish any locks it holds.  This could prevent keys from being accessed by other writers.
    ///
    /// Default: -1.
    pub fn set_expiration(&mut self, expiration: i64) {
        unsafe {
            ffi::rocksdb_transaction_options_set_expiration(self.inner, expiration);
        }
    }

    /// Specifies the number of traversals to make during deadlock detection.
    ///
    /// Default: 50.
    pub fn set_deadlock_detect_depth(&mut self, depth: i64) {
        unsafe {
            ffi::rocksdb_transaction_options_set_deadlock_detect_depth(self.inner, depth);
        }
    }

    /// Specifies the maximum number of bytes used for the write batch. 0 means no limit.
    ///
    /// Default: 0.
    pub fn set_max_write_batch_size(&mut self, size: usize) {
        unsafe {
            ffi::rocksdb_transaction_options_set_max_write_batch_size(self.inner, size);
        }
    }

    /// The following three options enable optimizations for large transaction commit to
    /// bypass memtable write.
    /// - If any transaction's commit should bybass memtable write, set
    ///   commit_bypass_memtable to true.
    /// - If only bypass memtable write for transactions with >= n operations, set
    ///   commit_bypass_memtable to false, large_txn_commit_optimize_threshold to n, and
    ///   large_txn_commit_optimize_byte_threshold to 0. Similarly for only optimize when a
    ///   transaction's write batch size is >= n.
    /// - If bypass memtable write for transactions with >= n operations or >= x bytes, set
    ///   commit_bypass_memtable to false, large_txn_commit_optimize_threshold to n, and
    ///   large_txn_commit_optimize_byte_threshold to x.
    ///
    /// EXPERIMENTAL, SUBJECT TO CHANGE Only supports write-committed policy. If set to true,
    /// the transaction will skip memtable write and ingest into the DB directly during
    /// Commit(). This makes Commit() much faster for transactions with many operations.
    /// Transaction neeeds to call Prepare() before Commit() for this option to take effect.
    /// Transactions with Merge() or PutEntity() is not supported yet.
    ///
    /// Note that the transaction will be ingested as an immutable memtable for CFs it
    /// updates, and the current memtable will be switched to a new one. So ingesting many
    /// transactions in a short period of time may cause stall due to too many memtables. Note
    /// that the ingestion relies on the transaction's underlying index,
    /// (WriteBatchWithIndex), so updates that are added to the transaction without indexing
    /// (i.e. added directly to the transaction underlying write batch through
    /// Transaction::GetWriteBatch()->GetWriteBatch()) are not supported, and the optimization
    /// will not apply in that case.
    ///
    /// NOTE: since WBWI keep track of the most recent update per key, a Put followed by a
    /// SingleDelete will be written to DB as a SingleDelete. This can cause flush/compaction
    /// to report `num_single_del_mismatch` due to consecutive SingleDeletes.
    pub fn set_commit_bypass_memtable(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_transaction_options_set_commit_bypass_memtable(self.inner, u8::from(val));
        }
    }

    /// Returns the value of the `commit_bypass_memtable` option.
    pub fn get_commit_bypass_memtable(&self) -> bool {
        unsafe { ffi::rocksdb_transaction_options_get_commit_bypass_memtable(self.inner) != 0 }
    }

    /// Setting to true means that before acquiring locks, this transaction will check if
    /// doing so will cause a deadlock. If so, it will return with Status::Busy.  The user
    /// should retry their transaction.
    pub fn get_deadlock_detect(&self) -> bool {
        unsafe { ffi::rocksdb_transaction_options_get_deadlock_detect(self.inner) != 0 }
    }

    /// EXPERIMENTAL, SUBJECT TO CHANGE When the size of a transaction's write batch is at
    /// least this threshold, we will enable optimizations for commiting a large transaction.
    /// See comment for `commit_bypass_memtable` for more optimization detail.
    ///
    /// Default: 0 (disabled).
    pub fn set_large_txn_commit_optimize_byte_threshold(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_transaction_options_set_large_txn_commit_optimize_byte_threshold(
                self.inner, val,
            );
        }
    }

    /// Returns the value of the `large_txn_commit_optimize_byte_threshold` option.
    pub fn get_large_txn_commit_optimize_byte_threshold(&self) -> u64 {
        unsafe {
            ffi::rocksdb_transaction_options_get_large_txn_commit_optimize_byte_threshold(
                self.inner,
            )
        }
    }

    /// EXPERIMENTAL, SUBJECT TO CHANGE When the number of updates in a transaction is at
    /// least this threshold, we will enable optimizations for commiting a large transaction.
    /// See comment for `commit_bypass_memtable` for more optimization detail.
    ///
    /// Default: 0 (disabled).
    pub fn set_large_txn_commit_optimize_threshold(&mut self, val: u32) {
        unsafe {
            ffi::rocksdb_transaction_options_set_large_txn_commit_optimize_threshold(
                self.inner, val,
            );
        }
    }

    /// Returns the value of the `large_txn_commit_optimize_threshold` option.
    pub fn get_large_txn_commit_optimize_threshold(&self) -> u32 {
        unsafe {
            ffi::rocksdb_transaction_options_get_large_txn_commit_optimize_threshold(self.inner)
        }
    }

    /// The maximum number of bytes used for the write batch. 0 means no limit.
    pub fn get_max_write_batch_size(&self) -> usize {
        unsafe { ffi::rocksdb_transaction_options_get_max_write_batch_size(self.inner) }
    }

    /// Setting set_snapshot=true is the same as calling Transaction::SetSnapshot().
    pub fn get_set_snapshot(&self) -> bool {
        unsafe { ffi::rocksdb_transaction_options_get_set_snapshot(self.inner) != 0 }
    }

    /// If true, the TransactionDB implementation might skip concurrency control unless it is
    /// overridden by TransactionOptions or TransactionDBWriteOptimizations. This can be used
    /// in conjunction with DBOptions::unordered_write when the TransactionDB is used solely
    /// for write ordering rather than concurrency control.
    pub fn set_skip_concurrency_control(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_transaction_options_set_skip_concurrency_control(
                self.inner,
                u8::from(val),
            );
        }
    }

    /// Returns the value of the `skip_concurrency_control` option.
    pub fn get_skip_concurrency_control(&self) -> bool {
        unsafe { ffi::rocksdb_transaction_options_get_skip_concurrency_control(self.inner) != 0 }
    }

    /// In pessimistic transaction, if this is true, then you can skip Prepare before Commit,
    /// otherwise, you must Prepare before Commit.
    pub fn get_skip_prepare(&self) -> bool {
        unsafe { ffi::rocksdb_transaction_options_get_skip_prepare(self.inner) != 0 }
    }

    /// If set, it states that the CommitTimeWriteBatch represents the latest state of the
    /// application, has only one sub-batch, i.e., no duplicate keys,  and meant to be used
    /// later during recovery. It enables an optimization to postpone updating the memtable
    /// with CommitTimeWriteBatch to only SwitchMemtable or recovery. This option does not
    /// affect write-committed. Only write-prepared/write-unprepared transactions will be
    /// affected.
    pub fn set_use_only_the_last_commit_time_batch_for_recovery(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_transaction_options_set_use_only_the_last_commit_time_batch_for_recovery(
                self.inner,
                u8::from(val),
            );
        }
    }

    /// Returns the value of the `use_only_the_last_commit_time_batch_for_recovery` option.
    pub fn get_use_only_the_last_commit_time_batch_for_recovery(&self) -> bool {
        unsafe {
            ffi::rocksdb_transaction_options_get_use_only_the_last_commit_time_batch_for_recovery(
                self.inner,
            ) != 0
        }
    }

    /// DO NOT USE. This is only a temporary option dedicated for MyRocks that will soon be
    /// removed. In normal use cases, meta info like column family's timestamp size is tracked
    /// at the transaction layer, so it's not necessary and even detrimental to track such
    /// info inside the internal WriteBatch because it may let anti-patterns like bypassing
    /// Transaction write APIs and directly write to its internal `WriteBatch` retrieved like
    /// this:
    /// <https://github.com/facebook/mysql-5.6/blob/fb-mysql-8.0.32/storage/rocksdb/ha_rocksdb.cc#L4949-L4950>
    /// Setting this option to true will keep aforementioned use case continue to work before
    /// it's refactored out. When this flag is enabled, we also intentionally only track the
    /// timestamp size in APIs that MyRocks currently are using, including Put, Merge, Delete
    /// DeleteRange, SingleDelete.
    pub fn set_write_batch_track_timestamp_size(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_transaction_options_set_write_batch_track_timestamp_size(
                self.inner,
                u8::from(val),
            );
        }
    }

    /// Returns the value of the `write_batch_track_timestamp_size` option.
    pub fn get_write_batch_track_timestamp_size(&self) -> bool {
        unsafe {
            ffi::rocksdb_transaction_options_get_write_batch_track_timestamp_size(self.inner) != 0
        }
    }

    /// Returns the current `deadlock_detect_depth` setting.
    ///
    /// See [`Self::set_deadlock_detect_depth`] for what this controls.
    pub fn get_deadlock_detect_depth(&self) -> i64 {
        unsafe { ffi::rocksdb_transaction_options_get_deadlock_detect_depth(self.inner) }
    }

    /// Returns the current `deadlock_timeout_us` setting.
    ///
    /// See [`Self::set_deadlock_timeout_us`] for what this controls.
    pub fn get_deadlock_timeout_us(&self) -> i64 {
        unsafe { ffi::rocksdb_transaction_options_get_deadlock_timeout_us(self.inner) }
    }

    /// Returns the current `expiration` setting.
    ///
    /// See [`Self::set_expiration`] for what this controls.
    pub fn get_expiration(&self) -> i64 {
        unsafe { ffi::rocksdb_transaction_options_get_expiration(self.inner) }
    }

    /// Returns the current `lock_timeout` setting.
    ///
    /// See [`Self::set_lock_timeout`] for what this controls.
    pub fn get_lock_timeout(&self) -> i64 {
        unsafe { ffi::rocksdb_transaction_options_get_lock_timeout(self.inner) }
    }

    /// Returns the current `write_batch_flush_threshold` setting.
    ///
    /// See [`Self::set_write_batch_flush_threshold`] for what this controls.
    pub fn get_write_batch_flush_threshold(&self) -> i64 {
        unsafe { ffi::rocksdb_transaction_options_get_write_batch_flush_threshold(self.inner) }
    }

    /// Timeout in microseconds before perform dead lock detection. If 0, deadlock detection
    /// will be performed immediately.
    ///
    /// To optimize performance, this parameter could be tuned.
    ///
    /// When deadlock happens very frequently, deadlock timeout should be set to 0, so
    /// deadlock will be detected immediately.
    ///
    /// When deadlock happen very rarely, this timeout could be turned to be slightly longer
    /// than the typical transaction execution time, so that transaction will be waked up to
    /// take the lock before this timeout, which will allow the transaction to save the CPU
    /// time on deadlock detection.
    ///
    /// Deadlock timeout is always smaller than lock_timeout.
    pub fn set_deadlock_timeout_us(&mut self, val: i64) {
        unsafe {
            ffi::rocksdb_transaction_options_set_deadlock_timeout_us(self.inner, val);
        }
    }

    /// See TransactionDBOptions::default_write_batch_flush_threshold for description. If a
    /// negative value is specified, then the default value from TransactionDBOptions is used.
    pub fn set_write_batch_flush_threshold(&mut self, val: i64) {
        unsafe {
            ffi::rocksdb_transaction_options_set_write_batch_flush_threshold(self.inner, val);
        }
    }
}

impl Drop for TransactionOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_transaction_options_destroy(self.inner);
        }
    }
}

/// When a [`TransactionDB`] writes transaction data into the DB.
///
/// [`TransactionDB`] reads this once while opening, to choose which implementation to
/// build, and keeps its own copy. Changing it on a [`TransactionDBOptions`] afterwards
/// has no effect on a DB that is already open.
///
/// [`TransactionDB`]: crate::TransactionDB
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub enum TxnDBWritePolicy {
    /// Write data at commit time.
    ///
    /// Only committed data ever reaches the DB, so readers need no extra machinery to
    /// tell committed from uncommitted. This is the default.
    WriteCommitted = ffi::rocksdb_txndb_write_policy_write_committed as isize,

    /// Write data after the prepare phase of two-phase commit.
    ///
    /// Data lands before the commit, so the DB has to track which of it is committed and
    /// readers have to consult that. Experimental: RocksDB describes the non-committed
    /// policies as less mature, less validated and less compatible with other features
    /// than `WriteCommitted`.
    WritePrepared = ffi::rocksdb_txndb_write_policy_write_prepared as isize,

    /// Write data before the prepare phase of two-phase commit.
    ///
    /// A running transaction can flush its pending writes into the DB once its batch
    /// crosses
    /// [`TransactionDBOptions::set_default_write_batch_flush_threshold`], which keeps
    /// memory bounded for very large transactions. Experimental, and the least mature of
    /// the three.
    WriteUnprepared = ffi::rocksdb_txndb_write_policy_write_unprepared as isize,
}

impl TxnDBWritePolicy {
    /// Decodes a raw `rocksdb::TxnDBWritePolicy`.
    ///
    /// This covers every policy RocksDB defines today, so `None` only means a future
    /// release added one.
    pub(crate) fn try_from_raw(raw: c_int) -> Option<Self> {
        match raw {
            n if n == TxnDBWritePolicy::WriteCommitted as c_int => {
                Some(TxnDBWritePolicy::WriteCommitted)
            }
            n if n == TxnDBWritePolicy::WritePrepared as c_int => {
                Some(TxnDBWritePolicy::WritePrepared)
            }
            n if n == TxnDBWritePolicy::WriteUnprepared as c_int => {
                Some(TxnDBWritePolicy::WriteUnprepared)
            }
            _ => None,
        }
    }
}

pub struct TransactionDBOptions {
    pub(crate) inner: *mut ffi::rocksdb_transactiondb_options_t,
}

unsafe impl Send for TransactionDBOptions {}
unsafe impl Sync for TransactionDBOptions {}

impl Default for TransactionDBOptions {
    fn default() -> Self {
        let txn_db_opts = unsafe { ffi::rocksdb_transactiondb_options_create() };
        assert!(
            !txn_db_opts.is_null(),
            "Could not create RocksDB transaction_db options"
        );
        Self { inner: txn_db_opts }
    }
}

impl TransactionDBOptions {
    pub fn new() -> TransactionDBOptions {
        TransactionDBOptions::default()
    }

    /// Specifies the wait timeout in milliseconds when writing a key
    /// outside a transaction (i.e. by calling `TransactionDB::put` directly).
    ///
    /// If 0, no waiting is done if a lock cannot instantly be acquired.
    /// If negative, there is no timeout and will block indefinitely when acquiring
    /// a lock.
    ///
    /// Not using a timeout can lead to deadlocks.  Currently, there
    /// is no deadlock-detection to recover from a deadlock.  While DB writes
    /// cannot deadlock with other DB writes, they can deadlock with a transaction.
    /// A negative timeout should only be used if all transactions have a small
    /// expiration set.
    ///
    /// Default: 1000(1s).
    pub fn set_default_lock_timeout(&mut self, default_lock_timeout: i64) {
        unsafe {
            ffi::rocksdb_transactiondb_options_set_default_lock_timeout(
                self.inner,
                default_lock_timeout,
            );
        }
    }

    /// Specifies the default wait timeout in milliseconds when a transaction
    /// attempts to lock a key if not specified in `TransactionOptions`.
    ///
    /// If 0, no waiting is done if a lock cannot instantly be acquired.
    /// If negative, there is no timeout.  Not using a timeout is not recommended
    /// as it can lead to deadlocks.  Currently, there is no deadlock-detection to
    /// recover from a deadlock.
    ///
    /// Default: 1000(1s).
    pub fn set_txn_lock_timeout(&mut self, txn_lock_timeout: i64) {
        unsafe {
            ffi::rocksdb_transactiondb_options_set_transaction_lock_timeout(
                self.inner,
                txn_lock_timeout,
            );
        }
    }

    /// Specifies the maximum number of keys that can be locked at the same time
    /// per column family.
    ///
    /// If the number of locked keys is greater than `max_num_locks`, transaction
    /// `writes` (or `get_for_update`) will return an error.
    /// If this value is not positive, no limit will be enforced.
    ///
    /// Default: -1.
    pub fn set_max_num_locks(&mut self, max_num_locks: i64) {
        unsafe {
            ffi::rocksdb_transactiondb_options_set_max_num_locks(self.inner, max_num_locks);
        }
    }

    /// Specifies lock table stripes count.
    ///
    /// Increasing this value will increase the concurrency by dividing the lock
    /// table (per column family) into more sub-tables, each with their own
    /// separate mutex.
    ///
    /// Default: 16.
    pub fn set_num_stripes(&mut self, num_stripes: usize) {
        unsafe {
            ffi::rocksdb_transactiondb_options_set_num_stripes(self.inner, num_stripes);
        }
    }

    /// A flag to control for the whole DB whether user-defined timestamp based validation are
    /// enabled when applicable. Only WriteCommittedTxn support user-defined timestamps so
    /// this option only applies in this case.
    pub fn set_enable_udt_validation(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_transactiondb_options_set_enable_udt_validation(self.inner, u8::from(val));
        }
    }

    /// Returns the value of the `enable_udt_validation` option.
    pub fn get_enable_udt_validation(&self) -> bool {
        unsafe { ffi::rocksdb_transactiondb_options_get_enable_udt_validation(self.inner) != 0 }
    }

    /// Stores the number of latest deadlocks to track
    pub fn set_max_num_deadlocks(&mut self, val: u32) {
        unsafe {
            ffi::rocksdb_transactiondb_options_set_max_num_deadlocks(self.inner, val);
        }
    }

    /// Returns the value of the `max_num_deadlocks` option.
    pub fn get_max_num_deadlocks(&self) -> u32 {
        unsafe { ffi::rocksdb_transactiondb_options_get_max_num_deadlocks(self.inner) }
    }

    /// Increasing this value will increase the concurrency by dividing the lock table (per
    /// column family) into more sub-tables, each with their own separate mutex.
    pub fn get_num_stripes(&self) -> usize {
        unsafe { ffi::rocksdb_transactiondb_options_get_num_stripes(self.inner) }
    }

    /// TODO(myabandeh): remove this option Note: this is a temporary option as a hot fix in
    /// rollback of writeprepared txns in myrocks. MyRocks uses merge operands for autoinc
    /// column id without however obtaining locks. This breaks the assumption behind the
    /// rollback logic in myrocks. This hack of simply not rolling back merge operands works
    /// for the special way that myrocks uses this operands.
    pub fn set_rollback_merge_operands(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_transactiondb_options_set_rollback_merge_operands(
                self.inner,
                u8::from(val),
            );
        }
    }

    /// Returns the value of the `rollback_merge_operands` option.
    pub fn get_rollback_merge_operands(&self) -> bool {
        unsafe { ffi::rocksdb_transactiondb_options_get_rollback_merge_operands(self.inner) != 0 }
    }

    /// If true, the TransactionDB implementation might skip concurrency control unless it is
    /// overridden by TransactionOptions or TransactionDBWriteOptimizations. This can be used
    /// in conjunction with DBOptions::unordered_write when the TransactionDB is used solely
    /// for write ordering rather than concurrency control.
    pub fn set_skip_concurrency_control(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_transactiondb_options_set_skip_concurrency_control(
                self.inner,
                u8::from(val),
            );
        }
    }

    /// Returns the value of the `skip_concurrency_control` option.
    pub fn get_skip_concurrency_control(&self) -> bool {
        unsafe { ffi::rocksdb_transactiondb_options_get_skip_concurrency_control(self.inner) != 0 }
    }

    /// Deprecated, this option has no effect and may be removed in the future. Use
    /// TransactionOptions::large_txn_commit_optimize_threshold instead.
    ///
    /// This option is only valid for write committed. If the number of updates in a
    /// transaction is at least this threshold, then the transaction commit will skip
    /// insertions into memtable as an optimization to reduce commit latency. See comment for
    /// TransactionOptions::commit_bypass_memtable for more detail. Setting
    /// TransactionOptions::commit_bypass_memtable to true takes precedence over this option.
    pub fn set_txn_commit_bypass_memtable_threshold(&mut self, val: u32) {
        unsafe {
            ffi::rocksdb_transactiondb_options_set_txn_commit_bypass_memtable_threshold(
                self.inner, val,
            );
        }
    }

    /// Returns the value of the `txn_commit_bypass_memtable_threshold` option.
    pub fn get_txn_commit_bypass_memtable_threshold(&self) -> u32 {
        unsafe {
            ffi::rocksdb_transactiondb_options_get_txn_commit_bypass_memtable_threshold(self.inner)
        }
    }

    /// EXPERIMENTAL
    ///
    /// Flag to enable/disable the per key point lock manager.
    pub fn set_use_per_key_point_lock_mgr(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_transactiondb_options_set_use_per_key_point_lock_mgr(
                self.inner,
                u8::from(val),
            );
        }
    }

    /// Returns the value of the `use_per_key_point_lock_mgr` option.
    pub fn get_use_per_key_point_lock_mgr(&self) -> bool {
        unsafe {
            ffi::rocksdb_transactiondb_options_get_use_per_key_point_lock_mgr(self.inner) != 0
        }
    }

    /// Returns the current `default_lock_timeout` setting.
    ///
    /// See [`Self::set_default_lock_timeout`] for what this controls.
    pub fn get_default_lock_timeout(&self) -> i64 {
        unsafe { ffi::rocksdb_transactiondb_options_get_default_lock_timeout(self.inner) }
    }

    /// Returns the current `default_write_batch_flush_threshold` setting.
    ///
    /// See [`Self::set_default_write_batch_flush_threshold`] for what this controls.
    pub fn get_default_write_batch_flush_threshold(&self) -> i64 {
        unsafe {
            ffi::rocksdb_transactiondb_options_get_default_write_batch_flush_threshold(self.inner)
        }
    }

    /// Returns the current `max_num_locks` setting.
    ///
    /// See [`Self::set_max_num_locks`] for what this controls.
    pub fn get_max_num_locks(&self) -> i64 {
        unsafe { ffi::rocksdb_transactiondb_options_get_max_num_locks(self.inner) }
    }

    /// If positive, specifies the default wait timeout in milliseconds when a transaction
    /// attempts to lock a key if not specified by TransactionOptions::lock_timeout.
    ///
    /// If 0, no waiting is done if a lock cannot instantly be acquired. If negative, there is
    /// no timeout.  Not using a timeout is not recommended as it can lead to deadlocks.
    /// Currently, there is no deadlock-detection to recover from a deadlock.
    pub fn get_transaction_lock_timeout(&self) -> i64 {
        unsafe { ffi::rocksdb_transactiondb_options_get_transaction_lock_timeout(self.inner) }
    }

    /// This option is only valid for write unprepared. If a write batch exceeds this
    /// threshold, then the transaction will implicitly flush the currently pending writes
    /// into the database. A value of 0 or less means no limit.
    pub fn set_default_write_batch_flush_threshold(&mut self, val: i64) {
        unsafe {
            ffi::rocksdb_transactiondb_options_set_default_write_batch_flush_threshold(
                self.inner, val,
            );
        }
    }

    /// Sets when transaction data becomes visible in the DB.
    ///
    /// Only read while opening a [`TransactionDB`], so this has to be set before the
    /// open call. See [`TxnDBWritePolicy`] for what each policy costs.
    ///
    /// Default: [`TxnDBWritePolicy::WriteCommitted`].
    ///
    /// [`TransactionDB`]: crate::TransactionDB
    pub fn set_write_policy(&mut self, policy: TxnDBWritePolicy) {
        unsafe {
            ffi::rocksdb_transactiondb_options_set_write_policy(self.inner, policy as c_int);
        }
    }

    /// The write policy set by [`Self::set_write_policy`].
    ///
    /// [`TxnDBWritePolicy`] covers every policy RocksDB defines today, so `None` only
    /// shows up if a future release adds one.
    pub fn get_write_policy(&self) -> Option<TxnDBWritePolicy> {
        let raw = unsafe { ffi::rocksdb_transactiondb_options_get_write_policy(self.inner) };
        TxnDBWritePolicy::try_from_raw(raw)
    }
}

impl Drop for TransactionDBOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_transactiondb_options_destroy(self.inner);
        }
    }
}

pub struct OptimisticTransactionOptions {
    pub(crate) inner: *mut ffi::rocksdb_optimistictransaction_options_t,
}

unsafe impl Send for OptimisticTransactionOptions {}
unsafe impl Sync for OptimisticTransactionOptions {}

impl Default for OptimisticTransactionOptions {
    fn default() -> Self {
        let txn_opts = unsafe { ffi::rocksdb_optimistictransaction_options_create() };
        assert!(
            !txn_opts.is_null(),
            "Could not create RocksDB optimistic transaction options"
        );
        Self { inner: txn_opts }
    }
}

impl OptimisticTransactionOptions {
    pub fn new() -> OptimisticTransactionOptions {
        OptimisticTransactionOptions::default()
    }

    /// Specifies use snapshot or not.
    ///
    /// Default: false.
    ///
    /// If a transaction has a snapshot set, the transaction will ensure that
    /// any keys successfully written(or fetched via `get_for_update`) have not
    /// been modified outside the transaction since the time the snapshot was
    /// set.
    /// If a snapshot has not been set, the transaction guarantees that keys have
    /// not been modified since the time each key was first written (or fetched via
    /// `get_for_update`).
    ///
    /// Using snapshot will provide stricter isolation guarantees at the
    /// expense of potentially more transaction failures due to conflicts with
    /// other writes.
    ///
    /// Calling `set_snapshot` will not affect the version of Data returned by `get`
    /// methods.
    pub fn set_snapshot(&mut self, snapshot: bool) {
        unsafe {
            ffi::rocksdb_optimistictransaction_options_set_set_snapshot(
                self.inner,
                u8::from(snapshot),
            );
        }
    }

    /// Setting set_snapshot=true is the same as calling Transaction::SetSnapshot().
    pub fn get_set_snapshot(&self) -> bool {
        unsafe { ffi::rocksdb_optimistictransaction_options_get_set_snapshot(self.inner) != 0 }
    }
}

impl Drop for OptimisticTransactionOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_optimistictransaction_options_destroy(self.inner);
        }
    }
}

/// How an [`OptimisticTransactionDB`] checks a transaction for write conflicts at commit.
///
/// [`OptimisticTransactionDB`]: crate::OptimisticTransactionDB
// The C API has no constants for these, unlike `TxnDBWritePolicy`. The discriminants are
// spelled out in `rocksdb::OccValidationPolicy` in
// include/rocksdb/utilities/optimistic_transaction_db.h and must stay in step with it.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub enum OccValidationPolicy {
    /// Validate serially at commit, after the transaction has entered the write group.
    ///
    /// Validation is single threaded because the write group is. Simple, but it can
    /// suffer from high mutex contention. See
    /// <https://github.com/facebook/rocksdb/issues/4402>.
    ValidateSerial = 0,

    /// Validate in parallel before the transaction enters the write group.
    ///
    /// Each transaction takes locks for its write set in a well defined order, off the
    /// write group, which cuts the mutex contention. This is the default. The locks come
    /// from the pool sized by [`OptimisticTransactionDBOptions::set_occ_lock_buckets`]
    /// or supplied by [`OptimisticTransactionDBOptions::set_shared_lock_buckets`].
    ValidateParallel = 1,
}

impl OccValidationPolicy {
    /// Decodes a raw `rocksdb::OccValidationPolicy`.
    ///
    /// This covers every policy RocksDB defines today, so `None` only means a future
    /// release added one.
    pub(crate) fn try_from_raw(raw: c_int) -> Option<Self> {
        match raw {
            n if n == OccValidationPolicy::ValidateSerial as c_int => {
                Some(OccValidationPolicy::ValidateSerial)
            }
            n if n == OccValidationPolicy::ValidateParallel as c_int => {
                Some(OccValidationPolicy::ValidateParallel)
            }
            _ => None,
        }
    }
}

struct OccLockBucketsWrapper {
    inner: NonNull<ffi::rocksdb_optimistictransactiondb_lock_buckets_t>,
}

// The C handle is a `std::shared_ptr<OccLockBuckets>` and nothing else, so moving it
// between threads moves a refcounted pointer. Every call this crate makes on it either
// reads the pool's memory usage through a `const` method or copies the `shared_ptr` into
// an options struct, and shared_ptr refcounting is atomic. The `Arc` in `OccLockBuckets`
// is what guarantees the `_destroy` call is unshared.
unsafe impl Send for OccLockBucketsWrapper {}
unsafe impl Sync for OccLockBucketsWrapper {}

impl Drop for OccLockBucketsWrapper {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_optimistictransactiondb_lock_buckets_destroy(self.inner.as_ptr());
        }
    }
}

/// A pool of mutex locks used to validate optimistic transactions.
///
/// Only consulted when the validation policy is
/// [`OccValidationPolicy::ValidateParallel`]. Hand it to
/// [`OptimisticTransactionDBOptions::set_shared_lock_buckets`] to make several databases
/// validate against one pool instead of each allocating its own.
///
/// Cloning is cheap and produces another handle to the same pool. The pool itself lives
/// as long as any handle or any options value or database that was given one, so it is
/// fine to drop every Rust handle right after the `set_shared_lock_buckets` call.
#[derive(Clone)]
pub struct OccLockBuckets(Arc<OccLockBucketsWrapper>);

impl OccLockBuckets {
    /// Allocates a pool of `bucket_count` locks.
    ///
    /// More buckets mean less contention between transactions validating at the same
    /// time, and more memory. `cache_aligned` pads each lock out to a cache line, which
    /// avoids false sharing between neighbouring buckets at the cost of more memory
    /// again. RocksDB has historically used `false` here.
    ///
    /// # Panics
    ///
    /// Panics if RocksDB could not allocate the pool.
    pub fn new(bucket_count: usize, cache_aligned: bool) -> Self {
        let inner = NonNull::new(unsafe {
            ffi::rocksdb_optimistictransactiondb_lock_buckets_create(
                bucket_count,
                u8::from(cache_aligned),
            )
        })
        .expect("Could not create RocksDB OCC lock buckets");
        Self(Arc::new(OccLockBucketsWrapper { inner }))
    }

    /// Estimates how much memory this pool occupies, in bytes.
    pub fn approximate_memory_usage(&self) -> usize {
        unsafe {
            ffi::rocksdb_optimistictransactiondb_lock_buckets_approximate_memory_usage(
                self.0.inner.as_ptr(),
            )
        }
    }
}

/// Options for opening an [`OptimisticTransactionDB`].
///
/// These control conflict validation only. Everything else about the database still
/// comes from [`Options`], which is passed alongside these to the `open` call.
///
/// [`OptimisticTransactionDB`]: crate::OptimisticTransactionDB
/// [`Options`]: crate::Options
pub struct OptimisticTransactionDBOptions {
    pub(crate) inner: *mut ffi::rocksdb_optimistictransactiondb_options_t,
}

// The C handle wraps a plain `OptimisticTransactionDBOptions` struct: an enum, a
// `uint32_t` and a `shared_ptr`. It owns no thread-local or thread-affine state, so it
// can move between threads. Every mutating call here takes `&mut self`, so a shared `&`
// only ever reaches the getters, which read those fields.
unsafe impl Send for OptimisticTransactionDBOptions {}
unsafe impl Sync for OptimisticTransactionDBOptions {}

impl Default for OptimisticTransactionDBOptions {
    fn default() -> Self {
        let otxn_db_opts = unsafe { ffi::rocksdb_optimistictransactiondb_options_create() };
        assert!(
            !otxn_db_opts.is_null(),
            "Could not create RocksDB optimistic transaction_db options"
        );
        Self {
            inner: otxn_db_opts,
        }
    }
}

impl OptimisticTransactionDBOptions {
    pub fn new() -> OptimisticTransactionDBOptions {
        OptimisticTransactionDBOptions::default()
    }

    /// Specifies how transactions are checked for write conflicts at commit.
    ///
    /// Default: [`OccValidationPolicy::ValidateParallel`].
    pub fn set_validate_policy(&mut self, policy: OccValidationPolicy) {
        unsafe {
            ffi::rocksdb_optimistictransactiondb_options_set_validate_policy(
                self.inner,
                policy as c_int,
            );
        }
    }

    /// The validation policy set by [`Self::set_validate_policy`].
    ///
    /// [`OccValidationPolicy`] covers every policy RocksDB defines today, so `None` only
    /// shows up if a future release adds one.
    pub fn get_validate_policy(&self) -> Option<OccValidationPolicy> {
        let raw =
            unsafe { ffi::rocksdb_optimistictransactiondb_options_get_validate_policy(self.inner) };
        OccValidationPolicy::try_from_raw(raw)
    }

    /// Specifies how many striped mutex locks to allocate for validating transactions.
    ///
    /// Only used when the validation policy is
    /// [`OccValidationPolicy::ValidateParallel`] and no pool was supplied through
    /// [`Self::set_shared_lock_buckets`]. A larger count potentially reduces contention
    /// but uses more memory.
    ///
    /// Default: 1048576 (1 << 20).
    pub fn set_occ_lock_buckets(&mut self, num_buckets: u32) {
        unsafe {
            ffi::rocksdb_optimistictransactiondb_options_set_occ_lock_buckets(
                self.inner,
                num_buckets,
            );
        }
    }

    /// Returns the value of the `occ_lock_buckets` option.
    pub fn get_occ_lock_buckets(&self) -> u32 {
        unsafe { ffi::rocksdb_optimistictransactiondb_options_get_occ_lock_buckets(self.inner) }
    }

    /// Validates against `buckets` instead of a pool private to this database.
    ///
    /// Give the same [`OccLockBuckets`] to several databases and they share one set of
    /// locks, which bounds the total memory spent on validation. Ignored unless the
    /// validation policy is [`OccValidationPolicy::ValidateParallel`], and it overrides
    /// [`Self::set_occ_lock_buckets`] when set.
    ///
    /// These options take their own reference to the pool, so `buckets` may be dropped
    /// as soon as this returns.
    pub fn set_shared_lock_buckets(&mut self, buckets: &OccLockBuckets) {
        unsafe {
            ffi::rocksdb_optimistictransactiondb_options_set_shared_lock_buckets(
                self.inner,
                buckets.0.inner.as_ptr(),
            );
        }
    }
}

impl Drop for OptimisticTransactionDBOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_optimistictransactiondb_options_destroy(self.inner);
        }
    }
}

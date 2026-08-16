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

//! Inputs to a manual compaction and read only views of what one did.
//!
//! Two independent halves live here.
//!
//! [`CompactionOptions`] is the owned options object for `CompactFiles`, the manual
//! compaction entry point that takes an explicit list of input files. It carries the output
//! compression, the output file size limit, the subcompaction count, the trivial move
//! switch, the output temperature override, and an optional [`CompactionCancellationToken`]
//! that lets another thread abort the job while it runs.
//!
//! Everything else is a borrowed view over data RocksDB hands to an event listener:
//! [`CompactionJobStats`], [`CompactionFileInfo`], [`BlobFileAdditionInfo`], and
//! [`BlobFileGarbageInfo`]. You never build or own one of these. RocksDB owns the underlying
//! object and it stays alive only as long as the job info it was read from, so the `'a`
//! lifetime ties each view and every byte slice it hands back to that borrow.
//!
//! String-like getters return raw bytes rather than `str`. RocksDB does not guarantee UTF-8
//! for key prefixes or file paths, and the slices point straight into the C++ strings, so
//! reading them copies and allocates nothing.

use std::marker::PhantomData;
use std::sync::Arc;

use libc::{c_char, c_int, c_uchar};

use crate::ffi_util::bytes_from_raw;
use crate::{DBCompressionType, Temperature, ffi};

/// `kDisableCompressionOption` from `include/rocksdb/compression_type.h`.
///
/// This is the default for `CompactionOptions::compression` and is not a compression
/// algorithm. It tells RocksDB to pick the output compression from the column family
/// options instead.
const DISABLE_COMPRESSION_OPTION: c_int = 0xff;

/// A cancellation flag that aborts an in progress `CompactFiles` job.
///
/// Create one, hand it to [`CompactionOptions::set_canceled`], keep a clone of the [`Arc`]
/// somewhere else, and call [`cancel`](Self::cancel) from that other thread to stop the
/// compaction. Cancellation is one shot and best effort. The compaction iterator checks the
/// flag as it walks the input, so the job stops at the next check rather than immediately,
/// and upstream notes that cancellation can be delayed waiting on automatic compactions when
/// `exclusive_manual_compaction` is set.
///
/// There is no C API to read the flag back, so this wrapper is write only and there is no
/// way to un-cancel. Use a fresh token per compaction.
pub struct CompactionCancellationToken {
    inner: *mut c_uchar,
}

// SAFETY: the `unsigned char*` in the C API is a lie of convenience. Every access treats it
// as a `std::atomic<bool>*`: `rocksdb_compaction_options_canceled_create` allocates one with
// `new std::atomic<bool>(false)` (db/c.cc:7236), `rocksdb_compaction_options_canceled_set`
// does an atomic store through it (db/c.cc:7248), the compaction thread reads it with
// `manual_compaction_canceled_.load(std::memory_order_relaxed)`
// (db/compaction/compaction_iterator.h:653), and `..._canceled_destroy` deletes it as
// `std::atomic<bool>*` (db/c.cc:7241). Setting the flag on one thread while a background
// compaction polls it is therefore an atomic access rather than a data race, which is the
// whole point of the token, so sharing `&CompactionCancellationToken` across threads is
// sound. Moving one between threads is sound for the same reason.
unsafe impl Send for CompactionCancellationToken {}
unsafe impl Sync for CompactionCancellationToken {}

impl Default for CompactionCancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CompactionCancellationToken {
    /// Allocates a fresh token in the not cancelled state.
    pub fn new() -> Self {
        let inner = unsafe { ffi::rocksdb_compaction_options_canceled_create() };
        assert!(
            !inner.is_null(),
            "Could not create RocksDB compaction cancellation token"
        );
        Self { inner }
    }

    /// Signals that the compaction using this token should stop.
    ///
    /// Returns as soon as the flag is stored. The compaction winds down on its own thread and
    /// the `CompactFiles` call it belongs to then fails with
    /// [`ErrorKind::Incomplete`](crate::ErrorKind::Incomplete) and the message
    /// `Result incomplete: Manual compaction paused`.
    pub fn cancel(&self) {
        unsafe {
            ffi::rocksdb_compaction_options_canceled_set(self.inner, 1);
        }
    }

    /// The raw flag pointer, for handing to `rocksdb_compaction_options_set_canceled`.
    fn as_ptr(&self) -> *mut c_uchar {
        self.inner
    }
}

impl Drop for CompactionCancellationToken {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_compaction_options_canceled_destroy(self.inner);
        }
    }
}

/// Options for a `CompactFiles` call, which compacts an explicit list of input files.
///
/// These are RocksDB's `CompactionOptions` from `include/rocksdb/options.h`, not the
/// `CompactRangeOptions` behind [`CompactOptions`](crate::CompactOptions).
pub struct CompactionOptions {
    pub(crate) inner: *mut ffi::rocksdb_compaction_options_t,
    /// Keeps the cancellation flag alive for as long as the C struct points at it. See
    /// [`Self::set_canceled`] for why this is an `Arc` and not a lifetime.
    canceled: Option<Arc<CompactionCancellationToken>>,
}

// SAFETY: the C struct is a plain `CompactionOptions` value (db/c.cc:453) with no interior
// mutability and no thread affinity. Every setter here takes `&mut self`, so the raw pointer
// is never aliased mutably, and the getters only read. The cancellation flag it can point at
// is an atomic owned by a `Send + Sync` token.
unsafe impl Send for CompactionOptions {}
unsafe impl Sync for CompactionOptions {}

impl Default for CompactionOptions {
    fn default() -> Self {
        let inner = unsafe { ffi::rocksdb_compaction_options_create() };
        assert!(
            !inner.is_null(),
            "Could not create RocksDB compaction options"
        );
        Self {
            inner,
            canceled: None,
        }
    }
}

impl Drop for CompactionOptions {
    fn drop(&mut self) {
        // The C struct holds a borrowed pointer to the token's flag, so it has to go first.
        // `canceled` is dropped after this body returns, which is the right order.
        unsafe {
            ffi::rocksdb_compaction_options_destroy(self.inner);
        }
    }
}

impl CompactionOptions {
    /// Sets the compression used for the compaction output.
    ///
    /// Deprecated upstream, because the `CompressionOptions` still come from the column
    /// family options and so the algorithm picked here can end up paired with tuning meant
    /// for a different one. Unset by default. [`Self::unset_compression`] puts it back.
    pub fn set_compression(&mut self, t: DBCompressionType) {
        unsafe {
            ffi::rocksdb_compaction_options_set_compression(self.inner, t as c_int);
        }
    }

    /// Restores the default, letting RocksDB choose the output compression from the column
    /// family options.
    ///
    /// RocksDB takes the output level into account, so level specific settings still apply.
    pub fn unset_compression(&mut self) {
        unsafe {
            ffi::rocksdb_compaction_options_set_compression(self.inner, DISABLE_COMPRESSION_OPTION);
        }
    }

    /// The compression set for the compaction output, or `None` when RocksDB will pick it
    /// from the column family options.
    ///
    /// `None` also covers a compression type this crate does not name, currently only xpress,
    /// which is Windows only.
    pub fn get_compression(&self) -> Option<DBCompressionType> {
        let raw = unsafe { ffi::rocksdb_compaction_options_get_compression(self.inner) };
        DBCompressionType::try_from_raw(raw)
    }

    /// Caps the size of each file the compaction creates.
    ///
    /// Defaults to `u64::MAX`, which means the compaction writes a single output file.
    pub fn set_output_file_size_limit(&mut self, v: u64) {
        unsafe {
            ffi::rocksdb_compaction_options_set_output_file_size_limit(self.inner, v);
        }
    }

    /// The current output file size limit.
    pub fn get_output_file_size_limit(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_options_get_output_file_size_limit(self.inner) }
    }

    /// Overrides `DBOptions::max_subcompactions` for this compaction when greater than 0.
    ///
    /// Defaults to 0, meaning the DB level setting wins.
    pub fn set_max_subcompactions(&mut self, v: u32) {
        unsafe {
            ffi::rocksdb_compaction_options_set_max_subcompactions(self.inner, v);
        }
    }

    /// The current subcompaction override, 0 when the DB level setting is in effect.
    pub fn get_max_subcompactions(&self) -> u32 {
        unsafe { ffi::rocksdb_compaction_options_get_max_subcompactions(self.inner) }
    }

    /// Lets the compaction move non overlapping input files to the output level instead of
    /// rewriting them.
    ///
    /// Defaults to false.
    pub fn set_allow_trivial_move(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_compaction_options_set_allow_trivial_move(self.inner, c_uchar::from(v));
        }
    }

    /// Whether trivial moves are allowed for this compaction.
    pub fn get_allow_trivial_move(&self) -> bool {
        unsafe { ffi::rocksdb_compaction_options_get_allow_trivial_move(self.inner) != 0 }
    }

    /// Writes the output files with this file temperature.
    ///
    /// Leaving it at the default [`Temperature::Unknown`] means no override: the output
    /// falls back to `last_level_temperature` when the output level is the last level and
    /// to `default_write_temperature` otherwise.
    pub fn set_output_temperature_override(&mut self, v: Temperature) {
        unsafe {
            ffi::rocksdb_compaction_options_set_output_temperature_override(self.inner, v as c_int);
        }
    }

    /// The output temperature override, [`Temperature::Unknown`] when nothing is
    /// overridden.
    pub fn get_output_temperature_override(&self) -> Temperature {
        let raw =
            unsafe { ffi::rocksdb_compaction_options_get_output_temperature_override(self.inner) };
        Temperature::from(raw)
    }

    /// Attaches a cancellation token so another thread can abort this compaction.
    ///
    /// The C side stores the token as a borrowed `std::atomic<bool>*` inside the options
    /// struct, so the flag has to outlive both these options and the `CompactFiles` call that
    /// reads them. Holding an [`Arc`] clone enforces that at runtime and keeps
    /// `CompactionOptions` free of a lifetime parameter, which would otherwise spread to
    /// every signature that passes the options around. Sharing the token is also the normal
    /// case, since something on another thread has to own a handle in order to cancel, and
    /// that already wants an `Arc`.
    pub fn set_canceled(&mut self, token: Arc<CompactionCancellationToken>) {
        let ptr = token.as_ptr();
        // Point the C struct at the new flag before replacing the field, so a token being
        // swapped out is only released once nothing references it.
        unsafe {
            ffi::rocksdb_compaction_options_set_canceled(self.inner, ptr);
        }
        self.canceled = Some(token);
    }

    /// Detaches the cancellation token, if any, and releases this object's share of it.
    pub fn clear_canceled(&mut self) {
        if self.canceled.is_none() {
            return;
        }
        unsafe {
            ffi::rocksdb_compaction_options_set_canceled(self.inner, std::ptr::null_mut());
        }
        self.canceled = None;
    }

    /// The cancellation token attached by [`Self::set_canceled`], if there is one.
    ///
    /// Handed back as the [`Arc`] so you can clone another handle out of it.
    pub fn canceled(&self) -> Option<&Arc<CompactionCancellationToken>> {
        self.canceled.as_ref()
    }
}

/// Shared signature of the `rocksdb_compaction_job_stats_*_output_key_prefix` getters.
type KeyPrefixGetter =
    unsafe extern "C" fn(*const ffi::rocksdb_compaction_job_stats_t, *mut usize) -> *const c_char;

/// What one compaction job did, borrowed from the event that reported it.
///
/// Read from a compaction job info or a subcompaction job info. Counters that are not
/// applicable to the compaction, or that RocksDB was not asked to collect, read back as 0.
pub struct CompactionJobStats<'a> {
    inner: *const ffi::rocksdb_compaction_job_stats_t,
    _marker: PhantomData<&'a ()>,
}

impl<'a> CompactionJobStats<'a> {
    /// Wraps a compaction job stats pointer owned by RocksDB.
    ///
    /// # Safety
    ///
    /// `inner` must point to a live `rocksdb_compaction_job_stats_t` that stays valid for all
    /// of `'a`. RocksDB owns the object, so the caller must never free it and must not pick
    /// an `'a` that outlives the compaction or subcompaction job info it was read from.
    pub(crate) unsafe fn from_ptr(
        inner: *const ffi::rocksdb_compaction_job_stats_t,
    ) -> CompactionJobStats<'a> {
        CompactionJobStats {
            inner,
            _marker: PhantomData,
        }
    }

    /// Wall clock time this compaction took, in microseconds.
    pub fn elapsed_micros(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_elapsed_micros(self.inner) }
    }

    /// CPU time this compaction took, in microseconds.
    pub fn cpu_micros(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_cpu_micros(self.inner) }
    }

    /// Whether [`Self::num_input_records`] is accurate across all subcompactions.
    pub fn has_accurate_num_input_records(&self) -> bool {
        unsafe { ffi::rocksdb_compaction_job_stats_has_accurate_num_input_records(self.inner) != 0 }
    }

    /// Number of compaction input records. Only trustworthy when
    /// [`Self::has_accurate_num_input_records`] is true.
    pub fn num_input_records(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_num_input_records(self.inner) }
    }

    /// Number of blobs read from blob files.
    pub fn num_blobs_read(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_num_blobs_read(self.inner) }
    }

    /// Number of compaction input files, counting table files only.
    pub fn num_input_files(&self) -> usize {
        unsafe { ffi::rocksdb_compaction_job_stats_num_input_files(self.inner) }
    }

    /// Number of compaction input table files that were already at the output level.
    pub fn num_input_files_at_output_level(&self) -> usize {
        unsafe { ffi::rocksdb_compaction_job_stats_num_input_files_at_output_level(self.inner) }
    }

    /// Number of compaction input files filtered out by compaction optimizations.
    pub fn num_filtered_input_files(&self) -> usize {
        unsafe { ffi::rocksdb_compaction_job_stats_num_filtered_input_files(self.inner) }
    }

    /// Number of compaction input files at the output level that were filtered out by
    /// compaction optimizations.
    pub fn num_filtered_input_files_at_output_level(&self) -> usize {
        unsafe {
            ffi::rocksdb_compaction_job_stats_num_filtered_input_files_at_output_level(self.inner)
        }
    }

    /// Number of compaction output records.
    pub fn num_output_records(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_num_output_records(self.inner) }
    }

    /// Number of compaction output table files.
    pub fn num_output_files(&self) -> usize {
        unsafe { ffi::rocksdb_compaction_job_stats_num_output_files(self.inner) }
    }

    /// Number of compaction output blob files.
    pub fn num_output_files_blob(&self) -> usize {
        unsafe { ffi::rocksdb_compaction_job_stats_num_output_files_blob(self.inner) }
    }

    /// Whether this was a full compaction, meaning every live SST file was an input.
    pub fn is_full_compaction(&self) -> bool {
        unsafe { ffi::rocksdb_compaction_job_stats_is_full_compaction(self.inner) != 0 }
    }

    /// Whether this was a manual compaction.
    pub fn is_manual_compaction(&self) -> bool {
        unsafe { ffi::rocksdb_compaction_job_stats_is_manual_compaction(self.inner) != 0 }
    }

    /// Whether the compaction ran in a remote worker.
    ///
    /// Only the compaction completed event carries the truth. On the compaction begin event
    /// RocksDB sets this to true whenever a `compaction_service` is configured, before it
    /// knows whether the job will really be scheduled remotely or fall back to local.
    pub fn is_remote_compaction(&self) -> bool {
        unsafe { ffi::rocksdb_compaction_job_stats_is_remote_compaction(self.inner) != 0 }
    }

    /// Total size of the table files in the compaction input.
    pub fn total_input_bytes(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_total_input_bytes(self.inner) }
    }

    /// Total size of the input table files that were skipped because compaction
    /// optimizations filtered them out.
    pub fn total_skipped_input_bytes(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_total_skipped_input_bytes(self.inner) }
    }

    /// Total size of the blobs read from blob files.
    pub fn total_blob_bytes_read(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_total_blob_bytes_read(self.inner) }
    }

    /// Total size of the table files in the compaction output.
    pub fn total_output_bytes(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_total_output_bytes(self.inner) }
    }

    /// Total size of the blob files in the compaction output.
    pub fn total_output_bytes_blob(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_total_output_bytes_blob(self.inner) }
    }

    /// Number of input files that were trivially moved rather than rewritten.
    pub fn num_input_files_trivially_moved(&self) -> usize {
        unsafe { ffi::rocksdb_compaction_job_stats_num_input_files_trivially_moved(self.inner) }
    }

    /// Number of records superseded by a newer record for the same key. Counts both updates
    /// and deletions.
    pub fn num_records_replaced(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_num_records_replaced(self.inner) }
    }

    /// Sum of the uncompressed input keys, in bytes.
    pub fn total_input_raw_key_bytes(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_total_input_raw_key_bytes(self.inner) }
    }

    /// Sum of the uncompressed input values, in bytes.
    pub fn total_input_raw_value_bytes(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_total_input_raw_value_bytes(self.inner) }
    }

    /// Number of deletion entries before the compaction. Deletion entries can disappear
    /// during compaction because they expired.
    pub fn num_input_deletion_records(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_num_input_deletion_records(self.inner) }
    }

    /// Number of deletion records dropped as obsolete because every deletion they could still
    /// cause has already happened.
    pub fn num_expired_deletion_records(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_num_expired_deletion_records(self.inner) }
    }

    /// Number of corrupt keys encountered and written out, meaning keys that failed to parse
    /// as internal keys.
    pub fn num_corrupt_keys(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_num_corrupt_keys(self.inner) }
    }

    /// Time spent in file `Append` calls, in nanoseconds.
    ///
    /// Only populated when
    /// [`report_bg_io_stats`](crate::Options::set_report_bg_io_stats) is on.
    pub fn file_write_nanos(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_file_write_nanos(self.inner) }
    }

    /// Time spent syncing file ranges, in nanoseconds.
    ///
    /// Only populated when
    /// [`report_bg_io_stats`](crate::Options::set_report_bg_io_stats) is on.
    pub fn file_range_sync_nanos(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_file_range_sync_nanos(self.inner) }
    }

    /// Time spent in file fsync, in nanoseconds.
    ///
    /// Only populated when
    /// [`report_bg_io_stats`](crate::Options::set_report_bg_io_stats) is on.
    pub fn file_fsync_nanos(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_file_fsync_nanos(self.inner) }
    }

    /// Time spent preparing file writes, such as `fallocate`, in nanoseconds.
    ///
    /// Only populated when
    /// [`report_bg_io_stats`](crate::Options::set_report_bg_io_stats) is on.
    pub fn file_prepare_write_nanos(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_file_prepare_write_nanos(self.inner) }
    }

    /// First 8 bytes of the smallest user key in the output, or fewer if the key is shorter.
    ///
    /// Empty when the compaction wrote no table files.
    pub fn smallest_output_key_prefix(&self) -> &'a [u8] {
        self.key_prefix(ffi::rocksdb_compaction_job_stats_smallest_output_key_prefix)
    }

    /// First 8 bytes of the largest user key in the output, or fewer if the key is shorter.
    ///
    /// Empty when the compaction wrote no table files.
    pub fn largest_output_key_prefix(&self) -> &'a [u8] {
        self.key_prefix(ffi::rocksdb_compaction_job_stats_largest_output_key_prefix)
    }

    /// Number of single deletes that did not meet a put.
    pub fn num_single_del_fallthru(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_num_single_del_fallthru(self.inner) }
    }

    /// Number of single deletes that met something other than a put.
    pub fn num_single_del_mismatch(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_job_stats_num_single_del_mismatch(self.inner) }
    }

    /// Reads one of the borrowed output key prefixes as raw bytes.
    fn key_prefix(&self, getter: KeyPrefixGetter) -> &'a [u8] {
        let mut len: usize = 0;
        // SAFETY: `self.inner` is valid for `'a` and the getter writes the byte length
        // through `len`, returning an interior pointer into a string RocksDB owns.
        unsafe {
            let ptr = getter(self.inner, &raw mut len);
            bytes_from_raw(ptr, len)
        }
    }
}

/// One input or output file of a compaction, borrowed from the job info that listed it.
pub struct CompactionFileInfo<'a> {
    inner: *const ffi::rocksdb_compaction_file_info_t,
    _marker: PhantomData<&'a ()>,
}

impl<'a> CompactionFileInfo<'a> {
    /// Wraps a compaction file info pointer owned by RocksDB.
    ///
    /// # Safety
    ///
    /// `inner` must point to a live `rocksdb_compaction_file_info_t` that stays valid for all
    /// of `'a`. RocksDB owns the object, so the caller must never free it and must not pick
    /// an `'a` that outlives the compaction job info it was read from.
    pub(crate) unsafe fn from_ptr(
        inner: *const ffi::rocksdb_compaction_file_info_t,
    ) -> CompactionFileInfo<'a> {
        CompactionFileInfo {
            inner,
            _marker: PhantomData,
        }
    }

    /// File number of this file.
    pub fn file_number(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_file_info_file_number(self.inner) }
    }

    /// LSM level this file sits at.
    pub fn level(&self) -> i32 {
        unsafe { ffi::rocksdb_compaction_file_info_level(self.inner) }
    }

    /// File number of the oldest blob file this SST file references, or 0 when it references
    /// no blob file.
    pub fn oldest_blob_file_number(&self) -> u64 {
        unsafe { ffi::rocksdb_compaction_file_info_oldest_blob_file_number(self.inner) }
    }
}

/// A blob file created by a flush or compaction, borrowed from the job info that listed it.
pub struct BlobFileAdditionInfo<'a> {
    inner: *const ffi::rocksdb_blob_file_addition_info_t,
    _marker: PhantomData<&'a ()>,
}

impl<'a> BlobFileAdditionInfo<'a> {
    /// Wraps a blob file addition info pointer owned by RocksDB.
    ///
    /// # Safety
    ///
    /// `inner` must point to a live `rocksdb_blob_file_addition_info_t` that stays valid for
    /// all of `'a`. RocksDB owns the object, so the caller must never free it and must not
    /// pick an `'a` that outlives the flush or compaction job info it was read from.
    pub(crate) unsafe fn from_ptr(
        inner: *const ffi::rocksdb_blob_file_addition_info_t,
    ) -> BlobFileAdditionInfo<'a> {
        BlobFileAdditionInfo {
            inner,
            _marker: PhantomData,
        }
    }

    /// Path of the blob file, borrowed as raw bytes.
    pub fn blob_file_path(&self) -> &'a [u8] {
        let mut len: usize = 0;
        // SAFETY: `self.inner` is valid for `'a` and the getter writes the byte length
        // through `len`, returning an interior pointer into a string RocksDB owns.
        unsafe {
            let ptr = ffi::rocksdb_blob_file_addition_info_blob_file_path(self.inner, &raw mut len);
            bytes_from_raw(ptr, len)
        }
    }

    /// File number of the blob file.
    pub fn blob_file_number(&self) -> u64 {
        unsafe { ffi::rocksdb_blob_file_addition_info_blob_file_number(self.inner) }
    }

    /// Number of blobs written to the file.
    pub fn total_blob_count(&self) -> u64 {
        unsafe { ffi::rocksdb_blob_file_addition_info_total_blob_count(self.inner) }
    }

    /// Total size of the blobs written to the file, in bytes.
    pub fn total_blob_bytes(&self) -> u64 {
        unsafe { ffi::rocksdb_blob_file_addition_info_total_blob_bytes(self.inner) }
    }
}

/// Garbage a compaction produced in an existing blob file, borrowed from the job info that
/// listed it.
///
/// A blob becomes garbage when the SST entry that referenced it is dropped or rewritten, so
/// these counts are what blob garbage collection later reclaims.
pub struct BlobFileGarbageInfo<'a> {
    inner: *const ffi::rocksdb_blob_file_garbage_info_t,
    _marker: PhantomData<&'a ()>,
}

impl<'a> BlobFileGarbageInfo<'a> {
    /// Wraps a blob file garbage info pointer owned by RocksDB.
    ///
    /// # Safety
    ///
    /// `inner` must point to a live `rocksdb_blob_file_garbage_info_t` that stays valid for
    /// all of `'a`. RocksDB owns the object, so the caller must never free it and must not
    /// pick an `'a` that outlives the compaction job info it was read from.
    pub(crate) unsafe fn from_ptr(
        inner: *const ffi::rocksdb_blob_file_garbage_info_t,
    ) -> BlobFileGarbageInfo<'a> {
        BlobFileGarbageInfo {
            inner,
            _marker: PhantomData,
        }
    }

    /// Path of the blob file, borrowed as raw bytes.
    pub fn blob_file_path(&self) -> &'a [u8] {
        let mut len: usize = 0;
        // SAFETY: `self.inner` is valid for `'a` and the getter writes the byte length
        // through `len`, returning an interior pointer into a string RocksDB owns.
        unsafe {
            let ptr = ffi::rocksdb_blob_file_garbage_info_blob_file_path(self.inner, &raw mut len);
            bytes_from_raw(ptr, len)
        }
    }

    /// File number of the blob file.
    pub fn blob_file_number(&self) -> u64 {
        unsafe { ffi::rocksdb_blob_file_garbage_info_blob_file_number(self.inner) }
    }

    /// Number of blobs in the file this compaction turned into garbage.
    pub fn garbage_blob_count(&self) -> u64 {
        unsafe { ffi::rocksdb_blob_file_garbage_info_garbage_blob_count(self.inner) }
    }

    /// Total size of the blobs this compaction turned into garbage, in bytes.
    pub fn garbage_blob_bytes(&self) -> u64 {
        unsafe { ffi::rocksdb_blob_file_garbage_info_garbage_blob_bytes(self.inner) }
    }
}

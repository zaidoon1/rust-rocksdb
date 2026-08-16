use crate::compaction::{
    BlobFileAdditionInfo, BlobFileGarbageInfo, CompactionFileInfo, CompactionJobStats,
};
use crate::ffi_util::convert_rocksdb_error;
use crate::table_properties::TableProperties;
use crate::{DBCompressionType, Error, ffi};
use libc::{c_char, c_int, c_void};
use std::fmt;
use std::iter::FusedIterator;
use std::ops::Range;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub enum DBWriteStallCondition {
    KDelayed,
    KStopped,
    KNormal,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
pub enum DBCompactionReason {
    KUnknown,
    // [Level] number of L0 files > level0_file_num_compaction_trigger
    KLevelL0filesNum,
    // [Level] total size of level > MaxBytesForLevel()
    KLevelMaxLevelSize,
    // [Universal] Compacting for size amplification
    KUniversalSizeAmplification,
    // [Universal] Compacting for size ratio
    KUniversalSizeRatio,
    // [Universal] number of sorted runs > level0_file_num_compaction_trigger
    KUniversalSortedRunNum,
    // [FIFO] total size > max_table_files_size
    KFifomaxSize,
    // [FIFO] reduce number of files.
    KFiforeduceNumFiles,
    // [FIFO] files with creation time < (current_time - interval)
    KFifottl,
    // Manual compaction
    KManualCompaction,
    // DB::SuggestCompactRange() marked files for compaction
    KFilesMarkedForCompaction,
    // [Level] Automatic compaction within bottommost level to cleanup duplicate
    // versions of same user key, usually due to a released snapshot.
    KBottommostFiles,
    // Compaction based on TTL
    KTtl,
    // According to the comments in flush_job.cc, RocksDB treats flush as
    // a level 0 compaction in internal stats.
    KFlush,
    // [InternalOnly] External sst file ingestion treated as a compaction
    // with placeholder input level L0 as file ingestion
    // technically does not have an input level like other compactions.
    // Used only for internal stats and conflict checking with other compactions
    KExternalSstIngestion,
    // Compaction due to SST file being too old
    KPeriodicCompaction,
    // Compaction in order to move files to temperature
    KChangeTemperature,
    // Compaction scheduled to force garbage collection of blob files
    KForcedBlobGc,
    // A special TTL compaction for RoundRobin policy, which basically the same as
    // kLevelMaxLevelSize, but the goal is to compact TTLed files.
    KRoundRobinTtl,
    // [InternalOnly] DBImpl::ReFitLevel treated as a compaction,
    // Used only for internal conflict checking with other compactions
    KRefitLevel,
    // Compaction triggered by high read frequency on SST files
    KReadTriggered,
    // total number of compaction reasons, new reasons must be added above this.
    KNumOfReasons,
}

impl From<u32> for DBCompactionReason {
    fn from(value: u32) -> Self {
        match value {
            1 => DBCompactionReason::KLevelL0filesNum,
            2 => DBCompactionReason::KLevelMaxLevelSize,
            3 => DBCompactionReason::KUniversalSizeAmplification,
            4 => DBCompactionReason::KUniversalSizeRatio,
            5 => DBCompactionReason::KUniversalSortedRunNum,
            6 => DBCompactionReason::KFifomaxSize,
            7 => DBCompactionReason::KFiforeduceNumFiles,
            8 => DBCompactionReason::KFifottl,
            9 => DBCompactionReason::KManualCompaction,
            10 => DBCompactionReason::KFilesMarkedForCompaction,
            11 => DBCompactionReason::KBottommostFiles,
            12 => DBCompactionReason::KTtl,
            13 => DBCompactionReason::KFlush,
            14 => DBCompactionReason::KExternalSstIngestion,
            15 => DBCompactionReason::KPeriodicCompaction,
            16 => DBCompactionReason::KChangeTemperature,
            17 => DBCompactionReason::KForcedBlobGc,
            18 => DBCompactionReason::KRoundRobinTtl,
            19 => DBCompactionReason::KRefitLevel,
            20 => DBCompactionReason::KReadTriggered,
            21 => DBCompactionReason::KNumOfReasons,
            _ => DBCompactionReason::KUnknown,
        }
    }
}

impl DBCompactionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            DBCompactionReason::KUnknown => "KUnknown",
            DBCompactionReason::KLevelL0filesNum => "KLevelL0filesNum",
            DBCompactionReason::KLevelMaxLevelSize => "KLevelMaxLevelSize",
            DBCompactionReason::KUniversalSizeAmplification => "KUniversalSizeAmplification",
            DBCompactionReason::KUniversalSizeRatio => "KUniversalSizeRatio",
            DBCompactionReason::KUniversalSortedRunNum => "KUniversalSortedRunNum",
            DBCompactionReason::KFifomaxSize => "KFifomaxSize",
            DBCompactionReason::KFiforeduceNumFiles => "KFiforeduceNumFiles",
            DBCompactionReason::KFifottl => "KFifottl",
            DBCompactionReason::KManualCompaction => "KManualCompaction",
            DBCompactionReason::KFilesMarkedForCompaction => "KFilesMarkedForCompaction",
            DBCompactionReason::KBottommostFiles => "KBottommostFiles",
            DBCompactionReason::KTtl => "KTtl",
            DBCompactionReason::KFlush => "KFlush",
            DBCompactionReason::KExternalSstIngestion => "KExternalSstIngestion",
            DBCompactionReason::KPeriodicCompaction => "KPeriodicCompaction",
            DBCompactionReason::KChangeTemperature => "KChangeTemperature",
            DBCompactionReason::KForcedBlobGc => "KForcedBlobGc",
            DBCompactionReason::KRoundRobinTtl => "KRoundRobinTtl",
            DBCompactionReason::KRefitLevel => "KRefitLevel",
            DBCompactionReason::KReadTriggered => "KReadTriggered",
            DBCompactionReason::KNumOfReasons => "KNumOfReasons",
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
pub enum DBFlushReason {
    KOthers,
    KGetLiveFiles,
    KShutDown,
    KExternalFileIngestion,
    KManualCompaction,
    KWriteBufferManager,
    KWriteBufferFull,
    KTest,
    KDeleteFiles,
    KAutoCompaction,
    KManualFlush,
    KErrorRecovery,
    KErrorRecoveryRetryFlush,
    KWalFull,
    KCatchUpAfterErrorRecovery,
    KMemtableMaxRangeDeletions,
    KUnknown, // not an actual flush reason but will be used when we don't recognize the enum value
}

impl From<u32> for DBFlushReason {
    fn from(value: u32) -> Self {
        match value {
            0 => DBFlushReason::KOthers,
            1 => DBFlushReason::KGetLiveFiles,
            2 => DBFlushReason::KShutDown,
            3 => DBFlushReason::KExternalFileIngestion,
            4 => DBFlushReason::KManualCompaction,
            5 => DBFlushReason::KWriteBufferManager,
            6 => DBFlushReason::KWriteBufferFull,
            7 => DBFlushReason::KTest,
            8 => DBFlushReason::KDeleteFiles,
            9 => DBFlushReason::KAutoCompaction,
            10 => DBFlushReason::KManualFlush,
            11 => DBFlushReason::KErrorRecovery,
            12 => DBFlushReason::KErrorRecoveryRetryFlush,
            13 => DBFlushReason::KWalFull,
            14 => DBFlushReason::KCatchUpAfterErrorRecovery,
            15 => DBFlushReason::KMemtableMaxRangeDeletions,
            _ => DBFlushReason::KUnknown,
        }
    }
}

impl DBFlushReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            DBFlushReason::KOthers => "KOthers",
            DBFlushReason::KGetLiveFiles => "KGetLiveFiles",
            DBFlushReason::KShutDown => "KShutDown",
            DBFlushReason::KExternalFileIngestion => "KExternalFileIngestion",
            DBFlushReason::KManualCompaction => "KManualCompaction",
            DBFlushReason::KWriteBufferManager => "KWriteBufferManager",
            DBFlushReason::KWriteBufferFull => "KWriteBufferFull",
            DBFlushReason::KTest => "KTest",
            DBFlushReason::KDeleteFiles => "KDeleteFiles",
            DBFlushReason::KAutoCompaction => "KAutoCompaction",
            DBFlushReason::KManualFlush => "KManualFlush",
            DBFlushReason::KErrorRecovery => "KErrorRecovery",
            DBFlushReason::KErrorRecoveryRetryFlush => "KErrorRecoveryRetryFlush",
            DBFlushReason::KWalFull => "KWalFull",
            DBFlushReason::KCatchUpAfterErrorRecovery => "KCatchUpAfterErrorRecovery",
            DBFlushReason::KMemtableMaxRangeDeletions => "KMemtableMaxRangeDeletions",
            DBFlushReason::KUnknown => "KUnknown",
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum DBBackgroundErrorReason {
    KFlush = 0,
    KCompaction = 1,
    KWriteCallback = 2,
    KMemTable = 3,
    KManifestWrite = 4,
    KFlushNoWAL = 5,
    KManifestWriteNoWAL = 6,
    KAsyncFileOpen = 7,
    KUnknown, // not an actual background error reason but will be used when we don't recognize the enum value
}

impl From<u32> for DBBackgroundErrorReason {
    fn from(value: u32) -> Self {
        match value {
            0 => DBBackgroundErrorReason::KFlush,
            1 => DBBackgroundErrorReason::KCompaction,
            2 => DBBackgroundErrorReason::KWriteCallback,
            3 => DBBackgroundErrorReason::KMemTable,
            4 => DBBackgroundErrorReason::KManifestWrite,
            5 => DBBackgroundErrorReason::KFlushNoWAL,
            6 => DBBackgroundErrorReason::KManifestWriteNoWAL,
            7 => DBBackgroundErrorReason::KAsyncFileOpen,
            _ => DBBackgroundErrorReason::KUnknown,
        }
    }
}

/// Severity carried by RocksDB's background error `Status`.
#[derive(Copy, Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum StatusSeverity {
    KNoError = 0,
    KSoftError = 1,
    KHardError = 2,
    KFatalError = 3,
    KUnrecoverableError = 4,
    KMaxSeverity = 5,
    KUnknown,
}

impl From<u8> for StatusSeverity {
    fn from(value: u8) -> Self {
        match value {
            0 => StatusSeverity::KNoError,
            1 => StatusSeverity::KSoftError,
            2 => StatusSeverity::KHardError,
            3 => StatusSeverity::KFatalError,
            4 => StatusSeverity::KUnrecoverableError,
            5 => StatusSeverity::KMaxSeverity,
            _ => StatusSeverity::KUnknown,
        }
    }
}

pub struct FlushJobInfo {
    pub(crate) inner: *const ffi::rocksdb_flushjobinfo_t,
}

impl FlushJobInfo {
    pub fn cf_name(&self) -> Option<Vec<u8>> {
        unsafe {
            let mut length: usize = 0;
            let cf_name_ptr = ffi::rocksdb_flushjobinfo_cf_name(self.inner, &raw mut length);

            if cf_name_ptr.is_null() || length == 0 {
                return None;
            }

            // SAFETY: We're copying `length` bytes from a valid, non-null pointer.
            let cf_name_vec = std::slice::from_raw_parts(cf_name_ptr.cast::<u8>(), length).to_vec();

            Some(cf_name_vec)
        }
    }

    pub fn triggered_writes_slowdown(&self) -> bool {
        let val = unsafe { ffi::rocksdb_flushjobinfo_triggered_writes_slowdown(self.inner) };
        val != 0
    }

    pub fn triggered_writes_stop(&self) -> bool {
        let val = unsafe { ffi::rocksdb_flushjobinfo_triggered_writes_stop(self.inner) };
        val != 0
    }

    pub fn largest_seqno(&self) -> u64 {
        unsafe { ffi::rocksdb_flushjobinfo_largest_seqno(self.inner) }
    }

    pub fn smallest_seqno(&self) -> u64 {
        unsafe { ffi::rocksdb_flushjobinfo_smallest_seqno(self.inner) }
    }

    pub fn flush_reason(&self) -> DBFlushReason {
        unsafe { DBFlushReason::from(ffi::rocksdb_flushjobinfo_flush_reason(self.inner)) }
    }

    /// Number of blob files created by this flush.
    pub fn blob_file_addition_infos_count(&self) -> usize {
        unsafe { ffi::rocksdb_flushjobinfo_blob_file_addition_infos_count(self.inner) }
    }

    /// The id of the column family where the compaction happened.
    pub fn cf_id(&self) -> u32 {
        unsafe { ffi::rocksdb_flushjobinfo_cf_id(self.inner) }
    }

    /// The file number of the newly created file.
    pub fn file_number(&self) -> u64 {
        unsafe { ffi::rocksdb_flushjobinfo_file_number(self.inner) }
    }

    /// The id of the job (which could be flush or compaction) that created the file.
    pub fn job_id(&self) -> i32 {
        unsafe { ffi::rocksdb_flushjobinfo_job_id(self.inner) }
    }

    /// The oldest blob file referenced by the newly created file.
    pub fn oldest_blob_file_number(&self) -> u64 {
        unsafe { ffi::rocksdb_flushjobinfo_oldest_blob_file_number(self.inner) }
    }

    /// The id of the thread that completed this flush job.
    pub fn thread_id(&self) -> u64 {
        unsafe { ffi::rocksdb_flushjobinfo_thread_id(self.inner) }
    }

    /// Full path of the newly created file, or `None` when RocksDB left it empty.
    ///
    /// `db/c.cc` hands back the interior pointer of a `std::string` this job info owns, so
    /// there is nothing to free. This copies it, the same as [`Self::cf_name`].
    pub fn file_path(&self) -> Option<Vec<u8>> {
        unsafe {
            let mut length: usize = 0;
            let file_path_ptr = ffi::rocksdb_flushjobinfo_file_path(self.inner, &raw mut length);

            if file_path_ptr.is_null() || length == 0 {
                return None;
            }

            // SAFETY: We're copying `length` bytes from a valid, non-null pointer.
            let file_path_vec =
                std::slice::from_raw_parts(file_path_ptr.cast::<u8>(), length).to_vec();

            Some(file_path_vec)
        }
    }

    /// Table properties of the newly created file.
    pub fn table_properties(&self) -> TableProperties<'_> {
        // SAFETY: the C API returns the address of a `TableProperties` member of this job
        // info, which RocksDB owns and which lives for at least the borrow of `self`.
        unsafe { TableProperties::from_ptr(ffi::rocksdb_flushjobinfo_table_properties(self.inner)) }
    }

    /// The blob file this flush created at `pos`, or `None` once `pos` reaches
    /// [`Self::blob_file_addition_infos_count`].
    ///
    /// The C side only asserts the bound, and the vendored RocksDB is always built with
    /// `NDEBUG`, so the check here is the only thing standing between an out of range `pos`
    /// and a read past the end of the vector.
    pub fn blob_file_addition_info_at(&self, pos: usize) -> Option<BlobFileAdditionInfo<'_>> {
        if pos >= self.blob_file_addition_infos_count() {
            return None;
        }
        // SAFETY: `pos` is in range, so the C API returns the address of an element of a
        // vector inside this job info, which lives for at least the borrow of `self`.
        unsafe {
            Some(BlobFileAdditionInfo::from_ptr(
                ffi::rocksdb_flushjobinfo_blob_file_addition_info_at(self.inner, pos),
            ))
        }
    }

    /// Walks the blob files this flush created.
    pub fn blob_file_addition_infos(
        &self,
    ) -> impl ExactSizeIterator<Item = BlobFileAdditionInfo<'_>> + DoubleEndedIterator + FusedIterator
    {
        JobInfoIter::new(
            self,
            self.blob_file_addition_infos_count(),
            Self::blob_file_addition_info_at,
        )
    }

    /// Compression used for the blob files this flush wrote.
    ///
    /// `None` for a compression type this crate does not name: xpress, which is Windows
    /// only, and the custom compression range a `CompressionManager` can hand out.
    pub fn blob_compression_type(&self) -> Option<DBCompressionType> {
        let raw = unsafe { ffi::rocksdb_flushjobinfo_blob_compression_type(self.inner) };
        DBCompressionType::try_from_raw(raw as c_int)
    }
}

pub struct CompactionJobInfo {
    pub(crate) inner: *const ffi::rocksdb_compactionjobinfo_t,
    /// Whether `num_l0_files` holds a real value.
    ///
    /// RocksDB fills this struct two ways. A listener callback gets one built by
    /// `NotifyOnCompactionBegin` and `NotifyOnCompactionCompleted`, which set every field.
    /// `CompactFiles` instead writes into a caller-allocated struct through
    /// `BuildCompactionJobInfo`, which never assigns `num_l0_files`, and the field has no
    /// in-class initialiser, so it stays indeterminate. Reading it there would be reading
    /// uninitialised memory, hence the flag.
    pub(crate) num_l0_files_set: bool,
}

impl CompactionJobInfo {
    pub fn status(&self) -> Result<(), Error> {
        unsafe { ffi_try!(ffi::rocksdb_compactionjobinfo_status(self.inner)) }
        Ok(())
    }

    pub fn cf_name(&self) -> Option<Vec<u8>> {
        unsafe {
            let mut length: usize = 0;
            let cf_name_ptr = ffi::rocksdb_compactionjobinfo_cf_name(self.inner, &raw mut length);

            if cf_name_ptr.is_null() || length == 0 {
                return None;
            }

            // SAFETY: We're copying `length` bytes from a valid, non-null pointer.
            let cf_name_vec = std::slice::from_raw_parts(cf_name_ptr.cast::<u8>(), length).to_vec();

            Some(cf_name_vec)
        }
    }

    pub fn input_file_count(&self) -> usize {
        unsafe { ffi::rocksdb_compactionjobinfo_input_files_count(self.inner) }
    }

    pub fn output_file_count(&self) -> usize {
        unsafe { ffi::rocksdb_compactionjobinfo_output_files_count(self.inner) }
    }

    pub fn elapsed_micros(&self) -> u64 {
        unsafe { ffi::rocksdb_compactionjobinfo_elapsed_micros(self.inner) }
    }

    pub fn num_corrupt_keys(&self) -> u64 {
        unsafe { ffi::rocksdb_compactionjobinfo_num_corrupt_keys(self.inner) }
    }

    pub fn base_input_level(&self) -> i32 {
        unsafe { ffi::rocksdb_compactionjobinfo_base_input_level(self.inner) }
    }

    pub fn output_level(&self) -> i32 {
        unsafe { ffi::rocksdb_compactionjobinfo_output_level(self.inner) }
    }

    pub fn input_records(&self) -> u64 {
        unsafe { ffi::rocksdb_compactionjobinfo_input_records(self.inner) }
    }

    pub fn output_records(&self) -> u64 {
        unsafe { ffi::rocksdb_compactionjobinfo_output_records(self.inner) }
    }

    pub fn total_input_bytes(&self) -> u64 {
        unsafe { ffi::rocksdb_compactionjobinfo_total_input_bytes(self.inner) }
    }

    pub fn total_output_bytes(&self) -> u64 {
        unsafe { ffi::rocksdb_compactionjobinfo_total_output_bytes(self.inner) }
    }

    pub fn num_input_files_at_output_level(&self) -> usize {
        unsafe { ffi::rocksdb_compactionjobinfo_num_input_files_at_output_level(self.inner) }
    }

    pub fn compaction_reason(&self) -> DBCompactionReason {
        unsafe {
            DBCompactionReason::from(ffi::rocksdb_compactionjobinfo_compaction_reason(self.inner))
        }
    }

    /// Whether this compaction was aborted via AbortAllCompactions().
    pub fn aborted(&self) -> bool {
        unsafe { ffi::rocksdb_compactionjobinfo_aborted(self.inner) != 0 }
    }

    /// Number of blob files created by this compaction.
    pub fn blob_file_addition_infos_count(&self) -> usize {
        unsafe { ffi::rocksdb_compactionjobinfo_blob_file_addition_infos_count(self.inner) }
    }

    /// Number of blob files this compaction produced garbage for.
    pub fn blob_file_garbage_infos_count(&self) -> usize {
        unsafe { ffi::rocksdb_compactionjobinfo_blob_file_garbage_infos_count(self.inner) }
    }

    /// The id of the column family where the compaction happened.
    pub fn cf_id(&self) -> u32 {
        unsafe { ffi::rocksdb_compactionjobinfo_cf_id(self.inner) }
    }

    /// Number of entries in the per-input-file detail list.
    ///
    /// This counts the same files as [`Self::input_file_count`], reported from the
    /// richer per-file records rather than the plain path list.
    pub fn input_file_infos_count(&self) -> usize {
        unsafe { ffi::rocksdb_compactionjobinfo_input_file_infos_count(self.inner) }
    }

    /// The id of the job (which could be flush or compaction) that created the file.
    pub fn job_id(&self) -> i32 {
        unsafe { ffi::rocksdb_compactionjobinfo_job_id(self.inner) }
    }

    /// The number of compaction input files (table files).
    pub fn num_input_files(&self) -> usize {
        unsafe { ffi::rocksdb_compactionjobinfo_num_input_files(self.inner) }
    }

    /// The number of L0 files in the column family right before the compaction.
    ///
    /// `None` for the job info returned by [`compact_files`](crate::DBCommon::compact_files),
    /// which RocksDB never assigns this field on. Listener callbacks always report a value.
    pub fn num_l0_files(&self) -> Option<i32> {
        self.num_l0_files_set
            .then(|| unsafe { ffi::rocksdb_compactionjobinfo_num_l0_files(self.inner) })
    }

    /// Number of entries in the per-output-file detail list.
    ///
    /// This counts the same files as [`Self::output_file_count`], reported from the
    /// richer per-file records rather than the plain path list.
    pub fn output_file_infos_count(&self) -> usize {
        unsafe { ffi::rocksdb_compactionjobinfo_output_file_infos_count(self.inner) }
    }

    /// Number of files whose table properties this compaction collected.
    pub fn table_properties_count(&self) -> usize {
        unsafe { ffi::rocksdb_compactionjobinfo_table_properties_count(self.inner) }
    }

    /// The id of the thread that completed this flush job.
    pub fn thread_id(&self) -> u64 {
        unsafe { ffi::rocksdb_compactionjobinfo_thread_id(self.inner) }
    }

    /// Counters and timings for this compaction.
    pub fn stats(&self) -> CompactionJobStats<'_> {
        // SAFETY: the C API returns the address of a `CompactionJobStats` member of this job
        // info, which RocksDB owns and which lives for at least the borrow of `self`.
        unsafe { CompactionJobStats::from_ptr(ffi::rocksdb_compactionjobinfo_stats(self.inner)) }
    }

    /// Path of the compaction input file at `pos`, or `None` once `pos` reaches
    /// [`Self::input_file_count`].
    ///
    /// Borrowed from the `std::string` inside this job info, so nothing is copied. The C
    /// side only asserts the bound, and the vendored RocksDB is always built with `NDEBUG`,
    /// so the check here is the only thing standing between an out of range `pos` and a read
    /// past the end of the vector.
    pub fn input_file_at(&self, pos: usize) -> Option<&[u8]> {
        if pos >= self.input_file_count() {
            return None;
        }
        let mut length: usize = 0;
        // SAFETY: `pos` is in range, so the C API writes the byte length through `length` and
        // returns an interior pointer into a string that lives for at least the borrow of
        // `self`.
        unsafe {
            let path =
                ffi::rocksdb_compactionjobinfo_input_file_at(self.inner, pos, &raw mut length);
            Some(bytes_from_raw(path, length))
        }
    }

    /// Walks the paths of the compaction input files.
    pub fn input_files(
        &self,
    ) -> impl ExactSizeIterator<Item = &[u8]> + DoubleEndedIterator + FusedIterator {
        JobInfoIter::new(self, self.input_file_count(), Self::input_file_at)
    }

    /// Path of the compaction output file at `pos`, or `None` once `pos` reaches
    /// [`Self::output_file_count`].
    ///
    /// Borrowed from the `std::string` inside this job info, so nothing is copied. Bounds
    /// checked here rather than by the C side, for the reason given on
    /// [`Self::input_file_at`].
    pub fn output_file_at(&self, pos: usize) -> Option<&[u8]> {
        if pos >= self.output_file_count() {
            return None;
        }
        let mut length: usize = 0;
        // SAFETY: `pos` is in range, so the C API writes the byte length through `length` and
        // returns an interior pointer into a string that lives for at least the borrow of
        // `self`.
        unsafe {
            let path =
                ffi::rocksdb_compactionjobinfo_output_file_at(self.inner, pos, &raw mut length);
            Some(bytes_from_raw(path, length))
        }
    }

    /// Walks the paths of the compaction output files.
    pub fn output_files(
        &self,
    ) -> impl ExactSizeIterator<Item = &[u8]> + DoubleEndedIterator + FusedIterator {
        JobInfoIter::new(self, self.output_file_count(), Self::output_file_at)
    }

    /// The per-file detail record for the compaction input file at `pos`, or `None` once
    /// `pos` reaches [`Self::input_file_infos_count`].
    ///
    /// The order matches [`Self::input_files`]. Bounds checked here rather than by the C
    /// side, for the reason given on [`Self::input_file_at`].
    pub fn input_file_info_at(&self, pos: usize) -> Option<CompactionFileInfo<'_>> {
        if pos >= self.input_file_infos_count() {
            return None;
        }
        // SAFETY: `pos` is in range, so the C API returns the address of an element of a
        // vector inside this job info, which lives for at least the borrow of `self`.
        unsafe {
            Some(CompactionFileInfo::from_ptr(
                ffi::rocksdb_compactionjobinfo_input_file_info_at(self.inner, pos),
            ))
        }
    }

    /// Walks the per-file detail records for the compaction input files.
    pub fn input_file_infos(
        &self,
    ) -> impl ExactSizeIterator<Item = CompactionFileInfo<'_>> + DoubleEndedIterator + FusedIterator
    {
        JobInfoIter::new(
            self,
            self.input_file_infos_count(),
            Self::input_file_info_at,
        )
    }

    /// The per-file detail record for the compaction output file at `pos`, or `None` once
    /// `pos` reaches [`Self::output_file_infos_count`].
    ///
    /// The order matches [`Self::output_files`]. Bounds checked here rather than by the C
    /// side, for the reason given on [`Self::input_file_at`].
    pub fn output_file_info_at(&self, pos: usize) -> Option<CompactionFileInfo<'_>> {
        if pos >= self.output_file_infos_count() {
            return None;
        }
        // SAFETY: `pos` is in range, so the C API returns the address of an element of a
        // vector inside this job info, which lives for at least the borrow of `self`.
        unsafe {
            Some(CompactionFileInfo::from_ptr(
                ffi::rocksdb_compactionjobinfo_output_file_info_at(self.inner, pos),
            ))
        }
    }

    /// Walks the per-file detail records for the compaction output files.
    pub fn output_file_infos(
        &self,
    ) -> impl ExactSizeIterator<Item = CompactionFileInfo<'_>> + DoubleEndedIterator + FusedIterator
    {
        JobInfoIter::new(
            self,
            self.output_file_infos_count(),
            Self::output_file_info_at,
        )
    }

    /// The blob file this compaction created at `pos`, or `None` once `pos` reaches
    /// [`Self::blob_file_addition_infos_count`].
    ///
    /// Bounds checked here rather than by the C side, for the reason given on
    /// [`Self::input_file_at`].
    pub fn blob_file_addition_info_at(&self, pos: usize) -> Option<BlobFileAdditionInfo<'_>> {
        if pos >= self.blob_file_addition_infos_count() {
            return None;
        }
        // SAFETY: `pos` is in range, so the C API returns the address of an element of a
        // vector inside this job info, which lives for at least the borrow of `self`.
        unsafe {
            Some(BlobFileAdditionInfo::from_ptr(
                ffi::rocksdb_compactionjobinfo_blob_file_addition_info_at(self.inner, pos),
            ))
        }
    }

    /// Walks the blob files this compaction created.
    pub fn blob_file_addition_infos(
        &self,
    ) -> impl ExactSizeIterator<Item = BlobFileAdditionInfo<'_>> + DoubleEndedIterator + FusedIterator
    {
        JobInfoIter::new(
            self,
            self.blob_file_addition_infos_count(),
            Self::blob_file_addition_info_at,
        )
    }

    /// The blob file this compaction produced garbage for at `pos`, or `None` once `pos`
    /// reaches [`Self::blob_file_garbage_infos_count`].
    ///
    /// Bounds checked here rather than by the C side, for the reason given on
    /// [`Self::input_file_at`].
    pub fn blob_file_garbage_info_at(&self, pos: usize) -> Option<BlobFileGarbageInfo<'_>> {
        if pos >= self.blob_file_garbage_infos_count() {
            return None;
        }
        // SAFETY: `pos` is in range, so the C API returns the address of an element of a
        // vector inside this job info, which lives for at least the borrow of `self`.
        unsafe {
            Some(BlobFileGarbageInfo::from_ptr(
                ffi::rocksdb_compactionjobinfo_blob_file_garbage_info_at(self.inner, pos),
            ))
        }
    }

    /// Walks the blob files this compaction produced garbage for.
    pub fn blob_file_garbage_infos(
        &self,
    ) -> impl ExactSizeIterator<Item = BlobFileGarbageInfo<'_>> + DoubleEndedIterator + FusedIterator
    {
        JobInfoIter::new(
            self,
            self.blob_file_garbage_infos_count(),
            Self::blob_file_garbage_info_at,
        )
    }

    /// Table properties collected for `file_name`, or `None` when this compaction has none
    /// for that file.
    ///
    /// The map is keyed by the paths in [`Self::input_files`] and [`Self::output_files`], so
    /// pass one of those rather than a bare file name.
    pub fn table_properties_for_file(&self, file_name: &[u8]) -> Option<TableProperties<'_>> {
        // SAFETY: the C API copies `file_name_len` bytes into a `std::string` to look up and
        // reads no further, so an empty slice's dangling pointer is never dereferenced. A
        // file that is not in the map comes back as null.
        unsafe {
            let props = ffi::rocksdb_compactionjobinfo_table_properties_for_file(
                self.inner,
                file_name.as_ptr().cast::<c_char>(),
                file_name.len(),
            );
            if props.is_null() {
                return None;
            }
            Some(TableProperties::from_ptr(props))
        }
    }

    /// The file name and table properties at `pos`, or `None` once `pos` runs past
    /// [`Self::table_properties_count`].
    ///
    /// `pos` indexes a `std::unordered_map`, so the order is whatever the hash table
    /// happens to give and two DBs holding the same files can report the same entries at
    /// different positions. Use [`Self::table_properties_for_file`] to look a file up by
    /// name, and treat `pos` as nothing more than a way to enumerate the whole set.
    ///
    /// Unlike the other `_at` accessors here, the C side does its own bounds check and
    /// reports out of range with a null pointer.
    ///
    /// Costs O(pos): the C API walks the underlying map from the beginning on every call, so
    /// random access here is not cheap.
    pub fn table_property_at(&self, pos: usize) -> Option<(&[u8], TableProperties<'_>)> {
        let mut key_len: usize = 0;
        // SAFETY: both getters bounds check `pos` themselves, the key getter writes the byte
        // length through `key_len`, and what they return points into the map inside this job
        // info, which lives for at least the borrow of `self`.
        unsafe {
            let key = ffi::rocksdb_compactionjobinfo_table_properties_key_at(
                self.inner,
                pos,
                &raw mut key_len,
            );
            if key.is_null() {
                return None;
            }
            let props = ffi::rocksdb_compactionjobinfo_table_properties_value_at(self.inner, pos);
            if props.is_null() {
                return None;
            }
            Some((
                bytes_from_raw(key, key_len),
                TableProperties::from_ptr(props),
            ))
        }
    }

    /// Walks the collected table properties, borrowing every file name.
    ///
    /// Yields in unspecified order, see [`Self::table_property_at`].
    ///
    /// Lazy and allocation free, but each step costs O(pos) because the C API walks the map
    /// from the beginning for every lookup, so a full pass over n files is O(n^2).
    pub fn table_properties(
        &self,
    ) -> impl ExactSizeIterator<Item = (&[u8], TableProperties<'_>)> + DoubleEndedIterator + FusedIterator
    {
        JobInfoIter::new(self, self.table_properties_count(), Self::table_property_at)
    }

    /// Compression used for the table files this compaction wrote.
    ///
    /// `None` for a compression type this crate does not name: xpress, which is Windows
    /// only, and the custom compression range a `CompressionManager` can hand out.
    pub fn compression(&self) -> Option<DBCompressionType> {
        let raw = unsafe { ffi::rocksdb_compactionjobinfo_compression(self.inner) };
        DBCompressionType::try_from_raw(raw as c_int)
    }

    /// Compression used for the blob files this compaction wrote.
    ///
    /// `None` covers the same unnamed types as [`Self::compression`].
    pub fn blob_compression_type(&self) -> Option<DBCompressionType> {
        let raw = unsafe { ffi::rocksdb_compactionjobinfo_blob_compression_type(self.inner) };
        DBCompressionType::try_from_raw(raw as c_int)
    }
}

/// A [`CompactionJobInfo`] this crate allocated, rather than one borrowed from a listener
/// callback.
///
/// `CompactFiles` reports what it did through an optional out parameter, and this is the
/// buffer for it: `DBCommon::compact_files` allocates one, hands its pointer to RocksDB, and
/// gives it back filled in. Read it with [`info`](Self::info).
///
/// `rocksdb_compactionjobinfo_create` default constructs the C++ struct, and RocksDB's
/// `CompactionJobInfo` only has an in-class initialiser for `aborted`. The strings, vectors,
/// map, `Status` and nested stats therefore start out empty, but the scalars start out
/// indeterminate. `BuildCompactionJobInfo` assigns all of them except `num_l0_files`, which
/// is why [`CompactionJobInfo::num_l0_files`] reports `None` here.
///
/// Nothing is written at all when `CompactFiles` returns early, either because it rejected
/// the request or because it satisfied it with a trivial move. `DBCommon::compact_files`
/// handles that by only allocating this when a trivial move cannot happen.
pub struct OwnedCompactionJobInfo {
    /// Owns the allocation from `rocksdb_compactionjobinfo_create`, held as the borrowed
    /// view so [`Self::info`] can return a reference to a real value. `CompactionJobInfo`
    /// has no lifetime parameter, so handing one out by value instead would let it outlive
    /// this handle and dangle.
    view: CompactionJobInfo,
}

// SAFETY: the pointee is a plain `CompactionJobInfo` value this handle owns outright. Nobody
// else points at it, RocksDB is done writing to it by the time `CompactFiles` returns, and
// this type only reads it afterwards, so moving the handle to another thread is sound. Sync
// is deliberately left off: reading the status goes through `Status::ToString`, which writes
// a `mutable` flag in a RocksDB built with `ROCKSDB_ASSERT_STATUS_CHECKED`, so concurrent
// reads through a shared reference are not provably race free.
unsafe impl Send for OwnedCompactionJobInfo {}

impl OwnedCompactionJobInfo {
    /// Allocates an empty job info for `CompactFiles` to fill in.
    ///
    /// The scalar fields start out indeterminate, see the type docs, so the caller must only
    /// hand the value out once `rocksdb_compact_files` has reported success.
    pub(crate) fn new() -> Self {
        let inner = unsafe { ffi::rocksdb_compactionjobinfo_create() };
        assert!(
            !inner.is_null(),
            "Could not create RocksDB compaction job info"
        );
        Self {
            view: CompactionJobInfo {
                inner: inner.cast_const(),
                num_l0_files_set: false,
            },
        }
    }

    /// The raw handle, for passing to `rocksdb_compact_files` as its out parameter.
    ///
    /// Takes `&mut self` because RocksDB writes through it, which also keeps any borrow
    /// handed out by [`Self::info`] from being live across the call.
    pub(crate) fn as_mut_ptr(&mut self) -> *mut ffi::rocksdb_compactionjobinfo_t {
        self.view.inner.cast_mut()
    }

    /// Reads the job info `CompactFiles` filled in.
    pub fn info(&self) -> &CompactionJobInfo {
        &self.view
    }
}

impl Drop for OwnedCompactionJobInfo {
    fn drop(&mut self) {
        unsafe { ffi::rocksdb_compactionjobinfo_destroy(self.view.inner.cast_mut()) }
    }
}

impl fmt::Debug for OwnedCompactionJobInfo {
    /// Reports the identifying fields and the byte and record totals.
    ///
    /// The per-file lists and table properties are left out to keep this short. Read them
    /// through [`info`](Self::info).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let info = self.info();
        f.debug_struct("OwnedCompactionJobInfo")
            .field("job_id", &info.job_id())
            .field(
                "cf_name",
                &info
                    .cf_name()
                    .map(|name| String::from_utf8_lossy(&name).into_owned()),
            )
            .field("base_input_level", &info.base_input_level())
            .field("output_level", &info.output_level())
            .field("input_file_count", &info.input_file_count())
            .field("output_file_count", &info.output_file_count())
            .field("total_input_bytes", &info.total_input_bytes())
            .field("total_output_bytes", &info.total_output_bytes())
            .field("input_records", &info.input_records())
            .field("output_records", &info.output_records())
            .finish_non_exhaustive()
    }
}

pub struct SubcompactionJobInfo {
    pub(crate) inner: *const ffi::rocksdb_subcompactionjobinfo_t,
}

impl SubcompactionJobInfo {
    pub fn status(&self) -> Result<(), Error> {
        unsafe { ffi_try!(ffi::rocksdb_subcompactionjobinfo_status(self.inner)) }
        Ok(())
    }

    pub fn cf_name(&self) -> Option<Vec<u8>> {
        unsafe {
            let mut length: usize = 0;
            let cf_name_ptr =
                ffi::rocksdb_subcompactionjobinfo_cf_name(self.inner, &raw mut length);

            if cf_name_ptr.is_null() || length == 0 {
                return None;
            }

            // SAFETY: We're copying `length` bytes from a valid, non-null pointer.
            let cf_name_vec = std::slice::from_raw_parts(cf_name_ptr.cast::<u8>(), length).to_vec();

            Some(cf_name_vec)
        }
    }

    pub fn thread_id(&self) -> u64 {
        unsafe { ffi::rocksdb_subcompactionjobinfo_thread_id(self.inner) }
    }

    pub fn base_input_level(&self) -> i32 {
        unsafe { ffi::rocksdb_subcompactionjobinfo_base_input_level(self.inner) }
    }

    pub fn output_level(&self) -> i32 {
        unsafe { ffi::rocksdb_subcompactionjobinfo_output_level(self.inner) }
    }

    pub fn compaction_reason(&self) -> DBCompactionReason {
        unsafe {
            DBCompactionReason::from(ffi::rocksdb_subcompactionjobinfo_compaction_reason(
                self.inner,
            ))
        }
    }

    /// The id of the column family where the compaction happened.
    pub fn cf_id(&self) -> u32 {
        unsafe { ffi::rocksdb_subcompactionjobinfo_cf_id(self.inner) }
    }

    /// The id of the job (which could be flush or compaction) that created the file.
    pub fn job_id(&self) -> i32 {
        unsafe { ffi::rocksdb_subcompactionjobinfo_job_id(self.inner) }
    }

    /// Sub-compaction job id, which is only unique within the same compaction, so use both
    /// 'job_id' and 'subcompaction_job_id' to identify a subcompaction within an instance.
    /// For non subcompaction job, it's set to -1.
    pub fn subcompaction_job_id(&self) -> i32 {
        unsafe { ffi::rocksdb_subcompactionjobinfo_subcompaction_job_id(self.inner) }
    }

    /// Counters and timings for this subcompaction.
    pub fn stats(&self) -> CompactionJobStats<'_> {
        // SAFETY: the C API returns the address of a `CompactionJobStats` member of this job
        // info, which RocksDB owns and which lives for at least the borrow of `self`.
        unsafe { CompactionJobStats::from_ptr(ffi::rocksdb_subcompactionjobinfo_stats(self.inner)) }
    }

    /// Compression used for the table files this subcompaction wrote.
    ///
    /// `None` for a compression type this crate does not name: xpress, which is Windows
    /// only, and the custom compression range a `CompressionManager` can hand out.
    pub fn compression(&self) -> Option<DBCompressionType> {
        let raw = unsafe { ffi::rocksdb_subcompactionjobinfo_compression(self.inner) };
        DBCompressionType::try_from_raw(raw as c_int)
    }

    /// Compression used for the blob files this subcompaction wrote.
    ///
    /// `None` covers the same unnamed types as [`Self::compression`].
    pub fn blob_compression_type(&self) -> Option<DBCompressionType> {
        let raw = unsafe { ffi::rocksdb_subcompactionjobinfo_blob_compression_type(self.inner) };
        DBCompressionType::try_from_raw(raw as c_int)
    }
}

pub struct IngestionInfo {
    pub(crate) inner: *const ffi::rocksdb_externalfileingestioninfo_t,
}

impl IngestionInfo {
    pub fn cf_name(&self) -> Option<Vec<u8>> {
        unsafe {
            let mut length: usize = 0;
            let cf_name_ptr =
                ffi::rocksdb_externalfileingestioninfo_cf_name(self.inner, &raw mut length);

            if cf_name_ptr.is_null() || length == 0 {
                return None;
            }

            // SAFETY: We're copying `length` bytes from a valid, non-null pointer.
            let cf_name_vec = std::slice::from_raw_parts(cf_name_ptr.cast::<u8>(), length).to_vec();

            Some(cf_name_vec)
        }
    }

    /// The global sequence number assigned to keys in this file.
    pub fn global_seqno(&self) -> u64 {
        unsafe { ffi::rocksdb_externalfileingestioninfo_global_seqno(self.inner) }
    }

    /// Path of the ingested file outside the DB, or `None` when RocksDB left it empty.
    ///
    /// `db/c.cc` hands back the interior pointer of a `std::string` this info owns, so there
    /// is nothing to free. This copies it, the same as [`Self::cf_name`].
    pub fn external_file_path(&self) -> Option<Vec<u8>> {
        unsafe {
            let mut length: usize = 0;
            let path_ptr = ffi::rocksdb_externalfileingestioninfo_external_file_path(
                self.inner,
                &raw mut length,
            );

            if path_ptr.is_null() || length == 0 {
                return None;
            }

            // SAFETY: We're copying `length` bytes from a valid, non-null pointer.
            let path_vec = std::slice::from_raw_parts(path_ptr.cast::<u8>(), length).to_vec();

            Some(path_vec)
        }
    }

    /// Path of the ingested file inside the DB, or `None` when RocksDB left it empty.
    ///
    /// Copied out of a borrowed `std::string`, the same as [`Self::external_file_path`].
    pub fn internal_file_path(&self) -> Option<Vec<u8>> {
        unsafe {
            let mut length: usize = 0;
            let path_ptr = ffi::rocksdb_externalfileingestioninfo_internal_file_path(
                self.inner,
                &raw mut length,
            );

            if path_ptr.is_null() || length == 0 {
                return None;
            }

            // SAFETY: We're copying `length` bytes from a valid, non-null pointer.
            let path_vec = std::slice::from_raw_parts(path_ptr.cast::<u8>(), length).to_vec();

            Some(path_vec)
        }
    }

    /// Table properties of the ingested file.
    pub fn table_properties(&self) -> TableProperties<'_> {
        // SAFETY: the C API returns the address of a `TableProperties` member of this info,
        // which RocksDB owns and which lives for at least the borrow of `self`.
        unsafe {
            TableProperties::from_ptr(ffi::rocksdb_externalfileingestioninfo_table_properties(
                self.inner,
            ))
        }
    }
}

pub struct WriteStallInfo {
    pub(crate) inner: *const ffi::rocksdb_writestallinfo_t,
}

impl WriteStallInfo {
    pub fn cf_name(&self) -> Option<Vec<u8>> {
        unsafe {
            let mut length: usize = 0;
            let cf_name_ptr = ffi::rocksdb_writestallinfo_cf_name(self.inner, &raw mut length);

            if cf_name_ptr.is_null() || length == 0 {
                return None;
            }

            // SAFETY: We're copying `length` bytes from a valid, non-null pointer.
            let cf_name_vec = std::slice::from_raw_parts(cf_name_ptr.cast::<u8>(), length).to_vec();

            Some(cf_name_vec)
        }
    }

    pub fn cur(&self) -> DBWriteStallCondition {
        unsafe {
            let raw = ffi::rocksdb_writestallinfo_cur(self.inner);
            *(raw as *const DBWriteStallCondition)
        }
    }
    pub fn prev(&self) -> DBWriteStallCondition {
        unsafe {
            let raw = ffi::rocksdb_writestallinfo_prev(self.inner);
            *(raw as *const DBWriteStallCondition)
        }
    }
}

pub struct MemTableInfo {
    pub(crate) inner: *const ffi::rocksdb_memtableinfo_t,
}

impl MemTableInfo {
    pub fn cf_name(&self) -> Option<Vec<u8>> {
        unsafe {
            let mut length: usize = 0;
            let cf_name_ptr = ffi::rocksdb_memtableinfo_cf_name(self.inner, &raw mut length);

            if cf_name_ptr.is_null() || length == 0 {
                return None;
            }

            // SAFETY: We're copying `length` bytes from a valid, non-null pointer.
            let cf_name_vec = std::slice::from_raw_parts(cf_name_ptr.cast::<u8>(), length).to_vec();

            Some(cf_name_vec)
        }
    }

    pub fn first_seqno(&self) -> u64 {
        unsafe { ffi::rocksdb_memtableinfo_first_seqno(self.inner) }
    }
    pub fn earliest_seqno(&self) -> u64 {
        unsafe { ffi::rocksdb_memtableinfo_earliest_seqno(self.inner) }
    }
    pub fn num_entries(&self) -> u64 {
        unsafe { ffi::rocksdb_memtableinfo_num_entries(self.inner) }
    }
    pub fn num_deletes(&self) -> u64 {
        unsafe { ffi::rocksdb_memtableinfo_num_deletes(self.inner) }
    }

    /// The newest user-defined timestamp in the sealed memtable, or `None` when there is
    /// none.
    ///
    /// RocksDB only fills this in for a column family that has user-defined timestamps and
    /// has `persist_user_defined_timestamps` turned off, so it is `None` for every other
    /// column family. Copied out of a borrowed `std::string`, the same as [`Self::cf_name`].
    pub fn newest_udt(&self) -> Option<Vec<u8>> {
        unsafe {
            let mut length: usize = 0;
            let udt_ptr = ffi::rocksdb_memtableinfo_newest_udt(self.inner, &raw mut length);

            if udt_ptr.is_null() || length == 0 {
                return None;
            }

            // SAFETY: We're copying `length` bytes from a valid, non-null pointer.
            let udt_vec = std::slice::from_raw_parts(udt_ptr.cast::<u8>(), length).to_vec();

            Some(udt_vec)
        }
    }
}

pub struct MutableStatus {
    result: Result<(), Error>,
    ptr: *mut ffi::rust_rocksdb_status_t,
}

impl MutableStatus {
    pub fn reset(&self) {
        unsafe { ffi::rust_rocksdb_status_reset(self.ptr) }
    }

    pub fn result(&self) -> &Result<(), Error> {
        &self.result
    }

    pub fn severity(&self) -> StatusSeverity {
        unsafe { StatusSeverity::from(ffi::rust_rocksdb_status_get_severity(self.ptr)) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundErrorStatus {
    result: Result<(), Error>,
    severity: StatusSeverity,
}

impl BackgroundErrorStatus {
    pub fn result(&self) -> &Result<(), Error> {
        &self.result
    }

    pub fn severity(&self) -> StatusSeverity {
        self.severity
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundErrorRecoveryInfo {
    old_bg_error: BackgroundErrorStatus,
    new_bg_error: BackgroundErrorStatus,
}

impl BackgroundErrorRecoveryInfo {
    pub fn old_bg_error(&self) -> &BackgroundErrorStatus {
        &self.old_bg_error
    }

    pub fn new_bg_error(&self) -> &BackgroundErrorStatus {
        &self.new_bg_error
    }
}

fn result_from_error_ptr(err: *mut c_char) -> Result<(), Error> {
    if err.is_null() {
        Ok(())
    } else {
        Err(convert_rocksdb_error(err))
    }
}

fn status_result(status_ptr: *mut ffi::rust_rocksdb_status_t) -> Result<(), Error> {
    unsafe {
        let mut err: *mut c_char = std::ptr::null_mut();
        ffi::rust_rocksdb_status_get_error(status_ptr, &raw mut err);
        result_from_error_ptr(err)
    }
}

fn background_error_status(status_ptr: *mut ffi::rust_rocksdb_status_t) -> BackgroundErrorStatus {
    BackgroundErrorStatus {
        result: status_result(status_ptr),
        severity: unsafe {
            StatusSeverity::from(ffi::rust_rocksdb_status_get_severity(status_ptr))
        },
    }
}

fn background_error_recovery_info(
    info: *const ffi::rust_rocksdb_background_error_recovery_info_t,
) -> BackgroundErrorRecoveryInfo {
    unsafe {
        let mut old_bg_error: *mut c_char = std::ptr::null_mut();
        let mut new_bg_error: *mut c_char = std::ptr::null_mut();
        ffi::rust_rocksdb_background_error_recovery_info_old_bg_error(info, &raw mut old_bg_error);
        ffi::rust_rocksdb_background_error_recovery_info_new_bg_error(info, &raw mut new_bg_error);
        BackgroundErrorRecoveryInfo {
            old_bg_error: BackgroundErrorStatus {
                result: result_from_error_ptr(old_bg_error),
                severity: StatusSeverity::from(
                    ffi::rust_rocksdb_background_error_recovery_info_old_bg_error_severity(info),
                ),
            },
            new_bg_error: BackgroundErrorStatus {
                result: result_from_error_ptr(new_bg_error),
                severity: StatusSeverity::from(
                    ffi::rust_rocksdb_background_error_recovery_info_new_bg_error_severity(info),
                ),
            },
        }
    }
}

/// EventListener trait contains a set of call-back functions that will
/// be called when specific RocksDB event happens such as flush.  It can
/// be used as a building block for developing custom features such as
/// stats-collector or external compaction algorithm.
///
/// Note that call-back functions should not run for an extended period of
/// time before the function returns, otherwise RocksDB may be blocked.
/// For more information, please see
/// [doc of rocksdb](https://github.com/facebook/rocksdb/blob/master/include/rocksdb/listener.h).
pub trait EventListener: Send + Sync {
    fn on_flush_begin(&self, _: &FlushJobInfo) {}
    fn on_flush_completed(&self, _: &FlushJobInfo) {}
    fn on_compaction_begin(&self, _: &CompactionJobInfo) {}
    fn on_compaction_completed(&self, _: &CompactionJobInfo) {}
    fn on_subcompaction_begin(&self, _: &SubcompactionJobInfo) {}
    fn on_subcompaction_completed(&self, _: &SubcompactionJobInfo) {}
    fn on_external_file_ingested(&self, _: &IngestionInfo) {}
    fn on_stall_conditions_changed(&self, _: &WriteStallInfo) {}
    fn on_memtable_sealed(&self, _: &MemTableInfo) {}
    fn on_background_error(&self, _: DBBackgroundErrorReason, _: MutableStatus) {}
    fn on_error_recovery_begin(
        &self,
        _: DBBackgroundErrorReason,
        _: &BackgroundErrorStatus,
        _: &mut bool,
    ) {
    }
    fn on_error_recovery_end(&self, _: &BackgroundErrorRecoveryInfo) {}
}

extern "C" fn destructor<E: EventListener>(ctx: *mut c_void) {
    unsafe {
        drop(Box::from_raw(ctx as *mut E));
    }
}

unsafe extern "C" fn on_flush_begin<E: EventListener>(
    ctx: *mut c_void,
    info: *const ffi::rocksdb_flushjobinfo_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let info = FlushJobInfo { inner: info };
    ctx.on_flush_begin(&info);
}

extern "C" fn on_flush_completed<E: EventListener>(
    ctx: *mut c_void,
    info: *const ffi::rocksdb_flushjobinfo_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let info = FlushJobInfo { inner: info };
    ctx.on_flush_completed(&info);
}

extern "C" fn on_compaction_begin<E: EventListener>(
    ctx: *mut c_void,
    info: *const ffi::rocksdb_compactionjobinfo_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let info = CompactionJobInfo {
        inner: info,
        num_l0_files_set: true,
    };
    ctx.on_compaction_begin(&info);
}

extern "C" fn on_compaction_completed<E: EventListener>(
    ctx: *mut c_void,
    info: *const ffi::rocksdb_compactionjobinfo_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let info = CompactionJobInfo {
        inner: info,
        num_l0_files_set: true,
    };
    ctx.on_compaction_completed(&info);
}

extern "C" fn on_subcompaction_begin<E: EventListener>(
    ctx: *mut c_void,
    info: *const ffi::rocksdb_subcompactionjobinfo_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let info = SubcompactionJobInfo { inner: info };
    ctx.on_subcompaction_begin(&info);
}

extern "C" fn on_subcompaction_completed<E: EventListener>(
    ctx: *mut c_void,
    info: *const ffi::rocksdb_subcompactionjobinfo_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let info = SubcompactionJobInfo { inner: info };
    ctx.on_subcompaction_completed(&info);
}

extern "C" fn on_external_file_ingested<E: EventListener>(
    ctx: *mut c_void,
    info: *const ffi::rocksdb_externalfileingestioninfo_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let info = IngestionInfo { inner: info };
    ctx.on_external_file_ingested(&info);
}

extern "C" fn on_stall_conditions_changed<E: EventListener>(
    ctx: *mut c_void,
    info: *const ffi::rocksdb_writestallinfo_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let info = WriteStallInfo { inner: info };
    ctx.on_stall_conditions_changed(&info);
}

extern "C" fn on_memtable_sealed<E: EventListener>(
    ctx: *mut c_void,
    info: *const ffi::rocksdb_memtableinfo_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let info = MemTableInfo { inner: info };
    ctx.on_memtable_sealed(&info);
}

extern "C" fn on_background_error<E: EventListener>(
    ctx: *mut c_void,
    reason: u32,
    status_ptr: *mut ffi::rust_rocksdb_status_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let status = MutableStatus {
        result: status_result(status_ptr),
        ptr: status_ptr,
    };
    ctx.on_background_error(DBBackgroundErrorReason::from(reason), status);
}

extern "C" fn on_error_recovery_begin<E: EventListener>(
    ctx: *mut c_void,
    reason: u32,
    status_ptr: *mut ffi::rust_rocksdb_status_t,
    auto_recovery: *mut u8,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let status = background_error_status(status_ptr);
    let mut auto_recovery_value = unsafe { !auto_recovery.is_null() && *auto_recovery != 0 };
    ctx.on_error_recovery_begin(
        DBBackgroundErrorReason::from(reason),
        &status,
        &mut auto_recovery_value,
    );
    if !auto_recovery.is_null() {
        unsafe {
            *auto_recovery = u8::from(auto_recovery_value);
        }
    }
}

extern "C" fn on_error_recovery_end<E: EventListener>(
    ctx: *mut c_void,
    info: *const ffi::rust_rocksdb_background_error_recovery_info_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let info = background_error_recovery_info(info);
    ctx.on_error_recovery_end(&info);
}

pub struct DBEventListener {
    pub(crate) inner: *mut ffi::rust_rocksdb_eventlistener_t,
}

pub fn new_event_listener<E: EventListener>(e: E) -> DBEventListener {
    let p: Box<E> = Box::new(e);
    unsafe {
        DBEventListener {
            // WARNING: none of the callbacks below are actually optional.
            // Rocksdb will try calling the callback as long as there is an
            // event listener setup, this means we must define all of them
            inner: ffi::rust_rocksdb_eventlistener_create(
                Box::into_raw(p) as *mut c_void,
                Some(destructor::<E>),
                Some(on_flush_begin::<E>),
                Some(on_flush_completed::<E>),
                Some(on_compaction_begin::<E>),
                Some(on_compaction_completed::<E>),
                Some(on_subcompaction_begin::<E>),
                Some(on_subcompaction_completed::<E>),
                Some(on_external_file_ingested::<E>),
                Some(on_background_error::<E>),
                Some(on_error_recovery_begin::<E>),
                Some(on_error_recovery_end::<E>),
                Some(on_stall_conditions_changed::<E>),
                Some(on_memtable_sealed::<E>),
            ),
        }
    }
}

/// Lazy walk over one of the counted lists hanging off a job info.
///
/// Holds the borrow of the job info and the remaining index range, so a step is one indexed
/// read straight into the C++ container behind it. Nothing is allocated and nothing is
/// copied.
///
/// The length is fixed when the iterator is created, which is exact because the borrow keeps
/// the job info, and so the container, from changing underneath it.
struct JobInfoIter<'a, T, I> {
    info: &'a T,
    range: Range<usize>,
    at: fn(&'a T, usize) -> Option<I>,
}

impl<'a, T, I> JobInfoIter<'a, T, I> {
    /// Walks `len` positions of `info` through the bounds checked accessor `at`.
    fn new(info: &'a T, len: usize, at: fn(&'a T, usize) -> Option<I>) -> Self {
        Self {
            info,
            range: 0..len,
            at,
        }
    }

    /// Reads position `pos`, ending the walk if the accessor comes back empty.
    ///
    /// Every accessor these iterators drive returns a value for any position below the count
    /// they were built from, so this only bites if RocksDB left a hole in one of its own
    /// containers. Stopping beats panicking: these accessors run inside event listener
    /// callbacks, where an unwind crosses back into C++ and aborts the process.
    fn read(&mut self, pos: usize) -> Option<I> {
        let item = (self.at)(self.info, pos);
        if item.is_none() {
            self.range = 0..0;
        }
        item
    }
}

impl<T, I> Iterator for JobInfoIter<'_, T, I> {
    type Item = I;

    fn next(&mut self) -> Option<Self::Item> {
        let pos = self.range.next()?;
        self.read(pos)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.range.size_hint()
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        let pos = self.range.nth(n)?;
        self.read(pos)
    }
}

impl<T, I> DoubleEndedIterator for JobInfoIter<'_, T, I> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let pos = self.range.next_back()?;
        self.read(pos)
    }
}

impl<T, I> ExactSizeIterator for JobInfoIter<'_, T, I> {}

impl<T, I> FusedIterator for JobInfoIter<'_, T, I> {}

/// Views a borrowed pointer and length pair from the C API as bytes, mapping the null or
/// empty case to an empty slice.
///
/// # Safety
///
/// When `ptr` is non-null and `len` is non-zero, `ptr` must point at `len` initialised bytes
/// that stay valid for all of `'a`.
unsafe fn bytes_from_raw<'a>(ptr: *const c_char, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: the caller guarantees `len` readable bytes at `ptr` for `'a`. Nothing is copied
    // and nothing needs freeing, the bytes live inside the C++ object RocksDB owns.
    unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) }
}

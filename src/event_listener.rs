use crate::ffi_util::convert_rocksdb_error;
use crate::{Error, ffi};
use libc::{c_char, c_void};

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
            20 => DBCompactionReason::KNumOfReasons,
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
            _ => DBBackgroundErrorReason::KUnknown,
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
            let cf_name_vec = std::slice::from_raw_parts(cf_name_ptr as *const u8, length).to_vec();

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
}

pub struct CompactionJobInfo {
    pub(crate) inner: *const ffi::rocksdb_compactionjobinfo_t,
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
            let cf_name_vec = std::slice::from_raw_parts(cf_name_ptr as *const u8, length).to_vec();

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
            let cf_name_vec = std::slice::from_raw_parts(cf_name_ptr as *const u8, length).to_vec();

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
            let cf_name_vec = std::slice::from_raw_parts(cf_name_ptr as *const u8, length).to_vec();

            Some(cf_name_vec)
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
            let cf_name_vec = std::slice::from_raw_parts(cf_name_ptr as *const u8, length).to_vec();

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
            let cf_name_vec = std::slice::from_raw_parts(cf_name_ptr as *const u8, length).to_vec();

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
}

pub struct MutableStatus {
    result: Result<(), Error>,
    ptr: *mut ffi::rocksdb_status_ptr_t,
}

impl MutableStatus {
    pub fn reset(&self) {
        unsafe { ffi::rocksdb_reset_status(self.ptr) }
    }

    pub fn result(&self) -> &Result<(), Error> {
        &self.result
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
}

extern "C" fn destructor<E: EventListener>(ctx: *mut c_void) {
    unsafe {
        drop(Box::from_raw(ctx as *mut E));
    }
}

unsafe extern "C" fn on_flush_begin<E: EventListener>(
    ctx: *mut c_void,
    _: *mut ffi::rocksdb_t,
    info: *const ffi::rocksdb_flushjobinfo_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let info = FlushJobInfo { inner: info };
    ctx.on_flush_begin(&info);
}

extern "C" fn on_flush_completed<E: EventListener>(
    ctx: *mut c_void,
    _: *mut ffi::rocksdb_t,
    info: *const ffi::rocksdb_flushjobinfo_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let info = FlushJobInfo { inner: info };
    ctx.on_flush_completed(&info);
}

extern "C" fn on_compaction_begin<E: EventListener>(
    ctx: *mut c_void,
    _: *mut ffi::rocksdb_t,
    info: *const ffi::rocksdb_compactionjobinfo_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let info = CompactionJobInfo { inner: info };
    ctx.on_compaction_begin(&info);
}

extern "C" fn on_compaction_completed<E: EventListener>(
    ctx: *mut c_void,
    _: *mut ffi::rocksdb_t,
    info: *const ffi::rocksdb_compactionjobinfo_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let info = CompactionJobInfo { inner: info };
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
    _: *mut ffi::rocksdb_t,
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
    status_ptr: *mut ffi::rocksdb_status_ptr_t,
) {
    let ctx = unsafe { &*(ctx as *mut E) };
    let result = unsafe {
        let mut err: *mut c_char = std::ptr::null_mut();
        ffi::rocksdb_status_ptr_get_error(status_ptr, &raw mut err);
        if err.is_null() {
            Ok(())
        } else {
            Err(convert_rocksdb_error(err))
        }
    };
    let status = MutableStatus {
        result,
        ptr: status_ptr,
    };
    ctx.on_background_error(DBBackgroundErrorReason::from(reason), status);
}

pub struct DBEventListener {
    pub(crate) inner: *mut ffi::rocksdb_eventlistener_t,
}

pub fn new_event_listener<E: EventListener>(e: E) -> DBEventListener {
    let p: Box<E> = Box::new(e);
    unsafe {
        DBEventListener {
            // WARNING: none of the callbacks below are actually optional.
            // Rocksdb will try calling the callback as long as there is an
            // event listener setup, this means we must define all of them
            inner: ffi::rocksdb_eventlistener_create(
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
                Some(on_stall_conditions_changed::<E>),
                Some(on_memtable_sealed::<E>),
            ),
        }
    }
}

use crate::db_options::{DBCompactionReason, DBWriteStallCondition};
use crate::{ffi, Error};
use libc::c_void;

pub struct FlushJobInfo {
    pub(crate) inner: *const ffi::rocksdb_flushjobinfo_t,
}

impl FlushJobInfo {
    pub fn cf_name(&self) -> Option<Vec<u8>> {
        unsafe {
            let mut length: usize = 0;
            let cf_name_ptr = ffi::rocksdb_flushjobinfo_cf_name(self.inner, &mut length);

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

    // TODO: make a pr to rocksdb to expose flush reason via c api
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
            let cf_name_ptr = ffi::rocksdb_compactionjobinfo_cf_name(self.inner, &mut length);

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
            let cf_name_ptr = ffi::rocksdb_subcompactionjobinfo_cf_name(self.inner, &mut length);

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
}

pub struct IngestionInfo {
    pub(crate) inner: *const ffi::rocksdb_externalfileingestioninfo_t,
}

impl IngestionInfo {
    pub fn cf_name(&self) -> Option<Vec<u8>> {
        unsafe {
            let mut length: usize = 0;
            let cf_name_ptr =
                ffi::rocksdb_externalfileingestioninfo_cf_name(self.inner, &mut length);

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
            let cf_name_ptr = ffi::rocksdb_writestallinfo_cf_name(self.inner, &mut length);

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
            let cf_name_ptr = ffi::rocksdb_memtableinfo_cf_name(self.inner, &mut length);

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

pub struct DBEventListener {
    pub(crate) inner: *mut ffi::rocksdb_eventlistener_t,
}

#[allow(clippy::struct_excessive_bools)]
pub struct EventListenerOptions {
    pub on_flush_begin: bool,
    pub on_flush_completed: bool,
    pub on_compaction_begin: bool,
    pub on_compaction_completed: bool,
    pub on_subcompaction_begin: bool,
    pub on_subcompaction_completed: bool,
    pub on_external_file_ingested: bool,
    pub on_stall_conditions_changed: bool,
    pub on_memtable_sealed: bool,
}

pub fn new_event_listener<E: EventListener>(
    e: E,
    options: EventListenerOptions,
) -> DBEventListener {
    let p: Box<E> = Box::new(e);
    unsafe {
        DBEventListener {
            inner: ffi::rocksdb_eventlistener_create(
                Box::into_raw(p) as *mut c_void,
                Some(destructor::<E>),
                if options.on_flush_begin {
                    Some(on_flush_begin::<E>)
                } else {
                    None
                },
                if options.on_flush_completed {
                    Some(on_flush_completed::<E>)
                } else {
                    None
                },
                if options.on_compaction_begin {
                    Some(on_compaction_begin::<E>)
                } else {
                    None
                },
                if options.on_compaction_completed {
                    Some(on_compaction_completed::<E>)
                } else {
                    None
                },
                if options.on_subcompaction_begin {
                    Some(on_subcompaction_begin::<E>)
                } else {
                    None
                },
                if options.on_subcompaction_completed {
                    Some(on_subcompaction_completed::<E>)
                } else {
                    None
                },
                if options.on_external_file_ingested {
                    Some(on_external_file_ingested::<E>)
                } else {
                    None
                },
                None, // TODO: add support for on_background_error
                if options.on_stall_conditions_changed {
                    Some(on_stall_conditions_changed::<E>)
                } else {
                    None
                },
                if options.on_memtable_sealed {
                    Some(on_memtable_sealed::<E>)
                } else {
                    None
                },
            ),
        }
    }
}

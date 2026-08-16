//! Running compactions on another process or another machine.
//!
//! A compaction service splits one compaction across two sides. The primary DB
//! serializes the job, hands it to a [`CompactionService`], and waits. A worker
//! somewhere else opens the same files read only, runs the compaction with
//! [`open_and_compact`], and returns a serialized result. The primary
//! deserializes that, renames the output files into its own directory, and
//! installs them like any other compaction output.
//!
//! Both halves live here.
//!
//! On the primary side, implement [`CompactionService`] and install it with
//! `Options::set_compaction_service`. RocksDB calls
//! [`schedule`](CompactionService::schedule) with the serialized job, then
//! [`wait`](CompactionService::wait) with the job id that
//! [`schedule`](CompactionService::schedule) handed back. Getting the bytes to
//! the worker and the result back is entirely up to the implementation. This
//! crate carries no transport.
//!
//! On the worker side, call [`open_and_compact`] with the bytes that arrived,
//! the source DB path, an output directory, and a
//! [`CompactionServiceOptionsOverride`] describing how to open the column
//! family. The override matters: a compaction that needs a custom comparator,
//! merge operator, or prefix extractor produces wrong output without it, and
//! RocksDB has no way to serialize those.
//!
//! Every status a compaction service reports is a
//! [`CompactionServiceJobStatus`]. Reporting
//! [`UseLocal`](CompactionServiceJobStatus::UseLocal) at any point makes the
//! primary run the compaction itself, which is the safe answer whenever the
//! remote path is unavailable.
//!
//! Upstream marks the whole feature experimental in `options.h` and says the
//! interface will change without compatibility guarantees.

use std::ffi::CStr;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::process;
use std::ptr::{self, NonNull};
use std::slice;
use std::sync::Arc;

use libc::{c_char, c_int, c_uchar, c_void};

use crate::compaction_filter::{self, CompactionFilterCallback, CompactionFilterFn};
use crate::compaction_filter_factory::{self, CompactionFilterFactory};
use crate::comparator::Comparator;
use crate::db_options::{OptionsMustOutliveDB, OwnedCompactionFilter};
use crate::event_listener::DBCompactionReason;
use crate::ffi_util::{CStrLike, convert_rocksdb_error, raw_data_and_free, to_cpath};
use crate::file_checksum::FileChecksumGenFactory;
use crate::merge_operator::{
    self, MergeFn, MergeOperatorCallback, full_merge_callback, partial_merge_callback,
};
use crate::slice_transform::SliceTransform;
use crate::sst_partitioner::SstPartitionerFactory;
use crate::{BlockBasedOptions, CuckooTableOptions, Env, Error, InfoLogger, Options, ffi};

/// `rocksdb_compactionservice_jobstatus_*` as `c_int`, which is what every
/// callback in this API actually passes.
///
/// The generated constants are `c_uint`, and a cast is not allowed in a match
/// pattern, so they are restated here at the width they are used at.
const JOB_STATUS_SUCCESS: c_int = ffi::rocksdb_compactionservice_jobstatus_success as c_int;
const JOB_STATUS_FAILURE: c_int = ffi::rocksdb_compactionservice_jobstatus_failure as c_int;
const JOB_STATUS_ABORTED: c_int = ffi::rocksdb_compactionservice_jobstatus_aborted as c_int;
const JOB_STATUS_USE_LOCAL: c_int = ffi::rocksdb_compactionservice_jobstatus_use_local as c_int;

/// How a remote compaction job ended.
///
/// Maps onto `CompactionServiceJobStatus` in `options.h`. The same four values
/// are used for scheduling a job, waiting on it, and reporting installation.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum CompactionServiceJobStatus {
    /// The step worked.
    Success,
    /// The step failed.
    ///
    /// Reported from [`wait`](CompactionService::wait) this still allows a
    /// serialized result, and RocksDB reads the remote `Status` out of it to
    /// explain the failure. With no result the compaction fails with
    /// `Incomplete`.
    Failure,
    /// The step was cancelled.
    ///
    /// The compaction fails with `Aborted` and is not retried locally.
    Aborted,
    /// Run this compaction on the primary DB instead.
    ///
    /// The only status that leaves the DB no worse off than having no
    /// compaction service at all, so it is the right answer whenever the
    /// worker fleet cannot take the job.
    UseLocal,
}

impl CompactionServiceJobStatus {
    /// The `rocksdb_compactionservice_jobstatus_*` constant this maps to.
    fn as_raw(self) -> c_int {
        match self {
            CompactionServiceJobStatus::Success => JOB_STATUS_SUCCESS,
            CompactionServiceJobStatus::Failure => JOB_STATUS_FAILURE,
            CompactionServiceJobStatus::Aborted => JOB_STATUS_ABORTED,
            CompactionServiceJobStatus::UseLocal => JOB_STATUS_USE_LOCAL,
        }
    }

    /// Reads a raw status, or `None` for a value this crate does not name.
    fn try_from_raw(raw: c_int) -> Option<Self> {
        match raw {
            JOB_STATUS_SUCCESS => Some(CompactionServiceJobStatus::Success),
            JOB_STATUS_FAILURE => Some(CompactionServiceJobStatus::Failure),
            JOB_STATUS_ABORTED => Some(CompactionServiceJobStatus::Aborted),
            JOB_STATUS_USE_LOCAL => Some(CompactionServiceJobStatus::UseLocal),
            _ => None,
        }
    }
}

/// Which background thread pool a compaction was scheduled in.
///
/// `Env::Priority` from `env.h`. The header also has a `TOTAL` member, which
/// counts the pools rather than naming one, so it is not a variant here and
/// reads back as `None`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum EnvPriority {
    /// The pool that runs bottommost compactions.
    Bottom,
    /// The pool that runs ordinary compactions.
    Low,
    /// The pool that runs flushes.
    High,
    /// The pool that runs work submitted directly by the application.
    User,
}

impl EnvPriority {
    /// Reads a raw `Env::Priority`, or `None` for a value this crate does not
    /// name.
    fn try_from_raw(raw: c_int) -> Option<Self> {
        // env.h:441, `enum Priority { BOTTOM, LOW, HIGH, USER, TOTAL }`.
        match raw {
            0 => Some(EnvPriority::Bottom),
            1 => Some(EnvPriority::Low),
            2 => Some(EnvPriority::High),
            3 => Some(EnvPriority::User),
            _ => None,
        }
    }
}

/// Reads a raw `rocksdb::CompactionReason`.
///
/// `None` covers `kReadTriggered`, which [`DBCompactionReason`] has no variant
/// for, the `kNumOfReasons` count sentinel, and anything a newer RocksDB adds.
fn compaction_reason_from_raw(raw: c_int) -> Option<DBCompactionReason> {
    // `DBCompactionReason` stops at `KRefitLevel`, which is 19 in
    // `listener.h:113`, and its own `From<u32>` reads 20 as the count sentinel
    // rather than as `kReadTriggered`. Only the range both agree on is mapped.
    if !(0..=19).contains(&raw) {
        return None;
    }
    Some(DBCompactionReason::from(raw as u32))
}

/// Builds a slice from a pointer and length pair borrowed from C++.
///
/// # Safety
///
/// When `len` is non-zero, `ptr` must point at `len` initialised bytes that
/// stay valid for all of `'a`.
unsafe fn borrowed_slice<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if len == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(ptr, len) }
    }
}

/// Copies `bytes` into a buffer allocated with `malloc`, or `None` when the
/// allocation fails.
///
/// The buffer crosses into C++, which releases it with `free` (c.cc:1323).
/// Handing over a `Vec`'s buffer instead would make `free` responsible for
/// memory Rust's global allocator owns, which is undefined behaviour whenever
/// the two are not the same allocator.
fn malloc_copy(bytes: &[u8]) -> Option<*mut c_char> {
    debug_assert!(!bytes.is_empty(), "malloc_copy is not for empty results");
    let buffer = unsafe { libc::malloc(bytes.len()) };
    if buffer.is_null() {
        return None;
    }
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast::<u8>(), bytes.len());
    }
    Some(buffer.cast::<c_char>())
}

/// What one compaction the primary DB wants run looks like, borrowed for the
/// length of the [`schedule`](CompactionService::schedule) call.
///
/// RocksDB builds this on the compaction thread's stack
/// (`compaction_service_job.cc:76`) and drops it as soon as
/// [`schedule`](CompactionService::schedule) returns, so `'a` ties the view and
/// every byte slice it hands back to that call. Copy out anything the worker
/// needs to keep.
///
/// None of this is needed to run the compaction. The serialized input carries
/// that. This is for routing, logging, and deciding whether to take the job at
/// all.
pub struct CompactionServiceJobInfo<'a> {
    inner: *const ffi::rocksdb_compactionservice_jobinfo_t,
    _marker: PhantomData<&'a ()>,
}

impl CompactionServiceJobInfo<'_> {
    /// Wraps a job info pointer owned by RocksDB.
    ///
    /// # Safety
    ///
    /// `inner` must point at a live `rocksdb_compactionservice_jobinfo_t` that
    /// stays valid for all of `'a`. RocksDB owns it, so the caller must never
    /// free it and must not pick an `'a` outliving the callback it came from.
    unsafe fn from_ptr(inner: *const ffi::rocksdb_compactionservice_jobinfo_t) -> Self {
        Self {
            inner,
            _marker: PhantomData,
        }
    }

    /// Path of the DB the compaction belongs to.
    ///
    /// Raw bytes, because RocksDB builds this from a path this crate passes
    /// through without validating it as UTF-8. Borrowed straight from the
    /// `std::string` inside the job info (c.cc:1115), so nothing is copied and
    /// nothing needs freeing.
    pub fn db_name(&self) -> &[u8] {
        unsafe { self.string_field(ffi::rocksdb_compactionservice_jobinfo_t_get_db_name) }
    }

    /// The DB's persistent identity, which survives restarts.
    ///
    /// Pair this with [`db_session_id`](Self::db_session_id) and
    /// [`job_id`](Self::job_id) to name a job uniquely across DBs and runs.
    pub fn db_id(&self) -> &[u8] {
        unsafe { self.string_field(ffi::rocksdb_compactionservice_jobinfo_t_get_db_id) }
    }

    /// Identity of this run of the DB, regenerated on every open.
    pub fn db_session_id(&self) -> &[u8] {
        unsafe { self.string_field(ffi::rocksdb_compactionservice_jobinfo_t_get_db_session_id) }
    }

    /// Name of the column family being compacted.
    ///
    /// Raw bytes, because RocksDB does not require column family names to be
    /// UTF-8.
    pub fn cf_name(&self) -> &[u8] {
        unsafe { self.string_field(ffi::rocksdb_compactionservice_jobinfo_t_get_cf_name) }
    }

    /// Id of the column family being compacted.
    pub fn cf_id(&self) -> u32 {
        unsafe { ffi::rocksdb_compactionservice_jobinfo_t_get_cf_id(self.inner) }
    }

    /// Id of the compaction job.
    ///
    /// Only unique within the current DB and session. It restarts from zero
    /// when the DB reopens.
    pub fn job_id(&self) -> u64 {
        unsafe { ffi::rocksdb_compactionservice_jobinfo_t_get_job_id(self.inner) }
    }

    /// Which background thread pool the compaction was scheduled in, or `None`
    /// for a priority this crate does not name.
    pub fn priority(&self) -> Option<EnvPriority> {
        let raw = unsafe { ffi::rocksdb_compactionservice_jobinfo_t_get_priority(self.inner) };
        EnvPriority::try_from_raw(raw)
    }

    /// Why RocksDB started this compaction, or `None` for a reason this crate
    /// does not name.
    pub fn compaction_reason(&self) -> Option<DBCompactionReason> {
        let raw =
            unsafe { ffi::rocksdb_compactionservice_jobinfo_t_get_compaction_reason(self.inner) };
        compaction_reason_from_raw(raw)
    }

    /// The lowest level the compaction reads from.
    pub fn base_input_level(&self) -> i32 {
        unsafe { ffi::rocksdb_compactionservice_jobinfo_t_get_base_input_level(self.inner) }
    }

    /// The level the compaction writes to.
    pub fn output_level(&self) -> i32 {
        unsafe { ffi::rocksdb_compactionservice_jobinfo_t_get_output_level(self.inner) }
    }

    /// Whether the compaction covers every file in the column family.
    pub fn is_full_compaction(&self) -> bool {
        unsafe { ffi::rocksdb_compactionservice_jobinfo_t_is_full_compaction(self.inner) != 0 }
    }

    /// Whether the application asked for this compaction rather than RocksDB
    /// picking it.
    pub fn is_manual_compaction(&self) -> bool {
        unsafe { ffi::rocksdb_compactionservice_jobinfo_t_is_manual_compaction(self.inner) != 0 }
    }

    /// Whether the output level is the bottommost one holding data.
    pub fn is_bottommost_level(&self) -> bool {
        unsafe { ffi::rocksdb_compactionservice_jobinfo_t_is_bottommost_level(self.inner) != 0 }
    }

    /// Reads one of the four getters that return an interior pointer into a
    /// `std::string` plus its length.
    ///
    /// # Safety
    ///
    /// `getter` must be one of the `rocksdb_compactionservice_jobinfo_t_get_*`
    /// functions that write a length through their out parameter and return a
    /// pointer borrowed from the job info.
    unsafe fn string_field(
        &self,
        getter: unsafe extern "C" fn(
            *const ffi::rocksdb_compactionservice_jobinfo_t,
            *mut usize,
        ) -> *const c_char,
    ) -> &[u8] {
        let mut len: usize = 0;
        unsafe {
            let ptr = getter(self.inner, &raw mut len);
            borrowed_slice(ptr.cast::<u8>(), len)
        }
    }
}

/// What [`CompactionService::schedule`] hands back: a status, and a job id for
/// [`wait`](CompactionService::wait) to block on.
///
/// # Ownership
///
/// Returning one of these transfers it to RocksDB, which moves the value out
/// and runs `delete` on the wrapper (c.cc:1302 and c.cc:1303). The transfer
/// happens inside this crate's callback trampoline, which suppresses the
/// [`Drop`] below. Anything a
/// [`schedule`](CompactionService::schedule) implementation builds and then
/// discards instead is destroyed normally by that [`Drop`], so neither path
/// leaks and neither double frees.
pub struct ScheduleResponse {
    inner: NonNull<ffi::rocksdb_compactionservice_scheduleresponse_t>,
}

impl ScheduleResponse {
    /// Reports a job that reached the worker fleet, under the id
    /// [`wait`](CompactionService::wait) will be called with.
    ///
    /// The id is read as a NUL terminated string on the C++ side and handed
    /// back to [`wait`](CompactionService::wait) and
    /// [`on_installation`](CompactionService::on_installation) the same way, so
    /// it round trips unchanged.
    ///
    /// A status of [`Success`](CompactionServiceJobStatus::Success) is the only
    /// one that makes RocksDB go on to wait. The others are better expressed
    /// with [`from_status`](Self::from_status), which leaves the id empty.
    ///
    /// # Errors
    ///
    /// Returns an error if `scheduled_job_id` contains an interior NUL byte.
    pub fn scheduled(
        scheduled_job_id: impl CStrLike,
        status: CompactionServiceJobStatus,
    ) -> Result<Self, Error> {
        let job_id = scheduled_job_id
            .bake()
            .map_err(|e| Error::new(format!("scheduled job id must not contain NUL: {e}")))?;
        let inner = unsafe {
            ffi_try!(ffi::rocksdb_compactionservice_scheduleresponse_create(
                job_id.as_ptr(),
                status.as_raw(),
            ))
        };
        Ok(Self {
            inner: NonNull::new(inner)
                .expect("rocksdb_compactionservice_scheduleresponse_create returned null"),
        })
    }

    /// Reports a status with no job id, for a job that never got scheduled.
    pub fn from_status(status: CompactionServiceJobStatus) -> Self {
        // The only failure this call has is a status outside 0 to 3
        // (c.cc:1212), which `CompactionServiceJobStatus` cannot produce, so
        // the error pointer is always left null.
        let mut err: *mut c_char = ptr::null_mut();
        let inner = unsafe {
            ffi::rocksdb_compactionservice_scheduleresponse_create_with_status(
                status.as_raw(),
                &raw mut err,
            )
        };
        assert!(
            err.is_null(),
            "rocksdb_compactionservice_scheduleresponse_create_with_status rejected a status \
             this crate produced: {}",
            convert_rocksdb_error(err)
        );
        Self {
            inner: NonNull::new(inner).expect(
                "rocksdb_compactionservice_scheduleresponse_create_with_status returned null",
            ),
        }
    }

    /// The status this response carries, or `None` for a value this crate does
    /// not name.
    pub fn status(&self) -> Option<CompactionServiceJobStatus> {
        let raw = unsafe {
            ffi::rocksdb_compactionservice_scheduleresponse_getstatus(self.inner.as_ptr())
        };
        CompactionServiceJobStatus::try_from_raw(raw)
    }

    /// The job id this response carries, empty when it came from
    /// [`from_status`](Self::from_status).
    ///
    /// Borrowed from the `std::string` inside the response (c.cc:1248), so
    /// nothing is copied.
    pub fn scheduled_job_id(&self) -> &[u8] {
        let mut len: usize = 0;
        unsafe {
            let ptr = ffi::rocksdb_compactionservice_scheduleresponse_get_scheduled_job_id(
                self.inner.as_ptr(),
                &raw mut len,
            );
            borrowed_slice(ptr.cast::<u8>(), len)
        }
    }

    /// Gives the response up to RocksDB, which will `delete` it (c.cc:1303).
    fn into_raw(self) -> *mut ffi::rocksdb_compactionservice_scheduleresponse_t {
        ManuallyDrop::new(self).inner.as_ptr()
    }
}

impl Drop for ScheduleResponse {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_compactionservice_scheduleresponse_t_destroy(self.inner.as_ptr());
        }
    }
}

// SAFETY: the pointee is a `CompactionServiceScheduleResponse` value this
// handle owns outright (c.cc:662), holding a `std::string` and an enum with no
// interior mutability and no thread affinity. Nobody else points at it until
// it is handed to RocksDB, which is the last thing that happens to it, and
// every method here only reads.
unsafe impl Send for ScheduleResponse {}
unsafe impl Sync for ScheduleResponse {}

/// Somewhere for the primary DB to send its compactions.
///
/// Install one with `Options::set_compaction_service`. RocksDB then calls
/// [`schedule`](Self::schedule) instead of compacting, waits in
/// [`wait`](Self::wait) for the result, and installs the output files the
/// worker wrote.
///
/// # Threading
///
/// The methods take `&self` and the trait requires `Send + Sync`, because
/// RocksDB calls them from wherever it happens to be. Each subcompaction calls
/// [`schedule`](Self::schedule) and [`wait`](Self::wait) on its own background
/// compaction thread (`compaction_service_job.cc:86` and
/// `compaction_service_job.cc:133`), so several can be in flight at once, and
/// `CancelAllBackgroundWork` calls
/// [`cancel_awaiting_jobs`](Self::cancel_awaiting_jobs) from the caller's
/// thread while those are still blocked (`db_impl.cc:584`). One service can
/// also back several DBs, since `Options` can be cloned. Reach for a `Mutex` or
/// an atomic for anything the service needs to accumulate.
///
/// # Panics
///
/// These methods are called from C++ across an `extern "C"` boundary, where an
/// unwind is undefined behaviour. A panic that escapes any of them aborts the
/// process instead. Report a problem with
/// [`CompactionServiceJobStatus::Failure`], or with
/// [`UseLocal`](CompactionServiceJobStatus::UseLocal) to have the primary DB do
/// the work itself.
pub trait CompactionService: Send + Sync {
    /// Identifies this service in the LOG file.
    ///
    /// Read once, when the service is installed, and copied into a
    /// `std::string` on the C++ side (c.cc:1271), so the pointer behind the
    /// returned `CStr` does not have to outlive that call.
    fn name(&self) -> &CStr;

    /// Sends a compaction to the worker fleet.
    ///
    /// `input` is the serialized job. It is opaque, it is binary rather than
    /// text, and it is the only thing the worker needs in order to run the
    /// compaction: hand exactly these bytes to [`open_and_compact`] on the
    /// other side. It is borrowed from a `std::string` for the length of this
    /// call (c.cc:1295), so copy it before returning.
    ///
    /// Return [`ScheduleResponse::scheduled`] with
    /// [`Success`](CompactionServiceJobStatus::Success) and a job id to have
    /// RocksDB go on and call [`wait`](Self::wait) with that id. Anything else
    /// ends the attempt, and only
    /// [`UseLocal`](CompactionServiceJobStatus::UseLocal) makes the primary run
    /// the compaction itself.
    fn schedule(&self, info: &CompactionServiceJobInfo<'_>, input: &[u8]) -> ScheduleResponse;

    /// Blocks until the job scheduled under `scheduled_job_id` finishes.
    ///
    /// Write the bytes [`open_and_compact`] returned into `result`, which
    /// starts empty. RocksDB copies them out and frees the copy this crate
    /// makes (c.cc:1321 and c.cc:1323).
    ///
    /// A result is read for [`Success`](CompactionServiceJobStatus::Success)
    /// and for [`Failure`](CompactionServiceJobStatus::Failure), where RocksDB
    /// pulls the remote `Status` out of it to explain what went wrong. It is
    /// ignored for the other two.
    ///
    /// This blocks a background compaction thread for as long as it runs.
    fn wait(&self, scheduled_job_id: &CStr, result: &mut Vec<u8>) -> CompactionServiceJobStatus;

    /// Drops every job this service is still waiting on.
    ///
    /// Called from `CancelAllBackgroundWork`, which runs on DB shutdown, while
    /// [`wait`](Self::wait) calls are still blocked on other threads. Upstream
    /// notes in `compaction_service_job.cc:118` that there is currently no way
    /// to signal an abort to a job that is already running remotely, so this is
    /// about not waiting for them rather than stopping them.
    fn cancel_awaiting_jobs(&self) {}

    /// Reports what the primary DB did with a finished job's output.
    ///
    /// [`Success`](CompactionServiceJobStatus::Success) means the output files
    /// were renamed into the DB and installed.
    /// [`Failure`](CompactionServiceJobStatus::Failure) means the install
    /// failed part way through.
    /// [`UseLocal`](CompactionServiceJobStatus::UseLocal) means the primary
    /// could not read the result and is redoing the compaction itself, leaving
    /// the worker's output untouched in the staging directory. `status` is
    /// `None` for a value this crate does not name.
    ///
    /// This is where a worker learns it can delete a job's staged output.
    fn on_installation(
        &self,
        _scheduled_job_id: &CStr,
        _status: Option<CompactionServiceJobStatus>,
    ) {
    }
}

unsafe extern "C" fn destructor_callback<S: CompactionService>(state: *mut c_void) {
    unsafe {
        drop(Box::from_raw(state.cast::<S>()));
    }
}

unsafe extern "C" fn schedule_callback<S: CompactionService>(
    state: *mut c_void,
    info: *const ffi::rocksdb_compactionservice_jobinfo_t,
    compaction_service_input: *const c_char,
    input_len: usize,
) -> *mut ffi::rocksdb_compactionservice_scheduleresponse_t {
    let service = unsafe { &*state.cast::<S>() };
    let job_info = unsafe { CompactionServiceJobInfo::from_ptr(info) };
    let input = unsafe { borrowed_slice(compaction_service_input.cast::<u8>(), input_len) };

    let response = catch_unwind(AssertUnwindSafe(|| service.schedule(&job_info, input)));
    let Ok(response) = response else {
        process::abort()
    };
    // RocksDB moves the value out and deletes the wrapper (c.cc:1302), so the
    // handle must not run its own destructor.
    response.into_raw()
}

unsafe extern "C" fn wait_callback<S: CompactionService>(
    state: *mut c_void,
    scheduled_job_id: *const c_char,
    result: *mut *mut c_char,
    result_len: *mut usize,
) -> c_int {
    let service = unsafe { &*state.cast::<S>() };
    // c.cc passes `std::string::c_str()` (c.cc:1317), and the id originally
    // came back through the same NUL terminated path in
    // `rocksdb_compactionservice_scheduleresponse_create`, so there is no
    // length to recover and none is lost.
    let job_id = unsafe { CStr::from_ptr(scheduled_job_id) };

    let mut buffer = Vec::new();
    let status = catch_unwind(AssertUnwindSafe(|| service.wait(job_id, &mut buffer)));
    let Ok(status) = status else { process::abort() };

    if buffer.is_empty() {
        // c.cc only looks at the out parameters when the callback sets them
        // (c.cc:1319), and leaving them alone is how no result is reported.
        return status.as_raw();
    }
    let Some(copied) = malloc_copy(&buffer) else {
        // Nothing useful is left to say once the result cannot be handed over,
        // and reporting success without it would make RocksDB fail to parse an
        // empty result instead.
        return CompactionServiceJobStatus::Failure.as_raw();
    };
    unsafe {
        *result = copied;
        *result_len = buffer.len();
    }
    status.as_raw()
}

unsafe extern "C" fn cancel_awaiting_jobs_callback<S: CompactionService>(state: *mut c_void) {
    let service = unsafe { &*state.cast::<S>() };
    if catch_unwind(AssertUnwindSafe(|| service.cancel_awaiting_jobs())).is_err() {
        process::abort();
    }
}

unsafe extern "C" fn on_installation_callback<S: CompactionService>(
    state: *mut c_void,
    scheduled_job_id: *const c_char,
    status: c_int,
) {
    let service = unsafe { &*state.cast::<S>() };
    let job_id = unsafe { CStr::from_ptr(scheduled_job_id) };
    let status = CompactionServiceJobStatus::try_from_raw(status);

    if catch_unwind(AssertUnwindSafe(|| service.on_installation(job_id, status))).is_err() {
        process::abort();
    }
}

/// A `rocksdb_compactionservice_t` on its way into an `Options`.
///
/// Ownership is one way and one shot. `rocksdb_options_set_compaction_service`
/// adopts the raw pointer into a fresh `std::shared_ptr<CompactionService>`
/// (c.cc:1361), so RocksDB frees the service, and the boxed implementation
/// behind it, when the last `Options` or DB holding a reference goes away.
/// Handing the same pointer over twice would build two control blocks and free
/// it twice, so [`into_ptr`](Self::into_ptr) consumes the handle.
///
/// The C API has no `rocksdb_compactionservice_destroy`, so there is nothing to
/// call for a handle that never reaches an `Options`. Dropping one leaks. That
/// is why this is `pub(crate)` and why the only caller installs it immediately.
#[must_use]
pub(crate) struct OwnedCompactionService {
    inner: NonNull<ffi::rocksdb_compactionservice_t>,
}

impl OwnedCompactionService {
    /// Gives the service up to `rocksdb_options_set_compaction_service`.
    pub(crate) fn into_ptr(self) -> *mut ffi::rocksdb_compactionservice_t {
        self.inner.as_ptr()
    }
}

/// Wraps `service` in a `rocksdb_compactionservice_t` with this module's
/// trampolines.
pub(crate) fn new_compaction_service<S: CompactionService + 'static>(
    service: S,
) -> OwnedCompactionService {
    let state = Box::into_raw(Box::new(service));
    // Read the name through the box rather than before it, so the pointer
    // cannot be invalidated by the move into the box. C++ copies the string at
    // c.cc:1271, so it only has to survive this call.
    let name = unsafe { (*state).name().as_ptr() };
    let inner = unsafe {
        ffi::rocksdb_compactionservice_create(
            state.cast::<c_void>(),
            Some(destructor_callback::<S>),
            Some(schedule_callback::<S>),
            name,
            Some(wait_callback::<S>),
            Some(cancel_awaiting_jobs_callback::<S>),
            Some(on_installation_callback::<S>),
        )
    };
    OwnedCompactionService {
        inner: NonNull::new(inner).expect("rocksdb_compactionservice_create returned null"),
    }
}

/// Rust values a [`CompactionServiceOptionsOverride`] only borrows on the C++
/// side and therefore has to keep alive itself.
#[derive(Default)]
struct OverrideOutlive {
    /// [`CompactionServiceOptionsOverride::set_env`] stores a bare `Env*`
    /// (c.cc:1413).
    env: Option<Env>,
    /// [`CompactionServiceOptionsOverride::set_comparator`] stores a bare
    /// `const Comparator*` (c.cc:1421).
    comparator: Option<Arc<Comparator>>,
    /// [`CompactionServiceOptionsOverride::set_compaction_filter`] stores a
    /// bare `const CompactionFilter*` (c.cc:1438).
    compaction_filter: Option<OwnedCompactionFilter>,
    /// [`CompactionServiceOptionsOverride::set_info_log`] copies the
    /// `shared_ptr` (c.cc:1493), which covers the C++ logger but not the Rust
    /// closure behind a callback logger. `InfoLogger` owns that.
    info_log: Option<InfoLogger>,
    /// [`CompactionServiceOptionsOverride::from_options`] copies the `Options`'
    /// bare `env`, `comparator` and `compaction_filter` pointers
    /// (c.cc:1381, c.cc:1384, c.cc:1386).
    _from_options: Option<OptionsMustOutliveDB>,
}

/// How the worker should open the column family it is about to compact.
///
/// A serialized compaction job carries the work but not the column family's
/// configuration, and RocksDB cannot serialize a comparator, a merge operator,
/// or a prefix extractor. Whatever the primary DB was opened with has to be
/// rebuilt here, or the worker produces output the primary cannot use.
///
/// [`from_options`](Self::from_options) is the shortcut when the worker can
/// build the same [`Options`] the primary uses. [`create`](Self::create) starts
/// from RocksDB defaults instead.
///
/// Every setter here replaces the previous value and none of them can be
/// unset. The C API ignores a null argument rather than clearing the field
/// (c.cc:1412 and the setters below it).
pub struct CompactionServiceOptionsOverride {
    inner: NonNull<ffi::rocksdb_compaction_service_options_override_t>,
    outlive: OverrideOutlive,
}

// SAFETY: the pointee is a plain `CompactionServiceOptionsOverride` value
// (c.cc:735) with no interior mutability and no thread affinity. Every setter
// here takes `&mut self`, so the pointer is never aliased mutably, and
// `open_and_compact` only reads through it, copying `shared_ptr`s out with
// atomic refcount bumps. The Rust values in `OverrideOutlive` are the same ones
// `Options` keeps in `OptionsMustOutliveDB`, which `Options` is already
// declared `Send` and `Sync` over (db_options.rs:365 and db_options.rs:379),
// and nothing here ever touches them beyond holding and dropping them.
unsafe impl Send for CompactionServiceOptionsOverride {}
unsafe impl Sync for CompactionServiceOptionsOverride {}

impl Default for CompactionServiceOptionsOverride {
    fn default() -> Self {
        Self::create()
    }
}

impl CompactionServiceOptionsOverride {
    /// Starts from RocksDB's defaults: the default `Env`, the bytewise
    /// comparator, a block-based table factory with default settings, no merge
    /// operator, and no prefix extractor.
    ///
    /// The table factory is set here rather than left alone on purpose. The C
    /// struct leaves it null, and the worker copies every override field over the
    /// column family's own options unconditionally
    /// (`db/db_impl/db_impl_secondary.cc:1396`), then dereferences the table
    /// factory without a null check (`db/column_family.cc:415`). Handing a
    /// freshly created override straight to [`open_and_compact`] would therefore
    /// crash the worker, so this fills in the same default a plain [`Options`]
    /// carries.
    pub fn create() -> Self {
        let inner = unsafe { ffi::rocksdb_compaction_service_options_override_create() };
        let mut override_options = Self {
            inner: NonNull::new(inner)
                .expect("rocksdb_compaction_service_options_override_create returned null"),
            outlive: OverrideOutlive::default(),
        };
        override_options.set_block_based_table_factory(&BlockBasedOptions::default());
        override_options
    }

    /// Copies the overridable settings out of `options`.
    ///
    /// Thirteen fields are taken (c.cc:1381 to c.cc:1397): the env, file
    /// checksum generator factory, comparator, merge operator, compaction
    /// filter, compaction filter factory, prefix extractor, table factory, SST
    /// partitioner factory, event listeners, statistics, info log, and table
    /// properties collector factories. Anything else the worker needs has to go
    /// through [`set_option`](Self::set_option).
    ///
    /// Three of those are bare pointers rather than `shared_ptr`s, so this
    /// keeps a handle on whatever `options` is holding them alive with, and the
    /// override stays valid after `options` is dropped.
    pub fn from_options(options: &Options) -> Self {
        let inner = unsafe {
            ffi::rocksdb_compaction_service_options_override_create_from_options(options.inner)
        };
        Self {
            inner: NonNull::new(inner).expect(
                "rocksdb_compaction_service_options_override_create_from_options returned null",
            ),
            outlive: OverrideOutlive {
                _from_options: Some(options.outlive.clone()),
                ..OverrideOutlive::default()
            },
        }
    }

    /// Sets the environment the worker reads and writes files through.
    ///
    /// The C API stores a bare `Env*` (c.cc:1413), so this keeps a handle on
    /// `env` for as long as the override lives.
    pub fn set_env(&mut self, env: &Env) {
        unsafe {
            ffi::rocksdb_compaction_service_options_override_set_env(
                self.inner.as_ptr(),
                env.0.inner,
            );
        }
        self.outlive.env = Some(env.clone());
    }

    /// Sets the key ordering, which must be the one the primary DB uses.
    ///
    /// The C API stores a bare `const Comparator*` (c.cc:1421), so this takes
    /// an [`Arc`] and holds a clone rather than borrowing. Sharing a comparator
    /// is the normal case anyway, since the same one usually goes into the
    /// worker's own `Options`.
    pub fn set_comparator(&mut self, comparator: Arc<Comparator>) {
        unsafe {
            ffi::rocksdb_compaction_service_options_override_set_comparator(
                self.inner.as_ptr(),
                comparator.inner.as_ptr(),
            );
        }
        self.outlive.comparator = Some(comparator);
    }

    /// Sets the merge operator, which must be the one the primary DB uses.
    ///
    /// Builds the operator here instead of taking a prepared one, because the
    /// C API adopts the pointer into a `std::shared_ptr<MergeOperator>`
    /// (c.cc:1429) and RocksDB frees it from then on. Handing the same pointer
    /// to two of these would build two control blocks and free it twice, which
    /// cannot happen when the only pointer is made on the spot.
    ///
    /// The two callbacks mean what they do on
    /// [`Options::set_merge_operator`](crate::Options::set_merge_operator).
    ///
    /// # Errors
    ///
    /// Returns an error if `name` contains an interior NUL byte.
    pub fn set_merge_operator<F: MergeFn, PF: MergeFn>(
        &mut self,
        name: impl CStrLike,
        full_merge_fn: F,
        partial_merge_fn: PF,
    ) -> Result<(), Error> {
        let name = name
            .into_c_string()
            .map_err(|e| Error::new(format!("merge operator name must not contain NUL: {e}")))?;
        let callback = Box::new(MergeOperatorCallback {
            name,
            full_merge_fn,
            partial_merge_fn,
        });
        unsafe {
            let operator = ffi::rocksdb_mergeoperator_create(
                Box::into_raw(callback).cast::<c_void>(),
                Some(merge_operator::destructor_callback::<F, PF>),
                Some(full_merge_callback::<F, PF>),
                Some(partial_merge_callback::<F, PF>),
                Some(merge_operator::delete_callback),
                Some(merge_operator::name_callback::<F, PF>),
            );
            ffi::rocksdb_compaction_service_options_override_set_merge_operator(
                self.inner.as_ptr(),
                operator,
            );
        }
        Ok(())
    }

    /// Drops or rewrites entries as the worker compacts them, the way
    /// [`Options::set_compaction_filter`](crate::Options::set_compaction_filter)
    /// does on the primary.
    ///
    /// The C API stores a bare `const CompactionFilter*` (c.cc:1438) instead of
    /// adopting it, so the filter is built here and held for as long as the
    /// override lives. Setting a second one destroys the first, after the C
    /// struct has stopped pointing at it.
    ///
    /// A filter set here wins over one from
    /// [`set_compaction_filter_factory`](Self::set_compaction_filter_factory).
    /// RocksDB only asks the factory when this field is null
    /// (`compaction_job.cc:1452`).
    ///
    /// # Errors
    ///
    /// Returns an error if `name` contains an interior NUL byte.
    pub fn set_compaction_filter<F>(
        &mut self,
        name: impl CStrLike,
        filter_fn: F,
    ) -> Result<(), Error>
    where
        F: CompactionFilterFn + Send + 'static,
    {
        let name = name
            .into_c_string()
            .map_err(|e| Error::new(format!("compaction filter name must not contain NUL: {e}")))?;
        let callback = Box::new(CompactionFilterCallback { name, filter_fn });
        let raw = unsafe {
            ffi::rocksdb_compactionfilter_create(
                Box::into_raw(callback).cast::<c_void>(),
                Some(compaction_filter::destructor_callback::<CompactionFilterCallback<F>>),
                Some(compaction_filter::filter_callback::<CompactionFilterCallback<F>>),
                Some(compaction_filter::name_callback::<CompactionFilterCallback<F>>),
            )
        };
        let filter = OwnedCompactionFilter::new(
            NonNull::new(raw).expect("rocksdb_compactionfilter_create returned null"),
        );
        // Point the C struct at the new filter before replacing the field, so a
        // filter being swapped out is only destroyed once nothing references it.
        unsafe {
            ffi::rocksdb_compaction_service_options_override_set_compaction_filter(
                self.inner.as_ptr(),
                raw,
            );
        }
        self.outlive.compaction_filter = Some(filter);
        Ok(())
    }

    /// Builds a fresh compaction filter for each compaction the worker runs.
    ///
    /// Takes the factory by value because the C API adopts the pointer into a
    /// `std::shared_ptr<CompactionFilterFactory>` (c.cc:1446), which makes
    /// RocksDB responsible for freeing it.
    ///
    /// Ignored while a filter set by
    /// [`set_compaction_filter`](Self::set_compaction_filter) is in place.
    pub fn set_compaction_filter_factory<F>(&mut self, factory: F)
    where
        F: CompactionFilterFactory + 'static,
    {
        let factory = Box::new(factory);
        unsafe {
            let raw = ffi::rocksdb_compactionfilterfactory_create(
                Box::into_raw(factory).cast::<c_void>(),
                Some(compaction_filter_factory::destructor_callback::<F>),
                Some(compaction_filter_factory::create_compaction_filter_callback::<F>),
                Some(compaction_filter_factory::name_callback::<F>),
            );
            ffi::rocksdb_compaction_service_options_override_set_compaction_filter_factory(
                self.inner.as_ptr(),
                raw,
            );
        }
    }

    /// Sets the prefix extractor, which must be the one the primary DB uses.
    ///
    /// Takes the transform by value because the C API adopts the pointer into a
    /// `std::shared_ptr<const SliceTransform>` (c.cc:1455), which makes RocksDB
    /// responsible for freeing it.
    pub fn set_prefix_extractor(&mut self, prefix_extractor: SliceTransform) {
        unsafe {
            ffi::rocksdb_compaction_service_options_override_set_prefix_extractor(
                self.inner.as_ptr(),
                prefix_extractor.inner,
            );
        }
    }

    /// Writes output with a block based table factory built from
    /// `table_options`.
    ///
    /// The C API reads the options and builds a fresh factory from them
    /// (c.cc:1464), so `table_options` is free to drop as soon as this returns.
    /// A block cache set on it is carried into the factory by `shared_ptr`.
    pub fn set_block_based_table_factory(&mut self, table_options: &BlockBasedOptions) {
        unsafe {
            ffi::rocksdb_compaction_service_options_override_set_block_based_table_factory(
                self.inner.as_ptr(),
                table_options.inner,
            );
        }
    }

    /// Writes output with a cuckoo table factory built from `table_options`.
    ///
    /// Replaces any factory set by
    /// [`set_block_based_table_factory`](Self::set_block_based_table_factory),
    /// since both write the same field. The C API builds a fresh factory from
    /// the options (c.cc:1473), so `table_options` is free to drop as soon as
    /// this returns.
    pub fn set_cuckoo_table_factory(&mut self, table_options: &CuckooTableOptions) {
        unsafe {
            ffi::rocksdb_compaction_service_options_override_set_cuckoo_table_factory(
                self.inner.as_ptr(),
                table_options.inner,
            );
        }
    }

    /// Cuts output SST files on the boundaries this factory reports.
    ///
    /// The C API copies the underlying `shared_ptr` (c.cc:1517), so the
    /// caller's handle is free to drop at any time.
    pub fn set_sst_partitioner_factory(&mut self, factory: &SstPartitionerFactory) {
        unsafe {
            ffi::rocksdb_compaction_service_options_override_set_sst_partitioner_factory(
                self.inner.as_ptr(),
                factory.as_ptr(),
            );
        }
    }

    /// Records a whole file checksum for each SST the worker writes.
    ///
    /// The C API copies the underlying `shared_ptr` (c.cc:1509), so the
    /// caller's handle is free to drop at any time.
    pub fn set_file_checksum_gen_factory(&mut self, factory: &FileChecksumGenFactory) {
        unsafe {
            ffi::rocksdb_compaction_service_options_override_set_file_checksum_gen_factory(
                self.inner.as_ptr(),
                factory.as_ptr(),
            );
        }
    }

    /// Collects statistics for the compaction into the statistics object
    /// `options` carries.
    ///
    /// The C API reaches into an `Options` for its `statistics` and copies the
    /// `shared_ptr` (c.cc:1485), so this takes an [`Options`] rather than a
    /// statistics handle. Call
    /// [`Options::enable_statistics`](crate::Options::enable_statistics) on it
    /// first, otherwise there is nothing to copy and this does nothing.
    ///
    /// Upstream notes on `CompactionServiceOptionsOverride` that these counters
    /// stay on the worker. Nothing is sent back to the primary DB.
    pub fn set_statistics(&mut self, options: &Options) {
        unsafe {
            ffi::rocksdb_compaction_service_options_override_set_statistics(
                self.inner.as_ptr(),
                options.inner,
            );
        }
    }

    /// Sends the worker's log lines to `logger` instead of the default log
    /// file.
    ///
    /// Takes the logger by value, the same as
    /// [`Options::set_info_logger`](crate::Options::set_info_logger), because a
    /// callback logger owns the Rust closure it calls and the C API only copies
    /// the C++ side of it (c.cc:1493).
    pub fn set_info_log(&mut self, logger: InfoLogger) {
        unsafe {
            ffi::rocksdb_compaction_service_options_override_set_info_log(
                self.inner.as_ptr(),
                logger.inner,
            );
        }
        self.outlive.info_log = Some(logger);
    }

    /// Sets one option by its name in the options string format.
    ///
    /// The escape hatch for everything without a setter of its own, such as
    /// `compression` or `max_subcompactions`. Names and values are the ones
    /// RocksDB's options string parser takes, for example `"compression"` and
    /// `"kZSTD"`. Both are copied into the override (c.cc:1501).
    ///
    /// Upstream ignores a name it does not recognise rather than reporting it,
    /// so a typo here is silent.
    ///
    /// # Errors
    ///
    /// Returns an error if `name` or `value` contains an interior NUL byte.
    pub fn set_option(&mut self, name: impl CStrLike, value: impl CStrLike) -> Result<(), Error> {
        let name = name
            .bake()
            .map_err(|e| Error::new(format!("option name must not contain NUL: {e}")))?;
        let value = value
            .bake()
            .map_err(|e| Error::new(format!("option value must not contain NUL: {e}")))?;
        unsafe {
            ffi::rocksdb_compaction_service_options_override_set_option(
                self.inner.as_ptr(),
                name.as_ptr(),
                value.as_ptr(),
            );
        }
        Ok(())
    }

    fn as_ptr(&self) -> *const ffi::rocksdb_compaction_service_options_override_t {
        self.inner.as_ptr().cast_const()
    }
}

impl Drop for CompactionServiceOptionsOverride {
    fn drop(&mut self) {
        // The C struct holds bare pointers into the values `outlive` owns, so
        // it has to go first. `outlive` is dropped after this body returns,
        // which is the right order.
        unsafe {
            ffi::rocksdb_compaction_service_options_override_destroy(self.inner.as_ptr());
        }
    }
}

/// A flag that aborts an [`open_and_compact_with_options`] call from another
/// thread.
///
/// Create one, hand it to [`OpenAndCompactOptions::set_canceled`], keep a clone
/// of the [`Arc`] somewhere else, and call [`cancel`](Self::cancel) from that
/// other thread. Cancellation is one shot and best effort. The compaction
/// iterator checks the flag as it walks the input, so the job stops at the next
/// check rather than immediately.
///
/// There is no C API to read the flag back, so this is write only and there is
/// no way to un-cancel. Use a fresh token per call.
pub struct OpenAndCompactCancellationToken {
    inner: *mut c_uchar,
}

// SAFETY: the `unsigned char*` in the C API is a lie of convenience. Every
// access treats it as a `std::atomic<bool>*`:
// `rocksdb_open_and_compact_canceled_create` allocates one with
// `new std::atomic<bool>(false)` (c.cc:1532),
// `rocksdb_open_and_compact_canceled_set` does an atomic store through it
// (c.cc:1544), `OpenAndCompactOptions::canceled` is typed as
// `std::atomic<bool>*` (options.h:3169) and the compaction thread reads it with
// `manual_compaction_canceled_.load(std::memory_order_relaxed)`
// (db/compaction/compaction_iterator.h), and
// `rocksdb_open_and_compact_canceled_destroy` deletes it as
// `std::atomic<bool>*` (c.cc:1537). Setting the flag on one thread while the
// compaction polls it is therefore an atomic access rather than a data race,
// which is the whole point of the token.
unsafe impl Send for OpenAndCompactCancellationToken {}
unsafe impl Sync for OpenAndCompactCancellationToken {}

impl Default for OpenAndCompactCancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAndCompactCancellationToken {
    /// Allocates a fresh token in the not cancelled state.
    pub fn new() -> Self {
        let inner = unsafe { ffi::rocksdb_open_and_compact_canceled_create() };
        assert!(
            !inner.is_null(),
            "Could not create RocksDB open and compact cancellation token"
        );
        Self { inner }
    }

    /// Signals that the compaction using this token should stop.
    ///
    /// Returns as soon as the flag is stored. The compaction winds down on its
    /// own thread and the [`open_and_compact_with_options`] call it belongs to
    /// then fails.
    pub fn cancel(&self) {
        unsafe {
            ffi::rocksdb_open_and_compact_canceled_set(self.inner, 1);
        }
    }

    fn as_ptr(&self) -> *mut c_uchar {
        self.inner
    }
}

impl Drop for OpenAndCompactCancellationToken {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_open_and_compact_canceled_destroy(self.inner);
        }
    }
}

/// Extra controls for [`open_and_compact_with_options`].
///
/// These are RocksDB's `OpenAndCompactOptions` from `options.h`.
pub struct OpenAndCompactOptions {
    inner: *mut ffi::rocksdb_open_and_compact_options_t,
    /// Keeps the cancellation flag alive for as long as the C struct points at
    /// it. See [`Self::set_canceled`] for why this is an [`Arc`] and not a
    /// lifetime.
    canceled: Option<Arc<OpenAndCompactCancellationToken>>,
}

// SAFETY: the C struct is a plain `OpenAndCompactOptions` value (c.cc:739) with
// no interior mutability and no thread affinity. Every setter takes `&mut
// self`, so the raw pointer is never aliased mutably, and the getter only
// reads. The cancellation flag it can point at is an atomic owned by a `Send +
// Sync` token.
unsafe impl Send for OpenAndCompactOptions {}
unsafe impl Sync for OpenAndCompactOptions {}

impl Default for OpenAndCompactOptions {
    fn default() -> Self {
        let inner = unsafe { ffi::rocksdb_open_and_compact_options_create() };
        assert!(
            !inner.is_null(),
            "Could not create RocksDB open and compact options"
        );
        Self {
            inner,
            canceled: None,
        }
    }
}

impl OpenAndCompactOptions {
    /// Lets the compaction pick up where an earlier interrupted run left off.
    ///
    /// With this on, the worker reads any progress left in the output directory
    /// and writes new progress there as each output file completes, so a
    /// retried job only redoes the file it was in the middle of. If the saved
    /// state cannot be used, it cleans the directory and starts fresh.
    ///
    /// With this off, which is the default, the output directory must be empty
    /// before the call. Upstream is explicit that leftover files there can
    /// cause correctness errors.
    ///
    /// Upstream marks this experimental and notes it does nothing when
    /// `paranoid_file_checks` is on.
    pub fn set_allow_resumption(&mut self, allow_resumption: bool) {
        unsafe {
            ffi::rocksdb_open_and_compact_options_set_allow_resumption(
                self.inner,
                c_uchar::from(allow_resumption),
            );
        }
    }

    /// Whether resumption is on. See [`Self::set_allow_resumption`].
    pub fn allow_resumption(&self) -> bool {
        unsafe { ffi::rocksdb_open_and_compact_options_get_allow_resumption(self.inner) != 0 }
    }

    /// Attaches a cancellation token so another thread can abort the
    /// compaction.
    ///
    /// The C side stores the token as a borrowed `std::atomic<bool>*` inside
    /// the options struct (c.cc:1563), so the flag has to outlive both these
    /// options and the [`open_and_compact_with_options`] call that reads them.
    /// Holding an [`Arc`] clone enforces that at runtime and keeps
    /// `OpenAndCompactOptions` free of a lifetime parameter. Sharing the token
    /// is also the normal case, since something on another thread has to own a
    /// handle in order to cancel.
    ///
    /// There is no way to detach a token once attached. The C API ignores a
    /// null argument rather than clearing the field (c.cc:1562).
    pub fn set_canceled(&mut self, token: Arc<OpenAndCompactCancellationToken>) {
        let ptr = token.as_ptr();
        // Point the C struct at the new flag before replacing the field, so a
        // token being swapped out is only released once nothing references it.
        unsafe {
            ffi::rocksdb_open_and_compact_options_set_canceled(self.inner, ptr);
        }
        self.canceled = Some(token);
    }

    /// The cancellation token attached by [`Self::set_canceled`], if there is
    /// one.
    ///
    /// Handed back as the [`Arc`] so you can clone another handle out of it.
    pub fn canceled(&self) -> Option<&Arc<OpenAndCompactCancellationToken>> {
        self.canceled.as_ref()
    }
}

impl Drop for OpenAndCompactOptions {
    fn drop(&mut self) {
        // The C struct holds a borrowed pointer to the token's flag, so it has
        // to go first. `canceled` is dropped after this body returns, which is
        // the right order.
        unsafe {
            ffi::rocksdb_open_and_compact_options_destroy(self.inner);
        }
    }
}

/// Runs one remote compaction job and returns the serialized result.
///
/// This is the worker half of the feature. `input` is the byte for byte payload
/// that [`CompactionService::schedule`] was handed on the primary, and the
/// returned bytes are what [`CompactionService::wait`] should write into its
/// result.
///
/// `db_path` is the source DB, opened read only. `output_directory` is where
/// the new SST files are written, and the primary renames them out of there
/// when it installs the result. It must be empty going in, because this
/// entry point has no
/// [`allow_resumption`](OpenAndCompactOptions::set_allow_resumption) control
/// and upstream requires an empty directory without it.
///
/// `override_options` is not optional. The C API rejects a null override with
/// `InvalidArgument` (c.cc:1581), so pass
/// [`CompactionServiceOptionsOverride::create`] even when nothing needs
/// overriding. It is only safe to pass one straight through like that because
/// `create` fills in a default table factory, which the worker would otherwise
/// dereference as null.
///
/// # Errors
///
/// Returns an error if either path contains an interior NUL byte, or if
/// RocksDB fails to open the DB, read the input, or run the compaction.
pub fn open_and_compact<P: AsRef<Path>, Q: AsRef<Path>>(
    db_path: P,
    output_directory: Q,
    input: &[u8],
    override_options: &CompactionServiceOptionsOverride,
) -> Result<Vec<u8>, Error> {
    let db_path = to_cpath(db_path)?;
    let output_directory = to_cpath(output_directory)?;
    let mut output_len: usize = 0;

    let output = unsafe {
        ffi_try!(ffi::rocksdb_open_and_compact(
            db_path.as_ptr(),
            output_directory.as_ptr(),
            input.as_ptr().cast::<c_char>(),
            input.len(),
            &raw mut output_len,
            override_options.as_ptr(),
        ))
    };
    take_compaction_output(output, output_len)
}

/// Runs one remote compaction job under `options`, and returns the serialized
/// result.
///
/// Same as [`open_and_compact`] except that `options` adds a cancellation flag
/// and the resumption switch.
///
/// # Errors
///
/// Returns an error if either path contains an interior NUL byte, if the
/// compaction was cancelled through
/// [`OpenAndCompactOptions::set_canceled`], or if RocksDB fails to open the DB,
/// read the input, or run the compaction.
pub fn open_and_compact_with_options<P: AsRef<Path>, Q: AsRef<Path>>(
    options: &OpenAndCompactOptions,
    db_path: P,
    output_directory: Q,
    input: &[u8],
    override_options: &CompactionServiceOptionsOverride,
) -> Result<Vec<u8>, Error> {
    let db_path = to_cpath(db_path)?;
    let output_directory = to_cpath(output_directory)?;
    let mut output_len: usize = 0;

    let output = unsafe {
        ffi_try!(ffi::rocksdb_open_and_compact_with_options(
            options.inner.cast_const(),
            db_path.as_ptr(),
            output_directory.as_ptr(),
            input.as_ptr().cast::<c_char>(),
            input.len(),
            &raw mut output_len,
            override_options.as_ptr(),
        ))
    };
    take_compaction_output(output, output_len)
}

/// Copies the serialized result out of the buffer `rocksdb_open_and_compact`
/// returned, and frees it.
///
/// The buffer comes from `malloc` (c.cc:1598 and c.cc:1639), so it is copied
/// and released through `rocksdb_free` rather than handed to Rust's allocator.
///
/// Every path in c.cc that returns null records an error first, so a null here
/// with nothing in `errptr` should not happen. It is reported rather than
/// turned into an empty result, because an empty result is not something
/// RocksDB can parse.
fn take_compaction_output(output: *mut c_char, output_len: usize) -> Result<Vec<u8>, Error> {
    unsafe { raw_data_and_free(output, output_len) }.ok_or_else(|| {
        Error::new("rocksdb_open_and_compact returned no result and no error".to_owned())
    })
}

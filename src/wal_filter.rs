//! Inspecting and rewriting WAL records during recovery.
//!
//! When a DB opens, RocksDB replays every write-ahead log record that has not
//! yet made it into an SST. A WAL filter sits in that loop: it sees each record
//! before it is applied and decides whether the record is replayed as written,
//! skipped, replaced with a different batch, or treated as the end of the
//! usable log.
//!
//! This is a recovery-time hook and nothing else. It runs on the thread inside
//! [`DB::open`](crate::DB::open), only for records that recovery actually reads,
//! and never again once the DB is up. RocksDB documents it as single threaded.
//!
//! Getting a filter wrong loses writes that were already acknowledged, so the
//! usual reasons to reach for one are narrow: dropping writes for a column
//! family that is being retired, rewriting a value encoding that changed
//! between releases, or cutting recovery short at a known good point after a
//! bad shutdown.
//!
//! Install one with [`Options::set_wal_filter`](crate::Options::set_wal_filter).

use std::ffi::CStr;
use std::mem::ManuallyDrop;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process;
use std::ptr::NonNull;
use std::slice;

use libc::{c_char, c_int, c_uchar, c_ulonglong, c_void};

use crate::WriteBatch;
use crate::ffi;

/// What recovery should do with the WAL record that was just handed to the
/// filter.
///
/// Maps onto `WalFilter::WalProcessingOption` in `wal_filter.h`, with
/// [`Replace`](Self::Replace) covering the case that upstream expresses as
/// continuing while setting the `batch_changed` out-parameter.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum WalRecordAction {
    /// Replay the record unchanged.
    Continue,
    /// Replay the batch the filter wrote into `replacement` instead of the
    /// original record.
    ///
    /// The replacement must not grow the record. RocksDB compares the two
    /// operation counts and fails recovery with `NotSupported` if the
    /// replacement holds more than the original.
    Replace,
    /// Drop this record and carry on with the next one.
    Ignore,
    /// Drop this record and stop replaying.
    ///
    /// Everything from here on is discarded, including the rest of this log and
    /// every later log, and it does not come back on a subsequent recovery.
    StopReplay,
    /// Report the record as corrupt.
    ///
    /// Recovery raises `Status::Corruption` naming this filter. With
    /// [`paranoid_checks`](crate::Options::set_paranoid_checks) off, RocksDB
    /// logs the error, drops it, and replays the record anyway.
    Corrupted,
}

impl WalRecordAction {
    /// The `rocksdb_wal_filter_*` constant this maps to.
    ///
    /// `Replace` has no constant of its own. It is `continue_processing` plus
    /// the `batch_changed` flag, which the caller sets separately.
    fn as_raw(self) -> c_int {
        let raw = match self {
            WalRecordAction::Continue | WalRecordAction::Replace => {
                ffi::rocksdb_wal_filter_continue_processing
            }
            WalRecordAction::Ignore => ffi::rocksdb_wal_filter_ignore_current_record,
            WalRecordAction::StopReplay => ffi::rocksdb_wal_filter_stop_replay,
            WalRecordAction::Corrupted => ffi::rocksdb_wal_filter_corrupted_record,
        };
        raw as c_int
    }
}

/// Which log each column family still needs replayed, handed to the filter once
/// before any records are.
///
/// The borrow lasts for the callback only. The C API layer flattens RocksDB's
/// two `std::map`s into arrays on its own stack and frees them as soon as the
/// callback returns, so nothing here can be kept.
pub struct ColumnFamilyLogNumbers<'a> {
    ids: &'a [u32],
    log_numbers: &'a [u64],
    names: &'a [*const c_char],
    name_lengths: &'a [usize],
    name_ids: &'a [u32],
}

impl<'a> ColumnFamilyLogNumbers<'a> {
    /// Every column family id paired with the log number it was last flushed
    /// at.
    ///
    /// A record from a log older than a family's number is already in an SST
    /// for that family, which is how a filter decides whether a record still
    /// matters.
    pub fn log_numbers(&self) -> impl Iterator<Item = (u32, u64)> + '_ {
        self.ids
            .iter()
            .copied()
            .zip(self.log_numbers.iter().copied())
    }

    /// The log number for one column family id.
    pub fn log_number(&self, cf_id: u32) -> Option<u64> {
        let at = self.ids.iter().position(|id| *id == cf_id)?;
        self.log_numbers.get(at).copied()
    }

    /// Every column family name paired with its id.
    ///
    /// Column family handles are not open yet during recovery, so a name is all
    /// a filter has to go on. Names are raw bytes because RocksDB does not
    /// require them to be UTF-8.
    pub fn names(&self) -> impl Iterator<Item = (&'a [u8], u32)> + '_ {
        let lengths = self.name_lengths.iter().copied();
        let ids = self.name_ids.iter().copied();
        self.names
            .iter()
            .zip(lengths)
            .zip(ids)
            .map(|((name, len), id)| (unsafe { borrowed_slice(name.cast::<u8>(), len) }, id))
    }

    /// The id of the column family with this name.
    pub fn id(&self, name: &[u8]) -> Option<u32> {
        self.names()
            .find_map(|(candidate, id)| (candidate == name).then_some(id))
    }
}

/// A hook into WAL replay.
///
/// Implementations are shared: `Options` can be cloned and used to open several
/// DBs, all of which point at the same filter, so the methods take `&self` and
/// the trait requires `Send + Sync`. Reach for a `Mutex` or an atomic if the
/// filter needs to accumulate state.
///
/// # Panics
///
/// These methods are called from C++ across an `extern "C"` boundary, where an
/// unwind is undefined behaviour. A panic that escapes any of them aborts the
/// process instead. Return [`WalRecordAction::Corrupted`] to report a bad
/// record.
pub trait WalFilter: Send + Sync {
    /// Identifies this filter in the LOG file and in the error text RocksDB
    /// produces when the filter reports corruption.
    ///
    /// The pointer behind the returned `CStr` is handed to C++ as is, so it has
    /// to stay valid for as long as the filter does. A field of `self` or a
    /// `c"..."` literal both work.
    fn name(&self) -> &CStr;

    /// Called for each WAL record recovery reads.
    ///
    /// `batch` is the record as written. `replacement` starts empty and is only
    /// looked at if this returns [`WalRecordAction::Replace`], in which case it
    /// is replayed in place of `batch` and inherits the original's sequence
    /// number.
    ///
    /// `log_file_name` is the path of the log being read, for logging only. It
    /// is raw bytes because it is built from the DB path, which this crate
    /// passes through without validating it as UTF-8.
    fn log_record_found(
        &self,
        log_number: u64,
        log_file_name: &[u8],
        batch: &WriteBatch,
        replacement: &mut WriteBatch,
    ) -> WalRecordAction;

    /// Called once before replay starts, with the flush position of every
    /// column family.
    fn column_family_log_number_map(&self, _cf_log_numbers: &ColumnFamilyLogNumbers<'_>) {}
}

/// Builds a slice from a pointer and length that C++ may hand over as
/// `(null, 0)`.
///
/// `std::vector::data()` is allowed to return null for an empty vector, and
/// `slice::from_raw_parts` will not accept that.
unsafe fn borrowed_slice<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if len == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(ptr, len) }
    }
}

/// Wraps a batch RocksDB owns so the Rust side can read or write it without
/// taking on its lifetime.
///
/// Both batches in `LogRecordFound` belong to C++. The one being inspected is a
/// `const WriteBatch&` from the log reader, and the replacement is a stack local
/// in `rocksdb_walfilter_t::LogRecordFound` that c.cc moves out of afterwards
/// (c.cc:364, c.cc:372). Running [`WriteBatch`]'s destructor on either would
/// free memory this crate never allocated, so the wrapper suppresses it.
///
/// The caller must pass a live `rocksdb_writebatch_t` and must not let the
/// result escape the call it came from.
unsafe fn borrowed_batch(inner: *mut ffi::rocksdb_writebatch_t) -> ManuallyDrop<WriteBatch> {
    ManuallyDrop::new(WriteBatch { inner })
}

unsafe extern "C" fn destructor_callback<F: WalFilter>(state: *mut c_void) {
    unsafe {
        drop(Box::from_raw(state.cast::<F>()));
    }
}

unsafe extern "C" fn name_callback<F: WalFilter>(state: *mut c_void) -> *const c_char {
    let filter = unsafe { &*state.cast::<F>() };
    let name = catch_unwind(AssertUnwindSafe(|| filter.name().as_ptr()));
    let Ok(name) = name else { process::abort() };
    name
}

unsafe extern "C" fn log_record_found_callback<F: WalFilter>(
    state: *mut c_void,
    log_number: c_ulonglong,
    log_file_name: *const c_char,
    log_file_name_len: usize,
    batch: *const ffi::rocksdb_writebatch_t,
    new_batch: *mut ffi::rocksdb_writebatch_t,
    batch_changed: *mut c_uchar,
) -> c_int {
    let filter = unsafe { &*state.cast::<F>() };
    let file_name = unsafe { borrowed_slice(log_file_name.cast::<u8>(), log_file_name_len) };

    // The record arrives as `const rocksdb_writebatch_t*`, but every reader in
    // the C API takes a non-const pointer, so the cast is unavoidable and is
    // the same one c.cc performs to produce this argument (c.cc:363). Handing
    // out `&WriteBatch` keeps the filter to the read-only half of the API.
    let existing = unsafe { borrowed_batch(batch.cast_mut()) };
    let mut replacement = unsafe { borrowed_batch(new_batch) };

    let action = catch_unwind(AssertUnwindSafe(|| {
        filter.log_record_found(log_number, file_name, &existing, &mut replacement)
    }));
    let Ok(action) = action else { process::abort() };

    unsafe {
        *batch_changed = u8::from(action == WalRecordAction::Replace);
    }
    action.as_raw()
}

unsafe extern "C" fn column_family_log_number_map_callback<F: WalFilter>(
    state: *mut c_void,
    column_family_ids: *const u32,
    log_numbers: *const u64,
    column_family_log_number_count: usize,
    column_family_names: *const *const c_char,
    column_family_name_lengths: *const usize,
    column_family_name_ids: *const u32,
    column_family_name_count: usize,
) {
    let filter = unsafe { &*state.cast::<F>() };
    let cf_log_numbers = unsafe {
        ColumnFamilyLogNumbers {
            ids: borrowed_slice(column_family_ids, column_family_log_number_count),
            log_numbers: borrowed_slice(log_numbers, column_family_log_number_count),
            names: borrowed_slice(column_family_names, column_family_name_count),
            name_lengths: borrowed_slice(column_family_name_lengths, column_family_name_count),
            name_ids: borrowed_slice(column_family_name_ids, column_family_name_count),
        }
    };

    if catch_unwind(AssertUnwindSafe(|| {
        filter.column_family_log_number_map(&cf_log_numbers);
    }))
    .is_err()
    {
        process::abort();
    }
}

/// Holds a `rocksdb_walfilter_t` and destroys it when dropped.
///
/// `rocksdb_options_set_wal_filter` stores the bare pointer in
/// `DBOptions::wal_filter` (c.cc:5843) and RocksDB never takes ownership of it,
/// so this has to outlive both the options and every DB opened from them.
/// [`Options`](crate::Options) keeps it alive through `OptionsMustOutliveDB`.
pub(crate) struct OwnedWalFilter {
    inner: NonNull<ffi::rocksdb_walfilter_t>,
}

impl OwnedWalFilter {
    pub(crate) fn as_ptr(&self) -> *mut ffi::rocksdb_walfilter_t {
        self.inner.as_ptr()
    }
}

impl Drop for OwnedWalFilter {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_walfilter_destroy(self.inner.as_ptr());
        }
    }
}

// The only things behind the pointer are the callback table and the boxed `F`
// (c.cc:299), and `WalFilter` requires `Send + Sync`, so the state can be
// reached from any thread. Nothing mutates the handle after
// `rocksdb_walfilter_create`, and destruction happens once, when the last `Arc`
// holding it drops.
unsafe impl Send for OwnedWalFilter {}
unsafe impl Sync for OwnedWalFilter {}

pub(crate) fn new_wal_filter<F: WalFilter + 'static>(filter: F) -> OwnedWalFilter {
    let state = Box::into_raw(Box::new(filter)).cast::<c_void>();
    let inner = unsafe {
        ffi::rocksdb_walfilter_create(
            state,
            Some(destructor_callback::<F>),
            Some(column_family_log_number_map_callback::<F>),
            Some(log_record_found_callback::<F>),
            Some(name_callback::<F>),
        )
    };
    OwnedWalFilter {
        inner: NonNull::new(inner).expect("rocksdb_walfilter_create returned null"),
    }
}

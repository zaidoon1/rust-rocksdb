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
//

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::ffi::{CStr, CString};
use std::fmt;
use std::fs;
use std::iter;
use std::path::Path;
use std::path::PathBuf;
use std::ptr;
use std::slice;
use std::str;
use std::sync::Arc;
use std::time::Duration;

use crate::column_family::ColumnFamilyTtl;
use crate::ffi_util::CSlice;
use crate::{
    ColumnFamily, ColumnFamilyDescriptor, CompactOptions, DBIteratorWithThreadMode,
    DBPinnableBatch, DBPinnableSlice, DBRawIteratorWithThreadMode, DBWALIterator,
    DEFAULT_COLUMN_FAMILY_NAME, Direction, Error, FlushOptions, IngestExternalFileOptions,
    IteratorMode, Options, ReadOptions, SnapshotWithThreadMode, WaitForCompactOptions, WriteBatch,
    WriteBatchWithIndex, WriteOptions,
    column_family::{AsColumnFamilyRef, BoundColumnFamily, UnboundColumnFamily},
    compaction::CompactionOptions,
    db_options::{
        FlushWalOptions, ImportColumnFamilyOptions, OptionsMustOutliveDB, SizeApproximationFlags,
        SizeApproximationOptions,
    },
    event_listener::OwnedCompactionJobInfo,
    ffi,
    ffi_util::{
        CStrLike, convert_rocksdb_error, from_cstr_and_free, from_cstr_without_free,
        opt_bytes_to_ptr, raw_data, to_cpath,
    },
    metadata::{
        ColumnFamilyMetaDataOptions, LevelMetaData, LiveFilesStorageInfo,
        LiveFilesStorageInfoOptions, levels_from_cf_metadata_owned,
    },
    trace::{BlockCacheTraceOptions, BlockCacheTraceWriterOptions, Replayer, TraceOptions},
    wal::{OwnedWalFile, WalFiles},
};
use rust_librocksdb_sys::{
    rocksdb_livefile_destroy, rocksdb_livefile_t, rocksdb_livefiles_destroy, rocksdb_livefiles_t,
};

use libc::{self, c_char, c_int, c_uchar, c_void, size_t};
use parking_lot::RwLock;

// Default options are kept per-thread to avoid re-allocating on every call while
// also preventing cross-thread sharing. Some RocksDB option wrappers hold
// pointers into internal buffers and are not safe to share across threads.
// Using thread_local allows cheap reuse in the common "default options" path
// without synchronization overhead. Callers who need non-defaults must pass
// explicit options.
thread_local! { static DEFAULT_READ_OPTS: ReadOptions = ReadOptions::default(); }
thread_local! { static DEFAULT_WRITE_OPTS: WriteOptions = WriteOptions::default(); }
thread_local! { static DEFAULT_FLUSH_OPTS: FlushOptions = FlushOptions::default(); }
// Thread-local ReadOptions for hot prefix probes; preconfigured for prefix scans.
thread_local! { static PREFIX_READ_OPTS: RefCell<ReadOptions> = RefCell::new({ let mut o = ReadOptions::default(); o.set_prefix_same_as_start(true); o }); }

/// Runs `f` with `ReadOptions` bounded to `prefix` and `prefix_same_as_start`
/// enabled, reusing a thread-local instance when it is available.
///
/// The borrow is held across an FFI call that can synchronously re-enter Rust
/// through a user-supplied comparator or merge operator. If that callback probes
/// another prefix on the same thread, a plain `borrow_mut` would panic with
/// `BorrowMutError` — and because the callback runs inside an `extern "C"` frame
/// the panic aborts the process. Falling back to fresh options on contention
/// costs an allocation in that rare re-entrant case and keeps the fast path
/// allocation-free.
fn with_prefix_read_opts<R>(prefix: &[u8], f: impl FnOnce(&ReadOptions) -> R) -> R {
    PREFIX_READ_OPTS.with(|rc| {
        if let Ok(mut opts) = rc.try_borrow_mut() {
            opts.set_prefix_range_in_place(prefix);
            f(&opts)
        } else {
            let mut opts = ReadOptions::default();
            opts.set_prefix_same_as_start(true);
            opts.set_prefix_range_in_place(prefix);
            f(&opts)
        }
    })
}

/// A range of keys, `start_key` is included, but not `end_key`.
///
/// You should make sure `end_key` is not less than `start_key`.
pub struct Range<'a> {
    start_key: &'a [u8],
    end_key: &'a [u8],
}

impl<'a> Range<'a> {
    pub fn new(start_key: &'a [u8], end_key: &'a [u8]) -> Range<'a> {
        Range { start_key, end_key }
    }
}

/// Result of a [`get_into_buffer`](DBCommon::get_into_buffer) operation.
///
/// This enum represents the outcome of attempting to read a value directly
/// into a caller-provided buffer, avoiding memory allocation. This is the most
/// efficient way to read values when you have a pre-allocated buffer available.
///
/// # Performance
///
/// Using `get_into_buffer` with a reusable buffer can significantly reduce
/// allocation overhead in hot paths compared to [`get`](DBCommon::get) or even
/// [`get_pinned`](DBCommon::get_pinned):
///
/// - [`get`](DBCommon::get): Allocates a new `Vec<u8>` for each call
/// - [`get_pinned`](DBCommon::get_pinned): Pins memory in RocksDB's block cache
/// - `get_into_buffer`: Zero allocation when buffer is large enough
///
/// # Example
///
/// ```
/// use rust_rocksdb::{DB, GetIntoBufferResult};
///
/// # let tempdir = tempfile::Builder::new().prefix("ex").tempdir().unwrap();
/// let db = DB::open_default(tempdir.path()).unwrap();
/// db.put(b"key", b"value").unwrap();
///
/// let mut buffer = [0u8; 1024];
/// match db.get_into_buffer(b"key", &mut buffer).unwrap() {
///     GetIntoBufferResult::Found(len) => {
///         println!("Value: {:?}", &buffer[..len]);
///     }
///     GetIntoBufferResult::NotFound => {
///         println!("Key not found");
///     }
///     GetIntoBufferResult::BufferTooSmall(needed) => {
///         println!("Need a buffer of at least {} bytes", needed);
///     }
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GetIntoBufferResult {
    /// The key was not found in the database.
    NotFound,
    /// The value was found and successfully copied into the buffer.
    /// The `usize` contains the actual size of the value (number of bytes written).
    Found(usize),
    /// The value was found but the provided buffer was too small to hold it.
    /// The `usize` contains the actual size of the value, allowing the caller
    /// to allocate a larger buffer and retry.
    ///
    /// Note: When this variant is returned, no data is written to the buffer.
    BufferTooSmall(usize),
}

impl GetIntoBufferResult {
    /// Returns `true` if the key was found (regardless of buffer size).
    #[inline]
    pub fn is_found(&self) -> bool {
        matches!(self, Self::Found(_) | Self::BufferTooSmall(_))
    }

    /// Returns `true` if the key was not found.
    #[inline]
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound)
    }

    /// Returns the value size if the key was found, `None` otherwise.
    #[inline]
    pub fn value_size(&self) -> Option<usize> {
        match self {
            Self::Found(size) | Self::BufferTooSmall(size) => Some(*size),
            Self::NotFound => None,
        }
    }
}

/// Read options tuned for prefix probes.
fn prefix_probe_read_opts() -> ReadOptions {
    let mut opts = ReadOptions::default();
    opts.set_prefix_same_as_start(true);
    opts
}

/// A reusable prefix probe that avoids per-call iterator creation/destruction.
///
/// Use this when performing many prefix existence checks in a tight loop.
///
/// A prober reads the database as of the sequence number that was current when
/// it was created, and it pins the memtables and SST files that were current
/// then. Both are what [`refresh`](PrefixProber::refresh) exists to move
/// forward. A prober that is only used for a single burst of probes and then
/// dropped needs neither.
pub struct PrefixProber<'a, D: DBAccess> {
    raw: DBRawIteratorWithThreadMode<'a, D>,
}

impl<D: DBAccess> PrefixProber<'_, D> {
    /// Returns true if any key exists with the given prefix.
    /// This performs a seek to the prefix and checks the current key.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the seek failed. A seek that hit an error
    /// leaves the iterator invalid, so the key check below cannot observe one.
    pub fn exists(&mut self, prefix: &[u8]) -> Result<bool, Error> {
        self.raw.seek(prefix);
        if let Some(key) = self.raw.key() {
            return Ok(key.starts_with(prefix));
        }
        self.raw.status()?;
        Ok(false)
    }

    /// Moves the probe to the latest committed state of the database.
    ///
    /// Call this before reusing a prober that has been sitting idle. Writes
    /// that land after the prober was created or last refreshed are invisible
    /// to it until then.
    ///
    /// This is cheap when RocksDB's superversion has not changed, because the
    /// existing merge tree over the memtables and SST files is kept and only
    /// the read sequence moves. A flush or a compaction bumps the superversion
    /// and forces that tree to be rebuilt, which costs roughly what building a
    /// new prober costs. Refreshing a prober that has not probed yet is a
    /// pessimisation, because it builds the tree that the first
    /// [`exists`](PrefixProber::exists) call would otherwise build lazily.
    ///
    /// Refreshing also releases the memtables and SST files the prober was
    /// pinning, which is what lets a flush or a compaction reclaim them. A
    /// cached prober that is never refreshed holds them for as long as it
    /// lives, so pool them on a timer rather than on request arrival.
    ///
    /// Do not use this against a database that issues `delete_range`. RocksDB
    /// does not refresh range tombstones correctly, so a refreshed probe can
    /// report a deleted prefix as still present. See facebook/rocksdb#9255 and
    /// facebook/rocksdb#7212, both open.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the refresh failed.
    pub fn refresh(&mut self) -> Result<(), Error> {
        self.raw.refresh()
    }
}

/// A [`PrefixProber`] that keeps the database open instead of borrowing it.
///
/// Use this to cache a prober beyond the scope that built it, for example one
/// per worker thread. [`PrefixProber`] borrows the database, so it cannot be
/// held in a thread local or in shared state.
///
/// Everything [`PrefixProber`] documents about staleness and pinning applies
/// here, and matters more, because the point of an owned prober is to outlive
/// the request that created it. Refresh or drop cached probers on a timer. An
/// idle one goes on pinning the memtables and SST files it was built over, and
/// nothing will reclaim them.
///
/// `D` is `'static` because the wrapper owns the database rather than borrowing
/// it. Every `DBWithThreadMode` satisfies that.
pub struct OwnedPrefixProber<D: DBAccess + 'static> {
    // Field order is load bearing. Fields drop in declaration order, so the
    // iterator is destroyed while `_db` still holds the database open.
    // Reordering these two is a use after free.
    prober: PrefixProber<'static, D>,
    _db: Arc<D>,
}

// No `Deref`/`DerefMut` to `PrefixProber`. `DerefMut` would let two owned
// probers swap their inner probers, leaving each holding an iterator into the
// other's database, and dropping one could then free a database the other still
// points at.
impl<D: DBAccess + 'static> OwnedPrefixProber<D> {
    /// Creates an owned prober over the default column family, using read
    /// options tuned for prefix probes.
    pub fn new(db: Arc<D>) -> Self {
        Self::with_opts(db, prefix_probe_read_opts())
    }

    /// Creates an owned prober over the default column family with the given
    /// read options.
    ///
    /// The prober owns `readopts` so that any buffers it points at, such as
    /// iterate bounds, stay alive for as long as the iterator.
    pub fn with_opts(db: Arc<D>, readopts: ReadOptions) -> Self {
        // A `'static` iterator stores no dangling reference: the database is
        // recorded on `DBRawIteratorWithThreadMode` as a `PhantomData` lifetime
        // and nothing reads through it. `_db` is what actually keeps the
        // database alive, and the field order on the struct is what guarantees
        // the iterator is destroyed first.
        let raw = DBRawIteratorWithThreadMode::new(&*db, readopts);
        Self {
            prober: PrefixProber { raw },
            _db: db,
        }
    }

    /// Creates an owned prober over one column family, using read options tuned
    /// for prefix probes.
    pub fn new_cf(db: Arc<D>, cf_handle: &impl AsColumnFamilyRef) -> Self {
        Self::cf_with_opts(db, cf_handle, prefix_probe_read_opts())
    }

    /// Creates an owned prober over one column family with the given read
    /// options.
    ///
    /// `cf_handle` is only read while the iterator is being created. RocksDB
    /// takes its own reference to the column family, so the handle itself does
    /// not have to outlive the prober. Dropping the column family while a
    /// prober over it is alive is still not supported: the prober keeps reading
    /// the state it was built over, which is no longer meaningful.
    pub fn cf_with_opts(
        db: Arc<D>,
        cf_handle: &impl AsColumnFamilyRef,
        readopts: ReadOptions,
    ) -> Self {
        // See the safety note in `with_opts`.
        let raw = DBRawIteratorWithThreadMode::new_cf_detached(&*db, cf_handle.inner(), readopts);
        Self {
            prober: PrefixProber { raw },
            _db: db,
        }
    }

    /// Returns true if any key exists with the given prefix.
    ///
    /// See [`PrefixProber::exists`].
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the seek failed.
    pub fn exists(&mut self, prefix: &[u8]) -> Result<bool, Error> {
        self.prober.exists(prefix)
    }

    /// Moves the probe to the latest committed state of the database.
    ///
    /// See [`PrefixProber::refresh`], including its warning about
    /// `delete_range`.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the refresh failed.
    pub fn refresh(&mut self) -> Result<(), Error> {
        self.prober.refresh()
    }
}

/// Marker trait to specify single or multi threaded column family alternations for
/// [`DBWithThreadMode<T>`]
///
/// This arrangement makes differences in self mutability and return type in
/// some of `DBWithThreadMode` methods.
///
/// While being a marker trait to be generic over `DBWithThreadMode`, this trait
/// also has a minimum set of not-encapsulated internal methods between
/// [`SingleThreaded`] and [`MultiThreaded`].  These methods aren't expected to be
/// called and defined externally.
pub trait ThreadMode {
    /// Internal implementation for storing column family handles
    fn new_cf_map_internal(
        cf_map: BTreeMap<String, *mut ffi::rocksdb_column_family_handle_t>,
    ) -> Self;
    /// Internal implementation for dropping column family handles
    fn drop_all_cfs_internal(&mut self);
}

/// Actual marker type for the marker trait `ThreadMode`, which holds
/// a collection of column families without synchronization primitive, providing
/// no overhead for the single-threaded column family alternations. The other
/// mode is [`MultiThreaded`].
///
/// See [`DB`] for more details, including performance implications for each mode
pub struct SingleThreaded {
    pub(crate) cfs: HashMap<String, ColumnFamily>,
}

/// Actual marker type for the marker trait `ThreadMode`, which holds
/// a collection of column families wrapped in a RwLock to be mutated
/// concurrently. The other mode is [`SingleThreaded`].
///
/// See [`DB`] for more details, including performance implications for each mode
pub struct MultiThreaded {
    pub(crate) cfs: RwLock<HashMap<String, Arc<UnboundColumnFamily>>>,
}

impl ThreadMode for SingleThreaded {
    fn new_cf_map_internal(
        cfs: BTreeMap<String, *mut ffi::rocksdb_column_family_handle_t>,
    ) -> Self {
        Self {
            cfs: cfs
                .into_iter()
                .map(|(n, c)| (n, ColumnFamily { inner: c }))
                .collect(),
        }
    }

    fn drop_all_cfs_internal(&mut self) {
        // Cause all ColumnFamily objects to be Drop::drop()-ed.
        self.cfs.clear();
    }
}

impl ThreadMode for MultiThreaded {
    fn new_cf_map_internal(
        cfs: BTreeMap<String, *mut ffi::rocksdb_column_family_handle_t>,
    ) -> Self {
        Self {
            cfs: RwLock::new(
                cfs.into_iter()
                    .map(|(n, c)| (n, Arc::new(UnboundColumnFamily { inner: c })))
                    .collect(),
            ),
        }
    }

    fn drop_all_cfs_internal(&mut self) {
        // Cause all UnboundColumnFamily objects to be Drop::drop()-ed.
        self.cfs.write().clear();
    }
}

/// Get underlying `rocksdb_t`.
pub trait DBInner {
    fn inner(&self) -> *mut ffi::rocksdb_t;
}

/// A helper type to implement some common methods for [`DBWithThreadMode`]
/// and [`OptimisticTransactionDB`].
///
/// [`OptimisticTransactionDB`]: crate::OptimisticTransactionDB
///
/// When using [`SingleThreaded`] mode, `create_cf` requires `&mut self`,
/// preventing multiple immutable references from calling it concurrently:
///
/// ```compile_fail,E0596
/// use rust_rocksdb::{DBWithThreadMode, Options, SingleThreaded};
///
/// let db = DBWithThreadMode::<SingleThreaded>::open_default("/path/to/dummy").unwrap();
/// let db_ref1 = &db;
/// let db_ref2 = &db;
/// let opts = Options::default();
/// db_ref1.create_cf("cf1", &opts).unwrap();
/// db_ref2.create_cf("cf2", &opts).unwrap();
/// ```
///
/// [`SingleThreaded`]: crate::SingleThreaded
pub struct DBCommon<T: ThreadMode, D: DBInner> {
    pub(crate) inner: D,
    cfs: T, // Column families are held differently depending on thread mode
    path: PathBuf,
    _outlive: Vec<OptionsMustOutliveDB>,
    /// The TTL this DB was opened with, if it was opened with one.
    ///
    /// Two things need it. `create_cf_with_ttl` reaches a C function that casts the
    /// handle to `DBWithTTL` without checking, so calling it on any other kind of DB
    /// is undefined behaviour, and nothing in the C API can be asked after the fact.
    /// It is also the TTL that [`ColumnFamilyTtl::SameAsDb`] refers to.
    opened_with_ttl: Option<Duration>,
}

/// Minimal set of DB-related methods, intended to be generic over
/// `DBWithThreadMode<T>`. Mainly used internally
pub trait DBAccess {
    unsafe fn create_snapshot(&self) -> *const ffi::rocksdb_snapshot_t;

    unsafe fn release_snapshot(&self, snapshot: *const ffi::rocksdb_snapshot_t);

    unsafe fn create_iterator(&self, readopts: &ReadOptions) -> *mut ffi::rocksdb_iterator_t;

    unsafe fn create_iterator_cf(
        &self,
        cf_handle: *mut ffi::rocksdb_column_family_handle_t,
        readopts: &ReadOptions,
    ) -> *mut ffi::rocksdb_iterator_t;

    fn get_opt<K: AsRef<[u8]>>(
        &self,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<Vec<u8>>, Error>;

    fn get_cf_opt<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<Vec<u8>>, Error>;

    fn get_pinned_opt<K: AsRef<[u8]>>(
        &'_ self,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<DBPinnableSlice<'_>>, Error>;

    fn get_pinned_cf_opt<K: AsRef<[u8]>>(
        &'_ self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<DBPinnableSlice<'_>>, Error>;

    fn multi_get_opt<K, I>(
        &self,
        keys: I,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<Vec<u8>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>;

    fn multi_get_cf_opt<'b, K, I, W>(
        &self,
        keys_cf: I,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<Vec<u8>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = (&'b W, K)>,
        W: AsColumnFamilyRef + 'b;
}

impl<T: ThreadMode, D: DBInner> DBAccess for DBCommon<T, D> {
    unsafe fn create_snapshot(&self) -> *const ffi::rocksdb_snapshot_t {
        unsafe { ffi::rocksdb_create_snapshot(self.inner.inner()) }
    }

    unsafe fn release_snapshot(&self, snapshot: *const ffi::rocksdb_snapshot_t) {
        unsafe {
            ffi::rocksdb_release_snapshot(self.inner.inner(), snapshot);
        }
    }

    unsafe fn create_iterator(&self, readopts: &ReadOptions) -> *mut ffi::rocksdb_iterator_t {
        unsafe { ffi::rocksdb_create_iterator(self.inner.inner(), readopts.inner) }
    }

    unsafe fn create_iterator_cf(
        &self,
        cf_handle: *mut ffi::rocksdb_column_family_handle_t,
        readopts: &ReadOptions,
    ) -> *mut ffi::rocksdb_iterator_t {
        unsafe { ffi::rocksdb_create_iterator_cf(self.inner.inner(), readopts.inner, cf_handle) }
    }

    fn get_opt<K: AsRef<[u8]>>(
        &self,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<Vec<u8>>, Error> {
        self.get_opt(key, readopts)
    }

    fn get_cf_opt<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<Vec<u8>>, Error> {
        self.get_cf_opt(cf, key, readopts)
    }

    fn get_pinned_opt<K: AsRef<[u8]>>(
        &'_ self,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<DBPinnableSlice<'_>>, Error> {
        self.get_pinned_opt(key, readopts)
    }

    fn get_pinned_cf_opt<K: AsRef<[u8]>>(
        &'_ self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<DBPinnableSlice<'_>>, Error> {
        self.get_pinned_cf_opt(cf, key, readopts)
    }

    fn multi_get_opt<K, Iter>(
        &self,
        keys: Iter,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<Vec<u8>>, Error>>
    where
        K: AsRef<[u8]>,
        Iter: IntoIterator<Item = K>,
    {
        self.multi_get_opt(keys, readopts)
    }

    fn multi_get_cf_opt<'b, K, Iter, W>(
        &self,
        keys_cf: Iter,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<Vec<u8>>, Error>>
    where
        K: AsRef<[u8]>,
        Iter: IntoIterator<Item = (&'b W, K)>,
        W: AsColumnFamilyRef + 'b,
    {
        self.multi_get_cf_opt(keys_cf, readopts)
    }
}

pub struct DBWithThreadModeInner {
    inner: *mut ffi::rocksdb_t,
}

struct OwnedColumnFamilyHandle {
    inner: *mut ffi::rocksdb_column_family_handle_t,
}

struct PinnedMultiGetOutput {
    values: Vec<*mut ffi::rocksdb_pinnableslice_t>,
    errors: Vec<*mut c_char>,
}

/// The result of one `rocksdb_create_iterators` call: the iterator handles
/// plus the single `ReadOptions` they were all created from, which each
/// iterator must keep alive.
struct CreatedIterators {
    readopts: Arc<ReadOptions>,
    handles: Vec<*mut ffi::rocksdb_iterator_t>,
}

impl OwnedColumnFamilyHandle {
    fn default_for(db: *mut ffi::rocksdb_t) -> Self {
        Self {
            inner: unsafe { ffi::rocksdb_get_default_column_family_handle(db) },
        }
    }
}

impl Drop for OwnedColumnFamilyHandle {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_column_family_handle_destroy(self.inner);
        }
    }
}

impl DBInner for DBWithThreadModeInner {
    #[inline]
    fn inner(&self) -> *mut ffi::rocksdb_t {
        self.inner
    }
}

impl Drop for DBWithThreadModeInner {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_close(self.inner);
        }
    }
}

/// A type alias to RocksDB database.
///
/// See crate level documentation for a simple usage example.
/// See [`DBCommon`] for full list of methods.
pub type DBWithThreadMode<T> = DBCommon<T, DBWithThreadModeInner>;

/// A type alias to DB instance type with the single-threaded column family
/// creations/deletions
///
/// # Compatibility and multi-threaded mode
///
/// Previously, [`DB`] was defined as a direct `struct`. Now, it's type-aliased for
/// compatibility. Use `DBCommon<MultiThreaded>` for multi-threaded
/// column family alternations.
///
/// # Limited performance implication for single-threaded mode
///
/// Even with [`SingleThreaded`], almost all of RocksDB operations is
/// multi-threaded unless the underlying RocksDB instance is
/// specifically configured otherwise. `SingleThreaded` only forces
/// serialization of column family alternations by requiring `&mut self` of DB
/// instance due to its wrapper implementation details.
///
/// # Multi-threaded mode
///
/// [`MultiThreaded`] can be appropriate for the situation of multi-threaded
/// workload including multi-threaded column family alternations, costing the
/// RwLock overhead inside `DB`.
#[cfg(not(feature = "multi-threaded-cf"))]
pub type DB = DBWithThreadMode<SingleThreaded>;

#[cfg(feature = "multi-threaded-cf")]
pub type DB = DBWithThreadMode<MultiThreaded>;

// Safety note: auto-implementing Send on most db-related types is prevented by the inner FFI
// pointer. In most cases, however, this pointer is Send-safe because it is never aliased and
// rocksdb internally does not rely on thread-local information for its user-exposed types.
unsafe impl<T: ThreadMode + Send, I: DBInner> Send for DBCommon<T, I> {}

// Sync is similarly safe for many types because they do not expose interior mutability, and their
// use within the rocksdb library is generally behind a const reference
unsafe impl<T: ThreadMode, I: DBInner> Sync for DBCommon<T, I> {}

// Specifies whether open DB for read only.
enum AccessType<'a> {
    ReadWrite,
    ReadOnly { error_if_log_file_exist: bool },
    Secondary { secondary_path: &'a Path },
    WithTTL { ttl: Duration },
    TrimHistory { trim_ts: &'a [u8] },
}

/// Methods of `DBWithThreadMode`.
impl<T: ThreadMode> DBWithThreadMode<T> {
    /// Opens a database with default options.
    pub fn open_default<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        Self::open(&opts, path)
    }

    /// Opens the database with the specified options.
    pub fn open<P: AsRef<Path>>(opts: &Options, path: P) -> Result<Self, Error> {
        Self::open_cf(opts, path, None::<&str>)
    }

    /// Opens the database for read only with the specified options.
    pub fn open_for_read_only<P: AsRef<Path>>(
        opts: &Options,
        path: P,
        error_if_log_file_exist: bool,
    ) -> Result<Self, Error> {
        Self::open_cf_for_read_only(opts, path, None::<&str>, error_if_log_file_exist)
    }

    /// Opens the database as a secondary.
    pub fn open_as_secondary<P: AsRef<Path>>(
        opts: &Options,
        primary_path: P,
        secondary_path: P,
    ) -> Result<Self, Error> {
        Self::open_cf_as_secondary(opts, primary_path, secondary_path, None::<&str>)
    }

    /// Opens the database with a Time to Live compaction filter.
    ///
    /// This applies the given `ttl` to all column families created without an explicit TTL.
    /// See [`DB::open_cf_descriptors_with_ttl`] for more control over individual column family TTLs.
    ///
    /// RocksDB stores the TTL as a 32-bit second count, so a `ttl` longer than
    /// `i32::MAX` seconds (about 68 years) is clamped to that maximum rather
    /// than wrapping.
    pub fn open_with_ttl<P: AsRef<Path>>(
        opts: &Options,
        path: P,
        ttl: Duration,
    ) -> Result<Self, Error> {
        Self::open_cf_descriptors_with_ttl(opts, path, std::iter::empty(), ttl)
    }

    /// Opens the database with a Time to Live compaction filter and column family names.
    ///
    /// Column families opened using this function will be created with default `Options`.
    pub fn open_cf_with_ttl<P, I, N>(
        opts: &Options,
        path: P,
        cfs: I,
        ttl: Duration,
    ) -> Result<Self, Error>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = N>,
        N: AsRef<str>,
    {
        let cfs = cfs
            .into_iter()
            .map(|name| ColumnFamilyDescriptor::new(name.as_ref(), Options::default()));

        Self::open_cf_descriptors_with_ttl(opts, path, cfs, ttl)
    }

    /// Opens a database with the given database with a Time to Live compaction filter and
    /// column family descriptors.
    ///
    /// Applies the provided `ttl` as the default TTL for all column families.
    /// Column families will inherit this TTL by default, unless their descriptor explicitly
    /// sets a different TTL using [`ColumnFamilyTtl::Duration`] or opts out using [`ColumnFamilyTtl::Disabled`].
    ///
    /// *NOTE*: The `default` column family is opened with `Options::default()` unless
    /// explicitly configured within the `cfs` iterator.
    /// To customize the `default` column family's options, include a `ColumnFamilyDescriptor`
    /// with the name "default" in the `cfs` iterator.
    ///
    /// If you want to open `default` cf with different options, set them explicitly in `cfs`.
    pub fn open_cf_descriptors_with_ttl<P, I>(
        opts: &Options,
        path: P,
        cfs: I,
        ttl: Duration,
    ) -> Result<Self, Error>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = ColumnFamilyDescriptor>,
    {
        Self::open_cf_descriptors_internal(opts, path, cfs, &AccessType::WithTTL { ttl })
    }

    /// Opens the database and drops everything written after `trim_ts`.
    ///
    /// Column families opened using this function will be created with default
    /// `Options`. See [`open_cf_descriptors_and_trim_history`][Self::open_cf_descriptors_and_trim_history].
    ///
    /// # Errors
    ///
    /// See [`open_cf_descriptors_and_trim_history`][Self::open_cf_descriptors_and_trim_history].
    pub fn open_cf_and_trim_history<P, I, N>(
        opts: &Options,
        path: P,
        cfs: I,
        trim_ts: &[u8],
    ) -> Result<Self, Error>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = N>,
        N: AsRef<str>,
    {
        let cfs = cfs
            .into_iter()
            .map(|name| ColumnFamilyDescriptor::new(name.as_ref(), Options::default()));

        Self::open_cf_descriptors_and_trim_history(opts, path, cfs, trim_ts)
    }

    /// Opens the database and drops everything written after `trim_ts`, given column
    /// family descriptors.
    ///
    /// This is for recovering a column family that uses user-defined timestamps, where
    /// writes past a known good point have to be undone before anything reads them.
    /// Entries with a timestamp greater than `trim_ts` are gone once this returns.
    /// `trim_ts` is compared with the column family's comparator, so it has to be
    /// encoded the way that comparator expects. Column families without user-defined
    /// timestamps are left alone.
    ///
    /// The trim is permanent, so open a copy first if the discarded writes still
    /// matter.
    ///
    /// RocksDB marks the underlying API experimental and subject to change.
    ///
    /// *NOTE*: The `default` column family is opened with `Options::default()` unless
    /// explicitly configured within the `cfs` iterator. A column family that uses
    /// user-defined timestamps carries its comparator in its own options, so it has to
    /// be named here with those options even when it is the default one. Opening it
    /// with `Options::default()` instead fails with a comparator mismatch.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the database cannot be opened, which includes
    /// leaving out a column family that exists on disk. Also errors if a column family
    /// name contains an interior NUL byte, or if the directory cannot be created.
    pub fn open_cf_descriptors_and_trim_history<P, I>(
        opts: &Options,
        path: P,
        cfs: I,
        trim_ts: &[u8],
    ) -> Result<Self, Error>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = ColumnFamilyDescriptor>,
    {
        // The C function only takes the column family form, so make sure the open goes
        // down that path even when the caller named no families.
        let mut cfs: Vec<ColumnFamilyDescriptor> = cfs.into_iter().collect();
        if cfs.is_empty() {
            cfs.push(ColumnFamilyDescriptor::new(
                DEFAULT_COLUMN_FAMILY_NAME,
                Options::default(),
            ));
        }

        Self::open_cf_descriptors_internal(opts, path, cfs, &AccessType::TrimHistory { trim_ts })
    }

    /// Opens a database with the given database options and column family names.
    ///
    /// Column families opened using this function will be created with default `Options`.
    pub fn open_cf<P, I, N>(opts: &Options, path: P, cfs: I) -> Result<Self, Error>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = N>,
        N: AsRef<str>,
    {
        let cfs = cfs
            .into_iter()
            .map(|name| ColumnFamilyDescriptor::new(name.as_ref(), Options::default()));

        Self::open_cf_descriptors_internal(opts, path, cfs, &AccessType::ReadWrite)
    }

    /// Opens a database with the given database options and column family names.
    ///
    /// Column families opened using given `Options`.
    pub fn open_cf_with_opts<P, I, N>(opts: &Options, path: P, cfs: I) -> Result<Self, Error>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = (N, Options)>,
        N: AsRef<str>,
    {
        let cfs = cfs
            .into_iter()
            .map(|(name, opts)| ColumnFamilyDescriptor::new(name.as_ref(), opts));

        Self::open_cf_descriptors(opts, path, cfs)
    }

    /// Opens a database for read only with the given database options and column family names.
    /// *NOTE*: `default` column family is opened with `Options::default()`.
    /// If you want to open `default` cf with different options, set them explicitly in `cfs`.
    pub fn open_cf_for_read_only<P, I, N>(
        opts: &Options,
        path: P,
        cfs: I,
        error_if_log_file_exist: bool,
    ) -> Result<Self, Error>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = N>,
        N: AsRef<str>,
    {
        let cfs = cfs
            .into_iter()
            .map(|name| ColumnFamilyDescriptor::new(name.as_ref(), Options::default()));

        Self::open_cf_descriptors_internal(
            opts,
            path,
            cfs,
            &AccessType::ReadOnly {
                error_if_log_file_exist,
            },
        )
    }

    /// Opens a database for read only with the given database options and column family names.
    /// *NOTE*: `default` column family is opened with `Options::default()`.
    /// If you want to open `default` cf with different options, set them explicitly in `cfs`.
    pub fn open_cf_with_opts_for_read_only<P, I, N>(
        db_opts: &Options,
        path: P,
        cfs: I,
        error_if_log_file_exist: bool,
    ) -> Result<Self, Error>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = (N, Options)>,
        N: AsRef<str>,
    {
        let cfs = cfs
            .into_iter()
            .map(|(name, cf_opts)| ColumnFamilyDescriptor::new(name.as_ref(), cf_opts));

        Self::open_cf_descriptors_internal(
            db_opts,
            path,
            cfs,
            &AccessType::ReadOnly {
                error_if_log_file_exist,
            },
        )
    }

    /// Opens a database for ready only with the given database options and
    /// column family descriptors.
    /// *NOTE*: `default` column family is opened with `Options::default()`.
    /// If you want to open `default` cf with different options, set them explicitly in `cfs`.
    pub fn open_cf_descriptors_read_only<P, I>(
        opts: &Options,
        path: P,
        cfs: I,
        error_if_log_file_exist: bool,
    ) -> Result<Self, Error>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = ColumnFamilyDescriptor>,
    {
        Self::open_cf_descriptors_internal(
            opts,
            path,
            cfs,
            &AccessType::ReadOnly {
                error_if_log_file_exist,
            },
        )
    }

    /// Opens the database as a secondary with the given database options and column family names.
    /// *NOTE*: `default` column family is opened with `Options::default()`.
    /// If you want to open `default` cf with different options, set them explicitly in `cfs`.
    pub fn open_cf_as_secondary<P, I, N>(
        opts: &Options,
        primary_path: P,
        secondary_path: P,
        cfs: I,
    ) -> Result<Self, Error>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = N>,
        N: AsRef<str>,
    {
        let cfs = cfs
            .into_iter()
            .map(|name| ColumnFamilyDescriptor::new(name.as_ref(), Options::default()));

        Self::open_cf_descriptors_internal(
            opts,
            primary_path,
            cfs,
            &AccessType::Secondary {
                secondary_path: secondary_path.as_ref(),
            },
        )
    }

    /// Opens the database as a secondary with the given database options and
    /// column family descriptors.
    /// *NOTE*: `default` column family is opened with `Options::default()`.
    /// If you want to open `default` cf with different options, set them explicitly in `cfs`.
    pub fn open_cf_descriptors_as_secondary<P, I>(
        opts: &Options,
        path: P,
        secondary_path: P,
        cfs: I,
    ) -> Result<Self, Error>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = ColumnFamilyDescriptor>,
    {
        Self::open_cf_descriptors_internal(
            opts,
            path,
            cfs,
            &AccessType::Secondary {
                secondary_path: secondary_path.as_ref(),
            },
        )
    }

    /// Opens a database with the given database options and column family descriptors.
    /// *NOTE*: `default` column family is opened with `Options::default()`.
    /// If you want to open `default` cf with different options, set them explicitly in `cfs`.
    pub fn open_cf_descriptors<P, I>(opts: &Options, path: P, cfs: I) -> Result<Self, Error>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = ColumnFamilyDescriptor>,
    {
        Self::open_cf_descriptors_internal(opts, path, cfs, &AccessType::ReadWrite)
    }

    /// Internal implementation for opening RocksDB.
    fn open_cf_descriptors_internal<P, I>(
        opts: &Options,
        path: P,
        cfs: I,
        access_type: &AccessType,
    ) -> Result<Self, Error>
    where
        P: AsRef<Path>,
        I: IntoIterator<Item = ColumnFamilyDescriptor>,
    {
        let cfs: Vec<_> = cfs.into_iter().collect();
        let outlive = iter::once(opts.outlive.clone())
            .chain(cfs.iter().map(|cf| cf.options.outlive.clone()))
            .collect();

        let cpath = to_cpath(&path)?;

        if let Err(e) = fs::create_dir_all(&path) {
            return Err(Error::new(format!(
                "Failed to create RocksDB directory: `{e:?}`."
            )));
        }

        let db: *mut ffi::rocksdb_t;
        let mut cf_map = BTreeMap::new();

        if cfs.is_empty() {
            db = Self::open_raw(opts, &cpath, access_type)?;
        } else {
            let mut cfs_v = cfs;
            // Always open the default column family.
            if !cfs_v.iter().any(|cf| cf.name == DEFAULT_COLUMN_FAMILY_NAME) {
                cfs_v.push(ColumnFamilyDescriptor {
                    name: String::from(DEFAULT_COLUMN_FAMILY_NAME),
                    options: Options::default(),
                    ttl: ColumnFamilyTtl::SameAsDb,
                });
            }
            // We need to store our CStrings in an intermediate vector
            // so that their pointers remain valid.
            let c_cfs: Vec<CString> = cfs_v
                .iter()
                .map(|cf| CString::new(cf.name.as_bytes()).unwrap())
                .collect();

            let cfnames: Vec<_> = c_cfs.iter().map(|cf| cf.as_ptr()).collect();

            // These handles will be populated by DB.
            let mut cfhandles: Vec<_> = cfs_v.iter().map(|_| ptr::null_mut()).collect();

            let cfopts: Vec<_> = cfs_v
                .iter()
                .map(|cf| cf.options.inner.cast_const())
                .collect();

            db = Self::open_cf_raw(
                opts,
                &cpath,
                &cfs_v,
                &cfnames,
                &cfopts,
                &mut cfhandles,
                access_type,
            )?;
            for handle in &cfhandles {
                if handle.is_null() {
                    return Err(Error::new(
                        "Received null column family handle from DB.".to_owned(),
                    ));
                }
            }

            for (cf_desc, inner) in cfs_v.iter().zip(cfhandles) {
                cf_map.insert(cf_desc.name.clone(), inner);
            }
        }

        if db.is_null() {
            return Err(Error::new("Could not initialize database.".to_owned()));
        }

        Ok(Self {
            inner: DBWithThreadModeInner { inner: db },
            path: path.as_ref().to_path_buf(),
            cfs: T::new_cf_map_internal(cf_map),
            _outlive: outlive,
            opened_with_ttl: match access_type {
                AccessType::WithTTL { ttl } => Some(*ttl),
                _ => None,
            },
        })
    }

    fn open_raw(
        opts: &Options,
        cpath: &CString,
        access_type: &AccessType,
    ) -> Result<*mut ffi::rocksdb_t, Error> {
        let db = unsafe {
            match *access_type {
                AccessType::ReadOnly {
                    error_if_log_file_exist,
                } => ffi_try!(ffi::rocksdb_open_for_read_only(
                    opts.inner,
                    cpath.as_ptr(),
                    c_uchar::from(error_if_log_file_exist),
                )),
                AccessType::ReadWrite => {
                    ffi_try!(ffi::rocksdb_open(opts.inner, cpath.as_ptr()))
                }
                AccessType::TrimHistory { .. } => {
                    // open_cf_descriptors_and_trim_history always names at least the
                    // default column family, so this path is not reached.
                    return Err(Error::new(
                        "Trimming history requires opening with column families".to_owned(),
                    ));
                }
                AccessType::Secondary { secondary_path } => {
                    ffi_try!(ffi::rocksdb_open_as_secondary(
                        opts.inner,
                        cpath.as_ptr(),
                        to_cpath(secondary_path)?.as_ptr(),
                    ))
                }
                AccessType::WithTTL { ttl } => ffi_try!(ffi::rocksdb_open_with_ttl(
                    opts.inner,
                    cpath.as_ptr(),
                    ttl_to_seconds(ttl),
                )),
            }
        };
        Ok(db)
    }

    #[allow(clippy::pedantic)]
    fn open_cf_raw(
        opts: &Options,
        cpath: &CString,
        cfs_v: &[ColumnFamilyDescriptor],
        cfnames: &[*const c_char],
        cfopts: &[*const ffi::rocksdb_options_t],
        cfhandles: &mut [*mut ffi::rocksdb_column_family_handle_t],
        access_type: &AccessType,
    ) -> Result<*mut ffi::rocksdb_t, Error> {
        let db = unsafe {
            match *access_type {
                AccessType::ReadOnly {
                    error_if_log_file_exist,
                } => ffi_try!(ffi::rocksdb_open_for_read_only_column_families(
                    opts.inner,
                    cpath.as_ptr(),
                    cfs_v.len() as c_int,
                    cfnames.as_ptr(),
                    cfopts.as_ptr(),
                    cfhandles.as_mut_ptr(),
                    c_uchar::from(error_if_log_file_exist),
                )),
                AccessType::ReadWrite => ffi_try!(ffi::rocksdb_open_column_families(
                    opts.inner,
                    cpath.as_ptr(),
                    cfs_v.len() as c_int,
                    cfnames.as_ptr(),
                    cfopts.as_ptr(),
                    cfhandles.as_mut_ptr(),
                )),
                AccessType::Secondary { secondary_path } => {
                    ffi_try!(ffi::rocksdb_open_as_secondary_column_families(
                        opts.inner,
                        cpath.as_ptr(),
                        to_cpath(secondary_path)?.as_ptr(),
                        cfs_v.len() as c_int,
                        cfnames.as_ptr(),
                        cfopts.as_ptr(),
                        cfhandles.as_mut_ptr(),
                    ))
                }
                AccessType::WithTTL { ttl } => {
                    let ttls: Vec<_> = cfs_v
                        .iter()
                        .map(|cf| cf_ttl_to_seconds(cf.ttl, ttl))
                        .collect();

                    ffi_try!(ffi::rocksdb_open_column_families_with_ttl(
                        opts.inner,
                        cpath.as_ptr(),
                        cfs_v.len() as c_int,
                        cfnames.as_ptr(),
                        cfopts.as_ptr(),
                        cfhandles.as_mut_ptr(),
                        ttls.as_ptr(),
                    ))
                }
                AccessType::TrimHistory { trim_ts } => {
                    // The C function copies the timestamp into a std::string before
                    // doing anything with it, so this only has to stay alive across
                    // the call. It takes a non-const pointer without writing through
                    // it, hence the local copy rather than casting the borrow.
                    let mut trim_ts = trim_ts.to_vec();
                    ffi_try!(ffi::rocksdb_open_and_trim_history(
                        opts.inner,
                        cpath.as_ptr(),
                        cfs_v.len() as c_int,
                        cfnames.as_ptr(),
                        cfopts.as_ptr(),
                        cfhandles.as_mut_ptr(),
                        trim_ts.as_mut_ptr().cast::<c_char>(),
                        trim_ts.len(),
                    ))
                }
            }
        };
        Ok(db)
    }

    /// Manually, synchronously attempt to resume DB writes after a write failure
    /// to the underlying filesystem. Returns OK if writes are successfully resumed,
    /// or there was no outstanding error to recover from. Returns underlying write
    /// error if it is not recoverable. Returns [`crate::ErrorKind::Busy`] if an
    /// auto-resume is in progress, without waiting for it to complete.
    ///
    /// See <https://github.com/facebook/rocksdb/wiki/Background-Error-Handling>
    /// See [`crate::Options::set_max_bgerror_resume_count`]
    /// See [`crate::event_listener::EventListener::on_error_recovery_begin`]
    pub fn resume(&self) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rust_rocksdb_resume(self.inner.inner()));
        }

        Ok(())
    }

    /// Removes the database entries in the range `["from", "to")` using given write options.
    pub fn delete_range_cf_opt<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        from: K,
        to: K,
        writeopts: &WriteOptions,
    ) -> Result<(), Error> {
        let from = from.as_ref();
        let to = to.as_ref();

        unsafe {
            ffi_try!(ffi::rocksdb_delete_range_cf(
                self.inner.inner(),
                writeopts.inner,
                cf.inner(),
                from.as_ptr() as *const c_char,
                from.len() as size_t,
                to.as_ptr() as *const c_char,
                to.len() as size_t,
            ));
            Ok(())
        }
    }

    /// Removes the database entries in the range `["from", "to")` using default write options.
    pub fn delete_range_cf<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        from: K,
        to: K,
    ) -> Result<(), Error> {
        DEFAULT_WRITE_OPTS.with(|opts| self.delete_range_cf_opt(cf, from, to, opts))
    }

    pub fn write_opt(&self, batch: &WriteBatch, writeopts: &WriteOptions) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_write(
                self.inner.inner(),
                writeopts.inner,
                batch.inner
            ));
        }
        Ok(())
    }

    pub fn write(&self, batch: &WriteBatch) -> Result<(), Error> {
        DEFAULT_WRITE_OPTS.with(|opts| self.write_opt(batch, opts))
    }

    pub fn write_without_wal(&self, batch: &WriteBatch) -> Result<(), Error> {
        let mut wo = WriteOptions::new();
        wo.disable_wal(true);
        self.write_opt(batch, &wo)
    }

    pub fn write_wbwi(&self, wbwi: &WriteBatchWithIndex) -> Result<(), Error> {
        DEFAULT_WRITE_OPTS.with(|opts| self.write_wbwi_opt(wbwi, opts))
    }

    pub fn write_wbwi_opt(
        &self,
        wbwi: &WriteBatchWithIndex,
        writeopts: &WriteOptions,
    ) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_write_writebatch_wi(
                self.inner.inner(),
                writeopts.inner,
                wbwi.inner
            ));

            Ok(())
        }
    }
}

/// A value read from a DB with user-defined timestamps, and the timestamp it carries.
///
/// Both halves are RocksDB allocations this owns, freed on drop. Read them as bytes
/// through `AsRef<[u8]>`.
///
/// The timestamp is the raw bytes RocksDB stored, in whatever width and encoding the
/// column family's comparator defines, so it is only meaningful to code that knows
/// that comparator. RocksDB's own `comparator_with_u64_ts` uses a little endian
/// `u64`.
pub struct TimestampedValue {
    /// The value stored under the key.
    pub value: CSlice,
    /// The timestamp the value was written with.
    pub timestamp: CSlice,
}

impl fmt::Debug for TimestampedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimestampedValue")
            .field("value", &self.value.as_ref().len())
            .field("timestamp", &self.timestamp.as_ref())
            .finish()
    }
}

/// What one `rocksdb_create_column_families` call managed to create.
///
/// The call is not atomic, so it can commit some families and then fail. Both
/// fields can be non-empty at once, and the handles have to be recorded even when
/// there is an error.
struct CreatedCfHandles {
    /// Owned handles for the families that were created, in the order the names
    /// were given. Each one needs `rocksdb_column_family_handle_destroy`.
    handles: Vec<*mut ffi::rocksdb_column_family_handle_t>,
    /// Why the remaining families were not created.
    error: Option<Error>,
}

impl CreatedCfHandles {
    /// Nothing was created, because the call never got as far as RocksDB.
    fn failed(error: Error) -> Self {
        Self {
            handles: Vec::new(),
            error: Some(error),
        }
    }
}

/// What a [`DBCommon::compact_files`] call produced.
pub struct CompactFilesResult {
    /// The SST files the compaction wrote, as RocksDB names them.
    ///
    /// Empty when the compaction had nothing to write, for instance when every
    /// input key was deleted.
    pub output_files: Vec<String>,
    /// Statistics and file lists for the compaction that just ran.
    ///
    /// `None` when [`CompactionOptions::set_allow_trivial_move`] is enabled.
    /// RocksDB can then satisfy the request by moving files between levels
    /// instead of rewriting them, and that path returns success without
    /// reporting anything about the work it did. Nothing distinguishes it from
    /// the rewriting path afterwards, so the statistics are not collected at all
    /// rather than sometimes being made up.
    ///
    /// [`CompactionOptions::set_allow_trivial_move`]:
    ///     crate::compaction::CompactionOptions::set_allow_trivial_move
    pub job_info: Option<OwnedCompactionJobInfo>,
}

impl fmt::Debug for CompactFilesResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompactFilesResult")
            .field("output_files", &self.output_files)
            .field("job_info", &self.job_info)
            .finish()
    }
}

/// Splits an optional key into the pointer and length a RocksDB range bound wants.
///
/// A `None` bound becomes a null pointer, which RocksDB reads as unbounded. This is
/// why the bound is `Option<&T>` rather than `&[u8]`: an empty slice has a non-null
/// pointer and means the empty key, which is a different request.
fn optional_key_parts<T: AsRef<[u8]> + ?Sized>(key: Option<&T>) -> (*const c_char, usize) {
    (opt_bytes_to_ptr(key), key.map_or(0, |k| k.as_ref().len()))
}

/// Reads a `char**` array RocksDB allocated into owned `String`s and frees it.
///
/// The strings are read without freeing them individually because
/// `rocksdb_compact_files_output_file_names_destroy` frees the whole array,
/// entries included.
///
/// # Safety
///
/// `names` must be null, or an array of `count` NUL-terminated strings allocated
/// by `rocksdb_compact_files`, which nothing else will free.
unsafe fn collect_and_free_output_names(names: *mut *mut c_char, count: usize) -> Vec<String> {
    if names.is_null() || count == 0 {
        return Vec::new();
    }
    let collected = (0..count)
        .map(|i| unsafe { from_cstr_without_free(*names.add(i)) })
        .collect();
    unsafe { ffi::rocksdb_compact_files_output_file_names_destroy(names, count) };
    collected
}

/// The pointer arrays the `rocksdb_approximate_sizes*` family takes, kept in one
/// place because all five variants want the same six arguments.
///
/// The key pointers borrow from the `Range` slice, so this must not outlive it.
struct ApproximateSizesArgs {
    count: c_int,
    start_keys: Vec<*const c_char>,
    start_key_lens: Vec<usize>,
    end_keys: Vec<*const c_char>,
    end_key_lens: Vec<usize>,
    sizes: Vec<u64>,
}

impl ApproximateSizesArgs {
    fn new(ranges: &[Range]) -> Self {
        Self {
            count: c_int::try_from(ranges.len()).unwrap_or(c_int::MAX),
            start_keys: ranges
                .iter()
                .map(|x| x.start_key.as_ptr().cast::<c_char>())
                .collect(),
            start_key_lens: ranges.iter().map(|x| x.start_key.len()).collect(),
            end_keys: ranges
                .iter()
                .map(|x| x.end_key.as_ptr().cast::<c_char>())
                .collect(),
            end_key_lens: ranges.iter().map(|x| x.end_key.len()).collect(),
            sizes: vec![0; ranges.len()],
        }
    }

    /// Turns the `errptr` RocksDB filled in into a `Result`, yielding the sizes
    /// on success.
    ///
    /// RocksDB reports failures here through `errptr`. Ignoring it both leaked
    /// the `strdup`ed message and returned a vector of zeros that the caller
    /// could not distinguish from "these ranges are empty".
    fn finish(&mut self, err: *mut c_char) -> Result<Vec<u64>, Error> {
        if !err.is_null() {
            return Err(convert_rocksdb_error(err));
        }
        Ok(std::mem::take(&mut self.sizes))
    }
}

/// Common methods of `DBWithThreadMode` and `OptimisticTransactionDB`.
impl<T: ThreadMode, D: DBInner> DBCommon<T, D> {
    pub(crate) fn new(inner: D, cfs: T, path: PathBuf, outlive: Vec<OptionsMustOutliveDB>) -> Self {
        Self {
            inner,
            cfs,
            path,
            _outlive: outlive,
            opened_with_ttl: None,
        }
    }

    pub fn list_cf<P: AsRef<Path>>(opts: &Options, path: P) -> Result<Vec<String>, Error> {
        let cpath = to_cpath(path)?;
        let mut length = 0;

        unsafe {
            let ptr = ffi_try!(ffi::rocksdb_list_column_families(
                opts.inner,
                cpath.as_ptr(),
                &raw mut length,
            ));

            let vec = slice::from_raw_parts(ptr, length)
                .iter()
                .map(|ptr| from_cstr_without_free(*ptr))
                .collect();
            ffi::rocksdb_list_column_families_destroy(ptr, length);
            Ok(vec)
        }
    }

    pub fn destroy<P: AsRef<Path>>(opts: &Options, path: P) -> Result<(), Error> {
        let cpath = to_cpath(path)?;
        unsafe {
            ffi_try!(ffi::rocksdb_destroy_db(opts.inner, cpath.as_ptr()));
        }
        Ok(())
    }

    pub fn repair<P: AsRef<Path>>(opts: &Options, path: P) -> Result<(), Error> {
        let cpath = to_cpath(path)?;
        unsafe {
            ffi_try!(ffi::rocksdb_repair_db(opts.inner, cpath.as_ptr()));
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    /// Flushes the WAL buffer. If `sync` is set to `true`, also syncs
    /// the data to disk.
    pub fn flush_wal(&self, sync: bool) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_flush_wal(
                self.inner.inner(),
                c_uchar::from(sync)
            ));
        }
        Ok(())
    }

    /// Flushes the WAL buffer, taking the rate limiter priority as well as `sync`.
    ///
    /// [`flush_wal`](Self::flush_wal) covers the common case. Reach for this one to
    /// give the flush a priority other than the default, which decides how the WAL
    /// write is charged against a configured rate limiter.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the flush or the sync fails.
    pub fn flush_wal_with_options(&self, opts: &FlushWalOptions) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_flush_wal_with_options(
                self.inner.inner(),
                opts.as_ptr()
            ));
        }
        Ok(())
    }

    /// Stops background flushes and compactions and waits for the ones already
    /// running to finish.
    ///
    /// Writes are not blocked, so they keep filling memtables that cannot be
    /// flushed. Enough of them and the DB stalls or hits the stop trigger, so pair
    /// this with [`continue_background_work`](Self::continue_background_work) and
    /// keep the gap short.
    ///
    /// Calls nest. Background work resumes once as many `continue` calls have been
    /// made as `pause` calls.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the DB is shutting down.
    pub fn pause_background_work(&self) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_pause_background_work(self.inner.inner()));
        }
        Ok(())
    }

    /// Undoes one [`pause_background_work`](Self::pause_background_work) call.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if background work was not paused, or if the DB is
    /// shutting down.
    pub fn continue_background_work(&self) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_continue_background_work(self.inner.inner()));
        }
        Ok(())
    }

    /// Undoes one [`disable_manual_compaction`](Self::disable_manual_compaction) call.
    ///
    /// This is a counter, so it takes as many calls here as there were calls there
    /// before manual compactions start running again.
    ///
    /// Pair the two. RocksDB decrements without a floor, and the assertion that would
    /// catch it is compiled out of this build, so calling this more often than
    /// `disable_manual_compaction` drives the counter below zero. That reads as
    /// not paused, which looks fine, but it means the next
    /// `disable_manual_compaction` only brings the counter back to zero and does not
    /// actually pause anything.
    pub fn enable_manual_compaction(&self) {
        unsafe { ffi::rocksdb_enable_manual_compaction(self.inner.inner()) }
    }

    /// Cancels running manual compactions and makes later ones return immediately.
    ///
    /// Affects only manual compactions, so [`compact_range`](Self::compact_range),
    /// [`compact_files`](Self::compact_files) and the like. Automatic background
    /// compaction keeps going. Use it to get out of a long manual compaction during
    /// shutdown without waiting for it.
    ///
    /// This increments a counter, so it needs a matching
    /// [`enable_manual_compaction`](Self::enable_manual_compaction) call for each
    /// call here.
    pub fn disable_manual_compaction(&self) {
        unsafe { ffi::rocksdb_disable_manual_compaction(self.inner.inner()) }
    }

    /// Reads every live SST and blob file and checks its block checksums.
    ///
    /// Reads the whole DB, so it is as slow as the data is large.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error naming the first corrupt file found.
    pub fn verify_checksum(&self) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_verify_checksum(self.inner.inner()));
        }
        Ok(())
    }

    /// Like [`verify_checksum`](Self::verify_checksum), reading through `readopts`.
    ///
    /// Worth setting when the default read path is not what you want for a full
    /// scan, for instance to keep the verification from filling the block cache or
    /// to give it a rate limiter priority.
    ///
    /// # Errors
    ///
    /// See [`verify_checksum`](Self::verify_checksum).
    pub fn verify_checksum_opt(&self, readopts: &ReadOptions) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_verify_checksum_with_options(
                self.inner.inner(),
                readopts.inner
            ));
        }
        Ok(())
    }

    /// Recomputes each live file's whole file checksum and compares it against the
    /// one recorded in the manifest.
    ///
    /// This is the file level check, as opposed to the per block one
    /// [`verify_checksum`](Self::verify_checksum) does. It requires the DB to have
    /// been written with a file checksum generator, see
    /// [`Options::set_file_checksum_gen_factory`](crate::Options::set_file_checksum_gen_factory).
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error naming the first file whose checksum does not match
    /// what the manifest recorded. Also errors when no generator is configured, since
    /// then there is nothing recorded to compare against, rather than treating that
    /// as having nothing to check.
    pub fn verify_file_checksums(&self) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_verify_file_checksums(self.inner.inner()));
        }
        Ok(())
    }

    /// Like [`verify_file_checksums`](Self::verify_file_checksums), reading through
    /// `readopts`.
    ///
    /// # Errors
    ///
    /// See [`verify_file_checksums`](Self::verify_file_checksums).
    pub fn verify_file_checksums_opt(&self, readopts: &ReadOptions) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_verify_file_checksums_with_options(
                self.inner.inner(),
                readopts.inner
            ));
        }
        Ok(())
    }

    /// Marks the files overlapping `[start, end)` for compaction and returns
    /// without compacting anything.
    ///
    /// Background compaction picks the marked files up on its own schedule, so this
    /// returns as soon as the marking is done. That makes it the cheap way to hint
    /// that a range is worth compacting, as opposed to
    /// [`compact_range`](Self::compact_range), which does the work before it
    /// returns.
    ///
    /// `None` for either bound means unbounded in that direction. An empty slice is
    /// a real empty key, not the same thing.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the range cannot be marked.
    pub fn suggest_compact_range<S: AsRef<[u8]>, E: AsRef<[u8]>>(
        &self,
        start: Option<S>,
        end: Option<E>,
    ) -> Result<(), Error> {
        let (start, start_len) = optional_key_parts(start.as_ref());
        let (end, end_len) = optional_key_parts(end.as_ref());
        unsafe {
            ffi_try!(ffi::rocksdb_suggest_compact_range(
                self.inner.inner(),
                start,
                start_len,
                end,
                end_len
            ));
        }
        Ok(())
    }

    /// Like [`suggest_compact_range`](Self::suggest_compact_range), for a single
    /// column family.
    ///
    /// # Errors
    ///
    /// See [`suggest_compact_range`](Self::suggest_compact_range).
    pub fn suggest_compact_range_cf<S: AsRef<[u8]>, E: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        start: Option<S>,
        end: Option<E>,
    ) -> Result<(), Error> {
        let (start, start_len) = optional_key_parts(start.as_ref());
        let (end, end_len) = optional_key_parts(end.as_ref());
        unsafe {
            ffi_try!(ffi::rocksdb_suggest_compact_range_cf(
                self.inner.inner(),
                cf.inner(),
                start,
                start_len,
                end,
                end_len
            ));
        }
        Ok(())
    }

    /// Reads a key along with the user-defined timestamp its value was written with.
    ///
    /// Only meaningful on a column family configured for user-defined timestamps, so
    /// one whose comparator carries a timestamp size. The plain
    /// [`get`](Self::get) reads the same value but throws the timestamp away, and
    /// there is no pinned equivalent of this call in the C API.
    ///
    /// `readopts` decides which timestamp is read. Without a read timestamp set on
    /// it RocksDB rejects the read rather than picking one, see
    /// [`ReadOptions::set_timestamp`](crate::ReadOptions::set_timestamp).
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the read fails. A key that is not present is
    /// `Ok(None)`, not an error.
    pub fn get_with_ts<K: AsRef<[u8]>>(
        &self,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<TimestampedValue>, Error> {
        let key = key.as_ref();
        self.get_with_ts_impl(readopts, |db, ro, vallen, ts, tslen, err| unsafe {
            ffi::rocksdb_get_with_ts(
                db,
                ro,
                key.as_ptr().cast::<c_char>(),
                key.len(),
                vallen,
                ts,
                tslen,
                err,
            )
        })
    }

    /// Like [`get_with_ts`](Self::get_with_ts), for a single column family.
    ///
    /// # Errors
    ///
    /// See [`get_with_ts`](Self::get_with_ts).
    pub fn get_cf_with_ts<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<TimestampedValue>, Error> {
        let key = key.as_ref();
        let cf = cf.inner();
        self.get_with_ts_impl(readopts, |db, ro, vallen, ts, tslen, err| unsafe {
            ffi::rocksdb_get_cf_with_ts(
                db,
                ro,
                cf,
                key.as_ptr().cast::<c_char>(),
                key.len(),
                vallen,
                ts,
                tslen,
                err,
            )
        })
    }

    /// Shared tail of the two timestamped gets.
    ///
    /// `call` is handed the out-params and returns the value pointer. Note that
    /// RocksDB only writes through `ts` when the read succeeds, so `ts` starts as
    /// null here and a miss leaves it that way rather than leaving it indeterminate
    /// (c.cc:2592-2603).
    fn get_with_ts_impl(
        &self,
        readopts: &ReadOptions,
        call: impl FnOnce(
            *mut ffi::rocksdb_t,
            *const ffi::rocksdb_readoptions_t,
            *mut usize,
            *mut *mut c_char,
            *mut usize,
            *mut *mut c_char,
        ) -> *mut c_char,
    ) -> Result<Option<TimestampedValue>, Error> {
        let mut vallen: usize = 0;
        let mut ts: *mut c_char = ptr::null_mut();
        let mut tslen: usize = 0;
        let mut err: *mut c_char = ptr::null_mut();

        let value = call(
            self.inner.inner(),
            readopts.inner,
            &raw mut vallen,
            &raw mut ts,
            &raw mut tslen,
            &raw mut err,
        );

        if !err.is_null() {
            // RocksDB reports a failure without allocating either output, but free
            // anything it did hand back rather than trusting that on an error path.
            unsafe {
                if !value.is_null() {
                    ffi::rocksdb_free(value.cast::<c_void>());
                }
                if !ts.is_null() {
                    ffi::rocksdb_free(ts.cast::<c_void>());
                }
            }
            return Err(convert_rocksdb_error(err));
        }

        if value.is_null() {
            return Ok(None);
        }

        // SAFETY: both pointers came from RocksDB's `CopyString`, so they are
        // `malloc`ed buffers of the reported length that nothing else frees, which is
        // exactly what `CSlice` takes over.
        unsafe {
            Ok(Some(TimestampedValue {
                value: CSlice::from_raw_parts(value, vallen),
                timestamp: CSlice::from_raw_parts(ts, tslen),
            }))
        }
    }

    /// Reads many keys along with the timestamps their values were written with.
    ///
    /// One native batch, results in input order, one `Result` per key so a single
    /// bad key does not sink the batch. See [`get_with_ts`](Self::get_with_ts) for
    /// what the timestamp means and what `readopts` has to carry.
    pub fn multi_get_with_ts<K, I>(
        &self,
        keys: I,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<TimestampedValue>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        let owned_keys: Vec<K> = keys.into_iter().collect();
        let (ptr_keys, keys_sizes) = key_ptrs_and_sizes(&owned_keys);
        let mut out = MultiGetTsOut::with_capacity(ptr_keys.len());

        unsafe {
            ffi::rocksdb_multi_get_with_ts(
                self.inner.inner(),
                readopts.inner,
                ptr_keys.len(),
                ptr_keys.as_ptr(),
                keys_sizes.as_ptr(),
                out.values.as_mut_ptr(),
                out.values_sizes.as_mut_ptr(),
                out.timestamps.as_mut_ptr(),
                out.timestamps_sizes.as_mut_ptr(),
                out.errors.as_mut_ptr(),
            );
            out.assume_filled(ptr_keys.len());
        }

        out.into_results()
    }

    /// Like [`multi_get_with_ts`](Self::multi_get_with_ts), for one column family per
    /// key.
    pub fn multi_get_cf_with_ts<'c, K, I, W>(
        &self,
        keys: I,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<TimestampedValue>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = (&'c W, K)>,
        W: AsColumnFamilyRef + 'c,
    {
        let (cfs, owned_keys): (Vec<_>, Vec<K>) = keys.into_iter().unzip();
        let cf_ptrs: Vec<*const ffi::rocksdb_column_family_handle_t> =
            cfs.iter().map(|cf| cf.inner().cast_const()).collect();
        let (ptr_keys, keys_sizes) = key_ptrs_and_sizes(&owned_keys);
        let mut out = MultiGetTsOut::with_capacity(ptr_keys.len());

        unsafe {
            ffi::rocksdb_multi_get_cf_with_ts(
                self.inner.inner(),
                readopts.inner,
                cf_ptrs.as_ptr(),
                ptr_keys.len(),
                ptr_keys.as_ptr(),
                keys_sizes.as_ptr(),
                out.values.as_mut_ptr(),
                out.values_sizes.as_mut_ptr(),
                out.timestamps.as_mut_ptr(),
                out.timestamps_sizes.as_mut_ptr(),
                out.errors.as_mut_ptr(),
            );
            out.assume_filled(ptr_keys.len());
        }

        out.into_results()
    }

    /// Changes DB wide mutable options at runtime.
    ///
    /// Takes the same option names and string values the RocksDB configuration
    /// strings use, so `max_background_jobs` or `bytes_per_sync`. Only options
    /// RocksDB marks mutable at the DB level can be set, and the whole call is
    /// rejected if any name or value is not accepted.
    ///
    /// [`set_options`](Self::set_options) is the column family equivalent.
    ///
    /// # Aborts
    ///
    /// Some unparseable values take the process down instead of returning an error.
    /// See [`set_options`](Self::set_options) for the details and why this is not
    /// caught.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if a name is unknown or the option is not
    /// changeable at runtime, and for an empty `opts`, which RocksDB rejects as
    /// `empty input`. Also errors if any name or value contains an interior NUL
    /// byte.
    pub fn set_db_options(&self, opts: &[(&str, &str)]) -> Result<(), Error> {
        let copts = convert_options(opts)?;
        let names: Vec<*const c_char> = copts.iter().map(|(n, _)| n.as_ptr()).collect();
        let values: Vec<*const c_char> = copts.iter().map(|(_, v)| v.as_ptr()).collect();

        unsafe {
            ffi_try!(ffi::rocksdb_set_db_options(
                self.inner.inner(),
                option_count(&copts)?,
                names.as_ptr(),
                values.as_ptr(),
            ));
        }
        Ok(())
    }

    /// Suspend deleting obsolete files. Compactions will continue to occur,
    /// but no obsolete files will be deleted. To resume file deletions, each
    /// call to disable_file_deletions() must be matched by a subsequent call to
    /// enable_file_deletions(). For more details, see enable_file_deletions().
    pub fn disable_file_deletions(&self) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_disable_file_deletions(self.inner.inner()));
        }
        Ok(())
    }

    /// Resume deleting obsolete files, following up on `disable_file_deletions()`.
    ///
    /// File deletions disabling and enabling is not controlled by a binary flag,
    /// instead it's represented as a counter to allow different callers to
    /// independently disable file deletion. Disabling file deletion can be
    /// critical for operations like making a backup. So the counter implementation
    /// makes the file deletion disabled as long as there is one caller requesting
    /// so, and only when every caller agrees to re-enable file deletion, it will
    /// be enabled. Two threads can call this method concurrently without
    /// synchronization -- i.e., file deletions will be enabled only after both
    /// threads call enable_file_deletions()
    pub fn enable_file_deletions(&self) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_enable_file_deletions(self.inner.inner()));
        }
        Ok(())
    }

    /// Flushes database memtables to SST files on the disk.
    pub fn flush_opt(&self, flushopts: &FlushOptions) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_flush(self.inner.inner(), flushopts.inner));
        }
        Ok(())
    }

    /// Flushes database memtables to SST files on the disk using default options.
    pub fn flush(&self) -> Result<(), Error> {
        DEFAULT_FLUSH_OPTS.with(|opts| self.flush_opt(opts))
    }

    /// Flushes database memtables to SST files on the disk for a given column family.
    pub fn flush_cf_opt(
        &self,
        cf: &impl AsColumnFamilyRef,
        flushopts: &FlushOptions,
    ) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_flush_cf(
                self.inner.inner(),
                flushopts.inner,
                cf.inner()
            ));
        }
        Ok(())
    }

    /// Flushes multiple column families.
    ///
    /// If atomic flush is not enabled, it is equivalent to calling flush_cf multiple times.
    /// If atomic flush is enabled, it will flush all column families specified in `cfs` up to the latest sequence
    /// number at the time when flush is requested.
    pub fn flush_cfs_opt(
        &self,
        cfs: &[&impl AsColumnFamilyRef],
        opts: &FlushOptions,
    ) -> Result<(), Error> {
        let mut cfs = cfs.iter().map(|cf| cf.inner()).collect::<Vec<_>>();
        unsafe {
            ffi_try!(ffi::rocksdb_flush_cfs(
                self.inner.inner(),
                opts.inner,
                cfs.as_mut_ptr(),
                cfs.len() as libc::c_int,
            ));
        }
        Ok(())
    }

    /// Flushes database memtables to SST files on the disk for a given column family using default
    /// options.
    pub fn flush_cf(&self, cf: &impl AsColumnFamilyRef) -> Result<(), Error> {
        DEFAULT_FLUSH_OPTS.with(|opts| self.flush_cf_opt(cf, opts))
    }

    /// Return the bytes associated with a key value with read options. If you only intend to use
    /// the vector returned temporarily, consider using [`get_pinned_opt`](#method.get_pinned_opt)
    /// to avoid unnecessary memory copy.
    pub fn get_opt<K: AsRef<[u8]>>(
        &self,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<Vec<u8>>, Error> {
        self.get_pinned_opt(key, readopts)
            .map(|x| x.map(|v| v.as_ref().to_vec()))
    }

    /// Return the bytes associated with a key value. If you only intend to use the vector returned
    /// temporarily, consider using [`get_pinned`](#method.get_pinned) to avoid unnecessary memory
    /// copy.
    pub fn get<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Error> {
        DEFAULT_READ_OPTS.with(|opts| self.get_opt(key.as_ref(), opts))
    }

    /// Return the bytes associated with a key value and the given column family with read options.
    /// If you only intend to use the vector returned temporarily, consider using
    /// [`get_pinned_cf_opt`](#method.get_pinned_cf_opt) to avoid unnecessary memory.
    pub fn get_cf_opt<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<Vec<u8>>, Error> {
        self.get_pinned_cf_opt(cf, key, readopts)
            .map(|x| x.map(|v| v.as_ref().to_vec()))
    }

    /// Return the bytes associated with a key value and the given column family. If you only
    /// intend to use the vector returned temporarily, consider using
    /// [`get_pinned_cf`](#method.get_pinned_cf) to avoid unnecessary memory.
    pub fn get_cf<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
    ) -> Result<Option<Vec<u8>>, Error> {
        DEFAULT_READ_OPTS.with(|opts| self.get_cf_opt(cf, key.as_ref(), opts))
    }

    /// Return the value associated with a key using RocksDB's PinnableSlice
    /// so as to avoid unnecessary memory copy.
    pub fn get_pinned_opt<K: AsRef<[u8]>>(
        &'_ self,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<DBPinnableSlice<'_>>, Error> {
        if readopts.inner.is_null() {
            return Err(Error::new(
                "Unable to create RocksDB read options. This is a fairly trivial call, and its \
                 failure may be indicative of a mis-compiled or mis-loaded RocksDB library."
                    .to_owned(),
            ));
        }

        let key = key.as_ref();
        unsafe {
            let val = ffi_try!(ffi::rocksdb_get_pinned(
                self.inner.inner(),
                readopts.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
            ));
            if val.is_null() {
                Ok(None)
            } else {
                Ok(Some(DBPinnableSlice::from_c(val)))
            }
        }
    }

    /// Return the value associated with a key using RocksDB's PinnableSlice
    /// so as to avoid unnecessary memory copy. Similar to get_pinned_opt but
    /// leverages default options.
    pub fn get_pinned<K: AsRef<[u8]>>(
        &'_ self,
        key: K,
    ) -> Result<Option<DBPinnableSlice<'_>>, Error> {
        DEFAULT_READ_OPTS.with(|opts| self.get_pinned_opt(key, opts))
    }

    /// Return the value associated with a key using RocksDB's PinnableSlice
    /// so as to avoid unnecessary memory copy. Similar to get_pinned_opt but
    /// allows specifying ColumnFamily
    pub fn get_pinned_cf_opt<K: AsRef<[u8]>>(
        &'_ self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: &ReadOptions,
    ) -> Result<Option<DBPinnableSlice<'_>>, Error> {
        if readopts.inner.is_null() {
            return Err(Error::new(
                "Unable to create RocksDB read options. This is a fairly trivial call, and its \
                 failure may be indicative of a mis-compiled or mis-loaded RocksDB library."
                    .to_owned(),
            ));
        }

        let key = key.as_ref();
        unsafe {
            let val = ffi_try!(ffi::rocksdb_get_pinned_cf(
                self.inner.inner(),
                readopts.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
            ));
            if val.is_null() {
                Ok(None)
            } else {
                Ok(Some(DBPinnableSlice::from_c(val)))
            }
        }
    }

    /// Return the value associated with a key using RocksDB's PinnableSlice
    /// so as to avoid unnecessary memory copy. Similar to get_pinned_cf_opt but
    /// leverages default options.
    pub fn get_pinned_cf<K: AsRef<[u8]>>(
        &'_ self,
        cf: &impl AsColumnFamilyRef,
        key: K,
    ) -> Result<Option<DBPinnableSlice<'_>>, Error> {
        DEFAULT_READ_OPTS.with(|opts| self.get_pinned_cf_opt(cf, key, opts))
    }

    /// Read a value directly into a caller-provided buffer, avoiding memory allocation.
    ///
    /// This is the most efficient way to read values when you have a pre-allocated
    /// buffer. It completely avoids the allocation overhead of [`get`](#method.get)
    /// and even the pinning overhead of [`get_pinned`](#method.get_pinned).
    ///
    /// # Arguments
    ///
    /// * `key` - The key to look up
    /// * `buffer` - A mutable byte slice to write the value into. Can be empty if you
    ///   only want to check if a key exists and get its value size.
    ///
    /// # Returns
    ///
    /// * `Ok(GetIntoBufferResult::NotFound)` - The key doesn't exist
    /// * `Ok(GetIntoBufferResult::Found(size))` - Value was copied into the buffer.
    ///   `size` is the number of bytes written.
    /// * `Ok(GetIntoBufferResult::BufferTooSmall(size))` - The value exists but the buffer
    ///   is too small. `size` is the actual value size needed. No data is written.
    /// * `Err(...)` - Database error occurred
    ///
    /// # Performance
    ///
    /// This method is ideal for high-throughput scenarios where you can reuse a buffer:
    ///
    /// ```ignore
    /// use rust_rocksdb::{DB, GetIntoBufferResult};
    ///
    /// let db: DB = /* open database */;
    /// let keys_to_lookup: Vec<&[u8]> = /* keys to look up */;
    /// let mut buffer = vec![0u8; 4096]; // Reusable buffer
    ///
    /// for key in keys_to_lookup {
    ///     match db.get_into_buffer(key, &mut buffer).unwrap() {
    ///         GetIntoBufferResult::Found(len) => {
    ///             process_value(&buffer[..len]);
    ///         }
    ///         GetIntoBufferResult::BufferTooSmall(needed) => {
    ///             buffer.resize(needed, 0);
    ///             // Retry with larger buffer...
    ///         }
    ///         GetIntoBufferResult::NotFound => {}
    ///     }
    /// }
    /// ```
    ///
    /// # Example
    ///
    /// ```
    /// use rust_rocksdb::{DB, GetIntoBufferResult};
    ///
    /// let tempdir = tempfile::Builder::new()
    ///     .prefix("rocksdb_get_into_buffer")
    ///     .tempdir()
    ///     .unwrap();
    /// let db = DB::open_default(tempdir.path()).unwrap();
    /// db.put(b"key", b"value").unwrap();
    ///
    /// let mut buffer = [0u8; 100];
    /// match db.get_into_buffer(b"key", &mut buffer).unwrap() {
    ///     GetIntoBufferResult::Found(size) => {
    ///         assert_eq!(&buffer[..size], b"value");
    ///     }
    ///     GetIntoBufferResult::NotFound => panic!("expected value"),
    ///     GetIntoBufferResult::BufferTooSmall(needed) => {
    ///         panic!("buffer too small, need {} bytes", needed)
    ///     }
    /// }
    /// ```
    pub fn get_into_buffer<K: AsRef<[u8]>>(
        &self,
        key: K,
        buffer: &mut [u8],
    ) -> Result<GetIntoBufferResult, Error> {
        DEFAULT_READ_OPTS.with(|opts| self.get_into_buffer_opt(key, buffer, opts))
    }

    /// Read a value directly into a caller-provided buffer with custom read options.
    ///
    /// This is the same as [`get_into_buffer`](#method.get_into_buffer) but allows
    /// specifying custom [`ReadOptions`], such as setting a snapshot or fill cache behavior.
    ///
    /// See [`get_into_buffer`](#method.get_into_buffer) for full documentation.
    pub fn get_into_buffer_opt<K: AsRef<[u8]>>(
        &self,
        key: K,
        buffer: &mut [u8],
        readopts: &ReadOptions,
    ) -> Result<GetIntoBufferResult, Error> {
        if readopts.inner.is_null() {
            return Err(Error::new(
                "Unable to create RocksDB read options. This is a fairly trivial call, and its \
                 failure may be indicative of a mis-compiled or mis-loaded RocksDB library."
                    .to_owned(),
            ));
        }

        let key = key.as_ref();
        let mut val_len: size_t = 0;
        let mut found: c_uchar = 0;

        unsafe {
            let success = ffi_try!(ffi::rocksdb_get_into_buffer(
                self.inner.inner(),
                readopts.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                buffer.as_mut_ptr() as *mut c_char,
                buffer.len() as size_t,
                &raw mut val_len,
                &raw mut found,
            ));

            if found == 0 {
                Ok(GetIntoBufferResult::NotFound)
            } else if success != 0 {
                Ok(GetIntoBufferResult::Found(val_len))
            } else {
                Ok(GetIntoBufferResult::BufferTooSmall(val_len))
            }
        }
    }

    /// Read a value from a column family directly into a caller-provided buffer.
    ///
    /// This is the column family variant of [`get_into_buffer`](#method.get_into_buffer).
    /// See that method for full documentation on the zero-allocation buffer API.
    ///
    /// # Arguments
    ///
    /// * `cf` - The column family to read from
    /// * `key` - The key to look up
    /// * `buffer` - A mutable byte slice to write the value into
    pub fn get_into_buffer_cf<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        buffer: &mut [u8],
    ) -> Result<GetIntoBufferResult, Error> {
        DEFAULT_READ_OPTS.with(|opts| self.get_into_buffer_cf_opt(cf, key, buffer, opts))
    }

    /// Read a value from a column family directly into a caller-provided buffer
    /// with custom read options.
    ///
    /// This is the column family variant of [`get_into_buffer_opt`](#method.get_into_buffer_opt).
    /// See [`get_into_buffer`](#method.get_into_buffer) for full documentation.
    pub fn get_into_buffer_cf_opt<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        buffer: &mut [u8],
        readopts: &ReadOptions,
    ) -> Result<GetIntoBufferResult, Error> {
        if readopts.inner.is_null() {
            return Err(Error::new(
                "Unable to create RocksDB read options. This is a fairly trivial call, and its \
                 failure may be indicative of a mis-compiled or mis-loaded RocksDB library."
                    .to_owned(),
            ));
        }

        let key = key.as_ref();
        let mut val_len: size_t = 0;
        let mut found: c_uchar = 0;

        unsafe {
            let success = ffi_try!(ffi::rocksdb_get_into_buffer_cf(
                self.inner.inner(),
                readopts.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                buffer.as_mut_ptr() as *mut c_char,
                buffer.len() as size_t,
                &raw mut val_len,
                &raw mut found,
            ));

            if found == 0 {
                Ok(GetIntoBufferResult::NotFound)
            } else if success != 0 {
                Ok(GetIntoBufferResult::Found(val_len))
            } else {
                Ok(GetIntoBufferResult::BufferTooSmall(val_len))
            }
        }
    }

    /// Return the values associated with the given keys.
    pub fn multi_get<K, I>(&self, keys: I) -> Vec<Result<Option<Vec<u8>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        DEFAULT_READ_OPTS.with(|opts| self.multi_get_opt(keys, opts))
    }

    /// Return the values associated with the given keys using read options.
    pub fn multi_get_opt<K, I>(
        &self,
        keys: I,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<Vec<u8>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        let owned_keys: Vec<K> = keys.into_iter().collect();
        let (ptr_keys, keys_sizes): (Vec<*const c_char>, Vec<usize>) = owned_keys
            .iter()
            .map(|k| {
                let key = k.as_ref();
                (key.as_ptr() as *const c_char, key.len())
            })
            .unzip();

        let mut values: Vec<*mut c_char> = Vec::with_capacity(ptr_keys.len());
        let mut values_sizes: Vec<usize> = Vec::with_capacity(ptr_keys.len());
        let mut errors: Vec<*mut c_char> = Vec::with_capacity(ptr_keys.len());
        unsafe {
            ffi::rocksdb_multi_get(
                self.inner.inner(),
                readopts.inner,
                ptr_keys.len(),
                ptr_keys.as_ptr(),
                keys_sizes.as_ptr(),
                values.as_mut_ptr(),
                values_sizes.as_mut_ptr(),
                errors.as_mut_ptr(),
            );
        }

        unsafe {
            values.set_len(ptr_keys.len());
            values_sizes.set_len(ptr_keys.len());
            errors.set_len(ptr_keys.len());
        }

        convert_values(values, values_sizes, errors)
    }

    /// Returns pinned values associated with the given keys using default read options.
    ///
    /// RocksDB processes the keys in one native batch. Results stay in input order.
    pub fn multi_get_pinned<K, I>(
        &'_ self,
        keys: I,
    ) -> Vec<Result<Option<DBPinnableSlice<'_>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        DEFAULT_READ_OPTS.with(|opts| self.multi_get_pinned_opt(keys, opts))
    }

    /// Returns pinned values associated with the given keys using the provided read options.
    ///
    /// RocksDB processes the keys in one native batch. Results stay in input order.
    pub fn multi_get_pinned_opt<K, I>(
        &'_ self,
        keys: I,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<DBPinnableSlice<'_>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        let mut keys = keys.into_iter();
        let Some(first) = keys.next() else {
            return Vec::new();
        };
        // Decide before collecting. A single key does not benefit from the
        // native batch, and buying a key-slice vector, two result vectors and
        // a default column family handle to do one point lookup is a loss.
        let Some(second) = keys.next() else {
            return vec![self.get_pinned_opt(first.as_ref(), readopts)];
        };
        let mut owned_keys = Vec::with_capacity(2 + keys.size_hint().0);
        owned_keys.push(first);
        owned_keys.push(second);
        owned_keys.extend(keys);
        self.batched_multi_get_pinned_owned(&owned_keys, false, readopts)
    }

    /// Returns pinned values associated with the given keys and column families
    /// using default read options.
    pub fn multi_get_pinned_cf<'a, 'b: 'a, K, I, W>(
        &'a self,
        keys: I,
    ) -> Vec<Result<Option<DBPinnableSlice<'a>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = (&'b W, K)>,
        W: 'b + AsColumnFamilyRef,
    {
        DEFAULT_READ_OPTS.with(|opts| self.multi_get_pinned_cf_opt(keys, opts))
    }

    /// Returns pinned values associated with the given keys and column families
    /// using the provided read options.
    pub fn multi_get_pinned_cf_opt<'a, 'b: 'a, K, I, W>(
        &'a self,
        keys: I,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<DBPinnableSlice<'a>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = (&'b W, K)>,
        W: 'b + AsColumnFamilyRef,
    {
        keys.into_iter()
            .map(|(cf, k)| self.get_pinned_cf_opt(cf, k, readopts))
            .collect()
    }

    /// Returns pinned values for default-column-family keys in one native batch.
    ///
    /// Set `sorted_input` only when keys are sorted according to the column
    /// family's comparator. Results stay in input order, including duplicates.
    pub fn batched_multi_get_pinned<K, I>(
        &'_ self,
        keys: I,
        sorted_input: bool,
    ) -> Vec<Result<Option<DBPinnableSlice<'_>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        DEFAULT_READ_OPTS.with(|opts| self.batched_multi_get_pinned_opt(keys, sorted_input, opts))
    }

    /// Returns pinned values for default-column-family keys in one native batch
    /// using the provided read options.
    pub fn batched_multi_get_pinned_opt<K, I>(
        &'_ self,
        keys: I,
        sorted_input: bool,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<DBPinnableSlice<'_>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        let owned_keys: Vec<K> = keys.into_iter().collect();
        self.batched_multi_get_pinned_owned(&owned_keys, sorted_input, readopts)
    }

    /// Returns pinned values for keys in one column family using one native batch.
    ///
    /// Set `sorted_input` only when keys are sorted according to the column
    /// family's comparator. Results stay in input order, including duplicates.
    pub fn batched_multi_get_pinned_cf<K, I>(
        &'_ self,
        cf: &impl AsColumnFamilyRef,
        keys: I,
        sorted_input: bool,
    ) -> Vec<Result<Option<DBPinnableSlice<'_>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        DEFAULT_READ_OPTS
            .with(|opts| self.batched_multi_get_pinned_cf_opt(cf, keys, sorted_input, opts))
    }

    /// Returns pinned values for keys in one column family using one native batch
    /// and the provided read options.
    pub fn batched_multi_get_pinned_cf_opt<K, I>(
        &'_ self,
        cf: &impl AsColumnFamilyRef,
        keys: I,
        sorted_input: bool,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<DBPinnableSlice<'_>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        let owned_keys: Vec<K> = keys.into_iter().collect();
        let key_slices = Self::key_slices(&owned_keys);
        self.batched_multi_get_pinned_inner(cf.inner(), &key_slices, sorted_input, readopts)
    }

    /// Returns one owner for all default-column-family pinned results.
    ///
    /// Values borrow from the returned batch, avoiding one native wrapper
    /// allocation and one destroy call per successful key.
    pub fn batched_multi_get_pinned_batch<K, I>(
        &'_ self,
        keys: I,
        sorted_input: bool,
    ) -> Result<DBPinnableBatch<'_>, Error>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        DEFAULT_READ_OPTS
            .with(|opts| self.batched_multi_get_pinned_batch_opt(keys, sorted_input, opts))
    }

    /// Returns one owner for all default-column-family pinned results using
    /// the provided read options.
    pub fn batched_multi_get_pinned_batch_opt<K, I>(
        &'_ self,
        keys: I,
        sorted_input: bool,
        readopts: &ReadOptions,
    ) -> Result<DBPinnableBatch<'_>, Error>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        let owned_keys: Vec<K> = keys.into_iter().collect();
        let key_slices = Self::key_slices(&owned_keys);
        self.create_pinnable_batch(ptr::null_mut(), &key_slices, sorted_input, readopts)
    }

    /// Returns one owner for all pinned results from one column family.
    pub fn batched_multi_get_pinned_batch_cf<K, I>(
        &'_ self,
        cf: &impl AsColumnFamilyRef,
        keys: I,
        sorted_input: bool,
    ) -> Result<DBPinnableBatch<'_>, Error>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        DEFAULT_READ_OPTS
            .with(|opts| self.batched_multi_get_pinned_batch_cf_opt(cf, keys, sorted_input, opts))
    }

    /// Returns one owner for all pinned results from one column family using
    /// the provided read options.
    pub fn batched_multi_get_pinned_batch_cf_opt<K, I>(
        &'_ self,
        cf: &impl AsColumnFamilyRef,
        keys: I,
        sorted_input: bool,
        readopts: &ReadOptions,
    ) -> Result<DBPinnableBatch<'_>, Error>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        let owned_keys: Vec<K> = keys.into_iter().collect();
        let key_slices = Self::key_slices(&owned_keys);
        self.create_pinnable_batch(cf.inner(), &key_slices, sorted_input, readopts)
    }

    /// Return the values associated with the given keys and column families.
    pub fn multi_get_cf<'a, 'b: 'a, K, I, W>(
        &'a self,
        keys: I,
    ) -> Vec<Result<Option<Vec<u8>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = (&'b W, K)>,
        W: 'b + AsColumnFamilyRef,
    {
        DEFAULT_READ_OPTS.with(|opts| self.multi_get_cf_opt(keys, opts))
    }

    /// Return the values associated with the given keys and column families using read options.
    pub fn multi_get_cf_opt<'a, 'b: 'a, K, I, W>(
        &'a self,
        keys: I,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<Vec<u8>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = (&'b W, K)>,
        W: 'b + AsColumnFamilyRef,
    {
        let cfs_and_owned_keys: Vec<(&'b W, K)> = keys.into_iter().collect();
        let (ptr_keys, keys_sizes): (Vec<*const c_char>, Vec<usize>) = cfs_and_owned_keys
            .iter()
            .map(|(_, k)| {
                let key = k.as_ref();
                (key.as_ptr() as *const c_char, key.len())
            })
            .unzip();
        let ptr_cfs: Vec<*const ffi::rocksdb_column_family_handle_t> = cfs_and_owned_keys
            .iter()
            .map(|(c, _)| c.inner().cast_const())
            .collect();
        let mut values: Vec<*mut c_char> = Vec::with_capacity(ptr_keys.len());
        let mut values_sizes: Vec<usize> = Vec::with_capacity(ptr_keys.len());
        let mut errors: Vec<*mut c_char> = Vec::with_capacity(ptr_keys.len());
        unsafe {
            ffi::rocksdb_multi_get_cf(
                self.inner.inner(),
                readopts.inner,
                ptr_cfs.as_ptr(),
                ptr_keys.len(),
                ptr_keys.as_ptr(),
                keys_sizes.as_ptr(),
                values.as_mut_ptr(),
                values_sizes.as_mut_ptr(),
                errors.as_mut_ptr(),
            );
        }

        unsafe {
            values.set_len(ptr_keys.len());
            values_sizes.set_len(ptr_keys.len());
            errors.set_len(ptr_keys.len());
        }

        convert_values(values, values_sizes, errors)
    }

    /// Return the values associated with the given keys and the specified column family
    /// where internally the read requests are processed in batch if block-based table
    /// SST format is used.  It is a more optimized version of multi_get_cf.
    pub fn batched_multi_get_cf<'a, K, I>(
        &'_ self,
        cf: &impl AsColumnFamilyRef,
        keys: I,
        sorted_input: bool,
    ) -> Vec<Result<Option<DBPinnableSlice<'_>>, Error>>
    where
        K: AsRef<[u8]> + 'a + ?Sized,
        I: IntoIterator<Item = &'a K>,
    {
        DEFAULT_READ_OPTS.with(|opts| self.batched_multi_get_cf_opt(cf, keys, sorted_input, opts))
    }

    /// Return the values associated with the given keys and the specified column family
    /// where internally the read requests are processed in batch if block-based table
    /// SST format is used. It is a more optimized version of multi_get_cf_opt.
    pub fn batched_multi_get_cf_opt<'a, K, I>(
        &'_ self,
        cf: &impl AsColumnFamilyRef,
        keys: I,
        sorted_input: bool,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<DBPinnableSlice<'_>>, Error>>
    where
        K: AsRef<[u8]> + 'a + ?Sized,
        I: IntoIterator<Item = &'a K>,
    {
        let key_slices: Vec<_> = keys
            .into_iter()
            .map(|k| {
                let k = k.as_ref();
                ffi::rocksdb_slice_t {
                    data: k.as_ptr() as *const c_char,
                    size: k.len(),
                }
            })
            .collect();
        self.batched_multi_get_pinned_inner(cf.inner(), &key_slices, sorted_input, readopts)
    }

    /// Return the values associated with the given keys and the specified column family
    /// using an optimized slice-based API.
    ///
    /// This method uses RocksDB's optimized `rocksdb_batched_multi_get_cf_slice` C API,
    /// which takes a `rocksdb_slice_t` array directly. This eliminates the internal
    /// overhead of converting keys from separate pointer+size arrays to Slice objects.
    ///
    /// # Arguments
    ///
    /// * `cf` - The column family to read from
    /// * `keys` - An iterator of keys to look up
    /// * `sorted_input` - If `true`, indicates the keys are already sorted in ascending
    ///   order, which allows RocksDB to skip internal sorting and improve performance.
    ///   **Important**: If you pass `true` but keys are not sorted, results may be incorrect.
    ///
    /// # Returns
    ///
    /// A vector of results in the same order as the input keys. Each element is:
    /// - `Ok(Some(DBPinnableSlice))` if the key was found
    /// - `Ok(None)` if the key was not found
    /// - `Err(...)` if an error occurred for that key
    ///
    /// # Performance
    ///
    /// This is the fastest batch lookup method when:
    /// - You're looking up many keys (10+) from the same column family
    /// - You can pre-sort your keys (set `sorted_input = true`)
    /// - Block-based table format is used (default)
    ///
    /// For small numbers of keys, the overhead of batching may not be worth it.
    /// Consider using [`get_pinned_cf`](#method.get_pinned_cf) for single key lookups.
    ///
    /// # Example
    ///
    /// ```
    /// use rust_rocksdb::{DB, Options, ColumnFamilyDescriptor};
    ///
    /// let tempdir = tempfile::Builder::new().prefix("batch_slice").tempdir().unwrap();
    /// let mut opts = Options::default();
    /// opts.create_if_missing(true);
    /// opts.create_missing_column_families(true);
    /// let db = DB::open_cf_descriptors(&opts, tempdir.path(),
    ///     vec![ColumnFamilyDescriptor::new("cf", Options::default())]).unwrap();
    ///
    /// let cf = db.cf_handle("cf").unwrap();
    /// db.put_cf(&cf, b"k1", b"v1").unwrap();
    /// db.put_cf(&cf, b"k2", b"v2").unwrap();
    ///
    /// // Keys are sorted, so we can set sorted_input = true
    /// let keys: Vec<&[u8]> = vec![b"k1", b"k2", b"k3"];
    /// let results = db.batched_multi_get_cf_slice(&cf, keys, true);
    ///
    /// assert!(results[0].as_ref().unwrap().is_some()); // k1 found
    /// assert!(results[1].as_ref().unwrap().is_some()); // k2 found
    /// assert!(results[2].as_ref().unwrap().is_none()); // k3 not found
    /// ```
    pub fn batched_multi_get_cf_slice<'a, K, I>(
        &'_ self,
        cf: &impl AsColumnFamilyRef,
        keys: I,
        sorted_input: bool,
    ) -> Vec<Result<Option<DBPinnableSlice<'_>>, Error>>
    where
        K: AsRef<[u8]> + 'a + ?Sized,
        I: IntoIterator<Item = &'a K>,
    {
        DEFAULT_READ_OPTS
            .with(|opts| self.batched_multi_get_cf_slice_opt(cf, keys, sorted_input, opts))
    }

    /// Return the values associated with the given keys and the specified column family
    /// using an optimized slice-based API with custom read options.
    ///
    /// This is the same as [`batched_multi_get_cf_slice`](#method.batched_multi_get_cf_slice)
    /// but allows specifying custom [`ReadOptions`].
    ///
    /// See [`batched_multi_get_cf_slice`](#method.batched_multi_get_cf_slice) for full documentation.
    pub fn batched_multi_get_cf_slice_opt<'a, K, I>(
        &'_ self,
        cf: &impl AsColumnFamilyRef,
        keys: I,
        sorted_input: bool,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<DBPinnableSlice<'_>>, Error>>
    where
        K: AsRef<[u8]> + 'a + ?Sized,
        I: IntoIterator<Item = &'a K>,
    {
        // Convert keys to rocksdb_slice_t array
        let slices: Vec<ffi::rocksdb_slice_t> = keys
            .into_iter()
            .map(|k| {
                let k = k.as_ref();
                ffi::rocksdb_slice_t {
                    data: k.as_ptr() as *const c_char,
                    size: k.len(),
                }
            })
            .collect();

        self.batched_multi_get_pinned_inner(cf.inner(), &slices, sorted_input, readopts)
    }

    fn key_slices<K: AsRef<[u8]>>(keys: &[K]) -> Vec<ffi::rocksdb_slice_t> {
        keys.iter()
            .map(|key| {
                let key = key.as_ref();
                ffi::rocksdb_slice_t {
                    data: key.as_ptr() as *const c_char,
                    size: key.len(),
                }
            })
            .collect()
    }

    fn batched_multi_get_pinned_owned<'a, K: AsRef<[u8]>>(
        &'a self,
        keys: &[K],
        sorted_input: bool,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<DBPinnableSlice<'a>>, Error>> {
        let key_slices = Self::key_slices(keys);
        if key_slices.is_empty() {
            return Vec::new();
        }
        let default_cf = OwnedColumnFamilyHandle::default_for(self.inner.inner());
        self.batched_multi_get_pinned_inner(default_cf.inner, &key_slices, sorted_input, readopts)
    }

    fn create_pinnable_batch<'a>(
        &'a self,
        cf: *mut ffi::rocksdb_column_family_handle_t,
        keys: &[ffi::rocksdb_slice_t],
        sorted_input: bool,
        readopts: &ReadOptions,
    ) -> Result<DBPinnableBatch<'a>, Error> {
        let batch = unsafe {
            ffi_try!(ffi::rust_rocksdb_batched_multi_get_pinned(
                self.inner.inner(),
                readopts.inner,
                cf,
                keys.len(),
                keys.as_ptr(),
                c_uchar::from(sorted_input),
            ))
        };
        if batch.is_null() {
            // `ffi_try!` only returns early when the extension set `errptr`.
            // A null batch with no error means the extension could not even
            // allocate the message, so report it instead of unwrapping.
            return Err(Error::new(
                "rust_rocksdb_batched_multi_get_pinned returned no batch".to_owned(),
            ));
        }
        // SAFETY: The extension returns a uniquely owned batch.
        Ok(unsafe { DBPinnableBatch::from_c(batch) })
    }

    fn batched_multi_get_pinned_inner<'a>(
        &'a self,
        cf: *mut ffi::rocksdb_column_family_handle_t,
        keys: &[ffi::rocksdb_slice_t],
        sorted_input: bool,
        readopts: &ReadOptions,
    ) -> Vec<Result<Option<DBPinnableSlice<'a>>, Error>> {
        if keys.is_empty() {
            return Vec::new();
        }
        let output = match self.execute_batched_multi_get(cf, keys, sorted_input, readopts) {
            Ok(output) => output,
            Err(error) => {
                let message = error.to_string();
                return (0..keys.len())
                    .map(|_| Err(Error::new(message.clone())))
                    .collect();
            }
        };
        output
            .values
            .into_iter()
            .zip(output.errors)
            .map(|(value, error)| unsafe { Self::convert_pinned_result(value, error) })
            .collect()
    }

    fn execute_batched_multi_get(
        &self,
        cf: *mut ffi::rocksdb_column_family_handle_t,
        keys: &[ffi::rocksdb_slice_t],
        sorted_input: bool,
        readopts: &ReadOptions,
    ) -> Result<PinnedMultiGetOutput, Error> {
        let mut pinned_values = vec![ptr::null_mut(); keys.len()];
        let mut errors = vec![ptr::null_mut(); keys.len()];
        unsafe {
            ffi_try!(ffi::rust_rocksdb_batched_multi_get_cf_slice_safe(
                self.inner.inner(),
                readopts.inner,
                cf,
                keys.len(),
                keys.as_ptr(),
                pinned_values.as_mut_ptr(),
                errors.as_mut_ptr(),
                c_uchar::from(sorted_input),
            ));
        }
        Ok(PinnedMultiGetOutput {
            values: pinned_values,
            errors,
        })
    }

    /// Converts one result returned by `rocksdb_batched_multi_get_cf_slice`.
    ///
    /// # Safety
    ///
    /// `value` must be null or an owned pinnable slice. `error` must be null or
    /// an owned RocksDB error string.
    unsafe fn convert_pinned_result<'a>(
        value: *mut ffi::rocksdb_pinnableslice_t,
        error: *mut c_char,
    ) -> Result<Option<DBPinnableSlice<'a>>, Error> {
        if error.is_null() {
            return Ok((!value.is_null()).then(|| unsafe { DBPinnableSlice::from_c(value) }));
        }
        if !value.is_null() {
            unsafe {
                ffi::rocksdb_pinnableslice_destroy(value);
            }
        }
        Err(convert_rocksdb_error(error))
    }

    /// Returns `false` if the given key definitely doesn't exist in the database, otherwise returns
    /// `true`. This function uses default `ReadOptions`.
    pub fn key_may_exist<K: AsRef<[u8]>>(&self, key: K) -> bool {
        DEFAULT_READ_OPTS.with(|opts| self.key_may_exist_opt(key, opts))
    }

    /// Returns `false` if the given key definitely doesn't exist in the database, otherwise returns
    /// `true`.
    pub fn key_may_exist_opt<K: AsRef<[u8]>>(&self, key: K, readopts: &ReadOptions) -> bool {
        let key = key.as_ref();
        unsafe {
            0 != ffi::rocksdb_key_may_exist(
                self.inner.inner(),
                readopts.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                ptr::null_mut(), /*value*/
                ptr::null_mut(), /*val_len*/
                ptr::null(),     /*timestamp*/
                0,               /*timestamp_len*/
                ptr::null_mut(), /*value_found*/
            )
        }
    }

    /// Returns `false` if the given key definitely doesn't exist in the specified column family,
    /// otherwise returns `true`. This function uses default `ReadOptions`.
    pub fn key_may_exist_cf<K: AsRef<[u8]>>(&self, cf: &impl AsColumnFamilyRef, key: K) -> bool {
        DEFAULT_READ_OPTS.with(|opts| self.key_may_exist_cf_opt(cf, key, opts))
    }

    /// Returns `false` if the given key definitely doesn't exist in the specified column family,
    /// otherwise returns `true`.
    pub fn key_may_exist_cf_opt<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: &ReadOptions,
    ) -> bool {
        let key = key.as_ref();
        0 != unsafe {
            ffi::rocksdb_key_may_exist_cf(
                self.inner.inner(),
                readopts.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                ptr::null_mut(), /*value*/
                ptr::null_mut(), /*val_len*/
                ptr::null(),     /*timestamp*/
                0,               /*timestamp_len*/
                ptr::null_mut(), /*value_found*/
            )
        }
    }

    /// If the key definitely does not exist in the database, then this method
    /// returns `(false, None)`, else `(true, None)` if it may.
    /// If the key is found in memory, then it returns `(true, Some<CSlice>)`.
    ///
    /// This check is potentially lighter-weight than calling `get()`. One way
    /// to make this lighter weight is to avoid doing any IOs.
    pub fn key_may_exist_cf_opt_value<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: &ReadOptions,
    ) -> (bool, Option<CSlice>) {
        let key = key.as_ref();
        let mut val: *mut c_char = ptr::null_mut();
        let mut val_len: usize = 0;
        let mut value_found: c_uchar = 0;
        let may_exists = 0
            != unsafe {
                ffi::rocksdb_key_may_exist_cf(
                    self.inner.inner(),
                    readopts.inner,
                    cf.inner(),
                    key.as_ptr() as *const c_char,
                    key.len() as size_t,
                    &raw mut val,         /*value*/
                    &raw mut val_len,     /*val_len*/
                    ptr::null(),          /*timestamp*/
                    0,                    /*timestamp_len*/
                    &raw mut value_found, /*value_found*/
                )
            };
        // The value is only allocated (using malloc) and returned if it is found and
        // value_found isn't NULL. In that case the user is responsible for freeing it.
        if may_exists && value_found != 0 {
            (
                may_exists,
                Some(unsafe { CSlice::from_raw_parts(val, val_len) }),
            )
        } else {
            (may_exists, None)
        }
    }

    fn create_inner_cf_handle(
        &self,
        name: impl CStrLike,
        opts: &Options,
    ) -> Result<*mut ffi::rocksdb_column_family_handle_t, Error> {
        let cf_name = name.bake().map_err(|err| {
            Error::new(format!(
                "Failed to convert path to CString when creating cf: {err}"
            ))
        })?;

        // Can't use ffi_try: rocksdb_create_column_family has a bug where it allocates a
        // result that needs to be freed on error
        let mut err: *mut ::libc::c_char = ::std::ptr::null_mut();
        let cf_handle = unsafe {
            ffi::rocksdb_create_column_family(
                self.inner.inner(),
                opts.inner,
                cf_name.as_ptr(),
                &raw mut err,
            )
        };
        if !err.is_null() {
            if !cf_handle.is_null() {
                unsafe { ffi::rocksdb_column_family_handle_destroy(cf_handle) };
            }
            return Err(convert_rocksdb_error(err));
        }
        Ok(cf_handle)
    }

    /// Creates every named column family in one call.
    ///
    /// This is not atomic. RocksDB creates the families one at a time and stops at
    /// the first failure, so the ones before it are already committed and stay that
    /// way. Only the options file write at the end is shared, which is the whole
    /// saving over calling [`create_cf`](Self::create_cf) in a loop.
    ///
    /// Returns the handles that were created, in the order the names were given,
    /// alongside the error that stopped the rest. The caller owns those handles and
    /// has to record them in its column family map even when there is an error,
    /// because the families exist either way.
    fn create_inner_cf_handles(
        &self,
        names: &[(String, CString)],
        opts: &Options,
    ) -> CreatedCfHandles {
        let name_ptrs: Vec<*const c_char> = names.iter().map(|(_, name)| name.as_ptr()).collect();
        let Ok(count) = c_int::try_from(names.len()) else {
            return CreatedCfHandles::failed(Error::new(format!(
                "Too many column families to create at once: {}",
                names.len()
            )));
        };

        let mut len: usize = 0;
        let mut err: *mut ::libc::c_char = ::std::ptr::null_mut();
        // Can't use ffi_try: like rocksdb_create_column_family, this allocates a result
        // that needs to be freed on error.
        let list = unsafe {
            ffi::rocksdb_create_column_families(
                self.inner.inner(),
                opts.inner,
                count,
                name_ptrs.as_ptr(),
                &raw mut len,
                &raw mut err,
            )
        };

        // Two allocations come back: the array, and a handle per family. Freeing the
        // array does not touch the handles, so take copies of them and release the
        // array on its own.
        let handles = if list.is_null() {
            Vec::new()
        } else {
            let handles = unsafe { std::slice::from_raw_parts(list, len) }.to_vec();
            unsafe { ffi::rocksdb_create_column_families_destroy(list) };
            handles
        };

        if !err.is_null() {
            // Hand the handles back even though this failed. The families they name
            // were committed before the failing one and are still there, so the caller
            // needs them to be able to use or drop those families.
            return CreatedCfHandles {
                handles,
                error: Some(convert_rocksdb_error(err)),
            };
        }

        let created = handles.len();
        let error = (created != names.len()).then(|| {
            Error::new(format!(
                "Expected {} column family handles, got {created}",
                names.len(),
            ))
        });

        CreatedCfHandles { handles, error }
    }

    /// Creates one column family whose entries expire after `ttl`.
    ///
    /// Only valid on a DB opened with a TTL. The C function casts the handle to
    /// `DBWithTTL` unchecked, so the caller has to have proven that already.
    fn create_inner_cf_handle_with_ttl(
        &self,
        name: impl CStrLike,
        opts: &Options,
        ttl: ColumnFamilyTtl,
    ) -> Result<*mut ffi::rocksdb_column_family_handle_t, Error> {
        let Some(db_ttl) = self.opened_with_ttl else {
            return Err(Error::new(
                "create_cf_with_ttl requires a database opened with DB::open_with_ttl \
                 or one of the open_cf*_with_ttl functions"
                    .to_owned(),
            ));
        };

        let cf_name = name.bake().map_err(|err| {
            Error::new(format!(
                "Failed to convert name to CString when creating cf with ttl: {err}"
            ))
        })?;
        let ttl = cf_ttl_to_seconds(ttl, db_ttl);

        // Can't use ffi_try: like rocksdb_create_column_family, this allocates a result
        // that needs to be freed on error.
        let mut err: *mut ::libc::c_char = ::std::ptr::null_mut();
        let cf_handle = unsafe {
            ffi::rocksdb_create_column_family_with_ttl(
                self.inner.inner(),
                opts.inner,
                cf_name.as_ptr(),
                ttl,
                &raw mut err,
            )
        };
        if !err.is_null() {
            if !cf_handle.is_null() {
                unsafe { ffi::rocksdb_column_family_handle_destroy(cf_handle) };
            }
            return Err(convert_rocksdb_error(err));
        }
        Ok(cf_handle)
    }

    pub fn iterator<'a: 'b, 'b>(
        &'a self,
        mode: IteratorMode,
    ) -> DBIteratorWithThreadMode<'b, Self> {
        let readopts = ReadOptions::default();
        self.iterator_opt(mode, readopts)
    }

    pub fn iterator_opt<'a: 'b, 'b>(
        &'a self,
        mode: IteratorMode,
        readopts: ReadOptions,
    ) -> DBIteratorWithThreadMode<'b, Self> {
        DBIteratorWithThreadMode::new(self, readopts, mode)
    }

    /// Opens an iterator using the provided ReadOptions.
    /// This is used when you want to iterate over a specific ColumnFamily with a modified ReadOptions
    pub fn iterator_cf_opt<'a: 'b, 'b>(
        &'a self,
        cf_handle: &impl AsColumnFamilyRef,
        readopts: ReadOptions,
        mode: IteratorMode,
    ) -> DBIteratorWithThreadMode<'b, Self> {
        DBIteratorWithThreadMode::new_cf(self, cf_handle.inner(), readopts, mode)
    }

    /// Opens an iterator with `set_total_order_seek` enabled.
    /// This must be used to iterate across prefixes when `set_memtable_factory` has been called
    /// with a Hash-based implementation.
    pub fn full_iterator<'a: 'b, 'b>(
        &'a self,
        mode: IteratorMode,
    ) -> DBIteratorWithThreadMode<'b, Self> {
        let mut opts = ReadOptions::default();
        opts.set_total_order_seek(true);
        DBIteratorWithThreadMode::new(self, opts, mode)
    }

    pub fn prefix_iterator<'a: 'b, 'b, P: AsRef<[u8]>>(
        &'a self,
        prefix: P,
    ) -> DBIteratorWithThreadMode<'b, Self> {
        let mut opts = ReadOptions::default();
        opts.set_prefix_same_as_start(true);
        DBIteratorWithThreadMode::new(
            self,
            opts,
            IteratorMode::From(prefix.as_ref(), Direction::Forward),
        )
    }

    pub fn iterator_cf<'a: 'b, 'b>(
        &'a self,
        cf_handle: &impl AsColumnFamilyRef,
        mode: IteratorMode,
    ) -> DBIteratorWithThreadMode<'b, Self> {
        let opts = ReadOptions::default();
        DBIteratorWithThreadMode::new_cf(self, cf_handle.inner(), opts, mode)
    }

    pub fn full_iterator_cf<'a: 'b, 'b>(
        &'a self,
        cf_handle: &impl AsColumnFamilyRef,
        mode: IteratorMode,
    ) -> DBIteratorWithThreadMode<'b, Self> {
        let mut opts = ReadOptions::default();
        opts.set_total_order_seek(true);
        DBIteratorWithThreadMode::new_cf(self, cf_handle.inner(), opts, mode)
    }

    pub fn prefix_iterator_cf<'a, P: AsRef<[u8]>>(
        &'a self,
        cf_handle: &impl AsColumnFamilyRef,
        prefix: P,
    ) -> DBIteratorWithThreadMode<'a, Self> {
        let mut opts = ReadOptions::default();
        opts.set_prefix_same_as_start(true);
        DBIteratorWithThreadMode::<'a, Self>::new_cf(
            self,
            cf_handle.inner(),
            opts,
            IteratorMode::From(prefix.as_ref(), Direction::Forward),
        )
    }

    /// Returns `true` if there exists at least one key with the given prefix
    /// in the default column family using default read options.
    ///
    /// When to use: prefer this for one-shot checks. It enables
    /// `prefix_same_as_start(true)` and bounds the iterator to the
    /// prefix via `PrefixRange`, minimizing stray IO per call.
    pub fn prefix_exists<P: AsRef<[u8]>>(&self, prefix: P) -> Result<bool, Error> {
        let p = prefix.as_ref();
        with_prefix_read_opts(p, |opts| self.prefix_exists_opt(p, opts))
    }

    /// Returns `true` if there exists at least one key with the given prefix
    /// in the default column family using the provided read options.
    pub fn prefix_exists_opt<P: AsRef<[u8]>>(
        &self,
        prefix: P,
        readopts: &ReadOptions,
    ) -> Result<bool, Error> {
        let prefix = prefix.as_ref();
        let iter = unsafe { self.create_iterator(readopts) };
        let res = unsafe {
            ffi::rocksdb_iter_seek(
                iter,
                prefix.as_ptr() as *const c_char,
                prefix.len() as size_t,
            );
            if ffi::rocksdb_iter_valid(iter) != 0 {
                let mut key_len: size_t = 0;
                let key_ptr = ffi::rocksdb_iter_key(iter, &raw mut key_len);
                // An empty key is legal, and `from_raw_parts` wants a
                // dereferenceable pointer even at length 0.
                let key = if key_len == 0 {
                    &[][..]
                } else {
                    slice::from_raw_parts(key_ptr.cast::<u8>(), key_len as usize)
                };
                Ok(key.starts_with(prefix))
            } else if let Err(e) = (|| {
                // Check status to differentiate end-of-range vs error
                ffi_try!(ffi::rocksdb_iter_get_error(iter));
                Ok::<(), Error>(())
            })() {
                Err(e)
            } else {
                Ok(false)
            }
        };
        unsafe { ffi::rocksdb_iter_destroy(iter) };
        res
    }

    /// Creates a reusable prefix prober over the default column family using
    /// read options optimized for prefix probes.
    ///
    /// When to use: prefer this in hot loops with many checks per second. It
    /// reuses a raw iterator to avoid per-call allocation/FFI overhead. If you
    /// need custom tuning (e.g. async IO, readahead, cache-only), use
    /// `prefix_prober_with_opts`.
    pub fn prefix_prober(&self) -> PrefixProber<'_, Self> {
        PrefixProber {
            raw: DBRawIteratorWithThreadMode::new(self, prefix_probe_read_opts()),
        }
    }

    /// Creates a reusable prefix prober over the default column family using
    /// the provided read options (owned).
    ///
    /// When to use: advanced tuning for heavy workloads. Callers can set
    /// `set_async_io(true)`, `set_readahead_size`, `set_read_tier`, etc. Note:
    /// the prober owns `ReadOptions` to keep internal buffers alive.
    pub fn prefix_prober_with_opts(&self, readopts: ReadOptions) -> PrefixProber<'_, Self> {
        PrefixProber {
            raw: DBRawIteratorWithThreadMode::new(self, readopts),
        }
    }

    /// Creates a reusable prefix prober over the specified column family using
    /// read options optimized for prefix probes.
    pub fn prefix_prober_cf(&self, cf_handle: &impl AsColumnFamilyRef) -> PrefixProber<'_, Self> {
        PrefixProber {
            raw: DBRawIteratorWithThreadMode::new_cf(
                self,
                cf_handle.inner(),
                prefix_probe_read_opts(),
            ),
        }
    }

    /// Creates a reusable prefix prober over the specified column family using
    /// the provided read options (owned).
    ///
    /// When to use: advanced tuning for heavy workloads on a specific CF.
    pub fn prefix_prober_cf_with_opts(
        &self,
        cf_handle: &impl AsColumnFamilyRef,
        readopts: ReadOptions,
    ) -> PrefixProber<'_, Self> {
        PrefixProber {
            raw: DBRawIteratorWithThreadMode::new_cf(self, cf_handle.inner(), readopts),
        }
    }

    /// Returns `true` if there exists at least one key with the given prefix
    /// in the specified column family using default read options.
    ///
    /// When to use: one-shot checks on a CF. Enables
    /// `prefix_same_as_start(true)` and bounds the iterator via `PrefixRange`.
    pub fn prefix_exists_cf<P: AsRef<[u8]>>(
        &self,
        cf_handle: &impl AsColumnFamilyRef,
        prefix: P,
    ) -> Result<bool, Error> {
        let p = prefix.as_ref();
        with_prefix_read_opts(p, |opts| self.prefix_exists_cf_opt(cf_handle, p, opts))
    }

    /// Returns `true` if there exists at least one key with the given prefix
    /// in the specified column family using the provided read options.
    pub fn prefix_exists_cf_opt<P: AsRef<[u8]>>(
        &self,
        cf_handle: &impl AsColumnFamilyRef,
        prefix: P,
        readopts: &ReadOptions,
    ) -> Result<bool, Error> {
        let prefix = prefix.as_ref();
        let iter = unsafe { self.create_iterator_cf(cf_handle.inner(), readopts) };
        let res = unsafe {
            ffi::rocksdb_iter_seek(
                iter,
                prefix.as_ptr() as *const c_char,
                prefix.len() as size_t,
            );
            if ffi::rocksdb_iter_valid(iter) != 0 {
                let mut key_len: size_t = 0;
                let key_ptr = ffi::rocksdb_iter_key(iter, &raw mut key_len);
                // An empty key is legal, and `from_raw_parts` wants a
                // dereferenceable pointer even at length 0.
                let key = if key_len == 0 {
                    &[][..]
                } else {
                    slice::from_raw_parts(key_ptr.cast::<u8>(), key_len as usize)
                };
                Ok(key.starts_with(prefix))
            } else if let Err(e) = (|| {
                ffi_try!(ffi::rocksdb_iter_get_error(iter));
                Ok::<(), Error>(())
            })() {
                Err(e)
            } else {
                Ok(false)
            }
        };
        unsafe { ffi::rocksdb_iter_destroy(iter) };
        res
    }

    /// Opens a raw iterator over the database, using the default read options
    pub fn raw_iterator<'a: 'b, 'b>(&'a self) -> DBRawIteratorWithThreadMode<'b, Self> {
        let opts = ReadOptions::default();
        DBRawIteratorWithThreadMode::new(self, opts)
    }

    /// Opens a raw iterator over the given column family, using the default read options
    pub fn raw_iterator_cf<'a: 'b, 'b>(
        &'a self,
        cf_handle: &impl AsColumnFamilyRef,
    ) -> DBRawIteratorWithThreadMode<'b, Self> {
        let opts = ReadOptions::default();
        DBRawIteratorWithThreadMode::new_cf(self, cf_handle.inner(), opts)
    }

    /// Opens raw iterators for multiple column families from one consistent
    /// RocksDB state.
    ///
    /// The returned iterators match the input column family order and own their
    /// native handles. They share one `ReadOptions`, because one native
    /// `rocksdb_create_iterators` call applies a single options object to every
    /// iterator it creates.
    pub fn raw_iterators_cf<'a, 'b, W, I>(
        &'a self,
        column_families: I,
    ) -> Result<Vec<DBRawIteratorWithThreadMode<'a, Self>>, Error>
    where
        W: AsColumnFamilyRef + 'b,
        I: IntoIterator<Item = &'b W>,
    {
        let mut cf_handles: Vec<_> = column_families
            .into_iter()
            .map(AsColumnFamilyRef::inner)
            .collect();
        if cf_handles.is_empty() {
            return Ok(Vec::new());
        }
        let created = self.create_iterators_cf(&mut cf_handles)?;
        Ok(created
            .handles
            .into_iter()
            .map(|handle| {
                DBRawIteratorWithThreadMode::from_inner(handle, Arc::clone(&created.readopts))
            })
            .collect())
    }

    fn create_iterators_cf(
        &self,
        cf_handles: &mut [*mut ffi::rocksdb_column_family_handle_t],
    ) -> Result<CreatedIterators, Error> {
        let mut iterator_handles = vec![ptr::null_mut(); cf_handles.len()];
        // Every iterator gets a handle on this. RocksDB's `DBIter` stores raw
        // `Slice*` into the options for iterate_lower_bound, iterate_upper_bound
        // and the read timestamps, and `ArenaWrappedDBIter::Refresh` re-reads
        // them, so the options have to outlive the last iterator rather than
        // this function. See issue #660.
        let readopts = Arc::new(ReadOptions::default());
        unsafe {
            ffi_try!(ffi::rust_rocksdb_create_iterators_safe(
                self.inner.inner(),
                readopts.inner,
                cf_handles.as_mut_ptr(),
                iterator_handles.as_mut_ptr(),
                iterator_handles.len(),
            ));
        }
        Self::validate_created_iterators(&iterator_handles)?;
        Ok(CreatedIterators {
            readopts,
            handles: iterator_handles,
        })
    }

    fn validate_created_iterators(
        iterator_handles: &[*mut ffi::rocksdb_iterator_t],
    ) -> Result<(), Error> {
        if iterator_handles.iter().any(|iterator| iterator.is_null()) {
            unsafe {
                Self::destroy_iterators(iterator_handles);
            }
            return Err(Error::new(
                "rocksdb_create_iterators returned a null iterator".to_owned(),
            ));
        }
        Ok(())
    }

    /// Destroys non-null iterator handles owned by the caller.
    ///
    /// # Safety
    ///
    /// Every non-null pointer must identify a live, uniquely owned RocksDB iterator.
    unsafe fn destroy_iterators(iterators: &[*mut ffi::rocksdb_iterator_t]) {
        for &iterator in iterators {
            if !iterator.is_null() {
                unsafe {
                    ffi::rocksdb_iter_destroy(iterator);
                }
            }
        }
    }

    /// Opens a raw iterator over the database, using the given read options
    pub fn raw_iterator_opt<'a: 'b, 'b>(
        &'a self,
        readopts: ReadOptions,
    ) -> DBRawIteratorWithThreadMode<'b, Self> {
        DBRawIteratorWithThreadMode::new(self, readopts)
    }

    /// Opens a raw iterator over the given column family, using the given read options
    pub fn raw_iterator_cf_opt<'a: 'b, 'b>(
        &'a self,
        cf_handle: &impl AsColumnFamilyRef,
        readopts: ReadOptions,
    ) -> DBRawIteratorWithThreadMode<'b, Self> {
        DBRawIteratorWithThreadMode::new_cf(self, cf_handle.inner(), readopts)
    }

    pub fn snapshot(&'_ self) -> SnapshotWithThreadMode<'_, Self> {
        SnapshotWithThreadMode::<Self>::new(self)
    }

    pub fn put_opt<K, V>(&self, key: K, value: V, writeopts: &WriteOptions) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let value = value.as_ref();

        unsafe {
            ffi_try!(ffi::rocksdb_put(
                self.inner.inner(),
                writeopts.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                value.as_ptr() as *const c_char,
                value.len() as size_t,
            ));
            Ok(())
        }
    }

    pub fn put_cf_opt<K, V>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        value: V,
        writeopts: &WriteOptions,
    ) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let value = value.as_ref();

        unsafe {
            ffi_try!(ffi::rocksdb_put_cf(
                self.inner.inner(),
                writeopts.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                value.as_ptr() as *const c_char,
                value.len() as size_t,
            ));
            Ok(())
        }
    }

    /// Set the database entry for "key" to "value" with WriteOptions.
    /// If "key" already exists, it will coexist with previous entry.
    /// `Get` with a timestamp ts specified in ReadOptions will return
    /// the most recent key/value whose timestamp is smaller than or equal to ts.
    /// Takes an additional argument `ts` as the timestamp.
    /// Note: the DB must be opened with user defined timestamp enabled.
    pub fn put_with_ts_opt<K, V, S>(
        &self,
        key: K,
        ts: S,
        value: V,
        writeopts: &WriteOptions,
    ) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
        S: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let value = value.as_ref();
        let ts = ts.as_ref();
        unsafe {
            ffi_try!(ffi::rocksdb_put_with_ts(
                self.inner.inner(),
                writeopts.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                ts.as_ptr() as *const c_char,
                ts.len() as size_t,
                value.as_ptr() as *const c_char,
                value.len() as size_t,
            ));
            Ok(())
        }
    }

    /// Put with timestamp in a specific column family with WriteOptions.
    /// If "key" already exists, it will coexist with previous entry.
    /// `Get` with a timestamp ts specified in ReadOptions will return
    /// the most recent key/value whose timestamp is smaller than or equal to ts.
    /// Takes an additional argument `ts` as the timestamp.
    /// Note: the DB must be opened with user defined timestamp enabled.
    pub fn put_cf_with_ts_opt<K, V, S>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        ts: S,
        value: V,
        writeopts: &WriteOptions,
    ) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
        S: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let value = value.as_ref();
        let ts = ts.as_ref();
        unsafe {
            ffi_try!(ffi::rocksdb_put_cf_with_ts(
                self.inner.inner(),
                writeopts.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                ts.as_ptr() as *const c_char,
                ts.len() as size_t,
                value.as_ptr() as *const c_char,
                value.len() as size_t,
            ));
            Ok(())
        }
    }

    pub fn merge_opt<K, V>(&self, key: K, value: V, writeopts: &WriteOptions) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let value = value.as_ref();

        unsafe {
            ffi_try!(ffi::rocksdb_merge(
                self.inner.inner(),
                writeopts.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                value.as_ptr() as *const c_char,
                value.len() as size_t,
            ));
            Ok(())
        }
    }

    pub fn merge_cf_opt<K, V>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        value: V,
        writeopts: &WriteOptions,
    ) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let value = value.as_ref();

        unsafe {
            ffi_try!(ffi::rocksdb_merge_cf(
                self.inner.inner(),
                writeopts.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                value.as_ptr() as *const c_char,
                value.len() as size_t,
            ));
            Ok(())
        }
    }

    pub fn delete_opt<K: AsRef<[u8]>>(
        &self,
        key: K,
        writeopts: &WriteOptions,
    ) -> Result<(), Error> {
        let key = key.as_ref();

        unsafe {
            ffi_try!(ffi::rocksdb_delete(
                self.inner.inner(),
                writeopts.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
            ));
            Ok(())
        }
    }

    pub fn delete_cf_opt<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        writeopts: &WriteOptions,
    ) -> Result<(), Error> {
        let key = key.as_ref();

        unsafe {
            ffi_try!(ffi::rocksdb_delete_cf(
                self.inner.inner(),
                writeopts.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
            ));
            Ok(())
        }
    }

    /// Remove the database entry (if any) for "key" with WriteOptions.
    /// Takes an additional argument `ts` as the timestamp.
    /// Note: the DB must be opened with user defined timestamp enabled.
    pub fn delete_with_ts_opt<K, S>(
        &self,
        key: K,
        ts: S,
        writeopts: &WriteOptions,
    ) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        S: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let ts = ts.as_ref();
        unsafe {
            ffi_try!(ffi::rocksdb_delete_with_ts(
                self.inner.inner(),
                writeopts.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                ts.as_ptr() as *const c_char,
                ts.len() as size_t,
            ));
            Ok(())
        }
    }

    /// Delete with timestamp in a specific column family with WriteOptions.
    /// Takes an additional argument `ts` as the timestamp.
    /// Note: the DB must be opened with user defined timestamp enabled.
    pub fn delete_cf_with_ts_opt<K, S>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        ts: S,
        writeopts: &WriteOptions,
    ) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        S: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let ts = ts.as_ref();
        unsafe {
            ffi_try!(ffi::rocksdb_delete_cf_with_ts(
                self.inner.inner(),
                writeopts.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                ts.as_ptr() as *const c_char,
                ts.len() as size_t,
            ));
            Ok(())
        }
    }

    pub fn put<K, V>(&self, key: K, value: V) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        DEFAULT_WRITE_OPTS.with(|opts| self.put_opt(key, value, opts))
    }

    pub fn put_cf<K, V>(&self, cf: &impl AsColumnFamilyRef, key: K, value: V) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        DEFAULT_WRITE_OPTS.with(|opts| self.put_cf_opt(cf, key, value, opts))
    }

    /// Set the database entry for "key" to "value".
    /// If "key" already exists, it will coexist with previous entry.
    /// `Get` with a timestamp ts specified in ReadOptions will return
    /// the most recent key/value whose timestamp is smaller than or equal to ts.
    /// Takes an additional argument `ts` as the timestamp.
    /// Note: the DB must be opened with user defined timestamp enabled.
    pub fn put_with_ts<K, V, S>(&self, key: K, ts: S, value: V) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
        S: AsRef<[u8]>,
    {
        DEFAULT_WRITE_OPTS
            .with(|opts| self.put_with_ts_opt(key.as_ref(), ts.as_ref(), value.as_ref(), opts))
    }

    /// Put with timestamp in a specific column family.
    /// If "key" already exists, it will coexist with previous entry.
    /// `Get` with a timestamp ts specified in ReadOptions will return
    /// the most recent key/value whose timestamp is smaller than or equal to ts.
    /// Takes an additional argument `ts` as the timestamp.
    /// Note: the DB must be opened with user defined timestamp enabled.
    pub fn put_cf_with_ts<K, V, S>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        ts: S,
        value: V,
    ) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
        S: AsRef<[u8]>,
    {
        DEFAULT_WRITE_OPTS.with(|opts| {
            self.put_cf_with_ts_opt(cf, key.as_ref(), ts.as_ref(), value.as_ref(), opts)
        })
    }

    pub fn merge<K, V>(&self, key: K, value: V) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        DEFAULT_WRITE_OPTS.with(|opts| self.merge_opt(key, value, opts))
    }

    pub fn merge_cf<K, V>(&self, cf: &impl AsColumnFamilyRef, key: K, value: V) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        DEFAULT_WRITE_OPTS.with(|opts| self.merge_cf_opt(cf, key, value, opts))
    }

    pub fn delete<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Error> {
        DEFAULT_WRITE_OPTS.with(|opts| self.delete_opt(key, opts))
    }

    pub fn delete_cf<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
    ) -> Result<(), Error> {
        DEFAULT_WRITE_OPTS.with(|opts| self.delete_cf_opt(cf, key, opts))
    }

    /// Remove the database entry (if any) for "key".
    /// Takes an additional argument `ts` as the timestamp.
    /// Note: the DB must be opened with user defined timestamp enabled.
    pub fn delete_with_ts<K: AsRef<[u8]>, S: AsRef<[u8]>>(
        &self,
        key: K,
        ts: S,
    ) -> Result<(), Error> {
        DEFAULT_WRITE_OPTS.with(|opts| self.delete_with_ts_opt(key, ts, opts))
    }

    /// Delete with timestamp in a specific column family.
    /// Takes an additional argument `ts` as the timestamp.
    /// Note: the DB must be opened with user defined timestamp enabled.
    pub fn delete_cf_with_ts<K: AsRef<[u8]>, S: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        ts: S,
    ) -> Result<(), Error> {
        DEFAULT_WRITE_OPTS.with(|opts| self.delete_cf_with_ts_opt(cf, key, ts, opts))
    }

    /// Remove the database entry for "key" with WriteOptions.
    ///
    /// Requires that the key exists and was not overwritten. Returns OK on success,
    /// and a non-OK status on error. It is not an error if "key" did not exist in the database.
    ///
    /// If a key is overwritten (by calling Put() multiple times), then the result
    /// of calling SingleDelete() on this key is undefined. SingleDelete() only
    /// behaves correctly if there has been only one Put() for this key since the
    /// previous call to SingleDelete() for this key.
    ///
    /// This feature is currently an experimental performance optimization
    /// for a very specific workload. It is up to the caller to ensure that
    /// SingleDelete is only used for a key that is not deleted using Delete() or
    /// written using Merge(). Mixing SingleDelete operations with Deletes and
    /// Merges can result in undefined behavior.
    ///
    /// Note: consider setting options.sync = true.
    ///
    /// For more information, see <https://github.com/facebook/rocksdb/wiki/Single-Delete>
    pub fn single_delete_opt<K: AsRef<[u8]>>(
        &self,
        key: K,
        writeopts: &WriteOptions,
    ) -> Result<(), Error> {
        let key = key.as_ref();

        unsafe {
            ffi_try!(ffi::rocksdb_singledelete(
                self.inner.inner(),
                writeopts.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
            ));
            Ok(())
        }
    }

    /// Remove the database entry for "key" from a specific column family with WriteOptions.
    ///
    /// See single_delete_opt() for detailed behavior and restrictions.
    pub fn single_delete_cf_opt<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        writeopts: &WriteOptions,
    ) -> Result<(), Error> {
        let key = key.as_ref();

        unsafe {
            ffi_try!(ffi::rocksdb_singledelete_cf(
                self.inner.inner(),
                writeopts.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
            ));
            Ok(())
        }
    }

    /// Remove the database entry for "key" with WriteOptions.
    ///
    /// Takes an additional argument `ts` as the timestamp.
    /// Note: the DB must be opened with user defined timestamp enabled.
    ///
    /// See single_delete_opt() for detailed behavior and restrictions.
    pub fn single_delete_with_ts_opt<K, S>(
        &self,
        key: K,
        ts: S,
        writeopts: &WriteOptions,
    ) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        S: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let ts = ts.as_ref();
        unsafe {
            ffi_try!(ffi::rocksdb_singledelete_with_ts(
                self.inner.inner(),
                writeopts.inner,
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                ts.as_ptr() as *const c_char,
                ts.len() as size_t,
            ));
            Ok(())
        }
    }

    /// Remove the database entry for "key" from a specific column family with WriteOptions.
    ///
    /// Takes an additional argument `ts` as the timestamp.
    /// Note: the DB must be opened with user defined timestamp enabled.
    ///
    /// See single_delete_opt() for detailed behavior and restrictions.
    pub fn single_delete_cf_with_ts_opt<K, S>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        ts: S,
        writeopts: &WriteOptions,
    ) -> Result<(), Error>
    where
        K: AsRef<[u8]>,
        S: AsRef<[u8]>,
    {
        let key = key.as_ref();
        let ts = ts.as_ref();
        unsafe {
            ffi_try!(ffi::rocksdb_singledelete_cf_with_ts(
                self.inner.inner(),
                writeopts.inner,
                cf.inner(),
                key.as_ptr() as *const c_char,
                key.len() as size_t,
                ts.as_ptr() as *const c_char,
                ts.len() as size_t,
            ));
            Ok(())
        }
    }

    /// Remove the database entry for "key".
    ///
    /// See single_delete_opt() for detailed behavior and restrictions.
    pub fn single_delete<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Error> {
        DEFAULT_WRITE_OPTS.with(|opts| self.single_delete_opt(key, opts))
    }

    /// Remove the database entry for "key" from a specific column family.
    ///
    /// See single_delete_opt() for detailed behavior and restrictions.
    pub fn single_delete_cf<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
    ) -> Result<(), Error> {
        DEFAULT_WRITE_OPTS.with(|opts| self.single_delete_cf_opt(cf, key, opts))
    }

    /// Remove the database entry for "key".
    ///
    /// Takes an additional argument `ts` as the timestamp.
    /// Note: the DB must be opened with user defined timestamp enabled.
    ///
    /// See single_delete_opt() for detailed behavior and restrictions.
    pub fn single_delete_with_ts<K: AsRef<[u8]>, S: AsRef<[u8]>>(
        &self,
        key: K,
        ts: S,
    ) -> Result<(), Error> {
        DEFAULT_WRITE_OPTS.with(|opts| self.single_delete_with_ts_opt(key, ts, opts))
    }

    /// Remove the database entry for "key" from a specific column family.
    ///
    /// Takes an additional argument `ts` as the timestamp.
    /// Note: the DB must be opened with user defined timestamp enabled.
    ///
    /// See single_delete_opt() for detailed behavior and restrictions.
    pub fn single_delete_cf_with_ts<K: AsRef<[u8]>, S: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        ts: S,
    ) -> Result<(), Error> {
        DEFAULT_WRITE_OPTS.with(|opts| self.single_delete_cf_with_ts_opt(cf, key, ts, opts))
    }

    /// Runs a manual compaction on the Range of keys given. This is not likely to be needed for typical usage.
    pub fn compact_range<S: AsRef<[u8]>, E: AsRef<[u8]>>(&self, start: Option<S>, end: Option<E>) {
        unsafe {
            let start = start.as_ref().map(AsRef::as_ref);
            let end = end.as_ref().map(AsRef::as_ref);

            ffi::rocksdb_compact_range(
                self.inner.inner(),
                opt_bytes_to_ptr(start),
                start.map_or(0, <[u8]>::len) as size_t,
                opt_bytes_to_ptr(end),
                end.map_or(0, <[u8]>::len) as size_t,
            );
        }
    }

    /// Same as `compact_range` but with custom options.
    pub fn compact_range_opt<S: AsRef<[u8]>, E: AsRef<[u8]>>(
        &self,
        start: Option<S>,
        end: Option<E>,
        opts: &CompactOptions,
    ) {
        unsafe {
            let start = start.as_ref().map(AsRef::as_ref);
            let end = end.as_ref().map(AsRef::as_ref);

            ffi::rocksdb_compact_range_opt(
                self.inner.inner(),
                opts.inner,
                opt_bytes_to_ptr(start),
                start.map_or(0, <[u8]>::len) as size_t,
                opt_bytes_to_ptr(end),
                end.map_or(0, <[u8]>::len) as size_t,
            );
        }
    }

    /// Runs a manual compaction on the Range of keys given on the
    /// given column family. This is not likely to be needed for typical usage.
    pub fn compact_range_cf<S: AsRef<[u8]>, E: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        start: Option<S>,
        end: Option<E>,
    ) {
        unsafe {
            let start = start.as_ref().map(AsRef::as_ref);
            let end = end.as_ref().map(AsRef::as_ref);

            ffi::rocksdb_compact_range_cf(
                self.inner.inner(),
                cf.inner(),
                opt_bytes_to_ptr(start),
                start.map_or(0, <[u8]>::len) as size_t,
                opt_bytes_to_ptr(end),
                end.map_or(0, <[u8]>::len) as size_t,
            );
        }
    }

    /// Same as `compact_range_cf` but with custom options.
    pub fn compact_range_cf_opt<S: AsRef<[u8]>, E: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        start: Option<S>,
        end: Option<E>,
        opts: &CompactOptions,
    ) {
        unsafe {
            let start = start.as_ref().map(AsRef::as_ref);
            let end = end.as_ref().map(AsRef::as_ref);

            ffi::rocksdb_compact_range_cf_opt(
                self.inner.inner(),
                cf.inner(),
                opts.inner,
                opt_bytes_to_ptr(start),
                start.map_or(0, <[u8]>::len) as size_t,
                opt_bytes_to_ptr(end),
                end.map_or(0, <[u8]>::len) as size_t,
            );
        }
    }

    /// Wait for all flush and compactions jobs to finish. Jobs to wait include the
    /// unscheduled (queued, but not scheduled yet).
    ///
    /// NOTE: This may also never return if there's sufficient ongoing writes that
    /// keeps flush and compaction going without stopping. The user would have to
    /// cease all the writes to DB to make this eventually return in a stable
    /// state. The user may also use timeout option in WaitForCompactOptions to
    /// make this stop waiting and return when timeout expires.
    pub fn wait_for_compact(&self, opts: &WaitForCompactOptions) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_wait_for_compact(
                self.inner.inner(),
                opts.inner
            ));
        }
        Ok(())
    }

    /// Changes mutable column family options on the default column family at
    /// runtime.
    ///
    /// [`set_db_options`](Self::set_db_options) is the DB wide equivalent. Either the
    /// whole set is applied or none of it is.
    ///
    /// # Aborts
    ///
    /// Some unparseable values take the process down instead of returning an error,
    /// so validate values before passing them here.
    ///
    /// RocksDB parses integers with `std::stoi` and friends
    /// (`util/string_util.cc:378`), which throw `std::invalid_argument`. It does try
    /// to catch that, but not everywhere, and an integer-valued option such as
    /// `write_buffer_size` given a non-numeric value aborts the process with
    /// "Rust cannot catch foreign exceptions". A boolean-valued option such as
    /// `disable_auto_compactions` returns an error instead. Do not rely on which
    /// options fall on which side of that line.
    ///
    /// Catching it here is not an option. `ColumnFamilyData::SetOptions` runs inside
    /// a callback that `VersionSet::LogAndApply` invokes while holding the DB mutex
    /// as the exclusive manifest writer (`db/db_impl/db_impl.cc:1655`), so unwinding
    /// through it leaves that state behind and the next option change on the DB
    /// blocks forever in `InstrumentedCondVar::Wait`. That was reproducible under
    /// ASAN and silent otherwise, which is worse than aborting.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if a name is unknown or the option is not changeable
    /// at runtime. Also errors if any name or value contains an interior NUL byte.
    pub fn set_options(&self, opts: &[(&str, &str)]) -> Result<(), Error> {
        let copts = convert_options(opts)?;
        let cnames: Vec<*const c_char> = copts.iter().map(|opt| opt.0.as_ptr()).collect();
        let cvalues: Vec<*const c_char> = copts.iter().map(|opt| opt.1.as_ptr()).collect();
        unsafe {
            ffi_try!(ffi::rocksdb_set_options(
                self.inner.inner(),
                option_count(&copts)?,
                cnames.as_ptr(),
                cvalues.as_ptr(),
            ));
        }
        Ok(())
    }

    /// Like [`set_options`](Self::set_options), for a single column family.
    ///
    /// # Aborts
    ///
    /// See [`set_options`](Self::set_options).
    ///
    /// # Errors
    ///
    /// See [`set_options`](Self::set_options).
    pub fn set_options_cf(
        &self,
        cf: &impl AsColumnFamilyRef,
        opts: &[(&str, &str)],
    ) -> Result<(), Error> {
        let copts = convert_options(opts)?;
        let cnames: Vec<*const c_char> = copts.iter().map(|opt| opt.0.as_ptr()).collect();
        let cvalues: Vec<*const c_char> = copts.iter().map(|opt| opt.1.as_ptr()).collect();
        unsafe {
            ffi_try!(ffi::rocksdb_set_options_cf(
                self.inner.inner(),
                cf.inner(),
                option_count(&copts)?,
                cnames.as_ptr(),
                cvalues.as_ptr(),
            ));
        }
        Ok(())
    }

    /// Implementation for property_value et al methods.
    ///
    /// `name` is the name of the property.  It will be converted into a CString
    /// and passed to `get_property` as argument.  `get_property` reads the
    /// specified property and either returns NULL or a pointer to a C allocated
    /// string; this method takes ownership of that string and will free it at
    /// the end. That string is parsed using `parse` callback which produces
    /// the returned result.
    fn property_value_impl<R>(
        name: impl CStrLike,
        get_property: impl FnOnce(*const c_char) -> *mut c_char,
        parse: impl FnOnce(&str) -> Result<R, Error>,
    ) -> Result<Option<R>, Error> {
        let value = match name.bake() {
            Ok(prop_name) => get_property(prop_name.as_ptr()),
            Err(e) => {
                return Err(Error::new(format!(
                    "Failed to convert property name to CString: {e}"
                )));
            }
        };
        if value.is_null() {
            return Ok(None);
        }
        let result = match unsafe { CStr::from_ptr(value) }.to_str() {
            Ok(s) => parse(s).map(|value| Some(value)),
            Err(e) => Err(Error::new(format!(
                "Failed to convert property value to string: {e}"
            ))),
        };
        unsafe {
            ffi::rocksdb_free(value as *mut c_void);
        }
        result
    }

    /// Retrieves a RocksDB property by name.
    ///
    /// Full list of properties could be find
    /// [here](https://github.com/facebook/rocksdb/blob/08809f5e6cd9cc4bc3958dd4d59457ae78c76660/include/rocksdb/db.h#L428-L634).
    pub fn property_value(&self, name: impl CStrLike) -> Result<Option<String>, Error> {
        Self::property_value_impl(
            name,
            |prop_name| unsafe { ffi::rocksdb_property_value(self.inner.inner(), prop_name) },
            |str_value| Ok(str_value.to_owned()),
        )
    }

    /// Retrieves a RocksDB property by name, for a specific column family.
    ///
    /// Full list of properties could be find
    /// [here](https://github.com/facebook/rocksdb/blob/08809f5e6cd9cc4bc3958dd4d59457ae78c76660/include/rocksdb/db.h#L428-L634).
    pub fn property_value_cf(
        &self,
        cf: &impl AsColumnFamilyRef,
        name: impl CStrLike,
    ) -> Result<Option<String>, Error> {
        Self::property_value_impl(
            name,
            |prop_name| unsafe {
                ffi::rocksdb_property_value_cf(self.inner.inner(), cf.inner(), prop_name)
            },
            |str_value| Ok(str_value.to_owned()),
        )
    }

    fn property_int_value_impl(
        name: impl CStrLike,
        get_property: impl FnOnce(*const c_char, *mut u64) -> c_int,
        get_string_property: impl FnOnce(*const c_char) -> *mut c_char,
    ) -> Result<Option<u64>, Error> {
        let prop_name = name.bake().map_err(|err| {
            Error::new(format!("Failed to convert property name to CString: {err}"))
        })?;
        let mut value = 0;
        if get_property(prop_name.as_ptr(), &raw mut value) == 0 {
            return Ok(Some(value));
        }

        Self::property_value_impl(
            prop_name.as_ref(),
            get_string_property,
            Self::parse_property_int_value,
        )
    }

    fn parse_property_int_value(value: &str) -> Result<u64, Error> {
        value.parse::<u64>().map_err(|err| {
            Error::new(format!(
                "Failed to convert property value {value} to int: {err}"
            ))
        })
    }

    /// Retrieves a RocksDB property and casts it to an integer.
    ///
    /// Full list of properties that return int values could be find
    /// [here](https://github.com/facebook/rocksdb/blob/08809f5e6cd9cc4bc3958dd4d59457ae78c76660/include/rocksdb/db.h#L654-L689).
    pub fn property_int_value(&self, name: impl CStrLike) -> Result<Option<u64>, Error> {
        Self::property_int_value_impl(
            name,
            |prop_name, value| unsafe {
                ffi::rocksdb_property_int(self.inner.inner(), prop_name, value)
            },
            |prop_name| unsafe { ffi::rocksdb_property_value(self.inner.inner(), prop_name) },
        )
    }

    /// Retrieves a RocksDB property for a specific column family and casts it to an integer.
    ///
    /// Full list of properties that return int values could be find
    /// [here](https://github.com/facebook/rocksdb/blob/08809f5e6cd9cc4bc3958dd4d59457ae78c76660/include/rocksdb/db.h#L654-L689).
    pub fn property_int_value_cf(
        &self,
        cf: &impl AsColumnFamilyRef,
        name: impl CStrLike,
    ) -> Result<Option<u64>, Error> {
        Self::property_int_value_impl(
            name,
            |prop_name, value| unsafe {
                ffi::rocksdb_property_int_cf(self.inner.inner(), cf.inner(), prop_name, value)
            },
            |prop_name| unsafe {
                ffi::rocksdb_property_value_cf(self.inner.inner(), cf.inner(), prop_name)
            },
        )
    }

    /// The sequence number of the most recent transaction.
    pub fn latest_sequence_number(&self) -> u64 {
        unsafe { ffi::rocksdb_get_latest_sequence_number(self.inner.inner()) }
    }

    /// Return the approximate file system space used by keys in each ranges.
    ///
    /// Note that the returned sizes measure file system space usage, so
    /// if the user data compresses by a factor of ten, the returned
    /// sizes will be one-tenth the size of the corresponding user data size.
    ///
    /// Due to lack of abi, only data flushed to disk is taken into account.
    /// # Errors
    ///
    /// Returns the RocksDB error if the size estimate fails, for instance on an
    /// I/O error reading the manifest. No partial sizes are reported in that
    /// case.
    pub fn get_approximate_sizes(&self, ranges: &[Range]) -> Result<Vec<u64>, Error> {
        self.get_approximate_sizes_cfopt(None::<&ColumnFamily>, ranges)
    }

    /// Like [`Self::get_approximate_sizes`], for a single column family.
    ///
    /// # Errors
    ///
    /// See [`Self::get_approximate_sizes`].
    pub fn get_approximate_sizes_cf(
        &self,
        cf: &impl AsColumnFamilyRef,
        ranges: &[Range],
    ) -> Result<Vec<u64>, Error> {
        self.get_approximate_sizes_cfopt(Some(cf), ranges)
    }

    fn get_approximate_sizes_cfopt(
        &self,
        cf: Option<&impl AsColumnFamilyRef>,
        ranges: &[Range],
    ) -> Result<Vec<u64>, Error> {
        let mut args = ApproximateSizesArgs::new(ranges);
        let mut err: *mut c_char = ptr::null_mut();
        match cf {
            None => unsafe {
                ffi::rocksdb_approximate_sizes(
                    self.inner.inner(),
                    args.count,
                    args.start_keys.as_ptr(),
                    args.start_key_lens.as_ptr(),
                    args.end_keys.as_ptr(),
                    args.end_key_lens.as_ptr(),
                    args.sizes.as_mut_ptr(),
                    &raw mut err,
                );
            },
            Some(cf) => unsafe {
                ffi::rocksdb_approximate_sizes_cf(
                    self.inner.inner(),
                    cf.inner(),
                    args.count,
                    args.start_keys.as_ptr(),
                    args.start_key_lens.as_ptr(),
                    args.end_keys.as_ptr(),
                    args.end_key_lens.as_ptr(),
                    args.sizes.as_mut_ptr(),
                    &raw mut err,
                );
            },
        }
        args.finish(err)
    }

    /// Like [`Self::get_approximate_sizes`], but lets the caller say what counts
    /// towards the total and how precise the answer has to be.
    ///
    /// [`Self::get_approximate_sizes`] counts SST files only. Pass a
    /// [`SizeApproximationOptions`] with
    /// [`set_include_memtables`](SizeApproximationOptions::set_include_memtables)
    /// on to include writes that have not been flushed yet.
    ///
    /// # Errors
    ///
    /// See [`Self::get_approximate_sizes`].
    pub fn get_approximate_sizes_with_options(
        &self,
        opts: &SizeApproximationOptions,
        ranges: &[Range],
    ) -> Result<Vec<u64>, Error> {
        let mut args = ApproximateSizesArgs::new(ranges);
        let mut err: *mut c_char = ptr::null_mut();
        unsafe {
            ffi::rocksdb_approximate_sizes_with_options(
                self.inner.inner(),
                opts.as_ptr(),
                args.count,
                args.start_keys.as_ptr(),
                args.start_key_lens.as_ptr(),
                args.end_keys.as_ptr(),
                args.end_key_lens.as_ptr(),
                args.sizes.as_mut_ptr(),
                &raw mut err,
            );
        }
        args.finish(err)
    }

    /// Like [`Self::get_approximate_sizes_with_options`], for a single column
    /// family.
    ///
    /// # Errors
    ///
    /// See [`Self::get_approximate_sizes`].
    pub fn get_approximate_sizes_cf_with_options(
        &self,
        cf: &impl AsColumnFamilyRef,
        opts: &SizeApproximationOptions,
        ranges: &[Range],
    ) -> Result<Vec<u64>, Error> {
        let mut args = ApproximateSizesArgs::new(ranges);
        let mut err: *mut c_char = ptr::null_mut();
        unsafe {
            ffi::rocksdb_approximate_sizes_cf_with_options(
                self.inner.inner(),
                cf.inner(),
                opts.as_ptr(),
                args.count,
                args.start_keys.as_ptr(),
                args.start_key_lens.as_ptr(),
                args.end_keys.as_ptr(),
                args.end_key_lens.as_ptr(),
                args.sizes.as_mut_ptr(),
                &raw mut err,
            );
        }
        args.finish(err)
    }

    /// Like [`Self::get_approximate_sizes_cf_with_options`], but selects what
    /// counts with a flag set instead of an options object.
    ///
    /// Prefer [`Self::get_approximate_sizes_cf_with_options`], which can also
    /// set an error margin. This variant exists because it is the form RocksDB
    /// offers without allocating.
    ///
    /// # Errors
    ///
    /// See [`Self::get_approximate_sizes`].
    pub fn get_approximate_sizes_cf_with_flags(
        &self,
        cf: &impl AsColumnFamilyRef,
        flags: SizeApproximationFlags,
        ranges: &[Range],
    ) -> Result<Vec<u64>, Error> {
        let mut args = ApproximateSizesArgs::new(ranges);
        let mut err: *mut c_char = ptr::null_mut();
        unsafe {
            ffi::rocksdb_approximate_sizes_cf_with_flags(
                self.inner.inner(),
                cf.inner(),
                args.count,
                args.start_keys.as_ptr(),
                args.start_key_lens.as_ptr(),
                args.end_keys.as_ptr(),
                args.end_key_lens.as_ptr(),
                flags.bits(),
                args.sizes.as_mut_ptr(),
                &raw mut err,
            );
        }
        args.finish(err)
    }

    /// Iterate over batches of write operations since a given sequence.
    ///
    /// Produce an iterator that will provide the batches of write operations
    /// that have occurred since the given sequence (see
    /// `latest_sequence_number()`). Use the provided iterator to retrieve each
    /// (`u64`, `WriteBatch`) tuple, and then gather the individual puts and
    /// deletes using the `WriteBatch::iterate()` function.
    ///
    /// Calling `get_updates_since()` with a sequence number that is out of
    /// bounds will return an error.
    pub fn get_updates_since(&self, seq_number: u64) -> Result<DBWALIterator, Error> {
        unsafe {
            // rocksdb_wal_readoptions_t does not appear to have any functions
            // for creating and destroying it; fortunately we can pass a nullptr
            // here to get the default behavior
            let opts: *const ffi::rocksdb_wal_readoptions_t = ptr::null();
            let iter = ffi_try!(ffi::rocksdb_get_updates_since(
                self.inner.inner(),
                seq_number,
                opts
            ));
            Ok(DBWALIterator {
                inner: iter,
                start_seq_number: seq_number,
            })
        }
    }

    /// Tries to catch up with the primary by reading as much as possible from the
    /// log files.
    pub fn try_catch_up_with_primary(&self) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_try_catch_up_with_primary(self.inner.inner()));
        }
        Ok(())
    }

    /// Loads a list of external SST files created with SstFileWriter into the DB with default opts
    pub fn ingest_external_file<P: AsRef<Path>>(&self, paths: Vec<P>) -> Result<(), Error> {
        let opts = IngestExternalFileOptions::default();
        self.ingest_external_file_opts(&opts, paths)
    }

    /// Loads a list of external SST files created with SstFileWriter into the DB
    pub fn ingest_external_file_opts<P: AsRef<Path>>(
        &self,
        opts: &IngestExternalFileOptions,
        paths: Vec<P>,
    ) -> Result<(), Error> {
        let paths_v: Vec<CString> = paths.iter().map(to_cpath).collect::<Result<Vec<_>, _>>()?;
        let cpaths: Vec<_> = paths_v.iter().map(|path| path.as_ptr()).collect();

        self.ingest_external_file_raw(opts, &paths_v, &cpaths)
    }

    /// Loads a list of external SST files created with SstFileWriter into the DB for given Column Family
    /// with default opts
    pub fn ingest_external_file_cf<P: AsRef<Path>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        paths: Vec<P>,
    ) -> Result<(), Error> {
        let opts = IngestExternalFileOptions::default();
        self.ingest_external_file_cf_opts(cf, &opts, paths)
    }

    /// Loads a list of external SST files created with SstFileWriter into the DB for given Column Family
    pub fn ingest_external_file_cf_opts<P: AsRef<Path>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        opts: &IngestExternalFileOptions,
        paths: Vec<P>,
    ) -> Result<(), Error> {
        let paths_v: Vec<CString> = paths.iter().map(to_cpath).collect::<Result<Vec<_>, _>>()?;
        let cpaths: Vec<_> = paths_v.iter().map(|path| path.as_ptr()).collect();

        self.ingest_external_file_raw_cf(cf, opts, &paths_v, &cpaths)
    }

    fn ingest_external_file_raw(
        &self,
        opts: &IngestExternalFileOptions,
        paths_v: &[CString],
        cpaths: &[*const c_char],
    ) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_ingest_external_file(
                self.inner.inner(),
                cpaths.as_ptr(),
                paths_v.len(),
                opts.inner.cast_const()
            ));
            Ok(())
        }
    }

    fn ingest_external_file_raw_cf(
        &self,
        cf: &impl AsColumnFamilyRef,
        opts: &IngestExternalFileOptions,
        paths_v: &[CString],
        cpaths: &[*const c_char],
    ) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_ingest_external_file_cf(
                self.inner.inner(),
                cf.inner(),
                cpaths.as_ptr(),
                paths_v.len(),
                opts.inner.cast_const()
            ));
            Ok(())
        }
    }

    /// Obtains the LSM-tree meta data of the default column family of the DB
    pub fn get_column_family_metadata(&self) -> ColumnFamilyMetaData {
        unsafe {
            let ptr = ffi::rocksdb_get_column_family_metadata(self.inner.inner());

            let metadata = ColumnFamilyMetaData {
                size: ffi::rocksdb_column_family_metadata_get_size(ptr),
                name: from_cstr_and_free(ffi::rocksdb_column_family_metadata_get_name(ptr)),
                file_count: ffi::rocksdb_column_family_metadata_get_file_count(ptr),
            };

            // destroy
            ffi::rocksdb_column_family_metadata_destroy(ptr);

            // return
            metadata
        }
    }

    /// Obtains the LSM-tree meta data of the specified column family of the DB
    pub fn get_column_family_metadata_cf(
        &self,
        cf: &impl AsColumnFamilyRef,
    ) -> ColumnFamilyMetaData {
        unsafe {
            let ptr = ffi::rocksdb_get_column_family_metadata_cf(self.inner.inner(), cf.inner());

            let metadata = ColumnFamilyMetaData {
                size: ffi::rocksdb_column_family_metadata_get_size(ptr),
                name: from_cstr_and_free(ffi::rocksdb_column_family_metadata_get_name(ptr)),
                file_count: ffi::rocksdb_column_family_metadata_get_file_count(ptr),
            };

            // destroy
            ffi::rocksdb_column_family_metadata_destroy(ptr);

            // return
            metadata
        }
    }

    /// Compacts the named SST files into `output_level`, giving the caller
    /// control over exactly which files are merged.
    ///
    /// [`Self::compact_range`] picks the files itself from a key range. This
    /// picks them by name, which is what a tool driving compaction from
    /// [`Self::live_files`] or [`Self::get_column_family_metadata_with_options`]
    /// needs. The file names come from that metadata, not from the filesystem.
    ///
    /// Runs on the calling thread, so it blocks until the compaction finishes.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if any input file is unknown, if the files do
    /// not form a compactible set, or if the compaction itself fails. Nothing is
    /// compacted in that case.
    pub fn compact_files<I, N>(
        &self,
        opts: &CompactionOptions,
        input_file_names: I,
        output_level: i32,
    ) -> Result<CompactFilesResult, Error>
    where
        I: IntoIterator<Item = N>,
        N: CStrLike,
    {
        self.compact_files_impl(None::<&ColumnFamily>, opts, input_file_names, output_level)
    }

    /// Like [`Self::compact_files`], for a single column family.
    ///
    /// # Errors
    ///
    /// See [`Self::compact_files`].
    pub fn compact_files_cf<I, N>(
        &self,
        cf: &impl AsColumnFamilyRef,
        opts: &CompactionOptions,
        input_file_names: I,
        output_level: i32,
    ) -> Result<CompactFilesResult, Error>
    where
        I: IntoIterator<Item = N>,
        N: CStrLike,
    {
        self.compact_files_impl(Some(cf), opts, input_file_names, output_level)
    }

    fn compact_files_impl<I, N>(
        &self,
        cf: Option<&impl AsColumnFamilyRef>,
        opts: &CompactionOptions,
        input_file_names: I,
        output_level: i32,
    ) -> Result<CompactFilesResult, Error>
    where
        I: IntoIterator<Item = N>,
        N: CStrLike,
    {
        let names = input_file_names
            .into_iter()
            .map(CStrLike::into_c_string)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| Error::new(format!("Invalid input file name: {e}")))?;
        let name_ptrs: Vec<*const c_char> = names.iter().map(|n| n.as_ptr()).collect();

        let mut output_names: *mut *mut c_char = ptr::null_mut();
        let mut output_count: usize = 0;

        // A trivial move returns from `CompactFilesImpl` before
        // `BuildCompactionJobInfo` runs, so it leaves a caller-allocated job info
        // untouched while still reporting success, and nothing afterwards says
        // which path ran. Asking for the job info only when that path is off
        // keeps every value handed out one RocksDB actually wrote.
        let mut job_info = (!opts.get_allow_trivial_move()).then(OwnedCompactionJobInfo::new);
        let job_info_ptr = job_info
            .as_mut()
            .map_or(ptr::null_mut(), OwnedCompactionJobInfo::as_mut_ptr);

        // `output_path_id` 0 means the first configured DB path. RocksDB has no
        // named constant for it and the crate does not expose `cf_paths`
        // selection here, so it is fixed rather than a parameter nobody could
        // use meaningfully.
        let output_path_id = 0;

        unsafe {
            match cf {
                None => ffi_try!(ffi::rocksdb_compact_files(
                    self.inner.inner(),
                    opts.inner.cast_const(),
                    name_ptrs.as_ptr(),
                    name_ptrs.len(),
                    output_level,
                    output_path_id,
                    &raw mut output_names,
                    &raw mut output_count,
                    job_info_ptr,
                )),
                Some(cf) => ffi_try!(ffi::rocksdb_compact_files_cf(
                    self.inner.inner(),
                    cf.inner(),
                    opts.inner.cast_const(),
                    name_ptrs.as_ptr(),
                    name_ptrs.len(),
                    output_level,
                    output_path_id,
                    &raw mut output_names,
                    &raw mut output_count,
                    job_info_ptr,
                )),
            }
        }

        // On failure RocksDB returns before touching either out-param, so this
        // only runs once the call succeeded and both are known good.
        let output_files = unsafe { collect_and_free_output_names(output_names, output_count) };

        Ok(CompactFilesResult {
            output_files,
            job_info,
        })
    }

    /// Obtains the LSM-tree meta data of the default column family, reporting
    /// only the levels and files `opts` selects.
    ///
    /// Unlike [`Self::get_column_family_metadata`], which returns only the
    /// totals, this reports every level and every SST file in it.
    pub fn get_column_family_metadata_with_options(
        &self,
        opts: &ColumnFamilyMetaDataOptions,
    ) -> Vec<LevelMetaData> {
        unsafe {
            let ptr = ffi::rocksdb_get_column_family_metadata_with_options(
                self.inner.inner(),
                opts.inner,
            );
            // The level and file handles borrow from `ptr`, so the returned
            // values own it and destroy it when the last one drops.
            levels_from_cf_metadata_owned(ptr)
        }
    }

    /// Like [`Self::get_column_family_metadata_with_options`], for a single
    /// column family.
    pub fn get_column_family_metadata_cf_with_options(
        &self,
        cf: &impl AsColumnFamilyRef,
        opts: &ColumnFamilyMetaDataOptions,
    ) -> Vec<LevelMetaData> {
        unsafe {
            let ptr = ffi::rocksdb_get_column_family_metadata_cf_with_options(
                self.inner.inner(),
                cf.inner(),
                opts.inner,
            );
            levels_from_cf_metadata_owned(ptr)
        }
    }

    /// Returns every file RocksDB needs in order to restore this DB, which is
    /// what a copy-based backup has to capture.
    ///
    /// This covers more than [`Self::live_files`] does: alongside the SST files
    /// it reports the WAL, manifest, options and `CURRENT` files, each with the
    /// size and, when `opts` asks for it, the checksum needed to verify a copy.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the file list cannot be gathered, for
    /// instance when a flush it needs to run fails.
    pub fn get_livefiles_storage_info(
        &self,
        opts: &LiveFilesStorageInfoOptions,
    ) -> Result<LiveFilesStorageInfo, Error> {
        unsafe {
            let ptr = ffi_try!(ffi::rocksdb_get_livefiles_storage_info(
                self.inner.inner(),
                opts.inner,
            ));
            if ptr.is_null() {
                return Err(Error::new(
                    "Could not get live files storage info".to_owned(),
                ));
            }
            Ok(LiveFilesStorageInfo::from_ptr(ptr))
        }
    }

    /// Returns every WAL file the DB currently knows about, oldest first,
    /// including the one being written to.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the WAL directory cannot be listed.
    pub fn get_sorted_wal_files(&self) -> Result<WalFiles, Error> {
        unsafe {
            let ptr = ffi_try!(ffi::rocksdb_get_sorted_wal_files(self.inner.inner()));
            if ptr.is_null() {
                return Err(Error::new("Could not get sorted WAL files".to_owned()));
            }
            Ok(WalFiles::from_ptr(ptr))
        }
    }

    /// Returns the WAL file currently being written to.
    ///
    /// Reported as alive, with its size read from the filesystem, and with a
    /// [`start_sequence`](crate::wal::WalFile::start_sequence) of 0 rather than a
    /// real sequence number.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the current WAL file cannot be identified.
    pub fn get_current_wal_file(&self) -> Result<OwnedWalFile, Error> {
        unsafe {
            let ptr = ffi_try!(ffi::rocksdb_get_current_wal_file(self.inner.inner()));
            if ptr.is_null() {
                return Err(Error::new("Could not get current WAL file".to_owned()));
            }
            Ok(OwnedWalFile::from_ptr(ptr))
        }
    }

    /// Starts recording every read and write to a trace file at `trace_path`,
    /// which [`Self::new_default_replayer`] can later replay against a DB.
    ///
    /// The trace file is written through the DB's own `Env` with default file
    /// options, so it is not rate limited.
    ///
    /// Call [`Self::end_trace`] to stop.
    ///
    /// Starting a second trace without ending the first does not fail. `StartTrace`
    /// installs the new tracer unconditionally, and the old one is dropped without
    /// its buffered records being written, so the first trace file is left
    /// truncated. Unlike [`Self::start_io_trace`] and
    /// [`Self::start_block_cache_trace`], which report `Busy`, this one is the
    /// caller's to get right.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the trace file cannot be created.
    pub fn start_trace<P: AsRef<Path>>(
        &self,
        opts: &TraceOptions,
        trace_path: P,
    ) -> Result<(), Error> {
        let cpath = to_cpath(trace_path)?;
        unsafe {
            // Null `env` uses the DB's own `Env`, which the DB already keeps
            // alive, and null `env_options` uses RocksDB's defaults. Passing an
            // `EnvOptions` here would be unsound: the trace writer keeps its
            // `rate_limiter` as a borrowed pointer for the life of the trace,
            // but the reference counting it lives behind stays in the caller's
            // handle.
            ffi_try!(ffi::rocksdb_start_trace(
                self.inner.inner(),
                ptr::null_mut(),
                ptr::null(),
                opts.inner,
                cpath.as_ptr(),
            ));
        }
        Ok(())
    }

    /// Stops the trace started by [`Self::start_trace`] and closes the file.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if no trace is running or the file cannot be
    /// closed cleanly.
    pub fn end_trace(&self) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_end_trace(self.inner.inner()));
        }
        Ok(())
    }

    /// Starts recording file system operations to a trace file at `trace_path`,
    /// for diagnosing IO behaviour rather than replaying queries.
    ///
    /// Written the same way as [`Self::start_trace`]. Call [`Self::end_io_trace`]
    /// to stop.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the trace file cannot be created, or `Busy` if
    /// an IO trace is already running.
    pub fn start_io_trace<P: AsRef<Path>>(
        &self,
        opts: &TraceOptions,
        trace_path: P,
    ) -> Result<(), Error> {
        let cpath = to_cpath(trace_path)?;
        unsafe {
            ffi_try!(ffi::rocksdb_start_io_trace(
                self.inner.inner(),
                ptr::null_mut(),
                ptr::null(),
                opts.inner,
                cpath.as_ptr(),
            ));
        }
        Ok(())
    }

    /// Stops the IO trace started by [`Self::start_io_trace`].
    ///
    /// Does nothing if no IO trace is running.
    ///
    /// # Errors
    ///
    /// `EndIOTrace` always reports success, so this only fails if a future RocksDB
    /// gives it something to report. It returns a `Result` to keep that a
    /// non-breaking change.
    pub fn end_io_trace(&self) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_end_io_trace(self.inner.inner()));
        }
        Ok(())
    }

    /// Starts recording block cache accesses to a trace file at `trace_path`,
    /// which the `block_cache_trace_analyzer` tool can then simulate cache
    /// configurations against.
    ///
    /// Written the same way as [`Self::start_trace`]. Call
    /// [`Self::end_block_cache_trace`] to stop.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the trace file cannot be created, or `Busy` if
    /// a block cache trace is already running.
    pub fn start_block_cache_trace<P: AsRef<Path>>(
        &self,
        opts: &BlockCacheTraceOptions,
        writer_opts: &BlockCacheTraceWriterOptions,
        trace_path: P,
    ) -> Result<(), Error> {
        let cpath = to_cpath(trace_path)?;
        unsafe {
            ffi_try!(ffi::rocksdb_start_block_cache_trace_with_options(
                self.inner.inner(),
                ptr::null_mut(),
                ptr::null(),
                opts.inner,
                writer_opts.inner,
                cpath.as_ptr(),
            ));
        }
        Ok(())
    }

    /// Stops the block cache trace started by [`Self::start_block_cache_trace`].
    ///
    /// Does nothing if no block cache trace is running.
    ///
    /// # Errors
    ///
    /// `EndBlockCacheTrace` always reports success, so this only fails if a future
    /// RocksDB gives it something to report. It returns a `Result` to keep that a
    /// non-breaking change.
    pub fn end_block_cache_trace(&self) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_end_block_cache_trace(self.inner.inner()));
        }
        Ok(())
    }

    /// Builds a replayer that replays the trace at `trace_path` against this DB.
    ///
    /// `column_families` must name every column family the trace touched, or
    /// replay fails with `Corruption: Invalid Column Family ID.`. An empty list
    /// means the default column family only.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if the trace file cannot be opened or read.
    pub fn new_default_replayer<'cf, W, I, P>(
        &self,
        column_families: I,
        trace_path: P,
    ) -> Result<Replayer<'_>, Error>
    where
        W: AsColumnFamilyRef + 'cf,
        I: IntoIterator<Item = &'cf W>,
        P: AsRef<Path>,
    {
        let cpath = to_cpath(trace_path)?;
        unsafe {
            Replayer::create_default(
                self.inner.inner(),
                column_families,
                ptr::null_mut(),
                ptr::null(),
                &cpath,
            )
        }
    }

    /// Returns a list of all table files with their level, start key
    /// and end key
    pub fn live_files(&self) -> Result<Vec<LiveFile>, Error> {
        unsafe {
            let livefiles_ptr = ffi::rocksdb_livefiles(self.inner.inner());
            if livefiles_ptr.is_null() {
                Err(Error::new("Could not get live files".to_owned()))
            } else {
                let files = LiveFile::from_rocksdb_livefiles_ptr(livefiles_ptr);

                // destroy livefiles metadata(s)
                ffi::rocksdb_livefiles_destroy(livefiles_ptr);

                // return
                Ok(files)
            }
        }
    }

    /// Delete sst files whose keys are entirely in the given range.
    ///
    /// Could leave some keys in the range which are in files which are not
    /// entirely in the range.
    ///
    /// Note: L0 files are left regardless of whether they're in the range.
    ///
    /// SnapshotWithThreadModes before the delete might not see the data in the given range.
    pub fn delete_file_in_range<K: AsRef<[u8]>>(&self, from: K, to: K) -> Result<(), Error> {
        let from = from.as_ref();
        let to = to.as_ref();
        unsafe {
            ffi_try!(ffi::rocksdb_delete_file_in_range(
                self.inner.inner(),
                from.as_ptr() as *const c_char,
                from.len() as size_t,
                to.as_ptr() as *const c_char,
                to.len() as size_t,
            ));
            Ok(())
        }
    }

    /// Same as `delete_file_in_range` but only for specific column family
    pub fn delete_file_in_range_cf<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        from: K,
        to: K,
    ) -> Result<(), Error> {
        let from = from.as_ref();
        let to = to.as_ref();
        unsafe {
            ffi_try!(ffi::rocksdb_delete_file_in_range_cf(
                self.inner.inner(),
                cf.inner(),
                from.as_ptr() as *const c_char,
                from.len() as size_t,
                to.as_ptr() as *const c_char,
                to.len() as size_t,
            ));
            Ok(())
        }
    }

    /// Request stopping background work, if wait is true wait until it's done.
    pub fn cancel_all_background_work(&self, wait: bool) {
        unsafe {
            ffi::rocksdb_cancel_all_background_work(self.inner.inner(), c_uchar::from(wait));
        }
    }

    /// Marks the column family as dropped in RocksDB.
    ///
    /// Deliberately does not take ownership of the handle. Callers must take
    /// the handle out of their map first, so that only one caller can ever
    /// *destroy* a given handle, and must put it back if this fails: destroying
    /// it on failure would leave the column family still present in the DB with
    /// no reachable handle, so the only way to touch it again would be to
    /// reopen the database.
    ///
    /// Taking it out of the map does not make the caller the only *reader*. In
    /// `MultiThreaded` mode `cf_handle` clones the same `Arc`, so other threads
    /// can still hold a live `BoundColumnFamily` for this handle. That is fine:
    /// the refcount keeps the handle alive until the last of them is gone.
    fn mark_column_family_dropped(
        &self,
        cf_inner: *mut ffi::rocksdb_column_family_handle_t,
    ) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_drop_column_family(
                self.inner.inner(),
                cf_inner
            ));
        }
        Ok(())
    }

    /// Increase the full_history_ts of column family. The new ts_low value should
    /// be newer than current full_history_ts value.
    /// If another thread updates full_history_ts_low concurrently to a higher
    /// timestamp than the requested ts_low, a try again error will be returned.
    pub fn increase_full_history_ts_low<S: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        ts: S,
    ) -> Result<(), Error> {
        let ts = ts.as_ref();
        unsafe {
            ffi_try!(ffi::rocksdb_increase_full_history_ts_low(
                self.inner.inner(),
                cf.inner(),
                ts.as_ptr() as *const c_char,
                ts.len() as size_t,
            ));
            Ok(())
        }
    }

    /// Get current full_history_ts value.
    pub fn get_full_history_ts_low(&self, cf: &impl AsColumnFamilyRef) -> Result<Vec<u8>, Error> {
        unsafe {
            let mut ts_lowlen = 0;
            let ts = ffi_try!(ffi::rocksdb_get_full_history_ts_low(
                self.inner.inner(),
                cf.inner(),
                &raw mut ts_lowlen,
            ));

            if ts.is_null() {
                Err(Error::new("Could not get full_history_ts_low".to_owned()))
            } else {
                let mut vec = vec![0; ts_lowlen];
                ptr::copy_nonoverlapping(ts.cast::<u8>(), vec.as_mut_ptr(), ts_lowlen);
                ffi::rocksdb_free(ts as *mut c_void);
                Ok(vec)
            }
        }
    }

    /// Returns the DB identity. This is typically ASCII bytes, but that is not guaranteed.
    pub fn get_db_identity(&self) -> Result<Vec<u8>, Error> {
        unsafe {
            let mut length: usize = 0;
            let identity_ptr = ffi::rocksdb_get_db_identity(self.inner.inner(), &raw mut length);
            let identity_vec = raw_data(identity_ptr, length);
            ffi::rocksdb_free(identity_ptr as *mut c_void);
            // In RocksDB: get_db_identity copies a std::string so it should not fail, but
            // the API allows it to be overridden, so it might
            identity_vec.ok_or_else(|| Error::new("get_db_identity returned NULL".to_string()))
        }
    }
}

impl<I: DBInner> DBCommon<SingleThreaded, I> {
    /// Creates column family with given name and options
    pub fn create_cf<N: AsRef<str>>(&mut self, name: N, opts: &Options) -> Result<(), Error> {
        let inner = self.create_inner_cf_handle(name.as_ref(), opts)?;
        self.cfs
            .cfs
            .insert(name.as_ref().to_string(), ColumnFamily { inner });
        Ok(())
    }

    /// Creates a column family whose entries expire after `ttl`, on a DB that was
    /// opened with a TTL.
    ///
    /// [`ColumnFamilyDescriptor::new_with_ttl`] sets this at open time. This is the
    /// way to add such a family to a DB that is already open.
    ///
    /// # Errors
    ///
    /// Errors if the DB was not opened with [`DB::open_with_ttl`] or one of the
    /// `open_cf*_with_ttl` functions, since RocksDB would otherwise treat the handle
    /// as a type it is not. Also returns the RocksDB error if the family already
    /// exists or cannot be created, and errors if the name contains an interior NUL
    /// byte.
    pub fn create_cf_with_ttl<N: AsRef<str>>(
        &mut self,
        name: N,
        opts: &Options,
        ttl: ColumnFamilyTtl,
    ) -> Result<(), Error> {
        let inner = self.create_inner_cf_handle_with_ttl(name.as_ref(), opts, ttl)?;
        self.cfs
            .cfs
            .insert(name.as_ref().to_string(), ColumnFamily { inner });
        Ok(())
    }

    /// Creates the named column families, all sharing `opts`.
    ///
    /// This saves one options file write over calling
    /// [`create_cf`](Self::create_cf) in a loop. It is not a saving on manifest
    /// writes, and it is not atomic.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if a family already exists or cannot be created,
    /// and errors if a name contains an interior NUL byte.
    ///
    /// RocksDB creates the families in order and stops at the first failure, so on
    /// error the families named before the failing one already exist and stay that
    /// way. Those are recorded here as usual and can be used or dropped, so retrying
    /// with the same list will fail again on the ones that now exist.
    pub fn create_cfs<Iter, N>(&mut self, names: Iter, opts: &Options) -> Result<(), Error>
    where
        Iter: IntoIterator<Item = N>,
        N: AsRef<str>,
    {
        let names = convert_cf_names(names)?;
        let created = self.create_inner_cf_handles(&names, opts);
        for ((name, _), inner) in names.into_iter().zip(created.handles) {
            self.cfs.cfs.insert(name, ColumnFamily { inner });
        }
        match created.error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    #[doc = include_str!("db_create_column_family_with_import.md")]
    pub fn create_column_family_with_import<N: AsRef<str>>(
        &mut self,
        options: &Options,
        column_family_name: N,
        import_options: &ImportColumnFamilyOptions,
        metadata: &ExportImportFilesMetaData,
    ) -> Result<(), Error> {
        let name = column_family_name.as_ref();
        let c_name = CString::new(name).map_err(|err| {
            Error::new(format!(
                "Failed to convert name to CString while importing column family: {err}"
            ))
        })?;
        let inner = unsafe {
            ffi_try!(ffi::rocksdb_create_column_family_with_import(
                self.inner.inner(),
                options.inner,
                c_name.as_ptr(),
                import_options.inner,
                metadata.inner
            ))
        };
        self.cfs
            .cfs
            .insert(column_family_name.as_ref().into(), ColumnFamily { inner });
        Ok(())
    }

    /// Drops the column family with the given name
    pub fn drop_cf(&mut self, name: &str) -> Result<(), Error> {
        let Some(cf) = self.cfs.cfs.remove(name) else {
            return Err(Error::new(format!("Invalid column family: {name}")));
        };
        match self.mark_column_family_dropped(cf.inner) {
            // `cf` is dropped here. In single-threaded mode that destroys the
            // handle; in `MultiThreaded` mode it drops one `Arc` reference and
            // the handle is destroyed once the last `BoundColumnFamily` clone
            // handed out by `cf_handle` is gone.
            Ok(()) => Ok(()),
            Err(e) => {
                // The column family is still there, so put the handle back
                // rather than destroying the only way to reach it.
                self.cfs.cfs.insert(name.to_owned(), cf);
                Err(e)
            }
        }
    }

    /// Returns the underlying column family handle
    pub fn cf_handle(&self, name: &str) -> Option<&ColumnFamily> {
        self.cfs.cfs.get(name)
    }

    /// Returns the list of column families currently open.
    ///
    /// The order of names is unspecified and may vary between calls.
    pub fn cf_names(&self) -> Vec<String> {
        self.cfs.cfs.keys().cloned().collect()
    }
}

impl<I: DBInner> DBCommon<MultiThreaded, I> {
    /// Creates column family with given name and options
    pub fn create_cf<N: AsRef<str>>(&self, name: N, opts: &Options) -> Result<(), Error> {
        // Note that we acquire the cfs lock before inserting: otherwise we might race
        // another caller who observed the handle as missing.
        let mut cfs = self.cfs.cfs.write();
        let inner = self.create_inner_cf_handle(name.as_ref(), opts)?;
        cfs.insert(
            name.as_ref().to_string(),
            Arc::new(UnboundColumnFamily { inner }),
        );
        Ok(())
    }

    /// Creates a column family whose entries expire after `ttl`, on a DB that was
    /// opened with a TTL.
    ///
    /// [`ColumnFamilyDescriptor::new_with_ttl`] sets this at open time. This is the
    /// way to add such a family to a DB that is already open.
    ///
    /// # Errors
    ///
    /// Errors if the DB was not opened with [`DB::open_with_ttl`] or one of the
    /// `open_cf*_with_ttl` functions, since RocksDB would otherwise treat the handle
    /// as a type it is not. Also returns the RocksDB error if the family already
    /// exists or cannot be created, and errors if the name contains an interior NUL
    /// byte.
    pub fn create_cf_with_ttl<N: AsRef<str>>(
        &self,
        name: N,
        opts: &Options,
        ttl: ColumnFamilyTtl,
    ) -> Result<(), Error> {
        // Note that we acquire the cfs lock before inserting: otherwise we might race
        // another caller who observed the handle as missing.
        let mut cfs = self.cfs.cfs.write();
        let inner = self.create_inner_cf_handle_with_ttl(name.as_ref(), opts, ttl)?;
        cfs.insert(
            name.as_ref().to_string(),
            Arc::new(UnboundColumnFamily { inner }),
        );
        Ok(())
    }

    /// Creates the named column families, all sharing `opts`.
    ///
    /// See [`DBCommon::create_cfs`](DBCommon::<SingleThreaded, I>::create_cfs),
    /// including the note that this is not atomic.
    ///
    /// # Errors
    ///
    /// Returns the RocksDB error if a family already exists or cannot be created,
    /// and errors if a name contains an interior NUL byte. On error the families
    /// named before the failing one already exist and are recorded here.
    pub fn create_cfs<Iter, N>(&self, names: Iter, opts: &Options) -> Result<(), Error>
    where
        Iter: IntoIterator<Item = N>,
        N: AsRef<str>,
    {
        let names = convert_cf_names(names)?;
        // Note that we acquire the cfs lock before creating: otherwise we might race
        // another caller who observed the handles as missing.
        let mut cfs = self.cfs.cfs.write();
        let created = self.create_inner_cf_handles(&names, opts);
        for ((name, _), inner) in names.into_iter().zip(created.handles) {
            cfs.insert(name, Arc::new(UnboundColumnFamily { inner }));
        }
        match created.error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    #[doc = include_str!("db_create_column_family_with_import.md")]
    pub fn create_column_family_with_import<N: AsRef<str>>(
        &self,
        options: &Options,
        column_family_name: N,
        import_options: &ImportColumnFamilyOptions,
        metadata: &ExportImportFilesMetaData,
    ) -> Result<(), Error> {
        // Acquire CF lock upfront, before creating the CF, to avoid a race with concurrent creators
        let mut cfs = self.cfs.cfs.write();
        let name = column_family_name.as_ref();
        let c_name = CString::new(name).map_err(|err| {
            Error::new(format!(
                "Failed to convert name to CString while importing column family: {err}"
            ))
        })?;
        let inner = unsafe {
            ffi_try!(ffi::rocksdb_create_column_family_with_import(
                self.inner.inner(),
                options.inner,
                c_name.as_ptr(),
                import_options.inner,
                metadata.inner
            ))
        };
        cfs.insert(
            column_family_name.as_ref().to_string(),
            Arc::new(UnboundColumnFamily { inner }),
        );
        Ok(())
    }

    /// Drops the column family with the given name by internally locking the inner column
    /// family map. This avoids needing `&mut self` reference
    pub fn drop_cf(&self, name: &str) -> Result<(), Error> {
        // Take the handle out under the write lock before touching RocksDB.
        // Looking it up under a read lock and removing it afterwards would let
        // two concurrent callers observe the same handle: the first would drop
        // and destroy it, and the second would then hand a freed pointer to
        // `rocksdb_drop_column_family`.
        let Some(cf) = self.cfs.cfs.write().remove(name) else {
            return Err(Error::new(format!("Invalid column family: {name}")));
        };
        match self.mark_column_family_dropped(cf.inner) {
            // `cf` is dropped here. In single-threaded mode that destroys the
            // handle; in `MultiThreaded` mode it drops one `Arc` reference and
            // the handle is destroyed once the last `BoundColumnFamily` clone
            // handed out by `cf_handle` is gone.
            Ok(()) => Ok(()),
            Err(e) => {
                // The column family is still there, so put the handle back
                // rather than destroying the only way to reach it.
                self.cfs.cfs.write().insert(name.to_owned(), cf);
                Err(e)
            }
        }
    }

    /// Returns the underlying column family handle
    pub fn cf_handle(&'_ self, name: &str) -> Option<Arc<BoundColumnFamily<'_>>> {
        self.cfs
            .cfs
            .read()
            .get(name)
            .cloned()
            .map(UnboundColumnFamily::bound_column_family)
    }

    /// Returns the list of column families currently open.
    ///
    /// The order of names is unspecified and may vary between calls.
    pub fn cf_names(&self) -> Vec<String> {
        self.cfs.cfs.read().keys().cloned().collect()
    }
}

impl<T: ThreadMode, I: DBInner> Drop for DBCommon<T, I> {
    fn drop(&mut self) {
        self.cfs.drop_all_cfs_internal();
    }
}

impl<T: ThreadMode, I: DBInner> fmt::Debug for DBCommon<T, I> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "RocksDB {{ path: {} }}", self.path().display())
    }
}

/// The metadata that describes a column family.
#[derive(Debug, Clone)]
pub struct ColumnFamilyMetaData {
    // The size of this column family in bytes, which is equal to the sum of
    // the file size of its "levels".
    pub size: u64,
    // The name of the column family.
    pub name: String,
    // The number of files in this column family.
    pub file_count: usize,
}

/// The metadata that describes a SST file
#[derive(Debug, Clone)]
pub struct LiveFile {
    /// Name of the column family the file belongs to
    pub column_family_name: String,
    /// Name of the file
    pub name: String,
    /// The directory containing the file, without a trailing '/'. This could be
    /// a DB path, wal_dir, etc.
    pub directory: String,
    /// Size of the file
    pub size: usize,
    /// Level at which this file resides
    pub level: i32,
    /// Smallest user defined key in the file
    pub start_key: Option<Vec<u8>>,
    /// Largest user defined key in the file
    pub end_key: Option<Vec<u8>>,
    pub smallest_seqno: u64,
    pub largest_seqno: u64,
    /// Number of entries/alive keys in the file
    pub num_entries: u64,
    /// Number of deletions/tomb key(s) in the file
    pub num_deletions: u64,
}

impl LiveFile {
    /// Create a `Vec<LiveFile>` from a `rocksdb_livefiles_t` pointer
    pub(crate) fn from_rocksdb_livefiles_ptr(
        files: *const ffi::rocksdb_livefiles_t,
    ) -> Vec<LiveFile> {
        unsafe {
            let n = ffi::rocksdb_livefiles_count(files);

            let mut livefiles = Vec::with_capacity(n as usize);
            let mut key_size: usize = 0;

            for i in 0..n {
                // rocksdb_livefiles_* returns pointers to strings, not copies
                let column_family_name =
                    from_cstr_without_free(ffi::rocksdb_livefiles_column_family_name(files, i));
                let name = from_cstr_without_free(ffi::rocksdb_livefiles_name(files, i));
                let directory = from_cstr_without_free(ffi::rocksdb_livefiles_directory(files, i));
                let size = ffi::rocksdb_livefiles_size(files, i);
                let level = ffi::rocksdb_livefiles_level(files, i);

                // get smallest key inside file
                let smallest_key = ffi::rocksdb_livefiles_smallestkey(files, i, &raw mut key_size);
                let smallest_key = raw_data(smallest_key, key_size);

                // get largest key inside file
                let largest_key = ffi::rocksdb_livefiles_largestkey(files, i, &raw mut key_size);
                let largest_key = raw_data(largest_key, key_size);

                livefiles.push(LiveFile {
                    column_family_name,
                    name,
                    directory,
                    size,
                    level,
                    start_key: smallest_key,
                    end_key: largest_key,
                    largest_seqno: ffi::rocksdb_livefiles_largest_seqno(files, i),
                    smallest_seqno: ffi::rocksdb_livefiles_smallest_seqno(files, i),
                    num_entries: ffi::rocksdb_livefiles_entries(files, i),
                    num_deletions: ffi::rocksdb_livefiles_deletions(files, i),
                });
            }

            livefiles
        }
    }
}

struct LiveFileGuard(*mut rocksdb_livefile_t);

impl LiveFileGuard {
    fn into_raw(mut self) -> *mut rocksdb_livefile_t {
        let ptr = self.0;
        self.0 = ptr::null_mut();
        ptr
    }
}

impl Drop for LiveFileGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                rocksdb_livefile_destroy(self.0);
            }
        }
    }
}

struct LiveFilesGuard(*mut rocksdb_livefiles_t);

impl LiveFilesGuard {
    fn into_raw(mut self) -> *mut rocksdb_livefiles_t {
        let ptr = self.0;
        self.0 = ptr::null_mut();
        ptr
    }
}

impl Drop for LiveFilesGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                rocksdb_livefiles_destroy(self.0);
            }
        }
    }
}

/// Metadata returned as output from [`Checkpoint::export_column_family`][export_column_family] and
/// used as input to [`DB::create_column_family_with_import`].
///
/// [export_column_family]: crate::checkpoint::Checkpoint::export_column_family
#[derive(Debug)]
pub struct ExportImportFilesMetaData {
    pub(crate) inner: *mut ffi::rocksdb_export_import_files_metadata_t,
}

impl ExportImportFilesMetaData {
    pub fn get_db_comparator_name(&self) -> String {
        unsafe {
            let c_name =
                ffi::rocksdb_export_import_files_metadata_get_db_comparator_name(self.inner);
            from_cstr_and_free(c_name)
        }
    }

    pub fn set_db_comparator_name(&mut self, name: &str) {
        let c_name = CString::new(name.as_bytes()).unwrap();
        unsafe {
            ffi::rocksdb_export_import_files_metadata_set_db_comparator_name(
                self.inner,
                c_name.as_ptr(),
            );
        };
    }

    pub fn get_files(&self) -> Vec<LiveFile> {
        unsafe {
            let livefiles_ptr = ffi::rocksdb_export_import_files_metadata_get_files(self.inner);
            let files = LiveFile::from_rocksdb_livefiles_ptr(livefiles_ptr);
            ffi::rocksdb_livefiles_destroy(livefiles_ptr);
            files
        }
    }

    pub fn set_files(&mut self, files: &[LiveFile]) -> Result<(), Error> {
        // Use a non-null empty pointer for zero-length keys
        static EMPTY: [u8; 0] = [];
        let empty_ptr = EMPTY.as_ptr() as *const libc::c_char;

        unsafe {
            let live_files = LiveFilesGuard(ffi::rocksdb_livefiles_create());

            for file in files {
                let live_file = LiveFileGuard(ffi::rocksdb_livefile_create());
                ffi::rocksdb_livefile_set_level(live_file.0, file.level);

                // SAFETY: C strings are copied inside the FFI layer so do not need to be kept alive
                let c_cf_name = CString::new(file.column_family_name.as_str()).map_err(|err| {
                    Error::new(format!("Unable to convert column family to CString: {err}"))
                })?;
                ffi::rocksdb_livefile_set_column_family_name(live_file.0, c_cf_name.as_ptr());

                let c_name = CString::new(file.name.as_str()).map_err(|err| {
                    Error::new(format!("Unable to convert file name to CString: {err}"))
                })?;
                ffi::rocksdb_livefile_set_name(live_file.0, c_name.as_ptr());

                let c_directory = CString::new(file.directory.as_str()).map_err(|err| {
                    Error::new(format!("Unable to convert directory to CString: {err}"))
                })?;
                ffi::rocksdb_livefile_set_directory(live_file.0, c_directory.as_ptr());

                ffi::rocksdb_livefile_set_size(live_file.0, file.size);

                let (start_key_ptr, start_key_len) = match &file.start_key {
                    None => (empty_ptr, 0),
                    Some(key) => (key.as_ptr() as *const libc::c_char, key.len()),
                };
                ffi::rocksdb_livefile_set_smallest_key(live_file.0, start_key_ptr, start_key_len);

                let (largest_key_ptr, largest_key_len) = match &file.end_key {
                    None => (empty_ptr, 0),
                    Some(key) => (key.as_ptr() as *const libc::c_char, key.len()),
                };
                ffi::rocksdb_livefile_set_largest_key(
                    live_file.0,
                    largest_key_ptr,
                    largest_key_len,
                );
                ffi::rocksdb_livefile_set_smallest_seqno(live_file.0, file.smallest_seqno);
                ffi::rocksdb_livefile_set_largest_seqno(live_file.0, file.largest_seqno);
                ffi::rocksdb_livefile_set_num_entries(live_file.0, file.num_entries);
                ffi::rocksdb_livefile_set_num_deletions(live_file.0, file.num_deletions);

                // moves ownership of live_files into live_file
                ffi::rocksdb_livefiles_add(live_files.0, live_file.into_raw());
            }

            // moves ownership of live_files into inner
            ffi::rocksdb_export_import_files_metadata_set_files(self.inner, live_files.into_raw());
            Ok(())
        }
    }
}

impl Default for ExportImportFilesMetaData {
    fn default() -> Self {
        let inner = unsafe { ffi::rocksdb_export_import_files_metadata_create() };
        assert!(
            !inner.is_null(),
            "Could not create rocksdb_export_import_files_metadata_t"
        );

        Self { inner }
    }
}

impl Drop for ExportImportFilesMetaData {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_export_import_files_metadata_destroy(self.inner);
        }
    }
}

unsafe impl Send for ExportImportFilesMetaData {}
unsafe impl Sync for ExportImportFilesMetaData {}

/// Converts a TTL to the `int` seconds count RocksDB's TTL API takes,
/// saturating instead of wrapping.
///
/// `Duration::as_secs` is a `u64`, so a plain `as i32` cast wraps: a TTL of
/// `Duration::from_secs(4_294_967_301)` (~136 years, i.e. "effectively never")
/// became `5`, and RocksDB then compaction-deleted the whole column family a few
/// seconds after the data was written.
///
/// Clamping to `i32::MAX` (~68 years) rather than mapping an over-large TTL to
/// RocksDB's never-expire sentinel (`ttl <= 0`, see `DBWithTTLImpl::IsStale`) is
/// deliberate: silently turning a finite TTL the caller asked for into "keep
/// forever" is a worse surprise than expiring it 68 years out, and `i32::MAX` is
/// the longest TTL the C API can express anyway.
fn ttl_to_seconds(ttl: Duration) -> c_int {
    c_int::try_from(ttl.as_secs()).unwrap_or(c_int::MAX)
}

/// Resolves a column family's TTL against the TTL the DB was opened with.
///
/// [`ColumnFamilyTtl::Disabled`] maps to the longest TTL the C API can express rather
/// than RocksDB's never-expire sentinel, for the reason on [`ttl_to_seconds`].
fn cf_ttl_to_seconds(ttl: ColumnFamilyTtl, db_ttl: Duration) -> c_int {
    match ttl {
        ColumnFamilyTtl::Disabled => c_int::MAX,
        ColumnFamilyTtl::Duration(duration) => ttl_to_seconds(duration),
        ColumnFamilyTtl::SameAsDb => ttl_to_seconds(db_ttl),
    }
}

/// Converts column family names to C strings, keeping the given order.
///
/// # Errors
///
/// Errors if a name contains an interior NUL byte.
fn convert_cf_names<I, N>(names: I) -> Result<Vec<(String, CString)>, Error>
where
    I: IntoIterator<Item = N>,
    N: AsRef<str>,
{
    names
        .into_iter()
        .map(|name| {
            let name = name.as_ref();
            let cname = CString::new(name).map_err(|err| {
                Error::new(format!(
                    "Failed to convert column family name to CString: {err}"
                ))
            })?;
            Ok((name.to_owned(), cname))
        })
        .collect()
}

/// The option count as the `int` the C API takes.
///
/// # Errors
///
/// Errors rather than truncating if there are more options than an `int` can count.
fn option_count(opts: &[(CString, CString)]) -> Result<c_int, Error> {
    c_int::try_from(opts.len())
        .map_err(|_| Error::new(format!("Too many options to set at once: {}", opts.len())))
}

fn convert_options(opts: &[(&str, &str)]) -> Result<Vec<(CString, CString)>, Error> {
    opts.iter()
        .map(|(name, value)| {
            let cname = match CString::new(name.as_bytes()) {
                Ok(cname) => cname,
                Err(e) => return Err(Error::new(format!("Invalid option name `{e}`"))),
            };
            let cvalue = match CString::new(value.as_bytes()) {
                Ok(cvalue) => cvalue,
                Err(e) => return Err(Error::new(format!("Invalid option value: `{e}`"))),
            };
            Ok((cname, cvalue))
        })
        .collect()
}

/// Borrows each key as the pointer and length pair the multi-get calls want.
///
/// The returned pointers borrow `keys`, so `keys` has to outlive them.
fn key_ptrs_and_sizes<K: AsRef<[u8]>>(keys: &[K]) -> (Vec<*const c_char>, Vec<usize>) {
    keys.iter()
        .map(|k| {
            let key = k.as_ref();
            (key.as_ptr().cast::<c_char>(), key.len())
        })
        .unzip()
}

/// The five output arrays `rocksdb_multi_get_*_with_ts` fills in.
///
/// Kept together because they are only ever allocated, passed and consumed as a set,
/// and because the unsafe `set_len` after the call has to cover all five or none.
struct MultiGetTsOut {
    values: Vec<*mut c_char>,
    values_sizes: Vec<usize>,
    timestamps: Vec<*mut c_char>,
    timestamps_sizes: Vec<usize>,
    errors: Vec<*mut c_char>,
}

impl MultiGetTsOut {
    fn with_capacity(n: usize) -> Self {
        Self {
            values: Vec::with_capacity(n),
            values_sizes: Vec::with_capacity(n),
            timestamps: Vec::with_capacity(n),
            timestamps_sizes: Vec::with_capacity(n),
            errors: Vec::with_capacity(n),
        }
    }

    /// Publishes the `n` entries RocksDB wrote into the spare capacity.
    ///
    /// # Safety
    ///
    /// `n` must be the key count the call was given, and the call must have returned.
    /// RocksDB writes every one of the five arrays at every index, either a pointer it
    /// allocated or null (c.cc:2694-2711), so no element is left uninitialised.
    unsafe fn assume_filled(&mut self, n: usize) {
        unsafe {
            self.values.set_len(n);
            self.values_sizes.set_len(n);
            self.timestamps.set_len(n);
            self.timestamps_sizes.set_len(n);
            self.errors.set_len(n);
        }
    }

    /// Takes ownership of every buffer RocksDB allocated and pairs it with its key.
    ///
    /// A null value with no error is a key that was not found. An error is reported
    /// with both buffers already null, so there is nothing to free on that path.
    fn into_results(self) -> Vec<Result<Option<TimestampedValue>, Error>> {
        self.values
            .into_iter()
            .zip(self.values_sizes)
            .zip(self.timestamps.into_iter().zip(self.timestamps_sizes))
            .zip(self.errors)
            .map(|(((value, vallen), (ts, tslen)), err)| {
                if !err.is_null() {
                    return Err(convert_rocksdb_error(err));
                }
                if value.is_null() {
                    return Ok(None);
                }
                // SAFETY: RocksDB allocated both with `CopyString` at the reported
                // lengths and nothing else frees them.
                unsafe {
                    Ok(Some(TimestampedValue {
                        value: CSlice::from_raw_parts(value, vallen),
                        timestamp: CSlice::from_raw_parts(ts, tslen),
                    }))
                }
            })
            .collect()
    }
}

pub(crate) fn convert_values(
    values: Vec<*mut c_char>,
    values_sizes: Vec<usize>,
    errors: Vec<*mut c_char>,
) -> Vec<Result<Option<Vec<u8>>, Error>> {
    values
        .into_iter()
        .zip(values_sizes)
        .zip(errors)
        .map(|((v, s), e)| {
            if e.is_null() {
                let value = unsafe { crate::ffi_util::raw_data(v, s) };
                unsafe {
                    ffi::rocksdb_free(v as *mut c_void);
                }
                Ok(value)
            } else {
                Err(convert_rocksdb_error(e))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::{ColumnFamilyDescriptor, DB, Options};

    /// One `rocksdb_create_iterators` call applies a single `ReadOptions` to
    /// every iterator it builds, and RocksDB's `DBIter` keeps raw `Slice*`
    /// into that object for the iterate bounds and read timestamps. So all the
    /// returned iterators have to keep the *same* options object alive, not a
    /// copy each and not none at all. Dropping it at the end of
    /// `create_iterators_cf` left those pointers dangling. See issue #660.
    #[test]
    fn raw_iterators_cf_share_one_live_readopts() {
        let dir = tempfile::Builder::new()
            .prefix("rocksdb-raw-iterators-cf-readopts")
            .tempdir()
            .unwrap();

        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        let db = DB::open_cf_descriptors(
            &opts,
            dir.path(),
            [
                ColumnFamilyDescriptor::new("first", Options::default()),
                ColumnFamilyDescriptor::new("second", Options::default()),
            ],
        )
        .unwrap();

        let first = db.cf_handle("first").unwrap();
        let second = db.cf_handle("second").unwrap();
        let iterators = db.raw_iterators_cf([&first, &second]).unwrap();
        assert_eq!(iterators.len(), 2);

        let shared = iterators[0].readopts_ptr();
        assert!(!shared.is_null());
        for iterator in &iterators {
            assert_eq!(
                iterator.readopts_ptr(),
                shared,
                "each iterator must hold the options object it was created from"
            );
        }
    }
}

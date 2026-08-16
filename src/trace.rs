//! Query tracing and trace replay.
//!
//! RocksDB can record the queries a DB serves into a trace file and later play
//! that file back against a DB. This module holds the parts of that feature
//! that are not methods on the DB: the options controlling what gets recorded
//! ([`TraceOptions`], [`BlockCacheTraceOptions`],
//! [`BlockCacheTraceWriterOptions`]), a reader for the raw records in a trace
//! file ([`TraceReader`]), and the replay side ([`Replayer`],
//! [`ReplayOptions`]).
//!
//! Wraps `include/rocksdb/trace_reader_writer.h`,
//! `include/rocksdb/utilities/replayer.h`, and the `TraceOptions` and
//! `TraceFilterType` declarations in `include/rocksdb/options.h`.

use crate::env::Env;
use crate::env_options::EnvOptions;
use crate::ffi_util::{raw_data_and_free, to_cpath};
use crate::{AsColumnFamilyRef, Error, ffi};
use libc::c_uchar;
use std::ffi::CStr;
use std::marker::PhantomData;
use std::ops::{BitOr, BitOrAssign};
use std::path::Path;
use std::ptr;

/// Which operation types tracing skips.
///
/// Every bit *excludes* an operation type from the trace, so
/// [`TraceFilter::empty`] traces everything and
/// `TraceFilter::GET | TraceFilter::MULTI_GET` traces everything except point
/// lookups. Filtering happens before sampling.
///
/// Mirrors `TraceFilterType` in `include/rocksdb/options.h`. Bits that this
/// version of RocksDB does not define are preserved rather than rejected, so a
/// value read back from [`TraceOptions::get_filter`] always round-trips.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TraceFilter(u64);

impl TraceFilter {
    /// Exclude nothing, the RocksDB default.
    pub const NONE: Self = Self(ffi::rocksdb_trace_filter_none as u64);
    /// Exclude `Get`.
    pub const GET: Self = Self(ffi::rocksdb_trace_filter_get as u64);
    /// Exclude writes.
    pub const WRITE: Self = Self(ffi::rocksdb_trace_filter_write as u64);
    /// Exclude `Iterator::Seek`.
    pub const ITERATOR_SEEK: Self = Self(ffi::rocksdb_trace_filter_iterator_seek as u64);
    /// Exclude `Iterator::SeekForPrev`.
    pub const ITERATOR_SEEK_FOR_PREV: Self =
        Self(ffi::rocksdb_trace_filter_iterator_seek_for_prev as u64);
    /// Exclude `MultiGet`.
    pub const MULTI_GET: Self = Self(ffi::rocksdb_trace_filter_multi_get as u64);

    /// A filter that excludes nothing, the same as [`TraceFilter::NONE`].
    pub const fn empty() -> Self {
        Self::NONE
    }

    /// The raw bitmask, as RocksDB stores it.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Builds a filter from a raw bitmask, keeping bits this crate does not
    /// know about instead of dropping them.
    pub const fn from_bits_retain(bits: u64) -> Self {
        Self(bits)
    }

    /// Whether every bit set in `other` is also set here. Always true when
    /// `other` is empty.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for TraceFilter {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for TraceFilter {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Controls what a query trace, IO trace, or block cache trace records.
///
/// Passed to the DB when a trace is started. Changing it afterwards has no
/// effect on a trace that is already running.
pub struct TraceOptions {
    pub(crate) inner: *mut ffi::rocksdb_trace_options_t,
}

impl Default for TraceOptions {
    fn default() -> Self {
        let opts = unsafe { ffi::rocksdb_trace_options_create() };
        assert!(!opts.is_null(), "Could not create RocksDB Trace Options");

        Self { inner: opts }
    }
}

impl Drop for TraceOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_trace_options_destroy(self.inner);
        }
    }
}

// SAFETY: the pointee is a plain options bag with no thread affinity, and the
// setters take `&mut self` so shared access cannot mutate it.
unsafe impl Send for TraceOptions {}
unsafe impl Sync for TraceOptions {}

impl TraceOptions {
    /// Stops the trace once the file reaches this many bytes, so a long trace
    /// cannot fill the disk.
    ///
    /// Default: 64 GiB
    pub fn set_max_trace_file_size(&mut self, size: u64) {
        unsafe {
            ffi::rocksdb_trace_options_set_max_trace_file_size(self.inner, size);
        }
    }

    /// Returns the current `max_trace_file_size` setting.
    ///
    /// See [`Self::set_max_trace_file_size`] for what this controls.
    pub fn get_max_trace_file_size(&self) -> u64 {
        unsafe { ffi::rocksdb_trace_options_get_max_trace_file_size(self.inner) }
    }

    /// Captures one request out of every `frequency`. Sampling runs after
    /// filtering.
    ///
    /// Default: 1, meaning capture every request.
    pub fn set_sampling_frequency(&mut self, frequency: u64) {
        unsafe {
            ffi::rocksdb_trace_options_set_sampling_frequency(self.inner, frequency);
        }
    }

    /// Returns the current `sampling_frequency` setting.
    ///
    /// See [`Self::set_sampling_frequency`] for what this controls.
    pub fn get_sampling_frequency(&self) -> u64 {
        unsafe { ffi::rocksdb_trace_options_get_sampling_frequency(self.inner) }
    }

    /// Sets which operation types to leave out of the trace. Note the
    /// inversion: a bit set here means that operation is *not* recorded.
    ///
    /// Default: [`TraceFilter::NONE`], record everything.
    pub fn set_filter(&mut self, filter: TraceFilter) {
        unsafe {
            ffi::rocksdb_trace_options_set_filter(self.inner, filter.bits());
        }
    }

    /// Returns the current `filter` setting.
    ///
    /// See [`Self::set_filter`] for what this controls.
    pub fn get_filter(&self) -> TraceFilter {
        TraceFilter::from_bits_retain(unsafe { ffi::rocksdb_trace_options_get_filter(self.inner) })
    }

    /// When true, write records land in the trace in the same order they land
    /// in the WAL. Costs some write throughput.
    ///
    /// Default: false, so traced writes may be ordered differently from the WAL.
    pub fn set_preserve_write_order(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_trace_options_set_preserve_write_order(self.inner, c_uchar::from(v));
        }
    }

    /// Returns the current `preserve_write_order` setting.
    ///
    /// See [`Self::set_preserve_write_order`] for what this controls.
    pub fn get_preserve_write_order(&self) -> bool {
        unsafe { ffi::rocksdb_trace_options_get_preserve_write_order(self.inner) != 0 }
    }
}

/// Controls how much of the block cache access stream a block cache trace
/// records.
///
/// This is the newer block cache tracing entry point and is paired with
/// [`BlockCacheTraceWriterOptions`]. The older entry point reuses
/// [`TraceOptions`] instead.
pub struct BlockCacheTraceOptions {
    pub(crate) inner: *mut ffi::rocksdb_block_cache_trace_options_t,
}

impl Default for BlockCacheTraceOptions {
    fn default() -> Self {
        let opts = unsafe { ffi::rocksdb_block_cache_trace_options_create() };
        assert!(
            !opts.is_null(),
            "Could not create RocksDB Block Cache Trace Options"
        );

        Self { inner: opts }
    }
}

impl Drop for BlockCacheTraceOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_block_cache_trace_options_destroy(self.inner);
        }
    }
}

// SAFETY: the pointee is a plain options bag with no thread affinity, and the
// setters take `&mut self` so shared access cannot mutate it.
unsafe impl Send for BlockCacheTraceOptions {}
unsafe impl Sync for BlockCacheTraceOptions {}

impl BlockCacheTraceOptions {
    /// Captures one block cache access out of every `frequency`.
    ///
    /// Default: 1, meaning capture every access.
    pub fn set_sampling_frequency(&mut self, frequency: u64) {
        unsafe {
            ffi::rocksdb_block_cache_trace_options_set_sampling_frequency(self.inner, frequency);
        }
    }

    /// Returns the current `sampling_frequency` setting.
    ///
    /// See [`Self::set_sampling_frequency`] for what this controls.
    pub fn get_sampling_frequency(&self) -> u64 {
        unsafe { ffi::rocksdb_block_cache_trace_options_get_sampling_frequency(self.inner) }
    }
}

/// Controls the file a block cache trace is written to.
///
/// Paired with [`BlockCacheTraceOptions`]: one says what to capture, this one
/// says how to store it.
pub struct BlockCacheTraceWriterOptions {
    pub(crate) inner: *mut ffi::rocksdb_block_cache_trace_writer_options_t,
}

impl Default for BlockCacheTraceWriterOptions {
    fn default() -> Self {
        let opts = unsafe { ffi::rocksdb_block_cache_trace_writer_options_create() };
        assert!(
            !opts.is_null(),
            "Could not create RocksDB Block Cache Trace Writer Options"
        );

        Self { inner: opts }
    }
}

impl Drop for BlockCacheTraceWriterOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_block_cache_trace_writer_options_destroy(self.inner);
        }
    }
}

// SAFETY: the pointee is a plain options bag with no thread affinity, and the
// setters take `&mut self` so shared access cannot mutate it.
unsafe impl Send for BlockCacheTraceWriterOptions {}
unsafe impl Sync for BlockCacheTraceWriterOptions {}

impl BlockCacheTraceWriterOptions {
    /// Stops the block cache trace once the file reaches this many bytes.
    ///
    /// Default: 64 GiB
    pub fn set_max_trace_file_size(&mut self, size: u64) {
        unsafe {
            ffi::rocksdb_block_cache_trace_writer_options_set_max_trace_file_size(self.inner, size);
        }
    }

    /// Returns the current `max_trace_file_size` setting.
    ///
    /// See [`Self::set_max_trace_file_size`] for what this controls.
    pub fn get_max_trace_file_size(&self) -> u64 {
        unsafe { ffi::rocksdb_block_cache_trace_writer_options_get_max_trace_file_size(self.inner) }
    }
}

/// Reads the raw, still-encoded records of a trace file one at a time.
///
/// This is the low level half of tracing: it hands back the bytes RocksDB
/// wrote, header and footer records included, and does not decode them into
/// queries. Use [`Replayer`] to run a trace against a DB instead.
///
/// Reading is sequential. [`reset`](Self::reset) rewinds to the start of the
/// file.
pub struct TraceReader {
    inner: *mut ffi::rocksdb_trace_reader_t,
    /// `read` dereferences a null file handle after the reader is closed, so
    /// this guards it. See [`Self::read`].
    closed: bool,
    /// The reader reads through this `Env`, so it has to outlive the reader.
    _env: Env,
}

// SAFETY: the pointee is a `FileTraceReader` holding a file handle, a read
// offset and a scratch buffer, none of which have thread affinity. Every
// method that touches them takes `&mut self`, so there is no `Sync`
// counterpart.
unsafe impl Send for TraceReader {}

impl TraceReader {
    /// Opens an existing trace file for reading through `env`.
    ///
    /// Fails if the file does not exist. Reads use a default [`EnvOptions`]. Use
    /// [`open_with_env_options`](Self::open_with_env_options) to control how the
    /// file is read.
    pub fn open<P: AsRef<Path>>(env: &Env, trace_path: P) -> Result<Self, Error> {
        Self::open_inner(env, None, trace_path)
    }

    /// Opens an existing trace file for reading through `env` and `env_opts`.
    ///
    /// `env_opts` only has to live for the call. RocksDB reads the options while
    /// opening the file and does not carry a rate limiter into the reader, so
    /// unlike [`SstFileWriter::create_with_env_options`] there is nothing here
    /// that outlives the borrow.
    ///
    /// [`SstFileWriter::create_with_env_options`]: crate::SstFileWriter::create_with_env_options
    pub fn open_with_env_options<P: AsRef<Path>>(
        env: &Env,
        env_opts: &EnvOptions,
        trace_path: P,
    ) -> Result<Self, Error> {
        Self::open_inner(env, Some(env_opts), trace_path)
    }

    fn open_inner<P: AsRef<Path>>(
        env: &Env,
        env_opts: Option<&EnvOptions>,
        trace_path: P,
    ) -> Result<Self, Error> {
        let c_path = to_cpath(trace_path)?;
        let env_opts = env_opts.map_or(ptr::null(), EnvOptions::as_ptr);
        let reader = unsafe {
            ffi_try!(ffi::rocksdb_trace_reader_create(
                env.0.inner,
                env_opts,
                c_path.as_ptr(),
            ))
        };

        if reader.is_null() {
            return Err(Error::new("Could not create trace reader.".to_owned()));
        }

        Ok(Self {
            inner: reader,
            closed: false,
            _env: env.clone(),
        })
    }

    /// Reads the next record, or `Ok(None)` at the end of the file.
    ///
    /// End of stream is unambiguous here. `FileTraceReader::Read` reports it as
    /// `Status::Incomplete`, and `rocksdb_trace_reader_read` in `db/c.cc`
    /// translates that specific status into a null return with no error string
    /// set, which is the only way a successful call can produce null: a record
    /// always carries a fixed size header, so a real record is never zero
    /// bytes, and a short read is reported as `Corruption` instead. Any other
    /// failure comes back as `Err`.
    ///
    /// Returns an error if the reader has been closed, because the C++ `Read`
    /// dereferences the file handle that [`close`](Self::close) released
    /// without checking it first.
    pub fn read(&mut self) -> Result<Option<Vec<u8>>, Error> {
        if self.closed {
            return Err(Error::new("TraceReader is closed.".to_owned()));
        }

        let mut size: usize = 0;
        let data = unsafe { ffi_try!(ffi::rocksdb_trace_reader_read(self.inner, &raw mut size)) };
        // The buffer comes from `CopyString` in `db/c.cc`, which `malloc`s it
        // and hands ownership over, so it is copied out and freed here.
        Ok(unsafe { raw_data_and_free(data, size) })
    }

    /// Rewinds to the start of the trace file so it can be read again.
    ///
    /// Fails if the reader has been closed.
    pub fn reset(&mut self) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_trace_reader_reset(self.inner));
        }
        Ok(())
    }

    /// Releases the underlying file handle and reports any error doing so.
    ///
    /// Dropping the reader closes it too, so this is only needed to see a close
    /// failure. Calling it more than once is a no-op, and reading afterwards
    /// returns an error.
    pub fn close(&mut self) -> Result<(), Error> {
        if self.closed {
            return Ok(());
        }
        // Marked closed before the call because `FileTraceReader::Close`
        // releases the file handle whatever it ends up returning.
        self.closed = true;
        unsafe {
            ffi_try!(ffi::rocksdb_trace_reader_close(self.inner));
        }
        Ok(())
    }
}

impl Drop for TraceReader {
    fn drop(&mut self) {
        // `rocksdb_trace_reader_destroy` deletes the `TraceReader`, and
        // `~FileTraceReader` closes it, so an explicit close first would only
        // repeat work. Close is idempotent either way.
        unsafe { ffi::rocksdb_trace_reader_destroy(self.inner) }
    }
}

/// Controls the pace and parallelism of a [`Replayer::replay`] run.
pub struct ReplayOptions {
    pub(crate) inner: *mut ffi::rocksdb_replay_options_t,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        let opts = unsafe { ffi::rocksdb_replay_options_create() };
        assert!(!opts.is_null(), "Could not create RocksDB Replay Options");

        Self { inner: opts }
    }
}

impl Drop for ReplayOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_replay_options_destroy(self.inner);
        }
    }
}

// SAFETY: the pointee is a plain options bag with no thread affinity, and the
// setters take `&mut self` so shared access cannot mutate it.
unsafe impl Send for ReplayOptions {}
unsafe impl Sync for ReplayOptions {}

impl ReplayOptions {
    /// Number of threads issuing the replayed operations. 0 and 1 both mean
    /// single threaded.
    ///
    /// Default: 1
    pub fn set_num_threads(&mut self, num_threads: u32) {
        unsafe {
            ffi::rocksdb_replay_options_set_num_threads(self.inner, num_threads);
        }
    }

    /// Returns the current `num_threads` setting.
    ///
    /// See [`Self::set_num_threads`] for what this controls.
    pub fn get_num_threads(&self) -> u32 {
        unsafe { ffi::rocksdb_replay_options_get_num_threads(self.inner) }
    }

    /// Scales the recorded delay between operations. Above 1.0 replays faster
    /// than real time, between 0.0 and 1.0 slower, 1.0 matches the original
    /// rate. [`Replayer::replay`] rejects values at or below 0.0.
    ///
    /// Default: 1.0
    pub fn set_fast_forward(&mut self, fast_forward: f64) {
        unsafe {
            ffi::rocksdb_replay_options_set_fast_forward(self.inner, fast_forward);
        }
    }

    /// Returns the current `fast_forward` setting.
    ///
    /// See [`Self::set_fast_forward`] for what this controls.
    pub fn get_fast_forward(&self) -> f64 {
        unsafe { ffi::rocksdb_replay_options_get_fast_forward(self.inner) }
    }
}

/// Plays a trace file back against a DB.
///
/// The replayer keeps raw pointers to the DB and to the column family handles
/// it was built from and executes operations through them, so `'a` ties it to
/// the DB it came from.
///
/// [`prepare`](Self::prepare) must succeed before [`replay`](Self::replay),
/// which otherwise fails with `Result incomplete`. Preparing again rewinds the
/// trace, which is also how to replay a second time after a run has consumed
/// it.
pub struct Replayer<'a> {
    inner: *mut ffi::rocksdb_replayer_t,
    _db: PhantomData<&'a ()>,
}

// SAFETY: the pointee is a `ReplayerImpl`, which holds no thread-affine state:
// its multi-threaded mode builds and joins a thread pool inside the `Replay`
// call rather than keeping one. The replay cursor it mutates is reached only
// through `&mut self`, so there is no `Sync` counterpart.
unsafe impl Send for Replayer<'_> {}

impl Replayer<'_> {
    /// Builds RocksDB's default replayer for `trace_path`.
    ///
    /// An empty `column_families` means the DB's default column family. A
    /// trace record naming a column family outside the list fails replay with
    /// `Corruption: Invalid Column Family ID.`, so pass every column family the
    /// trace touched. A null `env` falls back to the DB's own `Env`, and a null
    /// `env_options` to RocksDB's defaults.
    ///
    /// # Safety
    ///
    /// `db` must be a live `rocksdb_t`, the column family handles must belong
    /// to it, and `env` must be null or a live `rocksdb_env_t`. `'a` must not
    /// outlive any of them.
    pub(crate) unsafe fn create_default<'cf, W, I>(
        db: *mut ffi::rocksdb_t,
        column_families: I,
        env: *mut ffi::rocksdb_env_t,
        env_options: *const ffi::rocksdb_envoptions_t,
        trace_path: &CStr,
    ) -> Result<Self, Error>
    where
        W: AsColumnFamilyRef + 'cf,
        I: IntoIterator<Item = &'cf W>,
    {
        let mut cf_handles: Vec<_> = column_families
            .into_iter()
            .map(AsColumnFamilyRef::inner)
            .collect();
        let replayer = unsafe {
            ffi_try!(ffi::rocksdb_new_default_replayer(
                db,
                cf_handles.as_mut_ptr(),
                cf_handles.len(),
                env,
                env_options,
                trace_path.as_ptr(),
            ))
        };

        if replayer.is_null() {
            return Err(Error::new("Could not create replayer.".to_owned()));
        }

        Ok(Self {
            inner: replayer,
            _db: PhantomData,
        })
    }

    /// Reads the trace header and positions the replayer at the first record.
    ///
    /// Required before [`replay`](Self::replay). Calling it again rewinds the
    /// trace and clears the end-of-trace state.
    pub fn prepare(&mut self) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_replayer_prepare(self.inner));
        }
        Ok(())
    }

    /// Replays every remaining record against the DB, honouring the recorded
    /// delay between them as scaled by `options`.
    ///
    /// Blocks until the trace is exhausted, then returns `Ok`. Per-operation
    /// results are discarded: `db/c.cc` passes no result callback, so only an
    /// overall status comes back. A record whose type this RocksDB build cannot
    /// execute is skipped rather than failing the run. Once the run reaches the
    /// end of the trace, further calls fail with `Result incomplete` until
    /// [`prepare`](Self::prepare) rewinds it.
    ///
    /// Fails with `Result incomplete` if `prepare` has not succeeded, and with
    /// `Invalid argument` if [`ReplayOptions::set_fast_forward`] was given a
    /// value at or below 0.0.
    pub fn replay(&mut self, options: &ReplayOptions) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_replayer_replay(
                self.inner,
                options.inner.cast_const(),
            ));
        }
        Ok(())
    }

    /// The timestamp recorded in the trace header, in microseconds, which is
    /// when tracing started.
    ///
    /// Returns 0 until [`prepare`](Self::prepare) has succeeded, since that is
    /// what reads the header.
    pub fn header_timestamp(&self) -> u64 {
        unsafe { ffi::rocksdb_replayer_get_header_timestamp(self.inner.cast_const()) }
    }
}

impl Drop for Replayer<'_> {
    fn drop(&mut self) {
        unsafe { ffi::rocksdb_replayer_destroy(self.inner) }
    }
}

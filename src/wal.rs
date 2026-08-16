//! Write ahead log inspection.
//!
//! RocksDB can list the WAL files backing a DB, both the live ones in the DB
//! directory and the ones already moved to the archive. [`WalFiles`] is that
//! listing, [`WalFile`] is one entry in it, and [`OwnedWalFile`] is the single
//! file returned when asking only about the WAL currently being written.
//!
//! [`WalReadOptions`] belongs to the other half of the feature, reading the
//! updates recorded in the WAL back out with a
//! [`DBWALIterator`](crate::DBWALIterator).
//!
//! Wraps `WalFile`, `WalFileType` and `TransactionLogIterator::ReadOptions`
//! from `include/rocksdb/transaction_log.h`.

use crate::ffi;
use libc::c_uchar;
use std::borrow::Cow;
use std::ffi::CStr;
use std::fmt;
use std::iter::FusedIterator;
use std::marker::PhantomData;
use std::ops::Range;

/// Where a WAL file lives.
///
/// Mirrors `rocksdb::WalFileType` from `include/rocksdb/transaction_log.h`.
/// Values RocksDB adds in future versions decode as [`WalFileType::Unknown`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum WalFileType {
    /// Moved out of the main DB directory into the archive because it is no
    /// longer live. Cleaned up according to
    /// [`set_wal_size_limit_mb`](crate::Options::set_wal_size_limit_mb) and
    /// [`set_wal_ttl_seconds`](crate::Options::set_wal_ttl_seconds).
    ArchivedLogFile,
    /// Still live, in the main DB directory.
    AliveLogFile,
    /// A value this build of the crate does not know about.
    Unknown,
}

impl From<i32> for WalFileType {
    fn from(value: i32) -> Self {
        const ARCHIVED: i32 = ffi::rocksdb_wal_file_type_archived_log as i32;
        const ALIVE: i32 = ffi::rocksdb_wal_file_type_alive_log as i32;
        match value {
            ARCHIVED => WalFileType::ArchivedLogFile,
            ALIVE => WalFileType::AliveLogFile,
            _ => WalFileType::Unknown,
        }
    }
}

impl WalFileType {
    /// The variant name, for logs and error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            WalFileType::ArchivedLogFile => "ArchivedLogFile",
            WalFileType::AliveLogFile => "AliveLogFile",
            WalFileType::Unknown => "Unknown",
        }
    }
}

/// Options for streaming updates out of the WAL.
pub struct WalReadOptions {
    pub(crate) inner: *mut ffi::rocksdb_wal_readoptions_t,
}

impl Default for WalReadOptions {
    fn default() -> Self {
        let opts = unsafe { ffi::rocksdb_wal_readoptions_create() };
        assert!(!opts.is_null(), "Could not create RocksDB WAL Read Options");

        Self { inner: opts }
    }
}

impl Drop for WalReadOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_wal_readoptions_destroy(self.inner);
        }
    }
}

// SAFETY: the pointee is a plain options bag with no thread affinity, and the
// setters take `&mut self` so shared access cannot mutate it.
unsafe impl Send for WalReadOptions {}
unsafe impl Sync for WalReadOptions {}

impl WalReadOptions {
    /// Whether to check each WAL record's checksum while reading it. Turning
    /// this off trades corruption detection for speed.
    ///
    /// Default: true
    pub fn set_verify_checksums(&mut self, verify_checksums: bool) {
        unsafe {
            ffi::rocksdb_wal_readoptions_set_verify_checksums(
                self.inner,
                c_uchar::from(verify_checksums),
            );
        }
    }

    /// Returns the current `verify_checksums` setting.
    ///
    /// See [`Self::set_verify_checksums`] for what this controls.
    pub fn get_verify_checksums(&self) -> bool {
        unsafe { ffi::rocksdb_wal_readoptions_get_verify_checksums(self.inner) != 0 }
    }
}

/// A DB's WAL files, sorted oldest first.
///
/// This is a snapshot taken when the listing was made. It does not pin
/// anything, so a file listed here can still be archived or deleted by a
/// background job.
pub struct WalFiles {
    inner: *mut ffi::rocksdb_wal_files_t,
    /// Cached because the underlying vector is filled in once and never
    /// resized, and every bounds check would otherwise cost an FFI call.
    len: usize,
}

impl WalFiles {
    /// Takes ownership of a raw WAL file listing.
    ///
    /// # Safety
    ///
    /// `ptr` must be a non-null handle from `rocksdb_get_sorted_wal_files`
    /// that nothing else owns. It is destroyed when the returned value drops.
    pub(crate) unsafe fn from_ptr(ptr: *mut ffi::rocksdb_wal_files_t) -> Self {
        let len = unsafe { ffi::rocksdb_wal_files_count(ptr.cast_const()) };
        Self { inner: ptr, len }
    }

    /// Number of WAL files listed.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether no WAL files are listed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Borrows the entry at `index`, or `None` if out of range.
    pub fn get(&self, index: usize) -> Option<WalFile<'_>> {
        // `rocksdb_wal_files_get_wal_file` hands back a pointer straight into
        // the parent's `std::vector<rocksdb_wal_file_t>`, and already returns
        // null for an out of range index (see `db/c.cc`). Nothing is allocated,
        // so the result must not be passed to `rocksdb_wal_file_destroy`.
        let inner = unsafe { ffi::rocksdb_wal_files_get_wal_file(self.inner.cast_const(), index) };
        if inner.is_null() {
            return None;
        }
        Some(WalFile {
            inner,
            _files: PhantomData,
        })
    }

    /// Iterates the WAL files oldest first.
    pub fn iter(&self) -> WalFilesIter<'_> {
        WalFilesIter {
            files: self,
            range: 0..self.len,
        }
    }
}

impl Drop for WalFiles {
    fn drop(&mut self) {
        unsafe { ffi::rocksdb_wal_files_destroy(self.inner) }
    }
}

impl fmt::Debug for WalFiles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<'a> IntoIterator for &'a WalFiles {
    type Item = WalFile<'a>;
    type IntoIter = WalFilesIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

// SAFETY: the pointee is a `std::vector<rocksdb_wal_file_t>` of plain values
// that RocksDB fills in once and this type never mutates. Reads through `&self`
// cannot race, so both moving it between threads and sharing it across them are
// sound.
unsafe impl Send for WalFiles {}
unsafe impl Sync for WalFiles {}

/// Iterator over the entries of a [`WalFiles`] listing.
pub struct WalFilesIter<'a> {
    files: &'a WalFiles,
    range: Range<usize>,
}

impl<'a> Iterator for WalFilesIter<'a> {
    type Item = WalFile<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.files.get(self.range.next()?)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.range.size_hint()
    }
}

impl DoubleEndedIterator for WalFilesIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.files.get(self.range.next_back()?)
    }
}

impl ExactSizeIterator for WalFilesIter<'_> {}

impl FusedIterator for WalFilesIter<'_> {}

/// One WAL file in a [`WalFiles`] listing.
///
/// A borrowed view. The values live inside the parent listing and are freed
/// with it.
#[derive(Copy, Clone)]
pub struct WalFile<'a> {
    inner: *const ffi::rocksdb_wal_file_t,
    _files: PhantomData<&'a WalFiles>,
}

// SAFETY: the pointee is a plain struct of a `std::string` and four integers
// that RocksDB fills in once, and this type only reads it. It stays alive for
// `'a`, so it is sound both to move a view between threads and to read one from
// several at once.
unsafe impl Send for WalFile<'_> {}
unsafe impl Sync for WalFile<'_> {}

impl<'a> WalFile<'a> {
    /// The file's path relative to the main DB directory, for example
    /// `/000003.log` for a live file or `/archive/000003.log` for an archived
    /// one.
    ///
    /// Borrowed from the `std::string` inside the parent: the C API returns
    /// `path_name.c_str()`, not a copy, so nothing is allocated or freed here.
    pub fn path_name(self) -> &'a [u8] {
        unsafe { CStr::from_ptr(ffi::rocksdb_wal_file_path_name(self.inner).cast()) }.to_bytes()
    }

    /// [`path_name`](Self::path_name) as UTF-8, replacing invalid sequences.
    pub fn path_name_lossy(self) -> Cow<'a, str> {
        String::from_utf8_lossy(self.path_name())
    }

    /// The file's log number, RocksDB's primary identifier for it. It grows
    /// with creation time, so a higher number means a newer file.
    pub fn log_number(self) -> u64 {
        unsafe { ffi::rocksdb_wal_file_log_number(self.inner) }
    }

    /// Position of the last flushed write in the file, which for a recycled WAL
    /// is usually less than the file's size on disk.
    pub fn size_file_bytes(self) -> u64 {
        unsafe { ffi::rocksdb_wal_file_size_file_bytes(self.inner) }
    }

    /// Sequence number of the first write batch in the file.
    ///
    /// Always 0 for the file [`get_current_wal_file`] returns. RocksDB reads the first
    /// batch to find this, which it only does for the files it has stopped writing to, so
    /// the live WAL is reported with a placeholder rather than a real sequence number.
    ///
    /// [`get_current_wal_file`]: crate::DBCommon::get_current_wal_file
    pub fn start_sequence(self) -> u64 {
        unsafe { ffi::rocksdb_wal_file_start_sequence(self.inner) }
    }

    /// Whether the file is still live or has been archived.
    pub fn file_type(self) -> WalFileType {
        WalFileType::from(unsafe { ffi::rocksdb_wal_file_type(self.inner) })
    }
}

impl fmt::Debug for WalFile<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WalFile")
            .field("path_name", &self.path_name_lossy())
            .field("log_number", &self.log_number())
            .field("size_file_bytes", &self.size_file_bytes())
            .field("start_sequence", &self.start_sequence())
            .field("file_type", &self.file_type())
            .finish_non_exhaustive()
    }
}

/// A WAL file handle that owns its own copy of the metadata.
///
/// This is what asking for the current WAL file gives back: `db/c.cc` allocates
/// a fresh `rocksdb_wal_file_t` for it rather than pointing into a listing, so
/// it has to be freed on its own. Borrow it as a [`WalFile`] with
/// [`as_wal_file`](Self::as_wal_file) to share code with entries that came out
/// of a [`WalFiles`] listing.
pub struct OwnedWalFile {
    inner: *mut ffi::rocksdb_wal_file_t,
}

// SAFETY: the pointee is a plain struct of a `std::string` and four integers
// that RocksDB fills in once, and this type only reads it.
unsafe impl Send for OwnedWalFile {}
unsafe impl Sync for OwnedWalFile {}

impl OwnedWalFile {
    /// Takes ownership of a raw WAL file handle.
    ///
    /// # Safety
    ///
    /// `ptr` must be a non-null handle from `rocksdb_get_current_wal_file`
    /// that nothing else owns. It is destroyed when the returned value drops,
    /// so do not pass a pointer borrowed from a `rocksdb_wal_files_t`.
    pub(crate) unsafe fn from_ptr(ptr: *mut ffi::rocksdb_wal_file_t) -> Self {
        Self { inner: ptr }
    }

    /// Borrows this handle as a [`WalFile`] view.
    pub fn as_wal_file(&self) -> WalFile<'_> {
        WalFile {
            inner: self.inner.cast_const(),
            _files: PhantomData,
        }
    }

    /// See [`WalFile::path_name`].
    pub fn path_name(&self) -> &[u8] {
        self.as_wal_file().path_name()
    }

    /// See [`WalFile::path_name_lossy`].
    pub fn path_name_lossy(&self) -> Cow<'_, str> {
        self.as_wal_file().path_name_lossy()
    }

    /// See [`WalFile::log_number`].
    pub fn log_number(&self) -> u64 {
        self.as_wal_file().log_number()
    }

    /// See [`WalFile::size_file_bytes`].
    pub fn size_file_bytes(&self) -> u64 {
        self.as_wal_file().size_file_bytes()
    }

    /// See [`WalFile::start_sequence`].
    pub fn start_sequence(&self) -> u64 {
        self.as_wal_file().start_sequence()
    }

    /// See [`WalFile::file_type`].
    pub fn file_type(&self) -> WalFileType {
        self.as_wal_file().file_type()
    }
}

impl Drop for OwnedWalFile {
    fn drop(&mut self) {
        unsafe { ffi::rocksdb_wal_file_destroy(self.inner) }
    }
}

impl fmt::Debug for OwnedWalFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedWalFile")
            .field("path_name", &self.path_name_lossy())
            .field("log_number", &self.log_number())
            .field("size_file_bytes", &self.size_file_bytes())
            .field("start_sequence", &self.start_sequence())
            .field("file_type", &self.file_type())
            .finish_non_exhaustive()
    }
}

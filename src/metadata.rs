//! Per-level and per-file LSM metadata, plus the live files storage info API.
//!
//! Two independent trees live here:
//!
//! * [`LevelMetaData`] / [`SstFileMetaData`] expand
//!   [`ColumnFamilyMetaData`](crate::ColumnFamilyMetaData) into the levels and
//!   SST files it summarises, optionally filtered by
//!   [`ColumnFamilyMetaDataOptions`].
//! * [`LiveFilesStorageInfo`] lists every file needed to make a consistent copy
//!   of the DB (SSTs, blobs, WALs, MANIFEST, CURRENT, OPTIONS), which is what
//!   checkpoint and backup are built on.

use crate::ffi;
use crate::ffi_util::{from_cstr_and_free, raw_data, raw_data_and_free};
use libc::{c_char, c_uchar};
use std::borrow::Cow;
use std::ffi::CStr;
use std::fmt;
use std::iter::FusedIterator;
use std::ops::Range;
use std::sync::Arc;

/// Reads a borrowed, NUL-terminated C string as bytes without copying.
///
/// The returned lifetime is chosen by the caller, so only call this with a
/// pointer that is known to stay valid and immutable for that long.
unsafe fn borrowed_cstr<'a>(ptr: *const c_char) -> &'a [u8] {
    if ptr.is_null() {
        return &[];
    }
    unsafe { CStr::from_ptr(ptr.cast()) }.to_bytes()
}

/// Owns a `rocksdb_column_family_metadata_t` so that the level and SST handles
/// carved out of it stay valid.
///
/// `rocksdb_level_metadata_t` and `rocksdb_sst_file_metadata_t` are bare
/// pointers into the parent's `std::vector`s (see `db/c.cc`), and `c.h` states
/// the child handles must be released before the parent. Holding this behind an
/// `Arc` in every child enforces that ordering instead of leaving it to the
/// caller.
struct CfMetaDataRoot {
    inner: *mut ffi::rocksdb_column_family_metadata_t,
}

impl Drop for CfMetaDataRoot {
    fn drop(&mut self) {
        unsafe { ffi::rocksdb_column_family_metadata_destroy(self.inner) }
    }
}

// SAFETY: the pointee is a snapshot that RocksDB fills in once and never
// touches again; it shares no state with the DB. Nothing here mutates it
// through a shared reference, so it is safe both to move between threads and to
// read from several at once.
unsafe impl Send for CfMetaDataRoot {}
unsafe impl Sync for CfMetaDataRoot {}

/// The metadata that describes one level of a column family's LSM tree.
///
/// Obtained from a [`ColumnFamilyMetaData`](crate::ColumnFamilyMetaData) query.
/// The value is a snapshot taken when the metadata was collected and does not
/// track later compactions or flushes.
pub struct LevelMetaData {
    inner: *mut ffi::rocksdb_level_metadata_t,
    /// `None` when the handle merely borrows a caller-owned parent.
    root: Option<Arc<CfMetaDataRoot>>,
}

impl LevelMetaData {
    /// The level this metadata describes, for example 0 for L0.
    ///
    /// Do not assume this equals the index the value was read at: the filtered
    /// `GetColumnFamilyMetaData` overload skips levels with no matching files,
    /// so indices are not level numbers.
    pub fn level(&self) -> i32 {
        unsafe { ffi::rocksdb_level_metadata_get_level(self.inner) }
    }

    /// Total size of the level in bytes, the sum of its files' sizes.
    pub fn size(&self) -> u64 {
        unsafe { ffi::rocksdb_level_metadata_get_size(self.inner) }
    }

    /// Number of SST files in this level.
    pub fn file_count(&self) -> usize {
        unsafe { ffi::rocksdb_level_metadata_get_file_count(self.inner) }
    }

    /// Metadata for the `index`th SST file of this level, or `None` if `index`
    /// is out of range.
    ///
    /// For level 0 the files are ordered most-recently-updated first; for level
    /// 1 and above they are ordered by increasing key range.
    pub fn sst_file(&self, index: usize) -> Option<SstFileMetaData> {
        let inner = unsafe { ffi::rocksdb_level_metadata_get_sst_file_metadata(self.inner, index) };
        if inner.is_null() {
            return None;
        }
        Some(SstFileMetaData {
            inner,
            _root: self.root.clone(),
        })
    }

    /// Iterates the SST files of this level in order.
    pub fn sst_files(&self) -> impl Iterator<Item = SstFileMetaData> + '_ {
        (0..self.file_count()).filter_map(|index| self.sst_file(index))
    }
}

impl Drop for LevelMetaData {
    fn drop(&mut self) {
        // Frees only the handle. The `LevelMetaData` it points at belongs to the
        // parent column family metadata.
        unsafe { ffi::rocksdb_level_metadata_destroy(self.inner) }
    }
}

impl fmt::Debug for LevelMetaData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LevelMetaData")
            .field("level", &self.level())
            .field("size", &self.size())
            .field("file_count", &self.file_count())
            .finish()
    }
}

// SAFETY: the handle and the snapshot behind it are plain data with no ties to
// the originating thread, and the parent stays alive through `root`.
// `Sync` is also sound (every accessor is a read), but is deliberately not
// implemented until something needs it.
unsafe impl Send for LevelMetaData {}

/// The metadata that describes a single SST file within a level.
pub struct SstFileMetaData {
    inner: *mut ffi::rocksdb_sst_file_metadata_t,
    /// Held, never read: keeps the parent column family metadata alive.
    _root: Option<Arc<CfMetaDataRoot>>,
}

impl SstFileMetaData {
    /// The file name within its directory, for example `123456.sst`.
    pub fn relative_filename(&self) -> String {
        // `strdup`ed by the C API, so this frees it.
        unsafe {
            from_cstr_and_free(ffi::rocksdb_sst_file_metadata_get_relative_filename(
                self.inner,
            ))
        }
    }

    /// The directory holding the file, without a trailing `/`. This is a DB path
    /// or column family path, not necessarily the main DB directory.
    pub fn directory(&self) -> String {
        // `strdup`ed by the C API, so this frees it.
        unsafe { from_cstr_and_free(ffi::rocksdb_sst_file_metadata_get_directory(self.inner)) }
    }

    /// File size in bytes.
    pub fn size(&self) -> u64 {
        unsafe { ffi::rocksdb_sst_file_metadata_get_size(self.inner) }
    }

    /// Smallest user key in the file. Empty if the file's smallest key is empty.
    pub fn smallest_key(&self) -> Vec<u8> {
        let mut len: usize = 0;
        // `malloc`ed by `CopyString`, so this copies and frees.
        let ptr =
            unsafe { ffi::rocksdb_sst_file_metadata_get_smallestkey(self.inner, &raw mut len) };
        unsafe { raw_data_and_free(ptr, len) }.unwrap_or_default()
    }

    /// Largest user key in the file. Empty if the file's largest key is empty.
    pub fn largest_key(&self) -> Vec<u8> {
        let mut len: usize = 0;
        // `malloc`ed by `CopyString`, so this copies and frees.
        let ptr =
            unsafe { ffi::rocksdb_sst_file_metadata_get_largestkey(self.inner, &raw mut len) };
        unsafe { raw_data_and_free(ptr, len) }.unwrap_or_default()
    }
}

impl Drop for SstFileMetaData {
    fn drop(&mut self) {
        // Frees only the handle. The `SstFileMetaData` it points at belongs to
        // the parent column family metadata.
        unsafe { ffi::rocksdb_sst_file_metadata_destroy(self.inner) }
    }
}

impl fmt::Debug for SstFileMetaData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SstFileMetaData")
            .field("relative_filename", &self.relative_filename())
            .field("directory", &self.directory())
            .field("size", &self.size())
            .finish_non_exhaustive()
    }
}

// SAFETY: see the note on `LevelMetaData`.
unsafe impl Send for SstFileMetaData {}

/// Takes ownership of a column family metadata object and returns its levels.
///
/// The parent is destroyed once the last returned [`LevelMetaData`] and every
/// [`SstFileMetaData`] derived from it are dropped, so nothing can dangle.
///
/// # Safety
///
/// `ptr` must be a live `rocksdb_column_family_metadata_t` that nothing else
/// owns or will destroy.
pub(crate) unsafe fn levels_from_cf_metadata_owned(
    ptr: *mut ffi::rocksdb_column_family_metadata_t,
) -> Vec<LevelMetaData> {
    if ptr.is_null() {
        return Vec::new();
    }
    let root = Arc::new(CfMetaDataRoot { inner: ptr });
    unsafe { collect_levels(ptr, Some(root)) }
}

unsafe fn collect_levels(
    ptr: *mut ffi::rocksdb_column_family_metadata_t,
    root: Option<Arc<CfMetaDataRoot>>,
) -> Vec<LevelMetaData> {
    if ptr.is_null() {
        return Vec::new();
    }
    let count = unsafe { ffi::rocksdb_column_family_metadata_get_level_count(ptr) };
    let mut levels = Vec::with_capacity(count);
    for i in 0..count {
        let inner = unsafe { ffi::rocksdb_column_family_metadata_get_level_metadata(ptr, i) };
        if inner.is_null() {
            continue;
        }
        levels.push(LevelMetaData {
            inner,
            root: root.clone(),
        });
    }
    levels
}

/// Filters applied when collecting column family metadata.
///
/// Both filters narrow which SST files are reported:
///
/// * `level` picks a single LSM level. The default, `-1`, reports every level.
/// * `start_key` and `end_key` bound a user key range. A file is reported when
///   its key range overlaps the bound, so a file that merely straddles the
///   boundary is included. An unset bound is open-ended on that side.
///
/// The filtered query also drops levels that end up with no files, so the
/// resulting `Vec<LevelMetaData>` is not indexed by level number. Read
/// [`LevelMetaData::level`] to find out which level a value describes.
///
/// The reported `size` and `file_count` cover only the files that passed the
/// filter, not the whole column family.
pub struct ColumnFamilyMetaDataOptions {
    pub(crate) inner: *mut ffi::rocksdb_column_family_metadata_options_t,
}

impl ColumnFamilyMetaDataOptions {
    /// Creates options that filter nothing: all levels, unbounded key range.
    pub fn new() -> Self {
        Self {
            inner: unsafe { ffi::rocksdb_column_family_metadata_options_create() },
        }
    }

    /// Restricts the query to a single level. Pass `-1` to report every level.
    pub fn set_level(&mut self, level: i32) {
        unsafe { ffi::rocksdb_column_family_metadata_options_set_level(self.inner, level) }
    }

    /// Returns the level filter. `-1` means every level.
    pub fn get_level(&self) -> i32 {
        unsafe { ffi::rocksdb_column_family_metadata_options_get_level(self.inner) }
    }

    /// Sets the inclusive lower bound of the user key range to report.
    pub fn set_start_key(&mut self, key: impl AsRef<[u8]>) {
        let key = key.as_ref();
        unsafe {
            ffi::rocksdb_column_family_metadata_options_set_start_key(
                self.inner,
                key.as_ptr().cast::<c_char>(),
                key.len(),
            );
        }
    }

    /// Removes the lower bound, leaving the range open on that side.
    pub fn clear_start_key(&mut self) {
        unsafe {
            ffi::rocksdb_column_family_metadata_options_set_start_key(
                self.inner,
                std::ptr::null(),
                0,
            );
        }
    }

    /// Returns the lower bound, or `None` if unset.
    ///
    /// The C API hands back a borrowed pointer into a `std::string` owned by the
    /// options object, so the bytes are copied here rather than freed.
    pub fn get_start_key(&self) -> Option<Vec<u8>> {
        let mut len: usize = 0;
        let ptr = unsafe {
            ffi::rocksdb_column_family_metadata_options_get_start_key(
                self.inner.cast_const(),
                &raw mut len,
            )
        };
        unsafe { raw_data(ptr, len) }
    }

    /// Sets the inclusive upper bound of the user key range to report.
    pub fn set_end_key(&mut self, key: impl AsRef<[u8]>) {
        let key = key.as_ref();
        unsafe {
            ffi::rocksdb_column_family_metadata_options_set_end_key(
                self.inner,
                key.as_ptr().cast::<c_char>(),
                key.len(),
            );
        }
    }

    /// Removes the upper bound, leaving the range open on that side.
    pub fn clear_end_key(&mut self) {
        unsafe {
            ffi::rocksdb_column_family_metadata_options_set_end_key(
                self.inner,
                std::ptr::null(),
                0,
            );
        }
    }

    /// Returns the upper bound, or `None` if unset.
    ///
    /// The C API hands back a borrowed pointer into a `std::string` owned by the
    /// options object, so the bytes are copied here rather than freed.
    pub fn get_end_key(&self) -> Option<Vec<u8>> {
        let mut len: usize = 0;
        let ptr = unsafe {
            ffi::rocksdb_column_family_metadata_options_get_end_key(
                self.inner.cast_const(),
                &raw mut len,
            )
        };
        unsafe { raw_data(ptr, len) }
    }
}

impl Default for ColumnFamilyMetaDataOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ColumnFamilyMetaDataOptions {
    fn drop(&mut self) {
        unsafe { ffi::rocksdb_column_family_metadata_options_destroy(self.inner) }
    }
}

impl fmt::Debug for ColumnFamilyMetaDataOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ColumnFamilyMetaDataOptions")
            .field("level", &self.get_level())
            .field("start_key", &self.get_start_key())
            .field("end_key", &self.get_end_key())
            .finish()
    }
}

// SAFETY: the pointee is a plain options bag with no thread affinity, and the
// setters take `&mut self` so shared access cannot mutate it.
unsafe impl Send for ColumnFamilyMetaDataOptions {}
unsafe impl Sync for ColumnFamilyMetaDataOptions {}

/// The role a file plays in a DB directory.
///
/// Mirrors `rocksdb::FileType` from `include/rocksdb/types.h`. Values RocksDB
/// adds in future versions decode as [`FileType::Unknown`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum FileType {
    /// Write-ahead log, `<number>.log`.
    WalFile,
    /// The `LOCK` file guarding single-process access.
    DBLockFile,
    /// SST table file, `<number>.sst` (or `.ldb` for LevelDB-era files).
    TableFile,
    /// A `MANIFEST-<number>` file.
    DescriptorFile,
    /// The `CURRENT` file naming the live manifest.
    CurrentFile,
    /// A `<number>.dbtmp` file written while another file is being staged.
    TempFile,
    /// A `LOG` or `LOG.old.<number>` info log.
    InfoLogFile,
    /// A `METADB-<number>` metadata database.
    MetaDatabase,
    /// The `IDENTITY` file holding the DB's unique id.
    IdentityFile,
    /// An `OPTIONS-<number>` file.
    OptionsFile,
    /// Blob file, `<number>.blob`.
    BlobFile,
    /// A `COMPACTION_PROGRESS-<timestamp>` file.
    CompactionProgressFile,
    /// A value this build of the crate does not know about.
    Unknown,
}

impl From<i32> for FileType {
    fn from(value: i32) -> Self {
        for candidate in [
            FileType::WalFile,
            FileType::DBLockFile,
            FileType::TableFile,
            FileType::DescriptorFile,
            FileType::CurrentFile,
            FileType::TempFile,
            FileType::InfoLogFile,
            FileType::MetaDatabase,
            FileType::IdentityFile,
            FileType::OptionsFile,
            FileType::BlobFile,
            FileType::CompactionProgressFile,
        ] {
            if value == candidate as i32 {
                return candidate;
            }
        }
        FileType::Unknown
    }
}

impl FileType {
    pub fn as_str(self) -> &'static str {
        match self {
            FileType::WalFile => "WalFile",
            FileType::DBLockFile => "DBLockFile",
            FileType::TableFile => "TableFile",
            FileType::DescriptorFile => "DescriptorFile",
            FileType::CurrentFile => "CurrentFile",
            FileType::TempFile => "TempFile",
            FileType::InfoLogFile => "InfoLogFile",
            FileType::MetaDatabase => "MetaDatabase",
            FileType::IdentityFile => "IdentityFile",
            FileType::OptionsFile => "OptionsFile",
            FileType::BlobFile => "BlobFile",
            FileType::CompactionProgressFile => "CompactionProgressFile",
            FileType::Unknown => "Unknown",
        }
    }
}

/// Storage tier hint for a file, passed through to the `FileSystem` so it can
/// place or encode the file differently.
///
/// Mirrors `rocksdb::Temperature` from `include/rocksdb/types.h`, including its
/// discriminants, so `Temperature::Warm as i32` is a valid argument to the
/// `*_temperature` setters on [`Options`](crate::Options). Values RocksDB adds
/// in future versions decode as [`Temperature::Unknown`], as does the
/// `kLastTemperature` sentinel, which is not a real tier.
///
/// Upstream leaves gaps between the discriminants so new tiers can be slotted
/// in. This feature is experimental upstream and subject to change.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum Temperature {
    /// No tier recorded. Also the decoding of any unrecognised value.
    Unknown = 0,
    /// Frequently read.
    Hot = 0x04,
    /// Read now and then.
    Warm = 0x08,
    /// Rarely read.
    Cool = 0x0A,
    /// Read very rarely.
    Cold = 0x0C,
    /// Archival; expect slow reads.
    Ice = 0x10,
}

impl From<i32> for Temperature {
    fn from(value: i32) -> Self {
        match value {
            0x04 => Temperature::Hot,
            0x08 => Temperature::Warm,
            0x0A => Temperature::Cool,
            0x0C => Temperature::Cold,
            0x10 => Temperature::Ice,
            _ => Temperature::Unknown,
        }
    }
}

impl Temperature {
    pub fn as_str(self) -> &'static str {
        match self {
            Temperature::Unknown => "Unknown",
            Temperature::Hot => "Hot",
            Temperature::Warm => "Warm",
            Temperature::Cool => "Cool",
            Temperature::Cold => "Cold",
            Temperature::Ice => "Ice",
        }
    }
}

/// Options controlling how live files storage info is collected.
pub struct LiveFilesStorageInfoOptions {
    pub(crate) inner: *mut ffi::rocksdb_livefiles_storage_info_options_t,
}

impl LiveFilesStorageInfoOptions {
    /// Creates options with RocksDB's defaults: no checksum info, always flush,
    /// and follow the DB-wide atomic flush setting.
    pub fn new() -> Self {
        Self {
            inner: unsafe { ffi::rocksdb_livefiles_storage_info_options_create() },
        }
    }

    /// Whether to populate the checksum fields on each entry. Off by default, in
    /// which case
    /// [`file_checksum_func_name`](LiveFileStorageInfoEntry::file_checksum_func_name)
    /// comes back empty.
    pub fn set_include_checksum_info(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_livefiles_storage_info_options_set_include_checksum_info(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `include_checksum_info` option.
    pub fn get_include_checksum_info(&self) -> bool {
        unsafe {
            ffi::rocksdb_livefiles_storage_info_options_get_include_checksum_info(self.inner) != 0
        }
    }

    /// Flush memtables when the total size of live WAL files in bytes is at
    /// least this value and the DB is writable.
    ///
    /// The default, `0`, always flushes.
    pub fn set_wal_size_for_flush(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_livefiles_storage_info_options_set_wal_size_for_flush(self.inner, val);
        }
    }

    /// Returns the value of the `wal_size_for_flush` option.
    pub fn get_wal_size_for_flush(&self) -> u64 {
        unsafe { ffi::rocksdb_livefiles_storage_info_options_get_wal_size_for_flush(self.inner) }
    }

    /// Flush all column families atomically regardless of
    /// [`Options::set_atomic_flush`](crate::Options::set_atomic_flush), giving a
    /// consistent view across them.
    ///
    /// Only matters when a flush actually happens, so it has no effect if
    /// `wal_size_for_flush` suppressed the flush. Defaults to off, which follows
    /// the DB-wide setting.
    pub fn set_atomic_flush(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_livefiles_storage_info_options_set_atomic_flush(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `atomic_flush` option.
    pub fn get_atomic_flush(&self) -> bool {
        unsafe { ffi::rocksdb_livefiles_storage_info_options_get_atomic_flush(self.inner) != 0 }
    }
}

impl Default for LiveFilesStorageInfoOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LiveFilesStorageInfoOptions {
    fn drop(&mut self) {
        unsafe { ffi::rocksdb_livefiles_storage_info_options_destroy(self.inner) }
    }
}

impl fmt::Debug for LiveFilesStorageInfoOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveFilesStorageInfoOptions")
            .field("include_checksum_info", &self.get_include_checksum_info())
            .field("wal_size_for_flush", &self.get_wal_size_for_flush())
            .field("atomic_flush", &self.get_atomic_flush())
            .finish()
    }
}

// SAFETY: the pointee is a plain options bag with no thread affinity, and the
// setters take `&mut self` so shared access cannot mutate it.
unsafe impl Send for LiveFilesStorageInfoOptions {}
unsafe impl Sync for LiveFilesStorageInfoOptions {}

/// Everything needed to make a consistent copy of a DB: SST and blob files,
/// WALs, the MANIFEST, CURRENT and OPTIONS.
///
/// This is a snapshot. It does not pin the files on disk, so a file listed here
/// can still be deleted by a background job unless the DB is otherwise held
/// still.
pub struct LiveFilesStorageInfo {
    inner: *mut ffi::rocksdb_livefiles_storage_info_t,
    /// Cached because the underlying vector is filled in once and never
    /// resized, and every bounds check would otherwise cost an FFI call.
    len: usize,
}

impl LiveFilesStorageInfo {
    /// Takes ownership of a raw storage info handle.
    ///
    /// # Safety
    ///
    /// `ptr` must be a non-null handle from `rocksdb_get_livefiles_storage_info`
    /// that nothing else owns. It is destroyed when the returned value drops.
    pub(crate) unsafe fn from_ptr(ptr: *mut ffi::rocksdb_livefiles_storage_info_t) -> Self {
        let len = unsafe { ffi::rocksdb_livefiles_storage_info_count(ptr.cast_const()) };
        Self { inner: ptr, len }
    }

    /// Number of files listed.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether no files are listed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Borrows the entry at `index`, or `None` if out of range.
    ///
    /// The bounds check matters: the underlying C accessors index a
    /// `std::vector` without checking, so an out of range index there is
    /// undefined behaviour.
    pub fn get(&self, index: usize) -> Option<LiveFileStorageInfoEntry<'_>> {
        if index >= self.len {
            return None;
        }
        Some(LiveFileStorageInfoEntry { info: self, index })
    }

    /// Iterates over every entry.
    pub fn iter(&self) -> LiveFilesStorageInfoIter<'_> {
        LiveFilesStorageInfoIter {
            info: self,
            range: 0..self.len,
        }
    }
}

impl Drop for LiveFilesStorageInfo {
    fn drop(&mut self) {
        unsafe { ffi::rocksdb_livefiles_storage_info_destroy(self.inner) }
    }
}

impl fmt::Debug for LiveFilesStorageInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<'a> IntoIterator for &'a LiveFilesStorageInfo {
    type Item = LiveFileStorageInfoEntry<'a>;
    type IntoIter = LiveFilesStorageInfoIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

// SAFETY: the pointee is a `std::vector<LiveFileStorageInfo>` that RocksDB fills
// in once and this type never mutates. Reads through `&self` cannot race, so
// both moving it between threads and sharing it across them are sound. `Sync` is
// what lets a `LiveFileStorageInfoEntry` be `Send`.
unsafe impl Send for LiveFilesStorageInfo {}
unsafe impl Sync for LiveFilesStorageInfo {}

/// Iterator over the entries of a [`LiveFilesStorageInfo`].
pub struct LiveFilesStorageInfoIter<'a> {
    info: &'a LiveFilesStorageInfo,
    range: Range<usize>,
}

impl<'a> Iterator for LiveFilesStorageInfoIter<'a> {
    type Item = LiveFileStorageInfoEntry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.range.next()?;
        Some(LiveFileStorageInfoEntry {
            info: self.info,
            index,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.range.size_hint()
    }
}

impl DoubleEndedIterator for LiveFilesStorageInfoIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let index = self.range.next_back()?;
        Some(LiveFileStorageInfoEntry {
            info: self.info,
            index,
        })
    }
}

impl ExactSizeIterator for LiveFilesStorageInfoIter<'_> {}

impl FusedIterator for LiveFilesStorageInfoIter<'_> {}

/// One file in a [`LiveFilesStorageInfo`] listing.
///
/// Every string accessor borrows straight out of the parent listing: the C API
/// returns interior pointers into the `std::string`s it holds, so nothing is
/// copied or freed here.
#[derive(Copy, Clone)]
pub struct LiveFileStorageInfoEntry<'a> {
    info: &'a LiveFilesStorageInfo,
    index: usize,
}

impl<'a> LiveFileStorageInfoEntry<'a> {
    /// Position of this entry within the parent listing.
    pub fn index(&self) -> usize {
        self.index
    }

    /// The file name within its directory, for example `123456.sst`.
    pub fn relative_filename(&self) -> &'a [u8] {
        unsafe {
            borrowed_cstr(ffi::rocksdb_livefiles_storage_info_relative_filename(
                self.info.inner.cast_const(),
                self.index,
            ))
        }
    }

    /// [`relative_filename`](Self::relative_filename) as UTF-8, replacing
    /// invalid sequences.
    pub fn relative_filename_lossy(&self) -> Cow<'a, str> {
        String::from_utf8_lossy(self.relative_filename())
    }

    /// The directory holding the file, without a trailing `/`. This could be a
    /// DB path, the WAL directory, and so on.
    pub fn directory(&self) -> &'a [u8] {
        unsafe {
            borrowed_cstr(ffi::rocksdb_livefiles_storage_info_directory(
                self.info.inner.cast_const(),
                self.index,
            ))
        }
    }

    /// [`directory`](Self::directory) as UTF-8, replacing invalid sequences.
    pub fn directory_lossy(&self) -> Cow<'a, str> {
        String::from_utf8_lossy(self.directory())
    }

    /// The file's number within the DB, or `0` for files that have none, such
    /// as `CURRENT`.
    pub fn file_number(&self) -> u64 {
        unsafe {
            ffi::rocksdb_livefiles_storage_info_file_number(
                self.info.inner.cast_const(),
                self.index,
            )
        }
    }

    /// The role this file plays in the DB.
    pub fn file_type(&self) -> FileType {
        FileType::from(unsafe {
            ffi::rocksdb_livefiles_storage_info_file_type(self.info.inner.cast_const(), self.index)
        })
    }

    /// File size in bytes. See [`trim_to_size`](Self::trim_to_size) and
    /// [`replacement_contents`](Self::replacement_contents) for when the file on
    /// disk may differ.
    pub fn size(&self) -> u64 {
        unsafe {
            ffi::rocksdb_livefiles_storage_info_size(self.info.inner.cast_const(), self.index)
        }
    }

    /// When true the file on disk may be longer than [`size`](Self::size) and
    /// only the first `size` bytes belong in the copy. When false a length
    /// mismatch means the file is corrupt.
    pub fn trim_to_size(&self) -> bool {
        unsafe {
            ffi::rocksdb_livefiles_storage_info_trim_to_size(
                self.info.inner.cast_const(),
                self.index,
            ) != 0
        }
    }

    /// Contents to write instead of reading the file from disk, used for
    /// `CURRENT`. Empty means read the file on disk as usual; otherwise this is
    /// exactly [`size`](Self::size) bytes long.
    ///
    /// Unlike the other borrowed strings this may contain NUL bytes, so it comes
    /// from the length out-param rather than `strlen`.
    pub fn replacement_contents(&self) -> &'a [u8] {
        let mut size: usize = 0;
        let ptr = unsafe {
            ffi::rocksdb_livefiles_storage_info_replacement_contents(
                self.info.inner.cast_const(),
                self.index,
                &raw mut size,
            )
        };
        if ptr.is_null() || size == 0 {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), size) }
    }

    // There is deliberately no `file_checksum` accessor. RocksDB stores a binary
    // checksum in this field, and the built-in CRC32c generator writes four raw
    // big-endian bytes, but the only C accessor
    // (`rocksdb_livefiles_storage_info_file_checksum`) hands it back as a
    // NUL-terminated string with no length. Any byte of the digest can be zero, so
    // the value would be silently truncated for roughly one file in sixty-five with
    // no way for a caller to tell. Exposing the function name below is safe because
    // that really is a string.

    /// Name of the checksum function that produced this file's checksum.
    /// `Unknown` when no checksum function is configured, empty when checksum
    /// info was not requested.
    pub fn file_checksum_func_name(&self) -> &'a [u8] {
        unsafe {
            borrowed_cstr(ffi::rocksdb_livefiles_storage_info_file_checksum_func_name(
                self.info.inner.cast_const(),
                self.index,
            ))
        }
    }

    /// [`file_checksum_func_name`](Self::file_checksum_func_name) as UTF-8,
    /// replacing invalid sequences.
    pub fn file_checksum_func_name_lossy(&self) -> Cow<'a, str> {
        String::from_utf8_lossy(self.file_checksum_func_name())
    }

    /// The storage tier the file is placed on.
    pub fn temperature(&self) -> Temperature {
        Temperature::from(unsafe {
            ffi::rocksdb_livefiles_storage_info_temperature(
                self.info.inner.cast_const(),
                self.index,
            )
        })
    }
}

impl fmt::Debug for LiveFileStorageInfoEntry<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveFileStorageInfoEntry")
            .field("relative_filename", &self.relative_filename_lossy())
            .field("directory", &self.directory_lossy())
            .field("file_number", &self.file_number())
            .field("file_type", &self.file_type())
            .field("size", &self.size())
            .field("trim_to_size", &self.trim_to_size())
            .field("temperature", &self.temperature())
            .finish_non_exhaustive()
    }
}

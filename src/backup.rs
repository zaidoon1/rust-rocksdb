// Copyright 2016 Alex Regueiro
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

use crate::env::Env;
use crate::{
    DBCommon, Error, RateLimiterMode, ThreadMode,
    db::DBInner,
    ffi,
    ffi_util::{raw_data, to_cpath},
};

use libc::{c_char, c_int, c_uchar, c_void};
use std::ffi::CString;
use std::ops::BitOr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process;
use std::slice;
use std::sync::Arc;

/// Represents information of a backup including timestamp of the backup
/// and the size (please note that sum of all backups' sizes is bigger than the actual
/// size of the backup directory because some data is shared by multiple backups).
/// Backups are identified by their always-increasing IDs.
pub struct BackupEngineInfo {
    /// Timestamp of the backup
    pub timestamp: i64,
    /// ID of the backup
    pub backup_id: u32,
    /// Size of the backup
    pub size: u64,
    /// Number of files related to the backup
    pub num_files: u32,
    /// Application metadata passed to
    /// [`BackupEngine::create_new_backup_with_metadata`].
    ///
    /// Empty when the backup carries no metadata. RocksDB stores the metadata
    /// as a `std::string` and skips writing it when empty, so an absent value
    /// and an empty value are indistinguishable on read back.
    pub app_metadata: Vec<u8>,
}

pub struct BackupEngine {
    inner: *mut ffi::rocksdb_backup_engine_t,
    _outlive: Env,
    // `BackupEngineOptions::backup_env` is stored in the engine as a raw
    // `Env*` (see `rocksdb_backup_engine_options_set_env` in `db/c.cc`) and
    // `BackupEngineImpl` keeps a by-value copy of the options, so that Env has
    // to outlive the engine, not just the options it was set on.
    _outlive_backup_env: Option<Env>,
}

pub struct BackupEngineOptions {
    inner: *mut ffi::rocksdb_backup_engine_options_t,
    // Kept alive because RocksDB only borrows the `Env*`. See
    // `BackupEngineOptions::set_env`.
    backup_env: Option<Env>,
}

pub struct RestoreOptions {
    inner: *mut ffi::rocksdb_restore_options_t,
}

/// CPU priority for the background threads that copy files during a backup.
///
/// Mirrors `rocksdb::CpuPriority` in `include/rocksdb/port_defs.h`. The
/// discriminants are the upstream values, which is what the C API expects.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(i32)]
pub enum CpuPriority {
    KIdle = 0,
    KLow = 1,
    KNormal = 2,
    KHigh = 3,
}

impl From<i32> for CpuPriority {
    /// Values RocksDB does not define map to [`CpuPriority::KNormal`], which
    /// is both the upstream default for `background_thread_cpu_priority` and
    /// the priority the copy threads start at.
    fn from(value: i32) -> Self {
        match value {
            0 => Self::KIdle,
            1 => Self::KLow,
            3 => Self::KHigh,
            // 2 is kNormal, and so is anything unrecognised.
            _ => Self::KNormal,
        }
    }
}

/// How much of the destination database a restore may reuse.
///
/// Mirrors `RestoreOptions::Mode` in
/// `include/rocksdb/utilities/backup_engine.h`. The values look like flags but
/// RocksDB only ever compares the field for equality, so the modes are
/// exclusive and must not be combined.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(i32)]
pub enum RestoreMode {
    /// Cheapest way to restore a healthy database. Files whose names already
    /// encode the DB session id are kept, so an interrupted or partial copy
    /// can be completed instead of recopied. This mode also cooperates with
    /// [`CreateBackupOptions::set_exclude_files_callback`] by looking for
    /// excluded files in the existing database directory.
    ///
    /// Only effective for files using a modern share files naming scheme, see
    /// [`ShareFilesNaming`].
    KKeepLatestDbSessionIdFiles = 1,
    /// For a database suspected to be corrupt. Each existing file is checksummed
    /// against the checksum recorded in the backup metadata, and only files that
    /// fail the comparison are replaced.
    KVerifyChecksum = 2,
    /// Zero trust and least efficient. Deletes every file in the destination
    /// and copies everything from the backup. This is the default.
    KPurgeAllFiles = 0xffff,
}

impl From<i32> for RestoreMode {
    /// Values RocksDB does not define map to [`RestoreMode::KPurgeAllFiles`],
    /// the upstream default and the only mode that makes no assumptions about
    /// the destination directory.
    fn from(value: i32) -> Self {
        match value {
            1 => Self::KKeepLatestDbSessionIdFiles,
            2 => Self::KVerifyChecksum,
            // 0xffff is kPurgeAllFiles, and so is anything unrecognised.
            _ => Self::KPurgeAllFiles,
        }
    }
}

/// Naming scheme for table and blob files in the `shared_checksum` directory.
///
/// Mirrors `BackupEngineOptions::ShareFilesNaming` in
/// `include/rocksdb/utilities/backup_engine.h`. It is a bit mask with two
/// parts: the low 16 bits select one naming scheme, the high bits carry
/// independent flags. RocksDB masks the two apart with
/// [`Self::MASK_NO_NAMING_FLAGS`] and [`Self::MASK_NAMING_FLAGS`].
///
/// Only takes effect when both `share_table_files` and
/// `share_files_with_checksum` are true.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ShareFilesNaming(c_int);

impl ShareFilesNaming {
    /// `kLegacyCrc32cAndFileSize`. Names files
    /// `<file_number>_<crc32c>_<file_size>.sst` and `.blob`. Only recommended
    /// for preserving old behaviour: the triple can collide at massive scale,
    /// and picking the name requires reading the whole file to checksum it.
    pub const LEGACY_CRC32C_AND_FILE_SIZE: Self = Self(1);

    /// `kUseDbSessionId`. Names SST files `<file_number>_s<db_session_id>.sst`,
    /// which is strongly unique and known without reading the file. Blob files
    /// and SST files with no session id fall back to
    /// [`Self::LEGACY_CRC32C_AND_FILE_SIZE`].
    pub const USE_DB_SESSION_ID: Self = Self(2);

    /// `kFlagIncludeFileSize`. Inserts `_<file_size>` before the extension when
    /// the scheme does not already include it.
    ///
    /// This is bit 31. The C API passes the mask as an `int`, so a value
    /// carrying this flag reads back negative from [`Self::bits`].
    pub const FLAG_INCLUDE_FILE_SIZE: Self = Self(1 << 31);

    /// `kMaskNoNamingFlags`. Selects the naming scheme bits.
    pub const MASK_NO_NAMING_FLAGS: Self = Self(0xffff);

    /// `kMaskNamingFlags`. Selects the flag bits.
    pub const MASK_NAMING_FLAGS: Self = Self(!0xffff);

    /// The RocksDB default, [`Self::USE_DB_SESSION_ID`] with
    /// [`Self::FLAG_INCLUDE_FILE_SIZE`].
    pub const DEFAULT: Self = Self(Self::USE_DB_SESSION_ID.0 | Self::FLAG_INCLUDE_FILE_SIZE.0);

    /// Returns the raw mask as the C API represents it.
    pub const fn bits(self) -> c_int {
        self.0
    }

    /// Returns true when every bit set in `other` is also set in `self`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl BitOr for ShareFilesNaming {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

type ProgressFn = dyn Fn() + Send + Sync + 'static;
type ExcludeFilesFn = dyn Fn(&[u8]) -> bool + Send + Sync + 'static;

/// Owns a progress closure. [`CreateBackupOptions`] hands the address of the
/// holder to the C API as the opaque `state` pointer, so the holder is kept
/// behind an [`Arc`] rather than a `Box`: the C side holds a raw pointer to it
/// while this crate still owns it, which is exactly what `Box`'s uniqueness
/// guarantee rules out. This mirrors how `Options::set_callback_logger` keeps
/// its log callback alive.
struct ProgressCallback {
    callback: Box<ProgressFn>,
}

/// Owns an exclude files closure. See [`ProgressCallback`].
struct ExcludeFilesCallback {
    callback: Box<ExcludeFilesFn>,
}

/// Trampoline for `rocksdb_create_backup_options_progress_cb`.
unsafe extern "C" fn progress_callback_trampoline(state: *mut c_void) {
    // SAFETY: `state` is the address of the `ProgressCallback` that
    // `set_progress_callback` registered. `CreateBackupOptions` keeps it alive
    // until after `rocksdb_create_backup_options_destroy` has run, and a
    // shared reference is enough because `ProgressFn` is a `dyn Fn`. RocksDB
    // serialises progress reports behind a mutex but may report from any copy
    // thread, so `&mut` here would alias.
    let holder = unsafe { &*state.cast::<ProgressCallback>() };
    // A Rust panic cannot unwind out of an `extern "C"` function, and RocksDB
    // only knows how to turn a C++ exception into Status::Aborted.
    if catch_unwind(AssertUnwindSafe(|| (holder.callback)())).is_err() {
        process::abort();
    }
}

/// Trampoline for `rocksdb_create_backup_options_exclude_files_cb`.
unsafe extern "C" fn exclude_files_callback_trampoline(
    state: *mut c_void,
    relative_file: *const c_char,
    relative_file_len: usize,
) -> c_uchar {
    // SAFETY: see `progress_callback_trampoline`.
    let holder = unsafe { &*state.cast::<ExcludeFilesCallback>() };
    let relative_file = if relative_file_len == 0 {
        &[][..]
    } else {
        // SAFETY: `db/c.cc` passes `relative_file.data()` and
        // `relative_file.size()` of a live `std::string`, and the closure only
        // borrows it for the duration of this call.
        unsafe { slice::from_raw_parts(relative_file.cast::<u8>(), relative_file_len) }
    };
    match catch_unwind(AssertUnwindSafe(|| (holder.callback)(relative_file))) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(_) => process::abort(),
    }
}

/// Options for a single call to [`BackupEngine::create_new_backup_with_options`]
/// or [`BackupEngine::create_new_backup_with_metadata`].
///
/// Wraps `rocksdb::CreateBackupOptions`.
///
/// # Callback ownership
///
/// `rocksdb_create_backup_options_set_progress_callback` and
/// `rocksdb_create_backup_options_set_exclude_files_callback` take a bare
/// `void* state` with no destructor argument. `db/c.cc` stores it in
/// `rocksdb_create_backup_options_t::progress_state` /
/// `exclude_files_state` and copies it into a C++ lambda by value.
/// `rocksdb_create_backup_options_destroy` is just `delete options`, so the C
/// API never frees the state. This type therefore owns the closures: it frees
/// them when it is dropped, and frees the previous one when a setter is called
/// a second time.
///
/// The C++ lambda captures the state pointer by value rather than the options
/// wrapper, so the state must stay alive for the whole backup, not merely
/// until the FFI call returns. Backup creation is synchronous and borrows
/// these options for its duration, so holding the closures in this type is
/// enough.
pub struct CreateBackupOptions {
    inner: *mut ffi::rocksdb_create_backup_options_t,
    progress_callback: Option<Arc<ProgressCallback>>,
    exclude_files_callback: Option<Arc<ExcludeFilesCallback>>,
}

// BackupEngine is a simple pointer wrapper, so it's safe to send to another thread
// since the underlying RocksDB backup engine is thread-safe.
unsafe impl Send for BackupEngine {}

impl BackupEngine {
    /// Open a backup engine with the specified options and RocksDB Env.
    pub fn open(opts: &BackupEngineOptions, env: &Env) -> Result<Self, Error> {
        let be: *mut ffi::rocksdb_backup_engine_t;
        unsafe {
            be = ffi_try!(ffi::rocksdb_backup_engine_open_opts(
                opts.inner,
                env.0.inner
            ));
        }

        if be.is_null() {
            return Err(Error::new("Could not initialize backup engine.".to_owned()));
        }

        Ok(Self {
            inner: be,
            _outlive: env.clone(),
            _outlive_backup_env: opts.backup_env.clone(),
        })
    }

    /// Captures the state of the database in the latest backup.
    ///
    /// Note: no flush before backup is performed. User might want to
    /// use `create_new_backup_flush` instead.
    pub fn create_new_backup<T: ThreadMode, D: DBInner>(
        &mut self,
        db: &DBCommon<T, D>,
    ) -> Result<(), Error> {
        self.create_new_backup_flush(db, false)
    }

    /// Captures the state of the database in the latest backup.
    ///
    /// Set flush_before_backup=true to avoid losing unflushed key/value
    /// pairs from the memtable.
    pub fn create_new_backup_flush<T: ThreadMode, D: DBInner>(
        &mut self,
        db: &DBCommon<T, D>,
        flush_before_backup: bool,
    ) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_backup_engine_create_new_backup_flush(
                self.inner,
                db.inner.inner(),
                c_uchar::from(flush_before_backup),
            ));
            Ok(())
        }
    }

    /// Captures the state of the database in the latest backup and returns the
    /// id of the new backup.
    ///
    /// Unlike [`Self::create_new_backup_flush`] this exposes the full
    /// `CreateBackupOptions` surface: flush behaviour, CPU priority of the copy
    /// threads, and the progress and exclude files callbacks.
    pub fn create_new_backup_with_options<T: ThreadMode, D: DBInner>(
        &mut self,
        db: &DBCommon<T, D>,
        opts: &CreateBackupOptions,
    ) -> Result<u32, Error> {
        let mut backup_id: u32 = 0;
        unsafe {
            ffi_try!(ffi::rocksdb_backup_engine_create_new_backup_with_options(
                self.inner,
                db.inner.inner(),
                opts.inner,
                &raw mut backup_id,
            ));
        }
        Ok(backup_id)
    }

    /// Same as [`Self::create_new_backup_with_options`] but also stores
    /// application metadata with the backup, and returns the id of the new
    /// backup.
    ///
    /// Read the metadata back with [`BackupEngineInfo::app_metadata`].
    ///
    /// `app_metadata` is passed with an explicit length and RocksDB stores it
    /// hex encoded in the backup meta file, so any byte sequence is preserved,
    /// including embedded NULs.
    ///
    /// RocksDB rejects metadata larger than 1 MiB with an
    /// `Invalid argument: App metadata too large` error, and nothing is
    /// written in that case.
    pub fn create_new_backup_with_metadata<T: ThreadMode, D: DBInner>(
        &mut self,
        db: &DBCommon<T, D>,
        opts: &CreateBackupOptions,
        app_metadata: &[u8],
    ) -> Result<u32, Error> {
        // `db/c.cc` substitutes an empty string for a null pointer, which
        // avoids handing it the dangling pointer an empty slice yields.
        let (metadata_ptr, metadata_len) = if app_metadata.is_empty() {
            (std::ptr::null(), 0)
        } else {
            (app_metadata.as_ptr().cast::<c_char>(), app_metadata.len())
        };

        let mut backup_id: u32 = 0;
        unsafe {
            ffi_try!(ffi::rocksdb_backup_engine_create_new_backup_with_metadata(
                self.inner,
                db.inner.inner(),
                opts.inner,
                metadata_ptr,
                metadata_len,
                &raw mut backup_id,
            ));
        }
        Ok(backup_id)
    }

    pub fn purge_old_backups(&mut self, num_backups_to_keep: usize) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_backup_engine_purge_old_backups(
                self.inner,
                num_backups_to_keep as u32,
            ));
            Ok(())
        }
    }

    /// Restore from the latest backup
    ///
    /// # Arguments
    ///
    /// * `db_dir` - A path to the database directory
    /// * `wal_dir` - A path to the wal directory
    /// * `opts` - Restore options
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rust_rocksdb::backup::{BackupEngine, BackupEngineOptions};
    /// let backup_opts = BackupEngineOptions::default();
    /// let mut backup_engine = BackupEngine::open(&backup_opts, &backup_path).unwrap();
    /// let mut restore_option = rust_rocksdb::backup::RestoreOptions::default();
    /// restore_option.set_keep_log_files(true); /// true to keep log files
    /// if let Err(e) = backup_engine.restore_from_latest_backup(&db_path, &wal_dir, &restore_option) {
    ///     error!("Failed to restore from the backup. Error: {:?}", e);
    ///     return Err(e.to_string());
    /// }
    /// ```
    pub fn restore_from_latest_backup<D: AsRef<Path>, W: AsRef<Path>>(
        &mut self,
        db_dir: D,
        wal_dir: W,
        opts: &RestoreOptions,
    ) -> Result<(), Error> {
        let c_db_dir = to_cpath(db_dir)?;
        let c_wal_dir = to_cpath(wal_dir)?;

        unsafe {
            ffi_try!(ffi::rocksdb_backup_engine_restore_db_from_latest_backup(
                self.inner,
                c_db_dir.as_ptr(),
                c_wal_dir.as_ptr(),
                opts.inner,
            ));
        }
        Ok(())
    }

    /// Restore from a specified backup
    ///
    /// The specified backup id should be passed in as an additional parameter.
    pub fn restore_from_backup<D: AsRef<Path>, W: AsRef<Path>>(
        &mut self,
        db_dir: D,
        wal_dir: W,
        opts: &RestoreOptions,
        backup_id: u32,
    ) -> Result<(), Error> {
        let c_db_dir = to_cpath(db_dir)?;
        let c_wal_dir = to_cpath(wal_dir)?;

        unsafe {
            ffi_try!(ffi::rocksdb_backup_engine_restore_db_from_backup(
                self.inner,
                c_db_dir.as_ptr(),
                c_wal_dir.as_ptr(),
                opts.inner,
                backup_id,
            ));
        }
        Ok(())
    }

    /// Checks that each file exists and that the size of the file matches our
    /// expectations. it does not check file checksum.
    ///
    /// If this BackupEngine created the backup, it compares the files' current
    /// sizes against the number of bytes written to them during creation.
    /// Otherwise, it compares the files' current sizes against their sizes when
    /// the BackupEngine was opened.
    pub fn verify_backup(&self, backup_id: u32) -> Result<(), Error> {
        unsafe {
            ffi_try!(ffi::rocksdb_backup_engine_verify_backup(
                self.inner, backup_id,
            ));
        }
        Ok(())
    }

    /// Get a list of all backups together with information on timestamp of the backup
    /// and the size (please note that sum of all backups' sizes is bigger than the actual
    /// size of the backup directory because some data is shared by multiple backups).
    /// Backups are identified by their always-increasing IDs.
    ///
    /// You can perform this function safely, even with other BackupEngine performing
    /// backups on the same directory
    pub fn get_backup_info(&self) -> Vec<BackupEngineInfo> {
        unsafe {
            let i = ffi::rocksdb_backup_engine_get_backup_info(self.inner);

            let n = ffi::rocksdb_backup_engine_info_count(i);

            let mut info = Vec::with_capacity(n as usize);
            for index in 0..n {
                // `rocksdb_backup_engine_info_app_metadata` returns
                // `BackupInfo::app_metadata.data()`, a pointer borrowed from
                // the info object rather than a malloc'd copy, so it has to be
                // copied before `rocksdb_backup_engine_info_destroy` below. A
                // backup with no metadata yields an empty string, so the
                // pointer stays valid and the length is 0.
                let mut metadata_len: usize = 0;
                let metadata_ptr =
                    ffi::rocksdb_backup_engine_info_app_metadata(i, index, &raw mut metadata_len);
                let app_metadata = if metadata_len == 0 {
                    Vec::new()
                } else {
                    raw_data(metadata_ptr, metadata_len).unwrap_or_default()
                };

                info.push(BackupEngineInfo {
                    timestamp: ffi::rocksdb_backup_engine_info_timestamp(i, index),
                    backup_id: ffi::rocksdb_backup_engine_info_backup_id(i, index),
                    size: ffi::rocksdb_backup_engine_info_size(i, index),
                    num_files: ffi::rocksdb_backup_engine_info_number_files(i, index),
                    app_metadata,
                });
            }

            // destroy backup info object
            ffi::rocksdb_backup_engine_info_destroy(i);

            info
        }
    }

    /// Aborts a backup that is currently running.
    ///
    /// This is one way. Once it is called on a backup engine, every later backup
    /// request on that engine fails. The backup directory is left consistent and is
    /// cleaned up by the next backup or garbage collection performed through a new
    /// backup engine on the same directory.
    pub fn stop_backup(&mut self) {
        unsafe {
            ffi::rocksdb_backup_engine_stop_backup(self.inner);
        }
    }
}

impl BackupEngineOptions {
    /// Initializes `BackupEngineOptions` with the directory to be used for storing/accessing the
    /// backup files.
    pub fn new<P: AsRef<Path>>(backup_dir: P) -> Result<Self, Error> {
        let backup_dir = backup_dir.as_ref();
        let c_backup_dir = CString::new(backup_dir.to_string_lossy().as_bytes()).map_err(|_| {
            Error::new(
                "Failed to convert backup_dir to CString \
                     when constructing BackupEngineOptions"
                    .to_owned(),
            )
        })?;

        unsafe {
            let opts = ffi::rocksdb_backup_engine_options_create(c_backup_dir.as_ptr());
            assert!(!opts.is_null(), "Could not create RocksDB backup options");

            Ok(Self {
                inner: opts,
                backup_env: None,
            })
        }
    }

    /// Sets the number of operations (such as file copies or file checksums) that `RocksDB` may
    /// perform in parallel when executing a backup or restore.
    ///
    /// Default: 1
    pub fn set_max_background_operations(&mut self, max_background_operations: i32) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_max_background_operations(
                self.inner,
                max_background_operations,
            );
        }
    }

    /// Sets whether to use fsync(2) to sync file data and metadata to disk after every file write,
    /// guaranteeing that backups will be consistent after a reboot or if machine crashes. Setting
    /// it to false will speed things up a bit, but some (newer) backups might be inconsistent. In
    /// most cases, everything should be fine, though.
    ///
    /// Default: true
    ///
    /// Documentation: <https://github.com/facebook/rocksdb/wiki/How-to-backup-RocksDB#advanced-usage>
    pub fn set_sync(&mut self, sync: bool) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_sync(self.inner, c_uchar::from(sync));
        }
    }

    /// Returns the value of the `sync` option.
    pub fn get_sync(&mut self) -> bool {
        let val_u8 = unsafe { ffi::rocksdb_backup_engine_options_get_sync(self.inner) };
        val_u8 != 0
    }

    /// (Experimental - subject to change or removal) When taking a backup and saving file
    /// temperature info (minimum schema_version is 2), there are two potential sources of
    /// truth for the placement of files into temperature tiers: (a) the current file
    /// temperature reported by the FileSystem or (b) the expected file temperature recorded
    /// in DB manifest. When this option is false (default), (b) overrides (a) if both are not
    /// UNKNOWN. When true, (a) overrides (b) if both are not UNKNOWN. Regardless of this
    /// setting, a known temperature overrides UNKNOWN.
    pub fn set_current_temperatures_override_manifest(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_current_temperatures_override_manifest(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `current_temperatures_override_manifest` option.
    pub fn get_current_temperatures_override_manifest(&self) -> bool {
        unsafe {
            ffi::rocksdb_backup_engine_options_get_current_temperatures_override_manifest(
                self.inner,
            ) != 0
        }
    }

    /// Sets the `io_buffer_size` option.
    pub fn set_io_buffer_size(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_io_buffer_size(self.inner, val);
        }
    }

    /// Returns the value of the `io_buffer_size` option.
    pub fn get_io_buffer_size(&self) -> u64 {
        unsafe { ffi::rocksdb_backup_engine_options_get_io_buffer_size(self.inner) }
    }

    /// Major schema version to use when writing backup meta files 1 (default) - compatible
    /// with very old versions of RocksDB. 2 - can be read by RocksDB versions >= 6.19.0.
    /// Minimum schema version for
    /// - (Experimental) saving and restoring file temperature metadata
    pub fn set_schema_version(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_schema_version(self.inner, val);
        }
    }

    /// Returns the value of the `schema_version` option.
    pub fn get_schema_version(&self) -> c_int {
        unsafe { ffi::rocksdb_backup_engine_options_get_schema_version(self.inner) }
    }

    /// share_files_with_checksum supports table and blob files.
    ///
    /// Only used if share_table_files is set to true. Setting to false is DEPRECATED and
    /// potentially dangerous because in that case BackupEngine can lose data if backing up
    /// databases with distinct or divergent history, for example if restoring from a backup
    /// other than the latest, writing to the DB, and creating another backup. Setting to true
    /// (default) prevents these issues by ensuring that different table files (SSTs) and blob
    /// files with the same number are treated as distinct. See
    /// share_files_with_checksum_naming and ShareFilesNaming.
    ///
    /// Default: true
    pub fn set_share_files_with_checksum(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_share_files_with_checksum(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the value of the `share_files_with_checksum` option.
    pub fn get_share_files_with_checksum(&self) -> bool {
        unsafe { ffi::rocksdb_backup_engine_options_get_share_files_with_checksum(self.inner) != 0 }
    }

    /// Returns the current `backup_log_files` setting.
    ///
    /// See [`Self::set_backup_log_files`] for what this controls.
    pub fn get_backup_log_files(&self) -> bool {
        unsafe { ffi::rocksdb_backup_engine_options_get_backup_log_files(self.inner) != 0 }
    }

    /// Returns the current `backup_rate_limit` setting.
    ///
    /// See [`Self::set_backup_rate_limit`] for what this controls.
    pub fn get_backup_rate_limit(&self) -> u64 {
        unsafe { ffi::rocksdb_backup_engine_options_get_backup_rate_limit(self.inner) }
    }

    /// Returns the current `callback_trigger_interval_size` setting.
    ///
    /// See [`Self::set_callback_trigger_interval_size`] for what this controls.
    pub fn get_callback_trigger_interval_size(&self) -> u64 {
        unsafe { ffi::rocksdb_backup_engine_options_get_callback_trigger_interval_size(self.inner) }
    }

    /// Returns the current `destroy_old_data` setting.
    ///
    /// See [`Self::set_destroy_old_data`] for what this controls.
    pub fn get_destroy_old_data(&self) -> bool {
        unsafe { ffi::rocksdb_backup_engine_options_get_destroy_old_data(self.inner) != 0 }
    }

    /// Returns the current `max_background_operations` setting.
    ///
    /// See [`Self::set_max_background_operations`] for what this controls.
    pub fn get_max_background_operations(&self) -> c_int {
        unsafe { ffi::rocksdb_backup_engine_options_get_max_background_operations(self.inner) }
    }

    /// Returns the current `max_valid_backups_to_open` setting.
    ///
    /// See [`Self::set_max_valid_backups_to_open`] for what this controls.
    pub fn get_max_valid_backups_to_open(&self) -> c_int {
        unsafe { ffi::rocksdb_backup_engine_options_get_max_valid_backups_to_open(self.inner) }
    }

    /// Returns the current `restore_rate_limit` setting.
    ///
    /// See [`Self::set_restore_rate_limit`] for what this controls.
    pub fn get_restore_rate_limit(&self) -> u64 {
        unsafe { ffi::rocksdb_backup_engine_options_get_restore_rate_limit(self.inner) }
    }

    /// Returns the current `share_table_files` setting.
    ///
    /// See [`Self::set_share_table_files`] for what this controls.
    pub fn get_share_table_files(&self) -> bool {
        unsafe { ffi::rocksdb_backup_engine_options_get_share_table_files(self.inner) != 0 }
    }

    /// If false, we won't backup log files. This option can be useful for backing up
    /// in-memory databases where log file are persisted, but table files are in memory.
    /// Default: true.
    pub fn set_backup_log_files(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_backup_log_files(self.inner, c_uchar::from(val));
        }
    }

    /// Max bytes that can be transferred in a second while creating a backup.
    ///
    /// If 0, go as fast as you can. This limit only applies to writes.
    ///
    /// Default: `0`
    pub fn set_backup_rate_limit(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_backup_rate_limit(self.inner, val);
        }
    }

    /// During backup user can get callback every time next callback_trigger_interval_size
    /// bytes being copied. Default: 4194304.
    pub fn set_callback_trigger_interval_size(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_callback_trigger_interval_size(self.inner, val);
        }
    }

    /// If true, it will delete whatever backups there are already Default: false.
    pub fn set_destroy_old_data(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_destroy_old_data(self.inner, c_uchar::from(val));
        }
    }

    /// For BackupEngineReadOnly, Open() will open at most this many of the latest
    /// non-corrupted backups.
    ///
    /// Note: this setting is ignored (behaves like INT_MAX) for any kind of writable
    /// BackupEngine because it would inhibit accounting for shared files for proper backup
    /// deletion, including purging any incompletely created backups on creation of a new
    /// backup.
    ///
    /// Default: INT_MAX.
    pub fn set_max_valid_backups_to_open(&mut self, val: c_int) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_max_valid_backups_to_open(self.inner, val);
        }
    }

    /// Max bytes that can be transferred in a second during restore. If 0, go as fast as you
    /// can This limit only applies to writes. To also limit reads, a rate limiter able to
    /// also limit reads (e.g, its mode = kAllIo) have to be passed in through the option
    /// "restore_rate_limiter" Default: 0.
    pub fn set_restore_rate_limit(&mut self, val: u64) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_restore_rate_limit(self.inner, val);
        }
    }

    /// Share_table_files supports table and blob files.
    ///
    /// If share_table_files == true, the backup directory will share table and blob files
    /// among backups, to save space among backups of the same DB and to enable incremental
    /// backups by only copying new files. If share_table_files == false, each backup will be
    /// on its own and will not share any data with other backups.
    ///
    /// default: true.
    pub fn set_share_table_files(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_share_table_files(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Changes the directory backups are written to and read from, replacing
    /// the one given to [`Self::new`].
    ///
    /// RocksDB copies the string, so the path is not borrowed. It cannot
    /// contain an interior NUL.
    pub fn set_backup_dir<P: AsRef<Path>>(&mut self, backup_dir: P) -> Result<(), Error> {
        let c_backup_dir = to_cpath(backup_dir)?;
        unsafe {
            ffi::rocksdb_backup_engine_options_set_backup_dir(self.inner, c_backup_dir.as_ptr());
        }
        Ok(())
    }

    /// Returns the directory backups are written to and read from.
    pub fn get_backup_dir(&self) -> PathBuf {
        unsafe {
            // The C API returns `backup_dir.data()`, borrowed from the options
            // object rather than malloc'd, so it must be copied here and must
            // not be freed.
            let mut len: usize = 0;
            let ptr = ffi::rocksdb_backup_engine_options_get_backup_dir(self.inner, &raw mut len);
            if len == 0 {
                return PathBuf::new();
            }
            path_from_bytes(slice::from_raw_parts(ptr.cast::<u8>(), len))
        }
    }

    /// Sets the naming scheme used for table and blob files stored in the
    /// `shared_checksum` directory.
    ///
    /// The value must select exactly one naming scheme from the low bits, see
    /// [`ShareFilesNaming`]. Default: [`ShareFilesNaming::DEFAULT`].
    ///
    /// Changing this is downgrade safe, because RocksDB can read, restore and
    /// delete backups whose files use a different scheme. It does mean that
    /// adding more backups to the same directory can store a file a second
    /// time under its new shared name.
    pub fn set_share_files_with_checksum_naming(&mut self, val: ShareFilesNaming) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_share_files_with_checksum_naming(
                self.inner,
                val.bits(),
            );
        }
    }

    /// Returns the current `share_files_with_checksum_naming` setting.
    pub fn get_share_files_with_checksum_naming(&self) -> ShareFilesNaming {
        unsafe {
            ShareFilesNaming(
                ffi::rocksdb_backup_engine_options_get_share_files_with_checksum_naming(self.inner),
            )
        }
    }

    /// Sets the Env used for the backup directory itself. The database being
    /// backed up keeps using its own Env.
    ///
    /// RocksDB stores a borrowed `Env*` and `BackupEngineImpl` copies the
    /// options by value, so the Env has to outlive the engine. This type keeps
    /// a handle, and [`BackupEngine::open`] takes another one, so the Env stays
    /// alive for as long as either of them does.
    pub fn set_env(&mut self, env: &Env) {
        unsafe {
            ffi::rocksdb_backup_engine_options_set_env(self.inner, env.0.inner);
        }
        self.backup_env = Some(env.clone());
    }

    /// Sets a rate limiter for backup I/O, which unlike
    /// [`Self::set_backup_rate_limit`] can be tuned beyond a byte budget.
    ///
    /// The limiter created here defaults to limiting writes only. Use
    /// [`Self::set_backup_rate_limiter_with_mode`] to limit reads as well.
    ///
    /// The C API holds the limiter in a `shared_ptr`, so it stays alive for as
    /// long as the options and the engine need it and there is nothing for the
    /// caller to keep.
    pub fn set_backup_rate_limiter(
        &mut self,
        rate_bytes_per_sec: i64,
        refill_period_us: i64,
        fairness: i32,
    ) {
        unsafe {
            let ratelimiter =
                ffi::rocksdb_ratelimiter_create(rate_bytes_per_sec, refill_period_us, fairness);
            ffi::rocksdb_backup_engine_options_set_backup_rate_limiter(self.inner, ratelimiter);
            ffi::rocksdb_ratelimiter_destroy(ratelimiter);
        }
    }

    /// Sets a rate limiter for backup I/O, choosing which operations count
    /// against the limit and whether the limit is auto tuned.
    ///
    /// See [`crate::Options::set_ratelimiter_with_mode`] for what the
    /// parameters mean.
    pub fn set_backup_rate_limiter_with_mode(
        &mut self,
        rate_bytes_per_sec: i64,
        refill_period_us: i64,
        fairness: i32,
        mode: RateLimiterMode,
        auto_tuned: bool,
    ) {
        unsafe {
            let ratelimiter = ffi::rocksdb_ratelimiter_create_with_mode(
                rate_bytes_per_sec,
                refill_period_us,
                fairness,
                mode as c_int,
                auto_tuned,
            );
            ffi::rocksdb_backup_engine_options_set_backup_rate_limiter(self.inner, ratelimiter);
            ffi::rocksdb_ratelimiter_destroy(ratelimiter);
        }
    }

    /// Sets a rate limiter for restore I/O.
    ///
    /// The limiter created here defaults to limiting writes only, which is
    /// what [`Self::set_restore_rate_limit`] already does. Use
    /// [`Self::set_restore_rate_limiter_with_mode`] with
    /// [`RateLimiterMode::KAllIo`] to also limit reads.
    ///
    /// Ownership works as described on [`Self::set_backup_rate_limiter`].
    pub fn set_restore_rate_limiter(
        &mut self,
        rate_bytes_per_sec: i64,
        refill_period_us: i64,
        fairness: i32,
    ) {
        unsafe {
            let ratelimiter =
                ffi::rocksdb_ratelimiter_create(rate_bytes_per_sec, refill_period_us, fairness);
            ffi::rocksdb_backup_engine_options_set_restore_rate_limiter(self.inner, ratelimiter);
            ffi::rocksdb_ratelimiter_destroy(ratelimiter);
        }
    }

    /// Sets a rate limiter for restore I/O, choosing which operations count
    /// against the limit and whether the limit is auto tuned.
    ///
    /// See [`crate::Options::set_ratelimiter_with_mode`] for what the
    /// parameters mean.
    pub fn set_restore_rate_limiter_with_mode(
        &mut self,
        rate_bytes_per_sec: i64,
        refill_period_us: i64,
        fairness: i32,
        mode: RateLimiterMode,
        auto_tuned: bool,
    ) {
        unsafe {
            let ratelimiter = ffi::rocksdb_ratelimiter_create_with_mode(
                rate_bytes_per_sec,
                refill_period_us,
                fairness,
                mode as c_int,
                auto_tuned,
            );
            ffi::rocksdb_backup_engine_options_set_restore_rate_limiter(self.inner, ratelimiter);
            ffi::rocksdb_ratelimiter_destroy(ratelimiter);
        }
    }
}

/// Converts bytes borrowed from a RocksDB `std::string` into a path.
///
/// Unix paths are arbitrary bytes, so this is lossless there. Other platforms
/// have no byte level path constructor, so invalid UTF-8 is replaced.
#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

impl CreateBackupOptions {
    /// If true, flush the memtables before taking the backup, so that writes
    /// that never reached a WAL are not lost. A flush always happens when 2PC
    /// is enabled.
    ///
    /// Default: false
    pub fn set_flush_before_backup(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_create_backup_options_set_flush_before_backup(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the current `flush_before_backup` setting.
    pub fn get_flush_before_backup(&self) -> bool {
        unsafe { ffi::rocksdb_create_backup_options_get_flush_before_backup(self.inner) != 0 }
    }

    /// If true, flush all column families atomically, giving cross column
    /// family consistency without WAL files. Combined with
    /// `BackupEngineOptions::backup_log_files = false` this makes it safe to
    /// skip backing up WALs for a multi column family database.
    ///
    /// Only takes effect when `flush_before_backup` is also true.
    ///
    /// Default: false
    pub fn set_atomic_flush(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_create_backup_options_set_atomic_flush(self.inner, c_uchar::from(val));
        }
    }

    /// Returns the current `atomic_flush` setting.
    pub fn get_atomic_flush(&self) -> bool {
        unsafe { ffi::rocksdb_create_backup_options_get_atomic_flush(self.inner) != 0 }
    }

    /// If false, `background_thread_cpu_priority` is ignored. If true, the
    /// priority of the copy threads can be lowered. Raising it has no effect,
    /// since the threads start at [`CpuPriority::KNormal`].
    ///
    /// Default: false
    pub fn set_decrease_background_thread_cpu_priority(&mut self, val: bool) {
        unsafe {
            ffi::rocksdb_create_backup_options_set_decrease_background_thread_cpu_priority(
                self.inner,
                c_uchar::from(val),
            );
        }
    }

    /// Returns the current `decrease_background_thread_cpu_priority` setting.
    pub fn get_decrease_background_thread_cpu_priority(&self) -> bool {
        unsafe {
            ffi::rocksdb_create_backup_options_get_decrease_background_thread_cpu_priority(
                self.inner,
            ) != 0
        }
    }

    /// CPU priority for the threads that copy files during the backup. Only
    /// used when [`Self::set_decrease_background_thread_cpu_priority`] is true.
    ///
    /// Default: [`CpuPriority::KNormal`]
    pub fn set_background_thread_cpu_priority(&mut self, val: CpuPriority) {
        unsafe {
            ffi::rocksdb_create_backup_options_set_background_thread_cpu_priority(
                self.inner,
                val as c_int,
            );
        }
    }

    /// Returns the current `background_thread_cpu_priority` setting.
    pub fn get_background_thread_cpu_priority(&self) -> CpuPriority {
        unsafe {
            CpuPriority::from(
                ffi::rocksdb_create_backup_options_get_background_thread_cpu_priority(self.inner),
            )
        }
    }

    /// Registers a closure that RocksDB calls every
    /// `BackupEngineOptions::callback_trigger_interval_size` bytes copied.
    ///
    /// The closure is called from the copy threads, not the thread that
    /// started the backup. RocksDB serialises the calls behind a mutex, but
    /// every copy thread holds a copy of the callback at the same time, so it
    /// must be `Send + Sync`. `'static` is required because these options
    /// store the closure without borrowing from the caller.
    ///
    /// Calling this again replaces and frees the previously registered
    /// closure. See the type level docs for why this type owns it.
    ///
    /// A panic out of the closure aborts the process, because a Rust panic
    /// cannot unwind through the C boundary.
    pub fn set_progress_callback<F>(&mut self, callback: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        let holder = Arc::new(ProgressCallback {
            callback: Box::new(callback),
        });
        let state = std::ptr::from_ref::<ProgressCallback>(holder.as_ref())
            .cast::<c_void>()
            .cast_mut();
        unsafe {
            ffi::rocksdb_create_backup_options_set_progress_callback(
                self.inner,
                state,
                Some(progress_callback_trampoline),
            );
        }
        // The C side now points at the new holder, so dropping the old one
        // here cannot leave it with a dangling state pointer.
        self.progress_callback = Some(holder);
    }

    /// Registers a closure that decides which shared files to leave out of the
    /// backup. It is called once per candidate file with the file name relative
    /// to the backup directory, and returning true excludes that file.
    ///
    /// This is an advanced feature. RocksDB trusts the caller to keep the
    /// excluded files somewhere the database can still be restored from, for
    /// example an alternate backup directory. Restoring such a backup needs
    /// those files supplied through `RestoreOptions::alternate_dirs`, which the
    /// C API does not expose, so from this crate the only recovery path is
    /// [`RestoreMode::KKeepLatestDbSessionIdFiles`], which opportunistically
    /// looks for the missing files in the existing database directory.
    ///
    /// Only shared files are ever offered to the closure, so this needs
    /// `share_table_files` and `share_files_with_checksum` to be true. It also
    /// needs `schema_version` to be at least 2, otherwise creating the backup
    /// fails with `Invalid argument: exclude_files_callback requires
    /// schema_version >= 2`.
    ///
    /// `db/c.cc` calls this on the thread driving the backup, but the bound
    /// matches [`Self::set_progress_callback`] so that both closures have the
    /// same requirements.
    ///
    /// Calling this again replaces and frees the previously registered
    /// closure. A panic out of the closure aborts the process.
    pub fn set_exclude_files_callback<F>(&mut self, callback: F)
    where
        F: Fn(&[u8]) -> bool + Send + Sync + 'static,
    {
        let holder = Arc::new(ExcludeFilesCallback {
            callback: Box::new(callback),
        });
        let state = std::ptr::from_ref::<ExcludeFilesCallback>(holder.as_ref())
            .cast::<c_void>()
            .cast_mut();
        unsafe {
            ffi::rocksdb_create_backup_options_set_exclude_files_callback(
                self.inner,
                state,
                Some(exclude_files_callback_trampoline),
            );
        }
        // See `set_progress_callback`.
        self.exclude_files_callback = Some(holder);
    }
}

impl Default for CreateBackupOptions {
    fn default() -> Self {
        unsafe {
            let opts = ffi::rocksdb_create_backup_options_create();
            assert!(
                !opts.is_null(),
                "Could not create RocksDB create backup options"
            );

            Self {
                inner: opts,
                progress_callback: None,
                exclude_files_callback: None,
            }
        }
    }
}

impl RestoreOptions {
    /// Sets `keep_log_files`. If true, restore won't overwrite the existing log files in wal_dir.
    /// It will also move all log files from archive directory to wal_dir. Use this option in
    /// combination with BackupEngineOptions::backup_log_files = false for persisting in-memory
    /// databases.
    ///
    /// Default: false
    pub fn set_keep_log_files(&mut self, keep_log_files: bool) {
        unsafe {
            ffi::rocksdb_restore_options_set_keep_log_files(self.inner, i32::from(keep_log_files));
        }
    }

    /// If true, restore won't overwrite the existing log files in wal_dir. It will also move
    /// all log files from archive directory to wal_dir. Use this option in combination with
    /// BackupEngineOptions::backup_log_files = false for persisting in-memory databases.
    /// Default: false
    pub fn get_keep_log_files(&self) -> bool {
        unsafe { ffi::rocksdb_restore_options_get_keep_log_files(self.inner) != 0 }
    }

    /// Sets how much of the destination database the restore may reuse.
    ///
    /// Default: [`RestoreMode::KPurgeAllFiles`]
    pub fn set_mode(&mut self, mode: RestoreMode) {
        unsafe {
            ffi::rocksdb_restore_options_set_mode(self.inner, mode as c_int);
        }
    }

    /// Returns the current restore mode.
    pub fn get_mode(&self) -> RestoreMode {
        unsafe { RestoreMode::from(ffi::rocksdb_restore_options_get_mode(self.inner)) }
    }
}

impl Default for RestoreOptions {
    fn default() -> Self {
        unsafe {
            let opts = ffi::rocksdb_restore_options_create();
            assert!(!opts.is_null(), "Could not create RocksDB restore options");

            Self { inner: opts }
        }
    }
}

impl Drop for BackupEngine {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_backup_engine_close(self.inner);
        }
    }
}

impl Drop for BackupEngineOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_backup_engine_options_destroy(self.inner);
        }
    }
}

impl Drop for CreateBackupOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_create_backup_options_destroy(self.inner);
        }
        // The closure holders are fields, so they are dropped after this body
        // runs, which is the order that matters: the C++ lambdas holding their
        // addresses are gone by then.
    }
}

impl Drop for RestoreOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_restore_options_destroy(self.inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BackupEngineOptions;

    #[test]
    fn test_sync() {
        let dir = tempfile::Builder::new()
            .prefix("rocksdb-test-sync")
            .tempdir()
            .expect("Failed to create temporary path for db.");

        let mut opts = BackupEngineOptions::new(dir.path()).unwrap();
        assert!(opts.get_sync());
        opts.set_sync(false);
        assert!(!opts.get_sync());
    }
}

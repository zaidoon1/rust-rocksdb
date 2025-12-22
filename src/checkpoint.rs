// Copyright 2018 Eugene P.
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

//! Implementation of bindings to RocksDB Checkpoint[1] API
//!
//! [1]: https://github.com/facebook/rocksdb/wiki/Checkpoints

use crate::db::{DBInner, ExportImportFilesMetaData};
use crate::{ffi, ffi_util::to_cpath, AsColumnFamilyRef, DBCommon, Error, ThreadMode};
use std::{marker::PhantomData, path::Path};

/// Default value for the `log_size_for_flush` parameter passed to
/// `ffi::rocksdb_checkpoint_create`.
///
/// A value of `0` forces RocksDB to flush memtables as needed before creating
/// the checkpoint. This helps ensure the checkpoint includes the most recent
/// writes (which may still be in memtables at the time of checkpoint creation).
///
/// Forcing a flush can create new SST file(s), potentially very small L0 SSTs
/// if little data has been written since the last flush.
const DEFAULT_LOG_SIZE_FOR_FLUSH: u64 = 0_u64;

/// Database's checkpoint object.
/// Used to create checkpoints of the specified DB from time to time.
pub struct Checkpoint<'db> {
    inner: *mut ffi::rocksdb_checkpoint_t,
    _db: PhantomData<&'db ()>,
}

impl<'db> Checkpoint<'db> {
    /// Creates new checkpoint object for specific DB.
    ///
    /// Does not actually produce checkpoints, call `.create_checkpoint()` method to produce
    /// a DB checkpoint.
    pub fn new<T: ThreadMode, I: DBInner>(db: &'db DBCommon<T, I>) -> Result<Self, Error> {
        let checkpoint: *mut ffi::rocksdb_checkpoint_t;

        unsafe {
            checkpoint = ffi_try!(ffi::rocksdb_checkpoint_object_create(db.inner.inner()));
        }

        if checkpoint.is_null() {
            return Err(Error::new("Could not create checkpoint object.".to_owned()));
        }

        Ok(Self {
            inner: checkpoint,
            _db: PhantomData,
        })
    }

    /// Creates a new physical RocksDB checkpoint in the directory specified by `path`.
    ///
    /// A checkpoint is a consistent, read-only view of the database at a specific
    /// point in time. Internally, RocksDB creates a new MANIFEST and metadata files
    /// and hard-links the relevant SST files, making the checkpoint efficient to
    /// create and safe to keep for long-lived reads.
    ///
    /// This method uses the default `log_size_for_flush` value (`0`), which instructs
    /// RocksDB to flush memtables as needed before creating the checkpoint. Forcing
    /// a flush ensures that the checkpoint includes the most recent writes that may
    /// still reside in memtables at the time of checkpoint creation.
    ///
    /// Forcing a flush may create new SST file(s), including very small L0 SSTs if
    /// little data has been written since the last flush. Applications that create
    /// checkpoints frequently or during periods of low write volume may wish to
    /// control this behavior by using an API that allows specifying
    /// `log_size_for_flush`.
    ///
    /// Note:
    /// - Checkpoints are always SST-based and never depend on WAL files or live
    ///   memtables when opened.
    /// - If writes are performed with WAL disabled, forcing a flush is required to
    ///   ensure those writes appear in the checkpoint.
    /// - When using RocksDB TransactionDB with two-phase commit (2PC), RocksDB will
    ///   always flush regardless of the `log_size_for_flush` setting.
    pub fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        let c_path = to_cpath(path)?;
        unsafe {
            ffi_try!(ffi::rocksdb_checkpoint_create(
                self.inner,
                c_path.as_ptr(),
                DEFAULT_LOG_SIZE_FOR_FLUSH,
            ));
        }
        Ok(())
    }

    /// Creates a new physical DB checkpoint in `path`, allowing the caller to
    /// control `log_size_for_flush`.
    ///
    /// `log_size_for_flush` is forwarded to RocksDB's Checkpoint API:
    /// - `0` forces a flush as needed before checkpoint creation, which helps the
    ///   checkpoint include the latest writes; this may create new SST file(s).
    /// - A non-zero value:
    ///   - **Expected behavior** (once RocksDB bug is fixed): Only forces a flush
    ///     if the total WAL size exceeds the specified threshold. When a flush is
    ///     not forced and WAL writing is enabled, RocksDB includes WAL files in
    ///     the checkpoint that are replayed on open to reconstruct recent writes.
    ///     This avoids creating small SST files during periods of low write volume,
    ///     at the cost of additional checkpoint storage space for the copied WAL.
    ///   - **Current behavior** (RocksDB bug): Never flushes, regardless of WAL
    ///     size. The checkpoint will always include WAL files instead of flushing
    ///     to SST. See: <https://github.com/facebook/rocksdb/pull/14193>
    ///
    /// In practice, using a non-zero value means checkpoints may represent an
    /// *older, fully materialized database state* rather than the instantaneous
    /// state at the time the checkpoint is created.
    ///
    /// Note:
    /// - If writes are performed with WAL disabled, using a non-zero
    ///   `log_size_for_flush` may cause those writes to be absent from
    ///   the checkpoint.
    /// - When using RocksDB TransactionDB with two-phase commit (2PC),
    ///   RocksDB will always flush regardless of `log_size_for_flush`.
    pub fn create_checkpoint_with_log_size<P: AsRef<Path>>(
        &self,
        path: P,
        log_size_for_flush: u64,
    ) -> Result<(), Error> {
        let c_path = to_cpath(path)?;
        unsafe {
            ffi_try!(ffi::rocksdb_checkpoint_create(
                self.inner,
                c_path.as_ptr(),
                log_size_for_flush,
            ));
        }
        Ok(())
    }

    /// Export a specified Column Family
    ///
    /// Creates copies of the live SST files at the specified export path.
    ///
    /// - SST files will be created as hard links when the directory specified
    ///   is in the same partition as the db directory, copied otherwise.
    /// - the path must not yet exist - a new directory will be created as part of the export.
    /// - Always triggers a flush.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use rust_rocksdb::{DB, checkpoint::Checkpoint};
    ///
    /// fn export_column_family(db: &DB, column_family_name: &str, export_path: &str) {
    ///    let cp = Checkpoint::new(&db).unwrap();
    ///    let cf = db.cf_handle(column_family_name).unwrap();
    ///
    ///    let export_metadata = cp.export_column_family(&cf, export_path).unwrap();
    ///
    ///    assert!(export_metadata.get_files().len() > 0);
    /// }
    /// ```
    ///
    /// See also: [`DB::create_column_family_with_import`](crate::DB::create_column_family_with_import).
    pub fn export_column_family<P: AsRef<Path>>(
        &self,
        column_family: &impl AsColumnFamilyRef,
        path: P,
    ) -> Result<ExportImportFilesMetaData, Error> {
        let c_path = to_cpath(path)?;
        let column_family_handle = column_family.inner();
        let metadata = unsafe {
            ffi_try!(ffi::rocksdb_checkpoint_export_column_family(
                self.inner,
                column_family_handle,
                c_path.as_ptr(),
            ))
        };
        Ok(ExportImportFilesMetaData { inner: metadata })
    }
}

impl Drop for Checkpoint<'_> {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_checkpoint_object_destroy(self.inner);
        }
    }
}

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

/// Undocumented parameter for `ffi::rocksdb_checkpoint_create` function. Zero by default.
const LOG_SIZE_FOR_FLUSH: u64 = 0_u64;

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

    /// Creates new physical DB checkpoint in directory specified by `path`.
    pub fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        let c_path = to_cpath(path)?;
        unsafe {
            ffi_try!(ffi::rocksdb_checkpoint_create(
                self.inner,
                c_path.as_ptr(),
                LOG_SIZE_FOR_FLUSH,
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

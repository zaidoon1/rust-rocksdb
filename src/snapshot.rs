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

use crate::{
    AsColumnFamilyRef, DB, DBIteratorWithThreadMode, DBPinnableSlice, DBRawIteratorWithThreadMode,
    Error, IteratorMode, ReadOptions, db::DBAccess, ffi,
};

/// A type alias to keep compatibility. See [`SnapshotWithThreadMode`] for details
pub type Snapshot<'a> = SnapshotWithThreadMode<'a, DB>;

/// Reusable read state bound to a [`SnapshotWithThreadMode`].
///
/// Creating this value configures one native [`ReadOptions`] with the snapshot.
/// Its get and multi-get methods reuse those options until the session is
/// dropped. Custom options can be supplied with
/// [`SnapshotWithThreadMode::read_options_opt`].
///
/// The session cannot outlive its snapshot:
///
/// ```compile_fail,E0597
/// use rust_rocksdb::DB;
///
/// let db = DB::open_default("foo").unwrap();
/// let _read_options = {
///     let snapshot = db.snapshot();
///     snapshot.read_options()
/// };
/// ```
pub struct SnapshotReadOptions<'snapshot, 'db, D: DBAccess = DB> {
    snapshot: &'snapshot SnapshotWithThreadMode<'db, D>,
    readopts: ReadOptions,
}

/// A consistent view of the database at the point of creation.
///
/// # Examples
///
/// ```
/// use rust_rocksdb::{DB, IteratorMode, Options};
///
/// let tempdir = tempfile::Builder::new()
///     .prefix("_path_for_rocksdb_storage3")
///     .tempdir()
///     .expect("Failed to create temporary path for the _path_for_rocksdb_storage3");
/// let path = tempdir.path();
/// {
///     let db = DB::open_default(path).unwrap();
///     let snapshot = db.snapshot(); // Creates a longer-term snapshot of the DB, but closed when goes out of scope
///     let mut iter = snapshot.iterator(IteratorMode::Start); // Make as many iterators as you'd like from one snapshot
/// }
/// let _ = DB::destroy(&Options::default(), path);
/// ```
///
/// A `Snapshot` must not outlive the `DB` it was created from:
///
/// ```compile_fail,E0597
/// use rust_rocksdb::DB;
///
/// let _snapshot = {
///     let db = DB::open_default("foo").unwrap();
///     db.snapshot()
/// };
/// ```
pub struct SnapshotWithThreadMode<'a, D: DBAccess> {
    db: &'a D,
    pub(crate) inner: *const ffi::rocksdb_snapshot_t,
}

impl<'a, D: DBAccess> SnapshotWithThreadMode<'a, D> {
    /// Creates a new `SnapshotWithThreadMode` of the database `db`.
    pub fn new(db: &'a D) -> Self {
        let snapshot = unsafe { db.create_snapshot() };
        Self {
            db,
            inner: snapshot,
        }
    }

    /// Returns the sequence number of the snapshot.
    pub fn sequence_number(&self) -> u64 {
        unsafe { ffi::rocksdb_snapshot_get_sequence_number(self.inner) }
    }

    /// Creates reusable default read options bound to this snapshot.
    pub fn read_options(&'_ self) -> SnapshotReadOptions<'_, 'a, D> {
        self.read_options_opt(ReadOptions::default())
    }

    /// Creates reusable custom read options bound to this snapshot.
    ///
    /// Any snapshot already configured on `readopts` is replaced with this
    /// snapshot.
    pub fn read_options_opt(&'_ self, mut readopts: ReadOptions) -> SnapshotReadOptions<'_, 'a, D> {
        readopts.set_snapshot(self);
        SnapshotReadOptions {
            snapshot: self,
            readopts,
        }
    }

    /// Creates an iterator over the data in this snapshot, using the default read options.
    pub fn iterator(&self, mode: IteratorMode) -> DBIteratorWithThreadMode<'a, D> {
        let readopts = ReadOptions::default();
        self.iterator_opt(mode, readopts)
    }

    /// Creates an iterator over the data in this snapshot under the given column family, using
    /// the default read options.
    pub fn iterator_cf(
        &'_ self,
        cf_handle: &impl AsColumnFamilyRef,
        mode: IteratorMode,
    ) -> DBIteratorWithThreadMode<'_, D> {
        let readopts = ReadOptions::default();
        self.iterator_cf_opt(cf_handle, readopts, mode)
    }

    /// Creates an iterator over the data in this snapshot, using the given read options.
    pub fn iterator_opt(
        &self,
        mode: IteratorMode,
        mut readopts: ReadOptions,
    ) -> DBIteratorWithThreadMode<'a, D> {
        readopts.set_snapshot(self);
        DBIteratorWithThreadMode::<D>::new(self.db, readopts, mode)
    }

    /// Creates an iterator over the data in this snapshot under the given column family, using
    /// the given read options.
    pub fn iterator_cf_opt(
        &'_ self,
        cf_handle: &impl AsColumnFamilyRef,
        mut readopts: ReadOptions,
        mode: IteratorMode,
    ) -> DBIteratorWithThreadMode<'_, D> {
        readopts.set_snapshot(self);
        DBIteratorWithThreadMode::new_cf(self.db, cf_handle.inner(), readopts, mode)
    }

    /// Creates a raw iterator over the data in this snapshot, using the default read options.
    pub fn raw_iterator(&'_ self) -> DBRawIteratorWithThreadMode<'_, D> {
        let readopts = ReadOptions::default();
        self.raw_iterator_opt(readopts)
    }

    /// Creates a raw iterator over the data in this snapshot under the given column family, using
    /// the default read options.
    pub fn raw_iterator_cf(
        &'_ self,
        cf_handle: &impl AsColumnFamilyRef,
    ) -> DBRawIteratorWithThreadMode<'_, D> {
        let readopts = ReadOptions::default();
        self.raw_iterator_cf_opt(cf_handle, readopts)
    }

    /// Creates a raw iterator over the data in this snapshot, using the given read options.
    pub fn raw_iterator_opt(
        &'_ self,
        mut readopts: ReadOptions,
    ) -> DBRawIteratorWithThreadMode<'_, D> {
        readopts.set_snapshot(self);
        DBRawIteratorWithThreadMode::new(self.db, readopts)
    }

    /// Creates a raw iterator over the data in this snapshot under the given column family, using
    /// the given read options.
    pub fn raw_iterator_cf_opt(
        &'_ self,
        cf_handle: &impl AsColumnFamilyRef,
        mut readopts: ReadOptions,
    ) -> DBRawIteratorWithThreadMode<'_, D> {
        readopts.set_snapshot(self);
        DBRawIteratorWithThreadMode::new_cf(self.db, cf_handle.inner(), readopts)
    }

    /// Returns the bytes associated with a key value with default read options.
    pub fn get<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Error> {
        self.read_options().get(key)
    }

    /// Returns the bytes associated with a key value and given column family with default read
    /// options.
    pub fn get_cf<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
    ) -> Result<Option<Vec<u8>>, Error> {
        self.read_options().get_cf(cf, key)
    }

    /// Returns the bytes associated with a key value and given read options.
    pub fn get_opt<K: AsRef<[u8]>>(
        &self,
        key: K,
        readopts: ReadOptions,
    ) -> Result<Option<Vec<u8>>, Error> {
        self.read_options_opt(readopts).get(key)
    }

    /// Returns the bytes associated with a key value, given column family and read options.
    pub fn get_cf_opt<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: ReadOptions,
    ) -> Result<Option<Vec<u8>>, Error> {
        self.read_options_opt(readopts).get_cf(cf, key)
    }

    /// Return the value associated with a key using RocksDB's PinnableSlice
    /// so as to avoid unnecessary memory copy. Similar to get_pinned_opt but
    /// leverages default options.
    pub fn get_pinned<K: AsRef<[u8]>>(
        &'_ self,
        key: K,
    ) -> Result<Option<DBPinnableSlice<'_>>, Error> {
        self.read_options().get_pinned(key)
    }

    /// Return the value associated with a key using RocksDB's PinnableSlice
    /// so as to avoid unnecessary memory copy. Similar to get_pinned_cf_opt but
    /// leverages default options.
    pub fn get_pinned_cf<K: AsRef<[u8]>>(
        &'_ self,
        cf: &impl AsColumnFamilyRef,
        key: K,
    ) -> Result<Option<DBPinnableSlice<'_>>, Error> {
        self.read_options().get_pinned_cf(cf, key)
    }

    /// Return the value associated with a key using RocksDB's PinnableSlice
    /// so as to avoid unnecessary memory copy.
    pub fn get_pinned_opt<K: AsRef<[u8]>>(
        &'_ self,
        key: K,
        readopts: ReadOptions,
    ) -> Result<Option<DBPinnableSlice<'_>>, Error> {
        self.read_options_opt(readopts).get_pinned(key)
    }

    /// Return the value associated with a key using RocksDB's PinnableSlice
    /// so as to avoid unnecessary memory copy. Similar to get_pinned_opt but
    /// allows specifying ColumnFamily.
    pub fn get_pinned_cf_opt<K: AsRef<[u8]>>(
        &'_ self,
        cf: &impl AsColumnFamilyRef,
        key: K,
        readopts: ReadOptions,
    ) -> Result<Option<DBPinnableSlice<'_>>, Error> {
        self.read_options_opt(readopts).get_pinned_cf(cf, key)
    }

    /// Returns the bytes associated with the given key values and default read options.
    pub fn multi_get<K: AsRef<[u8]>, I>(&self, keys: I) -> Vec<Result<Option<Vec<u8>>, Error>>
    where
        I: IntoIterator<Item = K>,
    {
        self.read_options().multi_get(keys)
    }

    /// Returns the bytes associated with the given key values and default read options.
    pub fn multi_get_cf<'b, K, I, W>(&self, keys_cf: I) -> Vec<Result<Option<Vec<u8>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = (&'b W, K)>,
        W: AsColumnFamilyRef + 'b,
    {
        self.read_options().multi_get_cf(keys_cf)
    }

    /// Returns the bytes associated with the given key values and given read options.
    pub fn multi_get_opt<K, I>(
        &self,
        keys: I,
        readopts: ReadOptions,
    ) -> Vec<Result<Option<Vec<u8>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        self.read_options_opt(readopts).multi_get(keys)
    }

    /// Returns the bytes associated with the given key values, given column family and read options.
    pub fn multi_get_cf_opt<'b, K, I, W>(
        &self,
        keys_cf: I,
        readopts: ReadOptions,
    ) -> Vec<Result<Option<Vec<u8>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = (&'b W, K)>,
        W: AsColumnFamilyRef + 'b,
    {
        self.read_options_opt(readopts).multi_get_cf(keys_cf)
    }
}

impl<'db, D: DBAccess> SnapshotReadOptions<'_, 'db, D> {
    /// Returns the bytes associated with a key.
    pub fn get<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Error> {
        self.snapshot.db.get_opt(key, &self.readopts)
    }

    /// Returns the bytes associated with a key in a column family.
    pub fn get_cf<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
    ) -> Result<Option<Vec<u8>>, Error> {
        self.snapshot.db.get_cf_opt(cf, key, &self.readopts)
    }

    /// Returns a pinned value associated with a key.
    pub fn get_pinned<K: AsRef<[u8]>>(
        &self,
        key: K,
    ) -> Result<Option<DBPinnableSlice<'db>>, Error> {
        let db: &'db D = self.snapshot.db;
        db.get_pinned_opt(key, &self.readopts)
    }

    /// Returns a pinned value associated with a key in a column family.
    pub fn get_pinned_cf<K: AsRef<[u8]>>(
        &self,
        cf: &impl AsColumnFamilyRef,
        key: K,
    ) -> Result<Option<DBPinnableSlice<'db>>, Error> {
        let db: &'db D = self.snapshot.db;
        db.get_pinned_cf_opt(cf, key, &self.readopts)
    }

    /// Returns the values associated with the given keys.
    pub fn multi_get<K, I>(&self, keys: I) -> Vec<Result<Option<Vec<u8>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = K>,
    {
        self.snapshot.db.multi_get_opt(keys, &self.readopts)
    }

    /// Returns the values associated with the given keys and column families.
    pub fn multi_get_cf<'b, K, I, W>(&self, keys_cf: I) -> Vec<Result<Option<Vec<u8>>, Error>>
    where
        K: AsRef<[u8]>,
        I: IntoIterator<Item = (&'b W, K)>,
        W: AsColumnFamilyRef + 'b,
    {
        self.snapshot.db.multi_get_cf_opt(keys_cf, &self.readopts)
    }
}

impl<D: DBAccess> Drop for SnapshotWithThreadMode<'_, D> {
    fn drop(&mut self) {
        unsafe {
            self.db.release_snapshot(self.inner);
        }
    }
}

/// `Send` and `Sync` implementations for `SnapshotWithThreadMode` are safe, because `SnapshotWithThreadMode` is
/// immutable and can be safely shared between threads.
unsafe impl<D: DBAccess + Sync> Send for SnapshotWithThreadMode<'_, D> {}
unsafe impl<D: DBAccess + Sync> Sync for SnapshotWithThreadMode<'_, D> {}

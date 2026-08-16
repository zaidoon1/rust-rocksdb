//! Boundaries that compaction must cut SST files on.
//!
//! An SST partitioner is consulted for every key a compaction writes. When it
//! reports a boundary, the compaction closes the output file it is filling and
//! starts a new one, so the keys on either side of the boundary never share a
//! file. Trivial moves are held to the same rule: a file only moves down a
//! level untouched if the partitioner says its first and last key belong
//! together.
//!
//! The point is to line file boundaries up with something the application
//! cares about, usually a key prefix that identifies a tenant, a shard, or a
//! table. Once a prefix owns whole files, work scoped to that prefix gets
//! cheaper. [`DB::delete_file_in_range`](crate::DB::delete_file_in_range) only
//! drops files that sit entirely inside the range, so a partitioned prefix can
//! be deleted by unlinking files rather than by writing tombstones that later
//! compactions have to carry. Promoting one prefix to the next level stops
//! dragging its neighbours along, which is the write amplification argument
//! upstream makes for the feature.
//!
//! Partitioning only shapes files that future compactions produce. Files
//! already on disk keep whatever boundaries they were written with, and are
//! only re-cut when a compaction happens to rewrite them.
//!
//! Upstream marks the feature experimental in `options.h`.

use std::ptr::NonNull;

use crate::ffi;

/// A factory RocksDB asks for a partitioner at the start of every compaction.
///
/// Install one with
/// [`Options::set_sst_partitioner_factory`](crate::Options::set_sst_partitioner_factory).
/// The setter copies the underlying `shared_ptr`, so a factory can be installed
/// on any number of `Options` and dropped as soon as the last call returns.
pub struct SstPartitionerFactory {
    inner: NonNull<ffi::rocksdb_sst_partitioner_factory_t>,
}

impl SstPartitionerFactory {
    /// Cuts a new SST file whenever the first `prefix_len` bytes of the user
    /// key change.
    ///
    /// Keys shorter than `prefix_len` are compared whole, so a short key only
    /// groups with keys that share all of it. `prefix_len` of 0 makes every key
    /// compare equal and never forces a cut, which is the same as installing no
    /// partitioner.
    ///
    /// The comparison is over raw bytes of the user key and ignores the column
    /// family comparator, so this is a fit for fixed width prefixes such as a
    /// tenant id, not for prefixes an application defines through a
    /// [`SliceTransform`](crate::SliceTransform).
    ///
    /// Wraps `NewSstPartitionerFixedPrefixFactory`.
    #[must_use]
    pub fn fixed_prefix(prefix_len: usize) -> Self {
        let inner = unsafe { ffi::rocksdb_sst_partitioner_fixed_prefix_factory_create(prefix_len) };
        Self {
            inner: NonNull::new(inner)
                .expect("rocksdb_sst_partitioner_fixed_prefix_factory_create returned null"),
        }
    }

    pub(crate) fn as_ptr(&self) -> *mut ffi::rocksdb_sst_partitioner_factory_t {
        self.inner.as_ptr()
    }
}

impl Drop for SstPartitionerFactory {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_sst_partitioner_factory_destroy(self.inner.as_ptr());
        }
    }
}

// `rocksdb_sst_partitioner_factory_t` is a `std::shared_ptr<SstPartitionerFactory>`
// and nothing else (c.cc:501). Destroying the handle only drops that one
// reference, and the only other operation this crate performs on it is the
// refcount bump in `rocksdb_options_set_sst_partitioner_factory` (c.cc:6142),
// which is atomic. So the handle carries no thread affinity, and concurrent
// reads through `&SstPartitionerFactory` cannot race.
unsafe impl Send for SstPartitionerFactory {}
unsafe impl Sync for SstPartitionerFactory {}

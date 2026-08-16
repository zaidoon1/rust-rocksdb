//! Per-file I/O settings for the APIs that open a file outside of a DB.
//!
//! [`EnvOptions`] is RocksDB's `EnvOptions`. A DB derives one for every file
//! it opens from its own [`Options`](crate::Options), so most applications
//! never build one by hand. It matters for the APIs that open a file with no
//! `Options` to derive from, such as [`SstFileWriter`](crate::SstFileWriter)
//! and the trace readers and writers.

use libc::c_uchar;

use crate::ffi;

/// Per-file I/O settings handed to an [`Env`](crate::Env) when it opens a file.
///
/// A fresh instance carries the values a default `DBOptions` would produce.
/// Each setter below names the default it starts from.
///
/// See `rocksdb/include/rocksdb/env.h`.
pub struct EnvOptions {
    pub(crate) inner: *mut ffi::rocksdb_envoptions_t,
}

// Safety note matches the other options types in `db_options.rs`: the inner
// pointer is never aliased, and RocksDB keeps no thread-local state for
// `EnvOptions`. Every mutation goes through `&mut self`, so there is no
// interior mutability for `Sync` to worry about.
unsafe impl Send for EnvOptions {}
unsafe impl Sync for EnvOptions {}

impl Default for EnvOptions {
    fn default() -> Self {
        let inner = unsafe { ffi::rocksdb_envoptions_create() };
        assert!(!inner.is_null(), "Could not create RocksDB env options");

        Self { inner }
    }
}

impl Drop for EnvOptions {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_envoptions_destroy(self.inner);
        }
    }
}

impl EnvOptions {
    /// Returns the raw pointer the C API reads these options through.
    pub(crate) fn as_ptr(&self) -> *const ffi::rocksdb_envoptions_t {
        self.inner
    }

    /// If true, reads go through `mmap` instead of `read`.
    ///
    /// Not recommended on a 32-bit OS, where address space runs out well
    /// before the data does.
    ///
    /// The POSIX file system ignores
    /// [`set_use_direct_reads`](Self::set_use_direct_reads) while this is on.
    ///
    /// Default: false
    pub fn set_use_mmap_reads(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_envoptions_set_use_mmap_reads(self.inner, c_uchar::from(v));
        }
    }

    /// Returns the current `use_mmap_reads` setting.
    ///
    /// See [`Self::set_use_mmap_reads`] for what this controls.
    pub fn get_use_mmap_reads(&self) -> bool {
        unsafe { ffi::rocksdb_envoptions_get_use_mmap_reads(self.inner) != 0 }
    }

    /// If true, writes go through `mmap` instead of `write`.
    ///
    /// The POSIX file system ignores
    /// [`set_use_direct_writes`](Self::set_use_direct_writes) while this is on.
    ///
    /// Default: false
    pub fn set_use_mmap_writes(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_envoptions_set_use_mmap_writes(self.inner, c_uchar::from(v));
        }
    }

    /// Returns the current `use_mmap_writes` setting.
    ///
    /// See [`Self::set_use_mmap_writes`] for what this controls.
    pub fn get_use_mmap_writes(&self) -> bool {
        unsafe { ffi::rocksdb_envoptions_get_use_mmap_writes(self.inner) != 0 }
    }

    /// If true, read this file with direct I/O, bypassing the page cache.
    ///
    /// This is the file-level flag, not [`Options::set_use_direct_reads`].
    /// The DB option decides what RocksDB puts into the `EnvOptions` it builds
    /// for its own files. This one applies to whatever the caller opens with
    /// these options, and nothing propagates between the two.
    ///
    /// On POSIX, Linux and friends pass `O_DIRECT` at open time while macOS
    /// sets `F_NOCACHE` on the descriptor instead. Either way the flag is
    /// ignored while [`set_use_mmap_reads`](Self::set_use_mmap_reads) is on.
    ///
    /// Default: false
    ///
    /// [`Options::set_use_direct_reads`]: crate::Options::set_use_direct_reads
    pub fn set_use_direct_reads(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_envoptions_set_use_direct_reads(self.inner, c_uchar::from(v));
        }
    }

    /// Returns the current `use_direct_reads` setting.
    ///
    /// See [`Self::set_use_direct_reads`] for what this controls.
    pub fn get_use_direct_reads(&self) -> bool {
        unsafe { ffi::rocksdb_envoptions_get_use_direct_reads(self.inner) != 0 }
    }

    /// If true, write this file with direct I/O, bypassing the page cache.
    ///
    /// This is the file-level flag, not
    /// [`Options::set_use_direct_io_for_flush_and_compaction`]. The DB option
    /// decides what RocksDB puts into the `EnvOptions` it builds for its own
    /// files. This one applies to whatever the caller opens with these
    /// options, and nothing propagates between the two.
    ///
    /// On POSIX, Linux and friends pass `O_DIRECT` at open time while macOS
    /// sets `F_NOCACHE` on the descriptor instead. Either way the flag is
    /// ignored while [`set_use_mmap_writes`](Self::set_use_mmap_writes) is on.
    ///
    /// Default: false
    ///
    /// [`Options::set_use_direct_io_for_flush_and_compaction`]: crate::Options::set_use_direct_io_for_flush_and_compaction
    pub fn set_use_direct_writes(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_envoptions_set_use_direct_writes(self.inner, c_uchar::from(v));
        }
    }

    /// Returns the current `use_direct_writes` setting.
    ///
    /// See [`Self::set_use_direct_writes`] for what this controls.
    pub fn get_use_direct_writes(&self) -> bool {
        unsafe { ffi::rocksdb_envoptions_get_use_direct_writes(self.inner) != 0 }
    }

    /// If false, RocksDB skips the `fallocate` calls it would otherwise make
    /// to preallocate space for a file.
    ///
    /// Linux only. Builds without `fallocate` compile the calls out, so the
    /// setting has no effect there.
    ///
    /// Default: true
    pub fn set_allow_fallocate(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_envoptions_set_allow_fallocate(self.inner, c_uchar::from(v));
        }
    }

    /// Returns the current `allow_fallocate` setting.
    ///
    /// See [`Self::set_allow_fallocate`] for what this controls.
    pub fn get_allow_fallocate(&self) -> bool {
        unsafe { ffi::rocksdb_envoptions_get_allow_fallocate(self.inner) != 0 }
    }

    /// If true, opened descriptors get `FD_CLOEXEC` so they are not inherited
    /// across `exec`.
    ///
    /// This maps to `EnvOptions::set_fd_cloexec`. POSIX only, ignored on
    /// Windows.
    ///
    /// Default: true
    pub fn set_fd_cloexec(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_envoptions_set_fd_cloexec(self.inner, c_uchar::from(v));
        }
    }

    /// Returns the current `fd_cloexec` setting.
    ///
    /// See [`Self::set_fd_cloexec`] for what this controls.
    pub fn get_fd_cloexec(&self) -> bool {
        unsafe { ffi::rocksdb_envoptions_get_fd_cloexec(self.inner) != 0 }
    }

    /// Asks the OS to write the file back to disk incrementally, one request
    /// per this many bytes written. 0 turns it off.
    ///
    /// This spreads writeback out instead of leaving it all for the final
    /// sync. It is not a durability guarantee on its own.
    ///
    /// Default: 0
    pub fn set_bytes_per_sync(&mut self, v: u64) {
        unsafe {
            ffi::rocksdb_envoptions_set_bytes_per_sync(self.inner, v);
        }
    }

    /// Returns the current `bytes_per_sync` setting in bytes.
    ///
    /// See [`Self::set_bytes_per_sync`] for what this controls.
    pub fn get_bytes_per_sync(&self) -> u64 {
        unsafe { ffi::rocksdb_envoptions_get_bytes_per_sync(self.inner) }
    }

    /// When true, guarantees at most [`bytes_per_sync`](Self::set_bytes_per_sync)
    /// bytes are queued for writeback at any moment.
    ///
    /// On Linux this waits for the previous `sync_file_range` to finish before
    /// issuing the next one, so compression and other processing keep running
    /// in the gap and only I/O falling behind blocks. Everywhere else it falls
    /// back to a blocking `WritableFile::Sync`, which stops processing and
    /// I/O from overlapping at all.
    ///
    /// This adds no durability guarantee, because `sync_file_range` does not
    /// write out metadata.
    ///
    /// Default: false
    pub fn set_strict_bytes_per_sync(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_envoptions_set_strict_bytes_per_sync(self.inner, c_uchar::from(v));
        }
    }

    /// Returns the current `strict_bytes_per_sync` setting.
    ///
    /// See [`Self::set_strict_bytes_per_sync`] for what this controls.
    pub fn get_strict_bytes_per_sync(&self) -> bool {
        unsafe { ffi::rocksdb_envoptions_get_strict_bytes_per_sync(self.inner) != 0 }
    }

    /// If true, preallocation passes `FALLOC_FL_KEEP_SIZE`, so the reported
    /// file size does not grow with the preallocated space. If false,
    /// preallocation grows the file size too, which is faster for workloads
    /// that sync on every write.
    ///
    /// Linux only, and only when [`allow_fallocate`](Self::set_allow_fallocate)
    /// is on. RocksDB itself uses true for MANIFEST writes and false for WAL
    /// writes.
    ///
    /// Default: true
    pub fn set_fallocate_with_keep_size(&mut self, v: bool) {
        unsafe {
            ffi::rocksdb_envoptions_set_fallocate_with_keep_size(self.inner, c_uchar::from(v));
        }
    }

    /// Returns the current `fallocate_with_keep_size` setting.
    ///
    /// See [`Self::set_fallocate_with_keep_size`] for what this controls.
    pub fn get_fallocate_with_keep_size(&self) -> bool {
        unsafe { ffi::rocksdb_envoptions_get_fallocate_with_keep_size(self.inner) != 0 }
    }

    /// If non-zero, compaction reads this many bytes at a time. On spinning
    /// disks this turns compaction's random reads into sequential ones.
    ///
    /// Default: 2 MiB
    pub fn set_compaction_readahead_size(&mut self, v: usize) {
        unsafe {
            ffi::rocksdb_envoptions_set_compaction_readahead_size(self.inner, v);
        }
    }

    /// Returns the current `compaction_readahead_size` setting in bytes.
    ///
    /// See [`Self::set_compaction_readahead_size`] for what this controls.
    pub fn get_compaction_readahead_size(&self) -> usize {
        unsafe { ffi::rocksdb_envoptions_get_compaction_readahead_size(self.inner) }
    }

    /// Caps the buffer `WritableFileWriter` uses.
    ///
    /// With buffered I/O the buffer grows up to this limit. With direct I/O it
    /// is fixed at this size, so that write requests stay aligned even when
    /// the logical sector size is unusual.
    ///
    /// Default: 1 MiB
    pub fn set_writable_file_max_buffer_size(&mut self, v: usize) {
        unsafe {
            ffi::rocksdb_envoptions_set_writable_file_max_buffer_size(self.inner, v);
        }
    }

    /// Returns the current `writable_file_max_buffer_size` setting in bytes.
    ///
    /// See [`Self::set_writable_file_max_buffer_size`] for what this controls.
    pub fn get_writable_file_max_buffer_size(&self) -> usize {
        unsafe { ffi::rocksdb_envoptions_get_writable_file_max_buffer_size(self.inner) }
    }

    /// Rate limits writes to files opened with these options.
    ///
    /// The arguments match [`Options::set_ratelimiter`], and so does the
    /// handling: the C API's `rocksdb_ratelimiter_t` is only a handle around a
    /// `shared_ptr<RateLimiter>`, and `rocksdb_envoptions_set_rate_limiter`
    /// copies that `shared_ptr` into the options object (see
    /// `rocksdb_envoptions_t::rate_limiter` in `db/c.cc`). The limiter stays
    /// alive as long as these options do, so the handle is destroyed right
    /// after the call.
    ///
    /// There is no getter and no way to clear this. The C API exposes only the
    /// setter.
    ///
    /// Default: no limit
    ///
    /// [`Options::set_ratelimiter`]: crate::Options::set_ratelimiter
    pub fn set_ratelimiter(
        &mut self,
        rate_bytes_per_sec: i64,
        refill_period_us: i64,
        fairness: i32,
    ) {
        unsafe {
            let ratelimiter =
                ffi::rocksdb_ratelimiter_create(rate_bytes_per_sec, refill_period_us, fairness);
            ffi::rocksdb_envoptions_set_rate_limiter(self.inner, ratelimiter);
            ffi::rocksdb_ratelimiter_destroy(ratelimiter);
        }
    }
}

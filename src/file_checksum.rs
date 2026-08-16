//! Whole file checksums recorded in the manifest.
//!
//! With a checksum generator factory installed, RocksDB hashes every SST file
//! as it writes it and stores the digest and the generator's name in the
//! manifest alongside the file entry. Everything that claims to verify file
//! checksums reads those manifest entries:
//!
//! * `DB::VerifyFileChecksums` rehashes every live SST and blob file and
//!   compares against the manifest.
//! * Backup reuses the recorded digest instead of hashing the source file
//!   again, and checks the copy against it.
//! * Ingestion with
//!   [`IngestExternalFileOptions::set_verify_file_checksum`](crate::IngestExternalFileOptions::set_verify_file_checksum)
//!   checks the digest that came with the incoming file.
//!
//! `file_checksum_gen_factory` defaults to null, and none of that works without
//! it. `VerifyFileChecksums` refuses to run at all and returns
//! `InvalidArgument`, and backup goes back to hashing files itself.
//!
//! Turning the factory on does not backfill. Files already on disk carry no
//! digest, verification skips them one by one without complaining, and they
//! only pick one up when a compaction rewrites them.
//!
//! This is separate from the per block checksum that
//! [`BlockBasedOptions::set_checksum_type`](crate::BlockBasedOptions::set_checksum_type)
//! controls. Block checksums are stored inside the SST and verified on every
//! read, so readers detect block corruption whether or not a file checksum
//! exists. A file checksum covers the whole file in one digest, which is what
//! makes it useful for copying and moving files around, and useless for
//! locating corruption inside one.
//!
//! The generator's name surfaces in this crate as
//! [`LiveFileStorageInfoEntry::file_checksum_func_name`](crate::metadata::LiveFileStorageInfoEntry::file_checksum_func_name).
//! The digest itself is deliberately not exposed: the only C accessor returns it
//! as a NUL-terminated string with no length, and a digest can contain a zero
//! byte, so the value would be silently truncated.

use std::ptr::NonNull;

use crate::ffi;

/// A factory RocksDB asks for a checksum generator each time it creates an SST
/// file.
///
/// Install one with
/// [`Options::set_file_checksum_gen_factory`](crate::Options::set_file_checksum_gen_factory).
/// The setter copies the underlying `shared_ptr`, so a factory can be installed
/// on any number of `Options` and dropped as soon as the last call returns.
pub struct FileChecksumGenFactory {
    inner: NonNull<ffi::rocksdb_file_checksum_gen_factory_t>,
}

impl FileChecksumGenFactory {
    /// The built in CRC32C generator, recorded in the manifest under the name
    /// `FileChecksumCrc32c`.
    ///
    /// Upstream notes that this digest is big endian and unmasked, unlike the
    /// other CRC32C values RocksDB computes, which makes it comparable with
    /// CRC32C implementations outside RocksDB.
    ///
    /// Wraps `GetFileChecksumGenCrc32cFactory`.
    #[must_use]
    pub fn crc32c() -> Self {
        let inner = unsafe { ffi::rocksdb_file_checksum_gen_crc32c_factory_create() };
        Self {
            inner: NonNull::new(inner)
                .expect("rocksdb_file_checksum_gen_crc32c_factory_create returned null"),
        }
    }

    pub(crate) fn as_ptr(&self) -> *mut ffi::rocksdb_file_checksum_gen_factory_t {
        self.inner.as_ptr()
    }
}

impl Drop for FileChecksumGenFactory {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_file_checksum_gen_factory_destroy(self.inner.as_ptr());
        }
    }
}

// `rocksdb_file_checksum_gen_factory_t` is a
// `std::shared_ptr<FileChecksumGenFactory>` and nothing else (c.cc:498).
// Destroying the handle only drops that one reference, and the only other
// operation this crate performs on it is the refcount bump in
// `rocksdb_options_set_file_checksum_gen_factory` (c.cc:6067), which is atomic.
// So the handle carries no thread affinity, and concurrent reads through
// `&FileChecksumGenFactory` cannot race.
unsafe impl Send for FileChecksumGenFactory {}
unsafe impl Sync for FileChecksumGenFactory {}

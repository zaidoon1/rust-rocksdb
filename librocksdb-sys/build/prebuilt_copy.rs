//! Materializes a validated bundle inside Cargo's `OUT_DIR`.

use std::fs;
use std::path::{Path, PathBuf};

/// Copy the validated bundle to a Cargo-owned path used for linking.
pub(super) fn materialize_bundle(source: &Path, destination: &Path) -> PathBuf {
    if destination.exists() {
        fs::remove_dir_all(destination).unwrap_or_else(|e| {
            panic!(
                "cannot clear materialized RocksDB bundle `{}`: {e}",
                destination.display()
            )
        });
    }
    copy_directory(source, destination);
    destination.to_owned()
}

fn copy_directory(source: &Path, destination: &Path) {
    let metadata = fs::symlink_metadata(source).unwrap_or_else(|e| {
        panic!(
            "cannot inspect RocksDB bundle source `{}` while copying: {e}",
            source.display()
        )
    });
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        panic!(
            "RocksDB bundle copy source `{}` is not a regular directory",
            source.display()
        );
    }
    fs::create_dir(destination).unwrap_or_else(|e| {
        panic!(
            "cannot create materialized RocksDB bundle directory `{}`: {e}",
            destination.display()
        )
    });
    fs::set_permissions(destination, metadata.permissions()).unwrap_or_else(|e| {
        panic!(
            "cannot set permissions on materialized directory `{}`: {e}",
            destination.display()
        )
    });
    for entry in fs::read_dir(source).unwrap_or_else(|e| {
        panic!(
            "cannot read RocksDB bundle source directory `{}`: {e}",
            source.display()
        )
    }) {
        let entry = entry.unwrap_or_else(|e| {
            panic!(
                "cannot read entry under RocksDB bundle source `{}`: {e}",
                source.display()
            )
        });
        copy_entry(&entry.path(), &destination.join(entry.file_name()));
    }
}

fn copy_entry(source: &Path, destination: &Path) {
    let metadata = fs::symlink_metadata(source).unwrap_or_else(|e| {
        panic!(
            "cannot inspect RocksDB bundle entry `{}` while copying: {e}",
            source.display()
        )
    });
    if metadata.file_type().is_symlink() {
        panic!(
            "RocksDB bundle entry `{}` became a symbolic link while copying",
            source.display()
        );
    }
    if metadata.is_dir() {
        copy_directory(source, destination);
    } else if metadata.is_file() {
        copy_file(source, destination, &metadata);
    } else {
        panic!(
            "RocksDB bundle entry `{}` is not a regular file or directory",
            source.display()
        );
    }
}

fn copy_file(source: &Path, destination: &Path, metadata: &fs::Metadata) {
    fs::copy(source, destination).unwrap_or_else(|e| {
        panic!(
            "cannot copy RocksDB bundle file `{}` to `{}`: {e}",
            source.display(),
            destination.display()
        )
    });
    fs::set_permissions(destination, metadata.permissions()).unwrap_or_else(|e| {
        panic!(
            "cannot set permissions on materialized file `{}`: {e}",
            destination.display()
        )
    });
}

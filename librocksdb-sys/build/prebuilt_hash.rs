//! Deterministic streaming hashes for bundle artifacts and header trees.

use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// Reject bundle paths that another local account could replace or modify.
pub(super) fn validate_bundle_tree(root: &Path, checkout: &Path) {
    #[cfg(unix)]
    validate_bundle_tree_unix(root, checkout);

    #[cfg(not(unix))]
    {
        let _ = (root, checkout);
        panic!("prebuilt RocksDB bundle trust validation requires a Unix build host");
    }
}

#[cfg(unix)]
fn validate_bundle_tree_unix(root: &Path, checkout: &Path) {
    let owner = checkout_owner(checkout);
    validate_bundle_ancestors(root, owner);
    let mut pending = validate_bundle_root(root, owner);
    while let Some(path) = pending.pop() {
        let metadata = bundle_metadata(&path);
        validate_bundle_entry(&path, &metadata, owner);
        if metadata.is_dir() {
            pending.extend(bundle_children(&path));
        }
    }
}

#[cfg(unix)]
fn validate_bundle_ancestors(root: &Path, owner: u32) {
    let mut current = root.parent();
    while let Some(path) = current {
        let metadata = bundle_metadata(path);
        validate_ancestor(path, &metadata, owner);
        current = path.parent();
    }
}

#[cfg(unix)]
fn validate_ancestor(path: &Path, metadata: &fs::Metadata, owner: u32) {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        panic!(
            "prebuilt RocksDB bundle ancestor `{}` is not a trusted directory",
            path.display()
        );
    }
    if metadata.uid() != owner && metadata.uid() != 0 {
        panic!(
            "prebuilt RocksDB bundle ancestor `{}` is owned by uid {}, expected uid {owner} or root",
            path.display(),
            metadata.uid()
        );
    }
    if metadata.mode() & 0o022 != 0 && !is_root_owned_sticky_directory(metadata) {
        panic!(
            "prebuilt RocksDB bundle ancestor `{}` is group or world writable with mode {:04o}",
            path.display(),
            metadata.mode() & 0o7777
        );
    }
}

#[cfg(unix)]
fn is_root_owned_sticky_directory(metadata: &fs::Metadata) -> bool {
    metadata.uid() == 0 && metadata.mode() & 0o1000 != 0
}

#[cfg(unix)]
fn validate_bundle_root(root: &Path, owner: u32) -> Vec<PathBuf> {
    let metadata = bundle_metadata(root);
    validate_bundle_entry(root, &metadata, owner);
    if !metadata.is_dir() {
        panic!(
            "prebuilt RocksDB bundle root `{}` is not a directory",
            root.display()
        );
    }
    bundle_children(root)
}

#[cfg(unix)]
fn checkout_owner(checkout: &Path) -> u32 {
    let metadata = fs::metadata(checkout).unwrap_or_else(|e| {
        panic!(
            "cannot inspect crate checkout `{}` for prebuilt bundle ownership: {e}",
            checkout.display()
        )
    });
    if !metadata.is_dir() {
        panic!("crate checkout `{}` is not a directory", checkout.display());
    }
    metadata.uid()
}

#[cfg(unix)]
fn bundle_metadata(path: &Path) -> fs::Metadata {
    fs::symlink_metadata(path).unwrap_or_else(|e| {
        panic!(
            "cannot inspect prebuilt RocksDB bundle path `{}`: {e}",
            path.display()
        )
    })
}

#[cfg(unix)]
fn bundle_children(directory: &Path) -> Vec<PathBuf> {
    fs::read_dir(directory)
        .unwrap_or_else(|e| {
            panic!(
                "cannot read prebuilt RocksDB bundle directory `{}`: {e}",
                directory.display()
            )
        })
        .map(|entry| bundle_child_path(directory, entry))
        .collect()
}

#[cfg(unix)]
fn bundle_child_path(directory: &Path, entry: io::Result<fs::DirEntry>) -> PathBuf {
    entry
        .unwrap_or_else(|e| {
            panic!(
                "cannot read entry under prebuilt RocksDB bundle directory `{}`: {e}",
                directory.display()
            )
        })
        .path()
}

#[cfg(unix)]
fn validate_bundle_entry(path: &Path, metadata: &fs::Metadata, owner: u32) {
    validate_bundle_file_type(path, metadata);
    if metadata.uid() != owner {
        panic!(
            "prebuilt RocksDB bundle path `{}` is owned by uid {}, expected checkout owner uid {owner}",
            path.display(),
            metadata.uid()
        );
    }
    if metadata.mode() & 0o022 != 0 {
        panic!(
            "prebuilt RocksDB bundle path `{}` is group or world writable with mode {:04o}",
            path.display(),
            metadata.mode() & 0o7777
        );
    }
}

#[cfg(unix)]
fn validate_bundle_file_type(path: &Path, metadata: &fs::Metadata) {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        panic!(
            "prebuilt RocksDB bundle path `{}` is a symbolic link",
            path.display()
        );
    }
    if metadata.is_file() {
        validate_bundle_link_count(path, metadata);
    } else if !metadata.is_dir() {
        panic!(
            "prebuilt RocksDB bundle path `{}` is not a regular file or directory",
            path.display()
        );
    }
}

#[cfg(unix)]
fn validate_bundle_link_count(path: &Path, metadata: &fs::Metadata) {
    if metadata.nlink() != 1 {
        panic!(
            "prebuilt RocksDB bundle file `{}` has {} hard links, expected 1",
            path.display(),
            metadata.nlink()
        );
    }
}

/// Hash one file without loading the full artifact into memory.
pub(super) fn sha256_file(path: &Path) -> String {
    let mut hasher = Sha256::new();
    update_hash_from_file(&mut hasher, path);
    digest_hex(hasher.finalize())
}

/// Hash named files in order, separating each name from its contents.
pub(super) fn sha256_files(paths: &[PathBuf]) -> String {
    let mut hasher = Sha256::new();
    for path in paths {
        hasher.update(
            path.file_name()
                .expect("hash input has a file name")
                .as_encoded_bytes(),
        );
        hasher.update([0]);
        update_hash_from_file(&mut hasher, path);
    }
    digest_hex(hasher.finalize())
}

/// Hash a directory tree by sorted relative path and file contents.
pub(super) fn sha256_tree(root: &Path) -> String {
    let mut files = Vec::new();
    collect_tree_files(root, root, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));

    let mut hasher = Sha256::new();
    for (relative, path) in files {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        update_hash_from_file(&mut hasher, &path);
    }
    digest_hex(hasher.finalize())
}

fn collect_tree_files(root: &Path, directory: &Path, files: &mut Vec<(String, PathBuf)>) {
    let entries = fs::read_dir(directory).unwrap_or_else(|e| {
        panic!(
            "cannot read header directory `{}`: {e}",
            directory.display()
        )
    });
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("cannot read entry under `{}`: {e}", directory.display()))
            .path();
        collect_tree_path(root, path, files);
    }
}

fn collect_tree_path(root: &Path, path: PathBuf, files: &mut Vec<(String, PathBuf)>) {
    if path.is_dir() {
        collect_tree_files(root, &path, files);
    } else if path.is_file() {
        let relative = path
            .strip_prefix(root)
            .expect("tree entry is below its root")
            .to_string_lossy()
            .replace('\\', "/");
        files.push((relative, path));
    }
}

fn update_hash_from_file(hasher: &mut Sha256, path: &Path) {
    let file = File::open(path)
        .unwrap_or_else(|e| panic!("cannot read hash input `{}`: {e}", path.display()));
    let mut reader = BufReader::new(file);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .unwrap_or_else(|e| panic!("cannot hash `{}`: {e}", path.display()));
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
}

fn digest_hex(bytes: impl AsRef<[u8]>) -> String {
    let mut output = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

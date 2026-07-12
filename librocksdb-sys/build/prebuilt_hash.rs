//! Deterministic streaming hashes for bundle artifacts and header trees.

use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

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

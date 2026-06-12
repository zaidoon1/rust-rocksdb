// Copyright 2026 Tyler Neely
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

//! Integration test for the prebuilt `ldb` tool.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn test_ldb_help() {
    // 1. Locate the ldb binary.
    let mut ldb_path = None;

    // Only test ldb if we are explicitly using a prebuilt ROCKSDB_LIB_DIR
    if let Ok(lib_dir_str) = env::var("ROCKSDB_LIB_DIR") {
        let lib_dir = Path::new(&lib_dir_str);
        if let Some(prefix) = lib_dir.parent() {
            let candidate = prefix.join("bin").join("ldb");
            if candidate.exists() {
                ldb_path = Some(candidate);
            }
        }
    }

    let Some(ldb) = ldb_path else {
        println!("Skipping ldb test: ROCKSDB_LIB_DIR not set or ldb binary not found in prefix.");
        return;
    };

    println!("Found ldb binary at: {:?}", ldb);

    // 2. Set library search path (LD_LIBRARY_PATH / DYLD_LIBRARY_PATH) so ldb can load librocksdb.so/dylib
    let mut cmd = Command::new(ldb);
    cmd.arg("--help");

    // Add search paths
    let mut lib_dirs = Vec::new();
    if let Ok(lib_dir_str) = env::var("ROCKSDB_LIB_DIR") {
        lib_dirs.push(PathBuf::from(lib_dir_str));
    }
    lib_dirs.push(PathBuf::from("librocksdb-sys/rocksdb"));

    // LD_LIBRARY_PATH (Linux)
    let ld_library_path = env::var_os("LD_LIBRARY_PATH");
    let mut paths = env::split_paths(&ld_library_path.unwrap_or_default()).collect::<Vec<_>>();
    paths.extend(lib_dirs.clone());
    let new_ld_path = env::join_paths(paths).unwrap();
    cmd.env("LD_LIBRARY_PATH", new_ld_path);

    // DYLD_LIBRARY_PATH (macOS)
    let dyld_library_path = env::var_os("DYLD_LIBRARY_PATH");
    let mut mac_paths =
        env::split_paths(&dyld_library_path.unwrap_or_default()).collect::<Vec<_>>();
    mac_paths.extend(lib_dirs);
    let new_dyld_path = env::join_paths(mac_paths).unwrap();
    cmd.env("DYLD_LIBRARY_PATH", new_dyld_path);

    // 3. Execute ldb and assert it prints the expected tool header
    let output = cmd.output().expect("Failed to execute ldb process");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    println!("ldb stdout: {}", stdout);
    println!("ldb stderr: {}", stderr);

    assert!(
        stdout.contains("ldb - RocksDB Tool") || stderr.contains("ldb - RocksDB Tool"),
        "ldb output did not contain expected tool header"
    );
}

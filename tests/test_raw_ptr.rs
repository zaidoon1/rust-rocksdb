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

//! Tests for the `raw-ptr` feature which provides access to underlying RocksDB C API pointers.

#![cfg(feature = "raw-ptr")]

mod util;

use rust_rocksdb::{AsRawPtr, DB, Env, Options};
use util::DBPath;

#[test]
fn test_db_as_raw_ptr() {
    let path = DBPath::new("_rust_rocksdb_raw_ptr_db_test");

    let db = DB::open_default(&path).unwrap();

    // Get the raw pointer to the underlying rocksdb_t
    let raw_ptr = unsafe { db.as_raw_ptr() };

    // The pointer should not be null for a successfully opened database
    assert!(!raw_ptr.is_null());
}

#[test]
fn test_options_as_raw_ptr() {
    let opts = Options::default();

    // Get the raw pointer to the underlying rocksdb_options_t
    let raw_ptr = unsafe { opts.as_raw_ptr() };

    // The pointer should not be null for valid options
    assert!(!raw_ptr.is_null());
}

#[test]
fn test_env_as_raw_ptr() {
    let env = Env::new().unwrap();

    // Get the raw pointer to the underlying rocksdb_env_t
    let raw_ptr = unsafe { env.as_raw_ptr() };

    // The pointer should not be null for a successfully created environment
    assert!(!raw_ptr.is_null());
}

#[test]
fn test_raw_ptr_stability() {
    // Test that the raw pointer remains stable while the object is alive
    let path = DBPath::new("_rust_rocksdb_raw_ptr_stability_test");

    let db = DB::open_default(&path).unwrap();

    let ptr1 = unsafe { db.as_raw_ptr() };
    let ptr2 = unsafe { db.as_raw_ptr() };

    // Multiple calls should return the same pointer
    assert_eq!(ptr1, ptr2);
}

#[test]
fn test_options_raw_ptr_stability() {
    let opts = Options::default();

    let ptr1 = unsafe { opts.as_raw_ptr() };
    let ptr2 = unsafe { opts.as_raw_ptr() };

    // Multiple calls should return the same pointer
    assert_eq!(ptr1, ptr2);
}

#[test]
fn test_env_raw_ptr_stability() {
    let env = Env::new().unwrap();

    let ptr1 = unsafe { env.as_raw_ptr() };
    let ptr2 = unsafe { env.as_raw_ptr() };

    // Multiple calls should return the same pointer
    assert_eq!(ptr1, ptr2);
}

#[test]
fn test_raw_ptr_with_db_operations() {
    // Test that the raw pointer remains valid after performing database operations
    let path = DBPath::new("_rust_rocksdb_raw_ptr_operations_test");

    let db = DB::open_default(&path).unwrap();
    let initial_ptr = unsafe { db.as_raw_ptr() };

    // Perform some operations
    db.put(b"key1", b"value1").unwrap();
    db.put(b"key2", b"value2").unwrap();
    db.get(b"key1").unwrap();
    db.delete(b"key1").unwrap();

    // The pointer should still be the same
    let after_ops_ptr = unsafe { db.as_raw_ptr() };
    assert_eq!(initial_ptr, after_ops_ptr);
    assert!(!after_ops_ptr.is_null());
}

#[test]
fn test_raw_ptr_with_configured_options() {
    // Test raw pointer access on options with various configurations
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.set_max_open_files(100);
    opts.set_use_fsync(false);
    opts.set_bytes_per_sync(8 * 1024 * 1024);

    let raw_ptr = unsafe { opts.as_raw_ptr() };
    assert!(!raw_ptr.is_null());

    // The pointer should remain stable after more configurations
    opts.set_max_background_jobs(4);
    let raw_ptr_after = unsafe { opts.as_raw_ptr() };
    assert_eq!(raw_ptr, raw_ptr_after);
}

#[test]
fn test_raw_ptr_with_configured_env() {
    // Test raw pointer access on env with various configurations
    let mut env = Env::new().unwrap();
    env.set_bottom_priority_background_threads(2);
    env.set_low_priority_background_threads(4);
    env.set_high_priority_background_threads(2);

    let raw_ptr = unsafe { env.as_raw_ptr() };
    assert!(!raw_ptr.is_null());
}

#[test]
fn test_multiple_dbs_have_different_raw_ptrs() {
    // Test that different DB instances have different raw pointers
    let path1 = DBPath::new("_rust_rocksdb_raw_ptr_multi_db_1");
    let path2 = DBPath::new("_rust_rocksdb_raw_ptr_multi_db_2");

    let db1 = DB::open_default(&path1).unwrap();
    let db2 = DB::open_default(&path2).unwrap();

    let ptr1 = unsafe { db1.as_raw_ptr() };
    let ptr2 = unsafe { db2.as_raw_ptr() };

    // Different databases should have different pointers
    assert_ne!(ptr1, ptr2);
    assert!(!ptr1.is_null());
    assert!(!ptr2.is_null());
}

#[test]
fn test_multiple_options_have_different_raw_ptrs() {
    // Test that different Options instances have different raw pointers
    let opts1 = Options::default();
    let opts2 = Options::default();

    let ptr1 = unsafe { opts1.as_raw_ptr() };
    let ptr2 = unsafe { opts2.as_raw_ptr() };

    // Different options should have different pointers
    assert_ne!(ptr1, ptr2);
    assert!(!ptr1.is_null());
    assert!(!ptr2.is_null());
}

#[test]
fn test_multiple_envs_have_different_raw_ptrs() {
    // Test that different Env instances have different raw pointers
    let env1 = Env::new().unwrap();
    let env2 = Env::new().unwrap();

    let ptr1 = unsafe { env1.as_raw_ptr() };
    let ptr2 = unsafe { env2.as_raw_ptr() };

    // Different environments should have different pointers
    assert_ne!(ptr1, ptr2);
    assert!(!ptr1.is_null());
    assert!(!ptr2.is_null());
}

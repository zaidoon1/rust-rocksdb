// Copyright 2024
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

//! Tests for zero-copy APIs introduced in RocksDB C API.
//! These tests verify the new optimized functions:
//! - get_into_buffer / get_into_buffer_cf
//! - batched_multi_get_cf_slice / batched_multi_get_cf_slice_opt
//! - Iterator slice functions (rocksdb_iter_key_slice, etc.)

mod util;

use rust_rocksdb::{ColumnFamilyDescriptor, DB, GetIntoBufferResult, Options, ReadOptions};
use util::DBPath;

#[test]
fn test_get_into_buffer_result_is_found() {
    assert!(GetIntoBufferResult::Found(10).is_found());
    assert!(GetIntoBufferResult::BufferTooSmall(10).is_found());
    assert!(!GetIntoBufferResult::NotFound.is_found());
}

#[test]
fn test_get_into_buffer_result_is_not_found() {
    assert!(GetIntoBufferResult::NotFound.is_not_found());
    assert!(!GetIntoBufferResult::Found(10).is_not_found());
    assert!(!GetIntoBufferResult::BufferTooSmall(10).is_not_found());
}

#[test]
fn test_get_into_buffer_result_value_size() {
    assert_eq!(GetIntoBufferResult::Found(42).value_size(), Some(42));
    assert_eq!(
        GetIntoBufferResult::BufferTooSmall(100).value_size(),
        Some(100)
    );
    assert_eq!(GetIntoBufferResult::NotFound.value_size(), None);
}

#[test]
fn test_get_into_buffer_found() {
    let path = DBPath::new("_rust_rocksdb_get_into_buffer_found");
    let db = DB::open_default(&path).unwrap();

    // Put a value
    db.put(b"test_key", b"test_value").unwrap();

    // Get into buffer
    let mut buffer = [0u8; 100];
    let result = db.get_into_buffer(b"test_key", &mut buffer).unwrap();

    match result {
        GetIntoBufferResult::Found(size) => {
            assert_eq!(size, 10);
            assert_eq!(&buffer[..size], b"test_value");
        }
        _ => panic!("Expected Found result"),
    }
}

#[test]
fn test_get_into_buffer_not_found() {
    let path = DBPath::new("_rust_rocksdb_get_into_buffer_not_found");
    let db = DB::open_default(&path).unwrap();

    // Get a key that doesn't exist
    let mut buffer = [0u8; 100];
    let result = db.get_into_buffer(b"nonexistent_key", &mut buffer).unwrap();

    assert_eq!(result, GetIntoBufferResult::NotFound);
}

#[test]
fn test_get_into_buffer_too_small() {
    let path = DBPath::new("_rust_rocksdb_get_into_buffer_too_small");
    let db = DB::open_default(&path).unwrap();

    // Put a value
    let large_value = b"this_is_a_larger_value_that_wont_fit";
    db.put(b"large_key", large_value).unwrap();

    // Try to get into a buffer that's too small
    let mut buffer = [0u8; 5];
    let result = db.get_into_buffer(b"large_key", &mut buffer).unwrap();

    match result {
        GetIntoBufferResult::BufferTooSmall(actual_size) => {
            assert_eq!(actual_size, large_value.len());
        }
        _ => panic!("Expected BufferTooSmall result"),
    }
}

#[test]
fn test_get_into_buffer_exact_fit() {
    let path = DBPath::new("_rust_rocksdb_get_into_buffer_exact_fit");
    let db = DB::open_default(&path).unwrap();

    // Put a value
    let value = b"exact";
    db.put(b"exact_key", value).unwrap();

    // Get into a buffer of exact size
    let mut buffer = [0u8; 5];
    let result = db.get_into_buffer(b"exact_key", &mut buffer).unwrap();

    match result {
        GetIntoBufferResult::Found(size) => {
            assert_eq!(size, 5);
            assert_eq!(&buffer[..], value);
        }
        _ => panic!("Expected Found result"),
    }
}

#[test]
fn test_get_into_buffer_cf() {
    let path = DBPath::new("_rust_rocksdb_get_into_buffer_cf");

    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);

    let cf_desc = ColumnFamilyDescriptor::new("cf1", Options::default());
    let db = DB::open_cf_descriptors(&opts, &path, vec![cf_desc]).unwrap();

    let cf = db.cf_handle("cf1").unwrap();

    // Put a value in the column family
    db.put_cf(&cf, b"cf_key", b"cf_value").unwrap();

    // Get into buffer from column family
    let mut buffer = [0u8; 100];
    let result = db.get_into_buffer_cf(&cf, b"cf_key", &mut buffer).unwrap();

    match result {
        GetIntoBufferResult::Found(size) => {
            assert_eq!(size, 8);
            assert_eq!(&buffer[..size], b"cf_value");
        }
        _ => panic!("Expected Found result"),
    }
}

#[test]
fn test_get_into_buffer_cf_not_found() {
    let path = DBPath::new("_rust_rocksdb_get_into_buffer_cf_not_found");

    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);

    let cf_desc = ColumnFamilyDescriptor::new("cf1", Options::default());
    let db = DB::open_cf_descriptors(&opts, &path, vec![cf_desc]).unwrap();

    let cf = db.cf_handle("cf1").unwrap();

    // Get a key that doesn't exist in the column family
    let mut buffer = [0u8; 100];
    let result = db
        .get_into_buffer_cf(&cf, b"nonexistent", &mut buffer)
        .unwrap();

    assert_eq!(result, GetIntoBufferResult::NotFound);
}

#[test]
fn test_batched_multi_get_cf_slice() {
    let path = DBPath::new("_rust_rocksdb_batched_multi_get_cf_slice");

    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);

    let cf_desc = ColumnFamilyDescriptor::new("cf1", Options::default());
    let db = DB::open_cf_descriptors(&opts, &path, vec![cf_desc]).unwrap();

    let cf = db.cf_handle("cf1").unwrap();

    // Put multiple values
    db.put_cf(&cf, b"key1", b"value1").unwrap();
    db.put_cf(&cf, b"key2", b"value2").unwrap();
    db.put_cf(&cf, b"key3", b"value3").unwrap();

    // Batch get using slice API
    let keys: Vec<&[u8]> = vec![b"key1", b"key2", b"key3", b"nonexistent"];
    let results = db.batched_multi_get_cf_slice(&cf, keys, false);

    assert_eq!(results.len(), 4);

    // Check found keys
    assert!(results[0].is_ok());
    assert_eq!(
        results[0].as_ref().unwrap().as_ref().unwrap().as_ref(),
        b"value1"
    );

    assert!(results[1].is_ok());
    assert_eq!(
        results[1].as_ref().unwrap().as_ref().unwrap().as_ref(),
        b"value2"
    );

    assert!(results[2].is_ok());
    assert_eq!(
        results[2].as_ref().unwrap().as_ref().unwrap().as_ref(),
        b"value3"
    );

    // Check not found key
    assert!(results[3].is_ok());
    assert!(results[3].as_ref().unwrap().is_none());
}

#[test]
fn test_batched_multi_get_cf_slice_sorted() {
    let path = DBPath::new("_rust_rocksdb_batched_multi_get_cf_slice_sorted");

    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);

    let cf_desc = ColumnFamilyDescriptor::new("cf1", Options::default());
    let db = DB::open_cf_descriptors(&opts, &path, vec![cf_desc]).unwrap();

    let cf = db.cf_handle("cf1").unwrap();

    // Put multiple values
    db.put_cf(&cf, b"aaa", b"value_aaa").unwrap();
    db.put_cf(&cf, b"bbb", b"value_bbb").unwrap();
    db.put_cf(&cf, b"ccc", b"value_ccc").unwrap();

    // Batch get using slice API with sorted input
    let keys: Vec<&[u8]> = vec![b"aaa", b"bbb", b"ccc"];
    let results = db.batched_multi_get_cf_slice(&cf, keys, true);

    assert_eq!(results.len(), 3);

    assert!(results[0].is_ok());
    assert_eq!(
        results[0].as_ref().unwrap().as_ref().unwrap().as_ref(),
        b"value_aaa"
    );

    assert!(results[1].is_ok());
    assert_eq!(
        results[1].as_ref().unwrap().as_ref().unwrap().as_ref(),
        b"value_bbb"
    );

    assert!(results[2].is_ok());
    assert_eq!(
        results[2].as_ref().unwrap().as_ref().unwrap().as_ref(),
        b"value_ccc"
    );
}

#[test]
fn test_batched_multi_get_cf_slice_empty() {
    let path = DBPath::new("_rust_rocksdb_batched_multi_get_cf_slice_empty");

    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);

    let cf_desc = ColumnFamilyDescriptor::new("cf1", Options::default());
    let db = DB::open_cf_descriptors(&opts, &path, vec![cf_desc]).unwrap();

    let cf = db.cf_handle("cf1").unwrap();

    // Batch get with empty keys
    let keys: Vec<&[u8]> = vec![];
    let results = db.batched_multi_get_cf_slice(&cf, keys, false);

    assert!(results.is_empty());
}

#[test]
fn test_iterator_key_value_slice() {
    let path = DBPath::new("_rust_rocksdb_iterator_key_value_slice");
    let db = DB::open_default(&path).unwrap();

    // Put multiple values
    db.put(b"iter_key1", b"iter_value1").unwrap();
    db.put(b"iter_key2", b"iter_value2").unwrap();
    db.put(b"iter_key3", b"iter_value3").unwrap();

    // Create iterator and verify key/value access
    let mut iter = db.raw_iterator();
    iter.seek_to_first();

    let mut count = 0;
    while iter.valid() {
        let key = iter.key().unwrap();
        let value = iter.value().unwrap();

        // Verify the key/value pairs
        assert!(key.starts_with(b"iter_key"));
        assert!(value.starts_with(b"iter_value"));

        count += 1;
        iter.next();
    }

    assert_eq!(count, 3);
}

#[test]
fn test_get_into_buffer_empty_value() {
    let path = DBPath::new("_rust_rocksdb_get_into_buffer_empty_value");
    let db = DB::open_default(&path).unwrap();

    // Put an empty value
    db.put(b"empty_key", b"").unwrap();

    // Get into buffer
    let mut buffer = [0u8; 100];
    let result = db.get_into_buffer(b"empty_key", &mut buffer).unwrap();

    match result {
        GetIntoBufferResult::Found(size) => {
            assert_eq!(size, 0);
        }
        _ => panic!("Expected Found result with size 0"),
    }
}

#[test]
fn test_get_into_buffer_large_value() {
    let path = DBPath::new("_rust_rocksdb_get_into_buffer_large_value");
    let db = DB::open_default(&path).unwrap();

    // Put a large value
    let large_value: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
    db.put(b"large_key", &large_value).unwrap();

    // Get into a buffer large enough
    let mut buffer = vec![0u8; 20000];
    let result = db.get_into_buffer(b"large_key", &mut buffer).unwrap();

    match result {
        GetIntoBufferResult::Found(size) => {
            assert_eq!(size, 10000);
            assert_eq!(&buffer[..size], &large_value[..]);
        }
        _ => panic!("Expected Found result"),
    }
}

#[test]
fn test_get_into_buffer_zero_length_buffer() {
    let path = DBPath::new("_rust_rocksdb_get_into_buffer_zero_len");
    let db = DB::open_default(&path).unwrap();

    // Put a value
    db.put(b"key", b"value").unwrap();

    // Try to get with a zero-length buffer - should return BufferTooSmall
    let mut buffer: [u8; 0] = [];
    let result = db.get_into_buffer(b"key", &mut buffer).unwrap();

    match result {
        GetIntoBufferResult::BufferTooSmall(size) => {
            assert_eq!(size, 5); // "value" is 5 bytes
        }
        _ => panic!("Expected BufferTooSmall result for zero-length buffer"),
    }

    // Zero-length buffer for non-existent key should return NotFound
    let result = db.get_into_buffer(b"nonexistent", &mut buffer).unwrap();
    assert_eq!(result, GetIntoBufferResult::NotFound);
}

#[test]
fn test_get_into_buffer_zero_length_buffer_empty_value() {
    let path = DBPath::new("_rust_rocksdb_get_into_buffer_zero_len_empty");
    let db = DB::open_default(&path).unwrap();

    // Put an empty value
    db.put(b"empty", b"").unwrap();

    // Zero-length buffer should work for empty value
    let mut buffer: [u8; 0] = [];
    let result = db.get_into_buffer(b"empty", &mut buffer).unwrap();

    match result {
        GetIntoBufferResult::Found(size) => {
            assert_eq!(size, 0);
        }
        _ => panic!("Expected Found(0) for empty value with zero-length buffer"),
    }
}

#[test]
fn test_get_into_buffer_with_read_options() {
    let path = DBPath::new("_rust_rocksdb_get_into_buffer_opts");
    let db = DB::open_default(&path).unwrap();

    db.put(b"key", b"value").unwrap();

    let mut buffer = [0u8; 100];
    let read_opts = ReadOptions::default();
    let result = db
        .get_into_buffer_opt(b"key", &mut buffer, &read_opts)
        .unwrap();

    match result {
        GetIntoBufferResult::Found(size) => {
            assert_eq!(size, 5);
            assert_eq!(&buffer[..size], b"value");
        }
        _ => panic!("Expected Found result"),
    }
}

#[test]
fn test_get_into_buffer_binary_data() {
    let path = DBPath::new("_rust_rocksdb_get_into_buffer_binary");
    let db = DB::open_default(&path).unwrap();

    // Test with binary data including null bytes
    let binary_key = b"\x00\x01\x02\xff\xfe";
    let binary_value = b"\xff\x00\xab\xcd\x00\x00\xef";
    db.put(binary_key, binary_value).unwrap();

    let mut buffer = [0u8; 100];
    let result = db.get_into_buffer(binary_key, &mut buffer).unwrap();

    match result {
        GetIntoBufferResult::Found(size) => {
            assert_eq!(size, binary_value.len());
            assert_eq!(&buffer[..size], binary_value);
        }
        _ => panic!("Expected Found result"),
    }
}

#[test]
fn test_iterator_empty_database() {
    let path = DBPath::new("_rust_rocksdb_iterator_empty");
    let db = DB::open_default(&path).unwrap();

    let mut iter = db.raw_iterator();
    iter.seek_to_first();

    // Iterator should be invalid on empty database
    assert!(!iter.valid());
    assert!(iter.key().is_none());
    assert!(iter.value().is_none());
}

#[test]
fn test_iterator_empty_key_value() {
    let path = DBPath::new("_rust_rocksdb_iterator_empty_kv");
    let db = DB::open_default(&path).unwrap();

    // Put empty key with empty value (RocksDB allows this)
    db.put(b"", b"").unwrap();
    // Also put a regular key for comparison
    db.put(b"regular", b"value").unwrap();

    let mut iter = db.raw_iterator();
    iter.seek_to_first();

    assert!(iter.valid());
    let key = iter.key().unwrap();
    let value = iter.value().unwrap();

    // Empty key should come first lexicographically
    assert_eq!(key, b"");
    assert_eq!(value, b"");

    iter.next();
    assert!(iter.valid());
    assert_eq!(iter.key().unwrap(), b"regular");
    assert_eq!(iter.value().unwrap(), b"value");
}

#[test]
fn test_batched_multi_get_cf_slice_single_key() {
    let path = DBPath::new("_rust_rocksdb_batched_single");

    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);

    let cf_desc = ColumnFamilyDescriptor::new("cf1", Options::default());
    let db = DB::open_cf_descriptors(&opts, &path, vec![cf_desc]).unwrap();

    let cf = db.cf_handle("cf1").unwrap();
    db.put_cf(&cf, b"single", b"value").unwrap();

    // Test with single key
    let keys: Vec<&[u8]> = vec![b"single"];
    let results = db.batched_multi_get_cf_slice(&cf, keys, false);

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].as_ref().unwrap().as_ref().unwrap().as_ref(),
        b"value"
    );
}

#[test]
fn test_batched_multi_get_cf_slice_all_missing() {
    let path = DBPath::new("_rust_rocksdb_batched_all_missing");

    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);

    let cf_desc = ColumnFamilyDescriptor::new("cf1", Options::default());
    let db = DB::open_cf_descriptors(&opts, &path, vec![cf_desc]).unwrap();

    let cf = db.cf_handle("cf1").unwrap();

    // Look up keys that don't exist
    let keys: Vec<&[u8]> = vec![b"missing1", b"missing2", b"missing3"];
    let results = db.batched_multi_get_cf_slice(&cf, keys, false);

    assert_eq!(results.len(), 3);
    for result in results {
        assert!(result.unwrap().is_none());
    }
}

#[test]
fn test_batched_multi_get_cf_slice_with_read_options() {
    let path = DBPath::new("_rust_rocksdb_batched_opts");

    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);

    let cf_desc = ColumnFamilyDescriptor::new("cf1", Options::default());
    let db = DB::open_cf_descriptors(&opts, &path, vec![cf_desc]).unwrap();

    let cf = db.cf_handle("cf1").unwrap();
    db.put_cf(&cf, b"key", b"value").unwrap();

    let keys: Vec<&[u8]> = vec![b"key"];
    let read_opts = ReadOptions::default();
    let results = db.batched_multi_get_cf_slice_opt(&cf, keys, false, &read_opts);

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].as_ref().unwrap().as_ref().unwrap().as_ref(),
        b"value"
    );
}

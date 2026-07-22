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

mod util;

use std::io::IoSlice;

use rust_rocksdb::{
    Cache, ColumnFamilyDescriptor, DB, MergeOperands, Options, WriteBatch, WriteBufferManager,
};
use util::{DBPath, U64Comparator, U64Timestamp};

fn concatenate_merge(
    _key: &[u8],
    existing_value: Option<&[u8]>,
    operands: &MergeOperands,
) -> Option<Vec<u8>> {
    let mut value = existing_value.unwrap_or_default().to_vec();
    for operand in operands {
        value.extend_from_slice(operand);
    }
    Some(value)
}

#[test]
fn write_batch_vectored_operations_copy_and_apply_parts() {
    let path = DBPath::new("write_batch_vectored_operations");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.set_merge_operator_associative("concatenate", concatenate_merge);
    let db = DB::open(&options, &path).unwrap();

    db.put(b"merge-key", b"base").unwrap();
    db.put(b"delete-key", b"value").unwrap();
    db.put(b"range-a", b"value").unwrap();
    db.put(b"range-b", b"value").unwrap();
    db.put(b"range-z", b"value").unwrap();

    let mut key_tail = b"key".to_vec();
    let mut value_tail = b"value".to_vec();
    let mut batch = WriteBatch::default();
    batch
        .put_vectored(
            &[IoSlice::new(b"put-"), IoSlice::new(&key_tail)],
            &[IoSlice::new(b"put-"), IoSlice::new(&value_tail)],
        )
        .unwrap();
    let many_key_parts = vec![IoSlice::new(b"h"); 17];
    batch
        .put_vectored(&many_key_parts, &[IoSlice::new(b"heap-value")])
        .unwrap();
    key_tail.fill(b'x');
    value_tail.fill(b'x');

    batch
        .merge_vectored(
            &[IoSlice::new(b"merge-"), IoSlice::new(b"key")],
            &[IoSlice::new(b"-"), IoSlice::new(b"merged")],
        )
        .unwrap();
    batch
        .delete_vectored(&[IoSlice::new(b"delete-"), IoSlice::new(b"key")])
        .unwrap();
    batch
        .delete_range_vectored(
            &[IoSlice::new(b"range-"), IoSlice::new(b"a")],
            &[IoSlice::new(b"range-"), IoSlice::new(b"z")],
        )
        .unwrap();

    db.write(&batch).unwrap();

    assert_eq!(db.get(b"put-key").unwrap().unwrap(), b"put-value");
    assert_eq!(db.get(vec![b'h'; 17]).unwrap().unwrap(), b"heap-value");
    assert_eq!(db.get(b"merge-key").unwrap().unwrap(), b"base-merged");
    assert_eq!(db.get(b"delete-key").unwrap(), None);
    assert_eq!(db.get(b"range-a").unwrap(), None);
    assert_eq!(db.get(b"range-b").unwrap(), None);
    assert_eq!(db.get(b"range-z").unwrap().unwrap(), b"value");
}

#[test]
fn write_batch_cf_vectored_operations_apply_parts() {
    let path = DBPath::new("write_batch_cf_vectored_operations");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);

    let mut cf_options = Options::default();
    cf_options.set_merge_operator_associative("concatenate", concatenate_merge);
    let descriptor = ColumnFamilyDescriptor::new("cf", cf_options);
    let db = DB::open_cf_descriptors(&options, &path, vec![descriptor]).unwrap();
    let cf = db.cf_handle("cf").unwrap();

    db.put_cf(&cf, b"merge-key", b"base").unwrap();
    db.put_cf(&cf, b"delete-key", b"value").unwrap();
    db.put_cf(&cf, b"range-a", b"value").unwrap();
    db.put_cf(&cf, b"range-b", b"value").unwrap();
    db.put_cf(&cf, b"range-z", b"value").unwrap();

    let mut batch = WriteBatch::default();
    batch
        .put_cf_vectored(
            &cf,
            &[IoSlice::new(b"put-"), IoSlice::new(b"key")],
            &[IoSlice::new(b"put-"), IoSlice::new(b"value")],
        )
        .unwrap();
    batch
        .merge_cf_vectored(
            &cf,
            &[IoSlice::new(b"merge-"), IoSlice::new(b"key")],
            &[IoSlice::new(b"-"), IoSlice::new(b"merged")],
        )
        .unwrap();
    batch
        .delete_cf_vectored(&cf, &[IoSlice::new(b"delete-"), IoSlice::new(b"key")])
        .unwrap();
    batch
        .delete_range_cf_vectored(
            &cf,
            &[IoSlice::new(b"range-"), IoSlice::new(b"a")],
            &[IoSlice::new(b"range-"), IoSlice::new(b"z")],
        )
        .unwrap();

    db.write(&batch).unwrap();

    assert_eq!(db.get_cf(&cf, b"put-key").unwrap().unwrap(), b"put-value");
    assert_eq!(
        db.get_cf(&cf, b"merge-key").unwrap().unwrap(),
        b"base-merged"
    );
    assert_eq!(db.get_cf(&cf, b"delete-key").unwrap(), None);
    assert_eq!(db.get_cf(&cf, b"range-a").unwrap(), None);
    assert_eq!(db.get_cf(&cf, b"range-b").unwrap(), None);
    assert_eq!(db.get_cf(&cf, b"range-z").unwrap().unwrap(), b"value");
}

#[test]
fn write_batch_cf_vectored_operations_report_timestamp_errors() {
    let path = DBPath::new("write_batch_cf_vectored_timestamp_errors");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);

    let mut cf_options = Options::default();
    cf_options.set_comparator_with_ts(
        U64Comparator::NAME,
        U64Timestamp::SIZE,
        Box::new(U64Comparator::compare),
        Box::new(U64Comparator::compare_ts),
        Box::new(U64Comparator::compare_without_ts),
    );
    let db = DB::open_cf_descriptors(
        &options,
        &path,
        [ColumnFamilyDescriptor::new("timestamped", cf_options)],
    )
    .unwrap();
    let cf = db.cf_handle("timestamped").unwrap();
    let key = [IoSlice::new(b"key")];
    let value = [IoSlice::new(b"value")];
    let mut batch = WriteBatch::default();

    assert!(batch.put_cf_vectored(&cf, &key, &value).is_err());
    assert!(batch.merge_cf_vectored(&cf, &key, &value).is_err());
    assert!(batch.delete_cf_vectored(&cf, &key).is_err());
    assert!(
        batch
            .delete_range_cf_vectored(&cf, &key, &[IoSlice::new(b"z")])
            .is_err()
    );
    assert!(batch.is_empty());
}

#[test]
fn write_batch_vectored_ranges_require_matching_part_counts() {
    let mut batch = WriteBatch::default();
    let error = batch
        .delete_range_vectored(
            &[IoSlice::new(b"a")],
            &[IoSlice::new(b"b"), IoSlice::new(b"c")],
        )
        .unwrap_err();
    assert!(error.to_string().contains("expected equal counts"));
    assert!(batch.is_empty());
}

#[test]
fn options_cache_and_write_buffer_accounting_apis() {
    let mut options = Options::default();
    assert!(!options.get_open_files_async());
    assert!(Options::supports_open_files_async());
    options.set_open_files_async(true).unwrap();
    assert!(options.get_open_files_async());

    let mut cache = Cache::new_lru_cache(8 * 1024 * 1024);
    assert_eq!(cache.get_capacity(), 8 * 1024 * 1024);
    assert_eq!(cache.get_occupancy_count(), 0);
    assert!(cache.get_table_address_count() >= cache.get_occupancy_count());
    cache.set_capacity(16 * 1024 * 1024);
    assert_eq!(cache.get_capacity(), 16 * 1024 * 1024);

    let manager = WriteBufferManager::new_write_buffer_manager(4 * 1024 * 1024, false);
    assert!(!manager.cost_to_cache());
    assert_eq!(manager.get_mutable_memtable_memory_usage(), 0);
    assert_eq!(manager.get_dummy_entries_in_cache_usage(), 0);

    let manager = WriteBufferManager::new_write_buffer_manager_with_cache(
        4 * 1024 * 1024,
        false,
        cache.clone(),
    );
    assert!(manager.cost_to_cache());
    assert_eq!(manager.get_mutable_memtable_memory_usage(), 0);
    assert_eq!(manager.get_dummy_entries_in_cache_usage(), 0);

    let path = DBPath::new("write_buffer_accounting_apis");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.set_write_buffer_manager(&manager);
    let db = DB::open(&options, &path).unwrap();
    db.put(b"key", vec![0; 512 * 1024]).unwrap();

    assert!(manager.get_mutable_memtable_memory_usage() > 0);
    assert!(manager.get_usage() >= manager.get_mutable_memtable_memory_usage());
    assert!(manager.get_dummy_entries_in_cache_usage() > 0);
    assert!(cache.get_occupancy_count() > 0);
    assert!(cache.get_table_address_count() >= cache.get_occupancy_count());
}

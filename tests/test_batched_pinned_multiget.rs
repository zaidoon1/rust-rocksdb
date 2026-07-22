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

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use rust_rocksdb::{
    ColumnFamilyDescriptor, DB, DBPinnableSlice, Error, MergeOperands, Options, ReadOptions,
    statistics::Ticker,
};
use util::DBPath;

struct CountedKey {
    bytes: &'static [u8],
    calls: Arc<AtomicUsize>,
}

impl AsRef<[u8]> for CountedKey {
    fn as_ref(&self) -> &[u8] {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.bytes
    }
}

fn copied_values(results: Vec<Result<Option<DBPinnableSlice<'_>>, Error>>) -> Vec<Option<Vec<u8>>> {
    results
        .into_iter()
        .map(|result| result.unwrap().map(|value| value.to_vec()))
        .collect()
}

#[test]
fn multi_get_pinned_uses_one_native_batch() {
    let path = DBPath::new("_rust_rocksdb_multi_get_pinned_native_batch");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.enable_statistics();
    let db = DB::open(&options, &path).unwrap();
    db.put(b"a", b"va").unwrap();
    db.put(b"c", b"vc").unwrap();

    let as_ref_calls = Arc::new(AtomicUsize::new(0));
    let keys = [b"c".as_slice(), b"missing", b"a", b"c"].map(|bytes| CountedKey {
        bytes,
        calls: Arc::clone(&as_ref_calls),
    });
    let calls_before = options.get_ticker_count(Ticker::NumberMultigetCalls);
    let keys_before = options.get_ticker_count(Ticker::NumberMultigetKeysRead);

    let values = copied_values(db.multi_get_pinned(keys));

    assert_eq!(
        values,
        vec![
            Some(b"vc".to_vec()),
            None,
            Some(b"va".to_vec()),
            Some(b"vc".to_vec()),
        ]
    );
    assert_eq!(as_ref_calls.load(Ordering::Relaxed), 4);
    assert_eq!(
        options.get_ticker_count(Ticker::NumberMultigetCalls) - calls_before,
        1
    );
    assert_eq!(
        options.get_ticker_count(Ticker::NumberMultigetKeysRead) - keys_before,
        4
    );
}

#[test]
fn multi_get_pinned_keeps_single_key_on_point_read_path() {
    let path = DBPath::new("_rust_rocksdb_multi_get_pinned_single_key");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.enable_statistics();
    let db = DB::open(&options, &path).unwrap();
    db.put(b"a", b"va").unwrap();
    let calls_before = options.get_ticker_count(Ticker::NumberMultigetCalls);

    let values = copied_values(db.multi_get_pinned([b"a".as_slice()]));

    assert_eq!(values, vec![Some(b"va".to_vec())]);
    assert_eq!(
        options.get_ticker_count(Ticker::NumberMultigetCalls),
        calls_before
    );
}

#[test]
fn batched_multi_get_pinned_handles_sorted_duplicates_and_empty_input() {
    let path = DBPath::new("_rust_rocksdb_batched_multi_get_pinned_sorted");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.enable_statistics();
    let db = DB::open(&options, &path).unwrap();
    db.put(b"a", b"va").unwrap();
    db.put(b"b", b"vb").unwrap();

    let mut results = db.batched_multi_get_pinned([b"a".as_slice(), b"a", b"b", b"z"], true);
    assert_eq!(results.len(), 4);
    assert!(results.pop().unwrap().unwrap().is_none());
    assert_eq!(results.pop().unwrap().unwrap().unwrap().as_ref(), b"vb");
    let second = results.pop().unwrap().unwrap().unwrap();
    let first = results.pop().unwrap().unwrap().unwrap();
    assert_eq!(first.as_ref(), b"va");
    drop(first);
    assert_eq!(second.as_ref(), b"va");

    let calls_before = options.get_ticker_count(Ticker::NumberMultigetCalls);
    let empty = db.batched_multi_get_pinned(Vec::<Vec<u8>>::new(), false);
    assert!(empty.is_empty());
    assert_eq!(
        options.get_ticker_count(Ticker::NumberMultigetCalls),
        calls_before
    );
}

#[test]
fn batched_multi_get_pinned_cf_honors_snapshot_and_column_family() {
    let path = DBPath::new("_rust_rocksdb_batched_multi_get_pinned_cf_snapshot");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    options.enable_statistics();
    let db = DB::open_cf_descriptors(
        &options,
        &path,
        [ColumnFamilyDescriptor::new("cf", Options::default())],
    )
    .unwrap();
    let cf = db.cf_handle("cf").unwrap();

    db.put(b"same", b"default").unwrap();
    db.put_cf(&cf, b"same", b"before").unwrap();
    let snapshot = db.snapshot();
    db.put_cf(&cf, b"same", b"after").unwrap();
    let mut readopts = ReadOptions::default();
    readopts.set_snapshot(&snapshot);
    let calls_before = options.get_ticker_count(Ticker::NumberMultigetCalls);

    let values = copied_values(db.batched_multi_get_pinned_cf_opt(
        &cf,
        [b"same".as_slice(), b"missing", b"same"],
        false,
        &readopts,
    ));

    assert_eq!(
        values,
        vec![Some(b"before".to_vec()), None, Some(b"before".to_vec()),]
    );
    assert_eq!(
        options.get_ticker_count(Ticker::NumberMultigetCalls) - calls_before,
        1
    );

    let calls_before = options.get_ticker_count(Ticker::NumberMultigetCalls);
    let empty = db.batched_multi_get_pinned_cf(&cf, Vec::<Vec<u8>>::new(), false);
    assert!(empty.is_empty());
    assert_eq!(
        options.get_ticker_count(Ticker::NumberMultigetCalls),
        calls_before
    );
}

fn failing_merge(_key: &[u8], _value: Option<&[u8]>, _operands: &MergeOperands) -> Option<Vec<u8>> {
    None
}

#[test]
fn batched_multi_get_pinned_preserves_mixed_results() {
    let path = DBPath::new("_rust_rocksdb_batched_multi_get_pinned_mixed");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.set_merge_operator_associative("failing merge", failing_merge);
    let db = DB::open(&options, &path).unwrap();

    db.put(b"ok", b"value").unwrap();
    db.put(b"bad", b"base").unwrap();
    db.merge(b"bad", b"operand").unwrap();

    let results = db.batched_multi_get_pinned([b"ok".as_slice(), b"bad", b"missing"], false);

    assert_eq!(
        results[0].as_ref().unwrap().as_ref().unwrap().as_ref(),
        b"value"
    );
    let error = match &results[1] {
        Err(error) => error,
        Ok(_) => panic!("expected the failing merge to return an error"),
    };
    assert!(error.to_string().contains("Merge operator failed"));
    assert!(results[2].as_ref().unwrap().is_none());
}

#[test]
fn batch_owned_multiget_borrows_values_without_per_key_handles() {
    let path = DBPath::new("_rust_rocksdb_batch_owned_multiget");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.set_merge_operator_associative("failing merge", failing_merge);
    let db = DB::open(&options, &path).unwrap();

    db.put(b"a", b"value-a").unwrap();
    db.put(b"empty", b"").unwrap();
    db.put(b"bad", b"base").unwrap();
    db.merge(b"bad", b"operand").unwrap();

    let batch = db
        .batched_multi_get_pinned_batch(
            [b"a".as_slice(), b"empty", b"missing", b"bad", b"a"],
            false,
        )
        .unwrap();

    assert_eq!(batch.len(), 5);
    assert!(!batch.is_empty());
    assert_eq!(batch.get(0).unwrap().unwrap().unwrap(), b"value-a");
    assert_eq!(batch.get(1).unwrap().unwrap().unwrap(), b"");
    assert!(batch.get(2).unwrap().unwrap().is_none());
    assert!(
        batch
            .get(3)
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("Merge operator failed")
    );
    assert_eq!(batch.get(4).unwrap().unwrap().unwrap(), b"value-a");
    assert!(batch.get(5).is_none());

    let values = batch
        .iter()
        .map(|result| result.map(|value| value.map(<[u8]>::to_vec)))
        .collect::<Vec<_>>();
    assert_eq!(
        values[0].as_ref().unwrap().as_deref(),
        Some(b"value-a".as_slice())
    );
    assert_eq!(values[1].as_ref().unwrap().as_deref(), Some(b"".as_slice()));
    assert!(values[2].as_ref().unwrap().is_none());
    assert!(values[3].is_err());
    assert_eq!(
        values[4].as_ref().unwrap().as_deref(),
        Some(b"value-a".as_slice())
    );

    let empty = db
        .batched_multi_get_pinned_batch(Vec::<Vec<u8>>::new(), false)
        .unwrap();
    assert!(empty.is_empty());
    assert_eq!(empty.iter().len(), 0);
}

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

use rust_rocksdb::{ColumnFamilyDescriptor, DB, DBAccess, DBRawIteratorWithThreadMode, Options};
use util::DBPath;

fn collect_entries<D: DBAccess>(
    iterator: &mut DBRawIteratorWithThreadMode<'_, D>,
) -> Vec<(Vec<u8>, Vec<u8>)> {
    iterator.seek_to_first();
    let mut entries = Vec::new();
    while iterator.valid() {
        entries.push((
            iterator.key().unwrap().to_vec(),
            iterator.value().unwrap().to_vec(),
        ));
        iterator.next();
    }
    iterator.status().unwrap();
    entries
}

#[test]
fn raw_iterators_cf_preserve_order_ownership_and_consistent_view() {
    let path = DBPath::new("_rust_rocksdb_multi_cf_iterators");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    let db = DB::open_cf_descriptors(
        &options,
        &path,
        [
            ColumnFamilyDescriptor::new("first", Options::default()),
            ColumnFamilyDescriptor::new("second", Options::default()),
        ],
    )
    .unwrap();
    let first = db.cf_handle("first").unwrap();
    let second = db.cf_handle("second").unwrap();
    db.put_cf(&first, b"shared", b"first-old").unwrap();
    db.put_cf(&second, b"shared", b"second-old").unwrap();

    let mut iterators = db.raw_iterators_cf([&second, &first]).unwrap();
    db.put_cf(&first, b"later", b"first-new").unwrap();
    db.put_cf(&second, b"later", b"second-new").unwrap();

    let mut first_requested = iterators.remove(0);
    let second_requested = &mut iterators[0];
    assert_eq!(
        collect_entries(&mut first_requested),
        vec![(b"shared".to_vec(), b"second-old".to_vec())]
    );
    drop(first_requested);
    assert_eq!(
        collect_entries(second_requested),
        vec![(b"shared".to_vec(), b"first-old".to_vec())]
    );
}

#[test]
fn raw_iterators_cf_handles_empty_input() {
    let path = DBPath::new("_rust_rocksdb_multi_cf_iterators_empty");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    let db = DB::open_cf(&options, &path, ["cf"]).unwrap();
    let cf = db.cf_handle("cf").unwrap();
    let empty = [&cf; 0];

    assert!(db.raw_iterators_cf(empty).unwrap().is_empty());
}

#[cfg(feature = "multi-threaded-cf")]
#[test]
fn raw_iterators_cf_do_not_borrow_temporary_arc_handles() {
    let path = DBPath::new("_rust_rocksdb_multi_cf_iterators_temporary_handles");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    let db = DB::open_cf(&options, &path, ["first", "second"]).unwrap();

    let mut iterators = db
        .raw_iterators_cf([
            &db.cf_handle("first").unwrap(),
            &db.cf_handle("second").unwrap(),
        ])
        .unwrap();
    assert_eq!(iterators.len(), 2);
    for iterator in &mut iterators {
        iterator.seek_to_first();
        assert!(!iterator.valid());
    }
}

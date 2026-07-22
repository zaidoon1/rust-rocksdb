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

use rust_rocksdb::{DB, Options, ReadOptions, TransactionDB};
use util::DBPath;

fn assert_send_sync<T: Send + Sync>(_: &T) {}

fn seed_default_column_family(db: &DB) {
    db.put(b"k1", b"old1").unwrap();
    db.put(b"k2", b"old2").unwrap();
}

#[test]
fn snapshot_read_options_reuse_gets() {
    let path = DBPath::new("_rust_rocksdb_snapshot_read_options_gets");
    let db = DB::open_default(&path).unwrap();
    seed_default_column_family(&db);
    let snapshot = db.snapshot();
    db.put(b"k1", b"new1").unwrap();
    let reads = snapshot.read_options();
    assert_send_sync(&reads);

    for _ in 0..3 {
        assert_eq!(
            reads.get(b"k1").unwrap().as_deref(),
            Some(b"old1".as_slice())
        );
    }

    let pinned = reads.get_pinned(b"k1").unwrap().unwrap();
    assert_eq!(pinned.as_ref(), b"old1");

    let values = reads.multi_get([b"k1".as_slice(), b"k2", b"missing"]);
    assert_eq!(
        values[0].as_ref().unwrap().as_deref(),
        Some(b"old1".as_slice())
    );
    assert_eq!(
        values[1].as_ref().unwrap().as_deref(),
        Some(b"old2".as_slice())
    );
    assert_eq!(values[2].as_ref().unwrap().as_deref(), None);
}

#[test]
fn snapshot_read_options_reuse_custom_options_and_column_family() {
    let path = DBPath::new("_rust_rocksdb_snapshot_read_options_cf");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    let db = DB::open_cf(&options, &path, ["cf"]).unwrap();
    let cf = db.cf_handle("cf").unwrap();
    db.put_cf(&cf, b"cf-key", b"cf-old").unwrap();
    let snapshot = db.snapshot();
    db.put_cf(&cf, b"cf-key", b"cf-new").unwrap();

    let mut custom = ReadOptions::default();
    custom.fill_cache(false);
    let reads = snapshot.read_options_opt(custom);
    assert_eq!(
        reads.get_cf(&cf, b"cf-key").unwrap().as_deref(),
        Some(b"cf-old".as_slice())
    );
    let cf_pinned = reads.get_pinned_cf(&cf, b"cf-key").unwrap().unwrap();
    assert_eq!(cf_pinned.as_ref(), b"cf-old");
    let cf_values = reads.multi_get_cf([(&cf, b"cf-key".as_slice())]);
    assert_eq!(
        cf_values[0].as_ref().unwrap().as_deref(),
        Some(b"cf-old".as_slice())
    );
}

#[test]
fn snapshot_read_options_are_shareable() {
    let path = DBPath::new("_rust_rocksdb_snapshot_read_options_shared");
    let db = DB::open_default(&path).unwrap();
    db.put(b"k1", b"old1").unwrap();
    let snapshot = db.snapshot();
    let reads = snapshot.read_options();
    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                assert_eq!(reads.get_pinned(b"k1").unwrap().unwrap().as_ref(), b"old1");
            });
        }
    });
}

#[test]
fn snapshot_compatibility_methods_keep_snapshot_semantics() {
    let path = DBPath::new("_rust_rocksdb_snapshot_compatibility");
    let db = DB::open_default(&path).unwrap();
    seed_default_column_family(&db);
    let snapshot = db.snapshot();
    db.put(b"k1", b"new1").unwrap();
    assert_eq!(
        snapshot.get(b"k1").unwrap().as_deref(),
        Some(b"old1".as_slice())
    );
    assert_eq!(
        snapshot.get_pinned(b"k1").unwrap().unwrap().as_ref(),
        b"old1"
    );
    let values = snapshot.multi_get([b"k1".as_slice(), b"k2"]);
    assert_eq!(
        values[0].as_ref().unwrap().as_deref(),
        Some(b"old1".as_slice())
    );
    assert_eq!(
        values[1].as_ref().unwrap().as_deref(),
        Some(b"old2".as_slice())
    );
}

#[test]
fn snapshot_read_options_support_transaction_db() {
    let path = DBPath::new("_rust_rocksdb_snapshot_read_options_transaction_db");
    let db: TransactionDB = TransactionDB::open_default(&path).unwrap();
    db.put(b"k1", b"old1").unwrap();
    let snapshot = db.snapshot();
    db.put(b"k1", b"new1").unwrap();
    let reads = snapshot.read_options();

    assert_eq!(
        reads.get(b"k1").unwrap().as_deref(),
        Some(b"old1".as_slice())
    );
}

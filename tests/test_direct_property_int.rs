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

use rust_rocksdb::{DB, MultiThreaded, Options, TransactionDB, TransactionDBOptions, properties};
use util::DBPath;

const UNKNOWN_PROPERTY: &str = "rocksdb.this-property-does-not-exist";

#[test]
fn db_integer_properties_preserve_unavailable_property_semantics() {
    let path = DBPath::new("_rust_rocksdb_direct_property_int");
    let mut options = Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    let db = DB::open_cf(&options, &path, ["cf"]).unwrap();
    let cf = db.cf_handle("cf").unwrap();

    assert_eq!(
        db.property_int_value(properties::ESTIMATE_LIVE_DATA_SIZE)
            .unwrap(),
        Some(0)
    );
    assert!(db.property_int_value(properties::STATS).is_err());
    assert_eq!(db.property_int_value(UNKNOWN_PROPERTY).unwrap(), None);
    assert_eq!(
        db.property_int_value_cf(&cf, properties::ESTIMATE_NUM_KEYS)
            .unwrap(),
        Some(0)
    );
    assert_eq!(
        db.property_int_value_cf(&cf, UNKNOWN_PROPERTY).unwrap(),
        None
    );
    assert!(db.property_int_value("rocksdb.bad\0property").is_err());

    db.put(b"key", b"value").unwrap();
    db.flush().unwrap();
    assert_eq!(
        db.property_int_value(properties::num_files_at_level(0))
            .unwrap(),
        Some(1)
    );
}

#[test]
fn transaction_db_integer_properties_use_direct_api() {
    let path = DBPath::new("_rust_rocksdb_transaction_db_direct_property_int");
    let mut options = Options::default();
    options.create_if_missing(true);
    let transaction_options = TransactionDBOptions::default();
    let db = TransactionDB::<MultiThreaded>::open(&options, &transaction_options, &path).unwrap();
    db.put(b"key", b"value").unwrap();

    assert_eq!(
        db.property_int_value(properties::ESTIMATE_NUM_KEYS)
            .unwrap(),
        Some(1)
    );
    assert!(db.property_int_value(properties::STATS).is_err());
    assert_eq!(db.property_int_value(UNKNOWN_PROPERTY).unwrap(), None);
    assert!(db.property_int_value("rocksdb.bad\0property").is_err());
}

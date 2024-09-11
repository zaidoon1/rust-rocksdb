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

use pretty_assertions::assert_eq;
use std::path::Path;

use rust_rocksdb::checkpoint::Checkpoint;
use rust_rocksdb::{
    DBWithThreadMode, ExportImportFilesMetaData, ImportColumnFamilyOptions, IteratorMode,
    MultiThreaded, Options, DB,
};
use util::DBPath;

#[test]
pub fn test_single_checkpoint() {
    const PATH_PREFIX: &str = "_rust_rocksdb_cp_single_";

    // Create DB with some data
    let db_path = DBPath::new(&format!("{PATH_PREFIX}db1"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = DB::open(&opts, &db_path).unwrap();

    db.put(b"k1", b"v1").unwrap();
    db.put(b"k2", b"v2").unwrap();
    db.put(b"k3", b"v3").unwrap();
    db.put(b"k4", b"v4").unwrap();

    // Create checkpoint
    let cp1 = Checkpoint::new(&db).unwrap();
    let cp1_path = DBPath::new(&format!("{PATH_PREFIX}cp1"));
    cp1.create_checkpoint(&cp1_path).unwrap();

    // Verify checkpoint
    let cp = DB::open_default(&cp1_path).unwrap();

    assert_eq!(cp.get(b"k1").unwrap().unwrap(), b"v1");
    assert_eq!(cp.get(b"k2").unwrap().unwrap(), b"v2");
    assert_eq!(cp.get(b"k3").unwrap().unwrap(), b"v3");
    assert_eq!(cp.get(b"k4").unwrap().unwrap(), b"v4");
}

#[test]
pub fn test_multi_checkpoints() {
    const PATH_PREFIX: &str = "_rust_rocksdb_cp_multi_";

    // Create DB with some data
    let db_path = DBPath::new(&format!("{PATH_PREFIX}db1"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = DB::open(&opts, &db_path).unwrap();

    db.put(b"k1", b"v1").unwrap();
    db.put(b"k2", b"v2").unwrap();
    db.put(b"k3", b"v3").unwrap();
    db.put(b"k4", b"v4").unwrap();

    // Create first checkpoint
    let cp1 = Checkpoint::new(&db).unwrap();
    let cp1_path = DBPath::new(&format!("{PATH_PREFIX}cp1"));
    cp1.create_checkpoint(&cp1_path).unwrap();

    // Verify checkpoint
    let cp = DB::open_default(&cp1_path).unwrap();

    assert_eq!(cp.get(b"k1").unwrap().unwrap(), b"v1");
    assert_eq!(cp.get(b"k2").unwrap().unwrap(), b"v2");
    assert_eq!(cp.get(b"k3").unwrap().unwrap(), b"v3");
    assert_eq!(cp.get(b"k4").unwrap().unwrap(), b"v4");

    // Change some existing keys
    db.put(b"k1", b"modified").unwrap();
    db.put(b"k2", b"changed").unwrap();

    // Add some new keys
    db.put(b"k5", b"v5").unwrap();
    db.put(b"k6", b"v6").unwrap();

    // Create another checkpoint
    let cp2 = Checkpoint::new(&db).unwrap();
    let cp2_path = DBPath::new(&format!("{PATH_PREFIX}cp2"));
    cp2.create_checkpoint(&cp2_path).unwrap();

    // Verify second checkpoint
    let cp = DB::open_default(&cp2_path).unwrap();

    assert_eq!(cp.get(b"k1").unwrap().unwrap(), b"modified");
    assert_eq!(cp.get(b"k2").unwrap().unwrap(), b"changed");
    assert_eq!(cp.get(b"k5").unwrap().unwrap(), b"v5");
    assert_eq!(cp.get(b"k6").unwrap().unwrap(), b"v6");
}

#[test]
pub fn test_export_checkpoint_column_family() {
    const PATH_PREFIX: &str = "_rust_rocksdb_cf_export_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db-src"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = DBWithThreadMode::<MultiThreaded>::open(&opts, &db_path).unwrap();

    let opts = Options::default();
    db.create_cf("cf1", &opts).unwrap();
    db.create_cf("cf2", &opts).unwrap();

    let cf1 = db.cf_handle("cf1").unwrap();
    db.put_cf(&cf1, b"k1", b"v1").unwrap();
    db.put_cf(&cf1, b"k2", b"v2").unwrap();

    let cf2 = db.cf_handle("cf2").unwrap();
    db.put_cf(&cf2, b"k1", b"v1_cf2").unwrap();
    db.put_cf(&cf2, b"k2", b"v2_cf2").unwrap();

    // The CF will be checkpointed at the time of export, not when the struct is created
    let cp = Checkpoint::new(&db).unwrap();

    db.flush_cf(&cf1).expect("flush succeeds"); // Create an additonal SST to export
    db.delete_cf(&cf1, b"k2").unwrap();
    db.put_cf(&cf1, b"k3", b"v3").unwrap();

    let cf1_export_path = DBPath::new(&format!("{PATH_PREFIX}cf1-export"));
    let export_metadata = cp.export_column_family(&cf1, &cf1_export_path).unwrap();

    // Modify the column family after export - these changes will NOT be observable
    db.put_cf(&cf1, b"k4", b"v4").unwrap();
    db.delete_cf(&cf1, b"k1").unwrap();

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db-dest"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db_new = DBWithThreadMode::<MultiThreaded>::open(&opts, &db_path).unwrap();

    // Prepopulate some data in the destination DB - this should remain intact after import
    {
        db_new.create_cf("cf0", &opts).unwrap();
        let cf0 = db_new.cf_handle("cf0").unwrap();
        db_new.put_cf(&cf0, b"k1", b"v0").unwrap();
        db_new.put_cf(&cf0, b"k5", b"v5").unwrap();
    }

    let export_files = export_metadata.get_files();
    assert_eq!(export_files.len(), 2);

    export_files.iter().for_each(|export_file| {
        assert!(export_file.column_family_name.is_empty()); // CF export does not have the CF name
        assert!(!export_file.name.is_empty());
        assert!(!export_file.directory.is_empty());
    });

    let mut import_metadata = ExportImportFilesMetaData::default();
    import_metadata.set_db_comparator_name(&export_metadata.get_db_comparator_name());
    import_metadata.set_files(&export_files.to_vec());

    let cf_opts = Options::default();
    let mut import_opts = ImportColumnFamilyOptions::default();
    import_opts.set_move_files(true);
    db_new
        .create_column_family_with_import(&cf_opts, "cf1-new", &import_opts, &import_metadata)
        .unwrap();

    assert!(export_files.iter().all(|export_file| {
        !Path::new(&export_file.directory)
            .join(&export_file.name)
            .exists()
    }));

    let cf1_new = db_new.cf_handle("cf1-new").unwrap();
    let imported_data: Vec<_> = db_new
        .iterator_cf(&cf1_new, IteratorMode::Start)
        .map(Result::unwrap)
        .map(|(k, v)| {
            (
                String::from_utf8_lossy(&k).into_owned(),
                String::from_utf8_lossy(&v).into_owned(),
            )
        })
        .collect();
    assert_eq!(
        vec![
            ("k1".to_string(), "v1".to_string()),
            ("k3".to_string(), "v3".to_string()),
        ],
        imported_data,
    );

    let cf0 = db_new.cf_handle("cf0").unwrap();
    let original_data: Vec<_> = db_new
        .iterator_cf(&cf0, IteratorMode::Start)
        .map(Result::unwrap)
        .map(|(k, v)| {
            (
                String::from_utf8_lossy(&k).into_owned(),
                String::from_utf8_lossy(&v).into_owned(),
            )
        })
        .collect();
    assert_eq!(
        vec![
            ("k1".to_string(), "v0".to_string()),
            ("k5".to_string(), "v5".to_string()),
        ],
        original_data,
    );
}

#[test]
fn test_checkpoint_outlive_db() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/fail/checkpoint_outlive_db.rs");
}

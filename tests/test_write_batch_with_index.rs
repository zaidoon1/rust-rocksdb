use crate::util::{DBPath, assert_item, assert_no_item};
use rust_rocksdb::{ColumnFamilyDescriptor, DB, Options, ReadOptions, WriteBatchWithIndex};

mod util;

#[test]
fn test_write_batch_with_index_with_base_iterator() {
    let path = DBPath::new("_rust_rocksdb_wbwi_iterator");
    {
        let db = DB::open_default(&path).expect("DB should open");

        db.put(b"k1", b"v1").unwrap();
        db.put(b"k2", b"v2").unwrap();
        db.put(b"k3", b"v3").unwrap();
        db.put(b"k5", b"v5").unwrap();

        let mut wbwi = WriteBatchWithIndex::new(0, true);

        wbwi.put(b"k0", b"v0");
        wbwi.put(b"k4", b"v4");
        wbwi.delete(b"k3");
        wbwi.put(b"k6", b"v6");

        let mut readopts = ReadOptions::default();
        readopts.set_iterate_lower_bound(b"k2");
        readopts.set_iterate_upper_bound(b"k5");
        let base_iterator = db.raw_iterator_opt(readopts);
        let mut iterator = wbwi.iterator_with_base(base_iterator);

        iterator.seek_to_first();

        assert_item(&iterator, b"k2", b"v2");
        iterator.next();
        assert_item(&iterator, b"k4", b"v4");
        iterator.next();
        assert_no_item(&iterator);
    }
}

#[test]
fn test_write_batch_with_index_borrowed_reads() {
    let path = DBPath::new("_rust_rocksdb_wbwi_borrowed_reads");
    {
        let mut db_opts = Options::default();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);

        let cf_desc = ColumnFamilyDescriptor::new("cf1", Options::default());
        let db = DB::open_cf_descriptors(&db_opts, &path, vec![cf_desc]).expect("DB should open");
        let cf = db.cf_handle("cf1").expect("cf1 handle should exist");

        let mut wbwi = WriteBatchWithIndex::new(0, true);

        let key = b"borrowed_key";
        let val = b"borrowed_value";
        let cf_key = b"borrowed_cf_key";
        let cf_val = b"borrowed_cf_value";
        let empty_key = b"empty_key";
        let empty_cf_key = b"empty_cf_key";

        wbwi.put(key, val);
        wbwi.put_cf(&cf, cf_key, cf_val);
        wbwi.put(empty_key, []);
        wbwi.put_cf(&cf, empty_cf_key, []);

        let options = Options::default();

        let get_val = wbwi.get_from_batch(key, &options).unwrap();
        assert_eq!(get_val.as_deref(), Some(val.as_slice()));

        let get_cf_val = wbwi.get_from_batch_cf(&cf, cf_key, &options).unwrap();
        assert_eq!(get_cf_val.as_deref(), Some(cf_val.as_slice()));

        let empty_val = wbwi.get_from_batch(empty_key, &options).unwrap();
        assert_eq!(empty_val.as_deref(), Some([].as_slice()));

        let empty_cf_val = wbwi.get_from_batch_cf(&cf, empty_cf_key, &options).unwrap();
        assert_eq!(empty_cf_val.as_deref(), Some([].as_slice()));

        let found = wbwi
            .get_from_batch_with(key, &options, |slice| {
                assert_eq!(slice, val);
                true
            })
            .unwrap();
        assert_eq!(found, Some(true));

        let found_cf = wbwi
            .get_from_batch_cf_with(&cf, cf_key, &options, |slice| {
                assert_eq!(slice, cf_val);
                7
            })
            .unwrap();
        assert_eq!(found_cf, Some(7));

        let found_empty = wbwi
            .get_from_batch_with(empty_key, &options, |slice| {
                assert!(slice.is_empty());
                true
            })
            .unwrap();
        assert_eq!(found_empty, Some(true));

        let found_empty_cf = wbwi
            .get_from_batch_cf_with(&cf, empty_cf_key, &options, |slice| {
                assert!(slice.is_empty());
                11
            })
            .unwrap();
        assert_eq!(found_empty_cf, Some(11));

        let db_key = b"db_key";
        let db_val = b"db_val";
        let db_cf_key = b"db_cf_key";
        let db_cf_val = b"db_cf_val";
        let db_empty_key = b"db_empty_key";
        let db_empty_cf_key = b"db_empty_cf_key";
        db.put(db_key, db_val).unwrap();
        db.put_cf(&cf, db_cf_key, db_cf_val).unwrap();
        db.put(db_empty_key, []).unwrap();
        db.put_cf(&cf, db_empty_cf_key, []).unwrap();

        let readopts = ReadOptions::default();
        let found_and_db = wbwi
            .get_from_batch_and_db_with(&db, db_key, &readopts, |slice| {
                assert_eq!(slice, db_val);
                42
            })
            .unwrap();
        assert_eq!(found_and_db, Some(42));

        let found_and_db_cf = wbwi
            .get_from_batch_and_db_cf_with(&db, &cf, db_cf_key, &readopts, |slice| {
                assert_eq!(slice, db_cf_val);
                99
            })
            .unwrap();
        assert_eq!(found_and_db_cf, Some(99));

        let found_empty_and_db = wbwi
            .get_from_batch_and_db_with(&db, db_empty_key, &readopts, |slice| {
                assert!(slice.is_empty());
                13
            })
            .unwrap();
        assert_eq!(found_empty_and_db, Some(13));

        let found_empty_and_db_cf = wbwi
            .get_from_batch_and_db_cf_with(&db, &cf, db_empty_cf_key, &readopts, |slice| {
                assert!(slice.is_empty());
                17
            })
            .unwrap();
        assert_eq!(found_empty_and_db_cf, Some(17));
    }
}

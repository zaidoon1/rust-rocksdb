use crate::util::{DBPath, assert_item, assert_no_item};
use rust_rocksdb::{DB, ReadOptions, WriteBatchWithIndex};

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
fn test_write_batch_with_index_zero_copy() {
    let path = DBPath::new("_rust_rocksdb_wbwi_zero_copy");
    {
        let db = DB::open_default(&path).expect("DB should open");
        let mut wbwi = WriteBatchWithIndex::new(0, true);

        let key = b"zero_copy_key";
        let val = b"zero_copy_value";

        wbwi.put(key, val);

        let options = rust_rocksdb::Options::default();

        // Test get_from_batch (the safe/fixed traditional API)
        let get_val = wbwi.get_from_batch(key, &options).unwrap();
        assert_eq!(get_val.as_deref(), Some(val.as_slice()));

        // Test get_from_batch_with (the new zero-copy closure API)
        let found = wbwi
            .get_from_batch_with(key, &options, |slice| {
                assert_eq!(slice, val);
                true
            })
            .unwrap();
        assert_eq!(found, Some(true));

        // Test get_from_batch_and_db_with (the new zero-copy closure with DB fallback API)
        let db_key = b"db_key";
        let db_val = b"db_val";
        db.put(db_key, db_val).unwrap();

        let readopts = ReadOptions::default();
        let found_and_db = wbwi
            .get_from_batch_and_db_with(&db, db_key, &readopts, |slice| {
                assert_eq!(slice, db_val);
                42
            })
            .unwrap();
        assert_eq!(found_and_db, Some(42));
    }
}

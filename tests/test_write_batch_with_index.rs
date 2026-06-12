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
fn test_write_batch_with_index_get_from_batch() {
    let path = DBPath::new("_rust_rocksdb_wbwi_get");
    {
        let db = DB::open_default(&path).expect("DB should open");
        let mut wbwi = WriteBatchWithIndex::new(0, true);

        // Put keys into base DB
        db.put(b"k_db", b"v_db").unwrap();

        // Put keys into batch
        wbwi.put(b"k_batch", b"v_batch");

        let opts = rust_rocksdb::Options::default();

        // 1. Test get_from_batch in a loop to ensure memory allocation & free works stably
        for _ in 0..100 {
            let val1 = wbwi.get_from_batch(b"k_batch", &opts).unwrap().unwrap();
            assert_eq!(val1, b"v_batch");
        }

        // 2. Test get_from_batch_and_db
        let readopts = ReadOptions::default();
        let val2 = wbwi
            .get_from_batch_and_db(&db, b"k_db", &readopts)
            .unwrap()
            .unwrap();
        assert_eq!(val2, b"v_db");

        // Test non-existent keys
        assert!(
            wbwi.get_from_batch(b"non_existent", &opts)
                .unwrap()
                .is_none()
        );
        assert!(
            wbwi.get_from_batch_and_db(&db, b"non_existent", &readopts)
                .unwrap()
                .is_none()
        );
    }
}

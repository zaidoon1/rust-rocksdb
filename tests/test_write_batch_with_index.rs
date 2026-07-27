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

/// Regression test for the four `get_*_from_batch*` paths, which used to adopt
/// a buffer that RocksDB's C API allocated with `malloc` directly into a
/// `Vec<u8>` via `Vec::from_raw_parts`. That made Rust's global allocator
/// responsible for freeing memory it never allocated — heap corruption under
/// any custom `#[global_allocator]`, and on Windows. It also leaked the
/// `malloc(0)` block for every empty value, because a `Vec` with capacity 0
/// never deallocates.
///
/// Empty values are covered explicitly: they are the case where the old code
/// leaked, and where a naive `slice::from_raw_parts` on a `malloc(0)` pointer
/// would be undefined behaviour.
#[test]
fn test_get_from_batch_roundtrip_including_empty_values() {
    use rust_rocksdb::Options;

    let path = DBPath::new("_rust_rocksdb_wbwi_get_from_batch");
    {
        let db = DB::open_default(&path).expect("DB should open");
        let opts = Options::default();
        let readopts = ReadOptions::default();

        let mut wbwi = WriteBatchWithIndex::new(0, true);
        wbwi.put(b"present", b"value");
        wbwi.put(b"empty", b"");

        // Batch-only reads.
        assert_eq!(
            wbwi.get_from_batch(b"present", &opts).unwrap(),
            Some(b"value".to_vec())
        );
        assert_eq!(
            wbwi.get_from_batch(b"empty", &opts).unwrap(),
            Some(Vec::new()),
            "an empty value must read back as Some(empty), not None"
        );
        assert_eq!(wbwi.get_from_batch(b"absent", &opts).unwrap(), None);

        // Reads that fall through the batch to the DB.
        db.put(b"in_db", b"db_value").unwrap();
        db.put(b"in_db_empty", b"").unwrap();
        assert_eq!(
            wbwi.get_from_batch_and_db(&db, b"present", &readopts)
                .unwrap(),
            Some(b"value".to_vec())
        );
        assert_eq!(
            wbwi.get_from_batch_and_db(&db, b"in_db", &readopts)
                .unwrap(),
            Some(b"db_value".to_vec())
        );
        assert_eq!(
            wbwi.get_from_batch_and_db(&db, b"in_db_empty", &readopts)
                .unwrap(),
            Some(Vec::new())
        );
        assert_eq!(
            wbwi.get_from_batch_and_db(&db, b"absent", &readopts)
                .unwrap(),
            None
        );

        // Exercise the pinned variant too: its lifetime is now tied to the DB
        // rather than to the batch, since it pins a block-cache entry.
        let pinned = wbwi
            .get_pinned_from_batch_and_db(&db, b"in_db", &readopts)
            .unwrap()
            .expect("value should be present");
        assert_eq!(&*pinned, b"db_value");

        let pinned_empty = wbwi
            .get_pinned_from_batch_and_db(&db, b"in_db_empty", &readopts)
            .unwrap()
            .expect("empty value should still be Some");
        assert_eq!(&*pinned_empty, b"");
    }
}

/// Repeatedly round-trips values through the batch getters so that a
/// mismatched allocator or a double free shows up as a crash or as an
/// allocator abort rather than passing silently.
#[test]
fn test_get_from_batch_repeated_allocations() {
    use rust_rocksdb::Options;

    let path = DBPath::new("_rust_rocksdb_wbwi_get_from_batch_loop");
    {
        let db = DB::open_default(&path).expect("DB should open");
        let opts = Options::default();
        let readopts = ReadOptions::default();

        let mut wbwi = WriteBatchWithIndex::new(0, true);
        for i in 0..256u32 {
            wbwi.put(format!("k{i:04}").as_bytes(), vec![b'x'; i as usize]);
        }

        for _ in 0..8 {
            for i in 0..256u32 {
                let key = format!("k{i:04}");
                let from_batch = wbwi.get_from_batch(key.as_bytes(), &opts).unwrap();
                assert_eq!(from_batch, Some(vec![b'x'; i as usize]));

                let from_both = wbwi
                    .get_from_batch_and_db(&db, key.as_bytes(), &readopts)
                    .unwrap();
                assert_eq!(from_both, Some(vec![b'x'; i as usize]));
            }
        }
    }
}

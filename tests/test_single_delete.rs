use rust_rocksdb::{
    ColumnFamilyDescriptor, ColumnFamilyOptions, DBOptions, ReadOptions, WriteOptions, DB,
};

mod util;
use util::DBPath;

#[test]
fn test_single_delete() {
    let path = DBPath::new("_rust_rocksdb_test_single_delete");
    {
        let db = DB::open_default(&path).unwrap();

        // Test basic single_delete
        db.put(b"k1", b"v1").unwrap();
        assert_eq!(db.get(b"k1").unwrap().unwrap(), b"v1");

        db.single_delete(b"k1").unwrap();
        assert!(db.get(b"k1").unwrap().is_none());

        // Test that single_delete doesn't affect non-existent keys
        db.single_delete(b"k2").unwrap();
        assert!(db.get(b"k2").unwrap().is_none());
    }
}

#[test]
fn test_single_delete_opt() {
    let path = DBPath::new("_rust_rocksdb_test_single_delete_opt");
    {
        let db = DB::open_default(&path).unwrap();
        let write_opts = WriteOptions::default();

        // Test single_delete with write options
        db.put(b"k1", b"v1").unwrap();
        assert_eq!(db.get(b"k1").unwrap().unwrap(), b"v1");

        db.single_delete_opt(b"k1", &write_opts).unwrap();
        assert!(db.get(b"k1").unwrap().is_none());

        // Test multiple keys
        db.put(b"k2", b"v2").unwrap();
        db.put(b"k3", b"v3").unwrap();

        db.single_delete_opt(b"k2", &write_opts).unwrap();
        assert!(db.get(b"k2").unwrap().is_none());
        assert_eq!(db.get(b"k3").unwrap().unwrap(), b"v3");
    }
}

#[test]
fn test_single_delete_cf() {
    let path = DBPath::new("_rust_rocksdb_test_single_delete_cf");
    {
        let mut opts = DBOptions::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let cf1_descriptor = ColumnFamilyDescriptor::new("cf1", ColumnFamilyOptions::default());
        let cf2_descriptor = ColumnFamilyDescriptor::new("cf2", ColumnFamilyOptions::default());

        let db =
            DB::open_cf_descriptors(&opts, &path, vec![cf1_descriptor, cf2_descriptor]).unwrap();

        let cf1 = db.cf_handle("cf1").unwrap();
        let cf2 = db.cf_handle("cf2").unwrap();

        // Put and single_delete in cf1
        db.put_cf(&cf1, b"k1", b"v1").unwrap();
        assert_eq!(db.get_cf(&cf1, b"k1").unwrap().unwrap(), b"v1");

        db.single_delete_cf(&cf1, b"k1").unwrap();
        assert!(db.get_cf(&cf1, b"k1").unwrap().is_none());

        // Put and single_delete in cf2
        db.put_cf(&cf2, b"k2", b"v2").unwrap();
        assert_eq!(db.get_cf(&cf2, b"k2").unwrap().unwrap(), b"v2");

        db.single_delete_cf(&cf2, b"k2").unwrap();
        assert!(db.get_cf(&cf2, b"k2").unwrap().is_none());

        // Verify that single_delete in one cf doesn't affect another
        db.put_cf(&cf1, b"k3", b"v3").unwrap();
        db.put_cf(&cf2, b"k3", b"v3_cf2").unwrap();

        db.single_delete_cf(&cf1, b"k3").unwrap();
        assert!(db.get_cf(&cf1, b"k3").unwrap().is_none());
        assert_eq!(db.get_cf(&cf2, b"k3").unwrap().unwrap(), b"v3_cf2");
    }
}

#[test]
fn test_single_delete_cf_opt() {
    let path = DBPath::new("_rust_rocksdb_test_single_delete_cf_opt");
    {
        let mut opts = DBOptions::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let cf_descriptor = ColumnFamilyDescriptor::new("cf1", ColumnFamilyOptions::default());
        let db = DB::open_cf_descriptors(&opts, &path, vec![cf_descriptor]).unwrap();

        let cf = db.cf_handle("cf1").unwrap();
        let write_opts = WriteOptions::default();

        // Test single_delete_cf_opt
        db.put_cf(&cf, b"k1", b"v1").unwrap();
        assert_eq!(db.get_cf(&cf, b"k1").unwrap().unwrap(), b"v1");

        db.single_delete_cf_opt(&cf, b"k1", &write_opts).unwrap();
        assert!(db.get_cf(&cf, b"k1").unwrap().is_none());
    }
}

#[test]
fn test_single_delete_with_ts() {
    let path = DBPath::new("_rust_rocksdb_test_single_delete_with_ts");
    {
        let mut db_opts = DBOptions::default();
        db_opts.create_if_missing(true);

        // Set up user-defined timestamp
        let comparator_name = "test_comparator";

        let mut cf_opts = ColumnFamilyOptions::default();
        cf_opts.set_comparator_with_ts(
            comparator_name,
            8, // timestamp size
            Box::new(util::U64Comparator::compare),
            Box::new(util::U64Comparator::compare_ts),
            Box::new(util::U64Comparator::compare_without_ts),
        );

        let db = DB::open_cf_descriptors(
            &db_opts,
            &path,
            vec![ColumnFamilyDescriptor::new("default", cf_opts)],
        )
        .unwrap();

        let key = b"k1";
        let val1 = b"v1";
        let val2 = b"v2";
        let ts1 = 1_u64.to_le_bytes();
        let ts2 = 2_u64.to_le_bytes();
        let ts3 = 3_u64.to_le_bytes();

        // Put with timestamps
        db.put_with_ts(key, ts1, val1).unwrap();
        db.put_with_ts(key, ts2, val2).unwrap();

        // Read at different timestamps
        let mut opts = ReadOptions::default();
        opts.set_timestamp(ts2);
        let value = db.get_opt(key, &opts).unwrap();
        assert_eq!(value.unwrap().as_slice(), val2);

        // Single_delete with timestamp
        db.single_delete_with_ts(key, ts3).unwrap();
        opts.set_timestamp(ts3);
        let value = db.get_opt(key, &opts).unwrap();
        assert!(value.is_none());

        // ts2 should still read data before deletion
        opts.set_timestamp(ts2);
        let value = db.get_opt(key, &opts).unwrap();
        assert_eq!(value.unwrap().as_slice(), val2);
    }
}

#[test]
fn test_single_delete_with_ts_opt() {
    let path = DBPath::new("_rust_rocksdb_test_single_delete_with_ts_opt");
    {
        let mut db_opts = DBOptions::default();
        db_opts.create_if_missing(true);

        // Set up user-defined timestamp
        let comparator_name = "test_comparator";

        let mut cf_opts = ColumnFamilyOptions::default();
        cf_opts.set_comparator_with_ts(
            comparator_name,
            8, // timestamp size
            Box::new(util::U64Comparator::compare),
            Box::new(util::U64Comparator::compare_ts),
            Box::new(util::U64Comparator::compare_without_ts),
        );

        let db = DB::open_cf_descriptors(
            &db_opts,
            &path,
            vec![ColumnFamilyDescriptor::new("default", cf_opts)],
        )
        .unwrap();
        let write_opts = WriteOptions::default();

        let key = b"k1";
        let val = b"v1";
        let ts1 = 1_u64.to_le_bytes();
        let ts2 = 2_u64.to_le_bytes();

        // Put with timestamp
        db.put_with_ts(key, ts1, val).unwrap();

        // Single_delete with timestamp and write options
        db.single_delete_with_ts_opt(key, ts2, &write_opts).unwrap();

        let mut read_opts = ReadOptions::default();
        read_opts.set_timestamp(ts2);
        let value = db.get_opt(key, &read_opts).unwrap();
        assert!(value.is_none());
    }
}

#[test]
fn test_single_delete_cf_with_ts() {
    let path = DBPath::new("_rust_rocksdb_test_single_delete_cf_with_ts");
    {
        let mut opts = DBOptions::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        // Set up user-defined timestamp
        let comparator_name = "test_comparator";

        let mut cf_opts = ColumnFamilyOptions::default();
        cf_opts.set_comparator_with_ts(
            comparator_name,
            8, // timestamp size
            Box::new(util::U64Comparator::compare),
            Box::new(util::U64Comparator::compare_ts),
            Box::new(util::U64Comparator::compare_without_ts),
        );

        let cf_descriptor = ColumnFamilyDescriptor::new("cf1", cf_opts);
        let db = DB::open_cf_descriptors(&opts, &path, vec![cf_descriptor]).unwrap();

        let cf = db.cf_handle("cf1").unwrap();

        let key = b"k1";
        let val1 = b"v1";
        let val2 = b"v2";
        let ts1 = 1_u64.to_le_bytes();
        let ts2 = 2_u64.to_le_bytes();
        let ts3 = 3_u64.to_le_bytes();

        // Put with timestamps in column family
        db.put_cf_with_ts(&cf, key, ts1, val1).unwrap();
        db.put_cf_with_ts(&cf, key, ts2, val2).unwrap();

        // Read at different timestamps
        let mut opts = ReadOptions::default();
        opts.set_timestamp(ts2);
        let value = db.get_cf_opt(&cf, key, &opts).unwrap();
        assert_eq!(value.unwrap().as_slice(), val2);

        // Single_delete with timestamp in column family
        db.single_delete_cf_with_ts(&cf, key, ts3).unwrap();
        opts.set_timestamp(ts3);
        let value = db.get_cf_opt(&cf, key, &opts).unwrap();
        assert!(value.is_none());

        // ts2 should still read data before deletion
        opts.set_timestamp(ts2);
        let value = db.get_cf_opt(&cf, key, &opts).unwrap();
        assert_eq!(value.unwrap().as_slice(), val2);
    }
}

#[test]
fn test_single_delete_cf_with_ts_opt() {
    let path = DBPath::new("_rust_rocksdb_test_single_delete_cf_with_ts_opt");
    {
        let mut opts = DBOptions::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        // Set up user-defined timestamp
        let comparator_name = "test_comparator";

        let mut cf_opts = ColumnFamilyOptions::default();
        cf_opts.set_comparator_with_ts(
            comparator_name,
            8, // timestamp size
            Box::new(util::U64Comparator::compare),
            Box::new(util::U64Comparator::compare_ts),
            Box::new(util::U64Comparator::compare_without_ts),
        );

        let cf_descriptor = ColumnFamilyDescriptor::new("cf1", cf_opts);
        let db = DB::open_cf_descriptors(&opts, &path, vec![cf_descriptor]).unwrap();

        let cf = db.cf_handle("cf1").unwrap();
        let write_opts = WriteOptions::default();

        let key = b"k1";
        let val = b"v1";
        let ts1 = 1_u64.to_le_bytes();
        let ts2 = 2_u64.to_le_bytes();

        // Put with timestamp in column family
        db.put_cf_with_ts(&cf, key, ts1, val).unwrap();

        // Single_delete with timestamp and write options in column family
        db.single_delete_cf_with_ts_opt(&cf, key, ts2, &write_opts)
            .unwrap();

        let mut read_opts = ReadOptions::default();
        read_opts.set_timestamp(ts2);
        let value = db.get_cf_opt(&cf, key, &read_opts).unwrap();
        assert!(value.is_none());
    }
}

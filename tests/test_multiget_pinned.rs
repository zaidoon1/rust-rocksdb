use rust_rocksdb::{ColumnFamilyDescriptor, DB, Options, ReadOptions};

#[test]
fn multiget_pinned_default_cf() {
    let tempdir = tempfile::Builder::new()
        .prefix("rocksdb_test_multiget_pinned_default")
        .tempdir()
        .expect("create tempdir");
    let path = tempdir.path();

    let db = DB::open_default(path).unwrap();

    db.put(b"k1", b"v1").unwrap();
    db.put(b"k2", b"v2").unwrap();

    let keys = vec![b"k1".as_ref(), b"k3".as_ref(), b"k2".as_ref()];
    let res = db.multi_get_pinned(keys);

    assert_eq!(res.len(), 3);

    // k1 present
    match &res[0] {
        Ok(Some(ps)) => assert_eq!(&ps[..], b"v1"),
        _ => panic!("unexpected result for k1"),
    }

    // k3 missing
    match &res[1] {
        Ok(None) => {}
        _ => panic!("unexpected result for k3"),
    }

    // k2 present
    match &res[2] {
        Ok(Some(ps)) => assert_eq!(&ps[..], b"v2"),
        _ => panic!("unexpected result for k2"),
    }
}

#[test]
fn multiget_pinned_default_cf_opt() {
    let tempdir = tempfile::Builder::new()
        .prefix("rocksdb_test_multiget_pinned_default_opt")
        .tempdir()
        .expect("create tempdir");
    let path = tempdir.path();

    let db = DB::open_default(path).unwrap();

    db.put(b"a", b"1").unwrap();
    db.put(b"b", b"2").unwrap();

    let opts = ReadOptions::default();
    let res = db.multi_get_pinned_opt(vec![b"a", b"c", b"b"], &opts);

    assert_eq!(res.len(), 3);
    match &res[0] {
        Ok(Some(ps)) => assert_eq!(&ps[..], b"1"),
        _ => panic!("unexpected result for a"),
    }
    match &res[1] {
        Ok(None) => {}
        _ => panic!("unexpected result for c"),
    }
    match &res[2] {
        Ok(Some(ps)) => assert_eq!(&ps[..], b"2"),
        _ => panic!("unexpected result for b"),
    }
}

#[test]
fn multiget_pinned_cf() {
    let tempdir = tempfile::Builder::new()
        .prefix("rocksdb_test_multiget_pinned_cf")
        .tempdir()
        .expect("create tempdir");
    let path = tempdir.path();

    // Create DB with CF
    let mut db_opts = Options::default();
    db_opts.create_missing_column_families(true);
    db_opts.create_if_missing(true);

    let cf_desc = ColumnFamilyDescriptor::new("cf1", Options::default());
    let db = DB::open_cf_descriptors(&db_opts, path, vec![cf_desc]).unwrap();

    let cf = db.cf_handle("cf1").expect("cf1 handle");

    db.put_cf(&cf, b"k1", b"v1").unwrap();
    db.put_cf(&cf, b"k2", b"v2").unwrap();

    let items = vec![
        (&cf, b"k1".as_ref()),
        (&cf, b"k3".as_ref()),
        (&cf, b"k2".as_ref()),
    ];
    let res = db.multi_get_pinned_cf(items);

    assert_eq!(res.len(), 3);
    match &res[0] {
        Ok(Some(ps)) => assert_eq!(&ps[..], b"v1"),
        _ => panic!("unexpected result for k1"),
    }
    match &res[1] {
        Ok(None) => {}
        _ => panic!("unexpected result for k3"),
    }
    match &res[2] {
        Ok(Some(ps)) => assert_eq!(&ps[..], b"v2"),
        _ => panic!("unexpected result for k2"),
    }
}

#[test]
fn multiget_pinned_cf_opt() {
    let tempdir = tempfile::Builder::new()
        .prefix("rocksdb_test_multiget_pinned_cf_opt")
        .tempdir()
        .expect("create tempdir");
    let path = tempdir.path();

    // Create DB with CF
    let mut db_opts = Options::default();
    db_opts.create_missing_column_families(true);
    db_opts.create_if_missing(true);

    let cf_desc = ColumnFamilyDescriptor::new("cf1", Options::default());
    let db = DB::open_cf_descriptors(&db_opts, path, vec![cf_desc]).unwrap();

    let cf = db.cf_handle("cf1").expect("cf1 handle");

    db.put_cf(&cf, b"x", b"vx").unwrap();
    db.put_cf(&cf, b"y", b"vy").unwrap();

    let opts = ReadOptions::default();
    let items = vec![
        (&cf, b"x".as_ref()),
        (&cf, b"m".as_ref()),
        (&cf, b"y".as_ref()),
    ];
    let res = db.multi_get_pinned_cf_opt(items, &opts);

    assert_eq!(res.len(), 3);
    match &res[0] {
        Ok(Some(ps)) => assert_eq!(&ps[..], b"vx"),
        _ => panic!("unexpected result for x"),
    }
    match &res[1] {
        Ok(None) => {}
        _ => panic!("unexpected result for m"),
    }
    match &res[2] {
        Ok(Some(ps)) => assert_eq!(&ps[..], b"vy"),
        _ => panic!("unexpected result for y"),
    }
}

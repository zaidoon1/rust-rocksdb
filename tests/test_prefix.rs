use rust_rocksdb::{ColumnFamilyDescriptor, DB, Options, ReadOptions};

#[test]
fn prefix_exists_default_cf() {
    let tempdir = tempfile::Builder::new()
        .prefix("rocksdb_test_prefix_exists_default")
        .tempdir()
        .expect("create tempdir");
    let path = tempdir.path();

    let db = DB::open_default(path).unwrap();

    db.put(b"a1", b"v1").unwrap();
    db.put(b"a2", b"v2").unwrap();
    db.put(b"b1", b"v3").unwrap();

    assert!(db.prefix_exists(b"a").unwrap());
    assert!(db.prefix_exists(b"b").unwrap());
    assert!(!db.prefix_exists(b"c").unwrap());

    // Empty prefix matches any key when DB is non-empty
    assert!(db.prefix_exists(b"").unwrap());
}

#[test]
fn prefix_exists_with_readopts() {
    let tempdir = tempfile::Builder::new()
        .prefix("rocksdb_test_prefix_exists_readopts")
        .tempdir()
        .expect("create tempdir");
    let path = tempdir.path();

    let db = DB::open_default(path).unwrap();

    db.put(b"p1x", b"v1").unwrap();
    db.put(b"p1y", b"v2").unwrap();

    let opts = ReadOptions::default();
    assert!(db.prefix_exists_opt(b"p1", &opts).unwrap());
    assert!(!db.prefix_exists_opt(b"p2", &opts).unwrap());
}

#[test]
fn prefix_exists_cf_and_prober() {
    let tempdir = tempfile::Builder::new()
        .prefix("rocksdb_test_prefix_exists_cf")
        .tempdir()
        .expect("create tempdir");
    let path = tempdir.path();

    // Create DB with an extra CF
    let mut db_opts = Options::default();
    db_opts.create_missing_column_families(true);
    db_opts.create_if_missing(true);

    let cf_desc = ColumnFamilyDescriptor::new("cf1", Options::default());
    let db = DB::open_cf_descriptors(&db_opts, path, vec![cf_desc]).unwrap();

    let cf = db.cf_handle("cf1").expect("cf1 handle");

    // Default CF data for default prober
    db.put(b"d1", b"vd1").unwrap();
    db.put(b"d2", b"vd2").unwrap();

    // CF data for CF prober
    db.put_cf(&cf, b"x1", b"vx1").unwrap();
    db.put_cf(&cf, b"x2", b"vx2").unwrap();
    db.put_cf(&cf, b"y1", b"vy1").unwrap();

    assert!(db.prefix_exists_cf(&cf, b"x").unwrap());
    assert!(db.prefix_exists_cf(&cf, b"y").unwrap());
    assert!(!db.prefix_exists_cf(&cf, b"z").unwrap());

    // Reusable default-CF prober
    {
        let mut prober = db.prefix_prober();
        assert!(prober.exists(b"d").unwrap());
        assert!(!prober.exists(b"z").unwrap());
    }

    // Reusable CF prober
    {
        let mut prober = db.prefix_prober_cf(&cf);
        assert!(prober.exists(b"x").unwrap());
        assert!(!prober.exists(b"z").unwrap());
    }
}

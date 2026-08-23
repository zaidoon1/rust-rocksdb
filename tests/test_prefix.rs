use std::sync::Arc;

use rust_rocksdb::{ColumnFamilyDescriptor, DB, Options, OwnedPrefixProber, ReadOptions};

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

#[test]
fn prefix_prober_only_sees_writes_after_refresh() {
    let tempdir = tempfile::Builder::new()
        .prefix("rocksdb_test_prefix_prober_refresh")
        .tempdir()
        .expect("create tempdir");

    let db = DB::open_default(tempdir.path()).unwrap();
    db.put(b"a1", b"v1").unwrap();

    let mut prober = db.prefix_prober();
    assert!(prober.exists(b"a").unwrap());
    assert!(!prober.exists(b"b").unwrap());

    db.put(b"b1", b"v2").unwrap();

    // The prober reads at the sequence number current when it was built, so the
    // write above is invisible until it is refreshed.
    assert!(!prober.exists(b"b").unwrap());

    prober.refresh().unwrap();
    assert!(prober.exists(b"b").unwrap());
    assert!(prober.exists(b"a").unwrap());
}

#[test]
fn prefix_prober_refresh_survives_a_flush() {
    let tempdir = tempfile::Builder::new()
        .prefix("rocksdb_test_prefix_prober_refresh_flush")
        .tempdir()
        .expect("create tempdir");

    let db = DB::open_default(tempdir.path()).unwrap();
    db.put(b"a1", b"v1").unwrap();

    let mut prober = db.prefix_prober();
    assert!(prober.exists(b"a").unwrap());

    // Flushing installs a new superversion, which sends refresh down its
    // rebuild path rather than the cheap sequence bump.
    db.put(b"b1", b"v2").unwrap();
    db.flush().unwrap();
    db.put(b"c1", b"v3").unwrap();

    prober.refresh().unwrap();
    assert!(prober.exists(b"a").unwrap());
    assert!(prober.exists(b"b").unwrap());
    assert!(prober.exists(b"c").unwrap());
}

#[test]
fn owned_prefix_prober_default_cf() {
    let tempdir = tempfile::Builder::new()
        .prefix("rocksdb_test_owned_prefix_prober")
        .tempdir()
        .expect("create tempdir");

    let db = Arc::new(DB::open_default(tempdir.path()).unwrap());
    db.put(b"a1", b"v1").unwrap();

    let mut prober = OwnedPrefixProber::new(Arc::clone(&db));
    assert!(prober.exists(b"a").unwrap());
    assert!(!prober.exists(b"b").unwrap());

    db.put(b"b1", b"v2").unwrap();
    prober.refresh().unwrap();
    assert!(prober.exists(b"b").unwrap());
}

#[test]
fn owned_prefix_prober_cf() {
    let tempdir = tempfile::Builder::new()
        .prefix("rocksdb_test_owned_prefix_prober_cf")
        .tempdir()
        .expect("create tempdir");

    let mut db_opts = Options::default();
    db_opts.create_missing_column_families(true);
    db_opts.create_if_missing(true);
    let cf_desc = ColumnFamilyDescriptor::new("cf1", Options::default());
    let db = Arc::new(DB::open_cf_descriptors(&db_opts, tempdir.path(), vec![cf_desc]).unwrap());

    let cf = db.cf_handle("cf1").expect("cf1 handle");
    db.put_cf(&cf, b"x1", b"vx1").unwrap();

    let mut prober = OwnedPrefixProber::new_cf(Arc::clone(&db), &cf);
    assert!(prober.exists(b"x").unwrap());
    assert!(!prober.exists(b"y").unwrap());

    db.put_cf(&cf, b"y1", b"vy1").unwrap();
    prober.refresh().unwrap();
    assert!(prober.exists(b"y").unwrap());

    // The prober reads its own column family, not the default one.
    db.put(b"y2", b"vy2").unwrap();
    prober.refresh().unwrap();
    assert!(prober.exists(b"y").unwrap());
    assert!(!prober.exists(b"y2").unwrap());
}

#[test]
fn owned_prefix_prober_keeps_the_db_open() {
    // The point of the owned prober: it stays usable after every other handle
    // to the database is gone, and destroys its iterator before releasing the
    // database. A wrong field order would show up here as a use after free
    // under the ASan, UBSan and Valgrind jobs.
    let tempdir = tempfile::Builder::new()
        .prefix("rocksdb_test_owned_prefix_prober_lifetime")
        .tempdir()
        .expect("create tempdir");

    let mut prober = {
        let mut db_opts = Options::default();
        db_opts.create_missing_column_families(true);
        db_opts.create_if_missing(true);
        let cf_desc = ColumnFamilyDescriptor::new("cf1", Options::default());
        let db =
            Arc::new(DB::open_cf_descriptors(&db_opts, tempdir.path(), vec![cf_desc]).unwrap());

        let cf = db.cf_handle("cf1").expect("cf1 handle");
        db.put_cf(&cf, b"x1", b"vx1").unwrap();

        let prober = OwnedPrefixProber::new_cf(Arc::clone(&db), &cf);
        assert_eq!(Arc::strong_count(&db), 2);
        prober
    };

    // `db` and the `&ColumnFamily` borrowed from it are both out of scope now.
    assert!(prober.exists(b"x").unwrap());
    assert!(!prober.exists(b"z").unwrap());
    prober.refresh().unwrap();
    assert!(prober.exists(b"x").unwrap());

    drop(prober);
}

#[test]
fn owned_prefix_prober_moves_across_threads() {
    let tempdir = tempfile::Builder::new()
        .prefix("rocksdb_test_owned_prefix_prober_send")
        .tempdir()
        .expect("create tempdir");

    let db = Arc::new(DB::open_default(tempdir.path()).unwrap());
    db.put(b"a1", b"v1").unwrap();

    let mut prober = OwnedPrefixProber::new(Arc::clone(&db));
    assert!(prober.exists(b"a").unwrap());

    let handle = std::thread::spawn(move || {
        prober.refresh().unwrap();
        prober.exists(b"a").unwrap()
    });

    assert!(handle.join().unwrap());
}

#[test]
fn owned_prefix_prober_honours_custom_read_opts() {
    let tempdir = tempfile::Builder::new()
        .prefix("rocksdb_test_owned_prefix_prober_opts")
        .tempdir()
        .expect("create tempdir");

    let db = Arc::new(DB::open_default(tempdir.path()).unwrap());
    db.put(b"a1", b"v1").unwrap();
    db.put(b"b1", b"v2").unwrap();

    let mut opts = ReadOptions::default();
    opts.set_prefix_same_as_start(true);
    opts.fill_cache(false);

    let mut prober = OwnedPrefixProber::with_opts(Arc::clone(&db), opts);
    assert!(prober.exists(b"a").unwrap());
    assert!(prober.exists(b"b").unwrap());
    assert!(!prober.exists(b"c").unwrap());
}

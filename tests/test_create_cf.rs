mod util;

use std::time::Duration;

use rust_rocksdb::{ColumnFamilyTtl, DBWithThreadMode, MultiThreaded, Options, SingleThreaded};
use util::DBPath;

#[test]
fn create_cfs_creates_every_name() {
    let path = DBPath::new("_rust_rocksdb_create_cfs");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        let mut db = DBWithThreadMode::<SingleThreaded>::open(&opts, &path).unwrap();

        db.create_cfs(["alpha", "beta", "gamma"], &Options::default())
            .unwrap();

        for name in ["alpha", "beta", "gamma"] {
            let cf = db
                .cf_handle(name)
                .unwrap_or_else(|| panic!("{name} should exist"));
            db.put_cf(&cf, b"k", name.as_bytes()).unwrap();
            assert_eq!(
                db.get_cf(&cf, b"k").unwrap().as_deref(),
                Some(name.as_bytes())
            );
        }
    }
}

#[test]
fn create_cfs_survives_a_reopen() {
    let path = DBPath::new("_rust_rocksdb_create_cfs_reopen");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        {
            let mut db = DBWithThreadMode::<SingleThreaded>::open(&opts, &path).unwrap();
            db.create_cfs(["one", "two"], &Options::default()).unwrap();
        }

        // The manifest write has to have stuck, not just the in-memory map.
        let mut listed = DBWithThreadMode::<SingleThreaded>::list_cf(&opts, &path).unwrap();
        listed.sort();
        assert_eq!(listed, vec!["default", "one", "two"]);
    }
}

#[test]
fn create_cfs_keeps_the_families_it_created_before_failing() {
    let path = DBPath::new("_rust_rocksdb_create_cfs_conflict");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        let mut db = DBWithThreadMode::<SingleThreaded>::open(&opts, &path).unwrap();
        db.create_cfs(["taken"], &Options::default()).unwrap();

        // RocksDB creates these in order, so "fresh" is committed before "taken"
        // fails, and it stays committed.
        let err = db
            .create_cfs(["fresh", "taken"], &Options::default())
            .unwrap_err()
            .into_string();
        assert!(
            err.contains("taken") || err.contains("exists"),
            "error should name the conflict, got: {err}"
        );

        // The handle has to come back so the family can be used or dropped.
        assert!(
            db.cf_handle("fresh").is_some(),
            "the family created before the failure should be usable"
        );
        db.put_cf(&db.cf_handle("fresh").unwrap(), b"k", b"v")
            .unwrap();
    }

    // And it really is on disk, not just in the handle map.
    let listed = DBWithThreadMode::<SingleThreaded>::list_cf(&Options::default(), &path).unwrap();
    assert!(
        listed.contains(&"fresh".to_string()),
        "the partially created family should survive a reopen, got: {listed:?}"
    );
    assert!(listed.contains(&"taken".to_string()), "got: {listed:?}");
}

#[test]
fn create_cfs_accepts_an_empty_list() {
    let path = DBPath::new("_rust_rocksdb_create_cfs_empty");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        let mut db = DBWithThreadMode::<SingleThreaded>::open(&opts, &path).unwrap();

        let before = db.cf_names();
        let empty: [&str; 0] = [];
        db.create_cfs(empty, &Options::default()).unwrap();
        assert_eq!(db.cf_names(), before, "an empty list should add nothing");

        // Still usable afterwards.
        db.put(b"k", b"v").unwrap();
        assert_eq!(db.get(b"k").unwrap().as_deref(), Some(b"v".as_slice()));
    }
}

#[test]
fn create_cfs_works_on_a_multi_threaded_db() {
    let path = DBPath::new("_rust_rocksdb_create_cfs_mt");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        let db = DBWithThreadMode::<MultiThreaded>::open(&opts, &path).unwrap();

        // No &mut self here, which is the point of the multi threaded variant.
        db.create_cfs(["alpha", "beta"], &Options::default())
            .unwrap();

        let cf = db.cf_handle("beta").unwrap();
        db.put_cf(&cf, b"k", b"v").unwrap();
        assert_eq!(
            db.get_cf(&cf, b"k").unwrap().as_deref(),
            Some(b"v".as_slice())
        );
    }
}

#[test]
fn create_cf_with_ttl_adds_a_family_to_an_open_ttl_db() {
    let path = DBPath::new("_rust_rocksdb_create_cf_with_ttl");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        let mut db = DBWithThreadMode::<SingleThreaded>::open_with_ttl(
            &opts,
            &path,
            Duration::from_secs(3600),
        )
        .unwrap();

        db.create_cf_with_ttl(
            "short",
            &Options::default(),
            ColumnFamilyTtl::Duration(Duration::from_secs(60)),
        )
        .unwrap();

        let cf = db.cf_handle("short").unwrap();
        db.put_cf(&cf, b"k", b"v").unwrap();
        // Well inside the TTL, so it is still readable.
        assert_eq!(
            db.get_cf(&cf, b"k").unwrap().as_deref(),
            Some(b"v".as_slice())
        );
    }
}

#[test]
fn create_cf_with_ttl_accepts_same_as_db() {
    let path = DBPath::new("_rust_rocksdb_create_cf_with_ttl_same");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        let mut db = DBWithThreadMode::<SingleThreaded>::open_with_ttl(
            &opts,
            &path,
            Duration::from_secs(3600),
        )
        .unwrap();

        db.create_cf_with_ttl("inherit", &Options::default(), ColumnFamilyTtl::SameAsDb)
            .unwrap();

        let cf = db.cf_handle("inherit").unwrap();
        db.put_cf(&cf, b"k", b"v").unwrap();
        assert_eq!(
            db.get_cf(&cf, b"k").unwrap().as_deref(),
            Some(b"v".as_slice())
        );
    }
}

/// The C function casts the DB handle to `DBWithTTL` without checking it.
///
/// On a DB opened without a TTL that cast is undefined behaviour, so this has to be
/// refused in Rust before the call happens. If the guard is ever dropped this test
/// stops being an assertion and starts being a crash.
#[test]
fn create_cf_with_ttl_is_refused_on_a_db_without_ttl() {
    let path = DBPath::new("_rust_rocksdb_create_cf_with_ttl_rejected");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        let mut db = DBWithThreadMode::<SingleThreaded>::open(&opts, &path).unwrap();

        let err = db
            .create_cf_with_ttl(
                "nope",
                &Options::default(),
                ColumnFamilyTtl::Duration(Duration::from_secs(60)),
            )
            .unwrap_err()
            .into_string();
        assert!(
            err.contains("open_with_ttl"),
            "the error should say how to open the DB, got: {err}"
        );
        assert!(db.cf_handle("nope").is_none());

        // The DB is untouched and still usable.
        db.put(b"k", b"v").unwrap();
        assert_eq!(db.get(b"k").unwrap().as_deref(), Some(b"v".as_slice()));
    }
}

#[test]
fn create_cf_with_ttl_is_refused_on_a_multi_threaded_db_without_ttl() {
    let path = DBPath::new("_rust_rocksdb_create_cf_with_ttl_rejected_mt");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        let db = DBWithThreadMode::<MultiThreaded>::open(&opts, &path).unwrap();

        let err = db
            .create_cf_with_ttl("nope", &Options::default(), ColumnFamilyTtl::SameAsDb)
            .unwrap_err()
            .into_string();
        assert!(err.contains("open_with_ttl"), "got: {err}");
        assert!(db.cf_handle("nope").is_none());
    }
}

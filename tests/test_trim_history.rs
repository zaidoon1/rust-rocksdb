mod util;

use rust_rocksdb::{
    ColumnFamilyDescriptor, DBWithThreadMode, Options, ReadOptions, SingleThreaded,
};
use util::{DBPath, U64Comparator, U64Timestamp};

type DB = DBWithThreadMode<SingleThreaded>;

fn ts_opts() -> Options {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.set_comparator_with_ts(
        U64Comparator::NAME,
        U64Timestamp::SIZE,
        Box::new(U64Comparator::compare),
        Box::new(U64Comparator::compare_ts),
        Box::new(U64Comparator::compare_without_ts),
    );
    opts
}

/// The default family needs the timestamp comparator too, so it has to be named
/// explicitly rather than left to `Options::default()`.
fn ts_default_cf() -> Vec<ColumnFamilyDescriptor> {
    vec![ColumnFamilyDescriptor::new("default", ts_opts())]
}

fn read_at(db: &DB, key: &[u8], ts: u64) -> Option<Vec<u8>> {
    let mut readopts = ReadOptions::default();
    readopts.set_timestamp(ts.to_le_bytes());
    db.get_opt(key, &readopts).unwrap()
}

fn write_at(db: &DB, key: &[u8], value: &[u8], ts: u64) {
    db.put_with_ts(key, ts.to_le_bytes(), value).unwrap();
}

#[test]
fn open_and_trim_history_drops_writes_newer_than_the_bound() {
    let path = DBPath::new("_rust_rocksdb_trim_history");
    {
        let opts = ts_opts();

        // Three versions of the same key at increasing timestamps.
        {
            let db = DB::open_cf_descriptors(&opts, &path, ts_default_cf()).unwrap();
            write_at(&db, b"key", b"at-10", 10);
            write_at(&db, b"key", b"at-20", 20);
            write_at(&db, b"key", b"at-30", 30);

            assert_eq!(
                read_at(&db, b"key", 30).as_deref(),
                Some(b"at-30".as_slice())
            );
        }

        // Reopen trimming everything written after ts 20.
        {
            let db = DB::open_cf_descriptors_and_trim_history(
                &opts,
                &path,
                ts_default_cf(),
                &20_u64.to_le_bytes(),
            )
            .unwrap();

            // The write at 30 is gone, so reading at 30 now sees the value from 20.
            assert_eq!(
                read_at(&db, b"key", 30).as_deref(),
                Some(b"at-20".as_slice()),
                "the write past the trim bound should be gone"
            );
            assert_eq!(
                read_at(&db, b"key", 20).as_deref(),
                Some(b"at-20".as_slice())
            );
            assert_eq!(
                read_at(&db, b"key", 10).as_deref(),
                Some(b"at-10".as_slice()),
                "older versions should be untouched"
            );
        }

        // The trim is durable, not just a view on this handle.
        {
            let db = DB::open_cf_descriptors(&opts, &path, ts_default_cf()).unwrap();
            assert_eq!(
                read_at(&db, b"key", 30).as_deref(),
                Some(b"at-20".as_slice())
            );
        }
    }
}

#[test]
fn open_and_trim_history_keeps_everything_at_or_below_the_bound() {
    let path = DBPath::new("_rust_rocksdb_trim_history_noop");
    {
        let opts = ts_opts();
        {
            let db = DB::open_cf_descriptors(&opts, &path, ts_default_cf()).unwrap();
            write_at(&db, b"a", b"one", 5);
            write_at(&db, b"b", b"two", 6);
        }

        // Bound above every write, so nothing is discarded.
        let db = DB::open_cf_descriptors_and_trim_history(
            &opts,
            &path,
            ts_default_cf(),
            &100_u64.to_le_bytes(),
        )
        .unwrap();

        assert_eq!(read_at(&db, b"a", 100).as_deref(), Some(b"one".as_slice()));
        assert_eq!(read_at(&db, b"b", 100).as_deref(), Some(b"two".as_slice()));
    }
}

/// Trimming only applies to families that use user-defined timestamps.
///
/// RocksDB documents that families without them are left alone, and this also covers
/// the case where no families are named, where the default one still has to be opened.
#[test]
fn open_and_trim_history_leaves_a_family_without_timestamps_alone() {
    let path = DBPath::new("_rust_rocksdb_trim_history_no_ts");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        {
            let db = DB::open(&opts, &path).unwrap();
            db.put(b"key", b"kept").unwrap();
        }

        let db = DB::open_cf_descriptors_and_trim_history(
            &opts,
            &path,
            std::iter::empty(),
            &5_u64.to_le_bytes(),
        )
        .unwrap();

        assert_eq!(db.get(b"key").unwrap().as_deref(), Some(b"kept".as_slice()));
    }
}

#[test]
fn open_and_trim_history_by_name_opens_the_named_families() {
    let path = DBPath::new("_rust_rocksdb_trim_history_by_name");
    {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        {
            let mut db = DB::open(&opts, &path).unwrap();
            db.create_cf("other", &Options::default()).unwrap();
            let cf = db.cf_handle("other").unwrap();
            db.put_cf(&cf, b"key", b"kept").unwrap();
        }

        let db =
            DB::open_cf_and_trim_history(&opts, &path, ["default", "other"], &5_u64.to_le_bytes())
                .unwrap();

        let cf = db.cf_handle("other").unwrap();
        assert_eq!(
            db.get_cf(&cf, b"key").unwrap().as_deref(),
            Some(b"kept".as_slice())
        );
    }
}

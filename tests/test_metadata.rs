//! Coverage for [`rust_rocksdb::metadata`].
//!
//! Two things are being checked here, and the second is the reason this file exists.
//!
//! The first is that the values are right: levels, file counts, key ranges, the filters on
//! [`ColumnFamilyMetaDataOptions`], and the fields of a [`LiveFilesStorageInfo`] listing.
//!
//! The second is lifetimes. The C API hands out two kinds of borrowed handle here and the
//! crate manages them in two different ways:
//!
//! * `rocksdb_level_metadata_t` and `rocksdb_sst_file_metadata_t` are bare pointers into the
//!   `std::vector`s inside the parent `rocksdb_column_family_metadata_t`. The crate keeps the
//!   parent alive with a refcount that every child shares, so a child can outlive both the
//!   `Vec<LevelMetaData>` it came from and the `DB` that produced it. Nothing in the type
//!   signatures says so, which means only a test can show it. That is what
//!   `sst_file_metadata_outlives_its_level_and_the_db` is for, and it is the one worth
//!   running under ASAN.
//! * [`LiveFileStorageInfoEntry`](rust_rocksdb::LiveFileStorageInfoEntry) borrows its parent
//!   listing through an ordinary lifetime parameter, so the compiler already rejects the
//!   equivalent mistake and no runtime test is needed. The listing itself owns its data and
//!   outliving the `DB` is part of its contract, which several tests here rely on.

mod util;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rust_rocksdb::{
    ColumnFamilyMetaDataOptions, DB, FileChecksumGenFactory, FileType, FlushOptions, LevelMetaData,
    LiveFileStorageInfoEntry, LiveFilesStorageInfo, LiveFilesStorageInfoOptions, Options,
    SstFileMetaData, Temperature,
};
use util::DBPath;

/// Keys are grouped under two prefixes that sort well apart, so two flushes leave two L0 files
/// with disjoint key ranges. Disjoint matters: RocksDB's level 0 overlap search widens the
/// query range to cover every file it matches and then looks again, so overlapping files would
/// drag each other into results that are meant to exclude them.
const KEYS_PER_GROUP: u64 = 100;

fn key(group: u8, i: u64) -> Vec<u8> {
    format!("{}{i:05}", group as char).into_bytes()
}

fn value(i: u64) -> Vec<u8> {
    format!("{i:064}").into_bytes()
}

fn base_options() -> Options {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    // Two L0 files have to stay two L0 files for the level assertions to mean anything.
    opts.set_disable_auto_compactions(true);
    opts
}

fn flush_opts() -> FlushOptions {
    let mut fopts = FlushOptions::default();
    fopts.set_wait(true);
    fopts
}

/// Writes one group of keys and flushes it into an SST of its own.
fn write_group_and_flush(db: &DB, group: u8) {
    for i in 0..KEYS_PER_GROUP {
        db.put(key(group, i), value(i)).unwrap();
    }
    db.flush_opt(&flush_opts()).unwrap();
}

/// Writes group `a` then group `b`, leaving two files at level 0.
fn write_two_l0_files(db: &DB) {
    write_group_and_flush(db, b'a');
    write_group_and_flush(db, b'b');
}

/// The full path of the file some metadata describes.
fn sst_path(file: &SstFileMetaData) -> PathBuf {
    Path::new(&file.directory()).join(file.relative_filename())
}

/// Every file in `levels`, paired with the level it sits in.
fn all_files(levels: &[LevelMetaData]) -> Vec<(i32, SstFileMetaData)> {
    levels
        .iter()
        .flat_map(|level| {
            let number = level.level();
            level.sst_files().map(move |file| (number, file))
        })
        .collect()
}

/// The smallest key of every file in `levels`, sorted so a comparison against it does not
/// depend on the order the files came back in.
fn sorted_smallest_keys(levels: &[LevelMetaData]) -> Vec<Vec<u8>> {
    let mut keys: Vec<Vec<u8>> = all_files(levels)
        .into_iter()
        .map(|(_, file)| file.smallest_key())
        .collect();
    keys.sort();
    keys
}

/// The full path of the file a live files entry describes.
fn entry_path(entry: &LiveFileStorageInfoEntry<'_>) -> PathBuf {
    let directory = entry.directory_lossy();
    let filename = entry.relative_filename_lossy();
    Path::new(&*directory).join(&*filename)
}

#[test]
fn sst_file_metadata_outlives_its_level_and_the_db() {
    // The point of this file. `SstFileMetaData` holds a bare pointer into a vector owned by
    // the column family metadata that the query allocated, and nothing in its type ties it to
    // the `Vec<LevelMetaData>` or to the `DB`. It stays valid only because every child shares
    // ownership of that parent, so the parent is freed once the last child is gone. Getting
    // that wrong is a read after free, which is what ASAN would catch here.
    let path = DBPath::new("_rust_rocksdb_metadata_outlives");

    let orphan = {
        let db = DB::open(&base_options(), &path).unwrap();
        write_group_and_flush(&db, b'a');

        let levels =
            db.get_column_family_metadata_with_options(&ColumnFamilyMetaDataOptions::new());
        assert_eq!(levels.len(), 1, "one flush, so one non-empty level");
        let file = levels[0].sst_file(0).expect("the level has a file");

        // Both the vector of levels and the DB go out of scope here, leaving only `file`.
        drop(levels);
        drop(db);
        file
    };

    // Every getter, read after the parent handle and the DB are gone.
    let filename = orphan.relative_filename();
    let directory = orphan.directory();
    let size = orphan.size();
    let smallest = orphan.smallest_key();
    let largest = orphan.largest_key();

    assert!(
        filename.ends_with(".sst"),
        "expected an SST file name, got {filename:?}"
    );
    assert!(
        !filename.starts_with('/'),
        "relative_filename is relative to `directory`, got {filename:?}"
    );
    assert!(
        !directory.ends_with('/'),
        "directory is documented as having no trailing slash, got {directory:?}"
    );
    assert!(size > 0, "the file holds {KEYS_PER_GROUP} keys");
    assert_eq!(smallest, key(b'a', 0));
    assert_eq!(largest, key(b'a', KEYS_PER_GROUP - 1));

    // The two strings really do name a file of the reported size, which is the strongest
    // evidence available that they came from live memory rather than from a freed buffer that
    // happened to still look plausible.
    let full = Path::new(&directory).join(&filename);
    assert!(full.is_file(), "{full:?} should exist on disk");
    assert_eq!(std::fs::metadata(&full).unwrap().len(), size);

    // `Debug` reads the same handle, so it has to survive too.
    let rendered = format!("{orphan:?}");
    assert!(rendered.contains(&filename), "got {rendered}");
}

#[test]
fn level_metadata_describes_the_files_that_were_flushed() {
    let path = DBPath::new("_rust_rocksdb_metadata_levels");
    let db = DB::open(&base_options(), &path).unwrap();
    write_two_l0_files(&db);

    let levels = db.get_column_family_metadata_with_options(&ColumnFamilyMetaDataOptions::new());
    assert_eq!(
        levels.len(),
        1,
        "auto compaction is off, so both files are still at level 0"
    );
    let level = &levels[0];
    assert_eq!(level.level(), 0);
    assert_eq!(level.file_count(), 2);

    let files: Vec<SstFileMetaData> = level.sst_files().collect();
    assert_eq!(
        files.len(),
        level.file_count(),
        "the iterator covers them all"
    );
    assert!(
        level.sst_file(level.file_count()).is_none(),
        "one past the end is None rather than a wild pointer"
    );

    // A level's size is defined as the sum of its files' sizes.
    let summed: u64 = files.iter().map(SstFileMetaData::size).sum();
    assert_eq!(level.size(), summed);
    assert!(summed > 0);

    // Each file covers exactly the group it was flushed from, and none of the other group.
    let mut ranges: Vec<(Vec<u8>, Vec<u8>)> = files
        .iter()
        .map(|file| (file.smallest_key(), file.largest_key()))
        .collect();
    ranges.sort();
    assert_eq!(
        ranges,
        vec![
            (key(b'a', 0), key(b'a', KEYS_PER_GROUP - 1)),
            (key(b'b', 0), key(b'b', KEYS_PER_GROUP - 1)),
        ]
    );

    // Both files are named once each and both exist.
    let paths: HashSet<PathBuf> = files.iter().map(sst_path).collect();
    assert_eq!(paths.len(), 2, "the two files have distinct names");
    for full in &paths {
        assert!(full.is_file(), "{full:?} should exist on disk");
    }

    // `get_column_family_metadata` summarises the same version, so the totals have to agree
    // with what the levels add up to.
    let totals = db.get_column_family_metadata();
    assert_eq!(totals.name, "default");
    assert_eq!(totals.file_count, 2);
    assert_eq!(totals.size, summed);

    assert!(format!("{level:?}").contains("file_count: 2"));
}

#[test]
fn level_zero_files_come_back_newest_first() {
    // `LevelMetaData::sst_file` documents level 0 as most-recently-updated first. Group `b`
    // was flushed second, so it comes first even though its keys sort after group `a`'s.
    let path = DBPath::new("_rust_rocksdb_metadata_l0_order");
    let db = DB::open(&base_options(), &path).unwrap();
    write_two_l0_files(&db);

    let levels = db.get_column_family_metadata_with_options(&ColumnFamilyMetaDataOptions::new());
    let level = &levels[0];
    assert_eq!(level.file_count(), 2);
    assert_eq!(level.sst_file(0).unwrap().smallest_key(), key(b'b', 0));
    assert_eq!(level.sst_file(1).unwrap().smallest_key(), key(b'a', 0));
}

#[test]
fn column_family_metadata_options_round_trip() {
    // The options are exercised against a real query at the end rather than only in isolation.
    let path = DBPath::new("_rust_rocksdb_metadata_options");
    let db = DB::open(&base_options(), &path).unwrap();
    write_two_l0_files(&db);

    let mut opts = ColumnFamilyMetaDataOptions::new();
    assert_eq!(opts.get_level(), -1, "the default reports every level");
    assert_eq!(opts.get_start_key(), None);
    assert_eq!(opts.get_end_key(), None);

    opts.set_level(3);
    assert_eq!(opts.get_level(), 3);
    opts.set_level(-1);
    assert_eq!(opts.get_level(), -1);

    // Bounds are stored as handed over, including bytes that are not valid UTF-8.
    let start = key(b'a', 7);
    let end = vec![0xff, 0x00, 0xfe];
    opts.set_start_key(&start);
    opts.set_end_key(&end);
    assert_eq!(opts.get_start_key(), Some(start.clone()));
    assert_eq!(opts.get_end_key(), Some(end));

    // Clearing restores the open-ended default and leaves the other bound alone.
    opts.clear_end_key();
    assert_eq!(opts.get_end_key(), None);
    assert_eq!(opts.get_start_key(), Some(start));
    opts.clear_start_key();
    assert_eq!(opts.get_start_key(), None);

    // `Default` and `new` agree, and cleared options query the same as fresh ones.
    let defaulted = ColumnFamilyMetaDataOptions::default();
    assert_eq!(defaulted.get_level(), -1);
    assert_eq!(defaulted.get_start_key(), None);
    assert_eq!(defaulted.get_end_key(), None);
    assert_eq!(
        sorted_smallest_keys(&db.get_column_family_metadata_with_options(&defaulted)),
        sorted_smallest_keys(&db.get_column_family_metadata_with_options(&opts)),
    );

    let rendered = format!("{defaulted:?}");
    assert!(rendered.contains("level: -1"), "got {rendered}");
    assert!(rendered.contains("start_key: None"), "got {rendered}");
}

#[test]
fn metadata_level_filter_reports_only_that_level() {
    let path = DBPath::new("_rust_rocksdb_metadata_level_filter");
    let db = DB::open(&base_options(), &path).unwrap();
    write_two_l0_files(&db);

    let mut opts = ColumnFamilyMetaDataOptions::new();
    opts.set_level(0);
    let at_zero = db.get_column_family_metadata_with_options(&opts);
    assert_eq!(at_zero.len(), 1);
    assert_eq!(at_zero[0].level(), 0);
    assert_eq!(at_zero[0].file_count(), 2);

    // Nothing has been compacted, so every other level is empty. An empty level is dropped
    // from the result rather than reported with a file count of zero, which is why this is an
    // empty vector rather than a level whose `file_count` is 0.
    for level in 1..7 {
        let mut opts = ColumnFamilyMetaDataOptions::new();
        opts.set_level(level);
        assert!(
            db.get_column_family_metadata_with_options(&opts).is_empty(),
            "level {level} holds no files, so it should not be reported at all"
        );
    }
}

#[test]
fn metadata_key_range_filter_reports_overlapping_files() {
    let path = DBPath::new("_rust_rocksdb_metadata_range_filter");
    let db = DB::open(&base_options(), &path).unwrap();
    write_two_l0_files(&db);

    let query = |start: Option<Vec<u8>>, end: Option<Vec<u8>>| {
        let mut opts = ColumnFamilyMetaDataOptions::new();
        if let Some(start) = start {
            opts.set_start_key(start);
        }
        if let Some(end) = end {
            opts.set_end_key(end);
        }
        sorted_smallest_keys(&db.get_column_family_metadata_with_options(&opts))
    };

    let group_a = key(b'a', 0);
    let group_b = key(b'b', 0);

    // A bound covering one group's file reports only that file.
    assert_eq!(
        query(Some(key(b'a', 0)), Some(key(b'a', KEYS_PER_GROUP - 1))),
        vec![group_a.clone()]
    );
    assert_eq!(
        query(Some(key(b'b', 0)), Some(key(b'b', KEYS_PER_GROUP - 1))),
        vec![group_b.clone()]
    );

    // A file is reported when it overlaps the bound at all, so a single key from the middle of
    // group `a` pulls in the whole file.
    let middle = key(b'a', KEYS_PER_GROUP / 2);
    assert_eq!(
        query(Some(middle.clone()), Some(middle)),
        vec![group_a.clone()]
    );

    // A bound spanning both groups reports both.
    assert_eq!(
        query(Some(key(b'a', 0)), Some(key(b'b', KEYS_PER_GROUP - 1))),
        vec![group_a.clone(), group_b.clone()]
    );

    // One-sided bounds leave the other side open.
    assert_eq!(query(Some(key(b'b', 0)), None), vec![group_b.clone()]);
    assert_eq!(
        query(None, Some(key(b'a', KEYS_PER_GROUP - 1))),
        vec![group_a.clone()]
    );
    assert_eq!(query(None, None), vec![group_a, group_b]);

    // Bounds below, between and above every file report nothing. The middle case is the one
    // that matters: `a00100..=a00110` sits past group `a` and before group `b`.
    for (start, end) in [
        (b"A00000".to_vec(), b"A99999".to_vec()),
        (key(b'a', KEYS_PER_GROUP), key(b'a', KEYS_PER_GROUP + 10)),
        (b"z00000".to_vec(), b"z99999".to_vec()),
    ] {
        assert!(
            query(Some(start.clone()), Some(end.clone())).is_empty(),
            "no file covers {:?}..={:?}",
            String::from_utf8_lossy(&start),
            String::from_utf8_lossy(&end)
        );
    }
}

#[test]
fn metadata_for_a_column_family_covers_only_that_family() {
    let path = DBPath::new("_rust_rocksdb_metadata_cf");
    // `create_cf` takes `&self` with multi-threaded-cf and `&mut self` without.
    #[cfg(feature = "multi-threaded-cf")]
    let db = DB::open(&base_options(), &path).unwrap();
    #[cfg(not(feature = "multi-threaded-cf"))]
    let mut db = DB::open(&base_options(), &path).unwrap();
    db.create_cf("other", &base_options()).unwrap();

    // Two files in the default family, one in `other`, all with distinguishable keys.
    write_two_l0_files(&db);
    {
        let cf = db.cf_handle("other").unwrap();
        for i in 0..KEYS_PER_GROUP {
            db.put_cf(&cf, key(b'c', i), value(i)).unwrap();
        }
        db.flush_cf_opt(&cf, &flush_opts()).unwrap();
    }

    let opts = ColumnFamilyMetaDataOptions::new();
    let default_levels = db.get_column_family_metadata_with_options(&opts);
    assert_eq!(
        sorted_smallest_keys(&default_levels),
        vec![key(b'a', 0), key(b'b', 0)],
        "the default family does not see the other family's file"
    );

    let cf = db.cf_handle("other").unwrap();
    let other_levels = db.get_column_family_metadata_cf_with_options(&cf, &opts);
    assert_eq!(other_levels.len(), 1);
    assert_eq!(other_levels[0].level(), 0);
    assert_eq!(other_levels[0].file_count(), 1);
    assert_eq!(sorted_smallest_keys(&other_levels), vec![key(b'c', 0)]);

    // The two families never name the same file.
    let default_paths: HashSet<PathBuf> = all_files(&default_levels)
        .iter()
        .map(|(_, file)| sst_path(file))
        .collect();
    let other_paths: HashSet<PathBuf> = all_files(&other_levels)
        .iter()
        .map(|(_, file)| sst_path(file))
        .collect();
    assert!(default_paths.is_disjoint(&other_paths));

    // The per-family totals agree with the per-family levels.
    let other_totals = db.get_column_family_metadata_cf(&cf);
    assert_eq!(other_totals.name, "other");
    assert_eq!(other_totals.file_count, 1);
}

#[test]
fn metadata_of_an_untouched_db_is_empty() {
    let path = DBPath::new("_rust_rocksdb_metadata_empty");
    let db = DB::open(&base_options(), &path).unwrap();

    let opts = ColumnFamilyMetaDataOptions::new();
    assert!(
        db.get_column_family_metadata_with_options(&opts).is_empty(),
        "a DB with no SST files reports no levels at all"
    );

    let totals = db.get_column_family_metadata();
    assert_eq!(totals.file_count, 0);
    assert_eq!(totals.size, 0);

    // Data that is still in the memtable is not in any level yet.
    db.put(key(b'a', 0), value(0)).unwrap();
    assert!(
        db.get_column_family_metadata_with_options(&opts).is_empty(),
        "an unflushed write has no SST file to report"
    );

    db.flush_opt(&flush_opts()).unwrap();
    assert_eq!(
        db.get_column_family_metadata_with_options(&opts).len(),
        1,
        "flushing puts it in a level"
    );
}

/// Opens a DB at `path`, writes two SSTs, and returns its storage info.
///
/// The DB is dropped before this returns. That is deliberate: a `LiveFilesStorageInfo` owns
/// the vector RocksDB filled in and holds nothing borrowed from the DB, so the listing has to
/// keep working once the DB is closed.
fn storage_info_of_a_two_file_db(
    path: &DBPath,
    opts: &LiveFilesStorageInfoOptions,
) -> LiveFilesStorageInfo {
    let db = DB::open(&base_options(), path).unwrap();
    write_two_l0_files(&db);
    db.get_livefiles_storage_info(opts).unwrap()
}

#[test]
fn livefiles_storage_info_lists_everything_a_copy_needs() {
    let path = DBPath::new("_rust_rocksdb_metadata_livefiles");
    let db = DB::open(&base_options(), &path).unwrap();
    write_two_l0_files(&db);
    let db_dir = db.path().to_path_buf();

    let info = db
        .get_livefiles_storage_info(&LiveFilesStorageInfoOptions::new())
        .unwrap();

    assert!(!info.is_empty());
    assert_eq!(info.len(), info.iter().count());
    assert!(
        info.get(info.len()).is_none(),
        "the bounds check matters: the C accessors index a vector without one"
    );

    for (index, entry) in info.iter().enumerate() {
        assert_eq!(entry.index(), index, "entries report their own position");

        // Every entry names a real file. Nothing was configured onto a separate path here, so
        // that file is in the DB directory, WAL included.
        let directory = entry.directory_lossy();
        assert_eq!(
            Path::new(&*directory),
            db_dir,
            "{} landed outside the DB directory",
            entry.relative_filename_lossy()
        );
        let full = entry_path(&entry);
        assert!(
            full.is_file(),
            "{:?}, a {}, should exist on disk",
            full,
            entry.file_type().as_str()
        );
    }

    // A DB that has been written to and flushed needs its SSTs, the manifest that names them,
    // the CURRENT file that names the manifest, the OPTIONS file and the WAL.
    let types: HashSet<FileType> = info.iter().map(|entry| entry.file_type()).collect();
    for expected in [
        FileType::TableFile,
        FileType::DescriptorFile,
        FileType::CurrentFile,
        FileType::OptionsFile,
        FileType::WalFile,
    ] {
        assert!(
            types.contains(&expected),
            "expected a {} entry, got {:?}",
            expected.as_str(),
            types.iter().map(|ty| ty.as_str()).collect::<Vec<_>>()
        );
    }

    let tables: Vec<_> = info
        .iter()
        .filter(|entry| entry.file_type() == FileType::TableFile)
        .collect();
    assert_eq!(tables.len(), 2, "two flushes, two SST files");
    for table in &tables {
        assert!(table.relative_filename().ends_with(b".sst"));
        assert!(table.size() > 0);
        assert_ne!(table.file_number(), 0, "an SST always has a file number");
        assert!(
            !table.trim_to_size(),
            "an SST is complete on disk, so a length mismatch there means corruption"
        );
        assert!(
            table.replacement_contents().is_empty(),
            "CURRENT is the only file whose contents are replaced"
        );
        // No temperature was configured, so the files carry none.
        assert_eq!(table.temperature(), Temperature::Unknown);
        assert_eq!(
            std::fs::metadata(entry_path(table)).unwrap().len(),
            table.size(),
            "an SST that is not trimmed should match its recorded size exactly"
        );
    }

    // The manifest is the entry whose file on disk is allowed to have grown past the recorded
    // size, because RocksDB keeps appending to it.
    let manifest = info
        .iter()
        .find(|entry| entry.file_type() == FileType::DescriptorFile)
        .expect("checked above");
    assert!(manifest.relative_filename().starts_with(b"MANIFEST-"));
    assert!(manifest.trim_to_size());
    assert!(manifest.size() > 0);
    assert!(std::fs::metadata(entry_path(&manifest)).unwrap().len() >= manifest.size());

    // CURRENT is copied from `replacement_contents` rather than from disk, and what it should
    // contain is the manifest's name followed by a newline.
    let current = info
        .iter()
        .find(|entry| entry.file_type() == FileType::CurrentFile)
        .expect("checked above");
    assert_eq!(current.relative_filename(), b"CURRENT");
    assert_eq!(current.file_number(), 0, "CURRENT has no file number");
    let mut expected_contents = manifest.relative_filename().to_vec();
    expected_contents.push(b'\n');
    assert_eq!(current.replacement_contents(), expected_contents);
    assert_eq!(
        current.size() as usize,
        current.replacement_contents().len(),
        "size is the length of the replacement, not of the file on disk"
    );

    let options_file = info
        .iter()
        .find(|entry| entry.file_type() == FileType::OptionsFile)
        .expect("checked above");
    assert!(options_file.relative_filename().starts_with(b"OPTIONS-"));
    assert!(options_file.size() > 0);
    assert_ne!(options_file.file_number(), 0);

    let wal = info
        .iter()
        .find(|entry| entry.file_type() == FileType::WalFile)
        .expect("checked above");
    assert!(wal.relative_filename().ends_with(b".log"));
    assert_ne!(wal.file_number(), 0);

    // Debug renders the whole listing through the same accessors.
    let rendered = format!("{info:?}");
    assert!(rendered.contains("CURRENT"), "got {rendered}");
    assert!(rendered.contains("TableFile"), "got {rendered}");
}

#[test]
fn livefiles_storage_info_iterates_forwards_and_backwards() {
    let path = DBPath::new("_rust_rocksdb_metadata_livefiles_iter");
    let info = storage_info_of_a_two_file_db(&path, &LiveFilesStorageInfoOptions::new());

    let forwards: Vec<usize> = info.iter().map(|entry| entry.index()).collect();
    assert_eq!(forwards, (0..info.len()).collect::<Vec<_>>());

    let backwards: Vec<usize> = info.iter().rev().map(|entry| entry.index()).collect();
    let mut reversed = forwards.clone();
    reversed.reverse();
    assert_eq!(backwards, reversed);

    // `ExactSizeIterator` and `size_hint` both read the same range.
    let mut iter = info.iter();
    assert_eq!(iter.len(), info.len());
    assert_eq!(iter.size_hint(), (info.len(), Some(info.len())));
    iter.next();
    assert_eq!(iter.len(), info.len() - 1);

    // Fused: once it is done it stays done.
    let mut drained = info.iter();
    while drained.next().is_some() {}
    assert!(drained.next().is_none());
    assert!(drained.next().is_none());

    // `get` and the iterator describe the same entries. This also reads the listing after the
    // DB that produced it has been dropped, which the type signature already allows.
    for entry in &info {
        let same = info.get(entry.index()).unwrap();
        assert_eq!(same.relative_filename(), entry.relative_filename());
        assert_eq!(same.directory(), entry.directory());
        assert_eq!(same.file_type(), entry.file_type());
        assert_eq!(same.file_number(), entry.file_number());
        assert_eq!(same.size(), entry.size());
    }
}

#[test]
fn livefiles_storage_info_options_round_trip() {
    let path = DBPath::new("_rust_rocksdb_metadata_livefiles_options");
    let db = DB::open(&base_options(), &path).unwrap();
    write_group_and_flush(&db, b'a');

    let mut opts = LiveFilesStorageInfoOptions::new();
    assert!(
        !opts.get_include_checksum_info(),
        "checksum info is off by default"
    );
    assert_eq!(
        opts.get_wal_size_for_flush(),
        0,
        "the default always flushes"
    );
    assert!(
        !opts.get_atomic_flush(),
        "the default follows the DB-wide setting"
    );

    opts.set_include_checksum_info(true);
    opts.set_wal_size_for_flush(1 << 20);
    opts.set_atomic_flush(true);
    assert!(opts.get_include_checksum_info());
    assert_eq!(opts.get_wal_size_for_flush(), 1 << 20);
    assert!(opts.get_atomic_flush());

    let rendered = format!("{opts:?}");
    assert!(
        rendered.contains("include_checksum_info: true"),
        "got {rendered}"
    );
    assert!(
        rendered.contains("wal_size_for_flush: 1048576"),
        "got {rendered}"
    );

    // `Default` and `new` agree.
    let defaulted = LiveFilesStorageInfoOptions::default();
    assert!(!defaulted.get_include_checksum_info());
    assert_eq!(defaulted.get_wal_size_for_flush(), 0);
    assert!(!defaulted.get_atomic_flush());

    // A raised `wal_size_for_flush` suppresses the flush the query would otherwise do. The one
    // SST written above was flushed by hand, so it is listed either way.
    let info = db.get_livefiles_storage_info(&opts).unwrap();
    assert_eq!(
        info.iter()
            .filter(|entry| entry.file_type() == FileType::TableFile)
            .count(),
        1
    );
}

#[test]
fn livefiles_storage_info_checksums_are_opt_in() {
    let off_path = DBPath::new("_rust_rocksdb_metadata_livefiles_checksum_off");
    let without = storage_info_of_a_two_file_db(&off_path, &LiveFilesStorageInfoOptions::new());
    assert!(!without.is_empty());
    for entry in &without {
        assert!(
            entry.file_checksum_func_name().is_empty(),
            "{} should name no checksum function",
            entry.relative_filename_lossy()
        );
    }

    let mut opts = LiveFilesStorageInfoOptions::new();
    opts.set_include_checksum_info(true);
    let on_path = DBPath::new("_rust_rocksdb_metadata_livefiles_checksum_on");
    let with = storage_info_of_a_two_file_db(&on_path, &opts);

    // The flag decides whether the checksum fields are filled in, not which files are listed.
    let types_off: HashSet<FileType> = without.iter().map(|entry| entry.file_type()).collect();
    let types_on: HashSet<FileType> = with.iter().map(|entry| entry.file_type()).collect();
    assert_eq!(types_off, types_on);

    for entry in &with {
        // No checksum generator factory is configured, so RocksDB fills in its placeholder
        // name rather than leaving the field blank, and the checksum itself stays empty.
        assert_eq!(
            entry.file_checksum_func_name(),
            b"Unknown",
            "{} named the wrong checksum function",
            entry.relative_filename_lossy()
        );
        assert_eq!(entry.file_checksum_func_name_lossy(), "Unknown");
    }
}

#[test]
fn livefiles_storage_info_reports_a_real_checksum_for_sst_files() {
    // With a generator configured, an SST carries that generator's own name rather than the
    // `Unknown` placeholder, while the files RocksDB does not checksum keep the placeholder.
    // So this covers the other side of `livefiles_storage_info_checksums_are_opt_in` and shows
    // the field is per entry rather than one value for the whole listing.
    let path = DBPath::new("_rust_rocksdb_metadata_livefiles_crc32c");
    let mut db_opts = base_options();
    db_opts.set_file_checksum_gen_factory(&FileChecksumGenFactory::crc32c());

    let db = DB::open(&db_opts, &path).unwrap();
    write_group_and_flush(&db, b'a');

    let mut opts = LiveFilesStorageInfoOptions::new();
    opts.set_include_checksum_info(true);
    let info = db.get_livefiles_storage_info(&opts).unwrap();

    let tables: Vec<_> = info
        .iter()
        .filter(|entry| entry.file_type() == FileType::TableFile)
        .collect();
    assert_eq!(tables.len(), 1);
    assert_eq!(
        tables[0].file_checksum_func_name(),
        b"FileChecksumCrc32c",
        "the name comes from the generator rather than from the placeholder"
    );

    for entry in info.iter().filter(|e| e.file_type() != FileType::TableFile) {
        assert_eq!(
            entry.file_checksum_func_name(),
            b"Unknown",
            "{} is not checksummed",
            entry.relative_filename_lossy()
        );
    }
}

#[test]
fn livefiles_storage_info_reports_the_configured_write_temperature() {
    // The only way to see a temperature other than `Unknown` through this API without a custom
    // FileSystem: a flush stamps its output file with `default_write_temperature`. This also
    // pins the claim that `Temperature::Warm as i32` is a valid argument to the temperature
    // setters, which only holds while the discriminants match RocksDB's.
    let path = DBPath::new("_rust_rocksdb_metadata_temperature");
    let mut opts = base_options();
    opts.set_default_write_temperature(Temperature::Warm as i32);
    assert_eq!(
        opts.get_default_write_temperature(),
        Temperature::Warm as i32
    );

    let db = DB::open(&opts, &path).unwrap();
    write_group_and_flush(&db, b'a');

    let info = db
        .get_livefiles_storage_info(&LiveFilesStorageInfoOptions::new())
        .unwrap();
    let tables: Vec<_> = info
        .iter()
        .filter(|entry| entry.file_type() == FileType::TableFile)
        .collect();
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].temperature(), Temperature::Warm);
    assert_eq!(tables[0].temperature().as_str(), "Warm");

    // Only the SST is stamped. The metadata files are not written with that temperature.
    for entry in info.iter().filter(|e| e.file_type() != FileType::TableFile) {
        assert_eq!(
            entry.temperature(),
            Temperature::Unknown,
            "{} should carry no temperature",
            entry.relative_filename_lossy()
        );
    }
}

#[test]
fn livefiles_storage_info_of_an_untouched_db_has_no_table_files() {
    let path = DBPath::new("_rust_rocksdb_metadata_livefiles_empty");
    let db = DB::open(&base_options(), &path).unwrap();

    let info = db
        .get_livefiles_storage_info(&LiveFilesStorageInfoOptions::new())
        .unwrap();

    // Even an empty DB needs its manifest, CURRENT and OPTIONS copied. The query flushes by
    // default, but an empty memtable produces no file, so there is nothing at any level.
    assert!(!info.is_empty());
    assert_eq!(
        info.iter()
            .filter(|entry| entry.file_type() == FileType::TableFile)
            .count(),
        0,
        "nothing has been flushed, so there are no SST files"
    );
    for expected in [
        FileType::DescriptorFile,
        FileType::CurrentFile,
        FileType::OptionsFile,
    ] {
        assert!(
            info.iter().any(|entry| entry.file_type() == expected),
            "expected a {} entry",
            expected.as_str()
        );
    }
}

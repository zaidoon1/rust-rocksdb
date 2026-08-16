// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

//! Coverage for query, IO and block cache tracing, `TraceReader` and `Replayer`.
//!
//! Two lifetimes matter here. `TraceReader::read` hands back a buffer RocksDB
//! `malloc`ed and the crate has to copy out and free, and `TraceReader::close`
//! releases a file handle that the C++ `Read` would then dereference without
//! checking, so the crate's own closed flag is the only thing standing between
//! a closed reader and a null dereference in a release build. Both are
//! exercised below.
//!
//! Trace files live in their own temporary directory rather than in the DB
//! directory, because `DBPath` destroys the DB on drop and stray files in there
//! are not its business.

mod util;

use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use rust_rocksdb::{
    BlockCacheTraceOptions, BlockCacheTraceWriterOptions, ColumnFamily, DB, Env, EnvOptions,
    ErrorKind, Options, ReplayOptions, TraceFilter, TraceOptions, TraceReader,
};
use tempfile::TempDir;
use util::DBPath;

/// Offset of the one byte operation type inside an encoded trace record.
///
/// `TracerHelper::EncodeTrace` in trace_replay/trace_replay.cc lays a record out
/// as an 8 byte little endian timestamp, the type byte, a 4 byte little endian
/// payload length and then the payload.
const TYPE_OFFSET: usize = 8;
/// `kTraceMetadataSize`, the fixed part of a record that precedes the payload.
const METADATA_SIZE: usize = 13;

// `TraceType` values from include/rocksdb/trace_record.h. These are a persisted
// file format, so they are stable across RocksDB versions.
const TRACE_BEGIN: u8 = 1;
const TRACE_END: u8 = 2;
const TRACE_WRITE: u8 = 3;
const TRACE_GET: u8 = 4;

/// `kTraceMagic`, which every tracer puts in the payload of its first record.
const TRACE_MAGIC: &[u8] = b"feedcafedeadbeef";

/// Anything past this and the reader is not terminating, so fail rather than
/// spin. These traces hold a handful of records.
const MAX_RECORDS: usize = 10_000;

fn db_options() -> Options {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts
}

/// A directory for trace files, kept away from any DB directory.
fn trace_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("_rust_rocksdb_traces")
        .tempdir()
        .expect("failed to create a temporary directory for trace files")
}

fn read_all_records(trace_path: &Path) -> Vec<Vec<u8>> {
    let env = Env::new().unwrap();
    let mut reader = TraceReader::open(&env, trace_path).unwrap();
    drain(&mut reader)
}

fn drain(reader: &mut TraceReader) -> Vec<Vec<u8>> {
    let mut records = Vec::new();
    for _ in 0..MAX_RECORDS {
        match reader.read().unwrap() {
            Some(record) => records.push(record),
            None => return records,
        }
    }
    panic!("trace reader never reported the end of the file");
}

/// Checks a record carries its whole payload, which is what shows the reader is
/// framing records rather than handing back fixed size chunks.
#[track_caller]
fn assert_well_framed(record: &[u8]) {
    assert!(
        record.len() >= METADATA_SIZE,
        "record is shorter than the fixed header: {} bytes",
        record.len()
    );
    let payload_len = u32::from_le_bytes(record[9..13].try_into().unwrap()) as usize;
    assert_eq!(
        record.len(),
        METADATA_SIZE + payload_len,
        "record length does not match its payload length field"
    );
}

#[track_caller]
fn record_type(record: &[u8]) -> u8 {
    assert_well_framed(record);
    record[TYPE_OFFSET]
}

fn count_of_type(records: &[Vec<u8>], want: u8) -> usize {
    records.iter().filter(|r| record_type(r) == want).count()
}

fn contains_magic(record: &[u8]) -> bool {
    record.windows(TRACE_MAGIC.len()).any(|w| w == TRACE_MAGIC)
}

/// Every trace file opens with a `kTraceBegin` record carrying the magic.
#[track_caller]
fn assert_starts_with_header(records: &[Vec<u8>]) {
    let header = records
        .first()
        .expect("a trace file always holds at least its header");
    assert_eq!(record_type(header), TRACE_BEGIN);
    assert!(
        contains_magic(header),
        "the header record does not carry the trace magic"
    );
}

/// A query trace records the writes and reads that ran while it was on.
#[test]
fn test_query_trace_records_writes_and_gets() {
    let path = DBPath::new("_rust_rocksdb_trace_records");
    let traces = trace_dir();
    let trace_file: PathBuf = traces.path().join("query.trace");
    {
        let db = DB::open(&db_options(), &path).unwrap();

        // Written before tracing starts, so neither shows up in the trace.
        db.put(b"before", b"untraced").unwrap();

        db.start_trace(&TraceOptions::default(), &trace_file)
            .unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.put(b"k2", b"v2").unwrap();
        db.put(b"k3", b"v3").unwrap();
        assert_eq!(db.get(b"k1").unwrap().as_deref(), Some(&b"v1"[..]));
        assert_eq!(db.get(b"absent").unwrap(), None);
        db.end_trace().unwrap();

        // Written after tracing stops, also absent from the trace.
        db.put(b"after", b"untraced").unwrap();

        let records = read_all_records(&trace_file);
        assert_starts_with_header(&records);
        assert_eq!(
            record_type(records.last().unwrap()),
            TRACE_END,
            "end_trace writes a footer record before closing the file"
        );

        // Three puts and two gets, each its own write group because the calls
        // are sequential on one thread, plus the header and the footer.
        assert_eq!(count_of_type(&records, TRACE_WRITE), 3);
        assert_eq!(count_of_type(&records, TRACE_GET), 2);
        assert_eq!(records.len(), 7);
    }
}

/// A trace taken from one DB replays into another and reproduces its writes.
#[test]
fn test_replay_reproduces_writes_in_another_db() {
    let source_path = DBPath::new("_rust_rocksdb_replay_source");
    let target_path = DBPath::new("_rust_rocksdb_replay_target");
    let traces = trace_dir();
    let trace_file: PathBuf = traces.path().join("replay.trace");
    {
        {
            let source = DB::open(&db_options(), &source_path).unwrap();
            source
                .start_trace(&TraceOptions::default(), &trace_file)
                .unwrap();
            source.put(b"k1", b"v1").unwrap();
            source.put(b"k2", b"v2").unwrap();
            source.delete(b"k1").unwrap();
            source.put(b"k3", b"v3").unwrap();
            source.get(b"k2").unwrap();
            source.end_trace().unwrap();
        }

        let target = DB::open(&db_options(), &target_path).unwrap();
        assert_eq!(target.get(b"k2").unwrap(), None, "the target starts empty");

        // An empty column family list means the default one, which the C API
        // fills in from the DB itself.
        let mut replayer = target
            .new_default_replayer(None::<&ColumnFamily>, &trace_file)
            .unwrap();

        assert_eq!(
            replayer.header_timestamp(),
            0,
            "the header has not been read yet"
        );

        let mut replay_opts = ReplayOptions::default();
        // Scale the recorded gaps down so the replay does not sit waiting out
        // the original timings.
        replay_opts.set_fast_forward(1000.0);

        let err = replayer.replay(&replay_opts).unwrap_err();
        assert_eq!(
            err.kind(),
            ErrorKind::Incomplete,
            "replay before prepare should fail: {err}"
        );

        replayer.prepare().unwrap();
        assert!(
            replayer.header_timestamp() > 0,
            "prepare reads the header timestamp"
        );

        replayer.replay(&replay_opts).unwrap();

        assert_eq!(target.get(b"k1").unwrap(), None, "the delete replayed too");
        assert_eq!(target.get(b"k2").unwrap().as_deref(), Some(&b"v2"[..]));
        assert_eq!(target.get(b"k3").unwrap().as_deref(), Some(&b"v3"[..]));

        // The run consumed the trace, so replaying again fails until it is
        // rewound.
        let err = replayer.replay(&replay_opts).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Incomplete);

        // Preparing again rewinds it, and a second run is a no-op that still
        // succeeds because every record is idempotent.
        replayer.prepare().unwrap();
        replayer.replay(&replay_opts).unwrap();
        assert_eq!(target.get(b"k2").unwrap().as_deref(), Some(&b"v2"[..]));
    }
}

/// `fast_forward` at or below zero is rejected, and is checked before the
/// replayer has even been prepared.
#[test]
fn test_replay_rejects_non_positive_fast_forward() {
    let path = DBPath::new("_rust_rocksdb_replay_fast_forward");
    let traces = trace_dir();
    let trace_file: PathBuf = traces.path().join("fast_forward.trace");
    {
        let db = DB::open(&db_options(), &path).unwrap();
        db.start_trace(&TraceOptions::default(), &trace_file)
            .unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.end_trace().unwrap();

        let mut replayer = db
            .new_default_replayer(None::<&ColumnFamily>, &trace_file)
            .unwrap();
        replayer.prepare().unwrap();

        for bad in [0.0, -1.0] {
            let mut opts = ReplayOptions::default();
            opts.set_fast_forward(bad);
            let err = replayer.replay(&opts).unwrap_err();
            assert_eq!(
                err.kind(),
                ErrorKind::InvalidArgument,
                "fast_forward {bad} should be rejected: {err}"
            );
        }
    }
}

/// A write filter keeps write records out of the trace, so replaying it changes
/// nothing, while reads are still recorded.
#[test]
fn test_trace_filter_excludes_writes() {
    let source_path = DBPath::new("_rust_rocksdb_trace_filter_source");
    let target_path = DBPath::new("_rust_rocksdb_trace_filter_target");
    let traces = trace_dir();
    let trace_file: PathBuf = traces.path().join("filtered.trace");
    {
        {
            let source = DB::open(&db_options(), &source_path).unwrap();
            let mut trace_opts = TraceOptions::default();
            trace_opts.set_filter(TraceFilter::WRITE);
            source.start_trace(&trace_opts, &trace_file).unwrap();
            source.put(b"k1", b"v1").unwrap();
            source.put(b"k2", b"v2").unwrap();
            source.get(b"k1").unwrap();
            source.end_trace().unwrap();
        }

        let records = read_all_records(&trace_file);
        assert_starts_with_header(&records);
        assert_eq!(
            count_of_type(&records, TRACE_WRITE),
            0,
            "the write filter should have dropped both puts"
        );
        assert_eq!(
            count_of_type(&records, TRACE_GET),
            1,
            "reads are not filtered"
        );

        // Replaying a trace with no write records leaves the target untouched.
        let target = DB::open(&db_options(), &target_path).unwrap();
        let mut replayer = target
            .new_default_replayer(None::<&ColumnFamily>, &trace_file)
            .unwrap();
        replayer.prepare().unwrap();
        let mut replay_opts = ReplayOptions::default();
        replay_opts.set_fast_forward(1000.0);
        replayer.replay(&replay_opts).unwrap();

        assert_eq!(target.get(b"k1").unwrap(), None);
        assert_eq!(target.get(b"k2").unwrap(), None);
    }
}

/// `end_trace` without a running trace is an error.
#[test]
fn test_end_trace_without_start_fails() {
    let path = DBPath::new("_rust_rocksdb_end_trace_without_start");
    {
        let db = DB::open(&db_options(), &path).unwrap();
        let err = db.end_trace().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::IOError, "unexpected error: {err}");
    }
}

/// Starting a second IO trace while one is running is refused.
#[test]
fn test_start_io_trace_twice_is_busy() {
    let path = DBPath::new("_rust_rocksdb_io_trace_twice");
    let traces = trace_dir();
    {
        let db = DB::open(&db_options(), &path).unwrap();
        let opts = TraceOptions::default();

        db.start_io_trace(&opts, traces.path().join("io_first.trace"))
            .unwrap();
        let err = db
            .start_io_trace(&opts, traces.path().join("io_second.trace"))
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Busy, "unexpected error: {err}");

        db.end_io_trace().unwrap();
    }
}

/// An IO trace produces a readable file with the usual header record.
#[test]
fn test_io_trace_round_trip() {
    let path = DBPath::new("_rust_rocksdb_io_trace");
    let traces = trace_dir();
    let trace_file: PathBuf = traces.path().join("io.trace");
    {
        let db = DB::open(&db_options(), &path).unwrap();
        db.start_io_trace(&TraceOptions::default(), &trace_file)
            .unwrap();
        // Enough file activity to give the tracer something to record.
        for i in 0..32u32 {
            db.put(format!("key{i:04}").as_bytes(), b"value").unwrap();
        }
        db.flush().unwrap();
        db.get(b"key0000").unwrap();
        db.end_io_trace().unwrap();

        let records = read_all_records(&trace_file);
        assert_starts_with_header(&records);
    }
}

/// Starting a second block cache trace while one is running is refused.
#[test]
fn test_start_block_cache_trace_twice_is_busy() {
    let path = DBPath::new("_rust_rocksdb_block_cache_trace_twice");
    let traces = trace_dir();
    {
        let db = DB::open(&db_options(), &path).unwrap();
        let opts = BlockCacheTraceOptions::default();
        let writer_opts = BlockCacheTraceWriterOptions::default();

        db.start_block_cache_trace(&opts, &writer_opts, traces.path().join("bc_first.trace"))
            .unwrap();
        let err = db
            .start_block_cache_trace(&opts, &writer_opts, traces.path().join("bc_second.trace"))
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Busy, "unexpected error: {err}");

        db.end_block_cache_trace().unwrap();
    }
}

/// A block cache trace produces a readable file with the usual header record.
#[test]
fn test_block_cache_trace_round_trip() {
    let path = DBPath::new("_rust_rocksdb_block_cache_trace");
    let traces = trace_dir();
    let trace_file: PathBuf = traces.path().join("block_cache.trace");
    {
        let db = DB::open(&db_options(), &path).unwrap();
        db.start_block_cache_trace(
            &BlockCacheTraceOptions::default(),
            &BlockCacheTraceWriterOptions::default(),
            &trace_file,
        )
        .unwrap();
        for i in 0..32u32 {
            db.put(format!("key{i:04}").as_bytes(), b"value").unwrap();
        }
        db.flush().unwrap();
        for i in 0..32u32 {
            db.get(format!("key{i:04}").as_bytes()).unwrap();
        }
        db.end_block_cache_trace().unwrap();

        // The block cache writer frames its records the same way the query
        // tracer does, so the generic reader can walk it.
        let records = read_all_records(&trace_file);
        assert_starts_with_header(&records);
    }
}

/// A reader can be rewound and read again, and stays safe once it is closed.
#[test]
fn test_trace_reader_reset_and_close() {
    let path = DBPath::new("_rust_rocksdb_trace_reader_lifecycle");
    let traces = trace_dir();
    let trace_file: PathBuf = traces.path().join("lifecycle.trace");
    {
        let db = DB::open(&db_options(), &path).unwrap();
        db.start_trace(&TraceOptions::default(), &trace_file)
            .unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.put(b"k2", b"v2").unwrap();
        db.end_trace().unwrap();

        let env = Env::new().unwrap();
        let mut reader = TraceReader::open(&env, &trace_file).unwrap();

        let first_pass = drain(&mut reader);
        assert!(!first_pass.is_empty());

        // Already at the end, so another read is still `None` rather than an
        // error or a repeat.
        assert_eq!(reader.read().unwrap(), None);

        reader.reset().unwrap();
        let second_pass = drain(&mut reader);
        assert_eq!(second_pass, first_pass, "reset rewinds to the same records");

        reader.close().unwrap();

        // The C++ `Read` would dereference the file handle `Close` released, so
        // the crate has to reject this itself.
        assert!(reader.read().is_err(), "reading a closed reader must fail");
        assert!(
            reader.reset().is_err(),
            "resetting a closed reader must fail"
        );
        // Closing twice is a no-op, not a second release.
        reader.close().unwrap();

        // Dropping a closed reader still frees the C++ object exactly once.
        drop(reader);
    }
}

/// Opening a trace file that is not there fails instead of panicking.
#[test]
fn test_trace_reader_open_missing_file_fails() {
    let traces = trace_dir();
    let env = Env::new().unwrap();
    assert!(TraceReader::open(&env, traces.path().join("nope.trace")).is_err());
}

/// The reader can be given explicit env options, which only have to live for
/// the call.
#[test]
fn test_trace_reader_open_with_env_options() {
    let path = DBPath::new("_rust_rocksdb_trace_reader_env_options");
    let traces = trace_dir();
    let trace_file: PathBuf = traces.path().join("env_options.trace");
    {
        let db = DB::open(&db_options(), &path).unwrap();
        db.start_trace(&TraceOptions::default(), &trace_file)
            .unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.end_trace().unwrap();

        let env = Env::new().unwrap();
        let mut reader = {
            let env_opts = EnvOptions::default();
            TraceReader::open_with_env_options(&env, &env_opts, &trace_file).unwrap()
        };

        // The env options are gone and the reader still works, which is what
        // makes borrowing them for the call rather than the reader sound.
        let records = drain(&mut reader);
        assert_starts_with_header(&records);
        assert_eq!(count_of_type(&records, TRACE_WRITE), 1);
    }
}

/// The reader owns its `Env`, so it keeps working after the caller drops theirs.
#[test]
fn test_trace_reader_outlives_caller_env() {
    let path = DBPath::new("_rust_rocksdb_trace_reader_env_lifetime");
    let traces = trace_dir();
    let trace_file: PathBuf = traces.path().join("env_lifetime.trace");
    {
        let db = DB::open(&db_options(), &path).unwrap();
        db.start_trace(&TraceOptions::default(), &trace_file)
            .unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.end_trace().unwrap();

        let mut reader = {
            let env = Env::new().unwrap();
            TraceReader::open(&env, &trace_file).unwrap()
        };

        let records = drain(&mut reader);
        assert_starts_with_header(&records);
    }
}

/// `TraceFilter` composes as a bit set and preserves bits RocksDB has not
/// defined.
#[test]
fn test_trace_filter_bits() {
    assert_eq!(TraceFilter::empty(), TraceFilter::NONE);
    assert_eq!(TraceFilter::NONE.bits(), 0x0);
    assert_eq!(TraceFilter::GET.bits(), 0x1);
    assert_eq!(TraceFilter::WRITE.bits(), 0x2);
    assert_eq!(TraceFilter::ITERATOR_SEEK.bits(), 0x4);
    assert_eq!(TraceFilter::ITERATOR_SEEK_FOR_PREV.bits(), 0x8);
    assert_eq!(TraceFilter::MULTI_GET.bits(), 0x10);

    let combined = TraceFilter::GET | TraceFilter::WRITE;
    assert_eq!(combined.bits(), 0x3);
    assert!(combined.contains(TraceFilter::GET));
    assert!(combined.contains(TraceFilter::WRITE));
    assert!(!combined.contains(TraceFilter::MULTI_GET));
    // Everything contains the empty set, including the empty set.
    assert!(combined.contains(TraceFilter::NONE));
    assert!(TraceFilter::NONE.contains(TraceFilter::NONE));
    assert!(!TraceFilter::NONE.contains(TraceFilter::GET));

    let mut accumulated = TraceFilter::empty();
    accumulated |= TraceFilter::GET;
    accumulated |= TraceFilter::MULTI_GET;
    assert_eq!(accumulated, TraceFilter::GET | TraceFilter::MULTI_GET);

    // Undefined bits survive a round trip rather than being masked off.
    let unknown = TraceFilter::from_bits_retain(0x8000_0000_0000_0001);
    assert_eq!(unknown.bits(), 0x8000_0000_0000_0001);
    assert!(unknown.contains(TraceFilter::GET));
}

/// Undefined filter bits survive a trip through the C options struct.
#[test]
fn test_trace_options_round_trip() {
    let mut opts = TraceOptions::default();

    assert_eq!(opts.get_max_trace_file_size(), 64 * 1024 * 1024 * 1024);
    assert_eq!(opts.get_sampling_frequency(), 1);
    assert_eq!(opts.get_filter(), TraceFilter::NONE);
    assert!(!opts.get_preserve_write_order());

    opts.set_max_trace_file_size(4096);
    assert_eq!(opts.get_max_trace_file_size(), 4096);

    opts.set_sampling_frequency(10);
    assert_eq!(opts.get_sampling_frequency(), 10);

    let filter = TraceFilter::GET | TraceFilter::ITERATOR_SEEK;
    opts.set_filter(filter);
    assert_eq!(opts.get_filter(), filter);

    let unknown = TraceFilter::from_bits_retain(0x40);
    opts.set_filter(unknown);
    assert_eq!(opts.get_filter(), unknown);

    opts.set_preserve_write_order(true);
    assert!(opts.get_preserve_write_order());
}

/// The block cache trace option bags round trip too.
#[test]
fn test_block_cache_trace_options_round_trip() {
    let mut opts = BlockCacheTraceOptions::default();
    assert_eq!(opts.get_sampling_frequency(), 1);
    opts.set_sampling_frequency(7);
    assert_eq!(opts.get_sampling_frequency(), 7);

    let mut writer_opts = BlockCacheTraceWriterOptions::default();
    assert_eq!(
        writer_opts.get_max_trace_file_size(),
        64 * 1024 * 1024 * 1024
    );
    writer_opts.set_max_trace_file_size(1024);
    assert_eq!(writer_opts.get_max_trace_file_size(), 1024);
}

/// `ReplayOptions` reads back what was written.
#[test]
fn test_replay_options_round_trip() {
    let mut opts = ReplayOptions::default();
    assert_eq!(opts.get_num_threads(), 1);
    assert_eq!(opts.get_fast_forward(), 1.0);

    opts.set_num_threads(0);
    assert_eq!(opts.get_num_threads(), 0);
    opts.set_num_threads(4);
    assert_eq!(opts.get_num_threads(), 4);

    opts.set_fast_forward(2.5);
    assert_eq!(opts.get_fast_forward(), 2.5);
}

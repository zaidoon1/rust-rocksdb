//! Coverage for [`rust_rocksdb::TableProperties`].
//!
//! `TableProperties` is only reachable from an event listener callback, and every string
//! getter hands back a `&[u8]` pointing straight into a C++ `std::string` that RocksDB owns
//! for the length of that callback. So each test here drives a real flush, compaction or
//! ingestion, copies what it needs into owned values inside the callback, and asserts on the
//! copies afterwards. Anything read after the callback returns would be a use after free, and
//! the copy is what makes these tests safe to run under ASAN.
//!
//! The borrow cannot escape by accident. `FlushJobInfo::table_properties` returns
//! `TableProperties<'_>` tied to the `&FlushJobInfo` the callback was handed, and
//! `TableProperties::from_ptr` is `pub(crate)`, so there is no way to name a
//! `TableProperties<'static>` from outside the crate. Stashing one in the listener's own state
//! is rejected by the borrow checker, which is why no test tries it. A `compile_fail` doctest
//! would be the way to pin that down, but doctests do not run for integration tests, so this
//! note stands in for one.

mod util;

use std::sync::{Arc, Mutex};

use rust_rocksdb::{
    BlockBasedOptions, DB, FlushOptions, Options, SstFileWriter,
    event_listener::{CompactionJobInfo, EventListener, FlushJobInfo, IngestionInfo},
    table_properties::TableProperties,
};
use util::DBPath;

/// Keys are 8 bytes each and values 40, both fixed, so the raw size properties work out to
/// exact totals and a getter reading the wrong field lands nowhere near them. 500 keys is
/// enough to fill several data blocks without making the O(n^2) property map walks slow.
const KEY_COUNT: u64 = 500;
const KEY_LEN: u64 = 8;
const VALUE_LEN: u64 = 40;

fn key(i: u64) -> Vec<u8> {
    let k = format!("key{i:05}");
    assert_eq!(k.len() as u64, KEY_LEN);
    k.into_bytes()
}

fn value(i: u64) -> Vec<u8> {
    let v = format!("{i:040}");
    assert_eq!(v.len() as u64, VALUE_LEN);
    v.into_bytes()
}

/// Everything [`TableProperties`] exposes, copied out of the borrowed C++ strings so the test
/// can assert on it once the callback that produced it has returned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PropsSnapshot {
    orig_file_number: u64,
    data_size: u64,
    uncompressed_data_size: u64,
    index_size: u64,
    index_partitions: u64,
    top_level_index_size: u64,
    filter_size: u64,
    raw_key_size: u64,
    raw_value_size: u64,
    num_data_blocks: u64,
    num_data_blocks_compression_rejected: u64,
    num_data_blocks_compression_bypassed: u64,
    num_uniform_blocks: u64,
    num_entries: u64,
    num_filter_entries: u64,
    num_deletions: u64,
    num_merge_operands: u64,
    num_range_deletions: u64,
    format_version: u64,
    fixed_key_len: u64,
    column_family_id: u64,
    creation_time: u64,
    oldest_key_time: u64,
    newest_key_time: u64,
    file_creation_time: u64,
    slow_compression_estimated_data_size: u64,
    fast_compression_estimated_data_size: u64,
    external_sst_file_global_seqno_offset: u64,
    tail_start_offset: u64,
    key_largest_seqno: u64,
    key_smallest_seqno: u64,
    data_block_restart_interval: u64,
    index_block_restart_interval: u64,

    index_key_is_user_key: bool,
    index_value_is_delta_encoded: bool,
    udi_is_primary_index: bool,
    user_defined_timestamps_persisted: bool,
    has_key_largest_seqno: bool,
    has_key_smallest_seqno: bool,
    separate_key_value_in_data_block: bool,

    db_id: Vec<u8>,
    db_session_id: Vec<u8>,
    db_host_id: Vec<u8>,
    column_family_name: Vec<u8>,
    filter_policy_name: Vec<u8>,
    comparator_name: Vec<u8>,
    merge_operator_name: Vec<u8>,
    prefix_extractor_name: Vec<u8>,
    property_collectors_names: Vec<u8>,
    compression_name: Vec<u8>,
    compression_options: Vec<u8>,
    seqno_to_time_mapping: Vec<u8>,

    user_collected_count: usize,
    /// `user_collected_properties()` walked once.
    user_collected: Vec<(Vec<u8>, Vec<u8>)>,
    /// The same walk again, to prove the iterator is repeatable.
    user_collected_again: Vec<(Vec<u8>, Vec<u8>)>,
    /// `user_collected_property_at` driven directly for every in-range position. Kept as
    /// `Option` so a null from the C side fails an assertion out here instead of unwinding
    /// out of a C++ callback, which would abort the test process.
    user_collected_by_index: Vec<Option<(Vec<u8>, Vec<u8>)>>,
    /// `user_collected_property_at(count)`, which must run off the end.
    user_collected_past_end_is_none: bool,

    readable_count: usize,
    readable: Vec<(Vec<u8>, Vec<u8>)>,
    readable_again: Vec<(Vec<u8>, Vec<u8>)>,
    readable_by_index: Vec<Option<(Vec<u8>, Vec<u8>)>>,
    readable_past_end_is_none: bool,
}

/// Unwraps the `_at` results collected inside a callback, failing here rather than there.
fn require_all(entries: &[Option<(Vec<u8>, Vec<u8>)>], what: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    entries
        .iter()
        .enumerate()
        .map(|(pos, entry)| {
            entry
                .clone()
                .unwrap_or_else(|| panic!("{what} returned None at in-range position {pos}"))
        })
        .collect()
}

/// Copies every getter on `props` into owned values.
///
/// This is the only place the borrowed slices are touched. Everything the tests assert on is a
/// `Vec<u8>` produced here, so nothing reads C++ memory after the callback returns.
fn snapshot(props: &TableProperties<'_>) -> PropsSnapshot {
    let user_collected_count = props.user_collected_properties_count();
    let readable_count = props.readable_properties_count();

    let own = |pair: (&[u8], &[u8])| (pair.0.to_vec(), pair.1.to_vec());

    PropsSnapshot {
        orig_file_number: props.orig_file_number(),
        data_size: props.data_size(),
        uncompressed_data_size: props.uncompressed_data_size(),
        index_size: props.index_size(),
        index_partitions: props.index_partitions(),
        top_level_index_size: props.top_level_index_size(),
        filter_size: props.filter_size(),
        raw_key_size: props.raw_key_size(),
        raw_value_size: props.raw_value_size(),
        num_data_blocks: props.num_data_blocks(),
        num_data_blocks_compression_rejected: props.num_data_blocks_compression_rejected(),
        num_data_blocks_compression_bypassed: props.num_data_blocks_compression_bypassed(),
        num_uniform_blocks: props.num_uniform_blocks(),
        num_entries: props.num_entries(),
        num_filter_entries: props.num_filter_entries(),
        num_deletions: props.num_deletions(),
        num_merge_operands: props.num_merge_operands(),
        num_range_deletions: props.num_range_deletions(),
        format_version: props.format_version(),
        fixed_key_len: props.fixed_key_len(),
        column_family_id: props.column_family_id(),
        creation_time: props.creation_time(),
        oldest_key_time: props.oldest_key_time(),
        newest_key_time: props.newest_key_time(),
        file_creation_time: props.file_creation_time(),
        slow_compression_estimated_data_size: props.slow_compression_estimated_data_size(),
        fast_compression_estimated_data_size: props.fast_compression_estimated_data_size(),
        external_sst_file_global_seqno_offset: props.external_sst_file_global_seqno_offset(),
        tail_start_offset: props.tail_start_offset(),
        key_largest_seqno: props.key_largest_seqno(),
        key_smallest_seqno: props.key_smallest_seqno(),
        data_block_restart_interval: props.data_block_restart_interval(),
        index_block_restart_interval: props.index_block_restart_interval(),

        index_key_is_user_key: props.index_key_is_user_key(),
        index_value_is_delta_encoded: props.index_value_is_delta_encoded(),
        udi_is_primary_index: props.udi_is_primary_index(),
        user_defined_timestamps_persisted: props.user_defined_timestamps_persisted(),
        has_key_largest_seqno: props.has_key_largest_seqno(),
        has_key_smallest_seqno: props.has_key_smallest_seqno(),
        separate_key_value_in_data_block: props.separate_key_value_in_data_block(),

        db_id: props.db_id().to_vec(),
        db_session_id: props.db_session_id().to_vec(),
        db_host_id: props.db_host_id().to_vec(),
        column_family_name: props.column_family_name().to_vec(),
        filter_policy_name: props.filter_policy_name().to_vec(),
        comparator_name: props.comparator_name().to_vec(),
        merge_operator_name: props.merge_operator_name().to_vec(),
        prefix_extractor_name: props.prefix_extractor_name().to_vec(),
        property_collectors_names: props.property_collectors_names().to_vec(),
        compression_name: props.compression_name().to_vec(),
        compression_options: props.compression_options().to_vec(),
        seqno_to_time_mapping: props.seqno_to_time_mapping().to_vec(),

        user_collected_count,
        user_collected: props.user_collected_properties().map(own).collect(),
        user_collected_again: props.user_collected_properties().map(own).collect(),
        user_collected_by_index: (0..user_collected_count)
            .map(|pos| props.user_collected_property_at(pos).map(own))
            .collect(),
        user_collected_past_end_is_none: props
            .user_collected_property_at(user_collected_count)
            .is_none(),

        readable_count,
        readable: props.readable_properties().map(own).collect(),
        readable_again: props.readable_properties().map(own).collect(),
        readable_by_index: (0..readable_count)
            .map(|pos| props.readable_property_at(pos).map(own))
            .collect(),
        readable_past_end_is_none: props.readable_property_at(readable_count).is_none(),
    }
}

/// What a flush callback saw, plus the couple of [`FlushJobInfo`] fields the properties can be
/// cross-checked against.
#[derive(Clone, Debug)]
struct FlushCapture {
    file_number: u64,
    props: PropsSnapshot,
}

/// What a compaction callback saw.
#[derive(Clone, Debug)]
struct CompactionCapture {
    input_files: Vec<Vec<u8>>,
    output_files: Vec<Vec<u8>>,
    table_properties_count: usize,
    /// File names from `table_properties()`, walked once and then again.
    iter_names: Vec<Vec<u8>>,
    iter_names_again: Vec<Vec<u8>>,
    /// `num_entries` read through the first walk, proving the borrowed properties are usable.
    iter_num_entries: Vec<u64>,
    /// `table_property_at` driven directly for every in-range position, kept as `Option` for
    /// the same reason as [`PropsSnapshot::user_collected_by_index`].
    by_index_names: Vec<Option<Vec<u8>>>,
    past_end_is_none: bool,
    /// `table_properties_for_file` looked up with each input path.
    input_lookup_entries: Vec<Option<u64>>,
    /// `table_properties_for_file` looked up with each output path.
    output_lookup_entries: Vec<Option<u64>>,
    /// `table_properties_for_file` with names that cannot be keys of the map.
    bogus_lookups_are_none: Vec<bool>,
    /// Properties of the first input file, copied out.
    first_input_props: Option<PropsSnapshot>,
}

#[derive(Clone, Default)]
struct Capture {
    flushes: Arc<Mutex<Vec<FlushCapture>>>,
    compactions: Arc<Mutex<Vec<CompactionCapture>>>,
    ingestions: Arc<Mutex<Vec<PropsSnapshot>>>,
}

impl Capture {
    fn flushes(&self) -> Vec<FlushCapture> {
        self.flushes.lock().unwrap().clone()
    }

    fn compactions(&self) -> Vec<CompactionCapture> {
        self.compactions.lock().unwrap().clone()
    }

    fn ingestions(&self) -> Vec<PropsSnapshot> {
        self.ingestions.lock().unwrap().clone()
    }
}

impl EventListener for Capture {
    fn on_flush_completed(&self, info: &FlushJobInfo) {
        let props = info.table_properties();
        let capture = FlushCapture {
            file_number: info.file_number(),
            props: snapshot(&props),
        };
        self.flushes.lock().unwrap().push(capture);
    }

    fn on_compaction_completed(&self, info: &CompactionJobInfo) {
        let input_files: Vec<Vec<u8>> = info.input_files().map(|f| f.to_vec()).collect();
        let output_files: Vec<Vec<u8>> = info.output_files().map(|f| f.to_vec()).collect();
        let count = info.table_properties_count();

        // A real input path with its last byte lopped off, so the lookup misses only if it is
        // doing an exact match rather than something looser. Built without indexing, because a
        // panic in here would unwind into C++ and abort instead of failing a test.
        let mut bogus: Vec<Vec<u8>> = vec![Vec::new(), b"/definitely/not/a/file.sst".to_vec()];
        if let Some((_, truncated)) = input_files.first().and_then(|name| name.split_last()) {
            bogus.push(truncated.to_vec());
        }

        let capture = CompactionCapture {
            input_lookup_entries: input_files
                .iter()
                .map(|name| {
                    info.table_properties_for_file(name)
                        .map(|props| props.num_entries())
                })
                .collect(),
            output_lookup_entries: output_files
                .iter()
                .map(|name| {
                    info.table_properties_for_file(name)
                        .map(|props| props.num_entries())
                })
                .collect(),
            bogus_lookups_are_none: bogus
                .iter()
                .map(|name| info.table_properties_for_file(name).is_none())
                .collect(),
            iter_names: info
                .table_properties()
                .map(|(name, _)| name.to_vec())
                .collect(),
            iter_names_again: info
                .table_properties()
                .map(|(name, _)| name.to_vec())
                .collect(),
            iter_num_entries: info
                .table_properties()
                .map(|(_, props)| props.num_entries())
                .collect(),
            by_index_names: (0..count)
                .map(|pos| info.table_property_at(pos).map(|(name, _)| name.to_vec()))
                .collect(),
            past_end_is_none: info.table_property_at(count).is_none(),
            first_input_props: input_files
                .first()
                .and_then(|name| info.table_properties_for_file(name))
                .map(|props| snapshot(&props)),
            table_properties_count: count,
            input_files,
            output_files,
        };
        self.compactions.lock().unwrap().push(capture);
    }

    fn on_external_file_ingested(&self, info: &IngestionInfo) {
        let props = info.table_properties();
        self.ingestions.lock().unwrap().push(snapshot(&props));
    }
}

/// Options that keep the LSM shape predictable: no background compaction can fire between the
/// writes and the assertions, so a test that expects exactly one flushed file gets one.
fn base_options(capture: &Capture) -> Options {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.set_disable_auto_compactions(true);
    opts.add_event_listener(capture.clone());
    opts
}

fn flush_opts() -> FlushOptions {
    let mut fopts = FlushOptions::default();
    fopts.set_wait(true);
    fopts
}

/// Writes `KEY_COUNT` distinct keys and flushes them into a single SST.
fn write_and_flush(db: &DB) {
    for i in 0..KEY_COUNT {
        db.put(key(i), value(i)).unwrap();
    }
    db.flush_opt(&flush_opts()).unwrap();
}

/// Opens a DB, writes one SST worth of data, and returns what the flush callback saw.
fn capture_one_flush(prefix: &str) -> FlushCapture {
    let path = DBPath::new(prefix);
    let capture = Capture::default();
    {
        let db = DB::open(&base_options(&capture), &path).unwrap();
        write_and_flush(&db);
    }
    let mut flushes = capture.flushes();
    assert_eq!(flushes.len(), 1, "exactly one flush was asked for");
    flushes.pop().unwrap()
}

#[test]
fn flush_table_properties_count_what_was_written() {
    let flush = capture_one_flush("_rust_rocksdb_table_properties_counts");
    let p = &flush.props;

    // One flush of KEY_COUNT distinct puts writes exactly that many entries. RocksDB checks
    // the same equality itself in flush_job.cc before handing the info to the listener.
    assert_eq!(p.num_entries, KEY_COUNT);
    assert_eq!(p.num_deletions, 0, "no deletes were written");
    assert_eq!(p.num_merge_operands, 0, "no merges were written");
    assert_eq!(p.num_range_deletions, 0, "no range deletes were written");

    assert!(p.data_size > 0, "data blocks were written");
    assert!(p.num_data_blocks > 0, "data blocks were written");
    assert!(p.index_size > 0, "an index block is always written");
    assert!(
        p.tail_start_offset > 0,
        "the tail starts after the data blocks"
    );
    // Summed over the data blocks before compression. Values sit in the block verbatim, so
    // they are a floor; keys are not, because delta encoding elides the prefix each key shares
    // with the one before it, which is most of these keys. Not compared against `data_size`
    // either, since that is post compression and carries a trailer per block.
    assert!(
        p.uncompressed_data_size > p.raw_value_size,
        "uncompressed_data_size {} should exceed the {} bytes of values it stores verbatim",
        p.uncompressed_data_size,
        p.raw_value_size
    );

    // The builder counts internal keys, which are the user key plus an 8 byte sequence and
    // type footer. No user defined timestamps here, so nothing is subtracted back off.
    const INTERNAL_KEY_FOOTER_LEN: u64 = 8;
    assert_eq!(
        p.raw_key_size,
        KEY_COUNT * (KEY_LEN + INTERNAL_KEY_FOOTER_LEN)
    );
    assert_eq!(p.raw_value_size, KEY_COUNT * VALUE_LEN);

    // Two disjoint subsets of the data blocks: rejected means compression was tried and did
    // not pay, bypassed means it was never tried. Which way a block falls depends on whether
    // this build has a compression library, so only the bound is asserted.
    assert!(
        p.num_data_blocks_compression_rejected + p.num_data_blocks_compression_bypassed
            <= p.num_data_blocks
    );
    // Index blocks are only marked uniform when uniform_cv_threshold is set to a non-negative
    // value, and it defaults to -1.
    assert_eq!(p.num_uniform_blocks, 0);

    assert_eq!(
        p.column_family_id, 0,
        "the default column family always has id 0"
    );
    assert_eq!(
        p.fixed_key_len, 0,
        "the block based builder never records a fixed key length"
    );
    assert_eq!(
        p.orig_file_number, flush.file_number,
        "the properties name the same file the flush job info does"
    );

    // All three are seconds since the epoch, read from the system clock, so the tests below
    // are the relationships between them rather than the values, which keeps them independent
    // of what the clock says and of it stepping mid-run.
    assert!(p.creation_time > 0, "oldest ancestor time is known");
    assert!(p.file_creation_time > 0, "file creation time is known");
    assert!(p.oldest_key_time > 0, "oldest key time is known");
    // The flush job reads the clock once and uses that reading for both.
    assert_eq!(
        p.newest_key_time, p.file_creation_time,
        "a flush stamps the newest key and the file with the same clock reading"
    );
    // `creation_time` is the oldest ancestor time, which a flush computes as
    // min(now, memtable oldest key time).
    assert_eq!(
        p.creation_time,
        p.file_creation_time.min(p.oldest_key_time),
        "creation_time is the older of the file and its oldest key"
    );

    assert!(
        p.key_smallest_seqno > 0,
        "RocksDB sequence numbers start at 1"
    );
    assert!(p.key_smallest_seqno <= p.key_largest_seqno);
    assert_ne!(p.key_smallest_seqno, u64::MAX, "a real sequence number");
    assert_ne!(p.key_largest_seqno, u64::MAX, "a real sequence number");

    // Nothing here writes a partitioned index or ingests an external file.
    assert_eq!(p.index_partitions, 0);
    assert_eq!(p.top_level_index_size, 0);
    assert_eq!(p.external_sst_file_global_seqno_offset, 0);
    // sample_for_compression is off by default, so neither estimate is taken.
    assert_eq!(p.slow_compression_estimated_data_size, 0);
    assert_eq!(p.fast_compression_estimated_data_size, 0);
}

#[test]
fn flush_table_properties_without_a_filter_policy_report_no_filter() {
    let flush = capture_one_flush("_rust_rocksdb_table_properties_no_filter");
    let p = &flush.props;

    // Default `BlockBasedOptions` has no filter policy, so there is no filter block at all.
    assert_eq!(p.filter_size, 0);
    assert_eq!(p.num_filter_entries, 0);
    assert!(
        p.filter_policy_name.is_empty(),
        "filter_policy_name is empty when no policy is configured, got {:?}",
        String::from_utf8_lossy(&p.filter_policy_name)
    );
}

#[test]
fn flush_table_properties_string_getters_survive_the_callback() {
    // Every byte asserted on here was copied inside `on_flush_completed`. The `TableProperties`
    // that produced it, and the `FlushJobInfo` it borrowed from, are long gone by now.
    let flush = capture_one_flush("_rust_rocksdb_table_properties_strings");
    let p = &flush.props;

    assert_eq!(p.column_family_name, b"default");
    assert_eq!(p.comparator_name, b"leveldb.BytewiseComparator");
    // RocksDB writes the literal string "nullptr" rather than leaving these empty.
    assert_eq!(p.merge_operator_name, b"nullptr");
    assert_eq!(p.prefix_extractor_name, b"nullptr");
    // The list is empty but still bracketed.
    assert_eq!(p.property_collectors_names, b"[]");

    assert!(!p.db_id.is_empty(), "a DB always has an identity");
    assert!(
        !p.db_session_id.is_empty(),
        "a session id is generated on every open"
    );
    // db_host_id starts as the "__hostname__" placeholder and is resolved when the file is
    // written. It can legitimately end up empty if the hostname cannot be read, but the
    // placeholder must never survive into the file.
    assert!(
        !contains(&p.db_host_id, b"__hostname__"),
        "db_host_id placeholder was not resolved: {:?}",
        String::from_utf8_lossy(&p.db_host_id)
    );

    assert!(!p.compression_name.is_empty());
    // `compression_name` is documented as a built in name below format version 7 and
    // "<compatibility_name>;<hex coded compression types>;<future use>" from 7 on. Keying the
    // shape off the file's own `format_version` keeps this honest across upgrades instead of
    // pinning whichever form today's default produces.
    let semicolons = p.compression_name.iter().filter(|b| **b == b';').count();
    if p.format_version >= 7 {
        assert_eq!(
            semicolons,
            2,
            "format version {} should use the three field compression name, got {:?}",
            p.format_version,
            String::from_utf8_lossy(&p.compression_name)
        );
        let hex = p.compression_name.split(|b| *b == b';').nth(1).unwrap();
        assert_eq!(
            hex.len() % 2,
            0,
            "the compression type field is two hex digits per type, got {:?}",
            String::from_utf8_lossy(hex)
        );
        assert!(
            hex.iter()
                .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(b)),
            "the compression type field is upper case hex, got {:?}",
            String::from_utf8_lossy(hex)
        );
    } else {
        assert_eq!(
            semicolons,
            0,
            "format version {} should use a plain built in compression name, got {:?}",
            p.format_version,
            String::from_utf8_lossy(&p.compression_name)
        );
    }

    // RocksDB always appends the configured compression type as a pseudo option, so this is
    // never empty even with the default compression options.
    assert!(
        contains(&p.compression_options, b"_type="),
        "compression_options should record the configured type, got {:?}",
        String::from_utf8_lossy(&p.compression_options)
    );

    // Not asserted on beyond reading it: `seqno_to_time_mapping` is only populated when
    // preclude_last_level_data_seconds is configured, which this DB does not do.
    assert!(p.seqno_to_time_mapping.is_empty());
}

#[test]
fn flush_table_properties_boolean_flags() {
    let flush = capture_one_flush("_rust_rocksdb_table_properties_flags");
    let p = &flush.props;

    // A user defined index is never configured here, so the standard index stays primary.
    assert!(!p.udi_is_primary_index);
    // Only recorded as false for a column family with user defined timestamps that does not
    // persist them. This DB has no timestamps.
    assert!(p.user_defined_timestamps_persisted);
    // Off by default in BlockBasedOptions.
    assert!(!p.separate_key_value_in_data_block);
    // The file is not empty, so both sequence numbers are real.
    assert!(p.has_key_smallest_seqno);
    assert!(p.has_key_largest_seqno);

    // Index values are delta encoded from format version 4 on, as long as block alignment is
    // off. Gating on the file's own format version rather than hard coding today's default.
    if p.format_version >= 4 {
        assert!(p.index_value_is_delta_encoded);
    }

    // `index_key_is_user_key` is the inverse of whether any index separator needed the
    // sequence number appended, which follows from the key distribution rather than from any
    // setting. There is no configuration independent value to pin it to here, so it is only
    // checked for stability in `table_properties_agree_across_two_reads_of_the_same_file`.
}

#[test]
fn flush_table_properties_reflect_configured_block_options() {
    // Round-trips three block based settings through the SST and back out of the properties, so
    // a getter reading the wrong field shows up as a mismatch rather than as a plausible number.
    let path = DBPath::new("_rust_rocksdb_table_properties_block_opts");
    let capture = Capture::default();
    {
        let mut opts = base_options(&capture);
        let mut block_opts = BlockBasedOptions::default();
        block_opts.set_block_restart_interval(8);
        block_opts.set_index_block_restart_interval(4);
        block_opts.set_bloom_filter(10.0, false);
        opts.set_block_based_table_factory(&block_opts);

        let db = DB::open(&opts, &path).unwrap();
        write_and_flush(&db);
    }

    let flushes = capture.flushes();
    assert_eq!(flushes.len(), 1);
    let p = &flushes[0].props;

    assert_eq!(p.data_block_restart_interval, 8);
    assert_eq!(p.index_block_restart_interval, 4);

    // `filter_policy_name` is the policy's `Name()`, and the full bloom policy the C API
    // builds calls itself "bloomfilter".
    assert_eq!(p.filter_policy_name, b"bloomfilter");
    assert!(p.filter_size > 0, "a filter block was written");
    // Whole key filtering is on and every key is distinct, so the builder is handed exactly
    // one key per entry and its count is exact rather than approximate.
    assert_eq!(p.num_filter_entries, KEY_COUNT);
    assert_eq!(p.num_entries, KEY_COUNT);
}

#[test]
fn flush_table_properties_property_maps_are_repeatable_and_ordered() {
    let flush = capture_one_flush("_rust_rocksdb_table_properties_maps");
    let p = &flush.props;

    // Both walks are documented as O(n^2) because the C API restarts from the beginning of the
    // map for every position. That is why they are only walked over the handful of entries a
    // default file carries, and why this test checks the walks agree rather than timing them.
    assert_eq!(
        p.user_collected.len(),
        p.user_collected_count,
        "the iterator yields exactly `user_collected_properties_count` entries"
    );
    assert_eq!(
        p.user_collected, p.user_collected_again,
        "walking the user collected properties twice gives the same entries"
    );
    assert_eq!(
        p.user_collected,
        require_all(&p.user_collected_by_index, "user_collected_property_at"),
        "the iterator agrees with `user_collected_property_at`"
    );
    assert!(p.user_collected_past_end_is_none);

    // A plain block based file carries exactly the four entries its built in collector writes,
    // in the key order a std::map gives them. Names and values are RocksDB constants:
    // `BlockBasedTablePropertyNames`, and `kPropTrue`/`kPropFalse` which are "1" and "0". The
    // index type is not a string at all, it is a little endian fixed 32 of the `IndexType`
    // enum, and kBinarySearch is 0.
    let expected: Vec<(Vec<u8>, Vec<u8>)> = vec![
        (
            // Written whenever `decouple_partitioned_filters` is on, which is the default as
            // of RocksDB 11.8.1, and unlike the other three it is omitted rather than set to
            // "0" when off.
            b"rocksdb.block.based.table.decoupled.partitioned.filters".to_vec(),
            b"1".to_vec(),
        ),
        (
            b"rocksdb.block.based.table.index.type".to_vec(),
            vec![0, 0, 0, 0],
        ),
        (
            // No prefix extractor is configured, so prefix filtering is off.
            b"rocksdb.block.based.table.prefix.filtering".to_vec(),
            b"0".to_vec(),
        ),
        (
            // whole_key_filtering defaults to true, and is recorded even with no filter policy.
            b"rocksdb.block.based.table.whole.key.filtering".to_vec(),
            b"1".to_vec(),
        ),
    ];
    assert_eq!(p.user_collected, expected);

    // The empty case. Nothing in a plain DB returns anything from
    // `TablePropertiesCollector::GetReadableProperties`, so the readable map is empty. The
    // iterator, the count and the out of range read all have to agree about that.
    assert_eq!(p.readable_count, 0);
    assert!(p.readable.is_empty());
    assert_eq!(p.readable, p.readable_again);
    assert!(p.readable_by_index.is_empty());
    assert!(p.readable_past_end_is_none);
}

#[test]
fn compaction_table_properties_are_keyed_by_input_and_output_file_names() {
    let path = DBPath::new("_rust_rocksdb_table_properties_compaction");
    let capture = Capture::default();
    {
        let db = DB::open(&base_options(&capture), &path).unwrap();

        // Two flushes of the same key range give two L0 files that overlap, so the manual
        // compaction below has to read and merge them rather than trivially moving them.
        for i in 0..KEY_COUNT {
            db.put(key(i), value(i)).unwrap();
        }
        db.flush_opt(&flush_opts()).unwrap();
        for i in 0..KEY_COUNT {
            db.put(key(i), value(i + KEY_COUNT)).unwrap();
        }
        db.flush_opt(&flush_opts()).unwrap();

        assert_eq!(capture.flushes().len(), 2, "two files at L0");

        db.compact_range(None::<&[u8]>, None::<&[u8]>);
    }

    let compactions = capture.compactions();
    assert_eq!(compactions.len(), 1, "exactly one manual compaction");
    let c = &compactions[0];

    assert_eq!(c.input_files.len(), 2, "both L0 files were compacted");
    assert!(!c.output_files.is_empty());

    // The map holds an entry per input file and per output file.
    assert_eq!(
        c.table_properties_count,
        c.input_files.len() + c.output_files.len()
    );

    // Looking a known input or output path up by name must find it.
    for (name, entries) in c.input_files.iter().zip(&c.input_lookup_entries) {
        assert_eq!(
            *entries,
            Some(KEY_COUNT),
            "no properties for input file {:?}",
            String::from_utf8_lossy(name)
        );
    }
    for (name, entries) in c.output_files.iter().zip(&c.output_lookup_entries) {
        assert!(
            entries.is_some(),
            "no properties for output file {:?}",
            String::from_utf8_lossy(name)
        );
    }
    // Both inputs held the same KEY_COUNT keys, so the merged output holds exactly that many.
    assert_eq!(
        c.output_lookup_entries.iter().flatten().sum::<u64>(),
        KEY_COUNT
    );

    // A name that is not a key comes back empty rather than panicking or matching loosely.
    assert_eq!(
        c.bogus_lookups_are_none,
        vec![true, true, true],
        "empty, absent and truncated file names must all miss"
    );

    // The iterator covers the whole map, is repeatable, and agrees with `table_property_at`.
    assert_eq!(c.iter_names.len(), c.table_properties_count);
    assert_eq!(c.iter_names, c.iter_names_again);
    let by_index: Vec<Vec<u8>> = c
        .by_index_names
        .iter()
        .enumerate()
        .map(|(pos, name)| {
            name.clone()
                .unwrap_or_else(|| panic!("table_property_at returned None at position {pos}"))
        })
        .collect();
    assert_eq!(c.iter_names, by_index);
    assert!(c.past_end_is_none);
    assert_eq!(c.iter_num_entries.len(), c.table_properties_count);
    assert!(
        c.iter_num_entries.iter().all(|n| *n == KEY_COUNT),
        "every input and output file in this compaction holds KEY_COUNT entries, got {:?}",
        c.iter_num_entries
    );

    // Every input and output path is a key of the map, and nothing else is.
    //
    // Sorted before comparing on purpose. Unlike the maps on `TableProperties`, which are
    // std::map, this one is a `TablePropertiesCollection`, which is a std::unordered_map, so
    // the order the C API walks it in is not the key order.
    let mut expected: Vec<Vec<u8>> = c
        .input_files
        .iter()
        .chain(&c.output_files)
        .cloned()
        .collect();
    expected.sort();
    let mut got = c.iter_names.clone();
    got.sort();
    assert_eq!(got, expected);

    // The properties reached through the compaction are the same shape as the ones reached
    // through a flush, which is the point of `table_properties_for_file` returning the real
    // thing rather than a stub.
    let input_props = c.first_input_props.as_ref().unwrap();
    assert_eq!(input_props.num_entries, KEY_COUNT);
    assert_eq!(input_props.column_family_name, b"default");
    assert_eq!(input_props.comparator_name, b"leveldb.BytewiseComparator");
    assert_eq!(input_props.raw_value_size, KEY_COUNT * VALUE_LEN);
    assert!(input_props.data_size > 0);
}

#[test]
fn ingested_file_table_properties_describe_the_ingested_file() {
    let path = DBPath::new("_rust_rocksdb_table_properties_ingest");
    let sst_dir = tempfile::Builder::new()
        .prefix("_rust_rocksdb_table_properties_ingest_sst")
        .tempdir()
        .unwrap();
    let sst_path = sst_dir.path().join("ingest.sst");

    let ingested_keys: u64 = 32;
    {
        let opts = Options::default();
        let mut writer = SstFileWriter::create(&opts);
        writer.open(&sst_path).unwrap();
        for i in 0..ingested_keys {
            writer.put(key(i), value(i)).unwrap();
        }
        writer.finish().unwrap();
    }

    let capture = Capture::default();
    {
        let db = DB::open(&base_options(&capture), &path).unwrap();
        db.ingest_external_file(vec![&sst_path]).unwrap();
        // The ingested keys are readable, so the file really did land in the DB.
        assert_eq!(db.get(key(0)).unwrap().unwrap(), value(0));
    }

    let ingestions = capture.ingestions();
    assert_eq!(ingestions.len(), 1, "one file was ingested");
    let p = &ingestions[0];

    assert_eq!(p.num_entries, ingested_keys);
    assert_eq!(p.raw_value_size, ingested_keys * VALUE_LEN);
    assert_eq!(p.comparator_name, b"leveldb.BytewiseComparator");
    assert!(p.data_size > 0);
    assert!(!p.compression_name.is_empty());
    // Read back out of the file's property block, so the block based collector's entries are
    // there just as they are for a flush.
    assert!(
        p.user_collected
            .iter()
            .any(|(k, _)| k == b"rocksdb.block.based.table.index.type"),
        "expected the block based table properties in the ingested file"
    );
    assert_eq!(
        p.user_collected,
        require_all(&p.user_collected_by_index, "user_collected_property_at")
    );
}

#[test]
fn table_properties_agree_across_two_reads_of_the_same_file() {
    // Two flushes of identical content should produce identical properties everywhere the
    // values are not per file. This catches a getter that returns whatever happens to be at a
    // fixed offset rather than reading the named field.
    let path = DBPath::new("_rust_rocksdb_table_properties_stable");
    let capture = Capture::default();
    {
        let db = DB::open(&base_options(&capture), &path).unwrap();
        write_and_flush(&db);
        write_and_flush(&db);
    }

    let flushes = capture.flushes();
    assert_eq!(flushes.len(), 2);
    let (first, second) = (&flushes[0].props, &flushes[1].props);

    assert_eq!(first.num_entries, second.num_entries);
    assert_eq!(first.raw_key_size, second.raw_key_size);
    assert_eq!(first.raw_value_size, second.raw_value_size);
    assert_eq!(first.num_data_blocks, second.num_data_blocks);
    assert_eq!(first.column_family_name, second.column_family_name);
    assert_eq!(first.comparator_name, second.comparator_name);
    assert_eq!(first.compression_name, second.compression_name);
    assert_eq!(first.compression_options, second.compression_options);
    assert_eq!(first.user_collected, second.user_collected);
    assert_eq!(first.format_version, second.format_version);
    assert_eq!(
        first.data_block_restart_interval,
        second.data_block_restart_interval
    );
    assert_eq!(
        first.index_block_restart_interval,
        second.index_block_restart_interval
    );
    // Same keys and same value lengths in both files, so the index separators resolve the
    // same way and this flag has to land on the same side twice.
    assert_eq!(first.index_key_is_user_key, second.index_key_is_user_key);
    assert_eq!(
        first.index_value_is_delta_encoded,
        second.index_value_is_delta_encoded
    );
    assert_eq!(
        first.db_id, second.db_id,
        "the DB identity does not change between flushes"
    );
    assert_eq!(
        first.db_session_id, second.db_session_id,
        "the session id does not change within one open"
    );

    // The two files are still distinct.
    assert_ne!(first.orig_file_number, second.orig_file_number);
    assert!(first.key_largest_seqno < second.key_smallest_seqno);
}

/// Whether `haystack` contains `needle` as a contiguous run of bytes.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

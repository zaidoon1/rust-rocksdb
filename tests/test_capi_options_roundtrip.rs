// Round-trip coverage for the RocksDB C API options exposed in this change.
//
// Each option is set to a known value and read back through the matching
// getter, which is what catches a setter and getter that disagree or a bool
// cast that got inverted.

use rust_rocksdb::{
    BlockBasedOptions, CompactOptions, CuckooTableOptions, FifoCompactOptions, FlushOptions,
    IngestExternalFileOptions, Options, ReadOptions, TransactionDBOptions, TransactionOptions,
    UniversalCompactOptions, WaitForCompactOptions, WriteOptions,
};

#[test]
fn roundtrip_block_based_options() {
    let mut o = BlockBasedOptions::default();
    o.set_data_block_hash_table_util_ratio(0.5);
    assert_eq!(
        o.get_data_block_hash_table_util_ratio(),
        0.5,
        "BlockBasedOptions::data_block_hash_table_util_ratio"
    );
    let mut o = BlockBasedOptions::default();
    o.set_decouple_partitioned_filters(true);
    assert!(
        o.get_decouple_partitioned_filters(),
        "BlockBasedOptions::decouple_partitioned_filters"
    );
    let mut o = BlockBasedOptions::default();
    o.set_decouple_partitioned_filters(false);
    assert!(
        !o.get_decouple_partitioned_filters(),
        "BlockBasedOptions::decouple_partitioned_filters"
    );
    let mut o = BlockBasedOptions::default();
    o.set_detect_filter_construct_corruption(true);
    assert!(
        o.get_detect_filter_construct_corruption(),
        "BlockBasedOptions::detect_filter_construct_corruption"
    );
    let mut o = BlockBasedOptions::default();
    o.set_detect_filter_construct_corruption(false);
    assert!(
        !o.get_detect_filter_construct_corruption(),
        "BlockBasedOptions::detect_filter_construct_corruption"
    );
    let mut o = BlockBasedOptions::default();
    o.set_enable_index_compression(true);
    assert!(
        o.get_enable_index_compression(),
        "BlockBasedOptions::enable_index_compression"
    );
    let mut o = BlockBasedOptions::default();
    o.set_enable_index_compression(false);
    assert!(
        !o.get_enable_index_compression(),
        "BlockBasedOptions::enable_index_compression"
    );
    let mut o = BlockBasedOptions::default();
    o.set_fail_if_no_udi_on_open(true);
    assert!(
        o.get_fail_if_no_udi_on_open(),
        "BlockBasedOptions::fail_if_no_udi_on_open"
    );
    let mut o = BlockBasedOptions::default();
    o.set_fail_if_no_udi_on_open(false);
    assert!(
        !o.get_fail_if_no_udi_on_open(),
        "BlockBasedOptions::fail_if_no_udi_on_open"
    );
    let mut o = BlockBasedOptions::default();
    o.set_index_shortening(7);
    assert_eq!(
        o.get_index_shortening(),
        7,
        "BlockBasedOptions::index_shortening"
    );
    let mut o = BlockBasedOptions::default();
    o.set_initial_auto_readahead_size(4096);
    assert_eq!(
        o.get_initial_auto_readahead_size(),
        4096,
        "BlockBasedOptions::initial_auto_readahead_size"
    );
    let mut o = BlockBasedOptions::default();
    o.set_max_auto_readahead_size(4096);
    assert_eq!(
        o.get_max_auto_readahead_size(),
        4096,
        "BlockBasedOptions::max_auto_readahead_size"
    );
    let mut o = BlockBasedOptions::default();
    o.set_num_file_reads_for_auto_readahead(4096);
    assert_eq!(
        o.get_num_file_reads_for_auto_readahead(),
        4096,
        "BlockBasedOptions::num_file_reads_for_auto_readahead"
    );
    let mut o = BlockBasedOptions::default();
    o.set_prepopulate_block_cache(7);
    assert_eq!(
        o.get_prepopulate_block_cache(),
        7,
        "BlockBasedOptions::prepopulate_block_cache"
    );
    let mut o = BlockBasedOptions::default();
    o.set_read_amp_bytes_per_bit(7);
    assert_eq!(
        o.get_read_amp_bytes_per_bit(),
        7,
        "BlockBasedOptions::read_amp_bytes_per_bit"
    );
    let mut o = BlockBasedOptions::default();
    o.set_super_block_alignment_size(4096);
    assert_eq!(
        o.get_super_block_alignment_size(),
        4096,
        "BlockBasedOptions::super_block_alignment_size"
    );
    let mut o = BlockBasedOptions::default();
    o.set_super_block_alignment_space_overhead_ratio(4096);
    assert_eq!(
        o.get_super_block_alignment_space_overhead_ratio(),
        4096,
        "BlockBasedOptions::super_block_alignment_space_overhead_ratio"
    );
    let mut o = BlockBasedOptions::default();
    o.set_use_udi_as_primary_index(true);
    assert!(
        o.get_use_udi_as_primary_index(),
        "BlockBasedOptions::use_udi_as_primary_index"
    );
    let mut o = BlockBasedOptions::default();
    o.set_use_udi_as_primary_index(false);
    assert!(
        !o.get_use_udi_as_primary_index(),
        "BlockBasedOptions::use_udi_as_primary_index"
    );
    let mut o = BlockBasedOptions::default();
    o.set_verify_compression(true);
    assert!(
        o.get_verify_compression(),
        "BlockBasedOptions::verify_compression"
    );
    let mut o = BlockBasedOptions::default();
    o.set_verify_compression(false);
    assert!(
        !o.get_verify_compression(),
        "BlockBasedOptions::verify_compression"
    );
}

#[test]
fn roundtrip_compact_options() {
    let mut o = CompactOptions::default();
    o.set_blob_garbage_collection_policy(7);
    assert_eq!(
        o.get_blob_garbage_collection_policy(),
        7,
        "CompactOptions::blob_garbage_collection_policy"
    );
}

#[test]
fn roundtrip_cuckoo_table_options() {
    let mut o = CuckooTableOptions::default();
    o.set_hash_table_ratio(0.5);
    assert_eq!(
        o.get_hash_table_ratio(),
        0.5,
        "CuckooTableOptions::hash_table_ratio"
    );
}

#[test]
fn roundtrip_fifo_compact_options() {
    let mut o = FifoCompactOptions::default();
    o.set_age_for_warm(4096);
    assert_eq!(
        o.get_age_for_warm(),
        4096,
        "FifoCompactOptions::age_for_warm"
    );
    let mut o = FifoCompactOptions::default();
    o.set_allow_trivial_copy_when_change_temperature(true);
    assert!(
        o.get_allow_trivial_copy_when_change_temperature(),
        "FifoCompactOptions::allow_trivial_copy_when_change_temperature"
    );
    let mut o = FifoCompactOptions::default();
    o.set_allow_trivial_copy_when_change_temperature(false);
    assert!(
        !o.get_allow_trivial_copy_when_change_temperature(),
        "FifoCompactOptions::allow_trivial_copy_when_change_temperature"
    );
    let mut o = FifoCompactOptions::default();
    o.set_trivial_copy_buffer_size(4096);
    assert_eq!(
        o.get_trivial_copy_buffer_size(),
        4096,
        "FifoCompactOptions::trivial_copy_buffer_size"
    );
}

#[test]
fn roundtrip_flush_options() {
    let mut o = FlushOptions::default();
    o.set_allow_write_stall(true);
    assert!(o.get_allow_write_stall(), "FlushOptions::allow_write_stall");
    let mut o = FlushOptions::default();
    o.set_allow_write_stall(false);
    assert!(
        !o.get_allow_write_stall(),
        "FlushOptions::allow_write_stall"
    );
    let mut o = FlushOptions::default();
    o.set_force_atomic_flush(true);
    assert!(
        o.get_force_atomic_flush(),
        "FlushOptions::force_atomic_flush"
    );
    let mut o = FlushOptions::default();
    o.set_force_atomic_flush(false);
    assert!(
        !o.get_force_atomic_flush(),
        "FlushOptions::force_atomic_flush"
    );
    let mut o = FlushOptions::default();
    o.set_listener_wait(true);
    assert!(o.get_listener_wait(), "FlushOptions::listener_wait");
    let mut o = FlushOptions::default();
    o.set_listener_wait(false);
    assert!(!o.get_listener_wait(), "FlushOptions::listener_wait");
}

#[test]
fn roundtrip_ingest_external_file_options() {
    let mut o = IngestExternalFileOptions::default();
    o.set_allow_db_generated_files(true);
    assert!(
        o.get_allow_db_generated_files(),
        "IngestExternalFileOptions::allow_db_generated_files"
    );
    let mut o = IngestExternalFileOptions::default();
    o.set_allow_db_generated_files(false);
    assert!(
        !o.get_allow_db_generated_files(),
        "IngestExternalFileOptions::allow_db_generated_files"
    );
    let mut o = IngestExternalFileOptions::default();
    o.set_failed_move_fall_back_to_copy(true);
    assert!(
        o.get_failed_move_fall_back_to_copy(),
        "IngestExternalFileOptions::failed_move_fall_back_to_copy"
    );
    let mut o = IngestExternalFileOptions::default();
    o.set_failed_move_fall_back_to_copy(false);
    assert!(
        !o.get_failed_move_fall_back_to_copy(),
        "IngestExternalFileOptions::failed_move_fall_back_to_copy"
    );
    let mut o = IngestExternalFileOptions::default();
    o.set_file_opening_threads(7);
    assert_eq!(
        o.get_file_opening_threads(),
        7,
        "IngestExternalFileOptions::file_opening_threads"
    );
    let mut o = IngestExternalFileOptions::default();
    o.set_fill_cache(true);
    assert!(o.get_fill_cache(), "IngestExternalFileOptions::fill_cache");
    let mut o = IngestExternalFileOptions::default();
    o.set_fill_cache(false);
    assert!(!o.get_fill_cache(), "IngestExternalFileOptions::fill_cache");
    let mut o = IngestExternalFileOptions::default();
    o.set_link_files(true);
    assert!(o.get_link_files(), "IngestExternalFileOptions::link_files");
    let mut o = IngestExternalFileOptions::default();
    o.set_link_files(false);
    assert!(!o.get_link_files(), "IngestExternalFileOptions::link_files");
    let mut o = IngestExternalFileOptions::default();
    o.set_prefetch_lmax_index_and_filter_blocks(true);
    assert!(
        o.get_prefetch_lmax_index_and_filter_blocks(),
        "IngestExternalFileOptions::prefetch_lmax_index_and_filter_blocks"
    );
    let mut o = IngestExternalFileOptions::default();
    o.set_prefetch_lmax_index_and_filter_blocks(false);
    assert!(
        !o.get_prefetch_lmax_index_and_filter_blocks(),
        "IngestExternalFileOptions::prefetch_lmax_index_and_filter_blocks"
    );
    let mut o = IngestExternalFileOptions::default();
    o.set_verify_checksums_before_ingest(true);
    assert!(
        o.get_verify_checksums_before_ingest(),
        "IngestExternalFileOptions::verify_checksums_before_ingest"
    );
    let mut o = IngestExternalFileOptions::default();
    o.set_verify_checksums_before_ingest(false);
    assert!(
        !o.get_verify_checksums_before_ingest(),
        "IngestExternalFileOptions::verify_checksums_before_ingest"
    );
    let mut o = IngestExternalFileOptions::default();
    o.set_verify_checksums_readahead_size(4096);
    assert_eq!(
        o.get_verify_checksums_readahead_size(),
        4096,
        "IngestExternalFileOptions::verify_checksums_readahead_size"
    );
    let mut o = IngestExternalFileOptions::default();
    o.set_verify_file_checksum(true);
    assert!(
        o.get_verify_file_checksum(),
        "IngestExternalFileOptions::verify_file_checksum"
    );
    let mut o = IngestExternalFileOptions::default();
    o.set_verify_file_checksum(false);
    assert!(
        !o.get_verify_file_checksum(),
        "IngestExternalFileOptions::verify_file_checksum"
    );
    let mut o = IngestExternalFileOptions::default();
    o.set_write_global_seqno(true);
    assert!(
        o.get_write_global_seqno(),
        "IngestExternalFileOptions::write_global_seqno"
    );
    let mut o = IngestExternalFileOptions::default();
    o.set_write_global_seqno(false);
    assert!(
        !o.get_write_global_seqno(),
        "IngestExternalFileOptions::write_global_seqno"
    );
}

#[test]
fn roundtrip_options() {
    let mut o = Options::default();
    o.set_allow_2pc(true);
    assert!(o.get_allow_2pc(), "Options::allow_2pc");
    let mut o = Options::default();
    o.set_allow_2pc(false);
    assert!(!o.get_allow_2pc(), "Options::allow_2pc");
    let mut o = Options::default();
    o.set_allow_data_in_errors(true);
    assert!(
        o.get_allow_data_in_errors(),
        "Options::allow_data_in_errors"
    );
    let mut o = Options::default();
    o.set_allow_data_in_errors(false);
    assert!(
        !o.get_allow_data_in_errors(),
        "Options::allow_data_in_errors"
    );
    let mut o = Options::default();
    o.set_allow_fallocate(true);
    assert!(o.get_allow_fallocate(), "Options::allow_fallocate");
    let mut o = Options::default();
    o.set_allow_fallocate(false);
    assert!(!o.get_allow_fallocate(), "Options::allow_fallocate");
    let mut o = Options::default();
    o.set_async_wal_precreate(true);
    assert!(o.get_async_wal_precreate(), "Options::async_wal_precreate");
    let mut o = Options::default();
    o.set_async_wal_precreate(false);
    assert!(!o.get_async_wal_precreate(), "Options::async_wal_precreate");
    let mut o = Options::default();
    o.set_avoid_flush_during_recovery(true);
    assert!(
        o.get_avoid_flush_during_recovery(),
        "Options::avoid_flush_during_recovery"
    );
    let mut o = Options::default();
    o.set_avoid_flush_during_recovery(false);
    assert!(
        !o.get_avoid_flush_during_recovery(),
        "Options::avoid_flush_during_recovery"
    );
    let mut o = Options::default();
    o.set_avoid_flush_during_shutdown(true);
    assert!(
        o.get_avoid_flush_during_shutdown(),
        "Options::avoid_flush_during_shutdown"
    );
    let mut o = Options::default();
    o.set_avoid_flush_during_shutdown(false);
    assert!(
        !o.get_avoid_flush_during_shutdown(),
        "Options::avoid_flush_during_shutdown"
    );
    let mut o = Options::default();
    o.set_background_close_inactive_wals(true);
    assert!(
        o.get_background_close_inactive_wals(),
        "Options::background_close_inactive_wals"
    );
    let mut o = Options::default();
    o.set_background_close_inactive_wals(false);
    assert!(
        !o.get_background_close_inactive_wals(),
        "Options::background_close_inactive_wals"
    );
    let mut o = Options::default();
    o.set_best_efforts_recovery(true);
    assert!(
        o.get_best_efforts_recovery(),
        "Options::best_efforts_recovery"
    );
    let mut o = Options::default();
    o.set_best_efforts_recovery(false);
    assert!(
        !o.get_best_efforts_recovery(),
        "Options::best_efforts_recovery"
    );
    let mut o = Options::default();
    o.set_bgerror_resume_retry_interval(4096);
    assert_eq!(
        o.get_bgerror_resume_retry_interval(),
        4096,
        "Options::bgerror_resume_retry_interval"
    );
    let mut o = Options::default();
    o.set_blob_direct_write_partitions(7);
    assert_eq!(
        o.get_blob_direct_write_partitions(),
        7,
        "Options::blob_direct_write_partitions"
    );
    let mut o = Options::default();
    o.set_block_protection_bytes_per_key(3);
    assert_eq!(
        o.get_block_protection_bytes_per_key(),
        3,
        "Options::block_protection_bytes_per_key"
    );
    let mut o = Options::default();
    o.set_bottommost_file_compaction_delay(7);
    assert_eq!(
        o.get_bottommost_file_compaction_delay(),
        7,
        "Options::bottommost_file_compaction_delay"
    );
    let mut o = Options::default();
    o.set_cf_allow_ingest_behind(true);
    assert!(
        o.get_cf_allow_ingest_behind(),
        "Options::cf_allow_ingest_behind"
    );
    let mut o = Options::default();
    o.set_cf_allow_ingest_behind(false);
    assert!(
        !o.get_cf_allow_ingest_behind(),
        "Options::cf_allow_ingest_behind"
    );
    let mut o = Options::default();
    o.set_compaction_verify_record_count(true);
    assert!(
        o.get_compaction_verify_record_count(),
        "Options::compaction_verify_record_count"
    );
    let mut o = Options::default();
    o.set_compaction_verify_record_count(false);
    assert!(
        !o.get_compaction_verify_record_count(),
        "Options::compaction_verify_record_count"
    );
    let mut o = Options::default();
    o.set_default_temperature(7);
    assert_eq!(
        o.get_default_temperature(),
        7,
        "Options::default_temperature"
    );
    let mut o = Options::default();
    o.set_default_write_temperature(7);
    assert_eq!(
        o.get_default_write_temperature(),
        7,
        "Options::default_write_temperature"
    );
    let mut o = Options::default();
    o.set_delayed_write_rate(4096);
    assert_eq!(
        o.get_delayed_write_rate(),
        4096,
        "Options::delayed_write_rate"
    );
    let mut o = Options::default();
    o.set_disallow_memtable_writes(true);
    assert!(
        o.get_disallow_memtable_writes(),
        "Options::disallow_memtable_writes"
    );
    let mut o = Options::default();
    o.set_disallow_memtable_writes(false);
    assert!(
        !o.get_disallow_memtable_writes(),
        "Options::disallow_memtable_writes"
    );
    let mut o = Options::default();
    o.set_enable_blob_direct_write(true);
    assert!(
        o.get_enable_blob_direct_write(),
        "Options::enable_blob_direct_write"
    );
    let mut o = Options::default();
    o.set_enable_blob_direct_write(false);
    assert!(
        !o.get_enable_blob_direct_write(),
        "Options::enable_blob_direct_write"
    );
    let mut o = Options::default();
    o.set_enable_thread_tracking(true);
    assert!(
        o.get_enable_thread_tracking(),
        "Options::enable_thread_tracking"
    );
    let mut o = Options::default();
    o.set_enable_thread_tracking(false);
    assert!(
        !o.get_enable_thread_tracking(),
        "Options::enable_thread_tracking"
    );
    let mut o = Options::default();
    o.set_enforce_single_del_contracts(true);
    assert!(
        o.get_enforce_single_del_contracts(),
        "Options::enforce_single_del_contracts"
    );
    let mut o = Options::default();
    o.set_enforce_single_del_contracts(false);
    assert!(
        !o.get_enforce_single_del_contracts(),
        "Options::enforce_single_del_contracts"
    );
    let mut o = Options::default();
    o.set_enforce_write_buffer_manager_during_recovery(true);
    assert!(
        o.get_enforce_write_buffer_manager_during_recovery(),
        "Options::enforce_write_buffer_manager_during_recovery"
    );
    let mut o = Options::default();
    o.set_enforce_write_buffer_manager_during_recovery(false);
    assert!(
        !o.get_enforce_write_buffer_manager_during_recovery(),
        "Options::enforce_write_buffer_manager_during_recovery"
    );
    let mut o = Options::default();
    o.set_fast_sst_open(true);
    assert!(o.get_fast_sst_open(), "Options::fast_sst_open");
    let mut o = Options::default();
    o.set_fast_sst_open(false);
    assert!(!o.get_fast_sst_open(), "Options::fast_sst_open");
    let mut o = Options::default();
    o.set_flush_verify_memtable_count(true);
    assert!(
        o.get_flush_verify_memtable_count(),
        "Options::flush_verify_memtable_count"
    );
    let mut o = Options::default();
    o.set_flush_verify_memtable_count(false);
    assert!(
        !o.get_flush_verify_memtable_count(),
        "Options::flush_verify_memtable_count"
    );
    let mut o = Options::default();
    o.set_follower_catchup_retry_count(4096);
    assert_eq!(
        o.get_follower_catchup_retry_count(),
        4096,
        "Options::follower_catchup_retry_count"
    );
    let mut o = Options::default();
    o.set_follower_catchup_retry_wait_ms(4096);
    assert_eq!(
        o.get_follower_catchup_retry_wait_ms(),
        4096,
        "Options::follower_catchup_retry_wait_ms"
    );
    let mut o = Options::default();
    o.set_follower_refresh_catchup_period_ms(4096);
    assert_eq!(
        o.get_follower_refresh_catchup_period_ms(),
        4096,
        "Options::follower_refresh_catchup_period_ms"
    );
    let mut o = Options::default();
    o.set_force_consistency_checks(true);
    assert!(
        o.get_force_consistency_checks(),
        "Options::force_consistency_checks"
    );
    let mut o = Options::default();
    o.set_force_consistency_checks(false);
    assert!(
        !o.get_force_consistency_checks(),
        "Options::force_consistency_checks"
    );
    let mut o = Options::default();
    o.set_last_level_temperature(7);
    assert_eq!(
        o.get_last_level_temperature(),
        7,
        "Options::last_level_temperature"
    );
    let mut o = Options::default();
    o.set_log_readahead_size(4096);
    assert_eq!(
        o.get_log_readahead_size(),
        4096,
        "Options::log_readahead_size"
    );
    let mut o = Options::default();
    o.set_lowest_used_cache_tier(7);
    assert_eq!(
        o.get_lowest_used_cache_tier(),
        7,
        "Options::lowest_used_cache_tier"
    );
    let mut o = Options::default();
    o.set_max_bgerror_resume_count(7);
    assert_eq!(
        o.get_max_bgerror_resume_count(),
        7,
        "Options::max_bgerror_resume_count"
    );
    let mut o = Options::default();
    o.set_max_compaction_trigger_wakeup_seconds(4096);
    assert_eq!(
        o.get_max_compaction_trigger_wakeup_seconds(),
        4096,
        "Options::max_compaction_trigger_wakeup_seconds"
    );
    let mut o = Options::default();
    o.set_max_manifest_space_amp_pct(7);
    assert_eq!(
        o.get_max_manifest_space_amp_pct(),
        7,
        "Options::max_manifest_space_amp_pct"
    );
    let mut o = Options::default();
    o.set_max_write_batch_group_size_bytes(4096);
    assert_eq!(
        o.get_max_write_batch_group_size_bytes(),
        4096,
        "Options::max_write_batch_group_size_bytes"
    );
    let mut o = Options::default();
    o.set_memtable_max_range_deletions(7);
    assert_eq!(
        o.get_memtable_max_range_deletions(),
        7,
        "Options::memtable_max_range_deletions"
    );
    let mut o = Options::default();
    o.set_memtable_protection_bytes_per_key(7);
    assert_eq!(
        o.get_memtable_protection_bytes_per_key(),
        7,
        "Options::memtable_protection_bytes_per_key"
    );
    let mut o = Options::default();
    o.set_memtable_verify_per_key_checksum_on_seek(true);
    assert!(
        o.get_memtable_verify_per_key_checksum_on_seek(),
        "Options::memtable_verify_per_key_checksum_on_seek"
    );
    let mut o = Options::default();
    o.set_memtable_verify_per_key_checksum_on_seek(false);
    assert!(
        !o.get_memtable_verify_per_key_checksum_on_seek(),
        "Options::memtable_verify_per_key_checksum_on_seek"
    );
    let mut o = Options::default();
    o.set_metadata_write_temperature(7);
    assert_eq!(
        o.get_metadata_write_temperature(),
        7,
        "Options::metadata_write_temperature"
    );
    let mut o = Options::default();
    o.set_min_tombstones_for_range_conversion(7);
    assert_eq!(
        o.get_min_tombstones_for_range_conversion(),
        7,
        "Options::min_tombstones_for_range_conversion"
    );
    let mut o = Options::default();
    o.set_optimize_manifest_for_recovery(true);
    assert!(
        o.get_optimize_manifest_for_recovery(),
        "Options::optimize_manifest_for_recovery"
    );
    let mut o = Options::default();
    o.set_optimize_manifest_for_recovery(false);
    assert!(
        !o.get_optimize_manifest_for_recovery(),
        "Options::optimize_manifest_for_recovery"
    );
    let mut o = Options::default();
    o.set_paranoid_file_checks(true);
    assert!(
        o.get_paranoid_file_checks(),
        "Options::paranoid_file_checks"
    );
    let mut o = Options::default();
    o.set_paranoid_file_checks(false);
    assert!(
        !o.get_paranoid_file_checks(),
        "Options::paranoid_file_checks"
    );
    let mut o = Options::default();
    o.set_paranoid_memory_checks(true);
    assert!(
        o.get_paranoid_memory_checks(),
        "Options::paranoid_memory_checks"
    );
    let mut o = Options::default();
    o.set_paranoid_memory_checks(false);
    assert!(
        !o.get_paranoid_memory_checks(),
        "Options::paranoid_memory_checks"
    );
    let mut o = Options::default();
    o.set_persist_stats_to_disk(true);
    assert!(
        o.get_persist_stats_to_disk(),
        "Options::persist_stats_to_disk"
    );
    let mut o = Options::default();
    o.set_persist_stats_to_disk(false);
    assert!(
        !o.get_persist_stats_to_disk(),
        "Options::persist_stats_to_disk"
    );
    let mut o = Options::default();
    o.set_persist_user_defined_timestamps(true);
    assert!(
        o.get_persist_user_defined_timestamps(),
        "Options::persist_user_defined_timestamps"
    );
    let mut o = Options::default();
    o.set_persist_user_defined_timestamps(false);
    assert!(
        !o.get_persist_user_defined_timestamps(),
        "Options::persist_user_defined_timestamps"
    );
    let mut o = Options::default();
    o.set_preclude_last_level_data_seconds(4096);
    assert_eq!(
        o.get_preclude_last_level_data_seconds(),
        4096,
        "Options::preclude_last_level_data_seconds"
    );
    let mut o = Options::default();
    o.set_prefix_seek_opt_in_only(true);
    assert!(
        o.get_prefix_seek_opt_in_only(),
        "Options::prefix_seek_opt_in_only"
    );
    let mut o = Options::default();
    o.set_prefix_seek_opt_in_only(false);
    assert!(
        !o.get_prefix_seek_opt_in_only(),
        "Options::prefix_seek_opt_in_only"
    );
    let mut o = Options::default();
    o.set_preserve_internal_time_seconds(4096);
    assert_eq!(
        o.get_preserve_internal_time_seconds(),
        4096,
        "Options::preserve_internal_time_seconds"
    );
    let mut o = Options::default();
    o.set_read_io_executor_threads(7);
    assert_eq!(
        o.get_read_io_executor_threads(),
        7,
        "Options::read_io_executor_threads"
    );
    let mut o = Options::default();
    o.set_read_triggered_compaction_threshold(0.5);
    assert_eq!(
        o.get_read_triggered_compaction_threshold(),
        0.5,
        "Options::read_triggered_compaction_threshold"
    );
    let mut o = Options::default();
    o.set_reuse_manifest_on_open(true);
    assert!(
        o.get_reuse_manifest_on_open(),
        "Options::reuse_manifest_on_open"
    );
    let mut o = Options::default();
    o.set_reuse_manifest_on_open(false);
    assert!(
        !o.get_reuse_manifest_on_open(),
        "Options::reuse_manifest_on_open"
    );
    let mut o = Options::default();
    o.set_sample_for_compression(4096);
    assert_eq!(
        o.get_sample_for_compression(),
        4096,
        "Options::sample_for_compression"
    );
    let mut o = Options::default();
    o.set_stats_history_buffer_size(4096);
    assert_eq!(
        o.get_stats_history_buffer_size(),
        4096,
        "Options::stats_history_buffer_size"
    );
    let mut o = Options::default();
    o.set_strict_bytes_per_sync(true);
    assert!(
        o.get_strict_bytes_per_sync(),
        "Options::strict_bytes_per_sync"
    );
    let mut o = Options::default();
    o.set_strict_bytes_per_sync(false);
    assert!(
        !o.get_strict_bytes_per_sync(),
        "Options::strict_bytes_per_sync"
    );
    let mut o = Options::default();
    o.set_strict_max_successive_merges(true);
    assert!(
        o.get_strict_max_successive_merges(),
        "Options::strict_max_successive_merges"
    );
    let mut o = Options::default();
    o.set_strict_max_successive_merges(false);
    assert!(
        !o.get_strict_max_successive_merges(),
        "Options::strict_max_successive_merges"
    );
    let mut o = Options::default();
    o.set_target_file_size_is_upper_bound(true);
    assert!(
        o.get_target_file_size_is_upper_bound(),
        "Options::target_file_size_is_upper_bound"
    );
    let mut o = Options::default();
    o.set_target_file_size_is_upper_bound(false);
    assert!(
        !o.get_target_file_size_is_upper_bound(),
        "Options::target_file_size_is_upper_bound"
    );
    let mut o = Options::default();
    o.set_track_and_verify_wals(true);
    assert!(
        o.get_track_and_verify_wals(),
        "Options::track_and_verify_wals"
    );
    let mut o = Options::default();
    o.set_track_and_verify_wals(false);
    assert!(
        !o.get_track_and_verify_wals(),
        "Options::track_and_verify_wals"
    );
    let mut o = Options::default();
    o.set_two_write_queues(true);
    assert!(o.get_two_write_queues(), "Options::two_write_queues");
    let mut o = Options::default();
    o.set_two_write_queues(false);
    assert!(!o.get_two_write_queues(), "Options::two_write_queues");
    let mut o = Options::default();
    o.set_uncache_aggressiveness(7);
    assert_eq!(
        o.get_uncache_aggressiveness(),
        7,
        "Options::uncache_aggressiveness"
    );
    let mut o = Options::default();
    o.set_use_direct_io_for_compaction_reads(true);
    assert!(
        o.get_use_direct_io_for_compaction_reads(),
        "Options::use_direct_io_for_compaction_reads"
    );
    let mut o = Options::default();
    o.set_use_direct_io_for_compaction_reads(false);
    assert!(
        !o.get_use_direct_io_for_compaction_reads(),
        "Options::use_direct_io_for_compaction_reads"
    );
    let mut o = Options::default();
    o.set_verify_manifest_content_on_close(true);
    assert!(
        o.get_verify_manifest_content_on_close(),
        "Options::verify_manifest_content_on_close"
    );
    let mut o = Options::default();
    o.set_verify_manifest_content_on_close(false);
    assert!(
        !o.get_verify_manifest_content_on_close(),
        "Options::verify_manifest_content_on_close"
    );
    let mut o = Options::default();
    o.set_verify_output_flags(7);
    assert_eq!(
        o.get_verify_output_flags(),
        7,
        "Options::verify_output_flags"
    );
    let mut o = Options::default();
    o.set_verify_sst_unique_id_in_manifest(true);
    assert!(
        o.get_verify_sst_unique_id_in_manifest(),
        "Options::verify_sst_unique_id_in_manifest"
    );
    let mut o = Options::default();
    o.set_verify_sst_unique_id_in_manifest(false);
    assert!(
        !o.get_verify_sst_unique_id_in_manifest(),
        "Options::verify_sst_unique_id_in_manifest"
    );
    let mut o = Options::default();
    o.set_wal_write_temperature(7);
    assert_eq!(
        o.get_wal_write_temperature(),
        7,
        "Options::wal_write_temperature"
    );
    let mut o = Options::default();
    o.set_write_thread_max_yield_usec(4096);
    assert_eq!(
        o.get_write_thread_max_yield_usec(),
        4096,
        "Options::write_thread_max_yield_usec"
    );
    let mut o = Options::default();
    o.set_write_thread_slow_yield_usec(4096);
    assert_eq!(
        o.get_write_thread_slow_yield_usec(),
        4096,
        "Options::write_thread_slow_yield_usec"
    );
}

#[test]
fn roundtrip_read_options() {
    let mut o = ReadOptions::default();
    o.set_adaptive_readahead(true);
    assert!(
        o.get_adaptive_readahead(),
        "ReadOptions::adaptive_readahead"
    );
    let mut o = ReadOptions::default();
    o.set_adaptive_readahead(false);
    assert!(
        !o.get_adaptive_readahead(),
        "ReadOptions::adaptive_readahead"
    );
    let mut o = ReadOptions::default();
    o.set_allow_unprepared_value(true);
    assert!(
        o.get_allow_unprepared_value(),
        "ReadOptions::allow_unprepared_value"
    );
    let mut o = ReadOptions::default();
    o.set_allow_unprepared_value(false);
    assert!(
        !o.get_allow_unprepared_value(),
        "ReadOptions::allow_unprepared_value"
    );
    let mut o = ReadOptions::default();
    o.set_auto_prefix_mode(true);
    assert!(o.get_auto_prefix_mode(), "ReadOptions::auto_prefix_mode");
    let mut o = ReadOptions::default();
    o.set_auto_prefix_mode(false);
    assert!(!o.get_auto_prefix_mode(), "ReadOptions::auto_prefix_mode");
    let mut o = ReadOptions::default();
    o.set_auto_refresh_iterator_with_snapshot(true);
    assert!(
        o.get_auto_refresh_iterator_with_snapshot(),
        "ReadOptions::auto_refresh_iterator_with_snapshot"
    );
    let mut o = ReadOptions::default();
    o.set_auto_refresh_iterator_with_snapshot(false);
    assert!(
        !o.get_auto_refresh_iterator_with_snapshot(),
        "ReadOptions::auto_refresh_iterator_with_snapshot"
    );
    let mut o = ReadOptions::default();
    o.set_merge_operand_count_threshold(4096);
    assert_eq!(
        o.get_merge_operand_count_threshold(),
        4096,
        "ReadOptions::merge_operand_count_threshold"
    );
    let mut o = ReadOptions::default();
    o.set_value_size_soft_limit(4096);
    assert_eq!(
        o.get_value_size_soft_limit(),
        4096,
        "ReadOptions::value_size_soft_limit"
    );
}

#[test]
fn roundtrip_transaction_db_options() {
    let mut o = TransactionDBOptions::default();
    o.set_enable_udt_validation(true);
    assert!(
        o.get_enable_udt_validation(),
        "TransactionDBOptions::enable_udt_validation"
    );
    let mut o = TransactionDBOptions::default();
    o.set_enable_udt_validation(false);
    assert!(
        !o.get_enable_udt_validation(),
        "TransactionDBOptions::enable_udt_validation"
    );
    let mut o = TransactionDBOptions::default();
    o.set_max_num_deadlocks(7);
    assert_eq!(
        o.get_max_num_deadlocks(),
        7,
        "TransactionDBOptions::max_num_deadlocks"
    );
    let mut o = TransactionDBOptions::default();
    o.set_rollback_merge_operands(true);
    assert!(
        o.get_rollback_merge_operands(),
        "TransactionDBOptions::rollback_merge_operands"
    );
    let mut o = TransactionDBOptions::default();
    o.set_rollback_merge_operands(false);
    assert!(
        !o.get_rollback_merge_operands(),
        "TransactionDBOptions::rollback_merge_operands"
    );
    let mut o = TransactionDBOptions::default();
    o.set_txn_commit_bypass_memtable_threshold(7);
    assert_eq!(
        o.get_txn_commit_bypass_memtable_threshold(),
        7,
        "TransactionDBOptions::txn_commit_bypass_memtable_threshold"
    );
    let mut o = TransactionDBOptions::default();
    o.set_use_per_key_point_lock_mgr(true);
    assert!(
        o.get_use_per_key_point_lock_mgr(),
        "TransactionDBOptions::use_per_key_point_lock_mgr"
    );
    let mut o = TransactionDBOptions::default();
    o.set_use_per_key_point_lock_mgr(false);
    assert!(
        !o.get_use_per_key_point_lock_mgr(),
        "TransactionDBOptions::use_per_key_point_lock_mgr"
    );
}

#[test]
fn roundtrip_transaction_options() {
    let mut o = TransactionOptions::default();
    o.set_commit_bypass_memtable(true);
    assert!(
        o.get_commit_bypass_memtable(),
        "TransactionOptions::commit_bypass_memtable"
    );
    let mut o = TransactionOptions::default();
    o.set_commit_bypass_memtable(false);
    assert!(
        !o.get_commit_bypass_memtable(),
        "TransactionOptions::commit_bypass_memtable"
    );
    let mut o = TransactionOptions::default();
    o.set_large_txn_commit_optimize_byte_threshold(4096);
    assert_eq!(
        o.get_large_txn_commit_optimize_byte_threshold(),
        4096,
        "TransactionOptions::large_txn_commit_optimize_byte_threshold"
    );
    let mut o = TransactionOptions::default();
    o.set_large_txn_commit_optimize_threshold(7);
    assert_eq!(
        o.get_large_txn_commit_optimize_threshold(),
        7,
        "TransactionOptions::large_txn_commit_optimize_threshold"
    );
    let mut o = TransactionOptions::default();
    o.set_skip_concurrency_control(true);
    assert!(
        o.get_skip_concurrency_control(),
        "TransactionOptions::skip_concurrency_control"
    );
    let mut o = TransactionOptions::default();
    o.set_skip_concurrency_control(false);
    assert!(
        !o.get_skip_concurrency_control(),
        "TransactionOptions::skip_concurrency_control"
    );
    let mut o = TransactionOptions::default();
    o.set_use_only_the_last_commit_time_batch_for_recovery(true);
    assert!(
        o.get_use_only_the_last_commit_time_batch_for_recovery(),
        "TransactionOptions::use_only_the_last_commit_time_batch_for_recovery"
    );
    let mut o = TransactionOptions::default();
    o.set_use_only_the_last_commit_time_batch_for_recovery(false);
    assert!(
        !o.get_use_only_the_last_commit_time_batch_for_recovery(),
        "TransactionOptions::use_only_the_last_commit_time_batch_for_recovery"
    );
    let mut o = TransactionOptions::default();
    o.set_write_batch_track_timestamp_size(true);
    assert!(
        o.get_write_batch_track_timestamp_size(),
        "TransactionOptions::write_batch_track_timestamp_size"
    );
    let mut o = TransactionOptions::default();
    o.set_write_batch_track_timestamp_size(false);
    assert!(
        !o.get_write_batch_track_timestamp_size(),
        "TransactionOptions::write_batch_track_timestamp_size"
    );
}

#[test]
fn roundtrip_universal_compact_options() {
    let mut o = UniversalCompactOptions::default();
    o.set_allow_trivial_move(true);
    assert!(
        o.get_allow_trivial_move(),
        "UniversalCompactOptions::allow_trivial_move"
    );
    let mut o = UniversalCompactOptions::default();
    o.set_allow_trivial_move(false);
    assert!(
        !o.get_allow_trivial_move(),
        "UniversalCompactOptions::allow_trivial_move"
    );
    let mut o = UniversalCompactOptions::default();
    o.set_incremental(true);
    assert!(o.get_incremental(), "UniversalCompactOptions::incremental");
    let mut o = UniversalCompactOptions::default();
    o.set_incremental(false);
    assert!(!o.get_incremental(), "UniversalCompactOptions::incremental");
    let mut o = UniversalCompactOptions::default();
    o.set_max_read_amp(7);
    assert_eq!(
        o.get_max_read_amp(),
        7,
        "UniversalCompactOptions::max_read_amp"
    );
    let mut o = UniversalCompactOptions::default();
    o.set_reduce_file_locking(true);
    assert!(
        o.get_reduce_file_locking(),
        "UniversalCompactOptions::reduce_file_locking"
    );
    let mut o = UniversalCompactOptions::default();
    o.set_reduce_file_locking(false);
    assert!(
        !o.get_reduce_file_locking(),
        "UniversalCompactOptions::reduce_file_locking"
    );
}

#[test]
fn roundtrip_wait_for_compact_options() {
    let mut o = WaitForCompactOptions::default();
    o.set_wait_for_purge(true);
    assert!(
        o.get_wait_for_purge(),
        "WaitForCompactOptions::wait_for_purge"
    );
    let mut o = WaitForCompactOptions::default();
    o.set_wait_for_purge(false);
    assert!(
        !o.get_wait_for_purge(),
        "WaitForCompactOptions::wait_for_purge"
    );
}

#[test]
fn roundtrip_write_options() {
    let mut o = WriteOptions::default();
    o.set_io_activity(7);
    assert_eq!(o.get_io_activity(), 7, "WriteOptions::io_activity");
    let mut o = WriteOptions::default();
    o.set_protection_bytes_per_key(4096);
    assert_eq!(
        o.get_protection_bytes_per_key(),
        4096,
        "WriteOptions::protection_bytes_per_key"
    );
    let mut o = WriteOptions::default();
    o.set_rate_limiter_priority(7);
    assert_eq!(
        o.get_rate_limiter_priority(),
        7,
        "WriteOptions::rate_limiter_priority"
    );
}

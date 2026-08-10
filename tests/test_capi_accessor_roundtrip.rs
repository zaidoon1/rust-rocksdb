//! Round-trip coverage for the option accessors.
//!
//! Each field is written through its setter and read back through its getter,
//! twice with different values, so a getter wired to the wrong option or an
//! inverted bool cast fails here.

use rust_rocksdb::{
    BlockBasedOptions, CompactOptions, Env, FifoCompactOptions, FlushOptions,
    ImportColumnFamilyOptions, IngestExternalFileOptions, Options, ReadOptions,
    TransactionDBOptions, TransactionOptions, UniversalCompactOptions, WaitForCompactOptions,
    WriteOptions, backup::BackupEngineOptions,
};

#[test]
fn backup_engine_options_roundtrip() {
    let dir = tempfile::Builder::new()
        .prefix("backup_engine_options")
        .tempdir()
        .unwrap();
    let mut o = BackupEngineOptions::new(dir.path()).unwrap();
    {
        let o = &mut o;
        o.set_backup_log_files(true);
        assert!(
            o.get_backup_log_files(),
            "BackupEngineOptions::backup_log_files"
        );
        o.set_backup_log_files(false);
        assert!(
            !o.get_backup_log_files(),
            "BackupEngineOptions::backup_log_files"
        );
        o.set_backup_rate_limit(4096);
        assert_eq!(
            o.get_backup_rate_limit(),
            4096,
            "BackupEngineOptions::backup_rate_limit"
        );
        o.set_backup_rate_limit(1);
        assert_eq!(
            o.get_backup_rate_limit(),
            1,
            "BackupEngineOptions::backup_rate_limit"
        );
        o.set_callback_trigger_interval_size(4096);
        assert_eq!(
            o.get_callback_trigger_interval_size(),
            4096,
            "BackupEngineOptions::callback_trigger_interval_size"
        );
        o.set_callback_trigger_interval_size(1);
        assert_eq!(
            o.get_callback_trigger_interval_size(),
            1,
            "BackupEngineOptions::callback_trigger_interval_size"
        );
        o.set_destroy_old_data(true);
        assert!(
            o.get_destroy_old_data(),
            "BackupEngineOptions::destroy_old_data"
        );
        o.set_destroy_old_data(false);
        assert!(
            !o.get_destroy_old_data(),
            "BackupEngineOptions::destroy_old_data"
        );
        o.set_max_valid_backups_to_open(11);
        assert_eq!(
            o.get_max_valid_backups_to_open(),
            11,
            "BackupEngineOptions::max_valid_backups_to_open"
        );
        o.set_max_valid_backups_to_open(4);
        assert_eq!(
            o.get_max_valid_backups_to_open(),
            4,
            "BackupEngineOptions::max_valid_backups_to_open"
        );
        o.set_restore_rate_limit(4096);
        assert_eq!(
            o.get_restore_rate_limit(),
            4096,
            "BackupEngineOptions::restore_rate_limit"
        );
        o.set_restore_rate_limit(1);
        assert_eq!(
            o.get_restore_rate_limit(),
            1,
            "BackupEngineOptions::restore_rate_limit"
        );
        o.set_share_table_files(true);
        assert!(
            o.get_share_table_files(),
            "BackupEngineOptions::share_table_files"
        );
        o.set_share_table_files(false);
        assert!(
            !o.get_share_table_files(),
            "BackupEngineOptions::share_table_files"
        );
    }
}

#[test]
fn block_based_options_roundtrip() {
    let mut o = BlockBasedOptions::default();
    {
        let o = &mut o;
        o.set_block_align(true);
        assert!(o.get_block_align(), "BlockBasedOptions::block_align");
        o.set_block_align(false);
        assert!(!o.get_block_align(), "BlockBasedOptions::block_align");
        o.set_block_size_deviation(11);
        assert_eq!(
            o.get_block_size_deviation(),
            11,
            "BlockBasedOptions::block_size_deviation"
        );
        o.set_block_size_deviation(4);
        assert_eq!(
            o.get_block_size_deviation(),
            4,
            "BlockBasedOptions::block_size_deviation"
        );
        o.set_separate_key_value_in_data_block(true);
        assert!(
            o.get_separate_key_value_in_data_block(),
            "BlockBasedOptions::separate_key_value_in_data_block"
        );
        o.set_separate_key_value_in_data_block(false);
        assert!(
            !o.get_separate_key_value_in_data_block(),
            "BlockBasedOptions::separate_key_value_in_data_block"
        );
    }
}

#[test]
fn compact_options_roundtrip() {
    let mut o = CompactOptions::default();
    {
        let o = &mut o;
        o.set_allow_write_stall(true);
        assert!(
            o.get_allow_write_stall(),
            "CompactOptions::allow_write_stall"
        );
        o.set_allow_write_stall(false);
        assert!(
            !o.get_allow_write_stall(),
            "CompactOptions::allow_write_stall"
        );
        o.set_change_level(true);
        assert!(o.get_change_level(), "CompactOptions::change_level");
        o.set_change_level(false);
        assert!(!o.get_change_level(), "CompactOptions::change_level");
        o.set_exclusive_manual_compaction(true);
        assert!(
            o.get_exclusive_manual_compaction(),
            "CompactOptions::exclusive_manual_compaction"
        );
        o.set_exclusive_manual_compaction(false);
        assert!(
            !o.get_exclusive_manual_compaction(),
            "CompactOptions::exclusive_manual_compaction"
        );
        o.set_max_subcompactions(11);
        assert_eq!(
            o.get_max_subcompactions(),
            11,
            "CompactOptions::max_subcompactions"
        );
        o.set_max_subcompactions(4);
        assert_eq!(
            o.get_max_subcompactions(),
            4,
            "CompactOptions::max_subcompactions"
        );
        o.set_target_level(11);
        assert_eq!(o.get_target_level(), 11, "CompactOptions::target_level");
        o.set_target_level(4);
        assert_eq!(o.get_target_level(), 4, "CompactOptions::target_level");
        o.set_target_path_id(11);
        assert_eq!(o.get_target_path_id(), 11, "CompactOptions::target_path_id");
        o.set_target_path_id(4);
        assert_eq!(o.get_target_path_id(), 4, "CompactOptions::target_path_id");
    }
}

#[test]
fn env_roundtrip() {
    let mut o = Env::new().unwrap();
    {
        let o = &mut o;
        o.set_background_threads(11);
        assert_eq!(o.get_background_threads(), 11, "Env::background_threads");
        o.set_background_threads(4);
        assert_eq!(o.get_background_threads(), 4, "Env::background_threads");
        o.set_bottom_priority_background_threads(11);
        assert_eq!(
            o.get_bottom_priority_background_threads(),
            11,
            "Env::bottom_priority_background_threads"
        );
        o.set_bottom_priority_background_threads(4);
        assert_eq!(
            o.get_bottom_priority_background_threads(),
            4,
            "Env::bottom_priority_background_threads"
        );
        o.set_high_priority_background_threads(11);
        assert_eq!(
            o.get_high_priority_background_threads(),
            11,
            "Env::high_priority_background_threads"
        );
        o.set_high_priority_background_threads(4);
        assert_eq!(
            o.get_high_priority_background_threads(),
            4,
            "Env::high_priority_background_threads"
        );
        o.set_low_priority_background_threads(11);
        assert_eq!(
            o.get_low_priority_background_threads(),
            11,
            "Env::low_priority_background_threads"
        );
        o.set_low_priority_background_threads(4);
        assert_eq!(
            o.get_low_priority_background_threads(),
            4,
            "Env::low_priority_background_threads"
        );
    }
}

#[test]
fn fifo_compact_options_roundtrip() {
    let mut o = FifoCompactOptions::default();
    {
        let o = &mut o;
        o.set_allow_compaction(true);
        assert!(
            o.get_allow_compaction(),
            "FifoCompactOptions::allow_compaction"
        );
        o.set_allow_compaction(false);
        assert!(
            !o.get_allow_compaction(),
            "FifoCompactOptions::allow_compaction"
        );
        o.set_max_data_files_size(4096);
        assert_eq!(
            o.get_max_data_files_size(),
            4096,
            "FifoCompactOptions::max_data_files_size"
        );
        o.set_max_data_files_size(1);
        assert_eq!(
            o.get_max_data_files_size(),
            1,
            "FifoCompactOptions::max_data_files_size"
        );
        o.set_max_table_files_size(4096);
        assert_eq!(
            o.get_max_table_files_size(),
            4096,
            "FifoCompactOptions::max_table_files_size"
        );
        o.set_max_table_files_size(1);
        assert_eq!(
            o.get_max_table_files_size(),
            1,
            "FifoCompactOptions::max_table_files_size"
        );
        o.set_use_kv_ratio_compaction(true);
        assert!(
            o.get_use_kv_ratio_compaction(),
            "FifoCompactOptions::use_kv_ratio_compaction"
        );
        o.set_use_kv_ratio_compaction(false);
        assert!(
            !o.get_use_kv_ratio_compaction(),
            "FifoCompactOptions::use_kv_ratio_compaction"
        );
    }
}

#[test]
fn flush_options_roundtrip() {
    let mut o = FlushOptions::default();
    {
        let o = &mut o;
        o.set_wait(true);
        assert!(o.get_wait(), "FlushOptions::wait");
        o.set_wait(false);
        assert!(!o.get_wait(), "FlushOptions::wait");
    }
}

#[test]
fn import_column_family_options_roundtrip() {
    let mut o = ImportColumnFamilyOptions::default();
    {
        let o = &mut o;
        o.set_move_files(true);
        assert!(o.get_move_files(), "ImportColumnFamilyOptions::move_files");
        o.set_move_files(false);
        assert!(!o.get_move_files(), "ImportColumnFamilyOptions::move_files");
    }
}

#[test]
fn ingest_external_file_options_roundtrip() {
    let mut o = IngestExternalFileOptions::default();
    {
        let o = &mut o;
        o.set_fail_if_not_bottommost_level(true);
        assert!(
            o.get_fail_if_not_bottommost_level(),
            "IngestExternalFileOptions::fail_if_not_bottommost_level"
        );
        o.set_fail_if_not_bottommost_level(false);
        assert!(
            !o.get_fail_if_not_bottommost_level(),
            "IngestExternalFileOptions::fail_if_not_bottommost_level"
        );
    }
}

#[test]
fn options_roundtrip() {
    let mut o = Options::default();
    {
        let o = &mut o;
        o.set_advise_random_on_open(true);
        assert!(
            o.get_advise_random_on_open(),
            "Options::advise_random_on_open"
        );
        o.set_advise_random_on_open(false);
        assert!(
            !o.get_advise_random_on_open(),
            "Options::advise_random_on_open"
        );
        o.set_allow_concurrent_memtable_write(true);
        assert!(
            o.get_allow_concurrent_memtable_write(),
            "Options::allow_concurrent_memtable_write"
        );
        o.set_allow_concurrent_memtable_write(false);
        assert!(
            !o.get_allow_concurrent_memtable_write(),
            "Options::allow_concurrent_memtable_write"
        );
        o.set_allow_ingest_behind(true);
        assert!(o.get_allow_ingest_behind(), "Options::allow_ingest_behind");
        o.set_allow_ingest_behind(false);
        assert!(!o.get_allow_ingest_behind(), "Options::allow_ingest_behind");
        o.set_allow_mmap_reads(true);
        assert!(o.get_allow_mmap_reads(), "Options::allow_mmap_reads");
        o.set_allow_mmap_reads(false);
        assert!(!o.get_allow_mmap_reads(), "Options::allow_mmap_reads");
        o.set_allow_mmap_writes(true);
        assert!(o.get_allow_mmap_writes(), "Options::allow_mmap_writes");
        o.set_allow_mmap_writes(false);
        assert!(!o.get_allow_mmap_writes(), "Options::allow_mmap_writes");
        o.set_arena_block_size(8192);
        assert_eq!(o.get_arena_block_size(), 8192, "Options::arena_block_size");
        o.set_arena_block_size(2);
        assert_eq!(o.get_arena_block_size(), 2, "Options::arena_block_size");
        o.set_atomic_flush(true);
        assert!(o.get_atomic_flush(), "Options::atomic_flush");
        o.set_atomic_flush(false);
        assert!(!o.get_atomic_flush(), "Options::atomic_flush");
        o.set_avoid_unnecessary_blocking_io(true);
        assert!(
            o.get_avoid_unnecessary_blocking_io(),
            "Options::avoid_unnecessary_blocking_io"
        );
        o.set_avoid_unnecessary_blocking_io(false);
        assert!(
            !o.get_avoid_unnecessary_blocking_io(),
            "Options::avoid_unnecessary_blocking_io"
        );
        o.set_blob_compaction_readahead_size(4096);
        assert_eq!(
            o.get_blob_compaction_readahead_size(),
            4096,
            "Options::blob_compaction_readahead_size"
        );
        o.set_blob_compaction_readahead_size(1);
        assert_eq!(
            o.get_blob_compaction_readahead_size(),
            1,
            "Options::blob_compaction_readahead_size"
        );
        o.set_blob_file_size(4096);
        assert_eq!(o.get_blob_file_size(), 4096, "Options::blob_file_size");
        o.set_blob_file_size(1);
        assert_eq!(o.get_blob_file_size(), 1, "Options::blob_file_size");
        o.set_blob_file_starting_level(11);
        assert_eq!(
            o.get_blob_file_starting_level(),
            11,
            "Options::blob_file_starting_level"
        );
        o.set_blob_file_starting_level(4);
        assert_eq!(
            o.get_blob_file_starting_level(),
            4,
            "Options::blob_file_starting_level"
        );
        o.set_bloom_locality(17);
        assert_eq!(o.get_bloom_locality(), 17, "Options::bloom_locality");
        o.set_bloom_locality(3);
        assert_eq!(o.get_bloom_locality(), 3, "Options::bloom_locality");
        o.set_bytes_per_sync(4096);
        assert_eq!(o.get_bytes_per_sync(), 4096, "Options::bytes_per_sync");
        o.set_bytes_per_sync(1);
        assert_eq!(o.get_bytes_per_sync(), 1, "Options::bytes_per_sync");
        o.set_compaction_readahead_size(8192);
        assert_eq!(
            o.get_compaction_readahead_size(),
            8192,
            "Options::compaction_readahead_size"
        );
        o.set_compaction_readahead_size(2);
        assert_eq!(
            o.get_compaction_readahead_size(),
            2,
            "Options::compaction_readahead_size"
        );
        o.set_compression_options_max_dict_buffer_bytes(4096);
        assert_eq!(
            o.get_compression_options_max_dict_buffer_bytes(),
            4096,
            "Options::compression_options_max_dict_buffer_bytes"
        );
        o.set_compression_options_max_dict_buffer_bytes(1);
        assert_eq!(
            o.get_compression_options_max_dict_buffer_bytes(),
            1,
            "Options::compression_options_max_dict_buffer_bytes"
        );
        o.set_compression_options_use_zstd_dict_trainer(true);
        assert!(
            o.get_compression_options_use_zstd_dict_trainer(),
            "Options::compression_options_use_zstd_dict_trainer"
        );
        o.set_compression_options_use_zstd_dict_trainer(false);
        assert!(
            !o.get_compression_options_use_zstd_dict_trainer(),
            "Options::compression_options_use_zstd_dict_trainer"
        );
        o.create_if_missing(true);
        assert!(o.get_create_if_missing(), "Options::create_if_missing");
        o.create_if_missing(false);
        assert!(!o.get_create_if_missing(), "Options::create_if_missing");
        o.create_missing_column_families(true);
        assert!(
            o.get_create_missing_column_families(),
            "Options::create_missing_column_families"
        );
        o.create_missing_column_families(false);
        assert!(
            !o.get_create_missing_column_families(),
            "Options::create_missing_column_families"
        );
        o.set_db_write_buffer_size(8192);
        assert_eq!(
            o.get_db_write_buffer_size(),
            8192,
            "Options::db_write_buffer_size"
        );
        o.set_db_write_buffer_size(2);
        assert_eq!(
            o.get_db_write_buffer_size(),
            2,
            "Options::db_write_buffer_size"
        );
        o.set_delete_obsolete_files_period_micros(4096);
        assert_eq!(
            o.get_delete_obsolete_files_period_micros(),
            4096,
            "Options::delete_obsolete_files_period_micros"
        );
        o.set_delete_obsolete_files_period_micros(1);
        assert_eq!(
            o.get_delete_obsolete_files_period_micros(),
            1,
            "Options::delete_obsolete_files_period_micros"
        );
        o.set_disable_auto_compactions(true);
        assert!(
            o.get_disable_auto_compactions(),
            "Options::disable_auto_compactions"
        );
        o.set_disable_auto_compactions(false);
        assert!(
            !o.get_disable_auto_compactions(),
            "Options::disable_auto_compactions"
        );
        o.set_enable_blob_files(true);
        assert!(o.get_enable_blob_files(), "Options::enable_blob_files");
        o.set_enable_blob_files(false);
        assert!(!o.get_enable_blob_files(), "Options::enable_blob_files");
        o.set_enable_blob_gc(true);
        assert!(o.get_enable_blob_gc(), "Options::enable_blob_gc");
        o.set_enable_blob_gc(false);
        assert!(!o.get_enable_blob_gc(), "Options::enable_blob_gc");
        o.set_enable_pipelined_write(true);
        assert!(
            o.get_enable_pipelined_write(),
            "Options::enable_pipelined_write"
        );
        o.set_enable_pipelined_write(false);
        assert!(
            !o.get_enable_pipelined_write(),
            "Options::enable_pipelined_write"
        );
        o.set_enable_write_thread_adaptive_yield(true);
        assert!(
            o.get_enable_write_thread_adaptive_yield(),
            "Options::enable_write_thread_adaptive_yield"
        );
        o.set_enable_write_thread_adaptive_yield(false);
        assert!(
            !o.get_enable_write_thread_adaptive_yield(),
            "Options::enable_write_thread_adaptive_yield"
        );
        o.set_error_if_exists(true);
        assert!(o.get_error_if_exists(), "Options::error_if_exists");
        o.set_error_if_exists(false);
        assert!(!o.get_error_if_exists(), "Options::error_if_exists");
        o.set_experimental_mempurge_threshold(0.75);
        assert_eq!(
            o.get_experimental_mempurge_threshold(),
            0.75,
            "Options::experimental_mempurge_threshold"
        );
        o.set_experimental_mempurge_threshold(0.25);
        assert_eq!(
            o.get_experimental_mempurge_threshold(),
            0.25,
            "Options::experimental_mempurge_threshold"
        );
        o.set_hard_pending_compaction_bytes_limit(8192);
        assert_eq!(
            o.get_hard_pending_compaction_bytes_limit(),
            8192,
            "Options::hard_pending_compaction_bytes_limit"
        );
        o.set_hard_pending_compaction_bytes_limit(2);
        assert_eq!(
            o.get_hard_pending_compaction_bytes_limit(),
            2,
            "Options::hard_pending_compaction_bytes_limit"
        );
        o.set_inplace_update_support(true);
        assert!(
            o.get_inplace_update_support(),
            "Options::inplace_update_support"
        );
        o.set_inplace_update_support(false);
        assert!(
            !o.get_inplace_update_support(),
            "Options::inplace_update_support"
        );
        o.set_is_fd_close_on_exec(true);
        assert!(o.get_is_fd_close_on_exec(), "Options::is_fd_close_on_exec");
        o.set_is_fd_close_on_exec(false);
        assert!(!o.get_is_fd_close_on_exec(), "Options::is_fd_close_on_exec");
        o.set_keep_log_file_num(8192);
        assert_eq!(
            o.get_keep_log_file_num(),
            8192,
            "Options::keep_log_file_num"
        );
        o.set_keep_log_file_num(2);
        assert_eq!(o.get_keep_log_file_num(), 2, "Options::keep_log_file_num");
        o.set_level_compaction_dynamic_level_bytes(true);
        assert!(
            o.get_level_compaction_dynamic_level_bytes(),
            "Options::level_compaction_dynamic_level_bytes"
        );
        o.set_level_compaction_dynamic_level_bytes(false);
        assert!(
            !o.get_level_compaction_dynamic_level_bytes(),
            "Options::level_compaction_dynamic_level_bytes"
        );
        o.set_log_file_time_to_roll(8192);
        assert_eq!(
            o.get_log_file_time_to_roll(),
            8192,
            "Options::log_file_time_to_roll"
        );
        o.set_log_file_time_to_roll(2);
        assert_eq!(
            o.get_log_file_time_to_roll(),
            2,
            "Options::log_file_time_to_roll"
        );
        o.set_manifest_preallocation_size(8192);
        assert_eq!(
            o.get_manifest_preallocation_size(),
            8192,
            "Options::manifest_preallocation_size"
        );
        o.set_manifest_preallocation_size(2);
        assert_eq!(
            o.get_manifest_preallocation_size(),
            2,
            "Options::manifest_preallocation_size"
        );
        o.set_manual_wal_flush(true);
        assert!(o.get_manual_wal_flush(), "Options::manual_wal_flush");
        o.set_manual_wal_flush(false);
        assert!(!o.get_manual_wal_flush(), "Options::manual_wal_flush");
        o.set_max_background_jobs(11);
        assert_eq!(
            o.get_max_background_jobs(),
            11,
            "Options::max_background_jobs"
        );
        o.set_max_background_jobs(4);
        assert_eq!(
            o.get_max_background_jobs(),
            4,
            "Options::max_background_jobs"
        );
        o.set_max_bytes_for_level_base(4096);
        assert_eq!(
            o.get_max_bytes_for_level_base(),
            4096,
            "Options::max_bytes_for_level_base"
        );
        o.set_max_bytes_for_level_base(1);
        assert_eq!(
            o.get_max_bytes_for_level_base(),
            1,
            "Options::max_bytes_for_level_base"
        );
        o.set_max_bytes_for_level_multiplier(0.75);
        assert_eq!(
            o.get_max_bytes_for_level_multiplier(),
            0.75,
            "Options::max_bytes_for_level_multiplier"
        );
        o.set_max_bytes_for_level_multiplier(0.25);
        assert_eq!(
            o.get_max_bytes_for_level_multiplier(),
            0.25,
            "Options::max_bytes_for_level_multiplier"
        );
        o.set_max_compaction_bytes(4096);
        assert_eq!(
            o.get_max_compaction_bytes(),
            4096,
            "Options::max_compaction_bytes"
        );
        o.set_max_compaction_bytes(1);
        assert_eq!(
            o.get_max_compaction_bytes(),
            1,
            "Options::max_compaction_bytes"
        );
        o.set_max_file_opening_threads(11);
        assert_eq!(
            o.get_max_file_opening_threads(),
            11,
            "Options::max_file_opening_threads"
        );
        o.set_max_file_opening_threads(4);
        assert_eq!(
            o.get_max_file_opening_threads(),
            4,
            "Options::max_file_opening_threads"
        );
        o.set_max_log_file_size(8192);
        assert_eq!(
            o.get_max_log_file_size(),
            8192,
            "Options::max_log_file_size"
        );
        o.set_max_log_file_size(2);
        assert_eq!(o.get_max_log_file_size(), 2, "Options::max_log_file_size");
        o.set_max_manifest_file_size(8192);
        assert_eq!(
            o.get_max_manifest_file_size(),
            8192,
            "Options::max_manifest_file_size"
        );
        o.set_max_manifest_file_size(2);
        assert_eq!(
            o.get_max_manifest_file_size(),
            2,
            "Options::max_manifest_file_size"
        );
        o.set_max_open_files(11);
        assert_eq!(o.get_max_open_files(), 11, "Options::max_open_files");
        o.set_max_open_files(4);
        assert_eq!(o.get_max_open_files(), 4, "Options::max_open_files");
        o.set_max_sequential_skip_in_iterations(4096);
        assert_eq!(
            o.get_max_sequential_skip_in_iterations(),
            4096,
            "Options::max_sequential_skip_in_iterations"
        );
        o.set_max_sequential_skip_in_iterations(1);
        assert_eq!(
            o.get_max_sequential_skip_in_iterations(),
            1,
            "Options::max_sequential_skip_in_iterations"
        );
        o.set_max_subcompactions(17);
        assert_eq!(
            o.get_max_subcompactions(),
            17,
            "Options::max_subcompactions"
        );
        o.set_max_subcompactions(3);
        assert_eq!(o.get_max_subcompactions(), 3, "Options::max_subcompactions");
        o.set_max_successive_merges(8192);
        assert_eq!(
            o.get_max_successive_merges(),
            8192,
            "Options::max_successive_merges"
        );
        o.set_max_successive_merges(2);
        assert_eq!(
            o.get_max_successive_merges(),
            2,
            "Options::max_successive_merges"
        );
        o.set_max_total_wal_size(4096);
        assert_eq!(
            o.get_max_total_wal_size(),
            4096,
            "Options::max_total_wal_size"
        );
        o.set_max_total_wal_size(1);
        assert_eq!(o.get_max_total_wal_size(), 1, "Options::max_total_wal_size");
        o.set_max_write_buffer_number(11);
        assert_eq!(
            o.get_max_write_buffer_number(),
            11,
            "Options::max_write_buffer_number"
        );
        o.set_max_write_buffer_number(4);
        assert_eq!(
            o.get_max_write_buffer_number(),
            4,
            "Options::max_write_buffer_number"
        );
        o.set_max_write_buffer_size_to_maintain(1024);
        assert_eq!(
            o.get_max_write_buffer_size_to_maintain(),
            1024,
            "Options::max_write_buffer_size_to_maintain"
        );
        o.set_max_write_buffer_size_to_maintain(7);
        assert_eq!(
            o.get_max_write_buffer_size_to_maintain(),
            7,
            "Options::max_write_buffer_size_to_maintain"
        );
        o.set_memtable_avg_op_scan_flush_trigger(17);
        assert_eq!(
            o.get_memtable_avg_op_scan_flush_trigger(),
            17,
            "Options::memtable_avg_op_scan_flush_trigger"
        );
        o.set_memtable_avg_op_scan_flush_trigger(3);
        assert_eq!(
            o.get_memtable_avg_op_scan_flush_trigger(),
            3,
            "Options::memtable_avg_op_scan_flush_trigger"
        );
        o.set_memtable_op_scan_flush_trigger(17);
        assert_eq!(
            o.get_memtable_op_scan_flush_trigger(),
            17,
            "Options::memtable_op_scan_flush_trigger"
        );
        o.set_memtable_op_scan_flush_trigger(3);
        assert_eq!(
            o.get_memtable_op_scan_flush_trigger(),
            3,
            "Options::memtable_op_scan_flush_trigger"
        );
        o.set_min_blob_size(4096);
        assert_eq!(o.get_min_blob_size(), 4096, "Options::min_blob_size");
        o.set_min_blob_size(1);
        assert_eq!(o.get_min_blob_size(), 1, "Options::min_blob_size");
        o.set_min_write_buffer_number_to_merge(11);
        assert_eq!(
            o.get_min_write_buffer_number_to_merge(),
            11,
            "Options::min_write_buffer_number_to_merge"
        );
        o.set_min_write_buffer_number_to_merge(4);
        assert_eq!(
            o.get_min_write_buffer_number_to_merge(),
            4,
            "Options::min_write_buffer_number_to_merge"
        );
        o.set_num_levels(11);
        assert_eq!(o.get_num_levels(), 11, "Options::num_levels");
        o.set_num_levels(4);
        assert_eq!(o.get_num_levels(), 4, "Options::num_levels");
        o.set_optimize_filters_for_hits(true);
        assert!(
            o.get_optimize_filters_for_hits(),
            "Options::optimize_filters_for_hits"
        );
        o.set_optimize_filters_for_hits(false);
        assert!(
            !o.get_optimize_filters_for_hits(),
            "Options::optimize_filters_for_hits"
        );
        o.set_paranoid_checks(true);
        assert!(o.get_paranoid_checks(), "Options::paranoid_checks");
        o.set_paranoid_checks(false);
        assert!(!o.get_paranoid_checks(), "Options::paranoid_checks");
        o.set_periodic_compaction_seconds(4096);
        assert_eq!(
            o.get_periodic_compaction_seconds(),
            4096,
            "Options::periodic_compaction_seconds"
        );
        o.set_periodic_compaction_seconds(1);
        assert_eq!(
            o.get_periodic_compaction_seconds(),
            1,
            "Options::periodic_compaction_seconds"
        );
        o.set_recycle_log_file_num(8192);
        assert_eq!(
            o.get_recycle_log_file_num(),
            8192,
            "Options::recycle_log_file_num"
        );
        o.set_recycle_log_file_num(2);
        assert_eq!(
            o.get_recycle_log_file_num(),
            2,
            "Options::recycle_log_file_num"
        );
        o.set_report_bg_io_stats(true);
        assert!(o.get_report_bg_io_stats(), "Options::report_bg_io_stats");
        o.set_report_bg_io_stats(false);
        assert!(!o.get_report_bg_io_stats(), "Options::report_bg_io_stats");
        o.set_skip_stats_update_on_db_open(true);
        assert!(
            o.get_skip_stats_update_on_db_open(),
            "Options::skip_stats_update_on_db_open"
        );
        o.set_skip_stats_update_on_db_open(false);
        assert!(
            !o.get_skip_stats_update_on_db_open(),
            "Options::skip_stats_update_on_db_open"
        );
        o.set_soft_pending_compaction_bytes_limit(8192);
        assert_eq!(
            o.get_soft_pending_compaction_bytes_limit(),
            8192,
            "Options::soft_pending_compaction_bytes_limit"
        );
        o.set_soft_pending_compaction_bytes_limit(2);
        assert_eq!(
            o.get_soft_pending_compaction_bytes_limit(),
            2,
            "Options::soft_pending_compaction_bytes_limit"
        );
        o.set_target_file_size_base(4096);
        assert_eq!(
            o.get_target_file_size_base(),
            4096,
            "Options::target_file_size_base"
        );
        o.set_target_file_size_base(1);
        assert_eq!(
            o.get_target_file_size_base(),
            1,
            "Options::target_file_size_base"
        );
        o.set_ttl(4096);
        assert_eq!(o.get_ttl(), 4096, "Options::ttl");
        o.set_ttl(1);
        assert_eq!(o.get_ttl(), 1, "Options::ttl");
        o.set_unordered_write(true);
        assert!(o.get_unordered_write(), "Options::unordered_write");
        o.set_unordered_write(false);
        assert!(!o.get_unordered_write(), "Options::unordered_write");
        o.set_use_adaptive_mutex(true);
        assert!(o.get_use_adaptive_mutex(), "Options::use_adaptive_mutex");
        o.set_use_adaptive_mutex(false);
        assert!(!o.get_use_adaptive_mutex(), "Options::use_adaptive_mutex");
        o.set_use_direct_io_for_flush_and_compaction(true);
        assert!(
            o.get_use_direct_io_for_flush_and_compaction(),
            "Options::use_direct_io_for_flush_and_compaction"
        );
        o.set_use_direct_io_for_flush_and_compaction(false);
        assert!(
            !o.get_use_direct_io_for_flush_and_compaction(),
            "Options::use_direct_io_for_flush_and_compaction"
        );
        o.set_use_direct_reads(true);
        assert!(o.get_use_direct_reads(), "Options::use_direct_reads");
        o.set_use_direct_reads(false);
        assert!(!o.get_use_direct_reads(), "Options::use_direct_reads");
        o.set_wal_bytes_per_sync(4096);
        assert_eq!(
            o.get_wal_bytes_per_sync(),
            4096,
            "Options::wal_bytes_per_sync"
        );
        o.set_wal_bytes_per_sync(1);
        assert_eq!(o.get_wal_bytes_per_sync(), 1, "Options::wal_bytes_per_sync");
        o.set_wal_size_limit_mb(4096);
        assert_eq!(
            o.get_wal_size_limit_mb(),
            4096,
            "Options::wal_size_limit_mb"
        );
        o.set_wal_size_limit_mb(1);
        assert_eq!(o.get_wal_size_limit_mb(), 1, "Options::wal_size_limit_mb");
        o.set_wal_ttl_seconds(4096);
        assert_eq!(o.get_wal_ttl_seconds(), 4096, "Options::wal_ttl_seconds");
        o.set_wal_ttl_seconds(1);
        assert_eq!(o.get_wal_ttl_seconds(), 1, "Options::wal_ttl_seconds");
        o.set_writable_file_max_buffer_size(4096);
        assert_eq!(
            o.get_writable_file_max_buffer_size(),
            4096,
            "Options::writable_file_max_buffer_size"
        );
        o.set_writable_file_max_buffer_size(1);
        assert_eq!(
            o.get_writable_file_max_buffer_size(),
            1,
            "Options::writable_file_max_buffer_size"
        );
        o.set_write_buffer_size(8192);
        assert_eq!(
            o.get_write_buffer_size(),
            8192,
            "Options::write_buffer_size"
        );
        o.set_write_buffer_size(2);
        assert_eq!(o.get_write_buffer_size(), 2, "Options::write_buffer_size");
        o.set_write_identity_file(true);
        assert!(o.get_write_identity_file(), "Options::write_identity_file");
        o.set_write_identity_file(false);
        assert!(!o.get_write_identity_file(), "Options::write_identity_file");
    }
}

#[test]
fn read_options_roundtrip() {
    let mut o = ReadOptions::default();
    {
        let o = &mut o;
        o.set_async_io(true);
        assert!(o.get_async_io(), "ReadOptions::async_io");
        o.set_async_io(false);
        assert!(!o.get_async_io(), "ReadOptions::async_io");
        o.set_background_purge_on_iterator_cleanup(true);
        assert!(
            o.get_background_purge_on_iterator_cleanup(),
            "ReadOptions::background_purge_on_iterator_cleanup"
        );
        o.set_background_purge_on_iterator_cleanup(false);
        assert!(
            !o.get_background_purge_on_iterator_cleanup(),
            "ReadOptions::background_purge_on_iterator_cleanup"
        );
        o.set_deadline(4096);
        assert_eq!(o.get_deadline(), 4096, "ReadOptions::deadline");
        o.set_deadline(1);
        assert_eq!(o.get_deadline(), 1, "ReadOptions::deadline");
        o.fill_cache(true);
        assert!(o.get_fill_cache(), "ReadOptions::fill_cache");
        o.fill_cache(false);
        assert!(!o.get_fill_cache(), "ReadOptions::fill_cache");
        o.set_io_timeout(4096);
        assert_eq!(o.get_io_timeout(), 4096, "ReadOptions::io_timeout");
        o.set_io_timeout(1);
        assert_eq!(o.get_io_timeout(), 1, "ReadOptions::io_timeout");
        o.set_max_skippable_internal_keys(4096);
        assert_eq!(
            o.get_max_skippable_internal_keys(),
            4096,
            "ReadOptions::max_skippable_internal_keys"
        );
        o.set_max_skippable_internal_keys(1);
        assert_eq!(
            o.get_max_skippable_internal_keys(),
            1,
            "ReadOptions::max_skippable_internal_keys"
        );
        o.set_pin_data(true);
        assert!(o.get_pin_data(), "ReadOptions::pin_data");
        o.set_pin_data(false);
        assert!(!o.get_pin_data(), "ReadOptions::pin_data");
        o.set_prefix_same_as_start(true);
        assert!(
            o.get_prefix_same_as_start(),
            "ReadOptions::prefix_same_as_start"
        );
        o.set_prefix_same_as_start(false);
        assert!(
            !o.get_prefix_same_as_start(),
            "ReadOptions::prefix_same_as_start"
        );
        o.set_readahead_size(8192);
        assert_eq!(o.get_readahead_size(), 8192, "ReadOptions::readahead_size");
        o.set_readahead_size(2);
        assert_eq!(o.get_readahead_size(), 2, "ReadOptions::readahead_size");
        o.set_tailing(true);
        assert!(o.get_tailing(), "ReadOptions::tailing");
        o.set_tailing(false);
        assert!(!o.get_tailing(), "ReadOptions::tailing");
        o.set_total_order_seek(true);
        assert!(o.get_total_order_seek(), "ReadOptions::total_order_seek");
        o.set_total_order_seek(false);
        assert!(!o.get_total_order_seek(), "ReadOptions::total_order_seek");
        o.set_verify_checksums(true);
        assert!(o.get_verify_checksums(), "ReadOptions::verify_checksums");
        o.set_verify_checksums(false);
        assert!(!o.get_verify_checksums(), "ReadOptions::verify_checksums");
    }
}

#[test]
fn transaction_d_b_options_roundtrip() {
    let mut o = TransactionDBOptions::default();
    {
        let o = &mut o;
        o.set_default_lock_timeout(1024);
        assert_eq!(
            o.get_default_lock_timeout(),
            1024,
            "TransactionDBOptions::default_lock_timeout"
        );
        o.set_default_lock_timeout(7);
        assert_eq!(
            o.get_default_lock_timeout(),
            7,
            "TransactionDBOptions::default_lock_timeout"
        );
        o.set_default_write_batch_flush_threshold(1024);
        assert_eq!(
            o.get_default_write_batch_flush_threshold(),
            1024,
            "TransactionDBOptions::default_write_batch_flush_threshold"
        );
        o.set_default_write_batch_flush_threshold(7);
        assert_eq!(
            o.get_default_write_batch_flush_threshold(),
            7,
            "TransactionDBOptions::default_write_batch_flush_threshold"
        );
        o.set_max_num_locks(1024);
        assert_eq!(
            o.get_max_num_locks(),
            1024,
            "TransactionDBOptions::max_num_locks"
        );
        o.set_max_num_locks(7);
        assert_eq!(
            o.get_max_num_locks(),
            7,
            "TransactionDBOptions::max_num_locks"
        );
    }
}

#[test]
fn transaction_options_roundtrip() {
    let mut o = TransactionOptions::default();
    {
        let o = &mut o;
        o.set_deadlock_detect_depth(1024);
        assert_eq!(
            o.get_deadlock_detect_depth(),
            1024,
            "TransactionOptions::deadlock_detect_depth"
        );
        o.set_deadlock_detect_depth(7);
        assert_eq!(
            o.get_deadlock_detect_depth(),
            7,
            "TransactionOptions::deadlock_detect_depth"
        );
        o.set_deadlock_timeout_us(1024);
        assert_eq!(
            o.get_deadlock_timeout_us(),
            1024,
            "TransactionOptions::deadlock_timeout_us"
        );
        o.set_deadlock_timeout_us(7);
        assert_eq!(
            o.get_deadlock_timeout_us(),
            7,
            "TransactionOptions::deadlock_timeout_us"
        );
        o.set_expiration(1024);
        assert_eq!(o.get_expiration(), 1024, "TransactionOptions::expiration");
        o.set_expiration(7);
        assert_eq!(o.get_expiration(), 7, "TransactionOptions::expiration");
        o.set_lock_timeout(1024);
        assert_eq!(
            o.get_lock_timeout(),
            1024,
            "TransactionOptions::lock_timeout"
        );
        o.set_lock_timeout(7);
        assert_eq!(o.get_lock_timeout(), 7, "TransactionOptions::lock_timeout");
        o.set_write_batch_flush_threshold(1024);
        assert_eq!(
            o.get_write_batch_flush_threshold(),
            1024,
            "TransactionOptions::write_batch_flush_threshold"
        );
        o.set_write_batch_flush_threshold(7);
        assert_eq!(
            o.get_write_batch_flush_threshold(),
            7,
            "TransactionOptions::write_batch_flush_threshold"
        );
    }
}

#[test]
fn universal_compact_options_roundtrip() {
    let mut o = UniversalCompactOptions::default();
    {
        let o = &mut o;
        o.set_compression_size_percent(11);
        assert_eq!(
            o.get_compression_size_percent(),
            11,
            "UniversalCompactOptions::compression_size_percent"
        );
        o.set_compression_size_percent(4);
        assert_eq!(
            o.get_compression_size_percent(),
            4,
            "UniversalCompactOptions::compression_size_percent"
        );
        o.set_max_merge_width(11);
        assert_eq!(
            o.get_max_merge_width(),
            11,
            "UniversalCompactOptions::max_merge_width"
        );
        o.set_max_merge_width(4);
        assert_eq!(
            o.get_max_merge_width(),
            4,
            "UniversalCompactOptions::max_merge_width"
        );
        o.set_max_size_amplification_percent(11);
        assert_eq!(
            o.get_max_size_amplification_percent(),
            11,
            "UniversalCompactOptions::max_size_amplification_percent"
        );
        o.set_max_size_amplification_percent(4);
        assert_eq!(
            o.get_max_size_amplification_percent(),
            4,
            "UniversalCompactOptions::max_size_amplification_percent"
        );
        o.set_min_merge_width(11);
        assert_eq!(
            o.get_min_merge_width(),
            11,
            "UniversalCompactOptions::min_merge_width"
        );
        o.set_min_merge_width(4);
        assert_eq!(
            o.get_min_merge_width(),
            4,
            "UniversalCompactOptions::min_merge_width"
        );
        o.set_size_ratio(11);
        assert_eq!(
            o.get_size_ratio(),
            11,
            "UniversalCompactOptions::size_ratio"
        );
        o.set_size_ratio(4);
        assert_eq!(o.get_size_ratio(), 4, "UniversalCompactOptions::size_ratio");
    }
}

#[test]
fn wait_for_compact_options_roundtrip() {
    let mut o = WaitForCompactOptions::default();
    {
        let o = &mut o;
        o.set_abort_on_pause(true);
        assert!(
            o.get_abort_on_pause(),
            "WaitForCompactOptions::abort_on_pause"
        );
        o.set_abort_on_pause(false);
        assert!(
            !o.get_abort_on_pause(),
            "WaitForCompactOptions::abort_on_pause"
        );
        o.set_close_db(true);
        assert!(o.get_close_db(), "WaitForCompactOptions::close_db");
        o.set_close_db(false);
        assert!(!o.get_close_db(), "WaitForCompactOptions::close_db");
        o.set_flush(true);
        assert!(o.get_flush(), "WaitForCompactOptions::flush");
        o.set_flush(false);
        assert!(!o.get_flush(), "WaitForCompactOptions::flush");
        o.set_timeout(4096);
        assert_eq!(o.get_timeout(), 4096, "WaitForCompactOptions::timeout");
        o.set_timeout(1);
        assert_eq!(o.get_timeout(), 1, "WaitForCompactOptions::timeout");
    }
}

#[test]
fn write_options_roundtrip() {
    let mut o = WriteOptions::default();
    {
        let o = &mut o;
        o.disable_wal(true);
        assert!(o.get_disable_wal(), "WriteOptions::disable_wal");
        o.disable_wal(false);
        assert!(!o.get_disable_wal(), "WriteOptions::disable_wal");
        o.set_ignore_missing_column_families(true);
        assert!(
            o.get_ignore_missing_column_families(),
            "WriteOptions::ignore_missing_column_families"
        );
        o.set_ignore_missing_column_families(false);
        assert!(
            !o.get_ignore_missing_column_families(),
            "WriteOptions::ignore_missing_column_families"
        );
        o.set_low_pri(true);
        assert!(o.get_low_pri(), "WriteOptions::low_pri");
        o.set_low_pri(false);
        assert!(!o.get_low_pri(), "WriteOptions::low_pri");
        o.set_memtable_insert_hint_per_batch(true);
        assert!(
            o.get_memtable_insert_hint_per_batch(),
            "WriteOptions::memtable_insert_hint_per_batch"
        );
        o.set_memtable_insert_hint_per_batch(false);
        assert!(
            !o.get_memtable_insert_hint_per_batch(),
            "WriteOptions::memtable_insert_hint_per_batch"
        );
        o.set_no_slowdown(true);
        assert!(o.get_no_slowdown(), "WriteOptions::no_slowdown");
        o.set_no_slowdown(false);
        assert!(!o.get_no_slowdown(), "WriteOptions::no_slowdown");
        o.set_sync(true);
        assert!(o.get_sync(), "WriteOptions::sync");
        o.set_sync(false);
        assert!(!o.get_sync(), "WriteOptions::sync");
    }
}

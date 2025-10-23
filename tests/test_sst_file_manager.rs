mod util;

use rust_rocksdb::sst_file_manager::SstFileManager;
use rust_rocksdb::{Env, FlushOptions, Options, DB};
use util::DBPath;

#[test]
fn test_sst_file_manager_config_and_sizes() {
    let path = DBPath::new("_rust_rocksdb_test_sst_file_manager_config_and_sizes");

    let env = Env::new().unwrap();
    let sfm = SstFileManager::new(&env);

    // Set compaction buffer size (no direct getter, just ensure it doesn't panic).
    sfm.set_compaction_buffer_size(64 * 1024);

    // Delete rate and max trash/db ratio should round-trip.
    sfm.set_delete_rate_bytes_per_second(1024);
    assert_eq!(sfm.get_delete_rate_bytes_per_second(), 1024);

    sfm.set_max_trash_db_ratio(0.25);
    assert_eq!(sfm.get_max_trash_db_ratio(), 0.25);

    // Hook into Options and open a DB.
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.set_sst_file_manager(&sfm);

    let db = DB::open(&opts, &path).unwrap();

    // Write enough data and flush to create at least one SST file.
    for i in 0..1000u32 {
        db.put(format!("k{:04}", i).as_bytes(), b"value").unwrap();
    }
    let mut fopts = FlushOptions::default();
    fopts.set_wait(true);
    db.flush_opt(&fopts).unwrap();

    // After flush we should have some SST size accounted for.
    let total_size = sfm.get_total_size();
    assert!(total_size > 0, "expected some SST size; got {}", total_size);

    // Now set the space limit just below current usage and ensure flags flip.
    let limit = total_size.saturating_sub(1);
    sfm.set_max_allowed_space_usage(limit);
    assert!(sfm.is_max_allowed_space_reached());
    assert!(sfm.is_max_allowed_space_reached_including_compactions());

    // Trash size is non-negative; may be zero depending on environment.
    let _trash = sfm.get_total_trash_size();
}

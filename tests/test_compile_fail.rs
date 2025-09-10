#[test]
fn test_compile_fail_cases() {
    let t = trybuild::TestCases::new();
    
    // Checkpoint lifetime tests
    t.compile_fail("tests/fail/checkpoint_outlive_db.rs");
    
    // Iterator lifetime tests  
    t.compile_fail("tests/fail/iterator_outlive_db.rs");
    
    // Snapshot lifetime tests
    t.compile_fail("tests/fail/snapshot_outlive_db.rs");
    
    // Single-threaded mode reference tests
    t.compile_fail("tests/fail/open_with_multiple_refs_as_single_threaded.rs");
    
    // Transaction DB lifetime tests (if they exist)
    if std::path::Path::new("tests/fail/snapshot_outlive_transaction_db.rs").exists() {
        t.compile_fail("tests/fail/snapshot_outlive_transaction_db.rs");
    }
    if std::path::Path::new("tests/fail/transaction_outlive_transaction_db.rs").exists() {
        t.compile_fail("tests/fail/transaction_outlive_transaction_db.rs");
    }
    if std::path::Path::new("tests/fail/snapshot_outlive_transaction.rs").exists() {
        t.compile_fail("tests/fail/snapshot_outlive_transaction.rs");
    }
}

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

mod util;

use crate::util::DBPath;
use rust_rocksdb::{DB, Options, ReadOptions, WriteBatchWithIndex};

struct TrackingAllocator {
    allocations: AtomicUsize,
}

impl TrackingAllocator {
    const fn new() -> Self {
        Self {
            allocations: AtomicUsize::new(0),
        }
    }

    fn get_count(&self) -> usize {
        self.allocations.load(Ordering::SeqCst)
    }
}

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.allocations.fetch_add(1, Ordering::SeqCst);
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator::new();

#[test]
fn test_write_batch_with_index_allocations_comparison() {
    let path = DBPath::new("_rust_rocksdb_wbwi_allocations_test");
    {
        let db = DB::open_default(&path).expect("DB should open");
        let mut wbwi = WriteBatchWithIndex::new(0, true);

        let key = b"allocated_key";
        let val = b"allocated_value_data";

        wbwi.put(key, val);

        let options = Options::default();

        // Warm up any lazy static initialization/loading if any
        let _ = wbwi
            .get_from_batch_with(key, &options, |slice| slice[0])
            .unwrap();

        // Benchmark and verify the Zero-Copy closure API (get_from_batch_with)
        let before_zero_copy = ALLOCATOR.get_count();
        let found_zero_copy = wbwi
            .get_from_batch_with(key, &options, |slice| {
                let inside_zero_copy = ALLOCATOR.get_count();
                // Ensure zero Rust allocations have happened up to this point in the closure
                assert_eq!(
                    inside_zero_copy - before_zero_copy,
                    0,
                    "Rust allocations occurred inside get_from_batch_with closure"
                );
                slice[0]
            })
            .unwrap();
        let after_zero_copy = ALLOCATOR.get_count();

        assert_eq!(found_zero_copy, Some(b'a'));
        assert_eq!(
            after_zero_copy - before_zero_copy,
            0,
            "Rust allocations occurred during get_from_batch_with"
        );

        // Benchmark and verify the Zero-Copy closure with DB fallback API (get_from_batch_and_db_with)
        let readopts = ReadOptions::default();
        let before_zero_copy_db = ALLOCATOR.get_count();
        let found_zero_copy_db = wbwi
            .get_from_batch_and_db_with(&db, key, &readopts, |slice| {
                let inside_zero_copy_db = ALLOCATOR.get_count();
                // Ensure zero Rust allocations have happened up to this point in the closure
                assert_eq!(
                    inside_zero_copy_db - before_zero_copy_db,
                    0,
                    "Rust allocations occurred inside get_from_batch_and_db_with closure"
                );
                slice[1]
            })
            .unwrap();
        let after_zero_copy_db = ALLOCATOR.get_count();

        assert_eq!(found_zero_copy_db, Some(b'l'));
        assert_eq!(
            after_zero_copy_db - before_zero_copy_db,
            0,
            "Rust allocations occurred during get_from_batch_and_db_with"
        );

        // Compare with the traditional cloning API (get_from_batch)
        let before_cloning = ALLOCATOR.get_count();
        let found_cloning = wbwi.get_from_batch(key, &options).unwrap();
        let after_cloning = ALLOCATOR.get_count();

        assert_eq!(found_cloning.as_deref(), Some(val.as_slice()));
        // Ensure that at least 1 Rust allocation (the returned Vec) occurs
        assert!(
            after_cloning - before_cloning >= 1,
            "Traditional get_from_batch failed to allocate memory on the Rust heap"
        );

        println!(
            "SUCCESS: get_from_batch_with allocations: {}, get_from_batch allocations: {}",
            after_zero_copy - before_zero_copy,
            after_cloning - before_cloning
        );
    }
}

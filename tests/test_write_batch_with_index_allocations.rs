use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

mod util;

use crate::util::DBPath;
use rust_rocksdb::{DB, Options, ReadOptions, WriteBatchWithIndex};

thread_local! {
    static ALLOCATION_COUNT: Cell<usize> = const { Cell::new(0) };
}

struct TrackingAllocator;

impl TrackingAllocator {
    const fn new() -> Self {
        Self
    }

    fn get_count(&self) -> usize {
        ALLOCATION_COUNT.with(|cell| cell.get())
    }
}

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATION_COUNT.try_with(|cell| {
            cell.set(cell.get() + 1);
        });
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator::new();

#[test]
fn test_write_batch_with_index_borrowed_reads_avoid_rust_vec_allocation() {
    let path = DBPath::new("_rust_rocksdb_wbwi_allocations_test");
    {
        let db = DB::open_default(&path).expect("DB should open");
        let mut wbwi = WriteBatchWithIndex::new(0, true);

        let key = b"allocated_key";
        let val = b"allocated_value_data";

        wbwi.put(key, val);

        let options = Options::default();

        // Warm up any lazy initialization so the measured section focuses on
        // Rust allocation in the wrapper, not one-time setup. The allocator
        // counter is thread-local, so RocksDB background-thread allocations do
        // not affect these assertions.
        let _ = wbwi
            .get_from_batch_with(key, &options, |slice| slice[0])
            .unwrap();

        let before_borrowed = ALLOCATOR.get_count();
        let found_borrowed = wbwi
            .get_from_batch_with(key, &options, |slice| {
                let inside_borrowed = ALLOCATOR.get_count();
                assert_eq!(
                    inside_borrowed - before_borrowed,
                    0,
                    "borrowed read allocated Rust memory before invoking the closure"
                );
                slice[0]
            })
            .unwrap();
        let after_borrowed = ALLOCATOR.get_count();

        assert_eq!(found_borrowed, Some(b'a'));
        assert_eq!(
            after_borrowed - before_borrowed,
            0,
            "borrowed read allocated Rust memory"
        );

        let db_key = b"db_only_key";
        let db_val = b"db_only_value_data";
        db.put(db_key, db_val).expect("DB put should succeed");

        let readopts = ReadOptions::default();
        let before_borrowed_db = ALLOCATOR.get_count();
        let found_borrowed_db = wbwi
            .get_from_batch_and_db_with(&db, db_key, &readopts, |slice| {
                let inside_borrowed_db = ALLOCATOR.get_count();
                assert_eq!(
                    inside_borrowed_db - before_borrowed_db,
                    0,
                    "borrowed DB read allocated Rust memory before invoking the closure"
                );
                slice[1]
            })
            .unwrap();
        let after_borrowed_db = ALLOCATOR.get_count();

        assert_eq!(found_borrowed_db, Some(b'b'));
        assert_eq!(
            after_borrowed_db - before_borrowed_db,
            0,
            "borrowed DB read allocated Rust memory"
        );

        let before_owned = ALLOCATOR.get_count();
        let found_owned = wbwi.get_from_batch(key, &options).unwrap();
        let after_owned = ALLOCATOR.get_count();

        assert_eq!(found_owned.as_deref(), Some(val.as_slice()));
        assert!(
            after_owned - before_owned >= 1,
            "owned read should allocate Rust memory for the returned Vec"
        );
    }
}

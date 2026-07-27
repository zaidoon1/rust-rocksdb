// Copyright 2026 Tyler Neely, Alex Regueiro
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Regression guards for the allocation-free read paths.
//!
//! These paths keep a reusable thread-local `ReadOptions` specifically to avoid
//! per-call allocation. It is easy to reintroduce an allocation without noticing
//! (for example by calling `set_iterate_range`, which takes owned `Vec`s), so
//! the counts are asserted here rather than left to a benchmark to hint at.
//!
//! Only Rust-side allocations are counted. RocksDB's own C++ allocations go
//! through `malloc` and are invisible to a Rust global allocator.
//!
//! Everything lives in a single `#[test]` on purpose: the allocation counter is
//! process-global, so a second test running concurrently in this binary would
//! have its allocations attributed to whichever measurement happens to be open.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rust_rocksdb::{DB, Options};

mod util;
use util::DBPath;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static COUNTING: AtomicBool = AtomicBool::new(false);

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Counts Rust heap allocations performed by `f`.
fn count_allocs(f: impl FnOnce()) -> usize {
    let before = ALLOCS.load(Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    f();
    COUNTING.store(false, Ordering::Relaxed);
    ALLOCS.load(Ordering::Relaxed) - before
}

#[test]
fn hot_read_paths_do_not_allocate() {
    let path = DBPath::new("_rust_rocksdb_alloc_counts");
    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = DB::open(&opts, &path).unwrap();

    db.put(b"prefix_a/1", b"v").unwrap();
    db.put(b"prefix_b/1", b"v").unwrap();
    db.put(b"aaaa", b"v").unwrap();
    db.put(b"b", b"v").unwrap();
    db.put(b"k", b"value").unwrap();

    // ---------------------------------------------------------------------
    // prefix_exists used to allocate two `Vec<u8>` per call — one for the
    // lower bound and one for the computed upper bound — because it went
    // through `ReadOptions::set_iterate_range(PrefixRange(..))`, whose
    // `into_bounds` hands back owned vectors. That defeated the whole point of
    // the thread-local `ReadOptions` it was reusing.
    // ---------------------------------------------------------------------

    // Warm up: the first call on this thread initialises the thread-local
    // `ReadOptions` and grows its bound buffers.
    for _ in 0..4 {
        assert!(db.prefix_exists(b"prefix_a").unwrap());
    }

    let allocs = count_allocs(|| {
        for _ in 0..64 {
            assert!(db.prefix_exists(b"prefix_a").unwrap());
            assert!(!db.prefix_exists(b"prefix_z").unwrap());
        }
    });
    assert_eq!(
        allocs, 0,
        "prefix_exists should not allocate once its bound buffers are warm, got {allocs} \
         allocations over 128 calls"
    );

    // ---------------------------------------------------------------------
    // A shorter prefix after a longer one must reuse the same buffer rather
    // than reallocating, and must still produce correct results.
    // ---------------------------------------------------------------------

    // Longest prefix first so the buffer reaches its final capacity.
    let probes: &[(&[u8], bool)] = &[
        (b"aaaaa", false),
        (b"aaaa", true),
        (b"aaa", true),
        (b"aa", true),
        (b"a", true),
        (b"b", true),
        (b"c", false),
    ];

    for _ in 0..4 {
        for (prefix, expected) in probes {
            assert_eq!(db.prefix_exists(prefix).unwrap(), *expected);
        }
    }

    let allocs = count_allocs(|| {
        for _ in 0..16 {
            for (prefix, expected) in probes {
                assert_eq!(db.prefix_exists(prefix).unwrap(), *expected);
            }
        }
    });
    assert_eq!(
        allocs, 0,
        "prefix probes of varying length should reuse the bound buffers, got {allocs}"
    );

    // ---------------------------------------------------------------------
    // `get_into_buffer` exists so callers can read without allocating at all.
    // ---------------------------------------------------------------------

    let mut buf = vec![0u8; 64];
    // Warm the thread-local default ReadOptions.
    let _ = db.get_into_buffer(b"k", &mut buf).unwrap();

    let allocs = count_allocs(|| {
        for _ in 0..64 {
            let _ = db.get_into_buffer(b"k", &mut buf).unwrap();
        }
    });
    assert_eq!(
        allocs, 0,
        "get_into_buffer should not allocate, got {allocs} over 64 calls"
    );
}

// Copyright 2020 Tyler Neely
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

mod util;

use std::{
    alloc::{GlobalAlloc, Layout, System},
    ops::ControlFlow,
    sync::atomic::{AtomicUsize, Ordering},
};

use rust_rocksdb::{DB, Error, IteratorMode};
use util::DBPath;

static ALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Debug, PartialEq)]
enum ScanError {
    RocksDb(Error),
    Callback,
}

impl From<Error> for ScanError {
    fn from(error: Error) -> Self {
        Self::RocksDb(error)
    }
}

fn assert_allocation_free_forward_scan(db: &DB) {
    let mut item_count = 0;
    let mut total_bytes = 0;
    let mut iter = db.iterator(IteratorMode::Start);
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    let outcome: Result<ControlFlow<()>, ScanError> = iter.try_for_each_ref(|key, value| {
        item_count += 1;
        total_bytes += key.len() + value.len();
        Ok(ControlFlow::Continue(()))
    });
    let scan_allocations = ALLOCATION_COUNT.load(Ordering::Relaxed);

    assert_eq!(outcome, Ok(ControlFlow::Continue(())));
    assert_eq!(item_count, 3);
    assert_eq!(total_bytes, 12);
    assert_eq!(scan_allocations, 0);
    assert_eq!(iter.next(), None);
}

fn assert_reverse_scan(db: &DB) {
    let mut reverse_keys = [0; 3];
    let mut reverse_index = 0;
    let mut reverse = db.iterator(IteratorMode::End);
    let outcome: Result<ControlFlow<()>, ScanError> = reverse.try_for_each_ref(|key, _| {
        reverse_keys[reverse_index] = key[1];
        reverse_index += 1;
        Ok(ControlFlow::Continue(()))
    });
    assert_eq!(outcome, Ok(ControlFlow::Continue(())));
    assert_eq!(reverse_keys, *b"321");
}

fn assert_break_resumes_after_consumed_item(db: &DB) {
    let mut stopped = db.iterator(IteratorMode::Start);
    let outcome: Result<ControlFlow<usize>, ScanError> = stopped.try_for_each_ref(|key, _| {
        if key == b"k2" {
            Ok(ControlFlow::Break(2))
        } else {
            Ok(ControlFlow::Continue(()))
        }
    });
    assert_eq!(outcome, Ok(ControlFlow::Break(2)));
    let (key, value) = stopped.next().unwrap().unwrap();
    assert_eq!(key.as_ref(), b"k3");
    assert_eq!(value.as_ref(), b"v3");
}

fn assert_callback_error_resumes_after_consumed_item(db: &DB) {
    let mut failed = db.iterator(IteratorMode::Start);
    let outcome: Result<ControlFlow<()>, ScanError> = failed.try_for_each_ref(|key, _| {
        if key == b"k2" {
            Err(ScanError::Callback)
        } else {
            Ok(ControlFlow::Continue(()))
        }
    });
    assert_eq!(outcome, Err(ScanError::Callback));
    let (key, value) = failed.next().unwrap().unwrap();
    assert_eq!(key.as_ref(), b"k3");
    assert_eq!(value.as_ref(), b"v3");
}

#[test]
fn borrowed_scan_is_allocation_free_and_resumable() {
    let path = DBPath::new("_rust_rocksdb_borrowed_iterator");
    let db = DB::open_default(&path).unwrap();
    db.put(b"k1", b"v1").unwrap();
    db.put(b"k2", b"v2").unwrap();
    db.put(b"k3", b"v3").unwrap();

    assert_allocation_free_forward_scan(&db);
    assert_reverse_scan(&db);
    assert_break_resumes_after_consumed_item(&db);
    assert_callback_error_resumes_after_consumed_item(&db);
}

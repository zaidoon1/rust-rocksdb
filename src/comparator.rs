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
//

use crate::{ffi, ffi_util::CStrLike};
use libc::{c_char, c_int, c_uchar, c_void, size_t};
use std::cmp::Ordering;
use std::ffi::CString;
use std::ptr::NonNull;
use std::slice;

// RocksDB calls these from whatever thread reaches the comparison, including
// background flush and compaction threads, and from several at once. They have
// to be shareable across threads for `Options` and `Comparator` to be `Sync`.
pub type CompareFn = dyn Fn(&[u8], &[u8]) -> Ordering + Send + Sync;

pub type CompareTsFn = dyn Fn(&[u8], &[u8]) -> Ordering + Send + Sync;

pub type CompareWithoutTsFn = dyn Fn(&[u8], bool, &[u8], bool) -> Ordering + Send + Sync;

pub struct ComparatorCallback {
    pub name: CString,
    pub compare_fn: Box<CompareFn>,
}

impl ComparatorCallback {
    pub unsafe extern "C" fn destructor_callback(raw_cb: *mut c_void) {
        unsafe {
            drop(Box::from_raw(raw_cb as *mut Self));
        }
    }

    pub unsafe extern "C" fn name_callback(raw_cb: *mut c_void) -> *const c_char {
        unsafe {
            let cb: &Self = &*(raw_cb as *const Self);
            let ptr = cb.name.as_ptr();
            ptr as *const c_char
        }
    }

    pub unsafe extern "C" fn compare_callback(
        raw_cb: *mut c_void,
        a_raw: *const c_char,
        a_len: size_t,
        b_raw: *const c_char,
        b_len: size_t,
    ) -> c_int {
        unsafe {
            let cb: &Self = &*(raw_cb as *const Self);
            let a: &[u8] = slice::from_raw_parts(a_raw.cast::<u8>(), a_len);
            let b: &[u8] = slice::from_raw_parts(b_raw.cast::<u8>(), b_len);
            (cb.compare_fn)(a, b) as c_int
        }
    }
}

pub struct ComparatorWithTsCallback {
    pub name: CString,
    pub compare_fn: Box<CompareFn>,
    pub compare_ts_fn: Box<CompareTsFn>,
    pub compare_without_ts_fn: Box<CompareWithoutTsFn>,
}

impl ComparatorWithTsCallback {
    pub unsafe extern "C" fn destructor_callback(raw_cb: *mut c_void) {
        unsafe {
            drop(Box::from_raw(raw_cb as *mut Self));
        }
    }

    pub unsafe extern "C" fn name_callback(raw_cb: *mut c_void) -> *const c_char {
        unsafe {
            let cb: &Self = &*(raw_cb as *const Self);
            let ptr = cb.name.as_ptr();
            ptr as *const c_char
        }
    }

    pub unsafe extern "C" fn compare_callback(
        raw_cb: *mut c_void,
        a_raw: *const c_char,
        a_len: size_t,
        b_raw: *const c_char,
        b_len: size_t,
    ) -> c_int {
        unsafe {
            let cb: &Self = &*(raw_cb as *const Self);
            let a: &[u8] = slice::from_raw_parts(a_raw.cast::<u8>(), a_len);
            let b: &[u8] = slice::from_raw_parts(b_raw.cast::<u8>(), b_len);
            (cb.compare_fn)(a, b) as c_int
        }
    }

    pub unsafe extern "C" fn compare_ts_callback(
        raw_cb: *mut c_void,
        a_ts_raw: *const c_char,
        a_ts_len: size_t,
        b_ts_raw: *const c_char,
        b_ts_len: size_t,
    ) -> c_int {
        unsafe {
            let cb: &Self = &*(raw_cb as *const Self);
            let a_ts: &[u8] = slice::from_raw_parts(a_ts_raw.cast::<u8>(), a_ts_len);
            let b_ts: &[u8] = slice::from_raw_parts(b_ts_raw.cast::<u8>(), b_ts_len);
            (cb.compare_ts_fn)(a_ts, b_ts) as c_int
        }
    }

    pub unsafe extern "C" fn compare_without_ts_callback(
        raw_cb: *mut c_void,
        a_raw: *const c_char,
        a_len: size_t,
        a_has_ts_raw: c_uchar,
        b_raw: *const c_char,
        b_len: size_t,
        b_has_ts_raw: c_uchar,
    ) -> c_int {
        unsafe {
            let cb: &Self = &*(raw_cb as *const Self);
            let a: &[u8] = slice::from_raw_parts(a_raw.cast::<u8>(), a_len);
            let a_has_ts = a_has_ts_raw != 0;
            let b: &[u8] = slice::from_raw_parts(b_raw.cast::<u8>(), b_len);
            let b_has_ts = b_has_ts_raw != 0;
            (cb.compare_without_ts_fn)(a, a_has_ts, b, b_has_ts) as c_int
        }
    }
}

/// A key ordering, owned by Rust and shareable with anything that needs one.
///
/// RocksDB never takes ownership of a comparator: `Options`, `WriteBatchWithIndex`
/// and the rest all keep a borrowed pointer to it. Wrap it in an `Arc` and hand
/// out clones so it outlives every user, which is what
/// [`Options::set_comparator`](crate::Options::set_comparator) does internally.
pub struct Comparator {
    pub(crate) inner: NonNull<ffi::rocksdb_comparator_t>,
}

impl Comparator {
    /// Builds a comparator from a total order over keys.
    ///
    /// `name` is persisted in the SST files. Changing the ordering without
    /// changing the name gives RocksDB no way to notice, and it will read the
    /// old files with the new ordering, so pick a new name whenever the order
    /// changes.
    ///
    /// # Panics
    ///
    /// Panics if `name` contains an interior nul byte.
    pub fn new(name: impl CStrLike, compare_fn: Box<CompareFn>) -> Self {
        let cb = Box::new(ComparatorCallback {
            name: name.into_c_string().unwrap(),
            compare_fn,
        });
        let inner = unsafe {
            ffi::rocksdb_comparator_create(
                Box::into_raw(cb).cast::<c_void>(),
                Some(ComparatorCallback::destructor_callback),
                Some(ComparatorCallback::compare_callback),
                Some(ComparatorCallback::name_callback),
            )
        };
        Self {
            inner: NonNull::new(inner).expect("rocksdb_comparator_create returned null"),
        }
    }

    /// Builds a comparator for a column family that uses user-defined timestamps.
    ///
    /// `timestamp_size` is the fixed width of the timestamp suffix on every key.
    /// `compare_fn` orders whole keys including the timestamp, `compare_ts_fn`
    /// orders two bare timestamps, and `compare_without_ts_fn` orders keys
    /// ignoring their timestamps. See the RocksDB [user-defined timestamp] docs.
    ///
    /// # Panics
    ///
    /// Panics if `name` contains an interior nul byte.
    ///
    /// [user-defined timestamp]: https://github.com/facebook/rocksdb/wiki/User-defined-Timestamp
    pub fn with_ts(
        name: impl CStrLike,
        timestamp_size: usize,
        compare_fn: Box<CompareFn>,
        compare_ts_fn: Box<CompareTsFn>,
        compare_without_ts_fn: Box<CompareWithoutTsFn>,
    ) -> Self {
        let cb = Box::new(ComparatorWithTsCallback {
            name: name.into_c_string().unwrap(),
            compare_fn,
            compare_ts_fn,
            compare_without_ts_fn,
        });
        let inner = unsafe {
            ffi::rocksdb_comparator_with_ts_create(
                Box::into_raw(cb).cast::<c_void>(),
                Some(ComparatorWithTsCallback::destructor_callback),
                Some(ComparatorWithTsCallback::compare_callback),
                Some(ComparatorWithTsCallback::compare_ts_callback),
                Some(ComparatorWithTsCallback::compare_without_ts_callback),
                Some(ComparatorWithTsCallback::name_callback),
                timestamp_size,
            )
        };
        Self {
            inner: NonNull::new(inner).expect("rocksdb_comparator_with_ts_create returned null"),
        }
    }

    pub(crate) fn from_raw(inner: NonNull<ffi::rocksdb_comparator_t>) -> Self {
        Self { inner }
    }
}

impl Drop for Comparator {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_comparator_destroy(self.inner.as_ptr());
        }
    }
}

// Safe because the callbacks are `Fn + Send + Sync` and the only other state is
// a `CString` that is read but never mutated. Destroying the comparator needs
// `&mut self`, so it cannot race with a comparison.
unsafe impl Send for Comparator {}
unsafe impl Sync for Comparator {}

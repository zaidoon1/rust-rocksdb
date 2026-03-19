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

use crate::util::{assert_item, assert_no_item};
use rust_rocksdb::{DB, Options, ReadOptions};
use util::{DBPath, U64Comparator, U64Timestamp};

#[test]
pub fn test_forwards_iteration() {
    let n = DBPath::new("forwards_iteration");
    {
        let db = DB::open_default(&n).unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.put(b"k2", b"v2").unwrap();
        db.put(b"k3", b"v3").unwrap();
        db.put(b"k4", b"v4").unwrap();

        let mut iter = db.raw_iterator();
        iter.seek_to_first();
        assert_item(&iter, b"k1", b"v1");

        iter.next();
        assert_item(&iter, b"k2", b"v2");

        iter.next(); // k3
        iter.next(); // k4

        iter.next(); // invalid!
        assert_no_item(&iter);
    }
}

#[test]
pub fn test_seek_last() {
    let n = DBPath::new("backwards_iteration");
    {
        let db = DB::open_default(&n).unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.put(b"k2", b"v2").unwrap();
        db.put(b"k3", b"v3").unwrap();
        db.put(b"k4", b"v4").unwrap();

        let mut iter = db.raw_iterator();
        iter.seek_to_last();
        assert_item(&iter, b"k4", b"v4");

        iter.prev();
        assert_item(&iter, b"k3", b"v3");

        iter.prev(); // k2
        iter.prev(); // k1

        iter.prev(); // invalid!
        assert_no_item(&iter);
    }
}

#[test]
pub fn test_seek() {
    let n = DBPath::new("seek");
    {
        let db = DB::open_default(&n).unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.put(b"k2", b"v2").unwrap();
        db.put(b"k4", b"v4").unwrap();

        let mut iter = db.raw_iterator();
        iter.seek(b"k2");

        assert_item(&iter, b"k2", b"v2");

        // Check it gets the next key when the key doesn't exist
        iter.seek(b"k3");
        assert_item(&iter, b"k4", b"v4");
    }
}

#[test]
pub fn test_seek_to_nonexistent() {
    let n = DBPath::new("seek_to_nonexistent");
    {
        let db = DB::open_default(&n).unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.put(b"k3", b"v3").unwrap();
        db.put(b"k4", b"v4").unwrap();

        let mut iter = db.raw_iterator();
        iter.seek(b"k2");
        assert_item(&iter, b"k3", b"v3");
    }
}

#[test]
pub fn test_seek_for_prev() {
    let n = DBPath::new("seek_for_prev");
    {
        let db = DB::open_default(&n).unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.put(b"k2", b"v2").unwrap();
        db.put(b"k4", b"v4").unwrap();

        let mut iter = db.raw_iterator();
        iter.seek(b"k2");
        assert_item(&iter, b"k2", b"v2");

        // Check it gets the previous key when the key doesn't exist
        iter.seek_for_prev(b"k3");
        assert_item(&iter, b"k2", b"v2");
    }
}

#[test]
pub fn test_next_without_seek() {
    let n = DBPath::new("test_forgot_seek");
    {
        let db = DB::open_default(&n).unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.put(b"k2", b"v2").unwrap();
        db.put(b"k4", b"v4").unwrap();

        let mut iter = db.raw_iterator();
        iter.next();
    }
}

#[test]
pub fn test_refresh() {
    let n = DBPath::new("test_refresh");
    {
        let db = DB::open_default(&n).unwrap();
        db.put(b"k1", b"v1").unwrap();

        let mut iter = db.raw_iterator();
        iter.seek_to_first();
        assert_item(&iter, b"k1", b"v1");

        // Write new data after iterator was created
        db.put(b"k2", b"v2").unwrap();

        // Iterator doesn't see k2 yet
        iter.seek(b"k2");
        assert_no_item(&iter);

        // After refresh, the iterator sees k2
        iter.refresh().unwrap();
        iter.seek(b"k2");
        assert_item(&iter, b"k2", b"v2");
    }
}

#[test]
pub fn test_refresh_with_snapshot() {
    let n = DBPath::new("test_refresh_snapshot");
    {
        let db = DB::open_default(&n).unwrap();
        db.put(b"k1", b"v1").unwrap();

        let snapshot = db.snapshot();
        let mut readopts = ReadOptions::default();
        readopts.set_snapshot(&snapshot);
        let mut iter = db.raw_iterator_opt(readopts);
        iter.seek_to_first();
        assert_item(&iter, b"k1", b"v1");

        // Write new data after snapshot was taken
        db.put(b"k2", b"v2").unwrap();

        // Snapshot iterator doesn't see k2 yet
        iter.seek(b"k2");
        assert_no_item(&iter);

        // After refresh, the iterator no longer honors the snapshot and
        // instead reads the latest DB state, so k2 is now visible
        iter.refresh().unwrap();
        iter.seek(b"k2");
        assert_item(&iter, b"k2", b"v2");
    }
}

#[test]
fn test_iterator_timestamp() {
    let n = DBPath::new("_rust_rocksdb_iterator_timestamp_test");

    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.set_comparator_with_ts(
        U64Comparator::NAME,
        U64Timestamp::SIZE,
        Box::new(U64Comparator::compare),
        Box::new(U64Comparator::compare_ts),
        Box::new(U64Comparator::compare_without_ts),
    );
    let db = DB::open(&opts, &n).unwrap();

    let ts1 = U64Timestamp::new(1);
    let ts2 = U64Timestamp::new(2);
    let ts3 = U64Timestamp::new(3);

    db.put_with_ts(b"k1", ts1, b"v1").unwrap();
    db.put_with_ts(b"k2", ts2, b"v2").unwrap();
    db.put_with_ts(b"k3", ts3, b"v3").unwrap();

    // Read at ts3 will return all keys
    let mut read_opts = ReadOptions::default();
    read_opts.set_timestamp(ts3);

    let mut iter = db.raw_iterator_opt(read_opts);

    // Seek to first and walk through all entries, check timestamps
    iter.seek_to_first();
    assert!(iter.valid());
    assert_eq!(iter.key(), Some(b"k1".as_ref()));
    unsafe {
        assert_eq!(iter.timestamp(), ts1.as_ref());
    }

    iter.next();
    assert!(iter.valid());
    assert_eq!(iter.key(), Some(b"k2".as_ref()));
    unsafe {
        assert_eq!(iter.timestamp(), ts2.as_ref());
    }

    iter.next();
    assert!(iter.valid());
    assert_eq!(iter.key(), Some(b"k3".as_ref()));
    unsafe {
        assert_eq!(iter.timestamp(), ts3.as_ref());
    }

    // Past last entry, iterator is invalid
    iter.next();
    assert!(!iter.valid());

    // Seek to specific key
    iter.seek(b"k2");
    assert!(iter.valid());
    unsafe {
        assert_eq!(iter.timestamp(), ts2.as_ref());
    }

    // Read at ts1, only k1 visible
    let mut read_opts = ReadOptions::default();
    read_opts.set_timestamp(ts1);
    let mut iter = db.raw_iterator_opt(read_opts);
    iter.seek_to_first();
    assert!(iter.valid());
    assert_eq!(iter.key(), Some(b"k1".as_ref()));
    unsafe {
        assert_eq!(iter.timestamp(), ts1.as_ref());
    }
}

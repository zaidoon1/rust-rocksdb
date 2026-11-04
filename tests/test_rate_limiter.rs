// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

//! Shared, runtime-adjustable `RateLimiter`.

mod util;

use pretty_assertions::assert_eq;
use rust_rocksdb::{DB, Options, RateLimiter, RateLimiterMode};
use util::DBPath;

const RATE: i64 = 64 << 20;

fn write_and_flush(db: &DB) {
    for i in 0..100_u32 {
        db.put(format!("key{i:04}"), vec![b'x'; 1024]).unwrap();
    }
    db.flush().unwrap();
    assert_eq!(
        db.get(b"key0042").unwrap().as_deref(),
        Some(&[b'x'; 1024][..])
    );
}

fn open(path: &DBPath, limiter: &RateLimiter) -> DB {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.set_rate_limiter(limiter);
    DB::open(&opts, path).unwrap()
}

#[test]
fn set_bytes_per_second_round_trips() {
    // The setter and getter reach the C++ RateLimiter through a cast that
    // assumes `rocksdb_ratelimiter_t` (db/c.cc) starts with its
    // `shared_ptr<RateLimiter> rep` field. If a RocksDB upgrade changes that
    // layout, the value read back will not be the one written.
    let limiter = RateLimiter::new(RATE, 100_000, 10);
    assert_eq!(limiter.get_bytes_per_second(), RATE);

    limiter.set_bytes_per_second(RATE / 4);
    assert_eq!(limiter.get_bytes_per_second(), RATE / 4);

    // A clone is the same limiter, not a copy.
    let clone = limiter.clone();
    clone.set_bytes_per_second(RATE * 2);
    assert_eq!(limiter.get_bytes_per_second(), RATE * 2);
}

#[test]
#[should_panic(expected = "must be positive")]
fn set_bytes_per_second_rejects_zero() {
    RateLimiter::new(RATE, 100_000, 10).set_bytes_per_second(0);
}

#[test]
fn one_limiter_is_shared_by_many_dbs() {
    let path1 = DBPath::new("_rust_rocksdb_shared_rate_limiter_1");
    let path2 = DBPath::new("_rust_rocksdb_shared_rate_limiter_2");
    let limiter = RateLimiter::new(RATE, 100_000, 10);
    assert_eq!(limiter.get_total_bytes_through(), 0);
    assert_eq!(limiter.get_total_requests(), 0);

    let db1 = open(&path1, &limiter);
    let db2 = open(&path2, &limiter);

    write_and_flush(&db1);
    let after_db1 = limiter.get_total_bytes_through();
    assert!(after_db1 > 0, "flush of db1 was not charged to the limiter");

    write_and_flush(&db2);
    let after_db2 = limiter.get_total_bytes_through();
    assert!(
        after_db2 > after_db1,
        "flush of db2 was not charged to the shared limiter"
    );
    assert!(limiter.get_total_requests() > 0);

    // A rate change is visible to, and does not disturb, both open DBs.
    limiter.set_bytes_per_second(RATE / 2);
    assert_eq!(limiter.get_bytes_per_second(), RATE / 2);
    write_and_flush(&db1);
    write_and_flush(&db2);
}

#[test]
fn dropping_the_handle_keeps_dbs_working() {
    let path1 = DBPath::new("_rust_rocksdb_rate_limiter_drop_1");
    let path2 = DBPath::new("_rust_rocksdb_rate_limiter_drop_2");
    let limiter = RateLimiter::new(RATE, 100_000, 10);

    let db1;
    let db2;
    {
        // Options dropped at the end of this scope, too.
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.set_rate_limiter(&limiter);
        db1 = DB::open(&opts, &path1).unwrap();
        db2 = DB::open(&opts, &path2).unwrap();
    }
    drop(limiter);

    write_and_flush(&db1);
    write_and_flush(&db2);
}

#[test]
fn auto_tuned_limiter() {
    let path = DBPath::new("_rust_rocksdb_rate_limiter_auto_tuned");
    let limiter = RateLimiter::new_auto_tuned(RATE, 100_000, 10);
    let initial = limiter.get_bytes_per_second();
    assert!(
        initial > 0 && initial <= RATE,
        "auto-tuned limiter started outside its range: {initial}"
    );

    let db = open(&path, &limiter);
    write_and_flush(&db);
    assert!(limiter.get_total_bytes_through() > 0);
}

#[test]
fn limiter_with_mode() {
    let path = DBPath::new("_rust_rocksdb_rate_limiter_with_mode");
    let limiter = RateLimiter::new_with_mode(RATE, 100_000, 10, RateLimiterMode::KAllIo, false);
    assert_eq!(limiter.get_bytes_per_second(), RATE);

    let db = open(&path, &limiter);
    write_and_flush(&db);
    limiter.set_bytes_per_second(RATE / 8);
    assert_eq!(limiter.get_bytes_per_second(), RATE / 8);
    write_and_flush(&db);
}

#[test]
fn set_rate_limiter_replaces_set_ratelimiter() {
    let path = DBPath::new("_rust_rocksdb_rate_limiter_replace");
    let limiter = RateLimiter::new(RATE, 100_000, 10);

    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.set_ratelimiter(RATE, 100_000, 10);
    opts.set_rate_limiter(&limiter);
    let db = DB::open(&opts, &path).unwrap();

    write_and_flush(&db);
    assert!(limiter.get_total_bytes_through() > 0);
}

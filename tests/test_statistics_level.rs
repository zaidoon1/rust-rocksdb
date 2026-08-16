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

mod util;

use rust_rocksdb::statistics::{Histogram, StatsLevel, Ticker};
use rust_rocksdb::{DB, Options};
use util::DBPath;

const ALL_LEVELS: [StatsLevel; 6] = [
    StatsLevel::DisableAll,
    StatsLevel::ExceptHistogramOrTimers,
    StatsLevel::ExceptTimers,
    StatsLevel::ExceptDetailedTimers,
    StatsLevel::ExceptTimeForMutex,
    StatsLevel::All,
];

/// The values RocksDB's C API documents in `c.h`, which are what the setter passes
/// straight through to `set_stats_level`.
///
/// Pinned as literals on purpose. The enum takes its discriminants from the
/// generated constants, so comparing the two would prove nothing. These are the
/// numbers a reader of `include/rocksdb/c.h` can check by hand.
#[test]
fn stats_level_discriminants_match_the_c_api() {
    assert_eq!(StatsLevel::DisableAll as u32, 0);
    assert_eq!(StatsLevel::ExceptHistogramOrTimers as u32, 1);
    assert_eq!(StatsLevel::ExceptTimers as u32, 2);
    assert_eq!(StatsLevel::ExceptDetailedTimers as u32, 3);
    assert_eq!(StatsLevel::ExceptTimeForMutex as u32, 4);
    assert_eq!(StatsLevel::All as u32, 5);

    // `kExceptTickers` shares `kDisableAll`'s value, so those two are the only
    // pair RocksDB allows to collide. Everything else has to be distinct, or the
    // setter and getter would disagree about which level is in force.
    for (i, a) in ALL_LEVELS.iter().enumerate() {
        for b in &ALL_LEVELS[i + 1..] {
            assert_ne!(*a as u32, *b as u32, "{a:?} and {b:?} share a value");
        }
    }
}

#[test]
fn every_stats_level_round_trips() {
    let mut opts = Options::default();
    opts.enable_statistics();

    for level in ALL_LEVELS {
        opts.set_statistics_level(level);
        assert_eq!(
            opts.get_statistics_level(),
            Some(level),
            "{level:?} did not come back unchanged"
        );
    }
}

#[test]
fn statistics_level_is_disable_all_before_statistics_are_enabled() {
    // The C API reports `kDisableAll` when there is no statistics object, so this
    // is not distinguishable from having asked for `DisableAll`.
    let opts = Options::default();
    assert_eq!(
        opts.get_statistics_level(),
        Some(StatsLevel::DisableAll),
        "with no statistics object the level should read as DisableAll"
    );
}

/// What a DB configured at one level actually recorded.
///
/// The three are gated differently, which is what makes them useful for pinning
/// the numeric levels:
/// - `keys_written` is a ticker, recorded above `DisableAll`.
/// - `bytes_per_write` is a plain histogram, recorded above
///   `ExceptHistogramOrTimers` through `RecordInHistogram`.
/// - `write_micros` is a timer histogram, and `StopWatch` only enables itself
///   above `ExceptTimers` (`util/stop_watch.h`).
#[derive(Debug, PartialEq, Eq)]
struct Collected {
    keys_written: u64,
    bytes_per_write: u64,
    write_micros: u64,
}

fn collect_at_level(name: &str, level: StatsLevel) -> Collected {
    let path = DBPath::new(name);
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.enable_statistics();
    opts.set_statistics_level(level);

    {
        let db = DB::open(&opts, &path).unwrap();
        for i in 0..200u32 {
            let key = format!("key{i:04}");
            db.put(key.as_bytes(), b"value").unwrap();
        }
        db.flush().unwrap();
        for i in 0..200u32 {
            let key = format!("key{i:04}");
            assert!(db.get(key.as_bytes()).unwrap().is_some());
        }
    }

    Collected {
        keys_written: opts.get_ticker_count(Ticker::NumberKeysWritten),
        bytes_per_write: opts.get_histogram_data(Histogram::BytesPerWrite).count(),
        write_micros: opts.get_histogram_data(Histogram::DbWrite).count(),
    }
}

#[test]
fn disable_all_records_nothing() {
    let got = collect_at_level(
        "_rust_rocksdb_stats_level_disable_all",
        StatsLevel::DisableAll,
    );
    assert_eq!(
        got,
        Collected {
            keys_written: 0,
            bytes_per_write: 0,
            write_micros: 0
        }
    );
}

#[test]
fn except_histogram_or_timers_records_only_tickers() {
    // Pins the value of `ExceptHistogramOrTimers`. If it were off by one and sent
    // `kExceptTimers`, `bytes_per_write` would be non-zero here.
    let got = collect_at_level(
        "_rust_rocksdb_stats_level_except_histogram",
        StatsLevel::ExceptHistogramOrTimers,
    );
    assert!(got.keys_written > 0, "tickers should still count: {got:?}");
    assert_eq!(
        got.bytes_per_write, 0,
        "histograms should be skipped: {got:?}"
    );
    assert_eq!(got.write_micros, 0, "timers should be skipped: {got:?}");
}

#[test]
fn except_timers_records_histograms_but_not_timers() {
    // Pins the value of `ExceptTimers` from both sides: plain histograms are on,
    // timer histograms are still off.
    let got = collect_at_level(
        "_rust_rocksdb_stats_level_except_timers",
        StatsLevel::ExceptTimers,
    );
    assert!(got.keys_written > 0, "tickers should count: {got:?}");
    assert!(
        got.bytes_per_write > 0,
        "plain histograms should be recorded: {got:?}"
    );
    assert_eq!(
        got.write_micros, 0,
        "timer histograms need a level above ExceptTimers: {got:?}"
    );
}

#[test]
fn levels_above_except_timers_record_timers_too() {
    for (name, level) in [
        (
            "_rust_rocksdb_stats_level_except_detailed",
            StatsLevel::ExceptDetailedTimers,
        ),
        (
            "_rust_rocksdb_stats_level_except_mutex",
            StatsLevel::ExceptTimeForMutex,
        ),
        ("_rust_rocksdb_stats_level_all", StatsLevel::All),
    ] {
        let got = collect_at_level(name, level);
        assert!(
            got.keys_written > 0,
            "{level:?} should count tickers: {got:?}"
        );
        assert!(
            got.bytes_per_write > 0,
            "{level:?} should record plain histograms: {got:?}"
        );
        assert!(
            got.write_micros > 0,
            "{level:?} should record timer histograms: {got:?}"
        );
    }
}

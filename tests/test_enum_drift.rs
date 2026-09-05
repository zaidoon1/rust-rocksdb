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

//! Guards the enums whose values this crate has to hardcode.
//!
//! Most enums that cross the C API have `rocksdb_*` constants in `c.h`, so
//! their discriminants are written as `ffi::rocksdb_...` and the compiler keeps
//! them honest. The enums checked here have no such constants. RocksDB passes
//! them as plain ints, so the only record of the right value is the C++ header,
//! and an upstream insertion silently shifts every variant after it.
//!
//! That has already bitten this crate three times: a compaction triggered by
//! read frequency reported as the `KNumOfReasons` sentinel, a flush for
//! memtable range deletions reported as `KUnknown`, and `StatsLevel` mapping
//! every level onto the wrong C++ one.
//!
//! Each test parses the vendored header and compares it against the Rust enum,
//! so bumping RocksDB fails here instead of silently misreporting. The vendored
//! source is a submodule, so the tests skip when it is not checked out.

use rust_rocksdb::event_listener::{
    DBBackgroundErrorReason, DBCompactionReason, DBFlushReason, DBWriteStallCondition,
    StatusSeverity,
};
use rust_rocksdb::perf::PerfStatsLevel;
use rust_rocksdb::statistics::StatsLevel;
use rust_rocksdb::{EnvPriority, FileType};

/// One entry of a C++ enum, with its computed value.
#[derive(Debug, PartialEq, Eq)]
struct CppEnumerator {
    name: String,
    value: i64,
}

/// Reads a vendored RocksDB header, or `None` when the submodule is absent.
fn read_header(relative: &str) -> Option<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("librocksdb-sys/rocksdb")
        .join(relative);
    std::fs::read_to_string(path).ok()
}

/// Parses a C++ enum body into its enumerators, resolving implicit increments.
///
/// Handles `kFoo,`, `kFoo = 3,` and `kFoo = 0x0c,`. Comments and blank lines are
/// ignored. `declaration` must be the exact text that opens the enum, for
/// example `enum class CompactionReason`, so a prefix of another enum's name
/// cannot match instead.
fn parse_cpp_enum(header: &str, declaration: &str) -> Vec<CppEnumerator> {
    let start = header
        .find(declaration)
        .unwrap_or_else(|| panic!("{declaration} not found in the vendored header"));
    let open = header[start..]
        .find('{')
        .unwrap_or_else(|| panic!("{declaration} has no opening brace"))
        + start;
    let close = header[open..]
        .find("};")
        .unwrap_or_else(|| panic!("{declaration} has no closing brace"))
        + open;

    let mut out = Vec::new();
    let mut next = 0i64;
    for raw in header[open + 1..close].lines() {
        // Strip trailing comments, then skip anything that is not an entry.
        let line = raw.split("//").next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('/') || line.starts_with('*') {
            continue;
        }
        for entry in line.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (name, value) = match entry.split_once('=') {
                Some((name, value)) => {
                    let value = value.trim();
                    let parsed = value
                        .strip_prefix("0x")
                        .map(|hex| i64::from_str_radix(hex.trim_end_matches(['U', 'u']), 16))
                        .unwrap_or_else(|| value.trim_end_matches(['U', 'u']).parse())
                        .ok()
                        // An entry can alias an earlier one, the way
                        // kExceptTickers aliases kDisableAll in statistics.h.
                        .or_else(|| {
                            out.iter()
                                .find(|e: &&CppEnumerator| e.name == value)
                                .map(|e| e.value)
                        })
                        .unwrap_or_else(|| panic!("cannot parse value {value:?} in {declaration}"));
                    (name.trim(), parsed)
                }
                None => (entry, next),
            };
            if !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                continue;
            }
            out.push(CppEnumerator {
                name: name.to_string(),
                value,
            });
            next = value + 1;
        }
    }
    assert!(!out.is_empty(), "{declaration} parsed to zero entries");
    out
}

/// Asserts a named C++ enumerator has the value the Rust side assumes.
fn assert_value(entries: &[CppEnumerator], name: &str, expected: i64) {
    let found = entries
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("{name} is gone from the vendored header"));
    assert_eq!(
        found.value, expected,
        "{name} moved to {} upstream, but this crate still uses {expected}",
        found.value
    );
}

#[test]
fn compaction_reason_matches_the_cpp_header() {
    let Some(header) = read_header("include/rocksdb/listener.h") else {
        return;
    };
    let cpp = parse_cpp_enum(&header, "enum class CompactionReason");

    // kNumOfReasons is upstream's own count of the reasons before it, so it
    // shifts the moment a reason is added. Checking it against our variant
    // catches an upstream insertion anywhere in the enum.
    assert_value(
        &cpp,
        "kNumOfReasons",
        DBCompactionReason::KNumOfReasons as i64,
    );
    assert_value(&cpp, "kUnknown", DBCompactionReason::KUnknown as i64);
    assert_value(
        &cpp,
        "kReadTriggered",
        DBCompactionReason::KReadTriggered as i64,
    );
    assert_value(&cpp, "kRefitLevel", DBCompactionReason::KRefitLevel as i64);
    assert_value(&cpp, "kFlush", DBCompactionReason::KFlush as i64);
}

#[test]
fn flush_reason_matches_the_cpp_header() {
    let Some(header) = read_header("include/rocksdb/listener.h") else {
        return;
    };
    let cpp = parse_cpp_enum(&header, "enum class FlushReason");

    // FlushReason has no count sentinel, so KUnknown sits one past the last
    // real reason. If upstream appends one, it lands on KUnknown and this
    // fails.
    let highest = cpp.iter().map(|e| e.value).max().expect("no entries");
    assert_eq!(
        DBFlushReason::KUnknown as i64,
        highest + 1,
        "upstream FlushReason now goes up to {highest}, so KUnknown collides with a real reason"
    );
    assert_value(&cpp, "kOthers", DBFlushReason::KOthers as i64);
    assert_value(
        &cpp,
        "kMemtableMaxRangeDeletions",
        DBFlushReason::KMemtableMaxRangeDeletions as i64,
    );
    assert_value(
        &cpp,
        "kCatchUpAfterErrorRecovery",
        DBFlushReason::KCatchUpAfterErrorRecovery as i64,
    );
}

#[test]
fn background_error_reason_matches_the_cpp_header() {
    let Some(header) = read_header("include/rocksdb/listener.h") else {
        return;
    };
    let cpp = parse_cpp_enum(&header, "enum class BackgroundErrorReason");

    let highest = cpp.iter().map(|e| e.value).max().expect("no entries");
    assert_eq!(
        DBBackgroundErrorReason::KUnknown as i64,
        highest + 1,
        "upstream BackgroundErrorReason grew, so KUnknown collides with a real reason"
    );
    assert_value(&cpp, "kFlush", DBBackgroundErrorReason::KFlush as i64);
    assert_value(
        &cpp,
        "kAsyncFileOpen",
        DBBackgroundErrorReason::KAsyncFileOpen as i64,
    );
}

#[test]
fn write_stall_condition_matches_the_cpp_header() {
    let Some(header) = read_header("include/rocksdb/types.h") else {
        return;
    };
    let cpp = parse_cpp_enum(&header, "enum class WriteStallCondition");

    assert_value(&cpp, "kDelayed", DBWriteStallCondition::KDelayed as i64);
    assert_value(&cpp, "kStopped", DBWriteStallCondition::KStopped as i64);
    // Upstream keeps kNormal last and says new conditions go before it.
    assert_value(&cpp, "kNormal", DBWriteStallCondition::KNormal as i64);
}

#[test]
fn status_severity_matches_the_cpp_header() {
    let Some(header) = read_header("include/rocksdb/status.h") else {
        return;
    };
    let cpp = parse_cpp_enum(&header, "enum Severity");

    assert_value(&cpp, "kNoError", StatusSeverity::KNoError as i64);
    assert_value(
        &cpp,
        "kUnrecoverableError",
        StatusSeverity::KUnrecoverableError as i64,
    );
    // kMaxSeverity is upstream's terminator, so it shifts if a severity is added.
    assert_value(&cpp, "kMaxSeverity", StatusSeverity::KMaxSeverity as i64);
}

#[test]
fn env_priority_matches_the_cpp_header() {
    let Some(header) = read_header("include/rocksdb/env.h") else {
        return;
    };
    let cpp = parse_cpp_enum(&header, "enum Priority");

    // TOTAL counts the pools rather than naming one, so a pool inserted
    // anywhere shifts it, exactly like the count sentinels above.
    assert_value(&cpp, "BOTTOM", EnvPriority::Bottom as i64);
    assert_value(&cpp, "LOW", EnvPriority::Low as i64);
    assert_value(&cpp, "HIGH", EnvPriority::High as i64);
    assert_value(&cpp, "USER", EnvPriority::User as i64);
    assert_value(&cpp, "TOTAL", EnvPriority::User as i64 + 1);
}

#[test]
fn file_type_matches_the_cpp_header() {
    let Some(header) = read_header("include/rocksdb/types.h") else {
        return;
    };
    let cpp = parse_cpp_enum(&header, "enum FileType");

    // Check every mapping. A reorder in the middle can leave a few anchors and
    // the highest value unchanged while still breaking raw-value decoding.
    assert_value(&cpp, "kWalFile", FileType::WalFile as i64);
    assert_value(&cpp, "kDBLockFile", FileType::DBLockFile as i64);
    assert_value(&cpp, "kTableFile", FileType::TableFile as i64);
    assert_value(&cpp, "kDescriptorFile", FileType::DescriptorFile as i64);
    assert_value(&cpp, "kCurrentFile", FileType::CurrentFile as i64);
    assert_value(&cpp, "kTempFile", FileType::TempFile as i64);
    assert_value(&cpp, "kInfoLogFile", FileType::InfoLogFile as i64);
    assert_value(&cpp, "kMetaDatabase", FileType::MetaDatabase as i64);
    assert_value(&cpp, "kIdentityFile", FileType::IdentityFile as i64);
    assert_value(&cpp, "kOptionsFile", FileType::OptionsFile as i64);
    assert_value(&cpp, "kBlobFile", FileType::BlobFile as i64);
    assert_value(
        &cpp,
        "kCompactionProgressFile",
        FileType::CompactionProgressFile as i64,
    );
    // FileType has no count sentinel, so also catch an appended variant.
    let highest = cpp.iter().map(|e| e.value).max().expect("no entries");
    assert_eq!(
        FileType::CompactionProgressFile as i64,
        highest,
        "upstream FileType now goes up to {highest}, past what this crate names"
    );
}

#[test]
fn stats_level_matches_the_cpp_header() {
    let Some(header) = read_header("include/rocksdb/statistics.h") else {
        return;
    };
    let cpp = parse_cpp_enum(&header, "enum StatsLevel");

    assert_value(&cpp, "kDisableAll", StatsLevel::DisableAll as i64);
    assert_value(
        &cpp,
        "kExceptHistogramOrTimers",
        StatsLevel::ExceptHistogramOrTimers as i64,
    );
    assert_value(&cpp, "kExceptTimers", StatsLevel::ExceptTimers as i64);
    assert_value(
        &cpp,
        "kExceptDetailedTimers",
        StatsLevel::ExceptDetailedTimers as i64,
    );
    assert_value(
        &cpp,
        "kExceptTimeForMutex",
        StatsLevel::ExceptTimeForMutex as i64,
    );
    assert_value(&cpp, "kAll", StatsLevel::All as i64);
}

#[test]
fn perf_level_matches_the_cpp_header_not_the_stale_c_api() {
    let Some(header) = read_header("include/rocksdb/perf_level.h") else {
        return;
    };
    let cpp = parse_cpp_enum(&header, "enum PerfLevel");

    // rocksdb_set_perf_level casts the int straight to PerfLevel, so this
    // header decides behavior. The c.h constants are missing kEnableWait and
    // kEnableTimeAndCPUTimeExceptForMutex and must not be used here.
    assert_value(&cpp, "kUninitialized", PerfStatsLevel::Uninitialized as i64);
    assert_value(&cpp, "kDisable", PerfStatsLevel::Disable as i64);
    assert_value(&cpp, "kEnableCount", PerfStatsLevel::EnableCount as i64);
    assert_value(&cpp, "kEnableWait", PerfStatsLevel::EnableWait as i64);
    assert_value(
        &cpp,
        "kEnableTimeExceptForMutex",
        PerfStatsLevel::EnableTimeExceptForMutex as i64,
    );
    assert_value(
        &cpp,
        "kEnableTimeAndCPUTimeExceptForMutex",
        PerfStatsLevel::EnableTimeAndCPUTimeExceptForMutex as i64,
    );
    assert_value(&cpp, "kEnableTime", PerfStatsLevel::EnableTime as i64);
    assert_value(&cpp, "kOutOfBounds", PerfStatsLevel::OutOfBound as i64);
}

#[test]
fn the_parser_resolves_implicit_and_explicit_values() {
    // Temperature is a good exercise: explicit, non-contiguous, hex values.
    let Some(header) = read_header("include/rocksdb/types.h") else {
        return;
    };
    let cpp = parse_cpp_enum(&header, "enum class Temperature");
    assert_value(&cpp, "kUnknown", 0);
    assert_value(&cpp, "kHot", 0x04);
    assert_value(&cpp, "kWarm", 0x08);
    assert_value(&cpp, "kCool", 0x0A);
    assert_value(&cpp, "kCold", 0x0C);
    assert_value(&cpp, "kIce", 0x10);
    // Implicit increment after an explicit hex value.
    assert_value(&cpp, "kLastTemperature", 0x11);
}

/// The `From<u32>` impls repeat the variant order as literals, so they can fall
/// out of step with the enum even when the enum itself matches C++. Decoding
/// each variant's own discriminant catches that without repeating the numbers.
#[test]
fn compaction_reason_decodes_every_variant() {
    let all = [
        DBCompactionReason::KUnknown,
        DBCompactionReason::KLevelL0filesNum,
        DBCompactionReason::KLevelMaxLevelSize,
        DBCompactionReason::KUniversalSizeAmplification,
        DBCompactionReason::KUniversalSizeRatio,
        DBCompactionReason::KUniversalSortedRunNum,
        DBCompactionReason::KFifomaxSize,
        DBCompactionReason::KFiforeduceNumFiles,
        DBCompactionReason::KFifottl,
        DBCompactionReason::KManualCompaction,
        DBCompactionReason::KFilesMarkedForCompaction,
        DBCompactionReason::KBottommostFiles,
        DBCompactionReason::KTtl,
        DBCompactionReason::KFlush,
        DBCompactionReason::KExternalSstIngestion,
        DBCompactionReason::KPeriodicCompaction,
        DBCompactionReason::KChangeTemperature,
        DBCompactionReason::KForcedBlobGc,
        DBCompactionReason::KRoundRobinTtl,
        DBCompactionReason::KRefitLevel,
        DBCompactionReason::KReadTriggered,
        DBCompactionReason::KNumOfReasons,
    ];
    for reason in all {
        let raw = reason as u32;
        assert_eq!(
            DBCompactionReason::from(raw),
            reason,
            "raw {raw} should decode to {reason:?}"
        );
    }
}

#[test]
fn flush_reason_decodes_every_variant() {
    let all = [
        DBFlushReason::KOthers,
        DBFlushReason::KGetLiveFiles,
        DBFlushReason::KShutDown,
        DBFlushReason::KExternalFileIngestion,
        DBFlushReason::KManualCompaction,
        DBFlushReason::KWriteBufferManager,
        DBFlushReason::KWriteBufferFull,
        DBFlushReason::KTest,
        DBFlushReason::KDeleteFiles,
        DBFlushReason::KAutoCompaction,
        DBFlushReason::KManualFlush,
        DBFlushReason::KErrorRecovery,
        DBFlushReason::KErrorRecoveryRetryFlush,
        DBFlushReason::KWalFull,
        DBFlushReason::KCatchUpAfterErrorRecovery,
        DBFlushReason::KMemtableMaxRangeDeletions,
    ];
    for reason in all {
        let raw = reason as u32;
        assert_eq!(
            DBFlushReason::from(raw),
            reason,
            "raw {raw} should decode to {reason:?}"
        );
    }
}

#[test]
fn file_type_decodes_every_variant() {
    let all = [
        FileType::WalFile,
        FileType::DBLockFile,
        FileType::TableFile,
        FileType::DescriptorFile,
        FileType::CurrentFile,
        FileType::TempFile,
        FileType::InfoLogFile,
        FileType::MetaDatabase,
        FileType::IdentityFile,
        FileType::OptionsFile,
        FileType::BlobFile,
        FileType::CompactionProgressFile,
    ];
    for file_type in all {
        let raw = file_type as i32;
        assert_eq!(
            FileType::from(raw),
            file_type,
            "raw {raw} should decode to {file_type:?}"
        );
    }
    // Anything past the last upstream value falls into the catch-all.
    assert_eq!(FileType::from(FileType::Unknown as i32), FileType::Unknown);
}

use crate::ffi;
use libc::c_int;

#[derive(Debug, Clone)]
pub struct NameParseError;
impl core::fmt::Display for NameParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unrecognized name")
    }
}

impl std::error::Error for NameParseError {}

// Helper macro to generate iterable nums that translate into static strings mapped from the cpp
// land.
macro_rules! iterable_named_enum {
    (
    $(#[$m:meta])*
    $type_vis:vis enum $typename:ident {
        $(
            $(#[$variant_meta:meta])*
            $variant:ident($variant_str:literal) $(= $value:expr)?,
        )+
    }
    ) => {
        // Main Type
        #[allow(clippy::all)]
        $(#[$m])*
        $type_vis enum $typename {
            $(
            $(#[$variant_meta])*
            $variant$( = $value)?,
            )+
        }

        impl $typename {
            #[doc = "The corresponding rocksdb string identifier for this variant"]
            pub const fn name(&self) -> &'static str {
                match self {
                    $(
                        $typename::$variant => $variant_str,
                    )+
                }
            }
            pub fn iter() -> ::core::slice::Iter<'static, $typename> {
                static VARIANTS: &'static [$typename] = &[
                    $(
                        $typename::$variant,
                    )+
                ];
                VARIANTS.iter()
            }
        }


        #[automatically_derived]
        impl ::core::str::FromStr for $typename {
            type Err = NameParseError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $(
                        $variant_str => Ok($typename::$variant),
                    )+
                    _ => Err(NameParseError),
                }
            }
        }

        #[automatically_derived]
        impl ::core::fmt::Display for $typename {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                self.name().fmt(f)
            }
        }
    };
}

/// How much statistics detail to collect, trading overhead for visibility.
///
/// The levels are ordered, and each one adds to the one before it. The
/// discriminants come from the C API rather than being written out here, because
/// the setter passes the value straight through and a mismatch would silently
/// select a different level instead of failing.
///
/// RocksDB also defines `kExceptTickers`, which has the same value as
/// `kDisableAll` and so cannot be a separate variant here. [`DisableAll`] is that
/// value.
///
/// [`DisableAll`]: StatsLevel::DisableAll
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
// MSVC types an anonymous C enum as signed int where clang picks unsigned, so
// these constants are i32 on Windows and u32 everywhere else. The cast is needed
// on Windows and redundant on the platforms clippy runs on.
#[allow(clippy::unnecessary_cast)]
pub enum StatsLevel {
    /// Collect nothing. Also what RocksDB reports when no statistics object has
    /// been installed, so this does not distinguish "turned off" from "never
    /// turned on".
    DisableAll = ffi::rocksdb_statistics_level_disable_all as u32,
    /// Skip histograms and timers.
    ExceptHistogramOrTimers = ffi::rocksdb_statistics_level_except_histogram_or_timers as u32,
    /// Collect histograms, skip timers.
    ExceptTimers = ffi::rocksdb_statistics_level_except_timers as u32,
    /// Collect everything except time spent inside a mutex lock and time spent on
    /// compression.
    ExceptDetailedTimers = ffi::rocksdb_statistics_level_except_detailed_timers as u32,
    /// Collect everything except the counters that need the time from inside the
    /// mutex lock.
    ExceptTimeForMutex = ffi::rocksdb_statistics_level_except_time_for_mutex as u32,
    /// Collect everything, including how long mutex operations take. Where reading
    /// the clock is expensive this can limit scalability across threads,
    /// especially for writes.
    All = ffi::rocksdb_statistics_level_all as u32,
}

impl StatsLevel {
    /// Decodes a raw `rocksdb::StatsLevel`.
    ///
    /// `None` for a value this crate has no variant for, which RocksDB's own
    /// clamping in `rocksdb_options_set_statistics_level` should prevent.
    pub(crate) fn try_from_raw(raw: c_int) -> Option<Self> {
        match raw {
            n if n == Self::DisableAll as c_int => Some(Self::DisableAll),
            n if n == Self::ExceptHistogramOrTimers as c_int => Some(Self::ExceptHistogramOrTimers),
            n if n == Self::ExceptTimers as c_int => Some(Self::ExceptTimers),
            n if n == Self::ExceptDetailedTimers as c_int => Some(Self::ExceptDetailedTimers),
            n if n == Self::ExceptTimeForMutex as c_int => Some(Self::ExceptTimeForMutex),
            n if n == Self::All as c_int => Some(Self::All),
            _ => None,
        }
    }
}

include!("statistics_enum_ticker.rs");
include!("statistics_enum_histogram.rs");

pub struct HistogramData {
    pub(crate) inner: *mut ffi::rocksdb_statistics_histogram_data_t,
}

impl HistogramData {
    pub fn new() -> HistogramData {
        HistogramData::default()
    }
    pub fn median(&self) -> f64 {
        unsafe { ffi::rocksdb_statistics_histogram_data_get_median(self.inner) }
    }
    pub fn average(&self) -> f64 {
        unsafe { ffi::rocksdb_statistics_histogram_data_get_average(self.inner) }
    }
    pub fn p95(&self) -> f64 {
        unsafe { ffi::rocksdb_statistics_histogram_data_get_p95(self.inner) }
    }
    pub fn p99(&self) -> f64 {
        unsafe { ffi::rocksdb_statistics_histogram_data_get_p99(self.inner) }
    }
    pub fn max(&self) -> f64 {
        unsafe { ffi::rocksdb_statistics_histogram_data_get_max(self.inner) }
    }
    pub fn min(&self) -> f64 {
        unsafe { ffi::rocksdb_statistics_histogram_data_get_min(self.inner) }
    }
    pub fn sum(&self) -> u64 {
        unsafe { ffi::rocksdb_statistics_histogram_data_get_sum(self.inner) }
    }
    pub fn count(&self) -> u64 {
        unsafe { ffi::rocksdb_statistics_histogram_data_get_count(self.inner) }
    }
    pub fn std_dev(&self) -> f64 {
        unsafe { ffi::rocksdb_statistics_histogram_data_get_std_dev(self.inner) }
    }
}

impl Default for HistogramData {
    fn default() -> Self {
        let histogram_data_inner = unsafe { ffi::rocksdb_statistics_histogram_data_create() };
        assert!(
            !histogram_data_inner.is_null(),
            "Could not create RocksDB histogram data"
        );

        Self {
            inner: histogram_data_inner,
        }
    }
}

impl Drop for HistogramData {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_statistics_histogram_data_destroy(self.inner);
        }
    }
}

#[test]
fn sanity_checks() {
    let want = "rocksdb.async.read.bytes";
    assert_eq!(want, Histogram::AsyncReadBytes.name());

    let want = "rocksdb.block.cache.index.miss";
    assert_eq!(want, Ticker::BlockCacheIndexMiss.to_string());

    // assert enum lengths
    assert_eq!(Ticker::iter().count(), 263 /* TICKER_ENUM_MAX */);
    assert_eq!(Histogram::iter().count(), 80 /* HISTOGRAM_ENUM_MAX */);
}

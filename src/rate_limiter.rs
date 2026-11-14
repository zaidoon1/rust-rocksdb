//! A rate limiter that can be shared by many DBs and retuned at runtime.

use std::ptr::NonNull;
use std::sync::Arc;

use libc::c_int;

use crate::{RateLimiterMode, ffi};

pub(crate) struct RateLimiterWrapper {
    pub(crate) inner: NonNull<ffi::rocksdb_ratelimiter_t>,
}

// RocksDB's GenericRateLimiter is internally synchronized: `SetBytesPerSecond`
// and the getters are safe to call from any thread while DBs use it.
unsafe impl Send for RateLimiterWrapper {}
unsafe impl Sync for RateLimiterWrapper {}

impl Drop for RateLimiterWrapper {
    fn drop(&mut self) {
        // Releases only this handle's reference. Every `Options` the limiter
        // was set on holds its own C++ reference, so DBs still using it are
        // unaffected.
        unsafe {
            ffi::rocksdb_ratelimiter_destroy(self.inner.as_ptr());
        }
    }
}

/// A RocksDB rate limiter that controls the I/O rate of flush and compaction.
///
/// Unlike [`Options::set_ratelimiter`](crate::Options::set_ratelimiter), which
/// creates a private limiter per `Options`, a `RateLimiter` is a handle that
/// can be set on many `Options` with
/// [`Options::set_rate_limiter`](crate::Options::set_rate_limiter). All DBs
/// opened with those options then draw from the same I/O budget, and the
/// budget can be changed at runtime with
/// [`set_bytes_per_second`](RateLimiter::set_bytes_per_second).
///
/// Cloning is cheap and yields another handle to the same limiter.
///
/// See the [RocksDB wiki](https://github.com/facebook/rocksdb/wiki/Rate-Limiter)
/// for details.
///
/// # Examples
///
/// ```
/// use rust_rocksdb::{DB, Options, RateLimiter};
/// # let dir = tempfile::tempdir().unwrap();
///
/// // 64 MiB/s shared by every DB opened below.
/// let limiter = RateLimiter::new(64 << 20, 100_000, 10);
///
/// let mut opts = Options::default();
/// opts.create_if_missing(true);
/// opts.set_rate_limiter(&limiter);
///
/// let db1 = DB::open(&opts, dir.path().join("db1")).unwrap();
/// let db2 = DB::open(&opts, dir.path().join("db2")).unwrap();
///
/// // Halve the budget for both DBs at once.
/// limiter.set_bytes_per_second(32 << 20);
/// assert_eq!(limiter.get_bytes_per_second(), 32 << 20);
/// # drop((db1, db2));
/// ```
#[derive(Clone)]
pub struct RateLimiter(pub(crate) Arc<RateLimiterWrapper>);

impl RateLimiter {
    /// Creates a rate limiter that limits writes (flush and compaction) to
    /// `rate_bytes_per_sec`.
    ///
    /// rate_bytes_per_sec: this is the only parameter you want to set most of the
    /// time. It controls the total write rate of compaction and flush in bytes per
    /// second. Currently, RocksDB does not enforce rate limit for anything other
    /// than flush and compaction, e.g. write to WAL.
    ///
    /// refill_period_us: this controls how often tokens are refilled. For example,
    /// when rate_bytes_per_sec is set to 10MB/s and refill_period_us is set to
    /// 100ms, then 1MB is refilled every 100ms internally. Larger value can lead to
    /// burstier writes while smaller value introduces more CPU overhead.
    /// The default should work for most cases.
    ///
    /// fairness: RateLimiter accepts high-pri requests and low-pri requests.
    /// A low-pri request is usually blocked in favor of hi-pri request. Currently,
    /// RocksDB assigns low-pri to request from compaction and high-pri to request
    /// from flush. Low-pri requests can get blocked if flush requests come in
    /// continuously. This fairness parameter grants low-pri requests permission by
    /// 1/fairness chance even though high-pri requests exist to avoid starvation.
    /// You should be good by leaving it at default 10.
    pub fn new(rate_bytes_per_sec: i64, refill_period_us: i64, fairness: i32) -> Self {
        let inner = NonNull::new(unsafe {
            ffi::rocksdb_ratelimiter_create(rate_bytes_per_sec, refill_period_us, fairness)
        })
        .expect("Could not create RocksDB rate limiter");
        Self::from_inner(inner)
    }

    /// Creates an auto-tuned rate limiter that limits writes.
    ///
    /// The limiter adjusts its rate within
    /// `[rate_bytes_per_sec / 20, rate_bytes_per_sec]` according to recent
    /// demand for background I/O. See [`RateLimiter::new`] for the parameters.
    pub fn new_auto_tuned(rate_bytes_per_sec: i64, refill_period_us: i64, fairness: i32) -> Self {
        let inner = NonNull::new(unsafe {
            ffi::rocksdb_ratelimiter_create_auto_tuned(
                rate_bytes_per_sec,
                refill_period_us,
                fairness,
            )
        })
        .expect("Could not create RocksDB rate limiter");
        Self::from_inner(inner)
    }

    /// Creates a rate limiter with an explicit mode.
    ///
    /// mode: Mode indicates which types of operations count against the limit.
    ///
    /// auto_tuned: Enables dynamic adjustment of rate limit within the range
    ///              `[rate_bytes_per_sec / 20, rate_bytes_per_sec]`, according to
    ///              the recent demand for background I/O.
    ///
    /// See [`RateLimiter::new`] for the other parameters.
    pub fn new_with_mode(
        rate_bytes_per_sec: i64,
        refill_period_us: i64,
        fairness: i32,
        mode: RateLimiterMode,
        auto_tuned: bool,
    ) -> Self {
        let inner = NonNull::new(unsafe {
            ffi::rocksdb_ratelimiter_create_with_mode(
                rate_bytes_per_sec,
                refill_period_us,
                fairness,
                mode as c_int,
                auto_tuned,
            )
        })
        .expect("Could not create RocksDB rate limiter");
        Self::from_inner(inner)
    }

    fn from_inner(inner: NonNull<ffi::rocksdb_ratelimiter_t>) -> Self {
        RateLimiter(Arc::new(RateLimiterWrapper { inner }))
    }

    /// Changes the rate limit in bytes per second.
    ///
    /// The new rate applies immediately to every DB that shares this limiter.
    ///
    /// On an auto-tuned limiter this only overrides the current rate until the
    /// next tuning step (every 100 refill periods). The tuning range stays
    /// `[rate_bytes_per_sec / 20, rate_bytes_per_sec]` from construction, and
    /// the tuner moves the rate within it based on demand. To control the rate
    /// from outside, use a limiter that is not auto-tuned.
    ///
    /// # Panics
    ///
    /// Panics if `bytes_per_second` is not positive. RocksDB requires a
    /// positive rate and does not check it in release builds.
    pub fn set_bytes_per_second(&self, bytes_per_second: i64) {
        assert!(
            bytes_per_second > 0,
            "rate limiter bytes_per_second must be positive, got {bytes_per_second}"
        );
        unsafe {
            ffi::rust_rocksdb_ratelimiter_set_bytes_per_second(
                self.0.inner.as_ptr(),
                bytes_per_second,
            );
        }
    }

    /// Returns the current rate limit in bytes per second.
    ///
    /// For an auto-tuned limiter this is the rate currently in effect, which
    /// moves within the tuning range.
    pub fn get_bytes_per_second(&self) -> i64 {
        unsafe { ffi::rust_rocksdb_ratelimiter_get_bytes_per_second(self.0.inner.as_ptr()) }
    }

    /// Returns the total number of bytes that have gone through the limiter,
    /// summed over all priorities and all DBs sharing it.
    pub fn get_total_bytes_through(&self) -> i64 {
        unsafe { ffi::rust_rocksdb_ratelimiter_get_total_bytes_through(self.0.inner.as_ptr()) }
    }

    /// Returns the total number of requests that have gone through the
    /// limiter, summed over all priorities and all DBs sharing it.
    pub fn get_total_requests(&self) -> i64 {
        unsafe { ffi::rust_rocksdb_ratelimiter_get_total_requests(self.0.inner.as_ptr()) }
    }
}

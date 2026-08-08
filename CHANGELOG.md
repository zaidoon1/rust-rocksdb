# Changelog

## 0.52.0 (2026-08-08)

This release contains breaking API changes, marked `fix!` and `feat!`
below, and needs a minor version bump. The two most likely to reach you
without a compiler error: the default column-family compression is now
LZ4 rather than Snappy, and `Ticker`/`Histogram` discriminants have
shifted.

### Memory safety

- fix: stop handing `malloc`ed buffers to Rust's allocator.
  `WriteBatchWithIndex::get_from_batch`, `get_from_batch_cf`,
  `get_from_batch_and_db` and `get_from_batch_and_db_cf` wrapped the
  `char*` from the C API in a `Vec` with `Vec::from_raw_parts`. It comes
  from `CopyString` (`db/c.cc`), which is plain `malloc`, so Rust's
  global allocator was being asked to free memory it never allocated.
  Works by accident with the system allocator, corrupts the heap under
  any `#[global_allocator]` or on Windows. Also leaked the `malloc(0)`
  block for every empty value.
- fix!: `WriteBatchWithIndex::iterator_with_base{,_cf}` returned an
  iterator tied only to the base iterator, so the batch could be dropped
  while the iterator was still reading its skip list. The iterator now
  borrows the batch, which is a source-breaking signature change.
- fix!: `WriteBatchWithIndex::get_pinned_from_batch_and_db{,_cf}` let
  elision tie the pinned slice to the batch rather than to the DB whose
  block cache it pins. Source-breaking in both directions: the slice may
  now outlive the batch, and may no longer outlive the DB.
- fix!: `Snapshot::iterator` and `iterator_opt` returned the DB lifetime
  instead of the `&self` borrow, so the iterator could outlive the
  snapshot. Every other iterator constructor in the file already got
  this right. Source-breaking for code that relied on the longer
  lifetime.
- fix: the callback logger ran `str::from_utf8_unchecked` over RocksDB
  log text and transmuted the level into a `#[repr(i32)]` enum. Paths
  reach RocksDB through `OsStr::as_bytes` and are not UTF-8 validated.
  It also took a `&mut` to the shared callback on every line. RocksDB
  logs from several background threads at once, so that minted aliasing
  `&mut`s to one object, which is undefined behaviour on its own.
  Both loggers now map an unrecognised level to `Info` instead of
  panicking out of an `extern "C"` frame, which aborts the process.
- fix!: `SstFileWriter::open` took `&self` while mutating the writer,
  which combined with the `Sync` impl let two threads race on it. It now
  takes `&mut self`, so callers need a `mut` binding.
- fix: guard `slice::from_raw_parts` on zero-length keys and values in
  the iterator accessors, `DBPinnableSlice::deref`, `CSlice::as_ref`,
  the `prefix_exists{,_cf}_opt` key reads and both logger callbacks.
  Empty keys are legal in RocksDB.
- fix: `prefix_exists` held a `RefCell` borrow across an FFI call that
  can re-enter through a user comparator, so a re-entrant probe hit
  `BorrowMutError` and aborted from an `extern "C"` frame.
- fix!: `SnapshotWithThreadMode`'s `Send`/`Sync` impls now require
  `D: Sync`. `Transaction` is `Send` but not `Sync`, so a
  transaction-backed snapshot was `Send + Sync` when it had no right to
  be. Source-breaking for code that moved one across threads.
- fix: bounds-check `rust_rocksdb_pinnable_batch_get` instead of
  asserting. The vendored build defines `NDEBUG`, so the asserts
  compiled away and left an unchecked `std::vector` index feeding a
  pointer and length into `slice::from_raw_parts`. It reports the new
  `rust_rocksdb_pinnable_batch_out_of_range` status, and the Rust side
  turns an unexpected status into an `Error` rather than panicking, so
  a System backend built against a skewed extension cannot abort the
  process. `multi_get_pinned` likewise returns an `Error` instead of
  panicking when RocksDB hands back a null pinned batch.
- fix: report an error rather than silent success when the C extension
  cannot allocate an error string. It left `errptr` null, which Rust
  reads as ok, so a failed vectored `WriteBatch` operation looked like it
  had been applied.
- fix: release the per-key error strings from a System-backend batched
  MultiGet if draining them into the batch throws partway through.

### Correctness

- fix!: `DB::get_approximate_sizes{,_cf}` now return
  `Result<Vec<u64>, Error>`. The error was ignored and leaked, and a
  failed call returned zeros the caller could not tell apart from empty
  ranges.
- fix!: `Snapshot::sequence_number` now returns `Option<u64>`. A
  transaction started without `TransactionOptions::set_snapshot(true)`
  gets a snapshot wrapping a null pointer, which the C getter
  dereferences unconditionally.
- fix: saturate rather than wrap when converting a TTL to seconds. A TTL
  above `i32::MAX` seconds truncated, so "effectively never" became one
  second and the data was compacted away. Affects `DB::open_with_ttl`
  and `ColumnFamilyTtl::Duration`/`SameAsDb`.
- fix: `drop_cf` destroyed the column family handle even when
  `rocksdb_drop_column_family` failed, leaving the column family in the
  DB with no handle left to reach it.

### Build configuration

- perf: enable hardware CRC32C on aarch64. The `-march=...+crc` flag was
  gated on `CARGO_CFG_TARGET_FEATURE`, which is only `neon` on stock
  `aarch64-unknown-linux-gnu` and `aarch64-linux-android`. On those
  targets `HAVE_ARM64_CRC` went undefined, `util/crc32c_arm64.cc`
  compiled to an empty object, and every block read and write used the
  software CRC table. `aarch64-apple-darwin` lists `crc` and was
  unaffected. RocksDB picks the ARM path at runtime via
  `getauxval(AT_HWCAP)`, so the flag is now gated on the compiler
  accepting it, as upstream does. Scoped to the two crc32c
  translation units so folly's `F14IntrinsicsMode` stays consistent with
  the prebuilt libfolly in coroutines builds, and applied regardless of
  `-Ctarget-cpu`. MSVC is excluded: it has no `-march` and would need
  `/arch:` instead.
- perf: build the vendored snappy with its accelerated paths on. snappy
  expects a generated `config.h`; we never produced one or defined
  `HAVE_CONFIG_H`, so every `#if HAVE_*` was 0 and the NEON/SSSE3
  `IncrementalCopy`, `__builtin_expect`, `__builtin_ctz` and
  `__builtin_prefetch` were all disabled. snappy is a default feature.
  `SNAPPY_HAVE_SSSE3` now carries `-mssse3` with it. Defining it alone
  made `snappy-internal.h` call `_mm_shuffle_epi8` from a function
  compiled without the ISA, which is a compile error, not a slow path, so
  x86_64 builds with `-Ctarget-cpu=native` (or `haswell`, or
  `x86-64-v2` and newer) failed. The snappy build has its own
  `cc::Build` and never went through `apply_x86`, so nothing else
  supplied the flag. `SNAPPY_HAVE_NEON` now also requires the `neon`
  target feature on top of the aarch64 check, for the few aarch64
  targets that build without SIMD. It stays gated on the architecture
  too, because snappy's NEON path uses AArch64-only intrinsics and
  `neon` is a target feature on 32-bit ARM as well.
- perf: pass `-msse4.2` and `-mbmi2` to the vendored snappy build when
  the target has them. snappy derives `SNAPPY_HAVE_X86_CRC32` and
  `SNAPPY_HAVE_BMI2` from `__SSE4_2__`/`__BMI2__`, and nothing was
  passing the flags that define those, so the CRC32 compressor hash and
  `_bzhi_u32` were off on every x86 build.
- perf: pass `-fno-builtin-memcmp` on non-MSVC, as upstream does, so key
  comparisons use glibc's SIMD `memcmp` rather than GCC's inline
  expansion.
- perf: forward the `bmi2` and `popcnt` target features to the C++
  build; `-Ctarget-feature=+bmi2` previously had no effect on it.
- fix: make the `rtti` feature work, and turn RTTI off when it is not
  asked for. The build defined `USE_RTTI`, which is the CMake option
  name; RocksDB's sources test `ROCKSDB_USE_RTTI`, so the feature did
  nothing. Builds without it now pass `-fno-rtti` (`/GR-` on MSVC)
  instead of leaving the compiler default of RTTI on, which changes the
  object code every default build produces. `coroutines` implies RTTI,
  because folly needs it.
- fix: pass `/MT` to the System-backend extension build under
  `mt_static`. The rest of the native build honoured the feature, so
  that one translation unit was compiled against a different CRT than
  everything it links with.
- fix: don't define `HAVE_UINT128_EXTENSION` on 32-bit targets. GCC and
  Clang only provide `__int128` on 64-bit, and `util/math128.h` aliases
  it directly, so `i686` and `armv7` could not compile.
- fix: stop defining `NIOSTATS_CONTEXT`/`NPERF_CONTEXT` on iOS, tvOS and
  watchOS. They compile `PerfContext` and `IOStatsContext` out, so the
  whole `perf` API silently returned zeros. Upstream defaults both on.
- fix: define `ROCKSDB_AUXV_GETAUXVAL_PRESENT` on Android, without which
  the aarch64 CRC32 runtime check always fails.
- fix: define `NDEBUG` when compiling `c_api_extensions.cc` for the
  System backend, and give it the same dev-profile defaults as the rest
  of the native build. One source file was getting two different
  `assert` and `rocksdb/slice.h` inline-function semantics depending on
  which backend built it, against a prebuilt librocksdb that was itself
  built with `NDEBUG`.
- fix: allow `ROCKSDB_COMPILE=1` on FreeBSD instead of rejecting it up
  front, and skip jemalloc there. This reverses the FreeBSD note in
  0.49.0.
- fix: drop the local C-API extensions that upstream now provides
  itself. `rocksdb_readoptions_{set,get}_optimize_multiget_for_io`,
  `rocksdb_block_based_options_set_uniform_cv_threshold`, the
  `rocksdb_block_based_table_index_block_search_type_auto` enum value,
  `rocksdb_options_{set,get}_memtable_batch_lookup_optimization` and
  `rocksdb_compactoptions_{set,get}_blob_garbage_collection_age_cutoff`
  all landed upstream with the same signatures. Keeping our copies made
  the enum a redefinition against `c.h` and the functions duplicate
  definitions against `db/c.cc`. The Rust side is unchanged: the same
  symbol names now resolve to upstream. This does narrow the System
  backend, which links a user-supplied prebuilt librocksdb. Our copies
  stood in for these on builds that predate them; upstream added three
  in 11.4.0 and the last in 11.6.0, so the System backend now needs
  11.6 or newer. An older one fails at link time with undefined
  symbols, not silently.
- build: the `coroutines` feature now needs liburing 2.15. RocksDB 11.8
  pins a folly commit whose `IoUringZeroCopyBufferPool.cpp` expects the
  system headers to declare the io_uring zero-copy receive UAPI instead
  of declaring it itself, and 2.15 is the first release with the zcrx
  control structs and `io_uring_zcrx_ifq_reg::rx_buf_len`. No distro
  ships it yet, so `scripts/build_folly.sh` builds it from source and
  puts it on the include and link paths, which keeps RocksDB's io_uring
  code and the prebuilt libfolly on one liburing.
- build: move `tikv-jemalloc-sys` from 0.6 to 0.7, which carries
  jemalloc 5.3.0 to 5.3.1. The crate sets `links = "jemalloc"`, so Cargo
  permits only one version of it in a dependency graph. A downstream
  still on 0.6 through another crate has to move too; there is no
  version of this that resolves.
- build: link fast_float instead of double-conversion under
  `coroutines`. The folly commit RocksDB 11.8 pins swapped the two in
  its getdeps dependency list, so the build looked for a directory folly
  no longer installs. fast_float is header-only and needs no link
  directives, and `libdouble-conversion-dev` is no longer a build
  dependency.

### Performance

- perf: reduce bundled RocksDB development build size by defaulting its
  native compilation to `opt-level = 1` without debug information.
  Set `ROCKSDB_NATIVE_DEBUG=1` to preserve Cargo's native debug settings.
- perf: `prefix_exists` and `prefix_exists_cf` no longer allocate once
  warm. They reuse a thread-local `ReadOptions` to avoid allocating and
  then reallocated both iterate bounds on every call through
  `set_iterate_range(PrefixRange(..))`, which returns owned `Vec`s.
- perf: read `DBPinnableSlice`'s pointer and length once at construction
  instead of calling into C on every deref, index and `as_ref`.
- perf: reuse thread-local defaults in `DB::flush`,
  `TransactionDB::transaction` and
  `OptimisticTransactionDB::transaction`, which each built a fresh C++
  options object per call.
- perf: `multi_get_pinned{,_opt}` no longer collects the keys before
  deciding whether the batch is worth it. A single key was paying for a
  key vector on top of the point lookup it falls back to.
- perf: `#[inline]` on the non-generic accessors a downstream crate
  can't inline without LTO: `AsColumnFamilyRef::inner`, `DBInner::inner`,
  the `DBPinnableSlice`/`DBPinnableBatch` accessors, `MergeOperands` and
  its iterator, and `PerfContext::reset`/`metric`.
- perf: add allocation-free iterator callbacks, reusable snapshot read
  options, native batched pinned reads, and a batch-owned pinned result
  type.
- perf: add slice-based vectored `WriteBatch` operations that avoid Rust
  key and value concatenation and return RocksDB errors.
- perf: use RocksDB's integer property API when available, reuse
  thread-local `PerfContext` wrappers safely, and avoid per-iterator
  `ReadOptions` allocations when creating iterators for multiple column
  families.
- feat: expose `open_files_async`, cache occupancy metrics, and detailed
  write buffer manager memory accounting.
- fix!: align `PerfStatsLevel` with RocksDB. The enum was missing
  `EnableWait`, so every level above it was off by one and RocksDB was
  given a different level than the caller asked for. Callers now get the
  level they named, which for `EnableTimeAndCPUTimeExceptForMutex` means
  the per-operation CPU-time clock reads it always implied. Adding the
  variant in the middle renumbers `EnableTimeExceptForMutex` and
  everything above it, so a stored or transmitted numeric level from an
  older version now means something else.

### Features

- feat(librocksdb-sys): upgrade the bundled RocksDB submodule to 11.8.1.
  Adds 28 tickers, 12 histograms and the `BlobCacheReadByte` perf metric
  to the generated enums. Upstream's `v11.8.0` and `v11.8.1` tags both
  point at this commit, whose `version.h` reads 11.8.1, so that is what
  the library reports.
- feat!: the default compression for column families that never call
  `set_compression_type` is now LZ4 rather than Snappy, an upstream
  change in 11.5.0. It affects newly written SST files only; existing
  data stays readable, since RocksDB picks the decompressor per block.
  If LZ4 is not compiled in, the fallback order is LZ4, Snappy, then no
  compression, so a `--no-default-features` build without `lz4` writes
  uncompressed rather than falling over. Other upstream behaviour
  changes worth reading before upgrading:
  `CompressionOptions::parallel_threads` is now ignored for the fast
  built-in compressors, and a non-default `compression_opts.level` now
  selects between the LZ4 and LZ4HC variants.
- feat!: `Ticker` and `Histogram` gained variants in the middle, not
  just at the end. `Ticker` goes from 235 to 263 and `Histogram` from
  68 to 80, which shifts the discriminant of 149 existing `Ticker`
  variants and 1 `Histogram` variant. Nothing is removed or renamed.
  Neither enum is `#[non_exhaustive]`, so an exhaustive `match` on
  either, or on `PerfMetric`, stops compiling until the new variants
  are handled. Anything that persisted or wire-encoded the numeric
  value of a ticker will not line up with the new numbering.
- feat!: `set_open_files_async` returns `Result<(), Error>`. It has to
  report that the linked RocksDB does not support the option, which it
  previously had no way to say.
- feat: new public API, all covered by semver: `batched_multi_get_pinned`
  and `batched_multi_get_pinned_opt` for one native `MultiGet` over the
  default column family, returning the batch-owned `DBPinnableBatch`
  and its iterator; `SnapshotReadOptions` and `Snapshot::read_options`
  for reusing one snapshot-pinned `ReadOptions` across reads;
  `try_for_each_ref` on the iterators, which hands out borrowed key and
  value slices instead of allocating a pair of `Box<[u8]>` per row;
  `perf::with_thread_local` for a reused `PerfContext`; and
  `raw_iterators_cf` for creating iterators over several column
  families off one `ReadOptions`, which they share through an `Arc`
  because RocksDB's `DBIter` keeps raw `Slice*` into it for the iterate
  bounds and re-reads them on `Refresh`.
- feat: `multi_get_pinned{,_opt}` now issues a single native `MultiGet`
  for two or more keys instead of a sequential `get_pinned` each. The
  results and their order are unchanged. `multi_get_pinned_cf{,_opt}`
  still reads key by key, because the batched C API takes one column
  family for the whole call.
- feat: expose `disable_file_deletions` and `enable_file_deletions` on
  `OptimisticTransactionDB`.
- feat: the `valgrind` feature now does something. It was declared and
  wired to nothing; it defines `ROCKSDB_VALGRIND_RUN`, which RocksDB
  uses to skip work that does not finish in reasonable time under
  Memcheck. Build with it before running the suite under Valgrind.
- feat: re-export the sys crate as `ffi_raw` under the `raw-ptr` feature.
  `AsRawPtr` hands out `ffi::rocksdb_t` and friends, but the sys crate was
  only imported privately, so callers had to add `rust-librocksdb-sys` as a
  direct dependency and keep its version in lockstep by hand. Nothing in
  `ffi_raw` is covered by semver.

### Documentation

- fix: stop rebuilding RocksDB on docs.rs. `build.rs` now honours the
  `DOCS_RS` environment variable and skips the C++ compile, since rustdoc
  never links and only needs `bindings.rs` to exist. Documenting the sys
  crate goes from 83 seconds to 6 locally, against a docs.rs budget of 15
  minutes and no network, and a docs.rs failure is only visible after
  publishing.
- docs: declare the docs.rs feature set explicitly and pass `--cfg docsrs`,
  so gated items carry "available on crate feature" badges. Not
  `all-features`, which would pull in `coroutines` and need a folly install.
- docs: correct seven documented option defaults that disagreed with the
  bundled RocksDB. `set_compression_type` is LZ4, not Snappy, as of RocksDB
  11.5.0. `set_paranoid_checks` and `set_level_compaction_dynamic_level_bytes`
  are on, not off. `set_level_zero_stop_writes_trigger` is 36, not 24.
  `set_format_version` is 7, not 6, and the linked version list pointed at
  RocksDB 8.6.7, which predates format 7. `set_max_manifest_file_size` is a
  1 GiB floor under an auto-tuned limit, not a hard MAX_INT that disables
  rollover. The deprecated `set_max_background_compactions` and
  `set_max_background_flushes` are -1, derived from `max_background_jobs`,
  not 1. `Options::default()` is RocksDB's own defaults, so these were wrong
  about shipped behaviour rather than about a crate-level override.

### Packaging

- fix: stop publishing files that nothing compiles. The `exclude` list in
  `rust-librocksdb-sys` relied on `*/tests`, but RocksDB keeps its tests in
  `*_test.cc` next to the sources, so that pattern matched none of them.
  0.47.0 shipped 1879 files against the 343 the build actually compiles:
  222 test files, the 444-file Java binding tree, db_stress_tool,
  buckifier, microbench, fuzz, build_tools, cmake, and all of `tools` bar
  the one file that is built. Also dropped snappy's googletest and
  benchmark submodules, which only appear in a recursive clone and would
  have added another 398. This release ships 1125 files and 3.7 MiB
  compressed, down from 6.1 MiB, which every downstream build unpacks and
  hashes.

### Continuous integration

- fix: the `-Ctarget-cpu=native` job cached `target/` and `~/.cargo/bin`
  across the hosted runner fleet, so dependency rlibs compiled under one
  machine's `native` were restored onto a narrower one and the
  `librocksdb-sys` build script died with SIGILL. It now caches only the
  registry and git checkouts, nothing holding machine code. Keying on the
  detected CPU instead would fragment the cache across the fleet to save a
  job that barely benefits, since the uncached RocksDB C++ build dominates
  its runtime.
- fix: `save-if` is not an input to `setup-rust-toolchain`, so every run
  printed `Unexpected input(s) 'save-if'` and saved a cache from every pull
  request, which is how the poisoned tuned-CPU entry was written. Renamed
  to `cache-save-if` at all 12 sites, and added to the doc-check, doctest
  and clippy jobs, which had no gating at all.
- ci: verify the published tarball. `cargo package` now runs in CI and
  compiles RocksDB from the unpacked archive, because no other job builds
  what users actually download. It also asserts the tarball stays trimmed
  and that the pieces the build needs, gtest's fused source and
  `test_util`, are still in it. Trimming one glob too far breaks every
  downstream build and cannot be undone without publishing a new version.
  The job checks out submodules recursively, which is how it caught snappy's
  nested test dependencies that a shallow clone hides.
- ci: track stable in `rust-toolchain.toml` instead of pinning it to the
  MSRV, and add an `MSRV` job that reads `rust-version` from Cargo.toml so
  the two cannot drift. Pinning meant no job ever built on current stable,
  so new lints and toolchain breakage stayed invisible until the pin moved
  and then arrived together. Moving to 1.97.1 surfaced three clippy
  findings, now fixed: `libc::strlen` on a `CStr`, a reference cast to a raw
  pointer, and a redundant reference in an `assert!`.
- ci: audit the committed `Cargo.lock`. It used to be gitignored, so the
  audit job ran `cargo generate-lockfile` first and checked whatever
  resolved that day rather than anything anyone builds. The lockfile is
  committed now and that step is gone, so a vulnerable pin in it gets
  reported instead of quietly resolved away.
- ci: run the test suite under UndefinedBehaviorSanitizer,
  ThreadSanitizer and Valgrind Memcheck, in a new `Sanitizers` workflow.
  UBSan and TSan run on every pull request; they take 14 and 10 minutes,
  which fits inside the 25 the coroutines build already takes. Valgrind
  needs 25 minutes by itself and only adds uninitialised-read detection
  over the per-PR AddressSanitizer job, so it runs nightly and on master,
  or on a pull request labelled `run-sanitizers`. All three are clean
  today. Two upstream
  RocksDB findings are suppressed with the file and line they came from
  in `.github/tsan-suppressions.txt` and `.github/valgrind.supp`: a race
  on the non-atomic `pmull_runtime_flag` global written from every
  `DB::Open` on aarch64, and a self-overlapping `memcpy` in the snappy
  compression sink.
- ci: turn LeakSanitizer on and add `detect_stack_use_after_return`. The
  concern that RocksDB's process-lifetime singletons would swamp the
  report was wrong, because LeakSanitizer treats globals as roots. The
  full suite reports no leaks and needs no suppressions.
- ci: build with `-Ctarget-cpu=native` on x86_64 and aarch64. Every
  other job builds for the target baseline, so nothing was exercising
  the ISA-gated paths; it caught the snappy SSSE3 break above before it
  shipped.
- ci: check the compression features individually and with
  `--no-default-features`. Nothing built without the default feature set
  before, so a backend that only compiled because a sibling feature
  pulled in its headers would not have been caught.
- ci: run the suite once in the release profile, which turns off
  `debug_assertions` and changes what the optimizer may assume.
- ci: limit link parallelism in the sanitizer jobs. Instrumented test
  binaries take several GB each to link and a runner-wide parallel link
  gets the linker OOM-killed.
- ci: fold the two duplicate security audit jobs, which used two
  different actions, into one that runs on pull requests, on master and
  on a timer. It keeps the `Security audit` check name, which branch
  protection requires; a required check that never reports never passes.

## 0.51.0 (2026-06-26)

- fix(librocksdb-sys): upgrade the bundled RocksDB submodule to
  11.1.2. (zaidoon1)
- feat: add `CompactOptions::set_blob_garbage_collection_age_cutoff`,
  exposing the matching RocksDB option through the safe Rust API.
  (kmorkos)
- feat: expose RocksDB's non-deprecated error recovery end callback
  through `EventListener::on_error_recovery_end`. The callback receives
  `BackgroundErrorRecoveryInfo`, including old/new background error
  statuses and severities. (zaidoon1)
- fix: make raw pointer casts clippy-clean on Linux aarch64. No
  behavior change is intended. (evanj)
- fix(librocksdb-sys): parse `CARGO_ENCODED_RUSTFLAGS` with the
  `rustflags` crate so both `-Ctarget-cpu=...` and
  `-C target-cpu=...` forms are handled correctly. (evanj)
- feat: add `SstFileWriter::delete_range` and expose
  `Options::set_experimental_mempurge_threshold`, matching upstream
  rust-rocksdb APIs. (cooronx, 0xdeafbeef)
- feat: add `TransactionDBCheckpoint::create_checkpoint_with_log_size`
  for upstream source compatibility. TransactionDB checkpoints may
  still flush memtables regardless of the threshold. (calavera, gdorsi,
  zaidoon1)
- fix: make `DBRawIteratorWithThreadMode::timestamp()` safe and
  upstream-compatible by returning `Option<&[u8]>`; the prior zero-copy
  unchecked behavior is available as `timestamp_unchecked()`.
  (dillonhicks, ali2992, zaidoon1)
- fix: derive multi-get key pointers and lengths from a single
  `AsRef<[u8]>` call per key across DB, Transaction, and TransactionDB
  paths. (niklasf)
- fix(librocksdb-sys): disable `zstd-sys` default features while
  preserving local `experimental` support, avoiding an unnecessary
  transitive bindgen path. (Congyuwang, zaidoon1)

## 0.50.0 (2026-05-23)

- breaking: bump MSRV to 1.91.0 per the rolling 6-month policy. 1.91.0
  was released 2025-10-30, the most recent stable that satisfied the
  6-month window at the time of the bump. No critical compiler bugs
  or soundness fixes in 1.92+ apply to this codebase. (zaidoon1)
- feat: implement `AsRawPtr<rocksdb_t>` for `OptimisticTransactionDB<T>`
  (gated on the `raw-ptr` feature). Returns the underlying base DB
  pointer for advanced C-API use cases such as verifying file
  checksums directly. (ksurent)
- fix: re-export two public types that were defined in private
  modules but appeared in the return type of a `pub` function in the
  crate-root surface, making them effectively unnameable by downstream
  callers (zaidoon1/rust-rocksdb#224):
  - `ColumnFamilyMetaData` — return type of
    `DB::get_column_family_metadata{,_cf}`. Users need this to store
    or pass the metadata through their own code.
  - `CSlice` — returned wrapped in `(bool, Option<CSlice>)` by the
    `key_may_exist_*_pinned_value` helpers. Users who want to hold
    onto the pinned value past the immediate call site need the name.
  A new `tests/test_public_api.rs` compile-checks both imports so a
  future accidental un-export fails the test build rather than only
  surfacing in downstream user reports. Thanks to JadedBlueEyes for
  the report. (zaidoon1)
- feat(librocksdb-sys): add local C-API extensions for RocksDB C++
  features that have no upstream C wrapper yet. Two new files in
  `librocksdb-sys/c-api-extensions/` (`c_api_extensions.h` and
  `c_api_extensions.cc`) declare and define the new C symbols
  additively; the vendored RocksDB submodule is never modified.
  `build.rs` compiles the extension `.cc` alongside the submodule's
  sources (vendored backend) or links it against the user's
  pre-built `librocksdb` (system backend). No build-time
  dependencies beyond the C++ compiler the crate already requires —
  in particular, no `git` is needed at build time. Three symbols
  ship in this release, each mirroring an upstream PR against
  `facebook/rocksdb`:
  - `rocksdb_readoptions_{set,get}_optimize_multiget_for_io`,
    matching upstream PR facebook/rocksdb#14752.
  - `rocksdb_block_based_options_set_uniform_cv_threshold` and the
    `rocksdb_block_based_table_index_block_search_type_auto = 2`
    enum constant, both needed for `kAuto` index-block search to
    take effect.
  - `rocksdb_options_{set,get}_memtable_batch_lookup_optimization`
    for the skip-list memtable's batch-lookup optimization for
    MultiGet.
  When upstream merges a matching PR and the submodule is bumped to
  a release containing it, the local entry can be dropped.
  (zaidoon1)
- feat: expose three new RocksDB option setters on the safe Rust API,
  matching the three C-API extensions above. Getters are also exposed
  for the two `bool` options so tests (and downstream callers) can
  confirm the C-side actually accepted the value.
  - `BlockBasedOptions::set_index_block_search_type(IndexBlockSearchType)`
    and the new `IndexBlockSearchType` enum (`Binary`, `Interpolation`,
    `Auto`) for selecting the index-block search algorithm.
  - `BlockBasedOptions::set_uniform_cv_threshold(f64)` to set the
    write-path uniformity threshold consulted by `Auto`. Any negative
    value (including the default `-1`) disables the feature; `Auto`
    then falls back to binary search at read time.
  - `Options::{set,get}_memtable_batch_lookup_optimization(bool)` to
    opt into the default skip-list memtable's batch-lookup
    optimization for `MultiGet`. Reduces per-key cost from O(log N) to
    O(log d), where d is the distance between consecutive keys; no-op
    for non-skip-list memtable factories (`Vector`, `HashSkipList`,
    `HashLinkList`). Immutable: must be set before opening the
    column family.
  - `ReadOptions::{set,get}_optimize_multiget_for_io(bool)` to toggle
    between the multi-level (default `true`, lowest latency, higher
    CPU) and single-level (lower CPU) parallel `MultiGet` paths.
    Has no effect outside `coroutines`-enabled builds with
    `set_async_io(true)` — with either condition unmet, both code
    paths fall through to the synchronous per-file lookup regardless
    of this flag.
  (zaidoon1)

## 0.49.1 (2026-05-18)

- removed: drop the `numa` cargo feature that shipped in 0.49.0. The
  feature set `-DNUMA` on the C++ build and linked `libnuma`, but the
  `Options::use_numa_aware_alloc()` runtime knob and the
  `numa_alloc_onnode()` arena code path were removed from rocksdb's
  library proper before 11.1.1 — `NUMA` is now only referenced by
  `tools/db_bench_tool.cc` and `tools/trace_analyzer_tool.cc`, neither
  of which rust-rocksdb compiles. The feature was therefore a no-op
  for library users while adding a build-time dependency on
  `libnuma-dev`. For NUMA-local memtable arenas on multi-socket hosts
  use OS-level pinning (`numactl --cpunodebind --membind`, systemd
  `NUMAPolicy=` / `NUMAMask=`) instead. If you had `features = ["numa"]`
  in your `Cargo.toml`, remove it. (zaidoon1)

## 0.49.0 (2026-05-18)

- feat: add transactiondb checkpoint support (gdorsi)
- feat: add opt-in `coroutines` feature for multi-level async `MultiGet`,
  wrapping RocksDB's `USE_COROUTINES=1` / `USE_FOLLY=1` build path with a
  `scripts/build_folly.sh` helper. Linux-only, requires liburing >= 2.7
  and the `ROCKSDB_FOLLY_INSTALL_PATH` env var pointing at a folly
  install. See README "Async MultiGet with C++20 Coroutines" for full
  prerequisites and runtime constraints. (zaidoon1)
- feat: add opt-in linking against a system-installed RocksDB. Set
  `ROCKSDB_USE_PKG_CONFIG=1` to discover rocksdb via pkg-config, or
  `ROCKSDB_LIB_DIR=<path>` to point at a prebuilt library directly.
  Default behavior (vendored build) is unchanged. Closes #310. (zaidoon1)
- feat: add opt-in `numa` cargo feature (Linux only). When enabled,
  rocksdb's `Options::use_numa_aware_alloc()` pins memtable arena
  allocations to the calling thread's NUMA node via `numa_alloc_onnode()`.
  Useful on multi-socket bare-metal hosts. Requires `libnuma-dev` /
  `numactl-devel` / equivalent. (zaidoon1)
- fix: define `ROCKSDB_BACKTRACE` on Linux glibc and Apple targets
  (macOS, iOS) so rocksdb's `port/stack_trace.cc` produces C++ frames in
  crash output instead of compiling a no-op stub. Matches RocksDB's own
  Makefile build path. (zaidoon1)
- fix: define `ROCKSDB_PTHREAD_ADAPTIVE_MUTEX` on Linux glibc, enabling
  brief adaptive spinning on contended mutexes before falling back to a
  futex wait. (zaidoon1)
- refactor: rewrite `librocksdb-sys/build.rs` into typed `Target` and
  `Backend` abstractions split across `mod vendor / system / snappy /
  bindings / coroutines`, fixing several correctness bugs along the way.
  (zaidoon1)
- fix: bindgen now runs against the chosen backend's headers (vendored
  vs system) instead of always the bundled headers. Eliminates a silent
  ABI-mismatch hazard when linking system rocksdb. (zaidoon1)
- fix: Windows runtime libs (`rpcrt4`, `shlwapi`) are now linked in both
  vendored and system-link paths. Previously, linking a system rocksdb on
  Windows produced unresolved-symbol errors. (zaidoon1)
- fix: target-OS detection now reads `CARGO_CFG_TARGET_*` instead of host
  `#[cfg(target_os=...)]`, eliminating a class of cross-compile bugs.
  (zaidoon1)
- fix: FreeBSD branch now honors `ROCKSDB_STATIC` and
  `ROCKSDB_INCLUDE_DIR`; `ROCKSDB_COMPILE=1` on FreeBSD is rejected up
  front with a clear error (the bundled sources don't build on FreeBSD).
  (zaidoon1)
- behavior change: on Android targets, the C++ stdlib link is now
  `libc++` (NDK r18+, 2018) instead of `libstdc++`. Previous behavior
  produced unresolved-symbol errors on modern NDK toolchains. (zaidoon1)
- feat: added `cargo::metadata=include=` and `cargo::metadata=root=`
  emissions; downstream `-sys` crates can read these as
  `DEP_ROCKSDB_INCLUDE` and `DEP_ROCKSDB_ROOT`. Legacy
  `cargo_manifest_dir`/`out_dir` keys preserved. (zaidoon1)
- fix: iOS deployment target pinned via `-mios-version-min=12.0`
  compiler flag instead of mutating process env, removing one of two
  `unsafe { env::set_var }` blocks. Flag only emitted for actual iOS
  targets (not tvos/watchos). (zaidoon1)
- fix: comprehensive `cargo::rerun-if-env-changed=` coverage including
  `CXXSTDLIB`, `CC`, `CXX`, `CFLAGS`, `CXXFLAGS`, `ROCKSDB_INCLUDE_DIR`,
  `ROCKSDB_CXX_STD`, `CARGO_ENCODED_RUSTFLAGS`,
  `BINDGEN_EXTRA_CLANG_ARGS`, `DEP_<LZ4/ZSTD/Z/BZIP2>_INCLUDE`,
  `DEP_JEMALLOC_ROOT`, `PKG_CONFIG_*`. (zaidoon1)
- fix: define `HAVE_FULLFSYNC` on Apple targets (macOS, iOS, tvOS,
  watchOS) so RocksDB takes the `fcntl(F_FULLFSYNC)` path for true on-
  disk durability rather than the weaker plain `fsync` (which on macOS
  only flushes to the drive cache). Matches RocksDB's CMakeLists.txt.
  (zaidoon1)
- fix: emit `-DWIN32` (not `-DDWIN32`) on Windows targets so the define
  matches what RocksDB's CMakeLists.txt sets. (zaidoon1)
- ci: run the coroutines workflow on both x86_64 and aarch64 Linux
  (ubuntu-24.04 / ubuntu-24.04-arm hosts, ubuntu:25.10 container) so
  arch-specific breakage in folly's build or our link config is caught
  before release. (zaidoon1)

## 0.48.0 (2026-05-04)

- upgrade RocksDB to 11.1.1 (zaidoon1)

## 0.47.0 (2026-04-08)

- upgrade RocksDB to 11.0.4 (zaidoon1)
- feat: add flush, flush_wal, flush_cf, flush_cf_opt, flush_cfs_opt to TransactionDB (gdorsi)
- feat: add timestamp() to DBRawIteratorWithThreadMode (ali2992)
- feat: add IngestExternalFileOptions AsRawPtr impl (ali2992)
- feat: add Iterator::Refresh() bindings (joshsend)
- fix: memory leak in Options.set_info_logger (evanj)
- fix: sync build.rs defines with RocksDB; pass through -Ctarget-cpu and set aarch64 CRC32 flags (evanj)
- breaking: remove set_skip_checking_sst_file_sizes_on_db_open (removed from RocksDB 11.0 C API) (zaidoon1)
- chore: replace trybuild with compile_fail doctests, removing trybuild dev-dependency (zaidoon1)

## 0.46.0 (2026-02-03)

- upgrade RocksDB to 10.10.1 (zaidoon1)
- doc: document enable_statistics stats are shared (evanj)
- feat: add sequence_number method to SnapshotWithThreadMode (lucacasonato)
- perf: use thread-local ReadOptions in Transaction::multi_get (zaidoon1)
- refactor: simplify MergeOperands::get_operand using pointer add() method (zaidoon1)
- chore: remove bincode and serde dev-dependencies, implement manual serialization in merge operator test (zaidoon1)
- feat: add zero-copy C API support for get_into_buffer, batched_multi_get_cf_slice, and optimized iterator (zaidoon1)

## 0.45.0 (2026-01-06)

- upgrade to RocksDB 10.9.1 (zaidoon1)
- upgrade to Rust edition 2024 and MSRV 1.89.0 (zaidoon1)
- breaking: remove set_options_from_string (incorrectly named and wrong implementation) (zaidoon1)
- feat: expose get_options_from_string (MarioRuiz)
- feat: add feature-gated `raw_ptr` trait to expose raw C pointers (jszwec)
- fix: memory leak in DBCommon.get_column_family_metadata(_cf) (evanj)
- fix: memory leak in DB.create_cf on error (evanj)
- fix: memory leak in MemoryUsageBuilder.add_tx_db (evanj)
- fix: memory leak in DBOptions.get_options_from_string on error (evanj)
- fix: memory leak in DBOptions.set_compaction_filter (evanj)
- fix: memory leak in DBOptions.set_comparator(_ts) (evanj)
- fix: MemoryUsageBuilder now requires DBs/caches to outlive the builder (evanj)
- improve Cache::new_hyper_clock_cache documentation (evanj)
- minor documentation improvements (luohewuyang, vastonus, Galoretka)

## 0.44.2 (2025-11-05)

- breaking: Switched internal column-family map to a HashMap; `DBCommon::cf_names()` no longer guarantees sorted order (order is unspecified and may vary) (zaidoon1)
- Add zero-copy MultiGet pinned APIs: `DB::multi_get_pinned`, `multi_get_pinned_opt`, `multi_get_pinned_cf`, `multi_get_pinned_cf_opt` for efficient batch point lookups without value copies. (zaidoon1)
- Add fast prefix existence APIs: `DB::prefix_exists`, `prefix_exists_opt`, `prefix_exists_cf`, `prefix_exists_cf_opt`. (zaidoon1)
- Add `PrefixProber` reusable iterator and constructors `prefix_prober`, `prefix_prober_cf`, `prefix_prober_with_opts`, `prefix_prober_cf_with_opts` to amortize iterator setup cost for high-QPS prefix checks. (zaidoon1)
- unix: Optimize `ffi_util::to_cpath` to perform zero-copy path conversion using `OsStrExt::as_bytes()` when available. (zaidoon1)
- Expose APIs to import and export Column Families (pcholakov)

## 0.44.1 (2025-10-23)

- simplify SstFileManager by using default Env internally (zaidoon1)

## 0.44.0 (2025-10-23)

- upgrade to RocksDB 10.7.5 (zaidoon1)
- feat: expose enable_file_deletions/disable_file_deletions (jszwec)
- expose SstFileManager and Options::set_sst_file_manager (zaidoon1)
- add add_compact_on_deletion_collector_factory_min_file_size option (zaidoon1)
- expose set_cache_index_and_filter_blocks_with_high_priority (zaidoon1)
- Options.set_callback_logger: Make closure lifetimes safe (evanj)
- add support for SingleDelete (zaidoon1)

## 0.43.0 (2025-08-11)

- upgrade to RocksDB 10.5.1 (zaidoon1)
- clippy fixes (zaidoon1)
- Ensure jemalloc is linked in when the feature is enabled (#1026) (timothyg-stripe)
- fix set_skip_prepare transaction option to call correct c api (zaidoon1)
- Expose get_pinned_from_batch_and_db for WriteBatchWithIndex (tillrohrmann)
- Expose WBWI through rust-rocksdb (tillrohrmann)
- Access to open column families names (AhmedSoliman)
- Pass write batch by reference (AhmedSoliman)
- Use parking_lot's RwLock (AhmedSoliman)
- minor code refactor to take out different complex options outside of db_options file (zaidoon1)
- feat: expose set_memtable_avg_op_scan_flush_trigger (zaidoon1)
- mark set_skip_checking_sst_file_sizes_on_db_open as deprecated (#1017) (evanj)
- Implement WriteBatchIteratorCf trait and update related methods for compatibility (#1002) (RiversJin)
- Add get_approximate_sizes (#998) (ran-openai)
- ffi_util.rs: improve opt_bytes_to_str to avoid potential use-after-free (#693) (#1003) (Chain-Fox)
- db_options.rs: deprecate set_ignore_range_deletions (RocksDB 10.2.1) (#1000) (evanj)
- prep work for upgrading to rust 2024 (zaidoon1)

## 0.42.1 (2025-07-15)

- fix event listener implementation and add partial support for on_background_error (zaidoon1)

## 0.42.0 (2025-07-14)

- add event listener support (zaidoon1)
- upgrade to RocksDB 10.4.2 (zaidoon1)
- fix: gcc15 build support (lucasl0st)

## 0.41.0 (2025-04-29)

- doc db_iterator.rs: Minor edits to rustdoc; more links (evanj)
- upgrade to RocksDB 10.2.1 (zaidoon1)
- feat: expose set_memtable_op_scan_flush_trigger (zaidoon1)

## 0.40.0 (2025-04-19)

- upgrade to RocksDB 10.1.3 (zaidoon1)

## 0.39.0 (2025-04-01)

- upgrade to RocksDB 10.0.1 (zaidoon1)
- bump snappy to 1.2.2 (zaidoon1)
- bump lz4 to v1.11 (zaidoon1)

## 0.38.0 (2025-03-30)

- upgrade to RocksDB 9.11.2 (zaidoon1)

## 0.37.0 (2025-03-07)

- Support builds on AIX (mustartt)
- WriteBatch: add support for WriteBatch::put_log_data (lucasvuillier)
- Fix C++ linking (brndnmtthws)
- add ROCKSDB_AUXV_GETAUXVAL_PRESENT for supported Linux systems (zaidoon1)
- Add backup options and db options sync/fsync getters/setters (timvisee)
- upgrade to RocksDB 9.11.1 (zaidoon1)
- bump msrv to 1.81.0 (zaidoon1)

## 0.36.0 (2025-01-03)

- Fix some typos (DeVikingMark)
- chore: fix multiple typos of different importance (crStiv)
- feat: allow to set per cf ttl (0xdeafbeef)
- Fix some typos (teenager-ETH)
- Fix future clippy warnings (niklasf)
- upgrade to RocksDB 9.10.0 (zaidoon1)

## 0.35.0 (2024-12-17)

- DB: Implement get_db_identity using rocksdb_get_db_identity (evanj)
- Add lto feature (0xdeafbeef)
- Options: Add set_track_and_verify_wals_in_manifest (evanj)
- upgrade to RocksDB 9.9.3 (zaidoon1)
- add set_use_delta_encoding() to Options (jevolk)

## 0.34.0 (2024-12-04)

- Fix two tests that want to write to the current working directory (mr-c)
- add missing supported bsd oses (drizzt)
- Fix column family creation race. (stuhood)
- Allow using static bindgen feature (Congyuwang)
- tests: use tempfile instead of the current working directory (mr-c)
- implement with_capacity for WriteBatch (0xdeafbeef)
- ci: make most directories read-only before running the tests (mr-c)
- More temp directories for tests (mr-c)
- fix(build): add ROCKSDB_SCHED_GETCPU_PRESENT for Linux build config (popcnt1)
- upgrade to RocksDB 9.8.4 (zaidoon1)

## 0.33.0 (2024-11-01)

- upgrade to RocksDB 9.7.4 (zaidoon1)

## 0.32.0 (2024-10-23)

- Decrement refcount after registering info loggers (jevolk)
- upgrade to RocksDB 9.7.3 (zaidoon1)

## 0.31.0 (2024-10-16)

- Expose LRU cache options (athre0z)
- add Env::from_raw constructor (jgraettinger)
- Fix unsoundness via impure AsRef (niklasf)
- Allow setting logging callback (jevolk)
- upgrade to RocksDB 9.7.2 (zaidoon1)

## 0.30.0 (2024-09-06)

- Improve statistics by auto gen enum Ticker & enum Histogram (rockeet)
- upgrade to RocksDB 9.6.1 (zaidoon1)

## 0.29.0 (2024-08-21)

- Implement Sync for BoundColumnFamily (jhpratt)
- use the provided system rocksdb prebuilt on freebsd (girlbossceo)
- TransactionDB support in MemoryUsageBuilder (4TT1L4)
- upgrade to RocksDB 9.5.2 (zaidoon1)

## 0.28.1 (2024-07-26)

- allow unprefixed musl jemalloc targets (girlbossceo)
- bump tikv-jemalloc-sys to 0.6 (girlbossceo)
- fix: android build in 32-bit devices (LucasXu0)
- Support user defined timestamp in rust bindings (siyuan0322)
- Bump lz4 1.10 (agourlay)
- feat: Properties for TransactionDB #899 (4TT1L4)
- Improvements to user defined timestamp (larry0x)

## 0.28.0 (2024-07-13)

- Add support for enabling blob cache (exabytes18)
- upgrade to RocksDB 9.4.0 (zaidoon1)

## 0.27.1 (2024-07-07)

- Add block based metadata cache options (zaidoon1)
- add feature flag to enable ZSTD_STATIC_LINKING_ONLY (zaidoon1)
- fix stats comments (zaidoon1)
- enable experimental feature in zstd-sys (zaidoon1)

## 0.27.0 (2024-06-29)

- Add option set_avoid_unnecessary_blocking_io (w41ter)
- add option to enable auto tuned ratelimiter (w41ter)
- clean up rate limiter object properly for set_ratelimiter_with_mode (zaidoon1)
- upgrade to RocksDB 9.3.1 (zaidoon1)
- Add option set_compaction_pri (zaidoon1)

## 0.26.0 (2024-04-24)

- Add delete_range to OptimisticTransactionDB (vadim-su)
- Bump snappy to 1.2.0 (aleksuss)
- docs: document that default cf doesn't inherit db open options (0xdeafbeef)
- upgrade to RocksDB 9.2.1 (zaidoon1)

## 0.25.0 (2024-04-23)

- Update to RocksDB 9.1.1 (zaidoon1)

## 0.24.0 (2024-04-18)

- update README to document the various crate features that can be enabled (zaidoon1)
- Update to RocksDB 9.1.0 (zaidoon1)

## 0.23.2 (2024-03-30)

- fix set_options_from_string binding (zaidoon1)

## 0.23.1 (2024-03-28)

- make ColumnFamily Sync (zaidoon1)
- fix histogram stats after enum re-shuffle introduced in rocksdb v9.0 (zaidoon1)
- Add linking libatomic command to build.rs to allow building for riscv64gc-unknown-linux-gnu target (willemolding)
- Make BackupEngine Send (widagdos)
- Add readme for mt_static feature (spector-9)
- Add method to set DBOptions from string (jevolk)

## 0.23.0 (2024-03-20)

- Update to RocksDB 9.0.0 (zaidoon1)
- Expose rate limiter with mode feature (zaidoon1)
- Revert portable feature (zaidoon1)

## 0.22.8 (2024-03-15)

- Expose io-timeout/deadline read options (zaidoon1)
- modernize CI and other CI related clean (zaidoon1)
- replace unmaintained dev dependency (zaidoon1)
- more ci clean up (zaidoon1)
- fix: ptr::copy requires both ptrs to be non-null (ruanpetterson)
- Feat: Adds crt_static method (spector-9)
- Add portable feature for RocksDB build (sujayakar)
- Update README.md with a new section for the portable feature (sujayakar)

## 0.22.7 (2024-03-02)

- don't use system jemalloc (zaidoon1)

## 0.22.6 (2024-02-27)

- Update to RocksDB 8.11.3 (zaidoon1)
- Expose set_ttl (zaidoon1)

## 0.22.5 (2024-02-26)

- add feature flag to enable malloc-usable-size used by optimize_filtes_for_memory feature (zaidoon1)
- gate malloc-usable-size to linux only (zaidoon1)
- actually enable jemalloc when feature is used on linux (zaidoon1)

## 0.22.4 (2024-02-20)

- Update to RocksDB 8.10.2 (zaidoon1)
- Fix build status badge and other bits in README.md (jdanford)

## 0.22.3 (2024-02-13)

- Export memory usage builder and MemoryUsage structs to users (AhmedSoliman)
- Make FlushOptions Send and Sync (jansegre)

## 0.22.2 (2024-02-12)

- Expose rocksdb cumulative statistics and histograms (AhmedSoliman)

## 0.22.1 (2024-02-10)

- rename librocksdb-sys library (zaidoon1)

## 0.22.0 (2024-02-10)

- update code imports after package name change and clean up README/MAINTAINERHSIP (zaidoon1)
- update README and package name (zaidoon1)
- bump dependencies & upgrade to latest rust version (zaidoon1)
- update doc and para name for optimize_for_point_lookup (XiangpengHao)
- Add WriteBufferManager support (benoitmeriaux)
- Update to RocksDB 8.10.0 (zaidoon1)
- Make `CompactOptions` `Send` and `Sync` (GodTamIt)
- Update hash commit of the rocksdb submodule to corresponding v8.9.1 tag (aleksuss)
- feat: Expose set_periodic_compaction_seconds (zaidoon1)
- Update RocksDB to 8.9.1 (zaidoon1)
- feat: Expose set_auto_readahead_size (niklasf)
- feat: Expose wait_for_compact (zaidoon1)
- Fix bug in DBWALIterator that would return updates before the given sequence (schmidek)
- feat: Expose compact_on_deletion_collector_factory (zaidoon1)
- Update RocksDB to 8.8.1 (zaidoon1)
- feat: Expose set_wal_compression_type (ovr)
- Fix typo in documentation (jazarine)
- fix: add raw iterator validation before calling next method (aleksuss)
- feat: expose compression option parallel_threads (zaidoon1)
- feat: expose set_optimize_filters_for_memory (zaidoon1)
- Update RocksDB to 8.6.7 (aleksuss)
- Expose `ReadTier` publicly (tinct-martini)
- Update RocksDB to 8.5.3 (niklasf)
- feat: support column_family_metadata, column_family_metadata_cf (ovr)
- Remove wrong outlive requirements for cache in docs (zheland)
- Add `allow_ingest_behind` ffi call for DB Options (siyuan0322)
- Wrap prop names into a PropName type offering free conversion to str (mina86)
- Remove temporary boxed keys in batched_multi_get (axnsan12)
- Update to RocksDB 8.3.2 (niklasf)
- Expose flush_cfs_opt to flush multiple column families (lizhanhui)
- Prefer rocksdb_free to free for RocksDB memory (niklasf)
- Update snappy to 1.1.10 (timsueberkrueb)
- Free memory on writebatch index and avoid unnecessary clones (jkurian)

## 0.21.0 (2023-05-09)

- Add doc-check to CI with fix warnings in docs (YuraKotov)
- Fix rustdoc::broken-intra-doc-links errors (YuraKotov)
- Fix 32-bit ARM build (EyeOfPython)
- Allow specifying checksum type (romanz)
- Enable librocksdb-sys to be built by rustc_codegen_cranelift (ZePedroResende)
- Update to RocksDB 8.0.0 (niklasf)
- Block cache creation failure is not recoverable (niklasf)
- Update iOS min version to 12 in the build script (mighty840)
- Actually enable `io-uring` (niklasf)
- Update to RocksDB 8.1.1 (niklasf)
- Add `Cache::new_hyper_clock_cache()` (niklasf)
- Retrieve Value from KeyMayExist if value found in Cache or Memory (Congyuwang)
- Support for comparators as closures (pegesund)
- Fix bug in DBWALIterator that would miss updates (Zagitta)

## 0.20.1 (2023-02-10)

- Fix supporting MSRV 1.60.0 (aleksuss)

## 0.20.0 (2023-02-09)

- Support RocksDB 7.x `BackupEngineOptions` (exabytes18)
- Fix `int128` compatibility check (Dirreke)
- Add `Options::load_latest` method to load the latest options from RockDB (Congyuwang)
- Bump bindgen to 0.64.0 (cwlittle)
- Bump rocksdb to 7.9.2 (kwek20)
- Make `set_snapshot` method public (a14e)
- Add `drop_cf` function to `TransactionDB` (bothra90)
- Bump rocksdb to 7.8.3 (aleksuss)
- Add doc for `set_cache_index_and_filter_blocks` (guerinoni)
- Re-run `build.rs` if env vars change (drahnr)
- Add `WriteBatch::data` method (w41ter)
- Add `DB::open_cf_with_opts` method (w41ter)
- Use lz4-sys crate rather then submodule (niklasf)
- Make create_new_backup_flush generic (minshao)

## 0.19.0 (2022-08-05)

- Add support for building with `io_uring` on Linux (parazyd)
- Change iterators to return Result (mina86)
- Support RocksDB transaction (yiyuanliu)
- Avoid pulling in dependencies via static feature flag (niklasf)
- Bump `rocksdb` to 7.4.4 (niklasf)
- Bump `tikv-jemalloc-sys` to 0.5 (niklasf)
- Update `set_use_fsync` comment (nazar-pc)
- Introduce ReadOptions::set_iterate_range and PrefixRange (mina86)
- Bump `rocksdb` to 7.4.3 (aleksuss)
- Don’t hold onto ReadOptions.inner when iterating (mina86)
- Bump `zstd-sys` from 1.6 to 2.0 (slightknack)
- Enable a building on the iOS platform (dignifiedquire)
- Add DBRawIteratorWithThreadMode::item method (mina86)
- Use NonNull in DBRawIteratorWithThreadMode (mina86)
- Tiny refactoring including fix for UB (niklasf)
- Add batched version MultiGet API (yhchiang-sol)
- Upgrade to rocksdb v7.3.1 (yhchiang-sol)
- Consistently use `ffi_util::to_cpath` to convert `Path` to `CString` (mina86)
- Convert properties to `&CStr` (mina86)
- Allow passing `&CStr` arguments (mina86)
- Fix memory leak when reading properties and avoid memory allocation (mina86)
- Fix Windows UTF-8 build flag (rajivshah3)
- Use more target features to build librocksdb-sys (niklasf)
- Fix `bz_internal_error` symbol multiply defined (nanpuyue)
- Bump rocksdb to 7.1.2 (dignifiedquire)
- Add BlobDB options (dignifiedquire)
- Add snapshot `PinnableSlice` based API (zheland)

## 0.18.0 (2022-02-03)

- Add open_cf_descriptor methods for Secondary and ReadOnly AccessType (steviez)
- Make Ribbon filters available (niklasf)
- Change versioning scheme of `librocksdb-sys` crate (aleksuss)
- Upgrade to RocksDB 6.28.2 (akrylysov)
- Fix theoretical UB while transmuting Arc (niklasf)
- Support configuring bottom-most compression level (mina86)
- Add BlockBasedOptions::set_whole_key_filtering (niklasf)
- Add constants for all supported properties (steviez)
- Make CacheWrapper and EnvWrapper Send and Sync (aleksuss)
- Replace mem::transmute with narrower conversions (niklasf)
- Optimize non-overlapping copy in raw_data (niklasf)
- Support multi*get*\* methods (olegnn)
- Optimize multi_get_cf_opt() to use size hint (niklasf)
- Fix typo in set_background_purge_on_iterator_cleanup method (Congyuwang)
- Use external compression crates where possible (Dr-Emann)
- Update compression dependencies (akrylysov)
- Add method for opening DB with ro access and cf descriptors (nikurt)
- Support restoring from a specified backup (GoldenLeaves)
- Add merge operands iterator (0xdeafbeef)
- Derive serde::{Serialize, Deserialize} for configuration enums (thibault-martinez)
- Add feature flag for runtime type information and metadata (jgraettinger)
- Add set_info_log_level to control log verbosity (tkintscher)
- Replace jemalloc-sys for tikv-jemalloc-sys (Rexagon)
- Support UTF-8 file paths on Windows (rajivshah3)
- Support building RocksDB with jemalloc (akrylysov)
- Add rocksdb WAL flush api (duarten)
- Update rocksdb to v6.22.1 (duarten)

## 0.17.0 (2021-07-22)

- Fix `multi_get` method (mikhailOK)
- Bump `librocksdb-sys` up to 6.19.3 (olegnn)
- Add support for the cuckoo table format (rbost)
- RocksDB is not compiled with SSE4 instructions anymore unless the corresponding features are enabled in rustc (mbargull)
- Bump `librocksdb-sys` up to 6.20.3 (olegnn, akrylysov)
- Add `DB::key_may_exist_cf_opt` method (stanislav-tkach)
- Add `Options::set_zstd_max_train_bytes` method (stanislav-tkach)
- Mark Cache and Env as Send and Sync (akrylysov)
- Allow cloning the Cache and Env (duarten)
- Make SSE inclusion conditional for target features (mbargull)
- Use Self where possible (adamnemecek)
- Don't leak dropped column families (ryoqun)

## 0.16.0 (2021-04-18)

- Add `DB::cancel_all_background_work` method (stanislav-tkach)
- Bump `librocksdb-sys` up to 6.13.3 (aleksuss)
- Add `multi_get`, `multi_get_opt`, `multi_get_cf` and `multi_get_cf_opt` `DB` methods (stanislav-tkach)
- Allow setting options on a ColumnFamily (romanz)
- Fix logic related to merge operator settings (BoOTheFurious)
- Export persist_period_sec option and background_threads (developerfred)
- Remove unneeded bindgen features (Kixunil)
- Add merge delete_callback omitted by mistake (zhangsoledad)
- Bump `librocksdb-sys` up to 6.17.3 (ordian)
- Remove the need for `&mut self` in `create_cf` and `drop_cf` (v2) (ryoqun)
- Keep Cache and Env alive with Rc (acrrd)
- Add `DB::open_cf_with_ttl` method (fdeantoni)

## 0.15.0 (2020-08-25)

- Fix building rocksdb library on windows host (aleksuss)
- Add github actions CI for windows build (aleksuss)
- Update doc for `Options::set_compression_type` (wqfish)
- Add clippy linter in CI (aleksuss)
- Use DBPath for backup_restore test (wqfish)
- Allow to build RocksDB with a different stdlib (calavera)
- Add some doc-comments and tiny refactoring (aleksuss)
- Expose `open_with_ttl`. (calavera)
- Fixed build for `x86_64-linux-android` that doesn't support PCLMUL (vimmerru)
- Add support for `SstFileWriter` and `DB::ingest_external_file` (methyl)
- Add set_max_log_file_size and set_recycle_log_file_num to the Options (stanislav-tkach)
- Export the `DEFAULT_COLUMN_FAMILY_NAME` constant (stanislav-tkach)
- Fix slice transformers with no in_domain callback (nelhage)
- Don't segfault on failed a merge operator (nelhage)
- Adding read/write/db/compaction options (linxGnu)
- Add dbpath and env options (linxGnu)
- Add compaction filter factory API (unrealhoang)
- Add link stdlib when linking prebuilt rocksdb (unrealhoang)
- Support fetching sst files metadata, delete files in range, get mem usage (linxGnu)
- Do not set rerun-if-changed=build.rs (xu-cheng)
- Use pretty_assertions in tests (stanislav-tkach)
- librocksdb-sys: update rocksdb to 6.11.4 (ordian)
- Adding backup engine info (linxGnu)
- Implement `Clone` trait for `Options` (stanislav-tkach)
- Added `Send` implementation to `WriteBatch` (stanislav-tkach)
- Extend github actions (stanislav-tkach)
- Avoid copy for merge operator result using delete_callback (xuchen-plus)

## 0.14.0 (2020-04-22)

- Updated lz4 to v1.9.2 (ordian)
- BlockBasedOptions: expose `format_version`, `[index_]block_restart_interval` (ordian)
- Improve `ffi_try` macro to make trailing comma optional (wqfish)
- Add `set_ratelimiter` to the `Options` (PatrickNicholas)
- Add `set_max_total_wal_size` to the `Options` (wqfish)
- Simplify conversion on iterator item (zhangsoledad)
- Add `flush_cf` method to the `DB` (wqfish)
- Fix potential segfault when calling `next` on the `DBIterator` that is at the end of the range (wqfish)
- Move to Rust 2018 (wqfish)
- Fix doc for `WriteBatch::delete` (wqfish)
- Bump `uuid` and `bindgen` dependencies (jonhoo)
- Change APIs that never return error to not return `Result` (wqfish)
- Fix lifetime parameter for iterators (wqfish)
- Add a doc for `optimize_level_style_compaction` method (NikVolf)
- Make `DBPath` use `tempfile` (jder)
- Refactor `db.rs` and `lib.rs` into smaller pieces (jder)
- Check if we're on a big endian system and act upon it (knarz)
- Bump internal snappy version up to 1.1.8 (aleksuss)
- Bump rocksdb version up to 6.7.3 (aleksuss)
- Atomic flush option (mappum)
- Make `set_iterate_upper_bound` method safe (wqfish)
- Add support for data block hash index (dvdplm)
- Add some extra config options (casualjim)
- Add support for range delete APIs (wqfish)
- Improve building `librocksdb-sys` with system libraries (basvandijk)
- Add support for `open_for_read_only` APIs (wqfish)
- Fix doc for `DBRawIterator::prev` and `next` methods (wqfish)
- Add support for `open_as_secondary` APIs (calavera)

## 0.13.0 (2019-11-12)

### Changes

- Added `ReadOptions::set_verify_checksums` and
  `Options::set_level_compaction_dynamic_level_bytes` methods (ordian)
- Array of bytes has been changed for pinnable slice for get operations (nbdd0121)
- Implemented `Sync` for `DBRawIterator` (nbdd0121)
- Removed extra copy in DBRawIterator (nbdd0121)
- Added `Options::max_dict_bytes` and `Options::zstd_max_training_bytes` methods(methyl)
- Added Android support (rtsisyk)
- Added lifetimes for `DBIterator` return types (ngotchac)
- Bumped rocksdb up to 6.2.4 (aleksuss)
- Disabled trait derivation for librocksdb-sys (EyeOfPython)
- Added `DB::get_updates_since()` to iterate write batches in a given sequence (nlfiedler)
- Added `ReadOptions::set_tailing()` to create a tailing iterator that continues to
  iterate over the database as new records are added (cjbradfield)
- Changed column families storing (aleksuss)
- Exposed the `status` method on iterators (rnarubin)

## 0.12.3 (2019-07-19)

### Changes

- Enabled sse4.2/pclmul for accelerated crc32c (yjh0502)
- Added `set_db_write_buffer_size` to the Options API (rnarubin)
- Bumped RocksDB to 6.1.2 (lispy)
- Added `Sync` and `Send` implementations to `Snapshot` (pavel-mukhanov)
- Added `raw_iterator_cf_opt` to the DB API (rnarubin)
- Added `DB::latest_sequence_number` method (vitvakatu)

## 0.12.2 (2019-05-03)

### Changes

- Updated `compact_range_cf` to use generic arguments (romanz)
- Removed allocations from `SliceTransform` implementation (ekmartin)
- Bumped RocksDB to 5.18.3 (baptistejamin)
- Implemented `delete_range` and `delete_range_cf` (baptistejamin)
- Added contribution guide (rhurkes)
- Cleaned up documentation for `ReadOptions.set_iterate_upper_bound` method (xiaobogaga)
- Added `flush` and `flush_opt` operations (valeriansaliou)

## 0.12.1 (2019-03-27)

### Changes

- Added `iterator_cf_opt` function to `DB` (elichai)
- Added `set_allow_mmap_writes` and `set_allow_mmap_reads` functions to `Options` (aleksuss)

## 0.12.0 (2019-03-10)

### Changes

- Added support for PlainTable factories (ekmartin)
- Added ability to restore latest backup (rohitjoshi)
- Added support for pinnable slices (xxuejie)
- Added ability to get property values (ekmartin)
- Simplified opening database when using non-default column families (iSynaptic)
- `ColumnFamily`, `DBIterator` and `DBRawIterator` now have lifetime parameters to prevent using them after the `DB` has been dropped (iSynaptic)
- Creating `DBIterator` and `DBRawIterator` now accept `ReadOptions` (iSynaptic)
- All database operations that accepted byte slices, `&[u8]`, are now generic and accept anything that implements `AsRef<[u8]>` (iSynaptic)
- Bumped RocksDB to version 5.17.2 (aleksuss)
- Added `set_readahead_size` to `ReadOptions` (iSynaptic)
- Updated main example in doc tests (mohanson)
- Updated requirements documentation (jamesray1)
- Implemented `AsRef<[u8]>` for `DBVector` (iSynaptic)

## 0.11.0 (2019-01-10)

### Announcements

- This is the first release under the new [Maintainership](MAINTAINERSHIP.md) model.
  Three contributors have been selected to help maintain this library -- (aleksuss) ([@aleksuss](https://github.com/aleksuss)), Jordan Terrell ([@iSynaptic](https://github.com/iSynaptic)), and Ilya Bogdanov ([@vitvakatu](https://github.com/vitvakatu)). Many thanks to Tyler Neely ([@spacejam](https://github.com/spacejam)) for your support while taking on this new role.

- A [gitter.im chat room](https://gitter.im/rust-rocksdb/Lobby) has been created. Although it's not guaranteed to be "staffed", it may help to collaborate on changes to `rust-rocksdb`.

### Changes

- added LZ4, ZSTD, ZLIB, and BZIP2 compression support (iSynaptic)
- added support for `Checkpoint` (aleksuss)
- added support for `SliceTransform` (spacejam)
- added `DBPath` struct to ensure test databases are cleaned up (ekmartin, iSynaptic)
- fixed `rustfmt.toml` to work with newer `rustfmt` version (ekmartin, iSynaptic)
- bindgen bumped up to 0.43 (s-panferov)
- made `ColumnFamily` struct `Send` (Tpt)
- made `DBIterator` struct `Send` (Elzor)
- `create_cf` and `drop_cf` methods on `DB` now work with immutable references (aleksuss)
- fixed crash in `test_column_family` test on macOS (aleksuss)
- fixed/implemented CI builds for macOS and Windows (aleksuss, iSynaptic)
- exposed `set_skip_stats_update_on_db_open` option (romanz)
- exposed `keep_log_file_num` option (romanz)
- added ability to retrieve `WriteBatch` serialized size (romanz)
- added `set_options` method to `DB` to allow changing options without closing and re-opening the database (romanz)

## 0.10.1 (2018-07-17)

- bump bindgen to 0.37 (ekmartin)
- bump rocksdb to 5.14.2 (ekmartin)
- add disable_cache to block-based options (ekmartin)
- add set_wal_dir (ekmartin)
- add set_memtable_prefix_bloom_ratio (ekmartin)
- add MemtableFactory support (ekmartin)
- add full_iterator (ekmartin)
- allow index type specification on block options (ekmartin)
- fix windows build (iSynaptic)

## 0.10.0 (2018-03-17)

- Bump rocksdb to 5.11.3 (spacejam)

### New Features

- Link with system rocksdb and snappy libs through envvars (ozkriff)

### Breaking Changes

- Fix reverse iteration from a given key (ongardie)

## 0.9.1 (2018-02-10)

### New Features

- SliceTransform support (spacejam)

## 0.9.0 (2018-02-10)

### New Features

- Allow creating iterators over prefixes (glittershark)

### Breaking Changes

- Open cfs with options (garyttierney, rrichardson)
- Non-Associative merge ops (rrichardson)

## 0.8.3 (2018-02-10)

- Bump rocksdb to 5.10.2 (ongardie)
- Add Send marker to Options (iSynaptic)
- Expose advise_random_on_open option (ongardie)

## 0.8.2 (2017-12-28)

- Bump rocksdb to 5.7.1 (jquesnelle)

## 0.8.1 (2017-09-08)

- Added list_cf (jeizsm)

## 0.8.0 (2017-09-02)

- Removed set_disable_data_sync (glittershark)

## 0.7.2 (2017-09-02)

- Bumped rocksdb to 5.6.2 (spacejam)

## 0.7.1 (2017-08-29)

- Bumped rocksdb to 5.6.1 (vmx)

## 0.7 (2017-07-26)

### Breaking Changes

- Bumped rocksdb to 5.4.6 (derekdreery)
- Remove `use_direct_writes` now that `use_direct_io_for_flush_and_compaction` exists (derekdreery)

### New Features

- ReadOptions is now public (rschmukler)
- Implement Clone and AsRef<str> for Error (daboross)
- Support for `seek_for_prev` (kaedroho)
- Support for DirectIO (kaedroho)

### Internal Cleanups

- Fixed race condition in tests (debris)
- Move tests to the default `tests` directory (vmx)

## 0.6.1 (2017-03-13)

### New Features

- Support for raw iterator access (kaedroho)

## 0.6 (2016-12-18)

### Breaking Changes

- Comparator function now returns an Ordering (alexreg)

### New Features

- Compaction filter (tmccombs)
- Support for backups (alexreg)

  0.5 (2016-11-20)

### Breaking changes

- No more Writable trait, as WriteBatch is not thread-safe as a DB (spacejam)
- All imports of `rocksdb::rocksdb::*` should now be simply `rocksdb::*` (alexreg)
- All errors changed to use a new `rocksdb::Error` type (kaedroho, alexreg)
- Removed `Options.set_filter_deletes` as it was removed in RocksDB (kaedroho)
- Renamed `add_merge_operator` to `set_merge_operator` and `add_comparator` to `set_comparator` (kaedroho)

### New Features

- Windows support (development by jsgf and arkpar. ported by kaedroho)
- The RocksDB library is now built at crate compile-time and statically linked with the resulting binary (development by jsgf and arkpar. ported by kaedroho)
- Cleaned up and improved coverage and tests of the ffi module (alexreg)
- Added many new methods to the `Options` type (development by ngaut, BusyJay, zhangjinpeng1987, siddontang and hhkbp2. ported by kaedroho)
- Added `len` and `is_empty` methods to `WriteBatch` (development by siddontang. ported by kaedroho)
- Added `path` method to `DB` (development by siddontang. ported by kaedroho)
- `DB::open` now accepts any type that implements `Into<Path>` as the path argument (kaedroho)
- `DB` now implements the `Debug` trait (kaedroho)
- Add iterator_cf to snapshot (jezell)
- Changelog started

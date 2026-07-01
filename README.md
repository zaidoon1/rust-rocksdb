# rust-rocksdb

[![RocksDB build](https://github.com/zaidoon1/rust-rocksdb/actions/workflows/rust.yml/badge.svg?branch=master)](https://github.com/zaidoon1/rust-rocksdb/actions/workflows/rust.yml)
[![crates.io](https://img.shields.io/crates/v/rust-rocksdb.svg)](https://crates.io/crates/rust-rocksdb)
[![documentation](https://docs.rs/rust-rocksdb/badge.svg)](https://docs.rs/rust-rocksdb)
[![license](https://img.shields.io/crates/l/rust-rocksdb.svg)](https://github.com/zaidoon1/rust-rocksdb/blob/master/LICENSE)
![rust 1.91.0 required](https://img.shields.io/badge/rust-1.91.0-blue.svg?label=MSRV)
![GitHub commits (since latest release)](https://img.shields.io/github/commits-since/zaidoon1/rust-rocksdb/latest.svg)
[![dependency status](https://deps.rs/repo/github/zaidoon1/rust-rocksdb/status.svg)](https://deps.rs/repo/github/zaidoon1/rust-rocksdb)

**A high-performance Rust wrapper for Facebook's RocksDB embeddable database.**

RocksDB is a fast key-value storage engine based on LSM-trees, optimized for SSDs with excellent performance for both reads and writes. This crate provides safe, idiomatic Rust bindings with support for all major RocksDB features including transactions, column families, backups, and advanced compression.

## 📋 Table of Contents

- [🚀 Quick Start](#-quick-start)
- [ Usage Examples](#-usage-examples)
- [⚙️ Features & Configuration](#️-features--configuration)
- [🔧 Building from Source](#-building-from-source)
- [🤝 Contributing](#-contributing)
- [❓ Why This Fork](#-why-this-fork)

## 🚀 Quick Start

**Requirements:**
- **Clang and LLVM** - Required for building RocksDB C++ components
- **Rust 1.91.0+** - Current MSRV (rolling 6-month policy)

Add this to your `Cargo.toml`:

```toml
[dependencies]
rust-rocksdb = "0.43"
```

### Basic Example

```rust
use rust_rocksdb::{DB, Options};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Open database
    let path = "./my_db";
    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = DB::open(&opts, path)?;

    // Write data
    db.put(b"key1", b"value1")?;
    
    // Read data
    match db.get(b"key1")? {
        Some(value) => println!("Retrieved: {}", String::from_utf8_lossy(&value)),
        None => println!("Key not found"),
    }

    // Delete data
    db.delete(b"key1")?;
    
    Ok(())
}
```

##  Usage Examples

### Working with Iterators

```rust
use rust_rocksdb::{DB, Options, IteratorMode};

let db = DB::open(&Options::default(), path)?;

// Insert some data
db.put(b"key1", b"value1")?;
db.put(b"key2", b"value2")?;
db.put(b"key3", b"value3")?;

// Iterate over all keys
let iter = db.iterator(IteratorMode::Start);
for (key, value) in iter {
    println!("{}: {}", 
        String::from_utf8_lossy(&key), 
        String::from_utf8_lossy(&value)
    );
}

// Iterate from a specific key
let iter = db.iterator(IteratorMode::From(b"key2", rust_rocksdb::Direction::Forward));
for (key, value) in iter {
    println!("{}: {}", 
        String::from_utf8_lossy(&key), 
        String::from_utf8_lossy(&value)
    );
}
```

### Using Column Families

```rust
use rust_rocksdb::{DB, Options, ColumnFamilyDescriptor};

let mut opts = Options::default();
opts.create_if_missing(true);

// Define column families
let cf_opts = Options::default();
let cf_descriptors = vec![
    ColumnFamilyDescriptor::new("users", cf_opts.clone()),
    ColumnFamilyDescriptor::new("posts", cf_opts),
];

let db = DB::open_cf_descriptors(&opts, path, cf_descriptors)?;

// Get column family handles
let users_cf = db.cf_handle("users").unwrap();
let posts_cf = db.cf_handle("posts").unwrap();

// Write to specific column families
db.put_cf(&users_cf, b"user:1", b"alice")?;
db.put_cf(&posts_cf, b"post:1", b"Hello World!")?;

// Read from specific column families
let user = db.get_cf(&users_cf, b"user:1")?;
```

### Using Transactions

```rust
use rust_rocksdb::{TransactionDB, TransactionDBOptions, TransactionOptions, Options, WriteOptions};

let mut opts = Options::default();
opts.create_if_missing(true);

let txn_db_opts = TransactionDBOptions::default();
let db = TransactionDB::open(&opts, &txn_db_opts, path)?;

// Start a transaction
let txn_opts = TransactionOptions::default();
let txn = db.transaction_opt(&WriteOptions::default(), &txn_opts);

// Perform operations within transaction
txn.put(b"key1", b"value1")?;
txn.put(b"key2", b"value2")?;

// Commit the transaction
txn.commit()?;
```

## ⚙️ Features & Configuration

### Compression Support

By default, support for [Snappy](https://github.com/google/snappy), [LZ4](https://github.com/lz4/lz4), [Zstd](https://github.com/facebook/zstd), [Zlib](https://zlib.net), and [Bzip2](http://www.bzip.org) compression is enabled. To enable only specific algorithms:

```toml
[dependencies.rust-rocksdb]
default-features = false
features = ["lz4"]  # Enable only LZ4 compression
```

**Available compression features:**
- `snappy` - Google's Snappy compression (fast, moderate compression)
- `lz4` - LZ4 compression (very fast, light compression)
- `zstd` - Zstandard compression (configurable speed/ratio tradeoff)
- `zlib` - Zlib compression (widely compatible)
- `bzip2` - Bzip2 compression (high compression ratio, slower)

### Performance Features

#### Jemalloc Memory Allocator

> **⚠️ Highly Recommended for Production**

```toml
[dependencies.rust-rocksdb]
features = ["jemalloc"]
```

Enables jemalloc memory allocator which significantly reduces memory fragmentation compared to libc malloc, especially for RocksDB workloads. 

**Platform Support:**
- **Supported platforms** (Linux, macOS): RocksDB will be properly informed that Jemalloc is enabled, allowing internal optimizations
- **Unsupported platforms**: [See build.rs](https://github.com/zaidoon1/rust-rocksdb/blob/master/librocksdb-sys/build.rs#L4-L7) - You still get Jemalloc benefits but some RocksDB internal optimizations are skipped

See [GitHub issue](https://github.com/facebook/rocksdb/issues/12364) for more details on memory fragmentation with RocksDB.

#### Malloc Usable Size

```toml
[dependencies.rust-rocksdb]
features = ["malloc-usable-size"]
```

Required if you want to use RocksDB's `optimize_filters_for_memory` feature. See [RocksDB documentation](https://github.com/facebook/rocksdb/blob/v9.0.0/include/rocksdb/table.h#L401-L434) for details.

### Platform-Specific Features

#### Multi-threaded Column Family Operations

```toml
[dependencies.rust-rocksdb]
features = ["multi-threaded-cf"]
```

Enables concurrent column family creation/deletion from multiple threads using `RwLock`. Alternatively, use `DBWithThreadMode<MultiThreaded>` directly.

#### Windows Runtime Library

```toml
[dependencies.rust-rocksdb]
features = ["mt_static"]
```

**Windows Only**: The `mt_static` feature requests the library to be built with the [/MT](https://learn.microsoft.com/en-us/cpp/build/reference/md-mt-ld-use-run-time-library?view=msvc-170) flag, which results in the library using the static version of the run-time library.

**Use case**: This can be useful when there's a conflict in the dependency tree between different run-time versions.

#### Bindgen Linking

**Dynamic Linking (Default)**:
```toml
[dependencies.rust-rocksdb]
features = ["bindgen-runtime"]  # Enabled by default
```

The `bindgen-runtime` feature enables the `runtime` feature of bindgen, which dynamically links to libclang. This is suitable for most platforms and is enabled by default.

**Static Linking (Alpine Linux/musl)**:
```toml
[dependencies.rust-rocksdb]
default-features = false
features = ["bindgen-static", "snappy", "lz4", "zstd", "zlib", "bzip2"]
```

The `bindgen-static` feature enables the `static` feature of bindgen, which statically links to libclang. This is suitable for musllinux platforms, such as Alpine Linux.

> **⚠️ Important**: The `runtime` and `static` features are mutually exclusive and won't compile if both are enabled.

### Advanced Features

#### ZSTD Dictionary Optimization

```toml
[dependencies.rust-rocksdb]
features = ["zstd", "zstd-static-linking-only"]
```

Holds digested dictionaries in block cache for read-heavy workloads. Uses experimental APIs but is production-tested at Facebook. See [Dictionary Compression Blog](https://rocksdb.org/blog/2021/05/31/dictionary-compression.html).

#### Async MultiGet with C++20 Coroutines

> **⚠️ Experimental, Linux only.** The feature builds and tests in CI but has not been exercised on real production workloads from this crate. Benchmark your specific workload before adopting.

```toml
[dependencies.rust-rocksdb]
features = ["coroutines", "io-uring"]
```

Builds RocksDB with `USE_COROUTINES=1` and links against [folly](https://github.com/facebook/folly). This enables the **multi-level parallel `MultiGet` path** described in the RocksDB [Asynchronous IO blog post](https://rocksdb.org/blog/2022/10/07/asynchronous-io-in-rocksdb.html). When you then call `ReadOptions::set_async_io(true)` on a `MultiGet`, RocksDB will issue parallel `io_uring` reads across SST files in different LSM levels, not just within a single level.

**Performance — read this carefully before adopting.** The RocksDB team's [October 2022 benchmark](https://rocksdb.org/blog/2022/10/07/asynchronous-io-in-rocksdb.html#results) was run on their internal **remote/warm-storage flash** (`ws.flash.ftw3preprod1`), where storage round-trip latency is roughly two to three orders of magnitude higher than a local NVMe random read:

| Configuration (remote/warm-storage flash) | μs/op |
|---|---|
| `async_io=false` (baseline — no `coroutines` feature needed) | 1292 |
| `async_io=true` + `coroutines`, `optimize_multiget_for_io=false` (single-level parallel) | 775 |
| `async_io=true` + `coroutines`, `optimize_multiget_for_io=true` (multi-level parallel, default) | 508 |

Both the 775 and 508 numbers require the `coroutines` feature. Without it, even setting `async_io=true` only buys you within-single-SST-file block prefetching (no parallel reads across files); you stay near the 1292 baseline for cross-file workloads.

The RocksDB team **has not published** an equivalent benchmark for local NVMe. The mechanism (`async_io` hides per-read latency by overlapping multiple reads in flight) implies the relative gain should be smaller on local NVMe, where per-read latency is already low — but the actual numbers there could be anywhere from "still meaningful" to "noise". **Treat the table above as remote-flash-only and measure on your hardware before deciding.** A reasonable rule of thumb: this feature is most likely to pay off when (a) your storage is network-attached or remote, (b) your `MultiGet` batches commonly span many SST files across multiple LSM levels, or (c) both. Trades ~6–15% extra CPU per the same blog post.

##### Prerequisites

This feature is harder to build than the rest of the crate. Read all of the constraints below before starting.

1. **Linux only.** macOS and Windows are not supported. Folly's build (`getdeps.py`) doesn't reliably work on macOS, and RocksDB's coroutine code path needs `io_uring`.
2. **liburing ≥ 2.7.** The pinned folly commit references `io_uring_zcrx_*` symbols from liburing 2.7 (`IoUringZeroCopyBufferPool.cpp`) and `IOU_PBUF_RING_INC` / `io_uring_buf_ring_head` from liburing 2.6 (`IoUringProvidedBufferRing.cpp`). Distro coverage:
   - Ubuntu 25.10+ (`liburing-dev` 2.11): works out of the box.
   - Ubuntu 24.04 LTS (`liburing-dev` 2.5): too old. `scripts/build_folly.sh` auto-detects this and builds liburing 2.9 from source under the scratch directory, then exports `PKG_CONFIG_PATH` so folly and rust-rocksdb's `io-uring` feature both pick it up.
   - Debian, RHEL, etc.: check `pkg-config --modversion liburing`; the script handles either case.
3. **A C/C++ compiler that is not GCC 15.** Folly's pinned libunwind dependency contains test code using legacy K&R-style empty parameter lists, which GCC 15 rejects under its default `-std=gnu23`. GCC 11–14 and Clang ≥ 14 all work. On Ubuntu 25.10 you can install `gcc-14`/`g++-14` from apt and switch via `update-alternatives` (see the CI workflow at `.github/workflows/coroutines.yml` for the exact commands).
4. **Build dependencies.** On Ubuntu / Debian:
   ```bash
   apt-get install -y build-essential cmake ninja-build python3 python3-pip \
     pkg-config patchelf wget \
     libdouble-conversion-dev libssl-dev liburing-dev \
     zlib1g-dev libbz2-dev autoconf automake libtool
   ```
   `wget` is needed because folly's getdeps shells out to it (`GETDEPS_USE_WGET=1`, inherited from RocksDB's own `folly.mk`).
5. **Build folly + its 8 transitive deps** (boost, fmt, glog, gflags, double-conversion, libevent, libsodium, fast_float, xz, lz4, zstd, snappy, libdwarf, libiberty, ...):
   ```bash
   ./scripts/build_folly.sh
   ```
   The script invokes folly's `getdeps.py` directly with `--scratch-path`, clones folly at the commit pinned by `librocksdb-sys/rocksdb/folly.mk:FOLLY_COMMIT_HASH`, applies the two upstream patches RocksDB's own `folly.mk` applies, and runs the build. Allow ~20–30 minutes on a cold cache. Outputs land under `librocksdb-sys/folly-build/installed/`.
6. **Build the crate**:
   ```bash
   export ROCKSDB_FOLLY_INSTALL_PATH="$PWD/librocksdb-sys/folly-build/installed"
   cargo build --release --features coroutines,io-uring
   ```

##### Runtime constraints

- **Dynamic dependencies on `libglog.so` and `libgflags.so`.** Folly's `getdeps` produces these two as shared libraries only (no static archives). Your final binary therefore has runtime `.so` dependencies on them. Cargo's `rustc-link-arg` does not propagate from a transitive `-sys` crate to a downstream binary ([rust-lang/cargo#9554](https://github.com/rust-lang/cargo/issues/9554)), so rust-rocksdb cannot embed `rpath` on your binary for you. Pick one of:

  - **Set `LD_LIBRARY_PATH`** when running the binary. Concretely:
    ```bash
    GLOG_LIBDIR=$(ls -d "$ROCKSDB_FOLLY_INSTALL_PATH"/glog-*/lib* | head -1)
    GFLAGS_LIBDIR=$(ls -d "$ROCKSDB_FOLLY_INSTALL_PATH"/gflags-*/lib* | head -1)
    export LD_LIBRARY_PATH="$GLOG_LIBDIR:$GFLAGS_LIBDIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    ./your-binary
    ```
    This is what `.github/workflows/coroutines.yml` does for test binaries; reference it for a working example.

  - **Embed `rpath` in your final binary crate's `build.rs`.** This must live in the crate that produces the binary you're shipping (a `[[bin]]` target), **NOT** in an intermediate library crate — `cargo:rustc-link-arg` does not propagate through transitive library dependencies, so adding this to a library crate would have the same problem as embedding rpath here in rust-rocksdb. This crate exports the discovered glog and gflags lib directories via the `links` metadata, available in your binary's build script as `DEP_ROCKSDB_FOLLY_GLOG_LIBDIR` and `DEP_ROCKSDB_FOLLY_GFLAGS_LIBDIR`:
    ```rust
    // your-binary-crate/build.rs
    fn main() {
        if let Ok(d) = std::env::var("DEP_ROCKSDB_FOLLY_GLOG_LIBDIR") {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{d}");
        }
        if let Ok(d) = std::env::var("DEP_ROCKSDB_FOLLY_GFLAGS_LIBDIR") {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{d}");
        }
    }
    ```

  - **System-install the `.so` files** (copy them into `/usr/local/lib` and run `ldconfig`).

- **Not compatible with `mt_static`.** Folly's build precludes producing a fully static link.
- **`optimize_multiget_for_io` is a tuning knob within the coroutine-enabled space, not an on/off switch for coroutines.** The flag controls whether `MultiGet` parallelizes reads *across* LSM levels (`true`, default) or only *within* a single level (`false`). Both rely on the coroutine machinery this feature compiles in; without the `coroutines` feature, neither path runs and `MultiGet` falls back to the synchronous one-file-at-a-time loop. Per the performance table above, turning it off keeps ~40% of the latency reduction (1292→775 μs/op) at lower CPU cost than the multi-level path (1292→508). The flag currently cannot be set from Rust until [facebook/rocksdb#14752](https://github.com/facebook/rocksdb/pull/14752) merges and we bump the submodule; the C++ default of `true` is the right starting point for most workloads.
- **The folly install is large and slow to rebuild.** ~2 GB on disk. Cache `librocksdb-sys/folly-build/` between CI runs (see `.github/workflows/coroutines.yml` for an `actions/cache` example with a cache key that pins the OS image, arch, and `FOLLY_COMMIT_HASH`).

##### Verifying the feature is active

At runtime:

```rust
assert!(rust_rocksdb::built_with_coroutines());
```

Note: this reflects how rust-rocksdb was *compiled* (i.e. whether the `coroutines` feature was on). If you used `ROCKSDB_LIB_DIR` to link against an externally-built `librocksdb.a`, the answer here may not match what that library was actually built with.

#### Link-Time Optimization (LTO)

```toml
[dependencies.rust-rocksdb]
features = ["lto"]
```

> **⚠️ CRITICAL REQUIREMENTS**
> 
> - **Must use clang**: `CC=/usr/bin/clang CXX=/usr/bin/clang++`
> - **Clang LLVM version must match Rust compiler**
> - **Rust flags**: `RUSTFLAGS="-Clinker-plugin-lto -Clinker=clang -Clink-arg=-fuse-ld=lld"`

```bash
CC=/usr/bin/clang CXX=/usr/bin/clang++ \
RUSTFLAGS="-Clinker-plugin-lto -Clinker=clang -Clink-arg=-fuse-ld=lld" \
cargo build --release --features lto
```

See [Rust LTO documentation](https://doc.rust-lang.org/rustc/linker-plugin-lto.html) for details.

## 🔧 Building from Source

Clone with submodules for RocksDB and compression libraries:

```bash
git clone --recursive https://github.com/zaidoon1/rust-rocksdb.git
cd rust-rocksdb

# Or if already cloned:
git submodule update --init --recursive
```

### Linking Against a Prebuilt RocksDB

By default, `rust-rocksdb` builds RocksDB from the bundled submodule. To link against a system-installed `librocksdb` instead (e.g. to share a single library across multiple Rust projects, cut compile time, or use a distro's package), the build script honors these opt-in environment variables:

| Variable                   | Effect                                                                                                       |
| -------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `ROCKSDB_USE_PKG_CONFIG=1` | Probe `pkg-config rocksdb` to discover lib + include paths automatically. Accepts `1` or `true`.             |
| `ROCKSDB_LIB_DIR=<path>`   | Look for `librocksdb.{a,so,dylib,dll}` in `<path>`. **Requires `ROCKSDB_INCLUDE_DIR` to be set too.**        |
| `ROCKSDB_STATIC`           | Static-link the system rocksdb (default is dynamic). Any non-empty value enables it (legacy semantics).      |
| `ROCKSDB_INCLUDE_DIR=<p>`  | Headers for `bindgen`. Mandatory with `ROCKSDB_LIB_DIR`; with `ROCKSDB_USE_PKG_CONFIG` it is *merged in front* of pkg-config's discovered paths (does not replace them). |
| `ROCKSDB_COMPILE=1`        | Force the bundled vendored build even if the above are set. Accepts `1` or `true` (case-insensitive).        |
| `ROCKSDB_CXX_STD=c++23`    | Override the C++ standard used to compile RocksDB (default `c++20`). Only used for vendored builds.          |
| `CXXSTDLIB=stdc++`         | Override the C++ stdlib linked (e.g. `c++` for libc++, `stdc++` for libstdc++).                              |

When you opt in via any of these:

- `bindgen` runs against the **chosen backend's headers** (system rocksdb when linked from the system, bundled otherwise), so the generated FFI cannot silently drift from the linked library. If no include directory can be determined, the build script panics with an actionable error rather than guessing `/usr/include`.
- No version pin is enforced &mdash; you're the power user. The bundled RocksDB version is the trailing component of `librocksdb-sys`'s `version = "X.Y.Z+RR.S.T"` in `Cargo.toml`; make sure your system rocksdb is API-compatible.
- The `snappy` Cargo feature becomes a no-op: the system librocksdb is expected to provide snappy support itself, so building and linking a second copy would risk duplicate symbols. The build script emits a `cargo::warning=` so the silent skip isn't surprising.
- The `coroutines` Cargo feature still emits folly link directives, but the build script emits a `cargo::warning=` reminding you that your prebuilt librocksdb must have been built with `USE_COROUTINES=1` and `USE_FOLLY=1` &mdash; otherwise you'll get unresolved-symbol link errors against folly.

Same set of variables exists for snappy (`SNAPPY_LIB_DIR`, `SNAPPY_STATIC`, `SNAPPY_COMPILE`) if you'd like to swap in a system libsnappy while keeping the bundled rocksdb.

#### Examples

```bash
# Use distro-installed librocksdb via pkg-config (Debian/Ubuntu: librocksdb-dev)
ROCKSDB_USE_PKG_CONFIG=1 cargo build

# Manual path; static link
ROCKSDB_LIB_DIR=/opt/rocksdb/lib \
ROCKSDB_INCLUDE_DIR=/opt/rocksdb/include \
ROCKSDB_STATIC=1 \
  cargo build
```

#### Downstream `-sys` Integration

Crates that depend on `rust-librocksdb-sys` and want access to its outputs can read these via the `links = "rocksdb"` metadata channel:

| Build env var                     | Source                                                                                                                |
| --------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| `DEP_ROCKSDB_INCLUDE`             | Path to the RocksDB headers in use. Always a single path; downstream crates needing the full pkg-config set should probe pkg-config themselves. |
| `DEP_ROCKSDB_ROOT`                | `OUT_DIR` of `rust-librocksdb-sys`.                                                                                   |
| `DEP_ROCKSDB_LINK_TARGET`         | Name of the linked library (always `rocksdb`). Project-local convenience; equivalently readable from `CARGO_MANIFEST_LINKS`. |
| `DEP_ROCKSDB_FOLLY_GLOG_LIBDIR`   | glog lib dir (only with `coroutines` feature).                                                                        |
| `DEP_ROCKSDB_FOLLY_GFLAGS_LIBDIR` | gflags lib dir (only with `coroutines` feature).                                                                      |
| `DEP_ROCKSDB_CARGO_MANIFEST_DIR`  | *Legacy*. Manifest dir of `rust-librocksdb-sys`. Kept for backwards compatibility; prefer `DEP_ROCKSDB_INCLUDE` for header discovery. |
| `DEP_ROCKSDB_OUT_DIR`             | *Legacy*. Alias for `DEP_ROCKSDB_ROOT`. Kept for backwards compatibility.                                             |

## 🤝 Contributing

Feedback and pull requests welcome! Open an issue for feature requests or submit PRs. This fork maintains regular updates with latest RocksDB releases and Rust versions.

**Current MSRV**: 1.91.0 (rolling 6-month policy)

## ❓ Why This Fork

This fork of the original [rust-rocksdb](https://github.com/rust-rocksdb/rust-rocksdb) focuses on:

- **Regular updates** with latest RocksDB releases
- **Modern Rust support** with up-to-date MSRV policy  
- **Active maintenance** and quick issue resolution
- **Performance optimizations** and new feature integration

---

## 📚 Resources

- **[API Documentation](https://docs.rs/rust-rocksdb)** - Complete API reference
- **[RocksDB Wiki](https://github.com/facebook/rocksdb/wiki)** - Upstream documentation
- **[RocksDB Tuning Guide](https://github.com/facebook/rocksdb/wiki/RocksDB-Tuning-Guide)** - Performance tuning

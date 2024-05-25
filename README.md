rust-rocksdb
============

[![RocksDB build](https://github.com/zaidoon1/rust-rocksdb/actions/workflows/rust.yml/badge.svg?branch=master)](https://github.com/zaidoon1/rust-rocksdb/actions/workflows/rust.yml)
[![crates.io](https://img.shields.io/crates/v/rust-rocksdb.svg)](https://crates.io/crates/rust-rocksdb)
[![documentation](https://docs.rs/rust-rocksdb/badge.svg)](https://docs.rs/rust-rocksdb)
[![license](https://img.shields.io/crates/l/rust-rocksdb.svg)](https://github.com/zaidoon1/rust-rocksdb/blob/master/LICENSE)
![rust 1.75.0 required](https://img.shields.io/badge/rust-1.75.0-blue.svg?label=MSRV)

![GitHub commits (since latest release)](https://img.shields.io/github/commits-since/zaidoon1/rust-rocksdb/latest.svg)

## Why The Fork

The original [rust-rocksdb repo](https://github.com/rust-rocksdb/rust-rocksdb) is amazing and I appreciate all the work that has
been done, however, for my use case, I need to stay up to date with the latest
rocksdb releases as well as the latest rust releases so in order to to keep
everything up to date, I decided to fork the original repo so I can have total
control and be able to create regular releases.

## Requirements

- Clang and LLVM

## Rust version

rust-rocksdb keeps a rolling MSRV (minimum supported Rust version) policy of 6 months. This means we will accept PRs that upgrade the MSRV as long as the new Rust version used is at least 6 months old.

Our current MSRV is 1.75.

## Contributing

Feedback and pull requests welcome! If a particular feature of RocksDB is
important to you, please let me know by opening an issue, and I'll
prioritize it.

## Usage

This binding is statically linked with a specific version of RocksDB. If you
want to build it yourself, make sure you've also cloned the RocksDB and
compression submodules:

```shell
git submodule update --init --recursive
```

## Features

### Compression Support

By default, support for [Snappy](https://github.com/google/snappy),
[LZ4](https://github.com/lz4/lz4), [Zstd](https://github.com/facebook/zstd),
[Zlib](https://zlib.net), and [Bzip2](http://www.bzip.org) compression
is enabled through crate features. If support for all of these compression
algorithms is not needed, default features can be disabled and specific
compression algorithms can be enabled. For example, to enable only LZ4
compression support, make these changes to your Cargo.toml:

```toml
[dependencies.rocksdb]
default-features = false
features = ["lz4"]
```

### Multithreaded ColumnFamily alternation

RocksDB allows column families to be created and dropped
from multiple threads concurrently, but this crate doesn't allow it by default
for compatibility. If you need to modify column families concurrently, enable
the crate feature `multi-threaded-cf`, which makes this binding's
data structures use `RwLock` by default. Alternatively, you can directly create
`DBWithThreadMode<MultiThreaded>` without enabling the crate feature.

### Switch between /MT or /MD run time library (Only for Windows)

The feature `mt_static` will request the library to be built with [/MT](https://learn.microsoft.com/en-us/cpp/build/reference/md-mt-ld-use-run-time-library?view=msvc-170)
flag, which results in library using the static version of the run-time library.
*This can be useful in case there's a conflict in the dependecy tree between different
run-time versions.*

### Jemalloc

The feature `jemalloc` will enable the
`unprefixed_malloc_on_supported_platforms` feature of `tikv-jemalloc-sys`,
hooking the actual malloc and free, so jemalloc is used to allocate memory. On
Supported platforms such as Linux, Rocksdb will also be properly informed that
Jemalloc is enabled so that it can apply internal optimizations gated behind
Jemalloc being enabled. On [unsupported
platforms](https://github.com/zaidoon1/rust-rocksdb/blob/master/librocksdb-sys/build.rs#L4-L7),
Rocksdb won't be properly
informed that Jemalloc is being used so some internal optimizations are skipped
BUT you will still get the benefits of Jemalloc memory allocation. Note that by
default, Rust uses libc malloc on Linux which is known to have more memory
fragmentation than Jemalloc especially with Rocksdb. See [github
issue](https://github.com/facebook/rocksdb/issues/12364) for more information.
In general, I highly suggest enabling Jemalloc unless there is a specific reason
not to (your system doesn't support it, etc.)

### Malloc Usable Size

The feature `malloc-usable-size` will inform Rocksdb that malloc_usable_size is
supported by the platform and is necessary if you want to use the
`optimize_filters_for_memory` rocksdb feature as this feature is gated behind
malloc_usable_size being available. See
[rocksdb](https://github.com/facebook/rocksdb/blob/v9.0.0/include/rocksdb/table.h#L401-L434)
for more information on the feature.

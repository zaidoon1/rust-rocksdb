# rust-rocksdb

[![RocksDB build](https://github.com/zaidoon1/rust-rocksdb/actions/workflows/rust.yml/badge.svg?branch=master)](https://github.com/zaidoon1/rust-rocksdb/actions/workflows/rust.yml)
[![crates.io](https://img.shields.io/crates/v/rust-rocksdb.svg)](https://crates.io/crates/rust-rocksdb)
[![documentation](https://docs.rs/rust-rocksdb/badge.svg)](https://docs.rs/rust-rocksdb)
[![license](https://img.shields.io/crates/l/rust-rocksdb.svg)](https://github.com/zaidoon1/rust-rocksdb/blob/master/LICENSE)
![rust 1.89.0 required](https://img.shields.io/badge/rust-1.89.0-blue.svg?label=MSRV)
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
- **Rust 1.89.0+** - Current MSRV (rolling 6-month policy)

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

## 🤝 Contributing

Feedback and pull requests welcome! Open an issue for feature requests or submit PRs. This fork maintains regular updates with latest RocksDB releases and Rust versions.

**Current MSRV**: 1.89.0 (rolling 6-month policy)

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

# rust-librocksdb-sys

Low-level bindings to [RocksDB's](https://github.com/facebook/rocksdb) C API.

Based on the original work by Tyler Neely (https://github.com/rust-rocksdb/rust-rocksdb)
and Jeremy Fitzhardinge (https://github.com/jsgf/rocksdb-sys).

## Version

The librocksdb-sys version number is in the format `X.Y.Z+RX.RY.RZ`, where:

- `X.Y.Z` is the version of this crate and follows SemVer conventions
- `RX.RY.RZ` is the version of the bundled RocksDB

## Build Configuration

This crate supports two primary build modes:

### 1. Vendored Build (Default)

Builds RocksDB from source code included in this crate. This is the default behavior and ensures compatibility across all platforms.

```toml
# Cargo.toml
[dependencies]
rust-librocksdb-sys = "0.39.0"
```

### 2. System Library

Links against a pre-installed RocksDB library on your system. This can reduce build times and binary size.

```toml
# Cargo.toml
[dependencies]
rust-librocksdb-sys = { version = "0.39.0", features = ["no-vendor"] }
```

Or via environment variables:

```bash
ROCKSDB_LIB_DIR=/usr/local/lib cargo build
```

## Feature Flags

### Build Source Features

- `vendored` - Build RocksDB from vendored sources (included by default)
- `no-vendor` - Use system-installed RocksDB library instead of building from source

### Linking Strategy Features

- `static` - Prefer static linking (default)
- `dynamic` - Prefer dynamic linking
- `static-only` - Force static linking only

### Compression Features

- `snappy` - Enable Snappy compression support
- `lz4` - Enable LZ4 compression support
- `zstd` - Enable Zstandard compression support
- `zlib` - Enable zlib compression support
- `bzip2` - Enable bzip2 compression support

### Advanced Features

- `jemalloc` - Use jemalloc memory allocator
- `io-uring` - Enable io_uring support (Linux only)
- `rtti` - Enable RTTI (Run-Time Type Information)
- `malloc-usable-size` - Enable malloc_usable_size feature
- `lto` - Enable Link-Time Optimization (requires clang)
- `mt_static` - Use static MSVC runtime (Windows only)

## Environment Variables

The build process can be configured using environment variables:

### RocksDB Configuration

- `ROCKSDB_LIB_DIR` - Directory containing RocksDB library files
- `ROCKSDB_INCLUDE_DIR` - Directory containing RocksDB header files
- `ROCKSDB_STATIC` - Set to `1` to force static linking
- `ROCKSDB_COMPILE` - Set to `1` to force compilation from source even if system lib is found
- `ROCKSDB_CXX_STD` - C++ standard to use (default: `c++17`)

### Compression Libraries

Similar environment variables are available for compression libraries:

- `SNAPPY_LIB_DIR`, `SNAPPY_STATIC`
- `LZ4_LIB_DIR`, `LZ4_STATIC`
- `ZSTD_LIB_DIR`, `ZSTD_STATIC`
- `Z_LIB_DIR`, `Z_STATIC` (for zlib)
- `BZIP2_LIB_DIR`, `BZIP2_STATIC`

## Usage Examples

### Using System RocksDB with Dynamic Linking

```bash
# Install RocksDB on your system first
# Ubuntu/Debian:
sudo apt-get install librocksdb-dev

# macOS:
brew install rocksdb

# Build with system library
cargo build --features "no-vendor,dynamic"
```

### Custom RocksDB Installation

```bash
# Point to custom RocksDB installation
export ROCKSDB_LIB_DIR=/opt/rocksdb/lib
export ROCKSDB_INCLUDE_DIR=/opt/rocksdb/include
cargo build --features "no-vendor"
```

### Building with Specific Compression Support

```bash
# Build with Snappy and LZ4 compression
cargo build --features "snappy,lz4"
```

### Static Linking with All Compressions

```bash
cargo build --features "static-only,snappy,lz4,zstd,zlib,bzip2"
```

### Cross-Compilation

```bash
# Example for Android
export TARGET=aarch64-linux-android
export CC=$ANDROID_NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin/aarch64-linux-android21-clang
export CXX=$ANDROID_NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin/aarch64-linux-android21-clang++
cargo build --target $TARGET
```

## Platform-Specific Notes

### Linux

- io_uring support available with `io-uring` feature (requires liburing)
- Default uses system allocator, jemalloc optional

### macOS

- Homebrew RocksDB can be used with `no-vendor` feature
- Universal binaries supported for Apple Silicon

### Windows

- MSVC and GNU toolchains supported
- Use `mt_static` feature for static MSVC runtime
- Requires Visual Studio or MinGW-w64

### FreeBSD

- System RocksDB from ports/packages works well
- Use: `ROCKSDB_LIB_DIR=/usr/local/lib cargo build --features "no-vendor"`

### Android

- Requires Android NDK
- Set appropriate `CC` and `CXX` for target architecture
- jemalloc not supported

## Troubleshooting

### Build Failures

#### "The 'rocksdb' directory is empty"

```bash
# Initialize git submodules
git submodule update --init --recursive
```

#### "Cannot find RocksDB headers"

```bash
# Install development headers
# Ubuntu/Debian:
sudo apt-get install librocksdb-dev

# Or use vendored build:
cargo build --features "vendored"
```

#### "LTO requires clang"

```bash
# Either disable LTO:
cargo build --no-default-features --features "vendored,static"

# Or use clang:
export CC=clang
export CXX=clang++
cargo build --features "lto"
```

#### Linking Errors

```bash
# Force static linking:
cargo build --features "static-only"

# Or force dynamic linking:
cargo build --features "dynamic"
```

### Performance Considerations

1. **Compression**: Enable only needed compression algorithms to reduce binary size
2. **LTO**: Can improve performance but increases build time significantly
3. **jemalloc**: Can improve performance for high-throughput applications
4. **Static vs Dynamic**: Static linking increases binary size but simplifies deployment

## Build Time Optimization

To speed up builds during development:

1. **Use system libraries**: `--features "no-vendor"` avoids rebuilding RocksDB
2. **Disable unnecessary features**: Only enable compressions you actually use
3. **Use sccache or ccache**: Cache C++ compilation results
4. **Parallel compilation**: Enabled by default via cc crate

## Contributing

Contributions are welcome! Please note:

1. The build.rs is organized into logical sections - maintain this structure
2. Add clear error messages for new failure modes
3. Test both vendored and system builds
4. Update this README for new features or environment variables


//! `librocksdb-sys` build script.
//!
//! High-level flow:
//!
//! 1. Read target info from `CARGO_CFG_*` env vars (typed in [`Target`]).
//! 2. Pick a [`Backend`] (vendored sources or prebuilt system library)
//!    based on env vars; see [`Backend::resolve`] for the precedence rules.
//! 3. Build vendored, or emit the system link directives.
//! 4. Link the platform runtime libs RocksDB needs regardless of backend
//!    (Windows rpcrt4/shlwapi, riscv64 libatomic, the C++ stdlib).
//! 5. Resolve `snappy` the same way if the `snappy` feature is on.
//! 6. Run `bindgen` against the *chosen* backend's headers so the
//!    generated FFI cannot drift from the linked library.
//! 7. Emit `cargo::metadata=` entries for downstream crates.
//!
//! See the project README for the full list of environment variables.
//! Every variable that can influence the build is registered with
//! `cargo::rerun-if-env-changed=` at the top of [`main`].

use std::env;
use std::path::{Path, PathBuf};

// =========================================================================
// Constants
// =========================================================================

/// On these platforms `jemalloc-sys` uses a prefixed jemalloc that cannot be
/// linked together with RocksDB's own usage of jemalloc symbols.
/// See <https://github.com/tikv/jemallocator/blob/f7adfca5aff272b43fd3ad896252b57fbbd9c72a/jemalloc-sys/src/env.rs#L24>.
/// Additionally, FreeBSD comes with jemalloc out of the box, so we don't
/// need to recompile it.
const NO_JEMALLOC_TARGETS: &[&str] = &["android", "dragonfly", "darwin", "freebsd"];

/// Default C++ standard used to build RocksDB. Can be overridden with
/// `ROCKSDB_CXX_STD`.
const DEFAULT_CXX_STD: &str = "c++20";

// =========================================================================
// Target info
// =========================================================================

/// Snapshot of the **target** platform (NOT the host). Always read from
/// `CARGO_CFG_TARGET_*` env vars, never from `#[cfg(target_os=...)]`
/// attributes — those reflect the host machine where the build script
/// runs, which is wrong when cross-compiling.
struct Target {
    /// Full target triple, e.g. `x86_64-unknown-linux-gnu`.
    triple: String,
    /// e.g. `linux`, `macos`, `windows`, `ios`, `android`, `freebsd`.
    os: String,
    /// e.g. `x86_64`, `aarch64`, `riscv64`.
    arch: String,
    /// e.g. `gnu`, `musl`, `msvc`, or empty.
    env_abi: String,
    /// 32 or 64.
    pointer_width: u32,
    /// `little` or `big`.
    endian: String,
    /// Target features enabled at build time, e.g. `sse4.2`, `avx2`, `crc`.
    features: Vec<String>,
    /// Argument to `-Ctarget-cpu=`, if set in `RUSTFLAGS`.
    rust_target_cpu: Option<String>,
}

impl Target {
    fn from_env() -> Self {
        let features = env::var("CARGO_CFG_TARGET_FEATURE")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();

        Self {
            triple: env::var("TARGET").expect("TARGET is always set by Cargo"),
            os: env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS"),
            arch: env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH"),
            env_abi: env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default(),
            pointer_width: env::var("CARGO_CFG_TARGET_POINTER_WIDTH")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(64),
            endian: env::var("CARGO_CFG_TARGET_ENDIAN").expect("CARGO_CFG_TARGET_ENDIAN"),
            features,
            rust_target_cpu: parse_rust_target_cpu(),
        }
    }

    fn has_feature(&self, name: &str) -> bool {
        self.features.iter().any(|f| f == name)
    }

    fn is_msvc(&self) -> bool {
        self.env_abi == "msvc"
    }
}

/// Parse the `target-cpu` option from `CARGO_ENCODED_RUSTFLAGS`, if present.
fn parse_rust_target_cpu() -> Option<String> {
    const TARGET_CPU: &str = "target-cpu";
    // use rustflags since parsing is annoying
    // e.g. "-Ctarget-cpu=native" and "-C target-cpu=native" are equivalent
    rustflags::from_env().find_map(|flag| match flag {
        rustflags::Flag::Codegen { opt, value } if opt == TARGET_CPU => value,
        _ => None,
    })
}

// =========================================================================
// Backend resolution
// =========================================================================

/// Which RocksDB build path was chosen, plus the include director(ies)
/// that downstream stages (bindgen, downstream crates) should use.
enum Backend {
    /// Build RocksDB from the bundled `rocksdb/` submodule sources.
    Vendored { include: PathBuf },
    /// Link against an already-built RocksDB. The build script has already
    /// emitted the `cargo::rustc-link-*` directives; `includes` is the set
    /// of header directories for bindgen to scan. Empty means we couldn't
    /// determine one — [`bindings::generate`] panics with a helpful error
    /// in that case rather than silently misbinding.
    System { includes: Vec<PathBuf> },
}

impl Backend {
    /// Decide the backend and emit any needed link directives for the
    /// system path. Vendored compilation is deferred to [`vendor::build`].
    fn resolve(target: &Target) -> Self {
        // Highest priority: explicit "force compile" override.
        if env_truthy("ROCKSDB_COMPILE") {
            return Backend::Vendored {
                include: vendored_include(),
            };
        }

        // Opt-in pkg-config probe.
        if env_truthy("ROCKSDB_USE_PKG_CONFIG") {
            return system::probe_pkg_config();
        }

        // Explicit lib-dir override.
        if env::var_os("ROCKSDB_LIB_DIR").is_some() {
            return system::from_lib_dir_env();
        }

        // Fall through to the system library at the
        // platform's conventional location.
        if target.os == "freebsd" {
            return system::from_freebsd_defaults();
        }

        // Default: build vendored.
        Backend::Vendored {
            include: vendored_include(),
        }
    }

    /// All include directories for this backend. Bindgen passes each as
    /// `-I<path>` so headers split across multiple roots (rare but happens
    /// with chained pkg-config deps) are all visible. Downstream metadata
    /// emission (`emit_metadata`) takes the first one as the canonical
    /// `DEP_ROCKSDB_INCLUDE` path.
    ///
    /// Empty slice means the backend couldn't determine a header location;
    /// `bindings::generate` is responsible for panicking with an
    /// actionable error in that case.
    fn all_includes(&self) -> &[PathBuf] {
        match self {
            Backend::Vendored { include } => std::slice::from_ref(include),
            Backend::System { includes } => includes,
        }
    }
}

// =========================================================================
// Main
// =========================================================================

fn main() {
    rerun_if_env_changed(&[
        // RocksDB
        "ROCKSDB_COMPILE",
        "ROCKSDB_LIB_DIR",
        "ROCKSDB_STATIC",
        "ROCKSDB_INCLUDE_DIR",
        "ROCKSDB_USE_PKG_CONFIG",
        "ROCKSDB_CXX_STD",
        // Snappy
        "SNAPPY_COMPILE",
        "SNAPPY_LIB_DIR",
        "SNAPPY_STATIC",
        // Compiler
        "CC",
        "CXX",
        "CFLAGS",
        "CXXFLAGS",
        "CXXSTDLIB",
        "CARGO_ENCODED_RUSTFLAGS",
        // Bindgen
        "BINDGEN_EXTRA_CLANG_ARGS",
        // pkg-config
        "PKG_CONFIG_PATH",
        "PKG_CONFIG_LIBDIR",
        "PKG_CONFIG_ALL_STATIC",
        "PKG_CONFIG_ALLOW_CROSS",
        "PKG_CONFIG_SYSROOT_DIR",
        // Compression-sys upstream metadata. These are set by cargo via the
        // `links` mechanism, but cargo does NOT automatically register them
        // for rerun tracking — we must do that ourselves or the build will
        // silently use stale paths after an upstream sys-crate bumps.
        "DEP_LZ4_INCLUDE",
        "DEP_ZSTD_INCLUDE",
        "DEP_Z_INCLUDE",
        "DEP_BZIP2_INCLUDE",
        "DEP_JEMALLOC_ROOT",
        // coroutines
        "ROCKSDB_FOLLY_INSTALL_PATH",
    ]);

    let target = Target::from_env();

    #[cfg(feature = "coroutines")]
    coroutines::validate_target(&target);

    let backend = Backend::resolve(&target);

    // Re-run build.rs if the local C-API extensions change. The
    // extensions are a small handful of files outside the submodule
    // (see librocksdb-sys/c-api-extensions/) compiled and linked into
    // every build, vendored or system. There's no patch application
    // step: the extensions just add new symbols additively.
    println!("cargo::rerun-if-changed=c-api-extensions/");

    match &backend {
        Backend::Vendored { .. } => {
            println!("cargo::rerun-if-changed=rocksdb/");
            ensure_submodule_present("rocksdb");
            vendor::build(&target);
        }
        Backend::System { .. } => {
            // Link directives already emitted by `Backend::resolve`.
            // We still need to link the C++ stdlib explicitly because
            // there's no `cc::Build` to do it for us.
            cpp_link_stdlib(&target);
            // The C-API extensions are not part of the user's installed
            // librocksdb, so we compile them ourselves and link the
            // resulting tiny static archive into the final binary
            // alongside the system rocksdb.
            extensions::build_for_system_backend(&target, &backend);
        }
    }

    // Platform runtime libs needed regardless of backend.
    apply_platform_runtime_libs(&target);

    #[cfg(feature = "coroutines")]
    {
        coroutines::warn_if_system_backend(&backend);
        coroutines::link();
    }

    if cfg!(feature = "snappy") {
        snappy::ensure(&target, &backend);
    }

    bindings::generate(&backend);

    emit_metadata(&backend);
}

/// Link platform-specific runtime libs that RocksDB needs whether built
/// vendored or linked from the system.
fn apply_platform_runtime_libs(target: &Target) {
    if target.os == "windows" {
        // RocksDB's port_win uses RPC + path-string APIs.
        println!("cargo::rustc-link-lib=dylib=rpcrt4");
        println!("cargo::rustc-link-lib=dylib=shlwapi");
    }

    // riscv64 needs libatomic for some 64-bit atomic ops on this arch.
    if target.arch == "riscv64" {
        println!("cargo::rustc-link-lib=atomic");
    }
}

/// Emit the platform-appropriate C++ standard library link. Called on
/// the system-link path; the vendored path lets `cc::Build` link the C++
/// stdlib automatically as part of `compile()`.
fn cpp_link_stdlib(target: &Target) {
    if let Ok(stdlib) = env::var("CXXSTDLIB") {
        if !stdlib.is_empty() {
            println!("cargo::rustc-link-lib=dylib={stdlib}");
        }
        return;
    }
    match target.os.as_str() {
        // Apple platforms, the BSDs, and Android (NDK r18+, 2018) all
        // default to libc++.
        "macos" | "ios" | "tvos" | "watchos" | "freebsd" | "openbsd" | "android" => {
            println!("cargo::rustc-link-lib=dylib=c++");
        }
        "aix" => {
            println!("cargo::rustc-link-lib=dylib=c++");
            println!("cargo::rustc-link-lib=dylib=c++abi");
        }
        "linux" | "netbsd" | "dragonfly" => {
            println!("cargo::rustc-link-lib=dylib=stdc++");
        }
        // Windows-MSVC links the CRT automatically.
        _ => {}
    }
}

/// Emit `cargo::metadata=` entries for downstream build scripts.
///
/// Cargo's `links = "rocksdb"` mechanism re-exposes each `KEY=VALUE`
/// emitted here to dependent crates as `DEP_ROCKSDB_<KEY>=VALUE`.
///
/// Keys emitted (mirroring `libz-sys`, `openssl-sys`, etc. where there
/// is a shared convention):
///
/// - `include` — path to the RocksDB headers. A single path, even when
///   bindgen sees multiple via pkg-config; downstream crates needing
///   the full set should probe pkg-config themselves.
/// - `root` — `OUT_DIR` of this crate.
/// - `link-target` — name of the linked library (always `rocksdb`).
///   Project-local key; equivalently readable as `CARGO_MANIFEST_LINKS`.
/// - `cargo_manifest_dir` — manifest dir of this crate.
/// - `out_dir` — alias for `root`.
fn emit_metadata(backend: &Backend) {
    if let Some(inc) = backend.all_includes().first() {
        println!("cargo::metadata=include={}", inc.display());
    }
    println!("cargo::metadata=root={}", out_dir().display());
    println!("cargo::metadata=link-target=rocksdb");
    println!(
        "cargo::metadata=cargo_manifest_dir={}",
        manifest_dir().display()
    );
    println!("cargo::metadata=out_dir={}", out_dir().display());
}

// =========================================================================
// Vendored build
// =========================================================================

mod vendor {
    use super::*;

    /// Compile RocksDB from the bundled submodule sources.
    pub(super) fn build(target: &Target) {
        let mut cfg = base_cfg(target);

        apply_compression_features(&mut cfg);
        apply_optional_features(&mut cfg, target);
        apply_jemalloc(&mut cfg, target);
        apply_target_cpu(&mut cfg, target);
        apply_target_arch(&mut cfg, target);
        let layout = apply_target_os(&mut cfg, target);
        apply_lfs_defines(&mut cfg, target, layout);
        apply_io_uring(&mut cfg, target);
        apply_backtrace(&mut cfg, target);

        #[cfg(feature = "coroutines")]
        super::coroutines::apply_compile_config(&mut cfg);

        for src in collect_sources(target, layout) {
            cfg.file(format!("rocksdb/{src}"));
        }
        cfg.file("build_version.cc");
        // Local C-API extensions are compiled as a regular translation
        // unit alongside the submodule's `db/c.cc`. The two end up in the
        // same `librocksdb.a`; the linker resolves the new symbols out of
        // the extension's `.o` and everything else out of the submodule's.
        cfg.file("c-api-extensions/c_api_extensions.cc");

        if !target.is_msvc() {
            // Force-include <cstdint>. Some translation units use uintN_t
            // through transitive includes that break under stricter GCC
            // versions (e.g. GCC 15 hides them by default).
            cfg.flag("-include").flag("cstdint");
        }

        cfg.cpp(true);
        cfg.compile("librocksdb.a");
    }

    /// Base `cc::Build` with the always-on flags, defines, includes, and
    /// warning settings. The local `c-api-extensions/` directory is
    /// prepended to the include path so `c_api_extensions.cc` finds its
    /// own header via `#include "c_api_extensions.h"` without needing a
    /// fully qualified path.
    fn base_cfg(target: &Target) -> cc::Build {
        let mut cfg = cc::Build::new();

        cfg.include("c-api-extensions/")
            .include("rocksdb/include/")
            .include("rocksdb/")
            .include("rocksdb/third-party/gtest-1.8.1/fused-src/")
            .include(".");

        cfg.define("NDEBUG", Some("1"));
        // True for C++ >= 17. We set `-std=c++20` below.
        cfg.define("HAVE_ALIGNED_NEW", None);
        // __uint128_t is supported by GCC and Clang; not MSVC.
        if !target.is_msvc() {
            cfg.define("HAVE_UINT128_EXTENSION", None);
        }

        if target.is_msvc() {
            if cfg!(feature = "mt_static") {
                cfg.static_crt(true);
            }
            cfg.flag("-EHsc");
            // MSVC uses `-std:c++20`, not `-std=c++20`.
            cfg.flag("-std:c++20");
        } else {
            cfg.flag(cxx_standard());
            // Mirrors RocksDB's CMakeLists.txt warning flags.
            for f in [
                "-Wsign-compare",
                "-Wshadow",
                "-Wno-unused-parameter",
                "-Wno-unused-variable",
                "-Woverloaded-virtual",
                "-Wnon-virtual-dtor",
                "-Wno-missing-field-initializers",
                "-Wno-strict-aliasing",
                "-Wno-invalid-offsetof",
            ] {
                cfg.flag(f);
            }
        }

        // LTO: gated on the `lto` feature. RocksDB's Makefile only
        // supports clang-LTO; bail if the user's CC isn't clang.
        if cfg!(feature = "lto") {
            cfg.flag("-flto");
            if !cfg.get_compiler().is_like_clang() {
                panic!(
                    "the `lto` feature requires Clang; set \
                     `CC=/usr/bin/clang CXX=/usr/bin/clang++` or disable the feature"
                );
            }
        }

        cfg
    }

    /// Compression-format defines + include path injection. Each external
    /// compression library is its own `-sys` crate that exports
    /// `DEP_<NAME>_INCLUDE` for us via cargo `links` metadata.
    fn apply_compression_features(cfg: &mut cc::Build) {
        // snappy is special — we vendor it in this crate.
        if cfg!(feature = "snappy") {
            cfg.define("SNAPPY", Some("1"));
            cfg.include("snappy/");
        }

        // (enabled, rocksdb_define, dep_env_var)
        let externals: &[(bool, &str, &str)] = &[
            (cfg!(feature = "lz4"), "LZ4", "DEP_LZ4_INCLUDE"),
            (cfg!(feature = "zstd"), "ZSTD", "DEP_ZSTD_INCLUDE"),
            (cfg!(feature = "zlib"), "ZLIB", "DEP_Z_INCLUDE"),
            (cfg!(feature = "bzip2"), "BZIP2", "DEP_BZIP2_INCLUDE"),
        ];
        for (enabled, define, dep_var) in externals {
            if !*enabled {
                continue;
            }
            cfg.define(define, Some("1"));
            if let Some(p) = env::var_os(dep_var) {
                cfg.include(p);
            }
        }

        if cfg!(feature = "zstd") && cfg!(feature = "zstd-static-linking-only") {
            cfg.define("ZSTD_STATIC_LINKING_ONLY", Some("1"));
        }
    }

    /// Other Cargo features that translate into preprocessor defines.
    fn apply_optional_features(cfg: &mut cc::Build, target: &Target) {
        if cfg!(feature = "rtti") {
            cfg.define("USE_RTTI", Some("1"));
        }
        if cfg!(feature = "malloc-usable-size") && target.os == "linux" {
            cfg.define("ROCKSDB_MALLOC_USABLE_SIZE", Some("1"));
        }
    }

    /// Pass `-Ctarget-cpu=...` through to the C/C++ compiler as `-march=` /
    /// `-mcpu=` so the C++ side benefits from the same CPU baseline.
    fn apply_target_cpu(cfg: &mut cc::Build, target: &Target) {
        let Some(cpu) = &target.rust_target_cpu else {
            return;
        };
        match target.arch.as_str() {
            "x86_64" | "x86" => {
                cfg.flag_if_supported(format!("-march={cpu}"));
            }
            "aarch64" | "arm" => {
                cfg.flag_if_supported(format!("-mcpu={cpu}"));
            }
            other => {
                println!(
                    "cargo::warning=unknown target architecture: \
                     {other}; C/C++ target-cpu flag not passed through"
                );
            }
        }
    }

    /// CPU-feature flags driven by `CARGO_CFG_TARGET_FEATURE`. RocksDB uses
    /// these to enable hardware-accelerated CRC32C and other intrinsics.
    fn apply_target_arch(cfg: &mut cc::Build, target: &Target) {
        match target.arch.as_str() {
            "x86_64" | "x86" => apply_x86(cfg, target),
            "aarch64" => apply_aarch64(cfg, target),
            _ => {}
        }
    }

    fn apply_x86(cfg: &mut cc::Build, target: &Target) {
        // SSE4.2 enables hardware CRC32C (Intel Nehalem+ / AMD Bulldozer+).
        if target.has_feature("sse2") {
            cfg.flag_if_supported("-msse2");
        }
        if target.has_feature("sse4.1") {
            cfg.flag_if_supported("-msse4.1");
        }
        if target.has_feature("sse4.2") {
            cfg.flag_if_supported("-msse4.2");
        } else {
            println!(
                r#"cargo::warning=compiling without SSE4.2: CRC will be slow (set RUSTFLAGS="-Ctarget-cpu=..." to optimize RocksDB e.g. -Ctarget-cpu=broadwell)"#
            );
        }
        if target.has_feature("avx2") {
            cfg.flag_if_supported("-mavx2");
        }
        if target.has_feature("bmi1") {
            cfg.flag_if_supported("-mbmi");
        }
        if target.has_feature("lzcnt") {
            cfg.flag_if_supported("-mlzcnt");
        }
        // Android targets don't define __PCLMUL__ even with the feature.
        if target.os != "android" && target.has_feature("pclmulqdq") {
            cfg.flag_if_supported("-mpclmul");
        }
        // RocksDB <= 10.11.0 assumes AVX implies PCLMUL, which isn't true
        // for x86-64-v3/-v4. Warn so the user knows the build may fail.
        if target.has_feature("avx") && !target.has_feature("pclmulqdq") {
            println!(
                r#"cargo::warning=RocksDB BUG: target arch missing -mpclmul; compile may fail: pass a named architecture e.g. -Ctarget-cpu=broadwell"#
            );
        }
    }

    fn apply_aarch64(cfg: &mut cc::Build, target: &Target) {
        if target.has_feature("crc") && target.has_feature("aes") {
            // If no -Ctarget-cpu was provided, set an explicit baseline
            // that includes the crypto extensions RocksDB checks for via
            // __ARM_FEATURE_CRYPTO. See facebook/rocksdb#14217.
            if target.rust_target_cpu.is_none() {
                cfg.flag_if_supported("-march=armv8-a+crc+aes+crypto");
            }
        } else {
            println!(
                r#"cargo::warning=building for aarch64 WITHOUT CRC instruction: build with RUSTFLAGS="-Ctarget-cpu=..." to optimize RocksDB e.g. -Ctarget-cpu=neoverse-n1"#
            );
        }
    }

    /// What source-file filter to apply when collecting RocksDB sources.
    /// Non-Windows targets use POSIX implementations; Windows swaps in its
    /// own `port/win/` set.
    #[derive(Copy, Clone)]
    enum SourceLayout {
        Posix,
        Windows,
    }

    /// All POSIX-ish targets share these two defines.
    fn define_posix(cfg: &mut cc::Build) {
        cfg.define("ROCKSDB_PLATFORM_POSIX", None);
        cfg.define("ROCKSDB_LIB_IO_POSIX", None);
    }

    /// Plain `OS_<NAME>` mapping for the BSDs and AIX, all of which need
    /// nothing beyond `define_posix()` + their own OS marker.
    const PLAIN_POSIX_OS: &[(&str, &str)] = &[
        ("freebsd", "OS_FREEBSD"),
        ("dragonfly", "OS_DRAGONFLYBSD"),
        ("netbsd", "OS_NETBSD"),
        ("openbsd", "OS_OPENBSD"),
        ("aix", "OS_AIX"),
    ];

    /// Apply target-OS specific defines. Returns the source layout to use.
    fn apply_target_os(cfg: &mut cc::Build, target: &Target) -> SourceLayout {
        if let Some((_, marker)) = PLAIN_POSIX_OS.iter().find(|(os, _)| *os == target.os) {
            cfg.define(marker, None);
            define_posix(cfg);
            return SourceLayout::Posix;
        }

        match target.os.as_str() {
            "ios" | "tvos" | "watchos" => {
                cfg.define("OS_MACOSX", None);
                cfg.define("IOS_CROSS_COMPILE", None);
                cfg.define("PLATFORM", "IOS");
                cfg.define("NIOSTATS_CONTEXT", None);
                cfg.define("NPERF_CONTEXT", None);
                define_posix(cfg);
                // Enable RocksDB's fcntl(F_FULLFSYNC) path for true on-
                // disk durability. F_FULLFSYNC is Apple-specific and is
                // available on all supported macOS / iOS / tvOS / watchOS
                // versions. RocksDB's CMakeLists.txt detects it via
                // check_cxx_symbol_exists; we hardcode it for Apple
                // targets since the constant is universally available.
                cfg.define("HAVE_FULLFSYNC", None);
                // Pin the iOS deployment target via compiler flag rather
                // than mutating process env (which Rust 2024 marks unsafe).
                // Only emit for actual iOS — tvos/watchos use their own
                // version-min flags and we don't pin those here, letting
                // cc-rs's SDK detection drive the default. Use `flag()`
                // (not `flag_if_supported`) so a non-Apple Clang fails
                // loudly rather than silently dropping the pin.
                if target.os == "ios" {
                    cfg.flag("-mios-version-min=12.0");
                }
                SourceLayout::Posix
            }
            "macos" => {
                cfg.define("OS_MACOSX", None);
                define_posix(cfg);
                // Enable true on-disk durability via fcntl(F_FULLFSYNC).
                // See the iOS arm above for rationale.
                cfg.define("HAVE_FULLFSYNC", None);
                SourceLayout::Posix
            }
            "android" => {
                cfg.define("OS_ANDROID", None);
                define_posix(cfg);
                if target.triple == "armv7-linux-androideabi" {
                    cfg.define("_FILE_OFFSET_BITS", Some("32"));
                }
                SourceLayout::Posix
            }
            "linux" => {
                cfg.define("OS_LINUX", None);
                define_posix(cfg);
                // getauxval has been in glibc since 2.16 (2012) and musl
                // since 1.1.0 (2014); both predate our MSRV ecosystem.
                for d in [
                    "ROCKSDB_SCHED_GETCPU_PRESENT",
                    "ROCKSDB_AUXV_GETAUXVAL_PRESENT",
                    "ROCKSDB_FALLOCATE_PRESENT",
                    "ROCKSDB_RANGESYNC_PRESENT",
                ] {
                    cfg.define(d, None);
                }
                // PTHREAD_MUTEX_ADAPTIVE_NP is a glibc extension. Without it,
                // rocksdb uses default pthread mutexes; with it, contended
                // mutexes spin briefly before falling back to a futex wait,
                // which is a small win for read-heavy workloads. Enable on
                // Linux glibc only - musl and bionic don't define the constant.
                if target.env_abi != "musl" {
                    cfg.define("ROCKSDB_PTHREAD_ADAPTIVE_MUTEX", None);
                }
                SourceLayout::Posix
            }
            "windows" => {
                // Mirrors RocksDB's CMakeLists.txt Windows branch:
                //   add_definitions(-DWIN32 -DOS_WIN -D_MBCS -DWIN64 -DNOMINMAX)
                for d in [
                    "WIN32",
                    "OS_WIN",
                    "_MBCS",
                    "WIN64",
                    "NOMINMAX",
                    "ROCKSDB_WINDOWS_UTF8_FILENAMES",
                ] {
                    cfg.define(d, None);
                }
                if target.triple == "x86_64-pc-windows-gnu" {
                    // MinGW needs localtime_r and Vista+ headers.
                    cfg.define("_POSIX_C_SOURCE", Some("1"));
                    cfg.define("_WIN32_WINNT", Some("_WIN32_WINNT_VISTA"));
                }
                SourceLayout::Windows
            }
            other => {
                println!(
                    "cargo::warning=unknown target OS `{other}`; \
                     building with default POSIX configuration"
                );
                define_posix(cfg);
                SourceLayout::Posix
            }
        }
    }

    /// Apply LFS (Large File Support) defines for 32-bit POSIX targets.
    /// Inside [`apply_target_os`] for android-armv7 we already set
    /// `_FILE_OFFSET_BITS=32`, so skip that case here.
    fn apply_lfs_defines(cfg: &mut cc::Build, target: &Target, layout: SourceLayout) {
        if matches!(layout, SourceLayout::Windows) {
            return;
        }
        if target.triple == "armv7-linux-androideabi" {
            return;
        }
        if target.pointer_width != 64 {
            cfg.define("_FILE_OFFSET_BITS", Some("64"));
            cfg.define("_LARGEFILE64_SOURCE", Some("1"));
        }
    }

    fn apply_jemalloc(cfg: &mut cc::Build, target: &Target) {
        if !cfg!(feature = "jemalloc") {
            return;
        }
        if NO_JEMALLOC_TARGETS
            .iter()
            .any(|t| target.triple.contains(t))
        {
            return;
        }
        cfg.define("ROCKSDB_JEMALLOC", Some("1"));
        cfg.define("JEMALLOC_NO_DEMANGLE", Some("1"));
        if let Some(root) = env::var_os("DEP_JEMALLOC_ROOT") {
            cfg.include(Path::new(&root).join("include"));
        }
    }

    fn apply_io_uring(_cfg: &mut cc::Build, _target: &Target) {
        #[cfg(feature = "io-uring")]
        {
            if _target.os == "linux" {
                pkg_config::probe_library("liburing").unwrap_or_else(|e| {
                    panic!(
                        "the `io-uring` feature was requested but pkg-config probe for \
                         `liburing` failed: {e}\n\
                         Hints:\n\
                          - Debian/Ubuntu:  apt-get install liburing-dev\n\
                          - Fedora/RHEL:    dnf install liburing-devel\n\
                          - Arch:           pacman -S liburing\n\
                          - Alpine:         apk add liburing-dev\n\
                          - or set PKG_CONFIG_PATH to a directory containing liburing.pc\n\
                          - when cross-compiling, also set PKG_CONFIG_ALLOW_CROSS=1\n\
                            and point PKG_CONFIG_PATH at the target sysroot's pkgconfig dir."
                    )
                });
                _cfg.define("ROCKSDB_IOURING_PRESENT", Some("1"));
            }
        }
    }

    /// Enable rocksdb's `execinfo.h`-based stack tracer (`port/stack_trace.cc`).
    /// Without `ROCKSDB_BACKTRACE` defined, that file compiles a no-op
    /// stub and rocksdb crashes (segfaults, aborts, assertion failures)
    /// print no C++ frames at all - a real DX regression vs. the Makefile
    /// build, which sets this define on every glibc-Linux and Apple target.
    ///
    /// We enable it on the same set: Linux glibc and Apple (macOS/iOS).
    /// Skip:
    ///   - musl Linux: doesn't ship libexecinfo by default; some distros
    ///     patch it in via the `libexecinfo` package but we can't assume.
    ///   - Android: bionic only added `<execinfo.h>` in API 33; older API
    ///     levels would fail to compile.
    ///   - Windows / MSVC: no equivalent; would need DbgHelp instead.
    ///   - BSDs: would need libexecinfo from ports.
    fn apply_backtrace(cfg: &mut cc::Build, target: &Target) {
        let supported = match target.os.as_str() {
            "linux" => target.env_abi != "musl",
            "macos" | "ios" => true,
            _ => false,
        };
        if supported {
            cfg.define("ROCKSDB_BACKTRACE", None);
        }
    }

    /// Read `rocksdb_lib_sources.txt`, drop the pre-generated
    /// `build_version.cc`, and apply the OS-specific source filter.
    fn collect_sources(target: &Target, layout: SourceLayout) -> Vec<&'static str> {
        let sources = include_str!("rocksdb_lib_sources.txt")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            // We ship our own build_version.cc; the upstream one needs git.
            .filter(|f| *f != "util/build_version.cc");

        let mut out: Vec<&'static str> = match layout {
            SourceLayout::Posix => sources.collect(),
            SourceLayout::Windows => sources
                .filter(|f| {
                    !matches!(
                        *f,
                        "port/port_posix.cc"
                            | "env/env_posix.cc"
                            | "env/fs_posix.cc"
                            | "env/io_posix.cc"
                    )
                })
                .collect(),
        };

        if matches!(layout, SourceLayout::Windows) {
            out.extend([
                "port/win/env_default.cc",
                "port/win/port_win.cc",
                "port/win/xpress_win.cc",
                "port/win/io_win.cc",
                "port/win/win_thread.cc",
                "port/win/env_win.cc",
                "port/win/win_logger.cc",
            ]);
            if cfg!(feature = "jemalloc")
                && !NO_JEMALLOC_TARGETS
                    .iter()
                    .any(|t| target.triple.contains(t))
            {
                out.push("port/win/win_jemalloc.cc");
            }
        }

        out
    }

    /// Returns the `-std=c++NN` flag to use. Honors `ROCKSDB_CXX_STD`.
    fn cxx_standard() -> String {
        env::var("ROCKSDB_CXX_STD").map_or_else(
            |_| format!("-std={DEFAULT_CXX_STD}"),
            |s| {
                if s.starts_with("-std=") {
                    s
                } else {
                    format!("-std={s}")
                }
            },
        )
    }
}

// =========================================================================
// System backend (env-vars and pkg-config)
// =========================================================================

mod system {
    use super::*;

    /// Build a `Backend::System` from `ROCKSDB_LIB_DIR`. Emits the
    /// corresponding `cargo::rustc-link-*` directives.
    pub(super) fn from_lib_dir_env() -> Backend {
        let lib_dir =
            env::var_os("ROCKSDB_LIB_DIR").expect("checked by caller in Backend::resolve");
        emit_link_directives(Path::new(&lib_dir));

        Backend::System {
            includes: env_includes_override(),
        }
    }

    /// FreeBSD default: link `/usr/local/lib/librocksdb.{so,a}` and read
    /// headers from `/usr/local/include`. Honors `ROCKSDB_INCLUDE_DIR`
    /// if set; `ROCKSDB_LIB_DIR` is handled in [`Backend::resolve`] and
    /// does not reach this function.
    pub(super) fn from_freebsd_defaults() -> Backend {
        emit_link_directives(Path::new("/usr/local/lib"));

        let mut includes = env_includes_override();
        if includes.is_empty() {
            includes.push(PathBuf::from("/usr/local/include"));
        }
        Backend::System { includes }
    }

    /// Use `pkg-config` to find rocksdb. Honors the standard `pkg-config`
    /// crate env vars (`PKG_CONFIG_ALL_STATIC`, `PKG_CONFIG_PATH`, ...)
    /// plus our `ROCKSDB_STATIC` for static-link opt-in.
    pub(super) fn probe_pkg_config() -> Backend {
        let mut config = pkg_config::Config::new();
        // Honor ROCKSDB_STATIC even when going through pkg-config.
        if env::var_os("ROCKSDB_STATIC").is_some() {
            config.statik(true);
        }
        // We intentionally don't pass `.atleast_version(...)` — power
        // users opting into this path know what version they have
        // installed and are responsible for ABI compatibility.
        let lib = config.probe("rocksdb").unwrap_or_else(|e| {
            panic!(
                "ROCKSDB_USE_PKG_CONFIG=1 but pkg-config probe for `rocksdb` failed: {e}\n\
                 Hints:\n\
                  - Debian/Ubuntu:  apt-get install librocksdb-dev\n\
                  - Fedora/RHEL:    dnf install rocksdb-devel\n\
                  - Arch:           pacman -S rocksdb\n\
                  - Alpine:         apk add rocksdb-dev\n\
                  - macOS:          brew install rocksdb\n\
                  - or set PKG_CONFIG_PATH to a directory containing rocksdb.pc\n\
                  - when cross-compiling, also set PKG_CONFIG_ALLOW_CROSS=1\n\
                    and point PKG_CONFIG_PATH at the target sysroot's pkgconfig dir."
            )
        });

        // pkg-config crate already emitted the link directives. We pass
        // ALL of its discovered include paths to bindgen so headers split
        // across multiple roots are visible.
        //
        // ROCKSDB_INCLUDE_DIR, when set, is merged in front of pkg-config's
        // paths: bindgen sees the env-override path first (so it wins on
        // duplicate header names), with pkg-config's additional paths
        // remaining visible for vendors that ship rocksdb with sibling
        // include dirs (e.g. for chained deps).
        let mut includes = env_includes_override();
        for p in lib.include_paths {
            if !includes.contains(&p) {
                includes.push(p);
            }
        }

        Backend::System { includes }
    }

    /// Common link-directive emission: search path + static/dylib choice.
    fn emit_link_directives(lib_dir: &Path) {
        println!("cargo::rustc-link-search=native={}", lib_dir.display());
        let kind = link_kind();
        println!("cargo::rustc-link-lib={kind}=rocksdb");
    }

    /// `ROCKSDB_STATIC` set to any non-empty value → static link;
    /// otherwise dylib.
    fn link_kind() -> &'static str {
        if env::var_os("ROCKSDB_STATIC").is_some() {
            "static"
        } else {
            "dylib"
        }
    }

    /// Read `ROCKSDB_INCLUDE_DIR` as a Vec (one element if set, empty
    /// otherwise). Uses `var_os` so non-UTF-8 paths work on Unix.
    fn env_includes_override() -> Vec<PathBuf> {
        env::var_os("ROCKSDB_INCLUDE_DIR")
            .map(|p| vec![PathBuf::from(p)])
            .unwrap_or_default()
    }
}

// =========================================================================
// Snappy
// =========================================================================

mod snappy {
    use super::*;

    /// Ensure libsnappy is available to the build. With a vendored
    /// rocksdb, that means either linking a prebuilt snappy (via
    /// `SNAPPY_LIB_DIR`) or compiling our bundled copy.
    ///
    /// With a system rocksdb, snappy is a no-op: the prebuilt librocksdb
    /// is typically already linked against its own libsnappy (or was
    /// built with snappy disabled). Adding another copy here risks
    /// duplicate-symbol link errors and silent version skew, so we skip
    /// entirely — and emit a `cargo::warning=` so users who explicitly
    /// enabled the `snappy` feature alongside a system rocksdb aren't
    /// silently surprised that the feature is ignored.
    pub(super) fn ensure(target: &Target, backend: &Backend) {
        if matches!(backend, Backend::System { .. }) {
            println!(
                "cargo::warning=`snappy` feature is enabled but rocksdb \
                 is being linked from the system; skipping vendored \
                 snappy build (the system library is expected to provide \
                 snappy support itself)."
            );
            return;
        }
        if try_system() {
            return;
        }
        println!("cargo::rerun-if-changed=snappy/");
        ensure_submodule_present("snappy");
        build_vendored(target);
    }

    /// Try to link a prebuilt snappy via `SNAPPY_LIB_DIR`. Returns true if
    /// linking succeeded and no vendored build is needed.
    fn try_system() -> bool {
        if env_truthy("SNAPPY_COMPILE") {
            return false;
        }
        let Some(lib_dir) = env::var_os("SNAPPY_LIB_DIR") else {
            return false;
        };
        println!(
            "cargo::rustc-link-search=native={}",
            lib_dir.to_string_lossy()
        );
        let kind = if env::var_os("SNAPPY_STATIC").is_some() {
            "static"
        } else {
            "dylib"
        };
        println!("cargo::rustc-link-lib={kind}=snappy");
        true
    }

    fn build_vendored(target: &Target) {
        let mut cfg = cc::Build::new();
        cfg.include("snappy/")
            .include(".")
            .define("NDEBUG", Some("1"))
            .extra_warnings(false);

        if target.is_msvc() {
            cfg.flag("-EHsc");
            if cfg!(feature = "mt_static") {
                cfg.static_crt(true);
            }
            cfg.flag("-std:c++20");
        } else {
            cfg.flag("-std=c++20");
        }

        if target.endian == "big" {
            cfg.define("SNAPPY_IS_BIG_ENDIAN", Some("1"));
        }

        for src in [
            "snappy/snappy.cc",
            "snappy/snappy-sinksource.cc",
            "snappy/snappy-c.cc",
        ] {
            cfg.file(src);
        }
        cfg.cpp(true);
        cfg.compile("libsnappy.a");
    }
}

// =========================================================================
// Bindgen
// =========================================================================

mod bindings {
    use super::*;

    /// Generate Rust FFI bindings against the backend's headers.
    /// Critically, this uses the **chosen backend's** include dirs so a
    /// system link cannot produce ABI-mismatched bindings.
    ///
    /// If we couldn't determine an include directory (only possible when
    /// the system path was chosen and neither `ROCKSDB_INCLUDE_DIR` nor
    /// pkg-config provided one), this panics with an actionable message
    /// rather than guessing `/usr/include` and silently misbinding.
    pub(super) fn generate(backend: &Backend) {
        let includes = backend.all_includes();
        // The first include path is only used as a sanity check (it has to
        // exist or the build will fail downstream); the actual primary
        // header is our local extensions header, which `#include`s
        // `rocksdb/c.h` and then declares the additional symbols.
        let _primary = includes.first().unwrap_or_else(|| {
            panic!(
                "could not determine the RocksDB include directory.\n\
                 Set ROCKSDB_INCLUDE_DIR to the directory containing \
                 `rocksdb/c.h`, OR ensure your pkg-config rocksdb.pc \
                 file has correct Cflags."
            )
        });

        // Bindgen reads our extensions header as the primary input. The
        // extensions header pulls in `rocksdb/c.h` via `#include "rocksdb/c.h"`,
        // so bindgen ends up scanning the full upstream C API surface plus
        // our local additions in one pass.
        let header = manifest_dir()
            .join("c-api-extensions")
            .join("c_api_extensions.h");

        let mut builder = bindgen::Builder::default()
            .header(header.display().to_string())
            .derive_debug(false)
            // https://github.com/rust-lang-nursery/rust-bindgen/issues/550
            .blocklist_type("max_align_t")
            .ctypes_prefix("libc")
            .size_t_is_usize(true);

        // Pass every backend include path so headers split across multiple
        // roots (rare but happens with chained pkg-config deps) are all
        // found.
        for inc in includes {
            builder = builder.clang_arg(format!("-I{}", inc.display()));
        }

        // Escape hatch for exotic system layouts. Whitespace-split — paths
        // with spaces aren't supported; users with such paths should
        // pre-process them upstream (e.g. via a shell function).
        if let Ok(extra) = env::var("BINDGEN_EXTRA_CLANG_ARGS") {
            for arg in extra.split_whitespace() {
                builder = builder.clang_arg(arg);
            }
        }

        let bindings = builder.generate().unwrap_or_else(|e| {
            panic!(
                "failed to generate RocksDB bindings from `{}`: {e}\n\
                 Tried include paths: {:?}\n\
                 Hints:\n\
                  - confirm `{}` exists and is readable\n\
                  - set BINDGEN_EXTRA_CLANG_ARGS=\"-I/extra/include\" if needed\n\
                  - on cross-compile, ensure clang can find your sysroot headers",
                header.display(),
                includes,
                header.display(),
            )
        });

        let out = out_dir().join("bindings.rs");
        bindings
            .write_to_file(&out)
            .unwrap_or_else(|e| panic!("failed to write bindings to `{}`: {e}", out.display()));
    }
}

// =========================================================================
// Coroutines (folly) — gated behind the `coroutines` feature
// =========================================================================

#[cfg(feature = "coroutines")]
mod coroutines {
    use super::*;

    /// Validates that the requested target is one we can build the
    /// `coroutines` feature for. Folly only ships a working build for
    /// Linux; macOS, Windows, and the BSDs are missing pieces (notably
    /// the io_uring async file system, and folly's getdeps build itself
    /// is flaky elsewhere).
    pub(super) fn validate_target(target: &Target) {
        if target.os != "linux" {
            panic!(
                "the `coroutines` feature is only supported on Linux \
                 (target was `{}`)",
                target.triple
            );
        }
    }

    /// Compile-time configuration for the coroutines build: defines,
    /// compiler flags, and include paths for folly + its dependencies.
    /// Mirrors RocksDB's `CMakeLists.txt` (USE_COROUTINES branch) and
    /// `folly.mk`.
    pub(super) fn apply_compile_config(cfg: &mut cc::Build) {
        cfg.define("USE_COROUTINES", None);
        cfg.define("USE_FOLLY", None);
        cfg.define("FOLLY_NO_CONFIG", None);
        cfg.define("HAVE_CXX11_ATOMIC", None);

        // GCC needs explicit -fcoroutines; clang enables coroutines under
        // -std=c++20 by default. Using `flag()` (not `flag_if_supported`)
        // forces an old GCC to fail loudly with "unrecognized option"
        // rather than silently produce confusing template errors deep in
        // folly headers.
        if !cfg.get_compiler().is_like_clang() {
            cfg.flag("-fcoroutines");
        }
        // Folly's headers trip warnings RocksDB's stricter flags would
        // otherwise treat as significant.
        for f in [
            "-Wno-deprecated",
            "-Wno-redundant-move",
            "-Wno-maybe-uninitialized",
            "-Wno-invalid-memory-model",
        ] {
            cfg.flag_if_supported(f);
        }

        let install_root = install_root();
        for dep in [
            "folly",
            "boost",
            "fmt",
            "glog",
            "gflags",
            "double-conversion",
            "libevent",
            "libsodium",
        ] {
            let dir = resolve_dep(&install_root, dep);
            cfg.include(dir.join("include"));
        }
    }

    /// When the user has opted into `coroutines` AND a system rocksdb,
    /// emit a `cargo::warning=` so a missing-symbol link error has a
    /// breadcrumb back to this combination. This crate cannot tell
    /// whether the prebuilt librocksdb was actually compiled with
    /// `USE_COROUTINES=1`; we still emit the folly link directives (the
    /// user accepted responsibility by opting into both), but surface
    /// the risk loudly.
    pub(super) fn warn_if_system_backend(backend: &Backend) {
        if matches!(backend, Backend::System { .. }) {
            println!(
                "cargo::warning=`coroutines` feature is enabled and \
                 RocksDB is being linked from the system: ensure that \
                 librocksdb was built with USE_COROUTINES=1 and \
                 USE_FOLLY=1, otherwise you will get unresolved-symbol \
                 link errors against folly's coroutine helpers."
            );
        }
    }

    /// Link-time configuration: emit the `cargo::rustc-link-*` directives
    /// for folly and its transitive deps. Order matters — folly must be
    /// listed *after* librocksdb on the link line so its coroutine symbols
    /// satisfy RocksDB's references.
    pub(super) fn link() {
        let install_root = install_root();

        let folly = resolve_dep(&install_root, "folly");
        let boost = resolve_dep(&install_root, "boost");
        let fmt = resolve_dep(&install_root, "fmt");
        let glog = resolve_dep(&install_root, "glog");
        let gflags = resolve_dep(&install_root, "gflags");
        let dbl_conv = resolve_dep(&install_root, "double-conversion");
        let libevent = resolve_dep(&install_root, "libevent");
        let libsodium = resolve_dep(&install_root, "libsodium");

        // folly itself
        println!(
            "cargo::rustc-link-search=native={}",
            folly.join("lib").display()
        );
        println!("cargo::rustc-link-lib=static=folly");

        // Boost components — list matches folly.mk's PLATFORM_LDFLAGS.
        // If FOLLY_COMMIT_HASH is bumped and a component vanishes, the
        // link will fail with "cannot find -lboost_<x>"; trim this list
        // then.
        println!(
            "cargo::rustc-link-search=native={}",
            boost.join("lib").display()
        );
        for c in [
            "context",
            "filesystem",
            "atomic",
            "program_options",
            "regex",
            "system",
            "thread",
        ] {
            println!("cargo::rustc-link-lib=static=boost_{c}");
        }

        println!(
            "cargo::rustc-link-search=native={}",
            dbl_conv.join("lib").display()
        );
        println!("cargo::rustc-link-lib=static=double-conversion");

        println!(
            "cargo::rustc-link-search=native={}",
            libevent.join("lib").display()
        );
        println!("cargo::rustc-link-lib=static=event");

        println!(
            "cargo::rustc-link-search=native={}",
            libsodium.join("lib").display()
        );
        println!("cargo::rustc-link-lib=static=sodium");

        // glog and gflags build as shared libs only. We export their dirs
        // as `cargo::metadata=folly_glog_libdir` etc. so downstream binary
        // crates can embed rpath; cargo:rustc-link-arg doesn't propagate
        // through transitive `-sys` crates (rust-lang/cargo#9554).
        let glog_libdir = libdir_containing(&glog, "glog");
        let gflags_libdir = libdir_containing(&gflags, "gflags");
        println!("cargo::rustc-link-search=native={}", glog_libdir.display());
        println!(
            "cargo::rustc-link-search=native={}",
            gflags_libdir.display()
        );
        println!("cargo::rustc-link-lib=dylib=glog");
        println!("cargo::rustc-link-lib=dylib=gflags");
        println!(
            "cargo::metadata=folly_glog_libdir={}",
            glog_libdir.display()
        );
        println!(
            "cargo::metadata=folly_gflags_libdir={}",
            gflags_libdir.display()
        );

        let fmt_libdir = libdir_containing(&fmt, "fmt");
        println!("cargo::rustc-link-search=native={}", fmt_libdir.display());
        println!("cargo::rustc-link-lib=static=fmt");

        // folly transitive dep
        println!("cargo::rustc-link-lib=dylib=dl");
    }

    /// `ROCKSDB_FOLLY_INSTALL_PATH` — must point at a folly install root
    /// produced by `scripts/build_folly.sh` (or equivalent).
    fn install_root() -> PathBuf {
        let raw = env::var("ROCKSDB_FOLLY_INSTALL_PATH").unwrap_or_else(|_| {
            panic!(
                "the `coroutines` feature requires the env var \
                 ROCKSDB_FOLLY_INSTALL_PATH to point at a folly install \
                 produced by `scripts/build_folly.sh` (or equivalent)."
            )
        });
        PathBuf::from(raw)
    }

    /// Resolve a dependency directory under folly's `installed/` tree.
    ///
    /// getdeps has two naming conventions:
    /// 1. The project being built (folly itself) installs to
    ///    `<root>/<name>` with no suffix.
    /// 2. Its dependencies install to `<root>/<name>-<hash>`.
    ///
    /// We check (1) first then fall back to globbing (2). Panicking on
    /// zero or multiple matches catches stale installs from prior
    /// FOLLY_COMMIT_HASH values that would otherwise resolve non-
    /// deterministically.
    fn resolve_dep(install_root: &Path, name: &str) -> PathBuf {
        let bare = install_root.join(name);
        if bare.is_dir() {
            return bare;
        }
        let pattern = install_root.join(format!("{name}-*"));
        let pattern_str = pattern
            .to_str()
            .expect("ROCKSDB_FOLLY_INSTALL_PATH must be valid UTF-8");
        let matches: Vec<PathBuf> = glob::glob(pattern_str)
            .unwrap_or_else(|e| panic!("invalid glob `{pattern_str}`: {e}"))
            .filter_map(Result::ok)
            .collect();
        match matches.as_slice() {
            [] => panic!(
                "could not find `{name}` or `{name}-*` under {}; \
                 did `scripts/build_folly.sh` finish successfully?",
                install_root.display()
            ),
            [only] => only.clone(),
            many => panic!(
                "found {} `{name}-*` directories under {}:\n  {}\n\
                 this usually means a stale install from a prior \
                 FOLLY_COMMIT_HASH is mixed with the current one. Remove \
                 the stale entries or point ROCKSDB_FOLLY_INSTALL_PATH at \
                 a clean install.",
                many.len(),
                install_root.display(),
                many.iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ),
        }
    }

    /// Returns whichever of `<prefix>/lib` or `<prefix>/lib64` actually
    /// contains the named library. getdeps sometimes leaves an empty
    /// sibling directory as a CMake `GNUInstallDirs` side effect, so a
    /// plain `is_dir()` check is not enough.
    fn libdir_containing(prefix: &Path, lib_name: &str) -> PathBuf {
        for sub in ["lib64", "lib"] {
            let candidate = prefix.join(sub);
            if !candidate.is_dir() {
                continue;
            }
            let pattern = candidate.join(format!("lib{lib_name}.*"));
            let Some(pattern_str) = pattern.to_str() else {
                continue;
            };
            let mut iter = match glob::glob(pattern_str) {
                Ok(it) => it,
                Err(_) => continue,
            };
            if iter.any(|r| r.is_ok()) {
                return candidate;
            }
        }
        panic!(
            "could not find `lib{lib_name}.{{so,a}}*` in either \
             `{}/lib/` or `{}/lib64/`. The folly install at `{}` looks \
             incomplete - rerun `scripts/build_folly.sh` to rebuild from \
             scratch.",
            prefix.display(),
            prefix.display(),
            prefix.display()
        );
    }
}

// =========================================================================
// Small utilities
// =========================================================================

fn rerun_if_env_changed(vars: &[&str]) {
    for v in vars {
        println!("cargo::rerun-if-env-changed={v}");
    }
}

fn out_dir() -> PathBuf {
    PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"))
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn vendored_include() -> PathBuf {
    manifest_dir().join("rocksdb").join("include")
}

/// Truthy interpretation of `1`, `true` (case-insensitive). Any other
/// value (including unset) is falsy.
fn env_truthy(name: &str) -> bool {
    match env::var(name) {
        Ok(v) => {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true")
        }
        Err(_) => false,
    }
}

// =========================================================================
// Local C-API extensions
// =========================================================================

/// Local additions to the RocksDB C API for C++ options that have no
/// upstream C wrapper yet. The actual sources live in
/// `librocksdb-sys/c-api-extensions/`:
///
/// - `c_api_extensions.h` declares the new C symbols and `#include`s
///   `rocksdb/c.h`, making it a clean superset of the upstream C API
///   header that bindgen scans as its primary input.
/// - `c_api_extensions.cc` defines the new symbols by reaching into the
///   relevant C++ types (`ReadOptions`, `Options`, `BlockBasedTableOptions`).
///
/// For the Vendored backend, `vendor::build()` already adds the extension
/// `.cc` to its `cc::Build` source list — there's nothing extra to do.
/// This module exists for the System backend, where the user supplies a
/// pre-built `librocksdb` that doesn't contain our extensions: we compile
/// the extension as a tiny standalone static archive and link it
/// alongside the system library.
mod extensions {
    use super::*;

    /// Compile `c_api_extensions.cc` as a small standalone static
    /// archive and emit the cargo link directives to pull it into the
    /// final binary. Used only on the System backend; the Vendored
    /// backend folds the same source into `librocksdb.a` directly.
    pub(super) fn build_for_system_backend(target: &Target, backend: &Backend) {
        let mut cfg = cc::Build::new();

        // The extension references types from `rocksdb/options.h` and
        // `rocksdb/table.h`, plus the opaque-handle types from
        // `rocksdb/c.h`. Pass the system rocksdb's include dirs so those
        // resolve against the version the user has installed.
        cfg.include("c-api-extensions/");
        for inc in backend.all_includes() {
            cfg.include(inc);
        }

        cfg.file("c-api-extensions/c_api_extensions.cc");
        cfg.cpp(true);

        // Match the vendored build's C++ standard so the extension's
        // headers compile the same way against the user's
        // `rocksdb/options.h`.
        let cxx_std = env::var("ROCKSDB_CXX_STD").unwrap_or_else(|_| DEFAULT_CXX_STD.to_string());
        if target.is_msvc() {
            cfg.flag(format!("/std:{cxx_std}"));
        } else {
            cfg.flag(format!("-std={cxx_std}"));
            // See `vendor::build`'s identical force-include of <cstdint>:
            // some translation units need it on stricter GCC versions.
            cfg.flag("-include").flag("cstdint");
        }

        cfg.compile("rust_rocksdb_c_api_extensions");
    }
}

/// Exit with a helpful error if a vendored submodule looks unpopulated.
/// On crates.io, the source tarball ships pre-populated, so this is only
/// hit by git-clone consumers who forgot `git submodule update --init`.
fn ensure_submodule_present(name: &str) {
    let dir = manifest_dir().join(name);
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| {
        panic!(
            "cannot read `{}`: {e}\n\
             If you cloned this repo with git, run:\n  \
             git submodule update --init --recursive",
            dir.display()
        )
    });
    if entries.count() == 0 {
        panic!(
            "the `{}` directory is empty. If you cloned this repo \
             with git, run:\n  git submodule update --init --recursive",
            dir.display()
        );
    }
}

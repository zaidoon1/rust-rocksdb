//! Build script for librocksdb-sys
//!
//! This script handles:
//! - Bindgen generation for RocksDB C API
//! - Building RocksDB from source (if not using system library)
//! - Building Snappy compression (if feature enabled)
//! - Platform-specific configuration

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(target_os = "linux")]
use libc::{AT_HWCAP, getauxval};

// =============================================================================
// Constants
// =============================================================================

/// Platforms where jemalloc-sys uses a prefixed jemalloc that cannot be linked
/// with RocksDB.
/// See: https://github.com/tikv/jemallocator/blob/f7adfca5aff272b43fd3ad896252b57fbbd9c72a/jemalloc-sys/src/env.rs#L24
const NO_JEMALLOC_TARGETS: &[&str] = &["android", "dragonfly", "darwin"];

/// POSIX-specific source files to exclude on Windows
const POSIX_SOURCES: &[&str] = &[
    "port/port_posix.cc",
    "env/env_posix.cc",
    "env/fs_posix.cc",
    "env/io_posix.cc",
];

/// Windows-specific source files
const WINDOWS_SOURCES: &[&str] = &[
    "port/win/env_default.cc",
    "port/win/port_win.cc",
    "port/win/xpress_win.cc",
    "port/win/io_win.cc",
    "port/win/win_thread.cc",
    "port/win/env_win.cc",
    "port/win/win_logger.cc",
];

// =============================================================================
// Platform Detection
// =============================================================================

/// Represents the target operating system/platform
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    AppleIos,
    Darwin,
    Android,
    Aix,
    Linux,
    DragonflyBsd,
    FreeBsd,
    NetBsd,
    OpenBsd,
    Windows,
    Unknown,
}

impl Platform {
    /// Detect the platform from the target triple
    fn detect(target: &str) -> Self {
        if target.contains("apple-ios") {
            Self::AppleIos
        } else if target.contains("darwin") {
            Self::Darwin
        } else if target.contains("android") {
            Self::Android
        } else if target.contains("aix") {
            Self::Aix
        } else if target.contains("linux") {
            Self::Linux
        } else if target.contains("dragonfly") {
            Self::DragonflyBsd
        } else if target.contains("freebsd") {
            Self::FreeBsd
        } else if target.contains("netbsd") {
            Self::NetBsd
        } else if target.contains("openbsd") {
            Self::OpenBsd
        } else if target.contains("windows") {
            Self::Windows
        } else {
            Self::Unknown
        }
    }

    /// Get the OS define macro for this platform
    const fn os_define(&self) -> Option<&'static str> {
        match self {
            Self::AppleIos | Self::Darwin => Some("OS_MACOSX"),
            Self::Android => Some("OS_ANDROID"),
            Self::Aix => Some("OS_AIX"),
            Self::Linux => Some("OS_LINUX"),
            Self::DragonflyBsd => Some("OS_DRAGONFLYBSD"),
            Self::FreeBsd => Some("OS_FREEBSD"),
            Self::NetBsd => Some("OS_NETBSD"),
            Self::OpenBsd => Some("OS_OPENBSD"),
            Self::Windows => Some("OS_WIN"),
            Self::Unknown => None,
        }
    }
}

/// Target information extracted from environment variables
struct TargetInfo {
    triple: String,
    platform: Platform,
    is_msvc: bool,
    is_x86_64: bool,
    is_riscv64gc: bool,
    pointer_width: String,
    endianness: String,
    target_features: Vec<String>,
}

impl TargetInfo {
    fn from_env() -> Self {
        let triple = env::var("TARGET").expect("TARGET environment variable not set");
        let platform = Platform::detect(&triple);

        let target_features = env::var("CARGO_CFG_TARGET_FEATURE")
            .map(|s| s.split(',').map(String::from).collect())
            .unwrap_or_default();

        Self {
            platform,
            is_msvc: triple.contains("msvc"),
            is_x86_64: triple.contains("x86_64"),
            is_riscv64gc: triple.contains("riscv64gc"),
            pointer_width: env::var("CARGO_CFG_TARGET_POINTER_WIDTH").unwrap_or_default(),
            endianness: env::var("CARGO_CFG_TARGET_ENDIAN").unwrap_or_else(|_| "little".into()),
            target_features,
            triple,
        }
    }

    fn has_feature(&self, feature: &str) -> bool {
        self.target_features.iter().any(|f| f == feature)
    }

    fn is_armv7_android(&self) -> bool {
        self.triple == "armv7-linux-androideabi"
    }

    fn is_x86_64_windows_gnu(&self) -> bool {
        self.triple == "x86_64-pc-windows-gnu"
    }
}

// =============================================================================
// Environment Helpers
// =============================================================================

/// Get the RocksDB include directory from env or use default
fn rocksdb_include_dir() -> String {
    env::var("ROCKSDB_INCLUDE_DIR").unwrap_or_else(|_| "rocksdb/include".to_string())
}

/// Get the C++ standard flag from env or use default
fn cxx_standard() -> String {
    env::var("ROCKSDB_CXX_STD").map_or("-std=c++20".to_owned(), |cxx_std| {
        if cxx_std.starts_with("-std=") {
            cxx_std
        } else {
            format!("-std={cxx_std}")
        }
    })
}

// =============================================================================
// Auxiliary Functions
// =============================================================================

#[cfg(target_os = "linux")]
fn is_getauxval_supported() -> bool {
    // SAFETY: getauxval is a safe libc function that reads process auxiliary vector
    unsafe { getauxval(AT_HWCAP) != 0 }
}

#[cfg(not(target_os = "linux"))]
fn is_getauxval_supported() -> bool {
    false
}

// =============================================================================
// Library Linking
// =============================================================================

/// Link a library for Windows targets
fn link_windows_lib(name: &str, bundled: bool) {
    let target = env::var("TARGET").unwrap();
    let parts: Vec<_> = target.split('-').collect();

    if parts.get(2) == Some(&"windows") {
        println!("cargo:rustc-link-lib=dylib={name}");
        if bundled && parts.get(3) == Some(&"gnu") {
            let dir = env::var("CARGO_MANIFEST_DIR").unwrap();
            println!("cargo:rustc-link-search=native={}/{}", dir, parts[0]);
        }
    }
}

/// Link the C++ standard library based on target
fn link_cpp_stdlib(target: &str) {
    // Check for explicit stdlib override
    if let Ok(stdlib) = env::var("CXXSTDLIB") {
        println!("cargo:rustc-link-lib=dylib={stdlib}");
        return;
    }

    // Platform-specific defaults
    // Reference: https://github.com/alexcrichton/cc-rs/blob/master/src/lib.rs#L2189
    if target.contains("apple") || target.contains("freebsd") || target.contains("openbsd") {
        println!("cargo:rustc-link-lib=dylib=c++");
    } else if target.contains("linux") {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    } else if target.contains("aix") {
        println!("cargo:rustc-link-lib=dylib=c++");
        println!("cargo:rustc-link-lib=dylib=c++abi");
    }
}

/// Try to find and link an external library
///
/// Returns `true` if the library was found and linked, `false` otherwise.
fn try_link_external_lib(lib_name: &str) -> bool {
    // Check if we should force compilation
    println!("cargo:rerun-if-env-changed={lib_name}_COMPILE");
    if let Ok(v) = env::var(format!("{lib_name}_COMPILE")) {
        if v.eq_ignore_ascii_case("true") || v == "1" {
            return false;
        }
    }

    println!("cargo:rerun-if-env-changed={lib_name}_LIB_DIR");
    println!("cargo:rerun-if-env-changed={lib_name}_STATIC");

    // Check if a library directory is specified
    if let Ok(lib_dir) = env::var(format!("{lib_name}_LIB_DIR")) {
        println!("cargo:rustc-link-search=native={lib_dir}");
        let mode = if env::var_os(format!("{lib_name}_STATIC")).is_some() {
            "static"
        } else {
            "dylib"
        };
        println!("cargo:rustc-link-lib={}={}", mode, lib_name.to_lowercase());
        return true;
    }

    false
}

// =============================================================================
// Submodule Management
// =============================================================================

/// Update git submodules
fn update_submodules() {
    let args = ["submodule", "update", "--init"];

    println!("Running command: \"git {}\" in dir: ../", args.join(" "));

    let result = Command::new("git").current_dir("../").args(args).status();

    match result {
        Ok(status) if status.success() => {}
        Ok(status) => {
            let code = status
                .code()
                .map_or("killed".to_string(), |c| c.to_string());
            panic!("Command failed with error code {code}");
        }
        Err(e) => panic!("Command failed with error: {e}"),
    }
}

/// Ensure a directory exists and is not empty
fn ensure_directory_not_empty(name: &str) {
    let count = fs::read_dir(name)
        .unwrap_or_else(|e| panic!("Cannot read directory '{name}': {e}"))
        .count();

    if count == 0 {
        eprintln!("The `{name}` directory is empty, did you forget to pull the submodules?");
        eprintln!("Try `git submodule update --init --recursive`");
        panic!("Empty submodule directory: {name}");
    }
}

// =============================================================================
// Bindgen
// =============================================================================

/// Generate Rust bindings for RocksDB C API
fn generate_bindings() {
    let bindings = bindgen::Builder::default()
        .header(format!("{}/rocksdb/c.h", rocksdb_include_dir()))
        .derive_debug(false)
        // https://github.com/rust-lang-nursery/rust-bindgen/issues/550
        .blocklist_type("max_align_t")
        .ctypes_prefix("libc")
        .size_t_is_usize(true)
        .generate()
        .expect("Unable to generate RocksDB bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Unable to write RocksDB bindings");
}

// =============================================================================
// Compiler Configuration
// =============================================================================

/// Configuration for building C++ code
struct CppBuildConfig {
    build: cc::Build,
}

impl CppBuildConfig {
    fn new() -> Self {
        Self {
            build: cc::Build::new(),
        }
    }

    /// Add an include path
    fn include<P: AsRef<Path>>(&mut self, path: P) -> &mut Self {
        self.build.include(path);
        self
    }

    /// Add multiple include paths
    fn includes<I, P>(&mut self, paths: I) -> &mut Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        for path in paths {
            self.build.include(path);
        }
        self
    }

    /// Define a preprocessor macro with no value
    fn define_flag(&mut self, name: &str) -> &mut Self {
        self.build.define(name, None);
        self
    }

    /// Define a preprocessor macro with a value
    fn define(&mut self, name: &str, value: &str) -> &mut Self {
        self.build.define(name, Some(value));
        self
    }

    /// Add a compiler flag
    fn flag(&mut self, flag: &str) -> &mut Self {
        self.build.flag(flag);
        self
    }

    /// Add a compiler flag if supported
    fn flag_if_supported(&mut self, flag: &str) -> &mut Self {
        self.build.flag_if_supported(flag);
        self
    }

    /// Add a source file
    fn file<P: AsRef<Path>>(&mut self, path: P) -> &mut Self {
        self.build.file(path);
        self
    }

    /// Add multiple source files
    fn files<I, P>(&mut self, paths: I) -> &mut Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        for path in paths {
            self.build.file(path);
        }
        self
    }

    /// Configure for C++ compilation
    fn cpp(&mut self, enabled: bool) -> &mut Self {
        self.build.cpp(enabled);
        self
    }

    /// Disable extra warnings
    fn extra_warnings(&mut self, enabled: bool) -> &mut Self {
        self.build.extra_warnings(enabled);
        self
    }

    /// Configure static CRT for MSVC
    fn static_crt(&mut self, enabled: bool) -> &mut Self {
        self.build.static_crt(enabled);
        self
    }

    /// Configure MSVC-specific settings
    fn configure_msvc(&mut self) -> &mut Self {
        if cfg!(feature = "mt_static") {
            self.static_crt(true);
        }
        self.flag("-EHsc").flag("-std:c++20")
    }

    /// Configure GCC/Clang-specific settings
    fn configure_gcc_clang(&mut self) -> &mut Self {
        self.flag(&cxx_standard())
            // Matches the flags in CMakeLists.txt from rocksdb
            .flag("-Wsign-compare")
            .flag("-Wshadow")
            .flag("-Wno-unused-parameter")
            .flag("-Wno-unused-variable")
            .flag("-Woverloaded-virtual")
            .flag("-Wnon-virtual-dtor")
            .flag("-Wno-missing-field-initializers")
            .flag("-Wno-strict-aliasing")
            .flag("-Wno-invalid-offsetof")
    }

    /// Compile the sources into a static library
    fn compile(&mut self, lib_name: &str) {
        self.build.compile(lib_name);
    }

    /// Check if the compiler is clang-like
    fn is_clang(&self) -> bool {
        self.build.get_compiler().is_like_clang()
    }
}

// =============================================================================
// Feature Configuration
// =============================================================================

/// Compression feature configuration
struct CompressionFeature {
    name: &'static str,
    define: &'static str,
    include_env_var: Option<&'static str>,
}

impl CompressionFeature {
    const fn new(
        name: &'static str,
        define: &'static str,
        include_env_var: Option<&'static str>,
    ) -> Self {
        Self {
            name,
            define,
            include_env_var,
        }
    }

    fn is_enabled(&self) -> bool {
        // Check cargo features at compile time using cfg!
        match self.name {
            "snappy" => cfg!(feature = "snappy"),
            "lz4" => cfg!(feature = "lz4"),
            "zstd" => cfg!(feature = "zstd"),
            "zlib" => cfg!(feature = "zlib"),
            "bzip2" => cfg!(feature = "bzip2"),
            _ => false,
        }
    }

    fn configure(&self, config: &mut CppBuildConfig) {
        if !self.is_enabled() {
            return;
        }

        config.define(self.define, "1");

        if let Some(env_var) = self.include_env_var {
            if let Some(path) = env::var_os(env_var) {
                config.include(path);
            }
        }

        // Special case for snappy - needs local include
        if self.name == "snappy" {
            config.include("snappy/");
        }
    }
}

/// All compression features
const COMPRESSION_FEATURES: &[CompressionFeature] = &[
    CompressionFeature::new("snappy", "SNAPPY", None),
    CompressionFeature::new("lz4", "LZ4", Some("DEP_LZ4_INCLUDE")),
    CompressionFeature::new("zstd", "ZSTD", Some("DEP_ZSTD_INCLUDE")),
    CompressionFeature::new("zlib", "ZLIB", Some("DEP_Z_INCLUDE")),
    CompressionFeature::new("bzip2", "BZIP2", Some("DEP_BZIP2_INCLUDE")),
];

/// Configure compression features for RocksDB build
fn configure_compression_features(config: &mut CppBuildConfig) {
    for feature in COMPRESSION_FEATURES {
        feature.configure(config);
    }

    // Special handling for zstd static linking
    if cfg!(feature = "zstd") && cfg!(feature = "zstd-static-linking-only") {
        config.define("ZSTD_STATIC_LINKING_ONLY", "1");
    }
}

// =============================================================================
// x86_64 SIMD Configuration
// =============================================================================

/// x86_64 target feature to compiler flag mapping
const X86_64_FEATURE_FLAGS: &[(&str, &str)] = &[
    ("sse2", "-msse2"),
    ("sse4.1", "-msse4.1"),
    ("sse4.2", "-msse4.2"),
    ("avx2", "-mavx2"),
    ("bmi1", "-mbmi"),
    ("lzcnt", "-mlzcnt"),
];

/// Configure x86_64 SIMD instructions
fn configure_x86_64_simd(config: &mut CppBuildConfig, target: &TargetInfo) {
    if !target.is_x86_64 {
        return;
    }

    for (feature, flag) in X86_64_FEATURE_FLAGS {
        if target.has_feature(feature) {
            config.flag_if_supported(flag);
        }
    }

    // pclmulqdq requires special handling - not supported on Android
    if target.platform != Platform::Android && target.has_feature("pclmulqdq") {
        config.flag_if_supported("-mpclmul");
    }
}

// =============================================================================
// Platform Configuration
// =============================================================================

/// Configure platform-specific defines for POSIX systems
fn configure_posix_platform(config: &mut CppBuildConfig) {
    config
        .define_flag("ROCKSDB_PLATFORM_POSIX")
        .define_flag("ROCKSDB_LIB_IO_POSIX");
}

/// Configure iOS-specific settings
fn configure_ios(config: &mut CppBuildConfig) {
    config
        .define_flag("OS_MACOSX")
        .define_flag("IOS_CROSS_COMPILE")
        .define("PLATFORM", "IOS")
        .define_flag("NIOSTATS_CONTEXT")
        .define_flag("NPERF_CONTEXT");
    configure_posix_platform(config);

    // SAFETY: Build scripts are single-threaded and run before any other code
    unsafe { env::set_var("IPHONEOS_DEPLOYMENT_TARGET", "12.0") };
}

/// Configure Android-specific settings
fn configure_android(config: &mut CppBuildConfig, target: &TargetInfo) {
    config.define_flag("OS_ANDROID");
    configure_posix_platform(config);

    if target.is_armv7_android() {
        config.define("_FILE_OFFSET_BITS", "32");
    }
}

/// Configure Linux-specific settings
fn configure_linux(config: &mut CppBuildConfig) {
    config
        .define_flag("OS_LINUX")
        .define_flag("ROCKSDB_SCHED_GETCPU_PRESENT");
    configure_posix_platform(config);

    if is_getauxval_supported() {
        config.define_flag("ROCKSDB_AUXV_GETAUXVAL_PRESENT");
    }
}

/// Configure Windows-specific settings
fn configure_windows(config: &mut CppBuildConfig, target: &TargetInfo) {
    link_windows_lib("rpcrt4", false);
    link_windows_lib("shlwapi", false);

    config
        .define_flag("DWIN32")
        .define_flag("OS_WIN")
        .define_flag("_MBCS")
        .define_flag("WIN64")
        .define_flag("NOMINMAX")
        .define_flag("ROCKSDB_WINDOWS_UTF8_FILENAMES");

    if target.is_x86_64_windows_gnu() {
        // Tell MinGW to create localtime_r wrapper of localtime_s function
        config.define("_POSIX_C_SOURCE", "1");
        // Tell MinGW to use at least Windows Vista headers (minimum supported version)
        config.define("_WIN32_WINNT", "_WIN32_WINNT_VISTA");
    }
}

/// Configure platform-specific settings based on target
fn configure_platform(config: &mut CppBuildConfig, target: &TargetInfo) {
    // Set OS-specific define
    if let Some(os_define) = target.platform.os_define() {
        config.define_flag(os_define);
    }

    // Platform-specific configuration
    match target.platform {
        Platform::AppleIos => configure_ios(config),
        Platform::Darwin => configure_posix_platform(config),
        Platform::Android => configure_android(config, target),
        Platform::Aix => configure_posix_platform(config),
        Platform::Linux => configure_linux(config),
        Platform::DragonflyBsd | Platform::FreeBsd | Platform::NetBsd | Platform::OpenBsd => {
            configure_posix_platform(config)
        }
        Platform::Windows => configure_windows(config, target),
        Platform::Unknown => {}
    }
}

// =============================================================================
// Source File Management
// =============================================================================

/// Get RocksDB library sources, filtered for the target platform
fn get_rocksdb_sources(platform: Platform) -> Vec<String> {
    let sources: Vec<&str> = include_str!("rocksdb_lib_sources.txt")
        .trim()
        .lines()
        .map(str::trim)
        // We have a pre-generated version of build_version.cc in the local directory
        .filter(|file| *file != "util/build_version.cc")
        .collect();

    let mut result: Vec<String> = if platform == Platform::Windows {
        // Filter out POSIX-specific sources on Windows
        sources
            .iter()
            .filter(|file| !POSIX_SOURCES.contains(file))
            .map(|file| format!("rocksdb/{file}"))
            .collect()
    } else {
        sources
            .iter()
            .map(|file| format!("rocksdb/{file}"))
            .collect()
    };

    // Add Windows-specific sources
    if platform == Platform::Windows {
        result.extend(WINDOWS_SOURCES.iter().map(|f| format!("rocksdb/{f}")));

        if cfg!(feature = "jemalloc") {
            result.push("rocksdb/port/win/win_jemalloc.cc".to_string());
        }
    }

    result
}

// =============================================================================
// Jemalloc Configuration
// =============================================================================

/// Configure jemalloc if enabled and supported
fn configure_jemalloc(config: &mut CppBuildConfig, target: &TargetInfo) {
    if !cfg!(feature = "jemalloc") {
        return;
    }

    // Check if jemalloc is supported on this target
    let is_supported = !NO_JEMALLOC_TARGETS
        .iter()
        .any(|t| target.triple.contains(t));
    if !is_supported {
        return;
    }

    config
        .define("ROCKSDB_JEMALLOC", "1")
        .define("JEMALLOC_NO_DEMANGLE", "1");

    if let Some(jemalloc_root) = env::var_os("DEP_JEMALLOC_ROOT") {
        config.include(Path::new(&jemalloc_root).join("include"));
    }
}

// =============================================================================
// io-uring Configuration
// =============================================================================

/// Configure io-uring if enabled (Linux only)
#[cfg(feature = "io-uring")]
fn configure_io_uring(config: &mut CppBuildConfig, target: &TargetInfo) {
    if target.platform != Platform::Linux {
        return;
    }

    pkg_config::probe_library("liburing")
        .expect("The io-uring feature was requested but the library is not available");
    config.define("ROCKSDB_IOURING_PRESENT", "1");
}

#[cfg(not(feature = "io-uring"))]
fn configure_io_uring(_config: &mut CppBuildConfig, _target: &TargetInfo) {}

// =============================================================================
// Build Functions
// =============================================================================

/// Build RocksDB from source
fn build_rocksdb(target: &TargetInfo) {
    let mut config = CppBuildConfig::new();

    // Basic includes
    config.includes([
        "rocksdb/include/",
        "rocksdb/",
        "rocksdb/third-party/gtest-1.8.1/fused-src/",
        ".",
    ]);

    // Compression features
    configure_compression_features(&mut config);

    // RTTI support
    if cfg!(feature = "rtti") {
        config.define("USE_RTTI", "1");
    }

    // malloc-usable-size support (Linux only)
    #[cfg(feature = "malloc-usable-size")]
    if target.platform == Platform::Linux {
        config.define("ROCKSDB_MALLOC_USABLE_SIZE", "1");
    }

    // LTO support (Clang only)
    if cfg!(feature = "lto") {
        if !config.is_clang() {
            panic!(
                "LTO is only supported with clang. Either disable the `lto` feature \
                 or set `CC=/usr/bin/clang CXX=/usr/bin/clang++` environment variables."
            );
        }
        config.flag("-flto");
    }

    // Debug disabled
    config.define("NDEBUG", "1");

    // x86_64 SIMD configuration
    configure_x86_64_simd(&mut config, target);

    // Platform-specific configuration
    configure_platform(&mut config, target);

    // Thread local storage support
    config.define_flag("ROCKSDB_SUPPORT_THREAD_LOCAL");

    // Jemalloc configuration
    configure_jemalloc(&mut config, target);

    // io-uring configuration
    configure_io_uring(&mut config, target);

    // Large file support for 32-bit systems (except armv7 Android)
    if !target.is_armv7_android() && target.pointer_width != "64" {
        config
            .define("_FILE_OFFSET_BITS", "64")
            .define("_LARGEFILE64_SOURCE", "1");
    }

    // Compiler-specific flags
    if target.is_msvc {
        config.configure_msvc();
    } else {
        config.configure_gcc_clang();
    }

    // RISC-V needs libatomic
    if target.is_riscv64gc {
        println!("cargo:rustc-link-lib=atomic");
    }

    // Add source files
    let sources = get_rocksdb_sources(target.platform);
    config.files(sources);
    config.file("build_version.cc");

    // C++ compilation settings
    config.cpp(true);
    config.flag_if_supported("-std=c++20");

    // Include cstdint on non-Windows platforms
    if target.platform != Platform::Windows {
        config.flag("-include").flag("cstdint");
    }

    config.compile("librocksdb.a");
}

/// Build Snappy compression library from source
fn build_snappy(target: &TargetInfo) {
    let mut config = CppBuildConfig::new();

    config
        .includes(["snappy/", "."])
        .define("NDEBUG", "1")
        .extra_warnings(false);

    if target.is_msvc {
        config.configure_msvc();
    } else {
        config.flag("-std=c++20");
    }

    // Big endian support
    if target.endianness == "big" {
        config.define("SNAPPY_IS_BIG_ENDIAN", "1");
    }

    config
        .file("snappy/snappy.cc")
        .file("snappy/snappy-sinksource.cc")
        .file("snappy/snappy-c.cc")
        .cpp(true)
        .compile("libsnappy.a");
}

// =============================================================================
// FreeBSD Special Handling
// =============================================================================

/// Handle FreeBSD which uses prebuilt system RocksDB
fn link_freebsd_rocksdb() {
    println!("cargo:rustc-link-search=native=/usr/local/lib");

    let mode = if env::var_os("ROCKSDB_STATIC").is_some() {
        "static"
    } else {
        "dylib"
    };

    println!("cargo:rustc-link-lib={mode}=rocksdb");
}

// =============================================================================
// Main Entry Point
// =============================================================================

fn main() {
    // Ensure submodules are initialized
    if !Path::new("rocksdb/AUTHORS").exists() {
        update_submodules();
    }

    // Generate bindings
    generate_bindings();

    let target = TargetInfo::from_env();

    // Build or link RocksDB
    if !try_link_external_lib("ROCKSDB") {
        // FreeBSD uses prebuilt system library
        if target.platform == Platform::FreeBsd {
            link_freebsd_rocksdb();
        } else {
            println!("cargo:rerun-if-changed=rocksdb/");
            ensure_directory_not_empty("rocksdb");
            build_rocksdb(&target);
        }
    } else {
        link_cpp_stdlib(&target.triple);
    }

    // Build or link Snappy (if feature enabled)
    if cfg!(feature = "snappy") && !try_link_external_lib("SNAPPY") {
        println!("cargo:rerun-if-changed=snappy/");
        ensure_directory_not_empty("snappy");
        build_snappy(&target);
    }

    // Export paths for dependent crates
    println!(
        "cargo:cargo_manifest_dir={}",
        env::var("CARGO_MANIFEST_DIR").unwrap()
    );
    println!("cargo:out_dir={}", env::var("OUT_DIR").unwrap());
}

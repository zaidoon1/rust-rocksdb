use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "linux")]
use libc::{getauxval, AT_HWCAP};

// ================================================================================================
// Configuration Constants
// ================================================================================================

// Platforms where jemalloc-sys uses a prefixed jemalloc that conflicts with RocksDB
const NO_JEMALLOC_TARGETS: &[&str] = &["android", "dragonfly", "darwin"];

// ================================================================================================
// Main Entry Point
// ================================================================================================

fn main() {
    // Determine linking strategy
    let use_system = should_use_system_rocksdb();
    let target = env::var("TARGET").unwrap();

    // Generate bindings
    let include_dir = if use_system {
        find_system_include_dir()
    } else {
        ensure_submodules_initialized();
        "rocksdb/include".to_string()
    };
    generate_bindings(&include_dir);

    // Build or link RocksDB
    if use_system {
        link_system_rocksdb(&target);
    } else {
        build_vendored_rocksdb(&target);
    }

    // Handle Snappy
    if cfg!(feature = "snappy") && !try_link_system_lib("SNAPPY") {
        build_vendored_snappy(&target);
    }

    // Export metadata for dependent crates
    println!(
        "cargo:cargo_manifest_dir={}",
        env::var("CARGO_MANIFEST_DIR").unwrap()
    );
    println!("cargo:out_dir={}", env::var("OUT_DIR").unwrap());
}

// ================================================================================================
// System vs Vendored Decision Logic
// ================================================================================================

/// Determine whether to use system RocksDB or build from vendored sources
fn should_use_system_rocksdb() -> bool {
    // Priority order:
    // 1. ROCKSDB_LIB_DIR environment variable (forces system linking)
    // 2. no-vendor feature flag (forces system linking)
    // 3. vendored feature flag (forces vendored build)
    // 4. Default: vendored build

    if env::var("ROCKSDB_LIB_DIR").is_ok() {
        println!("cargo:warning=Using system RocksDB (ROCKSDB_LIB_DIR is set)");
        return true;
    }

    if cfg!(feature = "no-vendor") {
        println!("cargo:warning=Using system RocksDB (no-vendor feature enabled)");
        return true;
    }

    if cfg!(feature = "vendored") {
        println!("cargo:warning=Building vendored RocksDB (vendored feature enabled)");
        return false;
    }

    // Default to vendored
    false
}

/// Find the include directory for system RocksDB
fn find_system_include_dir() -> String {
    // Check environment variable first
    if let Ok(dir) = env::var("ROCKSDB_INCLUDE_DIR") {
        return dir;
    }

    // Try common system paths
    let common_paths = [
        "/usr/include",
        "/usr/local/include",
        "/opt/homebrew/include",
        "/opt/local/include",
    ];

    for path in &common_paths {
        let rocksdb_header = format!("{}/rocksdb/c.h", path);
        if Path::new(&rocksdb_header).exists() {
            return path.to_string();
        }
    }

    panic!(
        "Error: Cannot find RocksDB headers for system linking.\n\
         \n\
         Please either:\n\
         1. Install RocksDB development headers (e.g., librocksdb-dev)\n\
         2. Set ROCKSDB_INCLUDE_DIR to point to the include directory\n\
         3. Use vendored build by removing 'no-vendor' feature"
    );
}

// ================================================================================================
// System Linking Functions
// ================================================================================================

/// Link against system RocksDB library
fn link_system_rocksdb(target: &str) {
    // Special case for FreeBSD
    if target.contains("freebsd") {
        link_freebsd_rocksdb();
        return;
    }

    // Try environment variable configuration
    if try_link_via_env("ROCKSDB") {
        link_cpp_stdlib(target);
        return;
    }

    // Try pkg-config
    if try_link_via_pkg_config("rocksdb") {
        return;
    }

    panic!(
        "Error: Cannot find system RocksDB library.\n\
         \n\
         Please either:\n\
         1. Install RocksDB (e.g., librocksdb-dev)\n\
         2. Set ROCKSDB_LIB_DIR to the library directory\n\
         3. Use vendored build by removing 'no-vendor' feature\n\
         \n\
         You can also control linking:\n\
         - ROCKSDB_STATIC=1 for static linking\n\
         - ROCKSDB_LIB_DIR=/path/to/lib for custom location"
    );
}

/// Special handling for FreeBSD
fn link_freebsd_rocksdb() {
    println!("cargo:rustc-link-search=native=/usr/local/lib");
    let mode = if env::var("ROCKSDB_STATIC").is_ok() {
        "static"
    } else {
        "dylib"
    };
    println!("cargo:rustc-link-lib={}=rocksdb", mode);
}

/// Try to link a library via environment variables
fn try_link_via_env(lib_name: &str) -> bool {
    println!("cargo:rerun-if-env-changed={}_LIB_DIR", lib_name);
    println!("cargo:rerun-if-env-changed={}_STATIC", lib_name);

    if let Ok(lib_dir) = env::var(format!("{}_LIB_DIR", lib_name)) {
        println!("cargo:rustc-link-search=native={}", lib_dir);

        let mode = if env::var(format!("{}_STATIC", lib_name)).is_ok() {
            "static"
        } else if cfg!(feature = "dynamic") {
            "dylib"
        } else {
            "static" // Default to static
        };

        println!("cargo:rustc-link-lib={}={}", mode, lib_name.to_lowercase());
        return true;
    }

    false
}

/// Try to link via pkg-config
fn try_link_via_pkg_config(_lib_name: &str) -> bool {
    #[cfg(all(not(target_os = "windows"), feature = "pkg-config"))]
    {
        let mut config = pkg_config::Config::new();
        config.atleast_version("6.0");

        if cfg!(feature = "static-only") {
            config.statik(true);
        }

        if config.probe(_lib_name).is_ok() {
            return true;
        }
    }

    false
}

/// Try to link a system library (generic)
fn try_link_system_lib(lib_name: &str) -> bool {
    println!("cargo:rerun-if-env-changed={}_COMPILE", lib_name);

    // Check if we should force compilation from source
    if let Ok(v) = env::var(format!("{}_COMPILE", lib_name)) {
        if v.to_lowercase() == "true" || v == "1" {
            return false;
        }
    }

    try_link_via_env(lib_name) || try_link_via_pkg_config(&lib_name.to_lowercase())
}

/// Link C++ standard library
fn link_cpp_stdlib(target: &str) {
    if let Ok(stdlib) = env::var("CXXSTDLIB") {
        println!("cargo:rustc-link-lib=dylib={}", stdlib);
    } else if target.contains("apple") || target.contains("freebsd") || target.contains("openbsd") {
        println!("cargo:rustc-link-lib=dylib=c++");
    } else if target.contains("linux") {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    } else if target.contains("aix") {
        println!("cargo:rustc-link-lib=dylib=c++");
        println!("cargo:rustc-link-lib=dylib=c++abi");
    }
}

// ================================================================================================
// Vendored Build Functions
// ================================================================================================

/// Build RocksDB from vendored sources
fn build_vendored_rocksdb(target: &str) {
    println!("cargo:rerun-if-changed=rocksdb/");
    verify_submodule_directory("rocksdb");

    let mut config = cc::Build::new();

    // Configure includes
    config.include("rocksdb/include/");
    config.include("rocksdb/");
    config.include("rocksdb/third-party/gtest-1.8.1/fused-src/");
    config.include(".");

    // Basic configuration
    config.define("NDEBUG", Some("1"));
    config.define("ROCKSDB_SUPPORT_THREAD_LOCAL", None);

    // Configure platform
    let platform_sources = configure_platform(&mut config, target);

    // Configure compression
    configure_compression(&mut config);

    // Configure features
    configure_features(&mut config, target);

    // Configure compiler
    configure_compiler(&mut config, target);

    // Load and compile sources
    let sources = load_rocksdb_sources(target, platform_sources);
    for source in sources {
        config.file(format!("rocksdb/{}", source));
    }
    config.file("build_version.cc");

    // Compile
    config.cpp(true);
    config.compile("librocksdb.a");
}

/// Build Snappy from vendored sources
fn build_vendored_snappy(target: &str) {
    println!("cargo:rerun-if-changed=snappy/");
    verify_submodule_directory("snappy");

    let mut config = cc::Build::new();

    config.include("snappy/");
    config.include(".");
    config.define("NDEBUG", Some("1"));
    config.extra_warnings(false);

    // Configure for target
    if target.contains("msvc") {
        config.flag("-EHsc");
        if cfg!(feature = "mt_static") {
            config.static_crt(true);
        }
    } else {
        config.flag("-std=c++11");
    }

    // Handle endianness
    if env::var("CARGO_CFG_TARGET_ENDIAN").unwrap() == "big" {
        config.define("SNAPPY_IS_BIG_ENDIAN", Some("1"));
    }

    // Add sources
    config.file("snappy/snappy.cc");
    config.file("snappy/snappy-sinksource.cc");
    config.file("snappy/snappy-c.cc");

    // Compile
    config.cpp(true);
    config.compile("libsnappy.a");
}

// ================================================================================================
// Configuration Functions
// ================================================================================================

/// Configure platform-specific settings
fn configure_platform(config: &mut cc::Build, target: &str) -> Vec<&'static str> {
    let mut platform_sources = vec![];

    if target.contains("apple-ios") {
        config.define("OS_MACOSX", None);
        config.define("IOS_CROSS_COMPILE", None);
        config.define("PLATFORM", "IOS");
        config.define("NIOSTATS_CONTEXT", None);
        config.define("NPERF_CONTEXT", None);
        config.define("ROCKSDB_PLATFORM_POSIX", None);
        config.define("ROCKSDB_LIB_IO_POSIX", None);
        env::set_var("IPHONEOS_DEPLOYMENT_TARGET", "12.0");
    } else if target.contains("darwin") {
        config.define("OS_MACOSX", None);
        config.define("ROCKSDB_PLATFORM_POSIX", None);
        config.define("ROCKSDB_LIB_IO_POSIX", None);
    } else if target.contains("android") {
        config.define("OS_ANDROID", None);
        config.define("ROCKSDB_PLATFORM_POSIX", None);
        config.define("ROCKSDB_LIB_IO_POSIX", None);
        if target == "armv7-linux-androideabi" {
            config.define("_FILE_OFFSET_BITS", Some("32"));
        }
    } else if target.contains("linux") {
        config.define("OS_LINUX", None);
        config.define("ROCKSDB_PLATFORM_POSIX", None);
        config.define("ROCKSDB_LIB_IO_POSIX", None);
        config.define("ROCKSDB_SCHED_GETCPU_PRESENT", None);

        #[cfg(target_os = "linux")]
        if check_getauxval_supported() {
            config.define("ROCKSDB_AUXV_GETAUXVAL_PRESENT", None);
        }
    } else if target.contains("dragonfly") {
        config.define("OS_DRAGONFLYBSD", None);
        config.define("ROCKSDB_PLATFORM_POSIX", None);
        config.define("ROCKSDB_LIB_IO_POSIX", None);
    } else if target.contains("freebsd") {
        config.define("OS_FREEBSD", None);
        config.define("ROCKSDB_PLATFORM_POSIX", None);
        config.define("ROCKSDB_LIB_IO_POSIX", None);
    } else if target.contains("netbsd") {
        config.define("OS_NETBSD", None);
        config.define("ROCKSDB_PLATFORM_POSIX", None);
        config.define("ROCKSDB_LIB_IO_POSIX", None);
    } else if target.contains("openbsd") {
        config.define("OS_OPENBSD", None);
        config.define("ROCKSDB_PLATFORM_POSIX", None);
        config.define("ROCKSDB_LIB_IO_POSIX", None);
    } else if target.contains("aix") {
        config.define("OS_AIX", None);
        config.define("ROCKSDB_PLATFORM_POSIX", None);
        config.define("ROCKSDB_LIB_IO_POSIX", None);
    } else if target.contains("windows") {
        link_windows_lib("rpcrt4");
        link_windows_lib("shlwapi");

        config.define("DWIN32", None);
        config.define("OS_WIN", None);
        config.define("_MBCS", None);
        config.define("WIN64", None);
        config.define("NOMINMAX", None);
        config.define("ROCKSDB_WINDOWS_UTF8_FILENAMES", None);

        if target == "x86_64-pc-windows-gnu" {
            config.define("_POSIX_C_SOURCE", Some("1"));
            config.define("_WIN32_WINNT", Some("_WIN32_WINNT_VISTA"));
        }

        // Windows-specific sources
        platform_sources.extend([
            "port/win/env_default.cc",
            "port/win/port_win.cc",
            "port/win/xpress_win.cc",
            "port/win/io_win.cc",
            "port/win/win_thread.cc",
            "port/win/env_win.cc",
            "port/win/win_logger.cc",
        ]);

        if cfg!(feature = "jemalloc") {
            platform_sources.push("port/win/win_jemalloc.cc");
        }
    }

    // Handle 32-bit file offsets
    if target != "armv7-linux-androideabi"
        && env::var("CARGO_CFG_TARGET_POINTER_WIDTH").unwrap() != "64"
    {
        config.define("_FILE_OFFSET_BITS", Some("64"));
        config.define("_LARGEFILE64_SOURCE", Some("1"));
    }

    platform_sources
}

/// Configure compression features
fn configure_compression(config: &mut cc::Build) {
    if cfg!(feature = "snappy") {
        config.define("SNAPPY", Some("1"));
        config.include("snappy/");
    }

    if cfg!(feature = "lz4") {
        config.define("LZ4", Some("1"));
        if let Some(path) = env::var_os("DEP_LZ4_INCLUDE") {
            config.include(path);
        }
    }

    if cfg!(feature = "zstd") {
        config.define("ZSTD", Some("1"));
        if let Some(path) = env::var_os("DEP_ZSTD_INCLUDE") {
            config.include(path);
        }
        if cfg!(feature = "zstd-static-linking-only") {
            config.define("ZSTD_STATIC_LINKING_ONLY", Some("1"));
        }
    }

    if cfg!(feature = "zlib") {
        config.define("ZLIB", Some("1"));
        if let Some(path) = env::var_os("DEP_Z_INCLUDE") {
            config.include(path);
        }
    }

    if cfg!(feature = "bzip2") {
        config.define("BZIP2", Some("1"));
        if let Some(path) = env::var_os("DEP_BZIP2_INCLUDE") {
            config.include(path);
        }
    }
}

/// Configure additional features
fn configure_features(config: &mut cc::Build, target: &str) {
    // RTTI support
    if cfg!(feature = "rtti") {
        config.define("USE_RTTI", Some("1"));
    }

    // Malloc usable size
    #[cfg(feature = "malloc-usable-size")]
    if target.contains("linux") {
        config.define("ROCKSDB_MALLOC_USABLE_SIZE", Some("1"));
    }

    // LTO
    if cfg!(feature = "lto") {
        config.flag("-flto");
        if !config.get_compiler().is_like_clang() {
            panic!(
                "Error: LTO requires clang compiler.\n\
                 \n\
                 Please either:\n\
                 1. Disable the 'lto' feature\n\
                 2. Set CC=clang CXX=clang++ environment variables"
            );
        }
    }

    // Jemalloc
    if cfg!(feature = "jemalloc") && NO_JEMALLOC_TARGETS.iter().all(|t| !target.contains(t)) {
        config.define("ROCKSDB_JEMALLOC", Some("1"));
        config.define("JEMALLOC_NO_DEMANGLE", Some("1"));
        if let Some(root) = env::var_os("DEP_JEMALLOC_ROOT") {
            config.include(Path::new(&root).join("include"));
        }
    }

    // IO-uring
    #[cfg(feature = "io-uring")]
    if target.contains("linux") {
        if pkg_config::probe_library("liburing").is_err() {
            panic!(
                "Error: io-uring feature requires liburing.\n\
                 \n\
                 Please either:\n\
                 1. Install liburing development package\n\
                 2. Disable the 'io-uring' feature"
            );
        }
        config.define("ROCKSDB_IOURING_PRESENT", Some("1"));
    }

    // Target features (x86_64)
    if let (true, Ok(features)) = (
        target.contains("x86_64"),
        env::var("CARGO_CFG_TARGET_FEATURE"),
    ) {
        let features: Vec<_> = features.split(',').collect();
        let feature_flags = [
            ("sse2", "-msse2"),
            ("sse4.1", "-msse4.1"),
            ("sse4.2", "-msse4.2"),
            ("avx2", "-mavx2"),
            ("bmi1", "-mbmi"),
            ("lzcnt", "-mlzcnt"),
        ];

        for (feature, flag) in &feature_flags {
            if features.contains(feature) {
                config.flag_if_supported(flag);
            }
        }

        if !target.contains("android") && features.contains(&"pclmulqdq") {
            config.flag_if_supported("-mpclmul");
        }
    }
}

/// Configure compiler settings
fn configure_compiler(config: &mut cc::Build, target: &str) {
    if target.contains("msvc") {
        if cfg!(feature = "mt_static") {
            config.static_crt(true);
        }
        config.flag("-EHsc");
        config.flag("-std:c++17");
    } else {
        // C++ standard
        let cxx_std = env::var("ROCKSDB_CXX_STD").unwrap_or_else(|_| "c++17".to_string());
        config.flag(format!("-std={}", cxx_std));

        // Warning flags
        config.flag("-Wsign-compare");
        config.flag("-Wshadow");
        config.flag("-Wno-unused-parameter");
        config.flag("-Wno-unused-variable");
        config.flag("-Woverloaded-virtual");
        config.flag("-Wnon-virtual-dtor");
        config.flag("-Wno-missing-field-initializers");
        config.flag("-Wno-strict-aliasing");
        config.flag("-Wno-invalid-offsetof");
    }

    // RISC-V needs libatomic
    if target.contains("riscv64gc") {
        println!("cargo:rustc-link-lib=atomic");
    }

    // Include cstdint on non-Windows
    if !target.contains("windows") {
        config.flag("-include").flag("cstdint");
    }
}

/// Load RocksDB source files
fn load_rocksdb_sources(target: &str, platform_sources: Vec<&'static str>) -> Vec<&'static str> {
    let mut sources = include_str!("rocksdb_lib_sources.txt")
        .trim()
        .split('\n')
        .map(str::trim)
        .filter(|file| !matches!(*file, "util/build_version.cc"))
        .collect::<Vec<&'static str>>();

    // Handle Windows-specific source adjustments
    if target.contains("windows") {
        sources.retain(|file| {
            !matches!(
                *file,
                "port/port_posix.cc" | "env/env_posix.cc" | "env/fs_posix.cc" | "env/io_posix.cc"
            )
        });

        sources.extend(platform_sources);
    }

    sources
}

// ================================================================================================
// Helper Functions
// ================================================================================================

/// Link a Windows library
fn link_windows_lib(name: &str) {
    let target = env::var("TARGET").unwrap();
    let target_parts: Vec<_> = target.split('-').collect();

    if target_parts.get(2) == Some(&"windows") {
        println!("cargo:rustc-link-lib=dylib={}", name);
        if target_parts.get(3) == Some(&"gnu") {
            let dir = env::var("CARGO_MANIFEST_DIR").unwrap();
            println!("cargo:rustc-link-search=native={}/{}", dir, target_parts[0]);
        }
    }
}

/// Verify submodule directory exists and is not empty
fn verify_submodule_directory(name: &str) {
    match fs::read_dir(name) {
        Ok(entries) => {
            if entries.count() == 0 {
                panic!(
                    "Error: The '{}' directory is empty.\n\
                     \n\
                     This is required for vendored builds. Please run:\n\
                     git submodule update --init --recursive\n\
                     \n\
                     Or use system library by setting ROCKSDB_LIB_DIR",
                    name
                );
            }
        }
        Err(e) => {
            panic!(
                "Error: Cannot access '{}' directory: {}\n\
                 \n\
                 For vendored builds, please run:\n\
                 git submodule update --init --recursive\n\
                 \n\
                 Or use system library by setting ROCKSDB_LIB_DIR",
                name, e
            );
        }
    }
}

/// Ensure submodules are initialized
fn ensure_submodules_initialized() {
    if !Path::new("rocksdb/AUTHORS").exists() {
        update_submodules();
    }
}

/// Update git submodules
fn update_submodules() {
    println!("cargo:warning=Initializing git submodules...");

    let output = Command::new("git")
        .args(["submodule", "update", "--init"])
        .current_dir("../")
        .output();

    match output {
        Ok(output) if output.status.success() => {
            println!("cargo:warning=Submodules initialized successfully");
        }
        Ok(output) => {
            panic!(
                "Failed to initialize submodules:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(e) => {
            panic!("Failed to run git: {}", e);
        }
    }
}

/// Generate bindings using bindgen
fn generate_bindings(include_dir: &str) {
    let header_path = format!("{}/rocksdb/c.h", include_dir);

    if !Path::new(&header_path).exists() {
        panic!(
            "Error: RocksDB C header not found at: {}\n\
             \n\
             Please ensure either:\n\
             1. RocksDB is properly installed\n\
             2. ROCKSDB_INCLUDE_DIR points to the correct location\n\
             3. Submodules are initialized for vendored builds",
            header_path
        );
    }

    let bindings = bindgen::Builder::default()
        .header(header_path)
        .derive_debug(false)
        .blocklist_type("max_align_t")
        .ctypes_prefix("libc")
        .size_t_is_usize(true)
        .generate()
        .expect("Failed to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Failed to write bindings");
}

#[cfg(target_os = "linux")]
fn check_getauxval_supported() -> bool {
    unsafe { getauxval(AT_HWCAP) != 0 }
}

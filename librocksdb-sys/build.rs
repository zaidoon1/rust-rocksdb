use std::path::Path;
use std::{env, fs, path::PathBuf, process::Command};

#[cfg(target_os = "linux")]
use libc::{AT_HWCAP, getauxval};

// On these platforms jemalloc-sys will use a prefixed jemalloc which cannot be linked together
// with RocksDB.
// See https://github.com/tikv/jemallocator/blob/f7adfca5aff272b43fd3ad896252b57fbbd9c72a/jemalloc-sys/src/env.rs#L24
const NO_JEMALLOC_TARGETS: &[&str] = &["android", "dragonfly", "darwin"];

fn link(name: &str, bundled: bool) {
    use std::env::var;
    let target = var("TARGET").unwrap();
    let target: Vec<_> = target.split('-').collect();
    if target.get(2) == Some(&"windows") {
        println!("cargo:rustc-link-lib=dylib={name}");
        if bundled && target.get(3) == Some(&"gnu") {
            let dir = var("CARGO_MANIFEST_DIR").unwrap();
            println!("cargo:rustc-link-search=native={}/{}", dir, target[0]);
        }
    }
}

fn fail_on_empty_directory(name: &str) {
    if fs::read_dir(name).unwrap().count() == 0 {
        println!("The `{name}` directory is empty, did you forget to pull the submodules?");
        println!("Try `git submodule update --init --recursive`");
        panic!();
    }
}

fn rocksdb_include_dir() -> String {
    match env::var("ROCKSDB_INCLUDE_DIR") {
        Ok(val) => val,
        Err(_) => "rocksdb/include".to_string(),
    }
}

fn bindgen_rocksdb() {
    let bindings = bindgen::Builder::default()
        .header(rocksdb_include_dir() + "/rocksdb/c.h")
        .derive_debug(false)
        .blocklist_type("max_align_t") // https://github.com/rust-lang-nursery/rust-bindgen/issues/550
        .ctypes_prefix("libc")
        .size_t_is_usize(true)
        .generate()
        .expect("unable to generate rocksdb bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("unable to write rocksdb bindings");
}

#[cfg(target_os = "linux")]
fn check_getauxval_supported() -> bool {
    unsafe {
        let aux_value = getauxval(AT_HWCAP);
        if aux_value == 0 {
            return false;
        }

        true
    }
}

/// Splits `CARGO_ENCODED_RUSTFLAGS` into a Vec.
fn split_encoded_rustflags() -> Vec<String> {
    let flags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();

    // extra flags that Cargo invokes rustc with, separated by a 0x1f character
    // https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-crates
    flags.split("\x1f").map(|flag| flag.to_string()).collect()
}

/// Returns the argument to `-Ctarget-cpu=` if it exists.
fn get_target_cpu_flag() -> Option<String> {
    const TARGET_CPU_FLAG: &str = "-Ctarget-cpu=";
    let flags = split_encoded_rustflags();
    let complete_flag = flags.iter().find(|flag| flag.starts_with(TARGET_CPU_FLAG));
    complete_flag.map(|flag| flag[TARGET_CPU_FLAG.len()..].to_string())
}

/// If the Rust `-Ctarget-cpu=` option is set, this attempts to pass it through to the C/C++
/// compiler. It should print a Cargo build warning if the compiler does not support the flag,
/// or if the architecture is not supported.
fn pass_through_target_cpu(cfg: &mut cc::Build) {
    let Some(target_cpu_flag) = get_target_cpu_flag() else {
        return;
    };

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    match arch.as_str() {
        "x86_64" => {
            cfg.flag_if_supported(format!("-march={target_cpu_flag}"));
        }
        "aarch64" => {
            cfg.flag_if_supported(format!("-mcpu={target_cpu_flag}"));
        }
        // TODO: add more architectures/compilers
        _ => {
            println!(
                "cargo::warning=unknown target architecture: {arch}; C/C++ target flags not passed through"
            );
        }
    }
}

fn build_rocksdb() {
    // https://doc.rust-lang.org/cargo/reference/environment-variables.html
    let target = env::var("TARGET").unwrap();
    // https://doc.rust-lang.org/reference/conditional-compilation.html#target_arch
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target_features_env = env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
    let target_features: Vec<_> = target_features_env.split(',').collect();

    let mut config = cc::Build::new();
    config.include("rocksdb/include/");
    config.include("rocksdb/");
    config.include("rocksdb/third-party/gtest-1.8.1/fused-src/");

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

    if cfg!(feature = "rtti") {
        config.define("USE_RTTI", Some("1"));
    }

    #[cfg(feature = "malloc-usable-size")]
    if target.contains("linux") {
        config.define("ROCKSDB_MALLOC_USABLE_SIZE", Some("1"));
    }

    // https://github.com/facebook/rocksdb/blob/be7703b27d9b3ac458641aaadf27042d86f6869c/Makefile#L195
    if cfg!(feature = "lto") {
        config.flag("-flto");
        if !config.get_compiler().is_like_clang() {
            panic!(
                "LTO is only supported with clang. Either disable the `lto` feature\
             or set `CC=/usr/bin/clang CXX=/usr/bin/clang++` environment variables."
            );
        }
    }

    config.include(".");
    config.define("NDEBUG", Some("1"));

    // true for C++ >= 17; we set -std=c++20 below
    config.define("HAVE_ALIGNED_NEW", None);

    // __uint128_t is supported by GCC and Clang; Don't use it for MSVC
    // TODO: implement a detection script?
    if !target.contains("msvc") {
        config.define("HAVE_UINT128_EXTENSION", None);
    }

    let mut lib_sources = include_str!("rocksdb_lib_sources.txt")
        .trim()
        .split('\n')
        .map(str::trim)
        // We have a pre-generated a version of build_version.cc in the local directory
        .filter(|file| !matches!(*file, "util/build_version.cc"))
        .collect::<Vec<&'static str>>();

    // attempt to pass through the RUSTFLAGS -Ctarget-cpu to allow the same optimizations for C/C++
    pass_through_target_cpu(&mut config);

    // CPU-specific build configuration
    if target_arch == "x86_64" {
        // This is needed to enable hardware CRC32C. Technically, SSE 4.2 is
        // only available since Intel Nehalem (about 2010) and AMD Bulldozer
        // (about 2011).
        if target_features.contains(&"sse2") {
            config.flag_if_supported("-msse2");
        }
        if target_features.contains(&"sse4.1") {
            config.flag_if_supported("-msse4.1");
        }
        if target_features.contains(&"sse4.2") {
            config.flag_if_supported("-msse4.2");
        } else {
            println!(
                r#"cargo::warning=compiling without SSE4.2: CRC will be slow (set RUSTFLAGS="-Ctarget-cpu=..." to optimize RocksDB e.g. -Ctarget-cpu=broadwell)"#
            );
        }
        // Pass along additional target features as defined in
        // build_tools/build_detect_platform.
        if target_features.contains(&"avx2") {
            config.flag_if_supported("-mavx2");
        }
        if target_features.contains(&"bmi1") {
            config.flag_if_supported("-mbmi");
        }
        if target_features.contains(&"lzcnt") {
            config.flag_if_supported("-mlzcnt");
        }

        if !target.contains("android") && target_features.contains(&"pclmulqdq") {
            config.flag_if_supported("-mpclmul");
        }

        if target_features.contains(&"avx") && !target_features.contains(&"pclmulqdq") {
            // RocksDB BUG (<= 10.11.0/2026-01-23): assumes AVX implies -mpclmul
            // x86-64-v3/-v4 does not include PCLMUL
            println!(
                r#"cargo:warning=RocksDB BUG: target arch missing -mpclmul; compile may fail: pass named architecture e.g. -Ctarget-cpu=broadwell"#
            );
        }
    } else if target_arch == "aarch64" {
        if target_features.contains(&"crc") && target_features.contains(&"aes") {
            // the target supports the instructions RocksDB needs: if we don't have a target-cpu,
            // use -march=armv8-a+crc+aes+crypto, like the RocksDB Makefile.
            // If we DO have a target-cpu, assume pass_through_target_cpu() has set it above
            if get_target_cpu_flag().is_none() {
                // TODO: Should just be +crc+aes but RocksDB checks for __ARM_FEATURE_CRYPTO
                // https://github.com/facebook/rocksdb/pull/14217
                config.flag_if_supported("-march=armv8-a+crc+aes+crypto");
            }
        } else {
            println!(
                r#"cargo:warning=building for aarch64 WITHOUT CRC instruction: build with RUSTFLAGS="-Ctarget-cpu=..." to optimize RocksDB e.g. -Ctarget-cpu=neoverse-n1"#
            );
        }
    }

    if target.contains("apple-ios") {
        config.define("OS_MACOSX", None);

        config.define("IOS_CROSS_COMPILE", None);
        config.define("PLATFORM", "IOS");
        config.define("NIOSTATS_CONTEXT", None);
        config.define("NPERF_CONTEXT", None);
        config.define("ROCKSDB_PLATFORM_POSIX", None);
        config.define("ROCKSDB_LIB_IO_POSIX", None);

        // SAFETY: This is the build script, which is single-threaded and runs
        // before any other code. Setting environment variables here is safe.
        unsafe { env::set_var("IPHONEOS_DEPLOYMENT_TARGET", "12.0") };
    } else if target.contains("darwin") {
        config.define("OS_MACOSX", None);
        config.define("ROCKSDB_PLATFORM_POSIX", None);
        config.define("ROCKSDB_LIB_IO_POSIX", None);
    } else if target.contains("android") {
        config.define("OS_ANDROID", None);
        config.define("ROCKSDB_PLATFORM_POSIX", None);
        config.define("ROCKSDB_LIB_IO_POSIX", None);

        if &target == "armv7-linux-androideabi" {
            config.define("_FILE_OFFSET_BITS", Some("32"));
        }
    } else if target.contains("aix") {
        config.define("OS_AIX", None);
        config.define("ROCKSDB_PLATFORM_POSIX", None);
        config.define("ROCKSDB_LIB_IO_POSIX", None);
    } else if target.contains("linux") {
        config.define("OS_LINUX", None);
        config.define("ROCKSDB_PLATFORM_POSIX", None);
        config.define("ROCKSDB_LIB_IO_POSIX", None);
        config.define("ROCKSDB_SCHED_GETCPU_PRESENT", None);

        #[cfg(target_os = "linux")]
        if check_getauxval_supported() {
            config.define("ROCKSDB_AUXV_GETAUXVAL_PRESENT", None);
        }
        config.define("ROCKSDB_FALLOCATE_PRESENT", None);
        config.define("ROCKSDB_RANGESYNC_PRESENT", None);
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
    } else if target.contains("windows") {
        link("rpcrt4", false);
        link("shlwapi", false);
        config.define("DWIN32", None);
        config.define("OS_WIN", None);
        config.define("_MBCS", None);
        config.define("WIN64", None);
        config.define("NOMINMAX", None);
        config.define("ROCKSDB_WINDOWS_UTF8_FILENAMES", None);

        if &target == "x86_64-pc-windows-gnu" {
            // Tell MinGW to create localtime_r wrapper of localtime_s function.
            config.define("_POSIX_C_SOURCE", Some("1"));
            // Tell MinGW to use at least Windows Vista headers instead of the ones of Windows XP.
            // (This is minimum supported version of rocksdb)
            config.define("_WIN32_WINNT", Some("_WIN32_WINNT_VISTA"));
        }

        // Remove POSIX-specific sources
        lib_sources = lib_sources
            .iter()
            .cloned()
            .filter(|file| {
                !matches!(
                    *file,
                    "port/port_posix.cc"
                        | "env/env_posix.cc"
                        | "env/fs_posix.cc"
                        | "env/io_posix.cc"
                )
            })
            .collect::<Vec<&'static str>>();

        // Add Windows-specific sources
        lib_sources.extend([
            "port/win/env_default.cc",
            "port/win/port_win.cc",
            "port/win/xpress_win.cc",
            "port/win/io_win.cc",
            "port/win/win_thread.cc",
            "port/win/env_win.cc",
            "port/win/win_logger.cc",
        ]);

        if cfg!(feature = "jemalloc") {
            lib_sources.push("port/win/win_jemalloc.cc");
        }
    }

    if cfg!(feature = "jemalloc") && NO_JEMALLOC_TARGETS.iter().all(|i| !target.contains(i)) {
        config.define("ROCKSDB_JEMALLOC", Some("1"));
        config.define("JEMALLOC_NO_DEMANGLE", Some("1"));
        if let Some(jemalloc_root) = env::var_os("DEP_JEMALLOC_ROOT") {
            config.include(Path::new(&jemalloc_root).join("include"));
        }
    }

    #[cfg(feature = "io-uring")]
    if target.contains("linux") {
        pkg_config::probe_library("liburing")
            .expect("The io-uring feature was requested but the library is not available");
        config.define("ROCKSDB_IOURING_PRESENT", Some("1"));
    }

    #[cfg(feature = "coroutines")]
    coroutines_compile_config(&mut config, &target);

    if &target != "armv7-linux-androideabi"
        && env::var("CARGO_CFG_TARGET_POINTER_WIDTH").unwrap() != "64"
    {
        config.define("_FILE_OFFSET_BITS", Some("64"));
        config.define("_LARGEFILE64_SOURCE", Some("1"));
    }

    if target.contains("msvc") {
        if cfg!(feature = "mt_static") {
            config.static_crt(true);
        }
        config.flag("-EHsc");
        // Don't use cxx_standard: Uses : instead of =
        config.flag("-std:c++20");
    } else {
        config.flag(cxx_standard());
        // matches the flags in CMakeLists.txt from rocksdb
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
    if target.contains("riscv64gc") {
        // link libatomic required to build for riscv64gc
        println!("cargo:rustc-link-lib=atomic");
    }
    for file in lib_sources {
        config.file(format!("rocksdb/{file}"));
    }

    config.file("build_version.cc");

    config.cpp(true);

    if !target.contains("windows") {
        config.flag("-include").flag("cstdint");
    }

    // By default `cc` will link C++ standard library automatically,
    // see https://docs.rs/cc/latest/cc/index.html#c-support.
    // There is no need to manually set `cpp_link_stdlib`.

    config.compile("librocksdb.a");
}

fn build_snappy() {
    let target = env::var("TARGET").unwrap();
    let endianness = env::var("CARGO_CFG_TARGET_ENDIAN").unwrap();
    let mut config = cc::Build::new();

    config.include("snappy/");
    config.include(".");
    config.define("NDEBUG", Some("1"));
    config.extra_warnings(false);

    if target.contains("msvc") {
        config.flag("-EHsc");
        if cfg!(feature = "mt_static") {
            config.static_crt(true);
        }
        config.flag("-std:c++20");
    } else {
        config.flag("-std=c++20");
    }

    if endianness == "big" {
        config.define("SNAPPY_IS_BIG_ENDIAN", Some("1"));
    }

    config.file("snappy/snappy.cc");
    config.file("snappy/snappy-sinksource.cc");
    config.file("snappy/snappy-c.cc");
    config.cpp(true);
    config.compile("libsnappy.a");
}

fn try_to_find_and_link_lib(lib_name: &str) -> bool {
    println!("cargo:rerun-if-env-changed={lib_name}_COMPILE");
    if let Ok(v) = env::var(format!("{lib_name}_COMPILE"))
        && (v.to_lowercase() == "true" || v == "1")
    {
        return false;
    }

    println!("cargo:rerun-if-env-changed={lib_name}_LIB_DIR");
    println!("cargo:rerun-if-env-changed={lib_name}_STATIC");

    if let Ok(lib_dir) = env::var(format!("{lib_name}_LIB_DIR")) {
        println!("cargo:rustc-link-search=native={lib_dir}");
        let mode = match env::var_os(format!("{lib_name}_STATIC")) {
            Some(_) => "static",
            None => "dylib",
        };
        println!("cargo:rustc-link-lib={}={}", mode, lib_name.to_lowercase());
        return true;
    }
    false
}

/// Returns the value of the `ROCKSDB_CXX_STD` env var, or the default `-std=c++{version}` flag for
/// building RocksDB.
fn cxx_standard() -> String {
    env::var("ROCKSDB_CXX_STD").map_or("-std=c++20".to_owned(), |cxx_std| {
        if !cxx_std.starts_with("-std=") {
            format!("-std={cxx_std}")
        } else {
            cxx_std
        }
    })
}

fn update_submodules() {
    let program = "git";
    let dir = "../";
    let args = ["submodule", "update", "--init"];
    println!(
        "Running command: \"{} {}\" in dir: {}",
        program,
        args.join(" "),
        dir
    );
    let ret = Command::new(program).current_dir(dir).args(args).status();

    match ret.map(|status| (status.success(), status.code())) {
        Ok((true, _)) => (),
        Ok((false, Some(c))) => panic!("Command failed with error code {c}"),
        Ok((false, None)) => panic!("Command got killed"),
        Err(e) => panic!("Command failed with error: {e}"),
    }
}

fn cpp_link_stdlib(target: &str) {
    // according to https://github.com/alexcrichton/cc-rs/blob/master/src/lib.rs#L2189
    if let Ok(stdlib) = env::var("CXXSTDLIB") {
        println!("cargo:rustc-link-lib=dylib={stdlib}");
    } else if target.contains("apple") || target.contains("freebsd") || target.contains("openbsd") {
        println!("cargo:rustc-link-lib=dylib=c++");
    } else if target.contains("linux") {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    } else if target.contains("aix") {
        println!("cargo:rustc-link-lib=dylib=c++");
        println!("cargo:rustc-link-lib=dylib=c++abi");
    }
}

fn main() {
    if !Path::new("rocksdb/AUTHORS").exists() {
        update_submodules();
    }
    bindgen_rocksdb();
    let target = env::var("TARGET").unwrap();

    #[cfg(feature = "coroutines")]
    validate_coroutines_target(&target);

    if !try_to_find_and_link_lib("ROCKSDB") {
        // rocksdb only works with the prebuilt rocksdb system lib on freebsd.
        // we dont need to rebuild rocksdb
        if target.contains("freebsd") {
            println!("cargo:rustc-link-search=native=/usr/local/lib");
            let mode = match env::var_os("ROCKSDB_STATIC") {
                Some(_) => "static",
                None => "dylib",
            };
            println!("cargo:rustc-link-lib={mode}=rocksdb");

            return;
        }

        println!("cargo:rerun-if-changed=rocksdb/");
        fail_on_empty_directory("rocksdb");
        build_rocksdb();
    } else {
        cpp_link_stdlib(&target);
    }

    // Folly + transitive deps must be linked after rocksdb itself so the
    // linker resolves rocksdb's coroutine references against folly. Done
    // outside `build_rocksdb()` so it also applies when ROCKSDB_LIB_DIR
    // points at an externally-built librocksdb that was compiled with
    // USE_COROUTINES.
    //
    // Note: the freebsd branch above returns early, but
    // `validate_coroutines_target()` (called before this block) already
    // panics on non-Linux targets, so the early return is unreachable when
    // the `coroutines` feature is enabled. If we ever relax the target
    // validation, this branch will need to handle freebsd explicitly.
    #[cfg(feature = "coroutines")]
    coroutines_link_config();

    if cfg!(feature = "snappy") && !try_to_find_and_link_lib("SNAPPY") {
        println!("cargo:rerun-if-changed=snappy/");
        fail_on_empty_directory("snappy");
        build_snappy();
    }

    // Allow dependent crates to locate the sources and output directory of
    // this crate. Notably, this allows a dependent crate to locate the RocksDB
    // sources and built archive artifacts provided by this crate.
    println!(
        "cargo:cargo_manifest_dir={}",
        env::var("CARGO_MANIFEST_DIR").unwrap()
    );
    println!("cargo:out_dir={}", env::var("OUT_DIR").unwrap());
}

/// Validates that the requested target is one we can build the `coroutines`
/// feature for. Folly only ships a working build for Linux; on macOS, Windows,
/// and the BSDs the folly toolchain is missing pieces (notably the io_uring
/// async file system, and folly's getdeps build itself is flaky).
#[cfg(feature = "coroutines")]
fn validate_coroutines_target(target: &str) {
    if !target.contains("linux") {
        panic!(
            "the `coroutines` feature is only supported on Linux \
             (target was `{target}`)"
        );
    }
}

/// Compile-time configuration for the coroutines build: defines, compiler
/// flags, and include paths for folly + its dependencies. Mirrors the logic
/// in RocksDB's own `CMakeLists.txt` (lines 607-667) and `Makefile`
/// (`USE_COROUTINES=1` branch around line 147).
///
/// Assumes `validate_coroutines_target()` has already been called by `main()`
/// and the target is Linux.
#[cfg(feature = "coroutines")]
fn coroutines_compile_config(config: &mut cc::Build, _target: &str) {
    config.define("USE_COROUTINES", None);
    config.define("USE_FOLLY", None);
    config.define("FOLLY_NO_CONFIG", None);
    config.define("HAVE_CXX11_ATOMIC", None);

    // GCC needs explicit -fcoroutines to enable coroutine support; clang
    // enables coroutines under -std=c++20 by default. We use `flag()` not
    // `flag_if_supported()` here: on a too-old GCC we want the build to
    // fail loudly with "unrecognized option -fcoroutines" rather than
    // silently drop the flag and produce a confusing C++ compile error
    // deep inside folly headers that depend on coroutine support.
    if !config.get_compiler().is_like_clang() {
        config.flag("-fcoroutines");
    }
    // Folly's headers trip warnings that RocksDB's stricter build flags
    // would otherwise treat as significant.
    config.flag_if_supported("-Wno-deprecated");
    config.flag_if_supported("-Wno-redundant-move");
    config.flag_if_supported("-Wno-maybe-uninitialized");
    config.flag_if_supported("-Wno-invalid-memory-model");

    let install_root = coroutines_install_root();
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
        let dir = resolve_folly_dep(&install_root, dep);
        config.include(dir.join("include"));
    }
}

/// Link-time configuration for the coroutines build: emits the
/// `cargo:rustc-link-*` directives for folly and its transitive dependencies.
/// Order matters - folly must come after `librocksdb.a` on the linker command
/// line so symbols resolve forward.
#[cfg(feature = "coroutines")]
fn coroutines_link_config() {
    println!("cargo:rerun-if-env-changed=ROCKSDB_FOLLY_INSTALL_PATH");

    let install_root = coroutines_install_root();

    let folly = resolve_folly_dep(&install_root, "folly");
    let boost = resolve_folly_dep(&install_root, "boost");
    let fmt = resolve_folly_dep(&install_root, "fmt");
    let glog = resolve_folly_dep(&install_root, "glog");
    let gflags = resolve_folly_dep(&install_root, "gflags");
    let dbl_conv = resolve_folly_dep(&install_root, "double-conversion");
    let libevent = resolve_folly_dep(&install_root, "libevent");
    let libsodium = resolve_folly_dep(&install_root, "libsodium");

    // Folly itself.
    println!(
        "cargo:rustc-link-search=native={}",
        folly.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=folly");

    // Boost components. This list mirrors the static archives that RocksDB's
    // own `folly.mk` links against (PLATFORM_LDFLAGS, ~line 50). The set is
    // larger than what folly *strictly* needs because RocksDB's dependency
    // chain reaches into more boost components than folly alone. If
    // FOLLY_COMMIT_HASH is bumped and a component is no longer produced by
    // the build, the link step will fail with "cannot find -lboost_<x>";
    // that's the signal to trim this list.
    println!(
        "cargo:rustc-link-search=native={}",
        boost.join("lib").display()
    );
    for lib in [
        "context",
        "filesystem",
        "atomic",
        "program_options",
        "regex",
        "system",
        "thread",
    ] {
        println!("cargo:rustc-link-lib=static=boost_{lib}");
    }

    println!(
        "cargo:rustc-link-search=native={}",
        dbl_conv.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=double-conversion");

    println!(
        "cargo:rustc-link-search=native={}",
        libevent.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=event");

    println!(
        "cargo:rustc-link-search=native={}",
        libsodium.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=sodium");

    // glog and gflags only build as shared libs from folly's getdeps, so we
    // link them dynamically. We deliberately do NOT emit `cargo:rustc-link-arg`
    // for rpath here: per `cargo:rustc-link-arg` semantics, those args only
    // apply to artifacts of the crate that emits them (tests/examples/benches
    // of `rust-librocksdb-sys` itself), not to downstream binaries that depend
    // on this crate (see rust-lang/cargo#9554). Embedding rpath that only
    // covers our own test binaries would be misleading. Downstream users must
    // handle runtime discovery of libglog/libgflags themselves - see the
    // "Async MultiGet with C++20 Coroutines" section in the top-level README.
    let glog_libdir = libdir_containing(&glog, "glog");
    let gflags_libdir = libdir_containing(&gflags, "gflags");
    println!("cargo:rustc-link-search=native={}", glog_libdir.display());
    println!("cargo:rustc-link-search=native={}", gflags_libdir.display());
    println!("cargo:rustc-link-lib=dylib=glog");
    println!("cargo:rustc-link-lib=dylib=gflags");
    // Export the discovered directories for downstream build scripts so they
    // can set rpath on their own crate's binaries without re-globbing folly's
    // install layout. Accessible as `DEP_ROCKSDB_FOLLY_GLOG_LIBDIR` and
    // `DEP_ROCKSDB_FOLLY_GFLAGS_LIBDIR` in dependent crates' build scripts
    // (via the `links = "rocksdb"` metadata).
    println!("cargo:folly_glog_libdir={}", glog_libdir.display());
    println!("cargo:folly_gflags_libdir={}", gflags_libdir.display());

    let fmt_libdir = libdir_containing(&fmt, "fmt");
    println!("cargo:rustc-link-search=native={}", fmt_libdir.display());
    println!("cargo:rustc-link-lib=static=fmt");

    // libdl, needed by folly.
    println!("cargo:rustc-link-lib=dylib=dl");
}

/// Returns the install root containing folly and its sibling dependency
/// directories (e.g. `<root>/folly-<hash>`, `<root>/boost-<hash>`, etc).
#[cfg(feature = "coroutines")]
fn coroutines_install_root() -> PathBuf {
    let raw = env::var("ROCKSDB_FOLLY_INSTALL_PATH").unwrap_or_else(|_| {
        panic!(
            "the `coroutines` feature requires the env var \
             ROCKSDB_FOLLY_INSTALL_PATH to point at a folly install \
             produced by `scripts/build_folly.sh` (or equivalent)."
        )
    });
    PathBuf::from(raw)
}

/// Resolves a dependency directory under folly's `installed/` tree.
///
/// getdeps uses two naming conventions:
///
/// 1. The project being built (folly itself, in our case) installs to
///    `<install_root>/<name>` with no version/hash suffix.
/// 2. Its dependencies install to `<install_root>/<name>-<hash>` where the
///    hash captures the manifest+ctx so changes to either reset the install.
///
/// We check (1) first - so `resolve_folly_dep(root, "folly")` returns
/// `<root>/folly` directly when present - and fall back to globbing (2).
/// Panics if zero or more than one directory matches in case (2): multiple
/// matches typically indicate a stale install from a prior FOLLY_COMMIT_HASH
/// mixed with the current one, which would otherwise be resolved
/// non-deterministically and could link the wrong version.
#[cfg(feature = "coroutines")]
fn resolve_folly_dep(install_root: &Path, name: &str) -> PathBuf {
    // Case 1: unsuffixed directory (used for the project being built).
    let bare = install_root.join(name);
    if bare.is_dir() {
        return bare;
    }
    // Case 2: glob for the hashed dependency directory.
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
             this usually means a stale install from a prior FOLLY_COMMIT_HASH \
             is mixed with the current one. Remove the stale entries or point \
             ROCKSDB_FOLLY_INSTALL_PATH at a clean install.",
            many.len(),
            install_root.display(),
            many.iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n  ")
        ),
    }
}

/// Returns whichever of `<prefix>/lib` or `<prefix>/lib64` actually contains
/// the named library (matching `lib<lib_name>.{so,a}*`). Panics with a clear
/// error if neither does.
///
/// Folly's getdeps install layout sometimes creates an empty `lib64/` as a
/// side effect of CMake's `GNUInstallDirs` probing (or vice-versa), so a
/// plain `.exists()` check on the directory is not enough - we'd point the
/// linker at an empty directory and get a confusing "cannot find -l<name>"
/// later. Probing for the actual library file catches this at config time
/// with a useful error message.
#[cfg(feature = "coroutines")]
fn libdir_containing(prefix: &Path, lib_name: &str) -> PathBuf {
    // Prefer `lib64/` when it has the library (more specific on RHEL-family
    // distros); fall back to `lib/` (Debian-family).
    for subdir in ["lib64", "lib"] {
        let candidate = prefix.join(subdir);
        if !candidate.is_dir() {
            continue;
        }
        let glob_pattern = candidate.join(format!("lib{lib_name}.*"));
        let pattern_str = match glob_pattern.to_str() {
            Some(s) => s,
            None => continue,
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
        "could not find `lib{lib_name}.{{so,a}}*` in either `{}/lib/` or `{}/lib64/`. \
         The folly install at `{}` looks incomplete - rerun `scripts/build_folly.sh` \
         to rebuild from scratch.",
        prefix.display(),
        prefix.display(),
        prefix.display()
    );
}

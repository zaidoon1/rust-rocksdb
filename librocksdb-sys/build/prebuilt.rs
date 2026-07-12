//! Validation and link setup for reusable RocksDB bundles.

use super::prebuilt_copy::materialize_bundle;
use super::prebuilt_hash::{sha256_file, sha256_files, sha256_tree, validate_bundle_tree};
use super::prebuilt_manifest::Manifest;
use super::*;

const FORMAT_VERSION: &str = "1";
const MANIFEST_NAME: &str = "rust-rocksdb-prebuilt.env";
const BUNDLED_ROCKSDB_REVISION: &str = "3b446089141659fad25328c5ea3e7ed283df46e4";
const SUPPORTED_FEATURES: &[&str] = &[
    "bzip2",
    "jemalloc",
    "lz4",
    "malloc-usable-size",
    "rtti",
    "snappy",
    "static",
    "zlib",
    "zstd",
    "zstd-static-linking-only",
];

/// Validate the configured bundle before emitting any native link directives.
pub(super) fn resolve(target: &Target) -> Backend {
    validate_prebuilt_platform(target);
    let source_root = prebuilt_root();
    validate_bundle_tree(&source_root, &manifest_dir());

    let manifest_path = source_root.join(MANIFEST_NAME);
    println!("cargo::rerun-if-changed={}", manifest_path.display());

    let manifest = Manifest::load(&manifest_path);
    validate_manifest(&manifest, target);

    let source_include = source_root.join("include");
    let source_bindings = source_root.join("bindings.rs");
    validate_files(&source_root, &source_include, &source_bindings, &manifest);

    let root = materialize_bundle(&source_root, &out_dir().join("prebuilt"));
    let manifest = Manifest::load(&root.join(MANIFEST_NAME));
    validate_manifest(&manifest, target);
    let include = root.join("include");
    let bindings = root.join("bindings.rs");
    validate_files(&root, &include, &bindings, &manifest);
    emit_link_directives(&root, &manifest);

    Backend::Prebuilt { include, bindings }
}

fn prebuilt_root() -> PathBuf {
    let root = PathBuf::from(
        env::var_os("ROCKSDB_PREBUILT_DIR")
            .expect("checked by Backend::resolve before prebuilt::resolve"),
    );
    let metadata = fs::symlink_metadata(&root).unwrap_or_else(|e| {
        panic!(
            "cannot inspect prebuilt RocksDB bundle root `{}`: {e}",
            root.display()
        )
    });
    if metadata.file_type().is_symlink() {
        panic!(
            "prebuilt RocksDB bundle path `{}` is a symbolic link",
            root.display()
        );
    }
    fs::canonicalize(&root).unwrap_or_else(|e| {
        panic!(
            "cannot canonicalize prebuilt RocksDB bundle root `{}`: {e}",
            root.display()
        )
    })
}

fn validate_prebuilt_platform(target: &Target) {
    if target.os == "windows" {
        panic!("ROCKSDB_PREBUILT_DIR does not support Windows yet; use ROCKSDB_COMPILE=1");
    }

    #[cfg(not(unix))]
    panic!("ROCKSDB_PREBUILT_DIR requires a Unix build host; use ROCKSDB_COMPILE=1");
}

fn validate_manifest(manifest: &Manifest, target: &Target) {
    validate_manifest_identity(manifest, target);
    validate_manifest_build(manifest, target);
    validate_manifest_provenance(manifest);
    validate_feature_set(manifest.get("features"));
}

fn validate_manifest_identity(manifest: &Manifest, target: &Target) {
    require_equal("format", manifest.get("format"), FORMAT_VERSION);
    require_equal(
        "crate_version",
        manifest.get("crate_version"),
        crate_version(),
    );
    require_equal(
        "rocksdb_version",
        manifest.get("rocksdb_version"),
        bundled_rocksdb_version(),
    );
    require_equal(
        "source_revision",
        manifest.get("source_revision"),
        BUNDLED_ROCKSDB_REVISION,
    );
    require_equal("target", manifest.get("target"), &target.triple);
}

fn validate_manifest_build(manifest: &Manifest, target: &Target) {
    require_equal("cxx_std", manifest.get("cxx_std"), &configured_cxx_std());
    require_equal(
        "cxx_stdlib",
        manifest.get("cxx_stdlib"),
        &configured_cxx_stdlib(target),
    );
    require_equal(
        "deployment_target",
        manifest.get("deployment_target"),
        configured_deployment_target(target),
    );
    require_equal("target_cpu", manifest.get("target_cpu"), "baseline");
    require_equal("link", manifest.get("link"), "static");
}

fn validate_manifest_provenance(manifest: &Manifest) {
    require_nonempty("compiler", manifest.get("compiler"));
    require_nonempty("compiler_version", manifest.get("compiler_version"));
    require_nonempty("libclang_identity", manifest.get("libclang_identity"));
    require_nonempty(
        "native_dependency_versions",
        manifest.get("native_dependency_versions"),
    );
}

fn require_equal(field: &str, actual: &str, expected: &str) {
    if actual != expected {
        panic!("prebuilt RocksDB `{field}` mismatch: bundle has `{actual}`, expected `{expected}`");
    }
}

fn require_nonempty(field: &str, value: &str) {
    if value.trim().is_empty() {
        panic!("prebuilt RocksDB manifest field `{field}` must not be empty");
    }
}

fn validate_feature_set(raw: &str) {
    let features = parse_features(raw);
    let expected = enabled_features();
    if features != expected {
        panic!(
            "prebuilt RocksDB `features` mismatch: bundle has `{}`, expected `{}`",
            features.join(","),
            expected.join(",")
        );
    }
    for feature in &features {
        if !SUPPORTED_FEATURES.contains(&feature.as_str()) {
            panic!("prebuilt RocksDB does not support feature `{feature}`; use ROCKSDB_COMPILE=1");
        }
    }
}

fn parse_features(raw: &str) -> Vec<String> {
    let mut features = raw
        .split(',')
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    features.sort();
    features.dedup();
    features
}

fn enabled_features() -> Vec<String> {
    let mut features = Vec::new();
    add_backend_features(&mut features);
    add_link_features(&mut features);
    features
}

fn add_backend_features(features: &mut Vec<String>) {
    add_feature(features, "bzip2", cfg!(feature = "bzip2"));
    add_feature(features, "coroutines", cfg!(feature = "coroutines"));
    add_feature(features, "io-uring", cfg!(feature = "io-uring"));
    add_feature(features, "jemalloc", cfg!(feature = "jemalloc"));
    add_feature(features, "lto", cfg!(feature = "lto"));
    add_feature(features, "lz4", cfg!(feature = "lz4"));
    add_feature(
        features,
        "malloc-usable-size",
        cfg!(feature = "malloc-usable-size"),
    );
    add_feature(features, "mt_static", cfg!(feature = "mt_static"));
    add_feature(features, "rtti", cfg!(feature = "rtti"));
    add_feature(features, "snappy", cfg!(feature = "snappy"));
}

fn add_link_features(features: &mut Vec<String>) {
    add_feature(features, "static", cfg!(feature = "static"));
    add_feature(features, "zlib", cfg!(feature = "zlib"));
    add_feature(features, "zstd", cfg!(feature = "zstd"));
    add_feature(
        features,
        "zstd-static-linking-only",
        cfg!(feature = "zstd-static-linking-only"),
    );
}

fn add_feature(features: &mut Vec<String>, name: &str, enabled: bool) {
    if enabled {
        features.push(name.to_owned());
    }
}

fn validate_files(root: &Path, include: &Path, bindings: &Path, manifest: &Manifest) {
    validate_bundle_files(root, include, bindings, manifest);
    validate_crate_files(manifest);
    validate_dependency_headers(manifest);
    validate_snappy(root, manifest);
}

fn validate_bundle_files(root: &Path, include: &Path, bindings: &Path, manifest: &Manifest) {
    validate_header_version(include);
    validate_tree_hash(
        "RocksDB headers",
        &include.join("rocksdb"),
        manifest.get("headers_sha256"),
    );
    validate_hash("bindings.rs", bindings, manifest.get("bindings_sha256"));
    validate_hash(
        "librocksdb.a",
        &root.join("lib/librocksdb.a"),
        manifest.get("rocksdb_sha256"),
    );
}

fn validate_crate_files(manifest: &Manifest) {
    validate_hash(
        "build.rs",
        &manifest_dir().join("build.rs"),
        manifest.get("build_script_sha256"),
    );
    validate_tree_hash(
        "prebuilt validator",
        &manifest_dir().join("build"),
        manifest.get("validator_sha256"),
    );
    validate_source_list_hash(manifest.get("source_list_sha256"));
    validate_extensions_hash(manifest.get("extensions_sha256"));
}

fn validate_source_list_hash(expected: &str) {
    let files = [
        manifest_dir().join("rocksdb_lib_sources.txt"),
        manifest_dir().join("build_version.cc"),
    ];
    for path in &files {
        println!("cargo::rerun-if-changed={}", path.display());
    }
    let actual = sha256_files(&files);
    if actual != expected {
        panic!(
            "prebuilt RocksDB source-list hash mismatch: bundle has `{expected}`, current crate has `{actual}`"
        );
    }
}

fn validate_header_version(include: &Path) {
    let header = include.join("rocksdb/version.h");
    let version = rocksdb_header_version(&header);
    require_equal("header version", &version, bundled_rocksdb_version());
    let c_header = include.join("rocksdb/c.h");
    if !c_header.is_file() {
        panic!(
            "prebuilt RocksDB is missing required header `{}`",
            c_header.display()
        );
    }
}

fn rocksdb_header_version(path: &Path) -> String {
    let content = fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "cannot read prebuilt RocksDB version header `{}`: {e}",
            path.display()
        )
    });
    let major = parse_define(path, &content, "ROCKSDB_MAJOR");
    let minor = parse_define(path, &content, "ROCKSDB_MINOR");
    let patch = parse_define(path, &content, "ROCKSDB_PATCH");
    format!("{major}.{minor}.{patch}")
}

fn parse_define(path: &Path, content: &str, name: &str) -> String {
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() == Some("#define") && parts.next() == Some(name) {
            return parts
                .next()
                .map(str::to_owned)
                .unwrap_or_else(|| panic!("`{}` has no value for `{name}`", path.display()));
        }
    }
    panic!("`{}` does not define `{name}`", path.display());
}

fn validate_hash(label: &str, path: &Path, expected: &str) {
    println!("cargo::rerun-if-changed={}", path.display());
    let actual = sha256_file(path);
    if actual != expected {
        panic!(
            "prebuilt RocksDB `{label}` hash mismatch: bundle has `{actual}`, manifest records `{expected}`"
        );
    }
}

fn validate_extensions_hash(expected: &str) {
    let files = [
        manifest_dir().join("c-api-extensions/c_api_extensions.h"),
        manifest_dir().join("c-api-extensions/c_api_extensions.cc"),
    ];
    for path in &files {
        println!("cargo::rerun-if-changed={}", path.display());
    }
    let actual = sha256_files(&files);
    if actual != expected {
        panic!(
            "prebuilt RocksDB local C-API extension hash mismatch: bundle has `{expected}`, current crate has `{actual}`"
        );
    }
}

fn validate_dependency_headers(manifest: &Manifest) {
    validate_dependency_header(
        "bzip2",
        "DEP_BZIP2_INCLUDE",
        manifest.get("bzip2_headers_sha256"),
    );
    validate_dependency_header("lz4", "DEP_LZ4_INCLUDE", manifest.get("lz4_headers_sha256"));
    validate_dependency_header("zlib", "DEP_Z_INCLUDE", manifest.get("zlib_headers_sha256"));
    validate_dependency_header(
        "zstd",
        "DEP_ZSTD_INCLUDE",
        manifest.get("zstd_headers_sha256"),
    );
    validate_jemalloc_headers(manifest.get("jemalloc_headers_sha256"));
}

fn validate_dependency_header(feature: &str, env_name: &str, expected: &str) {
    if enabled_features().iter().any(|value| value == feature) {
        let path = PathBuf::from(env::var_os(env_name).unwrap_or_else(|| {
            panic!("prebuilt RocksDB feature `{feature}` requires `{env_name}`")
        }));
        validate_tree_hash(&format!("{feature} headers"), &path, expected);
    } else {
        require_equal(&format!("{feature}_headers_sha256"), expected, "none");
    }
}

fn validate_jemalloc_headers(expected: &str) {
    if cfg!(feature = "jemalloc") {
        let root = PathBuf::from(env::var_os("DEP_JEMALLOC_ROOT").unwrap_or_else(|| {
            panic!("prebuilt RocksDB feature `jemalloc` requires `DEP_JEMALLOC_ROOT`")
        }));
        validate_tree_hash("jemalloc headers", &root.join("include"), expected);
    } else {
        require_equal("jemalloc_headers_sha256", expected, "none");
    }
}

fn validate_snappy(root: &Path, manifest: &Manifest) {
    let expected = manifest.get("snappy_sha256");
    if cfg!(feature = "snappy") {
        validate_hash("libsnappy.a", &root.join("lib/libsnappy.a"), expected);
    } else {
        require_equal("snappy_sha256", expected, "none");
    }
}

fn validate_tree_hash(label: &str, path: &Path, expected: &str) {
    println!("cargo::rerun-if-changed={}", path.display());
    let actual = sha256_tree(path);
    if actual != expected {
        panic!(
            "prebuilt RocksDB `{label}` hash mismatch: bundle has `{expected}`, current tree has `{actual}`"
        );
    }
}

fn emit_link_directives(root: &Path, manifest: &Manifest) {
    println!(
        "cargo::rustc-link-search=native={}",
        root.join("lib").display()
    );
    println!("cargo::rustc-link-lib=static=rocksdb");
    if manifest.get("features").split(',').any(|f| f == "snappy") {
        println!("cargo::rustc-link-lib=static=snappy");
    }
}

fn crate_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn bundled_rocksdb_version() -> &'static str {
    crate_version()
        .split_once('+')
        .map(|(_, version)| version)
        .unwrap_or_else(|| {
            panic!(
                "rust-librocksdb-sys package version `{}` has no RocksDB build metadata",
                crate_version()
            )
        })
}

fn configured_cxx_std() -> String {
    env::var("ROCKSDB_CXX_STD")
        .unwrap_or_else(|_| DEFAULT_CXX_STD.to_owned())
        .trim_start_matches("-std=")
        .to_owned()
}

fn configured_cxx_stdlib(target: &Target) -> String {
    if let Ok(value) = env::var("CXXSTDLIB")
        && !value.is_empty()
    {
        return value;
    }
    match target.os.as_str() {
        "macos" | "ios" | "tvos" | "watchos" | "freebsd" | "openbsd" | "android" | "aix" => "c++",
        "linux" | "netbsd" | "dragonfly" => "stdc++",
        "windows" if target.is_msvc() => "msvc",
        other => other,
    }
    .to_owned()
}

fn configured_deployment_target(target: &Target) -> &'static str {
    match target.triple.as_str() {
        "aarch64-apple-darwin" => "11.0",
        "x86_64-apple-darwin" => "10.15",
        _ => "none",
    }
}

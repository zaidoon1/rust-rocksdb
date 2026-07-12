#!/usr/bin/env bash
set -euo pipefail
umask 077

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
readonly REPO_ROOT
readonly MANIFEST_NAME="rust-rocksdb-prebuilt.env"
readonly DEFAULT_FEATURES="bzip2,lz4,snappy,static,zlib,zstd"

# shellcheck source=scripts/lib/rocksdb-prebuilt-metadata.sh
source "$SCRIPT_DIR/lib/rocksdb-prebuilt-metadata.sh"

PREFIX=""
FEATURES="$DEFAULT_FEATURES"
JOBS=""
MODE="build"

usage() {
    cat <<'EOF'
Usage: scripts/build-rocksdb-prebuilt.sh [options]

Build one optimized rust-librocksdb-sys static bundle outside Cargo's target
directory. The bundle can be reused by cargo check, clippy, test, and build.

Options:
  --prefix PATH      Bundle destination. Defaults under XDG_CACHE_HOME.
  --features LIST    Native feature list. Defaults to rust-rocksdb defaults.
  --jobs N           Parallel Cargo jobs.
  --print-config     Print the Cargo config for an existing bundle.
  --self-test        Test bundle validation without compiling RocksDB.
  -h, --help         Show this help.
EOF
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

parse_args() {
    while (($#)); do
        case "$1" in
            --prefix) PREFIX="${2:?missing value for --prefix}"; shift 2 ;;
            --features) FEATURES="${2:?missing value for --features}"; shift 2 ;;
            --jobs) JOBS="${2:?missing value for --jobs}"; shift 2 ;;
            --print-config) MODE="print"; shift ;;
            --self-test) exec "$SCRIPT_DIR/test-rocksdb-prebuilt.sh" ;;
            -h|--help) usage; exit 0 ;;
            *) fail "unknown argument: $1" ;;
        esac
    done
}

require_tool() {
    command -v "$1" >/dev/null 2>&1 || fail "required tool not found: $1"
}

metadata_field() {
    local package="$1"
    local field="$2"
    cargo metadata --manifest-path "$REPO_ROOT/Cargo.toml" --format-version 1 --no-deps |
        python3 -c '
import json, sys
package, field = sys.argv[1:]
data = json.load(sys.stdin)
match = next(p for p in data["packages"] if p["name"] == package)
print(match[field])
' "$package" "$field"
}

host_target() {
    rustc -vV | awk '/^host:/ { print $2 }'
}

normalize_features() {
    tr ',' '\n' <<<"$1" |
        sed '/^$/d' |
        LC_ALL=C sort -u |
        paste -sd, -
}

validate_features() {
    local feature
    IFS=',' read -ra values <<<"$FEATURES"
    for feature in "${values[@]}"; do
        case "$feature" in
            bzip2|jemalloc|lz4|malloc-usable-size|rtti|snappy|static|zlib|zstd|zstd-static-linking-only) ;;
            *) fail "prebuilt bundles do not support feature: $feature" ;;
        esac
    done
    feature_enabled static || fail "prebuilt bundles require the 'static' feature"
}

feature_enabled() {
    [[ ",$FEATURES," == *",$1,"* ]]
}

sha256_stream() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum | awk '{print $1}'
    else
        shasum -a 256 | awk '{print $1}'
    fi
}

sha256_file() {
    sha256_stream <"$1"
}

sha256_extensions() {
    (
        printf 'c_api_extensions.h\0'
        cat "$REPO_ROOT/librocksdb-sys/c-api-extensions/c_api_extensions.h"
        printf 'c_api_extensions.cc\0'
        cat "$REPO_ROOT/librocksdb-sys/c-api-extensions/c_api_extensions.cc"
    ) | sha256_stream
}

sha256_source_list() {
    (
        printf 'rocksdb_lib_sources.txt\0'
        cat "$REPO_ROOT/librocksdb-sys/rocksdb_lib_sources.txt"
        printf 'build_version.cc\0'
        cat "$REPO_ROOT/librocksdb-sys/build_version.cc"
    ) | sha256_stream
}

sha256_tree() {
    python3 - "$1" <<'PY'
import hashlib
from pathlib import Path
import sys

root = Path(sys.argv[1])
digest = hashlib.sha256()
for path in sorted(path for path in root.rglob("*") if path.is_file()):
    digest.update(path.relative_to(root).as_posix().encode())
    digest.update(b"\0")
    with path.open("rb") as source:
        while True:
            chunk = source.read(64 * 1024)
            if not chunk:
                break
            digest.update(chunk)
print(digest.hexdigest())
PY
}

compiler_command() {
    printf '%s\n' "${CXX:-c++}"
}

compiler_version() {
    local -a command
    read -ra command <<<"$(compiler_command)"
    "${command[@]}" --version | head -n1
}

deployment_target() {
    case "$(host_target)" in
        aarch64-apple-darwin) printf '11.0\n' ;;
        x86_64-apple-darwin) printf '10.15\n' ;;
        *) printf 'none\n' ;;
    esac
}

cxx_stdlib() {
    if [[ -n "${CXXSTDLIB:-}" ]]; then
        printf '%s\n' "$CXXSTDLIB"
        return
    fi
    case "$(host_target)" in
        *-apple-*|*-freebsd|*-openbsd|*-android) printf 'c++\n' ;;
        *-windows-msvc) printf 'msvc\n' ;;
        *-linux-*|*-netbsd|*-dragonfly) printf 'stdc++\n' ;;
        *) fail "cannot infer C++ standard library for $(host_target); set CXXSTDLIB" ;;
    esac
}

rocksdb_version() {
    local version="$1"
    [[ "$version" == *+* ]] || fail "sys crate version has no RocksDB version: $version"
    printf '%s\n' "${version#*+}"
}

default_prefix() {
    local crate_version="$1"
    local identity key
    identity="$crate_version|$(host_target)|$FEATURES|$(compiler_command)|$(compiler_version)"
    identity+="|$(cxx_stdlib)|$(deployment_target)|$(sha256_file "$REPO_ROOT/librocksdb-sys/build.rs")"
    identity+="|$(expected_submodule_revision)|$(sha256_file "$REPO_ROOT/Cargo.lock")"
    identity+="|$(sha256_file "$REPO_ROOT/scripts/build-rocksdb-prebuilt.sh")"
    identity+="|$(sha256_tree "$REPO_ROOT/librocksdb-sys/build")"
    identity+="|$(libclang_identity)|$(native_dependency_versions)"
    identity+="|$(sha256_extensions)|$(sha256_source_list)"
    key="$(printf '%s' "$identity" | sha256_stream | cut -c1-16)"
    printf '%s/rust-rocksdb/%s/%s/%s\n' \
        "${XDG_CACHE_HOME:-$HOME/.cache}" "$crate_version" "$(host_target)" "$key"
}

print_config() {
    local prefix="$1"
    local quoted
    [[ -f "$prefix/$MANIFEST_NAME" ]] ||
        fail "prebuilt bundle not found at $prefix"
    quoted="$(python3 -c 'import json, sys; print(json.dumps(sys.argv[1]))' "$prefix")"
    cat <<EOF
[env]
ROCKSDB_PREBUILT_DIR = $quoted
EOF
}

expected_submodule_revision() {
    git -C "$REPO_ROOT" ls-tree HEAD librocksdb-sys/rocksdb | awk '{print $3}'
}

validate_source_checkout() {
    local expected actual dirty header_version crate_rocksdb_version
    expected="$(expected_submodule_revision)"
    actual="$(git -C "$REPO_ROOT/librocksdb-sys/rocksdb" rev-parse HEAD)"
    [[ "$actual" == "$expected" ]] ||
        fail "RocksDB submodule is $actual, expected $expected; restore or update it before building a reusable bundle"
    dirty="$(git -C "$REPO_ROOT/librocksdb-sys/rocksdb" status --porcelain --untracked-files=all)"
    [[ -z "$dirty" ]] ||
        fail "RocksDB submodule has local changes; clean it before building a reusable bundle"
    header_version="$(header_rocksdb_version "$REPO_ROOT/librocksdb-sys/rocksdb/include/rocksdb/version.h")"
    crate_rocksdb_version="$(rocksdb_version "$SYS_CRATE_VERSION")"
    [[ "$header_version" == "$crate_rocksdb_version" ]] ||
        fail "RocksDB headers report $header_version, expected $crate_rocksdb_version"
}

header_rocksdb_version() {
    local header="$1"
    local major minor patch
    major="$(awk '/#define ROCKSDB_MAJOR / {print $3}' "$header")"
    minor="$(awk '/#define ROCKSDB_MINOR / {print $3}' "$header")"
    patch="$(awk '/#define ROCKSDB_PATCH / {print $3}' "$header")"
    printf '%s.%s.%s\n' "$major" "$minor" "$patch"
}

find_single_artifact() {
    local pattern="$1"
    local match
    local -a matches=()
    while IFS= read -r match; do
        matches+=("$match")
    done < <(find "$BUILD_OUTPUT_ROOT" -path "$pattern" -type f)
    ((${#matches[@]} == 1)) ||
        fail "expected one artifact matching $pattern, found ${#matches[@]}"
    printf '%s\n' "${matches[0]}"
}

build_sys_crate() {
    local -a command
    local target normalized_target libclang_path
    target="$(host_target)"
    normalized_target="${target//-/_}"
    libclang_path="$(resolved_libclang_path)"
    case "${CC:-} ${CXX:-}" in
        *-march*|*-mcpu*|*target-cpu*) fail "CC/CXX must not inject CPU-specific flags" ;;
    esac
    command=(cargo build --release --target "$target" -p rust-librocksdb-sys --no-default-features)
    command+=(--features "bindgen-runtime,$FEATURES")
    [[ -z "$JOBS" ]] || command+=(--jobs "$JOBS")
    (
        cd "$REPO_ROOT"
        unset BINDGEN_EXTRA_CLANG_ARGS CFLAGS CXXFLAGS
        unset ROCKSDB_PREBUILT_DIR ROCKSDB_LIB_DIR ROCKSDB_USE_PKG_CONFIG
        unset SNAPPY_COMPILE SNAPPY_LIB_DIR SNAPPY_STATIC
        unset CARGO_BUILD_RUSTFLAGS CARGO_BUILD_TARGET CARGO_ENCODED_RUSTFLAGS
        unset HOST_CC HOST_CXX HOST_CFLAGS HOST_CXXFLAGS SDKROOT
        unset TARGET_CC TARGET_CXX TARGET_CFLAGS TARGET_CXXFLAGS
        unset "CC_$normalized_target" "CXX_$normalized_target"
        unset "CFLAGS_$normalized_target" "CXXFLAGS_$normalized_target"
        export CARGO_TARGET_DIR="$BUILD_TARGET"
        export LIBCLANG_PATH="$libclang_path"
        export ROCKSDB_COMPILE=1
        export ROCKSDB_CXX_STD=c++20
        export RUSTFLAGS=
        if [[ "$(deployment_target)" != "none" ]]; then
            export MACOSX_DEPLOYMENT_TARGET
            MACOSX_DEPLOYMENT_TARGET="$(deployment_target)"
        else
            unset MACOSX_DEPLOYMENT_TARGET
        fi
        env -u "CC_$target" -u "CXX_$target" \
            -u "CFLAGS_$target" -u "CXXFLAGS_$target" "${command[@]}"
    )
}

build_output_file() {
    local crate="$1"
    local -a matches=()
    local match
    while IFS= read -r match; do
        matches+=("$match")
    done < <(find "$BUILD_OUTPUT_ROOT" -path "*/$crate-*/output" -type f)
    ((${#matches[@]} == 1)) ||
        fail "expected one build output for $crate, found ${#matches[@]}"
    printf '%s\n' "${matches[0]}"
}

build_output_value() {
    local crate="$1"
    local key="$2"
    local output
    output="$(build_output_file "$crate")"
    awk -v prefix="cargo:$key=" 'index($0, prefix) == 1 {value=substr($0, length(prefix)+1)} END {print value}' "$output"
}

dependency_headers_hash() {
    local feature="$1"
    local crate="$2"
    local key="$3"
    local suffix="${4:-}"
    if ! feature_enabled "$feature"; then
        printf 'none\n'
        return
    fi
    local include
    include="$(build_output_value "$crate" "$key")"
    [[ -n "$include" ]] || fail "$crate did not emit cargo:$key"
    sha256_tree "$include$suffix"
}

copy_bundle_files() {
    local rocksdb_archive="$1"
    local snappy_archive="$2"
    mkdir -p "$STAGE/lib" "$STAGE/include"
    cp "$rocksdb_archive" "$STAGE/lib/librocksdb.a"
    [[ -z "$snappy_archive" ]] || cp "$snappy_archive" "$STAGE/lib/libsnappy.a"
    cp "$BINDINGS_FILE" "$STAGE/bindings.rs"
    cp -R "$REPO_ROOT/librocksdb-sys/rocksdb/include/rocksdb" "$STAGE/include/"
}

write_manifest() {
    local source_revision="$1"
    local snappy_hash="none"
    [[ ! -f "$STAGE/lib/libsnappy.a" ]] ||
        snappy_hash="$(sha256_file "$STAGE/lib/libsnappy.a")"
    cat >"$STAGE/$MANIFEST_NAME" <<EOF
format=1
crate_version=$SYS_CRATE_VERSION
rocksdb_version=$(rocksdb_version "$SYS_CRATE_VERSION")
source_revision=$source_revision
target=$(host_target)
features=$FEATURES
cxx_std=c++20
cxx_stdlib=$(cxx_stdlib)
target_cpu=baseline
link=static
compiler=$(compiler_command)
compiler_version=$(compiler_version)
libclang_identity=$(libclang_identity)
native_dependency_versions=$(native_dependency_versions)
deployment_target=$(deployment_target)
extensions_sha256=$(sha256_extensions)
build_script_sha256=$(sha256_file "$REPO_ROOT/librocksdb-sys/build.rs")
validator_sha256=$(sha256_tree "$REPO_ROOT/librocksdb-sys/build")
source_list_sha256=$(sha256_source_list)
headers_sha256=$(sha256_tree "$STAGE/include/rocksdb")
bzip2_headers_sha256=$(dependency_headers_hash bzip2 bzip2-sys include)
lz4_headers_sha256=$(dependency_headers_hash lz4 lz4-sys include)
zlib_headers_sha256=$(dependency_headers_hash zlib libz-sys include)
zstd_headers_sha256=$(dependency_headers_hash zstd zstd-sys include)
jemalloc_headers_sha256=$(dependency_headers_hash jemalloc tikv-jemalloc-sys root /include)
rocksdb_sha256=$(sha256_file "$STAGE/lib/librocksdb.a")
snappy_sha256=$snappy_hash
bindings_sha256=$(sha256_file "$STAGE/bindings.rs")
EOF
}

publish_bundle() {
    [[ ! -e "$PREFIX" ]] || fail "bundle already exists at $PREFIX"
    mv "$STAGE" "$PREFIX"
    STAGE=""
}

wrapper_features() {
    tr ',' '\n' <<<"$FEATURES" |
        sed '/^static$/d' |
        paste -sd, -
}

verify_bundle() {
    local prefix="$1"
    local rust_features
    local -a command
    rust_features="$(wrapper_features)"
    command=(cargo test --locked --target "$(host_target)" --test test_db external --no-default-features)
    [[ -z "$rust_features" ]] || command+=(--features "$rust_features")
    (
        cd "$REPO_ROOT"
        unset CARGO_BUILD_TARGET ROCKSDB_COMPILE ROCKSDB_LIB_DIR ROCKSDB_USE_PKG_CONFIG
        ROCKSDB_PREBUILT_DIR="$prefix" CARGO_TARGET_DIR="$BUILD_TARGET/smoke-target" \
            "${command[@]}"
    )
}

acquire_lock() {
    local parent
    parent="$(dirname "$PREFIX")"
    mkdir -p "$parent"
    LOCK="${PREFIX}.lock"
    mkdir "$LOCK" 2>/dev/null || fail "another bundle build holds $LOCK"
    STAGE="$(mktemp -d "$parent/.rust-rocksdb-bundle.XXXXXX")"
}

build_bundle() {
    if [[ -f "$PREFIX/$MANIFEST_NAME" ]]; then
        validate_existing_bundle "$PREFIX"
        print_config "$PREFIX"
        return
    fi
    [[ ! -e "$PREFIX" ]] ||
        fail "path exists but is not a valid prebuilt bundle: $PREFIX"
    validate_source_checkout
    acquire_lock
    BUILD_TARGET="$(mktemp -d "${TMPDIR:-/tmp}/rust-rocksdb-build.XXXXXX")"
    BUILD_OUTPUT_ROOT="$BUILD_TARGET/$(host_target)/release/build"
    build_sys_crate
    local rocksdb_archive snappy_archive=""
    rocksdb_archive="$(find_single_artifact '*/out/librocksdb.a')"
    BINDINGS_FILE="$(find_single_artifact '*/out/bindings.rs')"
    if [[ ",$FEATURES," == *,snappy,* ]]; then
        snappy_archive="$(find_single_artifact '*/out/libsnappy.a')"
    fi
    copy_bundle_files "$rocksdb_archive" "$snappy_archive"
    write_manifest "$(expected_submodule_revision)"
    verify_bundle "$STAGE"
    publish_bundle
    print_config "$PREFIX"
}

cleanup() {
    [[ -z "${BUILD_TARGET:-}" ]] || rm -rf "$BUILD_TARGET"
    [[ -z "${STAGE:-}" ]] || rm -rf "$STAGE"
    [[ -z "${LOCK:-}" ]] || rmdir "$LOCK" 2>/dev/null || true
}

main() {
    parse_args "$@"
    require_tool cargo
    require_tool python3
    require_tool rustc
    FEATURES="$(normalize_features "$FEATURES")"
    validate_features
    SYS_CRATE_VERSION="$(metadata_field rust-librocksdb-sys version)"
    # Used by the sourced self-test helper.
    # shellcheck disable=SC2034
    ROCKSDB_VERSION="$(rocksdb_version "$SYS_CRATE_VERSION")"
    [[ "$(host_target)" != *-windows-* ]] ||
        fail "prebuilt bundle generation supports Linux and macOS only"
    [[ -n "$PREFIX" ]] || PREFIX="$(default_prefix "$SYS_CRATE_VERSION")"
    trap cleanup EXIT
    case "$MODE" in
        build) build_bundle ;;
        print)
            validate_existing_bundle "$PREFIX"
            print_config "$PREFIX"
            ;;
    esac
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi

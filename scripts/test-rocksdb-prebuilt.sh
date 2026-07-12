#!/usr/bin/env bash
set -euo pipefail

readonly TEST_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$TEST_SCRIPT_DIR/build-rocksdb-prebuilt.sh"

write_fake_bundle() {
    local prefix="$1"
    local major minor patch
    mkdir -p "$prefix/lib" "$prefix/include/rocksdb"
    printf 'pub type rocksdb_t = ::std::os::raw::c_void;\n' >"$prefix/bindings.rs"
    printf 'archive\n' >"$prefix/lib/librocksdb.a"
    major="${ROCKSDB_VERSION%%.*}"
    minor="$(cut -d. -f2 <<<"$ROCKSDB_VERSION")"
    patch="${ROCKSDB_VERSION##*.}"
    printf '#define ROCKSDB_MAJOR %s\n#define ROCKSDB_MINOR %s\n#define ROCKSDB_PATCH %s\n' \
        "$major" "$minor" "$patch" >"$prefix/include/rocksdb/version.h"
    printf '#pragma once\n' >"$prefix/include/rocksdb/c.h"
    FEATURES="static"
    STAGE="$prefix"
    write_manifest "$(expected_submodule_revision)"
    STAGE=""
}

run_valid_bundle() {
    local prefix="$1"
    shift
    (
        cd "$REPO_ROOT"
        unset CARGO_BUILD_TARGET ROCKSDB_COMPILE ROCKSDB_LIB_DIR ROCKSDB_USE_PKG_CONFIG
        CARGO_TARGET_DIR="$SELF_TEST_TARGET" ROCKSDB_PREBUILT_DIR="$prefix" \
            cargo "$@" --target "$(host_target)" \
                -p rust-librocksdb-sys --no-default-features --features static
    )
}

expect_check_failure() {
    local prefix="$1"
    local features="$2"
    local pattern="$3"
    local output="$4"
    local -a command
    command=(cargo check --target "$(host_target)" -p rust-librocksdb-sys --no-default-features)
    [[ -z "$features" ]] || command+=(--features "$features")
    if (
        cd "$REPO_ROOT"
        unset CARGO_BUILD_TARGET ROCKSDB_COMPILE ROCKSDB_LIB_DIR ROCKSDB_USE_PKG_CONFIG
        CARGO_TARGET_DIR="$SELF_TEST_TARGET" ROCKSDB_PREBUILT_DIR="$prefix" "${command[@]}"
    ) >"$output" 2>&1; then
        fail "invalid prebuilt bundle unexpectedly passed: $pattern"
    fi
    grep -q "$pattern" "$output" ||
        fail "invalid bundle did not produce the expected error: $pattern"
}

self_test() {
    local manifest original
    write_fake_bundle "$SELF_TEST_ROOT/bundle"
    run_valid_bundle "$SELF_TEST_ROOT/bundle" check
    run_valid_bundle "$SELF_TEST_ROOT/bundle" clippy
    run_valid_bundle "$SELF_TEST_ROOT/bundle" check --release
    if find "$SELF_TEST_TARGET" -name librocksdb.a -print -quit | grep -q .; then
        fail "prebuilt self-test compiled a RocksDB archive inside Cargo target"
    fi
    manifest="$SELF_TEST_ROOT/bundle/$MANIFEST_NAME"
    original="$SELF_TEST_ROOT/original-manifest"
    cp "$manifest" "$original"

    sed -i.bak 's/^target=.*/target=wrong-target/' "$manifest"
    expect_check_failure "$SELF_TEST_ROOT/bundle" "static" \
        'prebuilt RocksDB `target` mismatch' "$SELF_TEST_ROOT/target-mismatch.log"

    cp "$original" "$manifest"
    expect_check_failure "$SELF_TEST_ROOT/bundle" "" \
        'prebuilt RocksDB `features` mismatch' "$SELF_TEST_ROOT/feature-mismatch.log"

    cp "$original" "$manifest"
    printf 'corrupt\n' >>"$SELF_TEST_ROOT/bundle/lib/librocksdb.a"
    expect_check_failure "$SELF_TEST_ROOT/bundle" "static" \
        'prebuilt RocksDB `librocksdb.a` hash mismatch' "$SELF_TEST_ROOT/hash-mismatch.log"
}

main() {
    require_tool cargo
    require_tool python3
    require_tool rustc
    FEATURES="static"
    SYS_CRATE_VERSION="$(metadata_field rust-librocksdb-sys version)"
    ROCKSDB_VERSION="$(rocksdb_version "$SYS_CRATE_VERSION")"
    SELF_TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/rust-rocksdb-self-test.XXXXXX")"
    SELF_TEST_TARGET="$SELF_TEST_ROOT/target"
    trap 'rm -rf "$SELF_TEST_ROOT"' EXIT
    self_test
    printf 'prebuilt bundle self-test passed\n'
}

main "$@"

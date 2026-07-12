#!/usr/bin/env bash
set -euo pipefail

TEST_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly TEST_SCRIPT_DIR
# shellcheck source=scripts/build-rocksdb-prebuilt.sh
source "$TEST_SCRIPT_DIR/build-rocksdb-prebuilt.sh"

libclang_identity() {
    printf 'test-libclang\n'
}

assert_manifest_metadata() {
    local manifest="$1"
    grep -Fqx 'libclang_identity=test-libclang' "$manifest" ||
        fail "fake manifest is missing libclang_identity"
    grep -Fqx 'native_dependency_versions=none' "$manifest" ||
        fail "fake manifest is missing native_dependency_versions"
}

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
    # Global state owned by the sourced producer.
    # shellcheck disable=SC2034
    STAGE=""
    assert_manifest_metadata "$prefix/$MANIFEST_NAME"
}

fresh_bundle() {
    local name="$1"
    CASE_BUNDLE="$SELF_TEST_ROOT/$name"
    cp -R "$BASE_BUNDLE" "$CASE_BUNDLE"
}

set_manifest_field() {
    local prefix="$1"
    local field="$2"
    local value="$3"
    python3 - "$prefix/$MANIFEST_NAME" "$field" "$value" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
field, value = sys.argv[2:]
lines = path.read_text().splitlines()
prefix = f"{field}="
matches = [index for index, line in enumerate(lines) if line.startswith(prefix)]
if len(matches) != 1:
    raise SystemExit(f"expected one {field} field, found {len(matches)}")
lines[matches[0]] = f"{field}={value}"
path.write_text("\n".join(lines) + "\n")
PY
}

remove_manifest_field() {
    local prefix="$1"
    local field="$2"
    python3 - "$prefix/$MANIFEST_NAME" "$field" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
prefix = f"{sys.argv[2]}="
lines = path.read_text().splitlines()
kept = [line for line in lines if not line.startswith(prefix)]
if len(kept) != len(lines) - 1:
    raise SystemExit(f"expected one {sys.argv[2]} field")
path.write_text("\n".join(kept) + "\n")
PY
}

run_bundle_cargo() {
    local prefix="$1"
    local features="$2"
    shift 2
    local -a command
    command=(cargo "$@" --target "$(host_target)")
    command+=(-p rust-librocksdb-sys --no-default-features)
    [[ -z "$features" ]] || command+=(--features "$features")
    (
        cd "$REPO_ROOT"
        unset CARGO_BUILD_TARGET ROCKSDB_COMPILE ROCKSDB_LIB_DIR ROCKSDB_USE_PKG_CONFIG
        CARGO_TARGET_DIR="$SELF_TEST_TARGET" ROCKSDB_PREBUILT_DIR="$prefix" \
            "${command[@]}"
    )
}

expect_check_failure() {
    local prefix="$1"
    local features="$2"
    local pattern="$3"
    local name="$4"
    local output="$SELF_TEST_ROOT/$name.log"
    if run_bundle_cargo "$prefix" "$features" check >"$output" 2>&1; then
        fail "invalid prebuilt bundle unexpectedly passed: $name"
    fi
    if ! grep -Fq -- "$pattern" "$output"; then
        cat "$output" >&2
        fail "invalid bundle did not produce the expected error: $pattern"
    fi
}

expect_producer_failure() {
    local prefix="$1"
    local pattern="$2"
    local output="$SELF_TEST_ROOT/producer-failure.log"
    if "$TEST_SCRIPT_DIR/build-rocksdb-prebuilt.sh" \
        --prefix "$prefix" --features static >"$output" 2>&1; then
        fail "producer unexpectedly accepted stale prefix"
    fi
    if ! grep -Fq -- "$pattern" "$output"; then
        cat "$output" >&2
        fail "producer did not produce the expected error: $pattern"
    fi
}

test_manifest_structure() {
    fresh_bundle missing-field
    remove_manifest_field "$CASE_BUNDLE" target
    expect_check_failure "$CASE_BUNDLE" static "is missing \`target\`" missing-field

    fresh_bundle unknown-field
    printf 'unknown_field=value\n' >>"$CASE_BUNDLE/$MANIFEST_NAME"
    expect_check_failure "$CASE_BUNDLE" static \
        "unknown prebuilt RocksDB manifest field \`unknown_field\`" unknown-field

    fresh_bundle duplicate-field
    printf 'target=%s\n' "$(host_target)" >>"$CASE_BUNDLE/$MANIFEST_NAME"
    expect_check_failure "$CASE_BUNDLE" static \
        "duplicate prebuilt RocksDB manifest field \`target\`" duplicate-field

    fresh_bundle corrupt-manifest
    printf 'not-a-manifest-field\n' >>"$CASE_BUNDLE/$MANIFEST_NAME"
    expect_check_failure "$CASE_BUNDLE" static \
        "expected name=value, got \`not-a-manifest-field\`" corrupt-manifest
}

test_manifest_mismatches() {
    local field
    for field in crate_version rocksdb_version source_revision target cxx_std \
        cxx_stdlib deployment_target target_cpu link; do
        fresh_bundle "$field-mismatch"
        set_manifest_field "$CASE_BUNDLE" "$field" wrong-value
        expect_check_failure "$CASE_BUNDLE" static \
            "prebuilt RocksDB \`$field\` mismatch" "$field-mismatch"
    done

    fresh_bundle features-mismatch
    set_manifest_field "$CASE_BUNDLE" features ""
    expect_check_failure "$CASE_BUNDLE" static \
        "prebuilt RocksDB \`features\` mismatch" features-mismatch

    fresh_bundle unsupported-feature
    set_manifest_field "$CASE_BUNDLE" features "lto,static"
    expect_check_failure "$CASE_BUNDLE" "lto,static" \
        "prebuilt RocksDB does not support feature \`lto\`" unsupported-feature
}

test_bundle_corruption() {
    fresh_bundle bindings-corruption
    printf 'corrupt\n' >>"$CASE_BUNDLE/bindings.rs"
    expect_check_failure "$CASE_BUNDLE" static \
        "prebuilt RocksDB \`bindings.rs\` hash mismatch" bindings-corruption

    fresh_bundle header-corruption
    printf 'corrupt\n' >>"$CASE_BUNDLE/include/rocksdb/c.h"
    expect_check_failure "$CASE_BUNDLE" static \
        "prebuilt RocksDB \`RocksDB headers\` hash mismatch" header-corruption

    fresh_bundle manifest-archive-hash
    set_manifest_field "$CASE_BUNDLE" rocksdb_sha256 wrong-hash
    expect_check_failure "$CASE_BUNDLE" static \
        "prebuilt RocksDB \`librocksdb.a\` hash mismatch" manifest-archive-hash

    fresh_bundle manifest-bindings-hash
    set_manifest_field "$CASE_BUNDLE" bindings_sha256 wrong-hash
    expect_check_failure "$CASE_BUNDLE" static \
        "prebuilt RocksDB \`bindings.rs\` hash mismatch" manifest-bindings-hash

    fresh_bundle manifest-headers-hash
    set_manifest_field "$CASE_BUNDLE" headers_sha256 wrong-hash
    expect_check_failure "$CASE_BUNDLE" static \
        "prebuilt RocksDB \`RocksDB headers\` hash mismatch" manifest-headers-hash
}

test_crate_hash_mismatches() {
    fresh_bundle validator-hash
    set_manifest_field "$CASE_BUNDLE" validator_sha256 wrong-hash
    expect_check_failure "$CASE_BUNDLE" static \
        "prebuilt RocksDB \`prebuilt validator\` hash mismatch" validator-hash

    fresh_bundle source-list-hash
    set_manifest_field "$CASE_BUNDLE" source_list_sha256 wrong-hash
    expect_check_failure "$CASE_BUNDLE" static \
        'prebuilt RocksDB source-list hash mismatch' source-list-hash

    fresh_bundle extensions-hash
    set_manifest_field "$CASE_BUNDLE" extensions_sha256 wrong-hash
    expect_check_failure "$CASE_BUNDLE" static \
        'prebuilt RocksDB local C-API extension hash mismatch' extensions-hash
}

test_cached_corruption() {
    fresh_bundle cached-corruption
    run_bundle_cargo "$CASE_BUNDLE" static check
    run_bundle_cargo "$CASE_BUNDLE" static check
    printf 'corrupt\n' >>"$CASE_BUNDLE/lib/librocksdb.a"
    expect_check_failure "$CASE_BUNDLE" static \
        "prebuilt RocksDB \`librocksdb.a\` hash mismatch" cached-corruption
}

test_stale_prefix() {
    fresh_bundle stale-prefix
    printf 'corrupt\n' >>"$CASE_BUNDLE/lib/librocksdb.a"
    expect_producer_failure "$CASE_BUNDLE" \
        'existing prebuilt bundle failed validation'
}

test_filesystem_trust() {
    fresh_bundle symlink-file
    mv "$CASE_BUNDLE/bindings.rs" "$CASE_BUNDLE/bindings.real"
    ln -s bindings.real "$CASE_BUNDLE/bindings.rs"
    expect_check_failure "$CASE_BUNDLE" static \
        'is a symbolic link' symlink-file

    CASE_BUNDLE="$SELF_TEST_ROOT/symlink-root"
    ln -s "$BASE_BUNDLE" "$CASE_BUNDLE"
    expect_check_failure "$CASE_BUNDLE" static \
        'is a symbolic link' symlink-root

    fresh_bundle writable-file
    chmod 0660 "$CASE_BUNDLE/bindings.rs"
    expect_check_failure "$CASE_BUNDLE" static \
        'is group or world writable' writable-file

    fresh_bundle hard-link
    ln "$CASE_BUNDLE/bindings.rs" "$CASE_BUNDLE/bindings.alias"
    expect_check_failure "$CASE_BUNDLE" static \
        'hard links, expected 1' hard-link

    fresh_bundle special-file
    mkfifo "$CASE_BUNDLE/pipe"
    expect_check_failure "$CASE_BUNDLE" static \
        'is not a regular file or directory' special-file

    mkdir "$SELF_TEST_ROOT/writable-parent"
    chmod 0777 "$SELF_TEST_ROOT/writable-parent"
    CASE_BUNDLE="$SELF_TEST_ROOT/writable-parent/bundle"
    cp -R "$BASE_BUNDLE" "$CASE_BUNDLE"
    expect_check_failure "$CASE_BUNDLE" static \
        'bundle ancestor `' writable-parent
    chmod 0700 "$SELF_TEST_ROOT/writable-parent"
}

test_provenance_fields() {
    local field
    for field in compiler compiler_version libclang_identity \
        native_dependency_versions; do
        fresh_bundle "$field-empty"
        set_manifest_field "$CASE_BUNDLE" "$field" ""
        expect_check_failure "$CASE_BUNDLE" static \
            "manifest field \`$field\` must not be empty" "$field-empty"
    done
}

test_feature_cache_key() {
    local first second static_only
    FEATURES="$(normalize_features "zstd,static,zstd")"
    [[ "$FEATURES" == "static,zstd" ]] ||
        fail "feature normalization produced: $FEATURES"
    first="$(default_prefix "$SYS_CRATE_VERSION")"
    FEATURES="$(normalize_features "static,zstd")"
    second="$(default_prefix "$SYS_CRATE_VERSION")"
    [[ "$first" == "$second" ]] ||
        fail "feature order changed the default cache key"
    FEATURES="static"
    static_only="$(default_prefix "$SYS_CRATE_VERSION")"
    [[ "$first" != "$static_only" ]] ||
        fail "different feature sets shared a default cache key"
}

self_test() {
    BASE_BUNDLE="$SELF_TEST_ROOT/baseline"
    write_fake_bundle "$BASE_BUNDLE"
    run_bundle_cargo "$BASE_BUNDLE" static check
    run_bundle_cargo "$BASE_BUNDLE" static clippy
    run_bundle_cargo "$BASE_BUNDLE" static check --release
    test_manifest_structure
    test_manifest_mismatches
    test_bundle_corruption
    test_crate_hash_mismatches
    test_cached_corruption
    test_stale_prefix
    test_filesystem_trust
    test_provenance_fields
    test_feature_cache_key
    if find "$SELF_TEST_TARGET" -path '*/out/librocksdb.a' -print -quit |
        grep -q .; then
        fail "prebuilt self-test compiled a vendored RocksDB archive"
    fi
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

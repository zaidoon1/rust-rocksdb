#!/usr/bin/env bash

libclang_identity() {
    local path
    path="$(resolved_libclang_path)"
    libclang_path_identity "$path"
}

resolved_libclang_path() {
    if [[ -n "${LIBCLANG_PATH:-}" ]]; then
        [[ -d "$LIBCLANG_PATH" ]] ||
            fail "LIBCLANG_PATH is not a directory: $LIBCLANG_PATH"
        printf '%s\n' "$LIBCLANG_PATH"
        return
    fi
    if [[ -n "${LLVM_CONFIG_PATH:-}" ]]; then
        llvm_config_libdir "$LLVM_CONFIG_PATH"
        return
    fi
    if command -v llvm-config >/dev/null 2>&1; then
        llvm_config_libdir "$(command -v llvm-config)"
        return
    fi
    fail "cannot identify libclang; set LIBCLANG_PATH or LLVM_CONFIG_PATH"
}

llvm_config_libdir() {
    local llvm_config="$1"
    [[ -x "$llvm_config" ]] || fail "LLVM_CONFIG_PATH is not executable: $llvm_config"
    local libdir
    libdir="$("$llvm_config" --libdir)"
    [[ -d "$libdir" ]] || fail "llvm-config returned an invalid libdir: $libdir"
    printf '%s\n' "$libdir"
}

libclang_path_identity() {
    local path="$1"
    [[ -d "$path" ]] || fail "LIBCLANG_PATH is not a directory: $path"
    python3 - "$path" <<'PY'
import hashlib
from pathlib import Path
import sys

root = Path(sys.argv[1])
files = sorted(path for path in root.glob("libclang*") if path.is_file())
if not files:
    raise SystemExit(f"no libclang library found under {root}")
digest = hashlib.sha256()
for path in files:
    digest.update(path.name.encode())
    digest.update(b"\0")
    with path.open("rb") as source:
        while True:
            chunk = source.read(64 * 1024)
            if not chunk:
                break
            digest.update(chunk)
print(f"path:{root}:{digest.hexdigest()}")
PY
}

native_dependency_versions() {
    local versions
    versions="$(cargo metadata \
        --manifest-path "$REPO_ROOT/librocksdb-sys/Cargo.toml" \
        --format-version 1 \
        --locked \
        --no-default-features \
        --features "$FEATURES" |
        python3 -c '
import json, sys
features = set(sys.argv[1].split(","))
packages = {package["name"]: package["version"] for package in json.load(sys.stdin)["packages"]}
mapping = {
    "bzip2": "bzip2-sys",
    "jemalloc": "tikv-jemalloc-sys",
    "lz4": "lz4-sys",
    "zlib": "libz-sys",
    "zstd": "zstd-sys",
}
values = [
    f"{package}={packages[package]}"
    for feature, package in mapping.items()
    if feature in features
]
print(",".join(sorted(values)))
' "$FEATURES")"
    printf '%s\n' "${versions:-none}"
}

validate_existing_bundle() {
    local prefix="$1"
    local target_dir
    local -a command
    target_dir="$(mktemp -d "${TMPDIR:-/tmp}/rust-rocksdb-validate.XXXXXX")"
    command=(cargo check --locked --target "$(host_target)")
    command+=(-p rust-librocksdb-sys --no-default-features)
    command+=(--features "bindgen-runtime,$FEATURES")
    if ! (
        cd "$REPO_ROOT"
        unset CARGO_BUILD_TARGET ROCKSDB_COMPILE ROCKSDB_LIB_DIR ROCKSDB_USE_PKG_CONFIG
        CXX="$(command -v false)" LIBCLANG_PATH="$target_dir/missing-libclang" \
            CARGO_TARGET_DIR="$target_dir/target" ROCKSDB_PREBUILT_DIR="$prefix" \
            "${command[@]}"
    ); then
        rm -rf "$target_dir"
        fail "existing prebuilt bundle failed validation: $prefix"
    fi
    rm -rf "$target_dir"
}

#!/usr/bin/env bash
set -euo pipefail

readonly MANIFEST_NAME="rust-rocksdb-prebuilt.env"
readonly MIN_ROCKSDB_BYTES=$((1024 * 1024))
readonly MAX_ROCKSDB_BYTES=$((256 * 1024 * 1024))
readonly MAX_BUNDLE_BYTES=$((384 * 1024 * 1024))

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

require_file() {
    [[ -f "$1" ]] || fail "required file is missing: $1"
}

verify_bundle_files() {
    python3 - "$1" "$MIN_ROCKSDB_BYTES" "$MAX_ROCKSDB_BYTES" \
        "$MAX_BUNDLE_BYTES" <<'PY'
from pathlib import Path
import stat
import sys

root = Path(sys.argv[1])
minimum, maximum, bundle_maximum = map(int, sys.argv[2:])
archive = root / "lib" / "librocksdb.a"
manifest = root / "rust-rocksdb-prebuilt.env"
bindings = root / "bindings.rs"

sizes = {
    "librocksdb.a": archive.stat().st_size,
    "manifest": manifest.stat().st_size,
    "bindings.rs": bindings.stat().st_size,
}
if not minimum <= sizes["librocksdb.a"] <= maximum:
    raise SystemExit(f"librocksdb.a has unexpected size: {sizes['librocksdb.a']}")
if sizes["manifest"] > 64 * 1024:
    raise SystemExit(f"manifest is unexpectedly large: {sizes['manifest']}")
if sizes["bindings.rs"] > 64 * 1024 * 1024:
    raise SystemExit(f"bindings.rs is unexpectedly large: {sizes['bindings.rs']}")

total = 0
for path in [root, *root.rglob("*")]:
    if path.is_symlink():
        raise SystemExit(f"bundle contains a symlink: {path}")
    mode = stat.S_IMODE(path.stat().st_mode)
    if mode & 0o077:
        raise SystemExit(f"bundle path is accessible outside its owner: {path} ({mode:o})")
    if path.is_dir() and mode & 0o700 != 0o700:
        raise SystemExit(f"bundle directory lacks owner rwx permissions: {path} ({mode:o})")
    if path.is_file():
        total += path.stat().st_size
        if mode & 0o400 == 0:
            raise SystemExit(f"bundle file is not owner-readable: {path} ({mode:o})")
if total > bundle_maximum:
    raise SystemExit(f"bundle is unexpectedly large: {total}")

print(f"librocksdb.a bytes={sizes['librocksdb.a']}")
print(f"bundle bytes={total}")
PY
}

verify_symbols() {
    local archive="$1"
    local symbols
    symbols="$(mktemp "${TMPDIR:-/tmp}/rocksdb-symbols.XXXXXX")"
    nm -g "$archive" >"$symbols"
    if ! grep -Eq '[[:space:]]_?rocksdb_open$' "$symbols"; then
        rm -f "$symbols"
        fail "librocksdb.a does not export rocksdb_open"
    fi
    if ! grep -Eq '[[:space:]]_?rust_rocksdb_status_get_error$' "$symbols"; then
        rm -f "$symbols"
        fail "librocksdb.a does not export rust_rocksdb_status_get_error"
    fi
    if ! grep -Eq '[[:space:]]_?rocksdb_writebatch_iterate_ld$' "$symbols"; then
        rm -f "$symbols"
        fail "librocksdb.a does not export rocksdb_writebatch_iterate_ld"
    fi
    rm -f "$symbols"
}

verify_archive_members() {
    local archive="$1"
    local members
    members="$(mktemp "${TMPDIR:-/tmp}/rocksdb-members.XXXXXX")"
    ar t "$archive" | LC_ALL=C sort >"$members"
    [[ -s "$members" ]] || {
        rm -f "$members"
        fail "librocksdb.a has no archive members"
    }
    if uniq -d "$members" | grep -q .; then
        rm -f "$members"
        fail "librocksdb.a contains duplicate member names"
    fi
    rm -f "$members"
}

verify_no_debug_sections() {
    local archive="$1"
    local sections
    sections="$(mktemp "${TMPDIR:-/tmp}/rocksdb-sections.XXXXXX")"
    if command -v llvm-objdump >/dev/null 2>&1; then
        llvm-objdump --section-headers "$archive" >"$sections"
    elif command -v objdump >/dev/null 2>&1; then
        objdump -h "$archive" >"$sections"
    else
        rm -f "$sections"
        fail "neither llvm-objdump nor objdump is available"
    fi
    if grep -Eq '(\.debug_|__debug_|__DWARF)' "$sections"; then
        rm -f "$sections"
        fail "librocksdb.a contains debug sections"
    fi
    rm -f "$sections"
}

verify_bundle() {
    local bundle="$1"
    local features
    require_file "$bundle/$MANIFEST_NAME"
    require_file "$bundle/bindings.rs"
    require_file "$bundle/include/rocksdb/c.h"
    require_file "$bundle/include/rocksdb/version.h"
    require_file "$bundle/lib/librocksdb.a"
    features="$(awk -F= '$1 == "features" {print substr($0, length($1) + 2)}' \
        "$bundle/$MANIFEST_NAME")"
    case ",$features," in
        *,snappy,*) require_file "$bundle/lib/libsnappy.a" ;;
    esac
    verify_bundle_files "$bundle"
    verify_symbols "$bundle/lib/librocksdb.a"
    verify_archive_members "$bundle/lib/librocksdb.a"
    verify_no_debug_sections "$bundle/lib/librocksdb.a"
}

bundle_digest() {
    python3 - "$1" <<'PY'
from pathlib import Path
import hashlib
import sys

root = Path(sys.argv[1])
digest = hashlib.sha256()
for path in sorted(path for path in root.rglob("*") if path.is_file()):
    relative = path.relative_to(root).as_posix()
    digest.update(relative.encode())
    digest.update(b"\0")
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
print(digest.hexdigest())
PY
}

write_consumer() {
    local root="$1"
    local repo_path
    repo_path="$(python3 -c 'import json, sys; print(json.dumps(sys.argv[1]))' \
        "$GITHUB_WORKSPACE")"
    mkdir -p "$root/src"
    cat >"$root/Cargo.toml" <<EOF
[package]
name = "rocksdb-prebuilt-consumer"
version = "0.0.0"
edition = "2024"

[dependencies]
rocksdb = { package = "rust-rocksdb", path = $repo_path }
EOF
    cat >"$root/src/lib.rs" <<'EOF'
#[cfg(test)]
mod tests {
    use rocksdb::{Options, DB};

    #[test]
    fn opens_and_reads_a_database() {
        let path = std::env::temp_dir().join(format!(
            "rust-rocksdb-prebuilt-consumer-{}",
            std::process::id()
        ));
        let db = DB::open_default(&path).expect("open RocksDB");
        db.put(b"key", b"value").expect("write value");
        let value = db.get(b"key").expect("read value");
        assert_eq!(value.as_deref(), Some(b"value".as_slice()));
        drop(db);
        DB::destroy(&Options::default(), &path).expect("destroy RocksDB");
    }
}
EOF
}

assert_no_native_archive() {
    local root="$1"
    local archive
    while IFS= read -r archive; do
        case "$archive" in
            */out/prebuilt/lib/librocksdb.a|*/out/prebuilt/lib/libsnappy.a) ;;
            *) fail "consumer target contains a compiler-produced archive: $archive" ;;
        esac
    done < <(
        find "$root" -path '*/target-*/*' -type f \
            \( -name librocksdb.a -o -name libsnappy.a \)
    )
}

assert_materialized_bundle() {
    local target="$1"
    local rocksdb_copy
    rocksdb_copy="$(
        find "$target" -path '*/out/prebuilt/lib/librocksdb.a' -type f -print -quit
    )"
    [[ -n "$rocksdb_copy" ]] ||
        fail "consumer target is missing the materialized RocksDB archive"
    cmp "${ROCKSDB_PREBUILT_DIR:?}/lib/librocksdb.a" "$rocksdb_copy"
}

run_consumer_cargo() {
    local root="$1"
    local target_name="$2"
    local subcommand="$3"
    shift 3
    local target="$root/target-$target_name"
    [[ ! -e "$target" ]] || fail "target directory is not fresh: $target"
    cargo "$subcommand" --manifest-path "$root/Cargo.toml" \
        --target-dir "$target" "$@"
    assert_no_native_archive "$root"
    assert_materialized_bundle "$target"
}

consume_bundle() {
    local bundle="$1"
    local consumer="${RUNNER_TEMP:?}/rocksdb-prebuilt-consumer"
    local before after release_target
    [[ ! -e "$consumer" ]] || fail "consumer directory already exists: $consumer"
    [[ ! -e "$GITHUB_WORKSPACE/librocksdb-sys/rocksdb" ]] ||
        fail "RocksDB source must be moved aside before consumer checks"
    write_consumer "$consumer"
    cargo generate-lockfile --manifest-path "$consumer/Cargo.toml"
    before="$(bundle_digest "$bundle")"
    run_consumer_cargo "$consumer" check check --locked
    run_consumer_cargo "$consumer" clippy clippy --locked --all-targets -- -D warnings
    run_consumer_cargo "$consumer" test test --locked
    run_consumer_cargo "$consumer" release build --locked --release
    release_target="$consumer/target-release"
    find "$release_target" -type f -print -quit | grep -q . ||
        fail "release target is empty before cargo clean"
    cargo clean --manifest-path "$consumer/Cargo.toml" --target-dir "$release_target"
    if [[ -d "$release_target" ]] &&
        find "$release_target" -mindepth 1 -print -quit | grep -q .; then
        fail "cargo clean left files in $release_target"
    fi
    after="$(bundle_digest "$bundle")"
    [[ "$after" == "$before" ]] || fail "cargo clean changed the prebuilt bundle"
    run_consumer_cargo "$consumer" after-clean check --locked
    [[ ! -e "${PREBUILT_CXX_MARKER:?}" ]] ||
        fail "the prebuilt consumer invoked CXX"
}

compare_bundles() {
    local first="$1"
    local second="$2"
    verify_bundle "$first"
    verify_bundle "$second"
    diff -qr "$first" "$second"
    [[ "$(bundle_digest "$first")" == "$(bundle_digest "$second")" ]] ||
        fail "bundle digests differ"
}

case "${1:-}" in
    verify)
        [[ $# == 2 ]] || fail "usage: $0 verify BUNDLE"
        verify_bundle "$2"
        ;;
    consume)
        [[ $# == 2 ]] || fail "usage: $0 consume BUNDLE"
        consume_bundle "$2"
        ;;
    compare)
        [[ $# == 3 ]] || fail "usage: $0 compare FIRST SECOND"
        compare_bundles "$2" "$3"
        ;;
    *)
        fail "usage: $0 {verify BUNDLE|consume BUNDLE|compare FIRST SECOND}"
        ;;
esac

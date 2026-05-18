#!/usr/bin/env bash
# Build folly + all its transitive dependencies for the `coroutines` cargo
# feature.
#
# We do NOT use RocksDB's `make build_folly` target, because that runs
# `getdeps.py` without `--scratch-path`. With no `--scratch-path`, getdeps
# falls back to a `/tmp/fbcode_builder_getdeps-<munged-cwd>` directory
# (see folly's `build/fbcode_builder/getdeps/buildopts.py:setup_build_options`),
# which is unstable across machines and impossible to cache cleanly in CI.
# Instead, we replicate the few things `make build_folly` does (clone + pin
# folly, apply two upstream patches, run getdeps, patchelf libglog) and pass
# `--scratch-path` so the install lands at a predictable location.
#
# Usage:
#   ./scripts/build_folly.sh                       # builds to default scratch dir
#   ROCKSDB_FOLLY_SCRATCH_DIR=/path scripts/...    # explicit scratch dir
#
# The build is slow (~15-30 minutes on a clean machine, longer on CI). It
# downloads several hundred MB of source archives. On a warm cache the script
# is a near no-op.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ROCKSDB_DIR="$REPO_ROOT/librocksdb-sys/rocksdb"
SCRATCH_DIR="${ROCKSDB_FOLLY_SCRATCH_DIR:-$REPO_ROOT/librocksdb-sys/folly-build}"

if [ ! -f "$ROCKSDB_DIR/folly.mk" ]; then
    echo "Error: $ROCKSDB_DIR does not look like a RocksDB checkout." >&2
    echo "Did you clone with --recursive? Try:" >&2
    echo "    git submodule update --init --recursive" >&2
    exit 1
fi

case "$(uname -s)" in
    Linux*) ;;
    *)
        echo "Error: the coroutines feature is only supported on Linux." >&2
        echo "Folly's build (getdeps.py) does not reliably support" >&2
        echo "$(uname -s) and RocksDB's coroutine code path is Linux-only" >&2
        echo "(needs io_uring)." >&2
        exit 1
        ;;
esac

for tool in git python3 patchelf perl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Error: required tool '$tool' is not on PATH." >&2
        exit 1
    fi
done

# Extract the pinned folly commit from RocksDB's folly.mk
FOLLY_COMMIT_HASH="$(grep -E '^FOLLY_COMMIT_HASH = ' "$ROCKSDB_DIR/folly.mk" \
                       | sed -E 's/^FOLLY_COMMIT_HASH = //')"
if [ -z "$FOLLY_COMMIT_HASH" ]; then
    echo "Error: could not parse FOLLY_COMMIT_HASH from folly.mk." >&2
    exit 1
fi

FOLLY_DIR="$ROCKSDB_DIR/third-party/folly"
mkdir -p "$ROCKSDB_DIR/third-party"
mkdir -p "$SCRATCH_DIR"

echo ">>> Cloning folly @ $FOLLY_COMMIT_HASH..."
if [ -d "$FOLLY_DIR/.git" ]; then
    (cd "$FOLLY_DIR" && git fetch --quiet origin)
else
    git clone --quiet https://github.com/facebook/folly.git "$FOLLY_DIR"
fi
(cd "$FOLLY_DIR" && git reset --hard --quiet "$FOLLY_COMMIT_HASH")

echo ">>> Applying upstream patches..."
# These match the two `perl -pi -e` invocations in RocksDB's folly.mk
# `checkout_folly` target; the upstream folly commit needs them to compile
# cleanly against a modern toolchain. They are idempotent across re-runs.
perl -pi -e 's/(#include <atomic>)/$1\n#include <cstring>/ unless /#include <cstring>/' \
    "$FOLLY_DIR/folly/lang/Exception.h"
perl -pi -e 's/: environ/: (const char**)(environ)/ unless /\(const char\*\*\)\(environ\)/' \
    "$FOLLY_DIR/folly/Subprocess.cpp"

echo ">>> Building folly + dependencies into $SCRATCH_DIR..."
echo "    (allow 15-30 minutes on a cold cache)"
cd "$FOLLY_DIR"
GETDEPS_USE_WGET=1 \
CXXFLAGS=" -DHAVE_CXX11_ATOMIC " \
python3 build/fbcode_builder/getdeps.py \
    --scratch-path "$SCRATCH_DIR" \
    build --no-tests

# RocksDB's folly.mk patchelfs libglog.so to embed an rpath pointing at
# gflags, because folly's getdeps build links libglog -> libgflags but
# doesn't bake the lookup path in. Without this, ld.so cannot resolve
# libgflags when loading libglog at runtime, even if the user's binary has
# an rpath to libglog.
INSTALLED_DIR="$SCRATCH_DIR/installed"
GFLAGS_DIR="$(ls -d "$INSTALLED_DIR/gflags-"* 2>/dev/null | head -1)"
GLOG_DIR="$(ls -d "$INSTALLED_DIR/glog-"* 2>/dev/null | head -1)"
if [ -n "$GLOG_DIR" ] && [ -n "$GFLAGS_DIR" ]; then
    GLOG_SO="$(ls "$GLOG_DIR"/lib*/libglog.so.*.*.* 2>/dev/null | head -1 || true)"
    if [ -n "$GLOG_SO" ]; then
        echo ">>> Patching $GLOG_SO to find libgflags via rpath..."
        patchelf --add-rpath "$GFLAGS_DIR/lib" "$GLOG_SO"
    else
        echo "Warning: could not locate libglog shared object to patchelf." >&2
    fi
fi

cat <<EOF

==============================================================
Folly and its dependencies are installed under:
    $INSTALLED_DIR

To build rust-rocksdb with the coroutines feature:
    export ROCKSDB_FOLLY_INSTALL_PATH="$INSTALLED_DIR"
    cargo build --release --features coroutines,io-uring
==============================================================
EOF

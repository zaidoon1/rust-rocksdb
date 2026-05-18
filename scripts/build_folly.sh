#!/usr/bin/env bash
# Build folly + all its transitive dependencies for the `coroutines` cargo
# feature.
#
# This wraps RocksDB's own `make build_folly` target, which invokes folly's
# getdeps.py to build folly and its dependency chain (boost, fmt, glog,
# gflags, double-conversion, libevent, libsodium, xz) at the exact commit
# pinned by `librocksdb-sys/rocksdb/folly.mk:FOLLY_COMMIT_HASH`.
#
# The build is slow (15-30 minutes on a clean machine, longer on CI). It
# downloads several hundred MB of source archives. Results live under
# `librocksdb-sys/rocksdb/third-party/folly/build/fbcode_builder/installed/`
# and are reusable across rust-rocksdb builds.
#
# On success, prints the path to set `ROCKSDB_FOLLY_INSTALL_PATH` to, then
# subsequent `cargo build --features coroutines` will find folly.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ROCKSDB_DIR="$REPO_ROOT/librocksdb-sys/rocksdb"

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

cd "$ROCKSDB_DIR"

echo ">>> Cloning and pinning folly..."
make checkout_folly

echo ">>> Building folly + dependencies (this may take 15-30 minutes)..."
make build_folly

# getdeps.py's `show-inst-dir` prints folly's own install dir, e.g.
# `.../installed/folly-<hash>`. Its sibling directories (boost-*, fmt-*, etc)
# hold the rest of folly's deps. The install root is one level up.
FOLLY_INST_PATH="$(cd third-party/folly \
                    && python3 build/fbcode_builder/getdeps.py show-inst-dir)"
INSTALL_ROOT="$(dirname "$FOLLY_INST_PATH")"

cat <<EOF

==============================================================
Folly and its dependencies are installed under:
    $INSTALL_ROOT

To build rust-rocksdb with the coroutines feature:
    export ROCKSDB_FOLLY_INSTALL_PATH="$INSTALL_ROOT"
    cargo build --release --features coroutines,io-uring
==============================================================
EOF

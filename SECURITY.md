# Security Policy

## Reporting a vulnerability

Report memory-safety and security issues privately through GitHub's
[private vulnerability reporting](https://github.com/zaidoon1/rust-rocksdb/security/advisories/new).
That keeps the report out of public view until a fixed version is published.

Do not open a public issue for anything that lets a caller of the safe API
trigger memory unsafety.

Please include:

- the crate version, RocksDB version (the `+` suffix on `rust-librocksdb-sys`),
  target triple, and the feature set you built with
- a minimal reproducer, ideally a test that fails under
  `RUSTFLAGS="-Zsanitizer=address"` or Valgrind, since that is what CI runs
- whether the problem is in this crate's wrapper code or in upstream RocksDB

Expect an initial reply within a week. If you get nothing, ping the maintainer
on the issue tracker without describing the vulnerability.

## What counts

This crate is a safe wrapper over RocksDB's C API. The security boundary is the
safe Rust API: if code that contains no `unsafe` can cause undefined behaviour,
that is a bug in this crate.

In scope:

- use-after-free, double-free, or heap corruption reachable from safe API calls,
  including buffers freed with the wrong allocator
- returned types whose lifetimes let them outlive the object they borrow from
  (iterators, snapshots, pinned slices, column family handles)
- data races from a `Send`/`Sync` impl on a type that is not actually thread safe
- panics unwinding from a Rust callback into RocksDB's C++ frames
- wrong enum discriminants sent across the FFI, which make RocksDB read or write
  a different field than the caller asked for
- reading uninitialised memory, or treating a null or error return as valid

Out of scope:

- vulnerabilities in upstream RocksDB itself. Report those to
  [facebook/rocksdb](https://github.com/facebook/rocksdb/security). If upstream
  fixes one, open a normal issue here asking for a submodule bump.
- misuse of the `raw-ptr` feature, whose entire purpose is to hand out raw
  pointers
- undefined behaviour reachable only by writing your own `unsafe` block

## Supported versions

Only the latest published minor version gets security fixes. There are no
backports to older lines.

Fixes ship as a normal release with the problem described in `CHANGELOG.md`. If
the fix requires a source-breaking signature change, which past lifetime fixes
have, it ships in a minor version bump rather than being held back.

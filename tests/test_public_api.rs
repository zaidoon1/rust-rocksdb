// Regression test for types that were defined as `pub` in a private
// module but were missing from the crate-root re-export surface, even
// though they appear in the return type of a `pub` function downstream
// users call. Without these re-exports, callers can't name the type to
// store it, pass it through their own functions, or destructure it
// explicitly (see zaidoon1/rust-rocksdb#224).
//
// The test body is empty — the imports themselves are the test. A
// future accidental un-export of either type will fail the build of
// this test rather than only surfacing in a downstream user report.
//
// We deliberately do NOT include every `pub` type from every private
// module here: only types that genuinely block real user code patterns
// when missing. Types that users can work around with inference (for
// loops, pattern matching) or by spelling out the underlying type
// (`(Box<[u8]>, Box<[u8]>)` instead of an alias, `fn(&[u8]) -> &[u8]`
// instead of an alias) are intentionally NOT re-exported, to keep the
// public API surface small.

#[allow(unused_imports)]
use rust_rocksdb::{
    // Return type of `DB::get_column_family_metadata{,_cf}`.
    CSlice,
    // Returned wrapped in `(bool, Option<CSlice>)` by the
    // `key_may_exist_*_pinned_value` helpers. Users who want to hold
    // onto the pinned value past the immediate call site need the name.
    ColumnFamilyMetaData,
    // Returned by batch-owned pinned MultiGet APIs.
    DBPinnableBatch,
    DBPinnableBatchIter,
    // Returned by `Snapshot::read_options{,_opt}`.
    SnapshotReadOptions,
};

#[test]
fn newly_exported_types_resolve() {
    // The use-block above is the real test; this assertion is just to
    // make the test visible in `cargo test` output so a regression is
    // obvious in CI logs.
    let _ = std::any::type_name::<ColumnFamilyMetaData>();
    let _ = std::any::type_name::<CSlice>();
    let _ = std::any::type_name::<DBPinnableBatch<'static>>();
    let _ = std::any::type_name::<DBPinnableBatchIter<'static, 'static>>();
    let _ = std::any::type_name::<SnapshotReadOptions<'static, 'static>>();
}

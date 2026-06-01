/*
 * Local additions to the RocksDB C API.
 *
 * RocksDB's upstream C wrapper (`include/rocksdb/c.h`, `db/c.cc`) is
 * maintained reactively: new C++ options land in the C++ headers first, and
 * a matching C wrapper is added only when someone requests it. This crate
 * exposes the C++ feature surface through that C API, so any feature without
 * a C wrapper is unreachable from Rust.
 *
 * This header declares C wrappers for C++ options that don't have one yet
 * upstream. The matching definitions live in `c_api_extensions.cc`. Both
 * are compiled and linked alongside the vendored RocksDB sources (or
 * alongside a system-installed librocksdb, on the System backend); the
 * submodule is NEVER modified. Bindgen reads this header as its primary
 * input, so the new symbols flow into `rust-librocksdb-sys`'s generated
 * bindings.rs without any special-casing.
 *
 * Each declaration here mirrors an upstream PR against
 * facebook/rocksdb (see this file's comments and the project CHANGELOG).
 * When upstream lands the matching PR and we bump the submodule to a
 * release containing it, the local entry here can be deleted and the
 * binding falls through to the upstream symbol automatically.
 */

#ifndef RUST_LIBROCKSDB_SYS_C_API_EXTENSIONS_H_
#define RUST_LIBROCKSDB_SYS_C_API_EXTENSIONS_H_

/* Pull in every C-API type the extension functions reference. By including
 * c.h here (instead of forward-declaring), this header is a clean superset
 * of c.h: bindgen scanning this file generates declarations for everything
 * in the upstream C API plus our local additions, with no risk of missed
 * types. */
#include "rocksdb/c.h"

#ifdef __cplusplus
extern "C" {
#endif

/* -------------------------------------------------------------------------
 * ReadOptions::optimize_multiget_for_io
 *
 * Selects between the multi-level and single-level parallel MultiGet paths
 * in USE_COROUTINES builds. Mirrors the existing async_io setter/getter
 * pair. Matches upstream PR facebook/rocksdb#14752.
 * ------------------------------------------------------------------------- */
extern ROCKSDB_LIBRARY_API void rocksdb_readoptions_set_optimize_multiget_for_io(
    rocksdb_readoptions_t*, unsigned char);
extern ROCKSDB_LIBRARY_API unsigned char rocksdb_readoptions_get_optimize_multiget_for_io(
    rocksdb_readoptions_t*);

/* -------------------------------------------------------------------------
 * BlockBasedTableOptions::uniform_cv_threshold +
 * BlockSearchType::kAuto enum value
 *
 * The "auto" index-block search type was added to BlockBasedTableOptions
 * but upstream's enum in c.h only declares the binary and interpolation
 * values. The underlying C setter accepts any int via static_cast, so the
 * kAuto value (2) is reachable today by passing the raw int; only the
 * named constant was missing. The uniform_cv_threshold setter that gates
 * kAuto's behaviour at write time also has no upstream C wrapper.
 * ------------------------------------------------------------------------- */
enum {
  rocksdb_block_based_table_index_block_search_type_auto = 2,
};
extern ROCKSDB_LIBRARY_API void rocksdb_block_based_options_set_uniform_cv_threshold(
    rocksdb_block_based_table_options_t*, double);

/* -------------------------------------------------------------------------
 * AdvancedColumnFamilyOptions::memtable_batch_lookup_optimization
 *
 * Enables the skip-list memtable's batch-lookup optimization for MultiGet.
 * Immutable on the C++ side. Mirrors the existing memtable_huge_page_size
 * setter/getter pair.
 * ------------------------------------------------------------------------- */
extern ROCKSDB_LIBRARY_API void rocksdb_options_set_memtable_batch_lookup_optimization(
    rocksdb_options_t*, unsigned char);
extern ROCKSDB_LIBRARY_API unsigned char rocksdb_options_get_memtable_batch_lookup_optimization(
    rocksdb_options_t*);

/* -------------------------------------------------------------------------
 * CompactOptions::blob_garbage_collection_age_cutoff
 *
 * Sets the blob_garbage_collection_age_cutoff parameters on manual
 * compactions. Matches upstream PR facebook/rocksdb#14768.
 * ------------------------------------------------------------------------- */
extern ROCKSDB_LIBRARY_API void rocksdb_compactoptions_set_blob_garbage_collection_age_cutoff(
    rocksdb_compactoptions_t*, double);
extern ROCKSDB_LIBRARY_API double rocksdb_compactoptions_get_blob_garbage_collection_age_cutoff(
    rocksdb_compactoptions_t*);

#ifdef __cplusplus
}
#endif

#endif /* RUST_LIBROCKSDB_SYS_C_API_EXTENSIONS_H_ */

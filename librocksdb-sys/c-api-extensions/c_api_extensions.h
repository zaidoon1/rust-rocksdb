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

/* Options::open_files_async compatibility wrapper. */
extern ROCKSDB_LIBRARY_API unsigned char
rust_rocksdb_options_open_files_async_supported(void);
extern ROCKSDB_LIBRARY_API unsigned char
rust_rocksdb_options_set_open_files_async(rocksdb_options_t*, unsigned char);
extern ROCKSDB_LIBRARY_API unsigned char
rust_rocksdb_options_get_open_files_async(rocksdb_options_t*);

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

/* -------------------------------------------------------------------------
 * Batch-owned pinned MultiGet results
 *
 * The upstream batched C API allocates one rocksdb_pinnableslice_t wrapper
 * per successful key. This additive API keeps the PinnableSlice values in one
 * owner so Rust can borrow every result without per-key wrapper allocation.
 * ------------------------------------------------------------------------- */
typedef struct rust_rocksdb_pinnable_batch_t rust_rocksdb_pinnable_batch_t;

enum {
  rust_rocksdb_pinnable_batch_not_found = 0,
  rust_rocksdb_pinnable_batch_found = 1,
  rust_rocksdb_pinnable_batch_error = 2,
};

extern ROCKSDB_LIBRARY_API rust_rocksdb_pinnable_batch_t*
rust_rocksdb_batched_multi_get_pinned(
    rocksdb_t*, const rocksdb_readoptions_t*,
    rocksdb_column_family_handle_t*, size_t, const rocksdb_slice_t*,
    unsigned char, char**);
extern ROCKSDB_LIBRARY_API size_t rust_rocksdb_pinnable_batch_len(
    const rust_rocksdb_pinnable_batch_t*);
extern ROCKSDB_LIBRARY_API unsigned char rust_rocksdb_pinnable_batch_get(
    const rust_rocksdb_pinnable_batch_t*, size_t, const char**, size_t*,
    const char**, size_t*);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_pinnable_batch_destroy(
    rust_rocksdb_pinnable_batch_t*);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_batched_multi_get_cf_slice_safe(
    rocksdb_t*, const rocksdb_readoptions_t*,
    rocksdb_column_family_handle_t*, size_t, const rocksdb_slice_t*,
    rocksdb_pinnableslice_t**, char**, unsigned char, char**);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_create_iterators_safe(
    rocksdb_t*, rocksdb_readoptions_t*, rocksdb_column_family_handle_t**,
    rocksdb_iterator_t**, size_t, char**);

/* -------------------------------------------------------------------------
 * Slice-based vectored WriteBatch operations
 *
 * The upstream putv/mergev/deletev wrappers rebuild Slice arrays from
 * separate pointer and length arrays. These variants accept ABI-compatible
 * rocksdb_slice_t arrays directly.
 * ------------------------------------------------------------------------- */
extern ROCKSDB_LIBRARY_API void rust_rocksdb_writebatch_put_slices(
    rocksdb_writebatch_t*, int, const rocksdb_slice_t*, int,
    const rocksdb_slice_t*, char**);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_writebatch_put_slices_cf(
    rocksdb_writebatch_t*, rocksdb_column_family_handle_t*, int,
    const rocksdb_slice_t*, int, const rocksdb_slice_t*, char**);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_writebatch_merge_slices(
    rocksdb_writebatch_t*, int, const rocksdb_slice_t*, int,
    const rocksdb_slice_t*, char**);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_writebatch_merge_slices_cf(
    rocksdb_writebatch_t*, rocksdb_column_family_handle_t*, int,
    const rocksdb_slice_t*, int, const rocksdb_slice_t*, char**);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_writebatch_delete_slices(
    rocksdb_writebatch_t*, int, const rocksdb_slice_t*, char**);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_writebatch_delete_slices_cf(
    rocksdb_writebatch_t*, rocksdb_column_family_handle_t*, int,
    const rocksdb_slice_t*, char**);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_writebatch_delete_range_slices(
    rocksdb_writebatch_t*, int, const rocksdb_slice_t*, int,
    const rocksdb_slice_t*, char**);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_writebatch_delete_range_slices_cf(
    rocksdb_writebatch_t*, rocksdb_column_family_handle_t*, int,
    const rocksdb_slice_t*, int, const rocksdb_slice_t*, char**);

/* -------------------------------------------------------------------------
 * EventListener background error status severity and recovery callbacks
 *
 * RocksDB's C++ EventListener exposes Status::Severity on background errors
 * and has callbacks for the automatic error recovery lifecycle. The upstream
 * C listener wrapper available to this crate only forwards OnBackgroundError,
 * so the Rust listener uses this local additive wrapper instead of changing
 * the upstream rocksdb_eventlistener_create ABI.
 * ------------------------------------------------------------------------- */
typedef struct rust_rocksdb_status_t rust_rocksdb_status_t;
typedef struct rust_rocksdb_eventlistener_t rust_rocksdb_eventlistener_t;
typedef struct rust_rocksdb_background_error_recovery_info_t
    rust_rocksdb_background_error_recovery_info_t;

extern ROCKSDB_LIBRARY_API void rust_rocksdb_status_get_error(
    rust_rocksdb_status_t*, char**);
extern ROCKSDB_LIBRARY_API unsigned char rust_rocksdb_status_get_severity(
    rust_rocksdb_status_t*);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_status_reset(
    rust_rocksdb_status_t*);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_background_error_recovery_info_old_bg_error(
    const rust_rocksdb_background_error_recovery_info_t*, char**);
extern ROCKSDB_LIBRARY_API unsigned char
rust_rocksdb_background_error_recovery_info_old_bg_error_severity(
    const rust_rocksdb_background_error_recovery_info_t*);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_background_error_recovery_info_new_bg_error(
    const rust_rocksdb_background_error_recovery_info_t*, char**);
extern ROCKSDB_LIBRARY_API unsigned char
rust_rocksdb_background_error_recovery_info_new_bg_error_severity(
    const rust_rocksdb_background_error_recovery_info_t*);

typedef void (*rust_rocksdb_on_flush_begin_cb)(
    void*, const rocksdb_flushjobinfo_t*);
typedef void (*rust_rocksdb_on_flush_completed_cb)(
    void*, const rocksdb_flushjobinfo_t*);
typedef void (*rust_rocksdb_on_compaction_begin_cb)(
    void*, const rocksdb_compactionjobinfo_t*);
typedef void (*rust_rocksdb_on_compaction_completed_cb)(
    void*, const rocksdb_compactionjobinfo_t*);
typedef void (*rust_rocksdb_on_subcompaction_begin_cb)(
    void*, const rocksdb_subcompactionjobinfo_t*);
typedef void (*rust_rocksdb_on_subcompaction_completed_cb)(
    void*, const rocksdb_subcompactionjobinfo_t*);
typedef void (*rust_rocksdb_on_external_file_ingested_cb)(
    void*, const rocksdb_externalfileingestioninfo_t*);
typedef void (*rust_rocksdb_on_background_error_cb)(
    void*, uint32_t, rust_rocksdb_status_t*);
typedef void (*rust_rocksdb_on_error_recovery_begin_cb)(
    void*, uint32_t, rust_rocksdb_status_t*, unsigned char*);
typedef void (*rust_rocksdb_on_error_recovery_end_cb)(
    void*, const rust_rocksdb_background_error_recovery_info_t*);
typedef void (*rust_rocksdb_on_stall_conditions_changed_cb)(
    void*, const rocksdb_writestallinfo_t*);
typedef void (*rust_rocksdb_on_memtable_sealed_cb)(
    void*, const rocksdb_memtableinfo_t*);

extern ROCKSDB_LIBRARY_API rust_rocksdb_eventlistener_t*
rust_rocksdb_eventlistener_create(
    void* state, void (*destructor)(void*),
    rust_rocksdb_on_flush_begin_cb on_flush_begin,
    rust_rocksdb_on_flush_completed_cb on_flush_completed,
    rust_rocksdb_on_compaction_begin_cb on_compaction_begin,
    rust_rocksdb_on_compaction_completed_cb on_compaction_completed,
    rust_rocksdb_on_subcompaction_begin_cb on_subcompaction_begin,
    rust_rocksdb_on_subcompaction_completed_cb on_subcompaction_completed,
    rust_rocksdb_on_external_file_ingested_cb on_external_file_ingested,
    rust_rocksdb_on_background_error_cb on_background_error,
    rust_rocksdb_on_error_recovery_begin_cb on_error_recovery_begin,
    rust_rocksdb_on_error_recovery_end_cb on_error_recovery_end,
    rust_rocksdb_on_stall_conditions_changed_cb on_stall_conditions_changed,
    rust_rocksdb_on_memtable_sealed_cb on_memtable_sealed);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_eventlistener_destroy(
    rust_rocksdb_eventlistener_t*);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_options_add_eventlistener(
    rocksdb_options_t*, rust_rocksdb_eventlistener_t*);

#ifdef __cplusplus
}
#endif

#endif /* RUST_LIBROCKSDB_SYS_C_API_EXTENSIONS_H_ */

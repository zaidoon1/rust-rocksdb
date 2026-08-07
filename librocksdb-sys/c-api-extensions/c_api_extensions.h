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

/* Options::open_files_async compatibility wrapper. */
extern ROCKSDB_LIBRARY_API unsigned char
rust_rocksdb_options_open_files_async_supported(void);
extern ROCKSDB_LIBRARY_API unsigned char
rust_rocksdb_options_set_open_files_async(rocksdb_options_t*, unsigned char);
extern ROCKSDB_LIBRARY_API unsigned char
rust_rocksdb_options_get_open_files_async(rocksdb_options_t*);

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
  /* index outside the batch, or a batch whose internal state does not agree
     with itself. No out-params are written. */
  rust_rocksdb_pinnable_batch_out_of_range = 3,
};

/* A null `column_family` selects the default column family. */
extern ROCKSDB_LIBRARY_API rust_rocksdb_pinnable_batch_t*
rust_rocksdb_batched_multi_get_pinned(
    rocksdb_t* db, const rocksdb_readoptions_t* options,
    rocksdb_column_family_handle_t* column_family, size_t num_keys,
    const rocksdb_slice_t* keys, unsigned char sorted_input, char** errptr);
extern ROCKSDB_LIBRARY_API size_t rust_rocksdb_pinnable_batch_len(
    const rust_rocksdb_pinnable_batch_t* batch);
/* `value` and `error` are borrowed from `batch` and dangle once it is
   destroyed. Do not free them. */
extern ROCKSDB_LIBRARY_API unsigned char rust_rocksdb_pinnable_batch_get(
    const rust_rocksdb_pinnable_batch_t* batch, size_t index,
    const char** value, size_t* value_len, const char** error,
    size_t* error_len);
extern ROCKSDB_LIBRARY_API void rust_rocksdb_pinnable_batch_destroy(
    rust_rocksdb_pinnable_batch_t* batch);
/* `column_family` must not be null, unlike
   `rust_rocksdb_batched_multi_get_pinned` above.
   The caller owns and must release: each non-null `values[i]` with
   `rocksdb_pinnableslice_destroy`, each non-null per-key `errors[i]` with
   `rocksdb_free`, and `*errptr` with `rocksdb_free`. */
extern ROCKSDB_LIBRARY_API void rust_rocksdb_batched_multi_get_cf_slice_safe(
    rocksdb_t* db, const rocksdb_readoptions_t* options,
    rocksdb_column_family_handle_t* column_family, size_t num_keys,
    const rocksdb_slice_t* keys, rocksdb_pinnableslice_t** values,
    char** errors, unsigned char sorted_input, char** errptr);
/* On success the caller owns every `iterators[i]` and must release it with
   `rocksdb_iter_destroy`. On failure no iterator is returned and `*errptr` is
   set. `options` must outlive the returned iterators: RocksDB keeps raw
   pointers to its iterate bounds and timestamps. */
extern ROCKSDB_LIBRARY_API void rust_rocksdb_create_iterators_safe(
    rocksdb_t* db, rocksdb_readoptions_t* options,
    rocksdb_column_family_handle_t** column_families,
    rocksdb_iterator_t** iterators, size_t size, char** errptr);

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

/* -------------------------------------------------------------------------
 * Null-safe Snapshot::GetSequenceNumber
 *
 * `rocksdb_transaction_get_snapshot` always mallocs a `rocksdb_snapshot_t`
 * wrapper, but its `rep` is `Transaction::GetSnapshot()`, which is nullptr
 * unless the transaction was started with `set_snapshot(true)`. Upstream's
 * `rocksdb_snapshot_get_sequence_number` dereferences `rep`
 * unconditionally, so calling it on such a snapshot is a null dereference
 * reachable from safe Rust. `rep` is not visible from Rust because
 * `rocksdb_snapshot_t` is opaque, hence this accessor.
 *
 * Returns 1 and writes the sequence number through `seqno` when the
 * snapshot is present; returns 0 and leaves `seqno` untouched otherwise.
 * ------------------------------------------------------------------------- */
extern ROCKSDB_LIBRARY_API unsigned char
rust_rocksdb_snapshot_try_get_sequence_number(const rocksdb_snapshot_t*,
                                             uint64_t* seqno);

#ifdef __cplusplus
}
#endif

#endif /* RUST_LIBROCKSDB_SYS_C_API_EXTENSIONS_H_ */

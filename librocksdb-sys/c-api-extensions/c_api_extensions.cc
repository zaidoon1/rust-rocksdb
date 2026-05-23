// Local additions to the RocksDB C API. See c_api_extensions.h for the
// rationale and the list of extensions; this file just defines the
// declarations from that header.
//
// Each extension function is the smallest possible delta over the existing
// C API: a setter (or setter/getter pair) that exposes one C++ option
// field. The implementations are intentionally identical in shape to the
// upstream C wrappers they would replace once the matching PR lands.

#include "c_api_extensions.h"

#include "rocksdb/options.h"
#include "rocksdb/table.h"

using ROCKSDB_NAMESPACE::BlockBasedTableOptions;
using ROCKSDB_NAMESPACE::Options;
using ROCKSDB_NAMESPACE::ReadOptions;

// The opaque-handle types the C API hands out are defined at file scope in
// `rocksdb/db/c.cc` as POD wrappers around a single C++ class:
//
//   struct rocksdb_readoptions_t { ReadOptions rep; /* trailing Slices */ };
//   struct rocksdb_options_t { Options rep; };
//   struct rocksdb_block_based_table_options_t { BlockBasedTableOptions rep; };
//
// In every case the `rep` field is the FIRST member, so a pointer to the
// opaque C type also points at the start of its embedded C++ `rep` field.
// We exploit that here with a direct `reinterpret_cast` instead of
// replicating the struct definitions — replication would either drift
// silently if upstream ever adds a field before `rep` (the very change that
// would also break this cast), or trip C++'s one-definition rule against
// c.h's `typedef struct rocksdb_readoptions_t rocksdb_readoptions_t;`.
//
// If upstream ever adds a field BEFORE `rep` in any of these wrappers,
// every test that round-trips a value through one of our setters will
// fail loudly: the setter would write to one offset and rocksdb's
// internal code would read from another. The integration tests in
// `tests/test_rocksdb_options.rs` cover all three options that this file
// exposes, so a layout regression is detectable.

// -----------------------------------------------------------------------------
// ReadOptions::optimize_multiget_for_io
// -----------------------------------------------------------------------------

extern "C" void rocksdb_readoptions_set_optimize_multiget_for_io(
    rocksdb_readoptions_t* opt, unsigned char v) {
  reinterpret_cast<ReadOptions*>(opt)->optimize_multiget_for_io = v;
}

extern "C" unsigned char rocksdb_readoptions_get_optimize_multiget_for_io(
    rocksdb_readoptions_t* opt) {
  return reinterpret_cast<ReadOptions*>(opt)->optimize_multiget_for_io;
}

// -----------------------------------------------------------------------------
// BlockBasedTableOptions::uniform_cv_threshold
//
// The corresponding `kAuto` enum value is declared in c_api_extensions.h
// — no C-side definition is needed because the existing
// `rocksdb_block_based_options_set_index_block_search_type` setter in
// upstream `c.cc` already does `static_cast<BlockSearchType>(int)` and
// accepts any value the caller passes.
// -----------------------------------------------------------------------------

extern "C" void rocksdb_block_based_options_set_uniform_cv_threshold(
    rocksdb_block_based_table_options_t* opt, double v) {
  reinterpret_cast<BlockBasedTableOptions*>(opt)->uniform_cv_threshold = v;
}

// -----------------------------------------------------------------------------
// AdvancedColumnFamilyOptions::memtable_batch_lookup_optimization
// -----------------------------------------------------------------------------

extern "C" void rocksdb_options_set_memtable_batch_lookup_optimization(
    rocksdb_options_t* opt, unsigned char v) {
  reinterpret_cast<Options*>(opt)->memtable_batch_lookup_optimization = v;
}

extern "C" unsigned char rocksdb_options_get_memtable_batch_lookup_optimization(
    rocksdb_options_t* opt) {
  return reinterpret_cast<Options*>(opt)->memtable_batch_lookup_optimization;
}

// Local additions to the RocksDB C API. See c_api_extensions.h for the
// rationale and the list of extensions; this file just defines the
// declarations from that header.
//
// Each extension is the smallest practical delta over the existing C API:
// either an option setter/getter pair or a thin wrapper over an upstream C++
// callback surface that has not reached rocksdb/c.h yet.

#include "c_api_extensions.h"

#include <cassert>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <limits>
#include <string>
#include <unordered_map>
#include <vector>

#include "rocksdb/db.h"
#include "rocksdb/listener.h"
#include "rocksdb/options.h"
#include "rocksdb/slice.h"
#include "rocksdb/table.h"
#include "rocksdb/version.h"
#include "rocksdb/write_batch.h"

using ROCKSDB_NAMESPACE::BackgroundErrorRecoveryInfo;
using ROCKSDB_NAMESPACE::BlockBasedTableOptions;
using ROCKSDB_NAMESPACE::CompactRangeOptions;
using ROCKSDB_NAMESPACE::CompactionJobInfo;
using ROCKSDB_NAMESPACE::ColumnFamilyHandle;
using ROCKSDB_NAMESPACE::DB;
using ROCKSDB_NAMESPACE::EventListener;
using ROCKSDB_NAMESPACE::ExternalFileIngestionInfo;
using ROCKSDB_NAMESPACE::FlushJobInfo;
using ROCKSDB_NAMESPACE::Options;
using ROCKSDB_NAMESPACE::PinnableSlice;
using ROCKSDB_NAMESPACE::ReadOptions;
using ROCKSDB_NAMESPACE::Slice;
using ROCKSDB_NAMESPACE::SliceParts;
using ROCKSDB_NAMESPACE::Status;
using ROCKSDB_NAMESPACE::SubcompactionJobInfo;
using ROCKSDB_NAMESPACE::WriteStallInfo;
using ROCKSDB_NAMESPACE::WriteBatch;
using ROCKSDB_NAMESPACE::MemTableInfo;

struct rust_rocksdb_status_t {
  Status* rep;
};

struct rust_rocksdb_background_error_recovery_info_t {
  const BackgroundErrorRecoveryInfo* rep;
};

static bool RustSaveError(char** errptr, const Status& s) {
  assert(errptr != nullptr);
  if (s.ok()) {
    return false;
  }

  std::string message = s.ToString();
  char* copy = static_cast<char*>(std::malloc(message.size() + 1));
  if (copy != nullptr) {
    std::memcpy(copy, message.c_str(), message.size() + 1);
  }

  if (*errptr != nullptr) {
    std::free(*errptr);
  }
  *errptr = copy;
  return true;
}

static void RustSaveMessage(char** errptr, const char* message) {
  assert(errptr != nullptr);
  char* copy = message == nullptr ? nullptr : strdup(message);
  if (*errptr != nullptr) {
    std::free(*errptr);
  }
  *errptr = copy;
}

extern "C" void rust_rocksdb_status_get_error(rust_rocksdb_status_t* status,
                                               char** errptr) {
  RustSaveError(errptr, *(status->rep));
}

extern "C" unsigned char rust_rocksdb_status_get_severity(
    rust_rocksdb_status_t* status) {
  return static_cast<unsigned char>(status->rep->severity());
}

extern "C" void rust_rocksdb_status_reset(rust_rocksdb_status_t* status) {
  *(status->rep) = Status::OK();
}

extern "C" void rust_rocksdb_background_error_recovery_info_old_bg_error(
    const rust_rocksdb_background_error_recovery_info_t* info, char** errptr) {
  RustSaveError(errptr, info->rep->old_bg_error);
}

extern "C" unsigned char
rust_rocksdb_background_error_recovery_info_old_bg_error_severity(
    const rust_rocksdb_background_error_recovery_info_t* info) {
  return static_cast<unsigned char>(info->rep->old_bg_error.severity());
}

extern "C" void rust_rocksdb_background_error_recovery_info_new_bg_error(
    const rust_rocksdb_background_error_recovery_info_t* info, char** errptr) {
  RustSaveError(errptr, info->rep->new_bg_error);
}

extern "C" unsigned char
rust_rocksdb_background_error_recovery_info_new_bg_error_severity(
    const rust_rocksdb_background_error_recovery_info_t* info) {
  return static_cast<unsigned char>(info->rep->new_bg_error.severity());
}

struct rust_rocksdb_eventlistener_t : public EventListener {
  void* state{};
  void (*destructor)(void*){};
  rust_rocksdb_on_flush_begin_cb on_flush_begin{};
  rust_rocksdb_on_flush_completed_cb on_flush_completed{};
  rust_rocksdb_on_compaction_begin_cb on_compaction_begin{};
  rust_rocksdb_on_compaction_completed_cb on_compaction_completed{};
  rust_rocksdb_on_subcompaction_begin_cb on_subcompaction_begin{};
  rust_rocksdb_on_subcompaction_completed_cb on_subcompaction_completed{};
  rust_rocksdb_on_external_file_ingested_cb on_external_file_ingested{};
  rust_rocksdb_on_background_error_cb on_background_error{};
  rust_rocksdb_on_error_recovery_begin_cb on_error_recovery_begin{};
  rust_rocksdb_on_error_recovery_end_cb on_error_recovery_end{};
  rust_rocksdb_on_stall_conditions_changed_cb on_stall_conditions_changed{};
  rust_rocksdb_on_memtable_sealed_cb on_memtable_sealed{};

  rust_rocksdb_eventlistener_t() = default;

  rust_rocksdb_eventlistener_t(const rust_rocksdb_eventlistener_t&) = delete;
  rust_rocksdb_eventlistener_t& operator=(
      const rust_rocksdb_eventlistener_t&) = delete;
  rust_rocksdb_eventlistener_t(rust_rocksdb_eventlistener_t&&) = delete;
  rust_rocksdb_eventlistener_t& operator=(rust_rocksdb_eventlistener_t&&) =
      delete;

  void OnFlushBegin(DB* /*db*/, const FlushJobInfo& info) override {
    if (on_flush_begin != nullptr) {
      on_flush_begin(state,
                     reinterpret_cast<const rocksdb_flushjobinfo_t*>(&info));
    }
  }

  void OnFlushCompleted(DB* /*db*/, const FlushJobInfo& info) override {
    if (on_flush_completed != nullptr) {
      on_flush_completed(
          state, reinterpret_cast<const rocksdb_flushjobinfo_t*>(&info));
    }
  }

  void OnCompactionBegin(DB* /*db*/, const CompactionJobInfo& info) override {
    if (on_compaction_begin != nullptr) {
      on_compaction_begin(
          state, reinterpret_cast<const rocksdb_compactionjobinfo_t*>(&info));
    }
  }

  void OnCompactionCompleted(DB* /*db*/, const CompactionJobInfo& info)
      override {
    if (on_compaction_completed != nullptr) {
      on_compaction_completed(
          state, reinterpret_cast<const rocksdb_compactionjobinfo_t*>(&info));
    }
  }

  void OnSubcompactionBegin(const SubcompactionJobInfo& info) override {
    if (on_subcompaction_begin != nullptr) {
      on_subcompaction_begin(
          state,
          reinterpret_cast<const rocksdb_subcompactionjobinfo_t*>(&info));
    }
  }

  void OnSubcompactionCompleted(const SubcompactionJobInfo& info) override {
    if (on_subcompaction_completed != nullptr) {
      on_subcompaction_completed(
          state,
          reinterpret_cast<const rocksdb_subcompactionjobinfo_t*>(&info));
    }
  }

  void OnExternalFileIngested(DB* /*db*/,
                              const ExternalFileIngestionInfo& info) override {
    if (on_external_file_ingested != nullptr) {
      on_external_file_ingested(
          state,
          reinterpret_cast<const rocksdb_externalfileingestioninfo_t*>(&info));
    }
  }

  void OnBackgroundError(ROCKSDB_NAMESPACE::BackgroundErrorReason reason,
                         Status* status) override {
    if (on_background_error != nullptr) {
      rust_rocksdb_status_t s = {status};
      on_background_error(state, static_cast<uint32_t>(reason), &s);
    }
  }

  void OnErrorRecoveryBegin(ROCKSDB_NAMESPACE::BackgroundErrorReason reason,
                            Status bg_error,
                            bool* auto_recovery) override {
    if (on_error_recovery_begin != nullptr) {
      rust_rocksdb_status_t s = {&bg_error};
      unsigned char auto_recovery_value =
          auto_recovery != nullptr && *auto_recovery;
      on_error_recovery_begin(state, static_cast<uint32_t>(reason), &s,
                              &auto_recovery_value);
      if (auto_recovery != nullptr) {
        *auto_recovery = auto_recovery_value != 0;
      }
    }
    bg_error.PermitUncheckedError();
  }

  void OnErrorRecoveryEnd(const BackgroundErrorRecoveryInfo& info) override {
    if (on_error_recovery_end != nullptr) {
      rust_rocksdb_background_error_recovery_info_t c_info = {&info};
      on_error_recovery_end(state, &c_info);
    }
    info.old_bg_error.PermitUncheckedError();
    info.new_bg_error.PermitUncheckedError();
  }

  void OnStallConditionsChanged(const WriteStallInfo& info) override {
    if (on_stall_conditions_changed != nullptr) {
      on_stall_conditions_changed(
          state, reinterpret_cast<const rocksdb_writestallinfo_t*>(&info));
    }
  }

  void OnMemTableSealed(const MemTableInfo& info) override {
    if (on_memtable_sealed != nullptr) {
      on_memtable_sealed(
          state, reinterpret_cast<const rocksdb_memtableinfo_t*>(&info));
    }
  }

  ~rust_rocksdb_eventlistener_t() override {
    if (destructor != nullptr) {
      destructor(state);
    }
  }
};

extern "C" rust_rocksdb_eventlistener_t* rust_rocksdb_eventlistener_create(
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
    rust_rocksdb_on_memtable_sealed_cb on_memtable_sealed) {
  rust_rocksdb_eventlistener_t* listener = new rust_rocksdb_eventlistener_t;
  listener->state = state;
  listener->destructor = destructor;
  listener->on_flush_begin = on_flush_begin;
  listener->on_flush_completed = on_flush_completed;
  listener->on_compaction_begin = on_compaction_begin;
  listener->on_compaction_completed = on_compaction_completed;
  listener->on_subcompaction_begin = on_subcompaction_begin;
  listener->on_subcompaction_completed = on_subcompaction_completed;
  listener->on_external_file_ingested = on_external_file_ingested;
  listener->on_background_error = on_background_error;
  listener->on_error_recovery_begin = on_error_recovery_begin;
  listener->on_error_recovery_end = on_error_recovery_end;
  listener->on_stall_conditions_changed = on_stall_conditions_changed;
  listener->on_memtable_sealed = on_memtable_sealed;
  return listener;
}

extern "C" void rust_rocksdb_eventlistener_destroy(
    rust_rocksdb_eventlistener_t* listener) {
  delete listener;
}

extern "C" void rust_rocksdb_options_add_eventlistener(
    rocksdb_options_t* opt, rust_rocksdb_eventlistener_t* listener) {
  reinterpret_cast<Options*>(opt)->listeners.emplace_back(
      std::shared_ptr<EventListener>(listener));
}

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

// -----------------------------------------------------------------------------
// Options::open_files_async
// -----------------------------------------------------------------------------

#if ROCKSDB_MAJOR > 11 || (ROCKSDB_MAJOR == 11 && ROCKSDB_MINOR >= 1)
#define RUST_ROCKSDB_HAS_OPEN_FILES_ASYNC 1
#else
#define RUST_ROCKSDB_HAS_OPEN_FILES_ASYNC 0
#endif

extern "C" unsigned char rust_rocksdb_options_open_files_async_supported() {
  return RUST_ROCKSDB_HAS_OPEN_FILES_ASYNC;
}

extern "C" unsigned char rust_rocksdb_options_set_open_files_async(
    rocksdb_options_t* opt, unsigned char enabled) {
#if RUST_ROCKSDB_HAS_OPEN_FILES_ASYNC
  rocksdb_options_set_open_files_async(opt, enabled);
  return 1;
#else
  (void)opt;
  (void)enabled;
  return 0;
#endif
}

extern "C" unsigned char rust_rocksdb_options_get_open_files_async(
    rocksdb_options_t* opt) {
#if RUST_ROCKSDB_HAS_OPEN_FILES_ASYNC
  return rocksdb_options_get_open_files_async(opt);
#else
  (void)opt;
  return 0;
#endif
}

// -----------------------------------------------------------------------------
// CompactOptions::blob_garbage_collection_age_cutoff
// -----------------------------------------------------------------------------

extern "C" void rocksdb_compactoptions_set_blob_garbage_collection_age_cutoff(
    rocksdb_compactoptions_t* opt, double v) {
  reinterpret_cast<CompactRangeOptions*>(opt)->blob_garbage_collection_age_cutoff = v;
}

extern "C" double rocksdb_compactoptions_get_blob_garbage_collection_age_cutoff(
    rocksdb_compactoptions_t* opt) {
  return reinterpret_cast<CompactRangeOptions*>(opt)->blob_garbage_collection_age_cutoff;
}

// -----------------------------------------------------------------------------
// Batch-owned pinned MultiGet results
// -----------------------------------------------------------------------------

struct rust_rocksdb_pinnable_batch_t {
#ifdef RUST_ROCKSDB_SYSTEM_BACKEND
  std::vector<rocksdb_pinnableslice_t*> system_values;
#else
  std::vector<PinnableSlice> values;
  std::vector<size_t> result_indexes;
#endif
  std::unordered_map<size_t, std::string> errors;

#ifdef RUST_ROCKSDB_SYSTEM_BACKEND
  ~rust_rocksdb_pinnable_batch_t() {
    for (auto* value : system_values) {
      if (value != nullptr) {
        rocksdb_pinnableslice_destroy(value);
      }
    }
  }
#endif
};

#ifndef RUST_ROCKSDB_SYSTEM_BACKEND
static constexpr size_t kRustRocksDbBatchNotFound =
    std::numeric_limits<size_t>::max();
static constexpr size_t kRustRocksDbBatchError =
    std::numeric_limits<size_t>::max() - 1;
#endif

#ifndef RUST_ROCKSDB_SYSTEM_BACKEND
static DB* RustRocksDbRep(rocksdb_t* db) {
  return *reinterpret_cast<DB**>(db);
}

static ColumnFamilyHandle* RustRocksDbColumnFamilyRep(
    rocksdb_column_family_handle_t* column_family) {
  return *reinterpret_cast<ColumnFamilyHandle**>(column_family);
}
#endif

extern "C" rust_rocksdb_pinnable_batch_t*
rust_rocksdb_batched_multi_get_pinned(
    rocksdb_t* db, const rocksdb_readoptions_t* options,
    rocksdb_column_family_handle_t* column_family, size_t num_keys,
    const rocksdb_slice_t* keys, unsigned char sorted_input, char** errptr) {
  try {
    auto batch = std::make_unique<rust_rocksdb_pinnable_batch_t>();
#ifdef RUST_ROCKSDB_SYSTEM_BACKEND
    batch->system_values.resize(num_keys, nullptr);
    std::vector<char*> errors(num_keys, nullptr);
    using ColumnFamilyHandlePtr =
        std::unique_ptr<rocksdb_column_family_handle_t,
                        decltype(&rocksdb_column_family_handle_destroy)>;
    ColumnFamilyHandlePtr owned_default(nullptr,
                                        &rocksdb_column_family_handle_destroy);
    if (column_family == nullptr) {
      owned_default.reset(rocksdb_get_default_column_family_handle(db));
      column_family = owned_default.get();
    }
    try {
      rocksdb_batched_multi_get_cf_slice(
          db, options, column_family, num_keys, keys,
          batch->system_values.data(), errors.data(), sorted_input != 0);
    } catch (...) {
      for (auto* error : errors) {
        if (error != nullptr) {
          rocksdb_free(error);
        }
      }
      throw;
    }
    for (size_t i = 0; i < num_keys; ++i) {
      if (errors[i] != nullptr) {
        batch->errors.emplace(i, errors[i]);
        rocksdb_free(errors[i]);
      }
    }
#else
    batch->result_indexes.resize(num_keys, kRustRocksDbBatchNotFound);
    if (num_keys == 0) {
      return batch.release();
    }

    DB* db_rep = RustRocksDbRep(db);
    ColumnFamilyHandle* column_family_rep =
        column_family == nullptr ? db_rep->DefaultColumnFamily()
                                 : RustRocksDbColumnFamilyRep(column_family);
    const auto* read_options = reinterpret_cast<const ReadOptions*>(options);
    const auto* key_slices = reinterpret_cast<const Slice*>(keys);
    std::vector<PinnableSlice> values(num_keys);
    std::vector<Status> statuses(num_keys);

    db_rep->MultiGet(*read_options, column_family_rep, num_keys, key_slices,
                     values.data(), statuses.data(), sorted_input != 0);

    size_t hit_count = 0;
    for (const auto& status : statuses) {
      if (status.ok()) {
        ++hit_count;
      }
    }
    batch->values.reserve(hit_count);
    for (size_t i = 0; i < num_keys; ++i) {
      if (statuses[i].ok()) {
        batch->result_indexes[i] = batch->values.size();
        batch->values.emplace_back(std::move(values[i]));
      } else if (!statuses[i].IsNotFound()) {
        batch->result_indexes[i] = kRustRocksDbBatchError;
        batch->errors.emplace(i, statuses[i].ToString());
      }
    }
#endif
    return batch.release();
  } catch (const std::exception& error) {
    RustSaveMessage(errptr, error.what());
    return nullptr;
  } catch (...) {
    RustSaveMessage(errptr, "unknown C++ exception in pinned MultiGet");
    return nullptr;
  }
}

extern "C" size_t rust_rocksdb_pinnable_batch_len(
    const rust_rocksdb_pinnable_batch_t* batch) {
#ifdef RUST_ROCKSDB_SYSTEM_BACKEND
  return batch->system_values.size();
#else
  return batch->result_indexes.size();
#endif
}

extern "C" unsigned char rust_rocksdb_pinnable_batch_get(
    const rust_rocksdb_pinnable_batch_t* batch, size_t index,
    const char** value, size_t* value_len, const char** error,
    size_t* error_len) {
  *value = nullptr;
  *value_len = 0;
  *error = nullptr;
  *error_len = 0;

#ifdef RUST_ROCKSDB_SYSTEM_BACKEND
  assert(index < batch->system_values.size());
  const auto error_iter = batch->errors.find(index);
  if (error_iter != batch->errors.end()) {
    *error = error_iter->second.data();
    *error_len = error_iter->second.size();
    return rust_rocksdb_pinnable_batch_error;
  }
  if (batch->system_values[index] == nullptr) {
    return rust_rocksdb_pinnable_batch_not_found;
  }
  *value =
      rocksdb_pinnableslice_value(batch->system_values[index], value_len);
  return rust_rocksdb_pinnable_batch_found;
#else
  assert(index < batch->result_indexes.size());
  const size_t result_index = batch->result_indexes[index];
  if (result_index == kRustRocksDbBatchNotFound) {
    return rust_rocksdb_pinnable_batch_not_found;
  }
  if (result_index == kRustRocksDbBatchError) {
    const auto error_iter = batch->errors.find(index);
    assert(error_iter != batch->errors.end());
    *error = error_iter->second.data();
    *error_len = error_iter->second.size();
    return rust_rocksdb_pinnable_batch_error;
  }

  *value = batch->values[result_index].data();
  *value_len = batch->values[result_index].size();
  return rust_rocksdb_pinnable_batch_found;
#endif
}

extern "C" void rust_rocksdb_pinnable_batch_destroy(
    rust_rocksdb_pinnable_batch_t* batch) {
  delete batch;
}

extern "C" void rust_rocksdb_batched_multi_get_cf_slice_safe(
    rocksdb_t* db, const rocksdb_readoptions_t* options,
    rocksdb_column_family_handle_t* column_family, size_t num_keys,
    const rocksdb_slice_t* keys, rocksdb_pinnableslice_t** values, char** errors,
    unsigned char sorted_input, char** errptr) {
  for (size_t i = 0; i < num_keys; ++i) {
    values[i] = nullptr;
    errors[i] = nullptr;
  }
  try {
    rocksdb_batched_multi_get_cf_slice(
        db, options, column_family, num_keys, keys, values, errors,
        sorted_input != 0);
  } catch (const std::exception& error) {
    for (size_t i = 0; i < num_keys; ++i) {
      if (values[i] != nullptr) {
        rocksdb_pinnableslice_destroy(values[i]);
        values[i] = nullptr;
      }
      if (errors[i] != nullptr) {
        rocksdb_free(errors[i]);
        errors[i] = nullptr;
      }
    }
    RustSaveMessage(errptr, error.what());
  } catch (...) {
    for (size_t i = 0; i < num_keys; ++i) {
      if (values[i] != nullptr) {
        rocksdb_pinnableslice_destroy(values[i]);
        values[i] = nullptr;
      }
      if (errors[i] != nullptr) {
        rocksdb_free(errors[i]);
        errors[i] = nullptr;
      }
    }
    RustSaveMessage(errptr, "unknown C++ exception in batched MultiGet");
  }
}

extern "C" void rust_rocksdb_create_iterators_safe(
    rocksdb_t* db, rocksdb_readoptions_t* options,
    rocksdb_column_family_handle_t** column_families,
    rocksdb_iterator_t** iterators, size_t size, char** errptr) {
  for (size_t i = 0; i < size; ++i) {
    iterators[i] = nullptr;
  }
  try {
    rocksdb_create_iterators(db, options, column_families, iterators, size,
                             errptr);
  } catch (const std::exception& error) {
    for (size_t i = 0; i < size; ++i) {
      if (iterators[i] != nullptr) {
        rocksdb_iter_destroy(iterators[i]);
        iterators[i] = nullptr;
      }
    }
    RustSaveMessage(errptr, error.what());
  } catch (...) {
    for (size_t i = 0; i < size; ++i) {
      if (iterators[i] != nullptr) {
        rocksdb_iter_destroy(iterators[i]);
        iterators[i] = nullptr;
      }
    }
    RustSaveMessage(errptr, "unknown C++ exception while creating iterators");
  }
}

// -----------------------------------------------------------------------------
// Slice-based vectored WriteBatch operations
// -----------------------------------------------------------------------------

#ifndef RUST_ROCKSDB_SYSTEM_BACKEND
static WriteBatch* RustRocksDbWriteBatchRep(rocksdb_writebatch_t* batch) {
  return reinterpret_cast<WriteBatch*>(batch);
}

static SliceParts RustRocksDbSliceParts(int count,
                                       const rocksdb_slice_t* parts) {
  return SliceParts(reinterpret_cast<const Slice*>(parts), count);
}
#endif

template <typename Operation>
static void RustRocksDbWriteBatchCall(char** errptr, Operation operation) {
  try {
    RustSaveError(errptr, operation());
  } catch (const std::exception& error) {
    RustSaveMessage(errptr, error.what());
  } catch (...) {
    RustSaveMessage(errptr, "unknown C++ exception in vectored WriteBatch");
  }
}

#ifdef RUST_ROCKSDB_SYSTEM_BACKEND
struct RustRocksDbSliceArrays {
  std::vector<const char*> pointers;
  std::vector<size_t> lengths;

  RustRocksDbSliceArrays(int count, const rocksdb_slice_t* slices) {
    pointers.reserve(count);
    lengths.reserve(count);
    for (int i = 0; i < count; ++i) {
      pointers.push_back(slices[i].data);
      lengths.push_back(slices[i].size);
    }
  }
};

static void RustRocksDbCheckWriteBatchCount(rocksdb_writebatch_t* batch,
                                            int before, char** errptr) {
  if (rocksdb_writebatch_count(batch) != before + 1) {
    RustSaveMessage(errptr, "RocksDB rejected the vectored WriteBatch operation");
  }
}
#endif

extern "C" void rust_rocksdb_writebatch_put_slices(
    rocksdb_writebatch_t* batch, int key_count, const rocksdb_slice_t* keys,
    int value_count, const rocksdb_slice_t* values, char** errptr) {
  RustRocksDbWriteBatchCall(errptr, [&] {
#ifdef RUST_ROCKSDB_SYSTEM_BACKEND
    const int before = rocksdb_writebatch_count(batch);
    RustRocksDbSliceArrays key_parts(key_count, keys);
    RustRocksDbSliceArrays value_parts(value_count, values);
    rocksdb_writebatch_putv(batch, key_count, key_parts.pointers.data(),
                            key_parts.lengths.data(), value_count,
                            value_parts.pointers.data(),
                            value_parts.lengths.data());
    RustRocksDbCheckWriteBatchCount(batch, before, errptr);
    return Status::OK();
#else
    return RustRocksDbWriteBatchRep(batch)->Put(
        RustRocksDbSliceParts(key_count, keys),
        RustRocksDbSliceParts(value_count, values));
#endif
  });
}

extern "C" void rust_rocksdb_writebatch_put_slices_cf(
    rocksdb_writebatch_t* batch,
    rocksdb_column_family_handle_t* column_family, int key_count,
    const rocksdb_slice_t* keys, int value_count,
    const rocksdb_slice_t* values, char** errptr) {
  RustRocksDbWriteBatchCall(errptr, [&] {
#ifdef RUST_ROCKSDB_SYSTEM_BACKEND
    const int before = rocksdb_writebatch_count(batch);
    RustRocksDbSliceArrays key_parts(key_count, keys);
    RustRocksDbSliceArrays value_parts(value_count, values);
    rocksdb_writebatch_putv_cf(
        batch, column_family, key_count, key_parts.pointers.data(),
        key_parts.lengths.data(), value_count, value_parts.pointers.data(),
        value_parts.lengths.data());
    RustRocksDbCheckWriteBatchCount(batch, before, errptr);
    return Status::OK();
#else
    return RustRocksDbWriteBatchRep(batch)->Put(
        RustRocksDbColumnFamilyRep(column_family),
        RustRocksDbSliceParts(key_count, keys),
        RustRocksDbSliceParts(value_count, values));
#endif
  });
}

extern "C" void rust_rocksdb_writebatch_merge_slices(
    rocksdb_writebatch_t* batch, int key_count, const rocksdb_slice_t* keys,
    int value_count, const rocksdb_slice_t* values, char** errptr) {
  RustRocksDbWriteBatchCall(errptr, [&] {
#ifdef RUST_ROCKSDB_SYSTEM_BACKEND
    const int before = rocksdb_writebatch_count(batch);
    RustRocksDbSliceArrays key_parts(key_count, keys);
    RustRocksDbSliceArrays value_parts(value_count, values);
    rocksdb_writebatch_mergev(batch, key_count, key_parts.pointers.data(),
                              key_parts.lengths.data(), value_count,
                              value_parts.pointers.data(),
                              value_parts.lengths.data());
    RustRocksDbCheckWriteBatchCount(batch, before, errptr);
    return Status::OK();
#else
    return RustRocksDbWriteBatchRep(batch)->Merge(
        RustRocksDbSliceParts(key_count, keys),
        RustRocksDbSliceParts(value_count, values));
#endif
  });
}

extern "C" void rust_rocksdb_writebatch_merge_slices_cf(
    rocksdb_writebatch_t* batch,
    rocksdb_column_family_handle_t* column_family, int key_count,
    const rocksdb_slice_t* keys, int value_count,
    const rocksdb_slice_t* values, char** errptr) {
  RustRocksDbWriteBatchCall(errptr, [&] {
#ifdef RUST_ROCKSDB_SYSTEM_BACKEND
    const int before = rocksdb_writebatch_count(batch);
    RustRocksDbSliceArrays key_parts(key_count, keys);
    RustRocksDbSliceArrays value_parts(value_count, values);
    rocksdb_writebatch_mergev_cf(
        batch, column_family, key_count, key_parts.pointers.data(),
        key_parts.lengths.data(), value_count, value_parts.pointers.data(),
        value_parts.lengths.data());
    RustRocksDbCheckWriteBatchCount(batch, before, errptr);
    return Status::OK();
#else
    return RustRocksDbWriteBatchRep(batch)->Merge(
        RustRocksDbColumnFamilyRep(column_family),
        RustRocksDbSliceParts(key_count, keys),
        RustRocksDbSliceParts(value_count, values));
#endif
  });
}

extern "C" void rust_rocksdb_writebatch_delete_slices(
    rocksdb_writebatch_t* batch, int key_count, const rocksdb_slice_t* keys,
    char** errptr) {
  RustRocksDbWriteBatchCall(errptr, [&] {
#ifdef RUST_ROCKSDB_SYSTEM_BACKEND
    const int before = rocksdb_writebatch_count(batch);
    RustRocksDbSliceArrays key_parts(key_count, keys);
    rocksdb_writebatch_deletev(batch, key_count, key_parts.pointers.data(),
                               key_parts.lengths.data());
    RustRocksDbCheckWriteBatchCount(batch, before, errptr);
    return Status::OK();
#else
    return RustRocksDbWriteBatchRep(batch)->Delete(
        RustRocksDbSliceParts(key_count, keys));
#endif
  });
}

extern "C" void rust_rocksdb_writebatch_delete_slices_cf(
    rocksdb_writebatch_t* batch,
    rocksdb_column_family_handle_t* column_family, int key_count,
    const rocksdb_slice_t* keys, char** errptr) {
  RustRocksDbWriteBatchCall(errptr, [&] {
#ifdef RUST_ROCKSDB_SYSTEM_BACKEND
    const int before = rocksdb_writebatch_count(batch);
    RustRocksDbSliceArrays key_parts(key_count, keys);
    rocksdb_writebatch_deletev_cf(batch, column_family, key_count,
                                  key_parts.pointers.data(),
                                  key_parts.lengths.data());
    RustRocksDbCheckWriteBatchCount(batch, before, errptr);
    return Status::OK();
#else
    return RustRocksDbWriteBatchRep(batch)->Delete(
        RustRocksDbColumnFamilyRep(column_family),
        RustRocksDbSliceParts(key_count, keys));
#endif
  });
}

extern "C" void rust_rocksdb_writebatch_delete_range_slices(
    rocksdb_writebatch_t* batch, int begin_count,
    const rocksdb_slice_t* begin, int end_count, const rocksdb_slice_t* end,
    char** errptr) {
  RustRocksDbWriteBatchCall(errptr, [&] {
#ifdef RUST_ROCKSDB_SYSTEM_BACKEND
    if (begin_count != end_count) {
      RustSaveMessage(errptr,
                      "system RocksDB requires equal range-bound part counts");
      return Status::OK();
    }
    const int before = rocksdb_writebatch_count(batch);
    RustRocksDbSliceArrays begin_parts(begin_count, begin);
    RustRocksDbSliceArrays end_parts(end_count, end);
    rocksdb_writebatch_delete_rangev(
        batch, begin_count, begin_parts.pointers.data(),
        begin_parts.lengths.data(), end_parts.pointers.data(),
        end_parts.lengths.data());
    RustRocksDbCheckWriteBatchCount(batch, before, errptr);
    return Status::OK();
#else
    return RustRocksDbWriteBatchRep(batch)->DeleteRange(
        RustRocksDbSliceParts(begin_count, begin),
        RustRocksDbSliceParts(end_count, end));
#endif
  });
}

extern "C" void rust_rocksdb_writebatch_delete_range_slices_cf(
    rocksdb_writebatch_t* batch,
    rocksdb_column_family_handle_t* column_family, int begin_count,
    const rocksdb_slice_t* begin, int end_count, const rocksdb_slice_t* end,
    char** errptr) {
  RustRocksDbWriteBatchCall(errptr, [&] {
#ifdef RUST_ROCKSDB_SYSTEM_BACKEND
    if (begin_count != end_count) {
      RustSaveMessage(errptr,
                      "system RocksDB requires equal range-bound part counts");
      return Status::OK();
    }
    const int before = rocksdb_writebatch_count(batch);
    RustRocksDbSliceArrays begin_parts(begin_count, begin);
    RustRocksDbSliceArrays end_parts(end_count, end);
    rocksdb_writebatch_delete_rangev_cf(
        batch, column_family, begin_count, begin_parts.pointers.data(),
        begin_parts.lengths.data(), end_parts.pointers.data(),
        end_parts.lengths.data());
    RustRocksDbCheckWriteBatchCount(batch, before, errptr);
    return Status::OK();
#else
    return RustRocksDbWriteBatchRep(batch)->DeleteRange(
        RustRocksDbColumnFamilyRep(column_family),
        RustRocksDbSliceParts(begin_count, begin),
        RustRocksDbSliceParts(end_count, end));
#endif
  });
}

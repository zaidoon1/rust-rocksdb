// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

//! Read only properties of a single SST file.
//!
//! [`TableProperties`] is the wrapper over RocksDB's `TableProperties`: the sizes, counts,
//! and names recorded when the file was written, plus anything a `TablePropertiesCollector`
//! added on top.
//!
//! You never build or own one of these. RocksDB hands out a borrowed pointer from a flush
//! job info, a compaction job info, or an external file ingestion info, and the properties
//! stay alive only as long as that event object does. The `'a` lifetime ties the wrapper
//! and every byte slice it hands back to that borrow, so nothing here outlives the callback
//! it came from.
//!
//! String-like getters return raw bytes rather than `str`. RocksDB does not guarantee UTF-8
//! for user collected properties, and the built in names are only ASCII by convention. The
//! slices point straight into the C++ strings, so reading them copies and allocates nothing.

use std::marker::PhantomData;

use libc::c_char;

use crate::ffi;
use crate::ffi_util::bytes_from_raw;

/// Shared signature of the `rocksdb_table_properties_*` string getters.
type StringGetter =
    unsafe extern "C" fn(*const ffi::rocksdb_table_properties_t, *mut usize) -> *const c_char;

/// Shared signature of the property map key and value accessors.
type MapEntryGetter = unsafe extern "C" fn(
    *const ffi::rocksdb_table_properties_t,
    usize,
    *mut usize,
) -> *const c_char;

/// Properties of one SST file, borrowed from the event that produced it.
pub struct TableProperties<'a> {
    inner: *const ffi::rocksdb_table_properties_t,
    _marker: PhantomData<&'a ()>,
}

impl<'a> TableProperties<'a> {
    /// Wraps a table properties pointer owned by RocksDB.
    ///
    /// # Safety
    ///
    /// `inner` must point to a live `rocksdb_table_properties_t` that stays valid for all of
    /// `'a`. RocksDB owns the object, so the caller must never free it and must not pick an
    /// `'a` that outlives the flush, compaction, or ingestion info it was read from.
    pub(crate) unsafe fn from_ptr(
        inner: *const ffi::rocksdb_table_properties_t,
    ) -> TableProperties<'a> {
        TableProperties {
            inner,
            _marker: PhantomData,
        }
    }

    /// File number at creation time, or 0 when unknown. When known it identifies the SST file
    /// uniquely in combination with [`Self::db_session_id`].
    pub fn orig_file_number(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_orig_file_number(self.inner) }
    }

    /// Total size of all data blocks.
    pub fn data_size(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_data_size(self.inner) }
    }

    /// Total uncompressed size of all data blocks. Recorded since RocksDB 10.7.
    pub fn uncompressed_data_size(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_uncompressed_data_size(self.inner) }
    }

    /// Size of the index block.
    pub fn index_size(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_index_size(self.inner) }
    }

    /// Number of index partitions, set only when the two level index search is used.
    pub fn index_partitions(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_index_partitions(self.inner) }
    }

    /// Size of the top level index, set only when the two level index search is used.
    pub fn top_level_index_size(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_top_level_index_size(self.inner) }
    }

    /// Whether index keys are plain user keys. When false they also carry the 8 byte sequence
    /// number of the internal key format.
    pub fn index_key_is_user_key(&self) -> bool {
        unsafe { ffi::rocksdb_table_properties_index_key_is_user_key(self.inner) != 0 }
    }

    /// Whether index values are delta encoded.
    pub fn index_value_is_delta_encoded(&self) -> bool {
        unsafe { ffi::rocksdb_table_properties_index_value_is_delta_encoded(self.inner) != 0 }
    }

    /// Whether the UDI is the primary index for reads. The standard index is still fully
    /// populated alongside it.
    pub fn udi_is_primary_index(&self) -> bool {
        unsafe { ffi::rocksdb_table_properties_udi_is_primary_index(self.inner) != 0 }
    }

    /// Size of the filter block.
    pub fn filter_size(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_filter_size(self.inner) }
    }

    /// Total key size before compression and block encoding.
    pub fn raw_key_size(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_raw_key_size(self.inner) }
    }

    /// Total value size before compression and block encoding.
    pub fn raw_value_size(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_raw_value_size(self.inner) }
    }

    /// Number of data blocks in this file.
    pub fn num_data_blocks(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_num_data_blocks(self.inner) }
    }

    /// Data blocks stored uncompressed because the compressed output blew past the ratio limit
    /// in `CompressionOptions::max_compressed_bytes_per_kb`.
    pub fn num_data_blocks_compression_rejected(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_num_data_blocks_compression_rejected(self.inner) }
    }

    /// Data blocks stored uncompressed because compression was never attempted, for example
    /// with `kNoCompression` or with no compressor available.
    pub fn num_data_blocks_compression_bypassed(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_num_data_blocks_compression_bypassed(self.inner) }
    }

    /// Number of uniform blocks in this file.
    pub fn num_uniform_blocks(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_num_uniform_blocks(self.inner) }
    }

    /// Number of entries in this file.
    pub fn num_entries(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_num_entries(self.inner) }
    }

    /// Number of unique entries, keys or prefixes, added to the filter.
    pub fn num_filter_entries(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_num_filter_entries(self.inner) }
    }

    /// Number of deletions in this file.
    pub fn num_deletions(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_num_deletions(self.inner) }
    }

    /// Number of merge operands in this file.
    pub fn num_merge_operands(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_num_merge_operands(self.inner) }
    }

    /// Number of range deletions in this file.
    pub fn num_range_deletions(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_num_range_deletions(self.inner) }
    }

    /// SST format version, reserved for backward compatibility.
    pub fn format_version(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_format_version(self.inner) }
    }

    /// Byte length shared by every key, or 0 when keys are variable length.
    pub fn fixed_key_len(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_fixed_key_len(self.inner) }
    }

    /// Id of the column family this file belongs to, matching [`Self::column_family_name`]. An
    /// unknown column family reads back as `i32::MAX`.
    pub fn column_family_id(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_column_family_id(self.inner) }
    }

    /// Oldest ancestor time, 0 when unknown. For a flush this is the oldest key time in the
    /// file, falling back to the flush time. For a compaction it is the oldest such time across
    /// all input files, falling back to when this output file was created.
    pub fn creation_time(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_creation_time(self.inner) }
    }

    /// Timestamp of the earliest key, 0 when unknown.
    pub fn oldest_key_time(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_oldest_key_time(self.inner) }
    }

    /// Timestamp of the newest key, 0 when unknown.
    pub fn newest_key_time(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_newest_key_time(self.inner) }
    }

    /// Time the SST file was actually created, 0 when unknown.
    pub fn file_creation_time(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_file_creation_time(self.inner) }
    }

    /// Estimated size of the data blocks under a relatively slower compression algorithm, 0
    /// when unknown. Comes from `ColumnFamilyOptions::sample_for_compression`.
    pub fn slow_compression_estimated_data_size(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_slow_compression_estimated_data_size(self.inner) }
    }

    /// Estimated size of the data blocks under a relatively faster compression algorithm, 0
    /// when unknown. Comes from `ColumnFamilyOptions::sample_for_compression`.
    pub fn fast_compression_estimated_data_size(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_fast_compression_estimated_data_size(self.inner) }
    }

    /// Offset within the file of the external SST file global seqno value, 0 when the file has
    /// no such property.
    pub fn external_sst_file_global_seqno_offset(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_external_sst_file_global_seqno_offset(self.inner) }
    }

    /// Offset where the tail of the file begins, meaning every block after the data blocks.
    pub fn tail_start_offset(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_tail_start_offset(self.inner) }
    }

    /// Value of `AdvancedColumnFamilyOptions::persist_user_defined_timestamps` when the file
    /// was written. Defaults to true and is only recorded in the file when false.
    pub fn user_defined_timestamps_persisted(&self) -> bool {
        unsafe { ffi::rocksdb_table_properties_user_defined_timestamps_persisted(self.inner) != 0 }
    }

    /// Largest sequence number among the keys in this file. Only meaningful when
    /// [`Self::has_key_largest_seqno`] is true, otherwise it reads back as `u64::MAX`.
    pub fn key_largest_seqno(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_key_largest_seqno(self.inner) }
    }

    /// Smallest sequence number among the keys in this file. Only meaningful when
    /// [`Self::has_key_smallest_seqno`] is true, otherwise it reads back as `u64::MAX`.
    pub fn key_smallest_seqno(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_key_smallest_seqno(self.inner) }
    }

    /// Whether [`Self::key_largest_seqno`] holds a real sequence number. It should be true
    /// unless the file is empty.
    pub fn has_key_largest_seqno(&self) -> bool {
        unsafe { ffi::rocksdb_table_properties_has_key_largest_seqno(self.inner) != 0 }
    }

    /// Whether [`Self::key_smallest_seqno`] holds a real sequence number. It should be true
    /// unless the file is empty.
    pub fn has_key_smallest_seqno(&self) -> bool {
        unsafe { ffi::rocksdb_table_properties_has_key_smallest_seqno(self.inner) != 0 }
    }

    /// Restart interval used for data blocks when the file was written, 0 when unknown.
    pub fn data_block_restart_interval(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_data_block_restart_interval(self.inner) }
    }

    /// Restart interval used for index blocks when the file was written, 0 when unknown.
    pub fn index_block_restart_interval(&self) -> u64 {
        unsafe { ffi::rocksdb_table_properties_index_block_restart_interval(self.inner) }
    }

    /// Whether data blocks store keys and values separately. The block footer is the real
    /// source of truth, this property exists for debugging and validation.
    pub fn separate_key_value_in_data_block(&self) -> bool {
        unsafe { ffi::rocksdb_table_properties_separate_key_value_in_data_block(self.inner) != 0 }
    }

    /// DB identity, generated the first time the DB was created. Empty when unset.
    pub fn db_id(&self) -> &'a [u8] {
        self.string_field(ffi::rocksdb_table_properties_db_id)
    }

    /// DB session identity, regenerated every time the DB is opened. Empty when unset.
    pub fn db_session_id(&self) -> &'a [u8] {
        self.string_field(ffi::rocksdb_table_properties_db_session_id)
    }

    /// Location of the machine hosting the DB, the hostname by default. It can change whenever
    /// the DB is reopened.
    pub fn db_host_id(&self) -> &'a [u8] {
        self.string_field(ffi::rocksdb_table_properties_db_host_id)
    }

    /// Name of the column family this file belongs to. Empty when the column family is unknown.
    pub fn column_family_name(&self) -> &'a [u8] {
        self.string_field(ffi::rocksdb_table_properties_column_family_name)
    }

    /// Name of the filter policy used for this file. Empty when no filter policy was used.
    pub fn filter_policy_name(&self) -> &'a [u8] {
        self.string_field(ffi::rocksdb_table_properties_filter_policy_name)
    }

    /// Name of the comparator used for this file.
    pub fn comparator_name(&self) -> &'a [u8] {
        self.string_field(ffi::rocksdb_table_properties_comparator_name)
    }

    /// Name of the merge operator used for this file. Reads back as `nullptr` when no merge
    /// operator was used.
    pub fn merge_operator_name(&self) -> &'a [u8] {
        self.string_field(ffi::rocksdb_table_properties_merge_operator_name)
    }

    /// Name of the prefix extractor used for this file. Reads back as `nullptr` when no prefix
    /// extractor was used.
    pub fn prefix_extractor_name(&self) -> &'a [u8] {
        self.string_field(ffi::rocksdb_table_properties_prefix_extractor_name)
    }

    /// Comma separated names of the property collector factories used for this file.
    pub fn property_collectors_names(&self) -> &'a [u8] {
        self.string_field(ffi::rocksdb_table_properties_property_collectors_names)
    }

    /// Identifies the compression algorithm or schema used for this file. Below format version
    /// 7 it is a built in compression type name, from version 7 on it is
    /// `<compatibility_name>;<hex coded compression types>;<future use>`.
    pub fn compression_name(&self) -> &'a [u8] {
        self.string_field(ffi::rocksdb_table_properties_compression_name)
    }

    /// Compression options used to compress this file.
    pub fn compression_options(&self) -> &'a [u8] {
        self.string_field(ffi::rocksdb_table_properties_compression_options)
    }

    /// Delta encoded sequence number to time mapping.
    pub fn seqno_to_time_mapping(&self) -> &'a [u8] {
        self.string_field(ffi::rocksdb_table_properties_seqno_to_time_mapping)
    }

    /// Number of user collected properties recorded for this file.
    pub fn user_collected_properties_count(&self) -> usize {
        unsafe { ffi::rocksdb_table_properties_user_collected_properties_count(self.inner) }
    }

    /// The user collected property at `pos` in key order, or `None` once `pos` runs past the
    /// end.
    ///
    /// Costs O(pos): the C API walks the underlying `std::map` from the beginning on every
    /// call, so random access here is not cheap.
    pub fn user_collected_property_at(&self, pos: usize) -> Option<(&'a [u8], &'a [u8])> {
        self.map_entry_at(
            pos,
            ffi::rocksdb_table_properties_user_collected_properties_key_at,
            ffi::rocksdb_table_properties_user_collected_properties_value_at,
        )
    }

    /// Walks the user collected properties in key order, borrowing every key and value.
    ///
    /// Lazy and allocation free, but each step costs O(pos) because the C API walks the map
    /// from the beginning for every lookup. A full pass over n properties is therefore O(n^2),
    /// which is fine for the handful of entries most collectors emit and slow if you have
    /// thousands.
    pub fn user_collected_properties(&self) -> impl Iterator<Item = (&'a [u8], &'a [u8])> + '_ {
        (0..self.user_collected_properties_count())
            .map_while(move |pos| self.user_collected_property_at(pos))
    }

    /// Number of human readable properties recorded for this file. These are what collectors
    /// return from `GetReadableProperties` and exist for logging.
    pub fn readable_properties_count(&self) -> usize {
        unsafe { ffi::rocksdb_table_properties_readable_properties_count(self.inner) }
    }

    /// The readable property at `pos` in key order, or `None` once `pos` runs past the end.
    ///
    /// Costs O(pos): the C API walks the underlying `std::map` from the beginning on every
    /// call, so random access here is not cheap.
    pub fn readable_property_at(&self, pos: usize) -> Option<(&'a [u8], &'a [u8])> {
        self.map_entry_at(
            pos,
            ffi::rocksdb_table_properties_readable_properties_key_at,
            ffi::rocksdb_table_properties_readable_properties_value_at,
        )
    }

    /// Walks the human readable properties in key order, borrowing every key and value.
    ///
    /// Lazy and allocation free, but each step costs O(pos) because the C API walks the map
    /// from the beginning for every lookup. A full pass over n properties is therefore O(n^2),
    /// which is fine for the handful of entries most collectors emit and slow if you have
    /// thousands.
    pub fn readable_properties(&self) -> impl Iterator<Item = (&'a [u8], &'a [u8])> + '_ {
        (0..self.readable_properties_count()).map_while(move |pos| self.readable_property_at(pos))
    }

    /// Reads one of the borrowed string fields as raw bytes.
    fn string_field(&self, getter: StringGetter) -> &'a [u8] {
        let mut len: usize = 0;
        // SAFETY: `self.inner` is valid for `'a` and the getter writes the byte length through
        // `len`, returning an interior pointer into a string RocksDB owns.
        unsafe {
            let ptr = getter(self.inner, &raw mut len);
            bytes_from_raw(ptr, len)
        }
    }

    /// Reads one key and value pair out of a property map, or `None` when `pos` is out of range.
    ///
    /// The C API signals out of range with a null key pointer, and the value pointer of an
    /// entry that exists is never null even when the value is empty.
    fn map_entry_at(
        &self,
        pos: usize,
        key_at: MapEntryGetter,
        value_at: MapEntryGetter,
    ) -> Option<(&'a [u8], &'a [u8])> {
        let mut key_len: usize = 0;
        let mut value_len: usize = 0;
        // SAFETY: `self.inner` is valid for `'a`, both getters bounds check `pos` themselves,
        // and they write the byte lengths through the out params they are given.
        unsafe {
            let key_ptr = key_at(self.inner, pos, &raw mut key_len);
            if key_ptr.is_null() {
                return None;
            }
            let value_ptr = value_at(self.inner, pos, &raw mut value_len);
            Some((
                bytes_from_raw(key_ptr, key_len),
                bytes_from_raw(value_ptr, value_len),
            ))
        }
    }
}

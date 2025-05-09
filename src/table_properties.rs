use std::ffi::{c_char, c_void, CStr, CString};
use std::marker::PhantomData;
use std::mem;
use std::slice;

use libc::{c_int, size_t};

use crate::{ffi, Options};

/// Extension trait for [`Options`] to register table properties collectors
pub trait TablePropertiesExt {
    fn add_table_properties_collector_factory<F>(&mut self, factory: F)
    where
        F: TablePropertiesCollectorFactory + Send + 'static;
}

impl TablePropertiesExt for Options {
    fn add_table_properties_collector_factory<F>(&mut self, factory: F)
    where
        F: TablePropertiesCollectorFactory + Send + 'static,
    {
        unsafe {
            let factory_ptr = Box::into_raw(Box::new(factory)) as *mut c_void;

            // Takes ownership of the collector factory; the Rust callback wrapper will be
            // dropped via the destructor callback
            ffi::rocksdb_options_add_table_properties_collector_factory(
                self.inner,
                factory_ptr,
                Some(TablePropertiesCollectorFactoryCallback::<F>::destructor),
                Some(TablePropertiesCollectorFactoryCallback::<F>::name),
                Some(TablePropertiesCollectorFactoryCallback::<F>::create_collector),
            );
        }
    }
}

#[repr(C)]
pub enum EntryType {
    EntryPut,
    EntryDelete,
    EntrySingleDelete,
    EntryMerge,
    EntryRangeDeletion,
    EntryBlobIndex,
    EntryDeleteWithTimestamp,
    EntryWideColumnEntity,
    EntryTimedPut,
    EntryOther,
}

pub struct TablePropertiesCollectorContext {
    pub column_family_id: u32,
    pub level_at_creation: i32,
    pub num_levels: i32,
    pub last_level_inclusive_max_seqno_threshold: u64,
}

/// Table properties collector factory trait
pub trait TablePropertiesCollectorFactory {
    type Collector: TablePropertiesCollector;

    /// Create a new table properties collector
    fn create(&mut self, context: TablePropertiesCollectorContext) -> Self::Collector;

    /// Name of the collector factory to use for logging
    fn name(&self) -> &CStr;
}

/// Table properties collector trait
pub trait TablePropertiesCollector {
    /// Called when a new key/value pair is added to the table
    ///
    /// Returning `Err` will cause an error to be logged but otherwise continue building the
    /// table as normal.
    fn add_user_key(
        &mut self,
        key: &[u8],
        value: &[u8],
        entry_type: EntryType,
        seq: u64,
        file_size: u64,
    ) -> Result<(), CollectorError>;

    /// Called after each new block is cut
    fn block_add(
        &mut self,
        _block_uncompressed_bytes: u64,
        _block_compressed_bytes_fast: u64,
        _block_compressed_bytes_slow: u64,
    ) {
    }

    /// Called once when a table has been built and is ready for writing the properties block
    ///
    /// When the result is `Err`, the collected properties will not be written to the file's
    /// property block.
    fn finish(&mut self) -> Result<impl IntoIterator<Item = &(CString, CString)>, CollectorError>;

    /// Returns human-readable properties used for logging
    ///
    /// This method will be called after finish() has been called.
    fn get_readable_properties(&self) -> impl IntoIterator<Item = &(CString, CString)>;

    /// Name of the collector to use for logging
    fn name(&self) -> &CStr;
}

/// Collector error
///
/// This error is not meaningfully used by RocksDB, other than to indicate an unsuccessful callback.
/// It intentionally does not accept a message or other detail as these would be ignored downstream.
#[derive(Debug, Default)]
pub struct CollectorError {
    _private: (),
}

impl std::fmt::Display for CollectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Property collector error")
    }
}

struct TablePropertiesCollectorFactoryCallback<F>
where
    F: TablePropertiesCollectorFactory + Send + 'static,
{
    _phantom: PhantomData<F>,
}

impl<F> TablePropertiesCollectorFactoryCallback<F>
where
    F: TablePropertiesCollectorFactory + Send + 'static,
{
    unsafe extern "C" fn create_collector(
        raw_self: *mut c_void,
        ctx: *mut ffi::rocksdb_table_properties_collector_context_t,
    ) -> *mut ffi::rocksdb_table_properties_collector_t {
        let context = TablePropertiesCollectorContext {
                column_family_id: ffi::rocksdb_table_properties_collector_context_get_column_family_id(ctx),
                level_at_creation: ffi::rocksdb_table_properties_collector_context_get_level_at_creation(ctx),
                num_levels: ffi::rocksdb_table_properties_collector_context_get_num_levels(ctx),
                last_level_inclusive_max_seqno_threshold:
                    ffi::rocksdb_table_properties_collector_context_get_last_level_inclusive_max_seqno_threshold(ctx),
            };

        let factory: &mut F = &mut *(raw_self.cast());
        let collector = Box::new(factory.create(context));

        ffi::rocksdb_table_properties_collector_create(
            Box::into_raw(collector).cast(),
            Some(TablePropertiesCollectorCallback::<F::Collector>::destructor),
            Some(TablePropertiesCollectorCallback::<F::Collector>::add_user_key),
            Some(TablePropertiesCollectorCallback::<F::Collector>::block_add),
            Some(TablePropertiesCollectorCallback::<F::Collector>::finish),
            Some(TablePropertiesCollectorCallback::<F::Collector>::get_readable_properties),
            Some(TablePropertiesCollectorCallback::<F::Collector>::name),
        )
    }

    unsafe extern "C" fn name(raw_self: *mut c_void) -> *const c_char {
        let factory = &*(raw_self.cast_const() as *const F);
        factory.name().as_ptr()
    }

    unsafe extern "C" fn destructor(raw_self: *mut c_void) {
        drop(Box::from_raw(raw_self as *mut F));
    }
}

struct TablePropertiesCollectorCallback<C>
where
    C: TablePropertiesCollector,
{
    _marker: PhantomData<C>,
}

impl<C> TablePropertiesCollectorCallback<C>
where
    C: TablePropertiesCollector,
{
    unsafe extern "C" fn destructor(raw_collector: *mut c_void) {
        drop(Box::from_raw(raw_collector as *mut C));
    }

    unsafe extern "C" fn name(raw_collector: *mut c_void) -> *const c_char {
        let collector: &mut C = &mut *(raw_collector.cast());
        collector.name().as_ptr()
    }

    unsafe extern "C" fn add_user_key(
        raw_collector: *mut c_void,
        key_ptr: *const c_char,
        key_len: size_t,
        value_ptr: *const c_char,
        value_len: size_t,
        entry_type: c_int,
        seq: u64,
        file_size: u64,
    ) -> bool {
        let collector: &mut C = &mut *(raw_collector.cast());

        let key = slice::from_raw_parts(key_ptr as *const u8, key_len);
        let value = slice::from_raw_parts(value_ptr as *const u8, value_len);
        let entry_type = mem::transmute::<c_int, EntryType>(entry_type);

        collector
            .add_user_key(key, value, entry_type, seq, file_size)
            .is_ok()
    }

    unsafe extern "C" fn block_add(
        raw_collector: *mut c_void,
        block_uncompressed_bytes: u64,
        block_compressed_bytes_fast: u64,
        block_compressed_bytes_slow: u64,
    ) {
        let collector: &mut C = &mut *(raw_collector.cast());
        collector.block_add(
            block_uncompressed_bytes,
            block_compressed_bytes_fast,
            block_compressed_bytes_slow,
        );
    }

    unsafe extern "C" fn finish(
        raw_collector: *mut c_void,
        user_collected_properties: *mut ffi::rocksdb_user_collected_properties_t,
    ) -> bool {
        let collector: &mut C = &mut *(raw_collector.cast());

        let Ok(props) = collector.finish() else {
            // An error will be logged by RocksDB to its own log, though the details will be swallowed.
            // Property collectors should perform their own logging via other mechanisms if required.
            return false;
        };

        for (key, value) in props {
            ffi::rocksdb_user_collected_properties_insert(
                user_collected_properties,
                key.as_ref().as_ptr(),
                value.as_ref().as_ptr(),
            );
        }

        true
    }

    unsafe extern "C" fn get_readable_properties(
        raw_collector: *mut c_void,
        user_collected_properties: *mut ffi::rocksdb_user_collected_properties_t,
    ) {
        let collector: &mut C = &mut *(raw_collector.cast());
        let props = collector.get_readable_properties();

        for (key, value) in props {
            ffi::rocksdb_user_collected_properties_insert(
                user_collected_properties,
                key.as_ref().as_ptr(),
                value.as_ref().as_ptr(),
            );
        }
    }
}

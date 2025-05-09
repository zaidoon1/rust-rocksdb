// Copyright 2025 Restate Software, Inc., Restate GmbH
//
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

use std::ffi::CStr;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::Arc;

use libc::c_void;

use crate::ffi::rocksdb_flushjobinfo_t;
use crate::ffi_util::from_cstr;
use crate::{ffi, CStrLike, Options};

/// EventListener defining a set of callbacks from RocksDB
///
/// The EventListener trait contains a set of callback functions that will
/// be called when a specific RocksDB event happens such as flush. It can
/// be used as a building block for developing custom features.
///
/// ## Important
///
/// Callback functions should not run for an extended period before they
/// return as this will slow down RocksDB.
///
/// ## Threading
///
/// All EventListener callback will be called using the
/// actual thread that involves in that specific event. For example, it
/// is the RocksDB background flush thread that does the actual flush to
/// call EventListener::OnFlushCompleted().
///
/// ## Locking
///
/// All EventListener callbacks are designed to be called without
/// the current thread holding any DB mutex. This is to prevent potential
/// deadlock and performance issue when using EventListener callbacks
/// in a complex way.
pub trait EventListener {
    /// A callback function to RocksDB which will be called whenever a
    /// registered RocksDB flushes a file. The default implementation is
    /// no-op.
    ///
    /// Note that the this function must be implemented in a way such that
    /// it should not run for an extended period of time before the function
    /// returns.  Otherwise, RocksDB may be blocked.
    fn on_flush_completed(&self, _flush_job_info: FlushJobInfo) {}
}

impl<T: EventListener + ?Sized> EventListener for Arc<T> {
    fn on_flush_completed(&self, flush_job_info: FlushJobInfo) {
        (**self).on_flush_completed(flush_job_info);
    }
}

impl<T: EventListener + ?Sized> EventListener for Box<T> {
    fn on_flush_completed(&self, flush_job_info: FlushJobInfo) {
        (**self).on_flush_completed(flush_job_info);
    }
}

pub trait EventListenerExt {
    /// Adds an EventListener whose callback functions will be called
    /// when a specific RocksDB event happens.
    fn add_event_listener<T>(&mut self, listener: T)
    where
        T: EventListener + Send + Sync + 'static;
}

impl EventListenerExt for Options {
    fn add_event_listener<T>(&mut self, listener: T)
    where
        T: EventListener + Send + Sync + 'static,
    {
        unsafe {
            let cb = Box::new(EventListenerCallback::new(listener));
            let cb_ptr = Box::into_raw(cb) as *mut c_void;

            let event_listener_ptr = ffi::rocksdb_event_listener_create(
                cb_ptr,
                Some(EventListenerCallback::<T>::destructor),
            );

            ffi::rocksdb_event_listener_set_on_flush_completed(
                event_listener_ptr,
                Some(EventListenerCallback::<T>::on_flush_completed),
            );

            // Takes ownership of the event listener; the Rust callback wrapper will be
            // dropped via the destructor callback
            ffi::rocksdb_options_add_event_listener(self.inner, event_listener_ptr);
        }
    }
}

/// Flush information
#[derive(Debug)]
pub struct FlushJobInfo<'a> {
    /// The name of the column family
    pub cf_name: String,
    /// The path to the newly created file
    pub file_path: String,
    /// The smallest sequence number in the newly created file
    pub smallest_seqno: u64,
    /// The largest sequence number in the newly created file
    pub largest_seqno: u64,
    /// Flush reason
    pub flush_reason: FlushReason,

    // holds a pointer to data borrowed from RocksDB for the duration of the event handler callback;
    // we are responsible for freeing the wrapper struct
    table_properties: NonNull<ffi::rocksdb_table_properties_t>,

    _marker: PhantomData<&'a ()>,
}

/// Flush reason
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum FlushReason {
    Others = 0,
    GetLiveFiles = 1,
    ShutDown = 2,
    ExternalFileIngestion = 3,
    ManualCompaction = 4,
    WriteBufferManager = 5,
    WriteBufferFull = 6,
    Test = 7,
    DeleteFiles = 8,
    AutoCompaction = 9,
    ManualFlush = 10,
    ErrorRecovery = 11,
    ErrorRecoveryRetryFlush = 12,
    WalFull = 13,
    CatchUpAfterErrorRecovery = 14,
}

impl Drop for FlushJobInfo<'_> {
    fn drop(&mut self) {
        unsafe {
            ffi::rocksdb_table_properties_destroy(self.table_properties.as_ptr());
        }
    }
}

impl FlushJobInfo<'_> {
    pub fn get_user_collected_property(&self, key: impl CStrLike) -> Option<&CStr> {
        unsafe {
            let key_cstring = key.into_c_string().unwrap();
            let value_ptr = ffi::rocksdb_table_properties_get_user_collected_property(
                self.table_properties.as_ptr(),
                key_cstring.as_ptr(),
            );

            if value_ptr.is_null() {
                return None;
            }

            let value_string = CStr::from_ptr(value_ptr);
            Some(value_string)
        }
    }

    pub fn get_user_collected_property_keys(&self, prefix: impl CStrLike) -> Vec<&CStr> {
        unsafe {
            let mut key_count: usize = 0;
            let prefix = prefix.into_c_string().unwrap();
            let keys_ptr = ffi::rocksdb_table_properties_get_user_collected_property_keys(
                self.table_properties.as_ptr(),
                prefix.as_ptr(),
                &mut key_count,
            );

            if keys_ptr.is_null() {
                return Vec::new();
            }

            let mut result = Vec::with_capacity(key_count);
            for i in 0..key_count {
                let key_ptr = *keys_ptr.add(i);
                if !key_ptr.is_null() {
                    result.push(CStr::from_ptr(key_ptr));
                }
            }
            ffi::rocksdb_free(keys_ptr as *mut c_void);

            result
        }
    }
}

pub(crate) struct EventListenerCallback<T>
where
    T: EventListener,
{
    listener: T,
}

impl<T> EventListenerCallback<T>
where
    T: EventListener,
{
    pub fn new(listener: T) -> Self {
        Self { listener }
    }

    pub unsafe extern "C" fn destructor(raw_cb: *mut c_void) {
        drop(Box::from_raw(raw_cb as *mut Self));
    }

    pub unsafe extern "C" fn on_flush_completed(
        raw_cb: *mut c_void,
        flush_job_info: *const rocksdb_flushjobinfo_t,
    ) {
        let cf_name_ptr = ffi::rocksdb_flushjobinfo_cf_name(flush_job_info);
        let cf_name = if cf_name_ptr.is_null() {
            String::new()
        } else {
            from_cstr(cf_name_ptr)
        };

        let file_path_ptr = ffi::rocksdb_flushjobinfo_file_path(flush_job_info);
        let file_path = if file_path_ptr.is_null() {
            String::new()
        } else {
            from_cstr(file_path_ptr)
        };

        let smallest_seqno = ffi::rocksdb_flushjobinfo_smallest_seqno(flush_job_info);
        let largest_seqno = ffi::rocksdb_flushjobinfo_largest_seqno(flush_job_info);
        let flush_reason = std::mem::transmute::<libc::c_int, FlushReason>(
            ffi::rocksdb_flushjobinfo_flushreason(flush_job_info),
        );

        let table_properties = NonNull::new(unsafe {
            ffi::rocksdb_flushjobinfo_table_properties(flush_job_info).cast_mut()
        })
        .unwrap();

        let flush_job_info = FlushJobInfo {
            cf_name,
            file_path,
            smallest_seqno,
            largest_seqno,
            flush_reason,
            table_properties,
            _marker: PhantomData,
        };

        let cb = &mut *(raw_cb as *mut Self);
        cb.listener.on_flush_completed(flush_job_info);
    }
}

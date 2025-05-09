mod util;

use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

use rust_rocksdb::event_listener::{EventListener, EventListenerExt, FlushJobInfo};
use rust_rocksdb::table_properties::{
    CollectorError, EntryType, TablePropertiesCollector, TablePropertiesCollectorContext,
    TablePropertiesCollectorFactory, TablePropertiesExt,
};
use rust_rocksdb::{Options, DB};
use util::DBPath;

#[test]
fn test_table_properties_collector() {
    let path = DBPath::new("_rust_rocksdb_properties_collector_test");

    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);

    // Event listeners are DB-wide
    let listener = Arc::new(CustomPropertyListener {
        state: RwLock::new(ObservedProperties::default()),
    });
    opts.add_event_listener(listener.clone());

    let mut cf_opts = Options::default();
    let match_count = Arc::new(AtomicU32::new(0));
    let error_count = Arc::new(AtomicU32::new(0));
    let collector_factory = KeyStartsWithACollectorFactory {
        name: c"AKeyCounterFactory".to_owned(),
        match_count: Arc::clone(&match_count),
        error_count: Arc::clone(&error_count),
    };
    cf_opts.add_table_properties_collector_factory(collector_factory);

    let db = DB::open_cf_with_opts(&opts, &path, [("cf", cf_opts)]).unwrap();

    let cf = db.cf_handle("cf").unwrap();
    db.put_cf(&cf, b"a", b"foo").unwrap();
    assert_eq!(0, listener.state.read().flush_count);
    assert_eq!(None, listener.state.read().latest_persisted_key_count);
    assert_eq!(None, listener.state.read().all_keys);

    db.flush_cf(&cf).unwrap();
    assert_eq!(1, listener.state.read().flush_count);
    assert_eq!(
        Some(c"1".to_owned()),
        listener.state.read().latest_persisted_key_count
    );
    let all_user_keys = listener.state.read().all_keys.clone().unwrap();
    assert!(all_user_keys.contains(&c"key_count".to_owned()));

    db.put_cf(&cf, b"aaa", b"foo").unwrap();
    db.put_cf(&cf, b"bbb", b"bar").unwrap();
    db.put_cf(&cf, b"AAA", b"baz").unwrap();

    assert_eq!(1, listener.state.read().flush_count);
    assert_eq!(
        Some(c"1".to_owned()),
        listener.state.read().latest_persisted_key_count
    );

    db.flush_cf(&cf).unwrap();
    assert_eq!(2, listener.state.read().flush_count);
    assert_eq!(
        Some(c"3".to_owned()),
        listener.state.read().latest_persisted_key_count
    );

    db.put_cf(&cf, b"c", b"").unwrap();
    db.flush_cf(&cf).unwrap();
    assert_eq!(3, listener.state.read().flush_count);
    assert_eq!(
        Some(c"3".to_owned()),
        listener.state.read().latest_persisted_key_count
    );

    // Returning an error from the collector
    assert_eq!(0, error_count.load(Ordering::Relaxed));
    db.put_cf(&cf, b"error", b"test").unwrap();
    db.flush_cf(&cf).unwrap();
    assert_eq!(4, listener.state.read().flush_count);
    assert_eq!(None, listener.state.read().latest_persisted_key_count);
    assert_eq!(1, error_count.load(Ordering::Relaxed));
}

struct KeyStartsWithACollectorFactory {
    name: CString,
    match_count: Arc<AtomicU32>,
    error_count: Arc<AtomicU32>,
}

impl TablePropertiesCollectorFactory for KeyStartsWithACollectorFactory {
    type Collector = KeyStartsWithACollector;

    fn create(&mut self, _context: TablePropertiesCollectorContext) -> Self::Collector {
        KeyStartsWithACollector {
            match_count: Arc::clone(&self.match_count),
            error_count: Arc::clone(&self.error_count),
            encountered_error: false,
            props: vec![],
        }
    }

    fn name(&self) -> &CStr {
        &self.name
    }
}

#[derive(Debug)]
struct KeyStartsWithACollector {
    match_count: Arc<AtomicU32>,
    error_count: Arc<AtomicU32>,
    props: Vec<(CString, CString)>,
    encountered_error: bool,
}

impl TablePropertiesCollector for KeyStartsWithACollector {
    fn add_user_key(
        &mut self,
        key: &[u8],
        _value: &[u8],
        entry_type: EntryType,
        _seq: u64,
        _file_size: u64,
    ) -> Result<(), CollectorError> {
        if let EntryType::EntryPut = entry_type {
            if key.starts_with(b"err") {
                self.error_count.fetch_add(1, Ordering::Relaxed);
                self.encountered_error = true;
                return Err(CollectorError::default());
            }

            if key.starts_with(b"a") || key.starts_with(b"A") {
                self.match_count.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<impl IntoIterator<Item = &(CString, CString)>, CollectorError> {
        if self.encountered_error {
            return Err(CollectorError::default());
        }

        self.props.push((
            c"key_count".to_owned(),
            CString::new(self.match_count.load(Ordering::Relaxed).to_string()).unwrap(),
        ));

        Ok(self.props.iter())
    }

    fn get_readable_properties(&self) -> impl IntoIterator<Item = &(CString, CString)> {
        self.props.iter()
    }

    fn name(&self) -> &CStr {
        c"AKeyCounter"
    }
}

struct CustomPropertyListener {
    state: RwLock<ObservedProperties>,
}

#[derive(Default)]
struct ObservedProperties {
    flush_count: u32,
    latest_persisted_key_count: Option<CString>,
    all_keys: Option<Vec<CString>>,
}

impl EventListener for CustomPropertyListener {
    fn on_flush_completed(&self, info: FlushJobInfo) {
        let mut guard = self.state.write();
        guard.flush_count += 1;
        guard.latest_persisted_key_count = info
            .get_user_collected_property("key_count")
            .map(|cstr| cstr.to_owned());
        guard.all_keys = Some(
            info.get_user_collected_property_keys(c"key_")
                .iter()
                .map(|&cstr| cstr.to_owned())
                .collect(),
        );
    }
}

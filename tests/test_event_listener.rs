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

mod util;

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use rust_rocksdb::event_listener::{EventListener, EventListenerExt, FlushJobInfo, FlushReason};
use rust_rocksdb::{Options, DB};
use util::DBPath;

struct TestListener {
    counters: RwLock<HashMap<String, u32>>,
}

impl TestListener {
    fn new() -> Self {
        TestListener {
            counters: RwLock::new(HashMap::new()),
        }
    }
}

impl EventListener for TestListener {
    fn on_flush_completed(&self, info: FlushJobInfo) {
        assert_eq!(FlushReason::ManualFlush, info.flush_reason);
        let mut counters = self.counters.write();
        let cf_entry = counters.entry(info.cf_name.clone()).or_insert(0);
        *cf_entry += 1;
    }
}

#[test]
pub fn test_event_listener() {
    const PATH_PREFIX: &str = "_rust_rocksdb_event_listener_";

    let db_path = DBPath::new(&format!("{PATH_PREFIX}db1"));

    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);

    let listener = Arc::new(TestListener::new());

    opts.add_event_listener(listener.clone());
    assert_eq!(0, listener.counters.read().len());

    let db = DB::open_cf_with_opts(
        &opts,
        &db_path,
        vec![("cf1", Options::default()), ("cf2", Options::default())],
    )
    .unwrap();

    let cf1 = db.cf_handle("cf1").unwrap();
    db.put_cf(&cf1, b"k1", b"").unwrap();
    db.flush_cf(&cf1).unwrap();
    assert_eq!(
        1,
        listener
            .counters
            .read()
            .get("cf1")
            .copied()
            .unwrap_or_default()
    );
    assert_eq!(1, listener.counters.read().len());

    db.put_cf(&cf1, b"k2", b"").unwrap();
    db.flush_cf(&cf1).unwrap();
    assert_eq!(
        2,
        listener
            .counters
            .read()
            .get("cf1")
            .copied()
            .unwrap_or_default()
    );

    db.put_cf(&cf1, "k3", b"").unwrap();
    db.flush_cf(&cf1).unwrap();
    db.flush_cf(&cf1).unwrap(); // no-op
    db.flush_cf(&cf1).unwrap(); // no-op
    assert_eq!(
        3,
        listener
            .counters
            .read()
            .get("cf1")
            .copied()
            .unwrap_or_default()
    );
    assert_eq!(1, listener.counters.read().len());

    let cf2 = db.cf_handle("cf2").unwrap();
    db.put_cf(&cf2, b"k4", b"").unwrap();
    db.flush_cf(&cf2).unwrap();
    assert_eq!(
        3,
        listener
            .counters
            .read()
            .get("cf1")
            .copied()
            .unwrap_or_default()
    );
    assert_eq!(
        1,
        listener
            .counters
            .read()
            .get("cf2")
            .copied()
            .unwrap_or_default()
    );
    assert_eq!(2, listener.counters.read().len());
}

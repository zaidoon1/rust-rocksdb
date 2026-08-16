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

mod util;

use rust_rocksdb::compaction_service::{
    CompactionService, CompactionServiceJobInfo, CompactionServiceJobStatus,
    CompactionServiceOptionsOverride, OpenAndCompactCancellationToken, OpenAndCompactOptions,
    ScheduleResponse, open_and_compact, open_and_compact_with_options,
};
use rust_rocksdb::{DB, Env, Options};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use util::DBPath;

/// What one `schedule` call reported, so a test can assert on the job RocksDB
/// described and not just on the bytes it handed over.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedJob {
    cf_name: Vec<u8>,
    cf_id: u32,
    db_name: Vec<u8>,
    output_level: i32,
    is_manual: bool,
    input_len: usize,
}

/// How the worker side of a round trip should behave, so one harness covers the
/// happy path and each failure mode.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkerBehavior {
    /// Compact for real, with an override straight out of
    /// [`CompactionServiceOptionsOverride::create`].
    Compact,
    /// Compact for real, with an override built from `Options` and then adjusted
    /// through several setters.
    CompactWithConfiguredOverride,
    /// Compact under a token that is already cancelled.
    CompactCancelled,
    /// Refuse at schedule time and let the primary do the work.
    RefuseWithUseLocal,
    /// Accept the job, then report failure without producing a result.
    FailInWait,
}

/// Everything the test wants to read back, kept behind an `Arc` because
/// `Options::set_compaction_service` takes ownership of the service itself.
#[derive(Default)]
struct WorkerState {
    next_job_id: AtomicUsize,
    /// Serialized input for each job id still in flight.
    pending: Mutex<HashMap<String, Vec<u8>>>,
    jobs: Mutex<Vec<RecordedJob>>,
    waits: AtomicUsize,
    installs: Mutex<Vec<Option<CompactionServiceJobStatus>>>,
    cancels: AtomicUsize,
    /// Whatever the worker entry point complained about.
    worker_errors: Mutex<Vec<String>>,
}

impl WorkerState {
    fn recorded_jobs(&self) -> Vec<RecordedJob> {
        self.jobs.lock().unwrap().clone()
    }

    fn wait_count(&self) -> usize {
        self.waits.load(Ordering::SeqCst)
    }

    fn cancel_count(&self) -> usize {
        self.cancels.load(Ordering::SeqCst)
    }

    fn install_statuses(&self) -> Vec<Option<CompactionServiceJobStatus>> {
        self.installs.lock().unwrap().clone()
    }

    fn worker_errors(&self) -> Vec<String> {
        self.worker_errors.lock().unwrap().clone()
    }
}

/// A compaction service that runs the remote side in-process.
///
/// `wait` is where the work happens, which is what RocksDB expects: it calls
/// `wait` on a background compaction thread and blocks that thread until the job
/// finishes. Compacting inline there keeps the whole round trip on one machine
/// while still going through the real serialize, hand off, `open_and_compact`,
/// install path.
struct LocalWorkerService {
    name: CString,
    /// Source DB the worker reopens read only, which is the directory the primary
    /// has open. A real worker would fetch this.
    db_path: PathBuf,
    /// Parent for the per-job output directories.
    work_root: PathBuf,
    behavior: WorkerBehavior,
    state: Arc<WorkerState>,
}

impl LocalWorkerService {
    fn new(db_path: &DBPath, work_root: &DBPath, behavior: WorkerBehavior) -> Self {
        Self {
            name: CString::new("local-worker").unwrap(),
            db_path: db_path.as_ref().to_path_buf(),
            work_root: work_root.as_ref().to_path_buf(),
            behavior,
            state: Arc::new(WorkerState::default()),
        }
    }

    /// Handle for the assertions, taken before the service is handed to `Options`.
    fn state(&self) -> Arc<WorkerState> {
        Arc::clone(&self.state)
    }

    /// A fresh empty directory per job, because `open_and_compact` requires an
    /// empty output directory unless resumption is allowed.
    fn output_dir_for(&self, job_id: &str) -> PathBuf {
        let dir = self.work_root.join(format!("job_{job_id}"));
        std::fs::create_dir_all(&dir).expect("could not create the job output directory");
        dir
    }

    /// The worker's own view of how to open the column family.
    fn override_options(&self) -> CompactionServiceOptionsOverride {
        if self.behavior != WorkerBehavior::CompactWithConfiguredOverride {
            // Deliberately the bare constructor. This is the path that would hand
            // RocksDB a null table factory if `create` did not fill one in.
            return CompactionServiceOptionsOverride::create();
        }

        let mut options = Options::default();
        options.create_if_missing(true);
        let mut override_options = CompactionServiceOptionsOverride::from_options(&options);
        override_options.set_env(&Env::new().expect("could not create an Env"));
        override_options
            .set_option("write_buffer_size", "65536")
            .expect("write_buffer_size is a real option");
        override_options
    }

    /// Runs the compaction the way this behavior asks for.
    fn run_job(&self, job_id: &str, input: &[u8]) -> Result<Vec<u8>, rust_rocksdb::Error> {
        let output_dir = self.output_dir_for(job_id);
        let override_options = self.override_options();

        if self.behavior == WorkerBehavior::CompactCancelled {
            let token = Arc::new(OpenAndCompactCancellationToken::new());
            token.cancel();
            let mut options = OpenAndCompactOptions::default();
            options.set_canceled(token);
            return open_and_compact_with_options(
                &options,
                &self.db_path,
                &output_dir,
                input,
                &override_options,
            );
        }

        open_and_compact(&self.db_path, &output_dir, input, &override_options)
    }
}

impl CompactionService for LocalWorkerService {
    fn name(&self) -> &CStr {
        &self.name
    }

    fn schedule(&self, info: &CompactionServiceJobInfo<'_>, input: &[u8]) -> ScheduleResponse {
        self.state.jobs.lock().unwrap().push(RecordedJob {
            cf_name: info.cf_name().to_vec(),
            cf_id: info.cf_id(),
            db_name: info.db_name().to_vec(),
            output_level: info.output_level(),
            is_manual: info.is_manual_compaction(),
            input_len: input.len(),
        });

        if self.behavior == WorkerBehavior::RefuseWithUseLocal {
            return ScheduleResponse::from_status(CompactionServiceJobStatus::UseLocal);
        }

        let job_id = self
            .state
            .next_job_id
            .fetch_add(1, Ordering::SeqCst)
            .to_string();
        // The input is only borrowed for this call, so it has to be copied.
        self.state
            .pending
            .lock()
            .unwrap()
            .insert(job_id.clone(), input.to_vec());

        ScheduleResponse::scheduled(job_id.as_str(), CompactionServiceJobStatus::Success)
            .expect("a decimal job id has no interior NUL")
    }

    fn wait(&self, scheduled_job_id: &CStr, result: &mut Vec<u8>) -> CompactionServiceJobStatus {
        self.state.waits.fetch_add(1, Ordering::SeqCst);
        let job_id = scheduled_job_id
            .to_str()
            .expect("the job id round trips unchanged")
            .to_owned();

        let input = self
            .state
            .pending
            .lock()
            .unwrap()
            .remove(&job_id)
            .expect("wait should only run for a job that was scheduled");

        if self.behavior == WorkerBehavior::FailInWait {
            return CompactionServiceJobStatus::Failure;
        }

        match self.run_job(&job_id, &input) {
            Ok(bytes) => {
                assert!(
                    !bytes.is_empty(),
                    "a successful compaction should return a serialized result"
                );
                *result = bytes;
                CompactionServiceJobStatus::Success
            }
            Err(err) => {
                self.state
                    .worker_errors
                    .lock()
                    .unwrap()
                    .push(err.into_string());
                CompactionServiceJobStatus::Failure
            }
        }
    }

    fn cancel_awaiting_jobs(&self) {
        self.state.cancels.fetch_add(1, Ordering::SeqCst);
    }

    fn on_installation(
        &self,
        _scheduled_job_id: &CStr,
        status: Option<CompactionServiceJobStatus>,
    ) {
        self.state.installs.lock().unwrap().push(status);
    }
}

const BATCHES: u32 = 4;
const KEYS_PER_BATCH: u32 = 40;

/// Options that make a manual compaction produce a real remote job.
fn primary_options() -> Options {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    // Keep automatic compaction out of the way so the manual one is the only job.
    opts.set_disable_auto_compactions(true);
    opts
}

/// Opens a primary DB wired to a fresh worker, and returns the shared state.
fn open_with_worker(
    path: &DBPath,
    work: &DBPath,
    behavior: WorkerBehavior,
) -> (DB, Arc<WorkerState>) {
    let service = LocalWorkerService::new(path, work, behavior);
    let state = service.state();
    let mut opts = primary_options();
    opts.set_compaction_service(service);
    (DB::open(&opts, path).unwrap(), state)
}

/// `AsRef<Path>` is implemented for `&DBPath` rather than `DBPath`, so the
/// reference has to be bound before it can be converted.
fn path_of(path: &DBPath) -> PathBuf {
    path.as_ref().to_path_buf()
}

fn key_of(key: u32) -> String {
    format!("key{key:04}")
}

fn value_of(batch: u32, key: u32) -> String {
    format!("value-{batch}-{key}")
}

/// Writes several flushed SSTs that all cover the same key range.
///
/// The overlap matters. If each flushed file held a disjoint range, RocksDB would
/// satisfy a manual compaction with a trivial move, no compaction job would run,
/// and the compaction service would never be consulted.
fn seed_overlapping_sst_files(db: &DB) {
    for batch in 0..BATCHES {
        for key in 0..KEYS_PER_BATCH {
            db.put(key_of(key).as_bytes(), value_of(batch, key).as_bytes())
                .unwrap();
        }
        db.flush().unwrap();
    }
}

/// Every key reads back the value the last batch wrote, which is what merging
/// those overlapping files has to produce.
fn assert_all_keys_readable(db: &DB) {
    for key in 0..KEYS_PER_BATCH {
        let k = key_of(key);
        let got = db
            .get(k.as_bytes())
            .unwrap()
            .unwrap_or_else(|| panic!("{k} disappeared"));
        assert_eq!(
            got,
            value_of(BATCHES - 1, key).as_bytes(),
            "{k} came back with the wrong value"
        );
    }
}

#[test]
fn remote_compaction_round_trip_installs_the_workers_output() {
    let path = DBPath::new("_rust_rocksdb_compaction_service_round_trip");
    let work = DBPath::new("_rust_rocksdb_compaction_service_round_trip_work");

    {
        let (db, state) = open_with_worker(&path, &work, WorkerBehavior::Compact);

        seed_overlapping_sst_files(&db);
        assert_all_keys_readable(&db);

        db.compact_range(None::<&[u8]>, None::<&[u8]>);

        let jobs = state.recorded_jobs();
        assert!(
            !jobs.is_empty(),
            "the compaction service should have been asked to schedule a job"
        );
        assert_eq!(
            state.worker_errors(),
            Vec::<String>::new(),
            "the worker should not have failed"
        );
        assert_eq!(
            state.wait_count(),
            jobs.len(),
            "every scheduled job should have been waited on"
        );
        assert_eq!(
            state.install_statuses(),
            vec![Some(CompactionServiceJobStatus::Success); jobs.len()],
            "every job's output should have installed cleanly"
        );

        let job = &jobs[0];
        assert_eq!(job.cf_name, b"default");
        assert_eq!(job.cf_id, 0);
        assert!(job.is_manual, "compact_range is a manual compaction");
        assert!(
            job.output_level > 0,
            "a manual compaction off L0 should target a deeper level, got {}",
            job.output_level
        );
        assert!(
            job.input_len > 0,
            "the serialized job should carry the compaction input"
        );
        let expected_db_name = path_of(&path);
        assert_eq!(
            String::from_utf8_lossy(&job.db_name),
            expected_db_name.to_string_lossy(),
            "the job should name the primary DB"
        );

        // The data survived being compacted somewhere else.
        assert_all_keys_readable(&db);
    }

    // And it survived a reopen, so the installed files really are in the DB.
    let reopened = DB::open(&primary_options(), &path).unwrap();
    assert_all_keys_readable(&reopened);
}

#[test]
fn a_freshly_created_override_can_run_a_compaction() {
    // `CompactionServiceOptionsOverride::create` starts from a C struct whose table
    // factory is null, and the worker copies every override field over the column
    // family's options and then dereferences the table factory without checking it.
    // `create` fills that in, so passing one straight through has to work.
    let path = DBPath::new("_rust_rocksdb_compaction_service_default_override");
    let work = DBPath::new("_rust_rocksdb_compaction_service_default_override_work");

    let (db, state) = open_with_worker(&path, &work, WorkerBehavior::Compact);
    seed_overlapping_sst_files(&db);
    db.compact_range(None::<&[u8]>, None::<&[u8]>);

    assert_eq!(
        state.worker_errors(),
        Vec::<String>::new(),
        "a default override should be enough to open the column family"
    );
    assert!(state.wait_count() > 0, "the worker should have run a job");
    assert_all_keys_readable(&db);
}

#[test]
fn a_configured_override_can_run_a_compaction() {
    // Covers the other constructor and a few setters on the path that actually
    // uses them, so a setter that stored the wrong thing shows up as a failure.
    let path = DBPath::new("_rust_rocksdb_compaction_service_configured_override");
    let work = DBPath::new("_rust_rocksdb_compaction_service_configured_override_work");

    let (db, state) = open_with_worker(&path, &work, WorkerBehavior::CompactWithConfiguredOverride);
    seed_overlapping_sst_files(&db);
    db.compact_range(None::<&[u8]>, None::<&[u8]>);

    assert_eq!(
        state.worker_errors(),
        Vec::<String>::new(),
        "a configured override should still open the column family"
    );
    assert!(state.wait_count() > 0, "the worker should have run a job");
    assert_eq!(
        state.install_statuses(),
        vec![Some(CompactionServiceJobStatus::Success); state.wait_count()],
        "the output should have installed cleanly"
    );
    assert_all_keys_readable(&db);
}

#[test]
fn use_local_at_schedule_time_makes_the_primary_compact() {
    let path = DBPath::new("_rust_rocksdb_compaction_service_use_local");
    let work = DBPath::new("_rust_rocksdb_compaction_service_use_local_work");

    let (db, state) = open_with_worker(&path, &work, WorkerBehavior::RefuseWithUseLocal);
    seed_overlapping_sst_files(&db);
    db.compact_range(None::<&[u8]>, None::<&[u8]>);

    assert!(
        !state.recorded_jobs().is_empty(),
        "the service should still have been offered the job"
    );
    assert_eq!(
        state.wait_count(),
        0,
        "refusing at schedule time should not lead to a wait"
    );
    // The primary did the work itself, so nothing was lost.
    assert_all_keys_readable(&db);
}

#[test]
fn a_failing_worker_leaves_the_data_intact() {
    let path = DBPath::new("_rust_rocksdb_compaction_service_worker_failure");
    let work = DBPath::new("_rust_rocksdb_compaction_service_worker_failure_work");

    let (db, state) = open_with_worker(&path, &work, WorkerBehavior::FailInWait);
    seed_overlapping_sst_files(&db);
    // The compaction fails remotely. That comes back through the compaction's own
    // status rather than as a panic, and it must not lose data.
    db.compact_range(None::<&[u8]>, None::<&[u8]>);

    assert!(state.wait_count() > 0, "the job should have been waited on");
    assert_all_keys_readable(&db);
}

#[test]
fn a_cancelled_token_stops_the_worker_without_losing_data() {
    let path = DBPath::new("_rust_rocksdb_compaction_service_cancelled");
    let work = DBPath::new("_rust_rocksdb_compaction_service_cancelled_work");

    let (db, state) = open_with_worker(&path, &work, WorkerBehavior::CompactCancelled);
    seed_overlapping_sst_files(&db);
    db.compact_range(None::<&[u8]>, None::<&[u8]>);

    assert!(state.wait_count() > 0, "the job should have been waited on");
    let errors = state.worker_errors();
    assert!(
        !errors.is_empty(),
        "an already cancelled token should make the worker fail"
    );
    assert_all_keys_readable(&db);
}

#[test]
fn shutting_down_the_primary_tells_the_service_to_stop_waiting() {
    let path = DBPath::new("_rust_rocksdb_compaction_service_cancel_awaiting");
    let work = DBPath::new("_rust_rocksdb_compaction_service_cancel_awaiting_work");

    let state = {
        let (db, state) = open_with_worker(&path, &work, WorkerBehavior::Compact);
        seed_overlapping_sst_files(&db);
        db.compact_range(None::<&[u8]>, None::<&[u8]>);
        assert!(state.wait_count() > 0);
        state
    };

    // `cancel_awaiting_jobs` runs from `CancelAllBackgroundWork` on shutdown.
    assert!(
        state.cancel_count() > 0,
        "dropping the DB should have told the service to stop waiting"
    );
}

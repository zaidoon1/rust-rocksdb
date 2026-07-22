use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rust_rocksdb::{
    ColumnFamilyDescriptor, DB, Error, GetIntoBufferResult, IteratorMode, Options, PerfContext,
    PerfMetric, WriteBatch, properties, with_thread_local,
};
use std::{hint::black_box, io::IoSlice, ops::ControlFlow};
use tempfile::TempDir;

const ENTRY_COUNT: usize = 4_096;
const VALUE_SIZE: usize = 256;
const MULTIGET_SIZE: usize = 32;

struct Fixture {
    _dir: TempDir,
    db: DB,
    keys: Vec<Vec<u8>>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut options = Options::default();
        options.create_if_missing(true);
        let db = DB::open(&options, dir.path()).unwrap();
        let mut batch = WriteBatch::with_capacity_bytes(ENTRY_COUNT * (VALUE_SIZE + 16));
        let mut keys = Vec::with_capacity(ENTRY_COUNT);

        for index in 0..ENTRY_COUNT {
            let key = format!("key-{index:08}").into_bytes();
            let value = vec![(index % 251) as u8; VALUE_SIZE];
            batch.put(&key, value);
            keys.push(key);
        }
        db.write(&batch).unwrap();
        db.flush().unwrap();

        for key in &keys {
            black_box(db.get_pinned(key).unwrap());
        }

        Self {
            _dir: dir,
            db,
            keys,
        }
    }

    fn multiget_keys(&self) -> Vec<&[u8]> {
        self.keys[..MULTIGET_SIZE]
            .iter()
            .map(Vec::as_slice)
            .collect()
    }
}

fn point_reads(c: &mut Criterion) {
    let fixture = Fixture::new();
    let key = fixture.keys[ENTRY_COUNT / 2].as_slice();
    let mut group = c.benchmark_group("point_read");
    group.throughput(Throughput::Bytes(VALUE_SIZE as u64));

    group.bench_function("owned_vec", |b| {
        b.iter(|| black_box(fixture.db.get(black_box(key)).unwrap()))
    });
    group.bench_function("pinned", |b| {
        b.iter(|| black_box(fixture.db.get_pinned(black_box(key)).unwrap()))
    });
    group.bench_function("caller_buffer", |b| {
        let mut buffer = vec![0; VALUE_SIZE];
        b.iter(|| {
            let result = fixture
                .db
                .get_into_buffer(black_box(key), black_box(&mut buffer))
                .unwrap();
            assert_eq!(result, GetIntoBufferResult::Found(VALUE_SIZE));
            black_box(&buffer);
        })
    });
    group.finish();
}

fn snapshot_reads(c: &mut Criterion) {
    let fixture = Fixture::new();
    let snapshot = fixture.db.snapshot();
    let reusable = snapshot.read_options();
    let key = fixture.keys[ENTRY_COUNT / 2].as_slice();
    let mut group = c.benchmark_group("snapshot_point_read");

    group.bench_function("new_read_options_each_call", |b| {
        b.iter(|| black_box(snapshot.get(black_box(key)).unwrap()))
    });
    group.bench_function("reused_read_options", |b| {
        b.iter(|| black_box(reusable.get(black_box(key)).unwrap()))
    });
    for read_count in [1, 2, 4, 8, 64] {
        group.bench_with_input(
            BenchmarkId::new("new_options_batch", read_count),
            &read_count,
            |b, &read_count| {
                b.iter(|| {
                    for _ in 0..read_count {
                        black_box(snapshot.get(black_box(key)).unwrap());
                    }
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new("reused_options_batch", read_count),
            &read_count,
            |b, &read_count| {
                b.iter(|| {
                    let reads = snapshot.read_options();
                    for _ in 0..read_count {
                        black_box(reads.get(black_box(key)).unwrap());
                    }
                })
            },
        );
    }
    group.finish();
}

fn multiget(c: &mut Criterion) {
    let fixture = Fixture::new();
    let keys = fixture.multiget_keys();
    let mut group = c.benchmark_group("multiget");
    for batch_size in [1, 4, MULTIGET_SIZE] {
        let keys = &keys[..batch_size];
        group.throughput(Throughput::Elements(batch_size as u64));
        group.bench_with_input(
            BenchmarkId::new("scalar_pinned_gets", batch_size),
            keys,
            |b, keys| {
                b.iter(|| {
                    let bytes = keys
                        .iter()
                        .map(|key| {
                            fixture
                                .db
                                .get_pinned(black_box(key))
                                .unwrap()
                                .map_or(0, |value| value.len())
                        })
                        .sum::<usize>();
                    black_box(bytes)
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new("owned_values", batch_size),
            keys,
            |b, keys| {
                b.iter(|| {
                    let bytes = fixture
                        .db
                        .multi_get(black_box(keys.iter().copied()))
                        .into_iter()
                        .map(|result| result.unwrap().map_or(0, |value| value.len()))
                        .sum::<usize>();
                    black_box(bytes)
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new("individual_pinned_handles", batch_size),
            keys,
            |b, keys| {
                b.iter(|| {
                    let bytes = fixture
                        .db
                        .multi_get_pinned(black_box(keys.iter().copied()))
                        .into_iter()
                        .map(|result| result.unwrap().map_or(0, |value| value.len()))
                        .sum::<usize>();
                    black_box(bytes)
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new("batch_owned_pinned", batch_size),
            keys,
            |b, keys| {
                b.iter(|| {
                    let batch = fixture
                        .db
                        .batched_multi_get_pinned_batch(black_box(keys.iter().copied()), false)
                        .unwrap();
                    let bytes = batch
                        .iter()
                        .map(|result| result.unwrap().map_or(0, <[u8]>::len))
                        .sum::<usize>();
                    black_box(bytes)
                })
            },
        );
    }
    group.finish();
}

fn perf_context(c: &mut Criterion) {
    let mut group = c.benchmark_group("perf_context");
    group.bench_function("fresh_wrapper", |b| {
        b.iter(|| {
            let mut context = PerfContext::default();
            context.reset();
            black_box(context.metric(PerfMetric::UserKeyComparisonCount));
        })
    });
    group.bench_function("caller_reused", |b| {
        let mut context = PerfContext::default();
        b.iter(|| {
            context.reset();
            black_box(context.metric(PerfMetric::UserKeyComparisonCount));
        })
    });
    group.bench_function("thread_local_reused", |b| {
        b.iter(|| {
            with_thread_local(|context| {
                black_box(context.metric(PerfMetric::UserKeyComparisonCount))
            })
        })
    });
    group.finish();
}

fn property_reads(c: &mut Criterion) {
    let fixture = Fixture::new();
    let mut group = c.benchmark_group("integer_property");
    group.bench_function("direct_integer_api", |b| {
        b.iter(|| {
            black_box(
                fixture
                    .db
                    .property_int_value(properties::ESTIMATE_NUM_KEYS)
                    .unwrap(),
            )
        })
    });
    group.bench_function("numeric_string_fallback", |b| {
        b.iter(|| {
            black_box(
                fixture
                    .db
                    .property_int_value(properties::num_files_at_level(0))
                    .unwrap(),
            )
        })
    });
    group.bench_function("unknown_property", |b| {
        b.iter(|| {
            black_box(
                fixture
                    .db
                    .property_int_value("rocksdb.unknown-property")
                    .unwrap(),
            )
        })
    });
    group.finish();
}

fn multi_cf_iterator_creation(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let mut options = Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    let names = (0..32)
        .map(|index| format!("cf-{index:02}"))
        .collect::<Vec<_>>();
    let descriptors = names
        .iter()
        .map(|name| ColumnFamilyDescriptor::new(name, Options::default()))
        .collect::<Vec<_>>();
    let db = DB::open_cf_descriptors(&options, dir.path(), descriptors).unwrap();
    let handles = names
        .iter()
        .map(|name| db.cf_handle(name).unwrap())
        .collect::<Vec<_>>();
    let mut group = c.benchmark_group("multi_cf_iterator_creation");

    for count in [1_usize, 2, 8, 32] {
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(
            BenchmarkId::new("individual_calls", count),
            &count,
            |b, &count| {
                b.iter(|| {
                    let iterators = handles[..count]
                        .iter()
                        .map(|cf| db.raw_iterator_cf(cf))
                        .collect::<Vec<_>>();
                    black_box(iterators)
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new("single_native_call", count),
            &count,
            |b, &count| b.iter(|| black_box(db.raw_iterators_cf(handles[..count].iter()).unwrap())),
        );
    }
    group.finish();
}

fn scans(c: &mut Criterion) {
    let fixture = Fixture::new();
    let mut group = c.benchmark_group("full_scan");
    group.throughput(Throughput::Elements(ENTRY_COUNT as u64));

    group.bench_function("owned_iterator", |b| {
        b.iter(|| {
            let bytes = fixture
                .db
                .iterator(IteratorMode::Start)
                .map(|item| {
                    let (key, value) = item.unwrap();
                    key.len() + value.len()
                })
                .sum::<usize>();
            black_box(bytes)
        })
    });
    group.bench_function("borrowed_callback", |b| {
        b.iter(|| {
            let mut bytes = 0;
            let mut iterator = fixture.db.iterator(IteratorMode::Start);
            let result: Result<ControlFlow<()>, Error> = iterator.try_for_each_ref(|key, value| {
                bytes += key.len() + value.len();
                Ok(ControlFlow::Continue(()))
            });
            assert!(matches!(result, Ok(ControlFlow::Continue(()))));
            black_box(bytes)
        })
    });
    group.finish();
}

fn write_batch_assembly(c: &mut Criterion) {
    let prefix = b"tenant:";
    let suffix = b":metadata";
    let value_prefix = b"header:";
    let value_suffix = vec![7; VALUE_SIZE];
    let mut group = c.benchmark_group("write_batch_assembly");
    group.throughput(Throughput::Elements(64));

    group.bench_function("concatenated", |b| {
        b.iter(|| {
            let mut batch = WriteBatch::with_capacity_bytes(64 * (VALUE_SIZE + 32));
            for index in 0_u64..64 {
                let index = index.to_be_bytes();
                let mut key = Vec::with_capacity(prefix.len() + index.len() + suffix.len());
                key.extend_from_slice(prefix);
                key.extend_from_slice(&index);
                key.extend_from_slice(suffix);
                let mut value = Vec::with_capacity(value_prefix.len() + value_suffix.len());
                value.extend_from_slice(value_prefix);
                value.extend_from_slice(&value_suffix);
                batch.put(key, value);
            }
            black_box(batch)
        })
    });
    group.bench_function("vectored", |b| {
        b.iter(|| {
            let mut batch = WriteBatch::with_capacity_bytes(64 * (VALUE_SIZE + 32));
            for index in 0_u64..64 {
                let index = index.to_be_bytes();
                batch
                    .put_vectored(
                        &[
                            IoSlice::new(prefix),
                            IoSlice::new(&index),
                            IoSlice::new(suffix),
                        ],
                        &[IoSlice::new(value_prefix), IoSlice::new(&value_suffix)],
                    )
                    .unwrap();
            }
            black_box(batch)
        })
    });
    group.finish();
}

criterion_group!(
    benches,
    point_reads,
    snapshot_reads,
    multiget,
    perf_context,
    property_reads,
    multi_cf_iterator_creation,
    scans,
    write_batch_assembly
);
criterion_main!(benches);

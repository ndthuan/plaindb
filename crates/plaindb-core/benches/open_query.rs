use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use plaindb_core::cache::CACHE_FILE;
use plaindb_core::testgen::{COLLECTION, DB, build_store};
use plaindb_core::{Format, Query, Store};
use serde_json::{Value, json};
use tempfile::TempDir;

const COUNT: usize = 10_000;
const SEED: u64 = 7;

fn query(filter: Value) -> Query {
    Query {
        db: DB.to_owned(),
        coll: COLLECTION.to_owned(),
        filter,
        limit: Some(100),
        ..Query::default()
    }
}

fn forget_cache(root: &Path) {
    let _ = fs::remove_file(root.join(".plaindb").join(CACHE_FILE));
}

fn bench_format(criterion: &mut Criterion, name: &str, format: Format) {
    let dir = TempDir::new().expect("temp dir");
    drop(build_store(dir.path(), format, COUNT, SEED, true).expect("build"));
    let mut group = criterion.benchmark_group(name);
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("open_cold", |bencher| {
        bencher.iter_batched(
            || forget_cache(dir.path()),
            |()| black_box(Store::open(dir.path()).expect("open")),
            BatchSize::PerIteration,
        );
    });
    group.bench_function("open_warm", |bencher| {
        drop(Store::open(dir.path()).expect("open"));
        bencher.iter(|| black_box(Store::open(dir.path()).expect("open")));
    });

    let store = Store::open(dir.path()).expect("open");
    group.sample_size(50);
    group.bench_function("find_eq_indexed", |bencher| {
        bencher.iter(|| {
            black_box(
                store
                    .find(query(json!({"email": "user4242@example.test"})))
                    .expect("find"),
            )
        });
    });
    group.bench_function("count_range_indexed", |bencher| {
        bencher.iter(|| {
            black_box(
                store
                    .count(DB, COLLECTION, &json!({"age": {"$gte": 30, "$lt": 40}}))
                    .expect("count"),
            )
        });
    });
    if format == Format::Md {
        group.bench_function("text_search", |bencher| {
            bencher.iter(|| {
                black_box(
                    store
                        .find(query(json!({"$text": "harbour ledger"})))
                        .expect("find"),
                )
            });
        });
    }
    drop(store);

    let plain = TempDir::new().expect("temp dir");
    let store = build_store(plain.path(), format, COUNT, SEED, false).expect("build");
    group.bench_function("find_eq_unindexed", |bencher| {
        bencher.iter(|| {
            black_box(
                store
                    .find(query(json!({"email": "user4242@example.test"})))
                    .expect("find"),
            )
        });
    });
    group.finish();
}

fn benches(criterion: &mut Criterion) {
    bench_format(criterion, "json", Format::Json);
    bench_format(criterion, "yaml", Format::Yaml);
    bench_format(criterion, "md", Format::Md);
}

criterion_group!(open_query, benches);
criterion_main!(open_query);

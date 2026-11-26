use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use plaindb_core::testgen::{COLLECTION, DB, build_store, document, id_for};
use plaindb_core::{Durability, Format, Op, Query, Store, Tx};
use serde_json::json;
use tempfile::TempDir;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(bytes: usize) {
    let now = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

fn shrank(bytes: usize) {
    LIVE.fetch_sub(bytes, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            grew(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            grew(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        shrank(layout.size());
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(pointer, layout, size) };
        if !moved.is_null() {
            shrank(layout.size());
            grew(size);
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const COUNT: usize = 10_000;
const SEED: u64 = 7;
const WRITES: usize = 2_000;
const TOLERANCE: f64 = 1.25;
const GROWTH_AFTER_WRITES: f64 = 1.05;
const STEADY_BYTES_PER_DOCUMENT: usize = 6_600;
const WARM_OPEN_PEAK_BYTES: usize = 70 * 1024 * 1024;

fn live_since(base: usize) -> usize {
    LIVE.load(Ordering::Relaxed).saturating_sub(base)
}

fn query(store: &Store) {
    for filter in [
        json!({"status": "approved"}),
        json!({"age": {"$gte": 30, "$lt": 40}}),
        json!({"tags": {"$in": ["harbour", "ledger"]}}),
        json!({"$text": "harbour ledger"}),
        json!({"score": {"$gt": 50}}),
    ] {
        store
            .find(Query {
                db: DB.to_owned(),
                coll: COLLECTION.to_owned(),
                filter,
                limit: Some(100),
                ..Query::default()
            })
            .unwrap();
    }
}

fn rewrite(store: &Store, format: Format) {
    store.set_durability(Durability::Off).unwrap();
    for round in 0..WRITES {
        let index = round % COUNT;
        let mut doc = document(format, SEED + 1 + u64::try_from(round).unwrap(), index);
        doc.remove("_id");
        store
            .apply(Tx::new(vec![Op::Put {
                db: DB.to_owned(),
                coll: COLLECTION.to_owned(),
                id: id_for(index),
                doc,
                if_version: None,
            }]))
            .unwrap();
    }
}

fn within(measured: usize, budget: usize, tolerance: f64) -> bool {
    measured as f64 <= budget as f64 * tolerance
}

fn check(format: Format, writes: bool) {
    let dir = TempDir::new().unwrap();
    drop(build_store(dir.path(), format, COUNT, SEED, true).unwrap());
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let store = Store::open(dir.path()).unwrap();
    let open_peak = PEAK.load(Ordering::Relaxed) - base;
    query(&store);
    let steady = live_since(base);
    let per_document = steady / COUNT;
    println!("{format:?}: {per_document} bytes per document, open peak {open_peak} bytes");
    assert!(
        within(per_document, STEADY_BYTES_PER_DOCUMENT, TOLERANCE),
        "{format:?}: {per_document} bytes per document, budget {STEADY_BYTES_PER_DOCUMENT}"
    );
    assert!(
        within(open_peak, WARM_OPEN_PEAK_BYTES, TOLERANCE),
        "{format:?}: open peak {open_peak} bytes, budget {WARM_OPEN_PEAK_BYTES}"
    );
    if writes {
        rewrite(&store, format);
        query(&store);
        let written = live_since(base);
        println!("{format:?}: {written} bytes after {WRITES} writes");
        assert!(
            within(written, steady, GROWTH_AFTER_WRITES),
            "{format:?}: {written} bytes after {WRITES} writes, {steady} before"
        );
    }
}

#[test]
#[ignore = "builds ten thousand documents per format"]
fn memory_stays_within_the_budget_at_ten_thousand_documents() {
    check(Format::Json, true);
    check(Format::Yaml, false);
    check(Format::Md, false);
}

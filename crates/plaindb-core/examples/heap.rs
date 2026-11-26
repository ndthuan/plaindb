use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

use plaindb_core::cache::CACHE_FILE;
use plaindb_core::index::Indexes;
use plaindb_core::snapshot::{CollectionSnapshot, FileEntry, Outcome};
use plaindb_core::testgen::{COLLECTION, DB, build_store};
use plaindb_core::{CollectionMeta, Doc, Format, IndexSpec, Query, Store};
use rpds::RedBlackTreeMapSync;
use serde_json::json;

#[global_allocator]
static ALLOCATOR: dhat::Alloc = dhat::Alloc;

const COUNT: usize = 10_000;
const SEED: u64 = 7;
const MIB: f64 = 1024.0 * 1024.0;

fn format_named(name: &str) -> Format {
    match name {
        "json" => Format::Json,
        "yaml" => Format::Yaml,
        "md" => Format::Md,
        other => panic!("unknown format {other}"),
    }
}

fn live() -> u64 {
    dhat::HeapStats::get().curr_bytes as u64
}

fn measured<T>(build: impl FnOnce() -> T) -> u64 {
    let before = live();
    let built = build();
    let after = live();
    drop(built);
    after.saturating_sub(before)
}

fn queries(store: &Store) {
    let filters = [
        json!({"status": "approved"}),
        json!({"age": {"$gte": 30, "$lt": 40}}),
        json!({"tags": {"$in": ["harbour", "ledger"]}}),
        json!({"$text": "harbour ledger"}),
        json!({"score": {"$gt": 50}}),
    ];
    for round in 0..100 {
        store
            .find(Query {
                db: DB.to_owned(),
                coll: COLLECTION.to_owned(),
                filter: filters[round % filters.len()].clone(),
                limit: Some(100),
                ..Query::default()
            })
            .expect("find");
    }
}

fn meta_with(meta: &CollectionMeta, keep: impl Fn(&IndexSpec) -> bool) -> CollectionMeta {
    CollectionMeta {
        format: Format::Json,
        schema: None,
        indexes: meta
            .indexes
            .iter()
            .filter(|spec| keep(spec))
            .cloned()
            .collect(),
    }
}

fn text_meta(meta: &CollectionMeta) -> CollectionMeta {
    let mut text = meta_with(meta, |spec| matches!(spec, IndexSpec::Text { .. }));
    if text.indexes.is_empty() {
        text.format = meta.format;
    }
    text
}

fn breakdown(collection: &CollectionSnapshot) -> [(&'static str, u64); 4] {
    let documents = measured(|| {
        collection
            .files
            .values()
            .filter_map(|entry| match &entry.outcome {
                Outcome::Doc(found) => Some((*found.doc).clone()),
                _ => None,
            })
            .collect::<Vec<Doc>>()
    });
    let fields = measured(|| {
        let meta = meta_with(&collection.meta, |spec| {
            matches!(spec, IndexSpec::Field { .. })
        });
        Indexes::build(&meta, collection.files.values()).expect("fields")
    });
    let text = measured(|| {
        Indexes::build(&text_meta(&collection.meta), collection.files.values()).expect("text")
    });
    let files = measured(|| {
        let mut files: RedBlackTreeMapSync<String, FileEntry> = RedBlackTreeMapSync::new_sync();
        for (name, entry) in collection.files.iter() {
            files.insert_mut(name.clone(), entry.clone());
        }
        files
    });
    [
        ("documents", documents),
        ("field indexes", fields),
        ("text index", text),
        ("file entries", files),
    ]
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / MIB)
}

fn profile(root: &Path, mode: &str, output: &Path) {
    if mode == "cold" {
        let _ = fs::remove_file(root.join(".plaindb").join(CACHE_FILE));
    }
    let profiler = dhat::Profiler::builder().file_name(output).build();
    let store = Store::open(root).expect("open");
    let opened = dhat::HeapStats::get();
    queries(&store);
    let steady = live();
    let snapshot = store.snapshot();
    let collection = snapshot
        .dbs
        .get(DB)
        .and_then(|db| db.collections.get(COLLECTION))
        .expect("collection");
    let parts = breakdown(collection);
    let named: u64 = parts.iter().map(|(_, bytes)| bytes).sum();
    println!("| {mode} open peak | {} |", mib(opened.max_bytes as u64));
    println!(
        "| {mode} open peak above steady | {} |",
        mib((opened.max_bytes as u64).saturating_sub(steady))
    );
    if mode == "warm" {
        println!("| steady, all live bytes | {} |", mib(steady));
        for (name, bytes) in parts {
            println!(
                "| {name} | {} ({:.0} B per document) |",
                mib(bytes),
                bytes as f64 / COUNT as f64
            );
        }
        println!("| other | {} |", mib(steady.saturating_sub(named)));
    }
    drop(snapshot);
    drop(store);
    drop(profiler);
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if let [_, mode, root, output] = args.as_slice() {
        profile(Path::new(root), mode, Path::new(output));
        return;
    }
    let names: Vec<String> = if args.len() > 1 {
        args[1..].to_vec()
    } else {
        vec!["json".to_owned(), "md".to_owned()]
    };
    for name in names {
        let dir = tempfile::TempDir::new().expect("temp dir");
        drop(build_store(dir.path(), format_named(&name), COUNT, SEED, true).expect("build"));
        println!("\n{name}, {COUNT} documents\n\n| Heap | Bytes |\n| --- | --- |");
        for mode in ["cold", "warm"] {
            let output = env::temp_dir().join(format!("dhat-heap-{name}-{mode}.json"));
            let status = Command::new(env::current_exe().expect("exe"))
                .args([
                    mode,
                    &dir.path().to_string_lossy(),
                    &output.to_string_lossy(),
                ])
                .status()
                .expect("child");
            assert!(status.success(), "the profiling process failed");
        }
    }
}

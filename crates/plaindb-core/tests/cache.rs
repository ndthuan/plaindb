use std::fs;
use std::path::Path;

use plaindb_core::cache;
use plaindb_core::scan::full_scan;
use plaindb_core::{Dir, Doc, Format, IndexSpec, Op, Query, Store, Tx};
use plaindb_core::{Durability, StoreConfig};
use serde_json::{Value, json};
use tempfile::TempDir;

fn doc(value: Value) -> Doc {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn indexes() -> Vec<IndexSpec> {
    vec![
        IndexSpec::Field {
            field: "email".to_owned(),
            unique: true,
            multi: false,
        },
        IndexSpec::Field {
            field: "tags".to_owned(),
            unique: false,
            multi: true,
        },
        IndexSpec::Text {
            text: vec!["title".to_owned()],
        },
    ]
}

fn populate(root: &Path) {
    let store = Store::init_with(
        root,
        StoreConfig {
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.create_db("main").unwrap();
    for (name, format) in [
        ("people", Format::Json),
        ("notes", Format::Md),
        ("plans", Format::Yaml),
    ] {
        store.create_collection("main", name, format).unwrap();
        store.set_indexes("main", name, indexes()).unwrap();
        let ops = (0..60)
            .map(|n| Op::Insert {
                db: "main".to_owned(),
                coll: name.to_owned(),
                id: Some(format!("d{n:03}")),
                doc: doc(json!({
                    "email": format!("user{n}@example.com"),
                    "title": format!("report number {n} about harbour {}", n % 7),
                    "tags": [format!("t{}", n % 4), "all"],
                    "n": n,
                    "body": format!("text of {n}"),
                })),
            })
            .collect();
        store.apply(Tx::new(ops)).unwrap();
    }
}

fn find(store: &Store, coll: &str, filter: Value) -> Vec<(Doc, String)> {
    store
        .find(Query {
            db: "main".to_owned(),
            coll: coll.to_owned(),
            filter,
            sort: vec![("n".to_owned(), Dir::Asc)],
            limit: Some(1000),
            ..Query::default()
        })
        .unwrap()
        .docs
}

fn answers(store: &Store) -> Vec<Vec<(Doc, String)>> {
    let mut all = Vec::new();
    for coll in ["people", "notes", "plans"] {
        for filter in [
            json!({"email": "user7@example.com"}),
            json!({"tags": "t2"}),
            json!({"n": {"$gte": 10, "$lt": 20}}),
            json!({"$text": "harbour 3"}),
            Value::Null,
        ] {
            all.push(find(store, coll, filter));
        }
    }
    all
}

#[test]
fn cached_open_matches_a_full_scan() {
    let dir = TempDir::new().unwrap();
    populate(dir.path());
    let reference = full_scan(dir.path()).unwrap();
    let cached = cache::load(dir.path()).expect("the cache was written");
    assert_eq!(cached.generation, reference.generation);
    let from_cache = Store::open(dir.path()).unwrap();
    let from_cache_answers = answers(&from_cache);
    drop(from_cache);
    fs::remove_file(dir.path().join(".plaindb").join(cache::CACHE_FILE)).unwrap();
    let from_scan = Store::open(dir.path()).unwrap();
    assert_eq!(from_cache_answers, answers(&from_scan));
    let snapshot = from_scan.snapshot();
    for (db, db_snapshot) in reference.dbs.iter() {
        for (name, collection) in db_snapshot.collections.iter() {
            let found = &snapshot.dbs.get(db).unwrap().collections.get(name).unwrap();
            assert_eq!(found.files, collection.files);
        }
    }
}

#[test]
fn a_hand_edit_after_the_cache_was_written_is_seen() {
    let dir = TempDir::new().unwrap();
    populate(dir.path());
    assert!(cache::load(dir.path()).is_some());
    let path = dir.path().join("main/people/d005.json");
    let mut edited: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    edited["email"] = json!("edited-address@example.com");
    fs::write(&path, serde_json::to_vec_pretty(&edited).unwrap()).unwrap();
    fs::remove_file(dir.path().join("main/people/d006.json")).unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(
        find(
            &store,
            "people",
            json!({"email": "edited-address@example.com"})
        )
        .len(),
        1
    );
    assert!(find(&store, "people", json!({"email": "user5@example.com"})).is_empty());
    assert!(store.get("main", "people", "d006").unwrap().is_none());
    assert_eq!(store.list_ids("main", "people").unwrap().len(), 59);
}

#[test]
fn a_truncated_cache_is_ignored_and_replaced() {
    let dir = TempDir::new().unwrap();
    populate(dir.path());
    let path = dir.path().join(".plaindb").join(cache::CACHE_FILE);
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.list_ids("main", "notes").unwrap().len(), 60);
    assert_eq!(find(&store, "people", json!({"tags": "t1"})).len(), 15);
    assert!(cache::load(dir.path()).is_some());
}

#[test]
fn garbage_in_the_cache_is_ignored() {
    let dir = TempDir::new().unwrap();
    populate(dir.path());
    let path = dir.path().join(".plaindb").join(cache::CACHE_FILE);
    fs::write(&path, b"not a cache at all").unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.list_ids("main", "plans").unwrap().len(), 60);
    assert!(cache::load(dir.path()).is_some());
}

#[test]
fn changing_a_meta_file_invalidates_the_cache() {
    let dir = TempDir::new().unwrap();
    populate(dir.path());
    assert!(cache::load(dir.path()).is_some());
    let path = dir.path().join("main/people/_meta.json");
    let mut meta: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    meta["indexes"] = json!([{"field": "n"}]);
    fs::write(&path, serde_json::to_vec_pretty(&meta).unwrap()).unwrap();
    assert!(cache::load(dir.path()).is_none());
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(find(&store, "people", json!({"n": 12})).len(), 1);
    assert!(cache::load(dir.path()).is_some());
}

#[test]
fn a_dropped_store_whose_generation_moved_refreshes_the_cache() {
    let dir = TempDir::new().unwrap();
    populate(dir.path());
    let store = Store::open(dir.path()).unwrap();
    store
        .apply(Tx::new(vec![Op::Delete {
            db: "main".to_owned(),
            coll: "people".to_owned(),
            id: "d001".to_owned(),
            if_version: None,
        }]))
        .unwrap();
    let generation = store.snapshot().generation;
    drop(store);
    assert_eq!(cache::load(dir.path()).unwrap().generation, generation);
}

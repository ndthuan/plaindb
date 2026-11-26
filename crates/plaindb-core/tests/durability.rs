#![cfg(feature = "test-hooks")]

use std::path::Path;

use plaindb_core::{Doc, Durability, Format, Op, Store, StoreConfig, Tx, full_flush_count};
use serde_json::json;

fn store_at(root: &Path, durability: Durability) -> Store {
    let store = Store::init_with(
        root,
        StoreConfig {
            durability,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Json)
        .unwrap();
    store
}

fn put(id: &str, n: u64) -> Tx {
    let mut doc = Doc::new();
    doc.insert("n".to_owned(), json!(n));
    Tx::new(vec![Op::Put {
        db: "main".to_owned(),
        coll: "items".to_owned(),
        id: id.to_owned(),
        doc,
        if_version: None,
    }])
}

fn full_flushes_of_one_write(durability: Durability) -> u64 {
    let temp = tempfile::tempdir().unwrap();
    let store = store_at(temp.path(), durability);
    store.apply(put("warm", 0)).unwrap();
    let before = full_flush_count();
    store.apply(put("a", 1)).unwrap();
    full_flush_count() - before
}

#[test]
fn a_write_at_full_flushes_to_the_drive() {
    assert!(full_flushes_of_one_write(Durability::Full) >= 1);
}

#[test]
fn a_write_at_normal_or_off_makes_no_full_flush() {
    assert_eq!(full_flushes_of_one_write(Durability::Normal), 0);
    assert_eq!(full_flushes_of_one_write(Durability::Off), 0);
}

#[test]
fn every_level_keeps_the_written_document() {
    for durability in [Durability::Full, Durability::Normal, Durability::Off] {
        let temp = tempfile::tempdir().unwrap();
        let store = store_at(temp.path(), durability);
        store.apply(put("a", 7)).unwrap();
        drop(store);
        let opened = Store::open(temp.path()).unwrap();
        let (found, _) = opened.get("main", "items", "a").unwrap().unwrap();
        assert_eq!(found["n"], json!(7));
    }
}

#![cfg(feature = "test-hooks")]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use plaindb_core::{Doc, Durability, Format, Op, Store, StoreConfig, Tx, full_flush_count};
use serde_json::json;

fn store_at(root: &Path, durability: Durability) -> Store {
    store_with(
        root,
        StoreConfig {
            durability,
            ..StoreConfig::default()
        },
    )
}

fn store_with(root: &Path, config: StoreConfig) -> Store {
    let store = Store::init_with(root, config).unwrap();
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

fn journal_path(root: &Path) -> PathBuf {
    root.join(".plaindb").join("journal.jsonl")
}

fn journal_lines(root: &Path) -> usize {
    fs::read_to_string(journal_path(root))
        .unwrap_or_default()
        .lines()
        .count()
}

#[test]
fn a_single_document_write_at_full_makes_one_full_flush() {
    assert_eq!(full_flushes_of_one_write(Durability::Full), 1);
}

#[test]
fn a_multi_document_write_at_full_makes_one_full_flush() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_at(temp.path(), Durability::Full);
    store.apply(put("warm", 0)).unwrap();
    let mut tx = put("a", 1);
    tx.ops.extend(put("b", 2).ops);
    tx.ops.extend(put("c", 3).ops);
    let before = full_flush_count();
    store.apply(tx).unwrap();
    assert_eq!(full_flush_count() - before, 1);
}

#[test]
fn the_journal_is_checkpointed_every_interval() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with(
        temp.path(),
        StoreConfig {
            durability: Durability::Off,
            checkpoint_interval: 8,
            ..StoreConfig::default()
        },
    );
    let mut generation = store.apply(put("a", 0)).unwrap().generation;
    while !generation.is_multiple_of(8) {
        assert!(journal_lines(temp.path()) > 0);
        generation = store.apply(put("a", generation)).unwrap().generation;
    }
    assert_eq!(journal_lines(temp.path()), 0);
    store.apply(put("a", 1)).unwrap();
    assert_eq!(journal_lines(temp.path()), 1);
    let log = fs::read_to_string(temp.path().join(".plaindb").join("tx-log.jsonl")).unwrap();
    assert!(log.lines().count() >= 2);
}

#[test]
fn closing_a_store_checkpoints_its_journal() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_at(temp.path(), Durability::Off);
    let result = store.apply(put("a", 1)).unwrap();
    assert!(journal_lines(temp.path()) > 0);
    drop(store);
    assert_eq!(journal_lines(temp.path()), 0);
    let log = fs::read_to_string(temp.path().join(".plaindb").join("tx-log.jsonl")).unwrap();
    assert!(log.contains(&result.tx_id));
}

#[test]
fn an_explicit_checkpoint_empties_the_journal() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_at(temp.path(), Durability::Off);
    let first = store.apply(put("a", 1)).unwrap();
    store.checkpoint().unwrap();
    assert_eq!(journal_lines(temp.path()), 0);
    let again = store
        .apply(Tx {
            id: Some(first.tx_id.clone()),
            ..put("a", 2)
        })
        .unwrap();
    assert_eq!(again, first);
}

#[test]
fn a_torn_journal_tail_is_dropped() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_at(temp.path(), Durability::Off);
    store.apply(put("a", 1)).unwrap();
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(journal_path(temp.path()))
        .unwrap();
    file.write_all(b"0000 {\"tx_id\": \"torn").unwrap();
    drop(file);
    let other = Store::open(temp.path()).unwrap();
    assert_eq!(journal_lines(temp.path()), 0);
    other.apply(put("b", 2)).unwrap();
    store.apply(put("c", 3)).unwrap();
    drop(other);
    drop(store);
    let reopened = Store::open(temp.path()).unwrap();
    for (id, n) in [("a", 1), ("b", 2), ("c", 3)] {
        let (found, _) = reopened.get("main", "items", id).unwrap().unwrap();
        assert_eq!(found["n"], json!(n));
    }
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

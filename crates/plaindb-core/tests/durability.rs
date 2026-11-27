#![cfg(feature = "test-hooks")]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(not(target_vendor = "apple"))]
use plaindb_core::dir_fsync_count;
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

#[cfg(not(target_vendor = "apple"))]
fn dir_fsyncs_of_a_write_and_checkpoint(durability: Durability) -> u64 {
    let temp = tempfile::tempdir().unwrap();
    let store = store_at(temp.path(), durability);
    let before = dir_fsync_count();
    store.apply(put("a", 1)).unwrap();
    store.checkpoint().unwrap();
    dir_fsync_count() - before
}

#[cfg(not(target_vendor = "apple"))]
#[test]
fn directories_are_flushed_with_fsync_at_full() {
    assert!(dir_fsyncs_of_a_write_and_checkpoint(Durability::Full) >= 1);
    assert!(dir_fsyncs_of_a_write_and_checkpoint(Durability::Normal) >= 1);
    assert_eq!(dir_fsyncs_of_a_write_and_checkpoint(Durability::Off), 0);
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

fn rejected_files(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(root.join(".plaindb"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("journal.rejected-"))
        })
        .collect();
    found.sort();
    found
}

fn truncation_warnings(store: &Store) -> Vec<serde_json::Value> {
    store
        .status()
        .unwrap()
        .warnings
        .into_iter()
        .filter(|warning| warning.code == "journal_truncated")
        .map(|warning| warning.details)
        .collect()
}

#[test]
fn a_damaged_journal_record_keeps_what_follows_it() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with(
        temp.path(),
        StoreConfig {
            durability: Durability::Off,
            checkpoint_interval: 1_000,
            ..StoreConfig::default()
        },
    );
    store.checkpoint().unwrap();
    for (id, n) in [("a", 1), ("b", 2), ("c", 3)] {
        store.apply(put(id, n)).unwrap();
    }
    std::mem::forget(store);
    let bytes = fs::read(journal_path(temp.path())).unwrap();
    let lines: Vec<&[u8]> = bytes.split_inclusive(|byte| *byte == b'\n').collect();
    assert_eq!(lines.len(), 3);
    let mut damaged = lines[1].to_vec();
    damaged[0] = if damaged[0] == b'0' { b'1' } else { b'0' };
    let mut journal = lines[0].to_vec();
    journal.extend_from_slice(&damaged);
    journal.extend_from_slice(lines[2]);
    fs::write(journal_path(temp.path()), &journal).unwrap();

    let reopened = Store::open(temp.path()).unwrap();
    let (found, _) = reopened.get("main", "items", "a").unwrap().unwrap();
    assert_eq!(found["n"], json!(1));
    let rejected = rejected_files(temp.path());
    assert_eq!(rejected.len(), 1);
    let mut kept = damaged.clone();
    kept.extend_from_slice(lines[2]);
    assert_eq!(fs::read(&rejected[0]).unwrap(), kept);
    let name = rejected[0]
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        truncation_warnings(&reopened),
        vec![json!({ "file": name })]
    );
}

#[test]
fn a_torn_last_journal_line_leaves_no_rejected_file() {
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
    assert!(rejected_files(temp.path()).is_empty());
    assert!(truncation_warnings(&other).is_empty());
}

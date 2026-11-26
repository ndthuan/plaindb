#![cfg(feature = "test-hooks")]

use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use plaindb_core::{Doc, ErrorCode, Format, Op, Store, Tx, WriteLock, scan::read_generation};
use serde_json::{Value, json};

const WRITER: &str = env!("CARGO_BIN_EXE_plaindb-test-writer");

fn doc(value: Value) -> Doc {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn items_store(root: &Path) -> Store {
    let store = Store::init(root).unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Json)
        .unwrap();
    store
}

fn writer(root: &Path, mode: &str, count: u64) -> Command {
    let mut command = Command::new(WRITER);
    command
        .arg(root)
        .arg(mode)
        .arg(count.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

#[test]
fn four_processes_inserting_at_once_lose_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let store = items_store(temp.path());
    let generation = read_generation(temp.path()).unwrap();
    let children: Vec<_> = (0..4)
        .map(|_| writer(temp.path(), "insert", 500).spawn().unwrap())
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let ids = store.list_ids("main", "items").unwrap();
    assert_eq!(ids.len(), 2_000);
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 2_000);
    assert_eq!(read_generation(temp.path()).unwrap(), generation + 2_000);
    assert_eq!(store.snapshot().generation, generation + 2_000);
    let reopened = Store::open(temp.path()).unwrap();
    assert_eq!(reopened.list_ids("main", "items").unwrap(), ids);
}

#[test]
fn two_processes_patching_one_document_never_both_win() {
    let temp = tempfile::tempdir().unwrap();
    let store = items_store(temp.path());
    store
        .apply(Tx::new(vec![Op::Insert {
            db: "main".to_owned(),
            coll: "items".to_owned(),
            id: Some("shared".to_owned()),
            doc: doc(json!({"n": 0})),
        }]))
        .unwrap();
    let generation = read_generation(temp.path()).unwrap();
    let children: Vec<_> = (0..2)
        .map(|_| writer(temp.path(), "patch", 200).spawn().unwrap())
        .collect();
    let mut versions = Vec::new();
    let mut conflicts = 0;
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        for line in String::from_utf8(output.stdout).unwrap().lines() {
            match line.split_once(' ') {
                Some(("ok", version)) => versions.push(version.to_owned()),
                None if line == "conflict" => conflicts += 1,
                _ => panic!("unexpected line {line}"),
            }
        }
    }
    assert_eq!(versions.len() + conflicts, 400);
    assert_eq!(
        versions.iter().collect::<HashSet<_>>().len(),
        versions.len()
    );
    let wins = u64::try_from(versions.len()).unwrap();
    assert_eq!(read_generation(temp.path()).unwrap(), generation + wins);
    let (_, last) = store.get("main", "items", "shared").unwrap().unwrap();
    assert!(versions.contains(&last));
}

#[test]
fn a_held_lock_times_out_after_five_seconds() {
    let temp = tempfile::tempdir().unwrap();
    let store = items_store(temp.path());
    store.checkpoint().unwrap();
    let mut child = writer(temp.path(), "tx", 1)
        .env("PLAINDB_TEST_HOLD_LOCK_MS", "7000")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let journal = temp.path().join(".plaindb").join("journal.jsonl");
    let lines = || {
        std::fs::read_to_string(&journal)
            .unwrap_or_default()
            .lines()
            .count()
    };
    let waiting = Instant::now();
    while lines() == 0 {
        assert!(child.try_wait().unwrap().is_none());
        assert!(waiting.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(WriteLock::try_acquire(temp.path()).unwrap().is_none());
    let started = Instant::now();
    let error = store
        .apply(Tx::new(vec![Op::Insert {
            db: "main".to_owned(),
            coll: "items".to_owned(),
            id: Some("late".to_owned()),
            doc: Doc::new(),
        }]))
        .unwrap_err();
    let waited = started.elapsed();
    assert_eq!(error.code, ErrorCode::StoreLockedTimeout);
    assert!(waited >= Duration::from_secs(5), "{waited:?}");
    assert!(waited < Duration::from_millis(6_500), "{waited:?}");
    assert!(child.wait().unwrap().success());
    let freed = Instant::now();
    while WriteLock::try_acquire(temp.path()).unwrap().is_none() {
        assert!(freed.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(store.list_ids("main", "items").unwrap().len(), 5);
}

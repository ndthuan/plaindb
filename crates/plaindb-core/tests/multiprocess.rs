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

fn holding_writer(root: &Path, hold_ms: u64) -> std::process::Child {
    let mut child = writer(root, "tx", 1)
        .env("PLAINDB_TEST_HOLD_LOCK_MS", hold_ms.to_string())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let journal = root.join(".plaindb").join("journal.jsonl");
    let waiting = Instant::now();
    while std::fs::metadata(&journal).map_or(0, |meta| meta.len()) == 0 {
        assert!(child.try_wait().unwrap().is_none());
        assert!(waiting.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(1));
    }
    child
}

#[test]
fn a_short_lock_wait_times_out_on_time() {
    let temp = tempfile::tempdir().unwrap();
    let store = items_store(temp.path());
    store.checkpoint().unwrap();
    let mut child = holding_writer(temp.path(), 3_000);
    for _ in 0..5 {
        let started = Instant::now();
        let error = WriteLock::acquire(temp.path(), Duration::from_millis(50)).unwrap_err();
        let waited = started.elapsed();
        assert_eq!(error.code, ErrorCode::StoreLockedTimeout);
        assert!(waited >= Duration::from_millis(50), "{waited:?}");
        assert!(waited <= Duration::from_millis(300), "{waited:?}");
    }
    assert!(child.wait().unwrap().success());
}

#[test]
fn a_lock_released_while_waiting_is_taken() {
    let temp = tempfile::tempdir().unwrap();
    let store = items_store(temp.path());
    store.checkpoint().unwrap();
    let mut child = holding_writer(temp.path(), 100);
    let started = Instant::now();
    let lock = WriteLock::acquire(temp.path(), Duration::from_secs(2)).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    drop(lock);
    assert!(child.wait().unwrap().success());
}

fn seeded_values(root: &Path, count: usize) -> Store {
    let store = items_store(root);
    let ops = (0..count)
        .map(|n| Op::Insert {
            db: "main".to_owned(),
            coll: "items".to_owned(),
            id: Some(format!("d{n:02}")),
            doc: doc(json!({"v": 0})),
        })
        .collect();
    store.apply(Tx::new(ops)).unwrap();
    store
}

fn values_of(store: &Store) -> std::collections::BTreeSet<i64> {
    let snapshot = store.snapshot();
    snapshot.dbs["main"].collections["items"]
        .readable_docs()
        .map(|(_, entry)| entry.doc["v"].as_i64().unwrap())
        .collect()
}

fn wait_for_journal(root: &Path, child: &mut std::process::Child) {
    let journal = root.join(".plaindb").join("journal.jsonl");
    let waiting = Instant::now();
    while std::fs::metadata(&journal).map_or(0, |meta| meta.len()) == 0 {
        assert!(child.try_wait().unwrap().is_none());
        assert!(waiting.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn every_read_during_a_transaction_sees_all_of_it_or_none() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let store = seeded_values(root, 50);
    store.checkpoint().unwrap();
    drop(store);
    let mut child = writer(root, "put-all", 1)
        .env("PLAINDB_TEST_PAUSE_AFTER", "10:200")
        .spawn()
        .unwrap();
    wait_for_journal(root, &mut child);
    std::thread::sleep(Duration::from_millis(30));
    for _ in 0..5 {
        let reader = Store::open_reader(root).unwrap();
        let values = values_of(&reader);
        assert_eq!(values.len(), 1, "{values:?}");
        drop(reader);
        let cached = plaindb_core::cache::load(root).expect("a cache");
        let cached: std::collections::BTreeSet<i64> = cached.dbs["main"].collections["items"]
            .readable_docs()
            .map(|(_, entry)| entry.doc["v"].as_i64().unwrap())
            .collect();
        assert_eq!(cached.len(), 1, "{cached:?}");
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let values = values_of(&Store::open_reader(root).unwrap());
    assert_eq!(values, [1].into_iter().collect());
}

#[test]
fn a_reader_waits_no_longer_than_the_lock_timeout() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let store = items_store(root);
    store.set_setting("lock_timeout_ms", "100").unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let mut child = writer(root, "tx", 1)
        .env("PLAINDB_TEST_HOLD_LOCK_MS", "3000")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_journal(root, &mut child);
    let started = Instant::now();
    let error = Store::open_reader(root).unwrap_err();
    assert_eq!(error.code, ErrorCode::StoreLockedTimeout);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(child.wait().unwrap().success());
}

#[test]
fn a_push_after_a_write_does_not_hold_up_readers() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let store = Store::init_with_git(&root).unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Json)
        .unwrap();
    store.set_setting("push", "on-commit").unwrap();
    store
        .set_remote(&temp.path().join("unreachable.git").to_string_lossy())
        .unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let real = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_owned();
    let marker = temp.path().join("pushing");
    let script = temp.path().join("slow-git");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ncase \" $* \" in\n*\" push \"*) touch '{}'; sleep 2;;\nesac\nexec '{real}' \"$@\"\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut child = writer(&root, "insert", 1)
        .env("PLAINDB_GIT", &script)
        .spawn()
        .unwrap();
    let waiting = Instant::now();
    while !marker.exists() {
        assert!(child.try_wait().unwrap().is_none());
        assert!(waiting.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(5));
    }
    let started = Instant::now();
    let reader = Store::open_reader(&root).unwrap();
    assert_eq!(reader.list_ids("main", "items").unwrap().len(), 1);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    let _ = child.kill();
    child.wait().unwrap();
}

#[test]
fn a_store_whose_lock_cannot_be_opened_is_read_through_a_checked_scan() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let store = seeded_values(root, 3);
    store.checkpoint().unwrap();
    drop(store);
    let output = writer(root, "put-all", 1)
        .env("PLAINDB_TEST_CRASH_AFTER", "0")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(86));
    let state = root.join(".plaindb");
    std::fs::remove_file(state.join("lock")).unwrap();
    let mode = std::fs::metadata(&state).unwrap().permissions().mode();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(mode & !0o222)).unwrap();
    let refused = std::fs::write(state.join("probe"), b"x").is_err();
    if refused {
        let reader = Store::open_reader(root).unwrap();
        assert_eq!(values_of(&reader), [0].into_iter().collect());
        assert!(
            reader
                .status()
                .unwrap()
                .warnings
                .iter()
                .any(|warning| warning.code == "journal_unfinished")
        );
    }
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(mode)).unwrap();
}

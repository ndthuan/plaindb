#![cfg(feature = "test-hooks")]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use plaindb_core::{Doc, Format, Op, Store, Tx, TxResult, scan::read_generation};
use serde_json::{Value, json};

const WRITER: &str = env!("CARGO_BIN_EXE_plaindb-test-writer");

fn doc(value: Value) -> Doc {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn insert(id: &str, value: Value) -> Op {
    Op::Insert {
        db: "main".to_owned(),
        coll: "items".to_owned(),
        id: Some(id.to_owned()),
        doc: doc(value),
    }
}

fn seeded_store(root: &Path) -> Store {
    let store = Store::init(root).unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Json)
        .unwrap();
    store
        .apply(Tx::new(vec![
            insert("a", json!({"v": 1})),
            insert("b", json!({"v": 1})),
        ]))
        .unwrap();
    store
}

fn documents(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                if entry.file_name() != ".plaindb" {
                    pending.push(path);
                }
            } else {
                files.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    files
}

fn journal_path(root: &Path) -> PathBuf {
    root.join(".plaindb").join("journal.jsonl")
}

fn journal_records(root: &Path) -> Vec<Value> {
    fs::read_to_string(journal_path(root))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line.split_once(' ').unwrap().1).unwrap())
        .collect()
}

fn unfinished_records(root: &Path) -> Vec<Value> {
    let generation = read_generation(root).unwrap();
    journal_records(root)
        .into_iter()
        .filter(|record| record["generation"].as_u64().unwrap() > generation)
        .collect()
}

fn journal_is_empty(root: &Path) -> bool {
    fs::metadata(journal_path(root)).map_or(true, |metadata| metadata.len() == 0)
}

fn crash_writer(root: &Path, env: &[(&str, String)]) -> Command {
    let mut command = Command::new(WRITER);
    command
        .arg(root)
        .arg("crash")
        .arg("1")
        .stdout(Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    command
}

fn run(mut command: Command) -> Output {
    command.output().unwrap()
}

fn wait_for_record(root: &Path, child: &mut Child) {
    let started = Instant::now();
    while journal_records(root).is_empty() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "the writer exited before the journal appeared"
        );
        assert!(started.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn assert_committed(store: &Store) {
    for id in ["c1", "c2", "c3"] {
        let (found, _) = store.get("main", "items", id).unwrap().unwrap();
        assert_eq!(found["v"], json!(1));
    }
    let (a, _) = store.get("main", "items", "a").unwrap().unwrap();
    assert_eq!(a["v"], json!(2));
    assert!(store.get("main", "items", "b").unwrap().is_none());
}

#[test]
fn a_crash_before_the_journal_leaves_the_store_untouched() {
    let temp = tempfile::tempdir().unwrap();
    seeded_store(temp.path());
    let before = documents(temp.path());
    let generation = read_generation(temp.path()).unwrap();
    let output = run(crash_writer(
        temp.path(),
        &[("PLAINDB_TEST_CRASH_BEFORE_JOURNAL", "1".to_owned())],
    ));
    assert!(!output.status.success());
    assert!(journal_is_empty(temp.path()));
    let store = Store::open(temp.path()).unwrap();
    assert_eq!(documents(temp.path()), before);
    assert_eq!(read_generation(temp.path()).unwrap(), generation);
    assert!(store.get("main", "items", "c1").unwrap().is_none());
    assert!(store.get("main", "items", "b").unwrap().is_some());
}

#[test]
fn a_crash_after_any_number_of_files_rolls_forward_on_open() {
    for applied in 0..=5 {
        let temp = tempfile::tempdir().unwrap();
        seeded_store(temp.path());
        let generation = read_generation(temp.path()).unwrap();
        let output = run(crash_writer(
            temp.path(),
            &[("PLAINDB_TEST_CRASH_AFTER", applied.to_string())],
        ));
        assert!(!output.status.success(), "after {applied}");
        let journal = unfinished_records(temp.path()).pop().unwrap();
        let store = Store::open(temp.path()).unwrap();
        assert!(journal_is_empty(temp.path()), "after {applied}");
        assert_committed(&store);
        assert_eq!(read_generation(temp.path()).unwrap(), generation + 1);
        let state = fs::read_dir(temp.path().join(".plaindb"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.contains(".tmp-"))
            .count();
        assert_eq!(state, 0);
        let recorded: TxResult = serde_json::from_value(journal["result"].clone()).unwrap();
        let replayed = store
            .apply(Tx {
                id: Some(recorded.tx_id.clone()),
                ..Tx::new(vec![insert("c1", json!({}))])
            })
            .unwrap();
        assert_eq!(replayed, recorded);
    }
}

#[test]
fn a_long_lived_store_finishes_a_journal_left_by_a_crash() {
    let temp = tempfile::tempdir().unwrap();
    let store = seeded_store(temp.path());
    let output = run(crash_writer(
        temp.path(),
        &[("PLAINDB_TEST_CRASH_AFTER", "2".to_owned())],
    ));
    assert!(!output.status.success());
    assert_eq!(unfinished_records(temp.path()).len(), 1);
    store
        .apply(Tx::new(vec![insert("later", json!({}))]))
        .unwrap();
    assert!(unfinished_records(temp.path()).is_empty());
    assert_committed(&store);
    assert!(store.get("main", "items", "later").unwrap().is_some());
}

#[test]
fn opening_while_a_writer_holds_the_lock_leaves_its_journal_alone() {
    let temp = tempfile::tempdir().unwrap();
    seeded_store(temp.path());
    let before = documents(temp.path());
    let mut child = crash_writer(
        temp.path(),
        &[("PLAINDB_TEST_HOLD_LOCK_MS", "1500".to_owned())],
    )
    .spawn()
    .unwrap();
    wait_for_record(temp.path(), &mut child);
    let journal = fs::read(journal_path(temp.path())).unwrap();
    let store = Store::open(temp.path()).unwrap();
    assert_eq!(fs::read(journal_path(temp.path())).unwrap(), journal);
    assert_eq!(documents(temp.path()), before);
    assert!(store.get("main", "items", "c1").unwrap().is_none());
    assert!(child.wait().unwrap().success());
    assert!(journal_is_empty(temp.path()));
    assert_committed(&store);
}

fn captured_writer(root: &Path, env: &[(&str, String)]) -> Command {
    let mut command = crash_writer(root, env);
    command.stdout(Stdio::piped());
    command
}

#[test]
fn a_crash_at_any_checkpoint_step_loses_nothing() {
    for step in 0..=3 {
        let temp = tempfile::tempdir().unwrap();
        seeded_store(temp.path());
        let generation = read_generation(temp.path()).unwrap();
        let recorded = written_but_not_checkpointed(temp.path(), step);
        let store = Store::open(temp.path()).unwrap();
        assert!(journal_is_empty(temp.path()), "at step {step}");
        assert_committed(&store);
        assert_eq!(read_generation(temp.path()).unwrap(), generation + 1);
        let replayed = store
            .apply(Tx {
                id: Some(recorded.tx_id.clone()),
                ..Tx::new(vec![insert("c1", json!({}))])
            })
            .unwrap();
        assert_eq!(replayed, recorded, "at step {step}");
    }
}

fn written_but_not_checkpointed(root: &Path, step: usize) -> TxResult {
    let output = run(captured_writer(
        root,
        &[("PLAINDB_TEST_CRASH_IN_CHECKPOINT", step.to_string())],
    ));
    assert!(!output.status.success(), "at step {step}");
    let tx_id = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    assert!(unfinished_records(root).is_empty(), "at step {step}");
    let found = journal_records(root)
        .into_iter()
        .find(|record| record["tx_id"] == json!(tx_id));
    let tx_log = fs::read_to_string(root.join(".plaindb").join("tx-log.jsonl")).unwrap_or_default();
    let logged = tx_log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|record| record["tx_id"] == json!(tx_id));
    let result = found.or(logged).expect("the transaction is recorded")["result"].clone();
    serde_json::from_value(result).unwrap()
}

#[test]
fn files_lost_after_the_commit_are_written_again_on_open() {
    let temp = tempfile::tempdir().unwrap();
    seeded_store(temp.path());
    let items = temp.path().join("main").join("items");
    let old_a = fs::read(items.join("a.json")).unwrap();
    let old_b = fs::read(items.join("b.json")).unwrap();
    written_but_not_checkpointed(temp.path(), 0);
    fs::write(items.join("a.json"), &old_a).unwrap();
    fs::write(items.join("b.json"), &old_b).unwrap();
    fs::remove_file(items.join("c1.json")).unwrap();
    open_after_a_reboot(temp.path());
    let store = Store::open(temp.path()).unwrap();
    assert!(journal_is_empty(temp.path()));
    assert_committed(&store);
}

fn open_after_a_reboot(root: &Path) {
    let output = Command::new(WRITER)
        .arg(root)
        .arg("open")
        .env("PLAINDB_TEST_BOOT_ID", "another-boot")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_file_removed_by_hand_after_the_commit_stays_removed() {
    let temp = tempfile::tempdir().unwrap();
    seeded_store(temp.path());
    written_but_not_checkpointed(temp.path(), 0);
    let path = temp.path().join("main").join("items").join("c1.json");
    fs::remove_file(&path).unwrap();
    let store = Store::open(temp.path()).unwrap();
    assert!(journal_is_empty(temp.path()));
    assert!(!path.exists());
    assert!(store.get("main", "items", "c1").unwrap().is_none());
}

#[test]
fn a_hand_edit_after_the_commit_survives_the_replay() {
    let temp = tempfile::tempdir().unwrap();
    seeded_store(temp.path());
    written_but_not_checkpointed(temp.path(), 0);
    let path = temp.path().join("main").join("items").join("c1.json");
    let edited = "{\n  \"_id\": \"c1\",\n  \"v\": 9\n}\n";
    fs::write(&path, edited).unwrap();
    open_after_a_reboot(temp.path());
    let store = Store::open(temp.path()).unwrap();
    assert!(journal_is_empty(temp.path()));
    assert_eq!(fs::read_to_string(&path).unwrap(), edited);
    let (found, _) = store.get("main", "items", "c1").unwrap().unwrap();
    assert_eq!(found["v"], json!(9));
}

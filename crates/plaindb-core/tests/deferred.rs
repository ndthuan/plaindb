use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use plaindb_core::{
    CheckStatus, Doc, DoctorOptions, Durability, ErrorCode, Format, Op, Store, StoreConfig, Tx,
    new_id, scan::read_generation,
};
use serde_json::{Value, json};

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

fn store_with(root: &Path, git: bool) -> Store {
    let store = Store::init_with(
        root,
        StoreConfig {
            git,
            durability: Durability::Off,
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

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn set_writable(path: &Path, writable: bool) {
    let metadata = fs::metadata(path).unwrap();
    let mut permissions = metadata.permissions();
    let mode = permissions.mode();
    permissions.set_mode(if writable {
        mode | 0o200
    } else {
        mode & !0o222
    });
    fs::set_permissions(path, permissions).unwrap();
    if metadata.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            set_writable(&entry.unwrap().path(), writable);
        }
    }
}

fn set_dir_writable(path: &Path, writable: bool) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    let mode = permissions.mode();
    permissions.set_mode(if writable {
        mode | 0o200
    } else {
        mode & !0o222
    });
    fs::set_permissions(path, permissions).unwrap();
}

fn writes_are_refused(dir: &Path) -> bool {
    let probe = dir.join("probe");
    match fs::write(&probe, b"x") {
        Ok(()) => {
            fs::remove_file(probe).unwrap();
            false
        }
        Err(_) => true,
    }
}

fn pending_file(root: &Path) -> std::path::PathBuf {
    root.join(".plaindb").join("git-pending.json")
}

fn journal_len(root: &Path) -> u64 {
    fs::metadata(root.join(".plaindb").join("journal.jsonl")).map_or(0, |meta| meta.len())
}

#[test]
fn a_held_index_lock_does_not_fail_the_write() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = store_with(root, true);
    fs::write(root.join(".git").join("index.lock"), b"").unwrap();
    let result = store
        .apply(Tx::new(vec![insert("a2", json!({"t": 2}))]))
        .unwrap();
    assert_eq!(result.deferred, None);
    assert!(git(root, &["log", "--format=%s"]).contains("insert main/items/a2"));
    drop(store);
    let reopened = Store::open(root).unwrap();
    assert!(reopened.get("main", "items", "a2").unwrap().is_some());
}

#[test]
fn a_failed_commit_is_deferred_and_made_by_a_later_write() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = store_with(root, true);
    let objects = root.join(".git").join("objects");
    set_writable(&objects, false);
    if !writes_are_refused(&objects) {
        set_writable(&objects, true);
        return;
    }
    let tx_id = new_id();
    let outcome = store.apply(Tx {
        id: Some(tx_id.clone()),
        ..Tx::new(vec![insert("a2", json!({"t": 2}))])
    });
    let result = outcome.unwrap();
    let deferred = result.deferred.clone().expect("the git step is deferred");
    assert_eq!(deferred.step, "git");
    assert!(!deferred.message.is_empty());
    assert!(root.join("main/items/a2.json").is_file());
    drop(store);

    let reopened = Store::open(root).unwrap();
    assert!(reopened.get("main", "items", "a2").unwrap().is_some());
    let status = reopened.status().unwrap();
    let warning = status
        .warnings
        .iter()
        .find(|warning| warning.code == "git_commit_pending")
        .expect("a pending commit warning");
    assert_eq!(warning.details["transactions"], json!(1));
    assert!(warning.details["message"].as_str().is_some());
    let report = reopened.doctor(DoctorOptions { offline: true });
    let check = report
        .checks
        .iter()
        .find(|check| check.id == "git_pending_commits")
        .expect("a pending commit check");
    assert_eq!(check.status, CheckStatus::Error);
    let before = journal_len(root);
    assert!(before > 0);
    reopened.checkpoint().unwrap();
    assert_eq!(journal_len(root), before);

    let generation = read_generation(root).unwrap();
    let retried = reopened
        .apply(Tx {
            id: Some(tx_id.clone()),
            ..Tx::new(vec![insert("a2", json!({"t": 2}))])
        })
        .unwrap();
    assert_eq!(retried.tx_id, tx_id);
    assert_eq!(retried.results, result.results);
    assert_eq!(read_generation(root).unwrap(), generation);

    set_writable(&objects, true);
    reopened
        .apply(Tx::new(vec![insert("a3", json!({"t": 3}))]))
        .unwrap();
    let subjects = git(root, &["log", "--format=%s"]);
    let subjects: Vec<&str> = subjects.lines().collect();
    assert_eq!(subjects[0], "insert main/items/a3");
    assert_eq!(subjects[1], "insert main/items/a2");
    assert_eq!(
        subjects
            .iter()
            .filter(|subject| subject.contains("a2"))
            .count(),
        1
    );
    let message = git(root, &["log", "-1", "--format=%B", "HEAD~1"]);
    assert!(message.contains(&tx_id), "{message}");
    assert!(!pending_file(root).exists());
    assert!(
        !reopened
            .status()
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.code == "git_commit_pending")
    );
}

#[test]
fn a_failed_file_step_is_deferred_and_blocks_open_until_fixed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = store_with(root, false);
    let items = root.join("main").join("items");
    set_dir_writable(&items, false);
    if !writes_are_refused(&items) {
        set_dir_writable(&items, true);
        return;
    }
    let result = store
        .apply(Tx::new(vec![insert("a2", json!({"t": 2}))]))
        .unwrap();
    let deferred = result.deferred.expect("the file step is deferred");
    assert_eq!(deferred.step, "files");
    drop(store);

    let error = Store::open(root).unwrap_err();
    assert_eq!(error.code, ErrorCode::Io);
    assert_eq!(error.details["reason"], json!("recovery_failed"));

    set_dir_writable(&items, true);
    let reopened = Store::open(root).unwrap();
    assert!(reopened.get("main", "items", "a2").unwrap().is_some());
}

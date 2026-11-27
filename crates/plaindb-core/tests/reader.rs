#![cfg(feature = "test-hooks")]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use plaindb_core::{Doc, Durability, ErrorCode, Format, Op, Store, StoreConfig, Tx};
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

fn store_with(root: &Path, git: bool) -> Store {
    let store = Store::init_with(
        root,
        StoreConfig {
            git,
            durability: Durability::Off,
            checkpoint_interval: 1_000,
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

fn files_under(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    let mut files = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                pending.push(path);
            } else {
                let modified = entry.metadata().unwrap().modified().unwrap();
                files.insert(path.clone(), (fs::read(&path).unwrap(), modified));
            }
        }
    }
    files
}

fn set_tree_writable(path: &Path, writable: bool) {
    let metadata = fs::symlink_metadata(path).unwrap();
    if metadata.file_type().is_symlink() {
        return;
    }
    let mut permissions = metadata.permissions();
    let mode = permissions.mode();
    permissions.set_mode(if writable {
        mode | 0o200
    } else {
        mode & !0o222
    });
    if writable {
        fs::set_permissions(path, permissions.clone()).unwrap();
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            set_tree_writable(&entry.unwrap().path(), writable);
        }
    }
    if !writable {
        fs::set_permissions(path, permissions).unwrap();
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
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

fn journal(root: &Path) -> PathBuf {
    root.join(".plaindb").join("journal.jsonl")
}

#[test]
fn a_reader_leaves_a_settled_journal_and_the_repository_alone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = store_with(root, true);
    store
        .apply(Tx::new(vec![insert("a", json!({"n": 1}))]))
        .unwrap();
    std::mem::forget(store);
    let journal_before = fs::read(journal(root)).unwrap();
    assert!(!journal_before.is_empty());
    let git_before = files_under(&root.join(".git"));
    let reader = Store::open_reader(root).unwrap();
    assert!(reader.get("main", "items", "a").unwrap().is_some());
    drop(reader);
    assert_eq!(fs::read(journal(root)).unwrap(), journal_before);
    assert_eq!(files_under(&root.join(".git")), git_before);
}

#[test]
fn a_reader_refuses_to_write() {
    let dir = tempfile::tempdir().unwrap();
    drop(store_with(dir.path(), false));
    let reader = Store::open_reader(dir.path()).unwrap();
    let error = reader
        .apply(Tx::new(vec![insert("a", json!({"n": 1}))]))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidQuery);
    assert!(dir.path().join("main/items").read_dir().unwrap().count() == 1);
}

#[test]
fn a_reader_finishes_the_files_of_a_crashed_write_when_it_can() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = store_with(&root, false);
    store
        .apply(Tx::new(vec![
            insert("a", json!({"v": 1})),
            insert("b", json!({"v": 1})),
        ]))
        .unwrap();
    drop(store);
    let output = Command::new(WRITER)
        .arg(&root)
        .arg("crash")
        .arg("1")
        .env("PLAINDB_TEST_CRASH_AFTER", "0")
        .stdout(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(86));
    let copy = dir.path().join("copy");
    copy_tree(&root, &copy);
    set_tree_writable(&copy, false);
    if writes_are_refused(&copy) {
        let stuck = Store::open_reader(&copy).unwrap();
        assert!(stuck.get("main", "items", "c1").unwrap().is_none());
        let warning = stuck
            .status()
            .unwrap()
            .warnings
            .into_iter()
            .find(|warning| warning.code == "journal_unfinished")
            .expect("an unfinished journal warning");
        assert_eq!(warning.details["records"], json!(1));
    }
    set_tree_writable(&copy, true);

    let reader = Store::open_reader(&root).unwrap();
    let (found, _) = reader.get("main", "items", "c1").unwrap().unwrap();
    assert_eq!(found["v"], json!(1));
    assert!(
        !reader
            .status()
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.code == "journal_unfinished")
    );
}

#[test]
fn a_missing_repository_is_never_made_again_by_itself() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = store_with(root, true);
    store
        .apply(Tx::new(vec![insert("a", json!({"n": 1}))]))
        .unwrap();
    drop(store);
    fs::remove_dir_all(root.join(".git")).unwrap();

    let reader = Store::open_reader(root).unwrap();
    assert!(reader.get("main", "items", "a").unwrap().is_some());
    drop(reader);
    let store = Store::open(root).unwrap();
    assert!(store.get("main", "items", "a").unwrap().is_some());
    assert!(!root.join(".git").exists());
    assert!(
        store
            .status()
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.code == "git_repository_missing")
    );
    let error = store
        .apply(Tx::new(vec![insert("b", json!({"n": 2}))]))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::GitDisabled);
    assert_eq!(error.details["reason"], json!("repository_missing"));
    assert!(
        error.message.contains("plaindb git enable"),
        "{}",
        error.message
    );
    assert!(!root.join(".git").exists());
    assert!(store.get("main", "items", "b").unwrap().is_none());

    store.enable_git().unwrap();
    assert!(root.join(".git").is_dir());
    store
        .apply(Tx::new(vec![insert("b", json!({"n": 2}))]))
        .unwrap();
    let log = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "--format=%s"])
        .output()
        .unwrap();
    let subjects = String::from_utf8(log.stdout).unwrap();
    assert_eq!(subjects, "insert main/items/b\ngit: enable\n");
}

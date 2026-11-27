use std::fs;
use std::path::Path;
use std::process::Command;

use plaindb_core::{CommitMode, Doc, ErrorCode, Format, Op, Store, Tx};
use plaindb_core::{Durability, StoreConfig};
use serde_json::{Value, json};

fn doc(value: Value) -> Doc {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
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

fn insert(id: &str, value: Value) -> Op {
    Op::Insert {
        db: "main".to_owned(),
        coll: "items".to_owned(),
        id: Some(id.to_owned()),
        doc: doc(value),
    }
}

fn manual_store(root: &Path) -> Store {
    let store = Store::init_with(
        root,
        StoreConfig {
            git: true,
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Json)
        .unwrap();
    store.set_commit_mode(CommitMode::Manual).unwrap();
    store
}

fn commit_count(root: &Path) -> usize {
    git(root, &["rev-list", "--count", "HEAD"])
        .trim()
        .parse()
        .unwrap()
}

fn trailer_values(root: &Path, key: &str) -> Vec<String> {
    git(
        root,
        &[
            "log",
            "-1",
            &format!("--format=%(trailers:key={key},valueonly)"),
        ],
    )
    .lines()
    .filter(|line| !line.is_empty())
    .map(str::to_owned)
    .collect()
}

#[test]
fn pending_transactions_become_one_manual_commit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = manual_store(root);
    let before = commit_count(root);
    assert_eq!(
        git(root, &["log", "-1", "--format=%s"]).trim(),
        "meta store: set commit"
    );
    let mut tx_ids = Vec::new();
    for n in 0..5 {
        let result = store
            .apply(Tx::new(vec![insert(&format!("d{n}"), json!({"n": n}))]))
            .unwrap();
        tx_ids.push(result.tx_id);
    }
    assert_eq!(commit_count(root), before);
    let status = store.status().unwrap().git;
    assert_eq!(status.commit, CommitMode::Manual);
    assert_eq!(status.pending_txs, 5);
    assert_eq!(status.pending_paths, 5);
    let id = store.commit(None, Some("sample-agent")).unwrap().unwrap();
    assert_eq!(git(root, &["rev-parse", "HEAD"]).trim(), id);
    assert_eq!(commit_count(root), before + 1);
    assert_eq!(
        git(root, &["log", "-1", "--format=%s"]).trim(),
        "tx: 5 pending txs"
    );
    assert_eq!(trailer_values(root, "Plaindb-Tx-Id"), tx_ids);
    assert_eq!(trailer_values(root, "Plaindb-Actor"), ["sample-agent"]);
    let body = git(root, &["log", "-1", "--format=%b"]);
    assert!(body.starts_with(&format!("{} (1 ops)\n", tx_ids[0])));
    assert_eq!(git(root, &["status", "--porcelain"]), "");
    let status = store.status().unwrap().git;
    assert_eq!(status.pending_txs, 0);
    assert_eq!(status.pending_paths, 0);
    assert_eq!(store.commit(None, None).unwrap(), None);
    assert_eq!(commit_count(root), before + 1);
}

#[test]
fn a_manual_commit_takes_the_given_message() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = manual_store(root);
    store
        .apply(Tx::new(vec![insert("a", json!({"n": 1}))]))
        .unwrap();
    store
        .commit(Some("load the first batch"), None)
        .unwrap()
        .unwrap();
    assert_eq!(
        git(root, &["log", "-1", "--format=%s"]).trim(),
        "load the first batch"
    );
}

#[test]
fn switching_modes_back_and_forth_keeps_committing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = manual_store(root);
    let pending = store
        .apply(Tx::new(vec![insert("a", json!({"n": 1}))]))
        .unwrap();
    store.set_commit_mode(CommitMode::Each).unwrap();
    assert_eq!(
        git(root, &["log", "-1", "--format=%s"]).trim(),
        "meta store: set commit"
    );
    let flushed = trailer_values(root, "Plaindb-Tx-Id");
    assert_eq!(flushed.len(), 2);
    assert_eq!(flushed[0], pending.tx_id);
    assert_eq!(git(root, &["status", "--porcelain"]), "");
    assert!(git(root, &["show", "HEAD:_store.json"]).contains("\"each\""));
    store
        .apply(Tx::new(vec![insert("b", json!({"n": 2}))]))
        .unwrap();
    assert_eq!(
        git(root, &["log", "-1", "--format=%s"]).trim(),
        "insert main/items/b"
    );
    store.set_commit_mode(CommitMode::Manual).unwrap();
    store
        .apply(Tx::new(vec![insert("c", json!({"n": 3}))]))
        .unwrap();
    assert_eq!(
        git(root, &["log", "-1", "--format=%s"]).trim(),
        "meta store: set commit"
    );
    assert_eq!(store.status().unwrap().git.pending_txs, 1);
    store.commit(None, None).unwrap().unwrap();
    assert_eq!(git(root, &["status", "--porcelain"]), "");
    assert_eq!(
        Store::open(root).unwrap().status().unwrap().git.commit,
        CommitMode::Manual
    );
}

#[test]
fn a_hand_edit_while_pending_goes_into_the_manual_commit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = manual_store(root);
    store.set_commit_mode(CommitMode::Each).unwrap();
    store
        .apply(Tx::new(vec![insert("a", json!({"n": 1}))]))
        .unwrap();
    store.set_commit_mode(CommitMode::Manual).unwrap();
    store
        .apply(Tx::new(vec![insert("b", json!({"n": 2}))]))
        .unwrap();
    fs::write(
        root.join("main/items/a.json"),
        "{\"_id\": \"a\", \"n\": 9}\n",
    )
    .unwrap();
    assert_eq!(store.status().unwrap().git.pending_paths, 2);
    store.commit(None, None).unwrap().unwrap();
    assert_eq!(
        git(root, &["diff", "--name-only", "HEAD~1", "HEAD"]),
        "main/items/a.json\nmain/items/b.json\n"
    );
    assert_eq!(git(root, &["status", "--porcelain"]), "");
    let reopened = Store::open(root).unwrap();
    assert!(
        reopened
            .status()
            .unwrap()
            .warnings
            .iter()
            .all(|warning| warning.code != "foreign_commit")
    );
    assert_eq!(
        reopened.get("main", "items", "a").unwrap().unwrap().0["n"],
        json!(9)
    );
}

#[test]
fn a_hand_edit_alone_is_committed_with_a_fresh_transaction_id() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = manual_store(root);
    store
        .apply(Tx::new(vec![insert("a", json!({"n": 1}))]))
        .unwrap();
    store.commit(None, None).unwrap().unwrap();
    fs::write(
        root.join("main/items/a.json"),
        "{\"_id\": \"a\", \"n\": 5}\n",
    )
    .unwrap();
    store.commit(None, None).unwrap().unwrap();
    assert_eq!(
        git(root, &["log", "-1", "--format=%s"]).trim(),
        "tx: 0 pending txs"
    );
    assert_eq!(trailer_values(root, "Plaindb-Tx-Id").len(), 1);
}

#[test]
fn commit_without_git_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init_with(
        dir.path(),
        StoreConfig {
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    let error = store.commit(None, None).unwrap_err();
    assert_eq!(error.code, ErrorCode::GitDisabled);
}

fn commit_by_hand(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let mut add = vec!["add", "-f", "--"];
    add.extend(files.iter().map(|(path, _)| *path));
    git(root, &add);
    git(
        root,
        &[
            "-c",
            "user.name=Sample Writer",
            "-c",
            "user.email=writer@example.com",
            "commit",
            "-q",
            "-m",
            "files by hand",
        ],
    );
}

fn tree_paths(root: &Path) -> Vec<String> {
    git(root, &["ls-tree", "-r", "--name-only", "HEAD"])
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_manual_commit_keeps_files_committed_outside_the_databases() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = manual_store(root);
    store.commit(None, None).unwrap();
    let foreign = ["scripts/run.sh", "docs/note.md", "main/.gitkeep"];
    commit_by_hand(
        root,
        &[
            (foreign[0], "echo run\n"),
            (foreign[1], "a note\n"),
            (foreign[2], ""),
        ],
    );
    store
        .apply(Tx::new(vec![insert("a", json!({"n": 1}))]))
        .unwrap();
    store.commit(None, None).unwrap().unwrap();
    let paths = tree_paths(root);
    for path in foreign {
        assert!(paths.contains(&path.to_owned()), "{path} in {paths:?}");
    }
    assert_eq!(
        git(root, &["diff", "--name-only", "HEAD~1", "HEAD"]),
        "main/items/a.json\n"
    );
}

#[test]
fn a_manual_commit_removes_every_file_of_a_dropped_database() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = manual_store(root);
    store.create_db("old").unwrap();
    store
        .create_collection("old", "things", Format::Json)
        .unwrap();
    store
        .apply(Tx::new(vec![Op::Insert {
            db: "old".to_owned(),
            coll: "things".to_owned(),
            id: Some("t".to_owned()),
            doc: doc(json!({"n": 1})),
        }]))
        .unwrap();
    store.commit(None, None).unwrap().unwrap();
    assert!(tree_paths(root).contains(&"old/_db.json".to_owned()));
    store.drop_db("old").unwrap();
    store.commit(None, None).unwrap().unwrap();
    let paths = tree_paths(root);
    assert!(
        paths.iter().all(|path| !path.starts_with("old/")),
        "{paths:?}"
    );
}

#[test]
fn a_manual_commit_removes_a_collection_deleted_by_hand() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = manual_store(root);
    store
        .create_collection("main", "gone", Format::Json)
        .unwrap();
    store
        .apply(Tx::new(vec![Op::Insert {
            db: "main".to_owned(),
            coll: "gone".to_owned(),
            id: Some("g".to_owned()),
            doc: doc(json!({"n": 1})),
        }]))
        .unwrap();
    store.commit(None, None).unwrap().unwrap();
    assert!(tree_paths(root).contains(&"main/gone/g.json".to_owned()));
    fs::remove_dir_all(root.join("main/gone")).unwrap();
    store.commit(None, None).unwrap().unwrap();
    let paths = tree_paths(root);
    assert!(
        paths.iter().all(|path| !path.starts_with("main/gone/")),
        "{paths:?}"
    );
}

#[test]
fn a_manual_commit_takes_a_changed_store_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = manual_store(root);
    store.commit(None, None).unwrap();
    store.set_setting("gc_every", "7").unwrap();
    store.commit(None, None).unwrap().unwrap();
    assert_eq!(
        git(root, &["diff", "--name-only", "HEAD~1", "HEAD"]),
        "_store.json\n"
    );
}

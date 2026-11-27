#![cfg(feature = "test-hooks")]

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use plaindb_core::{Doc, Format, Op, Store, Tx};
use plaindb_core::{Durability, StoreConfig, scan::read_generation};
use serde_json::{Value, json};

const WRITER: &str = env!("CARGO_BIN_EXE_plaindb-test-writer");

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
        .args([
            "-c",
            "user.name=Sample Writer",
            "-c",
            "user.email=writer@example.com",
        ])
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

fn assert_clean(root: &Path) {
    assert_eq!(git(root, &["status", "--porcelain"]), "");
}

fn insert(db: &str, id: &str, value: Value) -> Op {
    Op::Insert {
        db: db.to_owned(),
        coll: "items".to_owned(),
        id: Some(id.to_owned()),
        doc: doc(value),
    }
}

fn put(id: &str, value: Value) -> Op {
    Op::Put {
        db: "main".to_owned(),
        coll: "items".to_owned(),
        id: id.to_owned(),
        doc: doc(value),
        if_version: None,
    }
}

fn git_store(root: &Path) -> Store {
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
    store
}

fn subjects(root: &Path) -> Vec<String> {
    git(root, &["log", "--format=%s"])
        .lines()
        .map(str::to_owned)
        .collect()
}

fn parsed_trailers(root: &Path, rev: &str) -> Vec<String> {
    let message = git(root, &["log", "-1", "--format=%B", rev]);
    let mut child = Command::new("git")
        .args(["interpret-trailers", "--parse"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), message.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn each_transaction_becomes_one_commit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = git_store(root);
    assert_clean(root);
    store
        .apply(Tx::new(vec![insert("main", "42", json!({"n": 1}))]))
        .unwrap();
    assert_clean(root);
    store
        .apply(Tx::new(vec![
            insert("main", "43", json!({"n": 2})),
            put("42", json!({"n": 3})),
        ]))
        .unwrap();
    assert_clean(root);
    store
        .apply(Tx::new(vec![Op::Delete {
            db: "main".to_owned(),
            coll: "items".to_owned(),
            id: "43".to_owned(),
            if_version: None,
        }]))
        .unwrap();
    assert_clean(root);
    assert_eq!(
        subjects(root),
        [
            "delete main/items/43",
            "tx main: 2 ops",
            "insert main/items/42",
            "init collection main/items (json)",
            "init db main",
            "init store",
        ]
    );
    assert_eq!(
        git(root, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main"
    );
    assert_eq!(
        git(root, &["ls-files"]),
        ".gitignore\n_store.json\nmain/_db.json\nmain/items/42.json\nmain/items/_meta.json\n"
    );
    assert_eq!(
        fs::read_to_string(root.join(".gitignore")).unwrap(),
        ".plaindb/\n"
    );
}

#[test]
fn trailers_name_the_transaction_and_the_actor() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = git_store(root);
    let result = store
        .apply(Tx {
            actor: Some("sample-agent".to_owned()),
            ..Tx::new(vec![insert("main", "42", json!({"n": 1}))])
        })
        .unwrap();
    assert_eq!(
        parsed_trailers(root, "HEAD"),
        [
            format!("Plaindb-Tx-Id: {}", result.tx_id),
            "Plaindb-Actor: sample-agent".to_owned(),
            "Plaindb-Op-Count: 1".to_owned(),
        ]
    );
    for rev in ["HEAD~1", "HEAD~2", "HEAD~3"] {
        let trailers = parsed_trailers(root, rev);
        assert!(
            trailers
                .iter()
                .any(|line| line.starts_with("Plaindb-Tx-Id: ")),
            "{rev}: {trailers:?}"
        );
    }
}

#[test]
fn a_large_transaction_caps_the_body_at_fifty_lines() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = git_store(root);
    let ops = (0..60)
        .map(|n| insert("main", &format!("d{n:02}"), json!({"n": n})))
        .collect();
    store.apply(Tx::new(ops)).unwrap();
    assert_eq!(subjects(root)[0], "tx main: 60 ops");
    let body = git(root, &["log", "-1", "--format=%b"]);
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines[0], "insert main/items/d00");
    assert_eq!(lines[49], "insert main/items/d49");
    assert_eq!(lines[50], "... and 10 more");
    assert_eq!(lines[51], "");
    assert!(lines[52].starts_with("Plaindb-Tx-Id: "));
    assert_clean(root);
}

#[test]
fn a_transaction_across_dbs_names_their_count() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = git_store(root);
    store.create_db("other").unwrap();
    store
        .create_collection("other", "items", Format::Yaml)
        .unwrap();
    store
        .apply(Tx::new(vec![
            insert("main", "a", json!({"n": 1})),
            insert("other", "b", json!({"n": 2})),
        ]))
        .unwrap();
    assert_eq!(subjects(root)[0], "tx: 2 ops across 2 dbs");
    assert_eq!(subjects(root)[1], "init collection other/items (yaml)");
    assert_clean(root);
}

#[test]
fn no_document_content_reaches_a_message() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = git_store(root);
    store
        .apply(Tx::new(vec![insert(
            "main",
            "a",
            json!({"note": "unmistakable-field-value"}),
        )]))
        .unwrap();
    store
        .apply(Tx::new(vec![put(
            "a",
            json!({"note": "second-field-value"}),
        )]))
        .unwrap();
    let messages = git(root, &["log", "--format=%B"]);
    assert!(!messages.contains("unmistakable-field-value"));
    assert!(!messages.contains("second-field-value"));
    assert!(!messages.contains("note"));
}

#[test]
fn admin_operations_commit_with_their_subjects() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = git_store(root);
    store.create_collection("main", "old", Format::Md).unwrap();
    store
        .set_schema("main", "items", Some(json!({"type": "object"})))
        .unwrap();
    store.set_schema("main", "items", None).unwrap();
    store.set_indexes("main", "items", Vec::new()).unwrap();
    store.drop_collection("main", "old").unwrap();
    store.create_db("spare").unwrap();
    store.drop_db("spare").unwrap();
    assert_eq!(
        subjects(root)[..7],
        [
            "drop db spare",
            "init db spare",
            "drop collection main/old",
            "meta main/items: set indexes",
            "meta main/items: clear schema",
            "meta main/items: set schema",
            "init collection main/old (md)",
        ]
    );
    assert!(!git(root, &["ls-files"]).contains("main/old"));
    assert!(!git(root, &["ls-files"]).contains("spare"));
    assert_clean(root);
}

#[test]
fn enabling_git_commits_the_existing_store_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = Store::init_with(
        root,
        StoreConfig {
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Md)
        .unwrap();
    store
        .apply(Tx::new(vec![insert(
            "main",
            "a",
            json!({"body": "Hello\n"}),
        )]))
        .unwrap();
    fs::write(root.join("main/items/.notes.swp"), "scratch").unwrap();
    store.enable_git().unwrap();
    store.enable_git().unwrap();
    assert_eq!(subjects(root), ["git: enable"]);
    assert_eq!(
        git(root, &["ls-files"]),
        ".gitignore\n_store.json\nmain/_db.json\nmain/items/_meta.json\nmain/items/a.md\n"
    );
    assert_eq!(
        git(root, &["status", "--porcelain", "--untracked-files=no"]),
        ""
    );
    assert!(Store::open(root).unwrap().config().git);
    let trailers = parsed_trailers(root, "HEAD");
    assert!(trailers[0].starts_with("Plaindb-Tx-Id: "));
}

#[test]
fn a_hand_commit_builds_on_the_index_plaindb_keeps() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = git_store(root);
    store
        .apply(Tx::new(vec![
            insert("main", "a", json!({"n": 1})),
            insert("main", "b", json!({"n": 1})),
        ]))
        .unwrap();
    store
        .apply(Tx::new(vec![Op::Delete {
            db: "main".to_owned(),
            coll: "items".to_owned(),
            id: "b".to_owned(),
            if_version: None,
        }]))
        .unwrap();
    fs::write(root.join("main/items/a.json"), "{\"n\": 2}\n").unwrap();
    git(root, &["add", "main/items/a.json"]);
    git(root, &["commit", "-q", "-m", "edit by hand"]);
    assert_eq!(
        git(root, &["diff", "--name-status", "HEAD~1", "HEAD"]),
        "M\tmain/items/a.json\n"
    );
    assert_clean(root);
}

fn journal_tx_id(root: &Path) -> String {
    let text = fs::read_to_string(root.join(".plaindb/journal.jsonl")).unwrap();
    let last = text.lines().last().unwrap();
    let journal: Value = serde_json::from_str(last.split_once(' ').unwrap().1).unwrap();
    assert_eq!(journal["commit_pending"], json!(true));
    journal["tx_id"].as_str().unwrap().to_owned()
}

fn crash_writer(root: &Path, hook: &str) {
    let status = Command::new(WRITER)
        .arg(root)
        .arg("crash")
        .env(hook, "1")
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(86));
}

fn crash_store(root: &Path) -> Store {
    let store = git_store(root);
    store
        .apply(Tx::new(vec![
            insert("main", "a", json!({"v": 1})),
            insert("main", "b", json!({"v": 1})),
        ]))
        .unwrap();
    store
}

#[test]
fn a_crash_before_the_commit_is_committed_at_next_open() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    drop(crash_store(root));
    let before = subjects(root).len();
    crash_writer(root, "PLAINDB_TEST_CRASH_BEFORE_COMMIT");
    let tx_id = journal_tx_id(root);
    assert_eq!(subjects(root).len(), before);
    let store = Store::open(root).unwrap();
    assert_eq!(
        fs::metadata(root.join(".plaindb/journal.jsonl"))
            .unwrap()
            .len(),
        0
    );
    assert_eq!(subjects(root).len(), before + 1);
    assert_eq!(subjects(root)[0], "tx main: 5 ops");
    assert!(parsed_trailers(root, "HEAD").contains(&format!("Plaindb-Tx-Id: {tx_id}")));
    assert_clean(root);
    assert!(store.get("main", "items", "b").unwrap().is_none());
    assert!(store.get("main", "items", "c3").unwrap().is_some());
}

#[test]
fn a_crash_after_the_commit_is_not_committed_twice() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    drop(crash_store(root));
    let before = subjects(root).len();
    crash_writer(root, "PLAINDB_TEST_CRASH_AFTER_COMMIT");
    let tx_id = journal_tx_id(root);
    assert_eq!(subjects(root).len(), before + 1);
    let store = Store::open(root).unwrap();
    assert_eq!(subjects(root).len(), before + 1);
    assert!(parsed_trailers(root, "HEAD").contains(&format!("Plaindb-Tx-Id: {tx_id}")));
    assert_clean(root);
    assert!(store.status().unwrap().warnings.is_empty());
}

#[test]
fn a_store_without_git_makes_no_repository() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = Store::init_with(
        root,
        StoreConfig {
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
        .apply(Tx::new(vec![insert("main", "a", json!({"n": 1}))]))
        .unwrap();
    assert!(!root.join(".git").exists());
    assert!(!root.join(".gitignore").exists());
    let status = store.status().unwrap();
    assert!(!status.git.enabled);
    assert_eq!(status.git.head, None);
}

#[test]
fn a_store_copied_without_its_repository_starts_a_new_one_when_git_is_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    drop(git_store(root));
    fs::remove_dir_all(root.join(".git")).unwrap();
    let store = Store::open(root).unwrap();
    assert!(!root.join(".git").exists());
    let error = store
        .apply(Tx::new(vec![insert("main", "a", json!({"n": 1}))]))
        .unwrap_err();
    assert_eq!(error.details["reason"], json!("repository_missing"));
    store.enable_git().unwrap();
    assert_eq!(subjects(root), ["git: enable"]);
    store
        .apply(Tx::new(vec![insert("main", "a", json!({"n": 1}))]))
        .unwrap();
    assert_eq!(subjects(root)[0], "insert main/items/a");
    assert_clean(root);
}

fn tx_id_trailers(root: &Path) -> Vec<String> {
    git(
        root,
        &["log", "--format=%(trailers:key=Plaindb-Tx-Id,valueonly)"],
    )
    .lines()
    .filter(|line| !line.is_empty())
    .map(str::to_owned)
    .collect()
}

#[test]
fn transactions_rolled_forward_after_a_lost_generation_are_not_committed_twice() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    drop(git_store(root));
    let generation = read_generation(root).unwrap();
    let status = Command::new(WRITER)
        .arg(root)
        .arg("insert")
        .arg("3")
        .env("PLAINDB_TEST_CRASH_IN_CHECKPOINT", "0")
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(86));
    let before = subjects(root).len();
    let committed = tx_id_trailers(root);
    fs::write(root.join(".plaindb/generation"), format!("{generation}\n")).unwrap();
    let store = Store::open(root).unwrap();
    assert_eq!(subjects(root).len(), before);
    assert_eq!(tx_id_trailers(root), committed);
    assert_eq!(read_generation(root).unwrap(), generation + 3);
    assert_clean(root);
    assert!(store.status().unwrap().warnings.is_empty());
}

fn attach(id: &str, name: &str, bytes: &[u8]) -> Op {
    Op::AttachFile {
        db: "main".to_owned(),
        coll: "items".to_owned(),
        id: id.to_owned(),
        name: name.to_owned(),
        bytes: bytes.to_vec(),
        mime: None,
        if_version: None,
    }
}

fn detach(id: &str, name: &str) -> Op {
    Op::DetachFile {
        db: "main".to_owned(),
        coll: "items".to_owned(),
        id: id.to_owned(),
        name: name.to_owned(),
        if_version: None,
    }
}

fn tracked(root: &Path) -> Vec<String> {
    git(root, &["ls-tree", "-r", "--name-only", "HEAD"])
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn an_attached_file_is_committed_with_its_document() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = git_store(root);
    store
        .apply(Tx::new(vec![insert("main", "n1", json!({}))]))
        .unwrap();
    let bytes: Vec<u8> = (0..=255).collect();
    store
        .apply(Tx::new(vec![attach("n1", "a.png", &bytes)]))
        .unwrap();
    assert_clean(root);
    assert_eq!(subjects(root)[0], "attach main/items/n1 a.png");
    let changed = git(root, &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(
        changed.lines().collect::<Vec<_>>(),
        ["main/items/_files/n1/a.png", "main/items/n1.json"]
    );
    let committed = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", "HEAD:main/items/_files/n1/a.png"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(committed, bytes);

    store
        .apply(Tx::new(vec![
            attach("n1", "b.pdf", b"pdf"),
            detach("n1", "a.png"),
        ]))
        .unwrap();
    assert_clean(root);
    assert!(tracked(root).contains(&"main/items/_files/n1/b.pdf".to_owned()));
    assert!(!tracked(root).contains(&"main/items/_files/n1/a.png".to_owned()));

    store
        .apply(Tx::new(vec![Op::Delete {
            db: "main".to_owned(),
            coll: "items".to_owned(),
            id: "n1".to_owned(),
            if_version: None,
        }]))
        .unwrap();
    assert_clean(root);
    assert!(
        !tracked(root)
            .iter()
            .any(|path| path.starts_with("main/items/_files/"))
    );
}

#[test]
fn detaching_a_file_lost_from_disk_removes_it_from_git() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = git_store(root);
    store
        .apply(Tx::new(vec![
            insert("main", "n1", json!({})),
            attach("n1", "a.png", b"png"),
        ]))
        .unwrap();
    fs::remove_file(root.join("main/items/_files/n1/a.png")).unwrap();
    store.apply(Tx::new(vec![detach("n1", "a.png")])).unwrap();
    assert_clean(root);
    assert_eq!(tracked(root).len(), 5, "{:?}", tracked(root));
}

use std::fs;
use std::path::Path;
use std::process::Command;

use plaindb_core::{Doc, Format, Op, Store, Tx, Warning};
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
        .apply(Tx::new(vec![put("a", json!({"n": 1}))]))
        .unwrap();
    store
}

fn foreign_warnings(store: &Store) -> Vec<Warning> {
    store
        .status()
        .unwrap()
        .warnings
        .into_iter()
        .filter(|warning| warning.code == "foreign_commit")
        .collect()
}

fn head(root: &Path) -> String {
    git(root, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn hand_commit(root: &Path, contents: &str) -> String {
    fs::write(root.join("main/items/a.json"), contents).unwrap();
    git(root, &["commit", "-q", "-am", "edit by hand"]);
    head(root)
}

#[test]
fn a_hand_commit_is_reported_and_its_edit_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    drop(git_store(root));
    let foreign = hand_commit(root, "{\"_id\": \"a\", \"n\": 7}\n");
    let store = Store::open(root).unwrap();
    assert_eq!(
        store.get("main", "items", "a").unwrap().unwrap().0["n"],
        json!(7)
    );
    let warnings = foreign_warnings(&store);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].details["ids"], json!([foreign]));
    assert_eq!(warnings[0].details["head"], json!(foreign));
    assert!(warnings[0].details["at"].is_string());
    assert_eq!(
        fs::read_to_string(root.join(".plaindb/last-commit"))
            .unwrap()
            .trim(),
        foreign
    );
}

#[test]
fn the_warning_outlives_one_plaindb_commit_and_clears_at_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = git_store(root);
    hand_commit(root, "{\"_id\": \"a\", \"n\": 7}\n");
    let hand_version = plaindb_core::version_of(&fs::read(root.join("main/items/a.json")).unwrap());
    let result = store
        .apply(Tx::new(vec![Op::Patch {
            db: "main".to_owned(),
            coll: "items".to_owned(),
            id: "a".to_owned(),
            patch: json!({"m": 1}),
            if_version: None,
        }]))
        .unwrap();
    assert_eq!(result.results[0].previous_version, Some(hand_version));
    let (patched, _) = store.get("main", "items", "a").unwrap().unwrap();
    assert_eq!(patched["n"], json!(7));
    assert_eq!(patched["m"], json!(1));
    assert_eq!(foreign_warnings(&store).len(), 1);
    store
        .apply(Tx::new(vec![put("b", json!({"n": 1}))]))
        .unwrap();
    assert!(foreign_warnings(&store).is_empty());
    assert_eq!(git(root, &["status", "--porcelain"]), "");
}

#[test]
fn a_hand_reset_rescans_and_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = git_store(root);
    let (_, first) = store.get("main", "items", "a").unwrap().unwrap();
    store
        .apply(Tx::new(vec![put("a", json!({"n": 2}))]))
        .unwrap();
    store
        .apply(Tx::new(vec![put("c", json!({"n": 1}))]))
        .unwrap();
    git(root, &["reset", "-q", "--hard", "HEAD~2"]);
    let rewound = head(root);
    store
        .apply(Tx::new(vec![Op::Put {
            db: "main".to_owned(),
            coll: "items".to_owned(),
            id: "a".to_owned(),
            doc: doc(json!({"n": 3})),
            if_version: Some(first),
        }]))
        .unwrap();
    assert!(store.get("main", "items", "c").unwrap().is_none());
    let warnings = foreign_warnings(&store);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].details["ids"], json!([rewound]));
    assert_eq!(git(root, &["status", "--porcelain"]), "");
    let reopened = Store::open(root).unwrap();
    assert_eq!(
        reopened.get("main", "items", "a").unwrap().unwrap().0["n"],
        json!(3)
    );
    assert!(reopened.get("main", "items", "c").unwrap().is_none());
}

#[test]
fn plaindb_commits_from_another_process_are_not_foreign() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let first = git_store(root);
    let second = Store::open(root).unwrap();
    second
        .apply(Tx::new(vec![put("b", json!({"n": 1}))]))
        .unwrap();
    first
        .apply(Tx::new(vec![put("c", json!({"n": 1}))]))
        .unwrap();
    fs::remove_file(root.join(".plaindb/last-commit")).unwrap();
    first
        .apply(Tx::new(vec![put("d", json!({"n": 1}))]))
        .unwrap();
    assert!(foreign_warnings(&first).is_empty());
    assert!(foreign_warnings(&Store::open(root).unwrap()).is_empty());
}

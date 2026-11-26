use std::process::Command;

use plaindb_core::{
    CommitMode, Doc, Durability, ErrorCode, Format, Op, PushMode, Store, StoreConfig, Tx,
};
use serde_json::{Value, json};

fn doc(value: Value) -> Doc {
    value.as_object().cloned().unwrap()
}

fn subject(root: &std::path::Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "-1", "--format=%s"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn config_follows_every_setting_made_through_the_store() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    store.set_setting("durability", "normal").unwrap();
    store.set_setting("gc_every", "7").unwrap();
    store.set_setting("git_binary", "/path/to/git").unwrap();
    store.set_setting("commit", "manual").unwrap();
    store.set_setting("push", "on-commit").unwrap();
    let config = store.config();
    assert_eq!(config.durability, Durability::Normal);
    assert_eq!(config.gc_every, 7);
    assert_eq!(config.git_binary.as_deref(), Some("/path/to/git"));
    assert_eq!(config.commit, CommitMode::Manual);
    assert_eq!(config.push, PushMode::OnCommit);
    store.set_setting("git_binary", "").unwrap();
    assert_eq!(store.config().git_binary, None);
}

#[test]
fn config_sees_a_change_made_by_another_handle() {
    let temp = tempfile::tempdir().unwrap();
    let first = Store::init(temp.path()).unwrap();
    let second = Store::open(temp.path()).unwrap();
    second.set_setting("gc_every", "3").unwrap();
    assert_eq!(first.config().gc_every, 3);
}

#[test]
fn a_bad_value_changes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let before = std::fs::read(temp.path().join("_store.json")).unwrap();
    for (key, value) in [
        ("durability", "sturdy"),
        ("gc_every", "-1"),
        ("commit", "never"),
        ("remote", "two words"),
    ] {
        let error = store.set_setting(key, value).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig, "{key}");
    }
    assert_eq!(
        store.set_setting("colour", "red").unwrap_err().code,
        ErrorCode::InvalidName
    );
    assert_eq!(
        std::fs::read(temp.path().join("_store.json")).unwrap(),
        before
    );
    assert_eq!(*store.config(), StoreConfig::default());
}

#[test]
fn a_limit_takes_effect_on_the_next_write() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    store.create_db("app").unwrap();
    store
        .create_collection("app", "notes", Format::Json)
        .unwrap();
    store
        .set_setting("limits.max_document_bytes", "40")
        .unwrap();
    let big = Tx::new(vec![Op::Insert {
        db: "app".to_owned(),
        coll: "notes".to_owned(),
        id: Some("a".to_owned()),
        doc: doc(json!({ "text": "x".repeat(200) })),
    }]);
    assert_eq!(store.apply(big).unwrap_err().code, ErrorCode::LimitExceeded);
}

#[test]
fn a_setting_is_committed_when_git_is_on() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init_with(
        temp.path(),
        StoreConfig {
            git: true,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.set_setting("gc_every", "11").unwrap();
    assert_eq!(subject(temp.path()), "meta store: set gc_every");
    store.set_setting("gc_every", "11").unwrap();
    store.set_setting("git_binary", "").unwrap();
    assert_eq!(subject(temp.path()), "meta store: set gc_every");
}

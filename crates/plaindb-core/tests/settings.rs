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
    store.set_setting("commit", "manual").unwrap();
    store.set_setting("push", "on-commit").unwrap();
    let config = store.config();
    assert_eq!(config.durability, Durability::Normal);
    assert_eq!(config.gc_every, 7);
    assert_eq!(config.commit, CommitMode::Manual);
    assert_eq!(config.push, PushMode::OnCommit);
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
    store.set_setting("durability", "full").unwrap();
    assert_eq!(subject(temp.path()), "meta store: set gc_every");
}

const BAD_REMOTES: &[&str] = &["--receive-pack=x", "-x", "a b", "a;b", ""];
const BAD_BRANCHES: &[&str] = &[
    "+main", ":main", "-x", "a..b", "a//b", "x.lock", "a@{1}", "main/", "main.",
];

fn long_remote() -> String {
    "r".repeat(65)
}

fn write_store_key(root: &std::path::Path, key: &str, value: &str) {
    let path = root.join("_store.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config[key] = json!(value);
    std::fs::write(&path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
}

fn refused(key: &str, values: &[String]) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    for value in values {
        let error = store.set_setting(key, value).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig, "{key} {value:?}");
        assert_eq!(error.details["key"], key, "{value:?}");
        assert_eq!(error.details["value"], value.as_str(), "{value:?}");
    }
    assert_eq!(*store.config(), StoreConfig::default());
    drop(store);
    for value in values {
        write_store_key(temp.path(), key, value);
        let error = Store::open(temp.path()).err().unwrap();
        assert_eq!(error.code, ErrorCode::InvalidConfig, "{key} {value:?}");
        assert_eq!(error.details["key"], key, "{value:?}");
        assert_eq!(error.details["value"], value.as_str(), "{value:?}");
    }
}

#[test]
fn a_remote_that_is_not_a_plain_name_is_refused_when_set_and_when_read() {
    let mut values: Vec<String> = BAD_REMOTES
        .iter()
        .map(|value| (*value).to_owned())
        .collect();
    values.push(long_remote());
    refused("remote", &values);
}

#[test]
fn a_branch_that_is_not_a_plain_branch_name_is_refused_when_set_and_when_read() {
    let values: Vec<String> = BAD_BRANCHES
        .iter()
        .map(|value| (*value).to_owned())
        .collect();
    refused("branch", &values);
}

#[test]
fn plain_remote_and_branch_names_are_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    store.set_setting("remote", "backup-2.mirror_a").unwrap();
    store.set_setting("remote", &"r".repeat(64)).unwrap();
    for branch in ["main", "backup/2026", "v1.x"] {
        store.set_setting("branch", branch).unwrap();
        assert_eq!(store.config().branch, branch);
    }
    drop(store);
    write_store_key(temp.path(), "branch", "backup/2026");
    assert_eq!(
        Store::open(temp.path()).unwrap().config().branch,
        "backup/2026"
    );
}

#[test]
fn git_binary_is_no_longer_a_setting() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let error = store.set_setting("git_binary", "/usr/bin/git").unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert_eq!(
        error.message,
        "git_binary is no longer a setting: set the PLAINDB_GIT environment variable"
    );
    assert_eq!(
        store.config().setting("git_binary").unwrap_err().code,
        ErrorCode::InvalidName
    );
}

#[test]
fn a_store_file_that_names_a_git_program_opens_and_loses_it_on_the_next_write() {
    for stored in [Value::Null, json!("/path/to/git")] {
        let temp = tempfile::tempdir().unwrap();
        drop(Store::init(temp.path()).unwrap());
        let path = temp.path().join("_store.json");
        let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        config["git_binary"] = stored.clone();
        std::fs::write(&path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
        let store = Store::open(temp.path()).unwrap();
        store.set_setting("gc_every", "3").unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["gc_every"], 3, "{stored}");
        assert!(written.get("git_binary").is_none(), "{stored}");
    }
}

#[test]
fn checkpoint_interval_is_a_setting_that_refuses_zero() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    store.set_setting("checkpoint_interval", "8").unwrap();
    assert_eq!(store.config().checkpoint_interval, 8);
    assert_eq!(store.config().setting("checkpoint_interval").unwrap(), 8);
    for value in ["0", "often", "-1"] {
        let error = store.set_setting("checkpoint_interval", value).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig, "{value}");
    }
    assert_eq!(store.config().checkpoint_interval, 8);
}

#[test]
fn an_unknown_key_in_the_store_file_is_warned_about_and_kept() {
    let temp = tempfile::tempdir().unwrap();
    Store::init(temp.path()).unwrap();
    let path = temp.path().join("_store.json");
    let mut stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    stored["durabilty"] = json!("off");
    std::fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
    let store = Store::open(temp.path()).unwrap();
    let status = store.status().unwrap();
    let warning = status
        .warnings
        .iter()
        .find(|warning| warning.code == "unknown_setting")
        .expect("a warning");
    assert_eq!(warning.details["key"], json!("durabilty"));
    store.set_setting("gc_every", "100").unwrap();
    let rewritten: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(rewritten["durabilty"], json!("off"));
    assert_eq!(rewritten["gc_every"], json!(100));
}

#[test]
fn a_zero_result_or_scan_limit_is_refused() {
    for key in ["limits.max_results", "limits.max_scan_ms"] {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::init(temp.path()).unwrap();
        let error = store.set_setting(key, "0").unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig, "{key}");
        assert_eq!(error.details["key"], key);
        assert!(
            error
                .message
                .contains("a whole number above 0, or empty for no limit"),
            "{}",
            error.message
        );
        store.set_setting(key, "5").unwrap();
        store.set_setting(key, "").unwrap();
        drop(store);
        let path = temp.path().join("_store.json");
        let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let field = key.trim_start_matches("limits.");
        config["limits"][field] = json!(0);
        std::fs::write(&path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
        let error = Store::open(temp.path()).err().unwrap();
        assert_eq!(error.code, ErrorCode::InvalidConfig, "{key}");
        assert_eq!(error.details["key"], key);
    }
}

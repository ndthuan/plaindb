use std::fs::{self, File};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use plaindb_core::{
    Check, CheckStatus, CommitMode, Doc, DoctorOptions, Durability, Format, Op, Store, StoreConfig,
    Tx,
};
use serde_json::{Value, json};

#[test]
fn a_healthy_store_reports_stable_check_ids_and_no_error() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let report = store.doctor(DoctorOptions { offline: true });
    let ids: Vec<&str> = report.checks.iter().map(|check| check.id).collect();
    for id in [
        "git",
        "store_format",
        "journal",
        "write_lock",
        "documents_invalid",
        "documents_schema",
        "names_case",
        "index_cache",
        "filesystem_network",
        "filesystem_case",
        "durability",
        "disk_space",
    ] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
    assert_eq!(report.errors(), 0);
    assert!(
        report
            .checks
            .iter()
            .all(|check| check.status != CheckStatus::Error)
    );
    assert_eq!(
        report.to_json()["checks"].as_array().unwrap().len(),
        ids.len()
    );
}

#[test]
fn a_held_write_lock_is_reported() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let held = plaindb_core::WriteLock::try_acquire(temp.path())
        .unwrap()
        .unwrap();
    let report = store.doctor(DoctorOptions::default());
    let check = report
        .checks
        .iter()
        .find(|check| check.id == "write_lock")
        .unwrap();
    assert_eq!(check.status, CheckStatus::Warn);
    drop(held);
    let report = store.doctor(DoctorOptions::default());
    let check = report
        .checks
        .iter()
        .find(|check| check.id == "write_lock")
        .unwrap();
    assert_eq!(check.status, CheckStatus::Ok);
}

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
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn git_store(root: &Path) -> Store {
    let store = Store::init_with(
        root,
        StoreConfig {
            git: true,
            durability: Durability::Off,
            gc_every: 0,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Json)
        .unwrap();
    store
        .apply(Tx::new(vec![Op::Put {
            db: "main".to_owned(),
            coll: "items".to_owned(),
            id: "a".to_owned(),
            doc: doc(json!({ "n": 1 })),
            if_version: None,
        }]))
        .unwrap();
    store
}

fn find(store: &Store, id: &str) -> Check {
    store
        .doctor(DoctorOptions { offline: true })
        .checks
        .into_iter()
        .find(|check| check.id == id)
        .unwrap_or_else(|| panic!("no check {id}"))
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn age(path: &Path, minutes: u64) {
    fs::write(path, "").unwrap();
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(minutes * 60))
        .unwrap();
}

#[test]
fn fix_texts_quote_a_store_path_with_a_space_and_a_quote() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("the store's data");
    let store = git_store(&root);
    git(&root, &["checkout", "-q", "-b", "other"]);
    fs::write(
        root.join(".plaindb/gc.json"),
        r#"{"last_gc_outcome":"failed","last_gc_error":"boom"}"#,
    )
    .unwrap();
    fs::write(
        root.join(".plaindb/git-pending.json"),
        r#"{"tx_ids":["t1"],"message":"index.lock exists"}"#,
    )
    .unwrap();
    age(&root.join(".git/index.lock"), 11);
    for id in ["git_head_branch", "git_gc"] {
        let fix = find(&store, id).fix.unwrap();
        assert!(fix.contains(&quoted(&root)), "{id}: {fix}");
        let parsed = Command::new("sh")
            .args(["-n", "-c", &fix])
            .output()
            .unwrap();
        assert!(parsed.status.success(), "{id}: {fix}: {parsed:?}");
    }
    let lock = quoted(&root.join(".git").join("index.lock"));
    for id in ["git_locks", "git_pending_commits"] {
        let fix = find(&store, id).fix.unwrap();
        assert!(fix.contains(&lock), "{id}: {fix}");
    }
}

#[test]
fn the_remote_ahead_fix_names_the_reset_flag() {
    let temp = tempfile::tempdir().unwrap();
    let store = git_store(temp.path());
    fs::write(
        temp.path().join(".plaindb/push.json"),
        r#"{"remote_ahead":true}"#,
    )
    .unwrap();
    assert_eq!(
        find(&store, "git_remote_ahead").fix.as_deref(),
        Some(
            "merge the remote branch into the store's repository with git, then run plaindb push --reset-remote-ahead"
        )
    );
}

#[test]
fn the_foreign_commit_fix_says_when_it_clears() {
    let temp = tempfile::tempdir().unwrap();
    let store = git_store(temp.path());
    fs::write(
        temp.path().join(".plaindb/warnings.json"),
        r#"{"code":"foreign_commit","ids":["x"],"at":"2026-01-01T00:00:00Z","head":"x"}"#,
    )
    .unwrap();
    assert_eq!(
        find(&store, "git_foreign_commits").fix.as_deref(),
        Some(
            "review the commits with git log; the warning clears after the next two commits made by plaindb"
        )
    );
}

#[test]
fn only_the_locks_git_needs_for_a_commit_are_checked() {
    let temp = tempfile::tempdir().unwrap();
    let store = git_store(temp.path());
    age(&temp.path().join(".git/foo.lock"), 11);
    assert_eq!(find(&store, "git_locks").status, CheckStatus::Ok);
    age(&temp.path().join(".git/index.lock"), 11);
    assert_eq!(find(&store, "git_locks").status, CheckStatus::Error);
    fs::remove_file(temp.path().join(".git/index.lock")).unwrap();
    age(&temp.path().join(".git/refs/heads/main.lock"), 11);
    assert_eq!(find(&store, "git_locks").status, CheckStatus::Error);
}

#[test]
fn a_hand_edit_left_out_of_git_is_a_worktree_warning() {
    let temp = tempfile::tempdir().unwrap();
    let store = git_store(temp.path());
    assert_eq!(find(&store, "git_worktree").status, CheckStatus::Ok);
    fs::write(
        temp.path().join("main/items/a.json"),
        "{\"_id\": \"a\", \"n\": 9}\n",
    )
    .unwrap();
    let check = find(&store, "git_worktree");
    assert_eq!(check.status, CheckStatus::Warn);
    assert_eq!(check.details["count"], 1);
    assert_eq!(check.details["paths"], json!(["main/items/a.json"]));
    assert_eq!(check.fix.as_deref(), Some("plaindb commit"));
    store.commit(None, None).unwrap().unwrap();
    assert_eq!(find(&store, "git_worktree").status, CheckStatus::Ok);
}

#[test]
fn pending_manual_transactions_are_not_a_worktree_problem() {
    let temp = tempfile::tempdir().unwrap();
    let store = git_store(temp.path());
    store.set_commit_mode(CommitMode::Manual).unwrap();
    store
        .apply(Tx::new(vec![Op::Put {
            db: "main".to_owned(),
            coll: "items".to_owned(),
            id: "b".to_owned(),
            doc: doc(json!({ "n": 2 })),
            if_version: None,
        }]))
        .unwrap();
    let check = find(&store, "git_worktree");
    assert_eq!(check.status, CheckStatus::Ok);
    assert_eq!(check.message, "1 transactions wait for plaindb commit");
}

#[test]
fn an_unknown_store_setting_is_a_doctor_warning() {
    let temp = tempfile::tempdir().unwrap();
    Store::init(temp.path()).unwrap();
    let path = temp.path().join("_store.json");
    let mut stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let report_for = |stored: &Value| {
        fs::write(&path, serde_json::to_vec(stored).unwrap()).unwrap();
        Store::open(temp.path())
            .unwrap()
            .doctor(DoctorOptions { offline: true })
    };
    let clean = report_for(&stored);
    let check = clean
        .checks
        .iter()
        .find(|check| check.id == "store_unknown_settings")
        .expect("the check is always reported");
    assert_eq!(check.status, CheckStatus::Ok);
    stored["durabilty"] = json!("off");
    let report = report_for(&stored);
    let check = report
        .checks
        .iter()
        .find(|check| check.id == "store_unknown_settings")
        .unwrap();
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains("durabilty"), "{}", check.message);
    assert!(check.fix.as_deref().unwrap().contains("known settings"));
}

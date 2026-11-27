#![cfg(feature = "test-hooks")]

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

use plaindb_core::{
    CommitMode, Doc, ErrorCode, Format, Op, PushMode, PushOutcome, Store, Tx, TxResult,
};
use plaindb_core::{Durability, StoreConfig};
use serde_json::{Value, json};

const WRITER: &str = env!("CARGO_BIN_EXE_plaindb-test-writer");

fn doc(value: Value) -> Doc {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
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
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn bare_remote(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir_all(&path).unwrap();
    git(&path, &["init", "-q", "--bare", "-b", "main"]);
    path
}

fn put(id: &str, n: u64) -> Tx {
    Tx::new(vec![Op::Put {
        db: "main".to_owned(),
        coll: "items".to_owned(),
        id: id.to_owned(),
        doc: doc(json!({ "n": n })),
        if_version: None,
    }])
}

fn pushing_store(root: &Path, remote: &Path) -> Store {
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
    store.set_remote(remote.to_str().unwrap()).unwrap();
    store.set_push(PushMode::OnCommit).unwrap();
    store
}

fn pushed(store: &Store, id: &str, n: u64) -> TxResult {
    store.apply(put(id, n)).unwrap()
}

#[test]
fn each_commit_lands_on_the_remote() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let store = pushing_store(&dir.path().join("store"), &remote);
    let result = pushed(&store, "a", 1);
    assert_eq!(result.push, Some(PushOutcome::Ok));
    let status = store.status().unwrap().git;
    assert_eq!(status.unpushed, 0);
    assert!(status.last_push_at.is_some());
    assert_eq!(status.last_push_error, None);
    assert!(!status.remote_ahead);
    assert_eq!(status.remote_url.as_deref(), remote.to_str());
    assert_eq!(
        git(&remote, &["rev-parse", "main"]),
        status.head.clone().unwrap()
    );
    assert!(status.git_bytes > 0);
}

#[test]
fn a_store_that_does_not_push_reports_no_push() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let store = pushing_store(&dir.path().join("store"), &remote);
    store.set_push(PushMode::Off).unwrap();
    assert_eq!(pushed(&store, "a", 1).push, None);
    assert!(store.status().unwrap().git.unpushed > 0);
}

#[test]
fn manual_commits_push_when_committed() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let store = pushing_store(&dir.path().join("store"), &remote);
    store.set_commit_mode(CommitMode::Manual).unwrap();
    assert_eq!(pushed(&store, "a", 1).push, None);
    assert_eq!(pushed(&store, "b", 2).push, None);
    assert_eq!(store.status().unwrap().git.pending_txs, 2);
    store.commit(Some("two writes"), None).unwrap();
    let status = store.status().unwrap().git;
    assert_eq!(status.unpushed, 0);
    assert_eq!(git(&remote, &["rev-parse", "main"]), status.head.unwrap());
}

#[test]
fn a_remote_that_is_ahead_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let store = pushing_store(&dir.path().join("store"), &remote);
    pushed(&store, "a", 1);
    let other = dir.path().join("other");
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    fs::write(other.join("note.txt"), "from another clone").unwrap();
    git(&other, &["add", "note.txt"]);
    git(&other, &["commit", "-q", "-m", "another clone"]);
    git(&other, &["push", "-q", "origin", "main"]);
    let ahead = git(&remote, &["rev-parse", "main"]);

    assert_eq!(pushed(&store, "b", 2).push, Some(PushOutcome::RemoteAhead));
    let status = store.status().unwrap().git;
    assert!(status.remote_ahead);
    assert_eq!(status.unpushed, 1);
    assert_eq!(pushed(&store, "c", 3).push, Some(PushOutcome::RemoteAhead));
    assert_eq!(store.push().unwrap_err().code, ErrorCode::RemoteAhead);
    assert_eq!(git(&remote, &["rev-parse", "main"]), ahead);

    git(
        &other,
        &["pull", "-q", "--no-rebase", "--no-edit", "origin", "main"],
    );
    git(
        &dir.path().join("store"),
        &[
            "pull",
            "-q",
            "--no-rebase",
            "--no-edit",
            remote.to_str().unwrap(),
            "main",
        ],
    );
    store.reset_remote_ahead().unwrap();
    assert!(!store.status().unwrap().git.remote_ahead);
    assert_eq!(store.push().unwrap(), PushOutcome::Ok);
    assert_eq!(store.status().unwrap().git.unpushed, 0);
    assert_eq!(
        git(&remote, &["rev-parse", "main"]),
        git(&dir.path().join("store"), &["rev-parse", "HEAD"])
    );
}

#[test]
fn a_remote_with_unrelated_history_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let seed = dir.path().join("seed");
    fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    fs::write(seed.join("readme.txt"), "unrelated").unwrap();
    git(&seed, &["add", "readme.txt"]);
    git(&seed, &["commit", "-q", "-m", "unrelated"]);
    git(&seed, &["push", "-q", remote.to_str().unwrap(), "main"]);
    let existing = git(&remote, &["rev-parse", "main"]);

    let store = pushing_store(&dir.path().join("store"), &remote);
    assert!(store.status().unwrap().git.remote_ahead);
    assert_eq!(git(&remote, &["rev-parse", "main"]), existing);
    assert_eq!(pushed(&store, "a", 1).push, Some(PushOutcome::RemoteAhead));
}

#[test]
fn an_unreachable_remote_never_fails_a_write() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let store = pushing_store(&dir.path().join("store"), &remote);
    pushed(&store, "a", 1);
    let gone = dir.path().join("gone.git");
    store.set_remote(gone.to_str().unwrap()).unwrap();

    let first = pushed(&store, "b", 2);
    assert_eq!(first.push, Some(PushOutcome::Failed));
    let after_first = store.status().unwrap().git;
    assert!(after_first.last_push_error.is_some());
    assert!(after_first.last_push_error.as_deref().unwrap().len() <= 500);
    assert!(!after_first.remote_ahead);
    assert_eq!(store.get("main", "items", "b").unwrap().unwrap().0["n"], 2);

    let second = pushed(&store, "c", 3);
    assert_eq!(second.push, Some(PushOutcome::Failed));
    let after_second = store.status().unwrap().git;
    assert_eq!(after_second.unpushed, after_first.unpushed + 1);

    store.set_remote(remote.to_str().unwrap()).unwrap();
    assert_eq!(store.push().unwrap(), PushOutcome::Ok);
    let fixed = store.status().unwrap().git;
    assert_eq!(fixed.last_push_error, None);
    assert_eq!(fixed.unpushed, 0);
    assert_eq!(git(&remote, &["rev-parse", "main"]), fixed.head.unwrap());
}

#[test]
fn a_held_push_lock_defers_the_push() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let root = dir.path().join("store");
    let store = pushing_store(&root, &remote);
    let held = File::options()
        .write(true)
        .open(root.join(".plaindb/push.lock"))
        .unwrap();
    held.lock().unwrap();
    assert_eq!(pushed(&store, "a", 1).push, Some(PushOutcome::Deferred));
    assert_eq!(store.push().unwrap(), PushOutcome::Deferred);
    held.unlock().unwrap();
    assert_eq!(store.push().unwrap(), PushOutcome::Ok);
    assert_eq!(store.status().unwrap().git.unpushed, 0);
}

#[test]
fn a_missing_git_binary_is_reported_as_git_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let root = dir.path().join("store");
    let store = pushing_store(&root, &remote);
    let output = Command::new(WRITER)
        .arg(&root)
        .arg("push")
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("git_not_found"));
    assert_eq!(
        store.status().unwrap().git.last_push_error.as_deref(),
        Some("git_not_found")
    );
}

#[test]
fn concurrent_writers_leave_the_remote_at_the_local_head() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let root = dir.path().join("store");
    pushing_store(&root, &remote);
    std::thread::scope(|scope| {
        for writer in 0..4_u64 {
            let root = &root;
            scope.spawn(move || {
                let store = Store::open(root).unwrap();
                for n in 0..8 {
                    let result = store.apply(put(&format!("w{writer}n{n}"), n)).unwrap();
                    assert!(matches!(
                        result.push,
                        Some(PushOutcome::Ok | PushOutcome::Deferred)
                    ));
                }
            });
        }
    });
    assert_eq!(
        git(&remote, &["rev-parse", "main"]),
        git(&root, &["rev-parse", "HEAD"])
    );
}

fn set_store_key(root: &Path, key: &str, value: &str) {
    let path = root.join("_store.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config[key] = json!(value);
    fs::write(&path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
}

#[test]
fn a_remote_that_is_a_git_option_never_reaches_git() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let root = dir.path().join("store");
    let store = pushing_store(&root, &remote);
    pushed(&store, "a", 1);
    let injected = dir.path().join("INJ");
    set_store_key(
        &root,
        "remote",
        &format!(
            "--receive-pack=touch {};git-receive-pack",
            injected.display()
        ),
    );
    assert_eq!(store.push().unwrap_err().code, ErrorCode::InvalidConfig);
    assert_eq!(
        store
            .apply(put("b", 2))
            .map(|result| result.push)
            .ok()
            .flatten(),
        None
    );
    let error = Store::open(&root).err().unwrap();
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert_eq!(error.details["key"], "remote");
    let report =
        plaindb_core::doctor::inspect(&root, plaindb_core::doctor::DoctorOptions::default());
    let check = report
        .checks
        .iter()
        .find(|check| check.id == "store_config")
        .unwrap();
    assert_eq!(check.status, plaindb_core::doctor::CheckStatus::Error);
    assert!(check.fix.as_deref().unwrap().contains("_store.json"));
    assert!(!report.checks.iter().any(|check| check.id == "git_binary"));
    assert!(!injected.exists());
}

#[test]
fn a_remote_url_that_looks_like_an_option_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init_with_git(dir.path().join("store")).unwrap();
    for url in ["--upload-pack=x", "-x", "a b", "a\u{7}b"] {
        let error = store.set_remote(url).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig, "{url:?}");
        assert_eq!(error.details["key"], "remote_url", "{url:?}");
    }
}

#[cfg(unix)]
#[test]
fn the_push_names_the_remote_after_a_separator_and_sends_a_full_refspec() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let real = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    let calls = dir.path().join("calls");
    let recorder = dir.path().join("recording-git");
    fs::write(
        &recorder,
        format!(
            "#!/bin/sh\necho \"$*\" >> {}\nexec {real} \"$@\"\n",
            calls.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&recorder, fs::Permissions::from_mode(0o755)).unwrap();
    let root = dir.path().join("store");
    let store = Store::init_with(
        &root,
        StoreConfig {
            git: true,
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    plaindb_core::set_git_program(&root, Some(recorder.to_str().unwrap().to_owned()));
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Json)
        .unwrap();
    store.set_remote(remote.to_str().unwrap()).unwrap();
    store.set_push(PushMode::OnCommit).unwrap();
    assert_eq!(pushed(&store, "a", 1).push, Some(PushOutcome::Ok));
    let head = store.status().unwrap().git.head.unwrap();
    assert_eq!(git(&remote, &["rev-parse", "refs/heads/main"]), head);
    let recorded = fs::read_to_string(&calls).unwrap();
    let pushes: Vec<&str> = recorded
        .lines()
        .filter(|line| line.split(' ').any(|word| word == "push"))
        .collect();
    assert!(!pushes.is_empty(), "{recorded}");
    for line in pushes {
        assert!(
            line.ends_with("push --porcelain -- origin refs/heads/main:refs/heads/main"),
            "{line}"
        );
    }
}

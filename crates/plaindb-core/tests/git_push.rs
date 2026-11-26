#![cfg(feature = "test-hooks")]

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

use plaindb_core::{
    CommitMode, Doc, ErrorCode, Format, Op, PushMode, PushOutcome, Store, Tx, TxResult,
};
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
    let store = Store::init_with_git(root).unwrap();
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

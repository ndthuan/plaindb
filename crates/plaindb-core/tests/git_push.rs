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
    assert_eq!(second.push, Some(PushOutcome::Deferred));
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

#[test]
fn the_status_and_doctor_show_an_https_remote_without_its_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init_with_git(dir.path().join("store")).unwrap();
    for (url, shown) in [
        (
            "https://user:s3cret@example.com/r.git",
            "https://***@example.com/r.git",
        ),
        (
            "https://tok@example.com/r.git",
            "https://***@example.com/r.git",
        ),
        ("http://user:pw@example.com", "http://***@example.com"),
        ("ssh://git@example.com/r.git", "ssh://git@example.com/r.git"),
        ("git@example.com:r.git", "git@example.com:r.git"),
        ("/tmp/r.git", "/tmp/r.git"),
        ("https://example.com/a@b.git", "https://example.com/a@b.git"),
    ] {
        store.set_remote(url).unwrap();
        let status = store.status().unwrap();
        assert_eq!(status.git.remote_url.as_deref(), Some(shown), "{url}");
        let report = plaindb_core::doctor::inspect(
            store.root(),
            plaindb_core::doctor::DoctorOptions { offline: true },
        );
        let text = serde_json::to_string(&report.to_json()).unwrap();
        let listed = format!("{status:?}");
        for secret in ["s3cret", "tok@", "user:pw"] {
            assert!(!text.contains(secret), "{url}: {text}");
            assert!(!listed.contains(secret), "{url}: {listed}");
        }
    }
}

#[cfg(unix)]
fn recording_git(dir: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
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
    let calls = dir.join("calls");
    let recorder = dir.join("recording-git");
    fs::write(
        &recorder,
        format!(
            "#!/bin/sh\necho \"$*\" >> {}\nexec {real} \"$@\"\n",
            calls.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&recorder, fs::Permissions::from_mode(0o755)).unwrap();
    (recorder, calls)
}

fn push_calls(calls: &Path) -> usize {
    fs::read_to_string(calls)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.split(' ').any(|word| word == "push"))
        .count()
}

fn doctor_check(root: &Path, id: &str) -> plaindb_core::doctor::Check {
    plaindb_core::doctor::inspect(root, plaindb_core::doctor::DoctorOptions { offline: true })
        .checks
        .into_iter()
        .find(|check| check.id == id)
        .unwrap_or_else(|| panic!("no check {id}"))
}

fn warning_codes(store: &Store) -> Vec<String> {
    store
        .status()
        .unwrap()
        .warnings
        .into_iter()
        .map(|warning| warning.code)
        .collect()
}

#[cfg(unix)]
#[test]
fn a_push_from_another_branch_sends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let root = dir.path().join("store");
    let store = pushing_store(&root, &remote);
    assert_eq!(pushed(&store, "a", 1).push, Some(PushOutcome::Ok));
    let delivered = git(&remote, &["rev-parse", "main"]);
    let (recorder, calls) = recording_git(dir.path());
    plaindb_core::set_git_program(&root, Some(recorder.to_str().unwrap().to_owned()));
    git(&root, &["checkout", "-q", "-b", "other"]);

    assert_eq!(pushed(&store, "b", 2).push, Some(PushOutcome::Failed));
    let error = store.push().unwrap_err();
    assert_eq!(error.code, ErrorCode::HeadOffBranch);
    assert_eq!(error.code.as_str(), "head_off_branch");
    assert_eq!(error.message, "HEAD is on other, the store pushes main");
    assert_eq!(error.details["head"], "other");
    assert_eq!(error.details["branch"], "main");
    assert_eq!(git(&remote, &["rev-parse", "main"]), delivered);
    assert_eq!(push_calls(&calls), 0);

    git(&root, &["checkout", "-q", "--detach"]);
    let error = store.push().unwrap_err();
    assert_eq!(error.code, ErrorCode::HeadOffBranch);
    assert_eq!(error.details["head"], "detached");
    assert_eq!(push_calls(&calls), 0);
}

#[test]
fn doctor_and_status_name_a_head_off_the_branch_until_it_is_switched_back() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let root = dir.path().join("store");
    let store = pushing_store(&root, &remote);
    pushed(&store, "a", 1);
    assert_eq!(
        doctor_check(&root, "git_head_branch").status,
        plaindb_core::doctor::CheckStatus::Ok
    );
    git(&root, &["checkout", "-q", "-b", "other"]);

    let check = doctor_check(&root, "git_head_branch");
    assert_eq!(check.status, plaindb_core::doctor::CheckStatus::Error);
    let quoted = format!("'{}'", root.display());
    assert_eq!(
        check.fix.as_deref(),
        Some(format!("git -C {quoted} switch main, or plaindb config set branch other").as_str())
    );
    let warning = store
        .status()
        .unwrap()
        .warnings
        .into_iter()
        .find(|warning| warning.code == "head_off_branch")
        .unwrap();
    assert_eq!(warning.details["head"], "other");
    assert_eq!(warning.details["branch"], "main");

    git(&root, &["checkout", "-q", "--detach"]);
    assert_eq!(
        doctor_check(&root, "git_head_branch").fix.as_deref(),
        Some(format!("git -C {quoted} switch main").as_str())
    );

    git(&root, &["switch", "-q", "main"]);
    assert_eq!(
        doctor_check(&root, "git_head_branch").status,
        plaindb_core::doctor::CheckStatus::Ok
    );
    assert!(!warning_codes(&store).contains(&"head_off_branch".to_owned()));
    pushed(&store, "c", 3);
    assert_eq!(store.push().unwrap(), PushOutcome::Ok);
    assert_eq!(
        git(&remote, &["rev-parse", "main"]),
        git(&root, &["rev-parse", "HEAD"])
    );
}

#[test]
fn a_write_on_another_branch_records_why_its_push_failed() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let root = dir.path().join("store");
    let store = pushing_store(&root, &remote);
    pushed(&store, "a", 1);
    git(&root, &["checkout", "-q", "-b", "other"]);
    let result = pushed(&store, "b", 2);
    assert_eq!(result.push, Some(PushOutcome::Failed));
    assert_eq!(store.get("main", "items", "b").unwrap().unwrap().0["n"], 2);
    assert_eq!(
        store.status().unwrap().git.last_push_error.as_deref(),
        Some("HEAD is on other, the store pushes main")
    );
}

#[test]
fn the_branch_setting_must_name_the_checked_out_branch() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let root = dir.path().join("store");
    let store = pushing_store(&root, &remote);
    pushed(&store, "a", 1);
    let error = store.set_setting("branch", "backup").unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert_eq!(
        error.message,
        "branch must name the checked-out branch (main); switch with git first"
    );
    assert_eq!(store.config().branch, "main");

    git(&root, &["switch", "-q", "-c", "backup"]);
    store.set_setting("branch", "backup").unwrap();
    assert_eq!(store.config().branch, "backup");
    assert_eq!(store.push().unwrap(), PushOutcome::Ok);
    assert_eq!(
        git(&remote, &["rev-parse", "backup"]),
        git(&root, &["rev-parse", "HEAD"])
    );
}

#[test]
fn the_branch_setting_is_free_before_git_is_on() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path().join("store")).unwrap();
    store.set_setting("branch", "backup").unwrap();
    assert_eq!(store.config().branch, "backup");
}

fn push_state(root: &Path) -> Value {
    serde_json::from_slice(&fs::read(root.join(".plaindb/push.json")).unwrap()).unwrap()
}

#[cfg(unix)]
fn dead_remote_store(dir: &Path) -> (Store, PathBuf, PathBuf) {
    let remote = bare_remote(dir, "remote.git");
    let root = dir.join("store");
    let store = pushing_store(&root, &remote);
    pushed(&store, "seed", 0);
    store
        .set_remote(dir.join("gone.git").to_str().unwrap())
        .unwrap();
    let (recorder, calls) = recording_git(dir);
    plaindb_core::set_git_program(&root, Some(recorder.to_str().unwrap().to_owned()));
    (store, remote, calls)
}

#[cfg(unix)]
#[test]
fn a_dead_remote_costs_the_writes_after_the_first_failure_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, calls) = dead_remote_store(dir.path());
    let begun = std::time::Instant::now();
    let outcomes: Vec<Option<PushOutcome>> = (0..10)
        .map(|n| pushed(&store, &format!("d{n}"), n).push)
        .collect();
    assert!(begun.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(outcomes[0], Some(PushOutcome::Failed));
    assert!(
        outcomes[1..]
            .iter()
            .all(|outcome| *outcome == Some(PushOutcome::Deferred)),
        "{outcomes:?}"
    );
    assert_eq!(push_calls(&calls), 1);
    let status = store.status().unwrap().git;
    assert!(status.last_push_error.is_some());
    assert_eq!(status.unpushed, 10);
    let state = push_state(store.root());
    assert_eq!(state["failures"], 1);
    assert!(state["next_attempt_at"].is_string());
}

#[cfg(unix)]
#[test]
fn a_write_after_the_backoff_ends_tries_once_more() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, calls) = dead_remote_store(dir.path());
    pushed(&store, "a", 1);
    assert_eq!(push_calls(&calls), 1);
    let path = store.root().join(".plaindb/push.json");
    let mut state = push_state(store.root());
    state["next_attempt_at"] = json!("2000-01-01T00:00:00.000Z");
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(pushed(&store, "b", 2).push, Some(PushOutcome::Failed));
    assert_eq!(push_calls(&calls), 2);
    assert_eq!(push_state(store.root())["failures"], 2);
    assert_eq!(pushed(&store, "c", 3).push, Some(PushOutcome::Deferred));
    assert_eq!(push_calls(&calls), 2);
}

#[cfg(unix)]
#[test]
fn an_explicit_push_ignores_the_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let (store, remote, calls) = dead_remote_store(dir.path());
    pushed(&store, "a", 1);
    assert_eq!(push_calls(&calls), 1);
    assert!(store.push().is_err());
    assert_eq!(push_calls(&calls), 4);
    store.set_remote(remote.to_str().unwrap()).unwrap();
    assert_eq!(store.push().unwrap(), PushOutcome::Ok);
    let state = push_state(store.root());
    assert_eq!(state["failures"], 0);
    assert_eq!(state["next_attempt_at"], Value::Null);
    assert_eq!(pushed(&store, "b", 2).push, Some(PushOutcome::Ok));
}

#[cfg(unix)]
#[test]
fn a_store_without_a_remote_url_starts_no_push_after_a_write() {
    let dir = tempfile::tempdir().unwrap();
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
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Json)
        .unwrap();
    store.set_push(PushMode::OnCommit).unwrap();
    let (recorder, calls) = recording_git(dir.path());
    plaindb_core::set_git_program(&root, Some(recorder.to_str().unwrap().to_owned()));
    assert_eq!(pushed(&store, "a", 1).push, None);
    assert_eq!(push_calls(&calls), 0);
}

#[test]
fn the_background_push_delivers_the_newest_head_and_leaves_a_held_lock_alone() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let root = dir.path().join("store");
    let store = pushing_store(&root, &remote);
    store.set_push(PushMode::Off).unwrap();
    pushed(&store, "a", 1);
    pushed(&store, "b", 2);
    let held = File::options()
        .write(true)
        .open(root.join(".plaindb/push.lock"))
        .unwrap();
    held.lock().unwrap();
    plaindb_core::push_in_background(&root).unwrap();
    assert!(store.status().unwrap().git.unpushed > 0);
    held.unlock().unwrap();
    plaindb_core::push_in_background(&root).unwrap();
    assert_eq!(store.status().unwrap().git.unpushed, 0);
    assert_eq!(
        git(&remote, &["rev-parse", "main"]),
        git(&root, &["rev-parse", "HEAD"])
    );
}

#[test]
fn a_library_without_a_push_program_pushes_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let store = pushing_store(&dir.path().join("store"), &remote);
    store.set_push(PushMode::Background).unwrap();
    assert_eq!(store.config().push, PushMode::Background);
    assert_eq!(pushed(&store, "a", 1).push, Some(PushOutcome::Ok));
    assert_eq!(store.status().unwrap().git.unpushed, 0);
}

#[test]
fn a_push_that_runs_too_long_is_reported_as_stuck() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path(), "remote.git");
    let root = dir.path().join("store");
    let store = pushing_store(&root, &remote);
    assert!(!store.status().unwrap().git.push_running);
    assert_eq!(
        doctor_check(&root, "git_push_stuck").status,
        plaindb_core::doctor::CheckStatus::Ok
    );
    fs::write(
        root.join(".plaindb/push.lock"),
        format!("{} 2000-01-01T00:00:00.000Z\n", std::process::id()),
    )
    .unwrap();
    assert!(store.status().unwrap().git.push_running);
    let check = doctor_check(&root, "git_push_stuck");
    assert_eq!(check.status, plaindb_core::doctor::CheckStatus::Warn);
    assert_eq!(check.details["running"], true);
}

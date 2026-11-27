#![cfg(feature = "test-hooks")]

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use plaindb_core::{
    DEFAULT_GC_EVERY, Doc, Durability, ErrorCode, Format, GcOutcome, Op, Store, StoreConfig, Tx,
};
use serde_json::{Value, json};

const WRITER: &str = env!("CARGO_BIN_EXE_plaindb-test-writer");

fn doc(value: Value) -> Doc {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
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

fn collecting_store(root: &Path, gc_every: u64) -> Store {
    let store = Store::init_with(
        root,
        StoreConfig {
            git: true,
            durability: Durability::Off,
            gc_every,
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

fn real_git() -> String {
    let output = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn fake_git(dir: &Path, on_gc: &str) -> PathBuf {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = format!(
        "#!/bin/sh\ncase \" $* \" in\n*\" gc \"*)\n{on_gc}\n;;\nesac\nexec {} \"$@\"\n",
        real_git()
    );
    let path = bin.join("git");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn path_with(bin: &Path) -> String {
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
}

fn writer(root: &Path, mode: &str, count: u64, bin: Option<&Path>) -> Output {
    let mut command = Command::new(WRITER);
    command.arg(root).arg(mode).arg(count.to_string());
    if let Some(bin) = bin {
        command.env("PATH", path_with(bin));
    }
    command.output().unwrap()
}

fn pack_count(root: &Path) -> usize {
    fs::read_dir(root.join(".git/objects/pack"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".pack"))
                .count()
        })
        .unwrap_or(0)
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_default_runs_housekeeping_every_two_hundred_commits() {
    assert_eq!(DEFAULT_GC_EVERY, 200);
    assert_eq!(StoreConfig::default().gc_every, 200);
}

#[test]
fn an_explicit_run_packs_the_loose_objects() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = collecting_store(&root, 0);
    for n in 0..40 {
        store.apply(put(&format!("d{}", n % 5), n)).unwrap();
    }
    assert_eq!(pack_count(&root), 0);
    let report = store.gc().unwrap();
    assert_eq!(report.outcome, GcOutcome::Done);
    assert!(report.bytes_after < report.bytes_before);
    assert_eq!(pack_count(&root), 1);
    let git = store.status().unwrap().git;
    assert_eq!(git.last_gc_outcome.as_deref(), Some("done"));
    assert!(git.last_gc_at.is_some());
    assert_eq!(git.last_gc_error, None);
}

#[test]
fn commits_trigger_housekeeping_every_gc_every() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = collecting_store(&root, 100);
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .status()
            .unwrap()
            .success()
    };
    assert!(git(&["config", "gc.auto", "1"]));
    let before = store.status().unwrap().git;
    assert_eq!(before.last_gc_at, None);
    for n in 0..300 {
        store.apply(put(&format!("d{}", n % 5), n)).unwrap();
    }
    let after = store.status().unwrap().git;
    assert_eq!(after.last_gc_outcome.as_deref(), Some("done"));
    assert!(pack_count(&root) >= 1);
}

#[test]
fn a_store_with_gc_every_zero_never_runs_housekeeping() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = collecting_store(&root, 0);
    for n in 0..20 {
        store.apply(put("a", n)).unwrap();
    }
    let git = store.status().unwrap().git;
    assert_eq!(git.last_gc_at, None);
    assert_eq!(git.last_gc_outcome, None);
}

#[test]
fn a_store_without_git_refuses_an_explicit_run_and_runs_none_itself() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = Store::init_with(
        &root,
        StoreConfig {
            gc_every: 1,
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Json)
        .unwrap();
    store.apply(put("a", 1)).unwrap();
    assert_eq!(store.gc().unwrap_err().code, ErrorCode::GitDisabled);
    assert!(!root.join(".plaindb/gc.json").exists());
}

#[test]
fn a_held_lock_makes_the_run_busy() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = collecting_store(&root, 0);
    store.apply(put("a", 1)).unwrap();
    assert_eq!(store.gc().unwrap().outcome, GcOutcome::Done);
    let held = File::options()
        .write(true)
        .open(root.join(".plaindb/gc.lock"))
        .unwrap();
    held.lock().unwrap();
    assert_eq!(store.gc().unwrap().outcome, GcOutcome::Busy);
    held.unlock().unwrap();
    assert_eq!(store.gc().unwrap().outcome, GcOutcome::Done);
}

#[test]
fn a_write_never_waits_for_a_running_gc() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = collecting_store(&root, 1);
    let started = dir.path().join("started");
    let finished = dir.path().join("finished");
    let bin = fake_git(
        dir.path(),
        &format!(
            "touch {}\nsleep 4\ntouch {}",
            started.display(),
            finished.display()
        ),
    );
    let mut slow = Command::new(WRITER)
        .arg(&root)
        .arg("insert")
        .arg("1")
        .env("PATH", path_with(&bin))
        .spawn()
        .unwrap();
    wait_for(&started);
    let begun = Instant::now();
    for n in 0..3 {
        store.apply(put("b", n)).unwrap();
    }
    assert!(begun.elapsed() < Duration::from_secs(3));
    assert!(!finished.exists());
    assert!(slow.wait().unwrap().success());
    assert!(finished.exists());
}

#[test]
fn a_failing_gc_never_fails_a_write() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = collecting_store(&root, 1);
    let bin = fake_git(dir.path(), "echo 'fatal: disk exploded' >&2\nexit 1");
    let output = writer(&root, "insert", 3, Some(&bin));
    assert!(output.status.success());
    let git = store.status().unwrap().git;
    assert_eq!(git.last_gc_outcome.as_deref(), Some("failed"));
    assert!(git.last_gc_error.unwrap().contains("disk exploded"));
    assert_eq!(store.list_ids("main", "items").unwrap().len(), 3);
}

#[test]
fn a_missing_git_binary_is_recorded_and_writes_succeed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = collecting_store(&root, 1);
    let output = Command::new(WRITER)
        .arg(&root)
        .arg("insert")
        .arg("2")
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(output.status.success());
    let git = store.status().unwrap().git;
    assert_eq!(git.last_gc_outcome.as_deref(), Some("failed"));
    assert_eq!(git.last_gc_error.as_deref(), Some("git_not_found"));
    assert_eq!(store.list_ids("main", "items").unwrap().len(), 2);
}

#[test]
fn a_gc_already_running_elsewhere_is_recorded_as_busy_not_as_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = collecting_store(&root, 0);
    store.apply(put("a", 1)).unwrap();
    let bin = fake_git(
        dir.path(),
        "echo 'fatal: gc is already running on machine m pid 7 (use --force if not)' >&2\nexit 128",
    );
    let output = writer(&root, "gc", 1, Some(&bin));
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "Busy");
    let git = store.status().unwrap().git;
    assert_eq!(git.last_gc_outcome.as_deref(), Some("busy"));
    assert_eq!(git.last_gc_error, None);
}

#[test]
fn a_gc_pid_left_by_another_git_skips_the_automatic_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = collecting_store(&root, 1);
    let calls = dir.path().join("gc-calls");
    let bin = fake_git(dir.path(), &format!("echo gc >> {}", calls.display()));
    plaindb_core::set_git_program(&root, Some(bin.join("git").to_str().unwrap().to_owned()));
    store.apply(put("a", 1)).unwrap();
    assert!(calls.exists());
    fs::remove_file(&calls).unwrap();
    let state = root.join(".plaindb/gc.json");
    let before = fs::read(&state).unwrap();
    fs::write(root.join(".git/gc.pid"), "1 elsewhere").unwrap();
    store.apply(put("b", 2)).unwrap();
    assert!(!calls.exists());
    assert_eq!(fs::read(&state).unwrap(), before);
}

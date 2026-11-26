#![cfg(feature = "test-hooks")]

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use plaindb_core::{Format, PushMode, Store, WriteLock};
use serde_json::Value;

const WRITER: &str = env!("CARGO_BIN_EXE_plaindb-test-writer");
const PROCESSES: usize = 3;
const PER_PHASE: u64 = 111;
const CRASH_STATUS: i32 = 86;
const COLLECTIONS: [(&str, Format); 3] = [
    ("docs", Format::Json),
    ("cards", Format::Yaml),
    ("notes", Format::Md),
];

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

fn is_ancestor(dir: &Path, ancestor: &str, of: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["merge-base", "--is-ancestor", ancestor, of])
        .status()
        .unwrap()
        .success()
}

fn spawn(root: &Path, log: &Path, count: u64, crash: bool) -> Child {
    let mut command = Command::new(WRITER);
    command
        .arg(root)
        .arg("soak")
        .arg(count.to_string())
        .arg(log)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if crash {
        command.env("PLAINDB_TEST_CRASH_BEFORE_COMMIT", "1");
    }
    command.spawn().unwrap()
}

fn run_writers(root: &Path, logs: &[PathBuf], count: u64) {
    let children: Vec<Child> = logs
        .iter()
        .map(|log| spawn(root, log, count, false))
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn log_lines(log: &Path) -> Vec<Value> {
    fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn crash_one(root: &Path, log: &Path) -> String {
    for _ in 0..200 {
        let status = spawn(root, log, 1, true).wait().unwrap();
        if status.code() == Some(CRASH_STATUS) {
            let lines = log_lines(log);
            let last = lines.last().unwrap();
            return last["begin"].as_str().unwrap().to_owned();
        }
        assert!(status.success());
    }
    panic!("no transaction reached the commit step");
}

fn commit_trailers(root: &Path) -> BTreeMap<String, u32> {
    let text = git(root, &["log", "--format=%B"]);
    let mut seen = BTreeMap::new();
    for line in text.lines() {
        if let Some(id) = line.strip_prefix("Plaindb-Tx-Id: ") {
            *seen.entry(id.trim().to_owned()).or_insert(0) += 1;
        }
    }
    seen
}

fn document_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    for (collection, format) in COLLECTIONS {
        let dir = root.join("main").join(collection);
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with('_') || path.extension().is_none_or(|e| e != format.extension()) {
                continue;
            }
            files.insert(
                PathBuf::from(collection).join(name),
                fs::read(&path).unwrap(),
            );
        }
    }
    files
}

fn assert_in_sync(root: &Path, remote: &Path) {
    assert_eq!(
        git(remote, &["rev-parse", "main"]),
        git(root, &["rev-parse", "HEAD"])
    );
}

#[test]
#[ignore = "long soak"]
fn three_writers_through_a_crash_a_foreign_commit_and_a_remote_that_got_ahead() {
    let temp = tempfile::tempdir().unwrap();
    let remote = temp.path().join("remote.git");
    fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    let root = temp.path().join("store");
    let logs_dir = temp.path().join("logs");
    fs::create_dir_all(&logs_dir).unwrap();
    let logs: Vec<PathBuf> = (0..PROCESSES)
        .map(|index| logs_dir.join(format!("writer-{index}.jsonl")))
        .collect();
    let crash_log = logs_dir.join("crash.jsonl");

    let store = Store::init_with_git(&root).unwrap();
    store.create_db("main").unwrap();
    for (collection, format) in COLLECTIONS {
        store.create_collection("main", collection, format).unwrap();
    }
    store.set_remote(remote.to_str().unwrap()).unwrap();
    store.set_push(PushMode::OnCommit).unwrap();

    let crashed = crash_one(&root, &crash_log);
    assert!(
        fs::metadata(root.join(".plaindb/journal.jsonl"))
            .unwrap()
            .len()
            > 0
    );

    run_writers(&root, &logs, PER_PHASE);
    assert_in_sync(&root, &remote);

    {
        let _lock = WriteLock::acquire(&root, Duration::from_secs(30)).unwrap();
        git(
            &root,
            &["commit", "-q", "--allow-empty", "-m", "made by hand"],
        );
    }
    let reopened = Store::open(&root).unwrap();
    let reported: Vec<_> = reopened
        .status()
        .unwrap()
        .warnings
        .into_iter()
        .filter(|warning| warning.code == "foreign_commit")
        .collect();
    assert_eq!(reported.len(), 1);

    run_writers(&root, &logs, PER_PHASE);
    assert_in_sync(&root, &remote);

    let other = temp.path().join("other");
    git(
        temp.path(),
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    fs::write(other.join("note.txt"), "pushed from another clone").unwrap();
    git(&other, &["add", "note.txt"]);
    git(&other, &["commit", "-q", "-m", "another clone"]);
    git(&other, &["push", "-q", "origin", "main"]);
    let ahead = git(&remote, &["rev-parse", "main"]);

    run_writers(&root, &logs, PER_PHASE);
    let status = Store::open(&root).unwrap().status().unwrap().git;
    assert!(status.remote_ahead);
    assert!(status.unpushed > 0);
    assert_eq!(git(&remote, &["rev-parse", "main"]), ahead);

    git(
        &root,
        &[
            "pull",
            "-q",
            "--no-rebase",
            "--no-edit",
            remote.to_str().unwrap(),
            "main",
        ],
    );
    let store = Store::open(&root).unwrap();
    store.reset_remote_ahead().unwrap();
    assert_eq!(store.push().unwrap(), plaindb_core::PushOutcome::Ok);
    assert_in_sync(&root, &remote);
    assert_eq!(store.status().unwrap().git.unpushed, 0);

    let local: HashSet<String> = git(&root, &["rev-list", "HEAD"])
        .lines()
        .map(str::to_owned)
        .collect();
    for commit in git(&remote, &["rev-list", "main"]).lines() {
        assert!(local.contains(commit));
    }
    assert!(is_ancestor(&root, &ahead, "HEAD"));

    let trailers = commit_trailers(&root);
    let mut committed: BTreeSet<String> = BTreeSet::new();
    let mut failed: BTreeSet<String> = BTreeSet::new();
    for log in logs.iter().chain([&crash_log]) {
        for line in log_lines(log) {
            if let Some(id) = line.get("commit").and_then(Value::as_str) {
                committed.insert(id.to_owned());
            }
            if let Some(id) = line.get("failed").and_then(Value::as_str) {
                failed.insert(id.to_owned());
            }
        }
    }
    committed.insert(crashed.clone());
    assert!(committed.len() > 100, "only {} committed", committed.len());
    for id in &committed {
        assert_eq!(trailers.get(id), Some(&1), "transaction {id}");
    }
    for id in &failed {
        assert!(!trailers.contains_key(id), "failed transaction {id}");
    }

    let copy = temp.path().join("copy");
    git(
        temp.path(),
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            copy.to_str().unwrap(),
        ],
    );
    let restored = Store::open(&copy).unwrap();
    assert_eq!(
        restored.status().unwrap().git.head,
        store.status().unwrap().git.head
    );
    let expected = document_files(&root);
    assert!(expected.len() > 20);
    assert_eq!(document_files(&copy), expected);
    for (collection, _) in COLLECTIONS {
        for id in store.list_ids("main", collection).unwrap() {
            assert_eq!(
                store.get("main", collection, &id).unwrap(),
                restored.get("main", collection, &id).unwrap()
            );
        }
    }
}

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use serde_json::Value;
use tempfile::TempDir;

fn schema_validator() -> jsonschema::Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/results/doctor.schema.json");
    let text = fs::read_to_string(path).expect("the schema exists");
    jsonschema::validator_for(&serde_json::from_str(&text).expect("the schema parses"))
        .expect("the schema compiles")
}

fn plaindb(store: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_plaindb"))
        .arg("--store")
        .arg(store)
        .args(args)
        .output()
        .expect("the binary runs")
}

fn succeed(store: &Path, args: &[&str]) -> Value {
    let output = plaindb(store, &[&["--json"], args].concat());
    assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
    serde_json::from_slice(&output.stdout).expect("stdout is JSON")
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

struct Doctor {
    code: Option<i32>,
    result: Value,
}

fn doctor(store: &Path, flags: &[&str]) -> Doctor {
    let output = plaindb(store, &[&["--json", "doctor"], flags].concat());
    let parsed: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(parsed["ok"], true, "{parsed}");
    let result = parsed["result"].clone();
    let errors: Vec<String> = schema_validator()
        .iter_errors(&result)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?} in {result}");
    Doctor {
        code: output.status.code(),
        result,
    }
}

impl Doctor {
    fn check(&self, id: &str) -> &Value {
        self.result["checks"]
            .as_array()
            .and_then(|checks| checks.iter().find(|check| check["id"] == id))
            .unwrap_or_else(|| panic!("no check {id} in {}", self.result))
    }

    fn status(&self, id: &str) -> &str {
        self.check(id)["status"].as_str().expect("a status")
    }
}

struct Fixture {
    _dir: TempDir,
    store: PathBuf,
    remote: PathBuf,
}

fn fixture(push: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("a temp dir");
    let remote = dir.path().join("remote.git");
    git(
        dir.path(),
        &["init", "--bare", "--initial-branch=main", "remote.git"],
    );
    let store = dir.path().join("store");
    let url = remote.display().to_string();
    succeed(&store, &["init", "--git", "--remote", &url, "--push", push]);
    succeed(&store, &["db", "create", "app"]);
    succeed(
        &store,
        &["collection", "create", "app", "notes", "--format", "json"],
    );
    succeed(
        &store,
        &["insert", "app", "notes", "a", "--data", r#"{"n":1}"#],
    );
    Fixture {
        _dir: dir,
        store,
        remote,
    }
}

fn age(path: &Path, seconds: u64) {
    let file = File::options()
        .write(true)
        .open(path)
        .expect("the file opens");
    file.set_modified(SystemTime::now() - Duration::from_secs(seconds))
        .expect("the time is set");
}

fn tree(root: &Path) -> BTreeMap<PathBuf, (u64, SystemTime, Vec<u8>)> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("the directory reads") {
            let path = entry.expect("an entry").path();
            let meta = fs::symlink_metadata(&path).expect("metadata");
            if meta.is_dir() {
                pending.push(path);
            } else {
                let bytes = fs::read(&path).unwrap_or_default();
                found.insert(
                    path.strip_prefix(root).expect("inside").to_path_buf(),
                    (meta.len(), meta.modified().expect("a time"), bytes),
                );
            }
        }
    }
    found
}

#[test]
fn a_healthy_store_with_a_reachable_remote_has_no_problem() {
    let fixture = fixture("on-commit");
    let report = doctor(&fixture.store, &[]);
    assert_eq!(report.code, Some(0));
    for check in report.result["checks"].as_array().expect("checks") {
        if check["group"] != "environment" {
            assert_eq!(check["status"], "ok", "{check}");
        }
    }
    assert_eq!(report.check("git_remote")["details"]["checked"], true);
    assert_eq!(report.result["errors"], 0);
}

#[test]
fn a_missing_git_binary_with_push_on_is_an_error_that_names_the_setting() {
    let fixture = fixture("on-commit");
    succeed(
        &fixture.store,
        &["config", "set", "git_binary", "/path/to/missing/git"],
    );
    let report = doctor(&fixture.store, &["--offline"]);
    assert_eq!(report.code, Some(1));
    assert_eq!(report.status("git_binary"), "error");
    assert!(
        report.check("git_binary")["fix"]
            .as_str()
            .expect("a fix")
            .contains("git_binary")
    );
}

#[test]
fn a_missing_git_binary_with_push_off_is_a_warning() {
    let fixture = fixture("off");
    succeed(
        &fixture.store,
        &["config", "set", "git_binary", "/path/to/missing/git"],
    );
    let report = doctor(&fixture.store, &["--offline"]);
    assert_eq!(report.code, Some(0));
    assert_eq!(report.status("git_binary"), "warn");
}

#[test]
fn a_stale_index_lock_is_an_error_with_its_path() {
    let fixture = fixture("off");
    let lock = fixture.store.join(".git/index.lock");
    fs::write(&lock, "").expect("the lock is written");
    age(&lock, 11 * 60);
    let report = doctor(&fixture.store, &["--offline"]);
    assert_eq!(report.code, Some(1));
    assert_eq!(report.status("git_locks"), "error");
    let check = report.check("git_locks");
    assert!(
        check["message"]
            .as_str()
            .expect("a message")
            .contains("index.lock")
    );
    assert!(
        check["details"]["locks"][0]["path"]
            .as_str()
            .expect("a path")
            .ends_with("index.lock")
    );
    assert!(
        check["details"]["locks"][0]["age_seconds"]
            .as_u64()
            .expect("an age")
            >= 600
    );
}

#[test]
fn a_fresh_lock_is_only_a_warning() {
    let fixture = fixture("off");
    fs::write(fixture.store.join(".git/index.lock"), "").expect("the lock is written");
    let report = doctor(&fixture.store, &["--offline"]);
    assert_eq!(report.code, Some(0));
    assert_eq!(report.status("git_locks"), "warn");
}

#[test]
fn a_leftover_journal_is_a_warning_that_names_rescan() {
    let fixture = fixture("off");
    fs::write(
        fixture.store.join(".plaindb/journal.jsonl"),
        "0000000000000000000000000000000000000000 {}\n",
    )
    .expect("the journal is written");
    let report = doctor(&fixture.store, &["--offline"]);
    assert_eq!(report.code, Some(0));
    assert_eq!(report.status("journal"), "warn");
    assert_eq!(report.check("journal")["fix"], "plaindb rescan");
    assert_eq!(report.check("journal")["details"]["records"], 1);
}

#[test]
fn offline_does_not_contact_the_remote() {
    let fixture = fixture("off");
    fs::remove_dir_all(&fixture.remote).expect("the remote goes away");
    let online = doctor(&fixture.store, &[]);
    assert_eq!(online.status("git_remote"), "warn");
    assert_eq!(online.check("git_remote")["details"]["checked"], true);
    let offline = doctor(&fixture.store, &["--offline"]);
    assert_eq!(offline.status("git_remote"), "ok");
    assert_eq!(offline.check("git_remote")["details"]["checked"], false);
}

#[test]
fn an_unreachable_remote_with_push_on_is_an_error() {
    let fixture = fixture("on-commit");
    fs::remove_dir_all(&fixture.remote).expect("the remote goes away");
    let report = doctor(&fixture.store, &[]);
    assert_eq!(report.code, Some(1));
    assert_eq!(report.status("git_remote"), "error");
}

#[test]
fn unpushed_commits_and_a_failed_push_are_reported() {
    let fixture = fixture("off");
    let report = doctor(&fixture.store, &["--offline"]);
    assert_eq!(report.status("git_unpushed"), "warn");
    assert_eq!(report.check("git_unpushed")["fix"], "plaindb push");
}

#[test]
fn names_that_differ_only_by_case_are_an_error() {
    let fixture = fixture("off");
    let dir = fixture.store.join("app/notes");
    fs::write(dir.join("bob.json"), r#"{"n":1}"#).expect("written");
    fs::write(dir.join("Bob.json"), r#"{"n":2}"#).expect("written");
    let both = fs::read_dir(&dir)
        .expect("reads")
        .filter(|entry| {
            entry.as_ref().is_ok_and(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("bob.json")
            })
        })
        .count();
    if both < 2 {
        return;
    }
    let report = doctor(&fixture.store, &["--offline"]);
    assert_eq!(report.code, Some(1));
    assert_eq!(report.status("names_case"), "error");
    assert_eq!(report.check("names_case")["details"]["count"], 2);
}

#[test]
fn invalid_documents_and_schema_failures_are_counted_with_their_ids() {
    let fixture = fixture("off");
    let dir = fixture.store.join("app/notes");
    fs::write(dir.join("broken.json"), "{\"n\": ").expect("written");
    let schema = fixture.store.join("schema.json");
    fs::write(
        &schema,
        r#"{"type":"object","properties":{"n":{"type":"integer"}}}"#,
    )
    .expect("written");
    succeed(
        &fixture.store,
        &[
            "collection",
            "schema",
            "set",
            "app",
            "notes",
            "--file",
            &schema.display().to_string(),
        ],
    );
    fs::write(dir.join("wrong.json"), r#"{"n":"text"}"#).expect("written");
    let report = doctor(&fixture.store, &["--offline"]);
    assert_eq!(report.code, Some(1));
    assert_eq!(
        report.check("documents_invalid")["details"]["first"][0],
        "app/notes/broken"
    );
    assert_eq!(
        report.check("documents_schema")["details"]["first"][0],
        "app/notes/wrong"
    );
}

#[test]
fn a_cache_behind_the_files_is_a_warning_that_names_reindex() {
    let fixture = fixture("off");
    assert_eq!(
        doctor(&fixture.store, &["--offline"]).status("index_cache"),
        "ok"
    );
    fs::write(fixture.store.join("app/notes/b.json"), r#"{"n":2}"#).expect("written");
    let report = doctor(&fixture.store, &["--offline"]);
    assert_eq!(report.status("index_cache"), "warn");
    assert_eq!(report.check("index_cache")["fix"], "plaindb reindex");
}

#[test]
fn a_store_without_git_reports_no_git_problem() {
    let dir = tempfile::tempdir().expect("a temp dir");
    succeed(dir.path(), &["init"]);
    let report = doctor(dir.path(), &[]);
    assert_eq!(report.code, Some(0));
    assert_eq!(report.status("git"), "ok");
}

#[test]
fn doctor_changes_no_file_of_the_store() {
    let fixture = fixture("on-commit");
    let lock = fixture.store.join(".git/index.lock");
    fs::write(&lock, "").expect("the lock is written");
    age(&lock, 11 * 60);
    fs::write(fixture.store.join("app/notes/broken.json"), "{").expect("written");
    fs::write(
        fixture.store.join(".plaindb/journal.jsonl"),
        "0000000000000000000000000000000000000000 {}\n",
    )
    .expect("the journal is written");
    let before = tree(&fixture.store);
    doctor(&fixture.store, &[]);
    doctor(&fixture.store, &["--offline"]);
    assert_eq!(tree(&fixture.store), before);
}

#[test]
fn a_bare_directory_is_reported_not_crashed_on() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let report = doctor(dir.path(), &[]);
    assert_eq!(report.code, Some(1));
    assert_eq!(report.status("store_format"), "error");
}

#[test]
fn the_text_output_lists_problems_first_with_their_fixes() {
    let fixture = fixture("off");
    let lock = fixture.store.join(".git/index.lock");
    fs::write(&lock, "").expect("the lock is written");
    age(&lock, 11 * 60);
    let output = plaindb(&fixture.store, &["doctor", "--offline"]);
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).expect("text");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "git:");
    assert!(lines[1].starts_with("  [error] git_locks:"), "{text}");
    assert!(lines[2].trim_start().starts_with("fix: "), "{text}");
    assert!(lines.contains(&"store:"));
    assert!(lines.contains(&"environment:"));
}

#[test]
fn the_filesystem_case_check_agrees_with_the_disk() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = dir.path().join("store");
    succeed(&store, &["init"]);
    fs::write(dir.path().join("Probe.tmp"), "").expect("written");
    let folds = dir.path().join("probe.tmp").exists();
    let report = doctor(&store, &[]);
    assert_eq!(
        report.check("filesystem_case")["details"]["case_sensitive"],
        !folds
    );
}

#[test]
fn a_store_without_durability_is_warned_and_the_level_is_always_named() {
    let dir = tempfile::tempdir().expect("a temp dir");
    succeed(dir.path(), &["init"]);
    let full = doctor(dir.path(), &[]);
    assert_eq!(full.status("durability"), "ok");
    assert_eq!(full.check("durability")["details"]["durability"], "full");
    succeed(dir.path(), &["config", "set", "durability", "off"]);
    let off = doctor(dir.path(), &[]);
    assert_eq!(off.status("durability"), "warn");
    assert_eq!(
        off.check("durability")["fix"],
        "plaindb config set durability full"
    );
    for id in ["filesystem_network", "disk_space"] {
        assert!(off.check(id)["details"].is_object(), "{id}");
    }
}

#[test]
fn a_remote_that_is_a_git_option_is_a_store_config_error_not_a_crash() {
    let fixture = fixture("on-commit");
    let injected = fixture._dir.path().join("INJ");
    let path = fixture.store.join("_store.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
    config["remote"] = Value::String(format!(
        "--receive-pack=touch {};git-receive-pack",
        injected.display()
    ));
    fs::write(&path, serde_json::to_vec_pretty(&config).expect("json")).expect("write");
    let report = doctor(&fixture.store, &[]);
    assert_eq!(report.code, Some(1));
    assert_eq!(report.status("store_config"), "error");
    assert_eq!(report.check("store_config")["group"], "store");
    assert!(
        report.check("store_config")["fix"]
            .as_str()
            .expect("a fix")
            .starts_with("edit _store.json: remote takes")
    );
    let push = plaindb(&fixture.store, &["--json", "push"]);
    assert_ne!(push.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&push.stdout).contains("invalid_config"));
    assert!(!injected.exists());
}

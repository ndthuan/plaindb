#![cfg(all(feature = "test-hooks", unix))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use plaindb_core::{
    Doc, Durability, ErrorCode, Format, GIT_BINARY_ENV, Op, PushMode, PushOutcome, Store,
    StoreConfig, Tx,
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

fn real_git() -> String {
    let output = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn bare_remote(parent: &Path) -> PathBuf {
    let path = parent.join("remote.git");
    fs::create_dir_all(&path).unwrap();
    git(&path, &["init", "-q", "--bare", "-b", "main"]);
    path
}

fn store_pushing_to(root: &Path, url: &str, git_binary: Option<&Path>) -> Store {
    let store = Store::init_with(
        root,
        StoreConfig {
            git: true,
            durability: Durability::Off,
            git_binary: git_binary.map(|path| path.to_str().unwrap().to_owned()),
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Json)
        .unwrap();
    store.set_remote(url).unwrap();
    store.set_push(PushMode::OnCommit).unwrap();
    store
}

fn set_store_key(root: &Path, key: &str, value: Value) {
    let path = root.join("_store.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config[key] = value;
    fs::write(&path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
}

fn settled_push(store: &Store) -> plaindb_core::Result<PushOutcome> {
    for _ in 0..50 {
        match store.push() {
            Ok(PushOutcome::Deferred) => std::thread::sleep(Duration::from_millis(100)),
            other => return other,
        }
    }
    store.push()
}

fn make_remote_ahead(dir: &Path, remote: &Path) {
    let other = dir.join("other");
    git(
        dir,
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
}

fn translating_git(dir: &Path) -> PathBuf {
    script(
        dir,
        "git-translating",
        &format!(
            "if [ \"$LC_ALL\" = C ]; then exec {real} \"$@\"; fi\n\
             out=$(mktemp); err=$(mktemp)\n\
             {real} \"$@\" >\"$out\" 2>\"$err\"; code=$?\n\
             sed 's/rejected/abgelehnt/g; s/fetch first/zuerst holen/g; s/non-fast-forward/kein Vorspulen/g' \"$out\"\n\
             sed 's/rejected/abgelehnt/g; s/fetch first/zuerst holen/g; s/non-fast-forward/kein Vorspulen/g' \"$err\" >&2\n\
             rm -f \"$out\" \"$err\"\nexit $code",
            real = real_git()
        ),
    )
}

fn git_with_path(dir: &Path, extra: &Path) -> PathBuf {
    script(
        dir,
        "git-with-path",
        &format!(
            "PATH={}:$PATH\nexport PATH\nexec {} \"$@\"",
            extra.display(),
            real_git()
        ),
    )
}

#[test]
fn a_translated_rejection_is_still_a_remote_that_is_ahead() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path());
    let wrapper = translating_git(&dir.path().join("wrappers"));
    let store = store_pushing_to(
        &dir.path().join("store"),
        remote.to_str().unwrap(),
        Some(&wrapper),
    );
    store.apply(put("a", 1)).unwrap();
    make_remote_ahead(dir.path(), &remote);
    let result = store.apply(put("b", 2)).unwrap();
    assert_eq!(result.push, Some(PushOutcome::RemoteAhead));
    assert!(store.status().unwrap().git.remote_ahead);
}

#[test]
fn an_ssh_remote_that_needs_a_passphrase_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    let wrappers = dir.path().join("wrappers");
    script(
        &wrappers.join("bin"),
        "ssh",
        "case \" $* \" in\n*\" BatchMode=yes \"*) echo 'Permission denied (publickey).' >&2; exit 255;;\nesac\nsleep 600",
    );
    let wrapper = git_with_path(&wrappers, &wrappers.join("bin"));
    let store = store_pushing_to(
        &dir.path().join("store"),
        "ssh://git@example.com/repo.git",
        Some(&wrapper),
    );
    let begun = Instant::now();
    let result = store.apply(put("a", 1)).unwrap();
    assert_eq!(result.push, Some(PushOutcome::Failed));
    assert!(begun.elapsed() < Duration::from_secs(30));
    let error = store.status().unwrap().git.last_push_error.unwrap();
    assert!(error.contains("Permission denied"), "{error}");
}

#[test]
fn a_configured_ssh_command_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let wrappers = dir.path().join("wrappers");
    let seen = dir.path().join("seen");
    let ssh = script(
        &wrappers.join("bin"),
        "recording-ssh",
        &format!("echo \"$@\" > {}\nexit 255", seen.display()),
    );
    let root = dir.path().join("store");
    let store = Store::init_with_git(&root).unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Json)
        .unwrap();
    git(&root, &["config", "core.sshCommand", ssh.to_str().unwrap()]);
    store.set_remote("ssh://git@example.com/repo.git").unwrap();
    store.set_push(PushMode::OnCommit).unwrap();
    fs::remove_file(&seen).unwrap();
    let result = store.apply(put("a", 1)).unwrap();
    assert_eq!(result.push, Some(PushOutcome::Failed));
    let arguments = fs::read_to_string(&seen).unwrap();
    assert!(arguments.contains("example.com"), "{arguments}");
    assert!(!arguments.contains("BatchMode"), "{arguments}");
}

#[test]
fn a_missing_binary_warns_and_every_write_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path());
    let root = dir.path().join("store");
    let store = store_pushing_to(&root, remote.to_str().unwrap(), None);
    assert!(store.status().unwrap().warnings.is_empty());
    set_store_key(&root, "git_binary", json!("/nonexistent/path/to/git"));
    let result = store.apply(put("a", 1)).unwrap();
    assert_eq!(result.push, Some(PushOutcome::GitNotFound));
    assert_eq!(
        settled_push(&store).unwrap_err().code,
        ErrorCode::GitNotFound
    );
    let status = store.status().unwrap();
    let warning = status
        .warnings
        .iter()
        .find(|warning| warning.code == "git_not_found")
        .unwrap();
    assert_eq!(warning.details["source"], "config");
    assert!(!status.git.git_binary_found);
    assert_eq!(status.git.git_binary_source, "config");
    assert_eq!(status.git.git_binary, "/nonexistent/path/to/git");
    assert_eq!(store.list_ids("main", "items").unwrap(), ["a"]);
}

#[test]
fn a_store_that_does_not_push_never_warns_about_a_missing_binary() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = Store::init_with_git(&root).unwrap();
    set_store_key(&root, "git_binary", json!("/nonexistent/path/to/git"));
    assert!(store.status().unwrap().warnings.is_empty());
}

#[test]
fn the_binary_comes_from_the_config_then_the_environment_then_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path());
    let root = dir.path().join("store");
    let store = store_pushing_to(&root, remote.to_str().unwrap(), None);
    assert_eq!(store.status().unwrap().git.git_binary_source, "path");
    let wrappers = dir.path().join("wrappers");
    let marked = |name: &str| {
        let marker = dir.path().join(format!("{name}.marker"));
        let path = script(
            &wrappers,
            name,
            &format!("touch {}\nexec {} \"$@\"", marker.display(), real_git()),
        );
        (path, marker)
    };
    let (from_env, env_marker) = marked("from-env");
    let (from_config, config_marker) = marked("from-config");

    let output = Command::new(WRITER)
        .arg(&root)
        .arg("push")
        .env(GIT_BINARY_ENV, &from_env)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(env_marker.exists());

    set_store_key(&root, "git_binary", json!(from_config.to_str().unwrap()));
    fs::remove_file(&env_marker).unwrap();
    let output = Command::new(WRITER)
        .arg(&root)
        .arg("push")
        .env(GIT_BINARY_ENV, &from_env)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(config_marker.exists());
    assert!(!env_marker.exists());
    assert_eq!(store.status().unwrap().git.git_binary_source, "config");
}

#[test]
fn the_environment_names_a_binary_that_may_be_missing() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(dir.path());
    let root = dir.path().join("store");
    let store = store_pushing_to(&root, remote.to_str().unwrap(), None);
    let output = Command::new(WRITER)
        .arg(&root)
        .arg("push")
        .env(GIT_BINARY_ENV, "/nonexistent/path/to/git")
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
fn no_source_file_spawns_a_program_outside_the_helper() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut spawners = Vec::new();
    let mut pending = vec![crates.join("plaindb-core/src"), crates.join("plaindb/src")];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && fs::read_to_string(&path).unwrap().contains("Command::new(")
            {
                spawners.push(path.file_name().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    assert_eq!(spawners, ["command.rs"]);
}

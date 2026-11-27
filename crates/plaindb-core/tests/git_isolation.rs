#![cfg(all(feature = "test-hooks", unix))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use plaindb_core::{Durability, ErrorCode, Format, PushMode, Store, StoreConfig};

const WRITER: &str = env!("CARGO_BIN_EXE_plaindb-test-writer");

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
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn outer_repository(path: &Path) -> PathBuf {
    fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q", "-b", "main"]);
    fs::write(path.join("readme.txt"), "outer").unwrap();
    git(path, &["add", "readme.txt"]);
    git(path, &["commit", "-q", "-m", "outer"]);
    path.to_path_buf()
}

#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    index: Vec<u8>,
    refs: String,
    config: Vec<u8>,
    commits: String,
}

fn fingerprint(outer: &Path) -> Fingerprint {
    Fingerprint {
        index: fs::read(outer.join(".git/index")).unwrap(),
        refs: git(outer, &["for-each-ref"]),
        config: fs::read(outer.join(".git/config")).unwrap(),
        commits: git(outer, &["rev-list", "--all", "--count"]),
    }
}

fn bare_remote(path: &Path) -> PathBuf {
    fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q", "--bare", "-b", "main"]);
    path.to_path_buf()
}

fn pushing_store(root: &Path, remote: &Path) -> Store {
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
    store.set_remote(remote.to_str().unwrap()).unwrap();
    store.set_push(PushMode::OnCommit).unwrap();
    store
}

fn writer(root: &Path, mode: &str, outer: Option<&Path>) -> Output {
    let mut command = Command::new(WRITER);
    command.arg(root).arg(mode).arg("1");
    if let Some(outer) = outer {
        command
            .env("GIT_DIR", outer.join(".git"))
            .env("GIT_WORK_TREE", outer)
            .env("GIT_INDEX_FILE", outer.join(".git/index"));
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn head(root: &Path) -> String {
    git(root, &["rev-parse", "HEAD"])
}

fn insert_push_and_gc_stay_in_the_store(outer: &Path, root: &Path, remote: &Path, env: bool) {
    let exported = env.then_some(outer);
    let before = fingerprint(outer);
    let head_before = head(root);
    writer(root, "insert", exported);
    writer(root, "push", exported);
    writer(root, "gc", exported);
    assert_eq!(fingerprint(outer), before);
    assert_eq!(git(root, &["rev-parse", "HEAD~1"]), head_before);
    assert_eq!(git(remote, &["rev-parse", "main"]), head(root));
}

#[test]
fn exported_git_variables_do_not_steer_the_store_to_another_repository() {
    let dir = tempfile::tempdir().unwrap();
    let outer = outer_repository(&dir.path().join("outer"));
    let remote = bare_remote(&dir.path().join("remote.git"));
    let root = dir.path().join("store");
    drop(pushing_store(&root, &remote));
    insert_push_and_gc_stay_in_the_store(&outer, &root, &remote, true);
}

#[test]
fn a_store_nested_in_another_worktree_keeps_to_its_own_repository() {
    let dir = tempfile::tempdir().unwrap();
    let outer = outer_repository(&dir.path().join("outer"));
    let remote = bare_remote(&dir.path().join("remote.git"));
    let root = outer.join("data/store");
    drop(pushing_store(&root, &remote));
    insert_push_and_gc_stay_in_the_store(&outer, &root, &remote, false);
}

#[test]
fn a_store_whose_repository_is_gone_never_falls_back_to_an_enclosing_one() {
    let dir = tempfile::tempdir().unwrap();
    let outer = outer_repository(&dir.path().join("outer"));
    let remote = bare_remote(&dir.path().join("remote.git"));
    let root = outer.join("data/store");
    let store = pushing_store(&root, &remote);
    let before = fingerprint(&outer);
    fs::remove_dir_all(root.join(".git")).unwrap();
    let error = store.push().unwrap_err();
    assert_eq!(error.code, ErrorCode::GitDisabled);
    assert_eq!(error.details["reason"], "repository_missing");
    let error = store.set_remote("/path/to/elsewhere.git").unwrap_err();
    assert_eq!(error.code, ErrorCode::GitDisabled);
    assert_eq!(error.details["reason"], "repository_missing");
    let error = store.gc().unwrap_err();
    assert_eq!(error.code, ErrorCode::GitDisabled);
    let status = store.status().unwrap();
    assert!(
        status
            .warnings
            .iter()
            .any(|warning| warning.code == "git_repository_missing"),
        "{:?}",
        status.warnings
    );
    assert_eq!(fingerprint(&outer), before);
}

#[test]
fn a_store_opened_by_a_relative_path_commits_and_pushes() {
    let dir = tempfile::tempdir().unwrap();
    let remote = bare_remote(&dir.path().join("remote.git"));
    let root = dir.path().join("s");
    drop(pushing_store(&root, &remote));
    let head_before = head(&root);
    for mode in ["insert", "push"] {
        let output = Command::new(WRITER)
            .current_dir(dir.path())
            .arg("s")
            .arg(mode)
            .arg("1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(git(&root, &["rev-parse", "HEAD~1"]), head_before);
    assert_eq!(git(&remote, &["rev-parse", "main"]), head(&root));
}

#[test]
fn the_author_comes_from_the_global_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let outer = outer_repository(&dir.path().join("outer"));
    let root = dir.path().join("store");
    let store = Store::init_with_git(&root).unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Json)
        .unwrap();
    let global = dir.path().join("global.gitconfig");
    fs::write(
        &global,
        "[user]\n\tname = Global Writer\n\temail = global@example.com\n",
    )
    .unwrap();
    let output = Command::new(WRITER)
        .arg(&root)
        .arg("insert")
        .arg("1")
        .env("GIT_CONFIG_GLOBAL", &global)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_DIR", outer.join(".git"))
        .env("GIT_INDEX_FILE", outer.join(".git/index"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        git(&root, &["log", "-1", "--format=%an <%ae>"]),
        "Global Writer <global@example.com>"
    );
}

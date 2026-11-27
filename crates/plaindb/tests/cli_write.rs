use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use tempfile::TempDir;

struct Run {
    code: i32,
    json: Value,
}

fn schema_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schemas")
        .join(relative)
}

fn validator(relative: &str) -> jsonschema::Validator {
    let text = fs::read_to_string(schema_path(relative)).expect("the schema exists");
    jsonschema::validator_for(&serde_json::from_str(&text).expect("the schema parses"))
        .expect("the schema compiles")
}

fn assert_valid(validator: &jsonschema::Validator, value: &Value) {
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?} in {value}");
}

fn exec(store: &Path, kind: Option<&str>, args: &[&str], stdin: Option<&str>) -> Run {
    let mut child = Command::new(env!("CARGO_BIN_EXE_plaindb"))
        .arg("--store")
        .arg(store)
        .arg("--json")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    let mut input = child.stdin.take().expect("stdin is piped");
    if let Some(text) = stdin {
        input.write_all(text.as_bytes()).expect("stdin is written");
    }
    drop(input);
    let output = child.wait_with_output().expect("the binary finishes");
    let stdout = String::from_utf8(output.stdout).expect("stdout is text");
    assert_eq!(stdout.lines().count(), 1, "one line expected: {stdout:?}");
    let parsed: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_valid(&validator("response.schema.json"), &parsed);
    if let (Some(kind), Some(result)) = (kind, parsed.get("result")) {
        assert_valid(&validator(&format!("results/{kind}.schema.json")), result);
    }
    Run {
        code: output.status.code().expect("the binary exits"),
        json: parsed,
    }
}

fn plaindb(store: &Path, kind: Option<&str>, args: &[&str]) -> Run {
    exec(store, kind, args, None)
}

fn succeed(store: &Path, kind: &str, args: &[&str]) -> Value {
    let run = plaindb(store, Some(kind), args);
    assert_eq!(run.code, 0, "{args:?} failed: {}", run.json);
    run.json["result"].clone()
}

fn fail(store: &Path, args: &[&str]) -> String {
    let run = plaindb(store, None, args);
    assert_eq!(run.code, 1, "{args:?} should fail: {}", run.json);
    run.json["error"]["code"]
        .as_str()
        .expect("a code")
        .to_owned()
}

fn store_with_collection(format: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    succeed(dir.path(), "init", &["init"]);
    succeed(dir.path(), "db_ack", &["db", "create", "app"]);
    succeed(
        dir.path(),
        "collection_ack",
        &["collection", "create", "app", "notes", "--format", format],
    );
    dir
}

fn count(store: &Path) -> u64 {
    succeed(store, "count", &["count", "app", "notes"])["count"]
        .as_u64()
        .expect("a count")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout)
        .expect("text")
        .trim()
        .to_owned()
}

#[test]
fn insert_twice_with_one_id_is_already_exists() {
    let dir = store_with_collection("json");
    let first = succeed(
        dir.path(),
        "write",
        &["insert", "app", "notes", "a", "--data", r#"{"n":1}"#],
    );
    assert_eq!(first["results"][0]["id"], "a");
    assert_eq!(first["results"][0]["previous_version"], Value::Null);
    let code = fail(
        dir.path(),
        &["insert", "app", "notes", "a", "--data", r#"{"n":2}"#],
    );
    assert_eq!(code, "already_exists");
    assert_eq!(count(dir.path()), 1);
}

#[test]
fn insert_without_an_id_makes_one() {
    let dir = store_with_collection("json");
    let result = succeed(
        dir.path(),
        "write",
        &["insert", "app", "notes", "--data", r#"{"n":1}"#],
    );
    let id = result["results"][0]["id"].as_str().expect("an id");
    assert_eq!(id.len(), 26);
    let found = succeed(dir.path(), "get", &["get", "app", "notes", id]);
    assert_eq!(found["doc"]["n"], 1);
}

#[test]
fn writes_check_the_version_they_were_given() {
    let dir = store_with_collection("json");
    let first = succeed(
        dir.path(),
        "write",
        &["put", "app", "notes", "a", "--data", r#"{"n":1}"#],
    );
    let version = first["results"][0]["version"]
        .as_str()
        .expect("a version")
        .to_owned();
    let stale = "0".repeat(40);
    let stale_put = [
        "put",
        "app",
        "notes",
        "a",
        "--data",
        r#"{"n":2}"#,
        "--if-version",
        &stale,
    ];
    assert_eq!(fail(dir.path(), &stale_put), "version_conflict");
    let stale_patch = [
        "patch",
        "app",
        "notes",
        "a",
        "--data",
        r#"{"n":3}"#,
        "--if-version",
        &stale,
    ];
    assert_eq!(fail(dir.path(), &stale_patch), "version_conflict");
    let stale_delete = ["delete", "app", "notes", "a", "--if-version", &stale];
    assert_eq!(fail(dir.path(), &stale_delete), "version_conflict");
    let patched = succeed(
        dir.path(),
        "write",
        &[
            "patch",
            "app",
            "notes",
            "a",
            "--data",
            r#"{"n":null,"m":5}"#,
            "--if-version",
            &version,
        ],
    );
    assert_eq!(patched["results"][0]["previous_version"], version);
    let found = succeed(dir.path(), "get", &["get", "app", "notes", "a"]);
    assert_eq!(found["doc"]["m"], 5);
    assert!(found["doc"].get("n").is_none());
    let current = found["version"].as_str().expect("a version");
    let deleted = succeed(
        dir.path(),
        "write",
        &["delete", "app", "notes", "a", "--if-version", current],
    );
    assert_eq!(deleted["results"][0]["version"], Value::Null);
    assert_eq!(count(dir.path()), 0);
}

#[test]
fn documents_come_from_files_and_standard_input() {
    let dir = store_with_collection("md");
    let source = tempfile::tempdir().expect("a temp dir");
    let note = source.path().join("note.md");
    fs::write(&note, "---\ntitle: Hello\n---\nBody text\n").expect("a file");
    let path = note.to_str().expect("a path");
    succeed(
        dir.path(),
        "write",
        &["put", "app", "notes", "hello", "--file", path],
    );
    let found = succeed(dir.path(), "get", &["get", "app", "notes", "hello"]);
    assert_eq!(found["doc"]["title"], "Hello");
    assert_eq!(found["doc"]["body"], "Body text\n");
    let piped = exec(
        dir.path(),
        Some("write"),
        &["put", "app", "notes", "piped", "--file", "-"],
        Some(r#"{"title":"Piped","body":"text"}"#),
    );
    assert_eq!(piped.code, 0, "{}", piped.json);
    let default_input = exec(
        dir.path(),
        Some("write"),
        &["insert", "app", "notes", "second"],
        Some(r#"{"title":"Second","body":"x"}"#),
    );
    assert_eq!(default_input.code, 0);
    assert_eq!(count(dir.path()), 3);
}

#[test]
fn a_transaction_file_applies_all_or_nothing() {
    let dir = store_with_collection("json");
    let good = json!([
        { "op": "insert", "db": "app", "collection": "notes", "id": "a", "doc": { "n": 1 } },
        { "op": "put", "db": "app", "collection": "notes", "id": "b", "doc": { "n": 2 } },
        { "op": "patch", "db": "app", "collection": "notes", "id": "a", "patch": { "n": 5 } },
    ]);
    let applied = exec(
        dir.path(),
        Some("write"),
        &["tx", "--file", "-"],
        Some(&good.to_string()),
    );
    assert_eq!(applied.code, 0, "{}", applied.json);
    assert_eq!(
        applied.json["result"]["results"].as_array().map(Vec::len),
        Some(3)
    );
    assert_eq!(count(dir.path()), 2);

    let bad_op = json!([
        { "op": "insert", "db": "app", "collection": "notes", "id": "c", "doc": {} },
        { "op": "explode", "db": "app", "collection": "notes", "id": "d" },
    ]);
    let refused = exec(
        dir.path(),
        None,
        &["tx", "--file", "-"],
        Some(&bad_op.to_string()),
    );
    assert_eq!(refused.code, 1);
    assert_eq!(refused.json["error"]["code"], "invalid_document");
    assert_eq!(count(dir.path()), 2);

    let clash = json!([
        { "op": "insert", "db": "app", "collection": "notes", "id": "e", "doc": {} },
        { "op": "insert", "db": "app", "collection": "notes", "id": "a", "doc": {} },
    ]);
    let failed = exec(
        dir.path(),
        None,
        &["tx", "--file", "-"],
        Some(&clash.to_string()),
    );
    assert_eq!(failed.json["error"]["code"], "already_exists");
    assert_eq!(count(dir.path()), 2);
}

#[test]
fn a_repeated_transaction_id_writes_once() {
    let dir = store_with_collection("json");
    let id = "01JABCDEFGHJKMNPQRSTVWXYZ0";
    let args = ["--tx-id", id, "insert", "app", "notes", "a", "--data", "{}"];
    let first = succeed(dir.path(), "write", &args);
    assert_eq!(first["tx_id"], id);
    let again = succeed(dir.path(), "write", &args);
    assert_eq!(again["results"], first["results"]);
    assert_eq!(count(dir.path()), 1);
    assert_eq!(
        fail(
            dir.path(),
            &[
                "--tx-id", "nope", "insert", "app", "notes", "b", "--data", "{}"
            ]
        ),
        "invalid_name"
    );
}

#[test]
fn dropping_what_is_missing_is_not_found() {
    let dir = store_with_collection("json");
    assert_eq!(
        fail(dir.path(), &["collection", "drop", "app", "missing"]),
        "not_found"
    );
    assert_eq!(fail(dir.path(), &["db", "drop", "missing"]), "not_found");
    assert_eq!(fail(dir.path(), &["db", "create", "app"]), "already_exists");
    succeed(
        dir.path(),
        "collection_ack",
        &["collection", "drop", "app", "notes"],
    );
    assert_eq!(
        succeed(dir.path(), "collections", &["collection", "list", "app"])["collections"],
        json!([])
    );
    succeed(dir.path(), "db_ack", &["db", "drop", "app"]);
    assert_eq!(
        succeed(dir.path(), "dbs", &["db", "list"])["dbs"],
        json!([])
    );
}

#[test]
fn schemas_and_indexes_are_managed_from_the_command_line() {
    let dir = store_with_collection("json");
    let source = tempfile::tempdir().expect("a temp dir");
    let file = source.path().join("schema.json");
    fs::write(
        &file,
        r#"{"type":"object","properties":{"n":{"type":"integer"}}}"#,
    )
    .expect("a file");
    let path = file.to_str().expect("a path");
    let set = succeed(
        dir.path(),
        "collection_ack",
        &[
            "collection",
            "schema",
            "set",
            "app",
            "notes",
            "--file",
            path,
        ],
    );
    assert_eq!(set["has_schema"], true);
    assert_eq!(
        fail(
            dir.path(),
            &["insert", "app", "notes", "a", "--data", r#"{"n":"x"}"#]
        ),
        "schema_violation"
    );
    succeed(
        dir.path(),
        "collection_ack",
        &["collection", "schema", "set", "app", "notes", "--clear"],
    );
    succeed(
        dir.path(),
        "write",
        &["insert", "app", "notes", "a", "--data", r#"{"n":"x"}"#],
    );

    succeed(
        dir.path(),
        "collection_ack",
        &["index", "add", "app", "notes", "--field", "n", "--unique"],
    );
    succeed(
        dir.path(),
        "collection_ack",
        &["index", "add", "app", "notes", "--text", "title,body"],
    );
    assert_eq!(
        fail(
            dir.path(),
            &["index", "add", "app", "notes", "--field", "n"]
        ),
        "already_exists"
    );
    let listed = succeed(dir.path(), "indexes", &["index", "list", "app", "notes"]);
    assert_eq!(listed["indexes"].as_array().map(Vec::len), Some(2));
    succeed(
        dir.path(),
        "collection_ack",
        &["index", "drop", "app", "notes", "n"],
    );
    succeed(
        dir.path(),
        "collection_ack",
        &["index", "drop", "app", "notes", "text"],
    );
    assert_eq!(
        fail(dir.path(), &["index", "drop", "app", "notes", "n"]),
        "not_found"
    );
    let empty = succeed(dir.path(), "indexes", &["index", "list", "app", "notes"]);
    assert_eq!(empty["indexes"], json!([]));
}

#[test]
fn settings_are_read_and_changed() {
    let dir = store_with_collection("json");
    let all = succeed(dir.path(), "config", &["config", "get"]);
    assert_eq!(all["value"]["commit"]["value"], "each");
    assert_eq!(all["value"]["commit"]["default"], true);
    let limit = succeed(
        dir.path(),
        "config",
        &["config", "set", "limits.max_results", "7"],
    );
    assert_eq!(limit["value"], 7);
    assert_eq!(
        succeed(
            dir.path(),
            "config",
            &["config", "get", "limits.max_results"]
        )["value"],
        7
    );
    let cleared = succeed(
        dir.path(),
        "config",
        &["config", "set", "limits.max_results", ""],
    );
    assert_eq!(cleared["value"], Value::Null);
    succeed(
        dir.path(),
        "config",
        &["config", "set", "lock_timeout_ms", "1234"],
    );
    succeed(dir.path(), "config", &["config", "set", "branch", "trunk"]);
    assert_eq!(
        succeed(dir.path(), "config", &["config", "get", "branch"])["value"],
        "trunk"
    );
    succeed(dir.path(), "config", &["config", "set", "commit", "manual"]);
    assert_eq!(
        succeed(dir.path(), "config", &["config", "get", "commit"])["value"],
        "manual"
    );
    assert_eq!(
        fail(dir.path(), &["config", "set", "lock_timeout_ms", "soon"]),
        "invalid_config"
    );
    assert_eq!(
        fail(dir.path(), &["config", "set", "commit", "sometimes"]),
        "invalid_config"
    );
    assert_eq!(
        fail(dir.path(), &["config", "set", "colour", "red"]),
        "invalid_name"
    );
    assert_eq!(
        fail(dir.path(), &["config", "get", "colour"]),
        "invalid_name"
    );
}

#[test]
fn the_newer_settings_round_trip() {
    let dir = store_with_collection("json");
    for (key, value, expected) in [
        ("durability", "normal", json!("normal")),
        ("durability", "off", json!("off")),
        ("gc_every", "0", json!(0)),
        ("gc_every", "25", json!(25)),
        ("lock_timeout_ms", "250", json!(250)),
    ] {
        let set = succeed(dir.path(), "config", &["config", "set", key, value]);
        assert_eq!(set["value"], expected);
        assert_eq!(
            succeed(dir.path(), "config", &["config", "get", key])["value"],
            expected
        );
    }
}

#[test]
fn a_bad_setting_value_writes_nothing() {
    let dir = store_with_collection("json");
    let before = std::fs::read(dir.path().join("_store.json")).unwrap();
    for (key, value) in [
        ("durability", "sturdy"),
        ("gc_every", "often"),
        ("gc_every", "-1"),
        ("push", "always"),
        ("git_binary", "/usr/bin/git"),
    ] {
        assert_eq!(
            fail(dir.path(), &["config", "set", key, value]),
            "invalid_config"
        );
    }
    assert_eq!(
        std::fs::read(dir.path().join("_store.json")).unwrap(),
        before
    );
}

#[test]
fn the_listing_marks_what_differs_from_the_default() {
    let dir = store_with_collection("json");
    succeed(dir.path(), "config", &["config", "set", "gc_every", "9"]);
    let all = succeed(dir.path(), "config", &["config", "get"]);
    assert_eq!(
        all["value"]["gc_every"],
        json!({"value": 9, "default": false})
    );
    assert_eq!(
        all["value"]["durability"],
        json!({"value": "full", "default": true})
    );
    for key in ["limits.max_scan_ms", "branch", "remote"] {
        assert!(all["value"].get(key).is_some(), "{key}");
    }
    assert!(all["value"].get("git_binary").is_none());
}

#[test]
fn a_limit_set_from_the_command_line_is_enforced() {
    let dir = store_with_collection("json");
    succeed(
        dir.path(),
        "config",
        &["config", "set", "limits.max_document_bytes", "20"],
    );
    let big = format!(r#"{{"text":"{}"}}"#, "x".repeat(100));
    assert_eq!(
        fail(dir.path(), &["insert", "app", "notes", "a", "--data", &big]),
        "limit_exceeded"
    );
}

#[test]
fn a_setting_is_committed_when_git_is_on() {
    let dir = tempfile::tempdir().expect("a temp dir");
    succeed(dir.path(), "init", &["init", "--git"]);
    succeed(
        dir.path(),
        "config",
        &["config", "set", "lock_timeout_ms", "4000"],
    );
    assert_eq!(
        git(dir.path(), &["log", "-1", "--format=%s"]),
        "meta store: set lock_timeout_ms"
    );
    let status = succeed(dir.path(), "status", &["status"]);
    assert_eq!(status["git"]["pending_paths"], 0);
}

#[test]
fn pushes_follow_the_push_setting() {
    let workspace = tempfile::tempdir().expect("a temp dir");
    let remote = workspace.path().join("remote.git");
    git(
        workspace.path(),
        &["init", "--bare", "-q", remote.to_str().expect("a path")],
    );
    let store = workspace.path().join("store");
    let init = succeed(
        &store,
        "init",
        &[
            "init",
            "--git",
            "--remote",
            remote.to_str().expect("a path"),
            "--push",
            "on-commit",
        ],
    );
    assert_eq!(init["push"], "on-commit");
    succeed(&store, "db_ack", &["db", "create", "app"]);
    succeed(
        &store,
        "collection_ack",
        &["collection", "create", "app", "notes", "--format", "json"],
    );
    let written = succeed(
        &store,
        "write",
        &["insert", "app", "notes", "a", "--data", "{}"],
    );
    assert_eq!(written["push"], "ok");
    let head = succeed(&store, "status", &["status"])["git"]["head"]
        .as_str()
        .expect("a head")
        .to_owned();
    assert_eq!(git(&remote, &["rev-parse", "refs/heads/main"]), head);

    succeed(&store, "config", &["config", "set", "push", "off"]);
    let pushed = git(&remote, &["rev-parse", "refs/heads/main"]);
    succeed(
        &store,
        "write",
        &["insert", "app", "notes", "b", "--data", "{}"],
    );
    assert_eq!(git(&remote, &["rev-parse", "refs/heads/main"]), pushed);
    let status = succeed(&store, "status", &["status"]);
    assert_eq!(status["git"]["unpushed"], 2);
    let outcome = succeed(&store, "push", &["push"]);
    assert_eq!(outcome["outcome"], "ok");
    assert_eq!(
        git(&remote, &["rev-parse", "refs/heads/main"]),
        succeed(&store, "status", &["status"])["git"]["head"]
            .as_str()
            .expect("a head")
    );
}

#[test]
fn git_is_enabled_later_and_commits_are_manual() {
    let dir = store_with_collection("json");
    assert_eq!(fail(dir.path(), &["commit"]), "git_disabled");
    succeed(dir.path(), "git", &["git", "enable"]);
    succeed(dir.path(), "config", &["config", "set", "commit", "manual"]);
    succeed(
        dir.path(),
        "write",
        &["insert", "app", "notes", "a", "--data", "{}"],
    );
    let status = succeed(dir.path(), "status", &["status"]);
    assert_eq!(status["git"]["pending_txs"], 1);
    let committed = succeed(
        dir.path(),
        "commit",
        &["--actor", "tester", "commit", "-m", "first batch"],
    );
    let id = committed["commit"].as_str().expect("a commit");
    assert_eq!(
        git(dir.path(), &["log", "-1", "--format=%s"]),
        "first batch"
    );
    assert_eq!(
        succeed(dir.path(), "status", &["status"])["git"]["head"],
        id
    );
    assert_eq!(
        succeed(dir.path(), "commit", &["commit"])["commit"],
        Value::Null
    );
    let history = succeed(dir.path(), "history", &["history", "app", "notes", "a"]);
    assert_eq!(history["entries"][0]["actor"], "tester");
    let remote = succeed(
        dir.path(),
        "remote",
        &["git", "remote", "set", "https://example.com/data.git"],
    );
    assert_eq!(remote["remote"], "origin");
    assert_eq!(
        succeed(dir.path(), "status", &["status"])["git"]["remote_url"],
        "https://example.com/data.git"
    );
}

#[test]
fn validate_rescan_and_reindex_see_hand_edits() {
    let dir = store_with_collection("json");
    succeed(
        dir.path(),
        "write",
        &["insert", "app", "notes", "a", "--data", r#"{"n":1}"#],
    );
    let clean = succeed(dir.path(), "validate", &["validate"]);
    assert_eq!(clean["valid"], true);
    assert_eq!(clean["documents"], 1);
    fs::write(dir.path().join("app/notes/bad.json"), "{not json").expect("a hand edit");
    fs::write(dir.path().join("app/notes/b.json"), r#"{"_id":"b","n":2}"#).expect("a hand edit");
    let found = succeed(dir.path(), "validate", &["validate"]);
    assert_eq!(found["valid"], false);
    assert_eq!(found["invalid"][0]["id"], "bad");
    assert_eq!(found["invalid"][0]["path"], "app/notes/bad.json");
    assert_eq!(found["documents"], 2);
    let scanned = succeed(dir.path(), "scan", &["rescan"]);
    assert_eq!(scanned["documents"], 2);
    assert_eq!(scanned["invalid_files"], 1);
    fs::remove_file(dir.path().join("app/notes/bad.json")).expect("removed");
    let reindexed = succeed(dir.path(), "scan", &["reindex"]);
    assert_eq!(reindexed["documents"], 2);
    assert_eq!(reindexed["invalid_files"], 0);
}

#[test]
fn init_refuses_a_directory_that_holds_a_store() {
    let dir = store_with_collection("json");
    assert_eq!(fail(dir.path(), &["init"]), "already_exists");
    assert_eq!(
        fail(
            dir.path(),
            &["init", "--git", "--remote", "https://example.com/x.git"]
        ),
        "already_exists"
    );
}

fn set_tree_writable(path: &Path, writable: bool) {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::metadata(path).expect("the path exists");
    let mut permissions = metadata.permissions();
    let mode = permissions.mode();
    permissions.set_mode(if writable {
        mode | 0o200
    } else {
        mode & !0o222
    });
    fs::set_permissions(path, permissions).expect("permissions change");
    if metadata.is_dir() {
        for entry in fs::read_dir(path).expect("the directory reads") {
            set_tree_writable(&entry.expect("an entry").path(), writable);
        }
    }
}

#[test]
fn a_write_whose_commit_fails_is_reported_as_written() {
    let dir = tempfile::tempdir().expect("a temp dir");
    succeed(dir.path(), "init", &["init", "--git"]);
    succeed(dir.path(), "db_ack", &["db", "create", "app"]);
    succeed(
        dir.path(),
        "collection_ack",
        &["collection", "create", "app", "notes", "--format", "json"],
    );
    let objects = dir.path().join(".git").join("objects");
    set_tree_writable(&objects, false);
    if fs::write(objects.join("probe"), b"x").is_ok() {
        set_tree_writable(&objects, true);
        return;
    }
    let result = succeed(
        dir.path(),
        "write",
        &["insert", "app", "notes", "a1", "--data", r#"{"t":1}"#],
    );
    assert_eq!(result["deferred"]["step"], "git");
    assert!(result["deferred"]["message"].as_str().is_some());
    let output = Command::new(env!("CARGO_BIN_EXE_plaindb"))
        .arg("--store")
        .arg(dir.path())
        .args(["insert", "app", "notes", "a2", "--data", r#"{"t":2}"#])
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8(output.stderr).expect("stderr is text");
    assert!(
        stderr.contains("warning: the write is committed; git will finish on the next write ("),
        "{stderr}"
    );
    let found = succeed(dir.path(), "get", &["get", "app", "notes", "a2"]);
    assert_eq!(found["doc"]["t"], 2);
    let status = succeed(dir.path(), "status", &["status"]);
    let warning = status["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .find(|warning| warning["code"] == "git_commit_pending")
        .cloned()
        .expect("a pending commit warning");
    assert_eq!(warning["details"]["transactions"], 2);
    set_tree_writable(&objects, true);
    succeed(
        dir.path(),
        "write",
        &["insert", "app", "notes", "a3", "--data", r#"{"t":3}"#],
    );
    let subjects = git(dir.path(), &["log", "--format=%s", "-3"]);
    assert_eq!(
        subjects,
        "insert app/notes/a3\ninsert app/notes/a2\ninsert app/notes/a1"
    );
}

fn git_store_at(store: &Path) {
    succeed(store, "init", &["init", "--git"]);
    succeed(store, "db_ack", &["db", "create", "app"]);
    succeed(
        store,
        "collection_ack",
        &["collection", "create", "app", "notes", "--format", "json"],
    );
    succeed(
        store,
        "write",
        &["insert", "app", "notes", "a1", "--data", r#"{"t":1}"#],
    );
}

fn refused_for_a_missing_repository(store: &Path, args: &[&str]) {
    let run = plaindb(store, None, args);
    assert_eq!(run.code, 1, "{args:?} should fail: {}", run.json);
    assert_eq!(run.json["error"]["code"], "git_disabled", "{}", run.json);
    assert_eq!(run.json["error"]["details"]["reason"], "repository_missing");
    let message = run.json["error"]["message"].as_str().expect("a message");
    assert!(message.contains("plaindb git enable"), "{message}");
}

#[test]
fn a_missing_repository_is_reported_and_only_git_enable_makes_a_new_one() {
    let dir = tempfile::tempdir().expect("a temp dir");
    git_store_at(dir.path());
    fs::remove_dir_all(dir.path().join(".git")).expect("the repository is removed");
    let found = succeed(dir.path(), "get", &["get", "app", "notes", "a1"]);
    assert_eq!(found["doc"]["t"], 1);
    assert!(!dir.path().join(".git").exists());
    let status = succeed(dir.path(), "status", &["status"]);
    assert!(
        status["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning["code"] == "git_repository_missing"),
        "{status}"
    );
    refused_for_a_missing_repository(
        dir.path(),
        &["insert", "app", "notes", "a2", "--data", r#"{"t":2}"#],
    );
    assert!(!dir.path().join(".git").exists());
    succeed(dir.path(), "git", &["git", "enable"]);
    succeed(
        dir.path(),
        "write",
        &["insert", "app", "notes", "a2", "--data", r#"{"t":2}"#],
    );
    assert_eq!(
        git(dir.path(), &["log", "--format=%s"]),
        "insert app/notes/a2\ngit: enable"
    );
}

#[test]
fn a_nested_store_without_its_repository_never_writes_to_the_outer_one() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let outer = dir.path().join("outer");
    fs::create_dir_all(&outer).expect("the outer directory");
    let outer_git = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(&outer)
            .args([
                "-c",
                "user.name=Sample",
                "-c",
                "user.email=sample@example.com",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout).expect("text")
    };
    outer_git(&["init", "-q", "-b", "main"]);
    fs::write(outer.join("readme.txt"), "outer").expect("a file");
    outer_git(&["add", "readme.txt"]);
    outer_git(&["commit", "-q", "-m", "outer"]);
    let store = outer.join("data").join("store");
    git_store_at(&store);
    fs::remove_dir_all(store.join(".git")).expect("the repository is removed");
    let fingerprint = || {
        (
            outer_git(&["rev-list", "--all", "--count"]),
            outer_git(&["remote", "-v"]),
            outer_git(&["ls-files"]),
        )
    };
    let before = fingerprint();
    refused_for_a_missing_repository(&store, &["push"]);
    refused_for_a_missing_repository(
        &store,
        &["insert", "app", "notes", "a2", "--data", r#"{"t":2}"#],
    );
    assert_eq!(fingerprint(), before);
    assert!(!store.join(".git").exists());
}

struct Pushing {
    _dir: TempDir,
    store: PathBuf,
    remote: PathBuf,
    calls: PathBuf,
    slow_git: PathBuf,
}

fn run_with(store: &Path, env: &[(&str, &Path)], args: &[&str]) -> (i32, Value) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_plaindb"));
    command.arg("--store").arg(store).arg("--json").args(args);
    for (name, value) in env {
        command.env(name, value);
    }
    let output = command.output().expect("the binary runs");
    let parsed: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_valid(&validator("response.schema.json"), &parsed);
    (output.status.code().expect("an exit code"), parsed)
}

#[cfg(unix)]
fn slow_git(dir: &Path, calls: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let real = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("sh runs")
            .stdout,
    )
    .expect("text")
    .trim()
    .to_owned();
    let path = dir.join("slow-git");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\ncase \" $* \" in\n*\" push \"*) echo push >> {}; sleep 5;;\nesac\nexec {real} \"$@\"\n",
            calls.display()
        ),
    )
    .expect("the script is written");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("executable");
    path
}

#[cfg(unix)]
fn pushing(mode: &str) -> Pushing {
    let dir = tempfile::tempdir().expect("a temp dir");
    let remote = dir.path().join("remote.git");
    git(
        dir.path(),
        &[
            "init",
            "-q",
            "--bare",
            "--initial-branch=main",
            "remote.git",
        ],
    );
    let store = dir.path().join("store");
    let url = remote.display().to_string();
    succeed(
        &store,
        "init",
        &["init", "--git", "--remote", &url, "--push", "off"],
    );
    succeed(&store, "db_ack", &["db", "create", "app"]);
    succeed(
        &store,
        "collection_ack",
        &["collection", "create", "app", "notes", "--format", "json"],
    );
    succeed(&store, "push", &["push"]);
    succeed(&store, "config", &["config", "set", "push", mode]);
    settle(&store);
    let calls = dir.path().join("calls");
    let slow_git = slow_git(dir.path(), &calls);
    Pushing {
        _dir: dir,
        store,
        remote,
        calls,
        slow_git,
    }
}

fn status_git(store: &Path) -> Value {
    succeed(store, "status", &["status"])["git"].clone()
}

fn settle(store: &Path) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let git = status_git(store);
        if git["push_running"] == false {
            return git;
        }
        assert!(std::time::Instant::now() < deadline, "the push never ended");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn push_count(calls: &Path) -> usize {
    fs::read_to_string(calls)
        .unwrap_or_default()
        .lines()
        .count()
}

fn insert_with(fixture: &Pushing, id: &str) -> Value {
    let (code, parsed) = run_with(
        &fixture.store,
        &[("PLAINDB_GIT", &fixture.slow_git)],
        &["insert", "app", "notes", id, "--data", r#"{"n":1}"#],
    );
    assert_eq!(code, 0, "{parsed}");
    assert_valid(&validator("results/write.schema.json"), &parsed["result"]);
    parsed["result"].clone()
}

fn wait_for_push_state(store: &Path, done: impl Fn(&Value) -> bool) -> Value {
    let path = store.join(".plaindb/push.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Ok(bytes) = fs::read(&path)
            && let Ok(state) = serde_json::from_slice::<Value>(&bytes)
            && done(&state)
        {
            return state;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the push state never changed"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(unix)]
#[test]
fn a_background_push_lets_writes_return_before_it() {
    let fixture = pushing("background");
    let begun = std::time::Instant::now();
    for n in 0..10 {
        let result = insert_with(&fixture, &format!("d{n}"));
        assert_eq!(result["push"], "scheduled", "{result}");
    }
    assert!(
        begun.elapsed() < std::time::Duration::from_secs(3),
        "{:?}",
        begun.elapsed()
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    let git = settle(&fixture.store);
    assert!(
        push_count(&fixture.calls) <= 2,
        "{}",
        push_count(&fixture.calls)
    );
    assert_eq!(git["unpushed"], 0, "{git}");
    assert_eq!(
        crate::git(&fixture.remote, &["rev-parse", "main"]),
        crate::git(&fixture.store, &["rev-parse", "HEAD"])
    );
}

#[cfg(unix)]
#[test]
fn an_on_commit_push_still_holds_the_write() {
    let fixture = pushing("on-commit");
    let begun = std::time::Instant::now();
    let result = insert_with(&fixture, "a");
    assert!(begun.elapsed() >= std::time::Duration::from_secs(5));
    assert_eq!(result["push"], "ok", "{result}");
}

#[cfg(unix)]
#[test]
fn a_background_push_to_a_missing_remote_backs_off_and_catches_up() {
    let fixture = pushing("background");
    let moved = fixture.remote.with_extension("moved");
    fs::rename(&fixture.remote, &moved).expect("the remote moves");
    let begun = std::time::Instant::now();
    for n in 0..5 {
        assert_eq!(insert_with(&fixture, &format!("d{n}"))["push"], "scheduled");
    }
    assert!(begun.elapsed() < std::time::Duration::from_secs(3));
    let state = wait_for_push_state(&fixture.store, |state| state["failures"] != 0);
    assert!(state["next_attempt_at"].is_string(), "{state}");
    settle(&fixture.store);
    fs::rename(&moved, &fixture.remote).expect("the remote is back");
    succeed(&fixture.store, "push", &["push"]);
    assert_eq!(status_git(&fixture.store)["unpushed"], 0);
    assert_eq!(
        crate::git(&fixture.remote, &["rev-parse", "main"]),
        crate::git(&fixture.store, &["rev-parse", "HEAD"])
    );
}

#[cfg(unix)]
#[test]
fn a_background_push_from_another_branch_sends_nothing() {
    let fixture = pushing("background");
    let delivered = crate::git(&fixture.remote, &["rev-parse", "main"]);
    crate::git(&fixture.store, &["checkout", "-q", "-b", "other"]);
    assert_eq!(insert_with(&fixture, "a")["push"], "scheduled");
    wait_for_push_state(&fixture.store, |state| {
        state["last_push_error"] == "HEAD is on other, the store pushes main"
    });
    settle(&fixture.store);
    assert_eq!(push_count(&fixture.calls), 0);
    assert_eq!(
        crate::git(&fixture.remote, &["rev-parse", "main"]),
        delivered
    );
}

#[test]
fn an_unknown_push_mode_is_refused() {
    let dir = store_with_collection("json");
    assert_eq!(
        fail(dir.path(), &["config", "set", "push", "sometimes"]),
        "invalid_config"
    );
    succeed(
        dir.path(),
        "config",
        &["config", "set", "push", "background"],
    );
}

#[test]
fn a_new_name_may_not_start_with_a_dash() {
    let dir = store_with_collection("json");
    for args in [
        vec!["db", "create", "--", "-x"],
        vec![
            "collection",
            "create",
            "app",
            "--format",
            "json",
            "--",
            "-c",
        ],
    ] {
        let run = plaindb(dir.path(), None, &args);
        assert_eq!(run.code, 1, "{args:?}: {}", run.json);
        assert_eq!(run.json["error"]["code"], "invalid_name");
        assert_eq!(run.json["error"]["details"]["reason"], "leading_dash");
    }
    succeed(dir.path(), "db_ack", &["db", "create", "old"]);
    succeed(
        dir.path(),
        "collection_ack",
        &["collection", "create", "old", "kept", "--format", "json"],
    );
    fs::rename(dir.path().join("old"), dir.path().join("-old")).expect("renamed");
    assert_eq!(
        succeed(dir.path(), "dbs", &["db", "list"])["dbs"],
        json!(["-old", "app"])
    );
    succeed(dir.path(), "find", &["find", "--", "-old", "kept"]);
    succeed(
        dir.path(),
        "collection_ack",
        &[
            "collection",
            "create",
            "--format",
            "json",
            "--",
            "-old",
            "notes",
        ],
    );
}

#[test]
fn checkpoint_interval_is_a_setting() {
    let dir = store_with_collection("json");
    let set = succeed(
        dir.path(),
        "config",
        &["config", "set", "checkpoint_interval", "8"],
    );
    assert_eq!(set["value"], 8);
    assert_eq!(
        succeed(
            dir.path(),
            "config",
            &["config", "get", "checkpoint_interval"]
        )["value"],
        8
    );
    assert_eq!(
        fail(dir.path(), &["config", "set", "checkpoint_interval", "0"]),
        "invalid_config"
    );
}

#[test]
fn init_refuses_git_options_without_git_before_creating_anything() {
    for (args, message) in [
        (
            vec!["init", "--remote", "/tmp/x.git"],
            "--remote needs --git",
        ),
        (vec!["init", "--push", "on-commit"], "--push needs --git"),
        (vec!["init", "--commit", "manual"], "--commit needs --git"),
    ] {
        let dir = tempfile::tempdir().expect("a temp dir");
        let run = plaindb(dir.path(), None, &args);
        assert_eq!(run.code, 1, "{args:?}: {}", run.json);
        assert_eq!(run.json["error"]["code"], "invalid_config");
        assert_eq!(run.json["error"]["message"], message);
        assert_eq!(fs::read_dir(dir.path()).expect("a directory").count(), 0);
    }
    let dir = tempfile::tempdir().expect("a temp dir");
    succeed(
        dir.path(),
        "init",
        &["init", "--git", "--remote", "/tmp/x.git"],
    );
}

fn exit_code(store: &Path, args: &[&str]) -> Option<i32> {
    let output = Command::new(env!("CARGO_BIN_EXE_plaindb"))
        .arg("--store")
        .arg(store)
        .arg("--json")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("the binary runs");
    if output.status.code() == Some(2) {
        let stdout = String::from_utf8(output.stdout).expect("stdout is text");
        assert_eq!(stdout.lines().count(), 1, "one line expected: {stdout:?}");
        let parsed: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
        assert_valid(&validator("response.schema.json"), &parsed);
        assert_eq!(parsed["error"]["code"], "usage", "{parsed}");
    }
    output.status.code()
}

fn document_n(store: &Path) -> Value {
    succeed(store, "get", &["get", "app", "notes", "n1"])["doc"]["a"].clone()
}

#[test]
fn patch_takes_data_or_a_file_and_exactly_one() {
    let dir = store_with_collection("json");
    succeed(
        dir.path(),
        "write",
        &["put", "app", "notes", "n1", "--data", r#"{"b":1}"#],
    );
    let source = tempfile::tempdir().expect("a temp dir");
    let file = source.path().join("p.json");
    fs::write(&file, r#"{"a":1}"#).expect("a file");
    succeed(
        dir.path(),
        "write",
        &[
            "patch",
            "app",
            "notes",
            "n1",
            "--file",
            file.to_str().expect("a path"),
        ],
    );
    assert_eq!(document_n(dir.path()), 1);
    let from_stdin = exec(
        dir.path(),
        Some("write"),
        &["patch", "app", "notes", "n1", "--file", "-"],
        Some(r#"{"a":2}"#),
    );
    assert_eq!(from_stdin.code, 0, "{}", from_stdin.json);
    assert_eq!(document_n(dir.path()), 2);
    succeed(
        dir.path(),
        "write",
        &["patch", "app", "notes", "n1", "--data", r#"{"a":3}"#],
    );
    assert_eq!(document_n(dir.path()), 3);
    assert_eq!(
        exit_code(dir.path(), &["patch", "app", "notes", "n1"]),
        Some(2)
    );
    assert_eq!(
        exit_code(
            dir.path(),
            &["patch", "app", "notes", "n1", "--data", "{}", "--file", "-"]
        ),
        Some(2)
    );
}

#[test]
fn a_schema_is_set_from_data_a_file_or_standard_input() {
    let dir = store_with_collection("json");
    let set = succeed(
        dir.path(),
        "collection_ack",
        &[
            "collection",
            "schema",
            "set",
            "app",
            "notes",
            "--data",
            r#"{"type":"object"}"#,
        ],
    );
    assert_eq!(set["has_schema"], true);
    let from_stdin = exec(
        dir.path(),
        Some("collection_ack"),
        &["collection", "schema", "set", "app", "notes", "--file", "-"],
        Some(r#"{"type":"object"}"#),
    );
    assert_eq!(from_stdin.code, 0, "{}", from_stdin.json);
    for args in [
        vec![
            "collection",
            "schema",
            "set",
            "app",
            "notes",
            "--data",
            "{}",
            "--clear",
        ],
        vec![
            "collection",
            "schema",
            "set",
            "app",
            "notes",
            "--data",
            "{}",
            "--file",
            "-",
        ],
        vec!["collection", "schema", "set", "app", "notes"],
    ] {
        assert_eq!(exit_code(dir.path(), &args), Some(2), "{args:?}");
    }
}

#[test]
fn a_transaction_takes_inline_data() {
    let dir = store_with_collection("json");
    let ops = json!([
        { "op": "insert", "db": "app", "collection": "notes", "id": "a", "doc": { "n": 1 } },
        { "op": "insert", "db": "app", "collection": "notes", "id": "b", "doc": { "n": 2 } },
    ]);
    succeed(dir.path(), "write", &["tx", "--data", &ops.to_string()]);
    assert_eq!(count(dir.path()), 2);
    assert_eq!(exit_code(dir.path(), &["tx"]), Some(2));
}

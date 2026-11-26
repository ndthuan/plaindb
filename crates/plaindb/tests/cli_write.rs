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
    assert_eq!(all["value"]["commit"], "each");
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
        "invalid_document"
    );
    assert_eq!(
        fail(dir.path(), &["config", "set", "commit", "sometimes"]),
        "invalid_document"
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
            &["init", "--remote", "https://example.com/x.git"]
        ),
        "already_exists"
    );
}

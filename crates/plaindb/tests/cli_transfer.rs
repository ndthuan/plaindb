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

fn ids(store: &Path, collection: &str) -> Vec<String> {
    let found = succeed(
        store,
        "find",
        &[
            "find", "app", collection, "--sort", "_id", "--fields", "_id",
        ],
    );
    found["docs"]
        .as_array()
        .expect("docs")
        .iter()
        .map(|entry| entry["doc"]["_id"].as_str().expect("an id").to_owned())
        .collect()
}

fn raw_export(store: &Path, collection: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_plaindb"))
        .arg("--store")
        .arg(store)
        .arg("--json")
        .args(["export", "app", collection, "--jsonl"])
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(0));
    String::from_utf8(output.stdout).expect("text")
}

#[test]
fn a_directory_import_reports_what_it_took_left_out_and_ignored() {
    let dir = store_with_collection("json");
    let source = tempfile::tempdir().expect("a temp dir");
    fs::write(
        source.path().join("a.md"),
        "---\ntitle: A\n---\nbody of a\n",
    )
    .expect("a file");
    fs::write(source.path().join("b.yaml"), "title: B\n").expect("a file");
    fs::write(source.path().join("bad name.json"), "{}").expect("a file");
    fs::write(source.path().join("c.txt"), "ignored").expect("a file");
    let path = source.path().to_str().expect("a path");
    let report = succeed(
        dir.path(),
        "import",
        &["import", "app", "notes", "--dir", path],
    );
    assert_eq!(report["imported"], 2);
    assert_eq!(
        report["omitted"],
        json!([{ "file": "bad name.json", "reason": "invalid_name" }])
    );
    assert_eq!(report["failed"], json!([]));
    assert_eq!(ids(dir.path(), "notes"), ["a", "b"]);
    let again = import_failed(dir.path(), &["import", "app", "notes", "--dir", path], None);
    assert_eq!(again["imported"], 0);
    assert_eq!(again["failed"].as_array().map(Vec::len), Some(2));
    assert_eq!(again["failed"][0]["code"], "already_exists");
    assert_eq!(again["failed"][0]["file"], "a.md");
    let replaced = succeed(
        dir.path(),
        "import",
        &["import", "app", "notes", "--dir", path, "--replace"],
    );
    assert_eq!(replaced["imported"], 2);
}

#[test]
fn a_jsonl_import_reads_standard_input_and_names_the_bad_lines() {
    let dir = store_with_collection("json");
    let lines = "{\"_id\":\"a\",\"n\":1}\n\nnot json\n{\"n\":2}\n";
    let run = exec(
        dir.path(),
        Some("import"),
        &["import", "app", "notes", "--jsonl", "-"],
        Some(lines),
    );
    assert_eq!(run.code, 0);
    let report = &run.json["result"];
    assert_eq!(report["imported"], 2);
    assert_eq!(report["failed"][0]["line"], 3);
    assert_eq!(report["failed"][0]["code"], "invalid_document");
    assert_eq!(ids(dir.path(), "notes").len(), 2);
    let source = tempfile::tempdir().expect("a temp dir");
    let file = source.path().join("more.jsonl");
    fs::write(&file, "{\"_id\":\"z\"}\n").expect("a file");
    let from_file = succeed(
        dir.path(),
        "import",
        &[
            "import",
            "app",
            "notes",
            "--jsonl",
            file.to_str().expect("a path"),
        ],
    );
    assert_eq!(from_file["imported"], 1);
}

#[test]
fn an_import_needs_a_source() {
    let dir = store_with_collection("json");
    let run = exec(dir.path(), None, &["import", "app", "notes"], None);
    assert_eq!(run.code, 2);
    assert_eq!(run.json["error"]["code"], "usage");
}

#[test]
fn export_writes_documents_with_versions_to_standard_output_or_a_file() {
    let dir = store_with_collection("json");
    succeed(
        dir.path(),
        "write",
        &["put", "app", "notes", "b", "--data", r#"{"n":2}"#],
    );
    succeed(
        dir.path(),
        "write",
        &["put", "app", "notes", "a", "--data", r#"{"n":1}"#],
    );
    let text = raw_export(dir.path(), "notes");
    let docs: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON"))
        .collect();
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[0]["_id"], "a");
    let version = succeed(dir.path(), "get", &["get", "app", "notes", "a"])["version"].clone();
    assert_eq!(docs[0]["_version"], version);
    let target = tempfile::tempdir().expect("a temp dir");
    let out = target.path().join("notes.jsonl");
    let result = succeed(
        dir.path(),
        "export",
        &[
            "export",
            "app",
            "notes",
            "--jsonl",
            "--out",
            out.to_str().expect("a path"),
        ],
    );
    assert_eq!(result["exported"], 2);
    assert_eq!(fs::read_to_string(&out).expect("the file"), text);
    assert_eq!(
        fail(dir.path(), &["export", "app", "missing", "--jsonl"]),
        "not_found"
    );
    assert_eq!(
        fail(dir.path(), &["export", "app", "notes"]),
        "invalid_query"
    );
}

fn directory_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("the directory reads")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn export_to_path(store: &Path, collection: &str, out: &Path) -> Run {
    plaindb(
        store,
        None,
        &[
            "export",
            "app",
            collection,
            "--jsonl",
            "--out",
            out.to_str().expect("a path"),
        ],
    )
}

#[test]
fn a_failed_export_leaves_the_target_file_as_it_was() {
    let dir = store_with_collection("json");
    succeed(
        dir.path(),
        "write",
        &["put", "app", "notes", "a", "--data", r#"{"n":1}"#],
    );
    let target = tempfile::tempdir().expect("a temp dir");
    let out = target.path().join("backup.jsonl");
    assert_eq!(export_to_path(dir.path(), "notes", &out).code, 0);
    let before = fs::read(&out).expect("the file");
    let run = export_to_path(dir.path(), "missing", &out);
    assert_eq!(run.code, 1);
    assert_eq!(run.json["error"]["code"], "not_found");
    assert_eq!(fs::read(&out).expect("the file"), before);
    assert_eq!(directory_entries(target.path()), ["backup.jsonl"]);
}

#[test]
fn an_export_into_a_missing_directory_creates_nothing() {
    let dir = store_with_collection("json");
    let target = tempfile::tempdir().expect("a temp dir");
    let out = target.path().join("absent").join("notes.jsonl");
    let run = export_to_path(dir.path(), "notes", &out);
    assert_eq!(run.code, 1);
    assert!(directory_entries(target.path()).is_empty());
}

#[test]
fn an_export_to_a_dash_goes_to_standard_output() {
    let dir = store_with_collection("json");
    succeed(
        dir.path(),
        "write",
        &["put", "app", "notes", "a", "--data", r#"{"n":1}"#],
    );
    let work = tempfile::tempdir().expect("a temp dir");
    let output = Command::new(env!("CARGO_BIN_EXE_plaindb"))
        .current_dir(work.path())
        .arg("--store")
        .arg(dir.path())
        .args(["export", "app", "notes", "--jsonl", "--out", "-"])
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).expect("text");
    assert_eq!(text.lines().count(), 1);
    assert_eq!(
        serde_json::from_str::<Value>(&text).expect("JSON")["_id"],
        "a"
    );
    assert!(directory_entries(work.path()).is_empty());
}

#[test]
fn an_export_over_an_existing_file_replaces_it_and_leaves_no_temporary_file() {
    let dir = store_with_collection("json");
    succeed(
        dir.path(),
        "write",
        &["put", "app", "notes", "a", "--data", r#"{"n":1}"#],
    );
    let target = tempfile::tempdir().expect("a temp dir");
    let out = target.path().join("backup.jsonl");
    fs::write(&out, "old\n").expect("a file");
    assert_eq!(export_to_path(dir.path(), "notes", &out).code, 0);
    assert_eq!(
        fs::read_to_string(&out).expect("the file"),
        raw_export(dir.path(), "notes")
    );
    assert_eq!(directory_entries(target.path()), ["backup.jsonl"]);
}

#[test]
fn an_export_goes_back_in_through_the_command_line() {
    let dir = store_with_collection("md");
    succeed(
        dir.path(),
        "collection_ack",
        &["collection", "create", "app", "copy", "--format", "md"],
    );
    let body = "\n  indented  \nlast line without newline";
    let doc = json!({ "title": "T", "body": body }).to_string();
    succeed(
        dir.path(),
        "write",
        &["put", "app", "notes", "page", "--data", &doc],
    );
    let text = raw_export(dir.path(), "notes");
    let report = exec(
        dir.path(),
        Some("import"),
        &["import", "app", "copy", "--jsonl", "-"],
        Some(&text),
    );
    assert_eq!(report.json["result"]["imported"], 1);
    let copied = succeed(dir.path(), "get", &["get", "app", "copy", "page"]);
    assert_eq!(copied["doc"]["body"], body);
    let original = succeed(dir.path(), "get", &["get", "app", "notes", "page"]);
    assert_eq!(copied["doc"], original["doc"]);
}

fn import_failed(store: &Path, args: &[&str], stdin: Option<&str>) -> Value {
    let run = exec(store, None, args, stdin);
    assert_eq!(run.code, 1, "{args:?} should fail: {}", run.json);
    assert_eq!(run.json["error"]["code"], "import_failed", "{}", run.json);
    let report = run.json["error"]["details"]["report"].clone();
    assert_valid(&validator("results/import.schema.json"), &report);
    report
}

fn import_stdin(store: &Path, lines: &str) -> Run {
    exec(
        store,
        Some("import"),
        &["import", "app", "notes", "--jsonl", "-"],
        Some(lines),
    )
}

#[test]
fn an_import_where_every_line_fails_exits_one_with_the_report() {
    let dir = store_with_collection("json");
    let run = exec(
        dir.path(),
        None,
        &["import", "app", "notes", "--jsonl", "-"],
        Some("not json\n[1]\n"),
    );
    assert_eq!(run.code, 1);
    assert_eq!(
        run.json["error"]["message"],
        "no document was imported, 2 failed"
    );
    let report = import_failed(
        dir.path(),
        &["import", "app", "notes", "--jsonl", "-"],
        Some("not json\n[1]\n"),
    );
    assert_eq!(report["imported"], 0);
    assert_eq!(report["failed"][0]["line"], 1);
    assert_eq!(report["failed"][1]["details"]["reason"], "not_an_object");
    assert!(report.get("aborted").is_none());
}

#[test]
fn an_empty_input_and_a_directory_of_other_files_import_nothing_and_succeed() {
    let dir = store_with_collection("json");
    let empty = import_stdin(dir.path(), "");
    assert_eq!(empty.code, 0);
    assert_eq!(empty.json["result"]["imported"], 0);
    let source = tempfile::tempdir().expect("a temp dir");
    fs::write(source.path().join("notes.txt"), "text").expect("a file");
    fs::write(source.path().join("data.csv"), "a,b").expect("a file");
    let report = succeed(
        dir.path(),
        "import",
        &[
            "import",
            "app",
            "notes",
            "--dir",
            source.path().to_str().expect("a path"),
        ],
    );
    assert_eq!(report["imported"], 0);
    assert_eq!(report["failed"], json!([]));
}

#[test]
fn a_repeated_id_and_its_details_are_in_the_report() {
    let dir = store_with_collection("json");
    let run = import_stdin(
        dir.path(),
        "{\"_id\":\"dup1\",\"n\":1}\n{\"_id\":\"dup1\",\"n\":2}\n",
    );
    assert_eq!(run.code, 0);
    let report = &run.json["result"];
    assert_eq!(report["imported"], 1);
    assert_eq!(report["failed"][0]["line"], 2);
    assert_eq!(report["failed"][0]["code"], "duplicate_in_input");
    assert_eq!(report["failed"][0]["details"]["id"], "dup1");
}

fn lock_during_second_batch(store: &Path, json: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_plaindb"));
    command.arg("--store").arg(store);
    if json {
        command.arg("--json");
    }
    let mut child = command
        .args(["import", "app", "notes", "--jsonl", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    let mut input = child.stdin.take().expect("stdin is piped");
    let batch = |from: u32| -> String {
        (from..from + 500)
            .map(|n| format!("{}\n", json!({ "_id": format!("d{n:04}") })))
            .collect()
    };
    let generation = store.join(".plaindb").join("generation");
    let before = fs::read_to_string(&generation).unwrap_or_default();
    input
        .write_all(batch(0).as_bytes())
        .expect("stdin is written");
    input.flush().expect("stdin is flushed");
    let started = std::time::Instant::now();
    while fs::read_to_string(&generation).unwrap_or_default() == before {
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        std::thread::yield_now();
    }
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(store.join(".plaindb").join("lock"))
        .expect("the lock file opens");
    lock.lock().expect("the lock is taken");
    input
        .write_all(batch(500).as_bytes())
        .expect("stdin is written");
    drop(input);
    let output = child.wait_with_output().expect("the binary finishes");
    lock.unlock().expect("the lock is released");
    output
}

#[test]
fn an_import_stopped_by_the_lock_prints_its_report_and_exits_one() {
    let dir = store_with_collection("json");
    succeed(
        dir.path(),
        "config",
        &["config", "set", "lock_timeout_ms", "100"],
    );
    let output = lock_during_second_batch(dir.path(), true);
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).expect("text");
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    let parsed: Value = serde_json::from_str(&stdout).expect("JSON");
    assert_valid(&validator("response.schema.json"), &parsed);
    assert_eq!(parsed["error"]["code"], "import_failed");
    let message = parsed["error"]["message"].as_str().expect("a message");
    assert!(message.starts_with("the import stopped: "), "{message}");
    let report = &parsed["error"]["details"]["report"];
    assert_valid(&validator("results/import.schema.json"), report);
    assert_eq!(report["imported"], 500);
    assert_eq!(report["aborted"]["code"], "store_locked_timeout");
    assert_eq!(
        succeed(dir.path(), "count", &["count", "app", "notes"])["count"],
        500
    );

    let dir = store_with_collection("json");
    succeed(
        dir.path(),
        "config",
        &["config", "set", "lock_timeout_ms", "100"],
    );
    let output = lock_during_second_batch(dir.path(), false);
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).expect("text");
    assert!(stdout.starts_with("imported 500, "), "{stdout}");
    let stderr = String::from_utf8(output.stderr).expect("text");
    assert!(
        stderr.contains("error[import_failed]: the import stopped: "),
        "{stderr}"
    );
}

fn attach_to_note(store: &Path, id: &str) {
    let source = store.join("attachment.txt");
    fs::write(&source, b"attached").expect("the source is written");
    succeed(
        store,
        "file_put",
        &[
            "file",
            "put",
            "app",
            "notes",
            id,
            "--name",
            "a.txt",
            "--from",
            source.to_str().expect("a path"),
        ],
    );
}

#[test]
fn an_import_drops_files_and_names_the_documents_that_had_them() {
    let dir = store_with_collection("json");
    succeed(
        dir.path(),
        "collection_ack",
        &["collection", "create", "app", "copy", "--format", "json"],
    );
    for id in ["a", "b", "c"] {
        succeed(
            dir.path(),
            "write",
            &["put", "app", "notes", id, "--data", r#"{"n":1}"#],
        );
    }
    attach_to_note(dir.path(), "b");
    attach_to_note(dir.path(), "a");
    let text = raw_export(dir.path(), "notes");
    assert!(text.contains("_files"));
    let report = exec(
        dir.path(),
        Some("import"),
        &["import", "app", "copy", "--jsonl", "-"],
        Some(&text),
    );
    assert_eq!(report.code, 0, "{}", report.json);
    assert_eq!(report.json["result"]["imported"], 3);
    assert_eq!(report.json["result"]["files_dropped"], json!(["a", "b"]));
    let copied = succeed(dir.path(), "get", &["get", "app", "copy", "b"]);
    assert!(copied["doc"].get("_files").is_none());
    assert!(!dir.path().join("app/copy/_files").exists());
}

#[test]
fn an_import_of_documents_without_files_drops_none() {
    let dir = store_with_collection("json");
    let run = import_stdin(dir.path(), "{\"_id\":\"a\",\"n\":1}\n");
    assert_eq!(run.code, 0, "{}", run.json);
    assert_eq!(run.json["result"]["files_dropped"], json!([]));
}

#[test]
fn an_import_directory_drops_files_too() {
    let dir = store_with_collection("json");
    let source = tempfile::tempdir().expect("a temp dir");
    fs::write(
        source.path().join("a.json"),
        r#"{"n":1,"_files":{"x.txt":{"size":1,"sha256":"00","mime":"text/plain"}}}"#,
    )
    .expect("the file is written");
    let run = plaindb(
        dir.path(),
        Some("import"),
        &[
            "import",
            "app",
            "notes",
            "--dir",
            source.path().to_str().expect("a path"),
        ],
    );
    assert_eq!(run.code, 0, "{}", run.json);
    assert_eq!(run.json["result"]["files_dropped"], json!(["a"]));
}

#[test]
fn an_insert_with_files_is_still_refused() {
    let dir = store_with_collection("json");
    let run = plaindb(
        dir.path(),
        None,
        &["insert", "app", "notes", "a", "--data", r#"{"_files":{}}"#],
    );
    assert_eq!(run.code, 1);
    assert_eq!(run.json["error"]["code"], "invalid_document");
}

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};
use tempfile::TempDir;

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

fn raw(store: &Path, args: &[&str], stdin: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_plaindb"))
        .arg("--store")
        .arg(store)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    let mut input = child.stdin.take().expect("stdin is piped");
    if let Some(bytes) = stdin {
        input.write_all(bytes).expect("stdin is written");
    }
    drop(input);
    child.wait_with_output().expect("the binary finishes")
}

struct Run {
    code: i32,
    json: Value,
}

fn json_run(store: &Path, kind: Option<&str>, args: &[&str], stdin: Option<&[u8]>) -> Run {
    let mut with_json = vec!["--json"];
    with_json.extend_from_slice(args);
    let output = raw(store, &with_json, stdin);
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

fn succeed(store: &Path, kind: &str, args: &[&str]) -> Value {
    let run = json_run(store, Some(kind), args, None);
    assert_eq!(run.code, 0, "{args:?} failed: {}", run.json);
    run.json["result"].clone()
}

fn store_with_note() -> TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    succeed(dir.path(), "init", &["init"]);
    succeed(dir.path(), "db_ack", &["db", "create", "app"]);
    succeed(
        dir.path(),
        "collection_ack",
        &["collection", "create", "app", "notes", "--format", "json"],
    );
    succeed(
        dir.path(),
        "write",
        &["insert", "app", "notes", "n1", "--data", "{}"],
    );
    dir
}

fn binary(len: usize) -> Vec<u8> {
    (0..len)
        .map(|n| u8::try_from((n * 31 + 7) % 256).expect("a byte"))
        .collect()
}

#[test]
fn a_file_put_from_a_path_comes_back_byte_for_byte() {
    let dir = store_with_note();
    let source = dir.path().join("photo.png");
    let bytes = binary(3 * 1024);
    fs::write(&source, &bytes).expect("the source is written");
    let put = succeed(
        dir.path(),
        "file_put",
        &[
            "file",
            "put",
            "app",
            "notes",
            "n1",
            "--name",
            "photo.png",
            "--from",
            source.to_str().expect("a path"),
        ],
    );
    assert_eq!(put["file"]["size"], json!(3 * 1024));
    assert_eq!(put["file"]["mime"], json!("image/png"));
    let output = raw(
        dir.path(),
        &["file", "get", "app", "notes", "n1", "--name", "photo.png"],
        None,
    );
    assert!(output.status.success());
    assert_eq!(output.stdout, bytes);
    let out = dir.path().join("copy.png");
    let got = succeed(
        dir.path(),
        "file_get",
        &[
            "file",
            "get",
            "app",
            "notes",
            "n1",
            "--name",
            "photo.png",
            "--out",
            out.to_str().expect("a path"),
        ],
    );
    assert_eq!(got["sha256"], put["file"]["sha256"]);
    assert_eq!(fs::read(&out).expect("the copy exists"), bytes);
}

#[test]
fn a_file_put_from_standard_input_comes_back_byte_for_byte() {
    let dir = store_with_note();
    let bytes = binary(3 * 1024);
    let run = json_run(
        dir.path(),
        Some("file_put"),
        &[
            "file",
            "put",
            "app",
            "notes",
            "n1",
            "--name",
            "data.bin",
            "--from",
            "-",
            "--mime",
            "application/x-test",
        ],
        Some(&bytes),
    );
    assert_eq!(run.code, 0, "{}", run.json);
    assert_eq!(
        run.json["result"]["file"]["mime"],
        json!("application/x-test")
    );
    let output = raw(
        dir.path(),
        &["file", "get", "app", "notes", "n1", "--name", "data.bin"],
        None,
    );
    assert_eq!(output.stdout, bytes);
}

#[test]
fn file_get_with_json_and_no_out_is_a_usage_error() {
    let dir = store_with_note();
    let run = json_run(
        dir.path(),
        None,
        &["file", "get", "app", "notes", "n1", "--name", "a.png"],
        None,
    );
    assert_eq!(run.code, 2);
    assert_eq!(run.json["error"]["code"], json!("usage"));
}

#[test]
fn file_put_on_a_missing_document_is_not_found() {
    let dir = store_with_note();
    let run = json_run(
        dir.path(),
        None,
        &[
            "file", "put", "app", "notes", "nobody", "--name", "a.bin", "--from", "-",
        ],
        Some(b"bytes"),
    );
    assert_eq!(run.code, 1);
    assert_eq!(run.json["error"]["code"], json!("not_found"));
}

#[test]
fn file_list_names_every_file_in_order_and_file_rm_removes_one() {
    let dir = store_with_note();
    for name in ["b.pdf", "a.png", "c.txt"] {
        let run = json_run(
            dir.path(),
            Some("file_put"),
            &[
                "file", "put", "app", "notes", "n1", "--name", name, "--from", "-",
            ],
            Some(name.as_bytes()),
        );
        assert_eq!(run.code, 0, "{}", run.json);
    }
    let listed = succeed(
        dir.path(),
        "file_list",
        &["file", "list", "app", "notes", "n1"],
    );
    let names: Vec<&String> = listed["files"]
        .as_object()
        .expect("an object")
        .keys()
        .collect();
    assert_eq!(names, ["a.png", "b.pdf", "c.txt"]);
    assert_eq!(listed["files"]["c.txt"]["mime"], json!("text/plain"));
    let removed = succeed(
        dir.path(),
        "file_rm",
        &["file", "rm", "app", "notes", "n1", "--name", "b.pdf"],
    );
    assert_eq!(removed["results"][0]["id"], json!("n1"));
    let run = json_run(
        dir.path(),
        None,
        &[
            "file",
            "get",
            "app",
            "notes",
            "n1",
            "--name",
            "b.pdf",
            "--out",
            dir.path().join("b.pdf").to_str().expect("a path"),
        ],
        None,
    );
    assert_eq!(run.code, 1);
    assert_eq!(run.json["error"]["code"], json!("not_found"));
    let human = raw(dir.path(), &["file", "list", "app", "notes", "n1"], None);
    let text = String::from_utf8(human.stdout).expect("text");
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(text.starts_with("a.png 5 image/png"), "{text}");
}

#[test]
fn file_put_and_rm_honour_if_version() {
    let dir = store_with_note();
    let stale = succeed(dir.path(), "get", &["get", "app", "notes", "n1"])["version"]
        .as_str()
        .expect("a version")
        .to_owned();
    succeed(
        dir.path(),
        "write",
        &["patch", "app", "notes", "n1", "--data", r#"{"x":1}"#],
    );
    for args in [
        vec![
            "file",
            "put",
            "app",
            "notes",
            "n1",
            "--name",
            "a.bin",
            "--from",
            "-",
            "--if-version",
            &stale,
        ],
        vec![
            "file",
            "rm",
            "app",
            "notes",
            "n1",
            "--name",
            "a.bin",
            "--if-version",
            &stale,
        ],
    ] {
        let run = json_run(dir.path(), None, &args, Some(b"x"));
        assert_eq!(run.code, 1);
        assert_eq!(run.json["error"]["code"], json!("version_conflict"));
    }
}

#[test]
fn file_put_records_the_tx_id_and_actor_in_the_commit() {
    let dir = tempfile::tempdir().expect("a temp dir");
    succeed(dir.path(), "init", &["init", "--git"]);
    succeed(dir.path(), "db_ack", &["db", "create", "app"]);
    succeed(
        dir.path(),
        "collection_ack",
        &["collection", "create", "app", "notes", "--format", "json"],
    );
    succeed(
        dir.path(),
        "write",
        &["insert", "app", "notes", "n1", "--data", "{}"],
    );
    let tx_id = "01JD0000000000000000000000";
    let run = json_run(
        dir.path(),
        Some("file_put"),
        &[
            "--tx-id",
            tx_id,
            "--actor",
            "Sample Writer",
            "file",
            "put",
            "app",
            "notes",
            "n1",
            "--name",
            "a.bin",
            "--from",
            "-",
        ],
        Some(b"bytes"),
    );
    assert_eq!(run.code, 0, "{}", run.json);
    assert_eq!(run.json["result"]["tx_id"], json!(tx_id));
    let log = Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(dir.path())
        .output()
        .expect("git runs");
    let message = String::from_utf8(log.stdout).expect("text");
    assert!(
        message.starts_with("attach app/notes/n1 a.bin"),
        "{message}"
    );
    assert!(message.contains(tx_id), "{message}");
    assert!(message.contains("Sample Writer"), "{message}");
}

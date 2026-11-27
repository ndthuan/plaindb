use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use plaindb_core::{Format, IndexSpec, Op, Store, Tx};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
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

fn plaindb(store: &Path, kind: Option<&str>, args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_plaindb"))
        .arg("--store")
        .arg(store)
        .arg("--json")
        .args(args)
        .output()
        .expect("the binary runs");
    let stdout = String::from_utf8(output.stdout).expect("stdout is text");
    let stderr = String::from_utf8(output.stderr).expect("stderr is text");
    let mut parsed = Value::Null;
    if output.status.code() != Some(2) {
        assert_eq!(stdout.lines().count(), 1, "one line expected: {stdout:?}");
        parsed = serde_json::from_str(&stdout).expect("stdout is JSON");
        assert_valid(&validator("response.schema.json"), &parsed);
        if let (Some(kind), Some(result)) = (kind, parsed.get("result")) {
            assert_valid(&validator(&format!("results/{kind}.schema.json")), result);
        }
    }
    Run {
        code: output.status.code().expect("the binary exits"),
        stdout,
        stderr,
        json: parsed,
    }
}

fn human(store: &Path, args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_plaindb"))
        .arg("--store")
        .arg(store)
        .args(args)
        .output()
        .expect("the binary runs");
    Run {
        code: output.status.code().expect("the binary exits"),
        stdout: String::from_utf8(output.stdout).expect("stdout is text"),
        stderr: String::from_utf8(output.stderr).expect("stderr is text"),
        json: Value::Null,
    }
}

fn doc(value: Value) -> plaindb_core::Doc {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn fixture(git: bool) -> TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = if git {
        Store::init_with_git(dir.path())
    } else {
        Store::init(dir.path())
    }
    .expect("the store initialises");
    store.create_db("app").expect("a db");
    store
        .create_collection("app", "notes", Format::Json)
        .expect("a collection");
    store
        .set_indexes(
            "app",
            "notes",
            vec![IndexSpec::Field {
                field: "n".to_owned(),
                unique: false,
                multi: false,
            }],
        )
        .expect("an index");
    store
        .set_schema(
            "app",
            "notes",
            Some(json!({ "type": "object", "properties": { "n": { "type": "integer" } } })),
        )
        .expect("a schema");
    let ops = (0..5)
        .map(|n| Op::Put {
            db: "app".to_owned(),
            coll: "notes".to_owned(),
            id: format!("n{n}"),
            doc: doc(json!({ "n": n, "tag": if n % 2 == 0 { "even" } else { "odd" } })),
            if_version: None,
        })
        .collect();
    store.apply(Tx::new(ops)).expect("documents are written");
    dir
}

#[test]
fn get_returns_the_document_and_its_version() {
    let dir = fixture(false);
    let run = plaindb(dir.path(), Some("get"), &["get", "app", "notes", "n3"]);
    assert_eq!(run.code, 0);
    assert_eq!(run.json["ok"], true);
    assert_eq!(run.json["result"]["doc"]["n"], 3);
    assert_eq!(run.json["result"]["doc"]["_id"], "n3");
    assert_eq!(
        run.json["result"]["version"].as_str().map(str::len),
        Some(40)
    );
}

#[test]
fn get_of_a_missing_document_is_not_found() {
    let dir = fixture(false);
    let run = plaindb(dir.path(), None, &["get", "app", "notes", "missing"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.json["ok"], false);
    assert_eq!(run.json["error"]["code"], "not_found");
}

#[test]
fn a_bad_filter_is_an_invalid_query() {
    let dir = fixture(false);
    let run = plaindb(
        dir.path(),
        None,
        &["find", "app", "notes", "--filter", "{not json"],
    );
    assert_eq!(run.code, 1);
    assert_eq!(run.json["error"]["code"], "invalid_query");
}

#[test]
fn a_directory_without_a_store_is_not_a_store() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let run = plaindb(dir.path(), None, &["status"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.json["error"]["code"], "not_a_store");
}

#[test]
fn find_pages_through_next_cursor() {
    let dir = fixture(false);
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..5 {
        let mut args = vec!["find", "app", "notes", "--sort", "n:desc", "--limit", "2"];
        if let Some(cursor) = &cursor {
            args.extend(["--cursor", cursor]);
        }
        let run = plaindb(dir.path(), Some("find"), &args);
        assert_eq!(run.code, 0);
        for entry in run.json["result"]["docs"].as_array().expect("docs") {
            seen.push(entry["doc"]["_id"].as_str().expect("an id").to_owned());
        }
        cursor = run.json["result"]["next_cursor"]
            .as_str()
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen, ["n4", "n3", "n2", "n1", "n0"]);
}

#[test]
fn find_filters_and_projects_fields() {
    let dir = fixture(false);
    let run = plaindb(
        dir.path(),
        Some("find"),
        &[
            "find",
            "app",
            "notes",
            "--filter",
            r#"{"tag":"even"}"#,
            "--sort",
            "n",
            "--fields",
            "n",
        ],
    );
    assert_eq!(run.code, 0);
    let docs = run.json["result"]["docs"].as_array().expect("docs");
    let numbers: Vec<&Value> = docs.iter().map(|entry| &entry["doc"]["n"]).collect();
    assert_eq!(numbers, [0, 2, 4]);
    assert!(docs[0]["doc"].get("tag").is_none());
}

#[test]
fn count_and_distinct_answer_over_a_filter() {
    let dir = fixture(false);
    let count = plaindb(
        dir.path(),
        Some("count"),
        &["count", "app", "notes", "--filter", r#"{"n":{"$gte":2}}"#],
    );
    assert_eq!(count.json["result"]["count"], 3);
    let distinct = plaindb(
        dir.path(),
        Some("distinct"),
        &["distinct", "app", "notes", "tag"],
    );
    let mut values: Vec<String> = distinct.json["result"]["values"]
        .as_array()
        .expect("values")
        .iter()
        .map(|value| value.as_str().expect("text").to_owned())
        .collect();
    values.sort();
    assert_eq!(values, ["even", "odd"]);
}

#[test]
fn listings_report_names_schema_and_indexes() {
    let dir = fixture(false);
    let dbs = plaindb(dir.path(), Some("dbs"), &["db", "list"]);
    assert_eq!(dbs.json["result"]["dbs"], json!(["app"]));
    let collections = plaindb(
        dir.path(),
        Some("collections"),
        &["collection", "list", "app"],
    );
    assert_eq!(collections.json["result"]["collections"], json!(["notes"]));
    let schema = plaindb(
        dir.path(),
        Some("schema"),
        &["collection", "schema", "get", "app", "notes"],
    );
    assert_eq!(schema.json["result"]["schema"]["type"], "object");
    let indexes = plaindb(
        dir.path(),
        Some("indexes"),
        &["index", "list", "app", "notes"],
    );
    assert_eq!(
        indexes.json["result"]["indexes"],
        json!([{ "field": "n", "unique": false, "multi": false }])
    );
}

#[test]
fn status_counts_documents_and_reports_git() {
    let dir = fixture(true);
    let run = plaindb(dir.path(), Some("status"), &["status"]);
    assert_eq!(run.code, 0);
    assert_eq!(run.json["result"]["documents"], 5);
    assert_eq!(run.json["result"]["git"]["enabled"], true);
    assert_eq!(run.json["result"]["invalid"], json!([]));
}

#[test]
fn history_and_get_at_read_earlier_versions() {
    let dir = fixture(true);
    let store = Store::open(dir.path()).expect("the store opens");
    store
        .apply(Tx {
            actor: Some("tester".to_owned()),
            ..Tx::new(vec![Op::Put {
                db: "app".to_owned(),
                coll: "notes".to_owned(),
                id: "n1".to_owned(),
                doc: doc(json!({ "n": 10 })),
                if_version: None,
            }])
        })
        .expect("a second version");
    drop(store);
    let history = plaindb(
        dir.path(),
        Some("history"),
        &["history", "app", "notes", "n1"],
    );
    assert_eq!(history.code, 0);
    let entries = history.json["result"]["entries"]
        .as_array()
        .expect("entries");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["actor"], "tester");
    let first = entries[1]["commit"].as_str().expect("a commit");
    let old = plaindb(
        dir.path(),
        Some("get"),
        &["get", "app", "notes", "n1", "--at", first],
    );
    assert_eq!(old.code, 0);
    assert_eq!(old.json["result"]["doc"]["n"], 1);
    let now = plaindb(
        dir.path(),
        Some("get"),
        &["get", "app", "notes", "n1", "--at", "2999-01-01T00:00:00Z"],
    );
    assert_eq!(now.json["result"]["doc"]["n"], 10);
}

#[test]
fn history_without_git_is_git_disabled() {
    let dir = fixture(false);
    let run = plaindb(dir.path(), None, &["history", "app", "notes", "n1"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.json["error"]["code"], "git_disabled");
}

#[test]
fn the_human_form_prints_documents_and_errors_apart() {
    let dir = fixture(false);
    let found = human(dir.path(), &["get", "app", "notes", "n2"]);
    assert_eq!(found.code, 0);
    let printed: Value = serde_json::from_str(&found.stdout).expect("pretty JSON");
    assert_eq!(printed["_id"], "n2");
    assert!(found.stdout.lines().count() > 1);
    let listed = human(dir.path(), &["find", "app", "notes", "--limit", "2"]);
    assert_eq!(listed.stdout.lines().count(), 2);
    let missing = human(dir.path(), &["get", "app", "notes", "nope"]);
    assert_eq!(missing.code, 1);
    assert!(missing.stdout.is_empty());
    assert!(missing.stderr.starts_with("error[not_found]: "));
}

#[test]
fn the_store_comes_from_the_environment_and_usage_errors_exit_two() {
    let dir = fixture(false);
    let output = Command::new(env!("CARGO_BIN_EXE_plaindb"))
        .env("PLAINDB_STORE", dir.path())
        .args(["--json", "count", "app", "notes"])
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(0));
    let usage = plaindb(dir.path(), None, &["get", "app"]);
    assert_eq!(usage.code, 2);
    assert!(usage.stdout.is_empty());
    assert!(!usage.stderr.is_empty());
}

fn set_tree_writable(path: &Path, writable: bool) {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(path).expect("the path exists");
    if metadata.file_type().is_symlink() {
        return;
    }
    let mut permissions = metadata.permissions();
    let mode = permissions.mode();
    permissions.set_mode(if writable {
        mode | 0o200
    } else {
        mode & !0o222
    });
    if writable {
        fs::set_permissions(path, permissions.clone()).expect("permissions change");
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).expect("the directory reads") {
            set_tree_writable(&entry.expect("an entry").path(), writable);
        }
    }
    if !writable {
        fs::set_permissions(path, permissions).expect("permissions change");
    }
}

type Files = std::collections::BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)>;

fn files_under(dir: &Path) -> Files {
    let mut files = Files::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).expect("the directory reads") {
            let entry = entry.expect("an entry");
            let path = entry.path();
            if entry.file_type().expect("a type").is_dir() {
                pending.push(path);
            } else {
                let modified = entry
                    .metadata()
                    .expect("metadata")
                    .modified()
                    .expect("a time");
                let bytes = fs::read(&path).expect("the file reads");
                files.insert(path, (bytes, modified));
            }
        }
    }
    files
}

#[test]
fn every_read_command_answers_on_a_read_only_store() {
    let dir = fixture(true);
    let store = dir.path();
    set_tree_writable(store, false);
    if fs::write(store.join("probe"), b"x").is_ok() {
        set_tree_writable(store, true);
        return;
    }
    let before = files_under(store);
    let commands: [(&str, &[&str]); 12] = [
        ("get", &["get", "app", "notes", "n1"]),
        ("find", &["find", "app", "notes"]),
        ("count", &["count", "app", "notes"]),
        ("distinct", &["distinct", "app", "notes", "tag"]),
        ("history", &["history", "app", "notes", "n1"]),
        ("status", &["status"]),
        ("validate", &["validate"]),
        ("doctor", &["doctor", "--offline"]),
        ("config", &["config", "get"]),
        ("dbs", &["db", "list"]),
        ("collections", &["collection", "list", "app"]),
        ("indexes", &["index", "list", "app", "notes"]),
    ];
    for (kind, args) in commands {
        let run = plaindb(store, Some(kind), args);
        assert_eq!(run.code, 0, "{args:?}: {} {}", run.stdout, run.stderr);
    }
    let export = human(store, &["export", "app", "notes", "--jsonl"]);
    assert_eq!(export.code, 0, "{}", export.stderr);
    assert_eq!(export.stdout.lines().count(), 5);
    let after = files_under(store);
    set_tree_writable(store, true);
    assert_eq!(after, before);
}

#[test]
fn a_read_leaves_the_journal_and_the_repository_alone() {
    let dir = fixture(true);
    let config_path = dir.path().join("_store.json");
    let mut config: Value =
        serde_json::from_slice(&fs::read(&config_path).expect("the config reads"))
            .expect("the config parses");
    config["checkpoint_interval"] = json!(1000);
    fs::write(&config_path, config.to_string()).expect("the config is written");
    let store = Store::open(dir.path()).expect("the store opens");
    store
        .apply(Tx::new(vec![Op::Put {
            db: "app".to_owned(),
            coll: "notes".to_owned(),
            id: "n9".to_owned(),
            doc: doc(json!({ "n": 9 })),
            if_version: None,
        }]))
        .expect("the document is written");
    std::mem::forget(store);
    let journal = dir.path().join(".plaindb").join("journal.jsonl");
    let journal_before = fs::read(&journal).expect("the journal reads");
    assert!(!journal_before.is_empty());
    let git_before = files_under(&dir.path().join(".git"));
    let run = plaindb(dir.path(), Some("get"), &["get", "app", "notes", "n9"]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    let journal_after = fs::read(&journal).expect("the journal reads");
    assert_eq!(journal_after, journal_before);
    assert_eq!(files_under(&dir.path().join(".git")), git_before);
}

fn store_with_documents(count: u64) -> TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = Store::init(dir.path()).expect("the store initialises");
    store.create_db("app").expect("a db");
    store
        .create_collection("app", "notes", Format::Json)
        .expect("a collection");
    let ops = (0..count)
        .map(|n| Op::Put {
            db: "app".to_owned(),
            coll: "notes".to_owned(),
            id: format!("n{n:04}"),
            doc: doc(json!({ "n": n })),
            if_version: None,
        })
        .collect();
    store.apply(Tx::new(ops)).expect("documents are written");
    dir
}

#[test]
fn find_all_returns_every_page_in_one_result() {
    let dir = store_with_documents(250);
    let run = plaindb(
        dir.path(),
        Some("find"),
        &["find", "app", "notes", "--all", "--sort", "n"],
    );
    assert_eq!(run.code, 0, "{}", run.json);
    let docs = run.json["result"]["docs"].as_array().expect("docs");
    assert_eq!(docs.len(), 250);
    assert_eq!(docs[249]["doc"]["_id"], "n0249");
    assert!(run.json["result"]["next_cursor"].is_null());
}

#[test]
fn find_all_stops_at_the_result_limit() {
    let dir = store_with_documents(250);
    let set = plaindb(
        dir.path(),
        None,
        &["config", "set", "limits.max_results", "200"],
    );
    assert_eq!(set.code, 0, "{}", set.json);
    let run = plaindb(dir.path(), None, &["find", "app", "notes", "--all"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.json["error"]["code"], "limit_exceeded");
}

#[test]
fn find_all_refuses_a_cursor_and_a_limit() {
    let dir = store_with_documents(3);
    for extra in [["--limit", "5"], ["--cursor", "abc"]] {
        let run = plaindb(
            dir.path(),
            None,
            &["find", "app", "notes", "--all", extra[0], extra[1]],
        );
        assert_eq!(run.code, 2, "{extra:?}");
    }
}

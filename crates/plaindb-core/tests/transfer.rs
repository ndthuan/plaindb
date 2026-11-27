use std::fs;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use plaindb_core::transfer::{ImportOptions, ImportReport, Origin};
use plaindb_core::{Format, IndexSpec, Store};
use serde_json::{Value, json};
use tempfile::TempDir;

fn store_with(format: Format) -> (TempDir, Store) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = Store::init(dir.path()).expect("the store initialises");
    store.create_db("app").expect("a db");
    store
        .create_collection("app", "docs", format)
        .expect("a collection");
    (dir, store)
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/import")
}

fn import_lines(store: &Store, lines: &str, replace: bool) -> ImportReport {
    let options = ImportOptions {
        replace,
        actor: None,
    };
    store
        .import_jsonl("app", "docs", lines.as_bytes(), &options)
        .expect("the import runs")
}

fn exported(store: &Store, coll: &str) -> Vec<Value> {
    let mut out = Vec::new();
    store
        .export_jsonl("app", coll, &mut out)
        .expect("the export runs");
    String::from_utf8(out)
        .expect("text")
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect()
}

fn without_version(mut docs: Vec<Value>) -> Vec<Value> {
    for doc in &mut docs {
        doc.as_object_mut().expect("an object").remove("_version");
    }
    docs
}

fn generation(store: &Store) -> u64 {
    store.status().expect("status");
    store.snapshot().generation
}

#[test]
fn a_directory_import_reads_each_file_by_its_extension() {
    let (_dir, store) = store_with(Format::Json);
    let report = store
        .import_dir("app", "docs", &fixture_dir(), &ImportOptions::default())
        .expect("the import runs");
    assert_eq!(report.imported, 2);
    assert_eq!(report.omitted.len(), 1);
    assert_eq!(report.omitted[0].file, "bad name.json");
    assert_eq!(report.omitted[0].reason, "invalid_name");
    assert!(report.failed.is_empty());
    let (plain, _) = store
        .get("app", "docs", "a")
        .expect("read")
        .expect("present");
    assert_eq!(
        plain["body"],
        "Plain text with no frontmatter.\n\nSecond paragraph.\n"
    );
    let (yaml, _) = store
        .get("app", "docs", "b")
        .expect("read")
        .expect("present");
    assert_eq!(yaml["title"], "From yaml");
    assert_eq!(yaml["count"], 3);
    assert!(store.get("app", "docs", "c").expect("read").is_none());
    assert_eq!(store.list_ids("app", "docs").expect("ids").len(), 2);
}

#[test]
fn a_directory_import_writes_in_the_format_of_the_collection() {
    let (dir, store) = store_with(Format::Yaml);
    store
        .import_dir("app", "docs", &fixture_dir(), &ImportOptions::default())
        .expect("the import runs");
    assert!(dir.path().join("app/docs/a.yaml").is_file());
    assert!(dir.path().join("app/docs/b.yaml").is_file());
}

#[test]
fn a_file_whose_id_differs_from_its_name_fails() {
    let (_dir, store) = store_with(Format::Json);
    let source = tempfile::tempdir().expect("a temp dir");
    fs::write(source.path().join("one.json"), r#"{"_id":"two","n":1}"#).expect("a file");
    fs::write(source.path().join("broken.json"), "{oops").expect("a file");
    let report = store
        .import_dir("app", "docs", source.path(), &ImportOptions::default())
        .expect("the import runs");
    assert_eq!(report.imported, 0);
    assert_eq!(report.failed.len(), 2);
    assert!(
        report
            .failed
            .iter()
            .all(|failure| failure.code == "invalid_document")
    );
    assert!(matches!(&report.failed[0].origin, Origin::File(name) if name == "broken.json"));
}

#[test]
fn one_failing_document_among_ten_leaves_nine() {
    let (_dir, store) = store_with(Format::Json);
    store
        .set_schema(
            "app",
            "docs",
            Some(json!({ "type": "object", "properties": { "n": { "type": "integer" } } })),
        )
        .expect("a schema");
    let lines: String = (0..10)
        .map(|n| {
            let value = if n == 6 { json!("six") } else { json!(n) };
            format!("{}\n", json!({ "_id": format!("d{n}"), "n": value }))
        })
        .collect();
    let report = import_lines(&store, &lines, false);
    assert_eq!(report.imported, 9);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].origin, Origin::Line(7));
    assert_eq!(report.failed[0].code, "schema_violation");
    assert!(report.failed[0].details["errors"].is_array());
    assert!(store.get("app", "docs", "d6").expect("read").is_none());
    assert_eq!(store.list_ids("app", "docs").expect("ids").len(), 9);
}

#[test]
fn bad_lines_are_counted_with_their_numbers_and_blank_lines_are_ignored() {
    let (_dir, store) = store_with(Format::Json);
    let lines = "{\"_id\":\"a\"}\n\n{broken\n[1,2]\n   \n{\"_id\":\"-bad\"}\n{\"n\":5}\n";
    let report = import_lines(&store, lines, false);
    assert_eq!(report.imported, 2);
    let failed: Vec<(Origin, &str)> = report
        .failed
        .iter()
        .map(|failure| (failure.origin.clone(), failure.code.as_str()))
        .collect();
    assert_eq!(
        failed,
        [
            (Origin::Line(3), "invalid_document"),
            (Origin::Line(4), "invalid_document"),
            (Origin::Line(6), "invalid_name"),
        ]
    );
}

#[test]
fn an_existing_id_fails_unless_replace_is_given() {
    let (_dir, store) = store_with(Format::Json);
    import_lines(&store, "{\"_id\":\"a\",\"n\":1}\n", false);
    let again = import_lines(
        &store,
        "{\"_id\":\"a\",\"n\":2}\n{\"_id\":\"b\",\"n\":3}\n{\"_id\":\"b\"}\n",
        false,
    );
    assert_eq!(again.imported, 1);
    let failed: Vec<(Origin, &str)> = again
        .failed
        .iter()
        .map(|failure| (failure.origin.clone(), failure.code.as_str()))
        .collect();
    assert_eq!(
        failed,
        [
            (Origin::Line(1), "already_exists"),
            (Origin::Line(3), "duplicate_in_input"),
        ]
    );
    let (kept, _) = store
        .get("app", "docs", "a")
        .expect("read")
        .expect("present");
    assert_eq!(kept["n"], 1);
    let replaced = import_lines(&store, "{\"_id\":\"a\",\"n\":2}\n", true);
    assert_eq!(replaced.imported, 1);
    let (now, _) = store
        .get("app", "docs", "a")
        .expect("read")
        .expect("present");
    assert_eq!(now["n"], 2);
}

#[test]
fn timestamps_of_the_source_are_kept_and_missing_ones_are_set() {
    let (_dir, store) = store_with(Format::Json);
    let lines = "{\"_id\":\"old\",\"_created_at\":\"2020-01-02T03:04:05.000Z\",\"_updated_at\":\"2021-01-02T03:04:05.000Z\"}\n{\"_id\":\"new\"}\n";
    import_lines(&store, lines, false);
    let (old, _) = store
        .get("app", "docs", "old")
        .expect("read")
        .expect("present");
    assert_eq!(old["_created_at"], "2020-01-02T03:04:05.000Z");
    assert_eq!(old["_updated_at"], "2021-01-02T03:04:05.000Z");
    let (fresh, _) = store
        .get("app", "docs", "new")
        .expect("read")
        .expect("present");
    assert!(
        fresh["_created_at"]
            .as_str()
            .is_some_and(|text| !text.starts_with("2020"))
    );
    assert!(fresh["_updated_at"].is_string());
}

#[test]
fn documents_are_written_in_transactions_of_five_hundred() {
    let (_dir, store) = store_with(Format::Json);
    let before = generation(&store);
    let lines: String = (0..1200)
        .map(|n| format!("{}\n", json!({ "_id": format!("d{n:04}"), "n": n })))
        .collect();
    let report = import_lines(&store, &lines, false);
    assert_eq!(report.imported, 1200);
    assert_eq!(generation(&store) - before, 3);
    assert_eq!(store.list_ids("app", "docs").expect("ids").len(), 1200);
}

#[test]
fn a_document_the_write_refuses_is_reported_alone() {
    let (_dir, store) = store_with(Format::Json);
    store
        .set_indexes(
            "app",
            "docs",
            vec![IndexSpec::Field {
                field: "email".to_owned(),
                unique: true,
                multi: false,
            }],
        )
        .expect("an index");
    let lines: String = (0..8)
        .map(|n| {
            let email = if n == 5 {
                "user2@example.com".to_owned()
            } else {
                format!("user{n}@example.com")
            };
            format!("{}\n", json!({ "_id": format!("u{n}"), "email": email }))
        })
        .collect();
    let report = import_lines(&store, &lines, false);
    assert_eq!(report.imported, 7);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].code, "unique_violation");
    assert_eq!(report.failed[0].origin, Origin::Line(6));
}

#[test]
fn export_lists_every_document_with_its_version_in_id_order() {
    let (_dir, store) = store_with(Format::Json);
    import_lines(
        &store,
        "{\"_id\":\"b\",\"n\":2}\n{\"_id\":\"a\",\"n\":1}\n{\"_id\":\"c\",\"n\":3}\n",
        false,
    );
    let docs = exported(&store, "docs");
    let ids: Vec<&str> = docs
        .iter()
        .map(|doc| doc["_id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, ["a", "b", "c"]);
    for doc in &docs {
        let (_, version) = store
            .get("app", "docs", doc["_id"].as_str().expect("id"))
            .expect("read")
            .expect("present");
        assert_eq!(doc["_version"], version.as_str());
    }
}

#[test]
fn an_export_imports_again_into_a_new_collection_unchanged() {
    let (_dir, store) = store_with(Format::Json);
    store
        .create_collection("app", "copy", Format::Json)
        .expect("a collection");
    let lines: String = (0..25)
        .map(|n| {
            format!(
                "{}\n",
                json!({ "_id": format!("d{n:02}"), "n": n, "nested": { "list": [n, "x"] }, "tag": null })
            )
        })
        .collect();
    import_lines(&store, &lines, false);
    let mut out = Vec::new();
    store.export_jsonl("app", "docs", &mut out).expect("export");
    let report = store
        .import_jsonl("app", "copy", out.as_slice(), &ImportOptions::default())
        .expect("import");
    assert_eq!(report.imported, 25);
    assert!(report.failed.is_empty());
    assert_eq!(
        without_version(exported(&store, "docs")),
        without_version(exported(&store, "copy"))
    );
}

#[test]
fn a_markdown_body_survives_import_and_export_byte_for_byte() {
    let (_dir, store) = store_with(Format::Md);
    let body = "\n\n  indented line  \r\nsecond line\twith tab \n\u{1f600} caf\u{e9} \u{1ec7}\n---\nnot a fence\n\n\n";
    let line = format!(
        "{}\n",
        json!({ "_id": "note", "title": "Note", "body": body })
    );
    let report = import_lines(&store, &line, false);
    assert_eq!(report.imported, 1);
    let docs = exported(&store, "docs");
    assert_eq!(docs[0]["body"], body);

    let source = tempfile::tempdir().expect("a temp dir");
    let file_body = "\r\n  keep   \n\n\u{e9}\n";
    fs::write(
        source.path().join("page.md"),
        format!("---\ntitle: Page\n---\n{file_body}"),
    )
    .expect("a file");
    fs::write(source.path().join("bare.md"), file_body).expect("a file");
    store
        .import_dir("app", "docs", source.path(), &ImportOptions::default())
        .expect("import");
    let docs = exported(&store, "docs");
    let page = docs.iter().find(|doc| doc["_id"] == "page").expect("page");
    let bare = docs.iter().find(|doc| doc["_id"] == "bare").expect("bare");
    assert_eq!(page["body"], file_body);
    assert_eq!(page["title"], "Page");
    assert_eq!(bare["body"], file_body);
}

#[test]
fn a_jsonl_import_drops_a_leading_byte_order_mark() {
    let (_dir, store) = store_with(Format::Json);
    let report = import_lines(
        &store,
        "\u{feff}{\"_id\": \"a\", \"n\": 1}\n{\"_id\": \"b\", \"n\": 2}\n",
        false,
    );
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.imported, 2);
    let (doc, _) = store.get("app", "docs", "a").unwrap().unwrap();
    assert_eq!(doc["n"], 1);
}

fn commits(root: &Path) -> u64 {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-list", "--count", "HEAD"])
        .output()
        .expect("git runs");
    String::from_utf8(output.stdout)
        .expect("text")
        .trim()
        .parse()
        .expect("a count")
}

fn git_store() -> (TempDir, Store) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = Store::init_with_git(dir.path()).expect("the store initialises");
    store.create_db("app").expect("a db");
    store
        .create_collection("app", "docs", Format::Json)
        .expect("a collection");
    (dir, store)
}

fn codes(report: &ImportReport) -> Vec<(Origin, &str)> {
    report
        .failed
        .iter()
        .map(|failure| (failure.origin.clone(), failure.code.as_str()))
        .collect()
}

#[test]
fn a_repeated_id_in_one_input_fails_as_a_duplicate_with_or_without_replace() {
    for replace in [false, true] {
        let (_dir, store) = store_with(Format::Json);
        let report = import_lines(
            &store,
            "{\"_id\":\"dup1\",\"n\":1}\n{\"_id\":\"dup1\",\"n\":2}\n{\"_id\":\"DUP1\",\"n\":3}\n",
            replace,
        );
        assert_eq!(report.imported, 1, "replace {replace}");
        assert_eq!(
            codes(&report),
            [
                (Origin::Line(2), "duplicate_in_input"),
                (Origin::Line(3), "duplicate_in_input"),
            ],
            "replace {replace}"
        );
        let (kept, _) = store.get("app", "docs", "dup1").unwrap().unwrap();
        assert_eq!(kept["n"], 1, "replace {replace}");
        assert_eq!(store.list_ids("app", "docs").unwrap().len(), 1);
    }
}

#[test]
fn a_duplicate_directory_file_fails_as_a_duplicate() {
    let (_dir, store) = store_with(Format::Json);
    let source = tempfile::tempdir().expect("a temp dir");
    fs::write(source.path().join("a.json"), r#"{"n":1}"#).expect("a file");
    fs::write(source.path().join("a.yaml"), "n: 2\n").expect("a file");
    let report = store
        .import_dir("app", "docs", source.path(), &ImportOptions::default())
        .expect("the import runs");
    assert_eq!(report.imported, 1);
    assert_eq!(
        codes(&report),
        [(Origin::File("a.yaml".to_owned()), "duplicate_in_input")]
    );
}

#[test]
fn an_id_of_a_failed_line_is_still_a_duplicate_later() {
    let (_dir, store) = store_with(Format::Json);
    store
        .set_schema(
            "app",
            "docs",
            Some(json!({ "type": "object", "properties": { "n": { "type": "integer" } } })),
        )
        .expect("a schema");
    let lines: String = (1..=9)
        .map(|line| match line {
            3 => format!("{}\n", json!({ "_id": "x", "n": "three" })),
            9 => format!("{}\n", json!({ "_id": "x", "n": 9 })),
            other => format!("{}\n", json!({ "_id": format!("d{other}"), "n": other })),
        })
        .collect();
    let report = import_lines(&store, &lines, false);
    assert_eq!(report.imported, 7);
    assert_eq!(
        codes(&report),
        [
            (Origin::Line(3), "schema_violation"),
            (Origin::Line(9), "duplicate_in_input"),
        ]
    );
    assert!(store.get("app", "docs", "x").unwrap().is_none());
}

#[test]
fn an_id_already_in_the_collection_still_fails_as_existing() {
    let (_dir, store) = store_with(Format::Json);
    import_lines(&store, "{\"_id\":\"a\",\"n\":1}\n", false);
    let again = import_lines(&store, "{\"_id\":\"a\",\"n\":2}\n", false);
    assert_eq!(codes(&again), [(Origin::Line(1), "already_exists")]);
}

fn numbered(count: u64, line: impl Fn(u64) -> Value) -> String {
    (1..=count).map(|n| format!("{}\n", line(n))).collect()
}

#[test]
fn a_batch_lands_in_one_commit_even_when_documents_fail_in_it() {
    let (dir, store) = git_store();
    let before = commits(dir.path());
    let report = import_lines(
        &store,
        &numbered(1201, |n| json!({ "_id": format!("d{n:04}") })),
        false,
    );
    assert_eq!(report.imported, 1201);
    assert_eq!(commits(dir.path()) - before, 3);

    let (dir, store) = git_store();
    store
        .set_schema(
            "app",
            "docs",
            Some(json!({ "type": "object", "properties": { "n": { "type": "integer" } } })),
        )
        .expect("a schema");
    let before = commits(dir.path());
    let lines = numbered(1201, |n| {
        let value = if n == 700 {
            json!("seven hundred")
        } else {
            json!(n)
        };
        json!({ "_id": format!("d{n:04}"), "n": value })
    });
    let report = import_lines(&store, &lines, false);
    assert_eq!(report.imported, 1200);
    assert_eq!(codes(&report), [(Origin::Line(700), "schema_violation")]);
    assert!(report.failed[0].details["errors"].is_array());
    assert_eq!(commits(dir.path()) - before, 3);
}

fn unique_email(store: &Store) {
    store
        .set_indexes(
            "app",
            "docs",
            vec![IndexSpec::Field {
                field: "email".to_owned(),
                unique: true,
                multi: false,
            }],
        )
        .expect("an index");
}

#[test]
fn documents_the_write_refuses_leave_the_rest_of_their_batch_in_one_commit() {
    let (dir, store) = git_store();
    unique_email(&store);
    store
        .set_setting("limits.max_document_bytes", "400")
        .expect("a limit");
    let before = commits(dir.path());
    let lines = numbered(1201, |n| match n {
        700 => json!({ "_id": "d0700", "email": "user10@example.com" }),
        900 => json!({ "_id": "d0900", "email": "big@example.com", "text": "x".repeat(600) }),
        n => json!({ "_id": format!("d{n:04}"), "email": format!("user{n}@example.com") }),
    });
    let report = import_lines(&store, &lines, false);
    assert_eq!(report.imported, 1199);
    assert_eq!(
        codes(&report),
        [
            (Origin::Line(700), "unique_violation"),
            (Origin::Line(900), "limit_exceeded"),
        ]
    );
    assert_eq!(report.failed[0].details["field"], "email");
    assert_eq!(commits(dir.path()) - before, 3);
    let (first, _) = store.get("app", "docs", "d0010").unwrap().unwrap();
    assert_eq!(first["email"], "user10@example.com");
    assert!(store.get("app", "docs", "d0700").unwrap().is_none());
    assert!(report.aborted.is_none());
}

#[test]
fn two_lines_with_one_unique_value_in_a_batch_keep_the_first() {
    let (dir, store) = git_store();
    unique_email(&store);
    let before = commits(dir.path());
    let report = import_lines(
        &store,
        "{\"_id\":\"z\",\"email\":\"same@example.com\"}\n{\"_id\":\"a\",\"email\":\"same@example.com\"}\n",
        false,
    );
    assert_eq!(report.imported, 1);
    assert_eq!(codes(&report), [(Origin::Line(2), "unique_violation")]);
    assert!(store.get("app", "docs", "z").unwrap().is_some());
    assert_eq!(commits(dir.path()) - before, 1);
}

type Hook = Box<dyn FnMut(u64) -> io::Result<()>>;

struct Lines {
    lines: std::collections::VecDeque<Vec<u8>>,
    current: Vec<u8>,
    position: usize,
    number: u64,
    hook: Hook,
}

impl Lines {
    fn new(text: &str, hook: impl FnMut(u64) -> io::Result<()> + 'static) -> Self {
        Self {
            lines: text
                .split_inclusive('\n')
                .map(|line| line.as_bytes().to_vec())
                .collect(),
            current: Vec::new(),
            position: 0,
            number: 0,
            hook: Box::new(hook),
        }
    }
}

impl Read for Lines {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let count = available.len().min(buf.len());
        buf[..count].copy_from_slice(&available[..count]);
        self.consume(count);
        Ok(count)
    }
}

impl BufRead for Lines {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.position == self.current.len() {
            let Some(next) = self.lines.pop_front() else {
                return Ok(&[]);
            };
            self.number += 1;
            (self.hook)(self.number)?;
            self.current = next;
            self.position = 0;
        }
        Ok(&self.current[self.position..])
    }

    fn consume(&mut self, amount: usize) {
        self.position += amount;
    }
}

fn import_from(store: &Store, reader: Lines) -> ImportReport {
    store
        .import_jsonl("app", "docs", reader, &ImportOptions::default())
        .expect("the import returns a report")
}

#[test]
fn a_lock_held_by_another_writer_stops_the_import_with_a_report() {
    let (dir, store) = store_with(Format::Json);
    store
        .set_setting("lock_timeout_ms", "100")
        .expect("a timeout");
    let lock_path = dir.path().join(".plaindb").join("lock");
    let mut held = Vec::new();
    let reader = Lines::new(
        &numbered(1200, |n| json!({ "_id": format!("d{n:04}") })),
        move |line| {
            if line == 501 {
                let file = fs::OpenOptions::new().write(true).open(&lock_path)?;
                file.lock()?;
                held.push(file);
            }
            Ok(())
        },
    );
    let report = import_from(&store, reader);
    assert_eq!(report.imported, 500);
    let aborted = report.aborted.as_ref().expect("the import stopped");
    assert_eq!(aborted.code, "store_locked_timeout");
    assert!(!aborted.message.is_empty());
    assert!(report.failed.is_empty());
    assert_eq!(store.list_ids("app", "docs").unwrap().len(), 500);
}

#[test]
fn an_input_that_cannot_be_read_stops_the_import_with_a_report() {
    let (_dir, store) = store_with(Format::Json);
    let reader = Lines::new(
        &numbered(700, |n| json!({ "_id": format!("d{n:04}") })),
        |line| {
            if line == 650 {
                return Err(io::Error::other("the disk went away"));
            }
            Ok(())
        },
    );
    let report = import_from(&store, reader);
    assert_eq!(report.imported, 500);
    let aborted = report.aborted.as_ref().expect("the import stopped");
    assert_eq!(aborted.code, "io");
    assert!(
        aborted.message.contains("the disk went away"),
        "{}",
        aborted.message
    );
    assert_eq!(store.list_ids("app", "docs").unwrap().len(), 500);
}

#[test]
fn a_timestamp_that_is_not_rfc_3339_text_fails_the_line() {
    let (_dir, store) = store_with(Format::Json);
    let report = import_lines(
        &store,
        "{\"_id\":\"a\",\"_created_at\":5}\n{\"_id\":\"b\",\"_updated_at\":\"yesterday\"}\n{\"_id\":\"c\",\"_created_at\":\"2020-01-02T03:04:05Z\"}\n",
        false,
    );
    assert_eq!(report.imported, 1);
    assert_eq!(
        codes(&report),
        [
            (Origin::Line(1), "invalid_document"),
            (Origin::Line(2), "invalid_document"),
        ]
    );
    assert_eq!(report.failed[0].details["reason"], "bad_timestamp");
    assert_eq!(report.failed[0].details["field"], "_created_at");
    assert_eq!(report.failed[1].details["field"], "_updated_at");
    let (kept, _) = store.get("app", "docs", "c").unwrap().unwrap();
    assert_eq!(kept["_created_at"], "2020-01-02T03:04:05Z");
}

#[test]
fn a_line_longer_than_the_document_limit_fails_and_the_next_one_imports() {
    let (_dir, store) = store_with(Format::Json);
    store
        .set_setting("limits.max_document_bytes", "1000")
        .expect("a limit");
    let long = json!({ "_id": "long", "text": "x".repeat(2000) });
    let lines = format!("{long}\n{}\n", json!({ "_id": "short" }));
    let report = import_lines(&store, &lines, false);
    assert_eq!(report.imported, 1);
    assert_eq!(codes(&report), [(Origin::Line(1), "limit_exceeded")]);
    assert_eq!(report.failed[0].details["limit"], "max_document_bytes");
    assert_eq!(report.failed[0].details["max"], 1000);
    assert!(store.get("app", "docs", "short").unwrap().is_some());
    assert!(store.get("app", "docs", "long").unwrap().is_none());
}

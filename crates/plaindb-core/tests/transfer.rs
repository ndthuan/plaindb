use std::fs;
use std::path::PathBuf;

use plaindb_core::transfer::{ImportOptions, ImportReport, Origin};
use plaindb_core::{ErrorCode, Format, IndexSpec, Store};
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
            .all(|failure| failure.code == ErrorCode::InvalidDocument)
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
    assert_eq!(report.failed[0].code, ErrorCode::SchemaViolation);
    assert!(store.get("app", "docs", "d6").expect("read").is_none());
    assert_eq!(store.list_ids("app", "docs").expect("ids").len(), 9);
}

#[test]
fn bad_lines_are_counted_with_their_numbers_and_blank_lines_are_ignored() {
    let (_dir, store) = store_with(Format::Json);
    let lines = "{\"_id\":\"a\"}\n\n{broken\n[1,2]\n   \n{\"_id\":\"-bad\"}\n{\"n\":5}\n";
    let report = import_lines(&store, lines, false);
    assert_eq!(report.imported, 2);
    let failed: Vec<(Origin, ErrorCode)> = report
        .failed
        .iter()
        .map(|failure| (failure.origin.clone(), failure.code))
        .collect();
    assert_eq!(
        failed,
        [
            (Origin::Line(3), ErrorCode::InvalidDocument),
            (Origin::Line(4), ErrorCode::InvalidDocument),
            (Origin::Line(6), ErrorCode::InvalidName),
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
    assert_eq!(again.failed.len(), 2);
    assert!(
        again
            .failed
            .iter()
            .all(|failure| failure.code == ErrorCode::AlreadyExists)
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
    assert_eq!(report.failed[0].code, ErrorCode::UniqueViolation);
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

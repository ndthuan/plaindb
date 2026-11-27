use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use plaindb_core::{CollectionMeta, ErrorCode, Format, Store};
use plaindb_core::{Durability, StoreConfig};
use serde_json::json;

fn make_collection(root: &Path, db: &str, collection: &str, meta: &CollectionMeta) {
    let dir = root.join(db).join(collection);
    fs::create_dir_all(&dir).unwrap();
    fs::write(root.join(db).join("_db.json"), "{\"format\":1}\n").unwrap();
    fs::write(dir.join("_meta.json"), serde_json::to_vec(meta).unwrap()).unwrap();
}

fn json_store(root: &Path) -> Store {
    let store = Store::init_with(
        root,
        StoreConfig {
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    make_collection(root, "main", "users", &CollectionMeta::new(Format::Json));
    store.rescan().unwrap();
    store
}

fn users(root: &Path) -> std::path::PathBuf {
    root.join("main").join("users")
}

#[test]
fn a_hand_edit_between_two_opens_is_seen() {
    let temp = tempfile::tempdir().unwrap();
    json_store(temp.path());
    fs::write(users(temp.path()).join("bob.json"), "{\"name\": \"Bob\"}").unwrap();
    let store = Store::open(temp.path()).unwrap();
    let (doc, version) = store.get("main", "users", "bob").unwrap().unwrap();
    assert_eq!(doc["name"], json!("Bob"));
    assert_eq!(doc["_id"], json!("bob"));
    assert_eq!(version, plaindb_core::version_of(b"{\"name\": \"Bob\"}"));
    fs::write(
        users(temp.path()).join("bob.json"),
        "{\"name\": \"Robert\"}",
    )
    .unwrap();
    let store = Store::open(temp.path()).unwrap();
    let (doc, _) = store.get("main", "users", "bob").unwrap().unwrap();
    assert_eq!(doc["name"], json!("Robert"));
}

#[test]
fn rescan_sees_edits_additions_and_removals() {
    let temp = tempfile::tempdir().unwrap();
    let store = json_store(temp.path());
    let dir = users(temp.path());
    fs::write(dir.join("a.json"), "{\"n\": 1}").unwrap();
    fs::write(dir.join("b.json"), "{\"n\": 2}").unwrap();
    store.rescan().unwrap();
    assert_eq!(store.list_ids("main", "users").unwrap(), vec!["a", "b"]);

    fs::write(dir.join("a.json"), "{\"n\": 1000}").unwrap();
    fs::remove_file(dir.join("b.json")).unwrap();
    fs::write(dir.join("c.json"), "{\"n\": 3}").unwrap();
    store.rescan().unwrap();
    assert_eq!(store.list_ids("main", "users").unwrap(), vec!["a", "c"]);
    let (doc, _) = store.get("main", "users", "a").unwrap().unwrap();
    assert_eq!(doc["n"], json!(1000));
    assert!(store.get("main", "users", "b").unwrap().is_none());
}

#[test]
fn a_same_size_edit_is_seen_through_the_modification_time() {
    let temp = tempfile::tempdir().unwrap();
    let store = json_store(temp.path());
    let path = users(temp.path()).join("a.json");
    fs::write(&path, "{\"n\": 1}").unwrap();
    store.rescan().unwrap();
    fs::write(&path, "{\"n\": 2}").unwrap();
    let file = fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(60))
        .unwrap();
    drop(file);
    store.rescan().unwrap();
    let (doc, _) = store.get("main", "users", "a").unwrap().unwrap();
    assert_eq!(doc["n"], json!(2));
}

#[test]
fn a_collection_made_by_another_process_appears_after_rescan() {
    let temp = tempfile::tempdir().unwrap();
    let store = json_store(temp.path());
    assert_eq!(
        store.get("main", "notes", "x").unwrap_err().details["kind"],
        json!("collection")
    );
    make_collection(
        temp.path(),
        "main",
        "notes",
        &CollectionMeta::new(Format::Md),
    );
    make_collection(
        temp.path(),
        "other",
        "things",
        &CollectionMeta::new(Format::Yaml),
    );
    fs::write(
        temp.path().join("main").join("notes").join("n1.md"),
        "hello",
    )
    .unwrap();
    store.rescan().unwrap();
    let (doc, _) = store.get("main", "notes", "n1").unwrap().unwrap();
    assert_eq!(doc["body"], json!("hello"));
    assert!(store.get("other", "things", "x").unwrap().is_none());
    assert_eq!(
        store.get("missing", "things", "x").unwrap_err().details["kind"],
        json!("db")
    );
}

#[test]
fn a_broken_file_is_invalid_and_refuses_reads() {
    let temp = tempfile::tempdir().unwrap();
    let store = json_store(temp.path());
    let dir = users(temp.path());
    fs::write(dir.join("ok.json"), "{\"n\": 1}").unwrap();
    fs::write(dir.join("broken.json"), "{\"n\": ").unwrap();
    store.rescan().unwrap();
    let status = store.status().unwrap();
    assert_eq!(status.documents, 1);
    assert_eq!(status.invalid.len(), 1);
    assert_eq!(status.invalid[0].id, "broken");
    assert_eq!(status.invalid[0].path, dir.join("broken.json"));
    let error = store.get("main", "users", "broken").unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidDocument);
    assert_eq!(store.list_ids("main", "users").unwrap(), vec!["ok"]);
    assert!(status.store_bytes > 0);
}

#[cfg(target_os = "linux")]
#[test]
fn ids_equal_ignoring_case_are_both_invalid() {
    let temp = tempfile::tempdir().unwrap();
    let store = json_store(temp.path());
    let dir = users(temp.path());
    fs::write(dir.join("bob.json"), "{\"n\": 1}").unwrap();
    fs::write(dir.join("Bob.json"), "{\"n\": 2}").unwrap();
    store.rescan().unwrap();
    let status = store.status().unwrap();
    assert_eq!(status.invalid.len(), 2);
    assert!(
        status
            .invalid
            .iter()
            .all(|file| file.reason == "duplicate_id_case")
    );
    assert_eq!(status.documents, 0);
    assert_eq!(
        store.get("main", "users", "bob").unwrap_err().code,
        ErrorCode::InvalidDocument
    );
}

#[test]
fn the_file_name_wins_over_a_different_id_and_is_reported() {
    let temp = tempfile::tempdir().unwrap();
    let store = json_store(temp.path());
    fs::write(
        users(temp.path()).join("y.json"),
        "{\"_id\": \"x\", \"n\": 1}",
    )
    .unwrap();
    store.rescan().unwrap();
    let (doc, _) = store.get("main", "users", "y").unwrap().unwrap();
    assert_eq!(doc["_id"], json!("y"));
    assert!(store.get("main", "users", "x").unwrap().is_none());
    let status = store.status().unwrap();
    assert_eq!(status.warnings.len(), 1);
    assert_eq!(status.warnings[0].code, "id_mismatch");
    assert_eq!(status.documents, 1);
}

#[test]
fn missing_timestamps_stay_absent() {
    let temp = tempfile::tempdir().unwrap();
    let store = json_store(temp.path());
    fs::write(users(temp.path()).join("a.json"), "{\"n\": 1}").unwrap();
    store.rescan().unwrap();
    let (doc, _) = store.get("main", "users", "a").unwrap().unwrap();
    assert!(!doc.contains_key("_created_at"));
    assert!(!doc.contains_key("_updated_at"));
}

#[test]
fn a_document_failing_the_schema_is_invalid() {
    let temp = tempfile::tempdir().unwrap();
    Store::init_with(
        temp.path(),
        StoreConfig {
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    let mut meta = CollectionMeta::new(Format::Json);
    meta.schema = Some(json!({
        "type": "object",
        "required": ["name"],
        "additionalProperties": false,
        "properties": {"name": {"type": "string"}}
    }));
    make_collection(temp.path(), "main", "users", &meta);
    let dir = users(temp.path());
    fs::write(
        dir.join("good.json"),
        "{\"_id\": \"good\", \"name\": \"Ann\"}",
    )
    .unwrap();
    fs::write(dir.join("bad.json"), "{\"name\": 3}").unwrap();
    let store = Store::open(temp.path()).unwrap();
    assert!(store.get("main", "users", "good").unwrap().is_some());
    let status = store.status().unwrap();
    assert_eq!(status.invalid.len(), 1);
    assert_eq!(status.invalid[0].reason, "schema");
    assert_eq!(
        store.get("main", "users", "bad").unwrap_err().code,
        ErrorCode::InvalidDocument
    );
}

#[test]
fn other_files_and_bad_names_are_not_documents() {
    let temp = tempfile::tempdir().unwrap();
    let store = json_store(temp.path());
    let dir = users(temp.path());
    fs::write(dir.join("notes.txt"), "x").unwrap();
    fs::write(dir.join(".hidden.json"), "{}").unwrap();
    fs::write(dir.join("_odd.json"), "{}").unwrap();
    fs::write(dir.join("fine.json"), "{}").unwrap();
    store.rescan().unwrap();
    assert_eq!(store.list_ids("main", "users").unwrap(), vec!["fine"]);
    let status = store.status().unwrap();
    assert_eq!(status.warnings.len(), 1);
    assert_eq!(status.warnings[0].code, "invalid_name");
    assert_eq!(
        store.get("main", "users", "../x").unwrap_err().code,
        ErrorCode::InvalidName
    );
}

fn snapshot_of_files(dir: &Path) -> Vec<(String, Vec<u8>, SystemTime)> {
    let mut found: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                fs::read(entry.path()).unwrap(),
                entry.metadata().unwrap().modified().unwrap(),
            )
        })
        .collect();
    found.sort();
    found
}

#[test]
fn rescan_never_touches_a_file() {
    let temp = tempfile::tempdir().unwrap();
    let store = json_store(temp.path());
    let dir = users(temp.path());
    fs::write(dir.join("a.json"), "{\"z\":1,   \"a\":2}").unwrap();
    fs::write(dir.join("broken.json"), "nope").unwrap();
    fs::write(dir.join("m.json"), "{\"_id\": \"other\"}").unwrap();
    let before = snapshot_of_files(&dir);
    store.rescan().unwrap();
    store.rescan().unwrap();
    Store::open(temp.path()).unwrap();
    assert_eq!(snapshot_of_files(&dir), before);
}

#[cfg(unix)]
fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
#[test]
fn an_unreadable_file_is_invalid_and_the_rest_reads() {
    if is_root() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    json_store(temp.path());
    let dir = users(temp.path());
    fs::write(dir.join("ok.json"), "{\"n\": 1}").unwrap();
    fs::write(dir.join("u1.json"), "{\"n\": 2}").unwrap();
    set_mode(&dir.join("u1.json"), 0o000);
    let store = Store::open(temp.path()).unwrap();
    assert!(store.get("main", "users", "ok").unwrap().is_some());
    let status = store.status().unwrap();
    assert_eq!(status.invalid.len(), 1);
    assert_eq!(status.invalid[0].id, "u1");
    assert_eq!(status.invalid[0].reason, "unreadable");
    set_mode(&dir.join("u1.json"), 0o644);
    store.rescan().unwrap();
    let (doc, _) = store.get("main", "users", "u1").unwrap().unwrap();
    assert_eq!(doc["n"], json!(2));
    assert!(store.status().unwrap().invalid.is_empty());
}

#[test]
fn a_file_over_the_scan_limit_is_not_read() {
    let temp = tempfile::tempdir().unwrap();
    json_store(temp.path());
    let dir = users(temp.path());
    fs::write(dir.join("ok.json"), "{\"n\": 1}").unwrap();
    fs::File::create(dir.join("big.json"))
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    let started = std::time::Instant::now();
    let store = Store::open(temp.path()).unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(store.get("main", "users", "ok").unwrap().is_some());
    let status = store.status().unwrap();
    assert_eq!(status.invalid.len(), 1);
    assert_eq!(status.invalid[0].id, "big");
    assert_eq!(status.invalid[0].reason, "too_large");
}

#[test]
fn a_file_over_the_document_limit_is_too_large() {
    let temp = tempfile::tempdir().unwrap();
    json_store(temp.path());
    let config_path = temp.path().join("_store.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["limits"]["max_document_bytes"] = json!(1000);
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let dir = users(temp.path());
    let padding = "x".repeat(2000);
    fs::write(dir.join("wide.json"), format!("{{\"p\": \"{padding}\"}}")).unwrap();
    fs::write(dir.join("ok.json"), "{\"n\": 1}").unwrap();
    let store = Store::open(temp.path()).unwrap();
    assert!(store.get("main", "users", "ok").unwrap().is_some());
    let status = store.status().unwrap();
    assert_eq!(status.invalid.len(), 1);
    assert_eq!(status.invalid[0].id, "wide");
    assert_eq!(status.invalid[0].reason, "too_large");
}

#[cfg(unix)]
#[test]
fn skipped_entries_are_counted_per_collection() {
    let temp = tempfile::tempdir().unwrap();
    json_store(temp.path());
    make_collection(
        temp.path(),
        "main",
        "plans",
        &CollectionMeta::new(Format::Yaml),
    );
    make_collection(
        temp.path(),
        "main",
        "notes",
        &CollectionMeta::new(Format::Md),
    );
    let dir = users(temp.path());
    fs::write(dir.join("ok.json"), "{\"n\": 1}").unwrap();
    std::os::unix::fs::symlink(dir.join("ok.json"), dir.join("l.json")).unwrap();
    fs::write(dir.join("a b.json"), "{}").unwrap();
    fs::write(dir.join("readme.txt"), "x").unwrap();
    fs::create_dir(dir.join("sub")).unwrap();
    let plans = temp.path().join("main/plans");
    fs::write(plans.join("n.yml"), "a: 1\n").unwrap();
    fs::write(plans.join("p.yaml"), "a: 1\n").unwrap();
    fs::write(temp.path().join("main/notes/x.markdown"), "text\n").unwrap();
    let store = Store::open(temp.path()).unwrap();
    let status = store.status().unwrap();
    let mut skipped: Vec<(String, serde_json::Value)> = status
        .warnings
        .iter()
        .filter(|warning| warning.code == "skipped_files")
        .map(|warning| (warning.collection.clone(), warning.details.clone()))
        .collect();
    skipped.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        skipped,
        [
            (
                "notes".to_owned(),
                json!({"count": 1, "names": ["x.markdown"]})
            ),
            ("plans".to_owned(), json!({"count": 1, "names": ["n.yml"]})),
            ("users".to_owned(), json!({"count": 1, "names": ["l.json"]})),
        ]
    );
    let users_warning = status
        .warnings
        .iter()
        .find(|warning| warning.code == "skipped_files" && warning.collection == "users")
        .unwrap();
    assert_eq!(users_warning.path, users(temp.path()));
    assert_eq!(
        status
            .warnings
            .iter()
            .filter(|warning| warning.code == "invalid_name")
            .count(),
        1
    );
}

fn rewrite_keeping_stamp(path: &Path, text: &str) {
    let before = fs::metadata(path).unwrap();
    assert_eq!(before.len(), u64::try_from(text.len()).unwrap());
    fs::write(path, text).unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(before.modified().unwrap())
        .unwrap();
}

#[test]
fn a_same_size_edit_within_the_same_tick_is_seen() {
    let temp = tempfile::tempdir().unwrap();
    let store = json_store(temp.path());
    let path = users(temp.path()).join("bob.json");
    fs::write(&path, "{\"name\": \"Bob\"}").unwrap();
    store.rescan().unwrap();
    assert_eq!(
        store.get("main", "users", "bob").unwrap().unwrap().0["name"],
        json!("Bob")
    );
    rewrite_keeping_stamp(&path, "{\"name\": \"Rob\"}");
    store.rescan().unwrap();
    assert_eq!(
        store.get("main", "users", "bob").unwrap().unwrap().0["name"],
        json!("Rob")
    );
}

#[test]
fn a_same_size_edit_is_seen_through_the_cache() {
    let temp = tempfile::tempdir().unwrap();
    drop(json_store(temp.path()));
    let path = users(temp.path()).join("bob.json");
    fs::write(&path, "{\"name\": \"Bob\"}").unwrap();
    let store = Store::open(temp.path()).unwrap();
    assert_eq!(
        store.get("main", "users", "bob").unwrap().unwrap().0["name"],
        json!("Bob")
    );
    drop(store);
    rewrite_keeping_stamp(&path, "{\"name\": \"Rob\"}");
    let store = Store::open(temp.path()).unwrap();
    assert_eq!(
        store.get("main", "users", "bob").unwrap().unwrap().0["name"],
        json!("Rob")
    );
}

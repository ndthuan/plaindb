use std::fs;
use std::path::{Path, PathBuf};

use plaindb_core::{Doc, ErrorCode, Format, Op, Store, Tx};
use plaindb_core::{Durability, IndexSpec, StoreConfig};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn doc(value: Value) -> Doc {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn random_bytes(len: usize) -> Vec<u8> {
    let mut rng = fastrand::Rng::with_seed(7);
    (0..len).map(|_| rng.u8(..)).collect()
}

fn insert(coll: &str, id: &str, value: Value) -> Op {
    Op::Insert {
        db: "main".to_owned(),
        coll: coll.to_owned(),
        id: Some(id.to_owned()),
        doc: doc(value),
    }
}

fn attach(coll: &str, id: &str, name: &str, bytes: &[u8]) -> Op {
    Op::AttachFile {
        db: "main".to_owned(),
        coll: coll.to_owned(),
        id: id.to_owned(),
        name: name.to_owned(),
        bytes: bytes.to_vec(),
        mime: None,
        if_version: None,
    }
}

fn detach(coll: &str, id: &str, name: &str) -> Op {
    Op::DetachFile {
        db: "main".to_owned(),
        coll: coll.to_owned(),
        id: id.to_owned(),
        name: name.to_owned(),
        if_version: None,
    }
}

fn store_with_note(root: &Path) -> Store {
    let store = Store::init_with(
        root,
        StoreConfig {
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "notes", Format::Json)
        .unwrap();
    store
        .apply(Tx::new(vec![insert(
            "notes",
            "n1",
            json!({ "title": "one" }),
        )]))
        .unwrap();
    store
}

fn file_path(root: &Path, id: &str, name: &str) -> PathBuf {
    root.join("main")
        .join("notes")
        .join("_files")
        .join(id)
        .join(name)
}

fn files_field(store: &Store, id: &str) -> Option<Value> {
    let (found, _) = store.get("main", "notes", id).unwrap().unwrap();
    found.get("_files").cloned()
}

#[test]
fn an_attached_file_is_stored_beside_its_document_and_read_back() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    let (_, before) = store.get("main", "notes", "n1").unwrap().unwrap();
    let bytes = random_bytes(1024);
    let result = store
        .apply(Tx::new(vec![attach("notes", "n1", "a.png", &bytes)]))
        .unwrap();
    let (found, version) = store.get("main", "notes", "n1").unwrap().unwrap();
    assert_ne!(version, before);
    assert_eq!(result.results[0].version.as_deref(), Some(version.as_str()));
    assert_eq!(
        result.results[0].previous_version.as_deref(),
        Some(before.as_str())
    );
    assert_eq!(
        found["_files"],
        json!({ "a.png": { "size": 1024, "sha256": sha256_hex(&bytes), "mime": "image/png" } })
    );
    assert_eq!(
        fs::read(file_path(temp.path(), "n1", "a.png")).unwrap(),
        bytes
    );
    let (meta, read) = store.read_file("main", "notes", "n1", "a.png").unwrap();
    assert_eq!(read, bytes);
    assert_eq!(meta.size, 1024);
    assert_eq!(meta.sha256, sha256_hex(&bytes));
    assert_eq!(meta.mime, "image/png");
    drop(store);
    let reopened = Store::open(temp.path()).unwrap();
    assert_eq!(
        reopened
            .read_file("main", "notes", "n1", "a.png")
            .unwrap()
            .1,
        bytes
    );
}

#[test]
fn the_mime_type_is_given_guessed_from_the_extension_or_left_generic() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    store
        .apply(Tx::new(vec![
            Op::AttachFile {
                db: "main".to_owned(),
                coll: "notes".to_owned(),
                id: "n1".to_owned(),
                name: "song.bin".to_owned(),
                bytes: b"x".to_vec(),
                mime: Some("audio/flac".to_owned()),
                if_version: None,
            },
            attach("notes", "n1", "paper.PDF", b"%PDF"),
            attach("notes", "n1", "blob.xyz", b"?"),
            attach("notes", "n1", "noextension", b"?"),
        ]))
        .unwrap();
    let files = files_field(&store, "n1").unwrap();
    assert_eq!(files["song.bin"]["mime"], json!("audio/flac"));
    assert_eq!(files["paper.PDF"]["mime"], json!("application/pdf"));
    assert_eq!(files["blob.xyz"]["mime"], json!("application/octet-stream"));
    assert_eq!(
        files["noextension"]["mime"],
        json!("application/octet-stream")
    );
}

#[test]
fn attaching_again_under_the_same_name_replaces_the_file() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    store
        .apply(Tx::new(vec![attach("notes", "n1", "a.txt", b"first")]))
        .unwrap();
    store
        .apply(Tx::new(vec![attach("notes", "n1", "a.txt", b"second one")]))
        .unwrap();
    let (meta, bytes) = store.read_file("main", "notes", "n1", "a.txt").unwrap();
    assert_eq!(bytes, b"second one");
    assert_eq!(meta.size, 10);
    assert_eq!(meta.mime, "text/plain");
}

#[test]
fn a_stale_if_version_writes_no_file() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    let (_, version) = store.get("main", "notes", "n1").unwrap().unwrap();
    store
        .apply(Tx::new(vec![Op::Patch {
            db: "main".to_owned(),
            coll: "notes".to_owned(),
            id: "n1".to_owned(),
            patch: json!({ "title": "changed" }),
            if_version: None,
        }]))
        .unwrap();
    let error = store
        .apply(Tx::new(vec![Op::AttachFile {
            db: "main".to_owned(),
            coll: "notes".to_owned(),
            id: "n1".to_owned(),
            name: "a.png".to_owned(),
            bytes: b"png".to_vec(),
            mime: None,
            if_version: Some(version),
        }]))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::VersionConflict);
    assert!(!file_path(temp.path(), "n1", "a.png").exists());
    assert_eq!(files_field(&store, "n1"), None);
}

#[test]
fn a_failing_transaction_attaches_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    store
        .set_indexes(
            "main",
            "notes",
            vec![IndexSpec::Field {
                field: "title".to_owned(),
                unique: true,
                multi: false,
            }],
        )
        .unwrap();
    let error = store
        .apply(Tx::new(vec![
            attach("notes", "n1", "a.png", b"png"),
            insert("notes", "n2", json!({ "title": "one" })),
        ]))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UniqueViolation);
    assert!(!file_path(temp.path(), "n1", "a.png").exists());
    assert!(!temp.path().join("main/notes/_files").exists());
    assert_eq!(files_field(&store, "n1"), None);
}

#[test]
fn attaching_to_a_missing_document_is_not_found() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    let error = store
        .apply(Tx::new(vec![attach("notes", "nobody", "a.png", b"png")]))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(!temp.path().join("main/notes/_files").exists());
}

#[test]
fn a_file_over_the_limit_is_refused_until_the_limit_is_raised() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    assert_eq!(store.config().limits.max_file_bytes, Some(10 * 1024 * 1024));
    let big = vec![0_u8; 10 * 1024 * 1024 + 1];
    let error = store
        .apply(Tx::new(vec![attach("notes", "n1", "big.bin", &big)]))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::LimitExceeded);
    assert_eq!(error.details["limit"], json!("max_file_bytes"));
    assert_eq!(error.details["op_index"], json!(0));
    assert!(!file_path(temp.path(), "n1", "big.bin").exists());
    store
        .set_setting("limits.max_file_bytes", "20000000")
        .unwrap();
    store
        .apply(Tx::new(vec![attach("notes", "n1", "big.bin", &big)]))
        .unwrap();
    assert_eq!(
        fs::metadata(file_path(temp.path(), "n1", "big.bin"))
            .unwrap()
            .len(),
        10 * 1024 * 1024 + 1
    );
    for value in ["-1", "0", "lots"] {
        let error = store
            .set_setting("limits.max_file_bytes", value)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfig, "{value}");
    }
}

#[test]
fn file_names_follow_the_naming_rules() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    let long = "a".repeat(129);
    for name in [
        "", ".x", "-x", "_x", "a/b", "..", "a..b", "a b", "é.png", &long,
    ] {
        let error = store
            .apply(Tx::new(vec![attach("notes", "n1", name, b"x")]))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidName, "{name:?}");
    }
    store
        .apply(Tx::new(vec![attach("notes", "n1", &"a".repeat(128), b"x")]))
        .unwrap();
    store
        .apply(Tx::new(vec![attach("notes", "n1", "a.png", b"x")]))
        .unwrap();
    let error = store
        .apply(Tx::new(vec![attach("notes", "n1", "A.png", b"y")]))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidName);
    assert_eq!(error.details["existing"], json!("a.png"));
    assert_eq!(
        fs::read(file_path(temp.path(), "n1", "a.png")).unwrap(),
        b"x"
    );
    assert_eq!(
        files_field(&store, "n1")
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn the_files_field_cannot_be_written_by_document_writes() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    store
        .apply(Tx::new(vec![attach("notes", "n1", "a.png", b"png")]))
        .unwrap();
    let kept = files_field(&store, "n1").unwrap();
    let forged = json!({ "a.png": { "size": 1, "sha256": "00", "mime": "x/y" } });
    let attempts = vec![
        insert("notes", "n2", json!({ "_files": forged })),
        Op::Put {
            db: "main".to_owned(),
            coll: "notes".to_owned(),
            id: "n1".to_owned(),
            doc: doc(json!({ "title": "x", "_files": forged })),
            if_version: None,
        },
        Op::Patch {
            db: "main".to_owned(),
            coll: "notes".to_owned(),
            id: "n1".to_owned(),
            patch: json!({ "_files": null }),
            if_version: None,
        },
        Op::Patch {
            db: "main".to_owned(),
            coll: "notes".to_owned(),
            id: "n1".to_owned(),
            patch: json!({ "_files": { "b.png": forged["a.png"] } }),
            if_version: None,
        },
    ];
    for op in attempts {
        let error = store.apply(Tx::new(vec![op.clone()])).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidDocument, "{op:?}");
        assert_eq!(error.details["field"], json!("_files"), "{op:?}");
    }
    store
        .apply(Tx::new(vec![Op::Put {
            db: "main".to_owned(),
            coll: "notes".to_owned(),
            id: "n1".to_owned(),
            doc: doc(json!({ "title": "replaced" })),
            if_version: None,
        }]))
        .unwrap();
    assert_eq!(files_field(&store, "n1"), Some(kept.clone()));
    store
        .apply(Tx::new(vec![Op::Patch {
            db: "main".to_owned(),
            coll: "notes".to_owned(),
            id: "n1".to_owned(),
            patch: json!({ "title": "patched" }),
            if_version: None,
        }]))
        .unwrap();
    assert_eq!(files_field(&store, "n1"), Some(kept.clone()));
    let (found, _) = store.get("main", "notes", "n1").unwrap().unwrap();
    let mut copy = found.clone();
    copy.insert("title".to_owned(), json!("read and written back"));
    store
        .apply(Tx::new(vec![Op::Put {
            db: "main".to_owned(),
            coll: "notes".to_owned(),
            id: "n1".to_owned(),
            doc: copy,
            if_version: None,
        }]))
        .unwrap();
    assert_eq!(files_field(&store, "n1"), Some(kept));
    assert!(store.get("main", "notes", "n2").unwrap().is_none());
}

#[test]
fn importing_a_files_field_drops_it() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    let line = json!({ "_id": "n9", "_files": { "a.png": { "size": 1, "sha256": "00", "mime": "image/png" } } });
    let report = store
        .import_jsonl(
            "main",
            "notes",
            format!("{line}\n").as_bytes(),
            &plaindb_core::transfer::ImportOptions::default(),
        )
        .unwrap();
    assert!(report.failed.is_empty());
    assert_eq!(report.files_dropped, vec!["n9".to_owned()]);
    assert_eq!(files_field(&store, "n9"), None);
}

#[test]
fn detaching_removes_the_file_and_its_entry() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    store
        .apply(Tx::new(vec![
            attach("notes", "n1", "a.png", b"png"),
            attach("notes", "n1", "b.pdf", b"pdf"),
        ]))
        .unwrap();
    let (_, before) = store.get("main", "notes", "n1").unwrap().unwrap();
    let result = store
        .apply(Tx::new(vec![detach("notes", "n1", "a.png")]))
        .unwrap();
    assert_ne!(result.results[0].version.as_deref(), Some(before.as_str()));
    assert!(!file_path(temp.path(), "n1", "a.png").exists());
    let files = files_field(&store, "n1").unwrap();
    assert!(files.get("a.png").is_none());
    assert!(files.get("b.pdf").is_some());
    assert_eq!(
        store
            .read_file("main", "notes", "n1", "a.png")
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    store
        .apply(Tx::new(vec![detach("notes", "n1", "b.pdf")]))
        .unwrap();
    assert_eq!(files_field(&store, "n1"), None);
    let error = store
        .apply(Tx::new(vec![detach("notes", "n1", "b.pdf")]))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[test]
fn deleting_a_document_removes_its_files() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    store
        .apply(Tx::new(vec![
            insert("notes", "n2", json!({})),
            attach("notes", "n1", "a.png", b"png"),
            attach("notes", "n2", "b.png", b"png"),
        ]))
        .unwrap();
    store
        .apply(Tx::new(vec![Op::Delete {
            db: "main".to_owned(),
            coll: "notes".to_owned(),
            id: "n1".to_owned(),
            if_version: None,
        }]))
        .unwrap();
    assert!(!temp.path().join("main/notes/_files/n1").exists());
    assert!(file_path(temp.path(), "n2", "b.png").exists());
    drop(store);
    let store = Store::open(temp.path()).unwrap();
    assert!(!temp.path().join("main/notes/_files/n1").exists());
    store.drop_collection("main", "notes").unwrap();
    assert!(!temp.path().join("main/notes").exists());
}

#[test]
fn a_document_deleted_and_made_again_in_one_transaction_keeps_only_new_files() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    store
        .apply(Tx::new(vec![
            attach("notes", "n1", "old.png", b"old"),
            attach("notes", "n1", "kept.png", b"old"),
        ]))
        .unwrap();
    store
        .apply(Tx::new(vec![
            Op::Delete {
                db: "main".to_owned(),
                coll: "notes".to_owned(),
                id: "n1".to_owned(),
                if_version: None,
            },
            insert("notes", "n1", json!({})),
            attach("notes", "n1", "kept.png", b"new"),
        ]))
        .unwrap();
    assert!(!file_path(temp.path(), "n1", "old.png").exists());
    assert_eq!(
        fs::read(file_path(temp.path(), "n1", "kept.png")).unwrap(),
        b"new"
    );
    let files = files_field(&store, "n1").unwrap();
    assert_eq!(files.as_object().unwrap().len(), 1);
    assert_eq!(files["kept.png"]["sha256"], json!(sha256_hex(b"new")));
}

#[test]
fn a_hand_edited_file_is_not_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    store
        .apply(Tx::new(vec![attach("notes", "n1", "a.png", b"png")]))
        .unwrap();
    let path = file_path(temp.path(), "n1", "a.png");
    fs::write(&path, b"edited by hand").unwrap();
    for op in [
        attach("notes", "n1", "a.png", b"new"),
        detach("notes", "n1", "a.png"),
    ] {
        let error = store.apply(Tx::new(vec![op])).unwrap_err();
        assert_eq!(error.code, ErrorCode::VersionConflict);
        assert_eq!(error.details["reason"], json!("changed_on_disk"));
        assert_eq!(fs::read(&path).unwrap(), b"edited by hand");
    }
    let error = store.read_file("main", "notes", "n1", "a.png").unwrap_err();
    assert_eq!(error.code, ErrorCode::VersionConflict);
    assert_eq!(error.details["reason"], json!("changed_on_disk"));
}

#[test]
fn a_file_placed_by_hand_is_not_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    let path = file_path(temp.path(), "n1", "a.png");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"placed by hand").unwrap();
    let error = store
        .apply(Tx::new(vec![attach("notes", "n1", "a.png", b"new")]))
        .unwrap_err();
    assert_eq!(error.details["reason"], json!("changed_on_disk"));
    assert_eq!(fs::read(&path).unwrap(), b"placed by hand");
}

#[test]
fn a_missing_file_can_be_attached_again_or_detached() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    store
        .apply(Tx::new(vec![
            attach("notes", "n1", "a.png", b"png"),
            attach("notes", "n1", "b.png", b"png"),
        ]))
        .unwrap();
    fs::remove_file(file_path(temp.path(), "n1", "a.png")).unwrap();
    fs::remove_file(file_path(temp.path(), "n1", "b.png")).unwrap();
    assert_eq!(
        store
            .read_file("main", "notes", "n1", "a.png")
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    store
        .apply(Tx::new(vec![
            attach("notes", "n1", "a.png", b"again"),
            detach("notes", "n1", "b.png"),
        ]))
        .unwrap();
    assert_eq!(
        store.read_file("main", "notes", "n1", "a.png").unwrap().1,
        b"again"
    );
    assert!(files_field(&store, "n1").unwrap().get("b.png").is_none());
}

#[test]
fn the_files_directory_is_never_a_document() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    store
        .apply(Tx::new(vec![attach("notes", "n1", "doc.json", b"{}")]))
        .unwrap();
    store.rescan().unwrap();
    assert_eq!(store.list_ids("main", "notes").unwrap(), vec!["n1"]);
    drop(store);
    fs::remove_file(temp.path().join(".plaindb/index-cache.bin")).ok();
    let store = Store::open(temp.path()).unwrap();
    assert_eq!(store.list_ids("main", "notes").unwrap(), vec!["n1"]);
    let status = store.status().unwrap();
    assert_eq!(status.documents, 1);
    assert!(status.warnings.is_empty(), "{:?}", status.warnings);
    assert!(status.invalid.is_empty());
}

#[test]
fn a_markdown_document_carries_its_files_in_the_front_matter() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    store
        .create_collection("main", "pages", Format::Md)
        .unwrap();
    store
        .apply(Tx::new(vec![
            insert("pages", "p1", json!({ "body": "# Title\n" })),
            attach("pages", "p1", "photo.jpg", b"jpeg"),
        ]))
        .unwrap();
    let (found, _) = store.get("main", "pages", "p1").unwrap().unwrap();
    assert_eq!(
        found["_files"]["photo.jpg"]["sha256"],
        json!(sha256_hex(b"jpeg"))
    );
    assert_eq!(found["_files"]["photo.jpg"]["mime"], json!("image/jpeg"));
    assert_eq!(
        store
            .read_file("main", "pages", "p1", "photo.jpg")
            .unwrap()
            .1,
        b"jpeg"
    );
}

#[test]
fn large_files_do_not_pile_up_in_the_journal() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    let big = random_bytes(10 * 1024 * 1024);
    for n in 0..7 {
        store
            .apply(Tx::new(vec![attach(
                "notes",
                "n1",
                &format!("part{n}.bin"),
                &big,
            )]))
            .unwrap();
    }
    let journal = fs::metadata(temp.path().join(".plaindb/journal.jsonl"))
        .map_or(0, |metadata| metadata.len());
    assert!(journal < 64 * 1024 * 1024, "{journal}");
}

#[test]
fn a_strict_schema_does_not_see_the_files_field() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_note(temp.path());
    store
        .set_schema(
            "main",
            "notes",
            Some(json!({
                "type": "object",
                "properties": { "title": { "type": "string" } },
                "additionalProperties": false
            })),
        )
        .unwrap();
    store
        .apply(Tx::new(vec![attach("notes", "n1", "a.png", b"png")]))
        .unwrap();
    drop(store);
    fs::remove_file(temp.path().join(".plaindb/index-cache.bin")).ok();
    let store = Store::open(temp.path()).unwrap();
    assert!(store.status().unwrap().invalid.is_empty());
    assert!(files_field(&store, "n1").is_some());
}

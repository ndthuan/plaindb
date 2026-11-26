use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use plaindb_core::{
    Doc, Error, ErrorCode, Format, IndexSpec, Op, Store, Tx, new_id, scan::read_generation,
    version_of,
};
use plaindb_core::{Durability, StoreConfig};
use serde_json::{Value, json};

fn doc(value: Value) -> Doc {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn insert(coll: &str, id: &str, value: Value) -> Op {
    Op::Insert {
        db: "main".to_owned(),
        coll: coll.to_owned(),
        id: Some(id.to_owned()),
        doc: doc(value),
    }
}

fn put(coll: &str, id: &str, value: Value, if_version: Option<&str>) -> Op {
    Op::Put {
        db: "main".to_owned(),
        coll: coll.to_owned(),
        id: id.to_owned(),
        doc: doc(value),
        if_version: if_version.map(str::to_owned),
    }
}

fn patch(coll: &str, id: &str, value: Value, if_version: Option<&str>) -> Op {
    Op::Patch {
        db: "main".to_owned(),
        coll: coll.to_owned(),
        id: id.to_owned(),
        patch: value,
        if_version: if_version.map(str::to_owned),
    }
}

fn delete(coll: &str, id: &str, if_version: Option<&str>) -> Op {
    Op::Delete {
        db: "main".to_owned(),
        coll: coll.to_owned(),
        id: id.to_owned(),
        if_version: if_version.map(str::to_owned),
    }
}

fn store_with_users(root: &Path) -> Store {
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
        .create_collection("main", "users", Format::Json)
        .unwrap();
    store
}

fn one(store: &Store, op: Op) -> plaindb_core::Result<plaindb_core::TxResult> {
    store.apply(Tx::new(vec![op]))
}

fn users(root: &Path) -> PathBuf {
    root.join("main").join("users")
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                if entry.file_name() != ".plaindb" {
                    pending.push(path);
                }
            } else {
                files.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    files
}

fn expect_err(outcome: plaindb_core::Result<plaindb_core::TxResult>) -> Error {
    match outcome {
        Ok(result) => panic!("expected an error, got {result:?}"),
        Err(error) => error,
    }
}

fn is_timestamp(value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    let bytes = text.as_bytes();
    text.len() == 24
        && text.ends_with('Z')
        && bytes[4] == b'-'
        && bytes[10] == b'T'
        && bytes[19] == b'.'
        && text.parse::<jiff::Timestamp>().is_ok()
}

#[test]
fn insert_writes_a_normalised_file_with_system_fields() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    let result = one(&store, insert("users", "ann", json!({"name": "Ann"}))).unwrap();
    let bytes = fs::read(users(temp.path()).join("ann.json")).unwrap();
    assert_eq!(result.results[0].id, "ann");
    assert_eq!(
        result.results[0].version.as_deref(),
        Some(version_of(&bytes).as_str())
    );
    let (found, version) = store.get("main", "users", "ann").unwrap().unwrap();
    assert_eq!(version, version_of(&bytes));
    assert_eq!(found["name"], json!("Ann"));
    assert_eq!(found["_id"], json!("ann"));
    assert!(is_timestamp(&found["_created_at"]));
    assert_eq!(found["_created_at"], found["_updated_at"]);
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.starts_with("{\n  \"_created_at\""));
    assert!(text.ends_with("}\n"));
}

#[test]
fn insert_without_an_id_generates_one() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    let result = one(
        &store,
        Op::Insert {
            db: "main".to_owned(),
            coll: "users".to_owned(),
            id: None,
            doc: doc(json!({"name": "Anon"})),
        },
    )
    .unwrap();
    let id = &result.results[0].id;
    assert_eq!(id.len(), 26);
    assert!(users(temp.path()).join(format!("{id}.json")).is_file());
}

#[test]
fn every_write_bumps_the_generation() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    let before = read_generation(temp.path()).unwrap();
    one(&store, insert("users", "ann", json!({}))).unwrap();
    let second = one(&store, insert("users", "bob", json!({}))).unwrap();
    assert_eq!(second.generation, before + 2);
    assert_eq!(read_generation(temp.path()).unwrap(), before + 2);
    assert_eq!(store.snapshot().generation, before + 2);
    assert!(!temp.path().join(".plaindb").join("journal.json").exists());
}

#[test]
fn put_keeps_the_creation_time_and_replaces_the_fields() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    one(
        &store,
        insert("users", "ann", json!({"name": "Ann", "age": 3})),
    )
    .unwrap();
    let (first, first_version) = store.get("main", "users", "ann").unwrap().unwrap();
    let replaced = one(&store, put("users", "ann", json!({"name": "Anna"}), None)).unwrap();
    assert_eq!(replaced.results[0].previous_version, Some(first_version));
    let (second, _) = store.get("main", "users", "ann").unwrap().unwrap();
    assert_eq!(second["name"], json!("Anna"));
    assert!(second.get("age").is_none());
    assert_eq!(second["_created_at"], first["_created_at"]);
}

#[test]
fn put_creates_a_missing_document() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    one(&store, put("users", "new", json!({"n": 1}), None)).unwrap();
    assert!(store.get("main", "users", "new").unwrap().is_some());
}

#[test]
fn a_patch_null_removes_the_field() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    one(
        &store,
        insert("users", "ann", json!({"a": 1, "b": {"c": 2, "d": 3}})),
    )
    .unwrap();
    one(
        &store,
        patch(
            "users",
            "ann",
            json!({"a": null, "b": {"c": null, "e": 4}}),
            None,
        ),
    )
    .unwrap();
    let (found, _) = store.get("main", "users", "ann").unwrap().unwrap();
    assert!(found.get("a").is_none());
    assert_eq!(found["b"], json!({"d": 3, "e": 4}));
}

#[test]
fn a_patch_that_is_not_an_object_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    one(&store, insert("users", "ann", json!({"a": 1}))).unwrap();
    let error = expect_err(one(&store, patch("users", "ann", json!("text"), None)));
    assert_eq!(error.code, ErrorCode::InvalidDocument);
}

#[test]
fn delete_removes_the_file() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    let result = one(&store, insert("users", "ann", json!({}))).unwrap();
    let version = result.results[0].version.clone();
    one(&store, delete("users", "ann", version.as_deref())).unwrap();
    assert!(!users(temp.path()).join("ann.json").exists());
    assert!(store.get("main", "users", "ann").unwrap().is_none());
}

#[test]
fn insert_on_an_existing_id_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    one(&store, insert("users", "ann", json!({}))).unwrap();
    let error = expect_err(one(&store, insert("users", "ann", json!({}))));
    assert_eq!(error.code, ErrorCode::AlreadyExists);
}

#[test]
fn an_id_differing_only_in_case_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    one(&store, insert("users", "Ann", json!({}))).unwrap();
    for op in [
        insert("users", "ann", json!({})),
        put("users", "ANN", json!({}), None),
    ] {
        let error = expect_err(one(&store, op));
        assert_eq!(error.code, ErrorCode::AlreadyExists);
        assert_eq!(error.details["reason"], json!("case"));
    }
    one(&store, put("users", "Ann", json!({"n": 1}), None)).unwrap();
}

#[test]
fn a_case_clash_inside_one_transaction_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    let error = expect_err(store.apply(Tx::new(vec![
        insert("users", "bob", json!({})),
        insert("users", "Bob", json!({})),
    ])));
    assert_eq!(error.details["reason"], json!("case"));
}

#[test]
fn a_stale_if_version_is_a_conflict() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    let first = one(&store, insert("users", "ann", json!({"n": 1}))).unwrap();
    let stale = first.results[0].version.clone().unwrap();
    one(&store, put("users", "ann", json!({"n": 2}), Some(&stale))).unwrap();
    for op in [
        put("users", "ann", json!({"n": 3}), Some(&stale)),
        patch("users", "ann", json!({"n": 3}), Some(&stale)),
        delete("users", "ann", Some(&stale)),
        put("users", "nobody", json!({}), Some(&stale)),
    ] {
        let error = expect_err(one(&store, op));
        assert_eq!(error.code, ErrorCode::VersionConflict);
    }
    let (found, _) = store.get("main", "users", "ann").unwrap().unwrap();
    assert_eq!(found["n"], json!(2));
}

#[test]
fn patch_and_delete_of_a_missing_document_are_not_found() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    for op in [
        patch("users", "ghost", json!({"a": 1}), None),
        delete("users", "ghost", None),
    ] {
        let error = expect_err(one(&store, op));
        assert_eq!(error.code, ErrorCode::NotFound);
        assert_eq!(error.details["kind"], json!("document"));
    }
}

#[test]
fn a_missing_db_or_collection_is_not_found() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    let missing_db = Op::Insert {
        db: "other".to_owned(),
        coll: "users".to_owned(),
        id: Some("a".to_owned()),
        doc: Doc::new(),
    };
    let error = expect_err(one(&store, missing_db));
    assert_eq!(error.code, ErrorCode::NotFound);
    assert_eq!(error.details["kind"], json!("db"));
    let error = expect_err(one(&store, insert("posts", "a", json!({}))));
    assert_eq!(error.code, ErrorCode::NotFound);
    assert_eq!(error.details["kind"], json!("collection"));
}

#[test]
fn a_malformed_id_is_an_invalid_name() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    for id in ["../escape", "_hidden", "-dash", "a/b", ""] {
        let error = expect_err(one(&store, insert("users", id, json!({}))));
        assert_eq!(error.code, ErrorCode::InvalidName, "{id}");
    }
}

#[test]
fn a_document_marked_invalid_refuses_writes() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    fs::write(users(temp.path()).join("broken.json"), "{ not json").unwrap();
    store.rescan().unwrap();
    for op in [
        insert("users", "broken", json!({})),
        put("users", "broken", json!({}), None),
        patch("users", "broken", json!({}), None),
        delete("users", "broken", None),
    ] {
        let error = expect_err(one(&store, op));
        assert_eq!(error.code, ErrorCode::InvalidDocument);
    }
    assert_eq!(
        fs::read_to_string(users(temp.path()).join("broken.json")).unwrap(),
        "{ not json"
    );
}

#[test]
fn a_document_failing_the_schema_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    store
        .set_schema(
            "main",
            "users",
            Some(json!({
                "type": "object",
                "required": ["name"],
                "properties": {"name": {"type": "string"}},
                "additionalProperties": false
            })),
        )
        .unwrap();
    one(&store, insert("users", "ann", json!({"name": "Ann"}))).unwrap();
    let error = expect_err(one(&store, insert("users", "bob", json!({"name": 7}))));
    assert_eq!(error.code, ErrorCode::SchemaViolation);
    assert!(!error.details["errors"].as_array().unwrap().is_empty());
    let error = expect_err(one(
        &store,
        patch("users", "ann", json!({"extra": 1}), None),
    ));
    assert_eq!(error.code, ErrorCode::SchemaViolation);
}

#[test]
fn setting_a_schema_checks_existing_documents_first() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    one(&store, insert("users", "ann", json!({"name": "Ann"}))).unwrap();
    one(&store, insert("users", "bob", json!({"name": 7}))).unwrap();
    let error = store
        .set_schema(
            "main",
            "users",
            Some(json!({"properties": {"name": {"type": "string"}}})),
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::SchemaViolation);
    assert_eq!(error.details["ids"], json!(["bob"]));
    assert_eq!(store.collection_meta("main", "users").unwrap().schema, None);
}

#[test]
fn a_document_over_the_size_limit_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    store_with_users(temp.path());
    let config_path = temp.path().join("_store.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["limits"]["max_document_bytes"] = json!(64);
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let store = Store::open(temp.path()).unwrap();
    let error = expect_err(one(
        &store,
        insert("users", "big", json!({"text": "x".repeat(100)})),
    ));
    assert_eq!(error.code, ErrorCode::LimitExceeded);
    assert!(!users(temp.path()).join("big.json").exists());
}

#[test]
fn a_failing_op_leaves_every_file_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    one(&store, insert("users", "ann", json!({"n": 1}))).unwrap();
    let before = tree(temp.path());
    let generation = read_generation(temp.path()).unwrap();
    let error = expect_err(store.apply(Tx::new(vec![
        insert("users", "bob", json!({})),
        put("users", "ann", json!({"n": 2}), None),
        patch("users", "ghost", json!({}), None),
    ])));
    assert_eq!(error.code, ErrorCode::NotFound);
    assert_eq!(error.details["op_index"], json!(2));
    assert_eq!(tree(temp.path()), before);
    assert_eq!(read_generation(temp.path()).unwrap(), generation);
    assert!(store.get("main", "users", "bob").unwrap().is_none());
}

#[test]
fn ops_in_one_transaction_see_each_other() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    let result = store
        .apply(Tx::new(vec![
            insert("users", "ann", json!({"n": 1})),
            patch("users", "ann", json!({"m": 2}), None),
            insert("users", "bob", json!({})),
            delete("users", "bob", None),
        ]))
        .unwrap();
    let (found, version) = store.get("main", "users", "ann").unwrap().unwrap();
    assert_eq!(found["n"], json!(1));
    assert_eq!(found["m"], json!(2));
    assert_eq!(result.results[1].version.as_deref(), Some(version.as_str()));
    assert!(!users(temp.path()).join("bob.json").exists());
}

#[test]
fn a_hand_edit_after_open_is_never_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    one(&store, insert("users", "ann", json!({"n": 1}))).unwrap();
    let path = users(temp.path()).join("ann.json");
    fs::write(&path, "{\"n\": 99}\n").unwrap();
    let error = expect_err(one(&store, put("users", "ann", json!({"n": 2}), None)));
    assert_eq!(error.code, ErrorCode::VersionConflict);
    assert_eq!(error.details["reason"], json!("changed_on_disk"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"n\": 99}\n");
    let (found, version) = store.get("main", "users", "ann").unwrap().unwrap();
    assert_eq!(found["n"], json!(99));
    assert_eq!(version, version_of(b"{\"n\": 99}\n"));
    one(&store, put("users", "ann", json!({"n": 2}), Some(&version))).unwrap();
}

#[test]
fn a_file_created_by_hand_after_open_is_not_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    let path = users(temp.path()).join("cat.json");
    fs::write(&path, "{\"hand\": true}\n").unwrap();
    let error = expect_err(one(&store, put("users", "cat", json!({}), None)));
    assert_eq!(error.details["reason"], json!("changed_on_disk"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"hand\": true}\n");
}

#[test]
fn a_repeated_transaction_id_returns_the_first_result() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    let tx_id = new_id();
    let first = store
        .apply(Tx {
            id: Some(tx_id.clone()),
            ..Tx::new(vec![insert("users", "ann", json!({"n": 1}))])
        })
        .unwrap();
    assert_eq!(first.tx_id, tx_id);
    let generation = read_generation(temp.path()).unwrap();
    let again = store
        .apply(Tx {
            id: Some(tx_id.clone()),
            ..Tx::new(vec![put("users", "ann", json!({"n": 2}), None)])
        })
        .unwrap();
    assert_eq!(again, first);
    assert_eq!(read_generation(temp.path()).unwrap(), generation);
    let (found, _) = store.get("main", "users", "ann").unwrap().unwrap();
    assert_eq!(found["n"], json!(1));
    let reopened = Store::open(temp.path()).unwrap();
    let third = reopened
        .apply(Tx {
            id: Some(tx_id),
            ..Tx::new(vec![delete("users", "ann", None)])
        })
        .unwrap();
    assert_eq!(third, first);
}

#[test]
fn a_transaction_id_used_by_another_handle_is_still_recognised() {
    let temp = tempfile::tempdir().unwrap();
    let first_handle = store_with_users(temp.path());
    let second_handle = Store::open(temp.path()).unwrap();
    let seen = new_id();
    second_handle
        .apply(Tx {
            id: Some(new_id()),
            ..Tx::new(vec![insert("users", "early", json!({}))])
        })
        .unwrap();
    let first = first_handle
        .apply(Tx {
            id: Some(seen.clone()),
            ..Tx::new(vec![insert("users", "ann", json!({"n": 1}))])
        })
        .unwrap();
    let again = second_handle
        .apply(Tx {
            id: Some(seen),
            ..Tx::new(vec![put("users", "ann", json!({"n": 2}), None)])
        })
        .unwrap();
    assert_eq!(again, first);
    let (found, _) = second_handle.get("main", "users", "ann").unwrap().unwrap();
    assert_eq!(found["n"], json!(1));
}

#[test]
fn a_transaction_id_must_be_a_ulid() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    let error = expect_err(store.apply(Tx {
        id: Some("not a ulid".to_owned()),
        ..Tx::new(vec![insert("users", "ann", json!({}))])
    }));
    assert_eq!(error.code, ErrorCode::InvalidName);
}

#[test]
fn the_transaction_log_keeps_the_last_thousand() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    let ids: Vec<String> = (0..1_100).map(|_| new_id()).collect();
    let mut log = String::new();
    for id in &ids {
        let record = json!({
            "tx_id": id,
            "result": {"tx_id": id, "results": [{"id": "seeded", "version": null}]}
        });
        log.push_str(&record.to_string());
        log.push('\n');
    }
    let log_path = temp.path().join(".plaindb").join("tx-log.jsonl");
    fs::write(&log_path, log).unwrap();
    let replay = |id: &str| {
        store
            .apply(Tx {
                id: Some(id.to_owned()),
                ..Tx::new(vec![put("users", "fresh", json!({}), None)])
            })
            .unwrap()
    };
    assert_eq!(replay(&ids[0]).results[0].id, "seeded");
    one(&store, insert("users", "ann", json!({}))).unwrap();
    let lines = fs::read_to_string(&log_path).unwrap().lines().count();
    assert_eq!(lines, 1_000);
    assert_eq!(replay(&ids[0]).results[0].id, "fresh");
    assert_eq!(replay(&ids[1_099]).results[0].id, "seeded");
}

#[test]
fn imported_timestamps_are_kept_when_asked() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    let stamped = json!({
        "_created_at": "2020-01-02T03:04:05.006Z",
        "_updated_at": "2021-01-02T03:04:05.006Z",
        "n": 1
    });
    store
        .apply(Tx {
            keep_timestamps: true,
            ..Tx::new(vec![insert("users", "old", stamped.clone())])
        })
        .unwrap();
    one(&store, insert("users", "fresh", stamped)).unwrap();
    let (old, _) = store.get("main", "users", "old").unwrap().unwrap();
    assert_eq!(old["_created_at"], json!("2020-01-02T03:04:05.006Z"));
    assert_eq!(old["_updated_at"], json!("2021-01-02T03:04:05.006Z"));
    let (fresh, _) = store.get("main", "users", "fresh").unwrap().unwrap();
    assert_ne!(fresh["_created_at"], json!("2020-01-02T03:04:05.006Z"));
}

#[test]
fn markdown_and_yaml_documents_round_trip_through_writes() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    store
        .create_collection("main", "notes", Format::Md)
        .unwrap();
    store
        .create_collection("main", "cards", Format::Yaml)
        .unwrap();
    let body = "# Title\r\n\r\nLine with trailing spaces   \n";
    one(
        &store,
        insert("notes", "n1", json!({"tag": "x", "body": body})),
    )
    .unwrap();
    one(&store, insert("cards", "c1", json!({"z": 1, "a": [1, 2]}))).unwrap();
    let (note, version) = store.get("main", "notes", "n1").unwrap().unwrap();
    assert_eq!(note["body"], json!(body));
    let bytes = fs::read(temp.path().join("main/notes/n1.md")).unwrap();
    assert_eq!(version, version_of(&bytes));
    assert!(bytes.ends_with(body.as_bytes()));
    let reopened = Store::open(temp.path()).unwrap();
    assert_eq!(
        reopened.get("main", "notes", "n1").unwrap().unwrap().0,
        note
    );
    let card = fs::read_to_string(temp.path().join("main/cards/c1.yaml")).unwrap();
    assert!(card.find("_created_at").unwrap() < card.find("z:").unwrap());
}

#[test]
fn admin_writes_create_and_drop_dbs_and_collections() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init_with(
        temp.path(),
        StoreConfig {
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    let error = store
        .create_collection("main", "users", Format::Json)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    store.create_db("main").unwrap();
    assert_eq!(
        store.create_db("main").unwrap_err().code,
        ErrorCode::AlreadyExists
    );
    store
        .create_collection("main", "users", Format::Json)
        .unwrap();
    assert_eq!(
        store
            .create_collection("main", "users", Format::Yaml)
            .unwrap_err()
            .code,
        ErrorCode::AlreadyExists
    );
    assert_eq!(store.list_dbs().unwrap(), vec!["main"]);
    assert_eq!(store.list_collections("main").unwrap(), vec!["users"]);
    one(&store, insert("users", "ann", json!({}))).unwrap();
    store.drop_collection("main", "users").unwrap();
    assert!(!users(temp.path()).exists());
    assert_eq!(
        store.list_ids("main", "users").unwrap_err().code,
        ErrorCode::NotFound
    );
    store.drop_db("main").unwrap();
    assert!(!temp.path().join("main").exists());
    assert_eq!(store.drop_db("main").unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(read_generation(temp.path()).unwrap(), 5);
}

#[test]
fn admin_writes_reject_bad_names() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init_with(
        temp.path(),
        StoreConfig {
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    assert_eq!(
        store.create_db("Main").unwrap_err().code,
        ErrorCode::InvalidName
    );
    assert_eq!(
        store.create_db("../up").unwrap_err().code,
        ErrorCode::InvalidName
    );
    store.create_db("main").unwrap();
    assert_eq!(
        store
            .create_collection("main", "_meta", Format::Json)
            .unwrap_err()
            .code,
        ErrorCode::InvalidName
    );
}

#[test]
fn indexes_are_stored_in_the_collection_meta() {
    let temp = tempfile::tempdir().unwrap();
    let store = store_with_users(temp.path());
    let indexes = vec![
        IndexSpec::Field {
            field: "email".to_owned(),
            unique: true,
            multi: false,
        },
        IndexSpec::Text {
            text: vec!["bio".to_owned()],
        },
    ];
    store.set_indexes("main", "users", indexes.clone()).unwrap();
    assert_eq!(
        store.collection_meta("main", "users").unwrap().indexes,
        indexes
    );
    let reopened = Store::open(temp.path()).unwrap();
    assert_eq!(
        reopened.collection_meta("main", "users").unwrap().indexes,
        indexes
    );
}

#[test]
fn a_second_store_in_the_same_process_sees_the_writes() {
    let temp = tempfile::tempdir().unwrap();
    let first = store_with_users(temp.path());
    let second = Store::open(temp.path()).unwrap();
    one(&first, insert("users", "ann", json!({"n": 1}))).unwrap();
    let (found, version) = second.get("main", "users", "ann").unwrap().unwrap();
    assert_eq!(found["n"], json!(1));
    one(
        &second,
        put("users", "ann", json!({"n": 2}), Some(&version)),
    )
    .unwrap();
    assert_eq!(
        first.get("main", "users", "ann").unwrap().unwrap().0["n"],
        json!(2)
    );
}

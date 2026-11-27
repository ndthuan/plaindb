use std::fs;
use std::path::Path;

use plaindb_core::{
    CollectionMeta, Durability, ErrorCode, Format, Store, StoreConfig, new_id,
    validate_collection_name, validate_db_name, validate_id,
};
use serde_json::{Value, json};

fn make_collection(root: &Path, db: &str, collection: &str, format: Format) {
    let dir = root.join(db).join(collection);
    fs::create_dir_all(&dir).unwrap();
    fs::write(root.join(db).join("_db.json"), "{\"format\":1}\n").unwrap();
    fs::write(
        dir.join("_meta.json"),
        serde_json::to_vec(&CollectionMeta::new(format)).unwrap(),
    )
    .unwrap();
}

#[test]
fn error_codes_are_pinned() {
    let pairs = [
        (ErrorCode::NotFound, "not_found"),
        (ErrorCode::AlreadyExists, "already_exists"),
        (ErrorCode::VersionConflict, "version_conflict"),
        (ErrorCode::InvalidName, "invalid_name"),
        (ErrorCode::InvalidDocument, "invalid_document"),
        (ErrorCode::InvalidConfig, "invalid_config"),
        (ErrorCode::SchemaViolation, "schema_violation"),
        (ErrorCode::UniqueViolation, "unique_violation"),
        (ErrorCode::InvalidQuery, "invalid_query"),
        (ErrorCode::StoreLockedTimeout, "store_locked_timeout"),
        (ErrorCode::NotAStore, "not_a_store"),
        (ErrorCode::UnsupportedFormat, "unsupported_format"),
        (ErrorCode::LimitExceeded, "limit_exceeded"),
        (ErrorCode::GitDisabled, "git_disabled"),
        (ErrorCode::GitNotFound, "git_not_found"),
        (ErrorCode::RemoteAhead, "remote_ahead"),
        (ErrorCode::Io, "io"),
    ];
    for (code, text) in pairs {
        assert_eq!(code.as_str(), text);
    }
}

#[test]
fn init_in_an_empty_directory_writes_the_layout() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("store");
    Store::init(&dir).unwrap();
    assert!(dir.join(".plaindb").is_dir());
    let text = fs::read_to_string(dir.join("_store.json")).unwrap();
    assert!(text.ends_with("}\n"));
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["format"], json!(1));
    assert_eq!(value["git"], json!(false));
    assert_eq!(value["commit"], json!("each"));
    assert_eq!(value["push"], json!("off"));
    assert_eq!(value["remote"], json!("origin"));
    assert_eq!(value["branch"], json!("main"));
    assert_eq!(value["durability"], json!("full"));
    assert_eq!(value["limits"]["max_results"], Value::Null);
    let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

#[test]
fn init_twice_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    Store::init(temp.path()).unwrap();
    let error = Store::init(temp.path()).unwrap_err();
    assert_eq!(error.code, ErrorCode::AlreadyExists);
}

#[test]
fn init_refuses_a_non_empty_directory() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("notes.txt"), "x").unwrap();
    let error = Store::init(temp.path()).unwrap_err();
    assert_eq!(error.code, ErrorCode::AlreadyExists);
    assert_eq!(error.details["reason"], json!("not_empty"));
}

#[test]
fn open_without_a_store_file_fails() {
    let temp = tempfile::tempdir().unwrap();
    let error = Store::open(temp.path()).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotAStore);
}

#[test]
fn open_refuses_a_newer_format() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("_store.json"), "{\"format\": 2}\n").unwrap();
    let error = Store::open(temp.path()).unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedFormat);
}

#[test]
fn open_reads_what_init_wrote() {
    let temp = tempfile::tempdir().unwrap();
    let created = Store::init(temp.path()).unwrap();
    let opened = Store::open(temp.path()).unwrap();
    assert_eq!(created.config(), opened.config());
}

#[test]
fn durability_round_trips_through_the_store_file() {
    for durability in [Durability::Full, Durability::Normal, Durability::Off] {
        let temp = tempfile::tempdir().unwrap();
        Store::init_with(
            temp.path(),
            StoreConfig {
                durability,
                ..StoreConfig::default()
            },
        )
        .unwrap();
        let value: Value =
            serde_json::from_slice(&fs::read(temp.path().join("_store.json")).unwrap()).unwrap();
        assert_eq!(value["durability"], json!(durability.as_str()));
        let opened = Store::open(temp.path()).unwrap();
        assert_eq!(opened.config().durability, durability);
    }
}

#[test]
fn a_store_file_without_durability_reads_as_full() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("_store.json"), "{\"format\": 1}\n").unwrap();
    let opened = Store::open(temp.path()).unwrap();
    assert_eq!(opened.config().durability, Durability::Full);
}

#[test]
fn an_unknown_durability_fails_open() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("_store.json"),
        "{\"format\": 1, \"durability\": \"fast\"}\n",
    )
    .unwrap();
    let error = Store::open(temp.path()).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotAStore);
    assert_eq!(error.details["reason"], json!("unreadable_store_file"));
}

#[test]
fn set_durability_is_kept_in_the_store_file() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    store.set_durability(Durability::Normal).unwrap();
    store.set_durability(Durability::Normal).unwrap();
    let opened = Store::open(temp.path()).unwrap();
    assert_eq!(opened.config().durability, Durability::Normal);
}

#[test]
fn init_with_refuses_a_format_it_cannot_open() {
    let temp = tempfile::tempdir().unwrap();
    let error = Store::init_with(
        temp.path(),
        StoreConfig {
            format: 2,
            ..StoreConfig::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedFormat);
    assert!(!temp.path().join("_store.json").exists());
}

#[test]
fn ids_follow_the_rules() {
    let long_ok = "a".repeat(128);
    for id in ["a", "A-b_9", long_ok.as_str()] {
        assert!(validate_id(id).is_ok(), "{id}");
    }
    let too_long = "a".repeat(129);
    for id in [
        "",
        "_x",
        "-x",
        "a/b",
        "../x",
        "\u{e9}",
        "a b",
        too_long.as_str(),
    ] {
        let error = validate_id(id).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidName, "{id}");
    }
}

#[test]
fn collection_and_db_names_follow_the_rules() {
    assert!(validate_collection_name("users_2-b").is_ok());
    assert!(validate_db_name(&"a".repeat(64)).is_ok());
    for name in ["Users", "", "_meta", "a.b", "a/b", &"a".repeat(65)] {
        assert_eq!(
            validate_collection_name(name).unwrap_err().code,
            ErrorCode::InvalidName,
            "{name}"
        );
        assert_eq!(
            validate_db_name(name).unwrap_err().code,
            ErrorCode::InvalidName,
            "{name}"
        );
    }
}

#[test]
fn generated_ids_are_valid_and_distinct() {
    let first = new_id();
    let second = new_id();
    assert!(validate_id(&first).is_ok());
    assert_ne!(first, second);
}

#[test]
fn listing_skips_invalid_names_and_state_directory() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    make_collection(temp.path(), "main", "users", Format::Json);
    make_collection(temp.path(), "main", "notes", Format::Md);
    make_collection(temp.path(), "Bad", "users", Format::Json);
    fs::create_dir_all(temp.path().join("main").join("Upper")).unwrap();
    assert_eq!(store.list_dbs().unwrap(), vec!["main".to_owned()]);
    assert_eq!(
        store.list_collections("main").unwrap(),
        vec!["notes".to_owned(), "users".to_owned()]
    );
}

#[test]
fn a_missing_db_or_collection_is_not_found_with_its_kind() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let error = store.list_collections("main").unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    assert_eq!(error.details["kind"], json!("db"));
    make_collection(temp.path(), "main", "users", Format::Json);
    let error = store.collection_meta("main", "ghosts").unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    assert_eq!(error.details["kind"], json!("collection"));
}

#[test]
fn doc_path_validates_every_part() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let path = store
        .doc_path("main", "users", "bob", Format::Yaml)
        .unwrap();
    assert_eq!(
        path,
        temp.path().join("main").join("users").join("bob.yaml")
    );
    assert!(
        store
            .doc_path("main", "users", "../x", Format::Json)
            .is_err()
    );
    assert!(store.doc_path("main", "Users", "x", Format::Json).is_err());
    assert!(store.doc_path("..", "users", "x", Format::Json).is_err());
}

#[test]
fn new_names_may_not_start_with_a_dash_but_existing_ones_stay_usable() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    for error in [store.create_db("-x").unwrap_err(), {
        store.create_db("main").unwrap();
        store
            .create_collection("main", "-c", Format::Json)
            .unwrap_err()
    }] {
        assert_eq!(error.code, ErrorCode::InvalidName);
        assert_eq!(error.details["reason"], json!("leading_dash"));
    }
    make_collection(temp.path(), "-old", "kept", Format::Json);
    assert_eq!(
        store.list_dbs().unwrap(),
        vec!["-old".to_owned(), "main".to_owned()]
    );
    assert_eq!(
        store.list_collections("-old").unwrap(),
        vec!["kept".to_owned()]
    );
    store
        .create_collection("-old", "notes", Format::Json)
        .unwrap();
}

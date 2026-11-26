use plaindb_core::text::tokens;
use plaindb_core::{Doc, ErrorCode, Format, IndexSpec, Op, Query, Store, Tx};
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

fn store_with(root: &std::path::Path, coll: &str, format: Format) -> Store {
    let store = Store::init(root).unwrap();
    store.create_db("main").unwrap();
    store.create_collection("main", coll, format).unwrap();
    store
}

fn found(store: &Store, coll: &str, filter: Value) -> Vec<String> {
    store
        .find(Query {
            db: "main".to_owned(),
            coll: coll.to_owned(),
            filter,
            ..Query::default()
        })
        .unwrap()
        .docs
        .iter()
        .map(|(doc, _)| doc["_id"].as_str().unwrap().to_owned())
        .collect()
}

fn text_index(fields: &[&str]) -> IndexSpec {
    IndexSpec::Text {
        text: fields.iter().map(|field| (*field).to_owned()).collect(),
    }
}

#[test]
fn tokens_split_on_words_and_lowercase_without_folding() {
    assert_eq!(
        tokens("The quick, brown-fox!"),
        ["the", "quick", "brown", "fox"]
    );
    assert_eq!(tokens("Hà Nội"), ["hà", "nội"]);
    assert_ne!(tokens("Hà Nội"), tokens("Ha Noi"));
    assert!(tokens("... ,,, !").is_empty());
}

#[test]
fn diacritics_are_never_folded() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), "notes", Format::Json);
    store
        .set_indexes("main", "notes", vec![text_index(&["title"])])
        .unwrap();
    store
        .apply(Tx::new(vec![
            insert("notes", "accented", json!({ "title": "Du lịch hà nội" })),
            insert("notes", "plain", json!({ "title": "Du lich Ha Noi" })),
        ]))
        .unwrap();
    assert_eq!(
        found(&store, "notes", json!({ "$text": "Hà Nội" })),
        ["accented"]
    );
    assert_eq!(
        found(&store, "notes", json!({ "$text": "ha noi" })),
        ["plain"]
    );
}

#[test]
fn every_query_token_must_be_present() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), "notes", Format::Yaml);
    store
        .set_indexes("main", "notes", vec![text_index(&["title", "tags"])])
        .unwrap();
    store
        .apply(Tx::new(vec![
            insert(
                "notes",
                "a",
                json!({ "title": "The quick brown fox", "n": 1 }),
            ),
            insert(
                "notes",
                "b",
                json!({ "title": "A quick dog", "tags": ["fox", "red"], "n": 2 }),
            ),
            insert(
                "notes",
                "c",
                json!({ "title": "Lazy dog", "other": "quick fox" }),
            ),
        ]))
        .unwrap();
    assert_eq!(
        found(&store, "notes", json!({ "$text": "quick fox" })),
        ["a", "b"]
    );
    assert_eq!(
        found(&store, "notes", json!({ "$text": "QUICK" })),
        ["a", "b"]
    );
    assert_eq!(
        found(&store, "notes", json!({ "$text": "quick,fox." })),
        ["a", "b"]
    );
    assert_eq!(
        found(&store, "notes", json!({ "$text": "red fox quick" })),
        ["b"]
    );
    assert_eq!(
        found(
            &store,
            "notes",
            json!({ "$text": "quick", "n": { "$gt": 1 } })
        ),
        ["b"]
    );
    assert_eq!(
        found(
            &store,
            "notes",
            json!({ "$or": [{ "$text": "lazy" }, { "n": 1 }] })
        ),
        ["a", "c"]
    );
}

#[test]
fn the_text_index_follows_writes() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), "notes", Format::Json);
    store
        .set_indexes("main", "notes", vec![text_index(&["title"])])
        .unwrap();
    store
        .apply(Tx::new(vec![insert(
            "notes",
            "a",
            json!({ "title": "alpha beta" }),
        )]))
        .unwrap();
    store
        .apply(Tx::new(vec![Op::Patch {
            db: "main".to_owned(),
            coll: "notes".to_owned(),
            id: "a".to_owned(),
            patch: json!({ "title": "gamma" }),
            if_version: None,
        }]))
        .unwrap();
    assert!(found(&store, "notes", json!({ "$text": "alpha" })).is_empty());
    assert_eq!(found(&store, "notes", json!({ "$text": "gamma" })), ["a"]);
    store
        .apply(Tx::new(vec![Op::Delete {
            db: "main".to_owned(),
            coll: "notes".to_owned(),
            id: "a".to_owned(),
            if_version: None,
        }]))
        .unwrap();
    assert!(found(&store, "notes", json!({ "$text": "gamma" })).is_empty());
}

#[test]
fn markdown_searches_body_and_string_fields_without_an_index() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), "pages", Format::Md);
    store
        .apply(Tx::new(vec![
            insert(
                "pages",
                "intro",
                json!({ "title": "Welcome", "author": "Lan", "views": 3, "body": "Plain text about foxes.\n" }),
            ),
            insert("pages", "other", json!({ "title": "Elsewhere", "body": "Nothing here.\n" })),
        ]))
        .unwrap();
    assert_eq!(
        found(&store, "pages", json!({ "$text": "foxes" })),
        ["intro"]
    );
    assert_eq!(
        found(&store, "pages", json!({ "$text": "welcome lan" })),
        ["intro"]
    );
    assert!(found(&store, "pages", json!({ "$text": "3" })).is_empty());
    assert!(found(&store, "pages", json!({ "$text": "intro" })).is_empty());
}

#[test]
fn json_and_yaml_collections_need_a_declared_text_index() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), "notes", Format::Json);
    store
        .apply(Tx::new(vec![insert(
            "notes",
            "a",
            json!({ "title": "alpha" }),
        )]))
        .unwrap();
    let error = store
        .find(Query {
            db: "main".to_owned(),
            coll: "notes".to_owned(),
            filter: json!({ "$text": "alpha" }),
            ..Query::default()
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidQuery);
    assert!(error.message.contains("declare a text index"));
    for filter in [
        json!({ "$text": "" }),
        json!({ "$text": "!!" }),
        json!({ "$text": 4 }),
    ] {
        let error = store
            .find(Query {
                db: "main".to_owned(),
                coll: "notes".to_owned(),
                filter,
                ..Query::default()
            })
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidQuery);
    }
}

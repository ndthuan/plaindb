use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use plaindb_core::{Doc, ErrorCode, Format, IndexSpec, Op, Query, Store, Tx};
use plaindb_core::{Durability, StoreConfig};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
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

fn delete(coll: &str, id: &str) -> Op {
    Op::Delete {
        db: "main".to_owned(),
        coll: coll.to_owned(),
        id: id.to_owned(),
        if_version: None,
    }
}

fn unique(field: &str) -> IndexSpec {
    IndexSpec::Field {
        field: field.to_owned(),
        unique: true,
        multi: false,
    }
}

fn plain(field: &str) -> IndexSpec {
    IndexSpec::Field {
        field: field.to_owned(),
        unique: false,
        multi: false,
    }
}

fn multi(field: &str) -> IndexSpec {
    IndexSpec::Field {
        field: field.to_owned(),
        unique: false,
        multi: true,
    }
}

fn store_with(root: &Path, collections: &[&str]) -> Store {
    let store = Store::init_with(
        root,
        StoreConfig {
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.create_db("main").unwrap();
    for name in collections {
        store.create_collection("main", name, Format::Json).unwrap();
    }
    store
}

fn ids(store: &Store, coll: &str, filter: &Value) -> Vec<String> {
    let found = store
        .find(Query {
            db: "main".to_owned(),
            coll: coll.to_owned(),
            filter: filter.clone(),
            limit: Some(10_000),
            ..Query::default()
        })
        .unwrap();
    found
        .docs
        .iter()
        .map(|(doc, _)| doc["_id"].as_str().unwrap().to_owned())
        .collect()
}

fn small_scalar(seed: &mut fastrand::Rng) -> Value {
    match seed.u8(0..4) {
        0 => json!(seed.i64(0..6)),
        1 => json!(["a", "b", "ab", "c"][seed.usize(0..4)]),
        2 => json!(seed.bool()),
        _ => Value::Null,
    }
}

fn random_doc(seed: &mut fastrand::Rng) -> Value {
    let mut value = json!({});
    if seed.u8(0..5) != 0 {
        value["a"] = small_scalar(seed);
    }
    if seed.u8(0..4) != 0 {
        let items: Vec<Value> = (0..seed.usize(0..4)).map(|_| small_scalar(seed)).collect();
        value["t"] = Value::Array(items);
    }
    if seed.u8(0..4) != 0 {
        value["n"] = json!(seed.i64(0..6));
    }
    if seed.u8(0..3) != 0 {
        value["m"] = json!({ "x": small_scalar(seed) });
    }
    value
}

fn cond_strategy() -> impl Strategy<Value = Value> {
    let scalar = prop_oneof![
        (0i64..6).prop_map(|n| json!(n)),
        prop::sample::select(vec!["a", "b", "ab", "c", ""]).prop_map(|text| json!(text)),
        any::<bool>().prop_map(|flag| json!(flag)),
        Just(Value::Null),
    ];
    let field = prop::sample::select(vec!["a", "t", "n", "m.x", "missing"]);
    let operator = prop::sample::select(vec![
        "$eq", "$ne", "$gt", "$gte", "$lt", "$lte", "$in", "$exists", "$prefix", "bare",
    ]);
    (
        field,
        operator,
        scalar,
        prop::collection::vec(0i64..6, 0..3),
    )
        .prop_map(|(field, operator, scalar, list)| {
            let condition = match operator {
                "bare" => scalar,
                "$in" => json!({ "$in": list }),
                "$exists" => json!({ "$exists": matches!(scalar, Value::Bool(true)) }),
                "$prefix" => json!({ "$prefix": scalar.as_str().unwrap_or("a") }),
                other => json!({ other: scalar }),
            };
            json!({ field: condition })
        })
}

fn filter_strategy() -> impl Strategy<Value = Value> {
    (prop::collection::vec(cond_strategy(), 2..4), 0u8..3).prop_map(|(conds, shape)| match shape {
        0 => json!({ "$and": conds }),
        1 => json!({ "$or": conds }),
        _ => json!({ "$or": [conds[0].clone(), { "$and": conds[1..].to_vec() }] }),
    })
}

#[test]
fn indexed_and_scanned_results_are_identical() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), &["plain", "indexed"]);
    store
        .set_indexes(
            "main",
            "indexed",
            vec![plain("a"), multi("t"), plain("n"), plain("m.x")],
        )
        .unwrap();
    for coll in ["plain", "indexed"] {
        let mut seed = fastrand::Rng::with_seed(7);
        let ops = (0..200)
            .map(|n| insert(coll, &format!("d{n:03}"), random_doc(&mut seed)))
            .collect();
        store.apply(Tx::new(ops)).unwrap();
    }
    let mut runner = TestRunner::new(Config::with_cases(400));
    runner
        .run(&filter_strategy(), |filter| {
            let scanned = ids(&store, "plain", &filter);
            let indexed = ids(&store, "indexed", &filter);
            prop_assert_eq!(scanned, indexed, "filter {}", filter);
            Ok(())
        })
        .unwrap();
}

fn put(coll: &str, id: &str, value: Value) -> Op {
    Op::Put {
        db: "main".to_owned(),
        coll: coll.to_owned(),
        id: id.to_owned(),
        doc: doc(value),
        if_version: None,
    }
}

const WORDS: [&str; 5] = ["red", "green", "blue", "cyan", "gold"];

fn worded_doc(seed: &mut fastrand::Rng) -> Value {
    let mut value = random_doc(seed);
    let words: Vec<&str> = (0..seed.usize(0..4))
        .map(|_| WORDS[seed.usize(0..WORDS.len())])
        .collect();
    value["s"] = json!(words.join(" "));
    value
}

fn text_ids(model: &BTreeMap<String, Value>, query: &str) -> Vec<String> {
    model
        .iter()
        .filter(|(_, value)| {
            let words: Vec<&str> = value["s"].as_str().unwrap().split(' ').collect();
            query.split(' ').all(|word| words.contains(&word))
        })
        .map(|(id, _)| id.clone())
        .collect()
}

fn compare_with_scan(store: &Store, model: &BTreeMap<String, Value>, cases: u32) {
    let mut runner = TestRunner::new(Config::with_cases(cases));
    runner
        .run(&filter_strategy(), |filter| {
            let scanned = ids(store, "plain", &filter);
            let indexed = ids(store, "indexed", &filter);
            prop_assert_eq!(scanned, indexed, "filter {}", filter);
            Ok(())
        })
        .unwrap();
    for query in ["red", "blue gold", "cyan green red"] {
        assert_eq!(
            ids(store, "indexed", &json!({ "$text": query })),
            text_ids(model, query),
            "text {query}"
        );
    }
}

#[test]
fn indexes_match_a_scan_through_deletes_and_reinserts() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store_with(dir.path(), &["plain", "indexed"]);
    store
        .set_indexes(
            "main",
            "indexed",
            vec![
                plain("a"),
                multi("t"),
                plain("n"),
                plain("m.x"),
                IndexSpec::Text {
                    text: vec!["s".to_owned()],
                },
            ],
        )
        .unwrap();
    let mut seed = fastrand::Rng::with_seed(11);
    let mut model: BTreeMap<String, Value> = BTreeMap::new();
    for round in 0..600 {
        let mut ops = Vec::new();
        let mut planned: BTreeMap<String, Option<Value>> = BTreeMap::new();
        for _ in 0..seed.usize(1..4) {
            let id = format!("d{:02}", seed.usize(0..60));
            if planned.contains_key(&id) {
                continue;
            }
            let present = model.contains_key(&id);
            let next = if present && seed.bool() {
                None
            } else {
                Some(worded_doc(&mut seed))
            };
            for coll in ["plain", "indexed"] {
                ops.push(match (&next, present) {
                    (None, _) => delete(coll, &id),
                    (Some(value), true) => put(coll, &id, value.clone()),
                    (Some(value), false) => insert(coll, &id, value.clone()),
                });
            }
            planned.insert(id, next);
        }
        store.apply(Tx::new(ops)).unwrap();
        for (id, next) in planned {
            match next {
                Some(value) => model.insert(id, value),
                None => model.remove(&id),
            };
        }
        if round % 100 == 99 {
            compare_with_scan(&store, &model, 40);
        }
    }
    drop(store);
    store = Store::open(dir.path()).unwrap();
    compare_with_scan(&store, &model, 100);
    drop(store);
    fs::remove_file(dir.path().join(".plaindb").join("index-cache.bin")).unwrap();
    store = Store::open(dir.path()).unwrap();
    compare_with_scan(&store, &model, 100);
}

#[test]
fn indexes_follow_updates_and_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), &["items"]);
    store
        .set_indexes("main", "items", vec![plain("n"), multi("t")])
        .unwrap();
    store
        .apply(Tx::new(vec![
            insert("items", "a", json!({ "n": 1, "t": ["x", "y"] })),
            insert("items", "b", json!({ "n": 2, "t": ["y"] })),
        ]))
        .unwrap();
    assert_eq!(ids(&store, "items", &json!({ "t": "y" })), ["a", "b"]);
    store
        .apply(Tx::new(vec![
            Op::Patch {
                db: "main".to_owned(),
                coll: "items".to_owned(),
                id: "a".to_owned(),
                patch: json!({ "t": ["z"], "n": 3 }),
                if_version: None,
            },
            delete("items", "b"),
        ]))
        .unwrap();
    assert_eq!(
        ids(&store, "items", &json!({ "t": "y" })),
        Vec::<String>::new()
    );
    assert_eq!(ids(&store, "items", &json!({ "t": "z" })), ["a"]);
    assert_eq!(ids(&store, "items", &json!({ "n": { "$gte": 2 } })), ["a"]);
    assert_eq!(
        ids(&store, "items", &json!({ "n": 1 })),
        Vec::<String>::new()
    );
}

#[test]
fn unique_violation_inside_one_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), &["users"]);
    store
        .set_indexes("main", "users", vec![unique("email")])
        .unwrap();
    let error = store
        .apply(Tx::new(vec![
            insert("users", "u1", json!({ "email": "a@x.io" })),
            insert("users", "u2", json!({ "email": "a@x.io" })),
        ]))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UniqueViolation);
    assert_eq!(error.details["field"], "email");
    assert_eq!(error.details["value"], "a@x.io");
    assert_eq!(error.details["ids"], json!(["u1", "u2"]));
    assert!(store.list_ids("main", "users").unwrap().is_empty());
}

#[test]
fn unique_violation_across_transactions_leaves_files_alone() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), &["users"]);
    store
        .set_indexes("main", "users", vec![unique("email")])
        .unwrap();
    store
        .apply(Tx::new(vec![insert(
            "users",
            "u1",
            json!({ "email": "a@x.io" }),
        )]))
        .unwrap();
    let error = store
        .apply(Tx::new(vec![
            insert("users", "u2", json!({ "email": "b@x.io" })),
            insert("users", "u3", json!({ "email": "a@x.io" })),
        ]))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UniqueViolation);
    assert_eq!(store.list_ids("main", "users").unwrap(), ["u1"]);
    assert!(!dir.path().join("main/users/u2.json").exists());
    store
        .apply(Tx::new(vec![Op::Put {
            db: "main".to_owned(),
            coll: "users".to_owned(),
            id: "u1".to_owned(),
            doc: doc(json!({ "email": "a@x.io", "name": "same email, same document" })),
            if_version: None,
        }]))
        .unwrap();
}

#[test]
fn delete_then_reinsert_the_same_value_in_one_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), &["users"]);
    store
        .set_indexes("main", "users", vec![unique("email")])
        .unwrap();
    store
        .apply(Tx::new(vec![insert(
            "users",
            "u1",
            json!({ "email": "a@x.io" }),
        )]))
        .unwrap();
    store
        .apply(Tx::new(vec![
            delete("users", "u1"),
            insert("users", "u2", json!({ "email": "a@x.io" })),
        ]))
        .unwrap();
    assert_eq!(ids(&store, "users", &json!({ "email": "a@x.io" })), ["u2"]);
}

#[test]
fn adding_a_unique_index_over_duplicates_fails_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), &["users"]);
    store
        .apply(Tx::new(vec![
            insert("users", "u1", json!({ "email": "a@x.io" })),
            insert("users", "u2", json!({ "email": "a@x.io" })),
            insert("users", "u3", json!({ "email": "c@x.io" })),
        ]))
        .unwrap();
    let error = store
        .set_indexes("main", "users", vec![unique("email")])
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UniqueViolation);
    assert_eq!(error.details["ids"], json!(["u1", "u2"]));
    assert!(
        store
            .collection_meta("main", "users")
            .unwrap()
            .indexes
            .is_empty()
    );
}

#[test]
fn single_indexes_refuse_arrays() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), &["items"]);
    store
        .apply(Tx::new(vec![insert(
            "items",
            "a",
            json!({ "tags": ["x"] }),
        )]))
        .unwrap();
    let error = store
        .set_indexes("main", "items", vec![plain("tags")])
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidDocument);
    assert_eq!(error.details["reason"], "array_in_single_index");
    store
        .set_indexes("main", "items", vec![plain("size")])
        .unwrap();
    let error = store
        .apply(Tx::new(vec![insert(
            "items",
            "b",
            json!({ "size": [1, 2] }),
        )]))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidDocument);
    assert_eq!(error.details["reason"], "array_in_single_index");
    store
        .set_indexes("main", "items", vec![multi("tags")])
        .unwrap();
}

#[test]
fn a_hand_edited_array_in_a_single_index_still_matches() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), &["items"]);
    store
        .set_indexes("main", "items", vec![plain("size")])
        .unwrap();
    store
        .apply(Tx::new(vec![
            insert("items", "a", json!({ "size": 1 })),
            insert("items", "b", json!({ "size": 2 })),
        ]))
        .unwrap();
    fs::write(
        dir.path().join("main/items/b.json"),
        "{\"_id\": \"b\", \"size\": [1, 9]}\n",
    )
    .unwrap();
    store.rescan().unwrap();
    assert_eq!(ids(&store, "items", &json!({ "size": 1 })), ["a", "b"]);
    assert_eq!(
        ids(&store, "items", &json!({ "size": { "$gt": 5 } })),
        ["b"]
    );
}

#[test]
fn rescan_keeps_indexes_in_step_with_files_edited_by_hand() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), &["items"]);
    store
        .set_indexes("main", "items", vec![plain("size")])
        .unwrap();
    store
        .apply(Tx::new(vec![
            insert("items", "a", json!({ "size": 1 })),
            insert("items", "b", json!({ "size": 2 })),
        ]))
        .unwrap();
    fs::write(
        dir.path().join("main/items/b.json"),
        "{\"_id\": \"b\", \"size\": 7, \"padding\": true}\n",
    )
    .unwrap();
    fs::remove_file(dir.path().join("main/items/a.json")).unwrap();
    store.rescan().unwrap();
    assert_eq!(ids(&store, "items", &json!({ "size": 7 })), ["b"]);
    assert_eq!(
        ids(&store, "items", &json!({ "size": 2 })),
        Vec::<String>::new()
    );
    assert_eq!(
        ids(&store, "items", &json!({ "size": 1 })),
        Vec::<String>::new()
    );
}

#[test]
fn a_scan_over_the_time_limit_fails_but_an_indexed_lookup_does_not() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = store_with(dir.path(), &["items"]);
        store
            .set_indexes("main", "items", vec![plain("size")])
            .unwrap();
        store
            .apply(Tx::new(vec![insert(
                "items",
                "a",
                json!({ "size": 1, "other": 1 }),
            )]))
            .unwrap();
    }
    let path = dir.path().join("_store.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["limits"] = json!({ "max_scan_ms": 0 });
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(ids(&store, "items", &json!({ "size": 1 })), ["a"]);
    let error = store
        .find(Query {
            db: "main".to_owned(),
            coll: "items".to_owned(),
            filter: json!({ "other": 1 }),
            ..Query::default()
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::LimitExceeded);
}

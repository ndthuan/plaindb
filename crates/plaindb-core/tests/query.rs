use std::collections::BTreeSet;

use plaindb_core::{Dir, Doc, ErrorCode, Format, Op, Query, Store, Tx};
use plaindb_core::{Durability, StoreConfig};
use serde_json::{Value, json};

fn doc(value: Value) -> Doc {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn insert(id: &str, value: Value) -> Op {
    Op::Insert {
        db: "main".to_owned(),
        coll: "items".to_owned(),
        id: Some(id.to_owned()),
        doc: doc(value),
    }
}

fn fixture(root: &std::path::Path) -> Store {
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
        .create_collection("main", "items", Format::Json)
        .unwrap();
    let ops = (0..300)
        .map(|n| {
            let city = ["Hanoi", "Hue", "Saigon"][n % 3];
            let mut value = json!({
                "n": n,
                "name": format!("item-{n:03}"),
                "tags": [if n % 2 == 0 { "even" } else { "odd" }, format!("mod{}", n % 5)],
                "addr": { "city": city },
                "score": f64::from(u32::try_from(n).unwrap()) / 2.0,
            });
            if n % 4 == 0 {
                value["opt"] = json!(n);
            }
            insert(&format!("d{n:03}"), value)
        })
        .collect();
    store.apply(Tx::new(ops)).unwrap();
    store
}

fn query(filter: Value) -> Query {
    Query {
        db: "main".to_owned(),
        coll: "items".to_owned(),
        filter,
        limit: Some(1000),
        ..Query::default()
    }
}

fn ids(store: &Store, filter: Value) -> Vec<String> {
    store
        .find(query(filter))
        .unwrap()
        .docs
        .iter()
        .map(|(doc, _)| doc["_id"].as_str().unwrap().to_owned())
        .collect()
}

fn count(store: &Store, filter: Value) -> usize {
    ids(store, filter).len()
}

#[test]
fn every_operator_alone() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    assert_eq!(count(&store, json!({})), 300);
    assert_eq!(ids(&store, json!({ "n": 7 })), ["d007"]);
    assert_eq!(ids(&store, json!({ "n": { "$eq": 7 } })), ["d007"]);
    assert_eq!(count(&store, json!({ "n": { "$ne": 7 } })), 299);
    assert_eq!(count(&store, json!({ "n": { "$gt": 289 } })), 10);
    assert_eq!(count(&store, json!({ "n": { "$gte": 289 } })), 11);
    assert_eq!(count(&store, json!({ "n": { "$lt": 10 } })), 10);
    assert_eq!(count(&store, json!({ "n": { "$lte": 10 } })), 11);
    assert_eq!(count(&store, json!({ "n": { "$in": [1, 2, 999] } })), 2);
    assert_eq!(count(&store, json!({ "opt": { "$exists": true } })), 75);
    assert_eq!(count(&store, json!({ "opt": { "$exists": false } })), 225);
    assert_eq!(
        count(&store, json!({ "name": { "$prefix": "item-29" } })),
        10
    );
    assert_eq!(count(&store, json!({ "n": { "$gt": 10, "$lt": 20 } })), 9);
}

#[test]
fn and_or_nesting_and_paths() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    assert_eq!(count(&store, json!({ "addr.city": "Hue" })), 100);
    let filter = json!({
        "$or": [
            { "n": { "$lt": 3 } },
            { "$and": [{ "addr.city": "Saigon" }, { "n": { "$gte": 297 } }] },
        ],
    });
    assert_eq!(ids(&store, filter), ["d000", "d001", "d002", "d299"]);
    let both = json!({ "addr.city": "Hanoi", "tags": "even", "n": { "$lt": 20 } });
    assert_eq!(ids(&store, both), ["d000", "d006", "d012", "d018"]);
}

#[test]
fn arrays_match_any_element() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    assert_eq!(count(&store, json!({ "tags": "even" })), 150);
    assert_eq!(count(&store, json!({ "tags": { "$ne": "even" } })), 150);
    assert_eq!(
        count(&store, json!({ "tags": { "$in": ["mod1", "mod2"] } })),
        120
    );
    assert_eq!(count(&store, json!({ "tags": ["even", "mod0"] })), 30);
    assert_eq!(count(&store, json!({ "tags": { "$prefix": "mod" } })), 300);
    store
        .apply(Tx::new(vec![insert(
            "nested",
            json!({ "rows": [{ "k": 1 }, { "k": 5 }] }),
        )]))
        .unwrap();
    assert_eq!(ids(&store, json!({ "rows.k": { "$gt": 4 } })), ["nested"]);
    assert_eq!(ids(&store, json!({ "rows.k": 1 })), ["nested"]);
    assert_eq!(count(&store, json!({ "rows.k": { "$ne": 1 } })), 300);
}

#[test]
fn missing_fields_match_only_exists_false_and_ne() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    assert_eq!(count(&store, json!({ "opt": { "$gte": 0 } })), 75);
    assert_eq!(count(&store, json!({ "opt": { "$ne": 4 } })), 299);
    assert_eq!(count(&store, json!({ "opt": { "$in": [4, 8] } })), 2);
    assert_eq!(
        count(&store, json!({ "nothing": { "$exists": false } })),
        300
    );
    assert_eq!(count(&store, json!({ "nothing": null })), 0);
}

#[test]
fn sort_orders_mixed_types() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init_with(
        dir.path(),
        StoreConfig {
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Json)
        .unwrap();
    let values = [
        json!({ "v": "b" }),
        json!({ "v": 2 }),
        json!({ "v": null }),
        json!({ "v": [1] }),
        json!({ "v": true }),
        json!({ "v": { "a": 1 } }),
        json!({ "v": "a" }),
        json!({ "v": 1.5 }),
        json!({}),
    ];
    let ops = values
        .into_iter()
        .enumerate()
        .map(|(index, value)| insert(&format!("m{index}"), value))
        .collect();
    store.apply(Tx::new(ops)).unwrap();
    let mut asc = query(json!({}));
    asc.sort = vec![("v".to_owned(), Dir::Asc)];
    let found = store.find(asc.clone()).unwrap();
    let order: Vec<&str> = found
        .docs
        .iter()
        .map(|(doc, _)| doc["_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        ["m2", "m8", "m4", "m7", "m1", "m6", "m0", "m3", "m5"]
    );
    asc.sort = vec![("v".to_owned(), Dir::Desc)];
    let found = store.find(asc).unwrap();
    let order: Vec<&str> = found
        .docs
        .iter()
        .map(|(doc, _)| doc["_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        ["m5", "m3", "m0", "m6", "m1", "m7", "m4", "m2", "m8"]
    );
}

#[test]
fn paging_returns_each_document_once_despite_inserts() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    let mut page = query(json!({}));
    page.limit = Some(7);
    page.sort = vec![
        ("addr.city".to_owned(), Dir::Desc),
        ("n".to_owned(), Dir::Asc),
    ];
    let mut seen = Vec::new();
    let mut pages = 0;
    loop {
        let found = store.find(page.clone()).unwrap();
        seen.extend(
            found
                .docs
                .iter()
                .map(|(doc, _)| doc["_id"].as_str().unwrap().to_owned()),
        );
        pages += 1;
        if pages == 3 {
            store
                .apply(Tx::new(vec![insert(
                    "late",
                    json!({ "n": 1000, "addr": { "city": "Hue" } }),
                )]))
                .unwrap();
        }
        match found.next_cursor {
            Some(cursor) => page.cursor = Some(cursor),
            None => break,
        }
    }
    let unique: BTreeSet<&String> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len());
    assert_eq!(unique.len(), 301);
    assert_eq!(pages, 43);
}

#[test]
fn projection_keeps_the_id_and_versions_come_back() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    let mut request = query(json!({ "n": 3 }));
    request.fields = Some(vec!["name".to_owned(), "addr.city".to_owned()]);
    let found = store.find(request).unwrap();
    assert_eq!(found.docs.len(), 1);
    let (found_doc, version) = &found.docs[0];
    assert_eq!(
        Value::Object(found_doc.clone()),
        json!({ "_id": "d003", "name": "item-003", "addr": { "city": "Hanoi" } })
    );
    assert_eq!(
        &store.get("main", "items", "d003").unwrap().unwrap().1,
        version
    );
}

#[test]
fn unknown_operators_and_bad_input_are_invalid_queries() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    for filter in [
        json!({ "a": { "$regex": "x" } }),
        json!({ "$nor": [] }),
        json!({ "a": { "$in": 3 } }),
        json!({ "$or": [] }),
        json!({ "a": { "$gt": 1, "b": 2 } }),
        json!({ "a..b": 1 }),
        json!([1]),
    ] {
        let error = store.find(query(filter.clone())).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidQuery, "{filter}");
    }
    let error = store
        .find(query(json!({ "a": { "$regex": "x" } })))
        .unwrap_err();
    assert_eq!(error.details["operator"], "$regex");
    let mut bad = query(json!({}));
    bad.cursor = Some("not a cursor".to_owned());
    assert_eq!(store.find(bad).unwrap_err().code, ErrorCode::InvalidQuery);
}

#[test]
fn count_distinct_and_limits() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    assert_eq!(
        store
            .count("main", "items", &json!({ "tags": "odd" }))
            .unwrap(),
        150
    );
    let cities = store
        .distinct("main", "items", "addr.city", &json!({}))
        .unwrap();
    assert_eq!(cities, [json!("Hanoi"), json!("Hue"), json!("Saigon")]);
    let tags = store
        .distinct("main", "items", "tags", &json!({ "n": { "$lt": 5 } }))
        .unwrap();
    assert_eq!(
        tags,
        [
            json!("even"),
            json!("mod0"),
            json!("mod1"),
            json!("mod2"),
            json!("mod3"),
            json!("mod4"),
            json!("odd")
        ]
    );
    let default_page = store
        .find(Query {
            db: "main".to_owned(),
            coll: "items".to_owned(),
            ..Query::default()
        })
        .unwrap();
    assert_eq!(default_page.docs.len(), 100);
    assert!(default_page.next_cursor.is_some());
    let missing = Query {
        db: "main".to_owned(),
        coll: "nope".to_owned(),
        ..Query::default()
    };
    assert_eq!(store.find(missing).unwrap_err().code, ErrorCode::NotFound);
}

fn values_store(root: &std::path::Path, field: &str, values: &[Value]) -> Store {
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
        .create_collection("main", "items", Format::Json)
        .unwrap();
    let ops = values
        .iter()
        .enumerate()
        .map(|(n, value)| insert(&format!("v{n}"), json!({ field: value })))
        .collect();
    store.apply(Tx::new(ops)).unwrap();
    store
}

fn unique_on(store: &Store, field: &str) -> plaindb_core::Result<()> {
    store.set_indexes(
        "main",
        "items",
        vec![plaindb_core::IndexSpec::Field {
            field: field.to_owned(),
            unique: true,
            multi: false,
        }],
    )
}

fn sorted_ids(store: &Store, field: &str) -> Vec<String> {
    store
        .find(Query {
            sort: vec![(field.to_owned(), Dir::Asc)],
            ..query(Value::Null)
        })
        .unwrap()
        .docs
        .iter()
        .map(|(doc, _)| doc["_id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn integers_beyond_two_to_the_fifty_three_stay_apart() {
    let dir = tempfile::tempdir().unwrap();
    let store = values_store(
        dir.path(),
        "ext",
        &[
            json!(9_007_199_254_740_992_u64),
            json!(9_007_199_254_740_993_u64),
        ],
    );
    assert_eq!(
        ids(&store, json!({"ext": 9_007_199_254_740_993_u64})),
        ["v1"]
    );
    assert_eq!(
        store
            .distinct("main", "items", "ext", &Value::Null)
            .unwrap()
            .len(),
        2
    );
    unique_on(&store, "ext").unwrap();
    assert_eq!(
        ids(&store, json!({"ext": 9_007_199_254_740_993_u64})),
        ["v1"]
    );
}

#[test]
fn integers_of_every_width_sort_in_numeric_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = values_store(
        dir.path(),
        "n",
        &[
            json!(u64::MAX),
            json!(-1),
            json!(i64::MAX),
            json!(i64::MIN),
            json!(-0.5),
            json!(9_223_372_036_854_775_808_u64),
            json!(9_223_372_036_854_775_806_i64),
        ],
    );
    assert_eq!(
        sorted_ids(&store, "n"),
        ["v3", "v1", "v4", "v6", "v2", "v5", "v0"]
    );
}

#[test]
fn an_integer_and_the_same_float_are_one_value() {
    let dir = tempfile::tempdir().unwrap();
    let store = values_store(
        dir.path(),
        "x",
        &[
            json!(1),
            json!(1.0),
            json!(-0.0),
            json!(0),
            json!(0.0),
            json!(-3),
            json!(-3.0),
        ],
    );
    assert_eq!(ids(&store, json!({"x": -3})), ["v5", "v6"]);
    assert_eq!(ids(&store, json!({"x": 1})), ["v0", "v1"]);
    assert_eq!(ids(&store, json!({"x": 1.0})), ["v0", "v1"]);
    assert_eq!(
        store
            .distinct("main", "items", "x", &Value::Null)
            .unwrap()
            .len(),
        3
    );
    let error = unique_on(&store, "x").unwrap_err();
    assert_eq!(error.code, ErrorCode::UniqueViolation);
}

#[test]
fn objects_compare_by_their_values_under_a_unique_index() {
    let dir = tempfile::tempdir().unwrap();
    let store = values_store(dir.path(), "o", &[json!({"x": 1})]);
    assert_eq!(ids(&store, json!({"o": {"x": 1.0}})), ["v0"]);
    unique_on(&store, "o").unwrap();
    let error = store
        .apply(Tx::new(vec![insert("w", json!({"o": {"x": 1.0}}))]))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UniqueViolation);
}

#[test]
fn arrays_and_objects_compare_element_by_element() {
    let dir = tempfile::tempdir().unwrap();
    let store = values_store(
        dir.path(),
        "w",
        &[
            json!({"l": [10]}),
            json!({"l": [9]}),
            json!({"l": [9, 1]}),
            json!({"m": 1}),
            json!({"l": [9], "m": 0}),
            json!({"l": [2.5]}),
        ],
    );
    assert_eq!(
        store.distinct("main", "items", "w", &Value::Null).unwrap(),
        [
            json!({"l": [2.5]}),
            json!({"l": [9]}),
            json!({"l": [9], "m": 0}),
            json!({"l": [9, 1]}),
            json!({"l": [10]}),
            json!({"m": 1}),
        ]
    );
}

#[test]
fn floats_and_integers_order_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let store = values_store(
        dir.path(),
        "n",
        &[
            json!(3),
            json!(2.5),
            json!(2),
            json!(9_007_199_254_740_993_u64),
            json!(9_007_199_254_740_992.0),
        ],
    );
    assert_eq!(ids(&store, json!({"n": {"$gt": 2, "$lt": 3}})), ["v1"]);
    assert_eq!(
        ids(&store, json!({"n": {"$gt": 9_007_199_254_740_992.0}})),
        ["v3"]
    );
    assert_eq!(sorted_ids(&store, "n"), ["v2", "v1", "v0", "v4", "v3"]);
}

fn first_page_cursor(store: &Store, filter: Value, sort: &str) -> String {
    store
        .find(Query {
            sort: vec![(sort.to_owned(), Dir::Asc)],
            limit: Some(10),
            ..query(filter)
        })
        .unwrap()
        .next_cursor
        .expect("a second page")
}

fn cursor_reason(store: &Store, filter: Value, sort: &str, cursor: &str) -> Value {
    let error = store
        .find(Query {
            sort: vec![(sort.to_owned(), Dir::Asc)],
            limit: Some(10),
            cursor: Some(cursor.to_owned()),
            ..query(filter)
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidQuery);
    error.details["reason"].clone()
}

#[test]
fn a_cursor_only_fits_the_query_it_was_made_for() {
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    let cursor = first_page_cursor(&store, json!({"tags": "even"}), "n");
    assert_eq!(
        cursor_reason(&store, json!({"tags": "even"}), "score", &cursor),
        "cursor_mismatch"
    );
    assert_eq!(
        cursor_reason(&store, json!({"tags": "odd"}), "n", &cursor),
        "cursor_mismatch"
    );
    let page = store
        .find(Query {
            sort: vec![("n".to_owned(), Dir::Asc)],
            limit: Some(10),
            cursor: Some(cursor),
            ..query(json!({"tags": "even"}))
        })
        .unwrap();
    assert_eq!(page.docs.len(), 10);
}

#[test]
fn an_old_or_broken_cursor_is_a_bad_cursor() {
    use base64::Engine;
    let dir = tempfile::tempdir().unwrap();
    let store = fixture(dir.path());
    let old = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("[3,\"d003\"]");
    assert_eq!(cursor_reason(&store, Value::Null, "n", &old), "bad_cursor");
    assert_eq!(
        cursor_reason(&store, Value::Null, "n", "not a cursor"),
        "bad_cursor"
    );
}

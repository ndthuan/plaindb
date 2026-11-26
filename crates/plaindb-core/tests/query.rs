use std::collections::BTreeSet;

use plaindb_core::{Dir, Doc, ErrorCode, Format, Op, Query, Store, Tx};
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
    let store = Store::init(root).unwrap();
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
    let store = Store::init(dir.path()).unwrap();
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

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use plaindb_core::testgen::{COLLECTION, DB, build_store, document, id_for};
use plaindb_core::{Dir, Doc, ErrorCode, Format, Op, Query, Store, Tx, version_of};
use serde_json::{Value, json};
use tempfile::TempDir;

const SEED: u64 = 7;

fn rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}

fn order(left: &Value, right: &Value) -> Ordering {
    match (left, right) {
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Number(a), Value::Number(b)) => a.as_f64().unwrap().total_cmp(&b.as_f64().unwrap()),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => rank(left).cmp(&rank(right)),
    }
}

fn same(left: &Value, right: &Value) -> bool {
    rank(left) == rank(right) && order(left, right) == Ordering::Equal
}

fn candidates<'a>(value: &'a Value, path: &[&str], out: &mut Vec<&'a Value>) {
    match (value, path.split_first()) {
        (Value::Array(items), _) => {
            for item in items {
                candidates(item, path, out);
            }
        }
        (_, None) => out.push(value),
        (Value::Object(map), Some((head, rest))) => {
            if let Some(next) = map.get(*head) {
                candidates(next, rest, out);
            }
        }
        _ => {}
    }
}

fn found<'a>(doc: &'a Doc, field: &str) -> Vec<&'a Value> {
    let path: Vec<&str> = field.split('.').collect();
    let mut out = Vec::new();
    for (key, value) in doc {
        if key == path[0] {
            candidates(value, &path[1..], &mut out);
        }
    }
    out
}

fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

struct Row {
    doc: Doc,
    words: BTreeSet<String>,
}

fn text_words(doc: &Doc) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for field in ["title", "body"] {
        if let Some(Value::String(text)) = doc.get(field) {
            out.extend(words(text));
        }
    }
    out
}

fn condition(doc: &Doc, field: &str, cond: &Value) -> bool {
    let values = found(doc, field);
    let Value::Object(ops) = cond else {
        return values.iter().any(|value| same(value, cond));
    };
    ops.iter().all(|(op, operand)| match op.as_str() {
        "$eq" => values.iter().any(|value| same(value, operand)),
        "$ne" => !values.iter().any(|value| same(value, operand)),
        "$gt" => values.iter().any(|value| order(value, operand).is_gt()),
        "$gte" => values.iter().any(|value| order(value, operand).is_ge()),
        "$lt" => values.iter().any(|value| order(value, operand).is_lt()),
        "$lte" => values.iter().any(|value| order(value, operand).is_le()),
        "$in" => values.iter().any(|value| {
            operand
                .as_array()
                .unwrap()
                .iter()
                .any(|item| same(value, item))
        }),
        "$exists" => values.is_empty() != operand.as_bool().unwrap(),
        "$prefix" => values.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|text| text.starts_with(operand.as_str().unwrap()))
        }),
        other => panic!("unknown operator {other}"),
    })
}

fn matches(row: &Row, filter: &Value) -> bool {
    let doc = &row.doc;
    let Value::Object(map) = filter else {
        return true;
    };
    map.iter().all(|(key, value)| match key.as_str() {
        "$and" => value
            .as_array()
            .unwrap()
            .iter()
            .all(|part| matches(row, part)),
        "$or" => value
            .as_array()
            .unwrap()
            .iter()
            .any(|part| matches(row, part)),
        "$text" => words(value.as_str().unwrap())
            .iter()
            .all(|word| row.words.contains(word)),
        field => condition(doc, field, value),
    })
}

fn has_text(filter: &Value) -> bool {
    match filter {
        Value::Object(map) => map
            .iter()
            .any(|(key, value)| key == "$text" || has_text(value)),
        Value::Array(items) => items.iter().any(has_text),
        _ => false,
    }
}

fn sort_value(doc: &Doc, field: &str) -> Value {
    found(doc, field)
        .first()
        .map_or(Value::Null, |value| (*value).clone())
}

fn queries() -> Vec<(Value, Vec<(&'static str, Dir)>)> {
    let none: Vec<(&str, Dir)> = Vec::new();
    vec![
        (json!({}), none.clone()),
        (json!({"email": "user42@example.test"}), none.clone()),
        (
            json!({"email": {"$eq": "user77@example.test"}}),
            none.clone(),
        ),
        (json!({"status": "approved"}), vec![("age", Dir::Asc)]),
        (
            json!({"status": {"$ne": "draft"}}),
            vec![("score", Dir::Desc)],
        ),
        (json!({"age": {"$gt": 60}}), vec![("age", Dir::Desc)]),
        (json!({"age": {"$gte": 60}}), vec![("age", Dir::Asc)]),
        (json!({"age": {"$lt": 21}}), none.clone()),
        (json!({"age": {"$lte": 21}}), vec![("score", Dir::Asc)]),
        (
            json!({"age": {"$gte": 30, "$lt": 40}}),
            vec![("age", Dir::Asc), ("score", Dir::Desc)],
        ),
        (
            json!({"status": {"$in": ["draft", "review"]}}),
            vec![("status", Dir::Asc), ("age", Dir::Desc)],
        ),
        (json!({"age": {"$in": [20, 30.0, 40]}}), none.clone()),
        (
            json!({"address.city": {"$exists": true}}),
            vec![("address.city", Dir::Desc)],
        ),
        (json!({"missing": {"$exists": false}}), none.clone()),
        (json!({"missing.deeper": {"$exists": false}}), none.clone()),
        (json!({"missing": {"$exists": true}}), none.clone()),
        (json!({"name": {"$prefix": "Person 12"}}), none.clone()),
        (
            json!({"address.city": {"$prefix": "Ha"}}),
            vec![("name", Dir::Asc)],
        ),
        (json!({"tags": "alpha"}), none.clone()),
        (
            json!({"tags": {"$in": ["kernel", "delta"]}}),
            vec![("age", Dir::Asc)],
        ),
        (json!({"tags": {"$ne": "alpha"}}), none.clone()),
        (json!({"missing": {"$ne": 1}}), none.clone()),
        (json!({"missing": 1}), none.clone()),
        (json!({"age": {"$in": []}}), none.clone()),
        (
            json!({"$and": [{"status": "review"}, {"age": {"$gte": 40}}]}),
            vec![("age", Dir::Desc)],
        ),
        (
            json!({"$or": [{"status": "draft"}, {"address.country": "FR"}]}),
            vec![("score", Dir::Desc)],
        ),
        (
            json!({"$and": [{"$or": [{"tags": "alpha"}, {"tags": "beta"}]}, {"active": true}]}),
            none.clone(),
        ),
        (json!({"active": true}), vec![("age", Dir::Asc)]),
        (json!({"active": false}), none.clone()),
        (
            json!({"score": {"$gte": 25.5, "$lte": 75.25}}),
            vec![("score", Dir::Asc)],
        ),
        (json!({"score": 12.5}), none.clone()),
        (json!({"age": 30.0}), none.clone()),
        (
            json!({"address.country": "VN"}),
            vec![("address.city", Dir::Asc), ("age", Dir::Desc)],
        ),
        (json!({"address.zip": {"$prefix": "9"}}), none.clone()),
        (json!({"status": {"$gt": 5}}), none.clone()),
        (json!({"age": {"$lt": "a"}}), none.clone()),
        (json!({"age": {"$gt": null}}), none.clone()),
        (json!({"active": {"$gte": true}}), none.clone()),
        (
            json!({"name": {"$gt": "Person 5"}}),
            vec![("name", Dir::Desc)],
        ),
        (json!({"title": {"$prefix": "kernel"}}), none.clone()),
        (
            json!({"status": "published", "age": {"$gte": 50}, "active": true}),
            vec![("score", Dir::Desc)],
        ),
        (json!({"$text": "pears"}), none.clone()),
        (
            json!({"$text": "pears neighbours"}),
            vec![("age", Dir::Asc)],
        ),
        (json!({"$text": "checksum"}), none.clone()),
        (json!({"$text": "chợ"}), none.clone()),
        (json!({"$text": "kệ"}), vec![("name", Dir::Asc)]),
        (
            json!({"$and": [{"$text": "amendments"}, {"age": {"$gte": 30, "$lt": 50}}]}),
            none.clone(),
        ),
        (
            json!({"$or": [{"$text": "photographs"}, {"email": "user5@example.test"}]}),
            none.clone(),
        ),
        (json!({"$text": "zzzunknown"}), none.clone()),
        (
            json!({"tags": "alpha", "status": {"$ne": "draft"}}),
            vec![
                ("status", Dir::Desc),
                ("score", Dir::Asc),
                ("name", Dir::Asc),
            ],
        ),
        (
            json!({"$or": [{"age": {"$lt": 20}}, {"age": {"$gt": 75}}]}),
            vec![("age", Dir::Asc)],
        ),
    ]
}

fn query_for(filter: &Value, sort: &[(&str, Dir)], limit: u64, cursor: Option<String>) -> Query {
    Query {
        db: DB.to_owned(),
        coll: COLLECTION.to_owned(),
        filter: filter.clone(),
        sort: sort
            .iter()
            .map(|(field, dir)| ((*field).to_owned(), *dir))
            .collect(),
        limit: Some(limit),
        cursor,
        fields: None,
    }
}

fn expected(docs: &[Row], filter: &Value, sort: &[(&str, Dir)]) -> Vec<String> {
    let mut hits: Vec<&Doc> = docs
        .iter()
        .filter(|row| matches(row, filter))
        .map(|row| &row.doc)
        .collect();
    hits.sort_by(|left, right| {
        for (field, dir) in sort {
            let ordering = order(&sort_value(left, field), &sort_value(right, field));
            let ordering = if *dir == Dir::Desc {
                ordering.reverse()
            } else {
                ordering
            };
            if ordering.is_ne() {
                return ordering;
            }
        }
        left["_id"].as_str().cmp(&right["_id"].as_str())
    });
    hits.iter()
        .map(|doc| doc["_id"].as_str().unwrap().to_owned())
        .collect()
}

fn paged(store: &Store, filter: &Value, sort: &[(&str, Dir)], page: u64) -> Vec<String> {
    let mut ids = Vec::new();
    let mut cursor = None;
    loop {
        let result = store
            .find(query_for(filter, sort, page, cursor))
            .unwrap_or_else(|error| panic!("{filter}: {error:?}"));
        ids.extend(
            result
                .docs
                .iter()
                .map(|(doc, _)| doc["_id"].as_str().unwrap().to_owned()),
        );
        match result.next_cursor {
            Some(next) => cursor = Some(next),
            None => return ids,
        }
    }
}

fn distinct_expected(docs: &[Row], filter: &Value, field: &str) -> Vec<Value> {
    let mut values: Vec<Value> = Vec::new();
    for row in docs.iter().filter(|row| matches(row, filter)) {
        for value in found(&row.doc, field) {
            if !values.iter().any(|known| same(known, value)) {
                values.push(value.clone());
            }
        }
    }
    values.sort_by(order);
    values
}

fn generated(format: Format, count: usize) -> Vec<Row> {
    (0..count)
        .map(|index| {
            let mut doc = document(format, SEED, index);
            doc.insert("_id".to_owned(), Value::String(id_for(index)));
            let words = text_words(&doc);
            Row { doc, words }
        })
        .collect()
}

fn check(store: &Store, docs: &[Row], with_text: bool) {
    let page = u64::try_from(docs.len() / 4).unwrap();
    let mut checked = 0;
    for (filter, sort) in queries() {
        if has_text(&filter) && !with_text {
            continue;
        }
        let want = expected(docs, &filter, &sort);
        assert_eq!(
            paged(store, &filter, &sort, page),
            want,
            "find {filter} {sort:?}"
        );
        assert_eq!(
            store.count(DB, COLLECTION, &filter).unwrap(),
            u64::try_from(want.len()).unwrap(),
            "count {filter}"
        );
        for field in ["status", "tags"] {
            assert_eq!(
                store.distinct(DB, COLLECTION, field, &filter).unwrap(),
                distinct_expected(docs, &filter, field),
                "distinct {field} {filter}"
            );
        }
        checked += 1;
    }
    assert!(queries().len() >= 50);
    assert!(checked >= 42);
}

fn tree_hash(root: &Path) -> String {
    fn walk(dir: &Path, out: &mut Vec<u8>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.extend(path.to_string_lossy().as_bytes());
                out.extend(fs::read(&path).unwrap());
            }
        }
    }
    let mut bytes = Vec::new();
    walk(&root.join(DB), &mut bytes);
    version_of(&bytes)
}

fn insert(email: &str, index: usize, format: Format) -> Op {
    let mut doc = document(format, SEED, index);
    doc.insert("email".to_owned(), Value::String(email.to_owned()));
    Op::Insert {
        db: DB.to_owned(),
        coll: COLLECTION.to_owned(),
        id: Some(format!("extra-{index}")),
        doc,
    }
}

fn run(count: usize) {
    for format in [Format::Json, Format::Yaml, Format::Md] {
        let docs = generated(format, count);

        let indexed = TempDir::new().unwrap();
        let store = build_store(indexed.path(), format, count, SEED, true).unwrap();
        check(&store, &docs, true);

        let before = tree_hash(indexed.path());
        let error = store
            .apply(Tx::new(vec![
                insert("clash@example.test", count + 1, format),
                insert("clash@example.test", count + 2, format),
            ]))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::UniqueViolation);
        assert_eq!(tree_hash(indexed.path()), before);
        let clash = insert(docs[3].doc["email"].as_str().unwrap(), count + 3, format);
        let error = store
            .apply(Tx::new(vec![
                insert("fresh@example.test", count + 4, format),
                clash,
            ]))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::UniqueViolation);
        assert_eq!(tree_hash(indexed.path()), before);

        let plain = TempDir::new().unwrap();
        let store = build_store(plain.path(), format, count, SEED, false).unwrap();
        check(&store, &docs, format == Format::Md);
    }
}

#[test]
fn queries_match_a_brute_force_scan_on_two_thousand_documents() {
    run(2_000);
}

#[test]
#[ignore = "ten thousand documents per format"]
fn queries_match_a_brute_force_scan_on_ten_thousand_documents() {
    run(10_000);
}

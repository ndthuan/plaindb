mod cursor;
mod filter;
mod order;

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::codec::Doc;
use crate::config::Limits;
use crate::error::{Error, ErrorCode, Result};
use crate::snapshot::{CollectionSnapshot, DocEntry};
use crate::store::{Store, Version};

pub use filter::{Cond, Filter};
pub use order::{Key, compare};

pub const DEFAULT_LIMIT: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub db: String,
    pub coll: String,
    pub filter: Value,
    pub sort: Vec<(String, Dir)>,
    pub limit: Option<u64>,
    pub cursor: Option<String>,
    pub fields: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FindResult {
    pub docs: Vec<(Doc, Version)>,
    pub next_cursor: Option<String>,
}

fn matched<'a>(
    collection: &'a CollectionSnapshot,
    filter: &Filter,
    limits: &Limits,
) -> Result<Vec<&'a DocEntry>> {
    let started = Instant::now();
    let budget = limits.max_scan_ms.map(Duration::from_millis);
    let mut found = Vec::new();
    for (index, (_, entry)) in collection.readable_docs().enumerate() {
        if let Some(budget) = budget
            && index % 64 == 0
            && started.elapsed() > budget
        {
            return Err(Error::new(
                ErrorCode::LimitExceeded,
                "the scan took longer than the limit",
            )
            .with_detail("limit", "max_scan_ms")
            .with_detail("max", limits.max_scan_ms));
        }
        if filter.matches(&entry.doc) {
            found.push(entry);
        }
    }
    Ok(found)
}

fn sort_value(doc: &Map<String, Value>, path: &[String]) -> Value {
    let Some((first, rest)) = path.split_first() else {
        return Value::Null;
    };
    let mut current = doc.get(first);
    for part in rest {
        current = match current {
            Some(Value::Object(map)) => map.get(part),
            _ => None,
        };
    }
    current.cloned().unwrap_or(Value::Null)
}

fn project(doc: &Doc, fields: &[Vec<String>]) -> Doc {
    let mut out = Doc::new();
    if let Some(id) = doc.get("_id") {
        out.insert("_id".to_owned(), id.clone());
    }
    for path in fields {
        let Some((first, rest)) = path.split_first() else {
            continue;
        };
        let mut value = doc.get(first);
        for part in rest {
            value = match value {
                Some(Value::Object(map)) => map.get(part),
                _ => None,
            };
        }
        let Some(value) = value else { continue };
        let mut slot = out
            .entry(first.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if rest.is_empty() {
            *slot = value.clone();
            continue;
        }
        if !slot.is_object() {
            *slot = Value::Object(Map::new());
        }
        for (index, part) in rest.iter().enumerate() {
            let Value::Object(map) = slot else { break };
            let leaf = index + 1 == rest.len();
            slot = map.entry(part.clone()).or_insert_with(|| {
                if leaf {
                    value.clone()
                } else {
                    Value::Object(Map::new())
                }
            });
        }
    }
    out
}

fn compare_keys(left: &[Value], right: &[Value], sort: &[(Vec<String>, Dir)]) -> Ordering {
    for (index, (_, dir)) in sort.iter().enumerate() {
        let ordering = compare(&left[index], &right[index]);
        let ordering = match dir {
            Dir::Asc => ordering,
            Dir::Desc => ordering.reverse(),
        };
        if ordering.is_ne() {
            return ordering;
        }
    }
    compare(&left[sort.len()], &right[sort.len()])
}

fn entry_keys(entry: &DocEntry, sort: &[(Vec<String>, Dir)]) -> Vec<Value> {
    let mut keys: Vec<Value> = sort
        .iter()
        .map(|(path, _)| sort_value(&entry.doc, path))
        .collect();
    keys.push(entry.doc.get("_id").cloned().unwrap_or(Value::Null));
    keys
}

impl Store {
    pub fn find(&self, query: Query) -> Result<FindResult> {
        let filter = Filter::parse(&query.filter)?;
        let mut sort = Vec::new();
        for (path, dir) in &query.sort {
            sort.push((filter::split_path(path)?, *dir));
        }
        let fields = match &query.fields {
            Some(fields) => Some(
                fields
                    .iter()
                    .map(|path| filter::split_path(path))
                    .collect::<Result<Vec<_>>>()?,
            ),
            None => None,
        };
        let after = match &query.cursor {
            Some(text) => Some(cursor::decode(text, sort.len() + 1)?),
            None => None,
        };
        if query.limit == Some(0) {
            return Err(Error::new(
                ErrorCode::InvalidQuery,
                "the limit must be positive",
            ));
        }
        let cap = self.config.limits.max_results;
        let limit = query
            .limit
            .unwrap_or(DEFAULT_LIMIT)
            .min(cap.unwrap_or(u64::MAX));
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        self.with_collection(&query.db, &query.coll, |collection| {
            let found = matched(collection, &filter, &self.config.limits)?;
            let mut keyed: Vec<(Vec<Value>, &DocEntry)> = found
                .into_iter()
                .map(|entry| (entry_keys(entry, &sort), entry))
                .filter(|(keys, _)| {
                    after
                        .as_ref()
                        .is_none_or(|after| compare_keys(keys, after, &sort).is_gt())
                })
                .collect();
            keyed.sort_by(|left, right| compare_keys(&left.0, &right.0, &sort));
            let more = keyed.len() > limit;
            keyed.truncate(limit);
            let next_cursor = match keyed.last() {
                Some((keys, _)) if more => Some(cursor::encode(keys)),
                _ => None,
            };
            let docs = keyed
                .into_iter()
                .map(|(_, entry)| {
                    let doc = match &fields {
                        Some(fields) => project(&entry.doc, fields),
                        None => (*entry.doc).clone(),
                    };
                    (doc, entry.version.clone())
                })
                .collect();
            Ok(FindResult { docs, next_cursor })
        })
    }

    pub fn count(&self, db: &str, coll: &str, filter: &Value) -> Result<u64> {
        let filter = Filter::parse(filter)?;
        self.with_collection(db, coll, |collection| {
            let found = matched(collection, &filter, &self.config.limits)?;
            Ok(u64::try_from(found.len()).unwrap_or(u64::MAX))
        })
    }

    pub fn distinct(&self, db: &str, coll: &str, path: &str, filter: &Value) -> Result<Vec<Value>> {
        let filter = Filter::parse(filter)?;
        let path = filter::split_path(path)?;
        self.with_collection(db, coll, |collection| {
            let found = matched(collection, &filter, &self.config.limits)?;
            let mut values = BTreeSet::new();
            for entry in found {
                for value in filter::elements(&entry.doc, &path) {
                    values.insert(Key(value.clone()));
                }
            }
            Ok(values.into_iter().map(|key| key.0).collect())
        })
    }
}

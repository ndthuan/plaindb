use std::collections::BTreeSet;
use std::ops::Bound;

use super::filter::{Cond, Filter};
use crate::snapshot::CollectionSnapshot;

fn conjuncts<'a>(filter: &'a Filter, out: &mut Vec<&'a Filter>) {
    match filter {
        Filter::And(parts) => {
            for part in parts {
                conjuncts(part, out);
            }
        }
        other => out.push(other),
    }
}

fn lookup(collection: &CollectionSnapshot, path: &[String], cond: &Cond) -> Option<Vec<String>> {
    let index = collection.indexes.for_path(path)?;
    let mut ids = match cond {
        Cond::Eq(value) if !value.is_array() => index.exact(value),
        Cond::In(values) if values.iter().all(|value| !value.is_array()) => {
            values.iter().flat_map(|value| index.exact(value)).collect()
        }
        Cond::Gt(bound) => index.range(Bound::Excluded(bound.clone()), Bound::Unbounded),
        Cond::Gte(bound) => index.range(Bound::Included(bound.clone()), Bound::Unbounded),
        Cond::Lt(bound) => index.range(Bound::Unbounded, Bound::Excluded(bound.clone())),
        Cond::Lte(bound) => index.range(Bound::Unbounded, Bound::Included(bound.clone())),
        Cond::Prefix(prefix) => index.prefix(prefix),
        _ => return None,
    };
    ids.extend(index.unindexed());
    Some(ids)
}

pub(crate) fn candidates(collection: &CollectionSnapshot, filter: &Filter) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    conjuncts(filter, &mut parts);
    parts
        .into_iter()
        .find_map(|part| match part {
            Filter::Field { path, cond } => lookup(collection, path, cond),
            _ => None,
        })
        .map(|ids| {
            let unique: BTreeSet<String> = ids.into_iter().collect();
            unique.into_iter().collect()
        })
}

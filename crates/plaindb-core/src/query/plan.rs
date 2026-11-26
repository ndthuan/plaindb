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

pub(crate) struct Candidates {
    pub ids: Vec<String>,
    pub residual: Option<Filter>,
}

pub(crate) fn candidates(collection: &CollectionSnapshot, filter: &Filter) -> Option<Candidates> {
    let mut parts = Vec::new();
    conjuncts(filter, &mut parts);
    let (position, ids, exact) =
        parts
            .iter()
            .enumerate()
            .find_map(|(position, part)| match part {
                Filter::Field { path, cond } => {
                    lookup(collection, path, cond).map(|ids| (position, ids, false))
                }
                Filter::Text(query) => collection
                    .indexes
                    .text
                    .as_ref()
                    .map(|index| (position, index.matching(query), true)),
                _ => None,
            })?;
    let unique: BTreeSet<String> = ids.into_iter().collect();
    let residual = exact.then(|| {
        Filter::And(
            parts
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != position)
                .map(|(_, part)| (*part).clone())
                .collect(),
        )
    });
    Some(Candidates {
        ids: unique.into_iter().collect(),
        residual,
    })
}

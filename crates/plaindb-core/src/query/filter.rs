use serde_json::Value;

use super::order::compare;
use crate::error::{Error, ErrorCode, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Cond {
    Eq(Value),
    Ne(Value),
    Gt(Value),
    Gte(Value),
    Lt(Value),
    Lte(Value),
    In(Vec<Value>),
    Exists(bool),
    Prefix(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Field { path: Vec<String>, cond: Cond },
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidQuery, message)
}

fn unknown_operator(name: &str) -> Error {
    invalid(format!("unknown operator {name}")).with_detail("operator", name)
}

pub fn split_path(path: &str) -> Result<Vec<String>> {
    let parts: Vec<String> = path.split('.').map(str::to_owned).collect();
    if parts.iter().any(String::is_empty) {
        return Err(invalid(format!("invalid field path {path:?}")).with_detail("path", path));
    }
    Ok(parts)
}

impl Filter {
    pub fn parse(value: &Value) -> Result<Self> {
        match value {
            Value::Null => Ok(Self::And(Vec::new())),
            Value::Object(map) => {
                let mut parts = Vec::new();
                for (key, inner) in map {
                    parts.push(parse_entry(key, inner)?);
                }
                Ok(Self::And(parts))
            }
            _ => Err(invalid("a filter must be an object")),
        }
    }

    pub fn matches(&self, doc: &serde_json::Map<String, Value>) -> bool {
        match self {
            Self::And(parts) => parts.iter().all(|part| part.matches(doc)),
            Self::Or(parts) => parts.iter().any(|part| part.matches(doc)),
            Self::Field { path, cond } => cond.matches(doc, path),
        }
    }
}

fn parse_list(key: &str, value: &Value) -> Result<Vec<Filter>> {
    let Value::Array(items) = value else {
        return Err(invalid(format!("{key} needs an array of filters")));
    };
    if items.is_empty() {
        return Err(invalid(format!("{key} needs at least one filter")));
    }
    items.iter().map(Filter::parse).collect()
}

fn parse_entry(key: &str, value: &Value) -> Result<Filter> {
    match key {
        "$and" => Ok(Filter::And(parse_list(key, value)?)),
        "$or" => Ok(Filter::Or(parse_list(key, value)?)),
        "$text" => Err(invalid("text search needs a text index")),
        _ if key.starts_with('$') => Err(unknown_operator(key)),
        _ => parse_field(key, value),
    }
}

fn is_operator_object(map: &serde_json::Map<String, Value>) -> Result<bool> {
    let operators = map.keys().filter(|key| key.starts_with('$')).count();
    if operators == 0 {
        return Ok(false);
    }
    if operators != map.len() {
        return Err(invalid("an object cannot mix operators and plain keys"));
    }
    Ok(true)
}

fn parse_field(key: &str, value: &Value) -> Result<Filter> {
    let path = split_path(key)?;
    let Value::Object(map) = value else {
        return Ok(Filter::Field {
            path,
            cond: Cond::Eq(value.clone()),
        });
    };
    if !is_operator_object(map)? {
        return Ok(Filter::Field {
            path,
            cond: Cond::Eq(value.clone()),
        });
    }
    let mut parts = Vec::new();
    for (name, operand) in map {
        let cond = parse_cond(name, operand)?;
        parts.push(Filter::Field {
            path: path.clone(),
            cond,
        });
    }
    Ok(if parts.len() == 1 {
        parts.remove(0)
    } else {
        Filter::And(parts)
    })
}

fn parse_cond(name: &str, operand: &Value) -> Result<Cond> {
    match name {
        "$eq" => Ok(Cond::Eq(operand.clone())),
        "$ne" => Ok(Cond::Ne(operand.clone())),
        "$gt" => Ok(Cond::Gt(operand.clone())),
        "$gte" => Ok(Cond::Gte(operand.clone())),
        "$lt" => Ok(Cond::Lt(operand.clone())),
        "$lte" => Ok(Cond::Lte(operand.clone())),
        "$in" => match operand {
            Value::Array(items) => Ok(Cond::In(items.clone())),
            _ => Err(invalid("$in needs an array")),
        },
        "$exists" => match operand {
            Value::Bool(flag) => Ok(Cond::Exists(*flag)),
            _ => Err(invalid("$exists needs a boolean")),
        },
        "$prefix" => match operand {
            Value::String(text) => Ok(Cond::Prefix(text.clone())),
            _ => Err(invalid("$prefix needs a string")),
        },
        _ => Err(unknown_operator(name)),
    }
}

fn walk<'a>(value: &'a Value, path: &[String], out: &mut Vec<&'a Value>) {
    let Some((head, rest)) = path.split_first() else {
        out.push(value);
        return;
    };
    match value {
        Value::Object(map) => {
            if let Some(next) = map.get(head) {
                walk(next, rest, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, path, out);
            }
        }
        _ => {}
    }
}

pub(crate) fn terminals<'a>(
    doc: &'a serde_json::Map<String, Value>,
    path: &[String],
) -> Vec<&'a Value> {
    let mut out = Vec::new();
    if let Some((head, rest)) = path.split_first()
        && let Some(next) = doc.get(head)
    {
        walk(next, rest, &mut out);
    }
    out
}

pub(crate) fn elements<'a>(
    doc: &'a serde_json::Map<String, Value>,
    path: &[String],
) -> Vec<&'a Value> {
    let mut out = Vec::new();
    for value in terminals(doc, path) {
        match value {
            Value::Array(items) => out.extend(items.iter()),
            other => out.push(other),
        }
    }
    out
}

fn equals(left: &Value, right: &Value) -> bool {
    compare(left, right).is_eq()
}

fn matches_eq(doc: &serde_json::Map<String, Value>, path: &[String], wanted: &Value) -> bool {
    let found = terminals(doc, path);
    found.iter().any(|value| equals(value, wanted))
        || found.iter().any(|value| match value {
            Value::Array(items) => items.iter().any(|item| equals(item, wanted)),
            _ => false,
        })
}

impl Cond {
    pub(crate) fn matches(&self, doc: &serde_json::Map<String, Value>, path: &[String]) -> bool {
        match self {
            Self::Eq(wanted) => matches_eq(doc, path, wanted),
            Self::Ne(wanted) => !matches_eq(doc, path, wanted),
            Self::In(options) => options.iter().any(|wanted| matches_eq(doc, path, wanted)),
            Self::Exists(flag) => terminals(doc, path).is_empty() != *flag,
            Self::Gt(bound) => elements(doc, path)
                .iter()
                .any(|value| compare(value, bound).is_gt()),
            Self::Gte(bound) => elements(doc, path)
                .iter()
                .any(|value| compare(value, bound).is_ge()),
            Self::Lt(bound) => elements(doc, path)
                .iter()
                .any(|value| compare(value, bound).is_lt()),
            Self::Lte(bound) => elements(doc, path)
                .iter()
                .any(|value| compare(value, bound).is_le()),
            Self::Prefix(prefix) => elements(doc, path).iter().any(
                |value| matches!(value, Value::String(text) if text.starts_with(prefix.as_str())),
            ),
        }
    }
}

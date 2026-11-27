use std::collections::BTreeSet;

use serde_json::Value;

use crate::codec::Doc;
use crate::query::elements;
use crate::scan::SYSTEM_FIELDS;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextFields {
    All,
    Named(Vec<Vec<String>>),
}

pub fn tokens(text: &str) -> Vec<String> {
    let composed: String = text.nfc().collect();
    composed.unicode_words().map(str::to_lowercase).collect()
}

fn collect(value: &Value, into_objects: bool, out: &mut BTreeSet<String>) {
    match value {
        Value::String(text) => out.extend(tokens(text)),
        Value::Array(items) => {
            for item in items {
                collect(item, into_objects, out);
            }
        }
        Value::Object(map) if into_objects => {
            for item in map.values() {
                collect(item, into_objects, out);
            }
        }
        _ => {}
    }
}

pub fn doc_tokens(doc: &Doc, fields: &TextFields) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    match fields {
        TextFields::All => {
            for (key, value) in doc {
                if !SYSTEM_FIELDS.contains(&key.as_str()) {
                    collect(value, true, &mut out);
                }
            }
        }
        TextFields::Named(paths) => {
            for path in paths {
                for value in elements(doc, path) {
                    collect(value, false, &mut out);
                }
            }
        }
    }
    out
}

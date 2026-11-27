use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

use super::Dir;
use crate::error::{Error, ErrorCode, Result};
use crate::jsontext::sort_keys;
use crate::version::version_of;

const QUERY_HASH_CHARS: usize = 16;

fn invalid() -> Error {
    Error::new(
        ErrorCode::InvalidQuery,
        "the cursor is not valid for this query",
    )
    .with_detail("reason", "bad_cursor")
}

fn mismatch() -> Error {
    Error::new(
        ErrorCode::InvalidQuery,
        "the cursor belongs to another query",
    )
    .with_detail("reason", "cursor_mismatch")
}

pub fn query_hash(filter: &Value, sort: &[(String, Dir)]) -> String {
    let sort: Vec<Value> = sort
        .iter()
        .map(|(field, dir)| {
            let dir = match dir {
                Dir::Asc => "asc",
                Dir::Desc => "desc",
            };
            json!([field, dir])
        })
        .collect();
    let canonical = sort_keys(&json!({ "filter": filter, "sort": sort })).to_string();
    let mut hash = version_of(canonical.as_bytes());
    hash.truncate(QUERY_HASH_CHARS);
    hash
}

pub fn encode(keys: &[Value], query_hash: &str) -> String {
    URL_SAFE_NO_PAD.encode(json!({ "q": query_hash, "k": keys }).to_string())
}

pub fn decode(cursor: &str, width: usize, query_hash: &str) -> Result<Vec<Value>> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| invalid())?;
    let Ok(Value::Object(mut payload)) = serde_json::from_slice(&bytes) else {
        return Err(invalid());
    };
    let (Some(Value::String(hash)), Some(Value::Array(keys))) =
        (payload.remove("q"), payload.remove("k"))
    else {
        return Err(invalid());
    };
    if hash != query_hash {
        return Err(mismatch());
    }
    if keys.len() != width {
        return Err(invalid());
    }
    Ok(keys)
}

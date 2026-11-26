use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

use crate::error::{Error, ErrorCode, Result};

fn invalid() -> Error {
    Error::new(
        ErrorCode::InvalidQuery,
        "the cursor is not valid for this query",
    )
    .with_detail("reason", "bad_cursor")
}

pub fn encode(keys: &[Value]) -> String {
    URL_SAFE_NO_PAD.encode(Value::Array(keys.to_vec()).to_string())
}

pub fn decode(cursor: &str, width: usize) -> Result<Vec<Value>> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| invalid())?;
    match serde_json::from_slice(&bytes) {
        Ok(Value::Array(keys)) if keys.len() == width => Ok(keys),
        _ => Err(invalid()),
    }
}

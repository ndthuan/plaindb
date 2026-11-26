use serde_json::Value;

use super::{Codec, Doc};
use crate::error::{Error, Result};
use crate::jsontext::pretty_bytes;

pub struct JsonCodec;

impl Codec for JsonCodec {
    fn decode(&self, bytes: &[u8]) -> Result<Doc> {
        match serde_json::from_slice::<Value>(bytes) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) => Err(Error::invalid_document(
                "not_an_object",
                "a json document must be an object",
            )),
            Err(source) => Err(Error::invalid_document("invalid_json", source.to_string())),
        }
    }

    fn encode(&self, doc: &Doc) -> Result<Vec<u8>> {
        Ok(pretty_bytes(&Value::Object(doc.clone())))
    }
}

use serde_json::Value;

use super::{Codec, Doc, without_bom};
use crate::error::{Error, Result};
use crate::jsontext::sort_map;

pub struct YamlCodec;

pub(super) fn decode_text(text: &str) -> Result<Doc> {
    if text.trim().is_empty() {
        return Ok(Doc::new());
    }
    let mut options = serde_saphyr::Options::default();
    options.strict_booleans = true;
    options.leading_zero_numbers_as_strings = true;
    options.legacy_octal_numbers = false;
    match serde_saphyr::from_str_with_options::<Value>(text, options) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(Error::invalid_document(
            "not_a_mapping",
            "a yaml document must be a mapping",
        )),
        Err(source) => Err(Error::invalid_document("invalid_yaml", source.to_string())),
    }
}

pub(super) fn encode_text(doc: &Doc) -> Result<String> {
    if doc.is_empty() {
        return Ok(String::new());
    }
    let mut text = serde_saphyr::to_string(&Value::Object(sort_map(doc)))
        .map_err(|source| Error::invalid_document("unserialisable", source.to_string()))?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

impl Codec for YamlCodec {
    fn decode(&self, bytes: &[u8]) -> Result<Doc> {
        let text = std::str::from_utf8(without_bom(bytes))
            .map_err(|source| Error::invalid_document("not_utf8", source.to_string()))?;
        decode_text(text)
    }

    fn encode(&self, doc: &Doc) -> Result<Vec<u8>> {
        Ok(encode_text(doc)?.into_bytes())
    }
}

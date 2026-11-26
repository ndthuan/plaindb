use serde_json::Value;

use super::yaml::{decode_text, encode_text};
use super::{Codec, Doc};
use crate::error::{Error, Result};

pub struct MdCodec;

const FENCE: &[u8] = b"---";

fn is_fence_line(line: &[u8]) -> bool {
    line == FENCE || line == b"---\r"
}

fn split_frontmatter(bytes: &[u8]) -> Result<Option<(&[u8], &[u8])>> {
    let mut lines = bytes.split_inclusive(|byte| *byte == b'\n');
    let Some(first) = lines.next() else {
        return Ok(None);
    };
    if !is_fence_line(first.strip_suffix(b"\n").unwrap_or(first)) || !first.ends_with(b"\n") {
        return Ok(None);
    }
    let mut offset = first.len();
    for line in lines {
        let content = line.strip_suffix(b"\n").unwrap_or(line);
        if is_fence_line(content) {
            let end = offset + line.len();
            return Ok(Some((&bytes[first.len()..offset], &bytes[end..])));
        }
        offset += line.len();
    }
    Err(Error::invalid_document(
        "unterminated_frontmatter",
        "the frontmatter is not closed",
    ))
}

fn utf8(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes)
        .map_err(|source| Error::invalid_document("not_utf8", source.to_string()))
}

impl Codec for MdCodec {
    fn decode(&self, bytes: &[u8]) -> Result<Doc> {
        let (front, body) = match split_frontmatter(bytes)? {
            Some((front, body)) => (decode_text(utf8(front)?)?, body),
            None => (Doc::new(), bytes),
        };
        let mut doc = front;
        if doc.contains_key("body") {
            return Err(Error::invalid_document(
                "body_in_frontmatter",
                "the frontmatter must not hold a body field",
            ));
        }
        doc.insert("body".to_owned(), Value::String(utf8(body)?.to_owned()));
        Ok(doc)
    }

    fn encode(&self, doc: &Doc) -> Result<Vec<u8>> {
        let mut front = doc.clone();
        let body = match front.remove("body") {
            None => String::new(),
            Some(Value::String(text)) => text,
            Some(_) => {
                return Err(Error::invalid_document(
                    "body_not_string",
                    "the body field must be a string",
                ));
            }
        };
        let mut out = b"---\n".to_vec();
        out.extend_from_slice(encode_text(&front)?.as_bytes());
        out.extend_from_slice(b"---\n");
        out.extend_from_slice(body.as_bytes());
        Ok(out)
    }
}

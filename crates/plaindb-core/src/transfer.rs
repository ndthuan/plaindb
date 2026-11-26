use std::path::Path;

use crate::codec::{Doc, codec_for};
use crate::config::Format;
use crate::error::{Error, Result};

pub fn format_of_path(path: &Path) -> Option<Format> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "json" => Some(Format::Json),
        "yaml" | "yml" => Some(Format::Yaml),
        "md" => Some(Format::Md),
        _ => None,
    }
}

pub fn decode_file_bytes(path: &Path, bytes: &[u8]) -> Result<Doc> {
    let format = format_of_path(path).ok_or_else(|| {
        Error::invalid_document(
            "unsupported_extension",
            format!("{} is not a json, yaml or md file", path.display()),
        )
    })?;
    codec_for(format).decode(bytes)
}

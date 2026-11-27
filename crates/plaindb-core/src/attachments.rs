use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::codec::Doc;
use crate::error::{Error, ErrorCode, Result};
use crate::names::{validate_collection_name, validate_db_name, validate_file_name, validate_id};
use crate::scan;
use crate::store::Store;

pub const FILES_FIELD: &str = "_files";
pub const FILES_DIR: &str = "_files";
pub const DEFAULT_MIME: &str = "application/octet-stream";

const MIME_TYPES: [(&str, &str); 17] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("svg", "image/svg+xml"),
    ("pdf", "application/pdf"),
    ("mp3", "audio/mpeg"),
    ("m4a", "audio/mp4"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("mp4", "video/mp4"),
    ("webm", "video/webm"),
    ("txt", "text/plain"),
    ("csv", "text/csv"),
    ("json", "application/json"),
    ("zip", "application/zip"),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    pub size: u64,
    pub sha256: String,
    pub mime: String,
}

impl FileMeta {
    pub fn of(name: &str, bytes: &[u8], mime: Option<&str>) -> Self {
        Self {
            size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            sha256: sha256_hex(bytes),
            mime: mime.map_or_else(|| guess_mime(name).to_owned(), str::to_owned),
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({ "size": self.size, "sha256": self.sha256, "mime": self.mime })
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn guess_mime(name: &str) -> &'static str {
    let Some((_, extension)) = name.rsplit_once('.') else {
        return DEFAULT_MIME;
    };
    MIME_TYPES
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(extension))
        .map_or(DEFAULT_MIME, |(_, mime)| mime)
}

pub fn files_tree(db: &str, collection: &str, id: &str) -> String {
    format!("{db}/{collection}/{FILES_DIR}/{id}")
}

pub fn file_path(db: &str, collection: &str, id: &str, name: &str) -> String {
    format!("{}/{name}", files_tree(db, collection, id))
}

pub(crate) fn recorded_files(doc: &Doc, id: &str) -> Result<Map<String, Value>> {
    match doc.get(FILES_FIELD) {
        None => Ok(Map::new()),
        Some(Value::Object(files)) => Ok(files.clone()),
        Some(_) => Err(Error::invalid_document(
            "bad_files_field",
            format!("the {FILES_FIELD} field of document {id} is not an object"),
        )
        .with_detail("id", id)
        .with_detail("field", FILES_FIELD)),
    }
}

pub fn files_of(doc: &Doc) -> BTreeMap<String, Option<FileMeta>> {
    let Some(Value::Object(files)) = doc.get(FILES_FIELD) else {
        return BTreeMap::new();
    };
    files
        .iter()
        .map(|(name, meta)| (name.clone(), serde_json::from_value(meta.clone()).ok()))
        .collect()
}

pub(crate) fn changed_on_disk(id: &str, name: &str) -> Error {
    Error::new(
        ErrorCode::VersionConflict,
        format!("file {name} of document {id} changed on disk"),
    )
    .with_detail("id", id)
    .with_detail("name", name)
    .with_detail("reason", "changed_on_disk")
}

fn file_not_found(id: &str, name: &str) -> Error {
    Error::not_found("file", name).with_detail("id", id)
}

impl Store {
    pub fn read_file(
        &self,
        db: &str,
        collection: &str,
        id: &str,
        name: &str,
    ) -> Result<(FileMeta, Vec<u8>)> {
        validate_db_name(db)?;
        validate_collection_name(collection)?;
        validate_id(id)?;
        validate_file_name(name)?;
        scan::whole(&self.root, self.lock_timeout(), || {
            let (doc, _) = self
                .get(db, collection, id)?
                .ok_or_else(|| Error::not_found("document", id))?;
            let meta = files_of(&doc)
                .remove(name)
                .flatten()
                .ok_or_else(|| file_not_found(id, name))?;
            let path = self.root.join(file_path(db, collection, id, name));
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(source) if source.kind() == ErrorKind::NotFound => {
                    return Err(file_not_found(id, name));
                }
                Err(source) => return Err(source.into()),
            };
            if sha256_hex(&bytes) != meta.sha256 {
                return Err(changed_on_disk(id, name));
            }
            Ok((meta, bytes))
        })
    }

    pub fn list_files(
        &self,
        db: &str,
        collection: &str,
        id: &str,
    ) -> Result<BTreeMap<String, FileMeta>> {
        let (doc, _) = self
            .get(db, collection, id)?
            .ok_or_else(|| Error::not_found("document", id))?;
        Ok(files_of(&doc)
            .into_iter()
            .filter_map(|(name, meta)| meta.map(|meta| (name, meta)))
            .collect())
    }
}

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, Write};
use std::path::Path;

use serde_json::Value;

use crate::codec::{Doc, codec_for};
use crate::config::Format;
use crate::error::{Error, ErrorCode, Result};
use crate::names::{new_id, validate_id};
use crate::scan::{SYSTEM_FIELDS, compile_validator};
use crate::snapshot::CollectionSnapshot;
use crate::store::Store;
use crate::write::{Op, Tx};

pub const IMPORT_BATCH: usize = 500;
pub const VERSION_FIELD: &str = "_version";

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

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    pub replace: bool,
    pub actor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Line(u64),
    File(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Omitted {
    pub file: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub origin: Origin,
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: u64,
    pub omitted: Vec<Omitted>,
    pub failed: Vec<Failure>,
}

struct Candidate {
    origin: Origin,
    op: Op,
}

struct Importer<'a> {
    store: &'a Store,
    db: &'a str,
    coll: &'a str,
    options: &'a ImportOptions,
    snapshot: CollectionSnapshot,
    validator: Option<jsonschema::Validator>,
    taken: HashSet<String>,
    pending: Vec<Candidate>,
    report: ImportReport,
}

fn is_document_error(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::AlreadyExists
            | ErrorCode::VersionConflict
            | ErrorCode::InvalidName
            | ErrorCode::InvalidDocument
            | ErrorCode::SchemaViolation
            | ErrorCode::UniqueViolation
            | ErrorCode::LimitExceeded
    )
}

impl<'a> Importer<'a> {
    fn new(
        store: &'a Store,
        db: &'a str,
        coll: &'a str,
        options: &'a ImportOptions,
    ) -> Result<Self> {
        let snapshot = store.with_collection(db, coll, |found| Ok(found.clone()))?;
        let dir = store.collection_dir(db, coll)?;
        let validator = compile_validator(&snapshot.meta, &dir)?;
        let taken = snapshot
            .files
            .values()
            .map(|entry| entry.id.to_ascii_lowercase())
            .collect();
        Ok(Self {
            store,
            db,
            coll,
            options,
            snapshot,
            validator,
            taken,
            pending: Vec::new(),
            report: ImportReport::default(),
        })
    }

    fn fail(&mut self, origin: Origin, error: &Error) {
        self.report.failed.push(Failure {
            origin,
            code: error.code,
            message: error.message.clone(),
        });
    }

    fn resolve_id(explicit: Option<&str>, doc: &Doc) -> Result<String> {
        let inside = match doc.get("_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text.as_str()),
            Some(_) => {
                return Err(Error::invalid_document(
                    "id_not_text",
                    "the _id of a document must be text",
                ));
            }
        };
        match (explicit, inside) {
            (Some(file), Some(given)) if file != given => Err(Error::invalid_document(
                "id_mismatch",
                format!("the _id differs from the file name {file}"),
            )),
            (Some(id), _) | (None, Some(id)) => Ok(id.to_owned()),
            (None, None) => Ok(new_id()),
        }
    }

    fn check(&self, id: &str, doc: &Doc) -> Result<()> {
        validate_id(id)?;
        let folded = id.to_ascii_lowercase();
        if !self.options.replace {
            let exists = self
                .snapshot
                .files
                .contains_key(&self.snapshot.file_name(id));
            if exists || self.taken.contains(&folded) {
                return Err(Error::new(
                    ErrorCode::AlreadyExists,
                    format!("document {id} already exists"),
                )
                .with_detail("id", id)
                .with_detail("reason", "exists"));
            }
        }
        if let Some(validator) = &self.validator {
            let mut checked = doc.clone();
            for field in SYSTEM_FIELDS {
                checked.remove(field);
            }
            let errors: Vec<String> = validator
                .iter_errors(&Value::Object(checked))
                .map(|error| error.to_string())
                .collect();
            if !errors.is_empty() {
                return Err(Error::new(
                    ErrorCode::SchemaViolation,
                    format!("document {id} does not match the collection schema"),
                )
                .with_detail("id", id)
                .with_detail("errors", errors));
            }
        }
        Ok(())
    }

    fn offer(&mut self, origin: Origin, explicit: Option<&str>, mut doc: Doc) -> Result<()> {
        doc.remove(VERSION_FIELD);
        let id = Self::resolve_id(explicit, &doc);
        let checked = id.and_then(|id| self.check(&id, &doc).map(|()| id));
        let id = match checked {
            Ok(id) => id,
            Err(error) => {
                self.fail(origin, &error);
                return Ok(());
            }
        };
        self.taken.insert(id.to_ascii_lowercase());
        let (db, coll) = (self.db.to_owned(), self.coll.to_owned());
        let op = if self.options.replace {
            Op::Put {
                db,
                coll,
                id,
                doc,
                if_version: None,
            }
        } else {
            Op::Insert {
                db,
                coll,
                id: Some(id),
                doc,
            }
        };
        self.pending.push(Candidate { origin, op });
        if self.pending.len() >= IMPORT_BATCH {
            self.flush()?;
        }
        Ok(())
    }

    fn write(&mut self, batch: Vec<Candidate>) -> Result<()> {
        let ops = batch.iter().map(|entry| entry.op.clone()).collect();
        let tx = Tx {
            id: None,
            actor: self.options.actor.clone(),
            ops,
            keep_timestamps: true,
        };
        match self.store.apply(tx) {
            Ok(_) => {
                self.report.imported += u64::try_from(batch.len()).unwrap_or(u64::MAX);
                Ok(())
            }
            Err(error) if is_document_error(error.code) => {
                if batch.len() == 1 {
                    let origin = batch.into_iter().next().map(|entry| entry.origin);
                    if let Some(origin) = origin {
                        self.fail(origin, &error);
                    }
                    return Ok(());
                }
                let mut left = batch;
                let right = left.split_off(left.len() / 2);
                self.write(left)?;
                self.write(right)
            }
            Err(error) => Err(error),
        }
    }

    fn flush(&mut self) -> Result<()> {
        let batch = std::mem::take(&mut self.pending);
        if batch.is_empty() {
            return Ok(());
        }
        self.write(batch)
    }

    fn finish(mut self) -> Result<ImportReport> {
        self.flush()?;
        Ok(self.report)
    }
}

impl Store {
    pub fn import_jsonl(
        &self,
        db: &str,
        coll: &str,
        reader: impl BufRead,
        options: &ImportOptions,
    ) -> Result<ImportReport> {
        let mut importer = Importer::new(self, db, coll, options)?;
        for (index, line) in reader.split(b'\n').enumerate() {
            let number = u64::try_from(index + 1).unwrap_or(u64::MAX);
            let origin = Origin::Line(number);
            let bytes = line?;
            let decoded = std::str::from_utf8(&bytes)
                .map_err(|source| Error::invalid_document("not_utf8", source.to_string()))
                .and_then(|text| {
                    if text.trim().is_empty() {
                        return Ok(None);
                    }
                    match serde_json::from_str::<Value>(text) {
                        Ok(Value::Object(doc)) => Ok(Some(doc)),
                        Ok(_) => Err(Error::invalid_document(
                            "not_an_object",
                            "a line must hold one JSON object",
                        )),
                        Err(source) => Err(Error::invalid_document("bad_json", source.to_string())),
                    }
                });
            match decoded {
                Ok(Some(doc)) => importer.offer(origin, None, doc)?,
                Ok(None) => {}
                Err(error) => importer.fail(origin, &error),
            }
        }
        importer.finish()
    }

    pub fn import_dir(
        &self,
        db: &str,
        coll: &str,
        dir: &Path,
        options: &ImportOptions,
    ) -> Result<ImportReport> {
        let mut importer = Importer::new(self, db, coll, options)?;
        let mut paths = Vec::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_file() && format_of_path(&path).is_some() {
                paths.push(path);
            }
        }
        paths.sort();
        for path in paths {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let stem = path.file_stem().and_then(|stem| stem.to_str());
            let Some(stem) = stem.filter(|stem| validate_id(stem).is_ok()) else {
                importer.report.omitted.push(Omitted {
                    file: name,
                    reason: ErrorCode::InvalidName.as_str().to_owned(),
                });
                continue;
            };
            let decoded = fs::read(&path)
                .map_err(Error::from)
                .and_then(|bytes| decode_file_bytes(&path, &bytes));
            match decoded {
                Ok(doc) => importer.offer(Origin::File(name), Some(stem), doc)?,
                Err(error) => importer.fail(Origin::File(name), &error),
            }
        }
        importer.finish()
    }

    pub fn export_jsonl(&self, db: &str, coll: &str, mut out: impl Write) -> Result<u64> {
        let mut docs = self.with_collection(db, coll, |snapshot| {
            Ok(snapshot
                .readable_docs()
                .map(|(_, entry)| (entry.doc.clone(), entry.version.clone()))
                .collect::<Vec<_>>())
        })?;
        docs.sort_by(|(left, _), (right, _)| {
            let key = |doc: &Doc| {
                doc.get("_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            key(left).cmp(&key(right))
        });
        let mut written = 0;
        for (doc, version) in docs {
            let mut line = (*doc).clone();
            line.insert(VERSION_FIELD.to_owned(), Value::String(version));
            serde_json::to_writer(&mut out, &line)
                .map_err(|source| Error::from(std::io::Error::other(source)))?;
            out.write_all(b"\n")?;
            written += 1;
        }
        out.flush()?;
        Ok(written)
    }
}

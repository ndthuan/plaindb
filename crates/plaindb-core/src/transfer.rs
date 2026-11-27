use std::collections::HashSet;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::Path;

use serde_json::Value;

use crate::codec::{Doc, codec_for, without_bom};
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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
    pub code: String,
    pub message: String,
    pub details: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aborted {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: u64,
    pub omitted: Vec<Omitted>,
    pub failed: Vec<Failure>,
    pub aborted: Option<Aborted>,
}

pub const DUPLICATE_IN_INPUT: &str = "duplicate_in_input";
const DEFAULT_LINE_LIMIT: u64 = 64 * 1024 * 1024;

struct Candidate {
    origin: Origin,
    folded: String,
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
    seen: HashSet<String>,
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

const TIMESTAMP_FIELDS: [&str; 2] = ["_created_at", "_updated_at"];

fn check_timestamps(doc: &Doc) -> Result<()> {
    for field in TIMESTAMP_FIELDS {
        let Some(value) = doc.get(field) else {
            continue;
        };
        let parses = value
            .as_str()
            .is_some_and(|text| text.parse::<jiff::Timestamp>().is_ok());
        if !parses {
            return Err(Error::invalid_document(
                "bad_timestamp",
                format!("{field} must be an RFC 3339 timestamp"),
            )
            .with_detail("field", field));
        }
    }
    Ok(())
}

fn op_index(error: &Error, len: usize) -> Option<usize> {
    error
        .details
        .get("op_index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .filter(|index| *index < len)
}

fn without_op_index(error: &Error) -> Error {
    let mut error = error.clone();
    if let Value::Object(details) = &mut error.details {
        details.remove("op_index");
    }
    error
}

enum Line {
    Read,
    TooLong,
}

fn read_line(
    reader: &mut impl BufRead,
    limit: u64,
    buffer: &mut Vec<u8>,
) -> io::Result<Option<Line>> {
    buffer.clear();
    let read = reader
        .by_ref()
        .take(limit.saturating_add(1))
        .read_until(b'\n', buffer)?;
    if read == 0 {
        return Ok(None);
    }
    if buffer.last() == Some(&b'\n') {
        buffer.pop();
        return Ok(Some(Line::Read));
    }
    if u64::try_from(buffer.len()).unwrap_or(u64::MAX) <= limit {
        return Ok(Some(Line::Read));
    }
    buffer.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        if let Some(end) = available.iter().position(|byte| *byte == b'\n') {
            reader.consume(end + 1);
            break;
        }
        let skipped = available.len();
        reader.consume(skipped);
    }
    Ok(Some(Line::TooLong))
}

fn decode_line(bytes: &[u8]) -> Result<Option<Doc>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|source| Error::invalid_document("not_utf8", source.to_string()))?;
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
            seen: HashSet::new(),
            pending: Vec::new(),
            report: ImportReport::default(),
        })
    }

    fn fail(&mut self, origin: Origin, error: &Error) {
        self.report.failed.push(Failure {
            origin,
            code: error.code.as_str().to_owned(),
            message: error.message.clone(),
            details: error.details.clone(),
        });
    }

    fn fail_as_duplicate(&mut self, origin: Origin, id: &str) {
        self.report.failed.push(Failure {
            origin,
            code: DUPLICATE_IN_INPUT.to_owned(),
            message: "the id occurs earlier in this input".to_owned(),
            details: serde_json::json!({ "id": id }),
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
        let id = match (explicit, inside) {
            (Some(file), Some(given)) if file != given => {
                return Err(Error::invalid_document(
                    "id_mismatch",
                    format!("the _id differs from the file name {file}"),
                ));
            }
            (Some(id), _) | (None, Some(id)) => id.to_owned(),
            (None, None) => new_id(),
        };
        validate_id(&id)?;
        Ok(id)
    }

    fn check(&self, id: &str, folded: &str, doc: &Doc) -> Result<()> {
        check_timestamps(doc)?;
        if !self.options.replace {
            let exists = self
                .snapshot
                .files
                .contains_key(&self.snapshot.file_name(id));
            if exists || self.taken.contains(folded) {
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
        let id = match Self::resolve_id(explicit, &doc) {
            Ok(id) => id,
            Err(error) => {
                self.fail(origin, &error);
                return Ok(());
            }
        };
        let folded = id.to_ascii_lowercase();
        if !self.seen.insert(folded.clone()) {
            self.fail_as_duplicate(origin, &id);
            return Ok(());
        }
        if let Err(error) = self.check(&id, &folded, &doc) {
            self.fail(origin, &error);
            return Ok(());
        }
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
        self.pending.push(Candidate { origin, folded, op });
        if self.pending.len() >= IMPORT_BATCH {
            self.flush()?;
        }
        Ok(())
    }

    fn write(&mut self, mut batch: Vec<Candidate>) -> Result<()> {
        loop {
            if batch.is_empty() {
                return Ok(());
            }
            let tx = Tx {
                id: None,
                actor: self.options.actor.clone(),
                ops: batch.iter().map(|entry| entry.op.clone()).collect(),
                keep_timestamps: true,
            };
            let error = match self.store.apply(tx) {
                Ok(_) => {
                    self.report.imported += u64::try_from(batch.len()).unwrap_or(u64::MAX);
                    self.taken
                        .extend(batch.into_iter().map(|entry| entry.folded));
                    return Ok(());
                }
                Err(error) => error,
            };
            if !is_document_error(error.code) {
                return Err(error);
            }
            if let Some(index) = op_index(&error, batch.len()) {
                let refused = batch.remove(index);
                self.fail(refused.origin, &without_op_index(&error));
                continue;
            }
            if batch.len() == 1 {
                let refused = batch.remove(0);
                self.fail(refused.origin, &error);
                return Ok(());
            }
            let right = batch.split_off(batch.len() / 2);
            self.write(batch)?;
            return self.write(right);
        }
    }

    fn flush(&mut self) -> Result<()> {
        let batch = std::mem::take(&mut self.pending);
        self.write(batch)
    }

    fn read_lines(&mut self, mut reader: impl BufRead, limit: u64) -> Result<()> {
        let mut buffer = Vec::new();
        let mut number: u64 = 0;
        while let Some(line) = read_line(&mut reader, limit, &mut buffer)? {
            number += 1;
            let origin = Origin::Line(number);
            if matches!(line, Line::TooLong) {
                let error = Error::new(
                    ErrorCode::LimitExceeded,
                    format!("the line is longer than {limit} bytes"),
                )
                .with_detail("limit", "max_document_bytes")
                .with_detail("max", limit);
                self.fail(origin, &error);
                continue;
            }
            let bytes = if number == 1 {
                without_bom(&buffer)
            } else {
                &buffer
            };
            match decode_line(bytes) {
                Ok(Some(doc)) => self.offer(origin, None, doc)?,
                Ok(None) => {}
                Err(error) => self.fail(origin, &error),
            }
        }
        Ok(())
    }

    fn read_files(&mut self, entries: fs::ReadDir) -> Result<()> {
        let mut paths = Vec::new();
        for entry in entries {
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
                self.report.omitted.push(Omitted {
                    file: name,
                    reason: ErrorCode::InvalidName.as_str().to_owned(),
                });
                continue;
            };
            let decoded = fs::read(&path)
                .map_err(Error::from)
                .and_then(|bytes| decode_file_bytes(&path, &bytes));
            match decoded {
                Ok(doc) => self.offer(Origin::File(name), Some(stem), doc)?,
                Err(error) => self.fail(Origin::File(name), &error),
            }
        }
        Ok(())
    }

    fn conclude(mut self, outcome: Result<()>) -> ImportReport {
        if let Err(error) = outcome.and_then(|()| self.flush()) {
            self.report.aborted = Some(Aborted {
                code: error.code.as_str().to_owned(),
                message: error.message,
            });
        }
        self.report
            .failed
            .sort_by(|left, right| left.origin.cmp(&right.origin));
        self.report
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
        let limit = self
            .config()
            .limits
            .max_document_bytes
            .unwrap_or(DEFAULT_LINE_LIMIT);
        let outcome = importer.read_lines(reader, limit);
        Ok(importer.conclude(outcome))
    }

    pub fn import_dir(
        &self,
        db: &str,
        coll: &str,
        dir: &Path,
        options: &ImportOptions,
    ) -> Result<ImportReport> {
        let mut importer = Importer::new(self, db, coll, options)?;
        let entries = fs::read_dir(dir)?;
        let outcome = importer.read_files(entries);
        Ok(importer.conclude(outcome))
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

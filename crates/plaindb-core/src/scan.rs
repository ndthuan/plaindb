use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use rpds::{RedBlackTreeMapSync, RedBlackTreeSetSync};
use serde_json::Value;

use crate::codec::{Doc, codec_for};
use crate::config::{CollectionMeta, STORE_FORMAT, from_file_bytes};
use crate::error::{Error, ErrorCode, Result};
use crate::index::Indexes;
use crate::journal::{self, JOURNAL_FILE};
use crate::lock::ReadLock;
use crate::names::validate_id;
use crate::snapshot::{
    CollectionSnapshot, DbSnapshot, DocEntry, FileEntry, Invalid, Outcome, Snapshot,
};
use crate::store::{DB_FILE, META_FILE, STATE_DIR, child_directories};
use crate::txlog::{FileStamp, file_stamp};
use crate::version::version_of;

pub(crate) const SYSTEM_FIELDS: [&str; 3] = ["_id", "_created_at", "_updated_at"];
pub const GENERATION_FILE: &str = "generation";

const CHECKED_ATTEMPTS: usize = 5;

#[derive(Debug, PartialEq, Eq)]
struct Mark {
    generation: u64,
    journal: Option<FileStamp>,
    unfinished: bool,
}

fn mark(root: &Path) -> Result<Mark> {
    Ok(Mark {
        generation: read_generation(root)?,
        journal: file_stamp(&root.join(STATE_DIR).join(JOURNAL_FILE))?,
        unfinished: journal::unfinished_count(root)? > 0,
    })
}

pub(crate) fn checked<T>(root: &Path, scan: impl Fn() -> Result<T>) -> Result<T> {
    let mut previous = None;
    for _ in 0..CHECKED_ATTEMPTS {
        let before = mark(root)?;
        let result = scan()?;
        let after = mark(root)?;
        if before == after && (!after.unfinished || previous.as_ref() == Some(&after)) {
            return Ok(result);
        }
        previous = Some(after);
    }
    Err(Error::new(
        ErrorCode::StoreLockedTimeout,
        "the store kept changing while it was read",
    )
    .with_detail("reason", "store_moving"))
}

pub(crate) fn whole<T>(root: &Path, timeout: Duration, scan: impl Fn() -> Result<T>) -> Result<T> {
    match ReadLock::acquire(root, timeout)? {
        Some(_lock) => scan(),
        None => checked(root, scan),
    }
}

pub fn full_scan(root: &Path) -> Result<Snapshot> {
    rescan(root, &Snapshot::default())
}

pub fn read_generation(root: &Path) -> Result<u64> {
    match fs::read_to_string(root.join(STATE_DIR).join(GENERATION_FILE)) {
        Ok(text) => Ok(text.trim().parse().unwrap_or(0)),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(0),
        Err(source) => Err(source.into()),
    }
}

pub fn rescan(root: &Path, prev: &Snapshot) -> Result<Snapshot> {
    let generation = read_generation(root)?;
    let mut dbs = RedBlackTreeMapSync::new_sync();
    for db in child_directories(root, DB_FILE, crate::names::validate_db_name)? {
        check_db_file(&root.join(&db).join(DB_FILE))?;
        let prev_db = prev.dbs.get(&db);
        let mut collections = RedBlackTreeMapSync::new_sync();
        for collection in child_directories(
            &root.join(&db),
            META_FILE,
            crate::names::validate_collection_name,
        )? {
            let prev_collection = prev_db.and_then(|entry| entry.collections.get(&collection));
            let scanned = scan_collection(&root.join(&db).join(&collection), prev_collection)?;
            collections.insert_mut(collection, scanned);
        }
        dbs.insert_mut(db, DbSnapshot { collections });
    }
    Ok(Snapshot { generation, dbs })
}

fn check_db_file(path: &Path) -> Result<()> {
    let value: Value = serde_json::from_slice(&fs::read(path)?).map_err(|source| {
        Error::invalid_document("invalid_db_file", source.to_string())
            .with_detail("path", path.display().to_string())
    })?;
    let format = value.get("format").and_then(Value::as_u64).unwrap_or(1);
    if format > u64::from(STORE_FORMAT) {
        return Err(Error::new(
            ErrorCode::UnsupportedFormat,
            "the db format is newer than this build",
        )
        .with_detail("found", format)
        .with_detail("supported", STORE_FORMAT));
    }
    Ok(())
}

fn read_meta(dir: &Path) -> Result<CollectionMeta> {
    let path = dir.join(META_FILE);
    from_file_bytes(&fs::read(&path)?).map_err(|source| {
        Error::invalid_document("invalid_meta", source.message)
            .with_detail("path", path.display().to_string())
    })
}

pub(crate) fn compile_validator(
    meta: &CollectionMeta,
    dir: &Path,
) -> Result<Option<jsonschema::Validator>> {
    match &meta.schema {
        Some(schema) => Ok(Some(jsonschema::validator_for(schema).map_err(
            |source| {
                Error::invalid_document("invalid_schema", source.to_string())
                    .with_detail("path", dir.join(META_FILE).display().to_string())
            },
        )?)),
        None => Ok(None),
    }
}

#[cfg(unix)]
fn inode_of(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn inode_of(_metadata: &fs::Metadata) -> u64 {
    0
}

pub(crate) fn file_entry(
    path: &Path,
    stem: &str,
    meta: &CollectionMeta,
    validator: Option<&jsonschema::Validator>,
    bytes: &[u8],
    metadata: &fs::Metadata,
) -> FileEntry {
    let mut entry = FileEntry {
        id: stem.to_owned(),
        size: 0,
        mtime_ns: 0,
        inode: 0,
        outcome: load(path, stem, meta, validator, bytes),
    };
    stamp(&mut entry, metadata);
    entry
}

pub(crate) fn stamp(entry: &mut FileEntry, metadata: &fs::Metadata) {
    entry.size = metadata.len();
    entry.mtime_ns = modified_ns(metadata);
    entry.inode = inode_of(metadata);
}

fn modified_ns(metadata: &fs::Metadata) -> i128 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|elapsed| i128::try_from(elapsed.as_nanos()).ok())
        .unwrap_or(0)
}

fn scan_collection(dir: &Path, prev: Option<&CollectionSnapshot>) -> Result<CollectionSnapshot> {
    let meta = read_meta(dir)?;
    let reusable = prev.filter(|snapshot| snapshot.meta == meta);
    let validator = compile_validator(&meta, dir)?;
    let suffix = format!(".{}", meta.format.extension());
    let mut files = RedBlackTreeMapSync::new_sync();
    let mut changed = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') || name == META_FILE {
            continue;
        }
        let Some(stem) = name.strip_suffix(&suffix) else {
            continue;
        };
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == ErrorKind::NotFound => continue,
            Err(source) => return Err(source.into()),
        };
        let previous = reusable
            .and_then(|snapshot| snapshot.files.get(&name))
            .filter(|known| {
                known.size == metadata.len()
                    && known.mtime_ns == modified_ns(&metadata)
                    && known.inode == inode_of(&metadata)
            });
        let file_entry = match previous {
            Some(known) => known.clone(),
            None => {
                changed.push(name.clone());
                let path = entry.path();
                let bytes = match fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(source) if source.kind() == ErrorKind::NotFound => continue,
                    Err(source) => return Err(source.into()),
                };
                file_entry(&path, stem, &meta, validator.as_ref(), &bytes, &metadata)
            }
        };
        files.insert_mut(name, file_entry);
    }
    let case_duplicates = find_case_duplicates(&files);
    let indexes = match reusable {
        Some(known) => {
            let mut indexes = known.indexes.clone();
            for (name, old) in known.files.iter() {
                if !files.contains_key(name) {
                    indexes.apply(Some(old), None);
                }
            }
            for name in &changed {
                if let Some(new) = files.get(name) {
                    indexes.apply(known.files.get(name), Some(new));
                }
            }
            indexes
        }
        None => Indexes::build(&meta, files.values())?,
    };
    Ok(CollectionSnapshot {
        meta,
        files,
        case_duplicates,
        indexes,
    })
}

pub(crate) fn find_case_duplicates(
    files: &RedBlackTreeMapSync<String, FileEntry>,
) -> RedBlackTreeSetSync<String> {
    let mut groups: HashMap<String, Vec<&String>> = HashMap::new();
    for (name, entry) in files.iter() {
        if matches!(entry.outcome, Outcome::BadName { .. }) {
            continue;
        }
        groups
            .entry(entry.id.to_ascii_lowercase())
            .or_default()
            .push(name);
    }
    let mut duplicates = RedBlackTreeSetSync::new_sync();
    for names in groups.into_values().filter(|names| names.len() > 1) {
        for name in names {
            duplicates.insert_mut(name.clone());
        }
    }
    duplicates
}

fn invalid(path: &Path, reason: impl Into<String>) -> Outcome {
    Outcome::Invalid(Invalid {
        path: path.to_path_buf(),
        reason: reason.into(),
    })
}

fn load(
    path: &Path,
    stem: &str,
    meta: &CollectionMeta,
    validator: Option<&jsonschema::Validator>,
    bytes: &[u8],
) -> Outcome {
    if validate_id(stem).is_err() {
        return Outcome::BadName {
            path: path.to_path_buf(),
        };
    }
    let mut doc: Doc = match codec_for(meta.format).decode(bytes) {
        Ok(doc) => doc,
        Err(error) => return invalid(path, error.message),
    };
    if let Some(validator) = validator {
        let mut checked = doc.clone();
        for field in SYSTEM_FIELDS {
            checked.remove(field);
        }
        if !validator.is_valid(&Value::Object(checked)) {
            return invalid(path, "schema");
        }
    }
    let stored_id = doc.get("_id").cloned();
    let id_mismatch = matches!(&stored_id, Some(value) if value.as_str() != Some(stem));
    if stored_id.is_none() || id_mismatch {
        doc.insert("_id".to_owned(), Value::String(stem.to_owned()));
    }
    Outcome::Doc(DocEntry {
        version: version_of(bytes),
        doc: Arc::new(doc),
        id_mismatch,
    })
}

pub(crate) struct FileRef<'a> {
    pub db: &'a str,
    pub collection: &'a str,
    pub file_name: &'a str,
}

fn read_present(path: &Path) -> Result<Option<(fs::Metadata, Vec<u8>)>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(source.into()),
    };
    match fs::read(path) {
        Ok(bytes) => Ok(Some((metadata, bytes))),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(source.into()),
    }
}

pub(crate) fn refresh_files(
    root: &Path,
    prev: &Snapshot,
    files: &[FileRef<'_>],
) -> Result<Snapshot> {
    let mut next = prev.clone();
    for file in files {
        let refreshed =
            next.update_collection(file.db, file.collection, |collection| -> Result<()> {
                let dir = root.join(file.db).join(file.collection);
                let path = dir.join(file.file_name);
                let suffix = format!(".{}", collection.meta.format.extension());
                let Some(stem) = file.file_name.strip_suffix(&suffix) else {
                    return Ok(());
                };
                match read_present(&path)? {
                    Some((metadata, bytes)) => {
                        let validator = compile_validator(&collection.meta, &dir)?;
                        let entry = file_entry(
                            &path,
                            stem,
                            &collection.meta,
                            validator.as_ref(),
                            &bytes,
                            &metadata,
                        );
                        collection.put_file(file.file_name, Some(entry));
                    }
                    None => collection.put_file(file.file_name, None),
                }
                collection.case_duplicates = find_case_duplicates(&collection.files);
                Ok(())
            });
        refreshed.transpose()?;
    }
    Ok(next)
}

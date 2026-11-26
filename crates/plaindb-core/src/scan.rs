use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use rpds::{RedBlackTreeMapSync, RedBlackTreeSetSync};
use serde_json::Value;

use crate::codec::{Doc, codec_for};
use crate::config::{CollectionMeta, STORE_FORMAT, from_file_bytes};
use crate::error::{Error, ErrorCode, Result};
use crate::names::validate_id;
use crate::snapshot::{
    CollectionSnapshot, DbSnapshot, DocEntry, FileEntry, Invalid, Outcome, Snapshot,
};
use crate::store::{DB_FILE, META_FILE, STATE_DIR, child_directories};
use crate::version::version_of;

const SYSTEM_FIELDS: [&str; 3] = ["_id", "_created_at", "_updated_at"];
const GENERATION_FILE: &str = "generation";

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
    let validator = match &meta.schema {
        Some(schema) => Some(jsonschema::validator_for(schema).map_err(|source| {
            Error::invalid_document("invalid_schema", source.to_string())
                .with_detail("path", dir.join(META_FILE).display().to_string())
        })?),
        None => None,
    };
    let suffix = format!(".{}", meta.format.extension());
    let mut files = RedBlackTreeMapSync::new_sync();
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
        let size = metadata.len();
        let mtime_ns = modified_ns(&metadata);
        let previous = reusable
            .and_then(|snapshot| snapshot.files.get(&name))
            .filter(|known| known.size == size && known.mtime_ns == mtime_ns);
        let file_entry = match previous {
            Some(known) => known.clone(),
            None => {
                let path = entry.path();
                let bytes = match fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(source) if source.kind() == ErrorKind::NotFound => continue,
                    Err(source) => return Err(source.into()),
                };
                FileEntry {
                    id: stem.to_owned(),
                    size,
                    mtime_ns,
                    outcome: load(&path, stem, &meta, validator.as_ref(), &bytes),
                }
            }
        };
        files.insert_mut(name, file_entry);
    }
    let case_duplicates = find_case_duplicates(&files);
    Ok(CollectionSnapshot {
        meta,
        files,
        case_duplicates,
    })
}

fn find_case_duplicates(
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

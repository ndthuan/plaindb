use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rpds::RedBlackTreeMapSync;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::atomic::write_atomic_at;
use crate::codec::Doc;
use crate::config::{CollectionMeta, Durability, from_file_bytes};
use crate::error::{Error, ErrorCode, Result};
use crate::index::{IndexDump, Indexes};
use crate::names::{validate_collection_name, validate_db_name};
use crate::scan::find_case_duplicates;
use crate::snapshot::{
    CollectionSnapshot, DbSnapshot, DocEntry, FileEntry, Invalid, Outcome, Snapshot,
};
use crate::store::{DB_FILE, META_FILE, STATE_DIR, child_directories};

pub const CACHE_FILE: &str = "index-cache.bin";
const CACHE_FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Header {
    format: u32,
    generation: u64,
    meta_hash: String,
}

#[derive(Serialize)]
struct BodyRef<'a> {
    collections: Vec<CollectionRef<'a>>,
}

#[derive(Serialize)]
struct CollectionRef<'a> {
    db: &'a str,
    collection: &'a str,
    files: Vec<FileRef<'a>>,
    indexes: IndexDump,
}

#[derive(Serialize)]
struct FileRef<'a> {
    name: &'a str,
    id: &'a str,
    size: u64,
    mtime_ns: i64,
    inode: u64,
    outcome: OutcomeRef<'a>,
}

#[derive(Serialize)]
enum OutcomeRef<'a> {
    Doc {
        version: &'a str,
        doc: &'a Doc,
        id_mismatch: bool,
    },
    Invalid {
        reason: &'a str,
    },
    BadName,
}

#[derive(Deserialize)]
struct Body {
    collections: Vec<CollectionOwned>,
}

#[derive(Deserialize)]
struct CollectionOwned {
    db: String,
    collection: String,
    files: Vec<FileOwned>,
    indexes: IndexDump,
}

#[derive(Deserialize)]
struct FileOwned {
    name: String,
    id: String,
    size: u64,
    mtime_ns: i64,
    inode: u64,
    outcome: OutcomeOwned,
}

#[derive(Deserialize)]
enum OutcomeOwned {
    Doc {
        version: String,
        doc: Doc,
        id_mismatch: bool,
    },
    Invalid {
        reason: String,
    },
    BadName,
}

struct DiskMeta {
    db: String,
    collection: String,
    meta: CollectionMeta,
}

fn read_metas(root: &Path) -> Result<(String, Vec<DiskMeta>)> {
    let mut hasher = Sha1::new();
    let mut metas = Vec::new();
    for db in child_directories(root, DB_FILE, validate_db_name)? {
        for collection in child_directories(&root.join(&db), META_FILE, validate_collection_name)? {
            let path = root.join(&db).join(&collection).join(META_FILE);
            let bytes = fs::read(&path)?;
            hasher.update(format!("{db}/{collection}\0{}\0", bytes.len()).as_bytes());
            hasher.update(&bytes);
            metas.push(DiskMeta {
                db: db.clone(),
                collection,
                meta: from_file_bytes(&bytes)?,
            });
        }
    }
    let digest = hasher.finalize();
    let hash = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok((hash, metas))
}

fn cache_path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(CACHE_FILE)
}

pub fn stored_generation(root: &Path) -> Option<u64> {
    let bytes = fs::read(cache_path(root)).ok()?;
    let header: Header = rmp_serde::decode::from_read(Cursor::new(bytes)).ok()?;
    (header.format == CACHE_FORMAT).then_some(header.generation)
}

pub fn discard(root: &Path) {
    let _ = fs::remove_file(cache_path(root));
}

pub(crate) enum CacheRead {
    Missing,
    Unusable { discard: bool },
    Usable(Snapshot),
}

pub(crate) fn read(root: &Path) -> CacheRead {
    let Ok(bytes) = fs::read(cache_path(root)) else {
        return CacheRead::Missing;
    };
    let mut reader = Cursor::new(bytes);
    let Ok(header) = rmp_serde::decode::from_read::<_, Header>(&mut reader) else {
        return CacheRead::Unusable { discard: true };
    };
    if header.format != CACHE_FORMAT {
        return CacheRead::Unusable { discard: false };
    }
    let Ok((hash, metas)) = read_metas(root) else {
        return CacheRead::Unusable { discard: false };
    };
    if hash != header.meta_hash {
        return CacheRead::Unusable { discard: false };
    }
    let Ok(body) = rmp_serde::decode::from_read::<_, Body>(&mut reader) else {
        return CacheRead::Unusable { discard: true };
    };
    if reader.position() != reader.get_ref().len() as u64 {
        return CacheRead::Unusable { discard: true };
    }
    match assemble(root, header.generation, body, metas) {
        Some(snapshot) => CacheRead::Usable(snapshot),
        None => CacheRead::Unusable { discard: true },
    }
}

pub fn load(root: &Path) -> Option<Snapshot> {
    match read(root) {
        CacheRead::Usable(snapshot) => Some(snapshot),
        CacheRead::Unusable { discard: true } => {
            reject(root);
            None
        }
        CacheRead::Unusable { discard: false } | CacheRead::Missing => None,
    }
}

fn reject(root: &Path) {
    tracing::warn!("the index cache is unreadable and was discarded");
    discard(root);
}

fn assemble(root: &Path, generation: u64, body: Body, metas: Vec<DiskMeta>) -> Option<Snapshot> {
    let mut cached: HashMap<(String, String), (Vec<FileOwned>, IndexDump)> = body
        .collections
        .into_iter()
        .map(|entry| ((entry.db, entry.collection), (entry.files, entry.indexes)))
        .collect();
    let mut dbs: RedBlackTreeMapSync<String, DbSnapshot> = RedBlackTreeMapSync::new_sync();
    for disk in metas {
        let dir = root.join(&disk.db).join(&disk.collection);
        let mut files = RedBlackTreeMapSync::new_sync();
        let key = (disk.db.clone(), disk.collection.clone());
        let mut ids = Vec::new();
        let mut dump = None;
        if let Some((cached_files, cached_dump)) = cached.remove(&key) {
            ids = cached_files.iter().map(|file| file.id.clone()).collect();
            for file in cached_files {
                files.insert_mut(file.name.clone(), file_entry(file, &dir));
            }
            dump = Some(cached_dump);
        }
        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let indexes = match dump {
            Some(dump) => Indexes::restore(&disk.meta, dump, &id_refs)?,
            None => Indexes::build(&disk.meta, files.values()).ok()?,
        };
        let case_duplicates = find_case_duplicates(&files);
        let mut db_snapshot = dbs.get(&disk.db).cloned().unwrap_or_default();
        db_snapshot.collections.insert_mut(
            disk.collection,
            CollectionSnapshot {
                meta: disk.meta,
                files,
                case_duplicates,
                indexes,
            },
        );
        dbs.insert_mut(disk.db, db_snapshot);
    }
    Some(Snapshot { generation, dbs })
}

fn file_entry(file: FileOwned, dir: &Path) -> FileEntry {
    let outcome = match file.outcome {
        OutcomeOwned::Doc {
            version,
            doc,
            id_mismatch,
        } => Outcome::Doc(DocEntry {
            version,
            doc: Arc::new(doc),
            id_mismatch,
        }),
        OutcomeOwned::Invalid { reason } => Outcome::Invalid(Invalid {
            path: dir.join(&file.name),
            reason,
        }),
        OutcomeOwned::BadName => Outcome::BadName {
            path: dir.join(&file.name),
        },
    };
    FileEntry {
        id: file.id,
        size: file.size,
        mtime_ns: i128::from(file.mtime_ns),
        inode: file.inode,
        outcome,
    }
}

fn file_ref<'a>(name: &'a str, entry: &'a FileEntry) -> FileRef<'a> {
    let outcome = match &entry.outcome {
        Outcome::Doc(found) => OutcomeRef::Doc {
            version: &found.version,
            doc: &found.doc,
            id_mismatch: found.id_mismatch,
        },
        Outcome::Invalid(invalid) => OutcomeRef::Invalid {
            reason: &invalid.reason,
        },
        Outcome::BadName { .. } => OutcomeRef::BadName,
    };
    FileRef {
        name,
        id: &entry.id,
        size: entry.size,
        mtime_ns: i64::try_from(entry.mtime_ns).unwrap_or(0),
        inode: entry.inode,
        outcome,
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    rmp_serde::to_vec(value).map_err(|source| Error::new(ErrorCode::Io, source.to_string()))
}

pub fn write(root: &Path, snapshot: &Snapshot) -> Result<()> {
    let (meta_hash, metas) = read_metas(root)?;
    let mut collections = Vec::new();
    for disk in &metas {
        let Some(collection) = snapshot
            .dbs
            .get(&disk.db)
            .and_then(|db| db.collections.get(&disk.collection))
        else {
            return Ok(());
        };
        if collection.meta != disk.meta {
            return Ok(());
        }
        let lookup: HashMap<&str, u32> = collection
            .files
            .values()
            .zip(0..)
            .map(|(entry, ordinal)| (entry.id.as_str(), ordinal))
            .collect();
        collections.push(CollectionRef {
            db: &disk.db,
            collection: &disk.collection,
            files: collection
                .files
                .iter()
                .map(|(name, entry)| file_ref(name, entry))
                .collect(),
            indexes: collection.indexes.dump(&lookup),
        });
    }
    let header = Header {
        format: CACHE_FORMAT,
        generation: snapshot.generation,
        meta_hash,
    };
    let mut bytes = encode(&header)?;
    bytes.extend(encode(&BodyRef { collections })?);
    write_atomic_at(&cache_path(root), &bytes, Durability::Off)
}

pub fn same_files(left: &Snapshot, right: &Snapshot) -> bool {
    left.dbs.size() == right.dbs.size()
        && left.dbs.iter().all(|(db, left_db)| {
            right.dbs.get(db).is_some_and(|right_db| {
                left_db.collections.size() == right_db.collections.size()
                    && left_db.collections.iter().all(|(name, left_collection)| {
                        right_db
                            .collections
                            .get(name)
                            .is_some_and(|right_collection| {
                                same_collection(left_collection, right_collection)
                            })
                    })
            })
        })
}

fn same_collection(left: &CollectionSnapshot, right: &CollectionSnapshot) -> bool {
    left.meta == right.meta
        && left.files.size() == right.files.size()
        && left.files.iter().all(|(name, left_entry)| {
            right
                .files
                .get(name)
                .is_some_and(|right_entry| same_entry(left_entry, right_entry))
        })
}

fn same_entry(left: &FileEntry, right: &FileEntry) -> bool {
    left.size == right.size
        && left.mtime_ns == right.mtime_ns
        && left.inode == right.inode
        && match (&left.outcome, &right.outcome) {
            (Outcome::Doc(a), Outcome::Doc(b)) => Arc::ptr_eq(&a.doc, &b.doc),
            (Outcome::Invalid(a), Outcome::Invalid(b)) => a == b,
            (Outcome::BadName { path: a }, Outcome::BadName { path: b }) => a == b,
            _ => false,
        }
}

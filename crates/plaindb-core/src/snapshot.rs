use std::path::PathBuf;
use std::sync::Arc;

use rpds::{RedBlackTreeMapSync, RedBlackTreeSetSync};
use serde_json::Value;

use crate::codec::Doc;
use crate::config::{CollectionMeta, Format};
use crate::git::GitStatus;
use crate::index::Indexes;

#[derive(Debug, Clone, PartialEq)]
pub struct DocEntry {
    pub version: String,
    pub doc: Arc<Doc>,
    pub id_mismatch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Doc(DocEntry),
    Invalid(Invalid),
    BadName { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    pub id: String,
    pub size: u64,
    pub mtime_ns: i128,
    pub inode: u64,
    pub outcome: Outcome,
}

#[derive(Debug, Clone)]
pub struct CollectionSnapshot {
    pub meta: CollectionMeta,
    pub files: RedBlackTreeMapSync<String, FileEntry>,
    pub case_duplicates: RedBlackTreeSetSync<String>,
    pub indexes: Indexes,
    pub skipped: Vec<String>,
    pub skipped_count: u64,
}

impl CollectionSnapshot {
    pub fn file_name(&self, id: &str) -> String {
        format!("{id}.{}", self.meta.format.extension())
    }

    pub fn format(&self) -> Format {
        self.meta.format
    }

    pub fn is_duplicate(&self, file_name: &str) -> bool {
        self.case_duplicates.contains(file_name)
    }

    pub fn readable(&self, id: &str) -> Option<&DocEntry> {
        let name = self.file_name(id);
        match &self.files.get(&name)?.outcome {
            Outcome::Doc(doc) if !self.is_duplicate(&name) => Some(doc),
            _ => None,
        }
    }

    pub fn readable_docs(&self) -> impl Iterator<Item = (&String, &DocEntry)> {
        self.files
            .iter()
            .filter_map(|(name, entry)| match &entry.outcome {
                Outcome::Doc(doc) if !self.is_duplicate(name) => Some((name, doc)),
                _ => None,
            })
    }
}

#[derive(Debug, Clone, Default)]
pub struct DbSnapshot {
    pub collections: RedBlackTreeMapSync<String, CollectionSnapshot>,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub generation: u64,
    pub dbs: RedBlackTreeMapSync<String, DbSnapshot>,
}

impl Snapshot {
    pub(crate) fn update_collection<T>(
        &mut self,
        db: &str,
        collection: &str,
        update: impl FnOnce(&mut CollectionSnapshot) -> T,
    ) -> Option<T> {
        let mut db_snapshot = self.dbs.get(db)?.clone();
        let mut collection_snapshot = db_snapshot.collections.get(collection)?.clone();
        let outcome = update(&mut collection_snapshot);
        db_snapshot
            .collections
            .insert_mut(collection.to_owned(), collection_snapshot);
        self.dbs.insert_mut(db.to_owned(), db_snapshot);
        Some(outcome)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidFile {
    pub db: String,
    pub collection: String,
    pub id: String,
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub code: String,
    pub db: String,
    pub collection: String,
    pub path: PathBuf,
    pub details: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub invalid: Vec<InvalidFile>,
    pub warnings: Vec<Warning>,
    pub documents: u64,
    pub store_bytes: u64,
    pub git: GitStatus,
}

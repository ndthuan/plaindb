use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::ErrorKind;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::codec::{Doc, codec_for};
use crate::error::{Error, ErrorCode, Result};
use crate::git::{self, PushOutcome};
use crate::index::PendingWrite;
use crate::journal::{self, JournalEntry};
use crate::lock::WriteLock;
use crate::names::{new_id, validate_collection_name, validate_db_name, validate_id};
use crate::scan::{self, FileRef, SYSTEM_FIELDS, compile_validator, find_case_duplicates};
use crate::snapshot::{CollectionSnapshot, DocEntry, FileEntry, Outcome, Snapshot};
use crate::store::{Store, read_config};
use crate::txlog;
use crate::version::version_of;

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Insert {
        db: String,
        coll: String,
        id: Option<String>,
        doc: Doc,
    },
    Put {
        db: String,
        coll: String,
        id: String,
        doc: Doc,
        if_version: Option<String>,
    },
    Patch {
        db: String,
        coll: String,
        id: String,
        patch: Value,
        if_version: Option<String>,
    },
    Delete {
        db: String,
        coll: String,
        id: String,
        if_version: Option<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tx {
    pub id: Option<String>,
    pub actor: Option<String>,
    pub ops: Vec<Op>,
    pub keep_timestamps: bool,
}

impl Tx {
    pub fn new(ops: Vec<Op>) -> Self {
        Self {
            ops,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpResult {
    pub id: String,
    pub version: Option<String>,
    #[serde(default)]
    pub previous_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxResult {
    pub tx_id: String,
    #[serde(default)]
    pub generation: u64,
    pub results: Vec<OpResult>,
    #[serde(skip)]
    pub push: Option<PushOutcome>,
}

pub fn now_timestamp() -> String {
    format!("{:.3}", jiff::Timestamp::now())
}

fn validate_tx_id(tx_id: &str) -> Result<()> {
    ulid::Ulid::from_string(tx_id)
        .map(|_| ())
        .map_err(|_| Error::invalid_name("tx_id", tx_id))
}

#[derive(Debug, Clone)]
enum Current {
    Missing,
    Present { doc: Arc<Doc>, version: String },
    Refused(String),
}

impl Current {
    fn version(&self) -> Option<String> {
        match self {
            Self::Present { version, .. } => Some(version.clone()),
            Self::Missing | Self::Refused(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
struct Pending {
    bytes: Vec<u8>,
    doc: Arc<Doc>,
    version: String,
}

#[derive(Debug, Clone)]
struct Target {
    db: String,
    collection: String,
    file_name: String,
    id: String,
    base: Option<String>,
    next: Option<Pending>,
}

impl Target {
    fn relative_path(&self) -> String {
        format!("{}/{}/{}", self.db, self.collection, self.file_name)
    }
}

type TargetKey = (String, String, String);

struct Planner<'a> {
    snapshot: &'a Snapshot,
    max_document_bytes: Option<u64>,
    keep_timestamps: bool,
    now: String,
    targets: BTreeMap<TargetKey, Target>,
    validators: HashMap<(String, String), Option<Arc<jsonschema::Validator>>>,
    folded: HashMap<(String, String), HashMap<String, BTreeSet<String>>>,
    root: &'a std::path::Path,
}

fn refused(id: &str, reason: &str) -> Error {
    Error::invalid_document(reason, format!("document {id} is invalid"))
        .with_detail("id", id)
        .with_detail("reason", reason)
}

fn version_conflict(id: &str, expected: &str, actual: Option<&str>) -> Error {
    Error::new(
        ErrorCode::VersionConflict,
        format!("document {id} is not at the expected version"),
    )
    .with_detail("id", id)
    .with_detail("expected", expected)
    .with_detail("actual", actual.map_or(Value::Null, |text| json!(text)))
}

fn check_version(id: &str, if_version: Option<&String>, current: &Current) -> Result<()> {
    let Some(expected) = if_version else {
        return Ok(());
    };
    let actual = match current {
        Current::Present { version, .. } => Some(version.as_str()),
        _ => None,
    };
    if actual == Some(expected.as_str()) {
        Ok(())
    } else {
        Err(version_conflict(id, expected, actual))
    }
}

fn check_given_id(doc: &Doc, id: &str) -> Result<()> {
    match doc.get("_id") {
        Some(given) if given.as_str() != Some(id) => Err(Error::invalid_document(
            "id_mismatch",
            format!("the document's _id differs from {id}"),
        )
        .with_detail("id", id)),
        _ => Ok(()),
    }
}

fn not_found(id: &str) -> Error {
    Error::not_found("document", id)
}

impl<'a> Planner<'a> {
    fn collection(&self, db: &str, collection: &str) -> Result<&'a CollectionSnapshot> {
        validate_db_name(db)?;
        validate_collection_name(collection)?;
        let db_snapshot = self
            .snapshot
            .dbs
            .get(db)
            .ok_or_else(|| Error::not_found("db", db))?;
        db_snapshot
            .collections
            .get(collection)
            .ok_or_else(|| Error::not_found("collection", collection))
    }

    fn current(&self, db: &str, collection: &str, id: &str) -> Result<(Current, String)> {
        let snapshot = self.collection(db, collection)?;
        let file_name = snapshot.file_name(id);
        let key = (db.to_owned(), collection.to_owned(), file_name.clone());
        if let Some(target) = self.targets.get(&key) {
            let current = match &target.next {
                Some(pending) => Current::Present {
                    doc: pending.doc.clone(),
                    version: pending.version.clone(),
                },
                None => Current::Missing,
            };
            return Ok((current, file_name));
        }
        let current = match snapshot.files.get(&file_name) {
            None => Current::Missing,
            Some(_) if snapshot.is_duplicate(&file_name) => {
                Current::Refused("duplicate_id_case".to_owned())
            }
            Some(entry) => match &entry.outcome {
                Outcome::Doc(found) => Current::Present {
                    doc: found.doc.clone(),
                    version: found.version.clone(),
                },
                Outcome::Invalid(invalid) => Current::Refused(invalid.reason.clone()),
                Outcome::BadName { .. } => Current::Missing,
            },
        };
        Ok((current, file_name))
    }

    fn folded_ids(
        &mut self,
        db: &str,
        collection: &str,
    ) -> Result<&mut HashMap<String, BTreeSet<String>>> {
        let snapshot = self.collection(db, collection)?;
        let key = (db.to_owned(), collection.to_owned());
        Ok(self.folded.entry(key).or_insert_with(|| {
            let mut ids: HashMap<String, BTreeSet<String>> = HashMap::new();
            for entry in snapshot.files.values() {
                if matches!(entry.outcome, Outcome::BadName { .. }) {
                    continue;
                }
                ids.entry(entry.id.to_ascii_lowercase())
                    .or_default()
                    .insert(entry.id.clone());
            }
            ids
        }))
    }

    fn check_case(&mut self, db: &str, collection: &str, id: &str) -> Result<()> {
        let folded = self.folded_ids(db, collection)?;
        let clash = folded
            .get(&id.to_ascii_lowercase())
            .and_then(|ids| ids.iter().find(|existing| existing.as_str() != id).cloned());
        match clash {
            Some(existing) => Err(Error::new(
                ErrorCode::AlreadyExists,
                format!("document {existing} differs from {id} only in letter case"),
            )
            .with_detail("id", id)
            .with_detail("existing", existing)
            .with_detail("reason", "case")),
            None => Ok(()),
        }
    }

    fn validator(
        &mut self,
        db: &str,
        collection: &str,
    ) -> Result<Option<Arc<jsonschema::Validator>>> {
        let key = (db.to_owned(), collection.to_owned());
        if let Some(found) = self.validators.get(&key) {
            return Ok(found.clone());
        }
        let snapshot = self.collection(db, collection)?;
        let dir = self.root.join(db).join(collection);
        let compiled = compile_validator(&snapshot.meta, &dir)?.map(Arc::new);
        self.validators.insert(key, compiled.clone());
        Ok(compiled)
    }

    fn finalize(&self, mut doc: Doc, id: &str, existing: Option<&Doc>) -> Doc {
        let given_created = doc.remove("_created_at");
        let given_updated = doc.remove("_updated_at");
        doc.insert("_id".to_owned(), Value::String(id.to_owned()));
        let created = match existing {
            Some(existing) => existing.get("_created_at").cloned(),
            None => Some(Value::String(self.now.clone())),
        };
        let mut updated = Some(Value::String(self.now.clone()));
        let mut created = created;
        if self.keep_timestamps {
            if given_created.is_some() {
                created = given_created;
            }
            if given_updated.is_some() {
                updated = given_updated;
            }
        }
        if let Some(created) = created {
            doc.insert("_created_at".to_owned(), created);
        }
        if let Some(updated) = updated {
            doc.insert("_updated_at".to_owned(), updated);
        }
        doc
    }

    fn encode(&mut self, db: &str, collection: &str, id: &str, doc: &Doc) -> Result<Pending> {
        if let Some(validator) = self.validator(db, collection)? {
            let mut checked = doc.clone();
            for field in SYSTEM_FIELDS {
                checked.remove(field);
            }
            let checked = Value::Object(checked);
            let errors: Vec<String> = validator
                .iter_errors(&checked)
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
        let format = self.collection(db, collection)?.format();
        let codec = codec_for(format);
        let bytes = codec.encode(doc)?;
        if let Some(max) = self.max_document_bytes {
            let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            if size > max {
                return Err(Error::new(
                    ErrorCode::LimitExceeded,
                    format!("document {id} is larger than the limit"),
                )
                .with_detail("id", id)
                .with_detail("limit", "max_document_bytes")
                .with_detail("max", max)
                .with_detail("bytes", size));
            }
        }
        let stored = codec.decode(&bytes)?;
        Ok(Pending {
            version: version_of(&bytes),
            doc: Arc::new(stored),
            bytes,
        })
    }

    fn stage(
        &mut self,
        db: &str,
        collection: &str,
        file_name: String,
        id: &str,
        next: Option<Pending>,
    ) -> Result<()> {
        let key = (db.to_owned(), collection.to_owned(), file_name.clone());
        let snapshot = self.collection(db, collection)?;
        let base = match snapshot.files.get(&file_name).map(|entry| &entry.outcome) {
            Some(Outcome::Doc(found)) => Some(found.version.clone()),
            _ => None,
        };
        let present = next.is_some();
        self.targets
            .entry(key)
            .and_modify(|target| target.next = next.clone())
            .or_insert_with(|| Target {
                db: db.to_owned(),
                collection: collection.to_owned(),
                file_name,
                id: id.to_owned(),
                base,
                next,
            });
        let folded = self.folded_ids(db, collection)?;
        let ids = folded.entry(id.to_ascii_lowercase()).or_default();
        if present {
            ids.insert(id.to_owned());
        } else {
            ids.remove(id);
        }
        Ok(())
    }

    fn plan(&mut self, op: &Op) -> Result<OpResult> {
        match op {
            Op::Insert { db, coll, id, doc } => {
                let id = match (id, doc.get("_id")) {
                    (Some(id), _) => id.clone(),
                    (None, Some(Value::String(given))) => given.clone(),
                    (None, _) => new_id(),
                };
                validate_id(&id)?;
                check_given_id(doc, &id)?;
                let (current, file_name) = self.current(db, coll, &id)?;
                match current {
                    Current::Refused(reason) => return Err(refused(&id, &reason)),
                    Current::Present { .. } => {
                        return Err(Error::new(
                            ErrorCode::AlreadyExists,
                            format!("document {id} already exists"),
                        )
                        .with_detail("id", id.as_str())
                        .with_detail("reason", "exists"));
                    }
                    Current::Missing => self.check_case(db, coll, &id)?,
                }
                let next = self.finalize(doc.clone(), &id, None);
                let pending = self.encode(db, coll, &id, &next)?;
                let version = pending.version.clone();
                self.stage(db, coll, file_name, &id, Some(pending))?;
                Ok(OpResult {
                    id,
                    version: Some(version),
                    previous_version: None,
                })
            }
            Op::Put {
                db,
                coll,
                id,
                doc,
                if_version,
            } => {
                validate_id(id)?;
                check_given_id(doc, id)?;
                let (current, file_name) = self.current(db, coll, id)?;
                if let Current::Refused(reason) = &current {
                    return Err(refused(id, reason));
                }
                check_version(id, if_version.as_ref(), &current)?;
                let existing = match &current {
                    Current::Present { doc, .. } => Some(doc.clone()),
                    _ => {
                        self.check_case(db, coll, id)?;
                        None
                    }
                };
                let next = self.finalize(doc.clone(), id, existing.as_deref());
                let pending = self.encode(db, coll, id, &next)?;
                let version = pending.version.clone();
                self.stage(db, coll, file_name, id, Some(pending))?;
                Ok(OpResult {
                    id: id.clone(),
                    version: Some(version),
                    previous_version: current.version(),
                })
            }
            Op::Patch {
                db,
                coll,
                id,
                patch,
                if_version,
            } => {
                validate_id(id)?;
                let (current, file_name) = self.current(db, coll, id)?;
                let existing = match &current {
                    Current::Refused(reason) => return Err(refused(id, reason)),
                    Current::Missing => return Err(not_found(id)),
                    Current::Present { doc, .. } => doc.clone(),
                };
                check_version(id, if_version.as_ref(), &current)?;
                let mut merged = Value::Object((*existing).clone());
                json_patch::merge(&mut merged, patch);
                let Value::Object(merged) = merged else {
                    return Err(Error::invalid_document(
                        "not_an_object",
                        "the patched document is not an object",
                    )
                    .with_detail("id", id.as_str()));
                };
                let next = self.finalize(merged, id, Some(&existing));
                let pending = self.encode(db, coll, id, &next)?;
                let version = pending.version.clone();
                self.stage(db, coll, file_name, id, Some(pending))?;
                Ok(OpResult {
                    id: id.clone(),
                    version: Some(version),
                    previous_version: current.version(),
                })
            }
            Op::Delete {
                db,
                coll,
                id,
                if_version,
            } => {
                validate_id(id)?;
                let (current, file_name) = self.current(db, coll, id)?;
                match &current {
                    Current::Refused(reason) => return Err(refused(id, reason)),
                    Current::Missing => return Err(not_found(id)),
                    Current::Present { .. } => {}
                }
                check_version(id, if_version.as_ref(), &current)?;
                self.stage(db, coll, file_name, id, None)?;
                Ok(OpResult {
                    id: id.clone(),
                    version: None,
                    previous_version: current.version(),
                })
            }
        }
    }
}

fn check_indexes(snapshot: &Snapshot, targets: &[Target]) -> Result<()> {
    let mut groups: BTreeMap<(&str, &str), Vec<PendingWrite<'_>>> = BTreeMap::new();
    for target in targets {
        let doc = target.next.as_ref().map(|pending| &*pending.doc);
        groups
            .entry((&target.db, &target.collection))
            .or_default()
            .push((&target.id, doc));
    }
    for ((db, collection), writes) in groups {
        let found = snapshot
            .dbs
            .get(db)
            .and_then(|entry| entry.collections.get(collection));
        if let Some(found) = found {
            found.indexes.check_writes(db, collection, &writes)?;
        }
    }
    Ok(())
}

fn read_version(path: &std::path::Path) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(version_of(&bytes))),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(source.into()),
    }
}

impl Store {
    pub(crate) fn begin_write(&self) -> Result<WriteLock> {
        let lock = WriteLock::acquire(&self.root, self.lock_timeout())?;
        journal::recover(&self.root)?;
        let foreign = git::prepare_write(&self.root)?;
        let current = self.snapshot.load_full();
        if foreign || scan::read_generation(&self.root)? != current.generation {
            self.snapshot
                .store(Arc::new(scan::rescan(&self.root, &current)?));
        }
        Ok(lock)
    }

    pub fn apply(&self, tx: Tx) -> Result<TxResult> {
        let before = self.push_head();
        let mut result = self.apply_locked(tx)?;
        result.push = self.push_after(before);
        Ok(result)
    }

    fn apply_locked(&self, tx: Tx) -> Result<TxResult> {
        let supplied = tx.id.is_some();
        let tx_id = match tx.id {
            Some(id) => {
                validate_tx_id(&id)?;
                id
            }
            None => new_id(),
        };
        let _lock = self.begin_write()?;
        if supplied && let Some(found) = txlog::find(&self.root, &tx_id)? {
            return Ok(found);
        }
        let snapshot = self.snapshot.load_full();
        let mut planner = Planner {
            snapshot: &snapshot,
            max_document_bytes: self.config.limits.max_document_bytes,
            keep_timestamps: tx.keep_timestamps,
            now: now_timestamp(),
            targets: BTreeMap::new(),
            validators: HashMap::new(),
            folded: HashMap::new(),
            root: &self.root,
        };
        let mut results = Vec::with_capacity(tx.ops.len());
        for (index, op) in tx.ops.iter().enumerate() {
            let result = planner
                .plan(op)
                .map_err(|error| error.with_detail("op_index", index))?;
            results.push(result);
        }
        let targets: Vec<Target> = planner.targets.into_values().collect();
        if targets.is_empty() {
            return Ok(TxResult {
                tx_id,
                generation: snapshot.generation,
                results,
                push: None,
            });
        }
        check_indexes(&snapshot, &targets)?;
        self.check_disk(&snapshot, &targets)?;
        let generation = snapshot.generation + 1;
        let result = TxResult {
            tx_id,
            generation,
            results,
            push: None,
        };
        let entries = targets
            .iter()
            .map(|target| match &target.next {
                Some(pending) => {
                    JournalEntry::write(target.relative_path(), &pending.bytes, target.base.clone())
                }
                None => JournalEntry::remove(target.relative_path(), target.base.clone()),
            })
            .collect();
        let plan = git::plan_tx(
            &self.root,
            &read_config(&self.root)?,
            &result.tx_id,
            tx.actor.as_deref(),
            &tx.ops,
            &result.results,
        )?;
        let mut journal = plan.into_journal(result.tx_id.clone(), generation, entries);
        journal.result = Some(result.clone());
        journal::commit(&self.root, &journal)?;
        self.publish(&snapshot, generation, &targets)?;
        Ok(result)
    }

    fn check_disk(&self, snapshot: &Snapshot, targets: &[Target]) -> Result<()> {
        let mut changed = Vec::new();
        for target in targets {
            let path = self.root.join(target.relative_path());
            if read_version(&path)? != target.base {
                changed.push(target);
            }
        }
        let Some(first) = changed.first() else {
            return Ok(());
        };
        let files: Vec<FileRef<'_>> = changed
            .iter()
            .map(|target| FileRef {
                db: &target.db,
                collection: &target.collection,
                file_name: &target.file_name,
            })
            .collect();
        let refreshed = scan::refresh_files(&self.root, snapshot, &files)?;
        self.snapshot.store(Arc::new(refreshed));
        let ids: Vec<&str> = changed.iter().map(|target| target.id.as_str()).collect();
        Err(Error::new(
            ErrorCode::VersionConflict,
            format!("document {} changed on disk", first.id),
        )
        .with_detail("id", first.id.as_str())
        .with_detail("ids", ids)
        .with_detail("reason", "changed_on_disk"))
    }

    fn publish(&self, snapshot: &Snapshot, generation: u64, targets: &[Target]) -> Result<()> {
        let mut next = snapshot.clone();
        next.generation = generation;
        let mut touched: BTreeSet<(&str, &str)> = BTreeSet::new();
        for target in targets {
            let entry = match &target.next {
                Some(pending) => {
                    let metadata = fs::metadata(self.root.join(target.relative_path()))?;
                    let mut entry = FileEntry {
                        id: target.id.clone(),
                        size: 0,
                        mtime_ns: 0,
                        inode: 0,
                        outcome: Outcome::Doc(DocEntry {
                            version: pending.version.clone(),
                            doc: pending.doc.clone(),
                            id_mismatch: false,
                        }),
                    };
                    scan::stamp(&mut entry, &metadata);
                    Some(entry)
                }
                None => None,
            };
            next.update_collection(&target.db, &target.collection, |collection| {
                collection.put_file(&target.file_name, entry);
            });
            touched.insert((&target.db, &target.collection));
        }
        for (db, collection) in touched {
            next.update_collection(db, collection, |collection| {
                collection.case_duplicates = find_case_duplicates(&collection.files);
            });
        }
        self.snapshot.store(Arc::new(next));
        Ok(())
    }
}

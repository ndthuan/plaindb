use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use arc_swap::ArcSwap;
use serde_json::Value;

use crate::atomic::{ensure_dir, write_atomic_at};
use crate::cache;
use crate::codec::Doc;
use crate::config::{
    CollectionMeta, Durability, Format, STORE_FORMAT, StoreConfig, from_file_bytes, to_file_bytes,
    validate_branch_name, validate_remote_name,
};
use crate::error::{Error, ErrorCode, Result};
use crate::git;
use crate::journal;
use crate::lock::WriteLock;
use crate::names::{validate_collection_name, validate_db_name, validate_id};
use crate::scan;
use crate::snapshot::{CollectionSnapshot, InvalidFile, Outcome, Snapshot, Status, Warning};
use crate::txlog::{FileStamp, TxLog};
use crate::write::TxResult;

pub const STORE_FILE: &str = "_store.json";
pub const DB_FILE: &str = "_db.json";
pub const META_FILE: &str = "_meta.json";
pub const STATE_DIR: &str = ".plaindb";
pub const JOURNAL_TRUNCATED_WARNING: &str = "journal_truncated";
pub const JOURNAL_UNFINISHED_WARNING: &str = "journal_unfinished";

#[derive(Debug)]
pub(crate) struct WriterState {
    pub durability: Durability,
    pub checkpoint_interval: u64,
    pub tx_log: TxLog,
    pub journal_seen: Option<Option<FileStamp>>,
    pub journaled: Vec<TxResult>,
}

impl WriterState {
    fn new(config: &StoreConfig) -> Self {
        Self {
            durability: config.durability,
            checkpoint_interval: config.checkpoint_interval,
            tx_log: TxLog::default(),
            journal_seen: None,
            journaled: Vec::new(),
        }
    }

    pub(crate) fn configure(&mut self, config: &StoreConfig) {
        self.durability = config.durability;
        self.checkpoint_interval = config.checkpoint_interval;
    }

    pub(crate) fn find_tx(&mut self, root: &Path, tx_id: &str) -> Result<Option<TxResult>> {
        if let Some(found) = self.journaled.iter().find(|result| result.tx_id == tx_id) {
            return Ok(Some(found.clone()));
        }
        self.tx_log.find(root, tx_id)
    }
}

#[derive(Debug)]
pub struct Store {
    pub(crate) root: PathBuf,
    pub(crate) config: ArcSwap<StoreConfig>,
    pub(crate) snapshot: ArcSwap<Snapshot>,
    pub(crate) writer: Mutex<WriterState>,
    pub(crate) reader: bool,
    last_seen_head: Mutex<Option<git::HeadMark>>,
}

pub type Version = String;

fn directory_is_empty(dir: &Path) -> Result<bool> {
    Ok(fs::read_dir(dir)?.next().is_none())
}

pub(crate) fn child_directories(
    dir: &Path,
    marker: &str,
    valid: fn(&str) -> Result<()>,
) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if valid(&name).is_err() || !entry.path().join(marker).is_file() {
            continue;
        }
        names.push(name);
    }
    names.sort();
    Ok(names)
}

pub const UNKNOWN_SETTING_WARNING: &str = "unknown_setting";

pub(crate) fn read_config(root: &Path) -> Result<StoreConfig> {
    let bytes = match fs::read(root.join(STORE_FILE)) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == ErrorKind::NotFound => {
            return Err(
                Error::new(ErrorCode::NotAStore, "the directory holds no store")
                    .with_detail("reason", "missing_store_file"),
            );
        }
        Err(source) => return Err(source.into()),
    };
    let config: StoreConfig = from_file_bytes(&bytes).map_err(|source| {
        Error::new(ErrorCode::NotAStore, source.message)
            .with_detail("reason", "unreadable_store_file")
    })?;
    if config.format > STORE_FORMAT {
        return Err(Error::new(
            ErrorCode::UnsupportedFormat,
            "the store format is newer than this build",
        )
        .with_detail("found", config.format)
        .with_detail("supported", STORE_FORMAT));
    }
    validate_remote_name(&config.remote)?;
    validate_branch_name(&config.branch)?;
    Ok(config)
}

fn open_snapshot(root: &Path, timeout: Duration) -> Result<Snapshot> {
    let cached = cache::load(root);
    let (snapshot, unchanged) = scan::whole(root, timeout, || match &cached {
        Some(cached) => {
            let current = scan::rescan(root, cached)?;
            let unchanged = cache::same_files(cached, &current);
            Ok((current, unchanged))
        }
        None => Ok((scan::full_scan(root)?, false)),
    })?;
    if !unchanged && let Err(error) = cache::write(root, &snapshot) {
        tracing::debug!("the index cache could not be written: {}", error.message);
    }
    Ok(snapshot)
}

fn lock_timeout(config: &StoreConfig) -> Duration {
    Duration::from_millis(config.lock_timeout_ms)
}

fn state_dir_is_writable(root: &Path) -> bool {
    fs::metadata(root.join(STATE_DIR)).is_ok_and(|metadata| !metadata.permissions().readonly())
}

impl Drop for Store {
    fn drop(&mut self) {
        if !self.reader
            && journal::has_records(&self.root)
            && let Err(error) = self.checkpoint_if_free()
        {
            tracing::warn!("the checkpoint on close failed: {}", error.message);
        }
        let snapshot = self.snapshot.load();
        if cache::stored_generation(&self.root).is_none_or(|stored| snapshot.generation > stored) {
            let _ = cache::write(&self.root, &snapshot);
        }
    }
}

impl Store {
    pub fn init(dir: impl AsRef<Path>) -> Result<Self> {
        Self::init_with(dir, StoreConfig::default())
    }

    pub fn init_with(dir: impl AsRef<Path>, config: StoreConfig) -> Result<Self> {
        let root = dir.as_ref().to_path_buf();
        match fs::metadata(&root) {
            Ok(meta) if meta.is_dir() => {
                if root.join(STORE_FILE).exists() {
                    return Err(Error::new(
                        ErrorCode::AlreadyExists,
                        "the directory already holds a store",
                    )
                    .with_detail("reason", "initialized"));
                }
                if !directory_is_empty(&root)? {
                    return Err(
                        Error::new(ErrorCode::AlreadyExists, "the directory is not empty")
                            .with_detail("reason", "not_empty"),
                    );
                }
            }
            Ok(_) => {
                return Err(
                    Error::new(ErrorCode::AlreadyExists, "the path is not a directory")
                        .with_detail("reason", "not_a_directory"),
                );
            }
            Err(source) if source.kind() == ErrorKind::NotFound => fs::create_dir_all(&root)?,
            Err(source) => return Err(source.into()),
        }
        if config.format != STORE_FORMAT {
            return Err(Error::new(
                ErrorCode::UnsupportedFormat,
                "a new store must use the current format",
            )
            .with_detail("found", config.format)
            .with_detail("supported", STORE_FORMAT));
        }
        ensure_dir(&root.join(STATE_DIR), config.durability)?;
        let initial = StoreConfig {
            git: false,
            ..config.clone()
        };
        write_atomic_at(
            &root.join(STORE_FILE),
            &to_file_bytes(&initial),
            config.durability,
        )?;
        let snapshot = ArcSwap::from_pointee(scan::whole(&root, lock_timeout(&config), || {
            scan::full_scan(&root)
        })?);
        let writer = Mutex::new(WriterState::new(&config));
        let root_for_mark = root.clone();
        let store = Self {
            root,
            config: ArcSwap::from_pointee(config),
            snapshot,
            writer,
            reader: false,
            last_seen_head: Mutex::new(git::head_mark(&root_for_mark)),
        };
        if store.config.load().git {
            store.turn_git_on(git::INIT_STORE_SUBJECT)?;
        }
        Ok(store)
    }

    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let root = dir.as_ref().to_path_buf();
        let config = read_config(&root)?;
        let mut writer = WriterState::new(&config);
        if let Some(_lock) = WriteLock::try_acquire(&root)? {
            journal::recover(&root, &mut writer)?;
            if !config.git || git::has_repository(&root) {
                git::prepare_write(&root)?;
            }
        }
        let last_seen_head = Mutex::new(git::head_mark(&root));
        let snapshot = ArcSwap::from_pointee(open_snapshot(&root, lock_timeout(&config))?);
        Ok(Self {
            root,
            config: ArcSwap::from_pointee(config),
            snapshot,
            writer: Mutex::new(writer),
            reader: false,
            last_seen_head,
        })
    }

    pub fn open_reader(dir: impl AsRef<Path>) -> Result<Self> {
        let root = dir.as_ref().to_path_buf();
        let config = read_config(&root)?;
        let writer = WriterState::new(&config);
        if journal::unfinished_count(&root)? > 0 && state_dir_is_writable(&root) {
            match WriteLock::try_acquire(&root) {
                Ok(Some(_lock)) => {
                    if let Err(error) = journal::finish_files(&root, &writer) {
                        tracing::warn!(
                            "the unfinished journal was left as it is: {}",
                            error.message
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => tracing::debug!("the write lock was not taken: {}", error.message),
            }
        }
        let last_seen_head = Mutex::new(git::head_mark(&root));
        let snapshot = ArcSwap::from_pointee(open_snapshot(&root, lock_timeout(&config))?);
        Ok(Self {
            root,
            config: ArcSwap::from_pointee(config),
            snapshot,
            writer: Mutex::new(writer),
            reader: true,
            last_seen_head,
        })
    }

    pub fn checkpoint(&self) -> Result<()> {
        let _lock = self.begin_write()?;
        journal::checkpoint(&self.root, &mut self.writer())
    }

    fn checkpoint_if_free(&self) -> Result<()> {
        let Some(_lock) = WriteLock::try_acquire(&self.root)? else {
            return Ok(());
        };
        let config = read_config(&self.root)?;
        let mut writer = self.writer();
        writer.configure(&config);
        journal::settle(&self.root, &mut writer)?;
        journal::checkpoint(&self.root, &mut writer)
    }

    pub(crate) fn writer(&self) -> MutexGuard<'_, WriterState> {
        self.writer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn lock_timeout(&self) -> Duration {
        lock_timeout(&self.config.load())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> Arc<StoreConfig> {
        if let Ok(current) = read_config(&self.root)
            && current != **self.config.load()
        {
            self.config.store(Arc::new(current));
        }
        self.config.load_full()
    }

    pub fn list_dbs(&self) -> Result<Vec<String>> {
        child_directories(&self.root, DB_FILE, validate_db_name)
    }

    pub fn list_collections(&self, db: &str) -> Result<Vec<String>> {
        let dir = self.db_dir(db)?;
        child_directories(&dir, META_FILE, validate_collection_name)
    }

    pub fn db_dir(&self, db: &str) -> Result<PathBuf> {
        validate_db_name(db)?;
        let dir = self.root.join(db);
        if dir.join(DB_FILE).is_file() {
            Ok(dir)
        } else {
            Err(Error::not_found("db", db))
        }
    }

    pub fn collection_dir(&self, db: &str, collection: &str) -> Result<PathBuf> {
        validate_collection_name(collection)?;
        let dir = self.db_dir(db)?.join(collection);
        if dir.join(META_FILE).is_file() {
            Ok(dir)
        } else {
            Err(Error::not_found("collection", collection))
        }
    }

    pub fn collection_meta(&self, db: &str, collection: &str) -> Result<CollectionMeta> {
        let dir = self.collection_dir(db, collection)?;
        from_file_bytes(&fs::read(dir.join(META_FILE))?)
    }

    pub fn doc_path(
        &self,
        db: &str,
        collection: &str,
        id: &str,
        format: Format,
    ) -> Result<PathBuf> {
        validate_db_name(db)?;
        validate_collection_name(collection)?;
        validate_id(id)?;
        Ok(self
            .root
            .join(db)
            .join(collection)
            .join(format!("{id}.{}", format.extension())))
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.load_full()
    }

    pub fn rescan(&self) -> Result<()> {
        let head = self.read_head_mark();
        let current = self.snapshot.load_full();
        let next = Arc::new(scan::whole(&self.root, self.lock_timeout(), || {
            scan::rescan(&self.root, &current)
        })?);
        self.snapshot.compare_and_swap(&current, next);
        self.saw_head(head);
        Ok(())
    }

    pub fn refresh_if_moved(&self) -> Result<()> {
        let head = self.read_head_mark();
        let current = self.snapshot.load_full();
        if scan::read_generation(&self.root)? == current.generation && !self.head_moved(&head) {
            return Ok(());
        }
        let next = Arc::new(scan::whole(&self.root, self.lock_timeout(), || {
            scan::rescan(&self.root, &current)
        })?);
        self.snapshot.compare_and_swap(&current, next);
        self.saw_head(head);
        Ok(())
    }

    pub(crate) fn read_head_mark(&self) -> Option<git::HeadMark> {
        if self.config.load().git {
            git::head_mark(&self.root)
        } else {
            None
        }
    }

    fn last_seen_head(&self) -> MutexGuard<'_, Option<git::HeadMark>> {
        self.last_seen_head
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn head_moved(&self, head: &Option<git::HeadMark>) -> bool {
        *self.last_seen_head() != *head
    }

    pub(crate) fn saw_head(&self, head: Option<git::HeadMark>) {
        *self.last_seen_head() = head;
    }

    pub(crate) fn with_collection<T>(
        &self,
        db: &str,
        collection: &str,
        read: impl FnOnce(&CollectionSnapshot) -> Result<T>,
    ) -> Result<T> {
        validate_db_name(db)?;
        validate_collection_name(collection)?;
        self.refresh_if_moved()?;
        let snapshot = self.snapshot.load();
        let db_snapshot = snapshot
            .dbs
            .get(db)
            .ok_or_else(|| Error::not_found("db", db))?;
        let collection_snapshot = db_snapshot
            .collections
            .get(collection)
            .ok_or_else(|| Error::not_found("collection", collection))?;
        read(collection_snapshot)
    }

    pub fn get(&self, db: &str, collection: &str, id: &str) -> Result<Option<(Doc, Version)>> {
        validate_id(id)?;
        self.with_collection(db, collection, |snapshot| {
            let name = snapshot.file_name(id);
            let Some(entry) = snapshot.files.get(&name) else {
                return Ok(None);
            };
            let refused = |reason: &str| {
                Error::invalid_document(reason, format!("document {id} is invalid"))
                    .with_detail("id", id)
            };
            if snapshot.is_duplicate(&name) {
                return Err(refused("duplicate_id_case"));
            }
            match &entry.outcome {
                Outcome::Doc(found) => Ok(Some(((*found.doc).clone(), found.version.clone()))),
                Outcome::Invalid(invalid) => Err(refused(&invalid.reason)),
                Outcome::BadName { .. } => Ok(None),
            }
        })
    }

    pub fn list_ids(&self, db: &str, collection: &str) -> Result<Vec<String>> {
        self.with_collection(db, collection, |snapshot| {
            Ok(snapshot
                .readable_docs()
                .map(|(_, entry)| entry.doc["_id"].as_str().unwrap_or_default().to_owned())
                .collect())
        })
    }

    pub fn status(&self) -> Result<Status> {
        self.refresh_if_moved()?;
        let snapshot = self.snapshot.load();
        let mut status = Status {
            invalid: Vec::new(),
            warnings: Vec::new(),
            documents: 0,
            store_bytes: 0,
            git: self.git_status()?,
        };
        if let Some((path, details)) = self.git_warning()? {
            status.warnings.push(Warning {
                code: git::FOREIGN_COMMIT_WARNING.to_owned(),
                db: String::new(),
                collection: String::new(),
                path,
                details,
            });
        }
        let unfinished = journal::unfinished_count(&self.root)?;
        if unfinished > 0 {
            status.warnings.push(Warning {
                code: JOURNAL_UNFINISHED_WARNING.to_owned(),
                db: String::new(),
                collection: String::new(),
                path: self.root.clone(),
                details: serde_json::json!({ "records": unfinished }),
            });
        }
        for file in journal::rejected_files(&self.root) {
            status.warnings.push(Warning {
                code: JOURNAL_TRUNCATED_WARNING.to_owned(),
                db: String::new(),
                collection: String::new(),
                path: self.root.clone(),
                details: serde_json::json!({ "file": file }),
            });
        }
        if let Some(missing) = git::read_missing_commits(&self.root)? {
            status.warnings.push(Warning {
                code: git::GIT_COMMIT_PENDING_WARNING.to_owned(),
                db: String::new(),
                collection: String::new(),
                path: self.root.clone(),
                details: serde_json::json!({
                    "transactions": missing.tx_ids.len(),
                    "message": missing.message,
                }),
            });
        }
        if status.git.enabled && status.git.push.is_on() && !status.git.git_binary_found {
            status.warnings.push(Warning {
                code: git::GIT_NOT_FOUND_WARNING.to_owned(),
                db: String::new(),
                collection: String::new(),
                path: self.root.clone(),
                details: serde_json::json!({
                    "binary": status.git.git_binary,
                    "source": status.git.git_binary_source,
                }),
            });
        }
        if status.git.enabled && !git::has_repository(&self.root) {
            status.warnings.push(Warning {
                code: git::GIT_REPOSITORY_MISSING_WARNING.to_owned(),
                db: String::new(),
                collection: String::new(),
                path: self.root.clone(),
                details: serde_json::json!({
                    "message": git::repository_missing().message,
                }),
            });
        }
        if status.git.enabled && git::has_repository(&self.root) {
            let head = git::head_branch(&self.root)?;
            if head.as_deref() != Some(status.git.branch.as_str()) {
                status.warnings.push(Warning {
                    code: git::HEAD_OFF_BRANCH_WARNING.to_owned(),
                    db: String::new(),
                    collection: String::new(),
                    path: self.root.clone(),
                    details: serde_json::json!({
                        "head": git::head_shown(head.as_deref()),
                        "branch": status.git.branch,
                    }),
                });
            }
        }
        for key in read_config(&self.root)?.unknown.keys() {
            status.warnings.push(Warning {
                code: UNKNOWN_SETTING_WARNING.to_owned(),
                db: String::new(),
                collection: String::new(),
                path: self.root.join(STORE_FILE),
                details: serde_json::json!({ "key": key }),
            });
        }
        if read_config(&self.root)?.git_binary.is_some() {
            status.warnings.push(Warning {
                code: git::GIT_BINARY_IGNORED_WARNING.to_owned(),
                db: String::new(),
                collection: String::new(),
                path: self.root.clone(),
                details: serde_json::json!({
                    "message": "_store.json names a git program; it is ignored, set PLAINDB_GIT instead",
                }),
            });
        }
        for (db, db_snapshot) in snapshot.dbs.iter() {
            for (collection, collection_snapshot) in db_snapshot.collections.iter() {
                for (name, entry) in collection_snapshot.files.iter() {
                    status.store_bytes += entry.size;
                    let duplicate = collection_snapshot.is_duplicate(name);
                    let mut invalid = |path: &Path, reason: &str| {
                        status.invalid.push(InvalidFile {
                            db: db.clone(),
                            collection: collection.clone(),
                            id: entry.id.clone(),
                            path: path.to_path_buf(),
                            reason: reason.to_owned(),
                        });
                    };
                    match &entry.outcome {
                        Outcome::Doc(found) => {
                            let path = self.root.join(db).join(collection).join(name);
                            if duplicate {
                                invalid(&path, "duplicate_id_case");
                                continue;
                            }
                            status.documents += 1;
                            if found.id_mismatch {
                                status.warnings.push(Warning {
                                    code: "id_mismatch".to_owned(),
                                    db: db.clone(),
                                    collection: collection.clone(),
                                    path,
                                    details: Value::Null,
                                });
                            }
                        }
                        Outcome::Invalid(found) => {
                            let reason = if duplicate {
                                "duplicate_id_case"
                            } else {
                                &found.reason
                            };
                            invalid(&found.path, reason);
                        }
                        Outcome::BadName { path } => status.warnings.push(Warning {
                            code: "invalid_name".to_owned(),
                            db: db.clone(),
                            collection: collection.clone(),
                            path: path.clone(),
                            details: Value::Null,
                        }),
                    }
                }
            }
        }
        Ok(status)
    }
}

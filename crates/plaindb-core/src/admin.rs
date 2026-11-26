use std::sync::Arc;

use serde_json::{Value, json};

use crate::config::{
    CollectionMeta, CommitMode, Durability, Format, IndexSpec, STORE_FORMAT, to_file_bytes,
};
use crate::error::{Error, ErrorCode, Result};
use crate::git;
use crate::index::Indexes;
use crate::journal::{self, JournalEntry};
use crate::names::{new_id, validate_collection_name, validate_db_name};
use crate::scan::{self, SYSTEM_FIELDS, compile_validator};
use crate::store::{DB_FILE, META_FILE, STORE_FILE, Store, read_config};

fn already_exists(kind: &str, name: &str) -> Error {
    Error::new(
        ErrorCode::AlreadyExists,
        format!("{kind} {name} already exists"),
    )
    .with_detail("kind", kind)
    .with_detail("name", name)
    .with_detail("reason", "exists")
}

impl Store {
    pub(crate) fn commit_admin(&self, subject: &str, mut entries: Vec<JournalEntry>) -> Result<()> {
        for entry in &mut entries {
            entry.fill_old_version(&self.root)?;
        }
        let generation = self.snapshot.load().generation + 1;
        let tx_id = new_id();
        let plan = git::plan_admin(&self.root, &read_config(&self.root)?, &tx_id, subject)?;
        let journal = plan.into_journal(tx_id, generation, entries);
        journal::commit(&self.root, &mut self.writer(), &journal)?;
        let current = self.snapshot.load_full();
        self.snapshot
            .store(Arc::new(scan::rescan(&self.root, &current)?));
        Ok(())
    }

    fn write_meta(
        &self,
        subject: &str,
        db: &str,
        collection: &str,
        meta: &CollectionMeta,
    ) -> Result<()> {
        self.commit_admin(
            subject,
            vec![JournalEntry::write(
                format!("{db}/{collection}/{META_FILE}"),
                &to_file_bytes(meta),
                None,
            )],
        )
    }

    pub fn create_db(&self, db: &str) -> Result<()> {
        validate_db_name(db)?;
        let _push = self.push_guard();
        let _lock = self.begin_write()?;
        if self.root.join(db).join(DB_FILE).is_file() {
            return Err(already_exists("db", db));
        }
        self.commit_admin(
            &format!("init db {db}"),
            vec![JournalEntry::write(
                format!("{db}/{DB_FILE}"),
                &to_file_bytes(&json!({ "format": STORE_FORMAT })),
                None,
            )],
        )
    }

    pub fn drop_db(&self, db: &str) -> Result<()> {
        let _push = self.push_guard();
        let _lock = self.begin_write()?;
        self.db_dir(db)?;
        self.commit_admin(
            &format!("drop db {db}"),
            vec![JournalEntry::remove_tree(db.to_owned())],
        )
    }

    pub fn create_collection(&self, db: &str, collection: &str, format: Format) -> Result<()> {
        validate_db_name(db)?;
        validate_collection_name(collection)?;
        let _push = self.push_guard();
        let _lock = self.begin_write()?;
        let dir = self.db_dir(db)?.join(collection);
        if dir.join(META_FILE).is_file() {
            return Err(already_exists("collection", collection));
        }
        self.write_meta(
            &format!("init collection {db}/{collection} ({})", format.extension()),
            db,
            collection,
            &CollectionMeta::new(format),
        )
    }

    pub fn drop_collection(&self, db: &str, collection: &str) -> Result<()> {
        let _push = self.push_guard();
        let _lock = self.begin_write()?;
        self.collection_dir(db, collection)?;
        self.commit_admin(
            &format!("drop collection {db}/{collection}"),
            vec![JournalEntry::remove_tree(format!("{db}/{collection}"))],
        )
    }

    pub fn set_schema(&self, db: &str, collection: &str, schema: Option<Value>) -> Result<()> {
        let _push = self.push_guard();
        let _lock = self.begin_write()?;
        let mut meta = self.collection_meta(db, collection)?;
        meta.schema = schema;
        let dir = self.collection_dir(db, collection)?;
        if let Some(validator) = compile_validator(&meta, &dir)? {
            let snapshot = self.snapshot.load();
            let mut failing = Vec::new();
            if let Some(found) = snapshot
                .dbs
                .get(db)
                .and_then(|entry| entry.collections.get(collection))
            {
                for (_, entry) in found.readable_docs() {
                    let mut checked = (*entry.doc).clone();
                    for field in SYSTEM_FIELDS {
                        checked.remove(field);
                    }
                    if !validator.is_valid(&Value::Object(checked)) {
                        failing.push(entry.doc["_id"].as_str().unwrap_or_default().to_owned());
                    }
                }
            }
            if !failing.is_empty() {
                return Err(Error::new(
                    ErrorCode::SchemaViolation,
                    format!("{} documents do not match the new schema", failing.len()),
                )
                .with_detail("ids", failing));
            }
        }
        let action = if meta.schema.is_some() {
            "set schema"
        } else {
            "clear schema"
        };
        self.write_meta(
            &format!("meta {db}/{collection}: {action}"),
            db,
            collection,
            &meta,
        )
    }

    pub fn set_indexes(&self, db: &str, collection: &str, indexes: Vec<IndexSpec>) -> Result<()> {
        let _push = self.push_guard();
        let _lock = self.begin_write()?;
        let mut meta = self.collection_meta(db, collection)?;
        meta.indexes = indexes;
        {
            let snapshot = self.snapshot.load();
            if let Some(found) = snapshot
                .dbs
                .get(db)
                .and_then(|entry| entry.collections.get(collection))
            {
                Indexes::validate_new(db, collection, &meta, found.files.values())?;
            }
        }
        self.write_meta(
            &format!("meta {db}/{collection}: set indexes"),
            db,
            collection,
            &meta,
        )
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let _push = self.push_guard();
        let _lock = self.begin_write()?;
        let before = read_config(&self.root)?;
        let mut config = before.clone();
        config.set(key, value)?;
        if config == before {
            return Ok(());
        }
        let subject = format!("meta store: set {key}");
        self.commit_admin(
            &subject,
            vec![JournalEntry::write(
                STORE_FILE.to_owned(),
                &to_file_bytes(&config),
                None,
            )],
        )?;
        self.config.store(Arc::new(config.clone()));
        if key == "commit" && config.git && before.commit == CommitMode::Manual {
            self.commit_pending(Some(&subject), None)?;
        }
        Ok(())
    }

    pub fn set_durability(&self, durability: Durability) -> Result<()> {
        self.set_setting("durability", durability.as_str())
    }
}

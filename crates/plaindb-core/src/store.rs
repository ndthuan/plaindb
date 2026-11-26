use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::config::{
    CollectionMeta, Format, STORE_FORMAT, StoreConfig, from_file_bytes, to_file_bytes,
};
use crate::error::{Error, ErrorCode, Result};
use crate::names::{validate_collection_name, validate_db_name, validate_id};

pub const STORE_FILE: &str = "_store.json";
pub const DB_FILE: &str = "_db.json";
pub const META_FILE: &str = "_meta.json";
pub const STATE_DIR: &str = ".plaindb";

#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    config: StoreConfig,
}

fn directory_is_empty(dir: &Path) -> Result<bool> {
    Ok(fs::read_dir(dir)?.next().is_none())
}

fn child_directories(
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

impl Store {
    pub fn init(dir: impl AsRef<Path>) -> Result<Self> {
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
        let config = StoreConfig::default();
        fs::write(root.join(STORE_FILE), to_file_bytes(&config))?;
        fs::create_dir_all(root.join(STATE_DIR))?;
        Ok(Self { root, config })
    }

    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let root = dir.as_ref().to_path_buf();
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
        Ok(Self { root, config })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> &StoreConfig {
        &self.config
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
}

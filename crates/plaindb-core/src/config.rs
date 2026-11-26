use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, ErrorCode, Result};

pub const STORE_FORMAT: u32 = 1;
pub const DEFAULT_LOCK_TIMEOUT_MS: u64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommitMode {
    Each,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PushMode {
    Off,
    OnCommit,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub max_document_bytes: Option<u64>,
    pub max_results: Option<u64>,
    pub max_scan_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreConfig {
    pub format: u32,
    pub git: bool,
    pub commit: CommitMode,
    pub push: PushMode,
    pub remote: String,
    pub branch: String,
    pub lock_timeout_ms: u64,
    pub limits: Limits,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            format: STORE_FORMAT,
            git: false,
            commit: CommitMode::Each,
            push: PushMode::Off,
            remote: "origin".to_owned(),
            branch: "main".to_owned(),
            lock_timeout_ms: DEFAULT_LOCK_TIMEOUT_MS,
            limits: Limits::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Json,
    Yaml,
    Md,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Md => "md",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IndexSpec {
    Field {
        field: String,
        #[serde(default)]
        unique: bool,
        #[serde(default)]
        multi: bool,
    },
    Text {
        text: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionMeta {
    pub format: Format,
    #[serde(default)]
    pub schema: Option<Value>,
    #[serde(default)]
    pub indexes: Vec<IndexSpec>,
}

impl CollectionMeta {
    pub fn new(format: Format) -> Self {
        Self {
            format,
            schema: None,
            indexes: Vec::new(),
        }
    }
}

pub fn to_file_bytes<T: Serialize>(value: &T) -> Vec<u8> {
    let value = serde_json::to_value(value).expect("config serialises");
    crate::jsontext::pretty_bytes(&value)
}

pub fn from_file_bytes<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes)
        .map_err(|source| Error::new(ErrorCode::InvalidDocument, source.to_string()))
}

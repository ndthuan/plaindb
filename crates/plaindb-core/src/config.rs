use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, ErrorCode, Result};

pub const STORE_FORMAT: u32 = 1;
pub const DEFAULT_LOCK_TIMEOUT_MS: u64 = 5_000;
pub const DEFAULT_CHECKPOINT_INTERVAL: u64 = 64;
pub const DEFAULT_GC_EVERY: u64 = 200;

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Durability {
    #[default]
    Full,
    Normal,
    Off,
}

impl Durability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Normal => "normal",
            Self::Off => "off",
        }
    }
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
    pub durability: Durability,
    pub checkpoint_interval: u64,
    pub gc_every: u64,
    #[serde(default, skip_serializing)]
    pub git_binary: Option<String>,
    pub limits: Limits,
}

pub const SETTING_KEYS: &[&str] = &[
    "commit",
    "push",
    "remote",
    "branch",
    "lock_timeout_ms",
    "durability",
    "gc_every",
    "limits.max_document_bytes",
    "limits.max_results",
    "limits.max_scan_ms",
];

fn unknown_setting(key: &str) -> Error {
    Error::invalid_name("setting", key).with_detail("known", SETTING_KEYS.join(", "))
}

fn invalid_value(key: &str, expected: &str) -> Error {
    Error::new(ErrorCode::InvalidConfig, format!("{key} takes {expected}")).with_detail("key", key)
}

const REMOTE_EXPECTED: &str = "a remote name of letters, digits, . _ -";
const BRANCH_EXPECTED: &str = "a branch name without spaces, .., @{ or a leading - + :";
const MAX_REMOTE_CHARS: usize = 64;
const MAX_BRANCH_CHARS: usize = 200;

fn invalid_name_value(key: &str, expected: &str, value: &str) -> Error {
    invalid_value(key, expected).with_detail("value", value)
}

fn is_plain_name(value: &str, max: usize, extra: fn(char) -> bool) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && value.chars().count() <= max
        && chars.all(|c| c.is_ascii_alphanumeric() || extra(c))
}

pub fn validate_remote_name(value: &str) -> Result<()> {
    if is_plain_name(value, MAX_REMOTE_CHARS, |c| matches!(c, '.' | '_' | '-')) {
        Ok(())
    } else {
        Err(invalid_name_value("remote", REMOTE_EXPECTED, value))
    }
}

pub fn validate_branch_name(value: &str) -> Result<()> {
    let valid = is_plain_name(value, MAX_BRANCH_CHARS, |c| {
        matches!(c, '.' | '_' | '/' | '-')
    }) && !value.contains("..")
        && !value.contains("//")
        && !value.contains("@{")
        && !value.ends_with('/')
        && !value.ends_with('.')
        && !value
            .split('/')
            .any(|component| component.ends_with(".lock"));
    if valid {
        Ok(())
    } else {
        Err(invalid_name_value("branch", BRANCH_EXPECTED, value))
    }
}

pub(crate) fn validate_remote_url(url: &str) -> Result<()> {
    let valid = !url.is_empty()
        && !url.starts_with('-')
        && !url.chars().any(|c| c.is_whitespace() || c.is_control());
    if valid {
        Ok(())
    } else {
        Err(invalid_name_value(
            "remote_url",
            "a URL that does not start with - and has no spaces or control characters",
            url,
        ))
    }
}

fn parse_count(key: &str, value: &str) -> Result<u64> {
    value
        .parse()
        .map_err(|_| invalid_value(key, "a whole number"))
}

fn parse_limit(key: &str, value: &str) -> Result<Option<u64>> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_count(key, value).map(Some)
    }
}

fn parse_choice<T: Copy>(key: &str, value: &str, choices: &[(&str, T)]) -> Result<T> {
    choices
        .iter()
        .find(|(name, _)| *name == value)
        .map(|(_, choice)| *choice)
        .ok_or_else(|| {
            let names: Vec<&str> = choices.iter().map(|(name, _)| *name).collect();
            invalid_value(key, &format!("one of {}", names.join(", ")))
        })
}

impl StoreConfig {
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "commit" => {
                self.commit = parse_choice(
                    key,
                    value,
                    &[("each", CommitMode::Each), ("manual", CommitMode::Manual)],
                )?;
            }
            "push" => {
                self.push = parse_choice(
                    key,
                    value,
                    &[("off", PushMode::Off), ("on-commit", PushMode::OnCommit)],
                )?;
            }
            "durability" => {
                self.durability = parse_choice(
                    key,
                    value,
                    &[
                        ("full", Durability::Full),
                        ("normal", Durability::Normal),
                        ("off", Durability::Off),
                    ],
                )?;
            }
            "remote" => {
                validate_remote_name(value)?;
                value.clone_into(&mut self.remote);
            }
            "branch" => {
                validate_branch_name(value)?;
                value.clone_into(&mut self.branch);
            }
            "lock_timeout_ms" => self.lock_timeout_ms = parse_count(key, value)?,
            "gc_every" => self.gc_every = parse_count(key, value)?,
            "git_binary" => {
                return Err(Error::new(
                    ErrorCode::InvalidConfig,
                    "git_binary is no longer a setting: set the PLAINDB_GIT environment variable",
                )
                .with_detail("key", key));
            }
            "limits.max_document_bytes" => {
                self.limits.max_document_bytes = parse_limit(key, value)?;
            }
            "limits.max_results" => self.limits.max_results = parse_limit(key, value)?,
            "limits.max_scan_ms" => self.limits.max_scan_ms = parse_limit(key, value)?,
            _ => return Err(unknown_setting(key)),
        }
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Result<Value> {
        if !SETTING_KEYS.contains(&key) {
            return Err(unknown_setting(key));
        }
        let mut value = serde_json::to_value(self).unwrap_or_default();
        for part in key.split('.') {
            value = value.get(part).cloned().unwrap_or(Value::Null);
        }
        Ok(value)
    }
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
            durability: Durability::Full,
            checkpoint_interval: DEFAULT_CHECKPOINT_INTERVAL,
            gc_every: DEFAULT_GC_EVERY,
            git_binary: None,
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

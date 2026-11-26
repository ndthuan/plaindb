use std::fmt;

use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    NotFound,
    AlreadyExists,
    VersionConflict,
    InvalidName,
    InvalidDocument,
    InvalidConfig,
    SchemaViolation,
    UniqueViolation,
    InvalidQuery,
    StoreLockedTimeout,
    NotAStore,
    UnsupportedFormat,
    LimitExceeded,
    GitDisabled,
    GitNotFound,
    RemoteAhead,
    Io,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::AlreadyExists => "already_exists",
            Self::VersionConflict => "version_conflict",
            Self::InvalidName => "invalid_name",
            Self::InvalidDocument => "invalid_document",
            Self::InvalidConfig => "invalid_config",
            Self::SchemaViolation => "schema_violation",
            Self::UniqueViolation => "unique_violation",
            Self::InvalidQuery => "invalid_query",
            Self::StoreLockedTimeout => "store_locked_timeout",
            Self::NotAStore => "not_a_store",
            Self::UnsupportedFormat => "unsupported_format",
            Self::LimitExceeded => "limit_exceeded",
            Self::GitDisabled => "git_disabled",
            Self::GitNotFound => "git_not_found",
            Self::RemoteAhead => "remote_ahead",
            Self::Io => "io",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{code}: {message}")]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    pub details: Value,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: json!({}),
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    pub fn with_detail(mut self, key: &str, value: impl Into<Value>) -> Self {
        if let Value::Object(map) = &mut self.details {
            map.insert(key.to_owned(), value.into());
        }
        self
    }

    pub fn not_found(kind: &str, name: &str) -> Self {
        Self::new(ErrorCode::NotFound, format!("{kind} {name} not found"))
            .with_detail("kind", kind)
            .with_detail("name", name)
    }

    pub fn invalid_name(kind: &str, name: &str) -> Self {
        Self::new(ErrorCode::InvalidName, format!("invalid {kind} name"))
            .with_detail("kind", kind)
            .with_detail("name", name)
    }

    pub fn invalid_document(reason: &str, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidDocument, message).with_detail("reason", reason)
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::new(ErrorCode::Io, source.to_string())
            .with_detail("os_kind", format!("{:?}", source.kind()))
    }
}

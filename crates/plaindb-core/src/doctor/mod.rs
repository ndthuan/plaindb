mod environment;
mod store;

use std::path::Path;

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::StoreConfig;
use crate::error::ErrorCode;
use crate::git;
use crate::store::{Store, read_config};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    Git,
    Store,
    Environment,
}

impl Group {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Store => "store",
            Self::Environment => "environment",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub group: Group,
    pub status: CheckStatus,
    pub message: String,
    pub details: Value,
    pub fix: Option<String>,
}

impl Check {
    pub fn new(
        group: Group,
        id: &'static str,
        status: CheckStatus,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id,
            group,
            status,
            message: message.into(),
            details: json!({}),
            fix: None,
        }
    }

    pub fn ok(group: Group, id: &'static str, message: impl Into<String>) -> Self {
        Self::new(group, id, CheckStatus::Ok, message)
    }

    pub fn warn(group: Group, id: &'static str, message: impl Into<String>) -> Self {
        Self::new(group, id, CheckStatus::Warn, message)
    }

    pub fn error(group: Group, id: &'static str, message: impl Into<String>) -> Self {
        Self::new(group, id, CheckStatus::Error, message)
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DoctorOptions {
    pub offline: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    pub fn errors(&self) -> usize {
        self.count(CheckStatus::Error)
    }

    pub fn warnings(&self) -> usize {
        self.count(CheckStatus::Warn)
    }

    fn count(&self, status: CheckStatus) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == status)
            .count()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "errors": self.errors(),
            "warnings": self.warnings(),
            "checks": self.checks,
        })
    }
}

pub fn inspect(root: &Path, options: DoctorOptions) -> Report {
    let mut checks = Vec::new();
    match read_config(root) {
        Ok(config) => collect(root, &config, options, &mut checks),
        Err(error) if error.code == ErrorCode::InvalidConfig => checks.push(
            Check::error(Group::Store, "store_config", error.message.clone())
                .with_fix(format!("edit _store.json: {}", error.message))
                .with_details(error.details),
        ),
        Err(error) => checks.push(
            Check::error(Group::Store, "store_format", error.message)
                .with_details(error.details)
                .with_fix("run plaindb with --store pointing at a directory made by plaindb init"),
        ),
    }
    Report { checks }
}

fn collect(root: &Path, config: &StoreConfig, options: DoctorOptions, checks: &mut Vec<Check>) {
    checks.extend(git::doctor_checks(root, config, options));
    checks.extend(store::checks(root, config));
    checks.extend(environment::checks(root, config));
}

impl Store {
    pub fn doctor(&self, options: DoctorOptions) -> Report {
        inspect(&self.root, options)
    }
}

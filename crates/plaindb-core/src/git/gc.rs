use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::command::run_git;
use super::push::{directory_bytes, truncated, try_lock_file};
use super::{git_disabled, has_repository, read_optional, repository_missing, state_file};
use crate::atomic::replace_unsynced;
use crate::config::to_file_bytes;
use crate::error::Result;
use crate::store::{Store, read_config};
use crate::write::now_timestamp;

const GC_LOCK_FILE: &str = "gc.lock";
const GC_STATE_FILE: &str = "gc.json";
const GC_COUNT_FILE: &str = "gc-count.json";
const GC_TIMEOUT: Duration = Duration::from_secs(600);
const ALREADY_RUNNING: &str = "already running";
const GIT_NOT_FOUND: &str = "git_not_found";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GcOutcome {
    Done,
    Busy,
    Failed,
}

impl GcOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Busy => "busy",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcReport {
    pub outcome: GcOutcome,
    pub error: Option<String>,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct GcState {
    pub last_gc_at: Option<String>,
    pub last_gc_outcome: Option<String>,
    pub last_gc_error: Option<String>,
    pub commits_at_last_gc: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct GcCount {
    commits: u64,
}

pub(crate) fn read_state(root: &Path) -> Result<GcState> {
    let Some(bytes) = read_optional(&state_file(root, GC_STATE_FILE))? else {
        return Ok(GcState::default());
    };
    Ok(serde_json::from_slice(&bytes).unwrap_or_default())
}

fn read_count(root: &Path) -> u64 {
    read_optional(&state_file(root, GC_COUNT_FILE))
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<GcCount>(&bytes).ok())
        .unwrap_or_default()
        .commits
}

pub(crate) fn commits_since_last_gc(root: &Path) -> u64 {
    read_count(root).saturating_sub(read_state(root).unwrap_or_default().commits_at_last_gc)
}

pub(crate) fn count_commit(root: &Path) {
    let count = GcCount {
        commits: read_count(root) + 1,
    };
    let _ = replace_unsynced(&state_file(root, GC_COUNT_FILE), &to_file_bytes(&count));
}

fn finish(
    root: &Path,
    outcome: GcOutcome,
    error: Option<String>,
    counted: u64,
    bytes: (u64, u64),
) -> Result<GcReport> {
    let mut state = read_state(root)?;
    state.last_gc_at = Some(now_timestamp());
    state.last_gc_outcome = Some(outcome.as_str().to_owned());
    state.last_gc_error.clone_from(&error);
    if outcome != GcOutcome::Busy {
        state.commits_at_last_gc = counted;
    }
    replace_unsynced(&state_file(root, GC_STATE_FILE), &to_file_bytes(&state))?;
    Ok(GcReport {
        outcome,
        error,
        bytes_before: bytes.0,
        bytes_after: bytes.1,
    })
}

fn collect(root: &Path, auto: bool) -> Result<GcReport> {
    let git_dir = root.join(".git");
    let counted = read_count(root);
    let Some(lock) = try_lock_file(root, GC_LOCK_FILE)? else {
        let bytes = directory_bytes(&git_dir);
        return Ok(GcReport {
            outcome: GcOutcome::Busy,
            error: None,
            bytes_before: bytes,
            bytes_after: bytes,
        });
    };
    let before = directory_bytes(&git_dir);
    let mut args = vec!["-c", "gc.autoDetach=false", "gc", "--quiet"];
    if auto {
        args.push("--auto");
    }
    let report = match run_git(root, &args, GC_TIMEOUT) {
        Ok(run) if run.success => (GcOutcome::Done, None),
        Ok(run) if run.stderr.contains(ALREADY_RUNNING) => (GcOutcome::Busy, None),
        Ok(run) => (GcOutcome::Failed, Some(truncated(&run.stderr))),
        Err(source) if source.kind() == ErrorKind::NotFound => {
            (GcOutcome::Failed, Some(GIT_NOT_FOUND.to_owned()))
        }
        Err(source) => (GcOutcome::Failed, Some(truncated(&source.to_string()))),
    };
    let after = directory_bytes(&git_dir);
    let finished = finish(root, report.0, report.1, counted, (before, after));
    drop(lock);
    finished
}

impl Store {
    pub fn gc(&self) -> Result<GcReport> {
        let config = read_config(&self.root)?;
        if !config.git {
            return Err(git_disabled());
        }
        if !has_repository(&self.root) {
            return Err(repository_missing());
        }
        collect(&self.root, false)
    }

    pub(crate) fn collect_if_due(&self) {
        let Ok(config) = read_config(&self.root) else {
            return;
        };
        if !config.git || config.gc_every == 0 || !self.root.join(".git").exists() {
            return;
        }
        let Ok(state) = read_state(&self.root) else {
            return;
        };
        if read_count(&self.root).saturating_sub(state.commits_at_last_gc) < config.gc_every {
            return;
        }
        let _ = collect(&self.root, true);
    }
}

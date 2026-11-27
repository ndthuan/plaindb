use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::commit::commit_journal;
use super::read_optional;
use crate::atomic::{replace_unsynced, unlink};
use crate::error::Result;
use crate::journal::Journal;
use crate::store::STATE_DIR;

pub const MISSING_COMMITS_FILE: &str = "git-pending.json";
pub const GIT_COMMIT_PENDING_WARNING: &str = "git_commit_pending";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingCommits {
    pub tx_ids: Vec<String>,
    #[serde(default)]
    pub message: String,
}

fn path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(MISSING_COMMITS_FILE)
}

pub(crate) fn exists(root: &Path) -> bool {
    path(root).exists()
}

pub(crate) fn read(root: &Path) -> Result<Option<MissingCommits>> {
    let Some(bytes) = read_optional(&path(root))? else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_slice(&bytes).unwrap_or_default()))
}

fn write(root: &Path, missing: &MissingCommits) -> Result<()> {
    if missing.tx_ids.is_empty() {
        unlink(&path(root))?;
        return Ok(());
    }
    let mut bytes = serde_json::to_vec_pretty(missing).expect("missing commits serialise");
    bytes.push(b'\n');
    replace_unsynced(&path(root), &bytes)
}

pub(crate) fn add(root: &Path, tx_id: &str, message: Option<&str>) -> Result<()> {
    let mut missing = read(root)?.unwrap_or_default();
    if !missing.tx_ids.iter().any(|known| known == tx_id) {
        missing.tx_ids.push(tx_id.to_owned());
    }
    if let Some(message) = message {
        message.clone_into(&mut missing.message);
    }
    write(root, &missing)
}

pub(crate) fn retry(root: &Path, journals: &[Journal]) -> Result<()> {
    let Some(mut missing) = read(root)? else {
        return Ok(());
    };
    let look_back = journals.len() + 1;
    let wanted: Vec<&Journal> = journals
        .iter()
        .filter(|journal| missing.tx_ids.contains(&journal.tx_id))
        .collect();
    missing
        .tx_ids
        .retain(|tx_id| wanted.iter().any(|journal| &journal.tx_id == tx_id));
    for journal in wanted {
        if let Err(error) = commit_journal(root, journal, look_back) {
            missing.message.clone_from(&error.message);
            write(root, &missing)?;
            return Err(error);
        }
        missing.tx_ids.retain(|tx_id| tx_id != &journal.tx_id);
    }
    write(root, &missing)
}

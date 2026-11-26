use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use crate::atomic::{
    parent_of, place, remove_temp_files, remove_tree_durable, replace_unsynced, sync_dir, unlink,
    write_atomic,
};
use crate::error::{Error, ErrorCode, Result};
use crate::git::{PendingTx, commit_journal};
use crate::hooks;
use crate::scan::{GENERATION_FILE, read_generation};
use crate::store::STATE_DIR;
use crate::txlog;
use crate::write::TxResult;

pub const JOURNAL_FILE: &str = "journal.json";

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JournalEntry {
    pub path: String,
    pub bytes_b64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_version: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub tree: bool,
}

impl JournalEntry {
    pub fn write(path: String, bytes: &[u8], old_version: Option<String>) -> Self {
        Self {
            path,
            bytes_b64: Some(STANDARD.encode(bytes)),
            old_version,
            tree: false,
        }
    }

    pub fn remove(path: String, old_version: Option<String>) -> Self {
        Self {
            path,
            bytes_b64: None,
            old_version,
            tree: false,
        }
    }

    pub fn remove_tree(path: String) -> Self {
        Self {
            path,
            bytes_b64: None,
            old_version: None,
            tree: true,
        }
    }

    fn target(&self, root: &Path) -> Result<PathBuf> {
        let relative = Path::new(&self.path);
        let plain = !self.path.is_empty()
            && relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)));
        if plain {
            Ok(root.join(relative))
        } else {
            Err(
                invalid_journal("the journal names a path outside the store")
                    .with_detail("path", self.path.clone()),
            )
        }
    }

    fn apply(&self, root: &Path, dirty: &mut BTreeSet<PathBuf>) -> Result<()> {
        let target = self.target(root)?;
        if self.tree {
            return remove_tree_durable(&target);
        }
        match &self.bytes_b64 {
            Some(encoded) => {
                let bytes = STANDARD.decode(encoded).map_err(|source| {
                    invalid_journal(source.to_string()).with_detail("path", self.path.clone())
                })?;
                place(&target, &bytes)?;
            }
            None => {
                unlink(&target)?;
            }
        }
        dirty.insert(parent_of(&target)?.to_path_buf());
        Ok(())
    }
}

fn apply_all(root: &Path, journal: &Journal, after_each: impl Fn(usize)) -> Result<()> {
    let mut dirty = BTreeSet::new();
    for (index, entry) in journal.entries.iter().enumerate() {
        entry.apply(root, &mut dirty)?;
        after_each(index + 1);
    }
    for dir in dirty {
        sync_dir(&dir)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Journal {
    pub tx_id: String,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TxResult>,
    pub entries: Vec<JournalEntry>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub commit_pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_tx: Option<PendingTx>,
}

fn invalid_journal(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::Io, message).with_detail("reason", "invalid_journal")
}

fn journal_path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(JOURNAL_FILE)
}

fn finish(root: &Path, journal: &Journal) -> Result<()> {
    if let Some(result) = &journal.result {
        txlog::record(root, result)?;
    }
    if let Some(pending) = &journal.pending_tx {
        crate::git::append_pending(root, pending)?;
    }
    if read_generation(root)? < journal.generation {
        replace_unsynced(
            &root.join(STATE_DIR).join(GENERATION_FILE),
            format!("{}\n", journal.generation).as_bytes(),
        )?;
    }
    unlink(&journal_path(root))?;
    Ok(())
}

pub(crate) fn commit(root: &Path, journal: &Journal) -> Result<()> {
    hooks::before_journal();
    let bytes = serde_json::to_vec(journal).expect("a journal serialises");
    write_atomic(&journal_path(root), &bytes)?;
    hooks::after_journal();
    hooks::after_files(0);
    apply_all(root, journal, hooks::after_files)?;
    if journal.commit_pending {
        hooks::before_git_commit();
        commit_journal(root, journal)?;
    }
    finish(root, journal)
}

pub(crate) fn recover(root: &Path) -> Result<bool> {
    let bytes = match fs::read(journal_path(root)) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == ErrorKind::NotFound => {
            remove_temp_files(&root.join(STATE_DIR))?;
            return Ok(false);
        }
        Err(source) => return Err(source.into()),
    };
    let journal: Journal =
        serde_json::from_slice(&bytes).map_err(|source| invalid_journal(source.to_string()))?;
    remove_temp_files(&root.join(STATE_DIR))?;
    for entry in &journal.entries {
        if let Some(parent) = entry.target(root)?.parent() {
            remove_temp_files(parent)?;
        }
    }
    apply_all(root, &journal, |_| {})?;
    if journal.commit_pending {
        commit_journal(root, &journal)?;
    }
    finish(root, &journal)?;
    Ok(true)
}

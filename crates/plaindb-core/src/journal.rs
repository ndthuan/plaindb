use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use crate::atomic::{
    parent_of, place, remove_temp_files, remove_tree_at, replace_unsynced, sync_dir, unlink,
    write_atomic_at,
};
use crate::config::Durability;
use crate::error::{Error, ErrorCode, Result};
use crate::git::{PendingTx, commit_journal};
use crate::hooks;
use crate::scan::{GENERATION_FILE, read_generation};
use crate::store::{STATE_DIR, WriterState};
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

    fn apply(
        &self,
        root: &Path,
        durability: Durability,
        dirty: &mut BTreeSet<PathBuf>,
    ) -> Result<()> {
        let target = self.target(root)?;
        if self.tree {
            return remove_tree_at(&target, durability);
        }
        match &self.bytes_b64 {
            Some(encoded) => {
                let bytes = STANDARD.decode(encoded).map_err(|source| {
                    invalid_journal(source.to_string()).with_detail("path", self.path.clone())
                })?;
                place(&target, &bytes, durability)?;
            }
            None => {
                unlink(&target)?;
            }
        }
        dirty.insert(parent_of(&target)?.to_path_buf());
        Ok(())
    }
}

fn apply_all(
    root: &Path,
    journal: &Journal,
    durability: Durability,
    after_each: impl Fn(usize),
) -> Result<()> {
    let mut dirty = BTreeSet::new();
    for (index, entry) in journal.entries.iter().enumerate() {
        entry.apply(root, durability, &mut dirty)?;
        after_each(index + 1);
    }
    for dir in dirty {
        sync_dir(&dir, durability)?;
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

fn finish(root: &Path, state: &mut WriterState, journal: &Journal) -> Result<()> {
    if let Some(result) = &journal.result {
        state.tx_log.record(root, result, state.durability)?;
    }
    if let Some(pending) = &journal.pending_tx {
        crate::git::append_pending(root, pending, state.durability)?;
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

pub(crate) fn commit(root: &Path, state: &mut WriterState, journal: &Journal) -> Result<()> {
    hooks::before_journal();
    let bytes = serde_json::to_vec(journal).expect("a journal serialises");
    write_atomic_at(&journal_path(root), &bytes, state.durability)?;
    hooks::after_journal();
    hooks::after_files(0);
    apply_all(root, journal, state.durability, hooks::after_files)?;
    if journal.commit_pending {
        hooks::before_git_commit();
        commit_journal(root, journal)?;
    }
    finish(root, state, journal)
}

pub(crate) fn recover(root: &Path, state: &mut WriterState) -> Result<bool> {
    let durability = state.durability;
    let bytes = match fs::read(journal_path(root)) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == ErrorKind::NotFound => {
            remove_temp_files(&root.join(STATE_DIR), durability)?;
            return Ok(false);
        }
        Err(source) => return Err(source.into()),
    };
    let journal: Journal =
        serde_json::from_slice(&bytes).map_err(|source| invalid_journal(source.to_string()))?;
    remove_temp_files(&root.join(STATE_DIR), durability)?;
    for entry in &journal.entries {
        if let Some(parent) = entry.target(root)?.parent() {
            remove_temp_files(parent, durability)?;
        }
    }
    apply_all(root, &journal, durability, |_| {})?;
    if journal.commit_pending {
        commit_journal(root, &journal)?;
    }
    finish(root, state, &journal)?;
    Ok(true)
}

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use crate::atomic::{
    flush, parent_of, place, place_ordered, remove_temp_files, remove_tree, replace_unsynced,
    sync_dir, unlink, write_atomic_at,
};
use crate::boot::boot_id;
use crate::config::Durability;
use crate::error::{Error, ErrorCode, Result};
use crate::git::{
    PendingTx, add_missing_commit, commit_journal, commits_are_missing, read_missing_commits,
    retry_missing_commits,
};
use crate::hooks;
use crate::names::new_id;
use crate::scan::{GENERATION_FILE, read_generation};
use crate::store::{STATE_DIR, WriterState};
use crate::txlog::file_stamp;
use crate::version::version_of;
use crate::write::{Deferred, TxResult};

pub const JOURNAL_FILE: &str = "journal.jsonl";

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

    fn bytes(&self) -> Result<Option<Vec<u8>>> {
        self.bytes_b64
            .as_ref()
            .map(|encoded| {
                STANDARD.decode(encoded).map_err(|source| {
                    invalid_journal(source.to_string()).with_detail("path", self.path.clone())
                })
            })
            .transpose()
    }

    pub fn fill_old_version(&mut self, root: &Path) -> Result<()> {
        if self.old_version.is_none() && !self.tree {
            self.old_version = read_version(&self.target(root)?)?;
        }
        Ok(())
    }

    fn is_inside(&self, tree: &str) -> bool {
        self.path
            .strip_prefix(tree)
            .is_some_and(|rest| rest.starts_with('/'))
    }

    fn apply(&self, root: &Path, durability: Durability, replay: Replay) -> Result<()> {
        let target = self.target(root)?;
        if self.tree {
            if replay == Replay::KeepingLaterEdits {
                return Ok(());
            }
            return remove_tree(&target);
        }
        let bytes = self.bytes()?;
        if replay != Replay::Always {
            let current = read_version(&target)?;
            let wanted = bytes.as_deref().map(version_of);
            let created_then_removed = current.is_none() && self.old_version.is_none();
            if current == wanted || current != self.old_version || created_then_removed {
                return Ok(());
            }
        }
        match bytes {
            Some(bytes) => place_ordered(&target, &bytes, durability)?,
            None => {
                unlink(&target)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replay {
    Always,
    Guarded,
    KeepingLaterEdits,
}

fn read_version(path: &Path) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(version_of(&bytes))),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(source.into()),
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot: Option<String>,
}

fn invalid_journal(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::Io, message).with_detail("reason", "invalid_journal")
}

fn journal_path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(JOURNAL_FILE)
}

fn encode_line(journal: &Journal) -> Vec<u8> {
    let json = serde_json::to_vec(journal).expect("a journal serialises");
    let mut line = version_of(&json).into_bytes();
    line.push(b' ');
    line.extend_from_slice(&json);
    line.push(b'\n');
    line
}

fn decode_line(line: &[u8]) -> Option<Journal> {
    let space = line.iter().position(|byte| *byte == b' ')?;
    let (sum, json) = (&line[..space], &line[space + 1..]);
    if version_of(json).as_bytes() != sum {
        return None;
    }
    serde_json::from_slice(json).ok()
}

pub const REJECTED_PREFIX: &str = "journal.rejected-";

struct Records {
    journals: Vec<Journal>,
    valid_len: u64,
    file_len: u64,
    rejected: Option<Vec<u8>>,
}

fn read_records(root: &Path) -> Result<Records> {
    let bytes = match fs::read(journal_path(root)) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == ErrorKind::NotFound => Vec::new(),
        Err(source) => return Err(source.into()),
    };
    let mut journals = Vec::new();
    let mut offset = 0;
    let mut rejected = None;
    while let Some(end) = bytes[offset..].iter().position(|byte| *byte == b'\n') {
        let Some(journal) = decode_line(&bytes[offset..offset + end]) else {
            if offset + end + 1 < bytes.len() {
                rejected = Some(bytes[offset..].to_vec());
            }
            break;
        };
        journals.push(journal);
        offset += end + 1;
    }
    let as_len = |value: usize| u64::try_from(value).unwrap_or(u64::MAX);
    Ok(Records {
        journals,
        valid_len: as_len(offset),
        file_len: as_len(bytes.len()),
        rejected,
    })
}

fn truncate(root: &Path, len: u64, durability: Durability) -> Result<()> {
    let file = OpenOptions::new().write(true).open(journal_path(root))?;
    file.set_len(len)?;
    flush(&file, durability)
}

fn keep_rejected(root: &Path, bytes: &[u8], durability: Durability) -> Result<()> {
    let name = format!("{REJECTED_PREFIX}{}", new_id());
    write_atomic_at(&root.join(STATE_DIR).join(&name), bytes, durability)?;
    tracing::warn!("the journal held a damaged record; the rest was moved to {name}");
    Ok(())
}

fn read_settled(root: &Path, durability: Durability) -> Result<Vec<Journal>> {
    let records = read_records(root)?;
    if records.valid_len < records.file_len {
        if let Some(rejected) = &records.rejected {
            keep_rejected(root, rejected, durability)?;
        }
        truncate(root, records.valid_len, Durability::Off)?;
    }
    Ok(records.journals)
}

pub(crate) fn rejected_files(root: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(root.join(STATE_DIR)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(REJECTED_PREFIX))
        .collect();
    names.sort();
    names
}

fn append(root: &Path, journal: &Journal, durability: Durability) -> Result<()> {
    let path = journal_path(root);
    let created = !path.exists();
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(&encode_line(journal))?;
    flush(&file, durability)?;
    if created {
        sync_dir(parent_of(&path)?, durability)?;
    }
    Ok(())
}

fn finish(root: &Path, journal: &Journal) -> Result<()> {
    if let Some(pending) = &journal.pending_tx {
        crate::git::append_pending(root, pending, Durability::Off)?;
    }
    if read_generation(root)? < journal.generation {
        replace_unsynced(
            &root.join(STATE_DIR).join(GENERATION_FILE),
            format!("{}\n", journal.generation).as_bytes(),
        )?;
    }
    Ok(())
}

fn recovery_failed(error: Error) -> Error {
    Error {
        code: ErrorCode::Io,
        ..error
    }
    .with_detail("reason", "recovery_failed")
}

fn roll_forward(root: &Path, journal: &Journal, durability: Durability) -> Result<()> {
    for entry in &journal.entries {
        entry.apply(root, durability, Replay::Always)?;
    }
    if journal.commit_pending {
        add_missing_commit(root, &journal.tx_id, None)?;
    }
    finish(root, journal)
}

fn make_missing_commits(root: &Path, journals: &[Journal]) {
    if let Err(error) = retry_missing_commits(root, journals) {
        tracing::warn!("the missing git commits will be retried: {}", error.message);
    }
}

fn replay_settled(
    root: &Path,
    journal: &Journal,
    later: &[Journal],
    durability: Durability,
) -> Result<()> {
    for entry in &journal.entries {
        let recreated = entry.tree
            && later
                .iter()
                .flat_map(|journal| &journal.entries)
                .any(|later_entry| later_entry.is_inside(&entry.path));
        let replay = if recreated {
            Replay::KeepingLaterEdits
        } else {
            Replay::Guarded
        };
        entry.apply(root, durability, replay)?;
    }
    Ok(())
}

fn remove_stray_temp_files(root: &Path, journals: &[Journal]) -> Result<()> {
    let mut dirs = BTreeSet::new();
    dirs.insert(root.join(STATE_DIR));
    for entry in journals.iter().flat_map(|journal| &journal.entries) {
        if let Some(parent) = entry.target(root)?.parent() {
            dirs.insert(parent.to_path_buf());
        }
    }
    for dir in dirs {
        remove_temp_files(&dir)?;
    }
    Ok(())
}

fn remember(root: &Path, state: &mut WriterState, journals: &[Journal]) -> Result<()> {
    state.journaled = journals
        .iter()
        .filter_map(|journal| journal.result.clone())
        .collect();
    state.journal_seen = Some(file_stamp(&journal_path(root))?);
    Ok(())
}

fn deferred(step: &str, error: &Error) -> Deferred {
    Deferred {
        step: step.to_owned(),
        message: error.message.clone(),
    }
}

fn apply_files(root: &Path, journal: &Journal, durability: Durability) -> Result<()> {
    hooks::after_files(0);
    for (index, entry) in journal.entries.iter().enumerate() {
        entry.apply(root, durability, Replay::Always)?;
        hooks::after_files(index + 1);
    }
    Ok(())
}

fn make_commit(root: &Path, journal: &Journal) -> Result<()> {
    if let Some(missing) = read_missing_commits(root)? {
        return Err(Error::new(
            ErrorCode::Io,
            format!("earlier commits are missing: {}", missing.message),
        ));
    }
    hooks::before_git_commit();
    commit_journal(root, journal, 1)
}

fn defer_commit(root: &Path, journal: &Journal, error: &Error) -> Deferred {
    if let Err(record_error) = add_missing_commit(root, &journal.tx_id, Some(&error.message)) {
        tracing::warn!(
            "the missing commit could not be recorded: {}",
            record_error.message
        );
    }
    deferred("git", error)
}

pub(crate) fn commit(
    root: &Path,
    state: &mut WriterState,
    journal: &Journal,
) -> Result<Option<Deferred>> {
    let durability = state.durability;
    let path = journal_path(root);
    let in_step = state.journal_seen == Some(file_stamp(&path)?);
    hooks::before_journal();
    state.journal_seen = None;
    append(root, journal, durability)?;
    hooks::after_journal();
    if let Err(error) = apply_files(root, journal, durability) {
        tracing::warn!(
            "the files of a committed write will be finished later: {}",
            error.message
        );
        return Ok(Some(deferred("files", &error)));
    }
    let mut outcome = None;
    if journal.commit_pending
        && let Err(error) = make_commit(root, journal)
    {
        tracing::warn!(
            "the git commit of a write will be made later: {}",
            error.message
        );
        outcome = Some(defer_commit(root, journal, &error));
    }
    if let Err(error) = finish(root, journal) {
        tracing::warn!(
            "a committed write will be finished later: {}",
            error.message
        );
        return Ok(Some(deferred("finish", &error)));
    }
    if in_step && let Ok(stamp) = file_stamp(&path) {
        state.journal_seen = Some(stamp);
        if let Some(result) = &journal.result {
            state.journaled.push(result.clone());
        }
    }
    if journal
        .generation
        .is_multiple_of(state.checkpoint_interval.max(1))
        && let Err(error) = checkpoint(root, state)
    {
        tracing::warn!(
            "the checkpoint failed and will be retried: {}",
            error.message
        );
    }
    Ok(outcome)
}

pub(crate) fn settle(root: &Path, state: &mut WriterState) -> Result<bool> {
    let path = journal_path(root);
    if !commits_are_missing(root) && state.journal_seen == Some(file_stamp(&path)?) {
        return Ok(false);
    }
    state.journal_seen = None;
    let journals = read_settled(root, state.durability)?;
    let generation = read_generation(root)?;
    let unfinished: Vec<&Journal> = journals
        .iter()
        .filter(|journal| journal.generation > generation)
        .collect();
    if !unfinished.is_empty() {
        remove_stray_temp_files(root, &journals).map_err(recovery_failed)?;
    }
    for journal in &unfinished {
        roll_forward(root, journal, state.durability).map_err(recovery_failed)?;
    }
    make_missing_commits(root, &journals);
    remember(root, state, &journals)?;
    Ok(!unfinished.is_empty())
}

pub(crate) fn recover(root: &Path, state: &mut WriterState) -> Result<bool> {
    state.journal_seen = None;
    let journals = read_settled(root, state.durability)?;
    remove_stray_temp_files(root, &journals).map_err(recovery_failed)?;
    if journals.is_empty() {
        remember(root, state, &journals)?;
        return Ok(false);
    }
    let generation = read_generation(root)?;
    let boot = boot_id();
    for (index, journal) in journals.iter().enumerate() {
        if journal.generation > generation {
            roll_forward(root, journal, state.durability).map_err(recovery_failed)?;
        } else if boot.is_none() || journal.boot != boot {
            replay_settled(root, journal, &journals[index + 1..], state.durability)
                .map_err(recovery_failed)?;
        }
    }
    make_missing_commits(root, &journals);
    remember(root, state, &journals)?;
    checkpoint(root, state)?;
    Ok(true)
}

fn ancestors(root: &Path, target: &Path, dirs: &mut BTreeSet<PathBuf>) {
    let mut current = target.parent();
    while let Some(dir) = current {
        if !dir.starts_with(root) {
            break;
        }
        dirs.insert(dir.to_path_buf());
        if dir == root {
            break;
        }
        current = dir.parent();
    }
}

pub(crate) fn checkpoint(root: &Path, state: &mut WriterState) -> Result<()> {
    let durability = state.durability;
    let records = read_records(root)?;
    if records.file_len == 0 {
        return Ok(());
    }
    if commits_are_missing(root) {
        return Ok(());
    }
    let generation = read_generation(root)?;
    if records
        .journals
        .iter()
        .any(|journal| journal.generation > generation)
    {
        return Ok(());
    }
    hooks::during_checkpoint(0);
    let mut dirs = BTreeSet::new();
    dirs.insert(root.join(STATE_DIR));
    for entry in records.journals.iter().flat_map(|journal| &journal.entries) {
        ancestors(root, &entry.target(root)?, &mut dirs);
    }
    let results: Vec<&TxResult> = records
        .journals
        .iter()
        .filter_map(|journal| journal.result.as_ref())
        .collect();
    state.tx_log.record_all(root, &results, durability)?;
    place(
        &root.join(STATE_DIR).join(GENERATION_FILE),
        format!("{generation}\n").as_bytes(),
        durability,
    )?;
    hooks::during_checkpoint(1);
    for dir in dirs {
        match sync_dir(&dir, durability) {
            Err(error) if !dir.exists() => tracing::debug!("{}", error.message),
            other => other?,
        }
    }
    hooks::during_checkpoint(2);
    truncate(root, 0, durability)?;
    hooks::during_checkpoint(3);
    state.journaled.clear();
    state.journal_seen = Some(file_stamp(&journal_path(root))?);
    Ok(())
}

pub(crate) fn record_count(root: &Path) -> usize {
    fs::read(journal_path(root)).map_or(0, |bytes| {
        bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .count()
    })
}

pub(crate) fn has_records(root: &Path) -> bool {
    fs::metadata(journal_path(root)).is_ok_and(|metadata| metadata.len() > 0)
}

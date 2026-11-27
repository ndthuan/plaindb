mod command;
mod commit;
mod doctor;
mod foreign;
mod gc;
mod history;
mod message;
mod missing;
mod pending;
mod push;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use gix::ObjectId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::atomic::{TEMP_MARKER, remove_durable, replace_unsynced};
use crate::config::{CommitMode, PushMode, StoreConfig, to_file_bytes};
use crate::error::{Error, ErrorCode, Result};
use crate::journal::{Journal, JournalEntry};
use crate::names::{new_id, validate_db_name};
use crate::store::{DB_FILE, STATE_DIR, STORE_FILE, Store, read_config};
use crate::write::{Op, OpResult};

pub use command::GIT_BINARY_ENV;
pub use gc::{GcOutcome, GcReport};
pub use history::{At, DEFAULT_HISTORY_LIMIT, HistoryEntry};
pub use message::{ACTOR_TRAILER, OP_COUNT_TRAILER, TX_ID_TRAILER};
pub use missing::{GIT_COMMIT_PENDING_WARNING, MISSING_COMMITS_FILE, MissingCommits};
pub use pending::PendingTx;
pub use push::PushOutcome;

pub(crate) use commit::commit_journal;
pub(crate) use doctor::checks as doctor_checks;
pub(crate) use foreign::check_head;
pub(crate) use missing::{
    add as add_missing_commit, exists as commits_are_missing, read as read_missing_commits,
    retry as retry_missing_commits,
};
pub(crate) use pending::append as append_pending;
pub(crate) use push::{head_branch, head_shown};

pub const GITIGNORE_FILE: &str = ".gitignore";
pub const GITIGNORE_LINE: &str = ".plaindb/";
pub const LAST_COMMIT_FILE: &str = "last-commit";
pub const WARNINGS_FILE: &str = "warnings.json";
pub const FOREIGN_COMMIT_WARNING: &str = "foreign_commit";
pub const GIT_NOT_FOUND_WARNING: &str = "git_not_found";
pub const GIT_BINARY_IGNORED_WARNING: &str = "git_binary_ignored";
pub const GIT_REPOSITORY_MISSING_WARNING: &str = "git_repository_missing";
pub const HEAD_OFF_BRANCH_WARNING: &str = "head_off_branch";
const GIT_DIR: &str = ".git";
const FALLBACK_NAME: &str = "plaindb";
const FALLBACK_EMAIL: &str = "plaindb@localhost";
pub(crate) const INIT_STORE_SUBJECT: &str = "init store";
const ENABLE_SUBJECT: &str = "git: enable";

pub type CommitId = String;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatus {
    pub enabled: bool,
    pub commit: CommitMode,
    pub branch: String,
    pub head: Option<CommitId>,
    pub pending_txs: u64,
    pub pending_paths: u64,
    pub push: PushMode,
    pub remote: String,
    pub remote_url: Option<String>,
    pub unpushed: u64,
    pub last_push_at: Option<String>,
    pub last_push_error: Option<String>,
    pub remote_ahead: bool,
    pub git_bytes: u64,
    pub last_gc_at: Option<String>,
    pub last_gc_outcome: Option<String>,
    pub last_gc_error: Option<String>,
    pub git_binary: String,
    pub git_binary_source: String,
    pub git_binary_found: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ForeignWarning {
    pub code: String,
    pub ids: Vec<String>,
    pub at: String,
    pub head: String,
}

pub(crate) fn git_error(error: impl std::fmt::Display) -> Error {
    Error::new(ErrorCode::Io, error.to_string()).with_detail("reason", "git")
}

pub(crate) fn git_disabled() -> Error {
    Error::new(ErrorCode::GitDisabled, "git is not enabled for this store")
}

pub(crate) fn repository_missing() -> Error {
    Error::new(
        ErrorCode::GitDisabled,
        "git is on but the store has no .git directory: run plaindb git enable to start a new history",
    )
    .with_detail("reason", "repository_missing")
}

pub(crate) fn has_repository(root: &Path) -> bool {
    root.join(GIT_DIR).is_dir()
}

fn is_true(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn open_options() -> gix::open::Options {
    let mut permissions = gix::open::Permissions::default();
    permissions.env.git_prefix = gix::sec::Permission::Deny;
    permissions.env.objects = gix::sec::Permission::Deny;
    permissions.config.env = false;
    let mut options = gix::open::Options::default();
    if std::env::var("GIT_CONFIG_NOSYSTEM").is_ok_and(|value| is_true(&value)) {
        permissions.config.system = false;
    } else if let Some(path) = std::env::var_os("GIT_CONFIG_SYSTEM").filter(|path| !path.is_empty())
    {
        options = options.system_config_path(path);
    }
    if let Some(path) = std::env::var_os("GIT_CONFIG_GLOBAL") {
        permissions.config.user = false;
        permissions.config.git = false;
        if !path.is_empty() {
            permissions.config.git_binary = true;
            options = options.git_installation_config_path(path);
        }
    }
    options.permissions(permissions)
}

pub(crate) fn open(root: &Path) -> Result<gix::Repository> {
    if !has_repository(root) {
        return Err(repository_missing());
    }
    gix::open_opts(root, open_options()).map_err(git_error)
}

pub(crate) fn head_id(repo: &gix::Repository) -> Result<Option<ObjectId>> {
    Ok(repo.head().map_err(git_error)?.id().map(gix::Id::detach))
}

fn config_value(repo: &gix::Repository, key: &str) -> Option<String> {
    repo.config_snapshot()
        .string(key)
        .map(|value| message::one_line(&value.to_string()))
        .filter(|value| !value.is_empty())
}

fn os_user() -> Option<String> {
    ["USER", "USERNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| message::one_line(&value))
        .find(|value| !value.is_empty())
}

pub(crate) fn resolve_actor(repo: &gix::Repository, given: Option<&str>) -> String {
    given
        .map(message::one_line)
        .filter(|value| !value.is_empty())
        .or_else(|| config_value(repo, "user.email"))
        .or_else(os_user)
        .unwrap_or_else(|| FALLBACK_NAME.to_owned())
}

pub(crate) fn signature(repo: &gix::Repository) -> gix::actor::Signature {
    gix::actor::Signature {
        name: config_value(repo, "user.name")
            .unwrap_or_else(|| FALLBACK_NAME.to_owned())
            .into(),
        email: config_value(repo, "user.email")
            .unwrap_or_else(|| FALLBACK_EMAIL.to_owned())
            .into(),
        time: gix::date::Time::now_local_or_utc(),
    }
}

pub(super) fn state_file(root: &Path, name: &str) -> PathBuf {
    root.join(STATE_DIR).join(name)
}

pub(super) fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(source.into()),
    }
}

pub(crate) fn read_last_commit(root: &Path) -> Result<Option<ObjectId>> {
    let Some(bytes) = read_optional(&state_file(root, LAST_COMMIT_FILE))? else {
        return Ok(None);
    };
    Ok(ObjectId::from_hex(String::from_utf8_lossy(&bytes).trim().as_bytes()).ok())
}

pub(crate) fn write_last_commit(root: &Path, id: ObjectId) -> Result<()> {
    replace_unsynced(
        &state_file(root, LAST_COMMIT_FILE),
        format!("{id}\n").as_bytes(),
    )
}

pub(crate) fn read_warning(root: &Path) -> Result<Option<ForeignWarning>> {
    let Some(bytes) = read_optional(&state_file(root, WARNINGS_FILE))? else {
        return Ok(None);
    };
    Ok(serde_json::from_slice(&bytes).ok())
}

pub(crate) fn write_warning(root: &Path, warning: &ForeignWarning) -> Result<()> {
    replace_unsynced(&state_file(root, WARNINGS_FILE), &to_file_bytes(warning))
}

pub(crate) fn clear_warning(root: &Path) -> Result<()> {
    remove_durable(&state_file(root, WARNINGS_FILE))
}

fn is_hidden(name: &str) -> bool {
    name.starts_with('.') || name.contains(TEMP_MARKER)
}

fn collect_files(dir: &Path, prefix: &str, files: &mut BTreeMap<String, PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if is_hidden(&name) {
            continue;
        }
        let kind = entry.file_type()?;
        let relative = format!("{prefix}/{name}");
        if kind.is_dir() {
            collect_files(&entry.path(), &relative, files)?;
        } else if kind.is_file() {
            files.insert(relative, entry.path());
        }
    }
    Ok(())
}

pub(crate) fn worktree_files(root: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let mut files = BTreeMap::new();
    for name in [STORE_FILE, GITIGNORE_FILE] {
        let path = root.join(name);
        if path.is_file() {
            files.insert(name.to_owned(), path);
        }
    }
    for db in crate::store::child_directories(root, DB_FILE, validate_db_name)? {
        collect_files(&root.join(&db), &db, &mut files)?;
    }
    Ok(files)
}

pub(crate) fn in_scope(path: &str) -> bool {
    if path == STORE_FILE || path == GITIGNORE_FILE {
        return true;
    }
    path.split_once('/')
        .is_some_and(|(db, _)| validate_db_name(db).is_ok())
}

fn ensure_gitignore(root: &Path) -> Result<()> {
    let path = root.join(GITIGNORE_FILE);
    let existing = read_optional(&path)?.map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    let text = match existing {
        Some(text) if text.lines().any(|line| line.trim() == GITIGNORE_LINE) => return Ok(()),
        Some(text) if text.is_empty() || text.ends_with('\n') => {
            format!("{text}{GITIGNORE_LINE}\n")
        }
        Some(text) => format!("{text}\n{GITIGNORE_LINE}\n"),
        None => format!("{GITIGNORE_LINE}\n"),
    };
    crate::atomic::write_atomic_at(&path, text.as_bytes(), read_config(root)?.durability)
}

fn init_repo(root: &Path, branch: &str) -> Result<()> {
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    use gix::refs::{FullName, Target};

    let branch_ref = FullName::try_from(format!("refs/heads/{branch}"))
        .map_err(|_| Error::invalid_name("branch", branch))?;
    let repo: gix::Repository = gix::ThreadSafeRepository::init_opts(
        root,
        gix::create::Kind::WithWorktree,
        gix::create::Options::default(),
        open_options(),
    )
    .map_err(git_error)?
    .into();
    let signature = signature(&repo);
    let mut time = gix::date::parse::TimeBuf::default();
    let edit = RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "init".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(branch_ref),
        },
        name: "HEAD".try_into().map_err(git_error)?,
        deref: false,
    };
    repo.edit_references_as(Some(edit), Some(signature.to_ref(&mut time)))
        .map_err(git_error)?;
    Ok(())
}

pub(crate) struct GitPlan {
    pub commit_message: Option<String>,
    pub pending: Option<PendingTx>,
}

impl GitPlan {
    pub fn none() -> Self {
        Self {
            commit_message: None,
            pending: None,
        }
    }

    pub fn into_journal(
        self,
        tx_id: String,
        generation: u64,
        entries: Vec<JournalEntry>,
    ) -> Journal {
        Journal {
            tx_id,
            generation,
            result: None,
            entries,
            commit_pending: self.commit_message.is_some(),
            commit_message: self.commit_message,
            pending_tx: self.pending,
            boot: crate::boot::boot_id(),
        }
    }
}

fn plan(
    root: &Path,
    config: &StoreConfig,
    tx_id: &str,
    actor: Option<&str>,
    lines: Vec<String>,
    message: impl FnOnce(&str) -> String,
) -> Result<GitPlan> {
    if !config.git {
        return Ok(GitPlan::none());
    }
    let repo = open(root)?;
    let actor = resolve_actor(&repo, actor);
    Ok(match config.commit {
        CommitMode::Each => GitPlan {
            commit_message: Some(message(&actor)),
            pending: None,
        },
        CommitMode::Manual => GitPlan {
            commit_message: None,
            pending: Some(PendingTx {
                tx_id: tx_id.to_owned(),
                actor,
                ops: lines,
            }),
        },
    })
}

pub(crate) fn plan_tx(
    root: &Path,
    config: &StoreConfig,
    tx_id: &str,
    actor: Option<&str>,
    ops: &[Op],
    results: &[OpResult],
) -> Result<GitPlan> {
    let summaries = message::summarize(ops, results);
    let lines = summaries.iter().map(ToString::to_string).collect();
    plan(root, config, tx_id, actor, lines, |actor| {
        message::tx_message(&summaries, tx_id, actor)
    })
}

pub(crate) fn plan_admin(
    root: &Path,
    config: &StoreConfig,
    tx_id: &str,
    subject: &str,
) -> Result<GitPlan> {
    plan(
        root,
        config,
        tx_id,
        None,
        vec![subject.to_owned()],
        |actor| message::admin_message(subject, tx_id, actor),
    )
}

pub(crate) fn prepare_write(root: &Path) -> Result<bool> {
    let config = read_config(root)?;
    if !config.git {
        return Ok(false);
    }
    let repo = open(root)?;
    if head_id(&repo)?.is_none() {
        commit_worktree(root, ENABLE_SUBJECT)?;
        return Ok(true);
    }
    check_head(root, &repo)
}

fn commit_worktree(root: &Path, subject: &str) -> Result<Option<ObjectId>> {
    let changes = commit::worktree_changes(root)?;
    if changes.is_empty() {
        return Ok(None);
    }
    let repo = open(root)?;
    let tx_id = new_id();
    let actor = resolve_actor(&repo, None);
    let text = message::admin_message(subject, &tx_id, &actor);
    commit::commit_changes(root, &changes, &text, &tx_id, 1).map(Some)
}

impl Store {
    pub fn init_with_git(dir: impl AsRef<Path>) -> Result<Self> {
        Self::init_with(
            dir,
            StoreConfig {
                git: true,
                ..StoreConfig::default()
            },
        )
    }

    pub fn enable_git(&self) -> Result<()> {
        self.turn_git_on(ENABLE_SUBJECT)
    }

    pub(crate) fn turn_git_on(&self, subject: &str) -> Result<()> {
        let _push = self.push_guard();
        let _lock = self.begin_write_without_repository()?;
        let mut config = read_config(&self.root)?;
        if config.git {
            if has_repository(&self.root) {
                return Ok(());
            }
            init_repo(&self.root, &config.branch)?;
            ensure_gitignore(&self.root)?;
            commit_worktree(&self.root, subject)?;
            return Ok(());
        }
        if !self.root.join(GIT_DIR).exists() {
            init_repo(&self.root, &config.branch)?;
        }
        ensure_gitignore(&self.root)?;
        config.git = true;
        crate::atomic::write_atomic_at(
            &self.root.join(STORE_FILE),
            &to_file_bytes(&config),
            config.durability,
        )?;
        commit_worktree(&self.root, subject)?;
        Ok(())
    }

    pub fn set_commit_mode(&self, mode: CommitMode) -> Result<()> {
        self.set_setting(
            "commit",
            match mode {
                CommitMode::Each => "each",
                CommitMode::Manual => "manual",
            },
        )
    }

    pub fn commit(&self, message: Option<&str>, actor: Option<&str>) -> Result<Option<CommitId>> {
        let _push = self.push_guard();
        let _lock = self.begin_write()?;
        if !read_config(&self.root)?.git {
            return Err(git_disabled());
        }
        self.commit_pending(message, actor)
    }

    pub(crate) fn commit_pending(
        &self,
        message: Option<&str>,
        actor: Option<&str>,
    ) -> Result<Option<CommitId>> {
        let repo = open(&self.root)?;
        let committed: HashSet<String> = match head_id(&repo)? {
            Some(head) => commit::tx_ids_of(&repo, head)?.into_iter().collect(),
            None => HashSet::new(),
        };
        let pending: Vec<PendingTx> = pending::read(&self.root)?
            .into_iter()
            .filter(|entry| !committed.contains(&entry.tx_id))
            .collect();
        let changes = commit::worktree_changes(&self.root)?;
        if changes.is_empty() {
            pending::clear(&self.root)?;
            return Ok(None);
        }
        let actor = resolve_actor(&repo, actor);
        let fallback_tx_id = new_id();
        let lines: Vec<message::PendingLine<'_>> = pending
            .iter()
            .map(|entry| message::PendingLine {
                tx_id: &entry.tx_id,
                op_count: entry.ops.len(),
            })
            .collect();
        let text = message::manual_message(message, &lines, &fallback_tx_id, &actor);
        let tx_id = lines
            .first()
            .map_or(fallback_tx_id.as_str(), |line| line.tx_id);
        let id = commit::commit_changes(&self.root, &changes, &text, tx_id, 1)?;
        pending::clear(&self.root)?;
        Ok(Some(id.to_string()))
    }

    pub(crate) fn git_status(&self) -> Result<GitStatus> {
        status_at(&self.root)
    }

    pub(crate) fn git_warning(&self) -> Result<Option<(PathBuf, Value)>> {
        Ok(read_warning(&self.root)?.map(|warning| {
            (
                self.root.join(GIT_DIR),
                serde_json::json!({ "ids": warning.ids, "at": warning.at, "head": warning.head }),
            )
        }))
    }
}

pub(crate) fn status_at(root: &Path) -> Result<GitStatus> {
    let config = read_config(root)?;
    let binary = command::resolve(root);
    let found = command::is_available(&binary);
    let mut status = GitStatus {
        enabled: config.git,
        commit: config.commit,
        branch: config.branch.clone(),
        head: None,
        pending_txs: 0,
        pending_paths: 0,
        push: config.push,
        remote: config.remote.clone(),
        remote_url: None,
        unpushed: 0,
        last_push_at: None,
        last_push_error: None,
        remote_ahead: false,
        git_bytes: 0,
        last_gc_at: None,
        last_gc_outcome: None,
        last_gc_error: None,
        git_binary: binary.program,
        git_binary_source: binary.source.to_owned(),
        git_binary_found: found,
    };
    if !config.git || !root.join(GIT_DIR).exists() {
        return Ok(status);
    }
    let repo = open(root)?;
    status.head = head_id(&repo)?.map(|id| id.to_string());
    let remote_url = push::remote_url(root, &config.remote)?;
    if remote_url.is_some() {
        status.unpushed = push::unpushed(root, &config)?;
    }
    let state = push::read_state(root)?;
    status.last_push_at = state.last_push_at;
    status.last_push_error = state
        .last_push_error
        .map(|text| push::without_credentials(&text, remote_url.as_deref()));
    status.remote_url = remote_url.as_deref().map(push::display_url);
    status.remote_ahead = state.remote_ahead;
    let gc_state = gc::read_state(root)?;
    status.last_gc_at = gc_state.last_gc_at;
    status.last_gc_outcome = gc_state.last_gc_outcome;
    status.last_gc_error = gc_state.last_gc_error;
    status.git_bytes = push::directory_bytes(&root.join(GIT_DIR));
    if config.commit == CommitMode::Manual {
        status.pending_txs = u64::try_from(pending::read(root)?.len()).unwrap_or(u64::MAX);
        status.pending_paths =
            u64::try_from(commit::worktree_changes(root)?.len()).unwrap_or(u64::MAX);
    }
    Ok(status)
}

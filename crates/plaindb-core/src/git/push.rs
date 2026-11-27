use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::path::Path;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::command::{rejected_as_behind, run_git};
use super::{
    git_disabled, git_error, has_repository, head_id, open, read_optional, repository_missing,
    state_file,
};
use crate::atomic::{ensure_dir, replace_unsynced};
use crate::config::{Durability, validate_remote_url};
use crate::config::{PushMode, StoreConfig, to_file_bytes};
use crate::error::{Error, ErrorCode, Result};
use crate::store::{STATE_DIR, Store, read_config};
use crate::write::now_timestamp;

const PUSH_LOCK_FILE: &str = "push.lock";
const PUSH_STATE_FILE: &str = "push.json";
const PUSH_TIMEOUT: Duration = Duration::from_secs(60);
const WRITE_PUSH_TIMEOUT: Duration = Duration::from_secs(10);
const FIRST_RETRY_DELAY_SECS: u64 = 30;
const MAX_RETRY_DELAY_SECS: u64 = 3_600;
const ATTEMPTS: u32 = 3;
const FIRST_BACKOFF: Duration = Duration::from_secs(2);
const ERROR_CHARS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushOutcome {
    Ok,
    Failed,
    RemoteAhead,
    Deferred,
    GitNotFound,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PushState {
    pub last_push_at: Option<String>,
    pub last_push_error: Option<String>,
    pub remote_ahead: bool,
    pub failures: u32,
    pub next_attempt_at: Option<String>,
}

impl PushState {
    fn mark_pushed(&mut self) {
        self.last_push_at = Some(now_timestamp());
        self.last_push_error = None;
        self.failures = 0;
        self.next_attempt_at = None;
    }

    fn mark_failed(&mut self, text: String) {
        self.failures = self.failures.saturating_add(1);
        let doublings = self.failures.saturating_sub(1).min(16);
        let delay = FIRST_RETRY_DELAY_SECS
            .saturating_mul(1 << doublings)
            .min(MAX_RETRY_DELAY_SECS);
        let at = jiff::Timestamp::now()
            .checked_add(jiff::SignedDuration::from_secs(
                i64::try_from(delay).unwrap_or(i64::MAX),
            ))
            .unwrap_or(jiff::Timestamp::MAX);
        self.next_attempt_at = Some(format!("{at:.3}"));
        self.last_push_error = Some(text);
    }

    fn waits(&self) -> bool {
        self.next_attempt_at
            .as_deref()
            .and_then(|text| text.parse::<jiff::Timestamp>().ok())
            .is_some_and(|at| at > jiff::Timestamp::now())
    }
}

pub(crate) fn read_state(root: &Path) -> Result<PushState> {
    let Some(bytes) = read_optional(&state_file(root, PUSH_STATE_FILE))? else {
        return Ok(PushState::default());
    };
    Ok(serde_json::from_slice(&bytes).unwrap_or_default())
}

fn write_state(root: &Path, state: &PushState) -> Result<()> {
    replace_unsynced(&state_file(root, PUSH_STATE_FILE), &to_file_bytes(state))
}

pub(super) fn truncated(text: &str) -> String {
    text.trim().chars().take(ERROR_CHARS).collect()
}

fn remote_ahead_error() -> Error {
    Error::new(
        ErrorCode::RemoteAhead,
        "the remote holds commits this store lacks",
    )
}

fn git_not_found() -> Error {
    Error::new(ErrorCode::GitNotFound, "the git binary was not found")
}

pub(crate) const DETACHED_HEAD: &str = "detached";

pub(crate) fn head_branch(root: &Path) -> Result<Option<String>> {
    let repo = open(root)?;
    Ok(repo.head_name().map_err(git_error)?.and_then(|name| {
        name.as_bstr()
            .strip_prefix(b"refs/heads/")
            .map(|short| String::from_utf8_lossy(short).into_owned())
    }))
}

pub(crate) fn head_shown(head: Option<&str>) -> &str {
    head.unwrap_or(DETACHED_HEAD)
}

fn head_off_branch(head: Option<&str>, branch: &str) -> Error {
    let head = head_shown(head);
    Error::new(
        ErrorCode::HeadOffBranch,
        format!("HEAD is on {head}, the store pushes {branch}"),
    )
    .with_detail("head", head)
    .with_detail("branch", branch)
}

fn check_head_branch(root: &Path, config: &StoreConfig) -> Result<()> {
    let head = head_branch(root)?;
    if head.as_deref() == Some(config.branch.as_str()) {
        return Ok(());
    }
    let error = head_off_branch(head.as_deref(), &config.branch);
    let mut state = read_state(root)?;
    state.last_push_error = Some(error.message.clone());
    write_state(root, &state)?;
    Err(error)
}

fn push_failed(message: &str) -> Error {
    Error::new(ErrorCode::Io, message).with_detail("reason", "push_failed")
}

enum Attempt {
    Pushed,
    Rejected(String),
    Missing,
    Failed(String),
}

fn attempt(root: &Path, config: &StoreConfig, timeout: Duration) -> Attempt {
    let refspec = format!("refs/heads/{0}:refs/heads/{0}", config.branch);
    match run_git(
        root,
        &["push", "--porcelain", "--", &config.remote, &refspec],
        timeout,
    ) {
        Ok(run) if run.success => Attempt::Pushed,
        Ok(run) if rejected_as_behind(&run.stdout) => Attempt::Rejected(truncated(&run.stderr)),
        Ok(run) => Attempt::Failed(truncated(&run.stderr)),
        Err(source) if source.kind() == ErrorKind::NotFound => Attempt::Missing,
        Err(source) => Attempt::Failed(truncated(&source.to_string())),
    }
}

fn push_with_retries(root: &Path, config: &StoreConfig) -> Result<()> {
    let mut state = read_state(root)?;
    let mut backoff = FIRST_BACKOFF;
    let mut failure = String::new();
    for round in 0..ATTEMPTS {
        match attempt(root, config, PUSH_TIMEOUT) {
            Attempt::Pushed => {
                state.mark_pushed();
                return write_state(root, &state);
            }
            Attempt::Rejected(text) => {
                state.remote_ahead = true;
                state.last_push_error = Some(text);
                write_state(root, &state)?;
                return Err(remote_ahead_error());
            }
            Attempt::Missing => {
                state.last_push_error = Some(ErrorCode::GitNotFound.as_str().to_owned());
                write_state(root, &state)?;
                return Err(git_not_found());
            }
            Attempt::Failed(text) => failure = text,
        }
        if round + 1 < ATTEMPTS {
            thread::sleep(backoff);
            backoff *= 2;
        }
    }
    state.last_push_error = Some(failure.clone());
    write_state(root, &state)?;
    Err(push_failed(&failure))
}

pub(super) fn try_lock_file(root: &Path, name: &str) -> Result<Option<File>> {
    let dir = root.join(STATE_DIR);
    ensure_dir(&dir, Durability::Full)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(name))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(source)) => Err(source.into()),
    }
}

pub(crate) fn current_head(root: &Path) -> Result<Option<String>> {
    if !has_repository(root) {
        return Ok(None);
    }
    Ok(head_id(&open(root)?)?.map(|id| id.to_string()))
}

fn push_until_stable(root: &Path, config: &StoreConfig) -> Result<Option<String>> {
    loop {
        let pushed = current_head(root)?;
        push_with_retries(root, config)?;
        if current_head(root)? == pushed {
            return Ok(pushed);
        }
    }
}

fn run(root: &Path, config: &StoreConfig) -> Result<PushOutcome> {
    if !config.git {
        return Err(git_disabled());
    }
    if !has_repository(root) {
        return Err(repository_missing());
    }
    if read_state(root)?.remote_ahead {
        return Err(remote_ahead_error());
    }
    check_head_branch(root, config)?;
    let mut pushed_any = false;
    loop {
        let Some(lock) = try_lock_file(root, PUSH_LOCK_FILE)? else {
            return Ok(if pushed_any {
                PushOutcome::Ok
            } else {
                PushOutcome::Deferred
            });
        };
        let pushed = push_until_stable(root, config)?;
        drop(lock);
        pushed_any = true;
        if current_head(root)? == pushed {
            return Ok(PushOutcome::Ok);
        }
    }
}

pub(crate) fn remote_url(root: &Path, remote: &str) -> Result<Option<String>> {
    let repo = open(root)?;
    Ok(repo
        .config_snapshot()
        .string(format!("remote.{remote}.url").as_str())
        .map(|value| value.to_string()))
}

pub(crate) fn display_url(url: &str) -> String {
    let Some(scheme_end) = ["http://", "https://"]
        .into_iter()
        .find(|scheme| url.starts_with(scheme))
        .map(str::len)
    else {
        return url.to_owned();
    };
    let rest = &url[scheme_end..];
    let authority = rest.find('/').map_or(rest, |slash| &rest[..slash]);
    match authority.rfind('@') {
        Some(at) => format!("{}***{}", &url[..scheme_end], &rest[at..]),
        None => url.to_owned(),
    }
}

pub(crate) fn without_credentials(text: &str, url: Option<&str>) -> String {
    match url {
        Some(url) if display_url(url) != url => text.replace(url, &display_url(url)),
        _ => text.to_owned(),
    }
}

pub(crate) fn unpushed(root: &Path, config: &StoreConfig) -> Result<u64> {
    let repo = open(root)?;
    let Some(head) = head_id(&repo)? else {
        return Ok(0);
    };
    let tracking = repo
        .try_find_reference(format!("refs/remotes/{}/{}", config.remote, config.branch).as_str())
        .map_err(git_error)?
        .and_then(|mut reference| reference.peel_to_id().ok())
        .map(gix::Id::detach);
    let mut walk = repo.rev_walk([head]);
    if let Some(tracking) = tracking {
        walk = walk.with_hidden([tracking]);
    }
    let mut count = 0_u64;
    for item in walk.all().map_err(git_error)? {
        item.map_err(git_error)?;
        count += 1;
    }
    Ok(count)
}

pub(crate) fn directory_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(std::result::Result::ok)
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_bytes(&entry.path()),
            Ok(_) => entry.metadata().map_or(0, |meta| meta.len()),
            Err(_) => 0,
        })
        .sum()
}

fn push_once(root: &Path, config: &StoreConfig) -> Result<PushOutcome> {
    let mut state = read_state(root)?;
    if state.waits() {
        return Ok(PushOutcome::Deferred);
    }
    match attempt(root, config, WRITE_PUSH_TIMEOUT) {
        Attempt::Pushed => {
            state.mark_pushed();
            write_state(root, &state)?;
            Ok(PushOutcome::Ok)
        }
        Attempt::Rejected(text) => {
            state.remote_ahead = true;
            state.last_push_error = Some(text);
            write_state(root, &state)?;
            Ok(PushOutcome::RemoteAhead)
        }
        Attempt::Missing => {
            state.last_push_error = Some(ErrorCode::GitNotFound.as_str().to_owned());
            write_state(root, &state)?;
            Ok(PushOutcome::GitNotFound)
        }
        Attempt::Failed(text) => {
            state.mark_failed(text);
            write_state(root, &state)?;
            Ok(PushOutcome::Failed)
        }
    }
}

pub(crate) fn push_after_write(root: &Path, config: &StoreConfig) -> Result<Option<PushOutcome>> {
    if !has_repository(root) || remote_url(root, &config.remote)?.is_none() {
        return Ok(None);
    }
    if read_state(root)?.remote_ahead {
        return Ok(Some(PushOutcome::RemoteAhead));
    }
    if check_head_branch(root, config).is_err() {
        return Ok(Some(PushOutcome::Failed));
    }
    let mut pushed_any = false;
    loop {
        let Some(lock) = try_lock_file(root, PUSH_LOCK_FILE)? else {
            return Ok(Some(if pushed_any {
                PushOutcome::Ok
            } else {
                PushOutcome::Deferred
            }));
        };
        let pushed = loop {
            let head = current_head(root)?;
            let outcome = push_once(root, config)?;
            if outcome != PushOutcome::Ok {
                return Ok(Some(outcome));
            }
            if current_head(root)? == head {
                break head;
            }
        };
        drop(lock);
        pushed_any = true;
        if current_head(root)? == pushed {
            return Ok(Some(PushOutcome::Ok));
        }
    }
}

impl Store {
    pub(crate) fn push_head(&self) -> Option<String> {
        let config = read_config(&self.root).ok()?;
        if !config.git || config.push != PushMode::OnCommit {
            return None;
        }
        current_head(&self.root).ok().flatten()
    }

    pub(crate) fn push_after(&self, before: Option<String>) -> Option<PushOutcome> {
        let config = read_config(&self.root).ok()?;
        if !config.git || config.push != PushMode::OnCommit {
            return None;
        }
        let after = current_head(&self.root).ok().flatten()?;
        if before.as_deref() == Some(after.as_str()) {
            return None;
        }
        push_after_write(&self.root, &config).unwrap_or(Some(PushOutcome::Failed))
    }

    pub(crate) fn push_guard(&self) -> PushGuard<'_> {
        PushGuard {
            store: self,
            before: self.push_head(),
        }
    }

    pub fn push(&self) -> Result<PushOutcome> {
        run(&self.root, &read_config(&self.root)?)
    }

    pub fn reset_remote_ahead(&self) -> Result<()> {
        let mut state = read_state(&self.root)?;
        state.remote_ahead = false;
        write_state(&self.root, &state)
    }

    pub fn set_remote(&self, url: &str) -> Result<()> {
        validate_remote_url(url)?;
        let config = read_config(&self.root)?;
        if !config.git {
            return Err(git_disabled());
        }
        if !has_repository(&self.root) {
            return Err(repository_missing());
        }
        let known = run_git(
            &self.root,
            &["remote", "get-url", "--", &config.remote],
            PUSH_TIMEOUT,
        )
        .map_err(|source| match source.kind() {
            ErrorKind::NotFound => git_not_found(),
            _ => push_failed(&source.to_string()),
        })?;
        let verb = if known.success { "set-url" } else { "add" };
        let outcome = run_git(
            &self.root,
            &["remote", verb, "--", &config.remote, url],
            PUSH_TIMEOUT,
        )
        .map_err(|source| push_failed(&source.to_string()))?;
        if outcome.success {
            Ok(())
        } else {
            Err(push_failed(&truncated(&outcome.stderr)))
        }
    }

    pub fn set_push(&self, mode: PushMode) -> Result<()> {
        self.set_setting(
            "push",
            match mode {
                PushMode::Off => "off",
                PushMode::OnCommit => "on-commit",
            },
        )
    }
}

pub(crate) struct PushGuard<'a> {
    store: &'a Store,
    before: Option<String>,
}

impl Drop for PushGuard<'_> {
    fn drop(&mut self) {
        let _ = self.store.push_after(self.before.take());
        self.store.collect_if_due();
    }
}

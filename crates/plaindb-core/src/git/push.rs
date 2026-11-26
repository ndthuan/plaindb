use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{git_disabled, git_error, head_id, open, read_optional, state_file};
use crate::atomic::{ensure_dir, replace_unsynced};
use crate::config::Durability;
use crate::config::{PushMode, StoreConfig, to_file_bytes};
use crate::error::{Error, ErrorCode, Result};
use crate::store::{STATE_DIR, Store, read_config};
use crate::write::now_timestamp;

const PUSH_LOCK_FILE: &str = "push.lock";
const PUSH_STATE_FILE: &str = "push.json";
const PUSH_TIMEOUT: Duration = Duration::from_secs(60);
const ATTEMPTS: u32 = 3;
const FIRST_BACKOFF: Duration = Duration::from_secs(2);
const ERROR_CHARS: usize = 500;
const REJECTION_MARKERS: [&str; 3] = ["[rejected]", "non-fast-forward", "fetch first"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushOutcome {
    Ok,
    Failed,
    RemoteAhead,
    Deferred,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PushState {
    pub last_push_at: Option<String>,
    pub last_push_error: Option<String>,
    pub remote_ahead: bool,
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

fn truncated(text: &str) -> String {
    text.trim().chars().take(ERROR_CHARS).collect()
}

fn remote_ahead_error() -> Error {
    Error::new(
        ErrorCode::RemoteAhead,
        "the remote holds commits this store lacks",
    )
}

fn git_not_found() -> Error {
    Error::new(ErrorCode::GitNotFound, "git was not found on PATH")
}

fn push_failed(message: &str) -> Error {
    Error::new(ErrorCode::Io, message).with_detail("reason", "push_failed")
}

struct GitRun {
    success: bool,
    stderr: String,
}

fn run_git(root: &Path, args: &[&str], timeout: Duration) -> std::io::Result<GitRun> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut pipe = child.stderr.take();
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = pipe.as_mut() {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    });
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            let stderr = reader.join().unwrap_or_default();
            return Ok(GitRun {
                success: status.success(),
                stderr,
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(GitRun {
                success: false,
                stderr: "the push timed out".to_owned(),
            });
        }
        thread::sleep(Duration::from_millis(20));
    }
}

enum Attempt {
    Pushed,
    Rejected(String),
    Missing,
    Failed(String),
}

fn attempt(root: &Path, config: &StoreConfig) -> Attempt {
    match run_git(
        root,
        &["push", &config.remote, &config.branch],
        PUSH_TIMEOUT,
    ) {
        Ok(run) if run.success => Attempt::Pushed,
        Ok(run) if REJECTION_MARKERS.iter().any(|m| run.stderr.contains(m)) => {
            Attempt::Rejected(truncated(&run.stderr))
        }
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
        match attempt(root, config) {
            Attempt::Pushed => {
                state.last_push_at = Some(now_timestamp());
                state.last_push_error = None;
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

fn try_lock(root: &Path) -> Result<Option<File>> {
    let dir = root.join(STATE_DIR);
    ensure_dir(&dir, Durability::Full)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(PUSH_LOCK_FILE))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(source)) => Err(source.into()),
    }
}

pub(crate) fn current_head(root: &Path) -> Result<Option<String>> {
    if !root.join(".git").exists() {
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
    if read_state(root)?.remote_ahead {
        return Err(remote_ahead_error());
    }
    let mut pushed_any = false;
    loop {
        let Some(lock) = try_lock(root)? else {
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
        Some(match run(&self.root, &config) {
            Ok(outcome) => outcome,
            Err(error) if error.code == ErrorCode::RemoteAhead => PushOutcome::RemoteAhead,
            Err(_) => PushOutcome::Failed,
        })
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
        let config = read_config(&self.root)?;
        if !config.git {
            return Err(git_disabled());
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
        let _push = self.push_guard();
        let _lock = self.begin_write()?;
        let mut config = read_config(&self.root)?;
        if config.push == mode {
            return Ok(());
        }
        config.push = mode;
        self.commit_admin(
            "meta store: set push",
            vec![crate::journal::JournalEntry::write(
                crate::store::STORE_FILE.to_owned(),
                &to_file_bytes(&config),
                None,
            )],
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
    }
}

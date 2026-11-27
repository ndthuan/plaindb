use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::json;

use super::command::{self, run_git};
use super::{GIT_DIR, GitStatus, gc, missing, read_warning, status_at};
use crate::config::StoreConfig;
use crate::doctor::{Check, DoctorOptions, Group};

const FIX_MISSING_BINARY: &str =
    "install git, or set the PLAINDB_GIT environment variable to the git program's path";
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const COMMIT_LOCKS: &[&str] = &["index.lock", "HEAD.lock", "packed-refs.lock", "config.lock"];
const WORKTREE_PATHS_SHOWN: usize = 10;
const STALE_LOCK: Duration = Duration::from_secs(600);
const LOOSE_OBJECTS_WITHOUT_HOUSEKEEPING: u64 = 6_700;

pub(crate) fn checks(root: &Path, config: &StoreConfig, options: DoctorOptions) -> Vec<Check> {
    if !config.git {
        return vec![Check::ok(
            Group::Git,
            "git",
            "git is not enabled for this store",
        )];
    }
    let status = match status_at(root) {
        Ok(status) => status,
        Err(error) => {
            return vec![
                Check::error(Group::Git, "git", error.message)
                    .with_details(error.details)
                    .with_fix("repair the repository in the store directory"),
            ];
        }
    };
    vec![
        binary_check(root, config, &status),
        remote_check(root, &status, options),
        head_branch_check(root, &status),
        unpushed_check(&status),
        remote_ahead_check(&status),
        last_push_check(&status),
        push_stuck_check(root),
        lock_files_check(root),
        missing_commits_check(root),
        worktree_check(root),
        foreign_check(root),
        housekeeping_check(root, config, &status),
    ]
}

fn binary_check(root: &Path, config: &StoreConfig, status: &GitStatus) -> Check {
    let details = json!({
        "binary": status.git_binary,
        "source": status.git_binary_source,
    });
    if !command::is_allowed(&command::resolve(root)) {
        return Check::error(
            Group::Git,
            "git_binary",
            "PLAINDB_GIT must be an absolute path or a program name",
        )
        .with_details(details)
        .with_fix(FIX_MISSING_BINARY);
    }
    if !status.git_binary_found {
        let message = format!(
            "the git binary {} chosen by {} was not found",
            status.git_binary, status.git_binary_source
        );
        let check = if status.push.is_on() {
            Check::error(Group::Git, "git_binary", message)
        } else {
            Check::warn(Group::Git, "git_binary", message)
        };
        return check.with_details(details).with_fix(FIX_MISSING_BINARY);
    }
    let version = run_git(root, &["--version"], PROBE_TIMEOUT)
        .ok()
        .filter(|run| run.success)
        .map(|run| run.stdout.trim().to_owned());
    let message = format!(
        "{} ({}) chosen by {}",
        version.as_deref().unwrap_or("git"),
        status.git_binary,
        status.git_binary_source
    );
    let details = json!({
        "binary": status.git_binary,
        "source": status.git_binary_source,
        "version": version,
    });
    if config.git_binary.is_some() {
        return Check::warn(Group::Git, "git_binary", message)
            .with_details(details)
            .with_fix(
                "remove git_binary from _store.json; set the PLAINDB_GIT environment variable to choose a git program",
            );
    }
    Check::ok(Group::Git, "git_binary", message).with_details(details)
}

fn remote_check(root: &Path, status: &GitStatus, options: DoctorOptions) -> Check {
    let Some(url) = &status.remote_url else {
        if status.push.is_on() {
            return Check::error(
                Group::Git,
                "git_remote",
                format!("push is on but the remote {} has no URL", status.remote),
            )
            .with_fix("plaindb git remote set <url>");
        }
        return Check::ok(Group::Git, "git_remote", "no remote is set")
            .with_details(json!({ "remote": status.remote, "url": null }));
    };
    let details = json!({ "remote": status.remote, "url": url, "checked": !options.offline });
    if options.offline {
        return Check::ok(
            Group::Git,
            "git_remote",
            "the remote was not contacted because of --offline",
        )
        .with_details(details);
    }
    if !status.git_binary_found {
        return Check::warn(
            Group::Git,
            "git_remote",
            "the remote cannot be checked without a git binary",
        )
        .with_details(details)
        .with_fix(FIX_MISSING_BINARY);
    }
    match run_git(
        root,
        &["ls-remote", "--heads", "--", &status.remote],
        PROBE_TIMEOUT,
    ) {
        Ok(run) if run.success => {
            Check::ok(Group::Git, "git_remote", "the remote is reachable").with_details(details)
        }
        Ok(run) => unreachable_remote(status, &redacted(root, status, run.stderr.trim()), details),
        Err(source) => unreachable_remote(status, &source.to_string(), details),
    }
}

pub(crate) fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn head_branch_check(root: &Path, status: &GitStatus) -> Check {
    let head = match super::head_branch(root) {
        Ok(head) => head,
        Err(error) => {
            return Check::error(Group::Git, "git_head_branch", error.message)
                .with_details(error.details)
                .with_fix("repair the repository in the store directory");
        }
    };
    let details = json!({
        "head": super::head_shown(head.as_deref()),
        "branch": status.branch,
    });
    if head.as_deref() == Some(status.branch.as_str()) {
        return Check::ok(
            Group::Git,
            "git_head_branch",
            format!("HEAD is on {}, the branch the store pushes", status.branch),
        )
        .with_details(details);
    }
    let switch = format!("git -C {} switch {}", shell_quote(root), status.branch);
    let fix = match &head {
        Some(other) => format!("{switch}, or plaindb config set branch {other}"),
        None => switch,
    };
    Check::error(
        Group::Git,
        "git_head_branch",
        format!(
            "HEAD is on {}, the store pushes {}",
            super::head_shown(head.as_deref()),
            status.branch
        ),
    )
    .with_details(details)
    .with_fix(fix)
}

fn redacted(root: &Path, status: &GitStatus, text: &str) -> String {
    let url = super::push::remote_url(root, &status.remote).ok().flatten();
    super::push::without_credentials(text, url.as_deref())
}

fn unreachable_remote(status: &GitStatus, reason: &str, details: serde_json::Value) -> Check {
    let message = format!("the remote cannot be reached: {reason}");
    let check = if status.push.is_on() {
        Check::error(Group::Git, "git_remote", message)
    } else {
        Check::warn(Group::Git, "git_remote", message)
    };
    check
        .with_details(details)
        .with_fix("check the network and the credentials, or plaindb git remote set <url>")
}

fn unpushed_check(status: &GitStatus) -> Check {
    if status.remote_url.is_none() || status.unpushed == 0 {
        return Check::ok(Group::Git, "git_unpushed", "no commit waits to be pushed")
            .with_details(json!({ "unpushed": status.unpushed }));
    }
    Check::warn(
        Group::Git,
        "git_unpushed",
        format!("{} commits are not pushed", status.unpushed),
    )
    .with_details(json!({ "unpushed": status.unpushed }))
    .with_fix("plaindb push")
}

fn remote_ahead_check(status: &GitStatus) -> Check {
    if !status.remote_ahead {
        return Check::ok(Group::Git, "git_remote_ahead", "the remote is not ahead")
            .with_details(json!({ "remote_ahead": false }));
    }
    Check::error(
        Group::Git,
        "git_remote_ahead",
        "the remote has commits this store lacks, so pushes are refused",
    )
    .with_details(json!({ "remote_ahead": true }))
    .with_fix(
        "merge the remote branch into the store's repository with git, then run plaindb push --reset-remote-ahead",
    )
}

fn last_push_check(status: &GitStatus) -> Check {
    match &status.last_push_error {
        Some(reason) => Check::warn(
            Group::Git,
            "git_last_push",
            format!("the last push failed: {reason}"),
        )
        .with_details(json!({ "error": reason, "at": status.last_push_at }))
        .with_fix("plaindb push"),
        None => Check::ok(Group::Git, "git_last_push", "the last push did not fail")
            .with_details(json!({ "error": null, "at": status.last_push_at })),
    }
}

fn push_stuck_check(root: &Path) -> Check {
    let Some(running) = super::push::running_push(root) else {
        return Check::ok(Group::Git, "git_push_stuck", "no push is running")
            .with_details(json!({ "running": false, "started_at": null }));
    };
    let details = json!({ "running": true, "started_at": running.started_at });
    let age = running
        .started_at
        .parse::<jiff::Timestamp>()
        .ok()
        .and_then(|at| jiff::Timestamp::now().duration_since(at).try_into().ok())
        .unwrap_or(Duration::ZERO);
    if age >= STALE_LOCK {
        return Check::warn(
            Group::Git,
            "git_push_stuck",
            format!("a push has been running for {} minutes", age.as_secs() / 60),
        )
        .with_details(details)
        .with_fix(format!(
            "stop the process that holds {}, then run plaindb push",
            shell_quote(&root.join(crate::store::STATE_DIR).join("push.lock"))
        ));
    }
    Check::ok(Group::Git, "git_push_stuck", "a push is running").with_details(details)
}

fn collect_locks(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            collect_locks(&path, found);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "lock")
        {
            found.push(path);
        }
    }
}

fn lock_files_check(root: &Path) -> Check {
    let git_dir = root.join(GIT_DIR);
    let mut paths: Vec<PathBuf> = COMMIT_LOCKS
        .iter()
        .map(|name| git_dir.join(name))
        .filter(|path| path.is_file())
        .collect();
    collect_locks(&git_dir.join("refs"), &mut paths);
    paths.sort();
    let now = SystemTime::now();
    let locks: Vec<(PathBuf, u64)> = paths
        .into_iter()
        .map(|path| {
            let age = fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .unwrap_or_default();
            (path, age.as_secs())
        })
        .collect();
    if locks.is_empty() {
        return Check::ok(
            Group::Git,
            "git_locks",
            "no lock file is left in the repository",
        )
        .with_details(json!({ "locks": [] }));
    }
    let listing: Vec<_> = locks
        .iter()
        .map(|(path, age)| json!({ "path": path.display().to_string(), "age_seconds": age }))
        .collect();
    let stale: Vec<&(PathBuf, u64)> = locks
        .iter()
        .filter(|(_, age)| *age >= STALE_LOCK.as_secs())
        .collect();
    if let Some((path, age)) = stale.first() {
        return Check::error(
            Group::Git,
            "git_locks",
            format!(
                "{} is {} minutes old and blocks every commit",
                path.display(),
                age / 60
            ),
        )
        .with_details(json!({ "locks": listing }))
        .with_fix(format!(
            "make sure no git process runs on the store, then rm {}",
            shell_quote(path)
        ));
    }
    Check::warn(
        Group::Git,
        "git_locks",
        "a git operation holds a lock file right now",
    )
    .with_details(json!({ "locks": listing }))
    .with_fix("wait for the operation to finish and run plaindb doctor again")
}

fn missing_commits_check(root: &Path) -> Check {
    match missing::read(root) {
        Ok(Some(found)) => Check::error(
            Group::Git,
            "git_pending_commits",
            format!(
                "{} committed writes have no git commit yet: {}",
                found.tx_ids.len(),
                found.message
            ),
        )
        .with_details(json!({ "transactions": found.tx_ids.len(), "message": found.message }))
        .with_fix(format!(
            "remove the cause (for a stale lock: rm {}), then run any write or plaindb commit",
            shell_quote(&root.join(GIT_DIR).join("index.lock"))
        )),
        _ => Check::ok(
            Group::Git,
            "git_pending_commits",
            "every committed write has its git commit",
        )
        .with_details(json!({ "transactions": 0 })),
    }
}

fn worktree_check(root: &Path) -> Check {
    let pending = match super::pending::read(root) {
        Ok(pending) => pending.len(),
        Err(error) => {
            return Check::error(Group::Git, "git_worktree", error.message)
                .with_details(error.details);
        }
    };
    if pending > 0 {
        return Check::ok(
            Group::Git,
            "git_worktree",
            format!("{pending} transactions wait for plaindb commit"),
        )
        .with_details(json!({ "count": 0, "paths": [], "pending_transactions": pending }));
    }
    let changes = match super::commit::worktree_changes(root) {
        Ok(changes) => changes,
        Err(error) => {
            return Check::error(Group::Git, "git_worktree", error.message)
                .with_details(error.details);
        }
    };
    let paths: Vec<&str> = changes
        .iter()
        .take(WORKTREE_PATHS_SHOWN)
        .map(super::commit::Change::path)
        .collect();
    let details = json!({ "count": changes.len(), "paths": paths, "pending_transactions": 0 });
    if changes.is_empty() {
        return Check::ok(
            Group::Git,
            "git_worktree",
            "every file of the databases is committed",
        )
        .with_details(details);
    }
    Check::warn(
        Group::Git,
        "git_worktree",
        format!("{} files differ from the last commit", changes.len()),
    )
    .with_details(details)
    .with_fix("plaindb commit")
}

fn foreign_check(root: &Path) -> Check {
    match read_warning(root) {
        Ok(Some(warning)) => Check::warn(
            Group::Git,
            "git_foreign_commits",
            format!("{} commits were made outside plaindb", warning.ids.len()),
        )
        .with_details(json!({ "ids": warning.ids, "at": warning.at, "head": warning.head }))
        .with_fix(
            "review the commits with git log; the warning clears after the next two commits made by plaindb",
        ),
        _ => Check::ok(
            Group::Git,
            "git_foreign_commits",
            "every commit was made by plaindb",
        )
        .with_details(json!({ "ids": [] })),
    }
}

fn loose_objects(root: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(root.join(GIT_DIR).join("objects")) else {
        return 0;
    };
    entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_name().len() == 2)
        .filter_map(|entry| fs::read_dir(entry.path()).ok())
        .map(|files| files.count() as u64)
        .sum()
}

fn housekeeping_check(root: &Path, config: &StoreConfig, status: &GitStatus) -> Check {
    let loose = loose_objects(root);
    let since = gc::commits_since_last_gc(root);
    let details = json!({
        "gc_every": config.gc_every,
        "loose_objects": loose,
        "commits_since_gc": since,
        "last_gc_at": status.last_gc_at,
        "last_gc_outcome": status.last_gc_outcome,
    });
    let failed = status.last_gc_outcome.as_deref() == Some("failed");
    let behind = config.gc_every > 0 && since > config.gc_every.saturating_mul(2);
    let unmanaged = config.gc_every == 0 && loose > LOOSE_OBJECTS_WITHOUT_HOUSEKEEPING;
    if failed {
        return Check::warn(
            Group::Git,
            "git_gc",
            format!(
                "the last housekeeping run failed: {}",
                status
                    .last_gc_error
                    .as_deref()
                    .unwrap_or("no reason recorded")
            ),
        )
        .with_details(details)
        .with_fix(format!("git -C {} gc", shell_quote(root)));
    }
    if behind || unmanaged {
        return Check::warn(
            Group::Git,
            "git_gc",
            format!("{loose} loose objects have piled up; housekeeping is not keeping up"),
        )
        .with_details(details)
        .with_fix(format!(
            "git -C {} gc, or plaindb config set gc_every 200",
            shell_quote(root)
        ));
    }
    Check::ok(
        Group::Git,
        "git_gc",
        format!("{loose} loose objects; housekeeping is keeping up"),
    )
    .with_details(details)
}

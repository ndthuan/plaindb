use std::fs::{OpenOptions, TryLockError};
use std::path::Path;

use serde_json::json;

use super::{Check, Group};
use crate::cache::{self, CacheRead};
use crate::config::{SETTING_KEYS, STORE_FORMAT, StoreConfig};
use crate::journal;
use crate::lock::LOCK_FILE;
use crate::scan;
use crate::snapshot::{Outcome, Snapshot};
use crate::store::STATE_DIR;

const LISTED: usize = 10;
const SCHEMA_REASON: &str = "schema";

pub(super) fn checks(root: &Path, config: &StoreConfig) -> Vec<Check> {
    let mut checks = vec![
        Check::ok(
            Group::Store,
            "store_format",
            format!("the store format is {}", config.format),
        )
        .with_details(json!({ "format": config.format, "supported": STORE_FORMAT })),
        unknown_settings_check(config),
        journal_check(root),
        lock_check(root),
    ];
    let timeout = std::time::Duration::from_millis(config.lock_timeout_ms);
    match scan::whole(root, timeout, || scan::full_scan(root)) {
        Ok(snapshot) => {
            checks.extend(document_checks(&snapshot));
            checks.push(cache_check(root, &snapshot));
        }
        Err(error) => checks.push(
            Check::error(Group::Store, "documents", error.message)
                .with_details(error.details)
                .with_fix("repair the file the message names, then run plaindb rescan"),
        ),
    }
    checks
}

fn unknown_settings_check(config: &StoreConfig) -> Check {
    if config.unknown.is_empty() {
        return Check::ok(
            Group::Store,
            "store_unknown_settings",
            "_store.json holds only settings plaindb knows",
        )
        .with_details(json!({ "keys": [] }));
    }
    let keys: Vec<&String> = config.unknown.keys().collect();
    let names: Vec<&str> = keys.iter().map(|key| key.as_str()).collect();
    Check::warn(
        Group::Store,
        "store_unknown_settings",
        format!(
            "_store.json holds settings plaindb ignores: {}",
            names.join(", ")
        ),
    )
    .with_details(json!({ "keys": keys }))
    .with_fix(format!(
        "check the spelling in _store.json; known settings: {}",
        SETTING_KEYS.join(", ")
    ))
}

fn journal_check(root: &Path) -> Check {
    let records = journal::record_count(root);
    if records == 0 {
        return Check::ok(Group::Store, "journal", "every transaction is checkpointed")
            .with_details(json!({ "records": 0 }));
    }
    Check::warn(
        Group::Store,
        "journal",
        format!("{records} journal records are not checkpointed yet"),
    )
    .with_details(json!({ "records": records }))
    .with_fix("plaindb rescan")
}

fn lock_check(root: &Path) -> Check {
    let held = OpenOptions::new()
        .read(true)
        .open(root.join(STATE_DIR).join(LOCK_FILE))
        .map(|file| match file.try_lock_shared() {
            Ok(()) => {
                let _ = file.unlock();
                false
            }
            Err(TryLockError::WouldBlock) => true,
            Err(TryLockError::Error(_)) => false,
        })
        .unwrap_or(false);
    if held {
        return Check::warn(
            Group::Store,
            "write_lock",
            "another process holds the write lock; the holder and its age are not recorded",
        )
        .with_details(json!({ "held": true, "holder": null }))
        .with_fix("wait for the write to finish, or stop the plaindb process that keeps the lock");
    }
    Check::ok(
        Group::Store,
        "write_lock",
        "no process holds the write lock",
    )
    .with_details(json!({ "held": false }))
}

fn listed(items: &[String]) -> Vec<&String> {
    items.iter().take(LISTED).collect()
}

fn document_checks(snapshot: &Snapshot) -> Vec<Check> {
    let mut invalid = Vec::new();
    let mut schema = Vec::new();
    let mut duplicates = Vec::new();
    for (db, db_snapshot) in snapshot.dbs.iter() {
        for (collection, collection_snapshot) in db_snapshot.collections.iter() {
            for (name, entry) in collection_snapshot.files.iter() {
                let label = format!("{db}/{collection}/{}", entry.id);
                if collection_snapshot.is_duplicate(name) {
                    duplicates.push(format!("{db}/{collection}/{name}"));
                } else if let Outcome::Invalid(found) = &entry.outcome {
                    if found.reason == SCHEMA_REASON {
                        schema.push(label);
                    } else {
                        invalid.push(label);
                    }
                }
            }
        }
    }
    vec![
        problem_check(
            "documents_invalid",
            &invalid,
            "documents cannot be read",
            "repair or remove the files, then run plaindb validate",
        ),
        problem_check(
            "documents_schema",
            &schema,
            "documents do not match their collection's schema",
            "change the documents or the schema, then run plaindb validate",
        ),
        problem_check(
            "names_case",
            &duplicates,
            "file names differ only by letter case",
            "rename or remove all but one file of each pair, then run plaindb rescan",
        ),
    ]
}

fn problem_check(id: &'static str, found: &[String], what: &str, fix: &str) -> Check {
    if found.is_empty() {
        return Check::ok(Group::Store, id, format!("no {what}"))
            .with_details(json!({ "count": 0, "first": [] }));
    }
    Check::error(Group::Store, id, format!("{} {what}", found.len()))
        .with_details(json!({ "count": found.len(), "first": listed(found) }))
        .with_fix(fix)
}

fn stamps(snapshot: &Snapshot) -> Vec<(String, u64, i128, u64)> {
    let mut stamps = Vec::new();
    for (db, db_snapshot) in snapshot.dbs.iter() {
        for (collection, collection_snapshot) in db_snapshot.collections.iter() {
            for (name, entry) in collection_snapshot.files.iter() {
                stamps.push((
                    format!("{db}/{collection}/{name}"),
                    entry.size,
                    entry.mtime_ns,
                    entry.inode,
                ));
            }
        }
    }
    stamps
}

fn cache_check(root: &Path, fresh: &Snapshot) -> Check {
    match cache::read(root) {
        CacheRead::Missing => Check::ok(
            Group::Store,
            "index_cache",
            "no index cache has been written yet",
        )
        .with_details(json!({ "present": false })),
        CacheRead::Unusable { .. } => Check::warn(
            Group::Store,
            "index_cache",
            "the index cache cannot be used and is rebuilt on the next open",
        )
        .with_details(json!({ "present": true, "usable": false }))
        .with_fix("plaindb reindex"),
        CacheRead::Usable(cached) if stamps(&cached) == stamps(fresh) => Check::ok(
            Group::Store,
            "index_cache",
            "the index cache matches the files",
        )
        .with_details(json!({ "present": true, "usable": true, "in_step": true })),
        CacheRead::Usable(_) => Check::warn(
            Group::Store,
            "index_cache",
            "the index cache is out of step with the files",
        )
        .with_details(json!({ "present": true, "usable": true, "in_step": false }))
        .with_fix("plaindb reindex"),
    }
}

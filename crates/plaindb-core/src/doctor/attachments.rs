use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::json;

use super::{Check, Group};
use crate::attachments::{FILES_DIR, FileMeta, files_of, sha256_hex};
use crate::snapshot::{CollectionSnapshot, Outcome, Snapshot};

const LISTED: usize = 10;

#[derive(Default)]
struct Found {
    missing: Vec<String>,
    mismatch: Vec<String>,
    orphan: Vec<String>,
}

fn visible_entries(dir: &Path) -> Vec<(String, bool)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<(String, bool)> = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let is_dir = entry.file_type().ok()?.is_dir();
            Some((name, is_dir))
        })
        .filter(|(name, _)| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

fn recorded(
    collection: &CollectionSnapshot,
) -> BTreeMap<String, Option<BTreeMap<String, Option<FileMeta>>>> {
    collection
        .files
        .iter()
        .filter_map(|(name, entry)| match &entry.outcome {
            Outcome::Doc(found) if !collection.is_duplicate(name) => {
                Some((entry.id.clone(), Some(files_of(&found.doc))))
            }
            Outcome::Invalid(_) => Some((entry.id.clone(), None)),
            _ => None,
        })
        .collect()
}

fn check_collection(dir: &Path, prefix: &str, collection: &CollectionSnapshot, found: &mut Found) {
    let documents = recorded(collection);
    for (id, files) in &documents {
        let Some(files) = files else {
            continue;
        };
        for (name, meta) in files {
            let label = format!("{prefix}/{id}/{name}");
            match fs::read(dir.join(FILES_DIR).join(id).join(name)) {
                Err(_) => found.missing.push(label),
                Ok(bytes) => {
                    let matches = meta.as_ref().is_some_and(|meta| {
                        u64::try_from(bytes.len()).is_ok_and(|size| size == meta.size)
                            && sha256_hex(&bytes) == meta.sha256
                    });
                    if !matches {
                        found.mismatch.push(label);
                    }
                }
            }
        }
    }
    for (id, is_dir) in visible_entries(&dir.join(FILES_DIR)) {
        if !is_dir {
            found.orphan.push(format!("{prefix}/{FILES_DIR}/{id}"));
            continue;
        }
        let known = documents.get(&id);
        if matches!(known, Some(None)) {
            continue;
        }
        for (name, _) in visible_entries(&dir.join(FILES_DIR).join(&id)) {
            let listed = known
                .and_then(Option::as_ref)
                .is_some_and(|files| files.contains_key(&name));
            if !listed {
                found.orphan.push(format!("{prefix}/{id}/{name}"));
            }
        }
    }
}

fn problem(id: &'static str, found: &[String], what: &str, fix: &str) -> Check {
    let first: Vec<&String> = found.iter().take(LISTED).collect();
    let details = json!({ "count": found.len(), "first": first });
    if found.is_empty() {
        return Check::ok(Group::Store, id, format!("no {what}")).with_details(details);
    }
    Check::warn(Group::Store, id, format!("{} {what}", found.len()))
        .with_details(details)
        .with_fix(fix)
}

pub(super) fn checks(root: &Path, snapshot: &Snapshot) -> Vec<Check> {
    let mut found = Found::default();
    for (db, db_snapshot) in snapshot.dbs.iter() {
        for (name, collection) in db_snapshot.collections.iter() {
            check_collection(
                &root.join(db).join(name),
                &format!("{db}/{name}"),
                collection,
                &mut found,
            );
        }
    }
    vec![
        problem(
            "attachment_missing",
            &found.missing,
            "attached files are missing from disk",
            "attach the file again with plaindb file put <db> <collection> <id> --name <name> --from <path>, or forget it with plaindb file rm <db> <collection> <id> --name <name>",
        ),
        problem(
            "attachment_mismatch",
            &found.mismatch,
            "attached files differ in size or sha256 from their document's _files",
            "put the right bytes back by hand and run plaindb file put <db> <collection> <id> --name <name> --from <path> with them, or remove the file by hand and run plaindb file rm <db> <collection> <id> --name <name>",
        ),
        problem(
            "attachment_orphan",
            &found.orphan,
            "files under _files belong to no document's _files",
            "remove the files listed in details by hand, or move them out of the store",
        ),
    ]
}

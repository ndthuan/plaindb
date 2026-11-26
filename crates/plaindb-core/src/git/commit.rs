use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use gix::ObjectId;
use gix::bstr::{BStr, ByteSlice};
use gix::index::entry::{Flags, Mode, Stat};
use gix::objs::tree::EntryKind;

use super::message::{TX_ID_TRAILER, trailer_values};
use super::{
    clear_warning, git_error, head_id, in_scope, open, read_warning, signature, worktree_files,
    write_last_commit,
};
use crate::error::Result;
use crate::journal::Journal;
use crate::version::version_of;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    Write { path: String, bytes: Vec<u8> },
    Remove { path: String },
    RemoveTree { path: String },
}

fn blob_id(bytes: &[u8]) -> Result<ObjectId> {
    ObjectId::from_hex(version_of(bytes).as_bytes()).map_err(git_error)
}

pub(crate) fn tx_ids_of(repo: &gix::Repository, id: ObjectId) -> Result<Vec<String>> {
    let commit = repo.find_commit(id).map_err(git_error)?;
    let message = commit.message_raw_sloppy().to_str_lossy().into_owned();
    Ok(trailer_values(&message, TX_ID_TRAILER))
}

fn write_commit(
    repo: &gix::Repository,
    head: Option<ObjectId>,
    changes: &[Change],
    message: &str,
) -> Result<ObjectId> {
    let base = match head {
        Some(id) => repo
            .find_commit(id)
            .map_err(git_error)?
            .tree_id()
            .map_err(git_error)?
            .detach(),
        None => ObjectId::empty_tree(repo.object_hash()),
    };
    let mut editor = repo.edit_tree(base).map_err(git_error)?;
    for change in changes {
        match change {
            Change::Write { path, bytes } => {
                let blob = repo.write_blob(bytes).map_err(git_error)?.detach();
                editor
                    .upsert(path.as_str(), EntryKind::Blob, blob)
                    .map_err(git_error)?;
            }
            Change::Remove { path } | Change::RemoveTree { path } => {
                editor.remove(path.as_str()).map_err(git_error)?;
            }
        }
    }
    let tree = editor.write().map_err(git_error)?.detach();
    let signature = signature(repo);
    let mut time = gix::date::parse::TimeBuf::default();
    let signer = signature.to_ref(&mut time);
    let id = repo
        .commit_as(signer, signer, "HEAD", message, tree, head)
        .map_err(git_error)?;
    Ok(id.detach())
}

fn open_index(repo: &gix::Repository) -> Result<gix::index::File> {
    if repo.index_path().exists() {
        repo.open_index().map_err(git_error)
    } else {
        Ok(gix::index::File::from_state(
            gix::index::State::new(repo.object_hash()),
            repo.index_path(),
        ))
    }
}

fn update_index(repo: &gix::Repository, root: &Path, changes: &[Change]) -> Result<()> {
    let mut index = open_index(repo)?;
    let mut replaced: BTreeSet<&str> = BTreeSet::new();
    let mut trees: Vec<String> = Vec::new();
    let mut written: BTreeMap<&str, (Stat, ObjectId)> = BTreeMap::new();
    for change in changes {
        match change {
            Change::Write { path, bytes } => {
                let metadata = gix::index::fs::Metadata::from_path_no_follow(&root.join(path))?;
                let stat = Stat::from_fs(&metadata).map_err(git_error)?;
                replaced.insert(path);
                written.insert(path, (stat, blob_id(bytes)?));
            }
            Change::Remove { path } => {
                replaced.insert(path);
                written.remove(path.as_str());
            }
            Change::RemoveTree { path } => {
                trees.push(format!("{path}/"));
                written.retain(|written_path, _| !written_path.starts_with(&format!("{path}/")));
            }
        }
    }
    index.remove_entries(|_, path, _| {
        let path = path.to_str_lossy();
        replaced.contains(path.as_ref()) || trees.iter().any(|tree| path.starts_with(tree.as_str()))
    });
    for (path, (stat, id)) in written {
        let path: &BStr = path.as_bytes().as_bstr();
        index.dangerously_push_entry(stat, id, Flags::empty(), Mode::FILE, path);
    }
    index.sort_entries();
    index
        .write(gix::index::write::Options::default(), Default::default())
        .map_err(git_error)?;
    Ok(())
}

fn record_commit(root: &Path, repo: &gix::Repository, id: ObjectId) -> Result<()> {
    write_last_commit(root, id)?;
    let Some(warning) = read_warning(root)? else {
        return Ok(());
    };
    let parent = repo
        .find_commit(id)
        .map_err(git_error)?
        .parent_ids()
        .next()
        .map(|parent| parent.detach().to_string());
    if parent.as_deref() != Some(warning.head.as_str()) {
        clear_warning(root)?;
    }
    Ok(())
}

pub(crate) fn commit_changes(
    root: &Path,
    changes: &[Change],
    message: &str,
    tx_id: &str,
) -> Result<ObjectId> {
    let repo = open(root)?;
    let head = head_id(&repo)?;
    let already = match head {
        Some(id) if tx_ids_of(&repo, id)?.iter().any(|known| known == tx_id) => Some(id),
        _ => None,
    };
    let id = match already {
        Some(id) => id,
        None => write_commit(&repo, head, changes, message)?,
    };
    crate::hooks::after_git_commit();
    update_index(&repo, root, changes)?;
    record_commit(root, &repo, id)?;
    Ok(id)
}

pub(crate) fn commit_journal(root: &Path, journal: &Journal) -> Result<()> {
    let Some(message) = &journal.commit_message else {
        return Ok(());
    };
    let mut changes = Vec::with_capacity(journal.entries.len());
    for entry in &journal.entries {
        let change = if entry.tree {
            Change::RemoveTree {
                path: entry.path.clone(),
            }
        } else {
            match &entry.bytes_b64 {
                Some(encoded) => Change::Write {
                    path: entry.path.clone(),
                    bytes: STANDARD.decode(encoded).map_err(git_error)?,
                },
                None => Change::Remove {
                    path: entry.path.clone(),
                },
            }
        };
        changes.push(change);
    }
    commit_changes(root, &changes, message, &journal.tx_id)?;
    Ok(())
}

fn tree_files(
    repo: &gix::Repository,
    tree: ObjectId,
    prefix: &str,
    files: &mut BTreeMap<String, ObjectId>,
) -> Result<()> {
    let tree = repo.find_tree(tree).map_err(git_error)?;
    for entry in tree.iter() {
        let entry = entry.map_err(git_error)?;
        let name = entry.filename().to_str_lossy();
        let path = if prefix.is_empty() {
            name.into_owned()
        } else {
            format!("{prefix}/{name}")
        };
        if entry.mode().is_tree() {
            tree_files(repo, entry.oid().to_owned(), &path, files)?;
        } else {
            files.insert(path, entry.oid().to_owned());
        }
    }
    Ok(())
}

pub(crate) fn worktree_changes(root: &Path) -> Result<Vec<Change>> {
    let repo = open(root)?;
    let mut committed = BTreeMap::new();
    if let Some(head) = head_id(&repo)? {
        let tree = repo
            .find_commit(head)
            .map_err(git_error)?
            .tree_id()
            .map_err(git_error)?
            .detach();
        tree_files(&repo, tree, "", &mut committed)?;
    }
    committed.retain(|path, _| in_scope(path));
    let mut changes = Vec::new();
    let present = worktree_files(root)?;
    for (path, absolute) in &present {
        let bytes = fs::read(absolute)?;
        if committed.get(path) != Some(&blob_id(&bytes)?) {
            changes.push(Change::Write {
                path: path.clone(),
                bytes,
            });
        }
    }
    for path in committed.keys() {
        if !present.contains_key(path) {
            changes.push(Change::Remove { path: path.clone() });
        }
    }
    Ok(changes)
}

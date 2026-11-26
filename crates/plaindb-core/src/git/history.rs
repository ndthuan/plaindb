use gix::ObjectId;
use gix::bstr::ByteSlice;

use super::message::{ACTOR_TRAILER, TX_ID_TRAILER, trailer_values};
use super::{git_disabled, git_error, head_id, open};
use crate::codec::{Doc, codec_for};
use crate::config::{CollectionMeta, from_file_bytes};
use crate::error::{Error, ErrorCode, Result};
use crate::names::{validate_collection_name, validate_db_name, validate_id};
use crate::store::{META_FILE, Store, Version, read_config};

pub const DEFAULT_HISTORY_LIMIT: u32 = 50;
const MIN_PREFIX_LEN: usize = 7;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum At {
    Commit(String),
    Time(jiff::Timestamp),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub commit: String,
    pub time: jiff::Timestamp,
    pub actor: Option<String>,
    pub tx_ids: Vec<String>,
    pub version: Option<Version>,
}

fn invalid_commit(spec: &str, message: &str) -> Error {
    Error::new(ErrorCode::InvalidQuery, message.to_owned()).with_detail("at", spec)
}

fn resolve_commit(repo: &gix::Repository, spec: &str) -> Result<ObjectId> {
    let hex_len = repo.object_hash().len_in_hex();
    let well_formed = (MIN_PREFIX_LEN..=hex_len).contains(&spec.len())
        && spec.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !well_formed {
        return Err(invalid_commit(
            spec,
            "a commit id needs at least 7 hexadecimal digits",
        ));
    }
    let id = if spec.len() == hex_len {
        let id = ObjectId::from_hex(spec.as_bytes()).map_err(git_error)?;
        repo.has_object(id)
            .then_some(id)
            .ok_or_else(|| Error::not_found("commit", spec))?
    } else {
        let prefix = gix::hash::Prefix::from_hex(spec).map_err(git_error)?;
        match repo
            .objects
            .lookup_prefix(prefix, None)
            .map_err(git_error)?
        {
            None => return Err(Error::not_found("commit", spec)),
            Some(Err(())) => {
                return Err(invalid_commit(spec, "the commit id is ambiguous")
                    .with_detail("reason", "ambiguous"));
            }
            Some(Ok(id)) => id,
        }
    };
    let kind = repo.find_object(id).map_err(git_error)?.kind;
    if kind == gix::objs::Kind::Commit {
        Ok(id)
    } else {
        Err(invalid_commit(spec, "the id does not name a commit")
            .with_detail("reason", "not_a_commit"))
    }
}

fn commit_seconds(commit: &gix::Commit<'_>) -> Result<i64> {
    Ok(commit.time().map_err(git_error)?.seconds)
}

fn first_parent_walk(
    repo: &gix::Repository,
    head: ObjectId,
) -> Result<impl Iterator<Item = Result<ObjectId>> + '_> {
    Ok(repo
        .rev_walk([head])
        .first_parent_only()
        .all()
        .map_err(git_error)?
        .map(|info| info.map(|info| info.id).map_err(git_error)))
}

fn commit_at_time(repo: &gix::Repository, at: jiff::Timestamp) -> Result<ObjectId> {
    let limit = at.as_second();
    let Some(head) = head_id(repo)? else {
        return Err(Error::not_found("commit", &at.to_string()));
    };
    for id in first_parent_walk(repo, head)? {
        let id = id?;
        let commit = repo.find_commit(id).map_err(git_error)?;
        if commit_seconds(&commit)? <= limit {
            return Ok(id);
        }
    }
    Err(Error::not_found("commit", &at.to_string()))
}

fn blob_at(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    path: &str,
) -> Result<Option<(ObjectId, Vec<u8>)>> {
    let Some(entry) = tree.lookup_entry_by_path(path).map_err(git_error)? else {
        return Ok(None);
    };
    if !entry.mode().is_blob() {
        return Ok(None);
    }
    let id = entry.object_id();
    let blob = repo.find_blob(id).map_err(git_error)?;
    Ok(Some((id, blob.data.clone())))
}

fn entry_id(tree: &gix::Tree<'_>, path: &str) -> Result<Option<ObjectId>> {
    Ok(tree
        .lookup_entry_by_path(path)
        .map_err(git_error)?
        .filter(|entry| entry.mode().is_blob())
        .map(|entry| entry.object_id()))
}

fn commit_tree(repo: &gix::Repository, id: ObjectId) -> Result<gix::Tree<'_>> {
    repo.find_commit(id)
        .map_err(git_error)?
        .tree()
        .map_err(git_error)
}

impl Store {
    fn git_repo(&self) -> Result<gix::Repository> {
        if !read_config(&self.root)?.git {
            return Err(git_disabled());
        }
        open(&self.root)
    }

    pub fn get_at(
        &self,
        db: &str,
        collection: &str,
        id: &str,
        at: At,
    ) -> Result<Option<(Doc, Version)>> {
        validate_db_name(db)?;
        validate_collection_name(collection)?;
        validate_id(id)?;
        let repo = self.git_repo()?;
        let commit = match &at {
            At::Commit(spec) => resolve_commit(&repo, spec)?,
            At::Time(time) => commit_at_time(&repo, *time)?,
        };
        let tree = commit_tree(&repo, commit)?;
        let Some((_, meta_bytes)) =
            blob_at(&repo, &tree, &format!("{db}/{collection}/{META_FILE}"))?
        else {
            return Err(Error::not_found("collection", collection));
        };
        let meta: CollectionMeta = from_file_bytes(&meta_bytes)?;
        let path = format!("{db}/{collection}/{id}.{}", meta.format.extension());
        let Some((blob, bytes)) = blob_at(&repo, &tree, &path)? else {
            return Ok(None);
        };
        let mut doc = codec_for(meta.format).decode(&bytes)?;
        if doc.get("_id").and_then(|value| value.as_str()) != Some(id) {
            doc.insert("_id".to_owned(), serde_json::Value::String(id.to_owned()));
        }
        Ok(Some((doc, blob.to_string())))
    }

    pub fn history(
        &self,
        db: &str,
        collection: &str,
        id: &str,
        limit: u32,
    ) -> Result<Vec<HistoryEntry>> {
        validate_id(id)?;
        let repo = self.git_repo()?;
        let meta = self.collection_meta(db, collection)?;
        let path = format!("{db}/{collection}/{id}.{}", meta.format.extension());
        let Some(head) = head_id(&repo)? else {
            return Ok(Vec::new());
        };
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let mut entries = Vec::new();
        for commit_id in first_parent_walk(&repo, head)? {
            if entries.len() >= limit {
                break;
            }
            let commit_id = commit_id?;
            let commit = repo.find_commit(commit_id).map_err(git_error)?;
            let current = entry_id(&commit.tree().map_err(git_error)?, &path)?;
            let previous = match commit.parent_ids().next() {
                Some(parent) => entry_id(&commit_tree(&repo, parent.detach())?, &path)?,
                None => None,
            };
            if current == previous {
                continue;
            }
            let message = commit.message_raw_sloppy().to_str_lossy().into_owned();
            let time = jiff::Timestamp::from_second(commit_seconds(&commit)?).map_err(git_error)?;
            entries.push(HistoryEntry {
                commit: commit_id.to_string(),
                time,
                actor: trailer_values(&message, ACTOR_TRAILER).into_iter().next(),
                tx_ids: trailer_values(&message, TX_ID_TRAILER),
                version: current.map(|blob| blob.to_string()),
            });
        }
        Ok(entries)
    }
}

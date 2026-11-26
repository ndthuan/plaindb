use std::path::Path;

use super::commit::tx_ids_of;
use super::{
    FOREIGN_COMMIT_WARNING, ForeignWarning, git_error, head_id, read_last_commit,
    write_last_commit, write_warning,
};
use crate::error::Result;
use crate::write::now_timestamp;

const WALK_LIMIT: usize = 1_000;

pub(crate) fn check_head(root: &Path, repo: &gix::Repository) -> Result<bool> {
    let Some(head) = head_id(repo)? else {
        return Ok(false);
    };
    let last = read_last_commit(root)?;
    if last == Some(head) {
        return Ok(false);
    }
    let mut foreign = Vec::new();
    let mut reached = false;
    let walk = repo
        .rev_walk([head])
        .first_parent_only()
        .all()
        .map_err(git_error)?;
    for info in walk.take(WALK_LIMIT) {
        let id = info.map_err(git_error)?.id;
        if Some(id) == last {
            reached = true;
            break;
        }
        if tx_ids_of(repo, id)?.is_empty() {
            foreign.push(id.to_string());
        }
    }
    let rewound = last.is_some() && !reached;
    if rewound && foreign.is_empty() {
        foreign.push(head.to_string());
    }
    write_last_commit(root, head)?;
    if foreign.is_empty() {
        return Ok(false);
    }
    write_warning(
        root,
        &ForeignWarning {
            code: FOREIGN_COMMIT_WARNING.to_owned(),
            ids: foreign,
            at: now_timestamp(),
            head: head.to_string(),
        },
    )?;
    Ok(true)
}

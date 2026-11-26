use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atomic::{flush, remove_durable};
use crate::config::Durability;
use crate::error::Result;
use crate::store::STATE_DIR;

pub const PENDING_FILE: &str = "pending.jsonl";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTx {
    pub tx_id: String,
    pub actor: String,
    pub ops: Vec<String>,
}

fn pending_path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(PENDING_FILE)
}

fn read_text(root: &Path) -> Result<String> {
    match fs::read(pending_path(root)) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(source.into()),
    }
}

fn parse(text: &str) -> Vec<PendingTx> {
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

pub(crate) fn read(root: &Path) -> Result<Vec<PendingTx>> {
    Ok(parse(&read_text(root)?))
}

pub(crate) fn append(root: &Path, entry: &PendingTx, durability: Durability) -> Result<()> {
    let text = read_text(root)?;
    if parse(&text)
        .iter()
        .any(|existing| existing.tx_id == entry.tx_id)
    {
        return Ok(());
    }
    let mut line = serde_json::to_string(entry).expect("a pending entry serialises");
    line.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(pending_path(root))?;
    if !text.is_empty() && !text.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    file.write_all(line.as_bytes())?;
    flush(&file, durability)?;
    Ok(())
}

pub(crate) fn clear(root: &Path) -> Result<()> {
    remove_durable(&pending_path(root))
}

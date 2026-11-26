use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atomic::write_atomic;
use crate::error::Result;
use crate::store::STATE_DIR;
use crate::write::TxResult;

pub const TX_LOG_FILE: &str = "tx-log.jsonl";
pub const TX_LOG_KEPT: usize = 1_000;
const TX_LOG_SLACK: usize = 100;

#[derive(Serialize, Deserialize)]
struct Record {
    tx_id: String,
    result: TxResult,
}

fn log_path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(TX_LOG_FILE)
}

fn read_log(root: &Path) -> Result<String> {
    match fs::read(log_path(root)) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(source.into()),
    }
}

fn parse(line: &str) -> Option<Record> {
    serde_json::from_str(line).ok()
}

pub(crate) fn find(root: &Path, tx_id: &str) -> Result<Option<TxResult>> {
    let text = read_log(root)?;
    Ok(text
        .lines()
        .filter(|line| line.contains(tx_id))
        .filter_map(parse)
        .find(|record| record.tx_id == tx_id)
        .map(|record| record.result))
}

pub(crate) fn record(root: &Path, result: &TxResult) -> Result<()> {
    let text = read_log(root)?;
    if text.lines().any(|line| {
        line.contains(&result.tx_id)
            && parse(line).is_some_and(|record| record.tx_id == result.tx_id)
    }) {
        return Ok(());
    }
    let mut line = serde_json::to_string(&Record {
        tx_id: result.tx_id.clone(),
        result: result.clone(),
    })
    .expect("a tx record serialises");
    line.push('\n');
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if kept.len() + 1 > TX_LOG_KEPT + TX_LOG_SLACK {
        let mut rewritten = String::new();
        for old in &kept[kept.len() + 1 - TX_LOG_KEPT..] {
            rewritten.push_str(old);
            rewritten.push('\n');
        }
        rewritten.push_str(&line);
        return write_atomic(&log_path(root), rewritten.as_bytes());
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(root))?;
    if !text.is_empty() && !text.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    file.write_all(line.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

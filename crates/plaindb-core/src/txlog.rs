use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::atomic::{flush, write_atomic_at};
use crate::config::Durability;
use crate::error::Result;
use crate::store::STATE_DIR;
use crate::write::TxResult;

pub const TX_LOG_FILE: &str = "tx-log.jsonl";
pub const TX_LOG_KEPT: usize = 1_000;
const TX_LOG_SLACK: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    tx_id: String,
    result: TxResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileStamp {
    size: u64,
    modified: Option<SystemTime>,
    inode: u64,
}

#[cfg(unix)]
fn inode(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn inode(_metadata: &fs::Metadata) -> u64 {
    0
}

pub(crate) fn file_stamp(path: &Path) -> Result<Option<FileStamp>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(FileStamp {
            size: metadata.len(),
            modified: metadata.modified().ok(),
            inode: inode(&metadata),
        })),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(source.into()),
    }
}

#[derive(Debug, Default)]
pub(crate) struct TxLog {
    loaded: Option<Option<FileStamp>>,
    records: VecDeque<Record>,
    ends_with_newline: bool,
}

fn log_path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(TX_LOG_FILE)
}

fn line_of(record: &Record) -> String {
    let mut line = serde_json::to_string(record).expect("a tx record serialises");
    line.push('\n');
    line
}

impl TxLog {
    fn sync(&mut self, root: &Path) -> Result<()> {
        let path = log_path(root);
        let stamp = file_stamp(&path)?;
        if self.loaded == Some(stamp) {
            return Ok(());
        }
        let text = match fs::read(&path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(source) if source.kind() == ErrorKind::NotFound => String::new(),
            Err(source) => return Err(source.into()),
        };
        self.records = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        self.ends_with_newline = text.is_empty() || text.ends_with('\n');
        self.loaded = Some(stamp);
        Ok(())
    }

    pub(crate) fn find(&mut self, root: &Path, tx_id: &str) -> Result<Option<TxResult>> {
        self.sync(root)?;
        Ok(self
            .records
            .iter()
            .rev()
            .find(|record| record.tx_id == tx_id)
            .map(|record| record.result.clone()))
    }

    pub(crate) fn record_all(
        &mut self,
        root: &Path,
        results: &[&TxResult],
        durability: Durability,
    ) -> Result<()> {
        self.sync(root)?;
        let mut appended = String::new();
        for result in results {
            if self
                .records
                .iter()
                .any(|record| record.tx_id == result.tx_id)
            {
                continue;
            }
            let record = Record {
                tx_id: result.tx_id.clone(),
                result: (*result).clone(),
            };
            appended.push_str(&line_of(&record));
            self.records.push_back(record);
        }
        if appended.is_empty() {
            return Ok(());
        }
        let path = log_path(root);
        self.loaded = None;
        if self.records.len() > TX_LOG_KEPT + TX_LOG_SLACK {
            let excess = self.records.len() - TX_LOG_KEPT;
            self.records.drain(..excess);
            let rewritten: String = self.records.iter().map(line_of).collect();
            write_atomic_at(&path, rewritten.as_bytes(), durability)?;
        } else {
            let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
            if !self.ends_with_newline {
                file.write_all(b"\n")?;
            }
            file.write_all(appended.as_bytes())?;
            flush(&file, durability)?;
        }
        self.ends_with_newline = true;
        self.loaded = Some(file_stamp(&path)?);
        Ok(())
    }
}

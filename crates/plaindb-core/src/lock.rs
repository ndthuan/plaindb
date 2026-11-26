use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::atomic::ensure_dir;
use crate::config::DEFAULT_LOCK_TIMEOUT_MS;
use crate::error::{Error, ErrorCode, Result};
use crate::store::STATE_DIR;

pub const LOCK_FILE: &str = "lock";
pub const QUEUE_FILE: &str = "lock-queue";
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_millis(DEFAULT_LOCK_TIMEOUT_MS);

#[derive(Debug)]
pub struct WriteLock {
    file: File,
}

fn would_block<T>(outcome: std::result::Result<T, TryLockError>) -> Result<bool> {
    match outcome {
        Ok(_) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(source)) => Err(source.into()),
    }
}

impl WriteLock {
    fn open(store: &Path, name: &str) -> Result<File> {
        let dir = store.join(STATE_DIR);
        ensure_dir(&dir)?;
        Ok(OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(name))?)
    }

    fn others_are_waiting(queue: &File) -> Result<bool> {
        if would_block(queue.try_lock())? {
            return Ok(true);
        }
        queue.unlock()?;
        Ok(false)
    }

    pub fn try_acquire(store: impl AsRef<Path>) -> Result<Option<Self>> {
        let file = Self::open(store.as_ref(), LOCK_FILE)?;
        if would_block(file.try_lock())? {
            Ok(None)
        } else {
            Ok(Some(Self { file }))
        }
    }

    pub fn acquire(store: impl AsRef<Path>, timeout: Duration) -> Result<Self> {
        let store = store.as_ref();
        let file = Self::open(store, LOCK_FILE)?;
        let queue = Self::open(store, QUEUE_FILE)?;
        if !Self::others_are_waiting(&queue)? && !would_block(file.try_lock())? {
            return Ok(Self { file });
        }
        would_block(queue.try_lock_shared())?;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let outcome = file.lock().map(|()| file);
            let _ = sender.send(outcome);
        });
        match receiver.recv_timeout(timeout) {
            Ok(Ok(file)) => Ok(Self { file }),
            Ok(Err(source)) => Err(source.into()),
            Err(_) => Err(Error::new(
                ErrorCode::StoreLockedTimeout,
                "another process holds the store's write lock",
            )
            .with_detail(
                "timeout_ms",
                u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            )),
        }
    }
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

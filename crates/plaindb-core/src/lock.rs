use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::atomic::ensure_dir;
use crate::config::DEFAULT_LOCK_TIMEOUT_MS;
use crate::config::Durability;
use crate::error::{Error, ErrorCode, Result};
use crate::store::STATE_DIR;

pub const LOCK_FILE: &str = "lock";
pub const QUEUE_FILE: &str = "lock-queue";
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_millis(DEFAULT_LOCK_TIMEOUT_MS);
const RETRY_DELAY: Duration = Duration::from_millis(5);

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
        ensure_dir(&dir, Durability::Full)?;
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
        let others_wait = Self::others_are_waiting(&queue)?;
        if !others_wait && !would_block(file.try_lock())? {
            return Ok(Self { file });
        }
        would_block(queue.try_lock_shared())?;
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(locked_timeout(timeout));
            }
            thread::sleep(RETRY_DELAY.min(deadline - now));
            if !would_block(file.try_lock())? {
                return Ok(Self { file });
            }
        }
    }
}

fn locked_timeout(timeout: Duration) -> Error {
    Error::new(
        ErrorCode::StoreLockedTimeout,
        "another process holds the store's write lock",
    )
    .with_detail(
        "timeout_ms",
        u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
    )
}

#[derive(Debug)]
pub struct ReadLock {
    file: File,
}

impl ReadLock {
    pub fn acquire(store: impl AsRef<Path>, timeout: Duration) -> Result<Option<Self>> {
        let path = store.as_ref().join(STATE_DIR).join(LOCK_FILE);
        let Ok(file) = OpenOptions::new().read(true).open(path) else {
            return Ok(None);
        };
        let deadline = Instant::now() + timeout;
        loop {
            if !would_block(file.try_lock_shared())? {
                return Ok(Some(Self { file }));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(locked_timeout(timeout));
            }
            thread::sleep(RETRY_DELAY.min(deadline - now));
        }
    }
}

impl Drop for ReadLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

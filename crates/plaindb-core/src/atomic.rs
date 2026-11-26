use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, ErrorCode, Result};

pub(crate) const TEMP_MARKER: &str = ".tmp-";

pub(crate) fn parent_of(path: &Path) -> Result<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            Error::new(ErrorCode::Io, "the path has no parent directory")
                .with_detail("path", path.display().to_string())
        })
}

pub(crate) fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)?.sync_all()?;
    Ok(())
}

pub(crate) fn ensure_dir(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let parent = parent_of(dir)?;
    ensure_dir(parent)?;
    match fs::create_dir(dir) {
        Ok(()) => sync_dir(parent),
        Err(source) if source.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(source.into()),
    }
}

fn temp_path(path: &Path) -> Result<PathBuf> {
    let dir = parent_of(path)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            Error::new(ErrorCode::Io, "the path has no file name")
                .with_detail("path", path.display().to_string())
        })?;
    Ok(dir.join(format!(".{name}{TEMP_MARKER}{}", ulid::Ulid::generate())))
}

fn replace_with(path: &Path, bytes: &[u8], durable: bool) -> Result<()> {
    ensure_dir(parent_of(path)?)?;
    let temp = temp_path(path)?;
    let written = (|| -> Result<()> {
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        if durable {
            file.sync_all()?;
        }
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

pub(crate) fn replace_unsynced(path: &Path, bytes: &[u8]) -> Result<()> {
    replace_with(path, bytes, false)
}

pub(crate) fn place(path: &Path, bytes: &[u8]) -> Result<()> {
    replace_with(path, bytes, true)
}

pub(crate) fn unlink(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(false),
        Err(source) => Err(source.into()),
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    place(path, bytes)?;
    sync_dir(parent_of(path)?)
}

pub fn remove_durable(path: &Path) -> Result<()> {
    if unlink(path)? {
        sync_dir(parent_of(path)?)?;
    }
    Ok(())
}

pub(crate) fn remove_tree_durable(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => sync_dir(parent_of(path)?),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(()),
        Err(source) => Err(source.into()),
    }
}

pub(crate) fn remove_temp_files(dir: &Path) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(source.into()),
    };
    for entry in entries {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') && name.contains(TEMP_MARKER) && entry.file_type()?.is_file() {
            remove_durable(&entry.path())?;
        }
    }
    Ok(())
}

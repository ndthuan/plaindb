use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::config::Durability;
use crate::error::{Error, ErrorCode, Result};
use crate::hooks;

pub(crate) const TEMP_MARKER: &str = ".tmp-";

#[cfg(target_vendor = "apple")]
mod platform {
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    fn fcntl_or_fsync(file: &File, command: libc::c_int) -> io::Result<()> {
        let fd = file.as_raw_fd();
        if unsafe { libc::fcntl(fd, command) } != -1 {
            return Ok(());
        }
        fsync(file)
    }

    pub(super) fn full(file: &File) -> io::Result<()> {
        fcntl_or_fsync(file, libc::F_FULLFSYNC)
    }

    pub(super) fn fsync(file: &File) -> io::Result<()> {
        if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(not(target_vendor = "apple"))]
mod platform {
    use std::fs::File;
    use std::io;

    pub(super) fn full(file: &File) -> io::Result<()> {
        file.sync_data()
    }

    pub(super) fn fsync(file: &File) -> io::Result<()> {
        file.sync_all()
    }
}

pub(crate) fn flush(file: &File, durability: Durability) -> Result<()> {
    match durability {
        Durability::Off => {}
        Durability::Full => {
            hooks::full_flush();
            platform::full(file)?;
        }
        Durability::Normal => platform::fsync(file)?,
    }
    Ok(())
}

pub(crate) fn parent_of(path: &Path) -> Result<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            Error::new(ErrorCode::Io, "the path has no parent directory")
                .with_detail("path", path.display().to_string())
        })
}

pub(crate) fn sync_dir(dir: &Path, durability: Durability) -> Result<()> {
    if durability == Durability::Off {
        return Ok(());
    }
    flush(&File::open(dir)?, durability)
}

pub(crate) fn ensure_dir(dir: &Path, durability: Durability) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let parent = parent_of(dir)?;
    ensure_dir(parent, durability)?;
    match fs::create_dir(dir) {
        Ok(()) => sync_dir(parent, durability),
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

fn replace_with(path: &Path, bytes: &[u8], durability: Durability) -> Result<()> {
    ensure_dir(parent_of(path)?, durability)?;
    let temp = temp_path(path)?;
    let written = (|| -> Result<()> {
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        flush(&file, durability)?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

pub(crate) fn replace_unsynced(path: &Path, bytes: &[u8]) -> Result<()> {
    replace_with(path, bytes, Durability::Off)
}

pub(crate) fn place(path: &Path, bytes: &[u8], durability: Durability) -> Result<()> {
    replace_with(path, bytes, durability)
}

pub(crate) fn unlink(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(false),
        Err(source) => Err(source.into()),
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    write_atomic_at(path, bytes, Durability::Full)
}

pub(crate) fn write_atomic_at(path: &Path, bytes: &[u8], durability: Durability) -> Result<()> {
    place(path, bytes, durability)?;
    sync_dir(parent_of(path)?, durability)
}

pub fn remove_durable(path: &Path) -> Result<()> {
    remove_at(path, Durability::Full)
}

pub(crate) fn remove_at(path: &Path, durability: Durability) -> Result<()> {
    if unlink(path)? {
        sync_dir(parent_of(path)?, durability)?;
    }
    Ok(())
}

pub(crate) fn remove_tree_at(path: &Path, durability: Durability) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => sync_dir(parent_of(path)?, durability),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(()),
        Err(source) => Err(source.into()),
    }
}

pub(crate) fn remove_temp_files(dir: &Path, durability: Durability) -> Result<()> {
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
            remove_at(&entry.path(), durability)?;
        }
    }
    Ok(())
}

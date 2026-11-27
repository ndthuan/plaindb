use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::Durability;
use crate::error::{Error, ErrorCode, Result};
use crate::hooks;

pub(crate) const TEMP_MARKER: &str = ".tmp-";
const STRAY_TEMP_AGE: Duration = Duration::from_secs(60);

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

    pub(super) fn barrier(file: &File) -> io::Result<()> {
        fcntl_or_fsync(file, libc::F_BARRIERFSYNC)
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
        file.sync_all()
    }

    pub(super) fn barrier(file: &File) -> io::Result<()> {
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

fn order(file: &File, durability: Durability) -> Result<()> {
    if durability != Durability::Off {
        platform::barrier(file)?;
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

#[cfg(target_vendor = "apple")]
pub(crate) fn sync_dir(dir: &Path, durability: Durability) -> Result<()> {
    if durability == Durability::Off {
        return Ok(());
    }
    flush(&File::open(dir)?, durability)
}

#[cfg(not(target_vendor = "apple"))]
pub(crate) fn sync_dir(dir: &Path, durability: Durability) -> Result<()> {
    if durability == Durability::Off {
        return Ok(());
    }
    let dir = File::open(dir)?;
    if durability == Durability::Full {
        hooks::full_flush();
    }
    hooks::dir_fsync();
    dir.sync_all()?;
    Ok(())
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

#[cfg(unix)]
fn keep_mode(file: &File, path: &Path) -> Result<()> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(file.set_permissions(metadata.permissions())?),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(()),
        Err(source) => Err(source.into()),
    }
}

#[cfg(not(unix))]
fn keep_mode(_file: &File, _path: &Path) -> Result<()> {
    Ok(())
}

fn replace_with(path: &Path, bytes: &[u8], durability: Durability, ordered: bool) -> Result<()> {
    replace_by(path, durability, ordered, |file| Ok(file.write_all(bytes)?))
}

fn replace_by(
    path: &Path,
    durability: Durability,
    ordered: bool,
    fill: impl FnOnce(&mut File) -> Result<()>,
) -> Result<()> {
    let dir_durability = if ordered { Durability::Off } else { durability };
    ensure_dir(parent_of(path)?, dir_durability)?;
    let temp = temp_path(path)?;
    let written = (|| -> Result<()> {
        let mut file = File::create(&temp)?;
        fill(&mut file)?;
        keep_mode(&file, path)?;
        if ordered {
            order(&file, durability)?;
        } else {
            flush(&file, durability)?;
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
    replace_with(path, bytes, Durability::Off, false)
}

pub(crate) fn replace_unsynced_by(
    path: &Path,
    fill: impl FnOnce(&mut File) -> Result<()>,
) -> Result<()> {
    replace_by(path, Durability::Off, false, fill)
}

pub(crate) fn place(path: &Path, bytes: &[u8], durability: Durability) -> Result<()> {
    replace_with(path, bytes, durability, false)
}

pub(crate) fn place_ordered(path: &Path, bytes: &[u8], durability: Durability) -> Result<()> {
    replace_with(path, bytes, durability, true)
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
    if unlink(path)? {
        sync_dir(parent_of(path)?, Durability::Full)?;
    }
    Ok(())
}

pub(crate) fn remove_tree(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Err(source) if source.kind() != ErrorKind::NotFound => Err(source.into()),
        _ => Ok(()),
    }
}

fn is_stray(entry: &fs::DirEntry) -> bool {
    entry
        .metadata()
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| {
            modified
                .elapsed()
                .is_ok_and(|elapsed| elapsed >= STRAY_TEMP_AGE)
        })
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
        if name.starts_with('.')
            && name.contains(TEMP_MARKER)
            && entry.file_type()?.is_file()
            && is_stray(&entry)
        {
            unlink(&entry.path())?;
        }
    }
    Ok(())
}

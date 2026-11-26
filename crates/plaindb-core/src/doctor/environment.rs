use std::fs;
use std::path::Path;

use serde_json::json;

use super::{Check, Group};
use crate::config::{Durability, StoreConfig};
use crate::store::STORE_FILE;

const GIB: u64 = 1 << 30;
const MIN_FREE_BYTES: u64 = GIB;
const MIN_FREE_PERCENT: u64 = 10;

#[cfg(any(target_os = "linux", test))]
const NETWORK_TYPES: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "afs",
    "ceph",
    "9p",
    "ncpfs",
    "fuse.sshfs",
    "davfs",
    "fuse.rclone",
];

pub(super) fn checks(root: &Path, config: &StoreConfig) -> Vec<Check> {
    vec![
        network_check(root),
        case_check(root),
        durability_check(config.durability),
        space_check(root),
    ]
}

#[cfg(any(target_os = "linux", test))]
fn network_type_in(mounts: &str, path: &Path) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let (Some(_), Some(point), Some(kind)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let point = point.replace("\\040", " ");
        if path.starts_with(&point) && best.is_none_or(|(length, _)| point.len() >= length) {
            best = Some((point.len(), kind));
        }
    }
    best.filter(|(_, kind)| NETWORK_TYPES.contains(kind))
        .map(|(_, kind)| kind.to_owned())
}

#[cfg(target_os = "linux")]
fn network_filesystem(root: &Path) -> Option<String> {
    let path = fs::canonicalize(root).ok()?;
    let mounts = fs::read_to_string("/proc/self/mounts").ok()?;
    network_type_in(&mounts, &path)
}

#[cfg(target_vendor = "apple")]
fn network_filesystem(root: &Path) -> Option<String> {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(root.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &raw mut stats) } != 0 {
        return None;
    }
    let kind = unsafe { CStr::from_ptr(stats.f_fstypename.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    ["nfs", "smbfs", "afpfs", "webdav", "cifs"]
        .contains(&kind.as_str())
        .then_some(kind)
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
fn network_filesystem(_root: &Path) -> Option<String> {
    None
}

fn network_check(root: &Path) -> Check {
    match network_filesystem(root) {
        Some(kind) => Check::warn(
            Group::Environment,
            "filesystem_network",
            format!("the store is on a network filesystem ({kind}); file locks may not work"),
        )
        .with_details(json!({ "network": true, "type": kind }))
        .with_fix("move the store to a local disk, or run only one process that writes to it"),
        None => Check::ok(
            Group::Environment,
            "filesystem_network",
            "the store is on a local filesystem",
        )
        .with_details(json!({ "network": false })),
    }
}

fn is_case_insensitive(root: &Path) -> bool {
    let upper = root.join(STORE_FILE.to_uppercase());
    upper != root.join(STORE_FILE) && fs::metadata(upper).is_ok()
}

fn case_check(root: &Path) -> Check {
    if is_case_insensitive(root) {
        Check::ok(
            Group::Environment,
            "filesystem_case",
            "the filesystem ignores letter case; ids that differ only by case collide",
        )
        .with_details(json!({ "case_sensitive": false }))
    } else {
        Check::ok(
            Group::Environment,
            "filesystem_case",
            "the filesystem is case sensitive",
        )
        .with_details(json!({ "case_sensitive": true }))
    }
}

fn durability_check(durability: Durability) -> Check {
    let details = json!({ "durability": durability.as_str() });
    match durability {
        Durability::Full => Check::ok(
            Group::Environment,
            "durability",
            "durability is full: a power loss loses no committed write",
        ),
        Durability::Normal => Check::ok(
            Group::Environment,
            "durability",
            "durability is normal: a power loss may lose the last writes on macOS, never leaves the store half written",
        ),
        Durability::Off => Check::warn(
            Group::Environment,
            "durability",
            "durability is off: a crash of the machine can lose recent writes and leave files half written",
        )
        .with_fix("plaindb config set durability full"),
    }
    .with_details(details)
}

#[cfg(unix)]
fn disk_space(root: &Path) -> Option<(u64, u64)> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(root.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &raw mut stats) } != 0 {
        return None;
    }
    let unit = u128::from(stats.f_frsize);
    let free = u64::try_from(u128::from(stats.f_bavail) * unit).ok()?;
    let total = u64::try_from(u128::from(stats.f_blocks) * unit).ok()?;
    Some((free, total))
}

#[cfg(not(unix))]
fn disk_space(_root: &Path) -> Option<(u64, u64)> {
    None
}

fn space_check(root: &Path) -> Check {
    let Some((free, total)) = disk_space(root) else {
        return Check::ok(
            Group::Environment,
            "disk_space",
            "the free disk space could not be read",
        )
        .with_details(json!({ "free_bytes": null, "total_bytes": null }));
    };
    let details = json!({ "free_bytes": free, "total_bytes": total });
    let low =
        free < MIN_FREE_BYTES || free.saturating_mul(100) < total.saturating_mul(MIN_FREE_PERCENT);
    if low {
        Check::warn(
            Group::Environment,
            "disk_space",
            format!("only {} MiB are free on the store's disk", free >> 20),
        )
        .with_details(details)
        .with_fix("free some disk space; writes fail when the disk is full")
    } else {
        Check::ok(
            Group::Environment,
            "disk_space",
            format!("{} MiB are free on the store's disk", free >> 20),
        )
        .with_details(details)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::network_type_in;

    const MOUNTS: &str = "/dev/sda1 / ext4 rw 0 0\nserver:/export /mnt/shared nfs4 rw 0 0\n//host/share /mnt/shared/local cifs rw 0 0\n/dev/sdb1 /mnt/shared/disk ext4 rw 0 0\n";

    #[test]
    fn the_longest_mount_point_decides_the_type() {
        assert_eq!(network_type_in(MOUNTS, Path::new("/home/me/store")), None);
        assert_eq!(
            network_type_in(MOUNTS, Path::new("/mnt/shared/data")).as_deref(),
            Some("nfs4")
        );
        assert_eq!(
            network_type_in(MOUNTS, Path::new("/mnt/shared/local/data")).as_deref(),
            Some("cifs")
        );
        assert_eq!(
            network_type_in(MOUNTS, Path::new("/mnt/shared/disk/x")),
            None
        );
    }
}

use std::sync::OnceLock;

#[cfg(target_vendor = "apple")]
fn read_boot_id() -> Option<String> {
    let mut buffer = [0_u8; 64];
    let mut size = buffer.len();
    let status = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&buffer[..size.min(buffer.len())]);
    let id = text.trim_end_matches('\0').trim();
    (!id.is_empty()).then(|| id.to_owned())
}

#[cfg(target_os = "linux")]
fn read_boot_id() -> Option<String> {
    let text = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let id = text.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
fn read_boot_id() -> Option<String> {
    None
}

pub(crate) fn boot_id() -> Option<String> {
    if let Some(given) = crate::hooks::boot_id() {
        return Some(given);
    }
    static BOOT_ID: OnceLock<Option<String>> = OnceLock::new();
    BOOT_ID.get_or_init(read_boot_id).clone()
}

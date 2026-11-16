//! Leave trading disk headroom before creating another archive/compaction copy.
use super::*;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

pub const DEFAULT_MIN_FREE_BYTES: u64 = 10 * 1024 * 1024 * 1024;

pub fn available(path: &Path) -> Result<u64> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // statvfs initializes this struct only on success; f_bavail accounts for
    // blocks unavailable to this unprivileged Manager user.
    let result = unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) };
    ensure!(
        result == 0,
        "disk availability unavailable: {}",
        std::io::Error::last_os_error()
    );
    let stat = unsafe { stat.assume_init() };
    u64::try_from((stat.f_bavail as u128) * (stat.f_frsize as u128))
        .context("disk availability overflow")
}

pub(super) fn require(path: &Path, minimum: u64, temporary: u64) -> Result<u64> {
    let free = available(path)?;
    let needed = minimum
        .checked_add(temporary)
        .context("disk reserve overflow")?;
    ensure!(
        free >= needed,
        "disk headroom: {free} available bytes, {needed} required; defer new archive/compaction copies"
    );
    Ok(free)
}

//! File system queries.

use std::ffi::CString;
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The size and free space of one mounted file system, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsSpace {
    /// `f_frsize × f_blocks`.
    pub total: u64,
    /// `f_frsize × f_bavail`: the space an unprivileged process can still use.
    pub available: u64,
}

/// `statvfs64(path)`, as ZenUI reads `/media/p4` (`0x210f6c`).
pub fn statvfs(path: &Path) -> io::Result<FsSpace> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut stats = MaybeUninit::<libc::statvfs64>::uninit();
    // SAFETY: `path` is a live NUL-terminated string and `stats` is writable storage of the
    // type statvfs64 fills.
    if unsafe { libc::statvfs64(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs64 returned 0, so it filled `stats`.
    let stats = unsafe { stats.assume_init() };
    #[allow(clippy::useless_conversion)] // `c_ulong` is u32 on ARM, u64 on x86-64.
    let fragment = u64::from(stats.f_frsize);
    Ok(FsSpace {
        total: fragment.saturating_mul(stats.f_blocks),
        available: fragment.saturating_mul(stats.f_bavail),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The call reads a real file system and reports a missing path as the OS error.
    #[test]
    fn statvfs_reads_a_mounted_file_system_and_refuses_a_missing_path() {
        let space = statvfs(&std::env::temp_dir()).unwrap();
        assert!(space.total > 0 && space.available <= space.total);
        let missing = statvfs(Path::new("/tetrad-sys-no-such-dir")).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    }
}

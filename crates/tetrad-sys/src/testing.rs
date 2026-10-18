//! Test-only helpers shared by module tests.

use std::ffi::CStr;
use std::fs::OpenOptions;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;

pub struct PtySlave {
    pub fd: OwnedFd,
    path: String,
}

impl PtySlave {
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// A fresh pseudo-terminal pair: (master, slave). Stands in for a UART in termios tests.
pub fn pty_pair() -> (OwnedFd, PtySlave) {
    // SAFETY: posix_openpt takes only flags and returns a new descriptor or -1.
    let raw = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    assert!(
        raw >= 0,
        "posix_openpt: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `raw` is a fresh descriptor that nothing else owns.
    let master = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut name = [0 as libc::c_char; 64];
    // SAFETY: `raw` is an open pty master; `name` is a live buffer of the length passed.
    unsafe {
        assert_eq!(libc::grantpt(raw), 0);
        assert_eq!(libc::unlockpt(raw), 0);
        assert_eq!(libc::ptsname_r(raw, name.as_mut_ptr(), name.len()), 0);
    }
    // SAFETY: ptsname_r succeeded, so `name` holds a NUL-terminated path.
    let path = unsafe { CStr::from_ptr(name.as_ptr()) }
        .to_str()
        .unwrap()
        .to_owned();
    let slave = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&path)
        .unwrap();
    (
        master,
        PtySlave {
            fd: slave.into(),
            path,
        },
    )
}

/// A fresh FIFO: (read end opened `O_RDONLY | O_NONBLOCK`, writer). The path is unlinked at once.
pub fn fifo(name: &str) -> (OwnedFd, std::fs::File) {
    let path = std::env::temp_dir().join(format!("tetrad-sys-{}-{name}", std::process::id()));
    let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: `c_path` is NUL-terminated and outlives the call.
    let made = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(
        made,
        0,
        "mkfifo {path:?}: {}",
        std::io::Error::last_os_error()
    );
    let reader = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&path)
        .unwrap();
    let writer = OpenOptions::new().write(true).open(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    (reader.into(), writer)
}

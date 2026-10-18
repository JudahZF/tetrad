//! Linux backends for the `tetrad_watchdog` lifecycle traits: [`watchdog::LinuxWatchdog`]
//! (`HardwareWatchdog`), [`splash::PosixSplashQueue`] (`SplashQueue`), [`power::LinuxPower`]
//! (`PowerBackend`) and [`boot::LinuxBoot`] (`BootBackend`).
//!
//! Stock runs each step through `system()` or a shell script. Here a step is a direct syscall
//! (`sync`, `mount`, `umount2`, `setrlimit`, file operations) or one program run without a shell.
//! A program that cannot start counts as a failed command, as `sh` reports exit 127.

pub mod boot;
pub mod power;
pub mod splash;
pub mod watchdog;

use std::ffi::CString;
use std::io;
use std::process::Command;

/// `e2fsck -C0 -p -f -v <device>`: ZenUI `0x66a208`/`0x66b0cc`, `S80drvinit:22`.
const E2FSCK_ARGS: [&str; 4] = ["-C0", "-p", "-f", "-v"];

/// Runs one program to completion; `true` for exit status 0.
fn run(command: &mut Command) -> bool {
    command.status().is_ok_and(|status| status.success())
}

fn c_string(text: &str) -> io::Result<CString> {
    CString::new(text).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn check(result: libc::c_int) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `umount <target>`: BusyBox calls `umount2(target, 0)` (no force, no lazy detach).
fn unmount(target: &str) -> io::Result<()> {
    let target = c_string(target)?;
    // SAFETY: `target` is a live NUL-terminated string; umount2 only reads it.
    check(unsafe { libc::umount2(target.as_ptr(), 0) })
}

/// `mount -t ext4 <device> <target>` with no options (flags 0, no data).
fn mount_ext4(device: &str, target: &str) -> io::Result<()> {
    let (device, target) = (c_string(device)?, c_string(target)?);
    // SAFETY: the three strings are live and NUL-terminated; NULL data is allowed for ext4.
    check(unsafe {
        libc::mount(
            device.as_ptr(),
            target.as_ptr(),
            c"ext4".as_ptr(),
            0,
            std::ptr::null(),
        )
    })
}

/// Owned fallback: `mount -o remount,ro <target>`. The kernel ignores source and type on a remount.
fn remount_read_only(target: &str) -> io::Result<()> {
    let target = c_string(target)?;
    // SAFETY: `target` is a live NUL-terminated string; NULL source, type and data are valid for MS_REMOUNT.
    check(unsafe {
        libc::mount(
            std::ptr::null(),
            target.as_ptr(),
            std::ptr::null(),
            libc::MS_REMOUNT | libc::MS_RDONLY,
            std::ptr::null(),
        )
    })
}

/// `umask(mask)` for the whole process; returns the mask before the call. Init starts stock
/// ZenUI with umask 022 (files 0644, folders 0755); an SSH shell has 077 (gap census r48 R1).
/// It cannot fail.
pub fn set_umask(mask: libc::mode_t) -> libc::mode_t {
    // SAFETY: umask takes a plain mode value and touches no process memory.
    unsafe { libc::umask(mask) }
}

/// `sync`: schedule writeback of every filesystem. It cannot fail.
fn sync() {
    // SAFETY: sync takes no arguments and touches no process memory.
    unsafe { libc::sync() }
}

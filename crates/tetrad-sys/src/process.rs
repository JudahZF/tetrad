//! Limits for helper processes the device program starts.

use std::ffi::{CStr, CString};
use std::fs;
use std::io;
use std::path::Path;
use std::process::{Child, Command};
use std::ptr;
use std::time::Duration;

use tetrad_hal::node::{Element, Node};

use crate::paths;

/// The user an extension runner switches to when it starts as root: `nobody`.
const RUNNER_ID: libc::uid_t = 65534;

/// Confines the calling process, an extension runner: limits on address space, open files,
/// processes and core dumps, a lower scheduling priority (nice 10), a raised OOM-kill
/// preference (the kernel picks the runner before the device program), no new privileges,
/// and, when it runs as root, the unprivileged user `nobody`. Call once at start, after the
/// module is read and before the engine allocates.
pub fn confine_extension_runner(address_space: usize) -> io::Result<()> {
    for (resource, value) in [
        (libc::RLIMIT_AS, address_space as libc::rlim_t),
        (libc::RLIMIT_NOFILE, 32),
        (libc::RLIMIT_NPROC, 64),
        (libc::RLIMIT_CORE, 0),
    ] {
        let limit = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        // SAFETY: `setrlimit` reads the initialized `rlimit` through a valid pointer for the call.
        check(unsafe { libc::setrlimit(resource, &limit) })?;
    }
    // SAFETY: `setpriority` only reads its integer arguments; `who` 0 names this process.
    check(unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 10) })?;
    fs::write("/proc/self/oom_score_adj", "500")?;
    // SAFETY: `prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)` only reads its integer arguments.
    check(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) })?;
    if is_root() {
        // SAFETY: an empty group list (length 0, null pointer) is valid for `setgroups`.
        check(unsafe { libc::setgroups(0, std::ptr::null()) })?;
        // SAFETY: `setgid` only reads its integer argument. The group goes before the user,
        // while the process may still change it.
        check(unsafe { libc::setgid(RUNNER_ID) })?;
        // SAFETY: `setuid` only reads its integer argument.
        check(unsafe { libc::setuid(RUNNER_ID) })?;
    }
    Ok(())
}

/// The user and group of extension programs (`app` in a manifest). The unit has no name for
/// it, and nothing else runs as it, so [`end_app_processes`] ends only what programs started.
pub const APP_ID: libc::uid_t = 2000;

fn check(result: libc::c_int) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn is_root() -> bool {
    // SAFETY: `geteuid` has no arguments and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// Readies the unit for an extension program. It makes `dir`, the program's data folder, and as
/// root it also ends what an earlier program left running ([`end_app_processes`]), gives `dir`
/// to [`APP_ID`], and lets group [`APP_ID`] write the framebuffer and read the footswitch nodes.
/// The nodes keep that group until the unit restarts; only programs are in it. Tetrad must never
/// write into `dir` as root: a link that the program puts there would redirect the write.
pub fn prepare_app(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{PermissionsExt, chown, lchown};
    fs::create_dir_all(dir)?;
    if !is_root() {
        return Ok(());
    }
    end_app_processes()?;
    // `lchown` never follows a link, and only Tetrad can write the folder that holds `dir`.
    if !fs::symlink_metadata(dir)?.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a folder",
            dir.display()
        )));
    }
    lchown(dir, Some(APP_ID), Some(APP_ID))?;
    let stomps = Element::all().map(|element| (Node::KnobStomp(element).path(), 0o640));
    for (node, mode) in [(paths::FRAMEBUFFER, 0o660)].into_iter().chain(stomps) {
        // `chown` and `chmod` follow the stock links `/dev/zencoder/knob_stompN` to their
        // `/dev/input/eventN` nodes.
        match chown(node, Some(0), Some(APP_ID)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            changed => changed.map_err(|e| io::Error::new(e.kind(), format!("{node}: {e}")))?,
        }
        fs::set_permissions(node, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// The private `/tmp` and `/dev/shm` of a program: `mount(2)` data for tmpfs.
const APP_TMPFS: &CStr = c"mode=1777,size=16m";

/// Starts an extension program (`app` in a manifest) at nice 10, as the runners run, so the
/// device loop keeps its CPU. As root, the program runs as [`APP_ID`] with no supplementary
/// groups and no new privileges: it cannot change Tetrad, the stock files or the settings. It
/// also gets its own mount namespace with an empty `/tmp` and `/dev/shm`: on the unit both are
/// mode 0777 without the sticky bit, and Tetrad and the stock programs write files of fixed
/// names there as root, which a link from the program would redirect. In that namespace the
/// command's working folder, the program's data folder, is also at `/mnt` (an empty stock
/// folder), where programs made for the stock unit, such as qcDOOM, keep their files. The kernel
/// kills the program when the thread that started it ends (`PR_SET_PDEATHSIG`): start it from
/// the device loop thread, so a program never outlives Tetrad on the screen. Call
/// [`prepare_app`] first.
pub fn spawn_app(command: &mut Command) -> io::Result<Child> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;
    let root = is_root();
    let data = (command.get_current_dir())
        .ok_or_else(|| io::Error::other("a program needs its data folder as working folder"))?;
    // Made here: the child must not allocate.
    let data = CString::new(data.as_os_str().as_bytes())?;
    // SAFETY: the hook runs in the child between fork and exec and calls only `unshare`,
    // `mount`, `setgroups`, `setgid`, `setuid`, `prctl` and `setpriority`, which are
    // async-signal-safe system calls. They read only integers, C strings made before the fork
    // and null pointers (an empty group list is a length 0 and a null pointer).
    unsafe {
        command.pre_exec(move || {
            if root {
                check(libc::unshare(libc::CLONE_NEWNS))?;
                // No mount below reaches the namespace of Tetrad.
                let private = libc::MS_REC | libc::MS_PRIVATE;
                check(libc::mount(
                    ptr::null(),
                    c"/".as_ptr(),
                    ptr::null(),
                    private,
                    ptr::null(),
                ))?;
                for dir in [c"/tmp", c"/dev/shm"] {
                    check(libc::mount(
                        c"tmpfs".as_ptr(),
                        dir.as_ptr(),
                        c"tmpfs".as_ptr(),
                        libc::MS_NOSUID | libc::MS_NODEV,
                        APP_TMPFS.as_ptr().cast(),
                    ))?;
                }
                check(libc::mount(
                    data.as_ptr(),
                    c"/mnt".as_ptr(),
                    ptr::null(),
                    libc::MS_BIND,
                    ptr::null(),
                ))?;
                // The group goes before the user, while the process may still change it.
                check(libc::setgroups(0, ptr::null()))?;
                check(libc::setgid(APP_ID))?;
                check(libc::setuid(APP_ID))?;
            }
            check(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))?;
            // After the user change, which clears the parent-death signal.
            check(libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0))?;
            check(libc::setpriority(libc::PRIO_PROCESS, 0, 10))
        });
    }
    command.spawn()
}

/// Ends every process of [`APP_ID`], such as a daemon that a program left behind when it ended.
/// Does nothing unless it runs as root.
pub fn end_app_processes() -> io::Result<()> {
    if !is_root() {
        return Ok(());
    }
    // SAFETY: the child below calls only `setuid`, `kill` and `_exit`, which are
    // async-signal-safe, so a fork of this threaded process is sound.
    match unsafe { libc::fork() } {
        -1 => Err(io::Error::last_os_error()),
        0 => {
            // SAFETY: `setuid` and `kill` only read their integer arguments. As APP_ID,
            // `kill(-1)` reaches exactly the processes of APP_ID, never the caller itself.
            let killed = unsafe {
                check(libc::setuid(APP_ID)).and_then(|()| check(libc::kill(-1, libc::SIGKILL)))
            };
            let ended = killed
                .or_else(|e| match e.raw_os_error() {
                    // No process of APP_ID was left.
                    Some(libc::ESRCH) => Ok(()),
                    _ => Err(e),
                })
                .is_ok();
            // SAFETY: `_exit` ends the child at once, without running the parent's handlers.
            unsafe { libc::_exit(i32::from(!ended)) }
        }
        child => {
            let mut status = 0;
            // SAFETY: `waitpid` writes the status through a valid pointer for the call.
            while unsafe { libc::waitpid(child, &mut status, 0) } == -1 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
                Ok(())
            } else {
                Err(io::Error::other(
                    "the processes of the program user did not end",
                ))
            }
        }
    }
}

/// CPU time the calling thread has used: the extension runner's slice clock, so waiting for
/// the CPU at low priority never counts as JavaScript work.
pub fn thread_cpu_time() -> Duration {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `clock_gettime` writes the `timespec` through a valid pointer for the call;
    // Linux always provides CLOCK_THREAD_CPUTIME_ID for the calling thread.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
    Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::thread_cpu_time;

    /// The runner's slice clock counts work, not waiting: a sleep adds almost nothing, a
    /// busy loop adds its time.
    #[test]
    fn thread_cpu_time_counts_work_not_waiting() {
        let start = thread_cpu_time();
        std::thread::sleep(Duration::from_millis(100));
        assert!(thread_cpu_time() - start < Duration::from_millis(20));
        let wall = Instant::now();
        while thread_cpu_time() - start < Duration::from_millis(30) {
            assert!(
                wall.elapsed() < Duration::from_secs(5),
                "the clock does not advance"
            );
        }
    }
}

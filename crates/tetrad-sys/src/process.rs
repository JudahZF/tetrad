//! Limits for helper processes the device program starts.

use std::io;
use std::process::{Child, Command};
use std::time::Duration;

/// The user an extension runner switches to when it starts as root: `nobody`.
const RUNNER_ID: libc::uid_t = 65534;

/// Confines the calling process, an extension runner: limits on address space, open files,
/// processes and core dumps, a lower scheduling priority (nice 10), a raised OOM-kill
/// preference (the kernel picks the runner before the device program), no new privileges,
/// and, when it runs as root, the unprivileged user `nobody`. Call once at start, after the
/// module is read and before the engine allocates.
pub fn confine_extension_runner(address_space: usize) -> io::Result<()> {
    let check = |result: libc::c_int| {
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    };
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
    std::fs::write("/proc/self/oom_score_adj", "500")?;
    // SAFETY: `prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)` only reads its integer arguments.
    check(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) })?;
    // SAFETY: `geteuid` has no arguments and cannot fail.
    if unsafe { libc::geteuid() } == 0 {
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

/// Starts an extension program (`app` in a manifest) at nice 10, as the runners run, so the
/// device loop keeps its CPU. The kernel kills the program when the thread that started it ends
/// (`PR_SET_PDEATHSIG`): start it from the device loop thread, so a program never outlives Tetrad
/// on the screen.
pub fn spawn_app(command: &mut Command) -> io::Result<Child> {
    use std::os::unix::process::CommandExt;
    // SAFETY: the hook runs in the child between fork and exec and calls only `prctl` and
    // `setpriority`, which are async-signal-safe and only read their integer arguments.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                || libc::setpriority(libc::PRIO_PROCESS, 0, 10) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()
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

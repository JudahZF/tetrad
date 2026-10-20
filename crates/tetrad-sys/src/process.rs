//! Limits for helper processes the device program starts.

use std::io;
use std::time::Duration;

/// Confines the calling process, an extension runner: an address-space limit, a lower
/// scheduling priority (nice 10) and a raised OOM-kill preference, so the kernel picks the
/// runner before the device program. Call once at start, before the engine allocates.
pub fn confine_extension_runner(address_space: usize) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: address_space as libc::rlim_t,
        rlim_max: address_space as libc::rlim_t,
    };
    // SAFETY: `setrlimit` reads the initialized `rlimit` through a valid pointer for the call.
    if unsafe { libc::setrlimit(libc::RLIMIT_AS, &limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `setpriority` only reads its integer arguments; `who` 0 names this process.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 10) } != 0 {
        return Err(io::Error::last_os_error());
    }
    std::fs::write("/proc/self/oom_score_adj", "500")
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

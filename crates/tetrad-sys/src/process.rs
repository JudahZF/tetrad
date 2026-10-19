//! Limits for helper processes the device program starts.

use std::io;

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

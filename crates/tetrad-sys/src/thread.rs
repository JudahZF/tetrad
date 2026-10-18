//! Thread scheduling.

use std::io;

/// Gives the calling thread the lowest scheduling priority (nice 19), for background work that
/// must not delay the main loop on the QC's single core.
pub fn lowest_priority() -> io::Result<()> {
    // SAFETY: `setpriority` only reads its integer arguments; on Linux `who` 0 with
    // `PRIO_PROCESS` names the calling thread.
    let result = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 19) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

//! POSIX message-queue sender for `tetrad_watchdog::splash`. `splashqueue` main `0x858c` (librt):
//! `mq_open("/splashqueue", O_WRONLY | O_CREAT, 0666, {flags 0, maxmsg 10, msgsize 4})`, `mq_send` of one
//! little-endian `int` at priority 0, then `mq_close`; exit 1 on any failure. ZenUI reaches the queue only
//! through `popen("/opt/neuraldsp/splashqueue N 2>&1")` (`0x65b540`), so this is the whole transport.

use std::ffi::CString;
use std::io;
use std::time::{Duration, SystemTime};

use tetrad_watchdog::splash::{
    MESSAGE_PRIORITY, MESSAGE_SIZE, QUEUE_MAX_MESSAGES, QUEUE_MODE, QUEUE_NAME, SplashCommand,
    SplashQueue,
};

/// Owned bound for a full queue. Stock `mq_send` blocks until the splash reads.
pub const STOCK_SEND_TIMEOUT: Duration = Duration::from_secs(1);

/// Opens, sends and closes per command, like each stock `splashqueue` run, so a queue that the splash
/// recreated is always the one used. Only the wait for space is bounded.
#[derive(Debug, Clone)]
pub struct PosixSplashQueue {
    name: CString,
    send_timeout: Duration,
}

impl PosixSplashQueue {
    /// `name` is a POSIX queue name: one leading `/` and no other `/`.
    pub fn new(name: &str, send_timeout: Duration) -> io::Result<Self> {
        let valid = name.len() > 1 && name.starts_with('/') && !name[1..].contains('/');
        if !valid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "queue name must be /<name>",
            ));
        }
        Ok(Self {
            name: super::c_string(name)?,
            send_timeout,
        })
    }

    /// `/splashqueue` with [`STOCK_SEND_TIMEOUT`].
    pub fn stock() -> Self {
        Self::new(QUEUE_NAME, STOCK_SEND_TIMEOUT).expect("stock queue name is valid")
    }
}

impl SplashQueue for PosixSplashQueue {
    type Error = io::Error;

    /// A full queue past the timeout gives `TimedOut`. An existing queue keeps its own attributes; one with a
    /// message size below 4 gives `EMSGSIZE`.
    fn send(&mut self, command: SplashCommand) -> io::Result<()> {
        let deadline = SystemTime::now()
            .checked_add(self.send_timeout)
            .and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok())
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let deadline = libc::timespec {
            tv_sec: libc::time_t::try_from(deadline.as_secs())
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?,
            // Below 10^9, so it fits every `c_long`.
            tv_nsec: deadline.subsec_nanos() as _,
        };
        // SAFETY: `mq_attr` is plain integers (plus private padding on some targets); all-zero is valid.
        let mut attr: libc::mq_attr = unsafe { std::mem::zeroed() };
        attr.mq_maxmsg = QUEUE_MAX_MESSAGES as _;
        attr.mq_msgsize = MESSAGE_SIZE as _;
        // SAFETY: `name` is NUL-terminated; with O_CREAT the variadic arguments are a `mode_t` (`c_uint` on
        // Linux) and a pointer to a live `mq_attr`, which the call only reads.
        let queue = unsafe {
            libc::mq_open(
                self.name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT,
                QUEUE_MODE,
                &attr as *const libc::mq_attr,
            )
        };
        if queue == -1 {
            return Err(io::Error::last_os_error());
        }
        let bytes = command.to_bytes();
        let sent = loop {
            // SAFETY: `queue` is open; `bytes` is a live 4-byte buffer and `deadline` a live timespec, both only read.
            let result = unsafe {
                libc::mq_timedsend(
                    queue,
                    bytes.as_ptr().cast(),
                    bytes.len(),
                    MESSAGE_PRIORITY,
                    &deadline,
                )
            };
            if result == 0 {
                break Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                break Err(error);
            }
        };
        // SAFETY: `queue` came from mq_open above and is closed exactly once.
        unsafe { libc::mq_close(queue) };
        sent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_names_follow_posix_rules() {
        assert!(PosixSplashQueue::new("/splashqueue", STOCK_SEND_TIMEOUT).is_ok());
        for bad in ["splashqueue", "/", "/a/b", "/a\0b"] {
            assert!(
                PosixSplashQueue::new(bad, STOCK_SEND_TIMEOUT).is_err(),
                "{bad:?}"
            );
        }
    }

    /// Creates and unlinks a private POSIX queue (not a file, FIFO, pipe or pty), so it is opt-in:
    /// `cargo test -p tetrad-sys -- --ignored splash`.
    #[test]
    #[ignore = "creates a private POSIX message queue"]
    fn stock_wire_format_attributes_and_bounded_full_queue() {
        let name = format!("/tetrad-sys-splash-{}", std::process::id());
        let mut queue = PosixSplashQueue::new(&name, Duration::ZERO).unwrap();
        queue.send(SplashCommand::HAND_OFF).unwrap();

        let c_name = CString::new(name).unwrap();
        // SAFETY: `c_name` is NUL-terminated; without O_CREAT no variadic arguments are read.
        let reader = unsafe { libc::mq_open(c_name.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
        assert_ne!(reader, -1, "{}", io::Error::last_os_error());
        // SAFETY: all-zero is a valid `mq_attr`.
        let mut attr: libc::mq_attr = unsafe { std::mem::zeroed() };
        // SAFETY: `reader` is open; mq_getattr fills the live `attr`.
        let got = unsafe { libc::mq_getattr(reader, &mut attr) };
        assert_eq!(got, 0);
        assert_eq!(
            (attr.mq_maxmsg, attr.mq_msgsize, attr.mq_curmsgs),
            (10, 4, 1)
        );

        let mut buffer = [0_u8; MESSAGE_SIZE];
        let mut priority = u32::MAX;
        // SAFETY: `buffer` is a live buffer of the queue's message size; `priority` a live out word.
        let read = unsafe {
            libc::mq_receive(
                reader,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut priority,
            )
        };
        assert_eq!((read, buffer, priority), (4, [0xe8, 0x03, 0, 0], 0));

        for value in 0..10 {
            queue.send(SplashCommand::new(value)).unwrap();
        }
        let full = queue.send(SplashCommand::DISMISS).unwrap_err();
        assert_eq!(full.kind(), io::ErrorKind::TimedOut);
        // SAFETY: `reader` is open and closed once; `c_name` is NUL-terminated.
        unsafe {
            libc::mq_close(reader);
            libc::mq_unlink(c_name.as_ptr());
        }
    }
}

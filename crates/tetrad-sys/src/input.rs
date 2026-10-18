//! Nonblocking 16-byte evdev record nodes with the stock libzenhal read shape. Records stay raw;
//! decode them with `tetrad_hal::input`.
//!
//! Only evdev nodes (`/dev/zencoder/knob_stomp1..11`, `volume`, `tempo`, `/dev/zjack/soundcard_express1/2`) and
//! FIFOs are accepted. The zjack misc nodes (`soundcard_input`, `soundcard_power_button`, `soundcard_over1/2`) ignore
//! `O_NONBLOCK` and do not return the copied byte count, so they are rejected as `Unsupported`; use `crate::jack`.

use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::mem;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tetrad_hal::input::{InputRecord, JackRecord};

pub const FRAME_LEN: usize = InputRecord::BYTE_LEN;
pub type Frame = [u8; FRAME_LEN];

const _: () = assert!(JackRecord::BYTE_LEN == FRAME_LEN);
// asm-generic/poll.h:35: int fd, short events, short revents on every Linux target.
const _: () = assert!(size_of::<libc::pollfd>() == 8);

// Device ABI facts, checked only when building for the device's 32-bit ARM target.
#[cfg(target_arch = "arm")]
const _: () = {
    // VA 0x173b0/0x1749c: open64 flags 0x800; sysroot asm-generic/fcntl.h:38.
    assert!(libc::O_NONBLOCK == 0o4000);
    // VA 0x1c9a0/0x1ca88: STRH #1 into pollfd.events; sysroot asm-generic/poll.h:5.
    assert!(libc::POLLIN == 1);
    // VA 0x172dc/0x1c9b8: read count 16 equals linux/input.h:22 with a 32-bit timeval.
    assert!(size_of::<libc::input_event>() == FRAME_LEN);
};

/// `poll` timeout used by `InputStatus` (VA 0x1c998) and `PowerButtonEvent` (VA 0x1ca80).
pub const STOCK_POLL_TIMEOUT: Duration = Duration::from_millis(250);

/// Zero-based knob/stomp element, 0..=10. `GetKnobStompEvent` rejects index > 10 at VA 0x172a8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnobStompIndex(u8);

impl KnobStompIndex {
    pub const COUNT: u8 = 11;

    pub fn new(index: u8) -> Option<Self> {
        (index < Self::COUNT).then_some(Self(index))
    }

    pub fn get(self) -> u8 {
        self.0
    }

    /// `CZencoder::Open` (VA 0x17444..0x17470) appends `%d` of index + 1 to `/dev/zencoder/knob_stomp`.
    pub fn node_path(self) -> PathBuf {
        PathBuf::from(format!("/dev/zencoder/knob_stomp{}", self.0 + 1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    pub readable: bool,
    pub hangup: bool,
    pub error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollOutcome {
    TimedOut,
    Ready(Readiness),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainEnd {
    /// `read` failed with EAGAIN. Stock drain loops stop on any `read <= 0` as well.
    WouldBlock,
    /// `read` returned 0. `truncated` bytes of an incomplete frame were discarded.
    EndOfFile { truncated: usize },
    /// `limit` frames were appended; more data may remain.
    Limit,
}

/// Linux `INPUT_MAJOR` and the kernel 4.0 legacy evdev minors (`EVDEV_MINOR_BASE` 64, `EVDEV_MINORS` 32).
/// Dynamic input minors (>= 256) are rejected: `dev_t` alone does not tell evdev from joydev or mousedev there.
const INPUT_MAJOR: libc::c_uint = 13;
const EVDEV_MINORS: std::ops::Range<libc::c_uint> = 64..96;

/// One open nonblocking evdev or FIFO node. Short reads are reassembled into whole frames instead of being
/// interpreted like stock does (it accepts any `read > 0` and parses stale stack bytes).
#[derive(Debug)]
pub struct RecordNode {
    file: File,
    partial: Frame,
    partial_len: usize,
}

impl RecordNode {
    /// Opens `path` read-only and nonblocking, like stock `open64(path, 0x800)`. Rust std also adds
    /// `O_CLOEXEC` (and glibc `open64` adds `O_LARGEFILE`); stock does not request close-on-exec.
    /// The node kind is checked before opening and again on the authoritative descriptor.
    /// The path must be caller-controlled: concurrent replacement can open an unsupported
    /// object before the descriptor check rejects it.
    pub fn open(path: &Path) -> io::Result<Self> {
        check_kind(&std::fs::metadata(path)?)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        check_kind(&file.metadata()?)?;
        Ok(Self {
            file,
            partial: [0; FRAME_LEN],
            partial_len: 0,
        })
    }

    /// Adopts a caller-owned evdev or FIFO descriptor. It must already be `O_NONBLOCK`.
    pub fn from_fd(fd: OwnedFd) -> io::Result<Self> {
        let file = File::from(fd);
        check_kind(&file.metadata()?)?;
        ensure_nonblocking(&file)?;
        Ok(Self {
            file,
            partial: [0; FRAME_LEN],
            partial_len: 0,
        })
    }

    /// Waits for `POLLIN` on this node. `None` waits forever; durations round up to whole milliseconds.
    /// EINTR is returned to the caller rather than retried with a stale timeout.
    pub fn poll(&self, timeout: Option<Duration>) -> io::Result<PollOutcome> {
        poll_readable(self.file.as_fd(), timeout)
    }

    /// Appends whole frames until EAGAIN, EOF or `limit` new frames. Each `read` asks for exactly the
    /// bytes missing from the current frame, so frame-aligned reads use the stock count of 16.
    ///
    /// `O_NONBLOCK` belongs to the open file description, which clones made through `AsFd` share. The flag is
    /// rechecked once at the start of each call (`InvalidInput` if cleared), but the caller must not let any
    /// holder of that description clear it while a `drain` runs; then a `read` would block.
    pub fn drain(&mut self, out: &mut Vec<Frame>, limit: usize) -> io::Result<DrainEnd> {
        ensure_nonblocking(&self.file)?;
        let mut appended = 0;
        while appended < limit {
            match self.file.read(&mut self.partial[self.partial_len..]) {
                Ok(0) => {
                    return Ok(DrainEnd::EndOfFile {
                        truncated: mem::take(&mut self.partial_len),
                    });
                }
                Ok(read) => {
                    self.partial_len += read;
                    if self.partial_len == FRAME_LEN {
                        out.push(self.partial);
                        self.partial_len = 0;
                        appended += 1;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Ok(DrainEnd::WouldBlock);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(DrainEnd::Limit)
    }
}

impl AsFd for RecordNode {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

/// One `pollfd` waiting for `POLLIN`, shared by every node in this crate. `None` waits forever; durations
/// round up to whole milliseconds. EINTR is returned to the caller; `POLLNVAL` becomes `EBADF`.
pub(crate) fn poll_readable(
    fd: BorrowedFd<'_>,
    timeout: Option<Duration>,
) -> io::Result<PollOutcome> {
    let mut entry = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `entry` is one valid, exclusively borrowed pollfd and nfds is 1. The borrowed descriptor stays
    // open for the whole call.
    let ready = unsafe { libc::poll(&mut entry, 1, poll_timeout_ms(timeout)) };
    let has = |bit: libc::c_short| entry.revents & bit != 0;
    match ready {
        ..0 => Err(io::Error::last_os_error()),
        0 => Ok(PollOutcome::TimedOut),
        _ if has(libc::POLLNVAL) => Err(io::Error::from_raw_os_error(libc::EBADF)),
        _ => Ok(PollOutcome::Ready(Readiness {
            readable: has(libc::POLLIN),
            hangup: has(libc::POLLHUP),
            error: has(libc::POLLERR),
        })),
    }
}

/// Accepts evdev character nodes and FIFOs: their `read` honours `O_NONBLOCK` and returns the bytes copied.
fn check_kind(metadata: &Metadata) -> io::Result<()> {
    let file_type = metadata.file_type();
    if file_type.is_fifo() || (file_type.is_char_device() && is_evdev(metadata.rdev())) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "record node must be an evdev character device (13:64..=95) or a FIFO",
    ))
}

fn is_evdev(rdev: libc::dev_t) -> bool {
    libc::major(rdev) == INPUT_MAJOR && EVDEV_MINORS.contains(&libc::minor(rdev))
}

fn ensure_nonblocking(file: &File) -> io::Result<()> {
    // SAFETY: F_GETFL takes no pointer argument and `file` keeps the descriptor open for the call.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::O_NONBLOCK == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "record node descriptor must be O_NONBLOCK",
        ));
    }
    Ok(())
}

fn poll_timeout_ms(timeout: Option<Duration>) -> libc::c_int {
    timeout.map_or(-1, |timeout| {
        libc::c_int::try_from(timeout.as_nanos().div_ceil(1_000_000)).unwrap_or(libc::c_int::MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use tetrad_hal::input::KnobStompEvent;

    /// A fresh FIFO opened through `RecordNode::open`, plus its writer. The path is unlinked at once.
    fn fifo(name: &str) -> (RecordNode, File) {
        let path = std::env::temp_dir().join(format!("tetrad-sys-{}-{name}", std::process::id()));
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c_path` is NUL-terminated and outlives the call.
        let made = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(made, 0, "mkfifo {path:?}: {}", io::Error::last_os_error());
        let node = RecordNode::open(&path).unwrap();
        let writer = OpenOptions::new().write(true).open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        (node, writer)
    }

    fn frame(event_type: u16, code: u16, value: i32) -> Frame {
        let mut b = [0; FRAME_LEN];
        b[..4].copy_from_slice(&7_i32.to_le_bytes());
        b[4..8].copy_from_slice(&500_i32.to_le_bytes());
        b[8..10].copy_from_slice(&event_type.to_le_bytes());
        b[10..12].copy_from_slice(&code.to_le_bytes());
        b[12..].copy_from_slice(&value.to_le_bytes());
        b
    }

    #[test]
    fn drain_reassembles_split_frames_honours_limit_and_reports_truncated_eof() {
        let (mut node, mut writer) = fifo("drain");
        let (knob, stomp, third) = (frame(2, 8, 255), frame(1, 273, 1), frame(1, 273, 0));
        writer.write_all(&knob).unwrap();
        writer.write_all(&stomp).unwrap();
        writer.write_all(&third[..5]).unwrap();

        let mut frames = Vec::new();
        assert_eq!(node.drain(&mut frames, 1).unwrap(), DrainEnd::Limit);
        assert_eq!(node.drain(&mut frames, 8).unwrap(), DrainEnd::WouldBlock);
        assert_eq!(frames, [knob, stomp]);

        writer.write_all(&third[5..]).unwrap();
        writer.write_all(&[0xaa; 3]).unwrap();
        drop(writer);
        assert_eq!(
            node.drain(&mut frames, 8).unwrap(),
            DrainEnd::EndOfFile { truncated: 3 }
        );
        assert_eq!(frames, [knob, stomp, third]);

        let events: Vec<_> = frames
            .iter()
            .map(|f| InputRecord::decode(f).unwrap().event())
            .collect();
        assert_eq!(
            events,
            [
                KnobStompEvent::Knob { delta: -1 },
                KnobStompEvent::Stomp { value: 1 },
                KnobStompEvent::Stomp { value: 0 },
            ]
        );
    }

    #[test]
    fn poll_reports_timeout_readable_and_hangup() {
        assert_eq!(poll_timeout_ms(None), -1);
        assert_eq!(poll_timeout_ms(Some(Duration::from_nanos(1))), 1);
        assert_eq!(poll_timeout_ms(Some(STOCK_POLL_TIMEOUT)), 250);
        assert_eq!(poll_timeout_ms(Some(Duration::MAX)), libc::c_int::MAX);

        let (node, mut writer) = fifo("poll");
        assert_eq!(
            node.poll(Some(Duration::ZERO)).unwrap(),
            PollOutcome::TimedOut
        );
        writer.write_all(&frame(1, 116, 1)).unwrap();
        let ready = |hangup| {
            PollOutcome::Ready(Readiness {
                readable: true,
                hangup,
                error: false,
            })
        };
        assert_eq!(node.poll(Some(STOCK_POLL_TIMEOUT)).unwrap(), ready(false));
        drop(writer);
        assert_eq!(node.poll(Some(STOCK_POLL_TIMEOUT)).unwrap(), ready(true));
    }

    #[test]
    fn nonblocking_is_required_at_adoption_and_rechecked_before_drain() {
        let (reader, _writer) = io::pipe().unwrap();
        let error = RecordNode::from_fd(OwnedFd::from(reader)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        // Safe code can clear O_NONBLOCK on the shared open file description through a clone.
        let (mut node, mut writer) = fifo("clone");
        writer.write_all(&frame(1, 273, 1)).unwrap();
        let clone = node.as_fd().try_clone_to_owned().unwrap();
        std::os::unix::net::UnixStream::from(clone)
            .set_nonblocking(false)
            .unwrap();
        let mut frames = Vec::new();
        let error = node.drain(&mut frames, 8).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(frames.is_empty());
    }

    #[test]
    fn only_evdev_and_fifo_nodes_are_accepted() {
        assert!(is_evdev(libc::makedev(13, 64)));
        assert!(is_evdev(libc::makedev(13, 95)));
        // Legacy mousedev minor, dynamic input minor, and a misc device such as `/dev/zenjack_input`.
        for (major, minor) in [(13, 63), (13, 96), (13, 256), (10, 64)] {
            assert!(!is_evdev(libc::makedev(major, minor)), "{major}:{minor}");
        }
        let error = RecordNode::open(Path::new("/dev/null")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn knob_stomp_paths_are_one_based() {
        let path = |i| KnobStompIndex::new(i).map(KnobStompIndex::node_path);
        assert_eq!(path(0), Some("/dev/zencoder/knob_stomp1".into()));
        assert_eq!(path(10), Some("/dev/zencoder/knob_stomp11".into()));
        assert_eq!(path(11), None);
    }
}

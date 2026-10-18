//! zenjack misc nodes that `input::RecordNode` rejects: jack status (`soundcard_input`), power button
//! (`soundcard_power_button`) and the overload waits (`soundcard_over1/2`).
//! The zjack misc record drivers define the contracts.
//!
//! These drivers ignore `O_NONBLOCK`: `read` sleeps until a per-node ready byte is set, with no timeout. The
//! record API therefore reads only after a positive `poll`, exactly once per poll, like stock.

use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::Duration;

use crate::input::{FRAME_LEN, Frame, PollOutcome, poll_readable};

/// Linux `MISC_MAJOR`.
const MISC_MAJOR: libc::c_uint = 10;

/// The read-count policy differs per driver, so success cannot be judged by the count alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiscRecordDriver {
    /// `zenjack_input_detector_fops` (`soundcard_input`, `InputStatus` VA 0x1c934): returns `count`.
    InputDetector,
    /// `zenjack_capsense_fops` (`soundcard_power_button`, `PowerButtonEvent` VA 0x1ca38): copies 16 bytes and
    /// returns the `__copy_to_user` result, 0 on success.
    Capsense,
}

impl MiscRecordDriver {
    fn success_count(self) -> usize {
        match self {
            Self::InputDetector => FRAME_LEN,
            Self::Capsense => 0,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JackError {
    #[error("{driver:?} read returned {returned}, not its success count")]
    UnexpectedCount {
        driver: MiscRecordDriver,
        returned: usize,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// One single-record zenjack node. `&mut self` keeps one reader per descriptor; the ready byte is global per
/// node, so a second reader anywhere (another process included) can make a read sleep. Run one owner only.
#[derive(Debug)]
pub struct MiscRecordNode {
    file: File,
    driver: MiscRecordDriver,
}

impl MiscRecordNode {
    /// Stock flags `0x800` (`O_RDONLY | O_NONBLOCK`, ignored by the driver) plus `O_CLOEXEC`. Accepts misc
    /// character devices (major 10) and FIFOs only; the kind is checked again on the descriptor.
    pub fn open(path: &Path, driver: MiscRecordDriver) -> io::Result<Self> {
        check_kind(&std::fs::metadata(path)?)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        check_kind(&file.metadata()?)?;
        Ok(Self { file, driver })
    }

    pub fn from_fd(fd: OwnedFd, driver: MiscRecordDriver) -> io::Result<Self> {
        let file = File::from(fd);
        check_kind(&file.metadata()?)?;
        Ok(Self { file, driver })
    }

    /// `poll(POLLIN, timeout)`, then one 16-byte read only if it was readable. `Ok(None)` on timeout, EINTR or a
    /// hangup without data. Stock uses [`crate::input::STOCK_POLL_TIMEOUT`].
    ///
    /// Retained driver hazard: if another reader cleared the ready byte between `poll` and `read`, the read sleeps,
    /// and a signal then returns the previous record with no error. Single ownership avoids that case.
    pub fn next_record(&mut self, timeout: Duration) -> Result<Option<Frame>, JackError> {
        match poll_readable(self.file.as_fd(), Some(timeout)) {
            Ok(PollOutcome::Ready(ready)) if ready.readable => {}
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let mut frame = [0; FRAME_LEN];
        let returned = self.file.read(&mut frame)?;
        if returned != self.driver.success_count() {
            return Err(JackError::UnexpectedCount {
                driver: self.driver,
                returned,
            });
        }
        Ok(Some(frame))
    }
}

impl AsFd for MiscRecordNode {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

/// `soundcard_over1/2` (`zenjack_over{1,2}_fops`): no `poll`; `read` sleeps until the overload flag is set,
/// ignores `O_NONBLOCK` and returns 0. Driver bug kept by the kernel: `over2` waits on flag +3 but clears +2.
#[derive(Debug)]
pub struct OverloadNode {
    file: File,
}

impl OverloadNode {
    /// Stock flags `0x800` plus `O_CLOEXEC`.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        Ok(Self { file })
    }

    pub fn from_fd(fd: OwnedFd) -> Self {
        Self {
            file: File::from(fd),
        }
    }

    /// Stock `Overload1/2` (VA 0x1c8cc/0x1c900): `read(fd, NULL, 0)`, a real syscall even for zero bytes.
    /// Blocks with no timeout: run it on a dedicated thread. Whether a signal returns 0 (looks like an event)
    /// or `EINTR` is not recovered; `EINTR` is returned as an error.
    pub fn wait(&self) -> io::Result<()> {
        // SAFETY: a zero-length read with a NULL buffer: the kernel writes nothing. `file` keeps the fd open.
        let result = unsafe { libc::read(self.file.as_raw_fd(), std::ptr::null_mut(), 0) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn check_kind(metadata: &Metadata) -> io::Result<()> {
    let file_type = metadata.file_type();
    if file_type.is_fifo()
        || (file_type.is_char_device() && libc::major(metadata.rdev()) == MISC_MAJOR)
    {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "zenjack record node must be a misc character device (major 10) or a FIFO",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn records_are_read_once_after_poll_with_a_per_driver_count_policy() {
        let path = std::env::temp_dir().join(format!("tetrad-sys-jack-{}", std::process::id()));
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c_path` is a valid NUL-terminated path for the duration of the call.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let mut node = MiscRecordNode::open(&path, MiscRecordDriver::InputDetector).unwrap();
        let mut writer = OpenOptions::new().write(true).open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        // No ready record: the timeout returns None and no read is attempted.
        assert!(
            node.next_record(Duration::from_millis(1))
                .unwrap()
                .is_none()
        );
        let record: Frame = std::array::from_fn(|i| i as u8);
        writer.write_all(&record).unwrap();
        assert_eq!(
            node.next_record(Duration::from_millis(100)).unwrap(),
            Some(record)
        );

        // A short record breaks the input-detector contract (it returns `count`).
        writer.write_all(&record[..8]).unwrap();
        assert!(matches!(
            node.next_record(Duration::from_millis(100)),
            Err(JackError::UnexpectedCount { returned: 8, .. })
        ));

        // Capsense success is 0, so 16 from a reader that is not capsense is refused, not decoded.
        let mut capsense = MiscRecordNode {
            file: node.file,
            driver: MiscRecordDriver::Capsense,
        };
        writer.write_all(&record).unwrap();
        assert!(matches!(
            capsense.next_record(Duration::from_millis(100)),
            Err(JackError::UnexpectedCount { returned: 16, .. })
        ));
        let null = File::open("/dev/null").unwrap();
        let error = MiscRecordNode::from_fd(null.into(), MiscRecordDriver::Capsense).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn overload_wait_issues_a_real_zero_length_read() {
        let (_reader, writer) = std::io::pipe().unwrap();
        // A write-only descriptor fails a read with EBADF, which proves the zero-length syscall ran.
        let node = OverloadNode::from_fd(writer.into());
        assert_eq!(node.wait().unwrap_err().raw_os_error(), Some(libc::EBADF));
        let (reader, _writer) = std::io::pipe().unwrap();
        OverloadNode::from_fd(reader.into()).wait().unwrap();
    }
}

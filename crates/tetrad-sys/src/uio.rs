//! UIO interrupt nodes (`/dev/uio0`, `/dev/uio1`) for the SHARC notification workers. Lifecycle:
//! `tetrad_runtime::worker`.
//!
//! Kernel 4.0 `uio_read` accepts only a 4-byte count (`EINVAL` otherwise) and returns the 32-bit event counter;
//! `uio_write` accepts only 4 bytes and passes the `s32` to `irqcontrol` (1 unmasks). Both are exact here.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::time::Duration;

use tetrad_runtime::worker::{ProtocolCore, UIO_REENABLE, UioOperations};

use crate::input::{PollOutcome, Readiness, poll_readable};

const WORD: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UioTransfer {
    ReadCount,
    WriteControl,
}

#[derive(Debug, thiserror::Error)]
pub enum UioError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("UIO {transfer:?} moved {actual} of 4 bytes")]
    Short {
        transfer: UioTransfer,
        actual: usize,
    },
    /// `POLLERR`/`POLLHUP` without `POLLIN`: the device is gone or has no IRQ.
    #[error("UIO node reported {0:?} without an interrupt")]
    Fault(Readiness),
}

/// One open UIO node. Stock opens it `O_RDWR` (VA 0x8a778c); Rust std adds `O_CLOEXEC`.
#[derive(Debug)]
pub struct UioDevice {
    file: File,
}

impl UioDevice {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file })
    }

    pub fn from_fd(fd: OwnedFd) -> Self {
        Self {
            file: File::from(fd),
        }
    }

    /// True when an interrupt count can be read. EINTR counts as a no-interrupt tick, like stock `poll <= 0`.
    pub fn poll(&self, timeout: Duration) -> Result<bool, UioError> {
        match poll_readable(self.file.as_fd(), Some(timeout)) {
            Ok(PollOutcome::TimedOut) => Ok(false),
            Ok(PollOutcome::Ready(ready)) if ready.readable => Ok(true),
            Ok(PollOutcome::Ready(ready)) => Err(UioError::Fault(ready)),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// One 4-byte read of the event counter. Blocks until the next interrupt unless `poll` reported one.
    pub fn read_count(&self) -> Result<u32, UioError> {
        let mut word = [0; WORD];
        let actual = retry_interrupted(|| (&self.file).read(&mut word))?;
        if actual != WORD {
            return Err(UioError::Short {
                transfer: UioTransfer::ReadCount,
                actual,
            });
        }
        Ok(u32::from_ne_bytes(word))
    }

    /// One 4-byte `irqcontrol` write: [`UIO_REENABLE`] (LE `1`) unmasks, LE `0` masks.
    pub fn write_control(&self, word: [u8; 4]) -> Result<(), UioError> {
        let actual = retry_interrupted(|| (&self.file).write(&word))?;
        if actual != WORD {
            return Err(UioError::Short {
                transfer: UioTransfer::WriteControl,
                actual,
            });
        }
        Ok(())
    }
}

impl AsFd for UioDevice {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

impl tetrad_dsp_host::Doorbell for UioDevice {
    type Error = UioError;

    fn wait(&mut self, timeout: Duration) -> Result<Option<u32>, UioError> {
        if !self.poll(timeout)? {
            return Ok(None);
        }
        self.read_count().map(Some)
    }

    fn rearm(&mut self) -> Result<(), UioError> {
        self.write_control(UIO_REENABLE)
    }
}

/// `tetrad_runtime::worker::UioOperations` with the stock failure policy (ZenUI `0x8a4ec4`, `0x8a4d58`). Stock starts
/// the worker even when `open64("/dev/uioN")` failed and then sleeps `usleep(15000)` per tick; it ignores the UIO
/// `read`/`write` results; every tick drains the rings. Here a device fault is counted and kept
/// ([`Self::faults`], [`Self::last_fault`]) and the worker keeps draining; only a `drain` error stops it. Owned: a
/// failed `poll` also sleeps the interval, so a broken node cannot spin the loop.
///
/// `drain` empties one protocol ring and owns delivery and release order (the dsp-host transport in wave 2). The
/// caller binds `/dev/uio0` to `UioRoute::Uio0` and `/dev/uio1` to `Uio1`. The uio2 worker (45 ms poll, one DIN
/// `0xF8` per interrupt, no ring) is `tetrad_app::device::linux::ClockOut`.
pub struct UioPort<F> {
    device: Option<UioDevice>,
    drain: F,
    count: [u8; 4],
    faults: u64,
    last_fault: Option<UioError>,
}

impl<F, E> UioPort<F>
where
    F: FnMut(ProtocolCore) -> Result<(), E>,
{
    /// `device` is `None` when the node could not be opened.
    pub fn new(device: Option<UioDevice>, drain: F) -> Self {
        Self {
            device,
            drain,
            count: [0; 4],
            faults: 0,
            last_fault: None,
        }
    }

    pub fn faults(&self) -> u64 {
        self.faults
    }

    pub fn last_fault(&self) -> Option<&UioError> {
        self.last_fault.as_ref()
    }
}

impl<F, E> UioOperations for UioPort<F>
where
    F: FnMut(ProtocolCore) -> Result<(), E>,
{
    type Error = E;

    fn reenable_interrupts(&mut self, bytes: [u8; 4]) -> Result<(), E> {
        if let Some(device) = &self.device
            && let Err(error) = device.write_control(bytes)
        {
            self.faults += 1;
            self.last_fault = Some(error);
        }
        Ok(())
    }

    /// Polls and, on an interrupt, reads the count at once. A failed read is a no-interrupt tick after the stock
    /// re-enable write; [`UioOperations::read_interrupt_count`] returns the count read here.
    fn poll_interrupt(&mut self, timeout: Duration) -> Result<bool, E> {
        let Self {
            device,
            count,
            faults,
            last_fault,
            ..
        } = self;
        let mut fault = |error| {
            *faults += 1;
            *last_fault = Some(error);
        };
        let Some(device) = device else {
            std::thread::sleep(timeout);
            return Ok(false);
        };
        match device.poll(timeout) {
            Ok(false) => Ok(false),
            Ok(true) => match device.read_count() {
                Ok(value) => {
                    *count = value.to_le_bytes();
                    Ok(true)
                }
                Err(error) => {
                    fault(error);
                    if let Err(error) = device.write_control(UIO_REENABLE) {
                        fault(error);
                    }
                    Ok(false)
                }
            },
            Err(error) => {
                fault(error);
                std::thread::sleep(timeout);
                Ok(false)
            }
        }
    }

    fn read_interrupt_count(&mut self) -> Result<[u8; 4], E> {
        Ok(self.count)
    }

    fn drain_ring(&mut self, core: ProtocolCore) -> Result<(), E> {
        (self.drain)(core)
    }
}

fn retry_interrupted(mut op: impl FnMut() -> io::Result<usize>) -> io::Result<usize> {
    loop {
        match op() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::os::unix::net::UnixStream;
    use tetrad_dsp_host::Doorbell;
    use tetrad_runtime::worker::{Cancellation, UioRoute, UioWorker, WorkerEvent, WorkerState};

    /// The peer end of a stream pair stands in for the kernel side of a UIO node.
    fn node() -> (UioDevice, UnixStream) {
        let (ours, kernel) = UnixStream::pair().unwrap();
        (UioDevice::from_fd(ours.into()), kernel)
    }

    fn take_word(kernel: &mut UnixStream) -> [u8; 4] {
        let mut word = [0; 4];
        kernel.read_exact(&mut word).unwrap();
        word
    }

    #[test]
    fn worker_lifecycle_runs_exact_words_and_keeps_draining_after_a_short_count() {
        let (device, mut kernel) = node();
        let mut drained = Vec::new();
        let mut port = UioPort::new(Some(device), |core| {
            drained.push(core);
            Ok::<_, Infallible>(())
        });
        let cancel = Cancellation::default();
        let mut worker = UioWorker::new(UioRoute::Uio0);

        assert_eq!(worker.arm(&mut port, &cancel).unwrap(), WorkerEvent::Armed);
        assert_eq!(
            [take_word(&mut kernel), take_word(&mut kernel)],
            [[1, 0, 0, 0]; 2]
        );

        let event = worker.step(&mut port, &cancel).unwrap();
        assert_eq!(event, WorkerEvent::Drained { interrupt: None });

        kernel.write_all(&7_u32.to_ne_bytes()).unwrap();
        let event = worker.step(&mut port, &cancel).unwrap();
        assert_eq!(event, WorkerEvent::Drained { interrupt: Some(7) });
        assert_eq!(take_word(&mut kernel), [1, 0, 0, 0]);

        // Stock ignores a failed read: the re-enable word still goes out and the rings still drain.
        kernel.write_all(&[9, 0]).unwrap();
        let event = worker.step(&mut port, &cancel).unwrap();
        assert_eq!(event, WorkerEvent::Drained { interrupt: None });
        assert_eq!(take_word(&mut kernel), [1, 0, 0, 0]);
        assert_eq!(worker.state(), WorkerState::Running);
        assert_eq!(port.faults(), 1);
        assert!(matches!(
            port.last_fault(),
            Some(UioError::Short {
                transfer: UioTransfer::ReadCount,
                actual: 2
            })
        ));
        drop(port);
        assert_eq!(drained, UioRoute::Uio0.drain_order().repeat(3));
    }

    /// Stock runs the worker without its node: each tick sleeps the poll interval and drains the rings.
    #[test]
    fn a_missing_node_sleeps_the_interval_and_still_drains() {
        let mut drained = 0;
        let mut port = UioPort::new(None, |_| {
            drained += 1;
            Ok::<_, Infallible>(())
        });
        let cancel = Cancellation::default();
        let mut worker = UioWorker::new(UioRoute::Uio1);
        assert_eq!(worker.arm(&mut port, &cancel).unwrap(), WorkerEvent::Armed);
        let start = std::time::Instant::now();
        let event = worker.step(&mut port, &cancel).unwrap();
        assert_eq!(event, WorkerEvent::Drained { interrupt: None });
        assert!(start.elapsed() >= tetrad_runtime::worker::UIO_POLL_INTERVAL);
        assert_eq!(port.faults(), 0);
        drop(port);
        assert_eq!(drained, UioRoute::Uio1.drain_order().len());
    }

    #[test]
    fn doorbell_waits_with_timeout_and_reports_a_vanished_node() {
        let (mut device, mut kernel) = node();
        assert_eq!(device.wait(Duration::from_millis(1)).unwrap(), None);
        kernel.write_all(&0x0102_0304_u32.to_ne_bytes()).unwrap();
        assert_eq!(
            device.wait(Duration::from_millis(100)).unwrap(),
            Some(0x0102_0304)
        );
        device.rearm().unwrap();
        assert_eq!(take_word(&mut kernel), UIO_REENABLE);

        drop(kernel);
        // A closed peer reads as EOF: zero of four bytes, never a count.
        assert!(matches!(
            device.wait(Duration::from_millis(100)),
            Err(UioError::Short {
                transfer: UioTransfer::ReadCount,
                actual: 0
            })
        ));
    }
}

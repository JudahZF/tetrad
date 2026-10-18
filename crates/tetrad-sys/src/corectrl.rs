//! SHARC core control through `/dev/corectrl`. Implements `tetrad_dsp_host::CoreControl`.
//!
//! Kernel dispatcher (uImage file `0x16690`): legacy request numbers with a scalar third argument.
//! Stock ZenUI opens the node `O_RDWR` and sends only stop (`0x8a8420`: `ioctl(fd, 0x6201, 1)`,
//! then core 2); start and entry requests come from `/bin/corecontrol` and loadsharc.
//! `tetrad_loader::exec::CoreControl` is implemented here too, for `crate::loader::BootHardware`.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;

use tetrad_loader::exec::AccessError;
use tetrad_loader::intent::{CoreAction, LoaderCore, ScalarRequest, core_control_intent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum CoreRequest {
    /// `0x166e4`: scalar core number.
    Start = 0x6200,
    /// `0x16694`: scalar core number.
    Stop = 0x6201,
    /// `0x166a4`: scalar entry address for core 1 (`RCU0_SVECT1`).
    Entry1 = 0x6d11,
    /// `0x166b0`: scalar entry address for core 2 (`RCU0_SVECT2`).
    Entry2 = 0x6d12,
}

#[derive(Debug, thiserror::Error)]
pub enum CoreControlError {
    /// Only SHARC cores 1 and 2 have recovered start/stop/entry behavior.
    #[error("SHARC core {0} is not 1 or 2")]
    InvalidCore(u8),
    /// The driver setter returns without a write for entry 0 (`0x1661c`); refuse it instead of a silent no-op.
    #[error("entry address 0 is ignored by the driver")]
    ZeroEntry,
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug)]
pub struct CoreControlNode {
    file: File,
}

impl CoreControlNode {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file })
    }

    pub fn from_fd(fd: OwnedFd) -> Self {
        Self {
            file: File::from(fd),
        }
    }

    fn send(&self, request: CoreRequest, argument: u32) -> Result<(), CoreControlError> {
        // SAFETY: these legacy requests take a scalar, not a pointer, so the driver dereferences nothing.
        // The owned descriptor stays open for the call.
        let result = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                request as libc::Ioctl,
                libc::c_ulong::from(argument),
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }
}

fn checked_core(core: u8) -> Result<u32, CoreControlError> {
    match core {
        1 | 2 => Ok(u32::from(core)),
        other => Err(CoreControlError::InvalidCore(other)),
    }
}

impl tetrad_dsp_host::CoreControl for CoreControlNode {
    type Error = CoreControlError;

    fn start(&mut self, core: u8) -> Result<(), CoreControlError> {
        self.send(CoreRequest::Start, checked_core(core)?)
    }

    fn stop(&mut self, core: u8) -> Result<(), CoreControlError> {
        self.send(CoreRequest::Stop, checked_core(core)?)
    }

    fn set_entry(&mut self, core: u8, address: u32) -> Result<(), CoreControlError> {
        let request = match core {
            1 => CoreRequest::Entry1,
            2 => CoreRequest::Entry2,
            other => return Err(CoreControlError::InvalidCore(other)),
        };
        if address == 0 {
            return Err(CoreControlError::ZeroEntry);
        }
        self.send(request, address)
    }
}

impl tetrad_loader::exec::CoreControl for CoreControlNode {
    /// One scalar ioctl built by `tetrad_loader::intent::core_control_intent`. A number outside the four recovered
    /// requests and entry 0 are refused before the syscall as `InvalidInput`.
    fn control(&mut self, core: LoaderCore, action: CoreAction) -> Result<(), AccessError> {
        let ScalarRequest { request, argument } = core_control_intent(core, action);
        let request = match request {
            0x6200 => CoreRequest::Start,
            0x6201 => CoreRequest::Stop,
            0x6d11 => CoreRequest::Entry1,
            0x6d12 => CoreRequest::Entry2,
            _ => return Err(AccessError::Device(io::ErrorKind::InvalidInput)),
        };
        if matches!(request, CoreRequest::Entry1 | CoreRequest::Entry2) && argument == 0 {
            return Err(AccessError::Device(io::ErrorKind::InvalidInput));
        }
        self.send(request, argument).map_err(|error| match error {
            CoreControlError::Io(error) => AccessError::Device(error.kind()),
            _ => AccessError::Device(io::ErrorKind::InvalidInput),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tetrad_dsp_host::CoreControl;

    #[test]
    fn only_cores_one_and_two_with_nonzero_entries_reach_the_driver() {
        let (reader, _writer) = std::io::pipe().unwrap();
        let mut node = CoreControlNode::from_fd(reader.into());
        // Refusals happen before any syscall; a pipe would otherwise answer ENOTTY.
        assert!(matches!(
            node.start(0),
            Err(CoreControlError::InvalidCore(0))
        ));
        assert!(matches!(
            node.stop(3),
            Err(CoreControlError::InvalidCore(3))
        ));
        assert!(matches!(
            node.set_entry(3, 0x0024_0000),
            Err(CoreControlError::InvalidCore(3))
        ));
        assert!(matches!(
            node.set_entry(1, 0),
            Err(CoreControlError::ZeroEntry)
        ));
        let Err(CoreControlError::Io(error)) = node.stop(2) else {
            panic!("a valid stop must be issued");
        };
        assert_eq!(error.raw_os_error(), Some(libc::ENOTTY));
    }
}

//! Execution of recovered `tetrad_hal` ioctl requests on the libzenhal control nodes: zencoder,
//! tempo control, display, touch info, zenjack soundcard and the ES codec.
//!
//! The kernel does not enforce `_IOC_SIZE`; each driver picks its copy length. A request therefore runs only
//! when its number is [`recovered`] for the node and its buffer has the encoded size. The recovered drivers copy
//! exactly that size (module `.rel.text` call sites: codec 4 B; zencoder 5, 4 and 16 B; tempo 1 and 5 B; zenjack
//! 4 and 16 B), so the one remaining assumption is that the stock kernel modules are loaded.
//!
//! Kernel returns are kept raw: a failed call yields `-errno` (glibc turns the kernel's negative return into
//! `-1` + `errno`), a positive helper status passes through. Nothing is retried, because setters are not
//! proven idempotent and stock never retries.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::time::Duration;

use tetrad_hal::analog::{AnalogError, DeviceEndpoint, GainControl, GainOperation, GainSet};
use tetrad_hal::codec::{Acknowledged, CodecError, GETTER_SENTINEL, Query, Reading, Setting};
use tetrad_hal::node::Node;
use tetrad_hal::platform::TouchController;
use tetrad_hal::requests::{IoctlPayload, IoctlRequest};
use tetrad_hal::{zencoder::number as zencoder, zenjack::number as zenjack};

/// The kernel return and the payload as the driver left it (getters overwrite `Integer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoctlReply {
    pub raw_return: i32,
    pub payload: IoctlPayload,
}

/// A request refused before the syscall. Both cases are caller errors, never driver answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("request {number:#010x} is not recovered for {node:?}")]
    Unrecovered { node: Node, number: u32 },
    #[error("request {number:#010x} encodes {encoded} bytes but the buffer has {actual}")]
    SizeMismatch {
        number: u32,
        encoded: usize,
        actual: usize,
    },
}

/// Request numbers libzenhal sends to each node (`CZencoder`, `CZendisplay`, `CZenjack`, codec users).
pub fn recovered(node: Node, number: u32) -> bool {
    match node {
        Node::Zencoder => matches!(
            number,
            zencoder::ELEMENT_LED
                | zencoder::READ_VOLUME
                | zencoder::APP_FIRMWARE
                | zencoder::BOOTLOADER_FIRMWARE
                | zencoder::BOARD_REVISION
                | zencoder::UNIQUE_ID
        ),
        Node::TempoControl => matches!(
            number,
            zencoder::TEMPO | zencoder::TEMPO_LED | zencoder::TEMPO_DEFAULT_LED
        ),
        // Enable, disable and reset (NULL argument, no size bits), then brightness.
        Node::Display => matches!(number, 0xe203..=0xe205 | 0x4004_e206),
        Node::TouchInfo => number == TouchController::REQUEST,
        // Words `0x00..=0x36`; `0x24` and `0x37` exist only as 16-byte blocks and `0x2d` is never sent.
        Node::Soundcard => match number {
            0x4004_e024 | 0x4004_e02d => false,
            0x4004_e000..=0x4004_e036 | zenjack::INITIAL_INPUT_STATUS | zenjack::UNIQUE_ID => true,
            _ => false,
        },
        Node::Codec => (0x4004_e200..=0x4004_e213).contains(&number),
        _ => false,
    }
}

/// Something that runs one recovered request. [`IoctlNode`] is the syscall backend; tests use fakes.
pub trait Ioctl {
    fn execute(&self, request: IoctlRequest) -> Result<IoctlReply, Refusal>;
}

/// One open control node bound to its stock identity, which selects the [`recovered`] requests. Stock libzenhal
/// opens these `O_RDONLY | O_NONBLOCK` (flags `0x800`); Rust std adds `O_CLOEXEC`. Opening sends no request.
#[derive(Debug)]
pub struct IoctlNode {
    file: File,
    node: Node,
}

impl IoctlNode {
    pub fn open(node: Node) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(node.path())?;
        Ok(Self { file, node })
    }

    /// Adopts a descriptor for `node`'s requests, for example `/dev/zendisplay-dev` (brightnessctrl) or a test pty.
    pub fn from_fd(node: Node, fd: OwnedFd) -> Self {
        Self {
            file: File::from(fd),
            node,
        }
    }

    pub fn node(&self) -> Node {
        self.node
    }

    /// Passes the payload buffer, or NULL for `Null`. NULL is always allowed: stock sends it with a sized number
    /// too (`0x4004e107`, VA 0x1717c) and the kernel faults safely on a NULL copy.
    ///
    /// # Safety
    ///
    /// The driver behind this descriptor must copy at most the payload length for `request.number`.
    unsafe fn execute_unchecked(&self, request: IoctlRequest) -> IoctlReply {
        let mut payload = request.payload;
        let pointer = payload
            .bytes_mut()
            .map_or(std::ptr::null_mut(), |buffer| buffer.as_mut_ptr());
        // SAFETY: `pointer` is NULL or a live, exclusively borrowed buffer, and the caller guarantees the driver
        // copies no more than its length. glibc takes `unsigned long`; a 32-bit request number is the same bits
        // on every target.
        let result = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                request.number as libc::Ioctl,
                pointer,
            )
        };
        let raw_return = if result < 0 {
            -io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO)
        } else {
            result
        };
        IoctlReply {
            raw_return,
            payload,
        }
    }
}

impl Ioctl for IoctlNode {
    fn execute(&self, request: IoctlRequest) -> Result<IoctlReply, Refusal> {
        if !recovered(self.node, request.number) {
            return Err(Refusal::Unrecovered {
                node: self.node,
                number: request.number,
            });
        }
        if let Some(buffer) = request.payload.bytes()
            && buffer.len() != request.encoded_size()
        {
            return Err(Refusal::SizeMismatch {
                number: request.number,
                encoded: request.encoded_size(),
                actual: buffer.len(),
            });
        }
        // SAFETY: the number is recovered for this node and the buffer has its encoded size, which is what every
        // recovered driver copies (module doc).
        Ok(unsafe { self.execute_unchecked(request) })
    }
}

impl AsFd for IoctlNode {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CodecIoError {
    #[error(transparent)]
    Refused(#[from] Refusal),
    #[error(transparent)]
    Codec(#[from] CodecError),
}

/// `/dev/zencodec_es9017_es9826` (`tetrad_hal::codec::DEVICE_PATH`). A zero setter return is only
/// an acknowledgement: the driver also returns 0 when the target ADC is absent.
pub fn codec_set(node: &impl Ioctl, setting: Setting) -> Result<Acknowledged, CodecIoError> {
    Ok(Acknowledged::from_return(
        node.execute(setting.request())?.raw_return,
    )?)
}

/// Runs a getter on a sentinel-filled word, so an absent ADC decodes as `NoResponse`, not as a value.
pub fn codec_get(node: &impl Ioctl, query: Query) -> Result<Reading, CodecIoError> {
    let reply = node.execute(query.request())?;
    let IoctlPayload::Integer(word) = reply.payload else {
        unreachable!("codec getters carry one integer word")
    };
    Ok(query.decode(reply.raw_return, word)?)
}

/// Analog gain nodes by `tetrad_hal::analog::DeviceEndpoint`. `_hwType` 0 boards use only the
/// zenjack soundcard node; types 1 and 2 use only the codec node. A missing node fails the
/// operation.
pub struct AnalogNodes<'a, T> {
    pub soundcard: Option<&'a T>,
    pub codec: Option<&'a T>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AnalogIoError {
    #[error("no node is bound for the {0:?} endpoint")]
    MissingNode(DeviceEndpoint),
    #[error(transparent)]
    Refused(#[from] Refusal),
    #[error(transparent)]
    Contract(#[from] AnalogError),
}

impl<T: Ioctl> AnalogNodes<'_, T> {
    fn node(&self, device: DeviceEndpoint) -> Result<&T, AnalogIoError> {
        match device {
            DeviceEndpoint::Soundcard => self.soundcard,
            DeviceEndpoint::Codec => self.codec,
        }
        .ok_or(AnalogIoError::MissingNode(device))
    }

    /// Fresh read of the current gain (`GetInputGain`/`GetRetGain` observed mode).
    pub fn observe(&self, control: &mut GainControl) -> Result<i32, AnalogIoError> {
        let read = control.observed_request();
        let (raw_return, word) = self.read(read.device, read.number)?;
        Ok(control.complete_observation(raw_return, word)?)
    }

    /// Drives a `GainSet` to completion: writes, `usleep`s and read-backs in the order tetrad-hal requests.
    /// Returns the stock success flag.
    pub fn run(&self, set: &mut GainSet<'_>) -> Result<bool, AnalogIoError> {
        loop {
            match set.operation() {
                GainOperation::Write(write) => {
                    let raw_return = self.node(write.device)?.execute(write.request)?.raw_return;
                    set.complete_write(raw_return)?;
                }
                GainOperation::Read(read) => {
                    let (raw_return, word) = self.read(read.device, read.number)?;
                    set.complete_read(raw_return, word)?;
                }
                GainOperation::SleepMicroseconds(duration) => {
                    std::thread::sleep(Duration::from_micros(duration.get().into()));
                    set.complete_sleep(0)?;
                }
                GainOperation::Finished { success } => return Ok(success),
            }
        }
    }

    /// Output words start as `GETTER_SENTINEL`, so an unanswered getter reads `-1`, never a valid gain.
    fn read(&self, device: DeviceEndpoint, number: u32) -> Result<(i32, [u8; 4]), AnalogIoError> {
        let reply = self.node(device)?.execute(IoctlRequest {
            number,
            payload: IoctlPayload::Integer(GETTER_SENTINEL.to_le_bytes()),
        })?;
        let IoctlPayload::Integer(word) = reply.payload else {
            unreachable!("gain reads carry one integer word")
        };
        Ok((reply.raw_return, word))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::num::NonZeroU32;
    use tetrad_hal::analog::{GainEndpoint, HardwareType, Verification};
    use tetrad_hal::codec::{Flag, GainChannel};

    #[test]
    fn syscall_path_keeps_driver_output_maps_errno_and_refuses_before_the_kernel() {
        let (master, slave) = crate::testing::pty_pair();
        let node = IoctlNode::from_fd(Node::Codec, master);
        // TIOCGPTN is `_IOR('T', 0x30, u32)`: the kernel writes the pty index through the pointer, the same
        // in/out word path a codec getter uses.
        let tiocgptn = |payload| IoctlRequest {
            number: libc::TIOCGPTN as u32,
            payload,
        };
        let sentinel = IoctlPayload::Integer(GETTER_SENTINEL.to_le_bytes());
        // SAFETY: TIOCGPTN copies one 4-byte word, the size of the `Integer` payload.
        let reply = unsafe { node.execute_unchecked(tiocgptn(sentinel)) };
        let index: u32 = slave.path().rsplit('/').next().unwrap().parse().unwrap();
        assert_eq!(
            reply,
            IoctlReply {
                raw_return: 0,
                payload: IoctlPayload::Integer(index.to_ne_bytes()),
            }
        );
        // NULL passes even with a sized number (stock `0x4004e107`); the kernel copy faults safely.
        // SAFETY: a NULL argument; the kernel copy to NULL fails with EFAULT.
        let null = unsafe { node.execute_unchecked(tiocgptn(IoctlPayload::Null)) };
        assert_eq!(null.raw_return, -libc::EFAULT);

        // The safe path refuses a number that is not recovered for the node, and a buffer that disagrees with
        // the encoded size, before any syscall.
        assert_eq!(
            node.execute(tiocgptn(sentinel)),
            Err(Refusal::Unrecovered {
                node: Node::Codec,
                number: libc::TIOCGPTN as u32,
            })
        );
        let mismatched = IoctlRequest {
            number: 0x4004_e202,
            payload: IoctlPayload::ElementLed([0xaa; 5]),
        };
        assert_eq!(
            node.execute(mismatched),
            Err(Refusal::SizeMismatch {
                number: 0x4004_e202,
                encoded: 4,
                actual: 5,
            })
        );
        let wrong_node = IoctlNode::from_fd(Node::Zencoder, std::io::pipe().unwrap().0.into());
        assert!(matches!(
            codec_set(&wrong_node, Setting::Flag(Flag::Phantom48v, true)),
            Err(CodecIoError::Refused(Refusal::Unrecovered { .. }))
        ));

        // A pipe is no codec: the kernel answers ENOTTY, which surfaces as the raw return, never as a reading.
        let (reader, _writer) = std::io::pipe().unwrap();
        let pipe = IoctlNode::from_fd(Node::Codec, reader.into());
        assert_eq!(
            codec_get(&pipe, Query::Gain(GainChannel::Inst1)),
            Err(CodecError::Return(-libc::ENOTTY).into())
        );
        assert_eq!(
            codec_set(&pipe, Setting::Flag(Flag::Phantom48v, true)),
            Err(CodecError::Return(-libc::ENOTTY).into())
        );
    }

    /// Every number tetrad-hal builds for a node is recovered there, and no codec number reaches a zencoder node.
    #[test]
    fn allow_list_matches_the_tetrad_hal_request_builders() {
        use tetrad_hal::requests::DisplayCommand;
        use tetrad_hal::zenjack::PowerCommand;
        for command in [
            DisplayCommand::Enable,
            DisplayCommand::Disable,
            DisplayCommand::Reset,
        ] {
            assert!(recovered(Node::Display, command.request().number));
        }
        for command in [
            PowerCommand::Restart,
            PowerCommand::Shutdown,
            PowerCommand::AutoOn(true),
        ] {
            assert!(recovered(Node::Soundcard, command.request().number));
        }
        assert!(recovered(Node::Soundcard, zenjack::INITIAL_INPUT_STATUS));
        assert!(!recovered(Node::Soundcard, 0x4004_e024));
        assert!(recovered(Node::TouchInfo, TouchController::REQUEST));
        assert!(recovered(Node::TempoControl, zencoder::TEMPO));
        assert!(!recovered(Node::Zencoder, 0x4004_e202));
        assert!(!recovered(
            Node::KnobStomp(0.try_into().unwrap()),
            zencoder::ELEMENT_LED
        ));
    }

    /// Records each request and answers getters with a stored word.
    struct FakeNode {
        log: RefCell<Vec<IoctlRequest>>,
        word: Option<i32>,
    }

    impl Ioctl for FakeNode {
        fn execute(&self, request: IoctlRequest) -> Result<IoctlReply, Refusal> {
            self.log.borrow_mut().push(request);
            let payload = match (request.payload, self.word) {
                (IoctlPayload::Integer(_), Some(word)) if request.number & 0xff >= 0x0a => {
                    IoctlPayload::Integer(word.to_le_bytes())
                }
                (payload, _) => payload,
            };
            Ok(IoctlReply {
                raw_return: 0,
                payload,
            })
        }
    }

    #[test]
    fn gain_set_routes_by_board_type_and_an_unanswered_read_back_fails() {
        let codec = FakeNode {
            log: RefCell::default(),
            word: None,
        };
        let nodes = AnalogNodes {
            soundcard: None,
            codec: Some(&codec),
        };
        let mut control = GainControl::new(HardwareType::Type1, GainEndpoint::Input1);
        let verification = Verification::ReadBack {
            attempts: NonZeroU32::new(2).unwrap(),
            sleep_microseconds: None,
        };
        let mut set = control.begin_set(4, verification).unwrap();
        // The fake leaves the sentinel in place, so read-back sees -1 and both attempts fail.
        assert_eq!(nodes.run(&mut set), Ok(false));
        let numbers: Vec<u32> = codec.log.borrow().iter().map(|r| r.number).collect();
        assert_eq!(
            numbers,
            [0x4004_e202, 0x4004_e20a, 0x4004_e202, 0x4004_e20a]
        );
        assert_eq!(control.last_observed(), Some(-1));

        let mut type0 = GainControl::new(HardwareType::Type0, GainEndpoint::Return2);
        assert_eq!(
            nodes.observe(&mut type0),
            Err(AnalogIoError::MissingNode(DeviceEndpoint::Soundcard))
        );
        let soundcard = FakeNode {
            log: RefCell::default(),
            word: Some(17),
        };
        let nodes = AnalogNodes {
            soundcard: Some(&soundcard),
            codec: None,
        };
        assert_eq!(nodes.observe(&mut type0), Ok(17));
        assert_eq!(soundcard.log.borrow()[0].number, 0x4004_e01c);
    }
}

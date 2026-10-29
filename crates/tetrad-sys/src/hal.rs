//! `tetrad_hal::hal::Driver` over the 26 stock libzenhal nodes (`tetrad_hal::node::Node::all`).
//! Policy stays in `tetrad_hal::hal::StockHal`; this module opens nodes and runs raw operations
//! through [`IoctlNode`], [`RecordNode`], [`MiscRecordNode`], [`OverloadNode`] and [`MidiUart`].

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tetrad_hal::hal::{Driver, HalError, RawInput};
use tetrad_hal::node::Node;
use tetrad_hal::platform::Platform;
use tetrad_hal::requests::IoctlRequest;

use crate::input::{Frame, RecordNode};
use crate::ioctl::{Ioctl, IoctlNode};
use crate::jack::{JackError, MiscRecordDriver, MiscRecordNode, OverloadNode};
use crate::serial::MidiUart;

/// The open nodes. A node that is absent answers ioctls with `-EBADF` and has no input.
#[derive(Debug, Default)]
pub struct LinuxHal {
    nodes: BTreeMap<Node, Binding>,
    midi: Option<MidiUart>,
    failures: Vec<(Node, io::Error)>,
    frames: Vec<Frame>,
    /// The record and misc nodes of the last [`Driver::begin_inputs`] check, and their `pollfd`s.
    polled: Vec<Node>,
    pollfds: Vec<libc::pollfd>,
}

#[derive(Debug)]
enum Binding {
    Control(IoctlNode),
    Records(RecordNode),
    Misc(MiscRecordNode),
    Overload(OverloadWatch),
}

impl LinuxHal {
    /// Opens every node stock opens for `platform`, in `Node::all` order. The codec opens only with an ES codec
    /// (`CodecOpen` after 0x00497fd8) and the MIDI UART only when 0x0049c14c passes (`CMIDI::Open(true)`).
    /// Owned: a node that fails is recorded in [`Self::failures`] and the rest still open; stock
    /// `CZenjack::Open` stops at the first failure and leaves the HAL closed.
    pub fn open(platform: Platform) -> Self {
        let mut hal = Self::default();
        for node in Node::all() {
            let wanted = match node {
                Node::Codec => platform.has_ess_codec(),
                Node::MidiUart => platform.uses_midi_uart(),
                _ => true,
            };
            if wanted && let Err(error) = hal.attach(node, None) {
                hal.failures.push((node, error));
            }
        }
        hal
    }

    /// Binds `node` to `fd`, or opens its stock path with its stock flags when `fd` is `None`. Replaces an earlier
    /// binding. Record nodes must be evdev nodes or FIFOs, zjack misc nodes misc devices or FIFOs.
    pub fn attach(&mut self, node: Node, fd: Option<OwnedFd>) -> io::Result<()> {
        let path = Path::new(node.path());
        let binding = match node {
            Node::Zencoder
            | Node::TempoControl
            | Node::Display
            | Node::TouchInfo
            | Node::Soundcard
            | Node::Codec => Binding::Control(match fd {
                Some(fd) => IoctlNode::from_fd(node, fd),
                None => IoctlNode::open(node)?,
            }),
            Node::KnobStomp(_) | Node::Volume | Node::Tempo | Node::Expression(_) => {
                Binding::Records(match fd {
                    Some(fd) => RecordNode::from_fd(fd)?,
                    None => RecordNode::open(path)?,
                })
            }
            Node::JackInput | Node::PowerButton => {
                let driver = if node == Node::JackInput {
                    MiscRecordDriver::InputDetector
                } else {
                    MiscRecordDriver::Capsense
                };
                Binding::Misc(match fd {
                    Some(fd) => MiscRecordNode::from_fd(fd, driver)?,
                    None => MiscRecordNode::open(path, driver)?,
                })
            }
            Node::Overload(_) => Binding::Overload(OverloadWatch::spawn(match fd {
                Some(fd) => OverloadNode::from_fd(fd),
                None => OverloadNode::open(path)?,
            })?),
            Node::MidiUart => {
                self.midi = Some(match fd {
                    Some(fd) => MidiUart::from_fd(fd)?,
                    None => MidiUart::open(path)?,
                });
                return Ok(());
            }
        };
        self.nodes.insert(node, binding);
        self.polled.clear();
        Ok(())
    }

    /// Nodes [`Self::open`] could not open, with the reason.
    pub fn failures(&self) -> &[(Node, io::Error)] {
        &self.failures
    }

    /// The configured MIDI UART for the MIDI service; `tetrad_hal::hal::Driver` has no MIDI
    /// operation.
    pub fn take_midi(&mut self) -> Option<MidiUart> {
        self.midi.take()
    }
}

impl Driver for LinuxHal {
    /// A request outside the node's recovered set, or with a wrongly sized buffer, is refused before the
    /// syscall with `-ENOTTY`: the kernel's own answer to a request a node does not take.
    fn ioctl(&mut self, node: Node, request: &mut IoctlRequest) -> i32 {
        let Some(Binding::Control(control)) = self.nodes.get(&node) else {
            return -libc::EBADF;
        };
        match control.execute(*request) {
            Ok(reply) => {
                request.payload = reply.payload;
                reply.raw_return
            }
            Err(_) => -libc::ENOTTY,
        }
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }

    /// Evdev nodes: one whole 16-byte frame per call (partial frames wait for the rest). zjack misc nodes: one
    /// zero-timeout `poll`, then exactly one read; stock polls 250 ms in a thread, `StockHal` polls without
    /// blocking. A `HalError::Read` code is `-errno`, or a positive unexpected byte count from a misc node.
    /// A node that the last [`Driver::begin_inputs`] check found quiet answers `None` without a syscall.
    fn next_input(&mut self, node: Node) -> Result<Option<RawInput>, HalError> {
        if let Some(index) = self.polled.iter().position(|&polled| polled == node)
            && self.pollfds[index].revents == 0
        {
            return Ok(None);
        }
        let read_error = |code| HalError::Read { node, code };
        match self.nodes.get_mut(&node) {
            None | Some(Binding::Control(_)) => Ok(None),
            Some(Binding::Records(records)) => {
                self.frames.clear();
                records
                    .drain(&mut self.frames, 1)
                    .map_err(|error| read_error(errno(&error)))?;
                Ok(self.frames.pop().map(RawInput::Record))
            }
            Some(Binding::Misc(misc)) => match misc.next_record(Duration::ZERO) {
                Ok(record) => Ok(record.map(RawInput::Record)),
                Err(JackError::UnexpectedCount { returned, .. }) => {
                    Err(read_error(i32::try_from(returned).unwrap_or(i32::MAX)))
                }
                Err(JackError::Io(error)) => Err(read_error(errno(&error))),
            },
            Some(Binding::Overload(watch)) => watch.next(node),
        }
    }

    /// Owned: one zero-timeout `poll` over every record and misc node, so a pass reads only the nodes
    /// with input (or a hangup or error, which the read then reports) instead of a `fcntl` and a `read`
    /// per quiet evdev node and a `poll` per misc node. A failed `poll` checks nothing: every node is read.
    fn begin_inputs(&mut self) {
        self.polled.clear();
        self.pollfds.clear();
        for (&node, binding) in &self.nodes {
            let fd = match binding {
                Binding::Records(records) => records.as_fd(),
                Binding::Misc(misc) => misc.as_fd(),
                Binding::Control(_) | Binding::Overload(_) => continue,
            };
            self.polled.push(node);
            self.pollfds.push(libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        // SAFETY: `pollfds` is an exclusively borrowed array of `len` initialized pollfds. `self.nodes` owns
        // every descriptor in it and `&mut self` keeps them open through the call. The timeout is zero.
        let ready = unsafe {
            libc::poll(
                self.pollfds.as_mut_ptr(),
                self.pollfds.len() as libc::nfds_t,
                0,
            )
        };
        if ready < 0 {
            self.polled.clear();
        }
    }
}

fn errno(error: &io::Error) -> i32 {
    -error.raw_os_error().unwrap_or(libc::EIO)
}

/// `Overload1/2` (VA 0x1c8cc/0x1c900) on a dedicated thread: a blocking zero-length read; each return is one
/// [`RawInput::Wake`]. The thread reads again only after the wake was taken, so overloads between two polls
/// give one wake (the driver flag stays set meanwhile). A read error is reported once and ends the thread.
#[derive(Debug)]
struct OverloadWatch {
    shared: Arc<Shared>,
    /// `StockHal::poll_events` reads until `None`; one wake per pass keeps that loop finite.
    reported: bool,
}

#[derive(Debug, Default)]
struct Shared {
    slot: Mutex<Slot>,
    taken: Condvar,
}

#[derive(Debug, Default)]
struct Slot {
    pending: Option<Result<(), i32>>,
    closed: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl OverloadWatch {
    fn spawn(overload: OverloadNode) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let watched = Arc::clone(&shared);
        // Detached: a thread blocked in the driver read ends at the next overload after `Drop` closes the slot.
        std::thread::Builder::new()
            .name("tetrad-overload".into())
            .spawn(move || {
                loop {
                    let result = match overload.wait() {
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        result => result.map_err(|error| errno(&error)),
                    };
                    let failed = result.is_err();
                    let mut slot = watched.lock();
                    if slot.closed {
                        return;
                    }
                    slot.pending = Some(result);
                    if failed {
                        return;
                    }
                    while slot.pending.is_some() && !slot.closed {
                        slot = watched
                            .taken
                            .wait(slot)
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                    if slot.closed {
                        return;
                    }
                }
            })?;
        Ok(Self {
            shared,
            reported: false,
        })
    }

    fn next(&mut self, node: Node) -> Result<Option<RawInput>, HalError> {
        if std::mem::take(&mut self.reported) {
            return Ok(None);
        }
        let pending = self.shared.lock().pending.take();
        self.shared.taken.notify_one();
        match pending {
            Some(Ok(())) => {
                self.reported = true;
                Ok(Some(RawInput::Wake))
            }
            Some(Err(code)) => Err(HalError::Read { node, code }),
            None => Ok(None),
        }
    }
}

impl Drop for OverloadWatch {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.taken.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::Instant;

    use tetrad_hal::analog::HardwareType;
    use tetrad_hal::hal::{Event, Hal, StockHal};
    use tetrad_hal::input::JackRecord;
    use tetrad_hal::node::{Element, OverloadChannel};
    use tetrad_hal::zenjack::{JackStatus, PowerCommand};

    use super::*;
    use crate::testing::fifo;

    fn frame(event_type: u16, code: u16, value: i32) -> Frame {
        let mut bytes = [0; 16];
        bytes[8..10].copy_from_slice(&event_type.to_le_bytes());
        bytes[10..12].copy_from_slice(&code.to_le_bytes());
        bytes[12..].copy_from_slice(&value.to_le_bytes());
        bytes
    }

    /// Unbound nodes answer `-EBADF`; a request outside the node's recovered set is refused with `-ENOTTY` and
    /// its payload is left alone; a recovered one reaches the kernel (a pipe answers `ENOTTY` too).
    #[test]
    fn ioctls_route_by_node_and_refuse_before_the_kernel() {
        let mut hal = LinuxHal::default();
        let mut restart = PowerCommand::Restart.request();
        assert_eq!(hal.ioctl(Node::Soundcard, &mut restart), -libc::EBADF);

        let (reader, _writer) = io::pipe().unwrap();
        hal.attach(Node::Soundcard, Some(reader.into())).unwrap();
        let mut codec_number = IoctlRequest::word(0x4004_e202, 7);
        assert_eq!(hal.ioctl(Node::Soundcard, &mut codec_number), -libc::ENOTTY);
        assert_eq!(codec_number.payload.word(), Some(7));
        assert_eq!(hal.ioctl(Node::Soundcard, &mut restart), -libc::ENOTTY);
        assert_eq!(hal.ioctl(Node::Codec, &mut restart), -libc::EBADF);
    }

    /// Through `StockHal::poll_events`: evdev frames arrive whole, the jack node gives one record per readable
    /// poll, and an overload wake is reported once per pass. A capsense node that returns 16 bytes instead of
    /// its stock 0 is a read error carrying the count.
    #[test]
    fn stock_hal_polls_evdev_jack_and_overload_nodes_through_the_driver() {
        let mut driver = LinuxHal::default();
        let (knob, mut knob_writer) = fifo("hal-knob");
        let (jack, mut jack_writer) = fifo("hal-jack");
        let (overload, _overload_writer) = io::pipe().unwrap();
        let element = Element::try_from(3).unwrap();
        driver.attach(Node::KnobStomp(element), Some(knob)).unwrap();
        driver.attach(Node::JackInput, Some(jack)).unwrap();
        let channel = OverloadChannel::One;
        driver
            .attach(Node::Overload(channel), Some(overload.into()))
            .unwrap();

        let rotate = frame(2, 8, 255);
        knob_writer.write_all(&rotate[..10]).unwrap();
        let jacks = [0x11; 16];
        jack_writer.write_all(&jacks).unwrap();
        // A pipe's zero-length read returns at once, so the watcher has a wake ready almost immediately.
        let deadline = Instant::now() + Duration::from_secs(2);
        while driver.shared_pending(channel).is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }

        let mut hal = StockHal::new(driver, HardwareType::Type0);
        let mut events = Vec::new();
        hal.poll_events(&mut events).unwrap();
        let status = JackStatus::from(JackRecord::decode(&jacks).unwrap());
        assert_eq!(
            events,
            [Event::Jacks(status), Event::Overload(channel)],
            "the half frame waits"
        );

        knob_writer.write_all(&rotate[10..]).unwrap();
        events.clear();
        hal.poll_events(&mut events).unwrap();
        assert_eq!(events[0], Event::Knob { element, delta: -1 });
        assert!(!events.contains(&Event::Jacks(status)));

        let mut driver = hal.into_driver();
        let (button, mut button_writer) = fifo("hal-button");
        driver.attach(Node::PowerButton, Some(button)).unwrap();
        button_writer.write_all(&frame(1, 116, 0)).unwrap();
        assert_eq!(
            driver.next_input(Node::PowerButton),
            Err(HalError::Read {
                node: Node::PowerButton,
                code: 16,
            })
        );
    }

    /// A node that the pass's check found quiet is not read in that pass; input that arrives after
    /// the check is read at the next one, not lost.
    #[test]
    fn a_quiet_node_is_read_again_after_the_next_check() {
        let mut driver = LinuxHal::default();
        let (knob, mut writer) = fifo("hal-quiet");
        let node = Node::KnobStomp(Element::try_from(0).unwrap());
        driver.attach(node, Some(knob)).unwrap();
        let rotate = frame(2, 8, 255);
        driver.begin_inputs();
        writer.write_all(&rotate).unwrap();
        assert_eq!(driver.next_input(node), Ok(None));
        driver.begin_inputs();
        assert_eq!(driver.next_input(node), Ok(Some(RawInput::Record(rotate))));
        assert_eq!(driver.next_input(node), Ok(None));
    }

    impl LinuxHal {
        fn shared_pending(&self, channel: OverloadChannel) -> Option<()> {
            match self.nodes.get(&Node::Overload(channel)) {
                Some(Binding::Overload(watch)) => watch.shared.lock().pending.map(drop),
                _ => None,
            }
        }
    }
}

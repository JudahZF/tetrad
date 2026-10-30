//! The hardware boundary that the app uses.
//!
//! - [`Driver`]: raw node operations. `tetrad-sys` implements it on Linux.
//! - [`Hal`]: typed commands, queries and input events.
//! - [`StockHal`]: `Hal` over a `Driver` with stock libzenhal routing, verify loops and caches.
//! - [`RecordingHal`]: an in-memory `Hal` that records every call, for app tests.
use crate::analog::{HardwareType, Verification};
use crate::input::{InputRecord, JackRecord, KnobStompEvent};
use crate::node::{Element, InvalidElement, Node, OverloadChannel, Pedal};
use crate::platform::Product;
use crate::requests::{DisplayCommand, ElementLed, IoctlPayload, IoctlRequest};
use crate::zencoder::{self, BoardUniqueId, FirmwareVersion, TempoConfig, VolumeEvent};
use crate::zenjack::{
    self, Boost, Control, Endpoint, Input, JackStatus, NcOutput, PowerCommand, Setting, Value,
    ZenjackError,
};
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

/// One raw input from an event node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawInput {
    /// One complete 16-byte record.
    Record([u8; 16]),
    /// An overload node's blocking zero-length read returned (`Overload1/2` VA 0xc8cc).
    Wake,
}

/// Raw node operations. Implementations must not retry, reinterpret or log.
pub trait Driver {
    /// One ioctl on `node`. The backend passes `request.payload` as the in/out
    /// buffer (NULL for `IoctlPayload::Null`) and returns the raw kernel result:
    /// zero or positive, or `-errno`. A node that is not open returns `-EBADF`.
    fn ioctl(&mut self, node: Node, request: &mut IoctlRequest) -> i32;

    /// `usleep` between a verified write and its readback. Interruptions are ignored.
    fn sleep(&mut self, duration: Duration);

    /// The next ready input on an event node without blocking, or `None`.
    fn next_input(&mut self, node: Node) -> Result<Option<RawInput>, HalError>;

    /// The start of one [`Hal::poll_events`] pass. A backend may check all its event nodes at
    /// once here; until the next call, [`Self::next_input`] may then answer `None` without I/O
    /// for a node that had nothing to read at that check.
    fn begin_inputs(&mut self) {}
}

/// Display panel, LEDs and other one-shot outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// `SetLedColor`: one footswitch RGB LED. Build the bytes with
    /// `zencoder::led_output`. The fifth byte is brightness: ZenUI computes it
    /// (0x00668e98) and libzenhal passes it through (0x16698). Index 11 and up is
    /// rejected before any ioctl.
    ElementLed(ElementLed),
    /// `SetTempoLedColor`. ZenUI sends index byte 1 and fifth byte 0xff.
    TempoLed(ElementLed),
    /// `SetTempoDefaultLedColor`.
    TempoDefaultLed,
    /// `SetTempo`.
    Tempo(TempoConfig),
    /// `CZendisplay` backlight and panel power.
    Display(DisplayCommand),
    /// A zenjack/codec setting with the stock verify policy.
    Set(Setting, Verification),
    /// `SetInputBoost`/`SetRetBoost`: raw word, no verify or cache.
    Boost(Boost, i32),
    Power(PowerCommand),
}

impl Command {
    /// `SetNCOutSel(sel, false, 3, false, 25000)` (libzenhal VA 0xc470), the only
    /// ZenUI call shape (V2 setup and abort 0x202fd8, V1 trainer close 0x3c9a04).
    /// Verify is off, so stock makes one `0x4004e011` write and caches the value;
    /// the retry count and pause are dead arguments.
    pub fn nc_output(output: NcOutput) -> Self {
        let setting = Setting::new(Control::NcOutput, Value::NcOutput(output))
            .expect("NcOutput value fits its control");
        Self::Set(setting, Verification::Unverified)
    }

    /// Rejects what stock rejects before any ioctl. With `hw` of `None` only the
    /// board-free element check runs.
    fn validate(self, hw: Option<HardwareType>) -> Result<(), HalError> {
        match (self, hw) {
            (Self::ElementLed(led), _) => {
                Element::try_from(led.index)?;
            }
            (Self::Set(setting, _), Some(hw)) => {
                setting.control().encode(hw, setting.value())?;
                setting.control().route(hw)?;
            }
            (Self::Boost(boost, _), Some(hw)) => {
                boost.route(hw)?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Values read from the hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Query {
    /// Observed (not cached) setting value. Updates the cache.
    Setting(Control),
    Boost(BoostTarget),
    /// `CZencoder::ReadVolume`.
    Volume,
    /// `CZenjack::InitialInputStatus`.
    InitialJacks,
    Firmware(Board, FirmwareKind),
    /// Raw `BoarRevisionGet` word.
    Revision(Board),
    UniqueId(Board),
}

/// `Boost` without a value, for queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BoostTarget {
    Input(Input),
    Return(zenjack::Return),
}

impl From<BoostTarget> for Boost {
    fn from(target: BoostTarget) -> Self {
        match target {
            BoostTarget::Input(input) => Self::Input(input),
            BoostTarget::Return(ret) => Self::Return(ret),
        }
    }
}

/// The two microcontroller boards behind libzenhal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Board {
    /// Encoders/LEDs, `/dev/zencoder/fd`.
    Zencoder,
    /// Jacks/power, `/dev/zjack/soundcard`.
    Zenjack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FirmwareKind {
    Application,
    Bootloader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    Value(Value),
    Word(i32),
    Jacks(JackStatus),
    /// `None` is stock `"undefined"` (zero word).
    Firmware(Option<FirmwareVersion>),
    UniqueId(BoardUniqueId),
}

/// Decoded hardware input, in node read order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// One `EV_REL` record; stock sums these per drain (`GetKnobStompEvent` VA 0x71e0).
    Knob {
        element: Element,
        delta: i32,
    },
    /// One `EV_KEY` 273 record. Stock keeps every raw value in order.
    Stomp {
        element: Element,
        value: i32,
    },
    Volume(VolumeEvent),
    Tempo(i32),
    Expression {
        pedal: Pedal,
        value: i32,
    },
    Jacks(JackStatus),
    /// `EV_KEY`/`KEY_POWER` value (`PowerButtonEvent` VA 0xca38); see
    /// `Platform::power_button_pressed`.
    PowerButton(i32),
    Overload(OverloadChannel),
}

impl Event {
    /// Stock decode of one input from `node`; records stock skips give `None`.
    pub fn decode(node: Node, input: RawInput) -> Option<Self> {
        let record = match (node, input) {
            (Node::Overload(channel), RawInput::Wake) => return Some(Self::Overload(channel)),
            (_, RawInput::Record(bytes)) => bytes,
            (_, RawInput::Wake) => return None,
        };
        if node == Node::JackInput {
            return Some(Self::Jacks(
                JackRecord::decode(&record).expect("16 bytes").into(),
            ));
        }
        let record = InputRecord::decode(&record).expect("16 bytes");
        match node {
            Node::KnobStomp(element) => match record.event() {
                KnobStompEvent::Knob { delta } => Some(Self::Knob { element, delta }),
                KnobStompEvent::Stomp { value } => Some(Self::Stomp { element, value }),
                KnobStompEvent::Unknown { .. } => None,
            },
            Node::Volume => VolumeEvent::decode(record).map(Self::Volume),
            Node::Tempo => zencoder::absolute_value(record).map(Self::Tempo),
            Node::Expression(pedal) => {
                zencoder::absolute_value(record).map(|value| Self::Expression { pedal, value })
            }
            Node::PowerButton => (record.event_type == 1 && record.code == 116)
                .then_some(Self::PowerButton(record.value)),
            _ => None,
        }
    }
}

/// Event nodes in stock polling order.
pub fn event_nodes() -> impl Iterator<Item = Node> {
    Element::all().map(Node::KnobStomp).chain([
        Node::Volume,
        Node::Tempo,
        Node::Expression(Pedal::One),
        Node::Expression(Pedal::Two),
        Node::JackInput,
        Node::PowerButton,
        Node::Overload(OverloadChannel::One),
        Node::Overload(OverloadChannel::Two),
    ])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HalError {
    #[error(transparent)]
    Zenjack(#[from] ZenjackError),
    #[error(transparent)]
    Element(#[from] InvalidElement),
    #[error("ioctl {number:#010x} on {node:?} returned {code}")]
    Ioctl { node: Node, number: u32, code: i32 },
    #[error("readback {read} does not match written {written} after {attempts} attempts")]
    Mismatch {
        written: i32,
        read: i32,
        attempts: u32,
    },
    #[error("read on {node:?} failed with {code}")]
    Read { node: Node, code: i32 },
    #[error("no recorded reading for {0:?}")]
    NoReading(Query),
    /// A safety layer refused the command before it reached the driver (for example 48 V
    /// phantom power in a bring-up run without `--allow-phantom`).
    #[error("refused before the driver: {0}")]
    Refused(&'static str),
}

/// One failed verify attempt. `Fatal` ends the whole verify call at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptError {
    /// Counts as a failed attempt; the loop tries again.
    Retry(HalError),
    /// Stock returns at once on the zenjack readback of `_hwType` 1 impedance.
    Fatal(HalError),
}

/// The typed hardware surface. Errors are explicit; nothing is silently skipped.
pub trait Hal {
    fn apply(&mut self, command: Command) -> Result<(), HalError>;
    fn read(&mut self, query: Query) -> Result<Reading, HalError>;
    /// The last value written or observed for `control`, without I/O. Stock keeps a
    /// zero-initialized `_cache`; Rust returns `None` before the first write or read.
    fn cached(&self, control: Control) -> Option<Value>;
    /// Moves every ready input event into `out`. Never blocks.
    fn poll_events(&mut self, out: &mut Vec<Event>) -> Result<(), HalError>;
}

/// `Hal` with stock libzenhal semantics over an injected [`Driver`].
#[derive(Debug)]
pub struct StockHal<D> {
    driver: D,
    hw: HardwareType,
    cache: BTreeMap<Control, Value>,
    /// Stock `.data` flags (VA 0x18678, initial 1): the first expression read per
    /// process uses the initial-value ioctl instead of the event node.
    expression_primed: [bool; 2],
}

impl<D: Driver> StockHal<D> {
    pub fn new(driver: D, hw: HardwareType) -> Self {
        Self {
            driver,
            hw,
            cache: BTreeMap::new(),
            expression_primed: [false; 2],
        }
    }

    /// `CZenjack::Open` board selection: one zenjack revision ioctl (stock accepts
    /// only a zero return, VA 0xcf60), then [`HardwareType::detect`].
    pub fn open(mut driver: D, product: &Product) -> Self {
        let mut request = IoctlRequest::word(zenjack::number::BOARD_REVISION, 0);
        let revision = (driver.ioctl(Node::Soundcard, &mut request) == 0)
            .then(|| request.payload.word().expect("word payload"));
        let hw = HardwareType::detect(revision, *product == Product::Atma);
        Self::new(driver, hw)
    }

    pub fn hardware(&self) -> HardwareType {
        self.hw
    }

    pub fn driver(&self) -> &D {
        &self.driver
    }

    pub fn into_driver(self) -> D {
        self.driver
    }

    fn ioctl(&mut self, node: Node, request: &mut IoctlRequest) -> Result<(), HalError> {
        let code = self.driver.ioctl(node, request);
        if code < 0 {
            return Err(HalError::Ioctl {
                node,
                number: request.number,
                code,
            });
        }
        Ok(())
    }

    fn write_word(&mut self, at: Endpoint, word: i32) -> Result<(), HalError> {
        self.ioctl(at.node, &mut IoctlRequest::word(at.number, word))
    }

    /// Getters pass a word the driver overwrites; stock does not initialize it
    /// (zero here). Stock setters accept only an exact zero return for these reads
    /// in some paths and `>= 0` in others; Rust accepts `>= 0`.
    fn read_word(&mut self, at: Endpoint) -> Result<i32, HalError> {
        let mut request = IoctlRequest::word(at.number, 0);
        self.ioctl(at.node, &mut request)?;
        Ok(request.payload.word().expect("word payload"))
    }

    /// Stock verify loop: write, [sleep], read back, [sleep], compare; repeat up to
    /// `attempts`. A failed ioctl is a failed attempt, and the last attempt's error
    /// is returned. Stock can compare stale stack data after a failed read; Rust does not.
    fn verified_write(
        &mut self,
        writes: &[Endpoint],
        word: i32,
        verification: Verification,
        mut read_back: impl FnMut(&mut Self) -> Result<i32, AttemptError>,
    ) -> Result<(), HalError> {
        let Verification::ReadBack {
            attempts,
            sleep_microseconds,
        } = verification
        else {
            return writes.iter().try_for_each(|&at| self.write_word(at, word));
        };
        let pause = sleep_microseconds.map(|us| Duration::from_micros(u64::from(us.get())));
        let mut failure = None;
        for _ in 0..attempts.get() {
            match self.verify_attempt(writes, word, pause, &mut read_back) {
                Ok(read) if read == word => return Ok(()),
                Ok(read) => {
                    failure = Some(HalError::Mismatch {
                        written: word,
                        read,
                        attempts: attempts.get(),
                    });
                }
                Err(AttemptError::Retry(error)) => failure = Some(error),
                Err(AttemptError::Fatal(error)) => return Err(error),
            }
        }
        Err(failure.expect("verify runs at least one attempt"))
    }

    fn verify_attempt(
        &mut self,
        writes: &[Endpoint],
        word: i32,
        pause: Option<Duration>,
        read_back: &mut impl FnMut(&mut Self) -> Result<i32, AttemptError>,
    ) -> Result<i32, AttemptError> {
        for &at in writes {
            self.write_word(at, word).map_err(AttemptError::Retry)?;
        }
        if let Some(pause) = pause {
            self.driver.sleep(pause);
        }
        let read = read_back(self)?;
        if let Some(pause) = pause {
            self.driver.sleep(pause);
        }
        Ok(read)
    }

    fn set(&mut self, setting: Setting, verification: Verification) -> Result<(), HalError> {
        let control = setting.control();
        let word = control.encode(self.hw, setting.value())?;
        let result = match control {
            // `_hwType` 1 impedance (VA 0xa9dc..0xab08): zenjack then codec write; verify reads
            // zenjack then the codec. A codec 0 for a written 8 counts as 8 when zenjack also reads
            // 8 (mask aliasing).
            Control::InputImpedance(input) if self.hw == HardwareType::Type1 => {
                let route = control.impedance_route(self.hw, input)?;
                let codec = route.codec.expect("type 1 has a codec route");
                let writes = [route.zenjack.write, codec.write];
                self.verified_write(&writes, word, verification, |hal| {
                    let zenjack = hal
                        .read_word(route.zenjack.verify)
                        .map_err(AttemptError::Fatal)?;
                    let read = hal.read_word(codec.verify).map_err(AttemptError::Retry)?;
                    Ok(if read == 0 && word == 8 && zenjack == 8 {
                        8
                    } else {
                        read
                    })
                })
            }
            _ => {
                let route = control.route(self.hw)?;
                self.verified_write(&[route.write], word, verification, |hal| {
                    hal.read_word(route.verify).map_err(AttemptError::Retry)
                })
            }
        };
        // Stock caches an unverified request even when the write fails; a verified
        // set caches only on a matching readback.
        if result.is_ok() || verification == Verification::Unverified {
            self.cache.insert(control, setting.value());
        }
        result
    }

    fn observe(&mut self, control: Control) -> Result<Value, HalError> {
        let at = match control {
            // `GetInputImpedance` (VA 0xaca4) reads the codec on `_hwType` 1.
            Control::InputImpedance(input) => {
                let route = control.impedance_route(self.hw, input)?;
                route.codec.unwrap_or(route.zenjack).observe
            }
            _ => control.route(self.hw)?.observe,
        };
        let value = control.decode(self.read_word(at)?)?;
        self.cache.insert(control, value);
        Ok(value)
    }

    fn read_block(&mut self, node: Node, number: u32) -> Result<[u8; 16], HalError> {
        let mut request = zenjack::block_request(number);
        self.ioctl(node, &mut request)?;
        match request.payload {
            IoctlPayload::Block(bytes) => Ok(bytes),
            _ => unreachable!("block request"),
        }
    }

    fn board_node(board: Board) -> Node {
        match board {
            Board::Zencoder => Node::Zencoder,
            Board::Zenjack => Node::Soundcard,
        }
    }
}

impl<D: Driver> Hal for StockHal<D> {
    fn apply(&mut self, command: Command) -> Result<(), HalError> {
        command.validate(Some(self.hw))?;
        let (node, mut request) = match command {
            Command::Set(setting, verification) => return self.set(setting, verification),
            Command::Boost(boost, word) => {
                let (write, _) = boost.route(self.hw)?;
                return self.write_word(write, word);
            }
            Command::ElementLed(led) => (Node::Zencoder, led.request()),
            Command::TempoLed(led) => (Node::TempoControl, led.tempo_request()),
            Command::TempoDefaultLed => (Node::TempoControl, zencoder::tempo_default_led_request()),
            Command::Tempo(tempo) => (Node::TempoControl, tempo.request()),
            Command::Display(display) => (Node::Display, display.request()),
            Command::Power(power) => (Node::Soundcard, power.request()),
        };
        self.ioctl(node, &mut request)
    }

    fn read(&mut self, query: Query) -> Result<Reading, HalError> {
        Ok(match query {
            Query::Setting(control) => Reading::Value(self.observe(control)?),
            Query::Boost(target) => {
                let (_, read) = Boost::from(target).route(self.hw)?;
                Reading::Word(self.read_word(read)?)
            }
            Query::Volume => Reading::Word(self.read_word(Endpoint {
                node: Node::Zencoder,
                number: zencoder::number::READ_VOLUME,
            })?),
            Query::InitialJacks => {
                let block =
                    self.read_block(Node::Soundcard, zenjack::number::INITIAL_INPUT_STATUS)?;
                Reading::Jacks(JackRecord::decode(&block).expect("16 bytes").into())
            }
            Query::Firmware(board, kind) => {
                let number = match (board, kind) {
                    (Board::Zencoder, FirmwareKind::Application) => zencoder::number::APP_FIRMWARE,
                    (Board::Zencoder, FirmwareKind::Bootloader) => {
                        zencoder::number::BOOTLOADER_FIRMWARE
                    }
                    (Board::Zenjack, FirmwareKind::Application) => zenjack::number::APP_FIRMWARE,
                    (Board::Zenjack, FirmwareKind::Bootloader) => {
                        zenjack::number::BOOTLOADER_FIRMWARE
                    }
                };
                let word = self.read_word(Endpoint {
                    node: Self::board_node(board),
                    number,
                })?;
                Reading::Firmware(FirmwareVersion::from_word(word))
            }
            Query::Revision(board) => {
                let number = match board {
                    Board::Zencoder => zencoder::number::BOARD_REVISION,
                    Board::Zenjack => zenjack::number::BOARD_REVISION,
                };
                Reading::Word(self.read_word(Endpoint {
                    node: Self::board_node(board),
                    number,
                })?)
            }
            Query::UniqueId(board) => {
                let number = match board {
                    Board::Zencoder => zencoder::number::UNIQUE_ID,
                    Board::Zenjack => zenjack::number::UNIQUE_ID,
                };
                Reading::UniqueId(BoardUniqueId(
                    self.read_block(Self::board_node(board), number)?,
                ))
            }
        })
    }

    fn cached(&self, control: Control) -> Option<Value> {
        self.cache.get(&control).copied()
    }

    fn poll_events(&mut self, out: &mut Vec<Event>) -> Result<(), HalError> {
        for (index, pedal) in [Pedal::One, Pedal::Two].into_iter().enumerate() {
            if !self.expression_primed[index] {
                self.expression_primed[index] = true;
                let mut request = zenjack::expression_request(pedal);
                if self.driver.ioctl(Node::Soundcard, &mut request) >= 0 {
                    let value = request.payload.word().expect("word payload");
                    out.push(Event::Expression { pedal, value });
                }
            }
        }
        self.driver.begin_inputs();
        for node in event_nodes() {
            while let Some(input) = self.driver.next_input(node)? {
                out.extend(Event::decode(node, input));
            }
        }
        Ok(())
    }
}

/// In-memory `Hal` for app tests: records every command and query, answers queries
/// from recorded settings and scripted readings, and replays injected events (the
/// role of stock's `simulatedZencoder`/`simulatedZenjack` globals).
#[derive(Debug, Default)]
pub struct RecordingHal {
    /// Board for the routing checks, as in `StockHal`. `None` runs only the
    /// board-free element check.
    pub hardware: Option<HardwareType>,
    pub commands: Vec<Command>,
    pub queries: Vec<Query>,
    readings: BTreeMap<Query, Reading>,
    events: VecDeque<Event>,
    cache: BTreeMap<Control, Value>,
}

impl RecordingHal {
    /// The reading returned for `query` until replaced.
    pub fn set_reading(&mut self, query: Query, reading: Reading) {
        self.readings.insert(query, reading);
    }

    /// Queues an event for the next `poll_events`.
    pub fn inject(&mut self, event: Event) {
        self.events.push_back(event);
    }
}

impl Hal for RecordingHal {
    fn apply(&mut self, command: Command) -> Result<(), HalError> {
        command.validate(self.hardware)?;
        self.commands.push(command);
        if let Command::Set(setting, _) = command {
            self.cache.insert(setting.control(), setting.value());
            self.readings.insert(
                Query::Setting(setting.control()),
                Reading::Value(setting.value()),
            );
        }
        Ok(())
    }

    fn read(&mut self, query: Query) -> Result<Reading, HalError> {
        self.queries.push(query);
        let reading = self
            .readings
            .get(&query)
            .copied()
            .ok_or(HalError::NoReading(query))?;
        if let (Query::Setting(control), Reading::Value(value)) = (query, reading) {
            self.cache.insert(control, value);
        }
        Ok(reading)
    }

    fn cached(&self, control: Control) -> Option<Value> {
        self.cache.get(&control).copied()
    }

    fn poll_events(&mut self, out: &mut Vec<Event>) -> Result<(), HalError> {
        out.extend(self.events.drain(..));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Impedance;
    use crate::zenjack::{GroundLift, XlrOut};
    use std::num::NonZeroU32;

    #[derive(Debug, PartialEq)]
    enum Op {
        Ioctl(Node, u32, Option<i32>),
        Sleep(u64),
    }

    /// Records operations; answers each request number from its scripted
    /// `(return, word)` queue, else returns 0 with the buffer unchanged.
    #[derive(Default)]
    struct Script {
        ops: Vec<Op>,
        answers: BTreeMap<u32, VecDeque<(i32, i32)>>,
        inputs: BTreeMap<Node, VecDeque<RawInput>>,
    }

    impl Script {
        fn answer(&mut self, number: u32, replies: impl IntoIterator<Item = (i32, i32)>) {
            self.answers.entry(number).or_default().extend(replies);
        }
    }

    impl Driver for Script {
        fn ioctl(&mut self, node: Node, request: &mut IoctlRequest) -> i32 {
            let reply = self
                .answers
                .get_mut(&request.number)
                .and_then(VecDeque::pop_front);
            let code = match (reply, &mut request.payload) {
                (Some((code, word)), IoctlPayload::Integer(bytes)) => {
                    *bytes = word.to_le_bytes();
                    code
                }
                _ => 0,
            };
            self.ops
                .push(Op::Ioctl(node, request.number, request.payload.word()));
            code
        }

        fn sleep(&mut self, duration: Duration) {
            self.ops.push(Op::Sleep(duration.as_micros() as u64));
        }

        fn next_input(&mut self, node: Node) -> Result<Option<RawInput>, HalError> {
            Ok(self.inputs.get_mut(&node).and_then(VecDeque::pop_front))
        }
    }

    fn readback(attempts: u32, sleep: Option<u32>) -> Verification {
        Verification::ReadBack {
            attempts: NonZeroU32::new(attempts).unwrap(),
            sleep_microseconds: sleep.and_then(NonZeroU32::new),
        }
    }

    fn lift(status: GroundLift) -> Setting {
        Setting::new(
            Control::XlrOutGroundLift(XlrOut::Two),
            Value::GroundLift(status),
        )
        .unwrap()
    }

    /// The capture output select is one unverified write; no read-back or pause.
    #[test]
    fn nc_output_is_one_unverified_write_and_cached() {
        let mut hal = StockHal::new(Script::default(), HardwareType::Type0);
        hal.apply(Command::nc_output(NcOutput::Capture)).unwrap();
        assert_eq!(
            hal.driver().ops,
            [Op::Ioctl(Node::Soundcard, 0x4004_e011, Some(1))]
        );
        assert_eq!(
            hal.cached(Control::NcOutput),
            Some(Value::NcOutput(NcOutput::Capture))
        );
    }

    /// Stock order per attempt: write, sleep, read, sleep, compare; the cache
    /// changes only after a matching readback.
    #[test]
    fn verified_set_retries_in_stock_order_and_caches_on_match() {
        let mut script = Script::default();
        script.answer(0x4004_e20f, [(0, 1), (0, 0)]);
        let mut hal = StockHal::new(script, HardwareType::Type2);
        let setting = lift(GroundLift::Grounded);
        hal.apply(Command::Set(setting, readback(3, Some(500))))
            .unwrap();
        let (set, get) = (0x4004_e207, 0x4004_e20f);
        let attempt = |read| {
            [
                Op::Ioctl(Node::Codec, set, Some(0)),
                Op::Sleep(500),
                Op::Ioctl(Node::Codec, get, Some(read)),
                Op::Sleep(500),
            ]
        };
        let expected: Vec<Op> = attempt(1).into_iter().chain(attempt(0)).collect();
        assert_eq!(hal.driver().ops, expected);
        assert_eq!(hal.cached(setting.control()), Some(setting.value()));

        // Exhausted mismatch: error, cache unchanged.
        let mut hal = StockHal::new(Script::default(), HardwareType::Type2);
        hal.driver.answer(0x4004_e20f, [(0, 1), (0, 1)]);
        let err = hal
            .apply(Command::Set(setting, readback(2, None)))
            .unwrap_err();
        assert_eq!(
            err,
            HalError::Mismatch {
                written: 0,
                read: 1,
                attempts: 2
            }
        );
        assert_eq!(hal.cached(setting.control()), None);
    }

    /// Unverified sets cache the request even when the write fails (stock).
    #[test]
    fn unverified_failure_still_caches_and_reports() {
        struct Failing;
        impl Driver for Failing {
            fn ioctl(&mut self, _: Node, _: &mut IoctlRequest) -> i32 {
                -5
            }
            fn sleep(&mut self, _: Duration) {}
            fn next_input(&mut self, _: Node) -> Result<Option<RawInput>, HalError> {
                Ok(None)
            }
        }
        let mut hal = StockHal::new(Failing, HardwareType::Type0);
        let setting = lift(GroundLift::Lifted);
        let err = hal.apply(Command::Set(setting, Verification::Unverified));
        assert!(matches!(
            err,
            Err(HalError::Ioctl {
                code: -5,
                number: 0x4004_e00c,
                ..
            })
        ));
        assert_eq!(hal.cached(setting.control()), Some(setting.value()));
        // Unsupported targets fail before any ioctl and never cache.
        let gnd = Setting::new(
            Control::ReturnGroundLift(zenjack::Return::One),
            Value::GroundLift(GroundLift::Lifted),
        )
        .unwrap();
        let mut hal = StockHal::new(Failing, HardwareType::Type2);
        assert!(matches!(
            hal.apply(Command::Set(gnd, Verification::Unverified)),
            Err(HalError::Zenjack(ZenjackError::Unsupported { .. }))
        ));
        assert_eq!(hal.cached(gnd.control()), None);
    }

    /// `_hwType` 1 impedance writes zenjack then codec, reads both, and accepts a
    /// codec 0 for a written 8 when zenjack reads 8.
    #[test]
    fn type1_impedance_two_step_with_index8_alias() {
        let mut hal = StockHal::new(Script::default(), HardwareType::Type1);
        hal.driver.answer(0x4004_e017, [(0, 8)]);
        hal.driver.answer(0x4004_e213, [(0, 0), (0, 3)]);
        let z8 = Value::Impedance(Impedance::try_from(8).unwrap());
        let setting = Setting::new(Control::InputImpedance(Input::Two), z8).unwrap();
        hal.apply(Command::Set(setting, readback(1, None))).unwrap();
        assert_eq!(
            hal.driver().ops,
            [
                Op::Ioctl(Node::Soundcard, 0x4004_e005, Some(8)),
                Op::Ioctl(Node::Codec, 0x4004_e201, Some(8)),
                Op::Ioctl(Node::Soundcard, 0x4004_e017, Some(8)),
                Op::Ioctl(Node::Codec, 0x4004_e213, Some(0)),
            ]
        );
        // Observed impedance on type 1 reads the codec getter.
        assert_eq!(
            hal.read(Query::Setting(setting.control())),
            Ok(Reading::Value(Value::Impedance(
                Impedance::try_from(3).unwrap()
            )))
        );
        assert_eq!(
            hal.driver().ops.last(),
            Some(&Op::Ioctl(Node::Codec, 0x4004_e213, Some(3)))
        );
    }

    #[test]
    fn poll_primes_expression_once_then_decodes_nodes_in_order() {
        let record = |event_type: u16, code: u16, value: i32| {
            let mut b = [0u8; 16];
            b[8..10].copy_from_slice(&event_type.to_le_bytes());
            b[10..12].copy_from_slice(&code.to_le_bytes());
            b[12..].copy_from_slice(&value.to_le_bytes());
            RawInput::Record(b)
        };
        let element = Element::try_from(3).unwrap();
        let mut script = Script::default();
        script.answer(0x4004_e02a, [(0, 120)]);
        script.answer(0x4004_e02b, [(-19, 0)]);
        script.inputs.insert(
            Node::KnobStomp(element),
            [record(2, 8, 255), record(1, 273, 1), record(4, 4, 7)].into(),
        );
        script
            .inputs
            .insert(Node::Expression(Pedal::Two), [record(3, 0, 64)].into());
        script
            .inputs
            .insert(Node::PowerButton, [record(1, 116, 1)].into());
        let mut jack = [0u8; 16];
        jack[0] = 0x80;
        jack[4] = 0x40;
        script
            .inputs
            .insert(Node::JackInput, [RawInput::Record(jack)].into());
        script.inputs.insert(
            Node::Overload(OverloadChannel::Two),
            [RawInput::Wake].into(),
        );
        let mut hal = StockHal::new(script, HardwareType::Type0);
        let mut events = Vec::new();
        hal.poll_events(&mut events).unwrap();
        assert_eq!(
            events,
            [
                Event::Expression {
                    pedal: Pedal::One,
                    value: 120
                },
                Event::Knob { element, delta: -1 },
                Event::Stomp { element, value: 1 },
                Event::Expression {
                    pedal: Pedal::Two,
                    value: 64
                },
                Event::Jacks(JackStatus {
                    changed: 0x80,
                    plugged: 0x40
                }),
                Event::PowerButton(1),
                Event::Overload(OverloadChannel::Two),
            ]
        );
        events.clear();
        hal.poll_events(&mut events).unwrap();
        assert!(events.is_empty());
        let primes = hal
            .driver()
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Ioctl(_, 0x4004_e02a | 0x4004_e02b, _)))
            .count();
        assert_eq!(primes, 2);
    }

    /// Every request a `Hal` call can build carries a buffer whose length equals the
    /// `_IOC` size field, except the stock NULL-argument requests.
    #[test]
    fn every_request_payload_matches_its_ioc_size() {
        use crate::requests::Brightness;
        use crate::zencoder::LedColor;
        let led = ElementLed::palette(Element::try_from(0).unwrap(), LedColor::Red, 0);
        let mut requests = vec![
            led.request(),
            led.tempo_request(),
            TempoConfig(1).request(),
            DisplayCommand::Brightness(Brightness::try_from(50).unwrap()).request(),
            zenjack::block_request(zenjack::number::UNIQUE_ID),
            zenjack::block_request(zencoder::number::UNIQUE_ID),
            zenjack::block_request(zenjack::number::INITIAL_INPUT_STATUS),
            zenjack::expression_request(Pedal::Two),
            PowerCommand::Restart.request(),
            PowerCommand::AutoOn(true).request(),
        ];
        for number in [
            zencoder::number::READ_VOLUME,
            zencoder::number::APP_FIRMWARE,
            zenjack::number::BOARD_REVISION,
        ] {
            requests.push(IoctlRequest::word(number, 0));
        }
        for request in requests {
            let len = request.payload.bytes().map_or(0, <[u8]>::len);
            assert_eq!(request.encoded_size(), len, "{:#x}", request.number);
        }
        let nulls = [
            zencoder::tempo_default_led_request(),
            DisplayCommand::Enable.request(),
        ];
        for request in nulls {
            assert_eq!(request.payload, IoctlPayload::Null);
        }
    }

    #[test]
    fn recording_hal_records_and_answers_from_settings() {
        let mut hal = RecordingHal::default();
        let setting = lift(GroundLift::Grounded);
        hal.apply(Command::Set(setting, Verification::Unverified))
            .unwrap();
        hal.apply(Command::Power(PowerCommand::Shutdown)).unwrap();
        assert_eq!(hal.commands.len(), 2);
        let query = Query::Setting(setting.control());
        assert_eq!(hal.read(query), Ok(Reading::Value(setting.value())));
        assert_eq!(
            hal.read(Query::Volume),
            Err(HalError::NoReading(Query::Volume))
        );
        hal.set_reading(Query::Volume, Reading::Word(7));
        assert_eq!(hal.read(Query::Volume), Ok(Reading::Word(7)));
        hal.inject(Event::Tempo(3));
        let mut events = Vec::new();
        hal.poll_events(&mut events).unwrap();
        assert_eq!(events, [Event::Tempo(3)]);
    }

    /// An element index past 10 fails before any ioctl, on both `Hal` impls.
    #[test]
    fn element_led_index_is_checked_before_any_ioctl() {
        let led = |index| ElementLed {
            index,
            red: 0,
            green: 0,
            blue: 0,
            opaque: 0,
        };
        let mut hal = StockHal::new(Script::default(), HardwareType::Type0);
        assert_eq!(
            hal.apply(Command::ElementLed(led(11))),
            Err(HalError::Element(InvalidElement(11)))
        );
        assert!(hal.driver().ops.is_empty());
        assert!(hal.apply(Command::ElementLed(led(10))).is_ok());

        let mut recording = RecordingHal::default();
        assert!(recording.apply(Command::ElementLed(led(11))).is_err());
        assert!(recording.commands.is_empty());
    }

    /// A failed ioctl inside the verify loop is one failed attempt. Type1 impedance
    /// stops at once when its zenjack readback fails.
    #[test]
    fn failed_ioctl_counts_as_a_verify_attempt() {
        let (set, get) = (0x4004_e207, 0x4004_e20f);
        let setting = lift(GroundLift::Grounded);

        let mut script = Script::default();
        script.answer(set, [(-5, 0)]);
        script.answer(get, [(0, 0)]);
        let mut hal = StockHal::new(script, HardwareType::Type2);
        hal.apply(Command::Set(setting, readback(3, Some(25_000))))
            .unwrap();
        assert_eq!(
            hal.driver().ops,
            [
                Op::Ioctl(Node::Codec, set, Some(0)),
                Op::Ioctl(Node::Codec, set, Some(0)),
                Op::Sleep(25_000),
                Op::Ioctl(Node::Codec, get, Some(0)),
                Op::Sleep(25_000),
            ]
        );
        assert_eq!(hal.cached(setting.control()), Some(setting.value()));

        let mut script = Script::default();
        script.answer(set, [(-5, 0), (-7, 0)]);
        let mut hal = StockHal::new(script, HardwareType::Type2);
        let err = hal
            .apply(Command::Set(setting, readback(2, None)))
            .unwrap_err();
        assert_eq!(
            err,
            HalError::Ioctl {
                node: Node::Codec,
                number: set,
                code: -7,
            }
        );
        assert_eq!(hal.driver().ops.len(), 2);

        let z8 = Value::Impedance(Impedance::try_from(8).unwrap());
        let impedance = Setting::new(Control::InputImpedance(Input::Two), z8).unwrap();
        let mut script = Script::default();
        script.answer(0x4004_e017, [(-5, 0)]);
        let mut hal = StockHal::new(script, HardwareType::Type1);
        let err = hal
            .apply(Command::Set(impedance, readback(3, None)))
            .unwrap_err();
        assert!(matches!(
            err,
            HalError::Ioctl {
                number: 0x4004_e017,
                code: -5,
                ..
            }
        ));
        assert_eq!(hal.driver().ops.len(), 3);
    }

    /// `RecordingHal` applies the board rules of `StockHal` and records nothing it rejects.
    #[test]
    fn recording_hal_rejects_what_stock_rejects() {
        let gain =
            |value| Setting::new(Control::InputGain(Input::One), Value::Gain(value)).unwrap();
        let mut hal = RecordingHal {
            hardware: Some(HardwareType::Type1),
            ..RecordingHal::default()
        };
        assert_eq!(
            hal.apply(Command::Set(gain(31), Verification::Unverified)),
            Err(HalError::Zenjack(ZenjackError::InvalidValue))
        );
        assert!(
            hal.apply(Command::Set(gain(10), Verification::Unverified))
                .is_ok()
        );
        assert_eq!(hal.commands.len(), 1);

        let ground = Setting::new(
            Control::ReturnGroundLift(zenjack::Return::One),
            Value::GroundLift(GroundLift::Lifted),
        )
        .unwrap();
        let mut hal = RecordingHal {
            hardware: Some(HardwareType::Type2),
            ..RecordingHal::default()
        };
        assert!(matches!(
            hal.apply(Command::Set(ground, Verification::Unverified)),
            Err(HalError::Zenjack(ZenjackError::Unsupported { .. }))
        ));
        assert!(hal.commands.is_empty());
    }
}

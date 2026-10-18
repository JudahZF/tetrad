//! `ZenHAL::CZenjack`: analog I/O settings, jack detection, expression pedals,
//! power control and board identity. Pure routing and word codecs; the verified
//! write loop and caches live in [`crate::hal::StockHal`].
//!
//! Every setting goes to `/dev/zjack/soundcard` (zenjack, `0x4004e0xx`) or the ES
//! codec (`0x4004e2xx`), chosen by `_hwType` ([`HardwareType`]).
use crate::analog::{GainControl, GainEndpoint, HardwareType, Verification};
use crate::codec::Impedance;
use crate::input::JackRecord;
use crate::node::{Node, Pedal};
use crate::platform::Platform;
use crate::requests::{IoctlPayload, IoctlRequest};
use std::num::NonZeroU32;

/// `input_nbr_t` 0/1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Input {
    One,
    Two,
}

/// `ret_nbr_t` 0/1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Return {
    One,
    Two,
}

/// `xlr_out_nbr_t` 0/1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum XlrOut {
    One,
    Two,
}

/// `input_type_t` 0..=2: TS instrument, XLR, XLR with 48 V (ZenUI phantom wrapper
/// 0x00511944 sends `on ? 2 : 1`). On `_hwType` 2 only `XlrPhantom` changes
/// hardware: it sets the codec 48 V flag (`SetInputType` VA 0x9a10, 1->0, 2->1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputType {
    Instrument = 0,
    Xlr = 1,
    XlrPhantom = 2,
}

/// `hp_mode_t` 0..=2 (`SetHeadPhoneMode` VA 0xaec0). ZenUI never calls it; names are unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HeadphoneMode {
    Mode0 = 0,
    Mode1 = 1,
    Mode2 = 2,
}

/// `midi_type_t` 0/1 (`SetMIDIType` VA 0xb450). ZenUI sends `passthru ^ 1`
/// (0x00330478): 0 = MIDI THRU, 1 = MIDI OUT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MidiType {
    Thru = 0,
    Out = 1,
}

/// `nc_out_sel_t` 0/1 (`SetNCOutSel` VA 0xc470). ZenUI sets 1 for Neural Capture
/// setup and 0 on abort/teardown (0x00202fd8); the routing meaning is inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NcOutput {
    Normal = 0,
    Capture = 1,
}

/// `groundlift_status_t` 0/1. Both drivers take the inverted word: status 0 -> 1,
/// status 1 -> 0 (`SetGroundLiftStatus` VA 0xb6d4, `GetGroundLiftStatus` VA 0xb7a0).
/// ZenUI sends status `setting <= 0.5`, so a lift switched on is status 0 (names inferred).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroundLift {
    Lifted = 0,
    Grounded = 1,
}

/// Headphone level: 64 steps or mute. Word 100 means mute (`SetHeadPhoneVolume` VA 0xb158).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HeadphoneVolume {
    Level(u8),
    Muted,
}

impl HeadphoneVolume {
    pub const MAX_LEVEL: u8 = 63;
    const MUTE_WORD: i32 = 100;

    /// Stock float API: `(int)(level * 63.0f)` for 0.0..=1.0, truncating toward zero.
    pub fn from_unit(level: f32) -> Result<Self, ZenjackError> {
        if !(0.0..=1.0).contains(&level) {
            return Err(ZenjackError::InvalidValue);
        }
        Ok(Self::Level((level * 63.0) as u8))
    }

    /// Stock `GetHeadPhoneVolume` float: `word / 63.0f` (VA 0xb3f4); mute reads as 0.0.
    pub fn unit(self) -> f32 {
        match self {
            Self::Level(level) => f32::from(level) / 63.0,
            Self::Muted => 0.0,
        }
    }
}

/// A setting with a stock `Set*`/`Get*` pair and a `_cache` slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Control {
    InputType(Input),
    InputGain(Input),
    ReturnGain(Return),
    InputImpedance(Input),
    HeadphoneMode,
    HeadphoneVolume,
    MidiType,
    NcOutput,
    InputGroundLift(Input),
    ReturnGroundLift(Return),
    XlrOutGroundLift(XlrOut),
}

/// A decoded value of one [`Control`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Value {
    InputType(InputType),
    /// Integer PGA code, `0..=HardwareType::maximum_gain()` when written.
    Gain(i32),
    Impedance(Impedance),
    HeadphoneMode(HeadphoneMode),
    HeadphoneVolume(HeadphoneVolume),
    MidiType(MidiType),
    NcOutput(NcOutput),
    GroundLift(GroundLift),
}

/// One typed write. `Setting::new` rejects a value of the wrong kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Setting {
    control: Control,
    value: Value,
}

impl Setting {
    pub fn new(control: Control, value: Value) -> Result<Self, ZenjackError> {
        let fits = matches!(
            (control, value),
            (Control::InputType(_), Value::InputType(_))
                | (
                    Control::InputGain(_) | Control::ReturnGain(_),
                    Value::Gain(_)
                )
                | (Control::InputImpedance(_), Value::Impedance(_))
                | (Control::HeadphoneMode, Value::HeadphoneMode(_))
                | (Control::HeadphoneVolume, Value::HeadphoneVolume(_))
                | (Control::MidiType, Value::MidiType(_))
                | (Control::NcOutput, Value::NcOutput(_))
                | (
                    Control::InputGroundLift(_)
                        | Control::ReturnGroundLift(_)
                        | Control::XlrOutGroundLift(_),
                    Value::GroundLift(_)
                )
        );
        if fits {
            Ok(Self { control, value })
        } else {
            Err(ZenjackError::WrongValueKind)
        }
    }

    pub fn control(self) -> Control {
        self.control
    }

    pub fn value(self) -> Value {
        self.value
    }
}

/// One ioctl target: node plus request number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub node: Node,
    pub number: u32,
}

const fn soundcard(low: u32) -> Endpoint {
    Endpoint {
        node: Node::Soundcard,
        number: 0x4004_0000 | low,
    }
}

const fn codec(low: u32) -> Endpoint {
    Endpoint {
        node: Node::Codec,
        number: 0x4004_0000 | low,
    }
}

/// Where a setting is written, read back while verifying, and read when observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub write: Endpoint,
    pub verify: Endpoint,
    pub observe: Endpoint,
}

impl Route {
    const fn same(write: Endpoint, read: Endpoint) -> Self {
        Self {
            write,
            verify: read,
            observe: read,
        }
    }
}

/// `InputImpedance` on `_hwType` 1 writes zenjack first, then the codec
/// (`SetInputImpedance` VA 0xa878).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImpedanceRoute {
    pub zenjack: Route,
    pub codec: Option<Route>,
}

impl Control {
    /// Stock routing per `_hwType`. Deliberate deviations: stock observed ground-lift
    /// getters for returns and XLR outputs, and the XLR 1 verify read on `_hwType` 2,
    /// issue SET numbers on an uninitialized word (VA 0xbf08, 0xc364, 0xc048, 0xc224). Rust uses
    /// the GET numbers that the other verify paths and the codec ABI prove.
    pub fn route(self, hw: HardwareType) -> Result<Route, ZenjackError> {
        use HardwareType::{Type0, Type1, Type2};
        let unsupported = Err(ZenjackError::Unsupported { control: self, hw });
        Ok(match (self, hw) {
            (Self::InputType(Input::One), Type0 | Type1) => {
                Route::same(soundcard(0xe001), soundcard(0xe013))
            }
            (Self::InputType(Input::Two), Type0 | Type1) => {
                Route::same(soundcard(0xe004), soundcard(0xe016))
            }
            (Self::InputType(Input::One), Type2) => return unsupported,
            // Stock still observes `_hwType` 2 through zenjack (`GetInputType` VA 0x9da8).
            (Self::InputType(Input::Two), Type2) => Route {
                write: codec(0xe209),
                verify: codec(0xe211),
                observe: soundcard(0xe016),
            },
            (Self::InputGain(input), _) => gain_route(hw, input.into()),
            (Self::ReturnGain(ret), _) => gain_route(hw, ret.into()),
            (Self::InputImpedance(input), _) => self.impedance_route(hw, input)?.zenjack,
            (Self::HeadphoneMode, _) => Route::same(soundcard(0xe006), soundcard(0xe018)),
            (Self::HeadphoneVolume, _) => Route::same(soundcard(0xe007), soundcard(0xe019)),
            (Self::MidiType, _) => Route::same(soundcard(0xe008), soundcard(0xe01a)),
            (Self::NcOutput, _) => Route::same(soundcard(0xe011), soundcard(0xe023)),
            (Self::InputGroundLift(Input::One), Type2) => return unsupported,
            (Self::InputGroundLift(Input::One), _) => {
                Route::same(soundcard(0xe00d), soundcard(0xe01f))
            }
            (Self::InputGroundLift(Input::Two), Type2) => Route::same(codec(0xe208), codec(0xe210)),
            (Self::InputGroundLift(Input::Two), _) => {
                Route::same(soundcard(0xe00e), soundcard(0xe020))
            }
            (Self::ReturnGroundLift(_), Type2) => return unsupported,
            (Self::ReturnGroundLift(Return::One), _) => {
                Route::same(soundcard(0xe010), soundcard(0xe022))
            }
            (Self::ReturnGroundLift(Return::Two), _) => {
                Route::same(soundcard(0xe00f), soundcard(0xe021))
            }
            (Self::XlrOutGroundLift(XlrOut::One), Type2) => {
                Route::same(codec(0xe206), codec(0xe20e))
            }
            (Self::XlrOutGroundLift(XlrOut::Two), Type2) => {
                Route::same(codec(0xe207), codec(0xe20f))
            }
            (Self::XlrOutGroundLift(XlrOut::One), _) => {
                Route::same(soundcard(0xe00b), soundcard(0xe01d))
            }
            (Self::XlrOutGroundLift(XlrOut::Two), _) => {
                Route::same(soundcard(0xe00c), soundcard(0xe01e))
            }
        })
    }

    /// Stock `_hwType` 2 logs "No support to Impedance Control" and returns true
    /// without an ioctl; Rust reports `Unsupported` instead.
    pub fn impedance_route(
        self,
        hw: HardwareType,
        input: Input,
    ) -> Result<ImpedanceRoute, ZenjackError> {
        let (zj_set, zj_get, es_set, es_get) = match input {
            Input::One => (0xe002, 0xe014, 0xe200, 0xe212),
            Input::Two => (0xe005, 0xe017, 0xe201, 0xe213),
        };
        let zenjack = Route::same(soundcard(zj_set), soundcard(zj_get));
        match hw {
            HardwareType::Type0 => Ok(ImpedanceRoute {
                zenjack,
                codec: None,
            }),
            HardwareType::Type1 => Ok(ImpedanceRoute {
                zenjack,
                codec: Some(Route::same(codec(es_set), codec(es_get))),
            }),
            HardwareType::Type2 => Err(ZenjackError::Unsupported { control: self, hw }),
        }
    }

    /// Range-checks the value for this board and returns the written word.
    pub fn encode(self, hw: HardwareType, value: Value) -> Result<i32, ZenjackError> {
        Setting::new(self, value)?;
        Ok(match value {
            Value::InputType(kind) if hw == HardwareType::Type2 => {
                i32::from(kind == InputType::XlrPhantom)
            }
            Value::InputType(kind) => kind as i32,
            Value::Gain(gain) => {
                if !(0..=hw.maximum_gain()).contains(&gain) {
                    return Err(ZenjackError::InvalidValue);
                }
                gain
            }
            Value::Impedance(z) => i32::from(z.index()),
            Value::HeadphoneMode(mode) => mode as i32,
            Value::HeadphoneVolume(HeadphoneVolume::Muted) => HeadphoneVolume::MUTE_WORD,
            Value::HeadphoneVolume(HeadphoneVolume::Level(level)) => {
                if level > HeadphoneVolume::MAX_LEVEL {
                    return Err(ZenjackError::InvalidValue);
                }
                i32::from(level)
            }
            Value::MidiType(kind) => kind as i32,
            Value::NcOutput(sel) => sel as i32,
            Value::GroundLift(GroundLift::Lifted) => 1,
            Value::GroundLift(GroundLift::Grounded) => 0,
        })
    }

    /// Decodes an observed getter word. Words outside the stock getter's accepted
    /// set are errors (stock returns false and leaves the output unchanged).
    pub fn decode(self, word: i32) -> Result<Value, ZenjackError> {
        let unexpected = Err(ZenjackError::UnexpectedWord {
            control: self,
            word,
        });
        Ok(match self {
            Self::InputType(_) => Value::InputType(match word {
                0 => InputType::Instrument,
                1 => InputType::Xlr,
                2 => InputType::XlrPhantom,
                _ => return unexpected,
            }),
            // Stock applies no range check to observed gains.
            Self::InputGain(_) | Self::ReturnGain(_) => Value::Gain(word),
            Self::InputImpedance(_) => match u8::try_from(word).map(Impedance::try_from) {
                Ok(Ok(z)) => Value::Impedance(z),
                _ => return unexpected,
            },
            Self::HeadphoneMode => Value::HeadphoneMode(match word {
                0 => HeadphoneMode::Mode0,
                1 => HeadphoneMode::Mode1,
                2 => HeadphoneMode::Mode2,
                _ => return unexpected,
            }),
            Self::HeadphoneVolume => Value::HeadphoneVolume(match word {
                HeadphoneVolume::MUTE_WORD => HeadphoneVolume::Muted,
                0..=63 => HeadphoneVolume::Level(word as u8),
                _ => return unexpected,
            }),
            Self::MidiType => Value::MidiType(match word {
                0 => MidiType::Thru,
                1 => MidiType::Out,
                _ => return unexpected,
            }),
            Self::NcOutput => Value::NcOutput(match word {
                0 => NcOutput::Normal,
                1 => NcOutput::Capture,
                _ => return unexpected,
            }),
            Self::InputGroundLift(_) | Self::ReturnGroundLift(_) | Self::XlrOutGroundLift(_) => {
                Value::GroundLift(match word {
                    0 => GroundLift::Grounded,
                    1 => GroundLift::Lifted,
                    _ => return unexpected,
                })
            }
        })
    }
}

impl From<Input> for GainEndpoint {
    fn from(input: Input) -> Self {
        match input {
            Input::One => Self::Input1,
            Input::Two => Self::Input2,
        }
    }
}

impl From<Return> for GainEndpoint {
    fn from(ret: Return) -> Self {
        match ret {
            Return::One => Self::Return1,
            Return::Two => Self::Return2,
        }
    }
}

/// The proved PGA table in [`crate::analog`], including the `_hwType` 1 return swap.
fn gain_route(hw: HardwareType, endpoint: GainEndpoint) -> Route {
    let control = GainControl::new(hw, endpoint);
    let node = match hw {
        HardwareType::Type0 => Node::Soundcard,
        HardwareType::Type1 | HardwareType::Type2 => Node::Codec,
    };
    let write = control
        .write_request(0)
        .expect("gain 0 is valid on every hardware type");
    let read = Endpoint {
        node,
        number: control.observed_request().number,
    };
    Route::same(
        Endpoint {
            node,
            number: write.request.number,
        },
        read,
    )
}

/// Boost targets. Stock has no verify loop, cache or value check for boosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Boost {
    Input(Input),
    Return(Return),
}

impl Boost {
    /// (write, read) on zenjack. Input boost needs `_hwType` 0/1 (`SetInputBoost`
    /// VA 0xa2d0); return boost needs `_hwType` 1 and swaps returns (VA 0xa740).
    pub fn route(self, hw: HardwareType) -> Result<(Endpoint, Endpoint), ZenjackError> {
        match (self, hw) {
            (Self::Input(Input::One), HardwareType::Type0 | HardwareType::Type1) => {
                Ok((soundcard(0xe02e), soundcard(0xe032)))
            }
            (Self::Input(Input::Two), HardwareType::Type0 | HardwareType::Type1) => {
                Ok((soundcard(0xe02f), soundcard(0xe033)))
            }
            (Self::Return(Return::One), HardwareType::Type1) => {
                Ok((soundcard(0xe031), soundcard(0xe035)))
            }
            (Self::Return(Return::Two), HardwareType::Type1) => {
                Ok((soundcard(0xe030), soundcard(0xe034)))
            }
            _ => Err(ZenjackError::UnsupportedBoost { boost: self, hw }),
        }
    }
}

/// Power-button sensitivity level 0..=3. Stock passes a pointer into the `.data`
/// table `_power_button_sensitivity` (VA 0x18688) without a bounds check (VA 0xd5dc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PowerButtonSensitivity(u8);

impl PowerButtonSensitivity {
    const TABLE: [i32; 4] = [255, 127, 34, 17];

    pub fn level(self) -> u8 {
        self.0
    }

    pub fn word(self) -> i32 {
        Self::TABLE[usize::from(self.0)]
    }
}

impl TryFrom<i32> for PowerButtonSensitivity {
    type Error = ZenjackError;

    fn try_from(level: i32) -> Result<Self, Self::Error> {
        match u8::try_from(level) {
            Ok(level @ 0..=3) => Ok(Self(level)),
            _ => Err(ZenjackError::InvalidValue),
        }
    }
}

/// Zenjack power requests. Every one is a single word on `/dev/zjack/soundcard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PowerCommand {
    /// `Restart` VA 0xcb7c: word 0.
    Restart,
    /// `Shutdown` VA 0xcbe0: word 0.
    Shutdown,
    /// `SetPowerModeAutoOn` VA 0xcc44.
    AutoOn(bool),
    /// `SetPowerButtonSensitivity` VA 0xd5dc.
    ButtonSensitivity(PowerButtonSensitivity),
}

impl PowerCommand {
    pub fn request(self) -> IoctlRequest {
        match self {
            Self::Restart => IoctlRequest::word(number::RESTART, 0),
            Self::Shutdown => IoctlRequest::word(number::SHUTDOWN, 0),
            Self::AutoOn(on) => IoctlRequest::word(number::AUTO_ON, i32::from(on)),
            Self::ButtonSensitivity(level) => {
                IoctlRequest::word(number::BUTTON_SENSITIVITY, level.word())
            }
        }
    }
}

/// Zenjack request numbers outside the setting routes.
pub mod number {
    /// `InitialInputStatus` VA 0xcae4: 16 output bytes, the jack record layout.
    pub const INITIAL_INPUT_STATUS: u32 = 0x4010_e024;
    pub const RESTART: u32 = 0x4004_e025;
    /// `AppFwVersion` VA 0xccc8.
    pub const APP_FIRMWARE: u32 = 0x4004_e026;
    /// `BootloaderFwVersion` VA 0xcdd4.
    pub const BOOTLOADER_FIRMWARE: u32 = 0x4004_e027;
    pub const SHUTDOWN: u32 = 0x4004_e028;
    pub const BUTTON_SENSITIVITY: u32 = 0x4004_e029;
    /// `GetExprIoctl` VA 0xc6f4: initial pedal position, once per process.
    pub const EXPRESSION_1: u32 = 0x4004_e02a;
    pub const EXPRESSION_2: u32 = 0x4004_e02b;
    /// `BoarRevisionGet` VA 0xcee0.
    pub const BOARD_REVISION: u32 = 0x4004_e02c;
    pub const AUTO_ON: u32 = 0x4004_e036;
    /// `BoardUniqueID` VA 0xd4c8: 16 output bytes.
    pub const UNIQUE_ID: u32 = 0x4010_e037;
}

/// Initial-value ioctl for one pedal. The output word starts at zero (VA 0xc700).
pub fn expression_request(pedal: Pedal) -> IoctlRequest {
    let number = match pedal {
        Pedal::One => number::EXPRESSION_1,
        Pedal::Two => number::EXPRESSION_2,
    };
    IoctlRequest::word(number, 0)
}

/// Sixteen zeroed output bytes (unique ID, initial jack status).
pub fn block_request(number: u32) -> IoctlRequest {
    IoctlRequest {
        number,
        payload: IoctlPayload::Block([0; 16]),
    }
}

/// Jack detection masks from `InputStatus` (VA 0xc934) or `InitialInputStatus`
/// (VA 0xcae4). Stock names (from its simulation globals): `changed`, `plugged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct JackStatus {
    pub changed: u16,
    pub plugged: u16,
}

impl From<JackRecord> for JackStatus {
    fn from(record: JackRecord) -> Self {
        let [changed, plugged] = record.status_words();
        Self { changed, plugged }
    }
}

impl HardwareType {
    /// `CZenjack::Open` (VA 0xd2dc..0xd324): revision 2 -> Type1; revision 3 or
    /// product `atma` -> Type2; otherwise the `.bss` zero value, Type0. A failed
    /// revision ioctl is stock value 4 and falls through to the product test.
    pub fn detect(zenjack_revision: Option<i32>, atma: bool) -> Self {
        match zenjack_revision {
            Some(2) => Self::Type1,
            Some(3) => Self::Type2,
            _ if atma => Self::Type2,
            _ => Self::Type0,
        }
    }
}

/// The verify policy ZenUI's I/O page initialization uses (0x0035b068): 3 attempts,
/// 25 ms settle. Every other ZenUI setter call is `Unverified` (attempts/sleep unused).
pub const ZENUI_VERIFY: Verification = Verification::ReadBack {
    attempts: NonZeroU32::new(3).unwrap(),
    sleep_microseconds: NonZeroU32::new(25_000),
};

impl Impedance {
    /// Input impedance in ohms for index 0..=8 (ZenUI list: 10k .. 10M).
    pub fn ohms(self) -> u32 {
        const OHMS: [u32; 9] = [
            10_000, 33_000, 68_000, 91_000, 130_000, 232_000, 470_000, 1_000_000, 10_000_000,
        ];
        OHMS[usize::from(self.index())]
    }

    /// ZenUI setting value 0.0..=1.0 to index: `lround(v * 8)`.
    pub fn from_unit(value: f32) -> Result<Self, ZenjackError> {
        if !(0.0..=1.0).contains(&value) {
            return Err(ZenjackError::InvalidValue);
        }
        Impedance::try_from((value * 8.0).round() as u8).map_err(|_| ZenjackError::InvalidValue)
    }
}

/// ZenUI analog gain split (scheduler 0x007d65c4 / 0x007d6ec4, all f32 with fused
/// multiply-add): a PGA step, the ESS boost flag, and the linear digital trim that
/// ZenUI sends to the DSP for the remainder.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainPlan {
    pub decibels: f32,
    /// PGA code for `InputGain`/`ReturnGain`, after the boost split.
    pub step: i32,
    /// `Some` only on `Platform::QcEss`, where ZenUI always calls the boost setter.
    pub boost: Option<bool>,
    /// `exp10f((dB - analog) * 0.05)`. Rust uses `powf`; it can differ by one ulp.
    pub trim: f32,
}

impl GainPlan {
    /// `value` is the ZenUI gain setting 0.0..=1.0: dB = v*72-12 on QC, v*42-12 on ATMA.
    pub fn from_unit(platform: Platform, value: f32) -> Result<Self, ZenjackError> {
        if !value.is_finite() {
            return Err(ZenjackError::InvalidValue);
        }
        let span = if platform.is_qc() { 72.0 } else { 42.0 };
        let db = value.mul_add(span, -12.0f32);
        let (step, analog) = if !platform.has_ess_codec() {
            if db < 9.0 {
                (0, 0.0)
            } else {
                let step = if db >= 60.0 {
                    18
                } else {
                    ((db - 9.0) / 3.0) as i32 + 1
                };
                (step, (step as f32).mul_add(3.0, 6.0))
            }
        } else if db < 0.0 {
            (0, 0.0)
        } else {
            let step = (db / 3.0) as i32;
            (step, step as f32 * 3.0)
        };
        let trim = 10f32.powf((db - analog) * 0.05);
        let (step, boost) = match platform {
            Platform::QcEss if step > 10 => (step - 10, Some(true)),
            Platform::QcEss => (step, Some(false)),
            _ => (step, None),
        };
        Ok(Self {
            decibels: db,
            step,
            boost,
            trim,
        })
    }
}

/// One jack-detection bit (ZenUI PluggedController maps, `_INIT_552`). Port names
/// for inputs 1/2 and returns follow the gain channel ids; outputs 6..9 are unnamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Jack {
    /// Bits 0 and 2 (both switches of the combo jack, inferred).
    Input1,
    /// Bits 1 and 3.
    Input2,
    Return1,
    Return2,
    Output6,
    Output7,
    Output8,
    Output9,
    Expression1,
    Expression2,
    Headphones,
}

impl Jack {
    pub const ALL: [Self; 11] = [
        Self::Input1,
        Self::Input2,
        Self::Return1,
        Self::Return2,
        Self::Output6,
        Self::Output7,
        Self::Output8,
        Self::Output9,
        Self::Expression1,
        Self::Expression2,
        Self::Headphones,
    ];

    pub fn mask(self) -> u16 {
        match self {
            Self::Input1 => 0x0005,
            Self::Input2 => 0x000a,
            Self::Return1 => 1 << 8,
            Self::Return2 => 1 << 9,
            Self::Output8 => 1 << 4,
            Self::Output9 => 1 << 5,
            Self::Output6 => 1 << 10,
            Self::Output7 => 1 << 11,
            Self::Expression1 => 1 << 7,
            Self::Expression2 => 1 << 6,
            Self::Headphones => 1 << 12,
        }
    }
}

impl JackStatus {
    pub fn changed(self, jack: Jack) -> bool {
        self.changed & jack.mask() != 0
    }

    pub fn plugged(self, jack: Jack) -> bool {
        self.plugged & jack.mask() != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ZenjackError {
    #[error("{control:?} is not supported on {hw:?}")]
    Unsupported { control: Control, hw: HardwareType },
    #[error("{boost:?} is not supported on {hw:?}")]
    UnsupportedBoost { boost: Boost, hw: HardwareType },
    #[error("value kind does not match the control")]
    WrongValueKind,
    #[error("value is outside the stock range")]
    InvalidValue,
    #[error("{control:?} getter returned word {word}")]
    UnexpectedWord { control: Control, word: i32 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use HardwareType::{Type0, Type1, Type2};

    fn numbers(control: Control, hw: HardwareType) -> (Node, u32, u32, u32) {
        let r = control.route(hw).unwrap();
        (
            r.write.node,
            r.write.number,
            r.verify.number,
            r.observe.number,
        )
    }

    /// Exact (write, verify, observe) numbers from the setter/getter VAs.
    #[test]
    fn routes_match_stock_per_hardware_type() {
        use Control::*;
        let sc = Node::Soundcard;
        let es = Node::Codec;
        let cases = [
            (
                InputType(Input::One),
                Type0,
                (sc, 0x4004_e001, 0x4004_e013, 0x4004_e013),
            ),
            (
                InputType(Input::Two),
                Type1,
                (sc, 0x4004_e004, 0x4004_e016, 0x4004_e016),
            ),
            (
                InputType(Input::Two),
                Type2,
                (es, 0x4004_e209, 0x4004_e211, 0x4004_e016),
            ),
            (
                ReturnGain(Return::One),
                Type1,
                (es, 0x4004_e205, 0x4004_e20d, 0x4004_e20d),
            ),
            (
                ReturnGain(Return::One),
                Type2,
                (es, 0x4004_e204, 0x4004_e20c, 0x4004_e20c),
            ),
            (
                InputGain(Input::Two),
                Type0,
                (sc, 0x4004_e003, 0x4004_e015, 0x4004_e015),
            ),
            (
                InputImpedance(Input::Two),
                Type1,
                (sc, 0x4004_e005, 0x4004_e017, 0x4004_e017),
            ),
            (
                HeadphoneVolume,
                Type2,
                (sc, 0x4004_e007, 0x4004_e019, 0x4004_e019),
            ),
            (NcOutput, Type0, (sc, 0x4004_e011, 0x4004_e023, 0x4004_e023)),
            (
                InputGroundLift(Input::Two),
                Type2,
                (es, 0x4004_e208, 0x4004_e210, 0x4004_e210),
            ),
            (
                ReturnGroundLift(Return::One),
                Type1,
                (sc, 0x4004_e010, 0x4004_e022, 0x4004_e022),
            ),
            (
                XlrOutGroundLift(XlrOut::One),
                Type2,
                (es, 0x4004_e206, 0x4004_e20e, 0x4004_e20e),
            ),
            (
                XlrOutGroundLift(XlrOut::Two),
                Type0,
                (sc, 0x4004_e00c, 0x4004_e01e, 0x4004_e01e),
            ),
        ];
        for (control, hw, expected) in cases {
            assert_eq!(numbers(control, hw), expected, "{control:?} {hw:?}");
        }
        for control in [
            InputType(Input::One),
            InputImpedance(Input::One),
            InputGroundLift(Input::One),
            ReturnGroundLift(Return::Two),
        ] {
            assert!(matches!(
                control.route(Type2),
                Err(ZenjackError::Unsupported { .. })
            ));
        }
        assert_eq!(
            Boost::Return(Return::One).route(Type1).unwrap().0.number,
            0x4004_e031
        );
        assert!(Boost::Return(Return::One).route(Type0).is_err());
        assert!(Boost::Input(Input::One).route(Type2).is_err());
    }

    #[test]
    fn words_follow_stock_encodings() {
        let lift = Control::XlrOutGroundLift(XlrOut::One);
        assert_eq!(
            lift.encode(Type0, Value::GroundLift(GroundLift::Lifted)),
            Ok(1)
        );
        assert_eq!(lift.decode(0), Ok(Value::GroundLift(GroundLift::Grounded)));
        assert!(lift.decode(2).is_err());
        let ty = Control::InputType(Input::Two);
        let phantom = Value::InputType(InputType::XlrPhantom);
        assert_eq!(ty.encode(Type2, Value::InputType(InputType::Xlr)), Ok(0));
        assert_eq!(ty.encode(Type2, phantom), Ok(1));
        assert_eq!(ty.encode(Type1, phantom), Ok(2));
        let hp = Control::HeadphoneVolume;
        assert_eq!(
            hp.encode(Type0, Value::HeadphoneVolume(HeadphoneVolume::Muted)),
            Ok(100)
        );
        assert_eq!(
            hp.decode(100),
            Ok(Value::HeadphoneVolume(HeadphoneVolume::Muted))
        );
        assert!(hp.decode(64).is_err());
        assert_eq!(
            HeadphoneVolume::from_unit(0.5),
            Ok(HeadphoneVolume::Level(31))
        );
        assert_eq!(
            HeadphoneVolume::from_unit(1.0),
            Ok(HeadphoneVolume::Level(63))
        );
        assert!(HeadphoneVolume::from_unit(f32::NAN).is_err());
        let gain = Control::InputGain(Input::One);
        assert_eq!(gain.encode(Type0, Value::Gain(31)), Ok(31));
        assert!(gain.encode(Type1, Value::Gain(11)).is_err());
        assert_eq!(
            gain.encode(Type1, Value::MidiType(MidiType::Thru)),
            Err(ZenjackError::WrongValueKind)
        );
        assert_eq!(
            PowerCommand::ButtonSensitivity(PowerButtonSensitivity::try_from(2).unwrap()).request(),
            IoctlRequest::word(0x4004_e029, 34)
        );
        assert!(PowerButtonSensitivity::try_from(4).is_err());
    }

    /// ZenUI gain split: f32 fma dB, PGA step per codec family, ESS boost above 10.
    #[test]
    fn gain_plan_matches_zenui_scheduler() {
        let plan = |platform, v| {
            let p = GainPlan::from_unit(platform, v).unwrap();
            (p.step, p.boost, p.trim)
        };
        assert_eq!(plan(Platform::QcOriginal, 0.5), (6, None, 1.0));
        assert_eq!(plan(Platform::QcOriginal, 1.0), (18, None, 1.0));
        let (step, boost, trim) = plan(Platform::QcOriginal, 0.0);
        assert_eq!((step, boost), (0, None));
        assert!((trim - 0.251_188_6).abs() < 1e-6);
        assert_eq!(plan(Platform::QcEss, 0.5), (8, Some(false), 1.0));
        assert_eq!(plan(Platform::QcEss, 1.0), (10, Some(true), 1.0));
        assert_eq!(plan(Platform::Atma, 1.0), (10, None, 1.0));
        assert!(GainPlan::from_unit(Platform::Atma, f32::NAN).is_err());
        let z = Impedance::from_unit(0.875).unwrap();
        assert_eq!((z.index(), z.ohms()), (7, 1_000_000));
    }

    #[test]
    fn jack_bits_follow_plugged_controller_map() {
        let status = JackStatus {
            changed: 0x0004,
            plugged: 0x1080,
        };
        assert!(status.changed(Jack::Input1));
        assert!(!status.plugged(Jack::Input1));
        assert!(status.plugged(Jack::Expression1));
        assert!(status.plugged(Jack::Headphones));
        let all = Jack::ALL.iter().fold(0, |acc, jack| acc | jack.mask());
        assert_eq!(all, 0x1fff);
    }

    #[test]
    fn hardware_detection_follows_open() {
        assert_eq!(HardwareType::detect(Some(2), true), Type1);
        assert_eq!(HardwareType::detect(Some(3), false), Type2);
        assert_eq!(HardwareType::detect(None, true), Type2);
        assert_eq!(HardwareType::detect(Some(1), false), Type0);
    }
}

//! DSP → ARM responses as consumed by ZenUI `DSPCommsARM`.
//!
//! Two dispatchers exist: VA `0x89d858` for transport rings 0/1 and VA `0x89cc60`
//! for rings 2/3/4 (bound at VA `0x89c848`; reader loops VAs `0x8a4d58` /
//! `0x8a4ec4`). Both switch on byte 0, queue a body that starts at **+8**, and
//! `updateResponses` (VA `0x89ece0`) drains the queues. The 8-byte response
//! header is a separate namespace from the 16-byte request header in `fragment`;
//! never decode an incoming ring payload as a `Fragment`.
//!
//! Known fixed records are decoded only at their exact recovered size. Unknown
//! types keep their raw bytes. Header bytes `+2..4` and the selector `+1` are
//! retained unchanged.

use std::fmt;

use zeroize::Zeroize;

pub use crate::tuner_looper::{LooperStatus, MeterRecord, OpaqueCalibration};
use crate::{CoreId, DspCore, Error, read_u32};

pub const HEADER_BYTES: usize = 8;
/// Stock bulk-meter drain reads `count` floats from a 16-float record without a bound.
pub const BULK_METER_MAX_VALUES: usize = 16;
/// Bulk model-start masks are scanned for bits `0..30` (VA `0x89ece0`).
pub const MODULE_SLOT_COUNT: u32 = 30;
pub const DEBUG_TEXT_BYTES: usize = 68;
pub const SOC2_ARM_DIAGNOSTICS_BYTES: usize = 128;
pub const HID_REPORT_BYTES: usize = crate::hid::REPORT_BYTES;
pub const USB_METER_BYTES: usize = 16;

/// Which stock dispatcher a ring feeds. Logged as `DComms SOC %u`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dispatcher {
    Soc1,
    Soc2,
}

impl Dispatcher {
    pub const fn for_ring(ring: CoreId) -> Self {
        match ring {
            CoreId::Dsp(DspCore::Core0 | DspCore::Core1) => Self::Soc1,
            CoreId::Dsp(DspCore::Core2 | DspCore::Core3) | CoreId::Control => Self::Soc2,
        }
    }

    pub const fn number(self) -> u8 {
        match self {
            Self::Soc1 => 1,
            Self::Soc2 => 2,
        }
    }

    /// Type `0x00`: selector 0 names the SOC's first core, anything else the second.
    pub const fn diagnostics_core(self, selector: u8) -> DspCore {
        match (self, selector) {
            (Self::Soc1, 0) => DspCore::Core0,
            (Self::Soc1, _) => DspCore::Core1,
            (Self::Soc2, 0) => DspCore::Core2,
            (Self::Soc2, _) => DspCore::Core3,
        }
    }

    /// Type `0x15`: SOC1 tests selector 0, SOC2 tests selector 2.
    pub const fn self_test_core(self, selector: u8) -> DspCore {
        match (self, selector) {
            (Self::Soc1, 0) => DspCore::Core0,
            (Self::Soc1, _) => DspCore::Core1,
            (Self::Soc2, 2) => DspCore::Core2,
            (Self::Soc2, _) => DspCore::Core3,
        }
    }

    /// Type `0x1c` is the run-list-cleared acknowledgement that follows
    /// `DeleteAllModels`: body word 0 names the SOC's first core when zero
    /// (flags `0x1562d74..80`, read by comms vtable `+0x58`, VA 0x89b508).
    pub const fn run_list_core(self, word: u32) -> DspCore {
        match (self, word) {
            (Self::Soc1, 0) => DspCore::Core0,
            (Self::Soc1, _) => DspCore::Core1,
            (Self::Soc2, 0) => DspCore::Core2,
            (Self::Soc2, _) => DspCore::Core3,
        }
    }

    /// Type `0x0d`: SOC2 silently drops any selector other than 2.
    pub const fn accepts_bulk_meter(self, selector: u8) -> bool {
        match self {
            Self::Soc1 => true,
            Self::Soc2 => selector == 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handling {
    Soc1Only,
    Soc2Only,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyLayout {
    Fixed(usize),
    /// Body is `[8, total)`; the drain copies the bytes into a vector.
    Variable,
    /// The dispatcher only raises a flag; any body is ignored.
    FlagOnly,
}

/// Names come from the Qt signal each type feeds (`zenUI::IDSPComms`, meta
/// object VA `0x1260a10`; `zenUI::DSPCommsARM`, VA `0x1228030`) or from stock
/// log text. `Flag0f`, `Word1c` and `Vector24/25/27` have no recovered consumer name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ResponseType {
    Diagnostics = 0x00,
    Meter = 0x01,
    BufferResponse = 0x03,
    ModelStart = 0x04,
    SlaveBootBufferAddress = 0x05,
    SlaveBootResponse = 0x06,
    MidiFromType07 = 0x07,
    Soc2ArmDiagnostics = 0x08,
    DebugText = 0x0a,
    ModuleLatency = 0x0b,
    BulkModelStart = 0x0c,
    BulkMeter = 0x0d,
    MidiClockTempo = 0x0e,
    Flag0f = 0x0f,
    HidReport = 0x10,
    /// Cover name: the 16-byte cloud signing material, answer to request `0x35`.
    UsbMeterUpdate = 0x11,
    LooperStatus = 0x12,
    SelfTest = 0x15,
    UsbMeterInput = 0x17,
    /// Cover name: the 16-byte local backup stamp key, answer to request `0x43`.
    UsbMeterOutput = 0x18,
    MetronomeStatus = 0x19,
    Soc2SoundsReceived = 0x1a,
    Word1c = 0x1c,
    /// Cover name: the 32-byte cloud client id, answer to request `0x4f` (w4 §5).
    TunerCalibrate = 0x1f,
    TunerTimer = 0x20,
    MidiOutTimer = 0x21,
    StageOneComplete = 0x22,
    StageTwoComplete = 0x23,
    Vector24 = 0x24,
    Vector25 = 0x25,
    MidiFromType26 = 0x26,
    Vector27 = 0x27,
}

impl ResponseType {
    pub const fn layout(self) -> BodyLayout {
        use BodyLayout::{Fixed, FlagOnly, Variable};
        match self {
            Self::Diagnostics => Fixed(64),
            Self::Meter => Fixed(16),
            Self::BufferResponse => Fixed(16),
            Self::ModelStart => Fixed(12),
            Self::SlaveBootBufferAddress => Variable,
            Self::SlaveBootResponse => Fixed(4),
            Self::MidiFromType07 | Self::MidiFromType26 => Fixed(12),
            Self::Soc2ArmDiagnostics => Fixed(SOC2_ARM_DIAGNOSTICS_BYTES),
            Self::DebugText => Fixed(DEBUG_TEXT_BYTES),
            Self::ModuleLatency => Fixed(12),
            Self::BulkModelStart => Fixed(12),
            Self::BulkMeter => Fixed(76),
            Self::MidiClockTempo => Fixed(8),
            Self::Flag0f | Self::Soc2SoundsReceived => FlagOnly,
            Self::HidReport => Fixed(HID_REPORT_BYTES),
            Self::UsbMeterUpdate | Self::UsbMeterOutput => Fixed(USB_METER_BYTES),
            Self::LooperStatus => Fixed(88),
            Self::SelfTest => Fixed(16),
            Self::UsbMeterInput => Fixed(2 * USB_METER_BYTES),
            Self::MetronomeStatus => Fixed(28),
            // The QC sends 12 body bytes (S3 undo, 2026-10-15): word 0 names the core, word 2
            // was 1 in every answer.
            Self::Word1c => Fixed(12),
            Self::TunerCalibrate => Fixed(32),
            Self::TunerTimer | Self::MidiOutTimer => Variable,
            Self::StageOneComplete => Fixed(4),
            Self::StageTwoComplete => Fixed(8),
            Self::Vector24 | Self::Vector25 | Self::Vector27 => Variable,
        }
    }

    pub const fn handling(self) -> Handling {
        match self {
            Self::Diagnostics
            | Self::Meter
            | Self::BufferResponse
            | Self::ModelStart
            | Self::DebugText
            | Self::ModuleLatency
            | Self::BulkModelStart
            | Self::BulkMeter
            | Self::SelfTest
            | Self::Word1c => Handling::Both,
            Self::SlaveBootBufferAddress
            | Self::SlaveBootResponse
            | Self::Vector24
            | Self::Vector25
            | Self::Vector27 => Handling::Soc1Only,
            _ => Handling::Soc2Only,
        }
    }

    pub const fn is_handled_by(self, dispatcher: Dispatcher) -> bool {
        matches!(
            (self.handling(), dispatcher),
            (Handling::Both, _)
                | (Handling::Soc1Only, Dispatcher::Soc1)
                | (Handling::Soc2Only, Dispatcher::Soc2)
        )
    }
}

impl TryFrom<u8> for ResponseType {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        Ok(match value {
            0x00 => Self::Diagnostics,
            0x01 => Self::Meter,
            0x03 => Self::BufferResponse,
            0x04 => Self::ModelStart,
            0x05 => Self::SlaveBootBufferAddress,
            0x06 => Self::SlaveBootResponse,
            0x07 => Self::MidiFromType07,
            0x08 => Self::Soc2ArmDiagnostics,
            0x0a => Self::DebugText,
            0x0b => Self::ModuleLatency,
            0x0c => Self::BulkModelStart,
            0x0d => Self::BulkMeter,
            0x0e => Self::MidiClockTempo,
            0x0f => Self::Flag0f,
            0x10 => Self::HidReport,
            0x11 => Self::UsbMeterUpdate,
            0x12 => Self::LooperStatus,
            0x15 => Self::SelfTest,
            0x17 => Self::UsbMeterInput,
            0x18 => Self::UsbMeterOutput,
            0x19 => Self::MetronomeStatus,
            0x1a => Self::Soc2SoundsReceived,
            0x1c => Self::Word1c,
            0x1f => Self::TunerCalibrate,
            0x20 => Self::TunerTimer,
            0x21 => Self::MidiOutTimer,
            0x22 => Self::StageOneComplete,
            0x23 => Self::StageTwoComplete,
            0x24 => Self::Vector24,
            0x25 => Self::Vector25,
            0x26 => Self::MidiFromType26,
            0x27 => Self::Vector27,
            _ => return Err(Error::Invalid("DSP response type")),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseHeader {
    pub response_type: u8,
    /// Byte `+1`: a core selector for types `0x00`, `0x0d`, `0x15`; SOC/core tag for `0x0a`.
    pub selector: u8,
    pub reserved: [u8; 2],
    pub total_bytes: u32,
}

impl ResponseHeader {
    pub fn from_bytes(bytes: &[u8; HEADER_BYTES]) -> Self {
        Self {
            response_type: bytes[0],
            selector: bytes[1],
            reserved: [bytes[2], bytes[3]],
            total_bytes: read_u32(bytes, 4),
        }
    }

    pub fn to_bytes(self) -> [u8; HEADER_BYTES] {
        let mut bytes = [
            self.response_type,
            self.selector,
            self.reserved[0],
            self.reserved[1],
            0,
            0,
            0,
            0,
        ];
        bytes[4..].copy_from_slice(&self.total_bytes.to_le_bytes());
        bytes
    }
}

/// `DSPCommsDiagnostics`: 16 LE words. The drain reads word 3 (FPEx count),
/// word 4 (FPEx slot) and word 12 (error flags); the rest stay raw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticsBlock {
    pub words: [u32; 16],
}

impl DiagnosticsBlock {
    pub const fn fpex_count(&self) -> i32 {
        self.words[3] as i32
    }
    pub const fn fpex_slot(&self) -> u32 {
        self.words[4]
    }
    pub const fn error_flags(&self) -> u32 {
        self.words[12]
    }
}

/// `bufferResponseSignal(uint32_t x4)`. Stock logs word 3 as the byte count and
/// word 2 as the address; words 0/1 are unresolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferResponse {
    pub words: [u32; 4],
}

impl BufferResponse {
    pub const fn address(&self) -> u32 {
        self.words[2]
    }
    pub const fn byte_count(&self) -> u32 {
        self.words[3]
    }
}

/// Type `0x04`: zero-based core, slot, status word (zero means failed to start).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelStartRecord {
    pub core: u32,
    pub slot: u32,
    pub status: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotStart {
    Started,
    Failed,
}

/// Type `0x0c`: zero-based core plus started/failed bit masks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkModelStart {
    pub core: u32,
    pub started_mask: u32,
    pub failed_mask: u32,
}

impl BulkModelStart {
    /// Stock scan order: a set started bit wins over a set failed bit; bits `>= 30` are ignored.
    pub fn slots(&self) -> impl Iterator<Item = (u32, SlotStart)> + '_ {
        (0..MODULE_SLOT_COUNT).filter_map(move |slot| {
            let bit = 1_u32 << slot;
            if self.started_mask & bit != 0 {
                Some((slot, SlotStart::Started))
            } else if self.failed_mask & bit != 0 {
                Some((slot, SlotStart::Failed))
            } else {
                None
            }
        })
    }
}

/// Types `0x07` / `0x26`: three raw words routed to the MIDI sequencer or
/// `ZenHAL::CMIDI::RxInsertQueue`. Accessors expose the byte fields the drain
/// extracts; word 2 is passed through unresolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiRecord {
    pub words: [u32; 3],
}

impl MidiRecord {
    pub const fn code(&self) -> u8 {
        (self.words[0] & 0xff) as u8
    }
    pub const fn status(&self) -> u8 {
        ((self.words[0] >> 16) & 0xff) as u8
    }
    pub const fn data1(&self) -> u8 {
        (self.words[0] >> 24) as u8
    }
    pub const fn data2(&self) -> u8 {
        (self.words[1] & 0xff) as u8
    }
}

/// Type `0x0a`: 68 bytes logged with `%s` as `DComms SOC %u Core %u : <time> : %s`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DebugText(pub [u8; DEBUG_TEXT_BYTES]);

impl DebugText {
    /// Bytes before the first NUL, as the stock `%s` would print.
    pub fn text(&self) -> &[u8] {
        let end = self
            .0
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(DEBUG_TEXT_BYTES);
        &self.0[..end]
    }
}

impl fmt::Debug for DebugText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DebugText({:?})", String::from_utf8_lossy(self.text()))
    }
}

/// `dspLatencyResponseSignal(core, DSPCommsModuleLatency)`: word 0 is the core argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleLatency {
    pub words: [u32; 3],
}

/// `dspBulkMeterResponseSignal(uint32_t, uint32_t, std::vector<float>)`.
/// Decoding rejects `count > 16` (stock over-reads its stack record instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkMeter {
    pub first: u32,
    pub second: u32,
    pub count: u32,
    pub value_bits: [u32; BULK_METER_MAX_VALUES],
}

impl BulkMeter {
    pub fn values(&self) -> &[u32] {
        &self.value_bits[..self.count as usize]
    }
}

/// `DSPCommsARM::midiClockTempo(int, uint32_t)`; argument names were not recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiClockTempo {
    pub int_arg: i32,
    pub uint_arg: u32,
}

/// `MetronomeStatus`: seven LE words copied unchanged (`metronomeStatusResponseSignal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetronomeStatus {
    pub words: [u32; 7],
}

/// `DSPCommsSelfTestResponse`: four LE words copied unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelfTest {
    pub words: [u32; 4],
}

/// `SOC2ARMCommsDiagnostics`: 128 opaque bytes. `Debug` hides the contents.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Soc2ArmDiagnostics(pub [u8; SOC2_ARM_DIAGNOSTICS_BYTES]);

/// Type `0x10`: 128 report bytes posted to the Cortex Control HID object
/// (VA `0x6dfb54`, same object as the `0x27` sender at `0x6dfbf8`). `Debug` hides them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HidReport(pub [u8; HID_REPORT_BYTES]);

/// One 16-byte `std::vector<uint8_t>` USB meter payload. Not `Copy`: the bytes are zeroed on
/// drop.
#[derive(Clone, PartialEq, Eq)]
pub struct UsbMeter(pub [u8; USB_METER_BYTES]);

impl Drop for UsbMeter {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Reply `0x11` also carries the cloud request-signing material, so logs never show its bytes.
impl fmt::Debug for UsbMeter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UsbMeter(..)")
    }
}

impl fmt::Debug for Soc2ArmDiagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Soc2ArmDiagnostics(..)")
    }
}

impl fmt::Debug for HidReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HidReport(..)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseBody<'a> {
    Diagnostics(DiagnosticsBlock),
    Meter(MeterRecord),
    BufferResponse(BufferResponse),
    ModelStart(ModelStartRecord),
    SlaveBootBufferAddress(&'a [u8]),
    SlaveBootResponse(u32),
    /// Types `0x07` and `0x26`; the frame's type says which.
    Midi(MidiRecord),
    Soc2ArmDiagnostics(Soc2ArmDiagnostics),
    DebugText(DebugText),
    ModuleLatency(ModuleLatency),
    BulkModelStart(BulkModelStart),
    BulkMeter(BulkMeter),
    MidiClockTempo(MidiClockTempo),
    Flag0f,
    HidReport(HidReport),
    UsbMeterUpdate(UsbMeter),
    LooperStatus(LooperStatus),
    SelfTest(SelfTest),
    UsbMeterInput {
        first: UsbMeter,
        second: UsbMeter,
    },
    UsbMeterOutput(UsbMeter),
    MetronomeStatus(MetronomeStatus),
    Soc2SoundsReceived,
    Word1c(u32),
    /// The cloud client id; [`OpaqueCalibration`] hides it from `Debug`.
    TunerCalibrate(OpaqueCalibration),
    TunerTimer(&'a [u8]),
    MidiOutTimer(&'a [u8]),
    StageOneComplete(u32),
    StageTwoComplete(u32, u32),
    /// ModelRepo key pull (`DSPCommsARM` vtable `+0x60`, stored at controller `+0xf018`).
    Vector24(KeyBytes<'a>),
    /// Blob key pull (vtable `+0x64`, controller `+0xf024`).
    Vector25(KeyBytes<'a>),
    /// Third boot vector (vtable `+0x68`, controller `+0xf04c`); consumer unresolved.
    Vector27(KeyBytes<'a>),
    /// Type byte outside the recovered switch tables; both dispatchers ignore it.
    Unknown(&'a [u8]),
}

/// Boot vectors `0x24`/`0x25`/`0x27`: the DSP delivers the ModelRepo and blob
/// keys here (VAs 0x875394, 0x876598). `Debug` shows only the length.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct KeyBytes<'a>(pub &'a [u8]);

impl fmt::Debug for KeyBytes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyBytes({} bytes)", self.0.len())
    }
}

/// One decoded response over borrowed ring bytes. `Debug` omits the raw bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct ResponseFrame<'a> {
    header: ResponseHeader,
    response_type: Option<ResponseType>,
    body: ResponseBody<'a>,
    bytes: &'a [u8],
}

impl fmt::Debug for ResponseFrame<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseFrame")
            .field("header", &self.header)
            .field("body", &self.body)
            .finish_non_exhaustive()
    }
}

impl<'a> ResponseFrame<'a> {
    /// Owned rules: known types need `+4 == len` and their exact fixed size;
    /// variable types take `[8, len)`; unknown types only need the header.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error> {
        let Some(head) = bytes.first_chunk::<HEADER_BYTES>() else {
            return Err(Error::Invalid("response header"));
        };
        let header = ResponseHeader::from_bytes(head);
        let body_bytes = &bytes[HEADER_BYTES..];
        let Ok(kind) = ResponseType::try_from(header.response_type) else {
            return Ok(Self {
                header,
                response_type: None,
                body: ResponseBody::Unknown(body_bytes),
                bytes,
            });
        };
        if usize::try_from(header.total_bytes).ok() != Some(bytes.len()) {
            return Err(Error::Invalid("response declared length"));
        }
        if let BodyLayout::Fixed(size) = kind.layout()
            && body_bytes.len() != size
        {
            return Err(Error::Invalid("response body size"));
        }
        let word = |index: usize| read_u32(body_bytes, index * 4);
        let bytes_at = |offset: usize| {
            body_bytes[offset..offset + USB_METER_BYTES]
                .try_into()
                .expect("fixed size checked")
        };
        let body = match kind {
            ResponseType::Diagnostics => ResponseBody::Diagnostics(DiagnosticsBlock {
                words: words_at(body_bytes, 0),
            }),
            ResponseType::Meter => ResponseBody::Meter(MeterRecord {
                core: word(0) as i32,
                column: word(1) as i32,
                index: word(2) as i32,
                value_bits: word(3),
            }),
            ResponseType::BufferResponse => ResponseBody::BufferResponse(BufferResponse {
                words: words_at(body_bytes, 0),
            }),
            ResponseType::ModelStart => ResponseBody::ModelStart(ModelStartRecord {
                core: word(0),
                slot: word(1),
                status: word(2),
            }),
            ResponseType::SlaveBootBufferAddress => {
                ResponseBody::SlaveBootBufferAddress(body_bytes)
            }
            ResponseType::SlaveBootResponse => ResponseBody::SlaveBootResponse(word(0)),
            ResponseType::MidiFromType07 | ResponseType::MidiFromType26 => {
                ResponseBody::Midi(MidiRecord {
                    words: words_at(body_bytes, 0),
                })
            }
            ResponseType::Soc2ArmDiagnostics => ResponseBody::Soc2ArmDiagnostics(
                Soc2ArmDiagnostics(body_bytes.try_into().expect("fixed size checked")),
            ),
            ResponseType::DebugText => ResponseBody::DebugText(DebugText(
                body_bytes.try_into().expect("fixed size checked"),
            )),
            ResponseType::ModuleLatency => ResponseBody::ModuleLatency(ModuleLatency {
                words: words_at(body_bytes, 0),
            }),
            ResponseType::BulkModelStart => ResponseBody::BulkModelStart(BulkModelStart {
                core: word(0),
                started_mask: word(1),
                failed_mask: word(2),
            }),
            ResponseType::BulkMeter => {
                let count = word(2);
                if count as usize > BULK_METER_MAX_VALUES {
                    return Err(Error::Invalid("bulk meter count"));
                }
                ResponseBody::BulkMeter(BulkMeter {
                    first: word(0),
                    second: word(1),
                    count,
                    value_bits: words_at(body_bytes, 3),
                })
            }
            ResponseType::MidiClockTempo => ResponseBody::MidiClockTempo(MidiClockTempo {
                int_arg: word(0) as i32,
                uint_arg: word(1),
            }),
            ResponseType::Flag0f => ResponseBody::Flag0f,
            ResponseType::HidReport => ResponseBody::HidReport(HidReport(
                body_bytes.try_into().expect("fixed size checked"),
            )),
            ResponseType::UsbMeterUpdate => ResponseBody::UsbMeterUpdate(UsbMeter(bytes_at(0))),
            ResponseType::LooperStatus => {
                ResponseBody::LooperStatus(LooperStatus::from_words(words_at(body_bytes, 0)))
            }
            ResponseType::SelfTest => ResponseBody::SelfTest(SelfTest {
                words: words_at(body_bytes, 0),
            }),
            ResponseType::UsbMeterInput => ResponseBody::UsbMeterInput {
                first: UsbMeter(bytes_at(0)),
                second: UsbMeter(bytes_at(USB_METER_BYTES)),
            },
            ResponseType::UsbMeterOutput => ResponseBody::UsbMeterOutput(UsbMeter(bytes_at(0))),
            ResponseType::MetronomeStatus => ResponseBody::MetronomeStatus(MetronomeStatus {
                words: words_at(body_bytes, 0),
            }),
            ResponseType::Soc2SoundsReceived => ResponseBody::Soc2SoundsReceived,
            ResponseType::Word1c => ResponseBody::Word1c(word(0)),
            ResponseType::TunerCalibrate => ResponseBody::TunerCalibrate(OpaqueCalibration(
                body_bytes.try_into().expect("fixed size checked"),
            )),
            ResponseType::TunerTimer => ResponseBody::TunerTimer(body_bytes),
            ResponseType::MidiOutTimer => ResponseBody::MidiOutTimer(body_bytes),
            ResponseType::StageOneComplete => ResponseBody::StageOneComplete(word(0)),
            ResponseType::StageTwoComplete => ResponseBody::StageTwoComplete(word(0), word(1)),
            ResponseType::Vector24 => ResponseBody::Vector24(KeyBytes(body_bytes)),
            ResponseType::Vector25 => ResponseBody::Vector25(KeyBytes(body_bytes)),
            ResponseType::Vector27 => ResponseBody::Vector27(KeyBytes(body_bytes)),
        };
        Ok(Self {
            header,
            response_type: Some(kind),
            body,
            bytes,
        })
    }

    pub fn header(&self) -> ResponseHeader {
        self.header
    }

    /// `None` for an unknown type byte.
    pub fn response_type(&self) -> Option<ResponseType> {
        self.response_type
    }

    pub fn body(&self) -> &ResponseBody<'a> {
        &self.body
    }

    pub fn into_body(self) -> ResponseBody<'a> {
        self.body
    }

    pub fn body_bytes(&self) -> &'a [u8] {
        &self.bytes[HEADER_BYTES..]
    }

    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

fn words_at<const N: usize>(body: &[u8], offset: usize) -> [u32; N] {
    std::array::from_fn(|index| read_u32(body, (offset + index) * 4))
}

/// Builds a packet with the owned `+4 == len` total and zero reserved bytes.
/// This is a test/fixture helper, not a recovered DSP producer.
pub fn encode(kind: ResponseType, selector: u8, body: &[u8]) -> Vec<u8> {
    let header = ResponseHeader {
        response_type: kind as u8,
        selector,
        reserved: [0; 2],
        total_bytes: (HEADER_BYTES + body.len()) as u32,
    };
    let mut bytes = Vec::with_capacity(HEADER_BYTES + body.len());
    bytes.extend_from_slice(&header.to_bytes());
    bytes.extend_from_slice(body);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le_words(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    #[test]
    fn rings_map_to_the_recovered_dispatchers_and_core_selectors() {
        assert_eq!(
            Dispatcher::for_ring(CoreId::Dsp(DspCore::Core0)),
            Dispatcher::Soc1
        );
        assert_eq!(
            Dispatcher::for_ring(CoreId::Dsp(DspCore::Core1)),
            Dispatcher::Soc1
        );
        assert_eq!(
            Dispatcher::for_ring(CoreId::Dsp(DspCore::Core2)),
            Dispatcher::Soc2
        );
        assert_eq!(
            Dispatcher::for_ring(CoreId::Dsp(DspCore::Core3)),
            Dispatcher::Soc2
        );
        assert_eq!(Dispatcher::for_ring(CoreId::Control), Dispatcher::Soc2);
        assert_eq!(Dispatcher::Soc1.diagnostics_core(0), DspCore::Core0);
        assert_eq!(Dispatcher::Soc1.diagnostics_core(7), DspCore::Core1);
        assert_eq!(Dispatcher::Soc2.diagnostics_core(0), DspCore::Core2);
        assert_eq!(Dispatcher::Soc2.diagnostics_core(2), DspCore::Core3);
        assert_eq!(Dispatcher::Soc1.self_test_core(0), DspCore::Core0);
        assert_eq!(Dispatcher::Soc1.self_test_core(2), DspCore::Core1);
        assert_eq!(Dispatcher::Soc2.self_test_core(2), DspCore::Core2);
        assert_eq!(Dispatcher::Soc2.self_test_core(0), DspCore::Core3);
        assert!(Dispatcher::Soc1.accepts_bulk_meter(9));
        assert!(Dispatcher::Soc2.accepts_bulk_meter(2));
        assert!(!Dispatcher::Soc2.accepts_bulk_meter(0));
        assert!(ResponseType::SlaveBootResponse.is_handled_by(Dispatcher::Soc1));
        assert!(!ResponseType::SlaveBootResponse.is_handled_by(Dispatcher::Soc2));
        assert!(ResponseType::LooperStatus.is_handled_by(Dispatcher::Soc2));
        assert!(!ResponseType::LooperStatus.is_handled_by(Dispatcher::Soc1));
        assert!(ResponseType::Diagnostics.is_handled_by(Dispatcher::Soc1));
        assert!(ResponseType::Diagnostics.is_handled_by(Dispatcher::Soc2));
    }

    #[test]
    fn every_known_type_round_trips_at_its_recovered_size() {
        for byte in 0..=0xff_u8 {
            let Ok(kind) = ResponseType::try_from(byte) else {
                continue;
            };
            assert_eq!(kind as u8, byte);
            let size = match kind.layout() {
                BodyLayout::Fixed(size) => size,
                BodyLayout::Variable => 5,
                BodyLayout::FlagOnly => 0,
            };
            let mut body: Vec<u8> = (0..size).map(|i| (i % 251) as u8 + 1).collect();
            if kind == ResponseType::BulkMeter {
                body[8..12].copy_from_slice(&3_u32.to_le_bytes());
            }
            let packet = encode(kind, 2, &body);
            let frame = ResponseFrame::decode(&packet).unwrap_or_else(|e| panic!("{kind:?}: {e}"));
            assert_eq!(frame.response_type(), Some(kind));
            assert_eq!(frame.body_bytes(), &body[..]);
            assert_eq!(frame.header().selector, 2);
            if let BodyLayout::Fixed(size) = kind.layout() {
                let mut short = packet.clone();
                short.truncate(HEADER_BYTES + size - 1);
                assert!(ResponseFrame::decode(&short).is_err(), "{kind:?} short");
                let mut long = packet.clone();
                long.push(0);
                assert!(ResponseFrame::decode(&long).is_err(), "{kind:?} long");
            }
            let mut bad_total = packet.clone();
            bad_total[4] ^= 1;
            assert!(ResponseFrame::decode(&bad_total).is_err(), "{kind:?} total");
        }
    }

    #[test]
    fn literal_diagnostics_meter_and_bulk_vectors_decode_field_exact() {
        // Type 0 with selector 1 and the three drain-read words set.
        let mut words = [0_u32; 16];
        words[3] = 0xffff_fffd; // FPEx count -3 as i32
        words[4] = 17;
        words[12] = 0x8000_0001;
        let packet = encode(ResponseType::Diagnostics, 1, &le_words(&words));
        assert_eq!(packet.len(), 72);
        assert_eq!(&packet[..8], &[0, 1, 0, 0, 72, 0, 0, 0]);
        let ResponseBody::Diagnostics(block) = *ResponseFrame::decode(&packet).unwrap().body()
        else {
            panic!("diagnostics");
        };
        assert_eq!(block.fpex_count(), -3);
        assert_eq!(block.fpex_slot(), 17);
        assert_eq!(block.error_flags(), 0x8000_0001);

        let meter = [
            1_u8, 0, 0xaa, 0x55, 24, 0, 0, 0, 4, 0, 0, 0, 1, 0, 0, 0, 0x20, 0x40, 0, 0, 0, 0, 0xc0,
            0x7f,
        ];
        let frame = ResponseFrame::decode(&meter).unwrap();
        assert_eq!(frame.header().reserved, [0xaa, 0x55]);
        let ResponseBody::Meter(record) = *frame.body() else {
            panic!("meter");
        };
        assert_eq!((record.core, record.column, record.index), (4, 1, 0x4020));
        assert_eq!(record.value_bits, 0x7fc0_0000);

        let mut bulk = vec![0x0d, 2, 0, 0, 84, 0, 0, 0];
        bulk.extend(le_words(&[7, 9, 2]));
        bulk.extend(le_words(&[0x3f80_0000, 0xbf80_0000]));
        bulk.extend([0; 14 * 4]);
        let ResponseBody::BulkMeter(record) = *ResponseFrame::decode(&bulk).unwrap().body() else {
            panic!("bulk meter");
        };
        assert_eq!((record.first, record.second), (7, 9));
        assert_eq!(record.values(), &[0x3f80_0000, 0xbf80_0000]);
        bulk[16] = 17;
        assert_eq!(
            ResponseFrame::decode(&bulk),
            Err(Error::Invalid("bulk meter count"))
        );
        bulk[16] = 16;
        assert_eq!(ResponseFrame::decode(&bulk).unwrap().body_bytes().len(), 76);
    }

    #[test]
    fn model_start_midi_and_variable_records_keep_raw_fields() {
        let bulk = BulkModelStart {
            core: 1,
            started_mask: 0b0101 | (1 << 29) | (1 << 31),
            failed_mask: 0b0111 | (1 << 30),
        };
        let slots: Vec<_> = bulk.slots().collect();
        assert_eq!(
            slots,
            [
                (0, SlotStart::Started),
                (1, SlotStart::Failed),
                (2, SlotStart::Started),
                (29, SlotStart::Started)
            ]
        );

        let midi = encode(
            ResponseType::MidiFromType26,
            0,
            &le_words(&[0x7f9a_fe0b, 0x0000_0145, 0xdead_beef]),
        );
        let frame = ResponseFrame::decode(&midi).unwrap();
        assert_eq!(frame.response_type(), Some(ResponseType::MidiFromType26));
        let ResponseBody::Midi(record) = *frame.body() else {
            panic!("midi");
        };
        assert_eq!(
            (
                record.code(),
                record.status(),
                record.data1(),
                record.data2()
            ),
            (0x0b, 0x9a, 0x7f, 0x45)
        );
        assert_eq!(record.words[2], 0xdead_beef);

        let empty_timer = [0x20, 0, 0, 0, 8, 0, 0, 0];
        assert_eq!(
            *ResponseFrame::decode(&empty_timer).unwrap().body(),
            ResponseBody::TunerTimer(&[])
        );
        let hid = encode(ResponseType::Vector27, 0, &[1, 2, 3]);
        assert_eq!(
            *ResponseFrame::decode(&hid).unwrap().body(),
            ResponseBody::Vector27(KeyBytes(&[1, 2, 3]))
        );
        let mut text = [0_u8; DEBUG_TEXT_BYTES];
        text[..5].copy_from_slice(b"hello");
        let packet = encode(ResponseType::DebugText, 0x02, &text);
        let ResponseBody::DebugText(debug) = *ResponseFrame::decode(&packet).unwrap().body() else {
            panic!("debug text");
        };
        assert_eq!(debug.text(), b"hello");
        assert_eq!(format!("{debug:?}"), "DebugText(\"hello\")");
    }

    #[test]
    fn unknown_types_and_short_headers_are_kept_or_refused_without_guessing() {
        let unknown = [0x30, 9, 8, 7, 0xff, 0xff, 0xff, 0xff, 1, 2];
        let frame = ResponseFrame::decode(&unknown).unwrap();
        assert_eq!(frame.response_type(), None);
        assert_eq!(*frame.body(), ResponseBody::Unknown(&[1, 2]));
        assert_eq!(frame.header().total_bytes, 0xffff_ffff);
        assert_eq!(frame.as_bytes(), &unknown);
        assert_eq!(
            ResponseFrame::decode(&unknown[..7]),
            Err(Error::Invalid("response header"))
        );
        let flag = encode(ResponseType::Soc2SoundsReceived, 0, &[9, 9]);
        assert_eq!(
            *ResponseFrame::decode(&flag).unwrap().body(),
            ResponseBody::Soc2SoundsReceived
        );
        let hid = encode(ResponseType::HidReport, 0, &[0x5a; HID_REPORT_BYTES]);
        assert!(!format!("{:?}", ResponseFrame::decode(&hid).unwrap().body()).contains("90"));
    }

    /// The signing material (`0x11`) and client id (`0x1f`) never reach a log.
    #[test]
    fn cloud_signing_replies_hide_their_bytes_from_debug() {
        for (kind, size) in [
            (ResponseType::UsbMeterUpdate, USB_METER_BYTES),
            (ResponseType::TunerCalibrate, 32),
        ] {
            let body = vec![0xa5; size];
            let packet = encode(kind, 4, &body);
            let frame = ResponseFrame::decode(&packet).unwrap();
            let text = format!("{frame:?} {:?}", frame.body());
            assert!(
                !text.contains("165") && !text.to_lowercase().contains("a5"),
                "{text}"
            );
        }
    }
}

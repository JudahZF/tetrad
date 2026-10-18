//! Bounded tuner and looper DSP codecs recovered from ZenUI (CorOS 4.1.0).
//!
//! Requests (ARM → DSP) use a 16-byte header; responses (DSP → ARM) use an
//! 8-byte header and their own type namespace. Response byte `0x12` is a looper
//! status record, not [`MessageType::SlaveBootBufferMemAddrRequest`].
//!
//! These codecs only shape bytes. A successful encode, decode or ring copy does
//! not show that the DSP accepted, consumed or acknowledged a message.

use std::fmt;

use zeroize::Zeroize;

use crate::{Error, MessageType, read_u32};

pub const REQUEST_HEADER_BYTES: usize = 16;
pub const RESPONSE_HEADER_BYTES: usize = 8;
/// Stock copies `min(QString character count, 32)` bytes (VAs 0x5151bc, 0x5154c8).
pub const TEXT_PREFIX_MAX: usize = 32;
pub const REQUEST_MAX_BYTES: usize = REQUEST_HEADER_BYTES + TEXT_PREFIX_MAX;

/// Meter record selection used by `TunerMeterService` (VA 0x4e13d8). These are
/// payload words, not the response header selector byte.
pub const TUNER_METER_CORE: i32 = 4;
pub const TUNER_METER_INDEX: i32 = 0x4020;

/// Already-selected raw byte prefix (0..=32 bytes) for the `0x35`/`0x4f` text
/// requests. No encoding conversion or Unicode truncation is applied here.
/// `Debug` shows only the length.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TextPrefix {
    bytes: [u8; TEXT_PREFIX_MAX],
    len: u8,
}

impl TextPrefix {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

impl TryFrom<&[u8]> for TextPrefix {
    type Error = Error;
    fn try_from(value: &[u8]) -> Result<Self, Error> {
        if value.len() > TEXT_PREFIX_MAX {
            return Err(Error::Limit("tuner text prefix"));
        }
        let mut bytes = [0; TEXT_PREFIX_MAX];
        bytes[..value.len()].copy_from_slice(value);
        Ok(Self {
            bytes,
            len: value.len() as u8,
        })
    }
}

impl fmt::Debug for TextPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextPrefix")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

/// `0x36` is sent to destination 4, then 0 (VA 0x52098c). The physical identity
/// of either destination is unresolved, so they are named by wire value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TunerEnableDestination {
    Zero = 0,
    Four = 4,
}

/// Fields each recovered writer stores. Writer constants (fixed destinations and
/// zero selectors) are implied by the variant. All numeric IDs stay raw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunerLooperRequest {
    /// `0x29`, VA 0x875b64. `event` is a raw DSP event number.
    LooperEvent { track: u8, event: u32 },
    /// `0x2a`, VA 0x875bd0. Track byte is always 0. `value_bits` is an f32.
    LooperParameter { parameter: u32, value_bits: u32 },
    /// `0x2b`, VA 0x875c5c.
    LooperStatusRequest { track: u8 },
    /// `0x2c`, VA 0x8758a4. Stock uses destinations 0 and 1.
    GlobalLooperInsertPath { destination: u8, path: u32 },
    /// `0x34`, VA 0x87590c. Low byte of the caller's port/path word.
    GlobalTunerInputPath { port: u8 },
    /// `0x35` (nominal `TunerMuteMessage`), VA 0x5151bc. A text prefix to
    /// destination 3, not a mute Boolean. Purpose unresolved.
    TunerText35(TextPrefix),
    /// `0x36`, VA 0x52098c.
    TunerEnable {
        destination: TunerEnableDestination,
        enabled: bool,
    },
    /// `0x37`, VA 0x875e98. `reference_hz_bits` is the f32 reference
    /// frequency in Hz (not the host reference offset). `word` is the caller
    /// word at `+4`; observed UI callers pass 0.
    TunerBaseFrequency { word: u32, reference_hz_bits: u32 },
    /// `0x38`, VA 0x875ef8.
    TunerFrequencyRequest,
    /// `0x4f` (nominal `TunerCalibrate`), VA 0x5154c8. A text prefix to
    /// destination 4, not calibration math. Purpose unresolved.
    TunerText4f(TextPrefix),
}

impl TunerLooperRequest {
    pub const fn message_type(&self) -> MessageType {
        match self {
            Self::LooperEvent { .. } => MessageType::LooperEvent,
            Self::LooperParameter { .. } => MessageType::LooperParameter,
            Self::LooperStatusRequest { .. } => MessageType::LooperStatusRequest,
            Self::GlobalLooperInsertPath { .. } => MessageType::GlobalLooperInsertPath,
            Self::GlobalTunerInputPath { .. } => MessageType::GlobalTunerInputPath,
            Self::TunerText35(_) => MessageType::TunerMuteMessage,
            Self::TunerEnable { .. } => MessageType::TunerEnableMessage,
            Self::TunerBaseFrequency { .. } => MessageType::TunerBaseFrequency,
            Self::TunerFrequencyRequest => MessageType::TunerFrequencyRequest,
            Self::TunerText4f(_) => MessageType::TunerCalibrate,
        }
    }

    /// Transport destination byte `+1`, also the stock send destination argument.
    pub const fn destination(&self) -> u8 {
        match self {
            Self::GlobalLooperInsertPath { destination, .. } => *destination,
            Self::GlobalTunerInputPath { .. } => 0,
            Self::TunerText35(_) => 3,
            Self::TunerEnable { destination, .. } => *destination as u8,
            _ => 4,
        }
    }

    fn total_bytes(&self) -> usize {
        match self {
            Self::LooperEvent { .. }
            | Self::LooperParameter { .. }
            | Self::GlobalLooperInsertPath { .. }
            | Self::TunerBaseFrequency { .. } => 20,
            Self::TunerText35(text) | Self::TunerText4f(text) => {
                REQUEST_HEADER_BYTES + text.as_bytes().len()
            }
            _ => REQUEST_HEADER_BYTES,
        }
    }

    /// Writes the stored fields over `bytes`, leaving unstored bytes untouched.
    fn write(&self, bytes: &mut [u8; REQUEST_MAX_BYTES]) {
        let total = self.total_bytes();
        bytes[0] = self.message_type() as u8;
        bytes[1] = self.destination();
        bytes[8..12].copy_from_slice(&(total as u32).to_le_bytes());
        let mut put = |offset: usize, word: u32| {
            bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        };
        match *self {
            Self::LooperEvent { track, event } => {
                put(16, event);
                bytes[2] = track;
            }
            Self::LooperParameter {
                parameter,
                value_bits,
            } => {
                put(4, parameter);
                put(16, value_bits);
                bytes[2] = 0;
            }
            Self::LooperStatusRequest { track } => bytes[2] = track,
            Self::GlobalLooperInsertPath { path, .. } => {
                put(16, path);
                bytes[2] = 0;
            }
            Self::GlobalTunerInputPath { port } => {
                put(4, 0);
                bytes[2] = port;
            }
            Self::TunerText35(text) | Self::TunerText4f(text) => {
                bytes[16..total].copy_from_slice(text.as_bytes());
            }
            Self::TunerEnable { enabled, .. } => bytes[2] = u8::from(enabled),
            Self::TunerBaseFrequency {
                word,
                reference_hz_bits,
            } => {
                put(4, word);
                put(16, reference_hz_bits);
            }
            Self::TunerFrequencyRequest => bytes[2] = 0,
        }
    }

    /// Parses a frame whose type, declared length and slice length already agree.
    /// Writer constants are checked (owned policy: accept only shapes the
    /// recovered writers can produce).
    fn parse(message_type: MessageType, bytes: &[u8]) -> Result<Self, Error> {
        let fixed = |expected: usize, destination: u8| {
            if bytes.len() != expected {
                Err(Error::Invalid("tuner/looper request size"))
            } else if bytes[1] != destination {
                Err(Error::Invalid("tuner/looper request destination"))
            } else {
                Ok(())
            }
        };
        let zero_selector = || match bytes[2] {
            0 => Ok(()),
            _ => Err(Error::Invalid("tuner/looper request selector")),
        };
        let text = |destination: u8| {
            if bytes[1] != destination {
                return Err(Error::Invalid("tuner/looper request destination"));
            }
            TextPrefix::try_from(&bytes[REQUEST_HEADER_BYTES..])
        };
        Ok(match message_type {
            MessageType::LooperEvent => {
                fixed(20, 4)?;
                Self::LooperEvent {
                    track: bytes[2],
                    event: read_u32(bytes, 16),
                }
            }
            MessageType::LooperParameter => {
                fixed(20, 4)?;
                // VA 0x875bd0 asserts track zero.
                zero_selector()?;
                Self::LooperParameter {
                    parameter: read_u32(bytes, 4),
                    value_bits: read_u32(bytes, 16),
                }
            }
            MessageType::LooperStatusRequest => {
                fixed(16, 4)?;
                Self::LooperStatusRequest { track: bytes[2] }
            }
            MessageType::GlobalLooperInsertPath => {
                fixed(20, bytes[1])?;
                zero_selector()?;
                Self::GlobalLooperInsertPath {
                    destination: bytes[1],
                    path: read_u32(bytes, 16),
                }
            }
            MessageType::GlobalTunerInputPath => {
                fixed(16, 0)?;
                if read_u32(bytes, 4) != 0 {
                    return Err(Error::Invalid("tuner input path word"));
                }
                Self::GlobalTunerInputPath { port: bytes[2] }
            }
            MessageType::TunerMuteMessage => Self::TunerText35(text(3)?),
            MessageType::TunerEnableMessage => {
                let destination = match bytes[1] {
                    0 => TunerEnableDestination::Zero,
                    4 => TunerEnableDestination::Four,
                    _ => return Err(Error::Invalid("tuner/looper request destination")),
                };
                fixed(16, bytes[1])?;
                let enabled = match bytes[2] {
                    0 => false,
                    1 => true,
                    _ => return Err(Error::Invalid("tuner enable selector")),
                };
                Self::TunerEnable {
                    destination,
                    enabled,
                }
            }
            MessageType::TunerBaseFrequency => {
                fixed(20, 4)?;
                Self::TunerBaseFrequency {
                    word: read_u32(bytes, 4),
                    reference_hz_bits: read_u32(bytes, 16),
                }
            }
            MessageType::TunerFrequencyRequest => {
                fixed(16, 4)?;
                zero_selector()?;
                Self::TunerFrequencyRequest
            }
            MessageType::TunerCalibrate => Self::TunerText4f(text(4)?),
            _ => return Err(Error::Invalid("tuner/looper request type")),
        })
    }
}

/// One complete request as wire bytes, plus its typed view.
///
/// [`RequestFrame::decode`] retains every byte, including unresolved header
/// bytes (`+3`, `+0x0c..0x10`, and per type `+2`/`+4..8` where the writer
/// stores nothing). [`RequestFrame::new`] zero-fills those bytes; that is an
/// owned choice to avoid stale data. Stock writers reuse a controller buffer,
/// so `new` is not byte-identical to stock output.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RequestFrame {
    request: TunerLooperRequest,
    bytes: [u8; REQUEST_MAX_BYTES],
    len: u8,
}

impl RequestFrame {
    pub fn new(request: TunerLooperRequest) -> Self {
        let mut bytes = [0; REQUEST_MAX_BYTES];
        request.write(&mut bytes);
        Self {
            request,
            bytes,
            len: request.total_bytes() as u8,
        }
    }

    /// Strict decode: the `+8` total must equal the slice length and the exact
    /// size for the type.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if !(REQUEST_HEADER_BYTES..=REQUEST_MAX_BYTES).contains(&bytes.len()) {
            return Err(Error::Invalid("tuner/looper request size"));
        }
        let message_type = MessageType::try_from(bytes[0])?;
        if usize::try_from(read_u32(bytes, 8)).ok() != Some(bytes.len()) {
            return Err(Error::Invalid("tuner/looper request length word"));
        }
        let request = TunerLooperRequest::parse(message_type, bytes)?;
        let mut retained = [0; REQUEST_MAX_BYTES];
        retained[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            request,
            bytes: retained,
            len: bytes.len() as u8,
        })
    }

    pub fn request(&self) -> TunerLooperRequest {
        self.request
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

impl fmt::Debug for RequestFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Header only: text request data stays out of logs.
        f.debug_struct("RequestFrame")
            .field("request", &self.request)
            .field("header", &&self.bytes[..REQUEST_HEADER_BYTES])
            .finish_non_exhaustive()
    }
}

/// Response namespace (DSP → ARM), dispatched by VA 0x89cc60. Separate from
/// [`MessageType`]: the same byte values mean different things.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ResponseType {
    Meter = 0x01,
    LooperStatus = 0x12,
    OpaqueCalibration = 0x1f,
    OpaqueTimer = 0x20,
}

impl ResponseType {
    /// Owned strict size. For the fixed records stock only proves a minimum
    /// readable span; the DSP producer's `+4` value is not recovered.
    pub const fn exact_bytes(self) -> usize {
        RESPONSE_HEADER_BYTES
            + match self {
                Self::Meter => 16,
                Self::LooperStatus => 88,
                Self::OpaqueCalibration => 32,
                // The SceneGraph consumer (VA 0x5acc30) needs exactly 8 bytes.
                Self::OpaqueTimer => 8,
            }
    }
}

impl TryFrom<u8> for ResponseType {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            0x01 => Ok(Self::Meter),
            0x12 => Ok(Self::LooperStatus),
            0x1f => Ok(Self::OpaqueCalibration),
            0x20 => Ok(Self::OpaqueTimer),
            _ => Err(Error::Invalid("tuner/looper response type")),
        }
    }
}

/// `dspMeterResponseSignal(int,int,int,float)` record (body of type `0x01`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeterRecord {
    pub core: i32,
    pub column: i32,
    pub index: i32,
    pub value_bits: u32,
}

impl MeterRecord {
    /// Detected pitch in Hz when the record matches the tuner selection at
    /// VA 0x4e13d8 (core 4, index 0x4020, value >= 0). The column is ignored.
    /// Service/controller guards in that function are not modelled.
    pub fn tuner_detected_hz(&self) -> Option<f32> {
        let value = f32::from_bits(self.value_bits);
        (self.core == TUNER_METER_CORE && self.index == TUNER_METER_INDEX && value >= 0.0)
            .then_some(value)
    }
}

/// `Looper::Status`: 22 LE words, copied unchanged to the host protobuf by
/// VA 0x18fa84. `*_bits` fields are f32 bit patterns. Flag-like words stay u32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LooperStatus {
    pub state: u32,
    pub progress_bits: u32,
    pub undo_progress_bits: u32,
    pub duplicate_cycle: u32,
    pub num_duplicate_cycles: u32,
    pub one_shot_stopped: u32,
    pub redo_available: u32,
    pub loop_length: u32,
    pub free_samples: u32,
    pub in_reverse: u32,
    pub one_shot: u32,
    pub half_speed: u32,
    pub fixed_duplicate_cycles: u32,
    pub armed: u32,
    pub waiting_for_cycle: u32,
    pub undo_count: u32,
    pub max_write_displacement_bits: u32,
    pub min_write_displacement_bits: u32,
    pub events_waiting_for_quantize: u32,
    pub current_clock: u32,
    pub transition: u32,
    pub action: u32,
}

impl LooperStatus {
    pub const fn from_words(w: [u32; 22]) -> Self {
        Self {
            state: w[0],
            progress_bits: w[1],
            undo_progress_bits: w[2],
            duplicate_cycle: w[3],
            num_duplicate_cycles: w[4],
            one_shot_stopped: w[5],
            redo_available: w[6],
            loop_length: w[7],
            free_samples: w[8],
            in_reverse: w[9],
            one_shot: w[10],
            half_speed: w[11],
            fixed_duplicate_cycles: w[12],
            armed: w[13],
            waiting_for_cycle: w[14],
            undo_count: w[15],
            max_write_displacement_bits: w[16],
            min_write_displacement_bits: w[17],
            events_waiting_for_quantize: w[18],
            current_clock: w[19],
            transition: w[20],
            action: w[21],
        }
    }

    pub const fn to_words(&self) -> [u32; 22] {
        [
            self.state,
            self.progress_bits,
            self.undo_progress_bits,
            self.duplicate_cycle,
            self.num_duplicate_cycles,
            self.one_shot_stopped,
            self.redo_available,
            self.loop_length,
            self.free_samples,
            self.in_reverse,
            self.one_shot,
            self.half_speed,
            self.fixed_duplicate_cycles,
            self.armed,
            self.waiting_for_cycle,
            self.undo_count,
            self.max_write_displacement_bits,
            self.min_write_displacement_bits,
            self.events_waiting_for_quantize,
            self.current_clock,
            self.transition,
            self.action,
        ]
    }
}

/// 32 opaque bytes from response `0x1f` (`tunerCalibrateUpdateSignal`).
/// Meaning unresolved; response `0x1f` on the main rings carries the cloud client id. `Debug`
/// hides the contents. Not `Copy`: the bytes are zeroed on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct OpaqueCalibration(pub [u8; 32]);

impl Drop for OpaqueCalibration {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// 8 opaque bytes from response `0x20` (`tunerTimerUpdateSignal`). The stock
/// consumer reads them as a LE u64 of unknown unit; it is not pitch.
/// `Debug` hides the contents.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OpaqueTimer(pub [u8; 8]);

impl fmt::Debug for OpaqueCalibration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OpaqueCalibration(..)")
    }
}

impl fmt::Debug for OpaqueTimer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OpaqueTimer(..)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunerLooperResponse {
    Meter(MeterRecord),
    LooperStatus(LooperStatus),
    OpaqueCalibration(OpaqueCalibration),
    OpaqueTimer(OpaqueTimer),
}

impl TunerLooperResponse {
    pub const fn response_type(&self) -> ResponseType {
        match self {
            Self::Meter(_) => ResponseType::Meter,
            Self::LooperStatus(_) => ResponseType::LooperStatus,
            Self::OpaqueCalibration(_) => ResponseType::OpaqueCalibration,
            Self::OpaqueTimer(_) => ResponseType::OpaqueTimer,
        }
    }
}

/// One response with its raw 8-byte header. Byte `+1` (a core selector in
/// other response cases) and bytes `+2..4` are retained but not interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseFrame {
    header: [u8; RESPONSE_HEADER_BYTES],
    response: TunerLooperResponse,
}

impl ResponseFrame {
    /// Zero-fills header bytes `+1..4` (owned choice; producer values unknown)
    /// and writes the owned exact total at `+4`.
    pub fn new(response: TunerLooperResponse) -> Self {
        let kind = response.response_type();
        let mut header = [0; RESPONSE_HEADER_BYTES];
        header[0] = kind as u8;
        header[4..].copy_from_slice(&(kind.exact_bytes() as u32).to_le_bytes());
        Self { header, response }
    }

    /// Strict decode (owned policy): the slice length and the `+4` total must
    /// both equal [`ResponseType::exact_bytes`]. Does not allocate.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let Some(header) = bytes.first_chunk::<RESPONSE_HEADER_BYTES>() else {
            return Err(Error::Invalid("tuner/looper response header"));
        };
        let kind = ResponseType::try_from(header[0])?;
        if bytes.len() != kind.exact_bytes()
            || usize::try_from(read_u32(header, 4)).ok() != Some(bytes.len())
        {
            return Err(Error::Invalid("tuner/looper response length"));
        }
        let body = &bytes[RESPONSE_HEADER_BYTES..];
        let word = |index: usize| read_u32(body, index * 4);
        let response = match kind {
            ResponseType::Meter => TunerLooperResponse::Meter(MeterRecord {
                core: word(0) as i32,
                column: word(1) as i32,
                index: word(2) as i32,
                value_bits: word(3),
            }),
            ResponseType::LooperStatus => TunerLooperResponse::LooperStatus(
                LooperStatus::from_words(std::array::from_fn(word)),
            ),
            ResponseType::OpaqueCalibration => TunerLooperResponse::OpaqueCalibration(
                OpaqueCalibration(body.try_into().expect("exact size checked")),
            ),
            ResponseType::OpaqueTimer => TunerLooperResponse::OpaqueTimer(OpaqueTimer(
                body.try_into().expect("exact size checked"),
            )),
        };
        Ok(Self {
            header: *header,
            response,
        })
    }

    pub fn header(&self) -> &[u8; RESPONSE_HEADER_BYTES] {
        &self.header
    }

    pub fn response(&self) -> &TunerLooperResponse {
        &self.response
    }

    /// Byte-exact inverse of [`ResponseFrame::decode`].
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.response.response_type().exact_bytes());
        bytes.extend_from_slice(&self.header);
        let mut words = |words: &[u32]| {
            for word in words {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
        };
        match &self.response {
            TunerLooperResponse::Meter(meter) => words(&[
                meter.core as u32,
                meter.column as u32,
                meter.index as u32,
                meter.value_bits,
            ]),
            TunerLooperResponse::LooperStatus(status) => words(&status.to_words()),
            TunerLooperResponse::OpaqueCalibration(data) => bytes.extend_from_slice(&data.0),
            TunerLooperResponse::OpaqueTimer(data) => bytes.extend_from_slice(&data.0),
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(bytes: &[u8]) -> TextPrefix {
        TextPrefix::try_from(bytes).unwrap()
    }

    fn le(word: u32) -> [u8; 4] {
        word.to_le_bytes()
    }

    /// Expected bytes per recovered writer, with owned zero fill for unstored bytes.
    #[test]
    fn requests_match_recovered_writer_layouts() {
        let head = |kind: u8, dest: u8, selector: u8, word: u32, total: u32| {
            let mut h = vec![kind, dest, selector, 0];
            h.extend(le(word));
            h.extend(le(total));
            h.extend([0; 4]);
            h
        };
        let with = |mut h: Vec<u8>, data: &[u8]| {
            h.extend_from_slice(data);
            h
        };
        let nan = 0x7fc0_1234;
        let cases = [
            (
                TunerLooperRequest::LooperEvent {
                    track: 7,
                    event: 0x0102_0304,
                },
                with(head(0x29, 4, 7, 0, 20), &[4, 3, 2, 1]),
            ),
            (
                TunerLooperRequest::LooperParameter {
                    parameter: 0xdead_beef,
                    value_bits: nan,
                },
                with(head(0x2a, 4, 0, 0xdead_beef, 20), &le(nan)),
            ),
            (
                TunerLooperRequest::LooperStatusRequest { track: 0 },
                head(0x2b, 4, 0, 0, 16),
            ),
            (
                TunerLooperRequest::GlobalLooperInsertPath {
                    destination: 1,
                    path: u32::MAX,
                },
                with(head(0x2c, 1, 0, 0, 20), &[0xff; 4]),
            ),
            (
                TunerLooperRequest::GlobalTunerInputPath { port: 0x13 },
                head(0x34, 0, 0x13, 0, 16),
            ),
            (
                TunerLooperRequest::TunerText35(text(b"ab")),
                with(head(0x35, 3, 0, 0, 18), b"ab"),
            ),
            (
                TunerLooperRequest::TunerEnable {
                    destination: TunerEnableDestination::Four,
                    enabled: true,
                },
                head(0x36, 4, 1, 0, 16),
            ),
            (
                TunerLooperRequest::TunerEnable {
                    destination: TunerEnableDestination::Zero,
                    enabled: false,
                },
                head(0x36, 0, 0, 0, 16),
            ),
            (
                TunerLooperRequest::TunerBaseFrequency {
                    word: 0,
                    reference_hz_bits: (-0.0_f32).to_bits(),
                },
                with(head(0x37, 4, 0, 0, 20), &[0, 0, 0, 0x80]),
            ),
            (
                TunerLooperRequest::TunerFrequencyRequest,
                head(0x38, 4, 0, 0, 16),
            ),
            (
                TunerLooperRequest::TunerText4f(text(&[0xff; 32])),
                with(head(0x4f, 4, 0, 0, 48), &[0xff; 32]),
            ),
        ];
        for (request, expected) in cases {
            let frame = RequestFrame::new(request);
            assert_eq!(frame.as_bytes(), expected, "{request:?}");
            assert_eq!(frame.as_bytes()[1], request.destination());
            assert_eq!(RequestFrame::decode(&expected).unwrap(), frame);
        }
    }

    #[test]
    fn text_prefix_is_bounded_and_redacted() {
        let empty = RequestFrame::new(TunerLooperRequest::TunerText35(text(b"")));
        assert_eq!(empty.as_bytes().len(), 16);
        assert_eq!(
            TextPrefix::try_from(&[0_u8; 33][..]),
            Err(Error::Limit("tuner text prefix"))
        );
        let mut oversize = RequestFrame::new(TunerLooperRequest::TunerText4f(text(&[1; 32])))
            .as_bytes()
            .to_vec();
        oversize.push(1);
        oversize[8..12].copy_from_slice(&le(49));
        assert!(RequestFrame::decode(&oversize).is_err());
        let secret = RequestFrame::new(TunerLooperRequest::TunerText35(text(b"SECRET")));
        assert!(!format!("{secret:?}").contains("SECRET"));
        assert!(!format!("{secret:?}").contains("83, 69"));
    }

    #[test]
    fn request_decode_retains_unresolved_bytes_and_rejects_bad_shapes() {
        // Stale bytes at +3, +4..8 (unstored for 0x29) and +0x0c..0x10.
        let mut stale = RequestFrame::new(TunerLooperRequest::LooperEvent { track: 0, event: 2 })
            .as_bytes()
            .to_vec();
        stale[3] = 0xa5;
        stale[4..8].copy_from_slice(&le(0x1122_3344));
        stale[12..16].copy_from_slice(&le(0x5566_7788));
        let frame = RequestFrame::decode(&stale).unwrap();
        assert_eq!(frame.as_bytes(), stale);
        assert_eq!(
            frame.request(),
            TunerLooperRequest::LooperEvent { track: 0, event: 2 }
        );

        let valid = |request| RequestFrame::new(request).as_bytes().to_vec();
        let mutate = |mut bytes: Vec<u8>, offset: usize, value: u8| {
            bytes[offset] = value;
            bytes
        };
        let parameter = valid(TunerLooperRequest::LooperParameter {
            parameter: 0,
            value_bits: 0,
        });
        let rejected = [
            mutate(parameter.clone(), 2, 1),    // nonzero track asserts in stock
            mutate(parameter.clone(), 8, 16),   // declared length mismatch
            mutate(parameter.clone(), 0, 0x02), // ParameterChange is not owned here
            mutate(parameter, 0, 0x5c),         // unknown request type
            mutate(valid(TunerLooperRequest::TunerText35(text(b""))), 1, 4),
            mutate(
                valid(TunerLooperRequest::TunerEnable {
                    destination: TunerEnableDestination::Four,
                    enabled: false,
                }),
                2,
                2,
            ),
            mutate(
                valid(TunerLooperRequest::TunerEnable {
                    destination: TunerEnableDestination::Four,
                    enabled: false,
                }),
                1,
                1,
            ),
            mutate(
                valid(TunerLooperRequest::GlobalTunerInputPath { port: 0 }),
                4,
                1,
            ),
        ];
        for bytes in rejected {
            assert!(RequestFrame::decode(&bytes).is_err(), "{bytes:02x?}");
        }
        // A fixed 20-byte type declared and sent as 16 bytes.
        let mut short = valid(TunerLooperRequest::LooperStatusRequest { track: 0 });
        short[0] = 0x29;
        assert!(RequestFrame::decode(&short).is_err());
    }

    #[test]
    fn responses_decode_exact_offsets_and_round_trip_bits() {
        let mut meter = vec![0x01, 0x9c, 0xee, 0xff];
        meter.extend(le(24));
        for word in [4, u32::MAX, 0x4020, 440.0_f32.to_bits()] {
            meter.extend(le(word));
        }
        let frame = ResponseFrame::decode(&meter).unwrap();
        assert_eq!(frame.header(), &meter[..8]);
        let &TunerLooperResponse::Meter(record) = frame.response() else {
            panic!("meter");
        };
        assert_eq!((record.core, record.column, record.index), (4, -1, 0x4020));
        assert_eq!(record.tuner_detected_hz(), Some(440.0));
        assert_eq!(frame.to_bytes(), meter);
        // Header selector byte is not the payload core.
        let other_core = MeterRecord { core: 3, ..record };
        assert_eq!(other_core.tuner_detected_hz(), None);
        for bits in [(-1.0_f32).to_bits(), f32::NAN.to_bits()] {
            let rejected = MeterRecord {
                value_bits: bits,
                ..record
            };
            assert_eq!(rejected.tuner_detected_hz(), None);
        }

        let mut words: [u32; 22] = std::array::from_fn(|i| 0x0100_0000 * i as u32 + 7);
        words[1] = 0x7fa0_0001; // signalling NaN payload in progress
        words[2] = (-0.0_f32).to_bits();
        words[16] = f32::INFINITY.to_bits();
        let status = LooperStatus::from_words(words);
        let encoded = ResponseFrame::new(TunerLooperResponse::LooperStatus(status)).to_bytes();
        assert_eq!(encoded.len(), 96);
        assert_eq!(&encoded[..8], &[0x12, 0, 0, 0, 96, 0, 0, 0]);
        assert_eq!(&encoded[0x0c..0x10], &le(0x7fa0_0001));
        assert_eq!(&encoded[0x48..0x4c], &le(f32::INFINITY.to_bits()));
        assert_eq!(&encoded[0x5c..0x60], &le(words[21]));
        let decoded = ResponseFrame::decode(&encoded).unwrap();
        assert_eq!(
            decoded.response(),
            &TunerLooperResponse::LooperStatus(status)
        );
        assert_eq!(status.to_words(), words);
        assert_eq!(status.action, words[21]);
        assert_eq!(status.loop_length, words[7]);

        for response in [
            TunerLooperResponse::OpaqueCalibration(OpaqueCalibration([0x5a; 32])),
            TunerLooperResponse::OpaqueTimer(OpaqueTimer(*b"12345678")),
        ] {
            let mut bytes = ResponseFrame::new(response.clone()).to_bytes();
            bytes[1..4].copy_from_slice(&[9, 8, 7]);
            let frame = ResponseFrame::decode(&bytes).unwrap();
            assert_eq!(frame.response(), &response);
            assert_eq!(frame.to_bytes(), bytes);
            assert!(!format!("{frame:?}").contains("90, 90"));
            assert!(!format!("{frame:?}").contains("49, 50"));
        }
    }

    #[test]
    fn response_decoder_is_directional_and_exact_size() {
        // Request bytes are not responses, even where a type byte overlaps.
        let request = RequestFrame::new(TunerLooperRequest::LooperEvent { track: 0, event: 1 });
        assert!(ResponseFrame::decode(request.as_bytes()).is_err());
        let status = ResponseFrame::new(TunerLooperResponse::LooperStatus(
            LooperStatus::from_words([0; 22]),
        ))
        .to_bytes();
        assert!(RequestFrame::decode(&status).is_err());
        assert!(RequestFrame::decode(&status[..16]).is_err());

        let mut long = status.clone();
        long.push(0);
        long[4] = 97;
        let mut wrong_declared = status.clone();
        wrong_declared[4] = 0x60 + 4;
        let timer_without_body = [0x20, 0, 0, 0, 8, 0, 0, 0];
        for bytes in [
            &status[..95],
            &long[..],
            &wrong_declared[..],
            &timer_without_body[..],
            &status[..7],
        ] {
            assert!(
                ResponseFrame::decode(bytes).is_err(),
                "{} bytes",
                bytes.len()
            );
        }
    }
}

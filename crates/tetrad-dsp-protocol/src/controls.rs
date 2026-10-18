//! Offline metronome, MIDI, gate and output-EQ request codecs.
//!
//! Tempo change, bulk metronome block and sound bundle are included. Float values retain their bits; raw flags
//! are not assumed to be Booleans. No transport, file access or DSP execution.
use crate::fragment::{Fragment, Fragmenter, Marker, MessageMetadata};
use crate::{CodecLimits, CoreId, DspCore, Error, MessageType, read_u32};

pub const HEADER_BYTES: usize = 16;
pub const MAX_BYTES: usize = 28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbMidiDelivery {
    Immediate,
    /// Selector 1 and caller word at +4; the word's units remain unresolved.
    Tagged(u32),
}

/// The five bytes explicitly stored by the USB MIDI writers. This is not a
/// standard four-byte USB MIDI event packet: stock inserts zero at +17.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbMidiEvent {
    pub code: u8,
    pub status: u8,
    pub data: [u8; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlRequest {
    /// VA 0x875a48: the integer BPM travels in the header word at +4; there is
    /// no body. Stock passes `(int)bpm` without any range check.
    TempoChange {
        bpm: i32,
    },
    MetronomeParameter {
        destination: u8,
        parameter: u32,
        value_bits: u32,
    },
    MetronomeStatusRequest,
    MetronomeDestination {
        value_bits: u32,
    },
    MetronomeSound,
    EnableUsbMidiClock {
        value: u8,
    },
    EnableInternalMidiClock {
        value: u32,
    },
    ResetInternalMidiClock {
        value: u32,
    },
    EnableMidiClockOut {
        value: u32,
    },
    EnableMidiClockIn {
        value: u8,
    },
    MidiClockCountAndTempo {
        tempo_bits: u32,
        count: u32,
    },
    MidiClockStart,
    UsbMidiOut {
        delivery: UsbMidiDelivery,
        event: UsbMidiEvent,
    },
    InputGateSource {
        destination: u8,
        gate: u8,
        word: u32,
        source: u32,
    },
    InputGateEnabled {
        destination: u8,
        value: u32,
    },
    InputGateParameter {
        destination: u8,
        gate: u8,
        parameter: i32,
        value_bits: u32,
    },
    InputGateInputGain {
        destination: u8,
        gate: u8,
        value_bits: u32,
    },
    OutputEqualizerEnable {
        value: u32,
    },
    OutputEqualizerParameter {
        output: u8,
        parameter: i32,
        value_bits: u32,
    },
}

impl ControlRequest {
    pub const fn message_type(self) -> MessageType {
        match self {
            Self::TempoChange { .. } => MessageType::TempoChange,
            Self::MetronomeParameter { .. } => MessageType::MetronomeParameter,
            Self::MetronomeStatusRequest => MessageType::MetronomeStatusRequest,
            Self::MetronomeDestination { .. } => MessageType::MetronomeDestination,
            Self::MetronomeSound => MessageType::MetronomeSound,
            Self::EnableUsbMidiClock { .. } => MessageType::EnableUsbMidiClock,
            Self::EnableInternalMidiClock { .. } => MessageType::EnableInternalMidiClock,
            Self::ResetInternalMidiClock { .. } => MessageType::ResetInternalMidiClock,
            Self::EnableMidiClockOut { .. } => MessageType::EnableMidiClockOut,
            Self::EnableMidiClockIn { .. } => MessageType::EnableMidiClockIn,
            Self::MidiClockCountAndTempo { .. } => MessageType::MidiClockCountAndTempo,
            Self::MidiClockStart => MessageType::MidiClockStart,
            Self::UsbMidiOut { .. } => MessageType::UsbMidiOut,
            Self::InputGateSource { .. } => MessageType::SetInputGateSource,
            Self::InputGateEnabled { .. } => MessageType::SetInputGateEnabled,
            Self::InputGateParameter { .. } => MessageType::SetInputGateParam,
            Self::InputGateInputGain { .. } => MessageType::SetInputGateInputGain,
            Self::OutputEqualizerEnable { .. } => MessageType::OutputEqualizerEnable,
            Self::OutputEqualizerParameter { .. } => MessageType::OutputEqualizerParam,
        }
    }

    pub const fn destination(self) -> u8 {
        match self {
            Self::MetronomeParameter { destination, .. }
            | Self::InputGateSource { destination, .. }
            | Self::InputGateEnabled { destination, .. }
            | Self::InputGateParameter { destination, .. }
            | Self::InputGateInputGain { destination, .. } => destination,
            Self::MetronomeDestination { .. }
            | Self::OutputEqualizerEnable { .. }
            | Self::OutputEqualizerParameter { .. } => 1,
            _ => 4,
        }
    }

    const fn total_bytes(self) -> usize {
        match self {
            Self::TempoChange { .. } | Self::MetronomeStatusRequest | Self::MidiClockStart => 16,
            Self::EnableUsbMidiClock { .. } | Self::EnableMidiClockIn { .. } => 17,
            Self::MidiClockCountAndTempo { .. } => 24,
            Self::UsbMidiOut { .. } => 28,
            _ => 20,
        }
    }

    fn validate(self) -> Result<(), Error> {
        match self {
            Self::MetronomeParameter { destination, .. } if ![1, 4].contains(&destination) => {
                Err(Error::Invalid("observed metronome destination"))
            }
            Self::InputGateEnabled { destination, .. } if destination > 1 => {
                Err(Error::Invalid("input gate enable destination"))
            }
            // Signed comparisons recovered in the writers; negative IDs remain raw.
            Self::InputGateParameter { parameter, .. } if parameter >= 4 => {
                Err(Error::Invalid("input gate parameter"))
            }
            Self::OutputEqualizerParameter { parameter, .. } if parameter > 27 => {
                Err(Error::Invalid("output equalizer parameter"))
            }
            Self::UsbMidiOut { event, .. }
                if ![9, 11, 12, 15].contains(&event.code)
                    || (event.code == 15 && (event.status != 0xf8 || event.data != [0, 0])) =>
            {
                Err(Error::Invalid("observed USB MIDI event"))
            }
            _ => Ok(()),
        }
    }

    /// Overwrite only fields stored by the recovered writer.
    fn write(self, bytes: &mut [u8; MAX_BYTES]) {
        bytes[0] = self.message_type() as u8;
        bytes[1] = self.destination();
        bytes[8..12].copy_from_slice(&(self.total_bytes() as u32).to_le_bytes());
        if !matches!(
            self,
            Self::MidiClockStart | Self::MidiClockCountAndTempo { .. }
        ) {
            bytes[2] = 0;
        }
        let (selector, word, body) = match self {
            Self::TempoChange { bpm } => (0, Some(bpm as u32), None),
            Self::MetronomeParameter {
                parameter,
                value_bits,
                ..
            } => (0, Some(parameter), Some(value_bits)),
            Self::MetronomeDestination { value_bits } => (0, Some(9), Some(value_bits)),
            Self::MetronomeSound => (0, Some(0), Some(0x3f800000)),
            Self::EnableInternalMidiClock { value }
            | Self::ResetInternalMidiClock { value }
            | Self::EnableMidiClockOut { value }
            | Self::OutputEqualizerEnable { value } => (0, Some(0), Some(value)),
            Self::InputGateSource {
                gate, word, source, ..
            } => (gate, Some(word), Some(source)),
            Self::InputGateEnabled { value, .. } => (0, None, Some(value)),
            Self::InputGateParameter {
                gate,
                parameter,
                value_bits,
                ..
            } => (gate, Some(parameter as u32), Some(value_bits)),
            Self::InputGateInputGain {
                gate, value_bits, ..
            } => (gate, Some(0), Some(value_bits)),
            Self::OutputEqualizerParameter {
                output,
                parameter,
                value_bits,
            } => (output, Some(parameter as u32), Some(value_bits)),
            Self::EnableUsbMidiClock { value } | Self::EnableMidiClockIn { value } => {
                bytes[16] = value;
                (0, Some(0), None)
            }
            Self::UsbMidiOut { delivery, event } => {
                bytes[16..21].copy_from_slice(&[
                    event.code,
                    0,
                    event.status,
                    event.data[0],
                    event.data[1],
                ]);
                match delivery {
                    UsbMidiDelivery::Immediate => (0, Some(0), None),
                    UsbMidiDelivery::Tagged(word) => (1, Some(word), None),
                }
            }
            Self::MidiClockCountAndTempo { tempo_bits, count } => {
                bytes[16..20].copy_from_slice(&tempo_bits.to_le_bytes());
                bytes[20..24].copy_from_slice(&count.to_le_bytes());
                return;
            }
            Self::MidiClockStart => return,
            Self::MetronomeStatusRequest => (0, None, None),
        };
        bytes[2] = selector;
        if let Some(word) = word {
            bytes[4..8].copy_from_slice(&word.to_le_bytes());
        }
        if let Some(body) = body {
            bytes[16..20].copy_from_slice(&body.to_le_bytes());
        }
    }
}

/// Strict frame with lossless retention of unstored header and USB tail bytes.
/// New frames zero-fill those bytes as owned policy, not recovered stock values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestFrame {
    request: ControlRequest,
    bytes: [u8; MAX_BYTES],
}

impl RequestFrame {
    pub fn new(request: ControlRequest) -> Result<Self, Error> {
        request.validate()?;
        let mut bytes = [0; MAX_BYTES];
        request.write(&mut bytes);
        Ok(Self { request, bytes })
    }

    pub fn request(&self) -> ControlRequest {
        self.request
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.request.total_bytes()]
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if !(HEADER_BYTES..=MAX_BYTES).contains(&bytes.len())
            || read_u32(bytes, 8) as usize != bytes.len()
        {
            return Err(Error::Invalid("control request size"));
        }
        let mut retained = [0; MAX_BYTES];
        retained[..bytes.len()].copy_from_slice(bytes);
        let destination = bytes[1];
        let selector = bytes[2];
        let word = read_u32(bytes, 4);
        // Read padded storage, then require the recovered exact size below.
        let value = read_u32(&retained, 16);
        use ControlRequest as R;
        let request = match MessageType::try_from(bytes[0])? {
            MessageType::TempoChange => R::TempoChange { bpm: word as i32 },
            MessageType::MetronomeParameter => R::MetronomeParameter {
                destination,
                parameter: word,
                value_bits: value,
            },
            MessageType::MetronomeStatusRequest => R::MetronomeStatusRequest,
            MessageType::MetronomeDestination => R::MetronomeDestination { value_bits: value },
            MessageType::MetronomeSound => R::MetronomeSound,
            MessageType::EnableUsbMidiClock => R::EnableUsbMidiClock {
                value: retained[16],
            },
            MessageType::EnableInternalMidiClock => R::EnableInternalMidiClock { value },
            MessageType::ResetInternalMidiClock => R::ResetInternalMidiClock { value },
            MessageType::EnableMidiClockOut => R::EnableMidiClockOut { value },
            MessageType::EnableMidiClockIn => R::EnableMidiClockIn {
                value: retained[16],
            },
            MessageType::MidiClockCountAndTempo => R::MidiClockCountAndTempo {
                tempo_bits: value,
                count: read_u32(&retained, 20),
            },
            MessageType::MidiClockStart => R::MidiClockStart,
            MessageType::UsbMidiOut => R::UsbMidiOut {
                delivery: match selector {
                    0 => UsbMidiDelivery::Immediate,
                    1 => UsbMidiDelivery::Tagged(word),
                    _ => return Err(Error::Invalid("USB MIDI selector")),
                },
                event: UsbMidiEvent {
                    code: retained[16],
                    status: retained[18],
                    data: [retained[19], retained[20]],
                },
            },
            MessageType::SetInputGateSource => R::InputGateSource {
                destination,
                gate: selector,
                word,
                source: value,
            },
            MessageType::SetInputGateEnabled => R::InputGateEnabled { destination, value },
            MessageType::SetInputGateParam => R::InputGateParameter {
                destination,
                gate: selector,
                parameter: word as i32,
                value_bits: value,
            },
            MessageType::SetInputGateInputGain => R::InputGateInputGain {
                destination,
                gate: selector,
                value_bits: value,
            },
            MessageType::OutputEqualizerEnable => R::OutputEqualizerEnable { value },
            MessageType::OutputEqualizerParam => R::OutputEqualizerParameter {
                output: selector,
                parameter: word as i32,
                value_bits: value,
            },
            _ => return Err(Error::Invalid("control request type")),
        };
        request.validate()?;
        if request.total_bytes() != bytes.len() {
            return Err(Error::Invalid("control request exact size"));
        }
        let mut checked = retained;
        request.write(&mut checked);
        if checked != retained {
            return Err(Error::Invalid("control request writer constants"));
        }
        Ok(Self {
            request,
            bytes: retained,
        })
    }
}

/// Driver slot count (VA 0x558ab8) and the bulk block it produces (VA 0x512c74).
pub const METRONOME_PARAMETER_COUNT: usize = 23;
pub const METRONOME_BLOCK_BYTES: usize = METRONOME_PARAMETER_COUNT * 4;
/// Slot the resend path zeroes in the block while the output destination or
/// pan slot was dirty; the real value is then sent as a separate 0x44 frame.
pub const METRONOME_ENABLE_INDEX: usize = 4;

/// The 23 raw driver slot values sent as one generic-framed message
/// (VA 0x512c74 → 0x51965c). The message byte is 0x46 and the core byte is 4,
/// so the wire shape is distinct from [`ControlRequest::MetronomeDestination`]
/// (destination 1, word 9, 20 bytes). Values are the driver's stored floats
/// without the dB, pan or destination transforms applied by the single-slot
/// senders. Which slots carry meaningful values is caller state; this codec
/// never fills a slot it was not given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetronomeParameterBlock {
    values_bits: [u32; METRONOME_PARAMETER_COUNT],
}

impl MetronomeParameterBlock {
    pub const fn new(values_bits: [u32; METRONOME_PARAMETER_COUNT]) -> Self {
        Self { values_bits }
    }

    pub const fn values_bits(&self) -> &[u32; METRONOME_PARAMETER_COUNT] {
        &self.values_bits
    }

    /// Generic header fields stored by VA 0x51965c for this call site.
    pub const fn metadata() -> MessageMetadata {
        MessageMetadata {
            message_type: MessageType::MetronomeDestination as u8,
            core: CoreId::Control,
            slot: 0,
            argument: 0,
        }
    }

    pub fn payload(&self) -> [u8; METRONOME_BLOCK_BYTES] {
        let mut bytes = [0; METRONOME_BLOCK_BYTES];
        for (chunk, bits) in bytes.chunks_exact_mut(4).zip(self.values_bits) {
            chunk.copy_from_slice(&bits.to_le_bytes());
        }
        bytes
    }

    pub fn from_payload(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != METRONOME_BLOCK_BYTES {
            return Err(Error::Invalid("metronome block size"));
        }
        Ok(Self::new(std::array::from_fn(|index| {
            read_u32(bytes, index * 4)
        })))
    }

    /// The single 108-byte frame: 92 bytes fit the 128-byte control-core
    /// fragment limit, so the stock sender takes its unfragmented path
    /// (marker 0 at +3, 108 at +8, 92 at +12).
    pub fn to_frame(&self) -> Vec<u8> {
        let payload = self.payload();
        let mut fragments = Fragmenter::new(Self::metadata(), &payload, CodecLimits::default())
            .expect("fixed 92-byte control payload is within limits");
        let frame = fragments
            .next()
            .expect("fragmenter yields one frame")
            .to_bytes();
        debug_assert!(fragments.next().is_none());
        frame
    }

    /// Strict inverse of [`MetronomeParameterBlock::to_frame`]: header
    /// metadata, `Single` marker and the 92-byte payload must all match.
    pub fn decode_frame(bytes: &[u8]) -> Result<Self, Error> {
        let fragment = Fragment::decode(bytes, CodecLimits::default())?;
        if fragment.metadata() != Self::metadata() || fragment.marker() != Marker::Single {
            return Err(Error::Invalid("metronome block frame header"));
        }
        Self::from_payload(fragment.payload())
    }
}

/// `MetronomeDriver::sendAllSounds` (VA 0x558554) always bundles 18 files.
pub const METRONOME_SOUND_COUNT: usize = 18;

/// Sound bundle uploaded to DSP core 2 as message 0x47 through the generic
/// fragmenting sender. Payload: LE u32 count, then one LE u32 per sound
/// holding `byte_length / 2`, then every sound's bytes in order. Stock asserts
/// `total == sum(lengths) + 4 * (count + 1)`.
///
/// Owned policy: exactly [`METRONOME_SOUND_COUNT`] sounds, every length even
/// (an odd length cannot survive the halved size word), and the payload within
/// [`CodecLimits::max_message_bytes`]. The codec never reads files and does not
/// know the stock file order; the caller supplies bytes already in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetronomeSoundBundle {
    sounds: Vec<Vec<u8>>,
}

impl MetronomeSoundBundle {
    pub fn new(sounds: Vec<Vec<u8>>, limits: CodecLimits) -> Result<Self, Error> {
        if sounds.len() != METRONOME_SOUND_COUNT {
            return Err(Error::Invalid("metronome sound count"));
        }
        let mut total = 4 * (METRONOME_SOUND_COUNT + 1);
        for sound in &sounds {
            if sound.len() % 2 != 0 {
                return Err(Error::Invalid("metronome sound byte length parity"));
            }
            if u32::try_from(sound.len() / 2).is_err() {
                return Err(Error::Overflow("metronome sound length word"));
            }
            total = total
                .checked_add(sound.len())
                .ok_or(Error::Overflow("metronome sound bundle"))?;
        }
        if total > limits.max_message_bytes {
            return Err(Error::Limit("metronome sound bundle"));
        }
        Ok(Self { sounds })
    }

    pub fn sounds(&self) -> &[Vec<u8>] {
        &self.sounds
    }

    pub const fn metadata() -> MessageMetadata {
        MessageMetadata {
            message_type: MessageType::MetronomeSound as u8,
            core: CoreId::Dsp(DspCore::Core2),
            slot: 0,
            argument: 0,
        }
    }

    pub fn payload_len(&self) -> usize {
        4 * (METRONOME_SOUND_COUNT + 1) + self.sounds.iter().map(Vec::len).sum::<usize>()
    }

    pub fn to_payload(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.payload_len());
        bytes.extend_from_slice(&(METRONOME_SOUND_COUNT as u32).to_le_bytes());
        for sound in &self.sounds {
            bytes.extend_from_slice(&((sound.len() / 2) as u32).to_le_bytes());
        }
        for sound in &self.sounds {
            bytes.extend_from_slice(sound);
        }
        bytes
    }

    /// Strict inverse of [`MetronomeSoundBundle::to_payload`]: count word,
    /// length words and total consumption must all agree.
    pub fn from_payload(bytes: &[u8], limits: CodecLimits) -> Result<Self, Error> {
        if bytes.len() > limits.max_message_bytes {
            return Err(Error::Limit("metronome sound bundle"));
        }
        let table = 4 * (METRONOME_SOUND_COUNT + 1);
        if bytes.len() < table || read_u32(bytes, 0) as usize != METRONOME_SOUND_COUNT {
            return Err(Error::Invalid("metronome sound count"));
        }
        let mut offset = table;
        let mut sounds = Vec::with_capacity(METRONOME_SOUND_COUNT);
        for index in 0..METRONOME_SOUND_COUNT {
            let length = (read_u32(bytes, 4 + index * 4) as usize)
                .checked_mul(2)
                .ok_or(Error::Overflow("metronome sound length word"))?;
            let end = offset
                .checked_add(length)
                .filter(|end| *end <= bytes.len())
                .ok_or(Error::Invalid("metronome sound length"))?;
            sounds.push(bytes[offset..end].to_vec());
            offset = end;
        }
        if offset != bytes.len() {
            return Err(Error::Invalid("metronome sound bundle trailing bytes"));
        }
        Self::new(sounds, limits)
    }

    /// Generic 2032-byte DSP fragments over an owned payload buffer.
    pub fn fragments(&self, limits: CodecLimits) -> Result<SoundFragments, Error> {
        let payload = self.to_payload();
        // Validate once with the borrowed fragmenter; the iterator below
        // re-creates it over the owned buffer.
        Fragmenter::new(Self::metadata(), &payload, limits)?;
        Ok(SoundFragments { payload, limits })
    }
}

/// Owns the serialized bundle so the fragments can outlive the borrow.
#[derive(Debug)]
pub struct SoundFragments {
    payload: Vec<u8>,
    limits: CodecLimits,
}

impl SoundFragments {
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub fn iter(&self) -> Fragmenter<'_> {
        Fragmenter::new(MetronomeSoundBundle::metadata(), &self.payload, self.limits)
            .expect("validated in MetronomeSoundBundle::fragments")
    }
}

/// The DSP → ARM status that answers [`ControlRequest::MetronomeStatusRequest`]
/// is response-namespace byte `0x19` (dispatcher VA 0x89cc60), decoded by
/// `crate::responses::MetronomeStatus`. Response byte `0x1a` (`SOC2SoundsReceived`)
/// is what makes stock send the 20-byte [`ControlRequest::MetronomeSound`]
/// trigger after a [`MetronomeSoundBundle`] upload (VA 0x513e7c).
#[cfg(test)]
mod tests;

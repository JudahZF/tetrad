//! Host contracts recovered from the pinned IR object, WAV importer, and driver.

use std::fmt;

use crate::factory::{DeviceCipher, FactoryIrError};

pub const MAX_IR_SAMPLES: usize = 1024;
/// Message-type byte 0x0b (`ReceiveData`); historical name kept for callers.
pub const IR_PARAMETER_ID: u32 = 11;
/// Last sender argument. VA 0x51965c uses it as "log a too-small receive
/// buffer", not as a fragment marker; historical name kept for callers.
pub const IR_FINAL_FLAG: u32 = 1;

/// Every operation in this list remains unsupported by this library.
pub const UNSUPPORTED_OPERATIONS: [UnsupportedIrOperation; 2] = [
    UnsupportedIrOperation::RawPointerOverload,
    UnsupportedIrOperation::IrConvolution,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UnsupportedIrOperation {
    /// `decode` has no key input. Use [`decode_with_cipher`].
    #[error("factory .bin decoding needs a device cipher; use decode_with_cipher")]
    FactoryBinDecoder,
    #[error("the raw-pointer load overload has ambiguous count units")]
    RawPointerOverload,
    #[error("the stock IR convolution and DSP ABI are unverified")]
    IrConvolution,
}

/// Both limits are explicit. `max_chunks` includes `fmt`, `data`, and unknown
/// chunks, including those after `data`. Raw input uses only `max_asset_bytes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrLimits {
    pub max_asset_bytes: usize,
    pub max_chunks: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavPolicy {
    /// Requires a first, 18-byte `fmt` chunk and scans from byte 38 without odd
    /// padding. Validates the recovered four fields; floors partial data words.
    /// RIFF identity/size, layout, uniqueness, and whole-file bounds are owned
    /// safety requirements, not claims about the stock validator.
    RecoveredHeader38,
    /// Walks RIFF chunks with odd padding; accepts 16/18-byte IEEE-float `fmt`.
    /// Also requires consistent byte rate/block alignment, zero extension size,
    /// and complete data words. These are deliberate compatibility improvements.
    StandardRiff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrInputFormat {
    /// Explicit caller classification, not detection of factory files.
    RawLeFloat32,
    Wav(WavPolicy),
    FactoryBin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrSourceFormat {
    /// Raw words have no recovered channel-count or sample-rate metadata.
    RawLeFloat32,
    WavMono48kFloat32,
    /// Decrypted 4,096-byte factory file. Mono 48 kHz is inferred from the
    /// cab/IR DSP rate; the file itself carries no metadata.
    FactoryMono48kFloat32,
}

impl IrSourceFormat {
    pub fn sample_rate_hz(self) -> Option<u32> {
        match self {
            Self::RawLeFloat32 => None,
            Self::WavMono48kFloat32 | Self::FactoryMono48kFloat32 => Some(48_000),
        }
    }

    pub fn channel_count(self) -> Option<u16> {
        match self {
            Self::RawLeFloat32 => None,
            Self::WavMono48kFloat32 | Self::FactoryMono48kFloat32 => Some(1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrProvenance {
    /// The caller selected the recovered type-1 product word contract.
    /// This does not authenticate the bytes as a stock product.
    RawProductType1,
    Wav(WavPolicy),
    /// Decrypted with a caller-injected device cipher. CTR has no MAC, so this
    /// records the decode path, not authenticity.
    FactoryBin,
}

/// Decoder output. Private storage guarantees at most 1,024 selected words.
/// Bit preservation includes NaN payloads, infinities, subnormals, and signed
/// zero; accepting these words is not validation for audio playback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrSamples {
    bits: Vec<u32>,
    original_sample_count: usize,
    ignored_partial_bytes: u8,
    provenance: IrProvenance,
}

impl IrSamples {
    pub fn bits(&self) -> &[u32] {
        &self.bits
    }

    /// Count of complete source words, before the 1,024-sample selection.
    pub fn original_sample_count(&self) -> usize {
        self.original_sample_count
    }

    /// Partial WAV bytes are reported separately and do not count as samples.
    pub fn is_truncated(&self) -> bool {
        self.original_sample_count > self.bits.len()
    }

    pub fn ignored_partial_bytes(&self) -> u8 {
        self.ignored_partial_bytes
    }

    pub fn provenance(&self) -> IrProvenance {
        self.provenance
    }

    pub fn source_format(&self) -> IrSourceFormat {
        match self.provenance {
            IrProvenance::RawProductType1 => IrSourceFormat::RawLeFloat32,
            IrProvenance::Wav(_) => IrSourceFormat::WavMono48kFloat32,
            IrProvenance::FactoryBin => IrSourceFormat::FactoryMono48kFloat32,
        }
    }

    /// Selected words as `f32`, for callers that do arithmetic (rendering,
    /// export, owned DSP). Bits are reinterpreted, never converted.
    pub fn samples(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.bits.iter().copied().map(f32::from_bits)
    }

    /// Selected words only, with no header, arithmetic, or zero-padding.
    pub fn to_le_bytes(&self) -> Vec<u8> {
        self.bits
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrChannel {
    Channel0,
    Channel1,
}

impl IrChannel {
    pub fn index(self) -> u8 {
        match self {
            Self::Channel0 => 0,
            Self::Channel1 => 1,
        }
    }
}

impl TryFrom<u32> for IrChannel {
    type Error = IrError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Channel0),
            1 => Ok(Self::Channel1),
            _ => Err(IrError::Channel(value)),
        }
    }
}

/// Setter selectors are separate from direct channel indices and WAV channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrSelector {
    Selector2,
    Selector10,
}

impl IrSelector {
    pub fn channel(self) -> IrChannel {
        match self {
            Self::Selector2 => IrChannel::Channel0,
            Self::Selector10 => IrChannel::Channel1,
        }
    }
}

impl TryFrom<u32> for IrSelector {
    type Error = IrError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            2 => Ok(Self::Selector2),
            10 => Ok(Self::Selector10),
            _ => Err(IrError::Selector(value)),
        }
    }
}

/// An inert description of the recovered sender arguments
/// `0x51965c(rig, core, slot, channel, payload, count*4, 0x0b, 1)`. This is not
/// a framed packet or a statement about empty-IR audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrTransferPlan {
    channel: IrChannel,
    payload: Vec<u8>,
}

impl IrTransferPlan {
    pub fn new(samples: &IrSamples, channel: IrChannel) -> Self {
        Self {
            channel,
            payload: samples.to_le_bytes(),
        }
    }

    /// The count-zero record for a missing product id.
    pub(crate) fn empty(channel: IrChannel) -> Self {
        Self {
            channel,
            payload: Vec::new(),
        }
    }

    pub fn channel(&self) -> IrChannel {
        self.channel
    }

    /// Generic-header argument: the channel index.
    pub fn argument(&self) -> u8 {
        self.channel.index()
    }

    /// The message-type byte 0x0b (`ReceiveData`). Earlier docs named this
    /// value "parameter 11"; [`Self::parameter_id`] is kept for compatibility.
    pub fn message_type(&self) -> u8 {
        IR_PARAMETER_ID as u8
    }

    pub fn parameter_id(&self) -> u32 {
        IR_PARAMETER_ID
    }

    /// See [`IR_FINAL_FLAG`]: the stock log-on-error argument, always 1.
    pub fn final_flag(&self) -> u32 {
        IR_FINAL_FLAG
    }

    pub fn sample_count(&self) -> usize {
        self.payload.len() / 4
    }

    pub fn byte_count(&self) -> usize {
        self.payload.len()
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavField {
    FormatTag,
    Channels,
    SampleRate,
    BitsPerSample,
    ByteRate,
    BlockAlign,
    ExtensionSize,
}

impl fmt::Display for WavField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::FormatTag => "format tag",
            Self::Channels => "channel count",
            Self::SampleRate => "sample rate",
            Self::BitsPerSample => "bits per sample",
            Self::ByteRate => "byte rate",
            Self::BlockAlign => "block alignment",
            Self::ExtensionSize => "extension size",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavChunkKind {
    Format,
    Data,
}

impl fmt::Display for WavChunkKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Format => "fmt",
            Self::Data => "data",
        })
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum IrError {
    #[error("IR asset has {actual} bytes, exceeding limit {limit}")]
    AssetLimit { actual: usize, limit: usize },
    #[error("raw IR byte length {bytes} is not divisible by four")]
    RawByteAlignment { bytes: usize },
    #[error("WAV header needs {needed} bytes, but only {actual} are present")]
    HeaderTruncated { needed: usize, actual: usize },
    #[error("unsupported WAV container signature {0:?}; only RIFF is supported")]
    UnsupportedContainer([u8; 4]),
    #[error("RIFF form is not WAVE")]
    NotWave,
    #[error("RIFF declares {declared_total} total bytes, but input has {actual}")]
    RiffSize {
        declared_total: usize,
        actual: usize,
    },
    #[error("WAV size or offset arithmetic overflow")]
    SizeOverflow,
    #[error("WAV exceeds chunk limit {limit}")]
    ChunkLimit { limit: usize },
    #[error("incomplete WAV chunk header at byte {offset}")]
    ChunkHeaderTruncated { offset: usize },
    #[error("incomplete WAV chunk payload at byte {offset}, declaring {bytes} bytes")]
    ChunkPayloadTruncated { offset: usize, bytes: u32 },
    #[error("missing RIFF odd-chunk padding byte at byte {offset}")]
    ChunkPaddingTruncated { offset: usize },
    #[error("recovered WAV layout requires an initial 18-byte fmt chunk ending at byte 38")]
    RecoveredHeaderLayout,
    #[error("unsupported WAV fmt payload size {0}; only 16 or 18 bytes are supported")]
    UnsupportedFormatSize(usize),
    #[error("unsupported WAV {field}: got {actual}, required {expected}")]
    WavField {
        field: WavField,
        actual: u32,
        expected: u32,
    },
    #[error("WAV data must follow the fmt chunk")]
    DataBeforeFormat,
    #[error("WAV is missing its {0} chunk")]
    MissingChunk(WavChunkKind),
    #[error("WAV contains more than one {0} chunk")]
    DuplicateChunk(WavChunkKind),
    #[error("standard WAV data byte length {bytes} is not divisible by four")]
    WavDataAlignment { bytes: usize },
    #[error("IR channel {0} is outside 0..=1")]
    Channel(u32),
    #[error("IR setter selector {0} is unsupported; only 2 and 10 are recovered")]
    Selector(u32),
    #[error(transparent)]
    Factory(FactoryIrError),
    #[error("unsupported IR operation: {0}")]
    Unsupported(UnsupportedIrOperation),
}

/// Decodes explicitly classified bytes. Limits never come from asset metadata.
/// Both WAV policies require an exact RIFF size, one fmt before one data chunk,
/// and complete chunks throughout the file, including after selected data.
/// Factory input returns Unsupported without examining its contents; use
/// [`decode_with_cipher`] for it.
pub fn decode(bytes: &[u8], format: IrInputFormat, limits: IrLimits) -> Result<IrSamples, IrError> {
    if format == IrInputFormat::FactoryBin {
        return Err(IrError::Unsupported(
            UnsupportedIrOperation::FactoryBinDecoder,
        ));
    }
    if bytes.len() > limits.max_asset_bytes {
        return Err(IrError::AssetLimit {
            actual: bytes.len(),
            limit: limits.max_asset_bytes,
        });
    }
    match format {
        IrInputFormat::RawLeFloat32 => {
            // Check the full source before selecting only the bounded prefix.
            if !bytes.len().is_multiple_of(4) {
                return Err(IrError::RawByteAlignment { bytes: bytes.len() });
            }
            Ok(select_samples(bytes, IrProvenance::RawProductType1))
        }
        IrInputFormat::Wav(policy) => decode_wav(bytes, policy, limits.max_chunks),
        IrInputFormat::FactoryBin => unreachable!("factory input returned before decoding"),
    }
}

/// Decodes any input. Factory input is decrypted with `cipher`, which must come
/// from [`DeviceCipher::seed_and_serial`] for stock factory files.
pub fn decode_with_cipher(
    bytes: &[u8],
    format: IrInputFormat,
    limits: IrLimits,
    cipher: &DeviceCipher,
) -> Result<IrSamples, IrError> {
    if format != IrInputFormat::FactoryBin {
        return decode(bytes, format, limits);
    }
    if bytes.len() > limits.max_asset_bytes {
        return Err(IrError::AssetLimit {
            actual: bytes.len(),
            limit: limits.max_asset_bytes,
        });
    }
    cipher.decode_factory_ir(bytes).map_err(IrError::Factory)
}

pub(crate) fn select_samples(bytes: &[u8], provenance: IrProvenance) -> IrSamples {
    let bits = bytes
        .chunks_exact(4)
        .take(MAX_IR_SAMPLES)
        .map(le_u32)
        .collect();
    IrSamples {
        bits,
        original_sample_count: bytes.len() / 4,
        ignored_partial_bytes: (bytes.len() % 4) as u8,
        provenance,
    }
}

fn checked_end(offset: usize, size: usize) -> Result<usize, IrError> {
    offset.checked_add(size).ok_or(IrError::SizeOverflow)
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn le_u16(bytes: &[u8]) -> u32 {
    u32::from(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn validate_format(bytes: &[u8], policy: WavPolicy) -> Result<(), IrError> {
    if !matches!(bytes.len(), 16 | 18) {
        return Err(IrError::UnsupportedFormatSize(bytes.len()));
    }
    let fields = [
        (WavField::FormatTag, le_u16(&bytes[0..2]), 3),
        (WavField::Channels, le_u16(&bytes[2..4]), 1),
        (WavField::SampleRate, le_u32(&bytes[4..8]), 48_000),
        (WavField::BitsPerSample, le_u16(&bytes[14..16]), 32),
    ];
    for (field, actual, expected) in fields {
        if actual != expected {
            return Err(IrError::WavField {
                field,
                actual,
                expected,
            });
        }
    }
    if policy == WavPolicy::StandardRiff {
        let fields = [
            (WavField::ByteRate, le_u32(&bytes[8..12]), 192_000),
            (WavField::BlockAlign, le_u16(&bytes[12..14]), 4),
            (
                WavField::ExtensionSize,
                if bytes.len() == 18 {
                    le_u16(&bytes[16..18])
                } else {
                    0
                },
                0,
            ),
        ];
        for (field, actual, expected) in fields {
            if actual != expected {
                return Err(IrError::WavField {
                    field,
                    actual,
                    expected,
                });
            }
        }
    }
    Ok(())
}

fn decode_wav(bytes: &[u8], policy: WavPolicy, max_chunks: usize) -> Result<IrSamples, IrError> {
    if bytes.len() < 12 {
        return Err(IrError::HeaderTruncated {
            needed: 12,
            actual: bytes.len(),
        });
    }
    if &bytes[..4] != b"RIFF" {
        return Err(IrError::UnsupportedContainer([
            bytes[0], bytes[1], bytes[2], bytes[3],
        ]));
    }
    if &bytes[8..12] != b"WAVE" {
        return Err(IrError::NotWave);
    }
    let declared_size = usize::try_from(le_u32(&bytes[4..8])).map_err(|_| IrError::SizeOverflow)?;
    let declared_total = checked_end(declared_size, 8)?;
    if declared_total != bytes.len() {
        return Err(IrError::RiffSize {
            declared_total,
            actual: bytes.len(),
        });
    }

    let mut offset = 12;
    let mut chunk_count = 0_usize;
    let mut format_seen = false;
    let mut data = None;
    if policy == WavPolicy::RecoveredHeader38 {
        if bytes.len() < 38 {
            return Err(IrError::HeaderTruncated {
                needed: 38,
                actual: bytes.len(),
            });
        }
        if &bytes[12..16] != b"fmt " || le_u32(&bytes[16..20]) != 18 {
            return Err(IrError::RecoveredHeaderLayout);
        }
        if max_chunks == 0 {
            return Err(IrError::ChunkLimit { limit: max_chunks });
        }
        validate_format(&bytes[20..38], policy)?;
        // Intentionally fixed, matching the recovered extractor rather than a
        // generalized RIFF format-size-derived starting position.
        offset = 38;
        chunk_count = 1;
        format_seen = true;
    }

    while offset < bytes.len() {
        if chunk_count >= max_chunks {
            return Err(IrError::ChunkLimit { limit: max_chunks });
        }
        chunk_count = checked_end(chunk_count, 1)?;
        let payload_start = checked_end(offset, 8)?;
        if payload_start > bytes.len() {
            return Err(IrError::ChunkHeaderTruncated { offset });
        }
        let id = &bytes[offset..checked_end(offset, 4)?];
        let declared_bytes = le_u32(&bytes[checked_end(offset, 4)?..payload_start]);
        let size = usize::try_from(declared_bytes).map_err(|_| IrError::SizeOverflow)?;
        let payload_end = checked_end(payload_start, size)?;
        if payload_end > bytes.len() {
            return Err(IrError::ChunkPayloadTruncated {
                offset,
                bytes: declared_bytes,
            });
        }
        let payload = &bytes[payload_start..payload_end];
        let pad = if policy == WavPolicy::StandardRiff {
            size % 2
        } else {
            0
        };
        let next_offset = checked_end(payload_end, pad)?;
        if next_offset > bytes.len() {
            return Err(IrError::ChunkPaddingTruncated {
                offset: payload_end,
            });
        }
        match id {
            b"fmt " => {
                if format_seen {
                    return Err(IrError::DuplicateChunk(WavChunkKind::Format));
                }
                validate_format(payload, policy)?;
                format_seen = true;
            }
            b"data" => {
                if !format_seen {
                    return Err(IrError::DataBeforeFormat);
                }
                if data.is_some() {
                    return Err(IrError::DuplicateChunk(WavChunkKind::Data));
                }
                if policy == WavPolicy::StandardRiff && !size.is_multiple_of(4) {
                    return Err(IrError::WavDataAlignment { bytes: size });
                }
                data = Some(payload);
            }
            _ => {}
        }
        offset = next_offset;
    }
    if !format_seen {
        return Err(IrError::MissingChunk(WavChunkKind::Format));
    }
    let data = data.ok_or(IrError::MissingChunk(WavChunkKind::Data))?;
    Ok(select_samples(data, IrProvenance::Wav(policy)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_arithmetic_rejects_overflow_on_every_pointer_width() {
        assert_eq!(checked_end(usize::MAX, 1), Err(IrError::SizeOverflow));
        assert_eq!(checked_end(usize::MAX - 7, 8), Err(IrError::SizeOverflow));
        assert_eq!(checked_end(usize::MAX - 8, 8), Ok(usize::MAX));
    }
}

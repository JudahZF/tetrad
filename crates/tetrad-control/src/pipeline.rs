//! Stock payload stages between report reassembly and the dispatcher, and
//! between a typed reply and its report bodies.
//!
//! Receive (`0x290ae8`): drop types above 72, drop blocked types, undo the
//! transform, then inflate unless the type is Version (10) or
//! ResetCommsBuffers (52). Send (template copy `0xee2ec`): skip blocked types,
//! stamp the request ID, deflate payloads above 10 KiB when allowed, apply
//! the transform, then append the 8-byte trailer and fragment.
//!
//! zlib/gzip and the transform cipher are injected: the stock key lives in
//! the session and never enters this crate.

use std::collections::BTreeSet;

use crate::codecs::{CodecError, CodecLimits, ControlMessage, CortexMessageType, SceneMessage};
use crate::{Envelope, Limits, REPORT_BYTES};

/// Stock deflate threshold: `size > 0x2800` in the sender template.
pub const COMPRESS_ABOVE_BYTES: usize = 0x2800;
/// Highest dispatchable type; larger trailer types are logged and dropped.
pub const MAX_MESSAGE_TYPE: u32 = 72;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StageFailure {
    #[error("input is malformed")]
    Malformed,
    #[error("output exceeds {0} bytes")]
    Limit(usize),
}

/// zlib stages. Stock inflate uses windowBits 47 (zlib or gzip header,
/// `0x462318`); stock deflate uses level 1 with a gzip wrapper (windowBits 31,
/// `0x462a28`). Any output that a windowBits-47 inflater accepts is valid.
pub trait Compression {
    fn inflate(&self, input: &[u8], max_output: usize) -> Result<Vec<u8>, StageFailure>;
    fn deflate(&self, input: &[u8]) -> Result<Vec<u8>, StageFailure>;
}

/// The keyed payload transform (`0x6acc9c` decode, `0x6ade54` encode). The
/// implementation owns the session key.
pub trait Transform {
    fn decode(&self, input: &[u8]) -> Result<Vec<u8>, StageFailure>;
    fn encode(&self, input: &[u8]) -> Result<Vec<u8>, StageFailure>;
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PipelineError {
    #[error(transparent)]
    Report(#[from] crate::Error),
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("type {0} uses a transform, but none is installed")]
    TransformUnavailable(u32),
    #[error("type {0} is compressed, but no compression is installed")]
    CompressionUnavailable(u32),
    #[error("type {message_type} transform failed: {failure}")]
    Transform {
        message_type: u32,
        failure: StageFailure,
    },
    #[error("type {message_type} compression failed: {failure}")]
    Compression {
        message_type: u32,
        failure: StageFailure,
    },
}

/// Injected stages. `None` refuses payloads that need the stage.
#[derive(Clone, Copy, Default)]
pub struct Stages<'a> {
    pub compression: Option<&'a dyn Compression>,
    pub transform: Option<&'a dyn Transform>,
}

/// Why a complete envelope did not reach the dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dropped {
    /// Trailer type above 72 (logged by stock).
    UnknownType(u32),
    /// Present in the receiver's blocked-type set (`receiver + 0x140`).
    Blocked(CortexMessageType),
}

/// A plain payload ready for the dispatcher. Type 0 is admitted, as in stock;
/// the dispatcher has no case for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    pub message_type: u32,
    pub payload: Vec<u8>,
}

/// Stock per-call send flags (`*param_5`): bit 0 transform, bit 1 compressible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SendOptions {
    pub transform: bool,
    pub compressible: bool,
}

/// Receive and send stage configuration that the session owns.
#[derive(Debug, Clone, Default)]
pub struct PipelineConfig {
    pub blocked_inbound: BTreeSet<u32>,
    pub blocked_outbound: BTreeSet<u32>,
    /// Sender flag `+0x15e`: the peer may receive compressed payloads.
    pub peer_accepts_compression: bool,
    /// Bound for inflated payloads (owned policy).
    pub max_inflated_bytes: usize,
}

impl PipelineConfig {
    /// Stock sender `+0x15e` is set at construction and never cleared, so
    /// compression is on; clear it when no `Compression` stage is installed.
    pub fn new(max_inflated_bytes: usize) -> Self {
        Self {
            max_inflated_bytes,
            peer_accepts_compression: true,
            ..Self::default()
        }
    }

    /// Applies the stock receive stages to one reassembled envelope.
    pub fn receive(
        &self,
        envelope: Envelope,
        stages: Stages<'_>,
    ) -> Result<Result<Inbound, Dropped>, PipelineError> {
        let message_type = envelope.message_type;
        if message_type > MAX_MESSAGE_TYPE {
            return Ok(Err(Dropped::UnknownType(message_type)));
        }
        if self.blocked_inbound.contains(&message_type) {
            let known = CortexMessageType::try_from(message_type).expect("type at most 72");
            return Ok(Err(Dropped::Blocked(known)));
        }
        let mut payload = envelope.payload;
        if envelope.transform != 0 && !payload.is_empty() {
            let transform = stages
                .transform
                .ok_or(PipelineError::TransformUnavailable(message_type))?;
            payload = transform
                .decode(&payload)
                .map_err(|failure| PipelineError::Transform {
                    message_type,
                    failure,
                })?;
        }
        if envelope.decompress != 0 && !matches!(message_type, 10 | 52) && !payload.is_empty() {
            let compression = stages
                .compression
                .ok_or(PipelineError::CompressionUnavailable(message_type))?;
            payload = compression
                .inflate(&payload, self.max_inflated_bytes)
                .map_err(|failure| PipelineError::Compression {
                    message_type,
                    failure,
                })?;
        }
        Ok(Ok(Inbound {
            message_type,
            payload,
        }))
    }

    /// Encodes one outbound message. Returns `None` for a blocked type, which
    /// stock skips without error. `current_request_id` is the dispatcher's
    /// request context; `explicit` overrides it, as the sender's optional
    /// request-ID argument does.
    pub fn send<M: Outgoing>(
        &self,
        mut message: M,
        options: SendOptions,
        request: RequestStamp,
        limits: SendLimits,
        stages: Stages<'_>,
    ) -> Result<Option<Vec<[u8; REPORT_BYTES]>>, PipelineError> {
        let message_type = message.message_type();
        if self.blocked_outbound.contains(&message_type) {
            return Ok(None);
        }
        if let Some(id) = request.resolve(message_type) {
            *message.request_id_mut() = Some(id);
        }
        let mut payload = message.encode(limits.payload)?;
        let transform = options.transform && !payload.is_empty();
        let compress = options.compressible
            && payload.len() > COMPRESS_ABOVE_BYTES
            && self.peer_accepts_compression;
        if compress {
            let compression = stages
                .compression
                .ok_or(PipelineError::CompressionUnavailable(message_type))?;
            payload =
                compression
                    .deflate(&payload)
                    .map_err(|failure| PipelineError::Compression {
                        message_type,
                        failure,
                    })?;
        }
        if transform {
            let stage = stages
                .transform
                .ok_or(PipelineError::TransformUnavailable(message_type))?;
            payload = stage
                .encode(&payload)
                .map_err(|failure| PipelineError::Transform {
                    message_type,
                    failure,
                })?;
        }
        let envelope = Envelope {
            message_type,
            transform: u8::from(transform),
            decompress: u8::from(compress),
            // Stock leaves two uninitialized padding bytes; zero is chosen.
            opaque: [0; 2],
            payload,
        };
        Ok(Some(envelope.reports(limits.reports)?))
    }
}

impl PipelineConfig {
    /// The priority path (`0x29abb0` with queue `+0x58`), used by ModelRepo,
    /// Version, the Version GenericError, the ResetCommsBuffers echo and the
    /// teardown Connection message: no blocked set, no request stamp, no
    /// transform and no deflate. The caller sets any request ID itself.
    pub fn send_direct<M: Outgoing>(
        &self,
        message: M,
        limits: SendLimits,
    ) -> Result<Vec<[u8; REPORT_BYTES]>, PipelineError> {
        let envelope = Envelope {
            message_type: message.message_type(),
            transform: 0,
            decompress: 0,
            opaque: [0; 2],
            payload: message.encode(limits.payload)?,
        };
        Ok(envelope.reports(limits.reports)?)
    }
}

/// Stock per-type sender flags. Transform: CloudLogin, the three HTTP
/// forwards, CloudBackup and License. Compressible: File and RecallPreset; ModelPreset only
/// for its READ list (`0x1165ec` flag 2; pushes, loaded replies and the resync pass 0).
/// Every other sender passes 0.
pub fn stock_send_options(message_type: CortexMessageType) -> SendOptions {
    use CortexMessageType as T;
    SendOptions {
        transform: matches!(
            message_type,
            T::CloudLogin
                | T::ProductForward
                | T::BackupsForward
                | T::LogsForward
                | T::CloudBackup
                | T::License
        ),
        compressible: matches!(message_type, T::File | T::RecallPreset),
    }
}

/// A typed message that the sender can stamp and encode.
pub trait Outgoing {
    fn message_type(&self) -> u32;
    fn request_id_mut(&mut self) -> &mut Option<u64>;
    fn encode(&self, limits: CodecLimits) -> Result<Vec<u8>, CodecError>;
}

impl Outgoing for ControlMessage {
    fn message_type(&self) -> u32 {
        ControlMessage::message_type(self) as u32
    }

    fn request_id_mut(&mut self) -> &mut Option<u64> {
        ControlMessage::request_id_mut(self)
    }

    fn encode(&self, limits: CodecLimits) -> Result<Vec<u8>, CodecError> {
        ControlMessage::encode(self, limits)
    }
}

impl Outgoing for SceneMessage {
    fn message_type(&self) -> u32 {
        CortexMessageType::Scene as u32
    }

    fn request_id_mut(&mut self) -> &mut Option<u64> {
        &mut self.request_id
    }

    fn encode(&self, limits: CodecLimits) -> Result<Vec<u8>, CodecError> {
        SceneMessage::encode(self, limits)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SendLimits {
    pub payload: CodecLimits,
    pub reports: Limits,
}

/// Request-ID source for one send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequestStamp {
    /// Sender argument; a nonzero value always wins.
    pub explicit: u64,
    /// Dispatcher context (`USBHIDController + 0x2a8`): the request's ID while
    /// its receiver runs, otherwise zero.
    pub current: u64,
}

impl RequestStamp {
    /// Stock rule: a nonzero explicit ID is always set. Otherwise a nonzero
    /// current ID is set, except on Confirmation (47) and the Product, Backups
    /// and Logs forwards (29–31). Zero means the field stays absent.
    pub fn resolve(self, message_type: u32) -> Option<u64> {
        if self.explicit != 0 {
            return Some(self.explicit);
        }
        (self.current != 0 && message_type != 47 && !(29..=31).contains(&message_type))
            .then_some(self.current)
    }
}

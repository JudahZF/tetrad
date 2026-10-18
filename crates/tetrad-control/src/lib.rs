//! Cortex Control (USB HID link to the desktop app): report framing, typed
//! bounded payload codecs, and the stock ZenUI dispatcher.
//!
//! `link::ControlLink` runs the stock receive stages, session rules and the
//! per-type receivers (`dispatch`) against an app-implemented
//! `device::DeviceModel`, and returns encoded 128-byte report bodies plus
//! session events and notices. There is no transport here: report IDs belong
//! to hidapi, and the DSP HID frames carry the bodies on the device.
//!
//! The older scene modules keep their APIs: restricted SceneMessage dispatch
//! (`scene`, `application`) and lossless preset scene-label/color prefix
//! installation and bounded export via tetrad-preset (`preset_working`).

pub mod application;
pub mod block;
pub mod codecs;
pub mod device;
mod dispatch;
pub mod link;
mod outbox;
pub mod pipeline;
pub mod preset_working;
pub mod scene;
pub mod session;
mod wire;
mod xxtea;

pub const REPORT_BYTES: usize = 128;
pub const PAYLOAD_BYTES: usize = 126;
const FIRST: u16 = 0x4000;
const LAST: u16 = 0x8000;
const LENGTH: u16 = 0x3fff;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error(transparent)]
    Workflow(#[from] tetrad_core::workflow::WorkflowError),
    #[error("report body must have exactly 128 bytes")]
    ReportLength,
    #[error("fragment payload length exceeds 126 bytes")]
    FragmentLength,
    #[error("continuation report has no first report")]
    Sequence,
    #[error("message is missing its eight-byte trailer")]
    Trailer,
    #[error("control message exceeds {0} bytes")]
    MessageLimit(usize),
    #[error("control message exceeds {0} reports")]
    ReportLimit(usize),
    #[error("control message is incomplete at end of input")]
    Incomplete,
    #[error("message transform {0} is not recovered")]
    UnsupportedTransform(u8),
    #[error("compressed payload handling is not implemented")]
    UnsupportedCompression,
    #[error("scene index {0} is outside 0 through 7")]
    Scene(u32),
    #[error("message type {0} is not a SceneMessage")]
    UnsupportedSceneType(u32),
    #[error("scene action {0} is not supported by local dispatch")]
    UnsupportedSceneAction(u64),
    #[error("scene payload exceeds {0} bytes")]
    ScenePayloadLimit(usize),
    #[error("scene payload exceeds {0} fields")]
    SceneFieldLimit(usize),
    #[error("scene payload exceeds {0} nested groups")]
    SceneDepthLimit(usize),
    #[error("scene group depth {0} exceeds the supported maximum of 64")]
    SceneDepthConfiguration(usize),
    #[error("scene field {field} value {value} exceeds uint32")]
    SceneInteger { field: u32, value: u64 },
    #[error("invalid scene protobuf at byte {offset}: {problem}")]
    SceneWire {
        offset: usize,
        problem: scene::SceneWireError,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Completed message bytes, including the trailer.
    pub max_message_bytes: usize,
    pub max_reports: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message_bytes: 16 * 1024 * 1024,
            max_reports: 200_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub message_type: u32,
    pub transform: u8,
    pub decompress: u8,
    pub opaque: [u8; 2],
    pub payload: Vec<u8>,
}

impl Envelope {
    /// Returns only payloads whose processing is currently supported. Types
    /// Version (10) and ResetCommsBuffers (52) bypass stock decompression.
    pub fn plain_payload(&self) -> Result<&[u8]> {
        if self.transform != 0 {
            return Err(Error::UnsupportedTransform(self.transform));
        }
        if self.decompress != 0 && !matches!(self.message_type, 10 | 52) {
            return Err(Error::UnsupportedCompression);
        }
        Ok(&self.payload)
    }

    pub fn reports(&self, limits: Limits) -> Result<Vec<[u8; REPORT_BYTES]>> {
        let size = self
            .payload
            .len()
            .checked_add(8)
            .ok_or(Error::MessageLimit(limits.max_message_bytes))?;
        if size > limits.max_message_bytes {
            return Err(Error::MessageLimit(limits.max_message_bytes));
        }
        let count = size.div_ceil(PAYLOAD_BYTES);
        if count > limits.max_reports {
            return Err(Error::ReportLimit(limits.max_reports));
        }
        let mut message = Vec::with_capacity(size);
        message.extend_from_slice(&self.payload);
        message.extend_from_slice(&self.message_type.to_le_bytes());
        message.extend_from_slice(&[self.transform, self.decompress]);
        message.extend_from_slice(&self.opaque);
        let mut reports = Vec::with_capacity(count);
        for (index, chunk) in message.chunks(PAYLOAD_BYTES).enumerate() {
            let mut report = [0; REPORT_BYTES];
            let prefix = chunk.len() as u16
                | if index == 0 { FIRST } else { 0 }
                | if index + 1 == count { LAST } else { 0 };
            report[..2].copy_from_slice(&prefix.to_le_bytes());
            report[2..2 + chunk.len()].copy_from_slice(chunk);
            reports.push(report);
        }
        Ok(reports)
    }
}

#[derive(Debug)]
pub struct Reassembler {
    limits: Limits,
    bytes: Vec<u8>,
    active: bool,
    reports: usize,
}

impl Reassembler {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            bytes: Vec::new(),
            active: false,
            reports: 0,
        }
    }

    /// FIRST intentionally resets accumulation as in the recovered receiver.
    /// Any malformed report clears partial state, so unrelated messages cannot
    /// inherit bytes from a rejected input. Padding is outside declared payload.
    pub fn push(&mut self, report: &[u8]) -> Result<Option<Envelope>> {
        let result = self.push_checked(report);
        if result.is_err() {
            self.reset();
        }
        result
    }

    fn push_checked(&mut self, report: &[u8]) -> Result<Option<Envelope>> {
        if report.len() != REPORT_BYTES {
            return Err(Error::ReportLength);
        }
        let prefix = u16::from_le_bytes([report[0], report[1]]);
        let length = usize::from(prefix & LENGTH);
        if length > PAYLOAD_BYTES {
            return Err(Error::FragmentLength);
        }
        if prefix & FIRST != 0 {
            self.reset();
            self.active = true;
        }
        if !self.active {
            return Err(Error::Sequence);
        }
        if self.reports >= self.limits.max_reports {
            return Err(Error::ReportLimit(self.limits.max_reports));
        }
        let size = self
            .bytes
            .len()
            .checked_add(length)
            .ok_or(Error::MessageLimit(self.limits.max_message_bytes))?;
        if size > self.limits.max_message_bytes {
            return Err(Error::MessageLimit(self.limits.max_message_bytes));
        }
        self.reports += 1;
        self.bytes.extend_from_slice(&report[2..2 + length]);
        if prefix & LAST == 0 {
            return Ok(None);
        }
        if self.bytes.len() < 8 {
            return Err(Error::Trailer);
        }
        let trailer = self.bytes.split_off(self.bytes.len() - 8);
        let envelope = Envelope {
            message_type: u32::from_le_bytes(trailer[..4].try_into().expect("eight-byte trailer")),
            transform: trailer[4],
            decompress: trailer[5],
            opaque: [trailer[6], trailer[7]],
            payload: std::mem::take(&mut self.bytes),
        };
        self.reset();
        Ok(Some(envelope))
    }

    pub fn finish(&self) -> Result<()> {
        if self.active {
            Err(Error::Incomplete)
        } else {
            Ok(())
        }
    }

    pub fn reset(&mut self) {
        self.bytes.clear();
        self.active = false;
        self.reports = 0;
    }
}

/// A restricted in-process scene operation, not the complete application policy.
#[derive(Debug, Clone, Copy)]
pub enum SceneCommand {
    Read,
    Update { selected_scene: Option<u32> },
}

#[derive(Debug, Default)]
pub struct SceneState {
    selected: u32,
}

impl SceneState {
    pub fn apply(&mut self, command: SceneCommand) -> Result<u32> {
        if let SceneCommand::Update { selected_scene } = command {
            let selected = selected_scene.unwrap_or(0);
            if selected > 7 {
                return Err(Error::Scene(selected));
            }
            self.selected = selected;
        }
        Ok(self.selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(payload: Vec<u8>) -> Envelope {
        Envelope {
            message_type: 13,
            transform: 0,
            decompress: 0,
            opaque: [0; 2],
            payload,
        }
    }

    #[test]
    fn recovered_scene_wire_vector_includes_trailer_not_report_id() {
        let message = envelope(vec![8, 1, 16, 42, 24, 1]);
        let reports = message.reports(Limits::default()).unwrap();
        assert_eq!(
            &reports[0][..16],
            &[0x0e, 0xc0, 8, 1, 16, 42, 24, 1, 13, 0, 0, 0, 0, 0, 0, 0]
        );
        let mut decoder = Reassembler::new(Limits::default());
        assert_eq!(decoder.push(&reports[0]).unwrap(), Some(message));
        decoder.finish().unwrap();
    }

    #[test]
    fn fragments_roundtrip_empty_boundary_and_split_trailers() {
        for size in [0, 117, 118, 119, 126, 244, 245, 246, 247, 1024] {
            let mut message = envelope((0..size).map(|i| i as u8).collect());
            message.opaque = [0xab, 0xcd];
            let reports = message.reports(Limits::default()).unwrap();
            let mut decoder = Reassembler::new(Limits::default());
            let mut completed = None;
            for report in reports {
                completed = decoder.push(&report).unwrap();
            }
            assert_eq!(completed, Some(message));
            decoder.finish().unwrap();
        }
    }

    #[test]
    fn first_resets_and_invalid_sequences_cannot_leak_partial_payloads() {
        let reports = envelope(vec![5; 300]).reports(Limits::default()).unwrap();
        let single = envelope(vec![9]).reports(Limits::default()).unwrap()[0];
        let mut decoder = Reassembler::new(Limits::default());
        assert_eq!(decoder.push(&reports[1]), Err(Error::Sequence));
        assert!(decoder.push(&reports[0]).unwrap().is_none());
        assert_eq!(decoder.finish(), Err(Error::Incomplete));
        assert_eq!(decoder.push(&single).unwrap(), Some(envelope(vec![9])));
        assert!(decoder.push(&reports[0]).unwrap().is_none());
        assert_eq!(decoder.push(&[0; 127]), Err(Error::ReportLength));
        assert_eq!(decoder.push(&reports[1]), Err(Error::Sequence));
        let mut bad = [0; REPORT_BYTES];
        bad[..2].copy_from_slice(&(FIRST | LAST | 127).to_le_bytes());
        assert_eq!(decoder.push(&bad), Err(Error::FragmentLength));
        bad[..2].copy_from_slice(&(FIRST | LAST | 7).to_le_bytes());
        assert_eq!(decoder.push(&bad), Err(Error::Trailer));
    }

    #[test]
    fn message_and_report_limits_apply_on_both_encoding_and_receiving() {
        let message = envelope(vec![0; 119]);
        let limits = Limits {
            max_message_bytes: 126,
            max_reports: 2,
        };
        assert_eq!(message.reports(limits), Err(Error::MessageLimit(126)));
        let reports = message.reports(Limits::default()).unwrap();
        let mut decoder = Reassembler::new(limits);
        assert!(decoder.push(&reports[0]).unwrap().is_none());
        assert_eq!(decoder.push(&reports[1]), Err(Error::MessageLimit(126)));
        assert!(decoder.finish().is_ok());
        let limits = Limits {
            max_message_bytes: 1000,
            max_reports: 1,
        };
        assert_eq!(message.reports(limits), Err(Error::ReportLimit(1)));
        let mut decoder = Reassembler::new(limits);
        decoder.push(&reports[0]).unwrap();
        assert_eq!(decoder.push(&reports[1]), Err(Error::ReportLimit(1)));
    }

    #[test]
    fn transforms_and_compression_are_not_silently_treated_as_plaintext() {
        let mut message = envelope(vec![1]);
        message.transform = 1;
        assert_eq!(message.plain_payload(), Err(Error::UnsupportedTransform(1)));
        message.transform = 0;
        message.decompress = 1;
        assert_eq!(message.plain_payload(), Err(Error::UnsupportedCompression));
        message.message_type = 10;
        assert_eq!(message.plain_payload().unwrap(), &[1]);
        message.message_type = 52;
        assert_eq!(message.plain_payload().unwrap(), &[1]);
    }

    #[test]
    fn scene_update_default_bounds_read_and_failed_update_are_explicit() {
        let mut scene = SceneState::default();
        assert_eq!(scene.apply(SceneCommand::Read).unwrap(), 0);
        assert_eq!(
            scene
                .apply(SceneCommand::Update {
                    selected_scene: Some(7)
                })
                .unwrap(),
            7
        );
        assert_eq!(
            scene.apply(SceneCommand::Update {
                selected_scene: Some(8)
            }),
            Err(Error::Scene(8))
        );
        assert_eq!(scene.apply(SceneCommand::Read).unwrap(), 7);
        assert_eq!(
            scene
                .apply(SceneCommand::Update {
                    selected_scene: None
                })
                .unwrap(),
            0
        );
    }
}

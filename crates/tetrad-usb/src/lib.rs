//! The Tetrad link over the Quad Cortex USB HID interface (`docs/usb.md`).
//!
//! The stock SOC2 firmware owns the USB port. It passes every 128-byte HID report between the
//! host and Tetrad unchanged, next to the Cortex Control reports (C, USB probe 2026-10-22).
//! A Tetrad report starts with a zero stock prefix: no FIRST, no LAST, length 0. A stock
//! receiver either refuses it (no message in progress) or adds no bytes to the message in
//! progress (C, `tetrad_control` report decoder), so Cortex Control keeps working (O).
//!
//! Report: `00 00 | "TTRD" | protocol | flags | channel | length | payload` ([`PAYLOAD_BYTES`]).
//! A message is the payload of the reports from START to END on one channel. Its bytes are
//! the JSON header length (u32 LE), the JSON header and a binary body. Protocol 1 uses
//! channel 0 only: one [`Request`] from the host, one [`Reply`] from the device.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(feature = "host")]
pub mod host;

pub const REPORT_BYTES: usize = 128;
pub const MAGIC: [u8; 4] = *b"TTRD";
pub const PROTOCOL: u8 = 1;
const HEAD_BYTES: usize = 10;
pub const PAYLOAD_BYTES: usize = REPORT_BYTES - HEAD_BYTES;
/// The largest body of an [`Op::InstallChunk`].
pub const MAX_CHUNK_BYTES: usize = 64 << 10;
const START: u8 = 1;
const END: u8 = 2;

pub type Report = [u8; REPORT_BYTES];

/// A report body (without the HID report ID) of this protocol.
pub fn is_tetrad(report: &[u8]) -> bool {
    report.len() == REPORT_BYTES && report[..2] == [0, 0] && report[2..6] == MAGIC
}

/// The reports of one message on `channel`; an empty message is one START|END report.
pub fn reports(channel: u8, message: &[u8]) -> Vec<Report> {
    let chunks: Vec<&[u8]> = if message.is_empty() {
        vec![&[]]
    } else {
        message.chunks(PAYLOAD_BYTES).collect()
    };
    let last = chunks.len() - 1;
    (chunks.iter().enumerate())
        .map(|(index, chunk)| {
            let mut report = [0; REPORT_BYTES];
            report[2..6].copy_from_slice(&MAGIC);
            report[6] = PROTOCOL;
            report[7] = if index == 0 { START } else { 0 } | if index == last { END } else { 0 };
            report[8] = channel;
            report[9] = chunk.len() as u8;
            report[HEAD_BYTES..HEAD_BYTES + chunk.len()].copy_from_slice(chunk);
            report
        })
        .collect()
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("not a Tetrad report")]
    NotTetrad,
    #[error("protocol {0} is not {PROTOCOL}")]
    Protocol(u8),
    #[error("payload length {0} is over {PAYLOAD_BYTES}")]
    Length(u8),
    #[error("channel {0}: a report without a START report")]
    Sequence(u8),
    #[error("a message is over {0} bytes")]
    TooLarge(usize),
}

/// Joins reports into messages, one partial message per channel.
#[derive(Debug)]
pub struct Reassembler {
    max_message_bytes: usize,
    partial: BTreeMap<u8, Vec<u8>>,
}

impl Reassembler {
    pub fn new(max_message_bytes: usize) -> Self {
        Self {
            max_message_bytes,
            partial: BTreeMap::new(),
        }
    }

    /// A complete message as `(channel, bytes)`. START drops the partial message of its
    /// channel; an error drops it too.
    pub fn push(&mut self, report: &[u8]) -> Result<Option<(u8, Vec<u8>)>, FrameError> {
        if !is_tetrad(report) {
            return Err(FrameError::NotTetrad);
        }
        let [protocol, flags, channel, length] = [report[6], report[7], report[8], report[9]];
        if protocol != PROTOCOL {
            self.partial.remove(&channel);
            return Err(FrameError::Protocol(protocol));
        }
        if usize::from(length) > PAYLOAD_BYTES {
            self.partial.remove(&channel);
            return Err(FrameError::Length(length));
        }
        if flags & START != 0 {
            self.partial.insert(channel, Vec::new());
        }
        let Some(bytes) = self.partial.get_mut(&channel) else {
            return Err(FrameError::Sequence(channel));
        };
        if bytes.len() + usize::from(length) > self.max_message_bytes {
            self.partial.remove(&channel);
            return Err(FrameError::TooLarge(self.max_message_bytes));
        }
        bytes.extend_from_slice(&report[HEAD_BYTES..HEAD_BYTES + usize::from(length)]);
        if flags & END == 0 {
            return Ok(None);
        }
        Ok(self.partial.remove(&channel).map(|bytes| (channel, bytes)))
    }
}

/// A request from the host. `id` comes back in its [`Reply`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: u32,
    #[serde(flatten)]
    pub op: Op,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Op {
    /// [`Answer::Hello`].
    Hello,
    /// The UI frame now on the screen; the reply body is a PNG.
    Screenshot,
    /// The recent device notices; the reply body is UTF-8 text, one notice per line.
    Log,
    /// Starts a USB install of a release: the exact bytes of `tetrad-update.json` and of its
    /// signature file. The device checks the signature with the release key.
    InstallBegin { manifest: String, signature: String },
    /// The body is the bytes of manifest file `path` from `offset`, which is the size the
    /// device already has ([`Answer::InstallChunk`]).
    InstallChunk { path: String, offset: u64 },
    /// Every file is complete: the device offers the release on its Tetrad Updates page, where
    /// the user starts the install.
    InstallReady,
    /// A JSON message for the extension with this manifest ID; it needs the `usb` capability.
    /// The reply is what its `ctx.usb.onMessage` handler returns.
    Extension {
        extension: String,
        message: serde_json::Value,
    },
}

/// The reply to the request with the same `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u32,
    #[serde(flatten)]
    pub answer: Answer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case")]
pub enum Answer {
    Hello {
        tetrad: String,
        protocol: u8,
    },
    Screenshot {
        width: u32,
        height: u32,
    },
    Log {
        lines: usize,
    },
    /// The manifest files to send, in order.
    InstallBegin {
        version: String,
        files: Vec<FileSize>,
    },
    /// Bytes of `path` on the device so far.
    InstallChunk {
        path: String,
        received: u64,
    },
    InstallReady {
        version: String,
    },
    /// What the extension's handler returned.
    Extension {
        value: serde_json::Value,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSize {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Error)]
pub enum MessageError {
    #[error("the message is shorter than its header")]
    Truncated,
    #[error("the header is not valid: {0}")]
    Header(#[from] serde_json::Error),
}

/// The bytes of a message: header length, JSON header, body.
pub fn encode(header: &impl Serialize, body: &[u8]) -> Vec<u8> {
    let json = serde_json::to_vec(header).expect("protocol headers always serialize");
    let length = u32::try_from(json.len()).expect("a header is far below 4 GiB");
    let mut message = Vec::with_capacity(4 + json.len() + body.len());
    message.extend_from_slice(&length.to_le_bytes());
    message.extend_from_slice(&json);
    message.extend_from_slice(body);
    message
}

/// The header and the body of a message.
pub fn decode<T: DeserializeOwned>(message: &[u8]) -> Result<(T, &[u8]), MessageError> {
    let (length, rest) = message
        .split_first_chunk::<4>()
        .ok_or(MessageError::Truncated)?;
    let length =
        usize::try_from(u32::from_le_bytes(*length)).map_err(|_| MessageError::Truncated)?;
    if rest.len() < length {
        return Err(MessageError::Truncated);
    }
    let (json, body) = rest.split_at(length);
    Ok((serde_json::from_slice(json)?, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message spans reports and comes back whole; every report keeps the zero stock
    /// prefix that Cortex Control ignores.
    #[test]
    fn messages_round_trip_through_reports() {
        let message: Vec<u8> = (0..1000_u32).map(|i| i as u8).collect();
        let reports = reports(0, &message);
        assert_eq!(reports.len(), 1000_usize.div_ceil(PAYLOAD_BYTES));
        assert!(
            reports
                .iter()
                .all(|report| report[..2] == [0, 0] && is_tetrad(report))
        );
        let mut reassembler = Reassembler::new(4096);
        let (last, rest) = reports.split_last().unwrap();
        for report in rest {
            assert_eq!(reassembler.push(report), Ok(None));
        }
        assert_eq!(reassembler.push(last), Ok(Some((0, message))));
        assert_eq!(
            Reassembler::new(16).push(&super::reports(3, &[])[0]),
            Ok(Some((3, Vec::new())))
        );
    }

    /// Channels reassemble apart; a report without START, an oversize message and a foreign
    /// report are refused.
    #[test]
    fn reassembly_refuses_bad_input() {
        let mut reassembler = Reassembler::new(300);
        let a = reports(1, &[1; 200]);
        let b = reports(2, &[2; 10]);
        assert_eq!(reassembler.push(&a[0]), Ok(None));
        assert_eq!(reassembler.push(&b[0]), Ok(Some((2, vec![2; 10]))));
        assert_eq!(reassembler.push(&a[1]), Ok(Some((1, vec![1; 200]))));
        assert_eq!(reassembler.push(&a[1]), Err(FrameError::Sequence(1)));

        let big = reports(0, &[0; 400]);
        assert_eq!(reassembler.push(&big[0]), Ok(None));
        assert_eq!(reassembler.push(&big[1]), Ok(None));
        assert_eq!(reassembler.push(&big[2]), Err(FrameError::TooLarge(300)));
        assert_eq!(reassembler.push(&big[3]), Err(FrameError::Sequence(0)));

        // A bad report in the middle drops the message: its end does not complete it.
        let three = reports(4, &[4; 300]);
        let mut bad = three[1];
        bad[6] = 2;
        assert_eq!(reassembler.push(&three[0]), Ok(None));
        assert_eq!(reassembler.push(&bad), Err(FrameError::Protocol(2)));
        assert_eq!(reassembler.push(&three[2]), Err(FrameError::Sequence(4)));

        let mut stock = [0_u8; REPORT_BYTES];
        stock[..2].copy_from_slice(&(0xc000_u16 | 20).to_le_bytes());
        assert_eq!(reassembler.push(&stock), Err(FrameError::NotTetrad));
    }

    /// The JSON shape is the wire format: other tools (WebHID pages, scripts) depend on it.
    #[test]
    fn headers_have_a_stable_json_shape() {
        let request = Request {
            id: 7,
            op: Op::InstallChunk {
                path: "bin/tetrad".into(),
                offset: 32768,
            },
        };
        let message = encode(&request, b"elf");
        assert_eq!(
            &message[4..message.len() - 3],
            br#"{"id":7,"op":"install-chunk","path":"bin/tetrad","offset":32768}"#
        );
        let (back, body) = decode::<Request>(&message).unwrap();
        assert_eq!((back, body), (request, &b"elf"[..]));

        let reply = Reply {
            id: 1,
            answer: Answer::Hello {
                tetrad: "0.1.0".into(),
                protocol: PROTOCOL,
            },
        };
        let message = encode(&reply, &[]);
        assert_eq!(
            &message[4..],
            br#"{"id":1,"reply":"hello","tetrad":"0.1.0","protocol":1}"#
        );
        assert_eq!(decode::<Reply>(&message).unwrap().0, reply);
        assert!(matches!(
            decode::<Request>(&message[..6]),
            Err(MessageError::Truncated)
        ));
    }
}

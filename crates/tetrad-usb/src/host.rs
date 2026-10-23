//! The host transport: the unit's HID interface through hidapi. The same interface carries
//! Cortex Control; reports that are not Tetrad reports are skipped.

use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};
use thiserror::Error;

use crate::{
    Answer, FrameError, MessageError, Op, REPORT_BYTES, Reassembler, Reply, Request, decode,
    encode, is_tetrad, reports,
};

pub const VENDOR_ID: u16 = 0x152a;
pub const PRODUCT_ID: u16 = 0x880a;
/// The HID interface; 0–4 are audio and MIDI.
pub const INTERFACE: i32 = 5;
/// Report IDs of the HID descriptor: input 1 and output 2, 128 bytes each.
const IN_REPORT: u8 = 1;
const OUT_REPORT: u8 = 2;
/// A screenshot PNG and some room.
const MAX_REPLY_BYTES: usize = 8 << 20;

#[derive(Debug, Error)]
pub enum LinkError {
    #[error(transparent)]
    Hid(#[from] hidapi::HidError),
    #[error(
        "no Quad Cortex HID interface ({VENDOR_ID:04x}:{PRODUCT_ID:04x}, interface {INTERFACE})"
    )]
    NotFound,
    #[error("no reply in {0:?}: is Tetrad running on the unit?")]
    Timeout(Duration),
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error(transparent)]
    Message(#[from] MessageError),
    #[error("the unit refused: {0}")]
    Refused(String),
}

pub struct Link {
    device: HidDevice,
    reassembler: Reassembler,
    next_id: u32,
}

impl Link {
    pub fn open() -> Result<Self, LinkError> {
        let api = HidApi::new()?;
        let info = (api.device_list())
            .find(|info| {
                info.vendor_id() == VENDOR_ID
                    && info.product_id() == PRODUCT_ID
                    && info.interface_number() == INTERFACE
            })
            .ok_or(LinkError::NotFound)?;
        Ok(Self {
            device: info.open_device(&api)?,
            reassembler: Reassembler::new(MAX_REPLY_BYTES),
            next_id: 1,
        })
    }

    /// Sends `op` with `body` and waits for its reply: the answer and the reply body. An
    /// [`Answer::Error`] becomes [`LinkError::Refused`].
    pub fn call(
        &mut self,
        op: Op,
        body: &[u8],
        timeout: Duration,
    ) -> Result<(Answer, Vec<u8>), LinkError> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        for report in reports(0, &encode(&Request { id, op }, body)) {
            let mut out = [0; 1 + REPORT_BYTES];
            out[0] = OUT_REPORT;
            out[1..].copy_from_slice(&report);
            // The unit stalls the status stage of every SET_REPORT but takes the data (C, USB
            // probe 2026-10-22; Cortex Control gets the same stall), so a write error means
            // nothing. A lost link shows as a timeout below.
            let _ = self.device.write(&out);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(LinkError::Timeout(timeout));
            }
            let mut buffer = [0; 1 + REPORT_BYTES];
            let millis = i32::try_from(left.as_millis()).unwrap_or(i32::MAX);
            let read = self.device.read_timeout(&mut buffer, millis)?;
            // hidapi puts the report ID first when the descriptor numbers its reports.
            let report = match read {
                n if n == 1 + REPORT_BYTES && buffer[0] == IN_REPORT => &buffer[1..],
                REPORT_BYTES => &buffer[..REPORT_BYTES],
                _ => continue,
            };
            if !is_tetrad(report) {
                continue;
            }
            let Some((0, message)) = self.reassembler.push(report)? else {
                continue;
            };
            let (reply, body) = decode::<Reply>(&message)?;
            if reply.id != id {
                continue;
            }
            return match reply.answer {
                Answer::Error { message } => Err(LinkError::Refused(message)),
                answer => Ok((answer, body.to_vec())),
            };
        }
    }
}

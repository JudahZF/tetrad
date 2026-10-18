//! Cortex Control reports carried by the recovered DSP type `0x27` request.
//! VAs: constructor `0x29b0e4`, report copy `0x29affc`, sender `0x6dfbf8`.

use std::fmt;

use crate::{Error, MessageType, read_u32};

pub const HEADER_BYTES: usize = 16;
pub const REPORT_BYTES: usize = 128;
pub const FRAME_BYTES: usize = HEADER_BYTES + REPORT_BYTES;

#[derive(Clone, PartialEq, Eq)]
pub struct HidReportFrame([u8; FRAME_BYTES]);

impl HidReportFrame {
    /// Zeroes unstored header bytes as an owned policy.
    pub fn new(report: &[u8; REPORT_BYTES]) -> Self {
        Self::from_template([0; HEADER_BYTES], report)
    }

    /// Retains header byte 3 and word 12, which the recovered writer does not store.
    pub fn from_template(mut header: [u8; HEADER_BYTES], report: &[u8; REPORT_BYTES]) -> Self {
        header[0] = MessageType::UsbHidTxToHost as u8;
        header[1] = 4;
        header[2] = 0;
        header[4..8].copy_from_slice(&0_u32.to_le_bytes());
        header[8..12].copy_from_slice(&(FRAME_BYTES as u32).to_le_bytes());
        let mut bytes = [0; FRAME_BYTES];
        bytes[..HEADER_BYTES].copy_from_slice(&header);
        bytes[HEADER_BYTES..].copy_from_slice(report);
        Self(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let bytes: [u8; FRAME_BYTES] = bytes
            .try_into()
            .map_err(|_| Error::Invalid("USB HID request size"))?;
        if bytes[0] != MessageType::UsbHidTxToHost as u8
            || bytes[1] != 4
            || bytes[2] != 0
            || read_u32(&bytes, 4) != 0
            || read_u32(&bytes, 8) != FRAME_BYTES as u32
        {
            return Err(Error::Invalid("USB HID request header"));
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; FRAME_BYTES] {
        &self.0
    }

    pub fn report(&self) -> &[u8; REPORT_BYTES] {
        self.0[HEADER_BYTES..]
            .try_into()
            .expect("fixed-size report storage")
    }
}

impl fmt::Debug for HidReportFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HidReportFrame")
            .field("bytes", &FRAME_BYTES)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_and_unstored_header_bytes_survive_the_recovered_writer() {
        let report = std::array::from_fn(|i| i as u8);
        let frame = HidReportFrame::from_template([0xa5; 16], &report);
        assert_eq!(
            &frame.as_bytes()[..16],
            &[
                0x27, 4, 0, 0xa5, 0, 0, 0, 0, 0x90, 0, 0, 0, 0xa5, 0xa5, 0xa5, 0xa5
            ]
        );
        assert_eq!(frame.report(), &report);
        assert_eq!(HidReportFrame::decode(frame.as_bytes()).unwrap(), frame);
        let new = HidReportFrame::new(&report);
        assert_eq!(new.as_bytes()[3], 0);
        assert_eq!(&new.as_bytes()[12..16], &[0; 4]);
        assert_eq!(new.report(), &report);
        assert!(!format!("{frame:?}").contains("165"));
    }

    #[test]
    fn only_complete_reports_with_the_observed_header_are_admitted() {
        let frame = HidReportFrame::new(&[0xff; REPORT_BYTES]);
        assert!(HidReportFrame::decode(&frame.as_bytes()[..143]).is_err());
        assert!(HidReportFrame::decode(&[0; 145]).is_err());
        for offset in [0, 1, 2, 4, 5, 6, 7, 8, 9, 10, 11] {
            let mut bytes = *frame.as_bytes();
            bytes[offset] ^= 1;
            assert!(HidReportFrame::decode(&bytes).is_err(), "offset {offset}");
        }
        for offset in [3, 12, 13, 14, 15, 16, 143] {
            let mut bytes = *frame.as_bytes();
            bytes[offset] ^= 1;
            assert_eq!(HidReportFrame::decode(&bytes).unwrap().as_bytes(), &bytes);
        }
    }
}

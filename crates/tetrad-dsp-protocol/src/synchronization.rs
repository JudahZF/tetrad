//! Diagnostics requests used by `syncAllCores` (ZenUI VA `0x8763e8`).
//! Stock stores only type, core and total length. Other header bytes are opaque.
//! New frames equal `requests::Request::DiagnosticsRequest`, which the host
//! session uses; this type keeps caller templates (review R13).

use crate::{DspCore, Error, MessageType, read_u32};

pub const FRAME_BYTES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticsRequestFrame {
    core: DspCore,
    bytes: [u8; FRAME_BYTES],
}

impl DiagnosticsRequestFrame {
    /// Zero opaque bytes for a newly owned request.
    pub fn new(core: DspCore) -> Self {
        Self::from_template([0; FRAME_BYTES], core)
    }

    pub fn from_template(mut bytes: [u8; FRAME_BYTES], core: DspCore) -> Self {
        bytes[0] = MessageType::DiagnosticsRequest as u8;
        bytes[1] = core as u8;
        bytes[8..12].copy_from_slice(&(FRAME_BYTES as u32).to_le_bytes());
        Self { core, bytes }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != FRAME_BYTES {
            return Err(Error::Invalid("diagnostics request length"));
        }
        if bytes[0] != MessageType::DiagnosticsRequest as u8
            || read_u32(bytes, 8) != FRAME_BYTES as u32
        {
            return Err(Error::Invalid("diagnostics request header"));
        }
        let core = DspCore::try_from(bytes[1])?;
        Ok(Self {
            core,
            bytes: bytes.try_into().expect("length checked"),
        })
    }

    pub fn core(&self) -> DspCore {
        self.core
    }
    pub fn as_bytes(&self) -> &[u8; FRAME_BYTES] {
        &self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_header_preserves_every_unstored_byte_and_refuses_invalid_constants() {
        let frame = DiagnosticsRequestFrame::from_template([0xa5; 16], DspCore::Core3);
        assert_eq!(
            frame.as_bytes(),
            &[
                0x0a, 3, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 16, 0, 0, 0, 0xa5, 0xa5, 0xa5, 0xa5
            ]
        );
        assert_eq!(
            DiagnosticsRequestFrame::decode(frame.as_bytes()).unwrap(),
            frame
        );
        let zero = DiagnosticsRequestFrame::new(DspCore::Core0);
        assert_eq!(
            zero.as_bytes(),
            &[10, 0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0]
        );
        for offset in [0, 1, 8, 9, 10, 11] {
            let mut bytes = *zero.as_bytes();
            bytes[offset] = 0xff;
            assert!(DiagnosticsRequestFrame::decode(&bytes).is_err());
        }
        assert!(DiagnosticsRequestFrame::decode(&zero.as_bytes()[..15]).is_err());
        assert!(DiagnosticsRequestFrame::decode(&[0; 17]).is_err());
    }
}

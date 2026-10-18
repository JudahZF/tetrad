//! `BLw` boot wrapper used by the SOC2 images (`SOC2_SPL_Core0*.blwldr`,
//! `ZPU_SC589_SHARC_Core0.blwldr`). ARM never decrypts or interprets the payload:
//! ZenUI copies each file verbatim into the SOC2 boot buffer (`0x89bae0`) with no
//! content check, so the SOC2 path copies unchanged. [`Blw::parse`] is diagnostic only:
//! it reports whether the frame matches the three stock files, and never gates a copy.
//!
//! Header (0xb4 bytes, integer fields big-endian), observed on all three stock files:
//! magic `BLw\x01`; 56 opaque bytes; 8 zero bytes; 24 bytes identical in every file;
//! 16 bytes that differ per file; payload length; a word equal to 1; 64 zero bytes.
//! The roles of the opaque fields (signature, key id, nonce) are inferred.
use crate::Error;

pub const MAGIC: [u8; 4] = *b"BLw\x01";
pub const HEADER_BYTES: usize = 0xb4;
/// Payload lengths are whole 16-byte cipher blocks in every stock file.
pub const PAYLOAD_ALIGN: usize = 16;

const SIGNATURE: std::ops::Range<usize> = 0x04..0x3c;
const RESERVED_A: std::ops::Range<usize> = 0x3c..0x44;
const KEY_ID: std::ops::Range<usize> = 0x44..0x5c;
const NONCE: std::ops::Range<usize> = 0x5c..0x6c;
const LENGTH: usize = 0x6c;
const FLAGS: usize = 0x70;
const RESERVED_B: std::ops::Range<usize> = 0x74..HEADER_BYTES;

#[derive(Debug, Clone, Copy)]
pub struct Blw<'a> {
    bytes: &'a [u8],
}

impl<'a> Blw<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if !bytes.starts_with(&MAGIC) {
            return Err(Error::Blw("missing BLw\\x01 magic"));
        }
        if bytes.len() < HEADER_BYTES {
            return Err(Error::Truncated {
                part: "BLw header",
                offset: bytes.len(),
            });
        }
        let wrapper = Self { bytes };
        if usize::try_from(wrapper.payload_len()) != Ok(bytes.len() - HEADER_BYTES) {
            return Err(Error::Blw("payload length does not match the file size"));
        }
        if !wrapper.payload().len().is_multiple_of(PAYLOAD_ALIGN) {
            return Err(Error::Blw(
                "payload is not a whole number of 16-byte blocks",
            ));
        }
        if bytes[RESERVED_A]
            .iter()
            .chain(&bytes[RESERVED_B])
            .any(|&byte| byte != 0)
        {
            return Err(Error::Blw("reserved header bytes are not zero"));
        }
        Ok(wrapper)
    }
    /// The whole file, header included: this is what ZenUI copies.
    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }
    pub fn signature(&self) -> &'a [u8] {
        &self.bytes[SIGNATURE]
    }
    pub fn key_id(&self) -> &'a [u8] {
        &self.bytes[KEY_ID]
    }
    pub fn nonce(&self) -> &'a [u8] {
        &self.bytes[NONCE]
    }
    pub fn payload_len(&self) -> u32 {
        self.be_word(LENGTH)
    }
    pub fn flags(&self) -> u32 {
        self.be_word(FLAGS)
    }
    pub fn payload(&self) -> &'a [u8] {
        &self.bytes[HEADER_BYTES..]
    }
    fn be_word(&self, offset: usize) -> u32 {
        u32::from_be_bytes([
            self.bytes[offset],
            self.bytes[offset + 1],
            self.bytes[offset + 2],
            self.bytes[offset + 3],
        ])
    }
}

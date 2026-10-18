//! Control transport stages: the reset-marker frame (`0x29ad14`), the zlib and comms-cipher
//! pipeline stages and the ModelRepo archive helpers. Moved from `control.rs` (round 37).

use std::io::{Read, Write};

use flate2::Compression as Level;
use flate2::read::{GzDecoder, ZlibDecoder};
use flate2::write::GzEncoder;
use qc_format::crypto::NdspCipher;
use tetrad_control::pipeline::{self, StageFailure};
use tetrad_dsp_protocol::hid::{FRAME_BYTES, HEADER_BYTES, REPORT_BYTES};
use tetrad_dsp_protocol::{CoreId, MessageType};

/// `0x29ad14`: the `0x27` header template with byte 2 = `0xff`. Stock leaves the report
/// bytes uninitialized (zero here) and word 12 unstored; the generic fragment path stores
/// the report length there (owned).
pub fn reset_marker_frame() -> [u8; FRAME_BYTES] {
    let mut frame = [0; FRAME_BYTES];
    frame[0] = MessageType::UsbHidTxToHost as u8;
    frame[1] = CoreId::Control.wire();
    frame[2] = 0xff;
    frame[8..12].copy_from_slice(&(FRAME_BYTES as u32).to_le_bytes());
    frame[12..HEADER_BYTES].copy_from_slice(&(REPORT_BYTES as u32).to_le_bytes());
    frame
}

/// zlib stages: inflate accepts zlib or gzip (`inflateInit2_` windowBits 47, `0x462318`);
/// deflate is level 1 with a gzip wrapper (windowBits 31, `0x462a28`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Zlib;

impl pipeline::Compression for Zlib {
    fn inflate(&self, input: &[u8], max_output: usize) -> Result<Vec<u8>, StageFailure> {
        let reader: Box<dyn Read + '_> = if input.starts_with(&[0x1f, 0x8b]) {
            Box::new(GzDecoder::new(input))
        } else {
            Box::new(ZlibDecoder::new(input))
        };
        let mut output = Vec::new();
        let limit = u64::try_from(max_output)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        reader
            .take(limit)
            .read_to_end(&mut output)
            .map_err(|_| StageFailure::Malformed)?;
        if output.len() > max_output {
            return Err(StageFailure::Limit(max_output));
        }
        Ok(output)
    }

    fn deflate(&self, input: &[u8]) -> Result<Vec<u8>, StageFailure> {
        let mut encoder = GzEncoder::new(Vec::new(), Level::fast());
        encoder
            .write_all(input)
            .and_then(|()| encoder.finish())
            .map_err(|_| StageFailure::Malformed)
    }
}

/// The payload transform (`0x6acc9c`/`0x6ade54` → `0x9f53c8`): AES-128-CTR, initialized
/// per payload, so every message starts at the IV. Key and IV are
/// `EVP_BytesToKey(aes-128-ctr, sha1, no salt, count 10)` over the `+0x400` vector, which is
/// the NDSP derivation ([`NdspCipher`]). Not `Clone`; the derived bytes are zeroed on drop
/// and `Debug` is redacted.
pub struct CommsCipher(NdspCipher);

impl CommsCipher {
    /// The cipher of one key-exchange vector (run-time input; never a constant).
    pub fn derive(material: &[u8]) -> Self {
        Self(NdspCipher::derive(material))
    }
}

impl std::fmt::Debug for CommsCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CommsCipher(..)")
    }
}

impl pipeline::Transform for CommsCipher {
    fn decode(&self, input: &[u8]) -> Result<Vec<u8>, StageFailure> {
        Ok(self.0.crypt(input))
    }

    fn encode(&self, input: &[u8]) -> Result<Vec<u8>, StageFailure> {
        Ok(self.0.crypt(input))
    }
}

/// `0x11e2a0`: every non-empty `blob="…"` value becomes random lowercase letters. Stock
/// uses `rand()`; a fixed LCG is used here (owned).
pub(super) fn scramble_blob_names(xml: &mut [u8]) {
    const ATTRIBUTE: &[u8] = b"blob=\"";
    let mut seed: u32 = 1;
    let mut from = 0;
    while let Some(found) = xml[from..]
        .windows(ATTRIBUTE.len())
        .position(|window| window == ATTRIBUTE)
    {
        let start = from + found + ATTRIBUTE.len();
        let Some(length) = xml[start..].iter().position(|&byte| byte == b'"') else {
            break;
        };
        for byte in &mut xml[start..start + length] {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            *byte = b'a' + ((seed >> 16) % 26) as u8;
        }
        from = start + length + 1;
    }
}

/// One regular-file entry in a gzip-filtered ustar archive, as libarchive's
/// `pax_restricted` writes it for a short name (`0x6b440c`: pathname, size, `S_IFREG`,
/// 0644). Owner, time and the end padding are zero (owned).
pub(super) fn gzip_tar(name: &str, data: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 512;
    let mut header = [0u8; BLOCK];
    let mut field =
        |offset: usize, text: &[u8]| header[offset..offset + text.len()].copy_from_slice(text);
    field(0, name.as_bytes());
    field(100, b"0000644\0");
    field(108, b"0000000\0");
    field(116, b"0000000\0");
    field(124, format!("{:011o}\0", data.len()).as_bytes());
    field(136, b"00000000000\0");
    field(148, b"        ");
    field(156, b"0");
    field(257, b"ustar\0");
    field(263, b"00");
    field(329, b"0000000\0");
    field(337, b"0000000\0");
    let checksum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
    header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
    let mut tar = header.to_vec();
    tar.extend_from_slice(data);
    tar.resize(tar.len().div_ceil(BLOCK) * BLOCK + 2 * BLOCK, 0);
    let mut encoder = GzEncoder::new(Vec::new(), Level::default());
    encoder
        .write_all(&tar)
        .and_then(|()| encoder.finish())
        .expect("writing to memory cannot fail")
}

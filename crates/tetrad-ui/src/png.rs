//! PNG encoding of rendered frames: Cortex Control screenshots and the README images.

use std::io::Write;

use flate2::Crc;
use flate2::write::ZlibEncoder;

/// PNG colour types this encoder writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PngColour {
    /// Type 2, 3 bytes per pixel: [`crate::Frame::rgb`].
    Rgb = 2,
    /// Type 6, 4 bytes per pixel, not premultiplied: [`crate::UiState::preset_grid`].
    Rgba = 6,
}

/// An 8-bit PNG of `pixels` (`width × height` packed pixels): one IDAT, filter 0 per row.
/// `None` when the buffer does not match the size.
pub fn png(width: u32, height: u32, colour: PngColour, pixels: &[u8]) -> Option<Vec<u8>> {
    let channels = match colour {
        PngColour::Rgb => 3,
        PngColour::Rgba => 4,
    };
    let row = usize::try_from(width).ok()?.checked_mul(channels)?;
    if row == 0 || pixels.len() != row.checked_mul(usize::try_from(height).ok()?)? {
        return None;
    }
    let mut image = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    for line in pixels.chunks_exact(row) {
        image.write_all(&[0]).ok()?;
        image.write_all(line).ok()?;
    }
    let image = image.finish().ok()?;
    let mut header = Vec::with_capacity(13);
    header.extend(width.to_be_bytes());
    header.extend(height.to_be_bytes());
    // Bit depth 8, the colour type, deflate, adaptive filtering, no interlace.
    header.extend([8, colour as u8, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in [
        (b"IHDR", &header),
        (b"IDAT", &image),
        (b"IEND", &Vec::new()),
    ] {
        out.extend(u32::try_from(data.len()).ok()?.to_be_bytes());
        let mut crc = Crc::new();
        crc.update(kind);
        crc.update(data);
        out.extend(kind);
        out.extend(data);
        out.extend(crc.sum().to_be_bytes());
    }
    Some(out)
}

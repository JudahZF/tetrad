//! Stock metronome click sounds (`MetronomeDriver::sendAllSounds`, VA 0x558554).
//!
//! The driver reads 18 fixed files in this order, sizes each with `QFile::size`
//! (a file that cannot open logs `Metronome: Cannot open` and counts zero bytes)
//! and uploads them as one bundle (message 0x47, core 2). The bundle wire format
//! belongs to `tetrad-dsp-protocol::controls::MetronomeSoundBundle`; this module
//! only supplies the ordered sound bytes.

pub const METRONOME_SOUND_DIR: &str = "/opt/neuraldsp/metronomesounds";

/// Stock bundle order (literal order at VA 0x558554; string VAs 0xa533c8..0xa537a0).
pub const STOCK_SOUND_FILES: [&str; 18] = [
    "JP Blip 2 Downbeat.bin",
    "JP Blip 2 Offbeat.bin",
    "JP Blip 2 Onbeat.bin",
    "JP Block and Shaker Downbeat.bin",
    "JP Block and Shaker Offbeat.bin",
    "JP Block and Shaker Onbeat.bin",
    "JP Cowbell Downbeat.bin",
    "JP Cowbell Offbeat.bin",
    "JP Cowbell Onbeat.bin",
    "JP Digital Downbeat.bin",
    "JP Digital Offbeat.bin",
    "JP Digital Onbeat.bin",
    "JP Drum Kit Downbeat.bin",
    "JP Drum Kit Offbeat.bin",
    "JP Drum Kit Onbeat.bin",
    "TK Brush Accent 1 C (no brush).bin",
    "TK Brush Normal C.bin",
    "TK Brush Accent 2 C.bin",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetronomeSoundError {
    #[error("metronome sound {file} has {bytes} bytes, exceeding limit {limit}")]
    TooLarge {
        file: &'static str,
        bytes: usize,
        limit: usize,
    },
    /// The stock length word is `bytes >> 1`; an odd file would lose a byte.
    #[error("metronome sound {file} has odd byte length {bytes}")]
    OddLength { file: &'static str, bytes: usize },
}

/// One stock sound slot. `missing` records the stock zero-byte fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetronomeSound {
    pub file: &'static str,
    pub bytes: Vec<u8>,
    pub missing: bool,
}

/// Reads the 18 stock sounds through `read` (given the full path) in bundle
/// order. `None` from `read` is the stock "cannot open" case: an empty sound.
/// Sound contents are opaque 16-bit words; their sample format stays unverified.
pub fn load_stock_sounds(
    mut read: impl FnMut(&str) -> Option<Vec<u8>>,
    max_sound_bytes: usize,
) -> Result<Vec<MetronomeSound>, MetronomeSoundError> {
    STOCK_SOUND_FILES
        .iter()
        .map(|&file| {
            let bytes = read(&format!("{METRONOME_SOUND_DIR}/{file}"));
            let missing = bytes.is_none();
            let bytes = bytes.unwrap_or_default();
            if bytes.len() > max_sound_bytes {
                return Err(MetronomeSoundError::TooLarge {
                    file,
                    bytes: bytes.len(),
                    limit: max_sound_bytes,
                });
            }
            if !bytes.len().is_multiple_of(2) {
                return Err(MetronomeSoundError::OddLength {
                    file,
                    bytes: bytes.len(),
                });
            }
            Ok(MetronomeSound {
                file,
                bytes,
                missing,
            })
        })
        .collect()
}

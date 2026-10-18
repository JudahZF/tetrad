//! Owned raw ADI LDR block stream, in the shape of the stock SC589 core images.
//!
//! Header: four LE u32 words. Word 0 holds flags (bits 15:0), the header XOR
//! checksum (23:16) and the core signature (31:24). Word 1 is the target, word 2
//! the byte count, word 3 the argument. The XOR of all 16 header bytes is zero.
//! This is the format `tetrad_loader::RawLdr` parses.
//!
//! Stock structure (header metadata of `ZPUKernel_Core{1,2}.ldr`): the first block is `FIRST |
//! IGNORE | DMACODE 1`, targets the entry, has no payload and its argument is the byte offset of
//! the final header. Literal blocks use flags `0x0001` with 4-byte aligned targets and counts, at
//! most `0x800` bytes each (the stock maximum; the stock booter buffer is `0xa68` bytes). Fill
//! blocks use `0x0101` with argument 0. The final block is `FINAL | DMACODE 1` and targets the
//! entry. The emitter produces this shape and splits longer literals. It does not prove that a boot
//! ROM or the stock loader accepts an owned image.

/// Stock DMA code nibble of every block (`0x...1`). Its width meaning is inferred.
const DMACODE: u16 = 0x0001;
const FLAG_FILL: u16 = 0x0100;
const FLAG_IGNORE: u16 = 0x1000;
const FLAG_FIRST: u16 = 0x4000;
const FLAG_FINAL: u16 = 0x8000;
pub const LDR_HEADER_BYTES: usize = 16;
/// Largest literal payload per block, as in every stock image.
pub const LDR_LITERAL_MAX: usize = 0x800;

/// Header signature byte. Matches `tetrad_loader::Signature`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LdrCore {
    Core1 = 0xac,
    Core2 = 0xab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LdrError {
    #[error("block target {target:#x} or count {count:#x} is not 4-byte aligned")]
    Alignment { target: u32, count: u32 },
    #[error("block at {0:#x} is empty")]
    Empty(u32),
    #[error("block at {0:#x} overflows the 32-bit address space")]
    Overflow(u32),
    #[error("blocks at {first:#x} and {second:#x} overlap")]
    Overlap { first: u32, second: u32 },
    #[error("image exceeds the 32-bit stream offset of the final block")]
    StreamSize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Payload {
    Literal(Vec<u8>),
    ZeroFill(u32),
}

impl Payload {
    fn count(&self) -> u32 {
        match self {
            Self::Literal(bytes) => bytes.len() as u32,
            Self::ZeroFill(count) => *count,
        }
    }
}

/// Collects target-addressed blocks. Targets are system byte addresses.
#[derive(Debug, Clone)]
pub struct LdrBuilder {
    core: LdrCore,
    entry: u32,
    blocks: Vec<(u32, Payload)>,
}

impl LdrBuilder {
    /// `entry` is written as the FIRST/FINAL target, as in stock images.
    pub fn new(core: LdrCore, entry: u32) -> Self {
        Self {
            core,
            entry,
            blocks: Vec::new(),
        }
    }

    pub fn literal(&mut self, target: u32, bytes: Vec<u8>) -> Result<(), LdrError> {
        let count = u32::try_from(bytes.len()).map_err(|_| LdrError::Overflow(target))?;
        self.add(target, count, Payload::Literal(bytes))
    }

    pub fn zero_fill(&mut self, target: u32, count: u32) -> Result<(), LdrError> {
        self.add(target, count, Payload::ZeroFill(count))
    }

    fn add(&mut self, target: u32, count: u32, payload: Payload) -> Result<(), LdrError> {
        if count == 0 {
            return Err(LdrError::Empty(target));
        }
        if !target.is_multiple_of(4) || !count.is_multiple_of(4) {
            return Err(LdrError::Alignment { target, count });
        }
        let end = target
            .checked_add(count)
            .ok_or(LdrError::Overflow(target))?;
        for (other, existing) in &self.blocks {
            if target < other + existing.count() && *other < end {
                return Err(LdrError::Overlap {
                    first: *other,
                    second: target,
                });
            }
        }
        self.blocks.push((target, payload));
        Ok(())
    }

    /// Blocks keep insertion order, so callers control load order.
    pub fn finish(self) -> Result<LdrImage, LdrError> {
        let mut body = Vec::new();
        for (target, payload) in &self.blocks {
            match payload {
                Payload::Literal(bytes) => {
                    for (chunk, part) in (0u32..).zip(bytes.chunks(LDR_LITERAL_MAX)) {
                        let at = target + chunk * LDR_LITERAL_MAX as u32;
                        body.extend(header(self.core, DMACODE, at, part.len() as u32, 0));
                        body.extend_from_slice(part);
                    }
                }
                Payload::ZeroFill(count) => {
                    body.extend(header(self.core, DMACODE | FLAG_FILL, *target, *count, 0));
                }
            }
        }
        let final_offset =
            u32::try_from(LDR_HEADER_BYTES + body.len()).map_err(|_| LdrError::StreamSize)?;
        let mut bytes = Vec::with_capacity(body.len() + 2 * LDR_HEADER_BYTES);
        bytes.extend(header(
            self.core,
            DMACODE | FLAG_FIRST | FLAG_IGNORE,
            self.entry,
            0,
            final_offset,
        ));
        bytes.extend(body);
        bytes.extend(header(self.core, DMACODE | FLAG_FINAL, self.entry, 0, 0));
        Ok(LdrImage {
            core: self.core,
            entry: self.entry,
            bytes,
        })
    }
}

fn header(core: LdrCore, flags: u16, target: u32, count: u32, argument: u32) -> [u8; 16] {
    let mut bytes = [0; LDR_HEADER_BYTES];
    bytes[0..2].copy_from_slice(&flags.to_le_bytes());
    bytes[3] = core as u8;
    bytes[4..8].copy_from_slice(&target.to_le_bytes());
    bytes[8..12].copy_from_slice(&count.to_le_bytes());
    bytes[12..16].copy_from_slice(&argument.to_le_bytes());
    bytes[2] = bytes.iter().fold(0, |xor, byte| xor ^ byte);
    bytes
}

/// A finished stream. The entry is also the loader's `SetEntry` argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LdrImage {
    core: LdrCore,
    entry: u32,
    bytes: Vec<u8>,
}

impl LdrImage {
    pub fn core(&self) -> LdrCore {
        self.core
    }
    pub fn entry(&self) -> u32 {
        self.entry
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn stream_has_the_stock_first_literal_fill_final_shape() {
        let mut builder = LdrBuilder::new(LdrCore::Core2, 0x90004);
        builder.literal(0x28a4_0000, vec![1, 2, 3, 4]).unwrap();
        builder.zero_fill(0x28a4_0100, 0x40).unwrap();
        let image = builder.finish().unwrap();
        let bytes = image.bytes();
        assert_eq!(bytes.len(), 16 + 20 + 16 + 16);
        // FIRST|IGNORE: entry target, no payload, argument = final header offset.
        assert_eq!(word(bytes, 0) & 0xff00_ffff, 0xab00_5001);
        assert_eq!(
            (word(bytes, 4), word(bytes, 8), word(bytes, 12)),
            (0x90004, 0, 52)
        );
        assert_eq!(word(bytes, 16) & 0xff00_ffff, 0xab00_0001);
        assert_eq!(&bytes[32..36], &[1, 2, 3, 4]);
        assert_eq!(word(bytes, 36) & 0xff00_ffff, 0xab00_0101);
        assert_eq!((word(bytes, 44), word(bytes, 48)), (0x40, 0));
        assert_eq!(word(bytes, 52) & 0xff00_ffff, 0xab00_8001);
        assert_eq!(word(bytes, 56), 0x90004);
        for offset in [0, 16, 36, 52] {
            let header = &bytes[offset..offset + 16];
            assert_eq!(header.iter().fold(0, |x, b| x ^ b), 0, "{offset}");
        }
    }

    /// Regression for review F6: no literal block exceeds the stock maximum.
    #[test]
    fn long_literals_split_into_stock_sized_blocks() {
        let mut builder = LdrBuilder::new(LdrCore::Core1, 0x9_0004);
        let payload: Vec<u8> = (0..2 * LDR_LITERAL_MAX + 4).map(|i| i as u8).collect();
        builder.literal(0x2824_0000, payload.clone()).unwrap();
        let image = builder.finish().unwrap();
        let bytes = image.bytes();
        let mut offset = LDR_HEADER_BYTES;
        let mut joined = Vec::new();
        for (target, count) in [(0x2824_0000, 0x800), (0x2824_0800, 0x800), (0x2824_1000, 4)] {
            assert_eq!(word(bytes, offset) & 0xffff, 0x0001);
            assert_eq!(
                (word(bytes, offset + 4), word(bytes, offset + 8)),
                (target, count)
            );
            offset += LDR_HEADER_BYTES;
            joined.extend_from_slice(&bytes[offset..offset + count as usize]);
            offset += count as usize;
        }
        assert_eq!(joined, payload);
        assert_eq!(
            word(bytes, 12) as usize,
            offset,
            "FIRST argument = FINAL offset"
        );
    }

    #[test]
    fn misaligned_empty_and_overlapping_blocks_are_rejected() {
        let mut builder = LdrBuilder::new(LdrCore::Core1, 0);
        assert_eq!(
            builder.literal(0x1002, vec![0; 4]),
            Err(LdrError::Alignment {
                target: 0x1002,
                count: 4
            })
        );
        assert_eq!(
            builder.literal(0x1000, vec![]),
            Err(LdrError::Empty(0x1000))
        );
        builder.literal(0x1000, vec![0; 8]).unwrap();
        assert_eq!(
            builder.zero_fill(0x1004, 4),
            Err(LdrError::Overlap {
                first: 0x1000,
                second: 0x1004
            })
        );
        builder.zero_fill(0x1008, 4).unwrap();
        assert_eq!(
            builder.zero_fill(0xffff_fffc, 8),
            Err(LdrError::Overflow(0xffff_fffc))
        );
    }
}

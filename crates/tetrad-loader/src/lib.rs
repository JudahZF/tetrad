//! Stock SHARC boot for the ADSP-SC589: strict raw ADI LDR and `BLw` parsing, the booter embedded
//! in `loadsharc_SC589`, typed boot plans for SOC1 cores 1/2 and the SOC2 slave boot, and an
//! executor over injected hardware traits.
#![forbid(unsafe_code)]

pub mod blw;
pub mod booter;
pub mod exec;
pub mod fake;
pub mod intent;
pub mod plan;
pub mod soc2;

use std::fmt;
use thiserror::Error;

pub const HEADER_BYTES: usize = 16;
pub const FILL_FLAG: u32 = 0x100;
pub const IGNORE_FLAG: u32 = 0x1000;
pub const FIRST_FLAG: u32 = 0x4000;
pub const FINAL_FLAG: u32 = 0x8000;

/// ARM physical byte address on the SC589 (32-bit physical space).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysAddr(pub u32);

impl PhysAddr {
    pub fn checked_add(self, bytes: u32) -> Option<Self> {
        self.0.checked_add(bytes).map(Self)
    }
}

impl fmt::Display for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#010x}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Signature {
    Core1 = 0xac,
    Core2 = 0xab,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Error {
    #[error("BLw wrapper is not a supported raw ADI LDR stream")]
    UnsupportedWrapper,
    #[error("empty raw LDR stream")]
    Empty,
    #[error("truncated {part} at byte {offset}")]
    Truncated { part: &'static str, offset: usize },
    #[error("unsupported signature 0x{value:02x} at byte {offset}")]
    Signature { value: u8, offset: usize },
    #[error("header byte XOR is nonzero at byte {0}")]
    Checksum(usize),
    #[error("{0} exceeds its configured bound")]
    Limit(&'static str),
    #[error("checked arithmetic overflow in {0}")]
    Overflow(&'static str),
    #[error("LDR block signature does not match the intended core")]
    CoreMismatch,
    #[error("LDR stream layout: {0}")]
    Layout(&'static str),
    #[error("BLw wrapper: {0}")]
    Blw(&'static str),
    #[error("loadsharc booter: {0}")]
    Booter(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_stream_bytes: usize,
    pub max_blocks: usize,
    /// Applies to both literal payload counts and no-payload fill counts.
    pub max_block_bytes: u32,
    pub max_total_declared_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_stream_bytes: 16 * 1024 * 1024,
            max_blocks: 4096,
            max_block_bytes: 16 * 1024 * 1024,
            max_total_declared_bytes: 64 * 1024 * 1024,
        }
    }
}

/// The four words survive unchanged, including unknown flags and argument bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    words: [u32; 4],
    signature: Signature,
}

impl Header {
    pub fn decode(bytes: [u8; 16]) -> Result<Self, Error> {
        Self::decode_at(bytes, 0)
    }
    fn decode_at(bytes: [u8; 16], offset: usize) -> Result<Self, Error> {
        let signature = match bytes[3] {
            0xac => Signature::Core1,
            0xab => Signature::Core2,
            value => return Err(Error::Signature { value, offset }),
        };
        if bytes.iter().fold(0_u8, |xor, byte| xor ^ byte) != 0 {
            return Err(Error::Checksum(offset));
        }
        let words = std::array::from_fn(|word| {
            let start = word * 4;
            u32::from_le_bytes([
                bytes[start],
                bytes[start + 1],
                bytes[start + 2],
                bytes[start + 3],
            ])
        });
        Ok(Self { words, signature })
    }
    pub fn words(self) -> [u32; 4] {
        self.words
    }
    pub fn signature(self) -> Signature {
        self.signature
    }
    pub fn flags(self) -> u16 {
        self.words[0] as u16
    }
    pub fn target_address(self) -> u32 {
        self.words[1]
    }
    pub fn byte_count(self) -> u32 {
        self.words[2]
    }
    pub fn argument(self) -> u32 {
        self.words[3]
    }
    pub fn is_fill(self) -> bool {
        self.words[0] & FILL_FLAG != 0
    }
    pub fn is_ignore(self) -> bool {
        self.words[0] & IGNORE_FLAG != 0
    }
    pub fn is_first(self) -> bool {
        self.words[0] & FIRST_FLAG != 0
    }
    pub fn is_final(self) -> bool {
        self.words[0] & FINAL_FLAG != 0
    }
    /// Checked byte interval only; not permission to write to that address.
    pub fn target_end_exclusive(self) -> Result<u32, Error> {
        self.target_address()
            .checked_add(self.byte_count())
            .ok_or(Error::Overflow("LDR target interval"))
    }
    pub fn to_bytes(self) -> [u8; 16] {
        let mut result = [0; 16];
        for (index, word) in self.words.into_iter().enumerate() {
            result[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockData<'a> {
    Literal(&'a [u8]),
    /// No stream payload. Argument interpretation is not implemented.
    Fill {
        byte_count: u32,
        argument: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block<'a> {
    pub offset: usize,
    pub header: Header,
    pub data: BlockData<'a>,
}

#[derive(Debug)]
pub struct RawLdr<'a> {
    bytes: &'a [u8],
    blocks: Vec<Block<'a>>,
    total_declared_bytes: u64,
}

impl<'a> RawLdr<'a> {
    /// Scan strictly to EOF; FINAL/FIRST bits are preserved, not guessed as
    /// stop conditions. Payloads borrow the caller buffer with no expansion.
    pub fn parse(bytes: &'a [u8], limits: Limits) -> Result<Self, Error> {
        if bytes.starts_with(b"BLw") {
            return Err(Error::UnsupportedWrapper);
        }
        if bytes.is_empty() {
            return Err(Error::Empty);
        }
        if bytes.len() > limits.max_stream_bytes {
            return Err(Error::Limit("LDR stream"));
        }
        let mut blocks = Vec::new();
        let mut position = 0_usize;
        let mut total_declared_bytes = 0_u64;
        while position < bytes.len() {
            if blocks.len() >= limits.max_blocks {
                return Err(Error::Limit("LDR blocks"));
            }
            let header_end = position
                .checked_add(HEADER_BYTES)
                .ok_or(Error::Overflow("LDR header offset"))?;
            let raw: [u8; 16] = bytes
                .get(position..header_end)
                .ok_or(Error::Truncated {
                    part: "header",
                    offset: position,
                })?
                .try_into()
                .map_err(|_| Error::Truncated {
                    part: "header",
                    offset: position,
                })?;
            let header = Header::decode_at(raw, position)?;
            if header.byte_count() > limits.max_block_bytes {
                return Err(Error::Limit("LDR block bytes"));
            }
            header.target_end_exclusive()?;
            total_declared_bytes = total_declared_bytes
                .checked_add(u64::from(header.byte_count()))
                .ok_or(Error::Overflow("LDR total declared bytes"))?;
            if total_declared_bytes > limits.max_total_declared_bytes {
                return Err(Error::Limit("LDR total declared bytes"));
            }
            let (data, end) = if header.is_fill() {
                (
                    BlockData::Fill {
                        byte_count: header.byte_count(),
                        argument: header.argument(),
                    },
                    header_end,
                )
            } else {
                let count = usize::try_from(header.byte_count())
                    .map_err(|_| Error::Overflow("LDR payload count"))?;
                let end = header_end
                    .checked_add(count)
                    .ok_or(Error::Overflow("LDR payload offset"))?;
                let payload = bytes.get(header_end..end).ok_or(Error::Truncated {
                    part: "payload",
                    offset: header_end,
                })?;
                (BlockData::Literal(payload), end)
            };
            blocks.push(Block {
                offset: position,
                header,
                data,
            });
            position = end;
        }
        Ok(Self {
            bytes,
            blocks,
            total_declared_bytes,
        })
    }
    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }
    pub fn blocks(&self) -> &[Block<'a>] {
        &self.blocks
    }
    pub fn total_declared_bytes(&self) -> u64 {
        self.total_declared_bytes
    }
    pub fn validate_signature(&self, expected: Signature) -> Result<(), Error> {
        if self
            .blocks
            .iter()
            .any(|block| block.header.signature() != expected)
        {
            Err(Error::CoreMismatch)
        } else {
            Ok(())
        }
    }
    /// Checks the multi-block frame both stock kernel streams use: block 0 is
    /// FIRST|IGNORE with no payload, its target is the entry and its argument is the
    /// byte offset of the last block; the last block is FINAL with no payload and the
    /// same target; no other block carries FIRST or FINAL.
    pub fn stream_layout(&self) -> Result<StreamLayout, Error> {
        let (Some(first), Some(last)) = (self.blocks.first(), self.blocks.last()) else {
            return Err(Error::Empty);
        };
        if self.blocks.len() < 2 {
            return Err(Error::Layout(
                "stream needs separate FIRST and FINAL blocks",
            ));
        }
        let first_header = first.header;
        let last_header = last.header;
        if !(first_header.is_first() && first_header.is_ignore()) || first_header.byte_count() != 0
        {
            return Err(Error::Layout("block 0 is not an empty FIRST|IGNORE block"));
        }
        if !last_header.is_final() || last_header.is_fill() || last_header.byte_count() != 0 {
            return Err(Error::Layout("last block is not an empty FINAL block"));
        }
        if usize::try_from(first_header.argument()) != Ok(last.offset) {
            return Err(Error::Layout(
                "FIRST argument is not the FINAL block offset",
            ));
        }
        if first_header.target_address() != last_header.target_address() {
            return Err(Error::Layout("FIRST and FINAL targets differ"));
        }
        let inner = &self.blocks[1..self.blocks.len() - 1];
        if inner
            .iter()
            .any(|block| block.header.is_first() || block.header.is_final())
        {
            return Err(Error::Layout("FIRST or FINAL flag inside the stream"));
        }
        Ok(StreamLayout {
            entry: first_header.target_address(),
            final_offset: last.offset,
        })
    }
}

/// Recovered frame of a stock kernel LDR stream. The SHARC-side booter, not ARM,
/// interprets the blocks; ARM streams the file bytes unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLayout {
    /// SHARC entry address carried by the FIRST and FINAL blocks.
    pub entry: u32,
    pub final_offset: usize,
}

#[cfg(test)]
mod boot_tests;
#[cfg(test)]
mod tests;

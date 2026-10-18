//! VISA (variable-length instruction set) encodings for the owned block shim.
//!
//! Code linked into short-word (SW) space executes as VISA: 16-, 32- and 48-bit
//! instructions addressed in 16-bit parcels (PRM `.13.1`, `.06.004`). The
//! 48-bit "a" forms are the same words as [`Instruction48`]. Section 2 of a
//! model blob stores each parcel little-endian, high parcel first. Only the
//! shim forms below are encoded; [`crate::machine`] executes them. Every form
//! has a stock site (ABI record "Stock sites per emitted form").
use crate::address::{PmWordAddress24, VisaParcelOffset};
use crate::isa48::{EncodeError, Instruction48};
use crate::register::{Dag1Index, DataReg, UniversalReg};

/// Type 21c `NOP` (PRM `.17.16`).
const NOP: u16 = 0x0001;
/// Type 25c `RFRAME`: `I7 = I6, I6 = DM(0, I6)` (PRM `.17.21`).
const RFRAME: u16 = 0x1901;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisaInstruction {
    /// Type 17a `ureg = imm32` (48-bit). Kind-8 relocations patch `value`.
    Immediate { dst: UniversalReg, value: u32 },
    /// `DM(offset, Ia) = Rn`, I not updated: Type 15b (32-bit) for
    /// `-64..=63`, else Type 15a (48-bit).
    DmOffsetStore {
        src: DataReg,
        index: Dag1Index,
        offset: i32,
    },
    /// `Rn = DM(offset, Ia)`, I not updated: Type 15b or Type 15a.
    DmOffsetLoad {
        dst: DataReg,
        index: Dag1Index,
        offset: i32,
    },
    /// Type 19a `Ia = MODIFY(Ia, words)(NW)` (48-bit), as
    /// [`Instruction48::ModifyWords`]. The C prologue `I7 = MODIFY(I7, -n)(NW)`
    /// reserves n frame words below the CJUMP pair.
    ModifyWords { index: Dag1Index, words: i32 },
    /// Type 25a `CJUMP addr (DB)` (48-bit): `R2 = I6, I6 = I7`, then a jump
    /// with two delay slots that push R2 and the return literal (stock call
    /// shape, ABI record §4). Kind-11 relocations patch the target; an ISA
    /// target switches the sequencer to ISA (stock: fortin `0x93c`).
    CJump(PmWordAddress24),
    /// Type 3c `DM(I7, M7) = Rn` (16-bit): push one data register.
    Push(DataReg),
    /// Type 16a `DM(I7, M7) = imm32` (48-bit): push a literal. Kind-7
    /// relocations patch `value`, the CJUMP return address − 1.
    PushImmediate(u32),
    /// Type 21c `NOP` (16-bit).
    Nop,
    /// The stock C epilogue after a CJUMP entry: `I12 = DM(M7, I6)` (Type 3b),
    /// `JUMP (M14, I12) (DB)` (Type 9b), `RFRAME`, `NOP`. It returns to the
    /// saved address + M14 and needs the C-runtime M7 = -1, M14 = 1.
    CReturn,
}

impl VisaInstruction {
    /// Parcels in execution order.
    pub fn parcels(self) -> Result<Vec<u16>, EncodeError> {
        let word = match self {
            Self::Immediate { dst, value } => Instruction48::Immediate { dst, value },
            Self::DmOffsetStore { src, index, offset } => {
                match short_offset(src, index, offset, true) {
                    Some(short) => return Ok(split32(short).into()),
                    None => Instruction48::DmOffsetStore { src, index, offset },
                }
            }
            Self::DmOffsetLoad { dst, index, offset } => {
                match short_offset(dst, index, offset, false) {
                    Some(short) => return Ok(split32(short).into()),
                    None => Instruction48::DmOffsetLoad { dst, index, offset },
                }
            }
            Self::ModifyWords { index, words } => Instruction48::ModifyWords { index, words },
            // Type 25a: 00011000, J (DB) at 34, ADDR24.
            Self::CJump(target) => return Ok(split48(0x1804_0000_0000 | u64::from(target.get()))),
            // Type 3c: 1001, I=7, M=7, D=1, fixed 1, Rn.
            Self::Push(src) => return Ok(vec![0x9ff0 | u16::from(src.index())]),
            // Type 16a: 100, 1, I=7 (43:41), M=7 (40:38), G=0, DATA32.
            Self::PushImmediate(value) => {
                return Ok(split48(0x9fc0_0000_0000 | u64::from(value)));
            }
            Self::Nop => return Ok(vec![NOP]),
            Self::CReturn => {
                // Type 3b: 010, U=0 (no update), I6, M7, COND=31, G=D=L=0,
                // UREG=I12 (0x1c), marker 01111, W=X=1 (normal word).
                let load = 0x4000_0000 | (6 << 25) | (7 << 22) | (31 << 17) | (0x1c << 7) | 0x3f;
                // Type 9b: 0000100, REL=B=A=0, COND=31, PMI=I12-8, PMM=M14-8,
                // J=1 (DB), CI=0, marker 0x3f.
                let jump = 0x0800_0000 | (31 << 17) | (4 << 14) | (6 << 11) | (1 << 10) | 0x3f;
                let [l0, l1] = split32(load);
                let [j0, j1] = split32(jump);
                return Ok(vec![l0, l1, j0, j1, RFRAME, NOP]);
            }
        };
        Ok(split48(word.encode()?.bits()))
    }
}

fn split48(bits: u64) -> Vec<u16> {
    vec![(bits >> 32) as u16, (bits >> 16) as u16, bits as u16]
}

/// Type 15b (PRM `.16.11`): 1001, I, D, L=0, 0, G=0, 0, 1000, 00, UREG, DATA7.
fn short_offset(data: DataReg, index: Dag1Index, offset: i32, store: bool) -> Option<u32> {
    (-64..=63).contains(&offset).then(|| {
        0x9008_0000
            | (u32::from(index.index()) << 25)
            | (u32::from(store) << 24)
            | (u32::from(data.index()) << 7)
            | (offset as u32 & 0x7f)
    })
}

fn split32(word: u32) -> [u16; 2] {
    [(word >> 16) as u16, word as u16]
}

/// A section-2 parcel stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VisaCode {
    parcels: Vec<u16>,
}

impl VisaCode {
    /// Appends one instruction and returns its first-parcel offset, the unit
    /// that kind-8 and kind-11 relocation records name.
    pub fn push(&mut self, instruction: VisaInstruction) -> Result<VisaParcelOffset, EncodeError> {
        let start = VisaParcelOffset(self.parcels.len() as u32);
        self.parcels.extend(instruction.parcels()?);
        Ok(start)
    }
    pub fn parcels(&self) -> &[u16] {
        &self.parcels
    }
    /// Section-2 bytes: each parcel little-endian, in execution order.
    pub fn to_le_bytes(&self) -> Vec<u8> {
        self.parcels.iter().flat_map(|p| p.to_le_bytes()).collect()
    }
}

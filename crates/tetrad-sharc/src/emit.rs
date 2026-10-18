//! Private pending branch records become an immutable, completely linked ISA image.
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::num::NonZeroU16;

use crate::address::{AddressError, PcRelative24, PcWordOffset, PmWordAddress24};
use crate::isa48::{
    BranchKind, BranchTarget, Condition, EncodeError, Instruction48, InstructionWord48,
};
use crate::register::DataReg;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Label(String);

impl Label {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
    pub fn name(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchRelocation {
    PcRel24,
    AbsPm24,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmitError {
    #[error(transparent)]
    Encode(#[from] EncodeError),
    #[error(transparent)]
    Address(#[from] AddressError),
    #[error("label {0:?} is already defined")]
    DuplicateLabel(Label),
    #[error("label {0:?} is undefined")]
    UndefinedLabel(Label),
    #[error("label {label:?} at word {offset} does not name an instruction")]
    LabelBoundary { label: Label, offset: u32 },
    #[error("instruction at word offset {offset} exceeds the 24-bit PM space (origin {origin:#x})")]
    ProgramBounds { origin: u32, offset: u64 },
    #[error("branch at PC {site:#x} to {target:#x} has out-of-range displacement {delta}")]
    BranchRange { site: u32, target: u32, delta: i64 },
    #[error(
        "loop at PC {site:#x} must end after its DO within rel23; label {label:?} is at {end:#x}"
    )]
    LoopEnd { site: u32, end: u32, label: Label },
}

/// Iteration count of a Type12a `LCNTR = ..., DO ... UNTIL LCE` loop. Zero is
/// unrepresentable as an immediate; a zero register count is a machine error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopCount {
    Immediate(NonZeroU16),
    Register(DataReg),
}

#[derive(Debug)]
enum Pending {
    Encoded(InstructionWord48),
    Branch {
        kind: BranchKind,
        condition: Condition,
        label: Label,
        relocation: BranchRelocation,
    },
    Loop {
        count: LoopCount,
        last: Label,
    },
}

#[derive(Debug)]
pub struct IsaProgram {
    origin: PmWordAddress24,
    instructions: Vec<Pending>,
    labels: BTreeMap<Label, PcWordOffset>,
}

impl IsaProgram {
    pub fn new(origin: PmWordAddress24) -> Self {
        Self {
            origin,
            instructions: Vec::new(),
            labels: BTreeMap::new(),
        }
    }

    pub fn define(&mut self, label: Label) -> Result<(), EmitError> {
        let offset = self.next_offset()?;
        match self.labels.entry(label) {
            Entry::Occupied(entry) => Err(EmitError::DuplicateLabel(entry.key().clone())),
            Entry::Vacant(entry) => {
                entry.insert(offset);
                Ok(())
            }
        }
    }

    pub fn push(&mut self, instruction: Instruction48) -> Result<PcWordOffset, EmitError> {
        let offset = self.next_offset()?;
        let word = instruction.encode()?;
        self.instructions.push(Pending::Encoded(word));
        Ok(offset)
    }

    pub fn branch_to(
        &mut self,
        kind: BranchKind,
        condition: Condition,
        label: Label,
        relocation: BranchRelocation,
    ) -> Result<PcWordOffset, EmitError> {
        let offset = self.next_offset()?;
        self.instructions.push(Pending::Branch {
            kind,
            condition,
            label,
            relocation,
        });
        Ok(offset)
    }

    /// `LCNTR = count, DO last UNTIL LCE`; `last` labels the final body word.
    pub fn counted_loop(
        &mut self,
        count: LoopCount,
        last: Label,
    ) -> Result<PcWordOffset, EmitError> {
        let offset = self.next_offset()?;
        self.instructions.push(Pending::Loop { count, last });
        Ok(offset)
    }

    fn next_offset(&self) -> Result<PcWordOffset, EmitError> {
        let offset = self.instructions.len() as u64;
        if u64::from(self.origin.get()) + offset > 0xff_ffff {
            return Err(EmitError::ProgramBounds {
                origin: self.origin.get(),
                offset,
            });
        }
        Ok(PcWordOffset(offset as u32))
    }

    pub fn finish(self) -> Result<OwnedCode, EmitError> {
        let mut symbols = BTreeMap::new();
        for (label, offset) in self.labels {
            if offset.0 as usize >= self.instructions.len() {
                return Err(EmitError::LabelBoundary {
                    label,
                    offset: offset.0,
                });
            }
            symbols.insert(label, PmWordAddress24::new(self.origin.get() + offset.0)?);
        }
        let mut words = Vec::with_capacity(self.instructions.len());
        for (offset, instruction) in self.instructions.into_iter().enumerate() {
            let word = match instruction {
                Pending::Encoded(word) => word,
                Pending::Branch {
                    kind,
                    condition,
                    label,
                    relocation,
                } => {
                    let address = *symbols
                        .get(&label)
                        .ok_or(EmitError::UndefinedLabel(label))?;
                    let site = self.origin.get() + offset as u32;
                    link_branch(kind, condition, site, address, relocation)?
                }
                Pending::Loop { count, last } => {
                    let end = symbols
                        .get(&last)
                        .ok_or_else(|| EmitError::UndefinedLabel(last.clone()))?
                        .get();
                    let site = self.origin.get() + offset as u32;
                    // rel23 is two's complement; bit 23 is the F1 mode bit.
                    if end <= site || end - site > 0x3f_ffff {
                        return Err(EmitError::LoopEnd {
                            site,
                            end,
                            label: last,
                        });
                    }
                    let counter = match count {
                        LoopCount::Immediate(count) => {
                            0x0c00_0000_0000 | (u64::from(count.get()) << 24)
                        }
                        LoopCount::Register(register) => {
                            0x0d00_0000_0000 | (u64::from(register.index()) << 32)
                        }
                    };
                    InstructionWord48::from_bits(counter | u64::from(end - site))?
                }
            };
            words.push(word);
        }
        Ok(OwnedCode {
            origin: self.origin,
            words,
            symbols,
        })
    }
}

fn link_branch(
    kind: BranchKind,
    condition: Condition,
    site: u32,
    address: PmWordAddress24,
    relocation: BranchRelocation,
) -> Result<InstructionWord48, EmitError> {
    let target = match relocation {
        BranchRelocation::AbsPm24 => BranchTarget::Absolute(address),
        BranchRelocation::PcRel24 => {
            let delta = i64::from(address.get()) - i64::from(site);
            let displacement = PcRelative24::new(delta).map_err(|_| EmitError::BranchRange {
                site,
                target: address.get(),
                delta,
            })?;
            BranchTarget::Relative(displacement)
        }
    };
    Ok(Instruction48::Branch {
        kind,
        condition,
        target,
    }
    .encode()?)
}

/// Contains ISA48 code and symbols only. It is not a resident or a boot container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedCode {
    origin: PmWordAddress24,
    words: Vec<InstructionWord48>,
    symbols: BTreeMap<Label, PmWordAddress24>,
}

impl OwnedCode {
    pub fn origin(&self) -> PmWordAddress24 {
        self.origin
    }
    pub fn words(&self) -> &[InstructionWord48] {
        &self.words
    }
    pub fn word_count(&self) -> usize {
        self.words.len()
    }
    pub fn isa48_le_bytes(&self) -> Vec<u8> {
        self.words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect()
    }
    pub fn symbols(&self) -> &BTreeMap<Label, PmWordAddress24> {
        &self.symbols
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_relative_range_limits_and_one_beyond() {
        for (site, target, expected) in
            [(0, 0x7fffff, 0x073e007fffff), (0x800000, 0, 0x073e00800000)]
        {
            let word = link_branch(
                BranchKind::Jump,
                Condition::Always,
                site,
                PmWordAddress24::new(target).unwrap(),
                BranchRelocation::PcRel24,
            )
            .unwrap();
            assert_eq!(word.bits(), expected);
        }
        for (site, target, delta) in [(0, 0x800000, 0x800000), (0x800001, 0, -0x800001)] {
            assert_eq!(
                link_branch(
                    BranchKind::Jump,
                    Condition::Always,
                    site,
                    PmWordAddress24::new(target).unwrap(),
                    BranchRelocation::PcRel24
                ),
                Err(EmitError::BranchRange {
                    site,
                    target,
                    delta
                })
            );
        }
    }
}

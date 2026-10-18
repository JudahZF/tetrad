//! PC words, DM normal words, container bytes and VISA parcels are separate units.

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AddressError {
    #[error("PM word address {0:#x} exceeds unsigned 24 bits")]
    PmRange(u32),
    #[error("PC displacement {0} exceeds signed 24 bits")]
    RelativeRange(i64),
    #[error("DM normal-word address arithmetic overflow")]
    DmOverflow,
}

/// Program addresses cannot be substituted with container offsets.
///
/// ```compile_fail
/// use tetrad_sharc::{IsaProgram, address::ContainerByteOffset};
/// IsaProgram::new(ContainerByteOffset(54));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PmWordAddress24(u32);

impl PmWordAddress24 {
    pub const fn new(value: u32) -> Result<Self, AddressError> {
        if value <= 0xff_ffff {
            Ok(Self(value))
        } else {
            Err(AddressError::PmRange(value))
        }
    }
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A checked displacement in ISA48 PC words, distinct from VISA parcels.
///
/// ```compile_fail
/// use tetrad_sharc::address::VisaParcelOffset;
/// use tetrad_sharc::isa48::BranchTarget;
/// BranchTarget::Relative(VisaParcelOffset(12));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcRelative24(i32);

impl PcRelative24 {
    pub const fn new(value: i64) -> Result<Self, AddressError> {
        if value >= -0x80_0000 && value <= 0x7f_ffff {
            Ok(Self(value as i32))
        } else {
            Err(AddressError::RelativeRange(value))
        }
    }
    pub const fn get(self) -> i32 {
        self.0
    }
    pub(crate) const fn bits(self) -> u32 {
        self.0 as u32 & 0xff_ffff
    }
}

/// An explicit normal-word DM alias, with no inferred physical/byte mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DmWordAddress32(u32);

impl DmWordAddress32 {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u32 {
        self.0
    }
    pub fn checked_add(self, words: u32) -> Result<Self, AddressError> {
        self.0
            .checked_add(words)
            .map(Self)
            .ok_or(AddressError::DmOverflow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PcWordOffset(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContainerByteOffset(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct VisaParcelOffset(pub u32);

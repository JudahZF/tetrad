//! Ordinary IEEE nearest-even and gradual-underflow policy. No FMA, fast math,
//! target FPSCR, FTZ, exception or signaling-NaN emulation is claimed.
use super::{AdmissionError, AdmissionReason, Path};

pub const RELATIVE_F32_TOLERANCE_BITS: u32 = 0x3727_c5ac;
pub const RELATIVE_F64_TOLERANCE_BITS: u64 = 0x3d71_9799_812d_ea11;

#[derive(Debug, Clone, Copy, Default)]
pub struct F32(u32);
impl F32 {
    pub fn from_bits(bits: u32) -> Result<Self, AdmissionError> {
        if bits & 0x7f80_0000 == 0x7f80_0000 && bits & 0x007f_ffff != 0 && bits & 0x0040_0000 == 0 {
            return Err(AdmissionError {
                path: Path::root("f32"),
                reason: AdmissionReason::SignalingNaN,
            });
        }
        Ok(Self(bits))
    }
    pub fn bits(self) -> u32 {
        self.0
    }
    pub fn value(self) -> f32 {
        f32::from_bits(self.0)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct F64(u64);
impl F64 {
    pub fn from_bits(bits: u64) -> Result<Self, AdmissionError> {
        if bits & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000
            && bits & 0x000f_ffff_ffff_ffff != 0
            && bits & 0x0008_0000_0000_0000 == 0
        {
            return Err(AdmissionError {
                path: Path::root("f64"),
                reason: AdmissionReason::SignalingNaN,
            });
        }
        Ok(Self(bits))
    }
    pub fn bits(self) -> u64 {
        self.0
    }
    pub fn value(self) -> f64 {
        f64::from_bits(self.0)
    }
}

/// Rounded f32 subtraction and multiplication, with no absolute tolerance floor.
pub fn plain_relative_f32(lhs: F32, rhs: F32) -> bool {
    relative32(lhs.value(), rhs.value())
}
pub(crate) fn relative32(lhs: f32, rhs: f32) -> bool {
    if lhs.is_nan() || rhs.is_nan() {
        return false;
    }
    let difference = (lhs - rhs).abs();
    let lhs_magnitude = lhs.abs();
    let rhs_magnitude = rhs.abs();
    let minimum = if lhs_magnitude < rhs_magnitude {
        lhs_magnitude
    } else {
        rhs_magnitude
    };
    let threshold = minimum * f32::from_bits(RELATIVE_F32_TOLERANCE_BITS);
    threshold >= difference
}

pub fn same_float_f32(lhs: F32, rhs: F32) -> bool {
    (lhs.value().is_nan() && rhs.value().is_nan()) || plain_relative_f32(lhs, rhs)
}

pub fn same_double_f64(lhs: F64, rhs: F64) -> bool {
    let lhs = lhs.value();
    let rhs = rhs.value();
    if lhs.is_nan() && rhs.is_nan() {
        return true;
    }
    if lhs.is_nan() || rhs.is_nan() {
        return false;
    }
    let difference = (lhs - rhs).abs();
    let minimum = if lhs.abs() < rhs.abs() {
        lhs.abs()
    } else {
        rhs.abs()
    };
    let threshold = minimum * f64::from_bits(RELATIVE_F64_TOLERANCE_BITS);
    threshold >= difference
}

/// Raw VCMP equality; signed zeros equal and every NaN unequal.
pub fn raw_exact_f32(lhs: F32, rhs: F32) -> bool {
    lhs.value() == rhs.value()
}
pub fn raw_exact_f64(lhs: F64, rhs: F64) -> bool {
    lhs.value() == rhs.value()
}

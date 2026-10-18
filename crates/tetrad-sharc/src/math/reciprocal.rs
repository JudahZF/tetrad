//! Owned reciprocal: integer seed and Newton steps (no `RECIPS` seed ROM).
use super::Asm;
use super::Leaf;
use super::Op::{FClip, FCopySign, FMax, FMul, FSub, ISub};
use super::Scratch::{R0, R2, R4, R8};
use super::clip;
use crate::emit::EmitError;
use crate::isa48::Instruction48;

/// |x| is clamped to [2^-125, 2^125]: the seed of 2^125 is still normal.
pub const RECIPROCAL_MIN: f32 = f32::from_bits(0x0100_0000);
pub const RECIPROCAL_MAX: f32 = f32::from_bits(0x7e00_0000);
/// bits(1/x) ≈ SEED − bits(x) within ±5.06 % (the `tetrad-capture` ADAA seed).
const SEED: u32 = 0x7ef3_11c7;
/// Error 5.06e-2 → 2.6e-3 → 6.5e-6 → 4.2e-11: three steps reach f32 rounding.
const NEWTON_STEPS: usize = 3;

/// The seed takes the sign of x, so each step r = (2 − x·r)·r keeps it.
pub fn reciprocal(x: f32) -> f32 {
    let x = clip(x, RECIPROCAL_MAX);
    let magnitude = x.abs().max(RECIPROCAL_MIN);
    let x = magnitude.copysign(x);
    let mut r = f32::from_bits(SEED - magnitude.to_bits()).copysign(x);
    for _ in 0..NEWTON_STEPS {
        r = (2.0 - x * r) * r;
    }
    r
}

pub(super) fn emit_reciprocal(a: &mut Asm) -> Result<(), EmitError> {
    a.label(Leaf::Reciprocal.label())?;
    a.set(R2, RECIPROCAL_MAX.to_bits())?;
    a.op(FClip, R4, R4, R2)?;
    a.push(Instruction48::FloatAbs {
        dst: R8.into(),
        src: R4.into(),
    })?;
    a.set(R2, RECIPROCAL_MIN.to_bits())?;
    a.op(FMax, R8, R8, R2)?;
    a.op(FCopySign, R4, R8, R4)?;
    a.set(R2, SEED)?;
    a.op(ISub, R0, R2, R8)?;
    a.op(FCopySign, R0, R0, R4)?;
    a.set(R8, 2.0f32.to_bits())?;
    for _ in 0..NEWTON_STEPS {
        a.op(FMul, R2, R4, R0)?;
        a.op(FSub, R2, R8, R2)?;
        a.op(FMul, R0, R2, R0)?;
    }
    a.push(Instruction48::Rts)
}

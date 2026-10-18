//! `exp2` and dB → linear.
use super::Op::{FClip, FMul, FSub, IAdd};
use super::Scratch::{R0, R2, R4, R8};
use super::{Asm, Leaf, clip, horner};
use crate::emit::EmitError;
use crate::isa48::Instruction48;

/// `exp2` clips its argument to ±126, so 2^n and the result stay normal.
pub const EXP2_LIMIT: f32 = 126.0;
/// log2(10)/20: dB → `exp2` argument.
const DB_TO_LOG2: f32 = f32::from_bits(0x3e2a_152d);
/// 2^f on [-1/2, 1/2]; fit relative error 1.9e-9. c0 = 1 keeps 2^n exact.
const EXP2: [f32; 7] = [
    1.0,
    f32::from_bits(0x3f31_7218),
    f32::from_bits(0x3e75_fded),
    f32::from_bits(0x3d63_576a),
    f32::from_bits(0x3c1d_96de),
    f32::from_bits(0x3aaf_a2b5),
    f32::from_bits(0x3920_e99a),
];

/// n = round(x), f = x − n, 2^x = p(f)·2^n with 2^n added to the exponent bits.
/// For n = −126, x ≥ −126 gives f ≥ 0 and p ≥ 1, so the result stays normal.
pub fn exp2(x: f32) -> f32 {
    let x = clip(x, EXP2_LIMIT);
    let n = x.round_ties_even() as i32;
    let f = x - n as f32;
    f32::from_bits(horner(&EXP2, f).to_bits().wrapping_add((n as u32) << 23))
}

/// 10^(dB/20) = `exp2`(dB · log2(10)/20).
pub fn db_to_linear(db: f32) -> f32 {
    exp2(db * DB_TO_LOG2)
}

/// Falls through into `exp2`: [`super::emit`] places it directly before.
pub(super) fn emit_db_to_linear(a: &mut Asm) -> Result<(), EmitError> {
    a.label(Leaf::DbToLinear.label())?;
    a.set(R2, DB_TO_LOG2.to_bits())?;
    a.op(FMul, R4, R4, R2)
}

pub(super) fn emit_exp2(a: &mut Asm) -> Result<(), EmitError> {
    a.label(Leaf::Exp2.label())?;
    a.set(R2, EXP2_LIMIT.to_bits())?;
    a.op(FClip, R4, R4, R2)?;
    a.push(Instruction48::FixToInteger {
        dst: R0.into(),
        src: R4.into(),
    })?;
    a.push(Instruction48::FloatFromInteger {
        dst: R2.into(),
        src: R0.into(),
    })?;
    a.op(FSub, R4, R4, R2)?;
    a.horner(R8, R4, &EXP2)?;
    a.push(Instruction48::LogicalShift {
        dst: R0.into(),
        src: R0.into(),
        amount: 23,
    })?;
    a.op(IAdd, R0, R8, R0)?;
    a.push(Instruction48::Rts)
}

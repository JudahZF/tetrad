//! `log2` and linear → dB.
use super::Op::{And, FAdd, FMax, FMul, FSub, IAdd, ISub};
use super::Scratch::{R0, R1, R2, R4, R8};
use super::{Asm, Leaf, horner};
use crate::emit::EmitError;
use crate::isa48::Instruction48;

/// 20·log10(2): `log2` → dB.
const LOG2_TO_DB: f32 = f32::from_bits(0x40c0_a8c1);
/// bits(1) − bits(√½): moves mantissas ≥ √2 into the next binade.
const SQRT_HALF_SHIFT: u32 = 0x3f80_0000 - SQRT_HALF;
const SQRT_HALF: u32 = 0x3f35_04f3;
/// log2(1+t)/t on [√½ − 1, √2 − 1]; fit relative error 4.1e-9.
const LOG2: [f32; 10] = [
    f32::from_bits(0x3fb8_aa3b),
    f32::from_bits(0xbf38_aa38),
    f32::from_bits(0x3ef6_384b),
    f32::from_bits(0xbeb8_ad3d),
    f32::from_bits(0x3e93_bf20),
    f32::from_bits(0xbe75_5df7),
    f32::from_bits(0x3e51_81e6),
    f32::from_bits(0xbe43_a7b1),
    f32::from_bits(0x3e3e_abc1),
    f32::from_bits(0xbde1_363b),
];

/// x = 2^k·m with m in [√½, √2) from integer bit steps; t = m − 1 is exact,
/// log2 x = k + t·q(t). Inputs below 2^-126 (zero, negative) give −126.
pub fn log2(x: f32) -> f32 {
    let x = x.max(f32::MIN_POSITIVE);
    let bits = x.to_bits().wrapping_add(SQRT_HALF_SHIFT);
    let k = ((bits >> 23) as i32 - 127) as f32;
    let t = f32::from_bits((bits & 0x007f_ffff) + SQRT_HALF) - 1.0;
    k + horner(&LOG2, t) * t
}

/// 20·log10(x) = `log2`(x) · 20·log10(2).
pub fn linear_to_db(x: f32) -> f32 {
    log2(x) * LOG2_TO_DB
}

pub(super) fn emit_log2(a: &mut Asm) -> Result<(), EmitError> {
    a.label(Leaf::Log2.label())?;
    a.set(R2, f32::MIN_POSITIVE.to_bits())?;
    a.op(FMax, R1, R4, R2)?;
    a.set(R2, SQRT_HALF_SHIFT)?;
    a.op(IAdd, R1, R1, R2)?;
    a.push(Instruction48::LogicalShift {
        dst: R0.into(),
        src: R1.into(),
        amount: -23,
    })?;
    a.set(R2, 127)?;
    a.op(ISub, R0, R0, R2)?;
    a.push(Instruction48::FloatFromInteger {
        dst: R0.into(),
        src: R0.into(),
    })?;
    a.set(R2, 0x007f_ffff)?;
    a.op(And, R1, R1, R2)?;
    a.set(R2, SQRT_HALF)?;
    a.op(IAdd, R1, R1, R2)?;
    a.set(R2, 1.0f32.to_bits())?;
    a.op(FSub, R1, R1, R2)?;
    a.horner(R8, R1, &LOG2)?;
    a.op(FMul, R8, R8, R1)?;
    a.op(FAdd, R0, R0, R8)?;
    a.push(Instruction48::Rts)
}

pub(super) fn emit_linear_to_db(a: &mut Asm) -> Result<(), EmitError> {
    a.label(Leaf::LinearToDb.label())?;
    a.call(Leaf::Log2)?;
    a.set(R2, LOG2_TO_DB.to_bits())?;
    a.op(FMul, R0, R0, R2)?;
    a.push(Instruction48::Rts)
}

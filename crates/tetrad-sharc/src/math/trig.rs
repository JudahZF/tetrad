//! `sin`, `cos` and `tan` in radians.
use super::Op::{And, FAdd, FClip, FMul, FSub, IAdd, Xor};
use super::Scratch::{self, R0, R1, R2, R4, R8, R12};
use super::{Asm, Leaf, clip, horner, reciprocal};
use crate::emit::{EmitError, Label};
use crate::isa48::{Condition, Instruction48};
use crate::register::UniversalReg;

/// Arguments are clipped to ±1024, so |n| ≤ 652 < 2^10.
pub const TRIG_LIMIT: f32 = 1024.0;
const FRAC_2_PI: f32 = f32::from_bits(0x3f22_f983);
/// π/2 = C1 + C2 + C3 + C4 + 1.1e-23 (Cody–Waite, split from the hex digits
/// of π, not from f64 π). C1..C3 have 14 significant bits, so n·Ci is exact
/// for |n| < 2^10; only n·C4 rounds.
const HALF_PI: [f32; 4] = [
    f32::from_bits(0x3fc9_1000),
    f32::from_bits(0xb695_7800),
    f32::from_bits(0x2e85_a400),
    f32::from_bits(0xa6f7_2ced),
];
/// sin r = r + r·z·s(z), z = r², |r| ≤ π/4 (+0.1 %); fit relative error 3.3e-9.
const SIN: [f32; 3] = [
    f32::from_bits(0xbe2a_aaa0),
    f32::from_bits(0x3c08_82fd),
    f32::from_bits(0xb94c_7c07),
];
/// cos r = c(z); fit relative error 5.7e-11.
const COS: [f32; 5] = [
    1.0,
    -0.5,
    f32::from_bits(0x3d2a_aa9c),
    f32::from_bits(0xbab6_036e),
    f32::from_bits(0x37cc_721c),
];
const SIGN: u32 = 1 << 31;

fn core() -> Label {
    Label::new("math.trig")
}

/// x = n·π/2 + r with |r| ≤ π/4 (to rounding).
fn reduce(x: f32) -> (i32, f32) {
    let x = clip(x, TRIG_LIMIT);
    let n = (x * FRAC_2_PI).round_ties_even() as i32;
    let nf = n as f32;
    (n, HALF_PI.iter().fold(x, |r, &c| r - nf * c))
}

fn sin_poly(r: f32, z: f32) -> f32 {
    r + horner(&SIN, z) * z * r
}

/// Quadrant q = n + offset: bit 0 selects the polynomial, bit 1 the sign.
fn sin_cos(x: f32, offset: i32) -> f32 {
    let (n, r) = reduce(x);
    let quadrant = n.wrapping_add(offset);
    let z = r * r;
    let value = if quadrant & 1 == 0 {
        sin_poly(r, z)
    } else {
        horner(&COS, z)
    };
    f32::from_bits(value.to_bits() ^ (((quadrant & 2) as u32) << 30))
}

pub fn sin(x: f32) -> f32 {
    sin_cos(x, 0)
}

/// sin(x + π/2): one quadrant on, with no rounding in the shift.
pub fn cos(x: f32) -> f32 {
    sin_cos(x, 1)
}

/// sin/cos for even n, −cos/sin for odd n, through the owned reciprocal.
pub fn tan(x: f32) -> f32 {
    let (n, r) = reduce(x);
    let z = r * r;
    let s = sin_poly(r, z);
    let c = horner(&COS, z);
    if n & 1 == 0 {
        s * reciprocal(c)
    } else {
        c * reciprocal(-s)
    }
}

/// R0 = n, F4 = r; uses R1 (n as float) and R2.
fn emit_reduce(a: &mut Asm) -> Result<(), EmitError> {
    a.set(R2, TRIG_LIMIT.to_bits())?;
    a.op(FClip, R4, R4, R2)?;
    a.set(R2, FRAC_2_PI.to_bits())?;
    a.op(FMul, R0, R4, R2)?;
    a.push(Instruction48::FixToInteger {
        dst: R0.into(),
        src: R0.into(),
    })?;
    a.push(Instruction48::FloatFromInteger {
        dst: R1.into(),
        src: R0.into(),
    })?;
    for c in HALF_PI {
        a.set(R2, c.to_bits())?;
        a.op(FMul, R2, R1, R2)?;
        a.op(FSub, R4, R4, R2)?;
    }
    Ok(())
}

/// acc = sin r from F4 = r and F8 = z.
fn emit_sin_poly(a: &mut Asm, acc: Scratch) -> Result<(), EmitError> {
    a.horner(acc, R8, &SIN)?;
    a.op(FMul, acc, acc, R8)?;
    a.op(FMul, acc, acc, R4)?;
    a.op(FAdd, acc, R4, acc)
}

/// R0 = n & 1, which sets AZ for the parity branch.
fn emit_parity(a: &mut Asm) -> Result<(), EmitError> {
    a.set(R2, 1)?;
    a.op(And, R0, R0, R2)
}

/// `sin` sets the quadrant offset R12 = 0 and falls into the shared core.
pub(super) fn emit_sin(a: &mut Asm) -> Result<(), EmitError> {
    let cos_poly = Label::new("math.trig.cos_poly");
    a.label(Leaf::Sin.label())?;
    a.set(R12, 0)?;
    a.label(core())?;
    emit_reduce(a)?;
    a.op(IAdd, R0, R0, R12)?;
    a.op(FMul, R8, R4, R4)?;
    // R12 = (q & 2) << 30: the sign mask.
    a.set(R2, 2)?;
    a.op(And, R12, R0, R2)?;
    a.push(Instruction48::LogicalShift {
        dst: R12.into(),
        src: R12.into(),
        amount: 30,
    })?;
    emit_parity(a)?;
    a.jump(Condition::NotEqual, cos_poly.clone())?;
    emit_sin_poly(a, R1)?;
    a.op(Xor, R0, R1, R12)?;
    a.push(Instruction48::Rts)?;
    a.label(cos_poly)?;
    a.horner(R1, R8, &COS)?;
    a.op(Xor, R0, R1, R12)?;
    a.push(Instruction48::Rts)
}

pub(super) fn emit_cos(a: &mut Asm) -> Result<(), EmitError> {
    a.label(Leaf::Cos.label())?;
    a.set(R12, 1)?;
    a.jump(Condition::Always, core())
}

/// F12 = sin r and F1 = cos r survive the reciprocal CALL.
pub(super) fn emit_tan(a: &mut Asm) -> Result<(), EmitError> {
    let odd = Label::new("math.tan.odd");
    a.label(Leaf::Tan.label())?;
    emit_reduce(a)?;
    a.op(FMul, R8, R4, R4)?;
    emit_sin_poly(a, R12)?;
    a.horner(R1, R8, &COS)?;
    emit_parity(a)?;
    a.jump(Condition::NotEqual, odd.clone())?;
    a.push(Instruction48::RegisterMove {
        dst: UniversalReg::Data(R4.into()),
        src: UniversalReg::Data(R1.into()),
    })?;
    a.call(Leaf::Reciprocal)?;
    a.op(FMul, R0, R12, R0)?;
    a.push(Instruction48::Rts)?;
    a.label(odd)?;
    a.set(R2, SIGN)?;
    a.op(Xor, R4, R12, R2)?;
    a.call(Leaf::Reciprocal)?;
    a.op(FMul, R0, R1, R0)?;
    a.push(Instruction48::Rts)
}

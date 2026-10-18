//! Shared Type2a result semantics for the leaf simulator and the resident machine.
//! [`evaluate`] uses host `f32`; see the crate README for its limits. [`evaluate_prm`]
//! is the opt-in PRM model on 40-bit registers. The ISA has no fused forms: PRM 5.45
//! multifunction computations work as separate single-function operations.

use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComputeOp {
    FloatMultiply,
    FloatAdd,
    FloatSubtract,
    FloatAbs,
    FloatPass,
    FloatNegate,
    FloatMin,
    FloatMax,
    FloatClip,
    FloatCopySign,
    FloatCompare,
    IntegerPass,
    IntegerDecrement,
    IntegerSubtract,
    IntegerAdd,
    IntegerIncrement,
    IntegerCompare,
    /// `COMPU(Rx, Ry)`: AN means `Rx < Ry` unsigned.
    IntegerCompareUnsigned,
    And,
    Or,
    Xor,
    /// `Fn = FLOAT Rx`: signed fixed-point to float.
    FloatFromInteger,
    /// `Rn = FIX Fx`: float to signed fixed-point, round to nearest (MODE1
    /// TRUNCATE clear). Both models refuse ALUSAT, so this is the PRM 23.15
    /// non-saturating form: NaN, ±infinity and |Fx| >= 2^31 (AV: biased
    /// exponent > 157) return all ones, not a clipped value.
    FixToInteger,
}

impl ComputeOp {
    /// Leading sources (RX, then RY) read as floats. Integer forms read none.
    pub(crate) const fn float_sources(self) -> usize {
        match self {
            Self::IntegerPass
            | Self::IntegerDecrement
            | Self::IntegerSubtract
            | Self::IntegerAdd
            | Self::IntegerIncrement
            | Self::IntegerCompare
            | Self::IntegerCompareUnsigned
            | Self::And
            | Self::Or
            | Self::Xor
            | Self::FloatFromInteger => 0,
            Self::FloatAbs | Self::FloatPass | Self::FloatNegate | Self::FixToInteger => 1,
            _ => 2,
        }
    }

    /// The register result is a float view (for AZ/AN and float policies).
    pub(crate) const fn float_result(self) -> bool {
        match self {
            Self::FixToInteger => false,
            Self::FloatFromInteger => true,
            _ => self.float_sources() > 0,
        }
    }
}

const SIGN: u32 = 1 << 31;
/// 2^31: the smallest magnitude `FIX` flags as overflow. The largest f32 below
/// it is 2^31 - 128, so rounding cannot cross the bound.
const FIX_OVERFLOW: f32 = 2_147_483_648.0;

/// Result bits, or `None` for compares (flags only). Manual 23.09/.23/.25/.26:
/// ABS/MAX/CLIP/COPYSIGN return all ones for a NaN input.
pub(crate) fn evaluate(operation: ComputeOp, x: u32, y: u32) -> Option<u32> {
    let (fx, fy) = (f32::from_bits(x), f32::from_bits(y));
    let nan = fx.is_nan() || fy.is_nan();
    match operation {
        ComputeOp::IntegerPass => Some(x),
        ComputeOp::IntegerDecrement => Some(x.wrapping_sub(1)),
        ComputeOp::IntegerSubtract => Some(x.wrapping_sub(y)),
        ComputeOp::IntegerAdd => Some(x.wrapping_add(y)),
        ComputeOp::IntegerIncrement => Some(x.wrapping_add(1)),
        ComputeOp::And => Some(x & y),
        ComputeOp::Or => Some(x | y),
        ComputeOp::Xor => Some(x ^ y),
        ComputeOp::FloatFromInteger => Some((x as i32 as f32).to_bits()),
        ComputeOp::FixToInteger if fx.is_nan() || fx.abs() >= FIX_OVERFLOW => Some(u32::MAX),
        ComputeOp::FixToInteger => Some(fx.round_ties_even() as i32 as u32),
        ComputeOp::FloatMultiply => Some((fx * fy).to_bits()),
        ComputeOp::FloatAdd => Some((fx + fy).to_bits()),
        ComputeOp::FloatSubtract => Some((fx - fy).to_bits()),
        ComputeOp::FloatAbs | ComputeOp::FloatPass | ComputeOp::FloatNegate if fx.is_nan() => {
            Some(u32::MAX)
        }
        ComputeOp::FloatPass => Some(x),
        ComputeOp::FloatNegate => Some(x ^ SIGN),
        ComputeOp::FloatAbs => Some(x & !SIGN),
        ComputeOp::FloatMin
        | ComputeOp::FloatMax
        | ComputeOp::FloatClip
        | ComputeOp::FloatCopySign
            if nan =>
        {
            Some(u32::MAX)
        }
        ComputeOp::FloatMin => Some(if fx < fy {
            x
        } else if fy < fx {
            y
        } else {
            x | y
        }),
        ComputeOp::FloatMax => Some(if fx > fy {
            x
        } else if fy > fx {
            y
        } else {
            // Equal values: max(+0, -0) is +0, otherwise the bits match.
            x & y
        }),
        ComputeOp::FloatClip => Some(if fx.abs() < fy.abs() {
            x
        } else {
            (y & !SIGN) | (x & SIGN)
        }),
        ComputeOp::FloatCopySign => Some((x & !SIGN) | (y & SIGN)),
        ComputeOp::FloatCompare | ComputeOp::IntegerCompare | ComputeOp::IntegerCompareUnsigned => {
            None
        }
    }
}

/// Sign of a 40-bit register float. PRM 32.3: the IEEE layout with a 31-bit fraction.
pub(crate) const SIGN40: u64 = 1 << 39;
const EXPONENT40: u64 = 0xff << 31;
const FRACTION40: u64 = (1 << 31) - 1;
/// PRM 5.84: a NaN input returns a quiet NaN of all ones. Invalid operations
/// (infinity minus infinity, zero times infinity) return the same (inferred: the
/// PRM names only the AI/MI flags for them).
const ALL_ONES40: u64 = (1 << 40) - 1;

/// PRM result for one compute on 40-bit registers, or `None` for compares. Bits
/// 39:8 are the `R`/`F` word and bits 7:0 the extension field (PRM 4.06). `rnd32`
/// is MODE1 RND32. Rounding is to nearest even: both models refuse TRUNCATE.
pub(crate) fn evaluate_prm(operation: ComputeOp, x: u64, y: u64, rnd32: bool) -> Option<u64> {
    // PRM 4.06: fixed-point operands are bits 39:8; 32-bit results clear bits 7:0.
    let (rx, ry) = ((x >> 8) as u32, (y >> 8) as u32);
    let (fx, fy) = (operand(x, rnd32), operand(y, rnd32));
    let precision = if rnd32 { 24 } else { 32 };
    let nan = [fx, fy]
        .into_iter()
        .take(operation.float_sources())
        .any(|bits| value(bits).is_nan());
    Some(match operation {
        ComputeOp::FloatCompare | ComputeOp::IntegerCompare | ComputeOp::IntegerCompareUnsigned => {
            return None;
        }
        ComputeOp::IntegerPass
        | ComputeOp::IntegerDecrement
        | ComputeOp::IntegerSubtract
        | ComputeOp::IntegerAdd
        | ComputeOp::IntegerIncrement
        | ComputeOp::And
        | ComputeOp::Or
        | ComputeOp::Xor => u64::from(evaluate(operation, rx, ry)?) << 8,
        // PRM 23.19: FLOAT rounds at the 40-bit boundary whatever RND32 says. A
        // 32-bit significand holds every i32, so the result is exact.
        ComputeOp::FloatFromInteger => {
            let integer = rx as i32;
            round(integer < 0, u128::from(integer.unsigned_abs()), 0, 32)
        }
        ComputeOp::FixToInteger => fix(fx),
        // PRM 23.02/.03/.09/.23/.25/.26: a NaN input returns all ones.
        _ if nan => ALL_ONES40,
        ComputeOp::FloatMultiply => multiply(fx, fy, precision),
        ComputeOp::FloatAdd => add(fx, fy, precision),
        ComputeOp::FloatSubtract => add(fx, fy ^ SIGN40, precision),
        // PRM 23.09: a denormal input flushes to +zero.
        ComputeOp::FloatAbs => fx & !SIGN40,
        ComputeOp::FloatPass => fx,
        ComputeOp::FloatNegate => fx ^ SIGN40,
        ComputeOp::FloatMin => match value(fx).partial_cmp(&value(fy)) {
            Some(Ordering::Less) => fx,
            Some(Ordering::Greater) => fy,
            _ => fx | fy,
        },
        // PRM 23.25: MAX(+0, -0) is +0.
        ComputeOp::FloatMax => match value(fx).partial_cmp(&value(fy)) {
            Some(Ordering::Greater) => fx,
            Some(Ordering::Less) => fy,
            _ => fx & fy,
        },
        ComputeOp::FloatClip if value(fx).abs() < value(fy).abs() => fx,
        ComputeOp::FloatClip => (fy & !SIGN40) | (fx & SIGN40),
        ComputeOp::FloatCopySign => (fx & !SIGN40) | (fy & SIGN40),
    })
}

/// PRM 23.07 `COMP(Fx, Fy)` after the operand rules, so ±0 and denormals compare
/// equal. `None` if an operand is NaN: the PRM sets AI and defines no order.
pub(crate) fn compare_prm(x: u64, y: u64, rnd32: bool) -> Option<Ordering> {
    value(operand(x, rnd32)).partial_cmp(&value(operand(y, rnd32)))
}

/// PRM 5.84 input rules: RND32 clears the eight operand LSBs, and a denormal
/// flushes to zero of the same sign (PRM 23.02 "±zero").
fn operand(bits: u64, rnd32: bool) -> u64 {
    let bits = if rnd32 { bits & !0xff } else { bits };
    if bits & EXPONENT40 == 0 {
        bits & SIGN40
    } else {
        bits
    }
}

/// Exact value of a flushed 40-bit float (an f64 holds every one); NaN stays NaN.
fn value(bits: u64) -> f64 {
    let sign = (bits & SIGN40) << 24;
    let exponent = match (bits & EXPONENT40) >> 31 {
        0 => 0,
        0xff => 0x7ff,
        biased => biased + 1023 - 127,
    };
    f64::from_bits(sign | (exponent << 52) | ((bits & FRACTION40) << 21))
}

/// `(significand, scale)` with value = significand × 2^scale; zero is `(0, 0)`.
fn unpack(bits: u64) -> (u64, i32) {
    match ((bits & EXPONENT40) >> 31) as i32 {
        0 => (0, 0),
        biased => ((bits & FRACTION40) | (1 << 31), biased - 127 - 31),
    }
}

/// `magnitude / 2^shift`, rounded to nearest even. Callers keep magnitudes below
/// 2^127, so a shift of 128 or more rounds to zero.
fn shift_round(magnitude: u128, shift: u32) -> u128 {
    match shift {
        0 => magnitude,
        128.. => 0,
        _ => {
            let kept = magnitude >> shift;
            let rest = magnitude & ((1 << shift) - 1);
            let half = 1 << (shift - 1);
            kept + u128::from(rest > half || (rest == half && kept & 1 == 1))
        }
    }
}

/// Round `magnitude × 2^scale` to `precision` significand bits (24 under RND32,
/// else 32) and pack. PRM 23.02: post-rounded overflow returns ±infinity and a
/// post-rounded denormal returns ±zero. The multiplier uses the same rules
/// (inferred: PRM 26.2 names only the MU/MV flags).
fn round(negative: bool, magnitude: u128, scale: i32, precision: u32) -> u64 {
    let sign = if negative { SIGN40 } else { 0 };
    let width = 128 - magnitude.leading_zeros();
    if width == 0 {
        return sign;
    }
    let (mut significand, mut scale) = if width > precision {
        let shift = width - precision;
        (shift_round(magnitude, shift), scale + shift as i32)
    } else {
        let shift = precision - width;
        (magnitude << shift, scale - shift as i32)
    };
    if significand >> precision != 0 {
        significand >>= 1;
        scale += 1;
    }
    let fraction = (significand as u64) & ((1 << (precision - 1)) - 1);
    match scale + precision as i32 - 1 + 127 {
        ..=0 => sign,
        255.. => sign | EXPONENT40,
        biased => sign | ((biased as u64) << 31) | (fraction << (32 - precision)),
    }
}

/// Sum of two flushed, non-NaN operands with one rounding.
fn add(x: u64, y: u64, precision: u32) -> u64 {
    let (x_negative, y_negative) = (x & SIGN40 != 0, y & SIGN40 != 0);
    let infinite = |bits: u64| bits & EXPONENT40 == EXPONENT40;
    match (infinite(x), infinite(y)) {
        // PRM 23.02 AI: opposite-signed infinities are invalid.
        (true, true) if x_negative != y_negative => return ALL_ONES40,
        (true, _) => return x,
        (_, true) => return y,
        _ => {}
    }
    let ((xs, xe), (ys, ye)) = (unpack(x), unpack(y));
    let top = match (xs, ys) {
        // IEEE: -0 + -0 is -0; every other zero sum is +0.
        (0, 0) => return if x_negative && y_negative { SIGN40 } else { 0 },
        (0, _) => ye,
        (_, 0) => xe,
        _ => xe.max(ye),
    };
    // Align on the larger scale with 64 guard bits. Bits that a long shift drops
    // lie over 30 bits below the rounding position, so they cannot change the sum.
    let align = |significand: u64, scale: i32, negative: bool| {
        if significand == 0 {
            return 0;
        }
        let aligned = (u128::from(significand) << 64)
            .checked_shr((top - scale) as u32)
            .unwrap_or(0) as i128;
        if negative { -aligned } else { aligned }
    };
    let sum = align(xs, xe, x_negative) + align(ys, ye, y_negative);
    round(sum < 0, sum.unsigned_abs(), top - 64, precision)
}

/// Product of two flushed, non-NaN operands with one rounding.
fn multiply(x: u64, y: u64, precision: u32) -> u64 {
    let sign = (x ^ y) & SIGN40;
    let infinite = x & EXPONENT40 == EXPONENT40 || y & EXPONENT40 == EXPONENT40;
    let zero = x & EXPONENT40 == 0 || y & EXPONENT40 == 0;
    match (infinite, zero) {
        // PRM 26.2 MI: infinity times zero is invalid.
        (true, true) => ALL_ONES40,
        (true, false) => sign | EXPONENT40,
        (false, true) => sign,
        (false, false) => {
            let ((xs, xe), (ys, ye)) = (unpack(x), unpack(y));
            round(
                sign != 0,
                u128::from(xs) * u128::from(ys),
                xe + ye,
                precision,
            )
        }
    }
}

/// PRM 23.15 `FIX` with ALUSAT and TRUNCATE clear: round to nearest even. NaN,
/// ±infinity and |result| >= 2^31 return all ones, as in [`evaluate`]. An integer
/// result has a zero extension field.
fn fix(bits: u64) -> u64 {
    let (significand, scale) = unpack(bits);
    // A normal significand is at least 2^31, so scale >= 0 overflows. NaN and
    // ±infinity (biased exponent 255) take that path too.
    let magnitude = match (significand, scale) {
        (0, _) => 0,
        (_, 0..) => return ALL_ONES40,
        _ => shift_round(u128::from(significand), scale.unsigned_abs()),
    };
    match i32::try_from(magnitude) {
        Ok(magnitude) if bits & SIGN40 != 0 => u64::from((-magnitude) as u32) << 8,
        Ok(magnitude) => u64::from(magnitude as u32) << 8,
        Err(_) => ALL_ONES40,
    }
}

//! Host twins of single float computes under the kernel's callback modes (MODE1 RND32 = 1, TRUNCATE
//! = 0). Each function equals one compute of [`crate::machine::Arithmetic::Prm`] bit for bit, also
//! at underflow. There is no fused multiply-add. [`Precision`] and [`Float40`] add the 40-bit mode
//! (RND32 = 0) for code that brackets a loop with `BIT CLR/SET MODE1 RND32`, as the stock cascades
//! do.
//!
//! Rules (PRM 5.84, 23.02): a subnormal operand reads as zero of the same
//! sign. The exact result is rounded to nearest even at 24 significand bits
//! with an unbounded exponent. Then a magnitude below 2^-126 becomes signed
//! zero and one at 2^128 or above becomes infinity. Host f32 differs: it
//! rounds subnormal results at their own precision (gradual underflow).
//! Operands must be finite: a NaN or an invalid operation raises AIS/MIS,
//! which the kernel's NaN guard treats as a fault.

use crate::alu::{ComputeOp, evaluate_prm};

/// The operand rule: a subnormal reads as zero of the same sign.
pub fn operand(x: f32) -> f32 {
    if x.is_subnormal() {
        f32::from_bits(x.to_bits() & 0x8000_0000)
    } else {
        x
    }
}

/// `Fn = Fx * Fy`. The f64 product of two f32 values is exact.
pub fn mul(x: f32, y: f32) -> f32 {
    round(f64::from(operand(x)) * f64::from(operand(y)))
}

/// `Fn = Fx + Fy`. The f64 sum is exact when the exponents differ by at most
/// 29. Otherwise the smaller operand lies wholly below the 24-bit rounding
/// point by more than four bits, so both roundings give the larger operand.
pub fn add(x: f32, y: f32) -> f32 {
    round(f64::from(operand(x)) + f64::from(operand(y)))
}

/// `Fn = Fx - Fy`, with the same exactness as [`add`].
pub fn sub(x: f32, y: f32) -> f32 {
    round(f64::from(operand(x)) - f64::from(operand(y)))
}

/// Round to 24 bits (nearest even) on the f64 encoding, then flush or
/// overflow on the f32 exponent range. f64 zero signs follow IEEE, as the PRM.
fn round(exact: f64) -> f32 {
    const DROPPED: u32 = 52 - 23;
    let bits = exact.to_bits();
    let half_minus_one = (1 << (DROPPED - 1)) - 1;
    let odd = (bits >> DROPPED) & 1;
    // A carry out of the significand moves into the exponent: still exact.
    let rounded = f64::from_bits((bits + half_minus_one + odd) & !((1 << DROPPED) - 1));
    if rounded.abs() < f64::from(f32::MIN_POSITIVE) {
        f32::from_bits((bits >> 32) as u32 & 0x8000_0000)
    } else {
        // 24 significant bits: exact, or ±infinity from 2^128 up.
        rounded as f32
    }
}

/// One data register with all 40 bits (PRM 4.06): the 32-bit word in bits
/// 39:8, the extension in bits 7:0, as `Machine` keeps `r[n]` and
/// `extension[n]`. A 32-bit load clears the extension ([`From<f32>`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Float40(u64);

impl Float40 {
    /// The word a 32-bit store writes: PRM 4.06 ignores the eight LSBs on
    /// register reads, so the stored value is truncated, not rounded.
    pub const fn store(self) -> f32 {
        f32::from_bits((self.0 >> 8) as u32)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }
}

impl From<f32> for Float40 {
    fn from(word: f32) -> Self {
        Self(u64::from(word.to_bits()) << 8)
    }
}

/// MODE1 RND32: the precision of float computes on [`Float40`] registers
/// (PRM 5.84). Results flush below 2^-126 and overflow from 2^128 in both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    /// RND32 = 1, the callback entry mode: operands drop their eight LSBs and
    /// results round to 24 significand bits, as [`mul`], [`add`] and [`sub`].
    Single,
    /// RND32 = 0: 40-bit extended precision, operands and results keep 32
    /// significand bits.
    Extended,
}

impl Precision {
    /// `Fn = Fx * Fy`.
    pub fn mul(self, x: Float40, y: Float40) -> Float40 {
        self.compute(ComputeOp::FloatMultiply, x, y)
    }

    /// `Fn = Fx + Fy`.
    pub fn add(self, x: Float40, y: Float40) -> Float40 {
        self.compute(ComputeOp::FloatAdd, x, y)
    }

    /// `Fn = Fx - Fy`.
    pub fn sub(self, x: Float40, y: Float40) -> Float40 {
        self.compute(ComputeOp::FloatSubtract, x, y)
    }

    /// The machine's own compute, so the twin cannot drift from it. The test
    /// below checks the extended rounding against an independent integer rule.
    fn compute(self, op: ComputeOp, x: Float40, y: Float40) -> Float40 {
        let rnd32 = self == Self::Single;
        Float40(evaluate_prm(op, x.0, y.0, rnd32).expect("float computes have a result"))
    }
}

#[cfg(test)]
mod tests {
    use super::{Float40, Precision, add, mul, sub};
    use crate::alu::{ComputeOp, evaluate_prm};

    /// The machine's PRM compute on 32-bit register words, RND32 set.
    fn machine(op: ComputeOp, x: f32, y: f32) -> u32 {
        let wide = |v: f32| u64::from(v.to_bits()) << 8;
        let result = evaluate_prm(op, wide(x), wide(y), true).expect("float result");
        assert_eq!(result & 0xff, 0, "RND32 results have no extension");
        (result >> 8) as u32
    }

    /// Bit patterns near the edges that host f32 and the PRM treat differently:
    /// the flush threshold, subnormals, signed zeros, overflow, and ordinary
    /// values from a fixed LCG.
    fn operands() -> Vec<f32> {
        let mut values: Vec<f32> = [0u32, 1, 0x7f_ffff, 0x80_0000, 0x80_0001, 0x7f7f_ffff]
            .into_iter()
            .flat_map(|bits| [bits, bits | 0x8000_0000])
            .map(f32::from_bits)
            .collect();
        values.extend([1.0, 0.5, 1.0 - f32::EPSILON / 2.0, 1.5, 3.0, 1e-20, 2e19]);
        let mut seed = 0x2545_f491_u32;
        for _ in 0..600 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            // Exponents within ±40 of 2^-126, 2^0 and 2^127 hit every edge.
            let exponent = [0u32, 87, 127, 214][(seed >> 30) as usize] + (seed >> 24 & 0x3f);
            let bits = (seed & 0x807f_ffff) | (exponent.min(254) << 23);
            values.push(f32::from_bits(bits));
        }
        values
    }

    #[test]
    fn twins_equal_the_machine_prm_compute_bit_for_bit() {
        let values = operands();
        for &x in &values {
            for &y in &values {
                for (op, twin) in [
                    (ComputeOp::FloatMultiply, mul(x, y)),
                    (ComputeOp::FloatAdd, add(x, y)),
                    (ComputeOp::FloatSubtract, sub(x, y)),
                ] {
                    assert_eq!(
                        twin.to_bits(),
                        machine(op, x, y),
                        "{op:?}({:#010x}, {:#010x})",
                        x.to_bits(),
                        y.to_bits()
                    );
                }
            }
        }
        // The case that separates the rules: 2^-126 · (1 − 2^-24) is exactly
        // halfway in host f32 (rounds up to 2^-126) but a 24-bit PRM value.
        let below = f32::from_bits(0x3f7f_ffff);
        assert_eq!((f32::MIN_POSITIVE * below).to_bits(), 0x0080_0000);
        assert_eq!(mul(f32::MIN_POSITIVE, below).to_bits(), 0);
    }

    /// A finite 40-bit register as `(negative, significand, exponent)`, value
    /// = significand · 2^exponent. A zero or subnormal operand reads as zero.
    fn exact(bits: u64) -> (bool, u128, i32) {
        let negative = (bits >> 39) & 1 == 1;
        let biased = ((bits >> 31) & 0xff) as i32;
        if biased == 0 {
            return (negative, 0, 0);
        }
        (
            negative,
            u128::from(bits & 0x7fff_ffff) | 1 << 31,
            biased - 127 - 31,
        )
    }

    /// Round significand · 2^exponent to 32 significand bits (nearest even),
    /// then flush below 2^-126 or overflow from 2^128: the 40-bit register.
    fn pack(negative: bool, significand: u128, exponent: i32) -> u64 {
        let sign = u64::from(negative) << 39;
        if significand == 0 {
            return sign;
        }
        let drop = 128 - significand.leading_zeros() as i32 - 32;
        let (mut kept, mut exponent) = if drop > 0 {
            let (kept, rest, half) = (
                significand >> drop,
                significand & ((1 << drop) - 1),
                1 << (drop - 1),
            );
            let up = rest > half || (rest == half && kept & 1 == 1);
            (kept + u128::from(up), exponent + drop)
        } else {
            (significand << -drop, exponent + drop)
        };
        if kept >> 32 != 0 {
            (kept, exponent) = (kept >> 1, exponent + 1);
        }
        match exponent + 31 {
            unbiased if unbiased < -126 => sign,
            unbiased if unbiased > 127 => sign | 0xff << 31,
            unbiased => sign | ((unbiased + 127) as u64) << 31 | (kept as u64 & 0x7fff_ffff),
        }
    }

    /// Independent integer rule for RND32 = 0 (PRM 5.84): the exact result,
    /// one rounding at 32 significand bits. Two 32-bit significands and any
    /// sum up to 64 binades apart fit u128. Further apart, the smaller
    /// operand lies below 2^-32 of the larger's last bit, so the result is
    /// the larger. An exact cancellation gives +0.
    fn reference(op: ComputeOp, x: u64, y: u64) -> u64 {
        let ((sx, mx, ex), (sy, my, ey)) = (exact(x), exact(y));
        if op == ComputeOp::FloatMultiply {
            return pack(sx != sy, mx * my, ex + ey);
        }
        let sy = sy ^ (op == ComputeOp::FloatSubtract);
        if mx == 0 || my == 0 {
            let (negative, significand, exponent) =
                if mx == 0 { (sy, my, ey) } else { (sx, mx, ex) };
            return pack(
                negative && (mx != 0 || my != 0 || sx == sy),
                significand,
                exponent,
            );
        }
        let ((sa, ma, ea), (sb, mb, eb)) = if ex >= ey {
            ((sx, mx, ex), (sy, my, ey))
        } else {
            ((sy, my, ey), (sx, mx, ex))
        };
        if ea - eb > 64 {
            return pack(sa, ma, ea);
        }
        let a = ma << (ea - eb);
        match (sa == sb, a.cmp(&mb)) {
            (true, _) => pack(sa, a + mb, eb),
            (false, std::cmp::Ordering::Equal) => 0,
            (false, std::cmp::Ordering::Greater) => pack(sa, a - mb, eb),
            (false, std::cmp::Ordering::Less) => pack(sb, mb - a, eb),
        }
    }

    /// PRM 5.84, RND32 = 0, against the integer [`reference`]: 40-bit
    /// operands (nonzero extensions), every exponent gap and cancelling sums
    /// that leave only extension bits. A 32-bit store then truncates (PRM
    /// 4.06), and `Single` equals the f32 twins.
    #[test]
    fn extended_computes_round_at_32_bits_and_stores_truncate() {
        let values = operands();
        let mut seed = 0x6c07_8965_u32;
        let wide: Vec<Float40> = values
            .iter()
            .map(|&x| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                Float40(Float40::from(x).bits() | u64::from(seed >> 24))
            })
            .collect();
        let check = |x: Float40, y: Float40| {
            let name = format!("({:#012x}, {:#012x})", x.bits(), y.bits());
            for (op, result) in [
                (ComputeOp::FloatMultiply, Precision::Extended.mul(x, y)),
                (ComputeOp::FloatAdd, Precision::Extended.add(x, y)),
                (ComputeOp::FloatSubtract, Precision::Extended.sub(x, y)),
            ] {
                assert_eq!(
                    result.bits(),
                    reference(op, x.bits(), y.bits()),
                    "{op:?}{name}"
                );
            }
            let sum = Precision::Extended.add(x, y);
            assert_eq!(sum.store().to_bits(), (sum.bits() >> 8) as u32);
        };
        for (&x, &wx) in values.iter().zip(&wide) {
            // x − (x with another extension): only the extension bits remain.
            check(wx, Float40(Float40::from(x).bits() ^ 1 << 39));
            for (&y, &wy) in values.iter().zip(&wide) {
                check(wx, wy);
                check(Float40::from(x), Float40::from(y));
                let (wx, wy) = (Float40::from(x), Float40::from(y));
                for (single, twin) in [
                    (Precision::Single.mul(wx, wy), mul(x, y)),
                    (Precision::Single.add(wx, wy), add(x, y)),
                    (Precision::Single.sub(wx, wy), sub(x, y)),
                ] {
                    assert_eq!(
                        single,
                        Float40::from(twin),
                        "single({:#010x}, {:#010x})",
                        x.to_bits(),
                        y.to_bits()
                    );
                }
            }
        }
        // 1 + 2^-30 keeps its last bit at 40 bits; the store truncates it.
        let one = Float40::from(1.0);
        let tiny = Float40::from(2.0_f32.powi(-30));
        let sum = Precision::Extended.add(one, tiny);
        assert_eq!(sum.bits() & 0xff, 0x02);
        assert_eq!(sum.store(), 1.0);
        assert_eq!(Precision::Single.add(one, tiny), one);
    }
}

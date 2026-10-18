//! Conditional finite scalar reconstruction, with no live Qt/helper execution.
use super::{ParameterFields, Variant};

const EPSILON_BITS: u32 = 0x322b_cc77;
const SAFE_QUOTIENT_BITS: u32 = 0x4eff_ffff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandSide {
    Lhs,
    Rhs,
}

/// Owned admission limits, not claims that stock Qt refuses these inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type4Refusal {
    Variant {
        type_id: u32,
    },
    /// No toFloat(Invalid)=0 admission; this is a reached conversion refusal.
    ConstructorInvalid,
    NonFiniteValue,
    ToFloatOverflow,
    NonPositiveStepCount {
        count: i32,
    },
    NonFiniteStep {
        bits: u32,
    },
    NonPositiveStep {
        bits: u32,
    },
    DivisionOverflow {
        quotient_bits: u32,
    },
    QuantizedLong32Overflow {
        quotient_bits: u32,
    },
}

// This separate toFloat admission must not inherit toReal's nonfinite domain.
fn to_float(value: &Variant) -> Result<f32, Type4Refusal> {
    let converted = match value {
        Variant::Int32(value) => i64::from(*value) as f32,
        Variant::UInt32(value) => u64::from(*value) as f32,
        Variant::Float32(value) => value.value(),
        Variant::Double64(value) => {
            let value = value.value();
            if !value.is_finite() {
                return Err(Type4Refusal::NonFiniteValue);
            }
            if value.abs() > f64::from(f32::MAX) {
                return Err(Type4Refusal::ToFloatOverflow);
            }
            value as f32
        }
        Variant::ConstructorInvalid => return Err(Type4Refusal::ConstructorInvalid),
        _ => {
            return Err(Type4Refusal::Variant {
                type_id: value.type_id(),
            });
        }
    };
    if !converted.is_finite() {
        return Err(Type4Refusal::NonFiniteValue);
    }
    Ok(converted)
}

pub(super) fn index(value: &Variant, descriptor: &ParameterFields) -> Result<i32, Type4Refusal> {
    let value = to_float(value)?;
    if descriptor.step_count84 <= 0 {
        return Err(Type4Refusal::NonPositiveStepCount {
            count: descriptor.step_count84,
        });
    }
    let step = descriptor.step144.value();
    if !step.is_finite() {
        return Err(Type4Refusal::NonFiniteStep {
            bits: descriptor.step144.bits(),
        });
    }
    if step <= 0.0 {
        return Err(Type4Refusal::NonPositiveStep {
            bits: descriptor.step144.bits(),
        });
    }
    let epsilon = f32::from_bits(EPSILON_BITS);
    let denominator = if step <= epsilon { epsilon } else { step };
    // Exactly one binary32 division precedes rounding to the ILP32 long result.
    let quotient = value / denominator;
    let bits = quotient.to_bits();
    if !quotient.is_finite() {
        return Err(Type4Refusal::DivisionOverflow {
            quotient_bits: bits,
        });
    }
    if bits & 0x7fff_ffff > SAFE_QUOTIENT_BITS {
        return Err(Type4Refusal::QuantizedLong32Overflow {
            quotient_bits: bits,
        });
    }
    // Recovered libm exponent<=30 branches: integer mantissa arithmetic only.
    // The quotient guard proves each magnitude and its signed result fit i32.
    let exponent = ((bits >> 23) & 255) as i32 - 127;
    let magnitude = if exponent < -1 {
        0
    } else if exponent == -1 {
        1
    } else {
        let mantissa = (bits & 0x007f_ffff) | 0x0080_0000;
        if exponent <= 22 {
            (mantissa + (0x0040_0000 >> exponent)) >> (23 - exponent)
        } else {
            mantissa << (exponent - 23)
        }
    };
    let magnitude = magnitude as i32;
    Ok(if bits >> 31 == 0 {
        magnitude
    } else {
        -magnitude
    })
}

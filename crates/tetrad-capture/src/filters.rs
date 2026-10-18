//! Bounded V1 filter mathematics and recovered FIR/high-pass processing.
//! Optional FIR and high-pass execution decodes immutable owned words through tetrad-sharc.
//! Host sqrt/tan/division and scalar f32 reduction do not reproduce SHARC modes.

use crate::{Error, Result};

pub mod emitted;
mod processing;
pub use emitted::{
    EmittedFirBlock, EmittedHighPassBlock, EmittedV1FirReference, EmittedV1HighPassReference,
    FirLogicalPlacement, HighPassLogicalPlacement,
};
pub use processing::{HistoryUpdate, V1FirReference, V1HighPassReference};

pub(crate) fn finite(value: f32) -> Result<f32> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::NumericOverflow)
    }
}

/// Scalar left-to-right energy reduction, sqrt plus epsilon, one common reciprocal.
/// Only the 512-tap prefilter is normalized; this is not a postfilter operator.
/// Finite input and intermediates are required; underflow follows host f32 behavior.
pub fn normalize_v1_prefilter_reference(coefficients: &[f32; 512]) -> Result<[f32; 512]> {
    if coefficients.iter().any(|value| !value.is_finite()) {
        return Err(Error::NonFinite);
    }
    let mut energy = 0.0;
    for &coefficient in coefficients {
        let product = finite(coefficient * coefficient)?;
        energy = finite(energy + product)?;
    }
    let root = finite(energy.sqrt())?;
    let denominator = finite(root + f32::from_bits(0x3586_37bd))?;
    let scale = finite(1.0 / denominator)?;
    let mut normalized = [0.0; 512];
    for (output, &coefficient) in normalized.iter_mut().zip(coefficients) {
        *output = finite(coefficient * scale)?;
    }
    Ok(normalized)
}

/// Recovered temporary slots 0..3 are feedforward; slots 3..5 are feedback.
/// Processing uses two accumulated contributions and subtracts feedback products.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct V1HighPassCoefficients {
    pub feedforward: [f32; 3],
    pub feedback: [f32; 2],
}

/// Fixed 48000 Hz setup; bandwidth is a dimensionless multiplier, not Q.
/// Finite frequency below 1 Hz selects identity, irrespective of finite bandwidth.
/// The owned active domain is 1 <= frequency < Nyquist and bandwidth > 0.
/// Every intermediate must be finite; reciprocal underflow to zero is rejected.
pub fn v1_high_pass_coefficients_reference(
    frequency_hz: f32,
    bandwidth: f32,
) -> Result<V1HighPassCoefficients> {
    if !frequency_hz.is_finite() || !bandwidth.is_finite() {
        return Err(Error::NonFinite);
    }
    if frequency_hz < 1.0 {
        return Ok(V1HighPassCoefficients {
            feedforward: [1.0, 0.0, 0.0],
            feedback: [0.0, 0.0],
        });
    }
    if frequency_hz >= 24_000.0 || bandwidth <= 0.0 {
        return Err(Error::HighPassDomain);
    }
    let reciprocal_bandwidth = finite(1.0 / bandwidth)?;
    if reciprocal_bandwidth == 0.0 {
        return Err(Error::NumericOverflow);
    }
    let angle_product = finite(frequency_hz * f32::from_bits(0x4049_0fdb))?;
    let angle = finite(angle_product / 48_000.0)?;
    let g = finite(angle.tan())?;
    let h = finite(1.0 / reciprocal_bandwidth)?;
    let v = finite(g * h)?;
    let g2 = finite(g * g)?;
    // Both ALU results use the old v, not the newly computed plus.
    let plus = finite(1.0 + v)?;
    let minus = finite(1.0 - v)?;
    let denominator = finite(g2 + plus)?;
    if denominator <= 0.0 {
        return Err(Error::HighPassDomain);
    }
    let d = finite(1.0 / denominator)?;
    if d == 0.0 {
        return Err(Error::NumericOverflow);
    }
    let twice_d = finite(d + d)?;
    let first_feedback = finite(finite(g2 - 1.0)? * twice_d)?;
    let second_feedback = finite(finite(g2 + minus)? * d)?;
    Ok(V1HighPassCoefficients {
        feedforward: [d, finite(-2.0 * d)?, d],
        feedback: [first_feedback, second_feedback],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_uses_sqrt_energy_epsilon_and_common_scale() {
        let mut coefficients = [0.0; 512];
        coefficients[..2].copy_from_slice(&[3.0, 4.0]);
        let normalized = normalize_v1_prefilter_reference(&coefficients).unwrap();
        // Separate f32 rounds: sqrt(25)+epsilon, reciprocal, then multiplication.
        assert_eq!(normalized[0].to_bits(), 0x3f19_9998);
        assert_eq!(normalized[1].to_bits(), 0x3f4c_ccca);
        assert!(normalized[2..].iter().all(|&value| value == 0.0));
        assert_eq!(
            normalize_v1_prefilter_reference(&[0.0; 512]).unwrap(),
            [0.0; 512]
        );
        coefficients[0] = -0.0;
        coefficients[1] = 0.0;
        assert_eq!(
            normalize_v1_prefilter_reference(&coefficients).unwrap()[0].to_bits(),
            (-0.0_f32).to_bits()
        );
    }

    #[test]
    fn normalization_rejects_nonfinite_product_and_sum_overflow() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut coefficients = [0.0; 512];
            coefficients[511] = value;
            assert_eq!(
                normalize_v1_prefilter_reference(&coefficients),
                Err(Error::NonFinite)
            );
        }
        let mut coefficients = [0.0; 512];
        coefficients[0] = f32::MAX;
        assert_eq!(
            normalize_v1_prefilter_reference(&coefficients),
            Err(Error::NumericOverflow)
        );
        // Each product is finite; the reduction overflows.
        assert_eq!(
            normalize_v1_prefilter_reference(&[1e19; 512]),
            Err(Error::NumericOverflow)
        );
    }

    #[test]
    fn high_pass_threshold_slots_and_bandwidth_multiplier_are_recovered() {
        for (frequency, bandwidth) in [(-10.0, -1.0), (0.0, 0.0), (0.999, 2.0)] {
            assert_eq!(
                v1_high_pass_coefficients_reference(frequency, bandwidth).unwrap(),
                V1HighPassCoefficients {
                    feedforward: [1.0, 0.0, 0.0],
                    feedback: [0.0; 2],
                }
            );
        }
        assert_ne!(
            v1_high_pass_coefficients_reference(1.0, 2.0)
                .unwrap()
                .feedforward[0],
            1.0
        );
        for (bandwidth, expected_d, expected_feedback) in [
            (1.0, 1.0 / 3.0, 1.0 / 3.0),
            (2.0, 0.25, 0.0),
            (4.0, 1.0 / 6.0, -1.0 / 3.0),
        ] {
            let coefficients = v1_high_pass_coefficients_reference(12_000.0, bandwidth).unwrap();
            for (actual, expected) in coefficients
                .feedforward
                .into_iter()
                .chain(coefficients.feedback)
                .zip([
                    expected_d,
                    -2.0 * expected_d,
                    expected_d,
                    0.0,
                    expected_feedback,
                ])
            {
                assert!((actual - expected).abs() < 2e-7);
            }
        }
    }

    #[test]
    fn high_pass_owned_active_domain_and_intermediates_are_checked() {
        for (frequency, bandwidth) in [(1.0, 0.0), (1.0, -1.0), (24_000.0, 2.0), (48_000.0, 2.0)] {
            assert_eq!(
                v1_high_pass_coefficients_reference(frequency, bandwidth),
                Err(Error::HighPassDomain)
            );
        }
        for (frequency, bandwidth) in [(f32::NAN, 1.0), (0.0, f32::INFINITY)] {
            assert_eq!(
                v1_high_pass_coefficients_reference(frequency, bandwidth),
                Err(Error::NonFinite)
            );
        }
        assert_eq!(
            v1_high_pass_coefficients_reference(1.0, f32::from_bits(1)),
            Err(Error::NumericOverflow)
        );
        assert_eq!(
            v1_high_pass_coefficients_reference(18_000.0, f32::MAX),
            Err(Error::NumericOverflow)
        );
    }
}

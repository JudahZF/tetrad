use crate::{Error, Result};

pub const ADAA_EPSILON: f32 = f32::from_bits(0x38d1_b717);
pub const GAIN_SMOOTHING: f32 = f32::from_bits(0x3f73_3333);

pub(crate) fn clip(value: f32) -> f32 {
    let clipped = value.clamp(-2.0, 2.0);
    clipped * (1.0 - clipped.abs() / 4.0)
}

fn antiderivative(value: f32) -> f32 {
    let magnitude = value.abs();
    let bounded = magnitude.min(2.0);
    let coefficient = f32::from_bits(0xbdaa_aaab);
    magnitude + bounded * ((coefficient * bounded + 0.5) * bounded - 1.0)
}

/// One recovered ADAA clip neuron, storing raw preactivation history. A complete
/// network reset must seed each neuron using its own zero-input affine value.
#[derive(Debug, Clone)]
pub struct AdaaClip {
    previous: f32,
}

impl AdaaClip {
    pub fn new(zero_input_preactivation: f32) -> Result<Self> {
        if !zero_input_preactivation.is_finite() {
            return Err(Error::NonFinite);
        }
        Ok(Self {
            previous: zero_input_preactivation,
        })
    }

    pub fn reset(&mut self, zero_input_preactivation: f32) -> Result<()> {
        if !zero_input_preactivation.is_finite() {
            return Err(Error::NonFinite);
        }
        self.previous = zero_input_preactivation;
        Ok(())
    }

    /// The quotient uses Rust division as a reference. Stock RECIPS plus seven
    /// Newton refinements and SHARC exceptional-number behavior are unverified.
    pub fn process(&mut self, preactivation: f32) -> Result<f32> {
        if !preactivation.is_finite() {
            return Err(Error::NonFinite);
        }
        let difference = preactivation - self.previous;
        let output = if difference.abs() <= ADAA_EPSILON {
            let midpoint = (preactivation + self.previous) / 2.0;
            if !midpoint.is_finite() {
                return Err(Error::NumericOverflow);
            }
            clip(midpoint)
        } else {
            (antiderivative(preactivation) - antiderivative(self.previous)) / difference
        };
        if !difference.is_finite() || !output.is_finite() {
            return Err(Error::NumericOverflow);
        }
        self.previous = preactivation;
        Ok(output)
    }
}

/// Output-major V1 middle 2×2 affine with recovered bias-first association.
/// Host f32 remains a mathematical reference, not a SHARC numerical model.
pub fn affine_2(weights: [[f32; 2]; 2], bias: [f32; 2], input: [f32; 2]) -> Result<[f32; 2]> {
    if weights
        .iter()
        .flatten()
        .chain(bias.iter())
        .chain(input.iter())
        .any(|v| !v.is_finite())
    {
        return Err(Error::NonFinite);
    }
    let mut output = [0.0; 2];
    for (row, value) in output.iter_mut().enumerate() {
        let first = weights[row][0] * input[0];
        let second = weights[row][1] * input[1];
        *value = (first + bias[row]) + second;
        if !value.is_finite() {
            return Err(Error::NumericOverflow);
        }
    }
    Ok(output)
}

/// External gain law; finite inputs in the ordinary normalized 0..=1 range.
/// Host powf is a mathematical substitute for stock gain conversion.
pub fn gain_target(normalized: f32) -> Result<f32> {
    if !normalized.is_finite() {
        return Err(Error::NonFinite);
    }
    if !(0.0..=1.0).contains(&normalized) {
        return Err(Error::NormalizedRange);
    }
    // Updater case 0 subtracts at 0x1dc, then multiplies at 0x1dd by bits
    // 0x3d4ccccd (loaded at 0x1a3); it does not divide by 20.
    let decibels = 48.0 * normalized - 24.0;
    Ok(10.0_f32.powf(decibels * f32::from_bits(0x3d4c_cccd)))
}

/// Independent current/target state for the recovered per-sample gain step.
/// Construction defaults both slots to zero. This does not model parameter-service
/// ramps or assume that a stock neural reset resets these gain slots.
#[derive(Debug, Clone, Default)]
pub struct GainSmoother {
    current: f32,
    target: f32,
}

fn validate_linear_gain(value: f32) -> Result<()> {
    if !value.is_finite() {
        Err(Error::NonFinite)
    } else if value < 0.0 {
        Err(Error::LinearGainRange)
    } else {
        Ok(())
    }
}

impl GainSmoother {
    /// Explicit caller-owned initial state; both gains must be finite/nonnegative.
    pub fn from_state(current: f32, target: f32) -> Result<Self> {
        validate_linear_gain(current)?;
        validate_linear_gain(target)?;
        Ok(Self { current, target })
    }

    pub fn current(&self) -> f32 {
        self.current
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn set_normalized_target(&mut self, normalized: f32) -> Result<()> {
        let target = gain_target(normalized)?;
        self.set_linear_target(target)
    }

    pub fn set_linear_target(&mut self, target: f32) -> Result<()> {
        validate_linear_gain(target)?;
        self.target = target;
        Ok(())
    }

    /// Separate f32 subtraction, products and sum, then apply the NEW gain.
    /// Commit current only after finite output; underflow follows host f32.
    pub fn process(&mut self, sample: f32) -> Result<f32> {
        if !sample.is_finite() {
            return Err(Error::NonFinite);
        }
        let remaining = 1.0 - GAIN_SMOOTHING;
        let old_product = GAIN_SMOOTHING * self.current;
        let target_product = remaining * self.target;
        let next = old_product + target_product;
        let output = next * sample;
        if [remaining, old_product, target_product, next, output]
            .iter()
            .any(|value| !value.is_finite())
        {
            return Err(Error::NumericOverflow);
        }
        self.current = next;
        Ok(output)
    }

    /// Two deterministic passes preserve state and caller output on any failure.
    /// No allocations; work is linear in the supplied chunk, not a realtime claim.
    pub fn process_chunk(&mut self, inputs: &[f32], outputs: &mut [f32]) -> Result<()> {
        if inputs.len() != outputs.len() {
            return Err(Error::Shape);
        }
        let mut next = self.clone();
        for &input in inputs {
            next.process(input)?;
        }
        let mut writer = self.clone();
        for (&input, output) in inputs.iter().zip(outputs) {
            *output = writer
                .process(input)
                .expect("identical validated second pass");
        }
        *self = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothing_separate_rounds_new_gain_and_subtracted_remaining() {
        let mut smoother = GainSmoother::from_state(0.25, 2.0).unwrap();
        assert_eq!(smoother.process(3.0).unwrap().to_bits(), 0x3f81_999a);
        assert_eq!(smoother.current().to_bits(), 0x3eac_ccce);
        assert_eq!(smoother.target(), 2.0);
        assert_ne!((1.0 - GAIN_SMOOTHING).to_bits(), 0.05_f32.to_bits());
        let mut zero = GainSmoother::default();
        assert_eq!((zero.current(), zero.target()), (0.0, 0.0));
        zero.set_normalized_target(0.5).unwrap();
        assert_eq!(zero.target(), 1.0);
        assert_eq!(zero.process(2.0).unwrap().to_bits(), 0x3dcc_ccd0);
    }

    #[test]
    fn smoothing_invalid_targets_samples_and_overflow_preserve_state() {
        for value in [f32::NAN, f32::INFINITY, -1.0] {
            assert!(GainSmoother::from_state(value, 1.0).is_err());
            assert!(GainSmoother::from_state(1.0, value).is_err());
        }
        let mut smoother = GainSmoother::from_state(0.25, 2.0).unwrap();
        for (value, error) in [(f32::NAN, Error::NonFinite), (-1.0, Error::LinearGainRange)] {
            assert_eq!(smoother.set_linear_target(value), Err(error));
            assert_eq!((smoother.current(), smoother.target()), (0.25, 2.0));
        }
        assert_eq!(
            smoother.set_normalized_target(1.1),
            Err(Error::NormalizedRange)
        );
        assert_eq!(
            smoother.set_normalized_target(f32::INFINITY),
            Err(Error::NonFinite)
        );
        assert_eq!(smoother.process(f32::NAN), Err(Error::NonFinite));
        assert_eq!((smoother.current(), smoother.target()), (0.25, 2.0));
        let mut overflow = GainSmoother::from_state(2.0, 4.0).unwrap();
        assert_eq!(overflow.process(f32::MAX), Err(Error::NumericOverflow));
        assert_eq!((overflow.current(), overflow.target()), (2.0, 4.0));
    }

    #[test]
    fn smoothing_chunks_are_atomic_boundary_independent_and_instance_local() {
        let inputs = [3.0, -1.0, 0.0, 2.0, 0.5];
        let mut contiguous = GainSmoother::from_state(0.25, 2.0).unwrap();
        let mut expected = [0.0; 5];
        contiguous.process_chunk(&inputs, &mut expected).unwrap();
        let mut chunked = GainSmoother::from_state(0.25, 2.0).unwrap();
        let mut actual = [0.0; 5];
        for (input, output) in inputs.chunks(2).zip(actual.chunks_mut(2)) {
            chunked.process_chunk(input, output).unwrap();
        }
        assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
        assert_eq!(chunked.current().to_bits(), contiguous.current().to_bits());
        let mut independent = GainSmoother::from_state(0.25, 2.0).unwrap();
        assert_eq!(
            independent.process(3.0).unwrap().to_bits(),
            expected[0].to_bits()
        );
        for (bad, error) in [
            ([0.1, f32::INFINITY], Error::NonFinite),
            ([0.1, f32::MAX], Error::NumericOverflow),
        ] {
            let mut smoother = GainSmoother::from_state(2.0, 4.0).unwrap();
            let mut output = [123.0; 2];
            assert_eq!(smoother.process_chunk(&bad, &mut output), Err(error));
            assert_eq!(output, [123.0; 2]);
            assert_eq!((smoother.current(), smoother.target()), (2.0, 4.0));
            assert_eq!(smoother.process_chunk(&[], &mut output), Err(Error::Shape));
            smoother.process_chunk(&[], &mut []).unwrap();
            assert_eq!(smoother.current(), 2.0);
        }
    }

    #[test]
    fn scalar_adaa_vectors_and_threshold_include_equality() {
        for (previous, value, expected) in [
            (0.0, 1.0, 5.0 / 12.0),
            (-1.0, 1.0, 0.0),
            (1.0, 1.0, 0.75),
            (2.0, 3.0, 1.0),
            (0.0, 3.0, 7.0 / 9.0),
        ] {
            let mut neuron = AdaaClip::new(previous).unwrap();
            assert!((neuron.process(value).unwrap() - expected).abs() < 2e-7);
        }
        let mut neuron = AdaaClip::new(0.0).unwrap();
        assert_eq!(
            neuron.process(ADAA_EPSILON).unwrap().to_bits(),
            clip(ADAA_EPSILON / 2.0).to_bits()
        );
        assert_eq!(clip(-3.0), -1.0);
        assert_eq!(clip(3.0), 1.0);
    }

    #[test]
    fn history_reset_chunk_independence_and_instances_are_isolated() {
        let samples = [0.0, 1.0, 1.0, -1.0, 3.0, 2.0];
        let mut contiguous = AdaaClip::new(0.25).unwrap();
        let expected: Vec<_> = samples
            .into_iter()
            .map(|v| contiguous.process(v).unwrap().to_bits())
            .collect();
        let mut chunked = AdaaClip::new(0.25).unwrap();
        let actual: Vec<_> = samples
            .chunks(2)
            .flat_map(|chunk| {
                chunk
                    .iter()
                    .map(|&v| chunked.process(v).unwrap().to_bits())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(actual, expected);
        contiguous.reset(0.25).unwrap();
        assert_eq!(contiguous.process(0.0).unwrap().to_bits(), expected[0]);
        let mut other = AdaaClip::new(0.25).unwrap();
        let mut before = other.clone();
        assert_eq!(other.process(f32::NAN), Err(Error::NonFinite));
        assert_eq!(
            other.process(0.0).unwrap().to_bits(),
            before.process(0.0).unwrap().to_bits()
        );
    }

    #[test]
    fn output_major_matrix_and_gain_law_are_asymmetric_reference_checks() {
        assert_eq!(
            affine_2([[1.0, 2.0], [3.0, 4.0]], [11.0, 13.0], [5.0, 7.0]).unwrap(),
            [30.0, 56.0]
        );
        // Adding this bias after the two cancelling products would retain it.
        let small_bias = f32::from_bits(0x3080_0000); // 2^-30
        assert_eq!(
            affine_2([[1.0, -1.0], [-1.0, 1.0]], [small_bias; 2], [1.0; 2])
                .unwrap()
                .map(f32::to_bits),
            [0; 2]
        );
        assert_eq!(gain_target(0.5).unwrap(), 1.0);
        assert!((gain_target(0.0).unwrap() - 0.06309573).abs() < 1e-7);
        assert!((gain_target(1.0).unwrap() - 15.848932).abs() < 2e-6);
        assert!(gain_target(-0.1).is_err());
        assert_eq!(gain_target(f32::INFINITY), Err(Error::NonFinite));
        assert_eq!(GAIN_SMOOTHING.to_bits(), 0x3f73_3333);
    }
}

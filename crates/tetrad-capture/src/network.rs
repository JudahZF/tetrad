//! Host mathematical REFERENCE of the fixed V1 neural core, not the audio path.
//! Caller input must already have gain/high-pass/FIR/resampling processing applied.

pub mod emitted;
pub mod emitted_stack;

use crate::{
    Error, Result,
    reference::{AdaaClip, affine_2, clip},
};

/// Output-major 1→2→2→1 tensors. Beta is explicit; no gain/weight-derived
/// baseline factor is inferred from serialized data.
#[derive(Debug, Clone)]
pub struct NetworkParameters {
    first_weights: [f32; 2],
    first_biases: [f32; 2],
    middle_weights: [[f32; 2]; 2],
    middle_biases: [f32; 2],
    last_weights: [f32; 2],
    last_bias: f32,
    output_coefficient: f32,
}

impl NetworkParameters {
    pub fn new(
        first_weights: [f32; 2],
        first_biases: [f32; 2],
        middle_weights: [[f32; 2]; 2],
        middle_biases: [f32; 2],
        last_weights: [f32; 2],
        last_bias: f32,
        output_coefficient: f32,
    ) -> Result<Self> {
        if first_weights
            .iter()
            .chain(first_biases.iter())
            .chain(middle_weights.iter().flatten())
            .chain(middle_biases.iter())
            .chain(last_weights.iter())
            .chain([last_bias, output_coefficient].iter())
            .any(|value| !value.is_finite())
        {
            return Err(Error::NonFinite);
        }
        Ok(Self {
            first_weights,
            first_biases,
            middle_weights,
            middle_biases,
            last_weights,
            last_bias,
            output_coefficient,
        })
    }
}

/// Each instance owns five raw-affine histories. All three layers use ADAA.
/// Separate f32 multiply/add and Rust division do not emulate SHARC RECIPS.
#[derive(Debug, Clone)]
pub struct NeuralCore {
    parameters: NetworkParameters,
    first: [AdaaClip; 2],
    middle: [AdaaClip; 2],
    last: AdaaClip,
    scaled_baseline: f32,
}

fn finite_result(value: f32) -> Result<f32> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::NumericOverflow)
    }
}

fn last_affine(parameters: &NetworkParameters, input: [f32; 2]) -> Result<f32> {
    let first = parameters.last_weights[0] * input[0];
    let second = parameters.last_weights[1] * input[1];
    // Bias precedes the second product in both the recovered seed and sample paths.
    finite_result((first + parameters.last_bias) + second)
}

impl NeuralCore {
    /// Seeds first history from biases, then each later history from the affine
    /// transform of the preceding zero-input clipped values (not preceding ADAA).
    pub fn new(parameters: NetworkParameters) -> Result<Self> {
        let zero_middle = affine_2(
            parameters.middle_weights,
            parameters.middle_biases,
            parameters.first_biases.map(clip),
        )?;
        let zero_last = last_affine(&parameters, zero_middle.map(clip))?;
        let scaled_baseline = finite_result(parameters.output_coefficient * clip(zero_last))?;
        Ok(Self {
            first: [
                AdaaClip::new(parameters.first_biases[0])?,
                AdaaClip::new(parameters.first_biases[1])?,
            ],
            middle: [
                AdaaClip::new(zero_middle[0])?,
                AdaaClip::new(zero_middle[1])?,
            ],
            last: AdaaClip::new(zero_last)?,
            parameters,
            scaled_baseline,
        })
    }

    /// Recompute all zero-input seeds and baseline; commit only on success.
    pub fn reset(&mut self) -> Result<()> {
        self.reload(self.parameters.clone())
    }

    /// Atomic parameter reload and reset. Failed seed arithmetic retains the old core.
    pub fn reload(&mut self, parameters: NetworkParameters) -> Result<()> {
        let replacement = Self::new(parameters)?;
        *self = replacement;
        Ok(())
    }

    /// Process one already-prepared neural input. Any failure retains all history.
    pub fn process(&mut self, input: f32) -> Result<f32> {
        let mut next = self.clone();
        let output = next.process_inner(input)?;
        *self = next;
        Ok(output)
    }

    /// Atomic for the entire chunk, including caller output. A reference-only
    /// dry run checks all samples before a second pass writes output, avoiding
    /// allocation. Successful results do not depend on chunk boundaries.
    pub fn process_chunk(&mut self, inputs: &[f32], outputs: &mut [f32]) -> Result<()> {
        if inputs.len() != outputs.len() {
            return Err(Error::Shape);
        }
        let mut next = self.clone();
        for &input in inputs {
            next.process_inner(input)?;
        }
        let mut writer = self.clone();
        for (&input, output) in inputs.iter().zip(outputs) {
            *output = writer
                .process_inner(input)
                .expect("identical validated second pass");
        }
        *self = next;
        Ok(())
    }

    fn process_inner(&mut self, input: f32) -> Result<f32> {
        if !input.is_finite() {
            return Err(Error::NonFinite);
        }
        let mut first_output = [0.0; 2];
        for (index, output) in first_output.iter_mut().enumerate() {
            let product = self.parameters.first_weights[index] * input;
            let raw = finite_result(product + self.parameters.first_biases[index])?;
            *output = self.first[index].process(raw)?;
        }
        let raw_middle = affine_2(
            self.parameters.middle_weights,
            self.parameters.middle_biases,
            first_output,
        )?;
        let middle_output = [
            self.middle[0].process(raw_middle[0])?,
            self.middle[1].process(raw_middle[1])?,
        ];
        let raw_last = last_affine(&self.parameters, middle_output)?;
        let activated = self.last.process(raw_last)?;
        // Algebraically beta * (ADAA - clip(zero_last)), evaluated as recovered
        // separate products/subtraction. Do not fuse or factor these operations.
        let scaled = finite_result(self.parameters.output_coefficient * activated)?;
        finite_result(scaled - self.scaled_baseline)
    }
}

/// Opt-in SERIALIZED MATHEMATICAL GAINS policy for the fixed V1 neural core.
/// Alpha is the serialized pre-gain and beta is the serialized post-gain.
/// Stock reconstructs both through log/exp controls; bit equality is NOT proved.
/// Input must already be post-pre-FIR/resampled. This is not an audio processor.
#[derive(Debug, Clone)]
pub struct SerializedMathematicalGainsCore {
    alpha: f32,
    core: NeuralCore,
}

impl SerializedMathematicalGainsCore {
    /// Validate all external runtime values before constructing neural state.
    /// The owned ordinary gain domain is pre > 0.5 and post > 0.1; exceptional
    /// log arguments in stock are not inferred from this mathematical adapter.
    pub fn new(record: &crate::v1::RuntimeData<'_>) -> Result<Self> {
        if !record.layer_sizes().eq([1, 2, 2, 1]) {
            return Err(Error::UnsupportedV1Shape);
        }
        record.validate_runtime_values()?;
        let [alpha, beta] = record.constant_gains();
        if alpha <= 0.5 || beta <= f32::from_bits(0x3dcc_cccd) {
            return Err(Error::ConstantGainRange);
        }
        let [frequency, bandwidth] = record.high_pass();
        crate::filters::v1_high_pass_coefficients_reference(frequency, bandwidth)?;
        let core = NeuralCore::new(record.network_parameters(beta)?)?;
        Ok(Self { alpha, core })
    }

    /// Restore neural zero-input history/baseline while retaining the selected gains.
    pub fn reset(&mut self) -> Result<()> {
        self.core.reset()
    }

    /// Atomically validate, select serialized mathematical gains and reset.
    pub fn reload(&mut self, record: &crate::v1::RuntimeData<'_>) -> Result<()> {
        let replacement = Self::new(record)?;
        *self = replacement;
        Ok(())
    }

    /// Apply alpha to input before the first weight product and bias addition.
    /// Scaling errors and core errors retain all neural history.
    pub fn process(&mut self, input: f32) -> Result<f32> {
        if !input.is_finite() {
            return Err(Error::NonFinite);
        }
        let scaled_input = finite_result(self.alpha * input)?;
        self.core.process(scaled_input)
    }

    /// Atomic two-pass reference, including caller output; no allocation or
    /// realtime claim. Successful results are independent of chunk boundaries.
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

    fn parameters() -> NetworkParameters {
        NetworkParameters::new(
            [0.5, -0.25],
            [0.25, -0.5],
            [[0.75, -0.5], [0.25, 0.625]],
            [0.125, -0.0625],
            [0.5, -0.375],
            0.125,
            1.75,
        )
        .unwrap()
    }

    #[test]
    fn asymmetric_three_layer_adaa_and_zero_input_reset_reference_vectors() {
        let mut association = parameters();
        association.last_weights = [1.0, -1.0];
        association.last_bias = f32::from_bits(0x3080_0000); // 2^-30
        assert_eq!(last_affine(&association, [1.0; 2]).unwrap().to_bits(), 0);

        let mut core = NeuralCore::new(parameters()).unwrap();
        // Independent, separate-operation f32 oracle; synthetic math evidence.
        // Reset raw seeds: [.25,-.5], [.51953125,-.27734375], .44781923294067383.
        let samples = [0.0, 0.25, 0.25, 0.25001, -0.5, 1.0, -1.0];
        let expected = [
            0,
            0x3bd5_7000,
            0x3cd1_1240,
            0x3d34_1020,
            0x3cfb_e6c0,
            0x3c29_1bc0,
            0x3c6b_40c0,
        ];
        assert_eq!(core.scaled_baseline.to_bits(), 0x3f32_298d);
        for (sample, bits) in samples.into_iter().zip(expected) {
            assert_eq!(core.process(sample).unwrap().to_bits(), bits);
        }
        core.reset().unwrap();
        for _ in 0..8 {
            assert_eq!(core.process(0.0).unwrap(), 0.0);
        }
        core.reset().unwrap();
        core.process(0.0).unwrap();
        let output = core.process(0.25).unwrap();
        // An ordinary clip at the last layer gives ~.01301414, not this result.
        assert!((output - 0.0065135956).abs() < 1e-8);

        let constant = NetworkParameters::new(
            [0.0; 2],
            [0.25, -0.5],
            [[0.75, -0.5], [0.25, 0.625]],
            [0.125, -0.0625],
            [0.5, -0.375],
            0.125,
            1.75,
        )
        .unwrap();
        let mut constant = NeuralCore::new(constant).unwrap();
        for sample in [0.0, 1.0, -2.0, 1e-6] {
            assert_eq!(constant.process(sample).unwrap(), 0.0);
        }
    }

    #[test]
    fn chunks_reset_reload_and_instances_have_independent_history() {
        let samples = [0.0, 0.25, 0.25, 0.25001, -0.5, 1.0, -1.0];
        let mut contiguous = NeuralCore::new(parameters()).unwrap();
        let mut expected = [0.0; 7];
        contiguous.process_chunk(&samples, &mut expected).unwrap();
        let mut chunked = NeuralCore::new(parameters()).unwrap();
        let mut actual = [0.0; 7];
        for (input, output) in samples.chunks(2).zip(actual.chunks_mut(2)) {
            chunked.process_chunk(input, output).unwrap();
        }
        assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
        let mut other = NeuralCore::new(parameters()).unwrap();
        assert_eq!(
            other.process(samples[0]).unwrap().to_bits(),
            expected[0].to_bits()
        );
        assert_eq!(
            other.process(samples[1]).unwrap().to_bits(),
            expected[1].to_bits()
        );
        contiguous.reset().unwrap();
        contiguous.process_chunk(&samples, &mut actual).unwrap();
        assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
        contiguous.reload(parameters()).unwrap();
        assert_eq!(contiguous.process(0.0).unwrap(), 0.0);
    }

    #[test]
    fn failed_sample_chunk_and_seed_reload_are_atomic() {
        let mut core = NeuralCore::new(parameters()).unwrap();
        core.process(0.25).unwrap();
        let mut before = core.clone();
        assert_eq!(core.process(f32::NAN), Err(Error::NonFinite));
        let mut output = [123.0; 3];
        assert_eq!(
            core.process_chunk(&[0.1, -0.2, f32::INFINITY], &mut output),
            Err(Error::NonFinite)
        );
        assert_eq!(output, [123.0; 3]);
        assert_eq!(core.process_chunk(&[0.1], &mut output), Err(Error::Shape));
        assert_eq!(output, [123.0; 3]);
        let bad_reset = NetworkParameters::new(
            [0.0; 2],
            [2.0; 2],
            [[f32::MAX; 2]; 2],
            [0.0; 2],
            [1.0; 2],
            0.0,
            1.0,
        )
        .unwrap();
        assert_eq!(core.reload(bad_reset), Err(Error::NumericOverflow));
        for sample in [-0.1, 0.5, 0.0] {
            assert_eq!(
                core.process(sample).unwrap().to_bits(),
                before.process(sample).unwrap().to_bits()
            );
        }

        let early_overflow = NetworkParameters::new(
            [0.5, 2.0],
            [0.0; 2],
            [[1.0, 0.0], [0.0, 1.0]],
            [0.0; 2],
            [0.5; 2],
            0.0,
            1.0,
        )
        .unwrap();
        let late_overflow = NetworkParameters::new(
            [1.0, 0.0],
            [0.0; 2],
            [[1.0, 0.0], [0.0; 2]],
            [0.0; 2],
            [10.0, 0.0],
            -2.0,
            f32::MAX,
        )
        .unwrap();
        for (parameters, failing_input) in [(early_overflow, f32::MAX), (late_overflow, 100.0)] {
            let mut core = NeuralCore::new(parameters).unwrap();
            let mut before = core.clone();
            assert_eq!(core.process(failing_input), Err(Error::NumericOverflow));
            let mut output = [9.0; 2];
            assert_eq!(
                core.process_chunk(&[0.0, failing_input], &mut output),
                Err(Error::NumericOverflow)
            );
            assert_eq!(output, [9.0; 2]);
            assert_eq!(
                core.process(0.1).unwrap().to_bits(),
                before.process(0.1).unwrap().to_bits()
            );
        }
        assert!(matches!(
            NetworkParameters::new(
                [f32::NAN; 2],
                [0.0; 2],
                [[0.0; 2]; 2],
                [0.0; 2],
                [0.0; 2],
                0.0,
                1.0
            ),
            Err(Error::NonFinite)
        ));
    }
}

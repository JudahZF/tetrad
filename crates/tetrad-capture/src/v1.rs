//! Borrowed, decoded V1 runtime data emitted by ARM converter VA 0x6ec290.
//! Parsing establishes structure only; no encrypted/protobuf import is provided.

use crate::{Error, Result, network::NetworkParameters};

const SIZES_OFFSET: usize = 0x2818;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Applies to the entire supplied slice, including any prefix remainder.
    pub max_input_bytes: usize,
    pub max_layers: usize,
    pub max_dimension: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // Converter output bound, not a universal encrypted capture limit.
            max_input_bytes: 0x2fff,
            max_layers: 64,
            max_dimension: 16_384,
        }
    }
}

/// Unaligned LE float words, without allocation or reinterpretation through unsafe.
#[derive(Debug, Clone, Copy)]
pub struct Floats<'a>(&'a [u8]);

impl<'a> Floats<'a> {
    pub fn as_bytes(self) -> &'a [u8] {
        self.0
    }

    pub fn len(self) -> usize {
        self.0.len() / 4
    }

    pub fn is_empty(self) -> bool {
        self.0.is_empty()
    }

    pub fn get(self, index: usize) -> Option<f32> {
        let start = index.checked_mul(4)?;
        let end = start.checked_add(4)?;
        let word = self.0.get(start..end)?;
        Some(f32::from_le_bytes(word.try_into().expect("four-byte word")))
    }

    pub fn iter(self) -> impl ExactSizeIterator<Item = f32> + 'a {
        self.0
            .chunks_exact(4)
            .map(|word| f32::from_le_bytes(word.try_into().expect("four-byte word")))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Layer<'a> {
    pub inputs: usize,
    pub outputs: usize,
    pub weights: Floats<'a>,
    pub biases: Floats<'a>,
}

#[derive(Debug)]
pub struct RuntimeData<'a> {
    bytes: &'a [u8],
    sizes: &'a [u8],
    tensors: &'a [u8],
    tail: usize,
}

fn word(bytes: &[u8], offset: usize) -> Result<u32> {
    let end = offset.checked_add(4).ok_or(Error::Overflow)?;
    let bytes = bytes.get(offset..end).ok_or(Error::Truncated)?;
    Ok(u32::from_le_bytes(
        bytes.try_into().expect("four-byte word"),
    ))
}

fn dimension(bytes: &[u8], offset: usize) -> Result<usize> {
    usize::try_from(word(bytes, offset)?).map_err(|_| Error::Overflow)
}

fn tensor_bytes(inputs: usize, outputs: usize) -> Result<usize> {
    inputs
        .checked_mul(outputs)
        .and_then(|count| count.checked_mul(4))
        .ok_or(Error::Overflow)
}

impl<'a> RuntimeData<'a> {
    /// Requires exact consumption. Float finiteness and sample-rate compatibility
    /// are separate validation; the fixed filter lengths establish field offsets.
    pub fn parse(bytes: &'a [u8], limits: Limits) -> Result<Self> {
        let (record, consumed) = Self::parse_prefix(bytes, limits)?;
        if consumed != bytes.len() {
            return Err(Error::TrailingBytes);
        }
        Ok(record)
    }

    /// Explicit prefix parsing; trailing bytes are neither parsed nor validated.
    /// All tensor storage and metadata borrow the caller's slice.
    pub fn parse_prefix(bytes: &'a [u8], limits: Limits) -> Result<(Self, usize)> {
        if bytes.len() > limits.max_input_bytes {
            return Err(Error::Limit);
        }
        let pre = word(bytes, 4)?;
        let post = word(bytes, 8)?;
        if pre != 512 || post != 2048 {
            return Err(Error::V1FilterLengths { pre, post });
        }
        let layers = dimension(bytes, 0x2814)?;
        if layers == 0 {
            return Err(Error::Shape);
        }
        if layers > limits.max_layers {
            return Err(Error::Limit);
        }
        let sizes_end = layers
            .checked_add(1)
            .and_then(|count| count.checked_mul(4))
            .and_then(|len| SIZES_OFFSET.checked_add(len))
            .ok_or(Error::Overflow)?;
        if sizes_end > limits.max_input_bytes {
            return Err(Error::Limit);
        }
        let sizes = bytes.get(SIZES_OFFSET..sizes_end).ok_or(Error::Truncated)?;
        for offset in (0..sizes.len()).step_by(4) {
            let size = dimension(sizes, offset)?;
            if size == 0 {
                return Err(Error::Shape);
            }
            if size > limits.max_dimension {
                return Err(Error::Limit);
            }
        }
        let mut tail = sizes_end;
        for index in 0..layers {
            let inputs = dimension(sizes, index * 4)?;
            let outputs = dimension(sizes, (index + 1) * 4)?;
            tail = tail
                .checked_add(tensor_bytes(inputs, outputs)?)
                .and_then(|offset| outputs.checked_mul(4)?.checked_add(offset))
                .ok_or(Error::Overflow)?;
        }
        let end = tail.checked_add(8).ok_or(Error::Overflow)?;
        if end > limits.max_input_bytes {
            return Err(Error::Limit);
        }
        let record = bytes.get(..end).ok_or(Error::Truncated)?;
        Ok((
            Self {
                bytes: record,
                sizes,
                tensors: &record[sizes_end..tail],
                tail,
            },
            end,
        ))
    }

    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn sample_rate(&self) -> f32 {
        f32::from_bits(word(self.bytes, 0).expect("validated header"))
    }

    pub fn pre_coefficients(&self) -> Floats<'a> {
        Floats(&self.bytes[0xc..0x80c])
    }

    pub fn post_coefficients(&self) -> Floats<'a> {
        Floats(&self.bytes[0x80c..0x280c])
    }

    pub fn constant_gains(&self) -> [f32; 2] {
        [0x280c, 0x2810]
            .map(|offset| f32::from_bits(word(self.bytes, offset).expect("validated gain fields")))
    }

    /// Frequency and bandwidth in serialized order; see `crate::filters` for the
    /// recovered mathematical coefficient law and two-state recurrence.
    pub fn high_pass(&self) -> [f32; 2] {
        [self.tail, self.tail + 4]
            .map(|offset| f32::from_bits(word(self.bytes, offset).expect("validated tail fields")))
    }

    pub fn layer_sizes(&self) -> impl ExactSizeIterator<Item = u32> + 'a {
        self.sizes
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().expect("four-byte word")))
    }

    pub fn layers(&self) -> impl Iterator<Item = Layer<'a>> + 'a {
        let mut remaining = self.tensors;
        self.sizes.windows(8).step_by(4).map(move |pair| {
            let inputs = dimension(pair, 0).expect("validated dimension");
            let outputs = dimension(pair, 4).expect("validated dimension");
            let weights_end = tensor_bytes(inputs, outputs).expect("validated tensor size");
            let biases_end = weights_end + outputs * 4;
            let layer = Layer {
                inputs,
                outputs,
                weights: Floats(&remaining[..weights_end]),
                biases: Floats(&remaining[weights_end..biases_end]),
            };
            remaining = &remaining[biases_end..];
            layer
        })
    }

    /// Recovered rate tolerance and finite runtime fields, independent of topology.
    /// Success does not establish executable topology, filter semantics or audio parity.
    pub fn validate_runtime_values(&self) -> Result<()> {
        let scalars = [self.sample_rate()]
            .into_iter()
            .chain(self.constant_gains())
            .chain(self.high_pass());
        let tensors = self
            .layers()
            .flat_map(|layer| layer.weights.iter().chain(layer.biases.iter()));
        if scalars
            .chain(self.pre_coefficients().iter())
            .chain(self.post_coefficients().iter())
            .chain(tensors)
            .any(|value| !value.is_finite())
        {
            return Err(Error::NonFinite);
        }
        let rate = self.sample_rate();
        if (rate - 48_000.0).abs() > 1e-5 * rate.abs().min(48_000.0) {
            return Err(Error::SampleRate);
        }
        Ok(())
    }

    /// Extracts only the fixed neural tensors. The caller supplies beta explicitly:
    /// the separate serialized mathematical gains adapter is opt-in. External fields
    /// are not validated by this tensor-only extraction API.
    pub fn network_parameters(&self, output_coefficient: f32) -> Result<NetworkParameters> {
        if !self.layer_sizes().eq([1, 2, 2, 1]) {
            return Err(Error::UnsupportedV1Shape);
        }
        let mut layers = self.layers();
        let first = layers.next().expect("three validated layers");
        let middle = layers.next().expect("three validated layers");
        let last = layers.next().expect("three validated layers");
        NetworkParameters::new(
            [0, 1].map(|i| first.weights.get(i).expect("validated shape")),
            [0, 1].map(|i| first.biases.get(i).expect("validated shape")),
            [0, 1].map(|o| [0, 1].map(|i| middle.weights.get(o * 2 + i).expect("validated shape"))),
            [0, 1].map(|i| middle.biases.get(i).expect("validated shape")),
            [0, 1].map(|i| last.weights.get(i).expect("validated shape")),
            last.biases.get(0).expect("validated shape"),
            output_coefficient,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::{NeuralCore, SerializedMathematicalGainsCore};

    fn put(bytes: &mut [u8], offset: usize, word: u32) {
        bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
    }

    fn fixture(sizes: &[u32]) -> Vec<u8> {
        let mut bytes = vec![0; SIZES_OFFSET];
        for (offset, bits) in [
            (0, 48_000_f32.to_bits()),
            (4, 512),
            (8, 2048),
            (0x280c, 1_f32.to_bits()),
            (0x2810, 2_f32.to_bits()),
            (0x2814, u32::try_from(sizes.len() - 1).unwrap()),
        ] {
            put(&mut bytes, offset, bits);
        }
        for size in sizes {
            bytes.extend_from_slice(&size.to_le_bytes());
        }
        for pair in sizes.windows(2) {
            for index in 0..pair[0] * pair[1] + pair[1] {
                bytes.extend_from_slice(&(index as f32 / 16.0).to_le_bytes());
            }
        }
        bytes.extend_from_slice(&20_f32.to_le_bytes());
        bytes.extend_from_slice(&0.7_f32.to_le_bytes());
        bytes
    }

    fn mathematical_gains_fixture(alpha: f32, beta: f32) -> Vec<u8> {
        let mut bytes = fixture(&[1, 2, 2, 1]);
        put(&mut bytes, 0x280c, alpha.to_bits());
        put(&mut bytes, 0x2810, beta.to_bits());
        // Asymmetric weights and nonzero biases discriminate alpha placement.
        let tensors: [f32; 13] = [
            0.5, -0.25, 0.25, -0.5, 0.75, -0.5, 0.25, 0.625, 0.125, -0.0625, 0.5, -0.375, 0.125,
        ];
        for (index, value) in tensors.into_iter().enumerate() {
            put(&mut bytes, 0x2828 + 4 * index, value.to_bits());
        }
        bytes
    }

    #[test]
    fn serialized_mathematical_gains_alpha_precedes_affine_and_beta_scales_baseline() {
        let bytes = mathematical_gains_fixture(1.5, 1.75);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut adapter = SerializedMathematicalGainsCore::new(&record).unwrap();
        // Independent separate-operation f32 oracle with alpha on input only.
        let samples = [0.0, 0.25, 0.25, 0.25001, -0.5, 1.0, -1.0];
        let expected = [
            0,
            0x3c1d_7440,
            0x3d18_b690,
            0x3d82_84d0,
            0x3d34_9cd0,
            0x3c59_8700,
            0x3c92_ee00,
        ];
        for (sample, bits) in samples.into_iter().zip(expected) {
            assert_eq!(adapter.process(sample).unwrap().to_bits(), bits);
        }
        adapter.reset().unwrap();
        assert_eq!(adapter.process(0.0).unwrap(), 0.0);
        let correct = adapter.process(0.25).unwrap();
        let mut no_alpha = NeuralCore::new(record.network_parameters(1.75).unwrap()).unwrap();
        no_alpha.process(0.0).unwrap();
        let wrong_output_scaling = 1.5 * no_alpha.process(0.25).unwrap();
        assert_ne!(correct.to_bits(), wrong_output_scaling.to_bits());
        let bias_scaled = crate::network::NetworkParameters::new(
            [0.75, -0.375],
            [0.375, -0.75],
            [[0.75, -0.5], [0.25, 0.625]],
            [0.125, -0.0625],
            [0.5, -0.375],
            0.125,
            1.75,
        )
        .unwrap();
        let mut wrong_preactivation_scaling = NeuralCore::new(bias_scaled).unwrap();
        wrong_preactivation_scaling.process(0.0).unwrap();
        assert_ne!(
            correct.to_bits(),
            wrong_preactivation_scaling.process(0.25).unwrap().to_bits()
        );

        let bytes = mathematical_gains_fixture(1.0, 1.75);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        adapter.reload(&record).unwrap();
        let mut core = NeuralCore::new(record.network_parameters(1.75).unwrap()).unwrap();
        for sample in samples {
            assert_eq!(
                adapter.process(sample).unwrap().to_bits(),
                core.process(sample).unwrap().to_bits()
            );
        }
    }

    #[test]
    fn serialized_mathematical_gains_chunks_reset_and_instances_preserve_history() {
        let bytes = mathematical_gains_fixture(1.5, 1.75);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let inputs = [0.0, 0.25, -0.5, 1.0, -1.0];
        let mut contiguous = SerializedMathematicalGainsCore::new(&record).unwrap();
        let mut expected = [0.0; 5];
        contiguous.process_chunk(&inputs, &mut expected).unwrap();
        let mut chunked = SerializedMathematicalGainsCore::new(&record).unwrap();
        let mut actual = [0.0; 5];
        for (input, output) in inputs.chunks(2).zip(actual.chunks_mut(2)) {
            chunked.process_chunk(input, output).unwrap();
        }
        assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
        contiguous.reset().unwrap();
        contiguous.process_chunk(&inputs, &mut actual).unwrap();
        assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
        let mut independent = SerializedMathematicalGainsCore::new(&record).unwrap();
        assert_eq!(independent.process(0.0).unwrap(), 0.0);
        assert_eq!(
            independent.process(0.25).unwrap().to_bits(),
            expected[1].to_bits()
        );
    }

    #[test]
    fn serialized_mathematical_gains_reject_external_data_and_reload_atomically() {
        let bytes = mathematical_gains_fixture(1.5, 1.75);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut adapter = SerializedMathematicalGainsCore::new(&record).unwrap();
        adapter.process(0.25).unwrap();
        let mut before = adapter.clone();
        for (offset, value, error) in [
            (0, 48_001.0, Error::SampleRate),
            (0xc, f32::NAN, Error::NonFinite),
            (0x80c, f32::INFINITY, Error::NonFinite),
            (0x280c, 0.5, Error::ConstantGainRange),
            (0x2810, 0.1, Error::ConstantGainRange),
            (0x2810, f32::NAN, Error::NonFinite),
            (0x285c, 24_000.0, Error::HighPassDomain),
            (0x2860, 0.0, Error::HighPassDomain),
        ] {
            let mut bad = bytes.clone();
            put(&mut bad, offset, value.to_bits());
            let bad = RuntimeData::parse(&bad, Limits::default()).unwrap();
            assert_eq!(
                SerializedMathematicalGainsCore::new(&bad).unwrap_err(),
                error
            );
            assert_eq!(adapter.reload(&bad), Err(error));
        }
        let unsupported = fixture(&[1, 3, 1]);
        let unsupported = RuntimeData::parse(&unsupported, Limits::default()).unwrap();
        assert_eq!(adapter.reload(&unsupported), Err(Error::UnsupportedV1Shape));
        // Tensor-only explicit-beta extraction continues to ignore external fields.
        let mut external_bad = bytes.clone();
        put(&mut external_bad, 0x280c, f32::NAN.to_bits());
        let external_bad = RuntimeData::parse(&external_bad, Limits::default()).unwrap();
        external_bad.network_parameters(0.8).unwrap();
        // Finite tensors whose reset arithmetic overflows must not replace old state.
        let mut seed_overflow = bytes.clone();
        put(&mut seed_overflow, 0x2830, 2.0_f32.to_bits());
        put(&mut seed_overflow, 0x2834, 2.0_f32.to_bits());
        for offset in [0x2838, 0x283c] {
            put(&mut seed_overflow, offset, f32::MAX.to_bits());
        }
        let seed_overflow = RuntimeData::parse(&seed_overflow, Limits::default()).unwrap();
        assert_eq!(adapter.reload(&seed_overflow), Err(Error::NumericOverflow));
        for sample in [-0.25, 0.5, 0.0] {
            assert_eq!(
                adapter.process(sample).unwrap().to_bits(),
                before.process(sample).unwrap().to_bits()
            );
        }
    }

    #[test]
    fn serialized_mathematical_gains_failed_scaling_core_and_chunks_are_atomic() {
        let bytes = mathematical_gains_fixture(1.5, 1.75);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut adapter = SerializedMathematicalGainsCore::new(&record).unwrap();
        adapter.process(0.25).unwrap();
        let mut before = adapter.clone();
        for (input, error) in [
            (f32::NAN, Error::NonFinite),
            (f32::MAX, Error::NumericOverflow),
        ] {
            assert_eq!(adapter.process(input).unwrap_err(), error);
            let mut output = [123.0; 2];
            assert_eq!(
                adapter.process_chunk(&[0.1, input], &mut output),
                Err(error)
            );
            assert_eq!(output, [123.0; 2]);
            assert_eq!(adapter.process_chunk(&[], &mut output), Err(Error::Shape));
            adapter.process_chunk(&[], &mut []).unwrap();
        }
        for sample in [-0.25, 0.5, 0.0] {
            assert_eq!(
                adapter.process(sample).unwrap().to_bits(),
                before.process(sample).unwrap().to_bits()
            );
        }
        // Fail inside the core rather than the alpha multiplication.
        let mut core_overflow = bytes.clone();
        put(&mut core_overflow, 0x2828, 2.0_f32.to_bits());
        let core_overflow = RuntimeData::parse(&core_overflow, Limits::default()).unwrap();
        adapter.reload(&core_overflow).unwrap();
        let mut before = adapter.clone();
        assert_eq!(adapter.process(f32::MAX / 2.0), Err(Error::NumericOverflow));
        let mut output = [123.0; 2];
        assert_eq!(
            adapter.process_chunk(&[0.1, f32::MAX / 2.0], &mut output),
            Err(Error::NumericOverflow)
        );
        assert_eq!(output, [123.0; 2]);
        assert_eq!(
            adapter.process(0.25).unwrap().to_bits(),
            before.process(0.25).unwrap().to_bits()
        );
    }

    #[test]
    fn exact_layout_borrowed_tensors_and_prefix_consumption() {
        let bytes = fixture(&[1, 2, 2, 1]);
        assert_eq!(bytes.len(), 0x2864);
        let parsed = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        assert_eq!(parsed.as_bytes().as_ptr(), bytes.as_ptr());
        assert_eq!(parsed.sample_rate(), 48_000.0);
        assert_eq!(parsed.pre_coefficients().len(), 512);
        assert_eq!(parsed.post_coefficients().len(), 2048);
        assert_eq!(parsed.constant_gains(), [1.0, 2.0]);
        assert_eq!(parsed.high_pass(), [20.0, 0.7]);
        assert!(parsed.layer_sizes().eq([1, 2, 2, 1]));
        let layers: Vec<_> = parsed.layers().collect();
        assert_eq!(
            layers[0].weights.as_bytes().as_ptr(),
            bytes[0x2828..].as_ptr()
        );
        assert_eq!(
            layers[1].weights.iter().collect::<Vec<_>>(),
            [0.0, 0.0625, 0.125, 0.1875]
        );
        assert_eq!(layers[1].biases.iter().collect::<Vec<_>>(), [0.25, 0.3125]);
        assert_eq!(layers[2].biases.get(0), Some(0.125));
        assert_eq!(layers[2].biases.get(usize::MAX), None);
        parsed.validate_runtime_values().unwrap();
        NeuralCore::new(parsed.network_parameters(0.8).unwrap()).unwrap();

        let mut extended = bytes.clone();
        extended.extend_from_slice(&[0xff; 16]);
        assert!(matches!(
            RuntimeData::parse(&extended, Limits::default()),
            Err(Error::TrailingBytes)
        ));
        let (prefix, consumed) = RuntimeData::parse_prefix(&extended, Limits::default()).unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(prefix.as_bytes(), bytes);
        assert!(matches!(
            RuntimeData::parse_prefix(
                &extended,
                Limits {
                    max_input_bytes: bytes.len(),
                    ..Limits::default()
                }
            ),
            Err(Error::Limit)
        ));
    }

    #[test]
    fn truncations_counts_dimensions_and_checked_size_limits() {
        let bytes = fixture(&[1, 2, 2, 1]);
        for end in 0..bytes.len() {
            assert!(
                RuntimeData::parse(&bytes[..end], Limits::default()).is_err(),
                "end={end}"
            );
        }
        for limits in [
            Limits {
                max_input_bytes: bytes.len() - 1,
                ..Limits::default()
            },
            Limits {
                max_layers: 2,
                ..Limits::default()
            },
            Limits {
                max_dimension: 1,
                ..Limits::default()
            },
        ] {
            assert!(matches!(
                RuntimeData::parse(&bytes, limits),
                Err(Error::Limit)
            ));
        }
        for (offset, value, error) in [
            (0x2814, 0, Error::Shape),
            (0x2814, u32::MAX, Error::Limit),
            (SIZES_OFFSET, 0, Error::Shape),
            (SIZES_OFFSET + 4, u32::MAX, Error::Limit),
            (
                4,
                511,
                Error::V1FilterLengths {
                    pre: 511,
                    post: 2048,
                },
            ),
            (
                8,
                2047,
                Error::V1FilterLengths {
                    pre: 512,
                    post: 2047,
                },
            ),
        ] {
            let mut bad = bytes.clone();
            put(&mut bad, offset, value);
            assert_eq!(
                RuntimeData::parse(&bad, Limits::default()).unwrap_err(),
                error
            );
        }
        let mut huge = fixture(&[1, 1]);
        put(&mut huge, SIZES_OFFSET, u32::MAX);
        put(&mut huge, SIZES_OFFSET + 4, u32::MAX);
        assert!(matches!(
            RuntimeData::parse(
                &huge,
                Limits {
                    max_dimension: usize::MAX,
                    ..Limits::default()
                }
            ),
            Err(Error::Overflow)
        ));
    }

    #[test]
    fn structural_parsing_preserves_bits_and_validation_is_explicit() {
        let mut bytes = fixture(&[1, 2, 2, 1]);
        for (offset, bits) in [
            (0, 0x7fa1_2345),
            (0xc, 0x8000_0000),
            (0x80c, 0xff80_0000),
            (0x2828, 0x7fc0_4321),
        ] {
            put(&mut bytes, offset, bits);
        }
        let parsed = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        assert_eq!(parsed.as_bytes(), bytes);
        assert_eq!(parsed.sample_rate().to_bits(), 0x7fa1_2345);
        assert_eq!(
            parsed.pre_coefficients().get(0).unwrap().to_bits(),
            0x8000_0000
        );
        assert_eq!(
            parsed.post_coefficients().get(0).unwrap().to_bits(),
            0xff80_0000
        );
        assert_eq!(
            parsed
                .layers()
                .next()
                .unwrap()
                .weights
                .get(0)
                .unwrap()
                .to_bits(),
            0x7fc0_4321
        );
        assert_eq!(parsed.validate_runtime_values(), Err(Error::NonFinite));
        assert!(matches!(
            parsed.network_parameters(1.0),
            Err(Error::NonFinite)
        ));

        let mut bytes = fixture(&[1, 3, 1]);
        let parsed = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        parsed.validate_runtime_values().unwrap();
        assert!(matches!(
            parsed.network_parameters(1.0),
            Err(Error::UnsupportedV1Shape)
        ));
        put(&mut bytes, 0, 48_000.25_f32.to_bits());
        RuntimeData::parse(&bytes, Limits::default())
            .unwrap()
            .validate_runtime_values()
            .unwrap();
        put(&mut bytes, 0, 48_001_f32.to_bits());
        assert_eq!(
            RuntimeData::parse(&bytes, Limits::default())
                .unwrap()
                .validate_runtime_values(),
            Err(Error::SampleRate)
        );
    }
}

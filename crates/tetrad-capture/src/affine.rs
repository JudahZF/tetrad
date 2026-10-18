pub mod emitted;

use crate::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_dimension: usize,
    pub max_record_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_dimension: 16_384,
            max_record_bytes: 16 * 1024 * 1024,
        }
    }
}

/// A borrowed tag-4 record. Opaque intervals are retained in `as_bytes`;
/// selectors are not node IDs. Graph node IDs are runtime ordinals.
#[derive(Debug)]
pub struct AffineRecord<'a> {
    bytes: &'a [u8],
    weights: &'a [u8],
    biases: &'a [u8],
    inputs: usize,
    outputs: usize,
}

impl<'a> AffineRecord<'a> {
    /// Parses one record prefix, returning its consumed byte length. It does not
    /// treat following records or links as trailing garbage in this prefix API.
    pub fn parse(bytes: &'a [u8], limits: Limits) -> Result<(Self, usize)> {
        let word = |offset: usize| -> Result<u32> {
            let value = bytes.get(offset..offset + 4).ok_or(Error::Truncated)?;
            Ok(u32::from_le_bytes(
                value.try_into().expect("four-byte word"),
            ))
        };
        let tag = word(0)?;
        if tag != 4 {
            return Err(Error::Tag(tag));
        }
        let selector = word(4)?;
        let inputs = word(32)? as usize;
        let outputs = word(44)? as usize;
        let format = word(60)?;
        if selector != 4 || format != 2 {
            return Err(Error::Format { selector, format });
        }
        if inputs == 0 || outputs == 0 {
            return Err(Error::Shape);
        }
        if inputs > limits.max_dimension || outputs > limits.max_dimension {
            return Err(Error::Limit);
        }
        let weights_len = inputs
            .checked_mul(outputs)
            .and_then(|n| n.checked_mul(4))
            .ok_or(Error::Overflow)?;
        let biases_len = outputs.checked_mul(4).ok_or(Error::Overflow)?;
        let bias_offset = 84_usize.checked_add(weights_len).ok_or(Error::Overflow)?;
        let record_len = bias_offset.checked_add(biases_len).ok_or(Error::Overflow)?;
        if record_len > limits.max_record_bytes {
            return Err(Error::Limit);
        }
        let record = bytes.get(..record_len).ok_or(Error::Truncated)?;
        Ok((
            Self {
                bytes: record,
                weights: &record[76..76 + weights_len],
                biases: &record[bias_offset..],
                inputs,
                outputs,
            },
            record_len,
        ))
    }

    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn dimensions(&self) -> (usize, usize) {
        (self.inputs, self.outputs)
    }

    /// Serialized tensor bytes; parsing preserves non-finite bit patterns.
    /// Multi-output tensor orientation is not established by the O=1 path.
    pub fn weights(&self) -> impl Iterator<Item = f32> + '_ {
        self.weights
            .chunks_exact(4)
            .map(|word| f32::from_le_bytes(word.try_into().expect("four-byte word")))
    }

    pub fn biases(&self) -> impl Iterator<Item = f32> + '_ {
        self.biases
            .chunks_exact(4)
            .map(|word| f32::from_le_bytes(word.try_into().expect("four-byte word")))
    }

    /// Mathematical reference for the restricted recovered path, not a general
    /// O-output implementation or SHARC instruction-by-instruction simulator.
    pub fn reference_32(&self, inputs: &[f32]) -> Result<[f32; 32]> {
        if self.outputs != 1 || self.inputs < 4 || !self.inputs.is_multiple_of(4) {
            return Err(Error::UnsupportedShape);
        }
        if inputs.len() != self.inputs.checked_mul(32).ok_or(Error::Overflow)? {
            return Err(Error::Shape);
        }
        let bias = self.biases().next().expect("nonzero outputs");
        if !bias.is_finite()
            || inputs.iter().any(|v| !v.is_finite())
            || self.weights().any(|v| !v.is_finite())
        {
            return Err(Error::NonFinite);
        }
        let mut output = [0.0; 32];
        for (value, sample) in output.iter_mut().zip(inputs.chunks_exact(self.inputs)) {
            let mut sum = bias;
            for (weight, input) in self.weights().zip(sample) {
                let product = weight * input;
                sum += product;
            }
            if !sum.is_finite() {
                return Err(Error::NumericOverflow);
            }
            *value = sum;
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(inputs: usize, outputs: usize) -> Vec<u8> {
        let mut bytes = vec![0xab; 84 + 4 * inputs * outputs + 4 * outputs];
        for (offset, value) in [
            (0, 4),
            (4, 4),
            (32, inputs as u32),
            (44, outputs as u32),
            (60, 2),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (index, word) in bytes[76..76 + 4 * inputs * outputs]
            .chunks_exact_mut(4)
            .enumerate()
        {
            word.copy_from_slice(&(index as f32 + 1.0).to_le_bytes());
        }
        for word in bytes[84 + 4 * inputs * outputs..].chunks_exact_mut(4) {
            word.copy_from_slice(&11.0_f32.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn recovered_offsets_sizes_and_opaque_intervals_are_preserved() {
        for (inputs, outputs, expected_len) in [(4, 1, 104), (8, 1, 120), (12, 1, 136), (4, 3, 144)]
        {
            let mut bytes = record(inputs, outputs);
            bytes.extend_from_slice(&[9; 16]);
            let (parsed, consumed) = AffineRecord::parse(&bytes, Limits::default()).unwrap();
            assert_eq!(consumed, expected_len);
            assert_eq!(parsed.as_bytes(), &bytes[..consumed]);
            assert_eq!(parsed.dimensions(), (inputs, outputs));
            assert_eq!(parsed.weights().count(), inputs * outputs);
            assert_eq!(parsed.biases().collect::<Vec<_>>(), vec![11.0; outputs]);
        }
    }

    #[test]
    fn malformed_truncated_and_oversized_records_fail_before_tensor_allocation() {
        let bytes = record(4, 1);
        for end in 0..bytes.len() {
            assert!(AffineRecord::parse(&bytes[..end], Limits::default()).is_err());
        }
        assert!(matches!(
            AffineRecord::parse(
                &bytes,
                Limits {
                    max_dimension: 3,
                    ..Limits::default()
                }
            ),
            Err(Error::Limit)
        ));
        assert!(matches!(
            AffineRecord::parse(
                &bytes,
                Limits {
                    max_record_bytes: 103,
                    ..Limits::default()
                }
            ),
            Err(Error::Limit)
        ));
        let mut bad = bytes;
        bad[4..8].copy_from_slice(&77_u32.to_le_bytes());
        assert!(matches!(
            AffineRecord::parse(&bad, Limits::default()),
            Err(Error::Format { .. })
        ));
    }

    #[test]
    fn reference_is_restricted_to_verified_shapes_and_order() {
        let bytes = record(4, 1);
        let (parsed, _) = AffineRecord::parse(&bytes, Limits::default()).unwrap();
        let sample = [5.0, 7.0, 0.0, 0.0];
        let input: Vec<_> = (0..32).flat_map(|_| sample).collect();
        assert_eq!(parsed.reference_32(&input).unwrap(), [30.0; 32]);
        let bytes = record(4, 3);
        let (parsed, _) = AffineRecord::parse(&bytes, Limits::default()).unwrap();
        assert_eq!(parsed.reference_32(&input), Err(Error::UnsupportedShape));
        let bytes = record(3, 1);
        let (parsed, _) = AffineRecord::parse(&bytes, Limits::default()).unwrap();
        assert_eq!(
            parsed.reference_32(&[0.0; 96]),
            Err(Error::UnsupportedShape)
        );
    }
}

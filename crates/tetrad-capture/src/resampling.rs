//! Fixed 32→64→32 mathematical phases, with explicit caller-owned coefficients.
//! Minimal histories and f32 reduction are owned policies, not SHARC bit parity.

pub mod emitted;

use crate::{
    Error, Result,
    filters::{HistoryUpdate, finite},
};

/// No default or firmware-table import. Caller asserts lawful ownership and
/// records the source/policy; provenance text is not proof of stock matching.
#[derive(Debug, Clone, PartialEq)]
pub struct CallerOwnedV1ResamplingTable {
    coefficients: [f32; 16],
    provenance: String,
}

impl CallerOwnedV1ResamplingTable {
    pub fn new(coefficients: [f32; 16], provenance: String) -> Result<Self> {
        if coefficients.iter().any(|value| !value.is_finite()) {
            return Err(Error::NonFinite);
        }
        if provenance.trim().is_empty() {
            return Err(Error::ResamplingTableProvenance);
        }
        Ok(Self {
            coefficients,
            provenance,
        })
    }

    pub fn provenance(&self) -> &str {
        &self.provenance
    }

    pub fn coefficients(&self) -> &[f32; 16] {
        &self.coefficients
    }
}

/// Owns 15 low-rate and 31 high-rate samples, newest first. Both phases double
/// by addition to themselves; return selects even phase without extra doubling.
/// No fractional-rate phase, arbitrary-rate conversion or inferred stock table.
#[derive(Debug, Clone, PartialEq)]
pub struct V1ResamplingReference {
    table: CallerOwnedV1ResamplingTable,
    low: [f32; 15],
    high: [f32; 31],
}

fn delayed<const N: usize>(current: f32, history: &[f32; N], delay: usize) -> f32 {
    if delay == 0 {
        current
    } else {
        history[delay - 1]
    }
}

fn push<const N: usize>(history: &mut [f32; N], sample: f32) {
    history.rotate_right(1);
    history[0] = sample;
}

impl V1ResamplingReference {
    pub fn new(table: CallerOwnedV1ResamplingTable) -> Self {
        Self {
            table,
            low: [0.0; 15],
            high: [0.0; 31],
        }
    }

    pub fn table(&self) -> &CallerOwnedV1ResamplingTable {
        &self.table
    }

    pub fn reset(&mut self) {
        self.low = [0.0; 15];
        self.high = [0.0; 31];
    }

    /// Explicit OWNED table update; preserve keeps both mathematical histories.
    pub fn reload(&mut self, table: CallerOwnedV1ResamplingTable, policy: HistoryUpdate) {
        self.table = table;
        if policy == HistoryUpdate::Reset {
            self.reset();
        }
    }

    /// Atomic fixed callback. Input reduction policy: j ascending, two products,
    /// pair addition, sum addition, then doubling addition. This input reduction
    /// order is an owned mathematical policy, not fully proved stock ordering.
    /// Cloning copies histories and allocates a copy of the provenance String.
    pub fn interpolate_block(&mut self, inputs: &[f32; 32]) -> Result<[f32; 64]> {
        if inputs.iter().any(|value| !value.is_finite()) {
            return Err(Error::NonFinite);
        }
        let mut next = self.clone();
        let mut outputs = [0.0; 64];
        for (n, &input) in inputs.iter().enumerate() {
            let mut sums = [0.0; 2];
            for j in 0..8 {
                let recent = delayed(input, &next.low, j);
                let old = delayed(input, &next.low, 15 - j);
                let even = next.table.coefficients[2 * j];
                let odd = next.table.coefficients[2 * j + 1];
                let p0 = finite(even * recent)?;
                let p1 = finite(odd * old)?;
                sums[0] = finite(sums[0] + finite(p0 + p1)?)?;
                let p0 = finite(even * old)?;
                let p1 = finite(odd * recent)?;
                sums[1] = finite(sums[1] + finite(p0 + p1)?)?;
            }
            outputs[2 * n] = finite(sums[0] + sums[0])?;
            outputs[2 * n + 1] = finite(sums[1] + sums[1])?;
            push(&mut next.low, input);
        }
        *self = next;
        Ok(outputs)
    }

    /// Pair addition BEFORE table product; even/odd coefficient lanes accumulate
    /// eight products each in ascending index order, then add the two lanes.
    /// The lane grouping is recovered; host f32 is not target numerical proof.
    /// All 64 inputs are validated, including the last odd sample retained for
    /// the next callback. Histories/output commit only after the whole block.
    pub fn decimate_block(&mut self, inputs: &[f32; 64]) -> Result<[f32; 32]> {
        if inputs.iter().any(|value| !value.is_finite()) {
            return Err(Error::NonFinite);
        }
        let mut next = self.clone();
        let mut outputs = [0.0; 32];
        for (n, output) in outputs.iter_mut().enumerate() {
            let current = inputs[2 * n];
            let mut lanes = [0.0; 2];
            for j in 0..16 {
                let left = delayed(current, &next.high, j);
                let right = delayed(current, &next.high, 31 - j);
                let pair = finite(left + right)?;
                let product = finite(next.table.coefficients[j] * pair)?;
                lanes[j % 2] = finite(lanes[j % 2] + product)?;
            }
            *output = finite(lanes[0] + lanes[1])?;
            push(&mut next.high, current);
            push(&mut next.high, inputs[2 * n + 1]);
        }
        *self = next;
        Ok(outputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(coefficients: [f32; 16]) -> CallerOwnedV1ResamplingTable {
        CallerOwnedV1ResamplingTable::new(
            coefficients,
            "Owned synthetic mathematical discriminator; not stock".into(),
        )
        .unwrap()
    }

    #[test]
    fn asymmetric_t0_phases_even_return_boundary_and_reset() {
        let mut coefficients = [0.0; 16];
        coefficients[0] = 1.0;
        let mut resampler = V1ResamplingReference::new(table(coefficients));
        let mut input = [0.0; 32];
        input[0] = 1.0;
        input[31] = 3.0;
        let first = resampler.interpolate_block(&input).unwrap();
        let mut expected = [0.0; 64];
        expected[0] = 2.0;
        expected[31] = 2.0; // phase 1 at low sample 15, not phase 0.
        expected[62] = 6.0;
        assert_eq!(first, expected);
        let second = resampler.interpolate_block(&[0.0; 32]).unwrap();
        let mut expected = [0.0; 64];
        expected[29] = 6.0; // prior callback sample 31 at low sample 46.
        assert_eq!(second, expected);
        let mut high = [0.0; 64];
        high[0] = 5.0;
        high[1] = 7.0;
        high[33] = 11.0;
        high[63] = 13.0;
        let first = resampler.decimate_block(&high).unwrap();
        let mut expected = [0.0; 32];
        expected[0] = 5.0; // u[0]+u[-31], not odd u[1].
        expected[16] = 7.0; // u[32]+u[1].
        assert_eq!(first, expected);
        let second = resampler.decimate_block(&[0.0; 64]).unwrap();
        let mut expected = [0.0; 32];
        expected[0] = 11.0; // u[64]+u[33], even callback start.
        expected[15] = 13.0;
        assert_eq!(second, expected);
        resampler.reset();
        assert_eq!(resampler.low, [0.0; 15]);
        assert_eq!(resampler.high, [0.0; 31]);
        assert_eq!(resampler.interpolate_block(&input).unwrap()[0], 2.0);
        assert_eq!(resampler.decimate_block(&[0.0; 64]).unwrap(), [0.0; 32]);
    }

    #[test]
    fn asymmetric_table_continuous_convolution_and_return_lane_reduction() {
        let coefficients = std::array::from_fn(|j| (j as f32 - 6.0) / 64.0);
        let low: Vec<_> = (0..96).map(|n| ((n % 9) as f32 - 4.0) / 8.0).collect();
        let high: Vec<_> = (0..192).map(|n| ((n % 13) as f32 - 6.0) / 16.0).collect();
        // Independent continuous-time zero-extension convolution, without rings
        // or callback windows. Dyadic values make these sums exactly representable.
        let get = |samples: &[f32], n: isize| if n < 0 { 0.0 } else { samples[n as usize] };
        let mut resampler = V1ResamplingReference::new(table(coefficients));
        for (block, samples) in low.chunks_exact(32).enumerate() {
            let actual = resampler
                .interpolate_block(samples.try_into().unwrap())
                .unwrap();
            for n in 0..32 {
                let time = (block * 32 + n) as isize;
                let mut phases = [0.0; 2];
                for j in 0..8 {
                    let recent = get(&low, time - j as isize);
                    let old = get(&low, time - 15 + j as isize);
                    phases[0] += coefficients[2 * j] * recent + coefficients[2 * j + 1] * old;
                    phases[1] += coefficients[2 * j] * old + coefficients[2 * j + 1] * recent;
                }
                assert_eq!(&actual[2 * n..2 * n + 2], &phases.map(|sum| sum + sum));
            }
        }
        for (block, samples) in high.chunks_exact(64).enumerate() {
            let actual = resampler
                .decimate_block(samples.try_into().unwrap())
                .unwrap();
            for (n, &output) in actual.iter().enumerate() {
                let time = (block * 64 + 2 * n) as isize;
                let expected: f32 = coefficients
                    .iter()
                    .enumerate()
                    .map(|(j, &tap)| {
                        tap * (get(&high, time - j as isize) + get(&high, time - 31 + j as isize))
                    })
                    .sum();
                assert_eq!(output, expected);
            }
        }
        let mut coefficients = [0.0; 16];
        coefficients[..3].copy_from_slice(&[16_777_216.0, 1.0, -16_777_216.0]);
        let mut lanes = V1ResamplingReference::new(table(coefficients));
        lanes.decimate_block(&[1.0; 64]).unwrap();
        assert_eq!(lanes.decimate_block(&[1.0; 64]).unwrap(), [2.0; 32]);
        // A single sequential accumulator over all 16 products would give zero.
    }

    #[test]
    fn table_explicitness_atomic_errors_and_owned_update_policy() {
        assert!(matches!(
            CallerOwnedV1ResamplingTable::new([0.0; 16], " ".into()),
            Err(Error::ResamplingTableProvenance)
        ));
        assert!(matches!(
            CallerOwnedV1ResamplingTable::new([f32::NAN; 16], "owned".into()),
            Err(Error::NonFinite)
        ));
        let mut coefficients = [0.0; 16];
        coefficients[0] = 1.0;
        let mut resampler = V1ResamplingReference::new(table(coefficients));
        resampler.interpolate_block(&[0.25; 32]).unwrap();
        resampler.decimate_block(&[0.5; 64]).unwrap();
        let before = resampler.clone();
        let mut invalid = [0.0; 32];
        invalid[31] = f32::NAN;
        assert_eq!(resampler.interpolate_block(&invalid), Err(Error::NonFinite));
        assert_eq!(resampler, before);
        invalid[31] = f32::MAX; // finite sum, doubling addition overflows.
        assert_eq!(
            resampler.interpolate_block(&invalid),
            Err(Error::NumericOverflow)
        );
        assert_eq!(resampler, before);
        let mut invalid = [0.0; 64];
        invalid[63] = f32::INFINITY; // unselected odd sample still must be finite.
        assert_eq!(resampler.decimate_block(&invalid), Err(Error::NonFinite));
        assert_eq!(resampler, before);
        let mut half = [0.0; 16];
        half[0] = 0.5;
        let mut pair_first = V1ResamplingReference::new(table(half));
        let pair_before = pair_first.clone();
        // Half-products would each be finite; the required pair addition is not.
        assert_eq!(
            pair_first.decimate_block(&[f32::MAX; 64]),
            Err(Error::NumericOverflow)
        );
        assert_eq!(pair_first, pair_before);
        coefficients[0] = 2.0;
        resampler.reload(table(coefficients), HistoryUpdate::Preserve);
        assert_eq!(resampler.low, before.low);
        assert_eq!(resampler.decimate_block(&[0.0; 64]).unwrap()[0], 1.0);
        resampler.reload(table(coefficients), HistoryUpdate::Reset);
        assert_eq!(resampler.interpolate_block(&[0.0; 32]).unwrap(), [0.0; 64]);
        assert_eq!(resampler.decimate_block(&[0.0; 64]).unwrap(), [0.0; 32]);
    }
}

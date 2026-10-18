use super::{V1HighPassCoefficients, finite, normalize_v1_prefilter_reference};
use crate::{Error, Result};

/// Explicit owned update policy; no stock parameter publication timing is inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryUpdate {
    Preserve,
    Reset,
}

/// Two accumulated future contributions, not raw input/output delays.
/// Each product, add and subtraction is a separate finite f32 operation.
#[derive(Debug, Clone, PartialEq)]
pub struct V1HighPassReference {
    coefficients: V1HighPassCoefficients,
    state: [f32; 2],
}

impl V1HighPassReference {
    pub fn new(coefficients: V1HighPassCoefficients) -> Result<Self> {
        Self::from_state(coefficients, [0.0; 2])
    }

    /// Explicit finite caller-owned accumulated state, useful for reference probes.
    pub fn from_state(coefficients: V1HighPassCoefficients, state: [f32; 2]) -> Result<Self> {
        if coefficients
            .feedforward
            .iter()
            .chain(&coefficients.feedback)
            .chain(&state)
            .any(|value| !value.is_finite())
        {
            return Err(Error::NonFinite);
        }
        Ok(Self {
            coefficients,
            state,
        })
    }

    pub fn state(&self) -> [f32; 2] {
        self.state
    }

    pub fn reset(&mut self) {
        self.state = [0.0; 2];
    }

    /// Validate before committing; preserve retains even identity-filter transients.
    pub fn reload(
        &mut self,
        coefficients: V1HighPassCoefficients,
        policy: HistoryUpdate,
    ) -> Result<()> {
        let state = match policy {
            HistoryUpdate::Preserve => self.state,
            HistoryUpdate::Reset => [0.0; 2],
        };
        *self = Self::from_state(coefficients, state)?;
        Ok(())
    }

    pub fn process(&mut self, input: f32) -> Result<f32> {
        if !input.is_finite() {
            return Err(Error::NonFinite);
        }
        let [b0, b1, b2] = self.coefficients.feedforward;
        let [a1, a2] = self.coefficients.feedback;
        let [s1, s2] = self.state;
        let p0 = finite(b0 * input)?;
        let output = finite(p0 + s1)?;
        let p1 = finite(b1 * input)?;
        let t1 = finite(p1 + s2)?;
        let q1 = finite(a1 * output)?;
        let next_s1 = finite(t1 - q1)?;
        let p2 = finite(b2 * input)?;
        let q2 = finite(a2 * output)?;
        let next_s2 = finite(p2 - q2)?;
        self.state = [next_s1, next_s2];
        Ok(output)
    }

    /// Recovered stock callback shape; output and state commit only after success.
    pub fn process_block(&mut self, inputs: &[f32; 32]) -> Result<[f32; 32]> {
        let mut next = self.clone();
        let mut outputs = [0.0; 32];
        for (&input, output) in inputs.iter().zip(&mut outputs) {
            *output = next.process(input)?;
        }
        *self = next;
        Ok(outputs)
    }

    /// Arbitrary chunks are OWNED policy, not a recovered stock callback shape.
    /// Two deterministic passes retain caller output and state on all errors.
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

/// Causal serialized `c[0]*current input`, descending tap reduction N-1..0.
/// Construction restricts N to 512/2048; the N<=3 stock helper is not implemented.
/// History is an N-word ring; cursor is the next insertion slot, not a delay word.
#[derive(Debug, Clone, PartialEq)]
pub struct V1FirReference<const N: usize> {
    coefficients: [f32; N],
    history: [f32; N],
    cursor: usize,
}

impl<const N: usize> V1FirReference<N> {
    /// Explicit already-prepared taps. Imported pre taps must use
    /// `from_imported_prefilter`; post taps must never be normalized.
    pub fn from_prepared_coefficients(coefficients: [f32; N]) -> Result<Self> {
        if N != 512 && N != 2048 {
            return Err(Error::UnsupportedV1FirLength(N));
        }
        if coefficients.iter().any(|value| !value.is_finite()) {
            return Err(Error::NonFinite);
        }
        Ok(Self {
            coefficients,
            history: [0.0; N],
            cursor: 0,
        })
    }

    pub fn reset(&mut self) {
        self.history = [0.0; N];
        self.cursor = 0;
    }

    /// Explicit prepared-tap update; validation failure retains coefficients/history.
    pub fn reload_prepared(&mut self, coefficients: [f32; N], policy: HistoryUpdate) -> Result<()> {
        let mut replacement = Self::from_prepared_coefficients(coefficients)?;
        if policy == HistoryUpdate::Preserve {
            replacement.history = self.history;
            replacement.cursor = self.cursor;
        }
        *self = replacement;
        Ok(())
    }

    // Only invoked on staged copies; finite invariant follows from construction
    // and successful inputs. Never skip zero-tap products or change reduction order.
    fn process_inner(&mut self, input: f32) -> Result<f32> {
        if !input.is_finite() {
            return Err(Error::NonFinite);
        }
        self.history[self.cursor] = input;
        let mut output = 0.0;
        for tap in (0..N).rev() {
            let position = (self.cursor + N - tap) % N;
            let product = finite(self.coefficients[tap] * self.history[position])?;
            output = finite(output + product)?;
        }
        self.cursor = (self.cursor + 1) % N;
        Ok(output)
    }

    /// Atomic 32-sample stock-shaped callback; clones O(N) arrays once.
    pub fn process_block(&mut self, inputs: &[f32; 32]) -> Result<[f32; 32]> {
        let mut next = self.clone();
        let mut outputs = [0.0; 32];
        for (&input, output) in inputs.iter().zip(&mut outputs) {
            *output = next.process_inner(input)?;
        }
        *self = next;
        Ok(outputs)
    }

    /// OWNED arbitrary-chunk continuity policy; two passes and O(N) array clones.
    /// No heap allocation here; no realtime/whole-path heap-free claim.
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
}

impl V1FirReference<512> {
    /// Import pre taps using the recovered scalar normalization reference.
    pub fn from_imported_prefilter(coefficients: &[f32; 512]) -> Result<Self> {
        Self::from_prepared_coefficients(normalize_v1_prefilter_reference(coefficients)?)
    }

    pub fn reload_imported_prefilter(
        &mut self,
        coefficients: &[f32; 512],
        policy: HistoryUpdate,
    ) -> Result<()> {
        self.reload_prepared(normalize_v1_prefilter_reference(coefficients)?, policy)
    }
}

impl V1FirReference<2048> {
    /// Post taps retain their serialized values; no normalization is applied.
    pub fn from_postfilter(coefficients: [f32; 2048]) -> Result<Self> {
        Self::from_prepared_coefficients(coefficients)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dyadic_hp() -> V1HighPassCoefficients {
        V1HighPassCoefficients {
            feedforward: [0.5, 0.25, -0.125],
            feedback: [-0.25, 0.125],
        }
    }

    #[test]
    fn high_pass_dyadic_state_and_retained_identity_transient() {
        let mut hp = V1HighPassReference::new(dyadic_hp()).unwrap();
        let mut outputs = [0.0; 4];
        hp.process_chunk(&[1.0, 0.0, 0.0, 0.0], &mut outputs)
            .unwrap();
        assert_eq!(outputs, [0.5, 0.375, -0.09375, -0.0703125]);
        assert_eq!(hp.state(), [-0.005859375, 0.0087890625]);
        let identity = V1HighPassCoefficients {
            feedforward: [1.0, 0.0, 0.0],
            feedback: [0.0; 2],
        };
        hp.reload(identity, HistoryUpdate::Preserve).unwrap();
        assert_eq!(hp.process(0.0).unwrap(), -0.005859375);
        assert_eq!(hp.process(0.0).unwrap(), 0.0087890625);
        assert_eq!(hp.process(0.0).unwrap(), 0.0);
        hp.reload(dyadic_hp(), HistoryUpdate::Reset).unwrap();
        assert_eq!(hp.state(), [0.0; 2]);
        assert_eq!(hp.process(1.0).unwrap(), 0.5);
        hp.reset();
        assert_eq!(hp.state(), [0.0; 2]);
    }

    #[test]
    fn high_pass_separate_state_rounds_reject_df1_substitution() {
        let mut hp = V1HighPassReference::new(V1HighPassCoefficients {
            feedforward: [0.37, -0.21, 0.13],
            feedback: [-0.43, 0.17],
        })
        .unwrap();
        // Independent Python float32 operation oracle. A left-to-right raw
        // x/y-history DF1 gives 0x3d8d97a3 and 0x3da70f85 at samples 3 and 5.
        let expected = [
            0x3d17_8d50,
            0xbda1_f9f0,
            0x3e8c_2294,
            0x3d8d_97a2,
            0xbea4_c305,
            0x3da7_0f84,
        ];
        for (input, bits) in [0.1, -0.2, 0.7, 0.3, -0.9, 0.01].into_iter().zip(expected) {
            assert_eq!(hp.process(input).unwrap().to_bits(), bits);
        }
    }

    #[test]
    fn high_pass_owned_chunks_stock_blocks_and_errors_are_atomic() {
        let inputs = std::array::from_fn::<_, 64, _>(|i| (i % 7) as f32 / 8.0);
        let mut whole = V1HighPassReference::new(dyadic_hp()).unwrap();
        let mut expected = [0.0; 64];
        whole.process_chunk(&inputs, &mut expected).unwrap();
        let mut blocked = V1HighPassReference::new(dyadic_hp()).unwrap();
        let first = blocked
            .process_block(inputs[..32].try_into().unwrap())
            .unwrap();
        let second = blocked
            .process_block(inputs[32..].try_into().unwrap())
            .unwrap();
        assert_eq!([first, second].concat(), expected);
        assert_eq!(blocked, whole);
        let before = blocked.clone();
        let mut output = [9.0; 2];
        assert_eq!(
            blocked.process_chunk(&[1.0, f32::NAN], &mut output),
            Err(Error::NonFinite)
        );
        assert_eq!(output, [9.0; 2]);
        assert_eq!(blocked, before);
        assert_eq!(blocked.process_chunk(&[], &mut output), Err(Error::Shape));
        let mut bad = dyadic_hp();
        bad.feedback[1] = f32::INFINITY;
        assert_eq!(
            blocked.reload(bad, HistoryUpdate::Reset),
            Err(Error::NonFinite)
        );
        assert_eq!(blocked, before);
        assert!(matches!(
            V1HighPassReference::from_state(dyadic_hp(), [f32::NAN, 0.0]),
            Err(Error::NonFinite)
        ));
        // Output can be finite while a future contribution overflows.
        let mut overflow = V1HighPassReference::new(V1HighPassCoefficients {
            feedforward: [0.0, 0.0, f32::MAX],
            feedback: [0.0; 2],
        })
        .unwrap();
        let before = overflow.clone();
        assert_eq!(overflow.process(2.0), Err(Error::NumericOverflow));
        assert_eq!(overflow, before);
        let mut bad_block = [0.0; 32];
        bad_block[31] = 2.0;
        assert_eq!(
            overflow.process_block(&bad_block),
            Err(Error::NumericOverflow)
        );
        assert_eq!(overflow, before);
    }

    fn check_fir_ring<const N: usize>() {
        let mut taps = [0.0; N];
        taps[..4].copy_from_slice(&[2.0, -3.0, 5.0, 7.0]);
        taps[N - 1] = 11.0;
        let mut fir = V1FirReference::from_prepared_coefficients(taps).unwrap();
        let mut inputs = vec![0.0; N + 64];
        inputs[0] = 1.0;
        inputs[N] = 2.0;
        let mut expected = vec![0.0; inputs.len()];
        expected[..4].copy_from_slice(&[2.0, -3.0, 5.0, 7.0]);
        expected[N - 1] = 11.0;
        expected[N..N + 4].copy_from_slice(&[4.0, -6.0, 10.0, 14.0]);
        let mut contiguous = fir.clone();
        let mut actual = vec![0.0; inputs.len()];
        contiguous.process_chunk(&inputs, &mut actual).unwrap();
        assert_eq!(actual, expected);
        let mut blocks = Vec::new();
        for block in inputs.chunks_exact(32) {
            blocks.extend(fir.process_block(block.try_into().unwrap()).unwrap());
        }
        assert_eq!(blocks, expected);
        assert_eq!(fir, contiguous);
        assert_eq!(fir.cursor, 64 % N);
        fir.reset();
        assert_eq!(fir.cursor, 0);
        assert_eq!(fir.history, [0.0; N]);
        let mut replay = vec![0.0; inputs.len()];
        for (input, output) in inputs.chunks(13).zip(replay.chunks_mut(13)) {
            fir.process_chunk(input, output).unwrap();
        }
        assert_eq!(replay, expected); // arbitrary chunk sizes are OWNED policy.
    }

    #[test]
    fn fixed_firs_asymmetric_impulse_wrap_reset_and_callback_continuity() {
        check_fir_ring::<512>();
        check_fir_ring::<2048>();
        assert!(matches!(
            V1FirReference::<3>::from_prepared_coefficients([0.0; 3]),
            Err(Error::UnsupportedV1FirLength(3))
        ));
    }

    #[test]
    fn fir_descending_rounds_and_pre_only_import_normalization() {
        let mut taps = [0.0; 2048];
        taps[..3].copy_from_slice(&[1.0, 16_777_216.0, -16_777_216.0]);
        let mut fir = V1FirReference::from_postfilter(taps).unwrap();
        let mut outputs = [0.0; 3];
        fir.process_chunk(&[1.0; 3], &mut outputs).unwrap();
        assert_eq!(outputs[2], 1.0); // ascending reduction gives 0.0.
        let mut pre = [0.0; 512];
        pre[..2].copy_from_slice(&[3.0, 4.0]);
        let imported = V1FirReference::from_imported_prefilter(&pre).unwrap();
        assert_eq!(
            imported.coefficients,
            normalize_v1_prefilter_reference(&pre).unwrap()
        );
        let mut post = [0.0; 2048];
        post[..2].copy_from_slice(&[3.0, 4.0]);
        assert_eq!(
            V1FirReference::from_postfilter(post).unwrap().coefficients,
            post
        );
    }

    #[test]
    fn fir_atomic_overflow_nonfinite_and_explicit_reload_policy() {
        let mut taps = [0.0; 512];
        taps[1] = 2.0;
        let mut fir = V1FirReference::from_prepared_coefficients(taps).unwrap();
        fir.process_chunk(&[3.0], &mut [0.0]).unwrap();
        let before = fir.clone();
        let mut outputs = [9.0; 2];
        assert_eq!(
            fir.process_chunk(&[1.0, f32::INFINITY], &mut outputs),
            Err(Error::NonFinite)
        );
        assert_eq!(outputs, [9.0; 2]);
        assert_eq!(fir, before);
        assert_eq!(fir.process_chunk(&[], &mut outputs), Err(Error::Shape));
        let mut bad = taps;
        bad[511] = f32::NAN;
        assert_eq!(
            fir.reload_prepared(bad, HistoryUpdate::Reset),
            Err(Error::NonFinite)
        );
        assert_eq!(fir, before);
        let mut bad_pre = taps;
        bad_pre[0] = f32::MAX;
        assert_eq!(
            fir.reload_imported_prefilter(&bad_pre, HistoryUpdate::Reset),
            Err(Error::NumericOverflow)
        );
        assert_eq!(fir, before);
        fir.reload_prepared(taps, HistoryUpdate::Preserve).unwrap();
        assert_eq!(fir.process_block(&[0.0; 32]).unwrap()[0], 6.0);
        fir.reload_prepared(taps, HistoryUpdate::Reset).unwrap();
        assert_eq!(fir.process_block(&[0.0; 32]).unwrap(), [0.0; 32]);
        // Finite products but a descending accumulation overflow.
        let mut taps = [0.0; 512];
        taps[..3].copy_from_slice(&[-f32::MAX, f32::MAX, f32::MAX]);
        let mut overflow = V1FirReference::from_prepared_coefficients(taps).unwrap();
        let before = overflow.clone();
        let mut outputs = [9.0; 3];
        assert_eq!(
            overflow.process_chunk(&[1.0; 3], &mut outputs),
            Err(Error::NumericOverflow)
        );
        assert_eq!(outputs, [9.0; 3]);
        assert_eq!(overflow, before);
        let mut block = [0.0; 32];
        block[31] = 2.0; // product overflow after 31 staged cursor advances.
        assert_eq!(overflow.process_block(&block), Err(Error::NumericOverflow));
        assert_eq!(overflow, before);
    }
}

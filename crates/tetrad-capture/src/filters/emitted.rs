//! Offline adapters for immutable owned FIR and high-pass words; allocations are intentional.
pub mod high_pass;

use tetrad_sharc::{
    address::{DmWordAddress32, PmWordAddress24},
    fir::{CompiledFir, FirLayout, FirLength, compile_fir, simulate_fir},
    simulator::{DmMemory, ScalarState, SimulationOutcome},
};

use super::{HistoryUpdate, V1HighPassCoefficients, normalize_v1_prefilter_reference};
use crate::{Error, Result};
use high_pass::CompiledHighPass;

/// Caller-selected logical units, not a physical PM/DM image or device mapping.
/// Input, taps and output are derived adjacently from `dm_base`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirLogicalPlacement {
    pub origin: PmWordAddress24,
    pub dm_base: DmWordAddress32,
}

/// Outputs and actual decoder counts. Instructions are not hardware cycles.
#[derive(Debug, Clone, PartialEq)]
pub struct EmittedFirBlock {
    pub outputs: [f32; 32],
    pub receipt: SimulationOutcome,
}

/// Fixed-32 offline raw-word FIR with caller-prepared natural taps.
/// Owns N-1 chronological prior samples; no mutable code or scratch is exposed.
#[derive(Debug, Clone, PartialEq)]
pub struct EmittedV1FirReference<const N: usize> {
    coefficients: [f32; N],
    history: Vec<f32>,
    compiled: CompiledFir,
}

impl<const N: usize> EmittedV1FirReference<N> {
    pub fn from_prepared_coefficients(
        coefficients: [f32; N],
        placement: FirLogicalPlacement,
    ) -> Result<Self> {
        let length = match N {
            512 => FirLength::Fir512,
            2048 => FirLength::Fir2048,
            _ => return Err(Error::UnsupportedV1FirLength(N)),
        };
        if coefficients.iter().any(|value| !value.is_finite()) {
            return Err(Error::NonFinite);
        }
        let taps = placement
            .dm_base
            .checked_add(length.taps() + 31)
            .map_err(tetrad_sharc::fir::FirError::from)?;
        let output = taps
            .checked_add(length.taps())
            .map_err(tetrad_sharc::fir::FirError::from)?;
        let layout = FirLayout::new(length, placement.dm_base, taps, output)?;
        let compiled = compile_fir(placement.origin, layout)?;
        Ok(Self {
            coefficients,
            history: vec![0.0; N - 1],
            compiled,
        })
    }

    /// Immutable image, layout and exact instruction budget for auditing.
    pub fn compiled(&self) -> &CompiledFir {
        &self.compiled
    }

    pub fn reset(&mut self) {
        self.history.fill(0.0);
    }

    /// Does not compile again. Failed validation retains taps, history and code.
    pub fn reload_prepared(&mut self, coefficients: [f32; N], policy: HistoryUpdate) -> Result<()> {
        if coefficients.iter().any(|value| !value.is_finite()) {
            return Err(Error::NonFinite);
        }
        self.coefficients = coefficients;
        if policy == HistoryUpdate::Reset {
            self.reset();
        }
        Ok(())
    }

    /// Allocates a window and bounded adjacent DM, plus simulator clones/write log.
    /// Executes emitted words only. Every error retains the complete instance.
    pub fn process_block(
        &mut self,
        inputs: &[f32; 32],
        instruction_budget: usize,
    ) -> Result<EmittedFirBlock> {
        let mut window = Vec::with_capacity(N + 31);
        window.extend_from_slice(&self.history);
        window.extend_from_slice(inputs);
        let mut words = Vec::with_capacity(2 * N + 63);
        words.extend(window.iter().map(|value| value.to_bits()));
        words.extend(self.coefficients.iter().map(|value| value.to_bits()));
        words.resize(2 * N + 63, 0);
        let mut memory = DmMemory::new(self.compiled.layout().input(), words)
            .map_err(tetrad_sharc::fir::FirError::from)?;
        let mut state = ScalarState::default();
        let receipt = simulate_fir(&self.compiled, &mut state, &mut memory, instruction_budget)?;
        let outputs =
            std::array::from_fn(|index| f32::from_bits(memory.words()[2 * N + 31 + index]));
        self.history.copy_from_slice(&window[32..]);
        Ok(EmittedFirBlock { outputs, receipt })
    }
}

impl EmittedV1FirReference<512> {
    /// Same owned prefilter normalization as the mathematical reference.
    pub fn from_imported_prefilter(
        coefficients: &[f32; 512],
        placement: FirLogicalPlacement,
    ) -> Result<Self> {
        Self::from_prepared_coefficients(normalize_v1_prefilter_reference(coefficients)?, placement)
    }

    pub fn reload_imported_prefilter(
        &mut self,
        coefficients: &[f32; 512],
        policy: HistoryUpdate,
    ) -> Result<()> {
        self.reload_prepared(normalize_v1_prefilter_reference(coefficients)?, policy)
    }
}

impl EmittedV1FirReference<2048> {
    /// Serialized post taps are retained without normalization.
    pub fn from_postfilter(
        coefficients: [f32; 2048],
        placement: FirLogicalPlacement,
    ) -> Result<Self> {
        Self::from_prepared_coefficients(coefficients, placement)
    }
}

/// Caller-selected logical units, not a physical PM/DM image or device mapping.
/// Input32, output32, coefficients5 and state2 (71 words) follow `dm_base`
/// adjacently; the one-past end must fit u32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighPassLogicalPlacement {
    pub origin: PmWordAddress24,
    pub dm_base: DmWordAddress32,
}

/// Decoded DM outputs and actual decoder counts. Instructions are not hardware cycles.
#[derive(Debug, Clone, PartialEq)]
pub struct EmittedHighPassBlock {
    pub outputs: [f32; 32],
    pub receipt: SimulationOutcome,
}

/// Fixed-32 offline raw-word high-pass. Owns the two accumulated future
/// contributions and an immutable kernel that privately binds exact coefficient
/// bits and checks them in DM before execution. No mutable code, scratch or
/// simulator state is exposed.
#[derive(Debug, Clone, PartialEq)]
pub struct EmittedV1HighPassReference {
    state: [f32; 2],
    compiled: CompiledHighPass,
}

impl EmittedV1HighPassReference {
    /// Starts from zero state. Arbitrary finite coefficients are reference policy.
    pub fn new(
        coefficients: V1HighPassCoefficients,
        placement: HighPassLogicalPlacement,
    ) -> Result<Self> {
        let starts = [0, 32, 64, 69].map(|offset| {
            placement
                .dm_base
                .checked_add(offset)
                .map(DmWordAddress32::get)
                .map_err(high_pass::Error::Address)
        });
        let [input, output, coefficient, state] = starts;
        let layout = high_pass::Layout::new(input?, output?, coefficient?, state?)?;
        Ok(Self {
            state: [0.0; 2],
            compiled: CompiledHighPass::compile(placement.origin, layout, coefficients)?,
        })
    }

    /// Immutable image, layout and bound coefficient bits for auditing.
    pub fn compiled(&self) -> &CompiledHighPass {
        &self.compiled
    }

    pub fn state(&self) -> [f32; 2] {
        self.state
    }

    pub fn reset(&mut self) {
        self.state = [0.0; 2];
    }

    /// Changed coefficient bits compile and stage a NEW kernel at the retained
    /// origin/layout; an immutable image never changes its bound bits. Identical
    /// bits keep the existing kernel. Any failure retains code, layout and state.
    pub fn reload(
        &mut self,
        coefficients: V1HighPassCoefficients,
        policy: HistoryUpdate,
    ) -> Result<()> {
        let [b0, b1, b2] = coefficients.feedforward;
        let [a1, a2] = coefficients.feedback;
        if [b0, b1, b2, a1, a2].map(f32::to_bits) != self.compiled.coefficient_bits() {
            self.compiled = CompiledHighPass::compile(
                self.compiled.code().origin(),
                self.compiled.layout(),
                coefficients,
            )?;
        }
        if policy == HistoryUpdate::Reset {
            self.reset();
        }
        Ok(())
    }

    /// Allocates the 71 adjacent DM words (input bits, zero output scratch,
    /// bound coefficient bits, state) and a fresh default ScalarState, then runs
    /// the public atomic kernel. Outputs and state decode from simulated DM and
    /// commit only after the whole block succeeds; every error retains the instance.
    pub fn process_block(
        &mut self,
        inputs: &[f32; 32],
        instruction_budget: usize,
    ) -> Result<EmittedHighPassBlock> {
        let (base, _) = self.compiled.layout().spans()[0];
        let mut words = Vec::with_capacity(71);
        words.extend(inputs.map(f32::to_bits));
        words.extend([0; 32]);
        words.extend(self.compiled.coefficient_bits());
        words.extend(self.state.map(f32::to_bits));
        let mut memory = DmMemory::new(base, words).map_err(high_pass::Error::Simulation)?;
        let mut scalar = ScalarState::default();
        let receipt =
            self.compiled
                .simulate_atomic(&mut scalar, &mut memory, instruction_budget)?;
        let words = memory.words();
        let outputs = std::array::from_fn(|index| f32::from_bits(words[32 + index]));
        self.state = [f32::from_bits(words[69]), f32::from_bits(words[70])];
        Ok(EmittedHighPassBlock { outputs, receipt })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::V1FirReference;
    use tetrad_sharc::{address::AddressError, fir::FirError, simulator::SimulationError};

    fn placement() -> FirLogicalPlacement {
        FirLogicalPlacement {
            origin: PmWordAddress24::new(0x1234).unwrap(),
            dm_base: DmWordAddress32::new(0xffff_0000),
        }
    }

    fn compare_callbacks<const N: usize>() {
        let mut taps = std::array::from_fn(|index| ((index % 13) as f32 - 6.0) / 128.0);
        taps[..3].copy_from_slice(&[1.0, 16_777_216.0, -16_777_216.0]);
        taps[3] = -0.0;
        taps[N - 1] = 0.75;
        let mut raw =
            EmittedV1FirReference::<N>::from_prepared_coefficients(taps, placement()).unwrap();
        let mut scalar = V1FirReference::from_prepared_coefficients(taps).unwrap();
        let image = raw.compiled().clone();
        // Reach the last tap and cross the scalar ring wrap with mixed inputs.
        for block in 0..N / 32 + 2 {
            let inputs = std::array::from_fn(|index| match (block * 32 + index) % 7 {
                0 => -0.0,
                1 => 0.0,
                2 => 1.0,
                _ => ((index % 9) as f32 - 4.0) / 8.0,
            });
            let raw_outputs = raw.process_block(&inputs, image.executed_steps()).unwrap();
            assert_eq!(
                raw_outputs.outputs.map(f32::to_bits),
                scalar.process_block(&inputs).unwrap().map(f32::to_bits)
            );
            assert_eq!(raw.compiled(), &image);
        }
        raw.reload_prepared([0.0; N], HistoryUpdate::Preserve)
            .unwrap();
        scalar
            .reload_prepared([0.0; N], HistoryUpdate::Preserve)
            .unwrap();
        assert_eq!(
            raw.process_block(&[-0.0; 32], image.executed_steps())
                .unwrap()
                .outputs
                .map(f32::to_bits),
            scalar.process_block(&[-0.0; 32]).unwrap().map(f32::to_bits)
        );
        let mut cancellation = [0.0; N];
        cancellation[..3].copy_from_slice(&[1.0, 16_777_216.0, -16_777_216.0]);
        raw.reload_prepared(cancellation, HistoryUpdate::Reset)
            .unwrap();
        assert_eq!(
            raw.process_block(&[1.0; 32], image.executed_steps())
                .unwrap()
                .outputs[2]
                .to_bits(),
            1.0f32.to_bits()
        );
    }

    #[test]
    fn both_lengths_bitwise_chronology_last_tap_cancellation_and_zero_products() {
        compare_callbacks::<512>();
        compare_callbacks::<2048>();
    }

    #[test]
    fn preparation_updates_reset_and_instance_isolation_retain_code() {
        let mut imported = [0.0; 512];
        imported[..2].copy_from_slice(&[3.0, 4.0]);
        let mut raw =
            EmittedV1FirReference::from_imported_prefilter(&imported, placement()).unwrap();
        assert_eq!(
            raw.coefficients.map(f32::to_bits),
            normalize_v1_prefilter_reference(&imported)
                .unwrap()
                .map(f32::to_bits)
        );
        let independent = raw.clone();
        let image = raw.compiled().clone();
        let mut input = [0.0; 32];
        input[31] = 3.0;
        raw.process_block(&input, image.executed_steps()).unwrap();
        assert_ne!(raw.history, independent.history);
        let mut taps = [0.0; 512];
        taps[1] = 2.0;
        raw.reload_prepared(taps, HistoryUpdate::Preserve).unwrap();
        assert_eq!(
            raw.process_block(&[0.0; 32], image.executed_steps())
                .unwrap()
                .outputs[0],
            6.0
        );
        raw.reload_imported_prefilter(&imported, HistoryUpdate::Reset)
            .unwrap();
        assert!(raw.history.iter().all(|value| value.to_bits() == 0));
        assert_eq!(raw.compiled(), &image);
        let before = raw.clone();
        imported[511] = f32::NAN;
        assert_eq!(
            raw.reload_imported_prefilter(&imported, HistoryUpdate::Reset),
            Err(Error::NonFinite)
        );
        assert_eq!(raw, before);
        imported[511] = f32::MAX;
        assert_eq!(
            raw.reload_imported_prefilter(&imported, HistoryUpdate::Reset),
            Err(Error::NumericOverflow)
        );
        assert_eq!(raw, before);
        let mut post = [0.0; 2048];
        post[..2].copy_from_slice(&[3.0, -0.0]);
        let mut raw_post = EmittedV1FirReference::from_postfilter(post, placement()).unwrap();
        assert_eq!(
            raw_post.coefficients.map(f32::to_bits),
            post.map(f32::to_bits)
        );
        let post_image = raw_post.compiled().clone();
        raw_post
            .process_block(&[1.0; 32], post_image.executed_steps())
            .unwrap();
        raw_post.reset();
        assert_eq!(raw_post.compiled(), &post_image);
        assert_eq!(
            raw_post.coefficients.map(f32::to_bits),
            post.map(f32::to_bits)
        );
        assert!(raw_post.history.iter().all(|value| value.to_bits() == 0));
    }

    #[test]
    fn checked_placement_nonfinite_and_late_arithmetic_errors_recover_complete_state() {
        assert!(matches!(
            EmittedV1FirReference::<3>::from_prepared_coefficients([0.0; 3], placement()),
            Err(Error::UnsupportedV1FirLength(3))
        ));
        let bad_pm = FirLogicalPlacement {
            origin: PmWordAddress24::new(0xff_ffff - 353).unwrap(),
            ..placement()
        };
        assert!(matches!(
            EmittedV1FirReference::<512>::from_prepared_coefficients([0.0; 512], bad_pm),
            Err(Error::Fir(FirError::Emit(
                tetrad_sharc::emit::EmitError::ProgramBounds { .. }
            )))
        ));
        let bad_dm = FirLogicalPlacement {
            dm_base: DmWordAddress32::new(u32::MAX - 1086),
            ..placement()
        };
        assert_eq!(
            EmittedV1FirReference::<512>::from_prepared_coefficients([0.0; 512], bad_dm),
            Err(Error::Fir(FirError::Address(AddressError::DmOverflow)))
        );
        let mut taps = [0.0; 512];
        taps[0] = 2.0;
        let mut raw = EmittedV1FirReference::from_prepared_coefficients(taps, placement()).unwrap();
        let budget = raw.compiled().executed_steps();
        raw.process_block(&[0.25; 32], budget).unwrap();
        let before = raw.clone();
        let mut bad_taps = taps;
        bad_taps[511] = f32::INFINITY;
        assert_eq!(
            raw.reload_prepared(bad_taps, HistoryUpdate::Reset),
            Err(Error::NonFinite)
        );
        for invalid in [f32::NAN, f32::INFINITY, f32::MAX] {
            let mut inputs = [0.25; 32];
            inputs[31] = invalid;
            let error = raw.process_block(&inputs, budget).unwrap_err();
            if invalid.is_finite() {
                assert!(matches!(
                    error,
                    Error::Fir(FirError::Simulation(
                        SimulationError::NonFiniteResult { .. }
                    ))
                ));
            } else {
                assert!(matches!(
                    error,
                    Error::Fir(FirError::NonFiniteOperand { .. })
                ));
            }
            assert_eq!(raw, before);
        }
        let expected = before.clone().process_block(&[0.5; 32], budget).unwrap();
        assert_eq!(raw.process_block(&[0.5; 32], budget).unwrap(), expected);
        let mut sum_taps = [0.0; 512];
        sum_taps[..3].copy_from_slice(&[-f32::MAX, f32::MAX, f32::MAX]);
        raw.reload_prepared(sum_taps, HistoryUpdate::Reset).unwrap();
        let before = raw.clone();
        assert!(matches!(
            raw.process_block(&[1.0; 32], budget),
            Err(Error::Fir(FirError::Simulation(
                SimulationError::NonFiniteResult { .. }
            )))
        ));
        assert_eq!(raw, before);
    }

    fn budget_rollback<const N: usize>(steps: usize) {
        let taps = std::array::from_fn(|index| (index % 7) as f32 / 64.0);
        let mut raw =
            EmittedV1FirReference::<N>::from_prepared_coefficients(taps, placement()).unwrap();
        assert_eq!(raw.compiled().code().words().len(), 355);
        assert_eq!(raw.compiled().executed_steps(), steps);
        assert_eq!(raw.compiled().layout().input_words() as usize, N + 31);
        raw.process_block(&[0.25; 32], steps).unwrap();
        let before = raw.clone();
        for budget in [0, steps / 2, steps - 1] {
            assert_eq!(
                raw.process_block(&[0.5; 32], budget),
                Err(Error::Fir(FirError::Simulation(
                    SimulationError::StepBudget(budget)
                )))
            );
            assert_eq!(raw, before);
        }
        let result = raw.process_block(&[0.5; 32], steps).unwrap();
        assert_eq!(result.receipt, SimulationOutcome { steps, stores: 32 });
        assert_eq!(
            result,
            before.clone().process_block(&[0.5; 32], steps).unwrap()
        );
        assert_eq!(raw.compiled(), before.compiled());
    }

    #[test]
    fn exact_receipts_and_budget_errors_after_stores_keep_history_for_both_lengths() {
        budget_rollback::<512>(98_467);
        budget_rollback::<2048>(393_379);
    }

    const HP_BASE: u32 = 0xffff_0000;

    fn hp_placement() -> HighPassLogicalPlacement {
        HighPassLogicalPlacement {
            origin: PmWordAddress24::new(0x1200).unwrap(),
            dm_base: DmWordAddress32::new(HP_BASE),
        }
    }

    fn asymmetric_hp() -> V1HighPassCoefficients {
        V1HighPassCoefficients {
            feedforward: [0.37, -0.219, 0.08],
            feedback: [-0.31, 0.11],
        }
    }

    #[test]
    fn owned_high_pass_matches_reference_across_blocks_and_binds_reloaded_code() {
        use crate::filters::V1HighPassReference;
        let mut raw = EmittedV1HighPassReference::new(asymmetric_hp(), hp_placement()).unwrap();
        // Exactly the accepted kernel over the derived adjacent 71-word layout.
        let layout =
            high_pass::Layout::new(HP_BASE, HP_BASE + 32, HP_BASE + 64, HP_BASE + 69).unwrap();
        let image =
            CompiledHighPass::compile(hp_placement().origin, layout, asymmetric_hp()).unwrap();
        assert_eq!(raw.compiled(), &image);
        assert_eq!(image.code().isa48_le_bytes().len(), 2544);
        let mut scalar = V1HighPassReference::new(asymmetric_hp()).unwrap();
        for block in 0..3 {
            let inputs = std::array::from_fn(|index| match (block * 32 + index) % 5 {
                0 => -0.0,
                1 => 1.0,
                _ => (index as f32 - 15.0) / 17.0,
            });
            let result = raw.process_block(&inputs, image.max_steps()).unwrap();
            assert_eq!(
                result.receipt,
                SimulationOutcome {
                    steps: 424,
                    stores: 96
                }
            );
            assert_eq!(
                result.outputs.map(f32::to_bits),
                scalar.process_block(&inputs).unwrap().map(f32::to_bits)
            );
            assert_eq!(
                raw.state().map(f32::to_bits),
                scalar.state().map(f32::to_bits)
            );
        }
        let state = raw.state();
        assert_ne!(state[0], state[1]);
        // Identical bits retain the kernel; Preserve retains both states.
        raw.reload(asymmetric_hp(), HistoryUpdate::Preserve)
            .unwrap();
        assert_eq!((raw.compiled(), raw.state()), (&image, state));
        // Changed bits stage a NEW bound kernel at the retained placement.
        let identity = V1HighPassCoefficients {
            feedforward: [1.0, 0.0, 0.0],
            feedback: [0.0; 2],
        };
        raw.reload(identity, HistoryUpdate::Preserve).unwrap();
        scalar.reload(identity, HistoryUpdate::Preserve).unwrap();
        assert_eq!(
            raw.compiled(),
            &CompiledHighPass::compile(image.code().origin(), image.layout(), identity).unwrap()
        );
        assert_ne!(raw.compiled(), &image);
        assert_eq!(raw.state(), state);
        let transient = raw.process_block(&[0.0; 32], 424).unwrap().outputs;
        assert_eq!(
            transient.map(f32::to_bits),
            scalar.process_block(&[0.0; 32]).unwrap().map(f32::to_bits)
        );
        assert_eq!(transient[..2], state); // Identity replays preserved history.
        raw.reload(asymmetric_hp(), HistoryUpdate::Reset).unwrap();
        let fresh = EmittedV1HighPassReference::new(asymmetric_hp(), hp_placement()).unwrap();
        assert_eq!(raw, fresh);
        raw.process_block(&[0.5; 32], 424).unwrap();
        raw.reset();
        assert_eq!(raw, fresh);
    }

    #[test]
    fn owned_high_pass_admission_and_failures_retain_code_coefficients_and_state() {
        let at = |origin: u32, dm_base: u32| HighPassLogicalPlacement {
            origin: PmWordAddress24::new(origin).unwrap(),
            dm_base: DmWordAddress32::new(dm_base),
        };
        // The last 424-word PM origin and 71-word representable DM end are admitted.
        assert!(
            EmittedV1HighPassReference::new(asymmetric_hp(), at(0xff_ffff - 423, u32::MAX - 71))
                .is_ok()
        );
        assert_eq!(
            EmittedV1HighPassReference::new(asymmetric_hp(), at(0, u32::MAX - 70)),
            Err(Error::HighPass(high_pass::Error::Address(
                AddressError::DmOverflow
            )))
        );
        assert!(matches!(
            EmittedV1HighPassReference::new(asymmetric_hp(), at(0xff_ffff - 422, 0)),
            Err(Error::HighPass(high_pass::Error::Emit(_)))
        ));
        let mut bad = asymmetric_hp();
        bad.feedback[1] = f32::NAN;
        assert_eq!(
            EmittedV1HighPassReference::new(bad, hp_placement()),
            Err(Error::HighPass(high_pass::Error::NonFiniteCoefficient {
                slot: 4
            }))
        );
        let mut raw = EmittedV1HighPassReference::new(asymmetric_hp(), hp_placement()).unwrap();
        raw.process_block(&[0.25; 32], 424).unwrap();
        let before = raw.clone();
        for policy in [HistoryUpdate::Preserve, HistoryUpdate::Reset] {
            assert!(raw.reload(bad, policy).is_err());
            assert_eq!(raw, before);
        }
        for budget in [0, 423] {
            assert_eq!(
                raw.process_block(&[0.5; 32], budget),
                Err(Error::HighPass(high_pass::Error::Simulation(
                    SimulationError::StepBudget(budget)
                )))
            );
            assert_eq!(raw, before);
        }
        for invalid in [f32::NAN, f32::NEG_INFINITY] {
            let mut inputs = [0.5; 32];
            inputs[31] = invalid;
            assert_eq!(
                raw.process_block(&inputs, 424),
                Err(Error::HighPass(high_pass::Error::NonFiniteMemory {
                    address: DmWordAddress32::new(HP_BASE + 31)
                }))
            );
            assert_eq!(raw, before);
        }
        let expected = before.clone().process_block(&[0.5; 32], 424).unwrap();
        assert_eq!(raw.process_block(&[0.5; 32], 424).unwrap(), expected);
        // Late final-state overflow has no following output store but still fails.
        raw.reload(
            V1HighPassCoefficients {
                feedforward: [0.0, 0.0, f32::MAX],
                feedback: [0.0; 2],
            },
            HistoryUpdate::Reset,
        )
        .unwrap();
        let before = raw.clone();
        let mut inputs = [0.0; 32];
        inputs[31] = 2.0;
        assert_eq!(
            raw.process_block(&inputs, 424),
            Err(Error::HighPass(high_pass::Error::NonFiniteMemory {
                address: DmWordAddress32::new(HP_BASE + 70)
            }))
        );
        assert_eq!(raw, before);
    }
}

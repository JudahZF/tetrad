//! Partial V1 path with optional owned input gain, recovered tones and output gain.
//! Tones are host-mathematical or owned emitted; without them the path omits three
//! stock stages before output gain.
//! Not complete capture audio, inference parity, realtime or a heap-free API.

pub mod emitted;

use crate::{
    Error, Result,
    filters::{
        EmittedV1FirReference, EmittedV1HighPassReference, FirLogicalPlacement,
        HighPassLogicalPlacement, HistoryUpdate, V1FirReference, V1HighPassCoefficients,
        V1HighPassReference, emitted::high_pass::CompiledHighPass,
        normalize_v1_prefilter_reference, v1_high_pass_coefficients_reference,
    },
    gain::GainAdapter,
    network::SerializedMathematicalGainsCore,
    resampling::{CallerOwnedV1ResamplingTable, V1ResamplingReference},
    tones::{
        V1ToneStackReference,
        emitted_stack::{EmittedV1ToneStackReference, V1ToneInstructionBudgets},
    },
    v1::RuntimeData,
};

/// Separate logical invocations may reuse placements; no shared physical image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V1FirPlacements {
    pub pre: FirLogicalPlacement,
    pub post: FirLogicalPlacement,
}

/// Executed instruction limits for the two raw-word FIR invocations, not cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V1FirInstructionBudgets {
    pub pre: usize,
    pub post: usize,
}

/// Logical placements for all four owned-word stages. Separate invocations may
/// reuse placements; there is no shared physical image or allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V1StagePlacements {
    pub input_hp: HighPassLogicalPlacement,
    pub pre: FirLogicalPlacement,
    pub post: FirLogicalPlacement,
    pub output_hp: HighPassLogicalPlacement,
}

/// Executed instruction limits for all four raw-word invocations, not cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V1StageInstructionBudgets {
    pub input_hp: usize,
    pub pre: usize,
    pub post: usize,
    pub output_hp: usize,
}

/// Input-gain limit plus the unchanged four-stage limits, not cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V1InputGainInstructionBudgets {
    pub input_gain: usize,
    pub stages: V1StageInstructionBudgets,
}

#[derive(Debug, Clone, PartialEq)]
enum HighPassStage {
    Mathematical(V1HighPassReference),
    Emitted(EmittedV1HighPassReference),
}

impl HighPassStage {
    fn reset(&mut self) {
        match self {
            Self::Mathematical(hp) => hp.reset(),
            Self::Emitted(hp) => hp.reset(),
        }
    }

    /// Emitted stages compile a new kernel at their placement if bits change.
    fn reload(&mut self, coefficients: V1HighPassCoefficients) -> Result<()> {
        match self {
            Self::Mathematical(hp) => hp.reload(coefficients, HistoryUpdate::Reset),
            Self::Emitted(hp) => hp.reload(coefficients, HistoryUpdate::Reset),
        }
    }

    fn process_block(&mut self, inputs: &[f32; 32], budget: Option<usize>) -> Result<[f32; 32]> {
        match self {
            Self::Mathematical(hp) => hp.process_block(inputs),
            Self::Emitted(hp) => {
                let budget = budget.unwrap_or_else(|| hp.compiled().max_steps());
                Ok(hp.process_block(inputs, budget)?.outputs)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum FirStage<const N: usize> {
    Mathematical(V1FirReference<N>),
    Emitted(EmittedV1FirReference<N>),
}

impl<const N: usize> FirStage<N> {
    fn reset(&mut self) {
        match self {
            Self::Mathematical(fir) => fir.reset(),
            Self::Emitted(fir) => fir.reset(),
        }
    }

    fn reload_prepared(&mut self, coefficients: [f32; N]) -> Result<()> {
        match self {
            Self::Mathematical(fir) => fir.reload_prepared(coefficients, HistoryUpdate::Reset),
            Self::Emitted(fir) => fir.reload_prepared(coefficients, HistoryUpdate::Reset),
        }
    }

    fn process_block(&mut self, inputs: &[f32; 32], budget: Option<usize>) -> Result<[f32; 32]> {
        match self {
            Self::Mathematical(fir) => fir.process_block(inputs),
            Self::Emitted(fir) => {
                let budget = budget.unwrap_or_else(|| fir.compiled().executed_steps());
                Ok(fir.process_block(inputs, budget)?.outputs)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum ToneStage {
    Mathematical(V1ToneStackReference),
    /// Boxed: the emitted stack is about 3.5 times the host stack's inline size.
    Emitted(Box<EmittedV1ToneStackReference>),
}

impl ToneStage {
    fn set_normalized(&mut self, normalized: [f32; 3]) -> Result<()> {
        match self {
            Self::Mathematical(tones) => tones.set_normalized(normalized),
            Self::Emitted(tones) => tones.set_normalized(normalized),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Mathematical(tones) => tones.reset(),
            Self::Emitted(tones) => tones.reset(),
        }
    }

    fn process_block(
        &mut self,
        inputs: &[f32; 32],
        budgets: Option<V1ToneInstructionBudgets>,
    ) -> Result<[f32; 32]> {
        match self {
            Self::Mathematical(tones) => tones.process_block(inputs),
            Self::Emitted(tones) => {
                let budgets = budgets.unwrap_or_else(|| tones.full_budgets());
                Ok(tones.process_block(inputs, budgets)?.outputs)
            }
        }
    }
}

/// Optional owned input gain → input HP → normalized pre FIR → interpolation →
/// serialized mathematical gains/core → decimation → unnormalized post FIR → output HP
/// → optional recovered tones → optional owned output gain. With tones and both gains
/// attached the stage ORDER is the recovered stock order; without tones it is not.
/// Alpha/beta use direct serialized gains, with existing stock log/exp caveat.
/// Each instance owns all state and an explicit caller-owned resampling table.
/// All four constructors omit output gain; the three original ones also omit input gain.
/// Gains may share logical addresses because each invocation has private DM; no shared
/// physical image, bank, concurrency, transaction or allocation identity is implied.
#[derive(Debug, Clone)]
pub struct V1PartialSignalPathReference {
    input_gain: Option<GainAdapter>,
    input_hp: HighPassStage,
    pre: FirStage<512>,
    resampler: V1ResamplingReference,
    neural: SerializedMathematicalGainsCore,
    post: FirStage<2048>,
    output_hp: HighPassStage,
    tones: Option<ToneStage>,
    output_gain: Option<GainAdapter>,
}

impl V1PartialSignalPathReference {
    /// Construction is the explicit direct-mathematical-gains opt-in. Validates
    /// all runtime values, neural shape/gain domain, normalized pre taps and HP.
    /// Table source/provenance is mandatory; serialized taps do not supply it.
    pub fn new(record: &RuntimeData<'_>, table: CallerOwnedV1ResamplingTable) -> Result<Self> {
        let neural = SerializedMathematicalGainsCore::new(record)?;
        let [frequency, bandwidth] = record.high_pass();
        let coefficients = v1_high_pass_coefficients_reference(frequency, bandwidth)?;
        let pre =
            std::array::from_fn(|index| record.pre_coefficients().get(index).expect("512 taps"));
        let post =
            std::array::from_fn(|index| record.post_coefficients().get(index).expect("2048 taps"));
        Ok(Self {
            input_gain: None,
            input_hp: HighPassStage::Mathematical(V1HighPassReference::new(coefficients)?),
            pre: FirStage::Mathematical(V1FirReference::from_imported_prefilter(&pre)?),
            resampler: V1ResamplingReference::new(table),
            neural,
            post: FirStage::Mathematical(V1FirReference::from_postfilter(post)?),
            output_hp: HighPassStage::Mathematical(V1HighPassReference::new(coefficients)?),
            tones: None,
            output_gain: None,
        })
    }

    /// Opts only the FIR stages into immutable owned-word offline execution.
    /// Keeps direct mathematical gains, caller-owned table and partial-path scope.
    pub fn new_emitted(
        record: &RuntimeData<'_>,
        table: CallerOwnedV1ResamplingTable,
        placements: V1FirPlacements,
    ) -> Result<Self> {
        let mut path = Self::new(record, table)?;
        let pre =
            std::array::from_fn(|index| record.pre_coefficients().get(index).expect("512 taps"));
        let post =
            std::array::from_fn(|index| record.post_coefficients().get(index).expect("2048 taps"));
        path.pre = FirStage::Emitted(EmittedV1FirReference::from_imported_prefilter(
            &pre,
            placements.pre,
        )?);
        path.post = FirStage::Emitted(EmittedV1FirReference::from_postfilter(
            post,
            placements.post,
        )?);
        Ok(path)
    }

    /// Opts BOTH FIR and BOTH high-pass stages into immutable owned-word offline
    /// execution. Resampling, neural gains/core and partial-path scope are unchanged.
    pub fn new_emitted_stages(
        record: &RuntimeData<'_>,
        table: CallerOwnedV1ResamplingTable,
        placements: V1StagePlacements,
    ) -> Result<Self> {
        let fir = V1FirPlacements {
            pre: placements.pre,
            post: placements.post,
        };
        let mut path = Self::new_emitted(record, table, fir)?;
        let [frequency, bandwidth] = record.high_pass();
        let coefficients = v1_high_pass_coefficients_reference(frequency, bandwidth)?;
        path.input_hp = HighPassStage::Emitted(EmittedV1HighPassReference::new(
            coefficients,
            placements.input_hp,
        )?);
        path.output_hp = HighPassStage::Emitted(EmittedV1HighPassReference::new(
            coefficients,
            placements.output_hp,
        )?);
        Ok(path)
    }

    /// Opts into owned input gain before all four emitted filter stages. The caller's
    /// validated adapter supplies current/target and immutable logical code/placement;
    /// neither unity nor serialized alpha/beta supplies an external gain seed.
    pub fn new_emitted_stages_with_input_gain(
        record: &RuntimeData<'_>,
        table: CallerOwnedV1ResamplingTable,
        placements: V1StagePlacements,
        gain: GainAdapter,
    ) -> Result<Self> {
        let mut path = Self::new_emitted_stages(record, table, placements)?;
        path.input_gain = Some(gain);
        Ok(path)
    }

    /// Inspect external gain state and its immutable emitted backend, if configured.
    pub fn input_gain(&self) -> Option<&GainAdapter> {
        self.input_gain.as_ref()
    }

    /// Validated target update. Code, placement and selection stay fixed per instance.
    pub fn set_input_gain_target(&mut self, target: f32) -> Result<()> {
        match &mut self.input_gain {
            Some(gain) => gain.set_linear_target(target),
            None => Err(Error::InputGainNotConfigured),
        }
    }

    /// Explicit validated caller reseed, not a stock reset rule. Code and placement stay.
    pub fn reseed_input_gain(&mut self, current: f32, target: f32) -> Result<()> {
        match &mut self.input_gain {
            Some(gain) => gain.reseed(current, target),
            None => Err(Error::InputGainNotConfigured),
        }
    }

    /// Attach the host-mathematical tone stack after output HP and before any output
    /// gain. Refuses replacement by either variant and consumes the path even on
    /// failure. Parameters are the caller's normalized values; stock service
    /// ramps/publication are not modeled.
    pub fn with_tones(self, tones: V1ToneStackReference) -> Result<Self> {
        self.attach_tones(ToneStage::Mathematical(tones))
    }

    /// Attach the owned emitted tone stack at the same position. Same refusal and
    /// consuming rules as `with_tones`; no conversion between variants.
    pub fn with_emitted_tones(self, tones: EmittedV1ToneStackReference) -> Result<Self> {
        self.attach_tones(ToneStage::Emitted(Box::new(tones)))
    }

    fn attach_tones(mut self, tones: ToneStage) -> Result<Self> {
        if self.tones.is_some() {
            return Err(Error::TonesAlreadyConfigured);
        }
        self.tones = Some(tones);
        Ok(self)
    }

    /// The host-mathematical stack only; `None` when emitted tones are attached.
    pub fn tones(&self) -> Option<&V1ToneStackReference> {
        match &self.tones {
            Some(ToneStage::Mathematical(tones)) => Some(tones),
            _ => None,
        }
    }

    /// The owned emitted stack only; `None` when mathematical tones are attached.
    pub fn emitted_tones(&self) -> Option<&EmittedV1ToneStackReference> {
        match &self.tones {
            Some(ToneStage::Emitted(tones)) => Some(tones.as_ref()),
            _ => None,
        }
    }

    /// Block-boundary tone update as at updater `0x178` for either variant: all three
    /// or none; histories, emitted code and placements stay.
    pub fn set_tone_parameters(&mut self, normalized: [f32; 3]) -> Result<()> {
        match &mut self.tones {
            Some(tones) => tones.set_normalized(normalized),
            None => Err(Error::TonesNotConfigured),
        }
    }

    /// Attach validated caller-seeded output gain last, after output HP and any tones.
    /// Without tones this is not the recovered stock position. Refuses replacement. This
    /// consumes the path even on failure; the error does not return the moved path.
    pub fn with_output_gain(mut self, gain: GainAdapter) -> Result<Self> {
        if self.output_gain.is_some() {
            return Err(Error::OutputGainAlreadyConfigured);
        }
        self.output_gain = Some(gain);
        Ok(self)
    }

    /// Inspect output gain and its immutable emitted backend, if attached.
    pub fn output_gain(&self) -> Option<&GainAdapter> {
        self.output_gain.as_ref()
    }

    /// Validated linear target update; current, code and placement are retained.
    pub fn set_output_gain_target(&mut self, target: f32) -> Result<()> {
        match &mut self.output_gain {
            Some(gain) => gain.set_linear_target(target),
            None => Err(Error::OutputGainNotConfigured),
        }
    }

    /// Explicit validated caller reseed, not a stock reset rule. Code and placement stay.
    pub fn reseed_output_gain(&mut self, current: f32, target: f32) -> Result<()> {
        match &mut self.output_gain {
            Some(gain) => gain.reseed(current, target),
            None => Err(Error::OutputGainNotConfigured),
        }
    }

    /// Both immutable FIR images when this path selected emitted execution.
    pub fn emitted_firs(
        &self,
    ) -> Option<(
        &tetrad_sharc::fir::CompiledFir,
        &tetrad_sharc::fir::CompiledFir,
    )> {
        match (&self.pre, &self.post) {
            (FirStage::Emitted(pre), FirStage::Emitted(post)) => {
                Some((pre.compiled(), post.compiled()))
            }
            _ => None,
        }
    }

    /// Input and output immutable high-pass images when both selected emitted execution.
    pub fn emitted_high_passes(&self) -> Option<(&CompiledHighPass, &CompiledHighPass)> {
        match (&self.input_hp, &self.output_hp) {
            (HighPassStage::Emitted(input), HighPassStage::Emitted(output)) => {
                Some((input.compiled(), output.compiled()))
            }
            _ => None,
        }
    }

    pub fn table(&self) -> &CallerOwnedV1ResamplingTable {
        self.resampler.table()
    }

    /// Zero all filter histories/cursors and restore neural seeds.
    /// Coefficients, gains and explicit table/provenance are retained atomically.
    /// Both external gains' current/target/code/placement are retained, not reseeded.
    /// Tone histories zero and tone parameters, variant, emitted code and placements
    /// stay, as reset `0x492` does. The rest is owned policy, not a stock reset rule.
    pub fn reset(&mut self) -> Result<()> {
        let mut next = self.clone();
        next.neural.reset()?;
        next.input_hp.reset();
        next.pre.reset();
        next.resampler.reset();
        next.post.reset();
        next.output_hp.reset();
        if let Some(tones) = &mut next.tones {
            tones.reset();
        }
        *self = next;
        Ok(())
    }

    /// Atomic full-context replacement with RESET histories. The table must be
    /// supplied again; preserving old histories/context on successful reload is
    /// intentionally unsupported. Failure preserves the whole previous path.
    /// Both external gains' transient current/target/code/placement are retained;
    /// explicit caller reseeding is separate, not a recovered stock reload policy.
    /// Tone parameters, variant, emitted code and placements are retained with reset
    /// histories (owned policy).
    /// Backend selection and logical placements are retained. FIR code is retained;
    /// owned high-pass code is recompiled at its placement only if bound bits change.
    pub fn reload(
        &mut self,
        record: &RuntimeData<'_>,
        table: CallerOwnedV1ResamplingTable,
    ) -> Result<()> {
        let mut next = Self::new(record, table)?;
        // Retain selected backend, immutable code and logical placement.
        let pre =
            std::array::from_fn(|index| record.pre_coefficients().get(index).expect("512 taps"));
        let post =
            std::array::from_fn(|index| record.post_coefficients().get(index).expect("2048 taps"));
        let [frequency, bandwidth] = record.high_pass();
        let coefficients = v1_high_pass_coefficients_reference(frequency, bandwidth)?;
        next.input_gain = self.input_gain.clone();
        next.output_gain = self.output_gain.clone();
        next.tones = self.tones.clone();
        if let Some(tones) = &mut next.tones {
            tones.reset();
        }
        next.input_hp = self.input_hp.clone();
        next.pre = self.pre.clone();
        next.post = self.post.clone();
        next.output_hp = self.output_hp.clone();
        next.input_hp.reload(coefficients)?;
        next.pre
            .reload_prepared(normalize_v1_prefilter_reference(&pre)?)?;
        next.post.reload_prepared(post)?;
        next.output_hp.reload(coefficients)?;
        *self = next;
        Ok(())
    }

    /// Exactly 32 inputs/outputs; any stage failure preserves ALL state, including
    /// both gains' bits/code/placement, neural histories, FIR cursors and resampler/HP
    /// histories. Returned output exists only on success. Clones O(512+2048) arrays,
    /// small neural states and owned images; String provenance copies allocate.
    /// Inner atomic APIs also clone/work. Emitted stages allocate DM and use exact
    /// full instruction budgets: 230 for each configured gain, 366 per emitted tone.
    /// Allocation/OOM is outside Result rollback; this is not a realtime guarantee.
    pub fn process_block(&mut self, inputs: &[f32; 32]) -> Result<[f32; 32]> {
        self.process_block_inner(inputs, None, [None; 4], None, None)
    }

    /// Output-gain raw instruction limit, not cycles. Requires attached output gain
    /// before cloning; mathematical and emitted filter backends are both supported.
    /// Earlier stages use normal full budgets, including 230 for configured input gain.
    /// A late output-gain failure retains the complete path.
    pub fn process_block_with_output_gain_budget(
        &mut self,
        inputs: &[f32; 32],
        output_gain: usize,
    ) -> Result<[f32; 32]> {
        if self.output_gain.is_none() {
            return Err(Error::OutputGainNotConfigured);
        }
        self.process_block_inner(inputs, None, [None; 4], None, Some(output_gain))
    }

    /// Explicit raw instruction limits; refuses a mathematical-default path.
    /// Owned high-pass stages and both configured gains keep exact full budgets.
    /// A late post-FIR failure rolls back every stage and table provenance.
    pub fn process_block_with_fir_budgets(
        &mut self,
        inputs: &[f32; 32],
        budgets: V1FirInstructionBudgets,
    ) -> Result<[f32; 32]> {
        if self.emitted_firs().is_none() {
            return Err(Error::FirBudgetsRequireEmitted);
        }
        self.process_block_inner(
            inputs,
            None,
            [None, Some(budgets.pre), Some(budgets.post), None],
            None,
            None,
        )
    }

    /// Explicit raw instruction limits for all four stages; requires all four
    /// emitted. Both configured gains keep their full 230-instruction budgets.
    /// A late output high-pass failure rolls back every stage and table.
    pub fn process_block_with_stage_budgets(
        &mut self,
        inputs: &[f32; 32],
        budgets: V1StageInstructionBudgets,
    ) -> Result<[f32; 32]> {
        if self.emitted_firs().is_none() || self.emitted_high_passes().is_none() {
            return Err(Error::StageBudgetsRequireEmitted);
        }
        self.process_block_inner(
            inputs,
            None,
            [
                budgets.input_hp,
                budgets.pre,
                budgets.post,
                budgets.output_hp,
            ]
            .map(Some),
            None,
            None,
        )
    }

    /// Explicit input-gain and four-stage raw instruction limits. Refuses missing
    /// gain or any mathematical filter stage before doing work. Every Result failure
    /// preserves the whole path, including a gain ramp advanced on the staged clone.
    /// Configured output gain keeps its full 230-instruction budget.
    pub fn process_block_with_input_gain_budgets(
        &mut self,
        inputs: &[f32; 32],
        budgets: V1InputGainInstructionBudgets,
    ) -> Result<[f32; 32]> {
        if self.input_gain.is_none()
            || self.emitted_firs().is_none()
            || self.emitted_high_passes().is_none()
        {
            return Err(Error::InputGainBudgetsRequireEmitted);
        }
        let stages = budgets.stages;
        self.process_block_inner(
            inputs,
            Some(budgets.input_gain),
            [stages.input_hp, stages.pre, stages.post, stages.output_hp].map(Some),
            None,
            None,
        )
    }

    /// Explicit raw instruction limits for the three emitted tone leaves. Refuses a
    /// path without emitted tones before cloning or work; other stages may use either
    /// backend and keep their full budgets. A late tone failure retains the whole path.
    pub fn process_block_with_tone_budgets(
        &mut self,
        inputs: &[f32; 32],
        budgets: V1ToneInstructionBudgets,
    ) -> Result<[f32; 32]> {
        if self.emitted_tones().is_none() {
            return Err(Error::ToneBudgetsRequireEmitted);
        }
        self.process_block_inner(inputs, None, [None; 4], Some(budgets), None)
    }

    /// Optional input gain first, then filters, optional tones, optional output gain.
    fn process_block_inner(
        &mut self,
        inputs: &[f32; 32],
        input_gain_budget: Option<usize>,
        budgets: [Option<usize>; 4],
        tone_budgets: Option<V1ToneInstructionBudgets>,
        output_gain_budget: Option<usize>,
    ) -> Result<[f32; 32]> {
        let [input_hp_budget, pre_budget, post_budget, output_hp_budget] = budgets;
        let mut next = self.clone();
        let gained;
        let inputs = if let Some(gain) = &mut next.input_gain {
            let budget = input_gain_budget.unwrap_or_else(|| gain.compiled().max_steps());
            gained = gain.process_block(inputs, budget)?.outputs;
            &gained
        } else {
            inputs
        };
        let input_hp = next.input_hp.process_block(inputs, input_hp_budget)?;
        let pre = next.pre.process_block(&input_hp, pre_budget)?;
        let interpolated = next.resampler.interpolate_block(&pre)?;
        let mut neural = [0.0; 64];
        next.neural.process_chunk(&interpolated, &mut neural)?;
        let decimated = next.resampler.decimate_block(&neural)?;
        let post = next.post.process_block(&decimated, post_budget)?;
        let mut outputs = next.output_hp.process_block(&post, output_hp_budget)?;
        if let Some(tones) = &mut next.tones {
            outputs = tones.process_block(&outputs, tone_budgets)?;
        }
        if let Some(gain) = &mut next.output_gain {
            let budget = output_gain_budget.unwrap_or_else(|| gain.compiled().max_steps());
            outputs = gain.process_block(&outputs, budget)?.outputs;
        }
        *self = next;
        Ok(outputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Error, v1::Limits};

    fn put(bytes: &mut [u8], offset: usize, value: f32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn record_bytes() -> Vec<u8> {
        let mut bytes = vec![0; 0x2864];
        for (offset, word) in [
            (4, 512_u32),
            (8, 2048),
            (0x2814, 3),
            (0x2818, 1),
            (0x281c, 2),
            (0x2820, 2),
            (0x2824, 1),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        for (offset, value) in [
            (0, 48_000.0),
            (0xc, 3.0),
            (0x10, 4.0),
            (0x80c, 2.0),
            (0x810, -0.5),
            (0x280c, 1.5),
            (0x2810, 1.75),
            (0x285c, 0.0),
            (0x2860, 1.0),
        ] {
            put(&mut bytes, offset, value);
        }
        for (index, value) in [
            0.5, -0.25, 0.25, -0.5, 0.75, -0.5, 0.25, 0.625, 0.125, -0.0625, 0.5, -0.375, 0.125,
        ]
        .into_iter()
        .enumerate()
        {
            put(&mut bytes, 0x2828 + 4 * index, value);
        }
        bytes
    }

    fn table(t0: f32, provenance: &str) -> CallerOwnedV1ResamplingTable {
        let mut coefficients = [0.0; 16];
        coefficients[0] = t0;
        CallerOwnedV1ResamplingTable::new(coefficients, provenance.into()).unwrap()
    }

    fn assert_same_state(
        actual: &V1PartialSignalPathReference,
        expected: &V1PartialSignalPathReference,
    ) {
        for (actual, expected) in [
            (actual.input_gain(), expected.input_gain()),
            (actual.output_gain(), expected.output_gain()),
        ] {
            match (actual, expected) {
                (Some(actual), Some(expected)) => {
                    assert_eq!(actual.current().to_bits(), expected.current().to_bits());
                    assert_eq!(actual.target().to_bits(), expected.target().to_bits());
                    assert_eq!(actual.compiled(), expected.compiled());
                }
                (None, None) => {}
                _ => panic!("gain selection differs"),
            }
        }
        assert_eq!(actual.input_hp, expected.input_hp);
        assert_eq!(actual.pre, expected.pre);
        assert_eq!(actual.resampler, expected.resampler);
        assert_eq!(actual.post, expected.post);
        assert_eq!(actual.output_hp, expected.output_hp);
        assert_eq!(actual.tones, expected.tones);
        // Debug includes every private neural tensor, history, gain and baseline.
        assert_eq!(
            format!("{:?}", actual.neural),
            format!("{:?}", expected.neural)
        );
        let mut actual = actual.neural.clone();
        let mut expected = expected.neural.clone();
        for input in [0.0, 0.25, -0.5, 1.0, 0.01] {
            assert_eq!(
                actual.process(input).unwrap().to_bits(),
                expected.process(input).unwrap().to_bits()
            );
        }
    }

    #[test]
    fn composed_owned_oracle_callback_shape_reset_and_isolated_instances() {
        let bytes = record_bytes();
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let source = "Owned synthetic T0 discriminator; not stock";
        let mut path = V1PartialSignalPathReference::new(&record, table(1.0, source)).unwrap();
        let mut independent =
            V1PartialSignalPathReference::new(&record, table(1.0, source)).unwrap();
        let mut inputs = [0.0; 96];
        inputs[0] = 0.25;
        inputs[31] = -0.5;
        inputs[32] = 0.5;
        let mut outputs = Vec::new();
        for block in inputs.chunks_exact(32) {
            outputs.extend(path.process_block(block.try_into().unwrap()).unwrap());
        }
        // Independent continuous-time Python float32 oracle, updated to the
        // recovered bias-first neural association. See capture-neural-association.md.
        // No stock table/audio fixture.
        for (sample, bits) in [
            (0, 0x3cbb_2f80),
            (1, 0x3dba_2068),
            (2, 0x3d83_4968),
            (15, 0),
            (16, 0x3e0a_bad0),
            (31, 0xbd00_88c0),
            (32, 0xbda5_3308),
            (33, 0x3ded_f4b8),
            (47, 0xbeaa_f098),
            (48, 0xbe04_8b14),
            (62, 0xbd5e_2080),
            (63, 0xbe2f_a9b8),
            (64, 0x3d55_56a0),
            (65, 0x3e23_68ba),
            (95, 0),
        ] {
            assert_eq!(outputs[sample].to_bits(), bits, "sample {sample}");
        }
        assert_eq!(
            independent
                .process_block(inputs[..32].try_into().unwrap())
                .unwrap()
                .as_slice(),
            &outputs[..32]
        );
        path.reset().unwrap();
        assert_eq!(path.table().provenance(), source);
        for (block, expected) in inputs.chunks_exact(32).zip(outputs.chunks_exact(32)) {
            assert_eq!(
                path.process_block(block.try_into().unwrap())
                    .unwrap()
                    .as_slice(),
                expected
            );
        }
    }

    #[test]
    fn whole_block_late_postfilter_failure_rolls_back_all_histories() {
        let mut bytes = record_bytes();
        put(&mut bytes, 0x80c, f32::MAX);
        put(&mut bytes, 0x2810, 100.0);
        put(&mut bytes, 0x285c, 1200.0);
        put(&mut bytes, 0x2860, 2.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut path =
            V1PartialSignalPathReference::new(&record, table(1.0, "Owned overflow probe")).unwrap();
        let before = path.clone();
        let inputs = [1.0; 32];
        // Confirm this error is after all neural/resampler processing succeeds.
        let mut staged = path.clone();
        let hp = staged.input_hp.process_block(&inputs, None).unwrap();
        let pre = staged.pre.process_block(&hp, None).unwrap();
        let interpolated = staged.resampler.interpolate_block(&pre).unwrap();
        let mut neural = [0.0; 64];
        staged
            .neural
            .process_chunk(&interpolated, &mut neural)
            .unwrap();
        let decimated = staged.resampler.decimate_block(&neural).unwrap();
        assert_eq!(
            staged.post.process_block(&decimated, None),
            Err(Error::NumericOverflow)
        );
        assert_eq!(path.process_block(&inputs), Err(Error::NumericOverflow));
        assert_same_state(&path, &before);
        let mut invalid = [0.0; 32];
        invalid[31] = f32::NAN;
        assert_eq!(path.process_block(&invalid), Err(Error::NonFinite));
        assert_same_state(&path, &before);
    }

    #[test]
    fn full_context_reload_requires_explicit_table_resets_or_preserves_on_error() {
        let bytes = record_bytes();
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut path =
            V1PartialSignalPathReference::new(&record, table(1.0, "Owned table A")).unwrap();
        path.process_block(&[0.25; 32]).unwrap();
        let before = path.clone();
        let mut bad_bytes = bytes.clone();
        put(&mut bad_bytes, 0x80c, f32::NAN);
        let bad = RuntimeData::parse(&bad_bytes, Limits::default()).unwrap();
        assert_eq!(
            path.reload(&bad, table(0.5, "Owned table B")),
            Err(Error::NonFinite)
        );
        assert_same_state(&path, &before);
        path.reload(&record, table(0.5, "Owned table B")).unwrap();
        let mut fresh =
            V1PartialSignalPathReference::new(&record, table(0.5, "Owned table B")).unwrap();
        assert_same_state(&path, &fresh);
        assert_eq!(path.table().provenance(), "Owned table B");
        assert_eq!(
            path.process_block(&[0.25; 32]).unwrap(),
            fresh.process_block(&[0.25; 32]).unwrap()
        );
    }

    fn placements() -> V1FirPlacements {
        use tetrad_sharc::address::{DmWordAddress32, PmWordAddress24};
        let logical = FirLogicalPlacement {
            origin: PmWordAddress24::new(0x2000).unwrap(),
            dm_base: DmWordAddress32::new(0xffff_0000),
        };
        // Separate invocation memories allow identical logical placements.
        V1FirPlacements {
            pre: logical,
            post: logical,
        }
    }

    fn budgets() -> V1FirInstructionBudgets {
        V1FirInstructionBudgets {
            pre: 98_467,
            post: 393_379,
        }
    }

    #[test]
    fn emitted_partial_path_matches_default_and_preserves_backend_on_reset_reload() {
        let bytes = record_bytes();
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut raw = V1PartialSignalPathReference::new_emitted(
            &record,
            table(1.0, "Owned emitted A"),
            placements(),
        )
        .unwrap();
        let initial = raw.clone();
        let (pre, post) = raw.emitted_firs().unwrap();
        let images = (pre.clone(), post.clone());
        let mut mathematical =
            V1PartialSignalPathReference::new(&record, table(1.0, "Owned emitted A")).unwrap();
        let math_before = mathematical.clone();
        assert_eq!(
            mathematical.process_block_with_fir_budgets(&[0.0; 32], budgets()),
            Err(Error::FirBudgetsRequireEmitted)
        );
        assert_same_state(&mathematical, &math_before);
        for block in 0..4 {
            let inputs =
                std::array::from_fn(|index| ((index * 3 + block) % 17) as f32 / 32.0 - 0.25);
            let actual = if block % 2 == 0 {
                raw.process_block(&inputs)
            } else {
                raw.process_block_with_fir_budgets(&inputs, budgets())
            }
            .unwrap();
            assert_eq!(
                actual.map(f32::to_bits),
                mathematical
                    .process_block(&inputs)
                    .unwrap()
                    .map(f32::to_bits)
            );
        }
        raw.reset().unwrap();
        assert_same_state(&raw, &initial);
        let mut replacement = bytes.clone();
        put(&mut replacement, 0x80c, -0.75);
        let replacement_record = RuntimeData::parse(&replacement, Limits::default()).unwrap();
        raw.reload(&replacement_record, table(0.5, "Owned emitted B"))
            .unwrap();
        let fresh = V1PartialSignalPathReference::new_emitted(
            &replacement_record,
            table(0.5, "Owned emitted B"),
            placements(),
        )
        .unwrap();
        assert_same_state(&raw, &fresh);
        let (pre, post) = raw.emitted_firs().unwrap();
        assert_eq!((pre, post), (&images.0, &images.1));
        assert_eq!(raw.table().provenance(), "Owned emitted B");
        raw.process_block(&[0.25; 32]).unwrap();
        let before = raw.clone();
        for (offset, invalid) in [(0x80c, f32::NAN), (0xc, f32::MAX), (0x2810, 0.0)] {
            let mut bad_bytes = bytes.clone();
            put(&mut bad_bytes, offset, invalid);
            let bad = RuntimeData::parse(&bad_bytes, Limits::default()).unwrap();
            assert!(raw.reload(&bad, table(0.25, "Rejected table")).is_err());
            assert_same_state(&raw, &before);
        }
        raw.reset().unwrap();
        assert_same_state(&raw, &fresh);
    }

    #[test]
    fn emitted_pre_and_late_post_errors_roll_back_complete_partial_path() {
        use tetrad_sharc::{fir::FirError, simulator::SimulationError};
        let bytes = record_bytes();
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut raw = V1PartialSignalPathReference::new_emitted(
            &record,
            table(1.0, "Owned rollback context"),
            placements(),
        )
        .unwrap();
        raw.process_block(&[0.25; 32]).unwrap();
        let before = raw.clone();
        for limits in [
            V1FirInstructionBudgets {
                pre: budgets().pre - 1,
                ..budgets()
            },
            V1FirInstructionBudgets {
                post: budgets().post - 1,
                ..budgets()
            },
        ] {
            assert!(matches!(
                raw.process_block_with_fir_budgets(&[0.5; 32], limits),
                Err(Error::Fir(FirError::Simulation(
                    SimulationError::StepBudget(_)
                )))
            ));
            assert_same_state(&raw, &before);
        }
        let expected = before.clone().process_block(&[0.5; 32]).unwrap();
        assert_eq!(
            raw.process_block(&[0.5; 32]).unwrap().map(f32::to_bits),
            expected.map(f32::to_bits)
        );
        let mut overflow_bytes = bytes.clone();
        put(&mut overflow_bytes, 0x80c, f32::MAX);
        put(&mut overflow_bytes, 0x2810, 100.0);
        put(&mut overflow_bytes, 0x285c, 1200.0);
        put(&mut overflow_bytes, 0x2860, 2.0);
        let overflow_record = RuntimeData::parse(&overflow_bytes, Limits::default()).unwrap();
        raw.reload(&overflow_record, table(1.0, "Owned overflow context"))
            .unwrap();
        let before = raw.clone();
        let mut staged = raw.clone();
        let hp = staged.input_hp.process_block(&[1.0; 32], None).unwrap();
        let pre = staged.pre.process_block(&hp, None).unwrap();
        let interpolated = staged.resampler.interpolate_block(&pre).unwrap();
        let mut neural = [0.0; 64];
        staged
            .neural
            .process_chunk(&interpolated, &mut neural)
            .unwrap();
        let decimated = staged.resampler.decimate_block(&neural).unwrap();
        assert!(matches!(
            staged.post.process_block(&decimated, None),
            Err(Error::Fir(FirError::Simulation(
                SimulationError::NonFiniteResult { .. }
            )))
        ));
        assert!(matches!(
            raw.process_block(&[1.0; 32]),
            Err(Error::Fir(FirError::Simulation(
                SimulationError::NonFiniteResult { .. }
            )))
        ));
        assert_same_state(&raw, &before);
    }

    fn stage_placements() -> V1StagePlacements {
        use tetrad_sharc::address::{DmWordAddress32, PmWordAddress24};
        let fir = placements();
        let hp = HighPassLogicalPlacement {
            origin: PmWordAddress24::new(0x3000).unwrap(),
            dm_base: DmWordAddress32::new(0xffff_0000),
        };
        V1StagePlacements {
            input_hp: hp,
            pre: fir.pre,
            post: fir.post,
            output_hp: hp,
        }
    }

    fn stage_budgets() -> V1StageInstructionBudgets {
        V1StageInstructionBudgets {
            input_hp: 424,
            pre: budgets().pre,
            post: budgets().post,
            output_hp: 424,
        }
    }

    /// Active, asymmetric high-pass coefficients rather than the identity default.
    fn active_high_pass_record(frequency: f32) -> Vec<u8> {
        let mut bytes = record_bytes();
        put(&mut bytes, 0x285c, frequency);
        put(&mut bytes, 0x2860, 2.0);
        bytes
    }

    fn high_pass_state(stage: &HighPassStage) -> [u32; 2] {
        match stage {
            HighPassStage::Mathematical(hp) => hp.state(),
            HighPassStage::Emitted(hp) => hp.state(),
        }
        .map(f32::to_bits)
    }

    #[test]
    fn owned_four_stage_path_matches_mathematical_and_fir_only_paths_across_reset_reload() {
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut owned = V1PartialSignalPathReference::new_emitted_stages(
            &record,
            table(1.0, "Owned stages A"),
            stage_placements(),
        )
        .unwrap();
        let initial = owned.clone();
        let mut fir_only = V1PartialSignalPathReference::new_emitted(
            &record,
            table(1.0, "Owned stages A"),
            placements(),
        )
        .unwrap();
        let mut mathematical =
            V1PartialSignalPathReference::new(&record, table(1.0, "Owned stages A")).unwrap();
        assert!(fir_only.emitted_high_passes().is_none());
        assert!(mathematical.emitted_high_passes().is_none());
        let (input_hp, output_hp) = owned.emitted_high_passes().unwrap();
        let hp_images = (input_hp.clone(), output_hp.clone());
        let fir_images = owned
            .emitted_firs()
            .map(|(pre, post)| (pre.clone(), post.clone()))
            .unwrap();
        let [frequency, bandwidth] = record.high_pass();
        let coefficients = v1_high_pass_coefficients_reference(frequency, bandwidth).unwrap();
        assert_ne!(coefficients.feedforward[0], 1.0);
        let [b0, b1, b2] = coefficients.feedforward;
        let [a1, a2] = coefficients.feedback;
        let bits = [b0, b1, b2, a1, a2].map(f32::to_bits);
        assert_eq!(hp_images.0.coefficient_bits(), bits);
        assert_eq!(hp_images.1.coefficient_bits(), bits);
        for block in 0..4 {
            let inputs =
                std::array::from_fn(|index| ((index * 3 + block) % 17) as f32 / 32.0 - 0.25);
            let expected = mathematical
                .process_block(&inputs)
                .unwrap()
                .map(f32::to_bits);
            assert_eq!(
                fir_only.process_block(&inputs).unwrap().map(f32::to_bits),
                expected
            );
            let actual = match block {
                1 => owned.process_block_with_stage_budgets(&inputs, stage_budgets()),
                3 => owned.process_block_with_fir_budgets(&inputs, budgets()),
                _ => owned.process_block(&inputs),
            };
            assert_eq!(actual.unwrap().map(f32::to_bits), expected);
        }
        for (owned_stage, math_stage) in [
            (&owned.input_hp, &mathematical.input_hp),
            (&owned.output_hp, &mathematical.output_hp),
        ] {
            assert_eq!(high_pass_state(owned_stage), high_pass_state(math_stage));
            assert_ne!(high_pass_state(owned_stage), [0; 2]);
        }
        owned.reset().unwrap();
        assert_same_state(&owned, &initial);
        // Unchanged high-pass bits retain every image.
        owned.reload(&record, table(0.5, "Owned stages B")).unwrap();
        assert_eq!(
            owned.emitted_high_passes().unwrap(),
            (&hp_images.0, &hp_images.1)
        );
        // Changed bound bits compile new kernels at the retained placements.
        let mut replacement = active_high_pass_record(300.0);
        put(&mut replacement, 0x80c, -0.75);
        let replacement_record = RuntimeData::parse(&replacement, Limits::default()).unwrap();
        owned
            .reload(&replacement_record, table(0.5, "Owned stages C"))
            .unwrap();
        let fresh = V1PartialSignalPathReference::new_emitted_stages(
            &replacement_record,
            table(0.5, "Owned stages C"),
            stage_placements(),
        )
        .unwrap();
        assert_same_state(&owned, &fresh);
        let (input_hp, output_hp) = owned.emitted_high_passes().unwrap();
        for (image, old) in [(input_hp, &hp_images.0), (output_hp, &hp_images.1)] {
            assert_ne!(image.coefficient_bits(), old.coefficient_bits());
            assert_eq!(
                (image.code().origin(), image.layout()),
                (old.code().origin(), old.layout())
            );
        }
        let (pre, post) = owned.emitted_firs().unwrap();
        assert_eq!((pre, post), (&fir_images.0, &fir_images.1));
        owned.process_block(&[0.25; 32]).unwrap();
        let before = owned.clone();
        for (offset, invalid) in [(0x285c, 24_000.0), (0x2860, f32::NAN), (0x80c, f32::NAN)] {
            let mut bad_bytes = replacement.clone();
            put(&mut bad_bytes, offset, invalid);
            let bad = RuntimeData::parse(&bad_bytes, Limits::default()).unwrap();
            assert!(owned.reload(&bad, table(0.25, "Rejected table")).is_err());
            assert_same_state(&owned, &before);
        }
        let mut mathematical =
            V1PartialSignalPathReference::new(&replacement_record, table(0.5, "Owned stages C"))
                .unwrap();
        mathematical.process_block(&[0.25; 32]).unwrap();
        assert_eq!(
            owned.process_block(&[0.5; 32]).unwrap().map(f32::to_bits),
            mathematical
                .process_block(&[0.5; 32])
                .unwrap()
                .map(f32::to_bits)
        );
    }

    #[test]
    fn owned_stage_budget_and_late_output_high_pass_failures_roll_back_complete_path() {
        use crate::filters::emitted::high_pass;
        use tetrad_sharc::{fir::FirError, simulator::SimulationError};
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        for mut other in [
            V1PartialSignalPathReference::new(&record, table(1.0, "Owned refusal")).unwrap(),
            V1PartialSignalPathReference::new_emitted(
                &record,
                table(1.0, "Owned refusal"),
                placements(),
            )
            .unwrap(),
        ] {
            let before = other.clone();
            assert_eq!(
                other.process_block_with_stage_budgets(&[0.0; 32], stage_budgets()),
                Err(Error::StageBudgetsRequireEmitted)
            );
            assert_same_state(&other, &before);
        }
        let mut owned = V1PartialSignalPathReference::new_emitted_stages(
            &record,
            table(1.0, "Owned rollback stages"),
            stage_placements(),
        )
        .unwrap();
        owned.process_block(&[0.25; 32]).unwrap();
        let before = owned.clone();
        let hp_budget = |budget| {
            Err(Error::HighPass(high_pass::Error::Simulation(
                SimulationError::StepBudget(budget),
            )))
        };
        let fir_budget = |budget| {
            Err(Error::Fir(FirError::Simulation(
                SimulationError::StepBudget(budget),
            )))
        };
        let full = stage_budgets();
        // Input HP, pre FIR, post FIR and, last, the output HP after all other stages.
        for (limits, expected) in [
            (
                V1StageInstructionBudgets {
                    input_hp: 423,
                    ..full
                },
                hp_budget(423),
            ),
            (
                V1StageInstructionBudgets {
                    pre: full.pre - 1,
                    ..full
                },
                fir_budget(full.pre - 1),
            ),
            (
                V1StageInstructionBudgets {
                    post: full.post - 1,
                    ..full
                },
                fir_budget(full.post - 1),
            ),
            (
                V1StageInstructionBudgets {
                    output_hp: 423,
                    ..full
                },
                hp_budget(423),
            ),
        ] {
            assert_eq!(
                owned.process_block_with_stage_budgets(&[0.5; 32], limits),
                expected
            );
            assert_same_state(&owned, &before);
        }
        let fir_limits = V1FirInstructionBudgets {
            post: budgets().post - 1,
            ..budgets()
        };
        assert_eq!(
            owned.process_block_with_fir_budgets(&[0.5; 32], fir_limits),
            fir_budget(fir_limits.post)
        );
        assert_same_state(&owned, &before);
        let mut invalid = [0.5; 32];
        invalid[31] = f32::NAN;
        assert!(matches!(
            owned.process_block(&invalid),
            Err(Error::HighPass(high_pass::Error::NonFiniteMemory { .. }))
        ));
        assert_same_state(&owned, &before);
        let mut control = before.clone();
        let expected = control.process_block(&[0.5; 32]).unwrap();
        assert_eq!(
            owned
                .process_block_with_stage_budgets(&[0.5; 32], full)
                .unwrap()
                .map(f32::to_bits),
            expected.map(f32::to_bits)
        );
        assert_same_state(&owned, &control);
        assert_eq!(owned.table().provenance(), "Owned rollback stages");
    }

    fn input_gain(current: f32, target: f32) -> GainAdapter {
        use tetrad_sharc::address::{DmWordAddress32, PmWordAddress24};
        GainAdapter::new(
            PmWordAddress24::new(0x4000).unwrap(),
            DmWordAddress32::new(0xffff_0000),
            current,
            target,
        )
        .unwrap()
    }

    #[test]
    fn input_gain_before_active_hp_matches_composition_and_isolates_instances() {
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let source = "Owned input-gain composition";
        let mut gain = input_gain(0.125, 1.75);
        assert_eq!(gain.compiled().max_steps(), 230);
        let mut path = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &record,
            table(1.0, source),
            stage_placements(),
            gain.clone(),
        )
        .unwrap();
        let isolated = path.clone();
        let mut mathematical =
            V1PartialSignalPathReference::new(&record, table(1.0, source)).unwrap();
        for block in 0..4 {
            if block == 1 || block == 3 {
                let target = if block == 1 { 0.25 } else { 2.0 };
                path.set_input_gain_target(target).unwrap();
                gain.set_linear_target(target).unwrap();
            }
            let inputs =
                std::array::from_fn(|index| ((index * 7 + block * 3) % 23) as f32 / 32.0 - 0.375);
            let gained = gain.process_block(&inputs, 230).unwrap();
            assert_eq!(gained.receipt.steps, 230);
            let expected = mathematical.process_block(&gained.outputs).unwrap();
            let actual = match block {
                0 => path.process_block(&inputs),
                1 => path.process_block_with_fir_budgets(&inputs, budgets()),
                2 => path.process_block_with_stage_budgets(&inputs, stage_budgets()),
                _ => path.process_block_with_input_gain_budgets(
                    &inputs,
                    V1InputGainInstructionBudgets {
                        input_gain: 230,
                        stages: stage_budgets(),
                    },
                ),
            }
            .unwrap();
            assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
            assert_eq!(
                path.input_gain().unwrap().current().to_bits(),
                gain.current().to_bits()
            );
            assert_eq!(
                path.input_gain().unwrap().target().to_bits(),
                gain.target().to_bits()
            );
            assert_eq!(path.input_gain().unwrap().compiled(), gain.compiled());
            assert_eq!(
                high_pass_state(&path.input_hp),
                high_pass_state(&mathematical.input_hp)
            );
        }
        let fresh = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &record,
            table(1.0, source),
            stage_placements(),
            input_gain(0.125, 1.75),
        )
        .unwrap();
        assert_same_state(&isolated, &fresh);
        // All original constructors still omit gain and produce the ungained path.
        let mut originals = [
            V1PartialSignalPathReference::new(&record, table(1.0, source)).unwrap(),
            V1PartialSignalPathReference::new_emitted(&record, table(1.0, source), placements())
                .unwrap(),
            V1PartialSignalPathReference::new_emitted_stages(
                &record,
                table(1.0, source),
                stage_placements(),
            )
            .unwrap(),
        ];
        let expected = originals[0].clone().process_block(&[0.25; 32]).unwrap();
        for original in &mut originals {
            assert!(original.input_gain().is_none());
            let before = original.clone();
            assert_eq!(
                original.set_input_gain_target(1.0),
                Err(Error::InputGainNotConfigured)
            );
            assert_eq!(
                original.reseed_input_gain(1.0, 1.0),
                Err(Error::InputGainNotConfigured)
            );
            assert_same_state(original, &before);
            assert_eq!(
                original.process_block_with_input_gain_budgets(
                    &[f32::NAN; 32],
                    V1InputGainInstructionBudgets {
                        input_gain: 0,
                        stages: stage_budgets()
                    },
                ),
                Err(Error::InputGainBudgetsRequireEmitted)
            );
            assert_same_state(original, &before);
            assert_eq!(
                original
                    .process_block(&[0.25; 32])
                    .unwrap()
                    .map(f32::to_bits),
                expected.map(f32::to_bits)
            );
        }
    }

    #[test]
    fn input_gain_and_late_stage_failures_and_controls_preserve_composite_bits() {
        use crate::{filters::emitted::high_pass, gain::emitted};
        use tetrad_sharc::simulator::SimulationError;
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut path = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &record,
            table(1.0, "Owned composite rollback"),
            stage_placements(),
            input_gain(0.125, 1.75),
        )
        .unwrap();
        path.process_block(&[0.25; 32]).unwrap();
        path.reseed_input_gain(-0.0, -0.0).unwrap();
        let before = path.clone();
        for invalid in [f32::NAN, f32::INFINITY, -1.0] {
            assert!(path.set_input_gain_target(invalid).is_err());
            assert_same_state(&path, &before);
            for (current, target) in [(invalid, 0.0), (0.0, invalid)] {
                assert!(path.reseed_input_gain(current, target).is_err());
                assert_same_state(&path, &before);
            }
        }
        path.set_input_gain_target(1.75).unwrap();
        let before = path.clone();
        let full = V1InputGainInstructionBudgets {
            input_gain: 230,
            stages: stage_budgets(),
        };
        assert_eq!(
            path.process_block_with_input_gain_budgets(
                &[0.5; 32],
                V1InputGainInstructionBudgets {
                    input_gain: 229,
                    ..full
                }
            ),
            Err(Error::Gain(emitted::Error::Simulation(
                SimulationError::StepBudget(229)
            )))
        );
        assert_same_state(&path, &before);
        let late = V1StageInstructionBudgets {
            output_hp: 423,
            ..full.stages
        };
        // Prove the output-HP failure is reached after the gain ramp advances.
        let mut staged = path.clone();
        let gained = staged
            .input_gain
            .as_mut()
            .unwrap()
            .process_block(&[0.5; 32], 230)
            .unwrap();
        assert_ne!(
            staged.input_gain().unwrap().current().to_bits(),
            before.input_gain().unwrap().current().to_bits()
        );
        let hp = staged
            .input_hp
            .process_block(&gained.outputs, Some(full.stages.input_hp))
            .unwrap();
        let pre = staged
            .pre
            .process_block(&hp, Some(full.stages.pre))
            .unwrap();
        let interpolated = staged.resampler.interpolate_block(&pre).unwrap();
        let mut neural = [0.0; 64];
        staged
            .neural
            .process_chunk(&interpolated, &mut neural)
            .unwrap();
        let decimated = staged.resampler.decimate_block(&neural).unwrap();
        let post = staged
            .post
            .process_block(&decimated, Some(full.stages.post))
            .unwrap();
        let expected = Err(Error::HighPass(high_pass::Error::Simulation(
            SimulationError::StepBudget(423),
        )));
        assert_eq!(staged.output_hp.process_block(&post, Some(423)), expected);
        assert_eq!(
            path.process_block_with_input_gain_budgets(
                &[0.5; 32],
                V1InputGainInstructionBudgets {
                    stages: late,
                    ..full
                }
            ),
            expected
        );
        assert_same_state(&path, &before);
        assert_eq!(
            path.process_block_with_stage_budgets(&[0.5; 32], late),
            expected
        );
        assert_same_state(&path, &before);
        assert!(
            path.process_block_with_fir_budgets(
                &[0.5; 32],
                V1FirInstructionBudgets {
                    post: budgets().post - 1,
                    ..budgets()
                }
            )
            .is_err()
        );
        assert_same_state(&path, &before);
        let mut invalid = [0.5; 32];
        invalid[31] = f32::NAN;
        assert!(matches!(
            path.process_block(&invalid),
            Err(Error::Gain(emitted::Error::NonFiniteMemory { .. }))
        ));
        assert_same_state(&path, &before);
        let mut bad_bytes = bytes.clone();
        put(&mut bad_bytes, 0x285c, 24_000.0);
        let bad = RuntimeData::parse(&bad_bytes, Limits::default()).unwrap();
        assert_eq!(
            path.reload(&bad, table(0.5, "Rejected context")),
            Err(Error::HighPassDomain)
        );
        assert_same_state(&path, &before);
        // Even a configured adapter cannot bypass the all-four-emitted precondition.
        let mathematical =
            V1PartialSignalPathReference::new(&record, table(1.0, "Owned refusal")).unwrap();
        for filter in 0..4 {
            let mut other = before.clone();
            match filter {
                0 => other.input_hp = mathematical.input_hp.clone(),
                1 => other.pre = mathematical.pre.clone(),
                2 => other.post = mathematical.post.clone(),
                _ => other.output_hp = mathematical.output_hp.clone(),
            }
            let unchanged = other.clone();
            assert_eq!(
                other.process_block_with_input_gain_budgets(&[f32::NAN; 32], full),
                Err(Error::InputGainBudgetsRequireEmitted)
            );
            assert_same_state(&other, &unchanged);
        }
        let mut control = before.clone();
        let expected = control.process_block(&[0.5; 32]).unwrap();
        assert_eq!(
            path.process_block_with_input_gain_budgets(&[0.5; 32], full)
                .unwrap()
                .map(f32::to_bits),
            expected.map(f32::to_bits)
        );
        assert_same_state(&path, &control);
    }

    #[test]
    fn input_gain_transient_and_images_survive_reset_and_reload_without_reseed() {
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut path = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &record,
            table(1.0, "Owned gain reset A"),
            stage_placements(),
            input_gain(0.125, 1.75),
        )
        .unwrap();
        path.process_block(&[0.25; 32]).unwrap();
        path.process_block(&[-0.125; 32]).unwrap();
        path.set_input_gain_target(-0.0).unwrap();
        let retained = path.input_gain().unwrap().clone();
        assert_ne!(retained.current().to_bits(), 0.125f32.to_bits());
        let hp_images = path
            .emitted_high_passes()
            .map(|(a, b)| (a.clone(), b.clone()))
            .unwrap();
        let fir_images = path
            .emitted_firs()
            .map(|(a, b)| (a.clone(), b.clone()))
            .unwrap();
        path.reset().unwrap();
        let fresh = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &record,
            table(1.0, "Owned gain reset A"),
            stage_placements(),
            retained.clone(),
        )
        .unwrap();
        assert_same_state(&path, &fresh);
        path.process_block(&[0.25; 32]).unwrap();
        let retained = path.input_gain().unwrap().clone();
        path.reload(&record, table(0.5, "Owned gain reload B"))
            .unwrap();
        let fresh = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &record,
            table(0.5, "Owned gain reload B"),
            stage_placements(),
            retained.clone(),
        )
        .unwrap();
        assert_same_state(&path, &fresh);
        assert_eq!(
            path.emitted_high_passes().unwrap(),
            (&hp_images.0, &hp_images.1)
        );
        let mut replacement = active_high_pass_record(300.0);
        put(&mut replacement, 0x80c, -0.75);
        let replacement_record = RuntimeData::parse(&replacement, Limits::default()).unwrap();
        path.reload(&replacement_record, table(0.5, "Owned gain reload C"))
            .unwrap();
        let mut fresh = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &replacement_record,
            table(0.5, "Owned gain reload C"),
            stage_placements(),
            retained,
        )
        .unwrap();
        assert_same_state(&path, &fresh);
        for (image, old) in [
            (path.emitted_high_passes().unwrap().0, &hp_images.0),
            (path.emitted_high_passes().unwrap().1, &hp_images.1),
        ] {
            assert_ne!(image.coefficient_bits(), old.coefficient_bits());
            assert_eq!(
                (image.code().origin(), image.layout()),
                (old.code().origin(), old.layout())
            );
        }
        assert_eq!(path.emitted_firs().unwrap(), (&fir_images.0, &fir_images.1));
        let inputs = [0.5; 32];
        assert_eq!(
            path.process_block(&inputs).unwrap().map(f32::to_bits),
            fresh.process_block(&inputs).unwrap().map(f32::to_bits)
        );
        assert_same_state(&path, &fresh);
        let before = path.clone();
        put(&mut replacement, 0x80c, f32::NAN);
        let bad = RuntimeData::parse(&replacement, Limits::default()).unwrap();
        assert!(path.reload(&bad, table(0.25, "Rejected reload")).is_err());
        assert_same_state(&path, &before);
    }

    fn output_gain(current: f32, target: f32) -> GainAdapter {
        use tetrad_sharc::address::{DmWordAddress32, PmWordAddress24};
        GainAdapter::new(
            PmWordAddress24::new(0x5000).unwrap(),
            DmWordAddress32::new(0x10000),
            current,
            target,
        )
        .unwrap()
    }

    #[test]
    fn output_gain_after_active_hp_matches_composition_across_backends_and_entries() {
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let source = "Owned output-gain composition; tones absent";
        let originals = [
            V1PartialSignalPathReference::new(&record, table(1.0, source)).unwrap(),
            V1PartialSignalPathReference::new_emitted(&record, table(1.0, source), placements())
                .unwrap(),
            V1PartialSignalPathReference::new_emitted_stages(
                &record,
                table(1.0, source),
                stage_placements(),
            )
            .unwrap(),
            V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
                &record,
                table(1.0, source),
                stage_placements(),
                input_gain(0.125, 1.75),
            )
            .unwrap(),
        ];
        for mut prefix in originals {
            assert!(prefix.output_gain().is_none());
            let mut gain = output_gain(2.0, 0.375);
            let mut path = prefix.clone().with_output_gain(gain.clone()).unwrap();
            for block in 0..5 {
                let inputs = std::array::from_fn(|index| {
                    ((index * 7 + block * 3) % 23) as f32 / 32.0 - 0.375
                });
                let hp_outputs = prefix.process_block(&inputs).unwrap();
                let expected = gain.process_block(&hp_outputs, 230).unwrap();
                assert_eq!(expected.receipt.steps, 230);
                let actual = match block {
                    1 => path.process_block_with_output_gain_budget(&inputs, 230),
                    2 if path.emitted_firs().is_some() => {
                        path.process_block_with_fir_budgets(&inputs, budgets())
                    }
                    3 if path.emitted_high_passes().is_some() => {
                        path.process_block_with_stage_budgets(&inputs, stage_budgets())
                    }
                    4 if path.input_gain().is_some() => path.process_block_with_input_gain_budgets(
                        &inputs,
                        V1InputGainInstructionBudgets {
                            input_gain: 230,
                            stages: stage_budgets(),
                        },
                    ),
                    _ => path.process_block(&inputs),
                }
                .unwrap();
                assert_eq!(actual.map(f32::to_bits), expected.outputs.map(f32::to_bits));
                assert_ne!(high_pass_state(&path.output_hp), [0; 2]);
                let composed = prefix.clone().with_output_gain(gain.clone()).unwrap();
                assert_same_state(&path, &composed);
                if let Some(input) = path.input_gain() {
                    assert_ne!(input.current().to_bits(), gain.current().to_bits());
                    assert_ne!(input.target().to_bits(), gain.target().to_bits());
                }
            }
        }
    }

    #[test]
    fn late_output_gain_budget_failure_rolls_back_both_gains_and_all_prefix_state() {
        use crate::gain::emitted;
        use tetrad_sharc::simulator::SimulationError;
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut path = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &record,
            table(1.0, "Owned late output rollback"),
            stage_placements(),
            input_gain(0.125, 1.75),
        )
        .unwrap()
        .with_output_gain(output_gain(2.0, 0.375))
        .unwrap();
        path.process_block(&[0.25; 32]).unwrap();
        let before = path.clone();
        let inputs = [-0.125; 32];
        let mut prefix = before.clone();
        let mut gain = prefix.output_gain.take().unwrap();
        let hp_outputs = prefix.process_block(&inputs).unwrap();
        assert_ne!(
            prefix.input_gain().unwrap().current().to_bits(),
            before.input_gain().unwrap().current().to_bits()
        );
        assert_ne!(prefix.input_hp, before.input_hp);
        assert_ne!(prefix.pre, before.pre);
        assert_ne!(prefix.resampler, before.resampler);
        assert_ne!(
            format!("{:?}", prefix.neural),
            format!("{:?}", before.neural)
        );
        assert_ne!(prefix.post, before.post);
        assert_ne!(prefix.output_hp, before.output_hp);
        let expected_error = Err(Error::Gain(emitted::Error::Simulation(
            SimulationError::StepBudget(229),
        )));
        assert_eq!(
            gain.process_block(&hp_outputs, 229)
                .map(|block| block.outputs),
            expected_error
        );
        assert_eq!(
            path.process_block_with_output_gain_budget(&inputs, 229),
            expected_error
        );
        assert_same_state(&path, &before);
        let expected = gain.process_block(&hp_outputs, 230).unwrap().outputs;
        let composed = prefix.with_output_gain(gain).unwrap();
        let mut pristine = before;
        assert_eq!(
            pristine
                .process_block_with_output_gain_budget(&inputs, 230)
                .unwrap()
                .map(f32::to_bits),
            expected.map(f32::to_bits)
        );
        assert_eq!(
            path.process_block_with_output_gain_budget(&inputs, 230)
                .unwrap()
                .map(f32::to_bits),
            expected.map(f32::to_bits)
        );
        assert_same_state(&path, &pristine);
        assert_same_state(&path, &composed);
    }

    #[test]
    fn output_gain_controls_validate_atomically_and_keep_existing_input_refusals() {
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut path =
            V1PartialSignalPathReference::new(&record, table(1.0, "Owned output controls"))
                .unwrap();
        let before = path.clone();
        assert_eq!(
            path.set_output_gain_target(f32::NAN),
            Err(Error::OutputGainNotConfigured)
        );
        assert_eq!(
            path.reseed_output_gain(-1.0, f32::NAN),
            Err(Error::OutputGainNotConfigured)
        );
        assert_eq!(
            path.process_block_with_output_gain_budget(&[f32::NAN; 32], 0),
            Err(Error::OutputGainNotConfigured)
        );
        assert_same_state(&path, &before);
        path = path.with_output_gain(output_gain(2.0, 0.375)).unwrap();
        path.process_block(&[0.25; 32]).unwrap();
        let mut expected = path.clone();
        path.set_output_gain_target(-0.0).unwrap();
        expected
            .output_gain
            .as_mut()
            .unwrap()
            .set_linear_target(-0.0)
            .unwrap();
        assert_same_state(&path, &expected);
        assert_eq!(
            path.output_gain().unwrap().target().to_bits(),
            (-0.0_f32).to_bits()
        );
        let before = path.clone();
        for (invalid, error) in [
            (f32::NAN, Error::NonFinite),
            (f32::INFINITY, Error::NonFinite),
            (-1.0, Error::LinearGainRange),
        ] {
            assert_eq!(path.set_output_gain_target(invalid).unwrap_err(), error);
            assert_same_state(&path, &before);
            for (current, target) in [(invalid, 0.25), (0.25, invalid)] {
                assert_eq!(path.reseed_output_gain(current, target).unwrap_err(), error);
                assert_same_state(&path, &before);
            }
        }
        path.reseed_output_gain(-0.0, 0.0).unwrap();
        expected
            .output_gain
            .as_mut()
            .unwrap()
            .reseed(-0.0, 0.0)
            .unwrap();
        assert_same_state(&path, &expected);
        assert_eq!(
            path.output_gain().unwrap().current().to_bits(),
            (-0.0_f32).to_bits()
        );
        assert_eq!(
            path.output_gain().unwrap().target().to_bits(),
            0.0_f32.to_bits()
        );
        // Only this explicit clone survives: the receiver of the refused attach is consumed.
        let before = path.clone();
        assert!(matches!(
            path.clone().with_output_gain(output_gain(0.5, 1.0)),
            Err(Error::OutputGainAlreadyConfigured)
        ));
        assert_same_state(&path, &before);
        let emitted = V1PartialSignalPathReference::new_emitted_stages(
            &record,
            table(1.0, "Owned output controls"),
            stage_placements(),
        )
        .unwrap()
        .with_output_gain(output_gain(2.0, 0.375))
        .unwrap();
        for mut output_only in [path, emitted] {
            let before = output_only.clone();
            assert_eq!(
                output_only.set_input_gain_target(1.0),
                Err(Error::InputGainNotConfigured)
            );
            assert_eq!(
                output_only.reseed_input_gain(1.0, 1.0),
                Err(Error::InputGainNotConfigured)
            );
            assert_eq!(
                output_only.process_block_with_input_gain_budgets(
                    &[f32::NAN; 32],
                    V1InputGainInstructionBudgets {
                        input_gain: 0,
                        stages: stage_budgets()
                    }
                ),
                Err(Error::InputGainBudgetsRequireEmitted)
            );
            if output_only.emitted_firs().is_none() {
                assert_eq!(
                    output_only.process_block_with_fir_budgets(&[f32::NAN; 32], budgets()),
                    Err(Error::FirBudgetsRequireEmitted)
                );
                assert_eq!(
                    output_only.process_block_with_stage_budgets(&[f32::NAN; 32], stage_budgets()),
                    Err(Error::StageBudgetsRequireEmitted)
                );
            }
            assert_same_state(&output_only, &before);
        }
    }

    #[test]
    fn both_gain_transients_and_images_survive_owned_reset_and_reload() {
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut path = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &record,
            table(1.0, "Owned both-gain reset"),
            stage_placements(),
            input_gain(0.125, 1.75),
        )
        .unwrap()
        .with_output_gain(output_gain(2.0, 0.375))
        .unwrap();
        path.process_block(&[0.25; 32]).unwrap();
        path.process_block(&[-0.125; 32]).unwrap();
        path.set_input_gain_target(0.25).unwrap();
        path.set_output_gain_target(-0.0).unwrap();
        let input = path.input_gain().unwrap().clone();
        let output = path.output_gain().unwrap().clone();
        assert_ne!(input.current().to_bits(), input.target().to_bits());
        assert_ne!(output.current().to_bits(), output.target().to_bits());
        let hp_images = path
            .emitted_high_passes()
            .map(|(a, b)| (a.clone(), b.clone()))
            .unwrap();
        let fir_images = path
            .emitted_firs()
            .map(|(a, b)| (a.clone(), b.clone()))
            .unwrap();
        path.reset().unwrap();
        let fresh = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &record,
            table(1.0, "Owned both-gain reset"),
            stage_placements(),
            input,
        )
        .unwrap()
        .with_output_gain(output)
        .unwrap();
        assert_same_state(&path, &fresh);
        path.process_block(&[0.5; 32]).unwrap();
        let input = path.input_gain().unwrap().clone();
        let output = path.output_gain().unwrap().clone();
        path.reload(&record, table(0.5, "Owned both-gain reload"))
            .unwrap();
        let fresh = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &record,
            table(0.5, "Owned both-gain reload"),
            stage_placements(),
            input.clone(),
        )
        .unwrap()
        .with_output_gain(output.clone())
        .unwrap();
        assert_same_state(&path, &fresh);
        assert_eq!(
            path.emitted_high_passes().unwrap(),
            (&hp_images.0, &hp_images.1)
        );
        let mut replacement = active_high_pass_record(300.0);
        put(&mut replacement, 0x80c, -0.75);
        let replacement_record = RuntimeData::parse(&replacement, Limits::default()).unwrap();
        path.reload(
            &replacement_record,
            table(0.5, "Owned changed both-gain reload"),
        )
        .unwrap();
        let fresh = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &replacement_record,
            table(0.5, "Owned changed both-gain reload"),
            stage_placements(),
            input,
        )
        .unwrap()
        .with_output_gain(output)
        .unwrap();
        assert_same_state(&path, &fresh);
        assert_eq!(path.emitted_firs().unwrap(), (&fir_images.0, &fir_images.1));
        for (image, old) in [
            (path.emitted_high_passes().unwrap().0, &hp_images.0),
            (path.emitted_high_passes().unwrap().1, &hp_images.1),
        ] {
            assert_ne!(image.coefficient_bits(), old.coefficient_bits());
            assert_eq!(
                (image.code().origin(), image.layout()),
                (old.code().origin(), old.layout())
            );
        }
        path.process_block(&[0.25; 32]).unwrap();
        let before = path.clone();
        for (offset, invalid) in [(0x285c, 24_000.0), (0x80c, f32::NAN), (0xc, f32::MAX)] {
            let mut bad_bytes = replacement.clone();
            put(&mut bad_bytes, offset, invalid);
            let bad = RuntimeData::parse(&bad_bytes, Limits::default()).unwrap();
            assert!(
                path.reload(&bad, table(0.25, "Rejected both-gain context"))
                    .is_err()
            );
            assert_same_state(&path, &before);
        }
    }

    #[test]
    fn cross_block_gain_targets_and_matching_logical_maps_stay_independent() {
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let source = "Owned separate gain DM invocations";
        let prefix = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &record,
            table(1.0, source),
            stage_placements(),
            input_gain(0.125, 1.75),
        )
        .unwrap();
        let mut matching = prefix
            .clone()
            .with_output_gain(input_gain(2.0, 0.375))
            .unwrap();
        let mut distinct = prefix.with_output_gain(output_gain(2.0, 0.375)).unwrap();
        let isolated = matching.clone();
        assert_eq!(
            matching.input_gain().unwrap().compiled(),
            matching.output_gain().unwrap().compiled()
        );
        assert_ne!(
            distinct.input_gain().unwrap().compiled().layout(),
            distinct.output_gain().unwrap().compiled().layout()
        );
        assert_ne!(
            distinct.input_gain().unwrap().compiled().code().origin(),
            distinct.output_gain().unwrap().compiled().code().origin()
        );
        let mut mathematical =
            V1PartialSignalPathReference::new(&record, table(1.0, source)).unwrap();
        let mut input = input_gain(0.125, 1.75);
        let mut output = output_gain(2.0, 0.375);
        for block in 0..3 {
            let (input_target, output_target) = [(1.75, 0.375), (0.25, 2.0), (1.0, -0.0)][block];
            for path in [&mut matching, &mut distinct] {
                path.set_input_gain_target(input_target).unwrap();
                path.set_output_gain_target(output_target).unwrap();
                // The oracle adapter's current is decoded from the preceding actual DM store.
                assert_eq!(
                    path.input_gain().unwrap().current().to_bits(),
                    input.current().to_bits()
                );
                assert_eq!(
                    path.output_gain().unwrap().current().to_bits(),
                    output.current().to_bits()
                );
            }
            input.set_linear_target(input_target).unwrap();
            output.set_linear_target(output_target).unwrap();
            let inputs =
                std::array::from_fn(|index| ((index * 5 + block) % 19) as f32 / 32.0 - 0.25);
            let gained = input.process_block(&inputs, 230).unwrap();
            let hp_outputs = mathematical.process_block(&gained.outputs).unwrap();
            let expected = output.process_block(&hp_outputs, 230).unwrap();
            for path in [&mut matching, &mut distinct] {
                let actual = path.process_block(&inputs).unwrap();
                assert_eq!(actual.map(f32::to_bits), expected.outputs.map(f32::to_bits));
                assert_eq!(
                    path.input_gain().unwrap().current().to_bits(),
                    input.current().to_bits()
                );
                assert_eq!(
                    path.output_gain().unwrap().current().to_bits(),
                    output.current().to_bits()
                );
                assert_eq!(
                    path.input_gain().unwrap().target().to_bits(),
                    input.target().to_bits()
                );
                assert_eq!(
                    path.output_gain().unwrap().target().to_bits(),
                    output.target().to_bits()
                );
                assert_ne!(
                    path.input_gain().unwrap().current().to_bits(),
                    path.output_gain().unwrap().current().to_bits()
                );
                assert_eq!(
                    high_pass_state(&path.input_hp),
                    high_pass_state(&mathematical.input_hp)
                );
                assert_eq!(
                    high_pass_state(&path.output_hp),
                    high_pass_state(&mathematical.output_hp)
                );
            }
            let mut matching_prefix = matching.clone();
            let mut distinct_prefix = distinct.clone();
            for path in [&mut matching_prefix, &mut distinct_prefix] {
                path.input_gain = None;
                path.output_gain = None;
            }
            assert_same_state(&matching_prefix, &distinct_prefix);
        }
        let fresh = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &record,
            table(1.0, source),
            stage_placements(),
            input_gain(0.125, 1.75),
        )
        .unwrap()
        .with_output_gain(input_gain(2.0, 0.375))
        .unwrap();
        assert_same_state(&isolated, &fresh);
    }

    fn toned(
        prefix: &V1PartialSignalPathReference,
        tones: &V1ToneStackReference,
        gain: &GainAdapter,
    ) -> V1PartialSignalPathReference {
        prefix
            .clone()
            .with_tones(tones.clone())
            .unwrap()
            .with_output_gain(gain.clone())
            .unwrap()
    }

    #[test]
    fn tones_run_between_output_hp_and_output_gain_through_updates_reset_reload() {
        use crate::tones::V1Tone;
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let source = "Owned tone composition";
        let mut prefix = V1PartialSignalPathReference::new(&record, table(1.0, source)).unwrap();
        assert!(prefix.tones().is_none());
        assert_eq!(
            prefix.set_tone_parameters([0.5; 3]),
            Err(Error::TonesNotConfigured)
        );
        let mut tones = V1ToneStackReference::from_normalized([0.9, 0.2, 0.75]).unwrap();
        let mut gain = output_gain(2.0, 0.375);
        let mut path = toned(&prefix, &tones, &gain);
        for block in 0..4 {
            if block == 2 {
                // Block-boundary update: coefficients change, histories carry over.
                path.set_tone_parameters([0.1, 1.0, 0.0]).unwrap();
                tones.set_normalized([0.1, 1.0, 0.0]).unwrap();
            }
            let inputs =
                std::array::from_fn(|index| ((index * 5 + block * 3) % 19) as f32 / 16.0 - 0.5);
            let hp_outputs = prefix.process_block(&inputs).unwrap();
            let tone_outputs = tones.process_block(&hp_outputs).unwrap();
            assert_ne!(tone_outputs, hp_outputs);
            let expected = gain.process_block(&tone_outputs, 230).unwrap().outputs;
            assert_eq!(
                path.process_block(&inputs).unwrap().map(f32::to_bits),
                expected.map(f32::to_bits)
            );
            assert_same_state(&path, &toned(&prefix, &tones, &gain));
        }
        assert_eq!(
            path.clone().with_tones(tones.clone()).unwrap_err(),
            Error::TonesAlreadyConfigured
        );

        // Reset zeros tone histories and keeps parameters, like stock reset 0x492.
        path.reset().unwrap();
        let reset = path.tones().unwrap();
        assert_eq!(reset.normalized(), [0.1, 1.0, 0.0]);
        assert!(
            V1Tone::ALL
                .iter()
                .all(|&tone| reset.section(tone).history() == [0.0; 4])
        );
        prefix.reset().unwrap();
        tones.reset();
        assert_same_state(&path, &toned(&prefix, &tones, &gain));

        // Reload keeps tone parameters with reset histories.
        let mut reloaded = path.clone();
        reloaded.process_block(&[0.25; 32]).unwrap();
        reloaded.reload(&record, table(1.0, source)).unwrap();
        assert_eq!(reloaded.tones, path.tones);
    }

    #[test]
    fn late_tone_failure_rolls_back_gains_tones_and_all_filter_state() {
        use crate::tones::{V1Tone, V1ToneCoefficients, V1ToneReference};
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut tones = V1ToneStackReference::from_normalized([0.9, 0.2, 0.75]).unwrap();
        let mut path = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &record,
            table(1.0, "Owned late tone rollback"),
            stage_placements(),
            input_gain(0.125, 1.75),
        )
        .unwrap()
        .with_tones(tones.clone())
        .unwrap()
        .with_output_gain(output_gain(2.0, 0.375))
        .unwrap();
        let inputs = std::array::from_fn(|index| (index % 7) as f32 / 8.0 - 0.375);
        path.process_block(&inputs).unwrap();

        // Owned synthetic overflow coefficients force the third tone to fail late.
        tones.replace_section_for_test(
            V1Tone::Third,
            V1ToneReference::new(V1ToneCoefficients {
                feedforward: [f32::MAX, 0.0, 0.0],
                feedback: [f32::MAX, 0.0],
            })
            .unwrap(),
        );
        let Some(ToneStage::Mathematical(attached)) = &mut path.tones else {
            panic!("mathematical tones attached");
        };
        attached.replace_section_for_test(V1Tone::Third, tones.section(V1Tone::Third).clone());
        let before = path.clone();
        assert_eq!(path.process_block(&inputs), Err(Error::NumericOverflow));
        assert_same_state(&path, &before);
        assert_eq!(path.table().provenance(), before.table().provenance());
        assert_eq!(
            path.set_tone_parameters([0.5, 0.5, 2.0]),
            Err(Error::NormalizedRange)
        );
        assert_same_state(&path, &before);
    }

    fn tone_placements() -> [crate::tones::emitted::ToneLogicalPlacement; 3] {
        use crate::tones::emitted::ToneLogicalPlacement;
        use tetrad_sharc::address::{DmWordAddress32, PmWordAddress24};
        // Each leaf invocation has private DM, so placements may also match the gains'.
        let at = |origin, dm_base| ToneLogicalPlacement {
            origin: PmWordAddress24::new(origin).unwrap(),
            dm_base: DmWordAddress32::new(dm_base),
        };
        [
            at(0x1400, 0xffff_0000),
            at(0x1400, 0xffff_0000),
            at(0x1800, 0x10000),
        ]
    }

    fn emitted_tones(normalized: [f32; 3]) -> EmittedV1ToneStackReference {
        EmittedV1ToneStackReference::new(normalized, tone_placements()).unwrap()
    }

    const TONE_BUDGETS: V1ToneInstructionBudgets = V1ToneInstructionBudgets {
        first: 366,
        second: 366,
        third: 366,
    };

    fn originals(record: &RuntimeData<'_>, source: &str) -> [V1PartialSignalPathReference; 4] {
        [
            V1PartialSignalPathReference::new(record, table(1.0, source)).unwrap(),
            V1PartialSignalPathReference::new_emitted(record, table(1.0, source), placements())
                .unwrap(),
            V1PartialSignalPathReference::new_emitted_stages(
                record,
                table(1.0, source),
                stage_placements(),
            )
            .unwrap(),
            V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
                record,
                table(1.0, source),
                stage_placements(),
                input_gain(0.125, 1.75),
            )
            .unwrap(),
        ]
    }

    #[test]
    fn emitted_tones_match_host_tones_after_output_hp_and_before_output_gain() {
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        for mut prefix in originals(&record, "Owned emitted tone composition") {
            let mut host = V1ToneStackReference::from_normalized([0.9, 0.2, 0.75]).unwrap();
            let mut stack = emitted_tones([0.9, 0.2, 0.75]);
            let mut gain = output_gain(2.0, 0.375);
            let mut mathematical = toned(&prefix, &host, &gain);
            let mut path = prefix
                .clone()
                .with_emitted_tones(stack.clone())
                .unwrap()
                .with_output_gain(gain.clone())
                .unwrap();
            // Each accessor exposes only its own variant.
            assert!(path.tones().is_none() && mathematical.emitted_tones().is_none());
            for block in 0..6 {
                if block == 3 {
                    for target in [&mut path, &mut mathematical] {
                        target.set_tone_parameters([0.1, 1.0, 0.0]).unwrap();
                    }
                    host.set_normalized([0.1, 1.0, 0.0]).unwrap();
                    stack.set_normalized([0.1, 1.0, 0.0]).unwrap();
                }
                let inputs =
                    std::array::from_fn(|index| ((index * 5 + block * 3) % 19) as f32 / 16.0 - 0.5);
                let hp_outputs = prefix.process_block(&inputs).unwrap();
                let tone = stack.process_block(&hp_outputs, TONE_BUDGETS).unwrap();
                assert_ne!(tone.outputs, hp_outputs);
                let expected = gain.process_block(&tone.outputs, 230).unwrap().outputs;
                host.process_block(&hp_outputs).unwrap();
                let actual = match block {
                    1 => path.process_block_with_tone_budgets(&inputs, TONE_BUDGETS),
                    2 => path.process_block_with_output_gain_budget(&inputs, 230),
                    4 if path.emitted_high_passes().is_some() => {
                        path.process_block_with_stage_budgets(&inputs, stage_budgets())
                    }
                    5 if path.emitted_firs().is_some() => {
                        path.process_block_with_fir_budgets(&inputs, budgets())
                    }
                    _ => path.process_block(&inputs),
                }
                .unwrap();
                assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
                assert_eq!(
                    mathematical
                        .process_block(&inputs)
                        .unwrap()
                        .map(f32::to_bits),
                    expected.map(f32::to_bits)
                );
                let composed = prefix
                    .clone()
                    .with_emitted_tones(stack.clone())
                    .unwrap()
                    .with_output_gain(gain.clone())
                    .unwrap();
                assert_same_state(&path, &composed);
                assert_same_state(&mathematical, &toned(&prefix, &host, &gain));
            }
        }
    }

    #[test]
    fn tone_attachments_and_budget_entry_refuse_before_work() {
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let host = V1ToneStackReference::from_normalized([0.5; 3]).unwrap();
        let stack = emitted_tones([0.5; 3]);
        for prefix in originals(&record, "Owned tone refusals") {
            assert!(prefix.tones().is_none() && prefix.emitted_tones().is_none());
            let mathematical = prefix.clone().with_tones(host.clone()).unwrap();
            let emitted = prefix.clone().with_emitted_tones(stack.clone()).unwrap();
            // No replacement and no silent conversion in either direction.
            for attached in [&mathematical, &emitted] {
                assert_eq!(
                    attached.clone().with_tones(host.clone()).unwrap_err(),
                    Error::TonesAlreadyConfigured
                );
                assert_eq!(
                    attached
                        .clone()
                        .with_emitted_tones(stack.clone())
                        .unwrap_err(),
                    Error::TonesAlreadyConfigured
                );
            }
            // Missing or mathematical tones refuse before cloning: NaN inputs never run.
            for mut other in [prefix, mathematical] {
                let before = other.clone();
                assert_eq!(
                    other.process_block_with_tone_budgets(&[f32::NAN; 32], TONE_BUDGETS),
                    Err(Error::ToneBudgetsRequireEmitted)
                );
                assert_same_state(&other, &before);
            }
            let mut emitted = emitted;
            assert_eq!(emitted.emitted_tones(), Some(&stack));
            assert_eq!(
                emitted.emitted_tones().unwrap().full_budgets(),
                TONE_BUDGETS
            );
            emitted.process_block(&[0.25; 32]).unwrap();
            let before = emitted.clone();
            let mut control = before.clone();
            let expected = control.process_block(&[0.5; 32]).unwrap();
            let step_budget = |budget| {
                Err(Error::Tone(crate::tones::emitted::Error::Simulation(
                    tetrad_sharc::simulator::SimulationError::StepBudget(budget),
                )))
            };
            for (limits, budget) in [
                (
                    V1ToneInstructionBudgets {
                        first: 0,
                        ..TONE_BUDGETS
                    },
                    0,
                ),
                (
                    V1ToneInstructionBudgets {
                        second: 365,
                        ..TONE_BUDGETS
                    },
                    365,
                ),
            ] {
                assert_eq!(
                    emitted.process_block_with_tone_budgets(&[0.5; 32], limits),
                    step_budget(budget)
                );
                assert_same_state(&emitted, &before);
            }
            assert_eq!(
                emitted
                    .process_block_with_tone_budgets(&[0.5; 32], TONE_BUDGETS)
                    .unwrap()
                    .map(f32::to_bits),
                expected.map(f32::to_bits)
            );
            assert_same_state(&emitted, &control);
        }
    }

    #[test]
    fn late_third_tone_and_output_gain_failures_retain_every_state_then_retry_matches() {
        use crate::{
            gain::emitted,
            tones::{V1Tone, emitted::Error as ToneError},
        };
        use tetrad_sharc::simulator::SimulationError;
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let mut path = V1PartialSignalPathReference::new_emitted_stages_with_input_gain(
            &record,
            table(1.0, "Owned late emitted tone rollback"),
            stage_placements(),
            input_gain(0.125, 1.75),
        )
        .unwrap()
        .with_emitted_tones(emitted_tones([0.9, 0.2, 0.75]))
        .unwrap()
        .with_output_gain(output_gain(2.0, 0.375))
        .unwrap();
        path.process_block(&[0.25; 32]).unwrap();
        let before = path.clone();
        let inputs = [-0.125; 32];
        // Prove every earlier stage, both earlier tone leaves, advances on the stage.
        let mut prefix = before.clone();
        let mut gain = prefix.output_gain.take().unwrap();
        let Some(ToneStage::Emitted(mut stack)) = prefix.tones.take() else {
            panic!("emitted tones attached");
        };
        let hp_outputs = prefix.process_block(&inputs).unwrap();
        assert_ne!(
            prefix.input_gain().unwrap().current().to_bits(),
            before.input_gain().unwrap().current().to_bits()
        );
        assert_ne!(prefix.input_hp, before.input_hp);
        assert_ne!(prefix.pre, before.pre);
        assert_ne!(prefix.resampler, before.resampler);
        assert_ne!(
            format!("{:?}", prefix.neural),
            format!("{:?}", before.neural)
        );
        assert_ne!(prefix.post, before.post);
        assert_ne!(prefix.output_hp, before.output_hp);
        let late = V1ToneInstructionBudgets {
            third: 365,
            ..TONE_BUDGETS
        };
        let tone_error = Err(Error::Tone(ToneError::Simulation(
            SimulationError::StepBudget(365),
        )));
        let mut first = stack.section(V1Tone::First).clone();
        let mut second = stack.section(V1Tone::Second).clone();
        let first_outputs = first.process_block(&hp_outputs, 366).unwrap().outputs;
        second.process_block(&first_outputs, 366).unwrap();
        assert_ne!(&first, stack.section(V1Tone::First));
        assert_ne!(&second, stack.section(V1Tone::Second));
        assert_eq!(
            stack
                .clone()
                .process_block(&hp_outputs, late)
                .map(|b| b.outputs),
            tone_error
        );
        assert_eq!(
            path.process_block_with_tone_budgets(&inputs, late),
            tone_error
        );
        assert_same_state(&path, &before);

        let tone_outputs = stack
            .process_block(&hp_outputs, TONE_BUDGETS)
            .unwrap()
            .outputs;
        let gain_error = Err(Error::Gain(emitted::Error::Simulation(
            SimulationError::StepBudget(229),
        )));
        assert_eq!(
            gain.clone()
                .process_block(&tone_outputs, 229)
                .map(|b| b.outputs),
            gain_error
        );
        assert_eq!(
            path.process_block_with_output_gain_budget(&inputs, 229),
            gain_error
        );
        assert_same_state(&path, &before);

        let expected = gain.process_block(&tone_outputs, 230).unwrap().outputs;
        assert_eq!(
            path.process_block_with_tone_budgets(&inputs, TONE_BUDGETS)
                .unwrap()
                .map(f32::to_bits),
            expected.map(f32::to_bits)
        );
        let composed = prefix
            .with_emitted_tones(*stack)
            .unwrap()
            .with_output_gain(gain)
            .unwrap();
        assert_same_state(&path, &composed);
    }

    #[test]
    fn emitted_tone_controls_reset_and_reload_retain_variant_code_placement_and_parameters() {
        use crate::tones::V1Tone;
        let bytes = active_high_pass_record(1200.0);
        let record = RuntimeData::parse(&bytes, Limits::default()).unwrap();
        let source = "Owned emitted tone lifecycle";
        let prefix = V1PartialSignalPathReference::new(&record, table(1.0, source)).unwrap();
        let fresh = |normalized| prefix.clone().with_emitted_tones(emitted_tones(normalized));
        let mut path = fresh([0.9, 0.2, 0.75]).unwrap();
        let images = V1Tone::ALL.map(|tone| {
            path.emitted_tones()
                .unwrap()
                .section(tone)
                .compiled()
                .clone()
        });
        path.process_block(&[0.25; 32]).unwrap();
        path.process_block(&[-0.5; 32]).unwrap();
        let stack = |path: &V1PartialSignalPathReference| path.emitted_tones().unwrap().clone();
        let histories = |stack: &EmittedV1ToneStackReference| {
            V1Tone::ALL.map(|tone| stack.section(tone).history().map(f32::to_bits))
        };
        let advanced = histories(&stack(&path));
        assert_ne!(advanced, [[0; 4]; 3]);

        let before = path.clone();
        for invalid in [[0.5, 0.5, f32::NAN], [0.5, 2.0, 0.5]] {
            assert!(path.set_tone_parameters(invalid).is_err());
            assert_same_state(&path, &before);
        }
        // Valid update: coefficients change, histories and code stay.
        path.set_tone_parameters([0.1, 1.0, 0.0]).unwrap();
        let updated = stack(&path);
        assert_eq!(histories(&updated), advanced);
        assert_eq!(updated.normalized(), [0.1, 1.0, 0.0]);
        assert_ne!(
            updated.section(V1Tone::First).coefficients(),
            before
                .emitted_tones()
                .unwrap()
                .section(V1Tone::First)
                .coefficients()
        );

        path.reset().unwrap();
        let mut reset = updated.clone();
        reset.reset();
        let expected = fresh([0.1, 1.0, 0.0]).unwrap();
        assert_eq!(path.emitted_tones(), Some(&reset));
        assert_same_state(&path, &expected);

        path.process_block(&[0.25; 32]).unwrap();
        path.reload(&record, table(0.5, "Owned emitted tone reload"))
            .unwrap();
        assert!(path.tones().is_none());
        assert_eq!(path.emitted_tones(), Some(&reset));
        let reloaded =
            V1PartialSignalPathReference::new(&record, table(0.5, "Owned emitted tone reload"))
                .unwrap()
                .with_emitted_tones(emitted_tones([0.1, 1.0, 0.0]))
                .unwrap();
        assert_same_state(&path, &reloaded);
        for tone in V1Tone::ALL {
            assert_eq!(
                path.emitted_tones().unwrap().section(tone).compiled(),
                &images[tone as usize]
            );
        }
        // A rejected reload keeps the advanced emitted histories too.
        path.process_block(&[0.5; 32]).unwrap();
        let before = path.clone();
        let mut bad = bytes.clone();
        put(&mut bad, 0x80c, f32::NAN);
        let bad = RuntimeData::parse(&bad, Limits::default()).unwrap();
        assert!(path.reload(&bad, table(0.25, "Rejected")).is_err());
        assert_same_state(&path, &before);
    }
}

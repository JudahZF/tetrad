//! Owned REFERENCE of the complete fixed V1 stage ORDER whose output-producing float arithmetic,
//! alpha included, runs as raw ISA48 words:
//!
//! input gain → input HP → normalized pre FIR → 32→64 interpolator → per 32-sample
//! half: alpha scale then the ONE [`EmittedNeuralStack`] (histories carried from the
//! first half into the second) → 64→32 decimator → unnormalized post FIR → output HP
//! → tones → output gain.
//!
//! Separate from, and never a replacement for, [`super::V1PartialSignalPathReference`].
//! Every leaf call stages its own private logical DM and a default `ScalarState`:
//! NOT one linked resident image, CALL/ABI, physical memory alias, SIMD, DMA or
//! realtime proof. Host arithmetic is limited to configuration (validation, high-pass
//! coefficients, prefilter normalization, tone coefficients, neural seeds/baseline).
//! Alpha/beta are the serialized values (direct mathematical opt-in); stock log/exp
//! reconstruction equivalence is unknown. Not stock parity or hardware acceptance.
#![forbid(unsafe_code)]

use super::{V1StageInstructionBudgets, V1StagePlacements};
use crate::{
    filters::{
        EmittedV1FirReference, EmittedV1HighPassReference, HistoryUpdate, V1HighPassCoefficients,
        v1_high_pass_coefficients_reference,
    },
    gain::GainAdapter,
    network::{
        NetworkParameters,
        emitted_stack::{self, EmittedNeuralStack, LEAVES, LeafPlacement, LeafReceipt},
    },
    resampling::{
        CallerOwnedV1ResamplingTable,
        emitted::{
            self as resampling, EmittedV1Decimator, EmittedV1Interpolator,
            ResamplingLogicalPlacement,
        },
    },
    tones::emitted_stack::{EmittedV1ToneStackReference, V1ToneInstructionBudgets},
    v1::RuntimeData,
};
use tetrad_sharc::{
    EmitError, Instruction48, IsaProgram, Label, OwnedCode,
    address::AddressError,
    register::{DataReg, FloatReg, UniversalReg},
    simulator::{DmMemory, ScalarState, SimulationError, SimulationOutcome, run_scalar},
};

const N: usize = 32;

/// Which 32-sample half of the 64-sample high-rate block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Half {
    First,
    Second,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AlphaError {
    #[error(transparent)]
    Address(#[from] AddressError),
    #[error(transparent)]
    Emit(#[from] EmitError),
    #[error(transparent)]
    Simulation(#[from] SimulationError),
    #[error("alpha output sample {sample} is nonfinite")]
    NonFinite { sample: u32 },
    #[error("executed steps, store log or preserved registers differ from the image")]
    Receipt,
}

/// The failing stage with its own typed error. `half` is `None` while compiling.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("runtime data")]
    Record(#[source] crate::Error),
    #[error("input gain")]
    InputGain(#[source] crate::Error),
    #[error("input high-pass")]
    InputHighPass(#[source] crate::Error),
    #[error("pre FIR")]
    PreFir(#[source] crate::Error),
    #[error("interpolator")]
    Interpolate(#[source] resampling::Error),
    #[error("alpha scale ({half:?})")]
    Alpha {
        half: Option<Half>,
        #[source]
        error: AlphaError,
    },
    #[error("neural stack ({half:?})")]
    Neural {
        half: Option<Half>,
        #[source]
        error: emitted_stack::Error,
    },
    #[error("decimator")]
    Decimate(#[source] resampling::Error),
    #[error("post FIR")]
    PostFir(#[source] crate::Error),
    #[error("output high-pass")]
    OutputHighPass(#[source] crate::Error),
    #[error("tones")]
    Tones(#[source] crate::Error),
    #[error("output gain")]
    OutputGain(#[source] crate::Error),
}

/// Caller-selected logical placements of every image the path compiles. The gains
/// and tones carry their own placements. All may coincide: each call has private DM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmittedV1PathPlacements {
    pub stages: V1StagePlacements,
    pub interpolate: ResamplingLogicalPlacement,
    /// One image runs for both halves; DM input32+output32 (64 words).
    pub alpha: LeafPlacement,
    pub neural: [LeafPlacement; LEAVES],
    pub decimate: ResamplingLogicalPlacement,
}

/// Executed instruction limits for every leaf call of one frame, not cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmittedV1PathBudgets {
    pub input_gain: usize,
    pub stages: V1StageInstructionBudgets,
    pub interpolate: usize,
    pub alpha: [usize; 2],
    /// Per half, indexed by `emitted_stack::Leaf as usize`.
    pub neural: [[usize; LEAVES]; 2],
    pub decimate: usize,
    pub tones: V1ToneInstructionBudgets,
    pub output_gain: usize,
}

/// Actual decoder counts of every leaf call of one frame, in stage order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmittedV1PathReceipts {
    pub input_gain: SimulationOutcome,
    pub input_hp: SimulationOutcome,
    pub pre: SimulationOutcome,
    pub interpolate: SimulationOutcome,
    pub alpha: [SimulationOutcome; 2],
    pub neural: [[LeafReceipt; LEAVES]; 2],
    pub decimate: SimulationOutcome,
    pub post: SimulationOutcome,
    pub output_hp: SimulationOutcome,
    pub tones: [SimulationOutcome; 3],
    pub output_gain: SimulationOutcome,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmittedV1PathBlock {
    pub outputs: [f32; N],
    pub receipts: EmittedV1PathReceipts,
}

/// Straight-line `out[n] = alpha * in[n]`: `R1 = alpha` (immediate), then per
/// sample load R0, `F2 = F1 * F0`, store R2; RTS. 98 words = steps, 32 stores.
/// Inits every register it reads; clobbers only R0..R2 and ALU zero. Runs under
/// `run_scalar` (host f32): finite subnormal products are admitted, as in the
/// resampler before it; the neural stack's normal-only policy starts after it.
#[derive(Debug, Clone)]
struct AlphaScale {
    placement: LeafPlacement,
    alpha: f32,
    code: OwnedCode,
}

impl AlphaScale {
    fn compile(placement: LeafPlacement, alpha: f32) -> Result<Self, AlphaError> {
        let [input, scale, product] = [0, 1, 2].map(|i| DataReg::new(i).expect("R0..R2"));
        let at = |offset: usize| placement.dm_base.checked_add(offset as u32);
        at(2 * N - 1)?;
        let mut program = IsaProgram::new(placement.origin);
        program.define(Label::new("v1_alpha.entry"))?;
        program.push(Instruction48::Immediate {
            dst: UniversalReg::Data(scale),
            value: alpha.to_bits(),
        })?;
        for n in 0..N {
            program.push(Instruction48::DmDirectLoad {
                dst: input,
                address: at(n)?,
            })?;
            program.push(Instruction48::FloatMultiply {
                dst: FloatReg::from(product),
                lhs: FloatReg::from(scale),
                rhs: FloatReg::from(input),
            })?;
            program.push(Instruction48::DmDirectStore {
                src: product,
                address: at(N + n)?,
            })?;
        }
        program.push(Instruction48::Rts)?;
        Ok(Self {
            placement,
            alpha,
            code: program.finish()?,
        })
    }

    /// Every word executes once, so steps must equal the word count, the store log
    /// must be the 32 outputs in order and only R0..R2/ALU zero may change. A finite
    /// output proves a finite input and product (`alpha` is finite and normal).
    fn run(
        &self,
        input: &[f32; N],
        budget: usize,
    ) -> Result<([f32; N], SimulationOutcome), AlphaError> {
        let base = self.placement.dm_base;
        let mut words = Vec::with_capacity(2 * N);
        words.extend(input.map(f32::to_bits));
        words.resize(2 * N, 0);
        let mut memory = DmMemory::new(base, words)?;
        let mut state = ScalarState::default();
        let outcome = run_scalar(&self.code, &mut state, &mut memory, budget)?;
        let expected = (N..2 * N)
            .map(|n| base.checked_add(n as u32))
            .collect::<Result<Vec<_>, _>>()?;
        let mut untouched = ScalarState::default();
        untouched.data[..3].copy_from_slice(&state.data[..3]);
        untouched.alu_zero = state.alu_zero;
        if outcome.steps != self.code.word_count()
            || outcome.stores != N
            || memory.writes() != expected
            || state != untouched
        {
            return Err(AlphaError::Receipt);
        }
        let mut outputs = [0.0; N];
        for (n, output) in outputs.iter_mut().enumerate() {
            *output = f32::from_bits(memory.words()[N + n]);
            if !output.is_finite() {
                return Err(AlphaError::NonFinite { sample: n as u32 });
            }
        }
        Ok((outputs, outcome))
    }
}

/// Host-only configuration from one record. Outputs never come from here.
struct Config {
    high_pass: V1HighPassCoefficients,
    pre: [f32; 512],
    post: [f32; 2048],
    alpha: f32,
    parameters: NetworkParameters,
}

impl Config {
    /// Same admission as `SerializedMathematicalGainsCore::new`, without building
    /// the host neural core: 1→2→2→1 shape, runtime values, alpha > 0.5, beta >
    /// `0x3dcc_cccd` (0.1f32), active high-pass domain; beta is the scale factor.
    fn new(record: &RuntimeData<'_>) -> Result<Self, crate::Error> {
        if !record.layer_sizes().eq([1, 2, 2, 1]) {
            return Err(crate::Error::UnsupportedV1Shape);
        }
        record.validate_runtime_values()?;
        let [alpha, beta] = record.constant_gains();
        if alpha <= 0.5 || beta <= f32::from_bits(0x3dcc_cccd) {
            return Err(crate::Error::ConstantGainRange);
        }
        let [frequency, bandwidth] = record.high_pass();
        Ok(Self {
            high_pass: v1_high_pass_coefficients_reference(frequency, bandwidth)?,
            pre: std::array::from_fn(|i| record.pre_coefficients().get(i).expect("512 taps")),
            post: std::array::from_fn(|i| record.post_coefficients().get(i).expect("2048 taps")),
            alpha,
            parameters: record.network_parameters(beta)?,
        })
    }
}

/// The complete V1 stage order with every output-producing operation emitted.
/// Owns all state; nothing mutable is exposed. Not a realtime or heap-free API.
#[derive(Debug, Clone)]
pub struct V1EmittedSignalPathReference {
    input_gain: GainAdapter,
    input_hp: EmittedV1HighPassReference,
    pre: EmittedV1FirReference<512>,
    interpolator: EmittedV1Interpolator,
    alpha: AlphaScale,
    neural: EmittedNeuralStack,
    decimator: EmittedV1Decimator,
    post: EmittedV1FirReference<2048>,
    output_hp: EmittedV1HighPassReference,
    tones: EmittedV1ToneStackReference,
    output_gain: GainAdapter,
}

impl V1EmittedSignalPathReference {
    /// Construction is the explicit direct-serialized-gains opt-in. No default
    /// table, placement, gain state or tone parameter exists. Gains keep the
    /// caller's current/target; tone parameters stay and tone histories start at zero.
    pub fn new(
        record: &RuntimeData<'_>,
        table: CallerOwnedV1ResamplingTable,
        placements: EmittedV1PathPlacements,
        input_gain: GainAdapter,
        mut tones: EmittedV1ToneStackReference,
        output_gain: GainAdapter,
    ) -> Result<Self, Error> {
        let config = Config::new(record).map_err(Error::Record)?;
        let stages = placements.stages;
        tones.reset();
        Ok(Self {
            input_gain,
            input_hp: EmittedV1HighPassReference::new(config.high_pass, stages.input_hp)
                .map_err(Error::InputHighPass)?,
            pre: EmittedV1FirReference::from_imported_prefilter(&config.pre, stages.pre)
                .map_err(Error::PreFir)?,
            interpolator: EmittedV1Interpolator::new(table.clone(), placements.interpolate)
                .map_err(Error::Interpolate)?,
            alpha: AlphaScale::compile(placements.alpha, config.alpha)
                .map_err(|error| Error::Alpha { half: None, error })?,
            neural: EmittedNeuralStack::new(config.parameters, placements.neural)
                .map_err(|error| Error::Neural { half: None, error })?,
            decimator: EmittedV1Decimator::new(table, placements.decimate)
                .map_err(Error::Decimate)?,
            post: EmittedV1FirReference::from_postfilter(config.post, stages.post)
                .map_err(Error::PostFir)?,
            output_hp: EmittedV1HighPassReference::new(config.high_pass, stages.output_hp)
                .map_err(Error::OutputHighPass)?,
            tones,
            output_gain,
        })
    }

    pub fn input_gain(&self) -> &GainAdapter {
        &self.input_gain
    }
    pub fn input_high_pass(&self) -> &EmittedV1HighPassReference {
        &self.input_hp
    }
    pub fn pre_fir(&self) -> &EmittedV1FirReference<512> {
        &self.pre
    }
    pub fn interpolator(&self) -> &EmittedV1Interpolator {
        &self.interpolator
    }
    /// Serialized alpha bound as the scale image's immediate.
    pub fn alpha(&self) -> f32 {
        self.alpha.alpha
    }
    pub fn alpha_code(&self) -> &OwnedCode {
        &self.alpha.code
    }
    pub fn neural(&self) -> &EmittedNeuralStack {
        &self.neural
    }
    pub fn decimator(&self) -> &EmittedV1Decimator {
        &self.decimator
    }
    pub fn post_fir(&self) -> &EmittedV1FirReference<2048> {
        &self.post
    }
    pub fn output_high_pass(&self) -> &EmittedV1HighPassReference {
        &self.output_hp
    }
    pub fn tones(&self) -> &EmittedV1ToneStackReference {
        &self.tones
    }
    pub fn output_gain(&self) -> &GainAdapter {
        &self.output_gain
    }
    /// The explicit caller table; interpolator and decimator bind the same one.
    pub fn table(&self) -> &CallerOwnedV1ResamplingTable {
        self.interpolator.table()
    }

    /// Exact successful budgets derived from the bound images. ADAA leaves get
    /// their all-quotient maximum; every other leaf is straight-line.
    pub fn full_budgets(&self) -> EmittedV1PathBudgets {
        let neural = self.neural.full_budgets();
        EmittedV1PathBudgets {
            input_gain: self.input_gain.compiled().max_steps(),
            stages: V1StageInstructionBudgets {
                input_hp: self.input_hp.compiled().max_steps(),
                pre: self.pre.compiled().executed_steps(),
                post: self.post.compiled().executed_steps(),
                output_hp: self.output_hp.compiled().max_steps(),
            },
            interpolate: self.interpolator.compiled().max_steps(),
            alpha: [self.alpha.code.word_count(); 2],
            neural: [neural; 2],
            decimate: self.decimator.compiled().max_steps(),
            tones: self.tones.full_budgets(),
            output_gain: self.output_gain.compiled().max_steps(),
        }
    }

    /// Validated; current gain, code, placement and every history are retained.
    pub fn set_input_gain_target(&mut self, target: f32) -> Result<(), Error> {
        self.input_gain
            .set_linear_target(target)
            .map_err(Error::InputGain)
    }

    /// Validated; current gain, code, placement and every history are retained.
    pub fn set_output_gain_target(&mut self, target: f32) -> Result<(), Error> {
        self.output_gain
            .set_linear_target(target)
            .map_err(Error::OutputGain)
    }

    /// Block-boundary update as at updater `0x178`: all three or none; histories stay.
    pub fn set_tone_parameters(&mut self, normalized: [f32; 3]) -> Result<(), Error> {
        self.tones.set_normalized(normalized).map_err(Error::Tones)
    }

    /// Zero every filter, resampler and tone history and restore the five neural
    /// seeds. Gain current/target/code, tone parameters, table and all images stay.
    /// Owned policy, not a recovered stock reset.
    pub fn reset(&mut self) {
        self.input_hp.reset();
        self.pre.reset();
        self.interpolator.reset();
        self.neural.reset();
        self.decimator.reset();
        self.post.reset();
        self.output_hp.reset();
        self.tones.reset();
    }

    /// Atomic full-context replacement at the retained placements, ending reset.
    /// FIR taps rebind to the existing images; high-pass images recompile only if
    /// bits change; alpha, neural and resampling images are rebuilt. Gains and tone
    /// parameters/images stay. Any error retains the whole path. Not a stock reload.
    pub fn reload(
        &mut self,
        record: &RuntimeData<'_>,
        table: CallerOwnedV1ResamplingTable,
    ) -> Result<(), Error> {
        let config = Config::new(record).map_err(Error::Record)?;
        let mut next = self.clone();
        next.reset();
        next.input_hp
            .reload(config.high_pass, HistoryUpdate::Reset)
            .map_err(Error::InputHighPass)?;
        next.pre
            .reload_imported_prefilter(&config.pre, HistoryUpdate::Reset)
            .map_err(Error::PreFir)?;
        next.interpolator
            .reload(table.clone(), HistoryUpdate::Reset)
            .map_err(Error::Interpolate)?;
        next.alpha = AlphaScale::compile(self.alpha.placement, config.alpha)
            .map_err(|error| Error::Alpha { half: None, error })?;
        next.neural
            .reload(config.parameters)
            .map_err(|error| Error::Neural { half: None, error })?;
        next.decimator
            .reload(table, HistoryUpdate::Reset)
            .map_err(Error::Decimate)?;
        next.post
            .reload_prepared(config.post, HistoryUpdate::Reset)
            .map_err(Error::PostFir)?;
        next.output_hp
            .reload(config.high_pass, HistoryUpdate::Reset)
            .map_err(Error::OutputHighPass)?;
        *self = next;
        Ok(())
    }

    /// Runs every leaf in stage order on a whole-frame clone with explicit budgets
    /// and commits only after the output gain succeeds; any error retains the whole
    /// path and returns no output. Allocates per leaf; OOM is outside this Result.
    pub fn process_block(
        &mut self,
        inputs: &[f32; N],
        budgets: EmittedV1PathBudgets,
    ) -> Result<EmittedV1PathBlock, Error> {
        let mut next = self.clone();
        let input_gain = next
            .input_gain
            .process_block(inputs, budgets.input_gain)
            .map_err(Error::InputGain)?;
        let input_hp = next
            .input_hp
            .process_block(&input_gain.outputs, budgets.stages.input_hp)
            .map_err(Error::InputHighPass)?;
        let pre = next
            .pre
            .process_block(&input_hp.outputs, budgets.stages.pre)
            .map_err(Error::PreFir)?;
        let interpolated = next
            .interpolator
            .process_block(&pre.outputs, budgets.interpolate)
            .map_err(Error::Interpolate)?;
        let mut run_half = |half: Half| {
            let index = half as usize;
            let input = interpolated.outputs[N * index..][..N]
                .try_into()
                .expect("32 of 64");
            let (scaled, alpha) = next
                .alpha
                .run(input, budgets.alpha[index])
                .map_err(|error| Error::Alpha {
                    half: Some(half),
                    error,
                })?;
            let block = next
                .neural
                .process_block(&scaled, &budgets.neural[index])
                .map_err(|error| Error::Neural {
                    half: Some(half),
                    error,
                })?;
            Ok::<_, Error>((block.outputs, alpha, block.receipts))
        };
        let first = run_half(Half::First)?;
        let second = run_half(Half::Second)?;
        let high_rate = std::array::from_fn(|n| if n < N { first.0[n] } else { second.0[n - N] });
        let decimated = next
            .decimator
            .process_block(&high_rate, budgets.decimate)
            .map_err(Error::Decimate)?;
        let post = next
            .post
            .process_block(&decimated.outputs, budgets.stages.post)
            .map_err(Error::PostFir)?;
        let output_hp = next
            .output_hp
            .process_block(&post.outputs, budgets.stages.output_hp)
            .map_err(Error::OutputHighPass)?;
        let tones = next
            .tones
            .process_block(&output_hp.outputs, budgets.tones)
            .map_err(Error::Tones)?;
        let output_gain = next
            .output_gain
            .process_block(&tones.outputs, budgets.output_gain)
            .map_err(Error::OutputGain)?;
        *self = next;
        Ok(EmittedV1PathBlock {
            outputs: output_gain.outputs,
            receipts: EmittedV1PathReceipts {
                input_gain: input_gain.receipt,
                input_hp: input_hp.receipt,
                pre: pre.receipt,
                interpolate: interpolated.receipt,
                alpha: [first.1, second.1],
                neural: [first.2, second.2],
                decimate: decimated.receipt,
                post: post.receipt,
                output_hp: output_hp.receipt,
                tones: tones.receipts,
                output_gain: output_gain.receipt,
            },
        })
    }
}

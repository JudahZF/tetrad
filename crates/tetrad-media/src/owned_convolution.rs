//! Owned offline mono convolution of explicitly selected IrSamples.
//! Admission validates the nonempty finite selected prefix, 1..=1024 words,
//! not the whole source. Raw rate/channel metadata is unknown; the caller must
//! match the coefficient rate to its input. No factory decode, authentication,
//! normalization, resampling or stock ABI is supplied.
//! Tap0 is current input. Reduction visits N-1 down to0 from +0 with separate
//! MUL/ADD. All 32 outputs use 355 ISA48 words (2130 bytes) and execute
//! 3+32*(6*N+5) instructions, at most196771; these are not hardware cycles.
//! Checked adjacent DM spans are window N+31, coefficients N and output32,
//! totaling2*N+63<=2111 words; code uses checked PM24 endpoints. Immutable code,
//! layout and exact coefficient bits remain privately bound. Chronological N-1
//! history commits window[32..] only after all32 outputs are finite. Initial
//! history is explicitly Zero or Samples. Preserve reload requires equal count;
//! Reset zeros history. Every Result failure retains old state; empty IR refuses.
//! Per-block allocation gives no realtime, OOM transaction or hardware guarantee.
//! Host finite-output reasoning does not establish physical SHARC rounding.
//! Active scalar banks, RND32/cleared unsupported modes, L0/L1=0 and call context
//! are hardware caller premises. The stock UnsupportedIrOperation::IrConvolution
//! remains in ir because stock compatibility is unverified.
use crate::ir::{IrSamples, MAX_IR_SAMPLES};
use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::emit::{BranchRelocation, EmitError, IsaProgram, Label, OwnedCode};
use tetrad_sharc::isa48::{BranchKind, Condition, DmAccess, Instruction48};
use tetrad_sharc::register::{Dag1Index, Dag1Modify, DataReg, UniversalReg};
use tetrad_sharc::simulator::{DmMemory, ScalarState, SimulationError, run_scalar};

pub const SAMPLES: usize = 32;
pub const CODE_WORDS: usize = 355;
pub const MAX_SCRATCH_WORDS: usize = 2 * MAX_IR_SAMPLES + 63;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("owned convolution requires 1 through 1024 selected IR words, got {0}")]
    Length(usize),
    #[error("IR coefficient {0} must be finite")]
    NonFiniteCoefficient(usize),
    #[error("history needs {expected} samples, got {actual}")]
    HistoryLength { expected: usize, actual: usize },
    #[error("history sample {0} must be finite")]
    NonFiniteHistory(usize),
    #[error("input sample {0} must be finite")]
    NonFiniteInput(usize),
    #[error("convolution output sample {0} is not finite")]
    NumericOverflow(usize),
    #[error("preserving history requires unchanged coefficient count")]
    PreserveLength,
    #[error(transparent)]
    Address(#[from] AddressError),
    #[error(transparent)]
    Emit(#[from] EmitError),
    #[error(transparent)]
    Simulation(#[from] SimulationError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryUpdate {
    Preserve,
    Reset,
}

#[derive(Debug, Clone, Copy)]
pub enum InitialHistory<'a> {
    Zero,
    Samples(&'a [f32]),
}

/// Compact logical DM placement. Three adjacent ranges cannot overlap or create gaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    input: DmWordAddress32,
    coefficients: DmWordAddress32,
    output: DmWordAddress32,
    count: u32,
}

impl Layout {
    pub fn input(self) -> DmWordAddress32 {
        self.input
    }
    pub fn coefficients(self) -> DmWordAddress32 {
        self.coefficients
    }
    pub fn output(self) -> DmWordAddress32 {
        self.output
    }
    pub fn count(self) -> u32 {
        self.count
    }
    pub fn scratch_words(self) -> u32 {
        2 * self.count + 63
    }
}

/// Code, placement and exact finite coefficients remain privately bound.
#[derive(Debug)]
pub struct CompiledConvolution {
    code: OwnedCode,
    layout: Layout,
    coefficients: Vec<u32>,
}

impl CompiledConvolution {
    pub fn code(&self) -> &OwnedCode {
        &self.code
    }
    pub fn layout(&self) -> Layout {
        self.layout
    }
    pub fn coefficient_bits(&self) -> &[u32] {
        &self.coefficients
    }
    /// Executed instructions including RTS, not SHARC cycles or measured time.
    pub fn executed_steps(&self) -> usize {
        3 + SAMPLES * (6 * self.coefficients.len() + 5)
    }
}

/// Natural tap0 applies to current input; the owned reduction visits N-1 down to0.
/// Active scalar banks, RND32/clear modes, L0/L1=0 and call environment are ABI premises.
/// Clobbers R0/R1/R4/R9, I0/I1, M0/M1 and arithmetic flags. No stock startup is called.
pub fn compile(
    samples: &IrSamples,
    origin: PmWordAddress24,
    input: DmWordAddress32,
) -> Result<CompiledConvolution, Error> {
    let bits = samples.bits();
    if bits.is_empty() || bits.len() > MAX_IR_SAMPLES {
        return Err(Error::Length(bits.len()));
    }
    for (index, &word) in bits.iter().enumerate() {
        if !f32::from_bits(word).is_finite() {
            return Err(Error::NonFiniteCoefficient(index));
        }
    }
    let count = bits.len() as u32;
    let coefficients = input.checked_add(count + 31)?;
    let output = coefficients.checked_add(count)?;
    output.checked_add(SAMPLES as u32)?;
    let layout = Layout {
        input,
        coefficients,
        output,
        count,
    };
    let [r0, r1, r4, r9] = [0, 1, 4, 9].map(|n| DataReg::new(n).expect("fixed data register"));
    let [i0, i1] = [0, 1].map(|n| Dag1Index::new(n).expect("fixed index register"));
    let [m0, m1] = [0, 1].map(|n| Dag1Modify::new(n).expect("fixed modifier register"));
    let mut program = IsaProgram::new(origin);
    program.define(Label::new("convolution.entry"))?;
    for (modifier, value) in [(m0, 1), (m1, u32::MAX)] {
        program.push(Instruction48::Immediate {
            dst: UniversalReg::Modify(modifier),
            value,
        })?;
    }
    for sample in 0..SAMPLES as u32 {
        let body = Label::new(format!("convolution.output.{sample}.loop"));
        for (dst, value) in [
            (UniversalReg::Index(i0), input.checked_add(sample)?.get()),
            (
                UniversalReg::Index(i1),
                coefficients.checked_add(count - 1)?.get(),
            ),
            (UniversalReg::Data(r1), 0.0_f32.to_bits()),
            (UniversalReg::Data(r9), count),
        ] {
            program.push(Instruction48::Immediate { dst, value })?;
        }
        program.define(body.clone())?;
        for (dst, index, modifier) in [(r0, i0, m0), (r4, i1, m1)] {
            program.push(Instruction48::DmLoad {
                dst,
                index,
                modifier,
                access: DmAccess::PostModify,
            })?;
        }
        program.push(Instruction48::FloatMultiply {
            dst: r0.into(),
            lhs: r0.into(),
            rhs: r4.into(),
        })?;
        program.push(Instruction48::FloatAdd {
            dst: r1.into(),
            lhs: r1.into(),
            rhs: r0.into(),
        })?;
        program.push(Instruction48::IntegerDecrement { dst: r9, src: r9 })?;
        program.branch_to(
            BranchKind::Jump,
            Condition::NotEqual,
            body,
            BranchRelocation::PcRel24,
        )?;
        program.push(Instruction48::DmDirectStore {
            src: r1,
            address: output.checked_add(sample)?,
        })?;
    }
    program.define(Label::new("convolution.return"))?;
    program.push(Instruction48::Rts)?;
    Ok(CompiledConvolution {
        code: program.finish()?,
        layout,
        coefficients: bits.to_vec(),
    })
}

#[derive(Debug, PartialEq)]
pub struct ProcessedBlock {
    pub samples: [f32; SAMPLES],
    pub steps: usize,
    pub stores: usize,
}

/// Offline host processor with chronological N-1 history. Construction and callbacks allocate.
/// Coefficient rate must match the caller's input stream; raw IR words provide no rate metadata.
#[derive(Debug)]
pub struct Convolver {
    compiled: CompiledConvolution,
    history: Vec<f32>,
}

impl Convolver {
    pub fn new(
        samples: &IrSamples,
        origin: PmWordAddress24,
        input: DmWordAddress32,
        history: InitialHistory<'_>,
    ) -> Result<Self, Error> {
        let compiled = compile(samples, origin, input)?;
        let expected = compiled.coefficients.len() - 1;
        let history = match history {
            InitialHistory::Zero => vec![0.0; expected],
            InitialHistory::Samples(values) => {
                if values.len() != expected {
                    return Err(Error::HistoryLength {
                        expected,
                        actual: values.len(),
                    });
                }
                for (index, value) in values.iter().enumerate() {
                    if !value.is_finite() {
                        return Err(Error::NonFiniteHistory(index));
                    }
                }
                values.to_vec()
            }
        };
        Ok(Self { compiled, history })
    }
    pub fn compiled(&self) -> &CompiledConvolution {
        &self.compiled
    }
    pub fn history(&self) -> &[f32] {
        &self.history
    }
    pub fn reset(&mut self) {
        self.history.fill(0.0);
    }
    /// Compile first; any failure retains old code, coefficients and history.
    pub fn reload(&mut self, samples: &IrSamples, update: HistoryUpdate) -> Result<(), Error> {
        let next = compile(
            samples,
            self.compiled.code.origin(),
            self.compiled.layout.input,
        )?;
        if update == HistoryUpdate::Preserve
            && next.coefficients.len() != self.compiled.coefficients.len()
        {
            return Err(Error::PreserveLength);
        }
        if update == HistoryUpdate::Reset {
            self.history = vec![0.0; next.coefficients.len() - 1];
        }
        self.compiled = next;
        Ok(())
    }
    /// Executes actual immutable ISA48 words. Failure discards local scratch and retains history.
    /// Every multiply/add overflow propagates to its final sum; no FMA or recovery substitution.
    /// Finite underflow follows host IEEE arithmetic, not proved physical SHARC behavior.
    pub fn process_block(
        &mut self,
        input: &[f32; SAMPLES],
        step_budget: usize,
    ) -> Result<ProcessedBlock, Error> {
        for (index, value) in input.iter().enumerate() {
            if !value.is_finite() {
                return Err(Error::NonFiniteInput(index));
            }
        }
        let layout = self.compiled.layout;
        let mut words = Vec::with_capacity(layout.scratch_words() as usize);
        words.extend(self.history.iter().map(|v| v.to_bits()));
        words.extend(input.iter().map(|v| v.to_bits()));
        words.extend_from_slice(&self.compiled.coefficients);
        words.resize(layout.scratch_words() as usize, 0);
        let mut memory = DmMemory::new(layout.input, words)?;
        let mut state = ScalarState::default();
        let outcome = run_scalar(
            &self.compiled.code,
            &mut state,
            &mut memory,
            step_budget.min(self.compiled.executed_steps()),
        )?;
        let mut samples = [0.0; SAMPLES];
        for (index, value) in samples.iter_mut().enumerate() {
            *value = f32::from_bits(memory.read(layout.output.checked_add(index as u32)?)?);
            if !value.is_finite() {
                return Err(Error::NumericOverflow(index));
            }
        }
        let start = SAMPLES;
        for (value, &word) in self
            .history
            .iter_mut()
            .zip(&memory.words()[start..start + self.compiled.coefficients.len() - 1])
        {
            *value = f32::from_bits(word);
        }
        Ok(ProcessedBlock {
            samples,
            steps: outcome.steps,
            stores: memory.writes().len(),
        })
    }
}

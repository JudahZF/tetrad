//! Owned scalar ISA48 affine leaf for the recovered one-output V2 shape.
//! Logical DM placement and finite coefficients are bound to immutable code.
//! No stock startup, runtime, callback, hardware or numerical-parity claim.

#![forbid(unsafe_code)]
#![deny(warnings)]

use super::AffineRecord;
use tetrad_sharc::{
    address::{AddressError, DmWordAddress32, PmWordAddress24},
    emit::{BranchRelocation, EmitError, IsaProgram, Label, OwnedCode},
    isa48::{BranchKind, Condition, DmAccess, Instruction48, MODE1_RND32, MODE1_SCALAR_CLEAR},
    register::{Dag1Index, Dag1Length, Dag1Modify, DataReg, UniversalReg},
    simulator::{DmMemory, ScalarState, SimulationError, SimulationOutcome, run_scalar},
};

pub const SAMPLES: u32 = 32;
/// Owned resource policy, not a recovered stock dimension limit.
pub const MAX_INPUTS: usize = 16_384;
pub const CODE_WORDS: usize = 354;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("only O=1 and 4<=N<=16384 divisible by four is supported")]
    UnsupportedShape,
    #[error(transparent)]
    Address(#[from] AddressError),
    #[error(transparent)]
    Emit(#[from] EmitError),
    #[error(transparent)]
    Simulation(#[from] SimulationError),
    #[error("affine input, coefficients, bias and output ranges must be disjoint")]
    DmOverlap,
    #[error("nonfinite coefficient at index {0} (N denotes the bias)")]
    NonFiniteCoefficient(usize),
    #[error("coefficient at logical DM address {0:#x} differs from the compiled tensor")]
    CoefficientMismatch(u32),
    #[error("nonfinite input at logical DM address {0:#x}")]
    NonFiniteInput(u32),
    #[error("nonfinite computed output at sample {0}")]
    NumericOverflow(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    inputs: u32,
    input: DmWordAddress32,
    weights: DmWordAddress32,
    bias: DmWordAddress32,
    output: DmWordAddress32,
}

impl Layout {
    pub fn inputs(self) -> u32 {
        self.inputs
    }
    pub fn input(self) -> DmWordAddress32 {
        self.input
    }
    pub fn weights(self) -> DmWordAddress32 {
        self.weights
    }
    pub fn bias(self) -> DmWordAddress32 {
        self.bias
    }
    pub fn output(self) -> DmWordAddress32 {
        self.output
    }

    fn ranges(self) -> [(DmWordAddress32, u32); 4] {
        [
            (self.input, self.inputs * SAMPLES),
            (self.weights, self.inputs),
            (self.bias, 1),
            (self.output, SAMPLES),
        ]
    }
}

macro_rules! reg {
    ($class:ident, $index:literal) => {
        match $class::new($index) {
            Ok(register) => register,
            Err(_) => panic!("fixed ABI register outside class"),
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Abi {
    pub clobbered_data: [DataReg; 4],
    pub clobbered_indices: [Dag1Index; 2],
    pub clobbered_modifiers: [Dag1Modify; 1],
    pub zero_lengths: [Dag1Length; 2],
    pub clobbers_arithmetic_flags: bool,
    pub required_mode1_set: u32,
    pub required_mode1_clear: u32,
}

pub const ABI: Abi = Abi {
    clobbered_data: [
        reg!(DataReg, 0),
        reg!(DataReg, 1),
        reg!(DataReg, 4),
        reg!(DataReg, 9),
    ],
    clobbered_indices: [reg!(Dag1Index, 0), reg!(Dag1Index, 1)],
    clobbered_modifiers: [reg!(Dag1Modify, 0)],
    zero_lengths: [reg!(Dag1Length, 0), reg!(Dag1Length, 1)],
    clobbers_arithmetic_flags: true,
    required_mode1_set: MODE1_RND32,
    required_mode1_clear: MODE1_SCALAR_CLEAR,
};

/// Code, placement and tensor form one immutable compilation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledAffine {
    code: OwnedCode,
    layout: Layout,
    weights: Vec<u32>,
    bias: u32,
}

impl CompiledAffine {
    pub fn code(&self) -> &OwnedCode {
        &self.code
    }
    pub fn layout(&self) -> Layout {
        self.layout
    }
    pub fn weight_bits(&self) -> &[u32] {
        &self.weights
    }
    pub fn bias_bits(&self) -> u32 {
        self.bias
    }
    /// Executed scalar instructions including RTS; not processor cycles.
    pub fn executed_steps(&self) -> usize {
        2 + SAMPLES as usize * (6 * self.layout.inputs as usize + 5)
    }
}

/// Sample-major contiguous input vectors, ascending coefficients, separate MUL/ADD.
/// Establishes pointers/modifier/count but not numerical modes or circular lengths.
/// All ranges and their one-past endpoints must fit normal-word u32 addresses.
pub fn compile_affine(
    origin: PmWordAddress24,
    record: &AffineRecord<'_>,
    input: DmWordAddress32,
    weights: DmWordAddress32,
    bias: DmWordAddress32,
    output: DmWordAddress32,
) -> Result<CompiledAffine, Error> {
    let (n, o) = record.dimensions();
    if o != 1 || !(4..=MAX_INPUTS).contains(&n) || !n.is_multiple_of(4) {
        return Err(Error::UnsupportedShape);
    }
    let layout = Layout {
        inputs: n as u32,
        input,
        weights,
        bias,
        output,
    };
    let ranges = layout.ranges();
    for (index, &(start, words)) in ranges.iter().enumerate() {
        let end = start.checked_add(words)?;
        for &(other_start, other_words) in &ranges[index + 1..] {
            let other_end = other_start.checked_add(other_words)?;
            if start < other_end && other_start < end {
                return Err(Error::DmOverlap);
            }
        }
    }
    let tensor: Vec<u32> = record.weights().map(f32::to_bits).collect();
    let bias_bits = record
        .biases()
        .next()
        .expect("parsed nonzero outputs")
        .to_bits();
    for (index, &bits) in tensor.iter().chain(std::iter::once(&bias_bits)).enumerate() {
        if !f32::from_bits(bits).is_finite() {
            return Err(Error::NonFiniteCoefficient(index));
        }
    }
    let [r0, r1, r4, r9] = ABI.clobbered_data;
    let [i0, i1] = ABI.clobbered_indices;
    let [m0] = ABI.clobbered_modifiers;
    let mut program = IsaProgram::new(origin);
    program.define(Label::new("affine.entry"))?;
    program.push(Instruction48::Immediate {
        dst: UniversalReg::Modify(m0),
        value: 1,
    })?;
    for sample in 0..SAMPLES {
        let body = Label::new(format!("affine.sample.{sample}.loop"));
        for (dst, value) in [
            (
                UniversalReg::Index(i0),
                input.checked_add(sample * layout.inputs)?.get(),
            ),
            (UniversalReg::Index(i1), weights.get()),
            (UniversalReg::Data(r9), layout.inputs),
        ] {
            program.push(Instruction48::Immediate { dst, value })?;
        }
        program.push(Instruction48::DmDirectLoad {
            dst: r1,
            address: bias,
        })?;
        program.define(body.clone())?;
        for (dst, index) in [(r0, i0), (r4, i1)] {
            program.push(Instruction48::DmLoad {
                dst,
                index,
                modifier: m0,
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
    program.define(Label::new("affine.return"))?;
    program.push(Instruction48::Rts)?;
    Ok(CompiledAffine {
        code: program.finish()?,
        layout,
        weights: tensor,
        bias: bias_bits,
    })
}

/// Atomic offline simulation. Caller supplies the exact compiled coefficient bits
/// at their declared addresses. Rejects wrong modes, lengths, inputs and tensors.
/// Stages raw simulator writes and registers; commits only finite complete output.
/// This is a host f32 mathematical model, not a SHARC exceptional-number oracle.
pub fn simulate_affine(
    compiled: &CompiledAffine,
    state: &mut ScalarState,
    memory: &mut DmMemory,
    step_budget: usize,
) -> Result<SimulationOutcome, Error> {
    let layout = compiled.layout;
    for (base, words) in layout.ranges() {
        memory.validate_range(base, words)?;
    }
    if state.mode1 & ABI.required_mode1_set != ABI.required_mode1_set
        || state.mode1 & ABI.required_mode1_clear != 0
    {
        return Err(SimulationError::Modes(state.mode1).into());
    }
    for length in ABI.zero_lengths {
        let index = usize::from(length.index());
        if state.lengths[index] != 0 {
            return Err(SimulationError::CircularLength {
                index,
                length: state.lengths[index],
            }
            .into());
        }
    }
    for (offset, &bits) in compiled.weights.iter().enumerate() {
        let address = layout.weights.checked_add(offset as u32)?;
        if memory.read(address)? != bits {
            return Err(Error::CoefficientMismatch(address.get()));
        }
    }
    if memory.read(layout.bias)? != compiled.bias {
        return Err(Error::CoefficientMismatch(layout.bias.get()));
    }
    for offset in 0..layout.inputs * SAMPLES {
        let address = layout.input.checked_add(offset)?;
        if !f32::from_bits(memory.read(address)?).is_finite() {
            return Err(Error::NonFiniteInput(address.get()));
        }
    }
    let mut next_state = state.clone();
    let mut next_memory = memory.clone();
    let outcome = run_scalar(
        &compiled.code,
        &mut next_state,
        &mut next_memory,
        step_budget,
    )?;
    for sample in 0..SAMPLES {
        if !f32::from_bits(next_memory.read(layout.output.checked_add(sample)?)?).is_finite() {
            return Err(Error::NumericOverflow(sample));
        }
    }
    *state = next_state;
    *memory = next_memory;
    Ok(outcome)
}

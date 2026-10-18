//! Owned fixed-32 FIR leaf. Logical DM placement is explicit and bound to code.
use crate::address::{AddressError, DmWordAddress32, PmWordAddress24};
use crate::emit::{BranchRelocation, EmitError, IsaProgram, Label, OwnedCode};
use crate::isa48::{
    BranchKind, Condition, DmAccess, Instruction48, MODE1_RND32, MODE1_SCALAR_CLEAR,
};
use crate::register::{Dag1Index, Dag1Length, Dag1Modify, DataReg, UniversalReg};
use crate::simulator::{
    DmMemory, ScalarState, SimulationError, SimulationOutcome, run_scalar_checked_floats,
};

pub const FIR_OUTPUT_SAMPLES: u32 = 32;
pub const FIR_WORD_COUNT: usize = 355;

/// Only the two recovered V1 lengths; taps are caller-owned and already prepared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirLength {
    Fir512,
    Fir2048,
}

impl FirLength {
    pub const fn taps(self) -> u32 {
        match self {
            Self::Fir512 => 512,
            Self::Fir2048 => 2048,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FirError {
    #[error(transparent)]
    Address(#[from] AddressError),
    #[error(transparent)]
    Emit(#[from] EmitError),
    #[error(transparent)]
    Simulation(#[from] SimulationError),
    #[error("FIR input, taps and output ranges must be disjoint")]
    DmOverlap,
    #[error("FIR tap base must be at least one for the final descending index")]
    TapBaseZero,
    #[error("FIR operand at logical DM address {address:#x} must be finite")]
    NonFiniteOperand { address: u32 },
}

/// N-1 chronological prior samples followed by 32 current samples; natural taps.
/// All exclusive ends and post-modify endpoints must fit u32. No physical map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirLayout {
    length: FirLength,
    input: DmWordAddress32,
    taps: DmWordAddress32,
    output: DmWordAddress32,
}

impl FirLayout {
    pub fn new(
        length: FirLength,
        input: DmWordAddress32,
        taps: DmWordAddress32,
        output: DmWordAddress32,
    ) -> Result<Self, FirError> {
        if taps.get() == 0 {
            return Err(FirError::TapBaseZero);
        }
        let ranges = [
            (
                input,
                input.checked_add(length.taps() + FIR_OUTPUT_SAMPLES - 1)?,
            ),
            (taps, taps.checked_add(length.taps())?),
            (output, output.checked_add(FIR_OUTPUT_SAMPLES)?),
        ];
        for (index, &(start, end)) in ranges.iter().enumerate() {
            for &(other_start, other_end) in &ranges[index + 1..] {
                if start < other_end && other_start < end {
                    return Err(FirError::DmOverlap);
                }
            }
        }
        Ok(Self {
            length,
            input,
            taps,
            output,
        })
    }

    pub fn length(self) -> FirLength {
        self.length
    }
    pub fn input(self) -> DmWordAddress32 {
        self.input
    }
    pub fn taps(self) -> DmWordAddress32 {
        self.taps
    }
    pub fn output(self) -> DmWordAddress32 {
        self.output
    }
    pub fn input_words(self) -> u32 {
        self.length.taps() + FIR_OUTPUT_SAMPLES - 1
    }
}

/// Active-bank scalar requirements; the leaf establishes pointers and modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirAbi {
    pub clobbered_data: [DataReg; 4],
    pub clobbered_indices: [Dag1Index; 2],
    pub clobbered_modifiers: [Dag1Modify; 2],
    pub zero_lengths: [Dag1Length; 2],
    pub clobbers_arithmetic_flags: bool,
    pub required_mode1_set: u32,
    pub required_mode1_clear: u32,
    pub output_samples: u32,
}

pub const FIR_ABI: FirAbi = FirAbi {
    clobbered_data: [
        DataReg::of(0),
        DataReg::of(1),
        DataReg::of(4),
        DataReg::of(9),
    ],
    clobbered_indices: [Dag1Index::of(0), Dag1Index::of(1)],
    clobbered_modifiers: [Dag1Modify::of(0), Dag1Modify::of(1)],
    zero_lengths: [Dag1Length::of(0), Dag1Length::of(1)],
    clobbers_arithmetic_flags: true,
    required_mode1_set: MODE1_RND32,
    required_mode1_clear: MODE1_SCALAR_CLEAR,
    output_samples: FIR_OUTPUT_SAMPLES,
};

/// Immutable emitted image and its checked placement. Cannot accept replacement code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledFir {
    code: OwnedCode,
    layout: FirLayout,
}

impl CompiledFir {
    pub fn code(&self) -> &OwnedCode {
        &self.code
    }
    pub fn layout(&self) -> FirLayout {
        self.layout
    }
    /// Counts executed instructions, including RTS; these are not hardware cycles.
    pub fn executed_steps(&self) -> usize {
        3 + FIR_OUTPUT_SAMPLES as usize * (6 * self.layout.length.taps() as usize + 5)
    }
}

/// Emits 2 modifier words, 32 unrolled 11-word output blocks and RTS.
/// Inner reduction visits taps N-1..0 using separate f32 multiply/add instructions.
pub fn compile_fir(origin: PmWordAddress24, layout: FirLayout) -> Result<CompiledFir, FirError> {
    let [r0, r1, r4, r9] = FIR_ABI.clobbered_data;
    let [i0, i1] = FIR_ABI.clobbered_indices;
    let [m0, m1] = FIR_ABI.clobbered_modifiers;
    let mut program = IsaProgram::new(origin);
    program.define(Label::new("fir.entry"))?;
    for (modifier, value) in [(m0, 1), (m1, u32::MAX)] {
        program.push(Instruction48::Immediate {
            dst: UniversalReg::Modify(modifier),
            value,
        })?;
    }
    for sample in 0..FIR_OUTPUT_SAMPLES {
        let body = Label::new(format!("fir.output.{sample}.loop"));
        for (dst, value) in [
            (
                UniversalReg::Index(i0),
                layout.input.checked_add(sample)?.get(),
            ),
            (
                UniversalReg::Index(i1),
                layout.taps.checked_add(layout.length.taps() - 1)?.get(),
            ),
            (UniversalReg::Data(r1), 0.0f32.to_bits()),
            (UniversalReg::Data(r9), layout.length.taps()),
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
            address: layout.output.checked_add(sample)?,
        })?;
    }
    program.define(Label::new("fir.return"))?;
    program.push(Instruction48::Rts)?;
    Ok(CompiledFir {
        code: program.finish()?,
        layout,
    })
}

/// Checked atomic offline wrapper around the raw decoder, with fresh scratch allocation.
/// Clones all supplied DM words and write history plus scalar state; commits only on success.
/// Input history/taps stay caller-owned. The caller prepares the next chronological window.
pub fn simulate_fir(
    fir: &CompiledFir,
    state: &mut ScalarState,
    memory: &mut DmMemory,
    step_budget: usize,
) -> Result<SimulationOutcome, FirError> {
    let layout = fir.layout;
    for (base, words) in [
        (layout.input, layout.input_words()),
        (layout.taps, layout.length.taps()),
        (layout.output, FIR_OUTPUT_SAMPLES),
    ] {
        memory.validate_range(base, words)?;
    }
    if state.mode1 & FIR_ABI.required_mode1_set != FIR_ABI.required_mode1_set
        || state.mode1 & FIR_ABI.required_mode1_clear != 0
    {
        return Err(SimulationError::Modes(state.mode1).into());
    }
    for length in FIR_ABI.zero_lengths {
        let index = usize::from(length.index());
        if state.lengths[index] != 0 {
            return Err(SimulationError::CircularLength {
                index,
                length: state.lengths[index],
            }
            .into());
        }
    }
    for (base, words) in [
        (layout.input, layout.input_words()),
        (layout.taps, layout.length.taps()),
    ] {
        for offset in 0..words {
            let address = base.checked_add(offset)?;
            if !f32::from_bits(memory.read(address)?).is_finite() {
                return Err(FirError::NonFiniteOperand {
                    address: address.get(),
                });
            }
        }
    }
    let mut next_state = state.clone();
    let mut next_memory = memory.clone();
    let outcome =
        run_scalar_checked_floats(&fir.code, &mut next_state, &mut next_memory, step_budget)?;
    *state = next_state;
    *memory = next_memory;
    Ok(outcome)
}

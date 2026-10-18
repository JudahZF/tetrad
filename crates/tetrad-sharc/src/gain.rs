//! Nine-word owned leaf ABI; no stock callbacks or assumed resident setup.
use crate::address::PmWordAddress24;
use crate::emit::{BranchRelocation, EmitError, IsaProgram, Label, OwnedCode};
use crate::isa48::{
    BranchKind, Condition, DmAccess, Instruction48, MODE1_RND32, MODE1_SCALAR_CLEAR,
};
use crate::register::{Dag1Index, Dag1Length, Dag1Modify, DataReg, FloatReg, UniversalReg};

pub const MAX_GAIN_SAMPLES: u32 = 32;
pub const GAIN_WORD_COUNT: usize = 9;

/// Explicit leaf-call preconditions. The emitted loop does not establish modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GainAbi {
    pub input_index: Dag1Index,
    pub output_index: Dag1Index,
    pub gain_register: FloatReg,
    pub count_register: DataReg,
    pub zero_lengths: [Dag1Length; 2],
    pub clobbered_data: [DataReg; 2],
    pub clobbered_modifiers: [Dag1Modify; 1],
    pub clobbered_indices: [Dag1Index; 2],
    pub clobbers_arithmetic_flags: bool,
    pub required_mode1_set: u32,
    pub required_mode1_clear: u32,
    pub maximum_samples: u32,
}

pub const GAIN_ABI: GainAbi = GainAbi {
    input_index: Dag1Index::of(0),
    output_index: Dag1Index::of(1),
    gain_register: FloatReg::of(4),
    count_register: DataReg::of(8),
    zero_lengths: [Dag1Length::of(0), Dag1Length::of(1)],
    clobbered_data: [DataReg::of(0), DataReg::of(2)],
    clobbered_modifiers: [Dag1Modify::of(0)],
    clobbered_indices: [Dag1Index::of(0), Dag1Index::of(1)],
    clobbers_arithmetic_flags: true,
    required_mode1_set: MODE1_RND32,
    required_mode1_clear: MODE1_SCALAR_CLEAR,
    maximum_samples: MAX_GAIN_SAMPLES,
};

/// Exact report loop. The origin is a PC word address, never a byte offset.
pub fn compile_gain(origin: PmWordAddress24) -> Result<OwnedCode, EmitError> {
    let [r0, r2] = GAIN_ABI.clobbered_data;
    let r8 = GAIN_ABI.count_register;
    let f0 = FloatReg::from(r0);
    let f4 = GAIN_ABI.gain_register;
    let i0 = GAIN_ABI.input_index;
    let i1 = GAIN_ABI.output_index;
    let [m0] = GAIN_ABI.clobbered_modifiers;
    let entry = Label::new("gain.entry");
    let body = Label::new("gain.loop");
    let done = Label::new("gain.return");
    let mut program = IsaProgram::new(origin);
    program.define(entry)?;
    program.push(Instruction48::Immediate {
        dst: UniversalReg::Modify(m0),
        value: 1,
    })?;
    program.push(Instruction48::IntegerPass { dst: r2, src: r8 })?;
    program.branch_to(
        BranchKind::Jump,
        Condition::Equal,
        done.clone(),
        BranchRelocation::PcRel24,
    )?;
    program.define(body.clone())?;
    program.push(Instruction48::DmLoad {
        dst: r0,
        index: i0,
        modifier: m0,
        access: DmAccess::PostModify,
    })?;
    program.push(Instruction48::FloatMultiply {
        dst: f0,
        lhs: f0,
        rhs: f4,
    })?;
    program.push(Instruction48::DmStore {
        src: r0,
        index: i1,
        modifier: m0,
        access: DmAccess::PostModify,
    })?;
    program.push(Instruction48::IntegerDecrement { dst: r2, src: r2 })?;
    program.branch_to(
        BranchKind::Jump,
        Condition::NotEqual,
        body,
        BranchRelocation::PcRel24,
    )?;
    program.define(done)?;
    program.push(Instruction48::Rts)?;
    program.finish()
}

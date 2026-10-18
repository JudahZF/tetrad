//! Gain 16005: skewed dB level, stereo balance and polarity. Law (C, corrected VISA decode):
//!
//! - LEVEL (0): dB = −60 + 72·v^(1/3.8018) (P law `0x12`, setter `0x29e`).
//!   dB = −60 gives gain +0.0, else powf(10, dB·0.05).
//! - PAN (1) stores v: gain A = min(2(1 − v), 1), B = min(2v, 1) (`0xb7`).
//! - PHASE INVERT (2): invert when v > 0.5. The sign multiplies both gains.
//! - Process (`0x24f` → `0xd5`) latches target = level·sign·pan at block start
//!   when a value changed. Then per channel: step = (target − current)/2400
//!   (state word `0x39da740e`); |step| < 1e-6 snaps current to target with
//!   step 0; each sample gets current, then current += step.
//! - Reset (R8 = 1) latches and snaps. Init: level 1, pan 0.5, no invert,
//!   current = target = 1.
//!
//! Owned (I): log2/exp2/db_to_linear leaves replace the stock polynomials and
//! kernel powf; 1/3.8018 is one f32 divide (stock: RECIPS and one Newton
//! step). A = `io[0]` = left. Stock bit equality is a hardware proof (H8).

use std::num::NonZeroU16;

use tetrad_sharc::emit::{BranchRelocation, IsaProgram, Label, LoopCount};
use tetrad_sharc::isa48::{BranchKind, Condition, DmAccess, Instruction48};
use tetrad_sharc::math::{self, Leaf, prm};
use tetrad_sharc::register::{Dag1Index, Dag1Modify, DataReg, UniversalReg};

use crate::{
    BLOCK_SAMPLES, BlockError, HostReference, OwnedBlock, ParameterLaw, ParameterSpec,
    RoutineLabels, StaticLayout,
};

pub const STOCK_MODEL_ID: u32 = 16005;
pub const LEVEL_SKEW: f32 = 3.8018;

/// Worst mono path: 170 modeled steps (dirty, channel A, inverted, snapped).
/// The stock adapter reports a conservative 365 steps per stereo block.
pub const PROCESS_CYCLES: u64 = 170;

pub const PARAMETERS: &[ParameterSpec] = &[
    ParameterSpec {
        runtime_index: 0,
        name: "LEVEL",
        law: ParameterLaw::Range {
            minimum: -60.0,
            maximum: 12.0,
            skew: LEVEL_SKEW,
        },
    },
    ParameterSpec {
        runtime_index: 1,
        name: "PAN",
        law: ParameterLaw::Range {
            minimum: -1.0,
            maximum: 1.0,
            skew: 1.0,
        },
    },
    ParameterSpec {
        runtime_index: 2,
        name: "PHASE INVERT",
        law: ParameterLaw::Switch,
    },
];

/// Law immediates of the emitted body and the twin: one source, so V2
/// compares code against the same law, and V5 can change one in the code only.
#[derive(Debug, Clone, Copy)]
struct Law {
    /// Per-sample step over the remaining distance: stock 1/2400.
    rate: f32,
    /// A step below this snaps current to target.
    snap: f32,
    /// 1/skew of the LEVEL P law.
    exponent: f32,
    /// LEVEL dB at v = 0; this dB or less mutes.
    minimum: f32,
    /// LEVEL dB range, maximum − minimum.
    span: f32,
    /// PHASE INVERT is on when v is greater.
    invert: f32,
}

const LAW: Law = Law {
    rate: f32::from_bits(0x39da_740e),
    snap: 1e-6,
    exponent: 1.0 / LEVEL_SKEW,
    minimum: -60.0,
    span: 72.0,
    invert: 0.5,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct Gain;

// Parameter writes touch one aligned value and Dirty. Process latches the
// combined target at block start. Channel is immutable (A = 0, B = 1).
#[derive(Clone, Copy)]
#[repr(i32)]
enum Word {
    Level,
    Pan,
    Invert,
    Channel,
    Current,
    Target,
    Dirty,
}

const R0: DataReg = DataReg::of(0);
const R1: DataReg = DataReg::of(1);
const R2: DataReg = DataReg::of(2);
const R4: DataReg = DataReg::of(4);
const R8: DataReg = DataReg::of(8);
const R12: DataReg = DataReg::of(12);
const I4: Dag1Index = Dag1Index::of(4);
const M6: Dag1Modify = Dag1Modify::of(6);

impl OwnedBlock for Gain {
    fn model_id(&self) -> u32 {
        STOCK_MODEL_ID
    }

    fn parameters(&self) -> &'static [ParameterSpec] {
        PARAMETERS
    }

    fn static_layout(&self) -> StaticLayout {
        // Stock init (`0x1b9`): level 1, centre pan, normal polarity, snapped.
        StaticLayout::Channels(std::array::from_fn(|channel| {
            vec![
                1.0_f32.to_bits(),
                0.5_f32.to_bits(),
                0,
                channel as u32,
                1.0_f32.to_bits(),
                1.0_f32.to_bits(),
                0,
            ]
        }))
    }

    fn process_cycles(&self) -> u64 {
        PROCESS_CYCLES
    }

    fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
        emit(program, &LAW)
    }
}

impl HostReference for Gain {
    fn process(&self, state: &mut [u32], samples: &mut [f32; BLOCK_SAMPLES]) {
        if state[Word::Dirty as usize] != 0 {
            latch_target(state);
        }
        let target = f32::from_bits(state[Word::Target as usize]);
        let mut current = f32::from_bits(state[Word::Current as usize]);
        let mut step = prm::mul(prm::sub(target, current), LAW.rate);
        if prm::operand(step).abs() < LAW.snap {
            current = target;
            step = 0.0;
        }
        for sample in samples {
            *sample = prm::mul(*sample, current);
            current = prm::add(current, step);
        }
        state[Word::Current as usize] = current.to_bits();
    }

    fn set_parameter(&self, state: &mut [u32], index: u32, value: f32) {
        let word = match index {
            0 => Word::Level,
            1 => Word::Pan,
            2 => Word::Invert,
            _ => return,
        };
        let value = if index == 0 {
            let power = math::exp2(prm::mul(math::log2(value), LAW.exponent));
            let db = prm::add(prm::mul(LAW.span, power), LAW.minimum);
            if db <= LAW.minimum {
                0.0
            } else {
                math::db_to_linear(db)
            }
        } else {
            value
        };
        state[word as usize] = value.to_bits();
        state[Word::Dirty as usize] = 1;
    }

    fn reset(&self, state: &mut [u32]) {
        latch_target(state);
        state[Word::Current as usize] = state[Word::Target as usize];
    }
}

fn latch_target(state: &mut [u32]) {
    let pan = f32::from_bits(state[Word::Pan as usize]);
    let pan = if state[Word::Channel as usize] == 0 {
        prm::sub(1.0, pan)
    } else {
        pan
    };
    let balance = prm::mul(2.0, pan).min(1.0);
    let mut level = state[Word::Level as usize];
    if prm::operand(f32::from_bits(state[Word::Invert as usize])) > LAW.invert {
        level ^= 0x8000_0000;
    }
    state[Word::Target as usize] = prm::mul(f32::from_bits(level), balance).to_bits();
    state[Word::Dirty as usize] = 0;
}

fn emit(program: &mut IsaProgram, law: &Law) -> Result<RoutineLabels, BlockError> {
    let labels = RoutineLabels {
        process: Label::new("gain.process"),
        set_parameter: Label::new("gain.parameter"),
        reset: Label::new("gain.reset"),
    };
    let target = Label::new("gain.target");
    let clean = Label::new("gain.clean");
    let ramp = Label::new("gain.ramp");
    let loop_end = Label::new("gain.loop_end");
    program.define(labels.process.clone())?;
    program.push(load(R0, Word::Dirty))?;
    program.push(Instruction48::IntegerPass { dst: R0, src: R0 })?;
    branch(program, BranchKind::Jump, Condition::Equal, &clean)?;
    branch(program, BranchKind::Call, Condition::Always, &target)?;
    program.define(clean)?;
    for (register, word) in [(R0, Word::Target), (R12, Word::Current)] {
        program.push(load(register, word))?;
    }
    program.push(Instruction48::FloatSubtract {
        dst: R2.into(),
        lhs: R0.into(),
        rhs: R12.into(),
    })?;
    program.push(immediate(R1, law.rate.to_bits()))?;
    program.push(Instruction48::FloatMultiply {
        dst: R2.into(),
        lhs: R2.into(),
        rhs: R1.into(),
    })?;
    program.push(Instruction48::FloatAbs {
        dst: R1.into(),
        src: R2.into(),
    })?;
    program.push(immediate(R4, law.snap.to_bits()))?;
    program.push(Instruction48::FloatCompare {
        lhs: R1.into(),
        rhs: R4.into(),
    })?;
    branch(program, BranchKind::Jump, Condition::GreaterOrEqual, &ramp)?;
    program.push(Instruction48::RegisterMove {
        dst: UniversalReg::Data(R12),
        src: UniversalReg::Data(R0),
    })?;
    program.push(immediate(R2, 0))?;
    program.define(ramp)?;
    program.push(Instruction48::RegisterMove {
        dst: UniversalReg::Data(R4),
        src: UniversalReg::Index(I4),
    })?;
    program.push(Instruction48::RegisterMove {
        dst: UniversalReg::Index(I4),
        src: UniversalReg::Data(R8),
    })?;
    program.counted_loop(
        LoopCount::Immediate(NonZeroU16::new(BLOCK_SAMPLES as u16).unwrap()),
        loop_end.clone(),
    )?;
    program.push(Instruction48::DmOffsetLoad {
        dst: R0,
        index: I4,
        offset: 0,
    })?;
    program.push(Instruction48::FloatMultiply {
        dst: R0.into(),
        lhs: R0.into(),
        rhs: R12.into(),
    })?;
    program.push(Instruction48::DmStore {
        src: R0,
        index: I4,
        modifier: M6,
        access: DmAccess::PostModify,
    })?;
    program.define(loop_end)?;
    program.push(Instruction48::FloatAdd {
        dst: R12.into(),
        lhs: R12.into(),
        rhs: R2.into(),
    })?;
    program.push(Instruction48::RegisterMove {
        dst: UniversalReg::Index(I4),
        src: UniversalReg::Data(R4),
    })?;
    program.push(store(R12, Word::Current))?;
    program.push(Instruction48::Rts)?;

    emit_parameter(program, &labels.set_parameter, law)?;

    program.define(labels.reset.clone())?;
    branch(program, BranchKind::Call, Condition::Always, &target)?;
    program.push(load(R0, Word::Target))?;
    program.push(store(R0, Word::Current))?;
    program.push(Instruction48::Rts)?;
    emit_target(program, &target, law)?;
    math::emit(program, &[Leaf::Log2, Leaf::Exp2, Leaf::DbToLinear])?;
    Ok(labels)
}

fn emit_parameter(program: &mut IsaProgram, label: &Label, law: &Law) -> Result<(), BlockError> {
    let pan = Label::new("gain.parameter_pan");
    let invert = Label::new("gain.parameter_invert");
    let dirty = Label::new("gain.parameter_dirty");
    let mute = Label::new("gain.parameter_mute");
    let level = Label::new("gain.parameter_level");
    let done = Label::new("gain.parameter_done");
    program.define(label.clone())?;
    for (index, destination) in [(1, &pan), (2, &invert)] {
        program.push(immediate(R0, index))?;
        program.push(Instruction48::IntegerCompare { lhs: R8, rhs: R0 })?;
        branch(program, BranchKind::Jump, Condition::Equal, destination)?;
    }
    program.push(Instruction48::IntegerPass { dst: R8, src: R8 })?;
    branch(program, BranchKind::Jump, Condition::NotEqual, &done)?;
    program.push(Instruction48::RegisterMove {
        dst: UniversalReg::Data(R4),
        src: UniversalReg::Data(R12),
    })?;
    branch(
        program,
        BranchKind::Call,
        Condition::Always,
        &Leaf::Log2.label(),
    )?;
    program.push(immediate(R2, law.exponent.to_bits()))?;
    program.push(Instruction48::FloatMultiply {
        dst: R4.into(),
        lhs: R0.into(),
        rhs: R2.into(),
    })?;
    branch(
        program,
        BranchKind::Call,
        Condition::Always,
        &Leaf::Exp2.label(),
    )?;
    program.push(immediate(R2, law.span.to_bits()))?;
    program.push(Instruction48::FloatMultiply {
        dst: R4.into(),
        lhs: R0.into(),
        rhs: R2.into(),
    })?;
    program.push(immediate(R2, law.minimum.to_bits()))?;
    program.push(Instruction48::FloatAdd {
        dst: R4.into(),
        lhs: R4.into(),
        rhs: R2.into(),
    })?;
    program.push(Instruction48::FloatCompare {
        lhs: R4.into(),
        rhs: R2.into(),
    })?;
    branch(program, BranchKind::Jump, Condition::LessOrEqual, &mute)?;
    branch(
        program,
        BranchKind::Call,
        Condition::Always,
        &Leaf::DbToLinear.label(),
    )?;
    branch(program, BranchKind::Jump, Condition::Always, &level)?;
    program.define(mute)?;
    program.push(immediate(R0, 0))?;
    program.define(level)?;
    program.push(store(R0, Word::Level))?;
    branch(program, BranchKind::Jump, Condition::Always, &dirty)?;
    program.define(pan)?;
    program.push(store(R12, Word::Pan))?;
    branch(program, BranchKind::Jump, Condition::Always, &dirty)?;
    program.define(invert)?;
    program.push(store(R12, Word::Invert))?;
    program.define(dirty)?;
    program.push(immediate(R0, 1))?;
    program.push(store(R0, Word::Dirty))?;
    program.define(done)?;
    program.push(Instruction48::Rts)?;
    Ok(())
}

fn emit_target(program: &mut IsaProgram, label: &Label, law: &Law) -> Result<(), BlockError> {
    let balance = Label::new("gain.balance");
    let normal = Label::new("gain.normal");
    program.define(label.clone())?;
    program.push(load(R0, Word::Pan))?;
    program.push(load(R2, Word::Channel))?;
    program.push(Instruction48::IntegerPass { dst: R2, src: R2 })?;
    branch(program, BranchKind::Jump, Condition::NotEqual, &balance)?;
    program.push(immediate(R1, 1.0_f32.to_bits()))?;
    program.push(Instruction48::FloatSubtract {
        dst: R0.into(),
        lhs: R1.into(),
        rhs: R0.into(),
    })?;
    program.define(balance)?;
    program.push(Instruction48::FloatAdd {
        dst: R0.into(),
        lhs: R0.into(),
        rhs: R0.into(),
    })?;
    program.push(immediate(R1, 1.0_f32.to_bits()))?;
    program.push(Instruction48::FloatClip {
        dst: R0.into(),
        src: R0.into(),
        bound: R1.into(),
    })?;
    program.push(load(R1, Word::Level))?;
    program.push(load(R2, Word::Invert))?;
    program.push(immediate(R12, law.invert.to_bits()))?;
    program.push(Instruction48::FloatCompare {
        lhs: R2.into(),
        rhs: R12.into(),
    })?;
    branch(program, BranchKind::Jump, Condition::LessOrEqual, &normal)?;
    program.push(immediate(R2, 0x8000_0000))?;
    program.push(Instruction48::Xor {
        dst: R1,
        lhs: R1,
        rhs: R2,
    })?;
    program.define(normal)?;
    program.push(Instruction48::FloatMultiply {
        dst: R0.into(),
        lhs: R1.into(),
        rhs: R0.into(),
    })?;
    program.push(store(R0, Word::Target))?;
    program.push(immediate(R2, 0))?;
    program.push(store(R2, Word::Dirty))?;
    program.push(Instruction48::Rts)?;
    Ok(())
}

fn load(dst: DataReg, word: Word) -> Instruction48 {
    Instruction48::DmOffsetLoad {
        dst,
        index: I4,
        offset: word as i32,
    }
}

fn store(src: DataReg, word: Word) -> Instruction48 {
    Instruction48::DmOffsetStore {
        src,
        index: I4,
        offset: word as i32,
    }
}

fn immediate(register: DataReg, value: u32) -> Instruction48 {
    Instruction48::Immediate {
        dst: UniversalReg::Data(register),
        value,
    }
}

fn branch(
    program: &mut IsaProgram,
    kind: BranchKind,
    condition: Condition,
    target: &Label,
) -> Result<(), BlockError> {
    program.branch_to(kind, condition, target.clone(), BranchRelocation::PcRel24)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verification::{VerificationError, scheduled_frames, verify_module};

    /// The body emitted with a changed law against the stock twin (roadmap V5).
    struct Mutant(Law);

    impl OwnedBlock for Mutant {
        fn model_id(&self) -> u32 {
            STOCK_MODEL_ID
        }
        fn parameters(&self) -> &'static [ParameterSpec] {
            PARAMETERS
        }
        fn static_layout(&self) -> StaticLayout {
            Gain.static_layout()
        }
        fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
            emit(program, &self.0)
        }
        fn process_cycles(&self) -> u64 {
            PROCESS_CYCLES
        }
    }

    impl HostReference for Mutant {
        fn process(&self, state: &mut [u32], samples: &mut [f32; BLOCK_SAMPLES]) {
            Gain.process(state, samples);
        }
        fn set_parameter(&self, state: &mut [u32], index: u32, value: f32) {
            Gain.set_parameter(state, index, value);
        }
        fn reset(&self, state: &mut [u32]) {
            Gain.reset(state);
        }
    }

    /// One changed immediate per law part fails V2 at its first effect.
    /// LEVEL v = 0.5 is ≈ 0 dB, so stock snaps at once while a snap-free
    /// mutant ramps; v = 1 (+12 dB) ramps under either snap, so only the rate
    /// shows there; PHASE INVERT v = 0.5 is the switch edge.
    #[test]
    fn each_changed_law_constant_fails_v2_where_it_acts() {
        let frames = scheduled_frames(64, &[(0, 0, 0.5), (4, 0, 1.0), (8, 2, 0.5)]);
        verify_module(&Mutant(LAW), &frames).unwrap();
        let current = Word::Current as usize;
        let level = Word::Level as usize;
        let cases = [
            (
                Law {
                    rate: 1.0 / 2000.0,
                    ..LAW
                },
                4,
                "state",
                current,
            ),
            (Law { snap: 0.0, ..LAW }, 0, "state", current),
            (
                Law {
                    exponent: 1.0 / 3.8,
                    ..LAW
                },
                0,
                "parameter state",
                level,
            ),
            (
                Law {
                    minimum: -59.0,
                    ..LAW
                },
                0,
                "parameter state",
                level,
            ),
            (Law { span: 71.0, ..LAW }, 0, "parameter state", level),
            (
                Law {
                    invert: LAW.invert.next_down(),
                    ..LAW
                },
                8,
                "state",
                current,
            ),
        ];
        for (law, at, named, index) in cases {
            let error = verify_module(&Mutant(law), &frames).unwrap_err();
            assert!(
                matches!(
                    error,
                    VerificationError::Mismatch { frame, channel: 0, region, word, .. }
                        if frame == at && region == named && word == index
                ),
                "{law:?}: {error}"
            );
        }
    }
}

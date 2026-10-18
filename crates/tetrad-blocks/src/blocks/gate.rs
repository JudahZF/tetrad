//! Simple Gate (noisegate 16003). Law (C, corrected VISA decode):
//!
//! - THRESHOLD (0): T = −90 + 90·v^(1/0.6) dB (setter `0x275`). Process start
//!   (`0x53` → `0x403`) publishes a changed T: open = 10^(T/20), close =
//!   10^((T − 7)/20). No ramp.
//! - Process (`0x8d`), one law for both channels: peak = max |0.5·(A + B)|
//!   over the block, then 32 envelope steps e += c·(peak − e) with c latched
//!   at block start. Per sample: below close → hold −= 1, c = slow (from the
//!   next block); at or above open → hold = 481, open; hold ≤ 0 → hold = 0,
//!   closed, c = fast. Open: g = min(g + a·(1 − g), 1); closed: g =
//!   max(g·p_r, 0). Both channels × g.
//! - Setup (`0x529`) at 48 kHz, times as ms·0.001f: k = tan((1/t)·(1/fs)),
//!   p = (1 − k)/(1 + k); a = 1.5·(1 − p) at 10 ms, p_r = p at 40 ms; hold =
//!   max(ceilf(fs·10 ms), 32).
//! - Reset (R8 = 1, `0x80` → `0x53`) only publishes the pending threshold.
//!   Envelope, gain, hold and state keep running. The floor stays 0 (I: no
//!   writer of its target was found).
//!
//! Owned (I): poles come from the owned tan and reciprocal twins once at
//! build time (stock: in-blob tan, RECIPS, kernel divide at init); threshold
//! conversion uses owned log2/exp2/db_to_linear (stock: polynomials). Fault
//! recovery (R8 = 0) clears the dynamics and keeps the threshold. Stock
//! `0x6c0` re-runs setup: T becomes −60 and a poisoned envelope stays.

use std::num::NonZeroU16;

use tetrad_sharc::emit::{BranchRelocation, IsaProgram, Label, LoopCount};
use tetrad_sharc::isa48::{BranchKind, Condition, DmAccess, Instruction48};
use tetrad_sharc::math::{self, Leaf, prm};
use tetrad_sharc::register::{Dag1Index, Dag1Modify, DataReg, UniversalReg};

use crate::{
    BLOCK_SAMPLES, BlockError, HostReference, OwnedBlock, ParameterLaw, ParameterSpec,
    RoutineLabels, StaticLayout,
};

pub const STOCK_MODEL_ID: u32 = 16003;
pub const SAMPLE_RATE: u32 = 48_000;
/// 10 ms as stock computes it (ms·0.001f, `0x582`): attack and hold time.
pub const ATTACK_SECONDS: f32 = f32::from_bits(0x3c23_d70b);
/// 40 ms as stock computes it (`0x613`).
pub const RELEASE_SECONDS: f32 = f32::from_bits(0x3d23_d70b);
/// max(ceilf(48000·ATTACK_SECONDS), 32): the product rounds to 480.00003,
/// so stock holds 481 samples (`0x5f6..0x611`; k1 `0x16d452` is ceilf).
pub const HOLD_SAMPLES: u32 = 481;
/// Exact recovered literals at noisegate parcels 0x4b5/0x4b8 and 0x4bd.
pub const DETECTOR_FAST: f32 = f32::from_bits(0x3c55_124e);
pub const DETECTOR_SLOW: f32 = f32::from_bits(0x3b44_ed11);
/// Worst stereo leaf path: 81 setup/threshold + 195 peak + 134 envelope
/// + 5 gain-loop setup + 31 * 32 decisions/audio + 9 exit instructions.
pub const PROCESS_CYCLES: u64 = 1_416;

/// Law immediates of the emitted body and the twin: one source, so V2
/// compares code against the same law, and V5 can change one in the code only.
#[derive(Debug, Clone, Copy)]
struct Law {
    /// The detector reads |mono_scale·(A + B)|.
    mono_scale: f32,
    /// The close threshold is this many dB below the open threshold.
    hysteresis: f32,
    hold: u32,
    fast: f32,
    slow: f32,
    /// THRESHOLD = −90 + 90·v^(1/skew).
    skew: f32,
}

const LAW: Law = Law {
    mono_scale: 0.5,
    hysteresis: 7.0,
    hold: HOLD_SAMPLES,
    fast: DETECTOR_FAST,
    slow: DETECTOR_SLOW,
    skew: 0.6,
};

pub const PARAMETERS: &[ParameterSpec] = &[ParameterSpec {
    runtime_index: 0,
    name: "THRESHOLD",
    law: ParameterLaw::Range {
        minimum: -90.0,
        maximum: 0.0,
        skew: LAW.skew,
    },
}];

// The one stereo state (stock shape: one state for both channels).
const PENDING_THRESHOLD: usize = 0;
const THRESHOLD: usize = 1;
const OPEN_THRESHOLD: usize = 2;
const CLOSE_THRESHOLD: usize = 3;
const OPEN: usize = 4;
const ATTACK: usize = 5;
const RELEASE: usize = 6;
const HOLD: usize = 7;
const GAIN: usize = 8;
const ENVELOPE: usize = 9;
const COEFFICIENT: usize = 10;
const ENVELOPES: usize = 11;
const STATE_WORDS: usize = ENVELOPES + BLOCK_SAMPLES;

/// Stock 48 kHz setup, with one shared detector and gain for both channels.
#[derive(Debug, Clone, Copy, Default)]
pub struct SimpleGate;

impl SimpleGate {
    /// The live gate state, for burst/timing measurements without a machine.
    pub fn dynamics(state: &[u32]) -> GateDynamics {
        GateDynamics {
            open: state[OPEN] != 0,
            hold_remaining: state[HOLD],
            gain: f32::from_bits(state[GAIN]),
            envelope: f32::from_bits(state[ENVELOPE]),
            detector_coefficient: f32::from_bits(state[COEFFICIENT]),
            threshold_db: f32::from_bits(state[THRESHOLD]),
        }
    }

    /// V1 stereo twin of the process leaf, on the state of `static_layout()`.
    pub fn process_pair(&self, state: &mut [u32], samples: &mut [[f32; BLOCK_SAMPLES]; 2]) {
        if state[PENDING_THRESHOLD] != state[THRESHOLD] {
            publish_threshold(state);
        }
        let peak = samples[0]
            .iter()
            .zip(&samples[1])
            .map(|(&a, &b)| prm::mul(LAW.mono_scale, prm::add(a, b)).abs())
            .fold(0.0_f32, f32::max);
        let mut envelope = f32::from_bits(state[ENVELOPE]);
        let coefficient = f32::from_bits(state[COEFFICIENT]);
        for word in &mut state[ENVELOPES..] {
            envelope = prm::add(envelope, prm::mul(coefficient, prm::sub(peak, envelope)));
            *word = envelope.to_bits();
        }
        state[ENVELOPE] = envelope.to_bits();

        let close = f32::from_bits(state[CLOSE_THRESHOLD]);
        let open = f32::from_bits(state[OPEN_THRESHOLD]);
        let attack = f32::from_bits(state[ATTACK]);
        let release = f32::from_bits(state[RELEASE]);
        let mut gain = f32::from_bits(state[GAIN]);
        for n in 0..BLOCK_SAMPLES {
            let e = prm::operand(f32::from_bits(state[ENVELOPES + n]));
            // The stock decrement from zero becomes -1 before the <= 0 check.
            let mut hold = state[HOLD] as i32;
            if e < close {
                hold -= 1;
                state[COEFFICIENT] = LAW.slow.to_bits();
            } else if e >= open {
                hold = LAW.hold as i32;
                state[OPEN] = 1;
            }
            if hold <= 0 {
                hold = 0;
                state[OPEN] = 0;
                state[COEFFICIENT] = LAW.fast.to_bits();
            }
            state[HOLD] = hold as u32;
            gain = if state[OPEN] != 0 {
                prm::add(gain, prm::mul(attack, prm::sub(1.0, gain))).min(1.0)
            } else {
                prm::mul(gain, release).max(0.0)
            };
            for channel in samples.iter_mut() {
                channel[n] = prm::mul(channel[n], gain);
            }
        }
        state[GAIN] = gain.to_bits();
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateDynamics {
    pub open: bool,
    pub hold_remaining: u32,
    pub gain: f32,
    pub envelope: f32,
    pub detector_coefficient: f32,
    pub threshold_db: f32,
}

/// Setup `0x529`: k = tan((1/t)·(1/fs)), p = (1 − k)/(1 + k).
fn pole(seconds: f32) -> f32 {
    let x = prm::mul(
        math::reciprocal(seconds),
        math::reciprocal(SAMPLE_RATE as f32),
    );
    let k = math::tan(x);
    prm::mul(prm::sub(1.0, k), math::reciprocal(prm::add(1.0, k)))
}

/// `0x403`: T, open = 10^(T/20) and close = 10^((T − 7)/20) from the pending T.
fn publish_threshold(state: &mut [u32]) {
    let threshold = f32::from_bits(state[PENDING_THRESHOLD]);
    state[THRESHOLD] = threshold.to_bits();
    state[OPEN_THRESHOLD] = math::db_to_linear(threshold).to_bits();
    state[CLOSE_THRESHOLD] = math::db_to_linear(prm::sub(threshold, LAW.hysteresis)).to_bits();
}

fn initial_state() -> Vec<u32> {
    let mut state = vec![0; STATE_WORDS];
    state[PENDING_THRESHOLD] = (-60.0_f32).to_bits();
    state[ATTACK] = prm::mul(1.5, prm::sub(1.0, pole(ATTACK_SECONDS))).to_bits();
    state[RELEASE] = pole(RELEASE_SECONDS).to_bits();
    SimpleGate.recover(&mut state);
    state
}

impl OwnedBlock for SimpleGate {
    fn model_id(&self) -> u32 {
        STOCK_MODEL_ID
    }

    fn parameters(&self) -> &'static [ParameterSpec] {
        PARAMETERS
    }

    fn static_layout(&self) -> StaticLayout {
        StaticLayout::Stereo(initial_state())
    }

    fn process_cycles(&self) -> u64 {
        PROCESS_CYCLES
    }

    fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
        emit_body(program, &LAW)
    }
}

fn emit_body(program: &mut IsaProgram, law: &Law) -> Result<RoutineLabels, BlockError> {
    let labels = RoutineLabels {
        process: Label::new("gate.process"),
        set_parameter: Label::new("gate.parameter"),
        reset: Label::new("gate.reset"),
    };
    let mut code = Code(program);
    code.0.define(labels.process.clone())?;
    emit_process(&mut code, law)?;
    code.0.define(labels.set_parameter.clone())?;
    emit_parameter(&mut code, law)?;
    code.0.define(labels.reset.clone())?;
    emit_reset(&mut code, law)?;
    math::emit(code.0, &[Leaf::DbToLinear, Leaf::Log2])?;
    Ok(labels)
}

impl HostReference for SimpleGate {
    fn process_stereo(&self, state: &mut [u32], samples: &mut [[f32; BLOCK_SAMPLES]; 2]) {
        self.process_pair(state, samples);
    }

    fn process(&self, state: &mut [u32], samples: &mut [f32; BLOCK_SAMPLES]) {
        let mut pair = [*samples, *samples];
        self.process_pair(state, &mut pair);
        *samples = pair[0];
    }

    fn set_parameter(&self, state: &mut [u32], index: u32, value: f32) {
        if index == 0 {
            let v = prm::operand(value);
            let power = if v == 0.0 {
                0.0
            } else {
                math::exp2(prm::mul(math::log2(v), math::reciprocal(LAW.skew)))
            };
            // One aligned store publishes the new threshold. The audio
            // callback can preempt conversion and sees either complete value.
            state[PENDING_THRESHOLD] = prm::add(-90.0, prm::mul(90.0, power)).to_bits();
        }
    }

    fn reset(&self, state: &mut [u32]) {
        publish_threshold(state);
    }

    fn recover(&self, state: &mut [u32]) {
        for word in [OPEN, HOLD, GAIN, ENVELOPE] {
            state[word] = 0;
        }
        state[ENVELOPES..].fill(0);
        state[COEFFICIENT] = LAW.fast.to_bits();
        publish_threshold(state);
    }
}

/// Typed register operands keep the emission readable. All data operands
/// are CCES scratch; the three DAG indices are saved and restored.
#[derive(Clone, Copy)]
enum R {
    R0 = 0,
    R1 = 1,
    R2 = 2,
    R4 = 4,
    R8 = 8,
    R12 = 12,
}

impl From<R> for DataReg {
    fn from(r: R) -> Self {
        Self::new(r as u8).expect("CCES scratch register")
    }
}

#[derive(Clone, Copy)]
enum Op {
    Add,
    Subtract,
    Multiply,
    Max,
    Clip,
    Compare,
}

struct Code<'a>(&'a mut IsaProgram);

impl Code<'_> {
    fn set(&mut self, dst: R, value: u32) -> Result<(), BlockError> {
        self.0.push(Instruction48::Immediate {
            dst: UniversalReg::Data(dst.into()),
            value,
        })?;
        Ok(())
    }

    fn load(&mut self, dst: R, index: u8, word: usize) -> Result<(), BlockError> {
        self.0.push(Instruction48::DmOffsetLoad {
            dst: dst.into(),
            index: Dag1Index::new(index).expect("DAG1 index"),
            offset: word as i32,
        })?;
        Ok(())
    }

    fn store(&mut self, src: R, index: u8, word: usize) -> Result<(), BlockError> {
        self.0.push(Instruction48::DmOffsetStore {
            src: src.into(),
            index: Dag1Index::new(index).expect("DAG1 index"),
            offset: word as i32,
        })?;
        Ok(())
    }

    fn float(&mut self, op: Op, dst: R, lhs: R, rhs: R) -> Result<(), BlockError> {
        let (dst, lhs, rhs) = (
            DataReg::from(dst).into(),
            DataReg::from(lhs).into(),
            DataReg::from(rhs).into(),
        );
        self.0.push(match op {
            Op::Add => Instruction48::FloatAdd { dst, lhs, rhs },
            Op::Subtract => Instruction48::FloatSubtract { dst, lhs, rhs },
            Op::Multiply => Instruction48::FloatMultiply { dst, lhs, rhs },
            Op::Max => Instruction48::FloatMax { dst, lhs, rhs },
            Op::Clip => Instruction48::FloatClip {
                dst,
                src: lhs,
                bound: rhs,
            },
            Op::Compare => Instruction48::FloatCompare { lhs, rhs },
        })?;
        Ok(())
    }

    fn branch(&mut self, condition: Condition, target: &str) -> Result<(), BlockError> {
        self.0.branch_to(
            BranchKind::Jump,
            condition,
            Label::new(target),
            BranchRelocation::PcRel24,
        )?;
        Ok(())
    }

    fn call(&mut self, leaf: Leaf) -> Result<(), BlockError> {
        self.0.branch_to(
            BranchKind::Call,
            Condition::Always,
            leaf.label(),
            BranchRelocation::PcRel24,
        )?;
        Ok(())
    }

    fn loop32(&mut self, last: &str) -> Result<(), BlockError> {
        self.0.counted_loop(
            LoopCount::Immediate(NonZeroU16::new(BLOCK_SAMPLES as u16).expect("32 samples")),
            Label::new(last),
        )?;
        Ok(())
    }

    fn index(&mut self, dst: u8, src: R) -> Result<(), BlockError> {
        self.0.push(Instruction48::RegisterMove {
            dst: UniversalReg::Index(Dag1Index::new(dst).expect("DAG1 index")),
            src: UniversalReg::Data(src.into()),
        })?;
        Ok(())
    }

    fn transfer(&mut self, register: R, index: u8, store: bool) -> Result<(), BlockError> {
        let index = Dag1Index::new(index).expect("DAG1 index");
        let modifier = Dag1Modify::new(6).expect("M6 = 1");
        self.0.push(if store {
            Instruction48::DmStore {
                src: register.into(),
                index,
                modifier,
                access: DmAccess::PostModify,
            }
        } else {
            Instruction48::DmLoad {
                dst: register.into(),
                index,
                modifier,
                access: DmAccess::PostModify,
            }
        })?;
        Ok(())
    }
}

/// [`publish_threshold`] with R4 = the pending threshold.
fn emit_publish_threshold(code: &mut Code<'_>, law: &Law) -> Result<(), BlockError> {
    use R::*;
    code.store(R4, 4, THRESHOLD)?;
    code.call(Leaf::DbToLinear)?;
    code.store(R0, 4, OPEN_THRESHOLD)?;
    code.load(R4, 4, THRESHOLD)?;
    code.set(R2, law.hysteresis.to_bits())?;
    code.float(Op::Subtract, R4, R4, R2)?;
    code.call(Leaf::DbToLinear)?;
    code.store(R0, 4, CLOSE_THRESHOLD)
}

fn emit_process(code: &mut Code<'_>, law: &Law) -> Result<(), BlockError> {
    use Op::*;
    use R::*;
    // Stereo leaf: R8 is the io descriptor, not one channel's sample pointer.
    for n in 0..3 {
        code.0.push(Instruction48::RegisterMove {
            dst: UniversalReg::Data(R0.into()),
            src: UniversalReg::Index(Dag1Index::new(n).expect("saved DAG1 index")),
        })?;
        code.0.push(Instruction48::DmStore {
            src: R0.into(),
            index: Dag1Index::new(7).expect("I7"),
            modifier: Dag1Modify::new(7).expect("M7"),
            access: DmAccess::PostModify,
        })?;
    }
    code.index(2, R8)?;
    code.load(R0, 2, 0)?;
    code.index(0, R0)?;
    code.load(R0, 2, 1)?;
    code.index(1, R0)?;
    code.load(R4, 4, PENDING_THRESHOLD)?;
    code.load(R0, 4, THRESHOLD)?;
    code.0.push(Instruction48::IntegerCompare {
        lhs: R4.into(),
        rhs: R0.into(),
    })?;
    code.branch(Condition::Equal, "gate.peak")?;
    emit_publish_threshold(code, law)?;

    code.0.define(Label::new("gate.peak"))?;
    code.set(R12, 0)?;
    code.set(R2, law.mono_scale.to_bits())?;
    code.loop32("gate.peak.last")?;
    code.transfer(R0, 0, false)?;
    code.transfer(R1, 1, false)?;
    code.float(Add, R0, R0, R1)?;
    code.float(Multiply, R0, R0, R2)?;
    code.0.push(Instruction48::FloatAbs {
        dst: DataReg::from(R0).into(),
        src: DataReg::from(R0).into(),
    })?;
    code.0.define(Label::new("gate.peak.last"))?;
    code.float(Max, R12, R12, R0)?;

    code.0.push(Instruction48::RegisterMove {
        dst: UniversalReg::Index(Dag1Index::new(2).expect("I2")),
        src: UniversalReg::Index(Dag1Index::new(4).expect("I4")),
    })?;
    code.0.push(Instruction48::ModifyWords {
        index: Dag1Index::new(2).expect("I2"),
        words: ENVELOPES as i32,
    })?;
    code.load(R8, 4, ENVELOPE)?;
    code.load(R2, 4, COEFFICIENT)?;
    code.loop32("gate.envelope.last")?;
    code.float(Subtract, R0, R12, R8)?;
    code.float(Multiply, R0, R0, R2)?;
    code.float(Add, R8, R8, R0)?;
    code.0.define(Label::new("gate.envelope.last"))?;
    code.transfer(R8, 2, true)?;
    code.store(R8, 4, ENVELOPE)?;
    for index in 0..3 {
        code.0.push(Instruction48::ModifyWords {
            index: Dag1Index::new(index).expect("DAG1 index"),
            words: -(BLOCK_SAMPLES as i32),
        })?;
    }

    code.load(R8, 4, GAIN)?;
    code.loop32("gate.gain.last")?;
    code.transfer(R0, 2, false)?;
    code.load(R1, 4, CLOSE_THRESHOLD)?;
    code.float(Compare, R0, R0, R1)?;
    code.branch(Condition::LessThan, "gate.below_close")?;
    code.load(R1, 4, OPEN_THRESHOLD)?;
    code.float(Compare, R0, R0, R1)?;
    code.branch(Condition::LessThan, "gate.hold_check")?;
    code.set(R1, law.hold)?;
    code.store(R1, 4, HOLD)?;
    code.set(R1, 1)?;
    code.store(R1, 4, OPEN)?;
    code.branch(Condition::Always, "gate.hold_check")?;

    code.0.define(Label::new("gate.below_close"))?;
    code.load(R1, 4, HOLD)?;
    code.0.push(Instruction48::IntegerDecrement {
        dst: R1.into(),
        src: R1.into(),
    })?;
    code.store(R1, 4, HOLD)?;
    code.set(R1, law.slow.to_bits())?;
    code.store(R1, 4, COEFFICIENT)?;

    code.0.define(Label::new("gate.hold_check"))?;
    code.load(R1, 4, HOLD)?;
    code.0.push(Instruction48::IntegerPass {
        dst: R1.into(),
        src: R1.into(),
    })?;
    code.branch(Condition::GreaterThan, "gate.gain")?;
    code.set(R1, 0)?;
    code.store(R1, 4, HOLD)?;
    code.store(R1, 4, OPEN)?;
    code.set(R1, law.fast.to_bits())?;
    code.store(R1, 4, COEFFICIENT)?;

    code.0.define(Label::new("gate.gain"))?;
    code.load(R1, 4, OPEN)?;
    code.0.push(Instruction48::IntegerPass {
        dst: R1.into(),
        src: R1.into(),
    })?;
    code.branch(Condition::Equal, "gate.release")?;
    code.set(R1, 1.0_f32.to_bits())?;
    code.float(Subtract, R2, R1, R8)?;
    code.load(R0, 4, ATTACK)?;
    code.float(Multiply, R2, R2, R0)?;
    code.float(Add, R8, R8, R2)?;
    code.float(Clip, R8, R8, R1)?;
    code.branch(Condition::Always, "gate.multiply")?;

    code.0.define(Label::new("gate.release"))?;
    code.load(R2, 4, RELEASE)?;
    code.float(Multiply, R8, R8, R2)?;
    code.set(R1, 0)?;
    code.float(Max, R8, R8, R1)?;

    code.0.define(Label::new("gate.multiply"))?;
    code.load(R0, 0, 0)?;
    code.load(R12, 1, 0)?;
    code.float(Multiply, R0, R0, R8)?;
    code.transfer(R0, 0, true)?;
    code.float(Multiply, R12, R12, R8)?;
    code.0.define(Label::new("gate.gain.last"))?;
    code.transfer(R12, 1, true)?;
    code.store(R8, 4, GAIN)?;

    // Read I2, I1, I0 while they are above I7, then one stock pop (review N3).
    for n in (0..3).rev() {
        code.load(R0, 7, usize::from(3 - n))?;
        code.index(n, R0)?;
    }
    code.0.push(Instruction48::ModifyWords {
        index: Dag1Index::new(7).expect("I7"),
        words: 3,
    })?;
    code.0.push(Instruction48::Rts)?;
    Ok(())
}

fn emit_parameter(code: &mut Code<'_>, law: &Law) -> Result<(), BlockError> {
    use R::*;
    code.0.push(Instruction48::IntegerPass {
        dst: R8.into(),
        src: R8.into(),
    })?;
    code.branch(Condition::NotEqual, "gate.parameter.return")?;
    code.set(R2, 0)?;
    code.float(Op::Compare, R0, R12, R2)?;
    code.branch(Condition::Equal, "gate.parameter.zero")?;
    code.0.push(Instruction48::RegisterMove {
        dst: UniversalReg::Data(R4.into()),
        src: UniversalReg::Data(R12.into()),
    })?;
    code.call(Leaf::Log2)?;
    code.set(R2, math::reciprocal(law.skew).to_bits())?;
    code.float(Op::Multiply, R4, R0, R2)?;
    code.call(Leaf::Exp2)?;
    code.branch(Condition::Always, "gate.parameter.db")?;
    code.0.define(Label::new("gate.parameter.zero"))?;
    code.set(R0, 0)?;
    code.0.define(Label::new("gate.parameter.db"))?;
    code.set(R2, 90.0_f32.to_bits())?;
    code.float(Op::Multiply, R0, R0, R2)?;
    code.set(R2, (-90.0_f32).to_bits())?;
    code.float(Op::Add, R0, R0, R2)?;
    code.store(R0, 4, PENDING_THRESHOLD)?;
    code.0.define(Label::new("gate.parameter.return"))?;
    code.0.push(Instruction48::Rts)?;
    Ok(())
}

/// R8 ≠ 0 (kernel reset): publish the threshold only, as stock. R8 = 0
/// (fault recovery): clear the dynamics first.
fn emit_reset(code: &mut Code<'_>, law: &Law) -> Result<(), BlockError> {
    use R::*;
    code.0.push(Instruction48::IntegerPass {
        dst: R8.into(),
        src: R8.into(),
    })?;
    code.branch(Condition::NotEqual, "gate.reset.publish")?;
    code.set(R0, 0)?;
    for word in [OPEN, HOLD, GAIN, ENVELOPE]
        .into_iter()
        .chain(ENVELOPES..STATE_WORDS)
    {
        code.store(R0, 4, word)?;
    }
    code.set(R0, law.fast.to_bits())?;
    code.store(R0, 4, COEFFICIENT)?;
    code.0.define(Label::new("gate.reset.publish"))?;
    code.load(R4, 4, PENDING_THRESHOLD)?;
    emit_publish_threshold(code, law)?;
    code.0.push(Instruction48::Rts)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verification::{
        VerificationError, VerificationFrame, scheduled_frames, verify_module,
    };

    #[test]
    fn exact_threshold_boundaries_keep_hold_at_close_and_reload_at_open() {
        const START_HOLD: u32 = HOLD_SAMPLES / 2;
        for (threshold_word, below, expected_hold) in [
            (CLOSE_THRESHOLD, false, START_HOLD),
            (CLOSE_THRESHOLD, true, START_HOLD - BLOCK_SAMPLES as u32),
            (OPEN_THRESHOLD, false, HOLD_SAMPLES),
            (OPEN_THRESHOLD, true, START_HOLD),
        ] {
            let mut state = initial_state();
            let threshold = f32::from_bits(state[threshold_word]);
            let level = if below {
                threshold.next_down()
            } else {
                threshold
            };
            state[ENVELOPE] = level.to_bits();
            state[OPEN] = 1;
            state[HOLD] = START_HOLD;
            SimpleGate.process_pair(&mut state, &mut [[level; BLOCK_SAMPLES]; 2]);
            assert_eq!(
                state[HOLD], expected_hold,
                "boundary={threshold_word}, below={below}"
            );
            assert_eq!(state[OPEN], 1);
            let coefficient = if threshold_word == CLOSE_THRESHOLD && below {
                DETECTOR_SLOW
            } else {
                DETECTOR_FAST
            };
            assert_eq!(state[COEFFICIENT], coefficient.to_bits());
        }
    }

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
            SimpleGate.static_layout()
        }
        fn process_cycles(&self) -> u64 {
            PROCESS_CYCLES
        }
        fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
            emit_body(program, &self.0)
        }
    }

    impl HostReference for Mutant {
        fn process(&self, state: &mut [u32], samples: &mut [f32; BLOCK_SAMPLES]) {
            SimpleGate.process(state, samples);
        }
        fn process_stereo(&self, state: &mut [u32], samples: &mut [[f32; BLOCK_SAMPLES]; 2]) {
            SimpleGate.process_stereo(state, samples);
        }
        fn set_parameter(&self, state: &mut [u32], index: u32, value: f32) {
            SimpleGate.set_parameter(state, index, value);
        }
        fn reset(&self, state: &mut [u32]) {
            SimpleGate.reset(state);
        }
        fn recover(&self, state: &mut [u32]) {
            SimpleGate.recover(state);
        }
    }

    /// One changed immediate per law part fails V2 at its first effect: the
    /// hysteresis at the start reset, the fast coefficient at the first closed
    /// sample, mono scale and hold at the first opening (frame 1), the skew
    /// at the THRESHOLD update, and the slow coefficient when the frame-64
    /// burst decays below close while the hold runs (frame 80).
    #[test]
    fn each_changed_law_constant_fails_v2_where_it_acts() {
        let mut frames = scheduled_frames(64, &[(2, 0, 0.5)]);
        let burst = |level| VerificationFrame {
            samples: [[level; BLOCK_SAMPLES]; 2],
            reset: false,
            parameters: Vec::new(),
        };
        frames.push(burst(0.5));
        frames.extend(std::iter::repeat_n(burst(0.0), 20));
        verify_module(&SimpleGate, &frames).unwrap();
        let cases = [
            (
                Law {
                    hysteresis: 6.0,
                    ..LAW
                },
                0,
                "reset state",
                CLOSE_THRESHOLD,
            ),
            (
                Law {
                    fast: DETECTOR_FAST.next_up(),
                    ..LAW
                },
                0,
                "state",
                COEFFICIENT,
            ),
            (
                Law {
                    mono_scale: 0.25,
                    ..LAW
                },
                1,
                "state",
                ENVELOPE,
            ),
            (
                Law {
                    hold: HOLD_SAMPLES - 1,
                    ..LAW
                },
                1,
                "state",
                HOLD,
            ),
            (
                Law { skew: 0.5, ..LAW },
                2,
                "parameter state",
                PENDING_THRESHOLD,
            ),
            (
                Law {
                    slow: DETECTOR_SLOW.next_up(),
                    ..LAW
                },
                80,
                "state",
                COEFFICIENT,
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

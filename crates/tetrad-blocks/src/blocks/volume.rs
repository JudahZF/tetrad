//! Volume (stock model 16006): LEVEL through a Linear or Log CURVE, then the kernel smoother's
//! bounded linear gain ramp, in place on 32 samples (setter `0x233`, conversion `0x192`, smoother
//! k1 `0x161ac1`, `0x16278c`, `0x1627ac`, process loop `0x08c`).
//!
//! - LEVEL (0) stores v raw. CURVE (1) is Log when v > 0.5. Other indices do
//!   nothing. Both recompute the target: Linear → v; Log → v when v reads as
//!   zero, else 10^(y/20) with y interpolated over (0, −120), (0.12, −52.8),
//!   (0.25, −30), (1, 0) dB; a breakpoint belongs to the segment below it.
//! - Only a changed target sets the per-block bound |target − gain|/30. Each
//!   block: gain += 32·step; step = clip(target − gain, ±bound)/32; |step| <
//!   1e-6 snaps gain to target with step 0. The samples get gain + n·step in
//!   the stock SIMD lane order: even and odd samples each add 2·step per pair.
//!   The last ramp block lands exactly (Sterbenz), so the snap acts on small
//!   changes: |Δtarget| < 30·32·1e-6 ≈ 9.6e-4 jumps at once instead of ramping.
//! - Reset (and fault recovery) snaps gain to target. The initial state is the
//!   stock code init (LEVEL 1.0, Linear, gain and target 0), so the first
//!   parameter push ramps the block in over 30 blocks (20 ms at 48 kHz).
//!
//! Owned (I): segment slopes are f32 divides of the breakpoints and the rate is
//! f32 1/30 (stock divides at run time); 10^(y/20) is `math::db_to_linear`
//! (stock: kernel powf); the snap bound is 1e-6 (stock "≈ 1e-6"). Each channel
//! keeps its own ramp. Both get the same calls, so the ramps are equal, as the
//! one stock ramp that feeds both channels.

use std::num::NonZeroU16;

use tetrad_sharc::emit::{BranchRelocation, IsaProgram, Label, LoopCount};
use tetrad_sharc::isa48::{BranchKind, Condition, DmAccess, Instruction48};
use tetrad_sharc::math::{self, Leaf, prm};
use tetrad_sharc::register::{Dag1Index, Dag1Modify, DataReg, FloatReg, UniversalReg};

use crate::{
    BLOCK_SAMPLES, BlockError, HostReference, OwnedBlock, ParameterLaw, ParameterSpec,
    RoutineLabels, StaticLayout,
};

/// The deployment ID is the stock ID it replaces. The opt-in replacement map
/// and the owned ID range are wave 2 (`w4-f-wire`).
pub const STOCK_MODEL_ID: u32 = 16006;

/// ModelRepo: LEVEL 0–100 % (no skew), CURVE Linear/Log.
pub const PARAMETERS: &[ParameterSpec] = &[
    ParameterSpec {
        runtime_index: 0,
        name: "LEVEL",
        law: ParameterLaw::Range {
            minimum: 0.0,
            maximum: 100.0,
            skew: 1.0,
        },
    },
    ParameterSpec {
        runtime_index: 1,
        name: "CURVE",
        law: ParameterLaw::Switch,
    },
];

/// Modeled steps of one mono process call (snap path): 23 before the loop,
/// 8 per sample pair, RTS.
pub const PROCESS_CYCLES: u64 = 23 + 8 * PAIRS.get() as u64 + 1;

/// The owned Volume block. Static state: [`Word`] per channel.
#[derive(Debug, Clone, Copy)]
pub struct Volume;

/// One channel state word (32 bits each). Gain, step, target and bound are
/// the kernel smoother words `S[0]`, `S[1]`, `S[2]`, `S[4]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word {
    /// Delivered LEVEL v, raw f32 bits.
    Level,
    /// 1 = Log, 0 = Linear.
    Curve,
    /// Gain at the start of the current block.
    Gain,
    /// Per-sample gain step of the current block.
    Step,
    Target,
    /// Largest change per block: |target − gain when it changed|·rate.
    Bound,
}

impl Word {
    pub const COUNT: usize = 6;
}

/// One Log segment: y = (LEVEL − x0)·slope + y0 dB for LEVEL ≤ `upper`.
#[derive(Debug, Clone, Copy)]
struct Segment {
    upper: f32,
    x0: f32,
    y0: f32,
    slope: f32,
}

const fn segment((x0, y0): (f32, f32), (upper, y1): (f32, f32)) -> Segment {
    Segment {
        upper,
        x0,
        y0,
        slope: (y1 - y0) / (upper - x0),
    }
}

/// Constants of the emitted body and the twin: one source, so V2 compares code
/// against the same law, and V5 can change one immediate in the code only.
#[derive(Debug, Clone, Copy)]
struct Law {
    /// Bound per block over the step size: 32/(48000·0.02) = 1/30.
    rate: f32,
    /// A per-sample step below this snaps the gain to the target.
    snap: f32,
    /// CURVE is Log when v is greater.
    curve: f32,
    segments: [Segment; 3],
}

const LAW: Law = Law {
    rate: 1.0 / 30.0,
    snap: 1e-6,
    curve: 0.5,
    segments: [
        segment((0.0, -120.0), (0.12, -52.8)),
        segment((0.12, -52.8), (0.25, -30.0)),
        segment((0.25, -30.0), (1.0, 0.0)),
    ],
};

const BLOCK: f32 = BLOCK_SAMPLES as f32;
const PAIRS: NonZeroU16 = match NonZeroU16::new(BLOCK_SAMPLES as u16 / 2) {
    Some(pairs) => pairs,
    None => panic!("a block holds sample pairs"),
};

impl OwnedBlock for Volume {
    fn model_id(&self) -> u32 {
        STOCK_MODEL_ID
    }
    fn parameters(&self) -> &'static [ParameterSpec] {
        PARAMETERS
    }
    fn static_layout(&self) -> StaticLayout {
        let mut channel = vec![0; Word::COUNT];
        channel[Word::Level as usize] = 1.0_f32.to_bits();
        StaticLayout::Channels([channel.clone(), channel])
    }
    fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
        emit(program, &LAW)
    }
    fn process_cycles(&self) -> u64 {
        PROCESS_CYCLES
    }
}

impl HostReference for Volume {
    fn process(&self, state: &mut [u32], samples: &mut [f32; BLOCK_SAMPLES]) {
        let word = |name: Word| f32::from_bits(state[name as usize]);
        let gain = prm::add(word(Word::Gain), prm::mul(word(Word::Step), BLOCK));
        let difference = prm::sub(word(Word::Target), gain);
        let step = prm::mul(clip(difference, word(Word::Bound)), 1.0 / BLOCK);
        let (gain, step) = if prm::operand(step).abs() < LAW.snap {
            (word(Word::Target), 0.0)
        } else {
            (gain, step)
        };
        state[Word::Gain as usize] = gain.to_bits();
        state[Word::Step as usize] = step.to_bits();
        let (mut even, mut odd, pair) = (gain, prm::add(gain, step), prm::add(step, step));
        for samples in samples.chunks_exact_mut(2) {
            samples[0] = prm::mul(samples[0], even);
            samples[1] = prm::mul(samples[1], odd);
            even = prm::add(even, pair);
            odd = prm::add(odd, pair);
        }
    }

    fn set_parameter(&self, state: &mut [u32], index: u32, value: f32) {
        match index {
            0 => state[Word::Level as usize] = value.to_bits(),
            1 => state[Word::Curve as usize] = u32::from(prm::operand(value) > LAW.curve),
            _ => return,
        }
        let level = f32::from_bits(state[Word::Level as usize]);
        let target = if state[Word::Curve as usize] == 0 || prm::operand(level) == 0.0 {
            level
        } else {
            let (last, rest) = LAW.segments.split_last().expect("three segments");
            let s = rest
                .iter()
                .find(|s| prm::operand(level) <= s.upper)
                .unwrap_or(last);
            math::db_to_linear(prm::add(prm::mul(prm::sub(level, s.x0), s.slope), s.y0))
        };
        let previous = f32::from_bits(state[Word::Target as usize]);
        if prm::operand(target) == prm::operand(previous) {
            return;
        }
        let gain = f32::from_bits(state[Word::Gain as usize]);
        let bound = prm::mul(prm::sub(target, gain).abs(), LAW.rate);
        state[Word::Target as usize] = target.to_bits();
        state[Word::Bound as usize] = bound.to_bits();
    }

    fn reset(&self, state: &mut [u32]) {
        state[Word::Gain as usize] = state[Word::Target as usize];
        state[Word::Step as usize] = 0;
    }
}

/// ISA `CLIP x BY bound` under the PRM operand rule.
fn clip(x: f32, bound: f32) -> f32 {
    let (x, bound) = (prm::operand(x), prm::operand(bound).abs());
    if x.abs() < bound {
        x
    } else {
        bound.copysign(x)
    }
}

const R0: DataReg = DataReg::of(0);
const R1: DataReg = DataReg::of(1);
const R2: DataReg = DataReg::of(2);
const R4: DataReg = DataReg::of(4);
const R8: DataReg = DataReg::of(8);
const R12: DataReg = DataReg::of(12);
const F0: FloatReg = FloatReg::of(0);
const F1: FloatReg = FloatReg::of(1);
const F2: FloatReg = FloatReg::of(2);
const F4: FloatReg = FloatReg::of(4);
const F8: FloatReg = FloatReg::of(8);
const F12: FloatReg = FloatReg::of(12);
/// The channel state; the process loop reuses it for the samples.
const I4: Dag1Index = Dag1Index::of(4);
/// C run-time constant 1: one normal word, which the DAG scales in byte space.
const M6: Dag1Modify = Dag1Modify::of(6);

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

fn set(dst: DataReg, value: f32) -> Instruction48 {
    Instruction48::Immediate {
        dst: UniversalReg::Data(dst),
        value: value.to_bits(),
    }
}

/// `x = DM(0, I4)`, `Fx = Fx * gain`, `DM(I4, M6) = x`.
fn scale_sample(x: DataReg, gain: FloatReg) -> [Instruction48; 3] {
    [
        Instruction48::DmOffsetLoad {
            dst: x,
            index: I4,
            offset: 0,
        },
        Instruction48::FloatMultiply {
            dst: x.into(),
            lhs: x.into(),
            rhs: gain,
        },
        Instruction48::DmStore {
            src: x,
            index: I4,
            modifier: M6,
            access: DmAccess::PostModify,
        },
    ]
}

fn push(
    program: &mut IsaProgram,
    instructions: impl IntoIterator<Item = Instruction48>,
) -> Result<(), BlockError> {
    for instruction in instructions {
        program.push(instruction)?;
    }
    Ok(())
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

fn emit(program: &mut IsaProgram, law: &Law) -> Result<RoutineLabels, BlockError> {
    use Instruction48::{FloatAbs, FloatAdd, FloatClip, FloatCompare, FloatMultiply};
    use Instruction48::{FloatSubtract, IntegerCompare, IntegerPass, Rts};
    let (jump, call) = (BranchKind::Jump, BranchKind::Call);
    let labels = RoutineLabels {
        process: Label::new("volume.process"),
        set_parameter: Label::new("volume.set_parameter"),
        reset: Label::new("volume.reset"),
    };
    let label = |name: &str| Label::new(format!("volume.{name}"));
    let (ramp, last) = (label("ramp"), label("loop_end"));
    let (curve, curve_store, convert) = (label("curve"), label("curve_store"), label("convert"));
    let (interpolate, retarget, done) = (label("interpolate"), label("retarget"), label("done"));

    // process: R4 = I4 = state, R8 = 32 samples.
    program.define(labels.process.clone())?;
    push(
        program,
        [
            load(R0, Word::Gain),
            load(R1, Word::Step),
            set(R2, BLOCK),
            FloatMultiply {
                dst: F1,
                lhs: F1,
                rhs: F2,
            },
            FloatAdd {
                dst: F0,
                lhs: F0,
                rhs: F1,
            },
            load(R1, Word::Target),
            FloatSubtract {
                dst: F1,
                lhs: F1,
                rhs: F0,
            },
            load(R2, Word::Bound),
            FloatClip {
                dst: F1,
                src: F1,
                bound: F2,
            },
            set(R2, 1.0 / BLOCK),
            FloatMultiply {
                dst: F1,
                lhs: F1,
                rhs: F2,
            },
            FloatAbs { dst: F2, src: F1 },
            set(R12, law.snap),
            FloatCompare { lhs: F2, rhs: F12 },
        ],
    )?;
    branch(program, jump, Condition::GreaterOrEqual, &ramp)?;
    push(program, [load(R0, Word::Target), set(R1, 0.0)])?;
    program.define(ramp)?;
    push(
        program,
        [
            store(R0, Word::Gain),
            store(R1, Word::Step),
            // Lanes: F0 even samples, F2 odd samples, F1 = 2·step per pair.
            FloatAdd {
                dst: F2,
                lhs: F0,
                rhs: F1,
            },
            FloatAdd {
                dst: F1,
                lhs: F1,
                rhs: F1,
            },
            Instruction48::RegisterMove {
                dst: UniversalReg::Index(I4),
                src: UniversalReg::Data(R8),
            },
        ],
    )?;
    program.counted_loop(LoopCount::Immediate(PAIRS), last.clone())?;
    push(program, scale_sample(R4, F0))?;
    push(program, scale_sample(R8, F2))?;
    push(
        program,
        [FloatAdd {
            dst: F0,
            lhs: F0,
            rhs: F1,
        }],
    )?;
    program.define(last)?;
    push(
        program,
        [
            FloatAdd {
                dst: F2,
                lhs: F2,
                rhs: F1,
            },
            Rts,
        ],
    )?;

    // set-parameter: R8 = index, F12 = value in 0..=1.
    program.define(labels.set_parameter.clone())?;
    push(program, [IntegerPass { dst: R8, src: R8 }])?;
    branch(program, jump, Condition::NotEqual, &curve)?;
    push(program, [store(R12, Word::Level)])?;
    branch(program, jump, Condition::Always, &convert)?;
    program.define(curve)?;
    push(
        program,
        [
            Instruction48::Immediate {
                dst: UniversalReg::Data(R0),
                value: 1,
            },
            IntegerCompare { lhs: R8, rhs: R0 },
        ],
    )?;
    branch(program, jump, Condition::NotEqual, &done)?;
    push(
        program,
        [set(R1, law.curve), FloatCompare { lhs: F12, rhs: F1 }],
    )?;
    branch(program, jump, Condition::GreaterThan, &curve_store)?;
    push(program, [set(R0, 0.0)])?;
    program.define(curve_store)?;
    push(program, [store(R0, Word::Curve)])?;

    // Target in F0: Linear, or Log at zero, keeps the level.
    program.define(convert)?;
    push(
        program,
        [
            load(R0, Word::Level),
            load(R1, Word::Curve),
            IntegerPass { dst: R1, src: R1 },
        ],
    )?;
    branch(program, jump, Condition::Equal, &retarget)?;
    push(program, [set(R1, 0.0), FloatCompare { lhs: F0, rhs: F1 }])?;
    branch(program, jump, Condition::Equal, &retarget)?;
    let (last_segment, segments) = law.segments.split_last().expect("three segments");
    for (k, s) in segments.iter().enumerate() {
        let above = label(&format!("above_{k}"));
        push(
            program,
            [set(R8, s.upper), FloatCompare { lhs: F0, rhs: F8 }],
        )?;
        branch(program, jump, Condition::GreaterThan, &above)?;
        push(program, [set(R1, s.x0), set(R2, s.slope), set(R12, s.y0)])?;
        branch(program, jump, Condition::Always, &interpolate)?;
        program.define(above)?;
    }
    let s = last_segment;
    push(program, [set(R1, s.x0), set(R2, s.slope), set(R12, s.y0)])?;
    program.define(interpolate)?;
    push(
        program,
        [
            FloatSubtract {
                dst: F0,
                lhs: F0,
                rhs: F1,
            },
            FloatMultiply {
                dst: F0,
                lhs: F0,
                rhs: F2,
            },
            FloatAdd {
                dst: F4,
                lhs: F0,
                rhs: F12,
            },
        ],
    )?;
    branch(program, call, Condition::Always, &Leaf::DbToLinear.label())?;

    // An unchanged target keeps the ramp and its bound (k1 0x16278c).
    program.define(retarget)?;
    push(
        program,
        [load(R1, Word::Target), FloatCompare { lhs: F0, rhs: F1 }],
    )?;
    branch(program, jump, Condition::Equal, &done)?;
    push(
        program,
        [
            store(R0, Word::Target),
            load(R1, Word::Gain),
            FloatSubtract {
                dst: F1,
                lhs: F0,
                rhs: F1,
            },
            FloatAbs { dst: F1, src: F1 },
            set(R2, law.rate),
            FloatMultiply {
                dst: F1,
                lhs: F1,
                rhs: F2,
            },
            store(R1, Word::Bound),
        ],
    )?;
    program.define(done)?;
    push(program, [Rts])?;

    // reset: snap to the target.
    program.define(labels.reset.clone())?;
    push(
        program,
        [
            load(R0, Word::Target),
            store(R0, Word::Gain),
            set(R0, 0.0),
            store(R0, Word::Step),
            Rts,
        ],
    )?;
    math::emit(program, &[Leaf::DbToLinear])?;
    Ok(labels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verification::{VerificationError, scheduled_frames, verify_module};

    /// The body emitted with a changed law against the stock twin (roadmap V5).
    struct Mutant(Law);

    impl OwnedBlock for Mutant {
        fn model_id(&self) -> u32 {
            Volume.model_id()
        }
        fn parameters(&self) -> &'static [ParameterSpec] {
            PARAMETERS
        }
        fn static_layout(&self) -> StaticLayout {
            Volume.static_layout()
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
            Volume.process(state, samples);
        }
        fn set_parameter(&self, state: &mut [u32], index: u32, value: f32) {
            Volume.set_parameter(state, index, value);
        }
        fn reset(&self, state: &mut [u32]) {
            Volume.reset(state);
        }
    }

    /// One changed immediate per law part fails V2 at its first effect: the
    /// rate at the first bound, the snap at a 1 → 0.9995 change (below 9.6e-4
    /// stock jumps, the mutant ramps), the CURVE threshold at v = 0.5, and a
    /// segment-1 slope at its target.
    #[test]
    fn each_changed_law_constant_fails_v2_where_it_acts() {
        let mut slope = LAW;
        slope.segments[1].slope = slope.segments[1].slope.next_up();
        let cases = [
            (
                Law {
                    rate: 1.0 / 32.0,
                    ..LAW
                },
                0,
                "parameter state",
                Word::Bound as usize,
            ),
            (Law { snap: 0.0, ..LAW }, 35, "audio", 0),
            (
                Law {
                    curve: LAW.curve.next_down(),
                    ..LAW
                },
                40,
                "parameter state",
                Word::Curve as usize,
            ),
            (slope, 42, "parameter state", Word::Target as usize),
        ];
        let updates = [
            (0, 0, 1.0),
            (35, 0, 0.9995),
            (40, 1, 0.5),
            (41, 1, 1.0),
            (42, 0, 0.2),
        ];
        let frames = scheduled_frames(64, &updates);
        verify_module(&Mutant(LAW), &frames).unwrap();
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

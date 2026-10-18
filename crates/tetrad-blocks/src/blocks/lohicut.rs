//! Low-High Cut (stock model 4003): a Butterworth high-pass side and low-pass side of order n =
//! SLOPE step (6n dB/oct), an OUTPUT gain, then the biquad cascade leaf ([`crate::dsp::biquad`]).
//!
//! - SLOPE (0 HPF, 2 LPF): n = floor(8v + 0.5), 0..=8 (C). FREQ (1, 3):
//!   f = P(v; 20, 20000, 0.3) = 20 + 19980·exp2(log2(v)/0.3) Hz (C law, owned
//!   leaves). OUTPUT (4): gain = 10^((40v − 20)/20) (C law, `db_to_linear` for
//!   the kernel powf). Other indices do nothing.
//! - Design (C, laws §2): bilinear transform, K = tan(π·f/48000) at the fixed
//!   design rate. ⌊n/2⌋ biquads with d = 2 sin((2i + 1)π/(2n)) and
//!   a0 = 1 + d·K + K²: a1 = 2(K² − 1)/a0, a2 = (1 − d·K + K²)/a0; HPF
//!   b = [1, −2, 1]/a0, LPF b = K²·[1, 2, 1]/a0. Odd n adds one first-order
//!   section: a1 = (K − 1)/(1 + K); HPF b = [1, −1]/(1 + K), LPF b = K·[1, 1]/(1 + K).
//!   Identity sections pad each side to 4. Sections 0..4 are HPF, 4..8 LPF.
//! - Design accuracy (owned; equal in exact arithmetic): poles near z = 1 make
//!   s = 1 + a1 + a2 = 4K²/a0 tiny (7e-6 at 20 Hz). Rounding a1 ≈ −2 to f32
//!   alone then moves the cutoff by about 2 % and the LPF DC gain by 0.5 dB.
//!   So a2 = 4K²/a0 − (1 + a1), which keeps s to the rounding of a2, and LPF
//!   b0 = ((1 + a1) + a2)/4 (first order: (1 + a1)/2) from the stored poles,
//!   so the LPF DC gain is 1. HPF zeros sit exactly at z = 1 (b1 = −2·b0).
//! - Glide: set-parameter starts a 30-block linear ramp (20 ms) of the FREQ
//!   value, step (target − value at change)/30, that lands exactly on block 30;
//!   an unchanged value lands at once. While the ramp moves, or after a SLOPE
//!   change, process redesigns that side at block start (control rate, math
//!   leaves). The cascade then glides the live coefficients toward the new
//!   targets (factor 1/2, snap below 1e-5).
//! - OUTPUT: the same ramp on the gain (stock rate 1/30, laws §2); sample n of
//!   a block gets g + (n + 1)·Δ/32. The gain runs before the cascade, as in
//!   stock (gain loop `0xf2..0x11a`, cascade call `0x162`).
//! - Reset (R8 = 1: start, flagged slots, `0x1f` EnableAll on a same-layout
//!   recall): the ramps land and the reset sets both redesign flags and a
//!   settle flag. The next process call redesigns both sides and sets
//!   live = target at block start, before any sample, so the output equals a
//!   design at reset. The reset stores single words only: a frame can preempt
//!   it, and stock `0x9ae` writes coefficients only inside the kernel critical
//!   section (review N1). The history is kept, so a live reset gives no step
//!   transient (I: stock `ctx[16]` → `0x9ae` lands the ramps and redesigns;
//!   no history clear was found, review F6). At start the history is zero
//!   (section 1). Fault recovery (R8 = 0) also zeroes the history first.
//! - Cost per 32-sample mono call (modeled steps): cascade 3404 (with the
//!   RND32 bracket of the section loop), OUTPUT 149, each redesigning side
//!   574, the settle after a reset 97; bound [`PROCESS_CYCLES`] = 4800. Per
//!   stereo callback: bound 9625, V2 maximum 9371 (both sides ramping at
//!   order 8).
//!
//! Owned (I): the FREQ ramp ticks once per block in process (stock ticks it in
//! the background callback `ctx[7]`, 50 or 200 steps; owned blocks install no
//! `ctx[7]`); a block counter instead of the stock bound clip, which can leave a
//! rounding tail; the glide factor (stock factor not decoded); DF1, its sum
//! order and the silence snap at 1e-20 (stock section form not decoded; the
//! RND32 bracket around the section loop is stock, k1 `0x162c50`); the
//! accurate a2 and LPF b0 above; d from the `sin` leaf (stock: `cos` after a
//! double-precision step); the initial state HPF Flat at 20 Hz, LPF Flat at
//! 20 kHz, OUTPUT 0 dB (stock code init not decoded; ZenUI pushes all five).
//!
//! **19 Hz "off" (choice).** The FREQ law starts at 20 Hz (C), so this block
//! has no frequency off: a side is off only at SLOPE Flat, where its sections
//! are identity and pass x exactly. The 19 Hz OFF is the Graphic-9 HPF (laws
//! §3, O). The leaf's rule for it: an off side gets identity targets and
//! reaches them through the same glide (no bypass switch, no 19 Hz filter).
use std::f32::consts::PI;
use std::num::NonZeroU16;

use tetrad_sharc::emit::{IsaProgram, Label, LoopCount};
use tetrad_sharc::isa48::{Condition, DmAccess, Instruction48};
use tetrad_sharc::math::{self, Leaf, prm};
use tetrad_sharc::register::{DataReg, UniversalReg};

use crate::dsp::biquad::{
    Asm, Cascade, Glide, I4, I12, I13, M6, R0, R1, R2, R3, R4, R5, R6, R7, R8, R9, R10, R12,
    Section, restore, restore_cycles, save,
};
use crate::{
    BLOCK_SAMPLES, BlockError, HostReference, OwnedBlock, ParameterLaw, ParameterSpec,
    RoutineLabels, StaticLayout,
};

/// The deployment ID is the stock ID it replaces (opt-in map: wave 2).
pub const STOCK_MODEL_ID: u32 = 4003;

/// Display names (I): slope k is 6k dB/oct.
const SLOPES: &[&str] = &[
    "Flat",
    "6 dB/oct",
    "12 dB/oct",
    "18 dB/oct",
    "24 dB/oct",
    "30 dB/oct",
    "36 dB/oct",
    "42 dB/oct",
    "48 dB/oct",
];
const FREQUENCY: ParameterLaw = ParameterLaw::Range {
    minimum: 20.0,
    maximum: 20_000.0,
    skew: 0.3,
};

pub const PARAMETERS: &[ParameterSpec] = &[
    ParameterSpec {
        runtime_index: 0,
        name: "HPF SLOPE",
        law: ParameterLaw::Choices(SLOPES),
    },
    ParameterSpec {
        runtime_index: 1,
        name: "HPF FREQ",
        law: FREQUENCY,
    },
    ParameterSpec {
        runtime_index: 2,
        name: "LPF SLOPE",
        law: ParameterLaw::Choices(SLOPES),
    },
    ParameterSpec {
        runtime_index: 3,
        name: "LPF FREQ",
        law: FREQUENCY,
    },
    ParameterSpec {
        runtime_index: 4,
        name: "OUTPUT",
        law: ParameterLaw::Range {
            minimum: -20.0,
            maximum: 20.0,
            skew: 1.0,
        },
    },
];

pub const SECTIONS_PER_SIDE: usize = 4;
/// HPF sections, then LPF sections, at word 0 of each channel state.
pub const CASCADE: Cascade = Cascade::new(
    2 * SECTIONS_PER_SIDE as u16,
    Glide {
        factor: 0.5,
        snap: 1e-5,
    },
    1e-20,
);

/// Side records follow the cascade: `[order n, redesign flag, FREQ ramp]`.
const SIDES: usize = CASCADE.words();
const SIDE_WORDS: usize = 2 + RAMP_WORDS;
/// The OUTPUT gain ramp.
const GAIN: Ramp = Ramp(SIDES + 2 * SIDE_WORDS);
/// Set by reset: the next process settles the cascade (live = target).
const SETTLE: usize = GAIN.0 + RAMP_WORDS;
pub const STATE_WORDS: usize = SETTLE + 1;

/// A linear ramp: `[current, target, step, blocks left]` from the word offset.
#[derive(Debug, Clone, Copy)]
struct Ramp(usize);

const RAMP_WORDS: usize = 4;

impl Ramp {
    const fn current(self) -> usize {
        self.0
    }
    const fn target(self) -> usize {
        self.0 + 1
    }
    const fn step(self) -> usize {
        self.0 + 2
    }
    const fn count(self) -> usize {
        self.0 + 3
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    High,
    Low,
}

impl Side {
    pub const ALL: [Self; 2] = [Self::High, Self::Low];

    const fn order(self) -> usize {
        SIDES + SIDE_WORDS * self as usize
    }
    const fn dirty(self) -> usize {
        self.order() + 1
    }
    const fn frequency(self) -> Ramp {
        Ramp(self.order() + 2)
    }
    pub const fn first_section(self) -> usize {
        SECTIONS_PER_SIDE * self as usize
    }
    fn name(self) -> &'static str {
        match self {
            Self::High => "hpf",
            Self::Low => "lpf",
        }
    }
}

/// Constants of the emitted body and the twin: one source, so V2 compares code
/// against the same law, and V5 can change one constant in the code only.
#[derive(Debug, Clone, Copy)]
pub struct Law {
    cascade: Cascade,
    /// FREQ: f = minimum + span·exp2(log2(v)·exponent), exponent = 1/skew.
    minimum: f32,
    span: f32,
    exponent: f32,
    /// π/fs at the design rate 48000 (stock immediate at `0xa7b`).
    pi_over_rate: f32,
    /// Ramp length in blocks (30: 20 ms at 48 kHz) and its f32 reciprocal.
    ramp_blocks: u32,
    rate: f32,
    /// OUTPUT dB = scale·v − offset.
    db_scale: f32,
    db_offset: f32,
}

pub const LAW: Law = Law {
    cascade: CASCADE,
    minimum: 20.0,
    span: 19_980.0,
    exponent: 1.0 / 0.3,
    pi_over_rate: (std::f64::consts::PI / 48_000.0) as f32,
    ramp_blocks: 30,
    rate: 1.0 / 30.0,
    db_scale: 40.0,
    db_offset: 20.0,
};

const BLOCK: f32 = BLOCK_SAMPLES as f32;
const SAMPLES: NonZeroU16 = match NonZeroU16::new(BLOCK_SAMPLES as u16) {
    Some(samples) => samples,
    None => panic!("a block has samples"),
};

/// Leaf maxima (`tetrad_sharc::math` table) plus the CALL.
const LOG2: u64 = 1 + 45;
const EXP2: u64 = 1 + 27;
const TAN: u64 = 1 + 70;
const SIN: u64 = 1 + 42;
const RECIPROCAL: u64 = 1 + 20;
const DESIGN_SAVED: [DataReg; 6] = [R3, R5, R6, R7, R9, R10];
/// Compare and branch; d = 2 sin; a0; reciprocal; a1, s, 1 + a1, a2 and
/// their stores; LPF b (7); JUMP.
const BIQUAD_SLOT: u64 = 4 + 2 + SIN + 3 + RECIPROCAL + 9 + 7 + 1;
/// Save; K from the FREQ value; K², p, m; φ; 4 biquads; restore, RTS.
const DESIGN: u64 = DESIGN_SAVED.len() as u64
    + (1 + LOG2 + 2 + EXP2 + 6 + TAN)
    + 6
    + (3 + RECIPROCAL + 2)
    + SECTIONS_PER_SIDE as u64 * BIQUAD_SLOT
    + restore_cycles(DESIGN_SAVED.len())
    + 1;
/// [`emit_ramp_step`] when the ramp moves on.
const RAMP_STEP: u64 = 7;
/// Flag test and branch; clear it, CALL, the settle, restore I4.
const SETTLE_PATH: u64 = 3 + 3 + CASCADE.settle_cycles() + 1;
/// Count test and branch; current; ramp step; clear the flag, CALL; design.
const SIDE: u64 = 3 + 1 + RAMP_STEP + 3 + DESIGN;
/// Current and count test; ramp step; Δ/32; I4, DO, 4 per sample; three
/// moves, JUMP.
const OUTPUT: u64 = 5 + RAMP_STEP + 3 + 2 + 4 * BLOCK_SAMPLES as u64 + 3 + 1;
/// Modeled bound of one mono process call: both sides ramp and redesign at
/// order 8, the settle after a reset, then the gain and the cascade on its
/// longest path.
pub const PROCESS_CYCLES: u64 = 2 + 2 * SIDE + SETTLE_PATH + OUTPUT + CASCADE.process_cycles();

/// SLOPE choice floor(8v + 0.5): FIX rounds half to even, so an exact half
/// adds one.
pub fn slope_order(v: f32) -> u32 {
    let scaled = prm::mul(v, 8.0);
    let nearest = scaled.round_ties_even();
    nearest as u32 + u32::from(prm::sub(scaled, nearest) == 0.5)
}

fn word(state: &[u32], at: usize) -> f32 {
    f32::from_bits(state[at])
}

/// One block of a ramp: the last block lands on the target. Returns
/// (current, new current).
fn ramp_step(state: &mut [u32], ramp: Ramp) -> (f32, f32) {
    let current = word(state, ramp.current());
    let count = state[ramp.count()];
    if count != 0 {
        state[ramp.count()] = count - 1;
        state[ramp.current()] = if count == 1 {
            state[ramp.target()]
        } else {
            prm::add(current, word(state, ramp.step())).to_bits()
        };
    }
    (current, word(state, ramp.current()))
}

impl Law {
    /// The FREQ value in Hz (twin of the design routine's first part).
    pub fn cutoff_hz(&self, v: f32) -> f32 {
        let power = math::exp2(prm::mul(math::log2(v), self.exponent));
        prm::add(prm::mul(power, self.span), self.minimum)
    }

    /// The OUTPUT value as a linear gain.
    pub fn output_gain(&self, v: f32) -> f32 {
        math::db_to_linear(prm::sub(prm::mul(v, self.db_scale), self.db_offset))
    }

    /// The side's 4 sections for order n and FREQ value v.
    pub fn design(&self, side: Side, order: u32, v: f32) -> [Section; SECTIONS_PER_SIDE] {
        let k = math::tan(prm::mul(self.cutoff_hz(v), self.pi_over_rate));
        let k2 = prm::mul(k, k);
        let p = prm::add(k2, 1.0);
        let m = prm::sub(k2, 1.0);
        let m = prm::add(m, m);
        let n = order as f32;
        let phi = prm::mul(math::reciprocal(prm::add(n, n)), PI);
        std::array::from_fn(|i| {
            let odd = 2 * i as u32 + 1;
            if order < odd {
                return Section::IDENTITY;
            }
            if order == odd {
                let r = math::reciprocal(prm::add(k, 1.0));
                let a1 = prm::mul(prm::sub(k, 1.0), r);
                return Section(match side {
                    Side::High => [r, prm::sub(0.0, r), 0.0, a1, 0.0],
                    Side::Low => {
                        let b = prm::mul(prm::add(1.0, a1), 0.5);
                        [b, b, 0.0, a1, 0.0]
                    }
                });
            }
            let d = math::sin(prm::mul(phi, odd as f32));
            let r = math::reciprocal(prm::add(p, prm::mul(prm::add(d, d), k)));
            let a1 = prm::mul(m, r);
            let s = prm::mul(k2, r);
            let s = prm::add(s, s);
            let s = prm::add(s, s);
            let c = prm::add(1.0, a1);
            let a2 = prm::sub(s, c);
            Section(match side {
                Side::High => [r, prm::sub(0.0, prm::add(r, r)), r, a1, a2],
                Side::Low => {
                    let b = prm::mul(prm::add(c, a2), 0.25);
                    [b, prm::add(b, b), b, a1, a2]
                }
            })
        })
    }

    fn redesign(&self, state: &mut [u32], side: Side) {
        let v = word(state, side.frequency().current());
        let sections = self.design(side, state[side.order()], v);
        for (i, section) in sections.into_iter().enumerate() {
            Cascade::set_target(state, side.first_section() + i, section);
        }
    }

    fn retarget(&self, state: &mut [u32], ramp: Ramp, target: f32) {
        let difference = prm::sub(target, word(state, ramp.current()));
        state[ramp.target()] = target.to_bits();
        state[ramp.step()] = prm::mul(difference, self.rate).to_bits();
        state[ramp.count()] = if difference == 0.0 {
            state[ramp.current()] = target.to_bits();
            0
        } else {
            self.ramp_blocks
        };
    }

    fn process(&self, state: &mut [u32], samples: &mut [f32; BLOCK_SAMPLES]) {
        for side in Side::ALL {
            let moving = state[side.frequency().count()] != 0;
            if !moving && state[side.dirty()] == 0 {
                continue;
            }
            if moving {
                ramp_step(state, side.frequency());
            }
            state[side.dirty()] = 0;
            self.redesign(state, side);
        }
        if state[SETTLE] != 0 {
            state[SETTLE] = 0;
            self.cascade.settle(state);
        }
        let (old, new) = ramp_step(state, GAIN);
        let step = prm::mul(prm::sub(new, old), 1.0 / BLOCK);
        let mut gain = old;
        for sample in samples.iter_mut() {
            gain = prm::add(gain, step);
            *sample = prm::mul(*sample, gain);
        }
        self.cascade.process(state, samples);
    }

    fn set_parameter(&self, state: &mut [u32], index: u32, value: f32) {
        match index {
            0 | 2 => {
                let side = Side::ALL[index as usize / 2];
                state[side.order()] = slope_order(value);
                state[side.dirty()] = 1;
            }
            1 | 3 => self.retarget(state, Side::ALL[index as usize / 2].frequency(), value),
            4 => self.retarget(state, GAIN, self.output_gain(value)),
            _ => {}
        }
    }

    /// Single-word stores only (review N1): process designs and settles.
    fn reset(&self, state: &mut [u32]) {
        for side in Side::ALL {
            let frequency = side.frequency();
            state[frequency.count()] = 0;
            state[frequency.current()] = state[frequency.target()];
            state[side.dirty()] = 1;
        }
        state[GAIN.count()] = 0;
        state[GAIN.current()] = state[GAIN.target()];
        state[SETTLE] = 1;
    }

    fn emit(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
        let labels = RoutineLabels {
            process: Label::new("lohicut.process"),
            set_parameter: Label::new("lohicut.set_parameter"),
            reset: Label::new("lohicut.reset"),
        };
        let label = |name: String| Label::new(format!("lohicut.{name}"));
        let design = Side::ALL.map(|side| label(format!("design_{}", side.name())));
        let cascade = self.cascade.emit(program)?;
        let mut a = Asm(program);

        // process: R4 = I4 = state, R8 = samples. I12 keeps the samples and
        // I13 the state across the design calls: no leaf touches a DAG register.
        a.label(&labels.process)?;
        a.mv(UniversalReg::Dag2Index(I12), UniversalReg::Data(R8))?;
        a.mv(UniversalReg::Dag2Index(I13), UniversalReg::Data(R4))?;
        for side in Side::ALL {
            let name = side.name();
            let (step, redesign, done) = (
                label(format!("{name}.step")),
                label(format!("{name}.redesign")),
                label(format!("{name}.done")),
            );
            let frequency = side.frequency();
            a.load(R2, frequency.count())?;
            a.push(Instruction48::IntegerPass { dst: R2, src: R2 })?;
            a.jump(Condition::NotEqual, &step)?;
            a.load(R2, side.dirty())?;
            a.push(Instruction48::IntegerPass { dst: R2, src: R2 })?;
            a.jump(Condition::Equal, &done)?;
            a.jump(Condition::Always, &redesign)?;
            a.label(&step)?;
            a.load(R0, frequency.current())?;
            emit_ramp_step(&mut a, frequency, &label(format!("{name}.landed")))?;
            a.label(&redesign)?;
            a.int(R0, 0)?;
            a.store(R0, side.dirty())?;
            a.call(&design[side as usize])?;
            a.label(&done)?;
        }
        let settled = label("settled".into());
        a.load(R2, SETTLE)?;
        a.push(Instruction48::IntegerPass { dst: R2, src: R2 })?;
        a.jump(Condition::Equal, &settled)?;
        a.int(R0, 0)?;
        a.store(R0, SETTLE)?;
        a.call(&cascade.settle)?;
        a.mv(UniversalReg::Index(I4), UniversalReg::Dag2Index(I13))?;
        a.label(&settled)?;
        // OUTPUT: F0 = old gain, F1 = new gain, F2 = Δ/32.
        let still = label("gain.still".into());
        a.load(R0, GAIN.current())?;
        a.mv(UniversalReg::Data(R1), UniversalReg::Data(R0))?;
        a.load(R2, GAIN.count())?;
        a.push(Instruction48::IntegerPass { dst: R2, src: R2 })?;
        a.jump(Condition::Equal, &still)?;
        emit_ramp_step(&mut a, GAIN, &label("gain.landed".into()))?;
        a.label(&still)?;
        a.sub(R2, R1, R0)?;
        a.float(R12, 1.0 / BLOCK)?;
        a.mul(R2, R2, R12)?;
        a.mv(UniversalReg::Index(I4), UniversalReg::Dag2Index(I12))?;
        let gain_end = label("gain.end".into());
        a.0.counted_loop(LoopCount::Immediate(SAMPLES), gain_end.clone())?;
        a.load_at(R1, 0)?;
        a.add(R0, R0, R2)?;
        a.mul(R1, R1, R0)?;
        a.label(&gain_end)?;
        a.push(Instruction48::DmStore {
            src: R1,
            index: I4,
            modifier: M6,
            access: DmAccess::PostModify,
        })?;
        a.mv(UniversalReg::Data(R4), UniversalReg::Dag2Index(I13))?;
        a.mv(UniversalReg::Index(I4), UniversalReg::Dag2Index(I13))?;
        a.mv(UniversalReg::Data(R8), UniversalReg::Dag2Index(I12))?;
        a.jump(Condition::Always, &cascade.process)?;

        // set-parameter: R8 = index, F12 = value in 0..=1.
        let slope = Side::ALL.map(|side| label(format!("{}.slope", side.name())));
        let frequency = Side::ALL.map(|side| label(format!("{}.frequency", side.name())));
        let output = label("output".into());
        a.label(&labels.set_parameter)?;
        let handlers = [&slope[0], &frequency[0], &slope[1], &frequency[1], &output];
        for (index, handler) in handlers.into_iter().enumerate() {
            a.int(R0, index as u32)?;
            a.push(Instruction48::IntegerCompare { lhs: R8, rhs: R0 })?;
            a.jump(Condition::Equal, handler)?;
        }
        a.push(Instruction48::Rts)?;
        for side in Side::ALL {
            let stored = label(format!("{}.order", side.name()));
            a.label(&slope[side as usize])?;
            a.float(R0, 8.0)?;
            a.mul(R0, R12, R0)?;
            a.push(Instruction48::FixToInteger {
                dst: R1,
                src: R0.into(),
            })?;
            a.push(Instruction48::FloatFromInteger {
                dst: R2.into(),
                src: R1,
            })?;
            a.sub(R2, R0, R2)?;
            a.float(R0, 0.5)?;
            a.push(Instruction48::FloatCompare {
                lhs: R2.into(),
                rhs: R0.into(),
            })?;
            a.jump(Condition::NotEqual, &stored)?;
            a.push(Instruction48::IntegerIncrement { dst: R1, src: R1 })?;
            a.label(&stored)?;
            a.store(R1, side.order())?;
            a.int(R0, 1)?;
            a.store(R0, side.dirty())?;
            a.push(Instruction48::Rts)?;

            a.label(&frequency[side as usize])?;
            self.emit_retarget(&mut a, side.frequency(), R12)?;
        }
        a.label(&output)?;
        a.float(R0, self.db_scale)?;
        a.mul(R4, R12, R0)?;
        a.float(R0, self.db_offset)?;
        a.sub(R4, R4, R0)?;
        a.call(&Leaf::DbToLinear.label())?;
        self.emit_retarget(&mut a, GAIN, R0)?;

        // reset: I4 = state, R8 = flag. A frame can preempt it (review N1),
        // so it stores single control words only: each count is clear before
        // its current lands, and the settle flag is last.
        let land = Label::new("lohicut.reset_land");
        a.label(&labels.reset)?;
        a.push(Instruction48::IntegerPass { dst: R8, src: R8 })?;
        a.jump(Condition::NotEqual, &land)?;
        self.cascade.emit_clear_history(&mut a)?;
        a.label(&land)?;
        a.int(R0, 0)?;
        a.int(R1, 1)?;
        for ramp in [Side::High.frequency(), Side::Low.frequency(), GAIN] {
            a.store(R0, ramp.count())?;
            a.load(R2, ramp.target())?;
            a.store(R2, ramp.current())?;
        }
        for side in Side::ALL {
            a.store(R1, side.dirty())?;
        }
        a.store(R1, SETTLE)?;
        a.push(Instruction48::Rts)?;

        for side in Side::ALL {
            self.emit_design(&mut a, side, &design[side as usize])?;
        }
        math::emit(
            a.0,
            &[
                Leaf::Log2,
                Leaf::Exp2,
                Leaf::Tan,
                Leaf::Sin,
                Leaf::DbToLinear,
            ],
        )?;
        Ok(labels)
    }

    /// Start a ramp to `value`, RTS: step = (value − current)·rate and
    /// `ramp_blocks` blocks; no change lands at once. Uses R1, R2, R8.
    fn emit_retarget(&self, a: &mut Asm, ramp: Ramp, value: DataReg) -> Result<(), BlockError> {
        let moving = Label::new(format!("lohicut.retarget_{}", ramp.0));
        a.load(R1, ramp.current())?;
        a.sub(R2, value, R1)?;
        a.int(R1, self.ramp_blocks)?;
        a.jump(Condition::NotEqual, &moving)?;
        a.store(value, ramp.current())?;
        a.int(R1, 0)?;
        a.label(&moving)?;
        a.float(R8, self.rate)?;
        a.mul(R2, R2, R8)?;
        a.store(value, ramp.target())?;
        a.store(R2, ramp.step())?;
        a.store(R1, ramp.count())?;
        a.push(Instruction48::Rts)
    }

    /// I4 = state; writes the side's 4 target sections ([`Law::design`]).
    /// Keeps K in R3, K² in R5, p in R6, m in R7, n in R9 and φ in R10
    /// across the leaf calls.
    fn emit_design(&self, a: &mut Asm, side: Side, entry: &Label) -> Result<(), BlockError> {
        let name = side.name();
        a.label(entry)?;
        save(a, &DESIGN_SAVED)?;
        a.load(R4, side.frequency().current())?;
        a.call(&Leaf::Log2.label())?;
        a.float(R2, self.exponent)?;
        a.mul(R4, R0, R2)?;
        a.call(&Leaf::Exp2.label())?;
        a.float(R2, self.span)?;
        a.mul(R0, R0, R2)?;
        a.float(R2, self.minimum)?;
        a.add(R0, R0, R2)?;
        a.float(R2, self.pi_over_rate)?;
        a.mul(R4, R0, R2)?;
        a.call(&Leaf::Tan.label())?;
        a.mv(UniversalReg::Data(R3), UniversalReg::Data(R0))?;
        a.mul(R5, R0, R0)?;
        a.float(R2, 1.0)?;
        a.add(R6, R5, R2)?;
        a.sub(R7, R5, R2)?;
        a.add(R7, R7, R7)?;
        a.load(R9, side.order())?;
        a.push(Instruction48::FloatFromInteger {
            dst: R4.into(),
            src: R9,
        })?;
        a.add(R4, R4, R4)?;
        a.call(&Leaf::Reciprocal.label())?;
        a.float(R2, PI)?;
        a.mul(R10, R0, R2)?;
        for i in 0..SECTIONS_PER_SIDE {
            let target = Cascade::target_word(side.first_section() + i);
            let (identity, first, next) = (
                Label::new(format!("lohicut.{name}.identity_{i}")),
                Label::new(format!("lohicut.{name}.first_order_{i}")),
                Label::new(format!("lohicut.{name}.section_{}", i + 1)),
            );
            let odd = 2 * i as u32 + 1;
            a.int(R0, odd)?;
            a.push(Instruction48::IntegerCompare { lhs: R9, rhs: R0 })?;
            a.jump(Condition::LessThan, &identity)?;
            a.jump(Condition::Equal, &first)?;
            // Biquad: r = 1/(p + 2 sin(odd·φ)·K).
            a.float(R2, odd as f32)?;
            a.mul(R4, R10, R2)?;
            a.call(&Leaf::Sin.label())?;
            a.add(R0, R0, R0)?;
            a.mul(R1, R0, R3)?;
            a.add(R4, R6, R1)?;
            a.call(&Leaf::Reciprocal.label())?;
            // a1 = m·r (R1); s = 4K²·r; c = 1 + a1 (R4); a2 = s − c (R2).
            a.mul(R1, R7, R0)?;
            a.mul(R2, R5, R0)?;
            a.add(R2, R2, R2)?;
            a.add(R2, R2, R2)?;
            a.float(R4, 1.0)?;
            a.add(R4, R4, R1)?;
            a.sub(R2, R2, R4)?;
            a.store(R1, target + 3)?;
            a.store(R2, target + 4)?;
            match side {
                Side::High => {
                    a.store(R0, target)?;
                    a.store(R0, target + 2)?;
                    a.add(R1, R0, R0)?;
                    a.int(R2, 0)?;
                    a.sub(R1, R2, R1)?;
                    a.store(R1, target + 1)?;
                }
                Side::Low => {
                    a.add(R4, R4, R2)?;
                    a.float(R12, 0.25)?;
                    a.mul(R4, R4, R12)?;
                    a.store(R4, target)?;
                    a.store(R4, target + 2)?;
                    a.add(R4, R4, R4)?;
                    a.store(R4, target + 1)?;
                }
            }
            a.jump(Condition::Always, &next)?;

            // First order: r = 1/(K + 1), a1 = (K − 1)·r.
            a.label(&first)?;
            a.float(R2, 1.0)?;
            a.add(R4, R3, R2)?;
            a.call(&Leaf::Reciprocal.label())?;
            a.float(R2, 1.0)?;
            a.sub(R1, R3, R2)?;
            a.mul(R1, R1, R0)?;
            a.store(R1, target + 3)?;
            a.int(R2, 0)?;
            a.store(R2, target + 2)?;
            a.store(R2, target + 4)?;
            match side {
                Side::High => {
                    a.store(R0, target)?;
                    a.sub(R1, R2, R0)?;
                    a.store(R1, target + 1)?;
                }
                Side::Low => {
                    a.float(R4, 1.0)?;
                    a.add(R4, R4, R1)?;
                    a.float(R12, 0.5)?;
                    a.mul(R4, R4, R12)?;
                    a.store(R4, target)?;
                    a.store(R4, target + 1)?;
                }
            }
            a.jump(Condition::Always, &next)?;

            a.label(&identity)?;
            a.float(R0, 1.0)?;
            a.store(R0, target)?;
            a.int(R0, 0)?;
            for j in 1..5 {
                a.store(R0, target + j)?;
            }
            a.label(&next)?;
        }
        restore(a, &DESIGN_SAVED)?;
        a.push(Instruction48::Rts)
    }
}

/// R0 = current, R2 = blocks left (nonzero). Moves the ramp one block:
/// R1 = new current (stored; the target on the last block), R0 unchanged.
/// Uses R8.
fn emit_ramp_step(a: &mut Asm, ramp: Ramp, landed: &Label) -> Result<(), BlockError> {
    a.push(Instruction48::IntegerDecrement { dst: R2, src: R2 })?;
    a.store(R2, ramp.count())?;
    a.load(R1, ramp.target())?;
    a.jump(Condition::Equal, landed)?;
    a.load(R8, ramp.step())?;
    a.add(R1, R0, R8)?;
    a.label(landed)?;
    a.store(R1, ramp.current())
}

/// The owned Low-High Cut block.
#[derive(Debug, Clone, Copy, Default)]
pub struct LowHighCut;

impl OwnedBlock for LowHighCut {
    fn model_id(&self) -> u32 {
        STOCK_MODEL_ID
    }
    fn parameters(&self) -> &'static [ParameterSpec] {
        PARAMETERS
    }
    fn static_layout(&self) -> StaticLayout {
        let mut channel = CASCADE.initial_state();
        channel.resize(STATE_WORDS, 0);
        let low = Side::Low.frequency();
        for at in [low.current(), low.target(), GAIN.current(), GAIN.target()] {
            channel[at] = 1.0_f32.to_bits();
        }
        StaticLayout::Channels([channel.clone(), channel])
    }
    fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
        LAW.emit(program)
    }
    fn process_cycles(&self) -> u64 {
        PROCESS_CYCLES
    }
}

impl HostReference for LowHighCut {
    fn process(&self, state: &mut [u32], samples: &mut [f32; BLOCK_SAMPLES]) {
        LAW.process(state, samples);
    }
    fn set_parameter(&self, state: &mut [u32], index: u32, value: f32) {
        LAW.set_parameter(state, index, value);
    }
    fn reset(&self, state: &mut [u32]) {
        LAW.reset(state);
    }
    fn recover(&self, state: &mut [u32]) {
        LAW.cascade.clear_history(state);
        LAW.reset(state);
    }
}

#[cfg(test)]
mod tests {
    use tetrad_sharc::math::prm::Precision;

    use super::*;
    use crate::verification::{
        VerificationError, live_reset_writes, scheduled_frames, verify_module,
    };

    /// The body emitted with a changed law against the real twin (roadmap V5).
    struct Mutant(Law);

    impl OwnedBlock for Mutant {
        fn model_id(&self) -> u32 {
            STOCK_MODEL_ID
        }
        fn parameters(&self) -> &'static [ParameterSpec] {
            PARAMETERS
        }
        fn static_layout(&self) -> StaticLayout {
            LowHighCut.static_layout()
        }
        fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
            self.0.emit(program)
        }
        fn process_cycles(&self) -> u64 {
            PROCESS_CYCLES
        }
    }

    impl HostReference for Mutant {
        fn process(&self, state: &mut [u32], samples: &mut [f32; BLOCK_SAMPLES]) {
            LowHighCut.process(state, samples);
        }
        fn set_parameter(&self, state: &mut [u32], index: u32, value: f32) {
            LowHighCut.set_parameter(state, index, value);
        }
        fn reset(&self, state: &mut [u32]) {
            LowHighCut.reset(state);
        }
        fn recover(&self, state: &mut [u32]) {
            LowHighCut.recover(state);
        }
    }

    /// One changed constant per law part fails V2 with a named mismatch: the
    /// design rate, the ramp length, the glide factor, the glide snap and the
    /// RND32 bracket of the section loop.
    #[test]
    fn each_changed_law_constant_fails_v2() {
        let frames = scheduled_frames(64, &[(0, 0, 0.5), (0, 1, 0.4), (0, 2, 1.0), (0, 3, 0.6)]);
        let glide = |factor, snap| Cascade {
            glide: Glide { factor, snap },
            ..CASCADE
        };
        let mutants = [
            Law {
                pi_over_rate: (std::f64::consts::PI / 44_100.0) as f32,
                ..LAW
            },
            Law {
                ramp_blocks: 29,
                ..LAW
            },
            Law {
                cascade: glide(0.25, 1e-5),
                ..LAW
            },
            Law {
                cascade: glide(0.5, 1e-4),
                ..LAW
            },
            Law {
                cascade: Cascade {
                    precision: Precision::Single,
                    ..CASCADE
                },
                ..LAW
            },
        ];
        verify_module(&LowHighCut, &frames).unwrap();
        for law in mutants {
            let error = verify_module(&Mutant(law), &frames).unwrap_err();
            assert!(
                matches!(error, VerificationError::Mismatch { .. }),
                "{law:?}: {error}"
            );
        }
        // The silence snap (I, 1e-20) acts only at the end of a long decay.
        let mut decay = frames;
        for frame in &mut decay[4..] {
            frame.samples = [[0.0; BLOCK_SAMPLES]; 2];
        }
        verify_module(&LowHighCut, &decay).unwrap();
        let unsnapped = Law {
            cascade: Cascade {
                silence: 0.0,
                ..CASCADE
            },
            ..LAW
        };
        assert!(matches!(
            verify_module(&Mutant(unsnapped), &decay),
            Err(VerificationError::Mismatch { .. })
        ));
    }

    /// The reset before review N1: the fixed reset, then both designs and the
    /// settle in the caller's context, where a frame can preempt them.
    struct UnfixedReset;

    impl OwnedBlock for UnfixedReset {
        fn model_id(&self) -> u32 {
            STOCK_MODEL_ID
        }
        fn parameters(&self) -> &'static [ParameterSpec] {
            PARAMETERS
        }
        fn static_layout(&self) -> StaticLayout {
            LowHighCut.static_layout()
        }
        fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
            let labels = LAW.emit(program)?;
            let reset = Label::new("unfixed.reset");
            let mut a = Asm(program);
            a.label(&reset)?;
            a.call(&labels.reset)?;
            for side in Side::ALL {
                a.call(&Label::new(format!("lohicut.design_{}", side.name())))?;
            }
            a.jump(Condition::Always, &Label::new("biquad.settle"))?;
            Ok(RoutineLabels { reset, ..labels })
        }
        fn process_cycles(&self) -> u64 {
            PROCESS_CYCLES
        }
    }

    /// Why a process call that preempts the live reset could see a partial
    /// state, or `None`.
    fn preemption_fault(block: &impl OwnedBlock) -> Option<String> {
        let writes = live_reset_writes(block).unwrap();
        for channel in 0..2 {
            let order: Vec<usize> = writes
                .iter()
                .filter(|write| write.channel == channel)
                .map(|write| write.word)
                .collect();
            if let Some(word) = order.iter().find(|&&word| word < CASCADE.words()) {
                return Some(format!("channel {channel} writes cascade word {word}"));
            }
            let at = |word| order.iter().position(|&w| w == word);
            for ramp in [Side::High.frequency(), Side::Low.frequency(), GAIN] {
                if !matches!((at(ramp.count()), at(ramp.current())), (Some(c), Some(v)) if c < v) {
                    return Some(format!("channel {channel}: ramp {} lands first", ramp.0));
                }
            }
            if order.last() != Some(&SETTLE) {
                return Some(format!("channel {channel}: the settle flag is not last"));
            }
        }
        None
    }

    /// Review N1 (V2 on `Machine`, V5): a frame can preempt a live reset. The
    /// reset writes no cascade word, so a preempting process never filters
    /// with a half-written section; it clears each ramp's count before the
    /// current lands (else the preempting ramp step leaves current one step
    /// past the target); the settle flag is last. The unfixed reset fails.
    #[test]
    fn a_preempted_live_reset_leaves_no_partial_coefficients() {
        assert_eq!(preemption_fault(&LowHighCut), None);
        let fault = preemption_fault(&UnfixedReset).unwrap();
        assert!(fault.contains("cascade word"), "{fault}");
    }
}

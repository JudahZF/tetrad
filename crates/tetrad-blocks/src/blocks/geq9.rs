//! Graphic-9 (stock model 4002): a 2nd-order high-pass, a low shelf, seven peaks and a high shelf
//! (RBJ forms, Q 1, design rate 48 kHz), then the OUTPUT gain. Ten sections of the biquad cascade
//! leaf ([`crate::dsp::biquad`]).
//!
//! - Parameters (C, stock records at state `0x244` + 20·index): 0 HPF
//!   19..250 Hz, 1..=9 bands 65 Hz … 16 kHz −12..12 dB, 10 OUTPUT −20..20 dB.
//!   v maps linearly to the record range (I: the record skew word is 1.0).
//! - HPF (C, handler `0xb7`): f = 19 + 231v; f < 20 selects the flat designer
//!   (k1 `0x163485`), so 19 Hz is OFF and the section is identity. Otherwise
//!   K = tan(π·f/48000), a0 = 1 + K/Q + K², b = [1, −2, 1]/a0,
//!   a1 = 2(K² − 1)/a0, a2 = (1 − K/Q + K²)/a0 (k1 `0x163586`).
//! - Bands (C, init `0x5fc..0x690`; handler `0x245`): g = 10^(dB/20),
//!   A = √g = 10^(dB/40), w0 = 2π·f0/48000. Peak (k1 `0x1637d0`): α = sin w0/(2Q),
//!   a0 = 1 + α/A, b = [1 + αA, −2 cos w0, 1 − αA]/a0, a = [−2 cos w0, 1 − α/A]/a0.
//!   Low shelf (k1 `0x163640`): RBJ with 2√A·α = √A·sin w0/Q. High shelf
//!   (k1 `0x163737`, I: mirror of type 6): the low shelf at π − w0 with b1 and
//!   a1 negated (z → −z), which is the RBJ high shelf.
//! - OUTPUT (C, handler `0x32`, process `0x9c5..0x9e8`): one gain
//!   10^(dB/20) for the whole block, after the cascade, no ramp.
//! - Changes are instant (C: handlers write the live coefficients inside the
//!   kernel critical section; the cascade `0xbc3` reads them directly; no ramp
//!   in k1 `0x163145`). Owned: set-parameter writes the value word and a dirty
//!   word (ABI record §6 item 3); process designs each dirty band at block
//!   start, and the cascade takes the new targets with [`Glide::INSTANT`].
//! - Reset (R8 = 1: start, flagged slots, `0x1f` EnableAll on a same-layout
//!   recall): live = target, history kept, so a live reset gives no step
//!   transient (I: review F6; stock changes are instant and no history clear
//!   was found). At start the history is zero (section 1). Fault recovery
//!   (R8 = 0) also zeroes the history. Pending dirty bands design in the next
//!   process call, before any sample, so the output equals a design at reset.
//!
//! Owned (I): Q = 1 for every band (stock: only the init writes Q);
//! A = db_to_linear(dB/2) and √A = db_to_linear(dB/4) for powf and sqrtf;
//! w0 trig of the fixed bands as f64 constants rounded once (stock: run-time
//! sinf/cosf table); a 0 dB band (v = 0.5) is an identity section, so the
//! default preset passes samples exactly; the design keeps the pole sum
//! 1 + a1 + a2 accurate (below); DF1 and the 1e-20 silence of the leaf
//! (stock: SIMD multiply-accumulate in MRF, form not decoded); the initial
//! state is the stock defaults (HPF OFF, 0 dB, OUTPUT 0 dB). The leaf copies
//! the stock RND32 bracket (C: clear at `0x991`, set at `0x9bf`, then OUTPUT
//! with RND32 = 1).
//!
//! Design accuracy: the poles of the HPF at 20 Hz and of the 65 Hz shelf sit
//! near z = 1, so 1 + a1 + a2 is small. It is computed directly (HPF 4K²/a0,
//! peak 2(1 − cos w0)·A/(A + α), shelf 4(1 − cos w0)/a0 with 1 − cos w0 in
//! f64), and a2 = sum − (1 + a1). The zeros get the same rule: b2 = the
//! numerator sum − (b0 + b1), where that sum is the pole sum (peak) or A²
//! times it (shelf). The HPF zeros are exactly at z = 1 (b1 = −2·b0).
//! Remaining error is the f32 quantum of a2 (V1: HPF 0.026 dB, low shelf
//! 0.0073 dB, peaks ≤ 0.0011 dB vs the f64 RBJ curves).
//!
//! Cost per 32-sample mono call (modeled steps, bounds): cascade 4246,
//! OUTPUT 101; a redesigning band adds HPF 135, low shelf 137, high shelf
//! 140, peak 88; a clean band 3. [`PROCESS_CYCLES`] = 5399 (all ten bands
//! redesign in one block). Per stereo callback: bound 10823, V2 maximum 10621.
use std::f64::consts::{PI, TAU};
use std::num::NonZeroU16;

use tetrad_sharc::emit::{IsaProgram, Label, LoopCount};
use tetrad_sharc::isa48::{Condition, DmAccess, Instruction48};
use tetrad_sharc::math::{self, Leaf, prm};
use tetrad_sharc::register::{DataReg, UniversalReg};

use crate::dsp::biquad::{
    Asm, Cascade, Glide, I4, I12, I13, M6, R0, R1, R2, R3, R4, R5, R6, R7, R8, R9, R10, R11, R12,
    R13, R14, Section, restore, restore_cycles, save,
};
use crate::{
    BLOCK_SAMPLES, BlockError, HostReference, OwnedBlock, ParameterLaw, ParameterSpec,
    RoutineLabels, StaticLayout,
};

/// The deployment ID is the stock ID it replaces (opt-in map: wave 2).
pub const STOCK_MODEL_ID: u32 = 4002;

const BAND_GAIN: ParameterLaw = ParameterLaw::Range {
    minimum: -12.0,
    maximum: 12.0,
    skew: 1.0,
};

const fn band(runtime_index: u32, name: &'static str) -> ParameterSpec {
    ParameterSpec {
        runtime_index,
        name,
        law: BAND_GAIN,
    }
}

/// ModelRepo names; HPF shows "OFF" at its minimum.
pub const PARAMETERS: &[ParameterSpec] = &[
    ParameterSpec {
        runtime_index: 0,
        name: "HPF",
        law: ParameterLaw::Range {
            minimum: 19.0,
            maximum: 250.0,
            skew: 1.0,
        },
    },
    band(1, "65Hz"),
    band(2, "125Hz"),
    band(3, "250Hz"),
    band(4, "500Hz"),
    band(5, "1kHz"),
    band(6, "2kHz"),
    band(7, "4kHz"),
    band(8, "8kHz"),
    band(9, "16kHz"),
    ParameterSpec {
        runtime_index: OUTPUT_INDEX,
        name: "OUTPUT",
        law: ParameterLaw::Range {
            minimum: -20.0,
            maximum: 20.0,
            skew: 1.0,
        },
    },
];

/// Section designer of one band; the frequency is f0 in Hz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Band {
    HighPass,
    LowShelf(f64),
    Peak(f64),
    HighShelf(f64),
}

/// Section k of the cascade is band k; parameter k drives it.
pub const BANDS: [Band; 10] = [
    Band::HighPass,
    Band::LowShelf(65.0),
    Band::Peak(125.0),
    Band::Peak(250.0),
    Band::Peak(500.0),
    Band::Peak(1000.0),
    Band::Peak(2000.0),
    Band::Peak(4000.0),
    Band::Peak(8000.0),
    Band::HighShelf(16_000.0),
];
const OUTPUT_INDEX: u32 = BANDS.len() as u32;

pub const CASCADE: Cascade = Cascade::new(BANDS.len() as u16, Glide::INSTANT, 1e-20);

/// After the cascade: the band values, their dirty words, the OUTPUT gain.
const VALUES: usize = CASCADE.words();
const DIRTY: usize = VALUES + BANDS.len();
const GAIN: usize = DIRTY + BANDS.len();
pub const STATE_WORDS: usize = GAIN + 1;

/// Constants of the emitted body and the twin: one source, so V2 compares code
/// against the same law, and V5 can change one constant in the code only.
#[derive(Debug, Clone, Copy)]
pub struct Law {
    cascade: Cascade,
    /// Design rate (C: state word `0x63` = 48000.0).
    rate: f64,
    /// Q of every band (I).
    q: f64,
    /// HPF f = minimum + span·v; f below `off_below` is OFF (C).
    hpf_minimum: f32,
    hpf_span: f32,
    off_below: f32,
    /// A band at this value (0 dB) is an identity section (I).
    flat: f32,
    /// Band dB = gain_db·(2v − 1).
    gain_db: f32,
    /// OUTPUT dB = output_db·(2v − 1).
    output_db: f32,
}

pub const LAW: Law = Law {
    cascade: CASCADE,
    rate: 48_000.0,
    q: 1.0,
    hpf_minimum: 19.0,
    hpf_span: 231.0,
    off_below: 20.0,
    flat: 0.5,
    gain_db: 12.0,
    output_db: 20.0,
};

const SAMPLES: NonZeroU16 = match NonZeroU16::new(BLOCK_SAMPLES as u16) {
    Some(samples) => samples,
    None => panic!("a block has samples"),
};

/// Leaf maxima (`tetrad_sharc::math` table) plus the CALL.
const DB_TO_LINEAR: u64 = 1 + 29;
const TAN: u64 = 1 + 70;
const RECIPROCAL: u64 = 1 + 20;
/// Worst paths of the designers, CALL and RTS included (see `emit_*`).
const HPF: u64 = 1 + 7 + 2 + TAN + 9 + RECIPROCAL + 7 + 5 + 1;
const SHELF: u64 = 1 + 3 + 1 + 2 * (4 + DB_TO_LINEAR) + 2 + 3 + 7 + RECIPROCAL + 16 + 1;
const PEAK: u64 = 1 + 3 + 4 + DB_TO_LINEAR + 2 + RECIPROCAL + 12 + 1;
/// Dirty test, clear, value load; 5 stores.
const BAND: u64 = 6 + 5;
/// Band constants: three immediates; the high shelf negates b1 and a1.
const CONSTANTS: u64 = 3;
const MIRROR: u64 = 3;
const DESIGN_SAVED: [DataReg; 9] = [R3, R5, R6, R7, R9, R10, R11, R13, R14];
/// OUTPUT: pointers and gain, DO, 3 per sample, RTS.
const OUTPUT: u64 = 3 + 1 + 3 * BLOCK_SAMPLES as u64 + 1;
/// Modeled bound of one mono process call: pointers, save; every band
/// redesigns; restore, arguments, CALL; the cascade on its longest path; the
/// gain.
pub const PROCESS_CYCLES: u64 = 2
    + DESIGN_SAVED.len() as u64
    + BANDS.len() as u64 * BAND
    + HPF
    + 2 * (CONSTANTS + SHELF)
    + MIRROR
    + 7 * (CONSTANTS + PEAK)
    + restore_cycles(DESIGN_SAVED.len())
    + 3
    + CASCADE.process_cycles()
    + OUTPUT;

fn word(state: &[u32], at: usize) -> f32 {
    f32::from_bits(state[at])
}

impl Law {
    /// f32 design constants of a fixed band, rounded once from f64:
    /// peak `[α, −2 cos w0, 2(1 − cos w0)]`, shelf `[sin w0/Q, cos w0, 4(1 − cos w0)]`
    /// (the high shelf at π − w0). 1 − cos w0 = 2 sin²(w0/2) keeps its digits.
    pub fn constants(&self, band: Band) -> [f32; 3] {
        let w = |f0: f64| TAU * f0 / self.rate;
        let shelf = |w: f64| {
            let t = 2.0 * (w / 2.0).sin().powi(2);
            [w.sin() / self.q, w.cos(), 4.0 * t]
        };
        let constants = match band {
            Band::HighPass => [0.0; 3],
            Band::Peak(f0) => {
                let w = w(f0);
                let t = 2.0 * (w / 2.0).sin().powi(2);
                [w.sin() / (2.0 * self.q), -2.0 * w.cos(), 2.0 * t]
            }
            Band::LowShelf(f0) => shelf(w(f0)),
            Band::HighShelf(f0) => shelf(PI - w(f0)),
        };
        constants.map(|c| c as f32)
    }

    /// The HPF frequency in Hz.
    pub fn hpf_hz(&self, v: f32) -> f32 {
        prm::add(prm::mul(v, self.hpf_span), self.hpf_minimum)
    }

    /// 10^(dB/20) of a band value.
    pub fn band_gain(&self, v: f32) -> f32 {
        math::db_to_linear(prm::sub(prm::mul(v, 2.0 * self.gain_db), self.gain_db))
    }

    pub fn output_gain(&self, v: f32) -> f32 {
        math::db_to_linear(prm::sub(prm::mul(v, 2.0 * self.output_db), self.output_db))
    }

    /// The section of `band` (an index into [`BANDS`]) for value v (twin of
    /// the designers).
    pub fn design(&self, band: usize, v: f32) -> Section {
        let [k0, k1, k2] = self.constants(BANDS[band]);
        let a = |v| math::db_to_linear(prm::sub(prm::mul(v, self.gain_db), 0.5 * self.gain_db));
        match BANDS[band] {
            Band::HighPass => {
                let f = self.hpf_hz(v);
                if prm::operand(f) < self.off_below {
                    return Section::IDENTITY;
                }
                let k = math::tan(prm::mul(f, (PI / self.rate) as f32));
                let k2 = prm::mul(k, k);
                let p = prm::add(k2, 1.0);
                let m = prm::sub(k2, 1.0);
                let m = prm::add(m, m);
                let r = math::reciprocal(prm::add(p, prm::mul(k, (1.0 / self.q) as f32)));
                let a1 = prm::mul(m, r);
                let s = prm::mul(k2, r);
                let s = prm::add(s, s);
                let s = prm::add(s, s);
                let a2 = prm::sub(s, prm::add(1.0, a1));
                Section([r, prm::sub(0.0, prm::add(r, r)), r, a1, a2])
            }
            _ if prm::operand(v) == self.flat => Section::IDENTITY,
            Band::Peak(_) => {
                let a = a(v);
                let q = prm::mul(math::reciprocal(prm::add(a, k0)), a);
                let alpha_a = prm::mul(a, k0);
                let b0 = prm::mul(prm::add(1.0, alpha_a), q);
                let a1 = prm::mul(k1, q);
                let sum = prm::mul(k2, q);
                let b2 = prm::sub(sum, prm::add(b0, a1));
                Section([b0, a1, b2, a1, prm::sub(sum, prm::add(1.0, a1))])
            }
            Band::LowShelf(_) | Band::HighShelf(_) => {
                let a = a(v);
                let root = math::db_to_linear(prm::sub(
                    prm::mul(v, 0.5 * self.gain_db),
                    0.25 * self.gain_db,
                ));
                let beta = prm::mul(root, k0);
                let plus = prm::add(a, 1.0);
                let minus = prm::sub(a, 1.0);
                let minus_c = prm::mul(minus, k1);
                let p = prm::add(plus, minus_c);
                let m = prm::sub(plus, minus_c);
                let plus_c = prm::mul(plus, k1);
                let n1 = prm::add(minus, plus_c);
                let n2 = prm::sub(minus, plus_c);
                let r = math::reciprocal(prm::add(p, beta));
                let ar = prm::mul(a, r);
                let numerator = prm::mul(prm::mul(a, ar), k2);
                let b0 = prm::mul(prm::add(m, beta), ar);
                let b1 = prm::mul(prm::add(n2, n2), ar);
                let b2 = prm::sub(numerator, prm::add(b0, b1));
                let a1 = prm::mul(prm::mul(n1, -2.0), r);
                let a2 = prm::sub(prm::mul(k2, r), prm::add(1.0, a1));
                if matches!(BANDS[band], Band::HighShelf(_)) {
                    Section([b0, prm::sub(0.0, b1), b2, prm::sub(0.0, a1), a2])
                } else {
                    Section([b0, b1, b2, a1, a2])
                }
            }
        }
    }

    fn process(&self, state: &mut [u32], samples: &mut [f32; BLOCK_SAMPLES]) {
        for k in 0..BANDS.len() {
            if state[DIRTY + k] != 0 {
                state[DIRTY + k] = 0;
                let section = self.design(k, word(state, VALUES + k));
                Cascade::set_target(state, k, section);
            }
        }
        self.cascade.process(state, samples);
        let gain = word(state, GAIN);
        for sample in samples.iter_mut() {
            *sample = prm::mul(*sample, gain);
        }
    }

    fn set_parameter(&self, state: &mut [u32], index: u32, value: f32) {
        let k = index as usize;
        if k < BANDS.len() {
            state[VALUES + k] = value.to_bits();
            state[DIRTY + k] = 1;
        } else if index == OUTPUT_INDEX {
            state[GAIN] = self.output_gain(value).to_bits();
        }
    }

    fn emit(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
        let labels = RoutineLabels {
            process: Label::new("geq9.process"),
            set_parameter: Label::new("geq9.set_parameter"),
            reset: Label::new("geq9.reset"),
        };
        let designers = Designers {
            hpf: Label::new("geq9.design_hpf"),
            shelf: Label::new("geq9.design_shelf"),
            peak: Label::new("geq9.design_peak"),
            identity: Label::new("geq9.identity"),
        };
        let cascade = self.cascade.emit(program)?;
        let mut a = Asm(program);

        // process: R4 = I4 = state, R8 = samples. I12 keeps the samples and
        // I13 the state; no designer, leaf or cascade touches them.
        a.label(&labels.process)?;
        a.mv(UniversalReg::Dag2Index(I12), UniversalReg::Data(R8))?;
        a.mv(UniversalReg::Dag2Index(I13), UniversalReg::Data(R4))?;
        save(&mut a, &DESIGN_SAVED)?;
        for (k, band) in BANDS.into_iter().enumerate() {
            let clean = Label::new(format!("geq9.band_{k}.clean"));
            a.load(R2, DIRTY + k)?;
            a.push(Instruction48::IntegerPass { dst: R2, src: R2 })?;
            a.jump(Condition::Equal, &clean)?;
            a.int(R2, 0)?;
            a.store(R2, DIRTY + k)?;
            a.load(R4, VALUES + k)?;
            if band != Band::HighPass {
                for (register, value) in [R10, R11, R13].into_iter().zip(self.constants(band)) {
                    a.float(register, value)?;
                }
            }
            a.call(match band {
                Band::HighPass => &designers.hpf,
                Band::LowShelf(_) | Band::HighShelf(_) => &designers.shelf,
                Band::Peak(_) => &designers.peak,
            })?;
            if matches!(band, Band::HighShelf(_)) {
                a.int(R2, 0)?;
                a.sub(R5, R2, R5)?;
                a.sub(R7, R2, R7)?;
            }
            let target = Cascade::target_word(k);
            for (j, coefficient) in [R3, R5, R6, R7, R9].into_iter().enumerate() {
                a.store(coefficient, target + j)?;
            }
            a.label(&clean)?;
        }
        restore(&mut a, &DESIGN_SAVED)?;
        a.mv(UniversalReg::Data(R4), UniversalReg::Dag2Index(I13))?;
        a.mv(UniversalReg::Data(R8), UniversalReg::Dag2Index(I12))?;
        a.call(&cascade.process)?;
        // OUTPUT: one gain for the block.
        a.mv(UniversalReg::Index(I4), UniversalReg::Dag2Index(I13))?;
        a.load(R12, GAIN)?;
        a.mv(UniversalReg::Index(I4), UniversalReg::Dag2Index(I12))?;
        let gain_end = Label::new("geq9.gain_end");
        a.0.counted_loop(LoopCount::Immediate(SAMPLES), gain_end.clone())?;
        a.load_at(R1, 0)?;
        a.mul(R1, R1, R12)?;
        a.label(&gain_end)?;
        a.push(Instruction48::DmStore {
            src: R1,
            index: I4,
            modifier: M6,
            access: DmAccess::PostModify,
        })?;
        a.push(Instruction48::Rts)?;

        // set-parameter: R8 = index, F12 = value in 0..=1.
        let handlers: Vec<_> = (0..=OUTPUT_INDEX)
            .map(|index| Label::new(format!("geq9.parameter_{index}")))
            .collect();
        a.label(&labels.set_parameter)?;
        for (index, handler) in handlers.iter().enumerate() {
            a.int(R0, index as u32)?;
            a.push(Instruction48::IntegerCompare { lhs: R8, rhs: R0 })?;
            a.jump(Condition::Equal, handler)?;
        }
        a.push(Instruction48::Rts)?;
        for (k, handler) in handlers[..BANDS.len()].iter().enumerate() {
            a.label(handler)?;
            a.store(R12, VALUES + k)?;
            a.int(R0, 1)?;
            a.store(R0, DIRTY + k)?;
            a.push(Instruction48::Rts)?;
        }
        a.label(&handlers[BANDS.len()])?;
        a.float(R0, 2.0 * self.output_db)?;
        a.mul(R4, R12, R0)?;
        a.float(R0, self.output_db)?;
        a.sub(R4, R4, R0)?;
        a.call(&Leaf::DbToLinear.label())?;
        a.store(R0, GAIN)?;
        a.push(Instruction48::Rts)?;

        // reset: I4 = state, R8 = flag.
        a.label(&labels.reset)?;
        a.push(Instruction48::IntegerPass { dst: R8, src: R8 })?;
        a.jump(Condition::NotEqual, &cascade.settle)?;
        self.cascade.emit_clear_history(&mut a)?;
        a.jump(Condition::Always, &cascade.settle)?;

        self.emit_designers(&mut a, &designers)?;
        math::emit(a.0, &[Leaf::DbToLinear, Leaf::Tan, Leaf::Reciprocal])?;
        Ok(labels)
    }

    /// Designer ABI: F4 = v; band constants in F10, F11, F13 ([`Law::constants`]).
    /// Returns b0, b1, b2, a1, a2 in R3, R5, R6, R7, R9. Uses the scratch
    /// registers and R14; the caller saved the preserved ones.
    fn emit_designers(&self, a: &mut Asm, labels: &Designers) -> Result<(), BlockError> {
        // HPF (worst path: 7 to the OFF test, 2 + tan, 9 + reciprocal, 7, 5, RTS).
        a.label(&labels.hpf)?;
        a.float(R2, self.hpf_span)?;
        a.mul(R4, R4, R2)?;
        a.float(R2, self.hpf_minimum)?;
        a.add(R4, R4, R2)?;
        a.float(R2, self.off_below)?;
        compare(a, R4, R2)?;
        a.jump(Condition::LessThan, &labels.identity)?;
        a.float(R2, (PI / self.rate) as f32)?;
        a.mul(R4, R4, R2)?;
        a.call(&Leaf::Tan.label())?;
        // K in R3, K² in R5, p in R6, m in R7.
        a.mv(UniversalReg::Data(R3), UniversalReg::Data(R0))?;
        a.mul(R5, R0, R0)?;
        a.float(R2, 1.0)?;
        a.add(R6, R5, R2)?;
        a.sub(R7, R5, R2)?;
        a.add(R7, R7, R7)?;
        a.float(R2, (1.0 / self.q) as f32)?;
        a.mul(R1, R3, R2)?;
        a.add(R4, R6, R1)?;
        a.call(&Leaf::Reciprocal.label())?;
        a.mul(R7, R7, R0)?;
        a.mul(R9, R5, R0)?;
        a.add(R9, R9, R9)?;
        a.add(R9, R9, R9)?;
        a.float(R2, 1.0)?;
        a.add(R1, R2, R7)?;
        a.sub(R9, R9, R1)?;
        a.mv(UniversalReg::Data(R3), UniversalReg::Data(R0))?;
        a.mv(UniversalReg::Data(R6), UniversalReg::Data(R0))?;
        a.add(R5, R0, R0)?;
        a.int(R2, 0)?;
        a.sub(R5, R2, R5)?;
        a.push(Instruction48::Rts)?;

        // Identity: [1, 0, 0, 0, 0].
        a.label(&labels.identity)?;
        a.float(R3, 1.0)?;
        for register in [R5, R6, R7, R9] {
            a.int(register, 0)?;
        }
        a.push(Instruction48::Rts)?;

        // Shelf: F10 = sin w0/Q, F11 = cos w0, F13 = 4(1 − cos w0).
        a.label(&labels.shelf)?;
        self.emit_flat_test(a, &labels.identity)?;
        a.mv(UniversalReg::Data(R14), UniversalReg::Data(R4))?;
        self.emit_gain(a, R4, 1.0)?;
        a.mv(UniversalReg::Data(R3), UniversalReg::Data(R0))?;
        self.emit_gain(a, R14, 0.5)?;
        // A in R3, β in R14, A + 1 in R5, A − 1 in R6.
        a.mul(R14, R0, R10)?;
        a.float(R2, 1.0)?;
        a.add(R5, R3, R2)?;
        a.sub(R6, R3, R2)?;
        // p in R9, m in R7, n1 in R1, n2 in R12 (the reciprocal keeps R1, R12).
        a.mul(R7, R6, R11)?;
        a.add(R9, R5, R7)?;
        a.sub(R7, R5, R7)?;
        a.mul(R12, R5, R11)?;
        a.add(R1, R6, R12)?;
        a.sub(R12, R6, R12)?;
        a.add(R4, R9, R14)?;
        a.call(&Leaf::Reciprocal.label())?;
        // Ar in R2; b2 = A²·4(1 − cos w0)·r − (b0 + b1).
        a.mul(R2, R3, R0)?;
        a.mul(R6, R3, R2)?;
        a.mul(R6, R6, R13)?;
        a.add(R3, R7, R14)?;
        a.mul(R3, R3, R2)?;
        a.add(R12, R12, R12)?;
        a.mul(R5, R12, R2)?;
        a.add(R2, R3, R5)?;
        a.sub(R6, R6, R2)?;
        a.float(R4, -2.0)?;
        a.mul(R1, R1, R4)?;
        a.mul(R7, R1, R0)?;
        a.mul(R9, R13, R0)?;
        a.float(R2, 1.0)?;
        a.add(R1, R2, R7)?;
        a.sub(R9, R9, R1)?;
        a.push(Instruction48::Rts)?;

        // Peak: F10 = α, F11 = −2 cos w0, F13 = 2(1 − cos w0).
        a.label(&labels.peak)?;
        self.emit_flat_test(a, &labels.identity)?;
        self.emit_gain(a, R4, 1.0)?;
        a.mv(UniversalReg::Data(R14), UniversalReg::Data(R0))?;
        a.add(R4, R0, R10)?;
        a.call(&Leaf::Reciprocal.label())?;
        a.mul(R0, R0, R14)?;
        a.mul(R14, R14, R10)?;
        a.float(R2, 1.0)?;
        a.add(R3, R2, R14)?;
        a.mul(R3, R3, R0)?;
        a.mul(R7, R11, R0)?;
        a.mv(UniversalReg::Data(R5), UniversalReg::Data(R7))?;
        // Pole sum in R0; b2 = sum − (b0 + a1), a2 = sum − (1 + a1).
        a.mul(R0, R13, R0)?;
        a.add(R6, R3, R7)?;
        a.sub(R6, R0, R6)?;
        a.add(R1, R2, R7)?;
        a.sub(R9, R0, R1)?;
        a.push(Instruction48::Rts)
    }

    /// v = `flat` (0 dB) jumps to the identity designer. Three steps.
    fn emit_flat_test(&self, a: &mut Asm, identity: &Label) -> Result<(), BlockError> {
        a.float(R2, self.flat)?;
        compare(a, R4, R2)?;
        a.jump(Condition::Equal, identity)
    }

    /// F0 = db_to_linear(scale·(gain_db·v − gain_db/2)) from v in `value`:
    /// A for scale 1, √A for scale ½. Four steps and the leaf.
    fn emit_gain(&self, a: &mut Asm, value: DataReg, scale: f32) -> Result<(), BlockError> {
        a.float(R2, scale * self.gain_db)?;
        a.mul(R4, value, R2)?;
        a.float(R2, 0.5 * scale * self.gain_db)?;
        a.sub(R4, R4, R2)?;
        a.call(&Leaf::DbToLinear.label())
    }
}

struct Designers {
    hpf: Label,
    shelf: Label,
    peak: Label,
    identity: Label,
}

fn compare(a: &mut Asm, lhs: DataReg, rhs: DataReg) -> Result<(), BlockError> {
    a.push(Instruction48::FloatCompare {
        lhs: lhs.into(),
        rhs: rhs.into(),
    })
}

/// The owned Graphic-9 block.
#[derive(Debug, Clone, Copy, Default)]
pub struct Graphic9;

impl OwnedBlock for Graphic9 {
    fn model_id(&self) -> u32 {
        STOCK_MODEL_ID
    }
    fn parameters(&self) -> &'static [ParameterSpec] {
        PARAMETERS
    }
    /// Stock defaults: HPF OFF (v = 0), every band 0 dB, OUTPUT 0 dB.
    fn static_layout(&self) -> StaticLayout {
        let mut channel = CASCADE.initial_state();
        channel.resize(STATE_WORDS, 0);
        channel[VALUES + 1..DIRTY].fill(0.5_f32.to_bits());
        channel[GAIN] = 1.0_f32.to_bits();
        StaticLayout::Channels([channel.clone(), channel])
    }
    fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
        LAW.emit(program)
    }
    fn process_cycles(&self) -> u64 {
        PROCESS_CYCLES
    }
}

impl HostReference for Graphic9 {
    fn process(&self, state: &mut [u32], samples: &mut [f32; BLOCK_SAMPLES]) {
        LAW.process(state, samples);
    }
    fn set_parameter(&self, state: &mut [u32], index: u32, value: f32) {
        LAW.set_parameter(state, index, value);
    }
    fn reset(&self, state: &mut [u32]) {
        CASCADE.settle(state);
    }
    fn recover(&self, state: &mut [u32]) {
        CASCADE.clear_history(state);
        CASCADE.settle(state);
    }
}

#[cfg(test)]
mod tests {
    use tetrad_sharc::math::prm::Precision;

    use super::*;
    use crate::verification::{VerificationError, scheduled_frames, verify_module};

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
            Graphic9.static_layout()
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
            Graphic9.process(state, samples);
        }
        fn set_parameter(&self, state: &mut [u32], index: u32, value: f32) {
            Graphic9.set_parameter(state, index, value);
        }
        fn reset(&self, state: &mut [u32]) {
            Graphic9.reset(state);
        }
        fn recover(&self, state: &mut [u32]) {
            Graphic9.recover(state);
        }
    }

    /// One changed constant per law part fails V2 with a named mismatch: the
    /// design rate, Q, the instant glide, the OFF edge, the 0 dB identity and
    /// the RND32 bracket of the section loop.
    #[test]
    fn each_changed_law_constant_fails_v2() {
        let frames = scheduled_frames(
            64,
            &[
                (0, 0, 1.0 / 231.0),
                (0, 1, 0.8),
                (0, 2, 0.3),
                (0, 3, 0.5),
                (0, 9, 0.9),
            ],
        );
        let mutants = [
            Law {
                rate: 44_100.0,
                ..LAW
            },
            Law {
                q: std::f64::consts::FRAC_1_SQRT_2,
                ..LAW
            },
            Law {
                cascade: Cascade {
                    glide: Glide {
                        factor: 0.5,
                        snap: 1e-5,
                    },
                    ..CASCADE
                },
                ..LAW
            },
            Law {
                off_below: 21.0,
                ..LAW
            },
            Law { flat: 0.25, ..LAW },
            Law {
                cascade: Cascade {
                    precision: Precision::Single,
                    ..CASCADE
                },
                ..LAW
            },
        ];
        verify_module(&Graphic9, &frames).unwrap();
        for law in mutants {
            let error = verify_module(&Mutant(law), &frames).unwrap_err();
            assert!(
                matches!(error, VerificationError::Mismatch { .. }),
                "{law:?}: {error}"
            );
        }
        // The silence snap (I, 1e-20) acts only at the end of a long decay:
        // a +9.6 dB peak at 1 kHz rings for about 700 samples.
        let mut decay = scheduled_frames(64, &[(0, 5, 0.9)]);
        for frame in &mut decay[4..] {
            frame.samples = [[0.0; BLOCK_SAMPLES]; 2];
        }
        verify_module(&Graphic9, &decay).unwrap();
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
}

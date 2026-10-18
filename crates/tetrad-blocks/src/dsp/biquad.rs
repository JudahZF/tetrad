//! Biquad cascade leaf (Low-High Cut 4003, Graphic-9 4002): S
//! sections of `y = b0·x + b1·x1 + b2·x2 − a1·y1 − a2·y2`, run per sample in
//! place on one channel's 32 samples, with a per-block glide of the live
//! coefficients toward their targets.
//!
//! # RND32 bracket (C: stock cascades)
//!
//! The glide runs at the entry precision (RND32 = 1). The section loop runs
//! with RND32 = 0 (`BIT CLR MODE1 RND32; NOP` before it, `BIT SET MODE1
//! RND32; NOP` after it), as the stock cascades do: Low-High Cut kernel
//! cascade k1 `0x162c50`/`0x162ce3` (its glide stays RND32 = 1) and Graphic-9
//! `0x991`/`0x9bf` (OUTPUT runs after the set). Inside the loop the registers
//! keep 32 significand bits: the accumulator and the y1/y2 feedback within a
//! block. Each store (sample, history) keeps the upper 32 bits of the
//! register, so the next section and the next block read 24-bit values. The
//! leaf ABI allows this one MODE1 write (`crate::OwnedBlock`).
//!
//! # Section form: direct form I (owned choice)
//!
//! - The history words are signal values. The glide changes the coefficients
//!   every block; DF2/DF2T states are partial sums of the old coefficients, so
//!   a change meets inconsistent state and gives a transient. DF1 does not.
//! - No node exceeds the signal range: one accumulator per output sample.
//! - The same 5 multiplies and 4 adds as DF2T (no fused operation).
//!   Both forms add one rounding per term that the poles then amplify; neither
//!   removes the f32 coefficient error of poles near z = 1. The designer
//!   keeps 1 + a1 + a2 accurate instead (`blocks::lohicut`).
//! - Cascaded sections share history: the output pair of section k is the
//!   input pair of section k + 1, so S sections keep 2S + 2 history words.
//!
//! Sum order per sample (the twin does the same): ((((b2·x2 + b0·x) + b1·x1)
//! − a2·y2) − a1·y1). An identity section `[1, 0, 0, 0, 0]` passes x, except
//! that −0 becomes +0 and a subnormal becomes zero (both precisions).
//!
//! # State (per channel; the stock adapter calls the leaf for A, then B)
//!
//! The cascade starts at word 0 of the channel state. Record k (12 words):
//! `[x1, x2, b0, b1, b2, a1, a2, b0', b1', b2', a1', a2']` (history, live,
//! target). Two words follow record S − 1: the last output pair `[y1, y2]`.
//! Section k reads its output history from the next record's `[x1, x2]` before
//! section k + 1 overwrites it. Both channels keep their own coefficients,
//! because a leaf call sees only its own half.
//!
//! # Glide (I)
//!
//! Each block, before filtering: `live += factor·(target − live)`; when
//! |target − live| < `snap`, live = target exactly. Stock Low-High Cut moves
//! each live coefficient once per block toward its target and snaps below 1e-5
//! (k1 `0x162c17..0x162c4a`); its step factor is a stack argument, not
//! decoded. Each step is a convex combination of stable (a1, a2) pairs, and the
//! stability triangle is convex, so a glide between stable designs stays stable.
//! Graphic-9 uses [`Glide::INSTANT`]: stock swaps its coefficients in one
//! step (laws §3), so live = target before every block.
//!
//! # Numerics
//!
//! Inputs must be finite with |x| well below 2^125: then no operation is NaN,
//! ∞ − ∞ or 0·∞, so the kernel's NaN guard never fires.
//!
//! Silence (I): the PRM flushes results below 2^-126 to zero. Near that
//! edge a2·y2 flushes while a1·y1 (about twice as large) does not, so
//! y ≈ 2·y1: a decay ends in a limit cycle near 2^-117 instead of zero
//! (measured on the twin). So at each block end a history pair whose larger
//! magnitude is below `silence` becomes +0. With silent input the next block
//! is then exactly zero.
use std::num::NonZeroU16;

use tetrad_sharc::emit::{BranchRelocation, IsaProgram, Label, LoopCount};
use tetrad_sharc::isa48::{BranchKind, Condition, DmAccess, Instruction48, MODE1_RND32, Mode1Mask};
use tetrad_sharc::math::prm::{self, Float40, Precision};
use tetrad_sharc::register::{Dag1Index, Dag1Modify, Dag2Index, DataReg, UniversalReg};

use crate::{BLOCK_SAMPLES, BlockError};

/// Coefficients `[b0, b1, b2, a1, a2]` of one section.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Section(pub [f32; 5]);

impl Section {
    pub const IDENTITY: Self = Self([1.0, 0.0, 0.0, 0.0, 0.0]);

    /// |H(e^jω)| in f64 at `frequency` Hz for sample rate `rate`.
    pub fn magnitude(&self, frequency: f64, rate: f64) -> f64 {
        let w = std::f64::consts::TAU * frequency / rate;
        let [b0, b1, b2, a1, a2] = self.0.map(f64::from);
        let modulus = |c0: f64, c1: f64, c2: f64| {
            let re = c0 + c1 * w.cos() + c2 * (2.0 * w).cos();
            let im = c1 * w.sin() + c2 * (2.0 * w).sin();
            re.hypot(im)
        };
        modulus(b0, b1, b2) / modulus(1.0, a1, a2)
    }
}

/// Per-block coefficient glide (module docs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glide {
    pub factor: f32,
    pub snap: f32,
}

impl Glide {
    /// Live = target every block: every finite difference is below the snap.
    pub const INSTANT: Self = Self {
        factor: 1.0,
        snap: f32::MAX,
    };
}

/// Words per section record.
const RECORD: usize = 12;
const LIVE: usize = 2;
const TARGET: usize = 7;
const PAIRS: NonZeroU16 = match NonZeroU16::new(BLOCK_SAMPLES as u16 / 2) {
    Some(pairs) => pairs,
    None => panic!("a block holds sample pairs"),
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cascade {
    pub sections: NonZeroU16,
    pub glide: Glide,
    /// History pairs below this magnitude become zero at block end.
    pub silence: f32,
    /// Section-loop precision. [`Precision::Extended`] emits the stock RND32
    /// bracket (module docs); [`Precision::Single`] keeps the entry mode
    /// (V5 and the round-off comparison only).
    pub precision: Precision,
}

/// CALL or JUMP targets of the two routines that [`Cascade::emit`] defines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CascadeLabels {
    /// Leaf: R4 = I4 = channel state, R8 = 32 samples. Glides, filters, RTS.
    /// Preserves every CCES preserved register; RND32 is clear only inside
    /// the section loop.
    pub process: Label,
    /// I4 = channel state: live = target, history kept, RTS. Uses R0 and
    /// moves I4. Graphic-9 reset jumps here; Low-High Cut process calls it
    /// after a reset (review N1). Fault recovery first clears the history
    /// (`Cascade::emit_clear_history`).
    pub settle: Label,
}

impl Cascade {
    /// A cascade with the stock RND32 bracket.
    pub const fn new(sections: u16, glide: Glide, silence: f32) -> Self {
        match NonZeroU16::new(sections) {
            Some(sections) => Self {
                sections,
                glide,
                silence,
                precision: Precision::Extended,
            },
            None => panic!("a cascade has sections"),
        }
    }

    pub const fn words(&self) -> usize {
        RECORD * self.sections.get() as usize + 2
    }

    /// Identity sections, zero history.
    pub fn initial_state(&self) -> Vec<u32> {
        let mut state = vec![0; self.words()];
        for k in 0..self.sections.get() as usize {
            Self::set_target(&mut state, k, Section::IDENTITY);
        }
        self.settle(&mut state);
        state
    }

    /// The x1, x2 words of every record and the last output pair.
    fn history_words(&self) -> impl Iterator<Item = usize> {
        (0..=self.sections.get() as usize).flat_map(|k| [RECORD * k, RECORD * k + 1])
    }

    /// Word offset of the section's target b0; b1, b2, a1, a2 follow.
    pub const fn target_word(section: usize) -> usize {
        RECORD * section + TARGET
    }

    pub fn set_target(state: &mut [u32], section: usize, coefficients: Section) {
        let at = Self::target_word(section);
        for (word, value) in state[at..at + 5].iter_mut().zip(coefficients.0) {
            *word = value.to_bits();
        }
    }

    pub fn live(state: &[u32], section: usize) -> Section {
        let at = RECORD * section + LIVE;
        Section(std::array::from_fn(|j| f32::from_bits(state[at + j])))
    }

    pub fn target(state: &[u32], section: usize) -> Section {
        let at = Self::target_word(section);
        Section(std::array::from_fn(|j| f32::from_bits(state[at + j])))
    }

    /// Twin of [`CascadeLabels::settle`]: live = target, history kept.
    pub fn settle(&self, state: &mut [u32]) {
        for k in 0..self.sections.get() as usize {
            let record = RECORD * k;
            state.copy_within(record + TARGET..record + TARGET + 5, record + LIVE);
        }
    }

    /// Twin of the inline history clear that fault recovery emits: every
    /// x1, x2 and the last output pair become +0.
    pub fn clear_history(&self, state: &mut [u32]) {
        for word in self.history_words() {
            state[word] = 0;
        }
    }

    /// Inline: zero every history word of the cascade at I4 (fault
    /// recovery). Uses R1; I4 is unchanged.
    pub(crate) fn emit_clear_history(&self, a: &mut Asm) -> Result<(), BlockError> {
        a.int(R1, 0)?;
        for word in self.history_words() {
            a.store(R1, word)?;
        }
        Ok(())
    }

    /// Twin of [`CascadeLabels::process`].
    pub fn process(&self, state: &mut [u32], samples: &mut [f32; BLOCK_SAMPLES]) {
        let word = |state: &[u32], at: usize| f32::from_bits(state[at]);
        let sections = self.sections.get() as usize;
        for k in 0..sections {
            for j in 0..5 {
                let (live, target) = (RECORD * k + LIVE + j, RECORD * k + TARGET + j);
                let difference = prm::sub(word(state, target), word(state, live));
                state[live] = if difference.abs() < self.glide.snap {
                    state[target]
                } else {
                    let step = prm::mul(difference, self.glide.factor);
                    prm::add(word(state, live), step).to_bits()
                };
            }
        }
        let p = self.precision;
        let load = |state: &[u32], at: usize| Float40::from(word(state, at));
        let mut output = [0.0; 2];
        for k in 0..sections {
            let record = RECORD * k;
            let [b0, b1, b2, a1, a2]: [Float40; 5] =
                std::array::from_fn(|j| load(state, record + LIVE + j));
            let (mut x1, mut x2) = (load(state, record), load(state, record + 1));
            let (mut y1, mut y2) = (
                load(state, record + RECORD),
                load(state, record + RECORD + 1),
            );
            for sample in samples.iter_mut() {
                let x = Float40::from(*sample);
                let mut sum = p.mul(b2, x2);
                sum = p.add(sum, p.mul(b0, x));
                sum = p.add(sum, p.mul(b1, x1));
                sum = p.sub(sum, p.mul(a2, y2));
                let y = p.sub(sum, p.mul(a1, y1));
                *sample = y.store();
                (x1, x2, y1, y2) = (x, x1, y, y1);
            }
            // x1 and x2 are 32-bit loads: their silence test (inside the
            // bracket) reads the same values as the stored words.
            [state[record], state[record + 1]] = self.silence([x1.store(), x2.store()]);
            output = [y1.store(), y2.store()];
        }
        // After `BIT SET MODE1 RND32` the test reads y1, y2 without their
        // eight LSBs: the values the stores keep.
        let last = RECORD * sections;
        [state[last], state[last + 1]] = self.silence(output);
    }

    /// History words of a pair, zero below [`Cascade::silence`].
    fn silence(&self, pair: [f32; 2]) -> [u32; 2] {
        let [a, b] = pair.map(|x| prm::operand(x).abs());
        if a.max(b) < self.silence {
            [0; 2]
        } else {
            pair.map(f32::to_bits)
        }
    }

    /// Modeled steps of [`CascadeLabels::process`] when no coefficient snaps
    /// and every history pair is silenced (the longest path).
    pub const fn process_cycles(&self) -> u64 {
        let saved = SAVED.len() as u64;
        let sections = self.sections.get() as u64;
        // 5 × (2 loads, sub, abs, compare, jump, multiply, add, store), then
        // the record step.
        let glide = 5 * 9 + 1;
        // 2 input history loads, 5 coefficients, 2 output history loads, 2
        // moves, DO, 22 per pair, silence, move, 2 stores.
        let section = 2 + 5 + 2 + 2 + 1 + 22 * PAIRS.get() as u64 + SILENCE + 1 + 2;
        let bracket = match self.precision {
            Precision::Single => 0,
            Precision::Extended => 4,
        };
        // Save, M4, constants and DO; I4 and DO; silence and store the final
        // pair; restore, RTS.
        saved
            + 4
            + sections * glide
            + 2
            + sections * section
            + bracket
            + SILENCE
            + 2
            + restore_cycles(SAVED.len())
            + 1
    }

    /// Modeled steps of [`CascadeLabels::settle`]: DO; per section 5 loads,
    /// 5 stores and the record step; RTS.
    pub const fn settle_cycles(&self) -> u64 {
        1 + 11 * self.sections.get() as u64 + 1
    }

    /// Emit both routines once per program.
    pub fn emit(&self, program: &mut IsaProgram) -> Result<CascadeLabels, BlockError> {
        let labels = CascadeLabels {
            process: Label::new("biquad.process"),
            settle: Label::new("biquad.settle"),
        };
        let mut a = Asm(program);
        let (glide_end, section_end, sample_end) = (
            Label::new("biquad.glide_end"),
            Label::new("biquad.section_end"),
            Label::new("biquad.pair_end"),
        );
        let record = RECORD as i32;
        let extended = self.precision == Precision::Extended;

        a.label(&labels.process)?;
        save(&mut a, &SAVED)?;
        a.push(Instruction48::Immediate {
            dst: UniversalReg::Modify(M4),
            value: RECORD as u32,
        })?;
        // Glide at RND32 = 1: R1 = live, R0 = target, R2 = difference. The
        // last load only moves I4 to the next record.
        a.float(R11, self.glide.snap)?;
        a.float(R12, self.glide.factor)?;
        a.0.counted_loop(LoopCount::Immediate(self.sections), glide_end.clone())?;
        for j in 0..5 {
            let stored = Label::new(format!("biquad.glide_{j}"));
            a.load(R1, LIVE + j)?;
            a.load(R0, TARGET + j)?;
            a.sub(R2, R0, R1)?;
            a.push(Instruction48::FloatAbs {
                dst: R10.into(),
                src: R2.into(),
            })?;
            a.push(Instruction48::FloatCompare {
                lhs: R10.into(),
                rhs: R11.into(),
            })?;
            a.jump(Condition::LessThan, &stored)?;
            a.mul(R2, R2, R12)?;
            a.add(R0, R1, R2)?;
            a.label(&stored)?;
            a.store(R0, LIVE + j)?;
        }
        a.label(&glide_end)?;
        a.push(Instruction48::DmLoad {
            dst: R2,
            index: I4,
            modifier: M4,
            access: DmAccess::PostModify,
        })?;

        // Sections, in the RND32 bracket. x1 = R10, x2 = R11; y1 = R12, y2 =
        // R0 from the next record. R4 keeps the next record, R8 the samples.
        a.mv(UniversalReg::Index(I4), UniversalReg::Data(R4))?;
        if extended {
            a.push(Instruction48::Mode1Clear(RND32))?;
            a.push(Instruction48::Nop)?;
        }
        a.0.counted_loop(LoopCount::Immediate(self.sections), section_end.clone())?;
        a.push(Instruction48::DmLoad {
            dst: R10,
            index: I4,
            modifier: M4,
            access: DmAccess::PostModify,
        })?;
        a.load_at(R11, 1 - record)?;
        for (j, coefficient) in COEFFICIENTS.into_iter().enumerate() {
            a.load_at(coefficient, (LIVE + j) as i32 - record)?;
        }
        a.load_at(R12, 0)?;
        a.load_at(R0, 1)?;
        a.mv(UniversalReg::Data(R4), UniversalReg::Index(I4))?;
        a.mv(UniversalReg::Index(I4), UniversalReg::Data(R8))?;
        a.0.counted_loop(LoopCount::Immediate(PAIRS), sample_end.clone())?;
        // Two samples per pass: the history registers swap roles, so no moves.
        emit_sample(&mut a, [R10, R11, R12, R0], None)?;
        emit_sample(&mut a, [R11, R10, R0, R12], Some(&sample_end))?;
        self.emit_silence(&mut a, [R10, R11], &Label::new("biquad.input_kept"))?;
        a.mv(UniversalReg::Index(I4), UniversalReg::Data(R4))?;
        a.store_at(R10, -record)?;
        a.label(&section_end)?;
        a.store_at(R11, 1 - record)?;
        if extended {
            a.push(Instruction48::Mode1Set(RND32))?;
            a.push(Instruction48::Nop)?;
        }
        self.emit_silence(&mut a, [R12, R0], &Label::new("biquad.output_kept"))?;
        a.store_at(R12, 0)?;
        a.store_at(R0, 1)?;
        restore(&mut a, &SAVED)?;
        a.push(Instruction48::Rts)?;

        let settle_end = Label::new("biquad.settle_end");
        a.label(&labels.settle)?;
        a.0.counted_loop(LoopCount::Immediate(self.sections), settle_end.clone())?;
        for j in 0..5 {
            a.load(R0, TARGET + j)?;
            a.store(R0, LIVE + j)?;
        }
        a.label(&settle_end)?;
        a.push(Instruction48::ModifyWords {
            index: I4,
            words: record,
        })?;
        a.push(Instruction48::Rts)?;
        Ok(labels)
    }
}

impl Cascade {
    /// Zero the pair's registers when both magnitudes are below `silence`.
    /// Uses R1, R2. `kept` labels the next instruction.
    fn emit_silence(
        &self,
        a: &mut Asm,
        pair: [DataReg; 2],
        kept: &Label,
    ) -> Result<(), BlockError> {
        for (dst, src) in [(R1, pair[0]), (R2, pair[1])] {
            a.push(Instruction48::FloatAbs {
                dst: dst.into(),
                src: src.into(),
            })?;
        }
        a.push(Instruction48::FloatMax {
            dst: R1.into(),
            lhs: R1.into(),
            rhs: R2.into(),
        })?;
        a.float(R2, self.silence)?;
        a.push(Instruction48::FloatCompare {
            lhs: R1.into(),
            rhs: R2.into(),
        })?;
        a.jump(Condition::GreaterOrEqual, kept)?;
        for register in pair {
            a.int(register, 0)?;
        }
        a.label(kept)
    }
}

/// Worst path of [`Cascade::emit_silence`]: abs, abs, max, immediate,
/// compare, branch, two zeros.
const SILENCE: u64 = 8;

/// One output sample from `[x1, x2, y1, y2]` (twin order): the new input
/// replaces x2 and the output replaces y2, then the output is stored.
fn emit_sample(
    a: &mut Asm,
    [x1, x2, y1, y2]: [DataReg; 4],
    last: Option<&Label>,
) -> Result<(), BlockError> {
    let [b0, b1, b2, a1, a2] = COEFFICIENTS;
    a.mul(R1, b2, x2)?;
    a.load_at(x2, 0)?;
    a.mul(R2, b0, x2)?;
    a.add(R1, R1, R2)?;
    a.mul(R2, b1, x1)?;
    a.add(R1, R1, R2)?;
    a.mul(R2, a2, y2)?;
    a.sub(R1, R1, R2)?;
    a.mul(R2, a1, y1)?;
    a.sub(y2, R1, R2)?;
    if let Some(last) = last {
        a.label(last)?;
    }
    a.push(Instruction48::DmStore {
        src: y2,
        index: I4,
        modifier: M6,
        access: DmAccess::PostModify,
    })
}

pub(crate) const R0: DataReg = DataReg::of(0);
pub(crate) const R1: DataReg = DataReg::of(1);
pub(crate) const R2: DataReg = DataReg::of(2);
pub(crate) const R3: DataReg = DataReg::of(3);
pub(crate) const R4: DataReg = DataReg::of(4);
pub(crate) const R5: DataReg = DataReg::of(5);
pub(crate) const R6: DataReg = DataReg::of(6);
pub(crate) const R7: DataReg = DataReg::of(7);
pub(crate) const R8: DataReg = DataReg::of(8);
pub(crate) const R9: DataReg = DataReg::of(9);
pub(crate) const R10: DataReg = DataReg::of(10);
pub(crate) const R11: DataReg = DataReg::of(11);
pub(crate) const R12: DataReg = DataReg::of(12);
pub(crate) const R13: DataReg = DataReg::of(13);
pub(crate) const R14: DataReg = DataReg::of(14);
/// The channel state (leaf ABI); the loops reuse it for records and samples.
pub(crate) const I4: Dag1Index = Dag1Index::of(4);
/// The C stack pointer: push `DM(I7, M7)`, pop `MODIFY(I7, n)(NW)`.
const I7: Dag1Index = Dag1Index::of(7);
pub(crate) const M4: Dag1Modify = Dag1Modify::of(4);
/// CCES scratch DAG2 registers: blocks park pointers here across leaf calls.
pub(crate) const I12: Dag2Index = Dag2Index::of(12);
pub(crate) const I13: Dag2Index = Dag2Index::of(13);
/// C run-time constants 1 and −1: one normal word, scaled in byte space.
pub(crate) const M6: Dag1Modify = Dag1Modify::of(6);
const M7: Dag1Modify = Dag1Modify::of(7);

/// The MODE1 bit of the stock bracket.
const RND32: Mode1Mask = match Mode1Mask::new(MODE1_RND32) {
    Ok(mask) => mask,
    Err(_) => panic!("RND32 is a verified MODE1 bit"),
};

/// Live b0, b1, b2, a1, a2 while a section runs.
const COEFFICIENTS: [DataReg; 5] = [R3, R5, R6, R7, R9];
/// Preserved registers the process routine uses.
const SAVED: [DataReg; 7] = [R3, R5, R6, R7, R9, R10, R11];
/// Modeled steps of [`restore`]: one offset load per register and one pop.
pub(crate) const fn restore_cycles(registers: usize) -> u64 {
    registers as u64 + 1
}

/// Push `registers` on the C stack.
pub(crate) fn save(a: &mut Asm, registers: &[DataReg]) -> Result<(), BlockError> {
    for &src in registers {
        a.push(Instruction48::DmStore {
            src,
            index: I7,
            modifier: M7,
            access: DmAccess::PostModify,
        })?;
    }
    Ok(())
}

/// Reload what [`save`] pushed, then pop all of it with one
/// `I7 = MODIFY(I7, n)(NW)`, the stock pop (gainblock `0x322`, noisegate
/// `0x2b0`). Every value is read while it is still above I7: interrupts share
/// the stack.
pub(crate) fn restore(a: &mut Asm, registers: &[DataReg]) -> Result<(), BlockError> {
    for (slot, &dst) in registers.iter().enumerate() {
        a.push(Instruction48::DmOffsetLoad {
            dst,
            index: I7,
            offset: (registers.len() - slot) as i32,
        })?;
    }
    a.push(Instruction48::ModifyWords {
        index: I7,
        words: registers.len() as i32,
    })
}

/// Instruction builder over data registers (floats as `Fn` of the same index)
/// and I4-relative state words.
pub(crate) struct Asm<'a>(pub(crate) &'a mut IsaProgram);

impl Asm<'_> {
    pub(crate) fn push(&mut self, instruction: Instruction48) -> Result<(), BlockError> {
        self.0.push(instruction)?;
        Ok(())
    }

    pub(crate) fn label(&mut self, label: &Label) -> Result<(), BlockError> {
        Ok(self.0.define(label.clone())?)
    }

    pub(crate) fn float(&mut self, dst: DataReg, value: f32) -> Result<(), BlockError> {
        self.int(dst, value.to_bits())
    }

    pub(crate) fn int(&mut self, dst: DataReg, value: u32) -> Result<(), BlockError> {
        self.push(Instruction48::Immediate {
            dst: UniversalReg::Data(dst),
            value,
        })
    }

    pub(crate) fn mv(&mut self, dst: UniversalReg, src: UniversalReg) -> Result<(), BlockError> {
        self.push(Instruction48::RegisterMove { dst, src })
    }

    pub(crate) fn mul(
        &mut self,
        dst: DataReg,
        lhs: DataReg,
        rhs: DataReg,
    ) -> Result<(), BlockError> {
        self.push(Instruction48::FloatMultiply {
            dst: dst.into(),
            lhs: lhs.into(),
            rhs: rhs.into(),
        })
    }

    pub(crate) fn add(
        &mut self,
        dst: DataReg,
        lhs: DataReg,
        rhs: DataReg,
    ) -> Result<(), BlockError> {
        self.push(Instruction48::FloatAdd {
            dst: dst.into(),
            lhs: lhs.into(),
            rhs: rhs.into(),
        })
    }

    pub(crate) fn sub(
        &mut self,
        dst: DataReg,
        lhs: DataReg,
        rhs: DataReg,
    ) -> Result<(), BlockError> {
        self.push(Instruction48::FloatSubtract {
            dst: dst.into(),
            lhs: lhs.into(),
            rhs: rhs.into(),
        })
    }

    /// `dst = DM(word, I4)`.
    pub(crate) fn load(&mut self, dst: DataReg, word: usize) -> Result<(), BlockError> {
        self.load_at(dst, word as i32)
    }

    pub(crate) fn store(&mut self, src: DataReg, word: usize) -> Result<(), BlockError> {
        self.store_at(src, word as i32)
    }

    pub(crate) fn load_at(&mut self, dst: DataReg, offset: i32) -> Result<(), BlockError> {
        self.push(Instruction48::DmOffsetLoad {
            dst,
            index: I4,
            offset,
        })
    }

    pub(crate) fn store_at(&mut self, src: DataReg, offset: i32) -> Result<(), BlockError> {
        self.push(Instruction48::DmOffsetStore {
            src,
            index: I4,
            offset,
        })
    }

    pub(crate) fn jump(&mut self, condition: Condition, target: &Label) -> Result<(), BlockError> {
        self.0.branch_to(
            BranchKind::Jump,
            condition,
            target.clone(),
            BranchRelocation::PcRel24,
        )?;
        Ok(())
    }

    pub(crate) fn call(&mut self, target: &Label) -> Result<(), BlockError> {
        self.0.branch_to(
            BranchKind::Call,
            Condition::Always,
            target.clone(),
            BranchRelocation::PcRel24,
        )?;
        Ok(())
    }
}

//! Owned offline fixed32 compiler for one recovered V1 tone section (process `0x10`).
//! One leaf serves First, Second and Third: the code is coefficient-independent and
//! reads five caller-validated coefficient words `[b0,b1,b2,a1,a2]` from DM. It does
//! NOT compute coefficients; callers pass `V1ToneCoefficients` (for example from
//! `v1_tone_coefficients_reference`), so no resident tan/sqrt/pow/division is emitted.
//!
//! Four checked disjoint logical DM spans: input32, output32, coefficient5 and
//! history4 `[x1,x2,y1,y2]`. They are private logical words, not the stock
//! B+0x390.. struct-of-arrays layout, a physical DMA buffer or an in-place buffer.
//! Each sample uses the recovered separate-f32 order (no fused multiply-add):
//! p0=b0*x; p1=x1*b1; p2=x2*b2; s=p1+p0; q1=y1*a1; s=p2+s; q2=y2*a2; s=s-q1; y=s-q2.
//! Histories rotate through register rings (no moves); x3/y3 are not kept.
//! The 32 unrolled samples occupy 366 ISA48 words (2196 bytes): 9 loads, 32*11
//! sample words, 4 history stores and RTS. Every word executes once; 36 stores.
//! Direct DM addressing only, so DAG I/M/B/L registers are never read or written.
//! R0..R13 and ALU zero are clobbered. Active banks, RND32 with cleared
//! unsupported modes and a valid call context are caller premises.
//! Host f32 (including subnormals) stands in for SHARC arithmetic: no 40-bit,
//! flush-to-zero or rounding-mode parity. Staging allocates; OOM, realtime, stock
//! ABI and device acceptance are unproved.
#![forbid(unsafe_code)]

use super::V1ToneCoefficients;
use std::fmt;
use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::isa48::{MODE1_RND32, MODE1_SCALAR_CLEAR};
use tetrad_sharc::register::{DataReg, FloatReg};
use tetrad_sharc::simulator::{
    DmMemory, ScalarState, SimulationError, SimulationOutcome, run_scalar,
};
use tetrad_sharc::{EmitError, Instruction48, IsaProgram, Label, OwnedCode};

pub const SAMPLES: u32 = 32;
const SPAN_LENGTHS: [u32; 4] = [SAMPLES, SAMPLES, 5, 4];
/// Input, output, coefficient and history offsets from `ToneLogicalPlacement::dm_base`.
const ADJACENT: [u32; 4] = [0, 32, 64, 69];
const WORDS: usize = 73;

/// Coefficient slots are ordered `[b0,b1,b2,a1,a2]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Address(AddressError),
    Emit(EmitError),
    Simulation(SimulationError),
    Overlap { first: usize, second: usize },
    NonFiniteCoefficient { slot: usize },
    NonFiniteMemory { address: DmWordAddress32 },
    Modes(u32),
    Publication,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

/// Logical normal-word addresses. Spans are pairwise disjoint and every
/// one-past endpoint fits u32. No physical alias map is implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    starts: [DmWordAddress32; 4],
}

impl Layout {
    pub fn new(
        input: DmWordAddress32,
        output: DmWordAddress32,
        coefficients: DmWordAddress32,
        history: DmWordAddress32,
    ) -> Result<Self, Error> {
        let starts = [input, output, coefficients, history];
        let mut ends = starts;
        for ((end, start), length) in ends.iter_mut().zip(starts).zip(SPAN_LENGTHS) {
            *end = start.checked_add(length).map_err(Error::Address)?;
        }
        for first in 0..starts.len() {
            for second in first + 1..starts.len() {
                if starts[first] < ends[second] && starts[second] < ends[first] {
                    return Err(Error::Overlap { first, second });
                }
            }
        }
        Ok(Self { starts })
    }

    /// Order: input32, output32, coefficient5, history4.
    pub fn spans(self) -> [(DmWordAddress32, u32); 4] {
        std::array::from_fn(|index| (self.starts[index], SPAN_LENGTHS[index]))
    }
}

/// Immutable code, layout and expected store sequence. Coefficients, history
/// and inputs belong to the caller's DM and are reread on every invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledTone {
    code: OwnedCode,
    layout: Layout,
    publications: Vec<DmWordAddress32>,
}

impl CompiledTone {
    /// Registers: R4..R8 = b0,b1,b2,a1,a2; R0..R2 = x ring; R9..R11 = y ring;
    /// R12, R13, R3 = temporaries. Sample n loads x into X[n%3], finds x1 in
    /// X[(n+2)%3] and x2 in X[(n+1)%3], and writes y into Y[n%3] the same way.
    pub fn compile(origin: PmWordAddress24, layout: Layout) -> Result<Self, Error> {
        let r: [DataReg; 14] =
            std::array::from_fn(|index| DataReg::new(index as u8).expect("fixed R0..R13 set"));
        let f = r.map(FloatReg::from);
        let [b0, b1, b2, a1, a2] = [4, 5, 6, 7, 8];
        let x = [0, 1, 2];
        let y = [9, 10, 11];
        let [t0, t1, t2] = [12, 13, 3];
        let [input, output, coefficients, history] = layout.starts;
        let at =
            |start: DmWordAddress32, offset: u32| start.checked_add(offset).map_err(Error::Address);
        let ring = |n: u32| (n % 3) as usize;

        let mut instructions = Vec::with_capacity(366);
        for (slot, register) in [b0, b1, b2, a1, a2].into_iter().enumerate() {
            instructions.push(Instruction48::DmDirectLoad {
                dst: r[register],
                address: at(coefficients, slot as u32)?,
            });
        }
        // Sample 0 expects x1 in X[2], x2 in X[1], y1 in Y[2], y2 in Y[1].
        for (slot, register) in [x[2], x[1], y[2], y[1]].into_iter().enumerate() {
            instructions.push(Instruction48::DmDirectLoad {
                dst: r[register],
                address: at(history, slot as u32)?,
            });
        }
        let multiply = |dst: usize, lhs: usize, rhs: usize| Instruction48::FloatMultiply {
            dst: f[dst],
            lhs: f[lhs],
            rhs: f[rhs],
        };
        for n in 0..SAMPLES {
            let (now, first, second) = (ring(n), ring(n + 2), ring(n + 1));
            instructions.extend([
                Instruction48::DmDirectLoad {
                    dst: r[x[now]],
                    address: at(input, n)?,
                },
                multiply(t0, b0, x[now]),
                multiply(t1, x[first], b1),
                multiply(t2, x[second], b2),
                Instruction48::FloatAdd {
                    dst: f[t0],
                    lhs: f[t1],
                    rhs: f[t0],
                },
                multiply(t1, y[first], a1),
                Instruction48::FloatAdd {
                    dst: f[t0],
                    lhs: f[t2],
                    rhs: f[t0],
                },
                multiply(t2, y[second], a2),
                Instruction48::FloatSubtract {
                    dst: f[t0],
                    lhs: f[t0],
                    rhs: f[t1],
                },
                Instruction48::FloatSubtract {
                    dst: f[y[now]],
                    lhs: f[t0],
                    rhs: f[t2],
                },
                Instruction48::DmDirectStore {
                    src: r[y[now]],
                    address: at(output, n)?,
                },
            ]);
        }
        let (last, previous) = (ring(SAMPLES - 1), ring(SAMPLES - 2));
        for (slot, register) in [x[last], x[previous], y[last], y[previous]]
            .into_iter()
            .enumerate()
        {
            instructions.push(Instruction48::DmDirectStore {
                src: r[register],
                address: at(history, slot as u32)?,
            });
        }
        instructions.push(Instruction48::Rts);

        let mut program = IsaProgram::new(origin);
        program
            .define(Label::new("v1_tone.entry"))
            .map_err(Error::Emit)?;
        let mut publications = Vec::with_capacity(36);
        for instruction in instructions {
            if let Instruction48::DmDirectStore { address, .. } = instruction {
                publications.push(address);
            }
            program.push(instruction).map_err(Error::Emit)?;
        }
        Ok(Self {
            code: program.finish().map_err(Error::Emit)?,
            layout,
            publications,
        })
    }

    pub fn code(&self) -> &OwnedCode {
        &self.code
    }
    pub fn layout(&self) -> Layout {
        self.layout
    }
    /// Store addresses in emitted order: output0..31, then x1, x2, y1, y2.
    pub fn publications(&self) -> &[DmWordAddress32] {
        &self.publications
    }

    /// Straight-line leaf: every emitted word executes exactly once, including RTS.
    pub fn max_steps(&self) -> usize {
        self.code.word_count()
    }

    /// Checks modes, spans and finite coefficient/input/history words, then runs the
    /// raw words on staged clones of the caller's DM, write log and registers.
    /// Commits only if the exact store sequence, unchanged DAG/MODE1 registers and
    /// finite published words hold. Allocation failure is not returned.
    pub fn simulate_atomic(
        &self,
        state: &mut ScalarState,
        memory: &mut DmMemory,
        step_budget: usize,
    ) -> Result<SimulationOutcome, Error> {
        if state.mode1 & MODE1_RND32 == 0 || state.mode1 & MODE1_SCALAR_CLEAR != 0 {
            return Err(Error::Modes(state.mode1));
        }
        for (start, length) in self.layout.spans() {
            memory
                .validate_range(start, length)
                .map_err(Error::Simulation)?;
        }
        let [input, _, coefficients, history] = self.layout.starts;
        for (start, length) in [(coefficients, 5), (input, SAMPLES), (history, 4)] {
            for offset in 0..length {
                let address = start.checked_add(offset).map_err(Error::Address)?;
                if !f32::from_bits(memory.read(address).map_err(Error::Simulation)?).is_finite() {
                    return Err(if start == coefficients {
                        Error::NonFiniteCoefficient {
                            slot: offset as usize,
                        }
                    } else {
                        Error::NonFiniteMemory { address }
                    });
                }
            }
        }
        let mut staged_memory = memory.clone();
        let mut staged_state = state.clone();
        let prior_writes = staged_memory.writes().len();
        let outcome = run_scalar(
            &self.code,
            &mut staged_state,
            &mut staged_memory,
            step_budget,
        )
        .map_err(Error::Simulation)?;
        let pointers = |s: &ScalarState| (s.indices, s.modifiers, s.bases, s.lengths, s.mode1);
        if outcome.steps != self.max_steps()
            || outcome.stores != self.publications.len()
            || staged_memory.writes()[prior_writes..] != self.publications[..]
            || pointers(&staged_state) != pointers(state)
        {
            return Err(Error::Publication);
        }
        // Each address is stored once. With finite coefficients, input and prior
        // history, any nonfinite product or sum propagates through the add/sub
        // chain into that sample's y (inf+-x, inf-inf and NaN are all nonfinite),
        // and every y is a unique output word. So finite outputs prove every
        // intermediate finite, which is exactly the reference's failure domain.
        for &address in &self.publications {
            if !f32::from_bits(staged_memory.read(address).map_err(Error::Simulation)?).is_finite()
            {
                return Err(Error::NonFiniteMemory { address });
            }
        }
        *memory = staged_memory;
        *state = staged_state;
        Ok(outcome)
    }
}

/// Caller-selected logical units, not a physical PM/DM image or device mapping.
/// Input32, output32, coefficient5 and history4 (73 words) follow `dm_base`
/// adjacently; the one-past end must fit u32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToneLogicalPlacement {
    pub origin: PmWordAddress24,
    pub dm_base: DmWordAddress32,
}

/// Decoded DM outputs and actual decoder counts. Instructions are not hardware cycles.
#[derive(Debug, Clone, PartialEq)]
pub struct EmittedToneBlock {
    pub outputs: [f32; 32],
    pub receipt: SimulationOutcome,
}

/// One owned emitted tone section. Owns the coefficient and history words and
/// one immutable compiled leaf; nothing mutable is exposed. Usable for any of
/// the three V1 tones because the law lives only in the caller's coefficients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmittedV1ToneReference {
    coefficients: [u32; 5],
    history: [u32; 4],
    compiled: CompiledTone,
}

impl EmittedV1ToneReference {
    /// Histories start at zero, as after reset `0x492`.
    pub fn new(
        coefficients: V1ToneCoefficients,
        placement: ToneLogicalPlacement,
    ) -> Result<Self, Error> {
        let coefficients = coefficient_words(coefficients)?;
        let [input, output, coefficient, history] = ADJACENT.map(|offset| {
            placement
                .dm_base
                .checked_add(offset)
                .map_err(Error::Address)
        });
        let layout = Layout::new(input?, output?, coefficient?, history?)?;
        Ok(Self {
            coefficients,
            history: [0; 4],
            compiled: CompiledTone::compile(placement.origin, layout)?,
        })
    }

    /// Immutable image and layout for auditing.
    pub fn compiled(&self) -> &CompiledTone {
        &self.compiled
    }

    pub fn coefficients(&self) -> V1ToneCoefficients {
        let [b0, b1, b2, a1, a2] = self.coefficients.map(f32::from_bits);
        V1ToneCoefficients {
            feedforward: [b0, b1, b2],
            feedback: [a1, a2],
        }
    }

    /// `[x1, x2, y1, y2]`, newest first, as `V1ToneReference::history`.
    pub fn history(&self) -> [f32; 4] {
        self.history.map(f32::from_bits)
    }

    /// Setter semantics: replaces the five control words only; history stays and
    /// the code is not recompiled. A nonfinite word refuses the whole update.
    pub fn update_coefficients(&mut self, coefficients: V1ToneCoefficients) -> Result<(), Error> {
        self.coefficients = coefficient_words(coefficients)?;
        Ok(())
    }

    /// Reset `0x492`: zero history; coefficients and code stay.
    pub fn reset(&mut self) {
        self.history = [0; 4];
    }

    /// Stages the 73 adjacent DM words and a fresh default ScalarState, then runs
    /// the atomic leaf. Outputs and history decode from simulated DM and commit
    /// only after the whole block succeeds; every error retains the instance.
    pub fn process_block(
        &mut self,
        inputs: &[f32; 32],
        instruction_budget: usize,
    ) -> Result<EmittedToneBlock, Error> {
        let (base, _) = self.compiled.layout().spans()[0];
        let mut words = Vec::with_capacity(WORDS);
        words.extend(inputs.map(f32::to_bits));
        words.extend([0; 32]);
        words.extend(self.coefficients);
        words.extend(self.history);
        let mut memory = DmMemory::new(base, words).map_err(Error::Simulation)?;
        let mut scalar = ScalarState::default();
        let receipt =
            self.compiled
                .simulate_atomic(&mut scalar, &mut memory, instruction_budget)?;
        let words = memory.words();
        let outputs = std::array::from_fn(|index| f32::from_bits(words[32 + index]));
        self.history = std::array::from_fn(|index| words[69 + index]);
        Ok(EmittedToneBlock { outputs, receipt })
    }
}

fn coefficient_words(coefficients: V1ToneCoefficients) -> Result<[u32; 5], Error> {
    let [b0, b1, b2] = coefficients.feedforward;
    let [a1, a2] = coefficients.feedback;
    let values = [b0, b1, b2, a1, a2];
    match values.iter().position(|value| !value.is_finite()) {
        Some(slot) => Err(Error::NonFiniteCoefficient { slot }),
        None => Ok(values.map(f32::to_bits)),
    }
}

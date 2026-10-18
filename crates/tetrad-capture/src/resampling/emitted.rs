//! Owned offline ISA48 compilers for the fixed V1 32->64 interpolator and 64->32
//! decimator. They reproduce `V1ResamplingReference` (owned host policy), not a
//! recovered stock instruction order, stock table or physical image.
//!
//! The 16 caller-owned table words are immediates in the immutable code, so no DM
//! word holds coefficients and no coefficient span can be rewritten by a caller.
//! Each kernel is fully unrolled straight-line code with direct DM addressing only:
//! it never reads or writes DAG I/M/B/L registers or MODE1.
//!
//! | Kernel | DM spans (words) | Words = steps | Stores | Clobbers |
//! | --- | --- | --- | --- | --- |
//! | Interpolate | input32, output64, history15 | 3232 | 64 + 15 | R0..R8, ALU zero |
//! | Decimate | input64, output32, history31 | 3200 | 32 + 31 | R0..R6, ALU zero |
//!
//! History spans are newest first, like the reference arrays. Every output word
//! is stored once, then every history word once, in ascending offset order.
//! Execution uses `run_scalar` host f32: finite subnormals are allowed, as in the
//! reference. No 40-bit, flush-to-zero, rounding-mode or timing claim. Staging
//! allocates; OOM, realtime, stock ABI and device acceptance are unproved.
#![forbid(unsafe_code)]

use super::CallerOwnedV1ResamplingTable;
use crate::filters::HistoryUpdate;
use std::fmt;
use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::isa48::{MODE1_RND32, MODE1_SCALAR_CLEAR};
use tetrad_sharc::register::{DataReg, FloatReg, UniversalReg};
use tetrad_sharc::simulator::{
    DmMemory, ScalarState, SimulationError, SimulationOutcome, run_scalar,
};
use tetrad_sharc::{EmitError, Instruction48, IsaProgram, Label, OwnedCode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Address(AddressError),
    Emit(EmitError),
    Simulation(SimulationError),
    /// Span indices in `Layout::spans` order.
    Overlap {
        first: usize,
        second: usize,
    },
    NonFiniteMemory {
        address: DmWordAddress32,
    },
    Modes(u32),
    Publication,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kernel {
    Interpolate,
    Decimate,
}

impl Kernel {
    /// Input, output and history lengths in words.
    pub const fn span_lengths(self) -> [u32; 3] {
        match self {
            Self::Interpolate => [32, 64, 15],
            Self::Decimate => [64, 32, 31],
        }
    }

    /// Data registers R0..R(n-1) written by the kernel. All others are preserved.
    pub const fn clobbered_registers(self) -> usize {
        match self {
            Self::Interpolate => 9,
            Self::Decimate => 7,
        }
    }
}

/// Logical normal-word addresses. Spans are pairwise disjoint; a span may end at
/// word u32::MAX (one-past endpoints are u64). No physical alias map is implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    kernel: Kernel,
    starts: [DmWordAddress32; 3],
}

impl Layout {
    pub fn new(
        kernel: Kernel,
        input: DmWordAddress32,
        output: DmWordAddress32,
        history: DmWordAddress32,
    ) -> Result<Self, Error> {
        let starts = [input, output, history];
        let ends: [u64; 3] = std::array::from_fn(|index| {
            u64::from(starts[index].get()) + u64::from(kernel.span_lengths()[index])
        });
        if ends.iter().any(|&end| end > 1 << 32) {
            return Err(Error::Address(AddressError::DmOverflow));
        }
        for first in 0..3 {
            for second in first + 1..3 {
                if u64::from(starts[first].get()) < ends[second]
                    && u64::from(starts[second].get()) < ends[first]
                {
                    return Err(Error::Overlap { first, second });
                }
            }
        }
        Ok(Self { kernel, starts })
    }

    pub fn kernel(self) -> Kernel {
        self.kernel
    }

    /// Order: input, output, history.
    pub fn spans(self) -> [(DmWordAddress32, u32); 3] {
        std::array::from_fn(|index| (self.starts[index], self.kernel.span_lengths()[index]))
    }
}

/// Immutable code, layout, table bits and expected store sequence. Inputs and
/// history belong to the caller's DM and are reread on every invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledResampling {
    code: OwnedCode,
    layout: Layout,
    coefficient_bits: [u32; 16],
    publications: Vec<DmWordAddress32>,
}

impl CompiledResampling {
    pub fn compile(
        origin: PmWordAddress24,
        layout: Layout,
        table: &CallerOwnedV1ResamplingTable,
    ) -> Result<Self, Error> {
        let coefficient_bits = table.coefficients().map(f32::to_bits);
        let emitter = Emitter {
            layout,
            instructions: Vec::new(),
        };
        let (instructions, label) = match layout.kernel {
            Kernel::Interpolate => (
                emitter.interpolate(&coefficient_bits)?,
                "v1_interpolate.entry",
            ),
            Kernel::Decimate => (emitter.decimate(&coefficient_bits)?, "v1_decimate.entry"),
        };
        let mut program = IsaProgram::new(origin);
        program.define(Label::new(label)).map_err(Error::Emit)?;
        let mut publications = Vec::new();
        for instruction in instructions {
            if let Instruction48::DmDirectStore { address, .. } = instruction {
                publications.push(address);
            }
            program.push(instruction).map_err(Error::Emit)?;
        }
        Ok(Self {
            code: program.finish().map_err(Error::Emit)?,
            layout,
            coefficient_bits,
            publications,
        })
    }

    pub fn code(&self) -> &OwnedCode {
        &self.code
    }
    pub fn layout(&self) -> Layout {
        self.layout
    }
    /// The table bits encoded as immediates, in table order.
    pub fn coefficient_bits(&self) -> [u32; 16] {
        self.coefficient_bits
    }
    /// Store addresses in emitted order: every output word, then every history word.
    pub fn publications(&self) -> &[DmWordAddress32] {
        &self.publications
    }

    /// Straight-line leaf: every emitted word executes exactly once, including RTS.
    /// This is also the minimum successful step budget.
    pub fn max_steps(&self) -> usize {
        self.code.word_count()
    }

    /// Checks modes, spans and finite input/history words, then runs the raw words
    /// on staged clones of the caller's DM, write log and registers. Commits only
    /// if steps, the exact store sequence, unclobbered registers, DAG/MODE1 and
    /// finite published words all hold. Allocation failure is not returned.
    pub fn simulate_atomic(
        &self,
        state: &mut ScalarState,
        memory: &mut DmMemory,
        step_budget: usize,
    ) -> Result<SimulationOutcome, Error> {
        if state.mode1 & MODE1_RND32 == 0 || state.mode1 & MODE1_SCALAR_CLEAR != 0 {
            return Err(Error::Modes(state.mode1));
        }
        let spans = self.layout.spans();
        for (start, length) in spans {
            memory
                .validate_range(start, length)
                .map_err(Error::Simulation)?;
        }
        for (start, length) in [spans[0], spans[2]] {
            for offset in 0..length {
                finite_word(memory, start.checked_add(offset).map_err(Error::Address)?)?;
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
        let kept = self.layout.kernel.clobbered_registers();
        let preserved = |s: &ScalarState| {
            (
                s.data[kept..].to_vec(),
                s.indices,
                s.modifiers,
                s.bases,
                s.lengths,
                s.mode1,
            )
        };
        if outcome.steps != self.max_steps()
            || outcome.stores != self.publications.len()
            || staged_memory.writes()[prior_writes..] != self.publications[..]
            || preserved(&staged_state) != preserved(state)
        {
            return Err(Error::Publication);
        }
        // Each address is stored once. Inputs/history are finite, so a product is
        // finite or +-inf, never NaN by itself. Interpolate: product -> pair ->
        // running phase sum -> doubling -> output. Decimate: pair -> product (0*inf
        // is NaN) -> lane -> lane sum -> output. Every intermediate reaches exactly
        // one output through additions, and any nonfinite addend gives a nonfinite
        // sum. So finite outputs prove every intermediate finite, which is the
        // reference's failure domain. History words are input copies.
        for &address in &self.publications {
            finite_word(&staged_memory, address)?;
        }
        *memory = staged_memory;
        *state = staged_state;
        Ok(outcome)
    }
}

fn finite_word(memory: &DmMemory, address: DmWordAddress32) -> Result<f32, Error> {
    let value = f32::from_bits(memory.read(address).map_err(Error::Simulation)?);
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::NonFiniteMemory { address })
    }
}

fn reg(index: usize) -> DataReg {
    DataReg::new(index as u8).expect("fixed R0..R8 set")
}

fn fr(index: usize) -> FloatReg {
    FloatReg::from(reg(index))
}

struct Emitter {
    layout: Layout,
    instructions: Vec<Instruction48>,
}

impl Emitter {
    /// Address of the sample `delay` words before input `now`, crossing into the
    /// newest-first history for negative time.
    fn sample(&self, now: u32, delay: u32) -> Result<DmWordAddress32, Error> {
        let [(input, _), _, (history, _)] = self.layout.spans();
        match now.checked_sub(delay) {
            Some(index) => input.checked_add(index),
            None => history.checked_add(delay - now - 1),
        }
        .map_err(Error::Address)
    }

    fn at(&self, span: usize, offset: u32) -> Result<DmWordAddress32, Error> {
        self.layout.spans()[span]
            .0
            .checked_add(offset)
            .map_err(Error::Address)
    }

    fn load(&mut self, dst: usize, address: DmWordAddress32) {
        self.instructions.push(Instruction48::DmDirectLoad {
            dst: reg(dst),
            address,
        });
    }

    fn store(&mut self, src: usize, address: DmWordAddress32) {
        self.instructions.push(Instruction48::DmDirectStore {
            src: reg(src),
            address,
        });
    }

    fn immediate(&mut self, dst: usize, value: u32) {
        self.instructions.push(Instruction48::Immediate {
            dst: UniversalReg::Data(reg(dst)),
            value,
        });
    }

    fn multiply(&mut self, dst: usize, lhs: usize, rhs: usize) {
        self.instructions.push(Instruction48::FloatMultiply {
            dst: fr(dst),
            lhs: fr(lhs),
            rhs: fr(rhs),
        });
    }

    fn add(&mut self, dst: usize, lhs: usize, rhs: usize) {
        self.instructions.push(Instruction48::FloatAdd {
            dst: fr(dst),
            lhs: fr(lhs),
            rhs: fr(rhs),
        });
    }

    /// Retained history k is input `len-1-k`; copies go through `scratch`.
    fn finish(mut self, scratch: usize) -> Result<Vec<Instruction48>, Error> {
        let [(_, inputs), _, (_, history)] = self.layout.spans();
        for k in 0..history {
            let source = self.at(0, inputs - 1 - k)?;
            self.load(scratch, source);
            let target = self.at(2, k)?;
            self.store(scratch, target);
        }
        self.instructions.push(Instruction48::Rts);
        Ok(self.instructions)
    }

    /// R0=+0.0 so every phase sum starts as `0.0 + pair`, keeping the reference's
    /// signed-zero result. Per output n and j ascending: R1/R2 = T[2j]/T[2j+1],
    /// R3=x[n-j], R4=x[n-15+j]; phase0 += T[2j]*recent + T[2j+1]*old (R7),
    /// phase1 += T[2j]*old + T[2j+1]*recent (R8); then each phase doubles by
    /// addition and stores at outputs 2n, 2n+1.
    fn interpolate(mut self, table: &[u32; 16]) -> Result<Vec<Instruction48>, Error> {
        let (zero, even, odd, recent, old, p0, p1, phase0, phase1) = (0, 1, 2, 3, 4, 5, 6, 7, 8);
        self.immediate(zero, 0);
        for n in 0..32 {
            for j in 0..8 {
                let recent_address = self.sample(n, j)?;
                let old_address = self.sample(n, 15 - j)?;
                self.load(recent, recent_address);
                self.load(old, old_address);
                self.immediate(even, table[2 * j as usize]);
                self.immediate(odd, table[2 * j as usize + 1]);
                for (phase, first, second) in [(phase0, recent, old), (phase1, old, recent)] {
                    self.multiply(p0, even, first);
                    self.multiply(p1, odd, second);
                    self.add(p0, p0, p1);
                    self.add(phase, if j == 0 { zero } else { phase }, p0);
                }
            }
            for (lane, phase) in [phase0, phase1].into_iter().enumerate() {
                self.add(phase, phase, phase);
                let address = self.at(1, 2 * n + lane as u32)?;
                self.store(phase, address);
            }
        }
        self.finish(recent)
    }

    /// R0=+0.0 starts both lanes. Per output n and j ascending: R2=u[2n-j],
    /// R3=u[2n-31+j], R4=R2+R3, R1=T[j], R4=R1*R4, lane[j%2] (R5/R6) += R4;
    /// then R5=R5+R6 stores at output n.
    fn decimate(mut self, table: &[u32; 16]) -> Result<Vec<Instruction48>, Error> {
        let (zero, coefficient, left, right, term, lanes) = (0, 1, 2, 3, 4, [5, 6]);
        self.immediate(zero, 0);
        for n in 0..32 {
            for j in 0..16 {
                let left_address = self.sample(2 * n, j)?;
                let right_address = self.sample(2 * n, 31 - j)?;
                self.load(left, left_address);
                self.load(right, right_address);
                self.add(term, left, right);
                self.immediate(coefficient, table[j as usize]);
                self.multiply(term, coefficient, term);
                let lane = lanes[j as usize % 2];
                self.add(lane, if j < 2 { zero } else { lane }, term);
            }
            self.add(lanes[0], lanes[0], lanes[1]);
            let address = self.at(1, n)?;
            self.store(lanes[0], address);
        }
        self.finish(left)
    }
}

/// Caller-selected logical units, not a physical PM/DM image or device mapping.
/// Input, output and history follow `dm_base` adjacently; the last word may be u32::MAX.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResamplingLogicalPlacement {
    pub origin: PmWordAddress24,
    pub dm_base: DmWordAddress32,
}

/// Decoded DM outputs and actual simulator counts. Instructions are not cycles.
#[derive(Debug, Clone, PartialEq)]
pub struct EmittedResamplingBlock<const OUT: usize> {
    pub outputs: [f32; OUT],
    pub receipt: SimulationOutcome,
}

/// One owned emitted resampling direction: the table (with provenance), the
/// newest-first history words and one immutable compiled kernel. Every call
/// stages a private DM map; separate instances may reuse logical addresses.
#[derive(Debug, Clone, PartialEq)]
pub struct EmittedResampling<const IN: usize, const OUT: usize, const HISTORY: usize> {
    table: CallerOwnedV1ResamplingTable,
    history: [u32; HISTORY],
    compiled: CompiledResampling,
}

/// 32 low-rate inputs to 64 high-rate outputs; 15 retained low-rate samples.
pub type EmittedV1Interpolator = EmittedResampling<32, 64, 15>;
/// 64 high-rate inputs to 32 low-rate outputs; 31 retained high-rate samples.
pub type EmittedV1Decimator = EmittedResampling<64, 32, 31>;

impl EmittedV1Interpolator {
    pub fn new(
        table: CallerOwnedV1ResamplingTable,
        placement: ResamplingLogicalPlacement,
    ) -> Result<Self, Error> {
        Self::build(Kernel::Interpolate, table, placement)
    }
}

impl EmittedV1Decimator {
    pub fn new(
        table: CallerOwnedV1ResamplingTable,
        placement: ResamplingLogicalPlacement,
    ) -> Result<Self, Error> {
        Self::build(Kernel::Decimate, table, placement)
    }
}

impl<const IN: usize, const OUT: usize, const HISTORY: usize> EmittedResampling<IN, OUT, HISTORY> {
    /// Histories start at zero, as `V1ResamplingReference::new`.
    fn build(
        kernel: Kernel,
        table: CallerOwnedV1ResamplingTable,
        placement: ResamplingLogicalPlacement,
    ) -> Result<Self, Error> {
        let base = placement.dm_base;
        let at = |offset: usize| base.checked_add(offset as u32).map_err(Error::Address);
        let layout = Layout::new(kernel, base, at(IN)?, at(IN + OUT)?)?;
        Ok(Self {
            compiled: CompiledResampling::compile(placement.origin, layout, &table)?,
            table,
            history: [0; HISTORY],
        })
    }

    pub fn table(&self) -> &CallerOwnedV1ResamplingTable {
        &self.table
    }

    /// Immutable image and layout for auditing.
    pub fn compiled(&self) -> &CompiledResampling {
        &self.compiled
    }

    /// Newest first, as the matching `V1ResamplingReference` history.
    pub fn history(&self) -> [f32; HISTORY] {
        self.history.map(f32::from_bits)
    }

    /// Zero history; table, code and placement stay.
    pub fn reset(&mut self) {
        self.history = [0; HISTORY];
    }

    /// Explicit table replacement recompiles at the same origin and layout. The
    /// history policy matches `V1ResamplingReference::reload`. A failed compile
    /// keeps the previous table, code and history.
    pub fn reload(
        &mut self,
        table: CallerOwnedV1ResamplingTable,
        policy: HistoryUpdate,
    ) -> Result<(), Error> {
        let origin = self.compiled.code.origin();
        self.compiled = CompiledResampling::compile(origin, self.compiled.layout, &table)?;
        self.table = table;
        if policy == HistoryUpdate::Reset {
            self.reset();
        }
        Ok(())
    }

    /// Stages the adjacent DM words and a fresh default ScalarState, then runs the
    /// atomic kernel. Outputs and history decode from simulated DM and commit only
    /// after the whole block succeeds; every error retains the whole instance.
    pub fn process_block(
        &mut self,
        inputs: &[f32; IN],
        instruction_budget: usize,
    ) -> Result<EmittedResamplingBlock<OUT>, Error> {
        let (base, _) = self.compiled.layout.spans()[0];
        let mut words = Vec::with_capacity(IN + OUT + HISTORY);
        words.extend(inputs.map(f32::to_bits));
        words.extend([0; OUT]);
        words.extend(self.history);
        let mut memory = DmMemory::new(base, words).map_err(Error::Simulation)?;
        let mut scalar = ScalarState::default();
        let receipt =
            self.compiled
                .simulate_atomic(&mut scalar, &mut memory, instruction_budget)?;
        let words = memory.words();
        let outputs = std::array::from_fn(|index| f32::from_bits(words[IN + index]));
        self.history = std::array::from_fn(|index| words[IN + OUT + index]);
        Ok(EmittedResamplingBlock { outputs, receipt })
    }
}

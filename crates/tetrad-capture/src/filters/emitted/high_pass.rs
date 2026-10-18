//! Owned offline high-pass compiler over five finite V1 coefficient slots.
//! The immutable code/layout/exact bits bind four checked disjoint DM spans:
//! input32, output32, coefficients5 and accumulated state2. PM uses checked PM24.
//! Separate host-f32 operations follow p0=b0*x; y=p0+s1; p1=b1*x;
//! t1=p1+s2; q1=a1*y; next_s1=t1-q1; p2=b2*x; q2=a2*y; next_s2=p2-q2.
//! The 32 unrolled samples occupy 424 ISA48 words (2544 bytes), execute 424
//! instructions including RTS, and store 96 words. Raw execution uses direct DM,
//! no DAG. R0..R10 and arithmetic flags are clobbered; hardware active banks,
//! RND32/cleared unsupported modes and a valid call context are caller premises.
//! The wrapper stages all DM words, the address-only write log and ScalarState.
//! All finite outputs AND both finite final states imply finite intermediates
//! only under ordinary host-f32 arithmetic. The log contains no historic values.
//! This gives no first-fault-PC, hardware rounding or hardware transaction claim.
//! Staging allocates; OOM, realtime, stock ABI and device acceptance are unproved.
#![forbid(unsafe_code)]

use crate::filters::V1HighPassCoefficients;
use std::fmt;
use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::isa48::{MODE1_RND32, MODE1_SCALAR_CLEAR};
use tetrad_sharc::register::{DataReg, FloatReg};
use tetrad_sharc::simulator::{
    DmMemory, ScalarState, SimulationError, SimulationOutcome, run_scalar,
};
use tetrad_sharc::{EmitError, Instruction48, IsaProgram, OwnedCode};

pub const SAMPLES: u32 = 32;
const SPAN_LENGTHS: [u32; 4] = [SAMPLES, SAMPLES, 5, 2];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Address(AddressError),
    Emit(EmitError),
    Simulation(SimulationError),
    Overlap { first: usize, second: usize },
    NonFiniteCoefficient { slot: usize },
    CoefficientBits { slot: usize },
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

/// Logical normal-word addresses. All four one-past endpoints fit u32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    starts: [DmWordAddress32; 4],
}

impl Layout {
    pub fn new(input: u32, output: u32, coefficients: u32, state: u32) -> Result<Self, Error> {
        let starts = [input, output, coefficients, state].map(DmWordAddress32::new);
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

    /// Order: input32, output32, coefficient5, accumulated-state2.
    pub fn spans(self) -> [(DmWordAddress32, u32); 4] {
        std::array::from_fn(|index| (self.starts[index], SPAN_LENGTHS[index]))
    }
}

/// Code, placement and expected coefficient bits are private and immutable.
/// State and inputs belong to the caller and are reread on every invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledHighPass {
    code: OwnedCode,
    layout: Layout,
    coefficient_bits: [u32; 5],
}

impl CompiledHighPass {
    /// Arbitrary finite coefficients are an explicit reference policy, not provenance.
    pub fn compile(
        origin: PmWordAddress24,
        layout: Layout,
        coefficients: V1HighPassCoefficients,
    ) -> Result<Self, Error> {
        let [b0, b1, b2] = coefficients.feedforward;
        let [a1, a2] = coefficients.feedback;
        let values = [b0, b1, b2, a1, a2];
        for (slot, value) in values.iter().enumerate() {
            if !value.is_finite() {
                return Err(Error::NonFiniteCoefficient { slot });
            }
        }
        let [input, output, coefficient, state] = layout.starts;
        let r: [DataReg; 11] = std::array::from_fn(|index| {
            DataReg::new(index as u8).expect("fixed R0..R10 register set")
        });
        let f = r.map(FloatReg::from);
        let mut program = IsaProgram::new(origin);
        for slot in 0..5 {
            program
                .push(Instruction48::DmDirectLoad {
                    dst: r[slot as usize + 4],
                    address: coefficient.checked_add(slot).map_err(Error::Address)?,
                })
                .map_err(Error::Emit)?;
        }
        for slot in 0..2 {
            program
                .push(Instruction48::DmDirectLoad {
                    dst: r[slot as usize + 1],
                    address: state.checked_add(slot).map_err(Error::Address)?,
                })
                .map_err(Error::Emit)?;
        }
        for sample in 0..SAMPLES {
            // Exact recovered order: p0,y,p1,t1,q1,s1',p2,q2,s2'.
            let instructions = [
                Instruction48::DmDirectLoad {
                    dst: r[0],
                    address: input.checked_add(sample).map_err(Error::Address)?,
                },
                Instruction48::FloatMultiply {
                    dst: f[9],
                    lhs: f[4],
                    rhs: f[0],
                },
                Instruction48::FloatAdd {
                    dst: f[3],
                    lhs: f[9],
                    rhs: f[1],
                },
                Instruction48::FloatMultiply {
                    dst: f[9],
                    lhs: f[5],
                    rhs: f[0],
                },
                Instruction48::FloatAdd {
                    dst: f[9],
                    lhs: f[9],
                    rhs: f[2],
                },
                Instruction48::FloatMultiply {
                    dst: f[10],
                    lhs: f[7],
                    rhs: f[3],
                },
                Instruction48::FloatSubtract {
                    dst: f[1],
                    lhs: f[9],
                    rhs: f[10],
                },
                Instruction48::FloatMultiply {
                    dst: f[9],
                    lhs: f[6],
                    rhs: f[0],
                },
                Instruction48::FloatMultiply {
                    dst: f[10],
                    lhs: f[8],
                    rhs: f[3],
                },
                Instruction48::FloatSubtract {
                    dst: f[2],
                    lhs: f[9],
                    rhs: f[10],
                },
                Instruction48::DmDirectStore {
                    src: r[3],
                    address: output.checked_add(sample).map_err(Error::Address)?,
                },
                Instruction48::DmDirectStore {
                    src: r[1],
                    address: state,
                },
                Instruction48::DmDirectStore {
                    src: r[2],
                    address: state.checked_add(1).map_err(Error::Address)?,
                },
            ];
            for instruction in instructions {
                program.push(instruction).map_err(Error::Emit)?;
            }
        }
        program.push(Instruction48::Rts).map_err(Error::Emit)?;
        Ok(Self {
            code: program.finish().map_err(Error::Emit)?,
            layout,
            coefficient_bits: values.map(f32::to_bits),
        })
    }

    pub fn code(&self) -> &OwnedCode {
        &self.code
    }
    pub fn layout(&self) -> Layout {
        self.layout
    }
    pub fn coefficient_bits(&self) -> [u32; 5] {
        self.coefficient_bits
    }

    /// Straight-line leaf: every emitted word executes exactly once, including RTS.
    pub fn max_steps(&self) -> usize {
        self.code.word_count()
    }

    /// Stages the entire offline DM allocation, prior address log and registers.
    /// Allocation failure is not returned; this is not a realtime/RAM guarantee.
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
        let [input, output, coefficient, history] = self.layout.starts;
        for (slot, expected) in self.coefficient_bits.iter().enumerate() {
            let address = coefficient
                .checked_add(slot as u32)
                .map_err(Error::Address)?;
            if memory.read(address).map_err(Error::Simulation)? != *expected {
                return Err(Error::CoefficientBits { slot });
            }
        }
        for (start, length) in [(input, SAMPLES), (history, 2)] {
            for offset in 0..length {
                let address = start.checked_add(offset).map_err(Error::Address)?;
                if !f32::from_bits(memory.read(address).map_err(Error::Simulation)?).is_finite() {
                    return Err(Error::NonFiniteMemory { address });
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
        if outcome.steps != self.max_steps() || outcome.stores != SAMPLES as usize * 3 {
            return Err(Error::Publication);
        }
        let writes = &staged_memory.writes()[prior_writes..];
        for (sample, group) in writes.chunks_exact(3).enumerate() {
            let expected = [
                output.checked_add(sample as u32).map_err(Error::Address)?,
                history,
                history.checked_add(1).map_err(Error::Address)?,
            ];
            if group != expected {
                return Err(Error::Publication);
            }
            for &address in group {
                if !f32::from_bits(staged_memory.read(address).map_err(Error::Simulation)?)
                    .is_finite()
                {
                    return Err(Error::NonFiniteMemory { address });
                }
            }
        }
        // The log stores addresses, not historic values. With finite inputs/coefficients,
        // the first nonfinite operation makes y, s1' or s2' nonfinite. Nonfinite s1
        // makes y and the next s1 nonfinite (even 0*y is NaN). Nonfinite s2 makes
        // the next s1 nonfinite. Thus every failure reaches a unique output store
        // or the final state; repeated state writes cannot hide it. See the module contract and independent numeric proof.
        *memory = staged_memory;
        *state = staged_state;
        Ok(outcome)
    }
}

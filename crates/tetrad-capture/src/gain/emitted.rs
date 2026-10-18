//! Owned offline fixed32 gain smoother compiler; host f32, not stock parity.
#![forbid(unsafe_code)]

use crate::reference::GAIN_SMOOTHING;
use std::fmt;
use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::isa48::{MODE1_RND32, MODE1_SCALAR_CLEAR};
use tetrad_sharc::register::{DataReg, FloatReg, UniversalReg};
use tetrad_sharc::simulator::{
    DmMemory, ScalarState, SimulationError, SimulationOutcome, run_scalar,
};
use tetrad_sharc::{EmitError, Instruction48, IsaProgram, Label, OwnedCode};

pub const SAMPLES: u32 = 32;
const SPAN_LENGTHS: [u32; 4] = [SAMPLES, SAMPLES, 1, 1];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Address(AddressError),
    Emit(EmitError),
    Simulation(SimulationError),
    Overlap { first: usize, second: usize },
    Modes(u32),
    NonFiniteMemory { address: DmWordAddress32 },
    NegativeGain { address: DmWordAddress32 },
    Publication,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

/// Logical normal-word addresses. Spans are pairwise disjoint (input/output too)
/// and every one-past endpoint fits u32. No physical alias map is implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    starts: [DmWordAddress32; 4],
}

impl Layout {
    pub fn new(
        input: DmWordAddress32,
        output: DmWordAddress32,
        current: DmWordAddress32,
        target: DmWordAddress32,
    ) -> Result<Self, Error> {
        let starts = [input, output, current, target];
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

    /// Order: input32, output32, current1, target1.
    pub fn spans(self) -> [(DmWordAddress32, u32); 4] {
        std::array::from_fn(|index| (self.starts[index], SPAN_LENGTHS[index]))
    }
}

/// Code, placement and expected store sequence are private and immutable.
/// The smoothing coefficient is an immediate in the code, not caller data.
/// Inputs, current and target belong to the caller and are reread on every call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledGainSmoother {
    code: OwnedCode,
    layout: Layout,
    publications: Vec<DmWordAddress32>,
}

impl CompiledGainSmoother {
    /// R1=c and R2=1.0 by immediate; R3=current and R4=target load once. Each
    /// sample: F5=1-c, F6=c*current, F7=F5*target, F3=F6+F7, then the NEW F3
    /// multiplies the loaded input in F0, which is stored. The final F3 is stored
    /// to current once. Target is never written. Clobbers R0..R7 and ALU flags.
    pub fn compile(origin: PmWordAddress24, layout: Layout) -> Result<Self, Error> {
        let r: [DataReg; 8] =
            std::array::from_fn(|index| DataReg::new(index as u8).expect("fixed R0..R7 set"));
        let f = r.map(FloatReg::from);
        let [input, output, current, target] = layout.starts;
        let mut instructions = vec![
            Instruction48::Immediate {
                dst: UniversalReg::Data(r[1]),
                value: GAIN_SMOOTHING.to_bits(),
            },
            Instruction48::Immediate {
                dst: UniversalReg::Data(r[2]),
                value: 1.0f32.to_bits(),
            },
            Instruction48::DmDirectLoad {
                dst: r[3],
                address: current,
            },
            Instruction48::DmDirectLoad {
                dst: r[4],
                address: target,
            },
        ];
        for sample in 0..SAMPLES {
            // Recovered order; every step is a separate f32 operation.
            instructions.extend([
                Instruction48::FloatSubtract {
                    dst: f[5],
                    lhs: f[2],
                    rhs: f[1],
                },
                Instruction48::FloatMultiply {
                    dst: f[6],
                    lhs: f[1],
                    rhs: f[3],
                },
                Instruction48::FloatMultiply {
                    dst: f[7],
                    lhs: f[5],
                    rhs: f[4],
                },
                Instruction48::FloatAdd {
                    dst: f[3],
                    lhs: f[6],
                    rhs: f[7],
                },
                Instruction48::DmDirectLoad {
                    dst: r[0],
                    address: input.checked_add(sample).map_err(Error::Address)?,
                },
                Instruction48::FloatMultiply {
                    dst: f[0],
                    lhs: f[3],
                    rhs: f[0],
                },
                Instruction48::DmDirectStore {
                    src: r[0],
                    address: output.checked_add(sample).map_err(Error::Address)?,
                },
            ]);
        }
        instructions.extend([
            Instruction48::DmDirectStore {
                src: r[3],
                address: current,
            },
            Instruction48::Rts,
        ]);
        let mut program = IsaProgram::new(origin);
        program
            .define(Label::new("gain_smoothing.entry"))
            .map_err(Error::Emit)?;
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
            publications,
        })
    }

    pub fn code(&self) -> &OwnedCode {
        &self.code
    }
    pub fn layout(&self) -> Layout {
        self.layout
    }
    /// Store addresses in emitted order, taken from the emitted instructions.
    pub fn publications(&self) -> &[DmWordAddress32] {
        &self.publications
    }

    /// Straight-line leaf: every emitted word executes exactly once, including RTS.
    pub fn max_steps(&self) -> usize {
        self.code.word_count()
    }

    /// Atomic offline staging of the entire DM allocation, prior log and registers.
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
        let [input, _, current, target] = self.layout.starts;
        for offset in 0..SAMPLES {
            let address = input.checked_add(offset).map_err(Error::Address)?;
            finite_word(memory, address)?;
        }
        for address in [current, target] {
            // Matches the reference: -0.0 is accepted, any value below zero is not.
            if finite_word(memory, address)? < 0.0 {
                return Err(Error::NegativeGain { address });
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
        if outcome.steps != self.max_steps()
            || outcome.stores != self.publications.len()
            || staged_memory.writes()[prior_writes..] != self.publications[..]
        {
            return Err(Error::Publication);
        }
        // Every output address is unique and current is stored once, so each current
        // word is the value the code wrote. c is finite in (0.5, 1), so 1-c is exact
        // and finite, and finite target keeps (1-c)*target finite. A nonfinite current
        // makes c*current nonfinite and any sum with a nonfinite operand is nonfinite,
        // so it reaches the final current. Hence a finite final current proves every
        // per-sample product, sum and current stage was finite.
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

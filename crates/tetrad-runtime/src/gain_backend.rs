//! Owned emitted gain words executed by a host mathematical raw-word simulator.
//! Logical addresses are caller supplied. This is not a physical SHARC backend.
use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::emit::{EmitError, OwnedCode};
use tetrad_sharc::gain::{MAX_GAIN_SAMPLES, compile_gain};
use tetrad_sharc::simulator::{
    DmMemory, GainInvocation, SampleCount, ScalarState, SimulationError, SimulationOutcome,
    UnmodeledSemantics, simulate_gain,
};

use crate::resident::{BLOCK_FRAMES, Error, MonoBlock};

const _: () = assert!(BLOCK_FRAMES as u32 == MAX_GAIN_SAMPLES);

/// The current nine-word loop takes 3 entry + 5 * 32 body + 1 RTS steps.
/// This is a decoder instruction count, not SHARC cycles or elapsed ticks.
pub const GAIN_EXECUTION_STEPS: usize = 3 + 5 * BLOCK_FRAMES + 1;

/// Raw decoder instruction limit, independent of the resident's injected clock.
/// Requests above GAIN_EXECUTION_STEPS are capped at that fixed leaf bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstructionBudget(usize);

impl InstructionBudget {
    pub const fn new(steps: usize) -> Self {
        Self(if steps > GAIN_EXECUTION_STEPS {
            GAIN_EXECUTION_STEPS
        } else {
            steps
        })
    }

    pub const fn get(self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildError {
    #[error(transparent)]
    Address(#[from] AddressError),
    #[error(transparent)]
    Emit(#[from] EmitError),
    #[error("gain scratch buffers must be identical or adjacent 32-word ranges")]
    BufferLayout,
}

/// At most 64 DM words: either exact in-place operation or adjacent buffers.
/// Ranges require representable one-past indices; partial overlap/gaps fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GainLayout {
    input: DmWordAddress32,
    output: DmWordAddress32,
}

impl GainLayout {
    pub fn new(input: DmWordAddress32, output: DmWordAddress32) -> Result<Self, BuildError> {
        let input_end = input.checked_add(MAX_GAIN_SAMPLES)?;
        let output_end = output.checked_add(MAX_GAIN_SAMPLES)?;
        if input != output && input_end != output && output_end != input {
            return Err(BuildError::BufferLayout);
        }
        Ok(Self { input, output })
    }

    pub const fn input(self) -> DmWordAddress32 {
        self.input
    }

    pub const fn output(self) -> DmWordAddress32 {
        self.output
    }
}

/// Copyable fault causes keep the resident's existing Copy error/status API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RawExecutionError {
    #[error("raw instruction budget {0} exhausted before RTS")]
    StepBudget(usize),
    #[error("unsupported raw word {word:#014x} at PC {pc:#x}")]
    UnsupportedWord { pc: u32, word: u64 },
    #[error("unmodeled raw semantics {feature:?} at PC {pc:#x}")]
    Unmodeled {
        pc: u32,
        feature: UnmodeledSemantics,
    },
    #[error("generated gain execution violated its validated simulator contract")]
    ContractViolation,
}

impl From<SimulationError> for RawExecutionError {
    fn from(error: SimulationError) -> Self {
        match error {
            SimulationError::StepBudget(steps) => Self::StepBudget(steps),
            SimulationError::UnsupportedWord { pc, word } => Self::UnsupportedWord { pc, word },
            SimulationError::Unmodeled { pc, feature } => Self::Unmodeled { pc, feature },
            // Addresses, ranges, count, modes and lengths are established here.
            // Any other failure indicates an emitter/simulator contract defect.
            _ => Self::ContractViolation,
        }
    }
}

/// Generates owned gain code once. No callback or caller-supplied code can stand
/// in for raw execution. Only Resident can invoke it with its frozen block data.
/// Each invocation allocates fresh DM words and a write log, resets scalar state,
/// and discards scratch on error. No realtime or heap-free guarantee is made.
#[derive(Debug)]
pub struct EmittedGainBackend {
    code: OwnedCode,
    layout: GainLayout,
}

impl EmittedGainBackend {
    pub fn new(origin: PmWordAddress24, layout: GainLayout) -> Result<Self, BuildError> {
        Ok(Self {
            code: compile_gain(origin)?,
            layout,
        })
    }

    /// Immutable audit view of the actual code used for each invocation.
    pub fn code(&self) -> &OwnedCode {
        &self.code
    }

    pub const fn layout(&self) -> GainLayout {
        self.layout
    }

    pub(crate) fn execute(
        &self,
        input: &MonoBlock,
        gain: f32,
        budget: InstructionBudget,
    ) -> Result<(MonoBlock, SimulationOutcome), Error> {
        if !gain.is_finite() {
            return Err(Error::NonFiniteGain);
        }
        for (frame, sample) in input.iter().enumerate() {
            if !sample.is_finite() {
                return Err(Error::NonFiniteInput(frame));
            }
        }
        let result = (|| {
            let base = self.layout.input.min(self.layout.output);
            let words = if self.layout.input == self.layout.output {
                BLOCK_FRAMES
            } else {
                2 * BLOCK_FRAMES
            };
            let mut scratch = vec![0u32; words];
            let input_offset = (self.layout.input.get() - base.get()) as usize;
            for (dst, sample) in scratch[input_offset..input_offset + BLOCK_FRAMES]
                .iter_mut()
                .zip(input)
            {
                *dst = sample.to_bits();
            }
            let mut memory = DmMemory::new(base, scratch)?;
            let invocation = GainInvocation::new(
                self.layout.input,
                self.layout.output,
                SampleCount::new(MAX_GAIN_SAMPLES)?,
                gain,
            )?;
            let outcome = simulate_gain(
                &self.code,
                &mut ScalarState::default(),
                &mut memory,
                invocation,
                budget.get(),
            )?;
            let mut output = [0.0; BLOCK_FRAMES];
            for (frame, dst) in output.iter_mut().enumerate() {
                *dst = f32::from_bits(memory.read(self.layout.output.checked_add(frame as u32)?)?);
            }
            Ok((output, outcome))
        })();
        result.map_err(|error: SimulationError| Error::RawExecution(error.into()))
    }

    // Internal adversarial seam only. Production cannot replace the owned code.
    #[cfg(test)]
    pub(crate) fn replace_code_for_test(&mut self, code: OwnedCode) {
        self.code = code;
    }
}

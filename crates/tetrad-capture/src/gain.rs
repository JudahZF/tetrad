//! Stateful offline adapter for the owned fixed32 ISA48 gain smoother.
//! Outputs and next current come only from interpreted DM stores. Explicit initial
//! gains and reseeding are caller policy, not unity defaults or stock reset rules.
//! Logical placement uses 66 adjacent DM words; no physical bank/alias proof.
//! Per-block allocation excludes OOM, realtime and hardware transaction claims.
//! The partial pipeline can opt into input gain and output gain after output HP and any tones.

pub mod emitted;

use crate::{Error, Result};
use emitted::{CompiledGainSmoother, Layout, SAMPLES};
use tetrad_sharc::address::{DmWordAddress32, PmWordAddress24};
use tetrad_sharc::simulator::{DmMemory, ScalarState, SimulationOutcome};

const SCRATCH_WORDS: u32 = 2 * SAMPLES + 2;

/// A successful block and its interpreter instruction/store receipt, not cycles.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessedBlock {
    pub outputs: [f32; SAMPLES as usize],
    pub receipt: SimulationOutcome,
}

/// Private immutable backend/placement and independent caller-seeded gain state.
#[derive(Debug, Clone)]
pub struct GainAdapter {
    compiled: CompiledGainSmoother,
    current: f32,
    target: f32,
}

fn validate_gain(value: f32) -> Result<()> {
    if !value.is_finite() {
        Err(Error::NonFinite)
    } else if value < 0.0 {
        Err(Error::LinearGainRange)
    } else {
        Ok(())
    }
}

impl GainAdapter {
    /// Input32 at base, output32 at base+32, current at base+64, target at base+65.
    /// The checked exclusive DM endpoint must fit u32; code must fit PM24.
    /// Both explicit gains must be finite/nonnegative; signed zero is accepted.
    pub fn new(
        origin: PmWordAddress24,
        base: DmWordAddress32,
        current: f32,
        target: f32,
    ) -> Result<Self> {
        validate_gain(current)?;
        validate_gain(target)?;
        base.checked_add(SCRATCH_WORDS)
            .map_err(emitted::Error::Address)?;
        let output = base.checked_add(SAMPLES).map_err(emitted::Error::Address)?;
        let current_address = base
            .checked_add(2 * SAMPLES)
            .map_err(emitted::Error::Address)?;
        let target_address = base
            .checked_add(2 * SAMPLES + 1)
            .map_err(emitted::Error::Address)?;
        let layout = Layout::new(base, output, current_address, target_address)?;
        Ok(Self {
            compiled: CompiledGainSmoother::compile(origin, layout)?,
            current,
            target,
        })
    }

    pub fn compiled(&self) -> &CompiledGainSmoother {
        &self.compiled
    }

    pub fn current(&self) -> f32 {
        self.current
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn set_linear_target(&mut self, target: f32) -> Result<()> {
        validate_gain(target)?;
        self.target = target;
        Ok(())
    }

    /// Explicit caller policy; preserves the existing backend and placement.
    pub fn reseed(&mut self, current: f32, target: f32) -> Result<()> {
        validate_gain(current)?;
        validate_gain(target)?;
        self.current = current;
        self.target = target;
        Ok(())
    }

    /// Execute real emitted words with private DM and default scalar registers.
    /// Every Result failure retains all adapter state. No host recurrence supplies
    /// outputs or current. Allocation/OOM is outside this Result transaction.
    pub fn process_block(
        &mut self,
        inputs: &[f32; SAMPLES as usize],
        step_budget: usize,
    ) -> Result<ProcessedBlock> {
        let [(input, _), (output, _), (current, _), (target, _)] = self.compiled.layout().spans();
        let mut words = vec![0; SCRATCH_WORDS as usize];
        for (word, sample) in words.iter_mut().zip(inputs) {
            *word = sample.to_bits();
        }
        words[(2 * SAMPLES) as usize] = self.current.to_bits();
        words[(2 * SAMPLES + 1) as usize] = self.target.to_bits();
        let mut memory = DmMemory::new(input, words).map_err(emitted::Error::Simulation)?;
        let mut state = ScalarState::default();
        let receipt = self
            .compiled
            .simulate_atomic(&mut state, &mut memory, step_budget)?;
        let mut outputs = [0.0; SAMPLES as usize];
        for (offset, sample) in outputs.iter_mut().enumerate() {
            let address = output
                .checked_add(offset as u32)
                .map_err(emitted::Error::Address)?;
            *sample = f32::from_bits(memory.read(address).map_err(emitted::Error::Simulation)?);
        }
        let next_current =
            f32::from_bits(memory.read(current).map_err(emitted::Error::Simulation)?);
        if memory.read(target).map_err(emitted::Error::Simulation)? != self.target.to_bits() {
            return Err(emitted::Error::Publication.into());
        }
        self.current = next_current;
        Ok(ProcessedBlock { outputs, receipt })
    }
}

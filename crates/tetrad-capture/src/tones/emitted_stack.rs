//! Owned emitted V1 tone stack: three accepted `emitted::EmittedV1ToneReference`
//! leaves run First → Second → Third, as the callback calls `0x10` three times.
//! Coefficients come from the recovered host builders `v1_tone_gain_reference` and
//! `v1_tone_coefficients_reference` (host tan/sqrt/powf/division); only the
//! coefficient-independent recurrence runs as owned ISA48 words.
//!
//! Each leaf invocation stages its own private logical DM. Placements may coincide;
//! that implies no shared physical image, bank or allocation. Parameter-service
//! ramp/publication (`0x163112`), resident linking, stock layout/ABI, 40-bit/mode
//! parity, DMA and realtime/OOM behavior are not modeled.

use super::{
    V1Tone, V1ToneCoefficients,
    emitted::{EmittedV1ToneReference, ToneLogicalPlacement},
    v1_tone_coefficients_reference, v1_tone_gain_reference,
};
use crate::Result;
use tetrad_sharc::simulator::SimulationOutcome;

/// Executed instruction limits for the three tone leaves, not cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V1ToneInstructionBudgets {
    pub first: usize,
    pub second: usize,
    pub third: usize,
}

/// Final outputs and the actual decoder receipt of each leaf in call order.
#[derive(Debug, Clone, PartialEq)]
pub struct EmittedToneStackBlock {
    pub outputs: [f32; 32],
    pub receipts: [SimulationOutcome; 3],
}

/// Three owned emitted tone sections with the caller's normalized parameters.
/// No default parameters, placements or budgets; nothing mutable is exposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmittedV1ToneStackReference {
    /// Exact caller bits, so signed zero is retained.
    normalized: [u32; 3],
    sections: [EmittedV1ToneReference; 3],
}

impl EmittedV1ToneStackReference {
    /// Validates all three parameters and placements before returning; histories
    /// start at zero as after reset `0x492`.
    pub fn new(normalized: [f32; 3], placements: [ToneLogicalPlacement; 3]) -> Result<Self> {
        let coefficients = coefficients(normalized)?;
        let section = |tone: V1Tone| {
            EmittedV1ToneReference::new(coefficients[tone.index()], placements[tone.index()])
        };
        Ok(Self {
            normalized: normalized.map(f32::to_bits),
            sections: [
                section(V1Tone::First)?,
                section(V1Tone::Second)?,
                section(V1Tone::Third)?,
            ],
        })
    }

    pub fn normalized(&self) -> [f32; 3] {
        self.normalized.map(f32::from_bits)
    }

    pub fn section(&self, tone: V1Tone) -> &EmittedV1ToneReference {
        &self.sections[tone.index()]
    }

    /// Each leaf's exact straight-line step count (366 for the accepted image).
    pub fn full_budgets(&self) -> V1ToneInstructionBudgets {
        let [first, second, third] = self
            .sections
            .each_ref()
            .map(|section| section.compiled().max_steps());
        V1ToneInstructionBudgets {
            first,
            second,
            third,
        }
    }

    /// Block-boundary update as at updater `0x178`: all three or none. Histories,
    /// code and placements stay; nothing is recompiled.
    pub fn set_normalized(&mut self, normalized: [f32; 3]) -> Result<()> {
        let coefficients = coefficients(normalized)?;
        let mut sections = self.sections.clone();
        for (section, coefficients) in sections.iter_mut().zip(coefficients) {
            section.update_coefficients(coefficients)?;
        }
        self.sections = sections;
        self.normalized = normalized.map(f32::to_bits);
        Ok(())
    }

    /// Reset `0x492`: zero all histories; coefficients, parameters and code stay.
    pub fn reset(&mut self) {
        self.sections
            .iter_mut()
            .for_each(EmittedV1ToneReference::reset);
    }

    /// Runs the three leaves on staged clones with explicit per-leaf budgets.
    /// Commits all three only if every leaf succeeds; any error retains the stack.
    pub fn process_block(
        &mut self,
        inputs: &[f32; 32],
        budgets: V1ToneInstructionBudgets,
    ) -> Result<EmittedToneStackBlock> {
        let mut sections = self.sections.clone();
        let [first, second, third] = &mut sections;
        let a = first.process_block(inputs, budgets.first)?;
        let b = second.process_block(&a.outputs, budgets.second)?;
        let c = third.process_block(&b.outputs, budgets.third)?;
        self.sections = sections;
        Ok(EmittedToneStackBlock {
            outputs: c.outputs,
            receipts: [a.receipt, b.receipt, c.receipt],
        })
    }
}

/// Recovered host gain and coefficient laws for all three tones.
fn coefficients(normalized: [f32; 3]) -> Result<[V1ToneCoefficients; 3]> {
    let law = |tone: V1Tone| {
        v1_tone_coefficients_reference(
            tone,
            v1_tone_gain_reference(tone, normalized[tone.index()])?,
        )
    };
    Ok([
        law(V1Tone::First)?,
        law(V1Tone::Second)?,
        law(V1Tone::Third)?,
    ])
}

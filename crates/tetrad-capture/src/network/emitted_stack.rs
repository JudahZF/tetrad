//! Owned staged REFERENCE of the fixed V1 1→2→2→1 neural core whose output-producing float
//! arithmetic runs as raw ISA48 words: five scalar affine leaves, five `emitted::CompiledAdaaClip`
//! leaves and one scale/offset leaf. Input must already be prepared and pre-scaled, as for
//! [`super::NeuralCore`]; beta comes from `NetworkParameters`.
//!
//! Each of the 11 leaf calls stages its own private logical DM and a fresh
//! `ScalarState` and runs under `run_scalar_normal_floats`. This is owned offline
//! orchestration of separate calls: NOT one linked resident image, CALL/ABI, MMIO,
//! memory map or stock layout. Placements may coincide without sharing anything.
//!
//! Host arithmetic is limited to the zero-input seeds and baseline, staging and
//! validation, and the separate [`OwnedNeuralStack`] model. The quotient uses the
//! owned reciprocal (seed `0x7EF311C7 - bits(m)` plus seven Newton steps), so
//! results are NOT stock `RECIPS` parity and NOT [`super::NeuralCore`] (Rust division).
#![forbid(unsafe_code)]

use super::{
    NetworkParameters,
    emitted::{self, CompiledAdaaClip, Layout, OwnedAdaaClip, Preactivation, Receipt, SAMPLES},
};
use tetrad_sharc::{
    address::{DmWordAddress32, PmWordAddress24},
    emit::{IsaProgram, Label, OwnedCode},
    isa48::Instruction48,
    register::{DataReg, FloatReg, UniversalReg},
    simulator::{DmMemory, ScalarState, SimulationOutcome, run_scalar_normal_floats},
};

const N: usize = SAMPLES as usize;
pub const LEAVES: usize = 11;

/// The 11 leaf calls in execution order. `leaf as usize` indexes placements,
/// budgets and receipts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leaf {
    FirstAffine0,
    FirstAffine1,
    FirstAdaa0,
    FirstAdaa1,
    MiddleAffine0,
    MiddleAffine1,
    MiddleAdaa0,
    MiddleAdaa1,
    LastAffine,
    LastAdaa,
    Scale,
}

enum Slot {
    Linear(usize),
    Adaa(usize),
}

impl Leaf {
    pub const ORDER: [Leaf; LEAVES] = [
        Leaf::FirstAffine0,
        Leaf::FirstAffine1,
        Leaf::FirstAdaa0,
        Leaf::FirstAdaa1,
        Leaf::MiddleAffine0,
        Leaf::MiddleAffine1,
        Leaf::MiddleAdaa0,
        Leaf::MiddleAdaa1,
        Leaf::LastAffine,
        Leaf::LastAdaa,
        Leaf::Scale,
    ];

    /// Linear image index, or ADAA image and history index (first0, first1,
    /// middle0, middle1, last).
    fn slot(self) -> Slot {
        match self {
            Leaf::FirstAffine0 => Slot::Linear(0),
            Leaf::FirstAffine1 => Slot::Linear(1),
            Leaf::MiddleAffine0 => Slot::Linear(2),
            Leaf::MiddleAffine1 => Slot::Linear(3),
            Leaf::LastAffine => Slot::Linear(4),
            Leaf::Scale => Slot::Linear(5),
            Leaf::FirstAdaa0 => Slot::Adaa(0),
            Leaf::FirstAdaa1 => Slot::Adaa(1),
            Leaf::MiddleAdaa0 => Slot::Adaa(2),
            Leaf::MiddleAdaa1 => Slot::Adaa(3),
            Leaf::LastAdaa => Slot::Adaa(4),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("parameter word {0:#010x} is subnormal")]
    Coefficient(u32),
    #[error("zero-input seed or baseline arithmetic left the ±0/normal preactivation domain")]
    Seed,
    #[error("{leaf:?}: {error}")]
    Leaf { leaf: Leaf, error: emitted::Error },
}

/// Caller-selected logical PM origin and DM base of one leaf call. No stock
/// address, physical image or default exists. DM spans follow `dm_base`
/// adjacently: one-input affine and scale input32+output32 (64 words); two-input
/// affine input32+input32+output32 (96); ADAA input32+output32+state1 (65).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeafPlacement {
    pub origin: PmWordAddress24,
    pub dm_base: DmWordAddress32,
}

/// Actual decoder result of one leaf call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafReceipt {
    Linear(SimulationOutcome),
    Adaa(Receipt),
}

/// Final DM outputs and every leaf receipt in call order.
#[derive(Debug, Clone, PartialEq)]
pub struct NeuralStackBlock {
    pub outputs: [f32; N],
    pub receipts: [LeafReceipt; LEAVES],
}

/// Straight-line `out[n] = (c0*in0[n] + bias) [+ c1*in1[n]]`, separate f32 ops,
/// coefficients as immediates. One input serves the first affine `w*x + b` and
/// the scale `beta*y + (-baseline)`; two inputs use the stock bias-first order.
/// Registers: F0,F1 inputs; F2,F3 coefficients; F4 bias; F5,F6 temporaries.
/// Direct DM addressing only; R0..R6 and AZ are clobbered. 131 or 228 words.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LinearLeaf {
    code: OwnedCode,
}

impl LinearLeaf {
    fn compile(
        placement: LeafPlacement,
        coefficients: &[f32],
        bias: f32,
    ) -> Result<Self, emitted::Error> {
        let f: [FloatReg; 7] =
            std::array::from_fn(|index| FloatReg::new(index as u8).expect("fixed F0..F6 set"));
        let r = f.map(DataReg::from);
        let inputs = coefficients.len() as u32;
        let at = |span: u32, n: u32| placement.dm_base.checked_add(span * SAMPLES + n);
        let mut program = IsaProgram::new(placement.origin);
        program.define(Label::new("neural_linear.entry"))?;
        for (register, value) in r[2..].iter().zip(coefficients).chain([(&r[4], &bias)]) {
            program.push(Instruction48::Immediate {
                dst: UniversalReg::Data(*register),
                value: value.to_bits(),
            })?;
        }
        for n in 0..SAMPLES {
            for span in 0..inputs {
                program.push(Instruction48::DmDirectLoad {
                    dst: r[span as usize],
                    address: at(span, n)?,
                })?;
            }
            program.push(Instruction48::FloatMultiply {
                dst: f[5],
                lhs: f[2],
                rhs: f[0],
            })?;
            program.push(Instruction48::FloatAdd {
                dst: f[5],
                lhs: f[5],
                rhs: f[4],
            })?;
            if inputs == 2 {
                program.push(Instruction48::FloatMultiply {
                    dst: f[6],
                    lhs: f[3],
                    rhs: f[1],
                })?;
                program.push(Instruction48::FloatAdd {
                    dst: f[5],
                    lhs: f[5],
                    rhs: f[6],
                })?;
            }
            program.push(Instruction48::DmDirectStore {
                src: r[5],
                address: at(inputs, n)?,
            })?;
        }
        program.push(Instruction48::Rts)?;
        Ok(Self {
            code: program.finish()?,
        })
    }

    /// Stages inputs then a zeroed output span at the image's DM base. Every word
    /// executes once, so the outcome must equal the word count and 32 ordered stores.
    fn run(
        &self,
        dm_base: DmWordAddress32,
        inputs: &[[f32; N]],
        budget: usize,
    ) -> Result<([f32; N], SimulationOutcome), emitted::Error> {
        let output = inputs.len() * N;
        let mut words = Vec::with_capacity(output + N);
        for input in inputs {
            words.extend(input.map(f32::to_bits));
        }
        words.resize(output + N, 0);
        let mut memory = DmMemory::new(dm_base, words)?;
        let outcome =
            run_scalar_normal_floats(&self.code, &mut ScalarState::default(), &mut memory, budget)?;
        let expected = (0..SAMPLES)
            .map(|n| dm_base.checked_add(output as u32 + n))
            .collect::<Result<Vec<_>, _>>()?;
        if outcome.steps != self.code.word_count()
            || outcome.stores != N
            || memory.writes() != expected
        {
            return Err(emitted::Error::Receipt);
        }
        let words = memory.words();
        Ok((
            std::array::from_fn(|n| f32::from_bits(words[output + n])),
            outcome,
        ))
    }
}

fn normal(value: f32) -> Option<f32> {
    (value == 0.0 || value.is_normal()).then_some(value)
}

/// Host zero-input seeds and the scale offset, refusing every subnormal
/// intermediate. Two-input affines use the stock `(w0*h0 + b) + w1*h1`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Seeds {
    /// first0, first1, middle0, middle1, last raw preactivations.
    histories: [Preactivation; 5],
    /// `-(beta * clip(zero_last))`, the scale leaf's additive immediate.
    offset: f32,
}

impl Seeds {
    fn new(p: &NetworkParameters) -> Result<Self, Error> {
        if let Some(bad) = p
            .first_weights
            .iter()
            .chain(&p.first_biases)
            .chain(p.middle_weights.iter().flatten())
            .chain(&p.middle_biases)
            .chain(&p.last_weights)
            .chain([&p.last_bias, &p.output_coefficient])
            .find(|value| normal(**value).is_none())
        {
            return Err(Error::Coefficient(bad.to_bits()));
        }
        let n = |value: f32| normal(value).ok_or(Error::Seed);
        // Midpoint order of the ADAA leaf: c = clip v by 2.0; c * (1.0 - |c|*0.25).
        let zero_clip = |value: f32| {
            let c = if value.abs() < 2.0 {
                value
            } else {
                2.0f32.copysign(value)
            };
            n(c * n(1.0 - n(c.abs() * 0.25)?)?)
        };
        let affine =
            |w: [f32; 2], b: f32, h: [f32; 2]| n(n(n(w[0] * h[0])? + b)? + n(w[1] * h[1])?);
        let first = p.first_biases;
        let first_clip = [zero_clip(first[0])?, zero_clip(first[1])?];
        let middle = [
            affine(p.middle_weights[0], p.middle_biases[0], first_clip)?,
            affine(p.middle_weights[1], p.middle_biases[1], first_clip)?,
        ];
        let middle_clip = [zero_clip(middle[0])?, zero_clip(middle[1])?];
        let last = affine(p.last_weights, p.last_bias, middle_clip)?;
        let baseline = n(p.output_coefficient * zero_clip(last)?)?;
        let seed = |value: f32| Preactivation::new(value).map_err(|_| Error::Seed);
        Ok(Self {
            histories: [
                seed(first[0])?,
                seed(first[1])?,
                seed(middle[0])?,
                seed(middle[1])?,
                seed(last)?,
            ],
            offset: -baseline,
        })
    }
}

/// Five affine, five ADAA and one scale image bound immutably to the caller's
/// parameters and placements, plus five raw histories. Nothing mutable is exposed.
#[derive(Debug, Clone)]
pub struct EmittedNeuralStack {
    placements: [LeafPlacement; LEAVES],
    linear: [LinearLeaf; 6],
    adaa: [CompiledAdaaClip; 5],
    seeds: Seeds,
    histories: [Preactivation; 5],
}

impl EmittedNeuralStack {
    /// Validates ±0/normal parameters and seeds, compiles all 11 images and starts
    /// from the zero-input seeds.
    pub fn new(
        parameters: NetworkParameters,
        placements: [LeafPlacement; LEAVES],
    ) -> Result<Self, Error> {
        let seeds = Seeds::new(&parameters)?;
        let p = &parameters;
        let linear = |leaf: Leaf, coefficients: &[f32], bias: f32| {
            LinearLeaf::compile(placements[leaf as usize], coefficients, bias)
                .map_err(|error| Error::Leaf { leaf, error })
        };
        let adaa = |leaf: Leaf| {
            let LeafPlacement { origin, dm_base } = placements[leaf as usize];
            let compile = || {
                let output = dm_base.checked_add(SAMPLES)?;
                let state = dm_base.checked_add(2 * SAMPLES)?;
                CompiledAdaaClip::compile(origin, Layout::new(dm_base, output, state)?)
            };
            compile().map_err(|error| Error::Leaf { leaf, error })
        };
        Ok(Self {
            linear: [
                linear(Leaf::FirstAffine0, &[p.first_weights[0]], p.first_biases[0])?,
                linear(Leaf::FirstAffine1, &[p.first_weights[1]], p.first_biases[1])?,
                linear(
                    Leaf::MiddleAffine0,
                    &p.middle_weights[0],
                    p.middle_biases[0],
                )?,
                linear(
                    Leaf::MiddleAffine1,
                    &p.middle_weights[1],
                    p.middle_biases[1],
                )?,
                linear(Leaf::LastAffine, &p.last_weights, p.last_bias)?,
                linear(Leaf::Scale, &[p.output_coefficient], seeds.offset)?,
            ],
            adaa: [
                adaa(Leaf::FirstAdaa0)?,
                adaa(Leaf::FirstAdaa1)?,
                adaa(Leaf::MiddleAdaa0)?,
                adaa(Leaf::MiddleAdaa1)?,
                adaa(Leaf::LastAdaa)?,
            ],
            histories: seeds.histories,
            seeds,
            placements,
        })
    }

    /// Immutable image of one leaf, for auditing.
    pub fn code(&self, leaf: Leaf) -> &OwnedCode {
        match leaf.slot() {
            Slot::Linear(index) => &self.linear[index].code,
            Slot::Adaa(index) => self.adaa[index].code(),
        }
    }

    /// Raw previous preactivations: first0, first1, middle0, middle1, last.
    pub fn histories(&self) -> [Preactivation; 5] {
        self.histories
    }

    /// Word count of each straight-line leaf; all-quotient steps of each ADAA leaf.
    pub fn full_budgets(&self) -> [usize; LEAVES] {
        Leaf::ORDER.map(|leaf| match leaf.slot() {
            Slot::Linear(index) => self.linear[index].code.word_count(),
            Slot::Adaa(index) => self.adaa[index]
                .step_plan()
                .steps(SAMPLES)
                .expect("private compiled ADAA plan fits the fixed block"),
        })
    }

    /// Restore the zero-input seeds of the bound parameters; images stay.
    pub fn reset(&mut self) {
        self.histories = self.seeds.histories;
    }

    /// Atomic: builds candidate parameters, seeds and images at the retained
    /// placements, then resets. Any error retains the whole stack.
    pub fn reload(&mut self, parameters: NetworkParameters) -> Result<(), Error> {
        *self = Self::new(parameters, self.placements)?;
        Ok(())
    }

    /// Runs the 11 leaves in [`Leaf::ORDER`] with explicit per-leaf budgets. Every
    /// ADAA input is checked by `Preactivation::new` before its call. The five
    /// histories commit only after all 11 calls succeed; any error retains them.
    pub fn process_block(
        &mut self,
        input: &[f32; N],
        budgets: &[usize; LEAVES],
    ) -> Result<NeuralStackBlock, Error> {
        let mut call = Call {
            stack: self,
            budgets,
            histories: self.histories,
            receipts: [LeafReceipt::Linear(SimulationOutcome {
                steps: 0,
                stores: 0,
            }); LEAVES],
        };
        let input = std::slice::from_ref(input);
        let first = [
            call.linear(Leaf::FirstAffine0, input)?,
            call.linear(Leaf::FirstAffine1, input)?,
        ];
        let first = [
            call.adaa(Leaf::FirstAdaa0, &first[0])?,
            call.adaa(Leaf::FirstAdaa1, &first[1])?,
        ];
        let middle = [
            call.linear(Leaf::MiddleAffine0, &first)?,
            call.linear(Leaf::MiddleAffine1, &first)?,
        ];
        let middle = [
            call.adaa(Leaf::MiddleAdaa0, &middle[0])?,
            call.adaa(Leaf::MiddleAdaa1, &middle[1])?,
        ];
        let last = call.linear(Leaf::LastAffine, &middle)?;
        let last = call.adaa(Leaf::LastAdaa, &last)?;
        let outputs = call.linear(Leaf::Scale, std::slice::from_ref(&last))?;
        let (histories, receipts) = (call.histories, call.receipts);
        self.histories = histories;
        Ok(NeuralStackBlock { outputs, receipts })
    }
}

/// Staged state of one block; dropped on error.
struct Call<'a> {
    stack: &'a EmittedNeuralStack,
    budgets: &'a [usize; LEAVES],
    histories: [Preactivation; 5],
    receipts: [LeafReceipt; LEAVES],
}

impl Call<'_> {
    fn linear(&mut self, leaf: Leaf, inputs: &[[f32; N]]) -> Result<[f32; N], Error> {
        let Slot::Linear(index) = leaf.slot() else {
            unreachable!("linear call on ADAA leaf")
        };
        let (outputs, outcome) = self.stack.linear[index]
            .run(
                self.stack.placements[leaf as usize].dm_base,
                inputs,
                self.budgets[leaf as usize],
            )
            .map_err(|error| Error::Leaf { leaf, error })?;
        self.receipts[leaf as usize] = LeafReceipt::Linear(outcome);
        Ok(outputs)
    }

    fn adaa(&mut self, leaf: Leaf, input: &[f32; N]) -> Result<[f32; N], Error> {
        let Slot::Adaa(index) = leaf.slot() else {
            unreachable!("ADAA call on linear leaf")
        };
        let refuse = |error| Error::Leaf { leaf, error };
        for &value in input {
            Preactivation::new(value).map_err(refuse)?;
        }
        let compiled = &self.stack.adaa[index];
        let mut words = Vec::with_capacity(2 * N + 1);
        words.extend(input.map(f32::to_bits));
        words.resize(2 * N, 0);
        words.push(self.histories[index].get().to_bits());
        let mut memory = DmMemory::new(compiled.layout().input(), words)
            .map_err(|error| refuse(error.into()))?;
        let receipt = compiled
            .simulate_atomic(
                &mut ScalarState::default(),
                &mut memory,
                self.budgets[leaf as usize],
            )
            .map_err(refuse)?;
        let words = memory.words();
        self.histories[index] = Preactivation::new(f32::from_bits(words[2 * N])).map_err(refuse)?;
        self.receipts[leaf as usize] = LeafReceipt::Adaa(receipt);
        Ok(std::array::from_fn(|n| f32::from_bits(words[N + n])))
    }
}

/// Host model of the same 11 steps in f32: shared seeds, separate ops,
/// `emitted::OwnedAdaaClip` (owned reciprocal), refusing every subnormal or
/// nonfinite operand or result. Not `NeuralCore` and not stock parity.
#[derive(Debug, Clone)]
pub struct OwnedNeuralStack {
    parameters: NetworkParameters,
    seeds: Seeds,
    neurons: [OwnedAdaaClip; 5],
}

impl OwnedNeuralStack {
    pub fn new(parameters: NetworkParameters) -> Result<Self, Error> {
        let seeds = Seeds::new(&parameters)?;
        Ok(Self {
            neurons: seeds.histories.map(OwnedAdaaClip::new),
            parameters,
            seeds,
        })
    }

    pub fn reset(&mut self) {
        self.neurons = self.seeds.histories.map(OwnedAdaaClip::new);
    }

    /// Atomic: histories advance only if the whole block succeeds.
    pub fn process_block(&mut self, input: &[f32; N]) -> Result<[f32; N], Error> {
        let p = &self.parameters;
        let mut neurons = self.neurons;
        let mut adaa = |leaf: Leaf, index: usize, raw: [f32; N]| {
            let refuse = |error| Error::Leaf { leaf, error };
            // Same refusal point as the emitted path: before the ADAA call.
            let mut block = [Preactivation::new(0.0).map_err(refuse)?; N];
            for (slot, value) in block.iter_mut().zip(raw) {
                *slot = Preactivation::new(value).map_err(refuse)?;
            }
            neurons[index]
                .process_block(&block)
                .map(|block| block.output)
                .map_err(refuse)
        };
        let first = [
            linear(
                Leaf::FirstAffine0,
                &[p.first_weights[0]],
                p.first_biases[0],
                &[input],
            )?,
            linear(
                Leaf::FirstAffine1,
                &[p.first_weights[1]],
                p.first_biases[1],
                &[input],
            )?,
        ];
        let first = [
            adaa(Leaf::FirstAdaa0, 0, first[0])?,
            adaa(Leaf::FirstAdaa1, 1, first[1])?,
        ];
        let inputs = [&first[0], &first[1]];
        let middle = [
            linear(
                Leaf::MiddleAffine0,
                &p.middle_weights[0],
                p.middle_biases[0],
                &inputs,
            )?,
            linear(
                Leaf::MiddleAffine1,
                &p.middle_weights[1],
                p.middle_biases[1],
                &inputs,
            )?,
        ];
        let middle = [
            adaa(Leaf::MiddleAdaa0, 2, middle[0])?,
            adaa(Leaf::MiddleAdaa1, 3, middle[1])?,
        ];
        let last = linear(
            Leaf::LastAffine,
            &p.last_weights,
            p.last_bias,
            &[&middle[0], &middle[1]],
        )?;
        let last = adaa(Leaf::LastAdaa, 4, last)?;
        let output = linear(
            Leaf::Scale,
            &[p.output_coefficient],
            self.seeds.offset,
            &[&last],
        )?;
        self.neurons = neurons;
        Ok(output)
    }
}

/// Host twin of [`LinearLeaf`]: same operations and order, checking operands too.
fn linear(
    leaf: Leaf,
    coefficients: &[f32],
    bias: f32,
    inputs: &[&[f32; N]],
) -> Result<[f32; N], Error> {
    let mut output = [0.0; N];
    for (sample, y) in output.iter_mut().enumerate() {
        let n = |value: f32| {
            normal(value).ok_or(Error::Leaf {
                leaf,
                error: emitted::Error::NonNormal {
                    sample: Some(sample as u32),
                },
            })
        };
        let mut sum = 0.0;
        for (term, (&c, input)) in coefficients.iter().zip(inputs).enumerate() {
            let product = n(c * n(input[sample])?)?;
            sum = if term == 0 {
                n(product + bias)?
            } else {
                n(sum + product)?
            };
        }
        *y = sum;
    }
    Ok(output)
}

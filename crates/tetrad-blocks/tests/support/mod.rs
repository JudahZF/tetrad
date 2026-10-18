#![allow(dead_code)]

pub mod blobdis;
pub mod deployment;

use std::num::NonZeroU16;

use tetrad_blocks::dsp::biquad::Cascade;
use tetrad_blocks::{
    BLOCK_SAMPLES, BlockError, Callback, HostReference, LinkedModule, OwnedBlock, ParameterLaw,
    ParameterSpec, RoutineLabels, StaticLayout,
};
use tetrad_sharc::emit::{BranchRelocation, IsaProgram, Label, LoopCount};
use tetrad_sharc::isa48::{BranchKind, Condition, DmAccess, Instruction48, MODE1_RND32, Mode1Mask};
use tetrad_sharc::math::prm;
use tetrad_sharc::register::{Dag1Index, Dag1Modify, DataReg, FloatReg, UniversalReg};

pub fn r(n: u8) -> DataReg {
    DataReg::new(n).unwrap()
}
fn f(n: u8) -> FloatReg {
    FloatReg::new(n).unwrap()
}
fn i(n: u8) -> Dag1Index {
    Dag1Index::new(n).unwrap()
}
fn m(n: u8) -> Dag1Modify {
    Dag1Modify::new(n).unwrap()
}

/// A test block with the forms a real body needs: a counted loop over the
/// byte-space samples, register moves, offset loads and stores, a conditional
/// branch. State: [gain, last output]. LEVEL (0) stores v as the gain; reset
/// clears the history word only.
#[derive(Clone, Copy)]
pub struct Gain;

pub const GAIN_PARAMETERS: &[ParameterSpec] = &[ParameterSpec {
    runtime_index: 0,
    name: "LEVEL",
    law: ParameterLaw::Range {
        minimum: 0.0,
        maximum: 1.0,
        skew: 1.0,
    },
}];

/// Leaf steps: 4 before the loop, 3 per sample, 3 to store the history word and return.
pub const GAIN_PROCESS_CYCLES: u64 = 4 + 3 * BLOCK_SAMPLES as u64 + 3;

impl OwnedBlock for Gain {
    fn model_id(&self) -> u32 {
        0x7fff_0001
    }
    fn parameters(&self) -> &'static [ParameterSpec] {
        GAIN_PARAMETERS
    }
    fn static_layout(&self) -> StaticLayout {
        StaticLayout::Channels([vec![1.0_f32.to_bits(), 0], vec![0.5_f32.to_bits(), 0]])
    }
    fn process_cycles(&self) -> u64 {
        GAIN_PROCESS_CYCLES
    }
    fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
        let labels = labels("gain");
        let last = Label::new("gain.loop_end");
        let skip = Label::new("gain.parameter_end");
        let to_index = |register| Instruction48::RegisterMove {
            dst: UniversalReg::Index(i(4)),
            src: UniversalReg::Data(register),
        };
        program.define(labels.process.clone())?;
        program.push(Instruction48::DmOffsetLoad {
            dst: r(12),
            index: i(4),
            offset: 0,
        })?;
        program.push(to_index(r(8)))?;
        program.push(Instruction48::Immediate {
            dst: UniversalReg::Modify(m(4)),
            value: 1,
        })?;
        program.counted_loop(
            LoopCount::Immediate(NonZeroU16::new(32).unwrap()),
            last.clone(),
        )?;
        program.push(Instruction48::DmOffsetLoad {
            dst: r(0),
            index: i(4),
            offset: 0,
        })?;
        program.push(Instruction48::FloatMultiply {
            dst: f(0),
            lhs: f(0),
            rhs: f(12),
        })?;
        program.define(last)?;
        program.push(Instruction48::DmStore {
            src: r(0),
            index: i(4),
            modifier: m(4),
            access: DmAccess::PostModify,
        })?;
        program.push(to_index(r(4)))?;
        program.push(Instruction48::DmOffsetStore {
            src: r(0),
            index: i(4),
            offset: 1,
        })?;
        program.push(Instruction48::Rts)?;

        program.define(labels.set_parameter.clone())?;
        program.push(Instruction48::IntegerPass {
            dst: r(8),
            src: r(8),
        })?;
        program.branch_to(
            BranchKind::Jump,
            Condition::NotEqual,
            skip.clone(),
            BranchRelocation::PcRel24,
        )?;
        program.push(Instruction48::DmOffsetStore {
            src: r(12),
            index: i(4),
            offset: 0,
        })?;
        program.define(skip)?;
        program.push(Instruction48::Rts)?;

        program.define(labels.reset.clone())?;
        program.push(Instruction48::Immediate {
            dst: UniversalReg::Data(r(0)),
            value: 0,
        })?;
        program.push(Instruction48::DmOffsetStore {
            src: r(0),
            index: i(4),
            offset: 1,
        })?;
        program.push(Instruction48::Rts)?;
        Ok(labels)
    }
}

impl HostReference for Gain {
    fn process(&self, state: &mut [u32], samples: &mut [f32; BLOCK_SAMPLES]) {
        let gain = f32::from_bits(state[0]);
        for sample in samples.iter_mut() {
            *sample = prm::mul(*sample, gain);
        }
        state[1] = samples[BLOCK_SAMPLES - 1].to_bits();
    }
    fn set_parameter(&self, state: &mut [u32], index: u32, value: f32) {
        if index == 0 {
            state[0] = value.to_bits();
        }
    }
    fn reset(&self, state: &mut [u32]) {
        state[1] = 0;
    }
}

fn labels(prefix: &str) -> RoutineLabels {
    RoutineLabels {
        process: Label::new(format!("{prefix}.process")),
        set_parameter: Label::new(format!("{prefix}.parameter")),
        reset: Label::new(format!("{prefix}.reset")),
    }
}

/// One ABI violation each in the process leaf; the other leaves return.
#[derive(Debug, Clone, Copy)]
pub enum Mutation {
    /// R3 is callee-preserved.
    ClobberR3,
    /// I5 is callee-preserved (the first-pass adapter clobbered it).
    ClobberI5,
    /// The leaf must not change MODE1.
    ClearRnd32,
    /// Rewrites the kernel's saved I6 with the same value: any write outside
    /// the leaf's own memory is a finding. The stub's CJUMP frame holds the
    /// kernel's frame pointer.
    WriteCallerFrame,
    /// Infinity minus infinity sets AIS: the kernel would fault the slot.
    InvalidSubtract,
    /// Pushes R3, pops it, then reloads it from below I7, where an interrupt
    /// may have pushed (V2 canary).
    ReadBelowStack,
}

impl Mutation {
    pub const ALL: [Self; 6] = [
        Self::ClobberR3,
        Self::ClobberI5,
        Self::ClearRnd32,
        Self::WriteCallerFrame,
        Self::InvalidSubtract,
        Self::ReadBelowStack,
    ];

    fn violation(self) -> Vec<Instruction48> {
        match self {
            Self::ClobberR3 => vec![Instruction48::Immediate {
                dst: UniversalReg::Data(r(3)),
                value: 0,
            }],
            Self::ClobberI5 => vec![Instruction48::RegisterMove {
                dst: UniversalReg::Index(i(5)),
                src: UniversalReg::Data(r(4)),
            }],
            Self::ClearRnd32 => vec![Instruction48::Mode1Clear(
                Mode1Mask::new(MODE1_RND32).unwrap(),
            )],
            Self::WriteCallerFrame => vec![
                Instruction48::DmOffsetLoad {
                    dst: r(0),
                    index: i(6),
                    offset: 0,
                },
                Instruction48::RegisterMove {
                    dst: UniversalReg::Index(i(4)),
                    src: UniversalReg::Data(r(0)),
                },
                Instruction48::DmOffsetLoad {
                    dst: r(0),
                    index: i(4),
                    offset: 0,
                },
                Instruction48::DmOffsetStore {
                    src: r(0),
                    index: i(4),
                    offset: 0,
                },
            ],
            Self::InvalidSubtract => vec![
                Instruction48::Immediate {
                    dst: UniversalReg::Data(r(0)),
                    value: f32::INFINITY.to_bits(),
                },
                Instruction48::FloatSubtract {
                    dst: f(0),
                    lhs: f(0),
                    rhs: f(0),
                },
            ],
            Self::ReadBelowStack => vec![
                Instruction48::DmStore {
                    src: r(3),
                    index: i(7),
                    modifier: m(7),
                    access: DmAccess::PostModify,
                },
                Instruction48::ModifyWords {
                    index: i(7),
                    words: 1,
                },
                Instruction48::DmOffsetLoad {
                    dst: r(3),
                    index: i(7),
                    offset: 0,
                },
            ],
        }
    }
}

impl OwnedBlock for Mutation {
    fn model_id(&self) -> u32 {
        0x7fff_0002
    }
    fn parameters(&self) -> &'static [ParameterSpec] {
        &[]
    }
    fn static_layout(&self) -> StaticLayout {
        StaticLayout::Channels([vec![0], vec![0]])
    }
    fn process_cycles(&self) -> u64 {
        5
    }
    fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
        let labels = labels("mutation");
        program.define(labels.process.clone())?;
        for instruction in self.violation() {
            program.push(instruction)?;
        }
        program.push(Instruction48::Rts)?;
        for label in [&labels.set_parameter, &labels.reset] {
            program.define(label.clone())?;
            program.push(Instruction48::Rts)?;
        }
        Ok(labels)
    }
}

impl HostReference for Mutation {
    fn process(&self, _state: &mut [u32], _samples: &mut [f32; BLOCK_SAMPLES]) {}
    fn set_parameter(&self, _state: &mut [u32], _index: u32, _value: f32) {}
    fn reset(&self, _state: &mut [u32]) {}
}

/// Round-off of the `cascade` twin on `state` (live = target), in dB: the
/// RMS difference from an f64 DF1 with the same live coefficients, relative
/// to the RMS f64 output. Input: 400 blocks of fixed uniform noise in
/// ±0.5; the first 100 blocks are not measured.
pub fn round_off_db(cascade: &Cascade, state: &[u32]) -> f64 {
    let mut state = state.to_vec();
    let coefficients: Vec<[f64; 5]> = (0..cascade.sections.get() as usize)
        .map(|k| Cascade::live(&state, k).0.map(f64::from))
        .collect();
    let mut history = vec![[0.0_f64; 4]; coefficients.len()];
    let mut seed = 0x1234_5678_u32;
    let (mut error, mut power) = (0.0, 0.0);
    for block in 0..400 {
        let mut samples = [0.0_f32; BLOCK_SAMPLES];
        for sample in &mut samples {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *sample = (seed >> 8) as f32 / (1 << 24) as f32 - 0.5;
        }
        let mut reference = samples.map(f64::from);
        for ([b0, b1, b2, a1, a2], [x1, x2, y1, y2]) in coefficients.iter().zip(&mut history) {
            for x in &mut reference {
                let y = b0 * *x + b1 * *x1 + b2 * *x2 - a1 * *y1 - a2 * *y2;
                (*x2, *x1, *y2, *y1) = (*x1, *x, *y1, y);
                *x = y;
            }
        }
        cascade.process(&mut state, &mut samples);
        if block >= 100 {
            for (sample, exact) in samples.iter().zip(reference) {
                error += (f64::from(*sample) - exact).powi(2);
                power += exact.powi(2);
            }
        }
    }
    10.0 * (error / power).log10()
}

/// V4 vectors of the shim: the PRM 5a entry transfer, the stock VISA C return
/// (PRM/qc-asm vector, tetrad-sharc `tests/visa.rs`) in the entry and every stub,
/// the stock wrapper stub (volume `0x004`) and the 48-bit stock C return in
/// section 3 (fortin `0xdf..0xe2`).
pub fn verify_stub_vectors(linked: &LinkedModule) {
    let c_return = [0x4dfe, 0x0e3f, 0x083f, 0x343f, 0x1901, 0x0001];
    assert_eq!(&linked.visa[..3], &[0x707e, 0x0a00, 0x0000]); // 5a: I4 = R4.
    assert!(
        linked
            .visa
            .windows(c_return.len())
            .any(|window| window == c_return)
    );
    let isa = linked.at.isa..linked.at.isa + linked.isa.word_count() as u32;
    for callback in Callback::ALL {
        let offset = (linked.callback(callback) - linked.at.visa) as usize;
        if callback == Callback::Destroy {
            assert_eq!(&linked.visa[offset..offset + 6], &c_return);
            continue;
        }
        let stub = &linked.visa[offset..offset + 13];
        let field = |at: usize| (u32::from(stub[at]) << 16) | u32::from(stub[at + 1]);
        assert_eq!(stub[0], 0x1804); // 25a: CJUMP addr24 (DB).
        assert!(isa.contains(&field(1)));
        assert_eq!(stub[3], 0x9ff2); // 3c: DM(I7, M7) = R2.
        assert_eq!(stub[4], 0x9fc0); // 16a: DM(I7, M7) = imm32.
        assert_eq!(field(5), linked.at.visa + offset as u32 + 6);
        assert_eq!(&stub[7..], &c_return);
    }
    let words: Vec<u64> = linked.isa.words().iter().map(|w| w.bits()).collect();
    let isa_return = [0x4dfe_0e00_0000, 0x083f_3400_0000, 0x1900_0000_0000, 0];
    assert!(words.windows(4).any(|window| window == isa_return));
}

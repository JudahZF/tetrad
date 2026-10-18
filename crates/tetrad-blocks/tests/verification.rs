//! V2 and V5: the linked module on `Machine` (PRM rules, kernel protocol)
//! against the twin, and one named failure per ABI violation.
mod support;

use support::{GAIN_PROCESS_CYCLES, Gain, Mutation};
use tetrad_blocks::verification::{
    Call, ParameterUpdate, VerificationError, standard_frames, verify_module,
};
use tetrad_blocks::{
    BLOCK_SAMPLES, BlockError, BuiltModule, Callback, HostReference, OwnedBlock, ParameterSpec,
    RoutineLabels, StaticLayout,
};
use tetrad_sharc::emit::{IsaProgram, Label};
use tetrad_sharc::isa48::Instruction48;
use tetrad_sharc::math::prm;
use tetrad_sharc::register::{Dag1Index, DataReg, FloatReg, UniversalReg};

/// 64 frames with gain steps. NaN, infinity, −0, 1.5 and 1 + 1 ulp must be
/// dropped by the adapter (the twin never sees them); index 1 is ignored by
/// the leaf. Process cost equals the stats bound exactly for this body.
#[test]
fn gain_matches_its_twin_through_the_whole_kernel_lifecycle() {
    let mut frames = standard_frames();
    let steps = [
        (8, 0, 0.25),
        (9, 1, 0.75),
        (20, 0, 1.0),
        (40, 0, 0.0),
        (41, 0, 0.6),
    ];
    for (frame, index, value) in steps {
        frames[frame]
            .parameters
            .push(ParameterUpdate { index, value });
    }
    for value in [
        f32::NAN,
        f32::INFINITY,
        -0.0,
        1.5,
        f32::from_bits(0x3f80_0001),
    ] {
        frames[30]
            .parameters
            .push(ParameterUpdate { index: 0, value });
    }
    frames[33].samples[1][31] = f32::from_bits(0x8000_0001);
    let report = verify_module(&Gain, &frames).unwrap();
    // Entry, reset, 10 set-parameter, 64 process, reset R8 = 0, recovery, destroy.
    assert_eq!((report.frames, report.calls), (64, 79));
    let bound = BuiltModule::build(&Gain)
        .unwrap()
        .stats
        .cycles_per_stereo_block;
    assert_eq!(report.maximum_process_cycles, bound);
    assert!(bound > 2 * GAIN_PROCESS_CYCLES);
}

#[test]
fn each_abi_violation_fails_its_named_check() {
    let process = Call::Callback(Callback::Process);
    for mutation in Mutation::ALL {
        let error = verify_module(&mutation, &standard_frames()).unwrap_err();
        let named = match mutation {
            Mutation::ClobberR3 => matches!(
                error,
                VerificationError::Register { call, class: "R", index: 3 } if call == process
            ),
            Mutation::ClobberI5 => matches!(
                error,
                VerificationError::Register { call, class: "I", index: 5 } if call == process
            ),
            Mutation::ClearRnd32 => matches!(
                error,
                VerificationError::Register { call, class: "MODE1", .. } if call == process
            ),
            Mutation::WriteCallerFrame => matches!(
                error,
                VerificationError::Write { call, address: 0x24_a7f0 } if call == process
            ),
            Mutation::InvalidSubtract => matches!(
                error,
                VerificationError::InvalidFloat { call } if call == process
            ),
            Mutation::ReadBelowStack => matches!(
                error,
                VerificationError::Register { call, class: "R", index: 3 } if call == process
            ),
        };
        assert!(named, "{mutation:?}: {error}");
    }
}

/// State × (1 − 2^-24) from 2^-126: exactly 2^-126 − 2^-150. Host f32 rounds
/// it up to 2^-126 forever; the PRM keeps 24 bits and flushes it to zero. V2
/// runs the PRM rules, so the twin must use `prm::mul`.
struct Underflow;

const BELOW_ONE: f32 = f32::from_bits(0x3f7f_ffff);

impl OwnedBlock for Underflow {
    fn model_id(&self) -> u32 {
        0x7fff_0003
    }
    fn parameters(&self) -> &'static [ParameterSpec] {
        &[]
    }
    fn static_layout(&self) -> StaticLayout {
        let start = vec![f32::MIN_POSITIVE.to_bits()];
        StaticLayout::Channels([start.clone(), start])
    }
    fn process_cycles(&self) -> u64 {
        5
    }
    fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
        let labels = RoutineLabels {
            process: Label::new("underflow.process"),
            set_parameter: Label::new("underflow.parameter"),
            reset: Label::new("underflow.reset"),
        };
        let (r0, r2) = (DataReg::new(0).unwrap(), DataReg::new(2).unwrap());
        let i4 = Dag1Index::new(4).unwrap();
        program.define(labels.process.clone())?;
        program.push(Instruction48::DmOffsetLoad {
            dst: r0,
            index: i4,
            offset: 0,
        })?;
        program.push(Instruction48::Immediate {
            dst: UniversalReg::Data(r2),
            value: BELOW_ONE.to_bits(),
        })?;
        program.push(Instruction48::FloatMultiply {
            dst: FloatReg::from(r0),
            lhs: FloatReg::from(r0),
            rhs: FloatReg::from(r2),
        })?;
        program.push(Instruction48::DmOffsetStore {
            src: r0,
            index: i4,
            offset: 0,
        })?;
        program.push(Instruction48::Rts)?;
        for label in [&labels.set_parameter, &labels.reset] {
            program.define(label.clone())?;
            program.push(Instruction48::Rts)?;
        }
        Ok(labels)
    }
}

impl HostReference for Underflow {
    fn process(&self, state: &mut [u32], _samples: &mut [f32; BLOCK_SAMPLES]) {
        state[0] = prm::mul(f32::from_bits(state[0]), BELOW_ONE).to_bits();
    }
    fn set_parameter(&self, _state: &mut [u32], _index: u32, _value: f32) {}
    fn reset(&self, _state: &mut [u32]) {}
}

#[test]
fn v2_runs_the_prm_rules_so_twins_flush_at_underflow() {
    verify_module(&Underflow, &standard_frames()).unwrap();
    assert_eq!((f32::MIN_POSITIVE * BELOW_ONE).to_bits(), 0x0080_0000);
}

/// A stereo block whose leaves count their calls: state words process,
/// set-parameter, reset. One state means one leaf call per callback, as in
/// stock; a second call would double a count against the twin.
struct Counter;

impl OwnedBlock for Counter {
    fn model_id(&self) -> u32 {
        0x7fff_0004
    }
    fn parameters(&self) -> &'static [ParameterSpec] {
        &[]
    }
    fn static_layout(&self) -> StaticLayout {
        StaticLayout::Stereo(vec![0; 3])
    }
    fn process_cycles(&self) -> u64 {
        4
    }
    fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
        let labels = RoutineLabels {
            process: Label::new("counter.process"),
            set_parameter: Label::new("counter.parameter"),
            reset: Label::new("counter.reset"),
        };
        let (r0, i4) = (DataReg::new(0).unwrap(), Dag1Index::new(4).unwrap());
        for (word, label) in [&labels.process, &labels.set_parameter, &labels.reset]
            .into_iter()
            .enumerate()
        {
            program.define(label.clone())?;
            let offset = word as i32;
            program.push(Instruction48::DmOffsetLoad {
                dst: r0,
                index: i4,
                offset,
            })?;
            program.push(Instruction48::IntegerIncrement { dst: r0, src: r0 })?;
            program.push(Instruction48::DmOffsetStore {
                src: r0,
                index: i4,
                offset,
            })?;
            program.push(Instruction48::Rts)?;
        }
        Ok(labels)
    }
}

impl HostReference for Counter {
    fn process(&self, state: &mut [u32], _samples: &mut [f32; BLOCK_SAMPLES]) {
        state[0] += 1;
    }
    fn process_stereo(&self, state: &mut [u32], samples: &mut [[f32; BLOCK_SAMPLES]; 2]) {
        self.process(state, &mut samples[0]);
    }
    fn set_parameter(&self, state: &mut [u32], _index: u32, _value: f32) {
        state[1] += 1;
    }
    fn reset(&self, state: &mut [u32]) {
        state[2] += 1;
    }
}

#[test]
fn a_stereo_layout_calls_each_leaf_once_per_callback() {
    let mut frames = standard_frames();
    frames[3].reset = true;
    frames[5].parameters.push(ParameterUpdate {
        index: 0,
        value: 0.5,
    });
    verify_module(&Counter, &frames).unwrap();
    let built = BuiltModule::build(&Counter).unwrap();
    assert_eq!(built.layout.channel_b_offset_bytes, None);
    assert_eq!(built.stats.dm_bytes, 16);
}

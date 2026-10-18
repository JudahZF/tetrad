//! Simple Gate: recovered dynamics, PRM twin and stock-ABI module checks.
mod support;

use qc_format::blob::{ModuleBlob, Section};
use tetrad_blocks::blocks::gate::{
    ATTACK_SECONDS, DETECTOR_FAST, DETECTOR_SLOW, HOLD_SAMPLES, RELEASE_SECONDS, SAMPLE_RATE,
    SimpleGate,
};
use tetrad_blocks::verification::{
    ParameterUpdate, VerificationFrame, scheduled_frames, verify_module, verify_module_at,
};
use tetrad_blocks::{
    BLOCK_SAMPLES, BuiltModule, HostReference, LinkedModule, OwnedBlock, SectionAddresses,
};
use tetrad_sharc::math::{self, prm};

#[test]
fn detector_uses_the_stereo_sum_and_the_whole_block_peak() {
    let gate = SimpleGate;
    let initial = gate.static_layout().states()[0].clone();
    let mut state = initial.clone();
    let mut cancellation = [[0.75; BLOCK_SAMPLES], [-0.75; BLOCK_SAMPLES]];
    gate.process_pair(&mut state, &mut cancellation);
    assert!(
        cancellation
            .into_iter()
            .flatten()
            .all(|sample| sample == 0.0)
    );
    assert_eq!(SimpleGate::dynamics(&state).envelope, 0.0);
    assert!(!SimpleGate::dynamics(&state).open);

    let mut state = initial;
    let mut late_impulse = [[0.0; BLOCK_SAMPLES]; 2];
    late_impulse[0][BLOCK_SAMPLES - 1] = 1.0;
    gate.process_pair(&mut state, &mut late_impulse);
    // The last sample's impulse drives the envelope from sample zero.
    let envelopes = &state[state.len() - BLOCK_SAMPLES..];
    assert_eq!(envelopes[0], prm::mul(DETECTOR_FAST, 0.5).to_bits());
    assert!(SimpleGate::dynamics(&state).open);
    assert!(late_impulse[0][BLOCK_SAMPLES - 1] > 0.0);
}

#[test]
fn threshold_is_published_at_the_next_block_start() {
    let gate = SimpleGate;
    let mut state = gate.static_layout().states()[0].clone();
    let original = state.clone();
    gate.set_parameter(&mut state, 1, 1.0);
    assert_eq!(state, original, "unknown runtime index must be ignored");
    gate.set_parameter(&mut state, 0, 0.0);
    assert_eq!(SimpleGate::dynamics(&state).threshold_db, -60.0);
    gate.process_pair(&mut state, &mut [[0.0; BLOCK_SAMPLES]; 2]);
    assert_eq!(SimpleGate::dynamics(&state).threshold_db, -90.0);
    for v in [f32::from_bits(1), f32::MIN_POSITIVE, 0.25, 0.5, 0.75, 1.0] {
        gate.set_parameter(&mut state, 0, v);
        gate.process_pair(&mut state, &mut [[0.0; BLOCK_SAMPLES]; 2]);
        let ideal = -90.0 + 90.0 * f64::from(prm::operand(v)).powf(1.0 / 0.6);
        assert!((f64::from(SimpleGate::dynamics(&state).threshold_db) - ideal).abs() < 2e-5);
    }
}

#[test]
fn gated_burst_has_the_recovered_attack_hold_and_release_timing() {
    let gate = SimpleGate;
    let mut state = gate.static_layout().states()[0].clone();
    // Setup 0x529: k = tan((1/t)·(1/fs)), p = (1 − k)/(1 + k), t = ms·0.001f.
    let pole = |seconds: f32| {
        let x = prm::mul(
            math::reciprocal(seconds),
            math::reciprocal(SAMPLE_RATE as f32),
        );
        let k = math::tan(x);
        prm::mul(prm::sub(1.0, k), math::reciprocal(prm::add(1.0, k)))
    };
    assert_eq!(
        [ATTACK_SECONDS, RELEASE_SECONDS].map(f32::to_bits),
        [10.0, 40.0].map(|ms: f32| (ms * 0.001).to_bits())
    );
    let attack = prm::mul(1.5, prm::sub(1.0, pole(ATTACK_SECONDS)));
    let release = pole(RELEASE_SECONDS);
    assert!((f64::from(attack) - 0.006_237).abs() < 2e-7);
    assert!((f64::from(release) - 0.998_958_9).abs() < 2e-7);
    // f32 48000·0.010000001 = 480.00003, so ceilf gives 481 (not 480).
    assert_eq!(
        HOLD_SAMPLES,
        ((SAMPLE_RATE as f32 * ATTACK_SECONDS).ceil() as u32).max(32)
    );
    let open_threshold = math::db_to_linear(-60.0);
    let close_threshold = math::db_to_linear(-67.0);
    let (mut open, mut hold, mut gain) = (false, 0_u32, 0.0_f32);
    let (mut first_open, mut first_below_close, mut first_close) = (None, None, None);
    let mut counted_below = 0;
    for frame in 0..180 {
        let amplitude = if (4..8).contains(&frame) { 0.5 } else { 1e-5 };
        let mut pair = [[amplitude; BLOCK_SAMPLES]; 2];
        let previous = SimpleGate::dynamics(&state);
        gate.process_pair(&mut state, &mut pair);
        let envelopes = &state[state.len() - BLOCK_SAMPLES..];
        let mut e = previous.envelope;
        for (n, &word) in envelopes.iter().enumerate() {
            e = prm::add(
                e,
                prm::mul(previous.detector_coefficient, prm::sub(amplitude, e)),
            );
            assert_eq!(
                word,
                e.to_bits(),
                "coefficient must stay fixed within a block"
            );
            let absolute = frame * BLOCK_SAMPLES + n;
            if e < close_threshold {
                if first_open.is_some() && first_close.is_none() {
                    first_below_close.get_or_insert(absolute);
                    counted_below += 1;
                }
                hold = hold.saturating_sub(1);
            } else if e >= open_threshold {
                hold = HOLD_SAMPLES;
                open = true;
                first_open.get_or_insert(absolute);
            }
            if hold == 0 {
                if open {
                    first_close.get_or_insert(absolute);
                }
                open = false;
            }
            gain = if open {
                prm::add(gain, prm::mul(attack, prm::sub(1.0, gain))).min(1.0)
            } else {
                prm::mul(gain, release).max(0.0)
            };
            let expected = prm::mul(amplitude, gain).to_bits();
            assert_eq!(pair[0][n].to_bits(), expected);
            assert_eq!(pair[1][n].to_bits(), expected);
        }
        let after = SimpleGate::dynamics(&state);
        assert_eq!(
            (after.open, after.hold_remaining, after.gain.to_bits()),
            (open, hold, gain.to_bits())
        );
    }
    assert_eq!(first_open, Some(128));
    assert_eq!(first_below_close, Some(778));
    assert_eq!(first_close, Some(1258));
    assert_eq!(counted_below, HOLD_SAMPLES);
    assert_eq!(
        first_close.unwrap() - first_below_close.unwrap() + 1,
        HOLD_SAMPLES as usize
    );
    assert!(!SimpleGate::dynamics(&state).open);
    assert_eq!(
        SimpleGate::dynamics(&state).detector_coefficient,
        DETECTOR_FAST
    );
    assert!(
        gain > 0.0 && gain < 0.1,
        "40 ms pole produces a 20 ms exponential release"
    );
    println!(
        "gate burst: open={first_open:?}, hold begins={first_below_close:?}, close={first_close:?}"
    );
}

#[test]
fn hysteresis_pauses_the_hold_counter_and_slow_coefficient_is_delayed() {
    let gate = SimpleGate;
    let mut state = gate.static_layout().states()[0].clone();
    for _ in 0..40 {
        gate.process_pair(&mut state, &mut [[0.01; BLOCK_SAMPLES]; 2]);
    }
    assert_eq!(SimpleGate::dynamics(&state).hold_remaining, HOLD_SAMPLES);
    let between = 0.0007;
    for _ in 0..150 {
        gate.process_pair(&mut state, &mut [[between; BLOCK_SAMPLES]; 2]);
    }
    let latched = SimpleGate::dynamics(&state);
    assert!(latched.open);
    assert_eq!(latched.hold_remaining, HOLD_SAMPLES);
    gate.process_pair(&mut state, &mut [[0.0; BLOCK_SAMPLES]; 2]);
    // The first zero block is still above close. A later block crosses it.
    for _ in 0..40 {
        let before = SimpleGate::dynamics(&state);
        gate.process_pair(&mut state, &mut [[0.0; BLOCK_SAMPLES]; 2]);
        let after = SimpleGate::dynamics(&state);
        if after.detector_coefficient == DETECTOR_SLOW {
            assert_eq!(before.detector_coefficient, DETECTOR_FAST);
            assert!(after.hold_remaining > 0 && after.hold_remaining < HOLD_SAMPLES);
            gate.process_pair(&mut state, &mut [[1.0; BLOCK_SAMPLES]; 2]);
            let reopened = SimpleGate::dynamics(&state);
            assert_eq!(reopened.hold_remaining, HOLD_SAMPLES);
            assert_eq!(reopened.detector_coefficient, DETECTOR_SLOW);
            return;
        }
    }
    panic!("silence must cross the close threshold and start the hold countdown");
}

#[test]
fn release_flushes_to_exact_zero_under_the_prm_rule() {
    let gate = SimpleGate;
    let mut state = gate.static_layout().states()[0].clone();
    gate.process_pair(&mut state, &mut [[1.0; BLOCK_SAMPLES]; 2]);
    assert!(SimpleGate::dynamics(&state).gain > 0.0);
    for _ in 0..3_000 {
        gate.process_pair(&mut state, &mut [[0.0; BLOCK_SAMPLES]; 2]);
    }
    assert_eq!(SimpleGate::dynamics(&state).gain.to_bits(), 0);
    assert!(!SimpleGate::dynamics(&state).open);
}

/// Stock reset (`0x80` → `0x53`) only publishes the pending threshold, so a
/// live reset (EnableAll on a same-layout recall) keeps the gate open. Fault
/// recovery clears the dynamics and keeps the threshold.
#[test]
fn kernel_reset_keeps_the_dynamics_and_fault_recovery_clears_them() {
    let gate = SimpleGate;
    let initial = gate.static_layout().states()[0].clone();
    let mut state = initial.clone();
    gate.process_pair(&mut state, &mut [[0.5; BLOCK_SAMPLES]; 2]);
    gate.set_parameter(&mut state, 0, 0.25);
    let open = SimpleGate::dynamics(&state);
    gate.reset(&mut state);
    let after = SimpleGate::dynamics(&state);
    assert_eq!(
        (after.open, after.hold_remaining, after.gain, after.envelope),
        (open.open, open.hold_remaining, open.gain, open.envelope)
    );
    assert!(after.open && after.threshold_db < -80.0);
    gate.recover(&mut state);
    let mut expected = initial;
    gate.set_parameter(&mut expected, 0, 0.25);
    gate.reset(&mut expected);
    assert_eq!(state, expected);
}

fn gate_frames() -> Vec<VerificationFrame> {
    let mut frames = scheduled_frames(
        64,
        &[
            (0, 0, 0.0),
            (8, 0, 0.25),
            (20, 0, 0.5),
            (32, 0, 1.0),
            (40, 0, 0.75),
            (48, 1, 0.5),
            (49, 0, f32::from_bits(1)),
            (50, 0, f32::MIN_POSITIVE),
            (63, 0, 0.5),
        ],
    );
    frames[0].samples = [[1.0; BLOCK_SAMPLES]; 2];
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.0, -1.0, 1.5] {
        frames[30]
            .parameters
            .push(ParameterUpdate { index: 0, value });
    }
    frames[51].samples[0][0] = f32::from_bits(1);
    frames[51].samples[1][0] = f32::from_bits(0x8000_0001);
    for n in 0..180 {
        let level = if (4..8).contains(&n) { 0.5 } else { 1e-5 };
        frames.push(VerificationFrame {
            samples: [[level; BLOCK_SAMPLES]; 2],
            // A live kernel reset while the gate is open must keep it open.
            reset: n == 6,
            parameters: Vec::new(),
        });
    }
    frames.extend(std::iter::repeat_n(
        VerificationFrame {
            samples: [[0.0; BLOCK_SAMPLES]; 2],
            reset: false,
            parameters: Vec::new(),
        },
        3_000,
    ));
    frames
}

#[test]
fn v2_stereo_gate_matches_prm_bits_through_the_kernel_lifecycle() {
    let frames = gate_frames();
    let report = verify_module(&SimpleGate, &frames).unwrap();
    let built = BuiltModule::build(&SimpleGate).unwrap();
    assert_eq!(report.frames, 3_244);
    // Entry, start reset, 15 set-parameter, 1 live reset, 3,244 process,
    // reset R8 = 0, fault recovery, destroy.
    assert_eq!(report.calls, 3_265);
    assert_eq!(
        report.maximum_process_cycles,
        built.stats.cycles_per_stereo_block
    );
    assert_eq!(report.maximum_process_cycles, 1_429);
    println!(
        "Gate: {} maximum modeled cycles / stereo 32 frames; {} bound; {} DM, {} VISA, {} ISA bytes",
        report.maximum_process_cycles,
        built.stats.cycles_per_stereo_block,
        built.stats.dm_bytes,
        built.stats.visa_bytes,
        built.stats.isa_bytes,
    );
}

#[test]
fn v3_gate_relocation_matches_linking_and_two_instances_deploy() {
    let (built, placements) = support::deployment::verify_blob_path(&SimpleGate);
    assert_eq!(
        ModuleBlob::parse(&built.blob.to_bytes().unwrap()).unwrap(),
        built.blob
    );
    assert!(!built.stats.reuse_pmco);
    assert!(built.blob.relocation_words(Section::Isa).is_none());
    // One stereo state, as stock: no channel-B shadow in section 1.
    let state = SimpleGate.static_layout().states()[0].clone();
    let dm = built.blob.content(Section::Dm).unwrap();
    assert_eq!(built.layout.channel_b_offset_bytes, None);
    assert_eq!(dm.len(), (4 * state.len()).next_multiple_of(16));
    let words: Vec<u32> = dm
        .chunks_exact(4)
        .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
        .collect();
    assert_eq!(words[..state.len()], state);
    let frames = gate_frames();
    for at in placements {
        verify_module_at(&SimpleGate, &frames, at).unwrap();
    }
}

#[test]
fn v4_gate_shim_matches_the_prm_and_qc_asm_return_vectors() {
    let linked = LinkedModule::link(&SimpleGate, SectionAddresses::LINK).unwrap();
    support::verify_stub_vectors(&linked);
}

#[test]
#[ignore = "runs the blobdis binary that QC_BLOBDIS names (not in this repository)"]
fn blobdis_decodes_the_gate_shim() {
    support::blobdis::verify_shim_decode(&BuiltModule::build(&SimpleGate).unwrap());
}

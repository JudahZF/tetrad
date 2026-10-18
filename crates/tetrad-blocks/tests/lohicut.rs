//! Low-High Cut (4003): V1 laws, response, decay and glide on the twin, V2
//! kernel lifecycle on `Machine`, V3 blob path, V4 shim decode. V5 (one
//! changed law constant in the code) is the unit test in
//! `src/blocks/lohicut.rs`.
mod support;

use tetrad_blocks::blocks::lohicut::{
    CASCADE, LAW, LowHighCut, PROCESS_CYCLES, STATE_WORDS, Side, slope_order,
};
use tetrad_blocks::dsp::biquad::{Cascade, Section};
use tetrad_blocks::verification::{
    VerificationFrame, scheduled_frames, verify_module, verify_module_at,
};
use tetrad_blocks::{BLOCK_SAMPLES, BuiltModule, HostReference, OwnedBlock};
use tetrad_sharc::math::prm::Precision;

const HPF_SLOPE: u32 = 0;
const HPF_FREQ: u32 = 1;
const LPF_SLOPE: u32 = 2;
const LPF_FREQ: u32 = 3;
const OUTPUT: u32 = 4;
const RATE: f64 = 48_000.0;

/// One channel's twin state after the start reset and one silent block: a
/// reset only sets flags, and the next process call designs and settles
/// (review N1).
fn channel() -> Vec<u32> {
    let mut state = LowHighCut.static_layout().states()[0].clone();
    LowHighCut.reset(&mut state);
    run(&mut state, 1);
    state
}

fn run(state: &mut [u32], blocks: usize) {
    for _ in 0..blocks {
        LowHighCut.process(state, &mut [0.0; BLOCK_SAMPLES]);
    }
}

/// The ZenUI law P(v; 20, 20000, 0.3) in f64.
fn display_hz(v: f64) -> f64 {
    20.0 + 19_980.0 * v.powf(1.0 / 0.3)
}

/// FREQ within 0.01 cent of the f64 display law with exact ends; SLOPE is
/// floor(8v + 0.5), ties included (FIX alone rounds them to even); OUTPUT
/// within 1e-4 dB of 40v − 20 with exact unity at v = 0.5.
#[test]
fn parameter_laws_follow_the_stock_conversions() {
    for k in 1..=1000 {
        let v = k as f32 / 1000.0;
        let cents = 1200.0 * (f64::from(LAW.cutoff_hz(v)) / display_hz(f64::from(v))).log2();
        assert!(cents.abs() < 0.01, "v = {v}: {cents} cents");
    }
    assert_eq!([LAW.cutoff_hz(0.0), LAW.cutoff_hz(1.0)], [20.0, 20_000.0]);

    for k in 0..=8 {
        assert_eq!(slope_order(k as f32 / 8.0), k);
    }
    for (v, order) in [
        (1.0 / 16.0, 1),
        (5.0 / 16.0, 3),
        (9.0 / 16.0, 5),
        (15.0 / 16.0, 8),
    ] {
        assert_eq!(slope_order(v), order, "tie at v = {v}");
        assert_eq!(slope_order(f32::next_down(v)), order - 1);
    }

    for k in 0..=100 {
        let v = k as f32 / 100.0;
        let db = 20.0 * f64::from(LAW.output_gain(v)).log10();
        let error = db - (40.0 * f64::from(v) - 20.0);
        assert!(error.abs() < 1e-4, "v = {v}: {error} dB");
    }
    assert_eq!(LAW.output_gain(0.5), 1.0);
}

/// Bilinear Butterworth target: the analog |H(jΩ)| at the prewarped
/// frequency Ω = tan(πf/fs)/tan(πfc/fs), so −3.01 dB at fc exactly.
fn butterworth_db(side: Side, order: u32, cutoff: f64, frequency: f64) -> f64 {
    let warp = |f: f64| (std::f64::consts::PI * f / RATE).tan();
    let omega = warp(frequency) / warp(cutoff);
    let ratio = match side {
        Side::High => 1.0 / omega,
        Side::Low => omega,
    };
    -10.0 * (1.0 + ratio.powi(2 * order as i32)).log10()
}

fn response_db(sections: &[Section], frequency: f64) -> f64 {
    sections
        .iter()
        .map(|section| 20.0 * section.magnitude(frequency, RATE).log10())
        .sum()
}

/// Designed f32 sections (the twin's tan, sin and reciprocal) against the
/// analog Butterworth target at fc/4, fc/2, fc, 2fc and 4fc, down to −80 dB:
/// within 0.01 dB for cutoffs from 100 Hz (measured 3.1e-3 dB at 113 Hz),
/// within 0.1 dB below (measured 0.072 dB at 20 Hz, where the rounding of a2
/// still moves poles near z = 1). Flat is identity.
#[test]
fn designs_meet_the_analog_butterworth_target() {
    let mut worst = [0.0_f64; 2];
    for side in Side::ALL {
        for order in 1..=8 {
            for v in [0.0, 0.1, 0.2, 0.25, 0.4, 0.55, 0.7, 0.85, 0.95, 1.0] {
                let cutoff = f64::from(LAW.cutoff_hz(v));
                let sections = LAW.design(side, order, v);
                let used = order.div_ceil(2) as usize;
                assert!(sections[used..].iter().all(|s| *s == Section::IDENTITY));
                for scale in [0.25, 0.5, 1.0, 2.0, 4.0] {
                    let frequency = cutoff * scale;
                    let target = butterworth_db(side, order, cutoff, frequency);
                    if frequency >= RATE / 2.0 || target < -80.0 {
                        continue;
                    }
                    let error = (response_db(&sections, frequency) - target).abs();
                    let band = usize::from(cutoff >= 100.0);
                    worst[band] = worst[band].max(error);
                    let bound = [0.1, 0.01][band];
                    assert!(
                        error < bound,
                        "{side:?} n = {order}, fc = {cutoff} Hz, f = {frequency}: {error} dB"
                    );
                }
            }
        }
        assert_eq!(LAW.design(side, 0, 0.5), [Section::IDENTITY; 4]);
    }
    println!(
        "worst error: {:.2e} dB (fc 20 Hz), {:.2e} dB (fc ≥ 100 Hz)",
        worst[0], worst[1]
    );
}

/// Flat sides at 0 dB pass every normal sample bit for bit. A 48 dB/oct HPF
/// at 20 Hz (the slowest decay this block has) then rings out to exact zero:
/// every output and history word reaches ±0 under the PRM flush.
#[test]
fn flat_passes_exactly_and_a_cut_decays_to_exact_zero() {
    let mut state = channel();
    let input: [f32; BLOCK_SAMPLES] = std::array::from_fn(|n| (n as f32 * 0.61).sin() * 0.9);
    let mut samples = input;
    LowHighCut.process(&mut state, &mut samples);
    assert_eq!(samples.map(f32::to_bits), input.map(f32::to_bits));

    LowHighCut.set_parameter(&mut state, HPF_SLOPE, 1.0);
    LowHighCut.set_parameter(&mut state, LPF_SLOPE, 1.0);
    LowHighCut.reset(&mut state);
    let mut impulse = [0.0; BLOCK_SAMPLES];
    impulse[0] = 1.0;
    LowHighCut.process(&mut state, &mut impulse);
    let quiet = (1..20_000).find(|_| {
        let mut samples = [0.0; BLOCK_SAMPLES];
        LowHighCut.process(&mut state, &mut samples);
        samples.iter().all(|&s| s == 0.0)
            && (0..CASCADE.words()).all(|w| state[w] & 0x7fff_ffff == 0 || is_coefficient(w))
    });
    let blocks = quiet.expect("the decay reaches exact zero");
    println!("exact zero after {blocks} blocks");
}

/// Live and target coefficient words (12-word records: history, live, target).
fn is_coefficient(word: usize) -> bool {
    word < CASCADE.words() - 2 && word % 12 >= 2
}

/// The stock RND32 bracket (40-bit section loop) cuts the round-off of the
/// slowest design, 48 dB/oct HPF at 20 Hz, by more than 12 dB (measured
/// 16.9 dB: −68.2 → −85.1 dB). Not 48 dB: every store keeps 24 bits, and
/// the y history is truncated at each block end.
#[test]
fn rnd32_bracket_cuts_the_round_off_of_the_low_sections() {
    let mut state = channel();
    LowHighCut.set_parameter(&mut state, HPF_SLOPE, 1.0);
    LowHighCut.set_parameter(&mut state, HPF_FREQ, 0.0);
    LowHighCut.reset(&mut state);
    run(&mut state, 1);
    let single = Cascade {
        precision: Precision::Single,
        ..CASCADE
    };
    let (before, after) = (
        support::round_off_db(&single, &state),
        support::round_off_db(&CASCADE, &state),
    );
    println!("round-off: RND32 = 1 {before:.1} dB, bracket {after:.1} dB");
    assert!(after < -80.0 && before - after > 12.0);
}

/// A FREQ and SLOPE change mid-stream: the FREQ ramp lands after 30 blocks
/// (targets stop moving and equal the direct design at the new value), the
/// live coefficients then land exactly, and every live section on the way
/// stays inside the stability triangle |a2| < 1, |a1| < 1 + a2.
#[test]
fn glide_is_stable_and_lands_exactly() {
    let mut state = channel();
    for (index, value) in [
        (HPF_SLOPE, 0.5),
        (HPF_FREQ, 0.2),
        (LPF_SLOPE, 0.375),
        (LPF_FREQ, 0.9),
    ] {
        LowHighCut.set_parameter(&mut state, index, value);
    }
    LowHighCut.reset(&mut state);
    run(&mut state, 1);
    LowHighCut.set_parameter(&mut state, HPF_SLOPE, 1.0);
    LowHighCut.set_parameter(&mut state, HPF_FREQ, 0.7);
    LowHighCut.set_parameter(&mut state, LPF_FREQ, 0.6);
    let targets = |state: &[u32]| {
        (0..8)
            .map(|k| Cascade::target(state, k))
            .collect::<Vec<_>>()
    };
    let lives = |state: &[u32]| (0..8).map(|k| Cascade::live(state, k)).collect::<Vec<_>>();
    let mut landed = None;
    for block in 1..=200 {
        let before = targets(&state);
        run(&mut state, 1);
        for Section([.., a1, a2]) in lives(&state) {
            assert!(a2.abs() < 1.0 && a1.abs() < 1.0 + a2, "block {block}");
        }
        if before == targets(&state) && block > 1 && landed.is_none() {
            landed = Some(block - 1);
        }
        if lives(&state) == targets(&state) {
            assert_eq!(landed, Some(30), "the FREQ ramp lasts 30 blocks");
            let expected = [
                LAW.design(Side::High, 8, 0.7),
                LAW.design(Side::Low, 3, 0.6),
            ]
            .concat();
            assert_eq!(targets(&state), expected);
            println!("live = target after {block} blocks");
            return;
        }
    }
    panic!("the glide did not land");
}

/// V2 input: the ZenUI push, mid-ramp retargets, every order with its
/// first-order and identity sections, SLOPE ties, the FREQ and OUTPUT ends,
/// subnormal values, an ignored index, values the adapter drops, a resent
/// settled value (lands at once), and subnormal and −0 samples. Frame 20 drives the bound's path: both sides
/// ramp and redesign at order 8. Returns the frames and the update count.
fn lifecycle_frames() -> (Vec<VerificationFrame>, usize) {
    let mut updates = vec![
        (0, HPF_SLOPE, 0.25),
        (0, HPF_FREQ, 0.35),
        (0, LPF_SLOPE, 0.875),
        (0, LPF_FREQ, 0.8),
        (0, OUTPUT, 0.6),
        (5, HPF_FREQ, 0.6),
        (12, HPF_SLOPE, 0.125),
        (20, HPF_SLOPE, 1.0),
        (20, HPF_FREQ, 0.0),
        (20, LPF_SLOPE, 1.0),
        (20, LPF_FREQ, 1.0),
        (40, OUTPUT, 0.0),
        (44, OUTPUT, 1.0),
        (50, HPF_SLOPE, 1.0 / 16.0),
        (50, LPF_SLOPE, 9.0 / 16.0),
        (52, HPF_SLOPE, 0.375),
        (52, LPF_SLOPE, 0.625),
        (60, LPF_FREQ, f32::from_bits(1)),
        (60, HPF_FREQ, f32::MIN_POSITIVE),
        (64, 5, 0.5),
        (70, HPF_SLOPE, 0.0),
        (70, LPF_SLOPE, 0.0),
        (90, HPF_SLOPE, 0.75),
        (90, HPF_FREQ, 0.15),
        (90, LPF_SLOPE, 0.5),
        (90, LPF_FREQ, 0.55),
        (100, OUTPUT, 0.5),
        (120, LPF_FREQ, 0.55),
    ];
    for value in [f32::NAN, f32::INFINITY, -0.0, 1.5, 1.0_f32.next_up()] {
        updates.push((64, HPF_FREQ, value));
    }
    updates.extend((0..=20).map(|k| (80, LPF_FREQ, k as f32 / 20.0)));
    updates.sort_by_key(|&(frame, ..)| frame);
    let mut frames = scheduled_frames(128, &updates);
    frames[34].samples[1][31] = f32::from_bits(0x8000_0001);
    frames[34].samples[0][6] = -0.0;
    // A live reset (R8 = 1) on a sine frame keeps the history (review F6).
    frames[67].reset = true;
    (frames, updates.len())
}

/// Review F6: a live kernel reset (R8 = 1, `0x1f` EnableAll on a same-layout
/// recall) after the ramps landed keeps the filter history, so the audio
/// equals the audio without the reset (no step transient). Fault recovery
/// (R8 = 0) clears it.
#[test]
fn live_reset_keeps_the_history_and_recovery_clears_it() {
    let sine = |block: usize| -> [f32; BLOCK_SAMPLES] {
        std::array::from_fn(|n| ((block * BLOCK_SAMPLES + n) as f32 * 0.05).sin() * 0.5)
    };
    let mut state = channel();
    for (index, value) in [(HPF_SLOPE, 0.5), (HPF_FREQ, 0.4), (LPF_SLOPE, 0.5)] {
        LowHighCut.set_parameter(&mut state, index, value);
    }
    for block in 0..64 {
        LowHighCut.process(&mut state, &mut sine(block));
    }
    let run = |reset: fn(&LowHighCut, &mut [u32])| {
        let mut state = state.clone();
        reset(&LowHighCut, &mut state);
        let mut samples = sine(64);
        LowHighCut.process(&mut state, &mut samples);
        samples
    };
    let plain = run(|_, _| {});
    assert_eq!(run(LowHighCut::reset), plain);
    assert_ne!(run(LowHighCut::recover), plain);
}

/// V2: the whole kernel lifecycle on `Machine` (PRM rules) equals the twin
/// bit for bit after every call, within the stats cycle bound.
#[test]
fn lohicut_matches_its_twin_through_the_kernel_lifecycle() {
    let (frames, updates) = lifecycle_frames();
    let report = verify_module(&LowHighCut, &frames).unwrap();
    // Entry, start reset, one set-parameter call per update, one live
    // reset, 128 process, reset R8 = 0, fault recovery, destroy.
    assert_eq!((report.frames, report.calls), (128, 6 + updates + 128));
    let bound = BuiltModule::build(&LowHighCut)
        .unwrap()
        .stats
        .cycles_per_stereo_block;
    assert!(report.maximum_process_cycles <= bound);
    println!(
        "process: maximum {} / bound {bound} modeled steps per stereo block (mono bound {PROCESS_CYCLES})",
        report.maximum_process_cycles
    );
}

/// V3: relocation to each `place()` result equals the module linked there,
/// two instances deploy through type 7 and `0x1c`, and V2 passes at both
/// placements, so the relocated bytes are the ones verified. Section 1 holds
/// both initial channel states.
#[test]
fn each_placement_runs_its_relocated_bytes_through_v2() {
    let (built, placements) = support::deployment::verify_blob_path(&LowHighCut);
    let (frames, _) = lifecycle_frames();
    for at in placements {
        verify_module_at(&LowHighCut, &frames, at).unwrap();
    }
    assert!(!built.stats.reuse_pmco);
    assert_eq!(built.layout.channel_state_words, STATE_WORDS as u32);
    let dm = built.blob.content(qc_format::blob::Section::Dm).unwrap();
    let words: Vec<u32> = dm
        .chunks_exact(4)
        .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
        .collect();
    assert_eq!(
        words[..2 * STATE_WORDS],
        LowHighCut.static_layout().states().concat()
    );
}

/// V4: the independent `blobdis` decoder reads the whole shim as the ABI
/// record's entry and stubs.
#[test]
#[ignore = "runs the blobdis binary that QC_BLOBDIS names (not in this repository)"]
fn blobdis_decodes_the_shim() {
    support::blobdis::verify_shim_decode(&BuiltModule::build(&LowHighCut).unwrap());
}

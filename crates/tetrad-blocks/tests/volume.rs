//! Volume (16006): V1 law and ramp on the twin, V2 kernel lifecycle on
//! `Machine`, V3 blob path. V5 (one changed immediate per law part) is the unit
//! test in `src/blocks/volume.rs`, next to the private law constants.
mod support;

use qc_format::blob::Section;
use tetrad_blocks::blocks::volume::{PROCESS_CYCLES, Volume, Word};
use tetrad_blocks::verification::{
    VerificationFrame, scheduled_frames, verify_module, verify_module_at,
};
use tetrad_blocks::{BLOCK_SAMPLES, BuiltModule, HostReference, OwnedBlock};

const LEVEL: u32 = 0;
const CURVE: u32 = 1;

/// One channel's twin state after the start reset.
fn channel() -> Vec<u32> {
    let mut state = Volume.static_layout().states()[0].clone();
    Volume.reset(&mut state);
    state
}

fn word(state: &[u32], name: Word) -> f32 {
    f32::from_bits(state[name as usize])
}

/// The stock Log law in f64: dB interpolated over the breakpoints, 10^(dB/20).
fn log_law(v: f64) -> f64 {
    const POINTS: [(f64, f64); 4] = [(0.0, -120.0), (0.12, -52.8), (0.25, -30.0), (1.0, 0.0)];
    if v == 0.0 {
        return 0.0;
    }
    let k = POINTS.windows(2).position(|w| v <= w[1].0).unwrap();
    let ((x0, y0), (x1, y1)) = (POINTS[k], POINTS[k + 1]);
    10_f64.powf((y0 + (v - x0) * (y1 - y0) / (x1 - x0)) / 20.0)
}

/// Log targets stay within 1e-4 dB of the f64 law (H3 allows 0.01 dB); v = 0
/// gives 0 and v = 1 exact unity. Linear targets are v bit for bit, and
/// CURVE = 0.5 exactly is Linear.
#[test]
fn targets_follow_the_stock_curves() {
    let mut state = channel();
    Volume.set_parameter(&mut state, CURVE, 1.0);
    for step in 0..=1000 {
        let v = step as f32 / 1000.0;
        Volume.set_parameter(&mut state, LEVEL, v);
        let (target, law) = (word(&state, Word::Target), log_law(f64::from(v)));
        if v == 0.0 {
            assert_eq!(target.to_bits(), 0);
        } else {
            let error = 20.0 * (f64::from(target) / law).log10();
            assert!(error.abs() < 1e-4, "v = {v}: {error} dB");
        }
    }
    assert_eq!(word(&state, Word::Target), 1.0);
    Volume.set_parameter(&mut state, CURVE, 0.5);
    for v in [0.3, 0.0, 1.0] {
        Volume.set_parameter(&mut state, LEVEL, v);
        assert_eq!(word(&state, Word::Target).to_bits(), v.to_bits());
    }
}

/// The first LEVEL push ramps 0 → 1 in 30 bounded blocks (20 ms at 48 kHz),
/// then snaps exactly onto the target; from there every output word equals
/// its input word (the offline form of the H2 unity null). A CURVE change that
/// leaves the target equal keeps the ramp and its bound.
#[test]
fn ramp_is_bounded_and_ends_exactly_on_target() {
    let mut state = channel();
    Volume.set_parameter(&mut state, LEVEL, 1.0);
    assert_eq!(word(&state, Word::Bound), 1.0 / 30.0);
    let input: [f32; BLOCK_SAMPLES] = std::array::from_fn(|n| (n as f32 * 0.37).sin());
    let mut previous = -1.0;
    for block in 0.. {
        let mut samples = input;
        Volume.process(&mut state, &mut samples);
        let gain = word(&state, Word::Gain);
        if block == 5 {
            let before = state.clone();
            Volume.set_parameter(&mut state, CURVE, 1.0);
            assert_eq!(state[..1], before[..1]);
            assert_eq!(state[2..], before[2..]);
        }
        if word(&state, Word::Step) == 0.0 {
            assert_eq!((block, gain), (30, 1.0));
            assert_eq!(samples.map(f32::to_bits), input.map(f32::to_bits));
            break;
        }
        assert!(gain > previous && gain < 1.0, "block {block}: {gain}");
        previous = gain;
    }
}

/// Lifecycle schedule over 128 frames: ramp-in, a CURVE toggle with an
/// unchanged target, retargets mid-ramp, each Log segment and both breakpoints,
/// Log zero, a subnormal LEVEL (reads as zero), CURVE at exactly 0.5, an
/// ignored index, values the adapter drops, a dense Log sweep, subnormal and
/// −0 samples.
fn lifecycle_frames() -> Vec<VerificationFrame> {
    let mut updates = vec![
        (0, LEVEL, 1.0),
        (3, 2, 0.3),
        (10, CURVE, 1.0),
        (40, LEVEL, 0.5),
        (45, LEVEL, 0.1),
        (60, LEVEL, 0.12),
        (61, LEVEL, 0.2),
        (62, LEVEL, 0.25),
        (70, LEVEL, 0.0),
        (72, LEVEL, f32::from_bits(1)),
        (74, LEVEL, f32::MIN_POSITIVE),
        (76, LEVEL, 0.75),
        (80, CURVE, 0.5),
        (82, CURVE, 0.5_f32.next_up()),
        (100, CURVE, 0.0),
        (100, LEVEL, 0.6),
    ];
    for value in [f32::NAN, f32::INFINITY, -0.0, 1.5, 1.0_f32.next_up()] {
        updates.push((3, LEVEL, value));
    }
    updates.extend((0..=100).map(|k| (90, LEVEL, k as f32 / 100.0)));
    updates.sort_by_key(|&(frame, ..)| frame);
    let mut frames = scheduled_frames(128, &updates);
    frames[34].samples[1][31] = f32::from_bits(0x8000_0001);
    frames[34].samples[0][6] = -0.0;
    frames
}

/// V2: every call on `Machine` (PRM rules) equals the twin bit for bit. The
/// steady state runs the snap path, the longest one, so the measured maximum
/// equals the stats bound.
#[test]
fn volume_matches_its_twin_through_the_kernel_lifecycle() {
    let report = verify_module(&Volume, &lifecycle_frames()).unwrap();
    // Entry, start reset, 122 set-parameter, 128 process, reset R8 = 0,
    // fault recovery, destroy.
    assert_eq!((report.frames, report.calls), (128, 255));
    let stats = BuiltModule::build(&Volume).unwrap().stats;
    assert_eq!(report.maximum_process_cycles, stats.cycles_per_stereo_block);
    assert!(stats.cycles_per_stereo_block > 2 * PROCESS_CYCLES);
}

/// V3: relocation to each `place()` result equals the module linked there,
/// two instances deploy through type 7 and `0x1c`, and V2 passes at both
/// placements, so the relocated bytes are the ones verified. Section 1 is
/// the stock code init for both channels.
#[test]
fn each_placement_runs_its_relocated_bytes_through_v2() {
    let (built, placements) = support::deployment::verify_blob_path(&Volume);
    let frames = lifecycle_frames();
    for at in placements {
        verify_module_at(&Volume, &frames, at).unwrap();
    }
    assert!(!built.stats.reuse_pmco);
    let dm = built.blob.content(Section::Dm).unwrap();
    let words: Vec<u32> = dm
        .chunks_exact(4)
        .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
        .collect();
    assert_eq!(words, Volume.static_layout().states().concat());
    assert_eq!(words[Word::Level as usize], 1.0_f32.to_bits());
}

/// V4: the independent `blobdis` decoder reads the whole shim as the ABI
/// record's entry and stubs.
#[test]
#[ignore = "runs the blobdis binary that QC_BLOBDIS names (not in this repository)"]
fn blobdis_decodes_the_shim() {
    support::blobdis::verify_shim_decode(&BuiltModule::build(&Volume).unwrap());
}

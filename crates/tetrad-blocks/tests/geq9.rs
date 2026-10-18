//! Graphic-9 (4002): V1 laws, response and instant changes on the twin, V2
//! kernel lifecycle on `Machine`, V3 blob path, V4 shim decode. V5 (one
//! changed law constant in the code) is the unit test in
//! `src/blocks/geq9.rs`.
mod support;

use std::f64::consts::TAU;

use tetrad_blocks::blocks::geq9::{
    BANDS, Band, CASCADE, Graphic9, LAW, PROCESS_CYCLES, STATE_WORDS,
};
use tetrad_blocks::dsp::biquad::{Cascade, Section};
use tetrad_blocks::verification::{
    VerificationFrame, scheduled_frames, verify_module, verify_module_at,
};
use tetrad_blocks::{BLOCK_SAMPLES, BuiltModule, HostReference, OwnedBlock};
use tetrad_sharc::math::prm::Precision;

const HPF: u32 = 0;
const OUTPUT: u32 = 10;
const RATE: f64 = 48_000.0;

/// One channel's twin state after the start reset.
fn channel() -> Vec<u32> {
    let mut state = Graphic9.static_layout().states()[0].clone();
    Graphic9.reset(&mut state);
    state
}

/// HPF f = 19 + 231v with OFF exactly below 20 Hz (v < 1/231); band gain
/// and OUTPUT within 1e-4 dB of 24v − 12 and 40v − 20, unity at v = 0.5.
#[test]
fn parameter_laws_follow_the_stock_records() {
    for k in 0..=231 {
        let v = k as f32 / 231.0;
        assert!((f64::from(LAW.hpf_hz(v)) - (19.0 + f64::from(k))).abs() < 1e-4);
        assert_eq!(LAW.design(0, v) == Section::IDENTITY, k == 0, "v = {v}");
    }
    let mut v = 1.0_f32 / 231.0;
    for _ in 0..8 {
        v = v.next_down();
        let off = LAW.design(0, v) == Section::IDENTITY;
        assert_eq!(off, LAW.hpf_hz(v) < 20.0, "v = {v}");
    }

    for k in 0..=100 {
        let v = k as f32 / 100.0;
        for (gain, span) in [(LAW.band_gain(v), 12.0), (LAW.output_gain(v), 20.0)] {
            let error = 20.0 * f64::from(gain).log10() - span * (2.0 * f64::from(v) - 1.0);
            assert!(error.abs() < 1e-4, "v = {v}: {error} dB");
        }
    }
    assert_eq!([LAW.band_gain(0.5), LAW.output_gain(0.5)], [1.0, 1.0]);
}

/// RBJ cookbook coefficients `[b0, b1, b2, a1, a2]` in f64 (Q 1, fs 48 kHz).
fn rbj(band: Band, value: f64) -> [f64; 5] {
    let db = 24.0 * value - 12.0;
    let a = 10f64.powf(db / 40.0);
    let normalize =
        |[b0, b1, b2, a0, a1, a2]: [f64; 6]| [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0];
    let trig = |f0: f64| {
        let w = TAU * f0 / RATE;
        (w.sin(), w.cos())
    };
    match band {
        Band::HighPass => {
            let (s, c) = trig(19.0 + 231.0 * value);
            let alpha = s / 2.0;
            normalize([
                (1.0 + c) / 2.0,
                -(1.0 + c),
                (1.0 + c) / 2.0,
                1.0 + alpha,
                -2.0 * c,
                1.0 - alpha,
            ])
        }
        Band::Peak(f0) => {
            let (s, c) = trig(f0);
            let alpha = s / 2.0;
            normalize([
                1.0 + alpha * a,
                -2.0 * c,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * c,
                1.0 - alpha / a,
            ])
        }
        Band::LowShelf(f0) => {
            let (s, c) = trig(f0);
            let beta = a.sqrt() * s;
            normalize([
                a * ((a + 1.0) - (a - 1.0) * c + beta),
                2.0 * a * ((a - 1.0) - (a + 1.0) * c),
                a * ((a + 1.0) - (a - 1.0) * c - beta),
                (a + 1.0) + (a - 1.0) * c + beta,
                -2.0 * ((a - 1.0) + (a + 1.0) * c),
                (a + 1.0) + (a - 1.0) * c - beta,
            ])
        }
        Band::HighShelf(f0) => {
            let (s, c) = trig(f0);
            let beta = a.sqrt() * s;
            normalize([
                a * ((a + 1.0) + (a - 1.0) * c + beta),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
                a * ((a + 1.0) + (a - 1.0) * c - beta),
                (a + 1.0) - (a - 1.0) * c + beta,
                2.0 * ((a - 1.0) - (a + 1.0) * c),
                (a + 1.0) - (a - 1.0) * c - beta,
            ])
        }
    }
}

fn magnitude_db([b0, b1, b2, a1, a2]: [f64; 5], frequency: f64) -> f64 {
    let w = TAU * frequency / RATE;
    let modulus = |c0: f64, c1: f64, c2: f64| {
        (c0 + c1 * w.cos() + c2 * (2.0 * w).cos()).hypot(c1 * w.sin() + c2 * (2.0 * w).sin())
    };
    20.0 * (modulus(b0, b1, b2) / modulus(1.0, a1, a2)).log10()
}

/// Designed f32 sections (twin leaves, f32 constants) against the f64 RBJ
/// curves, 10 Hz to 20 kHz (120 log-spaced points): the HPF at every
/// integer Hz, each band at every 0.5 dB step. Bounds: peaks and the high
/// shelf 0.002 dB (measured ≤ 1.1e-3, at 125 Hz), low shelf 0.01 dB (7.3e-3),
/// HPF 0.05 dB (2.6e-2). The low bands hit the f32 coefficient quantum: near
/// 20 Hz 1 + a1 + a2 ≈ 8e-6, and one rounding of a2 moves it by up to 0.4 %
/// (below the cutoff; Low-High Cut has the same limit). OFF and 0 dB are exact
/// identity sections.
#[test]
fn designs_match_the_rbj_curves() {
    let frequencies: Vec<f64> = (0..120)
        .map(|n| 10.0 * 2000f64.powf(f64::from(n) / 119.0))
        .collect();
    let mut worst = [0.0_f64; BANDS.len()];
    for (k, band) in BANDS.into_iter().enumerate() {
        let values: Vec<f32> = match band {
            Band::HighPass => (1..=231).map(|n| n as f32 / 231.0).collect(),
            _ => (0..=48).map(|n| n as f32 / 48.0).collect(),
        };
        for v in values {
            let designed = LAW.design(k, v).0.map(f64::from);
            let exact = rbj(band, f64::from(v));
            for &frequency in &frequencies {
                let error =
                    (magnitude_db(designed, frequency) - magnitude_db(exact, frequency)).abs();
                worst[k] = worst[k].max(error);
            }
        }
        let bound = match band {
            Band::HighPass => 0.05,
            Band::LowShelf(_) => 0.01,
            _ => 0.002,
        };
        assert!(worst[k] < bound, "{band:?}: {} dB", worst[k]);
        if band != Band::HighPass {
            assert_eq!(LAW.design(k, 0.5), Section::IDENTITY);
        }
    }
    assert_eq!(LAW.design(0, 0.0), Section::IDENTITY);
    let named: Vec<String> = BANDS
        .iter()
        .zip(worst)
        .map(|(band, error)| format!("{band:?} {error:.1e}"))
        .collect();
    println!("worst |dB error| per band: {}", named.join(", "));
}

/// The defaults pass every normal sample bit for bit. A change lands at the
/// next block: before any sample, live = target = the direct design, and
/// OUTPUT scales that whole block.
#[test]
fn defaults_pass_exactly_and_changes_are_instant() {
    let mut state = channel();
    let input: [f32; BLOCK_SAMPLES] = std::array::from_fn(|n| (n as f32 * 0.61).sin() * 0.9);
    let mut samples = input;
    Graphic9.process(&mut state, &mut samples);
    assert_eq!(samples.map(f32::to_bits), input.map(f32::to_bits));

    for (index, value) in [(HPF, 0.3), (1, 0.9), (5, 0.1), (9, 0.75), (OUTPUT, 0.75)] {
        Graphic9.set_parameter(&mut state, index, value);
    }
    let before = state.clone();
    let mut samples = input;
    Graphic9.process(&mut state, &mut samples);
    for (k, value) in [(0, 0.3), (1, 0.9), (5, 0.1), (9, 0.75)] {
        let design = LAW.design(k, value);
        assert_eq!(Cascade::live(&state, k), design);
        assert_eq!(Cascade::target(&state, k), design);
    }
    assert_eq!(Cascade::live(&state, 2), Section::IDENTITY);

    let mut unscaled = before;
    for (k, value) in [(0, 0.3), (1, 0.9), (5, 0.1), (9, 0.75)] {
        Cascade::set_target(&mut unscaled, k, LAW.design(k, value));
    }
    let mut expected = input;
    CASCADE.process(&mut unscaled, &mut expected);
    let gain = LAW.output_gain(0.75);
    assert_eq!(
        samples,
        expected.map(|x| tetrad_sharc::math::prm::mul(x, gain))
    );
}

/// The stock RND32 bracket (40-bit section loop) cuts the round-off of the
/// low sections, HPF at 20 Hz and the 65 Hz shelf at +12 dB, by more than
/// 15 dB (measured 19.9 dB: −51.4 → −71.3 dB).
#[test]
fn rnd32_bracket_cuts_the_round_off_of_the_low_sections() {
    let mut state = channel();
    for (k, value) in [(0, 1.0 / 231.0), (1, 1.0)] {
        Cascade::set_target(&mut state, k, LAW.design(k, value));
    }
    CASCADE.settle(&mut state);
    let single = Cascade {
        precision: Precision::Single,
        ..CASCADE
    };
    let (before, after) = (
        support::round_off_db(&single, &state),
        support::round_off_db(&CASCADE, &state),
    );
    println!("round-off: RND32 = 1 {before:.1} dB, bracket {after:.1} dB");
    assert!(after < -65.0 && before - after > 15.0);
}

/// RBJ shelves keep their stock symmetry: the 65 Hz shelf at +12 dB reaches
/// +12 dB at DC, the 16 kHz shelf at Nyquist, each within 0.01 dB.
#[test]
fn shelves_reach_their_gain_at_the_band_edge() {
    let low = LAW.design(1, 1.0);
    let high = LAW.design(9, 1.0);
    assert!((20.0 * low.magnitude(0.0, RATE).log10() - 12.0).abs() < 0.01);
    assert!((20.0 * high.magnitude(RATE / 2.0, RATE).log10() - 12.0).abs() < 0.01);
    assert!((20.0 * high.magnitude(0.0, RATE).log10()).abs() < 0.01);
}

/// V2 input: the ZenUI push of all 11 values, both band ends, the HPF OFF
/// edge, a resent value, mid-stream changes, subnormal values, an ignored
/// index, values the adapter drops, and subnormal and −0 samples. Frame 20
/// redesigns every band (the bound's path). Returns the frames and the
/// update count.
fn lifecycle_frames() -> (Vec<VerificationFrame>, usize) {
    let mut updates: Vec<(usize, u32, f32)> = (0..=10)
        .map(|index| (0, index, 0.5 + 0.04 * index as f32))
        .collect();
    updates.extend((0..=10).map(|index| (20, index, if index % 2 == 0 { 1.0 } else { 0.0 })));
    updates.extend([
        (6, HPF, 1.0 / 231.0),
        (8, HPF, (1.0_f32 / 231.0).next_down()),
        (10, 3, 0.5),
        (12, 3, 0.5),
        (30, OUTPUT, 0.0),
        (34, OUTPUT, 1.0),
        (40, 9, f32::from_bits(1)),
        (40, HPF, f32::MIN_POSITIVE),
        (44, 11, 0.5),
        (50, HPF, 0.45),
        (50, 1, 0.2),
        (50, 9, 0.8),
        (60, 4, 0.0),
        (70, OUTPUT, 0.5),
        (90, 2, 0.95),
    ]);
    for value in [f32::NAN, f32::INFINITY, -0.0, 1.5, 1.0_f32.next_up()] {
        updates.push((64, 5, value));
    }
    updates.extend((0..=24).map(|n| (80, 7, n as f32 / 24.0)));
    updates.sort_by_key(|&(frame, ..)| frame);
    let mut frames = scheduled_frames(128, &updates);
    frames[34].samples[1][31] = f32::from_bits(0x8000_0001);
    frames[34].samples[0][6] = -0.0;
    // A live reset (R8 = 1) on a sine frame keeps the history (review F6).
    frames[67].reset = true;
    (frames, updates.len())
}

/// V2: the whole kernel lifecycle on `Machine` (PRM rules) equals the twin
/// bit for bit after every call, within the stats cycle bound.
#[test]
fn geq9_matches_its_twin_through_the_kernel_lifecycle() {
    let (frames, updates) = lifecycle_frames();
    let report = verify_module(&Graphic9, &frames).unwrap();
    // Entry, start reset, one set-parameter call per update, one live
    // reset, 128 process, reset R8 = 0, fault recovery, destroy.
    assert_eq!((report.frames, report.calls), (128, 6 + updates + 128));
    let built = BuiltModule::build(&Graphic9).unwrap();
    let bound = built.stats.cycles_per_stereo_block;
    assert!(report.maximum_process_cycles <= bound);
    println!(
        "process: maximum {} / bound {bound} modeled steps per stereo block (mono bound {PROCESS_CYCLES}); \
         cascade {}; DM / VISA / ISA {} / {} / {} bytes",
        report.maximum_process_cycles,
        CASCADE.process_cycles(),
        built.stats.dm_bytes,
        built.stats.visa_bytes,
        built.stats.isa_bytes,
    );
}

/// V3: relocation to each `place()` result equals the module linked there,
/// two instances deploy through type 7 and `0x1c`, and V2 passes at both
/// placements, so the relocated bytes are the ones verified. Section 1 holds
/// both initial channel states.
#[test]
fn each_placement_runs_its_relocated_bytes_through_v2() {
    let (built, placements) = support::deployment::verify_blob_path(&Graphic9);
    let (frames, _) = lifecycle_frames();
    for at in placements {
        verify_module_at(&Graphic9, &frames, at).unwrap();
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
        Graphic9.static_layout().states().concat()
    );
}

/// V4: the independent `blobdis` decoder reads the whole shim as the ABI
/// record's entry and stubs.
#[test]
#[ignore = "runs the blobdis binary that QC_BLOBDIS names (not in this repository)"]
fn blobdis_decodes_the_shim() {
    support::blobdis::verify_shim_decode(&BuiltModule::build(&Graphic9).unwrap());
}

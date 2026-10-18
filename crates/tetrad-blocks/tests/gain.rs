//! Gain V1–V4: recovered laws, whole callbacks, relocation and shim. V5 (one
//! changed law immediate per constant) is beside the constants in `blocks/gain.rs`.
mod support;

use qc_format::blob::{ModuleBlob, Section};
use tetrad_blocks::blocks::gain::{Gain, LEVEL_SKEW, STOCK_MODEL_ID};
use tetrad_blocks::verification::{ParameterUpdate, standard_frames, verify_module};
use tetrad_blocks::{
    BLOCK_SAMPLES, BuiltModule, HostReference, LinkedModule, OwnedBlock, ParameterLaw,
    SectionAddresses,
};
use tetrad_dsp_host::blob::{place, relocate};
use tetrad_sharc::math::prm;

#[test]
fn v1_level_matches_the_skewed_db_law_and_minimum_is_mute() {
    assert_eq!(Gain.model_id(), STOCK_MODEL_ID);
    assert_eq!(Gain.parameters().len(), 3);
    assert_eq!(Gain.parameters()[2].law, ParameterLaw::Switch);
    for step in 0..=128 {
        let value = step as f32 / 128.0;
        let mut state = Gain.static_layout().states()[0].clone();
        Gain.set_parameter(&mut state, 0, value);
        Gain.reset(&mut state);
        let mut samples = [1.0; BLOCK_SAMPLES];
        Gain.process(&mut state, &mut samples);
        if step == 0 {
            assert_eq!(samples.map(f32::to_bits), [0; BLOCK_SAMPLES]);
        } else {
            let db = -60.0 + 72.0 * f64::from(value).powf(1.0 / f64::from(LEVEL_SKEW));
            let expected = 10.0_f64.powf(db / 20.0);
            let relative = (f64::from(samples[0]) / expected - 1.0).abs();
            assert!(relative < 2e-5, "LEVEL={value}: error={relative}");
            assert!(
                samples
                    .iter()
                    .all(|sample| sample.to_bits() == samples[0].to_bits())
            );
        }
    }
    let mut state = Gain.static_layout().states()[0].clone();
    Gain.set_parameter(&mut state, 0, f32::from_bits(1));
    Gain.reset(&mut state);
    let mut samples = [1.0; BLOCK_SAMPLES];
    Gain.process(&mut state, &mut samples);
    assert_eq!(samples.map(f32::to_bits), [0; BLOCK_SAMPLES]);
}

#[test]
fn v1_balance_has_unity_at_centre_and_invert_changes_only_sign() {
    for (pan, gains) in [
        (0.0, [1.0, 0.0]),
        (0.25, [1.0, 0.5]),
        (0.5, [1.0, 1.0]),
        (0.75, [0.5, 1.0]),
        (1.0, [0.0, 1.0]),
    ] {
        for (channel, gain) in gains.into_iter().enumerate() {
            let mut state = Gain.static_layout().states()[channel].clone();
            Gain.set_parameter(&mut state, 1, pan);
            Gain.reset(&mut state);
            let mut samples = [1.0; BLOCK_SAMPLES];
            Gain.process(&mut state, &mut samples);
            assert_eq!(
                samples.map(f32::to_bits),
                [f32::to_bits(gain); BLOCK_SAMPLES]
            );
        }
    }
    let input = std::array::from_fn(|index| {
        [
            0.0,
            -0.0,
            1.0,
            -0.25,
            f32::MIN_POSITIVE,
            f32::from_bits(0x8000_0001),
        ][index % 6]
    });
    for value in [0.0, 0.5, f32::from_bits(0x3f00_0001), 1.0] {
        for mut state in Gain.static_layout().states().to_vec() {
            Gain.set_parameter(&mut state, 2, value);
            Gain.reset(&mut state);
            let mut samples = input;
            Gain.process(&mut state, &mut samples);
            let sign = if value > 0.5 { 0x8000_0000 } else { 0 };
            assert_eq!(
                samples.map(f32::to_bits),
                input.map(|v| prm::operand(v).to_bits() ^ sign)
            );
            let before = state.clone();
            Gain.set_parameter(&mut state, u32::MAX, 0.75);
            assert_eq!(state, before, "unknown indices must be ignored");
        }
    }
}

#[test]
fn v1_ramp_emits_before_increment_retargets_and_snaps_to_zero() {
    let mut state = Gain.static_layout().states()[0].clone();
    Gain.set_parameter(&mut state, 0, 0.0);
    let rate = f32::from_bits(0x39da_740e);
    let mut current = 1.0;
    for block in 0..3 {
        let step = prm::mul(prm::sub(0.0, current), rate);
        let expected: [u32; BLOCK_SAMPLES] = std::array::from_fn(|_| {
            let bits = current.to_bits();
            current = prm::add(current, step);
            bits
        });
        let mut samples = [1.0; BLOCK_SAMPLES];
        Gain.process(&mut state, &mut samples);
        assert_eq!(samples.map(f32::to_bits), expected, "block {block}");
    }
    // A parameter change starts the next interpolation at the live gain.
    Gain.set_parameter(&mut state, 2, 1.0);
    let mut samples = [1.0; BLOCK_SAMPLES];
    Gain.process(&mut state, &mut samples);
    assert_eq!(samples[0].to_bits(), current.to_bits());
    for _ in 0..1000 {
        let mut samples = [1.0; BLOCK_SAMPLES];
        Gain.process(&mut state, &mut samples);
    }
    let mut samples = [1.0; BLOCK_SAMPLES];
    Gain.process(&mut state, &mut samples);
    assert!(samples.iter().all(|sample| *sample == 0.0));
}

#[test]
fn v2_gain_matches_prm_through_parameters_audio_and_lifecycle() {
    let mut frames = standard_frames();
    for (frame, index, value) in [
        (0, 0, 0.5),
        (1, 0, 0.0),
        (2, 0, 1.0),
        (3, 1, 0.0),
        (4, 1, 0.25),
        (5, 1, 0.5),
        (6, 1, 0.75),
        (7, 1, 1.0),
        (8, 2, 0.5),
        (9, 2, f32::from_bits(0x3f00_0001)),
        (10, 2, 0.0),
        (11, 0, f32::from_bits(1)),
        (12, 0, 0.01),
        (20, 0, 0.8),
        (40, 1, 0.5),
        (41, 2, 1.0),
        (42, 99, 0.5),
        (63, 0, 0.0),
    ] {
        frames[frame]
            .parameters
            .push(ParameterUpdate { index, value });
    }
    for value in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -0.0,
        -1.0,
        1.5,
        f32::from_bits(0x3f80_0001),
    ] {
        frames[30]
            .parameters
            .push(ParameterUpdate { index: 0, value });
    }
    frames[33].samples[0] = std::array::from_fn(|index| {
        [
            f32::MIN_POSITIVE,
            f32::from_bits(0x007f_ffff),
            f32::from_bits(0x8000_0001),
            -0.0,
        ][index % 4]
    });
    // Let the 2400-sample smoother reach its exact snap, then dirty the same
    // inverted target to exercise the longest process path.
    let mut tail = frames[0].clone();
    tail.parameters.clear();
    tail.samples = [[1.0; BLOCK_SAMPLES]; 2];
    frames.extend(std::iter::repeat_n(tail, 640));
    frames.last_mut().unwrap().parameters.push(ParameterUpdate {
        index: 2,
        value: 1.0,
    });
    let report = verify_module(&Gain, &frames).unwrap();
    let built = BuiltModule::build(&Gain).unwrap();
    assert_eq!(report.frames, 704);
    assert_eq!(report.maximum_process_cycles, 363);
    assert_eq!(built.stats.cycles_per_stereo_block, 365);
    eprintln!(
        "Gain: {} maximum modeled stereo cycles / 32 frames; {} bound; {} DM, {} VISA, {} ISA bytes",
        report.maximum_process_cycles,
        built.stats.cycles_per_stereo_block,
        built.stats.dm_bytes,
        built.stats.visa_bytes,
        built.stats.isa_bytes
    );
}

#[test]
fn v3_gain_relocates_like_linking_and_deploys_two_distinct_instances() {
    let built = BuiltModule::build(&Gain).unwrap();
    assert_eq!(
        ModuleBlob::parse(&built.blob.to_bytes().unwrap()).unwrap(),
        built.blob
    );
    assert!(!built.stats.reuse_pmco);
    assert!(built.blob.relocation_words(Section::Isa).is_none());
    for placement in place(&[Some(&built.blob), Some(&built.blob)]).unwrap() {
        let placement = placement.unwrap();
        let placed = relocate(&built.blob, &placement).unwrap();
        let at = SectionAddresses {
            dm: placement.remap(Section::Dm.base()),
            visa: placement.remap(Section::Code.base()),
            isa: placement.remap(Section::Isa.base()),
        };
        let linked = LinkedModule::link(&Gain, at).unwrap();
        assert_eq!(placed.entry, at.visa);
        for transfer in placed.transfers {
            let expected = match transfer.section {
                Section::Dm => linked.dm.clone(),
                Section::Code => linked.visa.iter().flat_map(|p| p.to_le_bytes()).collect(),
                Section::Isa => linked.isa.isa48_le_bytes(),
                other => panic!("unexpected section {other:?}"),
            };
            assert_eq!(transfer.bytes, expected);
        }
    }
    support::deployment::verify_deployment(&built.blob, STOCK_MODEL_ID);
}

#[test]
fn v4_gain_shim_keeps_the_prm_and_qc_asm_return_vectors() {
    let linked = LinkedModule::link(&Gain, SectionAddresses::LINK).unwrap();
    support::verify_stub_vectors(&linked);
}

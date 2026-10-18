use std::sync::Arc;
use std::time::Duration;

use tetrad_dsp_protocol::requests::BypassState;
use tetrad_dsp_protocol::{DeploymentTarget, DspCore, ModuleSlot};
use tetrad_media::cab::{Biquad, CabBlock, CabDriver};
use tetrad_media::ir::IrChannel;

use super::*;
use crate::setup::drivers::media::test_media::{TestMedia, impulse};
use crate::setup::test_factory::build_with;
use crate::setup::{Block, SetupValue};

fn at() -> DeploymentTarget {
    DeploymentTarget {
        core: DspCore::Core2,
        slot: ModuleSlot::try_from(5).unwrap(),
    }
}

fn identity(model_id: u32, category: u32) -> ModelIdentity {
    ModelIdentity {
        target: at(),
        model_id,
        category,
        fx_loop_send: None,
    }
}

/// A runtime list of `count` values plus MUTE and BYPASS, strings at `texts`.
fn values(count: usize, texts: &[usize], set: &[(usize, f32)]) -> Vec<SetupValue> {
    let mut values = vec![SetupValue::Float(0.0_f32.to_bits()); count + 2];
    for &at in texts {
        values[at] = SetupValue::Text;
    }
    for &(at, value) in set {
        values[at] = SetupValue::Float(value.to_bits());
    }
    values
}

fn params(out: &[DspWrite]) -> Vec<(u32, f32)> {
    out.iter()
        .filter_map(|write| match write {
            DspWrite::Parameter {
                index, value_bits, ..
            } => Some((*index, f32::from_bits(*value_bits))),
            _ => None,
        })
        .collect()
}

fn data(out: &[DspWrite]) -> Vec<(u8, Vec<u8>)> {
    out.iter()
        .filter_map(|write| match write {
            DspWrite::Data {
                buffer, payload, ..
            } => Some((*buffer, payload.to_vec())),
            _ => None,
        })
        .collect()
}

fn edit(block: &mut Block, index: u32, value: f32, out: &mut Vec<DspWrite>) {
    block
        .edit(
            index,
            SetupValue::Float(value.to_bits()),
            ParamSetMode::Live,
            out,
        )
        .unwrap();
}

fn text(block: &mut Block, index: u32, value: &str, mode: ParamSetMode, media: &TestMedia) {
    block
        .set_text(index, value, mode, media, &mut Vec::new())
        .unwrap();
}

/// Plain mono cab: geometry, filters and OUTPUT VOLUME stay on the ARM; the
/// push end reloads both mics and sends their blends (mic A custom IR, mic
/// B default IR) through the filters; the 0 ms timer then gates live sends.
#[test]
fn mono_push_blends_filters_and_sends_both_mics() {
    let mut media = TestMedia::default();
    media
        .cabs
        .insert("CIR_a".into(), CabIrs::Custom(impulse(3, 1.0)));
    let mut out = Vec::new();
    let mut block = build_with(identity(12001, 12), &[], &media, &mut out).unwrap();
    assert!(out.is_empty(), "no plugin_id: no ctor writes");
    text(&mut block, 1, "CIR_a", ParamSetMode::Push, &media);
    text(&mut block, 9, "missing", ParamSetMode::Push, &media);
    let set = [
        (2, 0.5),
        (4, 0.25),
        (5, 0.75),
        (7, 1.0),
        (16, 0.5),
        (17, 1.0),
        (18, 0.5),
    ];
    let list = values(21, &[1, 9, 19, 20], &set);
    block
        .push(&list, BlockBypass::default(), None, &mut out)
        .unwrap();

    assert_eq!(
        out[0],
        DspWrite::Bypass {
            target: at(),
            state: BypassState::Engaged
        }
    );
    let gain = db_to_gain(to_display(0.5, -96.0, 12.0, OUTPUT_SKEW));
    let sent: Vec<u32> = params(&out).iter().map(|(i, _)| *i).collect();
    assert_eq!(sent, [0, 2, 3, 6, 8, 10, 11, 14, 18]);
    assert_eq!(params(&out)[8], (18, gain));

    let stock = Arc::clone(&media.stock.cab);
    let mut expected = CabBlock::new(CabDriver::Mono, 12001, &stock);
    let mic = &mut expected.mics[0];
    mic.set_distance(0.25);
    mic.set_position(0.75);
    mic.set_grid_mode(1.0);
    mic.load("Single", &[impulse(3, 1.0)]).unwrap();
    expected.mics[1].set_position(0.0);
    expected.mics[1].load_default(Arc::clone(&stock.default_ir));
    expected.low_cut = Biquad::low_cut(f64::from(to_display(0.5, 20.0, 500.0, FILTER_SKEW)));
    expected.gain = gain;
    let mic_a = expected.mic_transfer(IrChannel::Channel0).payload;
    let mic_b = expected.mic_transfer(IrChannel::Channel1).payload;
    assert_eq!(data(&out), [(0, mic_a), (1, mic_b)]);

    assert_eq!(block.take_timer(), Some(Duration::ZERO));
    out.clear();
    edit(&mut block, 13, 0.1, &mut out);
    assert!(out.is_empty(), "the timer is still running");
    block.timer_elapsed(&mut out);
    assert_eq!(data(&out).iter().map(|(b, _)| *b).collect::<Vec<_>>(), [1]);
    assert_eq!(block.take_timer(), Some(Duration::ZERO));
}

/// Plugin cabs (0x2ed194): PLUGIN MODE and the 448-byte table in the ctor
/// (21007 sets PLUGIN MODE instead), the effective bypass with both mics
/// off, the room flags, LEVEL as gain and the John Mayer SIZE rows.
#[test]
fn plugin_mono_cab_sends_its_own_bypass_rooms_and_sizes() {
    let mut media = TestMedia::default();
    media.plugins.extend([(12079, 21), (21007, 4)]);
    let mut out = Vec::new();
    build_with(identity(21007, 21), &[], &media, &mut out).unwrap();
    assert_eq!(out, [DspWrite::parameter(at(), PLUGIN_MODE, 1.0)]);

    out.clear();
    let mut block = build_with(identity(12079, 12), &[], &media, &mut out).unwrap();
    assert_eq!(out[0], DspWrite::parameter(at(), PLUGIN_MODE, 0.0));
    assert_eq!(data(&out[1..]), [(2, vec![0x11; 448])]);

    out.clear();
    let list = values(31, &[1, 9, 29, 30], &[(24, 1.0)]);
    block
        .push(&list, BlockBypass::default(), None, &mut out)
        .unwrap();
    assert_eq!(
        out[0],
        DspWrite::Bypass {
            target: at(),
            state: BypassState::Engaged
        }
    );
    let gain = db_to_gain(to_display(0.0, -40.0, 6.0, LEVEL_SKEW) - 6.0);
    assert!(params(&out).contains(&(2, gain)));
    assert!(params(&out).contains(&(24, 1.0)), "ROOM LEFT on, mic A on");

    block.timer_elapsed(&mut Vec::new());
    out.clear();
    edit(&mut block, 0, 1.0, &mut out);
    edit(&mut block, 8, 1.0, &mut out);
    let bypasses: Vec<_> = out
        .iter()
        .filter_map(|w| match w {
            DspWrite::Bypass { state, .. } => Some(*state),
            _ => None,
        })
        .collect();
    assert_eq!(bypasses, [BypassState::Engaged, BypassState::Bypassed]);
    assert_eq!(params(&out), [(24, 0.0), (0, 1.0), (26, 0.0), (8, 1.0)]);

    out.clear();
    edit(&mut block, 23, 0.5, &mut out);
    let [sizes, pre_delays, low_passes] = media.stock.room_sizes;
    assert_eq!(
        params(&out),
        [
            (23, sizes[1]),
            (
                20,
                to_normalized(pre_delays[1], 1.0, 100.0, IR_LOADER_FILTER_SKEW)
            ),
            (
                22,
                to_normalized(low_passes[1], 800.0, 20_000.0, IR_LOADER_FILTER_SKEW)
            ),
        ]
    );
    out.clear();
    edit(&mut block, 20, 0.5, &mut out);
    assert!(out.is_empty(), "John Mayer PRE DELAY is dropped");
}

/// Petrucci: OUTPUT VOLUME is a host gain on mics and rooms (all four
/// parts go dirty); ROOM DIST. sends one gained room IR; an unchanged gain
/// sends only the generic value.
#[test]
fn petrucci_rooms_follow_distance_and_output_gain() {
    let mut media = TestMedia::default();
    media.plugins.insert(12078, 13);
    let mut out = Vec::new();
    let mut block = build_with(identity(12078, 12), &[], &media, &mut out).unwrap();
    assert_eq!(out, [DspWrite::parameter(at(), PLUGIN_MODE, 0.0)]);
    let list = values(28, &[1, 9, 26, 27], &[(17, 1.0), (18, 1.0)]);
    block
        .push(&list, BlockBypass::default(), None, &mut out)
        .unwrap();
    let buffers: Vec<u8> = data(&out).iter().map(|(b, _)| *b).collect();
    assert_eq!(buffers, [0, 1, 2, 3]);

    block.timer_elapsed(&mut Vec::new());
    out.clear();
    edit(&mut block, 24, 0.5, &mut out);
    let stock = &media.stock.cab;
    let mut expected = CabBlock::new(CabDriver::Mono, 12078, stock);
    expected.gain = db_to_gain(12.0);
    expected.room_distance[0] = 0.5;
    let room = expected.room_transfer(stock, IrChannel::Channel0);
    assert_eq!(data(&out), [(2, room.payload)]);
    assert!(params(&out).is_empty());

    block.timer_elapsed(&mut Vec::new());
    out.clear();
    edit(&mut block, 18, 1.0, &mut out);
    assert_eq!(params(&out), [(18, 1.0)]);
    assert!(data(&out).is_empty());
}

/// Stereo cab: ctor config and GRID MODE writes; values remapped onto the
/// IR Loader Lite layout; mics scaled on send; scene recall reloads mono
/// cabs only.
#[test]
fn stereo_cab_remaps_values_and_skips_the_recall_reload() {
    let mut media = TestMedia::default();
    media
        .cabs
        .insert("set".into(), CabIrs::Custom(impulse(0, 1.0)));
    let mut out = Vec::new();
    let mut block = build_with(identity(32001, 32), &[], &media, &mut out).unwrap();
    let config: Vec<u8> = [2_u32, 2].iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(data(&out), [(3, config)]);
    assert_eq!(params(&out), [(7, 0.0), (15, 0.0)]);

    text(&mut block, 1, "set", ParamSetMode::Push, &media);
    out.clear();
    let set = [
        (2, 1.0),
        (3, 0.25),
        (6, 1.0),
        (16, 1.0),
        (17, 0.0),
        (18, 1.0),
    ];
    block
        .push(
            &values(21, &[1, 9, 19, 20], &set),
            BlockBypass::default(),
            None,
            &mut out,
        )
        .unwrap();
    let level = to_normalized(6.0, -96.0, 12.0, IR_LOADER_LEVEL_SKEW);
    let low_cut = to_normalized(500.0, 20.0, 800.0, IR_LOADER_FILTER_SKEW);
    let mapped = params(&out);
    let remapped = [
        (3, level),
        (6, 0.25),
        (1, 1.0),
        (4, low_cut),
        (12, low_cut),
        (5, 0.0),
        (21, 1.0),
    ];
    for expected in remapped {
        assert!(mapped.contains(&expected), "{expected:?} in {mapped:?}");
    }
    assert!(
        !mapped.iter().any(|(i, _)| *i == 2 || *i == 16),
        "no generic values"
    );
    let mut expected = CabBlock::new(CabDriver::Stereo, 32001, &media.stock.cab);
    expected.mics[0].set_position(0.0);
    expected.mics[1].set_position(0.0);
    expected.mics[0].load("Single", &[impulse(0, 1.0)]).unwrap();
    let mic_a = expected.mic_transfer(IrChannel::Channel0).payload;
    let mic_b = expected.mic_transfer(IrChannel::Channel1).payload;
    assert_eq!(data(&out), [(0, mic_a), (1, mic_b)]);

    block.timer_elapsed(&mut Vec::new());
    out.clear();
    block.recalled(&|_| None, &mut out);
    assert!(out.is_empty(), "stereo cabs keep their IRs in a recall");

    let mut mono = build_with(identity(12001, 12), &[], &media, &mut out).unwrap();
    text(&mut mono, 1, "set", ParamSetMode::Push, &media);
    mono.push(
        &values(21, &[1, 9, 19, 20], &[]),
        BlockBypass::default(),
        None,
        &mut out,
    )
    .unwrap();
    mono.timer_elapsed(&mut Vec::new());
    out.clear();
    mono.recalled(&|_| None, &mut out);
    assert_eq!(data(&out).len(), 1, "mono recall reloads and resends mic A");
}

/// Live `ir selector` edits load at once; the same value is ignored; an
/// unknown selector falls back to the default IR.
#[test]
fn live_selector_edits_load_once_and_fall_back_to_the_default_ir() {
    let mut media = TestMedia::default();
    media.cabs.insert(
        "ML_set".into(),
        CabIrs::Factory {
            tag: "ML".into(),
            irs: (0..9).map(|i| impulse(i, 1.0)).collect(),
        },
    );
    let mut out = Vec::new();
    let mut block = build_with(identity(12001, 12), &[], &media, &mut out).unwrap();
    // LPF at 20 kHz keeps the host filters off.
    let list = values(21, &[1, 9, 19, 20], &[(17, 1.0)]);
    block
        .push(&list, BlockBypass::default(), None, &mut out)
        .unwrap();
    block.timer_elapsed(&mut Vec::new());

    out.clear();
    block
        .set_text(1, "ML_set", ParamSetMode::Live, &media, &mut out)
        .unwrap();
    let mut expected = CabBlock::new(CabDriver::Mono, 12001, &media.stock.cab);
    expected.mics[0].set_position(0.0);
    let irs: Vec<_> = (0..9).map(|i| impulse(i, 1.0)).collect();
    expected.mics[0].load("ML", &irs).unwrap();
    assert_eq!(
        data(&out),
        [(0, expected.mic_transfer(IrChannel::Channel0).payload)]
    );

    block.timer_elapsed(&mut Vec::new());
    out.clear();
    block
        .set_text(1, "ML_set", ParamSetMode::Live, &media, &mut out)
        .unwrap();
    assert!(out.is_empty());
    block
        .set_text(1, "gone", ParamSetMode::Live, &media, &mut out)
        .unwrap();
    expected.mics[0].load_default(Arc::clone(&media.stock.cab.default_ir));
    assert_eq!(
        data(&out),
        [(0, expected.mic_transfer(IrChannel::Channel0).payload)]
    );
}

/// Stereo recall loads nothing and must not record its selector, so a later
/// live edit to the recalled IR still loads it.
#[test]
fn stereo_recall_does_not_store_the_selector() {
    let mut media = TestMedia::default();
    media
        .cabs
        .insert("set".into(), CabIrs::Custom(impulse(0, 1.0)));
    media
        .cabs
        .insert("next".into(), CabIrs::Custom(impulse(1, 1.0)));
    let mut out = Vec::new();
    let mut block = build_with(identity(32001, 32), &[], &media, &mut out).unwrap();
    text(&mut block, 1, "set", ParamSetMode::Push, &media);
    block
        .push(
            &values(21, &[1, 9, 19, 20], &[]),
            BlockBypass::default(),
            None,
            &mut out,
        )
        .unwrap();
    block.timer_elapsed(&mut Vec::new());

    out.clear();
    block
        .set_text(1, "next", ParamSetMode::Recall, &media, &mut out)
        .unwrap();
    block.recalled(&|_| None, &mut out);
    assert!(out.is_empty(), "stereo recall loads nothing");

    block
        .set_text(1, "next", ParamSetMode::Live, &media, &mut out)
        .unwrap();
    let loaded: Vec<u8> = data(&out).iter().map(|(buffer, _)| *buffer).collect();
    assert_eq!(loaded, [0], "the live edit loads mic A");
}

/// Parallax (cab base 14): its own switches drive DSP params 34..=38 and
/// the mic enables; the cab's OUTPUT VOLUME still goes to index 18.
#[test]
fn parallax_switches_and_cab_hooks_use_mic_base_14() {
    let mut media = TestMedia::default();
    media.plugins.insert(PARALLAX, 4);
    media
        .cabs
        .insert("p".into(), CabIrs::Custom(impulse(1, 1.0)));
    let mut out = Vec::new();
    let mut block = build_with(identity(PARALLAX, 3), &[], &media, &mut out).unwrap();
    assert_eq!(block.driver_kind(), Some(DriverKind::Parallax));
    assert!(out.is_empty());
    text(&mut block, 15, "p", ParamSetMode::Push, &media);
    let on = [
        (34, 1.0),
        (35, 1.0),
        (36, 1.0),
        (37, 1.0),
        (39, 1.0),
        (40, 1.0),
    ];
    let list = values(44, &[15, 23, 42, 43], &on);
    block
        .push(&list, BlockBypass::default(), None, &mut out)
        .unwrap();
    // Both mics: POSITION 0.0 differs from the ctor position.
    assert_eq!(
        data(&out).iter().map(|(b, _)| *b).collect::<Vec<_>>(),
        [0, 1]
    );
    block.timer_elapsed(&mut Vec::new());

    out.clear();
    edit(&mut block, 39, 0.0, &mut out);
    let pedal_off = [
        (34, 0.0),
        (35, 0.0),
        (36, 0.0),
        (37, 1.0),
        (14, 0.0),
        (22, 0.0),
        (38, PARALLAX_OUTPUT_CAB_ONLY),
    ];
    assert_eq!(params(&out), pedal_off);
    out.clear();
    edit(&mut block, 32, 1.0, &mut out);
    assert_eq!(params(&out), [(OUTPUT_VOLUME, db_to_gain(12.0))]);
}

use super::*;
use crate::setup::drivers::amps::tests::identity;

/// Synthetic words: insert paths 100 + i, QUANTIZE i, PRE ROLL 0.25·i.
struct Synthetic;

impl StockTables for Synthetic {
    fn read(&self, payload: StockPayload) -> Result<Vec<u8>, TableError> {
        let word = |i: usize| match payload {
            p if p == PRE_ROLL => (0.25 * i as f32).to_bits(),
            p if p == QUANTIZE => i as u32,
            _ => 100 + i as u32,
        };
        Ok((0..payload.len() / 4)
            .flat_map(|i| word(i).to_le_bytes())
            .collect())
    }
}

fn param(parameter: u32, value: f32) -> LooperWrite {
    LooperWrite::Request(TunerLooperRequest::LooperParameter {
        parameter,
        value_bits: value.to_bits(),
    })
}

/// 27002 routes int 0 to send 0 and return 0 and the gains to send 1 and
/// return 1; the second nonzero-path gain write keeps send 1's slot.
#[test]
fn looper_27002_routing_targets_and_waits() {
    let [send0, send1, return0, mut return1] = [8, 9, 10, 11].map(|slot| {
        let mut target = identity(27002).target;
        target.slot = tetrad_dsp_protocol::ModuleSlot::try_from(slot).unwrap();
        target
    });
    return1.core = tetrad_dsp_protocol::DspCore::Core2;
    let placements = Placements {
        sends: [Some(send0), Some(send1)],
        returns: [Some(return0), Some(return1)],
    };
    let tables = LooperTables::read(&Synthetic).unwrap();
    let mut looper = Looper::new(27002, placements, &tables).unwrap();
    let mut out = Vec::new();
    looper
        .update_parameter(17, 1.0, ParamSetMode::Push, &mut out)
        .unwrap();
    let router = |write: DspWrite| LooperWrite::Router(write);
    let insert = |destination, path| {
        LooperWrite::Request(TunerLooperRequest::GlobalLooperInsertPath { destination, path })
    };
    let quirk = DeploymentTarget {
        slot: send1.slot,
        ..return1
    };
    assert_eq!(
        out,
        [
            router(DspWrite::parameter(send1, 9, 0.0)),
            router(DspWrite::parameter(quirk, 9, 0.0)),
            router(DspWrite::IntParameter {
                target: send0,
                index: 0,
                value: 0
            }),
            router(DspWrite::IntParameter {
                target: return0,
                index: 0,
                value: 0
            }),
            insert(0, 112),
            insert(1, 112),
            router(DspWrite::parameter(send1, 7, 1.0)),
            router(DspWrite::parameter(return1, 7, 1.0)),
        ]
    );
    out.clear();
    looper
        .update_parameter(17, 0.0, ParamSetMode::Live, &mut out)
        .unwrap();
    assert_eq!(
        out[..3],
        [
            insert(0, u32::MAX),
            insert(1, u32::MAX),
            LooperWrite::Wait(PATH_WAIT)
        ]
    );
    out.clear();
    looper
        .update_parameter(17, 0.5, ParamSetMode::Recall, &mut out)
        .unwrap();
    assert_eq!(out[4], LooperWrite::Wait(PATH_WAIT));
    assert!(
        Looper::new(
            27000,
            Placements {
                returns: [Some(return0), None],
                ..Placements::default()
            },
            &tables
        )
        .is_err()
    );
}

/// 0x55e5ac → 0x513df0: each QUANTIZE send sets the controller flag `+0xb882` to
/// `quantize > 0` (the value Cortex Control reads).
#[test]
fn quantize_send_sets_the_controller_flag() {
    let target = identity(27000).target;
    let placements = Placements {
        sends: [Some(target), None],
        returns: [Some(target), None],
    };
    let tables = LooperTables::read(&Synthetic).unwrap();
    let mut looper = Looper::new(27000, placements, &tables).unwrap();
    let mut playback = LooperPlayback {
        cached_state: 1,
        incoming: LooperStatus::from_words([0; 22]),
        oneshot: false,
        dupwait: false,
        adopt_next_state: false,
        quantize_enabled: false,
    };
    let mut out = Vec::new();
    for (value, enabled) in [(1.0, true), (0.0, false)] {
        let context = LooperContext::default();
        looper
            .set_parameter(
                18,
                value,
                ParamSetMode::Live,
                Some(&mut playback),
                &context,
                &mut out,
            )
            .unwrap();
        assert_eq!(playback.quantize_enabled, enabled);
    }
    assert_eq!(out, [param(7, 9.0), param(7, 0.0)]);
}

/// Recovered controls: QUANTIZE dedupes only live, PRE ROLL and REC.
/// LENGTH quantize their display, 21 and 23+ are no-ops; the compatible
/// API refuses absent playback state and MIDI settings.
#[test]
fn looper_recovered_controls_and_unrecovered_vas() {
    let target = identity(27000).target;
    let placements = Placements {
        sends: [Some(target), None],
        returns: [Some(target), None],
    };
    let tables = LooperTables::read(&Synthetic).unwrap();
    let mut looper = Looper::new(27000, placements, &tables).unwrap();
    let mut out = Vec::new();
    for (index, value, mode) in [
        (18, 0.0, ParamSetMode::Live),
        (18, 0.0, ParamSetMode::Recall),
        (18, 1.0, ParamSetMode::Live),
        (20, 0.5, ParamSetMode::Push),
        (22, 0.51, ParamSetMode::Live),
        (19, 0.5, ParamSetMode::Live),
        (21, 1.0, ParamSetMode::Live),
        (30, 1.0, ParamSetMode::Live),
        (8, 0.0, ParamSetMode::Push),
        (5, f32::NAN, ParamSetMode::Live),
        (0, 1.0, ParamSetMode::Push),
    ] {
        looper
            .update_parameter(index, value, mode, &mut out)
            .unwrap();
    }
    assert_eq!(
        out,
        [
            param(7, 0.0),
            param(7, 9.0),
            param(10, 0.5),
            param(11, 16.0),
            param(9, 0.0),
            param(0, 0.0),
        ]
    );
    let va = |result: Result<(), SetupError>| match result {
        Err(SetupError::UnrecoveredPath { va, .. }) => va,
        other => panic!("expected an unrecovered path, got {other:?}"),
    };
    assert_eq!(
        va(looper.update_parameter(0, 1.0, ParamSetMode::Live, &mut out)),
        0x55e1fc
    );
    assert_eq!(
        va(looper.update_parameter(19, 1.0, ParamSetMode::Push, &mut out)),
        0x55e93c
    );
    out.clear();
    looper.status_tick(false, &mut out);
    assert_eq!(
        out,
        [LooperWrite::Request(
            TunerLooperRequest::LooperStatusRequest { track: 0 }
        )]
    );
}

mod buttons;
mod reconcile;

/// 0x55d89c..0x55d8b8: a mini driver starts with record, overdub and punch
/// latched and duplicate sync on; P13–16 cannot change them and P15 never
/// clears dupwait.
#[test]
fn mini_constructor_latches_modes() {
    let target = identity(27000).target;
    let placements = Placements {
        sends: [Some(target), None],
        returns: [Some(target), None],
    };
    let tables = LooperTables::read(&Synthetic).unwrap();
    let mut looper = Looper::for_product(27000, placements, &tables, LooperProduct::Mini).unwrap();
    let (_, mut playback, mut context) = fixture(4);
    context.mode_changes_allowed = Some(false);
    playback.dupwait = true;
    for index in 13..=16 {
        assert!(button(&mut looper, &mut playback, &context, index, 0.0).is_empty());
    }
    assert!(playback.dupwait);
    // Latched record: a release in state 4 does nothing.
    assert!(button(&mut looper, &mut playback, &context, 0, 0.0).is_empty());
    // Duplicate sync: Duplicate from state 2 waits for the cycle (event 9).
    playback.cached_state = 2;
    assert_eq!(
        button(&mut looper, &mut playback, &context, 3, 1.0),
        [event(9)]
    );
}

/// 0x50eef8: `+0xd4` is set only for an old 27000 whose model is absent from
/// the incoming grid; only an old 27000 adds one status request, unless
/// `nopoll`.
#[test]
fn replacement_picks_the_teardown_branch_and_status_request() {
    let status = LooperWrite::Request(TunerLooperRequest::LooperStatusRequest { track: 0 });
    let disconnect = |destination| {
        LooperWrite::Request(TunerLooperRequest::GlobalLooperInsertPath {
            destination,
            path: u32::MAX,
        })
    };
    let run = |model_id, incoming: &[u32], nopoll| {
        let target = identity(model_id).target;
        let tables = LooperTables::read(&Synthetic).unwrap();
        let looper = Looper::new(
            model_id,
            Placements {
                sends: [Some(target), Some(target)],
                returns: [Some(target), Some(target)],
            },
            &tables,
        )
        .unwrap();
        let (_, mut playback, _) = fixture(1);
        let mut out = Vec::new();
        looper.replace(incoming.iter().copied(), nopoll, &mut playback, &mut out);
        out
    };
    assert_eq!(
        run(27000, &[100], false),
        [disconnect(0), disconnect(1), status.clone()]
    );
    assert_eq!(run(27000, &[27000], false), [status]);
    assert_eq!(run(27000, &[], true), [disconnect(0), disconnect(1)]);
    assert!(run(27002, &[], false).is_empty());
}

fn fixture(state: u32) -> (Looper, LooperPlayback, LooperContext) {
    let target = identity(27000).target;
    let tables = LooperTables::read(&Synthetic).unwrap();
    let looper = Looper::new(
        27000,
        Placements {
            sends: [Some(target), None],
            returns: [Some(target), None],
        },
        &tables,
    )
    .unwrap();
    let playback = LooperPlayback {
        cached_state: state,
        incoming: LooperStatus::from_words([0; 22]),
        oneshot: false,
        dupwait: false,
        adopt_next_state: false,
        quantize_enabled: false,
    };
    let context = LooperContext {
        mode_changes_allowed: Some(true),
        clock: Some(LooperClock {
            metronome_slot_4: 0.0,
            midi_gate_f514: false,
            midi_gate_fbb4: false,
        }),
        midi_clock_start: None,
    };
    (looper, playback, context)
}

fn event(event: u32) -> LooperWrite {
    LooperWrite::Request(TunerLooperRequest::LooperEvent { track: 0, event })
}

fn reset_clock() -> LooperWrite {
    LooperWrite::Control(ControlRequest::ResetInternalMidiClock { value: 1 })
}

fn button(
    looper: &mut Looper,
    playback: &mut LooperPlayback,
    context: &LooperContext,
    index: u32,
    value: f32,
) -> Vec<LooperWrite> {
    let mut out = Vec::new();
    looper
        .set_parameter(
            index,
            value,
            ParamSetMode::Live,
            Some(playback),
            context,
            &mut out,
        )
        .unwrap();
    out
}

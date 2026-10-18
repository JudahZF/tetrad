use std::time::Duration;

use tetrad_dsp_protocol::requests::Request;
use tetrad_dsp_protocol::{DeploymentTarget, DspCore, ModuleSlot};

use super::*;
use crate::setup::DspWrite;

fn at(core: DspCore, slot: u32) -> DeploymentTarget {
    DeploymentTarget {
        core,
        slot: ModuleSlot::try_from(slot).unwrap(),
    }
}

fn lane(input: u32, output: u32) -> Lane {
    Lane {
        input,
        output,
        volume: 0.0,
        pan: 0.5,
        scene_volume: 0.0,
        mute: 0.0,
        solo: 0.0,
        gate: [0.0; 4],
        path_b_muted: false,
        input_router: None,
        output_router: None,
    }
}

fn context(lanes: &[Lane; 4], bulk: bool) -> SidechainContext<'_> {
    SidechainContext {
        lanes,
        pairs: [PairJunction::default(); 2],
        tuner_input: None,
        bulk,
    }
}

/// A sink with its receive router at `router`; SOC1 sinks use the router as
/// the source router too (compiler `0x87e90c`).
fn sink(row: u8, router: DeploymentTarget, source: SidechainSource) -> SidechainSink {
    let receive = LaneRouter {
        target: router,
        model_id: RECEIVE_ROUTER,
    };
    SidechainSink {
        cell: GridCell::new(row, 3).unwrap(),
        placement: at(router.core, u32::from(router.slot.index()) + 1),
        source_router: receive,
        router: receive,
        channel: 1,
        slave_channel: 0,
        source,
        level: Some(0.25),
        bypassed: false,
    }
}

fn int(target: DeploymentTarget, index: u32, value: u32) -> DspWrite {
    DspWrite::IntParameter {
        target,
        index,
        value,
    }
}

fn float(target: DeploymentTarget, index: u32, value: f32) -> DspWrite {
    DspWrite::parameter(target, index, value)
}

fn routes(same_core: bool, channel: u8, source: DeploymentTarget) -> Vec<DspWrite> {
    [
        DspCore::Core0,
        DspCore::Core1,
        DspCore::Core2,
        DspCore::Core3,
    ]
    .map(|core| {
        DspWrite::Request(Request::RouteToSidechainBuffer {
            core,
            same_core,
            channel,
            source_slot: u32::from(source.slot.index()),
            source_core: source.core as u32,
        })
    })
    .to_vec()
}

/// VA 0x5843a4 mode 0 for a SOC1 sink fed by a block: route to every core
/// (`0x49` when the source shares the sink's core), source router 0/1, then
/// `0x582bd4` (stereo, kind 12, channel, unity return, level) and the active
/// flag. A source in the other SOC adds 2 to the channel and uses `0x48`.
#[test]
fn block_source_routes_then_sets_the_sink_router() {
    let lanes = [lane(1, 1), lane(0, 0), lane(0, 0), lane(0, 0)];
    let router = at(DspCore::Core0, 4);
    let source = at(DspCore::Core0, 2);
    let block = SidechainSource::Block {
        cell: GridCell::new(0, 1).unwrap(),
        placement: Some(source),
    };
    let mut out = Vec::new();
    Sidechains::new().push(&[sink(0, router, block)], &context(&lanes, true), &mut out);
    let mut expected = routes(true, 1, source);
    expected.extend([
        int(router, 0, 12),
        int(router, 1, 1),
        int(router, 2, 1),
        int(router, 0, 12),
        int(router, 1, 1),
        float(router, 7, 1.0),
        float(router, 9, 1.0),
        float(router, 8, 0.25),
        int(router, 5, 1),
    ]);
    assert_eq!(out, expected);

    let far = at(DspCore::Core2, 6);
    let block = SidechainSource::Block {
        cell: GridCell::new(2, 0).unwrap(),
        placement: Some(far),
    };
    let mut out = Vec::new();
    let sink = SidechainSink {
        bypassed: true,
        ..sink(0, router, block)
    };
    Sidechains::new().push(&[sink], &context(&lanes, true), &mut out);
    assert_eq!(out[..4], routes(false, 3, far)[..]);
    assert_eq!(out[5], int(router, 1, 3));
    assert_eq!(out.last(), Some(&int(router, 5, 0)));
}

/// A SOC2 sink fed by an input: the core-1 forward takes the input's
/// kind and port; the sink router reads the SlaveLink channel, mono, with
/// the input return gain. The tuner cut (`0x582a90`) zeroes the sink
/// router's kind and channel only.
#[test]
fn soc2_input_source_uses_the_forward_and_slave_link() {
    let lanes = [lane(0, 0), lane(0, 0), lane(0, 0), lane(0, 0)];
    let forward = at(DspCore::Core1, 9);
    let router = at(DspCore::Core2, 5);
    let sink = SidechainSink {
        source_router: LaneRouter {
            target: forward,
            model_id: SEND_RECEIVE_ROUTER,
        },
        slave_channel: 4,
        level: None,
        ..sink(2, router, SidechainSource::Input(2))
    };
    let mut out = Vec::new();
    Sidechains::new().push(&[sink], &context(&lanes, true), &mut out);
    assert_eq!(
        out,
        [
            int(forward, 0, 1),
            int(forward, 1, 2),
            int(router, 2, 0),
            int(router, 0, 2),
            int(router, 1, 4),
            float(router, 7, 1.0),
            float(router, 9, INPUT_RETURN_GAIN),
            float(router, 8, DEFAULT_LEVEL),
            int(router, 5, 1),
        ]
    );
    let tuned = SidechainContext {
        tuner_input: Some(3),
        ..context(&lanes, true)
    };
    let mut out = Vec::new();
    Sidechains::new().push(&[sink], &tuned, &mut out);
    assert_eq!(out[1], int(forward, 1, 2));
    assert_eq!(out[3..5], [int(router, 0, 0), int(router, 1, 0)]);
}

/// Outside bulk mode a change from one block source to another silences the
/// sink router, syncs every core and waits 25 ms before the new route; the
/// first route and bulk pushes do not.
#[test]
fn source_change_outside_bulk_silences_and_settles_first() {
    let lanes = [lane(1, 1), lane(0, 0), lane(0, 0), lane(0, 0)];
    let router = at(DspCore::Core0, 4);
    let block = |column| SidechainSource::Block {
        cell: GridCell::new(0, column).unwrap(),
        placement: Some(at(DspCore::Core0, u32::from(column))),
    };
    let mut state = Sidechains::new();
    let live = context(&lanes, false);
    let mut out = Vec::new();
    state.route(
        &sink(0, router, block(0)),
        RouteMode::Source,
        &live,
        &mut out,
    );
    assert!(matches!(out[0], DspWrite::Request(_)));
    let mut out = Vec::new();
    state.route(
        &sink(0, router, block(1)),
        RouteMode::Source,
        &live,
        &mut out,
    );
    assert_eq!(
        out[..7],
        [
            int(router, 2, 0),
            int(router, 0, 0),
            int(router, 1, 0),
            float(router, 7, 1.0),
            float(router, 9, 1.0),
            float(router, 8, 0.25),
            DspWrite::SyncAllCores {
                settle: Duration::from_millis(25),
            },
        ]
    );
    let mut out = Vec::new();
    state.push(
        &[sink(0, router, block(2))],
        &context(&lanes, true),
        &mut out,
    );
    assert!(matches!(out[0], DspWrite::Request(_)));
}

/// VA 0x2e186c: row 1 follows lane 0 behind a splitter; rows 2/3 fed by an
/// upper `NEXT_ROW` output follow that lane; row 0 past the merge follows
/// lane 1 when lane 0 has no input.
#[test]
fn follow_input_resolves_the_feeding_lane() {
    let split = PairJunction {
        split: Some(2),
        merge: Some(5),
    };
    let lanes = [lane(0, 16), lane(9, 0), lane(4, 0), lane(5, 0)];
    let context = SidechainContext {
        pairs: [split, PairJunction::default()],
        ..context(&lanes, true)
    };
    assert_eq!(followed_lane(&context, 1, 0), 0);
    assert_eq!(followed_lane(&context, 0, 6), 1);
    assert_eq!(followed_lane(&context, 0, 5), 0);
    // Row 2 is fed by lane 0 (output 16); row 0 at column 7 is past the
    // merge and lane 0 has no input, so lane 1.
    assert_eq!(followed_lane(&context, 2, 0), 1);
    assert_eq!(followed_lane(&context, 3, 0), 3);
}

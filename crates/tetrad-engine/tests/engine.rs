use std::collections::BTreeMap;
use tetrad_engine::{
    Block, Column, Error, GainDb, Grid, Junction, LaneGains, LanePorts, ModelId, OpaquePolicy,
    Pair, PairRoute, PortId, Program, Row, SceneIndex, Scened, Slot, Step, StepValue,
};

const IN: PortId = PortId(1);
const OUT: PortId = PortId(2);
const OUT_B: PortId = PortId(3);

fn slot(row: u8, column: u8) -> Slot {
    Slot {
        row: Row::new(row).unwrap(),
        column: Column::new(column).unwrap(),
    }
}

fn db(value: f32) -> GainDb {
    GainDb::new(value).unwrap()
}

fn scene(index: u8) -> SceneIndex {
    SceneIndex::new(index).unwrap()
}

fn junction(column: u8, upper: f32, lower: f32) -> Junction {
    Junction {
        column: Column::new(column).unwrap(),
        gains: Scened::Fixed(LaneGains {
            upper: db(upper),
            lower: db(lower),
        }),
    }
}

/// Upper pair: row 0 reads IN and writes OUT; rows 0/1 split at `split`, merge at `merge`.
fn upper_pair(split: u8, merge: Option<u8>) -> Grid {
    let mut grid = Grid::new();
    grid.set_ports(
        Row::new(0).unwrap(),
        LanePorts {
            input: Some(IN),
            output: Some(OUT),
        },
    );
    grid.set_route(
        Pair::Upper,
        PairRoute {
            split: Some(junction(split, 0.0, 0.0)),
            merge: merge.map(|column| junction(column, 0.0, 0.0)),
        },
    );
    grid
}

fn render(grid: &Grid, scene_index: u8, policy: OpaquePolicy) -> BTreeMap<PortId, Vec<f32>> {
    let inputs = BTreeMap::from([(IN, vec![1.0, -0.5])]);
    grid.compile()
        .unwrap()
        .render(scene(scene_index), &inputs, 2, policy)
        .unwrap()
}

fn close(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
}

/// Pins the schedule contract the app and a future DSP backend rely on: split before
/// its column, every slave block after the master prefix whatever its column (stock
/// spec builder `0x5ed844`), merge after its column.
#[test]
fn schedule_order_is_deterministic() {
    let mut grid = upper_pair(2, Some(5));
    for (row, column) in [
        (0, 0),
        (0, 2),
        (0, 5),
        (0, 6),
        (1, 0),
        (1, 2),
        (1, 5),
        (1, 7),
    ] {
        grid.set_block(
            slot(row, column),
            Some(Block::model(ModelId(column.into()))),
        );
    }
    let order: Vec<String> = grid
        .compile()
        .unwrap()
        .steps()
        .iter()
        .map(|step| match step {
            Step::Load { row, .. } => format!("load{}", row.index()),
            Step::Split { .. } => "split".into(),
            Step::Process { slot, .. } => format!("{}.{}", slot.row.index(), slot.column.index()),
            Step::Merge { .. } => "merge".into(),
            Step::Store { row, .. } => format!("store{}", row.index()),
        })
        .collect();
    assert_eq!(
        order,
        [
            "load0", "0.0", "split", "0.2", "0.5", "1.0", "1.2", "1.5", "1.7", "merge", "0.6",
            "store0",
            // Lower pair is idle: silent master and slave loads, no stores.
            "load2", "load3",
        ]
    );
    assert_eq!(grid.compile(), grid.compile());
}

#[test]
fn invalid_routing_is_rejected() {
    let mut grid = upper_pair(5, Some(2));
    assert!(matches!(
        grid.compile(),
        Err(Error::SplitAfterMerge {
            split: 5,
            merge: 2,
            ..
        })
    ));

    grid = upper_pair(2, Some(5));
    let slave = LanePorts {
        input: Some(IN),
        output: None,
    };
    grid.set_ports(Row::new(1).unwrap(), slave);
    assert_eq!(grid.compile(), Err(Error::SlaveInputShadowed(Pair::Upper)));

    grid = upper_pair(2, Some(5));
    let slave = LanePorts {
        input: None,
        output: Some(OUT_B),
    };
    grid.set_ports(Row::new(1).unwrap(), slave);
    assert_eq!(grid.compile(), Err(Error::SlaveOutputShadowed(Pair::Upper)));

    assert_eq!(Row::new(4), Err(Error::Row(4)));
    assert_eq!(Column::new(8), Err(Error::Column(8)));
    assert_eq!(SceneIndex::new(8), Err(Error::Scene(8)));
    assert!(GainDb::new(f32::NAN).is_err());
}

#[test]
fn split_and_merge_mix_lanes() {
    let mut grid = upper_pair(0, Some(7));
    grid.set_route(
        Pair::Upper,
        PairRoute {
            split: Some(junction(0, 0.0, -6.0)),
            merge: Some(junction(7, -6.0, 0.0)),
        },
    );
    grid.set_block(slot(1, 3), Some(Block::gain(db(20.0))));
    let out = &render(&grid, 0, OpaquePolicy::Reject)[&OUT];
    // master: x * 1 * g(-6); slave: x * g(-6) * 10; merged sum.
    let gain = 10f32.powf(-6.0 / 20.0);
    close(out[0], gain + gain * 10.0);
    close(out[1], -0.5 * (gain + gain * 10.0));
}

#[test]
fn split_without_merge_drives_separate_output() {
    let mut grid = upper_pair(0, None);
    let slave = LanePorts {
        input: None,
        output: Some(OUT_B),
    };
    grid.set_ports(Row::new(1).unwrap(), slave);
    grid.set_block(slot(1, 4), Some(Block::gain(db(-20.0))));
    let outputs = render(&grid, 0, OpaquePolicy::Reject);
    close(outputs[&OUT][0], 1.0);
    close(outputs[&OUT_B][0], 0.1);
}

#[test]
fn scene_transition_lists_only_changed_values() {
    let mut grid = upper_pair(0, Some(7));
    let mut bypassed = Block::gain(db(-20.0));
    bypassed.bypass.set(scene(3), true);
    grid.set_block(slot(0, 1), Some(bypassed));
    let program = grid.compile().unwrap();

    assert!(program.transition(scene(0), scene(1)).is_empty());
    let changes = program.transition(scene(0), scene(3));
    assert_eq!(changes.len(), 1);
    assert!(matches!(
        program.steps()[changes[0].step],
        Step::Process { slot: s, .. } if s == slot(0, 1)
    ));
    assert_eq!(
        changes[0].value,
        StepValue::Block {
            bypassed: true,
            gain: Some(db(-20.0)),
        }
    );
    // Unity split/merge: output is the gained master lane plus the untouched slave lane.
    close(render(&grid, 0, OpaquePolicy::Reject)[&OUT][0], 1.1);
    close(render(&grid, 3, OpaquePolicy::Reject)[&OUT][0], 2.0);
}

#[test]
fn opaque_models_need_an_explicit_policy() {
    let mut grid = upper_pair(0, Some(7));
    grid.set_block(slot(0, 0), Some(Block::model(ModelId(6004))));
    let program = grid.compile().unwrap();
    let inputs = BTreeMap::from([(IN, vec![1.0])]);
    assert_eq!(
        program.render(scene(0), &inputs, 1, OpaquePolicy::Reject),
        Err(Error::OpaqueModel {
            slot: slot(0, 0),
            model: ModelId(6004),
        })
    );
    let out = program
        .render(scene(0), &inputs, 1, OpaquePolicy::PassThrough)
        .unwrap();
    close(out[&OUT][0], 2.0);
    assert!(matches!(
        program.render(scene(0), &inputs, 2, OpaquePolicy::PassThrough),
        Err(Error::FrameMismatch {
            expected: 2,
            actual: 1,
            ..
        })
    ));
}

/// Pins the render domain: frame ceiling before allocation, finite inputs only,
/// overflowing outputs rejected, and caller inputs untouched by a failure.
#[test]
fn render_rejects_samples_outside_the_finite_domain() {
    let mut grid = upper_pair(0, Some(7));
    grid.set_block(slot(0, 0), Some(Block::gain(db(GainDb::MAX))));
    let program = grid.compile().unwrap();
    let render = |inputs: &BTreeMap<PortId, Vec<f32>>, frames| {
        program.render(scene(0), inputs, frames, OpaquePolicy::Reject)
    };

    let too_long = Program::MAX_FRAMES + 1;
    assert_eq!(
        render(&BTreeMap::new(), too_long),
        Err(Error::FrameLimit {
            frames: too_long,
            max: Program::MAX_FRAMES,
        })
    );

    let nan = BTreeMap::from([(IN, vec![0.0, f32::NAN])]);
    assert_eq!(
        render(&nan, 2),
        Err(Error::NonFiniteInput { port: IN, frame: 1 })
    );

    let loud = BTreeMap::from([(IN, vec![0.5, f32::MAX])]);
    let before = loud.clone();
    assert_eq!(
        render(&loud, 2),
        Err(Error::NonFiniteOutput {
            port: OUT,
            frame: 1
        })
    );
    assert_eq!(loud, before);
}

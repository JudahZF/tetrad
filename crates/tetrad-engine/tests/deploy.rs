//! Deployment compiler contracts on a small synthetic catalog (no stock data).

use std::collections::BTreeSet;

use tetrad_catalog::{Catalog, Specs};
use tetrad_engine::{
    Channel, CompileError, CompileOptions, DeployError, Deployment, DropSide, DspCore, Endpoint,
    FitError, GateMessage, Globals, InputPort, Instance, LaneSink, LaneSource, Layout, LinkKind,
    ModelId, Node, OutputPort, Pair, PlanOp, RouteError, RouterValue, RouterWrite, Row, SceneIndex,
    Scened, SidechainSource, Site, Slot, StoredMasks, TextParam, Update, compile, route,
};

const MODELS: &str = "<Models>\
<Category id='0' name='Drive'>\
<Model id='1' name='Light' blob='light'><Parameter name='GAIN' type='float' min='0' max='10' defaultValue='5'/></Model>\
<Model id='2' name='Heavy' blob='heavy'><Parameter name='GAIN' type='float' min='0' max='10' defaultValue='5'/></Model>\
<Model id='3' name='Big' blob='big'><Parameter name='GAIN' type='float' min='0' max='10' defaultValue='5'/></Model>\
</Category>\
<Category id='5' name='Compressor'>\
<Model id='5018' name='Comp' blob='comp' sidechain='1'><Parameter name='THRESHOLD' type='float' min='-48' max='0' defaultValue='-40'/><Parameter name='SOURCE' type='rotarySwitch' min='0' max='44' defaultValue='0' dynamic='true' stepNames='Off'/></Model>\
</Category>\
<Category id='10' name='Splitter' hidden='true'><Model id='10004' name='Splitter'><Parameter name='TYPE' type='switch' min='0' max='2' steps='3' defaultValue='0'/><Parameter name='STEREO' type='switch' min='0' max='1' defaultValue='0'/><Parameter name='BALANCE' type='float' min='0' max='10' defaultValue='5'/><Parameter name='LEVEL TO A' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/><Parameter name='LEVEL TO B' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/><Parameter name='FREQUENCY' type='float' min='20' max='20000' defaultValue='400'/><Parameter name='MODE' type='switch' min='0' max='1' defaultValue='0'/></Model><Model id='10003' name='LR Crossover' blob='lr' hidden='true'><Parameter name='FREQ' type='float' min='20' max='20000' defaultValue='20'/><Parameter name='MODE' type='switch' min='0' max='1' defaultValue='0'/></Model></Category>\
<Category id='11' name='Mixer' hidden='true'><Model id='11000' name='Mixer'><Parameter name='LEVEL A' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/><Parameter name='PAN A' type='float' min='0' max='10' defaultValue='5'/><Parameter name='LEVEL B' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/></Model></Category>\
<Category id='28' name='InputGateControl'><Model id='28000' name='Input Gate Control' internal='true'><Parameter name='NOISE REDUCTION' type='float' min='0' max='100' defaultValue='30'/><Parameter name='BYPASS' type='switch' min='0' max='1' steps='2' defaultValue='1'/><Parameter name='GAIN REDUCTION' type='grMeter' min='-40' max='0' defaultValue='-40'/><Parameter name='INPUT GAIN' type='float' min='-24' max='24' defaultValue='0'/></Model></Category>\
</Models>";

const REPORT: &str = "<ModuleReport>\
<Kernel core='0' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='1' cpu='2' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='2' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='3' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Module id='1' name='Light' blob_name='light' category='0' dm_heap='100' pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'><Test name='Stimulus High' avg='10'/></Module>\
<Module id='2' name='Heavy' blob_name='heavy' category='0' dm_heap='100' pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'><Test name='Stimulus High' avg='95'/></Module>\
<Module id='3' name='Big' blob_name='big' category='0' dm_heap='100' pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'><Test name='Stimulus High' avg='80'/></Module>\
<Module id='5018' name='Comp' blob_name='comp' category='5' dm_heap='100' pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'><Test name='Stimulus High' avg='5'/></Module>\
<Module id='10003' name='LR Crossover' blob_name='lr' category='10' dm_heap='100' pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'><Test name='Stimulus High' avg='1'/></Module>\
</ModuleReport>";

const LIGHT: ModelId = ModelId(1);
const HEAVY: ModelId = ModelId(2);
const BIG: ModelId = ModelId(3);
const COMP: ModelId = ModelId(5018);

fn specs() -> Specs {
    Specs::resolve(&Catalog::parse(MODELS, REPORT).unwrap()).unwrap()
}

fn slot(row: u8, column: u8) -> Slot {
    Slot {
        row: Row::new(row).unwrap(),
        column: tetrad_engine::Column::new(column).unwrap(),
    }
}

fn options(input_gate: bool, global_eq: bool) -> CompileOptions {
    CompileOptions {
        globals: Globals {
            input_gate,
            global_eq,
        },
        ddc: true,
    }
}

/// Row `row` reads Input 1 and writes XLR 1/2, with `blocks` in its columns.
fn layout(specs: &Specs, rows: &[(u8, &[(u8, ModelId)])]) -> Layout {
    let mut layout = Layout::default();
    for &(row, blocks) in rows {
        let lane = layout.lane_mut(Row::new(row).unwrap());
        lane.input = InputPort::Input1;
        lane.output = OutputPort::Xlr1And2;
        for &(column, model) in blocks {
            layout.add_block(slot(row, column), model, specs).unwrap();
        }
    }
    layout
}

/// Recovered level-0 rule and run list (`0x87fc2c`): a light SOC1 preset runs
/// on core 1, the I/O hub; its row gets a receive, the blocks, an output delay
/// and a send, and each empty row a core-1 forward. Blocks on one core share
/// the lane's audio buffer (the layout word) and a same-name blob load.
#[test]
fn light_upper_preset_runs_on_the_io_core() {
    let specs = specs();
    let layout = layout(&specs, &[(0, &[(2, LIGHT), (3, LIGHT)])]);
    let plan = compile(&layout, &specs, options(true, true)).unwrap();
    assert_eq!(plan.cores[1].budget.cpu, 94.0);
    let nodes: Vec<Node> = plan.cores[1].slots.iter().map(|s| s.node).collect();
    let block = Node::Model(Site::Slot(slot(0, 2)));
    let rows = [0, 1, 2, 3].map(|row| Row::new(row).unwrap());
    assert_eq!(
        nodes,
        [
            Node::Receive(Endpoint::LaneInput(rows[0])),
            block,
            Node::Model(Site::Slot(slot(0, 3))),
            Node::Delay(Endpoint::OutputDelay(rows[0])),
            Node::Send(Endpoint::LaneOutput(rows[0])),
            Node::Forward(Endpoint::LaneThrough(rows[1])),
            Node::Forward(Endpoint::LaneThrough(rows[2])),
            Node::Forward(Endpoint::LaneThrough(rows[3])),
        ]
    );
    assert!(plan.cores[0].slots.is_empty() && plan.links.is_empty());
    assert_eq!(plan.gate_core, Some(DspCore::IO));
    let layouts: Vec<u16> = plan.cores[1].slots.iter().map(|s| s.layout).collect();
    assert_eq!(layouts, [1, 1, 1, 1, 1, 0, 0, 0]);
    assert_eq!(plan.cores[1].blobs.len(), 1);
    // Normalized default (5 of 0..10), then MUTE and BYPASS.
    let deployed = plan.slot(plan.placement(block).unwrap()).unwrap();
    assert_eq!(
        deployed.values,
        [Scened::Fixed(0.5), Scened::Fixed(0.0), Scened::Fixed(0.0)]
    );
    assert_eq!(deployed.skipped, [1, 2]);
    assert_eq!(plan.lane_latency[0][0], 32);
    // `0x87d3b4`: return gain 1.0, then Physical / Input 1, stereo.
    let receive: Vec<(u32, RouterValue)> = plan.cores[1].slots[0]
        .setup
        .iter()
        .map(|write| (write.index, write.value))
        .collect();
    assert_eq!(
        receive,
        [
            (9, RouterValue::Float(1.0)),
            (0, RouterValue::Int(Channel::Physical as i32)),
            (1, RouterValue::Int(1)),
            (2, RouterValue::Int(1)),
        ]
    );
}

/// A pair too heavy for one core spreads over its SOC with a Dual Core link;
/// a layout no mask can fit is refused, never truncated.
#[test]
fn heavy_pair_spreads_over_both_cores_and_overflow_is_refused() {
    let specs = specs();
    let layout2 = layout(&specs, &[(0, &[(0, HEAVY), (1, HEAVY)])]);
    let plan = compile(&layout2, &specs, options(false, false)).unwrap();
    let cores: Vec<_> = [slot(0, 0), slot(0, 1)]
        .map(|s| plan.placement(Node::Model(Site::Slot(s))).unwrap().core)
        .to_vec();
    assert_ne!(cores[0], cores[1]);
    assert!(
        plan.links
            .iter()
            .any(|link| link.kind == LinkKind::DualCore)
    );
    for core in &plan.cores {
        assert!(core.usage.fits(&core.budget), "{core:?}");
    }
    // Within the SOC pre-check (190%) but no split fits both cores.
    let tight = layout(&specs, &[(0, &[(0, HEAVY), (1, HEAVY), (2, LIGHT)])]);
    assert!(matches!(
        compile(&tight, &specs, options(false, false)),
        Err(CompileError::Fit(FitError::Cores(Pair::Upper)))
    ));
    let over = layout(&specs, &[(0, &[(0, HEAVY), (1, HEAVY), (2, HEAVY)])]);
    assert!(matches!(
        compile(&over, &specs, options(false, false)),
        Err(CompileError::Fit(FitError::Soc {
            pair: Pair::Upper,
            ..
        }))
    ));
}

/// `CompilerInhibitedModules`: when the preset fills both SOC1 cores, the
/// Global EQ and the Input Gate are switched off instead of refusing it.
#[test]
fn globals_are_dropped_when_soc1_is_full() {
    let specs = specs();
    let full = layout(&specs, &[(0, &[(0, HEAVY)]), (1, &[(0, HEAVY)])]);
    let plan = compile(&full, &specs, options(true, true)).unwrap();
    assert!(plan.inhibited.global_eq && plan.inhibited.input_gate);
    assert_eq!(plan.gate_core, None);
    let light = layout(&specs, &[(0, &[(0, LIGHT)])]);
    let plan = compile(&light, &specs, options(true, true)).unwrap();
    assert!(!plan.inhibited.global_eq && !plan.inhibited.input_gate);
}

/// SOC2 rows read their port through a core-1 forward and a SlaveLink
/// (`0x87f99c`) and send their output to a core-1 forward (`0x87e668`); each
/// hop adds 32 samples.
#[test]
fn soc2_rows_link_input_and_output_through_soc1() {
    let specs = specs();
    let layout = layout(&specs, &[(2, &[(0, LIGHT)])]);
    let plan = compile(&layout, &specs, options(false, false)).unwrap();
    let row = Row::new(2).unwrap();
    let input = plan.routers.lane_input[2].unwrap();
    assert_eq!(input.core, DspCore::IO);
    assert_eq!(plan.slot(input).unwrap().model, ModelId(22_002));
    let receive = plan
        .placement(Node::Receive(Endpoint::LaneInput(row)))
        .unwrap();
    let block = plan.placement(Node::Model(Site::Slot(slot(2, 0)))).unwrap();
    assert_eq!(
        (receive.core, block.core),
        (DspCore::ALL[3], DspCore::ALL[3])
    );
    let output = plan.routers.lane_output[2].unwrap();
    assert_eq!(output, plan.routers.output_delay[2].unwrap());
    assert_eq!(plan.slot(output).unwrap().model, ModelId(22_002));
    let slave_links = plan
        .links
        .iter()
        .filter(|link| link.kind == LinkKind::SlaveLink)
        .count();
    assert_eq!(slave_links, 2);
    assert_eq!(plan.lane_latency[2][0], 16 + 2 * 32);
}

/// `ProgramData::parameterSceneMode`: an edit writes all eight scenes unless
/// the parameter (or bypass) is in scene mode; then only the current scene.
#[test]
fn edits_follow_the_scene_mode_flag() {
    let (scene0, scene5) = (SceneIndex::new(0).unwrap(), SceneIndex::new(5).unwrap());
    let mut instance = Instance::new(LIGHT, vec![Scened::Fixed(0.5)]);
    assert!(instance.set_param(0, scene5, 0.1));
    assert_eq!(instance.params[0], Scened::Fixed(0.1));
    instance.scene_mode[0] = true;
    instance.bypass_scene_mode = true;
    assert!(instance.set_param(0, scene5, 0.9));
    instance.set_bypass(scene5, true);
    assert_eq!(
        (instance.params[0].at(scene0), instance.params[0].at(scene5)),
        (0.1, 0.9)
    );
    assert_eq!(
        (instance.bypass.at(scene0), instance.bypass.at(scene5)),
        (false, true)
    );
    assert!(!instance.set_param(1, scene5, 0.0));
    // String parameters take the same rule; a single stored text holds for every scene.
    instance.texts = vec![TextParam {
        index: 0,
        scenes: vec![Some("a".into())],
    }];
    assert!(instance.set_text(0, scene5, "b"));
    let text = |scene: SceneIndex| instance.texts[0].scenes[scene.index()].as_deref();
    assert_eq!((text(scene0), text(scene5)), (Some("a"), Some("b")));
    assert!(!instance.set_text(1, scene5, "c"));
}

/// Selector steps: Off, Follow Input, 12 external inputs, then preceding blocks
/// (`0x585b2c`, `0x52e390`); the stored value is normalised over that list.
#[test]
fn sidechain_selectors_name_preceding_blocks() {
    let specs = specs();
    let mut layout = layout(&specs, &[(0, &[(1, LIGHT), (3, COMP)])]);
    let comp = Site::Slot(slot(0, 3));
    assert_eq!(tetrad_engine::sidechain_steps(&layout, slot(0, 3)), 15);
    let mut select = |value: f32| {
        layout.instance_mut(comp).unwrap().params[1] = Scened::Fixed(value);
        route(&layout, &specs).map(|graph| graph.sidechains[0].scenes[0])
    };
    assert_eq!(select(14.0 / 14.0), Ok(SidechainSource::Block(slot(0, 1))));
    assert_eq!(
        select(2.0 / 14.0),
        Ok(SidechainSource::External(InputPort::Input1))
    );
    assert_eq!(select(1.0 / 14.0), Ok(SidechainSource::FollowInput));
    assert_eq!(select(0.0), Ok(SidechainSource::Off));
}

/// `0x87e90c`: each SOC counts its own sink channels; a SOC2 sink's receive
/// router reads the SlaveLink channel that the plan keeps for the route step.
#[test]
fn sidechain_plans_keep_base_and_slave_channels() {
    let specs = specs();
    let layout = layout(&specs, &[(0, &[(1, COMP)]), (2, &[(1, COMP)])]);
    let plan = compile(&layout, &specs, options(false, false)).unwrap();
    let [upper, lower] = [0, 2].map(|row| {
        *plan
            .sidechains
            .iter()
            .find(|sink| sink.site == slot(row, 1))
            .unwrap()
    });
    assert_eq!((upper.channel, upper.slave_channel), (0, None));
    assert_eq!(lower.channel, 0);
    let slave = lower.slave_channel.unwrap();
    let router = setup(&plan, lower.router.unwrap());
    assert!(router.contains(&(0, RouterValue::Int(Channel::SlaveLink as i32))));
    assert!(router.contains(&(1, RouterValue::Int(slave as i32))));
}

#[test]
fn routing_rejects_illegal_junctions_and_ports() {
    let specs = specs();
    let base = layout(&specs, &[(0, &[(0, LIGHT)])]);
    let column = |c| Some(tetrad_engine::Column::new(c).unwrap());

    let mut layout = base.clone();
    layout.pair_mut(Pair::Upper).split = column(2);
    assert_eq!(
        route(&layout, &specs),
        Err(RouteError::SplitterMissing(Pair::Upper))
    );
    let splitter = tetrad_engine::new_instance(specs.model(10_004).unwrap());
    layout.pair_mut(Pair::Upper).splitter = Some(splitter);
    layout.pair_mut(Pair::Upper).merge = column(1);
    layout.pair_mut(Pair::Upper).mixer =
        Some(tetrad_engine::new_instance(specs.model(11_000).unwrap()));
    assert!(matches!(
        route(&layout, &specs),
        Err(RouteError::SplitAfterMerge { .. })
    ));

    let mut layout = base.clone();
    layout.lane_mut(Row::new(2).unwrap()).output = OutputPort::NextRow4;
    assert!(matches!(
        route(&layout, &specs),
        Err(RouteError::NextRowFromLowerPair { .. })
    ));
    let mut layout = base;
    layout.lane_mut(Row::new(3).unwrap()).input = InputPort::PrevRow;
    assert!(matches!(
        route(&layout, &specs),
        Err(RouteError::PrevRowUnfed(_))
    ));
}

/// Stock update rules: parameters and bypass are incremental, scenes re-push
/// every value, structural edits redeploy, an identical preset is reused and a
/// failed edit keeps the deployed preset.
#[test]
fn deployment_updates_follow_stock_rules() {
    let specs = specs();
    let scene0 = SceneIndex::new(0).unwrap();
    let block = Site::Slot(slot(0, 0));
    let (mut deployment, initial) = Deployment::new(
        layout(&specs, &[(0, &[(0, LIGHT)])]),
        &specs,
        options(false, false),
        scene0,
    )
    .unwrap();
    assert!(matches!(initial, Update::Redeploy { ref clear, .. } if clear.is_empty()));
    let at = deployment.plan().placement(Node::Model(block)).unwrap();

    let update = deployment.set_parameter(&specs, block, 0, 0.25).unwrap();
    assert_eq!(
        update,
        Update::Ops(vec![PlanOp::SetParam {
            at,
            index: 0,
            value: 0.25
        }])
    );
    let Update::Ops(ops) = deployment.set_bypass(&specs, block, true).unwrap() else {
        panic!("bypass is incremental");
    };
    assert_eq!(ops[0], PlanOp::SetBypass { at, bypassed: true });
    // DDC: the bypassed block no longer adds its 16 samples.
    assert_eq!(deployment.plan().lane_latency[0][0], 0);

    // Not in scene mode: the edits reach every scene (`0x51e4d8`).
    let Update::Ops(ops) = deployment.select_scene(SceneIndex::new(3).unwrap()) else {
        panic!("scene change is incremental");
    };
    assert!(ops.contains(&PlanOp::SetBypass { at, bypassed: true }));
    assert!(ops.contains(&PlanOp::SetParam {
        at,
        index: 0,
        value: 0.25
    }));

    let update = deployment
        .edit(&specs, |layout| layout.add_block(slot(0, 1), LIGHT, &specs))
        .unwrap();
    assert!(matches!(update, Update::Redeploy { ref clear, .. } if clear == &[DspCore::IO]));
    let same = deployment.layout().clone();
    assert!(matches!(
        deployment.load(&specs, same),
        Ok(Update::Reuse(_))
    ));

    let before = deployment.layout().clone();
    let overflow = deployment.edit(&specs, |layout| {
        for column in 2..5 {
            layout.add_block(slot(0, column), HEAVY, &specs)?;
        }
        Ok(())
    });
    assert!(overflow.is_err());
    assert_eq!(deployment.layout(), &before);
}

/// (O) Issue #17: a dormant block stays in the layout, loads nothing and takes its edits
/// without a DSP op. A load where it stops being dormant redeploys, so the DSP gets it.
#[test]
fn dormant_blocks_stay_in_the_layout_and_load_nothing() {
    let specs = specs();
    let scene0 = SceneIndex::new(0).unwrap();
    let dormant = Site::Slot(slot(0, 1));
    let preset = layout(&specs, &[(0, &[(0, LIGHT), (1, BIG)])]);
    let (mut deployment, _) = Deployment::new_stored(
        preset.clone(),
        &specs,
        options(false, false),
        scene0,
        StoredMasks::default(),
        BTreeSet::from([BIG]),
    )
    .unwrap();
    assert_eq!(deployment.layout(), &preset);
    assert!(deployment.dsp_layout().instance(dormant).is_none());
    assert!(deployment.plan().block(slot(0, 1)).is_none());
    assert!(deployment.plan().block(slot(0, 0)).is_some());

    let update = deployment.set_parameter(&specs, dormant, 0, 0.25).unwrap();
    assert_eq!(update, Update::Ops(Vec::new()));
    assert_eq!(
        deployment.layout().instance(dormant).unwrap().params[0],
        Scened::Fixed(0.25)
    );
    let update = deployment.set_bypass(&specs, dormant, true).unwrap();
    assert_eq!(update, Update::Ops(Vec::new()));

    // The DSP layout is unchanged, so a reload with BIG still dormant reuses the models.
    let edited = deployment.layout().clone();
    let update = deployment.load_stored(
        &specs,
        edited.clone(),
        StoredMasks::default(),
        BTreeSet::from([BIG]),
    );
    assert!(matches!(update, Ok(Update::Reuse(_))));
    let update = deployment.load_stored(&specs, edited, StoredMasks::default(), BTreeSet::new());
    assert!(matches!(update, Ok(Update::Redeploy { .. })));
    assert!(deployment.plan().block(slot(0, 1)).is_some());
}

/// Block MUTE (`0x51e4d8` at the MUTE index): the DSP takes MUTE in the bypass record, so
/// the edit re-sends the bypass, only when MUTE at the current scene changes; in scene mode
/// only the current scene changes.
#[test]
fn mute_edit_resends_the_bypass_under_the_scene_rule() {
    let specs = specs();
    let (scene0, scene3) = (SceneIndex::new(0).unwrap(), SceneIndex::new(3).unwrap());
    let block = slot(0, 0);
    let (mut deployment, _) = Deployment::new(
        layout(&specs, &[(0, &[(0, LIGHT)])]),
        &specs,
        options(false, false),
        scene0,
    )
    .unwrap();
    let at = deployment
        .plan()
        .placement(Node::Model(Site::Slot(block)))
        .unwrap();
    let update = deployment.set_mute(&specs, block, true, true).unwrap();
    assert_eq!(
        update,
        Update::Ops(vec![PlanOp::SetBypass {
            at,
            bypassed: false
        }])
    );
    // Light has one XML parameter: values are GAIN, MUTE, BYPASS.
    let mute = &deployment.plan().slot(at).unwrap().values[1];
    assert_eq!((mute.at(scene0), mute.at(scene3)), (1.0, 0.0));
    let unchanged = deployment.set_mute(&specs, block, true, false).unwrap();
    assert_eq!(unchanged, Update::Ops(Vec::new()));
    let instance = deployment.layout().instance(Site::Slot(block)).unwrap();
    assert_eq!(instance.mute, Scened::Fixed(true));
}

/// `0x5f1330` scores every level-1 candidate and takes the best: Big (80%)
/// then Light cannot share core 1 with the globals. "Big on core 1" passes the
/// threshold first, but "Big on core 0, Light on core 1" is better balanced
/// and keeps the output on core 1, so it wins.
#[test]
fn level_one_takes_the_best_score_not_the_first_acceptable() {
    let specs = specs();
    let layout = layout(&specs, &[(0, &[(0, BIG), (1, LIGHT)])]);
    let fit = tetrad_engine::fit(&layout, &specs, options(true, true).globals, None).unwrap();
    assert_eq!(fit.cores[0], [DspCore::ALL[0], DspCore::IO]);
    assert_eq!(fit.gate_core, Some(DspCore::IO));
    assert!(fit.scores[0] > -2.0 && fit.scores[0] < -1.0);
}

/// Row 0 splits at column 0 and merges at column 3 around a 16-sample block;
/// row 1 holds only its 10003 head. Everything runs on core 1.
fn split_pair(specs: &Specs) -> Layout {
    let mut layout = layout(specs, &[(0, &[(1, LIGHT)])]);
    let pair = layout.pair_mut(Pair::Upper);
    pair.split = Some(tetrad_engine::Column::new(0).unwrap());
    pair.merge = Some(tetrad_engine::Column::new(3).unwrap());
    pair.splitter = Some(tetrad_engine::new_instance(specs.model(10_004).unwrap()));
    pair.mixer = Some(tetrad_engine::new_instance(specs.model(11_000).unwrap()));
    layout
}

fn setup(
    plan: &tetrad_engine::DeploymentPlan,
    at: tetrad_engine::Placement,
) -> Vec<(u32, RouterValue)> {
    let slot = plan.slot(at).unwrap();
    slot.setup.iter().map(|w| (w.index, w.value)).collect()
}

/// `0x87ebc4`, `0x87ddd0`: splitter and mixer take no slot. The split router
/// copies the master buffer to the slave head's buffer (channel 4) and the
/// merge router mixes it back with thru gain 1, each side behind a merge
/// delay. Junction edits write these routers (`0x582278`, `0x581f98`); a
/// bypassed mixer mutes LEVEL B (`0x514378`); the heads run only for a
/// crossover (`0x5821a4`).
#[test]
fn split_and_merge_run_as_routers() {
    let specs = specs();
    let scene0 = SceneIndex::new(0).unwrap();
    let (mut deployment, _) =
        Deployment::new(split_pair(&specs), &specs, options(false, false), scene0).unwrap();
    let plan = deployment.plan().clone();
    assert!(
        plan.deployed()
            .all(|slot| !matches!(slot.model.0, 10_004 | 11_000))
    );
    let split = plan.routers.split[0].unwrap();
    let merge = plan.routers.merge[0].unwrap();
    let slave_head = plan.routers.heads[1].unwrap();
    let b_buffer = plan.slot(slave_head).unwrap().layout;
    assert_ne!(b_buffer, plan.slot(split).unwrap().layout);
    assert_eq!(
        setup(&plan, split)[2..4],
        [
            (0, RouterValue::Int(4)),
            (1, RouterValue::Int(b_buffer.into()))
        ]
    );
    assert_eq!(
        setup(&plan, merge),
        [
            (9, RouterValue::Float(1.0)),
            (0, RouterValue::Int(4)),
            (1, RouterValue::Int(b_buffer.into())),
            (2, RouterValue::Int(1)),
            (7, RouterValue::Float(1.0)),
        ]
    );
    assert!(plan.routers.merge_delay[0].iter().all(Option::is_some));
    // TYPE Balance: both heads bypassed in every push.
    assert!(plan.slot(slave_head).unwrap().bypass.at(scene0));

    // Junction values stay normalized; the sender converts them (`0x582278`).
    let splitter = Site::Splitter(Pair::Upper);
    let update = deployment.set_parameter(&specs, splitter, 2, 0.25).unwrap();
    assert_eq!(
        update,
        Update::Ops(vec![PlanOp::Junction {
            site: splitter,
            index: 2
        }])
    );
    let mixer = Site::Mixer(Pair::Upper);
    let Update::Ops(ops) = deployment.set_bypass(&specs, mixer, true).unwrap() else {
        panic!("mixer bypass is incremental");
    };
    // The mixer's BYPASS (3 parameters, MUTE, BYPASS) rewrites LEVEL B.
    assert_eq!(
        ops[0],
        PlanOp::Junction {
            site: mixer,
            index: 4
        }
    );
    let pushed = tetrad_engine::push_ops(deployment.plan(), deployment.layout(), scene0);
    assert_eq!(pushed[0], PlanOp::PushJunctions { pair: Pair::Upper });
}

/// `0x87c11c` delays the shorter side of a merge (the slave by the master
/// window's 16 samples); `0x87c958` then aligns every row output to the
/// longest row, here the empty lower rows' forwards (delay parameter 9).
#[test]
fn merges_and_outputs_are_aligned() {
    let specs = specs();
    let plan = compile(&split_pair(&specs), &specs, options(false, false)).unwrap();
    let [master, slave] = plan.routers.merge_delay[0].map(Option::unwrap);
    let int = |at, index, value| RouterWrite {
        at,
        index,
        value: RouterValue::Int(value),
    };
    let writes = &plan.delay_writes[0];
    assert!(writes.contains(&int(slave, 0, 16)) && writes.contains(&int(master, 0, 0)));
    assert_eq!(plan.delays[0][0].slave, 16);
    let lower = plan.routers.output_delay[2].unwrap();
    assert!(writes.contains(&int(lower, 9, 16)));
    assert!(writes.contains(&int(plan.routers.output_delay[0].unwrap(), 0, 0)));
}

/// `0x87d0d8`: a reuse zeroes the alignment routers in stock order (pair 0
/// merge master, slave, output delays 1-2, …) so the merge delay of 16 does
/// not survive when the new alignment skips it; the parameter follows the
/// router model as in the alignment (0 on 22003, 9 on 22002).
#[test]
fn reuse_zeroes_the_alignment_delays() {
    let specs = specs();
    let plan = compile(&split_pair(&specs), &specs, options(false, false)).unwrap();
    let routers = plan.routers;
    let resets = plan.delay_resets();
    let [master, slave] = routers.merge_delay[0].map(Option::unwrap);
    let zero = |at, index| RouterWrite {
        at,
        index,
        value: RouterValue::Int(0),
    };
    assert_eq!(resets[..2], [zero(master, 0), zero(slave, 0)]);
    assert!(resets.contains(&zero(routers.output_delay[2].unwrap(), 9)));
    for write in &plan.delay_writes[0] {
        assert!(
            resets
                .iter()
                .any(|reset| reset.at == write.at && reset.index == write.index)
        );
    }
}

/// The capture lock (`+0x3409`/`+0x3411`: NC Recorder, NC2, self-test)
/// sends the deploy enables as 0 (`0x5184fc`, `0x5856e8`): no input gate and
/// no global EQ while it holds.
#[test]
fn capture_lock_disables_the_gate_and_global_eq() {
    let specs = specs();
    let layout = layout(&specs, &[(0, &[(0, LIGHT)])]);
    let plan = compile(&layout, &specs, options(true, true)).unwrap();
    let values = |lock| {
        plan.gate_enables(lock).map(|message| match message {
            GateMessage::Enabled { value, .. } => value,
            other => panic!("{other:?}"),
        })
    };
    assert_eq!((values(false), values(true)), ([2, 2], [0, 0]));
    assert_eq!(
        (plan.output_eq_enabled(false), plan.output_eq_enabled(true)),
        (!plan.inhibited.global_eq, false)
    );
}

/// With the gate placed, lane inputs read the gate's row output (channel 10)
/// and the gate reads the port (`0x581920`). Gate parameters are gate
/// messages; INPUT GAIN is the input router's return gain with the gate
/// taking the +2.26 dB trim (`0x581a78`). `set_parameter` refuses gates.
#[test]
fn input_gate_takes_the_lane_inputs() {
    let specs = specs();
    let scene0 = SceneIndex::new(0).unwrap();
    let row = Row::new(0).unwrap();
    let layout = layout(&specs, &[(0, &[(0, LIGHT)])]);
    let (mut deployment, _) =
        Deployment::new(layout, &specs, options(true, false), scene0).unwrap();
    let plan = deployment.plan().clone();
    let input = plan.routers.lane_input[0].unwrap();
    let route: Vec<_> = plan.lane_routes.iter().filter(|w| w.at == input).collect();
    assert_eq!(route.len(), 2);
    assert_eq!(
        (route[0].value, route[1].value),
        (RouterValue::Int(10), RouterValue::Int(0))
    );
    let core = DspCore::IO;
    let messages = plan.gate_messages(scene0);
    assert_eq!(
        messages[..4],
        [
            GateMessage::Enabled {
                core: DspCore::ALL[0],
                value: 2
            },
            GateMessage::Enabled { core, value: 2 },
            GateMessage::Source {
                core,
                row,
                channel: Channel::Physical,
                index: 1
            },
            GateMessage::InputGain {
                core,
                row,
                value: tetrad_engine::INPUT_TRIM
            },
        ]
    );
    let update = deployment.set_input_gate(&specs, row, 0, 0.7).unwrap();
    assert!(update.ops.is_empty());
    assert_eq!(
        update.gate,
        [GateMessage::Parameter {
            core,
            row,
            index: 0,
            value: 0.7
        }]
    );
    // +12 dB: 0.75 of the −24..=+24 dB range.
    let update = deployment.set_input_gate(&specs, row, 3, 0.75).unwrap();
    let PlanOp::SetParam { at, index, value } = update.ops[0] else {
        panic!("input gain is a router write");
    };
    assert_eq!((at, index), (input, 9));
    assert!((value - 10f32.powf(0.6)).abs() < 1e-4);
    assert!(matches!(
        deployment.set_parameter(&specs, Site::InputControl(row), 0, 0.5),
        Err(DeployError::Unsupported { .. })
    ));
}

/// `0x681d44` → `0x5f3940`, parity D10: the stored masks are `layout_code_1/2`
/// (Factory 1B stores `[224]`/`[63]`), not `recompile_code_1/2` (1B: `[0]`/`[0]`).
/// Each group tries its mask before the search (`0x5efa08`). A mask that drops
/// the Global EQ is kept only when the group's recompile word is set (`0x5f03a8`).
#[test]
fn stored_masks_come_from_the_layout_codes() {
    let specs = specs();
    let layout = layout(
        &specs,
        &[(0, &[(2, LIGHT), (3, LIGHT)]), (2, &[(2, LIGHT)])],
    );
    let cores = |options, preset: Option<&tetrad_preset::Preset>| {
        let plan = match preset {
            Some(preset) => {
                let stored = tetrad_engine::StoredMasks::from_preset(preset);
                tetrad_engine::compile_stored(&layout, &specs, options, stored)
            }
            None => compile(&layout, &specs, options),
        }
        .unwrap();
        [slot(0, 2), slot(0, 3), slot(2, 2)]
            .map(|slot| plan.placement(Node::Model(Site::Slot(slot))).unwrap().core)
    };
    let [c0, c1, c2, c3] = DspCore::ALL;
    let mut preset = tetrad_preset::Preset {
        layout_code_1: vec![0b01],
        layout_code_2: vec![0],
        recompile_code_1: vec![0],
        recompile_code_2: vec![0],
        ..Default::default()
    };
    assert_eq!(cores(options(true, true), None), [c1, c1, c3]);
    assert_eq!(cores(options(true, true), Some(&preset)), [c1, c0, c2]);
    // Without the Global EQ every placement drops it: the search runs.
    let searched = cores(options(true, false), None);
    assert_ne!(searched, [c1, c0, c2]);
    assert_eq!(cores(options(true, false), Some(&preset)), searched);
    preset.recompile_code_1 = vec![1];
    preset.recompile_code_2 = vec![1];
    assert_eq!(cores(options(true, false), Some(&preset)), [c1, c0, c2]);
}

/// Applies a [`tetrad_engine::GridDrop`] as the device does: the moves, then the points with
/// stock default junctions.
fn drop_block(layout: &mut Layout, specs: &Specs, from: Slot, to: Slot, side: DropSide) {
    let drop = layout
        .plan_drop(from, to, side)
        .expect("stock accepts the drop");
    layout.move_blocks(&drop.moves).unwrap();
    if let Some(points) = drop.points {
        let pair = layout.pair_mut(points.pair);
        pair.split = points.split;
        pair.merge = points.mix;
        let junction = |id| Some(tetrad_engine::new_instance(specs.model(id).unwrap()));
        pair.splitter = pair.splitter.take().or_else(|| junction(10_004));
        pair.mixer = pair.mixer.take().or_else(|| junction(11_000));
    }
}

fn occupied(layout: &Layout, row: u8) -> Vec<u8> {
    (0..8)
        .filter(|&column| layout.instance(Site::Slot(slot(row, column))).is_some())
        .collect()
}

/// Stock `Chain::dropSlot` (`0x59f620` → `0x59f1cc`), hardware B1 (`sl-2.png`): a block of
/// row 1 dropped in empty row 2 splits at column 0 and mixes at column 7; both branches
/// deploy (row 2 runs on the split router's B buffer). The last row-2 block dropped back on
/// row 1 clears both points.
#[test]
fn a_drop_from_path_a_to_path_b_deploys_a_parallel_path() {
    let specs = specs();
    let mut layout = layout(&specs, &[(0, &[(0, LIGHT), (1, COMP)])]);
    drop_block(&mut layout, &specs, slot(0, 1), slot(1, 2), DropSide::Left);
    let pair = layout.pair(Pair::Upper);
    assert_eq!((pair.split, pair.merge), (Some(column(0)), Some(column(7))));
    let routing = route(&layout, &specs).unwrap();
    assert_eq!(routing.sources[1], LaneSource::Split(Pair::Upper));
    assert_eq!(routing.sinks[1], LaneSink::Merge(Pair::Upper));

    let scene0 = SceneIndex::new(0).unwrap();
    let (deployment, _) = Deployment::new(layout.clone(), &specs, options(false, false), scene0)
        .expect("the parallel path deploys");
    let plan = deployment.plan();
    let master = plan.slot(plan.block(slot(0, 0)).unwrap()).unwrap();
    let branch = plan.slot(plan.block(slot(1, 2)).unwrap()).unwrap();
    assert_eq!(branch.model, COMP);
    let split = plan.routers.split[0].expect("a split router");
    assert_eq!(
        setup(plan, split)[3],
        (1, RouterValue::Int(branch.layout.into())),
        "the split copies path A into the branch buffer"
    );
    assert_ne!(branch.layout, master.layout);
    assert!(plan.routers.merge[0].is_some(), "the branch mixes back");

    drop_block(&mut layout, &specs, slot(1, 2), slot(0, 4), DropSide::Left);
    let pair = layout.pair(Pair::Upper);
    assert_eq!((pair.split, pair.merge), (None, None));
}

/// Path B with its own input keeps it: only the mix is added (`0x59f1cc`). Row 4 fed by
/// `NEXT_ROW_4` counts as an input (`0x6baf54`). Another row must have a free slot
/// (`0x5aa738`).
#[test]
fn a_drop_on_path_b_respects_its_ports_and_free_slots() {
    let specs = specs();
    let mut own_input = layout(&specs, &[(0, &[(0, LIGHT)]), (1, &[])]);
    own_input.lane_mut(Row::new(1).unwrap()).output = OutputPort::Empty;
    let drop = own_input.plan_drop(slot(0, 0), slot(1, 0), DropSide::Left);
    let points = drop.unwrap().points.unwrap();
    assert_eq!((points.split, points.mix), (None, Some(column(7))));

    let mut fed = layout(&specs, &[(0, &[(0, LIGHT)]), (2, &[(0, HEAVY)])]);
    fed.lane_mut(Row::new(0).unwrap()).output = OutputPort::NextRow4;
    let drop = fed.plan_drop(slot(2, 0), slot(3, 1), DropSide::Left);
    let points = drop.unwrap().points.unwrap();
    assert_eq!((points.split, points.mix), (None, Some(column(7))));

    let row: Vec<(u8, ModelId)> = (0..8).map(|column| (column, LIGHT)).collect();
    let full = layout(&specs, &[(0, &[(0, HEAVY)]), (2, row.as_slice())]);
    let drop = full.plan_drop(slot(0, 0), slot(2, 3), DropSide::Left);
    assert_eq!(drop, None);
}

/// An occupied target makes room (`0x5c09c8`, `0x5c0ee0`, fallback `0x5c0440`): the left
/// half pushes right first, the right half pushes left first; the source slot of a drop in
/// its own row is free. The dropped block takes the target column.
#[test]
fn a_drop_on_a_block_shifts_its_row_towards_the_nearest_gap() {
    let specs = specs();
    let blocks: &[(u8, ModelId)] = &[(1, LIGHT), (2, HEAVY), (3, BIG), (5, LIGHT)];
    let base = layout(&specs, &[(0, &[(6, COMP)]), (2, blocks)]);
    let cases = [
        // (to column, side, occupied columns of row 3 after the drop)
        (2, DropSide::Left, vec![1, 2, 3, 4, 5]),
        (2, DropSide::Right, vec![0, 1, 2, 3, 5]),
        (5, DropSide::Left, vec![1, 2, 3, 5, 6]),
        (1, DropSide::Right, vec![0, 1, 2, 3, 5]),
    ];
    for (to, side, after) in cases {
        let mut layout = base.clone();
        drop_block(&mut layout, &specs, slot(0, 6), slot(2, to), side);
        assert_eq!(occupied(&layout, 2), after, "drop on column {to} {side:?}");
        assert_eq!(
            layout.instance(Site::Slot(slot(2, to))).unwrap().model,
            COMP
        );
    }
    // Same row: the source slot is the gap.
    let mut layout = layout(&specs, &[(0, &[(0, LIGHT), (1, HEAVY), (2, BIG)])]);
    drop_block(&mut layout, &specs, slot(0, 0), slot(0, 2), DropSide::Right);
    let models: Vec<ModelId> = (0..3)
        .map(|column| layout.instance(Site::Slot(slot(0, column))).unwrap().model)
        .collect();
    assert_eq!(models, [HEAVY, BIG, LIGHT]);
}

fn column(index: u8) -> tetrad_engine::Column {
    tetrad_engine::Column::new(index).unwrap()
}

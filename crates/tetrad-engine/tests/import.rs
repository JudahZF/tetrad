//! Synthetic `PresetSummary` fixtures for the conservative grid importer.
use std::collections::BTreeMap;
use tetrad_engine::{
    AbsentScalars, BypassPolarity, Column, Detached, DetachedStage, Error, ImportError,
    ImportPolicy, LanePorts, ModelId, OpaquePolicy, Pair, PortId, PortSide, Row, SceneIndex,
    Scened, ShadowedPort, Slot, import_summary,
};
use tetrad_preset::{
    BypassSummary, ChainSummary, ColBypassSummary, ModelSummary, PresetSummary, SceneBypassSummary,
    SplitControlPointsSummary, WirePresence,
};

const OWNED: ImportPolicy = ImportPolicy {
    absent: AbsentScalars::Refuse,
    bypass: BypassPolarity::StoredTrueBypasses,
};

fn presence(fields: &[u32]) -> Vec<WirePresence> {
    fields
        .iter()
        .map(|&number| WirePresence {
            number,
            name: None,
            optional: true,
            occurrences: 1,
            wire_types: vec![0],
        })
        .collect()
}

fn model(position: usize, hash: u32) -> ModelSummary {
    ModelSummary {
        position,
        hash: Some(hash),
        column: Some(position as u32),
        notify_flag: None,
        sidechain_sink_flag: None,
        sidechain_source_flag: None,
        params: Vec::new(),
        presence: presence(&[1, 5]),
        unknown_field_count: 0,
    }
}

fn scenes(values: &[bool]) -> Vec<SceneBypassSummary> {
    values
        .iter()
        .enumerate()
        .map(|(position, &bypass)| SceneBypassSummary {
            position,
            bypass: Some(bypass),
            presence: presence(&[1]),
            unknown_field_count: 0,
        })
        .collect()
}

fn points(split: i32, mix: i32) -> SplitControlPointsSummary {
    SplitControlPointsSummary {
        position: 0,
        split: Some(split),
        mix: Some(mix),
        presence: presence(&[1, 2]),
        unknown_field_count: 0,
    }
}

/// Row `position` reads port 10+row, writes port 20+row, has empty model slots and
/// detached input/output controls 100+row / 200+row. Master rows store `(split, mix)`.
fn chain(position: usize, route: Option<(i32, i32)>) -> ChainSummary {
    let row = position as u32;
    let routing = |model: ModelSummary| route.map(|_| vec![model]).unwrap_or_default();
    ChainSummary {
        position,
        row: Some(row),
        in_portid: Some(10 + row),
        out_portid: Some(20 + row),
        models: (0..8).map(|column| model(column, 0)).collect(),
        split_control_points: route
            .map(|(split, mix)| vec![points(split, mix)])
            .unwrap_or_default(),
        splitter: routing(model(0, 300 + row)),
        mixer: routing(model(0, 400 + row)),
        split_bypass: Vec::new(),
        mix_bypass: Vec::new(),
        output_control: vec![model(0, 200 + row)],
        combined_splitter: Vec::new(),
        input_control: vec![model(0, 100 + row)],
        presence: presence(&[1, 2, 14]),
        unknown_field_count: 0,
    }
}

fn bypass_row(position: usize) -> BypassSummary {
    BypassSummary {
        position,
        row: Some(position as u32),
        col_bypass: (0..8)
            .map(|column| ColBypassSummary {
                position: column,
                column: Some(column as u32),
                scene_mode: Some(false),
                scene_bypass: scenes(&[false]),
                presence: presence(&[1, 2, 3]),
                unknown_field_count: 0,
            })
            .collect(),
        presence: presence(&[1, 2]),
        unknown_field_count: 0,
    }
}

/// Upper pair splits at column 2 and merges at column 5; lower pair is unsplit.
fn preset() -> PresetSummary {
    PresetSummary {
        name: None,
        hash: None,
        default_scene: None,
        side_chain_follow_exists: None,
        zsk_7182_applied: None,
        scene_count: 1,
        scenes: Vec::new(),
        chain_count: 4,
        model_slot_count: 32,
        parameter_count: 0,
        scene_value_count: 0,
        chains: vec![
            chain(0, Some((2, 5))),
            chain(1, None),
            chain(2, Some((-1, -1))),
            chain(3, None),
        ],
        bypass: (0..4).map(bypass_row).collect(),
        presence: Vec::new(),
        unknown_field_count: 0,
        wire_field_count: 0,
    }
}

fn slot(row: u8, column: u8) -> Slot {
    Slot {
        row: Row::new(row).unwrap(),
        column: Column::new(column).unwrap(),
    }
}

fn scene(index: u8) -> SceneIndex {
    SceneIndex::new(index).unwrap()
}

fn path_of(error: ImportError) -> String {
    match error {
        ImportError::ListLength { path, .. }
        | ImportError::CoordinateMismatch { path, .. }
        | ImportError::Absent { path }
        | ImportError::Duplicate { path, .. }
        | ImportError::Unknown { path, .. }
        | ImportError::Point { path, .. }
        | ImportError::Unsupported { path, .. }
        | ImportError::BypassMissing { path }
        | ImportError::BypassRefused { path } => path,
        other => panic!("no path: {other:?}"),
    }
}

/// Pins the admitted subset: positional placement, opaque ports, shadowed slave
/// ports, split/mix points and detached controls that block `OpaquePolicy::Reject`.
#[test]
fn admits_placement_ports_routes_and_detached_models() {
    let mut summary = preset();
    summary.chains[0].models[0] = model(0, 7);
    summary.chains[1].models[3] = model(3, 8);
    summary.chains[3].models[7] = model(7, 9);
    let imported = import_summary(&summary, OWNED).unwrap();
    let grid = &imported.grid;

    assert_eq!(
        grid.block(slot(0, 0)).unwrap().kind,
        tetrad_engine::BlockKind::Model(ModelId(7))
    );
    assert_eq!(
        grid.block(slot(1, 3)).unwrap().kind,
        tetrad_engine::BlockKind::Model(ModelId(8))
    );
    assert_eq!(
        grid.block(slot(3, 7)).unwrap().kind,
        tetrad_engine::BlockKind::Model(ModelId(9))
    );
    assert!(grid.block(slot(0, 1)).is_none(), "hash 0 is an empty slot");

    let upper = grid.route(Pair::Upper);
    assert_eq!(upper.split.map(|j| j.column.index()), Some(2));
    assert_eq!(upper.merge.map(|j| j.column.index()), Some(5));
    assert_eq!(grid.route(Pair::Lower), Default::default());

    let row = |index| Row::new(index).unwrap();
    assert_eq!(grid.ports(row(1)), LanePorts::default());
    assert_eq!(
        grid.ports(row(3)),
        LanePorts {
            input: Some(PortId(13)),
            output: Some(PortId(23)),
        }
    );
    assert_eq!(
        imported.shadowed,
        [
            ShadowedPort {
                row: row(1),
                side: PortSide::Input,
                raw: 11
            },
            ShadowedPort {
                row: row(1),
                side: PortSide::Output,
                raw: 21
            },
        ]
    );
    assert!(imported.defaulted.is_empty());
    assert_eq!(grid.detached().len(), 4 * 2 + 2 * 2);
    assert_eq!(
        grid.detached()[0],
        Detached {
            stage: DetachedStage::InputControl(row(0)),
            model: ModelId(100),
        }
    );

    let program = grid.compile().unwrap();
    let inputs = BTreeMap::from([(PortId(10), vec![0.5])]);
    assert!(matches!(
        program.render(scene(0), &inputs, 1, OpaquePolicy::Reject),
        Err(Error::DetachedModel { .. })
    ));
    let out = program
        .render(scene(0), &inputs, 1, OpaquePolicy::PassThrough)
        .unwrap();
    // Owned unity split/merge: master and slave copies of 0.5 sum to 1.0.
    assert_eq!(out[&PortId(20)], [1.0]);

    assert!(matches!(
        import_summary(&summary, ImportPolicy::STRICT),
        Err(ImportError::BypassRefused { .. })
    ));
}

/// −1 means no junction; any other value outside 0..=7 is refused; an absent point
/// is refused unless the caller opts into protobuf defaults, which are reported.
#[test]
fn split_mix_points_honour_sentinel_range_and_absence() {
    let mut summary = preset();
    summary.chains[0].split_control_points = vec![points(-1, 7)];
    let route = import_summary(&summary, OWNED)
        .unwrap()
        .grid
        .route(Pair::Upper);
    assert_eq!(route.split, None);
    assert_eq!(route.merge.map(|j| j.column.index()), Some(7));

    summary.chains[0].split_control_points = vec![points(8, -1)];
    assert!(matches!(
        import_summary(&summary, OWNED),
        Err(ImportError::Point { value: 8, .. })
    ));
    summary.chains[0].split_control_points = vec![points(-2, -1)];
    assert!(matches!(
        import_summary(&summary, OWNED),
        Err(ImportError::Point { value: -2, .. })
    ));

    summary.chains[0].split_control_points[0].split = None;
    assert_eq!(
        import_summary(&summary, OWNED),
        Err(ImportError::Absent {
            path: "chains[0].split_control_points[0].1".into(),
        })
    );
    let lenient = ImportPolicy {
        absent: AbsentScalars::Proto3Default,
        ..OWNED
    };
    let imported = import_summary(&summary, lenient).unwrap();
    assert_eq!(
        imported
            .grid
            .route(Pair::Upper)
            .split
            .map(|j| j.column.index()),
        Some(0)
    );
    assert_eq!(imported.defaulted, ["chains[0].split_control_points[0].1"]);
}

/// Mirrors stock `0x67c5d4`: a shared value from one entry, per-scene values only
/// with all eight entries, and split/mix bypass admitted only while not set.
#[test]
fn scene_bypass_becomes_scened_block_state() {
    let mut summary = preset();
    summary.chains[0].models[1] = model(1, 7);
    summary.chains[0].models[4] = model(4, 8);
    summary.bypass[0].col_bypass[1].scene_bypass = scenes(&[true]);
    let mut per_scene = [false; 8];
    per_scene[3] = true;
    summary.bypass[0].col_bypass[4].scene_bypass = scenes(&per_scene);
    let grid = import_summary(&summary, OWNED).unwrap().grid;
    assert_eq!(grid.block(slot(0, 1)).unwrap().bypass, Scened::Fixed(true));
    assert_eq!(
        grid.block(slot(0, 4)).unwrap().bypass,
        Scened::PerScene(per_scene)
    );
    let program = grid.compile().unwrap();
    assert!(program.transition(scene(0), scene(1)).is_empty());
    assert_eq!(program.transition(scene(0), scene(3)).len(), 1);

    let mut sparse = summary.clone();
    sparse.bypass[0].col_bypass[4].scene_bypass = scenes(&[false, true, false]);
    assert!(matches!(
        import_summary(&sparse, OWNED),
        Err(ImportError::ListLength { .. })
    ));
    sparse.bypass[0].col_bypass[4].scene_bypass = scenes(&[false]);
    sparse.bypass[0].col_bypass[4].scene_mode = Some(true);
    assert!(matches!(
        import_summary(&sparse, OWNED),
        Err(ImportError::ListLength { .. })
    ));

    let mut junction = summary.clone();
    junction.chains[0].mix_bypass = scenes(&[false, false]);
    assert!(import_summary(&junction, OWNED).is_ok());
    junction.chains[0].mix_bypass = scenes(&[false, true]);
    assert!(matches!(
        import_summary(&junction, OWNED),
        Err(ImportError::Unsupported { .. })
    ));
}

/// Every ambiguity the stock loader would resolve silently, or that this crate cannot
/// place, is refused with the offending path.
#[test]
fn malformed_missing_duplicate_and_unsupported_data_is_refused() {
    type Edit = fn(&mut PresetSummary);
    let cases: [(Edit, &str); 10] = [
        (|p| p.chains[2].models.truncate(7), "chains[2].models"),
        (|p| p.chains[1].row = Some(3), "chains[1].14"),
        (
            |p| p.bypass[2].col_bypass[6].column = Some(5),
            "bypass[2].col_bypass[6].2",
        ),
        (|p| p.chains[3].presence[0].occurrences = 2, "chains[3]"),
        (|p| p.bypass[0].unknown_field_count = 1, "bypass[0]"),
        (
            |p| p.bypass[0].col_bypass[2].scene_bypass.clear(),
            "chains[0].models[2]",
        ),
        (
            |p| p.chains[0].split_control_points.push(points(0, 7)),
            "chains[0].split_control_points",
        ),
        (
            |p| p.chains[2].combined_splitter = vec![model(0, 5), model(1, 5)],
            "chains[2].splitter+combined_splitter",
        ),
        (
            |p| p.chains[3].input_control.clear(),
            "chains[3].input_control",
        ),
        (
            |p| p.chains[0].models[2].sidechain_source_flag = Some(true),
            "chains[0].models[2]",
        ),
    ];
    for (edit, path) in cases {
        let mut summary = preset();
        summary.chains[0].models[2] = model(2, 7);
        edit(&mut summary);
        assert_eq!(path_of(import_summary(&summary, OWNED).unwrap_err()), path);
    }

    let mut summary = preset();
    summary.chains.pop();
    assert_eq!(
        import_summary(&summary, OWNED),
        Err(ImportError::ChainCount(3))
    );
}

fn twice(number: u32) -> WirePresence {
    WirePresence {
        occurrences: 2,
        ..presence(&[number]).remove(0)
    }
}

/// Review F1: stored bypass records are validated whether or not their slot is occupied,
/// under both policies. Empty-slot records need no polarity, and each absent scalar is
/// reported once.
#[test]
fn bypass_records_of_empty_slots_are_validated_not_applied() {
    type Edit = fn(&mut PresetSummary);
    let cases: [(Edit, &str); 4] = [
        (
            |p| p.bypass[0].col_bypass[3].scene_bypass = scenes(&[false; 9]),
            "bypass[0].col_bypass[3].scene_bypass",
        ),
        (
            |p| p.bypass[0].col_bypass[3].scene_bypass[0].presence = vec![twice(1)],
            "bypass[0].col_bypass[3].scene_bypass[0]",
        ),
        (
            |p| p.bypass[0].col_bypass[3].scene_bypass[0].bypass = None,
            "bypass[0].col_bypass[3].scene_bypass[0].1",
        ),
        (
            |p| p.bypass[0].col_bypass[3].scene_mode = Some(true),
            "bypass[0].col_bypass[3].scene_bypass",
        ),
    ];
    for (edit, path) in cases {
        let mut summary = preset();
        edit(&mut summary);
        for policy in [OWNED, ImportPolicy::STRICT] {
            assert_eq!(path_of(import_summary(&summary, policy).unwrap_err()), path);
        }
    }

    let mut summary = preset();
    summary.bypass[0].col_bypass[3].scene_bypass = scenes(&[true; 8]);
    let imported = import_summary(&summary, ImportPolicy::STRICT).unwrap();
    assert!(imported.grid.block(slot(0, 3)).is_none());

    summary.chains[0].models[2] = model(2, 7);
    summary.bypass[0].col_bypass[2].scene_bypass[0].bypass = None;
    summary.bypass[0].col_bypass[3].scene_bypass[5].bypass = None;
    let lenient = ImportPolicy {
        absent: AbsentScalars::Proto3Default,
        ..OWNED
    };
    let imported = import_summary(&summary, lenient).unwrap();
    assert_eq!(
        imported.defaulted,
        [
            "bypass[0].col_bypass[2].scene_bypass[0].1",
            "bypass[0].col_bypass[3].scene_bypass[5].1"
        ]
    );
    assert_eq!(
        imported.grid.block(slot(0, 2)).unwrap().bypass,
        Scened::Fixed(false)
    );
}

/// Review F2: sidechain flags are refused on every model kind, and the flags and the
/// root `side_chain_follow_exists` (field 36) must occur at most once.
#[test]
fn sidechain_flags_are_refused_on_every_model_and_root() {
    type Edit = fn(&mut PresetSummary);
    let cases: [(Edit, &str); 6] = [
        (
            |p| p.chains[1].input_control[0].sidechain_sink_flag = Some(true),
            "chains[1].input_control[0]",
        ),
        (
            |p| p.chains[3].output_control[0].sidechain_source_flag = Some(true),
            "chains[3].output_control[0]",
        ),
        (
            |p| p.chains[0].splitter[0].sidechain_sink_flag = Some(true),
            "chains[0].splitter[0]",
        ),
        (
            |p| p.chains[2].mixer[0].sidechain_source_flag = Some(true),
            "chains[2].mixer[0]",
        ),
        (
            |p| {
                p.chains[0].models[2].sidechain_sink_flag = Some(false);
                p.chains[0].models[2].presence.push(twice(7));
            },
            "chains[0].models[2]",
        ),
        (
            |p| {
                p.side_chain_follow_exists = Some(false);
                p.presence.push(twice(36));
            },
            "preset",
        ),
    ];
    for (index, (edit, path)) in cases.into_iter().enumerate() {
        let mut summary = preset();
        edit(&mut summary);
        for policy in [OWNED, ImportPolicy::STRICT] {
            let error = import_summary(&summary, policy).unwrap_err();
            let duplicate = matches!(error, ImportError::Duplicate { .. });
            assert_eq!(duplicate, index >= 4, "{error:?}");
            assert_eq!(path_of(error), path);
        }
    }
}

fn param(position: usize, value: f32) -> tetrad_preset::ParamSummary {
    tetrad_preset::ParamSummary {
        position,
        index: None,
        expression: None,
        expression_min: None,
        expression_max: None,
        scene_mode: None,
        is_led: None,
        values: vec![Some(tetrad_preset::ParamValue::Float(value)); 8],
        value_presence: Vec::new(),
        dynamic_steps: Vec::new(),
        dynamic_icons: Vec::new(),
        dynamic_metadata: Vec::new(),
        presence: Vec::new(),
        unknown_field_count: 0,
    }
}

/// Stock data rules of the deployment import: `0xFFFFFFFF` marks empty slots,
/// slave chains carry inert routing, stored values are normalized with MUTE
/// after the XML list, NaN in a hidden parameter is replaced and reported, and
/// a model in the wrong kind of site is refused.
#[test]
fn layout_import_follows_stock_preset_data() {
    use tetrad_engine::{InputPort, OutputPort, Site, import_layout};
    let models = |category: u32, name: &str, ids: [u32; 4]| {
        let models: String = ids
            .iter()
            .map(|id| format!("<Model id='{id}' name='{name}'/>"))
            .collect();
        format!("<Category id='{category}' name='{name}'>{models}</Category>")
    };
    let xml = format!(
        "<Models><Category id='0' name='Drive'><Model id='7' name='Drive' blob='d'>\
         <Parameter name='GAIN' type='float' min='0' max='10' defaultValue='5'/>\
         <Parameter name='NOTIFY' type='float' min='0' max='1' defaultValue='0' hidden='true'/>\
         </Model></Category>{}{}{}{}</Models>",
        models(28, "Gate", [100, 101, 102, 103]),
        models(23, "Out", [200, 201, 202, 203]),
        models(10, "Split", [300, 301, 302, 303]),
        models(11, "Mix", [400, 401, 402, 403]),
    );
    let catalog = tetrad_catalog::Catalog::parse(&xml, "<ModuleReport/>").unwrap();
    let specs = tetrad_catalog::Specs::resolve(&catalog).unwrap();

    let mut summary = preset();
    for (row, chain) in summary.chains.iter_mut().enumerate() {
        chain.in_portid = Some(1);
        chain.out_portid = Some(if row == 0 { 1 } else { 0 });
        for model in &mut chain.models {
            model.hash = Some(u32::MAX);
        }
    }
    summary.chains[1].split_control_points = vec![points(-1, -1)];
    summary.chains[1].splitter = vec![model(0, u32::MAX)];
    let mut drive = model(2, 7);
    drive.params = vec![param(0, 0.25), param(1, f32::NAN), param(2, 1.0)];
    summary.chains[0].models[2] = drive;

    let imported = import_layout(&summary, &specs, AbsentScalars::Refuse).unwrap();
    let site = Site::Slot(slot(0, 2));
    let instance = imported.layout.instance(site).unwrap();
    assert_eq!(instance.params, [Scened::Fixed(0.25), Scened::Fixed(0.0)]);
    assert_eq!(instance.mute, Scened::Fixed(true));
    assert_eq!(imported.sanitized, [(site, 1)]);
    assert!(imported.layout.instance(Site::Slot(slot(0, 3))).is_none());
    let lane = imported.layout.lane(Row::new(0).unwrap());
    assert_eq!(
        (lane.input, lane.output),
        (InputPort::Input1, OutputPort::Xlr1And2)
    );

    summary.chains[0].models[4] = model(4, 100);
    assert!(matches!(
        import_layout(&summary, &specs, AbsentScalars::Refuse),
        Err(ImportError::ModelRole { .. })
    ));
}

/// Stock reads routing only on master chains (`0x681d44`): live slave-chain
/// points, junction models and junction bypass are ignored and listed, never
/// refused.
#[test]
fn slave_chain_routing_is_ignored_not_refused() {
    let mut summary = preset();
    let slave = &mut summary.chains[1];
    slave.split_control_points = vec![points(3, 6)];
    slave.splitter = vec![model(0, 301)];
    slave.mixer = vec![model(0, 401)];
    slave.mix_bypass = scenes(&[true]);
    let imported = import_summary(&summary, OWNED).unwrap();
    assert_eq!(
        imported.ignored,
        [
            "chains[1].split_control_points",
            "chains[1].splitter",
            "chains[1].mixer",
            "chains[1].mix_bypass"
        ]
    );
    // The master chain's routing is the pair's routing.
    let route = imported.grid.route(Pair::Upper);
    assert_eq!(route.split.map(|j| j.column.index()), Some(2));
    assert_eq!(route.merge.map(|j| j.column.index()), Some(5));
    assert!(import_summary(&preset(), OWNED).unwrap().ignored.is_empty());
}

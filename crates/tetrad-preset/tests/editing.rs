use tetrad_preset::{
    CatalogModel, CatalogParam, Error, Expression, GridRef, Limits, MidiMessage, Model,
    ModelCatalog, ParamValue, Preset, PresetDocument, SaveStamp, Slot, SplitterType,
    StompAssignment, StompRole, UnknownBlock,
};

const SIDECHAIN_MODEL: u32 = 200;

/// Synthetic catalog: parameter i of every model defaults to i / 10.
struct Catalog {
    complete: bool,
}

const CATALOG: Catalog = Catalog { complete: true };

impl ModelCatalog for Catalog {
    fn model(&self, id: u32) -> Option<CatalogModel> {
        let count = match id {
            23000 | 28000 => 5,
            10004 | 11000 => 8,
            25000 => 24,
            10000 => 4,
            10002 | 100 | SIDECHAIN_MODEL => 3,
            _ => return None,
        };
        Some(CatalogModel {
            params: (0..count)
                .map(|i| CatalogParam {
                    default: ParamValue::Float(i as f32 / 10.0),
                    expression_assignable: !(id == 100 && i == 1),
                })
                .collect(),
            sidechain_parameter: (id == SIDECHAIN_MODEL).then_some(1),
        })
    }

    fn splitter_type(&self, selector: SplitterType) -> Option<f32> {
        Some(match selector {
            SplitterType::Type0 => 0.0,
            SplitterType::Type1 => 0.5,
            SplitterType::Type2 => 1.0,
        })
    }

    fn tempo_value(&self, bpm: u32) -> Option<f32> {
        Some((bpm - 40) as f32 / 200.0)
    }

    fn is_complete(&self) -> bool {
        self.complete
    }
}

fn float(param: &tetrad_preset::Param, scene: usize) -> f32 {
    match param.value(scene) {
        Some(ParamValue::Float(value)) => *value,
        other => panic!("expected float, found {other:?}"),
    }
}

fn block(id: u32) -> Model {
    Model::block(id, [0.25, 0.5, 0.75].map(ParamValue::Float))
}

#[test]
fn new_default_is_the_stock_empty_preset_and_an_import_fixed_point() {
    let preset = Preset::new_default("4.1.0", &CATALOG).unwrap();
    assert_eq!(preset.chains.len(), 4);
    assert!(preset.chains.iter().all(|c| c.models.len() == 8));
    assert_eq!(
        (preset.chains[0].in_portid, preset.chains[0].out_portid),
        (Some(1), Some(0x13))
    );
    assert_eq!(preset.chains[2].split_control_points[0].split, -1);
    assert!(preset.chains[1].combined_splitter.is_empty());
    assert_eq!(
        preset.chains[0].output_control[0]
            .expression_bypass_info
            .len(),
        2
    );
    assert_eq!(preset.scene_colors[0], 0xffff_2727);
    assert_eq!(
        (
            preset.midi_messages.len(),
            preset.midi_messages_general.len(),
            preset.midi_messages_general_v2.len()
        ),
        (12, 10, 120)
    );
    assert_eq!(preset.created_version, ["4.1.0"]);

    let bytes = preset.to_bytes();
    assert_eq!(Preset::decode(&bytes, Limits::default()).unwrap(), preset);
    let mut document = PresetDocument::parse(&bytes, Limits::default()).unwrap();
    assert!(document.validate().unwrap().is_empty());
    let report = document.modify(|p| p.migrate(&CATALOG)).unwrap();
    assert_eq!(document.to_bytes(), bytes);
    assert_eq!((report.imported_models, report.resized_models), (13, 0));
}

#[test]
fn block_edits_carry_slot_state_and_leave_stock_slot_shapes() {
    let mut preset = Preset::new_default("4.1.0", &CATALOG).unwrap();
    let (a, b) = (Slot { row: 0, column: 0 }, Slot { row: 1, column: 3 });
    let empty = preset.clone();
    preset.insert_block(a, block(100), &CATALOG).unwrap();
    preset
        .block_bypass_mut(a)
        .unwrap()
        .set_bypassed(4, true)
        .unwrap();
    preset.assign_stomp(a, 2, StompRole::Primary).unwrap();
    assert!(matches!(
        preset.insert_block(a, block(100), &CATALOG),
        Err(Error::SlotOccupied { row: 0, column: 0 })
    ));

    preset.move_block(a, b, &CATALOG).unwrap();
    assert!(preset.block(a).unwrap().is_none());
    assert_eq!(preset.chains[0].models[0], Model::empty_slot());
    assert_eq!(preset.block(b).unwrap().unwrap().hash, Some(100));
    let record = &preset.bypass[1].col_bypass[3];
    assert_eq!(record.scene_mode, Some(false));
    assert_eq!(record.bypassed(0), Some(true));
    assert_eq!(
        (
            preset.stomp_mode_assignments[0].row,
            preset.stomp_mode_assignments[0].column
        ),
        (1, 3)
    );

    let previous = preset.replace_model(b, block(SIDECHAIN_MODEL)).unwrap();
    assert_eq!(previous.hash, Some(100));
    assert_eq!(preset.stomp_mode_assignments.len(), 1);
    assert_eq!(
        preset.remove_block(b, &CATALOG).unwrap().hash,
        Some(SIDECHAIN_MODEL)
    );
    assert_eq!(preset, empty);

    assert!(matches!(
        preset.remove_block(b, &CATALOG),
        Err(Error::SlotEmpty { .. })
    ));
    assert!(matches!(
        preset.insert_block(Slot { row: 4, column: 0 }, block(100), &CATALOG),
        Err(Error::Index { .. })
    ));
    assert!(
        preset
            .insert_block(a, Model::empty_slot(), &CATALOG)
            .is_err()
    );
    assert!(preset.assign_stomp(a, 0, StompRole::Primary).is_err());
    preset.side_chain_follow_exists = None; // Not in loaded form.
    assert!(preset.insert_block(a, block(100), &CATALOG).is_err());
}

/// A grid drop moves the blocks that make room and the dropped block at once (stock
/// `0x59e194`): here a drop in its own row, whose source slot is the gap. Records and stomp
/// assignments follow their blocks; a target that stays occupied is refused.
#[test]
fn move_blocks_moves_a_row_at_once() {
    let slot = |row, column| Slot { row, column };
    let mut preset = Preset::new_default("4.1.0", &CATALOG).unwrap();
    for (column, id) in [(0, 100), (1, SIDECHAIN_MODEL), (2, 10002)] {
        preset
            .insert_block(slot(0, column), block(id), &CATALOG)
            .unwrap();
    }
    preset
        .assign_stomp(slot(0, 0), 1, StompRole::Primary)
        .unwrap();
    let rotate = [
        (slot(0, 1), slot(0, 0)),
        (slot(0, 2), slot(0, 1)),
        (slot(0, 0), slot(0, 2)),
    ];
    preset.move_blocks(&rotate, &CATALOG).unwrap();
    let ids: Vec<_> = (0..3)
        .map(|column| preset.block(slot(0, column)).unwrap().unwrap().hash)
        .collect();
    assert_eq!(ids, [Some(SIDECHAIN_MODEL), Some(10002), Some(100)]);
    let assignment = &preset.stomp_mode_assignments[0];
    assert_eq!((assignment.row, assignment.column), (0, 2));

    let before = preset.clone();
    assert!(matches!(
        preset.move_blocks(&[(slot(0, 0), slot(0, 1))], &CATALOG),
        Err(Error::SlotOccupied { row: 0, column: 1 })
    ));
    assert_eq!(preset, before);
}

/// Stock sidechain follow (`0x0052f23c` / `0x0052fecc`): an edit before a
/// sink changes its option list (14 fixed + earlier blocks in row-major order,
/// value = index / (options - 1)), and the sink keeps the same source.
#[test]
fn grid_edits_keep_sidechain_sources() {
    let slot = |row, column| Slot { row, column };
    let option = |index: usize, sources: usize| (1.0 / (13 + sources) as f32) * index as f32;
    let sink = slot(2, 4);
    let mut preset = Preset::new_default("4.1.0", &CATALOG).unwrap();
    for source in [slot(0, 1), slot(0, 5)] {
        preset.insert_block(source, block(100), &CATALOG).unwrap();
    }
    preset
        .insert_block(sink, block(SIDECHAIN_MODEL), &CATALOG)
        .unwrap();
    let param = &mut preset.block_mut(sink).unwrap().params[1];
    param.set_scene_mode(true, 0).unwrap();
    param
        .set_value(0, ParamValue::Float(option(14, 2)))
        .unwrap(); // R1 C2
    param.set_value(1, ParamValue::Float(option(3, 2))).unwrap(); // Fixed "In 2"
    let selected = |preset: &Preset| {
        let param = &preset.block(sink).unwrap().unwrap().params[1];
        (float(param, 0), float(param, 1))
    };

    // Insert before: sources (0,0) (0,1) (0,5); R1 C2 is option 15 of 17.
    preset
        .insert_block(slot(0, 0), block(100), &CATALOG)
        .unwrap();
    assert_eq!(selected(&preset), (option(15, 3), option(3, 3)));
    // Remove before: sources (0,1) (0,5).
    preset.remove_block(slot(0, 0), &CATALOG).unwrap();
    assert_eq!(selected(&preset), (option(14, 2), option(3, 2)));
    // Move a later source in front: sources (0,0) (0,1).
    preset.move_block(slot(0, 5), slot(0, 0), &CATALOG).unwrap();
    assert_eq!(selected(&preset), (option(15, 2), option(3, 2)));
    // Move the source itself: its step ID follows it (`0x0052e0f4`).
    preset.move_block(slot(0, 1), slot(1, 6), &CATALOG).unwrap();
    assert_eq!(selected(&preset), (option(15, 2), option(3, 2)));
    // Remove the source: option 0.
    preset.remove_block(slot(1, 6), &CATALOG).unwrap();
    assert_eq!(selected(&preset), (0.0, option(3, 1)));

    // A block the catalog lacks may be a sink: refuse when its list changes.
    preset.chains[3].models[0] = block(999);
    assert!(matches!(
        preset.insert_block(slot(0, 2), block(100), &CATALOG),
        Err(Error::UnknownModel(999))
    ));
}

#[test]
fn scene_mode_decides_whether_one_or_all_scenes_change() {
    let mut param = tetrad_preset::Param::new(ParamValue::Float(0.1));
    param.set_value(2, ParamValue::Float(0.2)).unwrap();
    assert!((0..8).all(|scene| float(&param, scene) == 0.2));
    param.set_scene_mode(true, 0).unwrap();
    param.set_value(5, ParamValue::Float(0.9)).unwrap();
    assert_eq!((float(&param, 4), float(&param, 5)), (0.2, 0.9));
    param.set_scene_mode(false, 5).unwrap();
    assert!((0..8).all(|scene| float(&param, scene) == 0.9));
    assert!(matches!(
        param.set_value(0, ParamValue::Float(f32::NAN)),
        Err(Error::NonFinite)
    ));
    assert!(param.set_value(8, ParamValue::Float(0.0)).is_err());
    assert!(param.set_expression(1, 0.0, f32::INFINITY).is_err());
}

/// Stock scene copy/swap (`0x50e274`, `0x51a2a8`): per-scene values, bypass, label and
/// color move; scene-mode flags stay.
#[test]
fn scene_copy_and_swap_move_every_per_scene_value() {
    let mut preset = Preset::new_default("4.1.0", &CATALOG).unwrap();
    let at = Slot { row: 0, column: 2 };
    preset.insert_block(at, block(100), &CATALOG).unwrap();
    let param = preset.block_mut(at).unwrap().param_mut(0).unwrap();
    param.set_scene_mode(true, 0).unwrap();
    param.set_value(1, ParamValue::Float(0.9)).unwrap();
    preset
        .block_bypass_mut(at)
        .unwrap()
        .set_scene_mode(true, 0)
        .unwrap();
    preset
        .block_bypass_mut(at)
        .unwrap()
        .set_bypassed(1, true)
        .unwrap();
    preset.scene_labels = vec!["Verse".into(), "Lead".into()];
    preset.copy_scene(1, 6).unwrap();
    let value =
        |preset: &Preset, scene| float(&preset.block(at).unwrap().unwrap().params[0], scene);
    assert_eq!((value(&preset, 6), value(&preset, 0)), (0.9, 0.25));
    assert!(preset.bypass[0].col_bypass[2].scene_bypass[6].bypass);
    assert_eq!(preset.scene_labels[6], "Lead");
    assert_eq!(
        preset.block(at).unwrap().unwrap().params[0].scene_mode,
        Some(true)
    );
    preset.swap_scenes(0, 6).unwrap();
    assert_eq!((value(&preset, 0), value(&preset, 6)), (0.9, 0.25));
    assert_eq!(
        preset.scene_labels[..2],
        ["Lead".to_owned(), "Lead".to_owned()]
    );
    assert_eq!(preset.scene_labels[6], "Verse");
    assert!(preset.copy_scene(0, 8).is_err());
}

#[test]
fn migration_applies_the_recovered_model_import_rules() {
    let mut preset = Preset::new_default("4.1.0", &CATALOG).unwrap();
    preset.modified_version = vec!["2.0.0-dev-44-g8675dcf2a".into()];
    let mut stored = block(100);
    stored.params.truncate(2); // Stored M=2, runtime T=4: MUTE reads the last value.
    stored.params[0].values[3].value = None; // Untyped value reads as float 0.0.
    stored.params[0].set_expression(1, 0.2, 0.8).unwrap();
    stored.params[1].expression = Some(2); // Not expression-assignable.
    stored.params[1].expression_min = Some(0.3);
    stored.bypass_expression = vec![Expression::default()]; // Zero-width range.
    preset.chains[0].models[0] = stored;
    preset.chains[0].models[1] = block(999); // Not in the catalog.
    preset.bypass[0].col_bypass[1].scene_mode = Some(true);
    preset.side_chain_follow_exists = Some(false);
    preset.chains[1].models[0] = block(SIDECHAIN_MODEL);
    preset.chains[1].models[0].params[1]
        .set_value(0, ParamValue::Float(0.3))
        .unwrap();
    // NaN decodes as k = 0 in stock and stays.
    preset.chains[1].models[0].params[1].values[1].value = Some(ParamValue::Float(f32::NAN));
    preset.chains[0].models[2] = block(100);
    preset.chains[0].models[3] = block(100);
    preset.chains[1].output_control.push(block(100)); // Two entries: default.

    // An incomplete catalog keeps the unknown block and lists it.
    let mut kept = preset.clone();
    let report = kept.migrate(&Catalog { complete: false }).unwrap();
    let unknown = [UnknownBlock {
        slot: Slot { row: 0, column: 1 },
        model_id: 999,
    }];
    assert_eq!(
        (&report.unknown_blocks[..], report.unknown_blocks_emptied),
        (&unknown[..], false)
    );
    assert_eq!(kept.chains[0].models[1], block(999));

    let report = preset.migrate(&CATALOG).unwrap();
    assert_eq!(
        (&report.unknown_blocks[..], report.unknown_blocks_emptied),
        (&unknown[..], true)
    );
    assert_eq!(report.resized_models, 1);
    assert_eq!(preset.chains[0].models[1], Model::empty_slot());
    assert_eq!(preset.bypass[0].col_bypass[1].scene_mode, None);
    let model = &preset.chains[0].models[0];
    assert_eq!(model.params.len(), 3);
    assert_eq!(float(&model.params[0], 3), 0.0);
    assert_eq!(float(&model.params[2], 0), 0.5); // MUTE took the last stored value.
    // Expression and its min/max are read only for expAssignable parameters.
    assert_eq!(
        (model.params[0].expression, model.params[0].expression_min),
        (Some(1), Some(0.2))
    );
    assert_eq!(
        (model.params[1].expression, model.params[1].expression_min),
        (Some(0), Some(0.0))
    );
    assert_eq!(model.bypass_expression[0].expression_max, 1.0);
    // Pass one with three earlier sources: k = trunc(0.3 * 15) = 4 → 5/16.
    let sidechain = &preset.chains[1].models[0].params[1];
    assert_eq!(float(sidechain, 0), 0.3125);
    assert!(float(sidechain, 1).is_nan());
    // A lane-control list without exactly one entry takes the catalog default.
    assert_eq!(report.default_controls, 1);
    let control = &preset.chains[1].output_control;
    assert_eq!((control.len(), control[0].hash), (1, Some(23000)));
    assert_eq!(control[0].expression_bypass_info.len(), 2);
    assert_eq!(report.sidechain_parameters, 1);
    assert_eq!(preset.side_chain_follow_exists, Some(true));
}

#[test]
fn migration_converts_legacy_preset_level_data() {
    let mut preset = Preset::new_default("4.1.0", &CATALOG).unwrap();
    preset.created_version.clear();
    preset.modified_version.clear();
    preset.oldest_compatible_version.clear();
    preset.chains[0].combined_splitter.clear();
    preset.chains[0].splitter = vec![block(10002)];
    preset.tempo = Some(300);
    preset.scene_tempo = vec![90; 8];
    preset.tempo_program_data.clear();
    preset.chains[0].models[0] = block(100);
    preset.legacy_stomp_mode_stomp_data = vec![
        GridRef::default(),
        GridRef {
            row: 9,
            ..GridRef::default()
        },
    ];
    preset.stomp_mode_assignments = vec![StompAssignment {
        stomp_index: 11,
        ..StompAssignment::default()
    }];
    preset.midi_messages_general = vec![MidiMessage {
        kind: 0x103,
        channel: 5,
        param1: 64,
        ..MidiMessage::default()
    }];
    preset.midi_messages_general_v2.clear();
    preset.scene_colors.truncate(3);

    let report = preset.migrate(&CATALOG).unwrap();
    assert_eq!(preset.created_version, ["1.0.3"]);
    assert_eq!(report.converted_splitters, 1);
    let splitter = &preset.chains[0].combined_splitter[0];
    assert_eq!(splitter.hash, Some(10004));
    assert_eq!(float(&splitter.params[0], 0), 0.0); // TYPE 0 encoding.
    assert_eq!(float(&splitter.params[2], 0), 0.25); // Source 0 → target 2.

    // Same BPM in every scene: one clamped tempo from `tempo`, scene mode off.
    assert!(report.converted_tempo);
    assert_eq!((preset.tempo, preset.scene_tempo.len()), (None, 0));
    let tempo = &preset.tempo_program_data[0].params[0];
    assert_eq!((float(tempo, 7), tempo.scene_mode), (1.0, Some(false)));

    // Legacy entry 0 becomes primary stomp 0; out-of-grid and index>10 drop.
    assert!(preset.legacy_stomp_mode_stomp_data.is_empty());
    assert_eq!(preset.stomp_mode_assignments.len(), 1);
    assert_eq!(
        preset.stomp_mode_assignments[0].role,
        Some(StompRole::Primary)
    );

    assert!(report.converted_midi);
    assert_eq!(preset.midi_messages_general_v2.len(), 120);
    // Stock v1 branch of `0x681d44` keeps channel and params, not the type.
    let converted = &preset.midi_messages_general_v2[0];
    assert_eq!(
        (converted.kind, converted.channel, converted.param1),
        (0, 5, 64)
    );
    assert_eq!(preset.midi_messages_general.len(), 10);
    assert_eq!(preset.scene_colors.len(), 8);
    assert_eq!(preset.scene_colors[7], 0xff00_ffdd);

    let message = MidiMessage {
        kind: 1,
        channel: 2,
        ..MidiMessage::default()
    };
    preset.set_midi_general(3, 0, message.clone()).unwrap();
    assert_eq!(preset.midi_messages_general[3], message);
    assert_eq!(preset.midi_messages_general_v2[36], message);
    assert!(preset.set_midi_general(3, 12, message.clone()).is_err());
    let wide = MidiMessage {
        kind: 0x100,
        ..MidiMessage::default()
    };
    assert!(preset.set_midi_message(0, wide).is_err());
}

#[test]
fn migration_refuses_inputs_the_stock_loader_cannot_read_safely() {
    let base = Preset::new_default("4.1.0", &CATALOG).unwrap();
    let cases: [fn(&mut Preset); 6] = [
        |p| p.default_scene = Some(8),
        |p| p.scene_colors.push(0),
        |p| p.scene_labels.truncate(7),
        |p| p.cloud_id.clear(),
        |p| p.oldest_compatible_version.clear(),
        |p| p.chains.truncate(3),
    ];
    for change in cases {
        let mut preset = base.clone();
        change(&mut preset);
        let mut document = PresetDocument::parse(&preset.to_bytes(), Limits::default()).unwrap();
        let before = document.to_bytes();
        assert!(document.modify(|p| p.migrate(&CATALOG)).is_err());
        assert_eq!(document.to_bytes(), before);
    }
}

#[test]
fn save_stamp_applies_the_stock_save_fields() {
    let mut preset = Preset::new_default("4.1.0", &CATALOG).unwrap();
    preset.modified_version = vec!["3.3.1".into()];
    let stamp = SaveStamp {
        date: "2026-10-10T12:00:00Z",
        selected_scene: 3,
        account_name: "account",
        account_id: "id",
        path: "/opt/neuraldsp/Factory Library/12 String Bass.pb",
        name: "12 String Bass",
        changed_in: None,
    };
    preset.stamp_save(&stamp).unwrap();
    assert_eq!(preset.hash, Some(0xefaf_eb7a));
    assert_eq!(preset.default_scene, Some(3));
    assert_eq!(preset.author_name.as_deref(), Some("account"));
    assert_eq!(preset.modified_version, ["3.3.1"]);

    preset.author_name = Some("kept".into());
    preset
        .stamp_save(&SaveStamp {
            changed_in: Some("4.1.0"),
            ..stamp
        })
        .unwrap();
    assert_eq!(preset.author_name.as_deref(), Some("kept"));
    assert_eq!(
        (
            &preset.modified_version[..],
            &preset.oldest_compatible_version[..]
        ),
        (&["4.1.0".to_owned()][..], &["4.1.0".to_owned()][..])
    );
    assert!(
        preset
            .stamp_save(&SaveStamp {
                selected_scene: 8,
                ..stamp
            })
            .is_err()
    );

    // Unmigrated data or a pre-3.0.0 modified version is refused, unchanged.
    let cases: [fn(&mut Preset); 4] = [
        |p| p.zsk_7182_applied = Some(false),
        |p| p.side_chain_follow_exists = None,
        |p| p.legacy_stomp_mode_stomp_data = vec![GridRef::default()],
        |p| p.modified_version = vec!["2.9.0".into()],
    ];
    for change in cases {
        let mut refused = preset.clone();
        change(&mut refused);
        let before = refused.clone();
        assert!(refused.stamp_save(&stamp).is_err());
        assert_eq!(refused, before);
    }
}

//! The Custom mode in the place of stock Stomp mode, on the simulated device: page 1's A–H of
//! THIS PRESET are the preset's stock Stomp data, edits go back to it, and the preset's old
//! footswitch mappings move to the Custom mode.

mod common;

use common::{Media, STOCK, Sim, boot_with_files, light_preset};
use tetrad_app::device::custom;
use tetrad_app::device::preset::PresetEdit;
use tetrad_app::device::{Action, Device, Event};
use tetrad_engine::{Column, Row, Site, Slot};
use tetrad_hal::hal::Event as HalEvent;
use tetrad_hal::node::Element;
use tetrad_preset::model::{TETRAD_FOOTSWITCH_FIELD, TETRAD_MAPPINGS_FIELD};
use tetrad_services::fake::MemFiles;
use tetrad_settings::controls::FootswitchMode;
use tetrad_ui::{
    ActionEdit, BlockTarget, CustomCommand, Gesture, GridPosition, LayoutScope, MappingCommand,
    MappingScope, ModeKind, SlotAction, SlotEdit, SystemAction, Trigger,
};

fn boot() -> (Device<Sim>, Media) {
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let (mut device, _log, media) = boot_with_files(media, MemFiles::default());
    settle(&mut device);
    (device, media)
}

fn settle(device: &mut Device<Sim>) {
    for _ in 0..40 {
        assert!(device.step().is_none());
    }
}

/// An action edit of switch A on page 1 of THIS PRESET.
fn edit_a(device: &mut Device<Sim>, index: u8, edit: ActionEdit) {
    device.push(Event::Custom(CustomCommand::Slot {
        scope: LayoutScope::Preset,
        page: 0,
        slot: 0,
        gesture: Gesture::Press,
        edit: SlotEdit::Action { index, edit },
    }));
    settle(device);
}

fn preset(device: &Device<Sim>) -> &tetrad_preset::model::Preset {
    &device.state().current.as_ref().unwrap().preset
}

fn bypassed(device: &Device<Sim>) -> bool {
    let current = device.state().current.as_ref().unwrap();
    let site = Site::Slot(Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    });
    let block = current.deployment.layout().instance(site).unwrap();
    block.bypass.at(current.deployment.scene())
}

fn switch_a(device: &mut Device<Sim>, value: i32) {
    let element = Element::all().next().unwrap();
    device.push(Event::Hal(HalEvent::Stomp { element, value }));
    settle(device);
}

/// The actions of A on page 1 of THIS PRESET.
fn preset_a(device: &Device<Sim>) -> Vec<(SlotAction, Option<ModeKind>)> {
    let view = custom::view(device.state()).unwrap();
    let preset = view.preset_slots.expect("a preset is loaded");
    (preset[0].slots[0].actions.iter())
        .map(|action| (action.action.clone(), action.mode.as_ref().map(|m| m.kind)))
        .collect()
}

/// Stored Stomp mode is the Custom mode. A bypass on A of THIS PRESET is the preset's stock
/// Stomp assignment, its Momentary is the stock momentary flag, and a press plays it through
/// the stock stomp path. The layout field keeps nothing of it.
#[test]
fn page_1_of_this_preset_is_the_stock_stomp_data() {
    let (mut device, _media) = boot();
    device.push(Event::Action(Action::SetMode(FootswitchMode::Stomp)));
    settle(&mut device);
    assert!(device.state().performance.footswitch.custom);
    let bypass = SlotAction::Block {
        position: GridPosition::new(0, 2).unwrap(),
        target: BlockTarget::Bypass,
    };
    edit_a(&mut device, 0, ActionEdit::Set(bypass.clone()));
    let assignments = |device: &Device<Sim>| {
        (preset(device).stomp_mode_assignments.iter())
            .map(|a| (a.row, a.column, a.stomp_index))
            .collect::<Vec<_>>()
    };
    assert_eq!(assignments(&device), [(0, 2, 0)]);
    assert_eq!(
        preset_a(&device),
        [(bypass.clone(), Some(ModeKind::Toggle))]
    );
    let field = |device: &Device<Sim>| preset(device).unknown.bytes_field(TETRAD_FOOTSWITCH_FIELD);
    assert_eq!(field(&device), None, "stock holds it all");

    // A toggle, through the stock stomp path.
    let start = bypassed(&device);
    switch_a(&mut device, 1);
    switch_a(&mut device, 0);
    assert_eq!(bypassed(&device), !start);

    // Momentary is the stock flag: the block changes while A is down.
    edit_a(&mut device, 0, ActionEdit::Mode(ModeKind::Momentary));
    let flag = preset(&device).stomp_is_momentary.get(0).copied();
    assert_eq!(flag, Some(true));
    assert_eq!(preset_a(&device), [(bypass, Some(ModeKind::Momentary))]);
    let start = bypassed(&device);
    switch_a(&mut device, 1);
    assert_eq!(bypassed(&device), !start, "held");
    switch_a(&mut device, 0);
    assert_eq!(bypassed(&device), start, "released");

    // Another action goes to the layout field; removing the bypass unassigns the block.
    let scene = SlotAction::System(SystemAction::Scene(2));
    edit_a(&mut device, 1, ActionEdit::Set(scene.clone()));
    assert!(field(&device).is_some());
    edit_a(&mut device, 0, ActionEdit::Remove);
    assert!(assignments(&device).is_empty());
    assert_eq!(preset_a(&device), [(scene, Some(ModeKind::OnPress))]);
}

/// The Mappings page no longer takes footswitch triggers. An old footswitch mapping of the
/// preset plays as an action of A on page 1, and the next edit of THIS PRESET moves it from the
/// Mappings field to the layout field.
#[test]
fn old_footswitch_mappings_move_to_the_custom_mode() {
    let (mut device, _media) = boot();
    let scope = MappingScope::Preset;
    device.push(Event::Mappings(MappingCommand::Add(scope)));
    settle(&mut device);
    let id = device.state().views.mappings.focus.clone().unwrap().1;
    let row = device
        .state()
        .views
        .mappings
        .row(scope, &id)
        .cloned()
        .unwrap();
    assert_eq!(row.trigger, Trigger::PresetLoaded);
    let trigger = Trigger::Stomp(0);
    device.push(Event::Mappings(MappingCommand::SetTrigger {
        scope,
        id,
        trigger,
    }));
    settle(&mut device);
    assert_eq!(
        device.state().views.mappings.notice.as_deref(),
        Some("The footswitches are on the Custom mode page")
    );
    device.push(Event::Mappings(MappingCommand::Remove {
        scope,
        id: tetrad_ui::MappingId::Tetrad(0),
    }));
    settle(&mut device);

    let old = br#"{"format":1,"mappings":[
        {"trigger":{"kind":"stomp","stomp":0},"action":{"kind":"system","action":{"scene":3}}}
    ]}"#;
    device.push(Event::Action(Action::PresetEdit(PresetEdit::Mappings(
        Some(old.to_vec()),
    ))));
    settle(&mut device);
    assert!(
        device.state().views.mappings.preset.is_empty(),
        "not a Mappings row"
    );
    let scene = SlotAction::System(SystemAction::Scene(3));
    assert_eq!(
        preset_a(&device),
        [(scene.clone(), Some(ModeKind::OnPress))]
    );
    device.push(Event::Action(Action::SetMode(FootswitchMode::Stomp)));
    settle(&mut device);
    switch_a(&mut device, 1);
    switch_a(&mut device, 0);
    let current = device.state().current.as_ref().unwrap();
    assert_eq!(current.deployment.scene().index(), 3);

    device.push(Event::Custom(CustomCommand::Slot {
        scope: LayoutScope::Preset,
        page: 0,
        slot: 0,
        gesture: Gesture::Press,
        edit: SlotEdit::SetColour(Some(2)),
    }));
    settle(&mut device);
    assert_eq!(
        preset(&device).unknown.bytes_field(TETRAD_MAPPINGS_FIELD),
        None
    );
    assert!(
        preset(&device)
            .unknown
            .bytes_field(TETRAD_FOOTSWITCH_FIELD)
            .is_some()
    );
    assert_eq!(preset_a(&device), [(scene, Some(ModeKind::OnPress))]);
}

/// Without the layout file, the layout of the old Custom Footswitch Mode extension (the one
/// the old mode file names) moves to it at the start, and plays.
#[test]
fn the_old_extension_layout_moves_to_the_layout_file() {
    let files = MemFiles::default();
    let old = br#"{"format":1,"slots":[
        {"page":0,"slot":9,"action":{"kind":"system","action":"tuner"},"label":"Tune"}
    ]}"#;
    let other = br#"{"format":1,"slots":[]}"#;
    files.put(
        "/media/p4/tetrad/footswitch-modes/A~other.json",
        std::str::from_utf8(other).unwrap(),
    );
    files.put(
        "/media/p4/tetrad/footswitch-modes/judahzf_tetrad-extensions~tetrad.custom-footswitch.json",
        std::str::from_utf8(old).unwrap(),
    );
    files.put(
        custom::ACTIVE_FILE,
        r#"{"format":1,"active":"JudahZF/tetrad-extensions:tetrad.custom-footswitch"}"#,
    );
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let (mut device, _log, _media) = boot_with_files(media, files.clone());
    settle(&mut device);
    let view = custom::view(device.state()).unwrap();
    assert_eq!(view.slots[0].slots[9].label, "Tune");
    let moved = files.get(custom::LAYOUT_FILE).expect("the layout file");
    let moved: serde_json::Value = serde_json::from_slice(&moved).unwrap();
    assert_eq!(moved["format"], 2);
}

/// A bypass that moves from B to A stays assigned (the old switch's unassignment goes first),
/// and a page 1 bypass from an older layout field plays and moves to the stock data with the
/// next edit.
#[test]
fn stock_bypasses_move_and_older_layouts_keep_theirs() {
    let (mut device, _media) = boot();
    let light = GridPosition::new(0, 2).unwrap();
    let bypass = SlotAction::Block {
        position: light,
        target: BlockTarget::Bypass,
    };
    device.push(Event::Custom(CustomCommand::Slot {
        scope: LayoutScope::Preset,
        page: 0,
        slot: 1,
        gesture: Gesture::Press,
        edit: SlotEdit::Action {
            index: 0,
            edit: ActionEdit::Set(bypass.clone()),
        },
    }));
    settle(&mut device);
    let assignments = |device: &Device<Sim>| {
        (preset(device).stomp_mode_assignments.iter())
            .map(|a| (a.row, a.column, a.stomp_index))
            .collect::<Vec<_>>()
    };
    assert_eq!(assignments(&device), [(0, 2, 1)]);
    device.push(Event::Custom(CustomCommand::Slot {
        scope: LayoutScope::Preset,
        page: 0,
        slot: 1,
        gesture: Gesture::Press,
        edit: SlotEdit::MoveTo { page: 0, slot: 0 },
    }));
    settle(&mut device);
    assert_eq!(assignments(&device), [(0, 2, 0)]);

    // An older layout field with a bypass on C: it shows and plays as a Custom action. (A
    // lets the block go first: stock holds a block on one switch.)
    device.push(Event::Custom(CustomCommand::Slot {
        scope: LayoutScope::Preset,
        page: 0,
        slot: 0,
        gesture: Gesture::Press,
        edit: SlotEdit::Clear,
    }));
    settle(&mut device);
    let old = br#"{"format":2,"slots":[{"page":0,"slot":2,"actions":[
        {"action":{"kind":"block","row":0,"column":2,"target":{"kind":"bypass"}}}]}]}"#;
    device.push(Event::Action(Action::PresetEdit(PresetEdit::CustomLayout(
        Some(old.to_vec()),
    ))));
    settle(&mut device);
    let view = custom::view(device.state()).unwrap();
    let c = &view.preset_slots.unwrap()[0].slots[2];
    assert_eq!(c.action, Some(bypass));
    device.push(Event::Custom(CustomCommand::Slot {
        scope: LayoutScope::Preset,
        page: 0,
        slot: 2,
        gesture: Gesture::Press,
        edit: SlotEdit::SetColour(Some(1)),
    }));
    settle(&mut device);
    assert_eq!(assignments(&device), [(0, 2, 2)], "moved to the stock data");
}

/// A stock switch with more actions than a stored switch holds cannot move to one: the edit is
/// refused and its assignments stay.
#[test]
fn a_stock_switch_too_big_to_store_does_not_move() {
    use tetrad_preset::{Model, ParamValue, StompAssignment};
    let specs = STOCK.specs();
    let catalog = tetrad_app::device::preset::SpecsCatalog(&specs);
    let mut stored = light_preset(&specs, 2);
    let columns: Vec<usize> = (0..8).filter(|&column| column != 2).collect();
    for &column in &columns {
        let block = Model::block(1, [ParamValue::Float(0.5), ParamValue::Float(0.0)]);
        let slot = tetrad_preset::Slot { row: 0, column };
        stored.insert_block(slot, block, &catalog).unwrap();
    }
    let block = Model::block(1, [ParamValue::Float(0.5), ParamValue::Float(0.0)]);
    let slot = tetrad_preset::Slot { row: 1, column: 0 };
    stored.insert_block(slot, block, &catalog).unwrap();
    for (row, column) in (0..8).map(|column| (0, column)).chain([(1, 0)]) {
        stored.stomp_mode_assignments.push(StompAssignment {
            row,
            column,
            stomp_index: 0,
            ..StompAssignment::default()
        });
    }
    let media = Media::new([("Many", stored)]);
    let (mut device, _log, _media) = boot_with_files(media, MemFiles::default());
    settle(&mut device);
    assert_eq!(preset_a(&device).len(), 9);
    device.push(Event::Custom(CustomCommand::Slot {
        scope: LayoutScope::Preset,
        page: 0,
        slot: 0,
        gesture: Gesture::Press,
        edit: SlotEdit::MoveTo { page: 1, slot: 0 },
    }));
    settle(&mut device);
    let view = custom::view(device.state()).unwrap();
    assert_eq!(
        view.notice.as_deref(),
        Some("A switch runs at most 8 actions")
    );
    assert_eq!(preset(&device).stomp_mode_assignments.len(), 9);

    // A bypass in Set mode leaves the stock data for the layout field: eight fit, the ninth
    // is refused.
    for _ in 0..9 {
        let at = (preset_a(&device).iter())
            .position(|(_, mode)| *mode != Some(ModeKind::Set))
            .unwrap() as u8;
        edit_a(&mut device, at, ActionEdit::Mode(ModeKind::Set));
    }
    let view = custom::view(device.state()).unwrap();
    assert_eq!(
        view.notice.as_deref(),
        Some("A switch runs at most 8 actions")
    );
    assert_eq!(preset(&device).stomp_mode_assignments.len(), 1);
    assert_eq!(preset_a(&device).len(), 9, "nothing is lost");
}

/// A copy that puts one block's toggle on two of A–H is refused (stock holds a block on one
/// switch), and Nothing stops a switch whose only stock data is a secondary assignment.
#[test]
fn stock_limits_copies_and_nothing_stops_hidden_stock_data() {
    use tetrad_preset::{StompAssignment, StompRole};
    let mut stored = light_preset(&STOCK.specs(), 2);
    stored.stomp_mode_assignments.push(StompAssignment {
        row: 0,
        column: 2,
        stomp_index: 1,
        role: Some(StompRole::Secondary),
        ..StompAssignment::default()
    });
    let media = Media::new([("Light", stored)]);
    let (mut device, _log, _media) = boot_with_files(media, MemFiles::default());
    settle(&mut device);
    device.push(Event::Action(Action::SetMode(FootswitchMode::Stomp)));
    settle(&mut device);
    let slot = |slot, edit| {
        Event::Custom(CustomCommand::Slot {
            scope: LayoutScope::Preset,
            page: 0,
            slot,
            gesture: Gesture::Press,
            edit,
        })
    };
    let bypass = SlotAction::Block {
        position: GridPosition::new(0, 2).unwrap(),
        target: BlockTarget::Bypass,
    };
    let set = |action| SlotEdit::Action {
        index: 0,
        edit: ActionEdit::Set(action),
    };
    device.push(slot(0, set(bypass)));
    settle(&mut device);
    device.push(slot(0, SlotEdit::CopyTo { page: 0, slot: 3 }));
    settle(&mut device);
    let view = custom::view(device.state()).unwrap();
    assert_eq!(
        view.notice.as_deref(),
        Some("A block can toggle on one of A–H of page 1: use Set on the other")
    );
}

/// Nothing stops the stock data of a switch, which a press would play otherwise.
#[test]
fn nothing_stops_the_stock_data() {
    let (mut device, _media) = boot();
    device.push(Event::Action(Action::SetMode(FootswitchMode::Stomp)));
    settle(&mut device);
    let bypass = SlotAction::Block {
        position: GridPosition::new(0, 2).unwrap(),
        target: BlockTarget::Bypass,
    };
    edit_a(&mut device, 0, ActionEdit::Set(bypass));
    edit_a(&mut device, 1, ActionEdit::Set(SlotAction::Nothing));
    let start = bypassed(&device);
    switch_a(&mut device, 1);
    switch_a(&mut device, 0);
    assert_eq!(bypassed(&device), start, "Nothing plays nothing");
}

/// An old footswitch mapping on a switch whose layout field is full shows as a ninth action;
/// edits are refused until one goes, so neither is lost.
#[test]
fn old_mappings_over_the_limit_are_kept() {
    let (mut device, _media) = boot();
    let tuner = r#"{"action":{"kind":"system","action":"tuner"}}"#;
    let field = format!(
        r#"{{"format":2,"slots":[{{"page":0,"slot":0,"actions":[{}]}}]}}"#,
        [tuner; 8].join(",")
    );
    let old = br#"{"format":1,"mappings":[
        {"trigger":{"kind":"stomp","stomp":0},"action":{"kind":"system","action":{"scene":3}}}
    ]}"#;
    for edit in [
        PresetEdit::CustomLayout(Some(field.into_bytes())),
        PresetEdit::Mappings(Some(old.to_vec())),
    ] {
        device.push(Event::Action(Action::PresetEdit(edit)));
        settle(&mut device);
    }
    assert_eq!(preset_a(&device).len(), 9);
    device.push(Event::Custom(CustomCommand::Slot {
        scope: LayoutScope::Preset,
        page: 0,
        slot: 1,
        gesture: Gesture::Press,
        edit: SlotEdit::SetColour(Some(2)),
    }));
    settle(&mut device);
    let view = custom::view(device.state()).unwrap();
    assert_eq!(
        view.notice.as_deref(),
        Some("A switch runs at most 8 actions")
    );
    assert!(
        preset(&device)
            .unknown
            .bytes_field(TETRAD_MAPPINGS_FIELD)
            .is_some()
    );
    edit_a(&mut device, 0, ActionEdit::Remove);
    assert!(
        preset(&device)
            .unknown
            .bytes_field(TETRAD_MAPPINGS_FIELD)
            .is_none()
    );
    assert_eq!(preset_a(&device).len(), 8);
}

/// Stomp and a stored Stomp hybrid both play Custom: the cycle does not stop on Custom twice.
#[test]
fn the_cycle_passes_one_custom() {
    let (mut device, _media) = boot();
    let modes = vec![FootswitchMode::Stomp, FootswitchMode::StompPreset];
    device.push(Event::Action(Action::SetAllowedModes(modes)));
    settle(&mut device);
    assert!(device.state().performance.footswitch.custom);
    device.push(Event::Action(Action::CycleMode));
    settle(&mut device);
    let mode = device.state().settings.settings().mode.mode();
    assert_eq!(mode, FootswitchMode::Stomp, "no second stop on Custom");
}

/// An old layout that does not decode is not replaced by an empty layout file: the next start
/// reads it again.
#[test]
fn an_old_layout_that_does_not_decode_stays() {
    let files = MemFiles::default();
    files.put("/media/p4/tetrad/footswitch-modes/a~b.json", "{not json");
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let (mut device, _log, _media) = boot_with_files(media, files.clone());
    settle(&mut device);
    assert_eq!(files.get(custom::LAYOUT_FILE), None);
}

/// MIDI CC#35 presses A: on a momentary stock stomp, one message is the press and the next
/// the release, as in stock Stomp mode.
#[test]
fn midi_cc_holds_a_momentary_stock_stomp() {
    let (mut device, _media) = boot();
    device.push(Event::Action(Action::SetMode(FootswitchMode::Stomp)));
    settle(&mut device);
    let bypass = SlotAction::Block {
        position: GridPosition::new(0, 2).unwrap(),
        target: BlockTarget::Bypass,
    };
    edit_a(&mut device, 0, ActionEdit::Set(bypass));
    edit_a(&mut device, 0, ActionEdit::Mode(ModeKind::Momentary));
    let start = bypassed(&device);
    let cc35 = |device: &mut Device<Sim>| {
        let inputs = device.platform().inputs.clone();
        inputs.borrow_mut().extend([0xb0, 35, 127].map(|byte| {
            tetrad_app::device::platform::Input::MidiByte {
                byte,
                at: std::time::Duration::ZERO,
            }
        }));
        settle(device);
    };
    cc35(&mut device);
    assert_eq!(bypassed(&device), !start, "held");
    cc35(&mut device);
    assert_eq!(bypassed(&device), start, "released");
}

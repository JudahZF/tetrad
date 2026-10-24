//! Mappings end to end on the simulated device: commands as the Mappings page sends them, MIDI
//! on the QC channel, Stomp-mode presses, the global file and the preset field.

mod common;

use std::time::Duration;

use common::{Media, STOCK, Sim, boot_with_files, light_preset};
use tetrad_app::device::mappings::GLOBAL_FILE;
use tetrad_app::device::platform::Input;
use tetrad_app::device::{Action, Device, Event};
use tetrad_engine::{Column, Row, Site, Slot};
use tetrad_hal::hal::Event as HalEvent;
use tetrad_hal::node::Element;
use tetrad_preset::model::TETRAD_MAPPINGS_FIELD;
use tetrad_services::fake::MemFiles;
use tetrad_settings::controls::FootswitchMode;
use tetrad_ui::{
    BlockTarget, FieldId, GridPosition, MappingAction, MappingCommand, MappingId, MappingScope,
    MappingsView, MidiInput, ModeKind, SystemAction, Trigger,
};

/// The Light block of [`light_preset`] at column 2: GAIN is parameter 0.
fn light() -> GridPosition {
    GridPosition::new(0, 2).unwrap()
}

fn media() -> Media {
    Media::new([("Light", light_preset(&STOCK.specs(), 2))])
}

/// Boots over `media` and `files`; the media root must outlive the device.
fn boot(media: Media, files: &MemFiles) -> (Device<Sim>, Media) {
    let (mut device, _log, media) = boot_with_files(media, files.clone());
    settle(&mut device);
    (device, media)
}

fn settle(device: &mut Device<Sim>) {
    for _ in 0..40 {
        assert!(device.step().is_none());
    }
}

fn view(device: &Device<Sim>) -> MappingsView {
    device.state().views.mappings.clone()
}

/// Sends `command` and answers the row it focused.
fn send(device: &mut Device<Sim>, command: MappingCommand) -> Option<(MappingScope, MappingId)> {
    device.push(Event::Mappings(command));
    settle(device);
    view(device).focus
}

fn midi(device: &mut Device<Sim>, bytes: &[u8]) {
    let inputs = device.platform().inputs.clone();
    inputs
        .borrow_mut()
        .extend(bytes.iter().map(|&byte| Input::MidiByte {
            byte,
            at: Duration::ZERO,
        }));
    settle(device);
}

fn scene(device: &Device<Sim>) -> usize {
    device
        .state()
        .current
        .as_ref()
        .unwrap()
        .deployment
        .scene()
        .index()
}

fn light_site() -> Site {
    Site::Slot(Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    })
}

fn gain(device: &Device<Sim>) -> f32 {
    let current = device.state().current.as_ref().unwrap();
    let block = current.deployment.layout().instance(light_site()).unwrap();
    block.params[0].at(current.deployment.scene())
}

fn bypassed(device: &Device<Sim>) -> bool {
    let current = device.state().current.as_ref().unwrap();
    let block = current.deployment.layout().instance(light_site()).unwrap();
    block.bypass.at(current.deployment.scene())
}

/// A new preset row: `trigger` → `action`. Answers its id.
fn map(device: &mut Device<Sim>, trigger: Trigger, action: MappingAction) -> MappingId {
    let scope = MappingScope::Preset;
    let (_, id) = send(device, MappingCommand::Add(scope)).unwrap();
    let (_, id) = send(device, MappingCommand::SetTrigger { scope, id, trigger }).unwrap();
    send(device, MappingCommand::SetAction { scope, id, action })
        .unwrap()
        .1
}

fn gain_action() -> MappingAction {
    MappingAction::Block {
        position: light(),
        target: BlockTarget::Parameter(0),
    }
}

fn set_mode(device: &mut Device<Sim>, id: &MappingId, mode: ModeKind) {
    let scope = MappingScope::Preset;
    let id = id.clone();
    send(device, MappingCommand::SetMode { scope, id, mode });
    assert_eq!(view(device).notice, None);
}

fn step(device: &mut Device<Sim>, id: &MappingId, field: FieldId, ticks: i32) {
    let scope = MappingScope::Preset;
    let id = id.clone();
    let command = MappingCommand::StepField {
        scope,
        id,
        field,
        ticks,
    };
    send(device, command);
}

/// Note `key` on the QC channel: on (velocity 100) or off.
fn note(device: &mut Device<Sim>, key: u8, on: bool) {
    let status = if on { 0x90 } else { 0x80 };
    midi(device, &[status, key, if on { 100 } else { 0 }]);
}

/// The full global flow: a stock-map CC is refused, LEARN takes a free CC, the CC selects a
/// scene, and the mapping comes back from the global file after a restart.
#[test]
fn a_global_midi_mapping_learns_runs_and_survives_a_restart() {
    let files = MemFiles::default();
    let (mut device, media) = boot(media(), &files);
    let scope = MappingScope::Global;
    let (_, id) = send(&mut device, MappingCommand::Add(scope)).unwrap();

    let refused = Trigger::Midi(MidiInput::Cc(43));
    send(
        &mut device,
        MappingCommand::SetTrigger {
            scope,
            id: id.clone(),
            trigger: refused,
        },
    );
    assert!(view(&device).notice.unwrap().contains("CC 43"));

    send(
        &mut device,
        MappingCommand::Learn {
            scope,
            id: id.clone(),
            on: true,
        },
    );
    assert_eq!(view(&device).learning, Some((scope, id.clone())));
    midi(&mut device, &[0xb0, 22, 127]);
    let (_, id) = view(&device).focus.unwrap();
    let row = view(&device).row(scope, &id).cloned().unwrap();
    assert_eq!(row.trigger, Trigger::Midi(MidiInput::Cc(22)));
    assert_eq!(view(&device).learning, None);

    let action = MappingAction::System(SystemAction::Scene(2));
    send(&mut device, MappingCommand::SetAction { scope, id, action });
    assert_ne!(scene(&device), 2);
    midi(&mut device, &[0xb0, 22, 127]);
    assert_eq!(scene(&device), 2);
    assert!(files.get(GLOBAL_FILE).is_some());

    drop(device);
    let (mut device, _media) = boot(media, &files);
    assert_eq!(view(&device).global.len(), 1);
    assert_ne!(scene(&device), 2);
    midi(&mut device, &[0xb0, 22, 127]);
    assert_eq!(scene(&device), 2);
}

/// A preset mapping is a stored preset edit in the Tetrad field, and it wins over a global
/// mapping with the same trigger.
#[test]
fn preset_mappings_live_in_the_preset_and_win_over_global() {
    let (mut device, _media) = boot(media(), &MemFiles::default());
    let cc = Trigger::Midi(MidiInput::Cc(22));
    for (scope, action) in [
        (
            MappingScope::Global,
            MappingAction::System(SystemAction::Scene(2)),
        ),
        (
            MappingScope::Preset,
            MappingAction::Block {
                position: light(),
                target: BlockTarget::Parameter(0),
            },
        ),
    ] {
        let (_, id) = send(&mut device, MappingCommand::Add(scope)).unwrap();
        let (_, id) = send(
            &mut device,
            MappingCommand::SetTrigger {
                scope,
                id,
                trigger: cc.clone(),
            },
        )
        .unwrap();
        send(&mut device, MappingCommand::SetAction { scope, id, action });
    }
    assert_eq!(gain(&device), 0.5);
    midi(&mut device, &[0xb0, 22, 127]);
    assert_eq!(gain(&device), 1.0);
    assert_eq!(scene(&device), 0, "the global mapping did not run");
    let preset = &device.state().current.as_ref().unwrap().preset;
    assert!(preset.unknown.bytes_field(TETRAD_MAPPINGS_FIELD).is_some());
    let row = &view(&device).preset[0];
    assert_eq!(
        (row.action_label.as_str(), row.action_detail.as_str()),
        ("Light", "GAIN")
    );
}

/// Stomp → block bypass is the stock Stomp-mode assignment: the row moves to the stock data
/// and back when its trigger changes.
#[test]
fn a_stomp_bypass_mapping_is_the_stock_stomp_assignment() {
    let (mut device, _media) = boot(media(), &MemFiles::default());
    let scope = MappingScope::Preset;
    let (_, id) = send(&mut device, MappingCommand::Add(scope)).unwrap();
    let bypass = MappingAction::Block {
        position: light(),
        target: BlockTarget::Bypass,
    };
    let (_, id) = send(
        &mut device,
        MappingCommand::SetAction {
            scope,
            id,
            action: bypass,
        },
    )
    .unwrap();
    assert_eq!(
        id,
        MappingId::Stomp {
            stomp: 0,
            position: light()
        }
    );
    let assignments = |device: &Device<Sim>| {
        let preset = &device.state().current.as_ref().unwrap().preset;
        (preset.stomp_mode_assignments.iter())
            .map(|a| (a.row, a.column, a.stomp_index))
            .collect::<Vec<_>>()
    };
    assert_eq!(assignments(&device), [(0, 2, 0)]);

    let cc = Trigger::Midi(MidiInput::Cc(23));
    let (_, id) = send(
        &mut device,
        MappingCommand::SetTrigger {
            scope,
            id,
            trigger: cc,
        },
    )
    .unwrap();
    assert_eq!(id, MappingId::Tetrad(0));
    assert!(assignments(&device).is_empty());
    midi(&mut device, &[0xb0, 23, 0]);
    let current = device.state().current.as_ref().unwrap();
    let light = current.deployment.layout().instance(Site::Slot(Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    }));
    assert!(
        light.unwrap().bypass.at(current.deployment.scene()),
        "CC 23 = 0 bypasses"
    );

    send(&mut device, MappingCommand::Remove { scope, id });
    assert!(view(&device).preset.is_empty());
}

/// A Stomp-mode press runs the preset's Tetrad mapping for that footswitch (here pressed by
/// MIDI CC#35, which takes the same footswitch path).
#[test]
fn a_stomp_press_runs_its_preset_mapping() {
    let (mut device, _media) = boot(media(), &MemFiles::default());
    let scope = MappingScope::Preset;
    let (_, id) = send(&mut device, MappingCommand::Add(scope)).unwrap();
    assert_eq!(
        view(&device).row(scope, &id).unwrap().trigger,
        Trigger::Stomp(0)
    );
    let action = MappingAction::System(SystemAction::Scene(3));
    send(&mut device, MappingCommand::SetAction { scope, id, action });
    device.push(Event::Action(Action::SetMode(FootswitchMode::Stomp)));
    settle(&mut device);
    midi(&mut device, &[0xb0, 35, 127]);
    assert_eq!(scene(&device), 3);
}

/// The press modes on a parameter, from a note: note-on presses, note-off releases.
#[test]
fn press_modes_drive_a_parameter_from_a_note() {
    let (mut device, _media) = boot(media(), &MemFiles::default());
    let id = map(
        &mut device,
        Trigger::Midi(MidiInput::Note(60)),
        gain_action(),
    );
    let start = gain(&device);
    assert_eq!(start, 0.5);

    // Toggle: A is the value now, B the other value; the release does nothing.
    set_mode(&mut device, &id, ModeKind::Toggle);
    note(&mut device, 60, true);
    let b = gain(&device);
    assert_ne!(b, start);
    note(&mut device, 60, false);
    assert_eq!(gain(&device), b);
    note(&mut device, 60, true);
    assert_eq!(gain(&device), start);

    // Momentary: held is the value, the release restores the value from before, also after an
    // edit of the list while held.
    set_mode(&mut device, &id, ModeKind::Momentary);
    note(&mut device, 60, true);
    assert_ne!(gain(&device), start);
    send(&mut device, MappingCommand::Add(MappingScope::Preset));
    note(&mut device, 60, false);
    assert_eq!(gain(&device), start);

    // Step: +3 steps per press, then -3.
    set_mode(&mut device, &id, ModeKind::Step);
    step(&mut device, &id, FieldId::Amount, 2);
    note(&mut device, 60, true);
    let up = gain(&device);
    assert!(up > start, "{up}");
    step(&mut device, &id, FieldId::Amount, -6);
    note(&mut device, 60, true);
    assert!((gain(&device) - start).abs() < 1e-4);

    // Set: the stepper moves the value; each press sets it.
    set_mode(&mut device, &id, ModeKind::Set);
    step(&mut device, &id, FieldId::Value, -10);
    note(&mut device, 60, true);
    assert!(gain(&device) < start);
    let row = view(&device)
        .row(MappingScope::Preset, &id)
        .cloned()
        .unwrap();
    assert_eq!(row.mode.unwrap().kind, ModeKind::Set);
    assert_eq!(row.fields.len(), 1);
}

/// A CC in Follow maps its window, in Relative steps the value, and with Pickup waits until it
/// reaches the value.
#[test]
fn cc_modes_follow_relative_and_pickup() {
    let (mut device, _media) = boot(media(), &MemFiles::default());
    let id = map(&mut device, Trigger::Midi(MidiInput::Cc(22)), gain_action());

    // Follow with the window 10..=110: below Low is the minimum, above High the maximum.
    step(&mut device, &id, FieldId::Low, 10);
    step(&mut device, &id, FieldId::High, -17);
    midi(&mut device, &[0xb0, 22, 5]);
    assert_eq!(gain(&device), 0.0);
    midi(&mut device, &[0xb0, 22, 60]);
    assert!((gain(&device) - 0.5).abs() < 0.01);
    midi(&mut device, &[0xb0, 22, 120]);
    assert_eq!(gain(&device), 1.0);
    let response = view(&device).preset[0].response.clone().unwrap();
    assert_eq!(
        (response.low, response.high, response.level),
        (10, 110, Some(120))
    );

    // Pickup: after a touch edit to 0.5 the knob at 110 (= 1.0) waits; it catches when it moves
    // across 0.5, then follows.
    step(&mut device, &id, FieldId::Pickup, 1);
    let touch = Action::SetParameter {
        site: light_site(),
        index: 0,
        value: 0.5,
    };
    device.push(Event::Action(touch));
    settle(&mut device);
    midi(&mut device, &[0xb0, 22, 100]);
    assert_eq!(gain(&device), 0.5, "not caught yet");
    assert!(view(&device).preset[0].response.as_ref().unwrap().waiting);
    midi(&mut device, &[0xb0, 22, 40]);
    assert!(
        (gain(&device) - 0.3).abs() < 0.01,
        "caught on the way across"
    );
    midi(&mut device, &[0xb0, 22, 90]);
    assert!((gain(&device) - 0.8).abs() < 0.01);

    // Relative, two's complement: 1 is one step up, 127 one step down.
    set_mode(&mut device, &id, ModeKind::Relative);
    let before = gain(&device);
    midi(&mut device, &[0xb0, 22, 1]);
    let up = gain(&device);
    assert!(up > before);
    midi(&mut device, &[0xb0, 22, 127]);
    assert!((gain(&device) - before).abs() < 1e-4);
    assert!(
        view(&device).preset[0].response.is_none(),
        "Relative has no window"
    );
}

/// A CC window turns a CC into a button (a repeated value presses again, a knob inside the
/// window presses once), and rows with windows side by side map CC values to set values.
#[test]
fn cc_windows_make_buttons_and_value_maps() {
    let (mut device, _media) = boot(media(), &MemFiles::default());
    let bypass = MappingAction::Block {
        position: light(),
        target: BlockTarget::Bypass,
    };
    let id = map(&mut device, Trigger::Midi(MidiInput::Cc(23)), bypass);
    set_mode(&mut device, &id, ModeKind::Toggle);
    assert!(!bypassed(&device));
    midi(&mut device, &[0xb0, 23, 127]);
    assert!(bypassed(&device));
    midi(&mut device, &[0xb0, 23, 127]);
    assert!(!bypassed(&device), "a repeat presses again");
    midi(&mut device, &[0xb0, 23, 0]);
    midi(&mut device, &[0xb0, 23, 70]);
    assert!(bypassed(&device));
    midi(&mut device, &[0xb0, 23, 90]);
    assert!(bypassed(&device), "a knob inside the window presses once");

    // CC 24: 0..=42 sets less gain, 43..=85 the gain now, 86..=127 more.
    let windows = [(-64, -85, -20), (-21, -42, 0), (22, 0, 20)];
    for (low, high, value) in windows {
        let id = map(&mut device, Trigger::Midi(MidiInput::Cc(24)), gain_action());
        set_mode(&mut device, &id, ModeKind::Set);
        step(&mut device, &id, FieldId::Low, low);
        step(&mut device, &id, FieldId::High, high);
        step(&mut device, &id, FieldId::Value, value);
    }
    let summaries: Vec<String> = (view(&device).preset.iter())
        .map(|row| row.summary.clone())
        .filter(|summary| summary.contains("CC "))
        .collect();
    assert!(summaries[1].ends_with("CC 0–42"), "{summaries:?}");
    let start = gain(&device);
    midi(&mut device, &[0xb0, 24, 64]);
    assert_eq!(gain(&device), start);
    midi(&mut device, &[0xb0, 24, 0]);
    assert!(gain(&device) < start);
    midi(&mut device, &[0xb0, 24, 127]);
    assert!(gain(&device) > start);
}

/// A footswitch release reaches a Tetrad mapping (Momentary restores), and on a stock Stomp →
/// bypass row Momentary is the footswitch's stock momentary flag.
#[test]
fn footswitch_releases_and_the_stock_momentary_flag() {
    let (mut device, _media) = boot(media(), &MemFiles::default());
    device.push(Event::Action(Action::SetMode(FootswitchMode::Stomp)));
    settle(&mut device);
    let scope = MappingScope::Preset;
    let id = map(&mut device, Trigger::Stomp(1), gain_action());
    set_mode(&mut device, &id, ModeKind::Momentary);
    let switch_b = Element::all().nth(1).unwrap();
    let start = gain(&device);
    device.push(Event::Hal(HalEvent::Stomp {
        element: switch_b,
        value: 1,
    }));
    settle(&mut device);
    assert_ne!(gain(&device), start, "held");
    device.push(Event::Hal(HalEvent::Stomp {
        element: switch_b,
        value: 0,
    }));
    settle(&mut device);
    assert_eq!(gain(&device), start, "released");

    let bypass = MappingAction::Block {
        position: light(),
        target: BlockTarget::Bypass,
    };
    let stock = map(&mut device, Trigger::Stomp(0), bypass);
    assert!(matches!(stock, MappingId::Stomp { stomp: 0, .. }));
    let momentary = |device: &Device<Sim>| {
        let preset = &device.state().current.as_ref().unwrap().preset;
        preset.stomp_is_momentary.get(0).copied().unwrap_or(false)
    };
    set_mode(&mut device, &stock, ModeKind::Momentary);
    assert!(momentary(&device));
    let row = view(&device).row(scope, &stock).cloned().unwrap();
    assert_eq!(row.mode.unwrap().kind, ModeKind::Momentary);

    // Set is not stock data: the row moves to the Tetrad list.
    set_mode(&mut device, &stock, ModeKind::Set);
    let (_, moved) = view(&device).focus.unwrap();
    assert!(matches!(moved, MappingId::Tetrad(_)), "{moved:?}");
    let preset = &device.state().current.as_ref().unwrap().preset;
    assert!(preset.stomp_mode_assignments.is_empty());
}

//! Mappings end to end on the simulated device: commands as the Mappings page sends them, MIDI
//! on the QC channel, Stomp-mode presses, the global file and the preset field.

mod common;

use std::time::Duration;

use common::{Media, STOCK, Sim, boot_with_files, light_preset};
use tetrad_app::device::mappings::GLOBAL_FILE;
use tetrad_app::device::platform::Input;
use tetrad_app::device::{Action, Device, Event};
use tetrad_engine::{Column, Row, Site, Slot};
use tetrad_preset::model::TETRAD_MAPPINGS_FIELD;
use tetrad_services::fake::MemFiles;
use tetrad_settings::controls::FootswitchMode;
use tetrad_ui::{
    BlockTarget, GridPosition, MappingAction, MappingCommand, MappingId, MappingScope,
    MappingsView, MidiInput, SystemAction, Trigger,
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

fn gain(device: &Device<Sim>) -> f32 {
    let current = device.state().current.as_ref().unwrap();
    let site = Site::Slot(Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    });
    let block = current.deployment.layout().instance(site).unwrap();
    block.params[0].at(current.deployment.scene())
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

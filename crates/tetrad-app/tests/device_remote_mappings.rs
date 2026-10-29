//! Remote's Mappings page (docs/remote.md, docs/mappings.md): the view lists the mappings of the
//! loaded preset and the global ones with the unit's labels, and a command does what a touch on
//! the unit's Mappings page does, with the same refusals. The server is the fake of `common`.

mod common;

use std::time::Duration;

use common::{FakeRemote, Media, STOCK, Sim, boot_remote, light_preset};
use serde_json::{Value, json};
use tetrad_app::device::mappings::GLOBAL_FILE;
use tetrad_app::device::platform::Input;
use tetrad_app::device::remote::SETTINGS_FILE;
use tetrad_app::device::{Device, Event};
use tetrad_remote::{Call, EditorOp, Reply, Request};
use tetrad_services::fake::MemFiles;
use tetrad_ui::{
    BlockTarget, GridPosition, MappingAction, MappingCommand, MappingScope, MappingsView,
    MidiInput, SystemAction, Trigger,
};

const PIN: &str = "0001";

/// A unit with Remote on (PIN 0001) over `files`, and the Light block of [`light_preset`] at
/// column 2 (GAIN is its parameter 0).
fn boot(files: &MemFiles) -> Device<Sim> {
    files.put(SETTINGS_FILE, r#"{"enabled":true}"#);
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let mut device = boot_remote(media, files.clone(), FakeRemote::default()).0;
    settle(&mut device);
    device
}

fn settle(device: &mut Device<Sim>) {
    for _ in 0..40 {
        assert!(device.step().is_none());
    }
}

/// What the unit's own Mappings page shows.
fn unit(device: &Device<Sim>) -> MappingsView {
    device.state().views.mappings.clone()
}

/// Sends one page request and returns the device's reply.
fn ask(device: &mut Device<Sim>, pin: &str, op: EditorOp) -> Reply {
    let replies = |device: &Device<Sim>| device.platform().remote.as_ref().unwrap().replies.clone();
    let id = replies(device).len() as u64 + 1;
    let request = Request::Mappings {
        pin: pin.into(),
        op,
    };
    device.push(Event::Remote(Call { id, request }));
    device.step();
    let (replied, reply) = replies(device).pop().expect("a reply");
    assert_eq!(replied, id);
    reply
}

fn view(device: &mut Device<Sim>) -> Value {
    match ask(device, PIN, EditorOp::View) {
        Reply::Json(view) => view,
        other => panic!("the view: {other:?}"),
    }
}

fn command(device: &mut Device<Sim>, command: Value) -> Reply {
    let json = command.to_string().into_bytes();
    ask(device, PIN, EditorOp::Command { json })
}

/// The view a command answered with; panics on a refusal.
fn applied(device: &mut Device<Sim>, json: Value) -> Value {
    match command(device, json) {
        Reply::Json(view) => view,
        other => panic!("the view after the command: {other:?}"),
    }
}

fn refused(device: &mut Device<Sim>, json: Value) -> String {
    match command(device, json) {
        Reply::Refused(reason) => reason,
        other => panic!("a refusal: {other:?}"),
    }
}

/// The key of the action labelled `label` in group `group` of the scope's action menu.
fn action(view: &Value, scope: &str, group: &str, label: &str) -> String {
    let groups = view["actions"][scope].as_array().unwrap();
    let group = (groups.iter())
        .find(|g| g["label"].as_str().unwrap().starts_with(group))
        .unwrap_or_else(|| panic!("no group {group}"));
    let action = (group["actions"].as_array().unwrap().iter())
        .find(|a| a["label"] == label)
        .unwrap_or_else(|| panic!("no action {label}"));
    action["key"].as_str().unwrap().to_owned()
}

/// Adds a mapping to `scope` and answers its id.
fn add(device: &mut Device<Sim>, scope: &str) -> String {
    let view = applied(device, json!({ "op": "add", "scope": scope }));
    assert_eq!(view["focus"]["scope"], scope);
    view["focus"]["id"].as_str().unwrap().to_owned()
}

fn light() -> GridPosition {
    GridPosition::new(0, 2).unwrap()
}

/// The page shows each list with the labels of the unit, the Type list and the action menu of
/// each scope, and the loaded preset's name.
#[test]
fn the_view_lists_preset_and_global_mappings_with_labels() {
    let mut device = boot(&MemFiles::default());
    let touch = |device: &mut Device<Sim>, command| {
        device.push(Event::Mappings(command));
        settle(device);
        unit(device).focus.unwrap().1
    };
    let id = touch(&mut device, MappingCommand::Add(MappingScope::Preset));
    let scope = MappingScope::Preset;
    let gain = MappingAction::Block {
        position: light(),
        target: BlockTarget::Parameter(0),
    };
    touch(
        &mut device,
        MappingCommand::SetAction {
            scope,
            id,
            action: gain,
        },
    );
    let scope = MappingScope::Global;
    let id = touch(&mut device, MappingCommand::Add(scope));
    let trigger = Trigger::Midi(MidiInput::Cc(22));
    let id = touch(
        &mut device,
        MappingCommand::SetTrigger { scope, id, trigger },
    );
    let scene = MappingAction::System(SystemAction::Scene(2));
    touch(
        &mut device,
        MappingCommand::SetAction {
            scope,
            id,
            action: scene,
        },
    );

    let view = view(&mut device);
    assert_eq!(view["preset"], "Light");
    let preset = &view["rows"]["preset"][0];
    assert_eq!(preset["trigger"], json!({ "kind": "stomp", "stomp": 0 }));
    assert_eq!(preset["triggerLabel"], "Stomp A");
    assert_eq!(preset["actionLabel"], "Light");
    assert_eq!(preset["actionDetail"], "GAIN");
    let global = &view["rows"]["global"][0];
    assert_eq!(global["trigger"], json!({ "kind": "cc", "number": 22 }));
    assert_eq!(global["triggerLabel"], "CC 22");
    assert_eq!(global["actionDetail"], "Scene C");
    assert_eq!(global["learns"], true);
    assert_eq!(
        view["triggers"]["global"],
        json!([{ "kind": "midi", "label": "MIDI (QC channel)" }])
    );
    assert_eq!(view["triggers"]["preset"].as_array().unwrap().len(), 4);
    // Blocks and Send MIDI are preset actions only.
    let labels = |scope: &str| -> Vec<String> {
        (view["actions"][scope].as_array().unwrap().iter())
            .map(|group| group["label"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(labels("global"), ["System"]);
    assert_eq!(labels("preset").len(), 3);
}

/// Add, a trigger and an action from the page give the same mapping as the same touches on the
/// unit, and the global list is saved at once.
#[test]
fn web_edits_give_the_mapping_of_the_touch_path_and_save_it() {
    let web_files = MemFiles::default();
    let mut web = boot(&web_files);
    let id = add(&mut web, "global");
    let trigger = json!({ "kind": "cc", "number": 22 });
    let view = applied(
        &mut web,
        json!({ "op": "trigger", "scope": "global", "id": id, "trigger": trigger }),
    );
    let id = view["focus"]["id"].as_str().unwrap().to_owned();
    let scene = action(&view, "global", "System", "Scene C");
    let view = applied(
        &mut web,
        json!({ "op": "action", "scope": "global", "id": id, "action": scene }),
    );
    assert_eq!(view["rows"]["global"][0]["actionDetail"], "Scene C");
    settle(&mut web);

    let touch_files = MemFiles::default();
    let mut touch = boot(&touch_files);
    let scope = MappingScope::Global;
    for command in [
        MappingCommand::Add(scope),
        MappingCommand::SetTrigger {
            scope,
            id: tetrad_ui::MappingId::Tetrad(0),
            trigger: Trigger::Midi(MidiInput::Cc(22)),
        },
        MappingCommand::SetAction {
            scope,
            id: tetrad_ui::MappingId::Tetrad(0),
            action: MappingAction::System(SystemAction::Scene(2)),
        },
    ] {
        touch.push(Event::Mappings(command));
        settle(&mut touch);
    }
    assert_eq!(unit(&web).global, unit(&touch).global);
    let saved = web_files.get(GLOBAL_FILE).expect("the global file");
    assert_eq!(Some(saved), touch_files.get(GLOBAL_FILE));
}

/// A new mode and a stepper tap change the row as on the unit, whose page shows the same row.
#[test]
fn a_mode_and_a_value_change_follow_the_touch_rules() {
    let mut device = boot(&MemFiles::default());
    let id = add(&mut device, "preset");
    let view = view(&mut device);
    let gain = action(&view, "preset", "Light", "GAIN");
    let view = applied(
        &mut device,
        json!({ "op": "action", "scope": "preset", "id": id, "action": gain }),
    );
    assert_eq!(view["rows"]["preset"][0]["mode"]["kind"], "set");
    let view = applied(
        &mut device,
        json!({ "op": "mode", "scope": "preset", "id": id, "mode": "toggle" }),
    );
    let row = &view["rows"]["preset"][0];
    assert_eq!(row["mode"]["kind"], "toggle");
    let b = row["fields"][1].clone();
    assert_eq!(b["id"], "b");
    let view = applied(
        &mut device,
        json!({ "op": "step", "scope": "preset", "id": id, "field": "b", "ticks": 5 }),
    );
    let stepped = &view["rows"]["preset"][0]["fields"][1]["text"];
    assert_ne!(stepped, &b["text"]);
    settle(&mut device);
    let row = &unit(&device).preset[0];
    assert_eq!(row.mode.as_ref().unwrap().label, "Toggle");
    assert_eq!(&json!(row.fields[1].text), stepped);
}

/// Remove deletes the row and saves the global list without it.
#[test]
fn remove_deletes_the_mapping() {
    let files = MemFiles::default();
    let mut device = boot(&files);
    let id = add(&mut device, "global");
    let view = applied(
        &mut device,
        json!({ "op": "remove", "scope": "global", "id": id }),
    );
    assert_eq!(view["rows"]["global"], json!([]));
    settle(&mut device);
    assert!(unit(&device).global.is_empty());
    let saved = files.get(GLOBAL_FILE).expect("the global file");
    let saved: Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(saved["mappings"], json!([]));
}

/// LEARN waits for the next free MIDI message; the page sees the end in the view.
#[test]
fn learn_takes_the_next_midi_message() {
    let mut device = boot(&MemFiles::default());
    let id = add(&mut device, "global");
    let view = applied(
        &mut device,
        json!({ "op": "learn", "scope": "global", "id": id, "on": true }),
    );
    assert_eq!(view["learning"], json!({ "scope": "global", "id": id }));
    let inputs = device.platform().inputs.clone();
    inputs
        .borrow_mut()
        .extend([0xb0, 21, 127].map(|byte| Input::MidiByte {
            byte,
            at: Duration::ZERO,
        }));
    settle(&mut device);
    let view = self::view(&mut device);
    assert_eq!(view["learning"], Value::Null);
    let row = &view["rows"]["global"][0];
    assert_eq!(row["trigger"], json!({ "kind": "cc", "number": 21 }));
}

/// What the page cannot do gets a reason, from the page's checks or from the bridge, and
/// changes nothing.
#[test]
fn a_bad_command_is_refused_with_a_reason_and_changes_nothing() {
    let mut device = boot(&MemFiles::default());
    let id = add(&mut device, "global");
    let scene = action(&view(&mut device), "global", "System", "Scene C");
    let command = json!({ "op": "action", "scope": "global", "id": id, "action": scene });
    let before = applied(&mut device, command)["rows"].clone();
    let row = |op: &str, extra: Value| {
        let mut command = json!({ "op": op, "scope": "global", "id": id });
        command
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        command
    };
    let cases = [
        (
            json!({ "op": "add", "scope": "both" }),
            "unknown variant `both`",
        ),
        (
            json!({ "op": "copy", "scope": "global", "id": id }),
            "unknown variant `copy`",
        ),
        (
            json!({ "op": "remove", "scope": "global", "id": "Tetrad(9)" }),
            "No mapping Tetrad(9) in this list now",
        ),
        (
            json!({ "op": "remove", "scope": "preset", "id": id }),
            "No mapping Tetrad(0) in this list now",
        ),
        (
            row("action", json!({ "action": "SendMidi" })),
            "SendMidi is not an action of this list",
        ),
        (
            row("mode", json!({ "mode": "relative" })),
            "cannot use the mode relative",
        ),
        (
            row("step", json!({ "field": "a", "ticks": 1 })),
            "has no value a",
        ),
        (
            row("step", json!({ "field": "low", "ticks": 0 })),
            "is not 0",
        ),
        (
            row("step", json!({ "field": "low", "ticks": i32::MIN })),
            "goes from -127 to 127",
        ),
        (
            row(
                "trigger",
                json!({ "trigger": { "kind": "extension", "key": "a/b:c", "trigger": "midi" } }),
            ),
            "a/b:c has no trigger midi now",
        ),
        // The bridge's own refusal, as the unit's page shows it.
        (
            row(
                "trigger",
                json!({ "trigger": { "kind": "cc", "number": 43 } }),
            ),
            "CC 43 is used by the QC's MIDI map",
        ),
        (
            row(
                "trigger",
                json!({ "trigger": { "kind": "stomp", "stomp": 1 } }),
            ),
            "Global mappings use MIDI triggers",
        ),
    ];
    for (json, reason) in cases {
        let refusal = refused(&mut device, json.clone());
        assert!(refusal.contains(reason), "{json}: {refusal}");
    }
    assert_eq!(view(&mut device)["rows"], before);
}

/// Every call needs the PIN, a command too.
#[test]
fn a_wrong_pin_stays_refused() {
    let files = MemFiles::default();
    let mut device = boot(&files);
    assert_eq!(ask(&mut device, "9999", EditorOp::View), Reply::WrongPin);
    let json = json!({ "op": "add", "scope": "global" })
        .to_string()
        .into_bytes();
    let add = EditorOp::Command { json };
    assert_eq!(ask(&mut device, "9999", add), Reply::WrongPin);
    settle(&mut device);
    assert!(unit(&device).global.is_empty());
    assert_eq!(files.get(GLOBAL_FILE), None);
}

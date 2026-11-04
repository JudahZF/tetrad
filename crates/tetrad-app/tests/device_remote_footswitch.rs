//! Remote's Footswitch page (docs/remote.md, docs/footswitch-modes.md): the view shows the
//! layouts of the Custom footswitch mode, and a command does what the touch editor does, with
//! the same layout file and refusals. The server is the fake of `common`.

mod common;

use common::{FakeRemote, Media, STOCK, Sim, boot_remote, light_preset};
use serde_json::{Value, json};
use tetrad_app::device::remote::SETTINGS_FILE;
use tetrad_app::device::{Device, Event, Subsystem};
use tetrad_remote::{Call, EditorOp, Reply, Request};
use tetrad_services::fake::MemFiles;
use tetrad_ui::{CustomCommand, LayoutScope, SlotAction, SlotEdit, SystemAction};

const PIN: &str = "0001";

/// A unit with Remote on (PIN 0001) and the "Light" preset (a block in row 1, slot 3) loaded.
/// The layout file goes to `files`. Keep the media: it removes its folder when dropped.
fn boot() -> (Device<Sim>, MemFiles, Media) {
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let files = MemFiles::default();
    files.put(SETTINGS_FILE, r#"{"enabled":true}"#);
    let (mut device, media) = boot_remote(media, files.clone(), FakeRemote::default());
    for _ in 0..40 {
        assert!(device.step().is_none());
    }
    (device, files, media)
}

fn layout_file(_device: &Device<Sim>, files: &MemFiles) -> Option<Vec<u8>> {
    files.get(tetrad_app::device::custom::LAYOUT_FILE)
}

/// Sends one page request and returns the device's reply.
fn ask(device: &mut Device<Sim>, pin: &str, op: EditorOp) -> Reply {
    let replies = |device: &Device<Sim>| device.platform().remote.as_ref().unwrap().replies.clone();
    let id = replies(device).len() as u64 + 1;
    let request = Request::Footswitch {
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

/// The new view; panics on a refusal.
fn applied(device: &mut Device<Sim>, command: Value) -> Value {
    match self::command(device, command) {
        Reply::Json(view) => view,
        other => panic!("the view after the command: {other:?}"),
    }
}

fn refused(reply: Reply) -> String {
    match reply {
        Reply::Refused(reason) => reason,
        other => panic!("a refusal: {other:?}"),
    }
}

fn edit(scope: &str, page: u8, slot: u8, edit: Value) -> Value {
    json!({ "op": "slot", "scope": scope, "page": page, "slot": slot, "edit": edit })
}

/// The mode is part of Tetrad: the view has it without any extension.
#[test]
fn the_mode_needs_no_extension() {
    let (mut device, _files, _media) = boot();
    assert_eq!(view(&mut device)["mode"]["name"], "Custom");
    let clear = edit("global", 0, 0, json!({ "kind": "clear" }));
    applied(&mut device, clear);
}

/// The view has the mode's pages of ten slots, the two layouts and the choices of the touch
/// action menu for this preset and this mode.
#[test]
fn the_view_lists_the_pages_slots_and_choices() {
    let (mut device, _files, _media) = boot();
    let view = view(&mut device);
    assert_eq!(
        view["mode"],
        json!({ "name": "Custom", "extension": "Tetrad", "pages": 4, "page": 0,
                "active": false })
    );
    let pages = view["layouts"]["global"].as_array().unwrap();
    assert_eq!(pages.len(), 4);
    assert!(
        pages
            .iter()
            .all(|page| page["slots"].as_array().unwrap().len() == 10)
    );
    assert_eq!(pages[0]["slots"][0]["action"], Value::Null);
    assert_eq!(view["layouts"]["preset"].as_array().map(Vec::len), Some(4));
    assert_eq!(view["preset"], "Light");
    assert_eq!(view["colours"].as_array().unwrap().len(), 8);
    assert_eq!(view["setlists"], json!(["My Presets"]));
    let block = &view["blocks"][0];
    assert_eq!((&block["row"], &block["column"]), (&json!(0), &json!(2)));
    let pages: Vec<_> = (view["systems"].as_array().unwrap().iter())
        .filter(|choice| choice["group"] == "Page")
        .map(|choice| choice["action"].clone())
        .collect();
    assert_eq!(
        pages,
        [
            json!({ "customPage": "next" }),
            json!({ "customPage": "previous" }),
            json!({ "customPage": { "go": 0 } }),
            json!({ "customPage": { "go": 1 } }),
            json!({ "customPage": { "go": 2 } }),
            json!({ "customPage": { "go": 3 } }),
        ]
    );
}

/// An action, a label and a colour from the page give the layout file that the same edits
/// on the unit give, the view shows them, and each edit leaves a Remote notice.
#[test]
fn an_edit_from_the_page_writes_the_same_layout_as_the_touch() {
    let (mut device, files, _media) = boot();
    let scene = json!({ "kind": "system", "action": { "scene": 2 } });
    applied(
        &mut device,
        edit("global", 1, 8, json!({ "kind": "action", "action": scene })),
    );
    applied(
        &mut device,
        edit("global", 1, 8, json!({ "kind": "label", "label": "Lead" })),
    );
    let view = applied(
        &mut device,
        edit("global", 1, 8, json!({ "kind": "colour", "colour": 3 })),
    );
    let slot = &view["layouts"]["global"][1]["slots"][8];
    assert_eq!(slot["action"], scene);
    assert_eq!(
        (&slot["label"], &slot["swatch"]),
        (&json!("Lead"), &json!(3))
    );
    assert_eq!(slot["colour"], view["colours"][3]);
    let notices = (device.state().notices.iter())
        .filter(|notice| notice.subsystem == Subsystem::Remote)
        .filter(|notice| notice.detail.starts_with("Remote: footswitch:"))
        .count();
    assert_eq!(notices, 3);
    let from_page = layout_file(&device, &files).expect("the layout file");

    let touch = |edit| CustomCommand::Slot {
        scope: LayoutScope::Global,
        page: 1,
        slot: 8,
        gesture: tetrad_ui::Gesture::Press,
        edit,
    };
    for edit in [
        SlotEdit::Clear,
        first_action(SlotAction::System(SystemAction::Scene(2))),
        SlotEdit::SetLabel("Lead".into()),
        SlotEdit::SetColour(Some(3)),
    ] {
        device.push(Event::Custom(touch(edit)));
        assert!(device.step().is_none());
    }
    assert_eq!(layout_file(&device, &files).unwrap(), from_page);
}

/// Copy puts the same slot on another page; move swaps two slots.
#[test]
fn copy_and_move_a_slot() {
    let (mut device, _files, _media) = boot();
    let tuner = json!({ "kind": "system", "action": "tuner" });
    let bypass = json!({ "kind": "block", "row": 0, "column": 2, "parameter": null });
    applied(
        &mut device,
        edit("global", 0, 0, json!({ "kind": "action", "action": tuner })),
    );
    applied(
        &mut device,
        edit(
            "global",
            0,
            2,
            json!({ "kind": "action", "action": bypass }),
        ),
    );
    applied(
        &mut device,
        edit(
            "global",
            0,
            0,
            json!({ "kind": "copyTo", "page": 1, "slot": 9 }),
        ),
    );
    let view = applied(
        &mut device,
        edit(
            "global",
            0,
            0,
            json!({ "kind": "moveTo", "page": 0, "slot": 2 }),
        ),
    );
    let global = &view["layouts"]["global"];
    assert_eq!(global[1]["slots"][9]["action"], tuner);
    assert_eq!(global[0]["slots"][2]["action"], tuner);
    assert_eq!(global[0]["slots"][0]["action"], bypass);
    // A bypass toggles until another mode is chosen; a second action runs after it.
    let moved = &global[0]["slots"][0]["actions"][0];
    assert_eq!(moved["mode"]["kind"], "toggle");
    let kinds: Vec<_> = (moved["modes"].as_array().unwrap().iter())
        .map(|mode| mode["kind"].clone())
        .collect();
    assert_eq!(kinds, ["toggle", "momentary", "set"]);
    applied(
        &mut device,
        edit(
            "global",
            0,
            0,
            json!({ "kind": "mode", "mode": "momentary" }),
        ),
    );
    let view = applied(
        &mut device,
        edit(
            "global",
            0,
            0,
            json!({ "kind": "action", "index": 1, "action": tuner }),
        ),
    );
    let actions = &view["layouts"]["global"][0]["slots"][0]["actions"];
    assert_eq!(actions[0]["mode"]["kind"], "momentary");
    assert_eq!(actions[1]["action"], tuner);
    let view = applied(
        &mut device,
        edit("global", 0, 0, json!({ "kind": "remove", "index": 0 })),
    );
    let actions = &view["layouts"]["global"][0]["slots"][0]["actions"];
    assert_eq!(actions.as_array().map(Vec::len), Some(1));
    assert_eq!(actions[0]["action"], tuner);
}

/// THIS PRESET plays switch by switch: an empty switch plays the ALL PRESETS one, and Nothing
/// is a preset edit that keeps the switch empty in this preset.
#[test]
fn this_preset_plays_switch_by_switch() {
    let (mut device, files, _media) = boot();
    let tuner = json!({ "kind": "system", "action": "tuner" });
    applied(
        &mut device,
        edit("global", 0, 9, json!({ "kind": "action", "action": tuner })),
    );
    let global = layout_file(&device, &files);
    let view = applied(
        &mut device,
        edit(
            "preset",
            0,
            9,
            json!({ "kind": "action", "action": { "kind": "nothing" } }),
        ),
    );
    assert_eq!(
        view["layouts"]["preset"][0]["slots"][9]["action"],
        json!({ "kind": "nothing" })
    );
    assert_eq!(view["layouts"]["global"][0]["slots"][9]["action"], tuner);
    assert!(device.state().preset_dirty(), "the preset holds its layout");
    assert_eq!(layout_file(&device, &files), global);
    let view = applied(
        &mut device,
        edit("preset", 0, 9, json!({ "kind": "clear" })),
    );
    assert_eq!(
        view["layouts"]["preset"][0]["slots"][9]["action"],
        Value::Null
    );
}

/// "Choose a preset" on the page: a slot of a setlist becomes a Load preset action, as on the
/// unit, without the unit browsing that setlist.
#[test]
fn a_preset_of_a_setlist_becomes_a_load_preset_action() {
    let (mut device, _files, _media) = boot();
    let view = applied(
        &mut device,
        json!({ "op": "presets", "setlist": "My Presets" }),
    );
    assert_eq!(
        view["presets"],
        json!({ "setlist": "My Presets", "slots": [{ "slot": 0, "label": "1A Light" }] })
    );
    let chosen = json!({ "kind": "preset", "setlist": "My Presets", "slot": 0 });
    let view = applied(
        &mut device,
        edit(
            "global",
            0,
            4,
            json!({ "kind": "action", "action": chosen }),
        ),
    );
    let slot = &view["layouts"]["global"][0]["slots"][4];
    assert_eq!(slot["action"], json!({ "kind": "loadPreset" }));
    assert_eq!(
        (&slot["label"], &slot["detail"]),
        (&json!("Light"), &json!("Load preset"))
    );
    let empty = json!({ "kind": "preset", "setlist": "My Presets", "slot": 1 });
    let reason = refused(command(
        &mut device,
        edit("global", 0, 4, json!({ "kind": "action", "action": empty })),
    ));
    assert_eq!(reason, "That slot is empty");
}

/// What the page cannot do gets a reason, and writes no layout.
#[test]
fn a_bad_command_is_refused_and_writes_nothing() {
    let (mut device, files, _media) = boot();
    let tuner = json!({ "kind": "action", "action": { "kind": "system", "action": "tuner" } });
    let cases = [
        (edit("global", 4, 0, tuner.clone()), "There is no page 5"),
        (
            edit(
                "global",
                0,
                0,
                json!({ "kind": "action", "action": { "kind": "system", "action": { "customPage": { "go": 255 } } } }),
            ),
            "There is no page 256",
        ),
        (
            edit("global", 0, 10, tuner.clone()),
            "There is no switch 11",
        ),
        (
            edit(
                "global",
                0,
                0,
                json!({ "kind": "copyTo", "page": 0, "slot": 12 }),
            ),
            "There is no switch 13",
        ),
        (
            json!({ "op": "copyPage", "scope": "global", "from": 0, "to": 4 }),
            "There is no page 5",
        ),
        (
            edit(
                "global",
                0,
                0,
                json!({ "kind": "action", "action": { "kind": "system", "action": { "customSlot": 1 } } }),
            ),
            "A slot cannot press another slot",
        ),
        (
            edit(
                "global",
                0,
                0,
                json!({ "kind": "action", "action": { "kind": "block", "row": 1, "column": 0 } }),
            ),
            "There is no block there",
        ),
        (
            edit(
                "global",
                0,
                0,
                json!({ "kind": "action", "action": { "kind": "command", "key": "a/b:c", "command": "x" } }),
            ),
            "That extension command is not running",
        ),
        (
            edit("global", 0, 0, json!({ "kind": "mode", "mode": "toggle" })),
            "There is no action 1",
        ),
        (
            edit(
                "global",
                0,
                0,
                json!({ "kind": "label", "label": "Seventeen letters" }),
            ),
            "A label has at most 16 characters",
        ),
    ];
    for (bad, expected) in cases {
        assert_eq!(
            refused(command(&mut device, bad.clone())),
            expected,
            "{bad}"
        );
    }
    let unknown = edit(
        "global",
        0,
        0,
        json!({ "kind": "action", "action": { "kind": "tap" } }),
    );
    let reason = refused(command(&mut device, unknown));
    assert!(reason.contains("unknown variant `tap`"), "{reason}");
    assert_eq!(layout_file(&device, &files), None);
    assert!(!device.state().preset_dirty());
}

/// Every call needs the PIN, a command too.
#[test]
fn a_wrong_pin_stays_refused() {
    let (mut device, files, _media) = boot();
    assert_eq!(ask(&mut device, "9999", EditorOp::View), Reply::WrongPin);
    let json = edit(
        "global",
        0,
        0,
        json!({ "kind": "action", "action": { "kind": "sendMidi" } }),
    )
    .to_string()
    .into_bytes();
    let reply = ask(&mut device, "9999", EditorOp::Command { json });
    assert_eq!(reply, Reply::WrongPin);
    assert_eq!(layout_file(&device, &files), None);
}

/// A command names the double press or a combo of a switch with `gesture`; the view shows
/// them. A combo needs two adjacent switches, and the colour belongs to the press.
#[test]
fn double_presses_and_combos_are_edited_from_the_page() {
    let (mut device, files, _media) = boot();
    let tuner = json!({ "kind": "system", "action": "tuner" });
    let gesture = |gesture: Value, edit: Value| {
        json!({ "op": "slot", "scope": "global", "page": 0, "slot": 3, "gesture": gesture,
                "edit": edit })
    };
    let set = json!({ "kind": "action", "action": tuner });
    applied(&mut device, gesture(json!("double"), set.clone()));
    let view = applied(&mut device, gesture(json!({ "combo": 2 }), set.clone()));
    let page = &view["layouts"]["global"][0];
    assert_eq!(page["slots"][3]["double"]["action"], tuner);
    assert_eq!(page["slots"][3]["action"], Value::Null);
    assert_eq!(page["combos"][0]["switches"], json!([2, 3]));
    assert_eq!(page["combos"][0]["action"], tuner);
    let saved: Value = serde_json::from_slice(&layout_file(&device, &files).unwrap()).unwrap();
    assert_eq!(saved["combos"][0]["switches"], json!([2, 3]));

    // D and G are diagonal: no combo.
    let reason = refused(command(&mut device, gesture(json!({ "combo": 6 }), set)));
    assert_eq!(
        reason,
        "A combo is two switches side by side or one above the other"
    );
    let colour = json!({ "kind": "colour", "colour": 1 });
    let reason = refused(command(&mut device, gesture(json!("double"), colour)));
    assert_eq!(
        reason,
        "The colour, COPY and MOVE belong to the switch: choose PRESS"
    );
    let view = applied(
        &mut device,
        gesture(json!({ "combo": 2 }), json!({ "kind": "clear" })),
    );
    assert_eq!(view["layouts"]["global"][0]["combos"], json!([]));
}

/// The first action of a slot.
fn first_action(action: tetrad_ui::SlotAction) -> tetrad_ui::SlotEdit {
    tetrad_ui::SlotEdit::Action {
        index: 0,
        edit: tetrad_ui::ActionEdit::Set(action),
    }
}

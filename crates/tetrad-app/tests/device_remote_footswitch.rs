//! Remote's Footswitch page (docs/remote.md, docs/footswitch-modes.md): the view shows the
//! layouts of the Custom footswitch mode, and a command does what the touch editor does, with
//! the same layout file and refusals. The mode comes from a data-only package served by a fake
//! repository and installed on a real runner thread; the server is the fake of `common`.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use common::{FakeRemote, Media, STOCK, Sim, boot_remote, light_preset};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use tetrad_app::device::extensions::LocalBackend;
use tetrad_app::device::remote::SETTINGS_FILE;
use tetrad_app::device::{Device, Event, Subsystem};
use tetrad_extensions::catalog::sha256_hex;
use tetrad_extensions::package::Package;
use tetrad_extensions::repository::{Fetch, FetchError};
use tetrad_remote::{Call, EditorOp, Reply, Request};
use tetrad_services::fake::MemFiles;
use tetrad_ui::{
    CustomCommand, ExtensionCommand, ExtensionsView, LayoutScope, SlotAction, SlotEdit,
    SystemAction,
};

const SOURCE: &str = "tester/exts";
const PIN: &str = "0001";

/// URL → body, shared with the worker thread.
#[derive(Clone, Default)]
struct Web(Arc<Mutex<HashMap<String, Vec<u8>>>>);

impl Fetch for Web {
    fn get(&mut self, url: &str, max_bytes: usize) -> Result<Vec<u8>, FetchError> {
        match self.0.lock().unwrap().get(url) {
            Some(body) if body.len() > max_bytes => Err(FetchError::TooLarge(max_bytes)),
            Some(body) => Ok(body.clone()),
            None => Err(FetchError::NotFound),
        }
    }
}

/// A signed repository at `SOURCE` with `test.footswitch`: a data-only package with a
/// two-page Custom mode, as the Custom Footswitch extension is.
fn repository() -> Web {
    let manifest = json!({
        "id": "test.footswitch", "name": "Test Footswitch", "version": "1.0.0",
        "description": "Adds a Custom mode.", "api": 4,
        "footswitchMode": { "name": "Custom", "pages": 2 }
    });
    let package = Package {
        manifest: serde_json::from_value(manifest).unwrap(),
        module: None,
        blocks: Default::default(),
        remote_page: None,
    }
    .encode()
    .unwrap();
    let catalog = serde_json::to_vec(&json!({
        "format": 1,
        "extensions": [{
            "id": "test.footswitch", "name": "Test Footswitch", "version": "1.0.0",
            "description": "Adds a Custom mode.", "api": 4,
            "asset": {
                "tag": "v1",
                "name": "test.footswitch-1.0.0.tetradext",
                "size": package.len(),
                "sha256": sha256_hex(&package),
            }
        }]
    }))
    .unwrap();
    let key = SigningKey::from_bytes(&[7; 32]);
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let raw = |file: &str| format!("https://raw.githubusercontent.com/{SOURCE}/HEAD/{file}");
    let web = Web::default();
    let mut files = web.0.lock().unwrap();
    files.insert(raw("tetrad-extensions.json"), catalog.clone());
    let signature = key.sign(&catalog).to_bytes();
    files.insert(raw("tetrad-extensions.sig"), b64(&signature).into_bytes());
    files.insert(
        raw("tetrad-extensions.pub"),
        b64(key.verifying_key().as_bytes()).into_bytes(),
    );
    files.insert(
        format!("https://github.com/{SOURCE}/releases/download/v1/test.footswitch-1.0.0.tetradext"),
        package,
    );
    drop(files);
    web
}

fn extensions(device: &Device<Sim>) -> ExtensionsView {
    device.bridges().extensions.view(&device.state().community)
}

/// Steps the device (the runner and the store worker are real threads) until `done` holds.
fn pump(device: &mut Device<Sim>, what: &str, done: impl Fn(&Device<Sim>) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done(device) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        assert!(device.step().is_none());
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A unit with Remote on (PIN 0001), the "Light" preset (a block in row 1, slot 3) loaded and
/// the Custom mode of `test.footswitch`. The layout file goes to `files`. Keep the media: it
/// removes its folder when dropped.
fn boot() -> (Device<Sim>, MemFiles, Media) {
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let root = media.0.join("tetrad/extensions");
    let backend = LocalBackend::threads().with_worker(root, repository());
    let files = MemFiles::default();
    files.put(SETTINGS_FILE, r#"{"enabled":true}"#);
    let (mut device, _log, media) =
        common::boot_extensions_remote(media, backend, files.clone(), FakeRemote::default());
    pump(&mut device, "the store load", |d| {
        !extensions(d).sources.is_empty()
    });
    device.push(Event::Extensions(ExtensionCommand::AddSource {
        repository: SOURCE.into(),
    }));
    pump(&mut device, "the key", |d| {
        extensions(d).pending_trust.is_some()
    });
    device.push(Event::Extensions(ExtensionCommand::TrustSource {
        repository: SOURCE.into(),
        trust: true,
    }));
    pump(&mut device, "the catalog", |d| {
        !extensions(d).available.is_empty()
    });
    device.push(Event::Extensions(ExtensionCommand::Install {
        source: SOURCE.into(),
        id: "test.footswitch".into(),
    }));
    pump(&mut device, "the provider", |d| {
        d.state().custom.provider().is_some()
    });
    (device, files, media)
}

fn layout_file(device: &Device<Sim>, files: &MemFiles) -> Option<Vec<u8>> {
    files.get(&device.state().custom.provider().unwrap().layout_path())
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

/// Without an extension that adds the mode, the view says so and a command is refused.
#[test]
fn without_a_provider_the_view_says_so() {
    let files = MemFiles::default();
    files.put(SETTINGS_FILE, r#"{"enabled":true}"#);
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let (mut device, _media) = boot_remote(media, files, FakeRemote::default());
    assert_eq!(view(&mut device), json!({ "mode": null }));
    let clear = edit("global", 0, 0, json!({ "kind": "clear" }));
    let reason = refused(command(&mut device, clear));
    assert_eq!(reason, "No extension adds a Custom footswitch mode");
}

/// The view has the mode's pages of ten slots, the two layouts and the choices of the touch
/// action menu for this preset and this mode.
#[test]
fn the_view_lists_the_pages_slots_and_choices() {
    let (mut device, _files, _media) = boot();
    let view = view(&mut device);
    assert_eq!(
        view["mode"],
        json!({ "name": "Custom", "extension": "Test Footswitch", "pages": 2, "page": 0,
                "active": false })
    );
    let pages = view["layouts"]["global"].as_array().unwrap();
    assert_eq!(pages.len(), 2);
    assert!(
        pages
            .iter()
            .all(|page| page.as_array().unwrap().len() == 10)
    );
    assert_eq!(pages[0][0]["action"], Value::Null);
    assert_eq!(view["layouts"]["preset"], Value::Null);
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
    let slot = &view["layouts"]["global"][1][8];
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
        edit,
    };
    for edit in [
        SlotEdit::Clear,
        SlotEdit::SetAction(SlotAction::System(SystemAction::Scene(2))),
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
    assert_eq!(global[1][9]["action"], tuner);
    assert_eq!(global[0][2]["action"], tuner);
    assert_eq!(global[0][0]["action"], bypass);
    assert_eq!(global[0][0]["momentary"], false);
}

/// THIS PRESET starts as a copy of ALL PRESETS, takes its own edits as a preset edit, and goes
/// away again.
#[test]
fn the_preset_layout_can_be_given_and_removed() {
    let (mut device, files, _media) = boot();
    let tuner = json!({ "kind": "system", "action": "tuner" });
    applied(
        &mut device,
        edit("global", 0, 0, json!({ "kind": "action", "action": tuner })),
    );
    let global = layout_file(&device, &files);
    let view = applied(&mut device, json!({ "op": "presetLayout", "own": true }));
    assert_eq!(view["layouts"]["preset"], view["layouts"]["global"]);

    let view = applied(
        &mut device,
        edit("preset", 0, 0, json!({ "kind": "clear" })),
    );
    assert_eq!(view["layouts"]["preset"][0][0]["action"], Value::Null);
    assert_eq!(view["layouts"]["global"][0][0]["action"], tuner);
    assert!(device.state().preset_dirty(), "the preset holds its layout");
    assert_eq!(layout_file(&device, &files), global);

    let view = applied(&mut device, json!({ "op": "presetLayout", "own": false }));
    assert_eq!(view["layouts"]["preset"], Value::Null);
    let reason = refused(command(
        &mut device,
        edit("preset", 0, 0, json!({ "kind": "clear" })),
    ));
    assert_eq!(reason, "This preset uses the layout of all presets");
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
    let slot = &view["layouts"]["global"][0][4];
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
        (edit("global", 2, 0, tuner.clone()), "There is no page 3"),
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
            json!({ "op": "copyPage", "scope": "global", "from": 0, "to": 3 }),
            "There is no page 4",
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
            edit("global", 0, 0, json!({ "kind": "value", "value": 50 })),
            "Only a parameter action has a value",
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

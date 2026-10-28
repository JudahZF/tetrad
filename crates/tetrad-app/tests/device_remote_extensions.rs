//! Remote's Manage page of extensions (docs/remote.md): the list shows an installed extension
//! with its state and settings, and a command does what the touch on Settings → Tetrad →
//! Extensions does. The repository is a signed catalog served by a fake fetcher, the runner a
//! real QuickJS thread, the server the fake of `common`.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use common::{FakeRemote, Media, STOCK, Sim, light_preset, target};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use tetrad_app::device::extensions::LocalBackend;
use tetrad_app::device::platform::TouchSample;
use tetrad_app::device::ui::UiInput;
use tetrad_app::device::{Device, Event};
use tetrad_extensions::catalog::sha256_hex;
use tetrad_extensions::package::Package;
use tetrad_extensions::repository::{Fetch, FetchError};
use tetrad_remote::{Call, ExtensionsOp, Reply, Request};
use tetrad_services::fake::MemFiles;
use tetrad_ui::{Control, ExtensionCommand, ExtensionState, ExtensionsView, Screen, SettingsPage};

const SOURCE: &str = "tester/exts";
const KEY: &str = "tester/exts:test.mapper";
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

/// A signed repository at `SOURCE` with `test.mapper` 1.0.0: a module that does nothing and
/// two settings.
fn repository() -> Web {
    let manifest = json!({
        "id": "test.mapper", "name": "Test Mapper", "version": "1.0.0",
        "description": "Maps CC to gain.", "api": 1, "capabilities": [],
        "settings": [
            { "key": "channel", "label": "MIDI channel", "kind": "choice", "default": 0,
              "options": [{ "value": 2, "label": "2" }, { "value": 3, "label": "3" }] },
            { "key": "relay", "label": "Relay", "kind": "toggle", "default": false }
        ]
    });
    let package = Package {
        manifest: serde_json::from_value(manifest).unwrap(),
        module: Some("export default { activate() {} };".into()),
        blocks: Default::default(),
        remote_page: None,
    }
    .encode()
    .unwrap();
    let catalog = serde_json::to_vec(&json!({
        "format": 1,
        "extensions": [{
            "id": "test.mapper", "name": "Test Mapper", "version": "1.0.0",
            "description": "Maps CC to gain.", "api": 1,
            "asset": {
                "tag": "v1",
                "name": "test.mapper-1.0.0.tetradext",
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
        format!("https://github.com/{SOURCE}/releases/download/v1/test.mapper-1.0.0.tetradext"),
        package,
    );
    drop(files);
    web
}

fn view(device: &Device<Sim>) -> ExtensionsView {
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

fn running(device: &Device<Sim>) -> bool {
    view(device).installed.first().map(|e| &e.state) == Some(&ExtensionState::Running)
}

/// A unit with Remote on (PIN 0001) and `test.mapper` installed from a trusted `SOURCE` and
/// running. The store is under `tetrad/extensions` of the media.
fn boot() -> (Device<Sim>, Media) {
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let root = media.0.join("tetrad/extensions");
    let backend = LocalBackend::threads().with_worker(root, repository());
    let files = MemFiles::default();
    let (mut device, _log, media) =
        common::boot_extensions_remote(media, backend, files, FakeRemote::default());
    pump(&mut device, "the store load", |d| {
        !view(d).sources.is_empty()
    });
    device.push(Event::Extensions(ExtensionCommand::AddSource {
        repository: SOURCE.into(),
    }));
    pump(&mut device, "the key", |d| view(d).pending_trust.is_some());
    device.push(Event::Extensions(ExtensionCommand::TrustSource {
        repository: SOURCE.into(),
        trust: true,
    }));
    pump(&mut device, "the catalog", |d| {
        !view(d).available.is_empty()
    });
    device.push(Event::Extensions(ExtensionCommand::Install {
        source: SOURCE.into(),
        id: "test.mapper".into(),
    }));
    pump(&mut device, "a running extension", running);

    device.push(Event::UiInput(UiInput::Show(Screen::Settings(
        SettingsPage::Remote,
    ))));
    device.step();
    let tap = target(&device.bridges().ui, Control::RemoteToggle);
    for pressed in [true, false] {
        device.push(Event::UiInput(UiInput::Touch(TouchSample {
            pressed,
            ..tap
        })));
        device.step();
    }
    (device, media)
}

/// Sends one page request and returns the device's reply.
fn ask(device: &mut Device<Sim>, pin: &str, op: ExtensionsOp) -> Reply {
    let replies = |device: &Device<Sim>| device.platform().remote.as_ref().unwrap().replies.clone();
    let id = replies(device).len() as u64 + 1;
    let request = Request::Extensions {
        pin: pin.into(),
        op,
    };
    device.push(Event::Remote(Call { id, request }));
    device.step();
    let (replied, reply) = replies(device).pop().expect("a reply");
    assert_eq!(replied, id);
    reply
}

fn list(device: &mut Device<Sim>) -> Value {
    match ask(device, PIN, ExtensionsOp::List) {
        Reply::Json(list) => list,
        other => panic!("the list: {other:?}"),
    }
}

fn command(device: &mut Device<Sim>, command: Value) -> Reply {
    let json = command.to_string().into_bytes();
    ask(device, PIN, ExtensionsOp::Command { json })
}

fn refused(reply: Reply) -> String {
    match reply {
        Reply::Refused(reason) => reason,
        other => panic!("a refusal: {other:?}"),
    }
}

/// The list has what the unit's page shows: the state, the settings with their values, and
/// nothing left to install from the repository.
#[test]
fn the_list_shows_an_installed_extension_with_its_state_and_settings() {
    let (mut device, _media) = boot();
    let list = list(&mut device);
    let extension = &list["installed"][0];
    assert_eq!(extension["key"], KEY);
    assert_eq!(extension["name"], "Test Mapper");
    assert_eq!(extension["version"], "1.0.0");
    assert_eq!(extension["source"], SOURCE);
    assert_eq!(extension["state"], "running");
    assert_eq!(extension["fault"], Value::Null);
    assert_eq!(
        extension["settings"],
        json!([
            { "key": "channel", "label": "MIDI channel", "kind": "choice",
              "options": ["2", "3"], "value": 0 },
            { "key": "relay", "label": "Relay", "kind": "toggle", "value": false },
        ])
    );
    assert_eq!(list["available"], json!([]));
    assert_eq!(list["busy"], false);
}

/// "disable" and "enable" switch the extension as the Enabled switch of the unit does, and the
/// answer is the new list.
#[test]
fn disable_and_enable_switch_the_extension_as_the_touch_does() {
    let (mut device, _media) = boot();
    let Reply::Json(answer) = command(&mut device, json!({ "op": "disable", "key": KEY })) else {
        panic!("the list after the command");
    };
    assert_eq!(answer["installed"][0]["enabled"], false);
    assert_eq!(answer["installed"][0]["state"], "off");
    // The unit's own pages draw from the synced copy, which follows the same step.
    assert!(!device.state().views.extensions.installed[0].enabled);

    command(&mut device, json!({ "op": "enable", "key": KEY }));
    pump(&mut device, "the restart", running);
    assert!(view(&device).installed[0].enabled);
}

/// A new setting is saved in the store, as on the unit.
#[test]
fn a_setting_change_is_saved() {
    let (mut device, media) = boot();
    let reply = command(
        &mut device,
        json!({ "op": "setting", "key": KEY, "setting": "channel", "value": 1 }),
    );
    let Reply::Json(answer) = reply else {
        panic!("the list after the command");
    };
    assert_eq!(answer["installed"][0]["settings"][0]["value"], 1);
    let config = media.0.join("tetrad/extensions/config");
    pump(&mut device, "the saved config", |_| {
        std::fs::read_dir(&config).is_ok_and(|mut files| {
            files.any(|f| {
                std::fs::read_to_string(f.unwrap().path())
                    .is_ok_and(|t| t.contains(r#""channel":3"#))
            })
        })
    });
}

/// What the page cannot do gets a reason, and changes nothing.
#[test]
fn an_unknown_op_or_key_is_refused_with_a_reason() {
    let (mut device, _media) = boot();
    let reason = refused(command(&mut device, json!({ "op": "trust", "key": KEY })));
    assert!(reason.contains("unknown variant `trust`"), "{reason}");
    let reason = refused(command(
        &mut device,
        json!({ "op": "disable", "key": "a/b:c" }),
    ));
    assert_eq!(reason, "a/b:c is not installed");
    let reason = refused(command(
        &mut device,
        json!({ "op": "setting", "key": KEY, "setting": "channel", "value": 2 }),
    ));
    assert_eq!(reason, "2 is not a value of MIDI channel");
    let reason = refused(command(
        &mut device,
        json!({ "op": "install", "source": SOURCE, "id": "test.other" }),
    ));
    assert!(reason.contains("does not offer test.other"), "{reason}");
    assert!(view(&device).installed[0].enabled);
}

/// Every call needs the PIN, a command too.
#[test]
fn a_wrong_pin_stays_refused() {
    let (mut device, _media) = boot();
    assert_eq!(
        ask(&mut device, "9999", ExtensionsOp::List),
        Reply::WrongPin
    );
    let json = json!({ "op": "disable", "key": KEY })
        .to_string()
        .into_bytes();
    let disable = ExtensionsOp::Command { json };
    assert_eq!(ask(&mut device, "9999", disable), Reply::WrongPin);
    assert!(view(&device).installed[0].enabled);
}

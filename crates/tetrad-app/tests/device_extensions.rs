//! Extensions end to end on the simulated device: a signed GitHub-style repository served by a
//! fake fetcher, the trust flow, install, a real QuickJS runner on a thread, MIDI on a reserved
//! channel turning into a stored preset edit, persistence, and an offline restart.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use common::{Media, STOCK, Sim, light_preset};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;
use tetrad_app::device::extensions::LocalBackend;
use tetrad_app::device::platform::Input;
use tetrad_app::device::ui::UiInput;
use tetrad_app::device::{Device, Event};
use tetrad_engine::{Column, Row, Site, Slot};
use tetrad_extensions::catalog::sha256_hex;
use tetrad_extensions::package::Package;
use tetrad_extensions::repository::{Fetch, FetchError};
use tetrad_ui::{
    Control, ExtensionCommand, ExtensionState, ExtensionsView, GridPosition, InputEvent,
    ParamControl,
};

const SOURCE: &str = "tester/exts";
const KEY: &str = "tester/exts:test.mapper";

/// Maps CC on its configured channel to GAIN of the Light block in row 1, column 3, and
/// remembers the last value in its storage. Reports native control changes on its status line.
const MODULE: &str = r#"
export default {
  activate(ctx) {
    ctx.midi.reserveChannel(ctx.config.channel);
    ctx.midi.onMessage(async (message, scope) => {
      if (message.kind !== "cc") return;
      const gain = { site: "slot", row: 0, column: 2, model: 1, control: { kind: "parameter", index: 0 } };
      await scope.controls.set(gain, message.value / 127);
      await scope.storage.set("last", message.value);
      await scope.ui.setStatus([`CC ${message.controller} = ${message.value}`]);
    });
    ctx.controls.onChange(async (change, scope) => {
      await scope.ui.setStatus([`${change.binding.control.kind} = ${change.value}`]);
    });
  },
};
"#;

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

/// A signed repository at `SOURCE` with one release of `test.mapper`.
fn repository() -> Web {
    let manifest = json!({
        "id": "test.mapper",
        "name": "Test Mapper",
        "version": "1.0.0",
        "description": "Maps CC to gain.",
        "api": 1,
        "capabilities": ["midiReceive", "controlsRead", "controlsEdit", "storage"],
        "settings": [{
            "key": "channel",
            "label": "MIDI channel",
            "kind": "choice",
            "options": [{ "value": 2, "label": "2" }, { "value": 3, "label": "3" }],
            "default": 0
        }]
    });
    let package = Package {
        manifest: serde_json::from_value(manifest).unwrap(),
        module: MODULE.into(),
    }
    .encode()
    .unwrap();
    let catalog = serde_json::to_vec(&json!({
        "format": 1,
        "extensions": [{
            "id": "test.mapper",
            "name": "Test Mapper",
            "version": "1.0.0",
            "description": "Maps CC to gain.",
            "api": 1,
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

fn boot(media: Media, web: Web) -> (Device<Sim>, Media) {
    let root = media.0.join("tetrad/extensions");
    let backend = LocalBackend::threads().with_worker(root, web);
    let (mut device, _log, media) = common::boot_extensions(media, backend);
    while !device.state().splash_dismissed {
        assert!(device.step().is_none());
    }
    (device, media)
}

fn view(device: &Device<Sim>) -> ExtensionsView {
    device.bridges().extensions.view()
}

/// Steps the device (runners and the store worker are real threads) until `done` holds.
fn pump(device: &mut Device<Sim>, what: &str, done: impl Fn(&Device<Sim>) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done(device) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {:#?}",
            view(device)
        );
        assert!(device.step().is_none());
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn state_of(device: &Device<Sim>) -> Option<ExtensionState> {
    let view = view(device);
    let installed = view.installed.iter().find(|e| e.key == KEY)?;
    Some(installed.state.clone())
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

fn midi(device: &Device<Sim>, bytes: &[u8]) {
    let inputs = device.platform().inputs.clone();
    inputs
        .borrow_mut()
        .extend((bytes.iter()).map(|&byte| Input::MidiByte {
            byte,
            at: Duration::ZERO,
        }));
}

/// Adds and trusts `SOURCE`, installs `test.mapper` and waits until it runs.
fn install(device: &mut Device<Sim>) {
    pump(device, "the store load", |d| !view(d).sources.is_empty());
    device.push(Event::Extensions(ExtensionCommand::AddSource {
        repository: format!("https://github.com/{SOURCE}"),
    }));
    pump(device, "the key", |d| view(d).pending_trust.is_some());
    let trust = view(device).pending_trust.unwrap();
    assert_eq!(trust.repository, SOURCE);
    device.push(Event::Extensions(ExtensionCommand::TrustSource {
        repository: trust.repository,
        trust: true,
    }));
    pump(device, "the catalog", |d| !view(d).available.is_empty());
    let available = &view(device).available[0];
    assert_eq!(
        (available.source.as_str(), available.id.as_str()),
        (SOURCE, "test.mapper")
    );
    device.push(Event::Extensions(ExtensionCommand::Install {
        source: SOURCE.into(),
        id: "test.mapper".into(),
    }));
    pump(device, "a running extension", |d| {
        state_of(d) == Some(ExtensionState::Running)
    });
}

#[test]
fn install_from_a_trusted_repository_then_map_midi_to_a_block() {
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let (mut device, media) = boot(media, repository());
    install(&mut device);
    assert_eq!(device.bridges().extensions.reserved_channels(), 1 << 1);

    // CC 20 = 127 on channel 2 reaches the extension, not native dispatch, and becomes a
    // native stored edit of GAIN.
    assert_eq!(gain(&device), 0.5);
    midi(&device, &[0xb1, 20, 127]);
    pump(&mut device, "the mapped edit", |d| gain(d) == 1.0);
    pump(&mut device, "the status line", |d| {
        view(d).installed[0].status_lines == ["CC 20 = 127"]
    });
    // The pages draw from the synced copy, which is rebuilt only after extension steps.
    assert_eq!(device.state().views.extensions, view(&device));
    let storage = media.0.join("tetrad/extensions/kv");
    pump(&mut device, "the saved storage", |_| {
        std::fs::read_dir(&storage).is_ok_and(|mut files| {
            files.any(|f| {
                std::fs::read_to_string(f.unwrap().path()).is_ok_and(|t| t.contains("127"))
            })
        })
    });

    // Channel 3 is not reserved: the extension never hears it.
    midi(&device, &[0xb2, 20, 0]);
    for _ in 0..50 {
        assert!(device.step().is_none());
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(gain(&device), 1.0);

    // A restart with no network runs the installed extension from the local store.
    drop(device);
    let (mut device, _media) = boot(media, Web::default());
    pump(&mut device, "the offline start", |d| {
        state_of(d) == Some(ExtensionState::Running)
    });
    assert_eq!(view(&device).installed[0].version, "1.0.0");
}

#[test]
fn a_new_setting_restarts_the_extension_on_the_new_channel() {
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let (mut device, _media) = boot(media, repository());
    install(&mut device);

    device.push(Event::Extensions(ExtensionCommand::SetSetting {
        key: KEY.into(),
        setting: "channel".into(),
        value: tetrad_ui::ExtensionSettingInput::Choice(1),
    }));
    pump(&mut device, "the channel 3 reservation", |d| {
        d.bridges().extensions.reserved_channels() == 1 << 2
            && state_of(d) == Some(ExtensionState::Running)
    });
    midi(&device, &[0xb2, 20, 0]);
    pump(&mut device, "the edit from channel 3", |d| gain(d) == 0.0);

    device.push(Event::Extensions(ExtensionCommand::SetEnabled {
        key: KEY.into(),
        enabled: false,
    }));
    pump(&mut device, "the disabled extension", |d| {
        state_of(d) == Some(ExtensionState::Disabled)
            && d.bridges().extensions.reserved_channels() == 0
    });
}

/// MIDI Mapper learns from touch: an edit made on the screen (wrapped for undo) reaches the
/// extension's control watcher.
#[test]
fn a_touch_edit_reaches_control_watchers() {
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let (mut device, _media) = boot(media, repository());
    install(&mut device);

    for control in [
        Control::SelectCell(GridPosition::new(0, 2).unwrap()),
        Control::Param(ParamControl::Bypass),
    ] {
        device.push(Event::UiInput(UiInput::View(InputEvent::Activate(control))));
        assert!(device.step().is_none());
    }
    pump(&mut device, "the reported bypass", |d| {
        view(d).installed[0].status_lines == ["bypass = 1"]
    });
}

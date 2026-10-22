//! Extensions end to end on the simulated device: a signed GitHub-style repository served by a
//! fake fetcher, the trust flow, install, a real QuickJS runner on a thread, MIDI on a reserved
//! channel turning into a stored preset edit, persistence, and an offline restart. The bridge
//! alone: declared triggers and the firings it hands to the device.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use common::{Media, STOCK, Sim, light_preset};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;
use tetrad_app::device::extensions::{
    Capabilities, Extensions, FiredTrigger, Installation, LocalBackend, RunState,
};
use tetrad_app::device::platform::Input;
use tetrad_app::device::ui::UiInput;
use tetrad_app::device::{Device, Event};
use tetrad_engine::{Column, Row, Site, Slot};
use tetrad_extensions::catalog::sha256_hex;
use tetrad_extensions::package::Package;
use tetrad_extensions::repository::{Fetch, FetchError};
use tetrad_midi::{Channel, MidiMessage, U7};
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

/// A signed repository at `SOURCE` with one release of `test.mapper` ([`MODULE`]).
fn repository() -> Web {
    repository_with(
        MODULE,
        &["midiReceive", "controlsRead", "controlsEdit", "storage"],
    )
}

/// [`repository`] with another module and capabilities.
fn repository_with(module: &str, capabilities: &[&str]) -> Web {
    let manifest = json!({
        "id": "test.mapper",
        "name": "Test Mapper",
        "version": "1.0.0",
        "description": "Maps CC to gain.",
        "api": 1,
        "capabilities": capabilities,
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
        module: module.into(),
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

/// An edit made on the screen (wrapped for undo) reaches the extension's control watcher.
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

/// Declares one trigger on its reserved channel and fires it for each CC.
const TRIGGERS: &str = r#"
export default {
  activate(ctx) {
    ctx.midi.reserveChannel(3);
    ctx.triggers.declare({ id: "midi", label: "Ch 3" });
    ctx.midi.onMessage((message, scope) => scope.triggers.fire({ trigger: "midi",
      source: `cc:${message.controller}`, label: `CC ${message.controller}`,
      value: message.value / 127, continuous: true }));
  },
};
"#;

/// The extensions bridge on its own, with thread runners. The device only lends its state.
struct Bridge {
    device: Device<Sim>,
    media: Media,
    extensions: Extensions,
    backend: LocalBackend,
}

impl Bridge {
    fn new() -> Self {
        let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
        let (device, _log, media) = common::boot(media);
        Self {
            device,
            media,
            extensions: Extensions::new(),
            backend: LocalBackend::threads(),
        }
    }

    /// Runs [`TRIGGERS`] as installation `tester/exts:<id>`.
    fn install(&mut self, id: &str, name: &str, triggers: bool) -> String {
        let module = self.media.0.join(format!("{id}.mjs"));
        std::fs::write(&module, TRIGGERS).unwrap();
        let installation = Installation {
            key: format!("{SOURCE}:{id}"),
            id: id.into(),
            name: name.into(),
            version: "1.0.0".into(),
            description: String::new(),
            source: SOURCE.into(),
            module,
            capabilities: Capabilities {
                midi_receive: true,
                triggers,
                ..Capabilities::default()
            },
            config: json!({}),
            storage: Default::default(),
            enabled: true,
            data_only: false,
        };
        let key = installation.key.clone();
        let mut installations: Vec<Installation> = (self.extensions.keys())
            .filter_map(|k| self.extensions.installation(k).cloned())
            .collect();
        installations.push(installation);
        self.extensions
            .set_installations(installations, &mut self.backend);
        key
    }

    /// Feeds runner output to the bridge until `done` holds; returns the firings on the way.
    fn pump(
        &mut self,
        what: &str,
        done: impl Fn(&Extensions, &[FiredTrigger]) -> bool,
    ) -> Vec<FiredTrigger> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut fired = Vec::new();
        while !done(&self.extensions, &fired) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            let mut inputs = Vec::new();
            self.backend.drain(&mut inputs);
            for input in inputs {
                let Input::Extension(input) = input else {
                    continue;
                };
                let state = self.device.state();
                let outcome = self
                    .extensions
                    .on_input(input, state, None, &mut self.backend);
                fired.extend(outcome.triggers);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        fired
    }
}

#[test]
fn running_extensions_offer_their_declared_triggers() {
    let mut bridge = Bridge::new();
    let key = bridge.install("test.pads", "Pads", true);
    bridge.pump("the running extension", |e, _| {
        e.state(&key) == Some(&RunState::Running)
    });
    assert_eq!(
        bridge.extensions.declared_triggers(),
        [tetrad_ui::ExtensionTriggerChoice {
            key: key.clone(),
            trigger: "midi".into(),
            label: "Pads · Ch 3".into(),
        }]
    );

    // Without the triggers capability the activation is refused and nothing is offered.
    let other = bridge.install("test.other", "Other", false);
    bridge.pump("the refused activation", |e, _| {
        matches!(e.state(&other), Some(RunState::Faulted(_)))
    });
    let Some(RunState::Faulted(reason)) = bridge.extensions.state(&other) else {
        unreachable!()
    };
    assert!(reason.contains("triggers capability"), "{reason}");
    assert_eq!(bridge.extensions.declared_triggers().len(), 1);
}

#[test]
fn midi_on_a_reserved_channel_fires_the_declared_trigger() {
    let mut bridge = Bridge::new();
    let key = bridge.install("test.pads", "Pads", true);
    bridge.pump("the running extension", |e, _| {
        e.state(&key) == Some(&RunState::Running)
    });
    let message = MidiMessage::ControlChange {
        channel: Channel::from_number(3).unwrap(),
        controller: U7::new(20).unwrap(),
        value: U7::new(127).unwrap(),
    };
    let outcome = bridge.extensions.on_midi(&message, &mut bridge.backend);
    assert!(outcome.triggers.is_empty(), "firings come from the runner");
    let fired = bridge.pump("the firing", |_, fired| !fired.is_empty());
    assert_eq!(
        fired,
        [FiredTrigger {
            key,
            trigger: "midi".into(),
            source: "cc:20".into(),
            label: "CC 20".into(),
            value: 1.0,
            continuous: true,
        }]
    );
}

/// The whole chain on the device: MIDI on the extension's channel fires its trigger, LEARN puts
/// the firing on a preset mapping, and the next firing edits the block.
#[test]
fn an_extension_trigger_runs_a_mapping() {
    use tetrad_ui::{BlockTarget, GridPosition, MappingAction, MappingCommand, MappingScope};
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let (mut device, _media) = boot(
        media,
        repository_with(TRIGGERS, &["midiReceive", "triggers"]),
    );
    install(&mut device);
    let scope = MappingScope::Preset;
    let focus = |d: &Device<Sim>| d.state().views.mappings.focus.clone();
    device.push(Event::Mappings(MappingCommand::Add(scope)));
    pump(&mut device, "the new mapping", |d| focus(d).is_some());
    let (_, id) = focus(&device).unwrap();
    device.push(Event::Mappings(MappingCommand::Learn {
        scope,
        id,
        on: true,
    }));
    pump(&mut device, "LEARN", |d| {
        d.state().views.mappings.learning.is_some()
    });
    midi(&device, &[0xb2, 20, 127]);
    pump(&mut device, "the learned trigger", |d| {
        d.state()
            .views
            .mappings
            .preset
            .iter()
            .any(|row| row.trigger_label == "CC 20")
    });
    let row = (device.state().views.mappings.preset.iter())
        .find(|row| row.trigger_label == "CC 20")
        .cloned()
        .unwrap();
    assert_eq!(row.trigger_detail, "Test Mapper · Ch 3");
    let action = MappingAction::Block {
        position: GridPosition::new(0, 2).unwrap(),
        target: BlockTarget::Parameter(0),
    };
    device.push(Event::Mappings(MappingCommand::SetAction {
        scope,
        id: row.id,
        action,
    }));
    midi(&device, &[0xb2, 20, 0]);
    pump(&mut device, "the mapped edit", |d| gain(d) == 0.0);
}

/// Choosing an extension's channel as the QC's own MIDI channel faults the extension and gives
/// the channel back to native MIDI.
#[test]
fn the_native_channel_takes_a_reserved_channel_back() {
    use tetrad_app::device::Action;
    use tetrad_ui::{SettingId, SettingValue};
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let (mut device, _media) = boot(media, repository());
    install(&mut device);
    assert_eq!(device.bridges().extensions.reserved_channels(), 1 << 1);
    device.push(Event::Action(Action::ChangeSetting {
        id: SettingId::MidiChannel,
        value: SettingValue::Choice(1),
    }));
    pump(&mut device, "the fault", |d| {
        matches!(state_of(d), Some(ExtensionState::Faulted(_)))
            && d.bridges().extensions.reserved_channels() == 0
    });
}

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
use tetrad_services::fake::MemFiles;
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
    repository_of(
        json!({
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
        }),
        Some(module),
    )
}

/// A signed repository at `SOURCE` with one release of `test.mapper` 1.0.0: `manifest` (its
/// ID, name, version and description as above) and `module`.
fn repository_of(manifest: serde_json::Value, module: Option<&str>) -> Web {
    let api = manifest["api"].clone();
    let package = Package {
        manifest: serde_json::from_value(manifest).unwrap(),
        module: module.map(Into::into),
        blocks: Default::default(),
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
            "api": api,
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

/// Serves every file of `SOURCE` in `web` again under `to`.
fn mirror(web: &Web, to: &str) {
    let mut files = web.0.lock().unwrap();
    let copies: Vec<_> = (files.iter())
        .map(|(url, body)| (url.replace(SOURCE, to), body.clone()))
        .collect();
    files.extend(copies);
}

fn boot(media: Media, web: Web) -> (Device<Sim>, Media) {
    boot_over(media, web, MemFiles::default())
}

/// [`boot`] over the service files `files`.
fn boot_over(media: Media, web: Web, files: MemFiles) -> (Device<Sim>, Media) {
    let root = media.0.join("tetrad/extensions");
    let backend = LocalBackend::threads().with_worker(root, web);
    let (mut device, _log, media) = common::boot_extensions(media, backend, files);
    while !device.state().splash_dismissed {
        assert!(device.step().is_none());
    }
    (device, media)
}

fn view(device: &Device<Sim>) -> ExtensionsView {
    device.bridges().extensions.view(&device.state().community)
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

/// The same extension on from two sources doubles its blocks and handlers: its Available row,
/// the install message and both Installed rows say so until one is off.
#[test]
fn a_second_source_of_an_installed_extension_warns() {
    const MIRROR: &str = "tester/mirror";
    let web = repository();
    mirror(&web, MIRROR);
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let (mut device, _media) = boot(media, web);
    install(&mut device);
    assert_eq!(
        view(&device).status.as_deref(),
        Some("Installed Test Mapper 1.0.0")
    );
    device.push(Event::Extensions(ExtensionCommand::AddSource {
        repository: format!("https://github.com/{MIRROR}"),
    }));
    pump(&mut device, "the key", |d| view(d).pending_trust.is_some());
    device.push(Event::Extensions(ExtensionCommand::TrustSource {
        repository: MIRROR.into(),
        trust: true,
    }));
    pump(&mut device, "the catalog", |d| {
        !view(d).available.is_empty()
    });
    let available = &view(&device).available[0];
    assert_eq!(available.source, MIRROR);
    assert_eq!(available.duplicates, [SOURCE]);
    device.push(Event::Extensions(ExtensionCommand::Install {
        source: MIRROR.into(),
        id: "test.mapper".into(),
    }));
    pump(&mut device, "the second install", |d| {
        view(d).installed.len() == 2
    });
    assert_eq!(
        view(&device).status.as_deref(),
        Some("Also installed from tester/exts: keep one on (Test Mapper 1.0.0)")
    );
    let duplicates = |d: &Device<Sim>| -> Vec<Vec<String>> {
        (view(d).installed.iter())
            .map(|e| e.duplicates.clone())
            .collect()
    };
    assert_eq!(duplicates(&device), [[MIRROR], [SOURCE]]);
    device.push(Event::Extensions(ExtensionCommand::SetEnabled {
        key: format!("{MIRROR}:test.mapper"),
        enabled: false,
    }));
    pump(&mut device, "the cleared warnings", |d| {
        duplicates(d).iter().all(Vec::is_empty)
    });
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
            module: Some(module),
            blocks: Vec::new(),
            block_irs: Default::default(),
            generated: Default::default(),
            capabilities: Capabilities {
                midi_receive: true,
                triggers,
                ..Capabilities::default()
            },
            config: json!({}),
            storage: Default::default(),
            enabled: true,
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
            pressed: true,
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

/// Builds a `tanh` shaper from each `.curve` asset (its text is the drive); throws on "bad" and
/// never returns on "loop".
const SHAPER: &str = r#"
export default {
  activate(ctx) {
    ctx.blocks.provide("shape", (asset) => {
      if (asset.text.trim() === "bad") throw new Error("bad curve");
      if (asset.text.trim() === "loop") for (;;) {}
      const dsp = ctx.blocks.builder();
      const out = dsp.tanh(dsp.mul(dsp.input(), dsp.constant(Number(asset.text))));
      return dsp.finish(out);
    });
  },
};
"#;

/// The DSP blocks of an installation: its static block, one generated block per asset file
/// (other files are left out), a logged build error for a file the provider refuses, and a
/// block that goes when its file goes.
#[test]
fn installations_offer_static_blocks_and_one_block_per_asset() {
    use tetrad_extensions::manifest::{AssetSpec, BlockSpec};
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let (device, _log, media) = common::boot(media);
    let root = media.0.join("tetrad/extensions");
    let mut backend = LocalBackend::threads().with_worker(root, Web::default());
    let mut extensions = Extensions::new();
    let assets = media.0.join("tetrad/assets/test.shaper");
    std::fs::create_dir_all(&assets).unwrap();
    for (file, text) in [
        ("soft.curve", "2"),
        ("hard.curve", "8"),
        ("broken.curve", "bad"),
        ("loop.curve", "loop"),
        ("notes.txt", "1"),
    ] {
        std::fs::write(assets.join(file), text).unwrap();
    }
    let module = media.0.join("shaper.mjs");
    std::fs::write(&module, SHAPER).unwrap();
    let block = |id: &str, name: &str, generated: bool| BlockSpec {
        id: id.into(),
        name: name.into(),
        category: qcdsp::Category::GuitarOverdrive,
        generated,
        asset: generated.then(|| AssetSpec {
            label: "Curve".into(),
            extensions: vec![".curve".into()],
        }),
        placeholder: None,
    };
    let key = format!("{SOURCE}:test.shaper");
    let installation = Installation {
        key: key.clone(),
        id: "test.shaper".into(),
        name: "Shaper".into(),
        version: "1.0.0".into(),
        description: String::new(),
        source: SOURCE.into(),
        module: Some(module),
        blocks: vec![block("gain", "Gain", false), block("shape", "Shaper", true)],
        block_irs: [("gain".to_string(), qcdsp::examples::gain().unwrap())].into(),
        generated: Default::default(),
        capabilities: Capabilities {
            blocks: true,
            ..Capabilities::default()
        },
        config: json!({}),
        storage: Default::default(),
        enabled: true,
    };
    let outcome = extensions.set_installations(vec![installation], &mut backend);
    assert!(outcome.community_changed);
    let pump = |extensions: &mut Extensions,
                backend: &mut LocalBackend,
                what: &str,
                done: &dyn Fn(&Extensions) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(extensions) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            let mut inputs = Vec::new();
            backend.drain(&mut inputs);
            for input in inputs {
                if let Input::Extension(input) = input {
                    extensions.on_input(input, device.state(), None, backend);
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    let keys = |extensions: &Extensions| {
        let mut keys: Vec<String> = (extensions.community_sources().into_iter())
            .map(|source| format!("{} = {}", source.key, source.name))
            .collect();
        keys.sort();
        keys
    };
    let error = |extensions: &Extensions| {
        (extensions.log(&key).into_iter()).find(|line| line.contains("bad curve"))
    };
    // "loop" runs past the build time limit: the runner faults once, and after RETRY that file
    // is not built again while the others are.
    pump(&mut extensions, &mut backend, "the build fault", &|e| {
        matches!(e.state(&key), Some(RunState::Faulted(_)))
    });
    extensions.retry(&key, &mut backend);
    pump(
        &mut extensions,
        &mut backend,
        "both asset blocks and the error",
        &|e| keys(e).len() == 3 && error(e).is_some(),
    );
    assert_eq!(
        extensions.state(&key),
        Some(&RunState::Running),
        "loop.curve did not run again"
    );
    assert_eq!(
        keys(&extensions),
        [
            format!("{key}/gain = Gain"),
            format!("{key}/shape#hard = Shaper hard"),
            format!("{key}/shape#soft = Shaper soft"),
        ]
    );
    let soft = (extensions.community_sources().into_iter())
        .find(|source| source.key.ends_with("#soft"))
        .unwrap();
    let mut processor = qcdsp::Processor::new(soft.ir).unwrap();
    assert!((processor.process_sample(0.25) - 0.5_f32.tanh()).abs() < 1e-4);
    let line = error(&extensions).unwrap();
    assert!(line.contains("shape (broken)"), "{line}");

    // A file that goes takes its block with it at the next start.
    std::fs::remove_file(assets.join("hard.curve")).unwrap();
    assert!(
        extensions
            .set_enabled(&key, false, &mut backend)
            .community_changed
    );
    assert!(
        extensions.community_sources().is_empty(),
        "a disabled package offers no block"
    );
    extensions.set_enabled(&key, true, &mut backend);
    pump(&mut extensions, &mut backend, "the removed asset", &|e| {
        keys(e).len() == 2
    });
    assert!(keys(&extensions).iter().all(|key| !key.contains("#hard")));
}

/// A computer's message over USB (`tetrad-usb ext`) reaches the extension's
/// `ctx.usb.onMessage`; the handler's return value is the reply. An extension that is not
/// installed gets an error reply. A message near the USB request limit (72 KiB) fits in one
/// runner frame (4 MiB), so its large reply comes back too.
#[test]
fn a_usb_message_is_answered_by_the_extension() {
    use tetrad_dsp_protocol::hid::{HEADER_BYTES, REPORT_BYTES};
    use tetrad_dsp_protocol::responses::{ResponseType, encode};
    use tetrad_dsp_protocol::{CoreId, DspCore};
    use tetrad_usb::{Answer, Op, Reassembler, Reply, Request};

    const PONG: &str = r#"export default { activate(ctx) {
        ctx.usb.onMessage((message) => ({ pong: message.ping }));
    } };"#;
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let root = media.0.join("tetrad/extensions");
    let backend = LocalBackend::threads().with_worker(root, repository_with(PONG, &["usb"]));
    let (mut device, log, _media) = common::boot_extensions(media, backend, MemFiles::default());
    while !device.state().splash_dismissed {
        assert!(device.step().is_none());
    }
    install(&mut device);

    let ask = |device: &mut Device<Sim>, id, extension: &str, ping: serde_json::Value| {
        let op = Op::Extension {
            extension: extension.into(),
            message: json!({ "ping": ping }),
        };
        let reports = tetrad_usb::reports(0, &tetrad_usb::encode(&Request { id, op }, &[]));
        // Fewer reports per step than the response ring's 14 slots, so none is overwritten.
        for batch in reports.chunks(12) {
            let rings = &device.platform().clock.rings;
            for report in batch {
                common::publish(
                    &mut rings.0.borrow_mut(),
                    CoreId::Dsp(DspCore::Core3),
                    &encode(ResponseType::HidReport, 0, report),
                );
            }
            assert!(device.step().is_none());
        }
    };
    let replies = |log: &common::Log| -> Vec<Reply> {
        let mut reassembler = Reassembler::new(1 << 20);
        (log.requests.iter())
            .filter(|sent| sent.kind() == 0x27)
            .map(|sent| &sent.frame[HEADER_BYTES..HEADER_BYTES + REPORT_BYTES])
            .filter(|report| tetrad_usb::is_tetrad(report))
            .filter_map(|report| reassembler.push(report).unwrap())
            .map(|(_, message)| tetrad_usb::decode::<Reply>(&message).unwrap().0)
            .collect()
    };
    ask(&mut device, 1, "test.mapper", json!(7));
    ask(&mut device, 2, "test.missing", json!(7));
    ask(&mut device, 3, "test.mapper", json!("x".repeat(66_000)));
    ask(&mut device, 4, "test.mapper", json!(8));
    pump(&mut device, "four USB replies", |_| {
        replies(&log.borrow()).len() == 4
    });
    let replies = replies(&log.borrow());
    let answer = |id| &replies.iter().find(|reply| reply.id == id).unwrap().answer;
    assert_eq!(
        answer(1),
        &Answer::Extension {
            value: json!({ "pong": 7 })
        }
    );
    assert!(
        matches!(answer(2), Answer::Error { message } if message.contains("test.missing")),
        "{:?}",
        answer(2)
    );
    assert_eq!(
        answer(3),
        &Answer::Extension {
            value: json!({ "pong": "x".repeat(66_000) })
        }
    );
    assert_eq!(
        answer(4),
        &Answer::Extension {
            value: json!({ "pong": 8 })
        }
    );
}

/// The Custom footswitch mode of a data-only package, end to end: the mode follows the stock
/// cycle, a momentary bypass held across a page change comes back on its release, Bank Up
/// leaves the mode, and the layout and the mode come back after a restart.
#[test]
fn a_custom_footswitch_mode_plays_pages_and_survives_a_restart() {
    use tetrad_app::device::Action;
    use tetrad_app::device::custom;
    use tetrad_hal::hal::Event as HalEvent;
    use tetrad_hal::node::Element;
    use tetrad_ui::{
        BlockTarget, CustomCommand, LayoutScope, PageStep, SlotAction, SlotEdit, SystemAction,
    };

    let web = repository_of(
        json!({
            "id": "test.mapper", "name": "Test Mapper", "version": "1.0.0",
            "description": "Maps CC to gain.", "api": 4,
            "footswitchMode": { "name": "Custom", "pages": 2 }
        }),
        None,
    );
    let files = MemFiles::default();
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let (mut device, media) = boot_over(media, web, files.clone());
    install(&mut device);
    pump(&mut device, "the provider", |d| {
        d.state().custom.provider().is_some_and(|p| p.pages == 2)
    });
    let light = GridPosition::new(0, 2).unwrap();
    let edit = |scope, page, slot, edit| CustomCommand::Slot {
        scope,
        page,
        slot,
        edit,
    };
    let all = LayoutScope::Global;
    let bypass = SlotAction::Block {
        position: light,
        target: BlockTarget::Bypass,
    };
    for command in [
        edit(all, 0, 0, SlotEdit::SetAction(bypass.clone())),
        edit(all, 0, 0, SlotEdit::SetMomentary(true)),
        edit(all, 0, 0, SlotEdit::CopyTo { page: 0, slot: 1 }),
        edit(
            all,
            0,
            8,
            SlotEdit::SetAction(SlotAction::System(SystemAction::CustomPage(PageStep::Next))),
        ),
        edit(
            all,
            1,
            0,
            SlotEdit::SetAction(SlotAction::System(SystemAction::Scene(2))),
        ),
    ] {
        device.push(Event::Custom(command));
        assert!(device.step().is_none());
    }
    let custom = |d: &Device<Sim>| d.state().performance.footswitch.custom;
    let bypassed = |d: &Device<Sim>| {
        let current = d.state().current.as_ref().unwrap();
        let site = Site::Slot(Slot {
            row: Row::new(0).unwrap(),
            column: Column::new(2).unwrap(),
        });
        let block = current.deployment.layout().instance(site).unwrap();
        block.bypass.at(current.deployment.scene())
    };
    let scene = |d: &Device<Sim>| {
        d.state()
            .current
            .as_ref()
            .unwrap()
            .deployment
            .scene()
            .index()
    };
    let switch = |d: &mut Device<Sim>, index: u8, value: i32| {
        let element = Element::all().nth(usize::from(index)).unwrap();
        d.push(Event::Hal(HalEvent::Stomp { element, value }));
        for _ in 0..4 {
            assert!(d.step().is_none());
        }
    };

    // Preset → Scene → Stomp → Custom.
    for _ in 0..3 {
        device.push(Event::Action(Action::CycleMode));
        assert!(device.step().is_none());
    }
    assert!(custom(&device));
    let before = bypassed(&device);
    // Two momentary switches of one block, overlapping: the block ends where it started.
    switch(&mut device, 0, 1);
    switch(&mut device, 1, 1);
    switch(&mut device, 0, 0);
    switch(&mut device, 1, 0);
    assert_eq!(bypassed(&device), before);
    // A (hardware 0) held, Down (hardware 4) turns the page, then A's release still ends
    // the momentary bypass of page 1.
    switch(&mut device, 0, 1);
    assert_eq!(bypassed(&device), !before);
    // A refused mode change (CC#47 to a slot the cycle does not have) keeps the mode and the
    // held switch.
    device.push(Event::Action(Action::ModeSlot(7)));
    assert!(device.step().is_none());
    assert!(custom(&device));
    switch(&mut device, 4, 1);
    switch(&mut device, 4, 0);
    assert_eq!(device.state().custom.page(), 1);
    switch(&mut device, 0, 0);
    assert_eq!(bypassed(&device), before);
    // Page 2's A selects scene C.
    switch(&mut device, 0, 1);
    switch(&mut device, 0, 0);
    assert_eq!(scene(&device), 2);

    // Bank Up (hardware 10) goes back to the first stock mode; the cycle comes back to Custom.
    switch(&mut device, 10, 1);
    switch(&mut device, 10, 0);
    assert!(!custom(&device));
    // A switch held while the mode ends releases quietly: no stock action from Custom's press.
    for _ in 0..3 {
        device.push(Event::Action(Action::CycleMode));
        assert!(device.step().is_none());
    }
    switch(&mut device, 4, 1);
    device.push(Event::Action(Action::CycleMode));
    assert!(device.step().is_none());
    assert!(!custom(&device));
    let preset = device.state().performance.footswitch.current_preset;
    switch(&mut device, 4, 0);
    let footswitch = &device.state().performance.footswitch;
    assert_eq!(
        (footswitch.current_preset, footswitch.overlay),
        (preset, None)
    );
    for _ in 0..3 {
        device.push(Event::Action(Action::CycleMode));
        assert!(device.step().is_none());
    }
    assert!(custom(&device));

    // A restart keeps the layout and the mode.
    drop(device);
    let (mut device, _media) = boot_over(media, Web::default(), files);
    pump(&mut device, "the restored mode", custom);
    assert_eq!(device.state().custom.page(), 0);
    let before = bypassed(&device);
    switch(&mut device, 0, 1);
    assert_eq!(bypassed(&device), !before);
    switch(&mut device, 0, 0);
    assert_eq!(bypassed(&device), before);

    // Copy page 2's A to C of page 1: C selects scene C.
    let send = |d: &mut Device<Sim>, command| {
        d.push(Event::Custom(command));
        assert!(d.step().is_none());
    };
    send(
        &mut device,
        edit(all, 1, 0, SlotEdit::CopyTo { page: 0, slot: 2 }),
    );
    switch(&mut device, 2, 1);
    switch(&mut device, 2, 0);
    assert_eq!(scene(&device), 2);

    // THIS PRESET: a copy of the layout, then its own A (scene D) plays instead.
    send(&mut device, CustomCommand::PresetLayout(true));
    send(
        &mut device,
        edit(
            LayoutScope::Preset,
            0,
            0,
            SlotEdit::SetAction(SlotAction::System(SystemAction::Scene(3))),
        ),
    );
    switch(&mut device, 0, 1);
    switch(&mut device, 0, 0);
    assert_eq!(scene(&device), 3);
    assert!(device.state().preset_dirty(), "the preset holds its layout");
    // Back to ALL PRESETS: A is the momentary bypass again.
    send(&mut device, CustomCommand::PresetLayout(false));
    let before = bypassed(&device);
    switch(&mut device, 0, 1);
    assert_eq!(bypassed(&device), !before);
    switch(&mut device, 0, 0);

    // Choose a preset: a slot of the browsed setlist becomes a Load preset action.
    send(
        &mut device,
        edit(all, 0, 9, SlotEdit::SetAction(SlotAction::PresetSlot(0))),
    );
    let view = custom::view(device.state()).unwrap();
    let tap = &view.slots[0][9];
    assert_eq!(
        (tap.action.clone(), tap.label.as_str(), tap.detail.as_str()),
        (Some(SlotAction::LoadPreset), "Light", "Load preset")
    );
}

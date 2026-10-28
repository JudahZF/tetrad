//! A simulated Cortex Control host against the device bridge: the stock handshake, a
//! preset READ, a scene UPDATE and USB unplug, through `Control::drive` with a recording
//! model. The State-backed model has its own unit tests in `device/control/tests.rs`.

use std::io::Read;
use std::time::Duration;

use flate2::read::GzDecoder;
use tetrad_app::device::Effect;
use tetrad_app::device::control::{Control, MODEL_REPO_ARCHIVE_BYTES, reset_marker_frame};
use tetrad_app::device::preset::UndoPath;
use tetrad_control::codecs::{
    CodecLimits, ConnectionMessage, ControlMessage, CortexMessageType, GridMoveElement,
    GridMoveMessage, KeepAliveMessage, MessageAction, ModelRepoMessage, RawMessageBytes,
    RecallPresetMessage, SceneMessage,
};
use tetrad_control::device::{DeviceModel, DeviceResult};
use tetrad_control::{Envelope, Limits, Reassembler};
use tetrad_dsp_protocol::fragment::Fragment;

mod common;

#[derive(Default)]
struct Recorder {
    scene: u32,
    connection: Vec<bool>,
}

/// Larger than the 10 KiB deflate threshold, so RecallPreset goes out compressed.
fn working_preset() -> Vec<u8> {
    let name = "x".repeat(12_000);
    let mut bytes = vec![0x0a];
    bytes.extend([0xe0, 0x5d]); // varint 12 000
    bytes.extend(name.as_bytes());
    bytes
}

impl DeviceModel for Recorder {
    fn connection_changed(&mut self, connected: bool) -> DeviceResult<()> {
        self.connection.push(connected);
        Ok(())
    }

    fn model_repo(&mut self) -> DeviceResult<Vec<u8>> {
        Ok(b"repo".to_vec())
    }

    fn current_preset(&mut self) -> DeviceResult<RecallPresetMessage> {
        Ok(RecallPresetMessage {
            preset: Some(RawMessageBytes {
                bytes: working_preset(),
            }),
            ..Default::default()
        })
    }

    fn scene(&mut self) -> DeviceResult<u32> {
        Ok(self.scene)
    }

    fn select_scene(&mut self, scene: u32) -> DeviceResult<()> {
        self.scene = scene;
        Ok(())
    }
}

fn host(message_type: u32, payload: Vec<u8>) -> Vec<[u8; 128]> {
    Envelope {
        message_type,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload,
    }
    .reports(Limits::default())
    .unwrap()
}

/// Encoded with the device link's payload bound, which a whole preset needs.
fn send(message: impl Into<ControlMessage>) -> Vec<[u8; 128]> {
    let message = message.into();
    host(
        message.message_type() as u32,
        message
            .encode(tetrad_control::link::LinkLimits::default().payload)
            .unwrap(),
    )
}

/// Feeds host reports, returns the decoded device messages (inflated when compressed)
/// with their trailer compression flag, and the other effects.
fn exchange(
    control: &mut Control,
    model: &mut Recorder,
    reports: &[[u8; 128]],
    now: Duration,
) -> (Vec<(ControlMessage, bool)>, Vec<Effect>) {
    let mut effects = Vec::new();
    for report in reports {
        effects.extend(
            control
                .drive(model, |link, model, stages| {
                    link.receive_report(report, now, model, stages)
                })
                .unwrap(),
        );
    }
    replies(effects)
}

fn replies(effects: Vec<Effect>) -> (Vec<(ControlMessage, bool)>, Vec<Effect>) {
    let mut reassembler = Reassembler::new(Limits::default());
    let mut messages = Vec::new();
    let mut other = Vec::new();
    for effect in effects {
        let Effect::ControlReport(report) = effect else {
            other.push(effect);
            continue;
        };
        let Some(mut envelope) = reassembler.push(&report[..]).unwrap() else {
            continue;
        };
        let compressed = envelope.decompress != 0;
        if compressed {
            let mut plain = Vec::new();
            GzDecoder::new(&envelope.payload[..])
                .read_to_end(&mut plain)
                .unwrap();
            envelope.payload = plain;
            envelope.decompress = 0;
        }
        let limits = tetrad_control::link::LinkLimits::default().payload;
        // The resync after a Download suspension also pushes types this codec does not
        // decode (Scene 13); they are left out.
        let decoded = match ControlMessage::decode(&envelope, limits) {
            Err(tetrad_control::codecs::CodecError::UnsupportedMessageType(_)) => continue,
            decoded => decoded.unwrap(),
        };
        messages.push((decoded.message, compressed));
    }
    (messages, other)
}

fn update() -> Option<tetrad_control::codecs::EnumValue<MessageAction>> {
    Some(MessageAction::Update.into())
}

#[test]
fn host_session_handshake_preset_read_scene_update_and_unplug() {
    let mut control = Control::new();
    let mut model = Recorder::default();
    let at = Duration::from_millis;

    // Handshake (`0x6e24a8`): keep-alive, then the ModelRepo reply sets "version", then
    // Connection. ModelRepo goes on the priority path without the request ID.
    let keep_alive = send(KeepAliveMessage {
        action: update(),
        is_online: Some(true),
        ..Default::default()
    });
    let (sent, _) = exchange(&mut control, &mut model, &keep_alive, at(0));
    assert!(sent.is_empty());
    let read_repo = send(ModelRepoMessage {
        action: Some(MessageAction::Read.into()),
        request_id: Some(5),
        ..Default::default()
    });
    let (sent, _) = exchange(&mut control, &mut model, &read_repo, at(10));
    let [(ControlMessage::ModelRepo(repo), false)] = &sent[..] else {
        panic!("{sent:?}");
    };
    assert_eq!(repo.model_repo_payload.as_deref(), Some(&b"repo"[..]));
    assert_eq!(repo.request_id, None);
    assert!(!control.connected());
    let connection = send(ConnectionMessage {
        connected: Some(true),
        ..Default::default()
    });
    exchange(&mut control, &mut model, &connection, at(20));
    assert!(control.connected());
    assert_eq!(model.connection, [true]);

    // READ of the working preset: compressible type above 10 KiB → gzip, ID echoed.
    let read_preset = send(RecallPresetMessage {
        action: Some(MessageAction::Read.into()),
        request_id: Some(7),
        ..Default::default()
    });
    let (sent, _) = exchange(&mut control, &mut model, &read_preset, at(30));
    let [(ControlMessage::RecallPreset(preset), true)] = &sent[..] else {
        panic!("{sent:?}");
    };
    assert_eq!(preset.request_id, Some(7));
    assert_eq!(preset.action, update());
    assert_eq!(preset.preset.as_ref().unwrap().bytes, working_preset());

    // Scene UPDATE selects without a reply; READ answers with the new scene.
    let select = SceneMessage {
        action: update(),
        request_id: Some(8),
        selected_scene: Some(3),
    };
    let reports = host(
        CortexMessageType::Scene as u32,
        select.encode(CodecLimits::default()).unwrap(),
    );
    let (sent, _) = exchange(&mut control, &mut model, &reports, at(40));
    assert!(sent.is_empty());
    assert_eq!(model.scene, 3);

    // Unplug (`0x6e2b8c`): teardown flushes transmit, so the reset marker precedes the
    // Connection(false) report on the priority path.
    let effects = control
        .drive(&mut model, |link, model, stages| {
            link.usb_unplugged(at(50), model, stages)
        })
        .unwrap();
    assert!(control.reset_marker_pending());
    let (sent, _) = replies(effects);
    let [(ControlMessage::Connection(closed), false)] = &sent[..] else {
        panic!("{sent:?}");
    };
    assert_eq!(closed.connected, Some(false));
    assert!(!control.connected());
    assert_eq!(model.connection, [true, false]);
}

#[test]
fn reset_marker_is_one_core4_hid_fragment_with_byte_2_set() {
    let frame = reset_marker_frame();
    assert_eq!(&frame[..12], &[0x27, 4, 0xff, 0, 0, 0, 0, 0, 0x90, 0, 0, 0]);
    let fragment = Fragment::decode(&frame, Default::default()).unwrap();
    assert_eq!(fragment.payload().len(), 128);
    assert_eq!(fragment.to_bytes(), frame);
}

#[test]
fn model_repo_payload_is_a_gzip_tar_with_scrambled_blob_names() {
    let mut control = Control::new();
    let xml = br#"<Models><Model id="1" blob="light"/><Model id="2" blob=""/></Models>"#;
    control.set_model_repo(xml).unwrap();
    let archive = control.model_repo_payload().unwrap();
    assert!(archive.len() <= MODEL_REPO_ARCHIVE_BYTES);
    let mut tar = Vec::new();
    GzDecoder::new(archive).read_to_end(&mut tar).unwrap();
    assert_eq!(&tar[..14], b"ModelRepo.xml\0");
    assert_eq!(&tar[100..108], b"0000644\0");
    assert_eq!(&tar[156..157], b"0");
    assert_eq!(&tar[257..263], b"ustar\0");
    let size = usize::from_str_radix(std::str::from_utf8(&tar[124..135]).unwrap(), 8).unwrap();
    let body = &tar[512..512 + size];
    assert_eq!(body.len(), xml.len());
    let blob = body.windows(6).position(|w| w == b"blob=\"").unwrap() + 6;
    assert!(body[blob..blob + 5].iter().all(u8::is_ascii_lowercase));
    assert_ne!(&body[blob..blob + 5], b"light");
    assert!(body.ends_with(br#"blob=""/></Models>"#));
    assert_eq!(tar.len() % 512, 0);
}

/// `0x6e0444`: unsolicited message 60 passes the codec only after the Control handshake.
#[test]
fn updater_notify_is_connected_only_and_round_trips_message_60() {
    use tetrad_app::device::State;
    use tetrad_control::codecs::{UpdaterMessage, UpdaterState};
    use tetrad_control::link::Notification;
    use tetrad_hal::platform::{Platform, Product};
    use tetrad_settings::device::{DeviceSettings, Hardware};
    use tetrad_settings::{SettingsFileId, SettingsStorage};
    struct NoFiles;
    impl SettingsStorage for NoFiles {
        fn read(&self, _: SettingsFileId) -> std::io::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn write(&mut self, _: SettingsFileId, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn remove(&mut self, _: SettingsFileId) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (settings, _) = DeviceSettings::load(&NoFiles, Hardware::QuadCortex).unwrap();
    let mut state = State::new(Product::QuadCortex, Platform::QcOriginal, settings);
    let mut control = Control::new();
    control.set_model_repo(b"<Models/>").unwrap();
    let message = UpdaterMessage {
        action: update(),
        request_id: Some(0),
        state: Some(UpdaterState::DownloadingData.into()),
        ..Default::default()
    };
    let notification = || Notification::Update(ControlMessage::Updater(message.clone()));
    assert!(
        control
            .notify(notification(), &mut state, Duration::ZERO)
            .unwrap()
            .is_empty()
    );
    for (at, reports) in [
        (
            0,
            send(KeepAliveMessage {
                action: update(),
                is_online: Some(true),
                ..Default::default()
            }),
        ),
        (
            10,
            send(ModelRepoMessage {
                action: Some(MessageAction::Read.into()),
                ..Default::default()
            }),
        ),
        (
            20,
            send(ConnectionMessage {
                connected: Some(true),
                ..Default::default()
            }),
        ),
    ] {
        for report in reports {
            control
                .on_report(&report, &mut state, Duration::from_millis(at))
                .unwrap();
        }
    }
    assert!(state.control_connected);
    let effects = control
        .notify(notification(), &mut state, Duration::from_millis(30))
        .unwrap();
    assert_eq!(
        replies(effects).0,
        [(ControlMessage::Updater(message), false)]
    );
    control
        .usb_unplugged(&mut state, Duration::from_millis(40))
        .unwrap();
    assert!(
        control
            .notify(
                Notification::Update(ControlMessage::Updater(UpdaterMessage::default())),
                &mut state,
                Duration::from_millis(50)
            )
            .unwrap()
            .is_empty()
    );
}

/// IOSettings (3) against the State-backed model, after the handshake. UPDATE `0x26acfc`
/// applies only the first input port's first field (IN 2 level here; IN 1 lift and the
/// output are ignored), queues the gain, restarts the 250 ms save and echoes one port
/// (`0x26dafc`). READ with an empty request is the full `0x17be68` dump.
#[test]
fn io_settings_update_applies_one_field_and_echoes_one_port() {
    use tetrad_app::device::jacks::InputChange;
    use tetrad_app::device::{State, Timer};
    use tetrad_control::codecs::{
        IOSettingsMessage, InputPortSettings, OutputPortSettings, PortSettings,
    };
    use tetrad_hal::platform::{Platform, Product};
    use tetrad_settings::device::{DeviceSettings, Hardware};
    use tetrad_settings::io::ConnectorId;
    use tetrad_settings::{SettingsFileId, SettingsStorage};
    struct NoFiles;
    impl SettingsStorage for NoFiles {
        fn read(&self, _: SettingsFileId) -> std::io::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn write(&mut self, _: SettingsFileId, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn remove(&mut self, _: SettingsFileId) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (settings, _) = DeviceSettings::load(&NoFiles, Hardware::QuadCortex).unwrap();
    let mut state = State::new(Product::QuadCortex, Platform::QcOriginal, settings);
    let mut control = Control::new();
    control.set_model_repo(b"<Models/>").unwrap();
    let handshake = [
        send(KeepAliveMessage {
            action: update(),
            is_online: Some(true),
            ..Default::default()
        }),
        send(ModelRepoMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        }),
        send(ConnectionMessage {
            connected: Some(true),
            ..Default::default()
        }),
    ];
    for (at, reports) in handshake.iter().enumerate() {
        for report in reports {
            control
                .on_report(report, &mut state, Duration::from_millis(at as u64 * 10))
                .unwrap();
        }
    }
    assert!(control.connected());

    let input = |port, level, ground_lift| InputPortSettings {
        input_port_id: Some(port),
        level,
        ground_lift,
        ..Default::default()
    };
    let change = IOSettingsMessage {
        action: update(),
        request_id: Some(3),
        settings: Some(PortSettings {
            in_port: vec![input(2, Some(0.5), None), input(1, None, Some(1.0))],
            out_port: vec![OutputPortSettings {
                output_port_id: Some(4),
                mute: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut effects = Vec::new();
    for report in send(change) {
        effects.extend(control.on_report(&report, &mut state, at(30)).unwrap());
    }
    assert_eq!(state.io.value(ConnectorId::In2Level), 0.5);
    assert_eq!(state.io.value(ConnectorId::In1GroundLift), 0.0);
    assert_eq!(state.io.value(ConnectorId::Out1Mute), 0.0);
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::ScheduleInput(InputChange::Gain {
            endpoint: tetrad_hal::analog::GainEndpoint::Input2,
            ..
        })
    )));
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::StartTimer {
            timer: Timer::ConnectorsSave,
            ..
        }
    )));
    let echoes: Vec<_> = effects
        .into_iter()
        .filter_map(|e| match e {
            Effect::ControlNotify(message) => Some(*message),
            _ => None,
        })
        .collect();
    let [echo] = &echoes[..] else {
        panic!("{echoes:?}");
    };
    let sent = control
        .notify(
            tetrad_control::link::Notification::Update(echo.clone()),
            &mut state,
            at(40),
        )
        .unwrap();
    let (sent, _) = replies(sent);
    let [(ControlMessage::IOSettings(echoed), false)] = &sent[..] else {
        panic!("{sent:?}");
    };
    let ports = echoed.settings.as_ref().unwrap();
    assert_eq!(ports.in_port, [input(2, Some(0.5), None)]);
    assert!(ports.out_port.is_empty() && ports.hp_port.is_none());

    let read = IOSettingsMessage {
        action: Some(MessageAction::Read.into()),
        request_id: Some(4),
        ..Default::default()
    };
    let mut effects = Vec::new();
    for report in send(read) {
        effects.extend(control.on_report(&report, &mut state, at(50)).unwrap());
    }
    let (sent, _) = replies(effects);
    let [(ControlMessage::IOSettings(dump), false)] = &sent[..] else {
        panic!("{sent:?}");
    };
    let ports = dump.settings.as_ref().unwrap();
    let ids = |ids: Vec<Option<u32>>| ids.into_iter().flatten().collect::<Vec<_>>();
    assert_eq!(
        ids(ports.in_port.iter().map(|p| p.input_port_id).collect()),
        [1, 2, 4, 5]
    );
    assert_eq!(
        ids(ports.out_port.iter().map(|p| p.output_port_id).collect()),
        [4, 5, 1, 6, 7, 2, 8, 9]
    );
    assert_eq!(ports.in_port[1].level, Some(0.5));
    assert_eq!(ports.in_port[0].input_type, Some(0.0));
    assert_eq!(dump.xlr1_2_linked, Some(false));
}

fn at(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

// --- w5 Control gaps: undo/redo, scene copy, Gig View, capture echoes -------------------

mod loaded {
    use std::time::Duration;

    use tetrad_app::device::bringup::{RunOptions, Stage};
    use tetrad_app::device::control::Control;
    use tetrad_app::device::preset::{SpecsCatalog, prepare};
    use tetrad_app::device::{Current, Effect, State};
    use tetrad_catalog::{Catalog, Specs};
    use tetrad_control::codecs::{
        ConnectionMessage, ControlMessage, KeepAliveMessage, ModelRepoMessage,
    };
    use tetrad_hal::platform::{Platform, Product};
    use tetrad_preset::Preset;
    use tetrad_settings::device::{DeviceSettings, Hardware};
    use tetrad_settings::{SettingsFileId, SettingsStorage};

    use super::{replies, send, update};

    /// Synthetic catalog (no stock data), as in `device/control/tests.rs`.
    const MODELS: &str = "<Models>\
<Category id='0' name='Drive'><Model id='1' name='Light' blob='light'>\
<Parameter name='GAIN' type='float' min='0' max='10' defaultValue='5'/></Model>\
<Model id='2' name='Linked' blob='linked'>\
<Parameter name='TIME' type='float' min='0' max='1' defaultValue='0.5' linkedSceneMode='1'/>\
<Parameter name='SYNC' type='switch' min='0' max='1' defaultValue='0'/></Model></Category>\
<Category id='26' name='Utility'><Model id='26001' name='Switch' blob='switch'>\
<Parameter name='LEVEL' type='float' min='0' max='1' defaultValue='0.5'/>\
<Parameter name='MOMENTARY' type='switch' min='0' max='1' defaultValue='0'/></Model></Category>\
<Category id='10' name='Splitter' hidden='true'><Model id='10004' name='Splitter'>\
<Parameter name='TYPE' type='switch' min='0' max='2' defaultValue='0'/></Model>\
<Model id='10003' name='LR Crossover' blob='lr' hidden='true'>\
<Parameter name='FREQ' type='float' min='20' max='20000' defaultValue='20'/></Model></Category>\
<Category id='11' name='Mixer' hidden='true'><Model id='11000' name='Mixer'>\
<Parameter name='LEVEL A' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/>\
</Model></Category>\
<Category id='23' name='Lane Output' hidden='true'><Model id='23000' name='Lane Output'>\
<Parameter name='VOLUME' type='float' min='0' max='1' defaultValue='0.76923'/>\
<Parameter name='PAN' type='float' min='0' max='1' defaultValue='0.5'/>\
<Parameter name='MUTE' type='switch' min='0' max='1' defaultValue='0'/>\
<Parameter name='SOLO' type='switch' min='0' max='1' defaultValue='0'/></Model></Category>\
<Category id='25' name='Tempo' hidden='true'><Model id='25000' name='Tempo'>\
<Parameter name='TEMPO' type='float' min='40' max='240' defaultValue='120'/>\
<Parameter name='SCOPE' type='switch' min='0' max='1' defaultValue='0'/>\
<Parameter name='LED' type='switch' min='0' max='1' defaultValue='1'/></Model></Category>\
<Category id='28' name='Input Gate' hidden='true'><Model id='28000' name='Input Gate'>\
<Parameter name='THRESHOLD' type='float' min='-96' max='0' defaultValue='-60'/></Model></Category>\
</Models>";

    const REPORT: &str = "<ModuleReport>\
<Kernel core='0' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='1' cpu='2' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='2' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='3' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Module id='1' name='Light' blob_name='light' category='0' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='2' name='Linked' blob_name='linked' category='0' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='5'/></Module>\
<Module id='26001' name='Switch' blob_name='switch' category='26' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='1'/></Module>\
<Module id='10003' name='LR Crossover' blob_name='lr' category='10' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='1'/></Module>\
</ModuleReport>";

    pub struct NoFiles;

    impl SettingsStorage for NoFiles {
        fn read(&self, _: SettingsFileId) -> std::io::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn write(&mut self, _: SettingsFileId, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn remove(&mut self, _: SettingsFileId) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A deployed default preset and a connected Control link.
    pub fn session() -> (State, Control) {
        session_with(MODELS, REPORT)
    }

    /// [`session`] over the test catalog plus `models` (inside `<Models>`) and their
    /// `modules` (inside `<ModuleReport>`).
    pub fn session_with_extra(models: &str, modules: &str) -> (State, Control) {
        let models = MODELS.replace("</Models>", &format!("{models}</Models>"));
        let report = REPORT.replace("</ModuleReport>", &format!("{modules}</ModuleReport>"));
        session_with(&models, &report)
    }

    /// [`session_with_extra`] with the default preset changed by `edit` before it deploys.
    pub fn session_with_preset(
        models: &str,
        modules: &str,
        edit: impl FnOnce(&mut Preset, &SpecsCatalog<'_>),
    ) -> (State, Control) {
        let models = MODELS.replace("</Models>", &format!("{models}</Models>"));
        let report = REPORT.replace("</ModuleReport>", &format!("{modules}</ModuleReport>"));
        session_edited(&models, &report, edit)
    }

    fn session_with(models: &str, report: &str) -> (State, Control) {
        session_edited(models, report, |_, _| {})
    }

    fn session_edited(
        models: &str,
        report: &str,
        edit: impl FnOnce(&mut Preset, &SpecsCatalog<'_>),
    ) -> (State, Control) {
        let specs = Specs::resolve(&Catalog::parse(models, report).unwrap()).unwrap();
        let mut preset = Preset::new_default("4.1.0", &SpecsCatalog(&specs)).unwrap();
        edit(&mut preset, &SpecsCatalog(&specs));
        let prepared = prepare(preset, &specs, false, None, Default::default()).unwrap();
        let (settings, _) = DeviceSettings::load(&NoFiles, Hardware::QuadCortex).unwrap();
        let mut state = State::new(Product::QuadCortex, Platform::QcOriginal, settings);
        state.specs = Some(specs);
        *state.current.get_mut() = Some(Current {
            path: None,
            preset: prepared.preset,
            migration: prepared.migration,
            deployment: prepared.deployment,
        });
        let control = connect(&mut state);
        (state, control)
    }

    /// An ATMA (QC mini) with nothing loaded and a connected Control link.
    pub fn atma() -> (State, Control) {
        let (settings, _) = DeviceSettings::load(&NoFiles, Hardware::Atma).unwrap();
        let mut state = State::new(Product::Atma, Platform::Atma, settings);
        let control = connect(&mut state);
        (state, control)
    }

    /// The device step's change pushes ([`Control::sync`]), decoded.
    pub fn sync(control: &mut Control, state: &mut State, now: Duration) -> Vec<ControlMessage> {
        let effects = control.sync(state, false, now).unwrap();
        replies(effects)
            .0
            .into_iter()
            .map(|(message, _)| message)
            .collect()
    }

    pub fn connect(state: &mut State) -> Control {
        let mut control = Control::new();
        control.set_model_repo(b"<Models/>").unwrap();
        handshake(&mut control, state, Duration::ZERO);
        control
    }

    /// The host connect sequence: KeepAlive, ModelRepo READ, Connection.
    pub fn handshake(control: &mut Control, state: &mut State, now: Duration) {
        let handshake = [
            send(KeepAliveMessage {
                action: update(),
                is_online: Some(true),
                ..Default::default()
            }),
            send(ModelRepoMessage {
                action: Some(tetrad_control::codecs::MessageAction::Read.into()),
                ..Default::default()
            }),
            send(ConnectionMessage {
                connected: Some(true),
                ..Default::default()
            }),
        ];
        for reports in handshake {
            for report in reports {
                control.on_report(&report, state, now).unwrap();
            }
        }
        assert!(control.connected());
    }

    /// One host message: the device replies and the other effects, without the request's
    /// end marker ([`done`] runs it).
    pub fn host(
        control: &mut Control,
        state: &mut State,
        message: impl Into<ControlMessage>,
        now: Duration,
    ) -> (Vec<ControlMessage>, Vec<Effect>) {
        let mut effects = Vec::new();
        for report in send(message) {
            effects.extend(control.on_report(&report, state, now).unwrap());
        }
        // A READ reply built over device passes (A3): every step, as the device runs them.
        while control.spreading() {
            effects.extend(control.spread_step(state, now).unwrap());
        }
        effects.retain(|effect| *effect != Effect::ControlRequestDone);
        let (sent, other) = replies(effects);
        (
            sent.into_iter().map(|(message, _)| message).collect(),
            other,
        )
    }

    /// `Effect::ControlRequestDone`: the request's pushes and deferred reply, decoded with
    /// their request IDs.
    pub fn done(
        control: &mut Control,
        state: &mut State,
        now: Duration,
    ) -> Vec<(ControlMessage, Option<u64>)> {
        let effects = control.request_done(state, false, now).unwrap();
        replies(effects)
            .0
            .into_iter()
            .map(|(message, _)| {
                let id = message.request_id();
                (message, id)
            })
            .collect()
    }

    /// Commits the layout changes and Control recalls as a successful device deploy does
    /// (with their undo frames) and reduces queued actions after them, in order; returns the
    /// layout change count.
    pub fn commit(state: &mut State, effects: Vec<Effect>) -> usize {
        let mut count = 0;
        for effect in effects {
            match effect {
                Effect::DeployLayout(mut change) => {
                    let undo = change.take_undo();
                    change.commit(state.current.get_mut().as_mut().unwrap());
                    if let Some(undo) = undo {
                        state.history.commit(undo);
                    }
                    count += 1;
                }
                Effect::ControlRecall { prepared, undo } => {
                    let current = state.current.get_mut().as_mut().unwrap();
                    state.history.commit(undo);
                    current.preset = prepared.preset;
                    current.migration = prepared.migration;
                    current.deployment = prepared.deployment;
                }
                Effect::Action(action) => {
                    tetrad_app::device::actions::reduce(action, state).unwrap();
                }
                _ => {}
            }
        }
        count
    }

    /// Bring-up options at stage full; the log folder is never opened here.
    pub fn bringup(persist: bool) -> RunOptions {
        let mut options = RunOptions::bringup(Stage::Full, Duration::from_secs(60), "logs".into());
        options.persist = persist;
        options
    }

    /// The Control messages queued as `Effect::ControlNotify`.
    pub fn notified(effects: &[Effect]) -> Vec<ControlMessage> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::ControlNotify(message) => Some((**message).clone()),
                _ => None,
            })
            .collect()
    }
}

/// UndoRedo (21) UPDATE `0x1ba218` steps the controller undo (`0x649330`) and redo
/// (`0x649b7c`). A Control scene label (`0x1b4534` → `0x65d260`) records no frame; the
/// restore pushes the frame lists with the restored frame still on its stack (`0x649120`),
/// RecallPreset (reason Undo), Scene and SetlistPosition to Control.
#[test]
fn undo_redo_update_restores_the_frame_and_pushes_the_preset() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::reduce;
    use tetrad_control::codecs::{RecallPresetReason, SceneLabelMessage, UndoRedoMessage};
    use tetrad_control::link::Notification;
    let (mut state, mut control) = loaded::session();
    let label = |text: &str| SceneLabelMessage {
        action: update(),
        index: Some(1),
        label: Some(text.into()),
        ..Default::default()
    };
    let labels = |state: &tetrad_app::device::State| {
        state
            .current
            .as_ref()
            .unwrap()
            .preset
            .scene_labels
            .get(1)
            .cloned()
    };
    loaded::host(&mut control, &mut state, label("Intro"), at(10));
    assert!(!state.history.can_undo());

    reduce(Action::RecordUndo(UndoPath::Direct), &mut state).unwrap();
    loaded::host(&mut control, &mut state, label("Verse"), at(20));
    let undo = UndoRedoMessage {
        action: update(),
        undo: Some(true),
        ..Default::default()
    };
    let (_, effects) = loaded::host(&mut control, &mut state, undo, at(30));
    assert_eq!(labels(&state).as_deref(), Some("Intro"));
    assert!(state.history.can_redo());
    let pushed: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::ControlNotify(_) | Effect::ControlNotification(_)))
        .collect();
    let [
        Effect::ControlNotification(lists),
        Effect::ControlNotify(recall),
        Effect::ControlNotification(scene),
        Effect::ControlNotify(position),
    ] = &pushed[..]
    else {
        panic!("{pushed:?}");
    };
    let intermediate = tetrad_control::link::UndoFrames {
        undo: vec![0],
        redo: vec![0],
    };
    assert_eq!(**lists, Notification::UndoFrames(intermediate));
    let ControlMessage::RecallPreset(recall) = &**recall else {
        panic!("{recall:?}");
    };
    assert!(matches!(**position, ControlMessage::SetlistPosition(_)));
    assert_eq!(**scene, Notification::Scene(0));
    assert_eq!(recall.reason, Some(RecallPresetReason::Undo.into()));
    let current = state.current.as_ref().unwrap();
    assert_eq!(
        recall.preset.as_ref().unwrap().bytes,
        current.preset.to_bytes()
    );

    let redo = UndoRedoMessage {
        action: update(),
        redo: Some(true),
        ..Default::default()
    };
    loaded::host(&mut control, &mut state, redo, at(40));
    assert_eq!(labels(&state).as_deref(), Some("Verse"));
}

/// SceneCopy (22) UPDATE `0x1b048c` → `0x51a2a8`: the target takes the source scene's
/// label, the destination is recalled (`0x5899d8`) and the device echoes the copy.
#[test]
fn scene_copy_update_copies_into_the_target_and_recalls_it() {
    use tetrad_app::device::Action;
    use tetrad_control::codecs::{SceneCopyMessage, SceneLabelMessage};
    use tetrad_engine::SceneIndex;
    let (mut state, mut control) = loaded::session();
    let label = SceneLabelMessage {
        action: update(),
        index: Some(0),
        label: Some("Intro".into()),
        ..Default::default()
    };
    loaded::host(&mut control, &mut state, label, at(10));
    let copy = SceneCopyMessage {
        action: update(),
        from_index: Some(0),
        to_index: Some(2),
        is_swap: Some(false),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, copy, at(20));
    let labels = &state.current.as_ref().unwrap().preset.scene_labels;
    assert_eq!(labels.get(2).map(String::as_str), Some("Intro"));
    let [ControlMessage::SceneCopy(echo)] = &sent[..] else {
        panic!("{sent:?}");
    };
    assert_eq!((echo.from_index, echo.to_index), (Some(0), Some(2)));
    let target = SceneIndex::new(2).unwrap();
    assert!(effects.contains(&Effect::Action(Action::SelectScene(target))));
    assert!(!state.history.can_undo());
}

/// ShowGigView (24) `0x1b8244`: UPDATE shows the Gig View; READ reports whether it is
/// visible.
#[test]
fn show_gig_view_update_shows_it_and_read_reports_it() {
    use tetrad_app::device::ui::UiInput;
    use tetrad_control::codecs::ShowGigViewMessage;
    use tetrad_ui::Screen;
    let (mut state, mut control) = loaded::session();
    let show = ShowGigViewMessage {
        action: update(),
        show: Some(true),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, show, at(10));
    assert!(sent.is_empty());
    assert!(effects.contains(&Effect::UiInput(UiInput::Show(Screen::Gig))));
    state.screen = Screen::Gig;
    let read = ShowGigViewMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read, at(20));
    let [ControlMessage::ShowGigView(shown)] = &sent[..] else {
        panic!("{sent:?}");
    };
    assert_eq!(shown.show, Some(true));
}

/// A capture lock change sends message 42 twice: `global_gate` only (`0x582db8`), then
/// `global_eq` only (`0x582d08`), each `lock || inhibited`. READ `0x15ac7c` follows it.
#[test]
fn capture_lock_sends_message_42_pair_and_read_follows_it() {
    use tetrad_control::codecs::CompilerInhibitedModulesMessage;
    let (mut state, mut control) = loaded::session();
    use tetrad_app::device::State;
    use tetrad_app::device::control::Control;
    fn pair(
        control: &mut Control,
        state: &State,
        locked: bool,
    ) -> Vec<(Option<bool>, Option<bool>)> {
        let effects = control.capture_lock_changed(locked, state);
        loaded::notified(&effects)
            .into_iter()
            .map(|message| match message {
                ControlMessage::CompilerInhibitedModules(m) => (m.global_gate, m.global_eq),
                other => panic!("{other:?}"),
            })
            .collect()
    }
    fn read(control: &mut Control, state: &mut State) -> (Option<bool>, Option<bool>) {
        let request = CompilerInhibitedModulesMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        };
        let (sent, _) = loaded::host(control, state, request, at(20));
        let [ControlMessage::CompilerInhibitedModules(m)] = &sent[..] else {
            panic!("{sent:?}");
        };
        (m.global_gate, m.global_eq)
    }
    assert_eq!(
        pair(&mut control, &state, true),
        [(Some(true), None), (None, Some(true))]
    );
    assert_eq!(read(&mut control, &mut state), (Some(true), Some(true)));
    assert_eq!(
        pair(&mut control, &state, false),
        [(Some(false), None), (None, Some(false))]
    );
    assert_eq!(read(&mut control, &mut state), (Some(false), Some(false)));
}

/// Lane edit echo `0x25ea1c`: a Grid UPDATE whose preset holds one chain for the row with
/// one lane output control parameter and its current-scene float.
#[test]
fn lane_echo_is_one_chain_with_one_output_control_parameter() {
    use tetrad_app::device::control::lane_echo;
    use tetrad_preset::{ParamValue, Preset};
    let row = tetrad_engine::Row::new(2).unwrap();
    let ControlMessage::Grid(grid) = lane_echo(row, 3, 1.0) else {
        panic!("not a Grid message");
    };
    assert_eq!(grid.action, update());
    let bytes = &grid.preset.as_ref().unwrap().bytes;
    let preset = Preset::decode(bytes, Default::default()).unwrap();
    let [chain] = &preset.chains[..] else {
        panic!("{:?}", preset.chains);
    };
    assert_eq!(chain.row, Some(2));
    let [control] = &chain.output_control[..] else {
        panic!("{chain:?}");
    };
    let [param] = &control.params[..] else {
        panic!("{control:?}");
    };
    assert_eq!(param.index, Some(3));
    assert_eq!(param.values.len(), 1);
    assert_eq!(param.values[0].value, Some(ParamValue::Float(1.0)));
}

// --- w5 Control gaps 2: frames, dirty flag, Mode, MasterVolume ------------------------

/// UndoRedo frames: READ `0x1ba218` and the frames-changed push `0x1be1a8` list one frame
/// type (0, `UndoRedoPresetFrame`) per frame. The push goes out once per change.
#[test]
fn undo_redo_frames_push_once_per_change_and_read_lists_them() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::reduce;
    use tetrad_control::codecs::UndoRedoMessage;
    let (mut state, mut control) = loaded::session();
    let frames = |message: &ControlMessage| match message {
        ControlMessage::UndoRedo(m) => (m.undo_frames.clone(), m.redo_frames.clone()),
        other => panic!("{other:?}"),
    };
    // The first send is forced (ctor flag `+0xd`).
    let sent = loaded::sync(&mut control, &mut state, at(5));
    assert_eq!(
        sent.iter().map(frames).collect::<Vec<_>>(),
        [(vec![], vec![])]
    );
    reduce(Action::RecordUndo(UndoPath::View), &mut state).unwrap();
    let sent = loaded::sync(&mut control, &mut state, at(10));
    assert_eq!(
        sent.iter().map(frames).collect::<Vec<_>>(),
        [(vec![0], vec![])]
    );
    assert!(loaded::sync(&mut control, &mut state, at(15)).is_empty());

    let read = UndoRedoMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read, at(20));
    assert_eq!(
        sent.iter().map(frames).collect::<Vec<_>>(),
        [(vec![0], vec![])]
    );

    let undo = UndoRedoMessage {
        action: update(),
        undo: Some(true),
        ..Default::default()
    };
    loaded::host(&mut control, &mut state, undo, at(30));
    let sent = loaded::sync(&mut control, &mut state, at(35));
    assert_eq!(
        sent.iter().map(frames).collect::<Vec<_>>(),
        [(vec![], vec![0])]
    );
}

/// PresetDirty (34): the live preset against the loaded or saved one (`+0x28`, `0x5ad250`),
/// tested at most every 100 ms (`0x5b71f8`). READ `0x1a5dc0` reports the cached flag; the
/// push `0x1a6dfc` goes out only when it changes.
#[test]
fn preset_dirty_follows_the_clean_preset_and_pushes_on_change() {
    use tetrad_app::device::preset::Snapshot;
    use tetrad_control::codecs::{PresetDirtyMessage, SceneLabelMessage};
    let (mut state, mut control) = loaded::session();
    *state.clean.get_mut() = state.current.as_ref().map(Snapshot::of);
    let label = |text: &str| SceneLabelMessage {
        action: update(),
        index: Some(1),
        label: Some(text.into()),
        ..Default::default()
    };
    let dirty = |sent: Vec<ControlMessage>| -> Vec<Option<bool>> {
        sent.into_iter()
            .filter_map(|message| match message {
                ControlMessage::PresetDirty(m) => Some(m.is_dirty),
                _ => None,
            })
            .collect()
    };
    assert!(dirty(loaded::sync(&mut control, &mut state, at(5))).is_empty());
    let before = state.current.as_ref().unwrap().preset.scene_labels.clone();
    loaded::host(&mut control, &mut state, label("Verse"), at(10));
    let read = PresetDirtyMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read.clone(), at(20));
    assert_eq!(dirty(sent), [Some(false)], "the cached flag");
    assert!(dirty(loaded::sync(&mut control, &mut state, at(104))).is_empty());
    assert_eq!(
        dirty(loaded::sync(&mut control, &mut state, at(105))),
        [Some(true)]
    );
    assert!(dirty(loaded::sync(&mut control, &mut state, at(210))).is_empty());
    let (sent, _) = loaded::host(&mut control, &mut state, read, at(220));
    assert_eq!(dirty(sent), [Some(true)]);

    state
        .current
        .get_mut()
        .as_mut()
        .unwrap()
        .preset
        .scene_labels = before;
    assert_eq!(
        dirty(loaded::sync(&mut control, &mut state, at(330))),
        [Some(false)]
    );
}

/// Mode (14) UPDATE `0x271aa0`: an unavailable mode becomes the first available one
/// (`0x58c738`); a new list sets its first mode (`0x58ce2c`). The changes are pushed: the
/// list (`0x273854`), then the mode (`0x2734e8`). READ adds the list; no ATMA page on QC.
#[test]
fn mode_update_follows_the_available_list_and_pushes_changes() {
    use tetrad_control::codecs::{AvailableModes, ModeMessage};
    use tetrad_settings::controls::FootswitchMode;
    let (mut state, mut control) = loaded::session();
    let mut host = |state: &mut tetrad_app::device::State, message: ModeMessage, ms: u64| {
        loaded::host(&mut control, state, message, at(ms));
        loaded::sync(&mut control, state, at(ms + 1))
            .into_iter()
            .filter_map(|message| match message {
                ControlMessage::Mode(m) => Some(m),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let mode = |mode: u32| ModeMessage {
        action: update(),
        mode: Some(mode),
        ..Default::default()
    };
    host(&mut state, mode(0), 5);
    let pushed = host(&mut state, mode(2), 10);
    assert_eq!(state.settings.settings().mode.mode(), FootswitchMode::Stomp);
    assert_eq!(pushed.iter().map(|m| m.mode).collect::<Vec<_>>(), [Some(2)]);
    // PresetStomp (4) is not in the default list: the first mode (Preset) plays.
    let pushed = host(&mut state, mode(4), 20);
    assert_eq!(
        state.settings.settings().mode.mode(),
        FootswitchMode::Preset
    );
    assert_eq!(pushed.iter().map(|m| m.mode).collect::<Vec<_>>(), [Some(0)]);

    let list = ModeMessage {
        action: update(),
        available_modes: Some(AvailableModes { modes: vec![1, 2] }),
        ..Default::default()
    };
    let pushed = host(&mut state, list, 30);
    let [listed, mode] = &pushed[..] else {
        panic!("{pushed:?}");
    };
    assert_eq!(listed.available_modes.as_ref().unwrap().modes, [1, 2]);
    assert_eq!((mode.mode, mode.atma_page), (Some(1), None));

    let read = ModeMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read, at(40));
    let [ControlMessage::Mode(read)] = &sent[..] else {
        panic!("{sent:?}");
    };
    assert_eq!(read.mode, Some(1));
    assert_eq!(read.available_modes.as_ref().unwrap().modes, [1, 2]);

    // `0x589a60` saves the ATMA page only on ATMA; the QC keeps none (owned).
    let page = ModeMessage {
        action: update(),
        atma_page: Some(3),
        ..Default::default()
    };
    let saved = state.settings.settings().mode.atma_page;
    loaded::host(&mut control, &mut state, page, at(50));
    let pushed = loaded::sync(&mut control, &mut state, at(51));
    assert!(!pushed.iter().any(|m| matches!(m, ControlMessage::Mode(_))));
    assert_eq!(state.settings.settings().mode.atma_page, saved);
}

/// MasterVolume (17) on ATMA: UPDATE `0x1962c8` → `0x1fbb7c` clamps, rounds to 0.01, sets the
/// DSP level and saves AtmaVolume; READ reports the level, engaged, then the second receiver
/// `0x1969b4` replies with only `calibrate` (S7 stock `[46]`, `[48]`).
#[test]
fn atma_master_volume_update_sets_and_saves_the_level_and_read_reports_it() {
    use tetrad_app::device::actions::master_volume;
    use tetrad_control::codecs::MasterVolumeMessage;
    let (mut state, mut control) = loaded::atma();
    let set = MasterVolumeMessage {
        action: update(),
        volume: Some(0.456),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, set, at(10));
    assert!(sent.is_empty());
    assert_eq!(state.settings.settings().atma_volume.get(), 0.46);
    assert!(effects.contains(&master_volume(0.46)));
    let read = MasterVolumeMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read, at(20));
    let [
        ControlMessage::MasterVolume(read),
        ControlMessage::MasterVolume(calibrate),
    ] = &sent[..]
    else {
        panic!("{sent:?}");
    };
    assert_eq!((read.volume, read.engaged), (Some(0.46), Some(true)));
    assert_eq!(read.calibrate, None);
    assert_eq!(
        calibrate,
        &MasterVolumeMessage {
            action: update(),
            calibrate: Some(false),
            ..Default::default()
        }
    );
}

// --- w5 Control gaps 3 ---------------------------------------------------------------------

/// MasterVolume (17) UPDATE on the QC (`0x1fbb7c`, QC branch): `engaged` false goes out at
/// once (`0x19b518`), the knob must pick the level up, and the level echo (`0x19af74`) waits
/// 100 ms because the receiver suspends the sender (`0x19ad34`). READ reports the level and
/// `engaged`; an equal level does nothing.
#[test]
fn qc_master_volume_update_takes_the_level_and_echoes_it_100_ms_later() {
    use tetrad_app::device::actions::master_volume;
    use tetrad_app::device::input::VolumeOwner;
    use tetrad_control::codecs::MasterVolumeMessage;
    let (mut state, mut control) = loaded::session();
    let set = |volume| MasterVolumeMessage {
        action: update(),
        volume: Some(volume),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, set(0.5), at(10));
    assert!(sent.is_empty());
    let [ControlMessage::MasterVolume(engaged)] = &loaded::notified(&effects)[..] else {
        panic!("{effects:?}");
    };
    assert_eq!((engaged.volume, engaged.engaged), (None, Some(false)));
    assert!(effects.contains(&master_volume(0.5)));
    assert_eq!(state.master_volume.owner, VolumeOwner::PickupFromBelow);

    let echo = |sent: Vec<ControlMessage>| -> Vec<(Option<f32>, Option<bool>)> {
        sent.into_iter()
            .filter_map(|message| match message {
                ControlMessage::MasterVolume(m) => Some((m.volume, m.engaged)),
                _ => None,
            })
            .collect()
    };
    assert!(echo(loaded::sync(&mut control, &mut state, at(109))).is_empty());
    assert_eq!(
        echo(loaded::sync(&mut control, &mut state, at(110))),
        [(Some(0.5), None)]
    );
    assert!(echo(loaded::sync(&mut control, &mut state, at(300))).is_empty());

    let read = MasterVolumeMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read, at(320));
    // The second reply is the calibration receiver's (`0x1969b4`).
    assert_eq!(echo(sent), [(Some(0.5), Some(false)), (None, None)]);
    let (_, effects) = loaded::host(&mut control, &mut state, set(0.5), at(330));
    assert!(effects.is_empty());
    assert!(echo(loaded::sync(&mut control, &mut state, at(500))).is_empty());
}

/// `0x1a6dfc` sends nothing while a capture preset runs (`+0x758`); the slot `0x5ad250` still
/// caches the flag (`+0x2e5d`), so the change is not sent afterwards either.
#[test]
fn preset_dirty_is_not_pushed_while_a_capture_preset_runs() {
    use tetrad_app::device::preset::Snapshot;
    use tetrad_control::codecs::SceneLabelMessage;
    let (mut state, mut control) = loaded::session();
    *state.clean.get_mut() = state.current.as_ref().map(Snapshot::of);
    loaded::sync(&mut control, &mut state, at(5));
    let label = SceneLabelMessage {
        action: update(),
        index: Some(1),
        label: Some("Verse".into()),
        ..Default::default()
    };
    loaded::host(&mut control, &mut state, label, at(10));
    let dirty = |effects: Vec<Effect>| {
        replies(effects)
            .0
            .into_iter()
            .any(|(message, _)| matches!(message, ControlMessage::PresetDirty(_)))
    };
    assert!(!dirty(control.sync(&mut state, true, at(15)).unwrap()));
    assert!(!dirty(control.sync(&mut state, false, at(20)).unwrap()));
}

/// The stock dirty test compares float parameter values with `0x5ce580`: two NaN are equal,
/// and values within a relative 1e-5 match. Rust `==` on the preset says neither.
#[test]
fn dirty_check_treats_two_nan_values_and_close_floats_as_equal() {
    use tetrad_app::device::preset::Snapshot;
    use tetrad_preset::ParamValue;
    let (mut state, _) = loaded::session();
    let current = state.current.get_mut().as_mut().unwrap();
    let set = |current: &mut tetrad_app::device::Current, value: f32| {
        current.preset.chains[0].output_control[0].params[0].values[0].value =
            Some(ParamValue::Float(value));
    };
    set(current, f32::NAN);
    let clean = Snapshot::of(current);
    assert!(clean.matches(current));
    set(current, 0.5);
    let clean = Snapshot::of(current);
    set(current, 0.5 * (1.0 + 1e-6));
    assert!(clean.matches(current));
    set(current, 0.6);
    assert!(!clean.matches(current));
}

/// `0x58ce2c`: a new allowed list sets its first mode even when the current mode stays
/// available; an equal list changes nothing.
#[test]
fn set_allowed_modes_takes_the_first_mode_of_a_new_list() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::reduce;
    use tetrad_settings::controls::FootswitchMode::{Scene, Stomp};
    let (mut state, _) = loaded::session();
    reduce(Action::SetMode(Stomp), &mut state).unwrap();
    reduce(Action::SetAllowedModes(vec![Scene, Stomp]), &mut state).unwrap();
    let mode = &state.settings.settings().mode;
    assert_eq!(
        (mode.mode(), mode.available()),
        (Scene, &[Scene, Stomp][..])
    );
    reduce(Action::SetMode(Stomp), &mut state).unwrap();
    let effects = reduce(Action::SetAllowedModes(vec![Scene, Stomp]), &mut state).unwrap();
    assert!(effects.is_empty());
    assert_eq!(state.settings.settings().mode.mode(), Stomp);
}

/// Be Right Back (`0x6a82ac`) ends with GeneralSettings UPDATE `power_option` Standby (2) on
/// enter and WakeUp (3) on exit (`0x16b758`), after the DSP mute step.
#[test]
fn standby_enter_and_exit_tell_control_last() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::reduce;
    use tetrad_control::codecs::{EnumValue, PowerOptions};
    let (mut state, _) = loaded::session();
    for (on, option) in [(true, PowerOptions::Standby), (false, PowerOptions::WakeUp)] {
        let effects = reduce(Action::Standby(on), &mut state).unwrap();
        let notify = effects
            .iter()
            .position(|effect| matches!(effect, Effect::ControlNotify(_)))
            .expect("the Control notify");
        let mute = effects
            .iter()
            .rposition(|effect| matches!(effect, Effect::Dsp(_)))
            .expect("the DSP mute");
        assert!(mute < notify, "{effects:?}");
        let Effect::ControlNotify(message) = &effects[notify] else {
            unreachable!();
        };
        let ControlMessage::GeneralSettings(general) = &**message else {
            panic!("{message:?}");
        };
        assert_eq!(general.power_option, Some(EnumValue::Known(option)));
        assert_eq!(general.action, update());
    }
}

/// License UPDATE (`0x113094` → `0x1f0e80`) and CloudLogin DELETE (`0x239828` → `0x3080b4`,
/// unpair then sign out) hand the work to the services bridge and send no reply.
/// License UPDATE (`0x113094`), CloudLogin DELETE (`0x239828`) and CREATE (`0x307e18`, with
/// both tokens; without them nothing) and SystemTimeSync UPDATE (`0x1ba1b0`) hand their work
/// to the services bridge. Fake tokens.
#[test]
fn account_and_clock_requests_hand_work_to_services() {
    use tetrad_app::device::control::AccountRequest;
    use tetrad_control::codecs::{
        AuthorizationToken, CloudLoginMessage, LicenseMessage, SystemTimeSyncMessage,
    };
    let (mut state, mut control) = loaded::session();
    let licence = LicenseMessage {
        action: update(),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, licence, at(10));
    assert!(sent.is_empty());
    assert!(effects.contains(&Effect::ControlAccount(AccountRequest::RefreshLicences)));
    let logout = CloudLoginMessage {
        action: Some(MessageAction::Delete.into()),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, logout, at(20));
    assert!(sent.is_empty());
    assert!(effects.contains(&Effect::ControlAccount(AccountRequest::SignOut)));

    let create = |token| CloudLoginMessage {
        action: Some(MessageAction::Create.into()),
        request_id: Some(31),
        username: Some("fake-user".into()),
        token,
    };
    let tokens = AuthorizationToken {
        access_token: Some("fake-access".into()),
        refresh_token: Some("fake-refresh".into()),
        expires_in: None,
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, create(Some(tokens)), at(30));
    assert!(sent.is_empty());
    let login = effects.iter().find_map(|effect| match effect {
        Effect::ControlAccount(AccountRequest::Login(login)) => login.take(),
        _ => None,
    });
    let login = login.expect("a Control login for the services bridge");
    assert_eq!(
        (login.username.as_deref(), login.request_id),
        (Some("fake-user"), Some(31))
    );
    assert_eq!(login.tokens.access_token.expose(), b"fake-access");
    let (_, effects) = loaded::host(&mut control, &mut state, create(None), at(40));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::ControlAccount(_)))
    );

    let time = SystemTimeSyncMessage {
        action: update(),
        ms_since_epoch: Some(1_791_590_400_000),
        ..Default::default()
    };
    let (_, effects) = loaded::host(&mut control, &mut state, time, at(50));
    assert!(
        effects.contains(&Effect::ControlAccount(AccountRequest::SetClock(
            1_791_590_400_000
        )))
    );
}

/// A Preset MIDI Out edit sends MIDISettings UPDATE with one `general_midi_messages` entry:
/// the 12-message list, `source` only for a source list (`0x5146d4`, `0x514784`).
#[test]
fn preset_midi_out_edit_notifies_control_with_the_stored_list() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::reduce;
    use tetrad_ui::{MidiMessageKind, MidiOutMessage, MidiOutTarget};
    let (mut state, _) = loaded::session();
    let cc = MidiOutMessage {
        kind: MidiMessageKind::Cc,
        channel: 3,
        values: [Some(20), Some(127), None],
    };
    for (target, source) in [
        (MidiOutTarget::OnLoad, None),
        (MidiOutTarget::Expression1, Some(8)),
    ] {
        let edit = Action::SetPresetMidi {
            target,
            messages: vec![cc],
        };
        let effects = reduce(edit, &mut state).unwrap();
        let [ControlMessage::MIDISettings(settings)] = &loaded::notified(&effects)[..] else {
            panic!("{effects:?}");
        };
        assert_eq!(settings.action, update());
        assert_eq!(settings.preset_load_messages, None);
        let [entry] = &settings.general_midi_messages.as_ref().unwrap().messages[..] else {
            panic!("{settings:?}");
        };
        assert_eq!(entry.source, source);
        assert_eq!(entry.messages.len(), 12);
        let first = &entry.messages[0];
        assert_eq!(
            (
                first.message_type,
                first.channel,
                first.param1,
                first.param2
            ),
            (Some(1), Some(3), Some(20), Some(127))
        );
        assert_eq!(entry.messages[1].message_type, None);
    }
}

/// RecallPreset UPDATE (`0x274220` → `0x5b22a8`): the device loads the prepared preset and
/// keeps its direct undo frame (`0x648f1c`) on the commit, also inside the 500 ms window of
/// a Control timer edit (`0x17004c` merges only its own edits). The reply follows the
/// deploy: the working preset with reason Other and the request ID (`0x277468`), last
/// after the pushes the request caused; after a refused deploy it is the live preset and no
/// frame stays. A preset that does not decode (`name` as a varint) is refused.
#[test]
fn recall_preset_keeps_a_direct_frame_and_replies_after_the_deploy() {
    use tetrad_control::codecs::{MessageAction::Update, RecallPresetMessage, RecallPresetReason};
    use tetrad_preset::{Chain, Model};
    let (mut state, mut control) = loaded::session();
    let lane = grid::chain(
        0,
        Chain {
            output_control: vec![Model {
                params: vec![grid::param(0, 0.3)],
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    loaded::host(&mut control, &mut state, grid::message(Update, lane), at(0));
    loaded::done(&mut control, &mut state, at(0));
    assert_eq!(state.history.depths(), (1, 0));

    state.now = at(100);
    let live = state.current.as_ref().unwrap().preset.clone();
    let mut recalled = live.clone();
    recalled.name = Some("Recalled".into());
    let recall = |bytes| RecallPresetMessage {
        action: update(),
        request_id: Some(9),
        preset: Some(RawMessageBytes { bytes }),
        ..Default::default()
    };
    let reply = |sent: &[(ControlMessage, Option<u64>)]| match sent.last() {
        Some((ControlMessage::RecallPreset(reply), id)) => {
            assert_eq!(reply.reason, Some(RecallPresetReason::Other.into()));
            assert!(sent.iter().all(|(_, other)| other == id), "{sent:?}");
            (reply.preset.clone().unwrap().bytes, *id)
        }
        _ => panic!("{sent:?}"),
    };

    // A refused deploy: the effect does not commit.
    let (sent, _) = loaded::host(
        &mut control,
        &mut state,
        recall(recalled.to_bytes()),
        at(100),
    );
    assert!(sent.is_empty(), "the reply waits for the deploy");
    let sent = loaded::done(&mut control, &mut state, at(100));
    assert_eq!(reply(&sent), (live.to_bytes(), Some(9)));
    assert_eq!(state.history.depths(), (1, 0));

    let (_, effects) = loaded::host(
        &mut control,
        &mut state,
        recall(recalled.to_bytes()),
        at(150),
    );
    loaded::commit(&mut state, effects);
    let sent = loaded::done(&mut control, &mut state, at(150));
    let current = &state.current.as_ref().unwrap().preset;
    assert_eq!(current.name.as_deref(), Some("Recalled"));
    assert_eq!(reply(&sent), (current.to_bytes(), Some(9)));
    assert_eq!(state.history.depths(), (2, 0));

    let (sent, effects) = loaded::host(&mut control, &mut state, recall(vec![0x08, 0x01]), at(200));
    assert!(sent.is_empty());
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::ControlRecall { .. }))
    );
    assert_eq!(state.history.depths(), (2, 0));
}

// --- w5 Control gaps 4: Grid UPDATE/DELETE, GridMove, resync ---------------------------

mod grid {
    use std::time::Duration;

    use tetrad_app::device::control::Control;
    use tetrad_app::device::{Effect, State};
    use tetrad_control::codecs::{ControlMessage, GridMessage, MessageAction, RawMessageBytes};
    use tetrad_preset::{Chain, Model, Param, ParamValue, Preset, SceneValue};

    use super::{replies, update};

    /// A Grid UPDATE (or `action`) with a partial preset and request ID 7.
    pub fn message(action: MessageAction, preset: Preset) -> GridMessage {
        GridMessage {
            action: Some(action.into()),
            request_id: Some(7),
            preset: Some(RawMessageBytes {
                bytes: preset.to_bytes(),
            }),
            ..Default::default()
        }
    }

    pub fn chain(row: u32, chain: Chain) -> Preset {
        Preset {
            chains: vec![Chain {
                row: Some(row),
                ..chain
            }],
            ..Default::default()
        }
    }

    pub fn block(row: u32, model: Model) -> Preset {
        chain(
            row,
            Chain {
                models: vec![model],
                ..Default::default()
            },
        )
    }

    pub fn param(index: u32, value: f32) -> Param {
        Param {
            index: Some(index),
            values: vec![SceneValue {
                value: Some(ParamValue::Float(value)),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// The preset of a Grid message, with its request ID.
    pub fn sent(message: &ControlMessage) -> (Preset, Option<u64>) {
        let ControlMessage::Grid(grid) = message else {
            panic!("not a Grid message: {message:?}");
        };
        assert_eq!(grid.action, update());
        let bytes = &grid.preset.as_ref().unwrap().bytes;
        let preset = Preset::decode(bytes, tetrad_preset::Limits::default()).unwrap();
        (preset, grid.request_id)
    }

    /// The link's 100 ms senders after `now`, decoded.
    pub fn tick(control: &mut Control, state: &mut State, now: Duration) -> Vec<ControlMessage> {
        let effects = control.tick(state, now).unwrap();
        replies(effects).0.into_iter().map(|(m, _)| m).collect()
    }

    pub fn has(effects: &[Effect], kind: fn(&Effect) -> bool) -> bool {
        effects.iter().any(kind)
    }
}

/// Grid (1) UPDATE block sub-handlers (`0x1783b0`): a model with only a hash adds the
/// catalog block (`0x17422c`, a direct frame kept when the deploy commits) and a parameter
/// value changes it (`0x1727a0`, a timer frame). The value echo waits 100 ms and keeps the request ID (`0x25b248`,
/// `0x262b1c`); an unchanged value does nothing. The bypass field toggles a changed block
/// and echoes inside the receiver (`0x254910` → `0x25d364`); DELETE removes the block
/// (`0x1724b4`). While a capture preset runs every Grid edit is refused.
#[test]
fn grid_block_edits_record_undo_frames_and_follow_stock_echoes() {
    use tetrad_control::codecs::MessageAction::{Delete, Update};
    use tetrad_preset::{BypassRow, ColBypass, Model, ParamValue, Preset, SceneBypass};
    let (mut state, mut control) = loaded::session();
    let slot = tetrad_preset::Slot { row: 1, column: 2 };
    let add = grid::message(
        Update,
        grid::block(
            1,
            Model {
                hash: Some(1),
                column: Some(2),
                ..Default::default()
            },
        ),
    );
    let (sent, effects) = loaded::host(&mut control, &mut state, add.clone(), at(0));
    assert!(sent.is_empty());
    assert_eq!(
        state.history.depths(),
        (0, 0),
        "the frame waits for the deploy"
    );
    assert_eq!(loaded::commit(&mut state, effects), 1);
    assert_eq!(state.history.depths(), (1, 0));

    state.now = at(1000);
    let gain = grid::message(
        Update,
        grid::block(
            1,
            Model {
                column: Some(2),
                params: vec![grid::param(0, 0.25)],
                ..Default::default()
            },
        ),
    );
    let (sent, effects) = loaded::host(&mut control, &mut state, gain.clone(), at(1000));
    assert!(sent.is_empty());
    assert!(grid::has(&effects, |e| matches!(e, Effect::Deploy(_))));
    assert_eq!(state.history.depths(), (2, 0));
    assert!(grid::tick(&mut control, &mut state, at(1050)).is_empty());
    let echo = grid::tick(&mut control, &mut state, at(1100));
    assert_eq!(echo.len(), 1);
    let (preset, request_id) = grid::sent(&echo[0]);
    assert_eq!(request_id, Some(7));
    let echoed = &preset.chains[0];
    assert_eq!((echoed.row, echoed.models[0].column), (Some(1), Some(2)));
    let value = echoed.models[0].params[0].values[0].value.clone();
    assert_eq!(
        (echoed.models[0].params[0].index, value),
        (Some(0), Some(ParamValue::Float(0.25)))
    );
    state.now = at(2000);
    let (_, effects) = loaded::host(&mut control, &mut state, gain, at(2000));
    assert!(effects.is_empty());
    assert_eq!(state.history.depths(), (2, 0));

    state.now = at(3000);
    let bypass = Preset {
        bypass: vec![BypassRow {
            row: Some(1),
            col_bypass: vec![ColBypass {
                column: Some(2),
                scene_bypass: vec![SceneBypass {
                    bypass: true,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let (sent, effects) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, bypass.clone()),
        at(3000),
    );
    assert!(grid::has(&effects, |e| matches!(e, Effect::Deploy(_))));
    assert_eq!(state.history.depths(), (3, 0));
    assert_eq!(sent.len(), 1);
    assert_eq!(grid::sent(&sent[0]), (bypass, Some(7)));

    state.now = at(4000);
    let remove = grid::block(
        1,
        Model {
            column: Some(2),
            ..Default::default()
        },
    );
    let (_, effects) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Delete, remove),
        at(4000),
    );
    assert_eq!(loaded::commit(&mut state, effects), 1);
    let current = state.current.as_ref().unwrap();
    assert_eq!(current.preset.block(slot).unwrap(), None);

    control.sync(&mut state, true, at(4100)).unwrap();
    let (_, effects) = loaded::host(&mut control, &mut state, add, at(4200));
    assert!(!grid::has(&effects, |e| matches!(
        e,
        Effect::DeployLayout(_)
    )));
}

/// Stomp momentary flag (`0x5227c4`): each assigned model 26001 takes it as parameter 1
/// (`0x51e4d8`) with the 100 ms block value echo (`0x25d51c`), before the flag is stored and
/// echoed (`0x26055c`).
#[test]
fn grid_momentary_flag_drives_the_26001_parameter() {
    use tetrad_control::codecs::MessageAction::Update;
    use tetrad_preset::{MapField, Model, ParamValue, Preset, StompAssignment, StompRole};
    let (mut state, mut control) = loaded::session();
    let add = grid::block(
        1,
        Model {
            hash: Some(26_001),
            column: Some(2),
            ..Default::default()
        },
    );
    let (_, effects) = loaded::host(&mut control, &mut state, grid::message(Update, add), at(0));
    assert_eq!(loaded::commit(&mut state, effects), 1);
    let assign = Preset {
        stomp_mode_assignments: vec![StompAssignment {
            row: 1,
            column: 2,
            stomp_index: 3,
            role: Some(StompRole::Primary),
            ..Default::default()
        }],
        ..Default::default()
    };
    loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, assign),
        at(1000),
    );
    let mut stomp_is_momentary = MapField::default();
    stomp_is_momentary.insert(3, true);
    let momentary = Preset {
        stomp_is_momentary,
        ..Default::default()
    };
    let (sent, _) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, momentary.clone()),
        at(2000),
    );
    assert_eq!(grid::sent(&sent[0]).0, momentary);
    let preset = &state.current.as_ref().unwrap().preset;
    let block = preset
        .block(tetrad_preset::Slot { row: 1, column: 2 })
        .unwrap()
        .unwrap();
    assert_eq!(
        block.params[1].values[0].value,
        Some(ParamValue::Float(1.0))
    );
    let echo = grid::tick(&mut control, &mut state, at(2100));
    let echoed = &grid::sent(&echo[0]).0.chains[0].models[0].params[0];
    assert_eq!(
        (echoed.index, echoed.values[0].value.clone()),
        (Some(1), Some(ParamValue::Float(1.0)))
    );
}

/// Grid block set with a ModelPresetID (`0x17422c` → `0x1f7e74` → `0x1f7b10`): a fresh block
/// of the preset's model takes its rows from `factory.db` (factory flag, `0x1f78f4`), after
/// one undo frame; the rows' EXP link and range and the preset switch are stored with it.
#[test]
fn grid_block_from_a_factory_device_preset_stores_its_rows_and_links() {
    use tetrad_control::codecs::{GridMessage, MessageAction, ModelPresetID};
    use tetrad_preset::Model;
    let path =
        std::env::temp_dir().join(format!("tetrad-app-control-vdp-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE product ( id TEXT, type INT, readonly INT, rawdata BLOB, metadata BLOB, PRIMARY KEY(id));
             CREATE TABLE folder ( id INTEGER PRIMARY KEY AUTOINCREMENT, pdType INT, folderType INT, name TEXT, parent INTEGER, readonly INT);
             CREATE TABLE folder_content ( folderId INTEGER, productId TEXT, PRIMARY KEY(folderId,productId));
             CREATE TABLE model_presets ( id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, hash INT, expSwitchType INT, expSwitchInverted INT, expSwitchLatchEmulation INT, expSwitchDelayMs INT);
             CREATE TABLE model_preset_parameters (presetId INT, paramIndex INT, value TEXT, exp INT, expTrimMax REAL, expTrimMin REAL, PRIMARY KEY (presetId, paramIndex));
             CREATE TABLE schema_version ( major INTEGER, minor INTEGER);
             INSERT INTO schema_version VALUES (410, 1);
             INSERT INTO model_presets VALUES (3, 'Edge', 1, 1, 1, 0, 250);
             INSERT INTO model_preset_parameters VALUES (3, 0, '0.8', 2, 0.9, 0.1);",
        )
        .unwrap();
    let factory = tetrad_storage::stock::StockDb::open_factory(&path, Default::default()).unwrap();
    let (mut state, mut control) = loaded::session();
    control.set_factory_presets(factory);
    let block = Model {
        hash: Some(1),
        column: Some(2),
        ..Default::default()
    };
    let message = GridMessage {
        model_preset_to_load: Some(ModelPresetID {
            value: Some("3".into()),
            is_factory: Some(true),
            hash: Some(1),
        }),
        ..grid::message(MessageAction::Update, grid::block(1, block))
    };
    let (_, effects) = loaded::host(&mut control, &mut state, message, at(0));
    assert_eq!(loaded::commit(&mut state, effects), 1);
    assert_eq!(state.history.depths(), (1, 0));
    let current = state.current.as_ref().unwrap();
    let stored = current
        .preset
        .block(tetrad_preset::Slot { row: 1, column: 2 });
    let stored = stored.unwrap().unwrap();
    let gain = &stored.params[0];
    assert_eq!(
        (gain.values[0].value.clone(), gain.expression),
        (Some(tetrad_preset::ParamValue::Float(0.8)), Some(2))
    );
    assert_eq!(
        (gain.expression_min, gain.expression_max),
        (Some(0.1), Some(0.9))
    );
    let info = &stored.expression_bypass_info[0];
    assert_eq!((info.kind, info.invert, info.delay_ms), (1, true, 250));
    let _ = std::fs::remove_file(&path);
}

/// Lane (`0x727ad4`) and input gate (`0x727794`) values: an undo frame, the edit, and the
/// echo inside the receiver with the request ID (`0x25ea1c`, `0x25edcc`). The capture lock
/// keeps the gate row (`0x582504`, inferred).
#[test]
fn grid_lane_and_gate_values_echo_inside_the_receiver() {
    use tetrad_control::codecs::MessageAction::Update;
    use tetrad_preset::{Chain, Model};
    let (mut state, mut control) = loaded::session();
    let lane = grid::chain(
        0,
        Chain {
            output_control: vec![Model {
                params: vec![grid::param(0, 0.5)],
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    let (sent, effects) =
        loaded::host(&mut control, &mut state, grid::message(Update, lane), at(0));
    assert!(grid::has(&effects, |e| matches!(e, Effect::Deploy(_))));
    assert_eq!(state.history.depths(), (1, 0));
    let ControlMessage::Grid(echo) = &sent[0] else {
        panic!("{sent:?}");
    };
    let expected =
        tetrad_app::device::control::lane_echo(tetrad_engine::Row::new(0).unwrap(), 0, 0.5);
    let ControlMessage::Grid(expected) = expected else {
        unreachable!()
    };
    assert_eq!((&echo.preset, echo.request_id), (&expected.preset, Some(7)));

    let gate = grid::chain(
        2,
        Chain {
            input_control: vec![Model {
                params: vec![grid::param(0, 0.25)],
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    let (sent, effects) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, gate.clone()),
        at(1000),
    );
    assert!(grid::has(&effects, |e| matches!(e, Effect::DeployGate(_))));
    assert_eq!(grid::sent(&sent[0]), (gate.clone(), Some(7)));

    control.capture_lock_changed(true, &state);
    let (sent, effects) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, gate),
        at(2000),
    );
    assert!(sent.is_empty() && effects.is_empty());
}

/// Lane MUTE and SOLO store `0.5 < value` as 1.0 or 0.0 and the echo holds the stored value
/// (`0x520498`, `0x520540` → `0x25ea1c`). Owned: a non-finite value refuses the whole Grid
/// UPDATE before any edit or frame.
#[test]
fn grid_lane_mute_stores_a_flag_and_non_finite_values_are_refused() {
    use tetrad_control::codecs::MessageAction::Update;
    use tetrad_preset::{Chain, Model};
    let (mut state, mut control) = loaded::session();
    let lane = |index, value| {
        let output_control = vec![Model {
            params: vec![grid::param(index, value)],
            ..Default::default()
        }];
        let chain = Chain {
            output_control,
            ..Default::default()
        };
        grid::message(Update, grid::chain(1, chain))
    };
    let (sent, _) = loaded::host(&mut control, &mut state, lane(2, 0.7), at(0));
    let row = tetrad_engine::Row::new(1).unwrap();
    let ControlMessage::Grid(expected) = tetrad_app::device::control::lane_echo(row, 2, 1.0) else {
        unreachable!()
    };
    assert_eq!(
        grid::sent(&sent[0]).0.to_bytes(),
        expected.preset.unwrap().bytes
    );
    assert_eq!(state.history.depths(), (1, 0));

    let (sent, effects) = loaded::host(&mut control, &mut state, lane(0, f32::NAN), at(1000));
    assert!(sent.is_empty(), "{sent:?}");
    assert!(grid::has(&effects, |e| matches!(e, Effect::Notice(..))));
    assert!(!grid::has(&effects, |e| matches!(e, Effect::Deploy(_))));
    assert_eq!(state.history.depths(), (1, 0));
}

/// SceneCopy (22) `0x1b048c` → `0x5899d8(to, 1)`: the recall is forced, so a copy onto the
/// playing scene recalls it too.
#[test]
fn scene_copy_onto_the_playing_scene_recalls_it() {
    use tetrad_control::codecs::SceneCopyMessage;
    let (mut state, mut control) = loaded::session();
    let copy = SceneCopyMessage {
        action: update(),
        from_index: Some(3),
        to_index: Some(0),
        ..Default::default()
    };
    let (_, effects) = loaded::host(&mut control, &mut state, copy, at(0));
    assert!(
        effects.iter().any(|e| matches!(e, Effect::RecallScene(_))),
        "{effects:?}"
    );
}

/// Expression links on a lane and an input gate parameter (`0x171b00` → `0x170110`): the
/// code and the message range are stored on that parameter after one undo frame each; a
/// code without a range keeps the stored range. No echo.
#[test]
fn grid_lane_and_gate_links_store_on_their_parameter() {
    use tetrad_control::codecs::MessageAction::Update;
    use tetrad_preset::{Chain, Model, Param};
    let (mut state, mut control) = loaded::session();
    let link = |code, range: Option<(f32, f32)>| Model {
        params: vec![Param {
            index: Some(0),
            expression: Some(code),
            expression_min: range.map(|(min, _)| min),
            expression_max: range.map(|(_, max)| max),
            ..Default::default()
        }],
        ..Default::default()
    };
    let lane = grid::chain(
        0,
        Chain {
            output_control: vec![link(2, Some((0.25, 0.75)))],
            ..Default::default()
        },
    );
    let gate = grid::chain(
        1,
        Chain {
            input_control: vec![link(1, None)],
            ..Default::default()
        },
    );
    for (at_ms, preset) in [(0, lane), (1000, gate)] {
        state.now = at(at_ms);
        let (sent, _) = loaded::host(
            &mut control,
            &mut state,
            grid::message(Update, preset),
            at(at_ms),
        );
        assert!(sent.is_empty());
    }
    assert_eq!(state.history.depths(), (2, 0));
    let chains = &state.current.as_ref().unwrap().preset.chains;
    let stored = |model: &Model| {
        let param = &model.params[0];
        (param.expression, param.expression_min, param.expression_max)
    };
    assert_eq!(
        stored(&chains[0].output_control[0]),
        (Some(2), Some(0.25), Some(0.75))
    );
    assert_eq!(
        stored(&chains[1].input_control[0]),
        (Some(1), Some(0.0), Some(1.0))
    );
}

/// Lane switch (`0x727e1c` → `0x171e78`): column 0 is MUTE. The code goes on lane parameter
/// 2 and the settings on `expression_bypass_info[0]` after one undo frame. The expression
/// engine then drives MUTE as a switch (`0x3242ec`, `0x6df4e0`): 0 or 1, never the position.
#[test]
fn grid_lane_mute_switch_stores_the_link_and_drives_mute() {
    use tetrad_control::codecs::MessageAction::Update;
    use tetrad_preset::{Chain, Expression, ExpressionBypassInfo, Model};
    let (mut state, mut control) = loaded::session();
    state.pedals[0].plugged = true;
    let switch = grid::chain(
        0,
        Chain {
            output_control: vec![Model {
                column: Some(0),
                bypass_expression: vec![Expression {
                    expression: 1,
                    ..Default::default()
                }],
                expression_bypass_info: vec![ExpressionBypassInfo {
                    kind: 1,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    let (sent, _) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, switch),
        at(0),
    );
    assert!(sent.is_empty());
    assert_eq!(state.history.depths(), (1, 0));
    let lane = &state.current.as_ref().unwrap().preset.chains[0].output_control[0];
    assert_eq!(lane.params[2].expression, Some(1));
    assert_eq!(lane.expression_bypass_info[0].kind, 1);

    let pedal = tetrad_hal::node::Pedal::One;
    let effects = tetrad_app::device::actions::reduce(
        tetrad_app::device::actions::Action::Expression {
            pedal,
            position: 0.75,
            from_midi: false,
        },
        &mut state,
    )
    .unwrap();
    assert!(grid::has(&effects, |e| matches!(e, Effect::Deploy(_))));
    let mute = (
        tetrad_engine::Site::OutputControl(tetrad_engine::Row::new(0).unwrap()),
        2,
    );
    assert_eq!(state.expression.values.get(&mute), Some(&1.0));
}

/// Block scene mode (`0x1700b8` → `0x517f54` / `0x524770`): a parameter's catalog
/// `linkedSceneMode` list (`0x5ce568`) follows it. Each index pushes its flag inside the
/// receiver (`0x25f1b4`); off also echoes each kept value 100 ms later (`0x25d51c`).
#[test]
fn grid_scene_mode_takes_linked_parameters_and_pushes_each_flag() {
    use tetrad_control::codecs::MessageAction::Update;
    use tetrad_preset::{Model, Param};
    let (mut state, mut control) = loaded::session();
    let add = grid::block(
        1,
        Model {
            hash: Some(2),
            column: Some(2),
            ..Default::default()
        },
    );
    let (_, effects) = loaded::host(&mut control, &mut state, grid::message(Update, add), at(0));
    loaded::commit(&mut state, effects);
    let mode = |enabled| {
        grid::block(
            1,
            Model {
                column: Some(2),
                params: vec![Param {
                    index: Some(0),
                    scene_mode: Some(enabled),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
    };
    let flags = |sent: &[ControlMessage]| -> Vec<_> {
        sent.iter()
            .map(|message| {
                let block = grid::sent(message).0.chains[0].models[0].clone();
                (
                    block.column,
                    block.params[0].index,
                    block.params[0].scene_mode,
                )
            })
            .collect()
    };
    let (sent, effects) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, mode(true)),
        at(1000),
    );
    loaded::commit(&mut state, effects);
    assert_eq!(
        flags(&sent),
        [
            (Some(2), Some(0), Some(true)),
            (Some(2), Some(1), Some(true))
        ]
    );
    let stored = state.current.as_ref().unwrap().preset.chains[1].models[2].clone();
    assert!(
        stored.params[..2]
            .iter()
            .all(|p| p.scene_mode == Some(true))
    );

    let (sent, effects) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, mode(false)),
        at(2000),
    );
    loaded::commit(&mut state, effects);
    assert_eq!(
        flags(&sent),
        [
            (Some(2), Some(0), Some(false)),
            (Some(2), Some(1), Some(false))
        ]
    );
    assert_eq!(grid::tick(&mut control, &mut state, at(2100)).len(), 2);
}

/// Review r11 N4: a value on a Pattern Tremolo STEP (`0x1727a0`, model 7040) also goes to
/// the other steps of its connected run at the current scene (`0x493e64`, then `0x523644`
/// per step), each with its own 100 ms echo.
#[test]
fn grid_pattern_tremolo_step_writes_its_connected_run() {
    use tetrad_control::codecs::MessageAction::Update;
    use tetrad_preset::Model;
    let params: String = (0..42)
        .map(|index| {
            format!("<Parameter name='P{index}' type='float' min='0' max='1' defaultValue='0'/>")
        })
        .collect();
    let (mut state, mut control) = loaded::session_with_extra(
        &format!(
            "<Category id='3' name='Modulation'><Model id='7040' name='Pattern' \
blob='pattern'>{params}</Model></Category>"
        ),
        "<Module id='7040' name='Pattern' blob_name='pattern' category='3' dm_heap='100' \
pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='1'/></Module>",
    );
    let block = |params: Vec<tetrad_preset::Param>| {
        grid::block(
            1,
            Model {
                hash: params.is_empty().then_some(7040),
                column: Some(2),
                params,
                ..Default::default()
            },
        )
    };
    let (_, effects) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, block(vec![])),
        at(0),
    );
    assert_eq!(loaded::commit(&mut state, effects), 1);
    // STEP 1 CONNECTED (27) and STEP 2 CONNECTED (28): steps 1..=3 (11..=13) are one run.
    for (index, ms) in [(27, 1000), (28, 2000)] {
        let connect = grid::message(Update, block(vec![grid::param(index, 1.0)]));
        loaded::host(&mut control, &mut state, connect, at(ms));
    }
    grid::tick(&mut control, &mut state, at(2100));

    let step = grid::message(Update, block(vec![grid::param(12, 0.75)]));
    loaded::host(&mut control, &mut state, step, at(3000));
    let current = state.current.as_ref().unwrap();
    let site = tetrad_engine::Site::Slot(tetrad_engine::Slot {
        row: tetrad_engine::Row::new(1).unwrap(),
        column: tetrad_engine::Column::new(2).unwrap(),
    });
    let instance = current.deployment.layout().instance(site).unwrap();
    let steps: Vec<f32> = (10..15)
        .map(|index| instance.params[index].at(current.deployment.scene()))
        .collect();
    assert_eq!(steps, [0.0, 0.75, 0.75, 0.75, 0.0]);
    let echoed: Vec<_> = grid::tick(&mut control, &mut state, at(3100))
        .iter()
        .map(|message| grid::sent(message).0.chains[0].models[0].params[0].index)
        .collect();
    assert_eq!(echoed, [Some(12), Some(11), Some(13)]);
}

/// Block MUTE (`0x1727a0` at the MUTE index → `0x5225c8` → `0x521dbc`): after the undo
/// frame a value that differs from MUTE in scene 0 sets it through the MUTE edit (the DSP
/// takes it in the bypass record) and echoes MUTE inside the receiver. A licence-locked block
/// (`0x2e02d0`) keeps its MUTE.
#[test]
fn grid_block_mute_sets_mute_and_echoes_it() {
    use tetrad_control::codecs::MessageAction::Update;
    use tetrad_preset::{Model, ParamValue};
    let (mut state, mut control) = loaded::session();
    let add = grid::block(
        1,
        Model {
            hash: Some(1),
            column: Some(2),
            ..Default::default()
        },
    );
    let (_, effects) = loaded::host(&mut control, &mut state, grid::message(Update, add), at(0));
    loaded::commit(&mut state, effects);
    // Light has one XML parameter: MUTE is index 1.
    let mute = grid::message(
        Update,
        grid::block(
            1,
            Model {
                column: Some(2),
                params: vec![grid::param(1, 1.0)],
                ..Default::default()
            },
        ),
    );
    state.now = at(1000);
    let (sent, effects) = loaded::host(&mut control, &mut state, mute.clone(), at(1000));
    assert!(grid::has(&effects, |e| matches!(e, Effect::Deploy(_))));
    assert_eq!(state.history.depths(), (2, 0));
    let current = state.current.as_ref().unwrap();
    let block = tetrad_engine::Site::Slot(tetrad_engine::Slot {
        row: tetrad_engine::Row::new(1).unwrap(),
        column: tetrad_engine::Column::new(2).unwrap(),
    });
    let instance = current.deployment.layout().instance(block).unwrap();
    assert_eq!(instance.mute, tetrad_engine::Scened::Fixed(true));
    let echoed = grid::sent(&sent[0]).0.chains[0].models[0].params[0].clone();
    assert_eq!(
        (echoed.index, echoed.values[0].value.clone()),
        (Some(1), Some(ParamValue::Float(1.0)))
    );
    state.now = at(2000);
    let (sent, effects) = loaded::host(&mut control, &mut state, mute, at(2000));
    assert!(sent.is_empty() && effects.is_empty());
    // `0x5225c8` skips a licence-locked block (`0x2e02d0`): a ModelRepo plugin whose licence
    // is not active. No toggle, no echo.
    (state.specs.as_mut().unwrap().models.get_mut(&1).unwrap()).plugin_id = Some(3);
    let unmute = grid::message(
        Update,
        grid::block(
            1,
            Model {
                column: Some(2),
                params: vec![grid::param(1, 0.0)],
                ..Default::default()
            },
        ),
    );
    state.now = at(3000);
    let (sent, effects) = loaded::host(&mut control, &mut state, unmute, at(3000));
    assert!(
        sent.is_empty() && effects.is_empty(),
        "{sent:?} {effects:?}"
    );
}

/// Tempo scene mode (`0x1700b8` row -1 → `0x517f54` / `0x524770`, model 25000): stored
/// without an undo frame; the flag goes back inside the receiver (`0x25f5ec`), and off also
/// pushes the value it keeps (`0x25f3f0`).
#[test]
fn grid_tempo_scene_mode_stores_and_pushes_the_flag() {
    use tetrad_control::codecs::MessageAction::Update;
    use tetrad_preset::{Model, Param, Preset};
    let (mut state, mut control) = loaded::session();
    let mode = |enabled| Preset {
        tempo_program_data: vec![Model {
            params: vec![Param {
                index: Some(2),
                scene_mode: Some(enabled),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let flag = |preset: &Preset| {
        let param = &preset.tempo_program_data[0].params[0];
        (param.index, param.scene_mode)
    };
    let (sent, _) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, mode(true)),
        at(0),
    );
    assert_eq!(sent.len(), 1);
    assert_eq!(flag(&grid::sent(&sent[0]).0), (Some(2), Some(true)));
    let tempo = |state: &tetrad_app::device::State| {
        let preset = &state.current.as_ref().unwrap().preset;
        preset.tempo_program_data[0].params[2].scene_mode
    };
    assert_eq!(tempo(&state), Some(true));

    let (sent, _) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, mode(false)),
        at(10),
    );
    assert_eq!(sent.len(), 2);
    assert_eq!(flag(&grid::sent(&sent[0]).0), (Some(2), Some(false)));
    let value = grid::sent(&sent[1]).0.tempo_program_data[0].params[0].clone();
    assert_eq!((value.index, value.values.len()), (Some(2), 1));
    assert_eq!(tempo(&state), Some(false));
    assert_eq!(state.history.depths(), (0, 0));
}

/// Routing sub-handlers of a chain without blocks: both row ports in one compile with one
/// undo frame (`0x59d4b4`, `0x59ccc4`); split and mix points (`0x2561d4`, echo `0x25e3f8`);
/// the junction bypass (`0x17b384` → `0x51dc80`, echo `splitBypass` `0x25e74c`); a mixer
/// value with the 100 ms echo (`0x17b084` → `0x25e55c`).
#[test]
fn grid_routing_edits_compile_once_and_echo_the_junction() {
    use tetrad_control::codecs::MessageAction::Update;
    use tetrad_preset::{Chain, Model, SceneBypass, SplitControlPoints};
    let (mut state, mut control) = loaded::session();
    let ports = grid::chain(
        0,
        Chain {
            in_portid: Some(2),
            out_portid: Some(1),
            ..Default::default()
        },
    );
    let (sent, effects) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, ports),
        at(0),
    );
    assert!(sent.is_empty());
    assert_eq!(state.history.depths(), (0, 0));
    assert_eq!(loaded::commit(&mut state, effects), 1);
    assert_eq!(state.history.depths(), (1, 0));

    let points = grid::chain(
        0,
        Chain {
            split_control_points: vec![SplitControlPoints {
                split: 2,
                mix: 5,
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    let (sent, effects) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, points.clone()),
        at(1000),
    );
    assert!(grid::has(&effects, |e| matches!(
        e,
        Effect::DeployLayout(_)
    )));
    assert_eq!(grid::sent(&sent[0]), (points, Some(7)));

    let bypass = grid::chain(
        0,
        Chain {
            split_bypass: vec![SceneBypass {
                bypass: true,
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    let (sent, effects) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, bypass.clone()),
        at(2000),
    );
    assert!(grid::has(&effects, |e| matches!(e, Effect::Deploy(_))));
    assert_eq!(grid::sent(&sent[0]), (bypass, Some(7)));

    let level = grid::chain(
        0,
        Chain {
            mixer: vec![Model {
                params: vec![grid::param(0, 0.75)],
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    let (sent, _) = loaded::host(
        &mut control,
        &mut state,
        grid::message(Update, level.clone()),
        at(3000),
    );
    assert!(sent.is_empty());
    // The device step sends a held echo when it is due (`0x262b1c`, 100 ms).
    let grid = |sent: Vec<ControlMessage>| -> Vec<ControlMessage> {
        sent.into_iter()
            .filter(|message| matches!(message, ControlMessage::Grid(_)))
            .collect()
    };
    assert!(grid(loaded::sync(&mut control, &mut state, at(3099))).is_empty());
    let echo = grid(loaded::sync(&mut control, &mut state, at(3100)));
    assert_eq!(grid::sent(&echo[0]), (level, Some(7)));
}

/// Preset-only sub-handlers record no undo frame and echo each change inside the receiver:
/// tags (`0x2551f0`, no echo), a stomp assignment (`0x254ed0` → `0x25e2b0`; DELETE clears
/// it without echo), a group stomp label (`0x254d10` → `0x25fdbc`), a momentary flag
/// (`0x254db0` → `0x26055c`) and a model update notification (`0x514fd8` → `0x25fa00`,
/// once per slot).
#[test]
fn grid_preset_fields_store_without_undo_and_echo_each_change() {
    use tetrad_control::codecs::MessageAction::{Delete, Update};
    use tetrad_preset::{MapField, Model, Preset, StompAssignment, StompRole};
    let (mut state, mut control) = loaded::session();
    let add = grid::block(
        1,
        Model {
            hash: Some(1),
            column: Some(2),
            ..Default::default()
        },
    );
    let (_, effects) = loaded::host(&mut control, &mut state, grid::message(Update, add), at(0));
    loaded::commit(&mut state, effects);
    let frames = state.history.depths();
    let mut host = |preset: &Preset, action, millis| {
        let message = grid::message(action, preset.clone());
        loaded::host(&mut control, &mut state, message, at(millis)).0
    };

    let tags = Preset {
        tags: vec!["Live".into()],
        ..Default::default()
    };
    assert!(host(&tags, Update, 1000).is_empty());
    let assign = Preset {
        stomp_mode_assignments: vec![StompAssignment {
            row: 1,
            column: 2,
            stomp_index: 3,
            role: Some(StompRole::Primary),
            ..Default::default()
        }],
        ..Default::default()
    };
    let echo = host(&assign, Update, 2000);
    assert_eq!(grid::sent(&echo[0]), (assign.clone(), Some(7)));
    let mut stomp_labels = MapField::default();
    stomp_labels.insert(3, "Drive".to_owned());
    let label = Preset {
        stomp_labels,
        ..Default::default()
    };
    assert_eq!(grid::sent(&host(&label, Update, 3000)[0]).0, label);
    let mut stomp_is_momentary = MapField::default();
    stomp_is_momentary.insert(3, true);
    let momentary = Preset {
        stomp_is_momentary,
        ..Default::default()
    };
    assert_eq!(grid::sent(&host(&momentary, Update, 4000)[0]).0, momentary);
    let notify = grid::block(
        1,
        Model {
            column: Some(2),
            notify_flag: Some(true),
            ..Default::default()
        },
    );
    assert_eq!(grid::sent(&host(&notify, Update, 5000)[0]).0, notify);
    assert!(host(&notify, Update, 6000).is_empty());
    {
        let preset = &state.current.as_ref().unwrap().preset;
        assert_eq!(preset.tags, ["Live"]);
        assert_eq!(preset.stomp_mode_assignments[0].stomp_index, 3);
        assert_eq!(
            preset.stomp_labels.get(3).map(String::as_str),
            Some("Drive")
        );
        assert_eq!(preset.stomp_is_momentary.get(3), Some(&true));
        assert_eq!(preset.model_update_notifications.len(), 1);
    }
    let mut host = |preset: &Preset, action, millis| {
        let message = grid::message(action, preset.clone());
        loaded::host(&mut control, &mut state, message, at(millis)).0
    };
    assert!(host(&assign, Delete, 7000).is_empty());
    let preset = &state.current.as_ref().unwrap().preset;
    assert!(preset.stomp_mode_assignments.is_empty());
    assert_eq!(state.history.depths(), frames);
}

/// GridMove (12) `0x26411c`: a move that fails the check `0x26321c` (here a stale grid
/// hash) changes nothing and pushes the current preset at once (`0x2778d4(.., 0)`); a valid
/// drop moves the block with a direct undo frame and sends no RecallPreset (B5 on the QC: its
/// stored `default_scene` moved Cortex Control off the playing scene). After the frame
/// lists the drop echoes its moves (`0x59f620` → `0x266290`: `moves` only), and Cortex
/// Control draws the move only on that echo (QC log `cc-p-stock-gm-tail.log`). A drop whose
/// deploy did not commit sends no echo.
#[test]
fn grid_move_checks_the_grid_then_moves_the_dropped_block() {
    use tetrad_control::codecs::{
        GridMoveElement, GridMoveGrid, GridMoveMessage, GridMoveRow, MessageAction::Update,
    };
    use tetrad_preset::{Model, Preset};
    let (mut state, mut control) = loaded::session();
    let add = grid::block(
        1,
        Model {
            hash: Some(1),
            column: Some(2),
            ..Default::default()
        },
    );
    let (_, effects) = loaded::host(&mut control, &mut state, grid::message(Update, add), at(0));
    loaded::commit(&mut state, effects);
    state.now = at(1000);
    let drag = |hash| GridMoveMessage {
        moves: vec![GridMoveElement {
            from_row: Some(1),
            from_col: Some(2),
            to_row: Some(1),
            to_col: Some(5),
            is_drop: Some(true),
        }],
        grid: Some(GridMoveGrid {
            rows: vec![
                GridMoveRow::default(),
                GridMoveRow {
                    model_ids: vec![0, 0, hash],
                },
            ],
        }),
        ..Default::default()
    };
    let reply_preset = |sent: Vec<ControlMessage>| {
        let reply = sent.iter().find_map(|message| match message {
            ControlMessage::RecallPreset(reply) => Some(reply),
            _ => None,
        });
        let bytes = &reply.unwrap().preset.as_ref().unwrap().bytes;
        Preset::decode(bytes, tetrad_preset::Limits::default()).unwrap()
    };
    let at_slot = |preset: &Preset, column| {
        let slot = tetrad_preset::Slot { row: 1, column };
        preset.block(slot).unwrap().and_then(|block| block.hash)
    };

    let (sent, effects) = loaded::host(&mut control, &mut state, drag(9), at(1000));
    assert!(effects.is_empty());
    assert_eq!(at_slot(&reply_preset(sent), 2), Some(1));
    assert_eq!(state.history.depths(), (1, 0));
    let done = loaded::done(&mut control, &mut state, at(1000));
    assert!(
        (done.iter()).all(|(message, _)| !matches!(message, ControlMessage::GridMove(_))),
        "a refused move has no echo: {done:?}"
    );

    // The deploy fails: the preset keeps the block at column 2, so no echo goes out.
    let (_, effects) = loaded::host(&mut control, &mut state, drag(1), at(1500));
    assert!(!effects.is_empty());
    let done = loaded::done(&mut control, &mut state, at(1500));
    assert!(
        (done.iter()).all(|(message, _)| !matches!(message, ControlMessage::GridMove(_))),
        "{done:?}"
    );

    let (sent, effects) = loaded::host(&mut control, &mut state, drag(1), at(2000));
    assert!(sent.is_empty());
    assert_eq!(loaded::commit(&mut state, effects), 1);
    let done = loaded::done(&mut control, &mut state, at(2000));
    assert!(
        (done.iter()).all(|(message, _)| !matches!(message, ControlMessage::RecallPreset(_))),
        "{done:?}"
    );
    let kinds: Vec<_> = (done.iter())
        .filter_map(|(message, _)| match message {
            ControlMessage::UndoRedo(_) => Some("UndoRedo"),
            ControlMessage::GridMove(_) => Some("GridMove"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["UndoRedo", "GridMove"], "{done:?}");
    let echo = GridMoveMessage {
        grid: None,
        ..drag(1)
    };
    let sent_echo = done.iter().find_map(|(message, id)| match message {
        ControlMessage::GridMove(echo) => Some((echo, *id)),
        _ => None,
    });
    assert_eq!(sent_echo, Some((&echo, None)));
    let stored = |column| {
        let slot = tetrad_preset::Slot { row: 1, column };
        let preset = &state.current.as_ref().unwrap().preset;
        preset.block(slot).unwrap().and_then(|block| block.hash)
    };
    assert_eq!((stored(2), stored(5)), (None, Some(1)));
    assert_eq!(state.history.depths(), (2, 0));
}

/// Places a model block at each `(column, hash)` of row 1 through Grid UPDATEs, each a whole
/// request.
fn row_one_blocks(
    control: &mut Control,
    state: &mut tetrad_app::device::State,
    blocks: &[(u32, u32)],
) {
    use tetrad_control::codecs::MessageAction::Update;
    for &(column, hash) in blocks {
        let add = grid::block(
            1,
            tetrad_preset::Model {
                hash: Some(hash),
                column: Some(column),
                ..Default::default()
            },
        );
        let (_, effects) = loaded::host(control, state, grid::message(Update, add), at(0));
        loaded::commit(state, effects);
        loaded::done(control, state, at(0));
    }
}

fn row_one_hashes(state: &tetrad_app::device::State) -> Vec<Option<u32>> {
    let preset = &state.current.as_ref().unwrap().preset;
    (0..8)
        .map(|column| {
            let slot = tetrad_preset::Slot { row: 1, column };
            preset.block(slot).unwrap().and_then(|block| block.hash)
        })
        .collect()
}

fn grid_move(from: (u32, u32), to: (u32, u32), is_drop: bool) -> GridMoveElement {
    let nonzero = |value: u32| (value != 0).then_some(value);
    GridMoveElement {
        from_row: nonzero(from.0),
        from_col: nonzero(from.1),
        to_row: nonzero(to.0),
        to_col: nonzero(to.1),
        is_drop: is_drop.then_some(true),
    }
}

/// A Control drop makes room the way its drag previews point (`0x5beca8` marks the new
/// column of each block of the row; previews to the left are the right half of the target,
/// `0x5c0ee0`), and its echo is the stock move list of that drop (`0x59e194`).
#[test]
fn grid_move_makes_room_the_way_its_previews_point() {
    let (mut state, mut control) = loaded::session();
    row_one_blocks(&mut control, &mut state, &[(4, 1), (5, 1), (7, 2)]);
    state.now = at(1000);
    let moves = vec![
        grid_move((1, 4), (1, 3), false),
        grid_move((1, 5), (1, 4), false),
        grid_move((1, 7), (1, 5), true),
    ];
    let drag = GridMoveMessage {
        moves: moves.clone(),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, drag, at(1000));
    assert!(sent.is_empty());
    assert_eq!(loaded::commit(&mut state, effects), 1);
    let hashes = row_one_hashes(&state);
    assert_eq!(&hashes[3..8], [Some(1), Some(1), Some(2), None, None]);
    let done = loaded::done(&mut control, &mut state, at(1000));
    let echo = done.iter().find_map(|(message, _)| match message {
        ControlMessage::GridMove(echo) => Some(&echo.moves),
        _ => None,
    });
    assert_eq!(echo, Some(&moves));
}

/// Item 2 (r53): a drop on the QC touch screen sends the GridMove echo too, as
/// `Chain::dropSlot` (`0x59f620`) does for every drop: after the frame lists, the blocks it
/// shifted (moving right: the far one first, `0x59e194`), then the drop. No request: no ID.
#[test]
fn a_touch_drop_echoes_its_moves_to_control() {
    use tetrad_app::device::actions::{self, Action};
    use tetrad_engine::{Column, DropSide, Row, Slot};
    let (mut state, mut control) = loaded::session();
    row_one_blocks(&mut control, &mut state, &[(2, 2), (5, 1), (6, 1)]);
    loaded::sync(&mut control, &mut state, at(0));
    state.now = at(1000);
    let slot = |column| Slot {
        row: Row::new(1).unwrap(),
        column: Column::new(column).unwrap(),
    };
    let drop = Action::DropBlock {
        from: slot(2),
        to: slot(5),
        side: DropSide::Left,
    };
    let action = Action::Undoable(UndoPath::Direct, Box::new(drop));
    control.record_drop(&action, &state);
    let effects = actions::reduce(action, &mut state).unwrap();
    assert_eq!(loaded::commit(&mut state, effects), 1);
    assert_eq!(
        &row_one_hashes(&state)[2..8],
        [None, None, None, Some(2), Some(1), Some(1)]
    );
    let sent = loaded::sync(&mut control, &mut state, at(1000));
    let kinds: Vec<_> = (sent.iter())
        .filter_map(|message| match message {
            ControlMessage::UndoRedo(_) => Some("UndoRedo"),
            ControlMessage::GridMove(_) => Some("GridMove"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["UndoRedo", "GridMove"], "{sent:?}");
    let echo = GridMoveMessage {
        moves: vec![
            grid_move((1, 6), (1, 7), false),
            grid_move((1, 5), (1, 6), false),
            grid_move((1, 2), (1, 5), true),
        ],
        ..Default::default()
    };
    assert!(sent.contains(&ControlMessage::GridMove(echo)), "{sent:?}");
    // The echo goes once.
    let again = loaded::sync(&mut control, &mut state, at(1100));
    assert!(
        again
            .iter()
            .all(|m| !matches!(m, ControlMessage::GridMove(_)))
    );
}

/// Resync `0x29a4f8` starts with CloudProduct (46) `0xf5470`: DELETE with an empty transfer
/// state, then UPDATE with the map `0x7db468` in key order: here the upload states by local key
/// (3 NotUploaded, 4 Uploading, 5 Uploaded) and each download in flight (state 1,
/// Downloading); a CloudProduct READ of the transfer state answers the same two. tetrad-app
/// answers no RecentsFavorites READ, so family 20 is skipped.
#[test]
fn resync_starts_with_the_cloud_product_reset_and_the_downloads_in_flight() {
    use tetrad_app::device::services::UploadState;
    use tetrad_control::codecs::{
        CloudProductMessage, CloudTransferItemState, CloudTransferState, CloudTransferStateItem,
    };
    let (mut state, mut control) = loaded::session();
    control.set_downloads_in_flight(&["capture-1".to_owned()]);
    control.set_upload_states(vec![
        ("cap-3".into(), UploadState::NotUploaded),
        ("/p/Lead.pb".into(), UploadState::Uploaded),
        ("cap-2".into(), UploadState::Uploading),
    ]);
    let effects = control.resync(false, &mut state, at(0)).unwrap();
    let mut reassembler = Reassembler::new(Limits::default());
    let envelopes: Vec<_> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::ControlReport(report) => reassembler.push(&report[..]).unwrap(),
            _ => None,
        })
        .collect();
    let transfers = |action: MessageAction, items| {
        ControlMessage::CloudProduct(CloudProductMessage {
            action: Some(action.into()),
            cloud_transfer_state: Some(CloudTransferState { items }),
            ..Default::default()
        })
    };
    let item = |id: &str, state: CloudTransferItemState| CloudTransferStateItem {
        id: Some(id.into()),
        state: Some(state.into()),
    };
    let downloading = vec![
        item("/p/Lead.pb", CloudTransferItemState::Uploaded),
        item("cap-2", CloudTransferItemState::Uploading),
        item("cap-3", CloudTransferItemState::NotUploaded),
        item("capture-1", CloudTransferItemState::Downloading),
    ];
    let expected = [
        transfers(MessageAction::Delete, Vec::new()),
        transfers(MessageAction::Update, downloading.clone()),
    ];
    let first: Vec<_> = (envelopes[..2].iter())
        .map(|envelope| {
            ControlMessage::decode(envelope, CodecLimits::default())
                .unwrap()
                .message
        })
        .collect();
    assert_eq!(first, expected);
    let recents = CortexMessageType::RecentsFavorites as u32;
    assert!(envelopes.iter().all(|e| e.message_type != recents));
    let read = CloudProductMessage {
        action: Some(MessageAction::Read.into()),
        cloud_transfer_state: Some(CloudTransferState::default()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read, at(10));
    let actions: Vec<_> = (sent.iter())
        .map(|message| match message {
            ControlMessage::CloudProduct(reply) => {
                let items = reply.cloud_transfer_state.clone().unwrap().items;
                (reply.action, items)
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        actions,
        [
            (Some(MessageAction::Delete.into()), Vec::new()),
            (Some(MessageAction::Update.into()), downloading)
        ]
    );
}

/// CloudProduct (46) DOWNLOAD (`0xf3f24`) of one capture key with no `to_folder` hands the
/// download to Services; a `to_folder` and a preset without `to_folder` are refused with no
/// request. More keys: `r47::control_46_multi_key_capture_and_ir_download_requests_each_key`.
#[test]
fn cloud_product_download_of_one_capture_goes_to_services() {
    use tetrad_app::device::control::AccountRequest;
    use tetrad_control::codecs::CloudProductMessage;
    use tetrad_services::cloud::services::ProductKind;
    let (mut state, mut control) = loaded::session();
    let download = |keys: &[&str], product_type, to_folder: Option<&str>| CloudProductMessage {
        action: Some(MessageAction::Download.into()),
        product_keys: keys.iter().map(|key| (*key).to_owned()).collect(),
        product_type: Some(product_type),
        to_folder: to_folder.map(str::to_owned),
        ..Default::default()
    };
    let (_, effects) = loaded::host(&mut control, &mut state, download(&["c1"], 2, None), at(10));
    let expected = AccountRequest::CloudProductDownload {
        kind: ProductKind::NeuralCapture,
        id: "c1".into(),
    };
    assert!(effects.contains(&Effect::ControlAccount(expected)));
    for (at_ms, refused) in [
        (20, download(&["c1"], 2, Some("folder"))),
        (40, download(&["p1"], 0, None)),
    ] {
        let (_, effects) = loaded::host(&mut control, &mut state, refused, at(at_ms));
        let account = |effect: &Effect| matches!(effect, Effect::ControlAccount(_));
        assert!(!effects.iter().any(account));
    }
}

/// The account requests among `effects`.
fn account_requests(effects: &[Effect]) -> Vec<tetrad_app::device::control::AccountRequest> {
    (effects.iter())
        .filter_map(|effect| match effect {
            Effect::ControlAccount(request) => Some(request.clone()),
            _ => None,
        })
        .collect()
}

/// A session whose library has the user setlist "Gig" (key `/media/p4/Presets/Gig`) with the
/// preset "Lead" in slot 1; the caller removes the returned root.
fn gig_session(
    tag: &str,
) -> (
    std::path::PathBuf,
    tetrad_app::device::State,
    Control,
    String,
) {
    use tetrad_app::device::Library;
    use tetrad_library::sidecar::Sidecar;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets/Gig")).unwrap();
    let (mut state, control) = loaded::session();
    let mut presets = StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap();
    let mut lead = state.current.as_ref().unwrap().preset.clone();
    lead.name = Some("Lead".into());
    let document =
        tetrad_preset::PresetDocument::parse(&lead.to_bytes(), tetrad_preset::Limits::default())
            .unwrap();
    let _ = (presets.store("Gig", 1, "Lead", &document, &Sidecar::default())).unwrap();
    let path = presets.slots("Gig").unwrap()[1].clone().unwrap();
    state.library = Some(Library {
        presets,
        database: None,
    });
    (root, state, control, path)
}

/// CloudProduct (46) DELETE (`0xf330c`) removes a product from the user's cloud account with
/// no device confirmation: one key gives exactly one request for exactly that id and type
/// (`0x765780`). No key, no `product_type`, an empty key, the folder key `"folder"`
/// (`0x1d916c`, review r32 E1) and an unknown type give nothing.
#[test]
fn control_46_delete_queues_one_account_request_per_message() {
    use tetrad_app::device::control::AccountRequest;
    use tetrad_control::codecs::CloudProductMessage;
    use tetrad_services::cloud::services::ProductKind;
    let (mut state, mut control) = loaded::session();
    let delete = |keys: &[&str], product_type: Option<i32>| CloudProductMessage {
        action: Some(MessageAction::Delete.into()),
        product_keys: keys.iter().map(|key| (*key).to_owned()).collect(),
        product_type,
        ..Default::default()
    };
    for (at_ms, product_type, kind) in [
        (10, 0, ProductKind::Preset),
        (20, 1, ProductKind::ImpulseResponse),
        (30, 2, ProductKind::NeuralCapture),
    ] {
        let message = delete(&["cloud-7"], Some(product_type));
        let (sent, effects) = loaded::host(&mut control, &mut state, message, at(at_ms));
        assert!(sent.is_empty());
        let ids = vec!["cloud-7".into()];
        let expected = [AccountRequest::CloudProductDelete { kind, ids }];
        assert_eq!(account_requests(&effects), expected);
    }
    for (at_ms, malformed) in [
        (40, delete(&[], Some(2))),
        (50, delete(&["cloud-7"], None)),
        (60, delete(&[""], Some(2))),
        (70, delete(&["cloud-7"], Some(3))),
        (85, delete(&["folder"], Some(0))),
    ] {
        let (_, effects) = loaded::host(&mut control, &mut state, malformed, at(at_ms));
        assert!(account_requests(&effects).is_empty(), "{effects:?}");
    }
    // Bring-up: refused without --persist, and with --persist but without --services.
    for (at_ms, persist) in [(90, false), (100, true)] {
        control.set_run_options(&loaded::bringup(persist));
        let message = delete(&["cloud-7"], Some(2));
        let (_, effects) = loaded::host(&mut control, &mut state, message, at(at_ms));
        assert!(account_requests(&effects).is_empty(), "{effects:?}");
    }
}

/// Re-r34 §6: CloudProduct (46) DELETE of more keys (the Multi Select worker `0x785b74`; no
/// question, a failure goes on) gives one request with exactly the given ids, in the given
/// order (one Services run); while Multi Select is on the worker returns with nothing
/// (`0x785b74` with fromControl 1, C). O: when one key is not a plain product id (empty, `"folder"`, `.`, `..`,
/// a `/`, a repeat), nothing is sent; bring-up refuses the run without `--persist` and without
/// `--services`.
#[test]
fn control_46_delete_of_many_keys_sends_exactly_those_keys() {
    use tetrad_app::device::control::AccountRequest;
    use tetrad_control::codecs::CloudProductMessage;
    use tetrad_services::cloud::services::ProductKind;
    let (mut state, mut control) = loaded::session();
    let delete = |keys: &[&str]| CloudProductMessage {
        action: Some(MessageAction::Delete.into()),
        product_keys: keys.iter().map(|key| (*key).to_owned()).collect(),
        product_type: Some(2),
        ..Default::default()
    };
    let keys = ["cloud-9", "cloud-7", "cloud-8"];
    let (sent, effects) = loaded::host(&mut control, &mut state, delete(&keys), at(10));
    assert!(sent.is_empty());
    let expected = [AccountRequest::CloudProductDelete {
        kind: ProductKind::NeuralCapture,
        ids: keys.map(String::from).into(),
    }];
    assert_eq!(account_requests(&effects), expected);
    control.set_multi_select(true);
    let (_, effects) = loaded::host(&mut control, &mut state, delete(&keys), at(15));
    assert!(account_requests(&effects).is_empty(), "{effects:?}");
    control.set_multi_select(false);
    for (at_ms, malformed) in [
        (20, delete(&["cloud-7", ""])),
        (30, delete(&["cloud-7", "folder"])),
        (40, delete(&["cloud-7", ".."])),
        (50, delete(&[".", "cloud-8"])),
        (60, delete(&["cloud-7", "user/cloud-8"])),
        (70, delete(&["cloud-7", "cloud-8", "cloud-7"])),
    ] {
        let (_, effects) = loaded::host(&mut control, &mut state, malformed, at(at_ms));
        assert!(account_requests(&effects).is_empty(), "{effects:?}");
    }
    for (at_ms, persist) in [(80, false), (90, true)] {
        control.set_run_options(&loaded::bringup(persist));
        let message = delete(&["cloud-7", "cloud-8"]);
        let (_, effects) = loaded::host(&mut control, &mut state, message, at(at_ms));
        assert!(account_requests(&effects).is_empty(), "{effects:?}");
    }
}

/// CloudProduct (46) UPLOAD (`0xf3060`) of a preset key from its `from_folder` setlist starts
/// the Directory upload of that preset file (`0x1e8a70` → `0x1d81d0`); an IR upload does
/// nothing (stock). A key that is not in the folder, a non-setlist folder and no
/// `from_folder` give no request (a capture: `control_46_upload_of_one_capture_…`).
#[test]
fn control_46_upload_of_a_setlist_preset_starts_the_directory_upload() {
    use tetrad_app::device::control::AccountRequest;
    use tetrad_control::codecs::CloudProductMessage;
    let (root, mut state, mut control, path) = gig_session("cloud-upload");
    let gig = "/media/p4/Presets/Gig";
    let upload = |key: &str, product_type, from: Option<&str>| CloudProductMessage {
        action: Some(MessageAction::Upload.into()),
        product_keys: vec![key.to_owned()],
        product_type: Some(product_type),
        from_folder: from.map(str::to_owned),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(
        &mut control,
        &mut state,
        upload(&path, 0, Some(gig)),
        at(10),
    );
    assert!(sent.is_empty());
    let expected = [AccountRequest::CloudProductUpload { path: path.clone() }];
    assert_eq!(account_requests(&effects), expected);
    let (sent, effects) = loaded::host(&mut control, &mut state, upload("ir-1", 1, None), at(20));
    assert!(sent.is_empty() && effects.iter().all(|e| !matches!(e, Effect::Notice(..))));
    for (at_ms, refused) in [
        (30, upload("/media/p4/Presets/Gig/Other.pb", 0, Some(gig))),
        (40, upload(&path, 0, Some("local_ir_root"))),
        (50, upload(&path, 0, None)),
    ] {
        let (_, effects) = loaded::host(&mut control, &mut state, refused, at(at_ms));
        assert!(account_requests(&effects).is_empty(), "{effects:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// CloudProduct (46) UPLOAD type 2 (`0xf3060` → `0x765520`) of one user capture key starts
/// the Directory capture upload (`AccountRequest::CloudCaptureUpload`), with or without
/// `from_folder`. More keys start the batch (`0x785dcc`, re-r38 §2) in key order, but not when
/// signed out or while Directory Multi Select runs (C: no text). Bring-up refuses it without
/// `--persist` and without `--services`.
#[test]
fn control_46_upload_of_one_capture_starts_the_directory_capture_upload() {
    use tetrad_app::device::control::AccountRequest;
    use tetrad_control::codecs::CloudProductMessage;
    let (mut state, mut control) = loaded::session();
    let upload = |keys: &[&str], from: Option<&str>| CloudProductMessage {
        action: Some(MessageAction::Upload.into()),
        product_keys: keys.iter().map(|key| (*key).to_owned()).collect(),
        product_type: Some(2),
        from_folder: from.map(str::to_owned),
        ..Default::default()
    };
    let expected = [AccountRequest::CloudCaptureUpload {
        key: "capture-1".into(),
    }];
    for (at_ms, from) in [(10, None), (20, Some("anything"))] {
        let message = upload(&["capture-1"], from);
        let (sent, effects) = loaded::host(&mut control, &mut state, message, at(at_ms));
        assert!(sent.is_empty());
        assert_eq!(account_requests(&effects), expected);
    }
    let batch = || upload(&["capture-2", "capture-1"], None);
    let (_, effects) = loaded::host(&mut control, &mut state, batch(), at(30));
    assert!(
        account_requests(&effects).is_empty(),
        "signed out: {effects:?}"
    );
    state
        .settings
        .update(|settings| settings.account.username = "fake-user".into());
    control.set_multi_select(true);
    let (_, effects) = loaded::host(&mut control, &mut state, batch(), at(31));
    assert!(
        account_requests(&effects).is_empty(),
        "Multi Select: {effects:?}"
    );
    control.set_multi_select(false);
    let (_, effects) = loaded::host(&mut control, &mut state, batch(), at(32));
    let items = ["capture-2", "capture-1"].map(|key| tetrad_ui::UploadItem::Capture(key.into()));
    let expected = [AccountRequest::CloudBatchUpload {
        items: items.to_vec(),
    }];
    assert_eq!(account_requests(&effects), expected);
    for (at_ms, persist) in [(40, false), (50, true)] {
        control.set_run_options(&loaded::bringup(persist));
        let message = upload(&["capture-1"], None);
        let (_, effects) = loaded::host(&mut control, &mut state, message, at(at_ms));
        assert!(account_requests(&effects).is_empty(), "{effects:?}");
    }
}

/// CloudProduct (46) DOWNLOAD (`0xf3f24`) of one preset with `to_folder` and `to_indices`
/// stores it into that slot of the setlist (`0x1e8a70` → `0x1dd61c`). Without `to_indices`,
/// into a non-setlist folder or into a negative slot: no request. More keys:
/// `r47::control_46_multi_key_preset_download_fills_setlist_slots`.
#[test]
fn control_46_preset_download_goes_into_the_setlist_slot() {
    use tetrad_app::device::control::AccountRequest;
    use tetrad_control::codecs::CloudProductMessage;
    let (root, mut state, mut control, _) = gig_session("cloud-preset");
    let gig = "/media/p4/Presets/Gig";
    let download = |keys: &[&str], to: &str, indices: Vec<i32>| CloudProductMessage {
        action: Some(MessageAction::Download.into()),
        product_keys: keys.iter().map(|key| (*key).to_owned()).collect(),
        product_type: Some(0),
        to_folder: Some(to.to_owned()),
        to_indices: indices,
        ..Default::default()
    };
    let message = download(&["cloud-p"], gig, vec![4]);
    let (sent, effects) = loaded::host(&mut control, &mut state, message, at(10));
    assert!(sent.is_empty());
    let expected = AccountRequest::CloudPresetDownload {
        setlist: "Gig".into(),
        items: vec![("cloud-p".into(), 4)],
    };
    assert_eq!(account_requests(&effects), [expected]);
    for (at_ms, refused) in [
        (20, download(&["cloud-p"], gig, Vec::new())),
        (30, download(&["cloud-p"], "local_ir_root", vec![4])),
        (40, download(&["cloud-p"], gig, vec![-1])),
    ] {
        let (_, effects) = loaded::host(&mut control, &mut state, refused, at(at_ms));
        assert!(account_requests(&effects).is_empty(), "{effects:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// File (4) UPDATE of a capture (`0x24eecc` → `0x241be4`) writes `instrument` (`0x4000`) and
/// `device` (`0x8000`) with the name; an absent field keeps its stored value (`None`; an
/// absent name is read from the record). A name-only UPDATE stays a rename; a code outside
/// the stock lists is refused.
#[test]
fn file_update_writes_capture_instrument_and_device() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::stock::{CAPTURE_ROOT_KEY, StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, ProductType, StockDb, StockProduct};
    use tetrad_ui::{CaptureDevice, CaptureInstrument, DirectoryRequest, MediaKind};
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-nc-edit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    let capture = StockProduct {
        id: "NC_1".into(),
        product_type: ProductType::Capture,
        read_only: false,
        raw_data: Some(vec![1]),
        metadata: Some(br#"{"name":"Plexi","deviceType":"cab"}"#.to_vec()),
    };
    (database.transaction(|write| {
        write.boot_maintenance()?;
        write.put_product(&capture)
    }))
    .unwrap();
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(database),
    });
    let mut file_update = |item: ProductData| {
        let message = FileMessage {
            action: update(),
            file_type: Some(2),
            folder: Some(FolderInfo {
                key: Some(CAPTURE_ROOT_KEY.into()),
                files: vec![ProductData {
                    key: Some("NC_1".into()),
                    ..item
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        loaded::host(&mut control, &mut state, message, at(10)).1
    };
    let edit =
        |name: &str, device: Option<CaptureDevice>, instrument: Option<CaptureInstrument>| {
            vec![Effect::Directory(DirectoryRequest::EditCapture {
                id: "NC_1".into(),
                name: name.into(),
                device,
                instrument,
            })]
        };
    let both = ProductData {
        name: Some("Plexi 2".into()),
        instrument: Some(1),
        device: Some(1),
        ..Default::default()
    };
    let (amp, guitar) = (Some(CaptureDevice::Amp), Some(CaptureInstrument::Guitar));
    assert_eq!(file_update(both), edit("Plexi 2", amp, guitar));
    let instrument = ProductData {
        instrument: Some(3),
        ..Default::default()
    };
    let synth = Some(CaptureInstrument::Synth);
    assert_eq!(file_update(instrument), edit("Plexi", None, synth));
    let name = ProductData {
        name: Some("Plexi 3".into()),
        ..Default::default()
    };
    let rename = DirectoryRequest::RenameMedia {
        kind: MediaKind::Capture,
        id: "NC_1".into(),
        name: "Plexi 3".into(),
    };
    assert_eq!(file_update(name), [Effect::Directory(rename)]);
    let unknown = ProductData {
        device: Some(9),
        ..Default::default()
    };
    let effects = file_update(unknown);
    assert!(
        !effects.iter().any(|e| matches!(e, Effect::Directory(_))),
        "{effects:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Resync `0x29a4f8` over a synthetic library sends DefaultParameters (19: DELETE, then the
/// capture entry `0x15d890`) and ModelPreset (71: DELETE, then one UPDATE with every
/// `0x1f6280` record) instead of the unsupported notices.
#[test]
fn resync_sends_default_parameters_and_model_presets_from_the_library() {
    use tetrad_app::device::{Library, State};
    use tetrad_catalog::{Catalog, Specs};
    use tetrad_control::codecs::ParamValue;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_media::factory::DeviceCipher;
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, StockDb};
    const MODELS: &str = concat!(
        "<Models><Category id='1' name='Drive'><Model id='1' name='Light' blob='light'>",
        "<Parameter name='GAIN' type='float' min='0' max='10' defaultValue='5'/></Model>",
        "<Model id='4004' name='Out EQ' hidden='true'>",
        "<Parameter name='GAIN' type='float' min='0' max='1' defaultValue='0'/></Model>",
        "<Model id='31000' name='IO' hidden='true'>",
        "<Parameter name='GAIN' type='float' min='0' max='1' defaultValue='0'/></Model>",
        "</Category><Category id='14' name='Capture'><Model id='14000' name='Capture'>",
        "<Parameter name='A' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='B' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='C' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='D' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='E' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='file_name' type='string' defaultValue=''/></Model></Category>",
        "</Models>",
    );
    const USER: &str = "
CREATE TABLE default_param (model_id TEXT,param_id INT,value TEXT,exp INT,exp_trim_max REAL,exp_trim_min REAL,PRIMARY KEY (model_id, param_id));
CREATE TABLE default_exp_switch (model_id TEXT,exp_switch_type INT,exp_switch_inverted INT,exp_switch_latch_emulation INT,exp_switch_delay_ms INT,PRIMARY KEY (model_id));
CREATE TABLE product ( id TEXT, type INT, readonly INT, rawdata BLOB, metadata BLOB, PRIMARY KEY(id));
CREATE TABLE downloads_product_preset ( id TEXT, rawdata BLOB, metadata BLOB, PRIMARY KEY ( id ) );
CREATE TABLE downloads_product_nc ( id TEXT, hash TEXT, rawdata BLOB, metadata BLOB, PRIMARY KEY ( id ) );
CREATE TABLE folder ( id INTEGER PRIMARY KEY AUTOINCREMENT, pdType INT, folderType INT, name TEXT, parent INTEGER, readonly INT);
CREATE TABLE folder_content ( folderId INTEGER, productId TEXT, PRIMARY KEY(folderId,productId));
CREATE TABLE nc_encryption ( nc_type TEXT, encryption INT, PRIMARY KEY ( nc_type ) );
CREATE TABLE pinned_models (hash TEXT);
CREATE TABLE model_presets ( id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, hash INT, expSwitchType INT, expSwitchInverted INT, expSwitchLatchEmulation INT, expSwitchDelayMs INT);
CREATE TABLE default_model_presets ( hash INT PRIMARY KEY, presetId INTEGER, isFactory INTEGER );
CREATE TABLE model_preset_parameters (presetId INT, paramIndex INT, value TEXT, exp INT, expTrimMax REAL, expTrimMin REAL, PRIMARY KEY (presetId, paramIndex));
CREATE TABLE schema_version ( major INTEGER, minor INTEGER);
INSERT INTO schema_version VALUES (410, 1);
INSERT INTO model_presets VALUES (1, 'Mine', 1, 2, 0, 0, 600);
";
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-resync-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let path = root.join("zenui.db");
    let key = format!("{:064x}", 0xbeef);
    let cipher = DeviceCipher::caller_secret(b"invented test secret");
    let mut metadata = br#"{"name":"Crunch","version":"1"}"#.to_vec();
    cipher.apply(&mut metadata);
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch(USER).unwrap();
    connection
        .execute(
            "INSERT INTO product VALUES (?1, 2, 0, NULL, ?2)",
            rusqlite::params![key, metadata],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO default_param VALUES (?1, 0, '0.5', 0, 1, 0)",
            [&key],
        )
        .unwrap();
    drop(connection);

    let (settings, _) = tetrad_settings::device::DeviceSettings::load(
        &loaded::NoFiles,
        tetrad_settings::device::Hardware::QuadCortex,
    )
    .unwrap();
    let mut state = State::new(
        tetrad_hal::platform::Product::QuadCortex,
        tetrad_hal::platform::Platform::QcOriginal,
        settings,
    );
    state.specs =
        Some(Specs::resolve(&Catalog::parse(MODELS, "<ModuleReport/>").unwrap()).unwrap());
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(StockDb::open(&path, DbLimits::default(), true).unwrap()),
    });
    let mut control = loaded::connect(&mut state);
    control.set_media_cipher(cipher);
    let (sent, other) = replies(control.resync(false, &mut state, at(0)).unwrap());

    let delete = Some(MessageAction::Delete.into());
    let defaults: Vec<_> = sent
        .iter()
        .filter_map(|(message, _)| match message {
            ControlMessage::DefaultParameters(message) => Some(message),
            _ => None,
        })
        .collect();
    let [cleared, list] = defaults[..] else {
        panic!("expected DELETE and the list, got {defaults:?}");
    };
    assert_eq!(cleared.action, delete);
    let [entry] = &list.all_defaults[..] else {
        panic!("expected one capture entry, got {list:?}");
    };
    assert_eq!(entry.id.as_deref(), Some(key.as_str()));
    let block = entry.model.as_ref().unwrap();
    assert_eq!(block.hash, Some(14_000));
    let name = ParamValue::string(format!("{key}Crunch"));
    assert_eq!(block.params[5].param_values, [name]);

    let presets: Vec<_> = sent
        .iter()
        .filter_map(|(message, _)| match message {
            ControlMessage::ModelPreset(message) => Some(message),
            _ => None,
        })
        .collect();
    let [cleared, listed] = presets[..] else {
        panic!("expected DELETE and UPDATE, got {presets:?}");
    };
    assert_eq!(cleared.action, delete);
    assert_eq!(listed.action, update());
    let records: Vec<_> = listed
        .presets
        .iter()
        .map(|entry| {
            let id = entry.id.as_ref().unwrap();
            (
                id.value.clone().unwrap(),
                id.hash.unwrap(),
                entry.model.is_some(),
            )
        })
        .collect();
    let special = || "SpecialFactoryModelPresetID".to_owned();
    assert_eq!(
        records,
        [
            ("1".to_owned(), 1, false),
            (special(), 1, true),
            (special(), 14_000, true),
            (special(), 4004, true),
            (special(), 31_000, true),
        ]
    );
    let notices: Vec<_> = other
        .iter()
        .filter_map(|effect| match effect {
            Effect::Notice(_, text) => Some(text),
            _ => None,
        })
        .collect();
    assert!(
        notices
            .iter()
            .all(|text| !text.contains("DefaultParameters") && !text.contains("ModelPreset")),
        "{notices:?}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// On the simulated device: a RecallPreset whose redeploy the DSP refuses (the run-list
/// clear stays unread, shared sim `hold`) changes nothing; the reply is the live preset,
/// last, with the request ID (`0x274220` replies after `0x5b22a8`), and no undo frame
/// stays.
#[test]
fn failed_recall_deploy_replies_the_live_preset_and_keeps_no_frame() {
    use common::{Media, STOCK, light_preset};
    use tetrad_app::device::Event;
    use tetrad_dsp_protocol::hid::{HEADER_BYTES, REPORT_BYTES};
    let (mut device, log, _media) =
        common::boot(Media::new([("Light", light_preset(&STOCK.specs(), 2))]));
    // Idle steps let the scripted DSP read the reports (it serves its rings on sleeps).
    let host = |device: &mut tetrad_app::device::Device<common::Sim>, message: ControlMessage| {
        for report in send(message) {
            device.push(Event::Control(Box::new(report)));
            device.step();
        }
        (0..50).for_each(|_| _ = device.step());
    };
    host(
        &mut device,
        KeepAliveMessage {
            action: update(),
            ..Default::default()
        }
        .into(),
    );
    let read = Some(MessageAction::Read.into());
    host(
        &mut device,
        ModelRepoMessage {
            action: read,
            ..Default::default()
        }
        .into(),
    );
    let connected = ConnectionMessage {
        connected: Some(true),
        ..Default::default()
    };
    host(&mut device, connected.into());
    assert!(device.bridges().control.connected());

    let live = device.state().current.as_ref().unwrap().preset.to_bytes();
    let recall = RecallPresetMessage {
        action: update(),
        request_id: Some(9),
        preset: Some(RawMessageBytes {
            bytes: light_preset(&STOCK.specs(), 5).to_bytes(),
        }),
        ..Default::default()
    };
    let replies_from = |mark: usize| -> Vec<ControlMessage> {
        let mut reassembler = Reassembler::new(Limits::default());
        let log = log.borrow();
        let reports = log.requests[mark..]
            .iter()
            .filter(|sent| sent.kind() == 0x27 && sent.frame[2] == 0)
            .map(|sent| sent.frame[HEADER_BYTES..HEADER_BYTES + REPORT_BYTES].to_vec());
        reports
            .filter_map(|report| reassembler.push(&report).unwrap())
            .map(|envelope| {
                let limits = tetrad_control::link::LinkLimits::default().payload;
                ControlMessage::decode(&envelope, limits).unwrap().message
            })
            .collect()
    };
    let last_preset = |sent: &[ControlMessage]| match sent.last() {
        Some(ControlMessage::RecallPreset(reply)) => {
            (reply.preset.clone().unwrap().bytes, reply.request_id)
        }
        other => panic!("{other:?}"),
    };

    device
        .platform()
        .clock
        .hold
        .set(Some(|_, frame| frame[0] == 0x1e));
    let mark = log.borrow().requests.len();
    host(&mut device, recall.into());
    device.platform().clock.hold.set(None);
    let sent = replies_from(mark);
    let current = device.state().current.as_ref().unwrap();
    assert_eq!(last_preset(&sent), (working_bytes(current), Some(9)));
    assert_eq!(
        device.state().current.as_ref().unwrap().preset.to_bytes(),
        live
    );
    assert_eq!(device.state().history.depths(), (0, 0));
}

/// Review C2 (`0x527d80`, scene recall `0x527248`): while a capture preset runs, an undo with
/// a layout difference, a scene select and a value edit (review r11 R4) change only `State`,
/// silently; the DSP keeps the capture preset (owned, as a preset load does, `0x5122c0`).
/// The revert plays the user preset.
#[test]
fn undo_and_scene_select_during_capture_keep_the_capture_preset() {
    use common::{Media, STOCK, light_preset};
    use tetrad_app::device::actions::LayoutEdit;
    use tetrad_app::device::capture::{Swap, run_entry};
    use tetrad_app::device::{Action, Event};
    use tetrad_capture::entry::{EntryAction, Hardware};
    use tetrad_engine::{Column, Row, SceneIndex, Slot};
    let (mut device, log, _media) =
        common::boot(Media::new([("Light", light_preset(&STOCK.specs(), 2))]));
    while !device.state().splash_dismissed {
        assert!(device.step().is_none());
    }
    let drive = |device: &mut tetrad_app::device::Device<common::Sim>, action| {
        device.push(Event::Action(action));
        assert!(device.step().is_none());
        assert!(device.step().is_none());
    };
    let slot = Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    };
    let block = |device: &tetrad_app::device::Device<common::Sim>| {
        let current = device.state().current.as_ref().unwrap();
        current
            .deployment
            .layout()
            .instance(tetrad_engine::Site::Slot(slot))
            .is_some()
    };
    drive(&mut device, Action::RecordUndo(UndoPath::Direct));
    drive(&mut device, Action::EditLayout(LayoutEdit::Remove(slot)));
    assert!(!block(&device));

    run_entry(
        &[EntryAction::LoadEmpty],
        Hardware::QuadCortex,
        &mut Swap::default(),
        &mut device,
    )
    .unwrap();
    assert!(device.bridges().capture.capture_running());
    assert!(device.step().is_none());

    let before = log.borrow().requests.len();
    drive(&mut device, Action::Undo);
    drive(
        &mut device,
        Action::SelectScene(SceneIndex::new(2).unwrap()),
    );
    let notices = log.borrow().notices.len();
    drive(
        &mut device,
        Action::SetParameter {
            site: tetrad_engine::Site::Slot(slot),
            index: 0,
            value: 0.9,
        },
    );
    assert_eq!(log.borrow().kinds(before), [0u8; 0], "no DSP push");
    assert_eq!(log.borrow().notices.len(), notices, "no capture notice");
    assert!(block(&device));
    let current = device.state().current.as_ref().unwrap();
    assert_eq!(current.deployment.scene().index(), 2);
    assert_eq!(device.state().history.depths(), (0, 1));
}

// --- R14 control: handshake key exchange, settings, meters, library, account ---------------

/// The device reports as host envelopes: transformed payloads decrypted with `cipher`,
/// compressed ones inflated, then decoded. The flag says whether the payload was transformed.
fn sealed_replies(
    effects: &[Effect],
    cipher: &tetrad_app::device::control::CommsCipher,
) -> Vec<(ControlMessage, bool)> {
    use tetrad_control::pipeline::Transform;
    let mut reassembler = Reassembler::new(Limits::default());
    let mut messages = Vec::new();
    for effect in effects {
        let Effect::ControlReport(report) = effect else {
            continue;
        };
        let Some(mut envelope) = reassembler.push(&report[..]).unwrap() else {
            continue;
        };
        // Scene (13) has its own codec (`tetrad_control::scene`).
        if envelope.message_type == CortexMessageType::Scene as u32 {
            continue;
        }
        let sealed = envelope.transform != 0;
        if sealed {
            envelope.payload = cipher.decode(&envelope.payload).unwrap();
            envelope.transform = 0;
        }
        let limits = tetrad_control::link::LinkLimits::default().payload;
        let decoded = ControlMessage::decode(&envelope, limits).unwrap();
        messages.push((decoded.message, sealed));
    }
    messages
}

/// Version READ `0x2a59d0` → `0x2a4dd4`: DSP request `0x42` with the serial, no reply yet.
/// The DSP answer (`0x17`, `0x2a532c`) sends the Version reply with the XXTEA `comms_version`
/// (key cut at its first NUL) and the serial, without request ID, then a Version READ, and
/// installs the AES-CTR transform from vector B: the factory resync's CloudLogin DELETE (18,
/// a transform type) then goes out sealed. An answer with no waiting READ does nothing.
#[test]
fn version_read_runs_the_dsp_key_exchange_and_installs_the_transform() {
    use tetrad_app::device::DspCommand;
    use tetrad_app::device::control::CommsCipher;
    use tetrad_control::codecs::{CloudLoginMessage, VersionMessage};
    use tetrad_dsp_protocol::requests::Request;
    use tetrad_dsp_protocol::responses::UsbMeter;
    use tetrad_media::factory::DeviceSerial;

    let read = || VersionMessage {
        action: Some(MessageAction::Read.into()),
        request_id: Some(9),
        ..Default::default()
    };
    // Fake vectors and serial; the bytes after the first NUL of vector A do not count.
    let key = |tail: u8| UsbMeter([1, 2, 3, 0, tail, tail, tail, tail, 9, 9, 9, 9, 9, 9, 9, 9]);
    let material = || UsbMeter(*b"fake vector B 16");
    let replies_for = |tail: u8| {
        let (mut state, mut control) = loaded::session();
        control.set_device_serial(DeviceSerial::parse(b"TEST12345").unwrap());
        let (sent, effects) = loaded::host(&mut control, &mut state, read(), at(10));
        assert!(sent.is_empty());
        let request = effects.iter().find_map(|effect| match effect {
            Effect::Dsp(DspCommand::Request(Request::Text42(text))) => Some(text.as_bytes()),
            _ => None,
        });
        assert_eq!(request, Some(&b"TEST12345"[..]));
        let effects = control
            .complete_key_exchange(&key(tail), &material(), &mut state, at(20))
            .unwrap();
        let cipher = CommsCipher::derive(&material().0);
        let messages = sealed_replies(&effects, &cipher);
        let again = control
            .complete_key_exchange(&key(tail), &material(), &mut state, at(30))
            .unwrap();
        assert!(sealed_replies(&again, &cipher).is_empty());
        let resync = control.factory_resync(&mut state, at(40)).unwrap();
        let logout = sealed_replies(&resync, &cipher).pop().unwrap();
        assert_eq!(
            logout,
            (
                ControlMessage::CloudLogin(CloudLoginMessage {
                    action: Some(MessageAction::Delete.into()),
                    ..Default::default()
                }),
                true
            )
        );
        messages
    };
    let messages = replies_for(7);
    let [(reply, false), (read_back, false)] = &messages[..] else {
        panic!("a Version reply and a Version READ: {messages:?}");
    };
    let ControlMessage::Version(reply) = reply else {
        panic!("Version reply");
    };
    assert_eq!(reply.request_id, None);
    assert_eq!(reply.device_serial_number.as_deref(), Some("TEST12345"));
    // Cortex Control reads field 4 as the CorOS version; without it, it refuses the unit.
    assert_eq!(reply.zenos_git_hash.as_deref(), Some("4.1.0"));
    assert_eq!(reply.custom_name.as_deref(), Some("Neural DSP Quad Cortex"));
    assert_eq!(reply.cloud_endpoint.as_deref(), Some("prod"));
    // 16 bytes are four words; the length word makes five.
    assert_eq!(reply.comms_version.as_ref().map(Vec::len), Some(20));
    assert_eq!(
        *read_back,
        ControlMessage::Version(VersionMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        })
    );
    assert_eq!(replies_for(0xee), messages);
}

/// The simulated device with a fake serial in its services files, booted, and a Cortex
/// Control link through the handshake.
fn connected_sim() -> (
    tetrad_app::device::Device<common::Sim>,
    std::rc::Rc<std::cell::RefCell<common::Log>>,
    common::Media,
) {
    let light = common::light_preset(&common::STOCK.specs(), 2);
    connected_sim_with(common::Media::new([("Light", light)]))
}

/// [`connected_sim`] with the given library.
fn connected_sim_with(
    media: common::Media,
) -> (
    tetrad_app::device::Device<common::Sim>,
    std::rc::Rc<std::cell::RefCell<common::Log>>,
    common::Media,
) {
    use tetrad_app::device::platform::{Platform, ServiceBackends};
    let log = std::rc::Rc::new(std::cell::RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    sim.services()
        .files()
        .put(tetrad_media::factory::SERIAL_PATH, "TEST12345\n");
    let mut device = tetrad_app::device::Device::start(sim).unwrap();
    let read = Some(MessageAction::Read.into());
    let handshake: [ControlMessage; 3] = [
        KeepAliveMessage {
            action: update(),
            ..Default::default()
        }
        .into(),
        ModelRepoMessage {
            action: read,
            ..Default::default()
        }
        .into(),
        ConnectionMessage {
            connected: Some(true),
            ..Default::default()
        }
        .into(),
    ];
    for message in handshake {
        sim_host(&mut device, message);
    }
    assert!(device.bridges().control.connected());
    (device, log, media)
}

/// r25 USB Statistics row 3: `0x6dfbf8` adds 1 to `+0x3f8` for each sent `0x27` frame whose
/// byte 2 is not `0xff`, so the transport reset marker of an unplug is not counted.
#[test]
fn hid_report_count_skips_the_reset_marker() {
    use tetrad_dsp_protocol::responses::{ResponseType, encode};
    use tetrad_dsp_protocol::{CoreId, DspCore};

    let (mut device, log, _media) = connected_sim();
    for usb in [1, 0] {
        let mut record = [0; 128];
        record[0] = usb;
        let rings = &device.platform().clock.rings;
        common::publish(
            &mut rings.0.borrow_mut(),
            CoreId::Dsp(DspCore::Core3),
            &encode(ResponseType::Soc2ArmDiagnostics, 0, &record),
        );
        (0..50).for_each(|_| _ = device.step());
    }
    assert!(!device.bridges().control.connected());
    let frames = |byte_2: u8| {
        (log.borrow().requests.iter())
            .filter(|sent| sent.kind() == 0x27 && sent.frame[2] == byte_2)
            .count()
    };
    assert_eq!(frames(0xff), 1, "the unplug sends one reset marker");
    assert!(frames(0) > 3);
    assert_eq!(
        device.bridges().control.hid_reports_sent() as usize,
        frames(0)
    );
}

/// One host message into the simulated device; idle steps let the scripted DSP read the
/// reports (it serves its rings on sleeps).
fn sim_host(device: &mut tetrad_app::device::Device<common::Sim>, message: ControlMessage) {
    for report in send(message) {
        device.push(tetrad_app::device::Event::Control(Box::new(report)));
        device.step();
    }
    (0..50).for_each(|_| _ = device.step());
}

/// One message the device sent to the host: Scene (13) has its own codec.
// A few values per test; unboxed so the tests can match on the message.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
enum Sent {
    Scene(SceneMessage),
    Message(ControlMessage),
}

/// The messages the device sent from request `mark` on, inflated, in send order.
fn sent(log: &common::Log, mark: usize) -> Vec<Sent> {
    use tetrad_dsp_protocol::hid::{HEADER_BYTES, REPORT_BYTES};
    let mut reassembler = Reassembler::new(Limits::default());
    let limits = tetrad_control::link::LinkLimits::default().payload;
    (log.requests[mark..].iter())
        .filter(|sent| sent.kind() == 0x27 && sent.frame[2] == 0)
        .filter_map(|sent| {
            reassembler
                .push(&sent.frame[HEADER_BYTES..HEADER_BYTES + REPORT_BYTES])
                .unwrap()
        })
        .map(|mut envelope| {
            if envelope.decompress != 0 {
                let mut plain = Vec::new();
                GzDecoder::new(&envelope.payload[..])
                    .read_to_end(&mut plain)
                    .unwrap();
                envelope.payload = plain;
                envelope.decompress = 0;
            }
            if envelope.message_type == CortexMessageType::Scene as u32 {
                let payload = envelope.plain_payload().unwrap();
                Sent::Scene(SceneMessage::decode(payload, limits).unwrap().message)
            } else {
                Sent::Message(ControlMessage::decode(&envelope, limits).unwrap().message)
            }
        })
        .collect()
}

fn scenes_sent(log: &common::Log, mark: usize) -> Vec<SceneMessage> {
    (sent(log, mark).into_iter())
        .filter_map(|sent| match sent {
            Sent::Scene(scene) => Some(scene),
            Sent::Message(_) => None,
        })
        .collect()
}

/// The RecallPreset, Scene, SetlistPosition, File and UndoRedo messages of `sent`, which
/// a QC-side preset change pushes (meters and the rest left out).
fn preset_pushes(sent: Vec<Sent>) -> Vec<Sent> {
    (sent.into_iter())
        .filter(|sent| match sent {
            Sent::Scene(_) => true,
            Sent::Message(message) => matches!(
                message,
                ControlMessage::RecallPreset(_)
                    | ControlMessage::SetlistPosition(_)
                    | ControlMessage::File(_)
                    | ControlMessage::UndoRedo(_)
            ),
        })
        .collect()
}

/// The working preset as Cortex Control gets it: the stored preset with the stock runtime
/// hash, FNV-1a of its `.pb` path (S7 stock `[1753]`; the sim presets hold no sidechain
/// block, so no source lists).
fn working_bytes(current: &tetrad_app::device::Current) -> Vec<u8> {
    let mut preset = current.preset.clone();
    if let Some(path) = current.path.as_deref().filter(|path| path.ends_with(".pb")) {
        preset.hash = Some(tetrad_core::PresetId::from_identifier(path).0);
    }
    preset.to_bytes()
}

fn frame_lists(sent: &Sent) -> bool {
    matches!(sent, Sent::Message(ControlMessage::UndoRedo(_)))
}

/// The pushes `0x5b2f40`, `0x5b7438` and `0x5b91dc` send for the loaded preset: RecallPreset
/// UPDATE with `reason` and the working preset, Scene, SetlistPosition with the slot.
fn expected_pushes(
    device: &tetrad_app::device::Device<common::Sim>,
    reason: tetrad_control::codecs::RecallPresetReason,
) -> [Sent; 3] {
    use tetrad_control::codecs::SetlistPositionMessage;
    let state = device.state();
    let current = state.current.as_ref().unwrap();
    let last = &state.settings.settings().last_preset;
    [
        Sent::Message(ControlMessage::RecallPreset(RecallPresetMessage {
            action: update(),
            preset: Some(RawMessageBytes {
                bytes: working_bytes(current),
            }),
            reason: Some(reason.into()),
            ..Default::default()
        })),
        Sent::Scene(scene_message(
            MessageAction::Update,
            None,
            current.deployment.scene().index() as u32,
        )),
        Sent::Message(ControlMessage::SetlistPosition(SetlistPositionMessage {
            action: update(),
            folder_key: last.setlist.clone(),
            position: Some(last.preset_index as u32),
            is_factory: Some(false),
            ..Default::default()
        })),
    ]
}

fn scene_message(action: MessageAction, request_id: Option<u64>, scene: u32) -> SceneMessage {
    SceneMessage {
        action: Some(action.into()),
        request_id,
        selected_scene: Some(scene),
    }
}

fn sim_select_scene(device: &mut tetrad_app::device::Device<common::Sim>, scene: u8) {
    let scene = tetrad_engine::SceneIndex::new(scene).unwrap();
    device.push(tetrad_app::device::Event::Action(
        tetrad_app::device::Action::SelectScene(scene),
    ));
    (0..50).for_each(|_| _ = device.step());
}

/// S7 on the QC (2026-10-16): a scene select on the QC left Cortex Control on its old scene.
/// Stock `setScene` `0x5899d8` pushes Scene UPDATE (`0x27c3fc`) on every change: a device
/// select (touch, footswitch, MIDI) outside any request, and a Control UPDATE inside it, so
/// the echo carries its request ID. The same scene again pushes nothing. READ replies with
/// the playing scene (`0x2787d8`).
#[test]
fn scene_changes_push_the_scene_to_control_and_read_replies() {
    use MessageAction::{Read, Update};
    let (mut device, log, _media) = connected_sim();
    let host_scene = |device: &mut tetrad_app::device::Device<common::Sim>,
                      message: SceneMessage| {
        let payload = message.encode(CodecLimits::default()).unwrap();
        for report in host(CortexMessageType::Scene as u32, payload) {
            device.push(tetrad_app::device::Event::Control(Box::new(report)));
            device.step();
        }
        (0..50).for_each(|_| _ = device.step());
    };
    let mark = log.borrow().requests.len();
    sim_select_scene(&mut device, 4);
    sim_select_scene(&mut device, 4);
    host_scene(&mut device, scene_message(Update, Some(8), 1));
    let read = SceneMessage {
        action: Some(Read.into()),
        request_id: Some(9),
        selected_scene: None,
    };
    host_scene(&mut device, read);
    assert_eq!(
        scenes_sent(&log.borrow(), mark),
        [
            scene_message(Update, None, 4),
            scene_message(Update, Some(8), 1),
            scene_message(Update, Some(9), 1),
        ]
    );
}

fn sim_load(device: &mut tetrad_app::device::Device<common::Sim>, index: u32) {
    device.push(tetrad_app::device::Event::Action(
        tetrad_app::device::Action::LoadPreset {
            target: tetrad_app::device::PresetRef {
                setlist: None,
                index,
            },
            reload: true,
        },
    ));
    (0..50).for_each(|_| _ = device.step());
}

/// S7 T1 on the QC (2026-10-16): after a preset load on the QC, Cortex Control kept the old
/// preset's grid. A load (`0x5b2f40` → `0x5b2de4`) pushes RecallPreset (reason Other, the
/// item's preset), then the forced `setScene(scene, 1)` the Scene (the preset's own scene,
/// not the one playing before), then SetlistPosition (`0x284bc8`), outside any request. The
/// frame lists with the load frame go before them (A12).
#[test]
fn a_preset_load_pushes_the_preset_scene_and_position_to_control() {
    use tetrad_control::codecs::RecallPresetReason;
    let (mut device, log, _media) = connected_sim();
    sim_select_scene(&mut device, 3);
    let loaded = device.state().current.as_ref().unwrap().path.clone();
    let mark = log.borrow().requests.len();
    sim_load(&mut device, 0);
    let current = device.state().current.as_ref().unwrap();
    assert_eq!(current.path, loaded);
    assert_ne!(current.deployment.scene().index(), 3);
    let pushes = preset_pushes(sent(&log.borrow(), mark));
    // A12: the frame lists with the load frame go first (`0x648f1c` before `0x5b2f40`), and
    // `Control::sync` sends no second list.
    let [frames, trio @ ..] = &pushes[..] else {
        panic!("{pushes:?}");
    };
    assert!(frame_lists(frames), "{pushes:?}");
    assert_eq!(trio, expected_pushes(&device, RecallPresetReason::Other));
}

/// The recent of a load (`0x7c3820`) runs after `0x5b2de4` in `0x5b2f40`: its DELETE and
/// CREATE go after the Scene and before the SetlistPosition (S7 stock `[1756]`, `[1757]`).
#[test]
fn a_preset_load_pushes_its_recent_between_the_scene_and_the_position() {
    let (mut device, log, _media) = connected_sim();
    let mark = log.borrow().requests.len();
    sim_load(&mut device, 0);
    let order: Vec<&str> = (sent(&log.borrow(), mark).iter())
        .filter_map(|sent| match sent {
            Sent::Scene(_) => Some("Scene"),
            Sent::Message(ControlMessage::RecallPreset(_)) => Some("RecallPreset"),
            Sent::Message(ControlMessage::SetlistPosition(_)) => Some("SetlistPosition"),
            Sent::Message(ControlMessage::RecentsFavorites(recent)) => {
                assert_eq!(recent.is_favorites, Some(false));
                let delete = recent.action == Some(MessageAction::Delete.into());
                let create = recent.action == Some(MessageAction::Create.into());
                assert!(delete || create, "{recent:?}");
                Some(if delete {
                    "RecentDelete"
                } else {
                    "RecentCreate"
                })
            }
            Sent::Message(_) => None,
        })
        .collect();
    assert_eq!(
        order,
        [
            "RecallPreset",
            "Scene",
            "RecentDelete",
            "RecentCreate",
            "SetlistPosition"
        ]
    );
}

/// A save of the loaded preset (`0x5b7438`, by touch) sends the File push of the save
/// (`0x2515b4`), then RecallPreset with reason Save, Scene and SetlistPosition.
#[test]
fn a_preset_save_pushes_the_file_then_the_saved_preset_to_control() {
    use tetrad_app::device::platform::TouchSample;
    use tetrad_app::device::ui::UiInput;
    use tetrad_control::codecs::RecallPresetReason;
    let (mut device, log, _media) = connected_sim();
    while !device.state().splash_dismissed {
        device.step();
    }
    device.platform().wall_clock.set(Some(
        std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_760_000_000),
    ));
    let mark = log.borrow().requests.len();
    let save = common::target(&device.bridges().ui, tetrad_ui::Control::SavePreset);
    for pressed in [true, false] {
        let touch = TouchSample { pressed, ..save };
        device.push(tetrad_app::device::Event::UiInput(UiInput::Touch(touch)));
        (0..50).for_each(|_| _ = device.step());
    }
    let pushes = preset_pushes(sent(&log.borrow(), mark));
    let [Sent::Message(ControlMessage::File(_)), rest @ ..] = &pushes[..] else {
        panic!("{pushes:?}");
    };
    assert_eq!(rest, expected_pushes(&device, RecallPresetReason::Save));
}

/// Undo of a load frame (`0x649330` case 5): the frame lists (`0x649120`), the reload
/// through `0x5b37dc` → `0x5b2f40` with the frame's working preset (`0x5b2de4` reason 1,
/// Undo), its Scene and SetlistPosition, then the case's own Scene push (`0x27c3fc`).
#[test]
fn an_undo_of_a_load_pushes_the_undo_preset_and_the_scene_twice() {
    use tetrad_control::codecs::RecallPresetReason;
    let specs = common::STOCK.specs();
    let media = common::Media::new([
        ("Light", common::light_preset(&specs, 2)),
        ("Other", common::light_preset(&specs, 3)),
    ]);
    let (mut device, log, _media) = connected_sim_with(media);
    sim_load(&mut device, 1);
    let mark = log.borrow().requests.len();
    device.push(tetrad_app::device::Event::Action(
        tetrad_app::device::Action::Undo,
    ));
    (0..50).for_each(|_| _ = device.step());
    assert_eq!(
        device.state().settings.settings().last_preset.preset_index,
        0
    );
    let pushes = preset_pushes(sent(&log.borrow(), mark));
    let [Sent::Message(ControlMessage::UndoRedo(_)), rest @ ..] = &pushes[..] else {
        panic!("{pushes:?}");
    };
    let [recall, scene, position] = expected_pushes(&device, RecallPresetReason::Undo);
    assert_eq!(rest[..4], [recall, scene.clone(), position, scene]);
    // `Control::sync` then sends the final frame lists.
    assert!(rest[4..].iter().all(frame_lists), "{rest:?}");
}

/// A8: a delete of the selected slot's preset (`0x1d916c` → `0x7c4c1c` → `0x5b2c54`) loads the
/// new-preset template in place: no file, the same slot, clean, with RecallPreset (reason
/// Other, no item), Scene and SetlistPosition. The UNDO (`0x647fa8` → `0x5b2ce8`) puts the
/// file back and plays the frame's working preset on it, so the edit returns unsaved:
/// RecallPreset (reason Undo) and Scene, no SetlistPosition.
#[test]
fn a_delete_of_the_selected_preset_loads_the_template_and_its_undo_brings_the_edit_back() {
    use tetrad_app::device::{Action, Event};
    use tetrad_control::codecs::RecallPresetReason;
    use tetrad_engine::{Column, Row, Site, Slot};
    // A delete frame needs a preset with a stored name (`SlotState::read`).
    let mut light = common::light_preset(&common::STOCK.specs(), 2);
    light.name = Some("Light".into());
    let (mut device, log, _media) = connected_sim_with(common::Media::new([("Light", light)]));
    let drive = |device: &mut tetrad_app::device::Device<common::Sim>, action| {
        device.push(Event::Action(action));
        (0..50).for_each(|_| _ = device.step());
    };
    let site = Site::Slot(Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    });
    let edit = Action::SetParameter {
        site,
        index: 0,
        value: 0.9,
    };
    drive(&mut device, edit);
    let current = device.state().current.as_ref().unwrap();
    let (file, edited) = (current.path.clone().unwrap(), current.preset.clone());
    assert!(device.state().preset_dirty());

    let mark = log.borrow().requests.len();
    let delete = Action::DeletePreset {
        setlist: "My Presets".into(),
        slot: 0,
    };
    drive(&mut device, delete);
    let state = device.state();
    let current = state.current.as_ref().unwrap();
    assert_eq!(current.path, None);
    assert_ne!(current.preset, edited);
    assert!(!state.preset_dirty());
    assert_eq!(state.settings.settings().last_preset.preset_index, 0);
    // A12: the delete frame (`0x1d916c`: `0x648f1c(.., 1, ..)` before `0x7c4c1c`) goes first.
    let pushes = preset_pushes(sent(&log.borrow(), mark));
    let [frames, trio @ ..] = &pushes[..] else {
        panic!("{pushes:?}");
    };
    assert!(frame_lists(frames), "{pushes:?}");
    assert_eq!(trio, expected_pushes(&device, RecallPresetReason::Other));

    let mark = log.borrow().requests.len();
    drive(&mut device, Action::Undo);
    let state = device.state();
    let current = state.current.as_ref().unwrap();
    assert_eq!(current.path.as_deref(), Some(file.as_str()));
    assert_eq!(current.preset, edited);
    assert!(state.preset_dirty());
    // The restored file has a sidecar, so the item adds its date: only the shape is compared.
    let pushes = preset_pushes(sent(&log.borrow(), mark));
    let [_, scene, _] = expected_pushes(&device, RecallPresetReason::Undo);
    let [
        Sent::Message(ControlMessage::RecallPreset(recall)),
        second,
        rest @ ..,
    ] = &pushes[..]
    else {
        panic!("{pushes:?}");
    };
    assert_eq!(recall.reason, Some(RecallPresetReason::Undo.into()));
    assert_eq!(*second, scene);
    assert!(rest.iter().all(frame_lists), "{pushes:?}");
}

/// Review r14 C-1 on the simulated device: the boot reads the serial (`0x876dec`), a Version
/// READ sends the DSP key request `0x42` with it (`0x5152c4`), and the `0x17` answer
/// (`0x2a4f58`) completes the exchange: a Version UPDATE with `comms_version`, then a Version
/// READ (Version is not a transform type). Fake serial and vectors.
#[test]
fn device_version_read_runs_the_key_exchange_through_the_dsp() {
    use tetrad_control::codecs::VersionMessage;
    use tetrad_dsp_protocol::hid::{HEADER_BYTES, REPORT_BYTES};
    use tetrad_dsp_protocol::responses::{ResponseType, encode};
    use tetrad_dsp_protocol::{CoreId, DspCore};

    let (mut device, log, _media) = connected_sim();
    let read = || Some(MessageAction::Read.into());
    let mark = log.borrow().requests.len();
    let version = VersionMessage {
        action: read(),
        request_id: Some(4),
        ..Default::default()
    };
    sim_host(&mut device, version.into());
    let key_request = log.borrow().requests[mark..]
        .iter()
        .find(|sent| sent.kind() == 0x42)
        .map(|sent| sent.frame.clone())
        .expect("the key request 0x42");
    assert!(key_request.windows(9).any(|text| text == b"TEST12345"));

    let (key, material) = ([7u8; 16], *b"fake vector B 16");
    let answer = encode(ResponseType::UsbMeterInput, 0, &[key, material].concat());
    let mark = log.borrow().requests.len();
    {
        let rings = &device.platform().clock.rings;
        common::publish(
            &mut rings.0.borrow_mut(),
            CoreId::Dsp(DspCore::Core3),
            &answer,
        );
    }
    (0..50).for_each(|_| _ = device.step());
    let mut reassembler = Reassembler::new(Limits::default());
    let sent: Vec<ControlMessage> = log.borrow().requests[mark..]
        .iter()
        .filter(|sent| sent.kind() == 0x27 && sent.frame[2] == 0)
        .filter_map(|sent| {
            let report = &sent.frame[HEADER_BYTES..HEADER_BYTES + REPORT_BYTES];
            reassembler.push(report).unwrap()
        })
        .map(|envelope| {
            assert_eq!(envelope.transform, 0);
            let limits = tetrad_control::link::LinkLimits::default().payload;
            ControlMessage::decode(&envelope, limits).unwrap().message
        })
        .collect();
    let [
        ControlMessage::Version(reply),
        ControlMessage::Version(read_back),
    ] = &sent[..]
    else {
        panic!("a Version UPDATE and a Version READ: {sent:?}");
    };
    assert_eq!(reply.action, update());
    assert_eq!(reply.device_serial_number.as_deref(), Some("TEST12345"));
    assert!(reply.comms_version.is_some());
    assert_eq!(read_back.action, read());
}

/// IOMeter CREATE registers the I/O page meter set (`0x6e08b4`): the device polls it (the
/// `0x0e` request for word `0x4024` belongs to that set only) while Control is subscribed,
/// off the I/O page.
#[test]
fn io_meter_subscription_polls_the_io_meter_set() {
    use tetrad_control::codecs::IOMeterMessage;
    use tetrad_dsp_host::session::Clock;
    let (mut device, log, _media) = connected_sim();
    let polls = |from: usize| {
        let log = log.borrow();
        (log.requests[from..].iter())
            .filter(|sent| sent.kind() == 0x0e && sent.frame[4..8] == 0x4024u32.to_le_bytes())
            .count()
    };
    let run = |device: &mut tetrad_app::device::Device<common::Sim>, millis| {
        let until = device.platform().clock.now() + Duration::from_millis(millis);
        while device.platform().clock.now() < until {
            device.step();
        }
    };
    let mark = log.borrow().requests.len();
    run(&mut device, 300);
    assert_eq!(polls(mark), 0);
    let subscribe = IOMeterMessage {
        action: Some(MessageAction::Create.into()),
        ..Default::default()
    };
    sim_host(&mut device, subscribe.into());
    run(&mut device, 300);
    assert!(polls(mark) >= 2);
}

/// NeuralCapture2 (63) `0x1201bc`: CREATE saves the audition (`0x1fef08`), UPDATE takes the
/// first present field (state before job id), a cab bypass carries the current bypass of
/// block (0,3), and the dialog open starts the controller (`showDialog` `0x3d791c`); no reply. Controller pushes go out as UPDATE
/// with one field (reset `0x122f04` = `start_from_scratch`).
#[test]
fn neural_capture2_requests_reach_the_nc2_controller_and_pushes_map_to_one_field() {
    use tetrad_app::device::control::nc2_message;
    use tetrad_capture::v2::{Nc2Input, Nc2Remote, Nc2State};
    use tetrad_control::codecs::NeuralCapture2Message;
    let (mut state, mut control) = loaded::session();
    let mut nc2 = |message: NeuralCapture2Message| {
        let (sent, effects) = loaded::host(&mut control, &mut state, message, at(10));
        assert!(sent.is_empty());
        effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Nc2(input) => Some(input),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let create = NeuralCapture2Message {
        action: Some(MessageAction::Create.into()),
        ..Default::default()
    };
    assert_eq!(nc2(create), [Nc2Input::Save]);
    let both = NeuralCapture2Message {
        action: update(),
        state: Some(2),
        job_id: Some("job".into()),
        ..Default::default()
    };
    assert_eq!(nc2(both), [Nc2Input::SetState(2)]);
    let bypass = NeuralCapture2Message {
        action: update(),
        cabsim_bypass: Some(true),
        ..Default::default()
    };
    assert_eq!(
        nc2(bypass),
        [Nc2Input::CabsimBypass {
            bypass: true,
            current: false
        }]
    );
    let open = NeuralCapture2Message {
        action: update(),
        open_dialog: Some(true),
        ..Default::default()
    };
    assert_eq!(nc2(open), [Nc2Input::Open]);

    let ControlMessage::NeuralCapture2(reset) = nc2_message(Nc2Remote::Reset) else {
        panic!("type 63");
    };
    assert_eq!(
        (reset.action, reset.start_from_scratch),
        (update(), Some(true))
    );
    let ControlMessage::NeuralCapture2(pushed) = nc2_message(Nc2Remote::State(Nc2State::Training))
    else {
        panic!("type 63");
    };
    assert_eq!(
        pushed,
        NeuralCapture2Message {
            action: update(),
            state: Some(3),
            ..Default::default()
        }
    );
}

/// GeneralSettings `0x163274`/`0x16c964`: READ reports the device settings; UPDATE applies
/// only the first present field in stock order (screen brightness before LED), and a field
/// that fails its guard (above 100) falls through to the next; each changed field is pushed
/// alone (`0x168974`, `0x168ffc`).
#[test]
fn general_settings_apply_the_first_field_and_push_each_change() {
    use tetrad_control::codecs::GeneralSettingsMessage;
    let (mut state, mut control) = loaded::session();
    let read = GeneralSettingsMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read, at(10));
    let [ControlMessage::GeneralSettings(reply)] = &sent[..] else {
        panic!("one GeneralSettings reply: {sent:?}");
    };
    assert_eq!(reply.action, update());
    assert_eq!(reply.screen_brightness, Some(50));
    assert_eq!(reply.midi_channel, Some(1));
    assert_eq!(reply.lock_screen_and_volume_knob, Some(false));
    assert_eq!(reply.looper_stomp_assignments.len(), 8);
    assert!(
        loaded::sync(&mut control, &mut state, at(20))
            .iter()
            .all(|message| { !matches!(message, ControlMessage::GeneralSettings(_)) })
    );

    let write = |screen, led| GeneralSettingsMessage {
        action: update(),
        screen_brightness: Some(screen),
        led_brightness: Some(led),
        ..Default::default()
    };
    loaded::host(&mut control, &mut state, write(80, 10), at(30));
    let display = &state.settings.settings().display;
    assert_eq!(
        (display.brightness.get(), display.led_brightness.get()),
        (80, 27)
    );
    loaded::host(&mut control, &mut state, write(150, 10), at(40));
    let display = &state.settings.settings().display;
    assert_eq!(
        (display.brightness.get(), display.led_brightness.get()),
        (80, 10)
    );
    let pushes: Vec<_> = loaded::sync(&mut control, &mut state, at(50))
        .into_iter()
        .filter_map(|message| match message {
            ControlMessage::GeneralSettings(message) => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(
        pushes,
        [
            GeneralSettingsMessage {
                action: update(),
                screen_brightness: Some(80),
                ..Default::default()
            },
            GeneralSettingsMessage {
                action: update(),
                led_brightness: Some(10),
                ..Default::default()
            },
        ]
    );
}

/// MIDISettings `0x1912d4`: UPDATE stores the on-load list as one undo frame and echoes it
/// in field 4 without `source` (`0x5146d4`); an equal list does nothing; READ has no arm; a
/// source of 10 or more is refused (owned).
#[test]
fn midi_settings_update_stores_the_list_once_and_read_is_ignored() {
    use tetrad_control::codecs::{
        GeneralMIDIMessage, GeneralMIDIMessages, MIDISettingsMessage, MidiMessageInfo,
    };
    let (mut state, mut control) = loaded::session();
    let list = |source| GeneralMIDIMessages {
        messages: vec![GeneralMIDIMessage {
            source,
            messages: vec![MidiMessageInfo {
                message_type: Some(1),
                channel: Some(2),
                param1: Some(3),
                ..Default::default()
            }],
        }],
    };
    let on_load = MIDISettingsMessage {
        action: update(),
        preset_load_messages: Some(list(None)),
        ..Default::default()
    };
    // Review r37 J1: the cached PresetDirty answer (read here) must not survive the edit.
    *state.clean.get_mut() = state
        .current
        .as_ref()
        .map(tetrad_app::device::preset::Snapshot::of);
    assert!(!state.preset_dirty());
    let (_, effects) = loaded::host(&mut control, &mut state, on_load.clone(), at(10));
    assert!(state.preset_dirty(), "the on-load MIDI edit is unsaved");
    let [ControlMessage::MIDISettings(echo)] = &loaded::notified(&effects)[..] else {
        panic!("one MIDISettings echo: {effects:?}");
    };
    assert_eq!(
        echo.general_midi_messages.as_ref().unwrap().messages[0].source,
        None
    );
    assert_eq!(echo.preset_load_messages, None);
    let stored = &state.current.as_ref().unwrap().preset.midi_messages[0];
    assert_eq!((stored.kind, stored.channel, stored.param1), (1, 2, 3));
    assert_eq!(state.history.depths(), (1, 0));
    let (_, effects) = loaded::host(&mut control, &mut state, on_load, at(5_000));
    assert!(loaded::notified(&effects).is_empty());
    assert_eq!(state.history.depths(), (1, 0));
    let read = MIDISettingsMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    assert!(
        loaded::host(&mut control, &mut state, read, at(5_010))
            .0
            .is_empty()
    );
    let source = MIDISettingsMessage {
        action: update(),
        general_midi_messages: Some(list(Some(10))),
        ..Default::default()
    };
    let (_, effects) = loaded::host(&mut control, &mut state, source, at(5_020));
    assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
}

/// GlobalEQ `0x16d6c4`/`0x16e044`: READ sends indices 0..=27 and `bypassed`; an ASSIGN edit
/// stores 1.0 and pushes the outputs; a band edit goes to the assigned output only (no ON
/// check); each change is pushed {index, value}; `bypassed` false turns the EQ on; a
/// negative index is refused (owned).
#[test]
fn global_eq_update_stores_writes_assigned_outputs_and_pushes_changes() {
    use tetrad_app::device::DspCommand;
    use tetrad_control::codecs::{GlobalEQMessage, GlobalEQParameter};
    use tetrad_dsp_protocol::controls::ControlRequest;
    let parameters: String = (0..28)
        .map(|i| format!("<Parameter name='P{i}' type='float' min='0' max='1' defaultValue='0'/>"))
        .collect();
    let (mut state, mut control) = loaded::session_with_extra(
        &format!(
            "<Category id='40' name='EQ' hidden='true'>\
<Model id='4004' name='Parametric-5'>{parameters}</Model></Category>"
        ),
        "",
    );
    let read = GlobalEQMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read, at(10));
    let [ControlMessage::GlobalEQ(reply)] = &sent[..] else {
        panic!("one GlobalEQ reply: {sent:?}");
    };
    assert_eq!((reply.parameters.len(), reply.bypassed), (28, Some(true)));
    loaded::sync(&mut control, &mut state, at(20));

    let set = |index, value| GlobalEQMessage {
        action: update(),
        parameters: vec![GlobalEQParameter {
            parameter_index: Some(index),
            value: Some(value),
        }],
        ..Default::default()
    };
    let (_, effects) = loaded::host(&mut control, &mut state, set(26, 0.9), at(30));
    assert!(!effects.is_empty());
    assert_eq!(
        state.settings.settings().global_eq.params.get(&26),
        Some(&1.0)
    );
    let (_, effects) = loaded::host(&mut control, &mut state, set(0, 0.25), at(40));
    let outputs: Vec<u8> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Dsp(DspCommand::Control(ControlRequest::OutputEqualizerParameter {
                output,
                parameter: 0,
                ..
            })) => Some(*output),
            _ => None,
        })
        .collect();
    assert_eq!(outputs, [0]);
    let pushed: Vec<_> = loaded::sync(&mut control, &mut state, at(50))
        .into_iter()
        .filter_map(|message| match message {
            ControlMessage::GlobalEQ(message) => Some(message.parameters),
            _ => None,
        })
        .collect();
    let parameter = |index, value| GlobalEQParameter {
        parameter_index: Some(index),
        value: Some(value),
    };
    assert_eq!(pushed, [vec![parameter(0, 0.25)], vec![parameter(26, 1.0)]]);
    let on = GlobalEQMessage {
        action: update(),
        bypassed: Some(false),
        ..Default::default()
    };
    loaded::host(&mut control, &mut state, on, at(60));
    let params = &state.settings.settings().global_eq.params;
    assert_eq!(params.get(&29), Some(&0.0));
    let (_, effects) = loaded::host(&mut control, &mut state, set(-1, 0.5), at(70));
    assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
}

/// IOMeter (`0x6e0040` → `0x111cf8`): CREATE starts the 100 ms sender; a field is the bar
/// level of its dB reading and an unchanged block is not sent again. CPULoad CREATE sends at
/// once: four chains of eight cells (`0x14a7b4`) and `cpu_total_load`, the mean of the four
/// DSP cores (`0x364ba0` `+0x1c8`; a core that has not reported counts 0).
#[test]
fn io_meters_send_levels_on_change_and_cpu_load_is_a_four_by_eight_table() {
    use tetrad_control::codecs::{CPULoadMessage, IOMeterMessage};
    let (mut state, mut control) = loaded::session();
    assert!(!control.io_meters_subscribed());
    let create = IOMeterMessage {
        action: Some(MessageAction::Create.into()),
        ..Default::default()
    };
    loaded::host(&mut control, &mut state, create, at(10));
    assert!(control.io_meters_subscribed());
    state.io_meters.inputs[0] = -20.0;
    let meters = |control: &mut Control, state: &mut tetrad_app::device::State, now| {
        replies(control.tick(state, now).unwrap()).0
    };
    let sent = meters(&mut control, &mut state, at(110));
    let [(ControlMessage::IOMeter(block), _)] = &sent[..] else {
        panic!("one IOMeter block: {sent:?}");
    };
    assert_eq!((block.input_1, block.input_2), (Some(0.5), Some(0.0)));
    assert!(meters(&mut control, &mut state, at(210)).is_empty());
    let mut words = [0; 16];
    words[5] = 40.0_f32.to_bits();
    let block = tetrad_dsp_protocol::responses::DiagnosticsBlock { words };
    state
        .cpu
        .record(tetrad_dsp_protocol::DspCore::Core2, &block);
    let cpu = CPULoadMessage {
        action: Some(MessageAction::Create.into()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, cpu, at(220));
    let [ControlMessage::CPULoad(table)] = &sent[..] else {
        panic!("one CPULoad table: {sent:?}");
    };
    assert_eq!(table.cpu_total_load, Some(10.0));
    assert_eq!(table.chains.len(), 4);
    assert!(table.chains.iter().all(|chain| chain.columns.len() == 8));
}

/// RecentSearches (`0x7a5334`, `0x7a50a0`) and PinnedModels (`0x12ba94`): changes answer
/// with pushes; an UPDATE of more than 20 searches is ignored; pinned rows read newest first
/// and split by length (ten characters or fewer is a model id).
#[test]
fn library_lists_push_changes_and_pinned_models_split_by_length() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{PinnedModelsMessage, RecentSearchesMessage};
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, StockDb};
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-lists-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap()),
    });
    let searches = |searches: Vec<String>| RecentSearchesMessage {
        action: update(),
        searches,
        ..Default::default()
    };
    let (_, effects) = loaded::host(
        &mut control,
        &mut state,
        searches(vec!["fuzz".into(), "amp".into()]),
        at(10),
    );
    assert_eq!(
        loaded::notified(&effects),
        [ControlMessage::RecentSearches(searches(vec![
            "fuzz".into(),
            "amp".into()
        ]))]
    );
    let many = searches((0..21).map(|i| i.to_string()).collect());
    let (_, effects) = loaded::host(&mut control, &mut state, many, at(20));
    assert!(loaded::notified(&effects).is_empty());

    let key = "k".repeat(64);
    let pin = PinnedModelsMessage {
        action: Some(MessageAction::Create.into()),
        models: vec![7],
        captures: vec![key.clone()],
        ..Default::default()
    };
    let (_, effects) = loaded::host(&mut control, &mut state, pin, at(30));
    assert_eq!(loaded::notified(&effects).len(), 2);
    let read = PinnedModelsMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read, at(40));
    let [ControlMessage::PinnedModels(pinned)] = &sent[..] else {
        panic!("one PinnedModels reply: {sent:?}");
    };
    assert_eq!(
        (&pinned.models[..], &pinned.captures[..]),
        (&[7][..], &[key][..])
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Review r15 R10: a bring-up run without `--persist` opens `zenui.db` read-only, but the list
/// files are plain files. Cortex Control list writes are refused there: the stored searches
/// stay and nothing is pushed.
#[test]
fn library_list_writes_need_persist() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{
        FileMessage, FolderInfo, RecentFavoriteType, RecentSearchesMessage, RecentsFavoritesItem,
        RecentsFavoritesMessage,
    };
    use tetrad_library::stock::{ItemList, StockLibrary, StockLimits};
    use tetrad_storage::media::Collection;
    use tetrad_storage::media::MediaRoot;
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: None,
    });
    let message = |action: MessageAction, searches: Vec<String>| RecentSearchesMessage {
        action: Some(action.into()),
        searches,
        ..Default::default()
    };
    loaded::host(
        &mut control,
        &mut state,
        message(MessageAction::Update, vec!["amp".into()]),
        at(10),
    );

    control.set_run_options(&loaded::bringup(false));
    for (action, searches) in [
        (MessageAction::Update, vec!["fuzz".to_owned()]),
        (MessageAction::Delete, Vec::new()),
    ] {
        let (_, effects) =
            loaded::host(&mut control, &mut state, message(action, searches), at(20));
        assert!(loaded::notified(&effects).is_empty(), "{effects:?}");
    }
    let stored = state.library.as_ref().unwrap().presets.recent_searches();
    assert_eq!(stored.unwrap(), ["amp"]);
    // RecentsFavorites CREATE and the File setlist CREATE are refused the same way (r18 F14).
    let favourite = RecentsFavoritesMessage {
        action: Some(MessageAction::Create.into()),
        is_favorites: Some(true),
        item_type: Some(RecentFavoriteType::ImpulseResponse.into()),
        items: vec![RecentsFavoritesItem {
            folder_key: Some("local_ir_root".into()),
            product_key: Some("CIR_1".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let setlist = FileMessage {
        action: Some(MessageAction::Create.into()),
        file_type: Some(0),
        folder: Some(FolderInfo {
            name: Some("Tour".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (_, effects) = loaded::host(&mut control, &mut state, favourite, at(30));
    assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
    let (_, effects) = loaded::host(&mut control, &mut state, setlist, at(40));
    assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
    let library = &state.library.as_ref().unwrap().presets;
    let favourites = ItemList::Favorites(Collection::Irs);
    assert!(library.items(favourites).unwrap().is_empty());
    assert!(library.setlists().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// License (58) and CloudLogin (18) are transform types. READ `0x114154` lists the licensed
/// ids, the unacknowledged ones and the supported table; DELETE `0x1f170c` clears the "new"
/// mark of `new_plugin_ids` and pushes the state; CloudLogin READ while logged out replies
/// DELETE (`0x23943c`).
#[test]
fn licence_read_and_acknowledge_and_logged_out_cloud_login_read() {
    use tetrad_app::device::control::CommsCipher;
    use tetrad_control::codecs::{CloudLoginMessage, LicenseMessage};
    let (mut state, mut control) = loaded::session();
    let cipher = || CommsCipher::derive(b"fake key-exchange vector");
    control.set_transform(Some(cipher()));
    state.settings.update(|settings| {
        settings.licenses.previous = vec![3];
        settings.licenses.new = vec![3];
    });
    state.licences = tetrad_app::device::services::Licences::stored(&state.settings);
    let host = |control: &mut Control, state: &mut tetrad_app::device::State, message, now| {
        let mut effects = Vec::new();
        for report in send(message) {
            effects.extend(control.on_report(&report, state, now).unwrap());
        }
        sealed_replies(&effects, &cipher())
    };
    let read = LicenseMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let sent = host(&mut control, &mut state, ControlMessage::from(read), at(10));
    let [(ControlMessage::License(licences), true)] = &sent[..] else {
        panic!("one sealed License reply: {sent:?}");
    };
    assert_eq!(licences.licenses.len(), 1);
    assert_eq!(licences.licenses[0].plugin_id, Some(3));
    assert_eq!(licences.new_plugin_ids, [3]);
    assert_eq!(licences.supported_plugin_ids.len(), 12);
    let acknowledge = LicenseMessage {
        action: Some(MessageAction::Delete.into()),
        new_plugin_ids: vec![3],
        ..Default::default()
    };
    // A device step after the connection takes the baseline of the change pushes.
    control.sync(&mut state, false, at(15)).unwrap();
    assert!(host(&mut control, &mut state, acknowledge.into(), at(20)).is_empty());
    assert!(state.settings.settings().licenses.new.is_empty());
    // The state push comes from the `Control::sync` diff at the request end, once.
    let effects = control.request_done(&mut state, false, at(21)).unwrap();
    let [(ControlMessage::License(pushed), true)] = &sealed_replies(&effects, &cipher())[..] else {
        panic!("one sealed License push: {effects:?}");
    };
    assert!(pushed.new_plugin_ids.is_empty());
    assert!(control.sync(&mut state, false, at(22)).unwrap().is_empty());
    let login = CloudLoginMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let sent = host(
        &mut control,
        &mut state,
        ControlMessage::from(login),
        at(30),
    );
    assert!(matches!(
        &sent[..],
        [(ControlMessage::CloudLogin(reply), true)] if reply.action == Some(MessageAction::Delete.into())
    ));
}

// --- r16-control: types that were at the trait default ----------------------------------

/// EnableCaptureOut (16) `0x240108`: `SetNCOutSel(enable, 0, 3, 0, 25000)` with no guard, then
/// the reply with the applied value.
#[test]
fn enable_capture_out_sets_the_nc_output_and_replies_the_value() {
    use tetrad_control::codecs::EnableCaptureOutMessage;
    use tetrad_hal::hal::Command;
    use tetrad_hal::zenjack::NcOutput;
    let (mut state, mut control) = loaded::session();
    for (enable, output) in [(true, NcOutput::Capture), (false, NcOutput::Normal)] {
        let request = EnableCaptureOutMessage {
            request_id: Some(9),
            enable: Some(enable),
        };
        let (sent, effects) = loaded::host(&mut control, &mut state, request, at(10));
        assert_eq!(effects, [Effect::Hal(Command::nc_output(output))]);
        assert_eq!(
            sent,
            [ControlMessage::EnableCaptureOut(EnableCaptureOutMessage {
                request_id: Some(9),
                enable: Some(enable),
            })]
        );
    }
}

/// BulkOperation (57) `0xed8d0`: READ replies `is_multiselection_active`; DELETE only sets the
/// abort flag (`+0x7c`): no reply, and a running batch upload stops before its next item
/// (`AccountRequest::CancelBulkRun`, re-r38 §2 step 8).
#[test]
fn bulk_operation_read_reports_no_active_job_and_delete_stops_a_batch() {
    use tetrad_control::codecs::BulkOperationMessage;
    let (mut state, mut control) = loaded::session();
    let request = |action: MessageAction| BulkOperationMessage {
        action: Some(action.into()),
        request_id: Some(4),
        ..Default::default()
    };
    let (sent, _) = loaded::host(
        &mut control,
        &mut state,
        request(MessageAction::Read),
        at(10),
    );
    assert_eq!(
        sent,
        [ControlMessage::BulkOperation(BulkOperationMessage {
            action: update(),
            request_id: Some(4),
            is_multiselection_active: Some(false),
            ..Default::default()
        })]
    );
    let (sent, effects) = loaded::host(
        &mut control,
        &mut state,
        request(MessageAction::Delete),
        at(20),
    );
    assert!(sent.is_empty(), "{sent:?}");
    let cancel = tetrad_app::device::control::AccountRequest::CancelBulkRun;
    assert_eq!(effects, [Effect::ControlAccount(cancel)]);
}

/// A float mono 48 kHz WAV with an 18-byte `fmt` chunk, the layout of the stock extractor
/// `0x69c384` (invented samples).
fn ir_wav(samples: usize) -> Vec<u8> {
    let data: Vec<u8> = (0..samples)
        .flat_map(|i| (i as f32 / samples as f32).to_le_bytes())
        .collect();
    let mut body = b"WAVEfmt ".to_vec();
    body.extend(18_u32.to_le_bytes());
    body.extend(3_u16.to_le_bytes());
    body.extend(1_u16.to_le_bytes());
    body.extend(48_000_u32.to_le_bytes());
    body.extend(192_000_u32.to_le_bytes());
    body.extend([4, 0, 32, 0, 0, 0]);
    body.extend(b"data");
    body.extend((data.len() as u32).to_le_bytes());
    body.extend(data);
    let mut file = b"RIFF".to_vec();
    file.extend((body.len() as u32).to_le_bytes());
    file.extend(body);
    file
}

/// MR 4 of r16-media: a File (4) CREATE with an IR payload into an IR folder key runs the
/// stock import `0x24dae0`: the first 1,024 samples under `CIR_` + MD5 in "My IRs", named by
/// `files[0]`. A preset folder key imports nothing.
#[test]
fn file_create_imports_an_ir_into_the_keyed_ir_folder() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_media::factory::DeviceCipher;
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, ProductType, StockDb};
    let root = std::env::temp_dir().join(format!("tetrad-app-control-ir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let (mut state, mut control) = loaded::session();
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    database
        .transaction(|write| write.boot_maintenance())
        .unwrap();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(database),
    });
    control.set_media_cipher(DeviceCipher::caller_secret(b"invented cipher"));
    control.set_wall_clock(Some(
        std::time::UNIX_EPOCH + Duration::from_secs(1_791_635_696),
    ));
    let import = |key: &str| FileMessage {
        action: Some(MessageAction::Create.into()),
        folder: Some(FolderInfo {
            key: Some(key.into()),
            files: vec![ProductData {
                name: Some("Room IR".into()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ir_payload: Some(ir_wav(1500)),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, import("local_ir_root"), at(10));
    assert!(sent.is_empty(), "{sent:?}");
    assert!(effects.contains(&Effect::RefreshDirectory), "{effects:?}");
    let view = state
        .library
        .as_ref()
        .unwrap()
        .database
        .as_ref()
        .unwrap()
        .view();
    let ids = view.product_ids(ProductType::Ir).unwrap();
    let [id] = &ids[..] else {
        panic!("one IR: {ids:?}");
    };
    assert!(id.starts_with("CIR_"));
    let row = view.product(id).unwrap().unwrap();
    assert_eq!(row.raw_data.unwrap().len(), 1024 * 4);
    let (_, effects) = loaded::host(
        &mut control,
        &mut state,
        import("/media/p4/Presets/My Presets"),
        at(20),
    );
    assert!(!effects.contains(&Effect::RefreshDirectory));
    let _ = std::fs::remove_dir_all(&root);
}

/// File (4) READ by type (`0x248ad4`) and the resync listing (`0x25057c`): a setlist lists its
/// presets with the sidecar fields (`0x242ae4`; key = path, index = slot); the IR tree goes
/// root first with full items, then each sub-folder with keys only (`0x240e68`). Every
/// listing message is UPDATE with the folder's type; an import into a sub-folder pushes
/// CREATE with that folder and the full item (`0x2536ac`). A stored download pushes CREATE
/// with the Downloads folder and that product (`0x250634`).
#[test]
fn file_read_and_resync_list_setlists_the_ir_tree_and_downloads() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::stock::{StockLibrary, StockLimits, create_media_folder};
    use tetrad_media::factory::DeviceCipher;
    use tetrad_storage::media::{MediaRoot, SETLIST_SLOTS, encode_setlist};
    use tetrad_storage::stock::{DbLimits, ProductType, StockDb};
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-files-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let setlist = root.join("Presets/My Presets");
    std::fs::create_dir_all(&setlist).unwrap();
    let path = "/media/p4/Presets/My Presets/Clean.pb";
    let mut slots = vec![None; SETLIST_SLOTS];
    slots[2] = Some(path.to_owned());
    std::fs::write(setlist.join("setlist"), encode_setlist(&slots).unwrap()).unwrap();
    std::fs::write(
        setlist.join("Clean.json"),
        r#"{"name":"Clean","author":"Me","date":"2026-01-02T03:04:05Z","tags":["a"]}"#,
    )
    .unwrap();
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    database
        .transaction(|write| write.boot_maintenance())
        .unwrap();
    let top = database
        .view()
        .root_folder(ProductType::Ir)
        .unwrap()
        .unwrap();
    let cabs = create_media_folder(&mut database, ProductType::Ir, top, "Cabs").unwrap();
    let download = tetrad_storage::stock::Download {
        id: "dl-preset".into(),
        hash: None,
        raw_data: Some(vec![1, 2]),
        metadata: Some(br#"{"name":"Cloud Lead"}"#.to_vec()),
    };
    let preset = tetrad_storage::stock::DownloadKind::Preset;
    tetrad_library::stock::store_download(&mut database, preset, &download).unwrap();
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(database),
    });
    control.set_media_cipher(DeviceCipher::caller_secret(b"invented cipher"));
    control.set_wall_clock(Some(
        std::time::UNIX_EPOCH + Duration::from_secs(1_791_635_696),
    ));
    let cabs_key = format!("{cabs}_q");
    let import = FileMessage {
        action: Some(MessageAction::Create.into()),
        folder: Some(FolderInfo {
            key: Some(cabs_key.clone()),
            files: vec![ProductData {
                name: Some("Room IR".into()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ir_payload: Some(ir_wav(64)),
        ..Default::default()
    };
    let (_, effects) = loaded::host(&mut control, &mut state, import, at(10));
    let [ControlMessage::File(pushed)] = &loaded::notified(&effects)[..] else {
        panic!("one File push: {effects:?}");
    };
    let folder = pushed.folder.as_ref().unwrap();
    assert_eq!(pushed.action, Some(MessageAction::Create.into()));
    assert_eq!(
        (folder.key.as_deref(), folder.files[0].name.as_deref()),
        (Some(&cabs_key[..]), Some("Room IR"))
    );
    let id = folder.files[0].key.clone().unwrap();

    let read = |file_type| FileMessage {
        action: Some(MessageAction::Read.into()),
        request_id: Some(5),
        file_type: Some(file_type),
        omit_factory_content: Some(true),
        ..Default::default()
    };
    // File replies; a Downloads folder follows its transfer state push.
    let files = |sent: Vec<ControlMessage>| -> Vec<FileMessage> {
        sent.into_iter()
            .filter_map(|message| match message {
                ControlMessage::File(file) => Some(file),
                ControlMessage::CloudProduct(push) if push.cloud_transfer_state.is_some() => None,
                other => panic!("File only: {other:?}"),
            })
            .collect()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read(0), at(20));
    let order: Vec<_> = sent.iter().map(ControlMessage::message_type).collect();
    assert_eq!(
        order,
        [
            CortexMessageType::File,
            CortexMessageType::CloudProduct,
            CortexMessageType::File
        ],
        "the setlist, the Downloads transfer state, the Downloads folder"
    );
    let presets = files(sent);
    let [setlist, downloads] = &presets[..] else {
        panic!("one setlist and Downloads: {presets:?}");
    };
    // `0x24592c`: key only, `is_downloads`, one product per stored row.
    let downloads = downloads.folder.as_ref().unwrap();
    assert_eq!(
        (downloads.key.as_deref(), downloads.name.as_deref()),
        (Some("cloud-0-1"), None)
    );
    assert_eq!(
        (downloads.is_downloads, downloads.is_factory),
        (Some(true), Some(false))
    );
    assert_eq!(
        (
            downloads.files[0].key.as_deref(),
            downloads.files[0].name.as_deref()
        ),
        (Some("dl-preset"), Some("Cloud Lead"))
    );
    assert_eq!(
        (setlist.action, setlist.file_type, setlist.request_id),
        (update(), Some(0), Some(5))
    );
    let folder = setlist.folder.as_ref().unwrap();
    assert_eq!(folder.key.as_deref(), Some("/media/p4/Presets/My Presets"));
    // All 256 slots; an empty one is `{index, is_readonly}` (S7 capture).
    assert_eq!(folder.files.len(), 256);
    assert_eq!(folder.is_user_default, Some(true));
    assert_eq!(
        folder.files[..3],
        [
            ProductData {
                index: Some(0),
                is_readonly: Some(false),
                ..Default::default()
            },
            ProductData {
                index: Some(1),
                is_readonly: Some(false),
                ..Default::default()
            },
            ProductData {
                key: Some(path.into()),
                index: Some(2),
                name: Some("Clean".into()),
                author: Some("Me".into()),
                tags: vec!["a".into()],
                date_ms_since_epoch: Some(1_767_323_045_000),
                is_readonly: Some(false),
                ..Default::default()
            }
        ]
    );
    let irs = files(loaded::host(&mut control, &mut state, read(1), at(30)).0);
    // The IRs Library root, the `zenui.db` user root `<id>_q` below it, then its folder.
    let [ir_root, user_root, sub] = &irs[..] else {
        panic!("the IR root, the user root and one folder: {irs:?}");
    };
    let (ir_root, user_root, sub) = (
        ir_root.folder.as_ref().unwrap(),
        user_root.folder.as_ref().unwrap(),
        sub.folder.as_ref().unwrap(),
    );
    assert_eq!(
        (ir_root.key.as_deref(), ir_root.name.as_deref()),
        (Some("local_ir_root"), Some("IRs Library"))
    );
    assert_eq!(ir_root.files[0].name.as_deref(), Some("Room IR"));
    let ir_user_key = format!("{top}_q");
    assert_eq!(
        (
            user_root.key.as_deref(),
            user_root.parent_key.as_deref(),
            user_root.name.as_deref(),
            user_root.is_user_default
        ),
        (
            Some(&ir_user_key[..]),
            Some("local_ir_root"),
            Some("My IRs"),
            Some(true)
        )
    );
    assert_eq!(
        (sub.parent_key.as_deref(), sub.name.as_deref()),
        (Some(&ir_user_key[..]), Some("Cabs"))
    );
    assert_eq!(
        sub.files,
        [ProductData {
            key: Some(id),
            ..Default::default()
        }]
    );

    let effects = control.resync(true, &mut state, at(40)).unwrap();
    // Scene goes out on its own sender, so only the File envelopes are decoded here.
    let mut reassembler = Reassembler::new(Limits::default());
    let mut keys = Vec::new();
    for effect in effects {
        let Effect::ControlReport(report) = effect else {
            continue;
        };
        let Some(envelope) = reassembler.push(&report[..]).unwrap() else {
            continue;
        };
        if envelope.message_type == CortexMessageType::File as u32 {
            let limits = tetrad_control::link::LinkLimits::default().payload;
            let decoded = ControlMessage::decode(&envelope, limits).unwrap().message;
            let ControlMessage::File(file) = decoded else {
                unreachable!("type 4 decodes as File");
            };
            keys.extend(file.folder.and_then(|folder| folder.key));
        }
    }
    let nc_user_key = (state.library.as_ref().unwrap().database.as_ref().unwrap())
        .view()
        .root_folder(ProductType::Capture)
        .unwrap()
        .map(|id| format!("{id}_q"))
        .unwrap();
    assert_eq!(
        keys,
        [
            "/media/p4/Presets/My Presets",
            "local_nc_root",
            &nc_user_key[..],
            "local_ir_root",
            &ir_user_key[..],
            &cabs_key[..],
            "cloud-0-1",
            "cloud-2-1"
        ]
    );
    let stored = control.download_stored(&state, preset, "dl-preset");
    let [ControlMessage::File(push)] = &loaded::notified(&stored)[..] else {
        panic!("one Downloads push: {stored:?}");
    };
    let folder = push.folder.as_ref().unwrap();
    let item = &folder.files[..];
    assert_eq!(
        (push.action, push.file_type, folder.key.as_deref()),
        (
            Some(MessageAction::Create.into()),
            Some(0),
            Some("cloud-0-1")
        )
    );
    assert_eq!(folder.is_downloads, Some(true));
    assert_eq!(
        (item.len(), item[0].key.as_deref(), item[0].name.as_deref()),
        (1, Some("dl-preset"), Some("Cloud Lead"))
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// ProcessDownloadsQueue (45) UPDATE `0x1326a8`: logged out does nothing; logged in, queue
/// type 0 runs the preset queue client and any other type the capture one (r26-media MR 1).
/// Bring-up without `--persist` refuses it (each product is a library store).
#[test]
fn downloads_queue_runs_the_queue_client_when_logged_in() {
    use tetrad_app::device::control::AccountRequest;
    use tetrad_control::codecs::ProcessDownloadsQueueMessage;
    use tetrad_services::cloud::services::ProductKind;
    let (mut state, mut control) = loaded::session();
    let queue = |control: &mut Control, state: &mut tetrad_app::device::State, queue_type| {
        let message = ProcessDownloadsQueueMessage {
            action: update(),
            queue_type: Some(queue_type),
            ..Default::default()
        };
        let (sent, effects) = loaded::host(control, state, message, at(10));
        assert!(sent.is_empty(), "{sent:?}");
        effects
    };
    assert!(queue(&mut control, &mut state, 0).is_empty());
    state
        .settings
        .update(|settings| settings.account.username = "fake-user".into());
    for (queue_type, kind) in [(0, ProductKind::Preset), (2, ProductKind::NeuralCapture)] {
        let effects = queue(&mut control, &mut state, queue_type);
        let request = Effect::ControlAccount(AccountRequest::DownloadsQueue(kind));
        assert_eq!(effects, [request]);
    }
    control.set_run_options(&loaded::bringup(false));
    let effects = queue(&mut control, &mut state, 0);
    assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
}

/// Diagnostics (7) `sendDiagnostics` `0x23e6d4`, every 1000 ms after CREATE: the last DSP
/// block per core (words 0..3 status, 5 CPU as `f32` bits, 6 heap as `(float)u32`, 12 error
/// flags) and the SOC2 ARM record by offset (byte 0 USB, `0x18` the u64 RX frame count);
/// field 8 is the HID TX failure count.
#[test]
fn diagnostics_send_the_last_dsp_and_soc2_records() {
    use tetrad_control::codecs::DiagnosticsMessage;
    use tetrad_dsp_protocol::DspCore;
    use tetrad_dsp_protocol::responses::{DiagnosticsBlock, Soc2ArmDiagnostics};
    let (mut state, mut control) = loaded::session();
    let mut words = [0; 16];
    words[0] = 1;
    words[5] = 12.5_f32.to_bits();
    words[6] = 2048;
    words[12] = 4;
    control.on_dsp_diagnostics(DspCore::Core1, &DiagnosticsBlock { words });
    let mut record = [0; 128];
    record[0] = 1;
    record[0x18..0x20].copy_from_slice(&77_u64.to_le_bytes());
    let soc2 = Soc2ArmDiagnostics(record);
    control
        .on_soc2_diagnostics(&soc2, &mut state, at(5))
        .unwrap();
    let create = DiagnosticsMessage {
        action: Some(MessageAction::Create.into()),
        ..Default::default()
    };
    loaded::host(&mut control, &mut state, create, at(10));
    let sent = replies(control.tick(&mut state, at(1010)).unwrap()).0;
    let [(ControlMessage::Diagnostics(diagnostics), _)] = &sent[..] else {
        panic!("one Diagnostics push: {sent:?}");
    };
    let core = diagnostics.soc1_core2_diagnostics.as_ref().unwrap();
    assert_eq!(
        (
            core.status1,
            core.process_cpu,
            core.internal_heap_usage,
            core.error_flags
        ),
        (Some(1), Some(12.5), Some(2048.0), Some(4))
    );
    assert_eq!(diagnostics.soc1_core1_diagnostics, Some(Default::default()));
    let arm = diagnostics.soc2_arm_core_diagnostics.as_ref().unwrap();
    assert_eq!(
        (arm.usb_connected, arm.usb_audio_rx_frame),
        (Some(1), Some(77))
    );
    assert_eq!(diagnostics.soc1_to_soc2_dropped_count, Some(0));
}

/// RemoteControl (72) READ with a screenshot (`0x4a0480`): the device renders its window and
/// replies UPDATE with an 8-bit RGB PNG of the whole 800 × 480 frame.
#[test]
fn remote_control_screenshot_sends_the_rendered_window_as_png() {
    use flate2::read::ZlibDecoder;
    use tetrad_control::codecs::{RemoteControlMessage, RemoteControlScreenshot};
    let (mut state, mut control) = loaded::session();
    let read = RemoteControlMessage {
        action: Some(MessageAction::Read.into()),
        screenshot: Some(RemoteControlScreenshot::default()),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, read, at(10));
    assert!(sent.is_empty());
    assert_eq!(effects, [Effect::ControlFrame]);
    let frame = vec![0x40; 800 * 480 * 3];
    let sent = replies(control.send_frame(&frame, &mut state, at(11)).unwrap()).0;
    let [(ControlMessage::RemoteControl(reply), _)] = &sent[..] else {
        panic!("one RemoteControl reply: {sent:?}");
    };
    assert_eq!(reply.action, update());
    let png = reply
        .screenshot
        .as_ref()
        .unwrap()
        .payload
        .as_deref()
        .unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&png[12..16], b"IHDR");
    assert_eq!(&png[16..24], [0, 0, 3, 32, 0, 0, 1, 224]);
    let idat = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
    assert_eq!(&png[37..41], b"IDAT");
    let mut pixels = Vec::new();
    ZlibDecoder::new(&png[41..41 + idat])
        .read_to_end(&mut pixels)
        .unwrap();
    assert_eq!(pixels.len(), 480 * (1 + 800 * 3));
}

/// GigViewButton (55) UPDATE `0x109ab4` goes to the input bridge as one tile press or release
/// (it bounds the tile, r16-perform); an index above 255 is dropped; no reply. GridModelMeter
/// CREATE subscribes the grid meters (`0x110a70`) until DELETE All; the device polls the row
/// meter words while [`Control::grid_meters_subscribed`], and each 100 ms tick sends one Grid
/// UPDATE with a chain per subscribed row: `input_control` parameter 2 = the meter (`0x25da6c`).
#[test]
fn gig_view_button_reaches_the_input_bridge_and_grid_meters_report_the_subscription() {
    use tetrad_control::codecs::{GigViewButtonMessage, GridModelMeterMessage};
    let (mut state, mut control) = loaded::session();
    let button = |index, pressed| GigViewButtonMessage {
        action: update(),
        button_index: Some(index),
        pressed: Some(pressed),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, button(2, true), at(10));
    assert!(sent.is_empty());
    assert_eq!(
        effects,
        [Effect::GigViewButton {
            tile: 2,
            pressed: true
        }]
    );
    let (_, effects) = loaded::host(&mut control, &mut state, button(300, false), at(20));
    assert!(effects.is_empty(), "{effects:?}");

    let meter = |action: MessageAction, row, column| GridModelMeterMessage {
        action: Some(action.into()),
        row: Some(row),
        column: Some(column),
        ..Default::default()
    };
    assert!(!control.grid_meters_subscribed());
    loaded::host(
        &mut control,
        &mut state,
        meter(MessageAction::Create, 1, -1),
        at(30),
    );
    assert!(control.grid_meters_subscribed());
    state.row_meters = [Some(-3.0), Some(-12.5), None, None];
    let (sent, _) = replies(control.tick(&mut state, at(130)).unwrap());
    let [(ControlMessage::Grid(grid), _)] = &sent[..] else {
        panic!("one Grid meter update: {sent:?}");
    };
    let bytes = &grid.preset.as_ref().unwrap().bytes;
    let body = tetrad_preset::Preset::decode(bytes, tetrad_preset::Limits::default()).unwrap();
    let [chain] = &body.chains[..] else {
        panic!("only the subscribed row: {:?}", body.chains);
    };
    let param = &chain.input_control[0].params[0];
    assert_eq!((chain.row, param.index), (Some(1), Some(2)));
    // `0x4df394`: `1 − |max(−40, dB) / −40|` (the 28000 GAIN REDUCTION floor).
    assert_eq!(
        param.values[0].value,
        Some(tetrad_preset::ParamValue::Float(0.6875))
    );
    loaded::host(
        &mut control,
        &mut state,
        meter(MessageAction::Delete, -1, -1),
        at(140),
    );
    assert!(!control.grid_meters_subscribed());
}

/// A device pin pushes a one-model PinnedModels CREATE or DELETE (`0x1304c8`, `0x1300a4`); a
/// device name change pushes Version UPDATE with only `custom_name`, empty when cleared
/// (`customNameChanged` → `0x13cec8`). Other actions push nothing here.
#[test]
fn device_pin_and_name_changes_push_to_control() {
    use tetrad_app::device::actions::Action;
    use tetrad_app::device::control::action_push;
    use tetrad_control::codecs::{PinnedModelsMessage, VersionMessage};
    let pushed = |action| match action_push(&action) {
        Some(Effect::ControlNotify(message)) => Some(*message),
        _ => None,
    };
    assert_eq!(
        pushed(Action::PinModel {
            model: 1,
            pinned: false
        }),
        Some(ControlMessage::PinnedModels(PinnedModelsMessage {
            action: Some(MessageAction::Delete.into()),
            models: vec![1],
            ..Default::default()
        }))
    );
    assert_eq!(
        pushed(Action::SetDeviceName(None)),
        Some(ControlMessage::Version(VersionMessage {
            action: update(),
            custom_name: Some(String::new()),
            ..Default::default()
        }))
    );
    assert!(pushed(Action::SetDeviceName(Some("Stage".into()))).is_some());
    assert_eq!(pushed(Action::CycleMode), None);
}

/// The FileMessageSender pushes of device edits (r16-control MR 4): a setlist rename is
/// UPDATE {folder: old key only, to_folder: the setlist} (`0x252c74`); a slot swap is UPDATE
/// {folder + the preset now at `to`, to_folder + the one at `from`} (`0x2519e8`); an IR delete
/// is DELETE {folder + files[{key}]} for each folder that held it (`0x25270c`); a preset save
/// is UPDATE {setlist + the saved preset} (`0x2515b4`). Nothing is read while Control is off.
#[test]
fn device_directory_edits_and_preset_saves_push_file_messages() {
    use tetrad_app::device::bringup::RunOptions;
    use tetrad_app::device::media::edit_directory;
    use tetrad_app::device::{Library, State};
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_media::factory::DeviceCipher;
    use tetrad_storage::media::{MediaRoot, SETLIST_SLOTS, encode_setlist};
    use tetrad_storage::stock::{DbLimits, ProductType, StockDb};
    use tetrad_ui::{DirectoryRequest, MediaKind};
    let root = std::env::temp_dir().join(format!("tetrad-app-control-push-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let setlist = root.join("Presets/Gig");
    std::fs::create_dir_all(&setlist).unwrap();
    let mut slots = vec![None; SETLIST_SLOTS];
    for (index, name) in [(0, "Clean"), (1, "Lead")] {
        slots[index] = Some(format!("/media/p4/Presets/Gig/{name}.pb"));
        let json = format!(r#"{{"name":"{name}"}}"#);
        std::fs::write(setlist.join(format!("{name}.json")), json).unwrap();
    }
    std::fs::write(setlist.join("setlist"), encode_setlist(&slots).unwrap()).unwrap();
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    database
        .transaction(|write| write.boot_maintenance())
        .unwrap();
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(database),
    });
    control.set_media_cipher(DeviceCipher::caller_secret(b"invented cipher"));
    control.set_wall_clock(Some(
        std::time::UNIX_EPOCH + Duration::from_secs(1_791_635_696),
    ));
    let import = FileMessage {
        action: Some(MessageAction::Create.into()),
        folder: Some(FolderInfo {
            key: Some("local_ir_root".into()),
            files: vec![ProductData {
                name: Some("Room IR".into()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ir_payload: Some(ir_wav(64)),
        ..Default::default()
    };
    loaded::host(&mut control, &mut state, import, at(10));
    let ir = state
        .library
        .as_ref()
        .and_then(|library| library.database.as_ref())
        .map(|database| database.view().product_ids(ProductType::Ir).unwrap())
        .unwrap()
        .remove(0);
    let edit = |control: &Control, state: &mut State, request: DirectoryRequest| {
        let pending = control.file_edit(&request, state);
        edit_directory(request, state, None, &RunOptions::STOCK).unwrap();
        let pushes = control.file_edited(pending, state);
        let files: Vec<FileMessage> = (loaded::notified(&pushes).into_iter())
            .filter_map(|message| match message {
                ControlMessage::File(file) => Some(file),
                _ => None,
            })
            .collect();
        files
    };
    let names = |folder: &Option<FolderInfo>| {
        let folder = folder.as_ref().unwrap();
        let files = folder.files.iter().map(|file| file.name.clone().unwrap());
        (folder.key.clone().unwrap(), files.collect::<Vec<_>>())
    };

    let swap = DirectoryRequest::MovePreset {
        setlist: "Gig".into(),
        from: 0,
        to: 1,
        overwrite: false,
    };
    let [swapped] = &edit(&control, &mut state, swap)[..] else {
        panic!("one swap push");
    };
    let key = "/media/p4/Presets/Gig".to_owned();
    assert_eq!(swapped.action, update());
    assert_eq!(names(&swapped.folder), (key.clone(), vec!["Clean".into()]));
    assert_eq!(
        names(&swapped.to_folder),
        (key.clone(), vec!["Lead".into()])
    );

    let rename = DirectoryRequest::RenameSetlist {
        from: "Gig".into(),
        to: "Tour".into(),
    };
    let [renamed] = &edit(&control, &mut state, rename)[..] else {
        panic!("one rename push");
    };
    assert_eq!(renamed.folder.as_ref().unwrap().key, Some(key));
    let to = renamed.to_folder.as_ref().unwrap();
    assert_eq!(
        (to.key.as_deref(), to.name.as_deref()),
        (Some("/media/p4/Presets/Tour"), Some("Tour"))
    );

    let delete = DirectoryRequest::DeleteMedia {
        kind: MediaKind::Ir,
        id: ir.clone(),
    };
    // `removed` once per folder that held the item: the IRs Library and the user root.
    let deleted = edit(&control, &mut state, delete);
    let keys: Vec<_> = (deleted.iter())
        .map(|push| {
            assert_eq!(
                (push.action, push.file_type),
                (Some(MessageAction::Delete.into()), Some(1))
            );
            let folder = push.folder.as_ref().unwrap();
            assert_eq!(folder.files[0].key.as_deref(), Some(&ir[..]));
            folder.key.clone().unwrap()
        })
        .collect();
    assert_eq!(keys[0], "local_ir_root");
    assert!(keys.len() == 2 && keys[1].ends_with("_q"), "{keys:?}");

    state.current.get_mut().as_mut().unwrap().path = Some("/media/p4/Presets/Tour/Lead.pb".into());
    let pushes = control.preset_saved(&state);
    let [ControlMessage::File(saved)] = &loaded::notified(&pushes)[..] else {
        panic!("one save push: {pushes:?}");
    };
    assert_eq!((saved.action, saved.file_type), (update(), Some(0)));
    let item = &saved.folder.as_ref().unwrap().files[0];
    assert_eq!((item.name.as_deref(), item.index), (Some("Lead"), Some(0)));
    assert!(Control::new().preset_saved(&state).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// LocalBackup (40) `0x17d104` both ways. CREATE suspends the link (`0x3fbb90(0)`) and starts
/// the services job; its file goes out in 150 000-byte UPDATE chunks (`0x17ddfc`) and the last
/// one resumes. A receive opens with `can_apply_backup` (Download suspension), joins the
/// chunks into one restore job and answers `applied_backup` (`0x17dab8`); chunks without a
/// receive answer `can_apply_backup: false` (`0x17cdc8`).
#[test]
fn local_backup_sends_and_receives_the_backup_file() {
    use tetrad_app::device::services::{ControlBackup, ControlBackupDone};
    use tetrad_control::codecs::LocalBackupMessage;
    let (mut state, mut control) = loaded::session();
    let suspends = |sent: &[ControlMessage]| -> Vec<bool> {
        sent.iter()
            .filter_map(|m| match m {
                ControlMessage::SuspendConnection(m) => m.suspend,
                _ => None,
            })
            .collect()
    };
    let backups = |sent: &[ControlMessage]| -> Vec<LocalBackupMessage> {
        sent.iter()
            .filter_map(|m| match m {
                ControlMessage::LocalBackup(m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    };
    let create = LocalBackupMessage {
        action: Some(MessageAction::Create.into()),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, create.clone(), at(10));
    assert_eq!(suspends(&sent), [true]);
    let [Effect::ControlBackup(ControlBackup::Create(header))] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert_eq!(header.creator_version, "4.1.0");
    let (busy, _) = loaded::host(&mut control, &mut state, create, at(20));
    assert_eq!(backups(&busy)[0].can_apply_backup, Some(false));

    let file = "x".repeat(150_001);
    let done = ControlBackupDone::Created { json: file };
    assert!(
        control
            .backup_done(done, &mut state, at(30))
            .unwrap()
            .is_empty()
    );
    let mut out = Vec::new();
    for second in 1..=3 {
        let effects = control.tick(&mut state, at(30 + second * 1_000)).unwrap();
        out.extend(replies(effects).0.into_iter().map(|(message, _)| message));
    }
    let chunks: Vec<_> = backups(&out)
        .into_iter()
        .map(|m| (m.backup_json.unwrap().len(), m.is_last_chunk))
        .collect();
    assert_eq!(chunks, [(150_000, Some(false)), (1, Some(true))]);
    assert_eq!(suspends(&out), [false]);

    let chunk = |text: &str, last| LocalBackupMessage {
        action: update(),
        backup_json: Some(text.into()),
        is_last_chunk: Some(last),
        ..Default::default()
    };
    let (refused, _) = loaded::host(&mut control, &mut state, chunk("{}", true), at(5_000));
    assert_eq!(backups(&refused)[0].can_apply_backup, Some(false));
    let open = LocalBackupMessage {
        action: update(),
        can_apply_backup: Some(true),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, open, at(5_010));
    assert_eq!(backups(&sent)[0].can_apply_backup, Some(true));
    assert_eq!(suspends(&sent), [true]);
    loaded::host(&mut control, &mut state, chunk("{\"a\":", false), at(5_020));
    let (_, effects) = loaded::host(&mut control, &mut state, chunk("1}", true), at(5_030));
    assert_eq!(
        effects,
        [Effect::ControlBackup(ControlBackup::Restore {
            json: "{\"a\":1}".into()
        })]
    );
    let done = ControlBackupDone::Restored { members: 3 };
    let effects = control.backup_done(done, &mut state, at(5_040)).unwrap();
    let sent: Vec<_> = replies(effects).0.into_iter().map(|(m, _)| m).collect();
    assert_eq!(backups(&sent)[0].applied_backup, Some(true));
    assert_eq!(suspends(&sent), [false]);
}

/// File (4) item edits run as device edits, so the stock pushes follow: UPDATE with a file
/// renames an IR (`0x24eecc`), DELETE removes it from a root folder and unlinks it from a
/// sub-folder (`0x1d916c`), and MOVE moves a setlist slot (`0x24b254` → `0x1e8a70`, swap on an
/// occupied slot).
#[test]
fn file_item_edits_and_slot_moves_run_as_directory_edits() {
    use tetrad_app::device::Library;
    use tetrad_app::device::actions::Action;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::stock::{StockLibrary, StockLimits, create_media_folder};
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, ProductType, StockDb};
    use tetrad_ui::{DirectoryRequest, MediaKind};
    let root = std::env::temp_dir().join(format!("tetrad-app-control-item-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets/Gig")).unwrap();
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    database
        .transaction(|write| write.boot_maintenance())
        .unwrap();
    let top = (database.view().root_folder(ProductType::Ir).unwrap()).unwrap();
    let cabs = create_media_folder(&mut database, ProductType::Ir, top, "Cabs").unwrap();
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(database),
    });
    let folder = |key: &str, item: ProductData| FolderInfo {
        key: Some(key.into()),
        files: vec![item],
        ..Default::default()
    };
    let ir = |name: Option<&str>| ProductData {
        key: Some("CIR_1".into()),
        name: name.map(str::to_owned),
        ..Default::default()
    };
    let mut edit = |action: MessageAction, folder: FolderInfo, to: Option<FolderInfo>| {
        let message = FileMessage {
            action: Some(action.into()),
            // Presets for the slot move, IRs for the item edits.
            file_type: Some(i32::from(action != MessageAction::Move)),
            folder: Some(folder),
            to_folder: to,
            ..Default::default()
        };
        let (sent, effects) = loaded::host(&mut control, &mut state, message, at(10));
        assert!(sent.is_empty());
        effects
    };
    assert_eq!(
        edit(
            MessageAction::Update,
            folder("local_ir_root", ir(Some("Room"))),
            None
        ),
        [Effect::Directory(DirectoryRequest::RenameMedia {
            kind: MediaKind::Ir,
            id: "CIR_1".into(),
            name: "Room".into(),
        })]
    );
    assert_eq!(
        edit(
            MessageAction::Delete,
            folder("local_ir_root", ir(None)),
            None
        ),
        [Effect::Directory(DirectoryRequest::DeleteMedia {
            kind: MediaKind::Ir,
            id: "CIR_1".into(),
        })]
    );
    assert_eq!(
        edit(
            MessageAction::Delete,
            folder(&format!("{cabs}_q"), ir(None)),
            None
        ),
        [Effect::Action(Action::LinkMedia {
            folder: cabs,
            products: vec!["CIR_1".into()],
            linked: false,
        })]
    );
    let slot = |index| ProductData {
        index: Some(index),
        ..Default::default()
    };
    let gig = "/media/p4/Presets/Gig";
    assert_eq!(
        edit(
            MessageAction::Move,
            folder(gig, slot(0)),
            Some(folder(gig, slot(3)))
        ),
        [Effect::Directory(DirectoryRequest::MovePreset {
            setlist: "Gig".into(),
            from: 0,
            to: 3,
            overwrite: false,
        })]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// DefaultParameters (19) CREATE `0x6684c8`: the block at row/column stores its current rows
/// as its model's defaults through the "Set Parameters as Defaults" action; the reply is
/// UPDATE with row and column, also for an empty slot, which stores nothing.
#[test]
fn default_parameters_create_stores_the_block_defaults_and_replies_its_position() {
    use tetrad_app::device::actions::Action;
    use tetrad_control::codecs::{DefaultParametersMessage, GridMessage};
    let (mut state, mut control) = loaded::session();
    let block = tetrad_preset::Model {
        hash: Some(1),
        column: Some(2),
        ..Default::default()
    };
    let add: GridMessage = grid::message(MessageAction::Update, grid::block(1, block));
    let (_, effects) = loaded::host(&mut control, &mut state, add, at(0));
    loaded::commit(&mut state, effects);
    let create = |row, column| DefaultParametersMessage {
        action: Some(MessageAction::Create.into()),
        row: Some(row),
        column: Some(column),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, create(1, 2), at(10));
    let [ControlMessage::DefaultParameters(reply)] = &sent[..] else {
        panic!("{sent:?}");
    };
    assert_eq!(
        (reply.action, reply.row, reply.column),
        (update(), Some(1), Some(2))
    );
    assert!(matches!(
        &effects[..],
        [Effect::Action(Action::SetDefaultParams { key, .. })] if key == "1"
    ));
    let (sent, effects) = loaded::host(&mut control, &mut state, create(0, 0), at(20));
    assert_eq!(sent.len(), 1);
    assert!(effects.is_empty(), "{effects:?}");
}

/// File (4) UPDATE of a folder without files renames it to `folder.name` (`0x24f750` →
/// `0x248548`); DELETE removes it, a `zenui.db` folder with its products when
/// `delete_from_library` (`0x77df68`). Both run as device Directory edits, so the device
/// pushes follow them; a media folder DELETE goes between the 57 pushes of `0x77df68`
/// (`operation_type` the folder type, 1 for IRs); a media root is refused.
#[test]
fn file_update_and_delete_of_a_folder_run_as_directory_edits() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo};
    use tetrad_library::stock::{StockLibrary, StockLimits, create_media_folder};
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, ProductType, StockDb};
    use tetrad_ui::DirectoryRequest;
    let root = std::env::temp_dir().join(format!("tetrad-app-control-edit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets/Gig")).unwrap();
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    database
        .transaction(|write| write.boot_maintenance())
        .unwrap();
    let top = (database.view().root_folder(ProductType::Ir).unwrap()).unwrap();
    let cabs = create_media_folder(&mut database, ProductType::Ir, top, "Cabs").unwrap();
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(database),
    });
    let request = |action: MessageAction, key: &str, name: Option<&str>| FileMessage {
        action: Some(action.into()),
        folder: Some(FolderInfo {
            key: Some(key.into()),
            name: name.map(str::to_owned),
            ..Default::default()
        }),
        delete_from_library: Some(true),
        ..Default::default()
    };
    let mut edit = |message| {
        let (sent, effects) = loaded::host(&mut control, &mut state, message, at(10));
        assert!(sent.is_empty());
        effects
    };
    let rename = request(MessageAction::Update, "/media/p4/Presets/Gig", Some("Tour"));
    assert_eq!(
        edit(rename),
        [Effect::Directory(DirectoryRequest::RenameSetlist {
            from: "Gig".into(),
            to: "Tour".into()
        })]
    );
    let delete = request(MessageAction::Delete, &format!("{cabs}_q"), None);
    let effects = edit(delete);
    let [start, delete, finished] = &effects[..] else {
        panic!("57 CREATE, the edit, 57 DELETE: {effects:?}");
    };
    assert_eq!(
        *delete,
        Effect::Directory(DirectoryRequest::DeleteFolder {
            id: cabs,
            remove_products: true
        })
    );
    let notified = loaded::notified(&[start.clone(), finished.clone()]);
    let [
        ControlMessage::BulkOperation(start),
        ControlMessage::BulkOperation(finished),
    ] = &notified[..]
    else {
        panic!("{notified:?}");
    };
    assert_eq!(
        (start.progress_message.as_deref(), start.cancellable),
        (Some("Removing, please wait."), Some(false))
    );
    let source = start.source_folder.as_ref().unwrap();
    assert_eq!(
        (start.operation_type, source.key.as_deref()),
        (Some(1), Some(format!("{cabs}_q").as_str()))
    );
    assert_eq!(finished.finished, Some(true));
    let root_folder = request(MessageAction::Delete, "local_ir_root", None);
    assert!(matches!(edit(root_folder)[..], [Effect::Notice(..)]));
    let _ = std::fs::remove_dir_all(&root);
}

/// ModelPreset (71) CREATE `0x1f8b9c` stores the grid block at `create_from_row`/`_column`
/// under `presets[0].name` and pushes CREATE with the stored entry (`0x11d950`); the same name
/// for the same model overwrites it and pushes UPDATE (`0x11cf1c`). A new name with the values
/// of a stored preset is a "Preset Conflict" (`0x1f867c` → `0x1f7f50`): nothing is stored or
/// pushed, and `0x397ff8(model, 1)` sends the Confirmation (47) to Control (`0xfa48c`); its
/// answer runs empty callbacks. Global EQ (4004) needs its catalog model.
#[test]
fn model_preset_create_stores_the_grid_block_and_overwrites_by_name() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{
        ConfirmationAction, ConfirmationMessage, GridMessage, ModelPreset, ModelPresetMessage,
    };
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, StockDb};
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-vdp-new-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    database
        .transaction(|write| write.boot_maintenance())
        .unwrap();
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(database),
    });
    let block = tetrad_preset::Model {
        hash: Some(1),
        column: Some(2),
        ..Default::default()
    };
    let add: GridMessage = grid::message(MessageAction::Update, grid::block(1, block));
    let (_, effects) = loaded::host(&mut control, &mut state, add, at(0));
    loaded::commit(&mut state, effects);
    let create = |hash| ModelPresetMessage {
        action: Some(MessageAction::Create.into()),
        create_from_row: Some(1),
        create_from_column: Some(2),
        create_from_hash: Some(hash),
        presets: vec![ModelPreset {
            name: Some("Mine".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut pushed = |message| {
        let (sent, effects) = loaded::host(&mut control, &mut state, message, at(10));
        assert!(sent.is_empty());
        let [ControlMessage::ModelPreset(push)] = &loaded::notified(&effects)[..] else {
            panic!("one ModelPreset push: {effects:?}");
        };
        push.clone()
    };
    let first = pushed(create(1));
    let entry = &first.presets[0];
    assert_eq!(first.action, Some(MessageAction::Create.into()));
    assert_eq!(
        (entry.name.as_deref(), entry.id.as_ref().unwrap().hash),
        (Some("Mine"), Some(1))
    );
    let again = pushed(create(1));
    assert_eq!(again.action, update());
    assert_eq!(again.presets[0].id, entry.id);
    let mut other = create(1);
    other.presets[0].name = Some("Other".into());
    let (sent, effects) = loaded::host(&mut control, &mut state, other, at(15));
    let [ControlMessage::Confirmation(conflict)] = &sent[..] else {
        panic!("one Preset Conflict confirmation: {sent:?} {effects:?}");
    };
    assert!(loaded::notified(&effects).is_empty(), "{effects:?}");
    assert_eq!(conflict.title.as_deref(), Some("Preset Conflict"));
    let text = conflict.msg.as_deref().unwrap_or_default();
    assert!(
        text.starts_with("The Virtual Device Preset could not be saved"),
        "{text}"
    );
    assert_eq!(conflict.actions, [ConfirmationAction::Ok.into()]);
    assert_eq!(
        (
            conflict.ok_title.as_deref(),
            conflict.cancel_title.as_deref()
        ),
        (Some("GOT IT"), None)
    );
    let answer = ConfirmationMessage {
        confirmation_id: conflict.confirmation_id,
        response: Some(ConfirmationAction::Ok.into()),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, answer, at(16));
    assert!(
        sent.is_empty() && effects.is_empty(),
        "{sent:?} {effects:?}"
    );
    let (_, effects) = loaded::host(&mut control, &mut state, create(4004), at(20));
    assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// DefaultParameters (19) UPDATE `0x15eb0c` → `0x66824c` → `0x15d74c`: without stored
/// `default_param` rows the block at row/column takes the catalog defaults (`0x6678d4`) at the
/// current scene, and each written parameter echoes as a held Grid value (`0x523644` →
/// `0x25d51c`). No reply; an empty slot does nothing.
#[test]
fn default_parameters_update_resets_the_block_to_its_defaults_and_echoes_it() {
    use tetrad_control::codecs::{DefaultParametersMessage, GridMessage, MessageAction::Update};
    use tetrad_preset::{Model, ParamValue, Slot};
    let (mut state, mut control) = loaded::session();
    let gain = |state: &tetrad_app::device::State| {
        let preset = &state.current.as_ref().unwrap().preset;
        let block = preset.block(Slot { row: 1, column: 2 }).unwrap().unwrap();
        block.params[0].values[0].value.clone()
    };
    let block = |params| Model {
        hash: Some(1),
        column: Some(2),
        params,
        ..Default::default()
    };
    let add: GridMessage = grid::message(Update, grid::block(1, block(Vec::new())));
    let (_, effects) = loaded::host(&mut control, &mut state, add, at(0));
    loaded::commit(&mut state, effects);
    let default = gain(&state);
    let edit = grid::message(Update, grid::block(1, block(vec![grid::param(0, 0.9)])));
    let (_, effects) = loaded::host(&mut control, &mut state, edit, at(1000));
    loaded::commit(&mut state, effects);
    grid::tick(&mut control, &mut state, at(1200));
    assert_ne!(gain(&state), default);
    let reset = |row, column| DefaultParametersMessage {
        action: update(),
        row: Some(row),
        column: Some(column),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, reset(1, 2), at(2000));
    assert!(sent.is_empty(), "{sent:?}");
    loaded::commit(&mut state, effects);
    assert_eq!(gain(&state), default);
    let echo = grid::tick(&mut control, &mut state, at(2100));
    let [message] = &echo[..] else {
        panic!("one held echo: {echo:?}");
    };
    let echoed = &grid::sent(message).0.chains[0].models[0].params[0];
    assert_eq!(echoed.index, Some(0));
    assert!(matches!(echoed.values[0].value, Some(ParamValue::Float(_))));
    let (sent, effects) = loaded::host(&mut control, &mut state, reset(0, 0), at(3000));
    assert!(sent.is_empty() && effects.is_empty(), "{effects:?}");
}

/// GeneralSettings READ (`0x16c964`) reports `lock_screen_and_volume_knob` from the input mask
/// (`0x5447b8`: bit 0 clear), which an update install clears (`0x1ec83c(0x3f)`) and reports
/// with the `0x16ad60` push. The flag follows the push even while Control is disconnected. Be
/// Right Back clears bit 0 without that push (`0x1ec83c(3)`): READ reports the lock from its
/// Standby push until the WakeUp push (`0x16b758`).
#[test]
fn general_settings_read_reports_the_install_lock() {
    use tetrad_control::codecs::{GeneralSettingsMessage, PowerOptions};
    use tetrad_control::link::Notification;
    let (mut state, mut control) = loaded::session();
    let lock = |locked| {
        Notification::Update(ControlMessage::GeneralSettings(GeneralSettingsMessage {
            action: update(),
            lock_screen_and_volume_knob: Some(locked),
            ..Default::default()
        }))
    };
    control.notify(lock(true), &mut state, at(10)).unwrap();
    let read = GeneralSettingsMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read.clone(), at(20));
    let [ControlMessage::GeneralSettings(reply)] = &sent[..] else {
        panic!("one GeneralSettings reply: {sent:?}");
    };
    assert_eq!(reply.lock_screen_and_volume_knob, Some(true));
    control.notify(lock(false), &mut state, at(30)).unwrap();
    let (sent, _) = loaded::host(&mut control, &mut state, read.clone(), at(40));
    let [ControlMessage::GeneralSettings(reply)] = &sent[..] else {
        panic!("one GeneralSettings reply: {sent:?}");
    };
    assert_eq!(reply.lock_screen_and_volume_knob, Some(false));
    for (option, locked, now) in [
        (PowerOptions::Standby, true, 50),
        (PowerOptions::WakeUp, false, 70),
    ] {
        let standby = GeneralSettingsMessage {
            action: update(),
            power_option: Some(option.into()),
            ..Default::default()
        };
        let notification = Notification::Update(ControlMessage::GeneralSettings(standby));
        control.notify(notification, &mut state, at(now)).unwrap();
        let (sent, _) = loaded::host(&mut control, &mut state, read.clone(), at(now + 10));
        let [ControlMessage::GeneralSettings(reply)] = &sent[..] else {
            panic!("one GeneralSettings reply: {sent:?}");
        };
        assert_eq!(
            reply.lock_screen_and_volume_knob,
            Some(locked),
            "{option:?}"
        );
    }
}

/// IOSettings UPDATE `preset_to_load` (`0x3634e8`): a stored I/O preset becomes a fresh 31000
/// block with its rows (`0x1f7e74`) and loads whole (`0x362d18`) through
/// `Action::LoadSettingsPreset`; a parameter without a row takes its catalog default, and the
/// EXP calibration rows 48..=51 (raw ADC counts for `0x320fdc`) pass unchanged. The special
/// factory id loads the factory default (`0x3615a0`, `Action::LoadFactoryIoPreset`). The full
/// push `0x26ebc0` is the READ dump as UPDATE.
#[test]
fn io_preset_load_from_control_loads_the_whole_preset_block() {
    use tetrad_app::device::Library;
    use tetrad_app::device::actions::{Action, reduce};
    use tetrad_control::codecs::{IOSettingsMessage, ModelPresetID};
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, ExpSwitch, ParamEntry, StockDb};
    let root = std::env::temp_dir().join(format!("tetrad-app-control-io-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    database
        .transaction(|write| write.boot_maintenance())
        .unwrap();
    let io = "<Category id='31' name='IO' hidden='true'><Model id='31000' name='IOSettings'>\
<Parameter name='IN 1 LEVEL' type='float' min='0' max='1' defaultValue='0.25'/>\
<Parameter name='IN 2 LEVEL' type='float' min='0' max='1' defaultValue='0.5'/></Model></Category>";
    let (mut state, mut control) = loaded::session_with_extra(io, "");
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(database),
    });
    let row = |index, value: &str| ParamEntry {
        index,
        value: value.into(),
        exp: 0,
        exp_trim_max: 1.0,
        exp_trim_min: 0.0,
    };
    let switch = ExpSwitch {
        switch_type: 2,
        inverted: false,
        latch_emulation: false,
        delay_ms: 600,
    };
    let create = Action::CreateDevicePreset {
        model: 31_000,
        name: "Stage".into(),
        switch,
        params: vec![row(0, "0.75"), row(48, "200"), row(49, "20")],
    };
    reduce(create, &mut state).unwrap();
    let database = state.library.as_ref().unwrap().database.as_ref().unwrap();
    let id = database.view().model_presets().unwrap()[0].id;
    let load = |value: String| IOSettingsMessage {
        action: update(),
        preset_to_load: Some(ModelPresetID {
            value: Some(value),
            is_factory: Some(false),
            hash: Some(31_000),
        }),
        ..Default::default()
    };
    let (_, effects) = loaded::host(&mut control, &mut state, load(id.to_string()), at(10));
    let [Effect::Action(Action::LoadSettingsPreset { model, rows })] = &effects[..] else {
        panic!("one settings preset load: {effects:?}");
    };
    let values: Vec<(i64, &str)> = (rows.iter())
        .map(|row| (row.index, row.value.as_str()))
        .collect();
    let want = vec![(0, "0.75"), (1, "0.5"), (48, "200"), (49, "20")];
    assert_eq!((*model, values), (31_000, want));
    let special = load("SpecialFactoryModelPresetID".into());
    let (_, effects) = loaded::host(&mut control, &mut state, special, at(20));
    assert!(
        matches!(effects[..], [Effect::Action(Action::LoadFactoryIoPreset)]),
        "{effects:?}"
    );
    let read = IOSettingsMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read, at(30));
    let [ControlMessage::IOSettings(dump)] = &sent[..] else {
        panic!("one READ dump: {sent:?}");
    };
    let Effect::ControlNotify(push) = tetrad_app::device::control::io::push(&state) else {
        panic!("a Control push");
    };
    let ControlMessage::IOSettings(push) = *push else {
        panic!("an IOSettings push: {push:?}");
    };
    assert_eq!((push.action, &push.settings), (update(), &dump.settings));
    let _ = std::fs::remove_dir_all(&root);
}

/// File (4) MOVE `0x24b804` and COPY `0x24c62c` between setlists (`0x24b254` → `0x1e8a70`)
/// copy into `to_folder.files[0].index` (an occupied slot is overwritten: the force flag skips
/// the dialog) only when `to_folder` holds exactly one item (else the first empty slot), and a
/// MOVE deletes the source; the changed setlists go out as UPDATE (I). The source item is
/// named by its key (`0x24b804` reads `files[0].key`), as stock sends it. The item onto its own
/// slot does nothing.
#[test]
fn file_copies_and_moves_presets_into_the_target_slot() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::sidecar::Sidecar;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    let root = std::env::temp_dir().join(format!("tetrad-app-control-copy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets/Gig")).unwrap();
    std::fs::create_dir_all(root.join("Presets/Tour")).unwrap();
    let (mut state, mut control) = loaded::session();
    let mut presets = StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap();
    let mut lead = state.current.as_ref().unwrap().preset.clone();
    lead.name = Some("Lead".into());
    let document =
        tetrad_preset::PresetDocument::parse(&lead.to_bytes(), tetrad_preset::Limits::default())
            .unwrap();
    for (setlist, slot) in [("Gig", 2), ("Gig", 0)] {
        let _ = (presets.store(setlist, slot, "Lead", &document, &Sidecar::default())).unwrap();
    }
    state.library = Some(Library {
        presets,
        database: None,
    });
    let key = |setlist: &str| format!("/media/p4/Presets/{setlist}");
    let folder = |setlist: &str, index: i32, name: Option<&str>| FolderInfo {
        key: Some(key(setlist)),
        files: vec![ProductData {
            index: Some(index),
            name: name.map(str::to_owned),
            ..Default::default()
        }],
        ..Default::default()
    };
    let message = |action: MessageAction, folder, to| FileMessage {
        action: Some(action.into()),
        file_type: Some(0),
        folder: Some(folder),
        to_folder: to,
        ..Default::default()
    };
    let mut edit = |state: &mut tetrad_app::device::State, action: MessageAction, folder, to| {
        let message = message(action, folder, to);
        let (sent, effects) = loaded::host(&mut control, state, message, at(10));
        assert!(sent.is_empty(), "{sent:?}");
        effects
    };
    let pushed = |effects: &[Effect]| -> Vec<String> {
        (loaded::notified(effects).into_iter())
            .filter_map(|message| match message {
                ControlMessage::File(file) if file.action == update() => file.folder?.key,
                _ => None,
            })
            .collect()
    };
    let slots_of = |state: &tetrad_app::device::State, setlist: &str| {
        let library = &state.library.as_ref().unwrap().presets;
        library.slots(setlist).unwrap()
    };
    let slots = |state: &tetrad_app::device::State, setlist: &str| {
        let slots = slots_of(state, setlist);
        [0, 1, 2, 3].map(|slot| slots[slot].is_some())
    };
    let lead = slots_of(&state, "Gig")[2].clone();
    let by_key = FolderInfo {
        key: Some(key("Gig")),
        files: vec![ProductData {
            key: lead,
            ..Default::default()
        }],
        ..Default::default()
    };
    let moved = edit(
        &mut state,
        MessageAction::Move,
        by_key,
        Some(folder("Tour", 3, None)),
    );
    assert_eq!(pushed(&moved), [key("Tour"), key("Gig")]);
    assert_eq!(slots(&state, "Tour"), [false, false, false, true]);
    assert_eq!(slots(&state, "Gig"), [true, false, false, false]);
    let copied = edit(
        &mut state,
        MessageAction::Copy,
        folder("Tour", 3, None),
        Some(folder("Gig", 0, None)),
    );
    assert_eq!(pushed(&copied), [key("Gig")]);
    assert_eq!(
        slots(&state, "Gig"),
        [true, false, false, false],
        "overwritten in place"
    );
    let mut two = folder("Gig", 0, None);
    two.files.push(ProductData {
        index: Some(3),
        ..Default::default()
    });
    let _ = edit(
        &mut state,
        MessageAction::Copy,
        folder("Tour", 3, None),
        Some(two),
    );
    assert_eq!(
        slots(&state, "Gig"),
        [true, true, false, false],
        "two target items: no index, the first empty slot"
    );
    let own = edit(
        &mut state,
        MessageAction::Copy,
        folder("Tour", 3, None),
        Some(folder("Tour", 3, None)),
    );
    assert!(own.is_empty(), "{own:?}");
    // `0x24c640`: a COPY without `file_type` (has-bit `0x20`) writes nothing.
    let untyped = FileMessage {
        file_type: None,
        ..message(
            MessageAction::Copy,
            folder("Tour", 3, None),
            Some(folder("Gig", 2, None)),
        )
    };
    let (_, effects) = loaded::host(&mut control, &mut state, untyped, at(20));
    assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
    assert_eq!(slots(&state, "Gig"), [true, true, false, false]);
    let _ = std::fs::remove_dir_all(&root);
}

/// File SWAP (8): the head `0x24d2d8` needs `file_type` 0, one keyed source item and one
/// indexed target item (review r29 B2); else nothing is written. Then `0x24b254` →
/// `0x1e8a70` with swap set and force clear. Onto an empty slot the drop copies (`0x1de9f8`
/// keeps the source without `move`), in any setlist; onto an occupied slot the two presets
/// exchange places (copy-swap `0x1e5a58`): in one setlist as a Directory move, between two
/// setlists as new files in the other setlist with the swap push `0x2519e8`. Bring-up without
/// `--persist` refuses the copy and the swap before they write.
#[test]
fn file_swap_copies_onto_an_empty_slot_and_exchanges_occupied_ones() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::sidecar::Sidecar;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    use tetrad_ui::DirectoryRequest;
    let root = std::env::temp_dir().join(format!("tetrad-app-control-swap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets/Gig")).unwrap();
    std::fs::create_dir_all(root.join("Presets/Tour")).unwrap();
    let (mut state, mut control) = loaded::session();
    let mut presets = StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap();
    for (slot, name) in [(0, "Solo"), (2, "Lead")] {
        let mut preset = state.current.as_ref().unwrap().preset.clone();
        preset.name = Some(name.into());
        let bytes = preset.to_bytes();
        let document =
            tetrad_preset::PresetDocument::parse(&bytes, tetrad_preset::Limits::default());
        let stored = presets.store("Gig", slot, name, &document.unwrap(), &Sidecar::default());
        let _ = stored.unwrap();
    }
    state.library = Some(Library {
        presets,
        database: None,
    });
    let key = |setlist: &str| format!("/media/p4/Presets/{setlist}");
    let folder = |setlist: &str, index: i32| FolderInfo {
        key: Some(key(setlist)),
        files: vec![ProductData {
            index: Some(index),
            ..Default::default()
        }],
        ..Default::default()
    };
    let paths = |state: &tetrad_app::device::State, setlist: &str| {
        let library = &state.library.as_ref().unwrap().presets;
        library.slots(setlist).unwrap()
    };
    // Stock sends the source item by key.
    let item = |state: &tetrad_app::device::State, setlist: &str, slot: usize| FolderInfo {
        key: Some(key(setlist)),
        files: vec![ProductData {
            key: paths(state, setlist)[slot].clone(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let send = |control: &mut Control, state: &mut tetrad_app::device::State, message| {
        let (sent, effects) = loaded::host(control, state, message, at(10));
        assert!(sent.is_empty(), "{sent:?}");
        effects
    };
    let message = |from, to| FileMessage {
        action: Some(MessageAction::Swap.into()),
        file_type: Some(0),
        folder: Some(from),
        to_folder: Some(to),
        ..Default::default()
    };
    let swap = |control: &mut Control, state: &mut tetrad_app::device::State, from, to| {
        let (setlist, slot) = from;
        let from = item(state, setlist, slot);
        send(control, state, message(from, to))
    };
    let slots = |state: &tetrad_app::device::State, setlist: &str| {
        let slots = paths(state, setlist);
        [0, 1, 2, 3].map(|slot| slots[slot].is_some())
    };
    // The SWAP head (`0x24d2e4`–`0x24d3b0`): no target index, no `file_type`, a type other
    // than presets, no source key: nothing is written (no first-empty copy).
    let unindexed = FolderInfo {
        files: vec![ProductData::default()],
        ..folder("Tour", 0)
    };
    let heads = [
        message(item(&state, "Gig", 2), unindexed),
        FileMessage {
            file_type: None,
            ..message(item(&state, "Gig", 2), folder("Tour", 1))
        },
        FileMessage {
            file_type: Some(1),
            ..message(item(&state, "Gig", 2), folder("Tour", 1))
        },
        message(folder("Gig", 2), folder("Tour", 1)),
        message(
            item(&state, "Gig", 2),
            FolderInfo {
                files: [folder("Tour", 1).files, folder("Tour", 2).files].concat(),
                ..folder("Tour", 1)
            },
        ),
    ];
    for head in heads {
        let refused = send(&mut control, &mut state, head);
        assert!(matches!(refused[..], [Effect::Notice(..)]), "{refused:?}");
        assert_eq!(slots(&state, "Tour"), [false; 4]);
    }
    let copied = swap(&mut control, &mut state, ("Gig", 2), folder("Tour", 1));
    let pushed: Vec<_> = (loaded::notified(&copied).into_iter())
        .filter_map(|message| match message {
            ControlMessage::File(file) => file.folder?.key,
            _ => None,
        })
        .collect();
    assert_eq!(pushed, [key("Tour")]);
    assert_eq!(slots(&state, "Tour"), [false, true, false, false]);
    assert_eq!(
        slots(&state, "Gig"),
        [true, false, true, false],
        "the source stays"
    );
    let name = |state: &tetrad_app::device::State, setlist: &str, slot: usize| {
        let library = &state.library.as_ref().unwrap().presets;
        let path = paths(state, setlist)[slot].clone().unwrap();
        library
            .load(&path)
            .unwrap()
            .document
            .summary()
            .unwrap()
            .name
    };
    let names = |state: &tetrad_app::device::State| (name(state, "Gig", 0), name(state, "Tour", 1));
    assert_eq!(names(&state), (Some("Solo".into()), Some("Lead".into())));
    // `0x1e5a58` → `0x1e3824`: the two presets change setlists (`Action::SwapPresets`).
    let across = swap(&mut control, &mut state, ("Gig", 0), folder("Tour", 1));
    assert_eq!(names(&state), (Some("Lead".into()), Some("Solo".into())));
    let [ControlMessage::File(push)] = &loaded::notified(&across)[..] else {
        panic!("one swap push: {across:?}");
    };
    let side = |info: &Option<FolderInfo>| {
        let info = info.as_ref().unwrap();
        (info.key.clone().unwrap(), info.files[0].index)
    };
    assert_eq!(
        (side(&push.folder), side(&push.to_folder)),
        ((key("Gig"), Some(0)), (key("Tour"), Some(1))),
        "the swap push `0x2519e8`"
    );
    assert_eq!(
        swap(&mut control, &mut state, ("Gig", 0), folder("Gig", 2)),
        [Effect::Directory(DirectoryRequest::MovePreset {
            setlist: "Gig".into(),
            from: 0,
            to: 2,
            overwrite: false,
        })]
    );
    control.set_run_options(&loaded::bringup(false));
    let refused = swap(&mut control, &mut state, ("Gig", 0), folder("Tour", 1));
    assert!(matches!(refused[..], [Effect::Notice(..)]), "{refused:?}");
    assert_eq!(names(&state), (Some("Lead".into()), Some("Solo".into())));
    let refused = swap(&mut control, &mut state, ("Gig", 0), folder("Tour", 3));
    assert!(matches!(refused[..], [Effect::Notice(..)]), "{refused:?}");
    assert_eq!(slots(&state, "Tour"), [false, true, false, false]);
    let _ = std::fs::remove_dir_all(&root);
}

/// A9 (r53): File MOVE or COPY of a Downloads capture (`cloud-2-1`, keyed by its hash) into a
/// user capture folder (`0x1e8a70` → `0x1d9954` → `0x1d94d4`, MOVE and COPY alike): the folder
/// gets the capture, the Downloads row goes, and both folders go out as UPDATE. A SWAP and
/// an IR target are refused with nothing written.
#[test]
fn a_move_out_of_the_downloads_captures_stores_the_capture() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::stock::{StockLibrary, StockLimits, create_media_folder};
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{
        CaptureEncryption, CaptureStore, DbLimits, Download, DownloadKind, ProductType, StockDb,
    };
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-dlmove-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    database
        .transaction(|write| {
            write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
            write.boot_maintenance()?;
            for index in [1, 2] {
                write.put_download(
                    DownloadKind::Capture,
                    &Download {
                        id: format!("cloud-{index}"),
                        hash: Some(format!("NC_{index}")),
                        raw_data: Some(vec![index; 16]),
                        metadata: Some(b"metadata".to_vec()),
                    },
                )?;
            }
            Ok(())
        })
        .unwrap();
    let captures = (database.view().root_folder(ProductType::Capture).unwrap()).unwrap();
    let gig = create_media_folder(&mut database, ProductType::Capture, captures, "Gig").unwrap();
    let irs = (database.view().root_folder(ProductType::Ir).unwrap()).unwrap();
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(database),
    });
    let message = |action: MessageAction, item: &str, to: i64| FileMessage {
        action: Some(action.into()),
        file_type: Some(2),
        folder: Some(FolderInfo {
            key: Some("cloud-2-1".into()),
            files: vec![ProductData {
                key: Some(item.into()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        to_folder: Some(FolderInfo {
            key: Some(format!("{to}_q")),
            ..Default::default()
        }),
        ..Default::default()
    };
    let downloads = |state: &tetrad_app::device::State| {
        let database = state.library.as_ref().unwrap().database.as_ref().unwrap();
        database.view().download_ids(DownloadKind::Capture).unwrap()
    };
    for refused in [
        message(MessageAction::Swap, "NC_1", gig),
        message(MessageAction::Move, "NC_1", irs),
    ] {
        let (_, effects) = loaded::host(&mut control, &mut state, refused, at(10));
        assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
    }
    assert_eq!(downloads(&state), ["cloud-1", "cloud-2"]);

    let (sent, effects) = loaded::host(
        &mut control,
        &mut state,
        message(MessageAction::Move, "NC_1", gig),
        at(20),
    );
    assert!(sent.is_empty(), "{sent:?}");
    assert!(effects.contains(&Effect::RefreshDirectory));
    let pushed: Vec<_> = (loaded::notified(&effects).into_iter())
        .filter_map(|message| match message {
            ControlMessage::File(file) => file.folder?.key,
            _ => None,
        })
        .collect();
    assert_eq!(pushed, [format!("{gig}_q"), "cloud-2-1".to_owned()]);
    loaded::host(
        &mut control,
        &mut state,
        message(MessageAction::Copy, "NC_2", gig),
        at(30),
    );
    let database = state.library.as_ref().unwrap().database.as_ref().unwrap();
    let view = database.view();
    assert_eq!(view.folder_products(gig).unwrap(), ["NC_1", "NC_2"]);
    assert_eq!(
        view.product("NC_2").unwrap().unwrap().raw_data,
        Some(vec![2; 16])
    );
    assert!(view.download_ids(DownloadKind::Capture).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// A9: File MOVE or COPY of a Downloads preset (`cloud-0-1`, keyed by its cloud id) into a
/// user setlist (`0x1e8a70` → `0x1dd61c` → `0x1dcf2c` → `0x742990`; MOVE and COPY alike):
/// the slot gets the preset with its row's sidecar, then the Downloads row goes (`0x1dc14c`),
/// and both folders go out as UPDATE. No target index: the first empty slot. A SWAP and an
/// occupied slot are refused with nothing written.
#[test]
fn a_downloads_preset_moves_into_a_setlist_slot() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::sidecar::Sidecar;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, Download, DownloadKind, StockDb};
    let root = std::env::temp_dir().join(format!(
        "tetrad-app-control-dlpreset-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets/Gig")).unwrap();
    let (mut state, mut control) = loaded::session();
    let mut preset = state.current.as_ref().unwrap().preset.clone();
    let mut presets = StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap();
    preset.name = Some("Held".into());
    let document =
        tetrad_preset::PresetDocument::parse(&preset.to_bytes(), tetrad_preset::Limits::default())
            .unwrap();
    let held = (presets.store("Gig", 1, "Held", &document, &Sidecar::default()))
        .unwrap()
        .value;
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    database
        .transaction(|write| {
            write.boot_maintenance()?;
            for name in ["Lead", "Rhythm"] {
                preset.name = Some(name.into());
                let mut sidecar = Sidecar::default();
                sidecar.set_text("cloudId", &format!("cloud-{name}"));
                write.put_download(
                    DownloadKind::Preset,
                    &Download {
                        id: format!("cloud-{name}"),
                        hash: None,
                        raw_data: Some(preset.to_bytes()),
                        metadata: Some(sidecar.encode()),
                    },
                )?;
            }
            Ok(())
        })
        .unwrap();
    state.library = Some(Library {
        presets,
        database: Some(database),
    });
    let gig = "/media/p4/Presets/Gig";
    let message = |action: MessageAction, item: &str, index: Option<i32>| FileMessage {
        action: Some(action.into()),
        file_type: Some(0),
        folder: Some(FolderInfo {
            key: Some("cloud-0-1".into()),
            files: vec![ProductData {
                key: Some(item.into()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        to_folder: Some(FolderInfo {
            key: Some(gig.into()),
            files: vec![ProductData {
                index,
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let downloads = |state: &tetrad_app::device::State| {
        let database = state.library.as_ref().unwrap().database.as_ref().unwrap();
        database.view().download_ids(DownloadKind::Preset).unwrap()
    };
    let slots = |state: &tetrad_app::device::State| {
        state
            .library
            .as_ref()
            .unwrap()
            .presets
            .slots("Gig")
            .unwrap()
    };
    for refused in [
        message(MessageAction::Swap, "cloud-Lead", Some(0)),
        message(MessageAction::Move, "cloud-Lead", Some(1)),
    ] {
        let (_, effects) = loaded::host(&mut control, &mut state, refused, at(10));
        assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
    }
    assert_eq!(downloads(&state), ["cloud-Lead", "cloud-Rhythm"]);
    assert_eq!(slots(&state)[..3], [None, Some(held.clone()), None]);

    let (sent, effects) = loaded::host(
        &mut control,
        &mut state,
        message(MessageAction::Move, "cloud-Lead", Some(0)),
        at(20),
    );
    assert!(sent.is_empty(), "{sent:?}");
    assert!(effects.contains(&Effect::RefreshDirectory));
    let pushed: Vec<_> = (loaded::notified(&effects).into_iter())
        .filter_map(|message| match message {
            ControlMessage::File(file) => file.folder?.key,
            _ => None,
        })
        .collect();
    assert_eq!(pushed, [gig.to_owned(), "cloud-0-1".to_owned()]);
    loaded::host(
        &mut control,
        &mut state,
        message(MessageAction::Copy, "cloud-Rhythm", None),
        at(30),
    );
    assert!(downloads(&state).is_empty());
    let slots = slots(&state);
    let library = &state.library.as_ref().unwrap().presets;
    for (slot, name) in [(0, "Lead"), (2, "Rhythm")] {
        let stored = library.load(slots[slot].as_deref().unwrap()).unwrap();
        let sidecar = stored.sidecar.unwrap();
        assert_eq!(
            sidecar.text("cloudId"),
            Some(format!("cloud-{name}").as_str())
        );
    }
    assert_eq!(slots[1].as_deref(), Some(held.as_str()));
    let _ = std::fs::remove_dir_all(&root);
}

/// File MOVE and COPY of IRs (`0x24b254` → `0x1e8a70` → the drop `0x1de9f8`): COPY links the
/// item into the target; MOVE links it into the target first (`vt+0x54` before `vt+0x4c`), then
/// unlinks it from a sub-folder source, so a failed target link keeps the source link (review
/// r29 B4); a MOVE out of the root keeps the root link (owned); an item already in the target
/// does nothing; an item missing from the source is refused. A SWAP of IRs writes nothing
/// (`0x24d2d8` needs `file_type` 0, review r29 B2). Bring-up without `--persist` refuses the
/// link.
#[test]
fn file_media_moves_link_the_target_before_the_source_unlinks() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::stock::{StockLibrary, StockLimits, create_media_folder};
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, ProductType, StockDb, StockProduct};
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-irmove-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    database
        .transaction(|write| write.boot_maintenance())
        .unwrap();
    let top = (database.view().root_folder(ProductType::Ir).unwrap()).unwrap();
    let [cabs, rooms, live, spare] = ["Cabs", "Rooms", "Live", "Spare"]
        .map(|name| create_media_folder(&mut database, ProductType::Ir, top, name).unwrap());
    database
        .transaction(|write| {
            write.put_product(&StockProduct {
                id: "CIR_1".into(),
                product_type: ProductType::Ir,
                read_only: false,
                raw_data: None,
                metadata: None,
            })?;
            write.link(top, "CIR_1")?;
            write.link(cabs, "CIR_1")
        })
        .unwrap();
    // A folder link whose product row is gone: the target link of a MOVE fails
    // (`ProductMissing`) after every MOVE check passed.
    rusqlite::Connection::open(root.join("zenui.db"))
        .unwrap()
        .execute(
            "INSERT INTO folder_content (folderId, productId) VALUES (?1, 'CIR_GONE')",
            [cabs],
        )
        .unwrap();
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(database),
    });
    let key = |id: i64| format!("{id}_q");
    let message = |action: MessageAction, item: &str, from: String, to: String| FileMessage {
        action: Some(action.into()),
        file_type: Some(1),
        folder: Some(FolderInfo {
            key: Some(from),
            files: vec![ProductData {
                key: Some(item.into()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        to_folder: Some(FolderInfo {
            key: Some(to),
            ..Default::default()
        }),
        ..Default::default()
    };
    let send = |control: &mut Control, state: &mut tetrad_app::device::State, message| {
        let (sent, effects) = loaded::host(control, state, message, at(10));
        assert!(sent.is_empty(), "{sent:?}");
        let pushed: Vec<_> = (loaded::notified(&effects).into_iter())
            .filter_map(|message| match message {
                ControlMessage::File(file) => file.folder?.key,
                _ => None,
            })
            .collect();
        (effects, pushed)
    };
    let edit =
        |control: &mut Control,
         state: &mut tetrad_app::device::State,
         action: MessageAction,
         from: String,
         to: String| { send(control, state, message(action, "CIR_1", from, to)) };
    let holds = |state: &tetrad_app::device::State, folder: i64, item: &str| {
        let database = state.library.as_ref().unwrap().database.as_ref().unwrap();
        let products = database.view().folder_products(folder).unwrap();
        products.contains(&item.to_owned())
    };
    let (_, pushed) = edit(
        &mut control,
        &mut state,
        MessageAction::Copy,
        key(cabs),
        key(rooms),
    );
    assert_eq!(pushed, [key(rooms)]);
    assert!(holds(&state, cabs, "CIR_1") && holds(&state, rooms, "CIR_1"));
    let (again, _) = edit(
        &mut control,
        &mut state,
        MessageAction::Copy,
        key(cabs),
        key(rooms),
    );
    assert!(again.is_empty(), "already in the target: {again:?}");
    let (_, pushed) = edit(
        &mut control,
        &mut state,
        MessageAction::Move,
        key(rooms),
        key(live),
    );
    assert_eq!(pushed, [key(live), key(rooms)]);
    assert!(holds(&state, live, "CIR_1") && !holds(&state, rooms, "CIR_1"));
    let gone = message(MessageAction::Move, "CIR_GONE", key(cabs), key(spare));
    let (failed, _) = send(&mut control, &mut state, gone);
    assert!(matches!(failed[..], [Effect::Notice(..)]), "{failed:?}");
    assert!(
        holds(&state, cabs, "CIR_GONE"),
        "a failed target link keeps the source link"
    );
    let (_, pushed) = edit(
        &mut control,
        &mut state,
        MessageAction::Move,
        "local_ir_root".into(),
        key(rooms),
    );
    assert_eq!(pushed, [key(rooms)]);
    assert!(
        holds(&state, top, "CIR_1") && holds(&state, rooms, "CIR_1"),
        "the root keeps it"
    );
    let (missing, _) = edit(
        &mut control,
        &mut state,
        MessageAction::Move,
        key(spare),
        key(cabs),
    );
    assert!(matches!(missing[..], [Effect::Notice(..)]), "{missing:?}");
    let mut swap = message(MessageAction::Swap, "CIR_1", key(cabs), key(spare));
    swap.to_folder.as_mut().unwrap().files = vec![ProductData {
        index: Some(0),
        ..Default::default()
    }];
    // `file_type` 1 fails the SWAP head; `file_type` 0 with IR folder keys is refused (O).
    for file_type in [1, 0] {
        let swap = FileMessage {
            file_type: Some(file_type),
            ..swap.clone()
        };
        let (swapped, _) = send(&mut control, &mut state, swap);
        assert!(matches!(swapped[..], [Effect::Notice(..)]), "{swapped:?}");
        assert!(
            !holds(&state, spare, "CIR_1"),
            "a SWAP of IRs links nothing"
        );
    }
    control.set_run_options(&loaded::bringup(false));
    let (refused, _) = edit(
        &mut control,
        &mut state,
        MessageAction::Copy,
        key(cabs),
        key(spare),
    );
    assert!(matches!(refused[..], [Effect::Notice(..)]), "{refused:?}");
    assert!(!holds(&state, spare, "CIR_1"));
    let _ = std::fs::remove_dir_all(&root);
}

/// File READ of IRs (`0x248ad4` type 1): after the user IR tree, the factory IR folder keyed
/// `/opt/neuraldsp/impulse_responses` (`is_factory`, no name), one name-only item per `*.bin`
/// base name cut at its last `_`, in name order ignoring case, each name once.
/// `omit_factory_content` leaves it out.
#[test]
fn file_read_lists_the_factory_ir_folder_after_the_user_irs() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::FileMessage;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_media::catalog::{FactoryIrCatalog, FactoryIrEntity, FactoryIrFile, MicPlacement};
    use tetrad_media::factory::FACTORY_IR_DIR;
    use tetrad_storage::media::MediaRoot;
    let root = std::env::temp_dir().join(format!("tetrad-app-control-fir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: None,
    });
    let entity = |tag: &str, suffixes: &[&str]| FactoryIrEntity {
        tag: tag.into(),
        cab: "Cab".into(),
        mic: "Mic".into(),
        readable_name: format!("{tag} Cab Mic"),
        files: (suffixes.iter())
            .map(|suffix| FactoryIrFile {
                path: format!("{FACTORY_IR_DIR}/{tag}_Cab_Mic_{suffix}.bin"),
                placement: MicPlacement::parse(suffix).unwrap(),
            })
            .collect(),
    };
    let catalog = FactoryIrCatalog::new(vec![entity("T1", &["A0", "B1"]), entity("t0", &["3"])]);
    control.set_factory_irs(&catalog.encode());
    let read = |omit| FileMessage {
        action: Some(MessageAction::Read.into()),
        file_type: Some(1),
        omit_factory_content: Some(omit),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read(false), at(10));
    let [ControlMessage::File(factory)] = &sent[..] else {
        panic!("the factory IR folder only (no zenui.db): {sent:?}");
    };
    assert_eq!((factory.action, factory.file_type), (update(), Some(1)));
    let folder = factory.folder.as_ref().unwrap();
    assert_eq!(folder.key.as_deref(), Some(FACTORY_IR_DIR));
    assert_eq!(
        (folder.is_factory, folder.name.as_deref()),
        (Some(true), None)
    );
    let names: Vec<_> = (folder.files.iter())
        .map(|item| (item.name.as_deref(), item.key.as_deref()))
        .collect();
    assert_eq!(
        names,
        [(Some("t0_Cab_Mic"), None), (Some("T1_Cab_Mic"), None)]
    );
    let (sent, _) = loaded::host(&mut control, &mut state, read(true), at(20));
    assert!(sent.is_empty(), "{sent:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// File (4) CREATE with an indexed item in a setlist is Cortex Control Save As (`0x24dae0`
/// → `savePresetSignal` `0x24e0c8` → `SceneGraph::savePresetSlot` `0x5b7438`): the WORKING
/// preset goes into the slot under the item name; on the loaded slot it saves in place
/// (`0x5b91dc`) under the slot's name, without a rename. The payload is never read on that
/// path, so an indexed CREATE that carries `preset_payload` is refused (owned guard, review r19
/// F1), as is a plugin import into a setlist; neither writes. Bring-up without `--persist`
/// refuses the save before it is queued.
#[test]
fn file_preset_create_is_save_as_of_the_working_preset() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData, RawMessageBytes};
    use tetrad_library::sidecar::Sidecar;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    let root = std::env::temp_dir().join(format!("tetrad-app-control-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets/Gig")).unwrap();
    let (mut state, mut control) = loaded::session();
    let mut presets = StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap();
    let working = state.current.as_ref().unwrap().preset.to_bytes();
    let document = tetrad_preset::PresetDocument::parse(&working, tetrad_preset::Limits::default());
    let mut sidecar = Sidecar::default();
    sidecar.set_text("name", "Clean");
    let stored = presets.store("Gig", 0, "Clean", &document.unwrap(), &sidecar);
    let stored = stored.unwrap().value;
    state.current.get_mut().as_mut().unwrap().path = Some(stored.clone());
    state.library = Some(Library {
        presets,
        database: None,
    });
    let gig = "/media/p4/Presets/Gig".to_owned();
    let create = |index: Option<i32>, name: Option<&str>, payload: Option<Vec<u8>>| FileMessage {
        action: Some(MessageAction::Create.into()),
        folder: Some(FolderInfo {
            key: Some(gig.clone()),
            is_plugin: Some(index.is_none()),
            files: vec![ProductData {
                index,
                name: name.map(str::to_owned),
                ..Default::default()
            }],
            ..Default::default()
        }),
        preset_payload: payload.map(|bytes| RawMessageBytes { bytes }),
        ..Default::default()
    };
    let host = |control: &mut Control, state: &mut tetrad_app::device::State, message| {
        let (sent, effects) = loaded::host(control, state, message, at(10));
        assert!(sent.is_empty(), "{sent:?}");
        effects
    };
    let save = host(
        &mut control,
        &mut state,
        create(Some(3), Some("Solo"), None),
    );
    let into = Effect::SavePresetInto {
        name: "Solo".into(),
        setlist: Some(gig.clone()),
        slot: 3,
        instrument: Default::default(),
    };
    assert_eq!(save, [into]);
    let in_place = host(&mut control, &mut state, create(Some(0), None, None));
    // No name: the file keeps its name, so the item key does not change (review r27 R5).
    assert_eq!(in_place, [Effect::SavePreset(None)]);
    for refused in [
        create(Some(3), Some("Solo"), Some(working.clone())),
        // `is_plugin` with a setlist key: not a plugin folder.
        create(None, Some("Plugin"), Some(working.clone())),
    ] {
        let effects = host(&mut control, &mut state, refused);
        assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
    }
    let library = &state.library.as_ref().unwrap().presets;
    let slots = library.slots("Gig").unwrap();
    assert_eq!(slots.iter().flatten().collect::<Vec<_>>(), [&stored]);
    control.set_run_options(&loaded::bringup(false));
    let effects = host(
        &mut control,
        &mut state,
        create(Some(3), Some("Solo"), None),
    );
    assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A9 (C): the Downloads folders are cloud folders (`vt+0x64` → 1), so a File MOVE or COPY
/// of a setlist preset into the Downloads presets folder is the cloud upload of that preset
/// (`0x24b254` → `0x1e8a70` → `0x1d81d0`); the setlist keeps it. Signed out: nothing
/// (`0x716b80`). An item that is not in the source folder and a SWAP are refused.
#[test]
fn a_move_into_the_downloads_presets_uploads_the_preset() {
    use tetrad_app::device::control::AccountRequest;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_storage::stock::{DbLimits, StockDb};
    let (root, mut state, mut control, path) = gig_session("downloads-move");
    let database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    state.library.as_mut().unwrap().database = Some(database);
    let message = |action: MessageAction, key: &str| FileMessage {
        action: Some(action.into()),
        file_type: Some(0),
        folder: Some(FolderInfo {
            key: Some("/media/p4/Presets/Gig".into()),
            files: vec![ProductData {
                key: Some(key.into()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        to_folder: Some(FolderInfo {
            key: Some("cloud-0-1".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (_, effects) = loaded::host(
        &mut control,
        &mut state,
        message(MessageAction::Move, &path),
        at(10),
    );
    assert!(
        account_requests(&effects).is_empty(),
        "signed out: {effects:?}"
    );
    state
        .settings
        .update(|settings| settings.account.username = "fake-user".into());
    for (ms, action) in [(20, MessageAction::Move), (30, MessageAction::Copy)] {
        let (_, effects) = loaded::host(&mut control, &mut state, message(action, &path), at(ms));
        let upload = AccountRequest::CloudProductUpload { path: path.clone() };
        assert_eq!(account_requests(&effects), [upload]);
    }
    for (ms, refused) in [
        (
            40,
            message(MessageAction::Move, "/media/p4/Presets/Gig/Other.pb"),
        ),
        (50, message(MessageAction::Swap, &path)),
    ] {
        let (_, effects) = loaded::host(&mut control, &mut state, refused, at(ms));
        assert!(account_requests(&effects).is_empty(), "{effects:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A9 plugin sub-folders (C): CREATE with `is_plugin` makes `<parent>/<name>`
/// (`0x248968` → `0x76f2b8`), UPDATE renames it (`0x248548` → `0x76aed8`), DELETE removes it
/// with its files (`0x77df68(.., false)` → `0x76ec04`); each pushes as a setlist or media
/// folder edit. O: the plugin root does not change.
#[test]
fn file_plugin_sub_folders_create_rename_and_delete() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo};
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-plugdir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for folder in ["Presets/Gig", "Plugins/Owned"] {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: None,
    });
    let owned = "/media/p4/Plugins/Owned";
    let message = |action: MessageAction, folder: FolderInfo| FileMessage {
        action: Some(action.into()),
        file_type: Some(0),
        folder: Some(folder),
        ..Default::default()
    };
    let pushes = |effects: &[Effect]| -> Vec<FileMessage> {
        (loaded::notified(effects).into_iter())
            .filter_map(|message| match message {
                ControlMessage::File(file) => Some(file),
                _ => None,
            })
            .collect()
    };
    let create = message(
        MessageAction::Create,
        FolderInfo {
            parent_key: Some(owned.into()),
            name: Some("Live".into()),
            is_plugin: Some(true),
            ..Default::default()
        },
    );
    let (_, effects) = loaded::host(&mut control, &mut state, create, at(10));
    assert!(root.join("Plugins/Owned/Live").is_dir());
    let live = format!("{owned}/Live");
    let created = pushes(&effects);
    assert_eq!(created.len(), 1, "{effects:?}");
    let folder = created[0].folder.as_ref().unwrap();
    assert_eq!(created[0].action, Some(MessageAction::Create.into()));
    assert_eq!(
        (folder.key.as_deref(), folder.is_plugin),
        (Some(live.as_str()), Some(true))
    );

    let rename = |key: &str, name: &str| {
        let folder = FolderInfo {
            key: Some(key.into()),
            name: Some(name.into()),
            ..Default::default()
        };
        message(MessageAction::Update, folder)
    };
    let (_, effects) = loaded::host(&mut control, &mut state, rename(&live, "Stage"), at(20));
    assert!(root.join("Plugins/Owned/Stage").is_dir());
    let renamed = pushes(&effects);
    assert_eq!(renamed.len(), 1, "{effects:?}");
    assert_eq!(renamed[0].action, Some(MessageAction::Update.into()));
    assert_eq!(
        renamed[0].folder.as_ref().unwrap().key.as_deref(),
        Some(live.as_str())
    );
    let stage = format!("{owned}/Stage");
    let to = renamed[0].to_folder.as_ref().unwrap();
    assert_eq!(
        (to.key.as_deref(), to.name.as_deref()),
        (Some(stage.as_str()), Some("Stage"))
    );
    loaded::host(&mut control, &mut state, rename(owned, "Mine"), at(30));
    assert!(root.join("Plugins/Owned").is_dir(), "the plugin root stays");

    let delete = FolderInfo {
        key: Some(stage.clone()),
        ..Default::default()
    };
    let delete = message(MessageAction::Delete, delete);
    let (_, effects) = loaded::host(&mut control, &mut state, delete, at(40));
    assert!(!root.join("Plugins/Owned/Stage").exists());
    let removed = pushes(&effects);
    assert_eq!(removed.len(), 1, "{effects:?}");
    assert_eq!(removed[0].action, Some(MessageAction::Delete.into()));
    assert_eq!(
        removed[0].folder.as_ref().unwrap().key.as_deref(),
        Some(stage.as_str())
    );
    // `0x77df68(.., false)`: 57 CREATE "Removing, please wait." (not cancellable, type 0, the
    // folder as source), the File DELETE, then 57 DELETE `finished`.
    let order: Vec<_> = (loaded::notified(&effects).into_iter())
        .filter_map(|message| match message {
            ControlMessage::BulkOperation(bulk) if bulk.finished == Some(true) => Some("finished"),
            ControlMessage::BulkOperation(bulk) => {
                assert_eq!(
                    bulk.progress_message.as_deref(),
                    Some("Removing, please wait.")
                );
                assert_eq!(
                    (bulk.cancellable, bulk.operation_type),
                    (Some(false), Some(0))
                );
                let source = bulk.source_folder.and_then(|folder| folder.key);
                assert_eq!(source.as_deref(), Some(stage.as_str()));
                Some("start")
            }
            ControlMessage::File(_) => Some("File"),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["start", "File", "finished"]);
    let _ = std::fs::remove_dir_all(&root);
}

/// File (4) plugin presets through the plugin store. CREATE without an index into a plugin
/// folder imports the payload (`0x24db44`): added → CREATE push with the stored item; the
/// same key again → replaced, UPDATE push with `old_key`; the item instrument goes into the
/// sidecar (`0x7ec4c4`); a payload that does not decode is refused (owned). MOVE from a plugin
/// folder (`0x1e01e8`) into a setlist slot copies, the source stays (`0x1de8ec`); into another
/// plugin folder it adds there and then deletes the source (`0x1dfe48`, `vt+0x4c`). Bring-up
/// without `--persist` refuses the import.
#[test]
fn file_plugin_presets_import_and_move_through_the_plugin_store() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{
        EnumValue, FileMessage, FolderInfo, ProductData, RawMessageBytes,
    };
    use tetrad_library::sidecar::Instrument;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-plugin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for folder in ["Presets/Gig", "Plugins/Owned", "Plugins/Other"] {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: None,
    });
    let working = state.current.as_ref().unwrap().preset.to_bytes();
    let owned = "/media/p4/Plugins/Owned".to_owned();
    let import = |key: &str, payload: Vec<u8>| FileMessage {
        action: Some(MessageAction::Create.into()),
        file_type: Some(0),
        folder: Some(FolderInfo {
            key: Some(owned.clone()),
            is_plugin: Some(true),
            files: vec![ProductData {
                key: Some(key.into()),
                name: Some("Lead".into()),
                instrument: Some(2),
                date_ms_since_epoch: Some(1_791_635_696_000),
                ..Default::default()
            }],
            ..Default::default()
        }),
        preset_payload: Some(RawMessageBytes { bytes: payload }),
        ..Default::default()
    };
    let host = |control: &mut Control, state: &mut tetrad_app::device::State, message| {
        let (sent, effects) = loaded::host(control, state, message, at(10));
        assert!(sent.is_empty(), "{sent:?}");
        effects
    };
    type Push = (EnumValue<MessageAction>, Option<String>, Vec<ProductData>);
    let pushes = |effects: &[Effect]| -> Vec<Push> {
        (loaded::notified(effects).into_iter())
            .filter_map(|message| match message {
                ControlMessage::File(file) => {
                    let folder = file.folder?;
                    Some((file.action?, folder.key, folder.files))
                }
                _ => None,
            })
            .collect()
    };
    let lead = format!("{owned}/Lead.pb");
    let added = host(
        &mut control,
        &mut state,
        import("cortex-key", working.clone()),
    );
    let [(action, folder, items)] = &pushes(&added)[..] else {
        panic!("one push: {added:?}");
    };
    assert_eq!(
        (*action, folder.as_deref()),
        (MessageAction::Create.into(), Some(&owned[..]))
    );
    assert_eq!(items[0].key.as_deref(), Some(&lead[..]));
    fn library(state: &tetrad_app::device::State) -> &StockLibrary {
        &state.library.as_ref().unwrap().presets
    }
    let sidecar = library(&state).sidecar(&lead).unwrap().unwrap();
    assert_eq!(sidecar.instrument(), Instrument::Bass);
    let replaced = host(&mut control, &mut state, import(&lead, working.clone()));
    let [(action, _, items)] = &pushes(&replaced)[..] else {
        panic!("one push: {replaced:?}");
    };
    assert_eq!(*action, MessageAction::Update.into());
    assert_eq!(items[0].old_key.as_deref(), Some(&lead[..]));
    // Owned: no write over the loaded preset's file (stock replaces and reloads it).
    let playing = state
        .current
        .get_mut()
        .as_mut()
        .unwrap()
        .path
        .replace(lead.clone());
    let refused = host(&mut control, &mut state, import(&lead, working.clone()));
    assert!(matches!(refused[..], [Effect::Notice(..)]), "{refused:?}");
    // The name (field 2) as invalid UTF-8: wire-valid, not a BinaryPreset.
    let bad = import("other-key", vec![0x12, 0x01, 0xff]);
    let refused = host(&mut control, &mut state, bad);
    assert!(matches!(refused[..], [Effect::Notice(..)]), "{refused:?}");
    assert!(library(&state).load(&format!("{owned}/Lead_1.pb")).is_err());
    let moved = |to: FolderInfo| FileMessage {
        action: Some(MessageAction::Move.into()),
        file_type: Some(0),
        folder: Some(FolderInfo {
            key: Some(owned.clone()),
            files: vec![ProductData {
                key: Some(lead.clone()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        to_folder: Some(to),
        ..Default::default()
    };
    let gig = FolderInfo {
        key: Some("/media/p4/Presets/Gig".into()),
        files: vec![ProductData {
            index: Some(4),
            ..Default::default()
        }],
        ..Default::default()
    };
    let copied = host(&mut control, &mut state, moved(gig.clone()));
    let folders: Vec<_> = pushes(&copied).into_iter().map(|(_, key, _)| key).collect();
    assert_eq!(folders, [Some("/media/p4/Presets/Gig".to_owned())]);
    let slot = library(&state).slots("Gig").unwrap()[4].clone();
    assert_eq!(slot.as_deref(), Some("/media/p4/Presets/Gig/Lead.pb"));
    assert!(
        library(&state).load(&lead).is_ok(),
        "a copy into a setlist keeps the source"
    );
    let other = FolderInfo {
        key: Some("/media/p4/Plugins/Other".into()),
        ..Default::default()
    };
    // Owned: no copy onto the loaded slot, no MOVE of the loaded preset.
    state.current.get_mut().as_mut().unwrap().path = slot;
    let refused = host(&mut control, &mut state, moved(gig));
    assert!(matches!(refused[..], [Effect::Notice(..)]), "{refused:?}");
    state.current.get_mut().as_mut().unwrap().path = Some(lead.clone());
    let refused = host(&mut control, &mut state, moved(other.clone()));
    assert!(matches!(refused[..], [Effect::Notice(..)]), "{refused:?}");
    assert!(library(&state).load(&lead).is_ok());
    state.current.get_mut().as_mut().unwrap().path = playing;
    let _ = host(&mut control, &mut state, moved(other));
    assert!(
        library(&state)
            .load("/media/p4/Plugins/Other/Lead.pb")
            .is_ok()
    );
    assert!(
        library(&state).load(&lead).is_err(),
        "a MOVE between plugin folders removes it"
    );
    control.set_run_options(&loaded::bringup(false));
    let refused = host(&mut control, &mut state, import("cortex-key", working));
    assert!(matches!(refused[..], [Effect::Notice(..)]), "{refused:?}");
    assert!(library(&state).load(&lead).is_err(), "nothing is written");
    let _ = std::fs::remove_dir_all(&root);
}

/// Re-r34 §6: File (4) MOVE of a setlist preset into a plugin folder (`0x1e8a70` → `0x1de9f8`).
/// No target index: nothing is done (C, `0x7c3d94` finds no empty item). Stock clears the slot
/// before the add, so a failed add loses the preset; Tetrad adds first and clears the slot only
/// after a good add (O). Refused (O): COPY (not traced), a read-only plugin folder, the loaded
/// preset, bring-up without `--persist`. A good MOVE stores a free `<name>.pb`, empties the
/// slot and pushes both folders.
#[test]
fn a_setlist_preset_moves_into_a_plugin_folder_only_after_the_add() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::sidecar::Sidecar;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    let root = std::env::temp_dir().join(format!(
        "tetrad-app-control-to-plugin-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    for folder in [
        "user/Presets/Gig",
        "user/Plugins/Owned",
        "factory/Plugins/Stock",
    ] {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
    let (mut state, mut control) = loaded::session();
    let mut presets = StockLibrary::open(MediaRoot::new(root.join("user")), StockLimits::default())
        .unwrap()
        .with_factory(MediaRoot::factory(root.join("factory")));
    let mut lead = state.current.as_ref().unwrap().preset.clone();
    lead.name = Some("Lead".into());
    let document =
        tetrad_preset::PresetDocument::parse(&lead.to_bytes(), tetrad_preset::Limits::default())
            .unwrap();
    let path = (presets.store("Gig", 1, "Lead", &document, &Sidecar::default()))
        .unwrap()
        .value;
    state.library = Some(Library {
        presets,
        database: None,
    });
    let (gig, owned) = ("/media/p4/Presets/Gig", "/media/p4/Plugins/Owned");
    let message = |action: MessageAction, to: &str, index: Option<i32>| FileMessage {
        action: Some(action.into()),
        file_type: Some(0),
        folder: Some(FolderInfo {
            key: Some(gig.into()),
            files: vec![ProductData {
                key: Some(path.clone()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        to_folder: Some(FolderInfo {
            key: Some(to.into()),
            files: vec![ProductData {
                index,
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let host = |control: &mut Control, state: &mut tetrad_app::device::State, message| {
        let (sent, effects) = loaded::host(control, state, message, at(10));
        assert!(sent.is_empty(), "{sent:?}");
        effects
    };
    let slot = |state: &tetrad_app::device::State| {
        let library = &state.library.as_ref().unwrap().presets;
        library.slots("Gig").unwrap()[1].clone()
    };
    let stored = root.join("user/Plugins/Owned/Lead.pb");
    let kept =
        |state: &tetrad_app::device::State| slot(state).as_ref() == Some(&path) && !stored.exists();
    let effects = host(
        &mut control,
        &mut state,
        message(MessageAction::Move, owned, None),
    );
    assert!(effects.is_empty() && kept(&state), "{effects:?}");
    let refused = message(MessageAction::Move, "/opt/neuraldsp/Plugins/Stock", Some(0));
    let effects = host(&mut control, &mut state, refused);
    assert!(
        matches!(&effects[..], [Effect::Notice(_, detail)] if detail.contains("read-only")),
        "{effects:?}"
    );
    assert!(kept(&state));
    let playing = state
        .current
        .get_mut()
        .as_mut()
        .unwrap()
        .path
        .replace(path.clone());
    let effects = host(
        &mut control,
        &mut state,
        message(MessageAction::Move, owned, Some(0)),
    );
    assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
    assert!(kept(&state));
    state.current.get_mut().as_mut().unwrap().path = playing;

    // The add fails (a directory without write permission): the preset stays in its slot.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let directory = root.join("user/Plugins/Owned");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root writes anyway; the failed add is then not observable.
        let locked = std::fs::write(directory.join("probe"), b"").is_err();
        if locked {
            let effects = host(
                &mut control,
                &mut state,
                message(MessageAction::Move, owned, Some(0)),
            );
            assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
            assert!(kept(&state));
            let library = &state.library.as_ref().unwrap().presets;
            assert!(library.load(&path).is_ok(), "the source file stays");
        }
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_file(directory.join("probe"));
    }

    control.set_run_options(&loaded::bringup(false));
    let effects = host(
        &mut control,
        &mut state,
        message(MessageAction::Move, owned, Some(0)),
    );
    assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
    assert!(kept(&state));
    control.set_run_options(&loaded::bringup(true));
    let effects = host(
        &mut control,
        &mut state,
        message(MessageAction::Move, owned, Some(0)),
    );
    assert_eq!(slot(&state), None);
    let library = &state.library.as_ref().unwrap().presets;
    assert!(library.load(&format!("{owned}/Lead.pb")).is_ok());
    assert!(library.load(&path).is_err(), "the setlist file goes");
    let pushed: Vec<_> = (loaded::notified(&effects).into_iter())
        .filter_map(|message| match message {
            ControlMessage::File(file) => file.folder?.key,
            _ => None,
        })
        .collect();
    assert_eq!(pushed, [owned, gig], "{effects:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// re-r38 §5: File (4) COPY and SWAP of a setlist preset into a plugin folder add a new plugin
/// file each and keep the source (`0x1e8a70` → `0x1de9f8`, the SWAP form `0x1e5a58` is not
/// reached); a SWAP into a read-only folder writes nothing; without `--persist` nothing is
/// written. Only the plugin folder goes out as UPDATE.
#[test]
fn a_setlist_preset_copies_or_swaps_into_a_plugin_folder_and_keeps_the_source() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::sidecar::Sidecar;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    let root = std::env::temp_dir().join(format!(
        "tetrad-app-control-copy-plugin-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    for folder in [
        "user/Presets/Gig",
        "user/Plugins/Owned",
        "factory/Plugins/Stock",
    ] {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
    let (mut state, mut control) = loaded::session();
    let mut presets = StockLibrary::open(MediaRoot::new(root.join("user")), StockLimits::default())
        .unwrap()
        .with_factory(MediaRoot::factory(root.join("factory")));
    let mut lead = state.current.as_ref().unwrap().preset.clone();
    lead.name = Some("Lead".into());
    let document =
        tetrad_preset::PresetDocument::parse(&lead.to_bytes(), tetrad_preset::Limits::default())
            .unwrap();
    let path = (presets.store("Gig", 1, "Lead", &document, &Sidecar::default()))
        .unwrap()
        .value;
    state.library = Some(Library {
        presets,
        database: None,
    });
    // The loaded preset may be copied: the source stays.
    state.current.get_mut().as_mut().unwrap().path = Some(path.clone());
    let (gig, owned) = ("/media/p4/Presets/Gig", "/media/p4/Plugins/Owned");
    let message = |action: MessageAction, to: &str| FileMessage {
        action: Some(action.into()),
        file_type: Some(0),
        folder: Some(FolderInfo {
            key: Some(gig.into()),
            files: vec![ProductData {
                key: Some(path.clone()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        to_folder: Some(FolderInfo {
            key: Some(to.into()),
            files: vec![ProductData {
                index: Some(0),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let plugin_files = || {
        let mut names: Vec<_> = std::fs::read_dir(root.join("user/Plugins/Owned"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".pb"))
            .collect();
        names.sort();
        names
    };
    let source_kept = |state: &tetrad_app::device::State| {
        let library = &state.library.as_ref().unwrap().presets;
        library.slots("Gig").unwrap()[1].as_ref() == Some(&path) && library.load(&path).is_ok()
    };

    control.set_run_options(&loaded::bringup(false));
    let (sent, effects) = loaded::host(
        &mut control,
        &mut state,
        message(MessageAction::Copy, owned),
        at(10),
    );
    assert!(sent.is_empty() && matches!(effects[..], [Effect::Notice(..)]));
    assert!(plugin_files().is_empty() && source_kept(&state));
    control.set_run_options(&loaded::bringup(true));
    let (sent, effects) = loaded::host(
        &mut control,
        &mut state,
        message(MessageAction::Swap, "/opt/neuraldsp/Plugins/Stock"),
        at(10),
    );
    assert!(sent.is_empty() && matches!(effects[..], [Effect::Notice(..)]));
    assert!(source_kept(&state));

    for (action, expected) in [
        (MessageAction::Copy, vec!["Lead.pb"]),
        (MessageAction::Swap, vec!["Lead.pb", "Lead_1.pb"]),
    ] {
        let (sent, effects) =
            loaded::host(&mut control, &mut state, message(action, owned), at(10));
        assert!(sent.is_empty(), "{sent:?}");
        assert_eq!(plugin_files(), expected, "{action:?}");
        assert!(source_kept(&state), "{action:?}");
        let pushed: Vec<_> = (loaded::notified(&effects).into_iter())
            .filter_map(|message| match message {
                ControlMessage::File(file) => file.folder?.key,
                _ => None,
            })
            .collect();
        assert_eq!(pushed, [owned], "{action:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A9 (`0x1e01e8` → `0x1d6ec8`, C): a copy out of a read-only plugin folder takes the account
/// as its author in the document and the sidecar, into a setlist slot and into a plugin
/// folder; the tags stay and the source is untouched. A copy from a writable folder keeps its
/// author. Invented names.
#[test]
fn a_copy_out_of_a_read_only_plugin_folder_takes_the_account_as_author() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-stamp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for folder in [
        "user/Presets/Gig",
        "user/Plugins/Owned",
        "factory/Plugins/Stock",
    ] {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
    let (mut state, mut control) = loaded::session();
    state
        .settings
        .update(|settings| settings.account.ui_username = "Test Player".into());
    let mut lead = state.current.as_ref().unwrap().preset.clone();
    lead.name = Some("Lead".into());
    lead.author_name = Some("Plugin Maker".into());
    lead.author_id = Some("maker-1".into());
    let stock = root.join("factory/Plugins/Stock");
    std::fs::write(stock.join("Lead.pb"), lead.to_bytes()).unwrap();
    let sidecar =
        br#"{"name":"Lead","author":"Plugin Maker","authorId":"maker-1","tags":["warm"]}"#;
    std::fs::write(stock.join("Lead.json"), sidecar).unwrap();
    let presets = StockLibrary::open(MediaRoot::new(root.join("user")), StockLimits::default())
        .unwrap()
        .with_factory(MediaRoot::factory(root.join("factory")));
    state.library = Some(Library {
        presets,
        database: None,
    });
    let copy = |from: &str, item: &str, to: &str, index: Option<i32>| FileMessage {
        action: Some(MessageAction::Copy.into()),
        file_type: Some(0),
        folder: Some(FolderInfo {
            key: Some(from.into()),
            files: vec![ProductData {
                key: Some(item.into()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        to_folder: Some(FolderInfo {
            key: Some(to.into()),
            files: vec![ProductData {
                index,
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let source = "/opt/neuraldsp/Plugins/Stock/Lead.pb";
    let owned = "/media/p4/Plugins/Owned";
    for (to, index) in [("/media/p4/Presets/Gig", Some(2)), (owned, None)] {
        let message = copy("/opt/neuraldsp/Plugins/Stock", source, to, index);
        let (sent, _) = loaded::host(&mut control, &mut state, message, at(10));
        assert!(sent.is_empty(), "{sent:?}");
    }
    let library = &state.library.as_ref().unwrap().presets;
    let author = |library: &StockLibrary, path: &str| {
        let preset = library.load(path).unwrap();
        let stored = tetrad_preset::Preset::try_from(&preset.document).unwrap();
        let sidecar = preset.sidecar.unwrap_or_default();
        let text = |key| sidecar.text(key).unwrap_or_default().to_owned();
        (
            (stored.author_name, stored.author_id),
            (text("author"), text("authorId")),
            sidecar.texts("tags").join(","),
        )
    };
    let stamped = (
        (Some("Test Player".to_owned()), Some(String::new())),
        ("Test Player".to_owned(), String::new()),
        "warm".to_owned(),
    );
    let slot = library.slots("Gig").unwrap()[2].clone().unwrap();
    assert_eq!(author(library, &slot), stamped);
    let plugin_copy = format!("{owned}/Lead.pb");
    assert_eq!(author(library, &plugin_copy), stamped);
    let original = (
        (Some("Plugin Maker".to_owned()), Some("maker-1".to_owned())),
        ("Plugin Maker".to_owned(), "maker-1".to_owned()),
        "warm".to_owned(),
    );
    assert_eq!(author(library, source), original);
    // A writable source keeps its author: the user copy into another slot, with another
    // account signed in.
    state
        .settings
        .update(|settings| settings.account.ui_username = "Other Player".into());
    let message = copy(owned, &plugin_copy, "/media/p4/Presets/Gig", Some(3));
    let (sent, _) = loaded::host(&mut control, &mut state, message, at(20));
    assert!(sent.is_empty(), "{sent:?}");
    let library = &state.library.as_ref().unwrap().presets;
    let slot = library.slots("Gig").unwrap()[3].clone().unwrap();
    assert_eq!(author(library, &slot), stamped);
    let _ = std::fs::remove_dir_all(&root);
}

/// File (4) UPDATE (`0x24eecc`) and DELETE (`0x24a570`) of setlist preset items, keyed by the
/// preset path: UPDATE renames another slot and pushes `changed` (`0x253d24`, the item with
/// `old_key`); UPDATE of the loaded preset saves the working preset in place
/// (`modifyPresetMetadataSignal` → `0x5b91dc`); DELETE empties the slot and pushes `removed`
/// (`0x25270c`).
#[test]
fn file_preset_item_update_and_delete_follow_stock() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::sidecar::Sidecar;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    let root = std::env::temp_dir().join(format!(
        "tetrad-app-control-preset-item-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets/Gig")).unwrap();
    let (mut state, mut control) = loaded::session();
    let mut presets = StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap();
    let working = state.current.as_ref().unwrap().preset.to_bytes();
    let document =
        tetrad_preset::PresetDocument::parse(&working, tetrad_preset::Limits::default()).unwrap();
    let mut paths = Vec::new();
    for (slot, name) in [(0, "Clean"), (1, "Lead")] {
        let stored = presets.store("Gig", slot, name, &document, &Sidecar::default());
        paths.push(stored.unwrap().value);
    }
    state.current.get_mut().as_mut().unwrap().path = Some(paths[0].clone());
    state.library = Some(Library {
        presets,
        database: None,
    });
    let gig = "/media/p4/Presets/Gig".to_owned();
    let mut edit =
        |state: &mut tetrad_app::device::State, action: MessageAction, key: &str, name| {
            let message = FileMessage {
                action: Some(action.into()),
                folder: Some(FolderInfo {
                    key: Some(gig.clone()),
                    files: vec![ProductData {
                        key: Some(key.to_owned()),
                        name: Some(name)
                            .filter(|name: &&str| !name.is_empty())
                            .map(str::to_owned),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            };
            let (sent, effects) = loaded::host(&mut control, state, message, at(10));
            assert!(sent.is_empty(), "{sent:?}");
            effects
        };
    let slot = |state: &tetrad_app::device::State, slot: usize| {
        let library = &state.library.as_ref().unwrap().presets;
        library.slots("Gig").unwrap()[slot].clone()
    };
    let renamed = edit(&mut state, MessageAction::Update, &paths[1], "Solo");
    let new_path = slot(&state, 1).unwrap();
    assert!(new_path.ends_with("/Solo.pb"), "{new_path}");
    let [ControlMessage::File(push)] = &loaded::notified(&renamed)[..] else {
        panic!("one changed push: {renamed:?}");
    };
    let item = &push.folder.as_ref().unwrap().files[0];
    assert_eq!(push.action, update());
    assert_eq!(item.key.as_ref(), Some(&new_path));
    assert_eq!(item.old_key.as_ref(), Some(&paths[1]));
    let loaded_update = edit(&mut state, MessageAction::Update, &paths[0], "Crunch");
    assert_eq!(loaded_update, [Effect::SavePreset(Some("Crunch".into()))]);
    assert_eq!(
        slot(&state, 0).as_ref(),
        Some(&paths[0]),
        "the save renames"
    );
    let deleted = edit(&mut state, MessageAction::Delete, &new_path, "");
    assert_eq!(slot(&state, 1), None);
    let [ControlMessage::File(push)] = &loaded::notified(&deleted)[..] else {
        panic!("one removed push: {deleted:?}");
    };
    let files = &push.folder.as_ref().unwrap().files;
    assert_eq!(push.action, Some(MessageAction::Delete.into()));
    assert_eq!(files[0].key.as_ref(), Some(&new_path));
    let _ = std::fs::remove_dir_all(&root);
}

/// A link loss during a Control backup send ends it (`0x6e1d74` resets the LocalBackup
/// receiver; tetrad-control drops the chunks): after a reconnect a new CREATE starts a backup
/// (review r18 F6). Before the fix the state stayed `Sending` and every CREATE was refused.
#[test]
fn local_backup_send_ends_on_a_link_loss() {
    use tetrad_app::device::services::{ControlBackup, ControlBackupDone};
    use tetrad_control::codecs::LocalBackupMessage;
    let (mut state, mut control) = loaded::session();
    let create = LocalBackupMessage {
        action: Some(MessageAction::Create.into()),
        ..Default::default()
    };
    loaded::host(&mut control, &mut state, create.clone(), at(10));
    let done = ControlBackupDone::Created {
        json: "x".repeat(200_000),
    };
    control.backup_done(done, &mut state, at(20)).unwrap();
    control.usb_unplugged(&mut state, at(30)).unwrap();
    loaded::handshake(&mut control, &mut state, at(35));
    let (_, effects) = loaded::host(&mut control, &mut state, create, at(40));
    assert!(
        matches!(
            effects[..],
            [Effect::ControlBackup(ControlBackup::Create(_))]
        ),
        "{effects:?}"
    );
}

/// Without `--persist` a Control ModelPreset CREATE is refused before the reducer runs: the
/// device never sees an action Control reduces itself, so Control runs `RunOptions::refusal`
/// (review r18 F12), and `zenui.db` keeps no new preset.
#[test]
fn model_preset_create_needs_persist() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{GridMessage, ModelPreset, ModelPresetMessage};
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, StockDb};
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-vdp-ro-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    database
        .transaction(|write| write.boot_maintenance())
        .unwrap();
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(database),
    });
    let block = tetrad_preset::Model {
        hash: Some(1),
        column: Some(2),
        ..Default::default()
    };
    let add: GridMessage = grid::message(MessageAction::Update, grid::block(1, block));
    let (_, effects) = loaded::host(&mut control, &mut state, add, at(0));
    loaded::commit(&mut state, effects);
    control.set_run_options(&loaded::bringup(false));
    let create = ModelPresetMessage {
        action: Some(MessageAction::Create.into()),
        create_from_row: Some(1),
        create_from_column: Some(2),
        presets: vec![ModelPreset {
            name: Some("Mine".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let (_, effects) = loaded::host(&mut control, &mut state, create, at(10));
    assert!(matches!(effects[..], [Effect::Notice(..)]), "{effects:?}");
    let database = state.library.as_ref().unwrap().database.as_ref().unwrap();
    assert!(database.view().model_presets().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// Review-2 R5: the `zenui.db` writes that Control receivers run themselves (File IR import,
/// File media folder CREATE, PinnedModels CREATE) are refused without `--persist` with a
/// "bring-up refused" notice, also on a writable database.
#[test]
fn control_database_writes_need_persist() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, PinnedModelsMessage, ProductData};
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_media::factory::DeviceCipher;
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, ProductType, StockDb};
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-db-ro-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    database
        .transaction(|write| write.boot_maintenance())
        .unwrap();
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(database),
    });
    control.set_media_cipher(DeviceCipher::caller_secret(b"invented cipher"));
    control.set_wall_clock(Some(
        std::time::UNIX_EPOCH + Duration::from_secs(1_791_635_696),
    ));
    let view = |state: &tetrad_app::device::State| {
        let view = state
            .library
            .as_ref()
            .unwrap()
            .database
            .as_ref()
            .unwrap()
            .view();
        (
            view.product_ids(ProductType::Ir).unwrap(),
            view.folders().unwrap().len(),
            view.pinned_models().unwrap(),
        )
    };
    let before = view(&state);
    control.set_run_options(&loaded::bringup(false));
    let import = FileMessage {
        action: Some(MessageAction::Create.into()),
        folder: Some(FolderInfo {
            key: Some("local_ir_root".into()),
            files: vec![ProductData {
                name: Some("Room IR".into()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ir_payload: Some(ir_wav(64)),
        ..Default::default()
    };
    let folder = FileMessage {
        action: Some(MessageAction::Create.into()),
        file_type: Some(1),
        folder: Some(FolderInfo {
            name: Some("Rooms".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let pin = PinnedModelsMessage {
        action: Some(MessageAction::Create.into()),
        models: vec![7],
        ..Default::default()
    };
    let messages: [ControlMessage; 3] = [import.into(), folder.into(), pin.into()];
    for (index, message) in messages.into_iter().enumerate() {
        let (sent, effects) =
            loaded::host(&mut control, &mut state, message, at(10 * index as u64));
        assert!(sent.is_empty(), "{sent:?}");
        let [Effect::Notice(_, notice)] = &effects[..] else {
            panic!("one notice: {effects:?}");
        };
        assert!(
            notice.contains("bring-up refused a library write"),
            "{notice}"
        );
    }
    assert_eq!(view(&state), before);
    let _ = std::fs::remove_dir_all(&root);
}

/// Review-2 R1: Cortex Control GeneralSettings `factory_reset` and `reset_settings` reach
/// `Action::Reset`, which Control reduces itself. A bring-up run refuses them in every mode, as
/// it refuses the UI reset (`RunOptions::refusal`): with and without `--persist` no setting
/// changes, no INI file is written (`would-write/` or the store), no HAL write runs, and the
/// receiver answers `Refused`.
#[test]
fn control_resets_are_refused_in_every_bringup_run() {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use tetrad_app::device::Device;
    use tetrad_app::device::bringup::{
        Bringup, Heartbeat, Host, Launch, RunOptions, Stage, WOULD_WRITE,
    };
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    use tetrad_app::device::platform::{Platform, ServiceBackends};
    use tetrad_control::codecs::GeneralSettingsMessage;
    struct NoStop;
    impl Host for NoStop {
        fn stop_requested(&mut self) -> bool {
            false
        }
        fn rss_kib(&mut self) -> Option<u64> {
            None
        }
    }
    let general = |change: fn(&mut GeneralSettingsMessage)| {
        let mut message = GeneralSettingsMessage {
            action: update(),
            ..Default::default()
        };
        change(&mut message);
        message
    };
    // `--persist` writes the store: no `would-write/` folder.
    let would_write = |dir: &std::path::Path| -> BTreeMap<_, _> {
        (std::fs::read_dir(dir.join(WOULD_WRITE))
            .into_iter()
            .flatten())
        .map(|entry| entry.unwrap().path())
        .map(|path| (path.clone(), std::fs::read(path).unwrap()))
        .collect()
    };
    let resets: [fn(&mut GeneralSettingsMessage); 2] = [
        |message| message.factory_reset = Some(true),
        |message| message.reset_settings = Some(true),
    ];
    for persist in [false, true] {
        for reset in resets {
            let root = std::env::temp_dir().join(format!(
                "tetrad-app-control-reset-{}-{persist}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            let preset = common::light_preset(&common::STOCK.specs(), 2);
            let media = common::Media::new([("Light", preset)]);
            let log = Rc::new(RefCell::new(common::Log::default()));
            let mut sim = common::Sim::new(common::STOCK, &media, &log);
            for marker in [CAPTURE_CORRUPTION_CHECKED, CAPTURE_CONVERSION_FINISHED] {
                sim.services().files().put(marker, "");
            }
            let mut options =
                RunOptions::bringup(Stage::Full, Duration::from_secs(600), root.clone());
            options.persist = persist;
            let launch = Launch {
                stored_boot_count: 1,
                binary_sha256: None,
                heartbeat: Heartbeat::default(),
                host: Box::new(NoStop),
            };
            let platform = Bringup::new(sim, &options, launch).unwrap();
            let mut device = Device::start_with(platform, options).unwrap();
            common::connect_control(&mut device);
            // Steps until the settings flush ran (250 ms of device time at most).
            let settle = |device: &mut Device<Bringup<common::Sim>>| {
                for _ in 0..10_000 {
                    if device.state().settings.dirty_files().next().is_none() {
                        return;
                    }
                    device.step();
                }
                panic!("the settings flush never ran");
            };
            let refusals = || {
                let run = std::fs::read_to_string(root.join("run.000.log")).unwrap();
                run.matches("bring-up refused a reset").count()
            };
            // A changed setting that a reset would undo; its flush is written before the mark.
            common::host_send(
                &mut device,
                general(|message| message.led_brightness = Some(37)),
            );
            settle(&mut device);
            let settings = device.state().settings.settings().clone();
            let files = would_write(&root);
            let (writes, trace) = {
                let log = log.borrow();
                (log.settings_writes.len(), log.trace.len())
            };
            assert!(
                writes > 0 || !files.is_empty(),
                "the change reached no INI file"
            );
            assert_eq!(refusals(), 0);

            common::host_send(&mut device, general(reset));
            settle(&mut device);
            (0..100).for_each(|_| _ = device.step());
            assert_eq!(*device.state().settings.settings(), settings);
            assert_eq!(would_write(&root), files);
            let log = log.borrow();
            assert_eq!(log.settings_writes.len(), writes);
            let hal = log.trace[trace..]
                .iter()
                .filter(|mark| matches!(mark, common::Mark::Hal(_)));
            assert_eq!(hal.count(), 0);
            assert_eq!(refusals(), 1);
            drop(log);
            drop(device);
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}

/// Looper (28) pushes (LooperMessageSender `0x18f9d0`): a new DSP record sends UPDATE `status`
/// (`0x513c94` → `0x18fa84`), a new controller state UPDATE `state` (`0x513bbc` →
/// `0x190908`), then `update_type` Meter (`0x55db24` → `0x18ff24`). A changed state word
/// restarts the 150 ms Buttons push (`0x1905c8`). An equal record sends nothing.
#[test]
fn looper_records_and_state_changes_push_to_control() {
    use tetrad_control::codecs::{LooperMessage, LooperUpdateType};
    use tetrad_dsp_protocol::responses::LooperStatus;
    let (mut state, mut control) = loaded::session();
    let mut looper = |state: &mut tetrad_app::device::State, now| -> Vec<LooperMessage> {
        (loaded::sync(&mut control, state, at(now)).into_iter())
            .filter_map(|message| match message {
                ControlMessage::Looper(looper) => Some(looper),
                _ => None,
            })
            .collect()
    };
    let record = LooperStatus {
        state: 2,
        armed: 1,
        ..LooperStatus::from_words([0; 22])
    };
    state.looper = Some(record);
    state.looper_playback.cached_state = 2;
    let [status, new_state, meter] = &looper(&mut state, 10)[..] else {
        panic!("status, state and meter pushes");
    };
    assert!(
        [status, new_state, meter]
            .iter()
            .all(|push| push.action == update())
    );
    let pushed = status.status.as_ref().expect("the record");
    assert_eq!((pushed.state, pushed.armed), (Some(2), Some(1)));
    assert_eq!(
        (new_state.state, new_state.status.is_none()),
        (Some(2), true)
    );
    assert_eq!(meter.update_type, Some(LooperUpdateType::Meter.into()));
    assert!(
        looper(&mut state, 100).is_empty(),
        "an equal record and no button push yet"
    );
    let [buttons] = &looper(&mut state, 160)[..] else {
        panic!("one Buttons push 150 ms later");
    };
    assert_eq!(buttons.update_type, Some(LooperUpdateType::Buttons.into()));
}

/// DefaultParameters UPDATE on a capture block (14000) keeps its capture key (parameter 5)
/// also with stored rows. Tetrad keys capture defaults by model id, so the rows can hold the key
/// of another capture (stock reads them by the block's own key, `0x66768c`); the other rows
/// still apply (review r19 F6).
#[test]
fn default_parameters_update_keeps_the_capture_key() {
    use tetrad_app::device::Library;
    use tetrad_app::device::actions::{Action, reduce};
    use tetrad_control::codecs::{DefaultParametersMessage, GridMessage, MessageAction::Update};
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_preset::{Model, Param, ParamValue, SceneValue, Slot};
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, ExpSwitch, ParamEntry, StockDb};
    let capture = "<Category id='14' name='Capture'><Model id='14000' name='Capture' blob='nc'>\
<Parameter name='A' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='B' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='C' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='D' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='E' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='file_name' type='string' defaultValue=''/></Model></Category>";
    let module = "<Module id='14000' name='Capture' blob_name='nc' category='14' dm_heap='100' \
pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='1'/></Module>";
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-nc-def-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    database
        .transaction(|write| write.boot_maintenance())
        .unwrap();
    let (mut state, mut control) = loaded::session_with_extra(capture, module);
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(database),
    });
    let key = Param {
        index: Some(5),
        values: vec![SceneValue {
            value: Some(ParamValue::String("capture-y".into())),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut params: Vec<Param> = (0..5).map(|index| grid::param(index, 0.0)).collect();
    params.push(key);
    let block = Model {
        hash: Some(14_000),
        column: Some(2),
        params,
        ..Default::default()
    };
    let add: GridMessage = grid::message(Update, grid::block(1, block));
    let (_, effects) = loaded::host(&mut control, &mut state, add, at(0));
    loaded::commit(&mut state, effects);
    let row = |index, value: &str| ParamEntry {
        index,
        value: value.into(),
        exp: 0,
        exp_trim_max: 1.0,
        exp_trim_min: 0.0,
    };
    let switch = ExpSwitch {
        switch_type: 2,
        inverted: false,
        latch_emulation: false,
        delay_ms: 600,
    };
    let defaults = Action::SetDefaultParams {
        key: "14000".into(),
        switch,
        params: vec![row(0, "0.5"), row(5, "capture-x")],
    };
    reduce(defaults, &mut state).unwrap();
    let reset = DefaultParametersMessage {
        action: update(),
        row: Some(1),
        column: Some(2),
        ..Default::default()
    };
    let (_, effects) = loaded::host(&mut control, &mut state, reset, at(1000));
    loaded::commit(&mut state, effects);
    let preset = &state.current.as_ref().unwrap().preset;
    let block = preset.block(Slot { row: 1, column: 2 }).unwrap().unwrap();
    let value = |index: usize| block.params[index].values[0].value.clone();
    assert_eq!(value(5), Some(ParamValue::String("capture-y".into())));
    assert_eq!(value(0), Some(ParamValue::Float(0.5)));
    let _ = std::fs::remove_dir_all(&root);
}

/// ModuleStats (35) READ `0x19c434`: one item per module report row (model id order, I), a
/// default row for a compiled module without one (the Looper X), and one item per SHARC core
/// with its capacity (`id` = core; the template minus the kernel, S7 capture). Zero fields
/// are proto3 defaults. `valid_id` is the compiler id map: the boot ids of `0x882148` and
/// every model with a blob, valid. `size_relations` (`0x6ba188`) pairs the visible models of
/// one category: 2 fits in 1; the replaced 7001 and the hidden 7003 take no part.
#[test]
fn module_stats_read_lists_the_module_report_and_the_cores() {
    use tetrad_control::codecs::{
        ModuleStatsMessage, ModuleStatsSizeRelation, ModuleStatsSizeRelationPair,
    };
    use tetrad_preset::{Model, ParamValue, Slot};
    let module = |id: u32| {
        format!(
            "<Module id='{id}' name='M' blob_name='m{id}' category='7' dm_heap='100' \
pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='5'/></Module>"
        )
    };
    let models = "<Category id='7' name='Modulation'>\
<Model id='7001' name='Old' blob='m7001'/>\
<Model id='7002' name='New' blob='m7002' replaces='7001'/>\
<Model id='7003' name='Hidden' blob='m7003' hidden='true'/></Category>\
<Category id='27' name='Loopers'><Model id='27000' name='Looper X' blob=''>\
<Parameter name='LEVEL' type='float' min='0' max='1' defaultValue='1'/></Model></Category>";
    let modules: String = [7001, 7002, 7003].map(module).concat();
    let (mut state, mut control) =
        loaded::session_with_preset(models, &modules, |preset, catalog| {
            let looper = Model::block(27000, [ParamValue::Float(1.0)]);
            let slot = Slot { row: 0, column: 0 };
            preset.insert_block(slot, looper, catalog).unwrap();
        });
    let read = ModuleStatsMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read, at(10));
    let [ControlMessage::ModuleStats(reply)] = &sent[..] else {
        panic!("one ModuleStats reply: {sent:?}");
    };
    assert_eq!(reply.action, update());
    let ids: Vec<_> = reply.stats.iter().filter_map(|item| item.id).collect();
    assert_eq!(ids, [1, 2, 7001, 7002, 7003, 10003, 26001, 27000]);
    let light = &reply.stats[0];
    assert_eq!((light.cpu_percent, light.latency), (Some(10.0), Some(16)));
    let looper = reply.stats.last().unwrap();
    assert_eq!((looper.cpu_percent, looper.reuse_pmco), (None, Some(true)));
    let cores: Vec<_> = (reply.core_stats.iter())
        .map(|item| (item.id, item.cpu_percent))
        .collect();
    assert_eq!(
        cores,
        [
            (None, Some(96.0)),
            (Some(1), Some(94.0)),
            (Some(2), Some(96.0)),
            (Some(3), Some(96.0))
        ]
    );
    assert!(
        reply
            .core_stats
            .iter()
            .all(|core| core.reuse_pmco == Some(true))
    );
    let relation = |id, smaller_models: &[u32], larger_models: &[u32]| ModuleStatsSizeRelation {
        id: Some(id),
        relations: Some(ModuleStatsSizeRelationPair {
            smaller_models: smaller_models.to_vec(),
            larger_models: larger_models.to_vec(),
        }),
    };
    assert_eq!(
        reply.size_relations,
        [relation(1, &[2], &[]), relation(2, &[], &[1])]
    );
    let valid: Vec<_> = (reply.valid_id.iter())
        .map(|item| (item.id.unwrap(), item.valid))
        .collect();
    assert!(valid.is_sorted() && valid.iter().all(|(_, valid)| *valid == Some(true)));
    let ids: Vec<_> = valid.iter().map(|(id, _)| *id).collect();
    assert!(ids.contains(&10003) && ids.contains(&27000) && ids.contains(&1));
}

/// Ticks the bridge until the service worker's next UpdaterForward (61) goes out.
fn next_updater_forward(
    control: &mut Control,
    state: &mut tetrad_app::device::State,
    now: Duration,
) -> tetrad_control::codecs::UpdaterForwardMessage {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let (sent, _) = replies(control.tick(state, now).unwrap());
        if let Some((ControlMessage::UpdaterForward(message), _)) = sent.into_iter().next() {
            return message;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("no UpdaterForward from the worker");
}

fn updater_answer(
    id: Option<u64>,
    payload: Option<&[u8]>,
    last: bool,
    error_code: Option<i32>,
) -> tetrad_control::codecs::UpdaterForwardMessage {
    tetrad_control::codecs::UpdaterForwardMessage {
        action: update(),
        updater_request_id: id,
        response: Some(tetrad_control::codecs::UpdaterForwardResponse {
            payload: payload.map(<[u8]>::to_vec),
            is_last_chunk: Some(last),
            error_code,
        }),
        ..Default::default()
    }
}

/// The USB update path (M1401, `CloudUpdaterUSB`): the stock download code runs on a worker
/// with Cortex Control as its HTTP transport. The size header and the package travel in
/// UpdaterForward (61) chunks (`0x1c0188`, `0x1beba4`) straight into `update.bin`, then the
/// stock `gzip -t` check sets the downloaded flag. Once the services report the package,
/// Updater (60) READ shows it and an Install goes to the services bridge, which runs the
/// device install pre-steps; before that the gate (`0x1391b4` case 2) ignores Install.
/// Synthetic bytes and URL.
#[test]
fn update_package_arrives_over_control_and_install_goes_to_the_services_bridge() {
    use tetrad_app::device::control::AccountRequest;
    use tetrad_app::device::control::forward::ForwardHttp;
    use tetrad_control::codecs::{
        UpdaterCommand, UpdaterForwardRequestType, UpdaterMessage, UpdaterState, UpdaterStatus,
    };
    use tetrad_control::link::Notification;
    use tetrad_services::exec::{CancelToken, Output};
    use tetrad_services::fake::{MemFiles, ScriptedRunner};
    use tetrad_services::updater::cloud::{UpdateOffer, download_update};
    use tetrad_services::updater::paths;

    let (mut state, mut control) = loaded::session();
    let link = control.forward_link();
    let worker = std::thread::spawn(move || {
        let files = MemFiles::default();
        let mut http = ForwardHttp::new(link, files.clone());
        let mut runner = ScriptedRunner::default();
        runner.expect(
            "gzip -t /media/p4/UpdaterTemp/update.bin",
            Ok(Output::code(0)),
        );
        let offer = UpdateOffer {
            version_id: "9.9.9".into(),
            url: "https://dl.example.invalid/u.bin".into(),
            changelog_url: None,
        };
        let cancel = CancelToken::default();
        let result = download_update(&mut http, &mut files.clone(), &mut runner, &offer, &cancel);
        (
            result.map_err(|error| error.to_string()),
            files.get(paths::UPDATE_BIN),
            files.get(paths::DOWNLOADED_FLAG),
        )
    });

    let header = next_updater_forward(&mut control, &mut state, at(100));
    let request = header.request.clone().unwrap();
    assert_eq!(
        (
            request.request_type,
            request.timeout,
            request.url.as_deref()
        ),
        (
            Some(UpdaterForwardRequestType::DownloadUpdateHeader.into()),
            Some(60_000),
            Some("https://dl.example.invalid/u.bin")
        )
    );
    let size = updater_answer(
        header.updater_request_id,
        Some(br#"{"Content-Length": 4}"#),
        true,
        None,
    );
    loaded::host(&mut control, &mut state, size, at(110));

    let package = next_updater_forward(&mut control, &mut state, at(120));
    let request = package.request.clone().unwrap();
    assert_eq!(
        (request.request_type, request.timeout),
        (
            Some(UpdaterForwardRequestType::DownloadUpdate.into()),
            Some(1_800_000)
        )
    );
    assert_ne!(package.updater_request_id, header.updater_request_id);
    for (chunk, last) in [(&[0x1f, 0x8b][..], false), (&[0, 0][..], true)] {
        let answer = updater_answer(package.updater_request_id, Some(chunk), last, None);
        loaded::host(&mut control, &mut state, answer, at(130));
    }
    let (result, bin, flag) = worker.join().unwrap();
    assert_eq!(result, Ok(()));
    assert_eq!(bin.as_deref(), Some(&[0x1f, 0x8b, 0, 0][..]));
    assert_eq!(flag.as_deref(), Some(&b"9.9.9"[..]));

    let install = || UpdaterMessage {
        action: update(),
        command: Some(UpdaterCommand::Install.into()),
        ..Default::default()
    };
    let routed = |effects: &[Effect]| {
        effects
            .iter()
            .filter(|effect| matches!(effect, Effect::ControlAccount(AccountRequest::Updater(_))))
            .count()
    };
    let (_, effects) = loaded::host(&mut control, &mut state, install(), at(140));
    assert_eq!(routed(&effects), 0);
    let downloaded = UpdaterMessage {
        action: update(),
        status: Some(UpdaterStatus::NewVersionDownloaded.into()),
        new_version_id: Some("9.9.9".into()),
        changelog: Some(String::new()),
        ..Default::default()
    };
    let notification = Notification::Update(ControlMessage::Updater(downloaded));
    control.notify(notification, &mut state, at(150)).unwrap();
    let read = UpdaterMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let (sent, _) = loaded::host(&mut control, &mut state, read, at(160));
    let [ControlMessage::Updater(view)] = &sent[..] else {
        panic!("one Updater reply: {sent:?}");
    };
    assert_eq!(
        (view.state, view.status, view.new_version_id.as_deref()),
        (
            Some(UpdaterState::Idle.into()),
            Some(UpdaterStatus::NewVersionDownloaded.into()),
            Some("9.9.9")
        )
    );
    let (_, effects) = loaded::host(&mut control, &mut state, install(), at(170));
    assert!(
        effects.contains(&Effect::ControlAccount(AccountRequest::Updater(
            tetrad_ui::UpdaterCommand::Install
        )))
    );
    // RestartDownloadViaControl (`0x4e4cc0` → `0x4e7808`) hands the check and download to the
    // services worker with Cortex Control as the transport.
    let restart = UpdaterMessage {
        action: update(),
        command: Some(UpdaterCommand::RestartDownloadViaControl.into()),
        ..Default::default()
    };
    let (_, effects) = loaded::host(&mut control, &mut state, restart, at(180));
    assert!(effects.contains(&Effect::ControlAccount(
        AccountRequest::RestartDownloadViaControl
    )));
}

/// `0x1beba4`: an UpdaterForward response without `payload` is the last one, so a Control
/// error (here code 7) fails the waiting call at once.
#[test]
fn updater_forward_error_without_payload_fails_the_call() {
    use tetrad_app::device::control::forward::ForwardHttp;
    use tetrad_services::http::{HttpClient, HttpError, HttpRequest, Method, Route, UpdaterOp};

    let (mut state, mut control) = loaded::session();
    let link = control.forward_link();
    let worker = std::thread::spawn(move || {
        let request = HttpRequest::new(
            Method::Get,
            "https://cloud.example.invalid/firmware/v1/latest",
            Route::Updater(UpdaterOp::Check),
            Duration::from_secs(60),
        );
        ForwardHttp::new(link, tetrad_services::fake::MemFiles::default()).send(&request)
    });
    let check = next_updater_forward(&mut control, &mut state, at(100));
    let error = updater_answer(check.updater_request_id, None, false, Some(7));
    loaded::host(&mut control, &mut state, error, at(110));
    assert_eq!(worker.join().unwrap(), Err(HttpError::Forward(7)));
}

/// `0x810694`: the `error_code` of a forward's last chunk is the HTTP status of the call, so
/// Cortex Control's 200 is a success and a 404 reaches the caller as that status with its body.
#[test]
fn forward_error_code_is_the_http_status() {
    use tetrad_app::device::control::forward::ForwardHttp;
    use tetrad_services::http::{HttpClient, HttpRequest, HttpResponse, Method, Route, UpdaterOp};

    let (mut state, mut control) = loaded::session();
    for (time, code) in [(100, 200), (200, 404)] {
        let link = control.forward_link();
        let worker = std::thread::spawn(move || {
            let request = HttpRequest::new(
                Method::Get,
                "https://cloud.example.invalid/firmware/v1/latest",
                Route::Updater(UpdaterOp::Check),
                Duration::from_secs(60),
            );
            ForwardHttp::new(link, tetrad_services::fake::MemFiles::default()).send(&request)
        });
        let check = next_updater_forward(&mut control, &mut state, at(time));
        let answer = updater_answer(check.updater_request_id, Some(b"{}"), true, Some(code));
        loaded::host(&mut control, &mut state, answer, at(time + 10));
        let expected = HttpResponse {
            status: code as u16,
            body: b"{}".to_vec(),
        };
        assert_eq!(worker.join().unwrap(), Ok(expected));
    }
}

/// Review r20 G8: after a Looper block is created (`+0xf058`), its first record goes out even
/// when it equals the last one (`0x513c94` → `0x18fa84`); a later equal record does not.
#[test]
fn first_looper_record_after_block_creation_is_pushed_even_when_equal() {
    use tetrad_control::codecs::LooperMessage;
    use tetrad_dsp_protocol::responses::LooperStatus;
    let (mut state, mut control) = loaded::session();
    let mut statuses = |state: &mut tetrad_app::device::State, now| -> usize {
        (loaded::sync(&mut control, state, at(now)).into_iter())
            .filter(|message| {
                matches!(
                    message,
                    ControlMessage::Looper(LooperMessage {
                        status: Some(_),
                        ..
                    })
                )
            })
            .count()
    };
    state.looper = Some(LooperStatus {
        state: 2,
        ..LooperStatus::from_words([0; 22])
    });
    assert_eq!(statuses(&mut state, 10), 1);
    state.looper_playback.adopt_next_state = true;
    assert_eq!(
        statuses(&mut state, 20),
        0,
        "no record of the new block yet"
    );
    state.looper_playback.adopt_next_state = false;
    assert_eq!(statuses(&mut state, 30), 1, "the new block's first record");
    assert_eq!(statuses(&mut state, 40), 0);
}

/// NeuralCapture (36) UPDATE `0x19ee00`: the first present field in stock order acts
/// (`state` before `toggle_ab_model`), `save_info` is the dialog save, other actions do
/// nothing, and the unbuilt `parameters` field is refused.
#[test]
fn neural_capture_update_takes_the_first_field_for_the_v1_session() {
    use tetrad_app::device::capture::NcRemote;
    use tetrad_capture::orchestration::Command;
    use tetrad_capture::session::SaveInfo;
    use tetrad_control::codecs::{
        NeuralCaptureMessage, NeuralCaptureParameter, NeuralCaptureSaveInfo,
    };
    let (mut state, mut control) = loaded::session();
    let mut remote = |message: NeuralCaptureMessage| {
        let (sent, effects) = loaded::host(&mut control, &mut state, message, at(10));
        assert!(sent.is_empty());
        effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::CaptureRemote(remote) => Some(remote),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let both = NeuralCaptureMessage {
        action: update(),
        state: Some(6),
        toggle_ab_model: Some(true),
        ..Default::default()
    };
    assert_eq!(
        remote(both.clone()),
        [NcRemote::Command(Command::RemoteState(6))]
    );
    let toggle = NeuralCaptureMessage {
        state: None,
        ..both.clone()
    };
    assert_eq!(remote(toggle), [NcRemote::Command(Command::ToggleAb(true))]);
    let save = NeuralCaptureMessage {
        action: update(),
        save_info: Some(NeuralCaptureSaveInfo {
            name: Some("Fake Amp".into()),
            device: Some(1),
            instrument: Some(2),
        }),
        ..Default::default()
    };
    assert_eq!(
        remote(save),
        [NcRemote::Save(SaveInfo {
            name: "Fake Amp".into(),
            device: 1,
            instrument: 2,
        })]
    );
    let read = NeuralCaptureMessage {
        action: Some(MessageAction::Read.into()),
        ..both
    };
    assert!(remote(read).is_empty());
    let parameter = NeuralCaptureMessage {
        action: update(),
        parameters: vec![NeuralCaptureParameter {
            index: Some(1),
            value: Some(0.5),
        }],
        ..Default::default()
    };
    assert!(remote(parameter).is_empty());
}

/// Screenshot (25) READ `0x1b6058`: a stored setlist preset renders as the offscreen grid
/// (`tetrad_ui::UiState::preset_grid`) without a deploy, and the push `0x1b6a5c` carries an
/// 800 × 384 RGBA PNG (colour type 6) with the request's folder and index. An empty slot is
/// refused.
#[test]
fn screenshot_read_renders_a_stored_preset_grid_as_an_rgba_png() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::ScreenshotMessage;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::{MediaRoot, SETLIST_SLOTS, encode_setlist};
    let root = std::env::temp_dir().join(format!("tetrad-app-control-shot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let setlist = root.join("Presets/Gig");
    std::fs::create_dir_all(&setlist).unwrap();
    let (mut state, mut control) = loaded::session();
    let stored = state.current.as_ref().unwrap().preset.to_bytes();
    std::fs::write(setlist.join("Lead.pb"), stored).unwrap();
    let mut slots = vec![None; SETLIST_SLOTS];
    slots[1] = Some("/media/p4/Presets/Gig/Lead.pb".to_owned());
    std::fs::write(setlist.join("setlist"), encode_setlist(&slots).unwrap()).unwrap();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: None,
    });
    let read = |index| ScreenshotMessage {
        action: Some(MessageAction::Read.into()),
        request_id: Some(9),
        folder_name: Some("Gig".into()),
        index: Some(index),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, read(1), at(10));
    assert!(sent.is_empty());
    let [ControlMessage::Screenshot(shot)] = &loaded::notified(&effects)[..] else {
        panic!("one Screenshot push: {effects:?}");
    };
    assert_eq!(
        (
            shot.action,
            shot.folder_name.as_deref(),
            shot.is_factory,
            shot.index
        ),
        (update(), Some("Gig"), Some(false), Some(1))
    );
    let png = shot.png.as_deref().unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let ihdr = &png[16..29];
    assert_eq!(u32::from_be_bytes(ihdr[0..4].try_into().unwrap()), 800);
    assert_eq!(u32::from_be_bytes(ihdr[4..8].try_into().unwrap()), 384);
    assert_eq!(ihdr[8..10], [8, 6]);
    let (_, effects) = loaded::host(&mut control, &mut state, read(2), at(20));
    assert!(loaded::notified(&effects).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// Review r20 G2 through Cortex Control: a File COPY or MOVE (`0x24c62c`, `0x24b804`) onto
/// the loaded preset's slot is refused (`actions::copy_preset`, as Save As), so the file the
/// next device save writes stays, and the moved source stays too.
#[test]
fn file_copy_or_move_onto_the_loaded_slot_is_refused() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
    use tetrad_library::sidecar::Sidecar;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    let root = std::env::temp_dir().join(format!("tetrad-app-control-g2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets/Gig")).unwrap();
    std::fs::create_dir_all(root.join("Presets/Tour")).unwrap();
    let (mut state, mut control) = loaded::session();
    let mut presets = StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap();
    let preset = state.current.as_ref().unwrap().preset.clone();
    let document =
        tetrad_preset::PresetDocument::parse(&preset.to_bytes(), tetrad_preset::Limits::default())
            .unwrap();
    let loaded = (presets.store("Gig", 0, "Lead", &document, &Sidecar::default()))
        .unwrap()
        .value;
    let other = (presets.store("Tour", 1, "Clean", &document, &Sidecar::default()))
        .unwrap()
        .value;
    state.current.get_mut().as_mut().unwrap().path = Some(loaded.clone());
    state.library = Some(Library {
        presets,
        database: None,
    });
    let folder = |setlist: &str, index: i32| FolderInfo {
        key: Some(format!("/media/p4/Presets/{setlist}")),
        files: vec![ProductData {
            index: Some(index),
            ..Default::default()
        }],
        ..Default::default()
    };
    for action in [MessageAction::Copy, MessageAction::Move] {
        let message = FileMessage {
            action: Some(action.into()),
            file_type: Some(0),
            folder: Some(folder("Tour", 1)),
            to_folder: Some(folder("Gig", 0)),
            ..Default::default()
        };
        let (sent, effects) = loaded::host(&mut control, &mut state, message, at(10));
        assert!(sent.is_empty() && loaded::notified(&effects).is_empty());
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::Notice(..))),
            "{action:?} refused: {effects:?}"
        );
        let library = &state.library.as_ref().unwrap().presets;
        assert_eq!(
            library.slots("Gig").unwrap()[0].as_deref(),
            Some(&loaded[..])
        );
        assert_eq!(
            library.slots("Tour").unwrap()[1].as_deref(),
            Some(&other[..])
        );
        assert!(library.load(&loaded).is_ok());
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// IOSettings (3) UPDATE EXP `calibrating` (`0x26acfc`): true starts the pedal's calibration
/// (`0x358330` → `0x32103c`), false ends it (`0x35828c` → `0x320a34` stores the tracked
/// range), as the device I/O page does; both echo the flag (`0x26fbd4`).
#[test]
fn exp_calibrating_update_runs_the_pedal_calibration_and_echoes_the_flag() {
    use tetrad_control::device::IoCommand;
    let (mut state, mut control) = loaded::session();
    for calibrating in [true, false] {
        let command = IoCommand::ExpCalibrating {
            pedal: 2,
            calibrating,
        };
        let (_, effects) = loaded::host(&mut control, &mut state, command.to_update(), at(10));
        assert!(effects.contains(&Effect::ExpressionCalibration {
            pedal: 2,
            calibrating
        }));
        let [ControlMessage::IOSettings(echo)] = &loaded::notified(&effects)[..] else {
            panic!("one IOSettings echo: {effects:?}");
        };
        assert_eq!(IoCommand::from_update(echo), Some(command));
    }
}

/// r21-services MR 3: the Excessive Downloads warning (`0x22cab0`). Without a connection it
/// waits (`0x1545925`); the next connect (`0x546b98`) checks the folder again (more than 512
/// presets, "Don't show again" off) and sends GenericError 3 with request ID 0 (absent on
/// the wire, `0x105d94`) once.
#[test]
fn excessive_downloads_warning_waits_for_the_next_connect() {
    use tetrad_app::device::Library;
    use tetrad_control::codecs::{GenericErrorCode, GenericErrorMessage};
    use tetrad_library::stock::{EXCESSIVE_DOWNLOADS, StockLibrary, StockLimits, store_download};
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, Download, DownloadKind, StockDb};
    let root =
        std::env::temp_dir().join(format!("tetrad-app-control-excess-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    database
        .transaction(|write| write.boot_maintenance())
        .unwrap();
    for n in 0..=EXCESSIVE_DOWNLOADS {
        let download = Download {
            id: format!("dl-{n}"),
            hash: None,
            raw_data: Some(vec![1]),
            metadata: Some(br#"{"name":"Cloud"}"#.to_vec()),
        };
        store_download(&mut database, DownloadKind::Preset, &download).unwrap();
    }
    let (mut state, mut control) = loaded::session();
    state.library = Some(Library {
        presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
        database: Some(database),
    });
    control.usb_unplugged(&mut state, at(10)).unwrap();
    let waiting = (control.excessive_downloads(DownloadKind::Preset, &mut state, at(20))).unwrap();
    assert!(waiting.is_empty());
    let connect = |control: &mut Control, state: &mut tetrad_app::device::State, now| {
        let mut effects = Vec::new();
        for message in [
            ControlMessage::from(KeepAliveMessage {
                action: update(),
                ..Default::default()
            }),
            ModelRepoMessage {
                action: Some(MessageAction::Read.into()),
                ..Default::default()
            }
            .into(),
            ConnectionMessage {
                connected: Some(true),
                ..Default::default()
            }
            .into(),
        ] {
            for report in send(message) {
                effects.extend(control.on_report(&report, state, at(now)).unwrap());
            }
        }
        (replies(effects).0.into_iter())
            .filter_map(|(message, _)| match message {
                ControlMessage::GenericError(error) => Some(error),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        connect(&mut control, &mut state, 30),
        [GenericErrorMessage {
            request_id: None,
            error_code: Some(GenericErrorCode::ExcessDownloadsPresetsWarningTriggered.into()),
        }]
    );
    control.usb_unplugged(&mut state, at(40)).unwrap();
    assert!(
        connect(&mut control, &mut state, 50).is_empty(),
        "the flag cleared"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// CloudBackup (41) `0x14e1e0`: logged out answers 17 (`0x7166d0`); CREATE suspends the link
/// and hands the request to the services bridge; a second request while it runs answers 0
/// (`0x47fb9c`); the result answers its code and resumes. A DELETE of an unlisted key ends
/// NotFound (13). READ lists with code 16 and one item per services backup (`0x14b8bc`).
/// A request without a result fails at its deadline (O). A READ during a CREATE answers from
/// the last list and keeps the CREATE running (review r23 K6).
#[test]
fn cloud_backup_requests_reach_services_and_answer_the_stock_codes() {
    use tetrad_app::device::control::{
        AccountRequest, CloudBackupDone, CloudBackupRequest, CommsCipher,
    };
    use tetrad_app::device::services::CloudBackup;
    use tetrad_control::codecs::{
        CloudBackupErrorCode as Code, CloudBackupItem, CloudBackupMessage, EnumValue,
        VersionMessage,
    };
    use tetrad_dsp_protocol::responses::UsbMeter;
    use tetrad_media::factory::DeviceSerial;
    // CloudBackup is a transform type: the fake key exchange installs the comms transform.
    let (mut state, mut control) = loaded::session();
    control.set_device_serial(DeviceSerial::parse(b"TEST12345").unwrap());
    let version = VersionMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    loaded::host(&mut control, &mut state, version, at(1));
    let material = UsbMeter(*b"fake vector B 16");
    let key = UsbMeter([1, 2, 3, 0, 4, 4, 4, 4, 9, 9, 9, 9, 9, 9, 9, 9]);
    (control.complete_key_exchange(&key, &material, &mut state, at(2))).unwrap();
    let cipher = CommsCipher::derive(&material.0);
    let decoded = |effects: &[Effect]| -> Vec<ControlMessage> {
        (sealed_replies(effects, &cipher).into_iter())
            .map(|(message, _)| message)
            .collect()
    };
    let host = |control: &mut Control,
                state: &mut tetrad_app::device::State,
                message: CloudBackupMessage,
                now| {
        let mut effects = Vec::new();
        for report in send(message) {
            effects.extend(control.on_report(&report, state, at(now)).unwrap());
        }
        let other = (effects.iter())
            .filter(|effect| {
                !matches!(
                    effect,
                    Effect::ControlReport(_) | Effect::ControlRequestDone
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        (decoded(&effects), other)
    };
    let request =
        |action: MessageAction, name: Option<&str>, key: Option<&str>| CloudBackupMessage {
            action: Some(action.into()),
            backups: vec![CloudBackupItem {
                name: name.map(str::to_owned),
                key: key.map(str::to_owned),
                ..Default::default()
            }],
            ..Default::default()
        };
    let codes = |sent: &[ControlMessage]| -> Vec<Option<EnumValue<Code>>> {
        (sent.iter())
            .filter_map(|m| match m {
                ControlMessage::CloudBackup(m) => Some(m.error_code),
                _ => None,
            })
            .collect()
    };
    let suspends = |sent: &[ControlMessage]| -> Vec<bool> {
        (sent.iter())
            .filter_map(|m| match m {
                ControlMessage::SuspendConnection(m) => m.suspend,
                _ => None,
            })
            .collect()
    };
    let create = request(MessageAction::Create, Some("Rig"), None);
    let (sent, effects) = host(&mut control, &mut state, create.clone(), 10);
    assert_eq!(codes(&sent), [Some(Code::NotConnectedOrLoggedIn.into())]);
    assert!(effects.is_empty(), "{effects:?}");

    state
        .settings
        .update(|settings| settings.account.username = "fake-user".into());
    let (sent, effects) = host(&mut control, &mut state, create.clone(), 20);
    assert_eq!(suspends(&sent), [true]);
    let [Effect::ControlAccount(AccountRequest::CloudBackup(CloudBackupRequest::Create { name }))] =
        effects.as_slice()
    else {
        panic!("{effects:?}");
    };
    assert_eq!(name, "Rig");
    let (busy, _) = host(&mut control, &mut state, create, 30);
    assert_eq!(codes(&busy), [Some(Code::BackupOperationInProgress.into())]);
    let read = request(MessageAction::Read, None, None);
    let (_, effects) = host(&mut control, &mut state, read.clone(), 35);
    let [Effect::CloudBackupDone(listed)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    let done = control.cloud_backup_done(listed.clone(), &[], &mut state, at(36));
    let sent = decoded(&done.unwrap());
    assert_eq!(codes(&sent), [Some(Code::BackupListingSucceeded.into())]);
    assert!(suspends(&sent).is_empty());
    let done = control.cloud_backup_done(CloudBackupDone::Done, &[], &mut state, at(40));
    let sent = decoded(&done.unwrap());
    assert_eq!(codes(&sent), [Some(Code::BackupCreationSucceeded.into())]);
    assert_eq!(suspends(&sent), [false]);

    let delete = request(MessageAction::Delete, None, Some("backups/gone"));
    let (_, effects) = host(&mut control, &mut state, delete, 50);
    assert!(matches!(
        effects.as_slice(),
        [Effect::ControlAccount(AccountRequest::CloudBackup(
            CloudBackupRequest::Delete { .. }
        ))]
    ));
    let done = control.cloud_backup_done(CloudBackupDone::NotFound, &[], &mut state, at(60));
    let sent = decoded(&done.unwrap());
    assert_eq!(codes(&sent), [Some(Code::BackupDeleteNotFound.into())]);

    let (_, effects) = host(&mut control, &mut state, read, 70);
    assert!(matches!(
        effects.as_slice(),
        [Effect::ControlAccount(AccountRequest::CloudBackup(
            CloudBackupRequest::List
        ))]
    ));
    let listed = [CloudBackup {
        id: 7,
        path: "backups/one".into(),
        title: "One".into(),
        detail: "2026-10-12T10:00:00Z".into(),
        downloaded: None,
    }];
    let done = control.cloud_backup_done(CloudBackupDone::Done, &listed, &mut state, at(80));
    let sent = decoded(&done.unwrap());
    let [ControlMessage::CloudBackup(list)] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(list.error_code, Some(Code::BackupListingSucceeded.into()));
    assert_eq!(
        list.backups,
        [CloudBackupItem {
            name: Some("One".into()),
            key: Some("backups/one".into()),
            timestamp: Some("2026-10-12T10:00:00Z".into()),
            ..Default::default()
        }]
    );

    // census-r38 L1: UPDATE with a `key` updates (`0x14e2d0`: 5, 4, 3); with a `name` too it
    // renames (`0x150c08`: 8, 7, 6, 15), and a good rename sends the new list (`0x152218`). Both
    // run under the Notice suspension.
    let refresh = request(MessageAction::Update, None, Some("backups/one"));
    let (sent, effects) = host(&mut control, &mut state, refresh, 82);
    assert_eq!(suspends(&sent), [true]);
    let [Effect::ControlAccount(AccountRequest::CloudBackup(CloudBackupRequest::Update { key }))] =
        effects.as_slice()
    else {
        panic!("{effects:?}");
    };
    assert_eq!(key, "backups/one");
    let done = control.cloud_backup_done(CloudBackupDone::NotFound, &[], &mut state, at(83));
    let sent = decoded(&done.unwrap());
    assert_eq!(codes(&sent), [Some(Code::BackupUpdateNotFound.into())]);
    assert_eq!(suspends(&sent), [false]);
    let rename = request(MessageAction::Update, Some("Two"), Some("backups/one"));
    let (sent, effects) = host(&mut control, &mut state, rename, 84);
    assert_eq!(suspends(&sent), [true]);
    let [
        Effect::ControlAccount(AccountRequest::CloudBackup(CloudBackupRequest::Rename {
            key,
            name,
        })),
    ] = effects.as_slice()
    else {
        panic!("{effects:?}");
    };
    assert_eq!((key.as_str(), name.as_str()), ("backups/one", "Two"));
    let done = control.cloud_backup_done(CloudBackupDone::Done, &listed, &mut state, at(85));
    let sent = decoded(&done.unwrap());
    assert_eq!(codes(&sent), [Some(Code::BackupRenameSucceeded.into())]);
    let renamed = (sent.iter()).find_map(|message| match message {
        ControlMessage::CloudBackup(message) => Some(message.backups.len()),
        _ => None,
    });
    assert_eq!(renamed, Some(1));
    assert_eq!(suspends(&sent), [false]);
    // Review r39 K11: a failed list of a rename, before or after its POST, answers 15 with no
    // `backups` (C: `0x150c08`, `0x152218`) and resumes.
    let rename = request(MessageAction::Update, Some("Three"), Some("backups/one"));
    host(&mut control, &mut state, rename, 86);
    let failed = CloudBackupDone::ListFailed("made-up".into());
    let done = control.cloud_backup_done(failed, &listed, &mut state, at(87));
    let sent = decoded(&done.unwrap());
    assert_eq!(codes(&sent), [Some(Code::BackupListingFailed.into())]);
    let listed_again = (sent.iter()).find_map(|message| match message {
        ControlMessage::CloudBackup(message) => Some(message.backups.len()),
        _ => None,
    });
    assert_eq!(listed_again, Some(0));
    assert_eq!(suspends(&sent), [false]);

    // O: a CREATE without a services result fails at its deadline and resumes the link.
    let create = request(MessageAction::Create, Some("Late"), None);
    host(&mut control, &mut state, create, 90);
    let alive = KeepAliveMessage {
        action: update(),
        ..Default::default()
    };
    loaded::host(&mut control, &mut state, alive, at(90 + 1_799_999));
    let late = control.tick(&mut state, at(90 + 1_800_000)).unwrap();
    let sent = decoded(&late);
    assert_eq!(codes(&sent), [Some(Code::BackupCreationFailed.into())]);
    assert_eq!(suspends(&sent), [false]);
}

/// re-r38 §3: a question of a Control-started upload goes to Cortex Control as Confirmation
/// (47) (`0xfa48c`): [Ok, Cancel], "OK" and "Cancel" ("SKIP" in a batch). Ok overwrites
/// (`0x4635f0`), Cancel skips (`0x463700`); a disconnect skips every open question (`0xf9994`);
/// not connected, the answer is SKIP at once (O: stock hangs). A question is asked once.
#[test]
fn a_control_upload_asks_overwrite_through_confirmation_47() {
    use tetrad_app::device::control::{AccountRequest, UploadAsk};
    use tetrad_control::codecs::{ConfirmationAction, ConfirmationMessage};
    use tetrad_services::cloud::services::ProductKind;
    let (mut state, mut control) = loaded::session();
    let ask = |kind, question, batch| UploadAsk {
        kind,
        question,
        title: "Neural Capture already exists".into(),
        text: "Neural Capture Rig already exists in your account. Would you like to overwrite it?"
            .into(),
        batch,
    };
    let asked = |control: &mut Control, state: &mut tetrad_app::device::State, ask, now| {
        let effects = control.upload_question(ask, state, at(now)).unwrap();
        let (sent, other) = replies(effects);
        let sent: Vec<_> = sent.into_iter().map(|(message, _)| message).collect();
        (sent, other)
    };
    let answer = |question, overwrite| {
        vec![Effect::ControlAccount(AccountRequest::UploadAnswer {
            question,
            overwrite,
        })]
    };
    let reply = |id, response: Option<ConfirmationAction>| ConfirmationMessage {
        confirmation_id: id,
        response: response.map(Into::into),
        ..Default::default()
    };

    let (sent, effects) = asked(
        &mut control,
        &mut state,
        Some(ask(ProductKind::NeuralCapture, 1, false)),
        10,
    );
    let [ControlMessage::Confirmation(form)] = &sent[..] else {
        panic!("one Confirmation: {sent:?} {effects:?}");
    };
    assert!(effects.is_empty(), "{effects:?}");
    assert_eq!(form.title.as_deref(), Some("Neural Capture already exists"));
    assert!(
        form.msg
            .as_deref()
            .unwrap()
            .starts_with("Neural Capture Rig already exists")
    );
    let actions = [ConfirmationAction::Ok, ConfirmationAction::Cancel].map(Into::into);
    assert_eq!(form.actions, actions);
    let titles = (form.ok_title.as_deref(), form.cancel_title.as_deref());
    assert_eq!(titles, (Some("OK"), Some("Cancel")));
    let again = asked(
        &mut control,
        &mut state,
        Some(ask(ProductKind::NeuralCapture, 1, false)),
        11,
    );
    assert!(again.0.is_empty() && again.1.is_empty(), "{again:?}");
    let ok = reply(form.confirmation_id, Some(ConfirmationAction::Ok));
    let (_, effects) = loaded::host(&mut control, &mut state, ok.clone(), at(12));
    assert_eq!(effects, answer(1, true));
    let (_, effects) = loaded::host(&mut control, &mut state, ok, at(13));
    let answers = effects
        .iter()
        .filter(|effect| matches!(effect, Effect::ControlAccount(_)));
    assert_eq!(answers.count(), 0, "one answer per question: {effects:?}");

    // A batch preset question: "SKIP"; no response is Cancel.
    asked(&mut control, &mut state, None, 20);
    let (sent, _) = asked(
        &mut control,
        &mut state,
        Some(ask(ProductKind::Preset, 2, true)),
        21,
    );
    let [ControlMessage::Confirmation(form)] = &sent[..] else {
        panic!("{sent:?}");
    };
    assert_eq!(form.cancel_title.as_deref(), Some("SKIP"));
    let skip = reply(form.confirmation_id, None);
    let (_, effects) = loaded::host(&mut control, &mut state, skip, at(22));
    assert_eq!(effects, answer(2, false));

    // A disconnect skips the open question; not connected, a new one is skipped at once.
    asked(&mut control, &mut state, None, 30);
    let (sent, _) = asked(
        &mut control,
        &mut state,
        Some(ask(ProductKind::NeuralCapture, 3, false)),
        31,
    );
    assert_eq!(sent.len(), 1, "{sent:?}");
    let effects = control.usb_unplugged(&mut state, at(32)).unwrap();
    let skipped = (effects.iter()).filter(|effect| matches!(effect, Effect::ControlAccount(_)));
    let skipped: Vec<_> = skipped.cloned().collect();
    assert_eq!(skipped, answer(3, false));
    asked(&mut control, &mut state, None, 33);
    let (sent, effects) = asked(
        &mut control,
        &mut state,
        Some(ask(ProductKind::Preset, 4, false)),
        34,
    );
    assert!(sent.is_empty(), "{sent:?}");
    assert_eq!(effects, answer(4, false));
}

/// A signed-in online unit with Cortex Control connected and the user capture "Amp" whose
/// key the account has under other tags (status 8, re-r38 §3): every upload of it asks. Made-up
/// capture, cipher and cloud records.
struct ControlCaptureUnit {
    device: tetrad_app::device::Device<common::Sim>,
    log: std::rc::Rc<std::cell::RefCell<common::Log>>,
    cloud: std::rc::Rc<std::cell::RefCell<common::Cloud>>,
    hash: String,
    _media: common::Media,
}

impl ControlCaptureUnit {
    fn new() -> Self {
        use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
        use tetrad_app::device::platform::{Platform as _, ServiceBackends as _};
        use tetrad_media::factory::DeviceCipher;
        use tetrad_services::http::Method;
        use tetrad_storage::stock::{
            CaptureEncryption, CaptureStore, DbLimits, ProductType, StockDb, StockProduct,
        };
        let hash = "c".repeat(64);
        let media =
            common::Media::new([("Light", common::light_preset(&common::STOCK.specs(), 0))]);
        let cipher = DeviceCipher::caller_secret(b"invented test secret");
        let sealed = |plain: &[u8]| {
            let mut bytes = plain.to_vec();
            cipher.apply(&mut bytes);
            bytes
        };
        let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
        db.transaction(|write| {
            write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
            let root = write.boot_maintenance()?.0;
            write.put_product(&StockProduct {
                id: hash.clone(),
                product_type: ProductType::Capture,
                read_only: false,
                raw_data: Some(sealed(&common::sound_v1_capture())),
                metadata: Some(sealed(br#"{"name":"Amp","tags":["clean"]}"#)),
            })?;
            write.link(root, &hash)
        })
        .unwrap();
        drop(db);
        let log = std::rc::Rc::new(std::cell::RefCell::new(common::Log::default()));
        let mut sim = common::Sim::new(common::STOCK, &media, &log);
        sim.media_cipher = Some(cipher.clone());
        let files = sim.services().files().clone();
        for marker in [CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED] {
            files.put(marker, "");
        }
        let cloud = sim.cloud.clone();
        let mut device = common::start_online(sim);
        device.set_capture_seed(DeviceCipher::caller_secret(b"invented seed-only secret"));
        let mut routes = cloud.borrow_mut();
        let mine = r#"{"existing":[{"cloud_id":"c-1","author":true}]}"#;
        routes.route(Method::Get, "/products/exists?hash=", 200, mine);
        let taken = r#"{"product_id":"c-1"}"#;
        routes.route(Method::Get, "/user/products/exists?name=", 200, taken);
        let record = r#"{"id":"c-1","type":"neural_capture","name":"Amp","tags":["lead"]}"#;
        routes.route(Method::Get, "/products/c-1", 200, record);
        routes.route(Method::Put, "/user/products/c-1", 200, "{}");
        drop(routes);
        common::connect_control(&mut device);
        Self {
            device,
            log,
            cloud,
            hash,
            _media: media,
        }
    }

    fn puts(&self) -> usize {
        let put = tetrad_services::http::Method::Put;
        self.cloud.borrow().sent(put, "/user/products/c-1").len()
    }

    /// The Confirmation (47) forms sent since `mark`.
    fn forms(&self, mark: usize) -> Vec<tetrad_control::codecs::ConfirmationMessage> {
        (common::control_sent(&self.log.borrow(), mark).into_iter())
            .filter_map(|message| match message {
                ControlMessage::Confirmation(form) => Some(form),
                _ => None,
            })
            .collect()
    }

    /// One Control 46 UPLOAD of the capture: the question goes to Control; returns its
    /// `confirmation_id`.
    fn control_ask(&mut self) -> Option<u64> {
        let mark = self.log.borrow().requests.len();
        let upload = tetrad_control::codecs::CloudProductMessage {
            action: Some(MessageAction::Upload.into()),
            product_keys: vec![self.hash.clone()],
            product_type: Some(2),
            ..Default::default()
        };
        common::host_send(&mut self.device, upload);
        (0..3).for_each(|_| _ = self.device.step());
        let question = self.device.bridges().services.capture_upload_question();
        assert_eq!(question, Some("Amp"));
        let forms = self.forms(mark);
        let [form] = &forms[..] else {
            panic!("one Confirmation: {forms:?}");
        };
        assert_eq!(form.title.as_deref(), Some("Neural Capture already exists"));
        assert_eq!(form.cancel_title.as_deref(), Some("Cancel"));
        form.confirmation_id
    }

    fn control_answer(
        &mut self,
        id: Option<u64>,
        response: tetrad_control::codecs::ConfirmationAction,
    ) {
        let reply = tetrad_control::codecs::ConfirmationMessage {
            confirmation_id: id,
            response: Some(response.into()),
            ..Default::default()
        };
        common::host_send(&mut self.device, reply);
        (0..3).for_each(|_| _ = self.device.step());
    }
}

/// re-r38 §3 on the device: a Control 46 capture UPLOAD whose capture is on the account under
/// other tags [8] asks in Cortex Control (Confirmation 47, `0xfa48c`). OK puts to the found id
/// (`0x1e2924`); Cancel and a disconnect send nothing and end the question (`0x463700`,
/// `0xf9994`).
#[test]
fn a_control_capture_upload_overwrites_only_on_ok_in_cortex_control() {
    use tetrad_control::codecs::ConfirmationAction;
    let mut unit = ControlCaptureUnit::new();
    let id = unit.control_ask();
    unit.control_answer(id, ConfirmationAction::Cancel);
    assert_eq!(
        unit.device.bridges().services.capture_upload_question(),
        None
    );
    assert_eq!(unit.puts(), 0, "Cancel sends nothing");

    let id = unit.control_ask();
    unit.control_answer(id, ConfirmationAction::Ok);
    assert_eq!(
        unit.device.bridges().services.capture_upload_question(),
        None
    );
    assert_eq!(unit.puts(), 1, "OK puts to the found id");

    unit.control_ask();
    let disconnect = tetrad_control::codecs::ConnectionMessage {
        connected: Some(false),
        ..Default::default()
    };
    common::host_send(&mut unit.device, disconnect);
    (0..3).for_each(|_| _ = unit.device.step());
    assert!(!unit.device.bridges().control.connected());
    assert_eq!(
        unit.device.bridges().services.capture_upload_question(),
        None
    );
    assert_eq!(unit.puts(), 1, "a disconnect sends nothing");
}

/// Review r39 K3 (`0x463d2c` takes the Control flag per call): the route of an upload question
/// belongs to the upload that asks it. A device upload that is refused (here before the
/// network: the preset is not in the library) does not move a waiting Control question to the
/// device screen; Control's OK still overwrites, and the device shows nothing.
#[test]
fn a_refused_device_upload_keeps_a_control_question_in_control() {
    use tetrad_control::codecs::ConfirmationAction;
    use tetrad_dsp_host::session::Clock as _;
    use tetrad_ui::UploadItem;
    let mut unit = ControlCaptureUnit::new();
    let id = unit.control_ask();
    let mark = unit.log.borrow().requests.len();
    let now = unit.device.platform().clock.now();
    let gone = UploadItem::Preset(format!("{}/Gone.pb", common::SETLIST));
    _ = unit.device.upload(vec![gone], now);
    (0..3).for_each(|_| _ = unit.device.step());
    let prompt = unit.device.bridges().services.upload_prompt();
    assert!(
        prompt.is_some_and(|question| question.from_control),
        "{prompt:?}"
    );
    let snapshot = unit.device.bridges().ui.view().snapshot();
    assert_eq!(snapshot.capture_upload_overwrite, None, "not on the device");
    assert!(unit.forms(mark).is_empty(), "asked once");
    unit.control_answer(id, ConfirmationAction::Ok);
    assert_eq!(unit.puts(), 1, "Control's OK overwrites");
}

/// Review r41 L3 (r41-upload §1, O): a refused start changes no other upload, also a question
/// of the same kind from the other side. A device upload of a capture in no user store is
/// refused before the network; the Control capture question still waits in Control and its OK
/// still overwrites.
#[test]
fn a_refused_device_capture_upload_keeps_the_control_capture_question() {
    use tetrad_control::codecs::ConfirmationAction;
    use tetrad_dsp_host::session::Clock as _;
    use tetrad_ui::UploadItem;
    let mut unit = ControlCaptureUnit::new();
    let id = unit.control_ask();
    let now = unit.device.platform().clock.now();
    _ = unit
        .device
        .upload(vec![UploadItem::Capture("d".repeat(64))], now);
    (0..3).for_each(|_| _ = unit.device.step());
    let services = &unit.device.bridges().services;
    assert_eq!(services.capture_upload_question(), Some("Amp"));
    unit.control_answer(id, ConfirmationAction::Ok);
    assert_eq!(unit.puts(), 1, "Control's OK overwrites");
}

/// Review r39 K3 (r41-upload MR 1): Control's answer reaches its own question
/// (`Services::answer_upload`). When it ends a batch item, the next item starts, as after a
/// device answer (`Effect::UploadNext`). Here a Control batch holds the capture twice: the first
/// asks with "SKIP", the SKIP serves the second, and the batch ends with nothing put.
#[test]
fn a_control_skip_in_a_batch_starts_the_next_item() {
    use tetrad_control::codecs::ConfirmationAction;
    let mut unit = ControlCaptureUnit::new();
    let mark = unit.log.borrow().requests.len();
    let upload = tetrad_control::codecs::CloudProductMessage {
        action: Some(MessageAction::Upload.into()),
        product_keys: vec![unit.hash.clone(), unit.hash.clone()],
        product_type: Some(2),
        ..Default::default()
    };
    common::host_send(&mut unit.device, upload);
    (0..3).for_each(|_| _ = unit.device.step());
    let forms = unit.forms(mark);
    let [form] = &forms[..] else {
        panic!("one Confirmation: {forms:?}");
    };
    assert_eq!(form.cancel_title.as_deref(), Some("SKIP"));
    unit.control_answer(form.confirmation_id, ConfirmationAction::Cancel);
    let services = &unit.device.bridges().services;
    assert!(
        !services.upload_batch_running(),
        "{:?}",
        services.upload_progress()
    );
    assert!(services.last_batch_end().is_some());
    assert_eq!(unit.puts(), 0);
}

/// BulkOperation (57) READ `0xed8d0`: `is_multiselection_active` follows Directory Multi
/// Select, which the device sets before each host report.
#[test]
fn bulk_operation_read_reports_directory_multi_select() {
    use tetrad_control::codecs::BulkOperationMessage;
    let (mut state, mut control) = loaded::session();
    let read = BulkOperationMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let mut active = Vec::new();
    for on in [false, true] {
        control.set_multi_select(on);
        let (sent, _) = loaded::host(&mut control, &mut state, read.clone(), at(10));
        active.extend(sent.iter().filter_map(|m| match m {
            ControlMessage::BulkOperation(m) => m.is_multiselection_active,
            _ => None,
        }));
    }
    assert_eq!(active, [false, true]);
}

/// Logs (44) CREATE `0x114cd4` goes to the services support-report upload (no reply), and
/// GainCalibration (62) never reaches a U-Boot write: on an ESS board it is refused.
#[test]
fn logs_go_to_the_report_upload_and_gain_calibration_is_refused() {
    use tetrad_app::device::control::AccountRequest;
    use tetrad_control::codecs::{GainCalibrationMessage, LogsMessage};
    let (mut state, mut control) = loaded::session();
    let logs = LogsMessage {
        action: Some(MessageAction::Create.into()),
        correlation_id: Some("host-id".into()),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, logs, at(10));
    assert!(sent.is_empty());
    assert!(matches!(
        effects.as_slice(),
        [Effect::ControlAccount(AccountRequest::UploadLogs)]
    ));
    state.platform = tetrad_hal::platform::Platform::QcEss;
    let save = GainCalibrationMessage {
        action: Some(MessageAction::Copy.into()),
        ..Default::default()
    };
    let (sent, effects) = loaded::host(&mut control, &mut state, save, at(20));
    assert!(sent.is_empty());
    let [Effect::Notice(_, notice)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert!(notice.contains("U-Boot"), "{notice}");
}

/// re-r44 §3: `0x80da7c` sends a cloud call over Cortex Control while it is connected
/// (`0x6e0444`) and its KeepAlive says online (`0x6e045c`); `Control::cloud_route` gives the
/// services that route, always the same handle (equality is identity). An offline KeepAlive
/// and an unplug take it away. The link has its transform (review-r45 M4; `r47::a_link_
/// without_its_key_is_no_cloud_route_and_its_forward_fails_at_once`).
#[test]
fn cloud_route_is_control_while_connected_and_online() {
    use tetrad_control::codecs::KeepAliveMessage;
    let (mut state, mut control) = loaded::session();
    let cipher = tetrad_app::device::control::CommsCipher::derive(b"fake key-exchange vector");
    control.set_transform(Some(cipher));
    let route = control.cloud_route().expect("connected and online");
    assert_eq!(control.cloud_route(), Some(route.clone()));
    let keep_alive = |online| KeepAliveMessage {
        action: update(),
        is_online: Some(online),
        ..Default::default()
    };
    loaded::host(&mut control, &mut state, keep_alive(false), at(10));
    assert_eq!(control.cloud_route(), None, "offline");
    loaded::host(&mut control, &mut state, keep_alive(true), at(20));
    assert_eq!(control.cloud_route(), Some(route));
    control.usb_unplugged(&mut state, at(30)).unwrap();
    assert_eq!(control.cloud_route(), None, "unplugged");
}

mod bulk {
    use tetrad_app::device::control::Control;
    use tetrad_app::device::services::{BatchOperation, BatchView};
    use tetrad_app::device::{Effect, State};
    use tetrad_control::codecs::{BulkOperationMessage, ControlMessage, FolderInfo, MessageAction};
    use tetrad_services::cloud::services::ProductKind;

    use super::{at, loaded, replies, update};

    /// One device step's read of the batch ([`Control::bulk_upload`]): the pushes, decoded.
    pub fn step(
        control: &mut Control,
        state: &mut State,
        view: Option<BatchView>,
        ms: u64,
    ) -> Vec<BulkOperationMessage> {
        let effects: Vec<Effect> = control.bulk_run(view, state, at(ms)).unwrap();
        (replies(effects).0.into_iter())
            .map(|(message, _)| match message {
                ControlMessage::BulkOperation(message) => message,
                other => panic!("not a BulkOperation: {other:?}"),
            })
            .collect()
    }

    pub fn view(kind: ProductKind, handled: usize, total: usize) -> Option<BatchView> {
        Some(BatchView {
            serial: 1,
            operation: BatchOperation::Upload,
            kind,
            handled,
            total,
            source: None,
        })
    }

    /// The cloud folder of the start push: `is_cloud` (O: key and name are the kind texts;
    /// stock reads `CloudProductFolder +0x44`, I).
    pub fn cloud_folder(key: &str, name: &str) -> FolderInfo {
        FolderInfo {
            key: Some(key.into()),
            name: Some(name.into()),
            is_factory: Some(false),
            is_downloads: Some(false),
            is_user_default: Some(false),
            is_cloud: Some(true),
            is_favorites: Some(false),
            is_plugin: Some(false),
            ..Default::default()
        }
    }

    pub fn progress(value: f32) -> BulkOperationMessage {
        BulkOperationMessage {
            action: update(),
            progress: Some(value),
            ..Default::default()
        }
    }

    pub fn finished() -> BulkOperationMessage {
        BulkOperationMessage {
            action: Some(MessageAction::Delete.into()),
            finished: Some(true),
            ..Default::default()
        }
    }

    /// re-r44 §2: a 3-capture batch goes to Control as CREATE (`0xf1cec`: text, blocking,
    /// cancellable, 0.0, the cloud folder, type 2), one UPDATE with only `progress` per item
    /// start (`0x77fef0`: total ≤ 399, so every item: 1/3, 2/3, 1.0), then DELETE with only
    /// `finished` (`0xf29b8`). A step with no new item sends nothing. Not connected
    /// (`0x6e0444`): a batch sends nothing.
    #[test]
    fn a_batch_upload_pushes_start_progress_and_finished_to_control() {
        let (mut state, mut control) = loaded::session();
        let captures = |handled| view(ProductKind::NeuralCapture, handled, 3);
        let mut sent = Vec::new();
        for (ms, view) in [captures(1), captures(1), captures(2), captures(3), None]
            .into_iter()
            .zip((10..).step_by(10))
            .map(|(view, ms)| (ms, view))
        {
            sent.extend(step(&mut control, &mut state, view, ms));
        }
        let create = BulkOperationMessage {
            action: Some(MessageAction::Create.into()),
            progress_message: Some("Uploading Neural Captures, please wait.".into()),
            blocking: Some(true),
            cancellable: Some(true),
            progress: Some(0.0),
            operation_type: Some(2),
            destination_folder: Some(cloud_folder("neural_capture", "Neural Captures")),
            ..Default::default()
        };
        let expected = [
            create,
            progress(1.0 / 3.0),
            progress(2.0 / 3.0),
            progress(1.0),
            finished(),
        ];
        assert_eq!(sent, expected);
        control.usb_unplugged(&mut state, at(60)).unwrap();
        for (ms, view) in [(70, captures(1)), (80, None)] {
            assert!(step(&mut control, &mut state, view, ms).is_empty());
        }
    }

    /// `0x847934` emits `progress` when an item starts, before it runs: when the last item
    /// starts and ends inside one device step (a refusal, a SKIP), the batch view goes from
    /// handled 1 to gone. The batch end (`Services::last_batch_end`, the view's serial, success:
    /// every item started) still gives UPDATE 1.0 before DELETE. A cancelled end adds none.
    #[test]
    fn a_last_item_that_ends_in_its_start_step_still_pushes_its_progress() {
        use tetrad_app::device::services::BatchEnd;
        let (mut state, mut control) = loaded::session();
        let end = |serial, success| BatchEnd {
            serial,
            kind: ProductKind::NeuralCapture,
            success,
            from_control: true,
        };
        let first = |serial| {
            Some(BatchView {
                serial,
                ..view(ProductKind::NeuralCapture, 1, 2).unwrap()
            })
        };
        assert_eq!(step(&mut control, &mut state, first(1), 10).len(), 2);
        control.set_batch_end(Some(end(1, true)));
        let sent = step(&mut control, &mut state, None, 20);
        assert_eq!(sent, [progress(1.0), finished()]);
        assert_eq!(step(&mut control, &mut state, first(2), 30).len(), 2);
        control.set_batch_end(Some(end(2, false)));
        assert_eq!(step(&mut control, &mut state, None, 40), [finished()]);
    }

    /// re-r44 §2 step 5: a Control DELETE during a batch sets the stop flag
    /// (`AccountRequest::CancelBulkRun`) and gets no reply. The item in flight completes
    /// (no new item, so no UPDATE), then the batch end sends one DELETE `finished` (a cancelled
    /// batch ends as a good one).
    #[test]
    fn a_control_bulk_delete_ends_the_batch_with_one_finished_push() {
        use tetrad_app::device::control::AccountRequest;
        let (mut state, mut control) = loaded::session();
        let captures = |handled| view(ProductKind::NeuralCapture, handled, 3);
        let started = step(&mut control, &mut state, captures(1), 10);
        assert_eq!(started.len(), 2, "CREATE, UPDATE 1/3: {started:?}");
        let delete = BulkOperationMessage {
            action: Some(MessageAction::Delete.into()),
            request_id: Some(5),
            ..Default::default()
        };
        let (sent, effects) = loaded::host(&mut control, &mut state, delete, at(20));
        assert!(sent.is_empty(), "no reply: {sent:?}");
        let cancel = Effect::ControlAccount(AccountRequest::CancelBulkRun);
        assert_eq!(effects, [cancel]);
        assert!(step(&mut control, &mut state, captures(1), 30).is_empty());
        assert_eq!(step(&mut control, &mut state, None, 40), [finished()]);
        assert!(step(&mut control, &mut state, None, 50).is_empty());
    }

    /// re-r44 §2 step 4: READ during a batch (`0xed8d0` action 3) reports
    /// `is_multiselection_active`, the overlay text, the bar value (the last progress that
    /// passed the step rule), blocking and cancellable, the stored folders and the type. Here a
    /// preset batch from the setlist "Gig" (the source folder as `0x2416f8`, no files). After
    /// the batch, READ reports Multi Select only.
    #[test]
    fn bulk_read_reports_the_running_batch() {
        let (root, mut state, mut control, _) = super::gig_session("bulk-read");
        let gig = "/media/p4/Presets/Gig";
        let presets = |handled| {
            Some(BatchView {
                source: Some(gig.into()),
                ..view(ProductKind::Preset, handled, 2).unwrap()
            })
        };
        let start = step(&mut control, &mut state, presets(1), 10);
        let source = FolderInfo {
            key: Some(gig.into()),
            name: Some("Gig".into()),
            is_factory: Some(false),
            is_downloads: Some(false),
            is_user_default: Some(false),
            is_cloud: Some(false),
            is_favorites: Some(false),
            is_plugin: Some(false),
            ..Default::default()
        };
        assert_eq!(start[0].source_folder.as_ref(), Some(&source));
        let read = BulkOperationMessage {
            action: Some(MessageAction::Read.into()),
            request_id: Some(8),
            ..Default::default()
        };
        let (sent, _) = loaded::host(&mut control, &mut state, read.clone(), at(20));
        let running = BulkOperationMessage {
            action: update(),
            request_id: Some(8),
            progress_message: Some("Uploading Presets, please wait.".into()),
            blocking: Some(true),
            cancellable: Some(true),
            progress: Some(0.5),
            is_multiselection_active: Some(false),
            operation_type: Some(0),
            source_folder: Some(source),
            destination_folder: Some(cloud_folder("preset", "Presets")),
            ..Default::default()
        };
        assert_eq!(sent, [ControlMessage::BulkOperation(running)]);
        assert_eq!(step(&mut control, &mut state, None, 30), [finished()]);
        let (sent, _) = loaded::host(&mut control, &mut state, read, at(40));
        let idle = BulkOperationMessage {
            action: update(),
            request_id: Some(8),
            is_multiselection_active: Some(false),
            ..Default::default()
        };
        assert_eq!(sent, [ControlMessage::BulkOperation(idle)]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A9: the other runs push as the upload batch does, with their own start (`0xf1cec`, C).
    /// A multi-key DELETE (`0x785b74`): "Removing %1, please wait." (%1 the noun `0x7ece20`),
    /// the cloud folder as source, no target, then progress and DELETE `finished` (done
    /// closure `0x783950`). A capture or IR download (`0x786264`): "Downloading %1, please
    /// wait." (%1 "IRs" unless the target is a capture folder), source the cloud folder. A
    /// preset download (`0x786ac4`): "Downloading presets, please wait.", the setlist as target.
    /// `operation_type` is the source folder type.
    #[test]
    fn delete_and_download_runs_push_their_own_start() {
        let (root, mut state, mut control, _) = super::gig_session("bulk-runs");
        let run = |operation: BatchOperation, kind, handled| {
            Some(BatchView {
                operation,
                ..view(kind, handled, 2).unwrap()
            })
        };
        let start =
            |text: &str, kind: &str, noun: &str, operation_type, target| BulkOperationMessage {
                action: Some(MessageAction::Create.into()),
                progress_message: Some(text.into()),
                blocking: Some(true),
                cancellable: Some(true),
                progress: Some(0.0),
                operation_type: Some(operation_type),
                source_folder: Some(cloud_folder(kind, noun)),
                destination_folder: target,
                ..Default::default()
            };

        let delete = |handled| {
            run(
                BatchOperation::Delete,
                ProductKind::ImpulseResponse,
                handled,
            )
        };
        let mut sent = Vec::new();
        for (ms, view) in [(10, delete(1)), (20, delete(2)), (30, None)] {
            sent.extend(step(&mut control, &mut state, view, ms));
        }
        let removing = start(
            "Removing IRs, please wait.",
            "impulse_response",
            "IRs",
            1,
            None,
        );
        assert_eq!(sent, [removing, progress(0.5), progress(1.0), finished()]);

        let media = BatchOperation::Download { setlist: None };
        let irs = run(media.clone(), ProductKind::ImpulseResponse, 1);
        let sent = step(&mut control, &mut state, irs, 40);
        // No media database here, so no saving destination folder (I: stock always has one).
        let downloading = start(
            "Downloading IRs, please wait.",
            "impulse_response",
            "IRs",
            1,
            None,
        );
        assert_eq!(sent, [downloading, progress(0.5)]);
        let captures =
            run(media, ProductKind::NeuralCapture, 1).map(|view| BatchView { serial: 2, ..view });
        let sent = step(&mut control, &mut state, captures, 50);
        assert_eq!(sent[0], finished(), "a new serial ends the last run");
        assert_eq!(
            sent[1].progress_message.as_deref(),
            Some("Downloading Neural Captures, please wait.")
        );
        assert_eq!(step(&mut control, &mut state, None, 60), [finished()]);

        let presets = BatchOperation::Download {
            setlist: Some("Gig".into()),
        };
        let sent = step(
            &mut control,
            &mut state,
            run(presets, ProductKind::Preset, 1),
            70,
        );
        let gig = FolderInfo {
            key: Some("/media/p4/Presets/Gig".into()),
            name: Some("Gig".into()),
            is_factory: Some(false),
            is_downloads: Some(false),
            is_user_default: Some(false),
            is_cloud: Some(false),
            is_favorites: Some(false),
            is_plugin: Some(false),
            ..Default::default()
        };
        let text = "Downloading presets, please wait.";
        let downloading = start(text, "preset", "Presets", 0, Some(gig));
        assert_eq!(sent, [downloading, progress(0.5)]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `0x843c78` (`+0x55`, C): while a run is active a multi-key Control DELETE, UPLOAD or
    /// DOWNLOAD returns with nothing; one key still goes (`0x1e8a70` has no run check).
    #[test]
    fn a_multi_key_request_waits_for_the_running_run() {
        use super::account_requests;
        use tetrad_control::codecs::CloudProductMessage;
        let (mut state, mut control) = loaded::session();
        let delete = |keys: &[&str]| CloudProductMessage {
            action: Some(MessageAction::Delete.into()),
            product_keys: keys.iter().map(|key| (*key).to_owned()).collect(),
            product_type: Some(2),
            ..Default::default()
        };
        let running = Some(BatchView {
            operation: BatchOperation::Delete,
            ..view(ProductKind::NeuralCapture, 1, 2).unwrap()
        });
        step(&mut control, &mut state, running, 10);
        let (_, effects) = loaded::host(&mut control, &mut state, delete(&["c-1", "c-2"]), at(20));
        assert!(account_requests(&effects).is_empty(), "{effects:?}");
        let (_, effects) = loaded::host(&mut control, &mut state, delete(&["c-1"]), at(30));
        assert_eq!(account_requests(&effects).len(), 1, "{effects:?}");
        step(&mut control, &mut state, None, 40);
        let (_, effects) = loaded::host(&mut control, &mut state, delete(&["c-1", "c-2"]), at(50));
        assert_eq!(account_requests(&effects).len(), 1, "{effects:?}");
    }
}

/// re-r44 §2 end to end (the `Device::step` hook): a Control batch of two captures goes to
/// Control as CREATE (type 2), UPDATE 0.5, UPDATE 1.0 and, after the SKIP that serves both
/// items, one DELETE `finished`, in that order.
#[test]
fn a_device_batch_upload_sends_the_bulk_operation_pushes_in_order() {
    use tetrad_control::codecs::{BulkOperationMessage, ConfirmationAction};
    let mut unit = ControlCaptureUnit::new();
    let mark = unit.log.borrow().requests.len();
    let upload = tetrad_control::codecs::CloudProductMessage {
        action: Some(MessageAction::Upload.into()),
        product_keys: vec![unit.hash.clone(), unit.hash.clone()],
        product_type: Some(2),
        ..Default::default()
    };
    common::host_send(&mut unit.device, upload);
    (0..3).for_each(|_| _ = unit.device.step());
    let [form] = &unit.forms(mark)[..] else {
        panic!("one Confirmation");
    };
    unit.control_answer(form.confirmation_id, ConfirmationAction::Cancel);
    (0..3).for_each(|_| _ = unit.device.step());
    let bulk: Vec<BulkOperationMessage> = (common::control_sent(&unit.log.borrow(), mark))
        .into_iter()
        .filter_map(|message| match message {
            ControlMessage::BulkOperation(message) => Some(message),
            _ => None,
        })
        .collect();
    let shape: Vec<_> = (bulk.iter())
        .map(|m| (m.action, m.operation_type, m.progress, m.finished))
        .collect();
    let create = Some(MessageAction::Create.into());
    let delete = Some(MessageAction::Delete.into());
    assert_eq!(
        shape,
        [
            (create, Some(2), Some(0.0), None),
            (update(), None, Some(0.5), None),
            (update(), None, Some(1.0), None),
            (delete, None, None, Some(true)),
        ]
    );
}

/// Round 47 (r47-control): the Control forward pacing and route (review-r45 M1, M4, M5), Control
/// 46 IR and multi-key DOWNLOAD (census-r46 ★1, ★3) and the File `instrument` / `device` fields
/// (census-r46 M1).
mod r47 {
    use std::time::{Duration, Instant};

    use tetrad_app::device::control::forward::ForwardHttp;
    use tetrad_app::device::control::{AccountRequest, CommsCipher, Control};
    use tetrad_app::device::{Effect, State};
    use tetrad_control::codecs::{
        BackupsForwardMessage, BackupsForwardResponse, CloudProductMessage, ControlMessage,
        MessageAction, UpdaterForwardMessage,
    };
    use tetrad_services::cloud::services::ProductKind;
    use tetrad_services::fake::MemFiles;
    use tetrad_services::http::{
        BackupsOp, HttpClient, HttpError, HttpRequest, Method, Route, UpdaterOp,
    };

    use super::{account_requests, at, gig_session, loaded, replies, sealed_replies};

    const KEY: &[u8] = b"fake key-exchange vector";

    fn cipher() -> CommsCipher {
        CommsCipher::derive(KEY)
    }

    /// A worker thread that sends `request` through Cortex Control.
    fn post(
        control: &Control,
        request: HttpRequest,
    ) -> std::thread::JoinHandle<Result<Vec<u8>, HttpError>> {
        let link = control.forward_link();
        std::thread::spawn(move || {
            let mut http = ForwardHttp::new(link, MemFiles::default());
            http.send(&request).map(|response| response.body)
        })
    }

    /// Review-r45 M1 (`0x1458ac`, C): a backup body goes to Control one 150 000-byte chunk per
    /// tick (stock: one per event-loop pass, from the single-shot `QTimer` `+8`), whole and in
    /// order under one ID, `is_last_chunk` on the last only. Before the fix every chunk left in
    /// the first tick (about 1 140 HID reports each).
    #[test]
    fn a_backup_body_over_control_goes_one_chunk_per_tick() {
        let (mut state, mut control) = loaded::session();
        control.set_transform(Some(cipher()));
        let body: Vec<u8> = (0..2 * 150_000 + 1).map(|i: u32| (i % 251) as u8).collect();
        let mut request = HttpRequest::new(
            Method::Put,
            "https://bucket.example.invalid/put",
            Route::Backups(BackupsOp::CreateOrUpdate),
            Duration::from_secs(600),
        );
        request.body = Some(body.clone());
        let worker = post(&control, request);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut chunks: Vec<BackupsForwardMessage> = Vec::new();
        let mut per_tick = Vec::new();
        while chunks
            .last()
            .is_none_or(|last| !last.request.as_ref().unwrap().is_last_chunk.unwrap())
        {
            assert!(Instant::now() < deadline, "no last chunk: {per_tick:?}");
            let effects = control.tick(&mut state, at(100)).unwrap();
            let sent: Vec<_> = (sealed_replies(&effects, &cipher()).into_iter())
                .filter_map(|(message, _)| match message {
                    ControlMessage::BackupsForward(m) => Some(m),
                    _ => None,
                })
                .collect();
            if chunks.is_empty() && sent.is_empty() {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            per_tick.push(sent.len());
            chunks.extend(sent);
        }
        assert_eq!(per_tick, [1, 1, 1], "one chunk per tick");
        let id = chunks[0].backups_request_id;
        assert!(chunks.iter().all(|chunk| chunk.backups_request_id == id));
        let lasts: Vec<_> = (chunks.iter())
            .map(|chunk| chunk.request.as_ref().unwrap().is_last_chunk)
            .collect();
        assert_eq!(lasts, [Some(false), Some(false), Some(true)]);
        let sent: Vec<u8> = (chunks.iter())
            .flat_map(|chunk| chunk.request.clone().unwrap().payload.unwrap())
            .collect();
        assert!(sent == body, "the body arrives whole and in order");
        let answer = BackupsForwardMessage {
            action: super::update(),
            backups_request_id: id,
            response: Some(BackupsForwardResponse {
                payload: Some(Vec::new()),
                is_last_chunk: Some(true),
                error_code: None,
            }),
            ..Default::default()
        };
        loaded::host(&mut control, &mut state, answer, at(200));
        assert_eq!(worker.join().unwrap(), Ok(Vec::new()));
    }

    /// Review-r45 M4 (O): a connected, online Control whose link has no transform (no Version
    /// key exchange) is no cloud route, as ProductForward (29) goes only sealed; a 29 posted
    /// anyway fails at once, not at its deadline. With the transform the route is Control.
    #[test]
    fn a_link_without_its_key_is_no_cloud_route_and_its_forward_fails_at_once() {
        let (mut state, mut control) = loaded::session();
        assert!(control.connected());
        assert_eq!(
            control.cloud_route(),
            None,
            "online, but no transform: no route"
        );
        let request = HttpRequest::new(
            Method::Get,
            "https://cloud.example.invalid/backups/v1",
            Route::Product,
            Duration::from_secs(30),
        );
        let worker = post(&control, request);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !worker.is_finished() && Instant::now() < deadline {
            control.tick(&mut state, at(100)).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            worker.is_finished(),
            "the call still waits for its deadline"
        );
        let Err(HttpError::Transport(error)) = worker.join().unwrap() else {
            panic!("a transport error");
        };
        assert!(error.contains("did not send"), "{error}");
        control.set_transform(Some(cipher()));
        assert!(control.cloud_route().is_some());
    }

    /// Review-r45 M5: in one tick a forward the link cannot send (29 without the transform)
    /// fails only its own call; the UpdaterForward (61, not sealed) of the same tick goes out
    /// and its call waits for Control's answer.
    #[test]
    fn an_unsent_forward_fails_only_its_own_call() {
        let (mut state, mut control) = loaded::session();
        let product = HttpRequest::new(
            Method::Get,
            "https://cloud.example.invalid/user/products",
            Route::Product,
            Duration::from_secs(30),
        );
        let check = HttpRequest::new(
            Method::Get,
            "https://cloud.example.invalid/firmware/v1/latest",
            Route::Updater(UpdaterOp::Check),
            Duration::from_secs(60),
        );
        let product = post(&control, product);
        let check = post(&control, check);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut updater: Vec<UpdaterForwardMessage> = Vec::new();
        while (updater.is_empty() || !product.is_finished()) && Instant::now() < deadline {
            let effects = control.tick(&mut state, at(100)).unwrap();
            updater.extend(replies(effects).0.into_iter().filter_map(
                |(message, _)| match message {
                    ControlMessage::UpdaterForward(m) => Some(m),
                    _ => None,
                },
            ));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(product.is_finished(), "the unsent call failed at once");
        assert!(matches!(
            product.join().unwrap(),
            Err(HttpError::Transport(_))
        ));
        let [sent] = &updater[..] else {
            panic!("one UpdaterForward: {updater:?}");
        };
        assert!(!check.is_finished(), "the sent call waits for its answer");
        let answer = UpdaterForwardMessage {
            action: super::update(),
            updater_request_id: sent.updater_request_id,
            response: Some(tetrad_control::codecs::UpdaterForwardResponse {
                payload: Some(b"{}".to_vec()),
                is_last_chunk: Some(true),
                error_code: None,
            }),
            ..Default::default()
        };
        loaded::host(&mut control, &mut state, answer, at(200));
        assert_eq!(check.join().unwrap(), Ok(b"{}".to_vec()));
    }

    fn download(keys: &[&str], product_type: i32) -> CloudProductMessage {
        CloudProductMessage {
            action: Some(MessageAction::Download.into()),
            product_keys: keys.iter().map(|key| (*key).to_owned()).collect(),
            product_type: Some(product_type),
            ..Default::default()
        }
    }

    /// Census-r46 ★1: CloudProduct (46) DOWNLOAD type 1 of one key (`0xf3f24` → `0x1e8a70`, an IR
    /// into the IR saving destination `0x799134`) goes to Services as the Directory IR row does.
    /// A `to_folder` is refused. Bring-up: refused without `--persist`, and without
    /// `--services`.
    #[test]
    fn control_46_ir_download_goes_into_the_ir_destination() {
        let (mut state, mut control) = loaded::session();
        let (sent, effects) =
            loaded::host(&mut control, &mut state, download(&["ir-1"], 1), at(10));
        assert!(sent.is_empty());
        let expected = AccountRequest::CloudProductDownload {
            kind: ProductKind::ImpulseResponse,
            id: "ir-1".into(),
        };
        assert_eq!(account_requests(&effects), std::slice::from_ref(&expected));
        let into_folder = CloudProductMessage {
            to_folder: Some("local_ir_root".into()),
            ..download(&["ir-1"], 1)
        };
        let (_, effects) = loaded::host(&mut control, &mut state, into_folder, at(20));
        assert!(account_requests(&effects).is_empty(), "{effects:?}");
        for (at_ms, persist, services, allowed) in [
            (30, false, true, false),
            (40, true, false, false),
            (50, true, true, true),
        ] {
            let options = tetrad_app::device::bringup::RunOptions {
                services,
                ..loaded::bringup(persist)
            };
            control.set_run_options(&options);
            let message = download(&["ir-1"], 1);
            let (_, effects) = loaded::host(&mut control, &mut state, message, at(at_ms));
            let got = account_requests(&effects);
            assert_eq!(
                got == std::slice::from_ref(&expected),
                allowed,
                "{effects:?}"
            );
        }
    }

    /// Census-r46 ★3, captures and IRs: more keys (`0x786264` → `0x847620`) download as one
    /// Multi Select batch into the saving destination, in order; nothing while Multi Select
    /// runs (`0x843c60`).
    #[test]
    fn control_46_multi_key_capture_and_ir_download_requests_each_key() {
        let (mut state, mut control) = loaded::session();
        for (at_ms, product_type, kind) in [
            (10, 2, ProductKind::NeuralCapture),
            (20, 1, ProductKind::ImpulseResponse),
        ] {
            let message = download(&["k-2", "k-1"], product_type);
            let (_, effects) = loaded::host(&mut control, &mut state, message, at(at_ms));
            let expected = AccountRequest::CloudProductDownloads {
                kind,
                ids: vec!["k-2".into(), "k-1".into()],
            };
            assert_eq!(account_requests(&effects), [expected]);
        }
        control.set_multi_select(true);
        let (_, effects) = loaded::host(&mut control, &mut state, download(&["a", "b"], 2), at(30));
        assert!(account_requests(&effects).is_empty(), "{effects:?}");
    }

    /// Census-r46 ★3, presets (`0x786ac4`): with one index the keys go into the consecutive
    /// slots from it (`0xf3878` → `0x84eac4`), up to the last slot (256: keys past it are
    /// dropped); with one index per key each key into its slot (`0xf3cc0` → `0x84e68c`).
    /// Another index count, a negative slot and a start past the last slot are refused; Multi
    /// Select running: nothing.
    #[test]
    fn control_46_multi_key_preset_download_fills_setlist_slots() {
        let (root, mut state, mut control, _) = gig_session("cloud-presets");
        let gig = "/media/p4/Presets/Gig";
        let presets = |keys: &[&str], indices: Vec<i32>| CloudProductMessage {
            to_folder: Some(gig.into()),
            to_indices: indices,
            ..download(keys, 0)
        };
        let items = |pairs: &[(&str, usize)]| AccountRequest::CloudPresetDownload {
            setlist: "Gig".into(),
            items: (pairs.iter())
                .map(|(id, slot)| ((*id).to_owned(), *slot))
                .collect(),
        };
        for (at_ms, message, expected) in [
            (
                10,
                presets(&["a", "b", "c"], vec![4]),
                items(&[("a", 4), ("b", 5), ("c", 6)]),
            ),
            (
                20,
                presets(&["a", "b"], vec![9, 2]),
                items(&[("a", 9), ("b", 2)]),
            ),
            (
                30,
                presets(&["a", "b", "c"], vec![254]),
                items(&[("a", 254), ("b", 255)]),
            ),
        ] {
            let (_, effects) = loaded::host(&mut control, &mut state, message, at(at_ms));
            assert_eq!(account_requests(&effects), [expected]);
        }
        for (at_ms, refused) in [
            (40, presets(&["a", "b", "c"], vec![1, 2])),
            (50, presets(&["a", "b"], vec![-1])),
            (60, presets(&["a", "b"], vec![3, -2])),
            (70, presets(&["a", "b"], vec![256])),
        ] {
            let (_, effects) = loaded::host(&mut control, &mut state, refused, at(at_ms));
            assert!(account_requests(&effects).is_empty(), "{effects:?}");
        }
        control.set_multi_select(true);
        let message = presets(&["a", "b"], vec![4]);
        let (_, effects) = loaded::host(&mut control, &mut state, message, at(80));
        assert!(account_requests(&effects).is_empty(), "{effects:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The setlist "My Presets" with `Bass.pb` (sidecar instrument bass) in slot 0 and
    /// `Plain.pb` (no instrument) in slot 1, and `zenui.db` with two captures in the capture
    /// root: `NC_1` (cab, guitar) and `NC_2` (default, not set). The caller removes the root.
    fn instrument_library(tag: &str) -> (std::path::PathBuf, State, Control) {
        use tetrad_app::device::Library;
        use tetrad_library::stock::{StockLibrary, StockLimits};
        use tetrad_storage::media::{MediaRoot, SETLIST_SLOTS, encode_setlist};
        use tetrad_storage::stock::{DbLimits, ProductType, StockDb, StockProduct};
        let root =
            std::env::temp_dir().join(format!("tetrad-app-r47-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let setlist = root.join("Presets/My Presets");
        std::fs::create_dir_all(&setlist).unwrap();
        let mut slots = vec![None; SETLIST_SLOTS];
        slots[0] = Some("/media/p4/Presets/My Presets/Bass.pb".to_owned());
        slots[1] = Some("/media/p4/Presets/My Presets/Plain.pb".to_owned());
        std::fs::write(setlist.join("setlist"), encode_setlist(&slots).unwrap()).unwrap();
        let sidecar = r#"{"name":"Bass","instrument_type":"bass"}"#;
        std::fs::write(setlist.join("Bass.json"), sidecar).unwrap();
        std::fs::write(setlist.join("Plain.json"), r#"{"name":"Plain"}"#).unwrap();
        let mut database = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
        let capture = |id: &str, metadata: &str| StockProduct {
            id: id.into(),
            product_type: ProductType::Capture,
            read_only: false,
            raw_data: Some(vec![1]),
            metadata: Some(metadata.as_bytes().to_vec()),
        };
        let captures = [
            capture(
                "NC_1",
                r#"{"name":"Plexi","deviceType":"cab","instrumentType":"guitar"}"#,
            ),
            capture(
                "NC_2",
                r#"{"name":"Flat","deviceType":"default","instrumentType":"instrument_not_set"}"#,
            ),
        ];
        (database.transaction(|write| {
            write.boot_maintenance()?;
            captures.iter().try_for_each(|c| write.put_product(c))
        }))
        .unwrap();
        let top = (database.view().root_folder(ProductType::Capture))
            .unwrap()
            .unwrap();
        (database.transaction(|write| captures.iter().try_for_each(|c| write.link(top, &c.id))))
            .unwrap();
        let (mut state, control) = loaded::session();
        state.library = Some(Library {
            presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
            database: Some(database),
        });
        (root, state, control)
    }

    /// Census-r46 M1 (`0x242ae4`, C): File READ items carry `instrument` (record `+0x5c`) and
    /// `device` (`+0x60`) only when not 0: a preset from its sidecar `instrument_type`, a
    /// capture from its `instrumentType` and `deviceType` (stock codes `0x7ec714`,
    /// `0x7ec994`). `gain` stays unknown.
    #[test]
    fn file_read_items_carry_instrument_and_device() {
        use tetrad_control::codecs::{FileMessage, ProductData};
        let (root, mut state, mut control) = instrument_library("read");
        let read = |file_type| FileMessage {
            action: Some(MessageAction::Read.into()),
            file_type: Some(file_type),
            omit_factory_content: Some(true),
            ..Default::default()
        };
        // Empty setlist slots (`{index, is_readonly}`) and key-only folders carry no name.
        let items = |sent: Vec<ControlMessage>| -> Vec<ProductData> {
            (sent.into_iter())
                .flat_map(|message| match message {
                    ControlMessage::File(file) => file.folder.unwrap().files,
                    other => panic!("File only: {other:?}"),
                })
                .filter(|item| item.name.is_some())
                .collect()
        };
        let fields = |items: Vec<ProductData>| -> Vec<(String, Option<i32>, Option<i32>)> {
            (items.into_iter())
                .map(|item| (item.name.unwrap(), item.instrument, item.device))
                .collect()
        };
        let presets = items(loaded::host(&mut control, &mut state, read(0), at(10)).0);
        assert_eq!(
            fields(presets),
            [("Bass".into(), Some(2), None), ("Plain".into(), None, None)]
        );
        let captures = items(loaded::host(&mut control, &mut state, read(2), at(20)).0);
        assert_eq!(
            fields(captures),
            [
                ("Plexi".into(), Some(1), Some(4)),
                ("Flat".into(), None, None)
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Census-r46 M1: a Control preset save carries its `instrument` (has-bit `0x4000`). On the
    /// loaded slot (`modifyPresetMetadataSignal` → `0x5b91dc`) a File CREATE with an index or an
    /// UPDATE is Edit Details with the item name, else the slot's; into another slot the Save
    /// As (`0x24e0c8`) is followed by Edit Details of the saved slot (`0x24e130`). Without an
    /// instrument, or with 0 (not set), the save stays `SavePreset`; a code past 5 is refused.
    #[test]
    fn a_control_preset_save_carries_its_instrument() {
        use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
        use tetrad_ui::PreferredInstrument;
        let (root, mut state, mut control) = instrument_library("save");
        let bass = "/media/p4/Presets/My Presets/Bass.pb";
        state.current.get_mut().as_mut().unwrap().path = Some(bass.into());
        let file = |action: MessageAction, item: ProductData| FileMessage {
            action: Some(action.into()),
            file_type: Some(0),
            folder: Some(FolderInfo {
                key: Some("/media/p4/Presets/My Presets".into()),
                files: vec![item],
                ..Default::default()
            }),
            ..Default::default()
        };
        let saves = |effects: Vec<Effect>| -> Vec<Effect> {
            (effects.into_iter())
                .filter(|effect| {
                    matches!(
                        effect,
                        Effect::SavePreset(_)
                            | Effect::EditPresetDetails { .. }
                            | Effect::SavePresetInto { .. }
                    )
                })
                .collect()
        };
        let indexed = |index, name: Option<&str>, instrument| ProductData {
            index: Some(index),
            name: name.map(str::to_owned),
            instrument,
            ..Default::default()
        };
        let edit = |name: &str, instrument| Effect::EditPresetDetails {
            name: name.into(),
            instrument,
        };
        let cases = [
            (
                file(MessageAction::Create, indexed(0, Some("Low"), Some(2))),
                vec![edit("Low", PreferredInstrument::Bass)],
            ),
            (
                file(MessageAction::Create, indexed(0, None, Some(4))),
                vec![edit("Bass", PreferredInstrument::Vocal)],
            ),
            (
                file(MessageAction::Create, indexed(0, Some("Low"), None)),
                vec![Effect::SavePreset(Some("Low".into()))],
            ),
            (
                file(MessageAction::Create, indexed(0, None, Some(0))),
                vec![Effect::SavePreset(None)],
            ),
            (
                file(MessageAction::Create, indexed(7, Some("Keys"), Some(3))),
                vec![Effect::SavePresetInto {
                    name: "Keys".into(),
                    setlist: Some("/media/p4/Presets/My Presets".into()),
                    slot: 7,
                    instrument: PreferredInstrument::Synth,
                }],
            ),
            (
                file(
                    MessageAction::Update,
                    ProductData {
                        key: Some(bass.into()),
                        instrument: Some(1),
                        ..Default::default()
                    },
                ),
                vec![edit("Bass", PreferredInstrument::Guitar)],
            ),
            (
                file(MessageAction::Create, indexed(0, Some("Low"), Some(9))),
                Vec::new(),
            ),
        ];
        for (at_ms, (message, expected)) in (10..).step_by(10).zip(cases) {
            let (_, effects) = loaded::host(&mut control, &mut state, message, at(at_ms));
            assert_eq!(saves(effects), expected);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Review r47 N1: a Control File CREATE into another slot with an `instrument` is ONE
    /// save that carries the instrument. When that store fails (a read-only target setlist),
    /// nothing touches the loaded preset: it keeps its path, file and folder.
    #[test]
    fn a_failed_save_into_another_slot_leaves_the_loaded_preset_alone() {
        use super::common::{self, Media, STOCK, light_preset};
        use std::os::unix::fs::PermissionsExt;
        use tetrad_control::codecs::{FileMessage, FolderInfo, ProductData};
        use tetrad_storage::media::{SETLIST_SLOTS, encode_setlist};
        let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
        let gig = media.0.join("Presets/Gig");
        std::fs::create_dir_all(&gig).unwrap();
        let empty = encode_setlist(&vec![None; SETLIST_SLOTS]).unwrap();
        std::fs::write(gig.join("setlist"), empty).unwrap();
        let folders = "/media/p4/Presets/My Presets\n/media/p4/Presets/Gig\n";
        std::fs::write(media.0.join("user_preset_folders"), folders).unwrap();
        let (mut device, _log, media) = common::boot(media);
        common::connect_control(&mut device);
        // A save stamps the file with the wall clock; without it the save fails earlier.
        let clock = std::time::UNIX_EPOCH + Duration::from_secs(1_791_635_696);
        device.platform().wall_clock.set(Some(clock));
        let loaded = |device: &tetrad_app::device::Device<common::Sim>| {
            (device.state().current.as_ref()).and_then(|current| current.path.clone())
        };
        let mine = media.0.join("Presets/My Presets");
        let files = |dir: &std::path::Path| {
            let mut names: Vec<_> = (std::fs::read_dir(dir).unwrap())
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        let (path, before) = (loaded(&device), files(&mine));
        let bytes = std::fs::read(mine.join("Light.pb")).unwrap();
        std::fs::set_permissions(&gig, std::fs::Permissions::from_mode(0o555)).unwrap();
        common::host_send(
            &mut device,
            FileMessage {
                action: Some(MessageAction::Create.into()),
                file_type: Some(0),
                folder: Some(FolderInfo {
                    key: Some("/media/p4/Presets/Gig".into()),
                    files: vec![ProductData {
                        index: Some(2),
                        name: Some("Keys".into()),
                        instrument: Some(3),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        std::fs::set_permissions(&gig, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(loaded(&device), path);
        assert_eq!(files(&mine), before, "no rename, no new sidecar");
        assert_eq!(std::fs::read(mine.join("Light.pb")).unwrap(), bytes);
        assert!(!gig.join("Keys.pb").exists());
    }
}

/// One Cortex Control host message as HID reports in the device queue, with no step: a
/// burst then arrives in one gather, as on the QC.
fn queue(device: &mut tetrad_app::device::Device<common::Sim>, message: impl Into<ControlMessage>) {
    let message = message.into();
    let envelope = Envelope {
        message_type: message.message_type() as u32,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload: message
            .encode(tetrad_control::link::LinkLimits::default().payload)
            .unwrap(),
    };
    for report in envelope.reports(Limits::default()).unwrap() {
        device.push(tetrad_app::device::Event::Control(Box::new(report)));
    }
}

/// The Cortex Control 4.1.0 subscription burst (order from a QC HID trace, 2026-10-16) arrives
/// in one gather. The device answers one message per pass and holds the later reports, so the
/// touch, DSP and frame work of a pass does not wait for the whole burst (the QC held one pass
/// for 2.6 s). The File READ reply takes the passes after its request (A3). Every reply still
/// goes out, in request order (r48).
#[test]
fn a_control_read_burst_is_answered_one_message_per_pass_in_request_order() {
    use tetrad_control::codecs::{
        FileMessage, GeneralSettingsMessage, GlobalTempoMessage, IOSettingsMessage,
        MasterVolumeMessage, ModeMessage, ModuleStatsMessage, PresetDirtyMessage,
        SetlistPositionMessage, UndoRedoMessage,
    };
    let (mut device, log, _media) = connected_sim();
    let read = || Some(MessageAction::Read.into());
    let burst: [ControlMessage; 11] = [
        ModuleStatsMessage {
            action: read(),
            ..Default::default()
        }
        .into(),
        UndoRedoMessage {
            action: read(),
            ..Default::default()
        }
        .into(),
        IOSettingsMessage {
            action: read(),
            ..Default::default()
        }
        .into(),
        GeneralSettingsMessage {
            action: read(),
            ..Default::default()
        }
        .into(),
        ModeMessage {
            action: read(),
            ..Default::default()
        }
        .into(),
        MasterVolumeMessage {
            action: read(),
            ..Default::default()
        }
        .into(),
        FileMessage {
            action: read(),
            omit_factory_content: Some(true),
            ..Default::default()
        }
        .into(),
        RecallPresetMessage {
            action: read(),
            ..Default::default()
        }
        .into(),
        GlobalTempoMessage {
            action: read(),
            ..Default::default()
        }
        .into(),
        SetlistPositionMessage {
            action: read(),
            ..Default::default()
        }
        .into(),
        PresetDirtyMessage {
            action: read(),
            ..Default::default()
        }
        .into(),
    ];
    let expected = burst.each_ref().map(ControlMessage::message_type);
    let mark = log.borrow().requests.len();
    for message in burst {
        queue(&mut device, message);
    }
    let received = |device: &tetrad_app::device::Device<common::Sim>| {
        let received = device.bridges().control.last_received();
        received.map(|received| received.message_type)
    };
    for request in expected {
        device.step();
        assert_eq!(
            received(&device),
            Some(request as u32),
            "{request:?} is reduced in its own pass"
        );
        // The File READ reply goes out over the next passes (A3), before the next request.
        while device.bridges().control.spreading() {
            device.step();
        }
    }
    // The queued reports go out over the next passes (at most 12 frames each).
    while device.bridges().control.transmitting() {
        device.step();
    }
    let sent = device.bridges().control.hid_reports_sent();
    device.step();
    assert_eq!(
        device.bridges().control.hid_reports_sent(),
        sent,
        "nothing left"
    );
    let mut replies: Vec<_> = (common::control_sent(&log.borrow(), mark).iter())
        .map(ControlMessage::message_type)
        .collect();
    replies.dedup();
    assert_eq!(replies, expected);
}

/// A3: a long reply goes out at most 12 frames per pass (stock `sendHIDReportsSlot`
/// `0x29ae64`), so a 400-report reply no longer holds one QC pass (0.63 ms per report). The sim
/// RecallPreset READ reply is 60 reports. The
/// later passes send the rest without new input, and the frames keep their order: the reply
/// reassembles whole.
#[test]
fn a_long_reply_goes_out_twelve_frames_per_pass_in_order() {
    use tetrad_app::device::control::outgoing::FRAMES_PER_PASS;
    let (mut device, log, _media) = connected_sim();
    let mark = log.borrow().requests.len();
    let sent = |device: &tetrad_app::device::Device<common::Sim>| {
        device.bridges().control.hid_reports_sent() as usize
    };
    let start = sent(&device);
    queue(
        &mut device,
        RecallPresetMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        },
    );
    device.step();
    let waiting = device.bridges().control.reports_waiting();
    assert_eq!(sent(&device) - start, FRAMES_PER_PASS);
    assert!(
        waiting > FRAMES_PER_PASS,
        "a long reply: {waiting} reports wait"
    );
    let mut passes = 0;
    while device.bridges().control.transmitting() {
        let before = sent(&device);
        device.step();
        assert!(sent(&device) - before <= FRAMES_PER_PASS);
        passes += 1;
    }
    assert_eq!(passes, waiting.div_ceil(FRAMES_PER_PASS));
    let replies = common::control_sent(&log.borrow(), mark);
    assert!(
        (replies.iter()).any(|reply| matches!(reply, ControlMessage::RecallPreset(_))),
        "the RecallPreset reply reassembles: {replies:?}"
    );
}

/// Cortex Control idle traffic (keep-alives, row meters and the 1 Hz SOC2 ARM block, whose
/// HID counters move with each Control report) costs no frame while the grid shows: the QC
/// rendered 45 unchanged frames per 10 s. The USB Statistics rows still follow each block and
/// draw when their page shows (`0x68a90c`, r48).
#[test]
fn control_idle_traffic_costs_no_frame_on_the_grid() {
    use tetrad_app::device::Event;
    use tetrad_app::device::ui::UiInput;
    use tetrad_control::codecs::{GridModelMeterMessage, IOMeterMessage};
    use tetrad_dsp_host::responses::Event as DspEvent;
    use tetrad_dsp_protocol::responses::{MeterRecord, Soc2ArmDiagnostics};
    use tetrad_dsp_protocol::{CoreId, DspCore};
    use tetrad_ui::{Control as UiControl, DiagnosticsPage, Screen, SettingControl, SettingsPage};
    let (mut device, _log, _media) = connected_sim();
    for row in 0..4 {
        common::host_send(
            &mut device,
            GridModelMeterMessage {
                action: Some(MessageAction::Create.into()),
                row: Some(row),
                column: Some(-1),
                ..Default::default()
            },
        );
    }
    common::host_send(&mut device, IOMeterMessage::default());
    assert!(device.bridges().control.grid_meters_subscribed());
    assert_eq!(device.state().screen, Screen::Grid);
    let block = |sent: u32| {
        let mut record = [0_u8; 128];
        record[0] = 1;
        record[0x78..0x7c].copy_from_slice(&sent.to_le_bytes());
        Event::Dsp(
            CoreId::Control,
            DspEvent::Soc2ArmDiagnostics(Soc2ArmDiagnostics(record)),
        )
    };
    device.take_renders();
    for second in 1..=10 {
        common::host_send(
            &mut device,
            KeepAliveMessage {
                action: Some(MessageAction::Update.into()),
                ..Default::default()
            },
        );
        for row in 0..4 {
            let meter = MeterRecord {
                core: 0,
                column: 1,
                index: 0x401c + row,
                value_bits: (-80.0_f32).to_bits(),
            };
            device.push(Event::Dsp(
                CoreId::Dsp(DspCore::Core0),
                DspEvent::Meter(meter),
            ));
        }
        device.push(block(second));
        device.step();
    }
    assert_eq!(device.take_renders(), 0);
    let rows = |device: &tetrad_app::device::Device<common::Sim>| {
        device
            .state()
            .views
            .usb_statistics
            .clone()
            .expect("the rows")
    };
    assert!(
        rows(&device)[3].text.ends_with("/10"),
        "{:?}",
        rows(&device)[3]
    );

    let support = Screen::Settings(SettingsPage::Diagnostics);
    device.push(Event::UiInput(UiInput::Show(support)));
    device.step();
    let usb = UiControl::Setting(SettingControl::Diagnostics(DiagnosticsPage::Usb));
    let tap = common::target(&device.bridges().ui, usb);
    for pressed in [true, false] {
        let touch = tetrad_app::device::platform::TouchSample { pressed, ..tap };
        device.push(Event::UiInput(UiInput::Touch(touch)));
        device.step();
    }
    device.take_renders();
    device.push(block(11));
    device.step();
    assert_eq!(device.take_renders(), 1);
    assert!(
        rows(&device)[3].text.ends_with("/11"),
        "{:?}",
        rows(&device)[3]
    );
}

/// r49-control-pushes: the change pushes the S7 capture showed missing or different, at State
/// level.
mod r49 {
    use tetrad_app::device::actions::{Action, reduce};
    use tetrad_app::device::features;
    use tetrad_app::device::{Effect, State};
    use tetrad_control::codecs::{
        ControlMessage, LooperMessage, LooperUpdateType, MessageAction, RecallPresetMessage,
        SetlistPositionMessage, UndoRedoMessage,
    };
    use tetrad_control::link::Notification;
    use tetrad_engine::SceneIndex;
    use tetrad_preset::{
        BypassRow, Chain, ColBypass, Model, ParamValue, Preset, SceneBypass, Slot as PresetSlot,
    };

    use super::{at, loaded, update};

    /// A sidechain compressor (source selector `SOURCE`, parameter 1) and a Looper X with
    /// 23 unit parameters, beside the `loaded` catalog.
    fn models() -> String {
        let looper: String = (0..23)
            .map(|index| {
                format!(
                    "<Parameter name='P{index}' type='float' min='0' max='1' defaultValue='0'/>"
                )
            })
            .collect();
        format!(
            "<Category id='5' name='Compressor'><Model id='5018' name='Comp' blob='comp' sidechain='1'>\
<Parameter name='THRESHOLD' type='float' min='-48' max='0' defaultValue='-40'/>\
<Parameter name='SOURCE' type='rotarySwitch' min='0' max='44' defaultValue='0' dynamic='true' \
stepNames='Off,Follow Input'/></Model></Category>\
<Category id='27' name='Looper'><Model id='27000' name='Loop' blob='loop'>{looper}</Model></Category>"
        )
    }

    const MODULES: &str = "<Module id='5018' name='Comp' blob_name='comp' category='5' \
dm_heap='100' pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='5'/></Module>\
<Module id='27000' name='Loop' blob_name='loop' category='27' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='10'/></Module>";

    /// Row 1: Light (column 3) with GAIN in scene mode (0.25 in scene E) and its bypass in
    /// scene mode (bypassed in scene E), a Comp sink (column 5) whose SOURCE names the Light
    /// block (step 14 of 15), and a Looper X (column 7) with QUANTIZE (P18) at `quantize`.
    /// The row 1 input gate is flagged as a sidechain source.
    fn session(quantize: f32) -> (State, tetrad_app::device::control::Control) {
        loaded::session_with_preset(&models(), MODULES, |preset, catalog| {
            let slot = |column| PresetSlot { row: 0, column };
            let light = Model::block(1, [ParamValue::Float(0.5), ParamValue::Float(0.0)]);
            preset.insert_block(slot(2), light, catalog).unwrap();
            let comp = Model::block(5018, [0.5, 1.0, 0.0].map(ParamValue::Float));
            preset.insert_block(slot(4), comp, catalog).unwrap();
            let mut looper = [0.0; 23];
            looper[18] = quantize;
            let looper = Model::block(27000, looper.map(ParamValue::Float));
            preset.insert_block(slot(6), looper, catalog).unwrap();
            let gain = &mut preset.chains[0].models[2].params[0];
            gain.scene_mode = Some(true);
            gain.values[4].value = Some(ParamValue::Float(0.25));
            let column = &mut preset.bypass[0].col_bypass[2];
            column.scene_mode = Some(true);
            column.scene_bypass[4].bypass = true;
            preset.chains[0].input_control[0].sidechain_source_flag = Some(true);
        })
    }

    /// The Control pushes of `effects` in order: Scene as its index, the rest as sent.
    #[derive(Debug, PartialEq)]
    enum Push {
        Scene(u32),
        Notify(ControlMessage),
        Grid(ControlMessage),
    }

    fn pushes(effects: &[Effect]) -> Vec<Push> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::ControlNotification(notification) => match **notification {
                    Notification::Scene(scene) => Some(Push::Scene(scene)),
                    _ => None,
                },
                Effect::ControlNotify(message) => Some(Push::Notify((**message).clone())),
                Effect::GridEcho(message) => Some(Push::Grid((**message).clone())),
                _ => None,
            })
            .collect()
    }

    fn looper(message: LooperMessage) -> ControlMessage {
        ControlMessage::Looper(LooperMessage {
            action: update(),
            ..message
        })
    }

    fn meter() -> ControlMessage {
        looper(LooperMessage {
            update_type: Some(LooperUpdateType::Meter.into()),
            ..Default::default()
        })
    }

    fn grid(preset: Preset) -> ControlMessage {
        ControlMessage::Grid(tetrad_control::codecs::GridMessage {
            action: update(),
            preset: Some(tetrad_control::codecs::RawMessageBytes {
                bytes: preset.to_bytes(),
            }),
            ..Default::default()
        })
    }

    /// `0x2607c0`: the four input gates with only their sidechain source flag.
    fn input_sources(flags: [bool; 4]) -> ControlMessage {
        let chains = (0..4)
            .map(|row| Chain {
                row: Some(row),
                input_control: vec![Model {
                    sidechain_source_flag: Some(flags[row as usize]),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .collect();
        grid(Preset {
            chains,
            ..Default::default()
        })
    }

    /// D1, S7 stock `[1647]`–`[1666]` (Factory 1A, scene A → E on the QC): after Scene the
    /// Looper X driver write pushes `quantize_enabled` and two Meters (`0x55e140` case
    /// `0x12`), the recall tail (`0x527248` → `0x5129a4`) echoes each scene-mode value
    /// (`0x25d51c`) and then the scene-mode bypass (`0x25d364`) of each block, and each
    /// recalled sidechain source emits signal 28 (`0x52570c` → `0x519d0c`) for one
    /// `input_control` push (`0x2607c0`). The driver write restarts the Buttons timer, so
    /// Buttons follows 150 ms later. Before r49 Tetrad sent the Scene only.
    #[test]
    fn a_scene_recall_pushes_the_looper_writes_the_scene_values_and_the_input_sources() {
        let (mut state, mut control) = session(0.5);
        let scene = SceneIndex::new(4).unwrap();
        let effects = reduce(Action::SelectScene(scene), &mut state).unwrap();
        let gain = super::grid::block(
            0,
            Model {
                column: Some(2),
                params: vec![super::grid::param(0, 0.25)],
                ..Default::default()
            },
        );
        let bypass = Preset {
            bypass: vec![BypassRow {
                row: Some(0),
                col_bypass: vec![ColBypass {
                    column: Some(2),
                    scene_bypass: vec![SceneBypass {
                        bypass: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            pushes(&effects),
            [
                Push::Scene(4),
                Push::Notify(looper(LooperMessage {
                    quantize_enabled: Some(true),
                    ..Default::default()
                })),
                Push::Notify(meter()),
                Push::Notify(meter()),
                Push::Grid(grid(gain)),
                Push::Grid(grid(bypass)),
                Push::Grid(input_sources([true, false, false, false])),
            ]
        );
        for effect in effects {
            if let Effect::ControlNotify(message) = effect {
                control
                    .notify(Notification::Update(*message), &mut state, at(1000))
                    .unwrap();
            }
        }
        let buttons = |sent: Vec<ControlMessage>| {
            sent.into_iter()
                .filter(|message| {
                    matches!(message, ControlMessage::Looper(looper)
                        if looper.update_type == Some(LooperUpdateType::Buttons.into()))
                })
                .count()
        };
        assert_eq!(buttons(loaded::sync(&mut control, &mut state, at(1149))), 0);
        assert_eq!(buttons(loaded::sync(&mut control, &mut state, at(1150))), 1);
    }

    /// `0x55e140` case `0x12`: QUANTIZE maps through `trunc(v · 9 + 0.5)`; index 0 (below
    /// 1/18) is the only "off" entry of table `0xa553c8`.
    #[test]
    fn the_looper_quantize_flag_follows_the_stock_table() {
        for (value, enabled) in [(0.0, false), (0.05, false), (0.06, true), (1.0, true)] {
            let (mut state, _) = session(value);
            let effects =
                reduce(Action::SelectScene(SceneIndex::new(1).unwrap()), &mut state).unwrap();
            let quantize = pushes(&effects).into_iter().find_map(|push| match push {
                Push::Notify(ControlMessage::Looper(message)) => message.quantize_enabled,
                _ => None,
            });
            assert_eq!(quantize, Some(enabled), "{value}");
        }
    }

    /// D5, S7 stock `[1753]` (Factory 1B): the working preset Control gets carries the
    /// runtime fields of the deploy that the file lacks: the hash is FNV-1a of the `.pb` path
    /// (`/opt/neuraldsp/Factory Library/Brit Plexi100 Normal.pb` gives stock's 4204141790);
    /// a sink's selector lists Off, Follow Input, the twelve inputs and then each preceding
    /// block with its glyph and `R%1 C%2` (`0x52ebf4`); the sink flag is set for a source
    /// that is not Off, and the source flag on the block it names (`0x52e53c`).
    #[test]
    fn recall_preset_carries_the_hash_and_the_sidechain_lists_of_the_deploy() {
        let (mut state, mut control) = session(0.0);
        let path = "/opt/neuraldsp/Factory Library/Brit Plexi100 Normal.pb";
        state.current.get_mut().as_mut().unwrap().path = Some(path.into());
        let read = RecallPresetMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        };
        let (sent, _) = loaded::host(&mut control, &mut state, read, at(10));
        let [ControlMessage::RecallPreset(reply)] = &sent[..] else {
            panic!("{sent:?}");
        };
        let bytes = &reply.preset.as_ref().unwrap().bytes;
        let preset = Preset::decode(bytes, tetrad_preset::Limits::default()).unwrap();
        assert_eq!(preset.hash, Some(4_204_141_790));
        let models = &preset.chains[0].models;
        let (light, comp) = (&models[2], &models[4]);
        assert_eq!(
            (comp.sidechain_sink_flag, light.sidechain_source_flag),
            (Some(true), Some(true))
        );
        let source = &comp.params[1];
        assert_eq!(source.dynamic_steps.len(), 15);
        assert_eq!(
            source.dynamic_steps[..2],
            ["Off".to_owned(), "Follow Input".to_owned()]
        );
        assert_eq!(source.dynamic_steps[13..], ["USB input 7/8", "Light"]);
        assert_eq!(
            source.dynamic_icons[13..],
            ["icons/IO/USB.png", "icons/virtual devices/overdrive.png"]
        );
        assert_eq!(source.dynamic_metadata[13..], ["", "R1 C3"]);
        // A block that is not a sink keeps its stored flags and lists.
        assert_eq!(light.sidechain_sink_flag, Some(false));
        assert!(
            light
                .params
                .iter()
                .all(|param| param.dynamic_steps.is_empty())
        );
    }

    /// D8, S7 stock `[1765]`/`[1776]`: the Looper status carries only its non-zero words
    /// (state, free samples, armed and the two NaN displacements: 21 B), not all 22.
    #[test]
    fn looper_status_leaves_out_zero_words() {
        let (mut state, mut control) = session(0.0);
        let nan = f32::NAN.to_bits();
        let mut words = [0; 22];
        (words[0], words[8], words[13], words[16], words[17]) = (1, 27_131_904, 1, nan, nan);
        state.looper = Some(tetrad_dsp_protocol::tuner_looper::LooperStatus::from_words(
            words,
        ));
        let read = LooperMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        };
        let (sent, _) = loaded::host(&mut control, &mut state, read, at(10));
        let [ControlMessage::Looper(reply)] = &sent[..] else {
            panic!("{sent:?}");
        };
        let status = reply.status.as_ref().unwrap();
        assert_eq!(
            (status.state, status.free_samples, status.armed),
            (Some(1), Some(27_131_904), Some(1))
        );
        assert!(status.max_write_displacement.unwrap().is_nan());
        assert!(status.min_write_displacement.unwrap().is_nan());
        let stock = tetrad_control::codecs::LooperStatus {
            state: Some(1),
            free_samples: Some(27_131_904),
            armed: Some(1),
            ..Default::default()
        };
        assert_eq!(
            tetrad_control::codecs::LooperStatus {
                max_write_displacement: None,
                min_write_displacement: None,
                ..status.clone()
            },
            stock
        );
    }

    /// D7, S7 stock GlobalTempo pushes of 19–23 B: the metronome status fields are proto3
    /// implicit, so zero words are left out (`0x10ae84` copies the words as they come).
    #[test]
    fn metronome_status_leaves_out_zero_words() {
        let (mut state, _) = loaded::session();
        let status = tetrad_dsp_protocol::responses::MetronomeStatus {
            words: [1, 0, 2, 41, 0, 1, u32::MAX],
        };
        let effects = features::metronome_status(&mut state, status);
        let [Effect::ControlNotify(message)] = &effects[..] else {
            panic!("{effects:?}");
        };
        let ControlMessage::GlobalTempo(tempo) = &**message else {
            panic!("{message:?}");
        };
        let status = tempo.metronome_status.as_ref().unwrap();
        assert_eq!(
            (
                status.is_enabled,
                status.preroll_enabled,
                status.current_tick
            ),
            (Some(1), None, None)
        );
        assert_eq!(
            (status.current_beat, status.current_bar),
            (Some(2), Some(41))
        );
        assert_eq!(
            (
                status.count_in_beats_remaining,
                status.count_in_bars_remaining
            ),
            (Some(1), Some(-1))
        );
    }

    /// r48 perf note: a tuner reading repaints only a shown tuner view (the Tuner screen, or
    /// Gig View with LIVE TUNER); a Cortex Control subscription on the grid sends the meter
    /// without a frame (it polled at 50 ms, up to 20 frames/s before).
    #[test]
    fn a_tuner_reading_costs_a_frame_only_with_a_tuner_view_shown() {
        let (mut state, _) = loaded::session();
        state.tuner_meter_subscribed = true;
        state.screen = tetrad_ui::Screen::Grid;
        let effects = features::tuner_reading(&mut state, Some(440.0));
        assert!(!effects.contains(&Effect::Present), "{effects:?}");
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::ControlNotify(_)))
        );
        state.screen = tetrad_ui::Screen::Tuner;
        let effects = features::tuner_reading(&mut state, Some(441.0));
        assert!(effects.contains(&Effect::Present), "{effects:?}");
    }

    /// D17, S7 stock `[27]`: the UndoRedo READ reply (`0x1ba218`) leaves the action at 0
    /// (implicit), so with empty stacks the body is empty.
    #[test]
    fn undo_redo_read_replies_without_an_action() {
        let (mut state, mut control) = loaded::session();
        let read = UndoRedoMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        };
        let (sent, _) = loaded::host(&mut control, &mut state, read, at(10));
        assert_eq!(sent, [ControlMessage::UndoRedo(UndoRedoMessage::default())]);
    }

    /// A7, `0x27ed1c`: a Downloads item sends only `is_downloads` and its key; a plugin preset
    /// `is_plugin`, its folder and its key; a setlist slot the folder, position and
    /// `is_factory`.
    #[test]
    fn setlist_position_takes_the_downloads_and_plugin_branches() {
        let (mut state, mut control) = loaded::session();
        let mut read = |state: &mut State, path: &str| {
            state.current.get_mut().as_mut().unwrap().path = Some(path.into());
            let read = SetlistPositionMessage {
                action: Some(MessageAction::Read.into()),
                ..Default::default()
            };
            let (sent, _) = loaded::host(&mut control, state, read, at(10));
            let [ControlMessage::SetlistPosition(reply)] = &sent[..] else {
                panic!("{sent:?}");
            };
            SetlistPositionMessage {
                action: None,
                ..reply.clone()
            }
        };
        assert_eq!(
            read(&mut state, "cloud-0-1/abc-123"),
            SetlistPositionMessage {
                is_downloads: Some(true),
                key_in_downloads: Some("abc-123".into()),
                ..Default::default()
            }
        );
        let plugin = "/media/p4/Plugins/Archetype/Lead.pb";
        assert_eq!(
            read(&mut state, plugin),
            SetlistPositionMessage {
                is_plugin: Some(true),
                folder_key: Some("/media/p4/Plugins/Archetype".into()),
                key_in_plugin_folder: Some(plugin.into()),
                ..Default::default()
            }
        );
        let setlist = read(&mut state, "/media/p4/Presets/My Presets/Light.pb");
        assert_eq!(
            (setlist.is_factory, setlist.is_downloads, setlist.is_plugin),
            (Some(false), None, None)
        );
    }
}

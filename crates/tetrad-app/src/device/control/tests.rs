//! The State-backed `DeviceModel`: a host session against a deployed synthetic preset,
//! and the stage implementations.

use std::io::Write as _;

use flate2::Compression as Level;
use tetrad_catalog::{Catalog, Specs};
use tetrad_control::codecs::{
    CodecLimits, ConnectionMessage, ControlMessage, KeepAliveMessage, ModelRepoMessage,
};
use tetrad_control::device::TunerCommand;
use tetrad_control::pipeline::{Compression as _, StageFailure, Transform as _};
use tetrad_control::{Envelope, Limits, Reassembler};
use tetrad_hal::platform::{Platform as HalPlatform, Product};
use tetrad_preset::Preset;
use tetrad_settings::device::{DeviceSettings, Hardware, SettingsEvent};
use tetrad_settings::{SettingsFileId, SettingsStorage};

use super::super::preset::{SpecsCatalog, prepare};
use super::super::{Current, Timer};
use super::*;

/// The options of a `tetrad run` for the hand-built models: nothing is refused.
static STOCK: RunOptions = RunOptions::STOCK;

/// Synthetic catalog (no stock data), as in `tests/device_boot.rs`.
const MODELS: &str = "<Models>\
<Category id='0' name='Drive'><Model id='1' name='Light' blob='light'>\
<Parameter name='GAIN' type='float' min='0' max='10' defaultValue='5'/></Model></Category>\
<Category id='10' name='Splitter' hidden='true'><Model id='10004' name='Splitter'>\
<Parameter name='TYPE' type='switch' min='0' max='2' defaultValue='0'/></Model></Category>\
<Category id='11' name='Mixer' hidden='true'><Model id='11000' name='Mixer'>\
<Parameter name='LEVEL A' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/>\
</Model></Category>\
<Category id='23' name='Lane Output' hidden='true'><Model id='23000' name='Lane Output'>\
<Parameter name='VOLUME' type='float' min='0' max='1' defaultValue='0.76923'/>\
<Parameter name='PAN' type='float' min='0' max='1' defaultValue='0.5'/></Model></Category>\
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
</ModuleReport>";

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

fn state() -> State {
    let specs = Specs::resolve(&Catalog::parse(MODELS, REPORT).unwrap()).unwrap();
    let preset =
        Preset::new_default(tetrad_firmware::running().version, &SpecsCatalog(&specs)).unwrap();
    let prepared = prepare(preset, &specs, false, None, Default::default()).unwrap();
    let (settings, _) = DeviceSettings::load(&NoFiles, Hardware::QuadCortex).unwrap();
    let mut state = State::new(Product::QuadCortex, HalPlatform::QcOriginal, settings);
    state.specs = Some(specs);
    *state.current.get_mut() = Some(Current {
        path: None,
        preset: prepared.preset,
        migration: prepared.migration,
        deployment: prepared.deployment,
    });
    state
}

fn host(message_type: u32, payload: Vec<u8>) -> Vec<[u8; 128]> {
    let envelope = Envelope {
        message_type,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload,
    };
    envelope.reports(Limits::default()).unwrap()
}

fn send(message: impl Into<ControlMessage>) -> Vec<[u8; 128]> {
    let message = message.into();
    let payload = message.encode(CodecLimits::default()).unwrap();
    host(message.message_type() as u32, payload)
}

fn feed(control: &mut Control, state: &mut State, reports: &[[u8; 128]]) -> Vec<Effect> {
    reports
        .iter()
        .flat_map(|report| control.on_report(report, state, Duration::ZERO).unwrap())
        .collect()
}

fn decode(effects: &[Effect]) -> Vec<ControlMessage> {
    let mut reassembler = Reassembler::new(Limits::default());
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::ControlReport(report) => reassembler.push(&report[..]).unwrap(),
            _ => None,
        })
        .map(|envelope| {
            ControlMessage::decode(&envelope, CodecLimits::default())
                .unwrap()
                .message
        })
        .collect()
}

#[test]
fn host_session_over_device_state() {
    let mut control = Control::new();
    let mut state = state();
    control
        .set_model_repo(b"<Models><Model blob=\"light\"/></Models>")
        .unwrap();
    let handshake = [
        send(KeepAliveMessage {
            action: Some(MessageAction::Update.into()),
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
    ]
    .concat();
    let effects = feed(&mut control, &mut state, &handshake);
    let [ControlMessage::ModelRepo(repo)] = &decode(&effects)[..] else {
        panic!("{effects:?}");
    };
    assert_eq!(
        repo.model_repo_payload.as_deref(),
        control.model_repo_payload()
    );
    assert!(control.connected());

    // RecallPreset READ replies with the loaded preset's bytes and echoes the ID.
    let read = send(RecallPresetMessage {
        action: Some(MessageAction::Read.into()),
        request_id: Some(4),
        ..Default::default()
    });
    let effects = feed(&mut control, &mut state, &read);
    let [ControlMessage::RecallPreset(reply)] = &decode(&effects)[..] else {
        panic!("{effects:?}");
    };
    assert_eq!(reply.request_id, Some(4));
    let loaded = state.current.as_ref().unwrap().preset.to_bytes();
    assert_eq!(reply.preset.as_ref().unwrap().bytes, loaded);

    // Scene UPDATE goes through the action reducer: the deployment moves and the DSP gets
    // the stock scene recall.
    let scene = tetrad_control::codecs::SceneMessage {
        action: Some(MessageAction::Update.into()),
        request_id: None,
        selected_scene: Some(2),
    };
    let reports = host(13, scene.encode(CodecLimits::default()).unwrap());
    let effects = feed(&mut control, &mut state, &reports);
    let deployment = &state.current.as_ref().unwrap().deployment;
    assert_eq!(deployment.scene().index(), 2);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::RecallScene(_))),
        "{effects:?}"
    );
    // The same scene again is a no-op (non-forced `setScene`): only the request ends.
    assert_eq!(
        feed(&mut control, &mut state, &reports),
        [Effect::ControlRequestDone]
    );

    // Unplug tears down; the marker then precedes the Connection(false) report.
    let effects = control.usb_unplugged(&mut state, Duration::ZERO).unwrap();
    assert!(!control.connected());
    assert!(control.reset_marker_pending());
    let [ControlMessage::Connection(closed)] = &decode(&effects)[..] else {
        panic!("{effects:?}");
    };
    assert_eq!(closed.connected, Some(false));
}

/// `0x58bcbc`: values of 10 and up are skipped and the list ends before the entry that
/// overflows the three slots (modes 3..=8 take two); 9 takes its slot and is dropped (owned).
#[test]
fn available_modes_are_sanitised_as_stock() {
    let stock = |modes: &[u32]| {
        sanitized_modes(modes)
            .iter()
            .map(|mode| mode.to_stock())
            .collect::<Vec<_>>()
    };
    assert_eq!(stock(&[0, 12, 3, 1]), [0, 3]);
    assert_eq!(stock(&[9, 1, 2, 0]), [1, 2]);
    assert_eq!(stock(&[4, 5]), [4]);
}

/// Also: the Version reply takes the boot reads (`0x2a36d0`); `app_fw_version` is the
/// Zenjack application firmware, absent when not read.
#[test]
fn settings_writes_schedule_the_stock_flush() {
    let mut state = state();
    state.info.kernel = Some("Linux qc 0.0.0-test".into());
    state.info.mac = Some("02:00:00:00:00:01".into());
    let mut model = StateModel {
        state: &mut state,
        serial: None,
        model_repo: None,
        capture_lock: false,
        capture_running: false,
        dirty: false,
        factory: None,
        factory_irs: None,
        downloads_in_flight: &[],
        upload_states: &[],
        cipher: None,
        wall_clock: None,
        disk_space: None,
        diagnostics: &super::status::Diagnostics::NONE,
        effects: Vec::new(),
        volume_echo: None,
        preset_reply: false,
        drop_echo: None,
        local_backup: super::backup::LocalBackup::Idle,
        cloud: None,
        multi_select: false,
        bulk: None,
        link_op: None,
        options: &STOCK,
        input_locked: false,
        standby: false,
        confirmation: None,
        asked: None,
        updater: super::updater::UpdaterSeen::default(),
        control_online: false,
        forwarded: Vec::new(),
        spreads: false,
        spread: None,
    };
    model.set_device_name("Stage left").unwrap();
    model.update_tuner(TunerCommand::Mute(false)).unwrap();
    assert!(model.update_tuner(TunerCommand::InputPort(7)).is_err());
    let info = model.version_info().unwrap();
    assert_eq!(info.custom_name.as_deref(), Some("Stage left"));
    assert_eq!(
        info.linux_kernel_version.as_deref(),
        Some("Linux qc 0.0.0-test")
    );
    assert_eq!(info.mac_address.as_deref(), Some("02:00:00:00:00:01"));
    assert_eq!(info.app_fw_version, None);
    let flush = Effect::StartTimer {
        timer: Timer::SettingsFlush,
        after: SettingsFileId::Zenjack.save_delay(),
    };
    // Each write reports its changed group, then schedules the debounced flush.
    let general = matches!(
        &model.effects[..2],
        [Effect::SettingsChanged(SettingsEvent::General(_)), timer] if *timer == flush
    );
    assert!(general, "{:?}", model.effects);
    assert!(model.effects.contains(&flush));
    assert!(!state.settings.settings().tuner.mutes_outputs);
}

#[test]
fn stages_follow_the_stock_zlib_and_per_payload_ctr() {
    // Inflate accepts both wrappers (windowBits 47) and bounds the output.
    let plain = vec![7u8; 20_000];
    let gzip = Zlib.deflate(&plain).unwrap();
    assert_eq!(&gzip[..2], &[0x1f, 0x8b]);
    assert_eq!(Zlib.inflate(&gzip, plain.len()).unwrap(), plain);
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), Level::fast());
    zlib.write_all(&plain).unwrap();
    let zlib = zlib.finish().unwrap();
    assert_eq!(Zlib.inflate(&zlib, plain.len()).unwrap(), plain);
    assert_eq!(
        Zlib.inflate(&gzip, plain.len() - 1),
        Err(StageFailure::Limit(plain.len() - 1))
    );
    // Each payload starts at the IV (`0x9f53c8` per call).
    let cipher = CommsCipher::derive(b"fake key-exchange vector");
    let sealed = cipher.encode(b"payload").unwrap();
    assert_eq!(cipher.encode(b"payload").unwrap(), sealed);
    assert_ne!(&sealed[..], b"payload");
    assert_eq!(cipher.decode(&sealed).unwrap(), b"payload");
}

/// RecallPreset library pass `0x7c2da4`: a capture `file_name` (parameter 5) of a 64-unit
/// key and a stale name takes the library name in all eight scenes; a key the library does
/// not hold and a value shorter than a key keep their text. Health-r34: a surrogate pair
/// across unit 64 splits as `preset::capture_key` cuts (63 units), so the pass and the other
/// key readers find the same key.
#[test]
fn recall_preset_capture_names_follow_the_library() {
    use tetrad_preset::{Chain, Model, Param, ParamValue, SceneValue};
    let key = "k".repeat(CAPTURE_KEY_UNITS);
    let capture = |file_name: String| Model {
        hash: Some(CAPTURE_MODELS[0]),
        params: (0..6)
            .map(|index| Param {
                values: vec![
                    SceneValue {
                        value: Some(ParamValue::String(if index == 5 {
                            file_name.clone()
                        } else {
                            String::new()
                        })),
                        ..Default::default()
                    };
                    tetrad_preset::SCENES
                ],
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    let unknown = format!("{}Other", "u".repeat(CAPTURE_KEY_UNITS));
    let mut preset = Preset {
        chains: vec![Chain {
            models: vec![
                capture(format!("{key}Old")),
                capture(unknown.clone()),
                capture("short".into()),
                capture(format!("{}\u{1F600}Old", "s".repeat(CAPTURE_KEY_UNITS - 1))),
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    let straddle = "s".repeat(CAPTURE_KEY_UNITS - 1);
    let names = BTreeMap::from([
        (key.clone(), "New".to_owned()),
        (straddle.clone(), "Named".to_owned()),
    ]);
    refresh_capture_names(&mut preset, &names);
    let file_names = |model: &Model| -> Vec<ParamValue> {
        model.params[5]
            .values
            .iter()
            .filter_map(|scene| scene.value.clone())
            .collect()
    };
    let models = &preset.chains[0].models;
    let text = |value: String| vec![ParamValue::String(value); tetrad_preset::SCENES];
    assert_eq!(file_names(&models[0]), text(format!("{key}New")));
    assert_eq!(file_names(&models[1]), text(unknown));
    assert_eq!(file_names(&models[2]), text("short".into()));
    assert_eq!(file_names(&models[3]), text(format!("{straddle}Named")));
}

// --- Resync 19 / 71 inputs (re8 §10) ------------------------------------------------------

/// D16, S7 stock `[1750]`–`[1773]` (Factory 1A → 1B on the QC): a load (reason Other)
/// deploys, so the Looper X driver writes (`quantize_enabled`, two Meters) come before
/// RecallPreset and the deploy's `input_control` push (signal 28) after SetlistPosition. A
/// save into the loaded slot redeploys nothing: RecallPreset, Scene, SetlistPosition only.
/// A12: the frame lists of a load go first (`0x648f1c` before `0x5b2f40`).
#[test]
fn a_load_pushes_the_looper_writes_first_and_the_input_sources_last() {
    use tetrad_preset::{Model, ParamValue, Slot as PresetSlot};
    let looper: String = (0..23)
        .map(|index| {
            format!("<Parameter name='P{index}' type='float' min='0' max='1' defaultValue='0'/>")
        })
        .collect();
    let models = MODELS.replace(
        "</Models>",
        &format!(
            "<Category id='27' name='Looper'><Model id='27000' name='Loop' blob='loop'>\
{looper}</Model></Category></Models>"
        ),
    );
    let report = REPORT.replace(
        "</ModuleReport>",
        "<Module id='27000' name='Loop' blob_name='loop' category='27' dm_heap='100' \
pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='10'/></Module></ModuleReport>",
    );
    let specs = Specs::resolve(&Catalog::parse(&models, &report).unwrap()).unwrap();
    let catalog = SpecsCatalog(&specs);
    let mut preset = Preset::new_default(tetrad_firmware::running().version, &catalog).unwrap();
    let block = Model::block(27000, [0.0; 23].map(ParamValue::Float));
    let slot = PresetSlot { row: 0, column: 2 };
    preset.insert_block(slot, block, &catalog).unwrap();
    let prepared = prepare(preset, &specs, false, None, Default::default()).unwrap();
    let mut state = state();
    state.specs = Some(specs);
    *state.current.get_mut() = Some(Current {
        path: None,
        preset: prepared.preset,
        migration: prepared.migration,
        deployment: prepared.deployment,
    });
    let kinds = |effects: Vec<Effect>| -> Vec<String> {
        effects
            .into_iter()
            .map(|effect| match effect {
                Effect::ControlNotify(message) => match *message {
                    ControlMessage::Looper(looper) if looper.quantize_enabled.is_some() => {
                        "quantize".to_owned()
                    }
                    ControlMessage::Looper(_) => "meter".to_owned(),
                    other => format!("{:?}", other.message_type()),
                },
                Effect::ControlNotification(notification) => match *notification {
                    Notification::Scene(_) => "Scene".to_owned(),
                    Notification::UndoFrames(_) => "frame lists".to_owned(),
                    other => format!("{other:?}"),
                },
                Effect::GridEcho(message) => match *message {
                    ControlMessage::Grid(_) => "input sources".to_owned(),
                    other => format!("{other:?}"),
                },
                other => format!("{other:?}"),
            })
            .collect()
    };
    assert_eq!(
        kinds(preset_pushes(&state, RecallPresetReason::Other)),
        [
            "frame lists",
            "quantize",
            "meter",
            "meter",
            "RecallPreset",
            "Scene",
            "SetlistPosition",
            "input sources"
        ]
    );
    assert_eq!(
        kinds(preset_pushes(&state, RecallPresetReason::Save)),
        ["RecallPreset", "Scene", "SetlistPosition"]
    );
}

mod resync {
    use std::path::PathBuf;

    use tetrad_control::block::{ValueCase, serialize};
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::MediaRoot;
    use tetrad_storage::stock::{DbLimits, StockDb};

    use super::super::grid::SPECIAL_FACTORY_PRESET;
    use super::*;

    /// Synthetic catalog: an `int` parameter, a hidden model, a clone, both capture models
    /// (parameter 5 `file_name`), 23000 and the hidden extras 4004 and 31000.
    const MODELS: &str = concat!(
        "<Models><Category id='1' name='Drive'><Model id='1' name='Light' blob='light'>",
        "<Parameter name='GAIN' type='float' min='0' max='10' defaultValue='5'/>",
        "<Parameter name='MODE' type='int' min='0' max='4' defaultValue='2'/></Model>",
        "<Model id='2' name='Gone' blob='gone' hidden='true'>",
        "<Parameter name='GAIN' type='float' min='0' max='1' defaultValue='0'/></Model>",
        "<Model id='3' name='Twin' clones='1'/></Category>",
        "<Category id='14' name='Capture'>",
        "<Model id='14000' name='Capture' blob='c1'>",
        "<Parameter name='A' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='B' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='C' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='D' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='E' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='file_name' type='string' defaultValue=''/></Model>",
        "<Model id='14001' name='Capture 2' blob='c2'>",
        "<Parameter name='A' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='B' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='C' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='D' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='E' type='float' min='0' max='1' defaultValue='0'/>",
        "<Parameter name='file_name' type='string' defaultValue=''/></Model></Category>",
        "<Category id='23' name='Lane Output' hidden='true'><Model id='23000' name='Lane'>",
        "<Parameter name='VOLUME' type='float' min='0' max='1' defaultValue='0.5'/></Model>",
        "</Category><Category id='4' name='EQ'><Model id='4004' name='Out EQ' hidden='true'>",
        "<Parameter name='GAIN' type='float' min='0' max='1' defaultValue='0'/></Model>",
        "</Category><Category id='31' name='IO' hidden='true'>",
        "<Model id='31000' name='IO' hidden='true'>",
        "<Parameter name='GAIN' type='float' min='0' max='1' defaultValue='0'/></Model>",
        "</Category></Models>",
    );

    const USER_DDL: &str = "
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
";

    const FACTORY_DDL: &str = "
CREATE TABLE product ( id TEXT, type INT, readonly INT, rawdata BLOB, metadata BLOB, PRIMARY KEY(id));
CREATE TABLE folder ( id INTEGER PRIMARY KEY AUTOINCREMENT, pdType INT, folderType INT, name TEXT, parent INTEGER, readonly INT);
CREATE TABLE folder_content ( folderId INTEGER, productId TEXT, PRIMARY KEY(folderId,productId));
CREATE TABLE model_presets ( id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, hash INT, expSwitchType INT, expSwitchInverted INT, expSwitchLatchEmulation INT, expSwitchDelayMs INT);
CREATE TABLE model_preset_parameters (presetId INT, paramIndex INT, value TEXT, exp INT, expTrimMax REAL, expTrimMin REAL, PRIMARY KEY (presetId, paramIndex));
CREATE TABLE schema_version ( major INTEGER, minor INTEGER);
INSERT INTO schema_version VALUES (410, 1);
";

    /// A 64-unit capture key, so `left(64)` of parameter 5 is the key again.
    fn capture_key() -> String {
        format!("{:064x}", 0xc0ffee)
    }

    fn cipher() -> DeviceCipher {
        DeviceCipher::caller_secret(b"invented test secret")
    }

    /// A state with [`MODELS`] and a library whose `zenui.db` (read-only) holds `user`
    /// rows, a user capture under [`capture_key`] (version "2", name "Lead"), and a
    /// `factory.db` with `factory` rows. Returns the temp root to remove.
    fn library(user: &str, factory: &str) -> (State, StockDb, PathBuf) {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("tetrad-resync-{}-{next}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Presets")).unwrap();
        let (user_path, factory_path) = (root.join("zenui.db"), root.join("factory.db"));
        let connection = rusqlite::Connection::open(&user_path).unwrap();
        connection.execute_batch(USER_DDL).unwrap();
        connection.execute_batch(user).unwrap();
        let mut metadata = br#"{"name":"Lead","version":"2"}"#.to_vec();
        cipher().apply(&mut metadata);
        connection
            .execute(
                "INSERT INTO product VALUES (?1, 2, 0, NULL, ?2)",
                rusqlite::params![capture_key(), metadata],
            )
            .unwrap();
        drop(connection);
        let connection = rusqlite::Connection::open(&factory_path).unwrap();
        connection.execute_batch(FACTORY_DDL).unwrap();
        connection.execute_batch(factory).unwrap();
        drop(connection);
        let mut state = state();
        state.specs = Some(Specs::resolve(&Catalog::parse(MODELS, REPORT).unwrap()).unwrap());
        state.library = Some(super::super::super::Library {
            presets: StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap(),
            database: Some(StockDb::open(&user_path, DbLimits::default(), true).unwrap()),
        });
        let factory = StockDb::open_factory(&factory_path, DbLimits::default()).unwrap();
        (state, factory, root)
    }

    fn model<'a>(
        state: &'a mut State,
        factory: Option<&'a StockDb>,
        cipher: Option<&'a DeviceCipher>,
    ) -> StateModel<'a> {
        StateModel {
            state,
            serial: None,
            model_repo: None,
            capture_lock: false,
            capture_running: false,
            dirty: false,
            factory,
            factory_irs: None,
            downloads_in_flight: &[],
            upload_states: &[],
            cipher,
            wall_clock: None,
            disk_space: None,
            diagnostics: &super::status::Diagnostics::NONE,
            effects: Vec::new(),
            volume_echo: None,
            preset_reply: false,
            drop_echo: None,
            local_backup: super::backup::LocalBackup::Idle,
            cloud: None,
            multi_select: false,
            bulk: None,
            link_op: None,
            options: &STOCK,
            input_locked: false,
            standby: false,
            confirmation: None,
            asked: None,
            updater: super::updater::UpdaterSeen::default(),
            control_online: false,
            forwarded: Vec::new(),
            spreads: false,
            spread: None,
        }
    }

    /// RecallPreset READ (`0x274220` action 3) and the resync sender `0x2778d4` add the
    /// library item (`0x7c2bdc` → `0x67c368`, review-w6 C14): the sidecar date always, the
    /// description and tags over the preset's own, and an empty cloud id keeps the preset's.
    #[test]
    fn preset_read_carries_the_sidecar_item() {
        let (mut state, _, root) = library("", "");
        let setlist = root.join("Presets/My Presets");
        std::fs::create_dir_all(&setlist).unwrap();
        std::fs::write(
            setlist.join("Lead.json"),
            r#"{"cloudId": "","date": "2024-05-06T07:08:09Z","description": "Warm","tags": ["Rock","Blues"]}"#,
        )
        .unwrap();
        let current = state.current.get_mut().as_mut().unwrap();
        current.path = Some("/media/p4/Presets/My Presets/Lead.pb".into());
        current.preset.cloud_id = vec!["cloud-old".into()];
        current.preset.tags = vec!["Old".into()];

        let read = model(&mut state, None, None).current_preset().unwrap();
        let bytes = read.preset.unwrap().bytes;
        let preset = Preset::decode(&bytes, tetrad_preset::Limits::default()).unwrap();
        assert_eq!(preset.date.as_deref(), Some("2024-05-06T07:08:09Z"));
        assert_eq!(preset.description, ["Warm"]);
        assert_eq!(preset.tags, ["Rock", "Blues"]);
        assert_eq!(preset.cloud_id, ["cloud-old"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// `0x6a2390`: each `default_param` model id once; no `zenui.db` gives no rows (stock's
    /// query on a database that did not open).
    #[test]
    fn default_param_ids_list_each_model_once() {
        let rows = "INSERT INTO default_param VALUES ('1', 0, '0.5', 0, 1, 0), ('1', 1, '2', 0, 1, 0), ('abc', 0, '1', 0, 1, 0);";
        let (mut state, _, root) = library(rows, "");
        let mut ids = model(&mut state, None, None).default_param_ids().unwrap();
        ids.sort();
        assert_eq!(ids, ["1", "abc"]);
        state.library = None;
        let ids = model(&mut state, None, None).default_param_ids();
        assert_eq!(ids.unwrap(), Vec::<String>::new());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// `0x704530` + `0x667acc`: version "2" picks 14001; parameter 5 is key + name in every
    /// scene and keeps it against its row; rows apply by position because they cover all
    /// eight runtime parameters (MUTE takes the value only, BYPASS the EXP only); the
    /// `default_exp_switch` row is the switch. A key the library lacks gives no entry.
    #[test]
    fn capture_default_is_the_0x704530_block_with_the_user_rows() {
        let key = capture_key();
        let mut rows = format!(
            "INSERT INTO default_exp_switch VALUES ('{key}', 1, 1, 0, 250);\n\
             INSERT INTO default_param VALUES ('{key}', 0, '0.25', 3, 0.9, 0.1);\n"
        );
        for index in 1..8 {
            let (value, exp) = match index {
                5 => ("ignored", 4),
                6 => ("1", 5),
                7 => ("0", 6),
                _ => ("0.5", 0),
            };
            rows += &format!(
                "INSERT INTO default_param VALUES ('{key}', {index}, '{value}', {exp}, 1, 0);\n"
            );
        }
        let (mut state, _, root) = library(&rows, "");
        let cipher = cipher();
        let mut model = model(&mut state, None, Some(&cipher));
        assert_eq!(model.capture_default("absent").unwrap(), None);
        let capture = model.capture_default(&key).unwrap().unwrap();
        assert_eq!(capture.id, key);
        let block = &capture.block.model;
        assert_eq!(block.hash, Some(14_001));
        let name = ParamValue::String(format!("{key}Lead"));
        assert!(
            block.params[5]
                .values
                .iter()
                .all(|scene| scene.value == Some(name.clone()))
        );
        assert_eq!(block.params[5].expression, Some(4));
        let gain = &block.params[0];
        assert_eq!(gain.value(7), Some(&ParamValue::Float(0.25)));
        assert_eq!(
            (gain.expression, gain.expression_min, gain.expression_max),
            (Some(3), Some(0.1), Some(0.9))
        );
        let mute = &block.params[6];
        assert_eq!(
            (mute.value(0), mute.expression),
            (Some(&ParamValue::Float(1.0)), Some(0))
        );
        assert_eq!(block.bypass_expression[0].expression, 6);
        let switch = &block.expression_bypass_info[0];
        assert_eq!(
            (switch.kind, switch.invert, switch.delay_ms),
            (1, true, 250)
        );
        let mut cases = vec![ValueCase::Float; 5];
        cases.push(ValueCase::String);
        assert_eq!(capture.block.cases, cases);
        assert!(serialize(block, &capture.block.cases).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Review r32 E3: `0x15d890` asks only the user capture store, so a Downloads capture
    /// (by product id or by hash) gives no entry; a user capture whose record does not read
    /// is skipped with a notice and does not fail the resync.
    #[test]
    fn capture_default_takes_user_captures_and_skips_a_bad_row() {
        let mut metadata = br#"{"name":"Fetched","version":"2"}"#.to_vec();
        cipher().apply(&mut metadata);
        let hex: String = metadata.iter().map(|byte| format!("{byte:02x}")).collect();
        let hash = format!("{:064x}", 0xd0d0);
        let rows = format!(
            "INSERT INTO downloads_product_nc VALUES ('dl-1', '{hash}', NULL, X'{hex}');\n\
             INSERT INTO product VALUES ('bad', 2, 0, NULL, X'00ff');\n"
        );
        let (mut state, _, root) = library(&rows, "");
        let cipher = cipher();
        let mut model = model(&mut state, None, Some(&cipher));
        assert_eq!(model.capture_default("dl-1").unwrap(), None);
        assert_eq!(model.capture_default(&hash).unwrap(), None);
        assert!(model.effects.is_empty());
        assert_eq!(model.capture_default("bad").unwrap(), None);
        assert!(matches!(
            model.effects.as_slice(),
            [Effect::Notice(Subsystem::Control, text)] if text.contains("\"bad\" skipped")
        ));
        assert!(model.capture_default(&capture_key()).unwrap().is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// `0x1f6280`: factory presets, then user presets, each by id; default only where the
    /// user `default_model_presets` row names the same id and database; then the special
    /// entries (listed models of the visible categories, visible clones, 4004, 31000), default
    /// where the user has no default. Default entries carry their block: preset rows and
    /// switch, catalog cases.
    #[test]
    fn model_preset_records_follow_the_0x1f6280_order() {
        let user = "INSERT INTO model_presets VALUES (1, 'Mine', 1, 0, 0, 0, 600);\n\
                    INSERT INTO default_model_presets VALUES (1, 1, 1);";
        let factory = "INSERT INTO model_presets VALUES (1, 'Stock', 1, 1, 1, 0, 250);\n\
                       INSERT INTO model_preset_parameters VALUES (1, 0, '0.8', 2, 0.9, 0.1);";
        let (mut state, factory, root) = library(user, factory);
        let records = model(&mut state, Some(&factory), None)
            .model_preset_records()
            .unwrap();
        let summary: Vec<_> = records
            .iter()
            .map(|record| {
                let id = record.id.as_str();
                (
                    id,
                    record.is_factory,
                    record.hash,
                    record.default_block.is_some(),
                )
            })
            .collect();
        let special = SPECIAL_FACTORY_PRESET;
        assert_eq!(
            summary,
            [
                ("1", true, 1, true),
                ("1", false, 1, false),
                (special, true, 1, false),
                (special, true, 14_000, true),
                (special, true, 14_001, true),
                (special, true, 3, true),
                (special, true, 4004, true),
                (special, true, 31_000, true),
            ]
        );
        assert_eq!(records[2].name, "Neural DSP® Default");
        let stock = records[0].default_block.as_ref().unwrap();
        assert_eq!(stock.cases, [ValueCase::Float, ValueCase::Int]);
        let gain = &stock.model.params[0];
        assert_eq!(
            (gain.value(0), gain.expression, gain.expression_max),
            (Some(&ParamValue::Float(0.8)), Some(2), Some(0.9))
        );
        let switch = &stock.model.expression_bypass_info[0];
        assert_eq!(
            (switch.kind, switch.invert, switch.delay_ms),
            (1, true, 250)
        );
        for block in records
            .iter()
            .filter_map(|record| record.default_block.as_ref())
        {
            serialize(&block.model, &block.cases).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The ModelPreset READ list (`0x1d037c(msg, 0)`) is the resync list without the blocks
    /// of the special entries; their default flags stay. It builds no special block (A3: 602
    /// blocks held a QC pass about 0.2 s).
    #[test]
    fn the_model_preset_read_list_is_the_resync_list_without_special_blocks() {
        use tetrad_control::codecs::{MessageAction, ModelPresetMessage};
        let user = "INSERT INTO model_presets VALUES (1, 'Mine', 1, 0, 0, 0, 600);\n\
                    INSERT INTO default_model_presets VALUES (1, 1, 1);";
        let factory = "INSERT INTO model_presets VALUES (1, 'Stock', 1, 1, 1, 0, 250);";
        let (mut state, factory, root) = library(user, factory);
        let mut model = model(&mut state, Some(&factory), None);
        let mut expected: Vec<_> = (model.model_preset_records().unwrap().iter())
            .map(|record| tetrad_control::block::model_preset_entry(record).unwrap())
            .collect();
        for entry in &mut expected {
            if entry.id.as_ref().unwrap().value.as_deref() == Some(SPECIAL_FACTORY_PRESET) {
                entry.model = None;
            }
        }
        let read = ModelPresetMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        };
        let [reply] = &model.model_preset(&read).unwrap()[..] else {
            panic!("one reply");
        };
        assert_eq!(reply.presets, expected);
        assert!(
            reply.presets[0].model.is_some(),
            "a stored default keeps its block"
        );
        assert_eq!(reply.presets[3].is_default, Some(true));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A3: the READ list is built once per model preset revision (each Virtual Device Preset
    /// write adds one, `State::model_preset_revision`): a row stored behind the device's back
    /// shows only after the next write; a write shows at once.
    #[test]
    fn the_model_preset_read_list_is_kept_until_a_model_preset_write() {
        use tetrad_control::codecs::{MessageAction, ModelPresetMessage};
        let user = "INSERT INTO model_presets VALUES (1, 'Mine', 1, 0, 0, 0, 600);";
        let (mut state, factory, root) = library(user, "");
        let read = ModelPresetMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        };
        let names = |state: &mut State| -> Vec<String> {
            let mut model = model(state, Some(&factory), None);
            let [reply] = &model.model_preset(&read).unwrap()[..] else {
                panic!("one reply");
            };
            (reply.presets.iter())
                .filter_map(|entry| entry.name.clone())
                .filter(|name| !name.starts_with("Neural DSP"))
                .collect()
        };
        assert_eq!(names(&mut state), ["Mine"]);
        let connection = rusqlite::Connection::open(root.join("zenui.db")).unwrap();
        connection
            .execute_batch("INSERT INTO model_presets VALUES (2, 'Later', 1, 0, 0, 0, 600);")
            .unwrap();
        assert_eq!(names(&mut state), ["Mine"], "the list of this revision");
        state.model_preset_revision += 1;
        assert_eq!(names(&mut state), ["Mine", "Later"]);
        std::fs::remove_dir_all(root).unwrap();
    }
}

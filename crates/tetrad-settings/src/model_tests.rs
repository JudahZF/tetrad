//! Typed model contracts against synthetic, stock-shaped files.

use std::collections::BTreeMap;

use crate::account::{AccountTokens, Secret};
use crate::controls::FootswitchMode;
use crate::general::{BypassSaveMode, PowerButtonSensitivity};
use crate::io::{ExpressionCalibrator, ExpressionRange};
use crate::library::TableRole;
use crate::midi::{MidiChannel, MidiClockOut};
use crate::tuner::{InputPortId, TempoDefaults};
use crate::{
    DeviceSettings, Hardware, LoadIssue, ResetKind, Settings, SettingsEvent, SettingsFileId,
    SettingsStorage, Variant,
};

#[derive(Default)]
struct MemoryStorage(BTreeMap<SettingsFileId, Vec<u8>>);

impl SettingsStorage for MemoryStorage {
    fn read(&self, file: SettingsFileId) -> std::io::Result<Option<Vec<u8>>> {
        Ok(self.0.get(&file).cloned())
    }
    fn write(&mut self, file: SettingsFileId, bytes: &[u8]) -> std::io::Result<()> {
        self.0.insert(file, bytes.to_vec());
        Ok(())
    }
    fn remove(&mut self, file: SettingsFileId) -> std::io::Result<()> {
        self.0.remove(&file);
        Ok(())
    }
}

impl MemoryStorage {
    fn with(files: &[(SettingsFileId, &str)]) -> Self {
        Self(
            files
                .iter()
                .map(|(id, text)| (*id, text.as_bytes().to_vec()))
                .collect(),
        )
    }

    fn text(&self, id: SettingsFileId) -> &str {
        std::str::from_utf8(&self.0[&id]).unwrap()
    }
}

fn load(storage: &MemoryStorage) -> (DeviceSettings, Vec<LoadIssue>) {
    DeviceSettings::load(storage, Hardware::QuadCortex).unwrap()
}

/// Synthetic device-25000 defaults; the real values come from the catalog.
fn tempo_defaults() -> TempoDefaults {
    TempoDefaults {
        params: BTreeMap::from([(0, 0.5), (1, 0.0), (2, 1.0)]),
    }
}

/// Stock defaults recovered from the `value(key, default)` calls.
#[test]
fn defaults_match_stock_readers() {
    let s = Settings::defaults(Hardware::QuadCortex);
    assert_eq!(
        (s.display.brightness.get(), s.display.led_brightness.get()),
        (50, 27)
    );
    assert_eq!(s.display.dimmed_led_brightness.get(), 6);
    assert!(s.display.dimmed_stomp && !s.display.dimmed_preset);
    assert_eq!(
        (s.midi.channel, s.midi.over_usb, s.midi.clock_out),
        (MidiChannel::DEFAULT, true, MidiClockOut::Off)
    );
    assert_eq!(
        (s.tuner.input, s.tuner.mutes_outputs, s.tuner.reference_hz()),
        (InputPortId::Input1, true, 440.0)
    );
    assert_eq!(s.mode.mode(), FootswitchMode::Preset);
    assert_eq!(s.mode.available().len(), 3);
    assert_eq!(s.general.hold_timing.millis(), 800);
    assert_eq!(
        s.general.power_button_sensitivity,
        PowerButtonSensitivity::Medium
    );
    assert!(s.general.dynamic_delay_compensation && s.general.atma_power_on_automatic);
    assert_eq!(s.library.presets.role, TableRole::Banks);
    assert_eq!(s.capture.nc1.cabsim_id, 12001);
    assert!(s.paired_version.needs_pairing());
    assert_eq!(s.looper.stomp_assignments(), [3, 4, 5, 6, 0, 1, 7, 2]);
    assert!(s.io.params.is_empty() && !s.io.out12_paired);

    let atma = Settings::defaults(Hardware::Atma);
    assert_eq!(atma.looper.stomp_assignments(), [6, 2, 0, 1, 3, 4, 5, 7]);
    assert_eq!(atma.capture.nc1.io.return_level, 2.0 / 7.0);
}

const ZENJACK: &str = r#"[General]
bypassSaveMode=SaveBypassNever
gigViewStompAccess=true
powerButtonSensitivity=3
selected_connector=Output 1/2

[%General]
powerButtonSensitivity=0

[ConnectorsSettings]
1602=@Variant(\0\0\0\x87?\x80\0\0)
exp%201602=2
exp%20max%201602=@Variant(\0\0\0\x87?\0\0\0)
exp%20min%201602=@Variant(\0\0\0\x87>\x80\0\0)

[FoldersMinimized]
captureFoldersMinimized=3

[GlobalEq]
0\4=@Variant(\0\0\0\x87?\0\0\0)

[MidiSettings]
Channel=17
ClockOutEn=3

[Mode]
availableModes=1, 2
mode=2

[TableSettings]
captureFilter=@Variant(\0\0\0\b\0\0\0\x1\0\0\0\x16\0O\0W\0N\0\x45\0\x44\0_\0\x42\0Y\0_\0M\0\x45\0\0\0\x1\x1)
lastTableFolder\1=/IRs

[TempoSettings]
0=@Variant(\0\0\0\x87>\x80\0\0)

[Tuner]
BaseFreqOffsetHz=@Variant(\0\0\0\x87\xc0\0\0\0)

[TunerInput]
inputPortId=2
"#;

#[test]
fn stock_shaped_file_loads_typed_values() {
    let (device, issues) = load(&MemoryStorage::with(&[(SettingsFileId::Zenjack, ZENJACK)]));
    assert!(issues.is_empty(), "{issues:?}");
    let s = device.settings();
    assert!(s.midi.channel.is_omni());
    assert_eq!(s.midi.clock_out, MidiClockOut::UsbAndDin);
    assert_eq!(s.general.bypass_save_mode, BypassSaveMode::Never);
    // 4.1.0 reads `General/powerButtonSensitivity` ([%General]) only; the
    // root key is a legacy leftover.
    assert_eq!(
        s.general.power_button_sensitivity,
        PowerButtonSensitivity::Off
    );
    assert_eq!(s.io.selected_connector.as_deref(), Some("Output 1/2"));
    let hp = s.io.params[&1602];
    assert_eq!(
        (hp.value, hp.expression, hp.exp_min, hp.exp_max),
        (Some(1.0), 2, 0.25, 0.5)
    );
    assert_eq!(s.global_eq.params[&4], 0.5);
    assert_eq!(
        s.mode.available(),
        [FootswitchMode::Scene, FootswitchMode::Stomp]
    );
    assert_eq!(s.mode.mode(), FootswitchMode::Stomp);
    assert_eq!(s.library.capture_filter.owned_by_me, Some(true));
    assert_eq!(s.library.last_folders[&1], "/IRs");
    assert_eq!(s.tempo.bpm(), Some(90.0));
    assert_eq!(s.tuner.reference_hz(), 438.0);
    assert_eq!(s.tuner.input, InputPortId::Input2);
}

/// Only changed keys are written; unknown keys, legacy keys and untouched
/// values keep their bytes and places.
#[test]
fn update_writes_only_changed_keys() {
    let mut storage = MemoryStorage::with(&[
        (SettingsFileId::Zenjack, ZENJACK),
        (
            SettingsFileId::Account,
            "[General]\ntoken_expires=1\nusername=a@b\n",
        ),
    ]);
    let (mut device, _) = load(&storage);
    assert!(device.update(|_| {}).is_empty());
    let events = device.update(|s| {
        s.midi.channel = MidiChannel::new(5).unwrap();
        s.display.brightness = crate::general::ScreenBrightness::new(80).unwrap();
    });
    assert!(matches!(
        events.as_slice(),
        [SettingsEvent::Display(_), SettingsEvent::Midi(_)]
    ));
    assert_eq!(
        device.dirty_files().collect::<Vec<_>>(),
        [SettingsFileId::Zenjack]
    );
    device.flush(&mut storage).unwrap();
    let expected = ZENJACK
        .replace("[General]\n", "[General]\nbrightness=80\n")
        .replace("Channel=17", "Channel=5");
    assert_eq!(storage.text(SettingsFileId::Zenjack), expected);
    assert_eq!(
        storage.text(SettingsFileId::Account),
        "[General]\ntoken_expires=1\nusername=a@b\n"
    );
}

#[test]
fn invalid_values_fall_back_like_stock() {
    let file = "[General]\nbrightness=2\nbypassSaveMode=Sometimes\nholdTiming=abc\n\n[LooperSettings]\nstompAssignments0=4\n\n[Mode]\navailableModes=@Variant(\\0\\0\\0\\t\\0\\0\\0\\x1\\0\\0\\0\\x2\\0\\0\\0\\0)\nmode=2\n\n[MidiSettings]\nChannel=0\n";
    let (device, issues) = load(&MemoryStorage::with(&[(SettingsFileId::Zenjack, file)]));
    let s = device.settings();
    assert_eq!(s.display.brightness.get(), 4, "stock clamps");
    assert_eq!(s.general.bypass_save_mode, BypassSaveMode::Always);
    // Qt toInt("abc") is 0 with ok == false; 0 is a valid index.
    assert_eq!(s.general.hold_timing.get(), 0);
    assert_eq!(s.looper.stomp_assignments(), [3, 4, 5, 6, 0, 1, 7, 2]);
    assert_eq!(s.mode.mode(), FootswitchMode::Preset, "first available");
    assert_eq!(s.midi.channel, MidiChannel::DEFAULT);
    let mut keys: Vec<&str> = issues
        .iter()
        .map(|i| i.key.as_deref().expect("keyed issue"))
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "LooperSettings/stompAssignments",
            "MidiSettings/Channel",
            "Mode/mode",
            "brightness",
            "bypassSaveMode",
            "holdTiming"
        ]
    );
}

#[test]
fn boot_migrations_follow_stock() {
    let storage = MemoryStorage::with(&[
        (
            SettingsFileId::Zenjack,
            "[TempoSettings]\n0=120\n1=1\ntempoLedIsOff=true\n",
        ),
        (
            SettingsFileId::Permanent,
            "[NewModels]\n2.0.0=7\n3.0.0=1, 2\n",
        ),
        (
            SettingsFileId::Account,
            "[General]\naccess_token=x\nrefresh_token=y\nusername=\n",
        ),
    ]);
    let (mut device, _) = load(&storage);
    device.migrate_on_boot(1, &tempo_defaults());
    let s = device.settings();
    assert_eq!(s.tempo.bpm(), Some(120.0));
    assert_eq!(s.tempo.params[&1], 1.0);
    assert_eq!(s.tempo.params[&2], 0.0, "LED was off");
    assert!(
        !device
            .file(SettingsFileId::Zenjack)
            .contains("TempoSettings/tempoLedIsOff")
    );
    assert!(!s.new_models.0.contains_key("2.0.0"));
    assert_eq!(s.new_models.0["3.0.0"], [1, 2], "existing lists are kept");
    assert_eq!(s.new_models.0["4.1.0"].len(), 8);
    assert_eq!(s.tokens, AccountTokens::default());

    device.migrate_on_boot(6, &tempo_defaults());
    assert!(
        device
            .settings()
            .new_models
            .0
            .values()
            .all(|ids| ids == &[0])
    );
    assert_eq!(
        device
            .file(SettingsFileId::Permanent)
            .get("NewModels/3.0.0"),
        Some(&crate::IniValue::Text("0".into()))
    );
}

#[test]
fn resets_follow_stock() {
    let account = "[General]\naccess_token=x\nlast_paired_version=4.1.0\nrefresh_token=y\ntoken_expires=1\nusername=u\n";
    let files = [
        (SettingsFileId::Zenjack, ZENJACK),
        (
            SettingsFileId::Permanent,
            "[General]\ncloudDomain=AlternativeCloudDomain\n",
        ),
        (SettingsFileId::Account, account),
        (SettingsFileId::PluginLicenses, "[Licenses]\nnew=1, 2\n"),
    ];

    let mut storage = MemoryStorage::with(&files);
    let (mut device, _) = load(&storage);
    let events = device.reset(ResetKind::Settings, &tempo_defaults());
    assert!(events.iter().all(|e| e.file() == SettingsFileId::Zenjack));
    device.flush(&mut storage).unwrap();
    let text = storage.text(SettingsFileId::Zenjack);
    assert!(
        text.starts_with("[General]\noutput_0_paired=true\noutput_1_paired=true\n"),
        "{text}"
    );
    let (reloaded, _) = load(&storage);
    assert_eq!(reloaded.settings().tempo.params, tempo_defaults().params);
    assert_eq!(storage.text(SettingsFileId::Account), account);
    assert!(storage.0.contains_key(&SettingsFileId::PluginLicenses));

    let mut storage = MemoryStorage::with(&files);
    let (mut device, _) = load(&storage);
    let events = device.reset(ResetKind::Factory, &tempo_defaults());
    assert!(events.iter().any(|e| matches!(e, SettingsEvent::Tokens(_))));
    device.flush(&mut storage).unwrap();
    assert_eq!(
        storage.text(SettingsFileId::Account),
        "[General]\naccess_token=\nlast_paired_version=4.1.0\nrefresh_token=\ntoken_expires=1\nusername=\n"
    );
    assert!(!storage.0.contains_key(&SettingsFileId::PluginLicenses));
    assert_eq!(
        device.settings().cloud_domain,
        crate::permanent::CloudDomain::Alternative
    );
}

#[test]
fn credentials_never_appear_in_debug_output() {
    let storage = MemoryStorage::with(&[(
        SettingsFileId::Account,
        "[General]\naccess_token=tok-AAA\nrefresh_token=tok-BBB\n",
    )]);
    let (device, _) = load(&storage);
    assert_eq!(
        device.settings().tokens.access_token,
        Secret::new("tok-AAA")
    );
    let events = vec![SettingsEvent::Tokens(device.settings().tokens.clone())];
    for text in [
        format!("{device:?}"),
        format!("{:?}", device.settings()),
        format!("{events:?}"),
    ] {
        assert!(!text.contains("tok-"), "{text}");
    }
}

/// Reading the legacy groups changes nothing (a failed database import must
/// not lose them); only the explicit removal does.
#[test]
fn legacy_model_default_groups_are_read_then_removed() {
    let file = "[Amp7]\n12=@Variant(\\0\\0\\0\\x87?\\0\\0\\0)\n12_exp=1\n12_exp_trim_min=@Variant(\\0\\0\\0\\x87\\0\\0\\0\\0)\n\n[Tuner]\nx_exp_trim_min=1\n\n[Other]\nk=1\n";
    let (mut device, _) = load(&MemoryStorage::with(&[(SettingsFileId::Zenjack, file)]));
    let legacy = device.legacy_model_defaults();
    assert_eq!(device.dirty_files().count(), 0, "a read changes no file");
    device.remove_legacy_model_defaults(&legacy);
    assert_eq!(legacy.len(), 1);
    assert_eq!(legacy[0].group, "Amp7");
    let param = &legacy[0].params[&12];
    assert_eq!(
        (param.value.clone(), param.expression, param.trim_min),
        (Some(Variant::Float(0.5)), Some(1), Some(0.0))
    );
    assert_eq!((param.bypass_type, param.bypass_delay_ms), (2, 600));
    assert_eq!(
        device.file(SettingsFileId::Zenjack).child_groups(""),
        ["Other", "Tuner"]
    );
}

#[test]
fn expression_calibration_contract() {
    let range = ExpressionRange { min: 10, max: 210 };
    assert_eq!(range.normalize(110), 0.5);
    assert_eq!(range.normalize(0), 0.0);
    assert_eq!(range.normalize(250), 1.0);
    let mut calibrator = ExpressionCalibrator::default();
    assert_eq!(calibrator.finish(), None);
    for raw in [20, 240, 100] {
        calibrator.sample(raw);
    }
    assert_eq!(
        calibrator.finish(),
        Some(ExpressionRange { min: 25, max: 235 })
    );
}

/// Stock writes the tempo defaults at boot, so a BPM set after boot survives the next boot.
#[test]
fn tempo_bpm_survives_reboot() {
    let mut storage = MemoryStorage::default();
    let (mut device, _) = load(&storage);
    device.migrate_on_boot(1, &tempo_defaults());
    device.update(|s| {
        s.tempo.set_bpm(120.0).unwrap();
    });
    device.flush(&mut storage).unwrap();

    let (mut device, _) = load(&storage);
    device.migrate_on_boot(2, &tempo_defaults());
    assert_eq!(device.settings().tempo.bpm(), Some(120.0));
}

/// tetrad-midi maps its runtime copy onto these nine keys; names and stock encodings must hold.
#[test]
fn midi_group_uses_stock_keys_and_encodings() {
    let (device, issues) = load(&MemoryStorage::with(&[(
        SettingsFileId::Zenjack,
        "[MidiSettings]\nChannel=9\nClockInEn=true\nClockOutEn=3\nIgnoreDuplicatePCs=true\nLastMidiMessageSource=5\nOverUSB=false\nPCInput=3\nPCOutput=4\nPassthru=true\n",
    )]));
    assert!(issues.is_empty(), "{issues:?}");
    let midi = &device.settings().midi;
    assert_eq!(midi.channel, MidiChannel::new(9).unwrap());
    assert_eq!(midi.clock_out, MidiClockOut::UsbAndDin);
    assert_eq!(midi.last_message_source, 5);
    assert_eq!((midi.pc_input, midi.pc_output), (3, 4));
    assert!(midi.clock_in && midi.ignore_duplicate_pcs && midi.thru && !midi.over_usb);
}

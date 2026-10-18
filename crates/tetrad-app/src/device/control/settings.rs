//! MIDISettings (8), GeneralSettings (9) and GlobalEQ (38) over device state.

use tetrad_control::codecs::{
    AtmaPowerOnMode, CloudDomain, CortexMessageType, DownloadDestinationFolder, EnumValue,
    GeneralMIDIMessage, GeneralSettingsMessage, GlobalBypassRows, GlobalEQMessage,
    GlobalEQParameter, MIDISettingsMessage, MasterVolumeAssignmentOptions, MessageAction,
    MidiMessageInfo, ModelPresetID, PowerOptions, ProtoEnum, SceneBlockBypass, StorageInfo,
    StorageInfoEntity, StorageInfoType,
};

use tetrad_catalog::ModelSpec;
use tetrad_control::device::{DeviceError as ModelError, DeviceResult};
use tetrad_dsp_protocol::controls::ControlRequest;
use tetrad_engine::SceneIndex;
use tetrad_preset::{
    MIDI_GENERAL_GROUPS, MIDI_GENERAL_SLOTS, MIDI_MESSAGES, MidiList, MidiMessage,
};
use tetrad_settings::device::ResetKind;
use tetrad_settings::general::BypassSaveMode;
use tetrad_settings::io::GLOBAL_EQ_MODEL;
use tetrad_storage::stock::ProductType;
use tetrad_ui::{Row, SettingId, SettingValue};
use tetrad_watchdog::power::RestartReason;

use super::super::actions::{self, Action};
use super::super::global_eq;
use super::super::preset::{self, UndoPath};
use super::super::{DspCommand, Effect, PowerAction, State};
use super::grid::SPECIAL_FACTORY_PRESET;
use super::status::nonzero;
use super::{StateModel, files, midi_out_edit};

const MIDI: CortexMessageType = CortexMessageType::MIDISettings;

fn refused(message_type: CortexMessageType, what: impl ToString) -> ModelError {
    ModelError::Refused(message_type, what.to_string())
}

/// A raw stored entry: `kind` holds the type byte (`midi_out_edit` writes it back).
fn stored_message(info: &MidiMessageInfo) -> MidiMessage {
    MidiMessage {
        kind: info.message_type.unwrap_or(0) & 0xff,
        channel: info.channel.unwrap_or(0),
        param1: info.param1.unwrap_or(0),
        param2: info.param2.unwrap_or(0),
        param3: info.param3.unwrap_or(0),
        ..Default::default()
    }
}

/// `0x2df518`: the five fields, not the stored extras.
fn same_list(stored: &[MidiMessage], new: &[MidiMessage], len: usize) -> bool {
    let fields = |message: &MidiMessage| {
        (
            message.kind,
            message.channel,
            message.param1,
            message.param2,
            message.param3,
        )
    };
    let padded = |list: &[MidiMessage]| -> Vec<_> {
        (list.iter().map(fields))
            .chain(std::iter::repeat((0, 0, 0, 0, 0)))
            .take(len)
            .collect()
    };
    padded(stored) == padded(new)
}

/// MIDISettings UPDATE `0x1912d4`: `preset_load_messages` (field 3, `messages[0]` only,
/// `0x3bf5b0`), then each `general_midi_messages` entry (field 4, `0x3bf66c`). A per-source
/// entry always resets the source's CC toggles (`Effect::PresetMidiSource`), then returns
/// while a capture preset runs. A list equal to the stored one does nothing; else an undo
/// frame (`0x648f1c`), the store (`0x2e1780`, `0x2e1c0c`) and the stock echo
/// ([`midi_out_edit`]). Owned: an empty on-load entry, more than 12 items in it, and a source
/// of 10 or more are refused (stock fails a CHECK or writes out of bounds); while a capture
/// runs the on-load list is not stored (stock stores it into the capture preset, which Tetrad
/// keeps outside `State`). A source entry with more than 12 items is skipped, as stock.
pub(super) fn update_midi(
    model: &mut StateModel<'_>,
    update: &MIDISettingsMessage,
) -> DeviceResult<()> {
    if let Some(load) = &update.preset_load_messages {
        let entry = load
            .messages
            .first()
            .ok_or_else(|| refused(MIDI, "empty preset_load_messages"))?;
        if entry.messages.len() > MIDI_MESSAGES {
            return Err(refused(MIDI, "more than 12 on-load messages"));
        }
        if !model.capture_running {
            store_midi(model, MidiList::OnLoad, entry)?;
        }
    }
    let Some(general) = &update.general_midi_messages else {
        return Ok(());
    };
    for entry in &general.messages {
        let source = entry.source.unwrap_or(0) & 0xff;
        let group = usize::try_from(source)
            .ok()
            .filter(|&group| group < MIDI_GENERAL_GROUPS)
            .ok_or_else(|| refused(MIDI, format!("MIDI source {source}")))?;
        if entry.messages.len() > MIDI_GENERAL_SLOTS {
            continue;
        }
        model.effects.push(Effect::PresetMidiSource(group as u8));
        if model.capture_running {
            return Ok(());
        }
        store_midi(model, MidiList::Group(group), entry)?;
    }
    Ok(())
}

fn store_midi(
    model: &mut StateModel<'_>,
    list: MidiList,
    entry: &GeneralMIDIMessage,
) -> DeviceResult<()> {
    let messages: Vec<MidiMessage> = entry.messages.iter().map(stored_message).collect();
    let preset = &model.current(MIDI)?.preset;
    let (stored, len) = match list {
        MidiList::OnLoad => (&preset.midi_messages[..], MIDI_MESSAGES),
        MidiList::Group(group) => {
            let start = group * MIDI_GENERAL_SLOTS;
            let general = &preset.midi_messages_general_v2;
            (
                general
                    .get(start..start + MIDI_GENERAL_SLOTS)
                    .unwrap_or_default(),
                MIDI_GENERAL_SLOTS,
            )
        }
    };
    if same_list(stored, &messages, len) {
        return Ok(());
    }
    model.undoable(UndoPath::Direct, |model| {
        (model.current(MIDI)?.preset)
            .set_midi_list(list, &messages)
            .map_err(|error| refused(MIDI, error))?;
        model.effects.push(midi_out_edit(list, &messages));
        Ok(())
    })
}

const GENERAL: CortexMessageType = CortexMessageType::GeneralSettings;

/// The row flags are implicit presence: stock writes only the set ones (S7 capture: an empty
/// message for no row, `{headphones 1}` for the master volume assignment, C).
fn rows(rows: [bool; 4]) -> GlobalBypassRows {
    let [row1, row2, row3, row4] = rows.map(|row| row.then_some(true));
    GlobalBypassRows {
        row1,
        row2,
        row3,
        row4,
    }
}

fn scene_block_bypass(mode: BypassSaveMode) -> SceneBlockBypass {
    match mode {
        BypassSaveMode::Always => SceneBlockBypass::AlwaysOverwrite,
        BypassSaveMode::NonStomp => SceneBlockBypass::NonstompOverwrite,
        BypassSaveMode::Never => SceneBlockBypass::NeverOverwrite,
    }
}

/// GeneralSettings READ builder `0x16c964` without the storage fields ([`general_read`]),
/// also the source of the change pushes ([`general_pushes`]). `power_option` (13) is
/// Standby while Be Right Back runs (`0x6a8244` set: 2), else WakeUp (3); the Control bridge
/// keeps the flag from the [`standby_notice`] pushes. Field 41 is MidiController `+0xff`
/// (`0x3303c4`, has-bits `+0xc` bit 3). A download destination without a stored folder is
/// the database default folder (`0x76be70`): the `zenui.db` user root, `<id>_q` (S7 capture:
/// `1_q` for captures, `2_q` for IRs, C).
///
/// [`standby_notice`]: super::standby_notice
/// `lock_screen_and_volume_knob` is `input_locked`: input mask bit 0 clear (`0x5447b8`), set
/// by LOCK and by an update install, as the `0x16ad60` push reports it.
pub(super) fn general_message(
    state: &State,
    input_locked: bool,
    standby: bool,
) -> GeneralSettingsMessage {
    let settings = state.settings.settings();
    let (display, general, midi) = (&settings.display, &settings.general, &settings.midi);
    let volume = &settings.master_volume;
    let atma = state.platform.is_atma();
    let library = &settings.library;
    let database = (state.library.as_ref()).and_then(|library| library.database.as_ref());
    let user_root = |kind: ProductType| {
        let id = database?.view().root_folder(kind).ok()??;
        Some(format!("{id}_q"))
    };
    let folders = [
        (2, &library.download_destination_nc, ProductType::Capture),
        (1, &library.download_destination_ir, ProductType::Ir),
    ];
    let clock = state.midi_clock;
    GeneralSettingsMessage {
        screen_brightness: Some(display.brightness.get()),
        led_brightness: Some(display.led_brightness.get()),
        lock_screen_and_volume_knob: Some(input_locked),
        global_bypass_cab: Some(rows(settings.global_bypass.cab)),
        global_bypass_ir: Some(rows(settings.global_bypass.ir)),
        scene_block_bypass: Some(scene_block_bypass(general.bypass_save_mode).into()),
        midi_over_usb: Some(midi.over_usb),
        midi_channel: Some(midi.channel.get()),
        ignore_duplicate_pc: Some(midi.ignore_duplicate_pcs),
        power_option: Some(
            match standby {
                true => PowerOptions::Standby,
                false => PowerOptions::WakeUp,
            }
            .into(),
        ),
        power_button_sensitivity: Some(general.power_button_sensitivity.to_stock()),
        internal_midi_clock_enabled: Some(!clock.running()),
        // Stock leaves a false flag out (S7 capture: `{headphones: true}` only, D18).
        master_volume_assignment: Some(MasterVolumeAssignmentOptions {
            out12: volume.out12.then_some(true),
            out34: volume.out34.then_some(true),
            send12: volume.send12.then_some(true),
            headphones: volume.headphones.then_some(true),
        }),
        stomp_mode_auto_assign: Some(general.stomp_mode_auto_assign),
        swap_tempo_tuner_access: Some(general.swap_tempo_and_tuner_access),
        midi_clock_out: Some(tetrad_control::codecs::MidiClockOut::from_number(
            midi.clock_out.to_stock(),
        )),
        download_destination_folders: folders
            .into_iter()
            .filter_map(|(folder_type, key, kind)| {
                Some(DownloadDestinationFolder {
                    key: Some(key.clone().or_else(|| user_root(kind))?),
                    folder_type: Some(folder_type),
                })
            })
            .collect(),
        disable_internet_connection_check: Some(general.disable_connection_check),
        enable_dynamic_delay_compensation: Some(general.dynamic_delay_compensation),
        dimmed_led_brightness: Some(display.dimmed_led_brightness.get()),
        enable_preset_dimmed: Some(display.dimmed_preset),
        enable_scene_dimmed: Some(display.dimmed_scene),
        enable_stomp_dimmed: Some(display.dimmed_stomp),
        atma_power_mode: atma.then(|| {
            match general.atma_power_on_automatic {
                true => AtmaPowerOnMode::PowerOnWhenPluggedIn,
                false => AtmaPowerOnMode::PowerButton,
            }
            .into()
        }),
        looper_stomp_assignments: settings.looper.stomp_assignments().to_vec(),
        midi_clock_in_enabled: Some(midi.clock_in),
        atma_looper_lock_mode: atma.then_some(settings.looper.lock_mode),
        cloud_domain: Some(
            match settings.cloud_domain {
                tetrad_settings::permanent::CloudDomain::Default => CloudDomain::DefaultDomain,
                tetrad_settings::permanent::CloudDomain::Alternative => {
                    CloudDomain::AlternativeDomain
                }
            }
            .into(),
        ),
        gig_view_stomp_access_enabled: Some(general.gig_view_stomp_access),
        hold_timing: Some(general.hold_timing.get()),
        external_midi_clock_tempo: clock.running().then_some(clock.tempo),
        external_midi_clock_out_of_range: Some(clock.out_of_range),
        ..Default::default()
    }
}

/// The item limits of `StorageInfoEntity` `size_maximum`: presets `0x21050c`, captures
/// `0x2104dc`, IRs `0x2104f4`.
const STORAGE_LIMITS: [(StorageInfoType, i32); 3] = [
    (StorageInfoType::Presets, 3072),
    (StorageInfoType::NeuralCaptures, 2048),
    (StorageInfoType::ImpulseResponses, 2048),
];

/// The GeneralSettings READ and resync body (`0x16c964`, `0x16d394`): [`general_message`]
/// with the storage fields. `/media/p4` sizes are kB (`statvfs64` bytes `>> 10`, `0x210f6c`).
/// Fields 17 (free) and 18 (total) are whole GiB as f32: kB `>> 20`, then rounded to 0.1
/// (`0x211bc0`, `0x211c44`; a no-op after the shift). S7 capture: 25 and 27 for 26 682 928
/// and 28 371 548 kB. `storage_info` (36) lists presets, captures and IRs with their limit
/// ([`STORAGE_LIMITS`]) and count ([`files::stored_items`]). No reading sends 0, as stock
/// after a failed `statvfs64`; a library read error counts 0 items (owned).
pub(super) fn general_read(model: &StateModel<'_>) -> GeneralSettingsMessage {
    let kb = |bytes: u64| bytes >> 10;
    let (total, free) = model
        .disk_space
        .map_or((0, 0), |space| (kb(space.total), kb(space.available)));
    let gib = |kb: u64| (kb >> 20) as f32;
    let size = |kb: u64| nonzero(i64::try_from(kb).unwrap_or(i64::MAX));
    let counts = files::stored_items(model.state).unwrap_or_default();
    let storage_entities = (STORAGE_LIMITS.into_iter().zip(counts))
        .map(|((kind, maximum), count)| StorageInfoEntity {
            entity_type: (kind != StorageInfoType::Presets).then(|| kind.into()),
            size_maximum: Some(maximum),
            size_current: nonzero(i32::try_from(count).unwrap_or(i32::MAX)),
        })
        .collect();
    GeneralSettingsMessage {
        available_disk_space: Some(gib(free)),
        total_disk_space: Some(gib(total)),
        storage_info: Some(StorageInfo {
            total_storage_size_kb: size(total),
            free_storage_size_kb: size(free),
            storage_entities,
        }),
        ..general_message(model.state, model.input_locked, model.standby)
    }
}

/// The per-field pushes of the stock setters (`0x168974` screen, `0x168ffc` LED, …,
/// `0x166c04` hold timing; list in the R14 record): one UPDATE with the one changed field.
/// `lock_screen_and_volume_knob` and `power_option` are pushed by their actions
/// (`lock_notice`, `standby_notice`). Field 41: `0x32fdbc` → `0x16aa1c` (owned: a new
/// out-of-range BPM with the flag still set sends no second push). Owned: brightness is pushed on any change (stock: on a
/// change of the 32-step level).
pub(super) fn general_pushes(
    old: &GeneralSettingsMessage,
    new: &GeneralSettingsMessage,
) -> Vec<GeneralSettingsMessage> {
    let mut pushes = Vec::new();
    macro_rules! changed {
        ($($field:ident),* $(,)?) => {
            $(if old.$field != new.$field {
                pushes.push(GeneralSettingsMessage {
                    action: Some(MessageAction::Update.into()),
                    $field: new.$field.clone(),
                    ..Default::default()
                });
            })*
        };
    }
    changed!(
        screen_brightness,
        led_brightness,
        global_bypass_cab,
        global_bypass_ir,
        scene_block_bypass,
        midi_over_usb,
        midi_channel,
        ignore_duplicate_pc,
        power_button_sensitivity,
        internal_midi_clock_enabled,
        master_volume_assignment,
        stomp_mode_auto_assign,
        swap_tempo_tuner_access,
        midi_clock_out,
        download_destination_folders,
        disable_internet_connection_check,
        enable_dynamic_delay_compensation,
        dimmed_led_brightness,
        enable_preset_dimmed,
        enable_scene_dimmed,
        enable_stomp_dimmed,
        atma_power_mode,
        looper_stomp_assignments,
        midi_clock_in_enabled,
        atma_looper_lock_mode,
        cloud_domain,
        gig_view_stomp_access_enabled,
        hold_timing,
        external_midi_clock_tempo,
        external_midi_clock_out_of_range,
    );
    pushes
}

/// GeneralSettings UPDATE `0x163274`: only the first present field that passes its guard is
/// applied, in the stock order (16, 15, 3, 4, 5, 28, 29, 30, 31, 6, 39, 7, 8, 9, 10, 11, 12,
/// 13, 32, 14, 25, 22, 21, 23, 27, 24, 34, 26, 33, 35, 37, 38). Settings go through
/// `Action::ChangeSetting`, so the device side effects follow; the echo is the change push
/// of [`super::Control::sync`]. Not modelled: `reset_wifi_networks` (the Wi-Fi config reload
/// `0x5d53ec`, refused). Inferred: WakeUp runs even when the device is not in standby (the
/// flag lives in the UI bridge).
pub(super) fn update_general(
    model: &mut StateModel<'_>,
    update: &GeneralSettingsMessage,
) -> DeviceResult<()> {
    use SettingValue::{Bool, Choice, Number};
    let change = |id, value| Action::ChangeSetting { id, value };
    let percent = |value: Option<i32>| value.filter(|v| *v <= 100);
    let atma = model.state.platform.is_atma();
    let u = update;
    let action = if let Some(reset) = u.factory_reset {
        if !reset {
            return Ok(());
        }
        Action::Reset(ResetKind::Factory)
    } else if let Some(reset) = u.reset_settings {
        if !reset {
            return Ok(());
        }
        Action::Reset(ResetKind::Settings)
    } else if u.reset_wifi_networks.is_some() {
        return Err(ModelError::Unsupported(GENERAL));
    } else if let Some(v) = percent(u.screen_brightness) {
        change(SettingId::ScreenBrightness, Number(v))
    } else if let Some(v) = percent(u.led_brightness) {
        change(SettingId::LedBrightness, Number(v))
    } else if let Some(v) = percent(u.dimmed_led_brightness) {
        change(SettingId::DimmedLedBrightness, Number(v))
    } else if let Some(v) = u.enable_preset_dimmed {
        change(SettingId::PresetDimmed, Bool(v))
    } else if let Some(v) = u.enable_scene_dimmed {
        change(SettingId::SceneDimmed, Bool(v))
    } else if let Some(v) = u.enable_stomp_dimmed {
        change(SettingId::StompDimmed, Bool(v))
    } else if let Some(v) = u.lock_screen_and_volume_knob {
        Action::SetLocked(v)
    } else if let Some(v) = u.hold_timing {
        let v = u16::try_from(v).map_err(|_| refused(GENERAL, format!("hold timing {v}")))?;
        change(SettingId::HoldTiming, Choice(v))
    } else if let Some(cab) = &u.global_bypass_cab {
        return bypass_rows(model, SettingId::GlobalBypassCab, cab);
    } else if let Some(ir) = &u.global_bypass_ir {
        return bypass_rows(model, SettingId::GlobalBypassIr, ir);
    } else if let Some(v) = u.scene_block_bypass {
        match v {
            EnumValue::Known(mode) => change(SettingId::SceneBypassBehavior, Choice(mode as u16)),
            EnumValue::Unknown(_) => return Ok(()),
        }
    } else if let Some(v) = u.midi_over_usb {
        change(SettingId::MidiOverUsb, Bool(v))
    } else if let Some(channel) = u.midi_channel {
        // tetrad-ui index = channel − 1; 17 is OMNI (`0x4163c4` takes 1..=17).
        match u16::try_from(channel) {
            Ok(channel @ 1..=17) => change(SettingId::MidiChannel, Choice(channel - 1)),
            _ => return Ok(()),
        }
    } else if let Some(v) = u.ignore_duplicate_pc {
        change(SettingId::IgnoreDuplicatePc, Bool(v))
    } else if let Some(option) = u.power_option {
        match option {
            EnumValue::Known(PowerOptions::Shutdown) => {
                model.effects.push(Effect::Power(PowerAction::PowerOff));
                return Ok(());
            }
            EnumValue::Known(PowerOptions::Reboot) => {
                let restart = PowerAction::Restart(RestartReason::PowerMenu);
                model.effects.push(Effect::Power(restart));
                return Ok(());
            }
            EnumValue::Known(PowerOptions::Standby) => Action::Standby(true),
            EnumValue::Known(PowerOptions::WakeUp) => Action::Standby(false),
            EnumValue::Unknown(_) => return Ok(()),
        }
    } else if let Some(mode) = u.atma_power_mode.filter(|_| atma) {
        change(SettingId::PowerOnMode, Choice(mode.number() as u16))
    } else if let Some(v) = u.power_button_sensitivity {
        let v = u16::try_from(v).map_err(|_| refused(GENERAL, format!("sensitivity {v}")))?;
        change(SettingId::PowerButtonSensitivity, Choice(v))
    } else if !u.download_destination_folders.is_empty() {
        return download_folders(model, &u.download_destination_folders);
    } else if let Some(v) = u.stomp_mode_auto_assign {
        change(SettingId::StompModeAutoAssign, Bool(v))
    } else if let Some(assignment) = &u.master_volume_assignment {
        return master_volume_assignment(model, assignment);
    } else if let Some(v) = u.swap_tempo_tuner_access {
        change(SettingId::SwapTempoTuner, Bool(v))
    } else if let Some(v) = u.enable_dynamic_delay_compensation {
        change(SettingId::LatencyCompensation, Bool(v))
    } else if let Some(v) = u.midi_clock_out {
        change(SettingId::MidiClockOut, Choice(v.number() as u16))
    } else if let Some(v) = u.midi_clock_in_enabled {
        change(SettingId::MidiClockIn, Bool(v))
    } else if let Some(v) = u.disable_internet_connection_check {
        change(SettingId::InternetCheck, Bool(!v))
    } else if !u.looper_stomp_assignments.is_empty() {
        // `0x651fa4` takes exactly eight entries; others are logged and dropped.
        let assignments: [i32; 8] = (u.looper_stomp_assignments.as_slice().try_into())
            .map_err(|_| refused(GENERAL, "looper stomp assignments need 8 entries"))?;
        let mut result = Ok(());
        let effects = actions::update_settings(model.state, |settings| {
            result = settings.looper.set_stomp_assignments(assignments);
        });
        result.map_err(|error| refused(GENERAL, error))?;
        model.effects.extend(effects);
        return Ok(());
    } else if let Some(v) = u.atma_looper_lock_mode.filter(|_| atma) {
        model.settings(|settings| settings.looper.lock_mode = v);
        return Ok(());
    } else if let Some(domain) = u.cloud_domain {
        let domain = match domain {
            EnumValue::Known(CloudDomain::DefaultDomain) => {
                tetrad_settings::permanent::CloudDomain::Default
            }
            EnumValue::Known(CloudDomain::AlternativeDomain) => {
                tetrad_settings::permanent::CloudDomain::Alternative
            }
            EnumValue::Unknown(_) => return Ok(()),
        };
        model.settings(|settings| settings.cloud_domain = domain);
        return Ok(());
    } else if let Some(v) = u.gig_view_stomp_access_enabled {
        change(SettingId::GigViewAccess, Bool(v))
    } else {
        return Ok(());
    };
    model.act(GENERAL, action)
}

/// `0x4111cc`: all four rows of the cab or IR global bypass.
fn bypass_rows(
    model: &mut StateModel<'_>,
    id: fn(Row) -> SettingId,
    rows: &GlobalBypassRows,
) -> DeviceResult<()> {
    let values = [rows.row1, rows.row2, rows.row3, rows.row4];
    for (index, value) in values.into_iter().enumerate() {
        let row = Row::new(index as u8).map_err(|error| refused(GENERAL, error))?;
        let action = Action::ChangeSetting {
            id: id(row),
            value: SettingValue::Bool(value.unwrap_or(false)),
        };
        model.act(GENERAL, action)?;
    }
    Ok(())
}

/// `0x164fe4` → `0x1fc414` for each of the four outputs.
fn master_volume_assignment(
    model: &mut StateModel<'_>,
    assignment: &MasterVolumeAssignmentOptions,
) -> DeviceResult<()> {
    let outputs = [
        (SettingId::MasterVolumeOut12, assignment.out12),
        (SettingId::MasterVolumeOut34, assignment.out34),
        (SettingId::MasterVolumeSend12, assignment.send12),
        (SettingId::MasterVolumeHeadphones, assignment.headphones),
    ];
    for (id, value) in outputs {
        let value = SettingValue::Bool(value.unwrap_or(false));
        model.act(GENERAL, Action::ChangeSetting { id, value })?;
    }
    Ok(())
}

/// `0x76bed4(type, folder)`: type 2 captures, 1 IRs (`0xa0fff8`). Inferred: the key is stored
/// as given (stock first looks the folder up, `0x76bc68`).
fn download_folders(
    model: &mut StateModel<'_>,
    folders: &[DownloadDestinationFolder],
) -> DeviceResult<()> {
    for folder in folders {
        let key = folder.key.clone().unwrap_or_default();
        match folder.folder_type {
            Some(2) => {
                model.settings(|settings| settings.library.download_destination_nc = Some(key))
            }
            Some(1) => {
                model.settings(|settings| settings.library.download_destination_ir = Some(key))
            }
            other => return Err(refused(GENERAL, format!("download folder type {other:?}"))),
        }
    }
    Ok(())
}

const EQ: CortexMessageType = CortexMessageType::GlobalEQ;
/// Wire indices 0..=27 (`0x16d6c4`, `0x16e044` bound `< 0x1c`): five bands of GAIN, FREQ, Q,
/// TYPE and BYPASS, OUTPUT (25), ASSIGN OUT 1/2 and OUT 3/4 (26, 27).
const EQ_WIRE_PARAMS: u16 = 28;
/// The two ASSIGN flags (`0x2fbce0`).
const EQ_ASSIGN: [u16; 2] = [26, 27];

/// GlobalEQ state `0x16e044` (READ, resync `0x16e460`, and the source of the change pushes):
/// the scene-0 value of indices 0..=27, then `bypassed`. `None` without the catalog model.
pub(super) fn global_eq_message(state: &State) -> Option<GlobalEQMessage> {
    let spec = global_eq::model(state)?;
    Some(GlobalEQMessage {
        action: Some(MessageAction::Update.into()),
        parameters: (0..EQ_WIRE_PARAMS)
            .filter_map(|index| {
                Some(GlobalEQParameter {
                    parameter_index: Some(i32::from(index)),
                    value: Some(global_eq::stored(state, spec, index)?),
                })
            })
            .collect(),
        bypassed: Some(global_eq::bypassed(state)),
        ..Default::default()
    })
}

/// The Global EQ pushes of a device or Control edit: `0x16eb54` {index, value} per changed
/// parameter (`0x16e7b8` for ASSIGN has the same shape), then `0x16e474` {bypassed}.
/// Inferred: a Control edit is echoed too (stock gates `0x16eb54` on sender flag `+5`, whose
/// start value was not found).
pub(super) fn global_eq_pushes(
    old: &GlobalEQMessage,
    new: &GlobalEQMessage,
) -> Vec<GlobalEQMessage> {
    let update = || Some(MessageAction::Update.into());
    let mut pushes: Vec<GlobalEQMessage> = (new.parameters.iter())
        .filter(|parameter| !old.parameters.contains(parameter))
        .map(|parameter| GlobalEQMessage {
            action: update(),
            parameters: vec![parameter.clone()],
            ..Default::default()
        })
        .collect();
    if old.bypassed != new.bypassed {
        pushes.push(GlobalEQMessage {
            action: update(),
            bypassed: new.bypassed,
            ..Default::default()
        });
    }
    pushes
}

fn eq_spec<'a>(model: &'a StateModel<'_>) -> DeviceResult<&'a ModelSpec> {
    global_eq::model(model.state).ok_or_else(|| refused(EQ, "no Global EQ model (4004)"))
}

/// Stores every index with `changes` applied, as the saver `0x2fa580` writes them.
fn store_eq(
    model: &mut StateModel<'_>,
    changes: impl IntoIterator<Item = (u16, f32)>,
) -> DeviceResult<()> {
    let spec = eq_spec(model)?;
    let mut all = global_eq::every_index(model.state, spec);
    all.extend(changes);
    model.settings(|settings| settings.global_eq.params = all);
    Ok(())
}

/// `0x2fa314(eq, index, value)` then the slot `0x2fbe44` per output: ASSIGN stores 1.0 or
/// 0.0 (`> 0.5`) and runs `0x2fbce0`; any other index goes to each assigned output's DSP
/// (`0x5150a0`), with no ON check. Owned: a negative index (stock accepts it) and a value that
/// is not finite or outside 0..=1 are refused; an ASSIGN change pushes both outputs
/// (`global_eq::push`; stock pushes the changed one).
pub(super) fn set_eq_parameter(
    model: &mut StateModel<'_>,
    parameter: &GlobalEQParameter,
) -> DeviceResult<()> {
    let index = parameter.parameter_index.unwrap_or(0);
    let value = parameter.value.unwrap_or(0.0);
    let index =
        u16::try_from(index).map_err(|_| refused(EQ, format!("Global EQ index {index}")))?;
    if !(0.0..=1.0).contains(&value) {
        return Err(refused(EQ, format!("Global EQ value {value}")));
    }
    if EQ_ASSIGN.contains(&index) {
        store_eq(model, [(index, if value > 0.5 { 1.0 } else { 0.0 })])?;
        let effects = global_eq::push(model.state);
        model.effects.extend(effects);
        return Ok(());
    }
    store_eq(model, [(index, value)])?;
    let spec = eq_spec(model)?;
    let sends: Vec<Effect> = (0u8..)
        .zip(EQ_ASSIGN)
        .filter(|&(_, assign)| {
            global_eq::stored(model.state, spec, assign).is_some_and(|flag| flag > 0.0)
        })
        .map(|(output, _)| {
            Effect::Dsp(DspCommand::Control(
                ControlRequest::OutputEqualizerParameter {
                    output,
                    parameter: i32::from(index),
                    value_bits: value.to_bits(),
                },
            ))
        })
        .collect();
    model.effects.extend(sends);
    Ok(())
}

/// The rows of a stored Virtual Device Preset (`model_preset_params`; factory.db when
/// `is_factory`). The special id (`SpecialFactoryModelPresetID`) has none.
pub(super) fn stored_preset_rows(
    model: &StateModel<'_>,
    preset: &ModelPresetID,
    message_type: CortexMessageType,
) -> DeviceResult<Vec<tetrad_storage::stock::ParamEntry>> {
    let value = preset.value.as_deref().unwrap_or_default();
    if value == SPECIAL_FACTORY_PRESET {
        return Ok(Vec::new());
    }
    let id: i64 =
        (value.parse()).map_err(|_| refused(message_type, format!("model preset id {value:?}")))?;
    let database = if preset.is_factory == Some(true) {
        model.factory
    } else {
        (model.state.library.as_ref()).and_then(|library| library.database.as_ref())
    };
    let database =
        database.ok_or_else(|| refused(message_type, "the preset database is not open"))?;
    (database.view().model_preset_params(id)).map_err(|error| refused(message_type, error))
}

/// `0x2fb9fc`: an equal value does nothing; else the ON button (`global_eq::change`).
pub(super) fn set_eq_bypass(model: &mut StateModel<'_>, bypassed: bool) -> DeviceResult<()> {
    eq_spec(model)?;
    if bypassed == global_eq::bypassed(model.state) {
        return Ok(());
    }
    let action = Action::ChangeSetting {
        id: SettingId::EqEnabled,
        value: SettingValue::Bool(!bypassed),
    };
    model.act(EQ, action)
}

/// `model_preset_to_load` (`0x1cf838` → `0x1f7e74` → `0x1f7b10` → `0x2fca74`): a fresh 4004
/// block with the preset's rows (zenui.db, or factory.db when `is_factory`; the special id
/// keeps the catalog defaults, `0x6678d4`), stored at once, then both outputs and the state
/// push. Owned: a hash other than 4004 is refused (stock does not check). Inferred: MUTE and
/// BYPASS take the fresh block values (0, so the EQ is ON).
pub(super) fn load_eq_preset(
    model: &mut StateModel<'_>,
    preset: &ModelPresetID,
) -> DeviceResult<()> {
    if preset.hash != Some(GLOBAL_EQ_MODEL) {
        return Err(refused(
            EQ,
            format!("model {:?} is not the Global EQ", preset.hash),
        ));
    }
    let rows = stored_preset_rows(model, preset, EQ)?;
    let specs = (model.state.specs.as_ref()).ok_or_else(|| refused(EQ, "no catalog"))?;
    let block = preset::model_preset_block(specs, GLOBAL_EQ_MODEL, None, &rows)
        .ok_or_else(|| refused(EQ, "no Global EQ model (4004)"))?;
    let scene = SceneIndex::new(0).expect("scene 0");
    let values: Vec<f32> = block.params.iter().map(|param| param.at(scene)).collect();
    let mute_and_bypass = [0.0, 0.0];
    store_eq(
        model,
        (0u16..).zip(values.into_iter().chain(mute_and_bypass)),
    )?;
    let effects = global_eq::push(model.state);
    model.effects.extend(effects);
    Ok(())
}

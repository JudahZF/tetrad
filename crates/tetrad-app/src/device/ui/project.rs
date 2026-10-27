//! Device [`State`] → [`UiSnapshot`]. Pure apart from the library cache, which the bridge
//! refreshes only when the browsed setlist or the loaded preset changes (directory reads).
//!
//! Cost per call: 32 grid cells, the selected block's parameters (formatted by tetrad-core),
//! about 40 settings and the cached catalog list. `State` has no change flags, so the bridge
//! rebuilds on every input and frame (not on a clock tick that changes nothing) and hands
//! tetrad-ui a new snapshot only when it differs (`set_snapshot` cancels the pointer gesture).
//! The bank preview is the footswitch bank overlay in `State`.

use std::collections::BTreeMap;

use std::sync::Arc;
use tetrad_catalog::{ModelSpec, Specs};
use tetrad_core::conversion::{ParameterScale, ParameterType};
use tetrad_core::performance::FootswitchMode as CoreMode;
use tetrad_core::performance::footswitch::BankSize;
use tetrad_dsp_protocol::responses::LooperStatus;
use tetrad_engine::{
    Column, DeploymentPlan, Instance, Layout, Pair, Row, SceneIndex as EngineScene,
    SidechainSource, Site, Slot,
};
use tetrad_library::stock::Setlist;

use tetrad_media::block::IrLoaderModel;
use tetrad_media::user::{IrReference, display_name, reference_params};
use tetrad_settings::device::Settings;
use tetrad_settings::general::BypassSaveMode;
use tetrad_ui::{
    AboutView, AccountView, BankSlot, BankView, BlockInfo, DspCoreView, FOOTSWITCH_COUNT,
    FootswitchMode, GRID_COLUMNS, GridCell, GridPosition, HybridRows, InputPort, LooperState,
    LooperView, LooperWait, MAX_LIST_ITEMS, MAX_PRESETS, MediaEntry, MediaKind, ModelCategory,
    ModelEntry, OutputPort, PairView, ParameterKind, ParameterView, PresetItem, RowView,
    SceneIndex, SetlistItem, SettingId, SettingValue, StompView, TempoView, UiSnapshot,
    UpdaterView,
};

use super::super::actions::stomp_slots;
use super::super::{State, meters};

/// Stock setlists hold 256 slots; tetrad-ui lists at most [`MAX_PRESETS`] at a time.
pub(super) const SETLIST_SLOTS: usize = 256;
/// Owned: the looper status carries sample counts; the QC engine runs at 48 kHz (inferred).
const SAMPLE_RATE: f32 = 48_000.0;
/// `0x523644` rejects parameter indices above 99.
const MAX_PARAMETER_INDEX: u32 = 99;

/// Library listing the bridge keeps between frames.
#[derive(Debug, Default)]
pub(super) struct LibraryView {
    pub directory: Arc<tetrad_ui::DirectoryView>,
    pub titles: BTreeMap<u64, String>,
    pub setlists: Vec<Setlist>,
    /// Index into `setlists` of the setlist the presets screen shows.
    pub browse: usize,
    /// Slots of the browsed setlist (`None` is empty).
    pub slots: Vec<Option<String>>,
    /// Preset paths in the favorites file, newest first.
    pub favorites: Vec<String>,
    /// Filled preset slots in every setlist (Device Storage "My Presets").
    pub presets: u32,
}

impl LibraryView {
    pub fn browsed(&self) -> Option<&Setlist> {
        self.setlists.get(self.browse)
    }
}

/// Bridge-local view state that the projection reads.
#[derive(Debug, Default)]
pub(super) struct Local<'a> {
    pub library: Option<&'a LibraryView>,
    /// Shared with the bridge: a snapshot copies the pointer, not the list.
    pub models: Arc<Vec<ModelEntry>>,
    pub media: Arc<Vec<MediaEntry>>,
    pub capture_library: Arc<tetrad_ui::CaptureLibrary>,
    pub capture_devices: Arc<BTreeMap<String, tetrad_ui::CaptureDevice>>,
    pub selected: Option<GridPosition>,
    pub clipboard: bool,
    pub dirty: bool,
    /// `zenui.db` Virtual Device Presets and the one auditioned in the selected block.
    pub device_presets: std::sync::Arc<Vec<tetrad_ui::DevicePresetItem>>,
    pub device_preset: Option<u64>,
    /// Pinned models in pin order (`pinned_models`).
    pub pinned_models: &'a [u32],
    pub notice: Option<tetrad_ui::NoticeView>,
    /// The open lane control or junction editor.
    pub site: Option<tetrad_ui::EditorSite>,
}

/// Device Information (`0x40d27c`) from `State::info`. The Zenjack and Zencoder rows show on
/// the boards that have them (not the mini, not ESS); firmware words print as hex.
fn about(state: &State) -> AboutView {
    let info = &state.info;
    let firmware = |version: Option<tetrad_hal::zencoder::FirmwareVersion>| {
        version.map(|version| version.to_string())
    };
    AboutView {
        device_name: state.settings.settings().general.custom_name.clone(),
        serial: info
            .serial
            .as_ref()
            .map(|serial| serial.as_str().to_owned()),
        mac: info.mac.clone(),
        zen_boards: !state.platform.is_atma() && !state.platform.has_ess_codec(),
        coros: Some(tetrad_firmware::running().version.into()),
        kernel: info.kernel.clone(),
        uboot: info.uboot.clone(),
        wireless_fw: info.wireless.clone(),
        zenjack_app: firmware(info.zenjack[0]),
        zenjack_bootloader: firmware(info.zenjack[1]),
        zencoder_app: firmware(info.zencoder[0]),
        zencoder_bootloader: firmware(info.zencoder[1]),
    }
}

pub(super) fn snapshot(state: &State, local: &Local) -> UiSnapshot {
    let settings = state.settings.settings();
    let index = state.performance.footswitch.current_preset;
    let mode = state.performance.footswitch.mode;
    let (footswitch_mode, hybrid) = view_mode(mode);
    // 8 per bank, or 4 when the mode cycle holds a hybrid with PRESET (`0x58b968`).
    let size = state.performance.footswitch.bank_size;
    let current_bank = u16::try_from(size.bank(index)).unwrap_or(u16::MAX);
    // The footswitch bank overlay is the bank preview (UP/DOWN hold, touch preview).
    let overlay = state.performance.footswitch.overlay;
    let shown_bank = overlay.map_or(current_bank, |overlay| {
        u16::try_from(overlay.bank).unwrap_or(u16::MAX)
    });
    let mut snapshot = UiSnapshot {
        preset_slot: Some(BankSlot {
            bank: current_bank,
            slot: (index % size.get()) as u8,
        }),
        mode_id: mode as u8,
        footswitch_mode: Some(if state.performance.footswitch.custom {
            FootswitchMode::Custom
        } else {
            footswitch_mode
        }),
        custom: super::super::custom::view(state),
        // The Modes configuration shows the cycle's hybrid slot while another mode is active.
        hybrid: hybrid
            .or_else(|| {
                settings.mode.available().iter().find_map(|stored| {
                    super::super::actions::core_mode(*stored).and_then(|mode| view_mode(mode).1)
                })
            })
            .unwrap_or_default(),
        allowed_modes: allowed_modes(settings),
        bank_preview_active: overlay.is_some(),
        bank_size: u8::try_from(size.get()).unwrap_or(u8::MAX),
        status: String::new(),
        clipboard: local.clipboard,
        dirty: local.dirty,
        models: Arc::clone(&local.models),
        media: Arc::clone(&local.media),
        capture_library: Arc::clone(&local.capture_library),
        device_presets: Arc::clone(&local.device_presets),
        pinned_models: local.pinned_models.to_vec(),
        device_preset: local.device_preset,
        settings: {
            let mut map = settings_map(settings);
            // Power Functions "Lock Touchscreen and Master Volume" (`0x11ff6f8`).
            let locked = state.master_volume.locked;
            map.insert(SettingId::LockScreen, SettingValue::Bool(locked));
            map
        },
        tempo: TempoView {
            bpm: super::super::actions::tempo_bpm(state) as f32,
            metronome: state
                .metronome_status
                .is_some_and(|status| status.words[0] != 0),
            beat: state
                .metronome_status
                .map(|status| (status.words[2] % beats(state)) as u8),
            beats_per_bar: beats(state) as u8,
            accents: accents(state),
        },
        looper: LooperView {
            editor_view: state.looper_view,
            mini: state.platform.is_atma(),
            tiles: looper_tiles(state),
            ..state
                .looper
                .as_ref()
                .map(|status| looper_view(status, state.looper_playback.cached_state))
                .unwrap_or_default()
        },
        tuner: tuner_view(state),
        account: AccountView {
            username: Some(settings.account.username.clone()).filter(|name| !name.is_empty()),
        },
        about: about(state),
        dsp_diagnostics: state.diagnostics.cores.map(|core| {
            core.map(|core| DspCoreView {
                dropped_messages: core.dropped_messages,
                fp_exceptions: core.fp_exceptions,
                cpu: core.cpu,
                emdma: core.emdma,
                heap: core.heap,
            })
        }),
        updater: UpdaterView {
            current_version: tetrad_firmware::running().version.into(),
            ..UpdaterView::default()
        },
        io_meters: state.io_meters,
        grid_clips: state.grid_clips.view(state.now),
        gate_meters: meters::gate_meters(state),
        mini: state.platform.is_atma(),
        locked_models: locked_models(state),
        // The Grid CPU Monitor (M694-697): the DSP Diagnostics mean of the four cores.
        cpu_percent: state.cpu.percent(),
        scene_copy: state.perform.scene_clipboard.is_some(),
        notice: local.notice.clone(),
        dsp_error: state.dsp_fault.is_some_and(|fault| fault.dialog),
        ..UiSnapshot::default()
    };
    project_tempo_settings(state, &mut snapshot.settings);
    snapshot.settings.extend(io_settings(&state.io, settings));
    snapshot
        .settings
        .extend(super::super::global_eq::settings(state));
    if let Some(current) = &*state.current {
        snapshot.preset_midi = preset_midi(&current.preset);
    }
    if let Some(library) = local.library {
        project_library(&mut snapshot, library, shown_bank, size);
        snapshot.bank.highlighted = overlay.and_then(|overlay| overlay.highlighted);
    }
    match &*state.current {
        Some(current) => {
            let preset = &current.preset;
            let layout = current.deployment.layout();
            let scene = current.deployment.scene();
            snapshot.preset_id = Some(u64::from(index));
            snapshot.preset_title = preset.name.clone().unwrap_or_else(|| "NEW PRESET".into());
            snapshot.preset_file = current.path.is_some();
            snapshot.preset_instrument = state.preset_instrument;
            snapshot.can_undo = state.history.can_undo();
            snapshot.can_redo = state.history.can_redo();
            snapshot.favorite = current.path.as_ref().is_some_and(|path| {
                local
                    .library
                    .is_some_and(|library| library.favorites.contains(path))
            });
            snapshot.selected_scene =
                SceneIndex::new(scene.index() as u8).unwrap_or(snapshot.selected_scene);
            for (slot, view) in snapshot.scenes.iter_mut().enumerate() {
                view.label = preset.scene_labels.get(slot).cloned().unwrap_or_default();
                view.raw_color = preset.scene_colors.get(slot).copied().unwrap_or(0);
            }
            project_grid(
                &mut snapshot,
                layout,
                &preset.chains,
                scene,
                state.specs.as_ref(),
            );
            project_sidechains(&mut snapshot, current.deployment.plan(), scene);
            project_captures(
                &mut snapshot,
                layout,
                scene,
                &local.capture_devices,
                &local.media,
            );
            project_stomps(&mut snapshot, current, layout, scene, state.specs.as_ref());
            snapshot.selected_block = local
                .selected
                .filter(|&position| instance(layout, position).is_some());
            snapshot.selected_stomp = snapshot.selected_block.and_then(|selected| {
                (0..tetrad_ui::FOOTSWITCH_COUNT as u8).find(|tile| {
                    snapshot.stomps[usize::from(*tile)]
                        .blocks
                        .contains(&selected)
                })
            });
            if let (Some(position), Some(specs)) = (snapshot.selected_block, &state.specs) {
                snapshot.parameters = parameters(layout, position, scene, specs, &local.media);
            }
            // The editor meters (`Action::EditorBlock` polls them, `meters::on_model_meter`).
            if let Some(slot) = snapshot.selected_block.and_then(slot_of)
                && let Some((_, values)) = state.model_meters.cells().find(|(at, _)| *at == slot)
            {
                snapshot.editor_meters = (values.iter())
                    .filter(|(_, level)| level.is_finite())
                    .filter_map(|(&index, &level)| Some((u16::try_from(index).ok()?, level)))
                    .collect();
            }
            // Minivoicer V1/V2 notes (`0x575010` → read-out `0x61d994`).
            if let Some(notes) = (snapshot.selected_block.and_then(slot_of))
                .and_then(|slot| state.voice_notes.get(&slot))
            {
                snapshot.voice_notes = *notes;
            }
            if let Some(block) = snapshot
                .selected_block
                .and_then(|position| instance(layout, position))
            {
                snapshot.step_connected = step_connected(block, scene);
            }
            if let (Some(site), Some(specs)) = (local.site, &state.specs)
                && let Some(block) = layout.instance(engine_site(site))
            {
                snapshot.selected_site = Some(site);
                snapshot.parameters = instance_parameters(block, scene, specs, &local.media);
            }
            if let Some(position) = snapshot.selected_block {
                (
                    snapshot.expression,
                    snapshot.expression_switch,
                    snapshot.expression_ranges,
                ) = super::picker::expression_view(preset, position);
            }
        }
        None => snapshot.status = "NO PRESET LOADED".into(),
    }
    snapshot
}

/// Plugin models that deploy bypassed because their licence is not active (the same gate as
/// the DSP push, `0x00521c84`): the UI draws a lock on them.
fn locked_models(state: &State) -> std::collections::BTreeSet<u32> {
    let Some(specs) = &state.specs else {
        return Default::default();
    };
    specs
        .models
        .values()
        .filter(|model| {
            model.plugin_id.is_some_and(|plugin| {
                state.licences.status(plugin) != super::super::services::PluginStatus::Active
            })
        })
        .map(|model| model.id)
        .collect()
}

/// tetrad-core mode → mode chip and hybrid rows. Hybrid rows: first named mode on the upper
/// row (A–D), second on the lower row (E–H) (stock `0x65dbcc`, `0x678304`).
fn view_mode(mode: CoreMode) -> (FootswitchMode, Option<HybridRows>) {
    use FootswitchMode::{Preset, Scene, Stomp};
    let rows = |upper, lower| (FootswitchMode::Hybrid, Some(HybridRows { upper, lower }));
    match mode {
        CoreMode::Preset => (Preset, None),
        CoreMode::Scene => (Scene, None),
        CoreMode::Stomp => (Stomp, None),
        CoreMode::PresetScene => rows(Preset, Scene),
        CoreMode::PresetStomp => rows(Preset, Stomp),
        CoreMode::ScenePreset => rows(Scene, Preset),
        CoreMode::SceneStomp => rows(Scene, Stomp),
        CoreMode::StompPreset => rows(Stomp, Preset),
        CoreMode::StompScene => rows(Stomp, Scene),
    }
}

fn allowed_modes(settings: &Settings) -> Vec<FootswitchMode> {
    let mut modes = Vec::new();
    for mode in settings.mode.available() {
        let view = match mode.to_stock() {
            0 => FootswitchMode::Preset,
            1 => FootswitchMode::Scene,
            2 => FootswitchMode::Stomp,
            _ => FootswitchMode::Hybrid,
        };
        if !modes.contains(&view) {
            modes.push(view);
        }
    }
    modes
}

fn project_library(snapshot: &mut UiSnapshot, library: &LibraryView, bank: u16, size: BankSize) {
    snapshot.directory = Arc::clone(&library.directory);
    let title = |slot: Option<&Option<String>>| match slot {
        Some(Some(path)) => preset_title(path),
        _ => "Unsaved".to_owned(),
    };
    snapshot.setlists = library
        .setlists
        .iter()
        .take(MAX_LIST_ITEMS)
        .enumerate()
        .map(|(id, setlist)| SetlistItem {
            id: id as u64,
            title: setlist.name.clone(),
        })
        .collect();
    snapshot.active_setlist = library.browsed().map(|_| library.browse as u64);
    // Owned: favorites show their file stem; the stored names would need 64 file reads.
    snapshot.favorite_presets = library
        .favorites
        .iter()
        .take(MAX_LIST_ITEMS)
        .enumerate()
        .map(|(id, path)| PresetItem {
            id: id as u64,
            title: preset_title(path),
        })
        .collect();
    let bank_first = size.first(u32::from(bank)) as usize;
    // The window of MAX_PRESETS slots that holds the shown bank.
    let first = bank_first / MAX_PRESETS * MAX_PRESETS;
    snapshot.presets = (first..(first + MAX_PRESETS).min(SETLIST_SLOTS))
        .map(|slot| PresetItem {
            id: slot as u64,
            title: library
                .titles
                .get(&(slot as u64))
                .cloned()
                .unwrap_or_else(|| title(library.slots.get(slot))),
        })
        .collect();
    // Eight tiles from the bank start: a hybrid preset half shows the first four, pure
    // PRESET with split banks shows two banks of four (`0x660c78`).
    snapshot.bank = BankView {
        index: bank,
        presets: (bank_first..(bank_first + FOOTSWITCH_COUNT).min(SETLIST_SLOTS))
            .map(|slot| PresetItem {
                id: slot as u64,
                title: library
                    .titles
                    .get(&(slot as u64))
                    .cloned()
                    .unwrap_or_else(|| title(library.slots.get(slot))),
            })
            .collect(),
        highlighted: None,
    };
}

/// `/media/p4/Presets/My Presets/Light.pb` → `Light`.
pub(crate) fn preset_title(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.strip_suffix(".pb").unwrap_or(file).to_owned()
}

pub(super) fn slot_of(position: GridPosition) -> Option<Slot> {
    Some(Slot {
        row: Row::new(position.row()).ok()?,
        column: Column::new(position.column()).ok()?,
    })
}

pub(super) fn instance(layout: &Layout, position: GridPosition) -> Option<&Instance> {
    let slot = slot_of(position)?;
    layout.lane(slot.row).slots[slot.column.index()].as_ref()
}

fn model_name(specs: Option<&Specs>, model: u32) -> String {
    specs
        .and_then(|specs| specs.model(model))
        .map_or_else(|| format!("MODEL {model}"), |spec| spec.name.clone())
}

/// The grid cells, block metadata and rows of a layout (also the Control screenshot,
/// `control/screen.rs`). `chains` are the stored chains of the same preset: they keep the
/// path B ports that a junction replaces, which the import drops from the layout.
pub(in crate::device) fn project_grid(
    snapshot: &mut UiSnapshot,
    layout: &Layout,
    chains: &[tetrad_preset::Chain],
    scene: EngineScene,
    specs: Option<&Specs>,
) {
    for (row, lane) in layout.lanes.iter().enumerate() {
        for (column, block) in lane.slots.iter().enumerate() {
            let Some(block) = block else { continue };
            let cell = row * GRID_COLUMNS + column;
            snapshot.grid[cell] = GridCell {
                label: model_name(specs, block.model.0),
                bypassed: block.bypass.at(scene),
            };
            snapshot.blocks[cell] = BlockInfo {
                category: ModelCategory::of_model(block.model.0),
                model_id: Some(block.model.0),
                plugin: specs
                    .and_then(|specs| specs.model(block.model.0))
                    .and_then(|model| model.plugin_id),
            };
        }
        // tetrad-ui and tetrad-engine use the same stock `InputPortId`/`OutputPortId` values. A row
        // that an upper row feeds shows "Prev. Row" whatever it stores (`0x87fc2c`, tetrad-engine
        // `effective_input`): Factory 1A row 3 stores input 0 under row 1's "Row 3" output.
        let fed = |port| {
            layout.lanes[..2].iter().any(|upper| {
                [port, tetrad_engine::OutputPort::NextRow3And4].contains(&upper.output)
            })
        };
        let input = match row {
            2 if fed(tetrad_engine::OutputPort::NextRow3) => tetrad_engine::InputPort::PrevRow,
            3 if fed(tetrad_engine::OutputPort::NextRow4) => tetrad_engine::InputPort::PrevRow,
            _ => lane.input,
        };
        snapshot.sidechain.inputs[row] =
            (lane.input_control.as_ref()).is_some_and(|control| control.sidechain.source);
        snapshot.rows[row] = RowView {
            input: InputPort::ALL
                .into_iter()
                .find(|port| *port as u8 == input as u8)
                .unwrap_or_default(),
            output: OutputPort::ALL
                .into_iter()
                .find(|port| *port as u8 == lane.output as u8)
                .unwrap_or_default(),
            ..RowView::default()
        };
    }
    for pair in Pair::ALL {
        let routes = layout.pair(pair);
        snapshot.pairs[pair as usize] = PairView {
            split: routes.split.map(|column| column.index() as u8),
            mix: routes.merge.map(|column| column.index() as u8),
        };
        // Stock keeps the stored port under a junction and labels the faded port with it
        // (`0x59d50c`): Bass V2 row 4 stores "Multi Out" under its Mixer.
        let row = pair.slave().index();
        let Some(chain) = chains.get(row) else {
            continue;
        };
        let view = &mut snapshot.rows[row];
        if routes.split.is_some() {
            view.replaced_input = (InputPort::ALL.into_iter())
                .find(|port| chain.in_portid == Some(*port as u32))
                .unwrap_or_default();
        }
        if routes.merge.is_some() {
            view.replaced_output = (OutputPort::ALL.into_iter())
                .find(|port| chain.out_portid == Some(*port as u32))
                .unwrap_or_default();
        }
    }
}

/// Grid sidechain marks of the shown scene (`0x0052e53c`): a sink selects anything but Off;
/// a block a sink selects is a source.
fn project_sidechains(snapshot: &mut UiSnapshot, plan: &DeploymentPlan, scene: EngineScene) {
    let cell = |slot: Slot| slot.row.index() * GRID_COLUMNS + slot.column.index();
    for sidechain in &plan.sidechains {
        let source = sidechain.scenes[scene.index()].source;
        snapshot.sidechain.sinks[cell(sidechain.site)] = source != SidechainSource::Off;
        if let SidechainSource::Block(slot) = source {
            snapshot.sidechain.sources[cell(slot)] = true;
        }
    }
}

/// The `deviceType` of the capture each Neural Capture block selects in the shown scene (its
/// string value, as the media library loaded it).
/// Each capture block's device type and its name: the block (and its editor title, shot sn-7)
/// shows the capture name, not the model name.
fn project_captures(
    snapshot: &mut UiSnapshot,
    layout: &Layout,
    scene: EngineScene,
    devices: &BTreeMap<String, tetrad_ui::CaptureDevice>,
    media: &[MediaEntry],
) {
    use tetrad_dsp_host::setup::drivers::capture::FILE_NAME;
    for (row, lane) in layout.lanes.iter().enumerate() {
        for (column, block) in lane.slots.iter().enumerate() {
            let Some(block) = block else { continue };
            if ModelCategory::of_model(block.model.0) != Some(ModelCategory::NeuralCapture) {
                continue;
            }
            let cell = row * GRID_COLUMNS + column;
            snapshot.captures[cell] = (block.texts.iter())
                .filter_map(|text| text.scenes.get(scene.index())?.as_deref())
                .find_map(|value| devices.get(value).copied());
            let value = (block.texts.iter())
                .find(|text| text.index == FILE_NAME)
                .and_then(|text| text.scenes.get(scene.index())?.as_deref())
                .unwrap_or_default();
            if let Some(name) = capture_name(value, media) {
                snapshot.grid[cell].label = name;
            }
        }
    }
}

/// The name in a capture `file_name` (the key, then the name, `0x6f7018`): the text after the
/// 64-unit key, else the "My Captures" name of the key, else the stored text. `None` when
/// nothing is stored.
fn capture_name(value: &str, media: &[MediaEntry]) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let key = super::super::preset::capture_key(value);
    let name = &value[key.len()..];
    if !name.is_empty() {
        return Some(name.to_owned());
    }
    let entry = (media.iter())
        .find(|entry| entry.folder.kind() == MediaKind::Capture && entry.value == key);
    Some(entry.map_or_else(|| value.to_owned(), |entry| entry.name.clone()))
}

/// Stomp tiles from `stomp_mode_assignments` (one switch may toggle several blocks).
/// Labels: the single-stomp label, then the stomp label, then the first block's model name.
fn project_stomps(
    snapshot: &mut UiSnapshot,
    current: &super::super::Current,
    layout: &Layout,
    scene: EngineScene,
    specs: Option<&Specs>,
) {
    let preset = &current.preset;
    for (switch, view) in snapshot.stomps.iter_mut().enumerate() {
        let blocks: Vec<(Slot, &Instance)> = stomp_slots(preset, switch as u32)
            .filter_map(|slot| {
                Some((
                    slot,
                    layout.lane(slot.row).slots[slot.column.index()].as_ref()?,
                ))
            })
            .collect();
        let key = switch as u32;
        view.momentary = preset.stomp_is_momentary.get(key).copied().unwrap_or(false);
        let Some(&(slot, first)) = blocks.first() else {
            continue;
        };
        *view = StompView {
            label: preset
                .single_stomp_labels
                .get(key)
                .or_else(|| preset.stomp_labels.get(key))
                .cloned()
                .unwrap_or_else(|| model_name(specs, first.model.0)),
            engaged: !first.bypass.at(scene),
            category: ModelCategory::of_model(first.model.0),
            multiple: blocks.len() > 1,
            position: position(slot),
            momentary: view.momentary,
            blocks: blocks
                .iter()
                .filter_map(|(slot, _)| position(*slot))
                .collect(),
        };
    }
}

/// The engine site of a lane control or junction editor.
pub(super) fn engine_site(site: tetrad_ui::EditorSite) -> Site {
    use tetrad_ui::EditorSite as E;
    let row = |row: tetrad_ui::Row| Row::new(row.get()).expect("tetrad-ui rows are in range");
    let pair = |pair| match pair {
        tetrad_ui::RowPair::Upper => Pair::Upper,
        tetrad_ui::RowPair::Lower => Pair::Lower,
    };
    match site {
        E::InputGate(r) => Site::InputControl(row(r)),
        E::LaneOutput(r) => Site::OutputControl(row(r)),
        E::Splitter(p) => Site::Splitter(pair(p)),
        E::Mixer(p) => Site::Mixer(pair(p)),
    }
}

fn position(slot: Slot) -> Option<GridPosition> {
    GridPosition::new(slot.row.index() as u8, slot.column.index() as u8).ok()
}

/// Pattern Tremolo (7040) STEP n CONNECTED, n = 1..=15 (hidden parameters 27..=41), for the
/// step editor (`0x3a81a8`). All off for other models.
fn step_connected(block: &Instance, scene: EngineScene) -> [bool; 15] {
    const PATTERN_TREMOLO: u32 = 7040;
    const FIRST_LINK: usize = 27;
    std::array::from_fn(|link| {
        block.model.0 == PATTERN_TREMOLO
            && block
                .params
                .get(FIRST_LINK + link)
                .is_some_and(|flag| flag.at(scene) > 0.5)
    })
}

/// Whether a parameter shows on the editor and how its widget looks. Stock type 12
/// (FloatWithLed) is a knob that drags as a Float (mask `0x1013`); its LED is not drawn yet.
pub(crate) fn parameter_kind(scale: &ParameterScale, normalized: f32) -> Option<ParameterKind> {
    let position = if normalized.is_finite() {
        normalized.clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some(match scale.kind {
        ParameterType::Switch | ParameterType::ToggleButton => ParameterKind::Toggle {
            on: normalized >= 0.5,
        },
        ParameterType::RotarySwitch => ParameterKind::Choice {
            position,
            steps: u16::try_from(scale.steps()).unwrap_or(0),
        },
        ParameterType::ComboBox => ParameterKind::ComboBox { position },
        ParameterType::Float | ParameterType::Fader | ParameterType::FloatWithLed => {
            ParameterKind::Continuous { position }
        }
        ParameterType::Int => ParameterKind::Int { position },
        ParameterType::Meter
        | ParameterType::StereoMeter
        | ParameterType::GrMeter
        | ParameterType::StereoGrMeter
        | ParameterType::String
        | ParameterType::Empty => return None,
    })
}

/// Visible parameters of one block, value text from tetrad-core (`display_with_units`). Media
/// parameters show even when the catalog hides them (the stock IR/capture browser is not
/// recovered; I), with the picked item's name as value text.
pub(crate) fn parameters(
    layout: &Layout,
    position: GridPosition,
    scene: EngineScene,
    specs: &Specs,
    media: &[MediaEntry],
) -> Vec<ParameterView> {
    instance(layout, position)
        .map(|block| instance_parameters(block, scene, specs, media))
        .unwrap_or_default()
}

/// Visible parameters of any instance: a block, a lane control or a junction.
pub(crate) fn instance_parameters(
    block: &Instance,
    scene: EngineScene,
    specs: &Specs,
    media: &[MediaEntry],
) -> Vec<ParameterView> {
    let Some(spec) = specs.model(block.model.0) else {
        return Vec::new();
    };
    let scene_mode = |index: u32| block.scene_mode.get(index as usize) == Some(&true);
    spec.parameters
        .iter()
        .filter(|param| param.index <= MAX_PARAMETER_INDEX)
        .filter_map(|param| {
            if param.scale.kind == ParameterType::String
                && let Some(kind) = media_kind(spec, param.index)
            {
                return Some(ParameterView {
                    index: param.index as u16,
                    label: param.name.clone(),
                    value: media_value(spec, block, scene, param.index, kind, media),
                    kind: ParameterKind::Media(kind),
                    entry: None,
                    scene: scene_mode(param.index),
                });
            }
            if param.hidden && !ir_switch(spec, param.index) {
                return None;
            }
            let normalized = block.params.get(param.index as usize)?.at(scene);
            let kind = parameter_kind(&param.scale, normalized)?;
            let value = match kind {
                ParameterKind::Toggle { on } => if on { "On" } else { "Off" }.to_owned(),
                _ => param
                    .scale
                    .display_with_units(normalized)
                    .unwrap_or_else(|_| "-".into()),
            };
            Some(ParameterView {
                index: param.index as u16,
                label: param.name.clone(),
                value,
                kind,
                entry: numeric_range(&param.scale, normalized, kind),
                scene: scene_mode(param.index),
            })
        })
        .collect()
}

/// The numeric keyboard range of a continuous knob (`0x4c62d4`): the display value and the
/// display ends (`0x5cd110`). Integer entry when one step is at least 1 or the parameter
/// shows as an integer (the number formatter's rule `0x5ccdc0`, I for the dialog).
pub(crate) fn numeric_range(
    scale: &ParameterScale,
    normalized: f32,
    kind: ParameterKind,
) -> Option<tetrad_ui::NumericRange> {
    if scale.disabled
        || !matches!(
            kind,
            ParameterKind::Continuous { .. }
                | ParameterKind::Centred { .. }
                | ParameterKind::Int { .. }
        )
    {
        return None;
    }
    let (Ok(value), Ok(start), Ok(end)) = (
        scale.to_display(normalized),
        scale.to_display(0.0),
        scale.to_display(1.0),
    ) else {
        return None;
    };
    let per_step = (end - start).abs() / (scale.steps() - 1).max(1) as f32;
    Some(tetrad_ui::NumericRange {
        value,
        min: start.min(end),
        max: start.max(end),
        integer: scale.show_as_integer || per_step >= 1.0,
    })
}

/// The hidden MUTE and INVERT switches of an IR Loader channel in use (0, 1; 8, 9 on Dual):
/// the IR Loader editor shows them as its power and phase buttons (`IRLoaderButtonArea`
/// `0x7fca70`).
fn ir_switch(model: &ModelSpec, index: u32) -> bool {
    let base = model.clones.first().copied().unwrap_or(model.id);
    IrLoaderModel::from_model_id(base).is_some_and(|loader| match index {
        0 | 1 => true,
        8 | 9 => loader.ir_count() == 2,
        _ => false,
    })
}

/// IR id parameters (`0x515d58`; IR Loader slot B only on Dual models) and the capture
/// `file_name` of models 14000/14001. Clones use their base model.
fn media_kind(model: &ModelSpec, index: u32) -> Option<MediaKind> {
    use tetrad_dsp_host::setup::drivers::{capture::FILE_NAME, media::CAPTURE_V1_MODEL};
    let base = model.clones.first().copied().unwrap_or(model.id);
    let ir = reference_params(model.category_id, base).is_some_and(|params| {
        params.id.contains(&index)
            && IrLoaderModel::from_model_id(base)
                .is_none_or(|loader| loader.channel_for_param(index).is_some())
    });
    if ir {
        Some(MediaKind::Ir)
    } else if matches!(base, CAPTURE_V1_MODEL | 14001) && index == FILE_NAME {
        Some(MediaKind::Capture)
    } else {
        None
    }
}

/// The picker name of the stored text. Otherwise IRs use the stock label rule (`0x8221f8`:
/// stored name parameter, else "DELETED IR"), factory selectors their readable form and
/// captures the stored text; "-" when nothing is stored.
fn media_value(
    model: &ModelSpec,
    block: &Instance,
    scene: EngineScene,
    index: u32,
    kind: MediaKind,
    media: &[MediaEntry],
) -> String {
    let text = |index: u32| {
        block
            .texts
            .iter()
            .find(|param| param.index == index)
            .and_then(|param| {
                param
                    .scenes
                    .get(scene.index())
                    .or(param.scenes.first())
                    .cloned()
                    .flatten()
            })
            .unwrap_or_default()
    };
    let value = text(index);
    if let Some(entry) = media
        .iter()
        .find(|entry| entry.folder.kind() == kind && entry.value == value)
    {
        return entry.name.clone();
    }
    if value.is_empty() {
        return "-".into();
    }
    if kind == MediaKind::Capture {
        return capture_name(&value, media).unwrap_or(value);
    }
    let base = model.clones.first().copied().unwrap_or(model.id);
    match IrReference::classify(&value) {
        IrReference::Factory { .. } => value.replace('_', " "),
        IrReference::Custom(_) => {
            let stored = reference_params(model.category_id, base)
                .and_then(|params| {
                    let slot = params.id.iter().position(|id| *id == index)?;
                    Some(text(params.name[slot]))
                })
                .unwrap_or_default();
            display_name(None, &stored).to_owned()
        }
        IrReference::Empty | IrReference::Unexpected(_) => value,
    }
}

/// Picker list: visible catalog models of the picker categories.
pub(super) fn models(specs: &Specs) -> Vec<ModelEntry> {
    specs
        .models
        .values()
        .filter(|model| !model.hidden && !model.internal)
        .filter_map(|model| {
            let category = ModelCategory::of_model(model.id)?;
            ModelCategory::PICKER
                .contains(&category)
                .then(|| ModelEntry {
                    id: model.id,
                    name: model.name.clone(),
                    category,
                    extension: model.community.is_some(),
                })
        })
        .take(MAX_LIST_ITEMS)
        .collect()
}

/// `LooperStatus` → Looper X view. Cached-state families are confirmed
/// (tuner-looper-dsp-contracts §5, `w4-looper-gates` §4.2).
/// Tile actions in footswitch order from the stored button numbers (same table as the
/// footswitch path in `input.rs`; 3 and unknown values are Duplicate).
fn looper_tiles(state: &State) -> tetrad_ui::LooperTiles {
    use tetrad_ui::LooperAction as A;
    let stored = state.settings.settings().looper.stomp_assignments();
    tetrad_ui::LooperTiles(stored.map(|button| match button {
        0 => A::Record,
        1 => A::PlayStop,
        2 => A::UndoRedo,
        4 => A::OneShot,
        5 => A::HalfSpeed,
        6 => A::PunchIn,
        7 => A::Reverse,
        _ => A::Duplicate,
    }))
}

fn looper_view(status: &LooperStatus, cached_state: u32) -> LooperView {
    let unit = |bits: u32| {
        let value = f32::from_bits(bits);
        if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    let state = match cached_state {
        _ if status.armed != 0 => LooperState::Armed,
        1 if status.loop_length > 0 => LooperState::Stopped,
        2 | 3 => LooperState::Playing,
        4 | 5 => LooperState::Recording,
        6 => LooperState::Overdubbing,
        8 | 9 => LooperState::Duplicating,
        10 | 11 => LooperState::Punching,
        _ => LooperState::Empty,
    };
    LooperView {
        state,
        waiting: if status.waiting_for_cycle != 0 {
            Some(LooperWait::CycleEnd)
        } else if status.events_waiting_for_quantize != 0 {
            Some(LooperWait::Quantize)
        } else {
            None
        },
        progress: unit(status.progress_bits),
        loop_seconds: (status.loop_length > 0).then(|| status.loop_length as f32 / SAMPLE_RATE),
        undo_available: status.undo_count > 0,
        redo_available: status.redo_available != 0,
        reverse: status.in_reverse != 0,
        half_speed: status.half_speed != 0,
        one_shot: status.one_shot != 0,
        undo_in_progress: f32::from_bits(status.undo_progress_bits) > 0.0,
        available_seconds: Some(status.free_samples as f32 / SAMPLE_RATE),
        ..LooperView::default()
    }
}

fn beats(state: &State) -> u32 {
    state
        .metronome
        .as_ref()
        .and_then(|controller| controller.driver_value(crate::tempo::TempoParam::TimeSignature))
        .and_then(|index| crate::tempo::TimeSignature::from_index(index as i32))
        .map_or(4, |signature| u32::from(signature.beats))
}

/// Beat marks from tempo parameters 10..=22 (`0x0037e89c`).
fn accents(state: &State) -> [tetrad_ui::BeatAccent; tetrad_ui::BeatAccent::BEATS] {
    std::array::from_fn(|beat| {
        state
            .metronome
            .as_ref()
            .zip(crate::tempo::Beat::new(beat as u8))
            .and_then(|(controller, beat)| {
                controller.driver_value(crate::tempo::TempoParam::Accent(beat))
            })
            .map_or_else(
                tetrad_ui::BeatAccent::default,
                tetrad_ui::BeatAccent::from_value,
            )
    })
}

fn tuner_view(state: &State) -> tetrad_ui::TunerView {
    let hz = state.tuner_hz;
    if hz <= 0.0 || !hz.is_finite() {
        return tetrad_ui::TunerView::default();
    }
    // I: twelve-tone equal temperament around the stored A4 reference.
    let pitch = 69.0 + 12.0 * (hz / state.settings.settings().tuner.reference_hz()).log2();
    let note = pitch.round() as i32;
    tetrad_ui::TunerView {
        note: Some(tetrad_ui::Note {
            pitch_class: note.rem_euclid(12) as u8,
            octave: (note.div_euclid(12) - 1).clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8,
        }),
        cents: (pitch - pitch.round()) * 100.0,
        frequency_hz: Some(hz),
    }
}

fn project_tempo_settings(state: &State, map: &mut BTreeMap<SettingId, SettingValue>) {
    use SettingValue::{Bool, Choice, Number};
    for (id, index) in [
        (SettingId::TempoMode, 1),
        (SettingId::TempoLed, 2),
        (SettingId::MetronomeLevel, 3),
        (SettingId::Metronome, 4),
        (SettingId::TimeSignature, 6),
        (SettingId::Subdivision, 7),
        (SettingId::MetronomeRouting, 9),
        (SettingId::MetronomePan, 5),
        (SettingId::MetronomeSound, 8),
    ] {
        let Some(normalized) = super::super::features::scoped_value(state, index) else {
            continue;
        };
        let real = if index == 1 {
            normalized
        } else {
            let Some(spec) = state
                .specs
                .as_ref()
                .and_then(|specs| specs.models.get(&25_000))
                .and_then(|model| model.parameter(index as u32))
            else {
                continue;
            };
            let Some(range) = spec.range else {
                continue;
            };
            crate::tempo::ParamRange {
                min: range.min,
                max: range.max,
                skew: spec.skew.unwrap_or(1.0),
            }
            .denormalize(normalized)
        };
        map.insert(
            id,
            match id {
                SettingId::TempoMode => Choice(u16::from(real.round() == 0.0)),
                SettingId::TempoLed | SettingId::Metronome => Bool(real.round() != 0.0),
                SettingId::MetronomeLevel => Number((real * 10.0).round() as i32),
                SettingId::MetronomePan => Number((real * 100.0).round() as i32),
                _ => Choice(real.round() as u16),
            },
        );
    }
    if let Some(note) = super::super::features::bpm_note(state) {
        map.insert(SettingId::BpmNote, Choice(note));
    }
}

/// Settings the device stores, in tetrad-ui terms. I/O connector and expression rows
/// without a recovered mapping remain absent.
pub(super) fn settings_map(settings: &Settings) -> BTreeMap<SettingId, SettingValue> {
    use SettingValue::{Bool, Choice, Number};
    let general = &settings.general;
    let display = &settings.display;
    let midi = &settings.midi;
    let mut map = BTreeMap::from([
        (
            SettingId::ScreenBrightness,
            Number(display.brightness.get()),
        ),
        (
            SettingId::LedBrightness,
            Number(display.led_brightness.get()),
        ),
        (
            SettingId::DimmedLedBrightness,
            Number(display.dimmed_led_brightness.get()),
        ),
        (SettingId::PresetDimmed, Bool(display.dimmed_preset)),
        (SettingId::SceneDimmed, Bool(display.dimmed_scene)),
        (SettingId::StompDimmed, Bool(display.dimmed_stomp)),
        (
            SettingId::PowerOnMode,
            Choice(u16::from(general.atma_power_on_automatic)),
        ),
        (
            SettingId::MasterVolumeOut12,
            Bool(settings.master_volume.out12),
        ),
        (
            SettingId::MasterVolumeOut34,
            Bool(settings.master_volume.out34),
        ),
        (
            SettingId::MasterVolumeSend12,
            Bool(settings.master_volume.send12),
        ),
        (
            SettingId::MasterVolumeHeadphones,
            Bool(settings.master_volume.headphones),
        ),
        (
            SettingId::InternetCheck,
            Bool(!general.disable_connection_check),
        ),
        (
            SettingId::CloudDomain,
            Choice(match settings.cloud_domain {
                tetrad_settings::permanent::CloudDomain::Default => 0,
                tetrad_settings::permanent::CloudDomain::Alternative => 1,
            }),
        ),
        (
            SettingId::SceneBypassBehavior,
            Choice(match general.bypass_save_mode {
                BypassSaveMode::Always => 0,
                BypassSaveMode::NonStomp => 1,
                BypassSaveMode::Never => 2,
            }),
        ),
        (
            SettingId::StompModeAutoAssign,
            Bool(general.stomp_mode_auto_assign),
        ),
        (
            SettingId::HoldTiming,
            Choice(general.hold_timing.get() as u16),
        ),
        (
            SettingId::SwapTempoTuner,
            Bool(general.swap_tempo_and_tuner_access),
        ),
        (
            SettingId::GigViewAccess,
            Bool(general.gig_view_stomp_access),
        ),
        (
            SettingId::LatencyCompensation,
            Bool(general.dynamic_delay_compensation),
        ),
        (SettingId::ReloadPreset, Bool(general.preset_reload)),
        (SettingId::ReloadScene, Bool(general.scene_reload)),
        // tetrad-ui index = stored channel − 1: 1..16, then OMNI (stored 17, `0x00416674`).
        (
            SettingId::MidiChannel,
            Choice(if midi.channel.is_omni() {
                16
            } else {
                midi.channel.get() as u16 - 1
            }),
        ),
        (SettingId::MidiOverUsb, Bool(midi.over_usb)),
        (
            SettingId::IgnoreDuplicatePc,
            Bool(midi.ignore_duplicate_pcs),
        ),
        (
            SettingId::MidiClockOut,
            Choice(midi.clock_out.to_stock() as u16),
        ),
        (SettingId::MidiClockIn, Bool(midi.clock_in)),
        (SettingId::MidiThru, Bool(midi.thru)),
        (
            SettingId::TunerReference,
            Number(settings.tuner.reference_hz().round() as i32),
        ),
        (SettingId::TunerMute, Bool(settings.tuner.mutes_outputs)),
        (SettingId::LiveTuner, Bool(settings.tuner.live_tuner)),
    ]);
    if let Some(index) = tetrad_settings::tuner::InputPortId::TUNER_CHOICES
        .iter()
        .position(|port| *port == settings.tuner.input)
    {
        map.insert(SettingId::TunerInput, Choice(index as u16));
    }
    // tetrad-ui index = stored `powerButtonSensitivity` 0..=3 (Off, Low, Medium, High).
    map.insert(
        SettingId::PowerButtonSensitivity,
        Choice(general.power_button_sensitivity.to_stock() as u16),
    );
    for row in 0..4u8 {
        let Ok(view_row) = tetrad_ui::Row::new(row) else {
            continue;
        };
        let row = usize::from(row);
        map.insert(
            SettingId::GlobalBypassCab(view_row),
            Bool(settings.global_bypass.cab[row]),
        );
        map.insert(
            SettingId::GlobalBypassIr(view_row),
            Bool(settings.global_bypass.ir[row]),
        );
    }
    map
}

/// The I/O page rows from the live connector values (`State::io`, w4-iosettings §1.1):
/// levels as tenths of dB by the stock law, choices and toggles as option indices. The
/// pair rows read the pair ids when linked, else OUT 1 / OUT 3 (tetrad-ui keeps one row set
/// per pair, I). A parameter the hardware lacks has no row.
fn io_settings(
    io: &super::super::jacks::Connectors,
    settings: &Settings,
) -> Vec<(SettingId, SettingValue)> {
    use tetrad_settings::io::{ConnectorId as C, ConnectorKind};
    use tetrad_ui::{InputJack, LoopJack, OutputJack};
    let paired12 = settings.io.out12_paired;
    let paired34 = settings.io.out34_paired;
    let pick = |paired: bool, pair: C, mono: C| if paired { pair } else { mono };
    let rows = [
        (SettingId::InputLevel(InputJack::In1), C::In1Level),
        (SettingId::InputImpedance(InputJack::In1), C::In1Impedance),
        (SettingId::InputType(InputJack::In1), C::In1Type),
        (SettingId::Phantom(InputJack::In1), C::In1Phantom),
        (SettingId::InputGroundLift(InputJack::In1), C::In1GroundLift),
        (SettingId::InputLevel(InputJack::In2), C::In2Level),
        (SettingId::InputImpedance(InputJack::In2), C::In2Impedance),
        (SettingId::InputType(InputJack::In2), C::In2Type),
        (SettingId::Phantom(InputJack::In2), C::In2Phantom),
        (SettingId::InputGroundLift(InputJack::In2), C::In2GroundLift),
        (
            SettingId::OutputLevel(OutputJack::Out12),
            pick(paired12, C::Out12Level, C::Out1Level),
        ),
        (
            SettingId::OutputGroundLift(OutputJack::Out12),
            pick(paired12, C::Out12GroundLift, C::Out1GroundLift),
        ),
        (
            SettingId::OutputMute(OutputJack::Out12),
            pick(paired12, C::Out12Mute, C::Out1Mute),
        ),
        (
            SettingId::OutputLevel(OutputJack::Out34),
            pick(paired34, C::Out34Level, C::Out3Level),
        ),
        (
            SettingId::OutputMute(OutputJack::Out34),
            pick(paired34, C::Out34Mute, C::Out3Mute),
        ),
        (SettingId::SendLevel(LoopJack::One), C::Send1Level),
        (SettingId::SendLevel(LoopJack::Two), C::Send2Level),
        (SettingId::ReturnLevel(LoopJack::One), C::Ret1Level),
        (
            SettingId::ReturnGroundLift(LoopJack::One),
            C::Ret1GroundLift,
        ),
        (SettingId::ReturnLevel(LoopJack::Two), C::Ret2Level),
        (
            SettingId::ReturnGroundLift(LoopJack::Two),
            C::Ret2GroundLift,
        ),
        (SettingId::HeadphoneLevel, C::HpLevel),
        (SettingId::HeadphoneSource, C::HpSource),
        (SettingId::UsbLevel, C::UsbLevel),
        (SettingId::UsbRouting, C::UsbDryWet),
    ];
    let feeds = tetrad_ui::HpFeed::ALL.map(|feed| {
        (
            SettingId::HeadphoneFeed(feed),
            super::super::actions::hp_feed_connector(feed),
        )
    });
    let mut out: Vec<(SettingId, SettingValue)> = rows
        .into_iter()
        .chain(feeds)
        .filter_map(|(row, id)| {
            let spec = id.spec(io.hardware())?;
            let value = io.value(id);
            let shown = match (spec.kind, row.kind()) {
                (ConnectorKind::Level { .. }, _) => {
                    SettingValue::Number((spec.decibels(value)? * 10.0).round() as i32)
                }
                (_, tetrad_ui::SettingKind::Toggle) => SettingValue::Bool(spec.index(value)? == 1),
                _ => SettingValue::Choice(u16::try_from(spec.index(value)?).ok()?),
            };
            Some((row, shown))
        })
        .collect();
    out.push((
        SettingId::OutputPair(OutputJack::Out12),
        SettingValue::Bool(paired12),
    ));
    out.push((
        SettingId::OutputPair(OutputJack::Out34),
        SettingValue::Bool(paired34),
    ));
    out
}

/// The loaded preset's Preset MIDI Out tables (`ProtoBufPreset::doLoad` rules through
/// tetrad-midi, legacy field 17 included). Empty and unknown slots are left out; a slot
/// with an out-of-range channel or value is skipped (tetrad-ui refuses it).
pub(in crate::device) fn preset_midi(preset: &tetrad_preset::Preset) -> tetrad_ui::PresetMidiView {
    use tetrad_midi::{MidiMessageInfo, MidiOutKind, PresetMidiFields, PresetMidiOut};
    use tetrad_ui::{MidiMessageKind, MidiOutMessage};
    let infos = |messages: &[tetrad_preset::MidiMessage]| -> Vec<MidiMessageInfo> {
        messages
            .iter()
            .map(|message| MidiMessageInfo {
                message_type: message.kind,
                channel: message.channel,
                param1: message.param1,
                param2: message.param2,
                param3: message.param3,
            })
            .collect()
    };
    let table = PresetMidiOut::from_fields(&PresetMidiFields {
        on_load: infos(&preset.midi_messages),
        general: infos(&preset.midi_messages_general),
        general_v2: infos(&preset.midi_messages_general_v2),
    });
    let list = |slots: &[tetrad_midi::MidiOutSlot]| -> Vec<MidiOutMessage> {
        slots
            .iter()
            .filter_map(|slot| {
                let kind = match slot.kind {
                    MidiOutKind::ControlChange => MidiMessageKind::Cc,
                    MidiOutKind::ControlToggle => MidiMessageKind::CcToggle,
                    MidiOutKind::ProgramChange => MidiMessageKind::Pc,
                    _ => return None,
                };
                let part = |raw: u32| match raw {
                    u32::MAX => Ok(None),
                    0..=127 => Ok(Some(raw as u8)),
                    _ => Err(()),
                };
                let values = [
                    part(slot.param1).ok()?,
                    part(slot.param2).ok()?,
                    match kind {
                        MidiMessageKind::Cc => None,
                        _ => part(slot.param3).ok()?,
                    },
                ];
                let channel = u8::try_from(slot.channel)
                    .ok()
                    .filter(|c| (1..=16).contains(c))?;
                Some(MidiOutMessage {
                    kind,
                    channel,
                    values,
                })
            })
            .collect()
    };
    tetrad_ui::PresetMidiView {
        on_load: list(&table.on_load),
        footswitches: std::array::from_fn(|index| list(&table.sources[index])),
        expression: [list(&table.sources[8]), list(&table.sources[9])],
        stored: (table.on_load.iter())
            .chain(table.sources.iter().flatten())
            .any(|slot| slot.kind != MidiOutKind::None),
    }
}

/// The saving destination folder of each media library (settings
/// `downloadDestinationFolderIR` / `NC`, else the user root; `0x76be70`).
pub(super) fn saving_destinations(
    db: &tetrad_storage::stock::StockDb,
    settings: &Settings,
) -> Vec<i64> {
    use tetrad_storage::stock::ProductType;
    let library = &settings.library;
    [
        (ProductType::Ir, &library.download_destination_ir),
        (ProductType::Capture, &library.download_destination_nc),
    ]
    .into_iter()
    .filter_map(|(kind, key)| {
        tetrad_library::stock::saving_destination(&db.view(), kind, key.as_deref()).ok()
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Neural Capture block shows the type of the capture its string value names in the
    /// shown scene (stock model word `[0x38]`); other blocks and unknown captures show none.
    #[test]
    fn capture_blocks_take_the_type_of_the_capture_they_select() {
        let capture = |values: [&str; 2]| {
            let mut block = Instance::new(tetrad_engine::ModelId(14_001), Vec::new());
            block.texts.push(tetrad_engine::TextParam {
                index: 5,
                scenes: values
                    .iter()
                    .map(|value| Some((*value).to_owned()))
                    .collect(),
            });
            Some(block)
        };
        let mut layout = Layout::default();
        layout.lanes[0].slots[2] = capture(["k1", "k2"]);
        layout.lanes[2].slots[0] = capture(["k3", "k3"]);
        let mut light = Instance::new(tetrad_engine::ModelId(1), Vec::new());
        light.texts = layout.lanes[0].slots[2].as_ref().unwrap().texts.clone();
        layout.lanes[1].slots[0] = Some(light);
        let devices = BTreeMap::from([
            ("k1".to_owned(), tetrad_ui::CaptureDevice::Pedal),
            ("k2".to_owned(), tetrad_ui::CaptureDevice::Cab),
        ]);
        let typed = |scene: u8| {
            let mut snapshot = UiSnapshot::default();
            let scene = EngineScene::new(scene).unwrap();
            project_captures(&mut snapshot, &layout, scene, &devices, &[]);
            (snapshot.captures.iter().enumerate())
                .filter_map(|(cell, device)| device.map(|device| (cell, device)))
                .collect::<Vec<_>>()
        };
        assert_eq!(typed(0), [(2, tetrad_ui::CaptureDevice::Pedal)]);
        assert_eq!(typed(1), [(2, tetrad_ui::CaptureDevice::Cab)]);
    }

    /// Shot sn-7: a capture block shows the capture name, the text after the 64-unit key
    /// (`0x6f7018` stores key + name); a key alone shows the "My Captures" name of the key.
    #[test]
    fn capture_blocks_show_the_capture_name() {
        let key = "a".repeat(64);
        let mut layout = Layout::default();
        for (column, value) in [(0, format!("{key}4-Comp Custom 1")), (1, "k1".to_owned())] {
            let mut block = Instance::new(tetrad_engine::ModelId(14_001), Vec::new());
            block.texts.push(tetrad_engine::TextParam {
                index: 5,
                scenes: vec![Some(value); 8],
            });
            layout.lanes[0].slots[column] = Some(block);
        }
        let media = [MediaEntry {
            folder: tetrad_ui::MediaFolder::MyCaptures,
            name: "Plexi".into(),
            value: "k1".into(),
        }];
        let mut snapshot = UiSnapshot::default();
        let scene = EngineScene::new(0).unwrap();
        project_captures(&mut snapshot, &layout, scene, &BTreeMap::new(), &media);
        assert_eq!(snapshot.grid[0].label, "4-Comp Custom 1");
        assert_eq!(snapshot.grid[1].label, "Plexi");
    }

    /// Block knobs show the stored catalog proportion and the value through the catalog law
    /// (`value = min + u^(1/skew)·(max − min)`): an amp OUTPUT −60…+12 dB with skew 3.8018
    /// reads 0.0 dB at 12 o'clock (`g3.png`) and −20.0 dB at about 0.107 of the sweep.
    #[test]
    fn block_knobs_draw_the_catalog_proportion() {
        const MODELS: &str = "<Models><Category id='1' name='Amp'><Model id='1001' name='Amp' \
blob='amp'><Parameter name='OUTPUT' type='float' min='-60' max='12' skew='3.8018' \
defaultValue='0' units='dB'/></Model></Category></Models>";
        let specs =
            Specs::resolve(&tetrad_catalog::Catalog::parse(MODELS, "<ModuleReport/>").unwrap())
                .unwrap();
        let knob = |stored: f32| {
            let block = Instance::new(
                tetrad_engine::ModelId(1001),
                vec![tetrad_engine::Scened::Fixed(stored)],
            );
            let view =
                instance_parameters(&block, EngineScene::new(0).unwrap(), &specs, &[]).remove(0);
            (view.kind, view.value)
        };
        assert_eq!(
            knob(0.5),
            (
                ParameterKind::Continuous { position: 0.5 },
                "0.0 dB".to_owned()
            )
        );
        let u = (40.0_f32 / 72.0).powf(3.8018);
        assert_eq!(
            knob(u),
            (
                ParameterKind::Continuous { position: u },
                "-20.0 dB".to_owned()
            )
        );
    }

    /// The kind keeps the stock types apart that drag differently (mask `0x1013`): a rotary
    /// switch carries its catalog step count (one step per 100 · step px, `0x5cc994`); Int and
    /// ComboBox do not drag; FloatWithLed (type 12) shows and drags as a Float.
    #[test]
    fn parameter_kinds_keep_the_stock_drag_types_and_step_count() {
        const MODELS: &str = "<Models><Category id='1' name='Amp'><Model id='1001' name='Amp' \
blob='amp'><Parameter name='MODE' type='rotarySwitch' min='0' max='2' steps='3' \
defaultValue='0'/><Parameter name='VOICES' type='int' min='1' max='4' steps='4' \
defaultValue='1'/><Parameter name='SHAPE' type='comboBox' min='0' max='2' steps='3' \
defaultValue='0'/><Parameter name='LEVEL' type='floatWithLed' min='0' max='10' \
defaultValue='5'/></Model></Category></Models>";
        let specs =
            Specs::resolve(&tetrad_catalog::Catalog::parse(MODELS, "<ModuleReport/>").unwrap())
                .unwrap();
        let block = Instance::new(
            tetrad_engine::ModelId(1001),
            vec![tetrad_engine::Scened::Fixed(0.5); 4],
        );
        let view = instance_parameters(&block, EngineScene::new(0).unwrap(), &specs, &[]);
        let kinds: Vec<_> = view.iter().map(|view| view.kind).collect();
        assert_eq!(
            kinds,
            [
                ParameterKind::Choice {
                    position: 0.5,
                    steps: 3
                },
                ParameterKind::Int { position: 0.5 },
                ParameterKind::ComboBox { position: 0.5 },
                ParameterKind::Continuous { position: 0.5 },
            ]
        );
        assert!(
            view[1].entry.is_some(),
            "an Int knob keeps the numeric keyboard"
        );
    }

    /// Stock `0x0032da4c`: the mode button's MIDI glyph follows any stored message type, a
    /// kind the MIDI Out page does not list (Clock, unknown) included; legacy field 17 stores
    /// no type, so it does not count.
    #[test]
    fn preset_midi_out_is_stored_when_any_slot_has_a_message_type() {
        let message = |kind| tetrad_preset::MidiMessage {
            kind,
            channel: 1,
            ..Default::default()
        };
        let mut preset = tetrad_preset::Preset::default();
        assert!(!preset_midi(&preset).stored);
        preset.midi_messages_general = vec![message(1)];
        assert!(!preset_midi(&preset).stored, "legacy field 17");
        preset.midi_messages_general.clear();
        preset.midi_messages_general_v2 = vec![message(0); 30];
        preset.midi_messages_general_v2[29] = message(4);
        let view = preset_midi(&preset);
        assert!(
            view.stored && view.footswitches[2].is_empty(),
            "a Clock slot"
        );
        preset.midi_messages_general_v2.clear();
        preset.midi_messages = vec![message(3)];
        assert!(preset_midi(&preset).stored, "on load");
    }

    /// Stock "1G Bass V2" (`st-mp1g.png`): row 4 mixes into row 3 but stores "Multi Out". The
    /// import drops that port from the layout; the row view keeps it for the faded label. A
    /// stored port without a junction (row 2 here) replaces nothing.
    #[test]
    fn path_b_keeps_the_stored_port_its_junction_replaces() {
        let mut layout = Layout::default();
        layout.pair_mut(Pair::Lower).merge = Some(tetrad_engine::Column::new(6).unwrap());
        let chain = |input, output| tetrad_preset::Chain {
            in_portid: Some(input),
            out_portid: Some(output),
            ..tetrad_preset::Chain::default()
        };
        let chains = [chain(1, 19), chain(1, 19), chain(0, 19), chain(0, 19)];
        let mut snapshot = UiSnapshot::default();
        project_grid(
            &mut snapshot,
            &layout,
            &chains,
            EngineScene::new(0).unwrap(),
            None,
        );
        let row = |row: usize| snapshot.rows[row];
        assert_eq!(row(3).output, OutputPort::Empty, "the mix routes row 4");
        assert_eq!(row(3).replaced_output, OutputPort::MultipleOuts);
        assert_eq!(row(3).replaced_input, InputPort::Empty, "no split");
        assert_eq!(row(1).replaced_output, OutputPort::Empty, "no upper mix");
    }
}

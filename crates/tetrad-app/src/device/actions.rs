//! The shared device-action layer (2C design).
//!
//! Input, MIDI, the UI and Cortex Control express every change of shared device state as an
//! [`Action`]. [`reduce`] is the one place that applies an action to [`State`]; it returns the
//! [`Effect`]s the device still runs (DSP pushes, preset loads, screens, settings flushes).
//! Bridges keep only their private state (hold timers, chords, the tetrad-ui view, link state);
//! the few direct `State` writes they make are listed on [`State`].

use std::collections::BTreeMap;
use std::time::Duration;

use tetrad_core::performance::bypass::{AssignedBlock, momentary_release, stomp_press};
use tetrad_core::performance::expression::Pedal as CorePedal;
use tetrad_core::performance::footswitch::BankOverlay;
use tetrad_core::performance::stomp::{NON_MOMENTARY_MODEL, effective_momentary};
use tetrad_core::performance::{
    BypassOverwriteMode, ChangeSource, FootswitchMode as CoreMode, StompIndex, StompWrite,
};
use tetrad_core::workflow::{BlockAddress, ModeId};
use tetrad_engine::{Column, InputPort, Instance, OutputPort, Pair, Row, SceneIndex, Site, Slot};
use tetrad_hal::node::{OverloadChannel, Pedal};
use tetrad_library::stock::{ItemList, ItemRef, StockLibrary};
use tetrad_midi::LooperButton;
use tetrad_settings::controls::{AtmaVolume, FootswitchMode as StoredMode};
use tetrad_settings::device::{DeviceSettings, ResetKind, Settings, SettingsEvent};
use tetrad_settings::general::{
    BypassSaveMode, HoldTiming, LedBrightness, PowerButtonSensitivity, ScreenBrightness,
};
use tetrad_settings::io::ConnectorId;
use tetrad_settings::midi::{MidiChannel, MidiClockOut};
use tetrad_storage::media::Collection;
use tetrad_ui::{Screen, SettingId, SettingValue};

use super::features::refresh_expression;
use super::jacks::IoPresetLoad;
use super::preset::{self, PresetError, SlotOperation, SlotState, UndoFrame};
use super::services::PresetFile;
use super::ui::UiInput;
use super::{Current, DspCommand, Effect, PresetRef, State, Subsystem, Timer};

/// Stock tempo range (`TempoSettings::BPM_RANGE`, MIDI CC#44 clamp).
const TEMPO_BPM: (f32, f32) = (40.0, 240.0);
/// `usleep(10000)` after the first pass of a stomp press (`0x5239d0`).
const STOMP_SETTLE: Duration = Duration::from_millis(10);
/// The Neural Capture category (models 14000, 14001).
const CAPTURE_CATEGORY: u32 = 14;

/// One change of shared device state.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Directory(tetrad_ui::DirectoryRequest),
    Time(Duration),
    /// Load a library slot. `reload: false` skips the slot that is already loaded (MIDI
    /// `ignore_if_current`); footswitches, the UI and Cortex Control always load. A setlist
    /// name becomes its directory path first (the stock `LastPreset/setlist` form).
    LoadPreset {
        target: PresetRef,
        reload: bool,
    },
    /// A MIDI program change load (`0x5b5bd8` case 3): as [`Self::LoadPreset`], but stock calls
    /// `0x5b2f40` directly, with no load frame (review r39 K13, C).
    ProgramChange {
        target: PresetRef,
        reload: bool,
    },
    /// Non-forced scene selection (`0x5899d8`): the active scene does nothing. The DSP gets
    /// the stock recall (`0x527248`) with the metronome resend after its bypass pass.
    SelectScene(SceneIndex),
    /// A normalized value with the stock scene rule (`0x51e4d8`), in the deployment and the
    /// stored preset.
    SetParameter {
        site: Site,
        index: u32,
        value: f32,
    },
    SetText {
        site: Site,
        index: u32,
        text: String,
    },
    /// Block bypass at the current scene (`0x521514`), a touch-source write under Scene Bypass
    /// Behaviour ([`preset::BypassCommits`]).
    SetBypass {
        site: Site,
        bypassed: bool,
    },
    /// (O) [`Self::SetBypass`] as a momentary edge (source 3, as a stock momentary stomp): a
    /// Custom mode momentary slot. Under Scene Bypass Behaviour the committed scene state stays.
    MomentaryBypass {
        site: Site,
        bypassed: bool,
    },
    /// Stomp press (`0x5239d0`, footswitch source; also a UI stomp tile): engaged blocks of
    /// categories {0,1,2,3,5,14} switch off first, 10 ms settle, then each other block
    /// toggles its own bypass, or its secondary stomp parameter when assigned SECONDARY
    /// (`tetrad_core::performance::bypass::stomp_press`). Momentary stomps write with source 3.
    ToggleStomp(StompIndex),
    /// A physical stomp release: a momentary stomp toggles each block back (`0x523f80`; not in
    /// scene copy mode).
    ReleaseStomp(StompIndex),
    /// Lock Touchscreen and Master Volume (Power & Locking). While locked the QC knob never
    /// sets the level and picks it up after the unlock (`0x1fb86c` forces owner 2); Cortex
    /// Control learns the state (`0x16ad60`).
    SetLocked(bool),
    /// Footswitch scene copy mode (ModeController `+0x14`, read by `0x20eec4`): `Some` enters
    /// it with its clipboard, `None` leaves it.
    SceneCopyMode(Option<SceneClipboard>),
    /// An accelerated BANK UP encoder turn (QC logical 10). tetrad-ui gives it its meaning:
    /// device cycling in the block editor ([`Action::CycleModel`]), VD Preset audition in
    /// their list (manual §4).
    BankEncoder(i32),
    /// BANK UP device cycling: the block at `slot` becomes the next (`step` > 0) or previous
    /// model in its category ([`preset::cycled_model`]), as a model change. A Neural Capture
    /// block steps through [`State::capture_cycle`] instead (`0x4f50dc`).
    CycleModel {
        slot: Slot,
        step: i32,
    },
    /// The Directory capture order for BANK UP capture cycling, view state from the UI bridge
    /// (as [`Action::ScreenShown`]); kept in [`State::capture_cycle`].
    CaptureCycle(CaptureCycle),
    /// The block editor opened on a grid block (`Some`) or closed (`None`); view state from
    /// the UI bridge, kept in [`State::editor_block`]. The editor meters poll while it is set
    /// (`Timer::EditorMeters`, Parallax `0x636740`).
    EditorBlock(Option<Slot>),
    /// Structural grid edit; recompiles with core reuse.
    EditLayout(LayoutEdit),
    /// Several structural edits in one compile (`preset::prepare_layout_edits`): a Cortex
    /// Control row's two ports or a pair's split and mix points.
    EditLayouts(Vec<LayoutEdit>),
    SceneLabel {
        scene: SceneIndex,
        label: String,
    },
    SceneColor {
        scene: SceneIndex,
        raw_color: u32,
    },
    /// Stomp footswitch label (`single_stomp_labels`; the map stock writes is inferred).
    StompLabel {
        stomp: u32,
        label: String,
    },
    /// One tetrad-ui setting.
    ChangeSetting {
        id: SettingId,
        value: SettingValue,
    },
    SetMode(StoredMode),
    /// The allowed footswitch modes (`0x58ce2c`): a new list also sets its first mode.
    SetAllowedModes(Vec<StoredMode>),
    /// The next allowed mode, wrapping. Inferred order: the stored list.
    CycleMode,
    /// MIDI CC#47 `setModeWithIndex(slot)`: entry `slot` of the allowed list (inferred).
    ModeSlot(u8),
    /// (O) The Custom footswitch mode of an extension on or off ([`super::custom`]).
    SetCustomMode(bool),
    /// (O) A page change of the Custom mode, while it is on.
    CustomPage(tetrad_ui::PageStep),
    /// (O) A press or release of slot 0..=9 of the Custom mode's current page.
    CustomSlot {
        slot: u8,
        pressed: bool,
        origin: super::custom::Origin,
    },
    /// (O) One action that a Custom mode press or release fired: the press of `gesture`, or
    /// (`None`) the release of what the switch holds. Each runs after the effects of the one
    /// before. `epoch` is the gestures' epoch when it fired (`custom::fired`).
    CustomFire {
        slot: u8,
        gesture: Option<tetrad_ui::Gesture>,
        epoch: u32,
        origin: super::custom::Origin,
    },
    /// Scoped tempo in BPM, clamped to 40..=240, then `0x16` TempoChange (`0x875a48`).
    SetTempo(f32),
    /// A normalized metronome/tempo slot (`0x513410`): UI settings, tap tempo, Cortex.
    SetTempoParameter {
        index: u32,
        value: f32,
        remote: bool,
    },
    /// Stock resend of every metronome slot (`0x512c74`): preset load and scope change.
    MetronomeResend,
    /// The 10 ms metronome flush timer (`0x556ed4`).
    MetronomeFlush,
    /// MIDI clock BPM for metronome slot 0 without a store write (`0x556e3c`).
    MetronomeClockTempo(i32),
    MetronomeStatus(tetrad_dsp_protocol::responses::MetronomeStatus),
    /// The external MIDI clock BPM while a clock drives the tempo (`0x64c6e0` gate).
    ClockTempo(Option<i32>),
    /// The MIDI controller clock after each step; also sets [`Action::ClockTempo`].
    MidiClock(super::midi::ClockStatus),
    /// Tuner engine `0x36` (`0x52098c`): on at boot, off only on the Neural Capture screen.
    TunerEngine(bool),
    /// A tuner meter reading in Hz; `None` when none came for 500 ms (`0x4e12b0`).
    TunerReading(Option<f32>),
    /// Cortex Control tuner meter subscription (a meter service consumer).
    TunerMeterSubscribed(bool),
    /// Cortex Control reference offset in Hz; it may be fractional, unlike the UI setting.
    SetTunerReference(f32),
    /// An I/O SETTINGS value (`ConnectorsSettings`, normalized), applied at once and saved
    /// 250 ms after the last edit (`jacks::set`).
    SetConnector {
        id: ConnectorId,
        value: f32,
    },
    /// Link or unlink OUT 1/2 or OUT 3/4 (`0x3608e8`, `jacks::set_pair`).
    PairOutputs {
        pair: super::jacks::OutputPair,
        linked: bool,
    },
    /// QC mini key step (`0x1fc76c`), from the saved AtmaVolume; Control gets the level at
    /// once (`0x19af74`).
    StepVolume(f32),
    /// Be Right Back on (`true`) or off: the stock standby sequence (`0x6a82ac`), then
    /// Standby or WakeUp to Cortex Control (`0x16b758`).
    Standby(bool),
    /// A DSPErrorDialog button ([`super::dsp::DspFault`]).
    DspErrorDialog(DspErrorChoice),
    /// Asks the UI to open (`true`) or close the tuner. [`State::screen`] changes only when
    /// the UI reports the screen ([`Action::ScreenShown`]).
    ShowTuner(bool),
    /// Asks the UI to open or close the tempo popup.
    ShowTempo(bool),
    /// Asks the UI to open or close the Gig View.
    ShowGigView(bool),
    /// The UI bridge reports a new top screen (touch, Back, footswitch close or a request).
    ScreenShown(Screen),
    /// The UI reports the keyboard, modes overlay and editor state for the LED view.
    UiLeds(super::leds::UiLeds),
    /// The footswitch bank overlay; `None` closes it.
    BankOverlay(Option<BankOverlay>),
    /// Expression position 0..=1 (physical pedal or MIDI CC#1/CC#2).
    Expression {
        pedal: Pedal,
        position: f32,
        from_midi: bool,
    },
    /// The HAL plug word (`PluggedController`, `Pedal::plug_mask`).
    PedalPlugs(u16),
    /// The expression overrides a deploy used; the old ones are replaced after it succeeds.
    ExpressionDeployed(Box<super::features::Expression>),
    /// The bypass auto-engage delay timer (`0x6df618`).
    ExpressionPoll,
    /// A clip indicator wake (`soundcard_over1/2`) at device time `at`.
    Overload {
        channel: OverloadChannel,
        at: Duration,
    },
    /// A Looper tile edge (touch or Looper-view footswitch): the tile gates, then the store
    /// path for P0–7 (`0x6133c4` → `0x523644`). See [`super::features::looper_tile`].
    LooperTile {
        button: LooperButton,
        pressed: bool,
    },
    /// Looper editor `+0x187` (setter `0x617934`): CC#60, the mini lock button.
    LooperView(bool),
    /// Looper X swap dialog: arm (`Some(tile)`) or cancel the footswitch assignment.
    LooperAssign(Option<u8>),
    /// "Reset Looper X Actions" ACCEPT (`0x4b40a8`): the factory `stompAssignments`.
    ResetLooperActions,
    /// A Looper X button edge straight to the driver, nothing stored (MIDI CC#49..=56).
    Looper {
        button: LooperButton,
        pressed: bool,
    },
    /// The last DSP Looper status record (`0x12`) for the UI and Cortex READ.
    LooperStatus(tetrad_dsp_protocol::responses::LooperStatus),
    /// The DSP controller's Looper playback state (`+0xb824..`), mirrored for the UI.
    LooperPlayback(tetrad_dsp_host::setup::drivers::looper::LooperPlayback),
    CreateSetlist(String),
    DeletePreset {
        setlist: String,
        slot: usize,
    },
    /// Renames a library preset; the file follows the name and the slot stays
    /// (`StockLibrary::rename_preset`, stock `0x711aa4`). The loaded preset takes Edit Details
    /// through a save instead (`Effect::SavePreset`).
    RenamePreset {
        setlist: String,
        slot: usize,
        name: String,
    },
    /// Copies a library preset into the first empty slot of its setlist (stock Copy, then
    /// "Paste in the first empty slot", `0x7c3d94`; the device menu has no Duplicate).
    DuplicatePreset {
        setlist: String,
        slot: usize,
    },
    /// Directory Multi Select: copy a preset of setlist `from` into the first empty slot of
    /// `to` (`preset::copy_into`); with `remove` the source slot is then deleted (move).
    CopyPreset {
        from: String,
        slot: usize,
        to: String,
        remove: bool,
    },
    /// [`Action::CopyPreset`] into slot `to_slot` of `to` (`None`: the first empty one); an
    /// occupied slot is overwritten (`preset::copy_into`). Cortex Control File COPY and MOVE
    /// with a target index (`0x24b254` → `0x1e8a70`).
    CopyPresetTo {
        from: String,
        slot: usize,
        to: String,
        to_slot: Option<usize>,
        remove: bool,
    },
    /// SWAP of slot `slot` of setlist `from` and slot `to_slot` of setlist `to`: in one
    /// setlist the two slots trade files (`StockLibrary::swap_slots`); between two setlists
    /// each preset becomes a new file in the other one (`StockLibrary::swap_between`,
    /// `0x1e5a58` → `0x1e3824`). A loaded preset keeps playing from its new file.
    SwapPresets {
        from: String,
        slot: usize,
        to: String,
        to_slot: usize,
    },
    /// Directory Multi Select PASTE into chosen slots (r29-ui §4): `pairs` of (slot of `from`,
    /// slot of `to`) in paste order, each source copied before a pair overwrites it. Every
    /// pair is checked before the first write and one refusal refuses the whole paste; the
    /// copies stop at the first error (review r29 B1). With `remove` each source then goes.
    PastePresets {
        from: String,
        to: String,
        pairs: Vec<(usize, usize)>,
        remove: bool,
    },
    /// Push an undo frame at once on a stock path (`preset::History::record`): a value edit
    /// merges with UI value edits under 500 ms old (the Grid view timer), a structural edit
    /// always pushes. UI edits use [`Action::Undoable`]; Cortex Control records its own
    /// frames when its edits commit (`preset::History::begin`).
    RecordUndo(preset::UndoPath),
    /// A UI edit on an undo path: the frame is taken before the action and kept only when the
    /// edit commits, at once or with the deploy of the layout change it queues
    /// ([`preset::LayoutChange::record_undo`]). Stock takes the frame after its fit check
    /// (`0x59ec78`: `0x511380`, then `0x648f1c`), so an add that does not fit pushes none; here
    /// a compile or deploy refusal leaves the history as it was.
    Undoable(preset::UndoPath, Box<Action>),
    /// A grid drag and drop of the block at `from` on the `side` half of `to` (touch): stock
    /// `Chain::dropSlot` (`0x59f620`, [`tetrad_engine::Layout::plan_drop`]) as
    /// [`LayoutEdit::Moves`] and the pair's split and mix points in one compile.
    DropBlock {
        from: Slot,
        to: Slot,
        side: tetrad_engine::DropSide,
    },
    /// Restore the newest undo or redo frame (`0x649330`, `0x649b7c`). A library swap frame
    /// becomes [`Action::StepLibrarySwap`], a delete or copy/move frame
    /// [`Action::StepLibraryFrame`], a load frame [`Effect::StepPresetLoad`].
    Undo,
    Redo,
    /// Undo (or redo) of the newest library swap frame (`0x649330` case 2, `0x649b7c`): the two
    /// slots swap back. A library write: bring-up refuses it without `--persist`.
    StepLibrarySwap {
        redo: bool,
    },
    /// Undo (or redo) of the newest delete or copy/move frame (`0x649330` cases 1 and 3): its
    /// setlist slots get back what they held. A library write: bring-up refuses it without
    /// `--persist`.
    StepLibraryFrame {
        redo: bool,
    },
    /// Stock scene copy or swap (`0x51a2a8`), then a recall of `second`. No undo frame.
    CopyScene {
        first: SceneIndex,
        second: SceneIndex,
        swap: bool,
    },
    /// Adds a preset file to the preset favorites (newest first, the oldest of 64 drops,
    /// `0x007ceb78`) or removes it (`0x007cd32c`). The folder is its setlist directory.
    SetFavorite {
        path: String,
        favorite: bool,
    },
    /// Settings or factory reset (`0x004decdc` / `0x004de79c`).
    Reset(ResetKind),
    /// The restore reload replaced the settings with the restored files (`0x3fcf64` step 9):
    /// these groups changed, and their readers follow (step 10).
    SettingsReloaded(Vec<SettingsEvent>),
    /// The Preset MIDI Out editor stores one whole list in the loaded preset (`0x3c1a7c`;
    /// no MIDI is sent). A footswitch or pedal list also resets the CC Toggle states.
    SetPresetMidi {
        target: tetrad_ui::MidiOutTarget,
        messages: Vec<tetrad_ui::MidiOutMessage>,
    },
    /// Expression assignment (shot 068): link a block parameter (`Some(index)`) or its bypass
    /// (`None`) to `pedal`; unlinking clears only a link to that pedal. `Param.expression`
    /// codes 0/1/2 (`0x3242ec`); the overrides follow at once (`refresh_expression`).
    LinkExpression {
        slot: Slot,
        parameter: Option<u32>,
        pedal: tetrad_core::performance::expression::Pedal,
        linked: bool,
    },
    /// The pedal bypass settings of a block (`expression_bypass_info`, dialog `0x34d9b8`).
    ExpressionSwitch {
        slot: Slot,
        info: tetrad_core::performance::expression::BypassInfo,
    },
    /// A Virtual Device Preset write in `zenui.db` (`model_presets`).
    DevicePreset(DevicePresetEdit),
    /// A stored-preset edit from Cortex Control Grid UPDATE (`preset::PresetEdit`).
    PresetEdit(preset::PresetEdit),
    /// Writes a stored block and its bypass record back after a paste of the same model
    /// (Virtual Device Preset ✕ after another model): EXP links and ranges, the pedal
    /// switch, scene modes and the scene bypass. Refused over another model (failed paste).
    RestoreBlock {
        slot: Slot,
        model: Box<tetrad_preset::Model>,
        bypass: Box<tetrad_preset::ColBypass>,
    },
    /// "Save Current Parameters As…" (manual "Virtual Device Presets", `0xa1b074`): a new
    /// user preset of `model` (`0x0120494c`; unique name per model, at most 32 per model).
    CreateDevicePreset {
        model: u32,
        name: String,
        switch: tetrad_storage::stock::ExpSwitch,
        params: Vec<tetrad_storage::stock::ParamEntry>,
    },
    /// "Set Parameters as Defaults" (`0xa32b44`): the block key's `default_param` rows
    /// (`0x011ee5d4`, `0x011ee124`) and `default_exp_switch` (`0x011ee4c0`).
    SetDefaultParams {
        key: String,
        switch: tetrad_storage::stock::ExpSwitch,
        params: Vec<tetrad_storage::stock::ParamEntry>,
    },
    /// Directory folder links (manual "FOLDERS & NESTED NAVIGATION"): `folder_content` rows of
    /// a writable capture/IR folder (`0x012045c8` link, `0x01204628` unlink), one transaction.
    LinkMedia {
        folder: i64,
        products: Vec<String>,
        linked: bool,
    },
    /// Virtual Device List hold (manual "BROWSING & LOADING VIRTUAL DEVICES"): the model's
    /// `pinned_models` row, keyed by the decimal model id (`0x011ee1bc`, as Cortex Control
    /// `PinnedModels`).
    PinModel {
        model: u32,
        pinned: bool,
    },
    /// Device Information name (`DeviceNameDialog`): `general.custom_name`; `None` clears it.
    SetDeviceName(Option<String>),
    /// Folder menu DUPLICATE (`0x76dabc`): the folder, its subfolders and links under a free
    /// sibling name (`tetrad_library::stock::duplicate_media_folder`).
    DuplicateMediaFolder {
        folder: i64,
    },
    /// Folder menu "SET AS SAVING DESTINATION": `downloadDestinationFolderNC` / `IR` takes the
    /// folder key (`0x64ee28`; `media::set_saving_destination`).
    SetSavingDestination {
        folder: i64,
    },
    /// Global EQ (4004) or I/O (31000) Virtual Device Preset load (`0x227a84` `load`): the
    /// preset rows go to the settings and the hardware (`global_eq::load_preset` at once;
    /// `global_eq::load_io_preset` after the input mute settled, `jacks::begin_io_load`).
    LoadSettingsPreset {
        model: u32,
        rows: Vec<tetrad_storage::stock::ParamEntry>,
    },
    /// The factory default I/O preset (the special id, `0x3634e8` → `0x3615a0`): every
    /// connector row to its catalog default, then the I/O reset ([`super::jacks::reset`]),
    /// under the same mute and settle as a stored I/O preset (`jacks::begin_io_load`).
    LoadFactoryIoPreset,
    /// Device Storage RESTORE (O, review r23 K5): a capture of the quarantine table returns to
    /// its store (`tetrad_library::stock::restore_quarantined_capture`).
    RestoreQuarantinedCapture {
        store: tetrad_storage::stock::CaptureStore,
        id: String,
    },
}

/// The DSPErrorDialog buttons (`confirmSignal` → `rebootButtonSlot`, `cancelSignal` →
/// `cancelRebootButtonSlot`, connected by `0x5149dc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DspErrorChoice {
    /// CANCEL (`0x514c48`): the dialog hides and `LastPreset` becomes Factory Library slot 0
    /// (`0x64cea0(settings, 0x765508(), 0, "")`), so the next boot does not load this preset.
    /// The fault latch stays; the fault mute ends and the unit plays on, as stock.
    Cancel,
    /// REBOOT (`0x514a6c`): as CANCEL, then the settings sync (`0x64be28`) and the DSP fault
    /// restart `0x6a8914(…, 2)`: `0x20` value 0, headphones 0, 2 s, `zenreboot`.
    Reboot,
}

/// The scene copy clipboard (`0x459d5c` → `0x459db0`): source scene and copy or swap. In
/// scene copy mode a scene footswitch other than the source copies or swaps into its scene
/// (`0x51a2a8`), forces it (`0x5899d8(dst, 1)`) and leaves the mode. Without a source the
/// press picks it (inferred: stock signals the UI, which keeps the clipboard).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneClipboard {
    pub source: Option<SceneIndex>,
    pub swap: bool,
}

/// Action-layer performance state beside tetrad-core's [`super::Performance`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PerformState {
    pub bypass: preset::BypassCommits,
    pub scene_clipboard: Option<SceneClipboard>,
    /// A preset loaded since the last stomp press (`+0x3ab`, set by `0x527d80`): the next
    /// momentary release does nothing and clears it (`0x523f80`); a press clears it
    /// (`0x5239d0`).
    pub preset_loaded: bool,
    /// Pressed stomps not yet released (DSPController map `+0xf034`): a press adds the stomp
    /// (`0x5239d0`), a release erases it first (`0x523f80`). The capture revert scans it.
    pub held: std::collections::BTreeSet<StompIndex>,
}

/// Virtual Device Preset writes (manual "Virtual Device Presets"; tetrad-storage `0x01204aa8`,
/// `0x011ee288`, `0x011ee360`). Presets are keyed by their `id`; `model` is the stock `hash`.
#[derive(Debug, Clone, PartialEq)]
pub enum DevicePresetEdit {
    /// `factory`: the preset is a `factory.db` row (`default_model_presets.isFactory`).
    SetDefault {
        model: u32,
        preset: u64,
        factory: bool,
    },
    Rename {
        preset: u64,
        name: String,
    },
    /// Stores a block's parameters and bypass switch into a user preset.
    Overwrite {
        preset: u64,
        switch: tetrad_storage::stock::ExpSwitch,
        params: Vec<tetrad_storage::stock::ParamEntry>,
    },
    Delete(u64),
}

/// One capture row of a Captures root view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleCapture {
    /// Parameter 5 text: the 64-unit key, then the name (`0x6f7018`).
    pub value: String,
    /// Metadata `version` ("1", "2") for the VERSIONS filter (`0x7f4c1c`).
    pub version: String,
}

/// The rows BANK UP walks on a capture block, in the Directory row order of each root view
/// (pinned first, then the view sort, `0x755258`). No folder rows; no VERSIONS filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureCycle {
    /// Captures Library root (`0x765520`, library `+0x18`): read-only (factory) captures
    /// first, by device code, then by natural name (`0x542628`); then the other rows.
    pub library: Vec<CycleCapture>,
    /// Downloads NC root (`0x7657f8`, library `+0x38`, `cloud-2-1`). Its rows are keyed by
    /// the capture hash, not the product `id`: stock walks product ids and maps hash → id
    /// before the walk and id → hash after it (`0x214bf4`, `0x214b84` on
    /// `CacheStrategyPermanentDb`, both maps from one `downloads_product_nc` row), so the same
    /// rows in the same order give the same next capture.
    pub downloads: Vec<CycleCapture>,
}

/// Why a capture cycle loads nothing. Stock only logs it (`0x4f50dc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleMiss {
    /// "loadNextNeuralCapture: No folder found" (`0x6f5ce0` found no root).
    NoFolder,
    /// "No next key found" (`0x808250`: the key is not a row of that version).
    NoNextKey,
}

impl CaptureCycle {
    /// `0x6f5ce0`: the root that holds `key`, the Captures Library first. `0x808250`: that
    /// root's rows of `version`, then the row after `key` (before it when `backwards`); both
    /// ends wrap.
    pub fn step(
        &self,
        key: &str,
        version: &str,
        backwards: bool,
    ) -> Result<&CycleCapture, CycleMiss> {
        let is_key = |row: &&CycleCapture| preset::capture_key(&row.value) == key;
        let root = [&self.library, &self.downloads]
            .into_iter()
            .find(|rows| rows.iter().any(|row| is_key(&row)))
            .ok_or(CycleMiss::NoFolder)?;
        let rows: Vec<&CycleCapture> = root.iter().filter(|row| row.version == version).collect();
        let at = rows.iter().position(is_key).ok_or(CycleMiss::NoNextKey)?;
        let next = match backwards {
            false => (at + 1) % rows.len(),
            true => at.checked_sub(1).unwrap_or(rows.len() - 1),
        };
        Ok(rows[next])
    }
}

/// Structural edits; each recompiles with core reuse (`Deployment::edit`). A failed compile
/// keeps the previous preset (`0x585464`).
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutEdit {
    Add {
        slot: Slot,
        model: u32,
    },
    Replace {
        slot: Slot,
        model: u32,
    },
    /// A Cortex Control GridMove drop of the block at `from` on `to`: [`Action::EditLayout`]
    /// runs it as [`Action::DropBlock`] on the left half (the GridMove previews pass no half
    /// yet). In a list of edits it moves to an empty slot.
    Move {
        from: Slot,
        to: Slot,
    },
    /// Blocks that move at once, each with its stored records ([`tetrad_preset::Preset::move_blocks`]).
    Moves(Vec<(Slot, Slot)>),
    Remove(Slot),
    /// A fresh instance of the same model (catalog defaults).
    Reset(Slot),
    Paste {
        slot: Slot,
        block: Box<Instance>,
    },
    Input {
        row: Row,
        port: InputPort,
    },
    Output {
        row: Row,
        port: OutputPort,
    },
    Split {
        pair: Pair,
        column: Option<Column>,
    },
    Mix {
        pair: Pair,
        column: Option<Column>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("{0} is not available")]
    Unavailable(&'static str),
    #[error("not supported yet: {0}")]
    Unsupported(&'static str),
    #[error(transparent)]
    Preset(#[from] PresetError),
    #[error(transparent)]
    Deploy(#[from] tetrad_engine::DeployError),
    #[error(transparent)]
    Setting(#[from] tetrad_settings::SettingError),
    #[error(transparent)]
    Library(#[from] tetrad_library::Error),
    #[error(transparent)]
    Jack(#[from] tetrad_hal::zenjack::ZenjackError),
    #[error(transparent)]
    Tempo(#[from] crate::tempo::TempoError),
    #[error(transparent)]
    PresetData(#[from] tetrad_preset::Error),
    #[error(transparent)]
    Database(#[from] tetrad_storage::Error),
}

impl ActionError {
    pub fn subsystem(&self) -> Subsystem {
        match self {
            Self::Setting(_) | Self::Tempo(_) => Subsystem::Settings,
            Self::Jack(_) => Subsystem::Input,
            Self::Library(_) | Self::Database(_) => Subsystem::Library,
            Self::Unavailable(_)
            | Self::Unsupported(_)
            | Self::Preset(_)
            | Self::PresetData(_)
            | Self::Deploy(_) => Subsystem::Preset,
        }
    }
}

/// Applies one action. A refused action changes nothing, except a stomp toggle over several
/// blocks: it keeps and deploys the blocks it flipped before the refused one, with a notice.
pub fn reduce(action: Action, state: &mut State) -> Result<Vec<Effect>, ActionError> {
    let mut effects = Vec::new();
    match action {
        Action::Directory(request) => effects.push(Effect::Directory(request)),
        Action::Time(now) => {
            state.now = now;
            return Ok(effects);
        }
        Action::LoadPreset { target, reload } => {
            effects.extend(load_target(state, target, reload).map(Effect::LoadPreset));
            return Ok(effects);
        }
        Action::ProgramChange { target, reload } => {
            effects.extend(load_target(state, target, reload).map(Effect::ProgramChange));
            return Ok(effects);
        }
        Action::SelectScene(scene) => {
            let deployment = &mut current(state)?.deployment;
            if deployment.scene() == scene {
                return Ok(effects);
            }
            // The engine's value ops are not pushed: the recall sends the stock order.
            let _ = deployment.select_scene(scene);
            if !state.perform.bypass.is_empty() {
                let (specs, current, commits) = loaded_bypass(state)?;
                if let Err(error) = commits.revert(current, specs) {
                    effects.push(Effect::Notice(
                        Subsystem::Preset,
                        format!("scene change bypass revert: {error}"),
                    ));
                }
            }
            effects.extend(scene_recall(state));
        }
        Action::SetParameter { site, index, value } => {
            let (specs, current) = loaded(state)?;
            effects.push(match site {
                Site::InputControl(row) => {
                    Effect::DeployGate(preset::set_input_gate(current, specs, row, index, value)?)
                }
                site => Effect::Deploy(preset::set_parameter(current, specs, site, index, value)?),
            });
        }
        Action::SetBypass { site, bypassed } => {
            effects.push(write_bypass(state, site, bypassed, ChangeSource::Touch)?);
        }
        Action::MomentaryBypass { site, bypassed } => {
            effects.push(write_bypass(
                state,
                site,
                bypassed,
                ChangeSource::Momentary,
            )?);
        }
        Action::SetText { site, index, text } => {
            let (specs, current) = loaded(state)?;
            effects.push(Effect::Deploy(preset::set_text(
                current, specs, site, index, &text,
            )?));
            let database = state
                .library
                .as_ref()
                .and_then(|library| library.database.as_ref());
            match super::media::recent_media_selection(database, &text) {
                Ok(Some(request)) => effects.push(Effect::Directory(request)),
                Ok(None) => {}
                Err(error) => effects.push(Effect::Notice(Subsystem::Library, error.to_string())),
            }
        }
        Action::ToggleStomp(stomp) => {
            state.perform.preset_loaded = false;
            state.perform.held.insert(stomp);
            let momentary = momentary(state, stomp);
            effects.extend(stomp_writes(state, stomp, Some(momentary))?);
        }
        Action::ReleaseStomp(stomp) => {
            state.perform.held.remove(&stomp);
            if state.perform.scene_clipboard.is_some() || !momentary(state, stomp) {
                return Ok(effects);
            }
            // `0x523f80` clears `+0x3ab` only when it skips a momentary toggle.
            if std::mem::take(&mut state.perform.preset_loaded) {
                return Ok(effects);
            }
            effects.extend(stomp_writes(state, stomp, None)?);
        }
        Action::SetLocked(locked) => {
            state.master_volume.locked = locked;
            effects.push(super::control::lock_notice(locked));
        }
        Action::SceneCopyMode(clipboard) => {
            state.perform.scene_clipboard = clipboard;
            state.performance.footswitch.scene_copy = clipboard.is_some();
        }
        Action::BankEncoder(delta) => {
            effects.push(Effect::UiInput(super::ui::UiInput::View(
                tetrad_ui::InputEvent::BankEncoder { delta },
            )));
        }
        Action::CycleModel { slot, step } => {
            let model = state
                .current
                .as_ref()
                .and_then(|current| current.deployment.layout().instance(Site::Slot(slot)))
                .map(|block| block.model.0)
                .ok_or(ActionError::Unavailable("a block at that slot"))?;
            let specs = state
                .specs
                .as_ref()
                .ok_or(ActionError::Unavailable("the catalog"))?;
            // `0x4f5f38`: Neural Capture (category 14) steps through the library captures
            // (`0x4f50dc`), not the models.
            if specs
                .model(model)
                .is_some_and(|spec| spec.category_id == CAPTURE_CATEGORY)
            {
                return cycle_capture(state, slot, step);
            }
            // `0x4f5f38` reads only the sign: one model per event (review r15 R7).
            let next = preset::cycled_model(specs, model, step.signum())
                .ok_or(ActionError::Unavailable("that model in the catalog"))?;
            if next == model {
                return Ok(effects);
            }
            return reduce(
                Action::EditLayout(LayoutEdit::Replace { slot, model: next }),
                state,
            );
        }
        Action::CaptureCycle(cycle) => state.capture_cycle = Some(cycle),
        Action::EditorBlock(block) => {
            effects.push(match block {
                Some(_) => Effect::StartTimer {
                    timer: Timer::EditorMeters,
                    after: super::meters::POLL,
                },
                None => Effect::StopTimer(Timer::EditorMeters),
            });
            state.editor_block = block;
        }
        Action::EditLayout(LayoutEdit::Move { from, to }) => {
            effects.extend(drop_block(state, from, to, tetrad_engine::DropSide::Left)?);
        }
        Action::DropBlock { from, to, side } => effects.extend(drop_block(state, from, to, side)?),
        Action::EditLayout(edit) => effects.extend(edit_layout(state, vec![edit])?),
        Action::EditLayouts(edits) => effects.extend(edit_layout(state, edits)?),
        Action::PresetEdit(edit) => {
            let expression = matches!(
                edit,
                preset::PresetEdit::ExpressionRange { .. }
                    | preset::PresetEdit::SiteExpression { .. }
                    | preset::PresetEdit::SiteSwitch { .. }
            );
            let (specs, current) = loaded(state)?;
            if let Some(change) = preset::apply_edit(current, specs, edit)? {
                effects.push(Effect::DeployLayout(change));
            }
            if expression {
                effects.extend(refresh_expression(state, true));
            }
        }
        Action::SceneLabel { scene, label } => {
            pad(&mut current(state)?.preset.scene_labels)[scene.index()] = label;
        }
        Action::SceneColor { scene, raw_color } => {
            pad(&mut current(state)?.preset.scene_colors)[scene.index()] = raw_color;
        }
        Action::StompLabel { stomp, label } => {
            current(state)?
                .preset
                .single_stomp_labels
                .insert(stomp, label);
        }
        Action::ChangeSetting { id, value } => effects.extend(change_setting(state, id, value)?),
        // A refused mode keeps the Custom mode and its held switches.
        Action::SetMode(mode) => {
            effects.extend(set_mode(state, mode)?);
            effects.extend(super::custom::set_active(state, false));
        }
        Action::SetCustomMode(active) => effects.extend(super::custom::set_active(state, active)),
        Action::CustomPage(step) => effects.extend(super::custom::turn(state, step)),
        Action::CustomSlot {
            slot,
            pressed,
            origin,
        } => effects.extend(super::custom::slot(state, slot, pressed, origin)),
        Action::CustomFire {
            slot,
            gesture,
            epoch,
            origin,
        } => effects.extend(super::custom::fired(state, slot, gesture, epoch, origin)),
        Action::SetConnector { id, value } => {
            effects.extend(super::jacks::set(state, id, value)?);
        }
        Action::PairOutputs { pair, linked } => {
            effects.extend(super::jacks::set_pair(state, pair, linked));
        }
        Action::StepVolume(step) => {
            if !state.platform.is_atma() || !step.is_finite() {
                return Ok(effects);
            }
            let volume = AtmaVolume::new(
                (state.settings.settings().atma_volume.get() + step).clamp(0.0, 1.0),
            )?;
            effects.extend(update_settings(state, |settings| {
                settings.atma_volume = volume
            }));
            effects.push(master_volume(volume.get()));
            effects.push(Effect::ControlNotify(Box::new(
                super::control::master_volume_echo(volume.get()),
            )));
            // `updatePotentiometerSignal(false)` shows the Master Volume overlay (`0x4fffb8`).
            if state.splash_dismissed {
                effects.push(Effect::UiInput(UiInput::View(
                    tetrad_ui::InputEvent::MasterVolume {
                        percent: (volume.get() * 100.0).round() as u8,
                    },
                )));
            }
        }
        Action::Standby(on) => {
            effects.extend(standby(state, on)?);
            effects.push(super::control::standby_notice(on));
        }
        Action::DspErrorDialog(choice) => {
            let fault = (state.dsp_fault.as_mut())
                .filter(|fault| fault.dialog)
                .ok_or(ActionError::Unavailable("the DSP error dialog"))?;
            fault.dialog = false;
            effects.extend(update_settings(state, |settings| {
                settings.last_preset = tetrad_settings::library::LastPreset {
                    setlist: Some(super::FACTORY_SETLIST.into()),
                    preset_index: 0,
                    key: String::new(),
                };
            }));
            // The power path flushes the settings when its teardown starts. CANCEL plays on
            // (`0x514c48`): the owned fault mute ends.
            effects.push(match choice {
                DspErrorChoice::Reboot => Effect::Power(super::PowerAction::Restart(
                    tetrad_watchdog::power::RestartReason::DspFault { delay_seconds: 2 },
                )),
                DspErrorChoice::Cancel => Effect::Dsp(DspCommand::ReleaseFaultMute),
            });
        }
        // `0x58ce2c`: an equal list does nothing; else the list, then its first mode.
        Action::SetAllowedModes(modes) => {
            if modes == state.settings.settings().mode.available() {
                return Ok(effects);
            }
            let first = modes.first().copied();
            let mut result = Ok(());
            effects.extend(update_settings(state, |settings| {
                result = settings.mode.set_available(modes);
                if let (Ok(()), Some(first)) = (&result, first) {
                    result = settings.mode.set_mode(first);
                }
            }));
            result?;
            effects.extend(super::custom::set_active(state, false));
        }
        // (O) A Custom mode comes after the allowed stock modes, then the cycle starts again.
        Action::CycleMode => {
            let mode = &state.settings.settings().mode;
            let available = mode.available();
            let position = (available.iter()).position(|candidate| *candidate == mode.mode());
            let last = position.is_none_or(|index| index + 1 >= available.len());
            if !state.performance.footswitch.custom && last && state.custom.provider().is_some() {
                effects.extend(super::custom::set_active(state, true));
                return Ok(effects);
            }
            let next = match state.performance.footswitch.custom {
                true => 0,
                false => position.map_or(0, |index| (index + 1) % available.len().max(1)),
            };
            let next = *available
                .get(next)
                .ok_or(ActionError::Unavailable("an allowed footswitch mode"))?;
            effects.extend(set_mode(state, next)?);
            effects.extend(super::custom::set_active(state, false));
        }
        Action::ModeSlot(slot) => {
            let mode = *state
                .settings
                .settings()
                .mode
                .available()
                .get(usize::from(slot))
                .ok_or(ActionError::Unavailable("that mode slot"))?;
            effects.extend(set_mode(state, mode)?);
            effects.extend(super::custom::set_active(state, false));
        }
        Action::SetTempo(bpm) => {
            let bpm = bpm.clamp(TEMPO_BPM.0, TEMPO_BPM.1);
            effects.extend(super::features::set_tempo_parameter(
                state,
                0,
                (bpm - 40.0) / 200.0,
                false,
            )?);
        }
        Action::SetTempoParameter {
            index,
            value,
            remote,
        } => effects.extend(super::features::set_tempo_parameter(
            state, index, value, remote,
        )?),
        Action::MetronomeResend => {
            effects.push(Effect::MetronomeResendPlan(
                super::features::metronome_resend(state)?,
            ));
            effects.push(Effect::StopTimer(Timer::MetronomeFlush));
        }
        Action::MetronomeFlush => {
            if let Some(controller) = state.metronome.as_mut() {
                effects.extend(
                    controller
                        .flush()?
                        .into_iter()
                        .map(|frame| Effect::Dsp(DspCommand::Control(frame.request()))),
                );
            }
        }
        Action::MetronomeClockTempo(bpm) => {
            effects.extend(super::features::metronome_clock_tempo(state, bpm)?)
        }
        Action::MetronomeStatus(status) => {
            return Ok(super::features::metronome_status(state, status));
        }
        // MIDI queues this after every byte: present only a new BPM.
        Action::MidiClock(clock) => {
            state.midi_clock = clock;
            return reduce(Action::ClockTempo(clock.bpm()), state);
        }
        Action::ClockTempo(bpm) => {
            if state.clock_bpm == bpm {
                return Ok(effects);
            }
            state.clock_bpm = bpm;
        }
        Action::TunerEngine(enabled) => {
            let polled = super::features::tuner_polls(state);
            effects.extend(super::features::tuner_engine(state, enabled));
            effects.extend(super::features::tuner_poll_changed(state, polled));
        }
        Action::TunerReading(hz) => return Ok(super::features::tuner_reading(state, hz)),
        Action::TunerMeterSubscribed(enabled) => {
            let polled = super::features::tuner_polls(state);
            state.tuner_meter_subscribed = enabled;
            effects.extend(super::features::tuner_poll_changed(state, polled));
        }
        Action::SetTunerReference(offset) => {
            let mut result = Ok(());
            effects.extend(update_settings(state, |settings| {
                result = settings.tuner.set_reference_offset_hz(offset);
            }));
            result?;
            effects.extend(super::features::tuner_setting_changed(
                state,
                SettingId::TunerReference,
            ));
        }
        Action::ShowTuner(show) => return Ok(vec![screen(Screen::Tuner, show)]),
        Action::ShowTempo(show) => return Ok(vec![screen(Screen::Tempo, show)]),
        Action::ShowGigView(show) => return Ok(vec![screen(Screen::Gig, show)]),
        // The tuner engine (`0x36`) runs from boot; the screen only has its own DSP steps.
        Action::ScreenShown(shown) => effects.extend(screen_shown(state, shown)),
        Action::BankOverlay(overlay) => state.performance.footswitch.overlay = overlay,
        Action::UiLeds(ui) => state.ui_leds = ui,
        Action::Expression {
            pedal,
            position,
            from_midi,
        } => effects.extend(super::features::expression_event(
            state, pedal, position, from_midi, state.now,
        )),
        Action::PedalPlugs(plugged) => {
            use tetrad_core::performance::expression::Pedal as CorePedal;
            for (pedal, pedal_state) in [CorePedal::Exp1, CorePedal::Exp2]
                .into_iter()
                .zip(&mut state.pedals)
            {
                pedal_state.plugged = plugged & pedal.plug_mask() != 0;
            }
            effects.extend(refresh_expression(state, true));
        }
        Action::ExpressionDeployed(expression) => {
            state.expression = *expression;
            effects.push(Effect::StopTimer(Timer::ExpressionBypass));
            effects.extend(super::features::expression_poll(state, state.now));
        }
        Action::ExpressionPoll => {
            effects.extend(super::features::expression_poll(state, state.now))
        }
        Action::Overload { channel, at } => {
            state.overload[match channel {
                OverloadChannel::One => 0,
                OverloadChannel::Two => 1,
            }] = Some(at);
        }
        Action::Looper { button, pressed } => {
            let site = super::features::looper_slot(state)
                .ok_or(ActionError::Unavailable("a Looper X block"))?;
            effects.push(Effect::Dsp(DspCommand::LooperParameter {
                site: Site::Slot(site),
                index: button as u32,
                value: f32::from(u8::from(pressed)),
            }));
        }
        // The 50 ms poll answers with the same record most of the time.
        Action::LooperStatus(status) => {
            if state.looper.replace(status) == Some(status) {
                return Ok(effects);
            }
        }
        Action::LooperPlayback(playback) => state.looper_playback = playback,
        Action::LooperTile { button, pressed } => {
            return super::features::looper_tile(state, button, pressed);
        }
        Action::LooperView(shown) => state.looper_view = shown,
        Action::LooperAssign(tile) => state.looper_assign = tile.filter(|tile| *tile < 8),
        Action::ResetLooperActions => {
            let hardware = state.settings.hardware();
            let defaults = tetrad_settings::controls::LooperSettings::default_assignments(hardware);
            return Ok(super::features::set_looper_actions(state, defaults));
        }
        Action::CreateSetlist(name) => {
            library(state)?.create_setlist(&name)?;
        }
        Action::RenamePreset {
            setlist,
            slot,
            name,
        } => {
            let library = library(state)?;
            let old = library.slots(&setlist)?.get(slot).cloned().flatten();
            let renamed = library.rename_preset(&setlist, slot, &name)?;
            if let Some(current) = state.current.get_mut().as_mut().filter(|c| c.path == old) {
                current.path = Some(renamed.value);
                current.preset.name = Some(name);
            }
            effects.extend(stale_index(renamed.search_index));
        }
        // Stock Copy, then "Paste in the first empty slot" of the same setlist (`0x7c3d94`).
        Action::DuplicatePreset { setlist, slot } => effects.extend(copy_preset(
            state,
            (&setlist, slot),
            None,
            &setlist,
            None,
            false,
        )?),
        Action::CopyPreset {
            from,
            slot,
            to,
            remove,
        } => effects.extend(copy_preset(state, (&from, slot), None, &to, None, remove)?),
        Action::CopyPresetTo {
            from,
            slot,
            to,
            to_slot,
            remove,
        } => effects.extend(copy_preset(
            state,
            (&from, slot),
            None,
            &to,
            to_slot,
            remove,
        )?),
        Action::PastePresets {
            from,
            to,
            pairs,
            remove,
        } => effects.extend(paste_presets(state, &from, &to, &pairs, remove)?),
        Action::SwapPresets {
            from,
            slot,
            to,
            to_slot,
        } => {
            let (swapped, sides) = swap_presets(state, [(from, slot), (to, to_slot)], true)?;
            effects.extend(swapped);
            state.history.record_swap(sides);
        }
        Action::StepLibrarySwap { redo } => effects.extend(step_swap(state, redo)?),
        Action::StepLibraryFrame { redo } => effects.extend(step_slots(state, redo)?),
        Action::RecordUndo(path) => {
            if let Some(current) = &*state.current {
                state.history.record(current, state.now, path);
            }
            return Ok(effects);
        }
        Action::Undoable(path, action) => {
            let frame = (state.current.as_ref())
                .map(|current| state.history.begin(current, state.now, path));
            let mut effects = reduce(*action, state)?;
            if let Some(frame) = frame {
                let change = effects.iter_mut().find_map(|effect| match effect {
                    Effect::DeployLayout(change) => Some(change),
                    _ => None,
                });
                match change {
                    Some(change) => change.record_undo(frame),
                    None => state.history.commit(frame),
                }
            }
            return Ok(effects);
        }
        Action::Undo => effects.extend(undo(state, false)?),
        Action::Redo => effects.extend(undo(state, true)?),
        Action::CopyScene {
            first,
            second,
            swap,
        } => {
            let (specs, current) = loaded(state)?;
            let playing = current.deployment.scene() == second;
            match preset::copy_scene(current, specs, first, second, swap)? {
                preset::Restore::Preset(preset) => current.preset = *preset,
                preset::Restore::Layout(change) => effects.push(Effect::DeployLayout(change)),
            }
            // Inferred: the copy writes every bypass copy, so nothing stays to revert.
            state.perform.bypass.clear();
            // `0x1b048c` → `0x5899d8(dst, 1)`: the destination plays, recalled even when it
            // is the playing scene.
            if playing {
                effects.extend(scene_recall(state));
            } else {
                effects.push(Effect::Action(Action::SelectScene(second)));
            }
        }
        Action::SetFavorite { path, favorite } => {
            let folder = path.rsplit_once('/').map_or("", |(folder, _)| folder);
            let entry = ItemRef {
                folder: folder.to_owned(),
                item: path.clone(),
            };
            let list = ItemList::Favorites(Collection::Presets);
            let library = library(state)?;
            let before = library.items(list)?;
            if favorite {
                library.push_item(list, &entry)?;
            } else {
                library.remove_item(list, &entry)?;
            }
            let after = library.items(list)?;
            let added = favorite.then_some(&entry);
            effects.extend(super::control::list_pushes(
                state, list, &before, &after, added,
            ));
        }
        // The frame (type 1, `0x1d916c`) reads the preset in full before the delete. A preset
        // that cannot be read is still deleted, with no frame (O). Review r39 K5: the loaded
        // preset keeps no path to its deleted file; the frame marks it as the loaded preset,
        // so an UNDO gives the file back to that preset only (K1). The delete of the selected
        // slot's item then loads the new-preset template in place (`0x7c4c1c` → `0x5b2c54`).
        Action::DeletePreset { setlist, slot } => {
            let path = (state.current.as_ref()).and_then(|current| current.path.clone());
            let loaded = path.as_deref().map(|path| (path, state.history.loaded()));
            let selected = is_selected(state, &setlist, slot);
            let library = library(state)?;
            let deleted = library.slots(&setlist)?.get(slot).cloned().flatten();
            let before = preset::SlotState::read(library, &setlist, slot, loaded);
            let saved = library.delete(&setlist, slot)?;
            effects.extend(stale_index(saved.search_index));
            if deleted.is_some()
                && deleted == path
                && let Some(current) = state.current.get_mut().as_mut()
            {
                current.path = None;
            }
            match before {
                Ok(before) => (state.history).record_slots(
                    SlotOperation::Delete,
                    vec![before],
                    state.current.as_ref(),
                ),
                Err(error) => effects.push(no_frame(&setlist, slot, &error)),
            }
            if deleted.is_some() && selected {
                effects.push(Effect::LoadTemplate);
            }
        }
        Action::Reset(kind) => effects.extend(reset(state, kind)),
        // `0x360da8(io, 1)`: the live I/O applies the restored connector rows (I: as the boot
        // restore does, but with the reload's unverified writes).
        Action::SettingsReloaded(events) => {
            let io = |state: &mut State| {
                super::jacks::restore(state, tetrad_hal::analog::Verification::Unverified)
            };
            effects.extend(reapply_settings(state, events, io));
        }
        Action::SetPresetMidi { target, messages } => {
            let (list, source) = super::midi::preset_midi_list(target).ok_or(
                ActionError::Unsupported("a Preset MIDI Out list without a stomp"),
            )?;
            let messages: Vec<_> = messages.iter().map(super::midi::stored_message).collect();
            current(state)?.preset.set_midi_list(list, &messages)?;
            effects.extend(source.map(Effect::PresetMidiSource));
            effects.push(super::control::midi_out_edit(list, &messages));
        }
        Action::LinkExpression {
            slot,
            parameter,
            pedal,
            linked,
        } => effects.extend(link_expression(state, slot, parameter, pedal, linked)?),
        Action::ExpressionSwitch { slot, info } => {
            let model = stored_block(state, slot)?;
            let stored = preset::switch_info(info);
            match model.expression_bypass_info.first_mut() {
                Some(first) => {
                    *first = tetrad_preset::ExpressionBypassInfo {
                        unknown: std::mem::take(&mut first.unknown),
                        ..stored
                    }
                }
                None => model.expression_bypass_info.push(stored),
            }
            effects.extend(refresh_expression(state, true));
        }
        Action::RestoreBlock {
            slot,
            model,
            bypass,
        } => {
            let stored = stored_block(state, slot)?;
            if stored.hash != model.hash {
                return Err(ActionError::Unsupported(
                    "restore a block over another model",
                ));
            }
            *stored = *model;
            *current(state)?
                .preset
                .block_bypass_mut(tetrad_preset::Slot {
                    row: slot.row.index(),
                    column: slot.column.index(),
                })? = *bypass;
            state.perform.bypass.forget(slot);
            effects.extend(refresh_expression(state, true));
        }
        Action::DevicePreset(edit) => {
            let database = state
                .library
                .as_mut()
                .and_then(|library| library.database.as_mut())
                .ok_or(ActionError::Unavailable("zenui.db"))?;
            let id = |preset: u64| i64::try_from(preset).unwrap_or(i64::MAX);
            database.transaction(|write| match &edit {
                DevicePresetEdit::SetDefault {
                    model,
                    preset,
                    factory,
                } => {
                    write.set_default_model_preset(i64::from(*model), Some((id(*preset), *factory)))
                }
                DevicePresetEdit::Rename { preset, name } => {
                    write.rename_model_preset(id(*preset), name)
                }
                DevicePresetEdit::Overwrite {
                    preset,
                    switch,
                    params,
                } => write.update_model_preset(id(*preset), *switch, params),
                DevicePresetEdit::Delete(preset) => write.delete_model_preset(id(*preset)),
            })?;
            state.model_preset_revision += 1;
        }
        Action::CreateDevicePreset {
            model,
            name,
            switch,
            params,
        } => {
            user_database(state)?.transaction(|write| {
                write
                    .create_model_preset(&name, i64::from(model), switch, &params)
                    .map(drop)
            })?;
            state.model_preset_revision += 1;
        }
        Action::SetDefaultParams {
            key,
            switch,
            params,
        } => {
            user_database(state)?.transaction(|write| {
                write.set_default_params(&key, &params)?;
                write.set_default_exp_switch(&key, Some(switch))
            })?;
        }
        Action::LinkMedia {
            folder,
            products,
            linked,
        } => {
            user_database(state)?.transaction(|write| {
                products.iter().try_for_each(|product| {
                    if linked {
                        write.link(folder, product)
                    } else {
                        write.unlink(folder, product)
                    }
                })
            })?;
            effects.push(Effect::RefreshDirectory);
        }
        Action::PinModel { model, pinned } => {
            let key = model.to_string();
            user_database(state)?.transaction(|write| match pinned {
                true => write.pin_model(&key),
                false => write.unpin_model(&key),
            })?;
            state.model_preset_revision += 1;
        }
        Action::SetDeviceName(name) => {
            effects.extend(update_settings(state, |settings| {
                settings.general.custom_name = name;
            }));
        }
        Action::DuplicateMediaFolder { folder } => {
            tetrad_library::stock::duplicate_media_folder(user_database(state)?, folder)?;
            effects.push(Effect::RefreshDirectory);
        }
        Action::SetSavingDestination { folder } => {
            effects.extend(super::media::set_saving_destination(state, folder)?);
        }
        Action::LoadSettingsPreset { model, rows } => effects.extend(match model {
            tetrad_settings::io::GLOBAL_EQ_MODEL => super::global_eq::load_preset(state, &rows)?,
            super::global_eq::IO_PRESET_MODEL => {
                let preset = super::global_eq::io_preset(state, rows)?;
                super::jacks::begin_io_load(state, IoPresetLoad::Preset(preset))
            }
            _ => return Err(ActionError::Unsupported("a settings preset of that model")),
        }),
        Action::LoadFactoryIoPreset => {
            effects.extend(super::jacks::begin_io_load(state, IoPresetLoad::Factory));
        }
        Action::RestoreQuarantinedCapture { store, id } => {
            tetrad_library::stock::restore_quarantined_capture(user_database(state)?, store, &id)?;
            effects.push(Effect::RefreshDirectory);
        }
    }
    effects.push(Effect::Present);
    Ok(effects)
}

/// [`Action::EditLayout`]: layout edits in one compile, their junction defaults and stomp
/// auto-assign, then the deploy and the media recents of a placed block.
fn edit_layout(state: &mut State, edits: Vec<LayoutEdit>) -> Result<Vec<Effect>, ActionError> {
    let mut effects = Vec::new();
    let auto_assign = state.settings.settings().general.stomp_mode_auto_assign;
    // The commits follow only an edit that prepares (review r15 R14).
    let mut commits = state.perform.bypass.clone();
    edits.iter().for_each(|edit| commits.follow(edit));
    let defaults = junction_defaults(state, &edits, &mut effects);
    let (specs, current) = loaded(state)?;
    let added: Vec<Slot> = edits.iter().filter_map(new_block).collect();
    // `0x59dd78`: block add, model change, placement and paste record media recents.
    let placed = edits.iter().find_map(|edit| match edit {
        LayoutEdit::Add { slot, .. }
        | LayoutEdit::Replace { slot, .. }
        | LayoutEdit::Paste { slot, .. }
        | LayoutEdit::Move { to: slot, .. } => Some(*slot),
        LayoutEdit::Moves(moves) => moves.last().map(|&(_, to)| to),
        _ => None,
    });
    let mut change = preset::prepare_junction_edits(current, specs, edits, &defaults)?;
    for slot in added {
        change.assign_new_block(specs, slot, auto_assign)?;
    }
    let database = state
        .library
        .as_ref()
        .and_then(|library| library.database.as_ref());
    let recents = placed
        .and_then(|slot| change.deployment().layout().instance(Site::Slot(slot)))
        .map(|block| super::media::recent_block_media(database, block, change.deployment().scene()))
        .unwrap_or_default();
    state.perform.bypass = commits;
    effects.push(Effect::DeployLayout(change));
    effects.extend(recents);
    Ok(effects)
}

/// [`LayoutEdit::Move`]: the stock grid drop (`Chain::dropSlot` `0x59f620`). The target row
/// makes room, the block moves, and a drop between the rows of a pair sets the pair's split
/// and mix points in the same compile ([`preset::drop_edits`]). When that layout does not
/// deploy, the block moves without the points; when that fails too, nothing changes
/// (`0x59f1cc`). A drop stock refuses changes nothing. A drop on the left half of a block
/// makes room to the right first, on the right half to the left (`0x5c09c8`, `0x5c0ee0`).
fn drop_block(
    state: &mut State,
    from: Slot,
    to: Slot,
    side: tetrad_engine::DropSide,
) -> Result<Vec<Effect>, ActionError> {
    let (_, current) = loaded(state)?;
    let Some((moves, points)) = preset::drop_edits(current.deployment.layout(), from, to, side)
    else {
        return Ok(Vec::new());
    };
    if !points.is_empty() {
        let mut edits = vec![moves.clone()];
        edits.extend(points);
        if let Ok(effects) = edit_layout(state, edits) {
            return Ok(effects);
        }
    }
    edit_layout(state, vec![moves])
}

/// [`Action::ChangeSetting`]: a Global EQ, I/O or tempo value goes to its owner; other
/// settings are written, sent and mirrored. [`reduce`] adds the frame.
fn change_setting(
    state: &mut State,
    id: SettingId,
    value: SettingValue,
) -> Result<Vec<Effect>, ActionError> {
    let mut effects = Vec::new();
    let polled = super::features::tuner_polls(state);
    if let Some(changed) = super::global_eq::change(state, id, &value) {
        effects.extend(changed?);
        return Ok(effects);
    }
    if let Some(changes) = io_setting(state, id, &value)? {
        for (id, value) in changes {
            effects.extend(super::jacks::set(state, id, value)?);
        }
        return Ok(effects);
    }
    if let Some((index, value)) = super::features::tempo_setting(state, id, &value)? {
        effects.extend(super::features::set_tempo_parameter(
            state, index, value, false,
        )?);
        return Ok(effects);
    }
    let mut result = Ok(());
    let changed = update_settings(state, |settings| {
        result = write_setting(settings, id, &value);
    });
    result?;
    effects.extend(changed);
    effects.extend(setting_sends(id, state));
    if id == SettingId::MidiThru {
        effects.extend(super::jacks::follow_midi_thru(state));
    }
    if matches!(
        id,
        SettingId::TunerReference
            | SettingId::TunerMute
            | SettingId::TunerInput
            | SettingId::LiveTuner
    ) {
        effects.extend(super::features::tuner_setting_changed(state, id));
        effects.extend(super::features::tuner_poll_changed(state, polled));
    }
    Ok(effects)
}

/// [`Action::ScreenShown`]: the screen on top changed; the meter timers, the Looper view and
/// the tuner screen, engine and poll follow it.
fn screen_shown(state: &mut State, shown: Screen) -> Vec<Effect> {
    let mut effects = Vec::new();
    let polled = super::features::tuner_polls(state);
    let tuner_changed = (shown == Screen::Tuner) != (state.screen == Screen::Tuner);
    let capture_changed = (shown == Screen::Capture) != (state.screen == Screen::Capture);
    let io = |screen: Screen| matches!(screen, Screen::Io(_));
    // The I/O page meter set polls while the page shows (`0x4ad480`, `0x4adaa0`).
    match (io(state.screen), io(shown)) {
        (false, true) => effects.push(Effect::StartTimer {
            timer: Timer::IoMeters,
            after: super::meters::POLL,
        }),
        (true, false) => effects.push(Effect::StopTimer(Timer::IoMeters)),
        _ => {}
    }
    let grid = (state.screen == Screen::Grid, shown == Screen::Grid);
    let looper_opened = shown == Screen::Looper && state.screen != Screen::Looper;
    state.screen = shown;
    // I: opening the Looper view sets `+0x187` (a new 27000 block does, `0x59b7bc`).
    if shown == Screen::Looper {
        state.looper_view = true;
        if looper_opened {
            effects.extend(super::features::looper_editor_opened(state));
        }
    } else {
        state.looper_assign = None;
    }
    if tuner_changed {
        effects.extend(super::features::tuner_screen(state, shown == Screen::Tuner));
    }
    if capture_changed {
        effects.extend(super::features::tuner_engine(
            state,
            shown != Screen::Capture,
        ));
    }
    effects.extend(super::features::tuner_poll_changed(state, polled));
    // The Grid meter set (`0x596108`) polls while the Grid shows (owned).
    match grid {
        (false, true) => effects.push(Effect::StartTimer {
            timer: Timer::GridMeters,
            after: super::meters::POLL,
        }),
        (true, false) => effects.push(Effect::StopTimer(Timer::GridMeters)),
        _ => {}
    }
    effects
}

/// [`Action::Reset`]: the settings reset and what the DSP and the live I/O hold of them, then
/// [`Effect::ResetSteps`] for the work outside the settings files.
fn reset(state: &mut State, kind: ResetKind) -> Vec<Effect> {
    let mut effects = Vec::new();
    let defaults = state.specs.as_ref().and_then(preset::tempo_defaults);
    if defaults.is_none() {
        effects.push(Effect::Notice(
            Subsystem::Settings,
            "tempo settings reset without catalog defaults (model 25000)".into(),
        ));
    }
    let events = state.settings.reset(kind, &defaults.unwrap_or_default());
    // Every reset clears `ConnectorsSettings`; the live I/O follows (`0x3615a0`).
    let io = |state: &mut State| super::jacks::reset(state, false);
    effects.extend(reapply_settings(state, events, io));
    effects.push(Effect::ResetSteps(kind));
    // The reset paths re-read the knob (`0x4dec74`, `0x4defa4` → `0x1fc518`).
    effects.push(Effect::ReadVolume);
    effects
}

/// After the settings changed wholesale (a reset, a restore reload): the change events
/// (`events`), the flush, the DSP globals, the Global EQ, the live I/O (`io`), and the tuner
/// path and the tempo when their groups changed.
fn reapply_settings(
    state: &mut State,
    events: Vec<SettingsEvent>,
    io: impl FnOnce(&mut State) -> Vec<Effect>,
) -> Vec<Effect> {
    let changed = |group: fn(&SettingsEvent) -> bool| events.iter().any(group);
    let tuner = changed(|event| matches!(event, SettingsEvent::Tuner(_)));
    let tempo = changed(|event| matches!(event, SettingsEvent::Tempo(_)));
    let mut effects = mirror_settings(state, events);
    effects.extend(settings_flush(&state.settings));
    effects.push(Effect::Dsp(DspCommand::Globals));
    // Inferred: the new Global EQ reaches the DSP as at boot (`0x2fc9f4`).
    effects.extend(super::global_eq::push(state));
    effects.extend(io(state));
    // The DSP holds the tuner path and the tempo; boot and preset loads send them.
    if tuner {
        effects.extend(super::features::tuner_setting_changed(
            state,
            SettingId::TunerReference,
        ));
        effects.extend(super::features::tuner_path(state));
    }
    if tempo {
        effects.push(Effect::Dsp(DspCommand::Tempo(tempo_bpm(state))));
        effects.push(Effect::Action(Action::MetronomeResend));
    }
    effects
}

/// [`Action::LinkExpression`]: links or unlinks `pedal` on a parameter (`Some`) or on the
/// block bypass (`None`), then refreshes the deployed expression.
fn link_expression(
    state: &mut State,
    slot: Slot,
    parameter: Option<u32>,
    pedal: CorePedal,
    linked: bool,
) -> Result<Vec<Effect>, ActionError> {
    let mut effects = Vec::new();
    let model = stored_block(state, slot)?;
    let code = |old: i32| match (linked, CorePedal::from_assignment(old)) {
        (true, _) => pedal as i32,
        (false, Some(linked)) if linked == pedal => 0,
        (false, _) => old,
    };
    match parameter {
        Some(index) => {
            let param = model.param_mut(index as usize)?;
            param.expression = Some(code(param.expression.unwrap_or(0)));
            param.expression_min.get_or_insert(0.0);
            param.expression_max.get_or_insert(1.0);
        }
        None => {
            if model.bypass_expression.is_empty() {
                model.bypass_expression.push(tetrad_preset::Expression {
                    expression_max: 1.0,
                    ..tetrad_preset::Expression::default()
                });
            }
            let assignment = &mut model.bypass_expression[0];
            assignment.expression = code(assignment.expression);
            if linked && model.expression_bypass_info.is_empty() {
                model.expression_bypass_info.push(preset::switch_info(
                    tetrad_core::performance::expression::BypassInfo::default(),
                ));
            }
        }
    }
    effects.extend(refresh_expression(state, true));
    Ok(effects)
}

/// [`Action::CopyPresetTo`]: the copy, then with `remove` the source slot goes (a move); a
/// moved loaded preset keeps playing from its new file. A copy or move onto the loaded
/// preset's slot is refused, as Save As (review r20 G2): the overwrite would delete the file
/// the next device save writes. Inferred: the loaded-slot rule of the drop `0x1e68d0` is not
/// read. After the copy its frame (type 3, `0x1de9f8`): the target slot as it was, then for a
/// move the source slot; a target that cannot be read gives no frame (O).
fn copy_preset(
    state: &mut State,
    (from, slot): (&str, usize),
    source: Option<preset::CopySource>,
    to: &str,
    to_slot: Option<usize>,
    remove: bool,
) -> Result<Vec<Effect>, ActionError> {
    let loaded = state
        .current
        .as_ref()
        .and_then(|current| current.path.clone());
    let id = state.history.loaded();
    let library = library(state)?;
    if let Some(index) = to_slot
        && loaded.is_some()
        && library.slots(to)?.get(index).cloned().flatten() == loaded
    {
        return Err(ActionError::Unsupported(
            "a copy or move onto the loaded preset's slot",
        ));
    }
    if from == to && to_slot == Some(slot) {
        return Err(PresetError::Unsupported("a copy onto its own slot").into());
    }
    let old = library.slots(from)?.get(slot).cloned().flatten();
    let source = match source {
        Some(source) => source,
        None => preset::copy_source(library, from, slot)?,
    };
    // The target never holds the loaded preset (refused above).
    let target = match to_slot {
        Some(index) => preset::SlotState::read(library, to, index, None),
        None => Ok(preset::SlotState {
            setlist: to.to_owned(),
            slot: 0,
            expect: None,
            held: None,
        }),
    };
    let moved = match (remove, old.clone()) {
        (true, Some(path)) => {
            let loaded = loaded.as_deref().map(|loaded| (loaded, id));
            Some(preset::SlotState::held(
                library,
                source.clone(),
                path,
                loaded,
            ))
        }
        _ => None,
    };
    let copied = preset::copy_source_into(library, source, to, to_slot)?;
    let (index, path) = (copied.slot, copied.path.clone());
    let mut effects = copied_effects(copied);
    let mut frame = target.map(|target| {
        vec![preset::SlotState {
            slot: index,
            expect: Some(path.clone()),
            ..target
        }]
    });
    let mut failed = None;
    match remove.then(|| library.delete(from, slot)) {
        Some(Ok(saved)) => {
            effects.extend(stale_index(saved.search_index));
            if let Some(current) = state.current.get_mut().as_mut().filter(|c| c.path == old) {
                current.path = Some(path);
            }
            frame = frame.and_then(|mut frame| {
                if let Some(held) = moved {
                    frame.push(preset::SlotState {
                        setlist: from.to_owned(),
                        slot,
                        expect: None,
                        held: Some(held?),
                    });
                }
                Ok(frame)
            });
        }
        Some(Err(error)) => failed = Some(error),
        None => {}
    }
    match frame {
        Ok(frame) => {
            let current = state.current.as_ref();
            state
                .history
                .record_slots(SlotOperation::CopyOrMove, frame, current);
        }
        Err(error) => effects.push(no_frame(to, index, &error)),
    }
    match failed {
        Some(error) => Err(error.into()),
        None => Ok(effects),
    }
}

/// A library write that keeps no undo frame: its slot could not be read in full.
fn no_frame(setlist: &str, slot: usize, error: &PresetError) -> Effect {
    let detail = format!("slot {slot} of setlist {setlist} changed with no undo frame: {error}");
    Effect::Notice(Subsystem::Library, detail)
}

/// [`Action::PastePresets`]. Refused before any write (O, user data first): a pair onto its
/// own slot or out of range, two pairs with one source or one target, a pair that overwrites
/// the source of a later pair, a target that holds the loaded preset ([`copy_preset`]), and a
/// source that is empty, unreadable or has no stored name. Stock pastes the presets it
/// collected (`0x786d38` → `0x786ac4`), so no refused pair can lose another one.
fn paste_presets(
    state: &mut State,
    from: &str,
    to: &str,
    pairs: &[(usize, usize)],
    remove: bool,
) -> Result<Vec<Effect>, ActionError> {
    let loaded = (state.current.as_ref()).and_then(|current| current.path.clone());
    let library = library(state)?;
    let targets = library.slots(to)?;
    // Review r30 C8: the checked sources are the ones copied, so each is read once.
    let mut sources = Vec::with_capacity(pairs.len());
    for (index, &(slot, to_slot)) in pairs.iter().enumerate() {
        let later = &pairs[index + 1..];
        if (from == to && slot == to_slot)
            || later
                .iter()
                .any(|&(other, other_to)| other == slot || other_to == to_slot)
        {
            return Err(ActionError::Unsupported("a paste with a slot used twice"));
        }
        if from == to && later.iter().any(|&(other, _)| other == to_slot) {
            return Err(ActionError::Unsupported(
                "a paste that overwrites a preset before it is copied",
            ));
        }
        let target = targets
            .get(to_slot)
            .ok_or(tetrad_library::Error::SlotRange(to_slot))?;
        if loaded.is_some() && *target == loaded {
            return Err(ActionError::Unsupported(
                "a paste onto the loaded preset's slot",
            ));
        }
        sources.push(preset::copy_source(library, from, slot)?);
    }
    let mut effects = Vec::new();
    for (done, (&(slot, to_slot), source)) in pairs.iter().zip(sources).enumerate() {
        match copy_preset(state, (from, slot), Some(source), to, Some(to_slot), remove) {
            Ok(copied) => effects.extend(copied),
            Err(error) => {
                let detail = format!("paste stopped after {done} of {}: {error}", pairs.len());
                effects.push(Effect::Notice(Subsystem::Library, detail));
                break;
            }
        }
    }
    Ok(effects)
}

/// The search index refresh of a copy and, when its hash rewrite failed, a notice: the copy
/// is done, but it keeps the source hash (review r21 H6).
fn copied_effects(copied: preset::Copied) -> Vec<Effect> {
    let notice = copied.hash_error.map(|error| {
        let detail = format!("copy {}: hash not rewritten: {error}", copied.path);
        Effect::Notice(Subsystem::Library, detail)
    });
    stale_index(copied.search_index)
        .into_iter()
        .chain(notice)
        .collect()
}

/// The writable `zenui.db` of the library.
fn user_database(state: &mut State) -> Result<&mut tetrad_storage::stock::StockDb, ActionError> {
    state
        .library
        .as_mut()
        .and_then(|library| library.database.as_mut())
        .ok_or(ActionError::Unavailable("zenui.db"))
}

/// Applies a settings change: state that mirrors settings, one
/// [`Effect::SettingsChanged`] per changed group, then the stock debounced flush.
pub fn update_settings(state: &mut State, change: impl FnOnce(&mut Settings)) -> Vec<Effect> {
    let events = state.settings.update(change);
    let mut effects = mirror_settings(state, events);
    effects.extend(settings_flush(&state.settings));
    effects
}

/// DSP `0x04` MasterVolumeChange (`0x511bb0`): `10^((v·40−40)/20)`, exactly 0 at v = 0.
pub fn master_volume(level: f32) -> Effect {
    let gain = if level <= 0.0 {
        0.0
    } else {
        10_f32.powf((level * 40.0 - 40.0) / 20.0)
    };
    Effect::Dsp(DspCommand::Request(
        tetrad_dsp_protocol::requests::Request::MasterVolumeChange {
            gain_bits: gain.to_bits(),
        },
    ))
}

/// Scoped BPM (`0x64c6e0`), with the active MIDI clock taking precedence.
pub fn tempo_bpm(state: &State) -> i32 {
    state.clock_bpm.unwrap_or_else(|| scoped_bpm(state))
}

/// The preset or global tempo (`0x64c6e0` while no MIDI clock runs).
pub fn scoped_bpm(state: &State) -> i32 {
    super::features::scoped_value(state, 0)
        .map_or(120, |value| (40.0 + value * 200.0).round() as i32)
}

/// The flush timer after a settings write (`SettingsFileId::save_delay`, longest dirty file).
pub fn settings_flush(settings: &DeviceSettings) -> Option<Effect> {
    settings
        .dirty_files()
        .map(|file| file.save_delay())
        .max()
        .map(|after| Effect::StartTimer {
            timer: Timer::SettingsFlush,
            after,
        })
}

/// A block bypass at the current scene from `source` (`SetBypass`, `MomentaryBypass`).
fn write_bypass(
    state: &mut State,
    site: Site,
    bypassed: bool,
    source: ChangeSource,
) -> Result<Effect, ActionError> {
    let mode = bypass_mode(state);
    let (specs, current, commits) = loaded_bypass(state)?;
    Ok(Effect::Deploy(match site {
        Site::Slot(slot) => commits.write(current, specs, slot, bypassed, source, mode)?,
        site => preset::set_bypass(current, specs, site, bypassed)?,
    }))
}

/// Keeps the performance state that mirrors settings in step and reports each group.
fn mirror_settings(state: &mut State, events: Vec<SettingsEvent>) -> Vec<Effect> {
    let platform = state.platform;
    let footswitch = &mut state.performance.footswitch;
    for event in &events {
        match event {
            SettingsEvent::Mode(modes) => {
                if let Some(mode) = core_mode(modes.mode()) {
                    footswitch.mode = mode;
                }
                footswitch.bank_size = super::input::bank_size(platform, modes);
            }
            SettingsEvent::General(general) => {
                footswitch.gig_view_access = general.gig_view_stomp_access;
            }
            _ => {}
        }
    }
    events.into_iter().map(Effect::SettingsChanged).collect()
}

/// `Mode/mode` as the tetrad-core mode (both enums use the stock numbers 0..=8).
pub fn core_mode(mode: StoredMode) -> Option<CoreMode> {
    let id = ModeId::new(u8::try_from(mode.to_stock()).ok()?).ok()?;
    CoreMode::try_from(id).ok()
}

fn set_mode(state: &mut State, mode: StoredMode) -> Result<Vec<Effect>, ActionError> {
    let mut result = Ok(());
    let effects = update_settings(state, |settings| result = settings.mode.set_mode(mode));
    result?;
    Ok(effects)
}

/// The scene recall of the selected scene (`0x527248`) with the metronome resend, after the
/// Cortex Control pushes of the recall ([`super::control::scene_pushes`]).
fn scene_recall(state: &mut State) -> Vec<Effect> {
    // `0x5899d8`: the Scene push goes before the scene-changed signal that recalls; the
    // recall's Looper and Grid pushes follow it.
    let mut effects = super::control::scene_pushes(state);
    let metronome = super::features::metronome_resend(state);
    if let Err(error) = &metronome {
        effects.push(Effect::Notice(
            error.subsystem(),
            format!("metronome resend skipped in the recall: {error}"),
        ));
    }
    effects.push(Effect::RecallScene(metronome.ok()));
    effects.push(Effect::StopTimer(Timer::MetronomeFlush));
    effects.extend(refresh_expression(state, true));
    effects
}

fn screen(screen: Screen, show: bool) -> Effect {
    Effect::UiInput(if show {
        UiInput::Show(screen)
    } else {
        UiInput::Hide(screen)
    })
}

/// A SWAP of two slots ([`Action::SwapPresets`]); returns the effects and the two slots as
/// they are after the swap (the swap undo frame, `0x648a24`). In one setlist the slots trade
/// files (`StockLibrary::swap_slots`). Between two setlists each preset becomes a new file in
/// the other one (`StockLibrary::swap_between`); the loaded preset's path follows its file.
/// Either way the loaded side then reloads or follows its file ([`loaded_swapped`]).
fn swap_presets(
    state: &mut State,
    sides: [(String, usize); 2],
    reload: bool,
) -> Result<(Vec<Effect>, [preset::SwapSide; 2]), ActionError> {
    let mut effects = Vec::new();
    let [(from, slot), (to, to_slot)] = &sides;
    if from == to {
        let path = state
            .current
            .as_ref()
            .and_then(|current| current.path.clone());
        let presets = library(state)?;
        let old = presets.slots(from)?;
        let loaded = [*slot, *to_slot]
            .iter()
            .position(|&slot| path.is_some() && old.get(slot).cloned().flatten() == path);
        presets.swap_slots(from, *slot, *to_slot)?;
        let slots = presets.slots(from)?;
        // The file stays; it now sits in the other slot.
        if let Some(side) = loaded {
            effects.extend(loaded_swapped(state, &sides, side, reload));
        }
        let done = sides.map(|(setlist, slot)| preset::SwapSide {
            path: slots.get(slot).cloned().flatten(),
            setlist,
            slot,
        });
        return Ok((effects, done));
    }
    let held = |library: &StockLibrary, (setlist, slot): &(String, usize)| {
        library.slots(setlist).ok()?.get(*slot).cloned().flatten()
    };
    let presets = library(state)?;
    let old = sides.each_ref().map(|side| held(presets, side));
    // The swap side that held the loaded preset.
    let loaded = (state.current.as_ref())
        .and_then(|current| (old.iter()).position(|old| old.is_some() && *old == current.path));
    let swapped = match library(state)?.swap_between(from, *slot, to, *to_slot) {
        Ok(swapped) => swapped,
        // Review r30 C2: the swap can fail after both setlists changed. The other side's
        // slot then names the loaded preset's new file; when it did not change, a path
        // that no slot names is dropped, so the next save asks for a target (O).
        Err(error) => {
            if let Some(side) = loaded {
                let path = state
                    .current
                    .as_ref()
                    .and_then(|current| current.path.clone());
                let library = library(state)?;
                let moved = held(library, &sides[1 - side])
                    .filter(|now| Some(now) != old[1 - side].as_ref());
                let named = |path: &str| {
                    (library.setlists().into_iter().flatten()).any(|setlist| {
                        (library.slots(&setlist.name).into_iter().flatten())
                            .any(|slot| slot.as_deref() == Some(path))
                    })
                };
                let path = moved.or(path.filter(|path| named(path)));
                if let Some(current) = state.current.get_mut().as_mut() {
                    current.path = path;
                }
            }
            return Err(error.into());
        }
    };
    effects.extend(stale_index(swapped.search_index));
    // The swap is done; a failed later step (favorites, recents, old files) is named.
    if let Some(error) = &swapped.tail_error {
        let detail = format!("swap done, a later step failed: {error}");
        effects.push(Effect::Notice(Subsystem::Library, detail));
    }
    // `value[i]`: the new file of the preset that was in pair side `i`.
    let [into_to, into_from] = swapped.value;
    if let Some(side) = loaded {
        if let Some(current) = state.current.get_mut().as_mut() {
            current.path = Some([&into_to, &into_from][side].clone());
        }
        effects.extend(loaded_swapped(state, &sides, side, reload));
    }
    let [(from, slot), (to, to_slot)] = sides;
    let done = [
        preset::SwapSide {
            setlist: from,
            slot,
            path: Some(into_from),
        },
        preset::SwapSide {
            setlist: to,
            slot: to_slot,
            path: Some(into_to),
        },
    ];
    Ok((effects, done))
}

/// The swap side `side` held the loaded preset; its file is now in the other side's slot.
/// Review r30 C1 and review r36 H1: stock reloads the loaded slot when it is either side of
/// the swap, in one setlist or between two (`0x1e5a58` → `0x971180`), so the other preset
/// plays there. Review r31 D7: only a known clean preset reloads (no clean snapshot keeps
/// it). O: a preset with unsaved edits, and an UNDO or REDO (`reload` false), follow the file
/// instead ([`follow_loaded`]), so the edits stay.
fn loaded_swapped(
    state: &mut State,
    sides: &[(String, usize); 2],
    side: usize,
    reload: bool,
) -> Vec<Effect> {
    let clean = (state.clean.as_ref())
        .zip(state.current.as_ref())
        .is_some_and(|(clean, current)| clean.matches(current));
    if !(reload && clean) {
        return follow_loaded(state, &sides[1 - side]).into_iter().collect();
    }
    let (setlist, index) = &sides[side];
    vec![Effect::LoadPreset(PresetRef {
        setlist: setlist_key(state, Some(setlist.clone())),
        index: *index as u32,
    })]
}

/// Review r35 G3: the loaded preset's file moved to `slot` and does not reload (a swap UNDO
/// or REDO, an edited preset). `LastPreset` and the footswitch index follow the file, as
/// after a Directory move, so the next save and the LEDs name the right slot (O; stock does
/// not read the pointer after an UNDO).
fn follow_loaded(state: &mut State, (setlist, index): &(String, usize)) -> Option<Effect> {
    let slot = PresetRef {
        setlist: setlist_key(state, Some(setlist.clone())),
        index: *index as u32,
    };
    state.select_slot(&slot);
    settings_flush(&state.settings)
}

/// Undo or redo of a library swap frame (`0x649330` case 2 → `0x1e3824`, then the swap push
/// `0x2519e8`). Stock swaps again with no check and no reload. Owned: when a slot of the
/// frame no longer names the file the swap left there, nothing is written, a notice names
/// the slot and the frame drops; a failed swap keeps the frame. The loaded preset plays on
/// and its path follows its file. REDO swaps once (stock `0x649b7c` calls `0x1e3824` twice;
/// I: a bug).
fn step_swap(state: &mut State, redo: bool) -> Result<Vec<Effect>, ActionError> {
    let Some(sides) = state.history.newest_swap(redo).cloned() else {
        return Ok(Vec::new());
    };
    let presets = library(state)?;
    let changed = sides.iter().find(|side| {
        let now = presets
            .slots(&side.setlist)
            .ok()
            .map(|slots| slots.get(side.slot).cloned());
        now != Some(Some(side.path.clone()))
    });
    if let Some(side) = changed {
        let step = if redo { "redo" } else { "undo" };
        let detail = format!(
            "swap {step} refused: slot {} of setlist {} changed since the swap",
            side.slot, side.setlist
        );
        state.history.step_swap(redo, None);
        return Ok(vec![Effect::Notice(Subsystem::Library, detail)]);
    }
    let [a, b] = sides.map(|side| (side.setlist, side.slot));
    let (mut effects, done) = swap_presets(state, [a, b], false)?;
    match super::control::swap_push(state, &done) {
        Ok(push) => effects.push(push),
        Err(error) => effects.push(Effect::Notice(Subsystem::Control, error.to_string())),
    }
    state.history.step_swap(redo, Some(done));
    Ok(effects)
}

/// Undo or redo of a delete or copy/move frame (`0x649330` cases 1 and 3, restore
/// `0x647fa8`): each slot gets back what it held, and the reverse frame (the slots as they
/// were, `0x21e42c`) goes onto the other stack. Owned rules, user data first:
/// - every slot must still name the file the operation left there, else nothing is written,
///   a notice names the slot and the frame drops (stock has no check);
/// - presets are put back before slots are emptied; a failure stops the step, the slots not
///   done stay in the frame and the done ones form the reverse frame;
/// - the reverse frame is read first; a failed read refuses as a changed slot does (review
///   r39 K7);
/// - the loaded preset never names a file it does not hold: a move takes it along (its path
///   follows the moved file); else when its file goes or is written over, it plays on with
///   no file (the next save asks for a target). A deleted file that comes back is its file
///   again only when the slot held that very preset ([`preset::LoadId`]) and it has no file
///   since (review r39 K1: never by path; another preset may hold the old name by then).
///
/// When the step changed the selected slot and the loaded preset did not follow its file
/// there, stock plays the frame's working preset in that slot (`0x647fa8` → `0x5b2ce8`):
/// [`Effect::RestoreSelected`].
fn step_slots(state: &mut State, redo: bool) -> Result<Vec<Effect>, ActionError> {
    let Some(UndoFrame::Slots {
        operation,
        slots,
        working,
        owner,
    }) = state.history.newest(redo).cloned()
    else {
        return Ok(Vec::new());
    };
    let step = if redo { "redo" } else { "undo" };
    let path = (state.current.as_ref()).and_then(|current| current.path.clone());
    let id = state.history.loaded();
    let loaded = path.as_deref().map(|path| (path, id));
    // The reverse frame holds the working preset as it is now (`0x649b7c`, `0x21e42c`).
    let now = (state.current.as_ref()).map(|current| Box::new(preset::Snapshot::of(current)));
    let selected: Vec<bool> = (slots.iter())
        .map(|slot| is_selected(state, &slot.setlist, slot.slot))
        .collect();
    let library = library(state)?;
    let changed = slots.iter().find(|slot| {
        let now = library.slots(&slot.setlist).ok();
        now.map(|now| now.get(slot.slot).cloned()) != Some(Some(slot.expect.clone()))
    });
    if let Some(slot) = changed {
        let detail = format!(
            "{step} refused: slot {} of setlist {} changed since",
            slot.slot, slot.setlist
        );
        state.history.step_library(redo, None, None);
        return Ok(vec![Effect::Notice(Subsystem::Library, detail)]);
    }
    let reverse: Result<Vec<_>, _> = (slots.iter())
        .map(|slot| SlotState::read(library, &slot.setlist, slot.slot, loaded))
        .collect();
    let mut reverse = match reverse {
        Ok(reverse) => reverse,
        Err(error) => {
            state.history.step_library(redo, None, None);
            let detail = format!("{step} refused: {error}");
            return Ok(vec![Effect::Notice(Subsystem::Library, detail)]);
        }
    };
    let mut order: Vec<usize> = (0..slots.len()).collect();
    order.sort_by_key(|&index| slots[index].held.is_none());
    let mut effects = Vec::new();
    let mut done = vec![false; slots.len()];
    let mut failed = None;
    for index in order {
        match put_back(library, &slots[index]) {
            Ok((after, put)) => {
                effects.extend(put);
                reverse[index].expect = after;
                done[index] = true;
            }
            Err(error) => {
                failed = Some(error);
                break;
            }
        }
    }
    // The file each slot names after the step (`None` for a slot not done).
    let after = |index: usize| done[index].then(|| reverse[index].expect.clone()).flatten();
    let moved = operation == SlotOperation::CopyOrMove && slots.len() == 2;
    // The loaded preset's next file: `Some` when the step changed it.
    let follow = match path {
        Some(_) => {
            let index = slots.iter().position(|slot| slot.expect == path);
            index
                .filter(|&index| done[index])
                .map(|index| if moved { after(1 - index) } else { None })
        }
        None => {
            let held = |slot: &SlotState| {
                slot.held
                    .as_ref()
                    .is_some_and(|held| held.loaded == Some(id))
            };
            let index = slots.iter().position(held);
            index.filter(|&index| done[index]).map(after)
        }
    };
    let followed = matches!(follow, Some(Some(_)));
    if let Some(path) = follow
        && let Some(current) = state.current.get_mut().as_mut()
    {
        current.path = path;
    }
    if !followed && (0..slots.len()).any(|index| done[index] && selected[index]) {
        effects.push(Effect::RestoreSelected {
            redo,
            working: working.clone(),
            owner,
        });
    }
    // Review r41 L1: a preset put back at its own path keeps its load frames.
    for (index, slot) in slots.iter().enumerate() {
        if let Some(held) = &slot.held
            && let Some(from) = &held.file
            && after(index).as_ref() == Some(&held.path)
        {
            state
                .history
                .refile(from, PresetFile::read(from.host.clone()));
        }
    }
    let part = |done_part: bool, states: Vec<SlotState>, working, owner| {
        let states: Vec<_> = (states.into_iter().zip(&done))
            .filter(|(_, done)| **done == done_part)
            .map(|(state, _)| state)
            .collect();
        (!states.is_empty()).then_some(UndoFrame::Slots {
            operation,
            slots: states,
            working,
            owner,
        })
    };
    let keep = (failed.is_some())
        .then(|| part(false, slots, working, owner))
        .flatten();
    state
        .history
        .step_library(redo, keep, part(true, reverse, now, id));
    if let Some(error) = failed {
        let detail = format!("{step} stopped: {error}");
        effects.push(Effect::Notice(Subsystem::Library, detail));
    }
    Ok(effects)
}

/// Puts one slot back as it was (`0x647fa8`): a held preset goes in as a copy (the occupant
/// goes, `vt+0x4c`; add `vt+0x54`) and back into the preset favorites when it was there; an
/// empty state empties the slot. Returns the file the slot names after it.
fn put_back(
    library: &mut StockLibrary,
    slot: &SlotState,
) -> Result<(Option<String>, Vec<Effect>), ActionError> {
    let Some(held) = &slot.held else {
        if slot.expect.is_none() {
            return Ok((None, Vec::new()));
        }
        let saved = library.delete(&slot.setlist, slot.slot)?;
        return Ok((None, stale_index(saved.search_index).into_iter().collect()));
    };
    let copied =
        preset::copy_source_into(library, held.source.clone(), &slot.setlist, Some(slot.slot))?;
    let path = copied.path.clone();
    let mut effects = copied_effects(copied);
    if held.favorite {
        let entry = ItemRef {
            folder: path
                .rsplit_once('/')
                .map_or("", |(folder, _)| folder)
                .to_owned(),
            item: path.clone(),
        };
        if let Err(error) = library.push_item(ItemList::Favorites(Collection::Presets), &entry) {
            let detail = format!("{path}: favorite not restored: {error}");
            effects.push(Effect::Notice(Subsystem::Library, detail));
        }
    }
    Ok((Some(path), effects))
}

/// Undo or redo: the frame, after the capture-name pass (`0x7c2da4`), replaces the loaded
/// preset (`0x527d80`); a layout difference deploys it ([`Effect::RestoreLayout`] moves the
/// frame only when the deploy commits).
/// Cortex Control then gets the restored preset (`super::control::undo_pushes`). A library
/// frame is [`Action::StepLibrarySwap`] or [`Action::StepLibraryFrame`], so bring-up can
/// refuse its write; a load frame is [`Effect::StepPresetLoad`] (the load rules apply).
fn undo(state: &mut State, redo: bool) -> Result<Vec<Effect>, ActionError> {
    match state.history.newest(redo) {
        Some(UndoFrame::LibrarySwap(_)) => {
            return Ok(vec![Effect::Action(Action::StepLibrarySwap { redo })]);
        }
        Some(UndoFrame::Slots { .. }) => {
            return Ok(vec![Effect::Action(Action::StepLibraryFrame { redo })]);
        }
        Some(UndoFrame::PresetLoad(_)) => return Ok(vec![Effect::StepPresetLoad { redo }]),
        Some(UndoFrame::Preset { .. }) | None => {}
    }
    let (Some(specs), Some(current)) = (state.specs.as_ref(), state.current.get_mut().as_mut())
    else {
        return Err(ActionError::Unavailable(
            "a loaded preset and the model catalog",
        ));
    };
    let before = state.history.depths();
    let mut history = state.history.clone();
    let Some(mut frame) = history.step(current, redo) else {
        return Ok(Vec::new());
    };
    // The restore `0x527d80` commits every bypass (`0x2e0e38(preset, 1)`).
    state.perform.bypass.clear();
    super::control::refresh_capture_names(&mut frame.preset, &state.capture_names);
    match preset::Restore::prepare(current, specs, frame)? {
        preset::Restore::Preset(preset) => {
            current.preset = *preset;
            state.history = history;
            Ok(super::control::undo_pushes(state, before, redo).into())
        }
        preset::Restore::Layout(change) => Ok(vec![Effect::RestoreLayout { change, redo }]),
    }
}

fn stale_index(stale: Option<tetrad_library::stock::StaleSearchIndex>) -> Option<Effect> {
    stale.map(|stale| Effect::Notice(Subsystem::Library, stale.to_string()))
}

/// The stored model of a grid block in the loaded preset.
fn stored_block(state: &mut State, slot: Slot) -> Result<&mut tetrad_preset::Model, ActionError> {
    Ok(current(state)?.preset.block_mut(tetrad_preset::Slot {
        row: slot.row.index(),
        column: slot.column.index(),
    })?)
}

fn current(state: &mut State) -> Result<&mut Current, ActionError> {
    state
        .current
        .get_mut()
        .as_mut()
        .ok_or(ActionError::Unavailable("a loaded preset"))
}

/// The catalog and the loaded preset together (disjoint borrows).
fn loaded(state: &mut State) -> Result<(&tetrad_catalog::Specs, &mut Current), ActionError> {
    match (state.specs.as_ref(), state.current.get_mut().as_mut()) {
        (Some(specs), Some(current)) => Ok((specs, current)),
        (None, _) => Err(ActionError::Unavailable("the model catalog")),
        (_, None) => Err(ActionError::Unavailable("a loaded preset")),
    }
}

fn library(state: &mut State) -> Result<&mut tetrad_library::stock::StockLibrary, ActionError> {
    state
        .library
        .as_mut()
        .map(|library| &mut library.presets)
        .ok_or(ActionError::Unavailable("the preset library"))
}

/// Stock writes eight scene entries; shorter stored lists grow on edit.
fn pad<T: Default + Clone>(values: &mut Vec<T>) -> &mut Vec<T> {
    if values.len() < tetrad_engine::SCENES {
        values.resize(tetrad_engine::SCENES, T::default());
    }
    values
}

/// The grid slots assigned to a stomp (`stomp_mode_assignments`, field 27).
pub fn stomp_slots(preset: &tetrad_preset::Preset, stomp: u32) -> impl Iterator<Item = Slot> + '_ {
    preset
        .stomp_mode_assignments
        .iter()
        .filter(move |assignment| assignment.stomp_index == stomp)
        .filter_map(|assignment| {
            Some(Slot {
                row: Row::new(u8::try_from(assignment.row).ok()?).ok()?,
                column: Column::new(u8::try_from(assignment.column).ok()?).ok()?,
            })
        })
}

/// Stored momentary flag (field 35), off when a 27000 block is assigned (`0x514598`).
fn momentary(state: &State, stomp: StompIndex) -> bool {
    let Some(current) = &*state.current else {
        return false;
    };
    let key = u32::from(stomp.get());
    let layout = current.deployment.layout();
    let non_momentary = stomp_slots(&current.preset, key).any(|slot| {
        layout
            .instance(Site::Slot(slot))
            .is_some_and(|block| block.model.0 == NON_MOMENTARY_MODEL)
    });
    let requested = current
        .preset
        .stomp_is_momentary
        .get(key)
        .copied()
        .unwrap_or(false);
    effective_momentary(requested, non_momentary)
}

/// The writes of a stomp press (`press: Some(momentary)`, [`stomp_press`]) or a momentary
/// release (`None`, [`momentary_release`]) over the blocks assigned to `stomp`, read at the
/// current scene. Bypass writes follow Scene Bypass Behaviour; a secondary toggle is a saved
/// touch-source parameter write (`0x52382c`). Unsupported blocks (hook `+0x40`, plugin stomp
/// handlers) are not modelled.
/// The assignment column `0x523f80` hands to `0x2fa254` (`0xfa4`).
const SPECIAL_COLUMN: u32 = 0xfa4;

fn stomp_writes(
    state: &mut State,
    stomp: StompIndex,
    press: Option<bool>,
) -> Result<Vec<Effect>, ActionError> {
    let mode = bypass_mode(state);
    let (specs, current, commits) = loaded_bypass(state)?;
    let scene = current.deployment.scene();
    let layout = current.deployment.layout();
    let mut secondary_index = BTreeMap::new();
    // `0x523f80` also drives an assignment at column 4004 through `0x2fa254`, whose target is
    // not recovered (review r14 P-8).
    let mut effects: Vec<Effect> = (current.preset.stomp_mode_assignments.iter())
        .filter(|a| a.stomp_index == u32::from(stomp.get()) && a.column == SPECIAL_COLUMN)
        .map(|_| {
            Effect::Notice(
                Subsystem::Preset,
                "stomp assignment at column 4004 (0x2fa254) is not supported".into(),
            )
        })
        .collect();
    let blocks: Vec<AssignedBlock> = current
        .preset
        .stomp_mode_assignments
        .iter()
        .filter(|assignment| assignment.stomp_index == u32::from(stomp.get()))
        .filter_map(|assignment| {
            let slot = Slot {
                row: Row::new(u8::try_from(assignment.row).ok()?).ok()?,
                column: Column::new(u8::try_from(assignment.column).ok()?).ok()?,
            };
            let block = layout.instance(Site::Slot(slot))?;
            let spec = specs.model(block.model.0);
            // A block whose driver takes the footswitch (Freeze, `0x55acd8`/`0x55ac5c`)
            // keeps its bypass: the device bridge sends it the press and release.
            let category = spec.map_or(u32::MAX, |spec| spec.category_id);
            if tetrad_dsp_host::setup::driver_kind(block.model.0, category)
                .is_some_and(tetrad_dsp_host::setup::DriverKind::takes_footswitch)
            {
                return None;
            }
            let index = spec.and_then(|spec| spec.secondary_stomp_parameter);
            let address = BlockAddress {
                row: slot.row.index() as u8,
                column: slot.column.index() as u8,
            };
            secondary_index.extend(index.map(|index| (address, (slot, index))));
            Some(AssignedBlock {
                block: address,
                secondary: !matches!(
                    assignment.role,
                    None | Some(tetrad_preset::StompRole::Primary)
                ),
                // Unknown models never take the first pass.
                category,
                bypassed: block.bypass.at(scene),
                secondary_value: index
                    .and_then(|index| block.params.get(index as usize))
                    .map(|value| value.at(scene)),
            })
        })
        .collect();
    // `0x5239d0` with no blocks records the source and signals only (review r14 P-5).
    if blocks.is_empty() {
        return Ok(effects);
    }
    let writes = match press {
        Some(momentary) => stomp_press(&blocks, momentary),
        None => momentary_release(&blocks),
    };
    let slot_of = |address: BlockAddress| Slot {
        row: Row::new(address.row).expect("an assigned row"),
        column: Column::new(address.column).expect("an assigned column"),
    };
    for write in writes {
        let result = match write {
            StompWrite::Bypass {
                block,
                bypassed,
                source,
            } => commits.write(current, specs, slot_of(block), bypassed, source, mode),
            StompWrite::Settle => {
                effects.push(Effect::Sleep(STOMP_SETTLE));
                continue;
            }
            StompWrite::Secondary { block, value } => {
                let (slot, index) = secondary_index[&block];
                preset::set_parameter(current, specs, Site::Slot(slot), index, value)
            }
        };
        match result {
            Ok(update) => effects.push(Effect::Deploy(update)),
            // The blocks written so far changed in the preset: they must reach the DSP too.
            Err(error)
                if effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::Deploy(_))) =>
            {
                effects.push(Effect::Notice(
                    Subsystem::Preset,
                    format!("stomp {} stopped at {write:?}: {error}", stomp.get()),
                ));
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(effects)
}

/// `bypassSaveMode` as the tetrad-core overwrite mode.
fn bypass_mode(state: &State) -> BypassOverwriteMode {
    match state.settings.settings().general.bypass_save_mode {
        BypassSaveMode::Always => BypassOverwriteMode::Always,
        BypassSaveMode::NonStomp => BypassOverwriteMode::NonStomp,
        BypassSaveMode::Never => BypassOverwriteMode::Never,
    }
}

/// [`loaded`] with the committed bypass records (disjoint borrows).
fn loaded_bypass(
    state: &mut State,
) -> Result<
    (
        &tetrad_catalog::Specs,
        &mut Current,
        &mut preset::BypassCommits,
    ),
    ActionError,
> {
    match (state.specs.as_ref(), state.current.get_mut().as_mut()) {
        (Some(specs), Some(current)) => Ok((specs, current, &mut state.perform.bypass)),
        (None, _) => Err(ActionError::Unavailable("the model catalog")),
        (_, None) => Err(ActionError::Unavailable("a loaded preset")),
    }
}

/// User junction defaults (`0x668354`) when an edit sets a split or mix point; read at the
/// edit, as stock does. A read failure keeps the catalog default and is a notice.
fn junction_defaults(
    state: &State,
    edits: &[LayoutEdit],
    effects: &mut Vec<Effect>,
) -> preset::JunctionDefaults {
    let junction = edits.iter().any(|edit| {
        matches!(
            edit,
            LayoutEdit::Split {
                column: Some(_),
                ..
            } | LayoutEdit::Mix {
                column: Some(_),
                ..
            }
        )
    });
    let database = state
        .library
        .as_ref()
        .and_then(|library| library.database.as_ref());
    let (true, Some(specs), Some(database)) = (junction, state.specs.as_ref(), database) else {
        return preset::JunctionDefaults::default();
    };
    preset::JunctionDefaults::read(specs, database).unwrap_or_else(|error| {
        effects.push(Effect::Notice(
            Subsystem::Library,
            format!("junction user defaults: {error}"),
        ));
        preset::JunctionDefaults::default()
    })
}

/// Parameter 5 of a capture block: the capture key, then its name.
const CAPTURE_FILE: u32 = 5;

/// BANK UP on a Neural Capture block (`0x4f50dc`): the key of parameter 5 at the current
/// scene, the next capture of the model's version (14000: "1", else "2") in
/// [`State::capture_cycle`] ([`CaptureCycle::step`]; `backwards` = turn ≤ 0, `0x4f5f38`).
/// A new block of the same model (`0x519cc0`) takes the `default_param` values of the new key
/// (`0x66824c`), then parameter 5 = the row value in all eight scenes; it replaces the block
/// (`0x9a8608`). An empty key or a miss is only a log, as in stock.
fn cycle_capture(state: &mut State, slot: Slot, step: i32) -> Result<Vec<Effect>, ActionError> {
    let log = |detail: &str| {
        let detail = format!("loadNextNeuralCapture: {detail}");
        Ok(vec![Effect::Notice(Subsystem::Input, detail)])
    };
    let cycle = state
        .capture_cycle
        .as_ref()
        .ok_or(ActionError::Unavailable("the Directory capture order"))?;
    let (Some(specs), Some(current)) = (state.specs.as_ref(), state.current.as_ref()) else {
        return Err(ActionError::Unavailable("a loaded preset"));
    };
    let scene = current.deployment.scene();
    let block = current
        .deployment
        .layout()
        .instance(Site::Slot(slot))
        .ok_or(ActionError::Unavailable("a block at that slot"))?;
    let model = block.model.0;
    let value = block
        .texts
        .iter()
        .find(|text| text.index == CAPTURE_FILE)
        .and_then(|text| {
            let at_scene = text.scenes.get(scene.index()).cloned().flatten();
            at_scene.or_else(|| text.scenes.iter().flatten().next().cloned())
        })
        .unwrap_or_default();
    let key = preset::capture_key(&value);
    if key.is_empty() {
        return log("NC hash is empty");
    }
    let version = if model == 14_000 { "1" } else { "2" };
    let next = match cycle.step(key, version, step <= 0) {
        Ok(next) => next,
        Err(CycleMiss::NoFolder) => return log(&format!("No folder found {key:?}")),
        Err(CycleMiss::NoNextKey) => return log(&format!("No next key found {key:?}")),
    };
    let rows = match state
        .library
        .as_ref()
        .and_then(|library| library.database.as_ref())
    {
        Some(database) => database
            .view()
            .default_params(preset::capture_key(&next.value))?,
        None => Vec::new(),
    };
    let mut block = preset::model_preset_block(specs, model, None, &rows)
        .ok_or(ActionError::Unavailable("that model in the catalog"))?;
    if !block.set_text(CAPTURE_FILE, scene, &next.value) {
        return Err(ActionError::Unavailable("the capture parameter"));
    }
    reduce(
        Action::EditLayout(LayoutEdit::Paste {
            slot,
            block: Box::new(block),
        }),
        state,
    )
}

/// The slot a block add or model change sets (stock model setter `0x522d70`).
fn new_block(edit: &LayoutEdit) -> Option<Slot> {
    match edit {
        LayoutEdit::Add { slot, .. } | LayoutEdit::Replace { slot, .. } => Some(*slot),
        _ => None,
    }
}

/// The slot a load request loads: `target` with its setlist key, unless `reload` is false and
/// it is the loaded slot.
fn load_target(state: &State, mut target: PresetRef, reload: bool) -> Option<PresetRef> {
    target.setlist = setlist_key(state, target.setlist);
    let loaded = PresetRef {
        setlist: state.settings.settings().last_preset.setlist.clone(),
        index: state.performance.footswitch.current_preset,
    };
    (reload || state.current.is_none() || target != loaded).then_some(target)
}

/// Whether `LastPreset` selects slot `slot` of setlist `setlist` (a name): the stock current
/// folder (`0x7c4ba4` `+8`) and index (`0x7c28cc`) that `0x7c4c1c` and `0x5b2ce8` compare.
fn is_selected(state: &State, setlist: &str, slot: usize) -> bool {
    let last = &state.settings.settings().last_preset;
    let Some(library) = state.library.as_ref() else {
        return false;
    };
    usize::try_from(last.preset_index) == Ok(slot)
        && preset::setlist(&library.presets, last.setlist.as_deref())
            .is_ok_and(|selected| selected.name == setlist)
}

/// The stock `LastPreset/setlist` form of a setlist key: the setlist directory path
/// (`/media/p4/Presets/My Presets`, device `ZenjackConfig.ini`). A setlist name becomes its
/// path; `None` (the default setlist), a path and an unknown name stay as given.
fn setlist_key(state: &State, key: Option<String>) -> Option<String> {
    let key = key?;
    if key.starts_with('/') {
        return Some(key);
    }
    let path = state
        .library
        .as_ref()
        .and_then(|library| library.presets.setlists().ok())
        .and_then(|setlists| setlists.into_iter().find(|setlist| setlist.name == key))
        .map(|setlist| setlist.path);
    Some(path.unwrap_or(key))
}

/// What a setting change sends beyond the stored value. DDC and the master volume
/// assignment are DSP globals (`0x4c`, `0x3a`); the global cab/IR rows apply at the next
/// push. Tuner settings use their own ordered path. The LED brightness settings reach the
/// LEDs at the next present (`leds.rs`); the screen brightness sets the backlight.
fn setting_sends(id: SettingId, state: &State) -> Vec<Effect> {
    match id {
        SettingId::LatencyCompensation
        | SettingId::MasterVolumeOut12
        | SettingId::MasterVolumeOut34
        | SettingId::MasterVolumeSend12
        | SettingId::MasterVolumeHeadphones => vec![Effect::Dsp(DspCommand::Globals)],
        SettingId::GlobalBypassCab(_) | SettingId::GlobalBypassIr(_) => vec![
            Effect::Dsp(DspCommand::Globals),
            Effect::Notice(
                Subsystem::Dsp,
                "global cab/IR bypass rows apply at the next preset push".into(),
            ),
        ],
        SettingId::ScreenBrightness => vec![Effect::Backlight(
            state.settings.settings().display.brightness.get(),
        )],
        _ => Vec::new(),
    }
}

/// Be Right Back (`0x6a82ac`), stock order. On: 48 V off where an ESS board uses it
/// (`0x357cf8`, `0x357dd0`), headphones muted (`SetHeadPhoneVolume(0, mute)`), backlight 0,
/// DSP `0x20` (1, 1). Off: 48 V back, the screen brightness (`0x409888`), HP LEVEL (setting
/// 900, muted at 0), `0x20` (0, 0), then the knob read (`0x1fda34` is the MasterVolume
/// getter of `0x1fc518`). Every HAL write is unverified, as stock. The footswitch LEDs follow
/// `UiLeds::standby`; tetrad-ui drops touch.
///
/// Not modelled: the input mask (`0x1ec658`, `+8` starts at `0x3f`). Enter keeps "bit 0 was
/// clear" in `+0x30` (`0x1ec97c`) and clears bits 0 and 1 (`0x1ec83c(3)`); the wake sets
/// bit 1, and bit 0 only when it was set (`0x1ec714(2 or 3)`, a no-op unless bit `0x40` is
/// in the mask or the request). Tetrad has no mask: the only reader found, the knob
/// `0x1fb980`, tests bit `0x10`, which Be Right Back does not touch.
fn standby(state: &State, on: bool) -> Result<Vec<Effect>, ActionError> {
    use tetrad_hal::analog::Verification;
    use tetrad_hal::hal::Command;
    use tetrad_hal::requests::{Brightness, DisplayCommand};
    use tetrad_hal::zenjack::{Control, HeadphoneVolume, Input, InputType, Setting, Value};
    let set = |control, value| -> Result<Effect, ActionError> {
        let setting = Setting::new(control, value)?;
        Ok(Effect::Hal(Command::Set(setting, Verification::Unverified)))
    };
    let kind = if on {
        InputType::Xlr
    } else {
        InputType::XlrPhantom
    };
    let mut effects = Vec::new();
    for (input, phantom) in [
        (Input::One, ConnectorId::In1Phantom),
        (Input::Two, ConnectorId::In2Phantom),
    ] {
        if state.platform.has_ess_codec() && state.io.value(phantom) > 0.5 {
            effects.push(set(Control::InputType(input), Value::InputType(kind))?);
        }
    }
    if on {
        let off = Brightness::try_from(0).expect("0 is a stock brightness");
        effects.extend([
            set(
                Control::HeadphoneVolume,
                Value::HeadphoneVolume(HeadphoneVolume::Muted),
            )?,
            Effect::Hal(Command::Display(DisplayCommand::Brightness(off))),
            Effect::Dsp(DspCommand::MasterMute(1 | 1 << 1)),
        ]);
    } else {
        let level = state.io.value(ConnectorId::HpLevel).min(1.0);
        let volume = if level <= 0.0 {
            HeadphoneVolume::Muted
        } else {
            HeadphoneVolume::from_unit(level)?
        };
        effects.extend([
            Effect::Backlight(state.settings.settings().display.brightness.get()),
            set(Control::HeadphoneVolume, Value::HeadphoneVolume(volume))?,
            Effect::Dsp(DspCommand::MasterMute(0)),
            Effect::ReadVolume,
        ]);
    }
    Ok(effects)
}

/// The I/O page settings as connector values (`ConnectorsSettings` ids, w4-iosettings §1.1):
/// a level in tenths of dB snaps to the stock steps, a choice is its option index, a toggle
/// is 0 or 1. The tetrad-ui OUT 1/2 and OUT 3/4 rows drive the pair ids when the pair is linked,
/// else both mono jacks (inferred: the owned page has no per-jack rows).
fn io_setting(
    state: &State,
    id: SettingId,
    value: &SettingValue,
) -> Result<Option<Vec<(ConnectorId, f32)>>, ActionError> {
    use ConnectorId as C;
    use tetrad_ui::{InputJack, LoopJack, OutputJack};
    let io = &state.settings.settings().io;
    let outputs = |jack, pair: [C; 3]| match jack {
        OutputJack::Out12 if io.out12_paired => vec![pair[2]],
        OutputJack::Out34 if io.out34_paired => vec![pair[2]],
        _ => pair[..2].to_vec(),
    };
    let input = |jack, one, two| match jack {
        InputJack::In1 => one,
        InputJack::In2 => two,
    };
    let ids = match id {
        SettingId::InputLevel(jack) => vec![input(jack, C::In1Level, C::In2Level)],
        SettingId::InputImpedance(jack) => vec![input(jack, C::In1Impedance, C::In2Impedance)],
        SettingId::InputType(jack) => vec![input(jack, C::In1Type, C::In2Type)],
        SettingId::Phantom(jack) => vec![input(jack, C::In1Phantom, C::In2Phantom)],
        SettingId::InputGroundLift(jack) => {
            vec![input(jack, C::In1GroundLift, C::In2GroundLift)]
        }
        SettingId::OutputLevel(jack @ OutputJack::Out12) => {
            outputs(jack, [C::Out1Level, C::Out2Level, C::Out12Level])
        }
        SettingId::OutputLevel(jack @ OutputJack::Out34) => {
            outputs(jack, [C::Out3Level, C::Out4Level, C::Out34Level])
        }
        SettingId::OutputMute(jack @ OutputJack::Out12) => {
            outputs(jack, [C::Out1Mute, C::Out2Mute, C::Out12Mute])
        }
        SettingId::OutputMute(jack @ OutputJack::Out34) => {
            outputs(jack, [C::Out3Mute, C::Out4Mute, C::Out34Mute])
        }
        SettingId::OutputGroundLift(jack @ OutputJack::Out12) => outputs(
            jack,
            [C::Out1GroundLift, C::Out2GroundLift, C::Out12GroundLift],
        ),
        SettingId::OutputGroundLift(OutputJack::Out34) => {
            return Err(ActionError::Unsupported(
                "OUT 3/4 ground lift (stock has none)",
            ));
        }
        SettingId::SendLevel(LoopJack::One) => vec![C::Send1Level],
        SettingId::SendLevel(LoopJack::Two) => vec![C::Send2Level],
        SettingId::ReturnLevel(LoopJack::One) => vec![C::Ret1Level],
        SettingId::ReturnLevel(LoopJack::Two) => vec![C::Ret2Level],
        SettingId::ReturnGroundLift(LoopJack::One) => vec![C::Ret1GroundLift],
        SettingId::ReturnGroundLift(LoopJack::Two) => vec![C::Ret2GroundLift],
        SettingId::HeadphoneLevel => vec![C::HpLevel],
        SettingId::HeadphoneFeed(feed) => vec![hp_feed_connector(feed)],
        SettingId::HeadphoneSource => vec![C::HpSource],
        SettingId::UsbLevel => vec![C::UsbLevel],
        SettingId::UsbRouting => vec![C::UsbDryWet],
        _ => return Ok(None),
    };
    let hardware = state.settings.hardware();
    ids.into_iter()
        .map(|id| {
            let spec = id.spec(hardware).ok_or(ActionError::Unavailable(
                "this I/O setting on this hardware",
            ))?;
            let normalized = match value {
                SettingValue::Number(tenths) => spec.from_decibels(*tenths as f32 / 10.0),
                SettingValue::Choice(index) => spec.index_value(usize::from(*index)),
                SettingValue::Bool(on) => spec.index_value(usize::from(*on)),
                _ => None,
            };
            normalized
                .map(|value| (id, value))
                .ok_or(ActionError::Unsupported("I/O setting value"))
        })
        .collect::<Result<_, _>>()
        .map(Some)
}

/// The stored id of a Headphones feed knob (1700..1709).
pub(super) fn hp_feed_connector(feed: tetrad_ui::HpFeed) -> ConnectorId {
    use tetrad_ui::HpFeed;
    match feed {
        HpFeed::Out1 => ConnectorId::HpFeedOut1,
        HpFeed::Out2 => ConnectorId::HpFeedOut2,
        HpFeed::Out3 => ConnectorId::HpFeedOut3,
        HpFeed::Out4 => ConnectorId::HpFeedOut4,
        HpFeed::Send1 => ConnectorId::HpFeedSend1,
        HpFeed::Send2 => ConnectorId::HpFeedSend2,
        HpFeed::Out12 => ConnectorId::HpFeedOut12,
        HpFeed::MultiOut => ConnectorId::HpFeedMultiOut,
        HpFeed::Out34 => ConnectorId::HpFeedOut34,
        HpFeed::Send12 => ConnectorId::HpFeedSend12,
    }
}

/// The inverse of the UI settings projection. Values are checked before anything changes.
fn write_setting(
    settings: &mut Settings,
    id: SettingId,
    value: &SettingValue,
) -> Result<(), ActionError> {
    use SettingValue::{Bool, Choice, Number};
    let unsupported =
        || ActionError::Unsupported("this setting has no tetrad-settings mapping yet");
    let general = &mut settings.general;
    match (id, value) {
        (SettingId::ScreenBrightness, Number(v)) => {
            settings.display.brightness = ScreenBrightness::clamped(*v);
        }
        (SettingId::LedBrightness, Number(v)) => {
            settings.display.led_brightness = LedBrightness::clamped(*v);
        }
        (SettingId::DimmedLedBrightness, Number(v)) => {
            settings.display.dimmed_led_brightness = LedBrightness::clamped(*v);
        }
        (SettingId::PresetDimmed, Bool(v)) => settings.display.dimmed_preset = *v,
        (SettingId::SceneDimmed, Bool(v)) => settings.display.dimmed_scene = *v,
        (SettingId::StompDimmed, Bool(v)) => settings.display.dimmed_stomp = *v,
        (SettingId::PowerButtonSensitivity, Choice(v)) => {
            general.power_button_sensitivity = PowerButtonSensitivity::from_stock(i32::from(*v))?;
        }
        (SettingId::PowerOnMode, Choice(v)) => general.atma_power_on_automatic = *v == 1,
        (SettingId::MasterVolumeOut12, Bool(v)) => settings.master_volume.out12 = *v,
        (SettingId::MasterVolumeOut34, Bool(v)) => settings.master_volume.out34 = *v,
        (SettingId::MasterVolumeSend12, Bool(v)) => settings.master_volume.send12 = *v,
        (SettingId::MasterVolumeHeadphones, Bool(v)) => settings.master_volume.headphones = *v,
        (SettingId::InternetCheck, Bool(v)) => general.disable_connection_check = !*v,
        (SettingId::CloudDomain, Choice(v)) => {
            settings.cloud_domain = match v {
                0 => tetrad_settings::permanent::CloudDomain::Default,
                1 => tetrad_settings::permanent::CloudDomain::Alternative,
                _ => return Err(unsupported()),
            };
        }
        (SettingId::GlobalBypassCab(row), Bool(v)) => {
            settings.global_bypass.cab[usize::from(row.get())] = *v;
        }
        (SettingId::GlobalBypassIr(row), Bool(v)) => {
            settings.global_bypass.ir[usize::from(row.get())] = *v;
        }
        (SettingId::SceneBypassBehavior, Choice(v)) => {
            general.bypass_save_mode = match v {
                0 => BypassSaveMode::Always,
                1 => BypassSaveMode::NonStomp,
                2 => BypassSaveMode::Never,
                _ => return Err(unsupported()),
            };
        }
        (SettingId::StompModeAutoAssign, Bool(v)) => general.stomp_mode_auto_assign = *v,
        (SettingId::HoldTiming, Choice(v)) => {
            general.hold_timing = HoldTiming::new(i32::from(*v))?;
        }
        (SettingId::SwapTempoTuner, Bool(v)) => general.swap_tempo_and_tuner_access = *v,
        (SettingId::GigViewAccess, Bool(v)) => general.gig_view_stomp_access = *v,
        (SettingId::LatencyCompensation, Bool(v)) => general.dynamic_delay_compensation = *v,
        (SettingId::ReloadPreset, Bool(v)) => general.preset_reload = *v,
        (SettingId::ReloadScene, Bool(v)) => general.scene_reload = *v,
        (SettingId::MidiChannel, Choice(v)) => {
            // tetrad-ui index = stored channel − 1; index 16 is OMNI (stored 17).
            settings.midi.channel = if *v == 16 {
                MidiChannel::OMNI
            } else {
                MidiChannel::new(i32::from(*v) + 1)?
            };
        }
        (SettingId::MidiOverUsb, Bool(v)) => settings.midi.over_usb = *v,
        (SettingId::IgnoreDuplicatePc, Bool(v)) => settings.midi.ignore_duplicate_pcs = *v,
        (SettingId::MidiClockOut, Choice(v)) => {
            settings.midi.clock_out = MidiClockOut::from_stock(i32::from(*v))?;
        }
        (SettingId::MidiClockIn, Bool(v)) => settings.midi.clock_in = *v,
        (SettingId::MidiThru, Bool(v)) => settings.midi.thru = *v,
        (SettingId::TunerReference, Number(hz)) => {
            settings.tuner.set_reference_offset_hz(*hz as f32 - 440.0)?;
        }
        (SettingId::TunerMute, Bool(v)) => settings.tuner.mutes_outputs = *v,
        (SettingId::LiveTuner, Bool(v)) => settings.tuner.live_tuner = *v,
        (SettingId::TunerInput, Choice(v)) => {
            settings.tuner.input = *tetrad_settings::tuner::InputPortId::TUNER_CHOICES
                .get(usize::from(*v))
                .ok_or_else(unsupported)?;
        }
        (SettingId::DontShowExcessDownloads(kind), Bool(v)) => match kind {
            tetrad_ui::ExcessDownloads::Captures => general.dont_show_excess_capture_downloads = *v,
            tetrad_ui::ExcessDownloads::Presets => general.dont_show_excess_preset_downloads = *v,
        },
        _ => return Err(unsupported()),
    }
    Ok(())
}

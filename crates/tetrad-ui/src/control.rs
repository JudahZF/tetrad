//! Screens, interactive controls, input events and application requests.

use crate::{MappingScope, MediaKind};

use std::fmt;

use crate::settings::{DeviceAction, EqTab, IoTab, SettingId, SettingValue, SettingsPage};
use crate::state::REDACTED;
use crate::types::*;
use crate::view::{
    AuditionSource, CloudScope, FootswitchMode, HybridRows, InputPort, MediaFolder, ModelCategory,
    OutputPort, PreferredInstrument,
};
use crate::view::{DeviceGroup, ExpressionPedal, ExpressionSwitch, ExpressionTarget, Instrument};
use crate::view::{MidiOutMessage, MidiOutTarget};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Screen {
    #[default]
    Grid,
    Gig,
    Presets,
    ModelPicker,
    /// IR or Neural Capture list for one media parameter of the selected block.
    MediaPicker,
    /// Wi-Fi, updates, backups and about are settings pages, as in stock.
    Settings(SettingsPage),
    Io(IoTab),
    Tuner,
    Looper,
    Tempo,
    Capture,
    /// Full-screen expression pedal assignment for the selected block (shot 068).
    Expression,
    /// Preset MIDI Out settings (`MidiMatrixDialog`, shot 016) and its message editor. Tetrad
    /// opens only the editor, from a Send MIDI mapping ([`Screen::Mappings`]).
    MidiOut,
    /// Owned (Tetrad): the Mappings page of the grid ⋮ menu (`crate::mappings`).
    Mappings,
    /// Boot recovery menu (A + H held at startup, manual §12): "Choose a Recovery option".
    Recovery,
    /// I/O Settings → GLOBAL EQ (shot 009) on one band tab or OUT.
    GlobalEq(EqTab),
}

impl Screen {
    /// Main views share the top bar with mode, view and preset controls.
    pub fn is_main(self) -> bool {
        matches!(self, Self::Grid | Self::Gig | Self::Presets)
    }
}

/// Main-menu, block, preset, port, backup and network menu items. The menu
/// overlay holds the target; the item carries only the action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    CreateNew,
    SaveAs,
    EditDetails,
    CopyScene,
    SwapScene,
    /// Owned (Tetrad): opens [`Screen::Mappings`]; replaces Preset MIDI Out in the ⋮ menu.
    Mappings,
    ToggleFavorite,
    DeletePreset,
    NewNeuralCapture,
    Tempo,
    ModesConfiguration,
    CpuMonitor,
    Looper,
    Settings,
    ChangeDevice,
    AddDevice,
    CopyDevice,
    PasteDevice,
    ResetDefaults,
    AssignExpression,
    ToggleBypass,
    RemoveBlock,
    ChangeInput,
    ChangeOutput,
    Routing,
    InstallBackup,
    /// Backup ⋮ "Edit name" (`0xa3a778` → `0x3f8660`).
    EditBackupName,
    /// Backup ⋮ "Update" (`0xa3a784` → "Update Cloud Backup" `0x3f8730`).
    UpdateBackup,
    DeleteBackup,
    /// Looper X ⋮ (`0x94d14`): the swap dialog (`zenUI::LooperSwapActionsDialog`).
    ReassignLooperActions,
    /// Looper X ⋮: the factory assignments after a confirm (`0x4be114`).
    ResetLooperActions,
    /// Block ⋮ "Save Current Parameters As…" (`0xa1b074`).
    SaveDevicePreset,
    /// Block ⋮ "Set Parameters as Defaults" (`0xa32b44`).
    SetDefaults,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamControl {
    Knob(KnobIndex),
    /// The value text of a continuous knob (`0x4cbe7c`, rect 12,66 65×20): the numeric
    /// keyboard.
    Value(KnobIndex),
    /// A `zenUI::TabBar` tab (`0x4ba858`): parameters `10·page..` (`0x60c43c`).
    Page(u8),
    Bypass,
    ChangeModel,
    Close,
    /// Editor header ⋮: the block's contextual menu (manual §4).
    Menu,
    /// FOOTSWITCH ASSIGNMENT button (shot 058): the "Assign footswitch" dialog (`0x3b5424`).
    Stomp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerControl {
    Category(ModelCategory),
    Model(u32),
    Search,
    ClearSearch,
    Paste,
    /// Virtual Device List category row or rail icon; the open group collapses.
    Group(DeviceGroup),
    Instrument(Instrument),
    /// Preset-stack button of a model row: its Virtual Device Presets.
    Presets(u32),
    /// FACTORY / USER tab of the presets pane.
    PresetTab {
        user: bool,
    },
    /// Preset row tap: audition it.
    Preset(u64),
    /// ⋮ of a preset row.
    PresetMenu(u64),
    PresetMenuItem(DevicePresetMenu),
    /// ✓ keeps the auditioned preset; ✕ (or Back) reverts it.
    KeepPreset,
    RevertPreset,
    /// Add device capture browser (`zenUI::VirtualDevicesCaptures`, `0x841f60`): a folder
    /// row. A library folder with subfolders is also entered (`onFolderTapped`, shot sn-5).
    CaptureFolder(CaptureFolder),
    /// The ← header button (`prevFolderButtonSlot`): back to the parent folder.
    CaptureBack,
    /// The REFRESH header button of the Downloads folder (`refreshButtonSlot` `0x83ff00`):
    /// the list reads again.
    CaptureRefresh,
    /// A capture row: index into [`crate::CaptureLibrary::captures`] (`captureClickedSlot`).
    Capture(u16),
    /// A band of the `# • A • I • R • Z` strip: the list scrolls to the first row at or past
    /// this initial (`#` = digits).
    CaptureJump(u8),
    /// The ✕ header button: the device list closes.
    Close,
}

/// A folder of the Add device capture browser (shot sn-4): the three fixed folders, the
/// Captures Library root and its folders ([`crate::CaptureLibrary::folders`] index).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CaptureFolder {
    Favorites,
    Recent,
    Downloads,
    #[default]
    Library,
    Folder(u16),
}

/// Row ⋮ menu of a Virtual Device Preset (shot 040).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevicePresetMenu {
    SetDefault,
    EditName,
    Overwrite,
    Delete,
}

/// A settings page with Virtual Device Presets (manual M553, M593-594). One stock
/// `GlobalEQPresetPopup` (`0x227a84`) serves both; I/O has no Set as Default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsPresetPage {
    /// GLOBAL EQ ("Parametric-5", model 4004).
    GlobalEq,
    /// I/O SETTINGS ("IOSettings", model 31000).
    Io,
}

impl SettingsPresetPage {
    pub const fn model_id(self) -> u32 {
        match self {
            Self::GlobalEq => 4004,
            Self::Io => 31_000,
        }
    }
}

/// Controls of the settings preset popup (`0x227a84`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsPresetControl {
    /// The 44 × 44 button at (8, 8) that opens the list.
    Open(SettingsPresetPage),
    /// "Save Current Parameters as..." (row 0, `0x22a1d8`).
    SaveAs,
    /// A preset row: load it (`load(const ModelPresetID&)`).
    Load(u64),
    /// A row ⋮ (`onContextMenuButton`).
    Menu(u64),
    /// A context menu row (`onContextMenuItem` `0x2289a0`).
    MenuItem(DevicePresetMenu),
}

/// Requests of the settings preset popup. The application stores the page values as the
/// model's preset rows in `zenui.db` (`model_presets`) and loads them back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsPresetRequest {
    /// Recall the preset at once (manual: "recalled immediately").
    Load(u64),
    /// `create()`: a new user preset of the current values.
    Create {
        name: String,
    },
    /// `overwrite(const ModelPresetID&)`: the current values into a user preset.
    Overwrite(u64),
    Rename {
        preset: u64,
        name: String,
    },
    Delete(u64),
    /// Global EQ only (`0x2289a0` checks model 4004 first).
    SetDefault(u64),
    /// A double tap on a Global EQ value (manual "Global EQ MENU"): the value of the current
    /// default preset.
    RestoreDefault(SettingId),
}

/// Expression assignment screen controls (shot 068).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpressionControl {
    Pedal(ExpressionPedal),
    /// ASSIGN button of a cell: links or unlinks the target.
    Assign(ExpressionTarget),
    /// SWITCH ON 3-way slider: next choice.
    SwitchOn,
    InvertRange,
    /// SWITCH DELAY knob: a tap adds 100 ms and wraps after 5000 (owned).
    SwitchDelay,
    LatchEmulation,
    /// Power button of the bypass row: block bypass.
    Bypass,
    /// Disk button: keep the assignment.
    Save,
    /// A linked parameter cell: its MIN RANGE / MAX RANGE show in the top row.
    Select(u16),
    /// MIN RANGE (`max` false) or MAX RANGE knob: a tap adds 10 % and wraps after 100
    /// (owned, like SWITCH DELAY); the encoders under the columns step 1 %.
    Range {
        max: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaControl {
    Folder(MediaFolder),
    /// The library row: every item of the picked kind.
    Library,
    /// Index into `UiSnapshot::media`.
    Item(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryControl {
    DatabaseFolder(i64),
    DatabaseFolderMenu(i64),
    PluginFolder(u16),
    /// A sidebar folder of the open plugin (index into its `children`).
    PluginChild(u16),
    /// A plugin preset row (index into the open plugin's `presets`): audition it.
    PluginPreset(u16),
    /// SWAP (`false`) or OVERWRITE (`true`) of the Move conflict dialog.
    ConfirmMove {
        overwrite: bool,
    },
    ConfirmDirectoryDelete {
        remove_products: bool,
    },
    Setlist(u64),
    Search,
    ClearSearch,
    NewSetlist,
    /// The category drop-down (Favorites and Recent, Presets, Neural Captures, Impulse
    /// Responses, Plugin Presets).
    Category,
    /// A bank of the bank column (a page of `bank size` presets).
    Bank(u16),
    /// Load the favorite at this list index.
    Favorite(u64),
    /// Remove the favorite at this list index.
    Unfavorite(u64),
    /// ✓: close the Directory.
    Done,
    /// MULTI SELECT on/off.
    MultiSelect,
    Sort,
    /// Capture filter pop-up (`FilterMenu` `0x7f929c`) and its "Made by Me" toggle.
    Filter,
    MadeByMe,
    /// A device type row of the capture filter (index in `CAPTURE_DEVICE_TYPES`): toggle it.
    TypeFilter(u8),
    /// A version row of the capture filter.
    Version(crate::CaptureVersion),
    /// Capture UPLOAD (Upload Mode, not built: the Wi-Fi notice) and REFRESH (with a cloud
    /// folder open: its first page again; else the Wi-Fi notice).
    Upload,
    Refresh,
    /// The DOWNLOADS and CLOUD sidebar rows: open that cloud folder of the category.
    Downloads,
    Cloud,
    /// A row of the open cloud folder (index into its rows): download it
    /// (`cloudDownloadClicked` `0x79992c`, `downloadsItemClicked` `0x78d204`).
    CloudItem(u16),
    Audition(u64),
    /// "+ New Folder" of the capture/IR libraries (not built).
    NewFolder,
    /// Capture/IR library root row, or one of its folders.
    Library,
    Folder(MediaFolder),
    /// Favorites sidebar row, or the search type tab.
    Kind(ItemKind),
    /// Favorites and Recent: the 🕒 tab (`true`) or the ♥ tab.
    Recent(bool),
    /// ⋮ of a setlist, a library folder or a capture/IR row.
    SetlistMenu(u64),
    FolderMenu(MediaFolder),
    MediaMenu(u16),
    /// A capture/IR row (index into `UiSnapshot::media`): highlight it.
    Media(u16),
    /// A capture/IR search result: open its folder with the row highlighted.
    Reveal(u16),
    /// A preset search result: index into `DirectoryView::presets`.
    RevealPreset(u32),
    /// A "Recently searched" row.
    RecentSearch(u8),
    /// Alphabet strip: `b'#'` or `b'A'..=b'Z'`.
    Index(u8),
    /// Date, Device Type or Gain jump bar: a date position 0..200, a device code or a gain.
    Jump(u8),
    /// Multi Select row toggle (preset id or media index).
    Check(u64),
    Bulk(BulkAction),
    /// An item of a Directory ⋮ menu.
    Item(ItemAction),
    /// A row of the pasting option dialog (`PastingPopup`).
    PasteChoice(PasteChoice),
    /// PASTE / CONTINUE of the pasting option dialog.
    ConfirmPaste,
    /// DELETE of the Multi Select delete dialog.
    ConfirmBulkDelete,
    /// REMOVE of the folder form of the capture/IR Multi Select delete dialog: unlink the
    /// checked items from the open folder.
    ConfirmBulkRemove,
}

/// The `PresetAddMode` rows of the stock `PastingPopup` (`0x3a4a28`; `pasteSelected`
/// `0x3a55f4` emits the checked row, `0x795b18` acts on it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteChoice {
    /// "Choose each slot manually" (row 0, checked when the popup opens, `0x3a6284`).
    Manual,
    /// "Paste consecutively from the first chosen slot onwards".
    FromChosen,
    /// "Paste consecutively from the first empty slot onwards".
    FromEmpty,
}

/// What Add to Grid places: a capture key (a library capture, or the product id of a stored
/// Downloads capture) or an IR value ([`crate::MediaEntry::value`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GridMedia {
    Capture(String),
    Ir(String),
}

/// One cell in Add to Grid mode (`setModelFitInfoSlot` `0x5947c4`). Tetrad fits every cell at
/// once, so the stock state 0 (spinner, fit pending) has no value here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellFit {
    /// State 2: an empty cell where the block fits (`icons/24/plus.png`).
    Add,
    /// State 1: the block fits and replaces the cell's block (`icons/32/change_black.png`).
    Replace,
    /// State 3: the block does not fit; the cell stays dim.
    NoFit,
}

/// Where [`UiCommand::CopyPresets`] puts the presets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteTarget {
    /// The first empty slots of the setlist.
    FirstEmpty,
    /// These slots, one per id in order; an occupied slot is overwritten.
    Slots(Vec<u64>),
}

/// Directory item types: favorites sidebar and search tabs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ItemKind {
    #[default]
    Preset,
    Capture,
    Ir,
}

/// Multi Select header actions (manual §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkAction {
    Copy,
    Paste,
    Move,
    Favorite,
    Upload,
    Download,
    Edit,
    Delete,
}

/// Directory ⋮ menu items (preset rows, setlists, folders, captures and IRs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemAction {
    Load,
    EditName,
    Duplicate,
    Favorite,
    Move,
    Upload,
    Delete,
    NewSubfolder,
    SavingDestination,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingControl {
    /// Row tap: toggles flip, choices open a list, numbers do nothing.
    Open(SettingId),
    Step(SettingId, i8),
    Page(SettingsPage),
    IoTab(IoTab),
    /// Open the group dropdown (Account, System, Device, Support).
    Groups,
    /// Request one value directly: a choice index (radio option), a toggle 0/1 or a raw
    /// number (brightness segment).
    Set(SettingId, i32),
    /// (i) help popup: title and stock help text.
    Info(&'static str, &'static str),
    /// Device Information pencil (`0x3ec328`): the `DeviceNameDialog`.
    EditDeviceName,
    /// A 3rd Party Licenses row: the package's licence text.
    Licence(&'static str),
    /// A Diagnostics list row.
    Diagnostics(crate::DiagnosticsPage),
}

/// Preset MIDI Out matrix and message editor (`MidiMatrixDialog`, `MidiOnLoadDialog`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiOutControl {
    /// Open the message editor of a target.
    Target(MidiOutTarget),
    /// Trash button: delete every assignment of the preset (after a confirm).
    ClearAll,
    /// Select message `n` of the open target.
    Select(u8),
    Add,
    Remove,
    /// Step a field of the selected message.
    Step(MidiField, i8),
}

/// Editable fields of one Preset MIDI Out message (labels `0xa36070..0xa360d8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiField {
    Kind,
    Channel,
    /// CC#, MIN VALUE… by kind: parameter 1..=3.
    Value(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogButton {
    Primary,
    Secondary,
    Cancel,
    /// A dialog checkbox ("Don't show again").
    Check,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyLayer {
    Letters,
    Digits,
    Symbols,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Shift,
    Layer(KeyLayer),
    Backspace,
    Space,
    Done,
    Cancel,
}

/// Keys of `KeyboardNumeric` (`0x2d7f1c`) and its dialog buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericKey {
    Digit(u8),
    Point,
    /// Flips the sign of a value that is not zero.
    Minus,
    Backspace,
    Done,
    Cancel,
}

/// The eight Looper X tiles in stock tile order (`0x6129fc`; positions `_INIT_519`). The
/// device maps each to its Looper button code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LooperAction {
    Duplicate,
    OneShot,
    HalfSpeed,
    PunchIn,
    /// RECORD, or OVERDUB while a loop exists.
    Record,
    /// PLAY, or PLAYING while it runs (a press stops it).
    PlayStop,
    Reverse,
    /// One tile: UNDO, or REDO after an undo.
    UndoRedo,
}

impl LooperAction {
    pub const TILES: [Self; 8] = [
        Self::Duplicate,
        Self::OneShot,
        Self::HalfSpeed,
        Self::PunchIn,
        Self::Record,
        Self::PlayStop,
        Self::Reverse,
        Self::UndoRedo,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TempoControl {
    Slower,
    Faster,
    Tap,
    /// The TEMPO knob of the Tempo page: a vertical drag changes the BPM, one per pixel (stock:
    /// a 60 px drag moved 120 → 180 BPM on the QC, 2026-10-15; the stock knob law over
    /// 40–240 BPM gives the same); a tap does nothing.
    Knob,
}

/// Neural Capture wizard requests (button strings at `0x3d2890`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureRequest {
    Begin,
    Continue,
    /// One page back: diagram BACK and CONNECTION DIAGRAM on calibration (manual §6).
    Back,
    StartCapture,
    Stop,
    Refine,
    BackToCalibration,
    StartFromScratch,
    Audition(AuditionSource),
    Restart,
    Abort,
    Exit,
    /// The "Unsaved capture" answer to a held Back: OK discards (`true`), CANCEL stays.
    Discard(bool),
    /// Encoder steps on a parameter of the capture preset's Cab block.
    CabNudge {
        parameter: u16,
        steps: i32,
    },
}

/// NC2 dialog buttons (`NeuralCapture2Dialog` ctor `0x3dadc8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nc2Request {
    /// ABORT THE PROCESS and the header ✕: `doneButtonSlot` → `hideDialog` `0x3d7098`, which
    /// closes NC2 (`0x203ca4`) and reverts its preset.
    Close,
    /// SAVE NEURAL CAPTURE (`0x3d8018` → `0x1fef08`).
    Save,
    /// RESTART CAPTURE (`0x3d8000` → `0x203ee4`: product cleanup, then start from scratch).
    Restart,
    /// CAPTURE / REFERENCE (`0x3d7424`): NC2 state 5 / 4.
    Audition(AuditionSource),
}

/// Capture record `device` ("TYPE OF CAPTURE", names `0x74b5f0`). The V1 save page offers
/// `Default`..=`Pedal` (manual screenshot `030`); the rest are NC2 types.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum CaptureDevice {
    #[default]
    Default = 0,
    Amp = 1,
    ComboAmp = 2,
    AmpAndCab = 3,
    Cab = 4,
    Pedal = 5,
    Fuzz = 6,
    Compressor = 7,
    Overdrive = 8,
}

impl CaptureDevice {
    /// Tiles of the V1 save page, left to right.
    pub const V1: [Self; 6] = [
        Self::Default,
        Self::Amp,
        Self::ComboAmp,
        Self::AmpAndCab,
        Self::Cab,
        Self::Pedal,
    ];

    /// The stock `deviceType` code (`0x74b5f0` order); other codes have no type.
    pub fn from_code(code: u8) -> Option<Self> {
        [
            Self::Default,
            Self::Amp,
            Self::ComboAmp,
            Self::AmpAndCab,
            Self::Cab,
            Self::Pedal,
            Self::Fuzz,
            Self::Compressor,
            Self::Overdrive,
        ]
        .get(usize::from(code))
        .copied()
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Amp => "Amp",
            Self::ComboAmp => "Combo Amp",
            Self::AmpAndCab => "Amp + Cab",
            Self::Cab => "Cab",
            Self::Pedal => "Pedal",
            Self::Fuzz => "Fuzz",
            Self::Compressor => "Compressor",
            Self::Overdrive => "Overdrive",
        }
    }
}

/// Capture record `instrument` ("PREFERRED INSTRUMENT", names `0x74b518`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum CaptureInstrument {
    #[default]
    NotSpecified = 0,
    Guitar = 1,
    Bass = 2,
    Synth = 3,
    Vocal = 4,
    Other = 5,
}

impl CaptureInstrument {
    /// The stock `instrumentType` code (`0x7ec714`); other codes have no instrument.
    pub fn from_code(code: u8) -> Option<Self> {
        [
            Self::NotSpecified,
            Self::Guitar,
            Self::Bass,
            Self::Synth,
            Self::Vocal,
            Self::Other,
        ]
        .get(usize::from(code))
        .copied()
    }

    /// Chips of the save page in stock order.
    pub const CHOICES: [Self; 5] = [
        Self::Guitar,
        Self::Bass,
        Self::Synth,
        Self::Vocal,
        Self::Other,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::NotSpecified => "Not specified",
            Self::Guitar => "Guitar",
            Self::Bass => "Bass",
            Self::Synth => "Synth",
            Self::Vocal => "Vocal",
            Self::Other => "Other",
        }
    }
}

/// Calibration panel tabs "1" and "2" (manual §6 Calibration Settings).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CalibrationTab {
    /// Levels, input types, meters.
    #[default]
    Levels,
    /// IN 2 GROUND LIFT, IN 2 PHANTOM, OUT 1/2 GND LIFT.
    Grounding,
}

/// Neural Capture controls the UI handles itself: the calibration tab and the save page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureControl {
    Tab(CalibrationTab),
    /// Save page ✕: back to the A/B test without saving.
    CloseSave,
    /// Save page "Name" chip: the keyboard.
    Name,
    Device(CaptureDevice),
    Instrument(CaptureInstrument),
    /// Save page floppy button: `UiCommand::SaveCapture`.
    Save,
    /// The capture Cab block tile: shows or hides its parameters in the panel.
    Cab,
}

/// Save As PREFERRED INSTRUMENT page (`PresetMetadataDialog` `0x7e79b8`) over the name
/// keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetMetadataControl {
    /// The header metadata button (`metadataButtonSlot`).
    Open,
    /// The page back button: the name keyboard again.
    Back,
    Instrument(PreferredInstrument),
    /// The blue floppy button (`saveButtonSlot`): saves as the keyboard Done does.
    Save,
}

/// `UpdaterMessage.command` subset used on the device screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdaterCommand {
    Check,
    CheckAndDownload,
    Install,
    CancelDownload,
}

/// Owned (Tetrad): the Tetrad Updates page. These update Tetrad only, never CorOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TetradUpdateCommand {
    Check,
    /// Download, check, install and arm the offered release, then restart.
    Install,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiControl {
    Scan,
    Network(u16),
    /// Connection page links (`0x5d3b38`, `0x5d3818`).
    DomainSettings,
    InternetCheck,
    /// The forget icon of a saved secured network (`0x5d1388`).
    Forget(u16),
}

/// Controls of the Extensions pages. Indices address the current
/// [`crate::ExtensionsView`] lists; detail controls act on the extension whose page is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionControl {
    Refresh,
    /// REPOSITORIES: the source list.
    Sources,
    /// An installed row: its detail page.
    Open(u16),
    /// INSTALL of an available row.
    Install(u16),
    /// The detail page's enable switch.
    Enable,
    Retry,
    Update,
    /// REMOVE, after a confirmation.
    Remove,
    /// A command button of the open extension.
    Run(u16),
    /// A setting row: a toggle flips, a choice opens its list.
    Setting(u16),
    /// A number setting stepped by one.
    Step(u16, i8),
    /// ADD REPOSITORY: the keyboard for `owner/repo`.
    AddSource,
    SourceEnabled(u16),
    /// REMOVE of a source row, after a confirmation.
    RemoveSource(u16),
}

/// Controls of the Mappings page (`screens::mappings`). Indices address the rows of the shown
/// tab; detail controls act on the row whose detail page is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingControl {
    /// THIS PRESET or GLOBAL.
    Tab(MappingScope),
    /// +: a new row in the shown tab.
    Add,
    /// A list row: its detail page.
    Open(u16),
    /// The detail page's trash button.
    Remove,
    /// A dropdown of the Trigger panel.
    Choice(MappingChoice),
    /// The MIDI number field: the numeric keyboard.
    Number,
    /// LEARN, or CANCEL while the row waits for its message.
    Learn,
    /// The Action row: the action menu.
    Actions,
    /// Row `n` of the open action menu.
    Pick(u16),
    /// The value stepper, in 5 % steps.
    Value(i8),
    /// Edit messages of a Send MIDI row: the Preset MIDI Out message editor.
    EditMessages,
}

/// The dropdowns of the Mappings Trigger panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingChoice {
    /// Type: Preset loaded, Footswitch, Expression pedal, MIDI or an extension trigger.
    Kind,
    /// Stomp A–H.
    Stomp,
    /// EXP 1 or 2.
    Pedal,
    /// CC or Note.
    Midi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupControl {
    Create,
    Item(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingControl {
    Split(i8),
    Mix(i8),
    SplitOff,
    MixOff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeControl {
    Allow(FootswitchMode),
    CycleUpper,
    CycleLower,
    /// Modes configuration slot `n` of the cycle: drag to reorder, merge or remove; a long
    /// press splits a hybrid slot (manual §3 "MODES CONFIGURATION MENU").
    Slot(u8),
    /// The right edge of the hybrid slot: swap its rows.
    SwapRows,
    /// The add button: a menu of the modes not in the cycle (`0xa256a8`).
    Add,
    /// Append this mode to the cycle (an item of the add menu).
    Insert(FootswitchMode),
    /// The ⓘ button: the MIDI recall help (`onInfoButtonReleased`, `0x2d4f00`).
    Info,
}

/// Power & Locking overlay buttons (`PowerButtonPopup`, `0x7d5690`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerControl {
    Lock,
    Cancel,
    Shutdown,
    Reboot,
    BeRightBack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Navigate(Screen),
    /// A key of the numeric parameter keyboard.
    Numeric(NumericKey),
    SelectScene(SceneIndex),
    SelectCell(GridPosition),
    SelectPreset(u64),
    EditScenes,
    /// Grid scene badge: opens or closes the scene dropdown (manual "SCENES DROPDOWN").
    SceneMenu,
    /// Grid header M406 status: opens the Global EQ / Input Gate disabled popup.
    GlobalEqStatus,
    ConfirmScenes,
    CloseDialog,
    CycleMode,
    BeginBankPreview,
    EndBankPreview,
    CancelBankPreview,
    /// Cancel of the batch upload progress dialog (`multiselectCancelSlot` `0x780390`).
    CancelUpload,
    PreviousPresetPage,
    NextPresetPage,
    /// Top bar back arrow; same as `InputEvent::Back`.
    Back,
    OpenMainMenu,
    Menu(MenuAction),
    SavePreset,
    Undo,
    Redo,
    Port {
        row: Row,
        output: bool,
    },
    Param(ParamControl),
    Tile(Footswitch),
    StepBank {
        up: bool,
    },
    Picker(PickerControl),
    Media(MediaControl),
    Library(LibraryControl),
    Setting(SettingControl),
    Choose(u16),
    Dialog(DialogButton),
    /// DSPErrorDialog CANCEL (`cancelButtonSlot`) or REBOOT (`confirmButtonSlot`).
    DspError {
        reboot: bool,
    },
    Key(Key),
    Looper(LooperAction),
    Tempo(TempoControl),
    Capture(CaptureRequest),
    Nc2(Nc2Request),
    /// A/B SAVE: name the capture, then the save page.
    SaveCapture,
    CaptureUi(CaptureControl),
    PresetMetadata(PresetMetadataControl),
    Wifi(WifiControl),
    /// Extensions and Repositories pages (`screens::extensions`).
    Extension(ExtensionControl),
    /// The Mappings page (`screens::mappings`).
    Mapping(MappingControl),
    /// The switch of the Remote page (`screens::remote`).
    RemoteToggle,
    Updater(UpdaterCommand),
    TetradUpdate(TetradUpdateCommand),
    ShowChangelog,
    Backup(BackupControl),
    Login,
    Action(DeviceAction),
    SceneTarget(SceneIndex),
    Mode(ModeControl),
    Power(PowerControl),
    Routing(RoutingControl),
    /// Open the keyboard for the text field of the open dialog.
    EditText,
    /// Looper X ⋮ button: Reassign / Reset Looper X actions.
    LooperMenu,
    /// DONE of the Looper X swap dialog (`onLooperSwapActionsDoneButton`).
    LooperSwapDone,
    /// CANCEL of the "Assign footswitch" prompt (`0x37ab08`).
    LooperAssignCancel,
    /// Looper X editor "Params": the Looper block's parameter editor on the Grid.
    LooperParams,
    /// The block editor of a Grid block from another screen (Gig View EDIT STOMP).
    EditBlock(GridPosition),
    /// Mini Looper X lock button: toggles whether footswitches act as Looper tiles.
    LooperLock,
    /// A preset row's contextual menu (•••, manual §5).
    PresetMenu(u64),
    /// Gig View scene tile tools (manual §3: EDIT SCENE, SWAP SCENE, COPY SCENE).
    SceneTool {
        scene: SceneIndex,
        tool: SceneTool,
    },
    Expression(ExpressionControl),
    MidiOut(MidiOutControl),
    /// Gig View EDIT STOMP (`0x2c56f8`): the stomp's name, Latching/Momentary and blocks.
    Stomp(StompEdit),
    /// A Splitter or Mixer token: its parameter editor.
    Site(EditorSite),
    /// Global EQ and I/O Virtual Device Presets.
    SettingsPreset(SettingsPresetControl),
    /// Splitter/Mixer editor quick split buttons (`SMParamsEditor` `0x3dfefc`).
    QuickSplit(QuickSplit),
    /// The full-screen Pattern Tremolo and Cab editors (`ModelEditor` `0x4f1f5c`).
    Editor(EditorControl),
    /// Device Storage RESTORE of quarantine row n ([`crate::UiSnapshot::quarantine`], O).
    RestoreCapture(u16),
    /// Add to Grid: a cell (`blockSelected(row, col)`), or `None` for the space above row 0
    /// and beside columns 0 and 7 (`tappedOutside`; press handler `0x7c65e0`).
    AddToGrid(Option<GridPosition>),
}

/// Controls of the full-screen model editors. Knobs name the stock parameter index, as the
/// cab editor shows two mics of eight parameters each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorControl {
    /// A knob: a drag turns it by the stock knob law (`widgets::KnobDrag`), a tap flips a
    /// toggle or steps a choice, a hold assigns it to Scenes.
    Knob(u16),
    /// Pattern Tremolo STEP fader 0..16 (`PatternTremoloFader`): a vertical drag sets the
    /// value, a sideways drag joins or splits steps, a hold assigns it to Scenes.
    Step(u8),
    /// STEPS − / + (`onStepsMinusButton` `0x3b0b08`, `onStepsPlusButton` `0x3b0c3c`).
    StepsMinus,
    StepsPlus,
    /// The step scroller (`PatternTremoloFaderScroller`): picks the first shown fader.
    Scroller,
    /// Cab mic placement graph (`0x825c68`): a tap or drag places the mic.
    Mic(CabMic),
    /// Cab phase button (`phaseReleased`): flips `phi`.
    Phase(CabMic),
    /// Cab mic bypass button (`bypassReleased`).
    MicBypass(CabMic),
    /// IR selector arrows (`icons/cabsim/back.png`, `forward.png`): the previous or next mic
    /// of the same cab.
    IrStep {
        mic: CabMic,
        forward: bool,
    },
    /// IR selector name (`irSelectorReleased` `0x82049c`): turns the panel's IR mode on or
    /// off.
    IrList(CabMic),
    /// IR mode buttons: `ir.png` (`irButtonReleased` `0x82036c`) opens the IR browser, close
    /// (`closeButtonReleased` `0x8202b8`) ends IR mode, a table row
    /// (`irTableCellSelected`) loads that IR.
    IrBrowse(CabMic),
    IrClose(CabMic),
    /// Parallax resize button (`0x636e28`): the graph grows or shrinks.
    Resize,
    IrRow {
        mic: CabMic,
        row: u16,
    },
    /// Pattern Tremolo info button (`info_gray.png`): the Step Groups tooltip (`0x3adf84`).
    StepGroupsInfo,
    /// EQ graph node of band n (`bandSelectedSignal`, `controlPointMovedToPosSignal`,
    /// `bandDoubleTappedSignal`): a tap selects the band, a drag sets FREQ and GAIN, a
    /// double tap turns the band on or off. Graphic EQ: 0 = HPF ("H"), 1 = LPF ("L").
    EqNode(u8),
    /// EQ band tab n (`bandTabSelectedSlot`); the tab after the last band is OUT.
    EqTab(u8),
    /// EQ TYPE button (`typeButtonSlot`): the filter type selector (`EQFilterTypeSelector`).
    EqType,
    /// EQ band power button (`bypassButtonSlot`).
    EqBypass,
    /// Graphic EQ band fader for parameter n: a touch or drag sets the gain.
    Fader(u16),
    /// IR Loader channel buttons (`IRLoaderButtonArea` `0x7fca70`): power (MUTE), phase
    /// (INVERT), the label area (IR browser) and up / down (previous / next IR).
    IrPower(u8),
    IrPhase(u8),
    IrName(u8),
    IrNext {
        channel: u8,
        forward: bool,
    },
    /// Neural Capture editor: for a missing file, the download button
    /// (`onCaptureErrorButtonPressed` `0x3c841c`).
    CaptureDownload,
    /// Tab n of a tabbed editor (Arpeggio Delay `setCurrentTab` `0x2a7f84`).
    Tab(u8),
    /// Multivoicer Global bank tab n (`onGlobalParamTabIndexChanged` `0x627708`).
    Bank(u8),
    /// A button that flips parameter n (Parallax LOW / MID / HIGH, Overlord Synth notes and
    /// OSC buttons); `u16::MAX` is the fixed ARP ROOT button.
    Toggle(u16),
    /// Overlord Synth wave button `wave` (1..=7) of oscillator `osc` (`0x62dde8`).
    Wave {
        osc: u8,
        wave: u8,
    },
    /// Graph control point n (Arpeggio Delay tap n: `voiceSelectedSignal`,
    /// `controlPointMovedToPosSignal`, `voiceDoubleTappedSignal`): a tap selects its tab, a
    /// drag moves it, a double tap turns it on or off.
    Point(u8),
}

/// The two mics of a cab (`CabsimPanel` `0x821408`, index 0 and 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CabMic {
    A,
    B,
}

impl CabMic {
    pub const ALL: [Self; 2] = [Self::A, Self::B];

    /// First parameter of the mic: bypass, ir selector, LEVEL, PAN, DISTANCE, POSITION,
    /// phi, GRID MODE follow (ModelRepo cab models).
    pub const fn base(self) -> u16 {
        match self {
            Self::A => 0,
            Self::B => 8,
        }
    }
}

/// The quick split buttons of the Splitter/Mixer editor (`onQuickButtonReleased`
/// `0x3e19e8` → `0x51a090(pair, split, mix)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickSplit {
    /// `quick1.png`: split at column 1, no merge (split 0, mix −1).
    Split,
    /// `quick2.png`: split at column 1, merge after column 8 (split 0, mix 7).
    SplitAndMix,
}

/// Gig View EDIT STOMP controls for one stomp tile (0..=7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StompEdit {
    Open(u8),
    Rename(u8),
    Momentary {
        tile: u8,
        momentary: bool,
    },
    /// Toggles the block's assignment to the tile.
    Block {
        tile: u8,
        position: GridPosition,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneTool {
    Edit,
    Swap,
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    PointerDown {
        x: i32,
        y: i32,
    },
    PointerMove {
        x: i32,
        y: i32,
    },
    PointerUp {
        x: i32,
        y: i32,
    },
    CancelPointer,
    /// Semantic input for keyboard/automation adapters. Only visible enabled
    /// controls can be activated, just as with pointer input.
    Activate(Control),
    Back,
    TextChanged(String),
    DraftColor(u32),
    /// Monotonic application clock in milliseconds. Drives long press,
    /// footswitch holds and toast expiry; rendering stays a pure function.
    Tick {
        now_ms: u64,
    },
    Knob {
        knob: KnobIndex,
        delta: i32,
    },
    /// One raw edge. Edges are never coalesced.
    Footswitch {
        switch: Footswitch,
        pressed: bool,
    },
    /// POWER button edge (full-size QC). A press toggles the Power & Locking overlay; a
    /// hold ends the lock or Be Right Back.
    PowerButton {
        pressed: bool,
    },
    /// The Master Volume knob moved to `percent` (0..=100): shows the Master Volume overlay.
    MasterVolume {
        percent: u8,
    },
    /// An accelerated BANK UP encoder turn (QC logical 10): cycles devices in the block
    /// editor and auditions Virtual Device Presets in their list (manual §4).
    BankEncoder {
        delta: i32,
    },
}

/// What a footswitch addresses in the mode shown when it changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FootswitchTarget {
    Scene(SceneIndex),
    Stomp(Footswitch),
    Preset(u64),
    BankUp,
    BankDown,
    Unassigned,
}

/// [`UiCommand::SetPosition`] value of a normalized 1.0.
pub const POSITION_SCALE: u16 = 10_000;

/// Requests for the application. None is an acknowledgement: the
/// application validates, applies and resupplies a snapshot. `Debug` hides
/// passwords.
#[derive(Clone, PartialEq, Eq)]
pub enum UiCommand {
    /// Persisted Directory edit or explicit audition request.
    Directory(DirectoryRequest),
    /// An Extensions page request (`crate::extensions`).
    Extension(crate::ExtensionCommand),
    /// A Mappings page request (`crate::mappings`).
    Mapping(crate::MappingCommand),
    /// The Remote page switch (`crate::remote`).
    SetRemote {
        enabled: bool,
    },
    SelectScene {
        scene: SceneIndex,
    },
    InspectCell {
        position: GridPosition,
    },
    RequestPreset {
        id: u64,
    },
    PreviewPreset {
        id: u64,
    },
    CycleMode,
    BeginBankPreview,
    EndBankPreview,
    CancelBankPreview,
    SetSceneLabel {
        scene: SceneIndex,
        label: String,
    },
    SetSceneColor {
        scene: SceneIndex,
        raw_color: u32,
    },
    Rejected(UiError),
    SavePreset,
    /// Save as... (`0x5ac350`). `setlist` is the destination setlist id; `None` for a preset
    /// without a file, which saves into its own slot. `instrument` is the PREFERRED
    /// INSTRUMENT of the save (`presetSaveSignal`, `0x58fe80`).
    SavePresetAs {
        name: String,
        setlist: Option<u64>,
        instrument: PreferredInstrument,
    },
    /// Edit Details of the loaded preset (`modifyPresetMetadataSignal` → `0x5b91dc` →
    /// `0x5b7438`): a save under the new name with `instrument` in its record (`0x7c4db8`
    /// `+0x5c`, the sidecar `instrument_type`).
    EditPresetDetails {
        name: String,
        instrument: PreferredInstrument,
    },
    CreatePreset,
    RenamePreset {
        id: u64,
        name: String,
    },
    DuplicatePreset {
        id: u64,
    },
    /// A slot of the setlist browsed in the Directory.
    DeletePreset {
        id: u64,
    },
    /// Grid ⋮ Delete Preset: the loaded preset in its own setlist; `id` is the footswitch
    /// index and `load` the load generation (`UiSnapshot::preset_load`) when the confirm
    /// opened, so a preset change in between deletes nothing.
    DeleteLoadedPreset {
        id: u64,
        load: u64,
    },
    SetFavorite {
        id: u64,
        favorite: bool,
    },
    /// Multi Select FAVORITE (`multiselectFavoriteSlot` `0x783aa0`): add the presets of these
    /// slots of the browsed setlist to Favorites.
    FavoritePresets {
        ids: Vec<u64>,
    },
    /// Directory upload to Cortex Cloud (`0x725ba4`; ⋮ Upload, Multi Select UPLOAD).
    Upload {
        items: Vec<UploadItem>,
    },
    /// The answer to the upload overwrite dialog (`0x46485c`): OVERWRITE (`true`) updates the
    /// account's product with that name; CANCEL or any other close (`false`) sends nothing.
    UploadOverwrite {
        overwrite: bool,
    },
    /// The answer to "Neural Capture already exists" for a cloud capture download (`0x465178`
    /// mode 1, [`crate::UiSnapshot::capture_overwrite`]): OVERWRITE (`true`, `0x1d8f48`)
    /// replaces the user capture's details; CANCEL (`0x1d5ac8`) or any other close (`false`)
    /// stores nothing.
    CaptureOverwrite {
        overwrite: bool,
    },
    /// The answer to "Neural Capture already exists" for a capture upload (`0x465178` mode 0,
    /// [`crate::UiSnapshot::capture_upload_overwrite`]): OVERWRITE (`true`, `0x1e2924`)
    /// replaces the account's capture; CANCEL (`0x1d5a84`) or any other close (`false`) sends
    /// nothing.
    CaptureUploadOverwrite {
        overwrite: bool,
    },
    /// Cancel of the batch upload progress dialog (`0x780390` sets the batch stop flag
    /// `+0x7c`): the item in flight completes; the batch stops before the next item.
    CancelUpload,
    /// One page of a Directory cloud folder (`CloudFolderModel` → `0x718684`): the first
    /// page when `refresh`, else the next one (fetchMore `0x431178`).
    BrowseCloud {
        kind: ItemKind,
        scope: CloudScope,
        refresh: bool,
    },
    /// A cloud folder row: download product `id` (`0x79992c`: a preset `0x719f90` and a
    /// capture `0x1d9954` into their Downloads folder, an IR `0x799134` into the IR saving
    /// destination).
    DownloadProduct {
        id: String,
    },
    /// Multi Select DOWNLOAD of cloud captures or IRs: `0x79992c` per row as one batch, so a
    /// taken capture name gives the batch text once (`0x843c90`).
    DownloadProducts {
        ids: Vec<String>,
    },
    /// Multi Select DOWNLOAD of cloud presets into a setlist (`0x794768` → `0x795b18` mode 2
    /// "Download"): product `ids` in order into setlist `setlist` at `to`, with the PASTE slot
    /// rules. The device checks every target before the first download.
    DownloadPresets {
        ids: Vec<String>,
        setlist: u64,
        to: PasteTarget,
        /// OVERWRITE of "Overwrite Existing Presets?" (`0x46485c(name, 1)`): an occupied target
        /// slot is replaced. `false`: every target slot must be empty.
        overwrite: bool,
    },
    /// A Directory capture or IR tap (`captureClicked` `0x77f860`, `irClicked` `0x790d28`):
    /// the Grid enters Add to Grid mode; the device answers with
    /// [`crate::UiSnapshot::add_to_grid`].
    StartAddToGrid(GridMedia),
    /// An Add device capture row (`onCaptureClicked` `0x386024`): the capture block of `key`
    /// (`0x704530`: model 14001 for a version "2" record, else 14000) is added at an empty
    /// `position` or replaces its block, as one undo step.
    AddCapture {
        position: GridPosition,
        key: String,
    },
    /// The capture browser REFRESH (`0x83ff00` → `0x43432c`): read the capture list again.
    RefreshCaptures,
    /// A cell tap in Add to Grid mode (`addToGridBlockSelected` `0x596c50`): insert or replace
    /// with one undo step; a cell where the block does not fit changes nothing. The mode ends.
    AddToGrid {
        position: GridPosition,
        media: GridMedia,
    },
    /// A tap above or beside the cells (`addToGridTappedOutside` `0x59470c`): nothing added.
    CancelAddToGrid,
    /// IRSaveDialog OVERWRITE (`0x7e5a30`): user IR `other` holds `name`; it is deleted, then
    /// IR `id` takes the name. The device refuses it while the loaded preset uses `other`.
    OverwriteIr {
        id: String,
        name: String,
        other: String,
    },
    /// NC editor DOWNLOAD (`onCaptureErrorButtonPressed` `0x3c892c`): find the capture with
    /// this NC hash (the `file_name` value) on Cortex Cloud (`0x7bcac8`) and download it
    /// (`0x1d9954`). Never empty: stock refuses an empty hash (`0xa364e8`).
    DownloadCapture {
        hash: String,
    },
    /// Directory favorites (`snapshot.favorite_presets` index): load or remove.
    LoadFavorite {
        index: u64,
    },
    RemoveFavorite {
        index: u64,
    },
    /// Multi Select COPY (or MOVE / hold COPY with `remove`) and PASTE: the presets in slots
    /// `ids` of setlist `from` into `setlist` at `to`. `from` is the name of the setlist open
    /// at COPY, so a PASTE in another setlist, or after a setlist create, delete or move,
    /// copies the right presets (review r21 H2, r23 K11).
    CopyPresets {
        ids: Vec<u64>,
        from: String,
        setlist: u64,
        remove: bool,
        to: PasteTarget,
    },
    OpenSetlist {
        id: u64,
    },
    CreateSetlist {
        name: String,
    },
    Undo,
    Redo,
    CopyScene {
        from: SceneIndex,
        to: SceneIndex,
    },
    SwapScene {
        first: SceneIndex,
        second: SceneIndex,
    },
    ClearBlockSelection,
    AddBlock {
        position: GridPosition,
        model_id: u32,
    },
    ReplaceBlock {
        position: GridPosition,
        model_id: u32,
    },
    /// A grid drag and drop; `side` is the half of `to` under the finger.
    MoveBlock {
        from: GridPosition,
        to: GridPosition,
        side: DropSide,
    },
    RemoveBlock {
        position: GridPosition,
    },
    CopyBlock {
        position: GridPosition,
    },
    PasteBlock {
        position: GridPosition,
    },
    SetBlockBypass {
        position: GridPosition,
        bypassed: bool,
    },
    ResetBlockDefaults {
        position: GridPosition,
    },
    SetRowInput {
        row: Row,
        port: InputPort,
    },
    SetRowOutput {
        row: Row,
        port: OutputPort,
    },
    SetSplit {
        pair: RowPair,
        column: Option<u8>,
    },
    SetMix {
        pair: RowPair,
        column: Option<u8>,
    },
    /// A media picker commits the stored text of a string parameter
    /// (`ParameterKind::Media`).
    SelectText {
        position: GridPosition,
        parameter: u16,
        text: String,
    },
    /// Encoder or drag steps; units and range belong to the application.
    NudgeParameter {
        position: GridPosition,
        parameter: u16,
        steps: i32,
    },
    /// A graph drag sets a normalized value, `value` / [`POSITION_SCALE`]; the application
    /// snaps it to the parameter's steps (`0x5cc994` step index, `0x5cc9e0` step value).
    SetPosition {
        position: GridPosition,
        parameter: u16,
        value: u16,
    },
    ToggleStomp {
        switch: Footswitch,
    },
    /// Stomp rename (`GigStompRenameDialog`); label already simplified.
    RenameStomp {
        switch: Footswitch,
        label: String,
    },
    StepBank {
        up: bool,
    },
    Footswitch {
        switch: Footswitch,
        pressed: bool,
        target: FootswitchTarget,
    },
    SetModeAllowed {
        mode: FootswitchMode,
        allowed: bool,
    },
    SetHybridRows {
        rows: HybridRows,
    },
    ChangeSetting {
        id: SettingId,
        value: SettingValue,
    },
    Device {
        action: DeviceAction,
    },
    /// End of an expression calibration started by `DeviceAction::CalibrateExpression`.
    ExpressionCalibration {
        pedal: u8,
        save: bool,
    },
    ShowTuner {
        show: bool,
    },
    Looper {
        action: LooperAction,
    },
    /// Swap dialog: the next footswitch letter takes the action of tile `Some(n)`; the device
    /// swaps the two `stompAssignments` and blocks other footswitch handling until then
    /// (`0x37ab08`, ModeController `0x5898f4(1)`). `None` cancels.
    LooperAssign {
        tile: Option<u8>,
    },
    /// The factory `stompAssignments` (`0x4b40a8`, defaults `0x11e9448`).
    ResetLooperActions,
    /// DSPErrorDialog: CANCEL (`0x514c48`) or REBOOT (`0x514a6c`). Both set the last preset to
    /// Factory slot 0; REBOOT then restarts the unit.
    DspErrorDialog {
        reboot: bool,
    },
    /// Set Looper editor `+0x187` (mini lock button, `_atmaLooperLockButtonReleased`).
    LooperView {
        shown: bool,
    },
    TapTempo,
    NudgeTempo {
        steps: i32,
    },
    Capture {
        request: CaptureRequest,
    },
    Nc2 {
        request: Nc2Request,
    },
    /// Stock `saveCaptureSignal(name, DeviceType, Instrument, folder)`; the folder is
    /// always "My Captures" here (I).
    SaveCapture {
        name: String,
        device: CaptureDevice,
        instrument: CaptureInstrument,
    },
    WifiScan,
    WifiConnect {
        ssid: String,
        password: Option<String>,
    },
    WifiForget {
        ssid: String,
    },
    /// Tap on the joined network (`0x5d4834`): `DISCONNECT`.
    WifiDisconnect,
    Updater {
        command: UpdaterCommand,
    },
    TetradUpdate {
        command: TetradUpdateCommand,
    },
    /// CREATE BACKUP after its `BackupEditDialog` (`0x3f8568` → `onBackupCreateNameConfirmed`).
    CreateBackup {
        name: String,
    },
    /// Backup ⋮ Edit name (`0x3f8660` → `onBackupEditNameConfirmed`): rename backup `id`.
    RenameBackup {
        id: u64,
        name: String,
    },
    RestoreBackup {
        id: u64,
    },
    /// "Update Cloud Backup" PROCEED (`0x3f8730` → `0x4045ac` → `0x403fb0(page, item)`): pack
    /// the current user data and upload it over backup `id` (`<backup path>/update`,
    /// `0x303564`; create uses `/generateBackupUrl/%1`, `0x303300`).
    UpdateBackup {
        id: u64,
    },
    DeleteBackup {
        id: u64,
    },
    CloudLogin {
        email: String,
        password: String,
    },
    /// Modes configuration: the whole cycle in order; a `Hybrid` entry uses `hybrid`.
    SetModeCycle {
        modes: Vec<FootswitchMode>,
        hybrid: HybridRows,
    },
    /// LOCK (`true`) or the POWER unlock (`false`): the touchscreen and the Master Volume
    /// knob.
    SetLocked {
        locked: bool,
    },
    /// BE RIGHT BACK: all outputs and the screen off (`true`) until the next POWER press
    /// (`false`).
    BeRightBack {
        active: bool,
    },
    /// Virtual Device Presets of the block at `position`.
    DevicePreset {
        position: GridPosition,
        request: DevicePresetRequest,
    },
    /// Split and mix points of a row pair in one edit (Splitter/Mixer quick buttons; one undo
    /// frame, `0x3e19e8`). `None` removes the point.
    QuickSplit {
        pair: RowPair,
        split: Option<u8>,
        mix: Option<u8>,
    },
    /// Global EQ or I/O Virtual Device Presets.
    SettingsPreset {
        page: SettingsPresetPage,
        request: SettingsPresetRequest,
    },
    /// Expression pedal assignment of the block at `position`.
    Expression {
        position: GridPosition,
        request: ExpressionRequest,
    },
    /// Replace the Preset MIDI Out list of one target of the loaded preset (at most
    /// [`crate::MIDI_OUT_MESSAGES`] messages; an empty list clears it).
    SetPresetMidi {
        target: MidiOutTarget,
        messages: Vec<MidiOutMessage>,
    },
    /// Assign the block to footswitch tile 0..=7, or remove its assignment (UNASSIGN).
    AssignStomp {
        position: GridPosition,
        tile: Option<u8>,
    },
    /// Latching or Momentary for a stomp tile (Gig View EDIT STOMP).
    SetMomentary {
        tile: u8,
        momentary: bool,
    },
    /// Open the editor of a lane control or junction (closes the block editor).
    InspectSite {
        site: EditorSite,
    },
    /// A knob step on a lane control or junction parameter.
    NudgeSite {
        site: EditorSite,
        parameter: u16,
        steps: i32,
    },
    /// Hold a parameter to assign it to Scenes or remove it (manual "SCENE ASSIGNMENTS").
    SetSceneMode {
        position: GridPosition,
        parameter: u16,
        enabled: bool,
    },
    /// BANK UP encoder in the block editor (manual "QC Param Editor Encoders"): the block
    /// becomes the model `step` places away in its category.
    CycleModel {
        position: GridPosition,
        step: i32,
    },
    /// Copy / Swap Scene (`0x5ad9d4`): the next scene footswitch takes the copy or swap of
    /// `source` on the device (ModeController `0x5898f4(1)`, `0x20eec4`).
    BeginSceneCopy {
        source: SceneIndex,
        swap: bool,
    },
    /// The Copy / Swap Scene dialog closed without a footswitch press.
    EndSceneCopy,
    /// Directory folder links (manual "FOLDERS & NESTED NAVIGATION"): link the products
    /// (stock keys) into a capture/IR folder, or remove them from it (`linked` false). The
    /// library keeps the items.
    LinkMedia {
        folder: i64,
        ids: Vec<String>,
        linked: bool,
    },
    /// Folder menu DUPLICATE (`0x76dabc`): copy a capture/IR folder with its subfolders and
    /// links under a free sibling name.
    DuplicateMediaFolder {
        folder: i64,
    },
    /// Folder menu "Set as saving destination" (`0x745130` type 0xf): new captures or IRs go
    /// to this folder.
    SetSavingDestination {
        folder: i64,
    },
    /// Hold a Virtual Device List row (manual "BROWSING & LOADING VIRTUAL DEVICES"): pin the
    /// model to the top of its category, or unpin it (`pinned_models`).
    PinModel {
        model: u32,
        pinned: bool,
    },
    /// Numeric keyboard value of a parameter, as typed in display units. The application
    /// converts and clamps it; text that is not a number does nothing.
    EnterParameter {
        target: ParameterTarget,
        parameter: u16,
        text: String,
    },
    /// Device Information name edit (`DeviceNameDialog`, `0x64fd2c`).
    SetDeviceName {
        name: String,
    },
    /// Read a 3rd party licence (`/opt/neuraldsp/ThirdPartyLicenses/<package>-license`) into
    /// [`crate::UiSnapshot::licence`]; `None` when the viewer closes.
    ShowLicence {
        package: Option<String>,
    },
    /// Put a quarantined capture back into its store (O, review r23 K5).
    RestoreQuarantinedCapture {
        id: String,
        downloads: bool,
    },
}

/// The element whose parameter a numeric entry sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterTarget {
    Block(GridPosition),
    Site(EditorSite),
}

/// Virtual Device Preset requests (manual "Virtual Device Presets", shots 093/040).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicePresetRequest {
    /// Load the preset (and its model) into the block for listening.
    Audition {
        model_id: u32,
        preset: u64,
    },
    /// ✓: keep what is auditioned.
    Keep,
    /// ✕: restore the block as it was before the first audition.
    Revert,
    SetDefault(u64),
    Rename {
        preset: u64,
        name: String,
    },
    /// Store the block's current parameters into a user preset.
    Overwrite(u64),
    Delete(u64),
    /// "Save Current Parameters As…": a new user preset of the block's model.
    Create {
        name: String,
    },
    /// "Set Parameters as Defaults": the model's user default parameters.
    SetDefaults,
    /// Double tap: the parameter's value from the model's defaults (manual "Virtual Device
    /// Presets": the current default preset; else the catalog default).
    RestoreDefault {
        parameter: u16,
    },
}

/// Expression assignment requests (shot 068). Links apply live; `Save` keeps
/// them and `Revert` restores the state from when the screen opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpressionRequest {
    Link {
        pedal: ExpressionPedal,
        target: ExpressionTarget,
        linked: bool,
    },
    SwitchOn(ExpressionSwitch),
    InvertRange(bool),
    SwitchDelay(u16),
    LatchEmulation(bool),
    /// MIN RANGE / MAX RANGE of a linked parameter, in percent.
    Range {
        parameter: u16,
        min: u8,
        max: u8,
    },
    Save,
    Revert,
}

/// Same output as a derived `Debug`, except that passwords print as
/// `<redacted>`.
impl fmt::Debug for UiCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        macro_rules! show {
            ($name:literal $(, $field:ident)*) => {{
                let mut out = f.debug_struct($name);
                $(out.field(stringify!($field), $field);)*
                out.finish()
            }};
        }
        use UiCommand as C;
        match self {
            C::Extension(command) => f.debug_tuple("Extension").field(command).finish(),
            C::Mapping(command) => f.debug_tuple("Mapping").field(command).finish(),
            C::WifiConnect { ssid, password } => f
                .debug_struct("WifiConnect")
                .field("ssid", ssid)
                .field("password", &password.as_ref().map(|_| REDACTED))
                .finish(),
            C::CloudLogin { email, .. } => f
                .debug_struct("CloudLogin")
                .field("email", email)
                .field("password", &REDACTED)
                .finish(),
            C::Rejected(error) => f.debug_tuple("Rejected").field(error).finish(),
            C::SelectScene { scene } => show!("SelectScene", scene),
            C::Directory(request) => show!("Directory", request),
            C::InspectCell { position } => show!("InspectCell", position),
            C::RequestPreset { id } => show!("RequestPreset", id),
            C::PreviewPreset { id } => show!("PreviewPreset", id),
            C::CycleMode => show!("CycleMode"),
            C::BeginBankPreview => show!("BeginBankPreview"),
            C::EndBankPreview => show!("EndBankPreview"),
            C::CancelBankPreview => show!("CancelBankPreview"),
            C::SetSceneLabel { scene, label } => show!("SetSceneLabel", scene, label),
            C::SetSceneColor { scene, raw_color } => show!("SetSceneColor", scene, raw_color),
            C::SavePreset => show!("SavePreset"),
            C::SavePresetAs {
                name,
                setlist,
                instrument,
            } => show!("SavePresetAs", name, setlist, instrument),
            C::EditPresetDetails { name, instrument } => {
                show!("EditPresetDetails", name, instrument)
            }
            C::CreatePreset => show!("CreatePreset"),
            C::RenamePreset { id, name } => show!("RenamePreset", id, name),
            C::DuplicatePreset { id } => show!("DuplicatePreset", id),
            C::DeletePreset { id } => show!("DeletePreset", id),
            C::DeleteLoadedPreset { id, load } => show!("DeleteLoadedPreset", id, load),
            C::SetFavorite { id, favorite } => show!("SetFavorite", id, favorite),
            C::FavoritePresets { ids } => show!("FavoritePresets", ids),
            C::Upload { items } => show!("Upload", items),
            C::UploadOverwrite { overwrite } => show!("UploadOverwrite", overwrite),
            C::CaptureOverwrite { overwrite } => show!("CaptureOverwrite", overwrite),
            C::CaptureUploadOverwrite { overwrite } => {
                show!("CaptureUploadOverwrite", overwrite)
            }
            C::CancelUpload => show!("CancelUpload"),
            C::DownloadCapture { hash } => show!("DownloadCapture", hash),
            C::BrowseCloud {
                kind,
                scope,
                refresh,
            } => show!("BrowseCloud", kind, scope, refresh),
            C::DownloadProduct { id } => show!("DownloadProduct", id),
            C::DownloadProducts { ids } => show!("DownloadProducts", ids),
            C::DownloadPresets {
                ids,
                setlist,
                to,
                overwrite,
            } => {
                show!("DownloadPresets", ids, setlist, to, overwrite)
            }
            C::StartAddToGrid(media) => show!("StartAddToGrid", media),
            C::AddCapture { position, key } => show!("AddCapture", position, key),
            C::RefreshCaptures => show!("RefreshCaptures"),
            C::AddToGrid { position, media } => show!("AddToGrid", position, media),
            C::CancelAddToGrid => show!("CancelAddToGrid"),
            C::OverwriteIr { id, name, other } => show!("OverwriteIr", id, name, other),
            C::LoadFavorite { index } => show!("LoadFavorite", index),
            C::RemoveFavorite { index } => show!("RemoveFavorite", index),
            C::CopyPresets {
                ids,
                from,
                setlist,
                remove,
                to,
            } => show!("CopyPresets", ids, from, setlist, remove, to),
            C::OpenSetlist { id } => show!("OpenSetlist", id),
            C::CreateSetlist { name } => show!("CreateSetlist", name),
            C::Undo => show!("Undo"),
            C::Redo => show!("Redo"),
            C::CopyScene { from, to } => show!("CopyScene", from, to),
            C::SwapScene { first, second } => show!("SwapScene", first, second),
            C::ClearBlockSelection => show!("ClearBlockSelection"),
            C::AddBlock { position, model_id } => show!("AddBlock", position, model_id),
            C::ReplaceBlock { position, model_id } => show!("ReplaceBlock", position, model_id),
            C::MoveBlock { from, to, side } => show!("MoveBlock", from, to, side),
            C::RemoveBlock { position } => show!("RemoveBlock", position),
            C::CopyBlock { position } => show!("CopyBlock", position),
            C::PasteBlock { position } => show!("PasteBlock", position),
            C::SetBlockBypass { position, bypassed } => {
                show!("SetBlockBypass", position, bypassed)
            }
            C::ResetBlockDefaults { position } => show!("ResetBlockDefaults", position),
            C::SetRowInput { row, port } => show!("SetRowInput", row, port),
            C::SetRowOutput { row, port } => show!("SetRowOutput", row, port),
            C::SetSplit { pair, column } => show!("SetSplit", pair, column),
            C::SetMix { pair, column } => show!("SetMix", pair, column),
            C::NudgeParameter {
                position,
                parameter,
                steps,
            } => show!("NudgeParameter", position, parameter, steps),
            C::SetPosition {
                position,
                parameter,
                value,
            } => show!("SetPosition", position, parameter, value),
            C::SelectText {
                position,
                parameter,
                text,
            } => show!("SelectText", position, parameter, text),
            C::ToggleStomp { switch } => show!("ToggleStomp", switch),
            C::RenameStomp { switch, label } => show!("RenameStomp", switch, label),
            C::StepBank { up } => show!("StepBank", up),
            C::Footswitch {
                switch,
                pressed,
                target,
            } => show!("Footswitch", switch, pressed, target),
            C::SetModeAllowed { mode, allowed } => show!("SetModeAllowed", mode, allowed),
            C::SetHybridRows { rows } => show!("SetHybridRows", rows),
            C::ChangeSetting { id, value } => show!("ChangeSetting", id, value),
            C::Device { action } => show!("Device", action),
            C::ExpressionCalibration { pedal, save } => {
                show!("ExpressionCalibration", pedal, save)
            }
            C::ShowTuner { show } => show!("ShowTuner", show),
            C::Looper { action } => show!("Looper", action),
            C::LooperView { shown } => show!("LooperView", shown),
            C::LooperAssign { tile } => show!("LooperAssign", tile),
            C::ResetLooperActions => show!("ResetLooperActions"),
            C::DspErrorDialog { reboot } => show!("DspErrorDialog", reboot),
            C::DevicePreset { position, request } => show!("DevicePreset", position, request),
            C::SettingsPreset { page, request } => show!("SettingsPreset", page, request),
            C::QuickSplit { pair, split, mix } => show!("QuickSplit", pair, split, mix),
            C::Expression { position, request } => show!("Expression", position, request),
            C::TapTempo => show!("TapTempo"),
            C::NudgeTempo { steps } => show!("NudgeTempo", steps),
            C::Capture { request } => show!("Capture", request),
            C::Nc2 { request } => show!("Nc2", request),
            C::SaveCapture {
                name,
                device,
                instrument,
            } => show!("SaveCapture", name, device, instrument),
            C::WifiScan => show!("WifiScan"),
            C::WifiForget { ssid } => show!("WifiForget", ssid),
            C::WifiDisconnect => f.write_str("WifiDisconnect"),
            C::Updater { command } => show!("Updater", command),
            C::TetradUpdate { command } => show!("TetradUpdate", command),
            C::SetRemote { enabled } => show!("SetRemote", enabled),
            C::CreateBackup { name } => show!("CreateBackup", name),
            C::RenameBackup { id, name } => show!("RenameBackup", id, name),
            C::RestoreBackup { id } => show!("RestoreBackup", id),
            C::DeleteBackup { id } => show!("DeleteBackup", id),
            C::UpdateBackup { id } => show!("UpdateBackup", id),
            C::SetModeCycle { modes, hybrid } => show!("SetModeCycle", modes, hybrid),
            C::SetLocked { locked } => show!("SetLocked", locked),
            C::BeRightBack { active } => show!("BeRightBack", active),
            C::SetPresetMidi { target, messages } => show!("SetPresetMidi", target, messages),
            C::AssignStomp { position, tile } => show!("AssignStomp", position, tile),
            C::InspectSite { site } => show!("InspectSite", site),
            C::NudgeSite {
                site,
                parameter,
                steps,
            } => show!("NudgeSite", site, parameter, steps),
            C::SetMomentary { tile, momentary } => show!("SetMomentary", tile, momentary),
            C::SetSceneMode {
                position,
                parameter,
                enabled,
            } => show!("SetSceneMode", position, parameter, enabled),
            C::CycleModel { position, step } => show!("CycleModel", position, step),
            C::BeginSceneCopy { source, swap } => show!("BeginSceneCopy", source, swap),
            C::EndSceneCopy => f.write_str("EndSceneCopy"),
            C::LinkMedia {
                folder,
                ids,
                linked,
            } => show!("LinkMedia", folder, ids, linked),
            C::DuplicateMediaFolder { folder } => show!("DuplicateMediaFolder", folder),
            C::SetSavingDestination { folder } => show!("SetSavingDestination", folder),
            C::PinModel { model, pinned } => show!("PinModel", model, pinned),
            C::EnterParameter {
                target,
                parameter,
                text,
            } => show!("EnterParameter", target, parameter, text),
            C::SetDeviceName { name } => show!("SetDeviceName", name),
            C::ShowLicence { package } => show!("ShowLicence", package),
            C::RestoreQuarantinedCapture { id, downloads } => {
                show!("RestoreQuarantinedCapture", id, downloads)
            }
        }
    }
}

/// An item of a Directory upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadItem {
    /// A slot of the browsed setlist (tetrad-ui); tetrad-app sends it on as [`Self::Preset`].
    Slot(u64),
    /// Device path of a preset file.
    Preset(String),
    /// Library id of a user capture.
    Capture(String),
    /// Library id of a user IR (stock uploads it as `impulse_response`, `0x7169ac`).
    Ir(String),
}

/// Directory requests use stock paths and database ids, never display-row indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryRequest {
    FavoritePreset {
        path: String,
        favorite: bool,
    },
    RenameSetlist {
        from: String,
        to: String,
    },
    DeleteSetlist {
        name: String,
    },
    /// `overwrite`: an occupied destination is deleted (stock OVERWRITE) instead of
    /// swapped (SWAP, `0x1e8290`).
    MovePreset {
        setlist: String,
        from: usize,
        to: usize,
        overwrite: bool,
    },
    CreateFolder {
        kind: MediaKind,
        parent: i64,
        name: String,
    },
    RenameFolder {
        id: i64,
        name: String,
    },
    DeleteFolder {
        id: i64,
        remove_products: bool,
    },
    RenameMedia {
        kind: MediaKind,
        id: String,
        name: String,
    },
    /// Directory ⋮ "Edit" / "Edit name" on a capture (`onEditClicked` `0x78e49c` →
    /// `NCSaveDialog` edit mode, save `0x68f134`): the name, and the device type and instrument
    /// of the record (`0x7e3664`, `0x7e3694`). `None` keeps the stored value. The name is
    /// already checked (simplified, first letter upper case, 4..=20 UTF-16 units, no other
    /// capture of that name).
    EditCapture {
        id: String,
        name: String,
        device: Option<CaptureDevice>,
        instrument: Option<CaptureInstrument>,
    },
    DeleteMedia {
        kind: MediaKind,
        id: String,
    },
    FavoriteMedia {
        kind: MediaKind,
        folder: String,
        id: String,
        favorite: bool,
    },
    VisitMedia {
        kind: MediaKind,
        folder: String,
        id: String,
    },
    LoadItem {
        kind: ItemKind,
        folder: String,
        id: String,
    },
    Audition {
        kind: ItemKind,
        folder: String,
        id: String,
    },
}

impl DirectoryRequest {
    pub fn writes_library(&self) -> bool {
        !matches!(self, Self::LoadItem { .. } | Self::Audition { .. })
    }
}

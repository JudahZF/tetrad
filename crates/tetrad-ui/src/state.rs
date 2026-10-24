//! UI state: navigation stack, overlays, pointer gestures, footswitch and
//! knob routing. Requests come out as `UiCommand`s; projections come in
//! through `set_snapshot`.

use std::collections::BTreeMap;

use crate::layout::{Layout, ListId, Look};
use crate::render::{HEIGHT, WIDTH};
use crate::screens::PanelKnob;
use crate::screens::model_editor;
use crate::settings::{DeviceAction, SettingId};
use crate::*;

/// Movement before a press becomes a drag or scroll (owned touch policy). Knobs take none,
/// as stock (`UiState::start_knob_drag`).
const DRAG_SLOP: i32 = 10;
/// Touch long press, one value for every hold. Stock holds are 750 ms: `ParameterControl`
/// (`0x4cbf84`), `Button` (`0x4d2380`) and `Selector` (`0x342ce8`). Not checked on the QC yet.
pub(crate) const LONG_PRESS_MS: u64 = 750;
const SWIPE_PX: i32 = 80;
const TOAST_MS: u64 = 2500;
/// Owned: two taps on one parameter knob within this restore its default (manual "Virtual
/// Device Presets"; the stock interval is not recovered).
pub(crate) const DOUBLE_TAP_MS: u64 = 400;
/// The Grid save button shows `saved.png` for 2 s after a save (`QTimer::start(2000)`
/// at `0x7e7eb0`, then `restoreSaveButtonIcon`).
pub(crate) const SAVED_FLASH_MS: u64 = 2000;
/// M406 header status: it shows for 10 s after a deploy switches a global module off
/// (single-shot `QTimer::start(10000)` at `0x5b0340`) and flashes every 500 ms
/// (`QTimer::start(500)` at `0x7e8158`).
pub(crate) const INHIBIT_STATUS_MS: u64 = 10_000;
pub(crate) const INHIBIT_FLASH_MS: u64 = 500;
pub(crate) const TOP_BAR: i32 = 52;
/// Stock grid region starts at y = 96 (`0x005ad2dc`).
pub(crate) const HEADER: i32 = 96;

/// M406 header status (`NavMenuBlock` button `+0x158`, `0x7e7fc8`): the global modules the
/// last deploy switched off. It shows from `since` for [`INHIBIT_STATUS_MS`]; a tap stops the
/// flash (`steady`) and opens the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InhibitStatus {
    pub global_eq: bool,
    pub input_gate: bool,
    pub since: u64,
    pub steady: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct SceneDraft {
    pub entries: [SceneView; SCENE_COUNT],
    pub selected: SceneIndex,
    pub original: [SceneView; SCENE_COUNT],
    pub preset_id: Option<u64>,
}

/// What the on-screen keyboard edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TextTarget {
    SceneLabel,
    PresetSaveAs,
    PresetRename(u64),
    SetlistName,
    DirectoryName(DirectoryNameEdit),
    /// Edit Details of the loaded preset (`modifyPresetMetadataSignal` → `0x5b91dc`).
    PresetDetails,
    LibrarySearch,
    ModelSearch,
    WifiPassword(String),
    CaptureName,
    StompLabel(Footswitch),
    LoginEmail,
    LoginPassword(String),
    /// Virtual Device Preset "Edit Name".
    DevicePresetName(u64),
    /// "Save Current Parameters As…" of the block at this position.
    DevicePresetSaveAs(GridPosition),
    /// Device Information name (`DeviceNameDialog` `0x40efe4`).
    DeviceName,
    /// `BackupEditDialog`: CREATE BACKUP (`None`, empty name) or ⋮ Edit name of a backup.
    BackupName(Option<u64>),
    /// Global EQ or I/O preset: Save Current Parameters as... (`None`) or Edit Name.
    SettingsPresetName {
        page: SettingsPresetPage,
        preset: Option<u64>,
    },
    /// Repositories ADD REPOSITORY: `owner/repo` or its GitHub URL.
    ExtensionSource,
}

impl TextTarget {
    pub fn title(&self) -> &'static str {
        match self {
            Self::DirectoryName(_) => "Edit Name",
            Self::SceneLabel => "Name",
            Self::PresetSaveAs => "Name your Preset",
            Self::PresetRename(_) | Self::StompLabel(_) => "Edit name",
            Self::PresetDetails => "Name your Preset",
            Self::SetlistName => "Name your Setlist",
            Self::LibrarySearch | Self::ModelSearch => "Search for ...",
            Self::WifiPassword(_) | Self::LoginPassword(_) => "Password",
            Self::CaptureName => "Name your Neural Capture",
            Self::LoginEmail => "Email",
            Self::DevicePresetName(_) => "Edit Name",
            Self::DevicePresetSaveAs(_) => "Name your Preset",
            Self::DeviceName => "Device name",
            // The field placeholder (`0x3f4204`); the dialog draws no title of its own (I).
            Self::BackupName(_) => "Backup name",
            Self::SettingsPresetName { preset: None, .. } => "Name your Preset",
            Self::SettingsPresetName { .. } => "Edit Name",
            Self::ExtensionSource => "owner/repo",
        }
    }

    /// The preset save dialog (`PresetSaveDialog` `0x58ed3c`): Save As and Edit Details
    /// (modify flag `+0x178`) have the PREFERRED INSTRUMENT page.
    pub fn has_metadata(&self) -> bool {
        matches!(self, Self::PresetSaveAs | Self::PresetDetails)
    }

    /// The preset save dialogs (re-r44 §4): the stock header and "KeyboardTextfield" replace
    /// the owned title, field and counter.
    pub fn preset_save(&self) -> bool {
        matches!(
            self,
            Self::PresetSaveAs | Self::PresetDetails | Self::DevicePresetSaveAs(_)
        )
    }

    /// Stock names allow 4–20 characters from a restricted set (`0xa26840`).
    pub fn restricted(&self) -> bool {
        matches!(
            self,
            Self::PresetSaveAs
                | Self::PresetRename(_)
                | Self::PresetDetails
                | Self::SetlistName
                | Self::CaptureName
                | Self::DirectoryName(DirectoryNameEdit::Setlist(_))
                | Self::DirectoryName(DirectoryNameEdit::Media {
                    kind: MediaKind::Capture,
                    ..
                })
        )
    }

    /// (minimum, maximum) characters. Names are recovered; the rest are owned
    /// (WPA passphrase 8–63 matches "Invalid password length").
    pub fn limits(&self) -> (usize, usize) {
        match self {
            Self::PresetSaveAs
            | Self::PresetRename(_)
            | Self::PresetDetails
            | Self::SetlistName
            | Self::CaptureName => (4, 20),
            Self::DirectoryName(edit) => edit.limits(),
            Self::SceneLabel | Self::StompLabel(_) => (0, 20),
            Self::LibrarySearch | Self::ModelSearch => (0, 40),
            Self::WifiPassword(_) => (8, 63),
            Self::LoginEmail => (3, 254),
            Self::LoginPassword(_) => (1, 128),
            // Manual: names up to 24 characters.
            Self::DevicePresetName(_)
            | Self::DevicePresetSaveAs(_)
            | Self::SettingsPresetName { .. } => (1, 24),
            // `0x0054a550(edit, 30)`; empty saves the default name.
            Self::DeviceName => (0, 30),
            // `0x54a550(edit, 30)` (`0x3f41f8`). O: an empty name is not sent.
            Self::BackupName(_) => (1, 30),
            // Owned: `owner/repo` (3) up to a long GitHub HTTPS URL.
            Self::ExtensionSource => (3, 140),
        }
    }

    pub fn secret(&self) -> bool {
        matches!(self, Self::WifiPassword(_) | Self::LoginPassword(_))
    }

    pub fn accepts(&self, ch: char) -> bool {
        if self.restricted() {
            ch.is_ascii_alphanumeric() || ch == ' ' || "!#$%&'()-@^_{}~".contains(ch)
        } else {
            !ch.is_control()
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Shift {
    #[default]
    Off,
    Once,
    Locked,
}

#[derive(Clone)]
pub(crate) struct Keyboard {
    pub target: TextTarget,
    pub text: String,
    pub layer: KeyLayer,
    pub shift: Shift,
    /// Save As PREFERRED INSTRUMENT (`PresetMetadataDialog`): the picked instrument and
    /// whether its page shows in place of the keys.
    pub instrument: PreferredInstrument,
    pub metadata_page: bool,
}

/// Passwords never reach a log through `{:?}`.
impl std::fmt::Debug for Keyboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text: &dyn std::fmt::Debug = if self.target.secret() {
            &REDACTED
        } else {
            &self.text
        };
        f.debug_struct("Keyboard")
            .field("target", &self.target)
            .field("text", text)
            .field("layer", &self.layer)
            .field("shift", &self.shift)
            .field("instrument", &self.instrument)
            .field("metadata_page", &self.metadata_page)
            .finish()
    }
}

pub(crate) const REDACTED: &str = "<redacted>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MenuKind {
    Main,
    Block(GridPosition),
    Port {
        row: Row,
        output: bool,
    },
    Backup(u64),
    /// Looper X ⋮ (`0x94d14`).
    Looper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Confirm {
    /// A Directory slot of the browsed setlist.
    DeletePreset(u64),
    /// Grid ⋮ Delete Preset: the loaded preset; the footswitch index and the load generation
    /// (`UiSnapshot::preset_load`) when it opened.
    DeleteLoadedPreset {
        id: u64,
        load: u64,
    },
    Device(DeviceAction),
    RestoreBackup(u64),
    /// "Update Cloud Backup" (`0x3f8730`): CANCEL / PROCEED.
    UpdateBackup(u64),
    DeleteBackup(u64),
    AbortCapture,
    /// "Unsaved capture" for a held Back (`0x3cea8c`); every close answers it.
    DiscardCapture,
    /// "Preset already exists" for a Directory upload (`0x46485c`): OVERWRITE / CANCEL; the
    /// name is `UiSnapshot::upload_overwrite`, and every close answers CANCEL.
    UploadOverwrite,
    /// "Neural Capture already exists" (`0x465178` mode 1) for a cloud capture download:
    /// OVERWRITE / CANCEL; the names are `UiSnapshot::capture_overwrite`, and every close
    /// answers CANCEL.
    CaptureOverwrite,
    /// The same dialog in mode 0 for a capture upload (`UiSnapshot::capture_upload_overwrite`).
    CaptureUploadOverwrite,
    /// "Automatic internet connectivity check" (`0x5d3818`): one ENABLE / DISABLE button.
    InternetCheck,
    /// Calibration in progress; primary saves, cancel discards.
    Expression(u8),
    /// "Download update" off the official channel (`0x42483c`): CANCEL / PROCEED.
    DownloadUpdate,
    /// Owned (Tetrad): UPDATE of a CorOS version that Tetrad does not support
    /// ([`crate::UpdaterView::stock_after_install`]): CANCEL / INSTALL. Stock has no such
    /// question.
    InstallUpdate,
    /// "Reset Looper X Actions" (`0x4be114`): CANCEL / ACCEPT.
    ResetLooperActions,
    /// "Assign footswitch" (`0x3b5424`): a footswitch press assigns the block; UNASSIGN
    /// (when assigned) removes it.
    AssignStomp(GridPosition),
    /// Virtual Device Preset OVERWRITE (`0x39793c`) or DELETE (`0x397cb4`).
    DevicePreset {
        preset: u64,
        delete: bool,
    },
    /// The same confirms from the Global EQ / I/O preset popup (`0x2289a0`).
    SettingsPreset {
        page: SettingsPresetPage,
        preset: u64,
        delete: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChoicePurpose {
    Setting(SettingId),
    RowInput(Row),
    RowOutput(Row),
    /// Settings group dropdown (manual §10).
    SettingsGroup,
    /// Save as... destination setlist (manual §5 "SAVING PRESETS").
    SaveAsSetlist,
    /// EQ filter type of a band's TYPE parameter (`EQFilterTypeSelector` `0x6426d8`).
    FilterType(u16),
    /// A choice setting (index) of the open extension.
    Extension(u16),
    /// A Trigger dropdown of the open mapping.
    Mapping(MappingChoice),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Overlay {
    Menu(MenuKind),
    Confirm(Confirm),
    Choice(ChoicePurpose),
    Info {
        title: std::borrow::Cow<'static, str>,
        body: String,
    },
    /// Gig View EDIT STOMP for a stomp tile (`0x2c56f8`).
    EditStomp(u8),
    /// A device notice in the stock MessageDialog (one button).
    Notice {
        title: String,
        body: String,
        button: &'static str,
    },
    SceneTarget {
        swap: bool,
    },
    Routing(RowPair),
    Modes,
    /// The Modes configuration with its add menu open.
    ModesAdd,
    /// The Modes configuration with its MIDI help open (ⓘ, `0x2d4f00`).
    ModesInfo,
    /// Power & Locking functions (POWER press).
    Power,
    /// Master Volume level and output assignment (knob turn).
    Volume,
    /// M406 `GlobalEqInhibitedPopup` (`0x375b40`, text `0x376204`): the deploy switched
    /// the Global EQ and/or the Input Gate off for CPU load. Opens only from a tap on the
    /// header status ([`InhibitStatus`]).
    GlobalEqInhibited {
        global_eq: bool,
        input_gate: bool,
    },
    /// "Corrupt Neural Capture" (`CorruptCapturesDialog` `0x2b0cf8`, shown by `0x54663c`
    /// at boot `0x3fcf64`) with the removed captures.
    CorruptCaptures {
        names: Vec<String>,
    },
    /// The cloud cannot be used (`0x5b0958`): title, text and one button that opens the
    /// Wi-Fi or My Account page.
    CloudUnavailable(crate::screens::CloudGate),
    /// "Excessive %1 in Downloads folder" (`0x22bd50`) and its "Don't show again" box.
    ExcessiveDownloads {
        kind: crate::ExcessDownloads,
        dont_show: bool,
    },
    /// IRSaveDialog (`0x7e503c`): the new name of IR `id` is the name of user IR `other`.
    /// OVERWRITE deletes `other`, RENAME opens the keyboard again.
    IrExists {
        id: String,
        name: String,
        other: String,
    },
    /// "Overwrite Existing Presets?" (`0x46485c(name, 1)` in a Multi Select run): a cloud
    /// preset download whose target slots are not all empty. OVERWRITE replaces them; SKIP
    /// downloads only the products whose slot is empty.
    DownloadOverwrite {
        ids: Vec<String>,
        setlist: u64,
        slots: Vec<u64>,
    },
    /// Global EQ / I/O preset popup (`0x227a84`) with the row whose ⋮ menu is open.
    SettingsPresets {
        page: SettingsPresetPage,
        menu: Option<u64>,
    },
    /// Owned: REMOVE of an extension or a repository.
    ExtensionConfirm(ExtensionConfirm),
    /// Owned: the host asks to trust this repository's key
    /// ([`crate::ExtensionsView::pending_trust`]); every close answers no.
    ExtensionTrust(crate::PendingTrust),
    /// Owned: the action menu of the open mapping, at this level.
    MappingActions(ActionMenu),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExtensionConfirm {
    Remove { key: String, name: String },
    RemoveSource(String),
}

/// What the Extensions settings page shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum ExtensionsPage {
    #[default]
    List,
    /// The detail page of the installation with this key.
    Detail(String),
    /// Repositories.
    Sources,
}

/// What the Mappings page shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MappingsPage {
    pub tab: MappingScope,
    /// The row whose detail page is open, in `tab`.
    pub detail: Option<MappingId>,
    /// The open row's Values page shows over its detail page. Only a row with more than two
    /// values has one.
    pub values: bool,
}

/// A level of the Mappings action menu: the root or one of its submenus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionMenu {
    Root,
    Preset,
    Scene,
    Tempo,
    Mode,
    Looper,
    /// Bypass and the parameters of this block.
    Block(GridPosition),
}

/// Looper X swap dialog (`zenUI::LooperSwapActionsDialog`, `0x37c7ac`): pick a tile, then
/// press the footswitch that takes its action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LooperSwap {
    Choose,
    /// "Assign footswitch" prompt for the tile at this footswitch index (`0x37ab08`).
    Assign(u8),
}

/// Power & Locking state (manual §3): LOCK drops touch and the volume knob; BE RIGHT
/// BACK also turns the screen and outputs off. A POWER hold (one mini press) ends LOCK;
/// the next POWER press ends BE RIGHT BACK.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PowerState {
    #[default]
    Awake,
    Locked,
    Away,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Picker {
    pub target: Option<GridPosition>,
    pub replace: bool,
    pub category: Option<ModelCategory>,
    pub query: String,
    /// Open Virtual Device List group (step 2); `None` lists the categories.
    pub group: Option<DeviceGroup>,
    pub instrument: Instrument,
    pub presets: Option<PresetPane>,
    /// The Neural Capture group's capture browser.
    pub captures: CaptureBrowse,
}

/// Add device capture browser state (`zenUI::VirtualDevicesCaptures`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CaptureBrowse {
    /// The folder whose captures the list shows.
    pub folder: CaptureFolder,
    /// The library folder the folder pane has entered (`onFolderTapped`, shot sn-5);
    /// `None`: the root view (Favorites, Recent, Downloads, Captures Library).
    pub entered: Option<u16>,
}

/// Virtual Device Presets pane beside the device list (shot 093).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PresetPane {
    pub model_id: u32,
    pub user: bool,
    /// Preset whose row ⋮ menu is open.
    pub menu: Option<u64>,
    /// An audition was sent; closing without ✓ reverts it.
    pub auditioned: bool,
}

/// The media parameter the picker edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MediaPick {
    pub position: GridPosition,
    pub parameter: u16,
    pub kind: MediaKind,
    pub folder: Option<MediaFolder>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) enum Gesture {
    #[default]
    Idle,
    Press {
        control: Option<Control>,
        origin: (i32, i32),
        at: u64,
        long_fired: bool,
    },
    Scroll {
        list: ListId,
        last_y: i32,
    },
    DragBlock {
        from: GridPosition,
    },
    /// A block or full-screen editor parameter knob follows the stock knob drag law
    /// ([`crate::widgets::KnobDrag`]).
    DragKnob {
        target: KnobTarget,
        drag: crate::widgets::KnobDrag,
    },
    /// A settings panel knob cell follows the same law; the value rounds to the setting's
    /// step.
    DragSetting {
        id: SettingId,
        drag: crate::widgets::KnobDrag,
    },
    /// The Tempo page TEMPO knob follows a vertical drag, one BPM per pixel.
    DragTempo {
        anchor_y: i32,
    },
    Swipe {
        origin_y: i32,
    },
    /// A Modes configuration slot follows the finger.
    DragMode {
        from: u8,
    },
    /// A full-screen editor step fader follows a vertical drag: one step per
    /// [`model_editor::STEP_DRAG_PX`].
    DragParam {
        parameter: u16,
        anchor_y: i32,
    },
    /// A sideways drag from a Pattern Tremolo step joins or splits steps on release.
    GroupSteps {
        from: u8,
    },
    /// The step scroller or a cab mic graph follows the finger.
    DragEditor {
        control: EditorControl,
    },
}

/// The knob a [`Gesture::DragKnob`] turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KnobTarget {
    /// A block editor knob (the parameter on the knob's cell of the current page).
    Block(KnobIndex),
    /// A full-screen editor knob of a block parameter.
    Editor(u16),
}

/// What the Support pages show besides their list (3rd Party Licenses, Diagnostics).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SupportView {
    #[default]
    List,
    /// The licence viewer (`0x6cb3c4`) of one package.
    Licence(&'static str),
    Diagnostics(DiagnosticsPage),
}

/// Footswitch Statistics counters of one switch (`0x68b7d0`, `+0x148`/`+0x174`): presses,
/// releases and encoder turns left and right, since boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SwitchStats {
    pub pressed: u32,
    pub released: u32,
    pub left: u32,
    pub right: u32,
}

/// Footswitch bit masks (bit = `Footswitch::index`).
#[derive(Debug, Clone, Copy, Default)]
struct Switches {
    held: u16,
    /// Presses the UI consumed; their releases are dropped.
    consumed: u16,
}

#[derive(Debug, Clone)]
pub(crate) struct Toast {
    pub text: String,
    pub until: u64,
}

#[derive(Debug, Clone)]
pub struct UiState {
    pub(crate) snapshot: UiSnapshot,
    pub(crate) stack: Vec<Screen>,
    pub(crate) draft: Option<SceneDraft>,
    pub(crate) overlay: Option<Overlay>,
    pub(crate) keyboard: Option<Keyboard>,
    pub(crate) pressed: Option<Control>,
    pub(crate) gesture: Gesture,
    pub(crate) pointer: (i32, i32),
    pub(crate) page: usize,
    pub(crate) scroll: BTreeMap<ListId, i32>,
    pub(crate) picker: Picker,
    pub(crate) media: Option<MediaPick>,
    pub(crate) library_query: String,
    pub(crate) param_page: usize,
    /// First shown Pattern Tremolo step fader (`PatternTremoloFaderScroller`, 0..=8).
    pub(crate) step_offset: u8,
    /// Selected EQ editor band tab (`EQModelEditor` `+0x138`); the band count is OUT.
    pub(crate) eq_band: u8,
    /// Cab mic panels in IR mode (`CabsimPanel` `0x8203a4`), mic A then B.
    pub(crate) cab_ir: [bool; 2],
    /// Selected tab of the Arpeggio Delay, voicer and synth editors (`setCurrentTab`).
    pub(crate) editor_tab: u8,
    /// Multivoicer Global bank (`0x627708`).
    pub(crate) editor_bank: u8,
    /// Parallax resize mode (`+0x188`, `0x63625c`): the tall graph and three compact knobs.
    pub(crate) editor_expanded: bool,
    /// The last tap on an EQ node or a graph point, for the double tap
    /// (`bandDoubleTappedSignal`, `voiceDoubleTappedSignal`).
    pub(crate) node_tap: Option<(u8, u64)>,
    pub(crate) focus: Option<KnobIndex>,
    pub(crate) toast: Option<Toast>,
    pub(crate) now: u64,
    pub(crate) show_cpu: bool,
    /// Save as... name and instrument waiting for the destination setlist.
    pub(crate) save_as: Option<(String, PreferredInstrument)>,
    /// Gig View scene tile that started a copy or swap (`0x664954`); otherwise the
    /// selected scene is the source.
    pub(crate) scene_source: Option<SceneIndex>,
    /// Directory category, view, pop-up, Multi Select and search state.
    pub(crate) directory: crate::screens::Directory,
    /// The Grid scene dropdown is open (scene badge, manual "SCENES DROPDOWN").
    pub(crate) scene_menu: bool,
    /// Pedal tab of the expression assignment screen.
    pub(crate) expression_pedal: ExpressionPedal,
    /// Linked parameter whose MIN RANGE / MAX RANGE the expression screen shows.
    pub(crate) expression_param: Option<u16>,
    /// The last tap on a continuous parameter knob, for the double-tap default.
    pub(crate) knob_tap: Option<(KnobIndex, u64)>,
    /// Preset MIDI Out: the target whose message editor is open, and its selected message.
    pub(crate) midi_edit: Option<(MidiOutTarget, usize)>,
    /// A confirmed factory reset runs: the locked progress screen covers everything.
    pub(crate) resetting: bool,
    /// Neural Capture calibration tab and save page draft.
    pub(crate) capture: crate::screens::CaptureUi,
    pub(crate) power: PowerState,
    /// When the POWER press that may end the lock or Be Right Back started.
    pub(crate) power_held: Option<u64>,
    /// The last POWER press: the full-size unlock hold cancels 350 ms after it.
    pub(crate) power_last: u64,
    /// POWER presses are ignored until then (full-size guard timer `+0x18`).
    pub(crate) power_guard_until: u64,
    /// Last Master Volume knob level, 0..=100, and when its overlay closes.
    pub(crate) volume_percent: u8,
    pub(crate) volume_until: u64,
    /// The Grid save button shows its "saved" tick until then.
    pub(crate) saved_until: u64,
    /// M406 header status button while it shows.
    pub(crate) inhibit_status: Option<InhibitStatus>,
    /// Looper X swap dialog state while it shows.
    pub(crate) looper_swap: Option<LooperSwap>,
    /// Licence viewer or Diagnostics sub-page on the Support pages.
    pub(crate) support: SupportView,
    /// Connection › Domain Settings is shown in place of the page (`0x5d3b38`).
    pub(crate) wifi_domains: bool,
    /// Extensions list, detail or Repositories.
    pub(crate) extensions: ExtensionsPage,
    /// The trust question this UI answered, until the host stops asking.
    pub(crate) trust_answered: Option<crate::PendingTrust>,
    /// Mappings tab and detail page.
    pub(crate) mappings: MappingsPage,
    /// Footswitch Statistics rows in stock order `A B C D DOWN E F G H TEMPO UP`.
    pub(crate) switch_stats: [SwitchStats; 11],
    /// Numeric keyboard of a parameter value (`ParameterValueDialog` `0x64b518`).
    pub(crate) numeric: Option<crate::screens::NumericEntry>,
    /// NC hash whose NC editor DOWNLOAD failed: the editor shows the stock failure text
    /// (`0x3c5aa0`) and no DOWNLOAD button.
    pub(crate) capture_download_failed: Option<String>,
    /// Add to Grid mode (Grid `+0x160` capture key / `+0x164` IR key): the media a cell tap
    /// places; the cells are [`UiSnapshot::add_to_grid`].
    pub(crate) add_to_grid: Option<GridMedia>,
    /// The upload overwrite question this UI answered (its preset name), until the device
    /// stops asking (review r31 D1).
    upload_answered: Option<String>,
    /// The capture download and upload questions this UI answered, until the device stops
    /// asking (as `upload_answered`).
    capture_answered: Option<CaptureQuestion>,
    capture_upload_answered: Option<String>,
    /// The last [`crate::NoticeView::id`] shown; a notice waits while another modal is open.
    shown_notice: Option<u64>,
    switches: Switches,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            snapshot: UiSnapshot::default(),
            stack: vec![Screen::Grid],
            draft: None,
            overlay: None,
            keyboard: None,
            pressed: None,
            gesture: Gesture::Idle,
            pointer: (0, 0),
            page: 0,
            scroll: BTreeMap::new(),
            picker: Picker::default(),
            media: None,
            library_query: String::new(),
            param_page: 0,
            step_offset: 0,
            eq_band: 0,
            cab_ir: [false; 2],
            editor_tab: 0,
            editor_bank: 0,
            editor_expanded: false,
            node_tap: None,
            focus: None,
            toast: None,
            now: 0,
            show_cpu: false,
            save_as: None,
            scene_source: None,
            directory: crate::screens::Directory::default(),
            scene_menu: false,
            expression_pedal: ExpressionPedal::Exp1,
            expression_param: None,
            knob_tap: None,
            midi_edit: None,
            resetting: false,
            capture: crate::screens::CaptureUi::default(),
            power: PowerState::Awake,
            power_held: None,
            power_last: 0,
            power_guard_until: 0,
            volume_percent: 0,
            volume_until: 0,
            saved_until: 0,
            inhibit_status: None,
            looper_swap: None,
            support: SupportView::List,
            wifi_domains: false,
            extensions: ExtensionsPage::List,
            trust_answered: None,
            mappings: MappingsPage::default(),
            switch_stats: [SwitchStats::default(); 11],
            numeric: None,
            capture_download_failed: None,
            add_to_grid: None,
            upload_answered: None,
            capture_answered: None,
            capture_upload_answered: None,
            shown_notice: None,
            switches: Switches::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitRegion {
    pub control: Control,
    pub bounds: Rect,
    pub enabled: bool,
    pub label: String,
}

impl UiState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate before replacing anything. A touch in progress survives live
    /// data changes; it is cancelled only when its target is gone. File-sourced Directory
    /// lists and texts are clamped first ([`crate::DirectoryView::clamp`]).
    pub fn set_snapshot(&mut self, mut snapshot: UiSnapshot) -> Result<(), UiError> {
        // A shared directory is clamped by its producer; it is checked only when it changes.
        if !std::sync::Arc::ptr_eq(&snapshot.directory, &self.snapshot.directory)
            && !snapshot.directory.is_clamped()
        {
            std::sync::Arc::make_mut(&mut snapshot.directory).clamp();
        }
        snapshot.validate(&self.snapshot)?;
        let old = std::mem::replace(&mut self.snapshot, snapshot);
        let s = &self.snapshot;
        let selection_changed = s.selected_block != old.selected_block;
        // Stock flashes the tick when a save ends (`0x005b7438` → `0x7e7e8c`). Inferred
        // here: the edited preset (or a first save) turns clean with a file.
        if old.dirty
            && !s.dirty
            && s.preset_file
            && (s.preset_id == old.preset_id || !old.preset_file)
        {
            self.saved_until = self.now + SAVED_FLASH_MS;
        }
        let networks_changed = s.wifi.networks != old.wifi.networks;
        let discard = Some(Overlay::Confirm(Confirm::DiscardCapture));
        if s.capture.unsaved_prompt && !old.capture.unsaved_prompt {
            self.overlay = discard;
        } else if !s.capture.unsaved_prompt && self.overlay == discard {
            self.overlay = None;
        }
        // The device refused or ended the Add to Grid mode (no cells): the overlay goes.
        if s.add_to_grid.is_none() {
            self.add_to_grid = None;
        }
        let questions = [
            (s.upload_overwrite.is_some(), Confirm::UploadOverwrite),
            (s.capture_overwrite.is_some(), Confirm::CaptureOverwrite),
            (
                s.capture_upload_overwrite.is_some(),
                Confirm::CaptureUploadOverwrite,
            ),
        ];
        for (asked, question) in questions {
            if !asked && self.overlay == Some(Overlay::Confirm(question)) {
                self.overlay = None;
            }
        }
        // Stock sets the missing text again at each editor update (`0x3c5d38`), so a DOWNLOAD
        // failure lasts while its editor stays open on that block (review r27 R2; I: the
        // button comes back with the text).
        if selection_changed || s.preset_id != old.preset_id {
            self.param_page = 0;
            self.focus = None;
            self.capture_download_failed = None;
        }
        // `updateCloudFoldersEnabled` (`0x78ff0c`): without the cloud an open cloud folder
        // goes to its local parent (`0x78f628`; review r28 A6).
        if !s.cloud_ready() && self.directory.cloud.is_some() {
            self.directory.cloud = None;
            self.directory.multi = None;
        }
        // A block menu acts on its cell; an external edit makes it stale.
        if let Some(Overlay::Menu(MenuKind::Block(position))) = self.overlay
            && (s.cell(position) != old.cell(position)
                || s.blocks[position.index()] != old.blocks[position.index()])
        {
            self.overlay = None;
        }
        // Review r15 R4: checked and highlighted captures/IRs are positions in `media`; a new
        // list moves them to the same product ids (a gone product drops), so a bulk action
        // never reaches other items.
        if s.media != old.media {
            let moved = |index: u64| {
                let value = &old.media.get(usize::try_from(index).ok()?)?.value;
                let at = s.media.iter().position(|entry| entry.value == *value)?;
                u64::try_from(at).ok()
            };
            let dir = &mut self.directory;
            if dir.category.media().is_some()
                && dir.cloud.is_none()
                && let Some(checked) = &mut dir.multi
            {
                *checked = checked.iter().filter_map(|&index| moved(index)).collect();
            }
            dir.selected_media = dir
                .selected_media
                .and_then(|index| u16::try_from(moved(u64::from(index))?).ok());
        }
        // Checked cloud rows are row positions: new rows keep them on the same products.
        let dir = &mut self.directory;
        if let (Some(kind), Some(scope), Some(checked)) =
            (dir.category.cloud_kind(), dir.cloud, &mut dir.multi)
        {
            let rows = |snapshot: &UiSnapshot| {
                (snapshot.cloud_folder(kind, scope)).map_or(Vec::new(), |folder| {
                    folder.rows.iter().map(|row| row.id.clone()).collect()
                })
            };
            let (before, after) = (rows(&old), rows(s));
            if before != after {
                *checked = (checked.iter())
                    .filter_map(|&index| {
                        let id = before.get(usize::try_from(index).ok()?)?;
                        after.iter().position(|row| row == id).map(|at| at as u64)
                    })
                    .collect();
            }
        }
        // Review r15 R1, R6: Copy/Swap Scene closes when the device leaves the mode, and on a
        // preset load (`0x5b2f40` → `0x5898f4(0)`); the bridge ends a mode left without dialog.
        if let Some(Overlay::SceneTarget { .. }) = self.overlay
            && ((old.scene_copy && !s.scene_copy)
                || s.preset_id != old.preset_id
                || s.preset_slot != old.preset_slot)
        {
            self.overlay = None;
            self.scene_source = None;
        }
        // Review r35 G1 (O): Grid ⋮ Delete Preset acts on the preset loaded when it opened; a
        // load of another file or slot, or a rename, closes it.
        if let Some(Overlay::Confirm(Confirm::DeleteLoadedPreset { .. })) = self.overlay
            && (s.preset_id, &s.preset_title, s.preset_file, s.preset_load)
                != (
                    old.preset_id,
                    &old.preset_title,
                    old.preset_file,
                    old.preset_load,
                )
        {
            self.overlay = None;
        }
        // Review r14 U-1: "Assign footswitch" acts on its block in this preset and mode; a
        // load, a mode change or a changed block closes it (no assignment into another preset).
        if let Some(Overlay::Confirm(Confirm::AssignStomp(position))) = self.overlay
            && (s.preset_id != old.preset_id
                || s.preset_slot != old.preset_slot
                || s.footswitch_mode != old.footswitch_mode
                || s.blocks[position.index()] != old.blocks[position.index()])
        {
            self.overlay = None;
        }
        self.follow_rows(&old);
        self.follow_extensions();
        self.follow_mappings(&old.mappings);
        self.page = self.page.min(self.last_page());
        self.param_page = self.param_page.min(self.last_param_page());
        let stale = match self.gesture {
            Gesture::DragKnob { .. }
            | Gesture::DragParam { .. }
            | Gesture::GroupSteps { .. }
            | Gesture::DragEditor { .. } => selection_changed,
            Gesture::Press {
                control: Some(Control::Wifi(WifiControl::Network(_))),
                ..
            } => networks_changed,
            Gesture::Press {
                control: Some(Control::Extension(_)),
                ..
            } => !crate::screens::extensions::same_targets(
                &old.extensions,
                &self.snapshot.extensions,
            ),
            Gesture::Press {
                control: Some(Control::Mapping(_)),
                ..
            } => !crate::screens::mappings::same_targets(&old.mappings, &self.snapshot.mappings),
            _ => false,
        };
        if stale {
            self.cancel_pointer();
        }
        self.revalidate_gesture();
        self.show_wifi_alert(&old);
        self.show_notice();
        self.show_questions();
        // An application toast shows once, when its id changes.
        if self.snapshot.toast != old.toast
            && let Some(toast) = &self.snapshot.toast
        {
            let text = toast.text.clone();
            self.show_toast(text);
        }
        Ok(())
    }

    /// A new Wi-Fi failure on the Connection page opens its stock popup (one OK button).
    fn show_wifi_alert(&mut self, old: &UiSnapshot) {
        let wifi = &self.snapshot.wifi;
        let Some(alert) = wifi.alert.filter(|_| wifi.alert != old.wifi.alert) else {
            return;
        };
        if self.screen() != Screen::Settings(SettingsPage::Connection) || self.modal() {
            return;
        }
        let (title, body) = alert.popup(wifi.joining.as_deref().unwrap_or_default());
        self.overlay = Some(Overlay::Notice {
            title: title.to_owned(),
            body,
            button: "OK",
        });
    }

    /// Opens the newest device notice once, when no other modal is open.
    fn show_notice(&mut self) {
        let Some(notice) = &self.snapshot.notice else {
            return;
        };
        if self.shown_notice == Some(notice.id) || self.modal() || self.resetting {
            return;
        }
        self.shown_notice = Some(notice.id);
        self.overlay = Some(Overlay::Notice {
            title: notice.title.clone(),
            body: notice.body.clone(),
            button: notice.button,
        });
    }

    /// Fast path for data that changes many times a second: no catalog or
    /// text checks, and the touch in progress is kept.
    pub fn update_live(&mut self, live: LiveView) -> Result<(), UiError> {
        live.validate()?;
        let s = &mut self.snapshot;
        s.tempo = live.tempo;
        s.tuner = live.tuner;
        s.looper = live.looper;
        s.capture.input_levels = live.input_levels;
        s.cpu_percent = live.cpu_percent;
        self.revalidate_gesture();
        Ok(())
    }

    /// Open a screen without a visible control: the application calls this
    /// for Tuner, Tempo or Gig view after it runs tetrad-core's footswitch logic.
    /// The keyboard, overlays and scene draft close first (inferred). Returns
    /// the requests the change implies (`ShowTuner`, a calibration cancel).
    /// Draws the screens a first tap reaches most often into throwaway frames, so their glyphs,
    /// icons and shape masks are in the global caches before the first visit. On the QC the
    /// first I/O page draw built them inline (its 104 discs alone took 13 ms; r53). Changes
    /// nothing in `self`; any thread may run it.
    pub fn warm_caches(&self) {
        let screens = [Screen::Gig, Screen::Presets, Screen::Tuner, Screen::Tempo]
            .into_iter()
            .chain(IoTab::ALL.map(Screen::Io));
        for screen in screens {
            let mut ui = self.clone();
            ui.show(screen);
            let _ = ui.render();
        }
        // The ⋮ menu, and the editor of the first block (on the QC the first ones after boot
        // took 104 and 159 ms against 31–47 and 63–117 ms later; r53).
        let first_block = (0..GRID_ROWS as u8)
            .flat_map(|row| (0..GRID_COLUMNS as u8).map(move |column| (row, column)))
            .filter_map(|(row, column)| GridPosition::new(row, column).ok())
            .find(|&position| self.snapshot.occupied(position));
        let controls = [
            Some(Control::OpenMainMenu),
            first_block.map(Control::SelectCell),
        ];
        for control in controls.into_iter().flatten() {
            let mut ui = self.clone();
            ui.handle_input(InputEvent::Activate(control));
            let _ = ui.render();
        }
    }

    pub fn show(&mut self, screen: Screen) -> Vec<UiCommand> {
        self.cancel_pointer();
        let mut commands = self.close_modals();
        commands.extend(self.navigate(screen));
        commands
    }

    /// Close `screen` and every screen above it. The root Grid stays; a
    /// screen that is not open is a no-op.
    pub fn hide(&mut self, screen: Screen) -> Vec<UiCommand> {
        let was_tuner = self.screen() == Screen::Tuner;
        if let Some(index) = self.stack.iter().position(|open| *open == screen)
            && index > 0
        {
            self.cancel_pointer();
            self.stack.truncate(index);
        }
        self.tuner_transition(was_tuner)
    }

    pub(crate) fn close_modals(&mut self) -> Vec<UiCommand> {
        self.scene_menu = false;
        self.keyboard = None;
        self.draft = None;
        match self.overlay.take() {
            Some(Overlay::Confirm(Confirm::Expression(pedal))) => {
                vec![UiCommand::ExpressionCalibration { pedal, save: false }]
            }
            Some(Overlay::Confirm(Confirm::DiscardCapture)) => vec![UiCommand::Capture {
                request: CaptureRequest::Discard(false),
            }],
            Some(Overlay::Confirm(Confirm::UploadOverwrite)) => self.answer_upload(false),
            Some(Overlay::Confirm(
                question @ (Confirm::CaptureOverwrite | Confirm::CaptureUploadOverwrite),
            )) => self.answer_capture(question, false),
            // Review r15 R2: every exit dismisses (`dismissSceneCopyPasteDialog` `0x5b23c8`).
            Some(Overlay::SceneTarget { .. }) => {
                self.scene_source = None;
                vec![UiCommand::EndSceneCopy]
            }
            Some(Overlay::ExtensionTrust(shown)) => self.answer_trust(shown, false),
            _ => Vec::new(),
        }
    }

    /// The NC2 A/B radio: `Some(true)` selects REFERENCE, `Some(false)` CAPTURE, `None`
    /// toggles (`0x6586e4`); a step to the selected side does nothing.
    fn nc2_audition(&self, reference: Option<bool>) -> Vec<UiCommand> {
        let current = self.snapshot.capture.audition == AuditionSource::Reference;
        let reference = reference.unwrap_or(!current);
        if reference == current {
            return Vec::new();
        }
        let source = if reference {
            AuditionSource::Reference
        } else {
            AuditionSource::Capture
        };
        vec![UiCommand::Nc2 {
            request: Nc2Request::Audition(source),
        }]
    }

    /// Cancel the gesture only when its target no longer exists.
    fn revalidate_gesture(&mut self) {
        let valid = match self.gesture {
            Gesture::Idle | Gesture::Press { control: None, .. } => true,
            Gesture::Press {
                control: Some(control),
                ..
            } => self
                .hit_regions()
                .iter()
                .any(|region| region.enabled && region.control == control),
            Gesture::Scroll { list, .. } => self
                .layout()
                .scrollers
                .iter()
                .any(|scroller| scroller.id == list),
            Gesture::DragBlock { from } => {
                self.screen() == Screen::Grid && !self.modal() && self.snapshot.occupied(from)
            }
            Gesture::DragKnob { target, .. } => {
                self.editing_block().is_some()
                    && !self.modal()
                    && self.knob_parameter(target).is_some()
                    && (matches!(target, KnobTarget::Block(_)) || self.model_editor().is_some())
            }
            Gesture::Swipe { .. } => self.screen().is_main() && !self.modal(),
            Gesture::DragMode { .. } => self.overlay == Some(Overlay::Modes),
            Gesture::DragSetting { id, .. } => !self.modal() && self.snapshot.setting(id).is_some(),
            Gesture::DragTempo { .. } => self.screen() == Screen::Tempo && !self.modal(),
            Gesture::DragParam { .. } | Gesture::GroupSteps { .. } | Gesture::DragEditor { .. } => {
                self.model_editor().is_some() && !self.modal()
            }
        };
        if !valid {
            self.cancel_pointer();
        }
    }

    /// The parameter a knob drag turns.
    fn knob_parameter(&self, target: KnobTarget) -> Option<&ParameterView> {
        match target {
            KnobTarget::Block(knob) => self.parameter_at(knob),
            KnobTarget::Editor(index) => self.snapshot.parameters.iter().find(|p| p.index == index),
        }
    }

    /// Starts the stock knob drag on a press of a knob control. Stock grabs the move at the
    /// first pixel (`0x4cbe7c` arms the drag on press, `0x4c9e20` has no threshold), so knobs
    /// take no [`DRAG_SLOP`].
    fn start_knob_drag(&mut self, control: Option<Control>, y: i32) -> bool {
        let target = match control {
            Some(Control::Param(ParamControl::Knob(knob) | ParamControl::Value(knob))) => {
                Some(KnobTarget::Block(knob))
            }
            Some(Control::Editor(EditorControl::Knob(parameter))) => {
                Some(KnobTarget::Editor(parameter))
            }
            _ => None,
        };
        let gesture = if let Some(target) = target {
            // A toggle has no drag: an editor toggle flips once on release (review r19 F8).
            let Some(drag) = (self.editing_block())
                .and(self.knob_parameter(target))
                .and_then(|parameter| crate::widgets::knob_drag(parameter.kind, y))
            else {
                return false;
            };
            if let KnobTarget::Block(knob) = target {
                self.focus = Some(knob);
            }
            Gesture::DragKnob { target, drag }
        } else if let Some(control @ Control::Setting(SettingControl::Open(id))) = control
            && !self.modal()
            && let Some(SettingValue::Number(raw)) = self.snapshot.setting(id)
            && self.knob_cell(control)
        {
            Gesture::DragSetting {
                id,
                drag: crate::widgets::KnobDrag::new(y, id.knob_position(*raw)),
            }
        } else {
            return false;
        };
        self.pressed = None;
        self.gesture = gesture;
        true
    }

    /// Whether `control` is drawn as a knob (settings cells; a brightness bar is not).
    fn knob_cell(&self, control: Control) -> bool {
        self.layout()
            .items
            .iter()
            .any(|item| item.control == Some(control) && matches!(item.look, Look::Knob { .. }))
    }

    pub(crate) fn parameter_at(&self, knob: KnobIndex) -> Option<&ParameterView> {
        self.snapshot
            .parameters
            .get(self.param_page * KNOB_COUNT + usize::from(knob.get()))
    }

    pub fn snapshot(&self) -> &UiSnapshot {
        &self.snapshot
    }

    pub fn screen(&self) -> Screen {
        *self.stack.last().expect("navigation stack is never empty")
    }

    /// The view draws DSP load: the CPU Monitor header or the Diagnostics pages. Other views
    /// ignore `cpu_percent` and `dsp_diagnostics`, so their updates need no frame.
    pub fn shows_dsp_load(&self) -> bool {
        self.show_cpu || matches!(self.support, SupportView::Diagnostics(_))
    }

    /// Screens from root to top.
    pub fn stack(&self) -> &[Screen] {
        &self.stack
    }

    pub fn scene_dialog_open(&self) -> bool {
        self.draft.is_some()
    }

    /// The Copy/Swap Scene dialog shows: the device scene copy mode needs it.
    pub fn scene_target_open(&self) -> bool {
        matches!(self.overlay, Some(Overlay::SceneTarget { .. }))
    }

    pub fn keyboard_open(&self) -> bool {
        self.keyboard.is_some()
    }

    /// Menu, dialog or picker list above the screen (not the scene draft).
    /// The NC hash whose editor DOWNLOAD failed, while the failure text shows.
    pub fn capture_download_failure(&self) -> Option<&str> {
        self.capture_download_failed.as_deref()
    }

    pub fn overlay_open(&self) -> bool {
        self.overlay.is_some()
    }

    /// "Assign footswitch" or Gig View EDIT STOMP is open. Both set the ModeController block
    /// (`0x5898f4` from `0x4b569c` and `0x2c5710`), so a footswitch press reaches only the
    /// dialog.
    pub fn assign_mode(&self) -> bool {
        matches!(
            self.overlay,
            Some(Overlay::Confirm(Confirm::AssignStomp(_)) | Overlay::EditStomp(_))
        )
    }

    /// LED inputs beyond the screen (`w4-leds-inputs` §11): (keyboard open, its confirm
    /// key accepts the text (`0x50ad88`), Modes configuration open, block editor open).
    pub fn led_state(&self) -> (bool, bool, bool, bool) {
        let confirm = self.keyboard.as_ref().is_some_and(|keyboard| {
            let (min, max) = keyboard.target.limits();
            let text = if keyboard.target.secret() {
                keyboard.text.clone()
            } else {
                simplify_text(&keyboard.text)
            };
            (min..=max).contains(&text.chars().count())
        });
        (
            self.keyboard.is_some(),
            confirm,
            matches!(
                self.overlay,
                Some(Overlay::Modes | Overlay::ModesAdd | Overlay::ModesInfo)
            ),
            self.editing_block().is_some(),
        )
    }

    /// Show a transient message; it expires on a later `Tick`.
    pub fn show_toast(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            until: self.now + TOAST_MS,
        });
    }

    pub fn hit_regions(&self) -> Vec<HitRegion> {
        let layout = self.layout();
        layout.items[layout.layer..]
            .iter()
            .filter_map(|item| {
                Some(HitRegion {
                    control: item.control?,
                    bounds: item.visible()?,
                    enabled: item.enabled,
                    label: item.label.clone(),
                })
            })
            .collect()
    }

    /// The topmost enabled control at a point is the area around a popover
    /// ([`crate::layout::Layout::popover`]).
    fn dismiss_at(&self, x: i32, y: i32) -> bool {
        let layout = self.layout();
        layout.items[layout.layer..]
            .iter()
            .rev()
            .find(|item| {
                item.enabled
                    && item.control.is_some()
                    && item.visible().is_some_and(|bounds| bounds.contains(x, y))
            })
            .is_some_and(|item| matches!(item.look, crate::layout::Look::Dismiss))
    }

    /// Topmost enabled control at a point.
    pub fn hit_test(&self, x: i32, y: i32) -> Option<Control> {
        if x < 0 || y < 0 || x >= WIDTH as i32 || y >= HEIGHT as i32 {
            return None;
        }
        self.hit_regions()
            .into_iter()
            .rev()
            .find(|region| region.enabled && region.bounds.contains(x, y))
            .map(|region| region.control)
    }

    pub fn handle_input(&mut self, input: InputEvent) -> Vec<UiCommand> {
        let commands = self.input(input);
        self.show_questions();
        commands
    }

    /// Review r31 D1: the overwrite questions (preset upload `0x46485c`, capture upload and
    /// download `0x465178`) are level-triggered. Each shows while the device asks, is not
    /// answered yet and no other dialog is open; a device dialog that covered it (capture
    /// prompt, Corrupt Captures, What's New, Excessive Downloads) gives it back when it closes.
    /// It never replaces an open dialog; one question shows at a time (O order: preset upload,
    /// capture upload, capture download).
    fn show_questions(&mut self) {
        self.show_extension_trust();
        let s = &self.snapshot;
        if s.upload_overwrite.is_none() {
            self.upload_answered = None;
        }
        if s.capture_upload_overwrite.is_none() {
            self.capture_upload_answered = None;
        }
        if s.capture_overwrite.is_none() {
            self.capture_answered = None;
        }
        let question = if s.upload_overwrite.is_some() && self.upload_answered != s.upload_overwrite
        {
            Confirm::UploadOverwrite
        } else if s.capture_upload_overwrite.is_some()
            && self.capture_upload_answered != s.capture_upload_overwrite
        {
            Confirm::CaptureUploadOverwrite
        } else if s.capture_overwrite.is_some() && self.capture_answered != s.capture_overwrite {
            Confirm::CaptureOverwrite
        } else {
            return;
        };
        if self.overlay.is_none() {
            self.overlay = Some(Overlay::Confirm(question));
        }
    }

    /// The answer to the upload overwrite question; it does not show again for that name.
    pub(crate) fn answer_upload(&mut self, overwrite: bool) -> Vec<UiCommand> {
        self.upload_answered = self.snapshot.upload_overwrite.clone();
        vec![UiCommand::UploadOverwrite { overwrite }]
    }

    /// The answer to a capture overwrite question; it does not show again for that question.
    /// Only an explicit OVERWRITE sends `true` (O).
    pub(crate) fn answer_capture(&mut self, question: Confirm, overwrite: bool) -> Vec<UiCommand> {
        if question == Confirm::CaptureUploadOverwrite {
            self.capture_upload_answered = self.snapshot.capture_upload_overwrite.clone();
            return vec![UiCommand::CaptureUploadOverwrite { overwrite }];
        }
        self.capture_answered = self.snapshot.capture_overwrite.clone();
        vec![UiCommand::CaptureOverwrite { overwrite }]
    }

    fn input(&mut self, input: InputEvent) -> Vec<UiCommand> {
        if self.power_gate(&input) {
            return Vec::new();
        }
        match input {
            InputEvent::PointerDown { x, y } => {
                // Qt closes a popup on a press outside it (stock ⋮ menu: ~50 ms after the
                // press, QC 2026-10-17); the release then does nothing.
                if self.dismiss_at(x, y) {
                    self.cancel_pointer();
                    return self.activate(Control::Back);
                }
                let control = self.hit_test(x, y);
                self.pointer = (x, y);
                self.pressed = control;
                self.gesture = Gesture::Press {
                    control,
                    origin: (x, y),
                    at: self.now,
                    long_fired: false,
                };
            }
            InputEvent::PointerMove { x, y } => {
                self.pointer = (x, y);
                return self.pointer_move(x, y);
            }
            InputEvent::PointerUp { x, y } => {
                self.pointer = (x, y);
                return self.pointer_up(x, y);
            }
            InputEvent::CancelPointer => self.cancel_pointer(),
            InputEvent::Activate(control) => {
                self.cancel_pointer();
                return self.activate(control);
            }
            InputEvent::Back => {
                self.cancel_pointer();
                return self.back();
            }
            InputEvent::TextChanged(text) => return self.text_changed(text),
            InputEvent::DraftColor(raw_color) => {
                if let Some(draft) = &mut self.draft {
                    draft.entries[usize::from(draft.selected.0)].raw_color = raw_color;
                }
            }
            InputEvent::Tick { now_ms } => return self.tick(now_ms),
            InputEvent::Knob { knob, delta } => {
                // Encoders A–H, BANK DOWN, TEMPO (see `widgets::parameter_panel`).
                const ROWS: [usize; 10] = [0, 1, 2, 3, 5, 6, 7, 8, 4, 9];
                if let Some(row) = ROWS.get(usize::from(knob.get())) {
                    self.count_turn(*row, delta);
                }
                return self.knob(knob, delta);
            }
            InputEvent::Footswitch { switch, pressed } => {
                let stats = &mut self.switch_stats[switch_row(switch)];
                if pressed {
                    stats.pressed = stats.pressed.saturating_add(1);
                } else {
                    stats.released = stats.released.saturating_add(1);
                }
                return self.footswitch(switch, pressed);
            }
            InputEvent::PowerButton { pressed } => return self.power_button(pressed),
            InputEvent::MasterVolume { percent } => self.master_volume(percent),
            InputEvent::BankEncoder { delta } => {
                self.count_turn(10, delta);
                return self.bank_encoder(delta);
            }
        }
        Vec::new()
    }

    /// Visible, enabled controls only: the same rule for pointer and semantic input.
    pub(crate) fn activate(&mut self, control: Control) -> Vec<UiCommand> {
        if !self
            .hit_regions()
            .iter()
            .any(|region| region.enabled && region.control == control)
        {
            return Vec::new();
        }
        self.apply(control)
    }

    pub(crate) fn cancel_pointer(&mut self) {
        self.gesture = Gesture::Idle;
        self.pressed = None;
    }

    pub(crate) fn last_page(&self) -> usize {
        self.snapshot.presets.len().saturating_sub(1) / FOOTSWITCH_COUNT
    }

    pub(crate) fn last_param_page(&self) -> usize {
        if let Some(editor) = self.model_editor() {
            return editor.pages() - 1;
        }
        self.snapshot.parameters.len().saturating_sub(1) / KNOB_COUNT
    }

    pub(crate) fn offset(&self, list: ListId) -> i32 {
        self.scroll.get(&list).copied().unwrap_or(0)
    }

    pub(crate) fn reset_scroll(&mut self, list: ListId) {
        self.scroll.remove(&list);
    }

    /// The parameter panel is open when the application reports a selection.
    pub(crate) fn editing_site(&self) -> Option<EditorSite> {
        (self.screen() == Screen::Grid)
            .then_some(self.snapshot.selected_site)
            .flatten()
    }

    pub(crate) fn editing_block(&self) -> Option<GridPosition> {
        (self.screen() == Screen::Grid)
            .then_some(self.snapshot.selected_block)
            .flatten()
    }

    pub(crate) fn dragging(&self) -> Option<GridPosition> {
        match self.gesture {
            Gesture::DragBlock { from } => Some(from),
            _ => None,
        }
    }

    pub(crate) fn modal(&self) -> bool {
        self.draft.is_some()
            || self.overlay.is_some()
            || self.keyboard.is_some()
            || self.numeric.is_some()
            || self.scene_menu
            || self.snapshot.capture.nc2.is_some()
            || self.snapshot.dsp_error
    }

    fn pointer_move(&mut self, x: i32, y: i32) -> Vec<UiCommand> {
        match self.gesture {
            Gesture::Idle => {}
            Gesture::Press {
                control, origin, ..
            } => {
                let (dx, dy) = (x - origin.0, y - origin.1);
                if dy != 0 && self.start_knob_drag(control, origin.1) {
                    return self.pointer_move(x, y);
                }
                if dx.abs().max(dy.abs()) <= DRAG_SLOP {
                    self.pressed = control.filter(|c| self.hit_test(x, y) == Some(*c));
                    return Vec::new();
                }
                match control {
                    Some(Control::Mode(ModeControl::Slot(from)))
                        if self.overlay == Some(Overlay::Modes) =>
                    {
                        self.pressed = None;
                        self.gesture = Gesture::DragMode { from };
                        return Vec::new();
                    }
                    Some(Control::SelectCell(from))
                        if !self.modal() && self.snapshot.occupied(from) =>
                    {
                        self.pressed = None;
                        self.gesture = Gesture::DragBlock { from };
                        return Vec::new();
                    }
                    Some(Control::Editor(EditorControl::Step(step))) => {
                        self.pressed = None;
                        self.gesture = if dy.abs() > dx.abs() {
                            Gesture::DragParam {
                                parameter: model_editor::step_parameter(step),
                                anchor_y: origin.1,
                            }
                        } else {
                            Gesture::GroupSteps { from: step }
                        };
                        return self.pointer_move(x, y);
                    }
                    Some(Control::Editor(
                        control @ (EditorControl::Scroller
                        | EditorControl::Mic(_)
                        | EditorControl::EqNode(_)
                        | EditorControl::Point(_)
                        | EditorControl::Fader(_)),
                    )) => {
                        self.pressed = None;
                        self.gesture = Gesture::DragEditor { control };
                        return self.editor_drag(control, x, y);
                    }
                    Some(Control::Tempo(crate::TempoControl::Knob))
                        if dy.abs() > dx.abs() && !self.modal() =>
                    {
                        self.pressed = None;
                        self.gesture = Gesture::DragTempo { anchor_y: origin.1 };
                        return self.pointer_move(x, y);
                    }
                    _ => {}
                }
                if origin.1 < TOP_BAR && dy > dx.abs() && self.screen().is_main() && !self.modal() {
                    self.pressed = None;
                    self.gesture = Gesture::Swipe { origin_y: origin.1 };
                    return Vec::new();
                }
                // Swipe up from the bottom edge of the Grid opens Gig View (manual §2).
                if origin.1 >= HEIGHT as i32 - TOP_BAR
                    && -dy > dx.abs()
                    && self.screen() == Screen::Grid
                    && !self.modal()
                {
                    self.pressed = None;
                    self.gesture = Gesture::Swipe { origin_y: origin.1 };
                    return Vec::new();
                }
                if dy.abs() > dx.abs()
                    && let Some(scroller) = self.layout().scroller_at(origin.0, origin.1)
                {
                    self.pressed = None;
                    self.gesture = Gesture::Scroll {
                        list: scroller.id,
                        last_y: origin.1,
                    };
                    return self.pointer_move(x, y);
                }
                self.pressed = control.filter(|c| self.hit_test(x, y) == Some(*c));
            }
            Gesture::Scroll { list, last_y } => {
                let max = self
                    .layout()
                    .scrollers
                    .iter()
                    .find(|scroller| scroller.id == list)
                    .map_or(0, |scroller| scroller.max_offset());
                let before = self.offset(list);
                let offset = (before - (y - last_y)).clamp(0, max);
                self.scroll.insert(list, offset);
                self.gesture = Gesture::Scroll { list, last_y: y };
                if offset == max && before < max {
                    return self.scrolled_to_end(list);
                }
            }
            Gesture::DragKnob { target, mut drag } => {
                let value = drag.follow(y);
                self.gesture = Gesture::DragKnob { target, drag };
                if let Some(value) = value
                    && let Some(position) = self.editing_block()
                    && let Some(parameter) = self.knob_parameter(target)
                {
                    return vec![UiCommand::SetPosition {
                        position,
                        parameter: parameter.index,
                        value: crate::widgets::position_value(value),
                    }];
                }
            }
            Gesture::DragTempo { anchor_y } => {
                let steps = anchor_y - y;
                if steps != 0 {
                    self.gesture = Gesture::DragTempo { anchor_y: y };
                    return vec![UiCommand::NudgeTempo { steps }];
                }
            }
            Gesture::DragSetting { id, mut drag } => {
                let raw = drag.follow(y).and_then(|value| id.raw_at(value));
                self.gesture = Gesture::DragSetting { id, drag };
                if let Some(raw) = raw {
                    return self.set_setting(id, raw);
                }
            }
            Gesture::DragParam {
                parameter,
                anchor_y,
            } => {
                let px = model_editor::STEP_DRAG_PX;
                let steps = (anchor_y - y) / px;
                if steps != 0
                    && let Some(position) = self.editing_block()
                {
                    self.gesture = Gesture::DragParam {
                        parameter,
                        anchor_y: anchor_y - steps * px,
                    };
                    return vec![UiCommand::NudgeParameter {
                        position,
                        parameter,
                        steps,
                    }];
                }
            }
            Gesture::DragEditor { control } => return self.editor_drag(control, x, y),
            Gesture::DragBlock { .. }
            | Gesture::Swipe { .. }
            | Gesture::DragMode { .. }
            | Gesture::GroupSteps { .. } => {}
        }
        Vec::new()
    }

    fn pointer_up(&mut self, x: i32, y: i32) -> Vec<UiCommand> {
        let gesture = self.gesture;
        self.cancel_pointer();
        match gesture {
            Gesture::Press {
                control: Some(control),
                long_fired: false,
                ..
            } if self.hit_test(x, y) == Some(control) => self.activate(control),
            Gesture::DragBlock { from } => {
                if crate::screens::grid::trash_zone().contains(x, y) {
                    return vec![UiCommand::RemoveBlock { position: from }];
                }
                let target = (self.hit_regions().into_iter().rev())
                    .find(|region| region.enabled && region.bounds.contains(x, y));
                match target {
                    Some(region) => match region.control {
                        Control::SelectCell(to) if to != from => {
                            // The slot and the stock item share their centre.
                            let center = region.bounds.x + region.bounds.width as i32 / 2;
                            let side = if x <= center {
                                DropSide::Left
                            } else {
                                DropSide::Right
                            };
                            vec![UiCommand::MoveBlock { from, to, side }]
                        }
                        _ => Vec::new(),
                    },
                    None => Vec::new(),
                }
            }
            Gesture::Swipe { origin_y } if y - origin_y >= SWIPE_PX => {
                self.navigate(Screen::Io(IoTab::default()))
            }
            Gesture::Swipe { origin_y } if origin_y - y >= SWIPE_PX => self.navigate(Screen::Gig),
            Gesture::DragMode { from } => self.drop_mode(from, x, y),
            Gesture::GroupSteps { from } => match self.hit_test(x, y) {
                Some(Control::Editor(EditorControl::Step(to))) => self.group_steps(from, to),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn tick(&mut self, now: u64) -> Vec<UiCommand> {
        self.now = self.now.max(now);
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| self.now >= toast.until)
        {
            self.toast = None;
        }
        // `globalEqGateStatusTimerSlot` (`0x5b02e8`) hides the status; an open popup stays.
        if (self.inhibit_status).is_some_and(|status| self.now >= status.since + INHIBIT_STATUS_MS)
        {
            self.inhibit_status = None;
        }
        self.show_notice();
        // Hold on a parameter assigns it to Scenes or removes it (manual "SCENE
        // ASSIGNMENTS"; shot 109). A drag turns the press into a knob drag first.
        if let Gesture::Press {
            control:
                Some(control @ Control::Param(ParamControl::Knob(knob) | ParamControl::Value(knob))),
            origin,
            at,
            long_fired: false,
        } = self.gesture
            && self.now.saturating_sub(at) >= LONG_PRESS_MS
            && !self.modal()
            && let Some(position) = self.editing_block()
            && let Some(parameter) = self.parameter_at(knob)
        {
            let command = UiCommand::SetSceneMode {
                position,
                parameter: parameter.index,
                enabled: !parameter.scene,
            };
            self.pressed = None;
            self.gesture = Gesture::Press {
                control: Some(control),
                origin,
                at,
                long_fired: true,
            };
            return vec![command];
        }
        // The same hold on a full-screen editor knob or step fader (`paramControlLongPressSlot`).
        if let Gesture::Press {
            control: Some(control @ Control::Editor(editor)),
            origin,
            at,
            long_fired: false,
        } = self.gesture
            && self.now.saturating_sub(at) >= LONG_PRESS_MS
            && !self.modal()
            && let Some(position) = self.editing_block()
            && let Some(parameter) = self.editor_scene_parameter(editor)
        {
            let command = UiCommand::SetSceneMode {
                position,
                parameter: parameter.index,
                enabled: !parameter.scene,
            };
            self.pressed = None;
            self.gesture = Gesture::Press {
                control: Some(control),
                origin,
                at,
                long_fired: true,
            };
            return vec![command];
        }
        // Hold a Virtual Device List row to pin it to the top of its category, or unpin it
        // (manual "BROWSING & LOADING VIRTUAL DEVICES").
        if let Gesture::Press {
            control: Some(control @ Control::Picker(PickerControl::Model(model))),
            origin,
            at,
            long_fired: false,
        } = self.gesture
            && self.now.saturating_sub(at) >= LONG_PRESS_MS
            && !self.modal()
            && self.screen() == Screen::ModelPicker
        {
            self.pressed = None;
            self.gesture = Gesture::Press {
                control: Some(control),
                origin,
                at,
                long_fired: true,
            };
            let pinned = !self.snapshot.pinned_models.contains(&model);
            return vec![UiCommand::PinModel { model, pinned }];
        }
        // Manual "INPUT & OUTPUT BLOCKS": hold an input or output tile for its editor. An
        // output routed to another row has none.
        if let Gesture::Press {
            control: Some(control @ Control::Port { row, output }),
            origin,
            at,
            long_fired: false,
        } = self.gesture
            && self.now.saturating_sub(at) >= LONG_PRESS_MS
            && !self.modal()
            && self.screen() == Screen::Grid
            && !(output
                && matches!(
                    self.snapshot.rows[usize::from(row.get())].output,
                    OutputPort::NextRow3 | OutputPort::NextRow4 | OutputPort::NextRow3And4
                ))
        {
            let site = if output {
                EditorSite::LaneOutput(row)
            } else {
                EditorSite::InputGate(row)
            };
            self.pressed = None;
            self.gesture = Gesture::Press {
                control: Some(control),
                origin,
                at,
                long_fired: true,
            };
            return vec![UiCommand::InspectSite { site }];
        }
        // Hold on an empty slot inserts a Splitter or Mixer for its row pair
        // (manual "SPLITTER & MIXER BLOCKS").
        if let Gesture::Press {
            control: Some(Control::SelectCell(position)),
            origin,
            at,
            long_fired: false,
        } = self.gesture
            && self.now.saturating_sub(at) >= LONG_PRESS_MS
            && !self.modal()
            && self.screen() == Screen::Grid
            && !self.snapshot.bank_preview_active
            && let Some(command) = crate::screens::grid::junction_at(&self.snapshot, position)
        {
            self.pressed = None;
            self.gesture = Gesture::Press {
                control: Some(Control::SelectCell(position)),
                origin,
                at,
                long_fired: true,
            };
            return vec![command];
        }
        // Manual §3: hold OUT 1/2 or 3/4 on the I/O port map to pair or unpair them.
        if let Gesture::Press {
            control: Some(control @ Control::Setting(SettingControl::IoTab(tab))),
            origin,
            at,
            long_fired: false,
        } = self.gesture
            && self.now.saturating_sub(at) >= LONG_PRESS_MS
            && !self.modal()
            && let Some(commands) = self.pair_outputs(tab)
        {
            self.pressed = None;
            self.gesture = Gesture::Press {
                control: Some(control),
                origin,
                at,
                long_fired: true,
            };
            return commands;
        }
        if let Gesture::Press {
            control: Some(control),
            origin,
            at,
            long_fired: false,
        } = self.gesture
            && self.now.saturating_sub(at) >= LONG_PRESS_MS
            && self.long_press(control)
        {
            self.pressed = None;
            self.gesture = Gesture::Press {
                control: Some(control),
                origin,
                at,
                long_fired: true,
            };
        }
        self.overlay_tick()
    }

    /// Long-press routes. Returns whether the press was consumed.
    fn long_press(&mut self, control: Control) -> bool {
        if self.keyboard.is_some() || self.overlay.is_some() {
            return false;
        }
        match control {
            Control::SelectCell(position) if self.draft.is_none() => {
                self.overlay = Some(Overlay::Menu(MenuKind::Block(position)));
            }
            Control::SelectPreset(id) if self.screen() == Screen::Presets => self.preset_menu(id),
            Control::Library(LibraryControl::Bulk(BulkAction::Copy)) => return self.hold_copy(),
            // Stock: ModeButton longPressed opens the modes configuration.
            Control::CycleMode => self.overlay = Some(Overlay::Modes),
            Control::SelectScene(scene)
                if self.draft.is_none() && !self.snapshot.bank_preview_active =>
            {
                self.open_draft();
                if let Some(draft) = &mut self.draft {
                    draft.selected = scene;
                }
            }
            Control::Tile(switch) => {
                let label = switch
                    .tile()
                    .map(|index| self.snapshot.stomps[index].label.clone())
                    .unwrap_or_default();
                self.open_keyboard(TextTarget::StompLabel(switch), label);
            }
            _ => return false,
        }
        true
    }

    fn knob(&mut self, knob: KnobIndex, delta: i32) -> Vec<UiCommand> {
        if delta == 0 || self.keyboard.is_some() {
            return Vec::new();
        }
        // NC2 A/B (`0x3d8094`): encoder 0 steps the radio, CAPTURE (row 0) → REFERENCE (row 1)
        // on a positive turn and back on a negative one. Encoder 1 steps the A/B panel level
        // knob (`+0x16c`, `0x5cca94` by the turn); I: its target behind `0x1ffbac` is the
        // headphone level, as on the V1 A/B page.
        if self.snapshot.capture.nc2 == Some(Nc2Dialog::Ready) {
            let id = SettingId::HeadphoneLevel;
            return match knob.get() {
                0 => self.nc2_audition(Some(delta > 0)),
                1 => (id.step(self.snapshot.setting(id), delta))
                    .map(|value| vec![UiCommand::ChangeSetting { id, value }])
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
        }
        if self.overlay.is_none() && self.draft.is_none() {
            match self.screen() {
                Screen::Grid if let Some(editor) = self.model_editor() => {
                    return self.editor_knob(editor, knob, delta);
                }
                Screen::Grid if self.editing_block().is_some() => {
                    self.focus = Some(knob);
                    return self.nudge_knob(knob, delta);
                }
                Screen::Grid if let Some(site) = self.editing_site() => {
                    self.focus = Some(knob);
                    return self
                        .parameter_at(knob)
                        .map(|parameter| {
                            vec![UiCommand::NudgeSite {
                                site,
                                parameter: parameter.index,
                                steps: delta,
                            }]
                        })
                        .unwrap_or_default();
                }
                screen @ (Screen::Tempo | Screen::Tuner) => {
                    return match crate::screens::panel_knob(screen, knob) {
                        Some(PanelKnob::Bpm) => vec![UiCommand::NudgeTempo { steps: delta }],
                        Some(PanelKnob::Setting(id)) => id
                            .step(self.snapshot.setting(id), delta)
                            .map(|value| vec![UiCommand::ChangeSetting { id, value }])
                            .unwrap_or_default(),
                        None => Vec::new(),
                    };
                }
                Screen::Io(tab) => return self.io_knob(tab, knob, delta),
                Screen::Settings(SettingsPage::Brightness) => {
                    return self.brightness_knob(knob, delta);
                }
                Screen::MidiOut if self.midi_edit.is_some() => {
                    return self.midi_out_knob(knob, delta);
                }
                Screen::GlobalEq(tab) => return self.eq_knob(tab, knob, delta),
                // The encoders under columns 1 and 2 turn MIN RANGE and MAX RANGE.
                Screen::Expression if matches!(knob.0, 1 | 2) => {
                    return self.step_range(knob.0 == 2, Some(delta));
                }
                Screen::Capture => return self.capture_knob(knob, delta),
                _ => {}
            }
        }
        // Otherwise the knob scrolls the topmost list.
        let layout = self.layout();
        if let Some(scroller) = layout
            .scrollers
            .iter()
            .rev()
            .find(|s| s.layer == layout.layer)
        {
            let before = self.offset(scroller.id);
            let offset = (before + delta * 40).clamp(0, scroller.max_offset());
            self.scroll.insert(scroller.id, offset);
            if offset == scroller.max_offset() && before < offset {
                return self.scrolled_to_end(scroller.id);
            }
        }
        Vec::new()
    }

    fn nudge_knob(&mut self, knob: KnobIndex, steps: i32) -> Vec<UiCommand> {
        let Some(position) = self.editing_block() else {
            return Vec::new();
        };
        self.parameter_at(knob)
            .filter(|parameter| !matches!(parameter.kind, ParameterKind::Media(_)))
            .map(|parameter| {
                vec![UiCommand::NudgeParameter {
                    position,
                    parameter: parameter.index,
                    steps,
                }]
            })
            .unwrap_or_default()
    }

    /// Mode addressed by a tile: bank preview shows presets; hybrid splits
    /// rows (upper A–D, lower E–H, stock `0x65dbcc`); an unknown mode shows scenes.
    pub(crate) fn tile_mode(&self, tile: usize) -> FootswitchMode {
        if self.snapshot.bank_preview_active {
            return FootswitchMode::Preset;
        }
        match self.snapshot.footswitch_mode {
            Some(FootswitchMode::Hybrid) if tile < 4 => self.snapshot.hybrid.upper,
            Some(FootswitchMode::Hybrid) => self.snapshot.hybrid.lower,
            Some(mode) => mode,
            None => FootswitchMode::Scene,
        }
    }

    /// Index into `bank.presets` for a preset tile: a hybrid preset half has four switches,
    /// so its tiles show the bank's first four presets (tetrad-core `preset_slot`).
    pub(crate) fn preset_offset(&self, tile: usize) -> usize {
        let rows = self.snapshot.hybrid;
        let preset_half = self.snapshot.footswitch_mode == Some(FootswitchMode::Hybrid)
            && [rows.upper, rows.lower].contains(&FootswitchMode::Preset);
        if preset_half { tile % 4 } else { tile }
    }

    fn footswitch_target(&self, switch: Footswitch) -> FootswitchTarget {
        let Some(tile) = switch.tile() else {
            return match switch {
                Footswitch::BankUp => FootswitchTarget::BankUp,
                Footswitch::BankDown => FootswitchTarget::BankDown,
                _ => FootswitchTarget::Unassigned,
            };
        };
        match self.tile_mode(tile) {
            FootswitchMode::Scene => FootswitchTarget::Scene(SceneIndex(tile as u8)),
            FootswitchMode::Stomp if !self.snapshot.stomps[tile].label.is_empty() => {
                FootswitchTarget::Stomp(switch)
            }
            FootswitchMode::Preset => self
                .snapshot
                .bank
                .presets
                .get(self.preset_offset(tile))
                .map_or(FootswitchTarget::Unassigned, |preset| {
                    FootswitchTarget::Preset(preset.id)
                }),
            _ => FootswitchTarget::Unassigned,
        }
    }

    /// Edges go to the application with their target. Footswitch shortcuts
    /// (hold TEMPO, double-tap TEMPO, DOWN+TEMPO) are tetrad-core's and run in
    /// the application, which then calls [`Self::show`]. The UI consumes only
    /// presses with a UI-local meaning, and drops the matching release:
    /// - the destination pick of Copy/Swap Scene;
    /// - BANK UP closes the Tuner and Tempo pages (manual §3 "Tuner", "Tempo");
    /// - on a mini B+D closes the Looper view (`0x11e7040`); on a QC the hint
    ///   shows the UP icon (`0x155e6ac`), so BANK UP closes it (I).
    fn footswitch(&mut self, switch: Footswitch, pressed: bool) -> Vec<UiCommand> {
        let bit = 1u16 << switch.index();
        if !pressed {
            self.switches.held &= !bit;
            if self.switches.consumed & bit != 0 {
                self.switches.consumed &= !bit;
                return Vec::new();
            }
        } else {
            self.switches.held |= bit;
            let held = |s: Footswitch| self.switches.held & (1 << s.index()) != 0;
            let consumed = if self.snapshot.capture.nc2 == Some(Nc2Dialog::Ready) {
                // NC2 A/B (`0x3d8284`): a press of stomp 0 toggles the radio (I: footswitch
                // A); the dialog is modal, so other presses do nothing here.
                Some(match switch {
                    Footswitch::A => self.nc2_audition(None),
                    _ => Vec::new(),
                })
            } else if let Some(LooperSwap::Assign(_)) = self.looper_swap {
                // The device swapped the actions on this edge; other switches are blocked.
                if switch.tile().is_some() {
                    self.looper_swap = Some(LooperSwap::Choose);
                }
                Some(Vec::new())
            } else if let Some(Overlay::SceneTarget { .. }) = self.overlay
                && switch.tile().is_some()
            {
                // The device copies on a scene footswitch other than the source and leaves the
                // mode (`0x20eec4`); the dialog closes when the snapshot shows the end (review
                // r15 R1: a press on the source keeps the mode and the dialog).
                Some(Vec::new())
            } else if let Some(Overlay::EditStomp(_)) = self.overlay
                && let Some(tile) = switch.tile()
            {
                // Review r15 R24: the device gives the press to the dialog only; as in the
                // stock Assign dialog (`0x2c56f8` → `0x3b5424`) the press picks the footswitch,
                // so EDIT STOMP moves to that tile (I).
                self.overlay = Some(Overlay::EditStomp(tile as u8));
                Some(Vec::new())
            } else if let Some(Overlay::Confirm(Confirm::AssignStomp(position))) = self.overlay
                && let Some(tile) = switch.tile()
            {
                self.overlay = None;
                Some(vec![UiCommand::AssignStomp {
                    position,
                    tile: Some(tile as u8),
                }])
            } else {
                let closes = match self.screen() {
                    Screen::Tuner | Screen::Tempo => switch == Footswitch::BankUp,
                    Screen::Looper if self.snapshot.looper.mini => {
                        held(Footswitch::B) && held(Footswitch::D)
                    }
                    Screen::Looper => switch == Footswitch::BankUp,
                    _ => false,
                };
                closes.then(|| self.pop())
            };
            if let Some(commands) = consumed {
                self.switches.consumed |= bit;
                return commands;
            }
        }
        vec![UiCommand::Footswitch {
            switch,
            pressed,
            target: self.footswitch_target(switch),
        }]
    }

    /// Navigation policy: Grid is the root; Gig sits on Grid; revisiting a
    /// screen on the stack returns to it; settings/I/O pages replace each other.
    pub(crate) fn navigate(&mut self, screen: Screen) -> Vec<UiCommand> {
        let was_tuner = self.screen() == Screen::Tuner;
        match screen {
            Screen::Grid => self.stack = vec![Screen::Grid],
            Screen::Gig => self.stack = vec![Screen::Grid, Screen::Gig],
            _ => {
                if let Some(index) = self.stack.iter().position(|s| *s == screen) {
                    self.stack.truncate(index + 1);
                } else if matches!(
                    (self.screen(), screen),
                    (Screen::Settings(_), Screen::Settings(_))
                        | (Screen::Io(_), Screen::Io(_))
                        | (Screen::GlobalEq(_), Screen::GlobalEq(_))
                ) {
                    *self.stack.last_mut().expect("non-empty") = screen;
                } else {
                    self.stack.push(screen);
                }
            }
        }
        self.tuner_transition(was_tuner)
    }

    pub(crate) fn pop(&mut self) -> Vec<UiCommand> {
        let was_tuner = self.screen() == Screen::Tuner;
        if self.stack.len() > 1 {
            self.stack.pop();
        }
        if self.screen() != Screen::Looper {
            self.looper_swap = None;
        }
        self.tuner_transition(was_tuner)
    }

    fn tuner_transition(&self, was_tuner: bool) -> Vec<UiCommand> {
        let is_tuner = self.screen() == Screen::Tuner;
        if was_tuner == is_tuner {
            Vec::new()
        } else {
            vec![UiCommand::ShowTuner { show: is_tuner }]
        }
    }

    /// Keyboard, then overlay, then scene draft, then bank preview, then the
    /// block selection, then the navigation stack.
    fn count_turn(&mut self, row: usize, delta: i32) {
        let stats = &mut self.switch_stats[row];
        let count = if delta < 0 {
            &mut stats.left
        } else {
            &mut stats.right
        };
        *count = count.saturating_add(delta.unsigned_abs());
    }

    pub(crate) fn back(&mut self) -> Vec<UiCommand> {
        if self.resetting {
            return Vec::new();
        }
        if self.keyboard.take().is_some() || self.numeric.take().is_some() {
            return Vec::new();
        }
        if matches!(self.screen(), Screen::Settings(_)) {
            // The page's back button closes Domain Settings (`0x5d3bec`).
            if std::mem::take(&mut self.wifi_domains) {
                return Vec::new();
            }
            match std::mem::take(&mut self.support) {
                SupportView::List => {}
                SupportView::Licence(_) => return vec![UiCommand::ShowLicence { package: None }],
                SupportView::Diagnostics(_) => return Vec::new(),
            }
        }
        if std::mem::take(&mut self.scene_menu) {
            return Vec::new();
        }
        if let Some(overlay) = self.overlay.take() {
            return match overlay {
                Overlay::ModesAdd | Overlay::ModesInfo => {
                    self.overlay = Some(Overlay::Modes);
                    Vec::new()
                }
                Overlay::Confirm(Confirm::Expression(pedal)) => {
                    vec![UiCommand::ExpressionCalibration { pedal, save: false }]
                }
                Overlay::Confirm(Confirm::DiscardCapture) => vec![UiCommand::Capture {
                    request: CaptureRequest::Discard(false),
                }],
                Overlay::Confirm(Confirm::UploadOverwrite) => self.answer_upload(false),
                Overlay::Confirm(
                    question @ (Confirm::CaptureOverwrite | Confirm::CaptureUploadOverwrite),
                ) => self.answer_capture(question, false),
                // A tap outside the ⋮ menu closes only the menu.
                Overlay::SettingsPresets {
                    page,
                    menu: Some(_),
                } => {
                    self.overlay = Some(Overlay::SettingsPresets { page, menu: None });
                    Vec::new()
                }
                // `dismissSceneCopyPasteDialog` (`0x5ad9d4` connects CANCEL to it).
                Overlay::SceneTarget { .. } => {
                    self.scene_source = None;
                    vec![UiCommand::EndSceneCopy]
                }
                Overlay::ExtensionTrust(shown) => self.answer_trust(shown, false),
                _ => Vec::new(),
            };
        }
        // An extension's detail page and Repositories go back to the Extensions list.
        if self.screen() == Screen::Settings(SettingsPage::Extensions)
            && std::mem::take(&mut self.extensions) != ExtensionsPage::List
        {
            return Vec::new();
        }
        // A mapping's Values page goes back to its detail page, the detail page to the list.
        if self.screen() == Screen::Mappings
            && (std::mem::take(&mut self.mappings.values) || self.mappings.detail.take().is_some())
        {
            return Vec::new();
        }
        if self.screen() == Screen::Presets && self.capture.edit.take().is_some() {
            return Vec::new();
        }
        if (matches!(self.screen(), Screen::Presets | Screen::MediaPicker)
            || self.capture_browser())
            && self.directory.close_layer()
        {
            return Vec::new();
        }
        if let Some(commands) = self.picker_back() {
            return commands;
        }
        if self.draft.take().is_some() {
            return Vec::new();
        }
        if self.snapshot.bank_preview_active {
            return vec![UiCommand::CancelBankPreview];
        }
        if self.editing_block().is_some() || self.editing_site().is_some() {
            return vec![UiCommand::ClearBlockSelection];
        }
        // The editor goes back to the matrix; opened from a mapping, it closes to the mapping.
        if self.screen() == Screen::MidiOut
            && self.midi_edit.take().is_some()
            && self.stack.iter().rev().nth(1) != Some(&Screen::Mappings)
        {
            return Vec::new();
        }
        self.pop()
    }

    pub(crate) fn open_draft(&mut self) {
        self.draft = Some(SceneDraft {
            entries: self.snapshot.scenes.clone(),
            selected: self.snapshot.selected_scene,
            original: self.snapshot.scenes.clone(),
            preset_id: self.snapshot.preset_id,
        });
    }

    pub(crate) fn open_keyboard(&mut self, target: TextTarget, text: String) {
        let max = target.limits().1;
        let text = text
            .chars()
            .filter(|ch| target.accepts(*ch))
            .take(max)
            .collect();
        self.keyboard = Some(Keyboard {
            target,
            text,
            layer: KeyLayer::Letters,
            shift: Shift::Off,
            instrument: self.snapshot.preset_instrument,
            metadata_page: false,
        });
    }

    fn text_changed(&mut self, text: String) -> Vec<UiCommand> {
        if text.len() > MAX_TEXT_BYTES {
            return if self.keyboard.is_some() || self.draft.is_some() {
                vec![UiCommand::Rejected(UiError::TextTooLong)]
            } else {
                Vec::new()
            };
        }
        if let Some(keyboard) = &mut self.keyboard {
            // Same character set and maximum as the on-screen keys.
            let (_, max) = keyboard.target.limits();
            keyboard.text = text
                .chars()
                .filter(|ch| keyboard.target.accepts(*ch))
                .take(max)
                .collect();
            if keyboard.text != text {
                return vec![UiCommand::Rejected(UiError::InvalidName)];
            }
        } else if let Some(draft) = &mut self.draft {
            // Pinned Qt 4.8 content set; Rust strings cannot represent
            // unpaired UTF-16. The backend remains authoritative.
            draft.entries[usize::from(draft.selected.0)].label = simplify_text(&text);
        }
        Vec::new()
    }

    pub(crate) fn layout(&self) -> Layout {
        crate::screens::layout(self)
    }
}

/// Qt `QString::simplified()` whitespace set (see ui-screen-contracts.md).
pub(crate) fn simplify_text(text: &str) -> String {
    text.split(|character| {
        matches!(character,
        '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{180e}' |
        '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}')
    })
    .filter(|word| !word.is_empty())
    .collect::<Vec<_>>()
    .join(" ")
}

/// Stomp rename helper (`0x2c4938`): simplify, then uppercase the first
/// character only when it is an ASCII lowercase letter.
pub(crate) fn stomp_name(text: &str) -> String {
    let simplified = simplify_text(text);
    let mut chars = simplified.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {
            first.to_ascii_uppercase().to_string() + chars.as_str()
        }
        _ => simplified,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DirectoryNameEdit {
    Setlist(String),
    NewFolder { kind: MediaKind, parent: i64 },
    Folder(i64),
    Media { kind: MediaKind, id: String },
}

impl DirectoryNameEdit {
    pub fn limits(&self) -> (usize, usize) {
        match self {
            Self::Media {
                kind: MediaKind::Ir,
                ..
            } => (4, 60),
            Self::Folder(_) | Self::NewFolder { .. } => (4, 60),
            _ => (4, 20),
        }
    }

    pub fn request(self, name: String) -> DirectoryRequest {
        match self {
            Self::Setlist(from) => DirectoryRequest::RenameSetlist { from, to: name },
            Self::NewFolder { kind, parent } => {
                DirectoryRequest::CreateFolder { kind, parent, name }
            }
            Self::Folder(id) => DirectoryRequest::RenameFolder { id, name },
            Self::Media { kind, id } => DirectoryRequest::RenameMedia { kind, id, name },
        }
    }
}

/// Footswitch Statistics row of a switch (stock order `A B C D DOWN E F G H TEMPO UP`).
fn switch_row(switch: Footswitch) -> usize {
    match switch {
        Footswitch::A => 0,
        Footswitch::B => 1,
        Footswitch::C => 2,
        Footswitch::D => 3,
        Footswitch::BankDown => 4,
        Footswitch::E => 5,
        Footswitch::F => 6,
        Footswitch::G => 7,
        Footswitch::H => 8,
        Footswitch::Tempo => 9,
        Footswitch::BankUp => 10,
    }
}

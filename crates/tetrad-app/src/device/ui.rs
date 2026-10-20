//! UI bridge (owner: 2B-ui). Owns the tetrad-ui [`UiState`], projects device [`State`] into its
//! [`UiSnapshot`](tetrad_ui::UiSnapshot), renders 800×480 RGB8 frames for
//! [`Platform::present`](super::Platform::present) and turns touch, knob, footswitch and
//! power-button input into device [`Request`]s.
//!
//! - [`Ui::handle`] is pure over `State`: it runs tetrad-ui and maps every tetrad-ui command to an
//!   [`Effect`], a device [`Action`] (run by `actions::reduce`) or a [`Handoff`] to another
//!   bridge. Commands without a backing become explicit notices.
//! - Footswitch shortcuts (hold TEMPO, double tap, chords) are tetrad-core's, run by the input
//!   bridge; the action reducer asks the UI to show or hide a screen ([`UiInput::Show`]).
//! - The UI owns what is on screen and the device owns the footswitch bank overlay: after
//!   each input the bridge reports a new top screen ([`Action::ScreenShown`]), and the touch
//!   bank preview writes the one overlay ([`Action::BankOverlay`]) that the snapshot shows.
//! - Power menu display rules (slot `0x6a852c`, gate `0x5447e0`) live in `Ui::power_button`.

mod captures;
pub(super) mod picker;
pub(super) mod project;
pub mod views;

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use tetrad_core::conversion::ParameterType;
use tetrad_core::performance::StompIndex;
use tetrad_core::performance::footswitch::BankOverlay;
use tetrad_engine::{Column, Pair, Row, SceneIndex as EngineScene, Site, Slot};
use tetrad_library::stock::ItemList;
use tetrad_midi::LooperButton;
use tetrad_preset::StompRole;
use tetrad_settings::controls::FootswitchMode as StoredMode;
use tetrad_storage::media::Collection;
use tetrad_ui::{
    CellFit, FootswitchMode, GridMedia, GridPosition, InputEvent, LooperAction, MediaEntry,
    ModelEntry, Nc2Request, PasteTarget, Screen, UiCommand as ViewCommand, UiState,
};
use tetrad_watchdog::power::RestartReason;

use super::actions::{Action, CaptureCycle, CycleCapture, LayoutEdit, SceneClipboard};
use super::platform::TouchSample;
use super::preset::{self, PresetEdit, UndoPath};
use super::{Effect, PowerAction, PresetRef, State, Subsystem};
use project::{LibraryView, Local};

/// Owned: pointer moves present at most every 33 ms (about 30 frames per second).
const MOVE_FRAME: Duration = Duration::from_millis(33);
/// Owned on-screen TAP: intervals outside 250..=1500 ms (240..=40 BPM) restart the count.
const TAP_INTERVAL_MS: std::ops::RangeInclusive<u128> = 250..=1500;
/// Owned: a UI, preset, library or settings notice raised within 1 s after a user input
/// answers that input and shows in the stock MessageDialog. Older notices and those of the
/// background subsystems (DSP, inputs, Control, …) stay in the log; services and capture
/// answer only a [`UserJob`].
const NOTICE_WINDOW: Duration = Duration::from_secs(1);
/// Owned: how long a services request waits for its answer. Each cloud step can take up to
/// the 28 s curl timeout (`CURLE_OPERATION_TIMEDOUT`), and a request has two or three steps.
const SERVICES_ANSWER: Duration = Duration::from_secs(90);
/// A footswitch show of the Tempo popup hides it after 4 s: single-shot timer `+0x148`
/// (`0x37ee98`, `timeout()` → `hideDialog()`), `QTimer::start(4000)` at `0x382844` on a timed
/// show (`0x382758`). Touch opens are not timed.
const TEMPO_POPUP_HIDE: Duration = Duration::from_millis(4000);

#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error("not supported yet: {0}")]
    Unsupported(&'static str),
    #[error("{0} is not available")]
    Unavailable(&'static str),
    /// A library read failed while the view refreshed its setlists or slots.
    #[error("{0}")]
    LibraryView(String),
    #[error(transparent)]
    View(#[from] tetrad_ui::UiError),
    #[error(transparent)]
    Library(#[from] tetrad_library::Error),
}

/// One UI input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiInput {
    Touch(TouchSample),
    /// A cab, IR or capture picker commits its selected string parameter.
    TextSelection {
        position: GridPosition,
        index: u32,
        text: String,
    },
    /// Knob, footswitch edge, tick or semantic input. The input bridge sends knob steps and
    /// raw footswitch edges.
    View(InputEvent),
    /// The device asks for a screen (tuner, tempo, Gig View): `UiState::show`.
    Show(Screen),
    /// The device closes a screen and every screen above it: `UiState::hide`.
    Hide(Screen),
    /// Power button edges (`newPowerOnOffEventRead`); the mini long press stays in the input
    /// bridge.
    PowerButton {
        pressed: bool,
    },
    /// The Excessive Downloads check found a folder (`0x22cab0`): show the dialog.
    ExcessiveDownloads(tetrad_storage::stock::DownloadKind),
    /// The capture check removed these captures (display names; boot `0x3fcf64` →
    /// `0x54663c`): show "Corrupt Neural Capture". An empty list shows nothing.
    CorruptCaptures(Vec<String>),
    /// The first boot after an update (`0x54491c`): the CorOS version and release notes.
    UpdateNotes {
        version: String,
        notes: String,
    },
}

/// What the UI asks of the device.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Effect(Effect),
    /// A device state change for the action reducer.
    Action(Action),
    /// Work another bridge owns.
    Handoff(Handoff),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Handoff {
    /// Extensions page requests → extensions bridge.
    Extensions(tetrad_ui::ExtensionCommand),
    /// Mappings page requests → mappings bridge.
    Mappings(tetrad_ui::MappingCommand),
    /// Capture wizard requests and saves → capture bridge.
    Capture(ViewCommand),
    /// The capture screen opened: `tetrad_capture::v2::Nc2Input::DialogShown` (`0x3d791c`).
    Nc2DialogShown,
    /// An NC2 dialog button → `Device::nc2_input`. The close is `hideDialog` (`0x3d7098` →
    /// `0x203ca4`), which reverts the NC2 preset.
    Nc2(tetrad_capture::v2::Nc2Input),
    /// Wi-Fi, updater, backups, cloud login/logout, Wi-Fi reset, support report → services.
    Services(ViewCommand),
    /// Expression calibration start/end → input bridge.
    Input(ViewCommand),
    /// Multi Select DOWNLOAD of cloud presets into the user setlist named `setlist`
    /// (`0x795b18` mode 2). `Device::handoff` turns `to` into slots, checks them with the PASTE
    /// rules, then starts the downloads. `overwrite`: the OVERWRITE answer of "Overwrite
    /// Existing Presets?" (`0x46485c(name, 1)`); else every slot must be empty.
    DownloadPresets {
        ids: Vec<String>,
        setlist: String,
        to: PasteTarget,
        overwrite: bool,
    },
    /// IRSaveDialog OVERWRITE (`0x7e5a30`): user IR `other` is deleted, then IR `id` takes
    /// `name`. `Device::handoff` renames only after a good delete (review r32 E6).
    OverwriteIr {
        id: String,
        name: String,
        other: String,
    },
}

/// A user request that services or capture answers later (owned rule, r27-ui). Its first
/// notice is the answer: a cloud refusal or failure, an upload result, a capture save result.
/// Stock shows these in a MessageDialog (backup download `0x3f926c` "Download failed", upload
/// `0x463810` "Failed to upload Neural Capture"); the NC editor DOWNLOAD shows its failure in
/// the editor (`0x3c5aa0`). Background notices (probe, automatic update check, Control
/// requests, capture engine progress) answer no job and stay in the log.
#[derive(Debug, Clone, PartialEq, Eq)]
enum UserJob {
    /// Any services request but a Wi-Fi scan, answered within [`SERVICES_ANSWER`].
    Services,
    /// Capture save or NC2 save, answered in the same pass (within [`NOTICE_WINDOW`]).
    Capture,
}

impl UserJob {
    fn of(request: &Request) -> Option<Self> {
        use tetrad_capture::v2::Nc2Input;
        match request {
            // A cloud page shows in its folder (rows, or the error); an NC editor DOWNLOAD
            // ends on its typed end ([`Ui::capture_download_end`], review r27 R1).
            // Review r31 D5: CANCEL of an overwrite question ends its job with no answer.
            Request::Handoff(Handoff::Services(
                ViewCommand::WifiScan
                | ViewCommand::BrowseCloud { .. }
                | ViewCommand::DownloadCapture { .. }
                | ViewCommand::UploadOverwrite { overwrite: false }
                | ViewCommand::CaptureOverwrite { overwrite: false }
                | ViewCommand::CaptureUploadOverwrite { overwrite: false }
                | ViewCommand::CancelUpload,
            )) => None,
            Request::Handoff(Handoff::Services(_) | Handoff::DownloadPresets { .. }) => {
                Some(Self::Services)
            }
            Request::Handoff(
                Handoff::Capture(ViewCommand::SaveCapture { .. }) | Handoff::Nc2(Nc2Input::Save),
            ) => Some(Self::Capture),
            _ => None,
        }
    }

    fn answered_by(&self, subsystem: Subsystem, after: Duration) -> bool {
        match self {
            Self::Services => subsystem == Subsystem::Services && after <= SERVICES_ANSWER,
            Self::Capture => subsystem == Subsystem::Capture && after <= NOTICE_WINDOW,
        }
    }
}

/// UTF-16 units of a capture key in a block value (as `media::capture_key`).
const CAPTURE_KEY_UNITS: usize = 64;

/// Add to Grid mode (Grid `+0x160` capture / `+0x164` IR key): the block a cell tap places
/// and the fit of each cell at the mode start.
#[derive(Debug)]
struct AddToGrid {
    media: GridMedia,
    block: Box<tetrad_engine::Instance>,
    cells: [[CellFit; tetrad_ui::GRID_COLUMNS]; tetrad_ui::GRID_ROWS],
    /// The layout the fits were computed for (review r32 E5).
    layout: Box<tetrad_engine::Layout>,
}

/// A frame for the display ([`Ui::render_for_display`]) and the rows, top first, that changed
/// since the display took the last one ([`crate::device::platform::Platform::present_rows`]).
#[derive(Debug)]
pub struct DisplayFrame<'a> {
    pub rgb: &'a [u8],
    pub rows: Range<u32>,
}

#[derive(Debug, Default)]
pub struct Ui {
    view: UiState,
    /// The kept frame; each render repaints only the changed items.
    renderer: tetrad_ui::Renderer,
    /// Frame rows the display does not show yet (top first), if any.
    unshown: Option<Range<u32>>,
    touch_down: bool,
    last_tick_ms: u64,
    last_present: Option<Duration>,
    /// Path and index of the loaded preset; a change resets the selection.
    loaded: Option<(Option<String>, u32)>,
    /// Bumped on each change of `loaded` (`UiSnapshot::preset_load`, review r35 G1).
    load: u64,
    selected: Option<GridPosition>,
    /// The open lane control or junction editor (instead of a block).
    site: Option<tetrad_ui::EditorSite>,
    clipboard: Option<Box<tetrad_engine::Instance>>,
    library: Option<LibraryView>,
    library_stale: bool,
    models: Option<(usize, Arc<Vec<ModelEntry>>)>,
    /// IR and capture names for the media picker ([`Ui::set_media`]).
    media: Arc<Vec<MediaEntry>>,
    /// The `deviceType` of each capture by block value: the loaded ones
    /// ([`Ui::set_capture_devices`]) and the ones a capture block was built for (`built`).
    capture_devices: Arc<BTreeMap<String, tetrad_ui::CaptureDevice>>,
    /// The `deviceType` of the record of each capture block built here (Add device, Add to
    /// Grid): stock writes it into the new block (`0x2ed0e0`), so the Grid glyph shows the
    /// type before the capture loads.
    built_devices: BTreeMap<String, tetrad_ui::CaptureDevice>,
    directory_media: Vec<tetrad_ui::DirectoryMedia>,
    /// The saving destination keys (IR, NC) the library view was built with.
    destinations: (Option<String>, Option<String>),
    last_tap: Option<Duration>,
    looper_pressed: Option<LooperButton>,
    /// When a footswitch-shown Tempo popup hides ([`TEMPO_POPUP_HIDE`]).
    tempo_hide_at: Option<Duration>,
    /// Virtual Device Presets and expression assignment state.
    picker: picker::Picker,
    /// The last touch, knob, footswitch or power input ([`NOTICE_WINDOW`]).
    last_input: Option<Duration>,
    /// [`UiState::warm_caches`] ran (once, when the first preset is loaded).
    warmed: bool,
    /// The open user request and when it went out ([`UserJob`]).
    user_job: Option<(UserJob, Duration)>,
    /// Notices seen so far: (new notices, repeats of the last one). A change is an arrival.
    notice_seq: (u64, u64),
    /// The notice that answered an input; kept until a newer one, so a notice behind a modal
    /// is not lost at the next input (review r14 U-2).
    pending_notice: Option<tetrad_ui::NoticeView>,
    /// Device dialogs shown (Preset Conflict, upload results); their notice ids count down
    /// from `u64::MAX`.
    dialogs: u64,
    /// 3rd party licence the viewer asked for, until `Device::sync_views` reads it.
    licence_request: Option<String>,
    licence: Option<tetrad_ui::LicenceView>,
    /// A RESTORE went out: `Device::sync_views` reads the quarantine list again.
    quarantine_stale: bool,
    /// Global EQ and Input Gate as the last deploy left them (`CompilerInhibitedModules`).
    inhibited: tetrad_engine::Inhibited,
    add_to_grid: Option<AddToGrid>,
    /// The device cipher of capture metadata (name and version) for Add to Grid.
    media_cipher: Option<tetrad_media::factory::DeviceCipher>,
}

impl Ui {
    pub fn new() -> Self {
        Self::default()
    }

    /// The licence package the viewer waits for (read by `Device::sync_views`).
    pub(super) fn take_licence_request(&mut self) -> Option<String> {
        self.licence_request.take()
    }

    /// The Captures Library rows in BANK UP order (`UiState::capture_cycle`). The value is
    /// the 64-unit key, then the name (`0x6f7018`); a key of another length stays alone.
    /// `downloads` is the stored DOWNLOADS rows `Device::sync_views` projects.
    fn capture_cycle(&self, state: &State) -> CaptureCycle {
        let library = (self.view.capture_cycle().into_iter())
            .map(|row| CycleCapture {
                value: match row.value.encode_utf16().count() {
                    CAPTURE_KEY_UNITS => format!("{}{}", row.value, row.name),
                    _ => row.value.to_owned(),
                },
                version: row.version.to_owned(),
            })
            .collect();
        CaptureCycle {
            library,
            downloads: state.views.downloads_cycle.clone(),
        }
    }

    /// The typed end of an NC editor DOWNLOAD (`Services::take_capture_download_ends`,
    /// review r27 R1): a failure shows the editor's failure text (`0x3c5aa0`, no dialog); a
    /// stored capture clears an earlier failure of that hash (R2).
    pub(super) fn capture_download_end(&mut self, hash: String, stored: bool) {
        match stored {
            true => self.view.capture_download_stored(&hash),
            false => self.view.capture_download_failed(hash),
        }
    }

    /// The result dialog of a Directory preset upload (`Services::take_upload_ends`), in the
    /// stock MessageDialog: a failure under the walk end title "Failed to upload Preset"
    /// (`0x73e4cc`; I for a request failure) with its reason; a good upload with an owned
    /// text (stock only closes its progress dialog, `0x7839a4`). It answers the services job,
    /// so the Services notice of the same end stays in the log.
    pub(super) fn upload_end(&mut self, end: super::services::UploadEnd) {
        self.user_job = None;
        // re-r38 §2: a good batch item only moves the bar of the progress dialog.
        if end.batch && end.result.is_ok() {
            return;
        }
        self.dialogs += 1;
        let (title, body) = end.dialog();
        self.pending_notice = Some(tetrad_ui::NoticeView {
            id: u64::MAX - self.dialogs,
            title: title.into(),
            body,
            button: "OK",
        });
    }

    /// The quarantine list changed since `Device::sync_views` last read it.
    pub(super) fn take_quarantine_stale(&mut self) -> bool {
        std::mem::take(&mut self.quarantine_stale)
    }

    pub(super) fn set_licence(&mut self, licence: tetrad_ui::LicenceView) {
        self.licence = Some(licence);
    }

    /// The tetrad-ui state (screen, hit regions) for platforms and tests.
    pub fn view(&self) -> &UiState {
        &self.view
    }

    /// `factory.db` for the factory Virtual Device Presets (`0x008328a4`).
    pub fn set_factory_presets(&mut self, factory: tetrad_storage::stock::StockDb) {
        self.picker.set_factory(factory);
    }

    /// The media picker list (`MediaLibrary::entries`); the device sets it when the media
    /// library loads or the user IRs change.
    pub fn set_media(&mut self, media: Vec<MediaEntry>) {
        self.library_stale = true;
        self.media = media.into();
    }

    /// The capture types the media library has read (`MediaLibrary::capture_devices`); a copy
    /// only when they changed. Returns whether they changed.
    pub fn set_capture_devices(
        &mut self,
        devices: &BTreeMap<String, tetrad_ui::CaptureDevice>,
    ) -> bool {
        let built = (self.built_devices.iter()).filter(|(value, _)| !devices.contains_key(*value));
        let unchanged = self.capture_devices.len() == devices.len() + built.clone().count()
            && (devices.iter().chain(built.clone()))
                .all(|(value, device)| self.capture_devices.get(value) == Some(device));
        if !unchanged {
            let mut merged = devices.clone();
            merged.extend(built.map(|(value, device)| (value.clone(), *device)));
            self.capture_devices = Arc::new(merged);
        }
        !unchanged
    }

    /// The media picker list, for views the bridge does not build (the capture Cab).
    pub fn media(&self) -> &[MediaEntry] {
        &self.media
    }

    /// The media library's device cipher: Add to Grid reads a capture's name and version.
    pub(super) fn set_media_cipher(&mut self, cipher: tetrad_media::factory::DeviceCipher) {
        self.media_cipher = Some(cipher);
    }

    pub fn set_directory_media(&mut self, details: Vec<tetrad_ui::DirectoryMedia>) {
        self.directory_media = details;
        self.library_stale = true;
    }

    /// Start-up build between splash 80 and 90 (`0x5af624`).
    pub fn build(&mut self, state: &State) -> Result<(), UiError> {
        self.library_stale = true;
        self.sync(state)
    }

    /// One packed RGB8 frame (`tetrad_ui::Frame::rgb`) from the kept [`tetrad_ui::Renderer`]
    /// frame: only the changed items are repainted (pixel-equal to a full render; health r34
    /// #9 a). Their rows wait for [`Ui::render_for_display`].
    pub fn render(&mut self, state: &State) -> Result<&[u8], UiError> {
        self.sync(state)?;
        for rect in self.renderer.render(&self.view) {
            let rows = rect.y.max(0) as u32..rect.bottom().max(0) as u32;
            self.unshown = Some(match self.unshown.take() {
                Some(unshown) => unshown.start.min(rows.start)..unshown.end.max(rows.end),
                None => rows,
            });
        }
        Ok(self.renderer.frame().rgb())
    }

    /// [`Ui::render`] for the display: `None` when the display already shows the frame, so an
    /// unchanged pass costs no present (the QC compared and copied 1.1 MB per idle frame).
    /// A failed present must call [`Ui::unshown`].
    pub fn render_for_display(
        &mut self,
        state: &State,
    ) -> Result<Option<DisplayFrame<'_>>, UiError> {
        self.render(state)?;
        let rows = self.unshown.take();
        Ok(rows.map(|rows| DisplayFrame {
            rgb: self.renderer.frame().rgb(),
            rows,
        }))
    }

    /// The display did not take the last frame: present all of it on the next pass.
    pub fn unshown(&mut self) {
        self.unshown = Some(0..tetrad_ui::HEIGHT);
    }

    /// See [`UiState::shows_dsp_load`].
    pub fn shows_dsp_load(&self) -> bool {
        self.view.shows_dsp_load()
    }

    /// The I/O pages draw the I/O meters; Neural Capture draws the IN 1/IN 2 levels.
    pub fn shows_meters(&self) -> bool {
        matches!(self.view.screen(), Screen::Io(_) | Screen::Capture)
    }

    /// One input: tetrad-ui first, then each tetrad-ui command as device requests. A frame is
    /// requested when the view can have changed (moves at most every `MOVE_FRAME`, 33 ms). A new
    /// top screen is reported as [`Action::ScreenShown`]. A clock tick that changes nothing
    /// returns before the snapshot rebuild (the device ticks every 50 ms).
    pub fn handle(&mut self, input: UiInput, state: &State, now: Duration) -> Vec<Request> {
        let mut requests = Vec::new();
        let mut commands = Vec::new();
        let tick = matches!(input, UiInput::View(InputEvent::Tick { .. }));
        if matches!(
            input,
            UiInput::Touch(_) | UiInput::View(_) | UiInput::PowerButton { .. }
        ) && !tick
        {
            self.last_input = Some(now);
        }
        if tick {
            let before = self.key();
            commands = self.tick(now);
            if commands.is_empty()
                && self.key() == before
                && self.editor_block() == state.editor_block
            {
                return requests;
            }
        }
        if let Err(error) = self.sync(state) {
            requests.push(notice(Subsystem::Ui, error.to_string()));
        }
        let before = self.key();
        if !tick {
            commands = self.tick(now);
        }
        let present = match input {
            UiInput::TextSelection {
                position,
                index,
                text,
            } => {
                requests.extend(set_text(position, index, text));
                true
            }
            UiInput::Touch(sample) => match self.pointer(sample) {
                Some(event @ InputEvent::PointerMove { .. }) => {
                    commands.extend(self.pointer_commands(event, &mut requests));
                    self.last_present
                        .is_none_or(|last| now.saturating_sub(last) >= MOVE_FRAME)
                }
                Some(event) => {
                    commands.extend(self.pointer_commands(event, &mut requests));
                    true
                }
                None => false,
            },
            UiInput::View(InputEvent::Tick { .. }) => false,
            UiInput::View(event) => {
                commands.extend(self.pointer_commands(event, &mut requests));
                true
            }
            UiInput::Show(screen) => {
                if self.view.screen() != screen {
                    commands.extend(self.view.show(screen));
                }
                // Each timed show restarts the hide timer (`0x3827b0` stop, then start).
                if screen == Screen::Tempo {
                    self.tempo_hide_at = Some(now + TEMPO_POPUP_HIDE);
                }
                true
            }
            UiInput::Hide(screen) => {
                commands.extend(self.view.hide(screen));
                true
            }
            UiInput::PowerButton { pressed } => {
                commands.extend(self.power_button(pressed, state));
                true
            }
            UiInput::ExcessiveDownloads(kind) => {
                use tetrad_storage::stock::DownloadKind;
                self.view.show_excessive_downloads(match kind {
                    DownloadKind::Capture => tetrad_ui::ExcessDownloads::Captures,
                    DownloadKind::Preset => tetrad_ui::ExcessDownloads::Presets,
                });
                true
            }
            UiInput::CorruptCaptures(names) => {
                self.view.show_corrupt_captures(names);
                true
            }
            UiInput::UpdateNotes { version, notes } => {
                self.view.show_update_notes(&version, notes);
                true
            }
        };
        let changed = !commands.is_empty() || self.key() != before;
        // The snapshot reads bridge state that commands change and the screen (picker, settings
        // page match); an overlay, keyboard or dialog alone needs no new snapshot here (a sync
        // costs 10–22 ms on the QC, and the frame syncs again).
        let resync = !commands.is_empty() || self.view.screen() != before.0;
        for command in commands {
            requests.extend(self.command(command, state, now));
        }
        let cancel = |request: &Request| {
            matches!(
                request,
                Request::Handoff(Handoff::Services(
                    ViewCommand::UploadOverwrite { overwrite: false }
                        | ViewCommand::CaptureOverwrite { overwrite: false }
                        | ViewCommand::CaptureUploadOverwrite { overwrite: false }
                ))
            )
        };
        if requests.iter().any(cancel) {
            self.user_job = None;
        }
        if let Some(job) = requests.iter().rev().find_map(UserJob::of) {
            self.user_job = Some((job, now));
        }
        if self.view.screen() == Screen::Capture && before.0 != Screen::Capture {
            requests.push(Request::Handoff(Handoff::Nc2DialogShown));
        }
        if self.view.screen() != state.screen {
            requests.push(Request::Action(Action::ScreenShown(self.view.screen())));
        }
        // A new block selection reaches the snapshot only through a sync. A library edit of
        // this pass applies after it: its refresh waits for the next sync.
        let edited = std::mem::take(&mut self.library_stale);
        if resync && let Err(error) = self.sync(state) {
            requests.push(notice(Subsystem::Ui, error.to_string()));
        }
        self.library_stale |= edited;
        // The block editor shows: its meters poll (`0x636740`) until it closes or the block
        // changes (r25-device MR 3).
        let editor = self.editor_block();
        if editor != state.editor_block {
            requests.push(Request::Action(Action::EditorBlock(editor)));
        }
        // Review r15 R2: the footswitch scene copy mode lives only with its dialog. A dialog
        // closed without a dismiss (a capture prompt over it, a load) ends the mode
        // (`dismissSceneCopyPasteDialog` `0x5b23c8` → `0x5898f4(0)`).
        if state.perform.scene_clipboard.is_some() && !self.view.scene_target_open() {
            requests.push(Request::Action(Action::SceneCopyMode(None)));
        }
        let (keyboard, confirm_valid, modes_overlay, editor) = self.view.led_state();
        let leds = super::leds::UiLeds {
            keyboard,
            confirm_valid,
            modes_overlay,
            editor,
            standby: self.view.away(),
        };
        if leds != state.ui_leds {
            requests.push(Request::Action(Action::UiLeds(leds)));
        }
        if present || changed {
            self.last_present = Some(now);
            requests.push(Request::Effect(Effect::Present));
        }
        requests
    }

    fn pointer_commands(
        &mut self,
        event: InputEvent,
        requests: &mut Vec<Request>,
    ) -> Vec<ViewCommand> {
        let mut handled = false;
        match event {
            InputEvent::PointerDown { x, y } => {
                if let Some(tetrad_ui::Control::Looper(action)) = self.view.hit_test(x, y) {
                    let button = looper_button(action);
                    self.looper_pressed = Some(button);
                    requests.push(Request::Action(Action::LooperTile {
                        button,
                        pressed: true,
                    }));
                    handled = true;
                }
            }
            InputEvent::PointerUp { .. } | InputEvent::CancelPointer => {
                // `features::looper_tile` applies the tile gates and the release filter.
                if let Some(button) = self.looper_pressed.take() {
                    requests.push(Request::Action(Action::LooperTile {
                        button,
                        pressed: false,
                    }));
                    handled = true;
                }
            }
            _ => {}
        }
        let mut commands = self.view.handle_input(event);
        if handled {
            commands.retain(|command| !matches!(command, ViewCommand::Looper { .. }));
        }
        commands
    }

    /// Feeds tetrad-ui the application clock (long press, toast expiry).
    fn tick(&mut self, now: Duration) -> Vec<ViewCommand> {
        let now_ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
        if now_ms <= self.last_tick_ms {
            return Vec::new();
        }
        self.last_tick_ms = now_ms;
        let mut commands = self.view.handle_input(InputEvent::Tick { now_ms });
        if self.view.screen() != Screen::Tempo {
            self.tempo_hide_at = None;
        } else if self.tempo_hide_at.is_some_and(|at| now >= at) {
            self.tempo_hide_at = None;
            commands.extend(self.view.hide(Screen::Tempo));
        }
        commands
    }

    /// Touch samples → pointer events: press, moves while held, release.
    fn pointer(&mut self, sample: TouchSample) -> Option<InputEvent> {
        let (x, y) = (i32::from(sample.x), i32::from(sample.y));
        let event = match (sample.pressed, self.touch_down) {
            (true, false) => InputEvent::PointerDown { x, y },
            (true, true) => InputEvent::PointerMove { x, y },
            (false, true) => InputEvent::PointerUp { x, y },
            (false, false) => return None,
        };
        self.touch_down = sample.pressed;
        Some(event)
    }

    /// What a frame shows beyond the snapshot, for the "did the view change" test.
    /// The grid block whose editor shows: the Grid is on top and a block is selected.
    fn editor_block(&self) -> Option<Slot> {
        (self.view.screen() == Screen::Grid)
            .then_some(self.view.snapshot().selected_block)
            .flatten()
            .and_then(project::slot_of)
    }

    fn key(&self) -> (Screen, usize, bool, bool, bool) {
        let view = &self.view;
        (
            view.screen(),
            view.stack().len(),
            view.overlay_open(),
            view.keyboard_open(),
            view.scene_dialog_open(),
        )
    }

    /// Power slot `0x6a852c`. Confirmed order: the UI gate `0x5447e0` (app flag `+0x1c0`;
    /// inferred: start-up finished), blocking app states (`0x4e48a8` 2/3; inferred: capture
    /// running) on the edge the slot acts on (full-size press, mini release), then tetrad-ui:
    /// Be Right Back wake, unlock, or the Power & Locking popup (`0x7d5690`). Not modelled:
    /// gates `0x1ec950`/`0x64f464`.
    fn power_button(&mut self, pressed: bool, state: &State) -> Vec<ViewCommand> {
        if !state.splash_dismissed {
            return Vec::new();
        }
        let acting = pressed != state.platform.is_atma();
        if acting && self.view.screen() == Screen::Capture {
            return Vec::new();
        }
        self.view.handle_input(InputEvent::PowerButton { pressed })
    }

    /// One tetrad-ui command → device requests. On an edit stock records for undo, the first
    /// action becomes [`Action::Undoable`]: its frame stays only when the edit commits (a
    /// failed add or deploy leaves no orphan frame; a notice alone pushes none).
    fn command(&mut self, command: ViewCommand, state: &State, now: Duration) -> Vec<Request> {
        // Review r15 R22: a BANK UP turn that keeps the model changes nothing and pushes no
        // frame (`0x4f4ddc` returns 0 before `0x5148b0`).
        let path = undo_path(&command).filter(|_| match command {
            ViewCommand::CycleModel { position, step } => changes_model(state, position, step),
            _ => true,
        });
        let mut requests = self.requests(command, state, now);
        let first = requests
            .iter()
            .position(|request| matches!(request, Request::Action(_)));
        if let (Some(path), Some(index)) = (path, first)
            && let Request::Action(action) = requests.remove(index)
        {
            let undoable = Action::Undoable(path, Box::new(action));
            requests.insert(index, Request::Action(undoable));
        }
        requests
    }

    /// The setlist name of a slot in the browsed setlist, and whether it holds the loaded
    /// preset.
    fn browsed_slot(&self, slot: u64) -> Option<(String, bool)> {
        let library = self.library.as_ref()?;
        let setlist = library.browsed()?;
        let path = library.slots.get(slot as usize).cloned().flatten();
        let loaded = path.is_some()
            && self
                .loaded
                .as_ref()
                .is_some_and(|(loaded, _)| *loaded == path);
        Some((setlist.name.clone(), loaded))
    }

    fn favorite(&self, index: u64) -> Option<String> {
        self.library
            .as_ref()?
            .favorites
            .get(index as usize)
            .cloned()
    }

    fn requests(&mut self, command: ViewCommand, state: &State, now: Duration) -> Vec<Request> {
        use ViewCommand as C;
        let edit = |action| vec![Request::Action(action)];
        let layout = |edit| vec![Request::Action(Action::EditLayout(edit))];
        let slot = project::slot_of;
        match command {
            C::Extension(command) => vec![Request::Handoff(Handoff::Extensions(command))],
            C::Mapping(command) => vec![Request::Handoff(Handoff::Mappings(command))],
            C::SelectScene { scene } => match EngineScene::new(scene.get()) {
                Ok(scene) => edit(Action::SelectScene(scene)),
                Err(error) => vec![notice(Subsystem::Ui, error.to_string())],
            },
            C::InspectCell { position } => {
                self.selected = Some(position);
                self.site = None;
                Vec::new()
            }
            C::InspectSite { site } => {
                self.selected = None;
                self.site = Some(site);
                Vec::new()
            }
            C::ClearBlockSelection => {
                self.selected = None;
                self.site = None;
                Vec::new()
            }
            // Inferred: choosing a preset during bank preview loads it; the load closes the
            // overlay.
            C::RequestPreset { id } | C::PreviewPreset { id } => self.load(id),
            // The touch preview is the footswitch bank overlay (one copy, in `State`).
            C::BeginBankPreview => {
                let footswitch = &state.performance.footswitch;
                overlay(Some(footswitch.bank_size.bank(footswitch.current_preset)))
            }
            C::EndBankPreview | C::CancelBankPreview => overlay(None),
            C::StepBank { up } => {
                let footswitch = &state.performance.footswitch;
                let size = footswitch.bank_size;
                let bank = footswitch
                    .overlay
                    .map_or(size.bank(footswitch.current_preset), |overlay| overlay.bank);
                let last = super::input::last_bank(size);
                overlay(Some(if up {
                    (bank + 1).min(last)
                } else {
                    bank.saturating_sub(1)
                }))
            }
            C::CycleMode => edit(Action::CycleMode),
            C::SetModeCycle { modes, hybrid } => {
                let stored: Vec<StoredMode> = modes
                    .iter()
                    .flat_map(|mode| stored_modes(*mode, hybrid))
                    .collect();
                let current = state.settings.settings().mode.mode();
                let mut requests = edit(Action::SetAllowedModes(stored.clone()));
                // A row swap keeps the active hybrid mode on its new row order.
                if current.slots() == 2
                    && let Some(hybrid) = hybrid_mode(hybrid)
                    && hybrid != current
                    && stored.contains(&hybrid)
                {
                    requests.push(Request::Action(Action::SetMode(hybrid)));
                }
                requests
            }
            // tetrad-ui drops touch while locked; the reducer holds the Master Volume knob and
            // sends Cortex Control the lock notice (`0x16ad60`).
            C::SetLocked { locked } => edit(Action::SetLocked(locked)),
            C::BeRightBack { active } => vec![Request::Action(Action::Standby(active))],
            C::SetModeAllowed { mode, allowed } => {
                let mut modes = state.settings.settings().mode.available().to_vec();
                let changed = stored_modes(mode, self.view.snapshot().hybrid);
                modes.retain(|stored| !changed.contains(stored));
                if allowed {
                    modes.extend(changed);
                }
                edit(Action::SetAllowedModes(modes))
            }
            C::SetHybridRows { rows } => {
                let Some(hybrid) = hybrid_mode(rows) else {
                    return vec![notice(
                        Subsystem::Ui,
                        "hybrid rows must name two different modes".into(),
                    )];
                };
                let settings = state.settings.settings();
                let mut modes: Vec<StoredMode> = settings
                    .mode
                    .available()
                    .iter()
                    .copied()
                    .filter(|mode| mode.slots() == 1)
                    .collect();
                modes.push(hybrid);
                let mut requests = edit(Action::SetAllowedModes(modes));
                if settings.mode.mode().slots() == 2 {
                    requests.push(Request::Action(Action::SetMode(hybrid)));
                }
                requests
            }
            C::SetSceneLabel { scene, label } => match EngineScene::new(scene.get()) {
                Ok(scene) => edit(Action::SceneLabel { scene, label }),
                Err(error) => vec![notice(Subsystem::Ui, error.to_string())],
            },
            C::SetSceneColor { scene, raw_color } => match EngineScene::new(scene.get()) {
                Ok(scene) => edit(Action::SceneColor { scene, raw_color }),
                Err(error) => vec![notice(Subsystem::Ui, error.to_string())],
            },
            C::Rejected(error) => vec![notice(Subsystem::Ui, format!("input refused: {error}"))],
            C::Directory(request) => {
                self.library_stale = true;
                edit(Action::Directory(request))
            }
            C::SavePreset => vec![Request::Effect(Effect::SavePreset(None))],
            // Save As copies a filed preset into the destination setlist and loads the copy;
            // the device saves a preset without a file into its own slot (`0x5ac350`; another
            // preset there: the first empty slot, review r41 L2).
            C::SavePresetAs {
                name,
                setlist,
                instrument,
            } => {
                self.library_stale = true;
                let setlist = setlist.and_then(|id| {
                    let library = self.library.as_ref()?;
                    Some(library.setlists.get(id as usize)?.path.clone())
                });
                vec![Request::Effect(Effect::SavePresetAs {
                    name,
                    setlist,
                    instrument,
                })]
            }
            C::EditPresetDetails { name, instrument } => {
                self.library_stale = true;
                vec![Request::Effect(Effect::EditPresetDetails {
                    name,
                    instrument,
                })]
            }
            // `0x5b3c88`: the stock new preset in the first free slot of the first setlist
            // with one; nothing is saved until Save.
            C::CreatePreset => match state
                .library
                .as_ref()
                .map(|library| super::preset::global_free_slot(&library.presets))
            {
                Some(Ok(Some(target))) => vec![Request::Action(Action::LoadPreset {
                    target,
                    reload: true,
                })],
                Some(Ok(None)) => vec![notice(Subsystem::Library, SETLISTS_FULL.into())],
                Some(Err(error)) => vec![notice(Subsystem::Library, error.to_string())],
                None => vec![notice(Subsystem::Library, "no preset library".into())],
            },
            C::RenamePreset { id, name } => match self.browsed_slot(id) {
                // The loaded preset takes Edit Details: a save under the new name.
                Some((_, true)) => {
                    self.library_stale = true;
                    vec![Request::Effect(Effect::SavePreset(Some(name)))]
                }
                Some((setlist, false)) => {
                    self.library_stale = true;
                    edit(Action::RenamePreset {
                        setlist,
                        slot: id as usize,
                        name,
                    })
                }
                None => vec![notice(Subsystem::Library, "no setlist is open".into())],
            },
            C::DuplicatePreset { id } => match self.browsed_slot(id) {
                Some((setlist, _)) => {
                    self.library_stale = true;
                    edit(Action::DuplicatePreset {
                        setlist,
                        slot: id as usize,
                    })
                }
                None => vec![notice(Subsystem::Library, "no setlist is open".into())],
            },
            C::CopyPresets {
                ids,
                from,
                setlist,
                remove,
                to,
            } => self.copy_presets(ids, from, setlist, remove, to),
            C::DownloadPresets {
                ids,
                setlist,
                to,
                overwrite,
            } => {
                let name = (self.library.as_ref())
                    .and_then(|library| library.setlists.get(setlist as usize))
                    .map(|setlist| setlist.name.clone());
                let Some(setlist) = name else {
                    return vec![notice(Subsystem::Library, "no setlist is open".into())];
                };
                self.library_stale = true;
                vec![Request::Handoff(Handoff::DownloadPresets {
                    ids,
                    setlist,
                    to,
                    overwrite,
                })]
            }
            C::StartAddToGrid(media) => self.start_add_to_grid(state, media),
            C::AddToGrid { position, media } => {
                let Some(mode) = self.add_to_grid.take().filter(|mode| mode.media == media) else {
                    return Vec::new();
                };
                let fit = mode.cells[usize::from(position.row())][usize::from(position.column())];
                match (slot(position), fit) {
                    // `0x59ee7c(.., 0)`: no fit inserts nothing and shows no message.
                    (None, _) | (_, CellFit::NoFit) => Vec::new(),
                    (Some(slot), _) => layout(LayoutEdit::Paste {
                        slot,
                        block: mode.block,
                    }),
                }
            }
            C::CancelAddToGrid => {
                self.add_to_grid = None;
                Vec::new()
            }
            C::RefreshCaptures => {
                self.picker.captures.reread();
                Vec::new()
            }
            // Add device capture row (`0x386024`): the capture block (`0x704530`) at the slot.
            C::AddCapture { position, key } => {
                match (
                    slot(position),
                    self.grid_block(state, &GridMedia::Capture(key)),
                ) {
                    (Some(slot), Ok(block)) => layout(LayoutEdit::Paste {
                        slot,
                        block: Box::new(block),
                    }),
                    (None, _) => Vec::new(),
                    (_, Err(detail)) => {
                        vec![notice(Subsystem::Library, format!("Add device: {detail}"))]
                    }
                }
            }
            // IRSaveDialog OVERWRITE (`0x7e5a30`): the other IR goes (store `vt+0x5c`/`vt+0x4c`,
            // I), then this IR takes the name (`vt+0x50`). O: refused while a block of the
            // loaded preset names the other IR; what stock does to such a block is not traced.
            C::OverwriteIr { id, name, other } => {
                let used = (state.current.as_ref()).is_some_and(|current| {
                    (current.deployment.layout().sites()).any(|(_, block)| {
                        (block
                            .texts
                            .iter()
                            .flat_map(|text| text.scenes.iter().flatten()))
                        .any(|value| *value == other)
                    })
                });
                if used {
                    let detail = format!("IR {other:?} is used by the loaded preset: not deleted");
                    return vec![notice(Subsystem::Library, detail)];
                }
                self.library_stale = true;
                vec![Request::Handoff(Handoff::OverwriteIr { id, name, other })]
            }
            C::DeletePreset { id } => match self.library.as_ref().and_then(LibraryView::browsed) {
                Some(setlist) => {
                    let setlist = setlist.name.clone();
                    self.library_stale = true;
                    edit(Action::DeletePreset {
                        setlist,
                        slot: id as usize,
                    })
                }
                None => vec![notice(
                    Subsystem::Library,
                    "no setlist to delete from".into(),
                )],
            },
            C::DeleteLoadedPreset { id, load } => self.delete_loaded_preset(state, id, load),
            // The Grid contextual menu favorites the loaded preset (manual §5).
            C::SetFavorite { favorite, .. } => {
                match state
                    .current
                    .as_ref()
                    .and_then(|current| current.path.clone())
                {
                    Some(path) => {
                        self.library_stale = true;
                        edit(Action::SetFavorite { path, favorite })
                    }
                    None => vec![notice(
                        Subsystem::Library,
                        "save the preset before adding it to favorites".into(),
                    )],
                }
            }
            // Services builds the `ProductUpload` (`Services::upload_product`, r21-services
            // MR 1); a slot goes on as its preset path.
            C::Upload { items } => {
                let items = items
                    .into_iter()
                    .filter_map(|item| match item {
                        tetrad_ui::UploadItem::Slot(slot) => {
                            let library = self.library.as_ref()?;
                            let path = library.slots.get(slot as usize).cloned().flatten()?;
                            Some(tetrad_ui::UploadItem::Preset(path))
                        }
                        item => Some(item),
                    })
                    .collect();
                vec![Request::Handoff(Handoff::Services(C::Upload { items }))]
            }
            C::FavoritePresets { ids } => {
                let paths: Vec<String> = ids
                    .iter()
                    .filter_map(|&id| {
                        let library = self.library.as_ref()?;
                        library.slots.get(id as usize).cloned().flatten()
                    })
                    .collect();
                self.library_stale |= !paths.is_empty();
                paths
                    .into_iter()
                    .flat_map(|path| {
                        edit(Action::SetFavorite {
                            path,
                            favorite: true,
                        })
                    })
                    .collect()
            }
            C::LoadFavorite { index } => match self.favorite(index) {
                Some(path) => match state
                    .library
                    .as_ref()
                    .map(|library| super::preset::locate(&library.presets, &path))
                {
                    Some(Ok(Some(target))) => vec![Request::Action(Action::LoadPreset {
                        target,
                        reload: true,
                    })],
                    Some(Ok(None)) => vec![notice(
                        Subsystem::Library,
                        format!("{path} is in no setlist slot"),
                    )],
                    Some(Err(error)) => vec![notice(Subsystem::Library, error.to_string())],
                    None => Vec::new(),
                },
                None => Vec::new(),
            },
            C::RemoveFavorite { index } => match self.favorite(index) {
                Some(path) => {
                    self.library_stale = true;
                    edit(Action::SetFavorite {
                        path,
                        favorite: false,
                    })
                }
                None => Vec::new(),
            },
            C::OpenSetlist { id } => {
                if let Some(library) = self.library.as_mut() {
                    library.browse = id as usize;
                }
                let mut requests = self.refresh_slots(state);
                if state.performance.footswitch.overlay.is_some() {
                    requests.extend(overlay(None));
                }
                requests
            }
            C::CreateSetlist { name } => {
                self.library_stale = true;
                edit(Action::CreateSetlist(name))
            }
            C::Undo => edit(Action::Undo),
            C::Redo => edit(Action::Redo),
            C::CopyScene { from, to } => scene_copy(from, to, false),
            C::SwapScene { first, second } => scene_copy(first, second, true),
            C::AddBlock { position, model_id } => self
                .picker
                .add_block(position, model_id, state)
                .or_else(|| {
                    slot(position).map(|slot| {
                        layout(LayoutEdit::Add {
                            slot,
                            model: model_id,
                        })
                    })
                })
                .unwrap_or_default(),
            C::ReplaceBlock { position, model_id } => slot(position)
                .map(|slot| {
                    layout(LayoutEdit::Replace {
                        slot,
                        model: model_id,
                    })
                })
                .unwrap_or_default(),
            C::MoveBlock { from, to, side } => match (slot(from), slot(to)) {
                (Some(source), Some(target)) => {
                    if self.selected == Some(from) {
                        self.selected = Some(to);
                    }
                    let side = match side {
                        tetrad_ui::DropSide::Left => tetrad_engine::DropSide::Left,
                        tetrad_ui::DropSide::Right => tetrad_engine::DropSide::Right,
                    };
                    edit(Action::DropBlock {
                        from: source,
                        to: target,
                        side,
                    })
                }
                _ => Vec::new(),
            },
            C::RemoveBlock { position } => {
                if self.selected == Some(position) {
                    self.selected = None;
                }
                slot(position)
                    .map(|slot| layout(LayoutEdit::Remove(slot)))
                    .unwrap_or_default()
            }
            C::CopyBlock { position } => {
                self.clipboard = state
                    .current
                    .as_ref()
                    .and_then(|current| project::instance(current.deployment.layout(), position))
                    .map(|block| Box::new(block.clone()));
                Vec::new()
            }
            C::PasteBlock { position } => match (slot(position), self.clipboard.clone()) {
                (Some(slot), Some(block)) => layout(LayoutEdit::Paste { slot, block }),
                _ => vec![notice(Subsystem::Ui, "nothing to paste".into())],
            },
            C::SetBlockBypass { position, bypassed } => slot(position)
                .map(|slot| {
                    edit(Action::SetBypass {
                        site: Site::Slot(slot),
                        bypassed,
                    })
                })
                .unwrap_or_default(),
            C::ResetBlockDefaults { position } => slot(position)
                .map(|slot| layout(LayoutEdit::Reset(slot)))
                .unwrap_or_default(),
            C::SetRowInput { row, port } => {
                match (
                    Row::new(row.get()),
                    tetrad_engine::InputPort::from_raw(port as u32),
                ) {
                    (Ok(row), Some(port)) => layout(LayoutEdit::Input { row, port }),
                    _ => Vec::new(),
                }
            }
            C::SetRowOutput { row, port } => {
                match (
                    Row::new(row.get()),
                    tetrad_engine::OutputPort::from_raw(port as u32),
                ) {
                    (Ok(row), Some(port)) => layout(LayoutEdit::Output { row, port }),
                    _ => Vec::new(),
                }
            }
            C::SetSplit { pair, column } => layout(LayoutEdit::Split {
                pair: engine_pair(pair),
                column: column.and_then(|column| Column::new(column).ok()),
            }),
            // One edit and one undo frame for both points (`0x3e19e8` → `0x51a090`).
            C::QuickSplit { pair, split, mix } => {
                let column = |column: Option<u8>| column.and_then(|c| Column::new(c).ok());
                let pair = engine_pair(pair);
                edit(Action::EditLayouts(vec![
                    LayoutEdit::Split {
                        pair,
                        column: column(split),
                    },
                    LayoutEdit::Mix {
                        pair,
                        column: column(mix),
                    },
                ]))
            }
            C::SetMix { pair, column } => layout(LayoutEdit::Mix {
                pair: engine_pair(pair),
                column: column.and_then(|column| Column::new(column).ok()),
            }),
            C::SelectText {
                position,
                parameter,
                text,
            } => set_text(position, u32::from(parameter), text)
                .into_iter()
                .collect(),
            C::NudgeParameter {
                position,
                parameter,
                steps,
            } => match slot(position) {
                Some(slot) => nudge(state, Site::Slot(slot), parameter, steps),
                None => Vec::new(),
            },
            C::SetPosition {
                position,
                parameter,
                value,
            } => match slot(position) {
                Some(slot) => set_position(state, Site::Slot(slot), parameter, value),
                None => Vec::new(),
            },
            C::NudgeSite {
                site,
                parameter,
                steps,
            } => nudge(state, project::engine_site(site), parameter, steps),
            C::ToggleStomp { switch } => {
                match switch.tile().and_then(|tile| StompIndex::new(tile as u32)) {
                    Some(stomp) => edit(Action::ToggleStomp(stomp)),
                    None => Vec::new(),
                }
            }
            C::RenameStomp { switch, label } => switch
                .tile()
                .map(|tile| {
                    edit(Action::StompLabel {
                        stomp: tile as u32,
                        label,
                    })
                })
                .unwrap_or_default(),
            // The input bridge acts on the same HAL edge through tetrad-core
            // (`FootswitchContext`, tap tempo); tetrad-ui uses the edge only to light tiles.
            C::Footswitch { .. } => Vec::new(),
            // Manual §3 OUTPUT PAIRING: the I/O port hold (`0x3608e8`).
            C::ChangeSetting {
                id: tetrad_ui::SettingId::OutputPair(jack),
                value: tetrad_ui::SettingValue::Bool(linked),
            } => edit(Action::PairOutputs {
                pair: match jack {
                    tetrad_ui::OutputJack::Out12 => super::jacks::OutputPair::Out12,
                    tetrad_ui::OutputJack::Out34 => super::jacks::OutputPair::Out34,
                },
                linked,
            }),
            C::ChangeSetting { id, value } => edit(Action::ChangeSetting { id, value }),
            C::SetPresetMidi { target, messages } => {
                edit(Action::SetPresetMidi { target, messages })
            }
            C::AssignStomp { position, tile } => slot(position)
                .map(|slot| {
                    edit(Action::PresetEdit(PresetEdit::Stomp {
                        slot,
                        assignment: tile.map(|tile| (u32::from(tile), StompRole::Primary)),
                    }))
                })
                .unwrap_or_default(),
            C::SetMomentary { tile, momentary } => {
                edit(Action::PresetEdit(PresetEdit::Momentary {
                    stomp: u32::from(tile),
                    momentary,
                }))
            }
            C::SetSceneMode {
                position,
                parameter,
                enabled,
            } => slot(position)
                .map(|slot| {
                    edit(Action::PresetEdit(PresetEdit::SceneMode {
                        site: Site::Slot(slot),
                        index: u32::from(parameter),
                        enabled,
                    }))
                })
                .unwrap_or_default(),
            // On a capture block the cycle order goes first: only the UI knows the Directory
            // sort (`0x4f50dc`).
            C::CycleModel { position, step } => slot(position)
                .map(|slot| {
                    let cycle = (capture_block(state, position))
                        .then(|| Request::Action(Action::CaptureCycle(self.capture_cycle(state))));
                    let next = Request::Action(Action::CycleModel { slot, step });
                    cycle.into_iter().chain([next]).collect()
                })
                .unwrap_or_default(),
            C::BeginSceneCopy { source, swap } => match EngineScene::new(source.get()) {
                Ok(source) => edit(Action::SceneCopyMode(Some(SceneClipboard {
                    source: Some(source),
                    swap,
                }))),
                Err(error) => vec![notice(Subsystem::Ui, error.to_string())],
            },
            C::EndSceneCopy => edit(Action::SceneCopyMode(None)),
            C::LinkMedia {
                folder,
                ids,
                linked,
            } => {
                // `Action::LinkMedia` refreshes the Directory media after the write: no local
                // mirror, so a refused or failed write shows nothing (review r15 R23).
                self.library_stale = true;
                edit(Action::LinkMedia {
                    folder,
                    products: ids,
                    linked,
                })
            }
            C::DuplicateMediaFolder { folder } => {
                self.library_stale = true;
                edit(Action::DuplicateMediaFolder { folder })
            }
            C::SetSavingDestination { folder } => edit(Action::SetSavingDestination { folder }),
            C::PinModel { model, pinned } => {
                self.picker.pin(model, pinned);
                edit(Action::PinModel { model, pinned })
            }
            C::EnterParameter {
                target,
                parameter,
                text,
            } => {
                let site = match target {
                    tetrad_ui::ParameterTarget::Block(position) => slot(position).map(Site::Slot),
                    tetrad_ui::ParameterTarget::Site(site) => Some(project::engine_site(site)),
                };
                site.map(|site| enter(state, site, parameter, &text))
                    .unwrap_or_default()
            }
            C::SetDeviceName { name } => edit(Action::SetDeviceName(
                Some(name.trim().to_owned()).filter(|name| !name.is_empty()),
            )),
            C::ShowLicence { package } => {
                self.licence = None;
                self.licence_request = package;
                Vec::new()
            }
            C::RestoreQuarantinedCapture { id, downloads } => {
                self.quarantine_stale = true;
                edit(Action::RestoreQuarantinedCapture {
                    store: match downloads {
                        true => tetrad_storage::stock::CaptureStore::Downloads,
                        false => tetrad_storage::stock::CaptureStore::User,
                    },
                    id,
                })
            }
            command @ C::Device { action } => device_action(action, command),
            command @ C::ExpressionCalibration { .. } => {
                vec![Request::Handoff(Handoff::Input(command))]
            }
            C::DevicePreset { position, request } => {
                let requests = self.picker.device_preset(position, request, state);
                self.show_conflict();
                requests
            }
            C::Expression { position, request } => {
                self.picker
                    .expression(position, request, self.view.snapshot())
            }
            C::SettingsPreset { page, request } => {
                let requests = self.picker.settings_preset(page, request, state);
                self.show_conflict();
                requests
            }
            // tetrad-ui reports its own tuner transition; `Action::ScreenShown` carries it.
            C::ShowTuner { .. } => Vec::new(),
            // Keyboard/automation activation: one tap, both edges.
            C::Looper { action } => [true, false]
                .map(|pressed| {
                    Request::Action(Action::LooperTile {
                        button: looper_button(action),
                        pressed,
                    })
                })
                .into(),
            C::LooperView { shown } => edit(Action::LooperView(shown)),
            C::LooperAssign { tile } => edit(Action::LooperAssign(tile)),
            C::ResetLooperActions => edit(Action::ResetLooperActions),
            C::DspErrorDialog { reboot } => edit(Action::DspErrorDialog(if reboot {
                super::actions::DspErrorChoice::Reboot
            } else {
                super::actions::DspErrorChoice::Cancel
            })),
            C::TapTempo => self.tap(now),
            C::NudgeTempo { steps } => {
                let bpm = super::actions::tempo_bpm(state) as f32;
                edit(Action::SetTempo(bpm + steps as f32))
            }
            command @ (C::Capture { .. } | C::SaveCapture { .. }) => {
                vec![Request::Handoff(Handoff::Capture(command))]
            }
            C::Nc2 { request } => {
                use tetrad_capture::v2::{Nc2Input, Nc2State};
                let input = match request {
                    Nc2Request::Close => Nc2Input::Close,
                    Nc2Request::Save => Nc2Input::Save,
                    Nc2Request::Restart => Nc2Input::Discard,
                    // `0x3d7424`: index 0 (CAPTURE) enters state 5, index 1 state 4.
                    Nc2Request::Audition(source) => Nc2Input::SetState(match source {
                        tetrad_ui::AuditionSource::Capture => Nc2State::AuditionCapture as i32,
                        tetrad_ui::AuditionSource::Reference => Nc2State::AuditionDut as i32,
                    }),
                };
                vec![Request::Handoff(Handoff::Nc2(input))]
            }
            command @ (C::WifiScan
            | C::WifiConnect { .. }
            | C::WifiForget { .. }
            | C::WifiDisconnect
            | C::Updater { .. }
            | C::CreateBackup { .. }
            | C::RenameBackup { .. }
            | C::RestoreBackup { .. }
            | C::UpdateBackup { .. }
            | C::DeleteBackup { .. }
            | C::CloudLogin { .. }
            | C::DownloadCapture { .. }
            | C::BrowseCloud { .. }
            | C::DownloadProduct { .. }
            | C::DownloadProducts { .. }
            | C::UploadOverwrite { .. }
            | C::CaptureOverwrite { .. }
            | C::CaptureUploadOverwrite { .. }
            | C::CancelUpload) => {
                vec![Request::Handoff(Handoff::Services(command))]
            }
        }
    }

    /// `UiCommand::CopyPresets`: COPY or MOVE of the checked slots into a setlist, to its
    /// first empty slots or as one PASTE.
    fn copy_presets(
        &mut self,
        ids: Vec<u64>,
        from: String,
        setlist: u64,
        remove: bool,
        target: PasteTarget,
    ) -> Vec<Request> {
        // `from` is a name (review r23 K11): a setlist deleted since COPY fails in the
        // copy, it does not fall back to another setlist.
        let to = (self.library.as_ref())
            .and_then(|library| library.setlists.get(setlist as usize))
            .map(|to| to.name.clone());
        let Some(to) = to else {
            return vec![notice(Subsystem::Library, "no setlist is open".into())];
        };
        self.library_stale = true;
        let slots = match target {
            PasteTarget::FirstEmpty => {
                return (ids.into_iter())
                    .map(|id| {
                        Request::Action(Action::CopyPreset {
                            from: from.clone(),
                            slot: id as usize,
                            to: to.clone(),
                            remove,
                        })
                    })
                    .collect();
            }
            PasteTarget::Slots(slots) => slots,
        };
        let (pairs, cyclic) = paste_order(ids.into_iter().zip(slots).collect(), from == to);
        // One action: a refused pair stops the whole paste (review r29 B1).
        let paste = Request::Action(Action::PastePresets {
            from,
            to,
            pairs: (pairs.into_iter())
                .map(|(slot, to_slot)| (slot as usize, to_slot as usize))
                .collect(),
            remove,
        });
        let refused = (cyclic > 0).then(|| {
            notice(
                Subsystem::Library,
                format!("{cyclic} pasted presets would overwrite each other: not pasted"),
            )
        });
        std::iter::once(paste).chain(refused).collect()
    }

    /// Grid ⋮ Delete Preset deletes the loaded preset where it is stored, not a slot
    /// of the setlist the Directory shows (O: a path held by two slots is refused).
    /// Review r35 G1: the load generation must be the one the confirm opened on, so a
    /// load of the same slot number in another setlist deletes nothing.
    fn delete_loaded_preset(&mut self, state: &State, id: u64, load: u64) -> Vec<Request> {
        let edit = |action| vec![Request::Action(action)];
        let now = u64::from(state.performance.footswitch.current_preset);
        let path = (state.current.as_ref()).and_then(|current| current.path.clone());
        let presets = state.library.as_ref().map(|library| &library.presets);
        // Review r35 G10: every slot that names the path, in every setlist.
        let holders = |presets: &tetrad_library::stock::StockLibrary, path: &str| {
            let mut found = Vec::new();
            for setlist in presets.setlists().ok()? {
                let slots = presets.slots(&setlist.name).ok()?;
                found.extend(
                    (slots.iter().enumerate())
                        .filter(|(_, slot)| slot.as_deref() == Some(path))
                        .map(|(slot, _)| (setlist.name.clone(), slot)),
                );
            }
            Some(found)
        };
        match (path, presets) {
            _ if now != id || load != self.load => vec![notice(
                Subsystem::Library,
                "the loaded preset changed: not deleted".into(),
            )],
            (Some(path), Some(presets)) => match holders(presets, &path).as_deref() {
                Some([(setlist, slot)]) => {
                    self.library_stale = true;
                    edit(Action::DeletePreset {
                        setlist: setlist.clone(),
                        slot: *slot,
                    })
                }
                _ => vec![notice(
                    Subsystem::Library,
                    format!("{path}: not in exactly one setlist slot; not deleted"),
                )],
            },
            _ => vec![notice(
                Subsystem::Library,
                "the loaded preset is not stored: nothing to delete".into(),
            )],
        }
    }

    /// `startAddNeuralCaptureMode` `0x598bac` / `startAddIRLoaderMode` `0x598b10`: the block
    /// of the media, then the fit of every cell (`0x597ec8`). A block that cannot be built
    /// starts no mode and names why.
    fn start_add_to_grid(&mut self, state: &State, media: GridMedia) -> Vec<Request> {
        self.add_to_grid = None;
        match self.grid_block(state, &media) {
            Ok(block) => {
                let Some(current) = &*state.current else {
                    return vec![notice(Subsystem::Library, "Add to Grid: no preset".into())];
                };
                let cells = grid_fits(state, &block);
                self.add_to_grid = Some(AddToGrid {
                    media,
                    block: Box::new(block),
                    cells,
                    layout: Box::new(current.deployment.layout().clone()),
                });
                Vec::new()
            }
            Err(detail) => vec![notice(Subsystem::Library, format!("Add to Grid: {detail}"))],
        }
    }

    /// The block `media` gives. A capture block's record type is kept for the Grid glyph
    /// ([`Self::built_devices`]).
    fn grid_block(
        &mut self,
        state: &State,
        media: &GridMedia,
    ) -> Result<tetrad_engine::Instance, String> {
        let specs = (state.specs.as_ref()).ok_or("the catalog is not loaded")?;
        match media {
            GridMedia::Capture(key) => {
                let database = (state.library.as_ref())
                    .and_then(|library| library.database.as_ref())
                    .ok_or("zenui.db is not open")?;
                let cipher = (self.media_cipher.as_ref()).ok_or("the media cipher is not set")?;
                let factory = (self.picker.factory()).map(tetrad_storage::stock::StockDb::view);
                let (block, record) =
                    preset::capture_block(&database.view(), factory.as_ref(), specs, cipher, key)
                        .map_err(|error| error.to_string())?
                        .ok_or_else(|| {
                            format!("capture {key:?} is not in the library or Downloads")
                        })?;
                if let Some(device) = record.device {
                    self.built_devices.insert(record.file_name.clone(), device);
                    Arc::make_mut(&mut self.capture_devices).insert(record.file_name, device);
                }
                Ok(block)
            }
            GridMedia::Ir(key) => {
                let name = (self.media.iter())
                    .find(|entry| {
                        entry.folder.kind() == tetrad_ui::MediaKind::Ir && entry.value == *key
                    })
                    .map(|entry| entry.name.as_str())
                    .ok_or_else(|| format!("IR {key:?} is not in the library"))?;
                preset::ir_block(specs, key, name).map_err(|error| error.to_string())
            }
        }
    }

    /// Owned on-screen TAP: the interval between two taps sets the tempo.
    fn tap(&mut self, now: Duration) -> Vec<Request> {
        let interval = self
            .last_tap
            .map(|last| now.saturating_sub(last).as_millis());
        self.last_tap = Some(now);
        match interval {
            Some(ms) if TAP_INTERVAL_MS.contains(&ms) => {
                vec![Request::Action(Action::SetTempo(60_000.0 / ms as f32))]
            }
            _ => Vec::new(),
        }
    }

    fn load(&self, id: u64) -> Vec<Request> {
        let setlist = self
            .library
            .as_ref()
            .and_then(LibraryView::browsed)
            .map(|setlist| setlist.path.clone());
        match u32::try_from(id) {
            Ok(index) => vec![Request::Action(Action::LoadPreset {
                target: PresetRef { setlist, index },
                reload: true,
            })],
            Err(_) => Vec::new(),
        }
    }

    /// Rebuilds the snapshot and hands it to tetrad-ui when it differs.
    fn sync(&mut self, state: &State) -> Result<(), UiError> {
        let identity = state.current.as_ref().map(|current| {
            (
                current.path.clone(),
                state.performance.footswitch.current_preset,
            )
        });
        if identity != self.loaded {
            self.loaded = identity;
            self.load += 1;
            self.selected = None;
            self.site = None;
            // O: the cell fits were for the preset that was loaded.
            self.add_to_grid = None;
            self.library_stale = true;
        }
        // Review r32 E5 (O): a layout edit while the overlay is open (Cortex Control) makes
        // the fits stale, so the mode ends; a new tap computes them again.
        if let Some(mode) = &self.add_to_grid
            && (state.current.as_ref()).is_none_or(|c| *c.deployment.layout() != *mode.layout)
        {
            self.add_to_grid = None;
        }
        // Review r14 U-3: a junction or lane control removed by a layout edit closes its editor.
        // The pair keeps its splitter and mixer instances without a split or mix point, so
        // those editors close with the point (review r15 R18).
        if let Some(site) = self.site
            && state.current.as_ref().is_none_or(|current| {
                let layout = current.deployment.layout();
                let point = match site {
                    tetrad_ui::EditorSite::Splitter(pair) => layout.pair(engine_pair(pair)).split,
                    tetrad_ui::EditorSite::Mixer(pair) => layout.pair(engine_pair(pair)).merge,
                    _ => Some(Column::new(0).expect("column 0")),
                };
                point.is_none() || layout.instance(project::engine_site(site)).is_none()
            })
        {
            self.site = None;
        }
        let library = &state.settings.settings().library;
        if (self.destinations.0 != library.download_destination_ir)
            || (self.destinations.1 != library.download_destination_nc)
        {
            self.destinations = (
                library.download_destination_ir.clone(),
                library.download_destination_nc.clone(),
            );
            self.library_stale = true;
        }
        let mut result = Ok(());
        if self.library_stale {
            self.library_stale = false;
            result = self.refresh_library(state);
        }
        if let Some(specs) = &state.specs
            && self
                .models
                .as_ref()
                .is_none_or(|(count, _)| *count != specs.models.len())
        {
            self.models = Some((specs.models.len(), project::models(specs).into()));
        }
        let picker_open = self.view.screen() == Screen::ModelPicker;
        let settings_page = match self.view.screen() {
            Screen::GlobalEq(_) => Some(tetrad_ui::SettingsPresetPage::GlobalEq),
            Screen::Io(_) => Some(tetrad_ui::SettingsPresetPage::Io),
            _ => None,
        };
        if let Err(error) = (self.picker).refresh(state, picker_open || settings_page.is_some()) {
            result = Err(UiError::LibraryView(format!("zenui.db: {error}")));
        }
        let unread = (self.picker).refresh_captures(state, self.media_cipher.as_ref(), &self.media);
        if !unread.is_empty() {
            result = Err(UiError::LibraryView(unread.join("; ")));
        }
        // "Unsaved" is the PresetDirty flag (`0x5ad250`): the live preset differs from the one
        // loaded or last saved. Every stored edit counts, expression links included.
        let dirty = state.preset_dirty();
        self.latch_notice(state);
        // M406: a deploy that changes which global modules it switched off for CPU load emits
        // `globalEqDisabled(eq, gate)` (`0x2fbb90`, only on a change) → `globalEqGateDisabledSlot`
        // (`0x5b030c`): the header status flashes for 10 s and the popup opens only on a tap.
        let inhibited = (state.current.as_ref())
            .map(|current| current.deployment.plan().inhibited)
            .unwrap_or_default();
        if inhibited != self.inhibited {
            self.inhibited = inhibited;
            self.view
                .set_global_eq_inhibited(inhibited.global_eq, inhibited.input_gate);
        }
        // The open editor's block (stock watches only it, `0x4b4764`).
        let editor_preset =
            (self.selected).and_then(|position| self.picker.block_match(state, position));
        let local = Local {
            library: self.library.as_ref(),
            models: self
                .models
                .as_ref()
                .map_or_else(Default::default, |(_, models)| Arc::clone(models)),
            media: Arc::clone(&self.media),
            capture_library: self.picker.captures.library(),
            capture_devices: Arc::clone(&self.capture_devices),
            selected: self.selected,
            clipboard: self.clipboard.is_some(),
            dirty,
            device_presets: self.picker.listed(),
            device_preset: self.picker.selected_preset(self.selected, editor_preset),
            pinned_models: self.picker.pinned(),
            notice: self.pending_notice.clone(),
            site: self.site,
        };
        let snapshot = project::snapshot(state, &local);
        let mut snapshot = views::apply(&state.views, snapshot);
        snapshot.storage = self.storage();
        snapshot.editor_preset = editor_preset;
        // Only the shown page: the match reads each preset's rows.
        snapshot.settings_preset_match = match settings_page {
            Some(page @ tetrad_ui::SettingsPresetPage::GlobalEq) => {
                tetrad_ui::SettingsPresetMatch {
                    global_eq: self.picker.settings_match(state, page),
                    io: None,
                }
            }
            Some(page) => tetrad_ui::SettingsPresetMatch {
                global_eq: None,
                io: self.picker.settings_match(state, page),
            },
            None => tetrad_ui::SettingsPresetMatch::default(),
        };
        snapshot.licence = self.licence.clone();
        snapshot.add_to_grid = self.add_to_grid.as_ref().map(|mode| mode.cells);
        snapshot.preset_load = self.load;
        if snapshot != *self.view.snapshot() {
            self.view.set_snapshot(snapshot)?;
        }
        if !self.warmed && state.current.is_some() {
            self.warmed = true;
            let view = self.view.clone();
            // Lowest priority: the QC has one core, and the boot and the ring drain go first.
            let warm = move || {
                // A failed priority change only makes the warm-up compete with the boot.
                _ = tetrad_sys::thread::lowest_priority();
                view.warm_caches();
            };
            if let Err(error) = std::thread::Builder::new()
                .name("ui-warm".into())
                .spawn(warm)
            {
                result = Err(UiError::LibraryView(format!("UI cache warm-up: {error}")));
            }
        }
        result
    }

    /// Device Storage counts (manual §10). The stock limits (`StorageInfoController`,
    /// page `0x65a8bc`) are not recovered: each maximum stays 0 (unknown).
    fn storage(&self) -> tetrad_ui::StorageView {
        let count = |folder| tetrad_ui::StorageUse {
            current: self
                .media
                .iter()
                .filter(|entry| entry.folder == folder)
                .count() as u32,
            maximum: 0,
        };
        tetrad_ui::StorageView {
            presets: tetrad_ui::StorageUse {
                current: self.library.as_ref().map_or(0, |library| library.presets),
                maximum: 0,
            },
            captures: count(tetrad_ui::MediaFolder::MyCaptures),
            impulse_responses: count(tetrad_ui::MediaFolder::MyIrs),
        }
    }

    /// Latches the newest notice when it arrived since the last sync and answers the last
    /// user input ([`NOTICE_WINDOW`]). A repeat of the same text counts as a new arrival at
    /// `state.now`; the id is the arrival sequence, so tetrad-ui shows each arrival once.
    fn latch_notice(&mut self, state: &State) {
        let notices = &state.notices;
        let seq = (
            notices.dropped + notices.iter().count() as u64,
            notices.repeats,
        );
        if seq == self.notice_seq {
            return;
        }
        let repeat = seq.0 == self.notice_seq.0;
        self.notice_seq = seq;
        let Some(notice) = notices.iter().last() else {
            return;
        };
        let at = if repeat { state.now } else { notice.at };
        let since = |start: Duration| at.checked_sub(start);
        let answers = match notice.subsystem {
            Subsystem::Ui | Subsystem::Preset | Subsystem::Library | Subsystem::Settings => {
                (self.last_input.and_then(since)).is_some_and(|after| after <= NOTICE_WINDOW)
            }
            subsystem => (self.user_job)
                .take_if(|(job, start)| {
                    since(*start).is_some_and(|after| job.answered_by(subsystem, after))
                })
                .is_some(),
        };
        if answers {
            self.pending_notice = Some(tetrad_ui::NoticeView {
                id: seq.0 << 16 | seq.1.min(0xffff),
                title: notice_title(notice.subsystem).into(),
                body: notice.detail.clone(),
                button: "OK",
            });
        }
    }

    /// Preset Conflict (`0x397ff8(model, 0)`) after a refused device save, in the stock info
    /// popup (`0x54617c`, GOT IT); `%1` from `0x1f5b58`.
    fn show_conflict(&mut self) {
        let Some(model) = self.picker.take_conflict() else {
            return;
        };
        let kind = match model {
            4004 => "Global EQ Preset",
            31000 => "I/O Settings Preset",
            _ => "Virtual Device Preset",
        };
        self.dialogs += 1;
        self.pending_notice = Some(tetrad_ui::NoticeView {
            id: u64::MAX - self.dialogs,
            title: "Preset Conflict".into(),
            body: format!(
                "The {kind} could not be saved because its parameters are already stored in an existing Preset."
            ),
            button: "GOT IT",
        });
    }

    /// Setlists, then the slots of the setlist that holds the loaded preset (directory reads).
    fn refresh_library(&mut self, state: &State) -> Result<(), UiError> {
        let library = state
            .library
            .as_ref()
            .ok_or(UiError::Unavailable("the preset library"))?;
        let mut setlists = library.presets.setlists()?;
        // Stock lists the read-only Factory Library first ("Factory Presets", number 0); a
        // load names its path, which `slot_path` reads through `factory_slots`. Seen missing
        // on the QC (2026-10-15).
        if library.presets.factory_slots().is_ok() {
            setlists.insert(
                0,
                tetrad_library::stock::Setlist {
                    name: FACTORY_PRESETS.into(),
                    path: super::FACTORY_SETLIST.into(),
                },
            );
        }
        let last = &state.settings.settings().last_preset.setlist;
        let browse = last
            .as_deref()
            .and_then(|key| setlists.iter().position(|s| s.path == key || s.name == key))
            .unwrap_or(0);
        let (mut directory, mut unread) = super::media::directory_view(
            &library.presets,
            library.database.as_ref(),
            &self.media,
            &self.directory_media,
        )
        .map_err(|error| UiError::LibraryView(error.to_string()))?;
        if let Some(db) = library.database.as_ref() {
            directory.saving_destinations =
                project::saving_destinations(db, state.settings.settings());
        }
        // A list that cannot be read is left out; the rest of the view still updates (N5).
        let favorites = library
            .presets
            .items(ItemList::Favorites(Collection::Presets))
            .unwrap_or_else(|error| {
                unread.push(format!("favorites not read: {error}"));
                Vec::new()
            })
            .into_iter()
            .map(|entry| entry.item)
            .collect();
        // Every filled slot for Device Storage and the Directory search; a setlist that
        // cannot be read counts as empty (its own slots report the error).
        directory.presets = (0u64..)
            .zip(&setlists)
            .filter_map(|(id, setlist)| {
                let slots = library.presets.directory_slots(&setlist.name).ok()?;
                Some(
                    slots
                        .into_iter()
                        .filter(|slot| slot.path.is_some())
                        .map(move |slot| tetrad_ui::DirectoryPreset {
                            setlist: id,
                            slot: slot.slot as u64,
                            title: slot.name,
                        }),
                )
            })
            .flatten()
            .collect();
        let presets = directory.presets.len() as u32;
        // Clamped once here, so `set_snapshot` finds it clamped and keeps the shared copy.
        directory.clamp();
        self.library = Some(LibraryView {
            directory: Arc::new(directory),
            titles: Default::default(),
            setlists,
            browse,
            slots: Vec::new(),
            favorites,
            presets,
        });
        if let Some(Request::Effect(Effect::Notice(_, detail))) = self.refresh_slots(state).pop() {
            unread.push(detail);
        }
        if unread.is_empty() {
            Ok(())
        } else {
            Err(UiError::LibraryView(unread.join("; ")))
        }
    }

    fn refresh_slots(&mut self, state: &State) -> Vec<Request> {
        let (Some(view), Some(library)) = (self.library.as_mut(), state.library.as_ref()) else {
            return Vec::new();
        };
        let Some(setlist) = view.setlists.get(view.browse) else {
            view.slots.clear();
            return Vec::new();
        };
        if setlist.path == super::FACTORY_SETLIST {
            return match library.presets.factory_slots() {
                Ok(slots) => {
                    view.titles = (slots.iter().enumerate())
                        .filter_map(|(slot, path)| {
                            Some((slot as u64, project::preset_title(path.as_ref()?)))
                        })
                        .collect();
                    Arc::make_mut(&mut view.directory).slots = (slots.iter().enumerate())
                        .map(|(slot, path)| tetrad_ui::DirectorySlot {
                            id: slot as u64,
                            key: path
                                .as_deref()
                                .map_or(0, |path| preset_key(library.presets.media(), path)),
                            empty: path.is_none(),
                            title: path
                                .as_deref()
                                .map(project::preset_title)
                                .unwrap_or_default(),
                            author: String::new(),
                            date: String::new(),
                        })
                        .collect();
                    view.slots = slots;
                    Vec::new()
                }
                Err(error) => {
                    view.slots.clear();
                    vec![notice(Subsystem::Library, error.to_string())]
                }
            };
        }
        match library.presets.slots(&setlist.name) {
            Ok(slots) => {
                match library.presets.directory_slots(&setlist.name) {
                    Ok(summaries) => {
                        view.titles = summaries
                            .iter()
                            .map(|slot| (slot.slot as u64, slot.name.clone()))
                            .collect();
                        let media = library.presets.media();
                        Arc::make_mut(&mut view.directory).slots = summaries
                            .into_iter()
                            .map(|slot| tetrad_ui::DirectorySlot {
                                id: slot.slot as u64,
                                key: (slot.path.as_deref())
                                    .map_or(0, |path| preset_key(media, path)),
                                empty: slot.path.is_none(),
                                title: slot.name,
                                author: slot.author,
                                date: slot.date,
                            })
                            .collect();
                    }
                    Err(error) => return vec![notice(Subsystem::Library, error.to_string())],
                }
                view.slots = slots;
                Vec::new()
            }
            Err(error) => {
                view.slots.clear();
                vec![notice(Subsystem::Library, error.to_string())]
            }
        }
    }
}

/// The title stock gives the Factory Library setlist (tetrad-ui guards it by this title).
const FACTORY_PRESETS: &str = "Factory Presets";

/// Review r35 G9 (O): the identity of a slot's preset file for [`tetrad_ui::DirectorySlot::key`].
/// A write replaces the file (staged, then renamed), so its inode and modification time
/// change even when the path stays (a same-name OVERWRITE); a delete and a paste make a new
/// file. A file that cannot be read keys by its path alone.
fn preset_key(media: &tetrad_storage::media::MediaRoot, path: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    use std::os::unix::fs::MetadataExt;
    let mut hasher = std::hash::DefaultHasher::new();
    path.hash(&mut hasher);
    if let Ok(file) = media.resolve(path)
        && let Ok(meta) = std::fs::metadata(file)
    {
        (
            meta.dev(),
            meta.ino(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.len(),
        )
            .hash(&mut hasher);
    }
    // 0 is the empty slot.
    hasher.finish().max(1)
}

/// Stock "Setlists full" body (`0x11e1b2c`).
const SETLISTS_FULL: &str = "All local setlists are full. Please create a new setlist or remove \
presets to be able to save more.";

/// The stock undo push of a UI edit (`0x648f1c` callers). Only the value slot `0x59b3ac`
/// (`0x523644`, a QVariant, so the IR/capture text pick too) uses the 500 ms Grid view
/// timer. Direct: bypass `0x5ad868`; add `0x59ec78`; replace, paste and reset `0x59ee7c`
/// (`param_3 != 0`, one model setter; which command takes it is inferred); move `0x59f620`, drag `0x5c8354`; remove `0x59b95c`, `0x5acfb8`; row
/// ports `0x59b714`/`0x59ccc4` (`0x520ac8`); split and mix `0x59d2e8`/`0x59d4b4`
/// (`0x519f0c`); scene texts `0x6795c0`/`0x679664`; Preset MIDI Out `0x32def4`/`0x32df64`.
/// Stomp toggles, labels, assignments, Latching/Momentary and scene assignment are inferred.
/// Scene copy and swap push nothing (`0x20eec4`).
fn undo_path(command: &ViewCommand) -> Option<UndoPath> {
    use ViewCommand as C;
    match command {
        C::NudgeParameter { .. }
        | C::SetPosition { .. }
        | C::NudgeSite { .. }
        | C::EnterParameter { .. }
        | C::SelectText { .. } => Some(UndoPath::View),
        C::SetBlockBypass { .. }
        | C::ToggleStomp { .. }
        | C::AddBlock { .. }
        | C::ReplaceBlock { .. }
        | C::CycleModel { .. }
        | C::MoveBlock { .. }
        | C::RemoveBlock { .. }
        | C::PasteBlock { .. }
        | C::ResetBlockDefaults { .. }
        | C::SetRowInput { .. }
        | C::SetRowOutput { .. }
        | C::SetSplit { .. }
        | C::SetMix { .. }
        | C::QuickSplit { .. }
        | C::SetSceneLabel { .. }
        | C::SetSceneColor { .. }
        | C::RenameStomp { .. }
        | C::AssignStomp { .. }
        | C::SetMomentary { .. }
        | C::SetSceneMode { .. }
        | C::SetPresetMidi { .. }
        | C::AddToGrid { .. }
        | C::AddCapture { .. } => Some(UndoPath::Direct),
        _ => None,
    }
}

/// The block at `position` is a Neural Capture (category 14): BANK UP walks the captures.
/// `setModelFitInfoSlot` `0x5947c4` for every cell: the block fits when the layout with it in
/// the cell deploys (I: Tetrad compiles the trial at once, stock asks the DSP per cell,
/// `0x5148f8`). Fits on a block → [`CellFit::Replace`], fits on an empty cell →
/// [`CellFit::Add`], else [`CellFit::NoFit`].
fn grid_fits(
    state: &State,
    block: &tetrad_engine::Instance,
) -> [[CellFit; tetrad_ui::GRID_COLUMNS]; tetrad_ui::GRID_ROWS] {
    let fit = |row: usize, column: usize| {
        let (Some(current), Some(specs)) = (&*state.current, &state.specs) else {
            return CellFit::NoFit;
        };
        let Some(slot) = GridPosition::new(row as u8, column as u8)
            .ok()
            .and_then(project::slot_of)
        else {
            return CellFit::NoFit;
        };
        let site = Site::Slot(slot);
        let occupied = current.deployment.layout().instance(site).is_some();
        let fits = (current.deployment.clone())
            .edit(specs, |layout| {
                layout.set_instance(site, Some(block.clone()));
                Ok(())
            })
            .is_ok();
        match (fits, occupied) {
            (false, _) => CellFit::NoFit,
            (true, true) => CellFit::Replace,
            (true, false) => CellFit::Add,
        }
    };
    std::array::from_fn(|row| std::array::from_fn(|column| fit(row, column)))
}

fn capture_block(state: &State, position: GridPosition) -> bool {
    let model = state
        .current
        .as_ref()
        .and_then(|current| project::instance(current.deployment.layout(), position))
        .map(|block| block.model.0);
    model
        .zip(state.specs.as_ref())
        .and_then(|(model, specs)| specs.model(model))
        .is_some_and(|spec| spec.category_id == 14)
}

/// A BANK UP turn on the block at `position` gives another model.
fn changes_model(state: &State, position: GridPosition, step: i32) -> bool {
    let model = state
        .current
        .as_ref()
        .and_then(|current| project::instance(current.deployment.layout(), position))
        .map(|block| block.model.0);
    let next = model
        .zip(state.specs.as_ref())
        .and_then(|(model, specs)| super::preset::cycled_model(specs, model, step));
    next.is_some() && next != model
}

fn scene_copy(
    first: tetrad_ui::SceneIndex,
    second: tetrad_ui::SceneIndex,
    swap: bool,
) -> Vec<Request> {
    match (
        EngineScene::new(first.get()),
        EngineScene::new(second.get()),
    ) {
        (Ok(first), Ok(second)) => vec![Request::Action(Action::CopyScene {
            first,
            second,
            swap,
        })],
        _ => Vec::new(),
    }
}

fn notice_title(subsystem: Subsystem) -> &'static str {
    match subsystem {
        Subsystem::Platform => "Device",
        Subsystem::Splash => "Start-up",
        Subsystem::Settings => "Settings",
        Subsystem::Library => "Library",
        Subsystem::Dsp => "Audio engine",
        Subsystem::Preset => "Preset",
        Subsystem::Ui => "Not available",
        Subsystem::Control => "Cortex Control",
        Subsystem::Input => "Footswitches",
        Subsystem::Midi => "MIDI",
        Subsystem::Services => "Network",
        Subsystem::Capture => "Neural Capture",
        Subsystem::Power => "Power",
        Subsystem::Extensions => "Extensions",
    }
}

fn notice(subsystem: Subsystem, detail: String) -> Request {
    Request::Effect(Effect::Notice(subsystem, detail))
}

/// The footswitch bank overlay at `bank` (no armed slot), or closed.
fn overlay(bank: Option<u32>) -> Vec<Request> {
    let overlay = bank.map(|bank| BankOverlay {
        bank,
        highlighted: None,
    });
    vec![Request::Action(Action::BankOverlay(overlay))]
}

fn engine_pair(pair: tetrad_ui::RowPair) -> Pair {
    match pair {
        tetrad_ui::RowPair::Upper => Pair::Upper,
        tetrad_ui::RowPair::Lower => Pair::Lower,
    }
}

/// The copy order of slot-addressed PASTE pairs (source slot, target slot). Tetrad copies from
/// the files, while stock pastes the presets it collected (`0x786d38`), so in one setlist a
/// pair runs before any pair that overwrites its source, and a slot onto itself is skipped.
/// Pairs that overwrite each other in a ring (a manual swap) are left out and counted (O).
fn paste_order(mut pending: Vec<(u64, u64)>, same_setlist: bool) -> (Vec<(u64, u64)>, usize) {
    if !same_setlist {
        return (pending, 0);
    }
    pending.retain(|(from, to)| from != to);
    let mut ordered = Vec::with_capacity(pending.len());
    while let Some(next) =
        (pending.iter()).position(|(_, to)| !pending.iter().any(|(from, _)| from == to))
    {
        ordered.push(pending.remove(next));
    }
    (ordered, pending.len())
}

/// The stored modes a tetrad-ui mode chip stands for (a hybrid chip is the shown hybrid).
fn stored_modes(mode: FootswitchMode, rows: tetrad_ui::HybridRows) -> Vec<StoredMode> {
    match mode {
        FootswitchMode::Preset => vec![StoredMode::Preset],
        FootswitchMode::Scene => vec![StoredMode::Scene],
        FootswitchMode::Stomp => vec![StoredMode::Stomp],
        FootswitchMode::Hybrid => hybrid_mode(rows).into_iter().collect(),
    }
}

/// Upper row first (row order inferred, see `project::view_mode`).
fn hybrid_mode(rows: tetrad_ui::HybridRows) -> Option<StoredMode> {
    use FootswitchMode::{Preset, Scene, Stomp};
    Some(match (rows.upper, rows.lower) {
        (Preset, Scene) => StoredMode::PresetScene,
        (Preset, Stomp) => StoredMode::PresetStomp,
        (Scene, Preset) => StoredMode::ScenePreset,
        (Scene, Stomp) => StoredMode::SceneStomp,
        (Stomp, Preset) => StoredMode::StompPreset,
        (Stomp, Scene) => StoredMode::StompScene,
        _ => return None,
    })
}

/// A picked IR or capture: the live string edit (`Block::set_text`) at the block's site.
fn set_text(position: GridPosition, index: u32, text: String) -> Option<Request> {
    project::slot_of(position).map(|slot| {
        Request::Action(Action::SetText {
            site: Site::Slot(slot),
            index,
            text,
        })
    })
}

/// Encoder steps through tetrad-core (`ParameterScale::encoder`, `0x5cca94`); toggles flip.
fn nudge(state: &State, site: Site, parameter: u16, steps: i32) -> Vec<Request> {
    let (Some(current), Some(specs)) = (&*state.current, &state.specs) else {
        return vec![notice(
            Subsystem::Ui,
            "parameter edit needs a loaded preset and the catalog".into(),
        )];
    };
    let Some(block) = current.deployment.layout().instance(site) else {
        return Vec::new();
    };
    let index = u32::from(parameter);
    let (Some(spec), Some(value)) = (
        specs
            .model(block.model.0)
            .and_then(|model| model.parameter(index)),
        block.params.get(usize::from(parameter)),
    ) else {
        return Vec::new();
    };
    // String parameters change only through the media picker (`SelectText`).
    if spec.scale.kind == ParameterType::String {
        return Vec::new();
    }
    let normalized = value.at(current.deployment.scene());
    let value = match project::parameter_kind(&spec.scale, normalized) {
        Some(tetrad_ui::ParameterKind::Toggle { on }) => f32::from(u8::from(!on)),
        _ => spec.scale.encoder(normalized, steps),
    };
    // A Pattern Tremolo step writes its connected run (`0x3afc94`).
    std::iter::once(index)
        .chain(super::preset::step_run(current, site, index))
        .map(|index| Request::Action(Action::SetParameter { site, index, value }))
        .collect()
}

/// A graph drag (`controlPointMovedToPosSlot` of the synth and voicer editors): the value
/// snaps to a step (`0x5cc994`, `0x5cc9e0`) and is written only when it changes.
fn set_position(state: &State, site: Site, parameter: u16, value: u16) -> Vec<Request> {
    let value = f32::from(value) / f32::from(tetrad_ui::POSITION_SCALE);
    let (Some(current), Some(specs)) = (&*state.current, &state.specs) else {
        return Vec::new();
    };
    let Some(block) = current.deployment.layout().instance(site) else {
        return Vec::new();
    };
    let index = u32::from(parameter);
    let (Some(spec), Some(stored)) = (
        specs
            .model(block.model.0)
            .and_then(|model| model.parameter(index)),
        block.params.get(usize::from(parameter)),
    ) else {
        return Vec::new();
    };
    let scale = &spec.scale;
    if scale.disabled || scale.kind == ParameterType::String {
        return Vec::new();
    }
    // A parameter without a `steps` attribute has step size 0 and takes the value as is.
    let snap = |normalized: f32| {
        let normalized = normalized.clamp(0.0, 1.0);
        if scale.step_size() <= 0.0 {
            return Some(normalized);
        }
        let last = i64::from(scale.steps().max(1) - 1);
        let step = scale.step_index(normalized).ok()?;
        scale.step_value(i64::from(step).clamp(0, last)).ok()
    };
    let (Some(old), Some(new)) = (snap(stored.at(current.deployment.scene())), snap(value)) else {
        return Vec::new();
    };
    if old == new {
        return Vec::new();
    }
    vec![Request::Action(Action::SetParameter {
        site,
        index,
        value: new,
    })]
}

/// Numeric keyboard entry (manual "PARAMETER EDITOR LAYOUT": tap the numeric display): the
/// typed number in display units (`0x5cd1fc`) as one parameter write, the same signal as a
/// knob drag (`0x4c9ce8`). The UI accepts only values in range; text that is not a number
/// or out of range does nothing.
fn enter(state: &State, site: Site, parameter: u16, text: &str) -> Vec<Request> {
    let (Some(current), Some(specs)) = (&*state.current, &state.specs) else {
        return Vec::new();
    };
    let Some(scale) = current
        .deployment
        .layout()
        .instance(site)
        .and_then(|block| specs.model(block.model.0))
        .and_then(|model| model.parameter(u32::from(parameter)))
        .map(|spec| &spec.scale)
    else {
        return Vec::new();
    };
    let Ok(typed) = text.trim().parse::<f32>() else {
        return Vec::new();
    };
    let in_range = (scale.to_display(0.0).ok())
        .zip(scale.to_display(1.0).ok())
        .is_some_and(|(start, end)| (start.min(end)..=start.max(end)).contains(&typed));
    let Ok(value) = scale
        .to_normalized(typed)
        .map(|value| value.clamp(0.0, 1.0))
    else {
        return Vec::new();
    };
    if !in_range {
        return Vec::new();
    }
    let index = u32::from(parameter);
    std::iter::once(index)
        .chain(super::preset::step_run(current, site, index))
        .map(|index| Request::Action(Action::SetParameter { site, index, value }))
        .collect()
}

/// The stock Looper X button code of a tile (`0x6129fc`: tile → block parameter 3, 4, 5, 6,
/// 0, 1, 7, 2). Stock has one Undo/Redo action.
fn looper_button(action: LooperAction) -> LooperButton {
    match action {
        LooperAction::Duplicate => LooperButton::Duplicate,
        LooperAction::OneShot => LooperButton::OneShot,
        LooperAction::HalfSpeed => LooperButton::HalfSpeed,
        LooperAction::PunchIn => LooperButton::PunchIn,
        LooperAction::Record => LooperButton::RecordOverdub,
        LooperAction::PlayStop => LooperButton::PlayStop,
        LooperAction::Reverse => LooperButton::Reverse,
        LooperAction::UndoRedo => LooperButton::Undo,
    }
}

fn device_action(action: tetrad_ui::DeviceAction, command: ViewCommand) -> Vec<Request> {
    use tetrad_settings::device::ResetKind;
    use tetrad_ui::DeviceAction as A;
    match action {
        A::Reboot => vec![Request::Effect(Effect::Power(PowerAction::Restart(
            RestartReason::PowerMenu,
        )))],
        A::Shutdown => vec![Request::Effect(Effect::Power(PowerAction::PowerOff))],
        A::ResetSettings => vec![Request::Action(Action::Reset(ResetKind::Settings))],
        A::FactoryReset => vec![Request::Action(Action::Reset(ResetKind::Factory))],
        A::CalibrateExpression(_) => vec![Request::Handoff(Handoff::Input(command))],
        A::ResetWifi | A::SendReport | A::CloudLogout => {
            vec![Request::Handoff(Handoff::Services(command))]
        }
        // Recovery opens on the A + H chord during the startup screen (`input.rs`), after
        // the boot steps ran (tetrad-app boots before its event loop), so CANCEL only leaves the
        // screen. CLEAR ALL is confirmed and expanded in tetrad-ui (`SetPresetMidi` per list).
        A::ContinueBoot | A::ClearPresetMidi => Vec::new(),
        // The Cortex Control License UPDATE path (online, signed in; results as notices).
        A::RefreshLicences => vec![Request::Effect(Effect::ControlAccount(
            super::control::AccountRequest::RefreshLicences,
        ))],
    }
}

#[cfg(test)]
mod tests {
    use tetrad_hal::hal::Command;
    use tetrad_hal::platform::{Platform as HalPlatform, Product};
    use tetrad_settings::device::{DeviceSettings, Hardware};
    use tetrad_settings::{SettingsFileId, SettingsStorage};
    use tetrad_ui::{Control, SettingsPage};

    use super::super::actions::reduce;
    use super::super::leds::{Leds, UiLeds};
    use super::*;

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

    /// Census #8: a slot-addressed PASTE in its own setlist never overwrites a source before
    /// it is copied (consecutive 0, 1 → 1, 2 copies 1 → 2 first), skips a slot onto itself,
    /// and leaves out a ring of pairs (a manual swap) instead of copying a copy.
    #[test]
    fn a_paste_into_its_own_setlist_copies_each_source_before_it_is_overwritten() {
        assert_eq!(
            paste_order(vec![(0, 1), (1, 2)], true),
            (vec![(1, 2), (0, 1)], 0)
        );
        assert_eq!(
            paste_order(vec![(2, 1), (3, 2)], true),
            (vec![(2, 1), (3, 2)], 0)
        );
        assert_eq!(paste_order(vec![(4, 4), (5, 9)], true), (vec![(5, 9)], 0));
        assert_eq!(
            paste_order(vec![(0, 1), (1, 0), (2, 5)], true),
            (vec![(2, 5)], 2)
        );
        assert_eq!(
            paste_order(vec![(0, 1), (1, 0)], false),
            (vec![(0, 1), (1, 0)], 0)
        );
    }

    /// Review r27 R1: an NC editor DOWNLOAD opens no notice job, so no Services notice (the
    /// queue `DELETE`, an update check) can end it; only its typed end sets or clears the
    /// editor failure.
    #[test]
    fn a_capture_download_ends_only_on_its_typed_end() {
        let hash = "Lost Capture".to_owned();
        let request = Request::Handoff(Handoff::Services(ViewCommand::DownloadCapture {
            hash: hash.clone(),
        }));
        assert_eq!(UserJob::of(&request), None);
        let mut ui = Ui::new();
        ui.capture_download_end(hash.clone(), false);
        assert_eq!(ui.view().capture_download_failure(), Some(hash.as_str()));
        ui.capture_download_end(hash, true);
        assert_eq!(ui.view().capture_download_failure(), None);
    }

    /// Review r31 D5: CANCEL of an overwrite question (preset upload, capture download or
    /// upload) ends its job with no answer, so it opens no Services job and ends the open one;
    /// else the next background Services notice would open as a MessageDialog.
    #[test]
    fn cancelling_the_upload_question_ends_the_services_job() {
        let (settings, _) = DeviceSettings::load(&NoFiles, Hardware::QuadCortex).unwrap();
        let mut state = State::new(Product::QuadCortex, HalPlatform::QcOriginal, settings);
        let question = tetrad_ui::CaptureQuestion::default();
        state.views.upload_overwrite = Some("Lead".into());
        state.views.capture_overwrite = Some(question.clone());
        state.views.capture_upload_overwrite = Some("Crunch".into());
        let now = Duration::from_secs(1);
        let cancels = [
            ViewCommand::UploadOverwrite { overwrite: false },
            ViewCommand::CaptureOverwrite { overwrite: false },
            ViewCommand::CaptureUploadOverwrite { overwrite: false },
        ];
        for (index, cancel) in cancels.into_iter().enumerate() {
            let mut ui = Ui::new();
            let mut snapshot = ui.view().snapshot().clone();
            snapshot.upload_overwrite = (index == 0).then(|| "Lead".into());
            snapshot.capture_overwrite = (index == 1).then(|| question.clone());
            snapshot.capture_upload_overwrite = (index == 2).then(|| "Crunch".into());
            ui.view.set_snapshot(snapshot).unwrap();
            assert!(ui.view().overlay_open());
            ui.user_job = Some((UserJob::Services, now));
            let requests = ui.handle(UiInput::View(InputEvent::Back), &state, now);
            assert!(requests.contains(&Request::Handoff(Handoff::Services(cancel))));
            assert_eq!(ui.user_job, None);
        }
    }

    /// tetrad-ui reports keyboard and confirm state (`Action::UiLeds`, w4-leds-inputs §11):
    /// the Settings all-off view becomes the black keyboard view, and valid text lights the
    /// confirm LED (switch 10, `LedView::TextEntry`).
    #[test]
    fn opening_the_keyboard_changes_the_led_view() {
        let (settings, _) = DeviceSettings::load(&NoFiles, Hardware::QuadCortex).unwrap();
        let mut state = State::new(Product::QuadCortex, HalPlatform::QcOriginal, settings);
        let mut ui = Ui::new();
        let mut leds = Leds::default();
        let now = Duration::from_secs(1);
        let run = |ui: &mut Ui, state: &mut State, input| {
            let mut reported = Vec::new();
            for request in ui.handle(input, state, now) {
                if let Request::Action(action) = request {
                    if let Action::UiLeds(view) = action {
                        reported.push(view);
                    }
                    reduce(action, state).unwrap();
                }
            }
            reported
        };
        let account = Screen::Settings(SettingsPage::MyAccount);
        assert!(run(&mut ui, &mut state, UiInput::Show(account)).is_empty());
        leds.update(&state);
        let keyboard = UiLeds {
            keyboard: true,
            ..UiLeds::default()
        };
        let login = UiInput::View(InputEvent::Activate(Control::Login));
        assert_eq!(run(&mut ui, &mut state, login), [keyboard]);
        let text = UiInput::View(InputEvent::TextChanged("a@b".into()));
        let valid = UiLeds {
            confirm_valid: true,
            ..keyboard
        };
        assert_eq!(run(&mut ui, &mut state, text), [valid]);
        let confirm_lit = leds.update(&state).into_iter().any(|effect| {
            matches!(effect, Effect::Hal(Command::ElementLed(led)) if led.index == 10 && led.opaque > 0)
        });
        assert!(confirm_lit);
    }
}

//! On-device composition root (`tetrad run`): one [`Device`] that boots the stock SHARC firmware
//! and runs the QC from the wave-1 crates.
//!
//! Shape: platform input becomes an [`Event`]; each event kind has one reducer; reducers
//! return [`Effect`]s; [`Device`] executes effects through the [`Platform`]. The loop is
//! single-threaded and deterministic: sources are drained in a fixed order and time only
//! comes from the platform clock. Subsystem errors never stop the loop; they become
//! [`Notice`]s (nothing is skipped silently).

pub mod actions;
pub mod app;
pub mod appearance;
pub mod bluetooth;
mod bluetooth_link;
mod boot;
pub mod boot_selector;
pub mod bringup;
pub mod capture;
pub mod community;
pub mod control;
pub mod custom;
pub mod dsp;
pub mod extension_jobs;
pub mod extensions;
pub mod features;
pub mod global_eq;
pub mod info;
pub mod input;
pub mod jacks;
pub mod leds;
#[cfg(target_os = "linux")]
pub mod linux;
mod load;
pub mod mappings;
pub mod media;
pub mod meters;
pub mod midi;
pub mod owned;
#[cfg(feature = "dev")]
mod perf;

/// Turns on the `perf` log line (`tetrad run --dev`).
#[cfg(feature = "dev")]
pub fn enable_perf_log() {
    perf::enable();
}
pub mod platform;
mod power;
pub mod preset;
mod probe;
pub mod remote;
mod remote_backup;
mod remote_extensions;
pub mod remote_files;
mod remote_footswitch;
mod remote_library;
mod remote_mappings;
mod remote_themes;
pub mod services;
pub mod support;
pub mod ui;
mod upload;
mod usb;

pub use upload::UploadRefused;

use std::cell::Cell;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::mem::Discriminant;
use std::path::Path;
use std::time::{Duration, Instant};

use tetrad_catalog::Specs;
use tetrad_core::performance::{FootswitchContext, TapTempo};
use tetrad_dsp_host::responses::Event as DspEvent;
use tetrad_dsp_host::session::{Clock, SendPolicy, SessionError};
use tetrad_dsp_host::transport::ControlPacket;
use tetrad_dsp_protocol::CoreId;
use tetrad_dsp_protocol::controls::{ControlRequest, RequestFrame};
use tetrad_dsp_protocol::requests::Request;
use tetrad_dsp_protocol::responses::LooperStatus;
use tetrad_dsp_protocol::responses::MetronomeStatus;
use tetrad_dsp_protocol::tuner_looper::TunerLooperRequest;
use tetrad_engine::{Deployment, Update};
use tetrad_hal::hal::{Command as HalCommand, Event as HalEvent, Hal, Query, Reading};
use tetrad_hal::platform::{DisplayPanel, Platform as HalPlatform, Product};
use tetrad_hal::requests::DisplayCommand;
use tetrad_library::stock::StockLibrary;
use tetrad_media::factory::{DeviceSerial, SERIAL_PATH};
use tetrad_preset::{MigrationReport, Preset};
use tetrad_services::exec::Files;
use tetrad_settings::device::{DeviceSettings, Hardware, ResetKind, SettingsEvent};
use tetrad_storage::stock::StockDb;
use tetrad_watchdog::power::{PowerReport, PowerStep, RestartReason};
use tetrad_watchdog::splash::{SplashCommand, SplashQueue};

pub use actions::Action;
use boot::identify;
use load::{context, resolve_media};
pub use platform::Platform;
use platform::{Input, Marker, ServiceBackends};

/// Stock UI progress (`tetrad_watchdog::splash::UI_PROGRESS`) at the step that sends it here.
/// Values are confirmed; the 40/50 position is inferred (o-lifecycle §4).
mod progress {
    /// ZenUI main (`0x7dd68`).
    pub const MAIN: i32 = 30;
    /// DSP driver and core load (`0x876dec`).
    pub const DSP_START: i32 = 40;
    pub const DSP_READY: i32 = 50;
    /// ZenUI main, after the DSP.
    pub const GLOBALS: i32 = 60;
    pub const PRESET: i32 = 70;
    /// UI build (`0x5af624`).
    pub const UI_BUILD: i32 = 80;
    pub const UI_BUILT: i32 = 90;
    /// UI full screen (`0x545ac0`).
    pub const FULL_SCREEN: i32 = 100;
}

/// `singleShot(1000, killSplashScreen)` after progress 100 (`0x545ac0` → `0x544860`).
pub const SPLASH_DISMISS_DELAY: Duration = Duration::from_secs(1);
/// Owned: after the dismiss, how often [`Timer::SplashGone`] looks for `zensplash`, and the
/// most polls before it repaints anyway.
pub const SPLASH_POLL: Duration = Duration::from_millis(200);
/// Stock keeps a `/media/p4` free-space reading for 10 minutes (`0x210f6c`, 600 000 ms).
pub const DISK_SPACE_HOLD: Duration = Duration::from_secs(600);
pub const SPLASH_POLLS: u8 = 50;
/// Owned: Cortex Control timers tick at the shortest inferred interval (f-control).
pub const CONTROL_TICK: Duration = Duration::from_millis(100);
/// Owned: tetrad-ui clock for long presses and toasts (w2b-ui M3).
pub const UI_TICK: Duration = Duration::from_millis(50);
/// Owned: tetrad-ui clock while a flicked list coasts, 50 Hz like the touch screen reports.
pub const ANIMATION_TICK: Duration = Duration::from_millis(20);
/// Confirmed: `0x50ed30` starts the diagnostics timer at 1000 ms.
pub const DIAGNOSTICS_TICK: Duration = Duration::from_secs(1);
/// Owned: the loop sleeps at most this long; stock receive workers sleep 1 ms.
pub const IDLE_POLL: Duration = Duration::from_millis(1);
/// Owned: the loop sleeps at most this long once nothing has used the unit for [`ACTIVE_HOLD`]
/// (no touch, knob, footswitch, MIDI, Cortex Control or remote input, no coasting list). The
/// unit has one core: this halves the wake-ups of an idle unit, and the first input after a
/// quiet time waits a few ms more.
pub const QUIET_POLL: Duration = Duration::from_millis(8);
/// Owned: how long after the last input the loop keeps polling at [`IDLE_POLL`].
pub const ACTIVE_HOLD: Duration = Duration::from_millis(500);
/// Notices kept for the UI and logs; older ones are dropped and counted.
pub const NOTICE_LIMIT: usize = 256;
/// Owned: a failed Wi-Fi bring-up is retried at this interval from [`Timer::Services`].
pub const SERVICES_RETRY: Duration = Duration::from_secs(10);
/// The read-only factory setlist key (`DAT_01560ef4`, MIDI CC#32 = 0).
pub const FACTORY_SETLIST: &str = "/opt/neuraldsp/Factory Library";

/// A power request from the menu, the power button, a DSP fault or an update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    PowerOff,
    Restart(RestartReason),
}

/// Everything that can happen to the device. One reducer per variant.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Hal(HalEvent),
    /// One DIN byte with its read time.
    Midi {
        byte: u8,
        at: Duration,
    },
    /// A drained DSP response other than a Cortex Control report.
    Dsp(CoreId, DspEvent),
    /// A Cortex Control HID report from the host (DSP response `0x10`).
    Control(Box<[u8; 128]>),
    Timer(Timer),
    /// Touch, knobs, footswitch edges and screen requests for the UI bridge.
    UiInput(ui::UiInput),
    /// A device action from any source; platforms and tests may inject one directly.
    Action(Action),
    Service(services::ServiceNotice),
    /// Runner output for the extensions bridge.
    Extension(extensions::ExtensionInput),
    /// An Extensions page request (also from tests and developer tools).
    Extensions(tetrad_ui::ExtensionCommand),
    /// A Mappings page request from outside the UI (tests, automation).
    Mappings(tetrad_ui::MappingCommand),
    /// A Custom footswitch mode request (the slot editor's, Gig View's).
    Custom(tetrad_ui::CustomCommand),
    /// A request of the Remote page (`remote`).
    Remote(tetrad_remote::Call),
    /// An event of the BLE host (`bluetooth`).
    Bluetooth(tetrad_ble::Event),
    Shutdown(PowerAction),
}

/// Device timers. One deadline per timer; starting a timer again moves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Timer {
    DismissSplash,
    /// Owned: the boot splash is a window of the stock QWS server, which paints its area black
    /// when `zensplash` exits after the dismiss, over Tetrad's frame (B13 boot 761). Once
    /// `zensplash` is gone on two polls in a row, Tetrad writes the whole frame again.
    SplashGone,
    /// Stock settings debounce (`SettingsFileId::save_delay`).
    SettingsFlush,
    ControlTick,
    /// tetrad-midi clock timeout (`MidiController::next_deadline`).
    MidiClock,
    /// Wi-Fi poll (stock 60 ms), internet probe and NTP (`services::TICK`); while the Wi-Fi
    /// bring-up has not succeeded, a retry every [`SERVICES_RETRY`].
    Services,
    /// The pending TEMPO or UP/DOWN hold (`input::Input::hold_deadline`).
    FootswitchHold,
    /// (O) A Custom mode press that waits for a double press or a combo
    /// (`custom::CustomMode::deadline`).
    CustomGesture,
    /// tetrad-ui clock ([`UI_TICK`]).
    UiTick,
    /// Neural Capture meter poll (`capture::Capture::meter_interval`).
    CaptureMeters,
    /// DSP diagnostics poll (`0x50ea74`), skipped while `/opt/neuraldsp/nopoll` exists.
    Diagnostics,
    /// The earliest model driver timer (`Dsp::next_timer`, `Block::timer_elapsed`).
    DspDriver,
    /// The one jack scheduler timer (`0x7d7450`).
    InputScheduler,
    /// QC mini volume hold ticks (`0x458914`).
    VolumeRepeat,
    /// QC mini shutdown after 3000 ms (`0x6a86c4`).
    PowerLongPress,
    /// Tuner meter request `0x38` every 50 ms while a consumer is registered (`0x4e159c`).
    TunerMeterPoll,
    /// No tuner reading for 500 ms (`0x4e1558`).
    TunerMeterIdle,
    /// Metronome single-shot flush, 10 ms (`0x556ed4`).
    MetronomeFlush,
    /// The earliest expression bypass auto-engage deadline (`0x6df618`).
    ExpressionBypass,
    /// Looper status poll `0x2b` every 50 ms while a Looper block exists (`0x55d85c`).
    LooperPoll,
    /// I/O SETTINGS save, 250 ms after the last edit (`jacks::SAVE_DELAY`, `0x35c95c`).
    ConnectorsSave,
    /// The 10 ms input mute settle before an I/O preset load (`jacks::begin_io_load`,
    /// `0x36352c`).
    IoPresetSettle,
    /// The next bank overlay blink phase (`leds::BLINK_PHASE`), while the overlay is open.
    OverlayBlink,
    /// I/O page meter poll every `meters::POLL` while `Screen::Io` shows (`0x4acabc`).
    IoMeters,
    /// Block editor meter poll every `meters::POLL` while [`State::editor_block`] is set
    /// (Parallax `0x636740` → `0x635fa0`).
    EditorMeters,
    /// Grid meter set poll (`0x596108`) every `meters::POLL` while the Grid shows a preset.
    /// Owned: stock keeps the set registered on every screen.
    GridMeters,
    /// Row meter poll (`meters::row_requests`, `0x4df0f4`) every `meters::POLL` while Cortex
    /// Control subscribes grid meters (r16-perform MR 3; I: the stock period).
    RowMeters,
    /// NC2 job re-poll (`Nc2Effect::SchedulePoll`, 1000 ms, `0x1fe630`).
    Nc2Poll,
    /// Every [`app::POLL`] while an extension program runs: its exit and the quit hold.
    App,
    /// Owned: Bluetooth starts again after a fault (`bluetooth`).
    Bluetooth,
}

/// Work a reducer asks for. Executed in order by [`Device`].
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Directory(tetrad_ui::DirectoryRequest),
    /// The library's folders changed outside a Directory edit (`Action::LinkMedia`): the
    /// directory projection is rebuilt from the library.
    RefreshDirectory,
    /// Audition a Downloads/Plugin preset: load its `.pb` path with no setlist slot, so
    /// `LastPreset` and the footswitch index stay (stock index -1, `0x5b57b8`).
    AuditionPreset(String),
    Dsp(DspCommand),
    /// A tetrad-engine update of the loaded preset (scene-independent edits, layout changes) for
    /// `Dsp::apply` with the committed deployment.
    Deploy(Update),
    /// Commit the proposed layout only after its DSP deploy succeeds.
    DeployLayout(preset::LayoutChange),
    /// An undo (`redo` false) or redo frame with a layout difference: deploy it, then move
    /// the history frame; a refused deploy keeps the frame. Cortex Control gets the live
    /// preset either way (`0x649330`).
    RestoreLayout {
        change: preset::LayoutChange,
        redo: bool,
    },
    /// A live input gate edit for `Dsp::apply_gate`.
    DeployGate(tetrad_engine::GateUpdate),
    Hal(HalCommand),
    /// One DIN MIDI message.
    MidiOut(Vec<u8>),
    /// Owned: one MIDI message for the BLE MIDI hosts (`bluetooth`).
    BluetoothMidi(Vec<u8>),
    /// Preset MIDI Out for a physical footswitch or pedal (`midi::Trigger`).
    PresetMidiOut(midi::Trigger),
    /// MIDI CC#35..=42: footswitch A..H (0..=7) as hardware edges
    /// (`input::Input::midi_footswitch`, `0x0020fb78`).
    MidiFootswitch(u8),
    /// One Cortex Control report to the host (DSP request `0x27`).
    ControlReport(Box<[u8; 128]>),
    Splash(SplashCommand),
    /// Render and present a UI frame; frames requested in one pass are presented once.
    Present,
    StartTimer {
        timer: Timer,
        after: Duration,
    },
    StopTimer(Timer),
    ScheduleInput(jacks::InputChange),
    /// A blocking stock wait (`usleep`) between two writes, as in the I/O type switch.
    Sleep(Duration),
    LoadPreset(PresetRef),
    /// A MIDI program change load: [`Self::LoadPreset`] with no load frame (`0x5b5bd8` →
    /// `0x5b2f40`, review r39 K13).
    ProgramChange(PresetRef),
    /// Undo (or redo) of the newest load frame (type 5): the old selection loads again with
    /// the frame's working preset (`Device::step_preset_load`).
    StepPresetLoad {
        redo: bool,
    },
    /// The selected slot's item was deleted (`0x7c4c1c` → `0x5b2c54`): the new-preset template
    /// plays in place with no file and no load frame (`Device::load_template`).
    LoadTemplate,
    /// A slot frame step changed the selected slot (`0x647fa8` → `0x5b2ce8`): the slot's
    /// preset becomes the clean preset and the frame's `working` preset of `owner` plays on
    /// it (`Device::restore_selected`).
    RestoreSelected {
        redo: bool,
        working: Option<Box<preset::Snapshot>>,
        owner: preset::LoadId,
    },
    /// Saves the loaded preset (`preset::save`); `None` keeps its file name.
    SavePreset(Option<String>),
    /// Edit Details of the loaded preset (`modifyPresetMetadataSignal` → `0x5b91dc` →
    /// `0x5b7438`): a save under `name` whose sidecar gets `instrument` (`0x7c4db8` `+0x5c`).
    EditPresetDetails {
        name: String,
        instrument: tetrad_ui::PreferredInstrument,
    },
    /// Save As (`preset::save_as_with`): a copy in the first free slot of a setlist (`None`:
    /// the first setlist) that becomes the loaded preset; a preset without a file saves into
    /// its own slot (`0x5ac350`). The new sidecar has `instrument` (`presetSaveSignal`).
    SavePresetAs {
        name: String,
        setlist: Option<String>,
        instrument: tetrad_ui::PreferredInstrument,
    },
    /// Save As into slot `slot` of `setlist` (an occupied slot is overwritten; `preset::save_as`),
    /// with the new preset's `instrument` (review r47 N1: one save, so a failed store cannot
    /// leave a details edit to run on the loaded preset).
    SavePresetInto {
        name: String,
        setlist: Option<String>,
        slot: usize,
        instrument: tetrad_ui::PreferredInstrument,
    },
    /// Runs through the action reducer ([`actions::reduce`]).
    Action(Action),
    /// An edit an extension made: [`Self::Action`] without the native-edit event to
    /// extensions, so an extension never hears its own writes.
    ExtensionAction(Action),
    /// A voice message on a channel an extension reserved (`extensions::Extensions::on_midi`).
    ExtensionMidi(tetrad_midi::MidiMessage),
    /// A native-channel message the stock MIDI map gives no job (`mappings::Mappings::on_midi`).
    MappingMidi(tetrad_midi::MidiMessage),
    /// A Stomp-mode footswitch press or release, 0..=7, for the preset's mappings
    /// (`mappings::Mappings::on_stomp`).
    MappingStomp {
        stomp: u8,
        pressed: bool,
    },
    /// MIDI output an extension or a Custom mode slot sent, through the controller's
    /// transmit path.
    ExtensionMidiOut(tetrad_midi::MidiMessage),
    /// A Custom mode slot runs a command that an extension registered.
    ExtensionCommand {
        key: String,
        command: String,
    },
    /// Start an extension program; it takes the screen and the footswitches ([`app`]).
    LaunchApp(app::AppLaunch),
    /// The Custom mode went on or off: [`custom::ACTIVE_FILE`] follows.
    CustomModeChanged,
    /// Input for the UI bridge: screens, knob steps, footswitch edges, the power button.
    UiInput(ui::UiInput),
    /// A settings group changed; the device tells consumers outside `State` (MIDI).
    SettingsChanged(SettingsEvent),
    /// The reset work outside the settings files, then the Cortex Control resync.
    ResetSteps(ResetKind),
    Power(PowerAction),
    Notice(Subsystem, String),
    /// Blocking service work for the platform worker (`ServiceBackends::run_job`).
    Service(services::ServiceJob),
    /// A Cortex Control backup request for the services bridge (r16-services MR-4).
    ControlBackup(services::ControlBackup),
    /// Its result for the Control bridge.
    ControlBackupDone(services::ControlBackupDone),
    /// The result of a Remote page backup for the Remote bridge (`Device::remote_backup_done`).
    RemoteBackupDone(services::RemoteBackupDone),
    /// The end of a Cortex Control cloud backup (CloudBackup 41) for the Control bridge.
    CloudBackupDone(control::CloudBackupDone),
    /// A restore job failed: the library comes back and `zenui.db` reopens (`0x3fcf64`).
    LibraryReopen,
    /// A restore job extracted the backup: settings, library and preset reload in place
    /// (`0x3fcf64`, `Device::reload_after_restore`).
    RestoreReload,
    /// Updater state for Cortex Control (`UpdaterMessage`, message 60).
    UpdaterReport(tetrad_control::codecs::UpdaterMessage),
    ControlNotify(Box<tetrad_control::codecs::ControlMessage>),
    /// A Grid echo of a live edit (`0x25f060`): skipped while a capture preset runs (`+0x758`),
    /// as the DSP write is.
    GridEcho(Box<tetrad_control::codecs::ControlMessage>),
    /// A Cortex Control notification with its own sender path, such as `Notification::Scene`
    /// (`0x27c3fc`), which has no `ControlMessage` form.
    ControlNotification(Box<tetrad_control::link::Notification>),
    /// Cortex Control account work for the services bridge (licence refresh, logout).
    ControlAccount(control::AccountRequest),
    /// A forced reload of a preset in place (`0x527d80`, `Device::recall_preset`).
    RecallPreset(Box<Preset>),
    /// A Cortex Control RecallPreset (`0x5b22a8`): the prepared preset replaces the working
    /// preset; its direct undo frame is kept when it commits (`Device::recall_prepared`).
    ControlRecall {
        prepared: Box<preset::Prepared>,
        undo: preset::PendingFrame,
    },
    /// The last effect of one Cortex Control request: its deferred reply and the pushes it
    /// caused go out in its request context (`control::Control::request_done`).
    ControlRequestDone,
    /// Cortex Control RemoteControl READ with a screenshot (`0x4a0480`, a whole-window grab):
    /// render the current UI frame and send it as a PNG (`control::Control::send_frame`).
    ControlFrame,
    /// The metronome resend `0x512c74` (`Dsp::send_metronome`).
    MetronomeResendPlan(crate::tempo::ResendPlan),
    /// Stock scene recall of the committed deployment (`Dsp::recall_scene`, `0x527248`) with
    /// the metronome resend after its bypass pass; `None` when the resend was refused.
    RecallScene(Option<crate::tempo::ResendPlan>),
    /// A Neural Capture DSPController reaction (`0x528498`) for `capture::run_entry`; its
    /// result goes back to the capture session.
    CaptureEntry(Box<capture::Entry>),
    /// Input for the NC2 controller (`capture::CaptureBridge::nc2`): Cortex Control message
    /// 63, cloud replies and timers.
    Nc2(tetrad_capture::v2::Nc2Input),
    /// A Cortex Control NeuralCapture (36) request for the V1 session
    /// (`Device::capture_remote`).
    CaptureRemote(capture::NcRemote),
    /// Cortex Control IOSettings EXP `calibrating` (`0x358330`, `0x35828c`): starts the
    /// pedal's calibration, or ends it and stores the tracked range, as the UI does.
    ExpressionCalibration {
        pedal: u8,
        calibrating: bool,
    },
    /// A downloaded NC2 product to decrypt and audition (`Device::nc2_downloaded`).
    Nc2Download(Box<services::Nc2Download>),
    /// A checked cloud product for its Downloads table (`Device::store_cloud_download`).
    StoreDownload(Box<services::CloudDownload>),
    /// A good Directory preset upload reply: the sidecar `cloudId` of `path` takes `cloud_id`
    /// (`0x1d5944`; `Device::preset_uploaded`), then the result dialog.
    PresetUploaded {
        path: String,
        name: String,
        cloud_id: String,
    },
    /// A good capture upload reply: the user capture `hash` takes `cloud_id` as its
    /// `cloudUUID` (`0x1d5944` → `0x7dd13c` → `0x82ddfc`; `Device::capture_uploaded`), then
    /// the result dialog.
    CaptureUploaded {
        hash: String,
        name: String,
        cloud_id: String,
    },
    /// A batch upload item ended (`0x847cf8`, its result is not read): the device starts the
    /// next item or ends the batch (`Device::upload_next`).
    UploadNext,
    /// A cloud request for the service worker (`services::Services::cloud_request`).
    Cloud(services::CloudRequest),
    /// The Preset MIDI Out editor stored a stomp (0..=7) or pedal (8, 9) list
    /// (`midi::Midi::source_edited`).
    PresetMidiSource(u8),
    /// The screen brightness setting (percent) as backlight (`0x409888` → `0x409da4`): the
    /// platform panel divides it ([`DisplayPanel::backlight`]).
    Backlight(i32),
    /// Reads the QC knob (`Query::Volume`) and applies [`input::read_volume`] (`0x1fc518`).
    ReadVolume,
    /// Cortex Control GigViewButton (55, `0x109ab4`): a Gig View tile press or release
    /// (`input::Input::gig_view_button`).
    GigViewButton {
        tile: u8,
        pressed: bool,
    },
}

/// One DSP request outside the deploy path.
#[derive(Debug, Clone, PartialEq)]
pub enum DspCommand {
    Request(Request),
    /// `0x20` to cores 0 and 1, `value = mute | flag << 1`.
    MasterMute(u32),
    /// CANCEL on the DSPErrorDialog: the fault mute ends (`Dsp::release_fault_mute`).
    ReleaseFaultMute,
    /// `0x05` selectors 1, 2, 4, 5 (`0x511968`); `None` mutes all four.
    InputTrims(Option<[f32; 4]>),
    /// A metronome, MIDI clock, USB MIDI or tempo control frame (`tetrad_dsp_protocol::controls`).
    Control(ControlRequest),
    /// A tuner (`0x34`..`0x38`) or Looper (`0x29`..`0x2c`) request.
    TunerLooper(TunerLooperRequest),
    /// `0x519dfc`: lane mutes for [`State::tuner_muted`], then the sidechain cut.
    TunerMute,
    /// The lane I/O push (`0x5205e8`) after the tuner engine goes off.
    LanePush,
    /// A Looper button edge or MIDI parameter to the block's driver (`Dsp::looper_parameter`).
    LooperParameter {
        site: tetrad_engine::Site,
        index: u32,
        value: f32,
    },
    /// Global audio settings again (`Dsp::apply_globals`: DDC and master volume assignment).
    Globals,
    /// A tempo change (`Dsp::set_tempo`, `0x875a48`): every driver's tempo hook, then `0x16`.
    Tempo(i32),
    /// A stomp press or release on the drivers of its blocks (`Dsp::footswitch`).
    Footswitch {
        stomp: tetrad_core::performance::StompIndex,
        pressed: bool,
    },
    /// A MIDI note or CC 120/123 for the grid drivers (`Dsp::block_midi`), channel 1..=16.
    BlockMidi {
        channel: u8,
        message: tetrad_dsp_host::setup::DriverMidi,
    },
}

/// A library slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetRef {
    /// Setlist key: the directory path (stock `LastPreset/setlist` form; [`FACTORY_SETLIST`]
    /// for the factory setlist). `Action::LoadPreset` turns a name into its path. `None` is
    /// the first setlist.
    pub setlist: Option<String>,
    pub index: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Subsystem {
    Platform,
    Splash,
    Settings,
    Library,
    Dsp,
    Preset,
    Ui,
    Control,
    Input,
    Midi,
    Services,
    Capture,
    Power,
    Extensions,
    Remote,
    Usb,
    Bluetooth,
}

/// A degraded step or an unsupported request, kept for the UI and the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub at: Duration,
    pub subsystem: Subsystem,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct Notices {
    recent: VecDeque<Notice>,
    pub dropped: u64,
    /// Times the last notice repeated (same subsystem and text); repeats are not kept again.
    pub repeats: u64,
}

impl Notices {
    /// Returns false for a repeat of the last notice.
    fn push(&mut self, notice: Notice) -> bool {
        if self
            .recent
            .back()
            .is_some_and(|last| last.subsystem == notice.subsystem && last.detail == notice.detail)
        {
            self.repeats += 1;
            return false;
        }
        if self.recent.len() == NOTICE_LIMIT {
            self.recent.pop_front();
            self.dropped += 1;
        }
        self.repeats = 0;
        self.recent.push_back(notice);
        true
    }

    pub fn iter(&self) -> impl Iterator<Item = &Notice> {
        self.recent.iter()
    }
}

/// A subsystem failure. The reducer that met it reports it as a notice.
#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("{0} is not available")]
    Unavailable(&'static str),
    #[error("platform: {0}")]
    Platform(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)]
    Hal(#[from] tetrad_hal::hal::HalError),
    #[error(transparent)]
    Settings(#[from] tetrad_settings::device::StorageError),
    #[error(transparent)]
    Library(#[from] tetrad_library::Error),
    #[error(transparent)]
    Database(#[from] tetrad_storage::Error),
    #[error(transparent)]
    Dsp(#[from] dsp::DspError),
    #[error(transparent)]
    Preset(#[from] preset::PresetError),
    #[error(transparent)]
    Action(#[from] actions::ActionError),
    #[error(transparent)]
    Ui(#[from] ui::UiError),
    #[error(transparent)]
    Control(#[from] control::ControlError),
    #[error(transparent)]
    Input(#[from] input::InputError),
    #[error(transparent)]
    Midi(#[from] midi::MidiError),
    #[error(transparent)]
    Services(#[from] services::ServiceError),
    #[error(transparent)]
    Capture(#[from] capture::CaptureError),
    /// A power step could not be attempted. `report` lists what ran before it.
    #[error("power sequence stopped at {step:?}: {source}")]
    PowerAborted {
        step: PowerStep,
        #[source]
        source: Box<DeviceError>,
        report: PowerReport,
    },
}

impl DeviceError {
    fn platform(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Platform(Box::new(error))
    }

    fn subsystem(&self) -> Subsystem {
        match self {
            Self::Unavailable(_) | Self::Platform(_) => Subsystem::Platform,
            Self::Hal(_) | Self::Input(_) => Subsystem::Input,
            Self::Settings(_) => Subsystem::Settings,
            Self::Library(_) | Self::Database(_) => Subsystem::Library,
            Self::Dsp(_) => Subsystem::Dsp,
            Self::Preset(_) => Subsystem::Preset,
            Self::Action(error) => error.subsystem(),
            Self::Ui(_) => Subsystem::Ui,
            Self::Control(_) => Subsystem::Control,
            Self::Midi(_) => Subsystem::Midi,
            Self::Services(_) => Subsystem::Services,
            Self::Capture(_) => Subsystem::Capture,
            Self::PowerAborted { .. } => Subsystem::Power,
        }
    }
}

/// Failures that stop the start-up: the device cannot run without them.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("platform: {0}")]
    Platform(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)]
    Settings(#[from] tetrad_settings::device::StorageError),
}

/// The stock storage: preset files and the user database.
pub struct Library {
    pub presets: StockLibrary,
    /// `zenui.db`; `None` when it could not be opened at boot.
    pub database: Option<StockDb>,
}

/// The loaded preset.
pub struct Current {
    /// `.pb` device path; `None` for the new-preset shape of an empty slot.
    pub path: Option<String>,
    pub preset: Preset,
    pub migration: MigrationReport,
    pub deployment: Deployment,
}

/// A value with a change count: reads go through `Deref`, and each [`Self::get_mut`] adds
/// one to the count, so a write cannot skip it. [`State::preset_dirty`] keeps its answer
/// until the count of `current` or `clean` moves. Only [`State::new`] makes one, so no value
/// can come back with an old count.
#[derive(Debug)]
pub struct Tracked<T> {
    value: T,
    revision: u64,
}

impl<T> Tracked<T> {
    /// Mutable access; counts as a change even when the caller writes nothing.
    pub fn get_mut(&mut self) -> &mut T {
        self.revision += 1;
        &mut self.value
    }

    /// The change count: the [`Self::get_mut`] calls since [`State::new`].
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

impl<T> std::ops::Deref for Tracked<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

/// tetrad-core performance state; `input.rs` and the action reducer drive it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Performance {
    pub footswitch: FootswitchContext,
    pub tap: TapTempo,
    /// HAL pressed mask of the footswitches.
    pub held: u16,
}

/// All subsystem state except the DSP session and the bridges. Bridges read it; the action
/// reducer and the device change it. Some bridge writes are direct on purpose: the input
/// bridge's `performance.held` and `performance.tap` (HAL edges), Control's
/// `control_connected` (link state), the MIDI controller's own `[MidiSettings]` copy and the
/// services bridge's account tokens. Tempo is `settings.tempo` (global mode) or the preset
/// tempo model; the MIDI controller owns the clock, and [`State::midi_clock`] is its copy
/// after each step ([`Action::MidiClock`]).
pub struct State {
    pub product: Product,
    pub platform: HalPlatform,
    pub settings: DeviceSettings,
    pub library: Option<Library>,
    /// Resolved catalog; needs the DSP `0x24` key.
    pub specs: Option<Specs>,
    /// The ModuleStats (35) parts that follow from [`Self::specs`] alone, built at the first
    /// READ (stock builds its size relations once at boot, `0x6ba7b8`). The specs are set
    /// once, before any Control step.
    pub module_stats: std::cell::OnceCell<control::StatsBase>,
    /// (O) The community blocks of the installed extensions and their build results.
    pub community: community::Installed,
    /// The loaded preset; a write goes through [`Tracked::get_mut`] ([`State::preset_dirty`]).
    pub current: Tracked<Option<Current>>,
    pub performance: Performance,
    pub soc2: Option<dsp::Soc2Status>,
    /// The DSP crash latch and its DSPErrorDialog ([`dsp::DspFault`]); the outputs stay muted
    /// until CANCEL or REBOOT.
    pub dsp_fault: Option<dsp::DspFault>,
    pub looper: Option<LooperStatus>,
    pub looper_playback: tetrad_dsp_host::setup::drivers::looper::LooperPlayback,
    /// Undo/redo frames of the preset, the library and the loads (`preset::History`).
    pub history: preset::History,
    /// The loaded preset as loaded or last saved (`+0x28`: load `0x5b2c54`, save `0x5b7438`).
    /// A difference from the live preset is the PresetDirty flag (`0x5ad250`,
    /// [`State::preset_dirty`]).
    pub clean: Tracked<Option<preset::Snapshot>>,
    /// Health r34 #9: the PresetDirty flag at the change counts of `current` and `clean`.
    /// The stock check runs every 100 ms; a full preset compare is ~0.1 ms.
    dirty_memo: Cell<Option<((u64, u64), bool)>>,
    /// The change count of the `zenui.db` model presets: each Virtual Device Preset write
    /// (create, overwrite, rename, delete, default) and each pin adds one, from the UI or
    /// Cortex Control. The picker reads its lists again only on a change.
    /// Stock emits `modelPresetCreated/Deleted/Updated` (`0x1f46c4`); the editor re-checks its
    /// match on each (`0x48c2f0`).
    pub model_preset_revision: u64,
    /// The ModelPreset (71) READ list reply as built at [`Self::model_preset_revision`] (A3,
    /// owned): stock builds it in each READ (`0x1d037c` → `0x1f6280`: the factory and user
    /// lists, then every special entry); the QC took 118 ms per build. Tetrad builds it at boot
    /// and at the first READ after a model preset write.
    pub model_preset_list: Option<(u64, tetrad_control::codecs::ModelPresetMessage)>,
    pub tuner_engine: bool,
    pub tuner_muted: bool,
    pub tuner_hz: f32,
    pub tuner_meter_subscribed: bool,
    pub metronome: Option<crate::tempo::TempoController>,
    pub metronome_status: Option<MetronomeStatus>,
    pub clock_bpm: Option<i32>,
    /// The MIDI bridge's clock flags and tempo ([`Action::MidiClock`]).
    pub midi_clock: midi::ClockStatus,
    /// Looper editor `+0x187` (I: the Looper view is shown; ctor `0x612438` clears it). While
    /// set with the Looper screen on top, footswitches drive the Looper tiles (`0x613510`).
    pub looper_view: bool,
    /// Looper X swap dialog: the tile whose action the next footswitch letter takes
    /// (`0x37ab08`). While set, other footswitch handling is blocked (`0x5898f4(1)`).
    pub looper_assign: Option<u8>,
    /// The UI shows "Assign footswitch" or Gig EDIT STOMP (`UiState::assign_mode`): the
    /// ModeController block (`0x5898f4(1)`) sends footswitch presses to the dialog only.
    pub footswitch_dialog: bool,
    pub pedals: [tetrad_core::performance::PedalState; 2],
    /// The last HAL jack detect (`HalEvent::Jacks`; stock reads the HAL word `+0xe`, `0x67b3b8`):
    /// the IOSettings `plugged` flags. Zero until the first report.
    pub jacks: tetrad_hal::zenjack::JackStatus,
    pub expression: features::Expression,
    pub now: Duration,
    /// The screen on top of the UI view stack, as the UI bridge last reported it
    /// ([`Action::ScreenShown`]). View state only: the UI owns what is on screen. The tuner
    /// (Preset MIDI Out gate), the tempo popup (TEMPO taps) and the Gig View are read from
    /// here; other sources only ask for a screen ([`Action::ShowTuner`] and the like). It never
    /// gates a deploy or the lane I/O push.
    pub screen: tetrad_ui::Screen,
    /// Device time of the last clip wake per overload channel (`soundcard_over1/2`).
    pub overload: [Option<Duration>; 2],
    /// Cortex Control handshake complete (keep-alive, ModelRepo, Connection).
    pub control_connected: bool,
    pub splash_dismissed: bool,
    pub notices: Notices,
    /// Services and capture views for the UI (`ui::views::Device::sync_views`).
    pub views: ui::views::SettingsViews,
    /// The loaded preset's sidecar `instrument_type`, read at its load; Save As starts on it
    /// (`0x58ed3c`).
    pub preset_instrument: tetrad_ui::PreferredInstrument,
    /// Live I/O SETTINGS values; `jacks::save` stores them.
    pub io: jacks::Connectors,
    /// I/O preset loads waiting for [`Timer::IoPresetSettle`] (`jacks::begin_io_load`).
    pub io_loads: Vec<jacks::IoPresetLoad>,
    /// UI state for the LED view ([`Action::UiLeds`]).
    pub ui_leds: leds::UiLeds,
    /// QC Master Volume knob and takeover (`input::MasterVolume`); the mini keeps its level
    /// in the `AtmaVolume` setting instead.
    pub master_volume: input::MasterVolume,
    /// I/O page meters (`meters`), polled while `Screen::Io` shows.
    pub io_meters: tetrad_ui::IoMeters,
    /// DSP CPU load from the Diagnostics replies (`0x364ba0`); the CPU monitor and Control
    /// CPULoad read it.
    pub cpu: meters::CpuLoad,
    /// DSP and USB Diagnostics pages: the last block per core and of the SOC2 ARM.
    pub diagnostics: info::DspDiagnostics,
    /// The last row meter values (`0x401c..=0x401f`) for the Control grid meter sender.
    pub row_meters: [Option<f32>; 4],
    /// The last Control model meter values (`meters::on_model_meter`, `0x110f60`).
    pub model_meters: meters::ModelMeters,
    /// Device Information values, read at boot (r16-ui MR 3).
    pub info: info::DeviceInfo,
    /// Plugin licences of the last DSP `0x20` answer, from `PluginLicenses.ini` at boot; deploys
    /// bypass plugin blocks that are not active (`services::Licences`).
    pub licences: services::Licences,
    /// Grid row port clip lights (`meters::GridClips`).
    pub grid_clips: meters::GridClips,
    /// User capture names by capture key ("My Captures"), from the media listing; the library
    /// pass `0x7c2da4` names capture blocks of every preset taken (`control::capture_names`).
    pub capture_names: BTreeMap<String, String>,
    /// The Directory capture order BANK UP capture cycling walks ([`Action::CaptureCycle`]);
    /// view state from the UI bridge. `None`: capture cycling is refused.
    pub capture_cycle: Option<actions::CaptureCycle>,
    /// The grid block whose editor shows ([`Action::EditorBlock`]); view state from the UI
    /// bridge. Its editor meters poll into `model_meters`.
    pub editor_block: Option<tetrad_engine::Slot>,
    /// Minivoicer voice notes by grid slot (`None`: a silent voice), from the driver's voice
    /// send (`0x575010` → editor read-out `0x61d994`); `dsp::Dsp::store_voice_notes`.
    pub voice_notes: BTreeMap<tetrad_engine::Slot, [Option<u8>; 2]>,
    /// Model 4004 values of the user default model preset, read at boot (`0x2fbf08` →
    /// `0x1f7e1c`); unsaved Global EQ indices fall back to them. `None`: the factory default.
    pub global_eq_default: Option<BTreeMap<u16, f32>>,
    /// Committed bypass copies and the scene copy clipboard (`actions::PerformState`).
    pub perform: actions::PerformState,
    /// (O) The Custom footswitch mode of an extension: layout, page and held slots.
    pub custom: custom::CustomMode,
    /// This boot's number (`/opt/neuraldsp/boot_count.txt`), for the support file.
    pub boot_count: Option<u32>,
    /// DSP response kinds without a consumer yet; each is noted once.
    unrouted: HashSet<Discriminant<DspEvent>>,
}

impl State {
    /// State before anything is loaded: footswitch mode, last preset index and Gig View access
    /// come from the settings. [`Device::start`] and the bridge tests build state with it.
    pub fn new(product: Product, platform: HalPlatform, settings: DeviceSettings) -> Self {
        let stored = settings.settings();
        let performance = Performance {
            footswitch: FootswitchContext {
                mode: actions::core_mode(stored.mode.mode())
                    .unwrap_or(tetrad_core::performance::FootswitchMode::Preset),
                current_preset: u32::try_from(stored.last_preset.preset_index).unwrap_or(0),
                overlay: None,
                scene_copy: false,
                gig_view_access: stored.general.gig_view_stomp_access,
                bank_size: input::bank_size(platform, &stored.mode),
                custom: false,
            },
            tap: TapTempo::default(),
            held: 0,
        };
        let io = jacks::Connectors::load(stored, settings.hardware());
        let licences = services::Licences::stored(&settings);
        Self {
            product,
            platform,
            settings,
            library: None,
            specs: None,
            module_stats: std::cell::OnceCell::new(),
            community: community::Installed::default(),
            current: Tracked {
                value: None,
                revision: 0,
            },
            performance,
            soc2: None,
            dsp_fault: None,
            looper: None,
            looper_playback: tetrad_dsp_host::setup::drivers::looper::LooperPlayback {
                cached_state: 1,
                incoming: LooperStatus::from_words([0; 22]),
                oneshot: false,
                dupwait: false,
                adopt_next_state: false,
                quantize_enabled: false,
            },
            history: preset::History::default(),
            clean: Tracked {
                value: None,
                revision: 0,
            },
            dirty_memo: Cell::new(None),
            model_preset_revision: 0,
            model_preset_list: None,
            tuner_engine: false,
            tuner_muted: false,
            tuner_hz: 0.0,
            tuner_meter_subscribed: false,
            metronome: None,
            metronome_status: None,
            clock_bpm: None,
            midi_clock: midi::ClockStatus::default(),
            looper_view: false,
            looper_assign: None,
            footswitch_dialog: false,
            pedals: [tetrad_core::performance::PedalState::default(); 2],
            jacks: tetrad_hal::zenjack::JackStatus::default(),
            expression: features::Expression::default(),
            now: Duration::ZERO,
            screen: tetrad_ui::Screen::Grid,
            overload: [None; 2],
            control_connected: false,
            splash_dismissed: false,
            notices: Notices::default(),
            views: ui::views::SettingsViews::default(),
            preset_instrument: tetrad_ui::PreferredInstrument::NotSet,
            io,
            io_loads: Vec::new(),
            ui_leds: leds::UiLeds::default(),
            master_volume: input::MasterVolume::default(),
            io_meters: tetrad_ui::IoMeters::default(),
            cpu: meters::CpuLoad::default(),
            diagnostics: info::DspDiagnostics::default(),
            row_meters: [None; 4],
            model_meters: meters::ModelMeters::default(),
            info: info::DeviceInfo::default(),
            licences,
            grid_clips: meters::GridClips::default(),
            capture_names: BTreeMap::new(),
            capture_cycle: None,
            editor_block: None,
            voice_notes: BTreeMap::new(),
            global_eq_default: None,
            perform: actions::PerformState::default(),
            custom: custom::CustomMode::default(),
            boot_count: None,
            unrouted: HashSet::new(),
        }
    }

    /// PresetDirty (`0x5ad250`): the live preset differs from the loaded or saved one
    /// ([`preset::Snapshot::matches`]). The full compare runs again only after a
    /// [`Tracked::get_mut`] of `current` or `clean`; debug builds check every kept answer.
    pub fn preset_dirty(&self) -> bool {
        let compare = || match (&*self.current, &*self.clean) {
            (Some(current), Some(clean)) => !clean.matches(current),
            _ => false,
        };
        let revision = (self.current.revision(), self.clean.revision());
        if let Some((memo, dirty)) = self.dirty_memo.get()
            && memo == revision
        {
            debug_assert_eq!(dirty, compare(), "a preset change outside Tracked::get_mut");
            return dirty;
        }
        let dirty = compare();
        self.dirty_memo.set(Some((revision, dirty)));
        dirty
    }

    /// Makes `slot` the loaded slot: `LastPreset` and the footswitch index change together
    /// (the third mirror is `Current::path`). The caller schedules the settings flush.
    pub(crate) fn select_slot(&mut self, slot: &PresetRef) {
        self.settings.update(|settings| {
            settings.last_preset.setlist = slot.setlist.clone();
            settings.last_preset.preset_index = i32::try_from(slot.index).unwrap_or(i32::MAX);
        });
        self.performance.footswitch.current_preset = slot.index;
    }
}

/// The subsystem bridges, one file each.
pub struct Bridges {
    pub ui: ui::Ui,
    pub appearance: appearance::Appearance,
    pub control: control::Control,
    pub input: input::Input,
    pub jacks: jacks::InputScheduler,
    pub leds: leds::Leds,
    pub midi: midi::Midi,
    pub services: services::Services,
    pub capture: capture::Capture,
    pub extensions: extensions::Extensions,
    pub mappings: mappings::Mappings,
    pub remote: remote::Remote,
    pub bluetooth: bluetooth::Bluetooth,
    pub boot_selector: boot_selector::BootSelector,
}

pub struct Device<P: Platform> {
    platform: P,
    clock: P::Clock,
    dsp: Option<dsp::Dsp<P>>,
    /// Factory and user IRs for cab and IR Loader blocks; `None` without the cipher or catalog.
    media: Option<media::MediaLibrary>,
    state: State,
    bridges: Bridges,
    timers: BTreeMap<Timer, Duration>,
    queue: VecDeque<Event>,
    /// A frame was requested in this pass.
    present: bool,
    /// When [`Platform::disk_space`] was last read ([`DISK_SPACE_HOLD`]).
    disk_space_at: Option<Duration>,
    /// [`Timer::SplashGone`] polls so far, and whether the last one found `zensplash` gone.
    splash_polls: u8,
    splash_gone: bool,
    /// Slowest wall time of each [`Self::step`] phase since the last [`Self::take_phase_max`].
    phase_max: PhaseTimes,
    /// UI renders (presented or not) since the last [`Self::take_renders`].
    renders: u32,
    /// Loop and frame times for the `perf` log line ([`perf`]).
    #[cfg(feature = "dev")]
    perf: perf::Perf,
    /// When the last input that someone made arrived ([`ACTIVE_HOLD`]).
    interaction_at: Duration,
    /// Grid clip lights as last requested for display ([`meters_need_frame`]).
    shown_clips: tetrad_ui::GridClips,
    /// Wall time of each phase of the last [`Self::step`].
    last_phases: PhaseTimes,
    /// The costliest unit of each phase of the last [`Self::step`] (bring-up `SLOW` lines).
    probe: probe::Probe,
    /// The Tetrad link over USB (`usb.rs`).
    usb: usb::UsbLink,
    /// The Wi-Fi bring-up (`services::Services::start`) succeeded.
    services_up: bool,
    /// The extension program that has the screen and the footswitches.
    app: Option<app::Running>,
    /// The touch that was down when the program ended (the quit hold): the UI gets none of it.
    app_touch: bool,
    /// [`bringup::RunOptions::STOCK`] for `run`; a bring-up run gates stages and writes.
    options: bringup::RunOptions,
    health: bringup::Health,
}

impl<P: Platform> Device<P> {
    /// Stock start-up order (ZenUI main `0x7dd68`, DSP `0x8a8420`/`0x89c848`/`0x876dec`, UI
    /// `0x5af624`/`0x545ac0`). Only platform identity and settings I/O are fatal; every
    /// other step degrades to a notice so the device still comes up.
    pub fn start(platform: P) -> Result<Self, StartupError> {
        Self::start_with(platform, bringup::RunOptions::STOCK)
    }

    /// [`Self::start`] under bring-up options: stage `probe` skips the DSP boot, the globals,
    /// the preset load and the tuner boot; `dsp` stops before the preset load; `--preset`
    /// replaces `LastPreset`; services start only with `options.services`.
    pub fn start_with(mut platform: P, options: bringup::RunOptions) -> Result<Self, StartupError> {
        let clock = platform.clock();
        let mut early = Vec::new();
        if let Err(error) = platform.splash().send(SplashCommand::new(progress::MAIN)) {
            early.push((Subsystem::Splash, error.to_string()));
        }
        let (product, hal_platform) = identify(&mut platform).map_err(StartupError::platform)?;
        let hardware = if hal_platform.is_atma() {
            Hardware::Atma
        } else {
            Hardware::QuadCortex
        };
        let (settings, issues) = DeviceSettings::load(platform.settings(), hardware)?;
        if !issues.is_empty() {
            early.push((
                Subsystem::Settings,
                format!("{} settings keys fell back to stock defaults", issues.len()),
            ));
        }
        let boot_count = platform.boot_count().map_err(StartupError::platform)?;
        let capture = capture::Capture::from_platform(&mut platform, hal_platform, &settings);
        let mut device = Self {
            bridges: Bridges {
                ui: ui::Ui::new(),
                control: control::Control::new(),
                input: input::Input::calibrated(&settings.settings().expression),
                jacks: jacks::InputScheduler::new(hal_platform),
                leds: leds::Leds::default(),
                midi: midi::Midi::new(),
                services: services::Services::new(),
                capture,
                extensions: extensions::Extensions::new(),
                mappings: mappings::Mappings::default(),
                appearance: appearance::Appearance::default(),
                remote: remote::Remote::default(),
                bluetooth: bluetooth::Bluetooth::default(),
                boot_selector: boot_selector::BootSelector::default(),
            },
            state: State::new(product, hal_platform, settings),
            platform,
            clock,
            dsp: None,
            media: None,
            timers: BTreeMap::new(),
            queue: VecDeque::new(),
            present: false,
            disk_space_at: None,
            splash_polls: 0,
            splash_gone: false,
            phase_max: PhaseTimes::default(),
            renders: 0,
            #[cfg(feature = "dev")]
            perf: perf::Perf::default(),
            interaction_at: Duration::ZERO,
            shown_clips: tetrad_ui::GridClips::default(),
            last_phases: PhaseTimes::default(),
            probe: probe::Probe::default(),
            usb: usb::UsbLink::default(),
            services_up: false,
            app: None,
            app_touch: false,
            options,
            health: bringup::Health::default(),
        };
        for (subsystem, detail) in early {
            device.notice(subsystem, detail);
        }
        // Before any Control step, whatever the library open does (review-2 R1).
        device.bridges.control.set_run_options(&device.options);
        // DSPController `+0xb884`, set as the DSP driver starts (`0x876dec`): the text of the
        // Control key request. Without the file stock sends an empty text.
        let serial = device
            .platform
            .services()
            .files()
            .read(Path::new(SERIAL_PATH));
        let serial = match serial.map(|raw| raw.map(|raw| DeviceSerial::parse(&raw))) {
            Ok(Some(Ok(serial))) => {
                device.bridges.control.set_device_serial(serial.clone());
                Some(serial)
            }
            Ok(None) => None,
            Ok(Some(Err(error))) => {
                device.notice(Subsystem::Control, error.to_string());
                None
            }
            Err(error) => {
                device.notice(Subsystem::Control, format!("device serial: {error}"));
                None
            }
        };
        let platform = device.state.platform;
        let zen_boards = !platform.is_atma() && !platform.has_ess_codec();
        let mut info = info::DeviceInfo::read(device.platform.services().files(), serial);
        if zen_boards {
            info.read_boards(device.platform.hal());
        }
        device.state.info = info;
        let stage = device.options.stage;
        let library = device.open_library();
        device.report(library);
        device.splash(progress::DSP_START);
        if stage.boots_dsp() {
            let booted = device.boot_dsp();
            device.report(booted);
            // Stock builds the FitCheckHelper size relations at boot (`0x6ba7b8`); here the
            // whole catalog part of ModuleStats (35), so the connect burst does not (A3).
            if let Some(specs) = &device.state.specs {
                (device.state.module_stats).get_or_init(|| control::StatsBase::new(specs));
                // The ModelPreset (71) READ list too (owned: stock builds it in the READ).
                let built = (device.bridges.control).build_model_preset_list(&mut device.state);
                if let Err(error) = built {
                    device.notice(Subsystem::Control, format!("ModelPreset list: {error}"));
                }
            }
            // The DSPController ctor ends with PowerRailMute 0 on the mini (`0x876dec` →
            // `0x515e00`, gate `0x499a60`). Bring-up skips it (never verified on hardware).
            if device.state.platform.is_atma() && !device.options.is_bringup() {
                let unmute = Request::PowerRailMute { value: 0 };
                device.execute(Effect::Dsp(DspCommand::Request(unmute)));
            }
        }
        device.splash(progress::DSP_READY);
        device.state.boot_count = Some(boot_count);
        device.migrate_settings(boot_count);
        // Before the first Tetrad frame, so a boot shows the chosen theme from the start.
        device.load_appearance();
        device.load_custom();
        // Review r30 C7: one `fw_printenv` per boot for both readers below (stock loads the map
        // once per process, `0x47d994`); a failure is reported by each reader.
        let mut boot_env = None;
        let mut boot_env = |device: &mut Self| {
            boot_env
                .get_or_insert_with(|| {
                    tetrad_services::bootenv::load(device.platform.services().runner())
                        .map_err(|error| error.to_string())
                })
                .clone()
        };
        if stage.boots_dsp() {
            let globals = device.apply_globals();
            device.report(globals);
        }
        if stage.boots_dsp() {
            device.load_global_eq_default();
            // Inferred place: the stored Global EQ (`0x2fc9f4`) after the globals.
            for effect in global_eq::push(&device.state) {
                device.execute(effect);
            }
            // Inferred place: the metronome driver's sound upload after the globals.
            let sounds = device.upload_metronome_sounds();
            device.report(sounds);
            // The MasterVolume ctor (`0x1fd8d0`) reads the QC knob range from the U-Boot
            // environment (`0x1fcfb4`; `fw_printenv`, read-only). Bring-up skips it (never
            // verified on hardware) and keeps 2..=242.
            if !device.state.platform.is_atma() && !device.options.is_bringup() {
                let env = boot_env(&mut device).unwrap_or_else(|error| {
                    device.notice(Subsystem::Input, format!("master volume range: {error}"));
                    BTreeMap::new()
                });
                if let Err(error) = input::load_volume_calibration(&mut device.state, &env) {
                    device.notice(Subsystem::Input, error.to_string());
                }
            }
            // The knob read of the boot sequence (`0x7e468` → `0x1fc518`).
            device.execute(Effect::ReadVolume);
            // The licence manager signs, then applies the stored licence data (`0x1ed7b8` →
            // `0x1f1050`).
            let now = device.clock.now();
            let files = device.platform.services().files();
            for effect in
                (device.bridges.services).boot_licences(files, &device.state.settings, now)
            {
                device.execute(effect);
            }
            // The Grid view registers its meter set when it is built (`0x59d17c` → `0x596108`).
            device.start_timer(Timer::GridMeters, meters::POLL);
        }
        // The cloud session reads the saved tokens at boot, so a factory reset can still unpair.
        let cloud = (device.bridges.services)
            .load_cloud(&mut device.state.settings)
            .map_err(DeviceError::from);
        device.report(cloud);
        device.splash(progress::GLOBALS);
        let midi = device.bridges.midi.boot(&device.state);
        device.apply(midi);
        if stage.deploys() {
            let last = &device.state.settings.settings().last_preset;
            let last = PresetRef {
                setlist: last.setlist.clone(),
                index: u32::try_from(last.preset_index).unwrap_or(0),
            };
            let loaded = match device.options.preset.clone() {
                Some(path) => device.load_preset_path(Some(path), None, true),
                None => device.load_preset(&last, true),
            };
            device.report(loaded);
            // `0x5b3924`: the engine, then the stored port and the lane mutes (off at boot).
            let tuner = actions::reduce(Action::TunerEngine(true), &mut device.state);
            device.apply(tuner);
            for effect in features::tuner_path(&device.state) {
                device.execute(effect);
            }
        }
        device.splash(progress::PRESET);
        device.splash(progress::UI_BUILD);
        // The I/O page restore (`0x35c4ac`) runs inside the UI build (`0x5af624`: splash
        // `0x50`, `0x5ae644`, the restore, splash `0x5a`), after the MIDI boot and the preset:
        // every connector value under the input mute, and its MIDI THRU wins over `Passthru`.
        for effect in jacks::restore(&mut device.state, tetrad_hal::zenjack::ZENUI_VERIFY) {
            device.execute(effect);
        }
        // `0x5ae644` also builds the output calibration view (`0x36d970` → `0x36d734`): ES9017
        // boards send the U-Boot calibration gains; a failed `fw_printenv` sends the defaults.
        // Bring-up skips it (never verified on hardware).
        if stage.boots_dsp()
            && device.state.platform.has_ess_codec()
            && !device.options.is_bringup()
        {
            let env = boot_env(&mut device).unwrap_or_else(|error| {
                device.notice(Subsystem::Dsp, format!("output calibration: {error}"));
                BTreeMap::new()
            });
            for request in dsp::output_calibration(&env) {
                device.execute(Effect::Dsp(DspCommand::Request(request)));
            }
        }
        let built = device.bridges.ui.build(&device.state);
        device.report(built.map_err(DeviceError::from));
        // "SEE WHAT'S NEW" after an update, else Excessive Downloads (`0x22cab0` from
        // `0x54491c`; stock runs one of the two).
        if !device.show_update_notes()
            && let Ok(Some(kind)) = device.excessive_downloads()
        {
            for effect in device.on_ui(ui::UiInput::ExcessiveDownloads(kind)) {
                device.execute(effect);
            }
            // The Control push now, or on the next connect (`0x105d94`).
            let now = device.clock.now();
            let pushed = device
                .bridges
                .control
                .excessive_downloads(kind, &mut device.state, now);
            let effects = device.report(pushed.map_err(DeviceError::from));
            for effect in effects.unwrap_or_default() {
                device.execute(effect);
            }
        }
        // The Display page built with the UI sends the stored brightness (`0x40a07c` →
        // `0x409da4`).
        let brightness = device.state.settings.settings().display.brightness.get();
        device.execute(Effect::Backlight(brightness));
        device.splash(progress::UI_BUILT);
        device.present_frame();
        device.splash(progress::FULL_SCREEN);
        device.start_timer(Timer::DismissSplash, SPLASH_DISMISS_DELAY);
        device.start_timer(Timer::ControlTick, CONTROL_TICK);
        device.start_timer(Timer::UiTick, UI_TICK);
        device.start_timer(Timer::Diagnostics, DIAGNOSTICS_TICK);
        if device.options.services {
            let services = device.start_services();
            device.apply(services);
        }
        Ok(device)
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn platform(&self) -> &P {
        &self.platform
    }

    pub fn dsp(&self) -> Option<&dsp::Dsp<P>> {
        self.dsp.as_ref()
    }

    pub fn bridges(&self) -> &Bridges {
        &self.bridges
    }

    /// Runs until a power action, then performs it.
    pub fn run(mut self) -> Result<PowerReport, DeviceError> {
        loop {
            if let Some(action) = self.step() {
                return self.power(action);
            }
        }
    }

    /// One deterministic pass: HAL, DSP and platform input in that order, then due timers,
    /// then at most one frame. Sleeps up to [`IDLE_POLL`] when nothing happened. Returns a
    /// requested power action.
    pub fn step(&mut self) -> Option<PowerAction> {
        use probe::{Label, Phase};
        self.last_phases = PhaseTimes::default();
        self.probe.reset();
        #[cfg(feature = "dev")]
        let began = self.clock.now();
        let mut lap = Lap::start();
        self.probe.enter_at(lap.0);
        let busy = self.gather();
        let at = self.probe.leave(Label::text("gather"));
        lap.end(at, &mut self.last_phases.gather);
        let now = self.clock.now();
        let due: Vec<Timer> = self
            .timers
            .iter()
            .filter(|&(_, &deadline)| deadline <= now)
            .map(|(&timer, _)| timer)
            .collect();
        let mut due: Vec<(Duration, Timer)> = due
            .into_iter()
            .filter_map(|timer| self.timers.remove(&timer).map(|at| (at, timer)))
            .collect();
        due.sort();
        self.queue
            .extend(due.iter().map(|&(_, timer)| Event::Timer(timer)));
        let handled = !self.queue.is_empty();
        // re-r44 §3: cloud calls go over Cortex Control while it is connected and online.
        let route = self.bridges.control.cloud_route();
        self.bridges.services.set_control_route(route);
        // One Cortex Control message per pass: the reports after it wait, in order, for the
        // next pass, while touch, DSP and timer events still run in this one. The connect
        // burst (about 30 READs) held one QC pass for 2.6 s (r48, owned: stock replies on
        // receive, `0x290ae8` → `0x28e8ac`). A File or CloudProduct READ reply in progress
        // takes the place of that message (A3, `Control::spread_step`).
        let mut control_done = false;
        self.probe.set_phase(Phase::Reduce);
        if self.bridges.control.spreading() {
            let label = Label::new(self.bridges.control.spread_label());
            self.probe.enter();
            let spread = (self.bridges.control).spread_step(&mut self.state, self.clock.now());
            self.probe.leave(label);
            self.apply(spread);
            lap.add(&mut self.last_phases.reduce);
            control_done = true;
        }
        let mut held = VecDeque::new();
        while let Some(event) = self.queue.pop_front() {
            if let Event::Shutdown(action) = event {
                return Some(action);
            }
            let control = matches!(event, Event::Control(_));
            if control && control_done {
                held.push_back(event);
                continue;
            }
            let label = Label::of(&event);
            self.probe.set_phase(Phase::Reduce);
            self.probe.enter();
            let effects = self.reduce(event);
            let received = control
                .then(|| self.bridges.control.last_received())
                .flatten();
            self.probe.leave(received.map_or(label, |message| {
                Label::new(format_args!("Control {message}"))
            }));
            lap.add(&mut self.last_phases.reduce);
            control_done |= control
                && (effects.as_ref()).is_ok_and(|effects| {
                    (effects.iter()).any(|effect| matches!(effect, Effect::ControlRequestDone))
                });
            self.probe.set_phase(Phase::Apply);
            self.apply(effects);
            lap.add(&mut self.last_phases.apply);
        }
        self.queue = held;
        self.probe.set_phase(Phase::Control);
        let capturing = self.bridges.capture.capture_running();
        self.probe.enter();
        let control = self
            .bridges
            .control
            .sync(&mut self.state, capturing, self.clock.now());
        self.probe.leave(Label::text("Control sync"));
        self.apply(control);
        // re-r38 §3: a question of a Control-started upload goes to Cortex Control only.
        let ask = (self.bridges.services.upload_prompt())
            .filter(|question| question.from_control)
            .map(|question| control::UploadAsk {
                kind: question.kind,
                question: question.serial,
                title: question.title(),
                text: question.text(),
                batch: question.batch,
            });
        let now = self.clock.now();
        let asked = (self.bridges.control).upload_question(ask, &mut self.state, now);
        self.apply(asked);
        // re-r44 §2: BulkOperation (57) start, progress and finished of a Multi Select run.
        let run = self.bridges.services.bulk_run();
        (self.bridges.control).set_batch_end(self.bridges.services.last_batch_end());
        let bulk = (self.bridges.control).bulk_run(run, &mut self.state, now);
        self.apply(bulk);
        // IOMeter CREATE registers the I/O page meter set (`0x6e08b4`): it polls as for the
        // page while Control stays subscribed.
        if self.bridges.control.io_meters_subscribed() {
            (self.timers)
                .entry(Timer::IoMeters)
                .or_insert(self.clock.now() + meters::POLL);
        }
        if self.bridges.control.grid_meters_subscribed() {
            (self.timers)
                .entry(Timer::RowMeters)
                .or_insert(self.clock.now() + meters::POLL);
        }
        self.probe.enter();
        self.send_control_reports(control::outgoing::FRAMES_PER_PASS);
        self.send_usb_reports(usb::REPORTS_PER_PASS);
        let at = self.probe.leave(Label::text("send_reports"));
        lap.end(at, &mut self.last_phases.control);
        self.probe.set_phase(Phase::Views);
        self.probe.enter_at(at);
        self.sync_looper();
        let at = self.probe.leave(Label::text("sync_looper"));
        self.probe.enter_at(at);
        self.sync_voice_notes();
        let at = self.probe.leave(Label::text("sync_voice_notes"));
        self.probe.enter_at(at);
        self.sync_views();
        let at = self.probe.leave(Label::text("sync_views"));
        lap.end(at, &mut self.last_phases.views);
        self.probe.set_phase(Phase::Frame);
        // A running program owns the screen; the frame waits for its end. Without a frame the
        // phase is empty.
        if self.present && self.app.is_none() {
            self.probe.enter_at(at);
            self.present_frame();
            let at = self.probe.leave(Label::text("present_frame"));
            lap.end(at, &mut self.last_phases.frame);
        }
        self.phase_max = self.phase_max.max(self.last_phases);
        #[cfg(feature = "dev")]
        {
            let ended = self.clock.now();
            let probe = &self.probe;
            self.perf
                .step(ended.saturating_sub(began), self.last_phases, || {
                    probe.summary(Duration::from_millis(10))
                });
            self.perf.sync(self.bridges.ui.take_sync_times());
            if let Some(line) = self.perf.report(ended) {
                self.notice(Subsystem::Platform, line);
            }
        }
        self.sync_driver_timer();
        self.sync_bluetooth_pairable();
        match self.state.custom.deadline() {
            Some(at) => self.timers.insert(Timer::CustomGesture, at),
            None => self.timers.remove(&Timer::CustomGesture),
        };
        // Like the stock zero-interval sender timer, queued Control reports keep the loop awake.
        let control = &self.bridges.control;
        if !busy && !handled && !control.spreading() && !control.transmitting() {
            let next = self.timers.values().min().copied();
            let quiet = now.saturating_sub(self.interaction_at) >= ACTIVE_HOLD
                && !self.bridges.ui.animating();
            let poll = if quiet { QUIET_POLL } else { IDLE_POLL };
            let wait = next.map_or(poll, |at| at.saturating_sub(now).min(poll));
            self.clock.clone().sleep(wait);
        }
        None
    }

    /// The slowest wall time of each step phase since the last call (bring-up `HEALTH`).
    pub fn take_phase_max(&mut self) -> PhaseTimes {
        std::mem::take(&mut self.phase_max)
    }

    /// UI renders since the last call (bring-up `HEALTH`): an unchanged frame still costs the
    /// snapshot, layout and diff.
    pub fn take_renders(&mut self) -> u32 {
        std::mem::take(&mut self.renders)
    }

    /// The wall time of each phase of the last [`Self::step`] (bring-up `SLOW` lines).
    pub fn last_phases(&self) -> PhaseTimes {
        self.last_phases
    }

    /// The costliest unit of each phase of the last [`Self::step`] that took at least
    /// `floor`: `reduce: Control File Read 141ms; apply: ControlReport x290 120ms` (A3).
    pub fn last_units(&self, floor: Duration) -> String {
        self.probe.summary(floor)
    }

    /// Sends up to `limit` queued Cortex Control frames (stock `sendHIDReportsSlot`
    /// `0x29ae64`, once per pass). Without a DSP the queue is dropped.
    pub(super) fn send_control_reports(&mut self, limit: usize) {
        if !self.bridges.control.transmitting() {
            return;
        }
        let result = match self.dsp.as_mut() {
            Some(dsp) => self.bridges.control.send_reports(dsp, limit),
            None => {
                let dropped = self.bridges.control.drop_reports();
                self.notice(
                    Subsystem::Control,
                    format!("{dropped} Control reports dropped: no DSP"),
                );
                Ok(())
            }
        };
        if let Err(error) = result {
            self.notice(Subsystem::Control, error.to_string());
        }
    }

    /// Queues one event, for platforms or tests that inject directly. An injected event counts
    /// as input for the loop's [`ACTIVE_HOLD`].
    pub fn push(&mut self, event: Event) {
        self.interaction_at = self.clock.now();
        self.queue.push_back(event);
    }

    /// Drains every source once. Returns whether any input arrived.
    fn gather(&mut self) -> bool {
        let before = self.queue.len();
        let mut hal = Vec::new();
        if let Err(error) = self.platform.hal().poll_events(&mut hal) {
            self.notice(Subsystem::Input, error.to_string());
        }
        self.queue.extend(hal.into_iter().map(Event::Hal));
        let mut usb = Vec::new();
        while let Some(dsp) = self.dsp.as_mut() {
            match dsp.next_event() {
                // Tetrad reports never reach the Cortex Control link (`usb.rs`).
                Ok(Some((_, DspEvent::HidReport(report)))) if tetrad_usb::is_tetrad(&report.0) => {
                    usb.push(report.0);
                }
                Ok(Some((_, DspEvent::HidReport(report)))) => {
                    self.queue.push_back(Event::Control(Box::new(report.0)));
                }
                // The signing material, the client id and the stamp key go to services at
                // once: a queue slot would keep a stale copy after the move (review r11 K3).
                Ok(Some((_, DspEvent::UsbMeterUpdate(meter)))) => {
                    self.bridges.services.on_usb_meter_update(meter);
                }
                Ok(Some((_, DspEvent::TunerCalibrate(reply)))) => {
                    self.bridges.services.on_tuner_calibrate(reply);
                }
                Ok(Some((_, DspEvent::UsbMeterOutput(reply)))) => {
                    let effects = self.bridges.services.on_usb_meter_output(reply);
                    self.apply(Ok::<_, DeviceError>(effects));
                }
                // The key exchange answer `0x17` completes a waiting Version READ (`0x2a4f58`).
                Ok(Some((_, DspEvent::UsbMeterInput { first, second }))) => {
                    let now = self.clock.now();
                    let effects = (self.bridges.control).complete_key_exchange(
                        &first,
                        &second,
                        &mut self.state,
                        now,
                    );
                    self.apply(effects);
                }
                Ok(Some((core, event))) => self.queue.push_back(Event::Dsp(core, event)),
                Ok(None) => break,
                Err(error) => {
                    self.notice(Subsystem::Dsp, error.to_string());
                    break;
                }
            }
        }
        for report in usb {
            self.usb_report(&report);
        }
        let mut input = Vec::new();
        if let Err(error) = self.platform.poll_input(&mut input) {
            self.notice(Subsystem::Platform, error.to_string());
        }
        platform::drop_replaced_moves(&mut input);
        // A restore owns the user data until its extract ends: a power request is dropped, as
        // stock runs the extract on the main thread (review r19 F3).
        let held = self.restore_hold();
        self.queue.extend(input.into_iter().filter_map(|input| {
            Some(match input {
                Input::MidiByte { byte, at } => Event::Midi { byte, at },
                Input::Touch(sample) => Event::UiInput(ui::UiInput::Touch(sample)),
                Input::Service(notice) => Event::Service(notice),
                Input::Extension(input) => Event::Extension(input),
                Input::Remote(call) => Event::Remote(call),
                Input::Bluetooth(event) => Event::Bluetooth(event),
                Input::Power(_) if held => return None,
                Input::Power(action) => Event::Shutdown(action),
            })
        }));
        // Only input that someone makes: USB and mini MIDI arrive from the DSP, but DSP meters and
        // diagnostics arrive on their own.
        if (self.queue.iter().skip(before)).any(|event| {
            matches!(
                event,
                Event::Hal(_)
                    | Event::Midi { .. }
                    | Event::Dsp(_, DspEvent::MidiReceived { .. })
                    | Event::Control(_)
                    | Event::UiInput(ui::UiInput::Touch(_))
                    | Event::Extension(_)
                    | Event::Remote(_)
                    | Event::Bluetooth(tetrad_ble::Event::Midi(_))
            )
        }) {
            self.interaction_at = self.clock.now();
        }
        self.queue.len() > before
    }

    /// The one reducer path per event kind.
    fn reduce(&mut self, event: Event) -> Result<Vec<Effect>, DeviceError> {
        let now = self.clock.now();
        actions::reduce(Action::Time(now), &mut self.state)?;
        // The update install clears the input mask (`0x4e63a4` → `0x1ec83c(0x3f)`). Its readers
        // drop touch (`0x544534`), footswitch and expression values (`0x50b548`), the knob
        // (`0x1fb980`) and the POWER popup (`0x6a82ac`..`0x6a8a64`); the stock screen says the
        // touchscreen and footswitches are disabled. Owned: MIDI input is dropped too (UART
        // here, DSP `0x26` and USB `0x07` records in `on_dsp`; review r18 F11).
        // Cortex Control stays live (stock not verified, `0x16ad60`); the DSP mute holds.
        // A backup restore drops the same input (`restore_hold`).
        let masked = match &event {
            Event::Hal(HalEvent::Jacks(_) | HalEvent::Overload(_)) => false,
            Event::Hal(_) | Event::Midi { .. } | Event::UiInput(ui::UiInput::Touch(_)) => true,
            _ => false,
        };
        if masked && (self.bridges.services.inputs_masked() || self.restore_hold()) {
            return Ok(Vec::new());
        }
        // A running program reads the footswitches, the knobs and TEMPO itself, and the touch
        // only counts for its quit hold. The power button ends it, so that the power dialog shows.
        if self.app_touch
            && let Event::UiInput(ui::UiInput::Touch(sample)) = &event
        {
            self.app_touch = sample.pressed;
            return Ok(Vec::new());
        }
        if let Some(app) = &mut self.app {
            match &event {
                Event::Hal(HalEvent::Knob { .. } | HalEvent::Stomp { .. } | HalEvent::Tempo(_)) => {
                    return Ok(Vec::new());
                }
                Event::Hal(HalEvent::PowerButton(_)) => app.stop(),
                Event::UiInput(ui::UiInput::Touch(sample)) => {
                    app.touch(sample.pressed, now);
                    return Ok(Vec::new());
                }
                _ => {}
            }
        }
        let state = &mut self.state;
        Ok(match event {
            Event::Hal(event) => {
                use tetrad_hal::zencoder::VolumeEvent;
                let level = match event {
                    HalEvent::Volume(VolumeEvent::Level(raw)) => Some(raw),
                    _ => None,
                };
                // LOCK and Be Right Back hold the Master Volume knob and the mini keys (manual
                // §3; mini lock mask `0x11`, `0x7d4bc4`); otherwise an applied turn shows the
                // tetrad-ui Master Volume overlay.
                let view = self.bridges.ui.view();
                if matches!(event, HalEvent::Volume(_)) && (view.locked() || view.away()) {
                    return Ok(Vec::new());
                }
                let shown = state.splash_dismissed;
                state.footswitch_dialog = view.assign_mode();
                let effects = self.bridges.input.on_hal(event, state, now);
                self.sync_hold_timer();
                let mut effects = effects?;
                if let Some(raw) = level
                    && shown
                    && !effects.is_empty()
                {
                    let calibration = self.state.master_volume.calibration;
                    let percent = (calibration.level(raw) * 100.0).round();
                    effects.push(Effect::UiInput(ui::UiInput::View(
                        tetrad_ui::InputEvent::MasterVolume {
                            percent: percent.clamp(0.0, 100.0) as u8,
                        },
                    )));
                }
                // The UI takes a footswitch edge at once, so its input stamp comes before the
                // notices of the footswitch actions (review r14 U-2).
                let (edges, actions): (Vec<_>, Vec<_>) = effects.into_iter().partition(|effect| {
                    matches!(
                        effect,
                        Effect::UiInput(ui::UiInput::View(
                            tetrad_ui::InputEvent::Footswitch { .. }
                        ))
                    )
                });
                let mut effects = Vec::new();
                for edge in edges {
                    if let Effect::UiInput(input) = edge {
                        effects.extend(self.on_ui(input));
                    }
                }
                effects.extend(actions);
                effects
            }
            Event::Midi { byte, at } => self.bridges.midi.on_din_byte(byte, at, state)?,
            Event::Extensions(command) => {
                self.extensions(|extensions, backend, _| extensions.on_ui(command, backend))
            }
            Event::Mappings(command) => self.mapping_command(command),
            Event::Custom(command) => self.custom_command(command),
            Event::Remote(call) => {
                self.remote_call(call);
                Vec::new()
            }
            Event::Bluetooth(event) => self.on_bluetooth(event, now),
            Event::Extension(input) => {
                let native = self.bridges.midi.native_channel();
                self.extensions(|extensions, backend, state| {
                    extensions.on_input(input, state, native, backend)
                })
            }
            Event::Dsp(core, event) => self.on_dsp(core, event)?,
            Event::Control(report) => {
                // A File (4) IR import dates its metadata (`0x24dae0`).
                (self.bridges.control).set_wall_clock(self.platform.wall_clock());
                // GeneralSettings (9) READ sends the `/media/p4` space (`0x16c964`); stock
                // keeps a reading for 10 minutes (`0x210f6c`).
                if (self.disk_space_at).is_none_or(|at| now.saturating_sub(at) >= DISK_SPACE_HOLD) {
                    self.disk_space_at = Some(now);
                    (self.bridges.control).set_disk_space(self.platform.disk_space());
                }
                let multi_select = self.bridges.ui.view().multi_select().is_some();
                self.bridges.control.set_multi_select(multi_select);
                // CloudProduct (46) READ lists the downloads in flight (`0xf5470`).
                (self.bridges.control)
                    .set_downloads_in_flight(self.bridges.services.downloads_in_flight());
                (self.bridges.control).set_upload_states(self.bridges.services.upload_states());
                self.bridges.control.on_report(&report, state, now)?
            }
            Event::Timer(timer) => self.on_timer(timer)?,
            Event::UiInput(input) => self.on_ui(input),
            Event::Action(action) => {
                self.bridges.control.record_drop(&action, state);
                actions::reduce(action, state)?
            }
            Event::Service(notice) => self.bridges.services.on_notice(
                self.platform.services(),
                notice,
                &mut state.settings,
                now,
            )?,
            Event::Shutdown(action) => vec![Effect::Power(action)],
        })
    }

    fn on_dsp(&mut self, core: CoreId, event: DspEvent) -> Result<Vec<Effect>, DeviceError> {
        let now = self.clock.now();
        let midi_held = self.bridges.services.inputs_masked() || self.restore_hold();
        let (state, bridges) = (&mut self.state, &mut self.bridges);
        let effects = match event {
            DspEvent::LooperStatus(status) => {
                let context = features::looper_context(state);
                self.dsp
                    .as_mut()
                    .ok_or(DeviceError::Unavailable("the DSP"))?
                    .looper_status(&status, &context)?;
                actions::reduce(Action::LooperStatus(status), state)?
            }
            DspEvent::MetronomeStatus(status) => {
                actions::reduce(Action::MetronomeStatus(status), state)?
            }
            DspEvent::Soc2SoundsReceived => vec![Effect::Dsp(DspCommand::Control(
                ControlRequest::MetronomeSound,
            ))],
            DspEvent::Meter(record) if record.tuner_detected_hz().is_some() => {
                actions::reduce(Action::TunerReading(record.tuner_detected_hz()), state)?
            }
            DspEvent::Crash(cause) => {
                self.health.crashes += 1;
                let mut effects = vec![Effect::Notice(
                    Subsystem::Dsp,
                    format!("{core:?} crashed: {cause:?}"),
                )];
                let cause = dsp::FaultCause::Crash { core, cause };
                let muted = dsp::raise_fault(self.dsp.as_mut(), &mut state.dsp_fault, cause);
                effects.extend(fault_mute_notice(muted));
                self.present = true;
                effects
            }
            // Stock logs both (`0x89ece0`); only error flags fail the health test `0x89b2f0`.
            DspEvent::ErrorFlagsChanged { core, flags } => {
                let n = core as u8 + 1;
                let mut effects = vec![Effect::Notice(
                    Subsystem::Dsp,
                    format!("DSP Core-{n} Error flags detected 0x{flags:X}"),
                )];
                if flags != 0 {
                    let cause = dsp::FaultCause::ErrorFlags { core, flags };
                    let muted = dsp::raise_fault(self.dsp.as_mut(), &mut state.dsp_fault, cause);
                    effects.extend(fault_mute_notice(muted));
                    self.present = true;
                }
                effects
            }
            DspEvent::FloatingPointException { core, count, slot } => {
                let n = core as u8 + 1;
                vec![Effect::Notice(
                    Subsystem::Dsp,
                    format!("DSP Core-{n} FPEx detected {count} slot {slot}"),
                )]
            }
            // Model starts also answer a running capture session (`0x33a648`).
            event @ (DspEvent::ModelStart { .. } | DspEvent::ModuleStartFailed { .. }) => {
                let mut effects = bridges.capture.on_reply(&event)?;
                if let DspEvent::ModelStart {
                    core,
                    slot,
                    started: false,
                }
                | DspEvent::ModuleStartFailed { core, slot } = event
                {
                    self.health.start_failures += 1;
                    effects.push(Effect::Notice(
                        Subsystem::Dsp,
                        format!("model on core {core} slot {slot} did not start"),
                    ));
                }
                effects
            }
            DspEvent::MidiReceived { .. } | DspEvent::MidiClockTempo(_) | DspEvent::Flag0f
                if midi_held =>
            {
                Vec::new()
            }
            event @ (DspEvent::MidiReceived { .. }
            | DspEvent::MidiClockTempo(_)
            | DspEvent::Flag0f) => bridges.midi.on_dsp(&event, state, now)?,
            DspEvent::BulkMeter(bulk) => {
                if meters::on_bulk(&mut state.io_meters, &bulk)
                    | meters::on_lanes(state, &bulk, now)
                {
                    self.present |= meters_need_frame(&bridges.ui, state, &mut self.shown_clips);
                }
                Vec::new()
            }
            event @ (DspEvent::BufferAllocated(_) | DspEvent::Meter(_)) => {
                // Capture also reads IN 1/IN 2 (`0x4000`, `0x4001`), so both consume it.
                // A block editor draws its model meters (Parallax GAIN REDUCTION).
                if let DspEvent::Meter(record) = &event {
                    let before = state.row_meters;
                    if meters::on_row_meter(&mut state.row_meters, record) {
                        // The Grid draws the row meters as Input Gate meters, in whole px.
                        let specs = state.specs.as_ref();
                        let moved = (meters::gate_meters(state).into_iter().zip(before)).any(
                            |(shown, db)| {
                                shown.is_some_and(|shown| shown != meters::gate_meter(db, specs))
                            },
                        );
                        self.present |= moved && state.screen == tetrad_ui::Screen::Grid;
                    } else if meters::on_model_meter(state, record) && state.editor_block.is_some()
                    {
                        self.present = true;
                    }
                }
                if let DspEvent::Meter(record) = &event
                    && meters::on_meter(&mut state.io_meters, record)
                {
                    meters::grid_input_clips(state, record, now);
                    self.present |= meters_need_frame(&bridges.ui, state, &mut self.shown_clips);
                }
                let effects = bridges.capture.on_reply(&event)?;
                self.sync_capture_timer();
                effects
            }
            DspEvent::Diagnostics { core, block } => {
                state.cpu.record(core, &block);
                state.diagnostics.record(core, &block);
                // The last block per core also feeds Control Diagnostics (7) (`0x6dfe28`).
                bridges.control.on_dsp_diagnostics(core, &block);
                // Each core reports once a second: a frame only when the view draws the load.
                self.present |= bridges.ui.shows_dsp_load();
                Vec::new()
            }
            DspEvent::Soc2ArmDiagnostics(diagnostics) => {
                state.diagnostics.soc2 = Some(diagnostics);
                bridges
                    .control
                    .on_soc2_diagnostics(&diagnostics, state, now)?
            }
            DspEvent::UsbMeterUpdate(meter) => {
                bridges.services.on_usb_meter_update(meter);
                Vec::new()
            }
            DspEvent::TunerCalibrate(reply) => {
                bridges.services.on_tuner_calibrate(reply);
                Vec::new()
            }
            DspEvent::UsbMeterOutput(reply) => bridges.services.on_usb_meter_output(reply),
            DspEvent::UsbMeterInput { first, second } => bridges
                .control
                .complete_key_exchange(&first, &second, state, now)?,
            // Licence channel `0x21`; `KeyMaterial` zeroes the reply when it drops here.
            DspEvent::MidiOutTimer(reply) => {
                bridges
                    .services
                    .on_midi_out_timer(reply.as_bytes(), &mut state.settings, now)
            }
            // Licence channel `0x20`, the answer to `0x51`; zeroed on drop as above.
            DspEvent::TunerTimer(reply) => {
                let before = state.licences;
                let (added, mut effects) = bridges.services.on_tuner_timer(
                    reply.as_bytes(),
                    &mut state.settings,
                    &mut state.licences,
                    now,
                );
                if let Some(dsp) = self.dsp.as_mut() {
                    dsp.set_licences(state.licences);
                }
                effects.extend(licence_repush(state, &before, &added));
                effects
            }
            // Plain text from the DSP firmware; logged so runs show it (stock handling not
            // traced).
            DspEvent::DebugText {
                soc,
                core_selector,
                text,
            } => vec![Effect::Notice(
                Subsystem::Dsp,
                format!(
                    "{soc:?} core {core_selector} text {:?}",
                    String::from_utf8_lossy(text.text())
                ),
            )],
            // The kind only: the licence channels are routed above; key vectors carry secrets.
            event => {
                if state.unrouted.insert(std::mem::discriminant(&event)) {
                    vec![Effect::Notice(
                        Subsystem::Dsp,
                        format!("no consumer yet for DSP response {}", event.kind()),
                    )]
                } else {
                    Vec::new()
                }
            }
        };
        Ok(effects)
    }

    fn on_timer(&mut self, timer: Timer) -> Result<Vec<Effect>, DeviceError> {
        let now = self.clock.now();
        Ok(match timer {
            Timer::App => {
                let exited = self.app.as_mut().and_then(|app| app.poll(now));
                match (exited, self.app.take()) {
                    (Some(how), Some(app)) => {
                        self.app_touch = app.touching();
                        // The program drew over every row: the next frame is whole.
                        self.platform.forget_display();
                        self.bridges.ui.unshown();
                        self.present = true;
                        vec![Effect::Notice(
                            Subsystem::Extensions,
                            format!("{} ended: {how}", app.name),
                        )]
                    }
                    (None, Some(app)) => {
                        self.app = Some(app);
                        self.start_timer(Timer::App, app::POLL);
                        Vec::new()
                    }
                    (_, None) => Vec::new(),
                }
            }
            Timer::DismissSplash => {
                self.state.splash_dismissed = true;
                self.start_timer(Timer::SplashGone, SPLASH_POLL);
                vec![Effect::Splash(SplashCommand::DISMISS)]
            }
            Timer::SplashGone => {
                let gone = !self.platform.splash_running();
                self.splash_polls += 1;
                if (gone && self.splash_gone) || self.splash_polls >= SPLASH_POLLS {
                    self.platform.forget_display();
                    self.bridges.ui.unshown();
                    self.present = true;
                } else {
                    self.splash_gone = gone;
                    self.start_timer(Timer::SplashGone, SPLASH_POLL);
                }
                Vec::new()
            }
            Timer::SettingsFlush => {
                self.flush_settings()?;
                Vec::new()
            }
            Timer::ControlTick => {
                self.start_timer(Timer::ControlTick, CONTROL_TICK);
                self.bridges.control.tick(&mut self.state, now)?
            }
            Timer::MidiClock => self.bridges.midi.poll(&mut self.state, now)?,
            Timer::Services if !self.services_up => self.start_services()?,
            Timer::Services => {
                self.start_timer(Timer::Services, services::TICK);
                self.bridges.services.tick(
                    self.platform.services(),
                    &mut self.state.settings,
                    now,
                )?
            }
            // A running program has the footswitches: a press that waited does not run in it.
            Timer::CustomGesture if self.app.is_some() => {
                self.state.custom.drop_waiting();
                Vec::new()
            }
            Timer::CustomGesture => custom::expire(&mut self.state),
            Timer::FootswitchHold => {
                let effects = self.bridges.input.on_hold_expired(&mut self.state, now);
                self.sync_hold_timer();
                effects
            }
            Timer::InputScheduler => self.bridges.jacks.on_timer(),
            Timer::ConnectorsSave => jacks::save(&mut self.state),
            Timer::IoPresetSettle => jacks::settle_io_loads(&mut self.state),
            Timer::OverlayBlink => vec![Effect::Present],
            Timer::VolumeRepeat => self.bridges.input.on_volume_timer(),
            Timer::PowerLongPress => self.bridges.input.on_power_timer(),
            Timer::UiTick => {
                let period = if self.bridges.ui.animating() {
                    ANIMATION_TICK
                } else {
                    UI_TICK
                };
                self.start_timer(Timer::UiTick, period);
                let now_ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
                self.on_ui(ui::UiInput::View(tetrad_ui::InputEvent::Tick { now_ms }))
            }
            Timer::IoMeters
                if matches!(self.state.screen, tetrad_ui::Screen::Io(_))
                    || self.bridges.control.io_meters_subscribed() =>
            {
                let mut effects: Vec<Effect> = meters::requests().collect();
                effects.push(Effect::StartTimer {
                    timer: Timer::IoMeters,
                    after: meters::POLL,
                });
                effects
            }
            Timer::IoMeters => Vec::new(),
            Timer::EditorMeters => {
                let (Some(slot), Some(current)) = (self.state.editor_block, &*self.state.current)
                else {
                    return Ok(Vec::new());
                };
                let mut effects = meters::editor_requests(current, slot);
                effects.push(Effect::StartTimer {
                    timer: Timer::EditorMeters,
                    after: meters::POLL,
                });
                effects
            }
            Timer::GridMeters if self.state.screen == tetrad_ui::Screen::Grid => {
                let lanes = (self.state.current.as_ref())
                    .map(|current| current.deployment.layout().lanes.as_slice());
                let mut effects: Vec<Effect> =
                    lanes.into_iter().flat_map(meters::grid_requests).collect();
                // The Input Gate meters subscribe to the row meters (`0x5a36d0`); Control's
                // row meter poll serves them while it runs.
                let gates = meters::gate_meters(&self.state).iter().any(Option::is_some);
                if gates && !self.bridges.control.grid_meters_subscribed() {
                    let gate_core = (self.state.current.as_ref())
                        .filter(|_| !self.bridges.capture.capture_running())
                        .and_then(|current| current.deployment.plan().gate_core);
                    effects.extend(meters::row_requests(gate_core));
                }
                effects.push(Effect::StartTimer {
                    timer: Timer::GridMeters,
                    after: meters::POLL,
                });
                effects
            }
            Timer::GridMeters => Vec::new(),
            Timer::RowMeters if self.bridges.control.grid_meters_subscribed() => {
                let current = (self.state.current.as_ref())
                    .filter(|_| !self.bridges.capture.capture_running());
                let gate_core = current.and_then(|current| current.deployment.plan().gate_core);
                let mut effects: Vec<Effect> = meters::row_requests(gate_core).collect();
                // The model meter service polls at the same period (`0x4a9844`, 100 ms).
                let control = &self.bridges.control;
                effects.extend(current.into_iter().flat_map(|current| {
                    meters::model_requests(current, |slot| control.model_meter_subscribed(slot))
                }));
                effects.push(Effect::StartTimer {
                    timer: Timer::RowMeters,
                    after: meters::POLL,
                });
                effects
            }
            // DELETE All cleared the subscriptions and the model meter maps (`0x110a70`).
            Timer::RowMeters => {
                self.state.model_meters.clear();
                Vec::new()
            }
            Timer::CaptureMeters => {
                let effects = self.bridges.capture.on_meter_timer();
                self.sync_capture_timer();
                effects?
            }
            Timer::Nc2Poll => self
                .bridges
                .capture
                .nc2(tetrad_capture::v2::Nc2Input::PollDue)?,
            Timer::Diagnostics => {
                self.start_timer(Timer::Diagnostics, DIAGNOSTICS_TICK);
                self.diagnostics()?
            }
            Timer::DspDriver => {
                if let Some(dsp) = self.dsp.as_mut() {
                    dsp.run_timers()?;
                }
                Vec::new()
            }
            Timer::TunerMeterPoll => {
                if features::tuner_polls(&self.state) {
                    vec![
                        Effect::Dsp(DspCommand::TunerLooper(
                            TunerLooperRequest::TunerFrequencyRequest,
                        )),
                        Effect::StartTimer {
                            timer: Timer::TunerMeterPoll,
                            after: features::TUNER_POLL,
                        },
                    ]
                } else {
                    Vec::new()
                }
            }
            Timer::TunerMeterIdle => actions::reduce(Action::TunerReading(None), &mut self.state)?,
            Timer::MetronomeFlush => actions::reduce(Action::MetronomeFlush, &mut self.state)?,
            Timer::ExpressionBypass => actions::reduce(Action::ExpressionPoll, &mut self.state)?,
            Timer::Bluetooth => {
                self.restart_bluetooth();
                Vec::new()
            }
            Timer::LooperPoll => {
                let disabled = self
                    .platform
                    .marker(Marker::NoPoll)
                    .map_err(DeviceError::platform)?;
                if let Some(dsp) = self.dsp.as_mut() {
                    dsp.poll_loopers(disabled)?;
                }
                vec![Effect::StartTimer {
                    timer: Timer::LooperPoll,
                    after: tetrad_dsp_host::setup::drivers::looper::STATUS_PERIOD,
                }]
            }
        })
    }

    /// The Wi-Fi bring-up. On failure the services timer retries it every
    /// [`SERVICES_RETRY`], so the internet probe and NTP still start later.
    fn start_services(&mut self) -> Result<Vec<Effect>, services::ServiceError> {
        let started = self
            .bridges
            .services
            .start(self.platform.services(), self.clock.now());
        self.services_up = started.is_ok();
        if !self.services_up {
            self.start_timer(Timer::Services, SERVICES_RETRY);
        }
        started
    }

    /// `0x50ea74`: unless `/opt/neuraldsp/nopoll` exists (tested on every tick), `0x0a` then
    /// `0x1a` to protocol cores 0..=4, then one more `0x1a` to core 4; wait 0, timeout 100.
    /// Dead cores are skipped quietly, as stock `sendMessage` refuses them; cores 2..=4 are
    /// polled only while SOC2 comms are open (owned).
    fn diagnostics(&mut self) -> Result<Vec<Effect>, DeviceError> {
        if self
            .platform
            .marker(Marker::NoPoll)
            .map_err(DeviceError::platform)?
        {
            return Ok(Vec::new());
        }
        let Some(dsp) = self.dsp.as_mut() else {
            return Ok(Vec::new());
        };
        let last = if self.state.soc2 == Some(dsp::Soc2Status::Open) {
            4
        } else {
            1
        };
        let requests = (0..=last)
            .filter_map(|wire| CoreId::try_from(wire).ok())
            .flat_map(|core| {
                [
                    Request::DiagnosticsRequest { core },
                    Request::DebugCommsRequest { core },
                ]
            })
            .chain((last == 4).then_some(Request::DebugCommsRequest {
                core: CoreId::Control,
            }));
        for request in requests {
            match dsp.request(request) {
                Ok(()) | Err(dsp::DspError::Session(SessionError::DeadCore(_))) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Vec::new())
    }

    /// One UI input: tetrad-ui requests become effects, actions and bridge handoffs (w2b-ui M2,
    /// M4). A handoff that fails becomes a notice; the other requests still run.
    fn on_ui(&mut self, input: ui::UiInput) -> Vec<Effect> {
        let now = self.clock.now();
        let mut effects = Vec::new();
        for request in self.bridges.ui.handle(input, &self.state, now) {
            match request {
                ui::Request::Effect(effect) => effects.push(effect),
                ui::Request::Action(action) => effects.push(Effect::Action(action)),
                ui::Request::Handoff(handoff) => match self.handoff(handoff, now) {
                    Ok(routed) => effects.extend(routed),
                    Err(error) => {
                        effects.push(Effect::Notice(error.subsystem(), error.to_string()));
                    }
                },
            }
        }
        // A flick that just started brings the next tick forward.
        if self.bridges.ui.animating() {
            let soon = now + ANIMATION_TICK;
            if self.timers.get(&Timer::UiTick).is_none_or(|&at| at > soon) {
                self.timers.insert(Timer::UiTick, soon);
            }
        }
        effects
    }

    fn handoff(&mut self, handoff: ui::Handoff, now: Duration) -> Result<Vec<Effect>, DeviceError> {
        if let Some(refused) = self.options.handoff_refusal(&handoff) {
            return Err(DeviceError::Unavailable(refused));
        }
        let effects = match handoff {
            ui::Handoff::Extensions(command) => {
                self.extensions(|extensions, backend, _| extensions.on_ui(command, backend))
            }
            ui::Handoff::Mappings(command) => self.mapping_command(command),
            ui::Handoff::Custom(command) => self.custom_command(command),
            ui::Handoff::Theme(id) => self.set_theme(id),
            ui::Handoff::Remote {
                enabled,
                pin_in_menu,
            } => {
                self.set_remote(enabled, pin_in_menu);
                Vec::new()
            }
            ui::Handoff::RemoteBackup { restore } => self.remote_backup_touch(restore),
            ui::Handoff::BootSelector { shown } => {
                self.set_boot_selector(shown);
                Vec::new()
            }
            ui::Handoff::Bluetooth {
                enabled,
                host_mode,
                midi,
            } => {
                self.set_bluetooth(enabled, host_mode, midi);
                Vec::new()
            }
            ui::Handoff::ForgetBluetooth { address } => {
                self.forget_bluetooth(&address);
                Vec::new()
            }
            ui::Handoff::Capture(command) => self.capture_ui(&command)?,
            // r16-capture MR 2: through `nc2_input`, so saves and the meter timer sync run.
            ui::Handoff::Nc2DialogShown => {
                self.nc2_input(tetrad_capture::v2::Nc2Input::DialogShown)?;
                Vec::new()
            }
            ui::Handoff::Nc2(input) => {
                self.nc2_input(input)?;
                Vec::new()
            }
            ui::Handoff::Services(tetrad_ui::UiCommand::Upload { items }) => {
                self.upload(items, now)?
            }
            ui::Handoff::Services(command) => self.bridges.services.on_ui(
                self.platform.services(),
                command,
                &mut self.state.settings,
                now,
            )?,
            ui::Handoff::Input(command) => self.bridges.input.on_ui(&command, &mut self.state)?,
            // Review r32 E6: a failed delete stops before the rename, so two user IRs never
            // share the name.
            ui::Handoff::OverwriteIr { id, name, other } => {
                let kind = tetrad_ui::MediaKind::Ir;
                self.edit_directory(tetrad_ui::DirectoryRequest::DeleteMedia { kind, id: other })?;
                self.edit_directory(tetrad_ui::DirectoryRequest::RenameMedia { kind, id, name })?;
                Vec::new()
            }
            ui::Handoff::DownloadPresets {
                ids,
                setlist,
                to,
                overwrite,
            } => {
                let slots = self.download_slots(&setlist, ids.len(), to)?;
                let items = ids.into_iter().zip(slots).collect();
                // Both check every target (PASTE rules) before the first request; OVERWRITE
                // also allows an occupied slot, never the loaded preset's (r33-ui MR A).
                if overwrite {
                    self.overwrite_presets(&setlist, items, now)?
                } else {
                    self.download_presets(&setlist, items, now)?
                }
            }
        };
        self.sync_capture_timer();
        Ok(effects)
    }

    /// A Directory library edit ([`media::edit_directory`]), its Cortex Control file echo,
    /// then the Directory media refresh and the settings flush.
    fn edit_directory(&mut self, request: tetrad_ui::DirectoryRequest) -> Result<(), DeviceError> {
        let edit = self.bridges.control.file_edit(&request, &self.state);
        let mut effects =
            media::edit_directory(request, &mut self.state, self.media.as_mut(), &self.options)?;
        effects.extend(self.bridges.control.file_edited(edit, &self.state));
        self.apply(Ok::<_, DeviceError>(effects));
        self.refresh_directory_media();
        self.schedule_settings_flush();
        Ok(())
    }

    /// The target slots of `count` cloud presets in `setlist` (`0x795b18`): the chosen slots,
    /// or the first empty ones. Too few empty slots refuses the whole download (O, as PASTE
    /// refuses a pair before any write).
    fn download_slots(
        &self,
        setlist: &str,
        count: usize,
        to: tetrad_ui::PasteTarget,
    ) -> Result<Vec<usize>, DeviceError> {
        let slots: Vec<usize> = match to {
            tetrad_ui::PasteTarget::Slots(slots) => (slots.into_iter())
                .filter_map(|slot| usize::try_from(slot).ok())
                .collect(),
            tetrad_ui::PasteTarget::FirstEmpty => {
                let library = (self.state.library.as_ref())
                    .ok_or(DeviceError::Unavailable("the preset library"))?;
                (library.presets.slots(setlist)?.iter().enumerate())
                    .filter(|(_, slot)| slot.is_none())
                    .map(|(index, _)| index)
                    .take(count)
                    .collect()
            }
        };
        if slots.len() != count {
            return Err(DeviceError::Unavailable("a target slot for every preset"));
        }
        Ok(slots)
    }

    /// Executes a reducer result; an error becomes a notice.
    fn apply(&mut self, effects: Result<Vec<Effect>, impl Into<DeviceError>>) {
        match effects {
            Ok(effects) => effects.into_iter().for_each(|effect| self.execute(effect)),
            Err(error) => {
                let error = error.into();
                self.health.record(&error);
                self.notice(error.subsystem(), error.to_string());
            }
        }
    }

    /// A Control cloud backup that cannot run ends at once: Control waits for its end with
    /// the link suspended, up to 30 min for a CREATE (review r23 K7).
    fn control_cloud_refused(&mut self, reason: String) {
        let failed = control::CloudBackupDone::Failed(reason);
        self.execute(Effect::CloudBackupDone(failed));
    }

    /// [`Self::run_effect`] as one probe unit, named by its variant ([`probe::Label::of`]).
    fn execute(&mut self, effect: Effect) {
        let label = probe::Label::of(&effect);
        self.probe.enter();
        self.run_effect(effect);
        self.probe.leave(label);
    }

    fn run_effect(&mut self, effect: Effect) {
        if let Some(refused) = self.options.refusal(&effect) {
            self.notice(Subsystem::Platform, format!("bring-up refused {refused}"));
            if let Some(input) = bringup::phantom_input(&effect) {
                let clears = jacks::refuse_phantom(&mut self.state, input);
                clears.into_iter().for_each(|clear| self.execute(clear));
            }
            // A refused job ends as a lost one: a cloud job brings its session home and an
            // install gives the inputs back (review r14 S-5).
            match effect {
                Effect::Service(job) => self.queue.push_back(Event::Service(job.lost())),
                Effect::ControlAccount(control::AccountRequest::CloudBackup(_)) => {
                    self.control_cloud_refused(format!("bring-up refused {refused}"));
                }
                _ => {}
            }
            return;
        }
        let result = match effect {
            Effect::Directory(request) => self.edit_directory(request),
            Effect::RefreshDirectory => {
                self.refresh_directory_media();
                Ok(())
            }
            Effect::Dsp(command) => self.dsp_command(command),
            Effect::Deploy(update) => self.deploy(&update),
            Effect::DeployLayout(change) => self.deploy_layout(change),
            Effect::RestoreLayout { change, redo } => self.restore_layout(change, redo),
            Effect::DeployGate(update) => self.deploy_gate(&update),
            Effect::Hal(command) => self
                .platform
                .hal()
                .apply(command)
                .map_err(DeviceError::from),
            Effect::MidiOut(message) => self
                .platform
                .midi_write(&message)
                .map_err(DeviceError::platform),
            Effect::BluetoothMidi(message) => {
                self.bluetooth_midi(message);
                Ok(())
            }
            Effect::PresetMidiOut(trigger) => {
                let effects = self.bridges.midi.trigger(trigger, &self.state);
                self.apply(effects);
                Ok(())
            }
            Effect::MidiFootswitch(letter) => {
                let now = self.clock.now();
                self.state.footswitch_dialog = self.bridges.ui.view().assign_mode();
                let effects = self
                    .bridges
                    .input
                    .midi_footswitch(letter, &mut self.state, now);
                self.sync_hold_timer();
                self.apply(effects);
                Ok(())
            }
            Effect::ControlReport(report) => {
                self.bridges.control.queue_report(report);
                Ok(())
            }
            Effect::Splash(command) => self
                .platform
                .splash()
                .send(command)
                .map_err(|error| DeviceError::Platform(error.to_string().into())),
            Effect::Present => {
                self.present = true;
                Ok(())
            }
            Effect::StartTimer { timer, after } => {
                self.start_timer(timer, after);
                Ok(())
            }
            Effect::StopTimer(timer) => {
                self.timers.remove(&timer);
                Ok(())
            }
            Effect::Sleep(duration) => {
                self.clock.clone().sleep(duration);
                Ok(())
            }
            Effect::ScheduleInput(change) => {
                let effects = self.bridges.jacks.enqueue(change, self.platform.hal());
                self.apply(effects);
                Ok(())
            }
            Effect::LoadPreset(preset) => self.load_preset(&preset, true),
            Effect::ProgramChange(preset) => self.load_preset(&preset, false),
            Effect::StepPresetLoad { redo } => self.step_preset_load(redo),
            Effect::LoadTemplate => self.load_template(),
            Effect::RestoreSelected {
                redo,
                working,
                owner,
            } => self.restore_selected(redo, working.map(|working| *working), owner),
            // An audition is a `0x5b2f40` load too (I), with the load's pushes.
            Effect::AuditionPreset(path) => {
                self.load_preset_path(Some(path), None, true).map(|()| {
                    let reason = tetrad_control::codecs::RecallPresetReason::Other;
                    let pushes = control::preset_pushes(&self.state, reason);
                    pushes.into_iter().for_each(|push| self.execute(push));
                })
            }
            Effect::SavePreset(name) => self.save_preset(name, None, Default::default()),
            Effect::EditPresetDetails { name, instrument } => {
                self.save_preset(Some(name), None, instrument)
            }
            Effect::SavePresetAs {
                name,
                setlist,
                instrument,
            } => {
                let filed = (self.state.current.as_ref()).is_some_and(|c| c.path.is_some());
                self.save_preset(Some(name), filed.then_some((setlist, None)), instrument)
            }
            Effect::SavePresetInto {
                name,
                setlist,
                slot,
                instrument,
            } => self.save_preset(Some(name), Some((setlist, Some(slot))), instrument),
            Effect::Action(action) => {
                let change = extensions::native_change(&action, &self.state);
                self.bridges.control.record_drop(&action, &self.state);
                let push = control::action_push(&action);
                let effects = actions::reduce(action, &mut self.state)
                    .map(|effects| effects.into_iter().chain(push).collect::<Vec<_>>());
                let applied = effects.is_ok();
                self.apply(effects);
                if let Some(change) = change.filter(|_| applied) {
                    let effects = self.extensions(|extensions, backend, _| {
                        extensions.control_changed(change, backend)
                    });
                    self.apply(Ok::<_, DeviceError>(effects));
                }
                Ok(())
            }
            Effect::ExtensionAction(action) => {
                self.bridges.control.record_drop(&action, &self.state);
                let push = control::action_push(&action);
                let effects = actions::reduce(action, &mut self.state)
                    .map(|effects| effects.into_iter().chain(push).collect::<Vec<_>>());
                self.apply(effects);
                Ok(())
            }
            Effect::ExtensionMidi(message) => {
                let effects =
                    self.extensions(|extensions, backend, _| extensions.on_midi(&message, backend));
                self.apply(Ok::<_, DeviceError>(effects));
                Ok(())
            }
            Effect::MappingMidi(message) => {
                let step = (self.bridges.mappings).on_midi(&message, &self.state, self.clock.now());
                let effects = self.mapping_step(step);
                self.apply(Ok::<_, DeviceError>(effects));
                Ok(())
            }
            Effect::MappingStomp { stomp, pressed } => {
                let effects =
                    (self.bridges.mappings).on_stomp(stomp, pressed, &self.state, self.clock.now());
                self.apply(Ok::<_, DeviceError>(effects));
                Ok(())
            }
            Effect::ExtensionMidiOut(message) => {
                let effects = self.bridges.midi.transmit(&message, &self.state);
                self.apply(effects);
                Ok(())
            }
            Effect::ExtensionCommand { key, command } => {
                let effects = self.extensions(|extensions, backend, _| {
                    extensions.run_command(&key, &command, backend)
                });
                self.apply(Ok::<_, DeviceError>(effects));
                Ok(())
            }
            Effect::LaunchApp(launch) => {
                self.launch_app(&launch);
                Ok(())
            }
            Effect::CustomModeChanged => {
                let bytes =
                    (self.state.custom).active_file(self.state.performance.footswitch.custom);
                self.write_file(custom::ACTIVE_FILE, &bytes, "Footswitch mode not saved");
                Ok(())
            }
            Effect::UiInput(input) => {
                self.queue.push_back(Event::UiInput(input));
                Ok(())
            }
            Effect::SettingsChanged(SettingsEvent::Midi(_)) => {
                let effects = self.bridges.midi.settings_changed(&self.state);
                self.apply(effects);
                let native = self.bridges.midi.native_channel();
                let effects = self.extensions(|extensions, backend, _| {
                    extensions.native_channel_changed(native, backend)
                });
                self.apply(Ok::<_, DeviceError>(effects));
                Ok(())
            }
            // A calibration confirm or a reset: the bridge keeps a copy of the ranges.
            Effect::SettingsChanged(SettingsEvent::Expression(saved)) => {
                self.bridges.input.calibration_changed(&saved);
                Ok(())
            }
            // Other groups are read from `State` where they are used. The tuner path and the
            // tempo also live in the DSP: their writers (`ChangeSetting`, `Reset`) send them.
            Effect::SettingsChanged(_) => Ok(()),
            Effect::ResetSteps(kind) => {
                self.reset_steps(kind);
                Ok(())
            }
            // A restore extract holds it until the job ends (review r19 F3).
            Effect::Power(action) => {
                if let Some(action) = self.power_request(action) {
                    self.queue.push_front(Event::Shutdown(action));
                }
                Ok(())
            }
            Effect::Notice(subsystem, detail) => {
                self.notice(subsystem, detail);
                Ok(())
            }
            Effect::Service(job) => {
                let job = self.prepare_service_job(job);
                if let Some(notice) = self.platform.services().run_job(job) {
                    self.queue.push_back(Event::Service(notice));
                }
                Ok(())
            }
            Effect::ControlBackup(request) => {
                let now = self.clock.now();
                let effects = (self.bridges.services)
                    .control_backup(self.platform.services(), request, now)
                    .or_else(|error| {
                        let failed = services::ControlBackupDone::Failed(error.to_string());
                        Ok::<_, DeviceError>(vec![Effect::ControlBackupDone(failed)])
                    });
                self.apply(effects);
                Ok(())
            }
            Effect::ControlBackupDone(done) => {
                let effects =
                    (self.bridges.control).backup_done(done, &mut self.state, self.clock.now());
                self.apply(effects);
                Ok(())
            }
            Effect::RemoteBackupDone(done) => {
                self.remote_backup_done(done);
                Ok(())
            }
            Effect::CloudBackupDone(done) => {
                let effects = self.bridges.control.cloud_backup_done(
                    done,
                    self.bridges.services.backups(),
                    &mut self.state,
                    self.clock.now(),
                );
                self.apply(effects);
                Ok(())
            }
            Effect::LibraryReopen => {
                self.reopen_library();
                // `0x3fcf64` → `0x3fd3e8`: the reload clears every undo frame (C).
                self.state.history.clear_all();
                Ok(())
            }
            Effect::RestoreReload => {
                self.reload_after_restore();
                Ok(())
            }
            Effect::UpdaterReport(message) => {
                let effects = self.bridges.control.notify(
                    tetrad_control::link::Notification::Update(
                        tetrad_control::codecs::ControlMessage::Updater(message),
                    ),
                    &mut self.state,
                    self.clock.now(),
                );
                self.apply(effects);
                Ok(())
            }
            Effect::GridEcho(_) if self.bridges.capture.capture_running() => Ok(()),
            Effect::ControlNotify(message) | Effect::GridEcho(message) => {
                let effects = self.bridges.control.notify(
                    tetrad_control::link::Notification::Update(*message),
                    &mut self.state,
                    self.clock.now(),
                );
                self.apply(effects);
                Ok(())
            }
            Effect::ControlNotification(notification) => {
                let effects =
                    self.bridges
                        .control
                        .notify(*notification, &mut self.state, self.clock.now());
                self.apply(effects);
                Ok(())
            }
            Effect::RecallPreset(preset) => self.recall_preset(*preset),
            Effect::ControlRecall { prepared, undo } => self.recall_prepared(*prepared, Some(undo)),
            Effect::ControlFrame => match self.bridges.ui.render(&self.state) {
                Ok(rgb) => {
                    let now = self.clock.now();
                    let effects = (self.bridges.control).send_frame(rgb, &mut self.state, now);
                    self.apply(effects);
                    Ok(())
                }
                Err(error) => Err(error.into()),
            },
            Effect::ControlRequestDone => {
                let capturing = self.bridges.capture.capture_running();
                let effects =
                    self.bridges
                        .control
                        .request_done(&mut self.state, capturing, self.clock.now());
                self.apply(effects);
                Ok(())
            }
            // CloudProduct (46) UPLOAD of one setlist preset: the Directory upload
            // (r32-control MR 1, `0xf3060` → `0x1d81d0`).
            Effect::ControlAccount(control::AccountRequest::CloudProductUpload { path }) => {
                let now = self.clock.now();
                let effects = self.control_upload(vec![tetrad_ui::UploadItem::Preset(path)], now);
                self.apply(effects);
                Ok(())
            }
            // CloudProduct (46) UPLOAD type 2 of one user capture (r36-upload MR 3).
            Effect::ControlAccount(control::AccountRequest::CloudCaptureUpload { key }) => {
                let now = self.clock.now();
                let effects = self.control_upload(vec![tetrad_ui::UploadItem::Capture(key)], now);
                self.apply(effects);
                Ok(())
            }
            // CloudProduct (46) UPLOAD of more keys: the batch (`0x785dcc`, fromControl).
            Effect::ControlAccount(control::AccountRequest::CloudBatchUpload { items }) => {
                let now = self.clock.now();
                let effects = self.control_upload(items, now);
                self.apply(effects);
                Ok(())
            }
            // CloudProduct (46) preset DOWNLOAD into setlist slots (r32-control MR 1,
            // `0x1dd61c`; more keys r47-control, `0x786ac4`).
            Effect::ControlAccount(control::AccountRequest::CloudPresetDownload {
                setlist,
                items,
            }) => {
                let now = self.clock.now();
                let effects = self.download_presets(&setlist, items, now);
                self.apply(effects);
                Ok(())
            }
            Effect::ControlAccount(request) => {
                let (services, now) = (self.platform.services(), self.clock.now());
                let settings = &mut self.state.settings;
                let effects = match request {
                    control::AccountRequest::RefreshLicences => self
                        .bridges
                        .services
                        .refresh_licences(services, settings, now),
                    // Control logout (0x003080b4): unpair, then the local logout only.
                    control::AccountRequest::SignOut => self.bridges.services.unpair(
                        services,
                        settings,
                        services::AfterUnpair::LocalLogout,
                        now,
                    ),
                    control::AccountRequest::Login(login) => match login.take() {
                        Some(login) => (self.bridges.services)
                            .login_from_control(services, settings, login, now),
                        None => Ok(Vec::new()),
                    },
                    control::AccountRequest::SetClock(ms) => Ok(self
                        .bridges
                        .services
                        .set_clock_if_unset(ms)
                        .into_iter()
                        .collect()),
                    control::AccountRequest::RestartDownloadViaControl => {
                        let transport =
                            services::ControlTransport::new(self.bridges.control.forward_link());
                        (self.bridges.services)
                            .restart_download_over_control(services, settings, transport, now)
                    }
                    // Updater (60): the device updater page's command, same gates.
                    control::AccountRequest::Updater(command) => self.bridges.services.on_ui(
                        services,
                        tetrad_ui::UiCommand::Updater { command },
                        settings,
                        now,
                    ),
                    // r23-control MR 1: services keeps the origin and answers at the flow's end.
                    control::AccountRequest::CloudBackup(request) => Ok(self
                        .bridges
                        .services
                        .control_cloud_backup(services, settings, request, now)),
                    // ProcessDownloadsQueue (45): the downloads-queue client (r26-media MR 1).
                    control::AccountRequest::DownloadsQueue(kind) => (self.bridges.services)
                        .process_downloads_queue(services, settings, kind, now),
                    // CloudProduct (46) DOWNLOAD of one capture or IR key (r30-services §4,
                    // r47-control): into its saving destination.
                    control::AccountRequest::CloudProductDownload { kind, id } => {
                        (self.bridges.services).control_download(services, settings, kind, id, now)
                    }
                    // More keys: one batch (r47-control, `0x786264`; r47-services).
                    control::AccountRequest::CloudProductDownloads { kind, ids } => {
                        (self.bridges.services)
                            .control_downloads(services, settings, kind, ids, now)
                    }
                    // CloudProduct (46) DELETE (r32-control MR 1, `0xf330c`; more ids one run).
                    control::AccountRequest::CloudProductDelete { kind, ids } => {
                        (self.bridges.services).control_delete(services, settings, kind, ids, now)
                    }
                    // Run by the arms before this one (they need the device).
                    control::AccountRequest::CloudPresetDownload { .. }
                    | control::AccountRequest::CloudProductUpload { .. }
                    | control::AccountRequest::CloudCaptureUpload { .. }
                    | control::AccountRequest::CloudBatchUpload { .. } => Ok(Vec::new()),
                    // Logs (44): the support report upload (`0x182868`, `0x6bcb30`).
                    control::AccountRequest::UploadLogs => self.bridges.services.on_ui(
                        services,
                        tetrad_ui::UiCommand::Device {
                            action: tetrad_ui::DeviceAction::SendReport,
                        },
                        settings,
                        now,
                    ),
                    // re-r38 §3: the Cortex Control answer to a Control-started upload
                    // question (Confirmation 47): OVERWRITE or SKIP / CANCEL of that question
                    // only (review r39 K3). As after a device answer (`Services::on_ui`), a
                    // batch item it ended lets the next one start; `batch_next` starts none
                    // while one runs.
                    control::AccountRequest::UploadAnswer {
                        question,
                        overwrite,
                    } => {
                        let answered = (self.bridges.services)
                            .answer_upload(services, settings, question, true, overwrite, now);
                        let answered = answered.unwrap_or_else(|error| {
                            vec![Effect::Notice(Subsystem::Services, error.to_string())]
                        });
                        Ok(answered.into_iter().chain([Effect::UploadNext]).collect())
                    }
                    // BulkOperation (57) DELETE (`0xed8d0`): the run stops before its next item.
                    control::AccountRequest::CancelBulkRun => {
                        self.bridges.services.cancel_bulk_run();
                        Ok(Vec::new())
                    }
                };
                self.apply(effects);
                Ok(())
            }
            Effect::MetronomeResendPlan(plan) => self
                .dsp
                .as_mut()
                .ok_or(DeviceError::Unavailable("the DSP"))
                .and_then(|dsp| dsp.send_metronome(&plan).map_err(DeviceError::from)),
            Effect::RecallScene(metronome) => self.recall_scene(metronome.as_ref()),
            Effect::CaptureEntry(entry) => self.capture_entry(*entry),
            Effect::Nc2(input) => self.nc2_input(input),
            Effect::CaptureRemote(remote) => self.capture_remote(remote),
            Effect::ExpressionCalibration { pedal, calibrating } => {
                let command = if calibrating {
                    tetrad_ui::UiCommand::Device {
                        action: tetrad_ui::DeviceAction::CalibrateExpression(pedal),
                    }
                } else {
                    tetrad_ui::UiCommand::ExpressionCalibration { pedal, save: true }
                };
                let effects = self.bridges.input.on_ui(&command, &mut self.state);
                self.apply(effects);
                Ok(())
            }
            Effect::Nc2Download(download) => self.nc2_downloaded(*download),
            Effect::StoreDownload(download) => {
                self.store_cloud_download(*download);
                Ok(())
            }
            Effect::PresetUploaded {
                path,
                name,
                cloud_id,
            } => self.preset_uploaded(&path, name, cloud_id),
            Effect::CaptureUploaded {
                hash,
                name,
                cloud_id,
            } => self.capture_uploaded(&hash, name, cloud_id),
            Effect::UploadNext => {
                let now = self.clock.now();
                let effects = self.upload_next(now);
                self.apply(effects);
                Ok(())
            }
            Effect::Cloud(request) => {
                let effects = self.bridges.services.cloud_request(
                    &mut self.state.settings,
                    request,
                    self.clock.now(),
                );
                self.apply(Ok::<_, DeviceError>(effects));
                Ok(())
            }
            Effect::PresetMidiSource(source) => {
                let now = self.clock.now();
                let effects = self
                    .bridges
                    .midi
                    .source_edited(source, &mut self.state, now);
                self.apply(effects);
                Ok(())
            }
            Effect::GigViewButton { tile, pressed } => {
                self.state.footswitch_dialog = self.bridges.ui.view().assign_mode();
                let masked = self.bridges.services.inputs_masked();
                let effects =
                    (self.bridges.input).gig_view_button(tile, pressed, &self.state, masked);
                self.apply(Ok::<_, DeviceError>(effects));
                Ok(())
            }
            Effect::ReadVolume => {
                let raw = if self.state.platform.is_atma() {
                    None
                } else {
                    match self.platform.hal().read(Query::Volume) {
                        Ok(Reading::Word(raw)) => Some(raw),
                        Ok(_) => None,
                        Err(error) => {
                            self.report::<()>(Err(error.into()));
                            None
                        }
                    }
                };
                let effect = input::read_volume(&mut self.state, raw);
                self.execute(effect);
                Ok(())
            }
            Effect::Backlight(percent) => {
                let level = DisplayPanel::backlight(self.platform.display_panel(), percent);
                self.platform
                    .hal()
                    .apply(HalCommand::Display(DisplayCommand::Brightness(level)))
                    .map_err(DeviceError::from)
            }
        };
        self.report(result);
        self.store_linked_edits();
    }

    fn dsp_command(&mut self, command: DspCommand) -> Result<(), DeviceError> {
        let dsp = self
            .dsp
            .as_mut()
            .ok_or(DeviceError::Unavailable("the DSP"))?;
        let pending = match command {
            DspCommand::Request(request) => {
                dsp.request(request)?;
                return Ok(());
            }
            // The install mute holds through later deploys until the install ends.
            DspCommand::MasterMute(value) => {
                dsp.set_master_mute(value)?;
                dsp.hold_mute(self.bridges.services.inputs_masked().then_some(value));
                return Ok(());
            }
            DspCommand::ReleaseFaultMute => {
                dsp.release_fault_mute()?;
                return Ok(());
            }
            DspCommand::InputTrims(trims) => {
                dsp.set_input_trims(trims)?;
                return Ok(());
            }
            // `0x875db0`: no `0x45` while `/opt/neuraldsp/nopoll` is a file.
            DspCommand::Control(ControlRequest::MetronomeStatusRequest)
                if self
                    .platform
                    .marker(Marker::NoPoll)
                    .map_err(DeviceError::platform)? =>
            {
                return Ok(());
            }
            DspCommand::Control(request) => {
                let frame = RequestFrame::new(request).map_err(dsp::DspError::from)?;
                dsp.send(ControlPacket::Controls(&frame), control_policy(&request))?;
                return Ok(());
            }
            DspCommand::TunerLooper(request) => {
                let frame = tetrad_dsp_protocol::tuner_looper::RequestFrame::new(request);
                let policy = match request {
                    TunerLooperRequest::GlobalTunerInputPath { .. }
                    | TunerLooperRequest::TunerEnable { .. }
                    | TunerLooperRequest::TunerBaseFrequency { .. } => SendPolicy::new(true, 100),
                    _ => SendPolicy::new(false, 100),
                };
                dsp.send(ControlPacket::TunerLooper(&frame), policy)?;
                return Ok(());
            }
            DspCommand::TunerMute | DspCommand::LanePush => {
                let (Some(specs), Some(current)) = (&self.state.specs, &*self.state.current) else {
                    return Err(DeviceError::Unavailable("a loaded preset"));
                };
                let context = context(&self.state, specs, &current.deployment, None);
                dsp.apply_tuner(&context, command == DspCommand::LanePush)?;
                return Ok(());
            }
            DspCommand::LooperParameter { site, index, value } => {
                let context = features::looper_context(&self.state);
                dsp.looper_parameter(site, index, value, &context)?;
                return Ok(());
            }
            DspCommand::Globals => dsp.apply_globals(self.state.settings.settings())?,
            DspCommand::Tempo(bpm) => {
                dsp.set_tempo(bpm)?;
                return Ok(());
            }
            DspCommand::Footswitch { stomp, pressed } => {
                if let Some(current) = &*self.state.current {
                    let stomp = u32::from(stomp.get());
                    let slots: Vec<_> = actions::stomp_slots(&current.preset, stomp).collect();
                    dsp.footswitch(current.deployment.plan(), &slots, pressed)?;
                }
                return Ok(());
            }
            DspCommand::BlockMidi { channel, message } => {
                if let Some(current) = &*self.state.current {
                    dsp.block_midi(current.deployment.plan(), channel, message)?;
                }
                return Ok(());
            }
        };
        self.note_pending(Subsystem::Dsp, pending);
        Ok(())
    }

    /// Starts an extension program unless one runs already.
    fn launch_app(&mut self, launch: &app::AppLaunch) {
        if self.app.is_some() {
            return;
        }
        // A black screen first: a program can draw a smaller picture (DOOM draws 640×400). The
        // next frame is whole, after the program or when it does not start.
        let black = vec![0; tetrad_ui::WIDTH as usize * tetrad_ui::HEIGHT as usize * 3];
        let cleared = self.platform.present(&black).map_err(DeviceError::platform);
        self.report(cleared);
        self.platform.forget_display();
        self.bridges.ui.unshown();
        match self.platform.start_app(launch) {
            Ok(process) => {
                self.app = Some(app::Running::new(launch.name.clone(), process));
                self.start_timer(Timer::App, app::POLL);
                self.notice(
                    Subsystem::Extensions,
                    format!("{} started {}", launch.name, launch.program.display()),
                );
            }
            Err(error) => {
                self.present = true;
                self.notice(
                    Subsystem::Extensions,
                    format!("{} did not start: {error}", launch.name),
                );
            }
        }
    }

    /// One UI frame and one LED frame (`leds::Leds::update`) per pass.
    fn present_frame(&mut self) {
        self.present = false;
        // The loaded preset's own Custom layout follows its edits (and undo) before the frame
        // and the LEDs read it.
        custom::sync(&mut self.state);
        self.renders += 1;
        #[cfg(feature = "dev")]
        let started = self.clock.now();
        #[cfg(feature = "dev")]
        let mut rendered = started;
        let presented = match self.bridges.ui.render_for_display(&self.state) {
            Ok(Some(frame)) => {
                #[cfg(feature = "dev")]
                {
                    rendered = self.clock.now();
                }
                (self.platform)
                    .present_rows(frame.rgb, frame.rows)
                    .map_err(DeviceError::platform)
            }
            Ok(None) => Ok(()),
            Err(error) => Err(error.into()),
        };
        #[cfg(feature = "dev")]
        {
            let finished = self.clock.now();
            if rendered == started {
                rendered = finished;
            }
            self.perf.frame(
                rendered.saturating_sub(started),
                finished.saturating_sub(rendered),
            );
        }
        if presented.is_err() {
            self.bridges.ui.unshown();
        }
        self.report(presented);
        for effect in self.bridges.leds.update(&self.state) {
            self.execute(effect);
        }
        match self.bridges.leds.blink_deadline(self.state.now) {
            Some(at) => self.timers.insert(Timer::OverlayBlink, at),
            None => self.timers.remove(&Timer::OverlayBlink),
        };
    }

    fn report<T>(&mut self, result: Result<T, DeviceError>) -> Option<T> {
        result
            .map_err(|error| {
                self.health.record(&error);
                self.notice(error.subsystem(), error.to_string());
            })
            .ok()
    }

    /// Runs one extensions bridge step with the platform's runner backend, then applies its
    /// reservation change. Without a backend (tests, bring-up) extensions do not run.
    fn extensions(
        &mut self,
        step: impl FnOnce(
            &mut extensions::Extensions,
            &mut dyn extensions::ExtensionBackend,
            &State,
        ) -> extensions::Outcome,
    ) -> Vec<Effect> {
        let Some(backend) = self.platform.extensions() else {
            return Vec::new();
        };
        let extensions = &mut self.bridges.extensions;
        extensions.view_stale = true;
        let outcome = step(extensions, backend, &self.state);
        for key in &outcome.storage_changed {
            extensions.save_storage(key, backend);
        }
        let community = outcome
            .community_changed
            .then(|| extensions.community_sources());
        if outcome.reservations_changed {
            let mask = extensions.reserved_channels();
            self.bridges.midi.set_reserved_channels(mask);
        }
        if let Some(sources) = community {
            self.sync_community(&sources);
        }
        // Declared triggers can change with any extension step.
        self.bridges.mappings.view_stale = true;
        for reply in outcome.usb_replies {
            self.usb.answer_extension(reply);
        }
        for reply in outcome.remote_replies {
            self.answer_tab(reply);
        }
        for reply in outcome.ble_replies {
            self.answer_link_extension(reply);
        }
        for (extension, value) in outcome.ble_events {
            self.link_event(extension, value);
        }
        let mut effects = outcome.effects;
        for fired in &outcome.triggers {
            let step = (self.bridges.mappings).on_extension(fired, &self.state, self.clock.now());
            effects.extend(self.mapping_step(step));
        }
        effects
    }

    /// Reads the stored theme choice and the theme files ([`appearance::FILE`],
    /// [`appearance::THEMES_DIR`]). Missing files are fine; bad ones are reported.
    fn load_appearance(&mut self) {
        use tetrad_services::exec::Files;
        let files = self.platform.services().files();
        let stored = files.read_bounded(Path::new(appearance::FILE), appearance::MAX_FILE);
        let problem = match stored {
            Ok(bytes) => self.bridges.appearance.load(bytes.as_deref()),
            Err(error) => Some(error.to_string()),
        };
        if let Some(problem) = problem {
            self.notice(Subsystem::Ui, format!("Appearance: {problem}"));
        }
        self.read_theme_files();
        // Into the views now: the start-up frames are presented before the first view sync.
        self.state.views.appearance = self.bridges.appearance.view();
    }

    /// Reads the theme files again (at boot and when Settings → Appearance opens, so files
    /// copied over while Tetrad runs show up).
    pub(in crate::device) fn read_theme_files(&mut self) {
        use tetrad_services::exec::Files;
        let files = self.platform.services().files();
        let dir = Path::new(appearance::THEMES_DIR);
        let (mut names, problem): (Vec<String>, _) = match files.list_dir(dir) {
            Ok(entries) => (
                (entries.into_iter().flatten())
                    .filter(|entry| !entry.is_dir && entry.name.ends_with(".json"))
                    .map(|entry| entry.name)
                    .collect(),
                None,
            ),
            Err(error) => (Vec::new(), Some(error)),
        };
        names.sort();
        names.truncate(appearance::MAX_THEME_FILES);
        let read = (names.into_iter())
            .map(|name| {
                let bytes = (files.read_bounded(&dir.join(&name), appearance::MAX_FILE))
                    .map_err(|error| error.to_string())
                    .and_then(|bytes| bytes.ok_or_else(|| "the file is gone".to_owned()));
                (name, bytes)
            })
            .collect();
        self.bridges.appearance.set_files(read);
        if let Some(error) = problem {
            self.notice(Subsystem::Ui, format!("Appearance: {error}"));
        }
    }

    /// Applies and stores a theme picked on Settings → Appearance.
    fn set_theme(&mut self, id: tetrad_theme::ThemeId) -> Vec<Effect> {
        use tetrad_services::exec::Files;
        let bytes = match self.bridges.appearance.select(id) {
            Ok(bytes) => bytes,
            Err(reason) => return vec![Effect::Notice(Subsystem::Ui, reason)],
        };
        let files = self.platform.services().files();
        let path = Path::new(appearance::FILE);
        let saved = (path.parent())
            .map_or(Ok(()), |dir| files.create_dirs(dir))
            .and_then(|()| files.write_atomic(path, &bytes));
        match saved {
            Ok(()) => Vec::new(),
            Err(error) => vec![Effect::Notice(
                Subsystem::Ui,
                format!("Theme not saved: {error}"),
            )],
        }
    }

    /// Writes `bytes` atomically to `path`, making its folder; a failure is a notice.
    fn write_file(&mut self, path: &str, bytes: &[u8], failure: &str) {
        use tetrad_services::exec::Files;
        let files = self.platform.services().files();
        let path = Path::new(path);
        let saved = (path.parent())
            .map_or(Ok(()), |dir| files.create_dirs(dir))
            .and_then(|()| files.write_atomic(path, bytes));
        if let Err(error) = saved {
            self.notice(Subsystem::Extensions, format!("{failure}: {error}"));
        }
    }

    /// Reads the Custom mode's stored state ([`custom::ACTIVE_FILE`]) at start-up.
    fn load_custom(&mut self) {
        use tetrad_services::exec::Files;
        let files = self.platform.services().files();
        let stored = files.read_bounded(Path::new(custom::ACTIVE_FILE), custom::MAX_FILE);
        let problem = match stored {
            Ok(bytes) => self.state.custom.load_active(bytes.as_deref()),
            Err(error) => Some(error.to_string()),
        };
        if let Some(problem) = problem {
            self.notice(Subsystem::Extensions, format!("Footswitch mode: {problem}"));
        }
    }

    /// The installation that provides the Custom mode, after any extension change: a new one
    /// brings its stored layout.
    pub(in crate::device) fn sync_custom(&mut self) {
        use tetrad_services::exec::Files;
        let provider = self.bridges.extensions.footswitch_mode();
        self.state
            .custom
            .set_commands(self.bridges.extensions.command_choices());
        let changed =
            (self.state.custom.provider()).map(|p| &p.key) != provider.as_ref().map(|p| &p.key);
        let layout = (provider.as_ref())
            .filter(|_| changed)
            .and_then(|provider| {
                let files = self.platform.services().files();
                let path = provider.layout_path();
                match files.read_bounded(Path::new(&path), custom::MAX_FILE) {
                    Ok(None) => None,
                    Ok(Some(bytes)) if bytes.len() as u64 > custom::MAX_FILE => {
                        Some(Err("the file is too large".to_owned()))
                    }
                    Ok(Some(bytes)) => Some(Ok(bytes)),
                    Err(error) => Some(Err(error.to_string())),
                }
            });
        let effects = custom::set_provider(&mut self.state, provider, layout);
        if !effects.is_empty() {
            self.present = true;
        }
        self.apply(Ok::<_, DeviceError>(effects));
    }

    fn custom_command(&mut self, command: tetrad_ui::CustomCommand) -> Vec<Effect> {
        let ui = &self.bridges.ui;
        let step = custom::command(&mut self.state, command, &|slot| ui.browsed_preset(slot));
        if let Some((path, bytes)) = step.save {
            self.write_file(&path, &bytes, "Footswitch layout not saved");
        }
        self.present = true;
        step.effects
    }

    fn mapping_command(&mut self, command: tetrad_ui::MappingCommand) -> Vec<Effect> {
        let step = self.bridges.mappings.command(command, &self.state);
        self.mapping_step(step)
    }

    /// Saves the global mappings a step changed and hands back its effects.
    fn mapping_step(&mut self, step: mappings::Step) -> Vec<Effect> {
        if let Some(bytes) = step.save_global {
            use tetrad_services::exec::Files;
            let files = self.platform.services().files();
            let path = std::path::Path::new(mappings::GLOBAL_FILE);
            let saved = (path.parent())
                .map_or(Ok(()), |dir| files.create_dirs(dir))
                .and_then(|()| files.write_atomic(path, &bytes));
            if let Err(error) = saved {
                self.notice(
                    Subsystem::Extensions,
                    format!("Mappings not saved: {error}"),
                );
            }
        }
        step.effects
    }

    fn notice(&mut self, subsystem: Subsystem, detail: String) {
        let notice = Notice {
            at: self.clock.now(),
            subsystem,
            detail,
        };
        if self.state.notices.push(notice.clone()) {
            self.platform.log(&notice);
        }
    }

    fn splash(&mut self, value: i32) {
        self.execute(Effect::Splash(SplashCommand::new(value)));
    }

    fn start_timer(&mut self, timer: Timer, after: Duration) {
        self.timers.insert(timer, self.clock.now() + after);
    }

    /// The driver timer follows the DSP bridge's earliest block timer (same clock).
    fn sync_driver_timer(&mut self) {
        match self.dsp.as_ref().and_then(dsp::Dsp::next_timer) {
            Some(at) => self.timers.insert(Timer::DspDriver, at),
            None => self.timers.remove(&Timer::DspDriver),
        };
    }

    /// Once per pass: the driver voice notes go to `State::voice_notes`. A changed voice goes
    /// to Cortex Control (`control::voice_note_echo`); a change of the editor block's notes
    /// requests a frame (`0x61d994` sets the label only while the editor shows that block).
    fn sync_voice_notes(&mut self) {
        let (Some(dsp), Some(current)) = (self.dsp.as_mut(), &*self.state.current) else {
            return;
        };
        let changed = dsp.store_voice_notes(current.deployment.plan(), &mut self.state.voice_notes);
        for &(slot, voice, note) in &changed {
            if self.state.editor_block == Some(slot) {
                self.present = true;
            }
            let echo = control::voice_note_echo(slot, voice, note);
            self.execute(Effect::GridEcho(Box::new(echo)));
        }
    }

    /// Once per pass: the poll timer follows the Looper drivers, `State` gets the DSP
    /// controller's playback, and driver rig writes go through the parameter reducer.
    fn sync_looper(&mut self) {
        let Some(dsp) = self.dsp.as_mut() else {
            return;
        };
        let rig_writes = dsp.take_rig_writes();
        let playback = dsp.has_loopers().then(|| dsp.looper_playback());
        if playback.is_some() {
            self.timers.entry(Timer::LooperPoll).or_insert(
                self.clock.now() + tetrad_dsp_host::setup::drivers::looper::STATUS_PERIOD,
            );
        } else {
            self.timers.remove(&Timer::LooperPoll);
        }
        if let Some(playback) = playback
            && playback != self.state.looper_playback
        {
            let effects = actions::reduce(Action::LooperPlayback(playback), &mut self.state);
            self.apply(effects);
        }
        for (slot, index, value) in rig_writes {
            let effects = actions::reduce(
                Action::SetParameter {
                    site: tetrad_engine::Site::Slot(slot),
                    index,
                    value,
                },
                &mut self.state,
            );
            self.apply(effects);
        }
    }

    /// The footswitch hold timer follows the input bridge's pending hold.
    fn sync_hold_timer(&mut self) {
        match self.bridges.input.hold_deadline() {
            Some(at) => self.timers.insert(Timer::FootswitchHold, at),
            None => self.timers.remove(&Timer::FootswitchHold),
        };
    }

    /// The capture meter timer runs while a capture session or the NC2 controller meters.
    fn sync_capture_timer(&mut self) {
        let now = self.clock.now();
        match self.bridges.capture.meter_interval() {
            Some(interval) => {
                self.timers
                    .entry(Timer::CaptureMeters)
                    .or_insert(now + interval);
            }
            None => {
                self.timers.remove(&Timer::CaptureMeters);
            }
        }
    }

    fn schedule_settings_flush(&mut self) {
        if let Some(Effect::StartTimer { timer, after }) =
            actions::settings_flush(&self.state.settings)
        {
            self.start_timer(timer, after);
        }
    }

    fn note_failures(&mut self, failures: Vec<String>) {
        for detail in failures {
            self.notice(Subsystem::Dsp, detail);
        }
    }

    fn note_pending(&mut self, subsystem: Subsystem, pending: dsp::Pending) {
        for step in pending.0 {
            self.notice(subsystem, format!("not done: {step}"));
        }
    }
}

impl<P: Platform> std::fmt::Debug for Device<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("product", &self.state.product)
            .field("platform", &self.state.platform)
            .field("dsp", &self.dsp.is_some())
            .field("timers", &self.timers)
            .finish_non_exhaustive()
    }
}

impl StartupError {
    fn platform(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Platform(Box::new(error))
    }
}

/// After a `0x20` answer (0x005280d8): a Grid plugin block that became licensed forces a
/// preset reload ("Forcing preset reload", 0x00527d80); otherwise a block whose status
/// changed gets its bypass again. Inferred: the scene recall re-pushes every block's bypass,
/// where stock sends only the changed blocks.
/// A fault mute that did not reach both cores (a dead core refuses it) is a notice.
/// I/O meters arrive about 12 times a second, signal or not. They need a frame only while
/// the view draws them ([`ui::Ui::shows_meters`]) or when a grid clip light went on or out;
/// expiry shows at the next reading.
fn meters_need_frame(ui: &ui::Ui, state: &State, shown: &mut tetrad_ui::GridClips) -> bool {
    let clips = state.grid_clips.view(state.now);
    let changed = clips != *shown;
    *shown = clips;
    changed || ui.shows_meters()
}

fn fault_mute_notice(muted: Result<(), dsp::DspError>) -> Option<Effect> {
    muted
        .err()
        .map(|error| Effect::Notice(Subsystem::Dsp, format!("DSP fault mute: {error}")))
}

fn licence_repush(state: &State, before: &services::Licences, added: &[u32]) -> Vec<Effect> {
    let (Some(specs), Some(current)) = (&state.specs, &*state.current) else {
        return Vec::new();
    };
    let plugins: Vec<u32> = (current.deployment.layout().sites())
        .filter(|(site, _)| matches!(site, tetrad_engine::Site::Slot(_)))
        .filter_map(|(_, instance)| specs.models.get(&instance.model.0)?.plugin_id)
        .collect();
    let changed = |&plugin: &u32| before.status(plugin) != state.licences.status(plugin);
    if plugins.iter().any(|plugin| added.contains(plugin)) {
        vec![Effect::RecallPreset(Box::new(current.preset.clone()))]
    } else if plugins.iter().any(changed) {
        vec![Effect::RecallScene(None)]
    } else {
        Vec::new()
    }
}

/// Stock wait flag and timeout per control writer (dsp-controls "Transport"): MIDI clock
/// enables and resets 1/1000, USB MIDI out 0/1000, incoming-clock notifications 0/0, gate
/// enable 1/100; tempo change (`0x875a48`), metronome status and the other writers 0/100.
fn control_policy(request: &ControlRequest) -> SendPolicy {
    match request {
        ControlRequest::EnableUsbMidiClock { .. }
        | ControlRequest::EnableInternalMidiClock { .. }
        | ControlRequest::ResetInternalMidiClock { .. }
        | ControlRequest::EnableMidiClockOut { .. }
        | ControlRequest::EnableMidiClockIn { .. } => SendPolicy::new(true, 1000),
        ControlRequest::UsbMidiOut { .. } => SendPolicy::new(false, 1000),
        ControlRequest::MidiClockCountAndTempo { .. } | ControlRequest::MidiClockStart => {
            SendPolicy::new(false, 0)
        }
        ControlRequest::InputGateEnabled { .. } => SendPolicy::new(true, 100),
        _ => SendPolicy::new(false, 100),
    }
}

impl<P: Platform> Device<P> {
    fn refresh_directory_media(&mut self) {
        // r30-services MR 1 (review r29 B10): a capture store edit outside Services moves the
        // cloud generation too.
        self.bridges.services.downloads_changed();
        if let Some(media) = &self.media {
            let database = self
                .state
                .library
                .as_ref()
                .and_then(|library| library.database.as_ref());
            let (entries, failures) = media.entries(database);
            self.state.capture_names = control::capture_names(&entries);
            self.bridges.ui.set_media(entries);
            match media.directory_media(database) {
                Ok(details) => self.bridges.ui.set_directory_media(details),
                Err(error) => self.notice(Subsystem::Library, error.to_string()),
            }
            for failure in failures {
                self.notice(Subsystem::Library, failure);
            }
        }
    }
}

/// Slowest wall time per [`Device::step`] phase: input gather, event reduce, Cortex Control and
/// services sync, view sync, frame (render, present, LEDs). Diagnostics only; no logic reads it.
#[derive(Debug, Clone, Copy, Default)]
pub struct PhaseTimes {
    pub gather: Duration,
    pub reduce: Duration,
    /// Effects of the reduced events (part of the reduce loop).
    pub apply: Duration,
    pub control: Duration,
    pub views: Duration,
    pub frame: Duration,
}

impl std::fmt::Display for PhaseTimes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "gather={} reduce={} apply={} control={} views={} frame={}",
            self.gather.as_millis(),
            self.reduce.as_millis(),
            self.apply.as_millis(),
            self.control.as_millis(),
            self.views.as_millis(),
            self.frame.as_millis()
        )
    }
}

impl PhaseTimes {
    fn max(self, other: Self) -> Self {
        Self {
            gather: self.gather.max(other.gather),
            reduce: self.reduce.max(other.reduce),
            apply: self.apply.max(other.apply),
            control: self.control.max(other.control),
            views: self.views.max(other.views),
            frame: self.frame.max(other.frame),
        }
    }
}

/// Wall time since the last [`Lap::end`]. A phase ends where its last probe unit ends, and the
/// next unit starts there: one clock read serves all three (on the unit, each read is a syscall).
struct Lap(Instant);

impl Lap {
    fn start() -> Self {
        Self(Instant::now())
    }

    /// Ends the phase at `now`, a time just read (the end of its last probe unit).
    fn end(&mut self, now: Instant, phase: &mut Duration) {
        *phase = now - self.0;
        self.0 = now;
    }

    /// As [`Self::end`], but adds to the phase (a phase split over loop iterations).
    fn add(&mut self, phase: &mut Duration) {
        let now = Instant::now();
        *phase += now - self.0;
        self.0 = now;
    }
}

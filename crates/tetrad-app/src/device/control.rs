//! Cortex Control bridge: one `tetrad_control::link::ControlLink`, a `DeviceModel` over [`State`],
//! the compression and transform stages, and the DSP HID transport.
//!
//! - Stock applies a request inside its receiver. Here the device applies the model's
//!   effects after the link step, so a request keeps its context until
//!   [`Effect::ControlRequestDone`]: the pushes it causes echo its request ID (`0x6e01d8`),
//!   and the RecallPreset reply goes out last with the live preset.
//! - Reports arrive as DSP response `0x10` ([`Event::Control`](super::Event)). The device
//!   drains the DSP only after boot, which holds them until the DSP is ready (`0x6e0c10`).
//! - Every `Step::reports` entry becomes one [`Effect::ControlReport`], which queues it
//!   ([`outgoing`], stock `USBMessageSender` `0x29b0e4`). [`Control::send_reports`] sends
//!   the queue in order, at most [`outgoing::FRAMES_PER_PASS`] frames per device pass, each
//!   as a `0x27` frame to core 4 with wait 0 and timeout 100 ms (`0x6dfbf8`).
//! - `SessionEvent::FlushTransmit` (`0x29ad14`): the queued reports are dropped and the
//!   reset-marker frame goes out before the next report.
//! - [`Control::tick`] runs every [`CONTROL_TICK`](super::CONTROL_TICK). The link schedules
//!   the stock 100 ms and 1000 ms senders on it.
//! - Host changes to shared state (scene, scene texts, setlist position, tuner view) go
//!   through the device action reducer; settings writes through `actions::update_settings`.
//! - A host File or CloudProduct READ builds its reply over the next device passes
//!   ([`Control::spread_step`], A3, owned): stock builds it in the receiver, which held one
//!   QC pass for seconds at connect. The messages and their request ID do not change.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use tetrad_control::codecs::{
    AvailableModes, CompilerInhibitedModulesMessage, ControlMessage, CortexMessageType,
    GeneralMIDIMessage, GeneralMIDIMessages, GeneralSettingsMessage, GridMessage, LooperMessage,
    LooperUpdateType, MIDISettingsMessage, MasterVolumeMessage, MessageAction, MidiMessageInfo,
    ModeMessage, PowerOptions, RawMessageBytes, RecallPresetMessage, RecallPresetReason,
    SetlistPositionMessage, SuspendReason, VersionMessage,
};
use tetrad_control::device::{DeviceError as ModelError, DeviceModel, DeviceResult};
use tetrad_control::link::{
    ControlLink, LinkError, LinkLimits, Notification, Step, Suspension, UndoFrames,
};
use tetrad_control::pipeline::Stages;
use tetrad_control::session::{Session, SessionEvent};
use tetrad_dsp_host::transport::ControlPacket;
use tetrad_dsp_protocol::CodecLimits;
use tetrad_dsp_protocol::fragment::Fragment;
use tetrad_dsp_protocol::hid::HidReportFrame;
use tetrad_dsp_protocol::requests::SendPolicy;
use tetrad_dsp_protocol::responses::{DiagnosticsBlock, Soc2ArmDiagnostics, UsbMeter};
use tetrad_engine::Row;
use tetrad_media::factory::{DeviceCipher, DeviceSerial};
use tetrad_preset::{Chain, MIDI_MESSAGES, MidiList, Model, Param, ParamValue, Preset, SceneValue};
use tetrad_settings::controls::{FootswitchMode, ModeSettings};
use zeroize::Zeroizing;

use super::actions::Action;
use super::bringup::RunOptions;
use super::dsp::{Dsp, DspError};
use super::preset::{self, CAPTURE_KEY_UNITS, CAPTURE_MODELS};
use super::services::{ControlLogin, ControlTransport, Handoff, UploadState};
use super::{Effect, Platform, State, Subsystem};

/// Keep-alive interval: `QTimer::setInterval(10000)` in the controller ctor (`0x6e1170`).
pub const KEEP_ALIVE: Duration = Duration::from_secs(10);
/// `0x6dfbf8`: the `0x27` request goes to core 4 without waiting, timeout 100 ms.
const HID_SEND: SendPolicy = SendPolicy::new(false, 100);
/// `0x6b4b50` packs ModelRepo into a 1 MiB memory archive.
pub const MODEL_REPO_ARCHIVE_BYTES: usize = 0x10_0000;
/// `0x19af74` while a Control UPDATE runs (`+0x3c`, set by `0x1962c8`): the echo waits on a
/// single-shot 100 ms timer, and a newer echo replaces a pending one.
pub const VOLUME_ECHO_DELAY: Duration = Duration::from_millis(100);
/// The dirty test runs on a single-shot 100 ms timer (ctor `0x5b71f8`–`0x5b720c`); here
/// at most once per 100 ms.
pub const DIRTY_CHECK: Duration = Duration::from_millis(100);
/// LooperMessageSender (`0x18f9d0`): the Looper Buttons push waits on a single-shot 150 ms
/// timer that each trigger restarts (`0x18ff24(−1)` → `onSendUpdateButtonsDelay` `0x1905c8`).
pub const LOOPER_BUTTONS_DELAY: Duration = Duration::from_millis(150);

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error(transparent)]
    Dsp(#[from] DspError),
    #[error("ModelRepo archive exceeds {MODEL_REPO_ARCHIVE_BYTES} bytes")]
    ModelRepoTooLarge,
}

pub struct Control {
    link: ControlLink,
    compression: Zlib,
    /// The comms transform: none until the first Version key exchange ends
    /// ([`Control::complete_key_exchange`]); stock keeps the `+0x400` vector until the next one.
    transform: Option<CommsCipher>,
    /// The device serial (DSPController `+0xb884`), the text of the key request `0x42`.
    serial: Option<DeviceSerial>,
    /// Prepared ModelRepo payload (`0x11e3cc`); `None` until the device installs it.
    model_repo: Option<Vec<u8>>,
    /// The reports that wait for the DSP, after a pending reset marker.
    outgoing: outgoing::Outgoing,
    /// Byte 0 of the last SOC2 ARM diagnostics record (USB host present).
    usb_present: Option<u8>,
    /// The capture compiler locks (`+0x3409`, `+0x3411`), set by the capture bridge.
    capture_lock: bool,
    /// Message 42 values last sent (gate, EQ).
    inhibited: Option<(bool, bool)>,
    /// `+0x2e5d`: the dirty flag last computed, connected or not.
    dirty: bool,
    /// The next dirty evaluation ([`DIRTY_CHECK`]).
    dirty_due: Duration,
    /// The request whose effects the device is applying (its request ID).
    request: Option<u64>,
    /// The request deferred its preset reply ([`tetrad_control::device::Reply::Deferred`]).
    preset_reply: bool,
    /// The GridMove echo of a Control drop, sent by [`Self::sync`] (`grid::DropEcho`).
    drop_echo: Option<grid::DropEcho>,
    /// The mode settings last seen; a change is pushed.
    mode: Option<ModeSettings>,
    /// The GeneralSettings state last seen; each changed field is pushed.
    general: Option<GeneralSettingsMessage>,
    /// The Global EQ state last seen; each change is pushed.
    global_eq: Option<tetrad_control::codecs::GlobalEQMessage>,
    /// The License state last seen; a change is pushed whole.
    licences: Option<tetrad_control::codecs::LicenseMessage>,
    /// The delayed MasterVolume echo (`DelayedMessageHandler<MasterVolume, 17>`): due time
    /// and level.
    volume_echo: Option<(Duration, f32)>,
    /// A capture preset ran at the last [`Control::sync`] (DSPController `+0x758`).
    capture_running: bool,
    /// `factory.db` (read-only) for factory Virtual Device Presets (`0x1f78f4` → `0x8330cc`).
    factory: Option<tetrad_storage::stock::StockDb>,
    /// The device cipher of capture metadata (name and version) for the DefaultParameters
    /// resync ([`Control::set_media_cipher`]).
    media_cipher: Option<DeviceCipher>,
    /// The factory IR names of the File listing ([`Self::set_factory_irs`]).
    factory_irs: Option<Vec<String>>,
    /// The cloud products in transfer state 1, Downloading
    /// ([`Self::set_downloads_in_flight`]).
    downloads_in_flight: Vec<String>,
    /// The upload states by local key ([`Self::set_upload_states`]).
    upload_states: Vec<(String, UploadState)>,
    /// The inputs of Diagnostics (7) (`sendDiagnostics` `0x23e6d4`).
    diagnostics: status::Diagnostics,
    /// The platform wall clock at the last host report ([`Control::set_wall_clock`]): the
    /// date of a File import.
    wall_clock: Option<SystemTime>,
    /// `/media/p4` at the last host report ([`Control::set_disk_space`]): GeneralSettings 17,
    /// 18 and 36.
    disk_space: Option<tetrad_sys::fs::FsSpace>,
    /// The LocalBackup (40) state on this side.
    local_backup: backup::LocalBackup,
    /// The CloudBackup (41) request that runs on the services bridge, with its deadline.
    cloud: Option<(cloud::CloudOp, Duration)>,
    /// Directory Multi Select is on (`UiState::multi_select`), for BulkOperation (57) READ.
    multi_select: bool,
    /// The run Control was told about ([`Control::bulk_run`]).
    batch: Option<bulk::BatchSeen>,
    /// The end of the last batch ([`Control::set_batch_end`]).
    batch_end: Option<super::services::BatchEnd>,
    /// A suspend or resume that runs after the current step.
    link_op: Option<backup::LinkOp>,
    /// The run's options ([`Control::set_run_options`]): bring-up refusals and `--persist`.
    options: Rc<RunOptions>,
    /// The input mask has bit 0 clear (`0x5447b8` → `0x1ec97c(mask, 1)`): LOCK or an update
    /// install. Kept from the `0x16ad60` pushes ([`lock_notice`]), connected or not.
    input_locked: bool,
    /// Be Right Back runs (from the [`standby_notice`] pushes). Its entry `0x6a82ac` clears
    /// mask bits 0 and 1 (`0x1ec83c(3)`) without the `0x16ad60` push (that needs bit `0x10`);
    /// its exit `0x6a852c` sets them again (`0x1ec714(3)`, or 2 when LOCK was on before).
    standby: bool,
    /// The Looper record and controller flags last seen ([`looper_pushes`]).
    looper: LooperSeen,
    /// The due time of the delayed Looper Buttons push ([`LOOPER_BUTTONS_DELAY`]).
    looper_buttons: Option<Duration>,
    /// The updater view from the services pushes, for Updater (60) READ and its gates.
    updater: updater::UpdaterSeen,
    /// HTTP calls of the service worker that Control carries ([`Control::forward_link`]).
    forwards: forward::Forwards,
    /// The services' handle of [`Self::forwards`] for cloud jobs ([`Control::cloud_route`]),
    /// made once: `ControlTransport` equality is identity.
    route: ControlTransport,
    /// Excess warnings that wait for a connect (`0x1545924` captures, `0x1545925` presets).
    excess_pending: Vec<tetrad_storage::stock::DownloadKind>,
    /// The Control-started upload question Control was asked ([`Control::upload_question`]).
    asked: Option<ask::Asked>,
    /// A host report is in the link ([`Control::on_report`]): a READ may spread its reply.
    receiving: bool,
    /// The message the last [`Self::on_report`] completed (the device `SLOW` line, A3).
    last_received: Option<tetrad_control::link::Received>,
    /// The READ reply in progress and its request ID ([`Control::spread_step`]).
    spread: Option<(u64, files::Spread)>,
}

/// `voiceNoteChanged` (`0x4f30c4` → `0x25fb7c`): a Minivoicer voice note change to Cortex
/// Control as NOTE DISPLAY 23 / 24 of the block, the Grid shape of [`super::dsp::block_echo`].
/// The value is `to_normalized(note, −1, 127, 1)` (`0x64b8f8`). A silent voice is note −1:
/// `0x61da30` loads the word `0xffffffff` (`0x61dc08`) and `0x61d9f8` converts it as the
/// integer −1 (`vcvt.f32.s32`), so it sends 0.0 (review r26 S1). Stock does not write the
/// preset.
pub fn voice_note_echo(
    slot: tetrad_engine::Slot,
    voice: usize,
    note: Option<u8>,
) -> ControlMessage {
    let value = (note.map_or(-1.0, f32::from) + 1.0) / 128.0;
    let index = if voice == 0 { 23 } else { 24 };
    super::dsp::block_echo(slot, index, value)
}

fn excess_warning(kind: tetrad_storage::stock::DownloadKind) -> Notification {
    use tetrad_control::codecs::GenericErrorCode as Code;
    Notification::GenericError {
        code: match kind {
            tetrad_storage::stock::DownloadKind::Capture => {
                Code::ExcessDownloadsCapturesWarningTriggered
            }
            tetrad_storage::stock::DownloadKind::Preset => {
                Code::ExcessDownloadsPresetsWarningTriggered
            }
        },
        request_id: 0,
    }
}

/// Account, clock and updater work that Cortex Control starts and the services bridge runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountRequest {
    /// License UPDATE (`0x113094` → `0x1f0e80`): the licence refresh.
    RefreshLicences,
    /// CloudLogin DELETE (`0x239828` → `0x3080b4`): unpair, then sign out.
    SignOut,
    /// CloudLogin CREATE (`loginFromControl` `0x307e18`): the tokens, the pair and the profile.
    Login(Handoff<ControlLogin>),
    /// SystemTimeSync UPDATE (`0x1ba1b0`): the clock in ms since the epoch, while unset.
    SetClock(u64),
    /// Updater UPDATE (`0x1391b4`) after the stock gates: the same services command as the
    /// device updater page, so an install keeps its pre-steps (input mask, mute, lock).
    Updater(tetrad_ui::UpdaterCommand),
    /// Updater UPDATE RestartDownloadViaControl (`0x4e4cc0` → `0x4e7808`): the update check
    /// and download run again with Cortex Control as the HTTP transport (forward 61).
    RestartDownloadViaControl,
    /// CloudBackup (41) after the stock gates (`cloud::run`).
    CloudBackup(CloudBackupRequest),
    /// Logs (44) CREATE (`0x114cd4` → `0x182868`): the same log upload as the device
    /// support report (`0x6bcb30`), so the same services gate.
    UploadLogs,
    /// ProcessDownloadsQueue (45) UPDATE (`0x1326a8`), logged in: the downloads-queue client
    /// (`Services::process_downloads_queue`) for presets or captures.
    DownloadsQueue(tetrad_services::cloud::services::ProductKind),
    /// CloudProduct (46) DOWNLOAD (`0xf3f24`) of one capture or IR key with no `to_folder`, into
    /// its saving destination.
    CloudProductDownload {
        kind: tetrad_services::cloud::services::ProductKind,
        id: String,
    },
    /// CloudProduct (46) DOWNLOAD of more capture or IR keys (`0x786264` → `0x847620`): one
    /// Multi Select batch into the saving destination, in order (`Services::control_downloads`).
    CloudProductDownloads {
        kind: tetrad_services::cloud::services::ProductKind,
        ids: Vec<String>,
    },
    /// CloudProduct (46) DOWNLOAD (`0xf3f24` → `0x1dd61c`; more keys `0x786ac4` →
    /// `0x84e68c`) of cloud presets into slots of the user setlist named `setlist`
    /// (`to_folder`): (product id, slot) in order, as `Device::download_presets` takes them.
    CloudPresetDownload {
        setlist: String,
        items: Vec<(String, usize)>,
    },
    /// CloudProduct (46) DELETE (`0xf330c` → `0x1d916c`; more ids the run `0x785b74`) of
    /// products from the user's cloud account (`Services::control_delete`). Stock asks for no
    /// confirmation on the device.
    CloudProductDelete {
        kind: tetrad_services::cloud::services::ProductKind,
        ids: Vec<String>,
    },
    /// CloudProduct (46) UPLOAD (`0xf3060` → `0x1e8a70` → `0x1d81d0`) of the user setlist
    /// preset at `path`: the Directory preset upload (`Device::upload`).
    CloudProductUpload { path: String },
    /// CloudProduct (46) UPLOAD type 2 (`0xf3060` → `0x765520` → `0x1d8278`) of the user
    /// capture `key`: the Directory capture upload (`Device::upload`).
    CloudCaptureUpload { key: String },
    /// CloudProduct (46) UPLOAD of more keys of one kind (`0xf3060` → `0x785dcc(..,
    /// fromControl 1)`): the batch upload, one item after the other (re-r38 §2).
    CloudBatchUpload { items: Vec<tetrad_ui::UploadItem> },
    /// BulkOperation (57) DELETE (`0xed8d0`): the running run stops before its next item
    /// (`+0x7c`, `Services::cancel_bulk_run`); the item in flight completes.
    CancelBulkRun,
    /// The answer to a Control-started upload question ([`Control::upload_question`],
    /// `0xf986c`): OVERWRITE or SKIP / CANCEL of the question `question` (`UploadAsk::question`).
    UploadAnswer { question: u64, overwrite: bool },
}

impl Default for Control {
    fn default() -> Self {
        Self::new()
    }
}

impl Control {
    pub fn new() -> Self {
        let session = Session::new(Duration::ZERO, KEEP_ALIVE)
            .expect("stock keep-alive interval is valid")
            .with_coros_version(tetrad_firmware::running().version);
        let forwards = forward::Forwards::default();
        Self {
            link: ControlLink::new(session, LinkLimits::default()),
            compression: Zlib,
            transform: None,
            serial: None,
            model_repo: None,
            outgoing: outgoing::Outgoing::default(),
            usb_present: None,
            capture_lock: false,
            inhibited: None,
            dirty: false,
            dirty_due: Duration::ZERO,
            request: None,
            preset_reply: false,
            drop_echo: None,
            mode: None,
            general: None,
            global_eq: None,
            licences: None,
            volume_echo: None,
            capture_running: false,
            factory: None,
            factory_irs: None,
            downloads_in_flight: Vec::new(),
            upload_states: Vec::new(),
            media_cipher: None,
            wall_clock: None,
            disk_space: None,
            diagnostics: status::Diagnostics::default(),
            local_backup: backup::LocalBackup::Idle,
            cloud: None,
            multi_select: false,
            batch: None,
            batch_end: None,
            link_op: None,
            options: Rc::new(RunOptions::STOCK),
            input_locked: false,
            standby: false,
            looper: LooperSeen::default(),
            looper_buttons: None,
            updater: updater::UpdaterSeen::default(),
            route: ControlTransport::new(forwards.link()),
            forwards,
            excess_pending: Vec::new(),
            asked: None,
            receiving: false,
            last_received: None,
            spread: None,
        }
    }

    /// The "Excessive %1 in Downloads folder" check found `kind` (`0x22cab0`; the device
    /// runs it with `Device::excessive_downloads`): a connected Control gets GenericError 4
    /// (captures) or 3 (presets) with request ID 0 (`0x105d94`); else the warning waits for
    /// the next connect.
    pub fn excessive_downloads(
        &mut self,
        kind: tetrad_storage::stock::DownloadKind,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        if !self.connected() {
            if !self.excess_pending.contains(&kind) {
                self.excess_pending.push(kind);
            }
            return Ok(Vec::new());
        }
        self.notify(excess_warning(kind), state, now)
    }

    /// `0x546b98` on a connect: each waiting warning goes out when its folder still holds
    /// more than 512 items and "Don't show again" is off; the waiting flags clear.
    fn excess_on_connect(
        &mut self,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        use tetrad_storage::stock::DownloadKind;
        let mut effects = Vec::new();
        for kind in std::mem::take(&mut self.excess_pending) {
            let general = &state.settings.settings().general;
            let suppressed = match kind {
                DownloadKind::Capture => general.dont_show_excess_capture_downloads,
                DownloadKind::Preset => general.dont_show_excess_preset_downloads,
            };
            let excessive = (state.library.as_ref())
                .and_then(|library| library.database.as_ref())
                .and_then(|db| db.view().download_ids(kind).ok())
                .is_some_and(|ids| ids.len() > tetrad_library::stock::EXCESSIVE_DOWNLOADS);
            if excessive && !suppressed {
                effects.extend(self.notify(excess_warning(kind), state, now)?);
            }
        }
        Ok(effects)
    }

    /// The handle of [`forward::ForwardHttp`]: an HTTP client for the service worker that
    /// sends each call through Cortex Control (forward messages 29, 30, 31, 61).
    pub fn forward_link(&self) -> forward::ForwardLink {
        self.forwards.link()
    }

    /// The route of the cloud calls (`Services::set_control_route`, read by the device before
    /// services work): Cortex Control while it is connected (`0x6e0444`) and its KeepAlive says
    /// online (`0x6e045c`, `+0x3d9`), as `0x80da7c` tests per call (re-r44 §3); else `None`
    /// (Wi-Fi). Stock does not test the Wi-Fi state: Control wins. A bring-up run without
    /// `--control-cloud` gets `None` (`RunOptions::control_cloud_refusal`). Review-r45 M4 (O):
    /// the link must also have its transform (the Version key exchange), as 29, 30 and 31 go
    /// only sealed; stock tests only the two flags, and its call then waits for its deadline.
    pub fn cloud_route(&self) -> Option<ControlTransport> {
        let usable = self.options.control_cloud_refusal().is_none()
            && self.online()
            && self.transform.is_some();
        usable.then(|| self.route.clone())
    }

    /// `factory.db`, opened read-only at boot (`0x008328a4`).
    pub fn set_factory_presets(&mut self, factory: tetrad_storage::stock::StockDb) {
        self.factory = Some(factory);
    }

    /// The factory IR catalog (`DspFile::FactoryIrCatalog`): the File READ lists its names in
    /// the factory IR folder (`0x248ad4`). A catalog that does not decode lists none.
    pub fn set_factory_irs(&mut self, catalog: &[u8]) {
        self.factory_irs = files::factory_ir_names(catalog);
    }

    /// The media library's device cipher: capture metadata names the DefaultParameters (19)
    /// capture blocks (`0x704530`).
    pub fn set_media_cipher(&mut self, cipher: DeviceCipher) {
        self.media_cipher = Some(cipher);
    }

    /// The run's options; [`RunOptions::STOCK`] until set. A bring-up run refuses each action
    /// Control reduces itself as `RunOptions::refusal` refuses it on the device (resets in
    /// every mode, review-2 R1), and without `--persist` the library writes that receivers run
    /// themselves: list files, setlists, `zenui.db` (review r15 R10, review-2 R5).
    pub fn set_run_options(&mut self, options: &RunOptions) {
        self.options = Rc::new(options.clone());
    }

    /// Directory Multi Select is on, read before each host report (BulkOperation READ).
    pub fn set_multi_select(&mut self, active: bool) {
        self.multi_select = active;
    }

    /// The cloud downloads in flight (`Services::downloads_in_flight`), read before each host
    /// report and resync: CloudProduct (46) READ lists them (`0xf5470`).
    pub fn set_downloads_in_flight(&mut self, ids: &[String]) {
        if self.downloads_in_flight != ids {
            self.downloads_in_flight = ids.to_vec();
        }
    }

    /// The upload states (`Services::upload_states`), read before each host report:
    /// CloudProduct (46) READ lists them (`0xf5470`, states 3, 4, 5).
    pub fn set_upload_states(&mut self, states: Vec<(String, UploadState)>) {
        self.upload_states = states;
    }

    /// The platform wall clock, read before each host report: a File (4) IR import dates its
    /// metadata with it (`0x24dae0`). `None` refuses imports.
    pub fn set_wall_clock(&mut self, now: Option<SystemTime>) {
        self.wall_clock = now;
    }

    /// The `/media/p4` space ([`Platform::disk_space`]): the GeneralSettings (9) READ sends it
    /// (fields 17, 18, 36). The device reads it at a host report at most every
    /// [`super::DISK_SPACE_HOLD`], as stock keeps a reading for 10 minutes (`0x210f6c`).
    pub fn set_disk_space(&mut self, space: Option<tetrad_sys::fs::FsSpace>) {
        self.disk_space = space;
    }

    /// Change pushes after each device step, as the stock senders run on their signals:
    /// UndoRedo frames (`0x1be1a8`, tetrad-control skips an unchanged pair), the available
    /// modes (`0x58ce2c` → `0x273854`), Mode (`0x58c738`, `0x589c98` → `0x2734e8`), message
    /// 42 (deploy `0x585464` → `0x581358`/`0x581398`), the GridMove echo of a Control drop
    /// (`grid::DropEcho`), then the timer senders outside any
    /// request: PresetDirty (`0x5ad250` → `0x1a6dfc`, every [`DIRTY_CHECK`]; not sent while a
    /// capture preset runs, `+0x758`), a due MasterVolume echo and the held Grid echoes.
    pub fn sync(
        &mut self,
        state: &mut State,
        capture_running: bool,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        self.capture_running = capture_running;
        let mut pushes = Vec::new();
        let connected = self.connected();
        let link = self.link.state();
        // Production automation disables the sender; stock retries only on the next signal.
        if connected && !link.production_automation {
            let frames = undo_frames(&state.history);
            if link.undo_frames.as_ref() != Some(&frames) {
                pushes.push(Notification::UndoFrames(frames));
            }
        }
        let mode = &state.settings.settings().mode;
        if self.mode.as_ref() != Some(mode)
            && let Some(previous) = self.mode.replace(mode.clone())
        {
            let update = Some(MessageAction::Update.into());
            if previous.available() != mode.available() {
                pushes.push(Notification::Update(ControlMessage::Mode(ModeMessage {
                    action: update,
                    available_modes: Some(available_modes(mode)),
                    ..Default::default()
                })));
            }
            if (previous.mode(), previous.atma_page) != (mode.mode(), mode.atma_page) {
                pushes.push(Notification::Update(ControlMessage::Mode(ModeMessage {
                    action: update,
                    ..mode_message(state)
                })));
            }
        }
        // The GeneralSettings setters push their own field (`0x168974`, …, `0x166c04`) and
        // the Global EQ editor its change (`0x16eb54`, `0x16e7b8`, `0x16e474`). Built only
        // while connected (a disconnected sender sends nothing; the resync sends the state).
        let input_locked = self.input_locked || self.standby;
        let general =
            connected.then(|| settings::general_message(state, input_locked, self.standby));
        let global_eq = connected
            .then(|| settings::global_eq_message(state))
            .flatten();
        let licences = connected.then(|| account::licences(state));
        if let (Some(previous), Some(new)) = (&self.general, &general) {
            pushes.extend(
                settings::general_pushes(previous, new)
                    .into_iter()
                    .map(|push| Notification::Update(push.into())),
            );
        }
        self.general = general;
        if let (Some(previous), Some(new)) = (&self.global_eq, &global_eq) {
            pushes.extend(
                settings::global_eq_pushes(previous, new)
                    .into_iter()
                    .map(|push| Notification::Update(push.into())),
            );
        }
        self.global_eq = global_eq;
        // The licence state push `0x1147f0` follows a refresh result (`0x1f18c0`), a failed
        // refresh (`0x1f2714` → `0x1144ac`) and the reset clear (`0x1f0c50`). Owned: pushed only
        // when the state changed.
        if let (Some(previous), Some(new)) = (&self.licences, &licences)
            && previous != new
        {
            pushes.push(Notification::Update(new.clone().into()));
        }
        self.licences = licences;
        // Owned: stock clears both flags and sets them again on every deploy, each with a
        // push; Tetrad pushes the pair only when a value changes.
        let inhibited = inhibited_modules(self.capture_lock, state);
        if self
            .inhibited
            .replace(inhibited)
            .is_some_and(|sent| sent != inhibited)
        {
            pushes.extend(inhibited_pair(inhibited).map(Notification::Update));
        }
        // `0x59f620`: the drop's GridMove echo goes after its frame lists.
        if let Some(echo) = self.drop_echo.take().and_then(|echo| echo.echo(state)) {
            pushes.push(Notification::Update(echo.into()));
        }
        let mut effects = Vec::new();
        for push in pushes {
            effects.extend(self.notify(push, state, now)?);
        }
        let mut timed = Vec::new();
        // The Looper pushes follow DSP records and button presses, never a host request.
        let (looper, buttons) = looper_pushes(&mut self.looper, state);
        timed.extend(looper.into_iter().map(Notification::Update));
        if buttons {
            self.looper_buttons = Some(now + LOOPER_BUTTONS_DELAY);
        }
        if let Some(due) = self.looper_buttons
            && now >= due
        {
            self.looper_buttons = None;
            timed.push(Notification::Update(looper_update(LooperMessage {
                update_type: Some(LooperUpdateType::Buttons.into()),
                ..Default::default()
            })));
        }
        if now >= self.dirty_due {
            self.dirty_due = now + DIRTY_CHECK;
            let dirty = state.preset_dirty();
            if dirty != self.dirty {
                self.dirty = dirty;
                if !capture_running {
                    timed.push(Notification::PresetDirty(dirty));
                }
            }
        }
        if let Some((due, volume)) = self.volume_echo
            && now >= due
        {
            self.volume_echo = None;
            timed.push(Notification::Update(master_volume_echo(volume)));
        }
        let request = self.request.take();
        for push in timed {
            effects.extend(self.notify(push, state, now)?);
        }
        if self.link.next_grid_echo().is_some_and(|due| due <= now) {
            effects.extend(self.with_state(state, now, |link, model, stages| {
                link.flush_grid_echoes(now, model, stages)
            })?);
        }
        self.request = request;
        Ok(effects)
    }

    /// [`Effect::ControlRequestDone`]: the pushes the request caused ([`Self::sync`]), then
    /// a deferred RecallPreset reply with the working preset (`0x277468`), all
    /// with the request ID. The request context then ends.
    pub fn request_done(
        &mut self,
        state: &mut State,
        capture_running: bool,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        let mut effects = self.sync(state, capture_running, now)?;
        if std::mem::take(&mut self.preset_reply)
            && let Some(preset) = working_preset(state)
        {
            let reply = preset_message(preset.to_bytes(), RecallPresetReason::Other);
            effects.extend(self.notify(Notification::Update(reply), state, now)?);
        }
        self.request = None;
        Ok(effects)
    }

    /// A capture lock change: `0x582db8` sends message 42 with `global_gate` only, then
    /// `0x582d08` with `global_eq` only; each value is `lock || inhibited`.
    pub fn capture_lock_changed(&mut self, locked: bool, state: &State) -> [Effect; 2] {
        self.capture_lock = locked;
        let inhibited = inhibited_modules(locked, state);
        self.inhibited = Some(inhibited);
        inhibited_pair(inhibited).map(|message| Effect::ControlNotify(Box::new(message)))
    }

    pub fn link(&self) -> &ControlLink {
        &self.link
    }

    /// Handshake complete (keep-alive, ModelRepo and Connection).
    pub fn connected(&self) -> bool {
        self.link.session().connected()
    }

    /// Connected and the KeepAlive says online (`0x6e0444` and `0x6e045c`, `+0x3d9`).
    pub fn online(&self) -> bool {
        self.connected() && self.link.session().online()
    }

    /// Control subscribed the I/O meters (IOMeter CREATE, `0x6e08b4`): stock then registers
    /// the I/O page word set with the meter service, so the device polls it as for the I/O
    /// page (`meters::requests`).
    pub fn io_meters_subscribed(&self) -> bool {
        self.link.state().timers.io_meters.enabled
    }

    /// Control subscribed grid meters (GridModelMeter CREATE, `0x110a70`): the 100 ms sender
    /// `0x110194` runs until DELETE All. The device polls the row meter words
    /// `0x401c..=0x401f` while this is true.
    pub fn grid_meters_subscribed(&self) -> bool {
        self.link.state().timers.grid_meters.enabled
    }

    /// GridModelMeter CREATE All or of this block (`0x4aba88`, `0x4ab4f8`): the model meter
    /// service polls the block (`meters::model_requests`).
    pub fn model_meter_subscribed(&self, slot: tetrad_engine::Slot) -> bool {
        use tetrad_control::device::MeterTarget;
        (self.link.state().grid_meters.iter()).any(|target| match *target {
            MeterTarget::All => true,
            MeterTarget::Model { row, column } => {
                usize::from(row) == slot.row.index() && usize::from(column) == slot.column.index()
            }
            MeterTarget::Row(_) => false,
        })
    }

    /// Reads what a device Directory edit will change, before it runs; nothing while Control
    /// is not connected (`0x6e0444`). Pass the result to [`Control::file_edited`].
    pub fn file_edit(&self, request: &tetrad_ui::DirectoryRequest, state: &State) -> FileEdit {
        if !self.connected() {
            return FileEdit::default();
        }
        FileEdit::before(request, state, self.media_cipher.as_ref()).unwrap_or_default()
    }

    /// The FileMessageSender pushes of a successful device Directory edit (`removed`
    /// `0x25270c`, `changed` `0x253d24`, `onFolderRenamed` `0x252c74`, `folderRemoved`
    /// `0x252310`, `folderCreated` `0x251f14`, swap `0x2519e8`). A failed listing is a notice.
    pub fn file_edited(&self, edit: FileEdit, state: &State) -> Vec<Effect> {
        edit.pushes(state, self.media_cipher.as_ref())
            .unwrap_or_else(|error| vec![Effect::Notice(Subsystem::Control, error.to_string())])
    }

    /// The File push of a product stored into Downloads (`0x250634`), while Control is
    /// connected (`0x6e0444`). The services store calls it after `store_download`.
    pub fn download_stored(
        &self,
        state: &State,
        kind: tetrad_storage::stock::DownloadKind,
        id: &str,
    ) -> Vec<Effect> {
        if !self.connected() {
            return Vec::new();
        }
        files::download_stored(state, self.media_cipher.as_ref(), kind, id)
            .unwrap_or_else(|error| vec![Effect::Notice(Subsystem::Control, error.to_string())])
    }

    /// The File push of a successful preset save (`0x2515b4`), while Control is connected.
    pub fn preset_saved(&self, state: &State) -> Vec<Effect> {
        if !self.connected() {
            return Vec::new();
        }
        files::preset_saved(state, self.media_cipher.as_ref())
            .unwrap_or_else(|error| vec![Effect::Notice(Subsystem::Control, error.to_string())])
    }

    /// The next report is preceded by the transport reset marker.
    pub fn reset_marker_pending(&self) -> bool {
        self.outgoing.marker_pending()
    }

    /// A report or the reset marker waits for [`Self::send_reports`].
    pub fn transmitting(&self) -> bool {
        self.outgoing.pending()
    }

    /// Prepares the ModelRepo payload from the decrypted `ModelRepo.xml` as `0x11e3cc`
    /// does after its digest check: scramble the `blob="…"` values (`0x11e2a0`), then a
    /// gzip-filtered pax tar with one `ModelRepo.xml` entry, mode 0644 (`0x6b4b50`).
    pub fn set_model_repo(&mut self, plain_xml: &[u8]) -> Result<(), ControlError> {
        let mut xml = plain_xml.to_vec();
        scramble_blob_names(&mut xml);
        let archive = gzip_tar("ModelRepo.xml", &xml);
        if archive.len() > MODEL_REPO_ARCHIVE_BYTES {
            return Err(ControlError::ModelRepoTooLarge);
        }
        self.model_repo = Some(archive);
        Ok(())
    }

    pub fn model_repo_payload(&self) -> Option<&[u8]> {
        self.model_repo.as_deref()
    }

    /// Installs or removes the AES-128-CTR transform directly. The device path is
    /// [`Self::complete_key_exchange`].
    pub fn set_transform(&mut self, cipher: Option<CommsCipher>) {
        self.transform = cipher;
    }

    /// The device serial (`qc_sn`), read at boot as stock sets DSPController `+0xb884`
    /// (`0x876dec`). It is the text of the Version key request and the Version
    /// `device_serial_number`.
    pub fn set_device_serial(&mut self, serial: DeviceSerial) {
        self.serial = Some(serial);
    }

    /// The DSP answer to the key request `0x42` (response `0x17`, slot `0x2a4f58` → callback
    /// `0x2a532c`). Stock connects the slot only while a Normal-mode Version READ waits
    /// (`0x2a4dd4`; `0x2a532c` disconnects it), so an answer without one does nothing. Else
    /// `material` becomes the transform key (`0x6e077c` → `+0x400`, derived per payload by
    /// `0x9f53c8`), and `key` up to its first NUL, zero padded (`0x2a5408`–`0x2a54bc`),
    /// keys the XXTEA `comms_version` of the Version reply; a Version READ follows
    /// (`ControlLink::complete_version`). Neither vector leaves this call: the cipher holds
    /// derived bytes and the XXTEA key copy is zeroed.
    pub fn complete_key_exchange(
        &mut self,
        key: &UsbMeter,
        material: &UsbMeter,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        if self.link.state().pending_version.is_none() {
            return Ok(Vec::new());
        }
        self.transform = Some(CommsCipher::derive(&material.0));
        let mut xxtea_key = Zeroizing::new(key.0);
        if let Some(end) = xxtea_key.iter().position(|&byte| byte == 0) {
            xxtea_key[end..].fill(0);
        }
        self.with_state(state, now, |link, model, stages| {
            link.complete_version(&xxtea_key, &material.0, now, model, stages)
        })
    }

    pub fn on_report(
        &mut self,
        report: &[u8; 128],
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        self.receiving = true;
        self.last_received = None;
        let effects = self.with_state(state, now, |link, model, stages| {
            link.receive_report(report, now, model, stages)
        });
        self.receiving = false;
        effects
    }

    /// A grid drop on the device (touch, or an injected [`Action::DropBlock`], also inside
    /// [`Action::Undoable`]): its GridMove echo goes out with the next [`Self::sync`], as
    /// stock sends one from `Chain::dropSlot` (`0x59f620`) for every drop. The device calls
    /// this before the action runs: the move list comes from the layout before the drop. A
    /// Cortex Control drop records its own echo (`grid::move_grid`).
    pub fn record_drop(&mut self, action: &Action, state: &State) {
        let mut action = action;
        while let Action::Undoable(_, inner) = action {
            action = inner;
        }
        if let &Action::DropBlock { from, to, side } = action
            && self.connected()
        {
            self.drop_echo = grid::DropEcho::plan(state, from, to, side);
        }
    }

    /// The message the last [`Self::on_report`] completed, if any (diagnostics).
    pub fn last_received(&self) -> Option<tetrad_control::link::Received> {
        self.last_received
    }

    /// The READ reply in progress and its folder, as `File Read part cloud-2-1` (diagnostics).
    pub fn spread_label(&self) -> String {
        self.spread
            .as_ref()
            .map_or_else(String::new, |(_, spread)| spread.to_string())
    }

    /// A File or CloudProduct READ reply is in progress ([`Self::spread_step`]).
    pub fn spreading(&self) -> bool {
        self.spread.is_some()
    }

    /// One step of the READ reply in progress (A3, `files::Spread`): at most
    /// [`READS_PER_PASS`] library reads, and the finished messages with the request ID,
    /// as the receiver would send them. The device runs one step per pass in place of the next
    /// Control message, so the reports after the READ wait, in order, until it is done.
    /// A disconnect drops the reply; a library fault drops it with a notice.
    pub fn spread_step(
        &mut self,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        let Some((request, mut spread)) = self.spread.take() else {
            return Ok(Vec::new());
        };
        if !self.connected() {
            return Ok(Vec::new());
        }
        let (messages, done) = match spread.step(state, self.media_cipher.as_ref()) {
            Ok(step) => step,
            Err(error) => {
                let notice = format!("READ reply of request {request} dropped: {error}");
                return Ok(vec![Effect::Notice(Subsystem::Control, notice)]);
            }
        };
        if !done {
            self.spread = Some((request, spread));
        }
        let outer = self.request.replace(request);
        let sent: Result<Vec<_>, _> = (messages.into_iter())
            .map(|message| self.notify(Notification::Update(message), state, now))
            .collect();
        self.request = outer;
        Ok(sent?.into_iter().flatten().collect())
    }

    /// The link timers, then the forwarded HTTP calls: expired ones fail, new ones and the
    /// next backup chunks go out (one chunk per tick, `0x1458ac`). A forward the link does not
    /// send (no transform, a send fault) fails its own call at once; the other messages and
    /// the link effects of the tick stay (review-r45 M4, M5).
    pub fn tick(&mut self, state: &mut State, now: Duration) -> Result<Vec<Effect>, ControlError> {
        let mut effects = self.with_state(state, now, |link, model, stages| {
            link.tick(now, model, stages)
        })?;
        if self.cloud.is_some_and(|(_, due)| now >= due) {
            let late = cloud::CloudBackupDone::Failed("no result before the deadline".into());
            effects.extend(self.cloud_backup_done(late, &[], state, now)?);
        }
        self.forwards.expire(now);
        if let Some(refusal) = self.options.control_cloud_refusal() {
            self.forwards.refuse_posted(refusal);
            return Ok(effects);
        }
        if !self.connected() {
            self.forwards
                .refuse_posted("Cortex Control is not connected");
            return Ok(effects);
        }
        for (key, message) in self.forwards.take(now) {
            match self.notify(Notification::Forward(message), state, now) {
                Ok(sent) => {
                    let reports = sent.iter().any(|e| matches!(e, Effect::ControlReport(_)));
                    if !reports {
                        let reason = "Cortex Control did not send the call".to_owned();
                        self.forwards.fail(&key, reason);
                    }
                    effects.extend(sent);
                }
                Err(error) => self.forwards.fail(&key, error.to_string()),
            }
        }
        Ok(effects)
    }

    /// A DSP diagnostics block (response `0x00`, slot `0x6dfe28`): the last one per core feeds
    /// Diagnostics (7).
    pub fn on_dsp_diagnostics(
        &mut self,
        core: tetrad_dsp_protocol::DspCore,
        block: &DiagnosticsBlock,
    ) {
        self.diagnostics.cores[core as usize] = Some(*block);
    }

    /// SOC2 ARM diagnostics (DSP response `0x08`): byte 0 falling from nonzero to zero is the
    /// USB unplug (`0x6e2b8c`).
    pub fn on_soc2_diagnostics(
        &mut self,
        diagnostics: &Soc2ArmDiagnostics,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        self.diagnostics.soc2 = Some(*diagnostics);
        let present = diagnostics.0[0];
        let previous = self.usb_present.replace(present);
        if previous.is_some_and(|previous| previous != 0) && present == 0 {
            return self.usb_unplugged(state, now);
        }
        Ok(Vec::new())
    }

    /// SOC2 ARM diagnostics byte 0 falling (`0x6e2b8c`): the keep-alive expiry path.
    pub fn usb_unplugged(
        &mut self,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        self.with_state(state, now, |link, model, stages| {
            link.usb_unplugged(now, model, stages)
        })
    }

    /// The factory-restore push (`0x4de79c` → `0x29a4f8(.., 1, 1)`): the listing first and
    /// CloudLogin DELETE last.
    pub fn factory_resync(
        &mut self,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        self.with_state(state, now, |link, model, stages| {
            link.factory_resync(now, model, stages)
        })
    }

    /// Stock push `0x29a4f8(.., 0, 0)` after a settings reset (factory restore uses
    /// [`Self::factory_resync`]).
    pub fn resync(
        &mut self,
        files: bool,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        self.with_state(state, now, |link, model, stages| {
            link.resync(files, now, model, stages)
        })
    }

    /// Unsolicited updates, gated by the connected Control session (`0x6e0444`). Inside a
    /// request's context they echo its request ID, as stock sends them from the receiver.
    pub fn notify(
        &mut self,
        notification: Notification,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        if let Notification::Update(ControlMessage::Updater(message)) = &notification {
            self.updater.observe(message);
        }
        // `0x55e140` case `0x12`: the driver write that pushes the quantize flag also restarts
        // the Buttons timer (`0x511458(.., −1)`), connected or not.
        if let Notification::Update(ControlMessage::Looper(message)) = &notification
            && message.quantize_enabled.is_some()
        {
            self.looper_buttons = Some(now + LOOPER_BUTTONS_DELAY);
        }
        if let Notification::Update(ControlMessage::GeneralSettings(message)) = &notification {
            if let Some(locked) = message.lock_screen_and_volume_knob {
                self.input_locked = locked;
            }
            if let Some(option) = message.power_option {
                self.standby =
                    option == tetrad_control::codecs::EnumValue::Known(PowerOptions::Standby);
            }
        }
        if !self.connected() {
            return Ok(Vec::new());
        }
        let request = self.request;
        self.with_state(state, now, |link, model, stages| match request {
            Some(id) => link.notify_for(id, notification, now, model, stages),
            None => link.notify(notification, now, model, stages),
        })
    }

    pub fn suspend(
        &mut self,
        kind: Suspension,
        reason: SuspendReason,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        self.with_state(state, now, |link, model, stages| {
            link.suspend(kind, reason, now, model, stages)
        })
    }

    pub fn resume(
        &mut self,
        reason: SuspendReason,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        self.with_state(state, now, |link, model, stages| {
            link.resume(reason, now, model, stages)
        })
    }

    /// Runs one link operation against any model with this bridge's stages, and turns its
    /// step into effects. The device uses the [`State`] model; tests use their own. A
    /// handled request opens its context; [`Effect::ControlRequestDone`] comes last.
    pub fn drive(
        &mut self,
        model: &mut dyn DeviceModel,
        operation: impl FnOnce(
            &mut ControlLink,
            &mut dyn DeviceModel,
            Stages<'_>,
        ) -> Result<Step, LinkError>,
    ) -> Result<Vec<Effect>, ControlError> {
        let stages = Stages {
            compression: Some(&self.compression),
            transform: self.transform.as_ref().map(|cipher| cipher as _),
        };
        let step = operation(&mut self.link, model, stages)?;
        self.last_received = step.received.or(self.last_received);
        // `0x17ddfc` resumes after the last outgoing chunk (`0x3fbb90(4)`).
        if step
            .notices
            .contains(&tetrad_control::link::Notice::LocalBackupSent)
        {
            self.link_op = Some(backup::LinkOp::Resume);
        }
        if step.session.contains(&SessionEvent::FlushTransmit) {
            self.outgoing.flush();
        }
        let reports = step
            .reports
            .into_iter()
            .map(|report| Effect::ControlReport(Box::new(report)));
        let notices = step
            .notices
            .into_iter()
            .map(|notice| Effect::Notice(Subsystem::Control, format!("{notice:?}")));
        let mut effects: Vec<Effect> = reports.chain(notices).collect();
        if let Some(id) = step.request {
            self.request = Some(id);
            effects.push(Effect::ControlRequestDone);
        }
        Ok(effects)
    }

    fn with_state(
        &mut self,
        state: &mut State,
        now: Duration,
        operation: impl FnOnce(
            &mut ControlLink,
            &mut dyn DeviceModel,
            Stages<'_>,
        ) -> Result<Step, LinkError>,
    ) -> Result<Vec<Effect>, ControlError> {
        let model_repo = self.model_repo.take();
        let factory = self.factory.take();
        let factory_irs = self.factory_irs.take();
        let downloads_in_flight = std::mem::take(&mut self.downloads_in_flight);
        let upload_states = std::mem::take(&mut self.upload_states);
        let cipher = self.media_cipher.take();
        let serial = self.serial.take();
        let diagnostics = std::mem::take(&mut self.diagnostics);
        let batch = self.batch.take();
        let control_online = self.link.session().online();
        let options = Rc::clone(&self.options);
        let mut model = StateModel {
            state,
            serial: serial.as_ref(),
            model_repo: model_repo.as_deref(),
            capture_lock: self.capture_lock,
            capture_running: self.capture_running,
            dirty: self.dirty,
            factory: factory.as_ref(),
            factory_irs: factory_irs.as_deref(),
            downloads_in_flight: &downloads_in_flight,
            upload_states: &upload_states,
            cipher: cipher.as_ref(),
            wall_clock: self.wall_clock,
            disk_space: self.disk_space,
            diagnostics: &diagnostics,
            effects: Vec::new(),
            volume_echo: None,
            preset_reply: false,
            drop_echo: None,
            local_backup: self.local_backup,
            cloud: self.cloud.map(|(op, _)| op),
            multi_select: self.multi_select,
            bulk: batch.as_ref().map(|seen| &seen.read),
            link_op: None,
            options: &options,
            input_locked: self.input_locked || self.standby,
            standby: self.standby,
            confirmation: None,
            asked: self.asked.as_ref().and_then(ask::Asked::open),
            updater: std::mem::take(&mut self.updater),
            control_online,
            forwarded: Vec::new(),
            spreads: self.receiving,
            spread: None,
        };
        let result = self.drive(&mut model, operation);
        if let Some(spread) = model.spread.take() {
            // A request without an ID (0) gets its reply without one.
            self.spread = Some((self.request.unwrap_or(0), spread));
        }
        self.local_backup = model.local_backup;
        self.cloud = model.cloud.map(|op| match self.cloud {
            Some((running, due)) if running == op => (op, due),
            _ => (op, now + op.deadline()),
        });
        self.link_op = model.link_op.or(self.link_op);
        let confirmation = model.confirmation.take();
        if model.asked.is_none() {
            // Answered in this step (or never sent): a new question is asked again.
            self.asked.take_if(|asked| asked.open().is_some());
        }
        self.updater = std::mem::take(&mut model.updater);
        for (message, last) in std::mem::take(&mut model.forwarded) {
            self.forwards.answer(message, last);
        }
        if let Some(volume) = model.volume_echo {
            self.volume_echo = Some((now + VOLUME_ECHO_DELAY, volume));
        }
        self.preset_reply |= model.preset_reply;
        self.drop_echo = model.drop_echo.take().or(self.drop_echo.take());
        // The model's effects (DSP, UI, settings) run before the replies, as stock applies
        // a change before it answers.
        let mut effects = model.effects;
        self.model_repo = model_repo;
        self.factory = factory;
        self.factory_irs = factory_irs;
        self.downloads_in_flight = downloads_in_flight;
        self.upload_states = upload_states;
        self.media_cipher = cipher;
        self.serial = serial;
        self.diagnostics = diagnostics;
        self.batch = batch;
        effects.extend(result?);
        if let Some(op) = self.link_op.take() {
            effects.extend(self.link_op(op, state, now)?);
        }
        if let Some(request) = confirmation {
            effects.extend(self.confirm(request, state, now)?);
        }
        let connected = self.connected();
        if !connected {
            self.forwards.disconnected();
        }
        if state.control_connected != connected {
            state.control_connected = connected;
            effects.push(Effect::Present);
            if connected {
                effects.extend(self.excess_on_connect(state, now)?);
            }
        }
        Ok(effects)
    }

    /// A device dialog shown in Cortex Control as a Confirmation (47) (`0xfa48c`), sent after
    /// the step that opened it; connected only.
    fn confirm(
        &mut self,
        request: tetrad_control::link::ConfirmationRequest,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        self.with_state(state, now, |link, model, stages| {
            let (_, step) = link.request_confirmation(request, now, model, stages)?;
            Ok(step)
        })
    }

    /// The RemoteControl (72) READ screenshot reply (`0x4a0480`): the rendered UI frame
    /// ([`Effect::ControlFrame`], packed RGB at the tetrad-ui size) as a PNG in UPDATE
    /// {`screenshot.payload`}. The request's `x`, `y`, `w`, `h` are ignored, as stock does.
    pub fn send_frame(
        &mut self,
        rgb: &[u8],
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        let Some(png) = tetrad_ui::png(
            tetrad_ui::WIDTH,
            tetrad_ui::HEIGHT,
            tetrad_ui::PngColour::Rgb,
            rgb,
        ) else {
            let notice = format!(
                "a UI frame of {} bytes is not {}x{} RGB",
                rgb.len(),
                tetrad_ui::WIDTH,
                tetrad_ui::HEIGHT
            );
            return Ok(vec![Effect::Notice(Subsystem::Control, notice)]);
        };
        let reply = tetrad_control::codecs::RemoteControlMessage {
            action: Some(MessageAction::Update.into()),
            screenshot: Some(tetrad_control::codecs::RemoteControlScreenshot {
                payload: Some(png),
                ..Default::default()
            }),
            ..Default::default()
        };
        self.notify(Notification::Update(reply.into()), state, now)
    }

    /// Queues one report to the host ([`Effect::ControlReport`]).
    pub fn queue_report(&mut self, report: Box<[u8; 128]>) {
        self.outgoing.push(report);
    }

    /// Builds the ModelPreset (71) READ list now ([`State::model_preset_list`]): the device
    /// calls it at boot, so the connect burst sends the list without building it (A3: the QC
    /// took 118 ms per build; stock builds it in the READ, `0x1d037c`).
    pub fn build_model_preset_list(&self, state: &mut State) -> Result<(), ModelError> {
        models::read_list(state, self.factory.as_ref()).map(drop)
    }

    /// The reports that wait in the queue.
    pub fn reports_waiting(&self) -> usize {
        self.outgoing.report_count()
    }

    /// Drops every queued frame (no DSP to send to); returns the report count.
    pub fn drop_reports(&mut self) -> usize {
        std::mem::take(&mut self.outgoing).report_count()
    }

    /// Sends up to `limit` queued frames as `0x27` frames, in order; a pending reset marker
    /// goes first. A failed send keeps its frame first for the next call; only the first
    /// failure of a run is returned.
    pub fn send_reports<P: Platform>(
        &mut self,
        dsp: &mut Dsp<P>,
        limit: usize,
    ) -> Result<(), ControlError> {
        let diagnostics = &mut self.diagnostics;
        let sent = self.outgoing.send(limit, |frame| match frame {
            outgoing::Frame::Marker => {
                let marker = reset_marker_frame();
                let fragment = Fragment::decode(&marker, CodecLimits::default())
                    .expect("the reset marker is one valid core-4 fragment");
                dsp.send(ControlPacket::Fragment(fragment), HID_SEND)
            }
            // `0x6dfbf8` counts each failed HID write (`+0x260`, Diagnostics field 8) and
            // each sent frame whose byte 2 is not `0xff` (`+0x3f8`, a wrapping u32): the
            // marker is not counted.
            outgoing::Frame::Report(report) => {
                let result = dsp.send(ControlPacket::Hid(&HidReportFrame::new(report)), HID_SEND);
                match &result {
                    Ok(()) => diagnostics.hid_tx_sent = diagnostics.hid_tx_sent.wrapping_add(1),
                    Err(_) => diagnostics.hid_tx_failures += 1,
                }
                result
            }
        });
        sent.error.map_or(Ok(()), |error| Err(error.into()))
    }

    /// The sent HID report count of the USB Statistics page (row 3, "UI to DSP HID count").
    pub fn hid_reports_sent(&self) -> u32 {
        self.diagnostics.hid_tx_sent
    }
}

/// Message 42 values (`0x15ac7c`): the capture lock forces both, else the fit of the
/// loaded preset dropped them (`+0x3408`, `+0x3410`).
fn inhibited_modules(locked: bool, state: &State) -> (bool, bool) {
    let inhibited = state
        .current
        .as_ref()
        .map(|current| current.deployment.plan().inhibited)
        .unwrap_or_default();
    (
        locked || inhibited.input_gate,
        locked || inhibited.global_eq,
    )
}

/// Message 42 as two pushes: `global_gate` only, then `global_eq` only (`0x15bd68`).
fn inhibited_pair((gate, eq): (bool, bool)) -> [ControlMessage; 2] {
    let message = |global_gate, global_eq| {
        ControlMessage::CompilerInhibitedModules(CompilerInhibitedModulesMessage {
            action: Some(MessageAction::Update.into()),
            global_gate,
            global_eq,
            ..Default::default()
        })
    };
    [message(Some(gate), None), message(None, Some(eq))]
}

/// The Control push of a device action that succeeded: a UI pin pushes a one-model
/// PinnedModels CREATE or DELETE (`0x1304c8`, `0x1300a4`); a device name change pushes
/// Version UPDATE with only `custom_name` (`customNameChanged` → `0x13cec8`, empty when
/// cleared). Cortex Control's own changes push from their receivers.
pub fn action_push(action: &Action) -> Option<Effect> {
    let message: ControlMessage = match action {
        Action::PinModel { model, pinned } => tetrad_control::codecs::PinnedModelsMessage {
            action: Some(
                if *pinned {
                    MessageAction::Create
                } else {
                    MessageAction::Delete
                }
                .into(),
            ),
            models: vec![*model],
            ..Default::default()
        }
        .into(),
        Action::SetDeviceName(name) => VersionMessage {
            action: Some(MessageAction::Update.into()),
            custom_name: Some(name.clone().unwrap_or_default()),
            ..Default::default()
        }
        .into(),
        _ => return None,
    };
    Some(Effect::ControlNotify(Box::new(message)))
}

/// Lane edit echo `0x25ea1c(row, parameter)`: a Grid UPDATE with one chain (`row`) and one
/// lane output control parameter holding the current-scene value. The sender returns while
/// a capture preset runs (`+0x758`); the caller sends only for the user preset.
pub fn lane_echo(row: Row, parameter: u32, value: f32) -> ControlMessage {
    let preset = Preset {
        chains: vec![Chain {
            row: Some(row.index() as u32),
            output_control: vec![Model {
                params: vec![Param {
                    index: Some(parameter),
                    values: vec![SceneValue {
                        value: Some(ParamValue::Float(value)),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    ControlMessage::Grid(GridMessage {
        action: Some(MessageAction::Update.into()),
        preset: Some(RawMessageBytes {
            bytes: preset.to_bytes(),
        }),
        ..Default::default()
    })
}

/// After an undo or redo restore (`0x649330`, `0x649b7c` case 0): the frame lists with the
/// restored frame still on its stack (`0x649120` emits them before the restore; redo is
/// inferred the same), RecallPreset with reason Undo (`0x2778d4(.., 1)`), Scene (`0x27c3fc`)
/// and SetlistPosition (`0x284bc8` → `0x27ed1c`). The preset is the restored working
/// preset with its library item ([`item_preset`]); `before` are the stack depths before the
/// step, and `state.history` is the history after it (stepped, or unchanged when the restore
/// failed). Both preset frames are type 0. [`Control::sync`] then sends the
/// final lists (`0x1be1a8` after the restore, `0x5b2a20`).
pub(super) fn undo_pushes(state: &State, before: (usize, usize), redo_step: bool) -> [Effect; 4] {
    let (mut undo, mut redo) = state.history.frame_types();
    let (from, to) = if redo_step {
        (&mut redo, &mut undo)
    } else {
        (&mut undo, &mut redo)
    };
    if state.history.depths() == before {
        // Not stepped: the working preset still goes onto the other stack.
        to.push(0);
        if to.len() > preset::HISTORY_DEPTH {
            to.remove(0);
        }
    } else {
        // Stepped: the restored frame is listed on its stack again.
        from.push(0);
    }
    let frames = UndoFrames { undo, redo };
    let [recall, scene, position] = preset_trio(state, RecallPresetReason::Undo);
    [
        Effect::ControlNotification(Box::new(Notification::UndoFrames(frames))),
        recall,
        scene,
        position,
    ]
}

/// The pushes of a QC-side preset change, in stock order: RecallPreset (`0x27788c` →
/// `0x277468`) with `reason` and the library item ([`item_preset`]), Scene ([`scene_push`]),
/// then SetlistPosition (`0x284bc8` → `0x27ed1c`). Reasons: a load (`0x5b2f40`, MIDI PC,
/// factory restore) is Other, an undo or redo is Undo, a save into the loaded slot
/// (`0x5b7438`, Edit Details `0x5b91dc`) is Save.
///
/// A load (reason Other) deploys, so its pushes also carry the deploy's side effects (stock
/// `[1750]`–`[1773]`): first the Looper X driver writes ([`looper_writes`]), and after the
/// trio one `input_control` push (signal 28 from the deploy's re-route,
/// [`pushes::input_sources`]). Not built: the row input port push `0x25e070` (sent by the
/// row I/O view `0x5a3a3c` when its shown port changes, `[1754]`) and the EXP link pushes of
/// `[1760]`–`[1762]` (sender not located).
///
/// Before all of them go the frame lists (A12): the load callers that record a load frame
/// (`0x5b3810`, `0x5b38c8`, `0x5b57b8`, a save into another slot `0x5b7438`) call
/// `0x648f1c(.., 5, ..)` before `0x5b2f40`, a delete of the loaded preset records its frame
/// before the template load (`0x1d916c`: `0x648f1c(.., 1, ..)`, then `0x7c4c1c` →
/// `0x5b2c54`), and the frame signals reach `0x1be1a8` by a
/// direct connection (`0x1be14c`). A load with no new frame (MIDI PC `0x5b5bd8` records
/// none; S7 `[1750]`–`[1776]` has no UndoRedo) sends no list: tetrad-control drops a list equal
/// to the last one sent.
pub(super) fn preset_pushes(state: &State, reason: RecallPresetReason) -> Vec<Effect> {
    let trio = preset_trio(state, reason);
    if reason != RecallPresetReason::Other {
        return trio.into();
    }
    let frames = Notification::UndoFrames(undo_frames(&state.history));
    let writes = looper_writes(state).into_iter();
    let sources = pushes::input_sources(state).into_iter();
    std::iter::once(Effect::ControlNotification(Box::new(frames)))
        .chain(writes.map(|message| Effect::ControlNotify(Box::new(message))))
        .chain(trio)
        .chain(sources.map(|message| Effect::GridEcho(Box::new(message))))
        .collect()
}

/// The pushes of `0x5b2ce8` (a slot frame step that changed the selected slot): RecallPreset
/// with reason Undo and the library item, then the Scene; no SetlistPosition.
pub(super) fn restore_pushes(state: &State) -> [Effect; 2] {
    let [recall, scene, _] = preset_trio(state, RecallPresetReason::Undo);
    [recall, scene]
}

/// RecallPreset, Scene and SetlistPosition of [`preset_pushes`].
fn preset_trio(state: &State, reason: RecallPresetReason) -> [Effect; 3] {
    let preset = item_preset(state).unwrap_or_default();
    let notify = |message| Effect::ControlNotify(Box::new(message));
    [
        notify(preset_message(preset, reason)),
        scene_push(state),
        notify(ControlMessage::SetlistPosition(SetlistPositionMessage {
            action: Some(MessageAction::Update.into()),
            ..setlist_position(state)
        })),
    ]
}

/// The SceneMessageSender push `0x27c3fc`: Scene UPDATE with the playing scene (`0x589a30`).
/// Stock `setScene` `0x5899d8` sends it on every change and every forced select, before the
/// scene-changed signal; a preset load sends it through the forced select of `0x5b2de4`.
/// tetrad-control drops it while Control is not connected (`0x6e0444`) or the sender is off.
pub(super) fn scene_push(state: &State) -> Effect {
    let scene = state
        .current
        .as_ref()
        .map_or(0, |current| current.deployment.scene().index() as u32);
    Effect::ControlNotification(Box::new(Notification::Scene(scene)))
}

/// The Control pushes of a scene recall, in stock order (`[1647]`–`[1666]`): Scene
/// ([`scene_push`], before the scene-changed signal), the Looper X driver writes
/// ([`looper_writes`]), the recall's Grid echoes ([`pushes::recall_echoes`]), then one
/// `input_control` push per recalled sidechain source ([`pushes::recalled_sources`]). The
/// Grid pushes are [`Effect::GridEcho`]: none while a capture preset runs (`+0x758`).
pub(super) fn scene_pushes(state: &State) -> Vec<Effect> {
    let grid = |message| Effect::GridEcho(Box::new(message));
    let sources = pushes::input_sources(state).map_or_else(Vec::new, |message| {
        vec![message; pushes::recalled_sources(state)]
    });
    std::iter::once(scene_push(state))
        .chain(
            looper_writes(state)
                .into_iter()
                .map(|m| Effect::ControlNotify(Box::new(m))),
        )
        .chain(pushes::recall_echoes(state).into_iter().map(grid))
        .chain(sources.into_iter().map(grid))
        .collect()
}

/// The working preset as `0x277468` sends it: UPDATE with `reason` always set. Its
/// `default_scene` is the stored scene: stock `0x67df1c` copies `+0x1520`, which only the
/// parser (`0x681d44`) and a save (`0x5b7438` → `0x514330`) write, not a scene change. So
/// each device push of it is followed by Scene ([`scene_push`]).
fn preset_message(preset: Vec<u8>, reason: RecallPresetReason) -> ControlMessage {
    ControlMessage::RecallPreset(RecallPresetMessage {
        action: Some(MessageAction::Update.into()),
        preset: Some(RawMessageBytes { bytes: preset }),
        reason: Some(reason.into()),
        ..Default::default()
    })
}

/// The working preset with the runtime fields of the deploy ([`pushes::deploy_fields`]).
fn working_preset(state: &State) -> Option<Preset> {
    let mut preset = state.current.as_ref()?.preset.clone();
    pushes::deploy_fields(&mut preset, state);
    Some(preset)
}

/// The working preset with the library item of its file, as READ (`0x274220` action 3) and
/// `0x2778d4` (resync, undo pushes) send it: `0x7c2bdc` → `0x67c368` takes the `.json`
/// sidecar's `date` (`+0x234`, always; empty when absent), then `cloud_id` (`+0xf0`) and
/// `description` (`+0x100`) as one entry each when not empty, and `tags` (`+0x20`) when not
/// empty, each replacing the preset's own. No file or no sidecar: no item, the preset alone.
/// The deferred UPDATE/CREATE reply carries no item (`0x277468(.., 0, 0)`); a refused
/// GridMove pushes the item (`0x2778d4(.., 0)`).
/// Inferred: the item date reads back in the format it was written in.
fn item_preset(state: &State) -> Option<Vec<u8>> {
    let current = state.current.as_ref()?;
    let mut preset = working_preset(state)?;
    let sidecar = (current.path.as_deref())
        .zip(state.library.as_ref())
        .and_then(|(path, library)| library.presets.sidecar(path).ok().flatten());
    let Some(sidecar) = sidecar else {
        return Some(preset.to_bytes());
    };
    preset.date = Some(sidecar.text("date").unwrap_or_default().to_owned());
    for (key, field) in [
        ("cloudId", &mut preset.cloud_id),
        ("description", &mut preset.description),
    ] {
        if let Some(text) = sidecar.text(key).filter(|text| !text.is_empty()) {
            *field = vec![text.to_owned()];
        }
    }
    let tags = sidecar.texts("tags");
    if !tags.is_empty() {
        preset.tags = tags.into_iter().map(str::to_owned).collect();
    }
    Some(preset.to_bytes())
}

/// `0x27ed1c` by the container of the current item (`0x7c4ba4`):
/// - Downloads (`0x7c2a48`, the downloads container `0x7657e0`): `is_downloads` and
///   `key_in_downloads` = the item key (`0x7c2944`) only;
/// - a plugin folder (`0x7c2a0c`, a dynamic cast of the container): `is_plugin`,
///   `folder_key` (container vfunc `+0x34`) and `key_in_plugin_folder` = the item key;
/// - else a setlist: `folder_key` (vfunc `+0x34`), `position` (`0x7c28cc`) and `is_factory`
///   (always set, has-bit `0x20`: the container is the factory list `0x765508`).
///
/// Tetrad: the Downloads and plugin branches follow the current path
/// ([`super::media::auditioned_path`]); the setlist branch follows the last-preset setting.
/// The factory `folder_key` `/opt/neuraldsp/Factory Library` matches stock `[1758]`.
/// Inferred: the item key of a plugin preset is its path and its folder key the parent
/// directory; a stored download's key is its product id.
fn setlist_position(state: &State) -> SetlistPositionMessage {
    let path = state
        .current
        .as_ref()
        .and_then(|current| current.path.as_deref());
    if let Some(id) = path.and_then(|path| path.strip_prefix(super::media::DOWNLOADS_PRESETS))
        && let Some(id) = id.strip_prefix('/')
    {
        return SetlistPositionMessage {
            is_downloads: Some(true),
            key_in_downloads: Some(id.to_owned()),
            ..Default::default()
        };
    }
    if let Some(path) = path.filter(|path| super::media::auditioned_path(path)) {
        let folder = path.rsplit_once('/').map_or("", |(folder, _)| folder);
        return SetlistPositionMessage {
            is_plugin: Some(true),
            folder_key: Some(folder.to_owned()),
            key_in_plugin_folder: Some(path.to_owned()),
            ..Default::default()
        };
    }
    let last = &state.settings.settings().last_preset;
    SetlistPositionMessage {
        folder_key: last.setlist.clone(),
        position: u32::try_from(last.preset_index).ok(),
        is_factory: Some(last.setlist.as_deref() == Some(super::FACTORY_SETLIST)),
        ..Default::default()
    }
}

/// UndoRedo READ (`0x1ba218`) and push (`0x1be1a8`): one frame type (`0x6476c8`) per frame,
/// oldest first (`preset::UndoFrame::stock_type`).
fn undo_frames(history: &preset::History) -> UndoFrames {
    let (undo, redo) = history.frame_types();
    UndoFrames { undo, redo }
}

/// The MasterVolume echo `0x19af74`: UPDATE with the level only.
pub(super) fn master_volume_echo(volume: f32) -> ControlMessage {
    ControlMessage::MasterVolume(MasterVolumeMessage {
        action: Some(MessageAction::Update.into()),
        volume: Some(volume),
        ..Default::default()
    })
}

/// A Preset MIDI Out edit (`0x5146d4` on-load list, `0x514784` per source): MIDISettings
/// UPDATE with one `general_midi_messages` entry (has-bit 2, read back by `0x1912d4`) that
/// holds the stored 12-message list. Only a source list sets `source`; zero fields are not
/// written, as in the proto3 entries stock fills.
pub(super) fn midi_out_edit(list: MidiList, messages: &[tetrad_preset::MidiMessage]) -> Effect {
    let field = |value: u32| (value != 0).then_some(value);
    let messages = messages
        .iter()
        .cloned()
        .chain(std::iter::repeat_with(tetrad_preset::MidiMessage::default))
        .take(MIDI_MESSAGES)
        .map(|message| MidiMessageInfo {
            message_type: field(message.kind & 0xff),
            channel: field(message.channel),
            param1: field(message.param1),
            param2: field(message.param2),
            param3: field(message.param3),
        })
        .collect();
    let source = match list {
        MidiList::OnLoad => None,
        MidiList::Group(group) => u32::try_from(group).ok(),
    };
    Effect::ControlNotify(Box::new(ControlMessage::MIDISettings(
        MIDISettingsMessage {
            action: Some(MessageAction::Update.into()),
            general_midi_messages: Some(GeneralMIDIMessages {
                messages: vec![GeneralMIDIMessage { source, messages }],
            }),
            ..Default::default()
        },
    )))
}

/// MasterVolume `engaged` alone (`0x19b518`): false when Control takes the level, true when
/// the knob picks it up again (`0x1fb980`).
pub(super) fn engaged_notice(engaged: bool) -> Effect {
    Effect::ControlNotify(Box::new(ControlMessage::MasterVolume(
        MasterVolumeMessage {
            action: Some(MessageAction::Update.into()),
            engaged: Some(engaged),
            ..Default::default()
        },
    )))
}

/// Be Right Back enter and exit (`0x6a82ac`, last step) → `0x16b758`: GeneralSettings UPDATE
/// with `power_option` Standby or WakeUp.
pub(super) fn standby_notice(standby: bool) -> Effect {
    let option = if standby {
        PowerOptions::Standby
    } else {
        PowerOptions::WakeUp
    };
    Effect::ControlNotify(Box::new(ControlMessage::GeneralSettings(
        GeneralSettingsMessage {
            action: Some(MessageAction::Update.into()),
            power_option: Some(option.into()),
            ..Default::default()
        },
    )))
}

/// LOCK on and off (`0x1ec83c`, `0x7d4a94`, `0x7d4bc4`) → `0x16ad60`: GeneralSettings
/// UPDATE with `lock_screen_and_volume_knob` (review r11 N17).
pub(super) fn lock_notice(locked: bool) -> Effect {
    Effect::ControlNotify(Box::new(ControlMessage::GeneralSettings(
        GeneralSettingsMessage {
            action: Some(MessageAction::Update.into()),
            lock_screen_and_volume_knob: Some(locked),
            ..Default::default()
        },
    )))
}

/// Mode READ (`0x271aa0`) and push (`0x2734e8`): the mode, and the ATMA page on ATMA only.
fn mode_message(state: &State) -> ModeMessage {
    let mode = &state.settings.settings().mode;
    ModeMessage {
        mode: u32::try_from(mode.mode().to_stock()).ok(),
        atma_page: u32::try_from(mode.atma_page)
            .ok()
            .filter(|_| state.platform.is_atma()),
        ..Default::default()
    }
}

/// The available list (`0x58b920`), as READ and `0x273854` send it.
fn available_modes(mode: &ModeSettings) -> AvailableModes {
    AvailableModes {
        modes: mode
            .available()
            .iter()
            .filter_map(|mode| u32::try_from(mode.to_stock()).ok())
            .collect(),
    }
}

/// `0x58bcbc`: values of 10 and up are skipped; modes 3..=8 take two of the three slots,
/// others one, and the list ends before the entry that overflows them. Owned: an empty
/// result is refused (stock stores it), and 9 has no `FootswitchMode`, so it is dropped
/// after taking its slot.
fn sanitized_modes(modes: &[u32]) -> Vec<FootswitchMode> {
    let mut slots = 0;
    let mut sanitized = Vec::new();
    for &mode in modes.iter().filter(|&&mode| mode < 10) {
        slots += if (3..=8).contains(&mode) { 2 } else { 1 };
        if slots > 3 {
            break;
        }
        sanitized.extend(footswitch_mode(mode).ok());
    }
    sanitized
}

/// Stock mode numbers 0..=8.
fn footswitch_mode(mode: u32) -> DeviceResult<FootswitchMode> {
    i32::try_from(mode)
        .ok()
        .and_then(|mode| FootswitchMode::from_stock(mode).ok())
        .ok_or_else(|| ModelError::Refused(CortexMessageType::Mode, format!("mode {mode}")))
}

/// The user captures of the media listing ("My Captures": product id and name), for
/// [`refresh_capture_names`].
pub(super) fn capture_names(media: &[tetrad_ui::MediaEntry]) -> BTreeMap<String, String> {
    media
        .iter()
        .filter(|entry| entry.folder == tetrad_ui::MediaFolder::MyCaptures)
        .map(|entry| (entry.value.clone(), entry.name.clone()))
        .collect()
}

/// The library pass `0x7c2da4` over a working preset: a capture block whose `file_name`
/// (parameter 5, scene 0) has at least 64 UTF-16 units splits into the key and the stored
/// name; a key in the library with another name takes `key + name` in all eight scenes. A
/// key the library does not hold keeps its text. The runtime capture version that stock
/// sets on the slot (`0x2e1dd4`) is read again at deploy. Owned: a surrogate pair is never
/// split. Only user captures are named (the listing holds no downloads).
///
/// Stock `0x527d80` runs it on every working preset it takes (load, undo and redo, recall),
/// after the import `0x681d44` and the push; Tetrad runs it before `preset::prepare`, so the
/// stored preset and the layout agree. The DSP reads only the 64-unit key, so the push is
/// the same.
pub(super) fn refresh_capture_names(preset: &mut Preset, names: &BTreeMap<String, String>) {
    const FILE_NAME: usize = 5;
    let blocks = preset.chains.iter_mut().flat_map(|chain| &mut chain.models);
    for block in blocks.filter(|block| {
        block
            .hash
            .is_some_and(|hash| CAPTURE_MODELS.contains(&hash))
    }) {
        let Some(param) = block.params.get_mut(FILE_NAME) else {
            continue;
        };
        let Some(ParamValue::String(value)) = param.value(0) else {
            continue;
        };
        let Some((key, stored)) = split_capture_key(value) else {
            continue;
        };
        let Some(name) = names.get(key).filter(|name| name.as_str() != stored) else {
            continue;
        };
        let text = ParamValue::String(format!("{key}{name}"));
        param
            .values
            .resize(tetrad_preset::SCENES, SceneValue::default());
        for scene in &mut param.values {
            scene.value = Some(text.clone());
        }
    }
}

/// A capture `file_name` with at least 64 UTF-16 units: the library key `left(64)`
/// ([`preset::capture_key`], one rule for every reader) and the stored name `mid(64)`.
fn split_capture_key(value: &str) -> Option<(&str, &str)> {
    let units: usize = value.chars().map(char::len_utf16).sum();
    (units >= CAPTURE_KEY_UNITS).then(|| value.split_at(preset::capture_key(value).len()))
}

mod account;
mod ask;
mod backup;
mod bulk;
pub mod cloud;
mod files;
pub mod forward;
mod grid;
pub mod io;
mod library;
mod looper;
mod model;
mod models;
mod nc1;
mod nc2;
pub mod outgoing;
mod products;
mod pushes;
mod resync;
pub(in crate::device) mod screen;
mod settings;
mod status;
mod transport;
mod updater;

pub use ask::UploadAsk;
pub use cloud::{CloudBackupDone, CloudBackupRequest};
pub(super) use files::swap_push;
pub use files::{FileEdit, READS_PER_PASS};
pub(super) use grid::expression_echo;
pub(super) use library::list_pushes;
use looper::{LooperSeen, looper_pushes, looper_status, looper_update, looper_writes};
use model::StateModel;
pub use nc1::nc1_message;
pub use nc2::nc2_message;
pub use status::StatsBase;
pub use transport::{CommsCipher, Zlib, reset_marker_frame};
use transport::{gzip_tar, scramble_blob_names};

#[cfg(test)]
mod tests;

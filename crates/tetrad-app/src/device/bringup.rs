//! Bring-up runs (`tetrad bringup`): the first live tests of the device program on a QC, one
//! stage per run. Every risky option is off by default.
//!
//! - [`RunOptions`]: stage gate, no-persist, services, cloud over Cortex Control, mute hold and
//!   probe, power mode.
//!   `tetrad run` uses [`RunOptions::STOCK`] (stock behaviour, refuses nothing).
//! - [`Bringup`]: the platform decorator (trace, unmute hold, would-write settings, last-line
//!   refusals, start-up stop, boot evidence).
//! - [`Device::bringup`]: start-up under `catch_unwind` (review B9), then
//!   [`Device::run_bringup`]: the loop with the time limit, the stop file, the health line,
//!   the panic catch and the test exit; [`Heartbeat`] feeds the guard thread in `main`.
//! - [`FaultWatch`]: from stage `dsp` on, the core-0 kernel fault count, first read after the
//!   DSP boot and before the preset load (`FAULTS boot`, B5), then at the loop start, in every
//!   health line and at the exit; a change fails the stage.

pub mod faults;
pub mod journal;
pub mod platform;

use std::any::Any;
use std::collections::BTreeSet;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tetrad_catalog::owned::OwnedModel;
use tetrad_dsp_host::drain::DrainMode;
use tetrad_dsp_host::session::{Clock, SessionError};
use tetrad_dsp_protocol::responses::Soc2ArmDiagnostics;
use tetrad_hal::hal::{Board, Command as HalCommand, Hal, Query};
use tetrad_hal::zenjack::{Control, Input, InputType, Value};
use tetrad_settings::io::ConnectorId;
use tetrad_ui::{InputJack, SettingId, UpdaterCommand};
use tetrad_watchdog::power::{PowerReport, PowerStep};

pub use faults::{FAULT_COUNT_ADDRESS, FaultWatch};
pub use platform::{Bringup, Launch, SUMMARY, WOULD_WRITE};

use super::control::AccountRequest;
use super::dsp::{self, DspError};
use super::jacks::InputChange;
use super::services::{CloudRequest, ServiceJob};
use super::ui::Handoff;
use super::{
    Action, Device, DeviceError, DspCommand, Effect, Platform, PowerAction, StartupError, preset,
};

/// Touch this file to end a bring-up run (`ssh qc touch /tmp/tetrad.stop`).
pub const STOP_FILE: &str = "/tmp/tetrad.stop";
pub const STOP_POLL: Duration = Duration::from_millis(250);
pub const HEALTH_PERIOD: Duration = Duration::from_secs(10);
/// Guard: the oldest loop heartbeat allowed after start-up …
pub const LOOP_STALL: Duration = Duration::from_secs(10);
/// … and during start-up, per splash phase (above the 20 s SOC2 wait).
pub const STARTUP_STALL: Duration = Duration::from_secs(40);
/// Guard: the abort this long after the time limit.
pub const OVERRUN: Duration = Duration::from_secs(30);
/// The mute probe (S4a): stock `0x20` values only, one per [`MUTE_PROBE_STEP`].
pub const MUTE_PROBE: [u32; 6] = [1, 0, 1, 3, 0, 1];
pub const MUTE_PROBE_STEP: Duration = Duration::from_secs(5);
/// Process exit code after a panic in the loop.
pub const PANIC_EXIT: u8 = 3;
/// The SOC2 USB statistics line (A14) at most this often.
pub const USB_PERIOD: Duration = Duration::from_secs(1);

/// How far a run goes. Each stage includes the one before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Stage {
    /// Platform, display, input, HAL, settings and library reads; no DSP.
    Probe,
    /// SHARC boot, keys, catalog and globals; no preset.
    Dsp,
    /// Deploys `--preset` at start-up; no other preset change.
    Deploy,
    /// Stock order: `--preset` or `LastPreset`, preset changes allowed.
    Full,
}

impl Stage {
    pub fn boots_dsp(self) -> bool {
        self >= Self::Dsp
    }

    pub fn deploys(self) -> bool {
        self >= Self::Deploy
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Probe => "probe",
            Self::Dsp => "dsp",
            Self::Deploy => "deploy",
            Self::Full => "full",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PowerMode {
    /// The bring-up test exit for every power action: no system power step.
    Test,
    /// The stock teardown (`Device::power`): unmount, then `CZenjack` shutdown or restart.
    Stock,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOptions {
    pub stage: Stage,
    /// Settings, library and preset writes reach the real files.
    pub persist: bool,
    /// Wi-Fi, internet probe, NTP and the services UI. Service files are not redirected.
    pub services: bool,
    /// Every `0x20` unmute is held muted.
    pub keep_muted: bool,
    /// The timed `0x20` sequence [`MUTE_PROBE`] after start-up; other unmutes are held.
    pub mute_probe: bool,
    /// 48 V phantom power may reach the input jacks.
    pub allow_phantom: bool,
    /// A `.pb` device path deployed at start-up instead of `LastPreset`.
    pub preset: Option<String>,
    pub power: PowerMode,
    /// Bring-up runs always have one; `run` has none.
    pub log_dir: Option<PathBuf>,
    /// DSP ring, model-window and HAL command trace.
    pub trace: bool,
    /// HAL, touch and MIDI input and DIN MIDI output in the trace.
    pub log_input: bool,
    /// Device time from the platform open to the test exit.
    pub time_limit: Option<Duration>,
    /// Stock models deployed as their owned blocks (`owned::install`), on DSP core 0 only.
    /// Empty (off) by default; only a bring-up run with `allow_owned` accepts it.
    pub owned: BTreeSet<OwnedModel>,
    /// A bring-up run may deploy `owned`; without it a bring-up refuses the map.
    pub allow_owned: bool,
    /// Cloud calls may go over Cortex Control (forward 29, 30, 31) when it is connected and
    /// online, as stock routes them (`0x80da7c`, re-r44 §3). Off in a bring-up run, so proof H1
    /// tests the Wi-Fi path first ([`Self::control_cloud_refusal`]).
    pub control_cloud: bool,
    /// Drill: the first DSP boot reports core 0 silent once after the handshake, so the SOC1
    /// retry (`CORE0_SILENT`) runs on the hardware. Bring-up DSP stages only.
    pub drill_core0: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("bring-up options: {0}")]
pub struct OptionsError(&'static str);

impl RunOptions {
    /// Stock behavior; nothing is refused. `tetrad run` and the host simulation run with it.
    pub const STOCK: Self = Self {
        stage: Stage::Full,
        persist: true,
        services: true,
        keep_muted: false,
        mute_probe: false,
        allow_phantom: true,
        preset: None,
        power: PowerMode::Stock,
        log_dir: None,
        trace: false,
        log_input: false,
        time_limit: None,
        owned: BTreeSet::new(),
        allow_owned: false,
        control_cloud: true,
        drill_core0: false,
    };

    /// A bring-up run with every risky option off.
    pub fn bringup(stage: Stage, time_limit: Duration, log_dir: PathBuf) -> Self {
        Self {
            stage,
            persist: false,
            services: false,
            keep_muted: false,
            mute_probe: false,
            allow_phantom: false,
            preset: None,
            power: PowerMode::Test,
            log_dir: Some(log_dir),
            trace: false,
            log_input: false,
            time_limit: Some(time_limit),
            owned: BTreeSet::new(),
            allow_owned: false,
            control_cloud: false,
            drill_core0: false,
        }
    }

    pub fn is_bringup(&self) -> bool {
        self.log_dir.is_some()
    }

    /// Combinations the stage plan does not allow.
    pub fn validate(&self) -> Result<(), OptionsError> {
        let error = match self {
            // Owned blocks run only in bring-up until proof H1 passes (review-w4-dsp F2).
            Self {
                log_dir: None,
                owned,
                ..
            } if !owned.is_empty() => "owned blocks run only in a bring-up run",
            Self {
                log_dir: None,
                drill_core0: true,
                ..
            } => "--drill-core0 runs only in a bring-up run",
            Self { log_dir: None, .. } => return Ok(()),
            Self {
                drill_core0: true,
                stage,
                ..
            } if !stage.boots_dsp() => "--drill-core0 needs a DSP stage",
            Self {
                time_limit: None, ..
            } => "a bring-up run needs a time limit",
            Self {
                stage: Stage::Deploy,
                preset: None,
                ..
            } => "stage deploy needs --preset",
            Self {
                preset: Some(_),
                stage,
                ..
            } if !stage.deploys() => "--preset needs stage deploy or full",
            Self {
                mute_probe: true,
                keep_muted: true,
                ..
            } => "--mute-probe and --keep-muted exclude each other",
            Self {
                mute_probe: true,
                stage,
                ..
            } if !stage.deploys() => "--mute-probe needs stage deploy or full",
            // Review bring-up safety 2 R10: a standby wake or a capture entry can send `0x20`
            // value 0.
            Self {
                stage: Stage::Dsp,
                keep_muted: false,
                ..
            } => "stage dsp needs --keep-muted",
            Self {
                allow_owned: false,
                owned,
                ..
            } if !owned.is_empty() => "--owned needs --allow-owned in a bring-up run",
            Self { persist: true, .. }
            | Self { services: true, .. }
            | Self {
                allow_phantom: true,
                ..
            }
            | Self {
                control_cloud: true,
                ..
            } if self.stage != Stage::Full => {
                "--persist, --services, --allow-phantom and --control-cloud need stage full"
            }
            Self {
                control_cloud: true,
                services: false,
                ..
            } => "--control-cloud needs --services",
            _ => return Ok(()),
        };
        Err(OptionsError(error))
    }

    /// What refuses a cloud call over Cortex Control, or `None` when it may go there: a
    /// bring-up run without `--control-cloud`. `Control::cloud_route` then gives the services
    /// no Control route (Wi-Fi only), and the Control bridge fails any forward posted anyway.
    /// The flag lifts no other rule: cloud work still needs `--services` and account writes
    /// `--persist` ([`Self::refusal`], [`Self::handoff_refusal`]).
    pub fn control_cloud_refusal(&self) -> Option<&'static str> {
        (self.is_bringup() && !self.control_cloud)
            .then_some("cloud over Cortex Control in a bring-up run without --control-cloud")
    }

    /// What bring-up refuses in a UI handoff, or `None`. Without `--services`: every services
    /// handoff. Without `--persist`: the cloud account writes of the services screens (review
    /// w6-device F5): backup delete and rename, sign-in (pairs the device), sign-out (unpairs)
    /// and a Directory upload and its preset or capture overwrite answer (publishes to the
    /// account); the capture download overwrite answer (a `zenui.db` write); and a capture
    /// save (a `zenui.db` record, r21-control MR 6); a Wi-Fi connect or forget (a
    /// `wpa_supplicant.conf` write). Always: a CorOS update download or install (the install
    /// pre-steps mute and lock before the job is refused, review-r21 H4), a Tetrad update
    /// install and Reset Wi-Fi (review bring-up safety 2 R3). Outside bring-up: nothing.
    pub fn handoff_refusal(&self, handoff: &Handoff) -> Option<&'static str> {
        use tetrad_ui::{DeviceAction, UiCommand as C};
        if !self.is_bringup() {
            return None;
        }
        match handoff {
            Handoff::Services(
                C::Updater {
                    command: UpdaterCommand::Install | UpdaterCommand::CheckAndDownload,
                }
                | C::TetradUpdate {
                    command: tetrad_ui::TetradUpdateCommand::Install,
                },
            ) => Some("an update download or install"),
            // Review bring-up safety 2 R3: `REMOVE_NETWORK all` + `SAVE_CONFIG` drops SSH.
            Handoff::Services(C::Device {
                action: DeviceAction::ResetWifi,
            }) => Some("a Wi-Fi reset (it removes every network, so SSH is lost)"),
            Handoff::Services(_) if !self.services => {
                Some("services in a bring-up run without --services")
            }
            Handoff::Services(
                C::DeleteBackup { .. }
                | C::RenameBackup { .. }
                | C::UpdateBackup { .. }
                | C::CloudLogin { .. }
                | C::Device {
                    action: DeviceAction::CloudLogout,
                }
                | C::Upload { .. }
                | C::UploadOverwrite { .. }
                | C::CaptureUploadOverwrite { .. },
            ) if !self.persist => Some("a cloud account write in a bring-up run without --persist"),
            // re-r34 §2: OVERWRITE replaces a user capture's details in `zenui.db` (O: CANCEL is
            // refused too; no question can wait without --persist).
            Handoff::Services(C::CaptureOverwrite { .. }) if !self.persist => {
                Some("a capture store write without --persist")
            }
            Handoff::Services(
                C::DownloadCapture { .. } | C::DownloadProduct { .. } | C::DownloadProducts { .. },
            ) if !self.persist => Some("a Downloads store write without --persist"),
            Handoff::Services(C::WifiConnect { .. } | C::WifiForget { .. }) if !self.persist => {
                Some("a wpa_supplicant.conf write without --persist")
            }
            Handoff::Capture(C::SaveCapture { .. }) if !self.persist => {
                Some("a capture save without --persist")
            }
            // Setlist slot writes and cloud requests (re-r31 §5).
            Handoff::DownloadPresets { .. } if !self.services => {
                Some("services in a bring-up run without --services")
            }
            Handoff::DownloadPresets { .. } if !self.persist => {
                Some("a setlist write without --persist")
            }
            _ => None,
        }
    }

    /// What refuses a boot step that changes user data before the run loop, or `None` when
    /// it may run: none of them runs in a bring-up run, also with `--persist` (review
    /// bring-up safety 2 R2). They write `zenui.db` and the capture files and markers in `/etc`
    /// and `/opt/neuraldsp`, which the plan never lets bring-up write; stock ZenUI already ran
    /// them on this unit, so they would be no-ops at best.
    pub fn boot_refusal(&self, step: BootDataStep) -> Option<&'static str> {
        self.is_bringup().then_some(match step {
            BootDataStep::DatabaseUpgrade => "the zenui.db upgrade in a bring-up run",
            BootDataStep::LegacyIrs => "the legacy IR migration in a bring-up run",
            BootDataStep::LegacyCaptures => "the legacy capture migration in a bring-up run",
            BootDataStep::CaptureStoreTranslation => {
                "the capture store translation in a bring-up run"
            }
            BootDataStep::LegacyCaptureFiles => "the legacy capture file steps in a bring-up run",
            BootDataStep::CaptureCorruptionCheck => {
                "the capture corruption check in a bring-up run"
            }
        })
    }

    /// What bring-up refuses in `effect`, or `None` when it may run. Always refused: resets
    /// (the Wi-Fi reset drops SSH), CorOS update downloads and installs, Tetrad update installs, and the Cortex Control
    /// update commands before their pre-steps (mute, Control lock; review-r21 H4). Without `--persist`: backup
    /// create and restore jobs, the Cortex Control account work (licence activation can use up an account activation;
    /// sign-out unpairs), the NC2 cloud product DELETE, as [`Self::handoff_refusal`] for the UI.
    /// Without `--services`: every service job, cloud request and Cortex Control account
    /// request (this is the only place of that rule). Without `--allow-phantom`:
    /// every HAL write and jack-scheduler change that selects `XlrPhantom` (the only way 48 V
    /// reaches a jack, whatever the source: UI, MIDI, Cortex Control, the boot restore, a TYPE
    /// change with PHANTOM stored), and the PHANTOM setting change itself. Bridges that reduce
    /// an action directly (Cortex Control) still change `State::io`; only the hardware write
    /// is refused for them. Outside bring-up: nothing.
    pub fn refusal(&self, effect: &Effect) -> Option<&'static str> {
        if !self.is_bringup() {
            return None;
        }
        match effect {
            Effect::ResetSteps(_) | Effect::Action(Action::Reset(_)) => Some("a reset"),
            Effect::Service(
                ServiceJob::Download(_) | ServiceJob::Install | ServiceJob::TetradInstall(_),
            )
            | Effect::ControlAccount(
                AccountRequest::Updater(UpdaterCommand::Install | UpdaterCommand::CheckAndDownload)
                | AccountRequest::RestartDownloadViaControl,
            ) => Some("an update download or install"),
            // A restore removes Presets, Captures, CustomIRs and Plugins (r16-services).
            Effect::Service(ServiceJob::Backup(_)) if !self.persist => {
                Some("a backup create or restore without --persist")
            }
            Effect::Service(_) if !self.services => Some("a service job without --services"),
            // Bring-up safety 3, S3-1: the NC2 Abort / RESTART product DELETE removes a trained
            // capture from the account. The other `Effect::Cloud` requests are GETs (job,
            // download) and need `--services` only.
            Effect::Cloud(_) if !self.services => {
                Some("services in a bring-up run without --services")
            }
            Effect::Cloud(CloudRequest::Nc2Delete(_)) if !self.persist => {
                Some("a cloud product delete without --persist")
            }
            Effect::ControlAccount(_) if !self.persist => {
                Some("a cloud account write without --persist")
            }
            // Review r30 C4: refused here, as `handoff_refusal` refuses a services handoff, not
            // later as a lost cloud job.
            Effect::ControlAccount(
                AccountRequest::CloudProductDownload { .. }
                | AccountRequest::CloudProductDownloads { .. }
                | AccountRequest::CloudPresetDownload { .. }
                | AccountRequest::DownloadsQueue(_),
            ) if !self.services => Some("a cloud download without --services"),
            // CloudProduct (46) DELETE and UPLOAD change the user's cloud account (r32-control).
            Effect::ControlAccount(
                AccountRequest::CloudProductDelete { .. }
                | AccountRequest::CloudProductUpload { .. }
                | AccountRequest::CloudCaptureUpload { .. },
            ) if !self.services => Some("a cloud account write without --services"),
            // Health r34 #5: every other account request needs `--services` too (the licence
            // refresh, sign-out, clock, log upload and cloud backup run in the services bridge).
            Effect::ControlAccount(_) if !self.services => {
                Some("services in a bring-up run without --services")
            }
            // A capture record goes into `zenui.db` (`0x6fa0c4`); the database is read-only
            // without `--persist`, so refuse before the capture session moves on.
            Effect::CaptureRemote(super::capture::NcRemote::Save(_)) if !self.persist => {
                Some("a capture save without --persist")
            }
            Effect::StoreDownload(_) if !self.persist => {
                Some("a Downloads store without --persist")
            }
            Effect::PresetUploaded { .. } if !self.persist => {
                Some("a preset sidecar write without --persist")
            }
            Effect::CaptureUploaded { .. } if !self.persist => {
                Some("a capture cloudUUID write without --persist")
            }
            Effect::LoadPreset(_)
            | Effect::ProgramChange(_)
            | Effect::StepPresetLoad { .. }
            | Effect::LoadTemplate
            | Effect::RestoreSelected { .. }
                if self.stage != Stage::Full =>
            {
                Some("a preset change before stage full")
            }
            Effect::SavePreset(_)
            | Effect::EditPresetDetails { .. }
            | Effect::SavePresetAs { .. }
            | Effect::SavePresetInto { .. }
                if !self.persist =>
            {
                Some("a preset save without --persist")
            }
            Effect::Action(
                Action::CreateSetlist(_)
                | Action::DeletePreset { .. }
                | Action::RenamePreset { .. }
                | Action::DuplicatePreset { .. }
                | Action::CopyPreset { .. }
                | Action::CopyPresetTo { .. }
                | Action::PastePresets { .. }
                | Action::SwapPresets { .. }
                | Action::StepLibrarySwap { .. }
                | Action::StepLibraryFrame { .. }
                | Action::SetFavorite { .. }
                | Action::DevicePreset(_)
                | Action::CreateDevicePreset { .. }
                | Action::SetDefaultParams { .. }
                | Action::LinkMedia { .. }
                | Action::DuplicateMediaFolder { .. }
                | Action::SetSavingDestination { .. }
                | Action::PinModel { .. }
                | Action::RestoreQuarantinedCapture { .. },
            ) if !self.persist => Some("a library write without --persist"),
            Effect::Hal(HalCommand::Set(setting, _))
                if !self.allow_phantom
                    && setting.value() == Value::InputType(InputType::XlrPhantom) =>
            {
                Some("48 V phantom power without --allow-phantom")
            }
            Effect::ScheduleInput(InputChange::Type {
                kind: InputType::XlrPhantom,
                ..
            }) if !self.allow_phantom => Some("48 V phantom power without --allow-phantom"),
            Effect::Action(
                Action::ChangeSetting {
                    id: SettingId::Phantom(_),
                    ..
                }
                | Action::SetConnector {
                    id: ConnectorId::In1Phantom | ConnectorId::In2Phantom,
                    ..
                },
            ) if !self.allow_phantom => Some("a PHANTOM change without --allow-phantom"),
            _ => None,
        }
    }
}

/// Boot steps that change user data (r16-media): the `zenui.db` patch chain
/// (`0x00483b18`), the legacy IR and capture migrations (`0x6e72d4`, `0x6e71cc`) and the
/// capture store translation (`0x6a6358`, `0x6a68f4`); the capture manager steps (r23-media):
/// the legacy capture files and their markers (`0x703900`, `0x702d58`) and the corruption
/// check (`0x702f70`). See [`RunOptions::boot_refusal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootDataStep {
    DatabaseUpgrade,
    LegacyIrs,
    LegacyCaptures,
    CaptureStoreTranslation,
    LegacyCaptureFiles,
    CaptureCorruptionCheck,
}

/// The jack whose 48 V a refused effect would select or change. Matches the arms of
/// [`RunOptions::refusal`]: a refused effect that returns `Some` must be cleared by
/// `jacks::refuse_phantom`.
pub(crate) fn phantom_input(effect: &Effect) -> Option<Input> {
    match effect {
        Effect::Hal(HalCommand::Set(setting, _)) => match setting.control() {
            Control::InputType(input)
                if setting.value() == Value::InputType(InputType::XlrPhantom) =>
            {
                Some(input)
            }
            _ => None,
        },
        Effect::ScheduleInput(InputChange::Type {
            input,
            kind: InputType::XlrPhantom,
            ..
        }) => Some(*input),
        Effect::Action(Action::ChangeSetting {
            id: SettingId::Phantom(jack),
            ..
        }) => Some(match jack {
            InputJack::In1 => Input::One,
            InputJack::In2 => Input::Two,
        }),
        Effect::Action(Action::SetConnector { id, .. }) => match id {
            ConnectorId::In1Phantom => Some(Input::One),
            ConnectorId::In2Phantom => Some(Input::Two),
            _ => None,
        },
        _ => None,
    }
}

impl fmt::Display for RunOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let flag = |on: bool| if on { "on" } else { "off" };
        let owned: Vec<_> = self.owned.iter().map(|model| model.name()).collect();
        write!(
            f,
            "stage={} time_limit={}s persist={} services={} keep_muted={} mute_probe={} \
             allow_phantom={} preset={} power={:?} trace={} log_input={} owned={} \
             allow_owned={} control_cloud={} drill_core0={}",
            self.stage,
            self.time_limit.map_or(0, |limit| limit.as_secs()),
            flag(self.persist),
            flag(self.services),
            flag(self.keep_muted),
            flag(self.mute_probe),
            flag(self.allow_phantom),
            self.preset.as_deref().unwrap_or("-"),
            self.power,
            flag(self.trace),
            flag(self.log_input),
            if owned.is_empty() {
                "-".to_owned()
            } else {
                owned.join(",")
            },
            flag(self.allow_owned),
            flag(self.control_cloud),
            flag(self.drill_core0),
        )
    }
}

/// Counters for the health line and the stage verdict.
#[derive(Debug, Default, Clone, Copy)]
pub struct Health {
    pub crashes: u64,
    /// DSP sends, fragments, run-list clears and responses that timed out.
    pub timeouts: u64,
    /// `ModelStart` with `started=false` and `ModuleStartFailed`.
    pub start_failures: u64,
}

impl Health {
    pub(crate) fn record(&mut self, error: &DeviceError) {
        if let DeviceError::Dsp(DspError::Session(
            SessionError::SendTimeout { .. }
            | SessionError::NoFreeSlot { .. }
            | SessionError::FragmentTimeout { .. }
            | SessionError::RunListTimeout(_)
            | SessionError::ResponseTimeout(_),
        )) = error
        {
            self.timeouts += 1;
        }
    }
}

/// The loop's last sign of life, in device time, shared with the guard thread.
#[derive(Debug, Clone, Default)]
pub struct Heartbeat(Arc<Beat>);

#[derive(Debug, Default)]
struct Beat {
    last_ms: AtomicU64,
    running: AtomicBool,
}

/// Why the guard ends the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stall {
    /// No heartbeat for `quiet` (`running`: after start-up).
    Stalled { quiet: Duration, running: bool },
    /// Past the time limit plus [`OVERRUN`].
    Overrun,
}

impl Heartbeat {
    pub fn beat(&self, now: Duration) {
        let ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
        self.0.last_ms.store(ms, Ordering::Relaxed);
    }

    /// Start-up is over: the stall limit drops to [`LOOP_STALL`].
    pub fn set_running(&self) {
        self.0.running.store(true, Ordering::Relaxed);
    }

    /// The guard check at device time `now` for a run limited to `limit`.
    pub fn verdict(&self, now: Duration, limit: Duration) -> Option<Stall> {
        if now > limit + OVERRUN {
            return Some(Stall::Overrun);
        }
        let quiet = now.saturating_sub(Duration::from_millis(
            self.0.last_ms.load(Ordering::Relaxed),
        ));
        let running = self.0.running.load(Ordering::Relaxed);
        let allowed = if running { LOOP_STALL } else { STARTUP_STALL };
        (quiet > allowed).then_some(Stall::Stalled { quiet, running })
    }
}

impl fmt::Display for Stall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stalled { quiet, running } => write!(
                f,
                "no heartbeat for {:.1} s ({})",
                quiet.as_secs_f64(),
                if *running { "loop" } else { "start-up" }
            ),
            Self::Overrun => write!(f, "time limit passed by {} s", OVERRUN.as_secs()),
        }
    }
}

/// Process probes outside the device platform; `main` implements them over the OS.
pub trait Host {
    /// [`STOP_FILE`] exists.
    fn stop_requested(&mut self) -> bool;
    /// `VmRSS` of `/proc/self/status`, in KiB.
    fn rss_kib(&mut self) -> Option<u64>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitReason {
    TimeLimit,
    StopFile,
    Power(PowerAction),
    Panic(String),
}

impl fmt::Display for ExitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimeLimit => f.write_str("time limit"),
            Self::StopFile => write!(f, "stop file {STOP_FILE}"),
            Self::Power(action) => write!(f, "power action {action:?}"),
            Self::Panic(message) => write!(f, "panic: {message}"),
        }
    }
}

#[derive(Debug)]
pub struct Exit {
    pub reason: ExitReason,
    /// The automatic stage checks; the manual checks of the plan come on top.
    pub verdict: Result<(), String>,
    /// `--power stock` only: the stock teardown result.
    pub power: Option<Result<PowerReport, DeviceError>>,
}

impl Exit {
    /// 0: stage pass, 1: stock teardown failed, 2: stage fail, [`PANIC_EXIT`]: panic.
    pub fn code(&self) -> u8 {
        match (&self.reason, &self.verdict, &self.power) {
            (ExitReason::Panic(_), ..) => PANIC_EXIT,
            (_, _, Some(Err(_))) => 1,
            (_, Err(_), _) => 2,
            _ => 0,
        }
    }
}

/// A loop pass at least this long writes one `SLOW` run-log line with its phase times and the
/// costliest unit of each phase ([`Device::last_units`]).
const SLOW_PASS: Duration = Duration::from_millis(100);
/// The `SLOW` line names the costliest unit of a phase when it took at least this long.
const SLOW_UNIT: Duration = Duration::from_millis(10);

/// Frames and the longest pass since the last health line.
struct Window {
    since: Duration,
    frames: u64,
    max_step: Duration,
}

impl<P: Platform> Device<Bringup<P>>
where
    P::RingDrain: DrainMode<platform::TapWindow<P>>,
{
    /// One bring-up run (B9): [`Device::start_with`] under `catch_unwind`, then
    /// [`Self::run_bringup`]. The stop file and the time limit end start-up at the next splash
    /// phase (`Bringup::splash` unwinds with the [`ExitReason`]). After a start-up stop, panic
    /// or error the dropped decorator has done the start-up exit (best-effort `0x20` mute,
    /// headphones 0); this adds the `STAGE … FAIL` line. A start-up error is returned.
    pub fn bringup(platform: Bringup<P>, options: RunOptions) -> Result<Exit, StartupError> {
        let (stage, dir) = (options.stage, options.log_dir.clone());
        let stage_failed = |failure: &str| {
            if let Some(dir) = &dir {
                let _ = platform::append_summary(dir, &format!("STAGE {stage} FAIL {failure}"));
            }
        };
        match catch_unwind(AssertUnwindSafe(|| Self::start_with(platform, options))) {
            Ok(Ok(device)) => Ok(device.run_bringup()),
            Ok(Err(error)) => {
                stage_failed(&format!("start-up error: {error}"));
                Err(error)
            }
            Err(payload) => {
                let reason = payload.downcast::<ExitReason>().map_or_else(
                    |payload| ExitReason::Panic(panic_text(&*payload)),
                    |reason| *reason,
                );
                let failure = format!("start-up ended by {reason}");
                stage_failed(&failure);
                Ok(Exit {
                    reason,
                    verdict: Err(failure),
                    power: None,
                })
            }
        }
    }

    /// The bring-up loop (P3, P4, P7, P8): runs [`Device::step`] under `catch_unwind` until
    /// the time limit, the stop file, a power action or a panic, then ends with the test
    /// exit (or the stock teardown for `--power stock`) and the `STAGE` line.
    pub fn run_bringup(mut self) -> Exit {
        self.platform.started = true;
        self.platform.heartbeat.set_running();
        self.device_summary();
        if self.options.stage.boots_dsp() {
            // Normally taken after the DSP boot (`FAULTS boot`); here only when the DSP
            // windows never opened.
            if self.platform.faults.is_none() {
                self.platform.faults =
                    Platform::sharc_memory(&mut self.platform).map(FaultWatch::new);
            }
            self.fault_line("start");
        }
        let start = self.clock.now();
        let mut probe = self.options.mute_probe.then_some((0, start));
        let (mut next_stop_poll, mut next_health) = (start, start + HEALTH_PERIOD);
        let (mut next_usb, mut usb_logged) = (start, None);
        let mut window = Window {
            since: start,
            frames: self.platform.frames,
            max_step: Duration::ZERO,
        };
        loop {
            let now = self.clock.now();
            self.platform.beat();
            if self.options.time_limit.is_some_and(|limit| now >= limit) {
                return self.finish(ExitReason::TimeLimit);
            }
            if now >= next_stop_poll {
                next_stop_poll = now + STOP_POLL;
                if self.platform.host.stop_requested() {
                    return self.finish(ExitReason::StopFile);
                }
            }
            if now >= next_health {
                next_health = now + HEALTH_PERIOD;
                let rss = self.platform.host.rss_kib();
                self.health_line(rss, &mut window, now);
            }
            if now >= next_usb {
                next_usb = now + USB_PERIOD;
                self.usb_line(&mut usb_logged);
            }
            if let Some((index, due)) = probe
                && now >= due
            {
                self.mute_probe_step(MUTE_PROBE[index]);
                probe = MUTE_PROBE
                    .get(index + 1)
                    .map(|_| (index + 1, due + MUTE_PROBE_STEP));
            }
            match catch_unwind(AssertUnwindSafe(|| self.step())) {
                Ok(None) => {
                    let step = self.clock.now().saturating_sub(now);
                    window.max_step = window.max_step.max(step);
                    if step >= SLOW_PASS {
                        let phases = self.last_phases();
                        let units = self.last_units(SLOW_UNIT);
                        self.platform.note(&format!(
                            "SLOW pass {}ms [{phases}] top [{units}]",
                            step.as_millis()
                        ));
                    }
                }
                Ok(Some(action)) if self.options.power == PowerMode::Stock => {
                    return self.stock_power(action);
                }
                Ok(Some(action)) => return self.finish(ExitReason::Power(action)),
                Err(payload) => return self.finish(ExitReason::Panic(panic_text(&*payload))),
            }
        }
    }

    /// The test exit (P4), then the verdict and the `STAGE` line.
    fn finish(mut self, reason: ExitReason) -> Exit {
        self.platform.note(&format!("EXIT {reason}"));
        // After a panic the state may be inconsistent: a second panic ends the exit early.
        let exited = catch_unwind(AssertUnwindSafe(|| self.test_exit()));
        self.fault_line("exit");
        let verdict = match (&reason, exited) {
            (ExitReason::Panic(message), _) => Err(format!("panic: {message}")),
            (_, Err(payload)) => Err(format!("panic in the test exit: {}", panic_text(&*payload))),
            (_, Ok(())) => self.verdict(),
        };
        self.stage_line(&verdict);
        Exit {
            reason,
            verdict,
            power: None,
        }
    }

    /// `--power stock`: the stock teardown. Its `0x20` value 0 (`PowerFlags(0)`, which stock
    /// names "mute first") is an unmute under the bring-up reading of bit 0, so the unmute hold
    /// turns it into value 1 (review B7; inferred until S4a, and S10 runs only after S4a).
    fn stock_power(mut self, action: PowerAction) -> Exit {
        let reason = ExitReason::Power(action);
        self.platform.note(&format!(
            "EXIT {reason}: stock power path, 0x20 value 0 held"
        ));
        self.platform.interlock.hold.store(true, Ordering::Relaxed);
        self.fault_line("exit");
        let verdict = self.verdict();
        self.stage_line(&verdict);
        Exit {
            reason,
            verdict,
            power: Some(self.power(action)),
        }
    }

    /// `0x20` mute to cores 0 and 1, headphones 0, the mini surface LEDs off, the settings
    /// flush (would-write under no-persist) and `zenui.db` closed. No system power step: the
    /// process exits and ZenWatchdog restarts stock (R1).
    fn test_exit(&mut self) {
        if self.dsp.is_some() {
            let muted = self.dsp_command(DspCommand::MasterMute(dsp::DEPLOY_MUTE));
            self.report(muted);
        } else {
            self.platform.note("EXIT no DSP: 0x20 mute not sent");
        }
        let mini = tetrad_watchdog::cmdline::Product::from(self.state.platform)
            == tetrad_watchdog::cmdline::Product::QuadCortexMini;
        let steps = [PowerStep::MuteHeadphones]
            .into_iter()
            .chain(mini.then_some(PowerStep::SurfaceLedsOff));
        for step in steps {
            let done = self.power_step(step);
            self.report(done);
        }
        let flushed = self.flush_settings();
        self.report(flushed.map_err(DeviceError::from));
        if let Some(library) = self.state.library.as_mut() {
            library.database = None;
        }
        self.platform.note("EXIT muted, settings flushed");
    }

    /// The automatic stage checks (P2). Stage `probe` needs a presented frame; `dsp` adds
    /// the boot, SOC2 open, the catalog, no crash, timeout or dead core, and an unchanged,
    /// always readable core-0 fault count; `deploy` and `full`
    /// add a deployed preset with every model started.
    fn verdict(&self) -> Result<(), String> {
        let mut failures = Vec::new();
        if self.platform.frames == 0 {
            failures.push("no frame presented".to_owned());
        }
        if self.options.stage.boots_dsp() {
            let dead = self.dsp.as_ref().map(dead_cores);
            match dead {
                None => failures.push("the DSP did not boot".into()),
                Some(0) => {}
                Some(dead) => failures.push(format!("{dead} dead cores")),
            }
            if self.state.soc2 != Some(dsp::Soc2Status::Open) {
                failures.push(format!("SOC2 {:?}", self.state.soc2));
            }
            if self.state.specs.is_none() {
                failures.push("no catalog (keys or ModelRepo)".into());
            }
            let Health {
                crashes, timeouts, ..
            } = self.health;
            if crashes + timeouts > 0 {
                failures.push(format!("{crashes} crashes, {timeouts} timeouts"));
            }
            match &self.platform.faults {
                None => failures.push("no SHARC memory: fault count not checked".into()),
                Some(watch) => failures.extend(watch.failure()),
            }
        }
        if self.options.stage.deploys() {
            if self.state.current.is_none() {
                failures.push("no preset deployed".into());
            }
            if self.health.start_failures > 0 {
                failures.push(format!(
                    "{} model starts failed",
                    self.health.start_failures
                ));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join(", "))
        }
    }

    fn stage_line(&mut self, verdict: &Result<(), String>) {
        let line = match verdict {
            Ok(()) => format!("STAGE {} PASS", self.options.stage),
            Err(reasons) => format!("STAGE {} FAIL {reasons}", self.options.stage),
        };
        self.platform.note(&line);
        let _ = self.platform.summary(&line);
    }

    /// Device facts after start-up for `summary.log` (P8), with the S2 evidence (review B6):
    /// markers and start kind, boot writes per region, SOC2 buffer and gate, key reply
    /// lengths, catalog model count, stock tables and the IR library. Node open failures are
    /// platform notices from before the decorator: see `stderr.log`.
    fn device_summary(&mut self) {
        let mut revision = |board| Hal::read(self.platform.hal(), Query::Revision(board));
        let (zenjack, zencoder) = (revision(Board::Zenjack), revision(Board::Zencoder));
        let loaded = |loaded: bool| if loaded { "loaded" } else { "absent" };
        let boot = format!(
            "{}\nmodels {} tables={} ir_library={}",
            self.platform.boot_summary(),
            self.state
                .specs
                .as_ref()
                .map_or("absent".into(), |specs| specs.models.len().to_string()),
            loaded(self.dsp.as_ref().is_some_and(dsp::Dsp::has_tables)),
            loaded(self.media.is_some()),
        );
        let state = &self.state;
        let last = &state.settings.settings().last_preset;
        let (setlists, last_path) = state.library.as_ref().map_or_else(
            || ("library not open".to_owned(), None),
            |library| {
                let index = usize::try_from(last.preset_index).unwrap_or(usize::MAX);
                let path =
                    preset::last_preset_path(&library.presets, last.setlist.as_deref(), index);
                let setlists = library.presets.setlists().map_or_else(
                    |error| format!("setlists unreadable: {error}"),
                    |setlists| format!("{} setlists", setlists.len()),
                );
                (setlists, Some(path))
            },
        );
        let text = format!(
            "product {:?} platform {:?}\nrevisions zenjack={zenjack:?} zencoder={zencoder:?}\n\
             dsp {} soc2={:?} catalog={}\n{boot}\nlast preset setlist={} index={} \
             path={last_path:?}\nlibrary {setlists}, zenui.db {}\npreset {}\n\
             start-up {:.1} s, {} notices",
            state.product,
            state.platform,
            if self.dsp.is_some() {
                "booted"
            } else {
                "not booted"
            },
            state.soc2,
            if state.specs.is_some() {
                "loaded"
            } else {
                "absent"
            },
            last.setlist.as_deref().unwrap_or("(first)"),
            last.preset_index,
            if state
                .library
                .as_ref()
                .is_some_and(|library| library.database.is_some())
            {
                "open"
            } else {
                "closed"
            },
            state
                .current
                .as_ref()
                .map_or("none".into(), |current| format!("{:?}", current.path)),
            self.clock.now().as_secs_f64(),
            self.platform.notices,
        );
        let _ = self.platform.summary(&text);
    }

    /// `FAULTS <when> faults core0=<n>` from stage `dsp` on.
    fn fault_line(&mut self, when: &str) {
        if self.options.stage.boots_dsp() {
            self.platform.fault_line(when);
        }
    }

    /// A14 (s4-usb-in §5.1): one `USB` line with the 21 USB Statistics rows (`0x68a90c`) when
    /// a SOC2 ARM block arrived since the last line. Counts and indexes only: the block holds
    /// no audio, MIDI or key bytes.
    fn usb_line(&mut self, logged: &mut Option<Soc2ArmDiagnostics>) {
        let block = self.state.diagnostics.soc2;
        if block.is_none() || block == *logged {
            return;
        }
        *logged = block;
        let hid_sent = self.bridges.control.hid_reports_sent();
        if let Some(rows) = self.state.diagnostics.usb_statistics(hid_sent) {
            let rows: Vec<_> = rows
                .iter()
                .map(|row| format!("{}={}", row.label, row.value))
                .collect();
            self.platform.note(&format!("USB {}", rows.join("; ")));
        }
    }

    /// One `HEALTH` line; from stage `dsp` on it has the fault count. `diag` lists the
    /// diagnostics requests sent and replies received per core since the last line (B10).
    fn health_line(&mut self, rss: Option<u64>, window: &mut Window, now: Duration) {
        let faults = self
            .platform
            .faults
            .as_mut()
            .map_or_else(String::new, |watch| format!(" {}", watch.sample()));
        let frames = self.platform.frames - window.frames;
        let seconds = now.saturating_sub(window.since).as_secs_f64();
        let line = format!(
            "HEALTH rss={} loop_max={}ms present_max={}ms phases_max_ms=[{}] fps={:.1} renders={} notices={} crashes={} timeouts={} \
             start_failures={} dead_cores={} unmutes_held={}{faults} {}",
            rss.map_or("?".into(), |kib| format!("{kib}KiB")),
            window.max_step.as_millis(),
            std::mem::take(&mut self.platform.present_max).as_millis(),
            self.take_phase_max(),
            if seconds > 0.0 {
                frames as f64 / seconds
            } else {
                0.0
            },
            self.take_renders(),
            self.platform.notices,
            self.health.crashes,
            self.health.timeouts,
            self.health.start_failures,
            self.dsp.as_ref().map_or(0, dead_cores),
            self.platform.interlock.held.load(Ordering::Relaxed),
            self.platform.rings.take_diagnostics(),
        );
        self.platform.note(&line);
        *window = Window {
            since: now,
            frames: self.platform.frames,
            max_step: Duration::ZERO,
        };
    }

    /// One mute probe value; the hold is lifted for this send only.
    fn mute_probe_step(&mut self, value: u32) {
        let interlock = self.platform.interlock.clone();
        interlock.hold.store(false, Ordering::Relaxed);
        let sent = self.dsp_command(DspCommand::MasterMute(value));
        interlock.hold.store(true, Ordering::Relaxed);
        self.platform
            .note(&format!("MUTE-PROBE 0x20 value={value}"));
        self.report(sent);
    }
}

fn dead_cores<P: Platform>(dsp: &dsp::Dsp<P>) -> usize {
    let comms = dsp.session().comms();
    platform::RINGS
        .into_iter()
        .filter(|&core| comms.link(core).dead)
        .count()
}

fn panic_text(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-text panic".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::control::CloudBackupRequest;
    use crate::device::services::CloudRequest;
    use tetrad_services::cloud::services::ProductKind;

    /// Health r34 #5: the Cortex Control account and cloud refusals, pinned for the four
    /// bring-up flag sets (none, `--services`, `--persist`, both); `tetrad run` refuses none.
    /// `R` = refused, `.` = runs. Every account request needs both flags; the `Effect::Cloud`
    /// reads need `--services`, the NC2 product DELETE also `--persist`; an update is always
    /// refused.
    #[test]
    fn control_account_and_cloud_refusals_need_their_flags() {
        let options = |services, persist| RunOptions {
            services,
            persist,
            ..RunOptions::bringup(Stage::Full, Duration::from_secs(60), PathBuf::from("log"))
        };
        let sets = [
            options(false, false),
            options(true, false),
            options(false, true),
            options(true, true),
        ];
        let account = |request| Effect::ControlAccount(request);
        let id = || "cloud-1".to_owned();
        let table = [
            (account(AccountRequest::RefreshLicences), "RRR."),
            (account(AccountRequest::SignOut), "RRR."),
            (account(AccountRequest::SetClock(1)), "RRR."),
            (account(AccountRequest::UploadLogs), "RRR."),
            (
                account(AccountRequest::CloudBackup(CloudBackupRequest::List)),
                "RRR.",
            ),
            (
                account(AccountRequest::DownloadsQueue(ProductKind::Preset)),
                "RRR.",
            ),
            (
                account(AccountRequest::CloudProductDownload {
                    kind: ProductKind::NeuralCapture,
                    id: id(),
                }),
                "RRR.",
            ),
            // r47-control: Control 46 IR DOWNLOAD, and more capture or IR keys as one batch.
            (
                account(AccountRequest::CloudProductDownload {
                    kind: ProductKind::ImpulseResponse,
                    id: id(),
                }),
                "RRR.",
            ),
            (
                account(AccountRequest::CloudProductDownloads {
                    kind: ProductKind::ImpulseResponse,
                    ids: vec![id(), "cloud-2".into()],
                }),
                "RRR.",
            ),
            // r47-control: Control 46 DOWNLOAD of more preset keys writes more setlist slots.
            (
                account(AccountRequest::CloudPresetDownload {
                    setlist: "Gig".into(),
                    items: vec![(id(), 0), ("cloud-2".into(), 1)],
                }),
                "RRR.",
            ),
            (
                account(AccountRequest::CloudProductDelete {
                    kind: ProductKind::Preset,
                    ids: vec![id()],
                }),
                "RRR.",
            ),
            (
                account(AccountRequest::CloudProductUpload { path: id() }),
                "RRR.",
            ),
            (
                account(AccountRequest::CloudCaptureUpload { key: id() }),
                "RRR.",
            ),
            (
                account(AccountRequest::Updater(UpdaterCommand::Install)),
                "RRRR",
            ),
            (account(AccountRequest::RestartDownloadViaControl), "RRRR"),
            (Effect::Cloud(CloudRequest::Nc2Job(id())), "R.R."),
            (Effect::Cloud(CloudRequest::Nc2Download(id())), "R.R."),
            (Effect::Cloud(CloudRequest::Nc2Delete(id())), "RRR."),
            (Effect::Service(ServiceJob::Install), "RRRR"),
        ];
        for (effect, expected) in &table {
            // re-r44 §3 (O): `--control-cloud` changes the route only, never a refusal.
            for control_cloud in [false, true] {
                let got: String = (sets.iter())
                    .map(|set| RunOptions {
                        control_cloud,
                        ..set.clone()
                    })
                    .map(|set| {
                        if set.refusal(effect).is_some() {
                            'R'
                        } else {
                            '.'
                        }
                    })
                    .collect();
                assert_eq!(got, *expected, "{effect:?} control_cloud={control_cloud}");
            }
            assert_eq!(RunOptions::STOCK.refusal(effect), None, "{effect:?}");
        }
        assert_eq!(
            sets[2].refusal(&account(AccountRequest::RefreshLicences)),
            Some("services in a bring-up run without --services")
        );
    }

    /// re-r44 §3 (O): a bring-up run routes no cloud call over Cortex Control without
    /// `--control-cloud`, so proof H1 tests the Wi-Fi path first. The flag is off by default,
    /// needs stage full and `--services`, and `--check` prints it. `run` routes over Control.
    #[test]
    fn cloud_over_control_needs_the_control_cloud_flag() {
        let options = |stage, services, control_cloud| RunOptions {
            services,
            control_cloud,
            ..RunOptions::bringup(stage, Duration::from_secs(60), PathBuf::from("log"))
        };
        let off = RunOptions::bringup(Stage::Full, Duration::from_secs(60), PathBuf::from("log"));
        assert!(!off.control_cloud);
        assert_eq!(
            options(Stage::Full, true, false).control_cloud_refusal(),
            Some("cloud over Cortex Control in a bring-up run without --control-cloud")
        );
        assert_eq!(
            options(Stage::Full, true, true).control_cloud_refusal(),
            None
        );
        assert_eq!(RunOptions::STOCK.control_cloud_refusal(), None);
        let invalid = |options: RunOptions| options.validate().unwrap_err().to_string();
        assert_eq!(
            invalid(options(Stage::Probe, false, true)),
            "bring-up options: --persist, --services, --allow-phantom and --control-cloud \
             need stage full"
        );
        assert_eq!(
            invalid(options(Stage::Full, false, true)),
            "bring-up options: --control-cloud needs --services"
        );
        assert!(options(Stage::Full, true, true).validate().is_ok());
        assert!(
            (options(Stage::Full, true, true).to_string()).contains(" control_cloud=on"),
            "--check prints the flag"
        );
        assert!(off.to_string().contains(" control_cloud=off"));
    }

    /// r35-library MR 1, re-r38 §4: UNDO or REDO of a library swap, delete or copy/move frame
    /// writes setlist slots, so it needs `--persist`. A load frame step is a preset change: it
    /// waits for stage full, as a load does.
    #[test]
    fn a_library_undo_needs_persist_and_a_load_undo_stage_full() {
        let options = |stage, persist| RunOptions {
            persist,
            ..RunOptions::bringup(stage, Duration::from_secs(60), PathBuf::from("log"))
        };
        for redo in [false, true] {
            for step in [
                Action::StepLibrarySwap { redo },
                Action::StepLibraryFrame { redo },
            ] {
                let step = Effect::Action(step);
                assert_eq!(
                    options(Stage::Full, false).refusal(&step),
                    Some("a library write without --persist")
                );
                assert_eq!(options(Stage::Full, true).refusal(&step), None);
            }
            let load = Effect::StepPresetLoad { redo };
            assert_eq!(
                options(Stage::Dsp, true).refusal(&load),
                Some("a preset change before stage full")
            );
            assert_eq!(options(Stage::Full, false).refusal(&load), None);
        }
        // Review r39 K13: a MIDI program change load is a preset change too.
        let slot = crate::device::PresetRef {
            setlist: None,
            index: 1,
        };
        let program_change = Effect::ProgramChange(slot);
        assert_eq!(
            options(Stage::Dsp, true).refusal(&program_change),
            Some("a preset change before stage full")
        );
    }

    /// re-r34 §1, §2: the capture overwrite answers (an account write, a `zenui.db` write) need
    /// `--services` and `--persist`, also a CANCEL.
    #[test]
    fn capture_overwrite_answers_need_services_and_persist() {
        use tetrad_ui::UiCommand as C;
        let options = |services, persist| RunOptions {
            services,
            persist,
            ..RunOptions::bringup(Stage::Full, Duration::from_secs(60), PathBuf::from("log"))
        };
        for overwrite in [true, false] {
            for command in [
                C::CaptureOverwrite { overwrite },
                C::CaptureUploadOverwrite { overwrite },
            ] {
                let handoff = Handoff::Services(command);
                for (services, persist) in [(false, false), (true, false), (false, true)] {
                    let refused = options(services, persist).handoff_refusal(&handoff);
                    assert!(refused.is_some(), "{handoff:?} {services} {persist}");
                }
                assert_eq!(options(true, true).handoff_refusal(&handoff), None);
                assert_eq!(RunOptions::STOCK.handoff_refusal(&handoff), None);
            }
        }
    }
}

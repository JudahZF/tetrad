//! Services bridge (owner: 2B-services): Wi-Fi, DHCP, internet probe and time, updater and
//! cloud through tetrad-services over the platform's [`ServiceBackends`].
//!
//! The bridge holds service state only (`WifiService`, `InternetManager`, `CloudApi`, updater
//! state). Every call borrows the backends through [`ServiceBackends::lend`]; a platform that
//! lends nothing gets [`ServiceError::Unsupported`].
//!
//! Contract for the owner (k-services, review K13):
//! - Blocking work (`udhcpc`, the internet probe and NTP sync, the update download, the
//!   installer, backups, every cloud request) is an [`Effect::Service`] job; the result comes
//!   back as a [`ServiceNotice`]. Each job names its [`Lane`]: a platform runs the lanes on
//!   separate workers, so a download never holds up a lease or a sign-in (review r14 S-2). A
//!   cloud job carries the session out and back ([`CloudJob`]); requests made meanwhile queue
//!   behind it, one job at a time (gap #18).
//! - Updater install success never reboots by itself; it returns [`Effect::Power`] with
//!   `RestartReason::Direct` (stock `0x424740`).
//! - Signed requests (update check, pair, unpair, licence activation) queue in one signing batch.
//!   The batch gets its signing material and client id from the DSP (`0x11` for `0x35`, `0x1f` for
//!   `0x4f`), signs every queued job once and drops them. No reply within 2 s fails each queued job
//!   with a notice.
//! - Tokens live in tetrad-settings (`AccountConfig.ini`) behind [`SettingsTokens`]. Never log
//!   them, the signing replies or licence data.

use std::collections::VecDeque;
use std::fmt::{self, Display};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tetrad_capture::v2::Nc2Input;
use tetrad_control::codecs::{
    EnumValue, GenericErrorCode, GenericErrorMessage, MessageAction, UpdaterMessage,
    UpdaterState as WireState, UpdaterStatus as WireStatus,
};
use tetrad_dsp_protocol::requests::{OpaqueBody, Request};
use tetrad_dsp_protocol::responses::UsbMeter;
use tetrad_dsp_protocol::tuner_looper::{OpaqueCalibration, TextPrefix};
use tetrad_media::factory::{DeviceCipher, DeviceSerial, SERIAL_PATH};
use tetrad_services::backup::local::{CreatorStamp, LocalHeader};
use tetrad_services::backup::{self, BackupError, Summary};
use tetrad_services::cloud::api::{ApiError, CloudApi, CloudError};
use tetrad_services::cloud::endpoint::{CloudDomain, CloudEndpoint, CloudStage};
use tetrad_services::cloud::products::{
    CaptureCheck, CaptureUploadStatus, CheckMode, CloudProduct, PAYLOAD_LIMIT, ProductDependency,
    ProductPayload,
};
use tetrad_services::cloud::services::{
    BackupData, BackupState, BackupUrl, BrowsePage, BrowseScope,
    ProductDependency as UploadDependency, ProductKind, ProductUpload, product_digest,
    upload_backup,
};
use tetrad_services::cloud::session::{TokenStore, TokenStoreError, Tokens};
use tetrad_services::cloud::signing::{ClientIdBytes, SharcSigner, SigningMaterial};
use tetrad_services::cloud::{ClientType, DeviceIdentity};
use tetrad_services::dhcp::{self, AppliedLease, DhcpError};
use tetrad_services::exec::{
    CancelToken, CommandError, CommandRunner, FileError, Files, NetError, Secret,
};
use tetrad_services::http::{BackupsOp, HttpClient, HttpError, HttpRequest, Method, Route};
use tetrad_services::internet::{
    InternetManager, InternetNotice, InternetProbe, InternetResult, InternetStatus, InternetWork,
    Recovery, RecoveryStep, STOCK_NTP_WAIT, STOCK_PROBE_TIMEOUT, STOCK_RECHECK_INTERVAL, TimeSync,
};
use tetrad_services::updater::cloud::{CheckOutcome, UpdateError, UpdateOffer, download_update};
use tetrad_services::updater::install::{
    self, InstallError, InstallOutcome, InstallPoll, STATUS_POLL_MS,
};
use tetrad_services::updater::paths;
use tetrad_services::updater::state::{UpdaterState, UpdaterStatus};
use tetrad_services::updater::tetrad::{self, Offer};
use tetrad_services::wifi::events::{
    STOCK_ASSOCIATION_TIMEOUT_MS, WifiAction, WifiNotice, WifiStatus as LinkStatus,
};
use tetrad_services::wifi::scan::{SavedNetwork, ScanEntry, Security};
use tetrad_services::wifi::{self, DhcpRun, Step, SupplicantLink, WifiError, WifiService};
use tetrad_settings::account::{AccountIdentity, AccountTokens, PairedVersion};
use tetrad_settings::device::{DeviceSettings, ResetEffect};
use tetrad_settings::permanent::CloudDomain as SavedDomain;
use tetrad_storage::stock::{DownloadKind, StockDb};

use tetrad_ui::{
    BackupItem, BackupView, CaptureQuestion, DeviceAction, TetradUpdateCommand,
    UiCommand as ViewCommand, UpdaterCommand, UpdaterView, WifiAlert, WifiNetwork, WifiView,
};
use tetrad_watchdog::power::RestartReason;

use super::actions::settings_flush as flush;
use super::platform::{ServiceBackends, ServiceIo};
use super::{DspCommand, Effect, PowerAction, Subsystem, Timer};

mod account;
mod backups;
mod cloud;
mod cloud_job;
mod downloads;
mod licences;
mod net;
mod products;
mod runs;
mod store;
mod tetrad_update;
mod updater;
mod uploads;
mod worker;

pub use self::account::{AfterUnpair, ControlLogin, SettingsTokens};
pub(in crate::device) use self::backups::utc_parts;
pub use self::backups::{
    BACKUP_STAGE, BackupDone, BackupJob, CaptureKeys, CloudBackup, ControlBackup,
    ControlBackupDone, Stage, convert_backup_gains, keep_device_values_after_restore,
};
pub use self::cloud_job::{CloudJob, CloudReply, CloudRequest, Nc2Download};
pub use self::licences::{Licences, PluginStatus};
pub use self::products::{
    CLOUD_FOLDER_ROWS, CLOUD_PAGE_ROWS, CaptureDownloadEnd, CloudDownload, CloudFolder,
    DOWNLOAD_QUEUE_BYTES, DOWNLOAD_QUEUE_PAGES, DownloadOrigin, DownloadOutcome, DownloadsRow,
    SlotDownload,
};
pub(in crate::device) use self::products::{browse_scope, product_kind};
pub use self::runs::{BatchEnd, BatchOperation, BatchView};
pub use self::updater::{ControlTransport, DownloadJob, Updater};
pub use self::uploads::{
    BatchItem, CaptureDraft, CaptureOrigin, NO_IR_UPLOAD, PresetDraft, PresetFile, UploadEnd,
    UploadQuestion, UploadState, WalkCapture, capture_meta_signature,
};
pub use self::worker::run_job;
use self::{
    account::*, backups::*, cloud_job::*, licences::*, products::*, runs::*, uploads::*, worker::*,
};
use super::{Device, DeviceError, control, media, platform};

/// Stock polls supplicant events every 60 ms; the internet manager keeps its own 5 s recheck.
pub const TICK: Duration = Duration::from_millis(60);

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("not supported yet: {0}")]
    Unsupported(&'static str),
    #[error("{0}")]
    NotReady(&'static str),
    #[error(transparent)]
    Wifi(#[from] WifiError),
    #[error(transparent)]
    Cloud(#[from] ApiError),
    #[error(transparent)]
    Store(#[from] TokenStoreError),
    #[error(transparent)]
    File(#[from] FileError),
    #[error(transparent)]
    Update(#[from] UpdateError),
}

const NO_TIME: &str = "time is not set yet (no NTP sync)";
const CAPTURE_NOT_IN_CLOUD: &str = "this Neural Capture is not available on Cortex Cloud";
/// A second multi-key run while one runs (stock: `+0x55`, the Control receivers return).
const RUN_IN_PROGRESS: &str = "a Multi Select run is in progress";

/// A value that rides in [`Effect`] and [`Input`](super::platform::Input), which must be
/// `Clone` and `PartialEq`. Clones share one slot and the first [`Self::take`] wins, so a
/// session or a signer is moved, never copied. Equality is identity.
pub struct Handoff<T>(Arc<Mutex<Option<T>>>);

impl<T> Handoff<T> {
    pub fn new(value: T) -> Self {
        Self(Arc::new(Mutex::new(Some(value))))
    }

    pub fn take(&self) -> Option<T> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

impl<T> Clone for Handoff<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> PartialEq for Handoff<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl<T> Eq for Handoff<T> {}

impl<T> fmt::Debug for Handoff<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Handoff")
    }
}

/// Blocking work the platform runs off the event loop ([`ServiceBackends::run_job`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceJob {
    /// `udhcpc` for this run ([`dhcp::acquire`]).
    Dhcp(DhcpRun),
    /// The TCP reachability probe or the NTP sync ([`InternetWork::run`]).
    Internet(InternetWork),
    /// Size header, package, `gzip -t` ([`tetrad_services::updater::cloud::download_update`]).
    Download(DownloadJob),
    /// The stock installer, polled to the end ([`install::start`]).
    Install,
    /// Cloud requests with the session they need.
    Cloud(Handoff<CloudJob>),
    /// Backup packaging, transfer or restore ([`tetrad_services::backup`]).
    Backup(BackupJob),
    /// Owned: the newest Tetrad release ([`tetrad::check`]).
    TetradCheck,
    /// Owned: a Tetrad release downloaded, checked, installed and armed ([`tetrad::install`]).
    TetradInstall(Offer),
}

/// The platform worker a job runs on. A platform with workers runs each lane on its own
/// thread, in order within the lane (review r14 S-2). Owned split: stock runs cloud calls
/// through `QNetworkAccessManager` and the update download in its own thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// DHCP, the internet probe and NTP: short, and the link waits for them.
    System,
    /// Cloud requests: one session, one job at a time.
    Cloud,
    /// Minutes-long work: the update download, the installer, backups.
    Bulk,
}

impl Lane {
    pub const ALL: [Self; 3] = [Self::System, Self::Cloud, Self::Bulk];
}

impl ServiceJob {
    pub fn lane(&self) -> Lane {
        match self {
            Self::Dhcp(_) | Self::Internet(_) => Lane::System,
            Self::Cloud(_) => Lane::Cloud,
            Self::Download(_)
            | Self::Install
            | Self::Backup(_)
            | Self::TetradCheck
            | Self::TetradInstall(_) => Lane::Bulk,
        }
    }
}

/// A service worker result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceNotice {
    /// One `wpa_supplicant` control event line (`CTRL-EVENT-…`).
    Supplicant(String),
    Dhcp {
        run: DhcpRun,
        result: Result<AppliedLease, DhcpError>,
    },
    Internet(InternetResult),
    Download(Result<(), JobError>),
    Install(Result<InstallOutcome, JobError>),
    Cloud(Handoff<CloudReply>),
    Backup(Result<BackupDone, JobError>),
    TetradChecked(Result<Option<Offer>, String>),
    TetradInstalled(Result<(), String>),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JobError {
    #[error(transparent)]
    Update(#[from] UpdateError),
    #[error(transparent)]
    Install(#[from] InstallError),
    #[error(transparent)]
    Backup(#[from] BackupError),
    #[error(transparent)]
    Cloud(#[from] ApiError),
    #[error("platform does not lend the service backends together")]
    NoBackends,
    #[error("the service worker stopped before it answered")]
    WorkerStopped,
}

impl ServiceJob {
    /// The failure a job reports when its worker died before it answered, so no state (DHCP,
    /// the updater's `DownloadingData` or `Updating`) waits forever. Owned.
    pub fn lost(self) -> ServiceNotice {
        let stopped = JobError::WorkerStopped;
        match self {
            Self::Dhcp(run) => ServiceNotice::Dhcp {
                run,
                result: Err(DhcpError::Command(CommandError::Wait {
                    program: "udhcpc".into(),
                    reason: stopped.to_string(),
                })),
            },
            Self::Internet(work) => {
                ServiceNotice::Internet(work.failed(NetError::Io(stopped.to_string())))
            }
            Self::Download(_) => ServiceNotice::Download(Err(stopped)),
            Self::Install => ServiceNotice::Install(Err(stopped)),
            Self::Backup(_) => ServiceNotice::Backup(Err(stopped)),
            Self::TetradCheck => ServiceNotice::TetradChecked(Err(stopped.to_string())),
            Self::TetradInstall(_) => ServiceNotice::TetradInstalled(Err(stopped.to_string())),
            Self::Cloud(job) => ServiceNotice::Cloud(Handoff::new(
                job.take().map_or_else(CloudReply::gone, CloudJob::lost),
            )),
        }
    }
}

/// Cloud calls waiting for the session, with the signer of their batch.
struct PendingCloud {
    calls: Vec<CloudCall>,
    signer: Option<(String, SharcSigner)>,
}

pub struct Services {
    internet: InternetManager,
    wifi: WifiService,
    /// Loaded on first cloud use, from the saved tokens. `None` while a job holds it.
    cloud: Option<CloudApi>,
    /// A [`CloudJob`] holds the session; `cloud_signed_in` is its state at dispatch.
    cloud_out: bool,
    cloud_signed_in: bool,
    cloud_queue: VecDeque<PendingCloud>,
    /// NC editor DOWNLOAD hashes queued or on the worker, and the ended ones the UI has not
    /// taken yet ([`Self::take_capture_download_ends`], review r27 R1).
    capture_jobs: Vec<String>,
    capture_ends: Vec<CaptureDownloadEnd>,
    /// Unpairs queued, signing or on the worker; their networks wait ([`Self::forget_all`]).
    unpairing: u32,
    batch: Option<SigningBatch>,
    licence: Option<LicenceStep>,
    apply: Option<LicenceApply>,
    updater: Updater,
    /// Owned: the Tetrad Updates page.
    tetrad: tetrad_update::TetradUpdate,
    /// NTP result and the device time it was taken at; the only wall clock.
    wall: Option<(i64, Duration)>,
    update_check_due: bool,
    reported_status: UpdaterStatus,
    release_notes: Option<(String, String)>,
    download_progress: Option<f32>,
    install_progress: Option<f32>,
    /// Download tries of this offer, for the stock retry rule (0x0046bc94).
    download_attempts: u32,
    /// The stop flag of the download on the worker; [`Self::cancel_download`] sets it.
    download_cancel: Option<CancelToken>,
    /// The transport of the running download flow and its retries.
    download_via: Option<ControlTransport>,
    /// Cortex Control while it is connected and online ([`Self::set_control_route`]): the
    /// next cloud job and backup body take it.
    control_route: Option<ControlTransport>,
    /// Cancelled downloads still on the worker: their results are dropped, oldest first.
    cancelled_downloads: u32,
    /// The install pre-step `0x001ec83c(0x3f)` clears the input mask; the knob is held.
    inputs_masked: bool,
    /// `LIST_NETWORKS` after each scan, association change, connect and forget.
    saved: Vec<SavedNetwork>,
    /// Last internet probe result; `None` before the first.
    online: Option<bool>,
    /// Last failed attempt for the Connection page; a new attempt clears it.
    alert: Option<WifiAlert>,
    /// The network of the last attempt ([`WifiView::joining`]).
    joining: Option<String>,
    /// Last `GET /backups/v1` list.
    backups: Vec<CloudBackup>,
    /// Directory cloud folders listed so far ([`Self::browse_cloud`]).
    cloud_folders: Vec<((ProductKind, BrowseScope), CloudFolder)>,
    /// Counts the changes of the Downloads tables ([`Self::downloads_changes`]).
    downloads_changes: u64,
    /// The stored DOWNLOADS rows of the views, read on a worker (A3).
    pub(in crate::device) stored_downloads: downloads::StoredDownloads,
    /// Counts the changes of the cloud folders and the Downloads tables
    /// ([`Self::cloud_changes`], review r28 A8).
    cloud_changes: u64,
    /// Product ids with a download queued or on the worker ([`Self::downloads_in_flight`]).
    downloads_out: Vec<String>,
    /// The last Multi Select download of captures or IRs (census K2).
    download_batch: Option<DownloadBatch>,
    /// The preset downloads into setlist slots in progress ([`Self::download_presets`]).
    preset_downloads: Option<PresetBatch>,
    /// The Directory preset upload in flight and the ended ones the UI has not taken yet.
    preset_upload: Option<PresetUpload>,
    /// The user capture upload in flight ([`Self::upload_capture`]).
    capture_upload: Option<CaptureUpload>,
    /// The running batch upload ([`Self::upload_progress`]).
    upload_batch: Option<UploadBatch>,
    /// Upload states by local key ([`Self::upload_states`]).
    upload_states: std::collections::BTreeMap<String, UploadState>,
    /// The running multi-key cloud DELETE ([`Self::control_delete`]).
    delete_run: Option<DeleteRun>,
    /// The serial of the last run started ([`BatchView::serial`]) and the end of the last run.
    run_serial: u64,
    batch_end: Option<BatchEnd>,
    /// The serial of the last upload question asked ([`UploadQuestion::serial`]).
    upload_questions: u64,
    /// Captures a preset walk uploaded whose `cloudUUID` write the device has not ended
    /// ([`Self::capture_written`]).
    walk_writes: Vec<String>,
    /// A cloud capture download that waits for OVERWRITE / CANCEL ([`Self::capture_question`]).
    capture_ask: Option<CaptureAsk>,
    /// Queue presets whose captures go into Downloads first ([`Self::next_queue_dependency`]);
    /// the front one runs.
    queue_held: VecDeque<HeldPreset>,
    upload_ends: Vec<UploadEnd>,
    /// A factory reset forgets the saved networks when its unpair ends ([`Self::forget_all`]).
    forget_after_unpair: bool,
    /// The backup in progress.
    backup: Option<BackupFlow>,
    /// A Control backup create waiting for the DSP stamp key ([`Services::control_backup`]).
    stamp_wait: Option<backups::StampWait>,
    /// The library, taken off the device while a restore job removes and extracts the user
    /// data (review r18 F3): every library read and write fails until the flow ends.
    parked_library: Option<super::Library>,
    /// The last power request (Cortex Control, the UI, a timer, a job) during a restore
    /// extract; it runs when the restore ends (review r19 F3).
    held_power: Option<PowerAction>,
    /// A restore extracted the user data: memory is older than the files until the reload
    /// (`Device::reload_after_restore`), so no settings flush and no other backup runs (review
    /// r18 F1).
    restored: bool,
    /// The hardware a restore in progress comes from (`creator`); the first settings flush
    /// after a good extract moves the connector levels to this unit once (`0x480bcc`).
    restore_source: Option<tetrad_settings::Hardware>,
    /// Names for the "Corrupt Neural Capture" dialog (`Device::take_corrupt_captures`).
    pub(super) corrupt_captures: Vec<String>,
    /// The Cortex Control cloud backup request the running flow serves (r23-control MR 1).
    control_cloud: Option<ControlCloud>,
    /// Control's name for the create it starts (`createBackup(name, …)`, `0x150260`).
    control_backup_name: Option<String>,
    /// A Control 41 rename (`key`, name) that waits for its first list (`0x150c08`).
    control_rename: Option<(String, String)>,
    /// The seed-only capture cipher (secret mode 0), derived once per run from the stock
    /// ZenUI image (review r18 F15). `Arc`-shared, zeroed on the last drop.
    capture_seed: Option<DeviceCipher>,
    /// `CLOUD_CUSTOM_ENDPOINT` / `CLOUD_ENDPOINT` at start (0x00459e7c).
    cloud_stage: CloudStage,
}

impl std::fmt::Debug for Services {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Services")
            .field("wifi", &self.wifi)
            .field("updater", &self.updater)
            .field("signing", &self.batch.as_ref().map(|batch| &batch.jobs))
            .field("licence", &self.licence)
            .finish_non_exhaustive()
    }
}

impl Default for Services {
    fn default() -> Self {
        Self::new()
    }
}

/// Reset steps that only remove or rewrite files (`0x004de3ec`, `0x00459014`, `0x0073b898`).
/// Missing files are fine. Returns `false` for the other steps.
pub fn reset_files(files: &mut impl Files, step: ResetEffect) -> Result<bool, ServiceError> {
    let (remove, write): (&[&str], Option<(&str, &[u8])>) = match step {
        // `0x0154b08c`/`0x0154b090` are the two updater flags, `0x0154b080` the status file
        // (updater path slots); `presetsDirPath` gets `/media/p4/Presets` (`0xa1d448`).
        ResetEffect::ResetSystemFlags => (
            &[
                "/opt/neuraldsp/productiontestmode",
                paths::DOWNLOADED_FLAG,
                paths::AVAILABLE_FLAG,
                paths::STATUS_FILE,
            ],
            Some(("/opt/neuraldsp/presetsDirPath", b"/media/p4/Presets")),
        ),
        ResetEffect::RemoveDebugFiles => {
            (&["/media/p4/coredump", "/opt/neuraldsp/allow_sshd"], None)
        }
        ResetEffect::ResetBootCount => (&[], Some(("/opt/neuraldsp/boot_count.txt", b"0"))),
        // Default `QSettings` of organization `NeuralDSP`, application `ZenUI`; busybox init
        // starts ZenUI with `HOME=/` (I). Cleared and synced: no key stays, so it is removed.
        ResetEffect::ClearNativeConf => (&["/.config/NeuralDSP/ZenUI.conf"], None),
        _ => return Ok(false),
    };
    for path in remove {
        files.remove(Path::new(path))?;
    }
    if let Some((path, data)) = write {
        files.write(Path::new(path), data)?;
    }
    files.sync()?;
    Ok(true)
}

fn lend<B: ServiceBackends>(backends: &mut B) -> Result<ServiceIo<'_, B>, ServiceError> {
    backends.lend().ok_or(ServiceError::Unsupported(
        "platform does not lend the service backends together (ServiceBackends::lend)",
    ))
}

fn millis(now: Duration) -> u64 {
    u64::try_from(now.as_millis()).unwrap_or(u64::MAX)
}

fn notice(text: impl Into<String>) -> Effect {
    Effect::Notice(Subsystem::Services, text.into())
}

/// `QUuid(text)` is not null (`0x73c0c4`): five hex groups of 8-4-4-4-12, braces optional
/// (I: the Qt 4.8 parser), and not all zero.
fn is_uuid(text: &str) -> bool {
    let text = (text.strip_prefix('{'))
        .and_then(|text| text.strip_suffix('}'))
        .unwrap_or(text);
    let groups: Vec<&str> = text.split('-').collect();
    let shaped = groups.len() == 5
        && (groups.iter().zip([8, 4, 4, 4, 12]))
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()));
    shaped && text.bytes().any(|b| b.is_ascii_hexdigit() && b != b'0')
}

impl Services {
    pub fn new() -> Self {
        Self {
            internet: InternetManager::new(
                InternetProbe::new(STOCK_PROBE_TIMEOUT),
                TimeSync::new(STOCK_NTP_WAIT),
                STOCK_RECHECK_INTERVAL,
            ),
            wifi: WifiService::new(STOCK_ASSOCIATION_TIMEOUT_MS),
            cloud: None,
            cloud_out: false,
            cloud_signed_in: false,
            cloud_queue: VecDeque::new(),
            capture_jobs: Vec::new(),
            capture_ends: Vec::new(),
            unpairing: 0,
            batch: None,
            licence: None,
            apply: None,
            updater: Updater {
                state: UpdaterState::Idle,
                status: UpdaterStatus::None,
                offer: None,
            },
            tetrad: Default::default(),
            wall: None,
            update_check_due: false,
            reported_status: UpdaterStatus::None,
            release_notes: None,
            download_progress: None,
            install_progress: None,
            download_attempts: 0,
            download_cancel: None,
            download_via: None,
            control_route: None,
            cancelled_downloads: 0,
            inputs_masked: false,
            saved: Vec::new(),
            online: None,
            alert: None,
            joining: None,
            backups: Vec::new(),
            cloud_folders: Vec::new(),
            downloads_changes: 0,
            stored_downloads: Default::default(),
            cloud_changes: 0,
            downloads_out: Vec::new(),
            download_batch: None,
            preset_downloads: None,
            preset_upload: None,
            capture_upload: None,
            upload_batch: None,
            upload_states: Default::default(),
            delete_run: None,
            run_serial: 0,
            batch_end: None,
            upload_questions: 0,
            walk_writes: Vec::new(),
            capture_ask: None,
            queue_held: VecDeque::new(),
            upload_ends: Vec::new(),
            forget_after_unpair: false,
            backup: None,
            stamp_wait: None,
            parked_library: None,
            held_power: None,
            restored: false,
            restore_source: None,
            corrupt_captures: Vec::new(),
            control_cloud: None,
            control_backup_name: None,
            control_rename: None,
            capture_seed: None,
            cloud_stage: CloudStage::from_env(
                std::env::var("CLOUD_CUSTOM_ENDPOINT").ok().as_deref(),
                std::env::var("CLOUD_ENDPOINT").ok().as_deref(),
            ),
        }
    }

    /// DSP `0x11`, the reply to `0x35`: the signing material. This copy is zeroed either way,
    /// and `UsbMeter` zeroes its own bytes on drop. Stock zeroes none of them (O).
    pub fn on_usb_meter_update(&mut self, mut meter: UsbMeter) {
        let material = SigningMaterial::take(&mut meter.0);
        if let Some(batch) = self.batch.as_mut() {
            batch.material = Some(material);
        }
    }

    /// DSP `0x1f`, the reply to `0x4f`: the client id. This copy is zeroed either way, and
    /// `OpaqueCalibration` zeroes its own bytes on drop.
    pub fn on_tuner_calibrate(&mut self, mut reply: OpaqueCalibration) {
        let client_id = ClientIdBytes::take(&mut reply.0);
        if let Some(batch) = self.batch.as_mut() {
            batch.client_id = Some(client_id);
        }
    }

    /// DSP `0x21`, the answer to `0x50` (0x001edd10). 32 bytes are the licence data: they go
    /// to `PluginLicenses.ini` and then to the DSP (0x001f2390 → 0x001f0680). Any other length
    /// ends the activation. The caller zeroes `reply` after.
    pub fn on_midi_out_timer(
        &mut self,
        reply: &[u8],
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Vec<Effect> {
        if !matches!(self.licence, Some(LicenceStep::Dsp(_))) {
            return Vec::new();
        }
        if reply.len() != LICENCE_DATA_BYTES {
            return self.licence_failed("the DSP licence answer has the wrong length");
        }
        self.licence = None;
        settings.update(|s| s.licenses.payload = reply.to_vec().into());
        let mut effects: Vec<Effect> = flush(settings).into_iter().collect();
        effects.push(notice("QC has unapplied license data"));
        effects.extend(self.apply_licences(settings, now));
        effects
    }

    /// DSP `0x20`, the answer to `0x51` (0x001edec4): 8 bytes are the licensed-plugin mask,
    /// little-endian; any other length counts as no plugins, as stock. 0x001f18c0 replaces the
    /// licensed set with mask bits 0..61, takes the flags from bits 62/63 and, with bit 62,
    /// adds each id that was not licensed before (or is outside the known table) to `new`
    /// (`+0x54`); 0x001f123c then stores `previous` and `new`. Returns the plugin ids that
    /// became licensed (a Grid block with one forces a preset reload, 0x005280d8). A queued
    /// apply runs next. The caller zeroes `reply` after.
    pub fn on_tuner_timer(
        &mut self,
        reply: &[u8],
        settings: &mut DeviceSettings,
        licences: &mut Licences,
        now: Duration,
    ) -> (Vec<u32>, Vec<Effect>) {
        let Some(apply) = self.apply.take() else {
            return (Vec::new(), Vec::new());
        };
        // Owned (O), S2 on the QC (2026-10-15): the answer to Tetrad's `0x51` was 8 bytes with an
        // empty body. Stock takes 8 body bytes only (0x001edf24); a short answer must not wipe
        // the stored licences or `PluginLicenses.ini`, so they stay as they are.
        let Ok(mask) = <[u8; 8]>::try_from(reply).map(u64::from_le_bytes) else {
            let mut effects = vec![notice("the DSP licence apply answer has the wrong length")];
            if apply.again {
                effects.extend(self.apply_licences(settings, now));
            }
            return (Vec::new(), effects);
        };
        let before = *licences;
        *licences = Licences {
            licensed: mask & ((1 << LICENCE_PLUGIN_BITS) - 1),
            active: mask >> 62 & 1 == 1,
        };
        let added: Vec<u32> = licences
            .ids()
            .filter(|&id| before.licensed >> id & 1 == 0)
            .collect();
        let mut new = settings.settings().licenses.new.clone();
        if licences.active {
            let fresh = |id: &u32| !LICENSABLE_PLUGINS.contains(id) || added.contains(id);
            for id in licences.ids().filter(fresh) {
                let id = i32::try_from(id).unwrap_or_default();
                if !new.contains(&id) {
                    new.push(id);
                }
            }
            new.sort_unstable();
        }
        let previous = licences
            .ids()
            .filter_map(|id| i32::try_from(id).ok())
            .collect();
        settings.update(|s| {
            s.licenses.new = new;
            s.licenses.previous = previous;
        });
        let mut effects: Vec<Effect> = flush(settings).into_iter().collect();
        if apply.again {
            effects.extend(self.apply_licences(settings, now));
        }
        (added, effects)
    }

    pub fn wifi(&self) -> &WifiService {
        &self.wifi
    }

    pub fn updater(&self) -> &Updater {
        &self.updater
    }

    pub fn cloud(&self) -> Option<&CloudApi> {
        self.cloud.as_ref()
    }

    /// Wi-Fi bring-up (driver, interface, config, supplicant), the first supplicant requests,
    /// then the service tick.
    pub fn start(
        &mut self,
        backends: &mut impl ServiceBackends,
        _now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let io = lend(backends)?;
        wifi::bring_up(io.runner, io.files)?;
        wifi::initialize(io.supplicant)?;
        Ok(vec![Effect::StartTimer {
            timer: Timer::Services,
            after: TICK,
        }])
    }

    pub fn on_notice(
        &mut self,
        backends: &mut impl ServiceBackends,
        notice: ServiceNotice,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let mut io = match lend(backends) {
            Ok(io) => io,
            // The session comes home even when its reply cannot be applied.
            // Its token changes are saved and the queued flows end as failures (S-4).
            Err(error) => {
                let ServiceNotice::Cloud(reply) = &notice else {
                    return Err(error);
                };
                let Some(reply) = reply.take() else {
                    return Err(error);
                };
                self.cloud_out = false;
                self.cloud = reply.session;
                let mut effects = save_tokens(settings, reply.tokens);
                effects.extend(self.drop_pending(&error));
                effects.extend(self.batch_due());
                return Ok(effects);
            }
        };
        let now_ms = millis(now);
        Ok(match notice {
            ServiceNotice::Supplicant(line) => {
                let step = self
                    .wifi
                    .on_event(io.supplicant, io.runner, &line, now_ms)?;
                self.refresh_lists(io.supplicant, &step.notices)?;
                self.wifi_step(step, now)
            }
            ServiceNotice::Dhcp { run, result } => {
                let step = self
                    .wifi
                    .dhcp_finished(io.supplicant, io.runner, run, result)?;
                self.wifi_step(step, now)
            }
            ServiceNotice::Internet(result) => {
                let notices = self.internet.finish(result, now_ms);
                let mut effects = self.internet_effects(&notices, now);
                effects.extend(self.internet_jobs(now));
                effects
            }
            ServiceNotice::Download(result) => self.downloaded(result),
            ServiceNotice::Install(result) => self.installed(result),
            ServiceNotice::Cloud(reply) => self.cloud_done(&mut io, settings, reply.take(), now),
            ServiceNotice::Backup(result) => self.backup_done(io.files, settings, result, now),
            ServiceNotice::TetradChecked(result) => self.tetrad_checked(result),
            ServiceNotice::TetradInstalled(result) => self.tetrad_installed(result),
        }
        .into_iter()
        .chain(self.due_check(io.files, settings, now))
        .chain(self.batch_due())
        .collect())
    }

    /// Wi-Fi association timers, the internet recheck and NTP retry as worker jobs, a due
    /// internet recovery step, the signing batch, licence timers and a due update check. Never
    /// waits for the network.
    pub fn tick(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let io = lend(backends)?;
        let step = self.wifi.on_tick(io.supplicant, io.runner, millis(now))?;
        let mut effects = self.wifi_step(step, now);
        let disabled = settings.settings().general.disable_connection_check;
        let notices = self.internet.set_check_disabled(disabled, millis(now));
        effects.extend(self.internet_effects(&notices, now));
        effects.extend(self.internet_jobs(now));
        effects.extend(self.recover_internet(io.supplicant, io.runner, io.files, now));
        effects.extend(self.finish_batch(settings, now));
        effects.extend(self.expire_batch(io.files, settings, now));
        effects.extend(self.expire_stamp(now));
        effects.extend(self.licence_timers(io.files, now));
        effects.extend(self.due_check(io.files, settings, now));
        effects.extend(self.backup_poll_due(settings, now));
        effects.extend(self.progress(io.files)?);
        self.tetrad_progress(io.files)?;
        Ok(effects)
    }

    /// UI requests for Wi-Fi, the updater, the cloud account, cloud backups, Directory upload,
    /// the Directory cloud folders and the NC editor DOWNLOAD (w2b-services MR-9). Cloud calls
    /// go to the service worker. The support report is refused (no injected logs API key).
    pub fn on_ui(
        &mut self,
        backends: &mut impl ServiceBackends,
        command: ViewCommand,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let effects = self.ui_request(backends, command, settings, now);
        // A batch item that an answer ended (SKIP, or OVERWRITE with no account): the next one
        // starts, and the error is the ended item's notice.
        match (effects, self.batch_due()) {
            (Ok(effects), next) => Ok(effects.into_iter().chain(next).collect()),
            (Err(error), Some(next)) => Ok(vec![notice(error.to_string()), next]),
            (Err(error), None) => Err(error),
        }
    }

    fn ui_request(
        &mut self,
        backends: &mut impl ServiceBackends,
        command: ViewCommand,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        use ViewCommand as C;
        match command {
            C::WifiScan => {
                self.scan(backends)?;
                Ok(Vec::new())
            }
            C::WifiConnect { ssid, password } => {
                self.connect(backends, &ssid, password.as_deref(), now)?;
                Ok(Vec::new())
            }
            C::WifiDisconnect => {
                self.disconnect(backends)?;
                Ok(Vec::new())
            }
            C::WifiForget { ssid } => {
                let saved = self.saved_networks(backends)?;
                let network = saved
                    .iter()
                    .find(|network| network.ssid == ssid)
                    .ok_or(ServiceError::NotReady("that network is not saved"))?;
                self.forget(backends, Some(network.id))?;
                Ok(Vec::new())
            }
            C::Device {
                action: DeviceAction::ResetWifi,
            } => {
                self.forget(backends, None)?;
                Ok(Vec::new())
            }
            C::DeleteBackup { id } => self.delete_backup(backends, settings, id, now),
            C::Updater { command } => match command {
                UpdaterCommand::Check => {
                    self.check_for_update(backends, settings, None, false, now)
                }
                UpdaterCommand::CheckAndDownload => {
                    self.check_for_update(backends, settings, None, true, now)
                }
                UpdaterCommand::Install => self.install(),
                UpdaterCommand::CancelDownload => Ok(self.cancel_download()),
            },
            C::TetradUpdate { command } => match command {
                TetradUpdateCommand::Check => Ok(self.check_tetrad_update()),
                TetradUpdateCommand::Install => self.install_tetrad_update(),
            },
            C::CloudLogin { email, password } => {
                self.sign_in(backends, settings, &email, &password, now)
            }
            C::Device {
                action: DeviceAction::CloudLogout,
            } => self.unpair(backends, settings, AfterUnpair::SignOut, now),
            // M1186 and Control Logs (44): stock packs the logs (0x00487f88) and uploads them
            // (`generateLogUrl` 0x006bcb30, PUT). O (owner decision, 2026-10-17): Tetrad never
            // sends support reports.
            C::Device {
                action: DeviceAction::SendReport,
            } => Err(ServiceError::Unsupported(
                "support reports are turned off in Tetrad",
            )),
            C::CreateBackup { name } => {
                self.create_named_backup(backends, settings, Some(name), now)
            }
            C::RenameBackup { id, name } => self.rename_backup(backends, settings, id, name, now),
            C::UpdateBackup { id } => self.update_backup(backends, settings, id, now),
            C::Upload { items } => self.upload(backends, settings, &items, now),
            // Review r39 K3: a device answer reaches only the device question of its kind.
            C::UploadOverwrite { overwrite } => {
                let device = (self.upload_prompts())
                    .find(|q| !q.from_control && q.kind == ProductKind::Preset)
                    .map(|q| q.serial);
                match device {
                    Some(serial) => {
                        self.answer_upload(backends, settings, serial, false, overwrite, now)
                    }
                    None => Ok(Vec::new()),
                }
            }
            C::RestoreBackup { id } => self.restore_backup(backends, settings, id, now),
            C::DownloadCapture { hash } => self.download_capture(backends, settings, hash, now),
            C::BrowseCloud {
                kind,
                scope,
                refresh,
            } => {
                let (kind, scope) = (product_kind(kind), browse_scope(scope));
                self.browse_cloud(backends, settings, kind, scope, refresh, now)
            }
            C::DownloadProduct { id } => self.download_product(backends, settings, id, now),
            C::DownloadProducts { ids } => self.download_products(backends, settings, ids, now),
            C::CaptureOverwrite { overwrite } => Ok(self.answer_capture(overwrite)),
            C::CaptureUploadOverwrite { overwrite } => {
                let device = (self.upload_prompts())
                    .find(|q| !q.from_control && q.kind == ProductKind::NeuralCapture)
                    .map(|q| q.serial);
                match device {
                    Some(serial) => {
                        self.answer_upload(backends, settings, serial, false, overwrite, now)
                    }
                    None => Ok(Vec::new()),
                }
            }
            // The device Cancel of the batch progress dialog (`0x780390` → `0x843f2c`): the
            // stop flag of the running run.
            C::CancelUpload => {
                self.cancel_bulk_run();
                Ok(Vec::new())
            }
            _ => Err(ServiceError::Unsupported("not a services request")),
        }
    }
}

/// All five stock message-60 senders use UPDATE and request ID 0 (`0x6e0444`).
fn update_message() -> UpdaterMessage {
    UpdaterMessage {
        action: Some(MessageAction::Update.into()),
        request_id: Some(0),
        ..UpdaterMessage::default()
    }
}

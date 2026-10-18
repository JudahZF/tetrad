//! Cloud and local backups and restores, Services side.

use super::*;
use qc_format::crypto::NdspCipher;
use std::hash::BuildHasher;
use tetrad_services::backup::local::STAMP_BYTES;

/// Where a cloud backup is packed before its upload (O).
pub(super) const UPLOAD_ARCHIVE: &str = "/media/p4/tetrad_backup_upload.tar.gz";
/// Where a Control backup is packed before it is wrapped (O; the job removes it).
pub(super) const LOCAL_ARCHIVE: &str = "/media/p4/tetrad_local_backup.tar.gz";
/// The `0x43` text that asks the DSP for the creator stamp key (`0x3fc054`: nine characters,
/// XOR `0x3a` in the binary). A request selector, not a key: the key stays in the DSP and
/// comes back in response `0x18` for each backup.
const STAMP_KEY_REQUEST: &[u8] = b"386260441";
/// The stamp key length; stock drops any other reply (`0x3fcb9c`).
const STAMP_KEY_BYTES: usize = 16;

/// A Cortex Control backup create that waits for the DSP stamp key (`0x3fc054` → `0x3fcb9c`).
pub(super) struct StampWait {
    header: Box<LocalHeader>,
    serial: String,
    sent: Duration,
}

/// The local backup creator stamp as stock builds it (`0x47f648`): a random byte before each
/// byte of `1` and the serial, random bytes up to [`STAMP_BYTES`], then the stamp key cipher
/// (`0x6ade54` mode 2: [`NdspCipher`], `EVP_BytesToKey` SHA-1 × 10 over the key). `None`
/// when the serial does not fill exactly [`STAMP_BYTES`] (stock serials have 9 characters).
fn seal_stamp(key: &[u8], serial: &str, mut noise: impl FnMut() -> u8) -> Option<CreatorStamp> {
    let mut plain = zeroize::Zeroizing::new(Vec::with_capacity(STAMP_BYTES));
    for byte in std::iter::once(1).chain(serial.bytes()) {
        plain.extend([noise(), byte]);
    }
    while plain.len() < STAMP_BYTES {
        plain.push(noise());
    }
    NdspCipher::derive(key).apply_keystream(&mut plain);
    CreatorStamp::from_slice(&plain)
}

/// The stage directory (`DAT_0154b120`) next to `zenui.db`: the create job puts a re-keyed
/// database copy there and removes it after the pack.
pub const BACKUP_STAGE: &str = "BackupTmpDir";

/// The staged `zenui.db` of a create (`0x4763b4`, re-key `0x4770f4`). The bulk worker makes
/// it, not the device thread (review r18 F8): a stepped copy of `database` in `dir` that
/// does not block device writes (review r19 F4), its captures re-keyed to the global
/// cipher. A failed copy fails the create; the live file is never packed (review r19 F5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    pub database: PathBuf,
    pub dir: PathBuf,
    /// The re-key ciphers, or why there are none (the copy keeps unit-bound captures).
    pub keys: Result<Handoff<CaptureKeys>, String>,
}

/// The two capture ciphers of a backup re-key: `Arc` handles to the device's keys, never key
/// bytes ([`DeviceCipher`]).
pub struct CaptureKeys {
    /// Secret mode 0 (seed only).
    pub global: DeviceCipher,
    /// Secret mode 1 (seed and serial): this unit.
    pub local: DeviceCipher,
}

/// Backup work on [`Lane::Bulk`]. `Debug` leaves out presigned URLs and backup text.
#[derive(Clone, PartialEq, Eq)]
pub enum BackupJob {
    /// Pack the user data into `dest` (`0x4774d8`) with the [`Stage`] database; `None`
    /// (no open database on the device, so nothing writes it) packs the file as it is.
    Create {
        dest: PathBuf,
        stage: Option<Box<Stage>>,
    },
    /// Pack as [`Self::Create`], then wrap the archive in the Control backup file
    /// (`0x47a1c8`). The header is boxed to keep [`ServiceJob`] small.
    CreateLocal {
        stage: Option<Box<Stage>>,
        header: Box<LocalHeader>,
        stamp: CreatorStamp,
    },
    /// Read a received Control backup file (`0x4080cc`, `0x4072fc`) into
    /// `downloaded_backup.tar.gz`, then restore it (`0x406844`).
    RestoreLocal { json: String },
    /// Wrap the packed archive in the cloud backup file (`0x47a1c8` without the stamp), PUT
    /// it to its presigned target (`0x30b73c`; through Cortex Control `0x1458ac`), then remove
    /// the archive.
    Upload {
        archive: PathBuf,
        target: BackupUrl,
        header: Box<LocalHeader>,
        via_control: Option<ControlTransport>,
    },
    /// GET the backup body from its presigned URL into `downloaded_backup.tar.gz` (through
    /// Cortex Control `0x1417b4`).
    Fetch {
        url: String,
        via_control: Option<ControlTransport>,
    },
    /// `0x406844` on the archive.
    Restore { archive: PathBuf },
}

impl fmt::Debug for BackupJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create { dest, stage } => f
                .debug_struct("Create")
                .field("dest", dest)
                .field("stage", stage)
                .finish(),
            Self::CreateLocal { stage, header, .. } => f
                .debug_struct("CreateLocal")
                .field("stage", stage)
                .field("header", header)
                .finish_non_exhaustive(),
            Self::RestoreLocal { json } => f
                .debug_struct("RestoreLocal")
                .field("bytes", &json.len())
                .finish(),
            Self::Upload { archive, .. } => f
                .debug_struct("Upload")
                .field("archive", archive)
                .finish_non_exhaustive(),
            Self::Fetch { .. } => f.debug_struct("Fetch").finish_non_exhaustive(),
            Self::Restore { archive } => {
                f.debug_struct("Restore").field("archive", archive).finish()
            }
        }
    }
}

/// A Cortex Control backup request (LocalBackup 40; r16-services MR-4). The Control bridge
/// sends it as [`super::Effect::ControlBackup`]; the result comes back as
/// [`super::Effect::ControlBackupDone`]. `Debug` leaves out the backup text.
#[derive(Clone, PartialEq, Eq)]
pub enum ControlBackup {
    /// Pack the user data and wrap it in the backup file (`0x17d104` → `0x4774d8`,
    /// `0x47a1c8`). Control then sends the file (`0x17e3e8`).
    Create(LocalHeader),
    /// Restore the backup file Control sent (`0x4080cc` → `0x406844`). Stock then reloads
    /// (`0x3fcf64`); Tetrad restarts (review r18 F1).
    Restore { json: String },
}

impl fmt::Debug for ControlBackup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create(header) => f.debug_tuple("Create").field(header).finish(),
            Self::Restore { json } => f
                .debug_struct("Restore")
                .field("bytes", &json.len())
                .finish(),
        }
    }
}

/// The end of a [`ControlBackup`], for the Control bridge. `Debug` leaves out the backup text.
#[derive(Clone, PartialEq, Eq)]
pub enum ControlBackupDone {
    /// The backup file text, for LocalBackup UPDATE chunks.
    Created { json: String },
    /// Members written; the device restarts next.
    Restored { members: usize },
    /// Busy, refused (no `--persist`) or failed.
    Failed(String),
}

impl fmt::Debug for ControlBackupDone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Created { json } => f
                .debug_struct("Created")
                .field("bytes", &json.len())
                .finish(),
            Self::Restored { members } => f
                .debug_struct("Restored")
                .field("members", members)
                .finish(),
            Self::Failed(reason) => f.debug_tuple("Failed").field(reason).finish(),
        }
    }
}

/// A finished [`BackupJob`]. `notes` are stage problems for the notice log. `Debug` leaves
/// out the backup text and its creator stamp (review r18 F9).
#[derive(Clone, PartialEq, Eq)]
pub enum BackupDone {
    Created {
        summary: Summary,
        notes: Vec<String>,
    },
    /// The Control backup file text.
    LocalCreated {
        json: String,
        notes: Vec<String>,
    },
    /// The file is on the cloud; `notes` holds a failed clean-up (health r34 #4).
    Uploaded {
        notes: Vec<String>,
    },
    Fetched,
    /// Members written.
    Restored(usize),
}

impl fmt::Debug for BackupDone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Created { summary, notes } => f
                .debug_struct("Created")
                .field("summary", summary)
                .field("notes", notes)
                .finish(),
            Self::LocalCreated { json, notes } => f
                .debug_struct("LocalCreated")
                .field("bytes", &json.len())
                .field("notes", notes)
                .finish(),
            Self::Uploaded { notes } => f.debug_struct("Uploaded").field("notes", notes).finish(),
            Self::Fetched => f.write_str("Fetched"),
            Self::Restored(members) => f.debug_tuple("Restored").field(members).finish(),
        }
    }
}

/// What a cloud backup pack is for (`0x3f62e0(name, backupUrl, update)` → `0x3fc1e4`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PackFor {
    /// A new backup under this name (`0x303300`).
    Create(String),
    /// A new body for the listed backup at this server path (`0x303564`).
    Update(String),
}

/// The kind of a Cortex Control cloud backup request (CloudBackup 41).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ControlCloud {
    List,
    Create,
    Delete,
    Restore,
    Update,
    Rename,
}

/// The cloud backup in progress; one at a time (stock busy flag `+0x180`, 0x00406844).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BackupFlow {
    /// Create or Update: packing (0x004774d8), then the upload target.
    Packing(PackFor),
    /// `generateBackupUrl` (0x00303300) or `<backupUrl>/update` (0x00303564). `update`
    /// (here and below): Backup ⋮ Update, which ends with no Control answer (review r35 G7).
    Targeting {
        update: bool,
    },
    /// The backup file PUT (`0x30b73c`); `path` is the target's `metadata.backupUrl`.
    Uploading {
        path: String,
        update: bool,
    },
    /// The `<path>/status` poll (`0x30a83c` → `0x3084fc`): from `since` (the PUT reply), one
    /// GET each second until "completed", an error or 60 s. `next` is when the next GET goes;
    /// `None` while one is out.
    Validating {
        path: String,
        since: Duration,
        next: Option<Duration>,
        update: bool,
    },
    /// Restore: the backup's download url (0x0030d4b0), then its body.
    Locating,
    Fetching,
    /// 0x00406844.
    Restoring,
    /// A Cortex Control request; its result goes back to Control.
    Control(ControlBackup),
}

impl BackupFlow {
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::Packing(_)
            | Self::Targeting { .. }
            | Self::Uploading { .. }
            | Self::Validating { .. } => "Creating backup",
            Self::Locating | Self::Fetching => "Downloading backup",
            Self::Restoring | Self::Control(ControlBackup::Restore { .. }) => "Restoring backup",
            Self::Control(ControlBackup::Create { .. }) => "Creating backup",
        }
    }
}

/// `QC-Backup-YYYYMMDD-HHMM` in UTC. O: the stock create takes a name (0x004774d8 arguments)
/// from an entry the Backups page does not have here.
pub(super) fn backup_name(unix: i64) -> String {
    let [year, month, day, hour, minute, _] = utc_parts(unix);
    format!("QC-Backup-{year:04}{month:02}{day:02}-{hour:02}{minute:02}")
}

/// UTC year, month, day, hour, minute and second of a Unix time.
pub(in crate::device) fn utc_parts(unix: i64) -> [i64; 6] {
    let (days, secs) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    // Days to civil date (H. Hinnant, `civil_from_days`).
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    [year, month, day, secs / 3_600, secs % 3_600 / 60, secs % 60]
}

/// One cloud backup as the Backups page lists it: a `BackupData` item (`0x30eb7c` →
/// `0x306794`, [`BackupData`]); `title` is the decoded `backupName`, `path` the `backupUrl`,
/// `detail` the `timestamp` as `yyyy-MM-ddThh:mm:ssZ` (UTC), `id` an FNV-1a hash of the
/// `backupKey` (stable over a rename). An item that does not parse ("Failed to load BackupData
/// from JSON") or comes from another client type ("Ignored backup from client", `0x45bb04`:
/// `quad_cortex`, `quad_cortex_mini`) is left out. The download time is not in the reply:
/// stock stores `currentDateTimeUtc()` under QSettings `BackupDownloadTimes/<backup>` when a
/// download completes (`0x407908`, save `0x30367c`, load `0x303a38`); Tetrad keeps none, so it
/// stays `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudBackup {
    pub id: u64,
    pub path: String,
    pub title: String,
    pub detail: String,
    pub downloaded: Option<String>,
}

impl CloudBackup {
    pub(super) fn parse(item: &serde_json::Value) -> Option<Self> {
        let data = BackupData::parse(item).ok()?;
        let clients = [ClientType::QuadCortex, ClientType::QuadCortexMini];
        if !clients
            .iter()
            .any(|client| client.as_str() == data.client_type)
        {
            return None;
        }
        let id = (data.key.bytes()).fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
        let [year, month, day, hour, minute, second] = utc_parts(data.timestamp);
        Some(Self {
            id,
            path: data.url,
            title: data.name,
            detail: format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"),
            downloaded: None,
        })
    }
}

impl Services {
    /// `GET /backups/v1` for the Backups page on the worker. Items without a name are skipped.
    pub fn refresh_backups(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        self.unix_seconds(now)?;
        lend(backends)?;
        Ok(self.cloud_job(
            settings,
            vec![CloudCall::Backups { delete: None }],
            None,
            now,
        ))
    }

    /// DELETE one listed backup by its server path, then list again.
    pub fn delete_backup(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        id: u64,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let path = self
            .backups
            .iter()
            .find(|backup| backup.id == id)
            .map(|backup| backup.path.clone())
            .ok_or(ServiceError::NotReady("that backup is not listed"))?;
        self.unix_seconds(now)?;
        lend(backends)?;
        let call = CloudCall::Backups { delete: Some(path) };
        Ok(self.cloud_job(settings, vec![call], None, now))
    }

    /// Backup ⋮ Edit name (`onBackupEditNameConfirmed` lambda `0x3fb0f4`): `POST
    /// <path>/rename` with the new name (`0x30d054`), then list again (I: stock's done
    /// callback `0x403194` is not traced). Over Cortex Control it goes as ProductForward 29
    /// (`0x140824` → `0x30d054`, [`Services::set_control_route`]); a blank name is refused. The
    /// Control 41 rename lists first (`Self::control_rename_backup`).
    pub fn rename_backup(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        id: u64,
        name: String,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if name.trim().is_empty() {
            return Err(ServiceError::NotReady("a backup name is empty"));
        }
        let path = (self.backups.iter())
            .find(|backup| backup.id == id)
            .map(|backup| backup.path.clone())
            .ok_or(ServiceError::NotReady("that backup is not listed"))?;
        self.unix_seconds(now)?;
        lend(backends)?;
        let call = CloudCall::BackupRename { path, name };
        Ok(self.cloud_job(settings, vec![call], None, now))
    }

    /// Cortex Control 41 rename (`0x150c08`, review r39 K11): a list first (failed → 15);
    /// the list reply finds `key` ([`Self::control_rename_listed`]). O: a blank name is
    /// refused (6) before the list.
    pub(in crate::device) fn control_rename_backup(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        key: String,
        name: String,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if name.trim().is_empty() {
            return Err(ServiceError::NotReady("a backup name is empty"));
        }
        let effects = self.refresh_backups(backends, settings, now)?;
        self.control_rename = Some((key, name));
        Ok(effects)
    }

    /// The list before a Control rename (`0x150c08`): a `key` that is not listed answers 7;
    /// else `POST <key>/rename` (`0x140824`). Its good reply lists again (`0x150ab8` →
    /// `0x152218`). `None`: no Control rename waits for a list.
    pub(super) fn control_rename_listed(
        &mut self,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Option<Vec<Effect>> {
        let (key, name) = self.control_rename.take()?;
        if self.control_cloud != Some(ControlCloud::Rename) {
            return None;
        }
        if !self.backups.iter().any(|backup| backup.path == key) {
            let done = super::control::CloudBackupDone::NotFound;
            return Some(
                self.control_cloud_end(&[ControlCloud::Rename], done)
                    .into_iter()
                    .collect(),
            );
        }
        let call = CloudCall::BackupRename { path: key, name };
        Some(self.cloud_job(settings, vec![call], None, now))
    }

    /// The rename reply (`0x150ab8`): good → the list again; failed → 6.
    pub(super) fn backup_renamed(
        &mut self,
        settings: &mut DeviceSettings,
        result: Result<(), ServiceError>,
        now: Duration,
    ) -> Vec<Effect> {
        match result {
            Ok(()) => self.cloud_job(
                settings,
                vec![CloudCall::Backups { delete: None }],
                None,
                now,
            ),
            Err(error) => self.backup_rename_failed(&error),
        }
    }

    pub(super) fn backup_rename_failed(&mut self, error: &dyn Display) -> Vec<Effect> {
        let done = super::control::CloudBackupDone::Failed(error.to_string());
        let control = self.control_cloud_end(&[ControlCloud::Rename], done);
        std::iter::once(notice(format!("backup rename failed: {error}")))
            .chain(control)
            .collect()
    }

    /// A failed backup list: a Control list or delete fails (16, 13); a Control rename
    /// answers 15 (`0x150c08` before the POST, `0x152218` after it).
    pub(super) fn backups_list_failed(&mut self, error: &dyn Display) -> Vec<Effect> {
        use super::control::CloudBackupDone;
        self.control_rename = None;
        let lists = [ControlCloud::List, ControlCloud::Delete];
        let failed = CloudBackupDone::Failed(error.to_string());
        let control = (self.control_cloud_end(&lists, failed)).or_else(|| {
            let failed = CloudBackupDone::ListFailed(error.to_string());
            self.control_cloud_end(&[ControlCloud::Rename], failed)
        });
        std::iter::once(notice(format!("cloud backups request failed: {error}")))
            .chain(control)
            .collect()
    }

    pub fn backups(&self) -> &[CloudBackup] {
        &self.backups
    }

    /// A Cortex Control cloud backup create: [`Self::create_named_backup`] with the name of
    /// the Control request, if any.
    pub fn create_backup(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let name = self.control_backup_name.take();
        self.create_named_backup(backends, settings, name, now)
    }

    /// Backups page CREATE with the `BackupEditDialog` name (`onBackupCreateNameConfirmed`,
    /// `0x3f8568`): pack the user data (0x004774d8) on the bulk worker, then
    /// `generateBackupUrl` (0x00303300) for `name`, the upload and a list refresh. O: no name
    /// (or a blank one) is `QC-Backup-YYYYMMDD-HHMM`.
    pub fn create_named_backup(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        name: Option<String>,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if self.backup.is_some() || self.restored {
            return Err(ServiceError::NotReady("a backup is in progress"));
        }
        let unix = self.unix_seconds(now)?;
        lend(backends)?;
        if !self.signed_in(settings)? {
            return Err(ServiceError::NotReady(
                "cloud backups need a signed-in account",
            ));
        }
        let name =
            (name.filter(|name| !name.trim().is_empty())).unwrap_or_else(|| backup_name(unix));
        self.backup = Some(BackupFlow::Packing(PackFor::Create(name)));
        Ok(vec![Effect::Service(ServiceJob::Backup(
            BackupJob::Create {
                dest: UPLOAD_ARCHIVE.into(),
                stage: None,
            },
        ))])
    }

    /// Backup ⋮ Update PROCEED (`0x4045ac` → `0x403fb0`): pack the user data as a create does
    /// (`0x3f62e0(name, backupUrl, 1)` → `0x4774d8`), then `GET <backupUrl>/update`
    /// (`0x303564`), the backup file PUT and the `/status` poll as for a create, then a list
    /// refresh. The chain has no DELETE or rename: the old cloud backup is never removed from
    /// the device (C). O: the backup must be in the last list (stock refreshes the list first
    /// and goes on only for the same `backupKey`, `0x403598` → `0x3f58e4`; Tetrad keys the list
    /// by `backupKey`).
    pub fn update_backup(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        id: u64,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if self.backup.is_some() || self.restored {
            return Err(ServiceError::NotReady("a backup is in progress"));
        }
        let path = (self.backups.iter())
            .find(|backup| backup.id == id)
            .map(|backup| backup.path.clone())
            .ok_or(ServiceError::NotReady("that backup is not listed"))?;
        self.unix_seconds(now)?;
        lend(backends)?;
        if !self.signed_in(settings)? {
            return Err(ServiceError::NotReady(
                "cloud backups need a signed-in account",
            ));
        }
        self.backup = Some(BackupFlow::Packing(PackFor::Update(path)));
        Ok(vec![Effect::Service(ServiceJob::Backup(
            BackupJob::Create {
                dest: UPLOAD_ARCHIVE.into(),
                stage: None,
            },
        ))])
    }

    /// Backups page RESTORE of a listed backup: its download url, the body into
    /// `downloaded_backup.tar.gz`, then 0x00406844 on the bulk worker.
    pub fn restore_backup(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        id: u64,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if self.backup.is_some() || self.restored {
            return Err(ServiceError::NotReady("a backup is in progress"));
        }
        let path = self
            .backups
            .iter()
            .find(|backup| backup.id == id)
            .map(|backup| backup.path.clone())
            .ok_or(ServiceError::NotReady("that backup is not listed"))?;
        self.unix_seconds(now)?;
        lend(backends)?;
        self.backup = Some(BackupFlow::Locating);
        Ok(self.cloud_job(settings, vec![CloudCall::BackupSource { path }], None, now))
    }

    /// DSP `0x18`, the reply to `0x43`: the stamp key of a waiting create. It seals the stamp
    /// and starts the pack; the key is zeroed when `reply` drops, as stock wipes its copy.
    pub fn on_usb_meter_output(&mut self, reply: UsbMeter) -> Vec<Effect> {
        let Some(wait) = self.stamp_wait.take() else {
            return Vec::new();
        };
        let noise = std::hash::RandomState::new();
        let mut count = 0u32;
        let stamp = seal_stamp(&reply.0[..STAMP_KEY_BYTES], &wait.serial, || {
            count += 1;
            noise.hash_one(count) as u8
        });
        match stamp {
            Some(stamp) => vec![Effect::Service(ServiceJob::Backup(
                BackupJob::CreateLocal {
                    stage: None,
                    header: wait.header,
                    stamp,
                },
            ))],
            None => self.stamp_failed("the device serial does not fit the creator stamp"),
        }
    }

    /// Fails a create whose stamp key did not arrive in time. Stock waits forever (O).
    pub(super) fn expire_stamp(&mut self, now: Duration) -> Vec<Effect> {
        match self
            .stamp_wait
            .take_if(|wait| now.saturating_sub(wait.sent) >= SIGNING_REPLY_TIMEOUT)
        {
            Some(_) => self.stamp_failed("no DSP creator stamp key reply (0x18)"),
            None => Vec::new(),
        }
    }

    fn stamp_failed(&mut self, error: &str) -> Vec<Effect> {
        self.backup = None;
        vec![Effect::ControlBackupDone(ControlBackupDone::Failed(
            error.to_owned(),
        ))]
    }

    /// A Cortex Control backup: one job on the bulk worker; the stock busy flag (`+0x180`)
    /// refuses it while another backup runs.
    /// A create first asks the DSP for the stamp key ([`Self::on_usb_meter_output`]).
    pub fn control_backup(
        &mut self,
        backends: &mut impl ServiceBackends,
        request: ControlBackup,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if self.backup.is_some() || self.restored {
            return Err(ServiceError::NotReady("a backup is in progress"));
        }
        let effect = match &request {
            ControlBackup::Create(header) => {
                let serial = Serial::read(backends.files())?;
                let text = TextPrefix::try_from(STAMP_KEY_REQUEST).expect("nine characters");
                self.stamp_wait = Some(StampWait {
                    header: Box::new(header.clone()),
                    serial: serial.text,
                    sent: now,
                });
                Effect::Dsp(DspCommand::Request(Request::StampKey(text)))
            }
            ControlBackup::Restore { json } => {
                let creator = backup::local::creator(json).unwrap_or_default();
                self.restore_source = Some(source_hardware(&creator));
                Effect::Service(ServiceJob::Backup(BackupJob::RestoreLocal {
                    json: json.clone(),
                }))
            }
        };
        lend(backends)?;
        self.backup = Some(BackupFlow::Control(request));
        Ok(vec![effect])
    }

    /// A backup job ended: the next step of its flow, or the flow's end.
    pub(super) fn backup_done(
        &mut self,
        files: &mut impl Files,
        settings: &mut DeviceSettings,
        result: Result<BackupDone, JobError>,
        now: Duration,
    ) -> Vec<Effect> {
        let mut effects = self.backup_step(files, settings, result, now);
        if !self.restoring() {
            effects.extend(self.held_power.take().map(Effect::Power));
        }
        effects
    }

    pub(super) fn backup_step(
        &mut self,
        files: &mut impl Files,
        settings: &mut DeviceSettings,
        result: Result<BackupDone, JobError>,
        now: Duration,
    ) -> Vec<Effect> {
        match (self.backup.take(), result) {
            (Some(BackupFlow::Packing(pack)), Ok(BackupDone::Created { notes, .. })) => {
                let update = matches!(pack, PackFor::Update(_));
                self.backup = Some(BackupFlow::Targeting { update });
                let mut effects: Vec<_> = notes
                    .into_iter()
                    .map(|note| Effect::Notice(Subsystem::Library, note))
                    .collect();
                let target = match pack {
                    PackFor::Create(name) => CloudCall::BackupTarget { name },
                    PackFor::Update(path) => CloudCall::BackupUpdate { path },
                };
                effects.extend(self.cloud_job(settings, vec![target], None, now));
                effects
            }
            (Some(BackupFlow::Uploading { path, update }), Ok(BackupDone::Uploaded { notes })) => {
                let mut effects: Vec<_> = notes.into_iter().map(notice).collect();
                let flow = BackupFlow::Validating {
                    path,
                    since: now,
                    next: None,
                    update,
                };
                effects.extend(self.backup_poll(settings, flow, now));
                effects
            }
            (Some(BackupFlow::Fetching), Ok(BackupDone::Fetched)) => {
                self.backup = Some(BackupFlow::Restoring);
                vec![Effect::Service(ServiceJob::Backup(BackupJob::Restore {
                    archive: backup::DOWNLOADED_BACKUP.into(),
                }))]
            }
            (Some(BackupFlow::Restoring), Ok(BackupDone::Restored(members))) => {
                self.restored(files, settings.hardware(), members, None)
            }
            (
                Some(BackupFlow::Control(ControlBackup::Create(_))),
                Ok(BackupDone::LocalCreated { json, notes }),
            ) => {
                let mut effects: Vec<_> = notes
                    .into_iter()
                    .map(|note| Effect::Notice(Subsystem::Library, note))
                    .collect();
                effects.push(Effect::ControlBackupDone(ControlBackupDone::Created {
                    json,
                }));
                effects
            }
            (
                Some(BackupFlow::Control(ControlBackup::Restore { .. })),
                Ok(BackupDone::Restored(members)),
            ) => self.restored(
                files,
                settings.hardware(),
                members,
                Some(ControlBackupDone::Restored { members }),
            ),
            (Some(flow @ BackupFlow::Control(_)), Err(error)) => {
                let mut effects = self.backup_failed(&error.to_string());
                if matches!(flow, BackupFlow::Control(ControlBackup::Restore { .. })) {
                    effects.push(Effect::LibraryReopen);
                }
                effects.push(Effect::ControlBackupDone(ControlBackupDone::Failed(
                    error.to_string(),
                )));
                effects
            }
            (Some(BackupFlow::Restoring), Err(error)) => {
                let mut effects = self.backup_failed(&error.to_string());
                effects.push(Effect::LibraryReopen);
                effects
            }
            (Some(BackupFlow::Fetching), Err(error)) => {
                self.backup_failed(&format!("{} ({error})", BackupError::Empty))
            }
            (Some(_), Err(error)) => self.backup_failed(&error.to_string()),
            (flow, Ok(done)) => {
                self.backup = flow;
                vec![notice(format!("backup result without its step: {done:?}"))]
            }
            (None, Err(error)) => vec![notice(format!("backup: {error}"))],
        }
    }

    /// One `<path>/status` GET of the poll of `flow` ([`BackupFlow::Validating`], `0x3084fc`):
    /// 60 s after its `since` the create fails ("Backup upload validation timeout!").
    pub(super) fn backup_poll(
        &mut self,
        settings: &mut DeviceSettings,
        flow: BackupFlow,
        now: Duration,
    ) -> Vec<Effect> {
        let BackupFlow::Validating {
            path,
            since,
            update,
            ..
        } = flow
        else {
            return Vec::new();
        };
        if now.saturating_sub(since) > BACKUP_POLL_LIMIT {
            return self.backup_failed("Backup upload validation timeout!");
        }
        let call = CloudCall::BackupStatus { path: path.clone() };
        self.backup = Some(BackupFlow::Validating {
            path,
            since,
            next: None,
            update,
        });
        self.cloud_job(settings, vec![call], None, now)
    }

    /// The poll's next GET when it is due ([`Self::tick`]).
    pub(super) fn backup_poll_due(
        &mut self,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Vec<Effect> {
        match self.backup.take() {
            Some(
                flow @ BackupFlow::Validating {
                    next: Some(next), ..
                },
            ) if next <= now => self.backup_poll(settings, flow, now),
            flow => {
                self.backup = flow;
                Vec::new()
            }
        }
    }

    /// `0x404684` with ok: the list refresh and the Control answer of a create or an update
    /// (r39-control MR 2, census L1). Stock shows no result text (re-r34 §3.7); Tetrad logs one
    /// (review r35 G7: "Backup updated" for an update).
    pub(super) fn backup_created(
        &mut self,
        settings: &mut DeviceSettings,
        notes: Vec<String>,
        update: bool,
        now: Duration,
    ) -> Vec<Effect> {
        let mut effects: Vec<_> = notes.into_iter().map(notice).collect();
        match update {
            true => {
                effects.push(notice("Backup updated"));
                let done = super::control::CloudBackupDone::Done;
                effects.extend(self.control_cloud_end(&[ControlCloud::Update], done));
            }
            false => {
                effects.push(notice("Backup created"));
                let done = super::control::CloudBackupDone::Done;
                effects.extend(self.control_cloud_end(&[ControlCloud::Create], done));
            }
        }
        let refresh = CloudCall::Backups { delete: None };
        effects.extend(self.cloud_job(settings, vec![refresh], None, now));
        effects
    }

    /// The cloud backup file header (`0x47a1c8`), as the Control file has it
    /// (`control/backup.rs`): `QC_` and the UTC date, the account user name. I: `author_id`
    /// is the access token's `sub` (stock reads `AccountConfig.ini`, `0x2ff3f8`).
    pub(super) fn backup_header(&self, settings: &DeviceSettings, unix: i64) -> LocalHeader {
        let [year, month, day, hour, minute, second] = utc_parts(unix);
        let created = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z");
        let creator = match settings.hardware() {
            tetrad_settings::Hardware::QuadCortex => "quad",
            tetrad_settings::Hardware::Atma => "atma",
        };
        LocalHeader {
            creator: creator.into(),
            creator_version: tetrad_firmware::running().version.into(),
            name: format!("QC_{created}"),
            author: settings.settings().account.username.clone(),
            author_id: (self.cloud.as_ref())
                .and_then(CloudApi::claims)
                .map(|claims| claims.user_id)
                .unwrap_or_default(),
            created,
        }
    }

    pub(super) fn backup_failed(&mut self, reason: &str) -> Vec<Effect> {
        self.backup = None;
        self.restore_source = None;
        let flows = [
            ControlCloud::Create,
            ControlCloud::Restore,
            ControlCloud::Update,
        ];
        let failed = super::control::CloudBackupDone::Failed(reason.to_owned());
        let control = self.control_cloud_end(&flows, failed);
        let mut effects = vec![notice(format!("backup failed: {reason}"))];
        effects.extend(control);
        effects
    }

    /// The end of the flow a Cortex Control cloud backup started, when the running request is
    /// one of `requests`: `done` for Control ([`Effect::CloudBackupDone`]).
    pub(super) fn control_cloud_end(
        &mut self,
        requests: &[ControlCloud],
        done: super::control::CloudBackupDone,
    ) -> Option<Effect> {
        self.control_cloud
            .take_if(|request| requests.contains(request))?;
        Some(Effect::CloudBackupDone(done))
    }

    /// A Cortex Control cloud backup request (CloudBackup 41, r23-control MR 1): it runs as the
    /// Backups page runs it (`control::cloud::run`), a create under Control's name (stock
    /// `createBackup(name, …)`, `0x150260`), and its origin is kept: the end of the flow (the
    /// list reply, the create upload, the list after a DELETE, the restore extract before the
    /// restart timer, or a failure) answers Control with [`Effect::CloudBackupDone`]. A request
    /// refused at once answers through `run`.
    pub fn control_cloud_backup(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        request: super::control::CloudBackupRequest,
        now: Duration,
    ) -> Vec<Effect> {
        use super::control::CloudBackupRequest as Request;
        let kind = match &request {
            Request::List => ControlCloud::List,
            Request::Create { name } => {
                self.control_backup_name = Some(name.clone());
                ControlCloud::Create
            }
            Request::Delete { .. } => ControlCloud::Delete,
            Request::Restore { .. } => ControlCloud::Restore,
            Request::Update { .. } => ControlCloud::Update,
            Request::Rename { .. } => ControlCloud::Rename,
        };
        let effects = super::control::cloud::run(self, backends, settings, request, now);
        self.control_backup_name = None;
        if !(effects.iter()).any(|effect| matches!(effect, Effect::CloudBackupDone(_))) {
            self.control_cloud = Some(kind);
        }
        effects
    }

    /// A successful extract. Stock reloads settings, library, preset and every view in place
    /// (`0x3fcf64`), and so does Tetrad
    /// ([`Effect::RestoreReload`], `Device::reload_after_restore`). Here, before it:
    ///
    /// 1. The capture markers (`0x7038a8`, `0x6f6218`, `0x6f5fa0`), so the reload's boot media
    ///    steps run the legacy capture translation, the conversion and the corruption check
    ///    (`media.rs`), as stock boot `0x704198` does on these markers.
    /// 2. The backup's hardware (`restore_source`): the reload's first flush converts the
    ///    connector levels (`0x480bcc`, [`convert_backup_gains`]) and keeps this device's
    ///    volume and name ([`keep_device_values_after_restore`]).
    ///
    /// A failed extract reopens the library as stock does.
    pub(super) fn restored(
        &mut self,
        files: &mut impl Files,
        hardware: tetrad_settings::Hardware,
        members: usize,
        control: Option<ControlBackupDone>,
    ) -> Vec<Effect> {
        use super::media::{
            CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED, CAPTURE_TRANSLATION_FLAG,
        };
        self.restored = true;
        let mut effects = vec![notice(format!(
            "Extracted backup ({members} members); reloading"
        ))];
        let done = super::control::CloudBackupDone::Done;
        effects.extend(self.control_cloud_end(&[ControlCloud::Restore], done));
        let markers = files
            .write(Path::new(CAPTURE_TRANSLATION_FLAG), b"")
            .and_then(|()| files.remove(Path::new(CAPTURE_CORRUPTION_CHECKED)))
            .and_then(|()| files.remove(Path::new(CAPTURE_CONVERSION_FINISHED)))
            .and_then(|()| files.sync());
        if let Err(error) = markers {
            effects.push(notice(format!(
                "capture markers after the restore: {error}; the next boot skips the capture \
                 translation and checks"
            )));
        }
        effects.extend(control.map(Effect::ControlBackupDone));
        self.restore_source = self.restore_source.filter(|source| *source != hardware);
        effects.push(Effect::RestoreReload);
        effects
    }

    /// A restore job runs: it owns the user data files.
    pub fn restoring(&self) -> bool {
        matches!(
            self.backup,
            Some(BackupFlow::Restoring | BackupFlow::Control(ControlBackup::Restore { .. }))
        )
    }

    pub fn backup_view(&self) -> BackupView {
        BackupView {
            items: self
                .backups
                .iter()
                .map(|backup| BackupItem {
                    id: backup.id,
                    title: backup.title.clone(),
                    detail: backup.detail.clone(),
                    downloaded: backup.downloaded.clone(),
                })
                .collect(),
            busy: self
                .backup
                .as_ref()
                .map(|flow| (flow.label().to_owned(), None)),
        }
    }
}

/// After a good restore extract. The stock reload `0x3fcf64` re-reads the settings
/// (`QSettings::sync` `0x653d18`) but keeps this device's `Atma/AtmaVolume` (`0x64e73c` →
/// `0x64e754`) and `General/customName` (`0x64fc30` → `0x64fd2c`). Tetrad writes those two keys
/// from `memory` into the restored files before its reload reads them, and nothing else:
/// every other value stays as restored. Equal values write nothing.
pub fn keep_device_values_after_restore(
    storage: &mut impl tetrad_settings::SettingsStorage,
    memory: &tetrad_settings::Settings,
    hardware: tetrad_settings::Hardware,
) -> Result<(), tetrad_settings::StorageError> {
    let (mut restored, _) = DeviceSettings::load(storage, hardware)?;
    restored.update(|settings| {
        settings.atma_volume = memory.atma_volume;
        settings
            .general
            .custom_name
            .clone_from(&memory.general.custom_name);
    });
    restored.flush(storage)
}

/// The hardware of a backup `creator` (`0x480bcc`: equal to `0x49625c(2)`, "atma", or not).
pub(super) fn source_hardware(creator: &str) -> tetrad_settings::Hardware {
    match creator {
        "atma" => tetrad_settings::Hardware::Atma,
        _ => tetrad_settings::Hardware::QuadCortex,
    }
}

/// The level keys `0x480bcc` converts (table built once, `DAT_0154b2b8`): IN 1, IN 2, RET 1,
/// RET 2 (`0x15399b8`: 100, 200, 500, 600), then both capture dialogs.
pub(super) const BACKUP_GAIN_KEYS: [&str; 8] = [
    "ConnectorsSettings/100",
    "ConnectorsSettings/200",
    "ConnectorsSettings/500",
    "ConnectorsSettings/600",
    "NeuralCaptureDialogSettings/ReturnLevel",
    "NeuralCaptureDialogSettings/InLevel",
    "NeuralCapture2ControllerSettings/ReturnLevel",
    "NeuralCapture2ControllerSettings/InLevel",
];

/// `0x480bcc` on the restored `ZenjackConfig.ini` (stock syncs the file first, `0x64be10`):
/// a backup from other hardware than this unit (`0x499a60`) gets its input and return levels
/// moved to this unit's range. Each value (absent: 0 dB of the source, 1/6 or 2/7) goes to dB
/// on the source range, is clamped to this range and normalized again (`0x47f9b0` through
/// `0x64b8f8`, skew 1; −12..60 dB on a Quad Cortex, −12..30 dB on an Atma). Equal hardware
/// writes nothing ("No conversion needed for backup from").
pub fn convert_backup_gains(
    storage: &mut impl tetrad_settings::SettingsStorage,
    source: tetrad_settings::Hardware,
    device: tetrad_settings::Hardware,
) -> Result<(), tetrad_settings::StorageError> {
    use tetrad_settings::{Hardware, SettingsFile, SettingsFileId, StorageError, Variant};
    if source == device {
        return Ok(());
    }
    let max_db = |hardware| match hardware {
        Hardware::QuadCortex => 60.0_f32,
        Hardware::Atma => 30.0,
    };
    let (from, to) = (max_db(source), max_db(device));
    let error = |op| {
        move |source| StorageError {
            file: SettingsFileId::Zenjack,
            op,
            source,
        }
    };
    let bytes = storage
        .read(SettingsFileId::Zenjack)
        .map_err(error("read"))?;
    let (mut file, _) = SettingsFile::parse(&bytes.unwrap_or_default());
    for key in BACKUP_GAIN_KEYS {
        let value = (file.value(key))
            .and_then(Result::ok)
            .and_then(|value| value.to_f32())
            .unwrap_or(12.0 / (from + 12.0));
        let db = (-12.0 + value.clamp(0.0, 1.0) * (from + 12.0)).clamp(-12.0, to);
        file.set(
            key,
            &Variant::Float(((db + 12.0) / (to + 12.0)).clamp(0.0, 1.0)),
        );
    }
    (storage.write(SettingsFileId::Zenjack, &file.save())).map_err(error("write"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `0x47f648`: noise, `1`, then a noise byte before each serial byte, one noise byte of
    /// padding, all under the key's cipher. Made-up key, serial and noise.
    #[test]
    fn the_stamp_puts_noise_before_each_serial_byte_under_the_key_cipher() {
        let key = [0x5a; STAMP_KEY_BYTES];
        let mut next = 0xa0u8;
        let stamp = seal_stamp(&key, "Q0000TEST", || {
            next += 1;
            next
        });
        let mut plain = vec![0xa1, 1];
        for (noise, byte) in (0xa2..).zip(b"Q0000TEST") {
            plain.extend([noise, *byte]);
        }
        plain.push(0xab);
        NdspCipher::derive(&key).apply_keystream(&mut plain);
        assert_eq!(stamp, CreatorStamp::from_slice(&plain));
        assert_eq!(seal_stamp(&key, "Q0000TEST0", || 0), None);
    }
}

//! Remote (owned, Tetrad; docs/remote.md): the Backup page: a backup of the user data to
//! download, and a backup to restore after a touch on the unit.
//!
//! The pack and the restore are jobs of the services bridge on the bulk worker
//! ([`RemoteBackup`]); their ends come back as [`RemoteBackupDone`], never to Cortex Control.
//! The device thread only moves pieces: a download request reads at most [`MAX_PIECE`] bytes,
//! an upload request appends its body to a `.part` file. One backup at a time: a new upload
//! replaces the file that waits, and a new pack replaces a packed one.

use std::path::Path;

use serde_json::{Value, json};
use tetrad_remote::{BackupOp, Reply};
use tetrad_services::exec::{FileError, Files};
use tetrad_ui::RemoteRestore;

use super::platform::{Platform, ServiceBackends};
use super::services::{REMOTE_ARCHIVE, REMOTE_RESTORE, RemoteBackup, RemoteBackupDone};
use super::{Device, DeviceError, Effect, Subsystem};

/// The most bytes that one download request gets, so the reply stays well inside the server's
/// 10 s per connection on a slow network.
pub const MAX_PIECE: u64 = 512 * 1024;
/// The largest backup that the page may send (O). A stock backup is some tens of MB.
pub const MAX_UPLOAD: u64 = 512 << 20;
/// The backup that the page sends, until its last piece.
const PART: &str = "/media/p4/tetrad_remote_restore.tar.gz.part";
const MAX_NAME_CHARS: usize = 64;
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

#[derive(Debug, Default, Clone, PartialEq, Eq)]
enum Slot {
    #[default]
    Idle,
    /// The pack job runs.
    Creating,
    /// The packed backup waits for its download.
    Ready {
        name: String,
        size: u64,
    },
    Receiving {
        name: String,
        received: u64,
    },
    /// The sent backup waits for RESTORE or DISCARD on the unit.
    Received {
        name: String,
        size: u64,
    },
    Restoring {
        name: String,
        size: u64,
    },
    /// The restore ended well, and the device reloaded.
    Restored,
}

/// The Backup page: the backup that waits, and the last problem.
#[derive(Debug, Default)]
pub struct BackupPage {
    slot: Slot,
    error: Option<String>,
    /// The id of the last received backup ([`RemoteRestore::id`]).
    received: u64,
}

impl BackupPage {
    /// The received backup for the unit's Remote page.
    pub(super) fn restore(&self) -> Option<RemoteRestore> {
        match &self.slot {
            Slot::Received { name, size } | Slot::Restoring { name, size } => Some(RemoteRestore {
                id: self.received,
                name: name.clone(),
                size: size_text(*size),
                restoring: matches!(self.slot, Slot::Restoring { .. }),
            }),
            _ => None,
        }
    }

    /// A job runs: its files stay until it ends.
    fn busy(&self) -> Result<(), String> {
        match self.slot {
            Slot::Creating => Err("The unit is packing a backup".into()),
            Slot::Restoring { .. } => Err("The unit is restoring a backup".into()),
            _ => Ok(()),
        }
    }

    /// The answer of every Backup call: `state`, and `name`, `size`, `received` and `error`
    /// when they apply (`null` when not).
    fn status(&self) -> Value {
        let (state, name, size, received) = match &self.slot {
            Slot::Idle => ("idle", None, None, None),
            Slot::Creating => ("creating", None, None, None),
            Slot::Ready { name, size } => ("ready", Some(name), Some(*size), None),
            Slot::Receiving { name, received } => ("receiving", Some(name), None, Some(*received)),
            Slot::Received { name, size } => ("received", Some(name), Some(*size), None),
            Slot::Restoring { name, size } => ("restoring", Some(name), Some(*size), None),
            Slot::Restored => ("restored", None, None, None),
        };
        json!({
            "state": state,
            "name": name,
            "size": size,
            "received": received,
            "error": self.error,
        })
    }
}

/// As the page shows sizes: MB of 1,048,576 bytes with one decimal, else KB.
fn size_text(size: u64) -> String {
    match size >= 1 << 20 {
        true => format!("{:.1} MB", size as f64 / f64::from(1 << 20)),
        false => format!("{} KB", size.div_ceil(1024).max(1)),
    }
}

impl<P: Platform> Device<P> {
    pub(super) fn remote_backup(&mut self, op: BackupOp) -> Reply {
        let done = match op {
            BackupOp::Status => Ok(()),
            BackupOp::Create => self.remote_backup_create(),
            BackupOp::Read { offset } => return self.remote_backup_read(offset),
            BackupOp::Upload {
                name,
                offset,
                last,
                bytes,
            } => self.remote_backup_upload(&name, offset, last, &bytes),
            BackupOp::Discard => self.remote_backup_discard(),
        };
        match done {
            Ok(()) => Reply::Json(self.bridges.remote.backup.status()),
            Err(reason) => Reply::Refused(reason),
        }
    }

    /// Starts the pack. The services bridge refuses it while another backup (cloud, Cortex
    /// Control or this page) runs; a received backup must be restored or discarded first.
    fn remote_backup_create(&mut self) -> Result<(), String> {
        self.bridges.remote.backup.busy()?;
        if let Slot::Received { name, .. } = &self.bridges.remote.backup.slot {
            return Err(format!(
                "{name} waits for RESTORE on the unit; discard it first"
            ));
        }
        let services = &mut self.bridges.services;
        let effects = (services.remote_backup(self.platform.services(), RemoteBackup::Create))
            .map_err(|error| error.to_string())?;
        self.remove_backup_files();
        let page = &mut self.bridges.remote.backup;
        page.slot = Slot::Creating;
        page.error = None;
        self.notice(Subsystem::Remote, "Remote: packing a backup".into());
        self.apply(Ok::<_, DeviceError>(effects));
        Ok(())
    }

    /// At most [`MAX_PIECE`] bytes of the packed backup from `offset` on.
    fn remote_backup_read(&mut self, offset: u64) -> Reply {
        let Slot::Ready { size, .. } = self.bridges.remote.backup.slot else {
            return Reply::Refused("No backup is ready to download".into());
        };
        if offset > size {
            return Reply::Refused(format!("The backup has {size} bytes, not {offset}"));
        }
        let files = self.platform.services().files();
        match files.read_at(Path::new(REMOTE_ARCHIVE), offset, MAX_PIECE) {
            Ok(Some(bytes)) => Reply::Bytes(bytes),
            Ok(None) => {
                let page = &mut self.bridges.remote.backup;
                page.slot = Slot::Idle;
                page.error = Some("The packed backup is gone; pack it again".into());
                Reply::Refused("The packed backup is gone; pack it again".into())
            }
            Err(error) => Reply::Refused(error.to_string()),
        }
    }

    /// One piece of a backup to restore. The piece at offset 0 starts the `.part` file (and
    /// drops the backup that waited); a later piece must start where the part ends. The last
    /// piece must make a gzip file, which then waits for RESTORE on the unit.
    fn remote_backup_upload(
        &mut self,
        name: &str,
        offset: u64,
        last: bool,
        bytes: &[u8],
    ) -> Result<(), String> {
        self.bridges.remote.backup.busy()?;
        let name: String = (name.chars())
            .filter(|c| !c.is_control())
            .take(MAX_NAME_CHARS)
            .collect();
        let part = Path::new(PART);
        let fail = |error: FileError| error.to_string();
        if offset == 0 {
            if !self.options.persist {
                return Err("This run does not write the user data".into());
            }
            self.remove_backup_files();
            self.bridges.remote.backup.error = None;
            self.platform
                .services()
                .files()
                .write(part, bytes)
                .map_err(fail)?;
        } else {
            let receiving = matches!(
                &self.bridges.remote.backup.slot,
                Slot::Receiving { name: current, .. } if *current == name
            );
            let have = self.platform.services().files().size(part).map_err(fail)?;
            if !receiving || have != Some(offset) {
                return Err(format!(
                    "Out of order: the unit has {} bytes of {name}, not {offset}",
                    have.filter(|_| receiving).unwrap_or(0)
                ));
            }
            self.platform
                .services()
                .files()
                .append(part, bytes)
                .map_err(fail)?;
        }
        let received = offset + bytes.len() as u64;
        let refuse = |device: &mut Self, reason: String| {
            let _ = device.platform.services().files().remove(part);
            device.bridges.remote.backup.slot = Slot::Idle;
            Err(reason)
        };
        if received > MAX_UPLOAD {
            let reason = format!("A backup is at most {} MiB", MAX_UPLOAD >> 20);
            return refuse(self, reason);
        }
        if !last {
            self.bridges.remote.backup.slot = Slot::Receiving { name, received };
            return Ok(());
        }
        let files = self.platform.services().files();
        let head = files
            .read_at(part, 0, GZIP_MAGIC.len() as u64)
            .map_err(fail)?;
        if head.as_deref() != Some(&GZIP_MAGIC[..]) {
            let reason = format!("{name} is not a Quad Cortex backup (a .tar.gz file)");
            return refuse(self, reason);
        }
        (files.rename(part, Path::new(REMOTE_RESTORE)))
            .and_then(|()| files.sync())
            .map_err(fail)?;
        let notice = format!("Remote: backup {name} received; RESTORE waits on the unit");
        self.notice(Subsystem::Remote, notice);
        let remote = &mut self.bridges.remote;
        remote.show_toast("Backup received: touch RESTORE in Settings → Tetrad → Remote".into());
        remote.backup.slot = Slot::Received {
            name,
            size: received,
        };
        remote.backup.received += 1;
        Ok(())
    }

    /// Drops the packed or the received backup.
    fn remote_backup_discard(&mut self) -> Result<(), String> {
        self.bridges.remote.backup.busy()?;
        self.remove_backup_files();
        let page = &mut self.bridges.remote.backup;
        page.slot = Slot::Idle;
        page.error = None;
        Ok(())
    }

    /// RESTORE or DISCARD on the unit's Remote page. RESTORE runs the restore job, which
    /// closes the library, replaces the backup members and reloads the device in place, as a
    /// cloud restore does.
    pub(super) fn remote_backup_touch(&mut self, restore: Option<u64>) -> Vec<Effect> {
        let Slot::Received { name, size } = self.bridges.remote.backup.slot.clone() else {
            return Vec::new();
        };
        // The page replaced the backup while the question was open: the touch was for another one.
        if restore.is_some_and(|id| id != self.bridges.remote.backup.received) {
            let notice = format!("Remote: backup {name} came while RESTORE waited; not restored");
            self.notice(Subsystem::Remote, notice);
            let toast = "The backup changed: touch RESTORE again".to_owned();
            self.bridges.remote.show_toast(toast);
            return Vec::new();
        }
        if restore.is_none() {
            let _ = self.remote_backup_discard();
            self.notice(
                Subsystem::Remote,
                format!("Remote: backup {name} discarded"),
            );
            return Vec::new();
        }
        let services = &mut self.bridges.services;
        let started = match self.options.persist {
            true => services
                .remote_backup(self.platform.services(), RemoteBackup::Restore)
                .map_err(|error| error.to_string()),
            false => Err("this run does not write the user data".to_owned()),
        };
        let page = &mut self.bridges.remote.backup;
        match started {
            Ok(effects) => {
                page.slot = Slot::Restoring {
                    name: name.clone(),
                    size,
                };
                page.error = None;
                self.notice(
                    Subsystem::Remote,
                    format!("Remote: restoring backup {name}"),
                );
                effects
            }
            Err(reason) => {
                page.error = Some(reason.clone());
                let notice = format!("Remote: backup {name} not restored: {reason}");
                self.notice(Subsystem::Remote, notice);
                Vec::new()
            }
        }
    }

    /// [`Effect::RemoteBackupDone`]: the end of the pack or the restore. A failed restore
    /// offers RESTORE again. After Remote stopped, the files go.
    pub(super) fn remote_backup_done(&mut self, done: RemoteBackupDone) {
        let slot = std::mem::take(&mut self.bridges.remote.backup.slot);
        let (slot, error, notice) = match (slot, done) {
            (Slot::Creating, RemoteBackupDone::Created) => {
                let files = self.platform.services().files();
                match files.size(Path::new(REMOTE_ARCHIVE)) {
                    Ok(Some(size)) => {
                        let name = self.backup_file_name();
                        let notice = format!("Remote: backup {name} packed ({size} bytes)");
                        (Slot::Ready { name, size }, None, notice)
                    }
                    Ok(None) => {
                        let reason = "the packed backup is missing".to_owned();
                        let notice = format!("Remote: {reason}");
                        (Slot::Idle, Some(reason), notice)
                    }
                    Err(error) => {
                        let notice = format!("Remote: {error}");
                        (Slot::Idle, Some(error.to_string()), notice)
                    }
                }
            }
            (Slot::Creating, RemoteBackupDone::Failed(reason)) => {
                let _ = self
                    .platform
                    .services()
                    .files()
                    .remove(Path::new(REMOTE_ARCHIVE));
                let notice = format!("Remote: backup not packed: {reason}");
                (Slot::Idle, Some(reason), notice)
            }
            (Slot::Restoring { name, .. }, RemoteBackupDone::Restored { members }) => {
                let _ = self
                    .platform
                    .services()
                    .files()
                    .remove(Path::new(REMOTE_RESTORE));
                let notice = format!("Remote: backup {name} restored ({members} members)");
                (Slot::Restored, None, notice)
            }
            (Slot::Restoring { name, size }, RemoteBackupDone::Failed(reason)) => {
                let notice = format!("Remote: backup {name} not restored: {reason}");
                (Slot::Received { name, size }, Some(reason), notice)
            }
            (slot, done) => {
                let notice = format!("Remote: backup result without its step: {done:?}");
                let error = self.bridges.remote.backup.error.take();
                (slot, error, notice)
            }
        };
        let page = &mut self.bridges.remote.backup;
        page.slot = slot;
        page.error = error;
        self.notice(Subsystem::Remote, notice);
        if !self.bridges.remote.running() {
            self.drop_remote_backup();
        }
    }

    /// Remote starts or stops: the backup files of this page go, unless a job still uses them
    /// (its end removes them when Remote is off).
    pub(super) fn drop_remote_backup(&mut self) {
        if self.bridges.remote.backup.busy().is_ok() {
            self.remove_backup_files();
            self.bridges.remote.backup = BackupPage::default();
        }
    }

    fn remove_backup_files(&mut self) {
        let files = self.platform.services().files();
        let failed: Vec<_> = [REMOTE_ARCHIVE, REMOTE_RESTORE, PART]
            .into_iter()
            .filter_map(|path| files.remove(Path::new(path)).err())
            .collect();
        for error in failed {
            self.notice(Subsystem::Remote, format!("Remote: {error}"));
        }
    }

    /// `QC-Backup-YYYYMMDD-HHMM.tar.gz` in UTC, as cloud backups are named, or
    /// `QC-Backup.tar.gz` without a wall clock.
    fn backup_file_name(&mut self) -> String {
        let unix = (self.platform.wall_clock())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_secs() as i64);
        match unix {
            Some(unix) => {
                let [year, month, day, hour, minute, _] = super::services::utc_parts(unix);
                format!("QC-Backup-{year:04}{month:02}{day:02}-{hour:02}{minute:02}.tar.gz")
            }
            None => "QC-Backup.tar.gz".into(),
        }
    }
}

//! CloudBackup (41) on the Control side (receiver `0x14e1e0`).
//!
//! Gates, in stock order: not logged in (`0x7166d0`) → error 17; a busy device (`0x47fb9c`) →
//! error 0. Then by action: CREATE with a `name` (`0x150260` → `createBackup`), UPDATE with
//! a `key` (update `0x14e2d0`; with a `name` too, rename `0x150c08`), DELETE with a `key`
//! (`0x14f968`), READ (list `0x14f208`, no busy gate), COPY with a `key` (download and restore
//! `0x400890`, done `0x151ecc`). Every result goes out as UPDATE {`error_code`}
//! (`0x14d3c4`, only while connected); a list adds `backups` (`0x14b8bc`). CREATE, UPDATE and
//! DELETE run under a Notice suspension (`0x3fbb90(0|2|3)`); their end resumes
//! (`0x3fbb90(4)`).
//!
//! UPDATE and rename (census-r38 L1), C: both list first (a failed list → 15); a `key` not
//! listed → 4 (update) or 7 (rename). The update runs `createBackup` with the backup's url
//! (I: its result code 5 or 3 comes from the backup manager slot, as for a create). The
//! rename POST fails → 6 (`0x150ab8`); else a new list: 8 with `backups`, or 15 when it fails
//! (`0x152218`). Tetrad runs the rename in this order (review r39 K11,
//! `Services::control_rename_backup`); an update finds the `key` in the last services list
//! (O, as DELETE and COPY).

use std::time::Duration;

use tetrad_control::codecs::{
    CloudBackupErrorCode as Code, CloudBackupItem, CloudBackupMessage, EnumValue, MessageAction,
};
use tetrad_control::device::DeviceResult;
use tetrad_control::link::{Notification, Suspension};
use tetrad_settings::device::DeviceSettings;

use super::super::platform::ServiceBackends;
use super::super::services::{CloudBackup, ServiceError, Services};
use super::super::{Effect, State, Subsystem};
use super::backup::{LinkOp, LocalBackup};
use super::{AccountRequest, Control, ControlError, StateModel};

/// A Control cloud backup for the services bridge ([`AccountRequest::CloudBackup`]). The
/// bridge runs the same services calls as the device Backups page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudBackupRequest {
    /// READ: `GET /backups/v1`.
    List,
    /// CREATE: pack, `generateBackupUrl`, upload. Stock passes `name` to `createBackup`.
    Create { name: String },
    /// DELETE of the listed backup with this `key`.
    Delete { key: String },
    /// COPY: download and restore the listed backup with this `key`.
    Restore { key: String },
    /// UPDATE with a `key` (`0x14e2d0`): the listed backup is packed again and uploaded to its
    /// `<backupUrl>/update` (`createBackup` with the backup's url; `Services::update_backup`).
    Update { key: String },
    /// UPDATE with a `key` and a `name` (`0x150c08`): a list, `POST <path>/rename`, then a
    /// list again (`Services::control_rename_backup`).
    Rename { key: String, name: String },
}

/// The end of a [`CloudBackupRequest`] ([`Control::cloud_backup_done`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudBackupDone {
    Done,
    /// No listed backup has the request's `key`.
    NotFound,
    Failed(String),
    /// The list of a rename failed, before or after its POST (C: 15, `0x150c08`,
    /// `0x152218`).
    ListFailed(String),
    /// I: a READ while another request runs, answered from the last services list. Stock
    /// lists in its own flow (`0x14f208`, no busy gate); Tetrad keeps one running request per
    /// side, so this answer takes no slot and the running request keeps its end.
    Listed,
}

/// The Control cloud backup that runs; stock allows one (`0x47fb9c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CloudOp {
    List,
    Create,
    Delete,
    Restore,
    Update,
    Rename,
}

impl CloudOp {
    /// The `error_code` of a result.
    fn code(self, done: &CloudBackupDone) -> Code {
        use CloudBackupDone::{Done, ListFailed, Listed, NotFound};
        match (self, done) {
            (_, ListFailed(_)) => Code::BackupListingFailed,
            (Self::List, Done | Listed) => Code::BackupListingSucceeded,
            (Self::List, _) => Code::BackupListingFailed,
            (Self::Create, Done) => Code::BackupCreationSucceeded,
            (Self::Create, _) => Code::BackupCreationFailed,
            (Self::Delete, Done) => Code::BackupDeleteSucceeded,
            (Self::Delete, NotFound) => Code::BackupDeleteNotFound,
            (Self::Delete, _) => Code::BackupDeleteFailed,
            (Self::Restore, Done) => Code::BackupDownloadSucceeded,
            (Self::Restore, NotFound) => Code::BackupDownloadNotFound,
            (Self::Restore, _) => Code::BackupDownloadFailed,
            (Self::Update, Done) => Code::BackupUpdateSucceeded,
            (Self::Update, NotFound) => Code::BackupUpdateNotFound,
            (Self::Update, _) => Code::BackupUpdateFailed,
            (Self::Rename, Done) => Code::BackupRenameSucceeded,
            (Self::Rename, NotFound) => Code::BackupRenameNotFound,
            (Self::Rename, _) => Code::BackupRenameFailed,
        }
    }

    /// O: a request with no result by then fails ([`Control::tick`]), so a lost services
    /// result cannot keep the link suspended. Stock waits for the backup manager. A list or a
    /// delete is one cloud call; a create or a restore moves the whole backup, as long as the
    /// update package forward (1 800 000 ms, `0x44a928`).
    pub(super) fn deadline(self) -> Duration {
        match self {
            Self::List | Self::Delete | Self::Rename => Duration::from_secs(120),
            Self::Create | Self::Restore | Self::Update => Duration::from_secs(1_800),
        }
    }

    /// CREATE, UPDATE, rename and DELETE suspend the link (`0x3fbb90(0|2|3)`); the list and
    /// the download do not.
    fn suspends(self) -> bool {
        !matches!(self, Self::List | Self::Restore)
    }
}

fn reply(code: Code) -> CloudBackupMessage {
    CloudBackupMessage {
        action: Some(MessageAction::Update.into()),
        error_code: Some(code.into()),
        ..Default::default()
    }
}

/// `0x14b8bc` I: the services list item as Control reads it. Stock dates are
/// `yyyy-MM-ddThh:mm:ssZ`; Tetrad sends the reply text as it is.
fn item(backup: &CloudBackup) -> CloudBackupItem {
    CloudBackupItem {
        name: Some(backup.title.clone()),
        key: Some(backup.path.clone()),
        timestamp: Some(backup.detail.clone()),
        download_timestamp: backup.downloaded.clone(),
        ..Default::default()
    }
}

/// The receiver `0x14e1e0`.
pub(super) fn request(
    model: &mut StateModel<'_>,
    request: &CloudBackupMessage,
) -> DeviceResult<Vec<CloudBackupMessage>> {
    if model.state.settings.settings().account.is_logged_out() {
        return Ok(vec![reply(Code::NotConnectedOrLoggedIn)]);
    }
    let first = request.backups.first();
    let text = |field: Option<&String>| field.filter(|text| !text.is_empty()).cloned();
    let name = text(first.and_then(|item| item.name.as_ref()));
    let key = text(first.and_then(|item| item.key.as_ref()));
    let action = request.action.unwrap_or(MessageAction::Create.into());
    let read = action == MessageAction::Read.into();
    if !read && (model.cloud.is_some() || model.local_backup != LocalBackup::Idle) {
        return Ok(vec![reply(Code::BackupOperationInProgress)]);
    }
    if read && model.cloud.is_some() {
        let listed = Effect::CloudBackupDone(CloudBackupDone::Listed);
        model.effects.push(listed);
        return Ok(Vec::new());
    }
    let (op, services) = match (action, name, key) {
        (EnumValue::Known(MessageAction::Read), ..) => (CloudOp::List, CloudBackupRequest::List),
        (EnumValue::Known(MessageAction::Create), Some(name), _) => {
            (CloudOp::Create, CloudBackupRequest::Create { name })
        }
        (EnumValue::Known(MessageAction::Delete), _, Some(key)) => {
            (CloudOp::Delete, CloudBackupRequest::Delete { key })
        }
        (EnumValue::Known(MessageAction::Copy), _, Some(key)) => {
            (CloudOp::Restore, CloudBackupRequest::Restore { key })
        }
        (EnumValue::Known(MessageAction::Update), None, Some(key)) => {
            (CloudOp::Update, CloudBackupRequest::Update { key })
        }
        (EnumValue::Known(MessageAction::Update), Some(name), Some(key)) => {
            (CloudOp::Rename, CloudBackupRequest::Rename { key, name })
        }
        // Stock does nothing without the item field it needs.
        _ => return Ok(Vec::new()),
    };
    if op.suspends() {
        model.link_op = Some(LinkOp::Suspend(Suspension::Notice));
    }
    model.cloud = Some(op);
    model
        .effects
        .push(Effect::ControlAccount(AccountRequest::CloudBackup(
            services,
        )));
    Ok(Vec::new())
}

/// Runs a [`CloudBackupRequest`] on the services bridge. A listed backup is found by its
/// server path (the Control `key`, I) in the last list. A refused call ends at once.
pub(in crate::device) fn run(
    services: &mut Services,
    backends: &mut impl ServiceBackends,
    settings: &mut DeviceSettings,
    request: CloudBackupRequest,
    now: Duration,
) -> Vec<Effect> {
    let listed = |services: &Services, key: &str| {
        let backup = services.backups().iter().find(|backup| backup.path == key);
        backup.map(|backup| backup.id)
    };
    let started = match request {
        CloudBackupRequest::List => services.refresh_backups(backends, settings, now),
        // Services names the backup with Control's `name` (`control_cloud_backup`).
        CloudBackupRequest::Create { .. } => services.create_backup(backends, settings, now),
        CloudBackupRequest::Delete { key } => match listed(services, &key) {
            Some(id) => services.delete_backup(backends, settings, id, now),
            None => return vec![Effect::CloudBackupDone(CloudBackupDone::NotFound)],
        },
        CloudBackupRequest::Restore { key } => match listed(services, &key) {
            Some(id) => services.restore_backup(backends, settings, id, now),
            None => return vec![Effect::CloudBackupDone(CloudBackupDone::NotFound)],
        },
        CloudBackupRequest::Update { key } => match listed(services, &key) {
            Some(id) => services.update_backup(backends, settings, id, now),
            None => return vec![Effect::CloudBackupDone(CloudBackupDone::NotFound)],
        },
        CloudBackupRequest::Rename { key, name } => {
            services.control_rename_backup(backends, settings, key, name, now)
        }
    };
    started.unwrap_or_else(|error: ServiceError| {
        vec![Effect::CloudBackupDone(CloudBackupDone::Failed(
            error.to_string(),
        ))]
    })
}

impl Control {
    /// The end of a Control cloud backup: UPDATE {`error_code`} (with `backups` for a list or
    /// a rename, from `backups`: the rename end lists again, `0x152218`), then the resume of a
    /// suspending request. [`CloudBackupDone::Listed`]
    /// leaves the running request as it is.
    pub fn cloud_backup_done(
        &mut self,
        done: CloudBackupDone,
        backups: &[CloudBackup],
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        let op = if done == CloudBackupDone::Listed {
            CloudOp::List
        } else if let Some((op, _)) = self.cloud.take() {
            op
        } else {
            let notice = format!("cloud backup result without its request: {done:?}");
            return Ok(vec![Effect::Notice(Subsystem::Control, notice)]);
        };
        let mut message = reply(op.code(&done));
        let listed = matches!(done, CloudBackupDone::Done | CloudBackupDone::Listed);
        if matches!(op, CloudOp::List | CloudOp::Rename) && listed {
            message.backups = backups.iter().map(item).collect();
        }
        let mut effects = self.notify(Notification::Update(message.into()), state, now)?;
        if let CloudBackupDone::Failed(reason) | CloudBackupDone::ListFailed(reason) = done {
            let notice = format!("Control cloud backup failed: {reason}");
            effects.push(Effect::Notice(Subsystem::Control, notice));
        }
        if op.suspends() {
            effects.extend(self.link_op(LinkOp::Resume, state, now)?);
        }
        Ok(effects)
    }
}

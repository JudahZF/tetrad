//! LocalBackup (40) on the Control side (`0x17d104`).
//!
//! - CREATE: a busy check (`0x47fc90`), then the backup job (`0x3fb818` → `0x4774d8`,
//!   `0x47a1c8`) under a Notice suspension (`0x3fbb90(0)`). The result (`0x17e3e8`) goes out
//!   in chunks (`0x17ddfc`); the last one resumes (`0x3fbb90(4)`). A failed create only resumes.
//! - UPDATE with `can_apply_backup`: a busy check (`0x47fbfc`), a Download suspension
//!   (`0x3fbb90(1)`), the receive file `/media/p4/downloaded_backup.json` (`0x30bc70`), reply
//!   UPDATE {`can_apply_backup`}.
//! - UPDATE chunks: written to that file (`0x2ff33c`); the last restores (`0x4080cc`), then
//!   UPDATE {`applied_backup`} (`0x17dab8`). Without a receive: UPDATE {`can_apply_backup`:
//!   false} (`0x17cdc8`).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tetrad_control::codecs::{CortexMessageType, LocalBackupMessage, MessageAction};
use tetrad_control::device::{DeviceError as ModelError, DeviceResult};
use tetrad_control::link::{Notification, Suspension};
use tetrad_services::backup::local::LocalHeader;

use super::super::services::{ControlBackup, ControlBackupDone, utc_parts};
use super::super::{Effect, State, Subsystem};
use super::{Control, ControlError, StateModel};

/// This side of one local backup (stock: the job, the file object `+0x20`, the suspension).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum LocalBackup {
    #[default]
    Idle,
    /// The create job runs.
    Creating,
    /// Chunks go out on the backup timer.
    Sending,
    /// `can_apply_backup` was accepted; chunks come in.
    Receiving,
    /// The restore job runs.
    Restoring,
}

/// A link state change that waits for the end of the current step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LinkOp {
    Suspend(Suspension),
    Resume,
}

fn update(build: impl FnOnce(&mut LocalBackupMessage)) -> LocalBackupMessage {
    let mut message = LocalBackupMessage {
        action: Some(MessageAction::Update.into()),
        ..Default::default()
    };
    build(&mut message);
    message
}

/// `0x17cdc8`.
fn refused() -> LocalBackupMessage {
    update(|m| m.can_apply_backup = Some(false))
}

/// The varying fields of `0x47a1c8`. `created` is `%FT%TZ` UTC (`0x6695c4`); `name` is `QC_`
/// and that date (I: the stock name step after `0x6695c4` is not read); `author_id` is empty
/// (I: the account id is not kept here).
fn header(state: &State, wall_clock: Option<SystemTime>) -> LocalHeader {
    let unix = wall_clock
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .unwrap_or(Duration::ZERO);
    let [year, month, day, hour, minute, second] =
        utc_parts(i64::try_from(unix.as_secs()).unwrap_or(i64::MAX));
    let created = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z");
    // `0x4962ac`: `quad` or `atma` by the device type.
    let creator = if state.platform.uses_midi_uart() {
        "quad"
    } else {
        "atma"
    };
    LocalHeader {
        creator: creator.into(),
        creator_version: tetrad_firmware::running().version.into(),
        name: format!("QC_{created}"),
        author: state.settings.settings().account.username.clone(),
        author_id: String::new(),
        created,
    }
}

/// CREATE and the `can_apply_backup` UPDATE.
pub(super) fn request(
    model: &mut StateModel<'_>,
    request: &LocalBackupMessage,
) -> DeviceResult<Vec<LocalBackupMessage>> {
    let action = request.action.unwrap_or(MessageAction::Create.into());
    if action == MessageAction::Create.into() {
        if model.local_backup != LocalBackup::Idle {
            return Ok(vec![refused()]);
        }
        let header = header(model.state, model.wall_clock);
        model
            .effects
            .push(Effect::ControlBackup(ControlBackup::Create(header)));
        model.local_backup = LocalBackup::Creating;
        model.link_op = Some(LinkOp::Suspend(Suspension::Notice));
        return Ok(Vec::new());
    }
    if action == MessageAction::Update.into() && request.can_apply_backup.is_some() {
        let can = match model.local_backup {
            LocalBackup::Idle => {
                model.local_backup = LocalBackup::Receiving;
                model.link_op = Some(LinkOp::Suspend(Suspension::Download));
                true
            }
            LocalBackup::Receiving => true,
            _ => false,
        };
        return Ok(vec![update(|m| m.can_apply_backup = Some(can))]);
    }
    Err(ModelError::Unsupported(CortexMessageType::LocalBackup))
}

/// The joined chunks: restore them when a receive is open.
pub(super) fn apply(
    model: &mut StateModel<'_>,
    json: String,
) -> DeviceResult<Vec<LocalBackupMessage>> {
    if model.local_backup != LocalBackup::Receiving {
        return Ok(vec![refused()]);
    }
    model.local_backup = LocalBackup::Restoring;
    (model.effects).push(Effect::ControlBackup(ControlBackup::Restore { json }));
    Ok(Vec::new())
}

/// A link loss (`0x6e1d74` resets the LocalBackup receiver): tetrad-control drops the chunk
/// buffers and the backup timer, so a send or a receive ends here and the link resumes. A
/// running create or restore keeps its job; [`Control::backup_done`] then drops its reply (a
/// disconnected link sends nothing) and resumes. Before this, a lost link left the state busy
/// and refused every later Control backup (review r18 F6).
pub(super) fn disconnected(model: &mut StateModel<'_>) {
    if matches!(
        model.local_backup,
        LocalBackup::Sending | LocalBackup::Receiving
    ) {
        model.link_op = Some(LinkOp::Resume);
    }
}

impl Control {
    /// The end of a [`ControlBackup`]: the file goes out in chunks (`0x17e3e8`), a restore
    /// answers `applied_backup` (`0x17dab8`); anything else resumes the link.
    pub fn backup_done(
        &mut self,
        done: ControlBackupDone,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        let mut effects = Vec::new();
        match (self.local_backup, done) {
            (LocalBackup::Creating, ControlBackupDone::Created { json }) if self.connected() => {
                self.local_backup = LocalBackup::Sending;
                self.link.send_local_backup(&json);
                return Ok(effects);
            }
            // The link was lost while the job ran ([`disconnected`]): the file is dropped.
            (LocalBackup::Creating, ControlBackupDone::Created { .. }) => {}
            (LocalBackup::Restoring, done) => {
                let applied = matches!(done, ControlBackupDone::Restored { .. });
                let reply = update(|m| m.applied_backup = Some(applied));
                effects = self.notify(Notification::Update(reply.into()), state, now)?;
            }
            (LocalBackup::Creating, ControlBackupDone::Failed(reason)) => {
                let notice = format!("local backup create failed: {reason}");
                effects.push(Effect::Notice(Subsystem::Control, notice));
            }
            (_, done) => {
                let notice = format!("local backup result without its request: {done:?}");
                return Ok(vec![Effect::Notice(Subsystem::Control, notice)]);
            }
        }
        effects.extend(self.link_op(LinkOp::Resume, state, now)?);
        Ok(effects)
    }

    /// Runs a [`LinkOp`]; a resume ends the local backup.
    pub(super) fn link_op(
        &mut self,
        op: LinkOp,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        let reason = tetrad_control::codecs::SuspendReason::Backup;
        match op {
            LinkOp::Suspend(kind) => self.suspend(kind, reason, state, now),
            LinkOp::Resume => {
                self.local_backup = LocalBackup::Idle;
                self.resume(reason, state, now)
            }
        }
    }
}

//! Updater (60), receiver `0x1391b4`. READ (action 3) replies the updater view; UPDATE
//! (action 1) runs `command` through the stock gates (`manager::decide`) and hands the work
//! to the services bridge, so a Control install takes the same pre-steps as a device install
//! (`startUpdate` `0x4e63a4`: input mask, mute, Control lock).

use tetrad_control::codecs::{EnumValue, UpdaterCommand as WireCommand, UpdaterMessage};
use tetrad_control::device::DeviceResult;
use tetrad_services::updater::manager::{self, Decision, UpdaterCommand};
use tetrad_services::updater::state::{UpdaterState, UpdaterStatus};
use tetrad_ui::UpdaterCommand as ViewCommand;

use super::{AccountRequest, Effect};

/// The updater view as the services bridge pushed it (`Effect::UpdaterReport`): state
/// (`0x13bbc0`), and status with version and changelog (`0x13bf2c`). Kept connected or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UpdaterSeen {
    state: UpdaterState,
    status: UpdaterStatus,
    version: String,
    changelog: String,
}

impl Default for UpdaterSeen {
    fn default() -> Self {
        Self {
            state: UpdaterState::Idle,
            status: UpdaterStatus::None,
            version: String::new(),
            changelog: String::new(),
        }
    }
}

impl UpdaterSeen {
    /// One pushed message; fields it does not carry stay.
    pub(super) fn observe(&mut self, message: &UpdaterMessage) {
        if let Some(state) = message.state.and_then(|state| local_state(state.number())) {
            self.state = state;
        }
        if let Some(status) = message
            .status
            .and_then(|status| local_status(status.number()))
        {
            self.status = status;
            self.version = message.new_version_id.clone().unwrap_or_default();
            self.changelog = message.changelog.clone().unwrap_or_default();
        }
    }

    fn state_message(&self) -> UpdaterMessage {
        UpdaterMessage {
            state: Some(wire(self.state as i32)),
            ..Default::default()
        }
    }

    /// Status, then version and changelog for status 1 and 2 (`0x4e51c8`, `0x4e54a8`).
    fn release_message(&self) -> UpdaterMessage {
        let release = matches!(
            self.status,
            UpdaterStatus::NewVersionAvailableToDownload | UpdaterStatus::NewVersionDownloaded
        );
        UpdaterMessage {
            status: Some(wire(self.status as i32)),
            new_version_id: release.then(|| self.version.clone()),
            changelog: release.then(|| self.changelog.clone()),
            ..Default::default()
        }
    }

    /// READ: state, status, and the release text for status 1 and 2.
    pub(super) fn read(&self) -> UpdaterMessage {
        UpdaterMessage {
            state: self.state_message().state,
            ..self.release_message()
        }
    }
}

/// The local updater enums use the wire numbers (table `0xa0db68`, `0xa0db80`).
fn wire<E: TryFrom<i32>>(number: i32) -> EnumValue<E> {
    E::try_from(number).map_or(EnumValue::Unknown(number), EnumValue::Known)
}

fn local_state(number: i32) -> Option<UpdaterState> {
    use UpdaterState as S;
    [
        S::Idle,
        S::RequestingData,
        S::DownloadingData,
        S::Updating,
        S::Reboot,
        S::Failed,
    ]
    .into_iter()
    .find(|state| *state as i32 == number)
}

fn local_status(number: i32) -> Option<UpdaterStatus> {
    use UpdaterStatus as S;
    [
        S::None,
        S::NewVersionAvailableToDownload,
        S::NewVersionDownloaded,
        S::NoNewVersionAvailable,
    ]
    .into_iter()
    .find(|status| *status as i32 == number)
}

/// UPDATE without `command` does nothing (has-bit 8 clear). `checks_allowed` is the
/// KeepAlive `is_online` flag (`+0x3d9`, `0x17c7f4` → `0x6e0740`; cleared on disconnect,
/// `0x6e1d74`). Install also opens the device updating view in stock (`0x3e9c20`); here the
/// services report drives it.
pub(super) fn update(
    seen: &UpdaterSeen,
    request: &UpdaterMessage,
    checks_allowed: bool,
    effects: &mut Vec<Effect>,
) -> DeviceResult<Option<UpdaterMessage>> {
    let Some(command) = request.command else {
        return Ok(None);
    };
    let command = match command {
        EnumValue::Known(WireCommand::Check) => UpdaterCommand::Check,
        EnumValue::Known(WireCommand::CheckAndDownload) => UpdaterCommand::CheckAndDownload,
        EnumValue::Known(WireCommand::Install) => UpdaterCommand::Install,
        EnumValue::Known(WireCommand::RestartDownloadViaControl) => {
            UpdaterCommand::RestartDownloadViaControl
        }
        EnumValue::Known(WireCommand::CancelDownload) => UpdaterCommand::CancelDownload,
        EnumValue::Unknown(_) => return Ok(None),
    };
    let view = match manager::decide(command, seen.state, seen.status, checks_allowed) {
        Decision::Ignore => return Ok(None),
        Decision::ReplyState => return Ok(Some(seen.state_message())),
        Decision::ReplyRelease => return Ok(Some(seen.release_message())),
        Decision::StartCheck => ViewCommand::Check,
        Decision::StartCheckAndDownload => ViewCommand::CheckAndDownload,
        Decision::StartInstall => ViewCommand::Install,
        Decision::CancelDownload => ViewCommand::CancelDownload,
        // `0x4e4cc0` → `0x4e7808`: the check and download run again with Cortex Control as
        // the HTTP transport (`CloudUpdaterUSB`, `forward::ForwardHttp`); the install after it
        // keeps the device pre-steps.
        Decision::RestartDownloadOverControl => {
            effects.push(Effect::ControlAccount(
                AccountRequest::RestartDownloadViaControl,
            ));
            return Ok(None);
        }
    };
    effects.push(Effect::ControlAccount(AccountRequest::Updater(view)));
    Ok(None)
}

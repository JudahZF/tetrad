//! What the updater manager does with an `UpdaterMessage` command from Cortex Control
//! (handler VA 0x001391b4).

use super::state::{UpdaterState, UpdaterStatus};

/// `UpdaterMessage.Command`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdaterCommand {
    Check = 0,
    CheckAndDownload = 1,
    Install = 2,
    RestartDownloadViaControl = 3,
    CancelDownload = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Ignore,
    /// Busy: reply with the current state.
    ReplyState,
    /// Reply with status plus version id and changelog ([`super::cloud::release_notes`]).
    ReplyRelease,
    /// `startCheck` (VA 0x004e7960).
    StartCheck,
    /// Clear stale files, then `startCheckAndDownload` (VA 0x004e6e38).
    StartCheckAndDownload,
    /// `startUpdate` (VA 0x004e63a4): publish state 1, then run the installer.
    StartInstall,
    /// Abort Wi-Fi requests, [`super::cloud::reset`], then check-and-download over Control
    /// (VAs 0x004e4cc0, 0x004e7808).
    RestartDownloadOverControl,
    /// Abort everything, reset partial files and flags, then re-check (VA 0x004e4c70).
    CancelDownload,
}

fn busy(state: UpdaterState) -> bool {
    matches!(
        state,
        UpdaterState::RequestingData | UpdaterState::DownloadingData | UpdaterState::Updating
    )
}

/// `checks_allowed` is the Control-side flag (+0x3d9) that gates CHECK and CHECK_AND_DOWNLOAD;
/// its source is not recovered.
pub fn decide(
    command: UpdaterCommand,
    state: UpdaterState,
    status: UpdaterStatus,
    checks_allowed: bool,
) -> Decision {
    use UpdaterStatus::{
        NewVersionAvailableToDownload as Available, NewVersionDownloaded as Downloaded,
    };
    match command {
        UpdaterCommand::Check | UpdaterCommand::CheckAndDownload if !checks_allowed => {
            Decision::Ignore
        }
        UpdaterCommand::Check | UpdaterCommand::CheckAndDownload if busy(state) => {
            Decision::ReplyState
        }
        UpdaterCommand::Check if matches!(status, Available | Downloaded) => Decision::ReplyRelease,
        UpdaterCommand::Check => Decision::StartCheck,
        UpdaterCommand::CheckAndDownload if status == Downloaded => Decision::ReplyRelease,
        UpdaterCommand::CheckAndDownload => Decision::StartCheckAndDownload,
        UpdaterCommand::Install if !busy(state) && status == Downloaded => Decision::StartInstall,
        UpdaterCommand::Install => Decision::Ignore,
        UpdaterCommand::RestartDownloadViaControl => Decision::RestartDownloadOverControl,
        UpdaterCommand::CancelDownload
            if status == Downloaded
                || matches!(state, UpdaterState::Updating | UpdaterState::Reboot) =>
        {
            Decision::Ignore
        }
        UpdaterCommand::CancelDownload => Decision::CancelDownload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_command_gates() {
        use UpdaterState::*;
        use UpdaterStatus::*;
        assert_eq!(
            decide(UpdaterCommand::Check, Idle, None, false),
            Decision::Ignore
        );
        assert_eq!(
            decide(UpdaterCommand::Check, DownloadingData, None, true),
            Decision::ReplyState
        );
        assert_eq!(
            decide(
                UpdaterCommand::CheckAndDownload,
                Idle,
                NewVersionAvailableToDownload,
                true
            ),
            Decision::StartCheckAndDownload
        );
        assert_eq!(
            decide(UpdaterCommand::Install, Idle, NewVersionDownloaded, true),
            Decision::StartInstall
        );
        assert_eq!(
            decide(
                UpdaterCommand::Install,
                Idle,
                NewVersionAvailableToDownload,
                true
            ),
            Decision::Ignore
        );
        assert_eq!(
            decide(UpdaterCommand::CancelDownload, Updating, None, true),
            Decision::Ignore
        );
    }
}

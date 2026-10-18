//! The firmware updater: the check, the download (also over Cortex Control) and the install.

use super::*;

/// An update download and its stop flag. [`Services::cancel_download`] sets the flag; the
/// worker's curl is killed (stock CancelDownload `0x13b87c`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadJob {
    pub offer: UpdateOffer,
    pub cancel: CancelToken,
    /// Cortex Control carries the transfer instead of the device's own HTTP.
    pub via_control: Option<ControlTransport>,
}

/// Cortex Control as the HTTP transport of an update check and download (stock
/// `CloudUpdaterUSB`, chosen by RestartDownloadViaControl `0x4e4cc0` → `0x4e7808`): the
/// handle of [`super::control::forward::ForwardHttp`]. Equality is identity.
#[derive(Clone)]
pub struct ControlTransport(Arc<super::control::forward::ForwardLink>);

impl ControlTransport {
    pub fn new(link: super::control::forward::ForwardLink) -> Self {
        Self(Arc::new(link))
    }

    /// The client for one job: `files` is shared between the client (a download writes its
    /// chunks) and the updater code.
    pub(super) fn client<'a, F: Files>(
        &self,
        files: &'a std::cell::RefCell<&'a mut F>,
    ) -> super::control::forward::ForwardHttp<&'a std::cell::RefCell<&'a mut F>> {
        super::control::forward::ForwardHttp::new((*self.0).clone(), files)
    }
}

impl PartialEq for ControlTransport {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ControlTransport {}

impl fmt::Debug for ControlTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ControlTransport")
    }
}

/// What Cortex Control and the UI see of the updater.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Updater {
    pub state: UpdaterState,
    pub status: UpdaterStatus,
    pub offer: Option<UpdateOffer>,
}

impl Services {
    /// Signed update check (`/firmware/v1/latest` or the account's channel `suffix`). Sends the
    /// DSP requests for the signing material and client id. [`Self::tick`] sends the check to
    /// the worker once both replies are in, or reports `Unsupported` after
    /// `SIGNING_REPLY_TIMEOUT` (2 s). An offer is published (changelog and available flag) for
    /// a later [`Self::download`].
    pub fn check_for_update(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        suffix: Option<&str>,
        then_download: bool,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let io = lend(backends)?;
        self.start_check(io.files, settings, suffix, then_download, None, now)
    }

    /// Cortex Control RestartDownloadViaControl (`0x4e4cc0` → `0x4e7808`): a running
    /// download stops, the updater flags and package files go, state and status return to 0,
    /// then the check and the download run again with Cortex Control as the HTTP transport
    /// (`CloudUpdaterUSB`, forward 61). The install that follows takes the same pre-steps
    /// (input mask, mute, Control lock).
    ///
    /// Refused while Updating or Reboot (owned: stock has no gate and would unmute and
    /// delete the package during an install).
    pub fn restart_download_over_control(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        control: ControlTransport,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if self.installing() {
            return Err(ServiceError::NotReady("an update is installing"));
        }
        let io = lend(backends)?;
        let mut effects = self.cancel_download();
        tetrad_services::updater::cloud::reset(io.files)?;
        self.updater.status = UpdaterStatus::None;
        self.updater.offer = None;
        self.release_notes = None;
        effects.extend(self.report(UpdaterState::Idle));
        effects.extend(self.start_check(io.files, settings, None, true, Some(control), now)?);
        Ok(effects)
    }

    pub(super) fn start_check(
        &mut self,
        files: &mut impl Files,
        settings: &mut DeviceSettings,
        suffix: Option<&str>,
        then_download: bool,
        via_control: Option<ControlTransport>,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        self.update_check_due = false;
        self.load_cloud(settings)?;
        let job = CloudCall::UpdateCheck {
            suffix: suffix.map(str::to_owned),
            then_download,
            via_control,
        };
        Ok(self.queue_signed(Serial::read(files)?, job, now))
    }

    /// The update check result: status, offer and release notes, then the state reports.
    pub(super) fn checked(
        &mut self,
        checked: Result<(CheckOutcome, Option<(String, String)>), ServiceError>,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        // An install outlives any check (owned): its state and progress stay; the status and
        // offer are stored only.
        let installing = self.installing();
        let report = |this: &mut Self, state| {
            if installing {
                Vec::new()
            } else {
                this.report(state)
            }
        };
        match checked {
            Ok((CheckOutcome::UpToDate(message), _)) => {
                self.updater.status = UpdaterStatus::NoNewVersionAvailable;
                effects.extend(report(self, UpdaterState::Idle));
                effects.push(notice(format!("no new version: {message}")));
            }
            Ok((CheckOutcome::Available(offer), notes)) => {
                effects.push(notice(format!("update {} available", offer.version_id)));
                self.updater.status = UpdaterStatus::NewVersionAvailableToDownload;
                self.release_notes = notes;
                self.updater.offer = Some(offer);
                effects.extend(report(self, UpdaterState::Idle));
            }
            Err(error) => {
                effects.extend(report(self, UpdaterState::Failed));
                effects.push(notice(match &error {
                    ServiceError::Update(update) => {
                        format!("update check failed ({}): {error}", update.stock_code())
                    }
                    _ => format!("update check failed: {error}"),
                }));
            }
        }
        effects
    }

    pub(super) fn due_check(
        &mut self,
        files: &mut impl Files,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Vec<Effect> {
        if !std::mem::take(&mut self.update_check_due) {
            return Vec::new();
        }
        self.start_check(files, settings, None, false, None, now)
            .unwrap_or_else(|error| vec![notice(format!("automatic update check: {error}"))])
    }

    /// Download the published offer on the platform worker.
    pub fn download(&mut self) -> Result<Vec<Effect>, ServiceError> {
        self.start_download(None)
    }

    /// A download and its retries over `via_control`, or the device's own HTTP.
    pub(super) fn start_download(
        &mut self,
        via_control: Option<ControlTransport>,
    ) -> Result<Vec<Effect>, ServiceError> {
        let offer = self
            .updater
            .offer
            .clone()
            .ok_or(ServiceError::NotReady("no update offer to download"))?;
        self.download_via = via_control;
        self.download_progress = None;
        self.download_attempts = 1;
        let mut effects = self.report(UpdaterState::DownloadingData);
        effects.push(self.download_job(offer));
        Ok(effects)
    }

    /// One download try with a fresh stop flag.
    pub(super) fn download_job(&mut self, offer: UpdateOffer) -> Effect {
        let cancel = CancelToken::default();
        self.download_cancel = Some(cancel.clone());
        Effect::Service(ServiceJob::Download(DownloadJob {
            offer,
            cancel,
            via_control: self.download_via.clone(),
        }))
    }

    /// Run the installer on a downloaded package (`startUpdate`, VA 0x004e63a4): state
    /// Updating, then the input mask cleared (`0x001ec83c(0x3f)`) and the outputs muted
    /// (`0x00876470(1, 1)`) before the installer starts. One run at a time. The installer is
    /// Tetrad's `tetrad-update.sh` ([`install::INSTALLER`]): the stock `sysupdate_run.sh` plus
    /// the step that keeps the Tetrad boot hook and the SSH keys on the new root.
    ///
    /// Clearing mask bits 1 and `0x10` tells Cortex Control the device is locked
    /// (`0x1ec83c` → `0x16ad60(…, 1)`). Stock sends it only when the bits were set; the
    /// install starts from the touchscreen, which LOCK holds, so the device is not locked.
    ///
    /// Bring-up never gets here: `RunOptions::refusal` and `handoff_refusal` refuse INSTALL
    /// before it.
    pub fn install(&mut self) -> Result<Vec<Effect>, ServiceError> {
        if self.updater.state == UpdaterState::Updating {
            return Ok(Vec::new());
        }
        if self.updater.status != UpdaterStatus::NewVersionDownloaded {
            return Err(ServiceError::NotReady("no downloaded update to install"));
        }
        self.install_progress = None;
        self.inputs_masked = true;
        let mut effects = self.report(UpdaterState::Updating);
        effects.push(super::control::lock_notice(true));
        effects.push(Effect::Dsp(DspCommand::MasterMute(1 | 1 << 1)));
        effects.push(Effect::Service(ServiceJob::Install));
        Ok(effects)
    }

    /// An install runs or ended in Reboot: no check or download may change the state.
    pub(super) fn installing(&self) -> bool {
        matches!(
            self.updater.state,
            UpdaterState::Updating | UpdaterState::Reboot
        )
    }

    /// A licence activation is in flight: queued, signing, delayed after a pair or waiting
    /// for the DSP answer (stock busy flag `+0x30`, 0x001f11d4). For Cortex Control.
    pub fn licence_refreshing(&self) -> bool {
        self.licence.is_some()
    }

    /// The install pre-step holds the inputs until the installer ends ([`Self::install`]).
    pub fn inputs_masked(&self) -> bool {
        self.inputs_masked
    }

    /// A failed download is retried while it may be (stock 0x0046bc94: fewer than 5 tries,
    /// not an integrity failure, online). A cancelled download's result is dropped.
    pub(super) fn downloaded(&mut self, result: Result<(), JobError>) -> Vec<Effect> {
        if self.cancelled_downloads > 0 {
            self.cancelled_downloads -= 1;
            return Vec::new();
        }
        self.download_cancel = None;
        match result {
            Ok(()) => {
                self.updater.status = UpdaterStatus::NewVersionDownloaded;
                self.report(UpdaterState::Idle)
            }
            Err(JobError::Update(error))
                if error.retryable(self.download_attempts, self.online == Some(true)) =>
            {
                self.download_attempts += 1;
                let offer = self.updater.offer.clone();
                let mut effects = vec![notice(format!(
                    "update download failed, try {}: {error}",
                    self.download_attempts
                ))];
                effects.extend(offer.map(|offer| self.download_job(offer)));
                effects
            }
            Err(error) => {
                let mut effects = self.report(UpdaterState::Failed);
                effects.push(notice(format!("update download failed: {error}")));
                effects
            }
        }
    }

    pub(super) fn installed(&mut self, result: Result<InstallOutcome, JobError>) -> Vec<Effect> {
        match result {
            Ok(InstallOutcome::Installed) => {
                let mut effects = self.report(UpdaterState::Reboot);
                effects.push(Effect::Power(PowerAction::Restart(RestartReason::Direct)));
                effects
            }
            result => {
                // Owned: a failed run gives the inputs and outputs back. Restoring the mask
                // unlocks Cortex Control (`0x1ec714` → `0x16ad60(…, 0)`).
                self.inputs_masked = false;
                let mut effects = self.report(UpdaterState::Failed);
                effects.push(super::control::lock_notice(false));
                effects.push(Effect::Dsp(DspCommand::MasterMute(0)));
                effects.push(notice(match result {
                    Ok(InstallOutcome::Failed {
                        code: Some(code), ..
                    }) => install::failure_text(code),
                    Ok(InstallOutcome::Failed { code: None, .. }) => {
                        "installer killed by a signal".to_owned()
                    }
                    Err(error) => format!("update install failed: {error}"),
                    Ok(InstallOutcome::Installed) => unreachable!(),
                }));
                effects
            }
        }
    }

    /// Separate stock senders: state (`0x13bbc0`), status (`0x13bf2c`), on change only.
    pub(super) fn report(&mut self, state: UpdaterState) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.updater.state != state {
            self.updater.state = state;
            effects.push(Effect::UpdaterReport(UpdaterMessage {
                state: Some(EnumValue::from(
                    WireState::try_from(state as i32).unwrap_or(WireState::Idle),
                )),
                ..update_message()
            }));
        }
        if self.reported_status != self.updater.status {
            self.reported_status = self.updater.status;
            let mut message = UpdaterMessage {
                status: Some(EnumValue::from(
                    WireStatus::try_from(self.updater.status as i32).unwrap_or(WireStatus::None),
                )),
                ..update_message()
            };
            if matches!(
                self.updater.status,
                UpdaterStatus::NewVersionAvailableToDownload | UpdaterStatus::NewVersionDownloaded
            ) {
                let (version, changelog) = self.release_notes.clone().unwrap_or_else(|| {
                    (
                        self.updater
                            .offer
                            .as_ref()
                            .map_or_else(String::new, |offer| offer.version_id.clone()),
                        String::new(),
                    )
                });
                message.new_version_id = Some(version.replace(['\n', '\t', '\r'], ""));
                message.changelog = Some(changelog);
            }
            effects.push(Effect::UpdaterReport(message));
        }
        effects
    }

    /// Poll worker-owned files on the service tick; send changed, known percentages only.
    pub(super) fn progress(&mut self, files: &mut impl Files) -> Result<Vec<Effect>, FileError> {
        let mut effects = Vec::new();
        if self.updater.state == UpdaterState::DownloadingData
            && let Some(value) =
                download_progress(files)?.filter(|value| Some(*value) != self.download_progress)
        {
            self.download_progress = Some(value);
            effects.push(Effect::UpdaterReport(UpdaterMessage {
                download_progress: Some(value),
                ..update_message()
            }));
        }
        if self.updater.state == UpdaterState::Updating {
            let value = files
                .read(Path::new(paths::STATUS_FILE))?
                .as_deref()
                .and_then(install_progress);
            if let Some(value) = value.filter(|value| Some(*value) != self.install_progress) {
                self.install_progress = Some(value);
                effects.push(Effect::UpdaterReport(UpdaterMessage {
                    installation_progress: Some(value),
                    ..update_message()
                }));
            }
        }
        Ok(effects)
    }

    /// Cancel the update download (`0x13b87c` sends the CancelDownload notification). The
    /// stop flag kills the worker's curl; the late result is dropped and the offer stays
    /// available (owned).
    pub fn cancel_download(&mut self) -> Vec<Effect> {
        if self.updater.state != UpdaterState::DownloadingData {
            return Vec::new();
        }
        if let Some(cancel) = self.download_cancel.take() {
            cancel.cancel();
        }
        self.cancelled_downloads += 1;
        self.download_progress = None;
        let mut effects = vec![Effect::UpdaterReport(UpdaterMessage {
            command: Some(tetrad_control::codecs::UpdaterCommand::CancelDownload.into()),
            ..update_message()
        })];
        effects.extend(self.report(UpdaterState::Idle));
        effects
    }

    /// The Updates page. The running version is the UI bridge's; the channel is always the
    /// official one (gap: no updater suffix or beta flag is read yet).
    pub fn updater_view(&self) -> UpdaterView {
        use tetrad_ui::{UpdaterState as State, UpdaterStatus as Status};
        let state = match self.updater.state {
            UpdaterState::Idle => State::Idle,
            UpdaterState::RequestingData => State::RequestingData,
            UpdaterState::DownloadingData => State::DownloadingData,
            UpdaterState::Updating => State::Updating,
            UpdaterState::Reboot => State::Reboot,
            UpdaterState::Failed => State::Failed,
        };
        let status = match self.updater.status {
            UpdaterStatus::None => Status::None,
            UpdaterStatus::NewVersionAvailableToDownload => Status::NewVersionAvailable,
            UpdaterStatus::NewVersionDownloaded => Status::NewVersionDownloaded,
            UpdaterStatus::NoNewVersionAvailable => Status::NoNewVersion,
        };
        let percent = match self.updater.state {
            UpdaterState::DownloadingData => self.download_progress,
            UpdaterState::Updating => self.install_progress,
            _ => None,
        };
        let (version, changelog) = self.release_notes.clone().unwrap_or_else(|| {
            (
                self.updater
                    .offer
                    .as_ref()
                    .map_or_else(String::new, |offer| offer.version_id.clone()),
                String::new(),
            )
        });
        let version = version.replace(['\n', '\t', '\r'], "");
        UpdaterView {
            stock_after_install: tetrad_firmware::Firmware::by_version(&version).is_none(),
            available_version: (!version.is_empty()).then_some(version),
            state,
            status,
            progress: percent.map(|value| (value / 100.0).clamp(0.0, 1.0)),
            changelog,
            online: self.online == Some(true),
            ..UpdaterView::default()
        }
    }
}

/// Floating percentage points, without the UI helper's integer truncation (`0x4e8824`).
pub(super) fn download_progress(files: &mut impl Files) -> Result<Option<f32>, FileError> {
    let (Some(name), Some(expected)) = (
        files.read(Path::new(paths::DOWNLOAD_FNAME))?,
        files.read(Path::new(paths::DOWNLOAD_FSIZE))?,
    ) else {
        return Ok(None);
    };
    let (Ok(name), Ok(expected)) = (std::str::from_utf8(&name), std::str::from_utf8(&expected))
    else {
        return Ok(None);
    };
    let Some(expected) = expected
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|&bytes| bytes > 0)
    else {
        return Ok(None);
    };
    let have = files.size(Path::new(name.trim()))?.unwrap_or(0);
    Ok(Some(
        ((have as f64 / (expected as f64 / 95.0)) as f32).min(95.0),
    ))
}

pub(super) fn install_progress(raw: &[u8]) -> Option<f32> {
    match install::parse_status(raw)? {
        install::StatusToken::Start => Some(0.0),
        install::StatusToken::Step(step) => {
            Some(((f64::from(step.get()) * (100.0 / 14.0)) as f32).min(100.0))
        }
        _ => None,
    }
}

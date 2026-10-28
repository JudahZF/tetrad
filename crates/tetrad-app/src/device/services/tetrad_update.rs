//! Owned (Tetrad): the Tetrad Updates page. The check and the install of a newer Tetrad
//! release run on the bulk worker ([`tetrad_services::updater::tetrad`]); a good install
//! restarts the unit. CorOS does not change. A release can also come over USB
//! (`device/usb.rs`): it is offered here, and UPDATE TETRAD on the unit installs it.

use tetrad_services::updater::tetrad::{self, Manifest, Offer, Source};
use tetrad_ui::{TetradUpdateState, TetradUpdateView};

use super::*;

/// The running Tetrad.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) enum TetradUpdate {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Offer),
    Installing {
        offer: Offer,
        progress: Option<f32>,
    },
    /// `offer`: the release that UPDATE TETRAD tries again. `usb`: a USB release failed and
    /// must be sent again.
    Failed {
        error: String,
        offer: Option<Offer>,
        usb: bool,
    },
}

impl Services {
    /// CHECK TETRAD: the newest Tetrad release, on the bulk worker.
    pub fn check_tetrad_update(&mut self) -> Vec<Effect> {
        if matches!(
            self.tetrad,
            TetradUpdate::Checking | TetradUpdate::Installing { .. }
        ) {
            return Vec::new();
        }
        self.tetrad = TetradUpdate::Checking;
        vec![Effect::Service(ServiceJob::TetradCheck)]
    }

    /// UPDATE TETRAD: download, check, install and arm the offered release. Refused while a
    /// CorOS download or install runs, so the two never write p4 at the same time.
    pub fn install_tetrad_update(&mut self) -> Result<Vec<Effect>, ServiceError> {
        let offer = match &self.tetrad {
            TetradUpdate::Available(offer)
            | TetradUpdate::Failed {
                offer: Some(offer), ..
            } => offer.clone(),
            TetradUpdate::Installing { .. } => return Ok(Vec::new()),
            _ => return Err(ServiceError::NotReady("no Tetrad update to install")),
        };
        if self.installing() || self.updater.state == UpdaterState::DownloadingData {
            return Err(ServiceError::NotReady("a CorOS update is running"));
        }
        self.tetrad = TetradUpdate::Installing {
            offer: offer.clone(),
            progress: None,
        };
        Ok(vec![Effect::Service(ServiceJob::TetradInstall(offer))])
    }

    /// A USB install may stage its files: no Tetrad or CorOS install and no CorOS download
    /// runs, since each of them writes p4.
    pub fn usb_staging_allowed(&self) -> Result<(), ServiceError> {
        if self.tetrad_installing() {
            return Err(ServiceError::NotReady("a Tetrad install is running"));
        }
        if self.installing() || self.updater.state == UpdaterState::DownloadingData {
            return Err(ServiceError::NotReady("a CorOS update is running"));
        }
        Ok(())
    }

    /// The release that `tetrad-usb install` pushed is on p4 as `.part` files: UPDATE TETRAD
    /// installs it.
    pub fn offer_usb_update(&mut self, manifest: Manifest) -> Result<Vec<Effect>, ServiceError> {
        self.offer_staged_update(manifest, "USB")
    }

    /// Files that came over `via` ("USB", or "Remote" in a dev build) are on p4 as `.part`
    /// files: UPDATE TETRAD installs them.
    pub fn offer_staged_update(
        &mut self,
        manifest: Manifest,
        via: &str,
    ) -> Result<Vec<Effect>, ServiceError> {
        self.usb_staging_allowed()?;
        let offer = Offer {
            source: Source::Usb,
            manifest,
        };
        let text = format!(
            "Tetrad {} from {via}: Settings > Tetrad > Tetrad Updates > UPDATE TETRAD",
            offer.version()
        );
        self.tetrad = TetradUpdate::Available(offer);
        Ok(vec![notice(text)])
    }

    /// A Tetrad install runs: no CorOS install may start.
    pub(super) fn tetrad_installing(&self) -> bool {
        matches!(self.tetrad, TetradUpdate::Installing { .. })
    }

    /// A new USB upload replaces the `.part` files: an earlier USB offer must not install
    /// them any more.
    pub fn withdraw_usb_offer(&mut self) {
        if matches!(&self.tetrad, TetradUpdate::Available(offer) if offer.source == Source::Usb) {
            self.tetrad = TetradUpdate::Idle;
        }
    }

    pub(super) fn tetrad_checked(&mut self, result: Result<Option<Offer>, String>) -> Vec<Effect> {
        // A USB offer or an install that started since the check owns the state now.
        if self.tetrad != TetradUpdate::Checking {
            return Vec::new();
        }
        let (state, text) = match result {
            Ok(Some(offer)) => {
                let text = format!("Tetrad {} available", offer.version());
                (TetradUpdate::Available(offer), text)
            }
            Ok(None) => (
                TetradUpdate::UpToDate,
                format!("Tetrad {VERSION} is up to date"),
            ),
            Err(error) => {
                let text = format!("Tetrad update check failed: {error}");
                let state = TetradUpdate::Failed {
                    error,
                    offer: None,
                    usb: false,
                };
                (state, text)
            }
        };
        self.tetrad = state;
        vec![notice(text)]
    }

    pub(super) fn tetrad_installed(&mut self, result: Result<(), String>) -> Vec<Effect> {
        let TetradUpdate::Installing { offer, .. } = std::mem::take(&mut self.tetrad) else {
            return Vec::new();
        };
        match result {
            Ok(()) => {
                let text = format!("Tetrad {} installed; restarting", offer.version());
                self.tetrad = TetradUpdate::Installing {
                    offer,
                    progress: Some(1.0),
                };
                vec![
                    notice(text),
                    Effect::Power(PowerAction::Restart(RestartReason::Direct)),
                ]
            }
            Err(error) => {
                let text = format!("Tetrad update failed: {error}");
                // A failed install removes the `.part` files, so a USB release must be
                // pushed again; a GitHub release downloads again on retry.
                let usb = offer.source == Source::Usb;
                let offer = (!usb).then_some(offer);
                self.tetrad = TetradUpdate::Failed { error, offer, usb };
                vec![notice(text)]
            }
        }
    }

    /// The binary download share, on the service tick.
    pub(super) fn tetrad_progress(&mut self, files: &mut impl Files) -> Result<(), FileError> {
        if let TetradUpdate::Installing { offer, progress } = &mut self.tetrad
            && *progress != Some(1.0)
        {
            *progress = tetrad::progress(files, offer)?.or(*progress);
        }
        Ok(())
    }

    pub fn tetrad_update_view(&self) -> TetradUpdateView {
        let state = match &self.tetrad {
            TetradUpdate::Idle => TetradUpdateState::Idle,
            TetradUpdate::Checking => TetradUpdateState::Checking,
            TetradUpdate::UpToDate => TetradUpdateState::UpToDate,
            TetradUpdate::Available(offer) => TetradUpdateState::Available {
                version: offer.version().to_string(),
                usb: offer.source == Source::Usb,
            },
            TetradUpdate::Installing { offer, progress } => TetradUpdateState::Installing {
                version: offer.version().to_string(),
                progress: *progress,
            },
            TetradUpdate::Failed { error, offer, usb } => TetradUpdateState::Failed {
                error: error.clone(),
                retry: offer.as_ref().map(|offer| offer.version().to_string()),
                usb: *usb,
            },
        };
        TetradUpdateView {
            current_version: VERSION.to_owned(),
            state,
            online: self.online == Some(true),
        }
    }
}

/// [`ServiceJob::TetradCheck`] on the worker.
pub(super) fn check(http: &mut impl HttpClient) -> Result<Option<Offer>, String> {
    let current = (VERSION.parse()).map_err(|e| format!("the running version {VERSION}: {e}"))?;
    tetrad::check(http, &current).map_err(|e| e.to_string())
}

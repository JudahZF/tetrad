//! Owned (Tetrad): the Tetrad Updates page. The check and the install of a newer Tetrad
//! release run on the bulk worker ([`tetrad_services::updater::tetrad`]); a good install
//! restarts the unit. CorOS does not change. A release can also come over USB
//! (`device/usb.rs`): it is offered here, and UPDATE TETRAD on the unit installs it.
//!
//! The Update channel (Stable or Nightly) selects the releases that CHECK TETRAD looks for. It
//! is kept in [`CHANNEL_FILE`], a file of its own, so older Tetrad builds never read it.

use serde::{Deserialize, Serialize};
use tetrad_services::updater::tetrad::{self, Channel, Found, Manifest, Offer, Source};
use tetrad_ui::{ReleaseChannel, TetradUpdateState, TetradUpdateView};

use super::*;

/// The running Tetrad.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The Update channel: `{"channel":"nightly"}`. No file, or no `channel`, is Stable.
pub const CHANNEL_FILE: &str = "/media/p4/tetrad/updates.json";
const MAX_CHANNEL_BYTES: u64 = 4 * 1024;

/// [`CHANNEL_FILE`]. Unknown fields are ignored, so a file from a later build still gives its
/// channel here.
#[derive(Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    channel: Channel,
}

pub fn release_channel(channel: Channel) -> ReleaseChannel {
    match channel {
        Channel::Stable => ReleaseChannel::Stable,
        Channel::Nightly => ReleaseChannel::Nightly,
    }
}

pub fn channel(channel: ReleaseChannel) -> Channel {
    match channel {
        ReleaseChannel::Stable => Channel::Stable,
        ReleaseChannel::Nightly => Channel::Nightly,
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) enum TetradUpdate {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Offer),
    /// Stable channel on a nightly: the newest stable, older than the running nightly. UPDATE
    /// TETRAD installs it only from a tap; the check never offers it as an update.
    Older(Offer),
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
        vec![Effect::Service(ServiceJob::TetradCheck(
            self.tetrad_channel,
        ))]
    }

    pub fn tetrad_channel(&self) -> Channel {
        self.tetrad_channel
    }

    /// Reads [`CHANNEL_FILE`] at start. A file that cannot be read gives Stable and a notice.
    pub fn load_tetrad_channel(&mut self, files: &mut impl Files) -> Option<Effect> {
        let bytes = match files.read_bounded(Path::new(CHANNEL_FILE), MAX_CHANNEL_BYTES) {
            Ok(None) => return None,
            Ok(Some(bytes)) if bytes.len() as u64 > MAX_CHANNEL_BYTES => {
                return Some(notice(format!("{CHANNEL_FILE}: the file is too large")));
            }
            Ok(Some(bytes)) => bytes,
            Err(error) => return Some(notice(format!("{CHANNEL_FILE}: {error}"))),
        };
        match serde_json::from_slice::<Stored>(&bytes) {
            Ok(stored) => {
                self.tetrad_channel = stored.channel;
                None
            }
            Err(error) => Some(notice(format!("{CHANNEL_FILE}: {error}; Stable is used"))),
        }
    }

    /// The Update channel, saved in [`CHANNEL_FILE`]. An offer of the other channel goes away,
    /// and the result of a running check is dropped (`tetrad_checked`). Refused while an
    /// install runs.
    pub fn set_tetrad_channel(
        &mut self,
        files: &mut impl Files,
        channel: Channel,
    ) -> Result<Vec<Effect>, ServiceError> {
        if self.tetrad_installing() {
            return Err(ServiceError::NotReady("a Tetrad install is running"));
        }
        if channel == self.tetrad_channel {
            return Ok(Vec::new());
        }
        let bytes = serde_json::to_vec(&Stored { channel }).unwrap_or_default();
        let path = Path::new(CHANNEL_FILE);
        (path.parent())
            .map_or(Ok(()), |dir| files.create_dirs(dir))
            .and_then(|()| files.write_atomic(path, &bytes))?;
        self.tetrad_channel = channel;
        // A USB offer is not of a channel: it stays.
        let usb =
            matches!(&self.tetrad, TetradUpdate::Available(offer) if offer.source == Source::Usb);
        if !usb {
            self.tetrad = TetradUpdate::Idle;
        }
        Ok(vec![notice(format!(
            "Tetrad Updates: the Update channel is {}",
            channel.name()
        ))])
    }

    /// UPDATE TETRAD: download, check, install and arm the offered release. Refused while a
    /// CorOS download or install runs, so the two never write p4 at the same time.
    pub fn install_tetrad_update(&mut self) -> Result<Vec<Effect>, ServiceError> {
        let offer = match &self.tetrad {
            TetradUpdate::Available(offer)
            | TetradUpdate::Older(offer)
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

    pub(super) fn tetrad_checked(
        &mut self,
        channel: Channel,
        result: Result<Found, String>,
    ) -> Vec<Effect> {
        // A USB offer or an install that started since the check owns the state now. A check
        // of the channel before a change never becomes an offer: a later check may be the one
        // that `Checking` waits for.
        if self.tetrad != TetradUpdate::Checking || channel != self.tetrad_channel {
            return Vec::new();
        }
        let (state, text) = match result {
            Ok(Found::Newer(offer)) => {
                let text = format!("Tetrad {} available", offer.version());
                (TetradUpdate::Available(offer), text)
            }
            Ok(Found::Older(offer)) => {
                let text = format!(
                    "Tetrad {VERSION} is newer than the newest stable release, {}",
                    offer.version()
                );
                (TetradUpdate::Older(offer), text)
            }
            Ok(Found::UpToDate) => (
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
                // pushed again; a GitHub release downloads again on retry. The rolling nightly
                // may have been replaced since the check (new hashes): check again instead.
                let usb = offer.source == Source::Usb;
                let nightly = offer.source == Source::Release(tetrad::NIGHTLY_TAG.to_owned());
                let offer = (!usb && !nightly).then_some(offer);
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
            TetradUpdate::Older(offer) => TetradUpdateState::Older {
                version: offer.version().to_string(),
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
            channel: release_channel(self.tetrad_channel),
        }
    }
}

/// [`ServiceJob::TetradCheck`] on the worker.
pub(super) fn check(http: &mut impl HttpClient, channel: Channel) -> Result<Found, String> {
    let current = (VERSION.parse()).map_err(|e| format!("the running version {VERSION}: {e}"))?;
    tetrad::check(http, tetrad::TRUSTED_KEY, &current, channel).map_err(|e| e.to_string())
}

//! Settings and capture view projections (owner: w4-settings-views). The services and capture
//! bridges keep their state private; after each device pass `Device::sync_views` copies their
//! projection into [`State::views`](super::super::State), and the UI bridge lays it over its
//! snapshot with [`apply`]. A changed projection requests a frame.

use std::mem::take;
use std::path::Path;
use std::sync::Arc;

use tetrad_capture::orchestration::NcState;
use tetrad_capture::v2::{Nc2Input, Nc2State};
use tetrad_dsp_protocol::responses::Soc2ArmDiagnostics;
use tetrad_services::cloud::services::ProductKind;
use tetrad_services::exec::Files;
use tetrad_ui::{
    AuditionSource, BackupView, CaptureQuestion, CaptureStep, CaptureView, CloudFolderView,
    CloudItem, CloudScope, ItemKind, Nc2Dialog, QuarantineRow, Screen, SettingsPage, StorageView,
    UiSnapshot, UpdaterView, UploadProgress, UsbTone, UsbValue, WifiView,
};

use super::super::actions::CycleCapture;
use super::super::platform::ServiceBackends;
use super::super::services::{DownloadOutcome, DownloadsRow, browse_scope, product_kind};
use super::super::{Device, Platform, info};

const LICENCE_DIR: &str = "/opt/neuraldsp/ThirdPartyLicenses/";
/// Owned cap on a licence or release-notes text the UI lays out.
const MAX_LICENCE_BYTES: usize = 256 * 1024;

/// Views whose state lives outside `State`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsViews {
    pub wifi: WifiView,
    /// Extensions pages (`extensions::Extensions::view`).
    pub extensions: tetrad_ui::ExtensionsView,
    /// Mappings page (`mappings::Mappings::view`).
    pub mappings: tetrad_ui::MappingsView,
    /// The palette and Settings → Appearance (`appearance::Appearance::view`).
    pub appearance: tetrad_ui::AppearanceView,
    /// Remote page (`remote::Remote::view`).
    pub remote: tetrad_ui::RemoteView,
    /// Boot Selector page (`boot_selector::BootSelector::view`).
    pub boot_selector: Option<bool>,
    /// The last application toast (`remote::Remote::toast`).
    pub toast: Option<tetrad_ui::ToastView>,
    /// `Control::cloud_route` is set: the cloud is usable without Wi-Fi (re-r44 §3).
    pub control_online: bool,
    /// Cortex Control's link for the cloud dialog (whatever the cloud route).
    pub control_link: tetrad_ui::ControlLinkState,
    pub updater: UpdaterView,
    pub tetrad_update: tetrad_ui::TetradUpdateView,
    pub backups: BackupView,
    /// `Device::restore_hold`: the restore overlay shows.
    pub restoring: bool,
    /// Unused: the UI bridge fills Device Storage from its library and media lists.
    pub storage: StorageView,
    pub capture: CaptureView,
    /// Device Storage quarantine rows: read when the page opens and after a RESTORE.
    pub quarantine: Vec<QuarantineRow>,
    /// USB Statistics values of `usb_block` with the HID sent count at its arrival: stock
    /// reads the count (`+0x3f8`) in the block slot `0x68a90c`.
    pub usb_statistics: Option<[UsbValue; 21]>,
    /// The SOC2 ARM block `usb_statistics` was made from.
    pub usb_block: Option<Soc2ArmDiagnostics>,
    /// The screen at the last sync, to detect a page that just opened.
    pub screen: Screen,
    /// Directory cloud folders: CLOUD as listed, DOWNLOADS from `Device::downloads_rows`.
    pub cloud: std::sync::Arc<Vec<CloudFolderView>>,
    /// The `Services::cloud_changes` generation `cloud` and `downloads_cycle` were built from
    /// (`None`: not built yet).
    pub cloud_changes: Option<u64>,
    /// The DOWNLOADS capture rows stored on this unit, in that view's order, for BANK UP
    /// ([`super::super::actions::CaptureCycle::downloads`]).
    pub downloads_cycle: Vec<CycleCapture>,
    /// The preset name the upload overwrite dialog asks about (`Services::upload_question`).
    pub upload_overwrite: Option<String>,
    /// The cloud capture download that asks before it overwrites (`Services::capture_question`).
    pub capture_overwrite: Option<CaptureQuestion>,
    /// The capture upload that asks before it overwrites (`Services::capture_upload_question`).
    pub capture_upload_overwrite: Option<String>,
    /// The kind of the upload that runs (`Services::uploading`, review r31 D2, r36 H4).
    pub uploading: Option<&'static str>,
    /// The running batch upload (`Services::upload_progress`, re-r38 §2).
    pub upload_progress: Option<UploadProgress>,
    /// The waiting upload question belongs to a batch (`Services::upload_prompt`).
    pub upload_batch: bool,
}

/// Puts `value` in `slot` and says whether it differed from the old value.
fn replace_changed<T: PartialEq>(slot: &mut T, value: T) -> bool {
    let changed = *slot != value;
    *slot = value;
    changed
}

/// BANK UP rows of DOWNLOADS (`0x7657f8`): the stored captures in the view's order, each
/// keyed by its hash then its name (`0x214b84`, `0x6f7018`) with its record version.
fn cycle_captures(rows: &[DownloadsRow]) -> Vec<CycleCapture> {
    (rows.iter())
        .filter_map(|row| {
            Some(CycleCapture {
                value: format!("{}{}", row.hash.as_deref()?, row.name),
                version: row.version.clone(),
            })
        })
        .collect()
}

/// Lays the views over a projected snapshot. The running version stays the snapshot's.
pub fn apply(views: &SettingsViews, mut snapshot: UiSnapshot) -> UiSnapshot {
    let current_version = std::mem::take(&mut snapshot.updater.current_version);
    snapshot.wifi = views.wifi.clone();
    snapshot.extensions = views.extensions.clone();
    snapshot.mappings = views.mappings.clone();
    snapshot.appearance = views.appearance.clone();
    snapshot.remote = views.remote.clone();
    snapshot.boot_selector = views.boot_selector;
    snapshot.toast = views.toast.clone();
    snapshot.control_online = views.control_online;
    snapshot.control_link = views.control_link;
    snapshot.updater = UpdaterView {
        current_version,
        ..views.updater.clone()
    };
    snapshot.tetrad_update = views.tetrad_update.clone();
    snapshot.backups = views.backups.clone();
    snapshot.restoring = views.restoring;
    snapshot.storage = views.storage;
    snapshot.capture = views.capture.clone();
    snapshot.quarantine = views.quarantine.clone();
    snapshot.usb_statistics = views.usb_statistics.clone();
    snapshot.cloud = views.cloud.clone();
    snapshot.upload_overwrite = views.upload_overwrite.clone();
    snapshot.capture_overwrite = views.capture_overwrite.clone();
    snapshot.capture_upload_overwrite = views.capture_upload_overwrite.clone();
    snapshot.uploading = views.uploading;
    snapshot.upload_progress = views.upload_progress;
    snapshot.upload_batch = views.upload_batch;
    snapshot
}

/// V1 orchestrator state onto the wizard (manual §6). Diagram steps 1–4 are pages 01–04 and
/// mic setup is 05 REVIEW (I); refine states share the refine page (I). An active NC2 job (a
/// Cortex Control flow) shows its stock status line instead (`0xa378ac`).
pub fn capture_view(
    v1: Option<NcState>,
    nc2: Option<Nc2State>,
    input_levels: [f32; 2],
    progress: Option<f32>,
) -> CaptureView {
    use NcState as N;
    let (step, diagram_step) = match v1.unwrap_or(N::Inactive) {
        N::Inactive | N::Initial => (CaptureStep::Idle, 0),
        N::DiagramStep1 => (CaptureStep::ConnectionDiagram, 0),
        N::DiagramStep2 => (CaptureStep::ConnectionDiagram, 1),
        N::DiagramStep3 => (CaptureStep::ConnectionDiagram, 2),
        N::DiagramStep4 => (CaptureStep::ConnectionDiagram, 3),
        N::MicSetup => (CaptureStep::ConnectionDiagram, 4),
        N::Calibrate => (CaptureStep::Calibration, 0),
        N::Capturing => (CaptureStep::RecordingSignals, 0),
        N::SanityCheck => (CaptureStep::SanityCheck, 0),
        N::Training | N::AutoRefine => (CaptureStep::Training, 0),
        N::RefineCapture | N::ManualRefine => (CaptureStep::Refine, 0),
        N::RefineReady => (CaptureStep::Audition, 0),
        N::Exporting => (CaptureStep::Saving, 0),
        N::HandleError => (CaptureStep::Failed, 0),
    };
    let nc2_active = nc2.is_some_and(|state| !matches!(state, Nc2State::Idle | Nc2State::Abort));
    CaptureView {
        step,
        progress: progress.map(|value| value.clamp(0.0, 1.0)),
        input_levels: input_levels.map(|level| level.clamp(0.0, 1.0)),
        audition: match nc2 {
            Some(Nc2State::AuditionDut) => AuditionSource::Reference,
            _ => AuditionSource::Capture,
        },
        message: if nc2_active {
            "Neural Capture Version 2 in progress.".into()
        } else {
            String::new()
        },
        diagram_step,
        ..CaptureView::default()
    }
}

/// The NC2 dialog mode for the NC2 state (slot `0x3dcc2c`). States 1 and 2 check the
/// Cortex Control link (`0x6e0444`). State 0 leaves the layout as it is in stock; it shows
/// the open layout here (I). State 6 closes the dialog (`Device::sync_views`). A capture
/// save (`newCaptureSaved()` → `0x3d7fc8`) shows mode 3 until the next state change, which
/// `saved` (`Capture::nc2_saved`) already follows.
pub fn nc2_dialog(state: Nc2State, control_connected: bool, saved: bool) -> Nc2Dialog {
    match state {
        _ if saved => Nc2Dialog::Saved,
        Nc2State::Setup | Nc2State::Recording if !control_connected => Nc2Dialog::LostConnection,
        Nc2State::AuditionDut | Nc2State::AuditionCapture => Nc2Dialog::Ready,
        _ => Nc2Dialog::InProgress,
    }
}

impl<P: Platform> Device<P> {
    /// A backup restore owns the user data during the extract; the reload that follows a good
    /// one runs in the same step. The stock loading overlay shows (`0x3fba64`) and the device
    /// drops local input: touch, footswitches, knobs and MIDI. I: stock blocks the touches
    /// with the overlay; Tetrad also drops the hardware input, so no edit reaches the library
    /// while it is off the device (review r18 F3).
    pub(in crate::device) fn restore_hold(&self) -> bool {
        self.bridges.services.restoring()
    }

    /// Copies the bridge projections into `State::views` after a pass; a change requests a
    /// frame. Opening Connection starts a scan and opening Backups lists them (I: stock
    /// "Refreshing backups list" `0xa3aa24`); a failure becomes a notice.
    pub(in crate::device) fn sync_views(&mut self) {
        // Capture types load with the captures (`MediaLibrary::resolve`) during a deploy.
        if let Some(media) = &self.media
            && self.bridges.ui.set_capture_devices(media.capture_devices())
        {
            self.present = true;
        }
        let screen = self.state.screen;
        let storage = Screen::Settings(SettingsPage::DeviceStorage);
        let quarantine = (self.bridges.ui.take_quarantine_stale()
            || (screen == storage && self.state.views.screen != storage))
            .then(|| self.quarantine());
        // Theme files copied over while Tetrad runs show up when Appearance opens.
        let appearance_page = Screen::Settings(SettingsPage::Appearance);
        if screen == appearance_page && self.state.views.screen != appearance_page {
            self.read_theme_files();
        }
        if screen != self.state.views.screen {
            self.state.views.screen = screen;
            let services = self.platform.services();
            let opened = match screen {
                Screen::Settings(SettingsPage::Connection) => {
                    self.bridges.services.scan(services).map(|_| Vec::new())
                }
                Screen::Settings(SettingsPage::Backups) => self.bridges.services.refresh_backups(
                    services,
                    &mut self.state.settings,
                    self.state.now,
                ),
                _ => Ok(Vec::new()),
            };
            self.apply(opened);
        }
        // 3rd Party Licenses viewer (`0x6c9434`); a missing file shows the stock log text.
        if let Some(package) = self.bridges.ui.take_licence_request() {
            let path = format!("{LICENCE_DIR}{package}-license");
            let text = match self.platform.services().files().read(Path::new(&path)) {
                Ok(Some(raw)) => {
                    let end = raw.len().min(MAX_LICENCE_BYTES);
                    Some(String::from_utf8_lossy(&raw[..end]).into_owned())
                }
                _ => None,
            };
            self.bridges
                .ui
                .set_licence(tetrad_ui::LicenceView { package, text });
            self.present = true;
        }
        // "Corrupt Neural Capture" (`0x54663c`) for the captures the boot or restore check
        // removed (`Device::take_corrupt_captures`, `0x702f70`).
        let corrupt = self.take_corrupt_captures();
        if !corrupt.is_empty() {
            for effect in self.on_ui(super::UiInput::CorruptCaptures(corrupt)) {
                self.execute(effect);
            }
            self.present = true;
        }
        // State 6 hides the NC2 dialog (`0x3dcc2c`), and `hideDialog` (`0x3d7098`) closes NC2.
        if self.bridges.capture.nc2_dialog() == Some(Nc2State::Abort) {
            let closed = self.nc2_input(Nc2Input::Close).map(|()| Vec::new());
            self.apply(closed);
        }
        let mut capture = self.bridges.capture.view();
        let saved = self.bridges.capture.nc2_saved();
        capture.nc2 = (self.bridges.capture.nc2_dialog())
            .map(|state| nc2_dialog(state, self.state.control_connected, saved));
        let usb_block = self.state.diagnostics.soc2;
        let usb_statistics = (usb_block != self.state.views.usb_block).then(|| {
            let hid_sent = self.bridges.control.hid_reports_sent();
            (self.state.diagnostics.usb_statistics(hid_sent)).map(|rows| {
                rows.map(|row| UsbValue {
                    text: row.value,
                    tone: match row.tone {
                        info::UsbTone::Plain => UsbTone::Plain,
                        info::UsbTone::InRange => UsbTone::InRange,
                        info::UsbTone::OutOfRange => UsbTone::OutOfRange,
                        info::UsbTone::Plus => UsbTone::Plus,
                        info::UsbTone::Minus => UsbTone::Minus,
                    },
                })
            })
        });
        // r31-upload: each ended Directory preset upload shows its result dialog.
        for end in self.bridges.services.take_upload_ends() {
            self.bridges.ui.upload_end(end);
            self.present = true;
        }
        let ends = self.bridges.services.take_capture_download_ends();
        for end in ends {
            let stored = end.outcome == DownloadOutcome::Stored;
            self.bridges.ui.capture_download_end(end.hash, stored);
            self.present = true;
        }
        // The cloud views are built only when the Services cloud generation moves or a worker
        // read of the stored DOWNLOADS rows lands; else the old rows move over, with no copy
        // or compare (review r29 B3, A3).
        let landed = self.poll_stored_downloads();
        // The extensions view is built only after a step reached that bridge (each build
        // cost ~250 µs a pass on the QC); else the old view moves over.
        let extensions = take(&mut self.bridges.extensions.view_stale)
            .then(|| self.bridges.extensions.view(&self.state.community));
        // The Custom footswitch mode and its command list follow the extensions too.
        if extensions.is_some() {
            self.sync_custom();
        }
        // Extension themes follow the installed extensions.
        if extensions.is_some()
            && let Some(themes) = self.bridges.extensions.themes()
            && let Some(notice) = self.bridges.appearance.set_extensions(themes)
        {
            self.notice(super::super::Subsystem::Ui, notice);
        }
        let appearance =
            take(&mut self.bridges.appearance.view_stale).then(|| self.bridges.appearance.view());
        let declared = &self.bridges.extensions;
        let mappings =
            (self.bridges.mappings).view_if_changed(&self.state, || declared.declared_triggers());
        let changes = self.bridges.services.cloud_changes();
        let generation = self.state.views.cloud_changes != Some(changes);
        let rebuilt = (landed || generation).then(|| self.downloads_folders(&self.cloud_views()));
        let services = &self.bridges.services;
        // re-r38 §3: a question of a Cortex Control upload goes to Control only (`0x463d2c`);
        // review r39 K3: each question by the route of its own upload.
        let prompt = services.upload_prompt();
        let device_prompt = |kind: ProductKind| {
            (services.upload_prompts())
                .find(|question| !question.from_control && question.kind == kind)
        };
        // `Services::uploading` is `None` while a question waits; the dialog keeps its noun
        // and stays under the question (Z 951 under 952). A question before the first sync
        // names the batch by its kind (a preset walk asks about captures only after the
        // preset batch showed its noun).
        let old_noun = (self.state.views.upload_progress).map(|progress| progress.noun);
        let asked_noun = prompt.map(|question| match question.kind {
            ProductKind::Preset => "Presets",
            _ => "Neural Captures",
        });
        let upload_progress = (services.upload_progress())
            .zip(services.uploading().or(old_noun).or(asked_noun))
            .map(|((handled, total), noun)| UploadProgress {
                noun,
                handled,
                total,
            });
        let control = &self.bridges.control;
        let control_link = match (control.connected(), control.online()) {
            (_, true) => tetrad_ui::ControlLinkState::Online,
            (true, false) => tetrad_ui::ControlLinkState::Offline,
            (false, false) => tetrad_ui::ControlLinkState::Disconnected,
        };
        let restoring = self.restore_hold();
        let usb_shown = self.bridges.ui.shows_dsp_load();
        let wifi = services.wifi_view();
        let remote = &self.bridges.remote;
        // Each view is compared and replaced in its own field: a pass builds, moves and drops
        // no whole `SettingsViews`. The pattern has no `..`: a new field does not compile until
        // it is synced here too.
        let SettingsViews {
            wifi: _,
            extensions: _,
            mappings: _,
            appearance: _,
            remote: _,
            boot_selector: _,
            toast: _,
            control_online: _,
            control_link: _,
            updater: _,
            tetrad_update: _,
            backups: _,
            restoring: _,
            storage: _,
            capture: _,
            quarantine: _,
            usb_statistics: _,
            usb_block: _,
            screen: _,
            cloud: _,
            cloud_changes: _,
            downloads_cycle: _,
            upload_overwrite: _,
            capture_overwrite: _,
            capture_upload_overwrite: _,
            uploading: _,
            upload_progress: _,
            upload_batch: _,
        } = &self.state.views;
        let old = &mut self.state.views;
        let mut changed = false;
        changed |= replace_changed(&mut old.remote, remote.view(wifi.ip_address.as_deref()));
        changed |= replace_changed(&mut old.boot_selector, self.bridges.boot_selector.view());
        changed |= replace_changed(&mut old.toast, remote.toast());
        changed |= replace_changed(&mut old.wifi, wifi);
        changed |= replace_changed(&mut old.control_online, control.cloud_route().is_some());
        changed |= replace_changed(&mut old.control_link, control_link);
        changed |= replace_changed(&mut old.updater, services.updater_view());
        changed |= replace_changed(&mut old.tetrad_update, services.tetrad_update_view());
        changed |= replace_changed(&mut old.backups, services.backup_view());
        changed |= replace_changed(&mut old.restoring, restoring);
        changed |= replace_changed(&mut old.storage, StorageView::default());
        changed |= replace_changed(&mut old.capture, capture);
        changed |= replace_changed(&mut old.screen, screen);
        changed |= replace_changed(&mut old.cloud_changes, Some(changes));
        let upload_overwrite =
            device_prompt(ProductKind::Preset).map(|question| question.name.to_owned());
        changed |= replace_changed(&mut old.upload_overwrite, upload_overwrite);
        let capture_overwrite = services.capture_question().cloned();
        changed |= replace_changed(&mut old.capture_overwrite, capture_overwrite);
        let capture_upload_overwrite =
            device_prompt(ProductKind::NeuralCapture).map(|question| question.name.to_owned());
        changed |= replace_changed(&mut old.capture_upload_overwrite, capture_upload_overwrite);
        changed |= replace_changed(&mut old.uploading, services.uploading());
        changed |= replace_changed(&mut old.upload_progress, upload_progress);
        let upload_batch =
            (services.upload_prompts()).any(|question| !question.from_control && question.batch);
        changed |= replace_changed(&mut old.upload_batch, upload_batch);
        // A generation move always presents (O: with equal rows it costs one frame); a landed
        // read presents only when its rows differ.
        if let Some((cloud, cycle)) = rebuilt {
            changed |= generation || cloud != *old.cloud || cycle != old.downloads_cycle;
            old.cloud = Arc::new(cloud);
            old.downloads_cycle = cycle;
        }
        // Views and rows built again are compared; else the old ones stay, with no copy.
        if let Some(view) = extensions {
            changed |= replace_changed(&mut old.extensions, view);
        }
        if let Some(view) = mappings {
            changed |= replace_changed(&mut old.mappings, view);
        }
        if let Some(view) = appearance {
            changed |= replace_changed(&mut old.appearance, view);
        }
        if let Some(rows) = quarantine {
            changed |= replace_changed(&mut old.quarantine, rows);
        }
        // Only the USB Statistics page draws the rows (stock fills them in its own slot
        // `0x68a90c`). Elsewhere a new block costs no frame: its HID counters move with each
        // Cortex Control report, so the 1 Hz block requested a frame on the grid (r48).
        if let Some(values) = usb_statistics {
            changed |= replace_changed(&mut old.usb_statistics, values) && usb_shown;
        }
        changed |= replace_changed(&mut old.usb_block, usb_block) && usb_shown;
        self.present |= changed;
    }

    fn cloud_views(&self) -> Vec<CloudFolderView> {
        let services = &self.bridges.services;
        [ItemKind::Preset, ItemKind::Capture, ItemKind::Ir]
            .into_iter()
            .flat_map(|kind| [CloudScope::Cloud, CloudScope::Downloads].map(|scope| (kind, scope)))
            .filter_map(|(kind, scope)| {
                let folder = services.cloud_folder(product_kind(kind), browse_scope(scope))?;
                Some(CloudFolderView {
                    kind,
                    scope,
                    rows: (folder.rows.iter())
                        .map(|row| CloudItem {
                            id: row.id.clone(),
                            name: row.name.clone(),
                            author: row.author_username.clone(),
                            stored: false,
                        })
                        .collect(),
                    more: folder.more,
                    fetching: folder.fetching,
                    error: folder.error.clone(),
                })
            })
            .collect()
    }

    /// The cloud folders with DOWNLOADS of presets and captures (review r28 A1): the products
    /// stored on this unit, marked (as the last worker read gave them,
    /// `Device::poll_stored_downloads`), then the queue rows listed this run. A stored product
    /// is listed with no cloud listing this boot; the listing keeps its paging state. Also the
    /// BANK UP list of the stored captures.
    fn downloads_folders(
        &self,
        source: &[CloudFolderView],
    ) -> (Vec<CloudFolderView>, Vec<CycleCapture>) {
        let downloads = |folder: &CloudFolderView| {
            folder.scope == CloudScope::Downloads && folder.kind != ItemKind::Ir
        };
        let mut cloud: Vec<CloudFolderView> = (source.iter())
            .filter(|folder| !downloads(folder))
            .cloned()
            .collect();
        let mut cycle = Vec::new();
        let stored = self.bridges.services.stored_downloads.rows();
        for kind in [ItemKind::Preset, ItemKind::Capture] {
            let kind_rows = stored.of(product_kind(kind)).to_vec();
            let rows = self.with_queue_rows(kind_rows, product_kind(kind));
            if kind == ItemKind::Capture {
                cycle = cycle_captures(&rows);
            }
            let listed = (source.iter()).find(|folder| folder.kind == kind && downloads(folder));
            if listed.is_none() && rows.is_empty() {
                continue;
            }
            cloud.push(CloudFolderView {
                kind,
                scope: CloudScope::Downloads,
                rows: (rows.into_iter())
                    .map(|row| CloudItem {
                        id: row.id,
                        name: row.name,
                        author: row.author,
                        stored: row.stored,
                    })
                    .collect(),
                more: listed.is_some_and(|folder| folder.more),
                fetching: listed.is_some_and(|folder| folder.fetching),
                error: listed.and_then(|folder| folder.error.clone()),
            });
        }
        (cloud, cycle)
    }

    /// "SEE WHAT'S NEW" at boot (`0x54491c`): when the update marker `changelog_boot` exists
    /// (`0x4e4398`), the popup shows "CorOS: %1" with its first line (`0x4e5338`) and the
    /// rest of the file (`0x4e5770`, its `%1` = the product name `0x498ce4`); then the marker
    /// is removed and synced (`0x4e43b4`). Returns whether it showed: stock then skips the
    /// Excessive Downloads check this boot. I: the version line loses its line end. O: a
    /// bring-up run neither shows nor removes it (stock ZenUI owns that unit's marker).
    pub(in crate::device) fn show_update_notes(&mut self) -> bool {
        use tetrad_services::updater::paths::CHANGELOG_BOOT;
        if self.options.is_bringup() {
            return false;
        }
        let files = self.platform.services().files();
        let marker = Path::new(CHANGELOG_BOOT);
        let Ok(Some(raw)) = files.read_bounded(marker, MAX_LICENCE_BYTES as u64) else {
            return false;
        };
        let removed = files.remove(marker).and_then(|()| files.sync());
        let text = String::from_utf8_lossy(&raw);
        let (version, notes) = text.split_once('\n').unwrap_or((&text, ""));
        let product = match self.state.platform.is_atma() {
            true => "Quad Cortex mini",
            false => "Quad Cortex",
        };
        let input = super::UiInput::UpdateNotes {
            version: version.trim_end_matches('\r').to_owned(),
            notes: notes.replace("%1", product),
        };
        for effect in self.on_ui(input) {
            self.execute(effect);
        }
        if let Err(error) = removed {
            self.notice(
                super::super::Subsystem::Services,
                format!("changelog_boot: {error}"),
            );
        }
        self.present = true;
        true
    }

    /// The Tetrad quarantine table (O, review r23 K5); a read failure becomes a notice.
    fn quarantine(&mut self) -> Vec<QuarantineRow> {
        let Some(db) = (self.state.library.as_ref()).and_then(|library| library.database.as_ref())
        else {
            return Vec::new();
        };
        match db.view().quarantined_captures() {
            Ok(rows) => (rows.into_iter())
                .map(|row| QuarantineRow {
                    downloads: row.store == tetrad_storage::stock::CaptureStore::Downloads,
                    id: row.id,
                    reason: row.reason,
                })
                .collect(),
            Err(error) => {
                self.notice(super::super::Subsystem::Library, error.to_string());
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// r28-ui (r28-device note), review r28 A1: BANK UP walks the stored DOWNLOADS captures
    /// in the view's row order, keyed by the capture hash (`0x214b84`); a queue row that is
    /// not stored has no hash and is left out.
    #[test]
    fn downloads_cycle_keeps_the_view_order_and_maps_ids_to_stored_hashes() {
        let row = |id: &str, name: &str, version: &str, stored: bool| DownloadsRow {
            id: id.into(),
            name: name.into(),
            author: String::new(),
            version: version.into(),
            hash: stored.then(|| format!("hash-{id}-")),
            stored,
        };
        let rows = [
            row("c2", "Zed", "2", true),
            row("c1", "Amp", "1", true),
            row("c9", "Not Here", "1", false),
        ];
        let cycle = cycle_captures(&rows);
        let values: Vec<_> = (cycle.iter())
            .map(|row| (&*row.value, &*row.version))
            .collect();
        assert_eq!(values, [("hash-c2-Zed", "2"), ("hash-c1-Amp", "1")]);
        let walk = super::super::super::actions::CaptureCycle {
            library: Vec::new(),
            downloads: cycle,
        };
        let next = walk.step("hash-c1-Amp", "1", false);
        assert_eq!(next.map(|row| &*row.value), Ok("hash-c1-Amp"));
    }

    /// Manual §6 page order from the V1 orchestrator states; levels and progress are clamped
    /// for the view's 0..=1 contract.
    #[test]
    fn capture_states_map_onto_the_wizard_pages() {
        let view = |state| capture_view(Some(state), None, [0.2, 1.5], Some(-0.1));
        assert_eq!(view(NcState::Inactive).step, CaptureStep::Idle);
        let diagram = view(NcState::DiagramStep3);
        assert_eq!(
            (diagram.step, diagram.diagram_step),
            (CaptureStep::ConnectionDiagram, 2)
        );
        assert_eq!(view(NcState::MicSetup).diagram_step, 4);
        assert_eq!(view(NcState::Calibrate).step, CaptureStep::Calibration);
        assert_eq!(view(NcState::Capturing).step, CaptureStep::RecordingSignals);
        assert_eq!(view(NcState::RefineReady).step, CaptureStep::Audition);
        assert_eq!(view(NcState::HandleError).step, CaptureStep::Failed);
        let calibrate = view(NcState::Calibrate);
        assert_eq!(calibrate.input_levels, [0.2, 1.0]);
        assert_eq!(calibrate.progress, Some(0.0));
        let nc2 = capture_view(None, Some(Nc2State::Training), [0.0; 2], None);
        assert_eq!(nc2.message, "Neural Capture Version 2 in progress.");
    }

    /// r18-ui MR 2: a saved capture shows mode 3 (`0x3d7fc8`) over the state's own mode;
    /// without it the audition states keep the A/B dialog.
    #[test]
    fn a_saved_capture_shows_the_saved_dialog() {
        assert_eq!(
            nc2_dialog(Nc2State::AuditionCapture, true, true),
            Nc2Dialog::Saved
        );
        assert_eq!(
            nc2_dialog(Nc2State::AuditionCapture, true, false),
            Nc2Dialog::Ready
        );
    }
}

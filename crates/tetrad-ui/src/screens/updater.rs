//! CorOS Updates page (manual §10 System › Updates). Titles, texts and buttons follow the stock
//! page: state view `0x42522c`, status view `0x426034`, offline view `0x42501c`, buttons
//! `0x426e08`, download confirmation `0x42483c`. "%1" is the product name, "Quad Cortex".
//! Owned: the CorOS buttons name CorOS, so they differ from the Tetrad Updates page
//! ([`tetrad_layout`]), which updates Tetrad only.

use tetrad_assets::{ACCENT, PANEL, Rgb, TEXT};

use crate::layout::{ButtonStyle, LARGE, Layout, Look, MEDIUM};
use crate::render::Align;
use crate::settings::DeviceAction;
use crate::*;

/// `_INIT_334` (`0xa3e2f4`, `0xa3e314`); shown while installing (I: placement).
const DURING_UPDATE: &str = "DURING THE UPDATE:\n· The touchscreen and footswitches will be temporarily disabled.\n· Audio will be unavailable.\n· Do not remove the power cable or shut down your Quad Cortex.";

#[derive(Debug, PartialEq)]
pub(super) struct Page {
    pub title: &'static str,
    pub body: String,
    pub progress: Option<f32>,
    pub button: Option<(&'static str, Control)>,
    pub changelog: bool,
}

/// What the page shows for one updater projection.
pub(super) fn page(u: &UpdaterView) -> Page {
    let version = if u.current_version.is_empty() {
        "-"
    } else {
        u.current_version.as_str()
    };
    let check = Some(("CHECK FOR UPDATES", Control::Updater(UpdaterCommand::Check)));
    let page = |title, body: &str, button| Page {
        title,
        body: body.to_owned(),
        progress: None,
        button,
        changelog: false,
    };
    let offline = !u.online
        && u.status != UpdaterStatus::NewVersionDownloaded
        && u.state != UpdaterState::Reboot;
    if offline {
        return page(
            "No Internet connection",
            "Your Quad Cortex is currently offline. Connect to Wi-Fi\nand try again.",
            None,
        );
    }
    match (u.state, u.status) {
        (UpdaterState::Idle, UpdaterStatus::NewVersionDownloaded) => page(
            "Update ready",
            "The latest update is ready to install.\nWe recommend you backup your Quad Cortex before\ninstalling.",
            Some(("UPDATE COROS", Control::Updater(UpdaterCommand::Install))),
        ),
        (UpdaterState::Idle, UpdaterStatus::NoNewVersion) => page(
            "Up to date",
            "Your Quad Cortex is running the most recent version of CorOS, no updates are available",
            check,
        ),
        (UpdaterState::Idle, status) => {
            let mut body = format!("Your Quad Cortex is currently running\nCorOS: {version}");
            let available = status == UpdaterStatus::NewVersionAvailable;
            if available {
                let next = u.available_version.as_deref().unwrap_or("-");
                body.push_str(&format!("\n\nA new version is available:\nCorOS: {next}"));
            }
            Page {
                changelog: available,
                ..page(
                    "CorOS Updates",
                    &body,
                    if available {
                        Some((
                            "DOWNLOAD COROS",
                            Control::Updater(UpdaterCommand::CheckAndDownload),
                        ))
                    } else {
                        check
                    },
                )
            }
        }
        (UpdaterState::RequestingData, _) => {
            page("Checking for updates", "Checking for new updates...", None)
        }
        (UpdaterState::DownloadingData, _) => Page {
            progress: Some(u.progress.unwrap_or(0.0)),
            ..page(
                "Downloading latest update",
                "Downloading an update while performing is not\nrecommended. Make sure you have enough time to\ncomplete the update.",
                Some(("CANCEL", Control::Updater(UpdaterCommand::CancelDownload))),
            )
        },
        (UpdaterState::Updating, _) => Page {
            progress: Some(u.progress.unwrap_or(0.0)),
            ..page("Update in progress", DURING_UPDATE, None)
        },
        (UpdaterState::Reboot, _) => page(
            "Please restart your Quad Cortex",
            "Your Quad Cortex needs to be restarted to finalize updates.\nPlease restart your device as soon as possible to finish the\ninstallation.",
            Some(("REBOOT", Control::Action(DeviceAction::Reboot))),
        ),
        (UpdaterState::Failed, _) => page(
            "The update procedure has failed.",
            "Please try again. If the problem persists, please contact support@neuraldsp.com",
            check,
        ),
    }
}

/// The whole page panel: the state title replaces the page name, as in stock. Settings
/// frame look (settings spec §1): 20 px regular title, white help text, inset buttons.
pub(super) fn layout(ui: &UiState, l: &mut Layout, pane: Rect) {
    let page = page(&ui.snapshot.updater);
    let (x, y) = draw(l, pane, &page, ButtonStyle::Inset);
    if page.changelog {
        l.button(
            Rect::new(x, y, 219, 45),
            Control::ShowChangelog,
            !ui.snapshot.updater.changelog.is_empty(),
            "SEE WHAT'S NEW",
            ButtonStyle::Inset,
        );
    }
}

/// Title, text, progress and button of `page`. Returns where a second button goes.
fn draw(l: &mut Layout, pane: Rect, page: &Page, style: ButtonStyle) -> (i32, i32) {
    l.text(
        Rect::new(pane.x, pane.y, pane.width, 32),
        page.title,
        LARGE,
        TEXT,
        Align::Left,
    );
    l.paragraph(
        Rect::new(pane.x, pane.y + 44, pane.width, 150),
        page.body.as_str(),
        MEDIUM,
        TEXT,
    );
    let mut y = pane.y + 200;
    if let Some(fraction) = page.progress {
        l.text(
            Rect::new(pane.x, y, pane.width, 24),
            format!("{:.0}%", fraction * 100.0),
            MEDIUM,
            TEXT,
            Align::Left,
        );
        l.push(
            Rect::new(pane.x, y + 30, pane.width, 16),
            "",
            Look::Progress {
                fraction,
                color: ACCENT,
            },
        );
        y += 60;
    }
    let y = (pane.bottom() - 45).max(y);
    if let Some((label, control)) = page.button {
        l.button(Rect::new(pane.x, y, 219, 45), control, true, label, style);
    }
    (pane.x + 235, y)
}

/// Owned (Tetrad): what the Tetrad Updates page shows. It names Tetrad in every title and
/// button, and says that CorOS does not change.
pub(super) fn tetrad_page(u: &TetradUpdateView) -> Page {
    use TetradUpdateState as S;
    let current = if u.current_version.is_empty() {
        "-"
    } else {
        u.current_version.as_str()
    };
    let check = Some((
        "CHECK TETRAD",
        Control::TetradUpdate(TetradUpdateCommand::Check),
    ));
    let install = Some((
        "UPDATE TETRAD",
        Control::TetradUpdate(TetradUpdateCommand::Install),
    ));
    let page = |title, body: String, button| Page {
        title,
        body,
        progress: None,
        button,
        changelog: false,
    };
    let installing = matches!(u.state, S::Installing { .. });
    if !u.online && !installing {
        return page(
            "No Internet connection",
            "Your Quad Cortex is currently offline. Connect to Wi-Fi\nand try again.".into(),
            None,
        );
    }
    let only_tetrad = "A Tetrad update changes only Tetrad. CorOS does not change.";
    match &u.state {
        S::Idle => page(
            "Tetrad Updates",
            format!("Your Quad Cortex is currently running\nTetrad: {current}\n\n{only_tetrad}"),
            check,
        ),
        S::Checking => page(
            "Checking for Tetrad updates",
            "Checking for a new Tetrad release...".into(),
            None,
        ),
        S::UpToDate => page(
            "Tetrad is up to date",
            format!("Tetrad {current} is the newest release.\n\n{only_tetrad}"),
            check,
        ),
        S::Available { version } => page(
            "Tetrad update available",
            format!(
                "Tetrad: {current} → {version}\n\nTetrad downloads the update, checks it and restarts\nyour Quad Cortex. CorOS does not change."
            ),
            install,
        ),
        S::Installing { version, progress } => Page {
            progress: Some(progress.unwrap_or(0.0)),
            ..page(
                "Updating Tetrad",
                format!(
                    "Installing Tetrad {version}. Do not turn off your\nQuad Cortex. It restarts when the update is installed."
                ),
                None,
            )
        },
        S::Failed { error, retry } => page(
            "The Tetrad update has failed.",
            format!("{error}\n\nPlease try again."),
            if retry.is_some() { install } else { check },
        ),
    }
}

/// Owned (Tetrad): the Tetrad Updates page. Its button is the blue primary button; the CorOS
/// page keeps the stock black buttons.
pub(super) fn tetrad_layout(ui: &UiState, l: &mut Layout, pane: Rect) {
    draw(
        l,
        pane,
        &tetrad_page(&ui.snapshot.tetrad_update),
        ButtonStyle::Primary,
    );
}

/// Stock "Download update" title colour (`#FE5948`, string table).
const CONFIRM_TITLE: Rgb = Rgb(0xfe, 0x59, 0x48);

/// "Download update" (`0x42483c`): asked only off the official channel.
pub(super) fn download_confirm(ui: &UiState, l: &mut Layout) {
    let body = match &ui.snapshot.updater.channel {
        UpdateChannel::Suffix(suffix) => {
            format!("You have set updater suffix to {suffix}. This will download unofficial update")
        }
        _ => "You are in a beta tester group. This will download a beta update".to_owned(),
    };
    confirm_panel(l, "Download update", body, "PROCEED");
}

/// Owned (Tetrad): UPDATE of a CorOS version without a Tetrad profile. The update keeps the
/// Tetrad boot hook, but stock CorOS starts until Tetrad supports the version.
pub(super) fn install_confirm(ui: &UiState, l: &mut Layout) {
    let version = match ui.snapshot.updater.available_version.as_deref() {
        Some(version) => format!("CorOS {version}"),
        None => "this CorOS version".to_owned(),
    };
    let body = format!(
        "Tetrad does not support {version} yet. The update keeps Tetrad, but the Quad Cortex \
         starts stock CorOS until Tetrad supports {version}."
    );
    confirm_panel(l, "Install update", body, "INSTALL");
}

/// The panel of the updater questions: title, text, CANCEL and the `primary` answer.
fn confirm_panel(l: &mut Layout, title: &'static str, body: String, primary: &'static str) {
    let panel = Rect::new(160, 120, 480, 240);
    l.panel(panel, PANEL, 8.0, None);
    l.text(
        Rect::new(panel.x + 24, panel.y + 16, panel.width - 48, 28),
        title,
        LARGE,
        CONFIRM_TITLE,
        Align::Left,
    );
    l.paragraph(
        Rect::new(panel.x + 24, panel.y + 60, panel.width - 48, 90),
        body,
        MEDIUM,
        TEXT,
    );
    l.button(
        Rect::new(panel.x + 24, panel.bottom() - 68, 200, 45),
        Control::Dialog(DialogButton::Cancel),
        true,
        "CANCEL",
        ButtonStyle::Inset,
    );
    l.button(
        Rect::new(panel.right() - 224, panel.bottom() - 68, 200, 45),
        Control::Dialog(DialogButton::Primary),
        true,
        primary,
        ButtonStyle::Primary,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Confirm, Overlay};

    fn view(state: UpdaterState, status: UpdaterStatus, online: bool) -> UpdaterView {
        UpdaterView {
            current_version: "4.1.0".into(),
            available_version: Some("4.2.0".into()),
            state,
            status,
            online,
            ..UpdaterView::default()
        }
    }

    /// Stock `0x42522c`: offline shows the offline page, except with a downloaded update
    /// or a reboot due; every state has its stock title and button.
    #[test]
    fn page_follows_stock_state_and_status() {
        use UpdaterState as S;
        use UpdaterStatus as T;
        let title = |state, status, online| page(&view(state, status, online)).title;
        assert_eq!(title(S::Idle, T::None, false), "No Internet connection");
        assert_eq!(
            title(S::Idle, T::NewVersionDownloaded, false),
            "Update ready"
        );
        assert_eq!(
            title(S::Reboot, T::None, false),
            "Please restart your Quad Cortex"
        );
        assert_eq!(title(S::Idle, T::NoNewVersion, true), "Up to date");
        assert_eq!(
            title(S::RequestingData, T::None, true),
            "Checking for updates"
        );
        assert_eq!(
            title(S::Failed, T::None, true),
            "The update procedure has failed."
        );
        let available = page(&view(S::Idle, T::NewVersionAvailable, true));
        assert_eq!(available.title, "CorOS Updates");
        assert!(available.body.contains("CorOS: 4.2.0") && available.changelog);
        assert_eq!(
            available.button,
            Some((
                "DOWNLOAD COROS",
                Control::Updater(UpdaterCommand::CheckAndDownload)
            ))
        );
        let downloading = page(&view(S::DownloadingData, T::NewVersionAvailable, true));
        assert_eq!(downloading.progress, Some(0.0));
        assert_eq!(downloading.button.map(|(label, _)| label), Some("CANCEL"));
        assert_eq!(
            page(&view(S::Idle, T::NewVersionDownloaded, true)).button,
            Some(("UPDATE COROS", Control::Updater(UpdaterCommand::Install)))
        );
    }

    /// The Tetrad page names Tetrad in every title and button, so it never reads as the CorOS
    /// page; an offline unit still shows a running install.
    #[test]
    fn tetrad_page_names_tetrad_not_coros() {
        use TetradUpdateState as S;
        let view = |state, online| TetradUpdateView {
            current_version: "0.1.0".into(),
            state,
            online,
        };
        let installing = S::Installing {
            version: "0.2.0".into(),
            progress: Some(0.5),
        };
        let states = [
            S::Idle,
            S::UpToDate,
            S::Available {
                version: "0.2.0".into(),
            },
            installing.clone(),
            S::Failed {
                error: "HTTP 404".into(),
                retry: Some("0.2.0".into()),
            },
        ];
        for state in states {
            let page = tetrad_page(&view(state.clone(), true));
            assert!(page.title.contains("Tetrad"), "{state:?}");
            if let Some((label, _)) = page.button {
                assert!(label.ends_with("TETRAD"), "{state:?}");
            }
            let coros = page.button.map(|(_, c)| matches!(c, Control::Updater(_)));
            assert_ne!(coros, Some(true), "{state:?}");
        }
        let available = tetrad_page(&view(
            S::Available {
                version: "0.2.0".into(),
            },
            true,
        ));
        assert_eq!(
            available.button,
            Some((
                "UPDATE TETRAD",
                Control::TetradUpdate(TetradUpdateCommand::Install)
            ))
        );
        assert_eq!(tetrad_page(&view(installing, false)).progress, Some(0.5));
        assert_eq!(
            tetrad_page(&view(S::Idle, false)).title,
            "No Internet connection"
        );
    }

    /// `0x42483c`: DOWNLOAD UPDATE asks first only off the official channel; PROCEED sends
    /// the download, CANCEL nothing.
    #[test]
    fn download_asks_only_off_the_official_channel() {
        let download = Control::Updater(UpdaterCommand::CheckAndDownload);
        let request = vec![UiCommand::Updater {
            command: UpdaterCommand::CheckAndDownload,
        }];
        let open = |channel| {
            let mut ui = UiState::new();
            let mut snapshot = ui.snapshot().clone();
            snapshot.updater = UpdaterView {
                channel,
                ..view(UpdaterState::Idle, UpdaterStatus::NewVersionAvailable, true)
            };
            ui.set_snapshot(snapshot).unwrap();
            ui.navigate(Screen::Settings(crate::SettingsPage::Updates));
            ui
        };
        let mut ui = open(UpdateChannel::Official);
        assert_eq!(ui.handle_input(InputEvent::Activate(download)), request);
        let mut ui = open(UpdateChannel::Suffix("rc".into()));
        assert!(ui.handle_input(InputEvent::Activate(download)).is_empty());
        let cancel = Control::Dialog(DialogButton::Cancel);
        assert!(ui.handle_input(InputEvent::Activate(cancel)).is_empty());
        ui.handle_input(InputEvent::Activate(download));
        let proceed = Control::Dialog(DialogButton::Primary);
        assert_eq!(ui.handle_input(InputEvent::Activate(proceed)), request);
    }

    /// The Tetrad version guard: UPDATE installs at once for a version Tetrad supports. For
    /// another version it asks first; CANCEL closes the question and sends nothing, INSTALL
    /// sends the install.
    #[test]
    fn update_asks_first_only_for_a_version_tetrad_does_not_support() {
        let update = Control::Updater(UpdaterCommand::Install);
        let request = vec![UiCommand::Updater {
            command: UpdaterCommand::Install,
        }];
        let open = |stock_after_install| {
            let mut ui = UiState::new();
            let mut snapshot = ui.snapshot().clone();
            snapshot.updater = UpdaterView {
                stock_after_install,
                ..view(
                    UpdaterState::Idle,
                    UpdaterStatus::NewVersionDownloaded,
                    true,
                )
            };
            ui.set_snapshot(snapshot).unwrap();
            ui.navigate(Screen::Settings(crate::SettingsPage::Updates));
            ui
        };
        let mut ui = open(false);
        assert_eq!(ui.handle_input(InputEvent::Activate(update)), request);
        assert!(ui.overlay.is_none());

        let mut ui = open(true);
        assert!(ui.handle_input(InputEvent::Activate(update)).is_empty());
        assert_eq!(ui.overlay, Some(Overlay::Confirm(Confirm::InstallUpdate)));
        let cancel = Control::Dialog(DialogButton::Cancel);
        assert!(ui.handle_input(InputEvent::Activate(cancel)).is_empty());
        assert!(ui.overlay.is_none());
        ui.handle_input(InputEvent::Activate(update));
        let install = Control::Dialog(DialogButton::Primary);
        assert_eq!(ui.handle_input(InputEvent::Activate(install)), request);
        assert!(ui.overlay.is_none());
    }
}

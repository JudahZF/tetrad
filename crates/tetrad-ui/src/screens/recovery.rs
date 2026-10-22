//! Recovery: the Factory Reset page body, the boot Recovery screen that reuses it (manual
//! §12, A + H held at startup; layout I), the locked reset progress screen and the backup
//! restore overlay. Stock strings `0xa3cfe8`..`0xa3d384` with %1 = Quad Cortex.

use crate::layout::{BODY, ButtonStyle, LARGE, LOADING_VEIL, Layout, Look, MEDIUM, MEDIUM_BOLD};
use crate::render::{Align, HEIGHT, WIDTH};
use crate::settings::DeviceAction;
use crate::*;

const HEADING: &str = "Choose a Recovery option";
const OPTIONS: [(DeviceAction, &str, ButtonStyle, &str); 2] = [
    (
        DeviceAction::ResetSettings,
        "RESET SETTINGS",
        ButtonStyle::Secondary,
        "Reset Settings will restore Quad Cortex to its default\nsettings. User data (Presets, Captures, etc.) will not be\nremoved.",
    ),
    (
        DeviceAction::FactoryReset,
        "FACTORY RESET",
        ButtonStyle::Danger,
        "Factory Reset will remove all user data and the device will\nbe restored to factory settings.",
    ),
];

/// "Choose a Recovery option" and one button + description block per option. Both
/// buttons open the stock confirm popup; the factory one then locks the screen.
pub(super) fn options(l: &mut Layout, area: Rect, centred: bool) {
    let p = l.palette;
    let align = if centred { Align::Center } else { Align::Left };
    l.text(
        Rect::new(area.x, area.y, area.width, 28),
        HEADING,
        if centred { LARGE } else { MEDIUM },
        p.text,
        align,
    );
    for (index, (action, label, style, text)) in OPTIONS.into_iter().enumerate() {
        let top = area.y + 48 + index as i32 * 134;
        let button_x = if centred {
            area.x + (area.width as i32 - 220) / 2
        } else {
            area.x
        };
        l.button(
            Rect::new(button_x, top, 220, 44),
            Control::Action(action),
            true,
            label,
            style,
        );
        l.push(
            Rect::new(area.x, top + 54, area.width, 66),
            text,
            Look::Paragraph {
                font: BODY,
                color: p.muted,
                align,
            },
        );
    }
}

/// Boot Recovery screen: full screen, no settings frame; CANCEL boots normally.
pub(super) fn layout(_ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(p.canvas));
    options(l, Rect::new(120, 32, 560, 310), true);
    l.push(
        Rect::new(120, 352, 560, 24),
        "Reset actions are permanent.",
        Look::Paragraph {
            font: BODY,
            color: p.muted,
            align: Align::Center,
        },
    );
    l.button(
        Rect::new((WIDTH as i32 - 220) / 2, 392, 220, 44),
        Control::Action(DeviceAction::ContinueBoot),
        true,
        "CANCEL",
        ButtonStyle::Inset,
    );
}

/// Locked progress screen after a confirmed factory reset: no controls, Back ignored.
pub(super) fn locked(l: &mut Layout) {
    let p = l.palette;
    l.overlay();
    l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(p.canvas));
    l.text(
        Rect::new(40, 180, WIDTH - 80, 32),
        "Resetting Quad Cortex to factory settings",
        LARGE,
        p.text,
        Align::Center,
    );
    l.push(
        Rect::new(120, 228, WIDTH - 240, 60),
        "This process can take up to a few minutes, during which time the device will be locked.",
        Look::Paragraph {
            font: BODY,
            color: p.muted,
            align: Align::Center,
        },
    );
}

/// The stock loading overlay as the Control and cloud restores show it (`0x17d104` →
/// `0x3fba64(0)`): no Cancel (`0x81bea4`). Tetrad has no transfer progress, so the bar stays at
/// the stock start value 0 (I).
pub(super) fn restoring(l: &mut Layout) {
    progress_dialog(l, "Downloading a backup, please wait.", 0.0, None);
}

/// The stock ProgressDialog (`0x81b74c`, Z 951) over a `#323232` veil at opacity 0.7: a
/// `#121212` panel, radius 10, at (178, 171), 443 × 130, or 443 × 190 with its Cancel button
/// (`0x81be2c`: 196 × 44 at (124, 120), `#2e2e2e`, radius 4); the text 18 px IBM Plex Sans
/// Medium at (0, 32) 443 × 24, centred; the bar 336 × 8 at (54, 90), radius 4, `#45f862`
/// filled to `progress` (`0x81bcf4`) on `#252525`.
pub(super) fn progress_dialog(l: &mut Layout, text: &str, progress: f32, cancel: Option<Control>) {
    let p = l.palette;
    l.veil((p.tone(LOADING_VEIL.0), LOADING_VEIL.1));
    let height = if cancel.is_some() { 190 } else { 130 };
    let panel = Rect::new(178, 171, 443, height);
    let panel_look = Look::Panel {
        fill: p.background,
        radius: 10.0,
        border: None,
    };
    l.push(panel, "", panel_look);
    l.text(
        Rect::new(panel.x, panel.y + 32, panel.width, 24),
        text,
        MEDIUM_BOLD,
        p.text,
        Align::Center,
    );
    let track = Look::Panel {
        fill: p.row_selected,
        radius: 4.0,
        border: None,
    };
    l.push(Rect::new(panel.x + 54, panel.y + 90, 336, 8), "", track);
    let filled = (336.0 * progress.clamp(0.0, 1.0)).round() as u32;
    if filled > 0 {
        l.panel(
            Rect::new(panel.x + 54, panel.y + 90, filled, 8),
            p.accent,
            4.0,
            None,
        );
    }
    if let Some(control) = cancel {
        let button = Rect::new(panel.x + 124, panel.y + 120, 196, 44);
        l.panel(button, p.panel, 4.0, None);
        l.control(button, control, true, "Cancel", Look::Area);
        l.text(button, "Cancel", BODY, p.text, Align::Center);
    }
}

//! Owned Remote page (Tetrad; stock has none, docs/remote.md): the switches (Remote, and its
//! PIN in the ⋮ menu), a backup that the page sent with RESTORE and DISCARD, then the address
//! and PIN a browser needs while Remote runs, and a QR code that opens the page with the PIN.
//! Rows follow the generic settings rows (60 px with a divider); the page scrolls as they do.

use super::extensions::{divider, label, switch_row};
use super::settings::rich_height;
use crate::layout::{ButtonStyle, CAPTION, LARGE, Layout, ListId, Look, MEDIUM};
use crate::render::Align;
use crate::*;
use qrcodegen::{QrCode, QrCodeEcc};
use tetrad_assets::Rgb;

const ROW: u32 = 60;
/// Pixels per QR module, and the light border around the code (4 modules, as the standard asks).
const QR_SCALE: u32 = 4;
const QR_QUIET: u32 = 4;
pub(super) const HELP: &str = "Scan the code, or open the address in a browser on the same \
    Wi-Fi network and enter the PIN. The page manages IRs, mappings, footswitch layouts, backups, \
    themes and extensions. Plain HTTP: turn Remote off on networks you do not trust.";
/// Send Report (owned; stock uploads the logs, Tetrad never does): the support file
/// (docs/support.md) comes from the Remote page instead.
pub(super) const SEND_REPORT_HELP: &str = "For a bug report, open the address in a browser on \
    the same Wi-Fi network, enter the PIN and tap Support file at the top. Send that file \
    with your report. It holds no passwords, tokens or presets.";

/// `body` is the settings content under the page title; `help` follows the rows. The Remote
/// page also shows the Show PIN in menu switch and the last upload status; Send Report (the
/// support file is downloaded here) does not. While Remote runs on Wi-Fi, the address and the
/// PIN share one block with the QR code at its right.
pub(super) fn layout(ui: &UiState, l: &mut Layout, body: Rect, help: &str, remote_page: bool) {
    let p = l.palette;
    let view = &ui.snapshot.remote;
    let code = scan_link(view).and_then(|link| QrCode::encode_text(&link, QrCodeEcc::Low).ok());
    let mut rows = Vec::new();
    if view.enabled && code.is_none() {
        let address = view.address.as_deref().unwrap_or("Connect to Wi-Fi first");
        rows.push(("Address", address, MEDIUM));
        if let Some(pin) = &view.pin {
            rows.push(("PIN", pin.as_str(), LARGE));
        }
    }
    if let Some(status) = view.status.as_ref().filter(|_| remote_page) {
        rows.push(("Status", status.as_str(), MEDIUM));
    }
    let code_side =
        (code.as_ref()).map_or(0, |code| (code.size() as u32 + 2 * QR_QUIET) * QR_SCALE);
    let code_block = if code.is_some() { code_side + 32 } else { 0 };
    // Stock rows run 8 px past the text inset (`screens::settings`).
    let area = Rect::new(body.x, body.y, body.width + 8, body.height);
    let help_width = body.width - 16;
    let restore = view.restore.as_ref().filter(|_| remote_page);
    let top_rows = if remote_page { 2 } else { 1 } + u32::from(restore.is_some());
    let content = (top_rows + rows.len() as u32) * ROW
        + code_block
        + 16
        + rich_height(help, MEDIUM, help_width);
    let offset = ui.offset(ListId::SettingsPage);
    l.scroll(ListId::SettingsPage, area, content, offset, |l, top| {
        let mut row = Rect::new(area.x, top, area.width, ROW);
        l.control(row, Control::RemoteToggle, true, "Remote", Look::Area);
        switch_row(l, row, "Remote", Some(view.enabled));
        if remote_page {
            row.y += ROW as i32;
            let menu = "Show PIN in menu";
            l.control(row, Control::RemotePinInMenuToggle, true, menu, Look::Area);
            switch_row(l, row, menu, Some(view.pin_in_menu));
        }
        if let Some(backup) = restore {
            row.y += ROW as i32;
            restore_row(l, row, backup);
        }
        if let (Some(code), Some(address), Some(pin)) = (&code, &view.address, &view.pin) {
            let block = Rect::new(row.x, row.bottom(), row.width, code_block);
            let code_x = block.right() - 16 - code_side as i32;
            let value_x = block.x + 120;
            let width = (code_x - 16 - value_x).max(0) as u32;
            let pairs = [
                ("Address", address.as_str(), MEDIUM),
                ("PIN", pin.as_str(), LARGE),
            ];
            // Three lines of 48 px beside the code: the address, the PIN and the hint.
            let line = |index: i32| Rect::new(block.x, block.y + 12 + index * 48, block.width, 48);
            for (index, (name, value, font)) in pairs.into_iter().enumerate() {
                let line = line(index as i32);
                label(l, line, value_x, name);
                let value_box = Rect::new(value_x, line.y, width, line.height);
                l.text(value_box, value, font, p.text, Align::Left);
            }
            let hint = line(2);
            let hint = Rect::new(
                hint.x + 16,
                hint.y,
                (code_x - 32 - block.x).max(0) as u32,
                48,
            );
            let text = "Scan to open the page with the PIN.";
            l.text(hint, text, MEDIUM, p.muted, Align::Left);
            qr(l, code, code_x, block.y + 16, code_side);
            divider(l, block);
            row = block;
        }
        for (name, value, font) in rows {
            row.y = row.bottom();
            row.height = ROW;
            let value_x = row.x + row.width as i32 / 3;
            label(l, row, value_x, name);
            l.text(
                Rect::new(
                    value_x,
                    row.y,
                    (row.right() - 16 - value_x).max(0) as u32,
                    ROW,
                ),
                value,
                font,
                p.text,
                Align::Right,
            );
            divider(l, row);
        }
        let top = row.bottom() + 16;
        l.paragraph(
            Rect::new(body.x + 16, top, help_width, content),
            help,
            MEDIUM,
            p.muted,
        );
    });
}

/// The backup that the page sent: its name over its size, then DISCARD and RESTORE, or the
/// restore that runs.
fn restore_row(l: &mut Layout, row: Rect, backup: &RemoteRestore) {
    let p = l.palette;
    let buttons = row.right() - 16 - 2 * 120 - 8;
    let text = Rect::new(
        row.x + 16,
        row.y + 8,
        (buttons - 16 - row.x - 16).max(0) as u32,
        26,
    );
    l.text(text, backup.name.as_str(), MEDIUM, p.text, Align::Left);
    let size = Rect::new(text.x, text.bottom(), text.width, 20);
    l.text(size, backup.size.as_str(), CAPTION, p.muted, Align::Left);
    let button = |index: i32| Rect::new(buttons + index * 128, row.y + 8, 120, 44);
    if backup.restoring {
        let state = Rect::new(buttons, row.y, (row.right() - 16 - buttons) as u32, ROW);
        l.text(state, "Restoring…", MEDIUM, p.muted, Align::Right);
    } else {
        let discard = Control::RemoteDiscard;
        l.button(button(0), discard, true, "DISCARD", ButtonStyle::Inset);
        let restore = Control::RemoteRestore;
        l.button(button(1), restore, true, "RESTORE", ButtonStyle::Primary);
    }
    divider(l, row);
}

/// The page with the PIN after the `#`, for the QR code: the page reads the PIN there, and a
/// browser does not send that part of the address to the unit.
pub(crate) fn scan_link(view: &RemoteView) -> Option<String> {
    let (address, pin) = (view.address.as_deref()?, view.pin.as_deref()?);
    Some(format!("{address}/#pin={pin}"))
}

/// Dark modules on a white square of `side` pixels: one fill per run of dark modules in a row.
/// A QR code needs dark on light whatever the theme, so these are fixed colours.
fn qr(l: &mut Layout, code: &QrCode, x: i32, y: i32, side: u32) {
    l.push(
        Rect::new(x, y, side, side),
        "",
        Look::Fill(Rgb(255, 255, 255)),
    );
    let origin = |modules: i32| (QR_QUIET as i32 + modules) * QR_SCALE as i32;
    for row in 0..code.size() {
        let mut column = 0;
        while column < code.size() {
            if !code.get_module(column, row) {
                column += 1;
                continue;
            }
            let start = column;
            while column < code.size() && code.get_module(column, row) {
                column += 1;
            }
            let run = Rect::new(
                x + origin(start),
                y + origin(row),
                (column - start) as u32 * QR_SCALE,
                QR_SCALE,
            );
            l.push(run, "", Look::Fill(Rgb(0, 0, 0)));
        }
    }
}

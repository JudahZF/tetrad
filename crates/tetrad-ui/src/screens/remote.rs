//! Owned Remote page (Tetrad; stock has none, docs/remote.md): the switches (Remote, and its
//! PIN in the ⋮ menu), then the address and PIN a browser needs while Remote runs. Rows follow
//! the generic settings rows (60 px with a divider); the page scrolls as they do.

use super::extensions::{divider, label, switch_row};
use super::settings::rich_height;
use crate::layout::{LARGE, Layout, ListId, Look, MEDIUM};
use crate::render::Align;
use crate::*;

const ROW: u32 = 60;
const HELP: &str = "Open the address in a browser on the same Wi-Fi network, enter the PIN and \
    drop IR files on the page. Plain HTTP: turn Remote off on networks you do not trust.";

/// `body` is the settings content under the page title.
pub(super) fn layout(ui: &UiState, l: &mut Layout, body: Rect) {
    let p = l.palette;
    let view = &ui.snapshot.remote;
    let mut rows = Vec::new();
    if view.enabled {
        let address = view.address.as_deref().unwrap_or("Connect to Wi-Fi first");
        rows.push(("Address", address, MEDIUM));
        if let Some(pin) = &view.pin {
            rows.push(("PIN", pin.as_str(), LARGE));
        }
    }
    if let Some(status) = &view.status {
        rows.push(("Status", status.as_str(), MEDIUM));
    }
    // Stock rows run 8 px past the text inset (`screens::settings`).
    let area = Rect::new(body.x, body.y, body.width + 8, body.height);
    let help_width = body.width - 16;
    let content = (2 + rows.len() as u32) * ROW + 16 + rich_height(HELP, MEDIUM, help_width);
    let offset = ui.offset(ListId::SettingsPage);
    l.scroll(ListId::SettingsPage, area, content, offset, |l, top| {
        let mut row = Rect::new(area.x, top, area.width, ROW);
        l.control(row, Control::RemoteToggle, true, "Remote", Look::Area);
        switch_row(l, row, "Remote", Some(view.enabled));
        row.y += ROW as i32;
        let menu = "Show PIN in menu";
        l.control(row, Control::RemotePinInMenuToggle, true, menu, Look::Area);
        switch_row(l, row, menu, Some(view.pin_in_menu));
        for (name, value, font) in rows {
            row.y += ROW as i32;
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
        let help = row.bottom() + 16;
        l.paragraph(
            Rect::new(body.x + 16, help, help_width, content),
            HELP,
            MEDIUM,
            p.muted,
        );
    });
}

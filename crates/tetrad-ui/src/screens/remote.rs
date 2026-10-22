//! Owned Remote page (Tetrad; stock has none, docs/remote.md): the switch, then the address
//! and PIN a browser needs while Remote runs. Rows follow the generic settings rows (60 px
//! with a divider).

use tetrad_assets::{MUTED, TEXT};

use super::extensions::{divider, label, switch_row};
use crate::layout::{LARGE, Layout, Look, MEDIUM};
use crate::render::Align;
use crate::*;

const ROW: u32 = 60;
const HELP: &str = "Open the address in a browser on the same Wi-Fi network, enter the PIN and \
    drop IR files on the page. Plain HTTP: turn Remote off on networks you do not trust.";

/// `body` is the settings content under the page title.
pub(super) fn layout(ui: &UiState, l: &mut Layout, body: Rect) {
    let view = &ui.snapshot.remote;
    // Stock rows run 8 px past the text inset (`screens::settings`).
    let mut row = Rect::new(body.x, body.y, body.width + 8, ROW);
    l.control(row, Control::RemoteToggle, true, "Remote", Look::Area);
    switch_row(l, row, "Remote", Some(view.enabled));
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
            TEXT,
            Align::Right,
        );
        divider(l, row);
    }
    let top = row.bottom() + 16;
    l.paragraph(
        Rect::new(
            body.x + 16,
            top,
            body.width - 16,
            (body.bottom() - top).max(0) as u32,
        ),
        HELP,
        MEDIUM,
        MUTED,
    );
}

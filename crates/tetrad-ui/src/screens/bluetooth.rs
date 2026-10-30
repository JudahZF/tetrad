//! Owned Bluetooth page (Tetrad; stock has none, docs/bluetooth.md): the switches (Bluetooth,
//! MIDI over Bluetooth, Host mode), then the paired devices with FORGET. New devices can pair
//! while this page is open. Rows follow the generic settings rows (60 px with a divider); the
//! page scrolls as they do.

use super::extensions::divider;
use super::settings::rich_height;
use crate::layout::{BODY, BODY_BOLD, ButtonStyle, CAPTION, Layout, ListId, Look, MEDIUM};
use crate::render::Align;
use crate::*;

const ROW: u32 = 60;
const HEADER: u32 = 34;
pub(super) const HELP: &str = "Bluetooth is experimental. New phones and computers can pair \
    while this page is open. \
    For BLE MIDI on a Mac, open Audio MIDI Setup, then MIDI Studio, then Bluetooth \
    Configuration. Pair at home: a pairing has no code.";

/// A switch row with a title and a second line under it.
fn switch_row(l: &mut Layout, row: Rect, control: Control, title: &str, detail: &str, on: bool) {
    let p = l.palette;
    l.control(row, control, true, title, Look::Area);
    let switch = Rect::new(row.right() - 66, row.y, 66, row.height);
    l.push(switch, "", Look::Switch { on: Some(on) });
    let x = row.x + 16;
    let width = (switch.x - 8 - x).max(0) as u32;
    l.text(
        Rect::new(x, row.y + 8, width, 22),
        title,
        BODY,
        p.text,
        Align::Left,
    );
    l.text(
        Rect::new(x, row.y + 32, width, 18),
        detail,
        CAPTION,
        p.muted,
        Align::Left,
    );
    divider(l, row);
}

pub(super) fn layout(ui: &UiState, l: &mut Layout, body: Rect) {
    let p = l.palette;
    let view = &ui.snapshot.bluetooth;
    // Stock rows run 8 px past the text inset (`screens::settings`).
    let area = Rect::new(body.x, body.y, body.width + 8, body.height);
    let help_width = body.width - 16;
    let devices = view.devices.len().max(1) as u32;
    let content = 3 * ROW + HEADER + devices * ROW + 16 + rich_height(HELP, MEDIUM, help_width);
    let offset = ui.offset(ListId::SettingsPage);
    l.scroll(ListId::SettingsPage, area, content, offset, |l, top| {
        let mut row = Rect::new(area.x, top, area.width, ROW);
        let radio = match (&view.status, &view.name) {
            (Some(status), _) => status.clone(),
            (None, Some(name)) if view.enabled => format!("On as {name}"),
            _ if view.enabled => "On".to_owned(),
            _ => "Off".to_owned(),
        };
        switch_row(
            l,
            row,
            Control::BluetoothToggle,
            "Bluetooth",
            &radio,
            view.enabled,
        );
        row.y += ROW as i32;
        let midi = "Notes, CCs and program changes, in and out";
        switch_row(
            l,
            row,
            Control::BluetoothMidiToggle,
            "MIDI over Bluetooth",
            midi,
            view.midi,
        );
        row.y += ROW as i32;
        let mut host = match &view.host {
            Some(host) => format!("Connected: {host}"),
            None if view.host_mode && view.enabled => "A phone or computer can connect".to_owned(),
            None if view.host_mode => "On when Bluetooth is on".to_owned(),
            None => "Phones and computers cannot connect".to_owned(),
        };
        if let Some(pin) = view.pin.as_ref().filter(|_| view.host_mode) {
            host = format!("{host} · App PIN {pin}");
        }
        let toggle = Control::BluetoothHostModeToggle;
        switch_row(l, row, toggle, "Host mode", &host, view.host_mode);
        row.y += ROW as i32;
        l.text(
            Rect::new(row.x + 16, row.y + 10, row.width - 32, 20),
            "Paired devices",
            BODY_BOLD,
            p.text,
            Align::Left,
        );
        row.y += HEADER as i32;
        if view.devices.is_empty() {
            l.text(
                Rect::new(row.x + 16, row.y, row.width - 32, ROW),
                "No paired devices",
                MEDIUM,
                p.muted,
                Align::Left,
            );
            divider(l, row);
            row.y += ROW as i32;
        }
        for (index, device) in view.devices.iter().enumerate() {
            let button = Rect::new(row.right() - 16 - 100, row.y + 10, 100, 40);
            let forget = Control::BluetoothForget(index as u8);
            l.button(button, forget, true, "FORGET", ButtonStyle::Inset);
            let x = row.x + 16;
            let width = (button.x - 12 - x).max(0) as u32;
            l.text(
                Rect::new(x, row.y + 8, width, 22),
                device.label.clone(),
                BODY,
                p.text,
                Align::Left,
            );
            let detail = if device.connected {
                format!("{} · connected", device.kind)
            } else {
                device.kind.clone()
            };
            let ink = if device.connected { p.accent } else { p.muted };
            l.text(
                Rect::new(x, row.y + 32, width, 18),
                detail,
                CAPTION,
                ink,
                Align::Left,
            );
            divider(l, row);
            row.y += ROW as i32;
        }
        l.paragraph(
            Rect::new(body.x + 16, row.y + 16, help_width, content),
            HELP,
            MEDIUM,
            p.muted,
        );
    });
}

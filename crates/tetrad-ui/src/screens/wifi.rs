//! Connection page (manual §10 System › Connection; `0x5d2508`, cells `0x5d0810`, states
//! `0x5d1530`): the status as the page title, the Domain Settings and Internet Check links,
//! the network table (the joined network first, as a card) and RESET WI-FI SETTINGS. Positions
//! are relative to the settings content panel and match the QC (stock 4.1.0, 2026-10-16).

use tetrad_assets::{Icon, IconSize, Rgb};

use crate::layout::{BODY, ButtonStyle, CAPTION, LARGE, Layout, ListId, Look, MEDIUM};
use crate::render::Align;
use crate::settings::DeviceAction;
use crate::*;

/// "Weak connection" (`0x5d1530` states 4 and 5).
const WEAK: Rgb = Rgb(0xff, 0xbb, 0x68);
/// Cell heights: the joined network's card and the other rows.
const CARD: u32 = 96;
const ROW: u32 = 60;
/// The network table (`0x5d2508`: 504 × 244 at 8, 58).
const TABLE: (i32, i32, u32, u32) = (8, 58, 504, 244);

/// Page title (`0x5d3f70`): "Internet Connected" while Cortex Control's internet is used,
/// else the status line; with no status yet, "No Internet Connection". Failures open stock
/// popups instead (`UiState::set_snapshot`).
fn title(s: &UiSnapshot) -> &'static str {
    match s.wifi.status {
        _ if s.control_online => WifiStatus::InternetConnected.label(),
        WifiStatus::Idle => WifiStatus::NoInternet.label(),
        status => status.label(),
    }
}

/// Cell state 3 (`0x5d42e0`): Cortex Control's internet is in use. I: Tetrad's `control_online`
/// (Control connected and online) stands for stock flags `0x3d8` and `0x3d9`.
const CONTROL_ROW: u32 = 80;

pub(super) fn layout(ui: &UiState, l: &mut Layout, page: Rect) {
    let p = l.palette;
    if ui.wifi_domains {
        return domains(ui, l, page);
    }
    let wifi = &ui.snapshot.wifi;
    let at =
        |x: i32, y: i32, width: u32, height: u32| Rect::new(page.x + x, page.y + y, width, height);
    l.text(
        at(16, 16, 360, 26),
        title(&ui.snapshot),
        LARGE,
        p.text,
        Align::Left,
    );
    link(
        l,
        // Stock 108 px overlaps Internet Check by 2 px; the touch area stops at it.
        at(289, 12, 106, 32),
        0,
        "Domain Settings",
        WifiControl::DomainSettings,
    );
    let check = at(395, 12, 116, 32);
    link(l, check, 16, "Internet Check", WifiControl::InternetCheck);
    // The red dot marks a disabled check (`0x5d59d4`).
    if ui.snapshot.setting(SettingId::InternetCheck) == Some(&SettingValue::Bool(false)) {
        l.push(
            Rect::new(check.x + 2, check.y + 12, 8, 8),
            "",
            Look::Dot { color: p.danger },
        );
    }
    let (x, y, width, height) = TABLE;
    let table = at(x, y, width, height);
    // With Control's internet in use the joined network is a plain row (`0x5d2028` state 0).
    let control = ui.snapshot.control_online;
    let card = |network: &WifiNetwork| network.connected && !control;
    let heights: Vec<u32> = wifi
        .networks
        .iter()
        .map(|network| if card(network) { CARD } else { ROW })
        .collect();
    let first = if control { CONTROL_ROW } else { 0 };
    l.scroll(
        ListId::Wifi,
        table,
        first + heights.iter().sum::<u32>(),
        ui.offset(ListId::Wifi),
        |l, mut top| {
            if control {
                control_row(ui, l, Rect::new(table.x, top, table.width, CONTROL_ROW));
                top += CONTROL_ROW as i32;
            }
            for (index, (network, height)) in wifi.networks.iter().zip(&heights).enumerate() {
                let cell = Rect::new(table.x, top, table.width, *height);
                let ip = wifi.ip_address.as_deref();
                network_cell(l, cell, index, network, card(network), ip);
                top += *height as i32;
            }
        },
    );
    // 16 px label: ink 166 × 11 on `ss4`, `ss5` and `sa2.png`.
    l.button_in(
        at(16, 351, 219, 45),
        Control::Action(DeviceAction::ResetWifi),
        true,
        "RESET WI-FI SETTINGS",
        ButtonStyle::Inset,
        BODY,
    );
}

/// A stock link button: 14 px #959595 underlined text `inset` from the left of `bounds`. The
/// touch area grows 4 px up and down to the 40 px minimum (review R10).
fn link(l: &mut Layout, bounds: Rect, inset: i32, text: &str, control: WifiControl) {
    let p = l.palette;
    let touch = Rect::new(bounds.x, bounds.y - 4, bounds.width, bounds.height + 8);
    l.control(touch, Control::Wifi(control), true, text, Look::Area);
    let width = CAPTION.width(text).ceil() as u32;
    let text_box = Rect::new(bounds.x + inset, bounds.y, width, bounds.height);
    l.text(text_box, text, CAPTION, p.muted, Align::Left);
    l.push(
        Rect::new(text_box.x, bounds.y + 23, width, 1),
        "",
        Look::Fill(p.muted),
    );
}

/// One table cell (`0x5d1530`). The joined network is a #1e1e1e card with its state line:
/// "Connected" (green) and the address above -60 dBm, "Weak connection" at or below it, or
/// "Obtaining IP address..." without a lease. Other networks are flat rows with a divider.
/// A WPA network (`secured`, stock auth > 2) has a 16 px lock before its SSID; a saved one
/// also has the forget icon (48 × 48 at x 407, `0x5d1388`).
fn network_cell(
    l: &mut Layout,
    cell: Rect,
    index: usize,
    network: &WifiNetwork,
    card: bool,
    ip: Option<&str>,
) {
    let p = l.palette;
    let inset = if card { 16 } else { 8 };
    if card {
        l.panel(cell, p.surface, 8.0, None);
    }
    let tap = Control::Wifi(WifiControl::Network(index as u16));
    if network.secured && network.saved {
        let forget = Rect::new(cell.x + 407, cell.y + (cell.height as i32 - 48) / 2, 48, 48);
        let (left, rest) = cell.take_left(407, 0);
        let (_, right) = rest.take_left(48, 0);
        l.control(left, tap, true, network.ssid.clone(), Look::Area);
        l.control(right, tap, true, network.ssid.clone(), Look::Area);
        l.control(
            forget,
            Control::Wifi(WifiControl::Forget(index as u16)),
            true,
            "FORGET",
            Look::Area,
        );
        l.push(
            forget,
            "",
            Look::Glyph {
                icon: Icon::Trash,
                size: IconSize::Medium,
                color: p.text,
            },
        );
    } else {
        l.control(cell, tap, true, network.ssid.clone(), Look::Area);
    }
    let (ssid_y, detail_y) = if card { (14, 37) } else { (10, 34) };
    let ssid_x = if network.secured {
        let (x, y) = if card { (16, 17) } else { (8, 13) };
        l.push(
            Rect::new(cell.x + x, cell.y + y, 16, 16),
            "",
            Look::Glyph {
                icon: Icon::Lock,
                size: IconSize::Small,
                color: p.text,
            },
        );
        inset + 20
    } else {
        inset
    };
    l.text(
        Rect::new(cell.x + ssid_x, cell.y + ssid_y, 390, 24),
        network.ssid.clone(),
        BODY,
        p.text,
        Align::Left,
    );
    l.text(
        Rect::new(cell.x + inset, cell.y + detail_y, 400, 18),
        network.detail(),
        CAPTION,
        p.muted,
        Align::Left,
    );
    l.push(
        Rect::new(cell.x + 466, cell.y + (cell.height as i32 - 24) / 2, 24, 24),
        "",
        Look::Signal {
            bars: network.signal.min(6),
        },
    );
    if !card {
        l.push(
            Rect::new(cell.x + 8, cell.y + 59, 488, 1),
            "",
            Look::Fill(p.surface),
        );
        return;
    }
    let line = |l: &mut Layout, x: i32, text: String, color: Rgb| {
        l.text(
            Rect::new(cell.x + x, cell.y + 64, 300, 18),
            text,
            CAPTION,
            color,
            Align::Left,
        );
    };
    let address = ip.map(|ip| format!("IP address {ip}"));
    match (network.weak(), address) {
        (false, Some(address)) => {
            line(l, 16, "Connected".into(), p.accent);
            line(l, 93, address, p.muted);
        }
        (false, None) => line(l, 16, WifiStatus::ObtainingIp.label().into(), p.muted),
        (true, address) => {
            line(l, 16, "Weak connection".into(), p.tone(WEAK));
            if let Some(address) = address {
                line(l, 133, address, p.muted);
            }
        }
    }
}

/// State 3: a card with the #ffd236 accent bar (2 × 56 at 14, 11) and the stock line
/// (`0x11e3e00`) in 16 px white at (32, 18).
fn control_row(ui: &UiState, l: &mut Layout, cell: Rect) {
    let p = l.palette;
    l.panel(cell, p.surface, 8.0, None);
    l.push(
        Rect::new(cell.x + 14, cell.y + 11, 2, 56),
        "",
        Look::Fill(p.warning),
    );
    l.paragraph(
        Rect::new(cell.x + 32, cell.y + 18, 420, 42),
        format!(
            "{} is currently using Cortex Control\u{2019}s internet connection",
            super::support::product_name(ui.snapshot.mini)
        ),
        BODY,
        p.text,
    );
}

/// Domain Settings (`0x40c4bc`), shown in place of the page; the page's ✓ closes it. Two
/// stock radio buttons (66 × 44, green when selected) pick the Cortex Cloud domain.
fn domains(ui: &UiState, l: &mut Layout, page: Rect) {
    let p = l.palette;
    let at =
        |x: i32, y: i32, width: u32, height: u32| Rect::new(page.x + x, page.y + y, width, height);
    l.panel(page, p.panel, 8.0, None);
    l.text(
        at(16, 16, 300, 26),
        "Domain Settings",
        LARGE,
        p.text,
        Align::Left,
    );
    l.paragraph(
        at(16, 58, 488, 70),
        format!(
            "Select which Cortex Cloud domain your {}\nconnects to. Use this setting to ensure \
             reliable cloud\nconnectivity based on your location.",
            super::support::product_name(ui.snapshot.mini)
        ),
        MEDIUM,
        p.text,
    );
    // Text box y and height, radio button y (`0x40c13c`: x 438, 66 × 44).
    let options: [(i32, u32, i32, &str); 2] = [
        (
            143,
            52,
            144,
            "Default Domain: Connects to the standard global\nCortex Cloud server.",
        ),
        (
            221,
            92,
            234,
            "Alternative Domain: Connects to an alternative\nserver that may improve \
             connectivity in certain\nregions experiencing access issues.",
        ),
    ];
    let current = ui.snapshot.setting(SettingId::CloudDomain);
    for (index, (y, height, button_y, text)) in options.into_iter().enumerate() {
        l.paragraph(at(16, y, 416, height), text, MEDIUM, p.text);
        let selected = current == Some(&SettingValue::Choice(index as u16));
        let button = at(438, button_y, 66, 44);
        l.panel(
            button,
            if selected { p.accent } else { p.surface },
            8.0,
            None,
        );
        l.control(
            button,
            Control::Setting(SettingControl::Set(SettingId::CloudDomain, index as i32)),
            current.is_some(),
            text.split(':').next().unwrap_or_default(),
            Look::Area,
        );
        let ink = if selected { p.canvas } else { p.text };
        let center = button.center();
        let dot = |l: &mut Layout, radius: i32, color: Rgb| {
            let size = (radius * 2) as u32;
            l.push(
                Rect::new(center.0 - radius, center.1 - radius, size, size),
                "",
                Look::Dot { color },
            );
        };
        dot(l, 10, ink);
        dot(l, 8, if selected { p.accent } else { p.surface });
        if selected {
            dot(l, 5, ink);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(rssi_dbm: i32, frequency_mhz: u32) -> WifiNetwork {
        WifiNetwork {
            ssid: "Home".into(),
            connected: true,
            bssid: "22:0b:8b:3e:5c:18".into(),
            frequency_mhz,
            rssi_dbm,
            ..WifiNetwork::default()
        }
    }

    /// The QC's own line (2026-10-16): channel from the 2.4 GHz table, "---" for 5 GHz; the
    /// joined network is weak at -60 dBm and below (`0x5d2028`).
    #[test]
    fn detail_line_and_weak_rule_follow_stock() {
        assert_eq!(
            network(-63, 2412).detail(),
            "22:0b:8b:3e:5c:18, ch: 1, rssi: -63 dBm"
        );
        assert_eq!(
            network(-50, 5180).detail(),
            "22:0b:8b:3e:5c:18, ch: ---, rssi: -50 dBm"
        );
        assert!(network(-60, 2412).weak());
        assert!(!network(-59, 2412).weak());
        let other = WifiNetwork {
            connected: false,
            ..network(-80, 2412)
        };
        assert!(!other.weak());
    }
}

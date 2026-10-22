//! Support pages (manual §10 "Support Settings"): Device Information with the device name
//! edit (`zenUI::DeviceInformation` `0x40d27c`, `DeviceNameDialog` `0x40f22c`), 3rd Party
//! Licenses with the licence viewer (`0x6cbbe0`, `0x6cb3c4`) and Diagnostics (`_INIT_561`).
//! Positions are the stock ones inside the 520 px settings page panel.

use tetrad_assets::{Icon, Rgb};

use crate::layout::{BODY, CAPTION, LARGE, Layout, ListId, Look};
use crate::render::{Align, HEIGHT, wrap};
use crate::state::SupportView;
use crate::*;

/// Stock orange of the diagnostics row names (`#ff7000`).
const ORANGE: Rgb = Rgb(0xff, 0x70, 0x00);
/// Device Information label and value boxes (292 × 33; values at x 212).
const ROW: i32 = 33;
const VALUE_X: i32 = 212;
/// Device Information pencil (`0x3ec328`): 98 px left of ✓ with an 18 px gap.
pub(super) const EDIT_BUTTON: Rect = Rect::new(578, 8, 98, 44);

/// The 3rd Party Licenses rows (`_INIT_584`); each label reads " license" after it.
pub(super) const LICENCES: [&str; 55] = [
    "binutils",
    "busybox",
    "bzip2",
    "collectd",
    "cramfs",
    "e2fsprogs",
    "elfutils",
    "expat",
    "f2fstools",
    "fontconfig",
    "freetype",
    "harfbuzz",
    "hostapd",
    "iw",
    "jpeg-turbo",
    "libarchive",
    "libcurl",
    "libevent",
    "libffi",
    "libnl",
    "libpcap",
    "libpng",
    "libsigc",
    "libsigsegv",
    "libtirpc",
    "libungif",
    "libunwind",
    "linux",
    "lrzsz",
    "lz4",
    "lzip",
    "lzo",
    "lzop",
    "memtester",
    "mtd",
    "ncurses",
    "ntp",
    "openssh",
    "openssl",
    "oprofile",
    "pcutils",
    "popt",
    "pppd",
    "protobuf",
    "qt",
    "readline",
    "rsync",
    "squashfs",
    "tslib",
    "uboot-tools",
    "util-linux",
    "wpa_supplicant",
    "zlib",
    "ADVobfuscator",
    "pugixml",
];

/// The file stem of a licence row (`0x6c9434`): two stock labels differ from their files.
pub(crate) fn licence_file(label: &str) -> &str {
    match label {
        "f2fstools" => "f2fs-tools",
        "pcutils" => "pciutils",
        label => label,
    }
}

/// The product name (`0x498ce4`: `isAtma` picks the mini), the `%1` of many stock texts.
pub(crate) fn product_name(mini: bool) -> &'static str {
    if mini {
        "Quad Cortex mini"
    } else {
        "Quad Cortex"
    }
}

/// Stock default name (`0x64fd2c` with `0x498ce4`).
pub(crate) fn default_device_name(mini: bool) -> &'static str {
    if mini {
        "Neural DSP Quad Cortex mini"
    } else {
        "Neural DSP Quad Cortex"
    }
}

/// Device Information: two sections of plain label / value text (no row cards) and the
/// pencil left of ✓ that opens the name keyboard.
pub(super) fn information(ui: &UiState, l: &mut Layout, page: Rect) {
    let p = l.palette;
    let a = &ui.snapshot.about;
    l.control(
        EDIT_BUTTON,
        Control::Setting(SettingControl::EditDeviceName),
        true,
        "EDIT",
        Look::IconButton {
            icon: Icon::Edit,
            fill: p.surface,
        },
    );
    let name = a.device_name.as_deref().map_or_else(
        || default_device_name(ui.snapshot.mini).to_owned(),
        str::to_owned,
    );
    let device = vec![
        ("Serial number:", a.serial.clone()),
        ("Device name:", Some(name)),
        ("MAC address:", a.mac.clone()),
    ];
    let mut software = vec![
        ("CorOS:", a.coros.clone()),
        ("Linux kernel:", a.kernel.clone()),
        ("U-Boot:", a.uboot.clone()),
    ];
    if a.zen_boards {
        software.extend([
            ("Zenjack FW app:", a.zenjack_app.clone()),
            ("Zenjack FW bootloader:", a.zenjack_bootloader.clone()),
            ("Zencoder FW app:", a.zencoder_app.clone()),
            ("Zencoder FW bootloader:", a.zencoder_bootloader.clone()),
        ]);
    }
    software.push(("Zenwireless FW:", a.wireless_fw.clone()));
    let sections = [
        ("Device information", device),
        ("Software information", software),
    ];
    let rows: usize = sections.iter().map(|(_, rows)| rows.len() + 1).sum();
    let viewport = page.inset(0, 8);
    l.scroll(
        ListId::SettingsPage,
        viewport,
        rows as u32 * ROW as u32 + 2 * 31,
        ui.offset(ListId::SettingsPage),
        |l, mut y| {
            for (index, (title, rows)) in sections.iter().enumerate() {
                if index > 0 {
                    l.push(
                        Rect::new(page.x + 16, y + 7, 489, 1),
                        "",
                        Look::Fill(p.background),
                    );
                    y += 31;
                }
                l.text(
                    Rect::new(page.x + 16, y, 292, ROW as u32),
                    *title,
                    LARGE,
                    p.text,
                    Align::Left,
                );
                y += ROW;
                for (label, value) in rows {
                    l.text(
                        Rect::new(page.x + 16, y, 292, ROW as u32),
                        *label,
                        CAPTION,
                        p.text,
                        Align::Left,
                    );
                    l.text(
                        Rect::new(page.x + VALUE_X, y, 292, ROW as u32),
                        value.as_deref().unwrap_or("-"),
                        CAPTION,
                        p.text,
                        Align::Left,
                    );
                    y += ROW;
                }
            }
        },
    );
}

/// 3rd Party Licenses: the package list (rows 60, spacing 8, from y 57), or the viewer of
/// the open package.
pub(super) fn licences(ui: &UiState, l: &mut Layout, page: Rect) {
    if let SupportView::Licence(label) = ui.support {
        return viewer(ui, l, label);
    }
    let list = Rect::new(page.x, page.y + 57, page.width, page.height - 57);
    const PITCH: i32 = 68;
    l.scroll(
        ListId::SettingsPage,
        list,
        LICENCES.len() as u32 * PITCH as u32,
        ui.offset(ListId::SettingsPage),
        |l, top| {
            for (index, label) in LICENCES.iter().enumerate() {
                crate::widgets::list_row(
                    l,
                    Rect::new(list.x + 8, top + index as i32 * PITCH, list.width - 16, 60),
                    Control::Setting(SettingControl::Licence(label)),
                    true,
                    format!("{label} license"),
                    None,
                    false,
                    None,
                );
            }
        },
    );
}

/// Licence viewer: a full-width 800 × 427 text page, text 600 px wide at x 100, centred
/// 16 px lines. The close control is not recovered: Back and ✓ return to the list (I).
fn viewer(ui: &UiState, l: &mut Layout, label: &str) {
    let p = l.palette;
    let area = Rect::new(0, 53, WIDTH, 427);
    // A page of its own over the settings frame: the frame stops taking input.
    l.overlay();
    l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(p.background));
    l.control(
        crate::widgets::CLOSE_BUTTON,
        Control::Back,
        true,
        "CLOSE",
        Look::IconButton {
            icon: Icon::Check,
            fill: p.panel,
        },
    );
    l.text(
        Rect::new(16, 8, 600, 44),
        format!("{label} license"),
        LARGE,
        p.muted,
        Align::Left,
    );
    let file = licence_file(label);
    let text = ui
        .snapshot
        .licence
        .as_ref()
        .filter(|licence| licence.package == file)
        .map(|licence| {
            licence
                .text
                .as_deref()
                .unwrap_or("Could not find license file!")
        });
    let lines = text.map_or_else(Vec::new, |text| {
        text.lines()
            .flat_map(|line| match line.trim_end() {
                "" => vec![String::new()],
                line => wrap(line, BODY, 600.0),
            })
            .collect()
    });
    const LINE: i32 = 22;
    let offset = ui.offset(ListId::SettingsPage);
    l.scroll(
        ListId::SettingsPage,
        area,
        lines.len() as u32 * LINE as u32 + 16,
        offset,
        |l, top| {
            // Only the lines in view become display items.
            let first = ((area.y - top) / LINE).max(0) as usize;
            let count = area.height as usize / LINE as usize + 2;
            for (index, line) in lines.iter().enumerate().skip(first).take(count) {
                l.text(
                    Rect::new(100, top + 8 + index as i32 * LINE, 600, LINE as u32),
                    line.clone(),
                    BODY,
                    p.text,
                    Align::Center,
                );
            }
        },
    );
}

/// Diagnostics: the three stock list rows, or the open page.
pub(super) fn diagnostics(ui: &UiState, l: &mut Layout, page: Rect) {
    match ui.support {
        SupportView::Diagnostics(DiagnosticsPage::Dsp) => dsp(ui, l, page),
        SupportView::Diagnostics(DiagnosticsPage::Footswitch) => footswitches(ui, l, page),
        SupportView::Diagnostics(DiagnosticsPage::Usb) => usb(ui, l, page),
        _ => {
            for (index, entry) in DiagnosticsPage::ALL.into_iter().enumerate() {
                crate::widgets::list_row(
                    l,
                    Rect::new(
                        page.x + 8,
                        page.y + 57 + index as i32 * 68,
                        page.width - 16,
                        60,
                    ),
                    Control::Setting(SettingControl::Diagnostics(entry)),
                    true,
                    entry.label(),
                    None,
                    false,
                    None,
                );
            }
        }
    }
}

fn title(l: &mut Layout, page: Rect, text: &str) {
    let p = l.palette;
    l.text(
        Rect::new(page.x + 16, page.y, page.width - 32, 52),
        text,
        LARGE,
        p.text,
        Align::Left,
    );
}

/// `QString::number(value, 'g', precision)`: `precision` significant digits without
/// trailing zeros; the exponent form below 1e-4 or from 10^precision (`%g`).
fn number_g(value: f32, precision: usize) -> String {
    let value = f64::from(value);
    if !value.is_finite() || value == 0.0 {
        return if value.is_nan() {
            "nan".into()
        } else {
            (value as f32).to_string()
        };
    }
    let trim = |text: String| {
        if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            text
        }
    };
    let scientific = format!("{:.*e}", precision.saturating_sub(1), value);
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    if exponent < -4 || exponent >= precision as i32 {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa.to_owned()), exponent.abs())
    } else {
        let decimals = (precision as i32 - 1 - exponent).max(0) as usize;
        trim(format!("{value:.decimals$}"))
    }
}

/// DSP Diagnostics (`0x3672a8`, update slot `0x364ba0`): rows by core; a cell shows "-1"
/// until its core reports. The "SOC2 ARM" column always shows "-1": `0x364ba0` writes only
/// the column of a `0x00` block and those come from cores 0..3.
fn dsp(ui: &UiState, l: &mut Layout, page: Rect) {
    let p = l.palette;
    title(l, page, DiagnosticsPage::Dsp.label());
    const COLUMN: i32 = 84;
    let x0 = page.x + 112;
    for (index, core) in ["Core 1", "Core 2", "Core 3", "Core 4", "SOC2 ARM"]
        .into_iter()
        .enumerate()
    {
        l.text(
            Rect::new(x0 + index as i32 * COLUMN, page.y + 60, COLUMN as u32, 17),
            core,
            CAPTION,
            p.text,
            Align::Left,
        );
    }
    for (row, label) in [
        "Drp Msg:",
        "FP exc:",
        "CPU Av/Pk(%):",
        "EMDMA (%):",
        "Heap(%):",
    ]
    .into_iter()
    .enumerate()
    {
        let y = page.y + 90 + row as i32 * 30;
        l.text(
            Rect::new(page.x + 16, y, 96, 17),
            label,
            CAPTION,
            p.tone(ORANGE),
            Align::Left,
        );
        for column in 0..5 {
            let core = ui
                .snapshot
                .dsp_diagnostics
                .get(column as usize)
                .copied()
                .flatten();
            let value = core.map_or_else(
                || "-1".to_owned(),
                |core| {
                    let [a, b, c] = core.cpu;
                    let [dm, pm, sd] = core.heap.map(|heap| number_g(heap, 3));
                    match row {
                        0 => core.dropped_messages.to_string(),
                        1 => core.fp_exceptions.to_string(),
                        2 => [a, b, c].map(|cpu| number_g(cpu, 2)).join("/"),
                        3 => number_g(core.emdma, 3),
                        _ => format!("DM:{dm}/PM:{pm}/SD:{sd}"),
                    }
                },
            );
            l.text(
                Rect::new(x0 + column * COLUMN, y, COLUMN as u32, 17),
                value,
                CAPTION,
                p.text,
                Align::Left,
            );
        }
    }
}

/// Footswitch Statistics (`0x68b8c4`): press/release and encoder turn counters since boot,
/// rows from y 80 on a 34 px pitch (classic order; the mini lists A–D).
fn footswitches(ui: &UiState, l: &mut Layout, page: Rect) {
    let p = l.palette;
    title(l, page, DiagnosticsPage::Footswitch.label());
    const NAMES: [&str; 11] = [
        "A", "B", "C", "D", "DOWN", "E", "F", "G", "H", "TEMPO", "UP",
    ];
    let shown = if ui.snapshot.mini { 4 } else { NAMES.len() };
    let rows = Rect::new(page.x, page.y + 80, page.width, page.height - 80);
    l.scroll(
        ListId::SettingsPage,
        rows,
        shown as u32 * 34,
        ui.offset(ListId::SettingsPage),
        |l, top| {
            for (index, name) in NAMES.iter().take(shown).enumerate() {
                let stats = ui.switch_stats[index];
                let y = top + index as i32 * 34;
                l.text(
                    Rect::new(rows.x + 24, y, 80, 17),
                    *name,
                    CAPTION,
                    p.tone(ORANGE),
                    Align::Left,
                );
                l.text(
                    Rect::new(rows.x + 120, y, 200, 17),
                    format!("(p: {}, r: {})", stats.pressed, stats.released),
                    CAPTION,
                    p.text,
                    Align::Left,
                );
                l.text(
                    Rect::new(rows.x + 300, y, 200, 17),
                    format!("(l: {}, r: {})", stats.left, stats.right),
                    CAPTION,
                    p.text,
                    Align::Left,
                );
            }
        },
    );
}

/// USB Statistics (`0x689de8`): 21 rows 18 px apart from y 68; label 300 × 17 at x 30 in
/// `#ff7000`, value 180 × 17 at x 350 in white, "---" until the first SOC2 ARM block. The
/// update slot `0x68a90c` colours the distances and the ±1 sample counts. I: 14 px text for
/// the stock 13 pt; the value is cut to the page; the rows scroll (the page is 411 px high).
fn usb(ui: &UiState, l: &mut Layout, page: Rect) {
    let p = l.palette;
    title(l, page, DiagnosticsPage::Usb.label());
    const LABELS: [&str; 21] = [
        "USBConnected",
        "IN rd/wr idx",
        "OUT rd/wr idx",
        "UI to DSP HID count (tx/rx)",
        "USBAudioInRdWrDistanceAverage",
        "USBAudioOutRdWrDistanceAverage",
        "USBAudioRxFrame",
        "USBAudioTxFrame",
        "USBAudioTxFrameSkipped",
        "USBAudioTxFrameAborted",
        "USBAudioOUTGapsDetected",
        "USBAudioOUTGapResets",
        "USBAudioOUTBufferResets",
        "USBAudioPlusOneSampleCount",
        "USBAudioMinusOneSampleCount",
        "USBAudioStreamingEPEnabled",
        "USBAudioStreamingEPDisabled",
        "USBMidiInCount",
        "USBMidiOutCount",
        "HID driver sent report count",
        "HID driver received report count",
    ];
    const PITCH: i32 = 18;
    const VALUE: i32 = 350;
    let rows = Rect::new(page.x, page.y + 68, page.width, page.height - 68);
    let values = ui.snapshot.usb_statistics.as_ref();
    l.scroll(
        ListId::SettingsPage,
        rows,
        LABELS.len() as u32 * PITCH as u32,
        ui.offset(ListId::SettingsPage),
        |l, top| {
            for (index, label) in LABELS.iter().enumerate() {
                let y = top + index as i32 * PITCH;
                l.text(
                    Rect::new(rows.x + 30, y, 300, 17),
                    *label,
                    CAPTION,
                    p.tone(ORANGE),
                    Align::Left,
                );
                let (text, tone) = values.map_or(("---", UsbTone::Plain), |values| {
                    (values[index].text.as_str(), values[index].tone)
                });
                let color = match tone {
                    UsbTone::Plain | UsbTone::InRange => p.text,
                    UsbTone::OutOfRange => Rgb(0xff, 0x00, 0x00),
                    UsbTone::Plus => Rgb(0x00, 0xff, 0x00),
                    UsbTone::Minus => Rgb(0x00, 0x00, 0xff),
                };
                l.text(
                    Rect::new(rows.x + VALUE, y, rows.width - VALUE as u32, 17),
                    text,
                    CAPTION,
                    color,
                    Align::Left,
                );
            }
        },
    );
}

#[cfg(test)]
mod tests {
    /// The DSP Diagnostics cells use Qt's `'g'` format: significant digits, no trailing
    /// zeros, the exponent form for large values (`0x364ba0`: CPU 2 digits, heap 3).
    #[test]
    fn diagnostics_numbers_follow_qt_g_format() {
        let g = super::number_g;
        assert_eq!(g(12.5, 2), "12");
        assert_eq!(g(3.25, 2), "3.2");
        assert_eq!(g(40.0, 2), "40");
        assert_eq!(g(123.4, 2), "1.2e+02");
        assert_eq!(g(0.0, 3), "0");
        assert_eq!(g(49.996, 3), "50");
        assert_eq!(g(0.000_012, 3), "1.2e-05");
    }
}

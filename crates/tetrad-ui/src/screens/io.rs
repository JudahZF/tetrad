//! I/O Settings (swipe down from the Grid; manual §3, shots 102 and 049): grey title with
//! the selected port in green, GLOBAL EQ and ✓, the rear-panel port map (tap a port to
//! select it, hold OUT 1/2 or 3/4 to pair them) and the 5 × 2 parameter card. Encoders
//! A–H, BANK DOWN and TEMPO turn the cells in reading order.

use tetrad_assets::{
    ACCENT, BACKGROUND, CANVAS, DIVIDER, FAINT, Icon, MUTED, PANEL, Rgb, SURFACE, TEXT,
};

use crate::layout::{BODY, ButtonStyle, CAPTION, Layout, Look, MEDIUM};
use crate::render::Align;
use crate::settings::{EqBand, EqTab, IoCell, OutputJack, SettingKind};
use crate::*;

/// Parameter card (spec quickstart §1): two 107 px rows of five ~157 px cells.
const CARD: Rect = Rect::new(8, 257, 784, 215);
const COLUMNS: i32 = 5;
const CELL: (u32, u32) = (157, 107);
const GLOBAL_EQ: Rect = Rect::new(577, 8, 108, 44);
/// Vertical cell divider on the card (shots 102 and 049 sample `#182418` in RGB565).
const CELL_DIVIDER: Rgb = Rgb(0x1c, 0x26, 0x1c);
/// Stock meter bar height (shots 102, 049) and its break, 1 px of card colour at the
/// −10 dB step (the yellow threshold): x 120 of the 16..152 bar in shot 102.
const BAR_HEIGHT: u32 = 16;
const BAR_BREAK: f32 = 0.76;

/// A rear-panel connector of the map (spec quickstart §13 centres, ±4 px). The capture
/// connection diagram draws the same map ([`rear_panel`]).
pub(super) struct Port {
    pub(super) tab: Option<IoTab>,
    pub(super) label: &'static str,
    center: (i32, i32),
    shape: Shape,
    /// Label above the connector (upper row) instead of below.
    above: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Usb,
    Jack,
    Din,
    Xlr,
    Combo,
}

impl Shape {
    fn radius(self) -> i32 {
        match self {
            Shape::Usb => 18,
            Shape::Jack => 20,
            Shape::Din => 22,
            Shape::Xlr | Shape::Combo => 32,
        }
    }
}

pub(super) const PORTS: [Port; 17] = {
    const fn port(
        tab: Option<IoTab>,
        label: &'static str,
        center: (i32, i32),
        shape: Shape,
        above: bool,
    ) -> Port {
        Port {
            tab,
            label,
            center,
            shape,
            above,
        }
    }
    use IoTab::{Expression, Headphones, Input1, Input2, Midi, Out12, Out34, Return, Send, Usb};
    use Shape::{Combo, Din, Jack, Xlr};
    [
        port(Some(Usb), "USB", (43, 111), Shape::Usb, false),
        port(Some(Expression), "EXP 2", (102, 115), Jack, true),
        port(Some(Expression), "EXP 1", (102, 175), Jack, false),
        port(Some(Midi), "MIDI\nOUT", (160, 115), Din, false),
        port(Some(Midi), "MIDI\nIN", (224, 115), Din, false),
        port(Some(Out12), "OUT 2/R", (298, 125), Xlr, false),
        port(Some(Out12), "OUT 1/L", (370, 125), Xlr, false),
        // The headphone jack's label is the icon above it (shots 102, 067).
        port(Some(Headphones), "", (430, 115), Jack, true),
        port(None, "CAPTURE\nOUT", (430, 175), Jack, false),
        port(Some(Out34), "OUT 4/R", (490, 115), Jack, true),
        port(Some(Out34), "OUT 3/L", (490, 175), Jack, false),
        port(Some(Return), "RET 2", (550, 115), Jack, true),
        port(Some(Return), "RET 1", (550, 175), Jack, false),
        port(Some(Send), "SEND 2", (612, 115), Jack, true),
        port(Some(Send), "SEND 1", (612, 175), Jack, false),
        port(Some(Input2), "INPUT 2", (674, 144), Combo, true),
        port(Some(Input1), "INPUT 1", (744, 144), Combo, true),
    ]
};

pub(super) fn layout(ui: &UiState, l: &mut Layout, tab: IoTab) {
    // 4.1.0 puts the I/O preset button (`0x35c4ac` builds `0x227a84` for 31000) left of the
    // title, as on the Global EQ page (I: same (8, 8) place); shot 102 is older.
    super::settings_presets::button(ui, l, crate::SettingsPresetPage::Io);
    let free = crate::widgets::page_header(l, "", Control::Back, Icon::Check);
    let title = "I/O Settings";
    let title_width = MEDIUM.width(title).ceil() as u32 + 8;
    let title_x = super::settings_presets::BUTTON.right() + 8;
    l.text(
        Rect::new(title_x, free.y, title_width, free.height),
        title,
        MEDIUM,
        MUTED,
        Align::Left,
    );
    l.text(
        Rect::new(title_x + title_width as i32 + 8, free.y, 200, free.height),
        tab.title(),
        MEDIUM,
        ACCENT,
        Align::Left,
    );
    // Stock 102: `#1e1e1e` button, grey 16 px Regular text.
    l.panel(GLOBAL_EQ, SURFACE, 8.0, None);
    l.control(
        GLOBAL_EQ,
        Control::Navigate(Screen::GlobalEq(EqTab::Band(EqBand(0)))),
        true,
        "GLOBAL EQ",
        Look::Area,
    );
    l.text(GLOBAL_EQ, "GLOBAL EQ", BODY, MUTED, Align::Center);
    let eq_on = ui.snapshot.setting(SettingId::EqEnabled) == Some(&SettingValue::Bool(true));
    l.push(
        Rect::new(GLOBAL_EQ.right() - 10, GLOBAL_EQ.y + 4, 6, 6),
        "",
        Look::Dot {
            color: if eq_on { ACCENT } else { FAINT },
        },
    );
    port_map(ui, l, tab);
    if matches!(tab, IoTab::Usb | IoTab::UsbRouting) {
        for (index, page) in [IoTab::Usb, IoTab::UsbRouting].into_iter().enumerate() {
            let bounds = Rect::new(652 + index as i32 * 70, 205, 66, 52);
            let selected = page == tab;
            if selected {
                l.panel(
                    Rect::new(bounds.x, bounds.y, bounds.width, 60),
                    PANEL,
                    8.0,
                    None,
                );
            }
            l.control(
                bounds,
                Control::Setting(SettingControl::IoTab(page)),
                true,
                ["1", "2"][index],
                Look::Area,
            );
            l.text(
                bounds,
                ["1", "2"][index],
                BODY,
                if selected { TEXT } else { MUTED },
                Align::Center,
            );
        }
    }
    l.panel(CARD, PANEL, 8.0, None);
    let rows = tab.cells(&ui.snapshot().rows);
    for (row, cells) in rows.iter().enumerate() {
        card_row(ui, l, tab, row, cells);
    }
    l.push(
        Rect::new(CARD.x, CARD.y + CELL.1 as i32, CARD.width, 2),
        "",
        Look::Fill(DIVIDER),
    );
}

fn port_map(ui: &UiState, l: &mut Layout, tab: IoTab) {
    let selected_tab = match tab {
        IoTab::UsbRouting => IoTab::Usb,
        tab => tab,
    };
    let paired =
        |jack| ui.snapshot.setting(SettingId::OutputPair(jack)) == Some(&SettingValue::Bool(true));
    for port in &PORTS {
        let Some(tab) = port.tab else { continue };
        let (cx, cy) = port.center;
        let radius = port.shape.radius();
        // Large connectors sit 68 px apart: their targets touch but stay disjoint.
        let reach = radius + if radius >= 32 { 2 } else { 4 };
        let target = Rect::new(cx - reach, cy - reach, reach as u32 * 2, reach as u32 * 2);
        l.control(
            target,
            Control::Setting(SettingControl::IoTab(tab)),
            true,
            if port.label.is_empty() {
                tab.title()
            } else {
                port.label
            },
            Look::Area,
        );
    }
    rear_panel(l, 0, |port| port.tab == Some(selected_tab));
    for (jack, center) in [
        (OutputJack::Out12, (327, 147)),
        (OutputJack::Out34, (490, 145)),
    ] {
        link(l, center, if paired(jack) { MUTED } else { FAINT });
    }
}

/// The rear-panel map `dy` px lower: each connector, its label and the stems from the USB,
/// MIDI and EXP 1 connectors to their labels (shot 102). `lit` ports are green.
pub(super) fn rear_panel(l: &mut Layout, dy: i32, lit: impl Fn(&Port) -> bool) {
    for port in &PORTS {
        let lit = lit(port);
        let (cx, cy) = (port.center.0, port.center.1 + dy);
        connector(l, port, (cx, cy), lit);
        let radius = port.shape.radius();
        if port.label.is_empty() {
            // The headphone jack: a white icon in place of the label.
            super::settings::glyph(
                l,
                Rect::new(cx - 14, cy - 61, 28, 28),
                Icon::Headphones,
                TEXT,
            );
            continue;
        }
        // USB and MIDI labels sit under their stems (shot 102).
        let y = match (port.above, port.shape) {
            (true, _) => cy - radius - 32,
            (false, Shape::Usb | Shape::Din) => 188 + dy,
            (false, _) => cy + radius + 12,
        };
        let height = if port.label.contains('\n') { 40 } else { 20 };
        l.push(
            Rect::new(cx - 40, y, 80, height),
            port.label,
            Look::Paragraph {
                font: CAPTION,
                color: if lit { TEXT } else { MUTED },
                align: Align::Center,
            },
        );
    }
    for x in [43, 160, 224] {
        l.push(Rect::new(x - 1, 140 + dy, 2, 40), "", Look::Fill(FAINT));
    }
}

/// One connector drawn from discs (no stock images): selected ports are green.
fn connector(l: &mut Layout, port: &Port, (cx, cy): (i32, i32), selected: bool) {
    let disc = |l: &mut Layout, (dx, dy): (i32, i32), radius: i32, color: Rgb| {
        let size = (radius * 2) as u32;
        l.push(
            Rect::new(cx + dx - radius, cy + dy - radius, size, size),
            "",
            Look::Dot { color },
        );
    };
    let body = if selected { ACCENT } else { PANEL };
    match port.shape {
        Shape::Usb => {
            let mark = if selected { ACCENT } else { TEXT };
            l.panel(Rect::new(cx - 18, cy - 18, 36, 36), mark, 3.0, None);
            l.panel(Rect::new(cx - 14, cy - 14, 28, 28), CANVAS, 1.0, None);
            l.push(Rect::new(cx - 7, cy - 4, 14, 7), "", Look::Fill(mark));
        }
        // Shot 102 row 115: black rim, solid hex nut, a 1 px dark ring, black hole.
        Shape::Jack => {
            disc(l, (0, 0), 22, CANVAS);
            hex_nut(l, (cx, cy), 21, body);
            disc(l, (0, 0), 13, CANVAS);
            disc(l, (0, 0), 12, body);
            disc(l, (0, 0), 8, CANVAS);
        }
        Shape::Din => {
            disc(l, (0, 0), 22, if selected { ACCENT } else { PANEL });
            disc(l, (0, 0), 19, PANEL);
            disc(l, (0, 0), 14, CANVAS);
            for (dx, dy) in [(-8, 2), (-5, -5), (0, -8), (5, -5), (8, 2)] {
                disc(l, (dx, dy), 2, MUTED);
            }
        }
        Shape::Xlr => {
            disc(l, (0, 0), 32, body);
            disc(
                l,
                (0, 0),
                29,
                if selected { ACCENT_FILL } else { BACKGROUND },
            );
            disc(l, (0, 0), 20, if selected { ACCENT } else { CANVAS });
            disc(l, (0, 0), 17, CANVAS);
            for (dx, dy) in [(-7, -4), (7, -4), (0, 7)] {
                disc(l, (dx, dy), 3, if selected { ACCENT } else { MUTED });
            }
        }
        // Shot 102 x 674: XLR/¼" combo. Black rim (a 1 px green ring when selected), a
        // 21 px body with the latch tab at the lower left, and a black centre hole with
        // three pin lobes (top, left, right).
        Shape::Combo => {
            disc(l, (0, 0), 32, body);
            disc(l, (0, 0), if selected { 31 } else { 32 }, CANVAS);
            disc(l, (-16, 15), 3, body);
            disc(l, (0, 0), 21, body);
            disc(l, (0, 0), 9, CANVAS);
            for (dx, dy) in [(0, -11), (-11, 0), (11, 0)] {
                disc(l, (dx, dy), 4, CANVAS);
            }
        }
    }
}

const ACCENT_FILL: Rgb = tetrad_assets::ACCENT_FILL;

/// Flat-top hex nut (¼" jack body, shot 102) of circumradius `radius`: solid 1 px rows.
fn hex_nut(l: &mut Layout, (cx, cy): (i32, i32), radius: i32, color: Rgb) {
    let apothem = (radius as f32 * 3f32.sqrt() / 2.0).round() as i32;
    for y in -apothem..apothem {
        let half = (radius as f32 - (y as f32 + 0.5).abs() / 3f32.sqrt()).round() as i32;
        l.push(
            Rect::new(cx - half, cy + y, half as u32 * 2, 1),
            "",
            Look::Fill(color),
        );
    }
}

/// Output pairing mark between two jacks: a chain link, two rings with a bar through them.
fn link(l: &mut Layout, (cx, cy): (i32, i32), color: Rgb) {
    for dx in [-10, -2] {
        l.panel(Rect::new(cx + dx, cy - 5, 12, 10), color, 5.0, None);
        l.panel(Rect::new(cx + dx + 2, cy - 3, 8, 6), BACKGROUND, 3.0, None);
    }
    l.push(Rect::new(cx - 2, cy - 6, 4, 12), "", Look::Fill(BACKGROUND));
    l.push(Rect::new(cx - 5, cy - 1, 10, 2), "", Look::Fill(color));
}

fn cell_rect(row: usize, column: i32, span: i32) -> Rect {
    let x = CARD.x + column * CELL.0 as i32;
    let right = if column + span >= COLUMNS {
        CARD.right()
    } else {
        x + span * CELL.0 as i32
    };
    Rect::new(
        x,
        CARD.y + row as i32 * CELL.1 as i32,
        (right - x) as u32,
        CELL.1,
    )
}

fn card_row(ui: &UiState, l: &mut Layout, tab: IoTab, row: usize, cells: &[IoCell]) {
    let meters = &ui.snapshot().io_meters;
    let fixed = cells
        .iter()
        .filter(|cell| !matches!(cell, IoCell::Text(_)))
        .count() as i32;
    let mut column = 0;
    for cell in cells {
        let span = match cell {
            IoCell::Text(_) => (COLUMNS - fixed).max(1),
            _ => 1,
        };
        let bounds = cell_rect(row, column, span);
        // Shot 102: a divider closes every cell short of the card edge, so a lone
        // meter cell still has one.
        if column + span < COLUMNS {
            l.push(
                Rect::new(bounds.right() - 2, bounds.y + 14, 2, 78),
                "",
                Look::Fill(CELL_DIVIDER),
            );
        }
        match *cell {
            IoCell::Setting(id) => setting_cell(ui, l, bounds, id),
            IoCell::Meter(label) => meter_cell(l, bounds, label, meter_db(meters, tab, label)),
            IoCell::UsbMeter(label, help) => {
                pair_meter_cell(l, bounds, label, Some(help), usb_meter_db(meters, label))
            }
            IoCell::HeadphoneMeter => pair_meter_cell(l, bounds, "LEVEL", None, meters.headphones),
            IoCell::FeedMeter(feed) => match *feed.jacks() {
                [left, right] => pair_meter_cell(
                    l,
                    bounds,
                    feed.label(),
                    None,
                    [meters.hp_feed[left], meters.hp_feed[right]],
                ),
                [jack] => meter_cell(l, bounds, feed.label(), meters.hp_feed[jack]),
                _ => {}
            },
            IoCell::Text(text) => {
                l.paragraph(bounds.inset(10, 10), text, BODY, MUTED);
            }
            IoCell::Action(action, label) => {
                l.button(
                    Rect::new(bounds.x + 10, bounds.y + 40, bounds.width - 20, 44),
                    Control::Action(action),
                    true,
                    label,
                    ButtonStyle::Inset,
                );
            }
        }
        column += span;
    }
}

/// Whether the combo-jack rules allow a cell (`0x3594fc`): PHANTOM needs Mic, IMPEDANCE
/// needs Instrument. The ESS rule (no TYPE) is the application's.
fn allowed(ui: &UiState, id: SettingId) -> bool {
    let mic =
        |jack| ui.snapshot.setting(SettingId::InputType(jack)) == Some(&SettingValue::Choice(1));
    match id {
        SettingId::Phantom(jack) => mic(jack),
        SettingId::InputImpedance(jack) => !mic(jack),
        _ => true,
    }
}

/// Stock panel cell per kind: level knob, stepped knob (opens its list), or a two-label
/// vertical switch (On/Off, Mic/Instrument).
fn setting_cell(ui: &UiState, l: &mut Layout, bounds: Rect, id: SettingId) {
    let value = ui.snapshot.setting(id);
    let enabled = value.is_some() && allowed(ui, id);
    match (id.kind(), value) {
        (SettingKind::Toggle, _) => {
            let on = value == Some(&SettingValue::Bool(true));
            switch_cell(
                l,
                bounds,
                id,
                ["On", "Off"],
                on.then_some(1).or(value.map(|_| 0)),
                enabled,
            );
        }
        (SettingKind::Choice(options), _) if options.len() == 2 => {
            let index = match value {
                Some(SettingValue::Choice(index)) => Some(usize::from(*index)),
                _ => None,
            };
            switch_cell(l, bounds, id, [options[1], options[0]], index, enabled);
        }
        (SettingKind::Choice(options), _) => {
            l.control(
                bounds,
                Control::Setting(SettingControl::Open(id)),
                enabled,
                id.label(),
                Look::Area,
            );
            let unit = matches!(id, SettingId::InputImpedance(_)).then_some("Ω");
            stepped_knob(l, bounds, id, value, options.len(), unit, enabled);
        }
        (SettingKind::Number { .. }, _) => {
            let position = match value {
                Some(SettingValue::Number(raw)) => id.knob_position(*raw),
                _ => 0.0,
            };
            l.control(
                bounds,
                Control::Setting(SettingControl::Open(id)),
                enabled,
                id.label(),
                Look::Knob {
                    value: id.format(value),
                    kind: ParameterKind::Continuous { position },
                    color: ACCENT,
                    focused: false,
                },
            );
        }
    }
}

/// Stock stepped knob (IMPEDANCE in 102, HP SOURCE in 049): a black ring, one detent dot
/// per option over 270° from the lower left (the current one green), a white pointer and
/// no arc. Value and grey unit at the bottom left, as the level knobs.
fn stepped_knob(
    l: &mut Layout,
    bounds: Rect,
    id: SettingId,
    value: Option<&SettingValue>,
    count: usize,
    unit: Option<&str>,
    enabled: bool,
) {
    let index = match value {
        Some(SettingValue::Choice(index)) => Some(usize::from(*index)),
        _ => None,
    };
    let ink = if enabled { TEXT } else { FAINT };
    l.text(
        Rect::new(bounds.x + 10, bounds.y + 6, bounds.width - 20, 28),
        id.label(),
        BODY,
        ink,
        Align::Left,
    );
    let value = id.format(value);
    let row = Rect::new(bounds.x + 10, bounds.bottom() - 40, 80, 28);
    l.text(row, value.clone(), BODY, ink, Align::Left);
    if let Some(unit) = unit.filter(|_| index.is_some()) {
        let x = row.x + BODY.width(&value).ceil() as i32 + 4;
        l.text(
            Rect::new(x, row.y + 2, 24, 28),
            unit,
            CAPTION,
            MUTED,
            Align::Left,
        );
    }
    let (cx, cy) = (
        bounds.right() - 44,
        bounds.y + bounds.height as i32 / 2 + 12,
    );
    let disc = |l: &mut Layout, (x, y): (f32, f32), radius: f32, color: Rgb| {
        let size = (radius * 2.0).round() as u32;
        l.push(
            Rect::new(
                (x - radius).round() as i32,
                (y - radius).round() as i32,
                size,
                size,
            ),
            "",
            Look::Dot { color },
        );
    };
    let center = (cx as f32, cy as f32);
    disc(l, center, 24.5, CANVAS);
    disc(l, center, 21.5, PANEL.mix(CANVAS, 60));
    let angle_of =
        |option: usize| (135.0 + 270.0 * option as f32 / (count.max(2) - 1) as f32).to_radians();
    for option in 0..count {
        let angle = angle_of(option);
        let color = match (Some(option) == index, enabled) {
            (true, true) => ACCENT,
            (true, false) => FAINT,
            (false, _) => ACCENT_FILL,
        };
        let at = (center.0 + angle.cos() * 34.0, center.1 + angle.sin() * 34.0);
        disc(l, at, 3.5, color);
    }
    if let Some(index) = index {
        let (cos, sin) = (angle_of(index).cos(), angle_of(index).sin());
        l.line(
            (center.0 + cos * 7.0, center.1 + sin * 7.0),
            (center.0 + cos * 21.0, center.1 + sin * 21.0),
            3.0,
            ink,
        );
    }
}

/// Vertical two-position switch with stock labels: `labels[0]` at the top (index 1),
/// `labels[1]` at the bottom (index 0). A tap flips it.
fn switch_cell(
    l: &mut Layout,
    bounds: Rect,
    id: SettingId,
    labels: [&str; 2],
    index: Option<usize>,
    enabled: bool,
) {
    let flip = index.map_or(1, |index| 1 - index.min(1) as i32);
    l.control(
        bounds,
        Control::Setting(SettingControl::Set(id, flip)),
        enabled,
        id.label(),
        Look::Area,
    );
    let ink = if enabled { TEXT } else { FAINT };
    l.text(
        Rect::new(bounds.x + 10, bounds.y + 6, bounds.width - 20, 28),
        id.label(),
        BODY,
        ink,
        Align::Left,
    );
    let track = Rect::new(bounds.x + 16, bounds.y + 44, 22, 46);
    l.panel(track, CANVAS, 11.0, None);
    for (position, label) in labels.into_iter().enumerate() {
        let active = index == Some(1 - position);
        let y = track.y + position as i32 * 23;
        l.text(
            Rect::new(track.right() + 16, y, bounds.width - 60, 23),
            label,
            BODY,
            if active && enabled { TEXT } else { FAINT },
            Align::Left,
        );
        if active {
            let ring = Rect::new(track.x + 1, y + 2, 20, 20);
            l.push(
                ring,
                "",
                Look::Dot {
                    color: if enabled { ACCENT } else { FAINT },
                },
            );
            l.push(ring.inset(3, 3), "", Look::Dot { color: CANVAS });
        }
    }
}

/// The reading of a level meter cell, by its stock label; the three LIMITER cells differ by
/// tab. Each LIMITER cell shows the louder of its pair (owned).
fn meter_db(meters: &IoMeters, tab: IoTab, label: &str) -> f32 {
    let pair_limit = |[left, right]: [f32; 2]| left.max(right);
    match (tab, label) {
        (_, "IN 1 LEVEL") => meters.inputs[0],
        (_, "IN 2 LEVEL") => meters.inputs[1],
        (_, "RET 1") => meters.returns[0],
        (_, "RET 2") => meters.returns[1],
        (_, "OUT 1/L") => meters.outputs[0],
        (_, "OUT 2/R") => meters.outputs[1],
        (_, "OUT 3/L") => meters.outputs[2],
        (_, "OUT 4/R") => meters.outputs[3],
        (_, "SEND 1") => meters.sends[0],
        (_, "SEND 2") => meters.sends[1],
        (IoTab::Out12, "LIMITER") => pair_limit([meters.limiters[0], meters.limiters[1]]),
        (IoTab::Out34, "LIMITER") => pair_limit([meters.limiters[2], meters.limiters[3]]),
        (IoTab::Send, "LIMITER") => pair_limit(meters.send_limiters),
        _ => IoMeters::FLOOR,
    }
}

/// The left/right reading of a USB pair meter cell, by its stock label.
fn usb_meter_db(meters: &IoMeters, label: &str) -> [f32; 2] {
    let pair = |list: &[[f32; 2]; 4], first: &str| {
        ["1/2", "3/4", "5/6", "7/8"]
            .iter()
            .position(|pair| *pair == first)
            .map_or([IoMeters::FLOOR; 2], |index| list[index])
    };
    match label.split_once(' ') {
        Some(("IN", channels)) => pair(&meters.usb_in, channels),
        Some(("OUT", channels)) => pair(&meters.usb_out, channels),
        _ => [IoMeters::FLOOR; 2],
    }
}

/// A left-aligned meter reading: the number white, the "dB" unit grey as stock (review R32).
fn db_readout(l: &mut Layout, bounds: Rect, db: f32) {
    let number = format!("{:.1}", db.max(IoMeters::FLOOR));
    let unit_x = bounds.x + BODY.width(&number).ceil() as i32 + 4;
    l.text(bounds, number, BODY, TEXT, Align::Left);
    l.text(
        Rect::new(
            unit_x,
            bounds.y,
            (bounds.right() - unit_x).max(0) as u32,
            bounds.height,
        ),
        "dB",
        BODY,
        MUTED,
        Align::Left,
    );
}

/// Level meter cell (shot 102 row 2).
fn meter_cell(l: &mut Layout, bounds: Rect, label: &str, db: f32) {
    l.text(
        Rect::new(bounds.x + 10, bounds.y + 6, bounds.width - 20, 28),
        label,
        BODY,
        TEXT,
        Align::Left,
    );
    db_readout(
        l,
        Rect::new(bounds.x + 10, bounds.y + 40, bounds.width - 20, 24),
        db,
    );
    meter_bar(
        l,
        Rect::new(
            bounds.x + 8,
            bounds.bottom() - 28,
            bounds.width - 20,
            BAR_HEIGHT,
        ),
        db,
    );
}

/// One stock meter bar with its break.
fn meter_bar(l: &mut Layout, bar: Rect, db: f32) {
    l.push(
        bar,
        "",
        Look::Meter {
            level: IoMeters::level(db),
        },
    );
    let x = bar.x + (bar.width as f32 * BAR_BREAK).round() as i32;
    l.push(Rect::new(x, bar.y, 1, bar.height), "", Look::Fill(PANEL));
}

/// Left/right pair meter (kind 6; USB in shot 049): title, (i) help when it has one, two
/// readouts (only the left one has the unit, as stock) and two stock bars.
fn pair_meter_cell(
    l: &mut Layout,
    bounds: Rect,
    label: &'static str,
    help: Option<&'static str>,
    db: [f32; 2],
) {
    l.text(
        Rect::new(bounds.x + 10, bounds.y + 6, bounds.width - 50, 28),
        label,
        BODY,
        TEXT,
        Align::Left,
    );
    // Stock ⓘ sits 28 px left of the cell's right edge (review R32).
    if let Some(help) = help {
        super::settings::info_button(l, (bounds.right() - 28, bounds.y + 20), label, help);
    }
    let readout = Rect::new(bounds.x + 10, bounds.y + 38, bounds.width - 20, 22);
    db_readout(l, readout, db[0]);
    l.text(
        readout,
        format!("{:.1}", db[1].max(IoMeters::FLOOR)),
        BODY,
        TEXT,
        Align::Right,
    );
    // Shot 049: bars at y + 62 and y + 83.
    for (channel, db) in db.into_iter().enumerate() {
        meter_bar(
            l,
            Rect::new(
                bounds.x + 8,
                bounds.y + 62 + channel as i32 * 21,
                bounds.width - 20,
                BAR_HEIGHT,
            ),
            db,
        );
    }
}

impl UiState {
    /// Encoder `knob` turns cell `knob` of the card in reading order (manual §2: encoders
    /// control the parameters on screen). Levels step, choices clamp, switches set by
    /// direction.
    pub(crate) fn io_knob(&mut self, tab: IoTab, knob: KnobIndex, delta: i32) -> Vec<UiCommand> {
        let index = usize::from(knob.get());
        let Some(IoCell::Setting(id)) = tab.cells(&self.snapshot.rows)[index / COLUMNS as usize]
            .get(index % COLUMNS as usize)
            .copied()
        else {
            return Vec::new();
        };
        if !allowed(self, id) {
            return Vec::new();
        }
        match (id.kind(), self.snapshot.setting(id)) {
            (SettingKind::Number { .. }, Some(_)) => id
                .step(self.snapshot.setting(id), delta)
                .map(|value| vec![UiCommand::ChangeSetting { id, value }])
                .unwrap_or_default(),
            (SettingKind::Choice(options), Some(SettingValue::Choice(index))) => {
                let last = options.len() as i32 - 1;
                self.set_setting(id, (i32::from(*index) + delta).clamp(0, last))
            }
            (SettingKind::Toggle, Some(_)) => self.set_setting(id, i32::from(delta > 0)),
            _ => Vec::new(),
        }
    }

    /// Hold OUT 1/2 or OUT 3/4 (manual §3 OUTPUT PAIRING). `None` for other ports or an
    /// unknown pairing, so the press stays a tap.
    pub(crate) fn pair_outputs(&self, tab: IoTab) -> Option<Vec<UiCommand>> {
        let jack = match tab {
            IoTab::Out12 => OutputJack::Out12,
            IoTab::Out34 => OutputJack::Out34,
            _ => return None,
        };
        let id = SettingId::OutputPair(jack);
        let Some(SettingValue::Bool(linked)) = self.snapshot.setting(id) else {
            return None;
        };
        Some(vec![UiCommand::ChangeSetting {
            id,
            value: SettingValue::Bool(!linked),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn io_ui(tab: IoTab) -> UiState {
        let mut snapshot = UiSnapshot::default();
        for (id, value) in [
            (
                SettingId::InputType(InputJack::In1),
                SettingValue::Choice(0),
            ),
            (
                SettingId::Phantom(InputJack::In1),
                SettingValue::Bool(false),
            ),
            (
                SettingId::InputLevel(InputJack::In1),
                SettingValue::Number(0),
            ),
            (
                SettingId::InputImpedance(InputJack::In1),
                SettingValue::Choice(7),
            ),
            (
                SettingId::OutputPair(OutputJack::Out12),
                SettingValue::Bool(true),
            ),
        ] {
            snapshot.settings.insert(id, value);
        }
        let mut ui = UiState::new();
        ui.set_snapshot(snapshot).expect("valid snapshot");
        ui.navigate(Screen::Io(tab));
        ui
    }

    /// Manual §3 / `0x3594fc`: PHANTOM is dim on Instrument; encoders follow reading order.
    #[test]
    fn combo_jack_rules_gate_cells_and_encoders_follow_the_card() {
        let mut ui = io_ui(IoTab::Input1);
        let phantom = Control::Setting(SettingControl::Set(SettingId::Phantom(InputJack::In1), 1));
        assert!(
            ui.hit_regions()
                .iter()
                .any(|r| r.control == phantom && !r.enabled)
        );
        let knob = |n| KnobIndex::new(n).expect("knob");
        assert_eq!(
            ui.handle_input(InputEvent::Knob {
                knob: knob(0),
                delta: 2
            }),
            vec![UiCommand::ChangeSetting {
                id: SettingId::InputLevel(InputJack::In1),
                value: SettingValue::Number(10),
            }]
        );
        // IMPEDANCE clamps at 10M instead of wrapping to 10k.
        assert_eq!(
            ui.handle_input(InputEvent::Knob {
                knob: knob(1),
                delta: 5
            }),
            vec![UiCommand::ChangeSetting {
                id: SettingId::InputImpedance(InputJack::In1),
                value: SettingValue::Choice(8),
            }]
        );
        assert!(
            ui.handle_input(InputEvent::Knob {
                knob: knob(3),
                delta: 1
            })
            .is_empty()
        );
    }

    /// `0x35a7f4`/`0x3597ac`: one feed knob per distinct lane output in lane order, its meter
    /// below it; encoders follow the knobs.
    #[test]
    fn headphones_card_shows_one_feed_per_distinct_lane_output() {
        let mut snapshot = UiSnapshot::default();
        for (row, output) in [
            OutputPort::Xlr1And2,
            OutputPort::Send1,
            OutputPort::Xlr1And2,
            OutputPort::MultipleOuts,
        ]
        .into_iter()
        .enumerate()
        {
            snapshot.rows[row].output = output;
        }
        let feed = SettingId::HeadphoneFeed(HpFeed::MultiOut);
        snapshot.settings.insert(feed, SettingValue::Number(60));
        let [top, bottom] = IoTab::Headphones.cells(&snapshot.rows);
        let feeds = [HpFeed::Out12, HpFeed::Send1, HpFeed::MultiOut];
        let knobs = feeds.map(|feed| IoCell::Setting(SettingId::HeadphoneFeed(feed)));
        assert_eq!(top[1..], knobs);
        assert_eq!(bottom[1..], feeds.map(IoCell::FeedMeter));
        let mut ui = UiState::new();
        ui.set_snapshot(snapshot).expect("valid snapshot");
        ui.navigate(Screen::Io(IoTab::Headphones));
        assert_eq!(
            ui.handle_input(InputEvent::Knob {
                knob: KnobIndex::new(3).expect("knob"),
                delta: 1
            }),
            vec![UiCommand::ChangeSetting {
                id: feed,
                value: SettingValue::Number(65),
            }]
        );
    }

    /// Manual §3 OUTPUT PAIRING: holding the OUT 1/2 port toggles the stereo link.
    #[test]
    fn holding_an_output_port_toggles_its_pairing() {
        let mut ui = io_ui(IoTab::Input1);
        let port = ui
            .hit_regions()
            .into_iter()
            .find(|r| r.control == Control::Setting(SettingControl::IoTab(IoTab::Out12)))
            .expect("OUT 1/2 port");
        let (x, y) = port.bounds.center();
        ui.handle_input(InputEvent::PointerDown { x, y });
        // A 750 ms hold (`LONG_PRESS_MS`); the test held 600 ms over the old 500 ms.
        assert_eq!(
            ui.handle_input(InputEvent::Tick {
                now_ms: crate::state::LONG_PRESS_MS,
            }),
            vec![UiCommand::ChangeSetting {
                id: SettingId::OutputPair(OutputJack::Out12),
                value: SettingValue::Bool(false),
            }]
        );
        assert!(ui.handle_input(InputEvent::PointerUp { x, y }).is_empty());
        assert_eq!(ui.screen(), Screen::Io(IoTab::Input1));
    }
}

//! Global EQ (I/O Settings → GLOBAL EQ, manual §3, shot 009): "GLOBAL EQ" + "Parametric-5"
//! header, ON button, the response graph with numbered band nodes, band tabs 1–5 and OUT,
//! and a one-row parameter card. Encoders turn the cells left to right. The graph, tabs and
//! card are shared with the block EQ editor (`EQModelEditor` `0x640440`, `eq_editor.rs`).

use tetrad_assets::{
    ACCENT, CANVAS, DIVIDER, FAINT, Icon, MUTED, PANEL, Rgb, SURFACE, TEXT, block,
};

use crate::layout::{BODY, ButtonStyle, CAPTION, Layout, Look, MEDIUM};
use crate::render::{Align, HEIGHT, WIDTH};
use crate::settings::{EqBand, EqTab, OutputJack, SettingKind};
use crate::state::DOUBLE_TAP_MS;
use crate::widgets::PAGE;
use crate::*;

/// Graph band (shot 009): 20 Hz at x 0, 264 px per decade, 0 dB at y 185, 9 px per dB.
/// Stock (`0x4f90f0`): 20 Hz · 1000^(x / (w − 9)), ±12 dB (`0x4f890c` `+0x148`/`+0x14c`).
pub(super) const GRAPH: Rect = Rect::new(0, 60, WIDTH, 250);
const PX_PER_DECADE: f32 = 264.0;
/// Band tabs: `zenUI::TabBar` at (24, 245) of the editor, 80 px tabs (`0x640440`).
const TAB_Y: i32 = 313;
pub(super) const CARD: Rect = Rect::new(8, 366, 784, 106);
pub(super) const CELL_WIDTH: i32 = 157;
/// Stock 009: a split pill, a 12 px `#1e1e1e` part with the green LED, then a
/// `#2e2e2e` part with a grey "ON".
const ON: Rect = Rect::new(606, 8, 80, 44);
/// Selected node fill (`0x4f6d08`: `#0a74e0` for a selected band that is on).
const NODE_BLUE: Rgb = Rgb(0x0a, 0x74, 0xe0);

fn x_of(hz: f32) -> f32 {
    PX_PER_DECADE * (hz / 20.0).log10()
}

/// 0 dB is the middle of a graph frame; shot 009 gives 9 px per dB on the 250 px frame.
fn zero_y(frame: Rect) -> f32 {
    frame.y as f32 + frame.height as f32 * 0.5
}

fn px_per_db(frame: Rect) -> f32 {
    9.0 * frame.height as f32 / GRAPH.height as f32
}

pub(super) fn y_of(frame: Rect, db: f32) -> f32 {
    (zero_y(frame) - db * px_per_db(frame)).clamp(frame.y as f32, frame.bottom() as f32 - 1.0)
}

/// Frequency and gain at a point of `frame` (screen coordinates), clamped to
/// 20 Hz..20 kHz and ±12 dB.
pub(super) fn point_value(frame: Rect, x: i32, y: i32) -> (f32, f32) {
    let hz = 20.0 * 10f32.powf(x as f32 / PX_PER_DECADE);
    let db = (zero_y(frame) - y as f32) / px_per_db(frame);
    (hz.clamp(20.0, 20_000.0), db.clamp(-12.0, 12.0))
}

fn number(ui: &UiState, id: SettingId) -> Option<f32> {
    match (id.kind(), ui.snapshot.setting(id)) {
        (SettingKind::Number { decimals, .. }, Some(SettingValue::Number(raw))) => {
            Some(*raw as f32 / 10f32.powi(i32::from(decimals)))
        }
        _ => None,
    }
}

/// Filter types. Block TYPE index order (`floor(4 · value)`, `0x6074bc`; names `0x639f34`,
/// icons `0x639df4`): PEAK, HI PASS, LO PASS, HI SHELF, LO SHELF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shape {
    Peak,
    HighPass,
    LowPass,
    HighShelf,
    LowShelf,
}

impl Shape {
    pub(super) const BLOCK_ORDER: [Self; 5] = [
        Self::Peak,
        Self::HighPass,
        Self::LowPass,
        Self::HighShelf,
        Self::LowShelf,
    ];
    /// The Global EQ setting's option order.
    const SETTING_ORDER: [Self; 5] = [
        Self::HighPass,
        Self::LowPass,
        Self::HighShelf,
        Self::LowShelf,
        Self::Peak,
    ];

    /// Pass types have no GAIN or Q (`0x63b7d4`; shot 009 dims both on HI PASS).
    pub(super) fn is_pass(self) -> bool {
        matches!(self, Self::HighPass | Self::LowPass)
    }

    pub(super) fn icon(self) -> Icon {
        match self {
            Self::Peak => Icon::EqPeak,
            Self::HighPass => Icon::EqHighPass,
            Self::LowPass => Icon::EqLowPass,
            Self::HighShelf => Icon::EqHighShelf,
            Self::LowShelf => Icon::EqLowShelf,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Peak => "PEAK",
            Self::HighPass => "HI PASS",
            Self::LowPass => "LO PASS",
            Self::HighShelf => "HI SHELF",
            Self::LowShelf => "LO SHELF",
        }
    }
}

/// One band as the graph draws it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Band {
    pub shape: Shape,
    pub gain: f32,
    pub hz: f32,
    pub q: f32,
    pub on: bool,
}

fn band(ui: &UiState, band: EqBand) -> Option<Band> {
    let SettingValue::Choice(kind) = ui.snapshot.setting(SettingId::EqType(band))? else {
        return None;
    };
    Some(Band {
        shape: *Shape::SETTING_ORDER.get(usize::from(*kind))?,
        gain: number(ui, SettingId::EqGain(band))?,
        hz: number(ui, SettingId::EqFreq(band))?,
        q: number(ui, SettingId::EqQ(band))?,
        on: ui.snapshot.setting(SettingId::EqBypass(band)) != Some(&SettingValue::Bool(true)),
    })
}

/// Display-only response in dB of the bands that are on (owned shapes; the DSP filter is
/// the stock's): second-order passes, smooth shelves and a bell whose width follows Q.
pub(super) fn response(bands: &[Band], hz: f32) -> f32 {
    bands
        .iter()
        .filter(|band| band.on)
        .map(|band| {
            let octaves = (hz / band.hz).log2();
            let ratio = (hz / band.hz).powi(4);
            match band.shape {
                Shape::HighPass => -10.0 * (1.0 + 1.0 / ratio).log10(),
                Shape::LowPass => -10.0 * (1.0 + ratio).log10(),
                Shape::HighShelf => band.gain / (1.0 + (-4.0 * octaves).exp()),
                Shape::LowShelf => band.gain / (1.0 + (4.0 * octaves).exp()),
                Shape::Peak => band.gain * (-(octaves * band.q).powi(2) * 2.0).exp(),
            }
        })
        .sum()
}

pub(super) fn layout(ui: &UiState, l: &mut Layout, tab: EqTab) {
    // Stock 009 page and graph: `#101010`.
    l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(PAGE));
    // Stock 4.1.0 preset button (`GlobalEQPresetPopup` `0x227a84`; shot 009 is older and
    // shows ⋮).
    super::settings_presets::button(ui, l, crate::SettingsPresetPage::GlobalEq);
    l.text(
        Rect::new(60, 8, 200, 20),
        "GLOBAL EQ",
        CAPTION,
        MUTED,
        Align::Left,
    );
    l.text(
        Rect::new(60, 26, 240, 26),
        "Parametric-5",
        MEDIUM,
        block::EQ,
        Align::Left,
    );
    let enabled = ui.snapshot.setting(SettingId::EqEnabled);
    let on = enabled == Some(&SettingValue::Bool(true));
    let label = Rect::new(ON.x + 12, ON.y, ON.width - 12, ON.height);
    l.panel(ON, SURFACE, 8.0, None);
    l.panel(label, PANEL, 8.0, None);
    l.push(
        Rect::new(label.x, ON.y, 8, ON.height),
        "",
        Look::Fill(PANEL),
    );
    l.control(
        ON,
        Control::Setting(SettingControl::Open(SettingId::EqEnabled)),
        enabled.is_some(),
        "ON",
        Look::Area,
    );
    l.panel(
        Rect::new(ON.x + 3, ON.y + 13, 6, 18),
        if on { ACCENT } else { FAINT },
        3.0,
        None,
    );
    // Stock text centre is x 653, 1 px right of the part's centre.
    l.text(
        Rect::new(label.x + 2, ON.y, label.width, ON.height),
        "ON",
        MEDIUM,
        if enabled.is_some() { MUTED } else { FAINT },
        Align::Center,
    );
    l.control(
        crate::widgets::CLOSE_BUTTON,
        Control::Back,
        true,
        "CLOSE",
        Look::IconButton {
            icon: Icon::Check,
            fill: PANEL,
        },
    );
    graph(ui, l, tab);
    let tabs = EqBand::all().map(EqTab::Band).chain([EqTab::Out]);
    for (index, entry) in tabs.enumerate() {
        band_tab(
            l,
            index,
            ["1", "2", "3", "4", "5", "OUT"][index],
            entry == tab,
            Control::Navigate(Screen::GlobalEq(entry)),
        );
    }
    card(l);
    for (column, id) in cells(tab).into_iter().enumerate() {
        cell(ui, l, card_cell(column), id);
    }
}

/// Band tab `index` (shot 009): 80 × 52 on an 84 px pitch; the selected tab is the card
/// colour and runs 8 px down into the card.
pub(super) fn band_tab(
    l: &mut Layout,
    index: usize,
    label: &str,
    selected: bool,
    control: Control,
) {
    let bounds = Rect::new(24 + index as i32 * 84, TAB_Y, 80, 52);
    l.panel(
        Rect::new(
            bounds.x,
            bounds.y,
            bounds.width,
            if selected { 60 } else { 52 },
        ),
        // Shot 009: selected `#282c28` (the card), others `#181c18`.
        if selected {
            crate::widgets::CARD
        } else {
            crate::draw::TAB_IDLE
        },
        8.0,
        None,
    );
    l.control(bounds, control, true, label, Look::Area);
    l.text(
        bounds,
        label,
        BODY,
        if selected { TEXT } else { MUTED },
        Align::Center,
    );
}

/// The parameter card with its four dividers (`#121212`, 1 × 78, `0x640440`).
pub(super) fn card(l: &mut Layout) {
    l.panel(CARD, PANEL, 8.0, None);
    for column in 1..5 {
        let x = CARD.x + column * CELL_WIDTH;
        l.push(
            Rect::new(x - 1, CARD.y + 8, 2, CARD.height - 16),
            "",
            Look::Fill(DIVIDER),
        );
    }
}

pub(super) fn card_cell(column: usize) -> Rect {
    Rect::new(
        CARD.x + column as i32 * CELL_WIDTH,
        CARD.y,
        CELL_WIDTH as u32,
        CARD.height,
    )
}

/// BYPASS cell (shot 009): the label and a 66 × 44 power button. Not bypassed: plain
/// `#1e1e1e` with a white ⏻; bypassed takes the green Global Bypass look (045, I).
pub(super) fn bypass_cell(
    l: &mut Layout,
    bounds: Rect,
    label: &str,
    bypassed: bool,
    enabled: bool,
    control: Control,
) {
    l.text(
        Rect::new(bounds.x + 10, bounds.y + 6, bounds.width - 20, 28),
        label,
        BODY,
        if enabled { TEXT } else { FAINT },
        Align::Left,
    );
    let button = Rect::new(bounds.x + 10, bounds.y + 48, 66, 44);
    let look = if bypassed {
        Look::Button {
            style: ButtonStyle::Tab { selected: true },
            icon: Some(Icon::Power),
            font: None,
        }
    } else {
        Look::IconButton {
            icon: Icon::Power,
            fill: SURFACE,
        }
    };
    l.control(button, control, enabled, label, look);
}

fn cells(tab: EqTab) -> Vec<SettingId> {
    match tab {
        EqTab::Band(band) => vec![
            SettingId::EqType(band),
            SettingId::EqGain(band),
            SettingId::EqFreq(band),
            SettingId::EqQ(band),
            SettingId::EqBypass(band),
        ],
        EqTab::Out => vec![
            SettingId::EqOutput,
            SettingId::EqAssign(OutputJack::Out12),
            SettingId::EqAssign(OutputJack::Out34),
        ],
    }
}

/// GAIN and Q are dim for the pass types.
fn cell_enabled(ui: &UiState, id: SettingId) -> bool {
    let pass = |id| band(ui, id).is_some_and(|band| band.shape.is_pass());
    ui.snapshot.setting(id).is_some()
        && match id {
            SettingId::EqGain(band) | SettingId::EqQ(band) => !pass(band),
            _ => true,
        }
}

fn cell(ui: &UiState, l: &mut Layout, bounds: Rect, id: SettingId) {
    let value = ui.snapshot.setting(id);
    let enabled = cell_enabled(ui, id);
    let control = Control::Setting(SettingControl::Open(id));
    match id.kind() {
        SettingKind::Choice(_) => l.control(
            bounds,
            control,
            enabled,
            id.label(),
            Look::Knob {
                value: id.format(value),
                kind: match (id, value) {
                    // Filter-shape glyph in stock option order (shot 009).
                    (SettingId::EqType(_), Some(SettingValue::Choice(kind))) => {
                        Shape::SETTING_ORDER.get(usize::from(*kind)).map_or(
                            ParameterKind::Choice {
                                position: 0.0,
                                steps: 0,
                            },
                            |shape| ParameterKind::IconChoice { icon: shape.icon() },
                        )
                    }
                    _ => ParameterKind::Choice {
                        position: 0.0,
                        steps: 0,
                    },
                },
                color: block::EQ,
                focused: false,
            },
        ),
        SettingKind::Number { .. } => {
            // Stock 009: the label sits 1 px higher than the shared cell origin gives (y 379);
            // `draw.rs` moves the dial and value of these knobs down (ring y 414, value 446).
            let bounds = Rect::new(bounds.x, bounds.y - 1, bounds.width, bounds.height + 1);
            let position = match value {
                Some(SettingValue::Number(raw)) => id.knob_position(*raw),
                _ => 0.0,
            };
            l.control(
                bounds,
                control,
                enabled,
                id.label(),
                Look::Knob {
                    value: id.format(value),
                    kind: ParameterKind::Continuous { position },
                    color: block::EQ,
                    focused: false,
                },
            );
        }
        SettingKind::Toggle if matches!(id, SettingId::EqBypass(_)) => bypass_cell(
            l,
            bounds,
            id.label(),
            value == Some(&SettingValue::Bool(true)),
            enabled,
            control,
        ),
        SettingKind::Toggle => l.control(
            bounds,
            control,
            enabled,
            id.label(),
            Look::Knob {
                value: String::new(),
                kind: ParameterKind::Toggle {
                    on: value == Some(&SettingValue::Bool(true)),
                },
                color: block::EQ,
                focused: false,
            },
        ),
    }
}

fn graph(ui: &UiState, l: &mut Layout, tab: EqTab) {
    let bands: Vec<(usize, Band)> = EqBand::all()
        .filter_map(|id| band(ui, id).map(|band| (usize::from(id.get()), band)))
        .collect();
    let curve: Vec<Band> = bands.iter().map(|(_, band)| *band).collect();
    let nodes: Vec<Node> = bands
        .iter()
        .map(|&(index, band)| Node {
            label: (index + 1).to_string(),
            hz: band.hz,
            db: node_db(&curve, band),
            on: band.on,
            selected: matches!(tab, EqTab::Band(id) if usize::from(id.get()) == index),
            // Nodes overlap (shot 009: 4 and 5), so the band tabs, not the nodes, select.
            control: None,
        })
        .collect();
    draw_graph(l, GRAPH, &curve, &nodes);
}

/// The node height of a band: on its gain, or on the curve for the pass types.
pub(super) fn node_db(curve: &[Band], band: Band) -> f32 {
    if band.shape.is_pass() {
        response(curve, band.hz)
    } else {
        band.gain
    }
}

/// A graph node (`EQGraphControlPoint`, paint `0x4f6d08`).
pub(super) struct Node {
    pub label: String,
    pub hz: f32,
    pub db: f32,
    pub on: bool,
    pub selected: bool,
    pub control: Option<Control>,
}

/// The response graph (`EQGraph` `0x4f890c`) in `frame`: decade grid and labels, the 0 dB
/// line, the curve of the `curve` bands that are on, then the nodes (`0x4f6d08`). A node
/// shows when it is on or selected: selected 52 px (`#0a74e0`, black label; `#121212`
/// while off), others 38 px `#2e2e2e` with a white label and a `#121212` ring when on.
/// The selected node is on top.
pub(super) fn draw_graph(l: &mut Layout, frame: Rect, curve: &[Band], nodes: &[Node]) {
    l.push(frame, "", Look::Fill(PAGE));
    for decade in [100.0f32, 1_000.0, 10_000.0] {
        for step in 1..10 {
            let x = x_of(decade / 10.0 * step as f32);
            if (0.0..WIDTH as f32).contains(&x) {
                let major = step == 1;
                l.line(
                    (x, frame.y as f32),
                    (x, frame.bottom() as f32),
                    1.0,
                    if major { FAINT } else { SURFACE },
                );
            }
        }
        let label = match decade as u32 {
            100 => "100",
            1_000 => "1k",
            _ => "10k",
        };
        l.text(
            Rect::new(x_of(decade) as i32 + 6, frame.y + 2, 40, 16),
            label,
            CAPTION,
            FAINT,
            Align::Left,
        );
    }
    let zero = zero_y(frame);
    l.line((0.0, zero), (WIDTH as f32, zero), 1.0, SURFACE);
    let hz_at = |x: f32| 20.0 * 10f32.powf(x / PX_PER_DECADE);
    if !curve.is_empty() {
        let mut previous = (0.0, y_of(frame, response(curve, hz_at(0.0))));
        for step in 1..=100 {
            let x = WIDTH as f32 * step as f32 / 100.0;
            let point = (x, y_of(frame, response(curve, hz_at(x))));
            l.line(previous, point, 2.0, TEXT);
            previous = point;
        }
    }
    let order = (nodes.iter().filter(|node| !node.selected))
        .chain(nodes.iter().filter(|node| node.selected));
    for node in order {
        if !node.on && !node.selected {
            continue;
        }
        let radius = if node.selected { 26 } else { 19 };
        let (x, y) = (x_of(node.hz) as i32, y_of(frame, node.db) as i32);
        let bounds = Rect::new(x - radius, y - radius, radius as u32 * 2, radius as u32 * 2);
        let (fill, text) = match (node.selected, node.on) {
            (true, true) => (NODE_BLUE, CANVAS),
            (true, false) => (DIVIDER, TEXT),
            _ => (PANEL, TEXT),
        };
        if node.on && !node.selected {
            let ring = Rect::new(
                x - radius - 2,
                y - radius - 2,
                bounds.width + 4,
                bounds.height + 4,
            );
            l.push(ring, "", Look::Dot { color: DIVIDER });
        }
        l.push(bounds, "", Look::Dot { color: fill });
        l.text(bounds, node.label.clone(), BODY, text, Align::Center);
        if let Some(control) = node.control {
            l.control(
                bounds,
                control,
                true,
                format!("NODE {}", node.label),
                Look::Area,
            );
        }
    }
}

impl UiState {
    /// Encoders turn the card cells left to right. FREQ moves a twelfth of an octave per
    /// detent (owned: a linear Hz step is unusable across 20 Hz..20 kHz).
    /// The encoder of a Global EQ card cell (cell n is encoder n).
    pub(crate) fn eq_knob_of(&self, tab: EqTab, id: SettingId) -> Option<KnobIndex> {
        let index = cells(tab).iter().position(|cell| *cell == id)?;
        KnobIndex::new(index as u8).ok()
    }

    /// A tap on a Global EQ value cell; the second tap within [`DOUBLE_TAP_MS`] restores the
    /// default preset's value (manual "Global EQ MENU").
    pub(crate) fn eq_tap(&mut self, tab: EqTab, id: SettingId) -> Vec<UiCommand> {
        let Some(knob) = self.eq_knob_of(tab, id) else {
            return Vec::new();
        };
        let double = (self.knob_tap)
            .is_some_and(|(last, at)| last == knob && self.now.saturating_sub(at) <= DOUBLE_TAP_MS);
        if !double {
            self.knob_tap = Some((knob, self.now));
            return Vec::new();
        }
        self.knob_tap = None;
        vec![UiCommand::SettingsPreset {
            page: SettingsPresetPage::GlobalEq,
            request: SettingsPresetRequest::RestoreDefault(id),
        }]
    }

    pub(crate) fn eq_knob(&mut self, tab: EqTab, knob: KnobIndex, delta: i32) -> Vec<UiCommand> {
        let Some(id) = cells(tab).get(usize::from(knob.get())).copied() else {
            return Vec::new();
        };
        if !cell_enabled(self, id) {
            return Vec::new();
        }
        match (id, self.snapshot.setting(id)) {
            (SettingId::EqFreq(_), Some(SettingValue::Number(hz))) => {
                let next = (*hz as f32 * 2f32.powf(delta as f32 / 12.0)).round() as i32;
                let next = if next == *hz {
                    hz + delta.signum()
                } else {
                    next
                };
                self.set_setting(id, next)
            }
            (_, Some(SettingValue::Choice(index))) => {
                self.set_setting(id, i32::from(*index) + delta)
            }
            (_, Some(SettingValue::Bool(_))) => self.set_setting(id, i32::from(delta > 0)),
            (_, value @ Some(_)) => id
                .step(value, delta)
                .map(|value| vec![UiCommand::ChangeSetting { id, value }])
                .unwrap_or_default(),
            (_, None) => Vec::new(),
        }
    }
}

//! EQ block editors. `EQModelEditor` (`0x640440`; Parametric-8 4000, Parametric-3 4001,
//! Output Equalizer 4004): the response graph with one node per band, band tabs and OUT,
//! and the band card TYPE | GAIN | FREQ | Q | BYPASS n (shot 009 shows the same editor as the
//! Global EQ). `GraphicEQModelEditor` (`0x2c6984`; Graphic-9 4005–4007): page 1 the nine band
//! faders, page 2 the `EqPanel` (`0x81dd9c`) with HPF, LPF, OUTPUT (and Q on Nolly) over a
//! graph with "H" and "L" nodes. Graph, tabs and card are shared with `global_eq.rs`.

use tetrad_assets::{BACKGROUND, FAINT, IconSize, MUTED, PANEL, Rgb, SURFACE, TEXT, block};

use super::global_eq::{self, Band, Node, Shape};
use super::model_editor::{BODY_TOP, ModelEditor, parameter};
use crate::layout::{BODY, CAPTION, Layout, Look};
use crate::render::{Align, WIDTH};
use crate::state::{ChoicePurpose, DOUBLE_TAP_MS};
use crate::{
    Control, EditorControl, KnobIndex, ParameterKind, ParameterTarget, ParameterView, Rect,
    UiCommand, UiState,
};

/// Band parameters: GAIN, FREQ, Q, TYPE, BYPASS at 5·band + 0..=4; OUTPUT at 5·bands.
const GAIN: u16 = 0;
const FREQ: u16 = 1;
const Q: u16 = 2;
const TYPE: u16 = 3;
const BYPASS: u16 = 4;

/// Graphic-9 parameters: HPF, 9 bands (65 Hz … 16 kHz), LPF, Q (hidden; Nolly), OUTPUT.
const HPF: u16 = 0;
const LPF: u16 = 10;
const GEQ_Q: u16 = 11;
const GEQ_OUTPUT: u16 = 12;
const GEQ_BANDS: [f32; 9] = [
    65.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];
/// Band faders (`0x2c6984`): 72 px columns on an 88 px pitch from x 8; owned track below
/// the label and value.
const FADER_X: i32 = 8;
const FADER_PITCH: i32 = 88;
const FADER_WIDTH: u32 = 72;
const FADER_TRACK: Rect = Rect::new(0, BODY_TOP + 52, 40, 356);
/// Fader colour (`0x2c6984`: `#0a74e0`).
const FADER_FILL: Rgb = Rgb(0x0a, 0x74, 0xe0);
/// `EqPanel` strip 784 × 107 at (8, 7) and graph 800 × 297 at (0, 115).
const STRIP: Rect = Rect::new(8, BODY_TOP + 7, 784, 107);
const PANEL_GRAPH: Rect = Rect::new(0, BODY_TOP + 115, WIDTH, 297);
/// `EQFilterTypeSelector` (`0x6426d8`): a 160 × 300 `#121212` table at (472, 16) of the
/// editor, 60 px rows.
const TYPE_TABLE: Rect = Rect::new(472, BODY_TOP + 16, 160, 300);
const TYPE_ROW: i32 = 60;

fn band_parameter(band: u8, offset: u16) -> u16 {
    5 * u16::from(band) + offset
}

/// The display value (Hz, dB, Q) of a continuous parameter.
fn display(ui: &UiState, index: u16) -> Option<f32> {
    parameter(ui, index)?.entry.map(|entry| entry.value)
}

fn position(p: &ParameterView) -> f32 {
    match p.kind {
        ParameterKind::Continuous { position }
        | ParameterKind::Centred { position }
        | ParameterKind::Int { position } => position,
        ParameterKind::Toggle { on } => f32::from(u8::from(on)),
        _ => 0.0,
    }
}

/// TYPE index: `floor(4 · value)`, 0 above 4 (`0x6074bc`).
fn type_index(ui: &UiState, band: u8) -> usize {
    parameter(ui, band_parameter(band, TYPE)).map_or(0, |p| {
        let index = (position(p) * 4.0 + 1e-3).floor() as usize;
        if index > 4 { 0 } else { index }
    })
}

fn band_on(ui: &UiState, band: u8) -> bool {
    matches!(
        parameter(ui, band_parameter(band, BYPASS)).map(|p| p.kind),
        Some(ParameterKind::Toggle { on: true })
    )
}

fn band(ui: &UiState, band: u8) -> Option<Band> {
    Some(Band {
        shape: Shape::BLOCK_ORDER[type_index(ui, band)],
        gain: display(ui, band_parameter(band, GAIN)).unwrap_or(0.0),
        hz: display(ui, band_parameter(band, FREQ))?,
        q: display(ui, band_parameter(band, Q)).unwrap_or(0.707),
        on: band_on(ui, band),
    })
}

/// A knob of the band card is live (`0x63b7d4`): every knob needs the band on; GAIN and Q
/// also need a type with gain (not HI PASS or LO PASS).
fn knob_enabled(ui: &UiState, band: u8, offset: u16) -> bool {
    band_on(ui, band)
        && (!matches!(offset, GAIN | Q) || !Shape::BLOCK_ORDER[type_index(ui, band)].is_pass())
}

/// The selected tab, clamped to OUT (= `bands`).
fn tab(ui: &UiState, bands: u8) -> u8 {
    ui.eq_band.min(bands)
}

pub(super) fn eq(ui: &UiState, l: &mut Layout, bands: u8, output: u16) {
    let tab = tab(ui, bands);
    let curve: Vec<Band> = (0..bands).filter_map(|b| band(ui, b)).collect();
    let nodes: Vec<Node> = (0..bands)
        .filter_map(|b| {
            let shape = band(ui, b)?;
            Some(Node {
                label: (b + 1).to_string(),
                hz: shape.hz,
                db: global_eq::node_db(&curve, shape),
                on: shape.on,
                selected: b == tab,
                control: Some(Control::Editor(EditorControl::EqNode(b))),
            })
        })
        .collect();
    global_eq::draw_graph(l, global_eq::GRAPH, &curve, &nodes);
    // The graph ends at y 310; the strip under it up to the card is the page colour.
    l.push(
        Rect::new(0, global_eq::GRAPH.bottom(), WIDTH, 170),
        "",
        Look::Fill(BACKGROUND),
    );
    for index in 0..=bands {
        let label = if index == bands {
            "OUT".to_owned()
        } else {
            (index + 1).to_string()
        };
        global_eq::band_tab(
            l,
            usize::from(index),
            &label,
            index == tab,
            Control::Editor(EditorControl::EqTab(index)),
        );
    }
    global_eq::card(l);
    if tab == bands {
        // OUT: OUTPUT is the fourth knob (x 628 of the card, `0x63a984`).
        if let Some(p) = parameter(ui, output) {
            knob_cell(l, global_eq::card_cell(4), p, &p.label, true);
        }
        return;
    }
    type_cell(ui, l, tab);
    // Stock labels the knobs "GAIN", "FREQ", "Q" (`0x640440`), not the band names.
    for (column, offset, label) in [(1, GAIN, "GAIN"), (2, FREQ, "FREQ"), (3, Q, "Q")] {
        if let Some(p) = parameter(ui, band_parameter(tab, offset)) {
            let enabled = knob_enabled(ui, tab, offset);
            knob_cell(l, global_eq::card_cell(column), p, label, enabled);
        }
    }
    global_eq::bypass_cell(
        l,
        global_eq::card_cell(4),
        &format!("BYPASS {}", tab + 1),
        !band_on(ui, tab),
        parameter(ui, band_parameter(tab, BYPASS)).is_some(),
        Control::Editor(EditorControl::EqBypass),
    );
}

/// TYPE cell: "TYPE" and the black type button with the filter icon and name.
fn type_cell(ui: &UiState, l: &mut Layout, band: u8) {
    let shape = Shape::BLOCK_ORDER[type_index(ui, band)];
    l.control(
        global_eq::card_cell(0),
        Control::Editor(EditorControl::EqType),
        band_on(ui, band) && parameter(ui, band_parameter(band, TYPE)).is_some(),
        "TYPE",
        Look::Knob {
            value: shape.label().into(),
            kind: ParameterKind::IconChoice { icon: shape.icon() },
            color: block::EQ,
            focused: false,
        },
    );
}

fn knob_cell(l: &mut Layout, bounds: Rect, p: &ParameterView, label: &str, enabled: bool) {
    // As the Global EQ card: the label sits 1 px above the shared cell origin.
    let bounds = Rect::new(bounds.x, bounds.y - 1, bounds.width, bounds.height + 1);
    l.control(
        bounds,
        Control::Editor(EditorControl::Knob(p.index)),
        enabled,
        label,
        Look::Knob {
            value: p.value.clone(),
            kind: p.kind,
            color: block::EQ,
            focused: false,
        },
    );
    if p.scene {
        crate::widgets::scene_mark(l, bounds.x + 10, bounds.y + 36);
    }
}

/// The filter type table (`0x6426d8`): five 60 px rows, icon and name; a cell sets TYPE to
/// cell / 4 (`onSelectFilterTypeTableCellSlot` `0x63d32c`).
pub(super) fn filter_type_selector(ui: &UiState, l: &mut Layout, parameter_index: u16) {
    let current = parameter(ui, parameter_index)
        .map(|p| ((position(p) * 4.0 + 1e-3).floor() as usize).min(4));
    l.popover(TYPE_TABLE);
    l.panel(TYPE_TABLE, BACKGROUND, 8.0, None);
    for (index, shape) in Shape::BLOCK_ORDER.into_iter().enumerate() {
        let row = Rect::new(
            TYPE_TABLE.x,
            TYPE_TABLE.y + index as i32 * TYPE_ROW,
            TYPE_TABLE.width,
            TYPE_ROW as u32,
        );
        if current == Some(index) {
            l.panel(row.inset(4, 4), SURFACE, 6.0, None);
        }
        l.control(
            row,
            Control::Choose(index as u16),
            true,
            shape.label(),
            Look::Area,
        );
        l.push(
            Rect::new(row.x + 12, row.y, 24, row.height),
            "",
            Look::Glyph {
                icon: shape.icon(),
                size: IconSize::Small,
                color: TEXT,
            },
        );
        l.text(
            Rect::new(row.x + 48, row.y, row.width - 56, row.height),
            shape.label(),
            BODY,
            TEXT,
            Align::Left,
        );
    }
}

// --- Graphic EQ ------------------------------------------------------------------------

fn fader_rect(band: usize) -> Rect {
    Rect::new(
        FADER_X + band as i32 * FADER_PITCH,
        BODY_TOP,
        FADER_WIDTH,
        420,
    )
}

/// Gain in dB for a touch at `y` on a fader track (−12..=12, 0 dB in the middle).
fn fader_db(y: i32) -> f32 {
    let half = FADER_TRACK.height as f32 / 2.0;
    let centre = FADER_TRACK.y as f32 + half;
    ((centre - y as f32) / half * 12.0).clamp(-12.0, 12.0)
}

pub(super) fn graphic(ui: &UiState, l: &mut Layout, nolly: bool) {
    l.push(Rect::new(0, BODY_TOP, WIDTH, 420), "", Look::Fill(PANEL));
    if ui.param_page == 1 {
        graphic_panel(ui, l, nolly);
        return;
    }
    for band in 0..GEQ_BANDS.len() {
        let Some(p) = parameter(ui, 1 + band as u16) else {
            continue;
        };
        let column = fader_rect(band);
        l.text(
            Rect::new(column.x, BODY_TOP + 8, FADER_WIDTH, 20),
            p.label.clone(),
            CAPTION,
            MUTED,
            Align::Center,
        );
        l.text(
            Rect::new(column.x, BODY_TOP + 26, FADER_WIDTH, 20),
            p.value.clone(),
            CAPTION,
            TEXT,
            Align::Center,
        );
        let track = Rect::new(
            column.x + 16,
            FADER_TRACK.y,
            FADER_TRACK.width,
            FADER_TRACK.height,
        );
        l.panel(track, BACKGROUND, 6.0, None);
        let centre = track.y + track.height as i32 / 2;
        let level = (centre as f32 - (position(p) - 0.5) * track.height as f32).round() as i32;
        let (top, bottom) = (level.min(centre), level.max(centre));
        if bottom > top {
            l.panel(
                Rect::new(track.x, top, track.width, (bottom - top) as u32),
                FADER_FILL,
                4.0,
                None,
            );
        }
        l.push(
            Rect::new(track.x - 6, centre, track.width + 12, 1),
            "",
            Look::Fill(FAINT),
        );
        l.control(
            Rect::new(column.x, track.y - 8, FADER_WIDTH, track.height + 16),
            Control::Editor(EditorControl::Fader(p.index)),
            true,
            p.label.clone(),
            Look::Area,
        );
        if p.scene {
            crate::widgets::scene_mark(l, track.x + 4, track.y + 4);
        }
    }
}

/// Page 2: the `EqPanel` strip (HPF, LPF, OUTPUT, Q on Nolly; dividers `#121212` 1 × 78)
/// over the response graph. I: the page split (stock pages `+0x138`; encoder page 2 turns
/// HPF, LPF, OUTPUT, Q, `0x2c6184`).
fn graphic_panel(ui: &UiState, l: &mut Layout, nolly: bool) {
    l.push(
        Rect::new(0, BODY_TOP, WIDTH, 420),
        "",
        Look::Fill(BACKGROUND),
    );
    l.panel(STRIP, PANEL, 8.0, None);
    let knobs: &[u16] = if nolly {
        &[HPF, LPF, GEQ_OUTPUT, GEQ_Q]
    } else {
        &[HPF, LPF, GEQ_OUTPUT]
    };
    for (column, &index) in knobs.iter().enumerate() {
        let x = STRIP.x + column as i32 * 157;
        if column > 0 {
            l.push(
                Rect::new(x - 1, STRIP.y + 15, 1, 78),
                "",
                Look::Fill(BACKGROUND),
            );
        }
        if let Some(p) = parameter(ui, index) {
            knob_cell(l, Rect::new(x, STRIP.y + 6, 157, 96), p, &p.label, true);
        }
    }
    let hpf = display(ui, HPF).unwrap_or(19.0);
    let lpf = display(ui, LPF).unwrap_or(20_001.0);
    let q = display(ui, GEQ_Q).unwrap_or(1.0);
    let pass = |shape, hz, on| Band {
        shape,
        gain: 0.0,
        hz,
        q,
        on,
    };
    // HPF is OFF at 19 Hz and LPF at 20001 Hz (`min_string`, `max_string`).
    let mut curve = vec![
        pass(Shape::HighPass, hpf, hpf > 19.5),
        pass(Shape::LowPass, lpf, lpf < 20_000.5),
    ];
    curve.extend(GEQ_BANDS.iter().enumerate().map(|(band, &hz)| Band {
        shape: match band {
            0 => Shape::LowShelf,
            8 => Shape::HighShelf,
            _ => Shape::Peak,
        },
        gain: display(ui, 1 + band as u16).unwrap_or(0.0),
        hz,
        q,
        on: true,
    }));
    let nodes: Vec<Node> = [("H", 0u8, hpf.max(20.0)), ("L", 1, lpf.min(20_000.0))]
        .into_iter()
        .map(|(label, node, hz)| Node {
            label: label.into(),
            hz,
            db: global_eq::response(&curve, hz),
            on: true,
            selected: false,
            control: Some(Control::Editor(EditorControl::EqNode(node))),
        })
        .collect();
    global_eq::draw_graph(l, PANEL_GRAPH, &curve, &nodes);
}

// --- Actions ---------------------------------------------------------------------------

impl UiState {
    /// A tap on an EQ editor control.
    pub(super) fn eq_control(
        &mut self,
        editor: ModelEditor,
        control: EditorControl,
    ) -> Vec<UiCommand> {
        let Some(position) = self.editing_block() else {
            return Vec::new();
        };
        let ModelEditor::Eq { bands, .. } = editor else {
            return match control {
                EditorControl::Fader(_) | EditorControl::EqNode(_) => {
                    let (x, y) = self.pointer;
                    self.eq_drag(editor, control, x, y)
                }
                _ => Vec::new(),
            };
        };
        let band = tab(self, bands);
        let nudge = |parameter: u16| {
            vec![UiCommand::NudgeParameter {
                position,
                parameter,
                steps: 1,
            }]
        };
        match control {
            EditorControl::EqTab(index) => {
                self.eq_band = index.min(bands);
                Vec::new()
            }
            EditorControl::EqNode(node) => {
                let double = self.node_tap.is_some_and(|(last, at)| {
                    last == node && self.now.saturating_sub(at) <= DOUBLE_TAP_MS
                });
                self.eq_band = node;
                if double {
                    self.node_tap = None;
                    return nudge(band_parameter(node, BYPASS));
                }
                self.node_tap = Some((node, self.now));
                Vec::new()
            }
            EditorControl::EqType if band < bands => {
                let current = type_index(self, band);
                self.open_choice(
                    ChoicePurpose::FilterType(band_parameter(band, TYPE)),
                    current,
                );
                Vec::new()
            }
            EditorControl::EqBypass if band < bands => nudge(band_parameter(band, BYPASS)),
            _ => Vec::new(),
        }
    }

    /// A drag on an EQ node or a Graphic EQ fader. A band node sets FREQ and, when GAIN is
    /// live, GAIN (`controlPointMovedToPosSlot` `0x63b988`); the Graphic EQ "H" and "L" nodes
    /// set HPF and LPF (I); a fader sets its gain.
    pub(super) fn eq_drag(
        &mut self,
        editor: ModelEditor,
        control: EditorControl,
        x: i32,
        y: i32,
    ) -> Vec<UiCommand> {
        let Some(position) = self.editing_block() else {
            return Vec::new();
        };
        let enter = |parameter: u16, text: String| UiCommand::EnterParameter {
            target: ParameterTarget::Block(position),
            parameter,
            text,
        };
        match (editor, control) {
            (ModelEditor::Eq { bands, .. }, EditorControl::EqNode(band)) if band < bands => {
                self.eq_band = band;
                self.node_tap = None;
                let (hz, db) = global_eq::point_value(global_eq::GRAPH, x, y);
                let mut commands = vec![enter(band_parameter(band, FREQ), format!("{hz:.0}"))];
                if knob_enabled(self, band, GAIN) {
                    commands.push(enter(band_parameter(band, GAIN), format!("{db:.1}")));
                }
                commands
            }
            (ModelEditor::GraphicEq { .. }, EditorControl::EqNode(node)) => {
                let (hz, _) = global_eq::point_value(PANEL_GRAPH, x, y);
                let index = if node == 0 { HPF } else { LPF };
                vec![enter(index, format!("{hz:.0}"))]
            }
            (ModelEditor::GraphicEq { .. }, EditorControl::Fader(index))
                if parameter(self, index).is_some() =>
            {
                vec![enter(index, format!("{:.1}", fader_db(y)))]
            }
            _ => Vec::new(),
        }
    }

    /// Encoders (`0x63d788`). Band tab: A TYPE, B GAIN, C FREQ, D Q while the band is on and
    /// the knob is live; DOWN turns the band on (left) or off (right). OUT: DOWN turns OUTPUT
    /// (`0x63a984`). Graphic EQ (`0x2c6184`): page 1 A–D, DOWN, E–H turn the nine faders in
    /// order; page 2 A HPF, B LPF, C OUTPUT, D Q (Nolly only).
    pub(super) fn block_eq_knob(
        &mut self,
        editor: ModelEditor,
        knob: KnobIndex,
        delta: i32,
    ) -> Option<(u16, i32)> {
        let k = knob.get();
        match editor {
            ModelEditor::Eq { bands, output } => {
                let band = tab(self, bands);
                if band == bands {
                    return (k == 8).then_some((output, delta));
                }
                let offset = match k {
                    0 => TYPE,
                    1 => GAIN,
                    2 => FREQ,
                    3 => Q,
                    8 => {
                        let on = band_on(self, band);
                        let flip = (delta < 0 && !on) || (delta > 0 && on);
                        return flip.then_some((band_parameter(band, BYPASS), 1));
                    }
                    _ => return None,
                };
                let live =
                    band_on(self, band) && (offset == TYPE || knob_enabled(self, band, offset));
                live.then_some((band_parameter(band, offset), delta))
            }
            ModelEditor::GraphicEq { nolly } if self.param_page == 1 => match k {
                0 => Some((HPF, delta)),
                1 => Some((LPF, delta)),
                2 => Some((GEQ_OUTPUT, delta)),
                3 if nolly => Some((GEQ_Q, delta)),
                _ => None,
            },
            ModelEditor::GraphicEq { .. } => {
                // HAL order A–D, DOWN, E–H → bands 1..=9.
                let band = match k {
                    0..=3 => k + 1,
                    8 => 5,
                    4..=7 => k + 2,
                    _ => return None,
                };
                Some((u16::from(band), delta))
            }
            _ => None,
        }
    }

    /// A filter type table cell: TYPE steps from the current type to the cell.
    pub(crate) fn choose_filter_type(&self, index: u16, cell: usize) -> Vec<UiCommand> {
        let Some(position) = self.editing_block() else {
            return Vec::new();
        };
        let band = (index / 5) as u8;
        let steps = cell as i32 - type_index(self, band) as i32;
        if steps == 0 || cell > 4 {
            return Vec::new();
        }
        vec![UiCommand::NudgeParameter {
            position,
            parameter: index,
            steps,
        }]
    }
}

//! Tabbed block editors with a control point graph over a five-knob card: Arpeggio Delay
//! (`ArpeggioDelayModelEditor` `0x2ad914`, model 6031) here, and the shared tab bar, knob
//! card and point look the voicer, Parallax and synth editors reuse. Stock geometry and
//! colours; the points and axis marks are owned drawings (no stock bitmaps).

use tetrad_assets::{
    BACKGROUND, CANVAS, DIVIDER, FAINT, Icon, IconSize, MUTED, PANEL, ROW_SELECTED, Rgb, SURFACE,
    TEXT,
};

use super::model_editor::{BODY_TOP, ModelEditor, parameter};
use crate::layout::{BODY, Layout, Look};
use crate::render::{Align, WIDTH};
use crate::state::DOUBLE_TAP_MS;
use crate::{
    Control, EditorControl, KnobIndex, POSITION_SCALE, ParameterKind, ParameterView, Rect,
    UiCommand,
};
use crate::{UiState, widgets};

/// The knob card (`0x2ab2b8`): a `#2e2e2e` radius 6 card 784 × 107 at (8, 305) per tab,
/// five 157 px cells.
pub(super) const CARD: Rect = Rect::new(8, BODY_TOP + 305, 784, 107);
const CELL: i32 = 157;
/// Selected control point fill (as the EQ graph, `0x4f6d08`; I for these graphs).
const POINT_BLUE: Rgb = Rgb(0x0a, 0x74, 0xe0);

/// The normalized value of a knob, toggle or choice.
pub(super) fn position(ui: &UiState, index: u16) -> f32 {
    match parameter(ui, index).map(|p| p.kind) {
        Some(
            ParameterKind::Continuous { position }
            | ParameterKind::Centred { position }
            | ParameterKind::Int { position }
            | ParameterKind::Choice { position, .. }
            | ParameterKind::ComboBox { position },
        ) => position,
        Some(ParameterKind::Toggle { on }) => f32::from(u8::from(on)),
        _ => 0.0,
    }
}

/// A tab bar (`TabBar` `0x346a94`): equal tabs over `frame`; the selected tab takes the card
/// colour so it joins the card under it (I: idle tabs `#1e1e1e`).
pub(super) fn tab_bar(l: &mut Layout, frame: Rect, labels: &[&str], selected: usize) {
    let width = frame.width as f32 / labels.len() as f32;
    for (index, label) in labels.iter().enumerate() {
        let x = frame.x + (index as f32 * width).round() as i32;
        let next = frame.x + ((index + 1) as f32 * width).round() as i32;
        let bounds = Rect::new(x, frame.y, (next - x - 2).max(1) as u32, frame.height);
        let on = index == selected;
        l.panel(bounds, if on { PANEL } else { SURFACE }, 4.0, None);
        l.control(
            bounds,
            Control::Editor(EditorControl::Tab(index as u8)),
            true,
            *label,
            Look::Area,
        );
        l.text(
            bounds,
            *label,
            BODY,
            if on { TEXT } else { MUTED },
            Align::Center,
        );
    }
}

/// The knob card with `knobs` in its cells left to right (`None` leaves a cell empty) and
/// `#121212` 1 × 78 dividers. `live` says whether a knob takes input.
pub(super) fn knob_card(
    ui: &UiState,
    l: &mut Layout,
    knobs: &[Option<u16>],
    hue: Rgb,
    live: impl Fn(u16) -> bool,
) {
    l.panel(CARD, PANEL, 6.0, None);
    for (column, index) in knobs.iter().enumerate() {
        let x = CARD.x + column as i32 * CELL;
        if column > 0 {
            l.push(
                Rect::new(x - 1, CARD.y + 15, 1, 78),
                "",
                Look::Fill(DIVIDER),
            );
        }
        if let Some(p) = index.and_then(|index| parameter(ui, index)) {
            knob(
                l,
                Rect::new(x, CARD.y, CELL as u32 - 1, CARD.height),
                p,
                hue,
                live(p.index),
            );
        }
    }
}

pub(super) fn knob(l: &mut Layout, bounds: Rect, p: &ParameterView, hue: Rgb, live: bool) {
    l.control(
        bounds,
        Control::Editor(EditorControl::Knob(p.index)),
        live,
        p.label.clone(),
        Look::Knob {
            value: p.value.clone(),
            kind: p.kind,
            color: hue,
            focused: false,
        },
    );
    if p.scene {
        widgets::scene_mark(l, bounds.x + 10, bounds.y + 36);
    }
}

/// A control point at `(x, y)` (the EQ node look, I): selected 52 px blue with a black
/// label, others 38 px `#2e2e2e` with a `#121212` ring; an off point is `#1e1e1e` with a
/// `#2e2e2e` ring and a grey label.
pub(super) fn point(l: &mut Layout, (x, y): (i32, i32), label: &str, selected: bool, on: bool) {
    let radius = if selected { 26 } else { 19 };
    let bounds = Rect::new(x - radius, y - radius, radius as u32 * 2, radius as u32 * 2);
    let (fill, text) = match (selected, on) {
        (true, true) => (POINT_BLUE, CANVAS),
        (_, false) => (SURFACE, FAINT),
        (false, true) => (PANEL, TEXT),
    };
    if !selected || !on {
        let ring = Rect::new(
            bounds.x - 2,
            bounds.y - 2,
            bounds.width + 4,
            bounds.height + 4,
        );
        let color = if on { DIVIDER } else { PANEL };
        l.push(ring, "", Look::Dot { color });
    }
    l.push(bounds, "", Look::Dot { color: fill });
    l.text(bounds, label, BODY, text, Align::Center);
}

// --- Arpeggio Delay --------------------------------------------------------------------

const MIX: u16 = 0;
const SYNC: u16 = 1;
const SCALE: u16 = 3;
/// TAP n starts at 5 + 7·n: ACTIVE, SEMITONES, INTERVAL, DELAY TIME, SYNC NOTE, LEVEL, PAN.
const TAP_FIRST: u16 = 5;
const TAP_STRIDE: u16 = 7;
const TABS: [&str; 5] = ["SETTINGS", "TAP 1", "TAP 2", "TAP 3", "OUTPUT"];
/// Knobs per tab (static init `0xa23360`…`0xa23300`) with DELAY TIME and INTERVAL; SYNC on
/// swaps every TIME for its SYNC NOTE (index + 1), SCALE Chromatic every INTERVAL for its
/// SEMITONES (index − 1) (`0x2ad474`, picked by `0x2a9104`).
const KNOBS: [[u16; 5]; 5] = [
    [MIX, SYNC, 2, SCALE, 4],
    [5, 7, 8, 10, 11],
    [12, 14, 15, 17, 18],
    [19, 21, 22, 24, 25],
    [26, 27, 29, 30, 31],
];
const TIMES: [u16; 4] = [8, 15, 22, 27];
const INTERVALS: [u16; 3] = [7, 14, 21];
/// Graph (`ArpeggioDelayGraph` `0x2a9dfc`, body coordinates): axes `#2e2e2e` at x 32 and
/// y 224; time runs over 711 px from x 52, pitch over 159 px up from y 199.5 (the drag law
/// of `0x2acb44`; I: the zero point of each axis, from the centred axis marks).
const TIME_X: f32 = 52.0;
const TIME_PX: f32 = 711.0;
const PITCH_Y: f32 = 199.5;
const PITCH_PX: f32 = 159.0;

fn arpeggio_tab(ui: &UiState) -> usize {
    usize::from(ui.editor_tab).min(TABS.len() - 1)
}

/// The parameter at `index` after the SYNC and SCALE swaps.
fn arpeggio_swap(ui: &UiState, index: u16) -> u16 {
    if TIMES.contains(&index) && position(ui, SYNC) > 0.5 {
        index + 1
    } else if INTERVALS.contains(&index) && (position(ui, SCALE) * 4.0).round() == 0.0 {
        index - 1
    } else {
        index
    }
}

fn tap_on(ui: &UiState, tap: u16) -> bool {
    position(ui, TAP_FIRST + TAP_STRIDE * tap) > 0.5
}

/// A tap's knobs other than ACTIVE take input only while it is on (`0x2ac414`, lists at
/// `0x153991c`).
fn arpeggio_live(ui: &UiState, index: u16) -> bool {
    match index {
        5..=25 => {
            let tap = (index - TAP_FIRST) / TAP_STRIDE;
            (index - TAP_FIRST).is_multiple_of(TAP_STRIDE) || tap_on(ui, tap)
        }
        _ => true,
    }
}

/// Pitch and time parameters of tap point `tap` (`0x2acb44`).
fn arpeggio_axes(ui: &UiState, tap: u16) -> (u16, u16) {
    let base = TAP_FIRST + TAP_STRIDE * tap;
    (arpeggio_swap(ui, base + 2), arpeggio_swap(ui, base + 3))
}

fn arpeggio_place(ui: &UiState, tap: u16) -> (i32, i32) {
    let (pitch, time) = arpeggio_axes(ui, tap);
    let x = TIME_X + position(ui, time) * TIME_PX;
    let y = BODY_TOP as f32 + PITCH_Y - position(ui, pitch) * PITCH_PX;
    (x.round() as i32, y.round() as i32)
}

pub(super) fn arpeggio(ui: &UiState, l: &mut Layout, hue: Rgb) {
    let top = BODY_TOP;
    l.push(Rect::new(0, top, WIDTH, 420), "", Look::Fill(BACKGROUND));
    // Axes and the pitch grid (`#252525` 751 × 1 lines; I: four of them).
    l.push(Rect::new(32, top + 16, 2, 208), "", Look::Fill(PANEL));
    l.push(Rect::new(32, top + 224, 751, 2), "", Look::Fill(PANEL));
    for quarter in 0..=4 {
        let y = top + (PITCH_Y - PITCH_PX * quarter as f32 / 4.0).round() as i32;
        l.push(Rect::new(34, y, 749, 1), "", Look::Fill(ROW_SELECTED));
    }
    // Axis marks (`0x2aa7d4`): + / − 11 px and the pitch and delay icons 16 px (owned).
    let glyph = |l: &mut Layout, x, y, size: u32, icon| {
        let small = if size == 16 {
            IconSize::Small
        } else {
            IconSize::Compact
        };
        let look = Look::Glyph {
            icon,
            size: small,
            color: MUTED,
        };
        l.push(Rect::new(x, top + y, size, size), "", look);
    };
    glyph(l, 10, 63, 11, Icon::Plus);
    glyph(l, 10, 169, 11, Icon::Minus);
    glyph(l, 8, 119, 16, Icon::Pitch);
    glyph(l, 211, 234, 11, Icon::Minus);
    glyph(l, 595, 234, 11, Icon::Plus);
    glyph(l, 398, 232, 16, Icon::Delay);
    let tab = arpeggio_tab(ui);
    let selected = tab.checked_sub(1).filter(|&tap| tap < 3);
    let mut order: Vec<u16> = (0..3)
        .filter(|&tap| Some(usize::from(tap)) != selected)
        .collect();
    order.extend(selected.map(|tap| tap as u16));
    for tap in order {
        let at = arpeggio_place(ui, tap);
        let label = (tap + 1).to_string();
        point(
            l,
            at,
            &label,
            Some(usize::from(tap)) == selected,
            tap_on(ui, tap),
        );
        let hit = Rect::new(at.0 - 26, at.1 - 26, 52, 52);
        let control = Control::Editor(EditorControl::Point(tap as u8));
        l.control(hit, control, true, format!("TAP {label}"), Look::Area);
    }
    // Tab bar (`0x2a7a98`): 761 × 56 at (16, 249).
    tab_bar(l, Rect::new(16, top + 249, 761, 56), &TABS, tab);
    let knobs = KNOBS[tab].map(|index| Some(arpeggio_swap(ui, index)));
    knob_card(ui, l, &knobs, hue, |index| arpeggio_live(ui, index));
}

// --- Actions ----------------------------------------------------------------------------

impl UiState {
    /// Encoders (`0x2ad0d0` → `0x2abc9c`): on the QC E–H and TEMPO (HAL 5..9) turn the five
    /// knobs of the tab; other encoders do nothing (the mini uses HAL 0..4).
    pub(super) fn fx_editor_knob(&self, editor: ModelEditor, knob: KnobIndex) -> Option<u16> {
        let slot = match knob.get() {
            k @ 4..=7 => usize::from(k - 4),
            9 => 4,
            _ => return None,
        };
        match editor {
            ModelEditor::Arpeggio => {
                let index = arpeggio_swap(self, KNOBS[arpeggio_tab(self)][slot]);
                arpeggio_live(self, index).then_some(index)
            }
            _ => None,
        }
    }

    /// A tap on a tab or a point. A point tap selects its tab (`voiceSelectedSlot`
    /// `0x2a877c`: tab = point + 1); a second tap within [`DOUBLE_TAP_MS`] flips the tap's
    /// ACTIVE (`voiceDoubleTappedSlot` `0x2ac904`, table `0xa232f0`).
    pub(super) fn fx_editor_control(&mut self, control: EditorControl) -> Vec<UiCommand> {
        let Some(position) = self.editing_block() else {
            return Vec::new();
        };
        match control {
            EditorControl::Tab(tab) => {
                self.editor_tab = tab.min(TABS.len() as u8 - 1);
                Vec::new()
            }
            EditorControl::Point(tap) if tap < 3 => {
                let double = self.node_tap.is_some_and(|(last, at)| {
                    last == tap && self.now.saturating_sub(at) <= DOUBLE_TAP_MS
                });
                self.editor_tab = tap + 1;
                if double {
                    self.node_tap = None;
                    return vec![UiCommand::NudgeParameter {
                        position,
                        parameter: TAP_FIRST + TAP_STRIDE * u16::from(tap),
                        steps: 1,
                    }];
                }
                self.node_tap = Some((tap, self.now));
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// A point drag (`controlPointMovedToPosSlot` `0x2acb44`): x sets the tap's time (DELAY
    /// TIME or SYNC NOTE) and y its pitch (INTERVAL or SEMITONES) on the graph scale; the
    /// application writes a value only when its step changes. A tap that is off does not
    /// move (I: stock disables its point, `0x3b61d8`).
    pub(super) fn fx_editor_drag(
        &mut self,
        control: EditorControl,
        x: i32,
        y: i32,
    ) -> Vec<UiCommand> {
        let (Some(position), EditorControl::Point(tap)) = (self.editing_block(), control) else {
            return Vec::new();
        };
        let tap = u16::from(tap);
        if tap >= 3 || !tap_on(self, tap) {
            return Vec::new();
        }
        self.editor_tab = tap as u8 + 1;
        self.node_tap = None;
        let (pitch, time) = arpeggio_axes(self, tap);
        let t = (x as f32 - TIME_X) / TIME_PX;
        let p = (BODY_TOP as f32 + PITCH_Y - y as f32) / PITCH_PX;
        [(time, t), (pitch, p)]
            .into_iter()
            .filter(|&(index, _)| parameter(self, index).is_some())
            .map(|(parameter, value)| UiCommand::SetPosition {
                position,
                parameter,
                value: (value.clamp(0.0, 1.0) * f32::from(POSITION_SCALE)).round() as u16,
            })
            .collect()
    }
}

//! Stock shared widgets as layout builders. Each builder only appends display-list
//! items, so drawing, hit testing and automation keep one source. Single-item widgets are plain
//! looks: [`Look::Switch`] (vertical On/Off switch), [`Look::Dropdown`] (`value ▼` field) and
//! [`Look::IconButton`].

use tetrad_assets::{ACCENT, CANVAS, DIVIDER, Icon, MUTED, PANEL, Rgb, TEXT};

use crate::layout::{BODY, Layout, ListId, Look, MEDIUM, SMALL_BOLD};
use crate::render::{Align, HEIGHT, WIDTH};
use crate::screens::CHOICE_ROW;
use crate::{
    Control, ExpressionLink, ExpressionTarget, KNOB_COUNT, KnobIndex, ParamControl, ParameterKind,
    ParameterView, Rect,
};

/// Vertical travel that sweeps a knob over its whole normalized range
/// (`ParameterControl::mouseMoveEvent` `0x4c9e20`, literal 200.0 at `0x4ca1e0`).
pub(crate) const KNOB_DRAG_SPAN: f32 = 200.0;

/// Stock knob drag law: each move adds (previous y − y) / [`KNOB_DRAG_SPAN`] to the normalized
/// value and clamps it to 0..=1. Up increases; no slop, no acceleration. The drag keeps its own
/// value, as the stock widget keeps its own copy of the parameter, so a late snapshot does not lose
/// travel. Block parameter knobs, full-screen editor knobs and the settings panel knobs share it.
///
/// A stepped knob (stock RotarySwitch, `0x4c9e20`) rounds the sum to a step (`0x5cc994`,
/// `lroundf`). On the same step the move is dropped and the anchor stays, so the travel adds
/// up; on a new step the value is that step (`0x5cc9e0`) and the anchor moves. Each change
/// needs 100 · step px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct KnobDrag {
    last_y: i32,
    value: f32,
    /// The normalized step of a stepped knob.
    step: Option<f32>,
}

impl KnobDrag {
    pub(crate) fn new(y: i32, value: f32) -> Self {
        Self {
            last_y: y,
            value: value.clamp(0.0, 1.0),
            step: None,
        }
    }

    /// A stepped knob of `steps` positions (the normalized step is 1 / (steps − 1)).
    pub(crate) fn stepped(y: i32, value: f32, steps: u16) -> Self {
        let step = 1.0 / f32::from(steps.saturating_sub(1).max(1));
        Self {
            step: Some(step),
            ..Self::new(y, value)
        }
    }

    /// Follows a move to `y`: the new normalized value, or `None` when it did not change
    /// (no travel, held at an end, or still on the same step).
    pub(crate) fn follow(&mut self, y: i32) -> Option<f32> {
        let mut value = (self.value + (self.last_y - y) as f32 / KNOB_DRAG_SPAN).clamp(0.0, 1.0);
        if let Some(step) = self.step
            && value != self.value
        {
            let index = (value / step).round();
            if index == (self.value / step).round() {
                return None;
            }
            value = (index * step).min(1.0);
        }
        self.last_y = y;
        (value != self.value).then(|| {
            self.value = value;
            value
        })
    }
}

/// The drag of a parameter knob pressed at `y`; `None` for parameters without a knob drag.
/// Stock drags types 0 Float, 1 Fader, 4 RotarySwitch and 12 FloatWithLed only (mask
/// `0x1013`), so Int, ComboBox, toggles and media pickers do not drag.
pub(crate) fn knob_drag(kind: ParameterKind, y: i32) -> Option<KnobDrag> {
    match kind {
        ParameterKind::Continuous { position } | ParameterKind::Centred { position } => {
            Some(KnobDrag::new(y, position))
        }
        ParameterKind::Choice { position, steps } => Some(KnobDrag::stepped(y, position, steps)),
        ParameterKind::Int { .. }
        | ParameterKind::ComboBox { .. }
        | ParameterKind::Toggle { .. }
        | ParameterKind::IconChoice { .. }
        | ParameterKind::Media(_) => None,
    }
}

/// A knob drag value as a [`crate::UiCommand::SetPosition`] value.
pub(crate) fn position_value(value: f32) -> u16 {
    (value.clamp(0.0, 1.0) * f32::from(crate::POSITION_SCALE)).round() as u16
}

/// Stock ✓/✕ page button (shots 008, 034, 037).
pub(crate) const CLOSE_BUTTON: Rect = Rect::new(694, 8, 98, 44);
/// Width of the ⋮ touch target at the right end of a list row.
const MENU_BUTTON: u32 = 48;
const MARGIN: i32 = 8;

/// Stock page chrome: a grey title at the top left on the page background
/// (no bar, no back arrow) and the icon-only `icon` button at [`CLOSE_BUTTON`]
/// sending `close`. Returns the free header area between the two.
pub(crate) fn page_header(l: &mut Layout, title: &str, close: Control, icon: Icon) -> Rect {
    let width = (MEDIUM.width(title).ceil() as u32 + 8).min(420);
    l.text(
        Rect::new(16, CLOSE_BUTTON.y, width, CLOSE_BUTTON.height),
        title,
        MEDIUM,
        MUTED,
        Align::Left,
    );
    l.control(
        CLOSE_BUTTON,
        close,
        true,
        "CLOSE",
        Look::IconButton { icon, fill: PANEL },
    );
    let left = 16 + width as i32 + 12;
    Rect::new(
        left,
        CLOSE_BUTTON.y,
        (CLOSE_BUTTON.x - MARGIN - left).max(0) as u32,
        CLOSE_BUTTON.height,
    )
}

/// One flat row of a list that sits on one panel. With `menu`, the right end
/// is a separate ⋮ target and the row look is drawn full width behind both
/// touch areas (always in its enabled style).
#[allow(clippy::too_many_arguments)]
pub(crate) fn list_row(
    l: &mut Layout,
    bounds: Rect,
    control: Control,
    enabled: bool,
    label: impl Into<String>,
    icon: Option<Icon>,
    selected: bool,
    menu: Option<Control>,
) {
    let label = label.into();
    let look = Look::ListRow {
        icon,
        value: String::new(),
        selected,
        divider: false,
        menu: menu.is_some(),
        font: BODY,
    };
    match menu {
        None => l.control(bounds, control, enabled, label, look),
        Some(menu) => {
            let (row, button) = bounds.take_right(MENU_BUTTON, 0);
            l.control(row, control, enabled, label.clone(), Look::Area);
            l.control(button, menu, enabled, "MORE", Look::Area);
            l.push(bounds, label, look);
        }
    }
}

/// Stock dropdown list opened in place: a black panel under its field (above
/// it when there is more room there), `CHOICE_ROW` rows, the current value in
/// green. It scrolls when the options do not fit.
pub(crate) fn dropdown_list(
    l: &mut Layout,
    anchor: Rect,
    options: &[&str],
    current: Option<usize>,
    offset: i32,
) {
    let content = options.len() as u32 * CHOICE_ROW;
    let widest = options
        .iter()
        .map(|option| BODY.width(option))
        .fold(0.0, f32::max)
        .ceil() as u32
        + 32;
    let width = anchor.width.max(widest).min(WIDTH - 2 * MARGIN as u32);
    let below = HEIGHT as i32 - MARGIN - anchor.bottom() - 4;
    let above = anchor.y - MARGIN - 4;
    let down = below >= content.min(CHOICE_ROW * 4) as i32 || below >= above;
    let height = content.min(below.max(above).max(CHOICE_ROW as i32) as u32);
    let y = if down {
        anchor.bottom() + 4
    } else {
        anchor.y - 4 - height as i32
    };
    let panel = Rect::new(clamp_x(anchor.x, width), y, width, height);
    l.popover(panel);
    l.panel(panel, CANVAS, 8.0, None);
    l.scroll(ListId::Choice, panel, content, offset, |l, top| {
        for (index, option) in options.iter().enumerate() {
            list_row(
                l,
                Rect::new(
                    panel.x,
                    top + index as i32 * CHOICE_ROW as i32,
                    panel.width,
                    CHOICE_ROW,
                ),
                Control::Choose(index as u16),
                true,
                *option,
                None,
                current == Some(index),
                None,
            );
        }
    });
}

/// Stock contextual menu (shot 040): a 256 px black list anchored at its
/// trigger, no title and no Cancel. A tap outside closes it. The `marked` row is drawn
/// selected (green), as the folder menu's "Saving destination" (`0x745130` type 0x10).
pub(crate) fn popover_menu(
    l: &mut Layout,
    anchor: Rect,
    items: &[(Control, &str, bool)],
    marked: Option<usize>,
) {
    const MENU_WIDTH: u32 = 256;
    const ROW: u32 = 52;
    let height = items.len() as u32 * ROW;
    let bottom = HEIGHT as i32 - MARGIN;
    let right_half = anchor.center().0 > WIDTH as i32 / 2;
    // Under or over the trigger, right-aligned to triggers in the right half
    // as the stock ⋮ menus are; beside it when neither fits.
    let (x, y) = if anchor.bottom() + 4 + height as i32 <= bottom {
        (None, anchor.bottom() + 4)
    } else if anchor.y - 4 - height as i32 >= MARGIN {
        (None, anchor.y - 4 - height as i32)
    } else if right_half {
        (Some(anchor.x - 4 - MENU_WIDTH as i32), anchor.y)
    } else {
        (Some(anchor.right() + 4), anchor.y)
    };
    let x = x.unwrap_or(if right_half {
        anchor.right() - MENU_WIDTH as i32
    } else {
        anchor.x
    });
    let panel = Rect::new(
        clamp_x(x, MENU_WIDTH),
        y.min(bottom - height as i32).max(MARGIN),
        MENU_WIDTH,
        height,
    );
    l.popover(panel);
    l.panel(panel, CANVAS, 8.0, None);
    for (index, (control, label, enabled)) in items.iter().enumerate() {
        list_row(
            l,
            Rect::new(
                panel.x,
                panel.y + (index as u32 * ROW) as i32,
                MENU_WIDTH,
                ROW,
            ),
            *control,
            *enabled,
            *label,
            None,
            marked == Some(index),
            None,
        );
    }
}

fn clamp_x(x: i32, width: u32) -> i32 {
    x.min(WIDTH as i32 - MARGIN - width as i32).max(MARGIN)
}

/// Stock parameter panel (`b4-stock1`, `sl-1`): one 156 × 107 cell per parameter on the
/// [`panel_cells`] card, five to a row; a row is drawn only when it holds a parameter, and
/// after the last cell the card runs on empty with no divider (`b4-stock1` row 2: dividers at
/// x 164 and 321 only). Cell `n` is knob `n`; the application maps knobs to the ten encoders
/// (A–H, BANK DOWN, TEMPO; stock order inferred). `color` is the block colour; `expression`
/// holds the links of the block that owns `parameters` (empty when they belong to another
/// element). Returns the card.
pub(crate) fn parameter_panel(
    l: &mut Layout,
    bounds: Rect,
    parameters: &[ParameterView],
    color: Rgb,
    expression: &[ExpressionLink],
    focus: Option<KnobIndex>,
) -> Rect {
    const COLUMNS: usize = KNOB_COUNT / 2;
    let shown = parameters.len().clamp(1, KNOB_COUNT);
    let rows = shown.div_ceil(COLUMNS);
    let last = shown - (rows - 1) * COLUMNS;
    let hidden: Vec<(u32, u32)> = (last + 1..COLUMNS)
        .map(|index| (rows as u32 - 1, index as u32))
        .collect();
    let card = Rect::new(bounds.x, bounds.y, bounds.width, rows as u32 * 109 - 2);
    let cells = panel_cells(l, card, rows as u32, DIVIDER, DIVIDER, &hidden);
    for ((slot, parameter), b) in parameters.iter().take(KNOB_COUNT).enumerate().zip(cells) {
        let (x, y) = (b.x, b.y);
        let knob = KnobIndex(slot as u8);
        // Stock `0x004c1e20`: an expression link paints the value arc green (`0x45f862`),
        // else a scene assignment paints it white, else the block colour.
        let linked = expression
            .iter()
            .any(|link| link.target == ExpressionTarget::Parameter(parameter.index));
        let arc = match (linked, parameter.scene) {
            (true, _) => ACCENT,
            (false, true) => TEXT,
            (false, false) => color,
        };
        l.control(
            b,
            Control::Param(ParamControl::Knob(knob)),
            true,
            parameter.label.clone(),
            Look::Knob {
                value: parameter.value.clone(),
                kind: parameter.kind,
                color: arc,
                focused: focus == Some(knob),
            },
        );
        // The value text opens the numeric keyboard (stock rect 12,66 65×20; owned 8 px
        // touch margin).
        if parameter.entry.is_some() {
            l.control(
                Rect::new(x + 4, y + 58, 81, 36),
                Control::Param(ParamControl::Value(knob)),
                true,
                parameter.value.clone(),
                Look::Area,
            );
        }
        if parameter.scene {
            scene_mark(l, x + 10, y + 36);
        }
    }
    card
}

/// The scene-assigned mark under a parameter label (shot 109): 24 × 24, four white
/// 11 px squares with the dark letters A B / C D.
pub(crate) fn scene_mark(l: &mut Layout, x: i32, y: i32) {
    for (index, letter) in ["A", "B", "C", "D"].into_iter().enumerate() {
        let square = Rect::new(
            x + (index as i32 % 2) * 13,
            y + (index as i32 / 2) * 13,
            11,
            11,
        );
        l.panel(square, TEXT, 1.0, None);
        l.text(square, letter, SMALL_BOLD, PANEL, Align::Center);
    }
}

/// Stock settings-card fill (Tuner 037, Tempo 008, Capture 095, A/B 098).
pub(crate) const CARD: Rgb = Rgb(0x28, 0x2c, 0x28);
/// Stock page colour behind the Tuner, Capture and A/B screens (037, 095, 098).
pub(crate) const PAGE: Rgb = Rgb(0x10, 0x10, 0x10);

/// Stock settings panel (Tuner, Tempo, Capture; shots 037, 008, 095): a card at the
/// origin of `bounds` with `rows` × 5 cells. Cells are 156 px wide on a 157 px pitch
/// and 107 px tall, with a 2 px `gap` between rows. Each divider is 1 px and runs 78 px
/// from 15 px below its row top. A `(row, index)` in `hidden` leaves that divider off;
/// divider `index` sits left of column `index`. Returns the cell bounds in encoder
/// order; the screen fills them with its own controls.
pub(crate) fn panel_cells(
    l: &mut Layout,
    bounds: Rect,
    rows: u32,
    gap: Rgb,
    divider: Rgb,
    hidden: &[(u32, u32)],
) -> Vec<Rect> {
    const COLUMNS: u32 = 5;
    const COLUMN_PITCH: i32 = 157;
    const CELL_WIDTH: u32 = 156;
    const ROW_PITCH: i32 = 109;
    const CELL_HEIGHT: u32 = 107;
    const DIVIDER_INSET: i32 = 15;
    const DIVIDER_HEIGHT: u32 = 78;
    let rows = rows.max(1);
    let row_top = |index: u32| bounds.y + index as i32 * ROW_PITCH;
    l.panel(bounds, CARD, 8.0, None);
    for index in 1..rows {
        l.push(
            Rect::new(bounds.x, row_top(index) - 2, bounds.width, 2),
            "",
            Look::Fill(gap),
        );
    }
    for row in 0..rows {
        for index in 1..COLUMNS {
            if hidden.contains(&(row, index)) {
                continue;
            }
            let x = bounds.x + index as i32 * COLUMN_PITCH - 1;
            l.push(
                Rect::new(x, row_top(row) + DIVIDER_INSET, 1, DIVIDER_HEIGHT),
                "",
                Look::Fill(divider),
            );
        }
    }
    (0..rows)
        .flat_map(|row| (0..COLUMNS).map(move |column| (column, row)))
        .map(|(column, row)| {
            Rect::new(
                bounds.x + column as i32 * COLUMN_PITCH,
                row_top(row),
                CELL_WIDTH,
                CELL_HEIGHT,
            )
        })
        .collect()
}

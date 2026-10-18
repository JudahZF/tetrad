//! Expression pedal assignment (spec `w4-ui-spec-grid` §6, shot 068): a full
//! screen with ✕, EXPRESSION 1 / 2 tabs and a save button, then one card of
//! 5 × 108 px cells. Each parameter cell has a range mark (green when linked
//! to the shown pedal) and ASSIGN / UNASSIGN (`0xa288f4`, `0xa34bf8`). While
//! BYPASS is linked, a bypass row with SWITCH ON (`0xa2dd70`), INVERT RANGE,
//! SWITCH DELAY (`0xa2dda8`) and LATCH EMULATION leads the card. The MIN /
//! MAX range editor of a linked cell is not drawn (layout not in the manual).

use tetrad_assets::{
    ACCENT, ACTION, BACKGROUND, CANVAS, FAINT, Icon, MUTED, PANEL, Rgb, SECTION, SURFACE, TEXT,
};

use crate::draw::{TAB_ACTIVE, TAB_IDLE};
use crate::layout::{BODY, ButtonStyle, Layout, ListId, Look, MEDIUM};
use crate::render::{Align, HEIGHT, WIDTH};
use crate::widgets::PAGE;
use crate::*;

const CARD: Rect = Rect::new(8, 148, WIDTH - 16, HEIGHT - 148 - 8);
const COLUMNS: u32 = 5;
const CELL_HEIGHT: u32 = 108;
/// Unlinked range-mark brackets (shot 068, unmapped stock grey).
const BRACKET: Rgb = Rgb(0x50, 0x50, 0x50);
/// Selected pedal tab outline (shot 068).
const TAB_OUTLINE: Rgb = Rgb(0x30, 0xc8, 0x48);
/// Owned SWITCH DELAY step for a tap on the knob.
const DELAY_STEP: u16 = 100;

pub(super) fn layout(ui: &UiState, l: &mut Layout) {
    let s = &ui.snapshot;
    let pedal = ui.expression_pedal;
    // Stock 068 page: `#101010`.
    l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(PAGE));
    l.control(
        Rect::new(8, 8, 98, 44),
        Control::Back,
        true,
        "CLOSE",
        Look::IconButton {
            icon: Icon::Close,
            fill: PANEL,
        },
    );
    for x in [115, 271] {
        l.push(Rect::new(x, 18, 1, 24), "", Look::Fill(SECTION));
    }
    for (bounds, tab) in [
        (Rect::new(124, 8, 138, 44), ExpressionPedal::Exp1),
        (Rect::new(280, 8, 134, 44), ExpressionPedal::Exp2),
    ] {
        // Stock 068: selected = `#30c848` outline on `#102818`, green 18 px Regular text;
        // the other tab white text on `#181c18`.
        let selected = tab == pedal;
        if selected {
            l.panel(bounds, TAB_ACTIVE, 8.0, Some(TAB_OUTLINE));
        } else {
            l.panel(bounds, TAB_IDLE, 8.0, None);
        }
        l.control(
            bounds,
            Control::Expression(ExpressionControl::Pedal(tab)),
            true,
            tab.label(),
            Look::Area,
        );
        l.text(
            bounds,
            tab.label(),
            MEDIUM,
            if selected { ACCENT } else { TEXT },
            Align::Center,
        );
    }
    let Some(position) = s.selected_block.filter(|position| s.occupied(*position)) else {
        l.text(
            Rect::new(0, 200, WIDTH, 30),
            "Select a device on the Grid first",
            BODY,
            FAINT,
            Align::Center,
        );
        return;
    };
    l.control(
        Rect::new(694, 8, 98, 44),
        Control::Expression(ExpressionControl::Save),
        true,
        "SAVE",
        Look::IconButton {
            icon: Icon::Save,
            fill: ACTION,
        },
    );
    l.paragraph(
        Rect::new(16, 70, 620, 50),
        "Please choose which parameters you wish to control.\nYou can assign multiple at once.",
        MEDIUM,
        TEXT,
    );

    let linked = |target| {
        s.expression
            .iter()
            .any(|link| link.pedal == pedal && link.target == target)
    };
    let mut targets: Vec<(ExpressionTarget, String)> = s
        .parameters
        .iter()
        .filter(|parameter| !matches!(parameter.kind, ParameterKind::Media(_)))
        .map(|parameter| {
            (
                ExpressionTarget::Parameter(parameter.index),
                parameter.label.to_uppercase(),
            )
        })
        .collect();
    targets.push((ExpressionTarget::Bypass, "BYPASS".into()));
    let bypass_row = linked(ExpressionTarget::Bypass);
    let range = ui.expression_range();
    let top_rows = u32::from(bypass_row) + u32::from(range.is_some());
    let rows = targets.len().div_ceil(COLUMNS as usize) as u32 + top_rows;

    l.panel(CARD, SURFACE, 8.0, None);
    l.scroll(
        ListId::ExpressionCells,
        CARD,
        rows * CELL_HEIGHT,
        ui.offset(ListId::ExpressionCells),
        |l, top| {
            let mut top = top;
            if let Some(range) = range {
                range_row(l, range, top);
                top += CELL_HEIGHT as i32;
            }
            if bypass_row {
                bypass(ui, l, position, top);
                top += CELL_HEIGHT as i32;
            }
            for (index, (target, label)) in targets.iter().enumerate() {
                let bounds = cell(top, index);
                let on = linked(*target);
                let selected = matches!(
                    (*target, range),
                    (ExpressionTarget::Parameter(index), Some(range)) if range.parameter == index
                );
                if on {
                    l.push(
                        bounds,
                        "",
                        Look::Fill(if selected { SECTION } else { PANEL }),
                    );
                }
                if let (true, ExpressionTarget::Parameter(index)) = (on, *target) {
                    l.control(
                        Rect::new(bounds.x, bounds.y, bounds.width, 48),
                        Control::Expression(ExpressionControl::Select(index)),
                        true,
                        label.clone(),
                        Look::Area,
                    );
                }
                l.text(
                    Rect::new(bounds.x + 10, bounds.y + 8, bounds.width - 52, 26),
                    label.clone(),
                    BODY,
                    TEXT,
                    Align::Left,
                );
                range_mark(l, bounds.right() - 32, bounds.y + 10, on);
                l.button(
                    // Stock 36 px high; 40 px keeps the owned minimum touch size.
                    Rect::new(bounds.x + 10, bounds.y + 54, bounds.width - 20, 40),
                    Control::Expression(ExpressionControl::Assign(*target)),
                    true,
                    if on { "UNASSIGN" } else { "ASSIGN" },
                    ButtonStyle::Inset,
                );
            }
            dividers(l, top - (top_rows * CELL_HEIGHT) as i32, rows);
        },
    );
}

/// Range row of the selected linked parameter (`0x34d9b8`): MIN RANGE and MAX RANGE knobs
/// (0–100 %) in columns 1 and 2.
fn range_row(l: &mut Layout, range: ExpressionRangeView, top: i32) {
    l.push(
        Rect::new(CARD.x, top, CARD.width, CELL_HEIGHT),
        "",
        Look::Fill(PANEL),
    );
    for (column, title, value, max) in [
        (1, "MIN RANGE", range.min, false),
        (2, "MAX RANGE", range.max, true),
    ] {
        l.control(
            cell(top, column),
            Control::Expression(ExpressionControl::Range { max }),
            true,
            title,
            Look::Knob {
                value: format!("{value} %"),
                kind: ParameterKind::Continuous {
                    position: f32::from(value) / 100.0,
                },
                color: ACCENT,
                focused: false,
            },
        );
    }
}

/// Cell `index` of the parameter rows starting at `top`.
fn cell(top: i32, index: usize) -> Rect {
    let column = index as u32 % COLUMNS;
    let row = index as u32 / COLUMNS;
    let x = CARD.x + (CARD.width * column / COLUMNS) as i32;
    let right = CARD.x + (CARD.width * (column + 1) / COLUMNS) as i32;
    Rect::new(
        x,
        top + (row * CELL_HEIGHT) as i32,
        (right - x) as u32,
        CELL_HEIGHT,
    )
}

/// 2 px row lines and 1 px column lines across the card content.
fn dividers(l: &mut Layout, top: i32, rows: u32) {
    for row in 1..rows {
        l.push(
            Rect::new(CARD.x, top + (row * CELL_HEIGHT) as i32 - 1, CARD.width, 2),
            "",
            Look::Fill(BACKGROUND),
        );
    }
    for column in 1..COLUMNS {
        let x = CARD.x + (CARD.width * column / COLUMNS) as i32;
        l.push(
            Rect::new(x, top, 1, rows * CELL_HEIGHT),
            "",
            Look::Fill(BACKGROUND),
        );
    }
}

/// Bypass row: power, SWITCH ON, INVERT RANGE, SWITCH DELAY, LATCH EMULATION.
fn bypass(ui: &UiState, l: &mut Layout, position: GridPosition, top: i32) {
    let switch = ui.snapshot.expression_switch;
    let bypassed = ui.snapshot.cell(position).bypassed;
    l.push(
        Rect::new(CARD.x, top, CARD.width, CELL_HEIGHT),
        "",
        Look::Fill(PANEL),
    );
    let power = cell(top, 0);
    l.control(
        Rect::new(power.x + (power.width as i32 - 66) / 2, top + 32, 66, 44),
        Control::Expression(ExpressionControl::Bypass),
        true,
        if bypassed { "BYPASSED" } else { "ON" },
        Look::Button {
            style: ButtonStyle::Inset,
            icon: Some(Icon::Power),
            font: None,
        },
    );

    let bounds = cell(top, 1);
    label(l, bounds, "SWITCH ON", true);
    l.control(
        bounds,
        Control::Expression(ExpressionControl::SwitchOn),
        true,
        switch.switch_on.label(),
        Look::Area,
    );
    let track = Rect::new(bounds.x + 16, top + 36, 22, 64);
    l.panel(track, CANVAS, 11.0, None);
    for (index, choice) in ExpressionSwitch::ALL.into_iter().enumerate() {
        let y = top + 36 + index as i32 * 21;
        let active = choice == switch.switch_on;
        if active {
            ring(l, (track.x + 11, y + 11));
        }
        l.text(
            Rect::new(track.right() + 16, y, bounds.width - 60, 21),
            choice.label(),
            BODY,
            if active { TEXT } else { MUTED },
            Align::Left,
        );
    }

    let latch_enabled = switch.switch_on == ExpressionSwitch::Switch;
    for (column, title, control, on, enabled) in [
        (
            2,
            "INVERT RANGE",
            ExpressionControl::InvertRange,
            switch.inverted,
            true,
        ),
        (
            4,
            "LATCH EMULATION",
            ExpressionControl::LatchEmulation,
            switch.latch,
            latch_enabled,
        ),
    ] {
        let bounds = cell(top, column);
        label(l, bounds, title, enabled);
        l.control(
            Rect::new(bounds.x + 16, top + 36, bounds.width - 24, 64),
            Control::Expression(control),
            enabled,
            title,
            Look::Switch { on: Some(on) },
        );
    }

    l.control(
        cell(top, 3),
        Control::Expression(ExpressionControl::SwitchDelay),
        true,
        "SWITCH DELAY",
        Look::Knob {
            value: format!("{} ms", switch.delay_ms),
            kind: ParameterKind::Continuous {
                position: f32::from(switch.delay_ms) / f32::from(MAX_SWITCH_DELAY_MS),
            },
            color: ACCENT,
            focused: false,
        },
    );
}

/// Stock range mark "-[-]-" (shot 068), 24 × 20 at (`x`, `y`): grey brackets, a green
/// bar and white side ticks; all green when the cell is linked to the shown pedal.
fn range_mark(l: &mut Layout, x: i32, y: i32, linked: bool) {
    let (bracket, tick) = if linked {
        (ACCENT, ACCENT)
    } else {
        (BRACKET, TEXT)
    };
    let parts = [
        (0, 9, 3, 2, tick),
        (21, 9, 3, 2, tick),
        (7, 9, 10, 2, ACCENT),
        (4, 0, 2, 20, bracket),
        (4, 0, 4, 2, bracket),
        (4, 18, 4, 2, bracket),
        (18, 0, 2, 20, bracket),
        (16, 0, 4, 2, bracket),
        (16, 18, 4, 2, bracket),
    ];
    for (dx, dy, width, height, color) in parts {
        l.push(
            Rect::new(x + dx, y + dy, width, height),
            "",
            Look::Fill(color),
        );
    }
}

fn label(l: &mut Layout, bounds: Rect, text: &str, enabled: bool) {
    // Inset 10 left, 2 right: stock shows "LATCH EMULATION" in full in a 157 px cell.
    l.text(
        Rect::new(bounds.x + 10, bounds.y + 8, bounds.width - 12, 26),
        text,
        BODY,
        if enabled { TEXT } else { FAINT },
        Align::Left,
    );
}

/// Green switch ring (shot 068) at `center`.
fn ring(l: &mut Layout, (x, y): (i32, i32)) {
    l.push(
        Rect::new(x - 8, y - 8, 16, 16),
        "",
        Look::Dot { color: ACCENT },
    );
    l.push(
        Rect::new(x - 6, y - 6, 12, 12),
        "",
        Look::Dot { color: CANVAS },
    );
}

impl UiState {
    /// Grid ⋮ "Assign expression pedal": select the block if needed, then open.
    pub(crate) fn open_expression(&mut self, position: GridPosition) -> Vec<UiCommand> {
        let mut commands = if self.snapshot.selected_block == Some(position) {
            Vec::new()
        } else {
            vec![UiCommand::InspectCell { position }]
        };
        self.expression_pedal = ExpressionPedal::Exp1;
        self.reset_scroll(ListId::ExpressionCells);
        commands.extend(self.navigate(Screen::Expression));
        commands
    }

    pub(crate) fn expression_control(&mut self, control: ExpressionControl) -> Vec<UiCommand> {
        let Some(position) = self.snapshot.selected_block else {
            return Vec::new();
        };
        let pedal = self.expression_pedal;
        let switch = self.snapshot.expression_switch;
        let request = match control {
            ExpressionControl::Pedal(pedal) => {
                self.expression_pedal = pedal;
                return Vec::new();
            }
            ExpressionControl::Assign(target) => ExpressionRequest::Link {
                pedal,
                target,
                linked: !self
                    .snapshot
                    .expression
                    .iter()
                    .any(|link| link.pedal == pedal && link.target == target),
            },
            ExpressionControl::SwitchOn => {
                let all = ExpressionSwitch::ALL;
                let next = all
                    .iter()
                    .position(|choice| *choice == switch.switch_on)
                    .map_or(all[0], |index| all[(index + 1) % all.len()]);
                ExpressionRequest::SwitchOn(next)
            }
            ExpressionControl::InvertRange => ExpressionRequest::InvertRange(!switch.inverted),
            ExpressionControl::SwitchDelay => {
                ExpressionRequest::SwitchDelay(if switch.delay_ms >= MAX_SWITCH_DELAY_MS {
                    0
                } else {
                    (switch.delay_ms + DELAY_STEP).min(MAX_SWITCH_DELAY_MS)
                })
            }
            ExpressionControl::LatchEmulation => ExpressionRequest::LatchEmulation(!switch.latch),
            ExpressionControl::Bypass => {
                return vec![UiCommand::SetBlockBypass {
                    position,
                    bypassed: !self.snapshot.cell(position).bypassed,
                }];
            }
            ExpressionControl::Save => return self.close_expression(true),
            ExpressionControl::Select(parameter) => {
                self.expression_param = Some(parameter);
                return Vec::new();
            }
            ExpressionControl::Range { max } => return self.step_range(max, None),
        };
        vec![UiCommand::Expression { position, request }]
    }

    /// The selected parameter's range while it is linked to the shown pedal.
    pub(crate) fn expression_range(&self) -> Option<ExpressionRangeView> {
        let parameter = self.expression_param?;
        let pedal = self.expression_pedal;
        let target = ExpressionTarget::Parameter(parameter);
        self.snapshot
            .expression
            .iter()
            .any(|link| link.pedal == pedal && link.target == target)
            .then_some(())?;
        self.snapshot
            .expression_ranges
            .iter()
            .find(|range| range.parameter == parameter)
            .copied()
    }

    /// MIN RANGE / MAX RANGE: an encoder `delta` in 1 % steps (clamped), or a tap that adds
    /// 10 % and wraps after 100.
    pub(crate) fn step_range(&mut self, max: bool, delta: Option<i32>) -> Vec<UiCommand> {
        let (Some(position), Some(mut range)) =
            (self.snapshot.selected_block, self.expression_range())
        else {
            return Vec::new();
        };
        let value = if max { &mut range.max } else { &mut range.min };
        *value = match delta {
            Some(delta) => (i32::from(*value) + delta).clamp(0, 100) as u8,
            None if *value >= 100 => 0,
            None => (*value + 10).min(100),
        };
        vec![UiCommand::Expression {
            position,
            request: ExpressionRequest::Range {
                parameter: range.parameter,
                min: range.min,
                max: range.max,
            },
        }]
    }

    /// Leave the screen, keeping (`save`) or reverting the assignment.
    pub(crate) fn close_expression(&mut self, save: bool) -> Vec<UiCommand> {
        let request = if save {
            ExpressionRequest::Save
        } else {
            ExpressionRequest::Revert
        };
        let mut commands: Vec<UiCommand> = self
            .snapshot
            .selected_block
            .map(|position| UiCommand::Expression { position, request })
            .into_iter()
            .collect();
        commands.extend(self.pop());
        commands
    }
}

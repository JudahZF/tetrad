//! Owned Custom footswitch mode screens (Tetrad, `crate::custom_mode`): the slot editor
//! ([`Screen::CustomMode`]) and the Gig View page while the mode is on. Both lay the ten
//! switches out as the unit has them: A–D and Down on the top row, E–H and Tap below.
//!
//! The editor shows page tabs, PRESS · DOUBLE · COMBO, the ten tiles of the page and the
//! selected press, double press or combo of the selected switch: its label, colour (the press
//! only: it is the switch's) and its actions. A tap on an action opens the action editor: the
//! Action, Mode and Values rows of the Mappings page (the Mappings action menu,
//! `super::mappings::entries`, and its mode widgets). A tile marks a double press with 2×, and
//! a link joins the two tiles of a combo. Each control sends a [`CustomCommand`] and the page
//! shows the next snapshot.

use tetrad_assets::{Icon, IconSize, Palette, Rgb};

use super::mappings::{
    Entry, ModeRows, block_tile, caption, chevron, divider, entries, glyph, label, lines, mode_row,
    system_icon, values, values_row,
};
use crate::layout::{
    BODY, BODY_BOLD, ButtonStyle, CAPTION, Layout, ListId, Look, MEDIUM, MEDIUM_BOLD, SMALL_BOLD,
    XLARGE_BOLD,
};
use crate::render::{Align, fit, wrap};
use crate::state::{ActionMenu, ChoicePurpose, CustomEdit, CustomPending, Overlay, TextTarget};
use crate::*;

/// Editor tiles: five 150 px columns on a 158 px pitch.
const EDIT_TILE: (u32, u32) = (150, 86);
const EDIT_TOP: i32 = 108;
const DETAIL: Rect = Rect::new(8, 296, 784, 176);
const DETAIL_ROW: u32 = 58;
/// The action list under the Label row: two rows show, the others scroll.
const ACTIONS: Rect = Rect::new(8, 354, 784, 118);
const ACTION_ROW: u32 = 59;
/// The action editor's rows, as the Mappings detail page's.
const EDITOR_ROW: u32 = 64;
const EDITOR_Y: i32 = 88;
/// Page tabs and PRESS · DOUBLE · COMBO share the row under the header: pitch and width.
const TAB: (i32, u32) = (80, 72);
const GESTURE_X: i32 = 328;
/// Gig View tiles: as the stock tiles, five across.
const GIG_TILE: (u32, u32) = (150, 194);

fn rgb([red, green, blue]: [u8; 3]) -> Rgb {
    Rgb(red, green, blue)
}

/// The bounds of slot `index` in a 5 × 2 grid from `top`, rows `pitch` apart.
fn tile_bounds(index: usize, top: i32, (width, height): (u32, u32), pitch: i32) -> Rect {
    let (row, column) = match index {
        8 => (0, 4),
        9 => (1, 4),
        index => (index / 4, index % 4),
    };
    Rect::new(
        8 + column as i32 * 158,
        top + row as i32 * pitch,
        width,
        height,
    )
}

/// One slot tile: the fill in the slot's colour (full when engaged), the switch name, the
/// label and, at Gig View size, the detail line. An unavailable target is dimmed.
fn tile(
    l: &mut Layout,
    bounds: Rect,
    slot: &CustomSlotView,
    name: &str,
    large: bool,
    selected: bool,
) {
    let p = l.palette;
    let assigned = slot.action.is_some();
    let hue = rgb(slot.colour);
    let engaged = assigned && slot.engaged && slot.available;
    let fill = match (assigned, engaged) {
        (true, true) => hue,
        (true, false) => hue.mix(p.surface, 228),
        (false, _) => p.surface,
    };
    // A slot colour is data: on it, the ink is stock's.
    let ink = if engaged {
        Palette::STOCK.on_accent
    } else {
        p.text
    };
    let border = selected.then_some(p.accent);
    l.panel(bounds, fill, 8.0, border);
    if !assigned {
        l.panel(bounds.inset(4, 4), p.surface, 6.0, None);
    }
    let marks = if engaged { ink.mix(hue, 60) } else { p.muted };
    let name_font = if large { MEDIUM_BOLD } else { SMALL_BOLD };
    l.text(
        Rect::new(bounds.x + 10, bounds.y + 6, bounds.width - 20, 24),
        name,
        name_font,
        marks,
        if large { Align::Left } else { Align::Right },
    );
    if !large && slot.double.is_some() {
        let mark = Rect::new(bounds.x + 10, bounds.y + 6, 40, 24);
        l.text(mark, "2×", SMALL_BOLD, marks, Align::Left);
    }
    // Gig View names the double press, also on a switch without a press.
    let detail = match &slot.double {
        Some(double) => format!("Double: {}", double.label),
        None => slot.detail.clone(),
    };
    if large && (assigned || slot.double.is_some()) {
        l.text(
            Rect::new(bounds.x + 12, bounds.y + 32, bounds.width - 24, 22),
            fit(&detail, CAPTION, (bounds.width - 24) as f32),
            CAPTION,
            marks,
            Align::Left,
        );
    }
    if !assigned {
        l.text(bounds, "-", MEDIUM, p.faint, Align::Center);
        return;
    }
    let color = if slot.available { ink } else { p.muted };
    if large {
        let lines = wrap(&slot.label, XLARGE_BOLD, (bounds.width - 24) as f32)
            .len()
            .min(2) as i32;
        let line = XLARGE_BOLD.size.line_px() as i32;
        l.push(
            Rect::new(
                bounds.x + 12,
                bounds.bottom() - 12 - lines * line,
                bounds.width - 20,
                (lines * line) as u32,
            ),
            slot.label.clone(),
            Look::Paragraph {
                font: XLARGE_BOLD,
                color,
                align: Align::Left,
            },
        );
    } else {
        let bottom = Rect::new(bounds.x + 10, bounds.bottom() - 34, bounds.width - 20, 26);
        let label = fit(&slot.label, BODY_BOLD, bottom.width as f32);
        l.text(bottom, label, BODY_BOLD, color, Align::Left);
    }
    if !slot.available {
        let y = (bounds.y + bounds.height as i32 / 2) as f32;
        let from = (bounds.x as f32 + 10.0, y);
        l.line(from, (bounds.right() as f32 - 10.0, y), 2.0, p.muted);
    }
}

/// Gig View while the Custom mode is on: the current page's ten tiles. A tap presses and
/// releases the slot.
pub(super) fn gig(ui: &UiState, l: &mut Layout, custom: &CustomModeView) {
    let Some(page) = custom.playing().get(usize::from(custom.page)) else {
        return;
    };
    let blocked = ui.snapshot.bank_preview_active;
    for (index, slot) in page.slots.iter().enumerate() {
        let bounds = tile_bounds(index, 76, GIG_TILE, 202);
        tile(l, bounds, slot, SLOT_NAMES[index], true, false);
        let control = Control::Custom(CustomControl::Tap(index as u8));
        let enabled = slot.action.is_some() && !blocked;
        l.control(bounds, control, enabled, slot.label.clone(), Look::Area);
    }
}

/// The Gig View header's Custom controls left of the mode button: EDIT and the page button.
pub(super) fn gig_header(l: &mut Layout, custom: &CustomModeView, right: i32) {
    let p = l.palette;
    let page = Rect::new(right - 8 - 82, 8, 82, 44);
    l.panel(page, p.surface, 8.0, None);
    let text = format!("{} / {}", u16::from(custom.page) + 1, custom.pages);
    l.control(
        page,
        Control::Custom(CustomControl::NextPage),
        custom.pages > 1,
        format!("PAGE {text}"),
        Look::Area,
    );
    l.text(page, text, BODY_BOLD, p.text, Align::Center);
    let edit = Rect::new(page.x - 8 - 56, 8, 56, 44);
    l.control(
        edit,
        Control::Custom(CustomControl::Edit),
        true,
        "EDIT",
        Look::IconButton {
            icon: Icon::Edit,
            fill: p.surface,
        },
    );
}

/// The slot editor: the layout tabs, COPY, MOVE and CLEAR in the header; the page tabs and
/// COPY PAGE (and, for THIS PRESET, REMOVE) under it; the ten switches; the selected switch.
pub(super) fn layout(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let Some(custom) = &ui.snapshot.custom else {
        crate::widgets::page_header(l, "Footswitch mode", Control::Back, Icon::Check);
        l.text(
            Rect::new(8, 88, 784, 384),
            "No extension adds a footswitch mode.",
            BODY,
            p.muted,
            Align::Center,
        );
        return;
    };
    let edit = clamped(ui.custom_edit, custom);
    if let Some(index) = edit.action {
        let binding = custom.binding(edit.scope, edit.page, edit.slot, edit.gesture);
        let action = binding.and_then(|binding| binding.actions.get(usize::from(index)));
        let title = format!(
            "{} · page {}",
            gesture_name(edit.slot, edit.gesture),
            edit.page + 1
        );
        match action {
            Some(action) if edit.values => values(
                l,
                &format!("{title} values"),
                action.into(),
                custom.notice.as_deref(),
            ),
            action => action_page(ui, l, custom, edit, &title, action),
        }
        return;
    }
    crate::widgets::page_header(l, "", Control::Back, Icon::Check);
    l.text(
        Rect::new(16, 8, 176, 44),
        fit(&custom.name, MEDIUM_BOLD, 176.0),
        MEDIUM_BOLD,
        p.text,
        Align::Left,
    );
    for (index, (scope, text)) in [
        (LayoutScope::Global, "ALL PRESETS"),
        (LayoutScope::Preset, "THIS PRESET"),
    ]
    .into_iter()
    .enumerate()
    {
        let bounds = Rect::new(200 + index as i32 * 134, 8, 128, 44);
        let current = scope == edit.scope;
        let look = Look::Panel {
            fill: if current { p.panel } else { p.surface },
            radius: 8.0,
            border: None,
        };
        let control = Control::Custom(CustomControl::Scope(scope));
        l.control(bounds, control, true, text, look);
        let ink = if current { p.accent } else { p.muted };
        l.text(bounds, text, BODY_BOLD, ink, Align::Center);
    }
    let Some(layout) = custom.layout(edit.scope) else {
        // THIS PRESET without a layout of its own.
        let panel = Rect::new(8, 108, 784, 364);
        l.panel(panel, p.panel, 8.0, None);
        let text = format!(
            "{} uses the layout of all presets.",
            if custom.preset.is_empty() {
                "This preset"
            } else {
                custom.preset.as_str()
            }
        );
        l.text(
            Rect::new(panel.x, panel.y + 110, panel.width, 32),
            fit(&text, MEDIUM, (panel.width - 32) as f32),
            MEDIUM,
            p.text,
            Align::Center,
        );
        l.text(
            Rect::new(panel.x, panel.y + 142, panel.width, 28),
            "Its own layout is saved in the preset and plays while it is loaded.",
            CAPTION,
            p.muted,
            Align::Center,
        );
        l.button_in(
            Rect::new(panel.x + 242, panel.y + 196, 300, 48),
            Control::Custom(CustomControl::PresetLayout(true)),
            !custom.preset.is_empty(),
            "GIVE IT ITS OWN LAYOUT",
            ButtonStyle::Primary,
            BODY,
        );
        return;
    };
    let switch = layout
        .get(usize::from(edit.page))
        .map(|page| &page.slots[usize::from(edit.slot)]);
    let selected = custom.binding(edit.scope, edit.page, edit.slot, edit.gesture);
    let assigned = switch.is_some_and(|slot| slot.action.is_some() || slot.double.is_some());
    let pending = edit.pending;
    let header = |l: &mut Layout, x, control, label, icon, on: bool, enabled| {
        let fill = if on { p.accent_fill } else { p.surface };
        l.control(
            Rect::new(x, 8, 66, 44),
            Control::Custom(control),
            enabled,
            label,
            Look::IconButton { icon, fill },
        );
    };
    let copying = matches!(pending, Some(CustomPending::Copy { .. }));
    let moving = matches!(pending, Some(CustomPending::Move { .. }));
    header(
        l,
        476,
        CustomControl::Copy,
        "COPY",
        Icon::Copy,
        copying,
        assigned || copying,
    );
    header(
        l,
        548,
        CustomControl::Move,
        "MOVE",
        Icon::Swap,
        moving,
        true,
    );
    // PRESS clears the whole switch, its double press too.
    let clearable = selected.is_some_and(|slot| {
        let double = edit.gesture == Gesture::Press && slot.double.is_some();
        slot.action.is_some() || slot.custom_label.is_some() || double
    });
    header(
        l,
        620,
        CustomControl::Clear,
        "CLEAR",
        Icon::Trash,
        false,
        clearable,
    );
    let tab = |l: &mut Layout, x: i32, width, control, text: String, current: bool| {
        let bounds = Rect::new(x, 58, width, 42);
        let look = Look::Panel {
            fill: if current { p.panel } else { p.surface },
            radius: 8.0,
            border: None,
        };
        l.control(bounds, Control::Custom(control), true, text.clone(), look);
        let ink = if current { p.accent } else { p.muted };
        l.text(
            bounds,
            fit(&text, BODY_BOLD, width as f32 - 8.0),
            BODY_BOLD,
            ink,
            Align::Center,
        );
    };
    let (pitch, width) = TAB;
    for page in 0..custom.pages {
        let text = format!("PAGE {}", page + 1);
        let x = 8 + i32::from(page) * pitch;
        tab(
            l,
            x,
            width,
            CustomControl::Tab(page),
            text,
            page == edit.page,
        );
    }
    let combo = combo_tab(edit, layout.get(usize::from(edit.page)));
    let combo_text = match edit.gesture {
        Gesture::Combo(partner) => gesture_name(edit.slot, Gesture::Combo(partner)),
        _ => "COMBO".to_owned(),
    };
    for (index, (gesture, text)) in [
        (Gesture::Press, "PRESS".to_owned()),
        (Gesture::Double, "DOUBLE".to_owned()),
        (combo, combo_text),
    ]
    .into_iter()
    .enumerate()
    {
        let current = std::mem::discriminant(&gesture) == std::mem::discriminant(&edit.gesture);
        let x = GESTURE_X + index as i32 * pitch;
        tab(
            l,
            x,
            width + 4,
            CustomControl::Gesture(gesture),
            text,
            current,
        );
    }
    let paging = matches!(pending, Some(CustomPending::Page { .. }));
    l.button_in(
        Rect::new(576, 58, 104, 42),
        Control::Custom(CustomControl::CopyPage),
        custom.pages > 1,
        if paging { "CANCEL" } else { "COPY PAGE" },
        if paging {
            ButtonStyle::Primary
        } else {
            ButtonStyle::Inset
        },
        CAPTION,
    );
    if edit.scope == LayoutScope::Preset {
        l.button_in(
            Rect::new(688, 58, 104, 42),
            Control::Custom(CustomControl::PresetLayout(false)),
            true,
            "REMOVE",
            ButtonStyle::Danger,
            CAPTION,
        );
    }
    let Some(page) = layout.get(usize::from(edit.page)) else {
        return;
    };
    let bounds = |index: usize| tile_bounds(index, EDIT_TOP, EDIT_TILE, 94);
    for (index, slot) in page.slots.iter().enumerate() {
        let switch = index as u8;
        let current = switch == edit.slot || edit.gesture == Gesture::Combo(switch);
        tile(l, bounds(index), slot, SLOT_NAMES[index], false, current);
        let control = Control::Custom(CustomControl::Select(switch));
        l.control(bounds(index), control, true, SLOT_NAMES[index], Look::Area);
        // A combo pairs only neighbours: the others dim.
        if matches!(edit.gesture, Gesture::Combo(_))
            && switch != edit.slot
            && !adjacent(switch, edit.slot)
        {
            let veil = Look::Veil {
                color: p.canvas,
                alpha: 160,
            };
            l.push(bounds(index), "", veil);
        }
    }
    for combo in &page.combos {
        let [a, b] = combo.switches.map(|switch| bounds(usize::from(switch)));
        let (x, y) = if a.y == b.y {
            ((a.right() + b.x) / 2, a.y + a.height as i32 / 2)
        } else {
            (a.x + a.width as i32 / 2, (a.bottom() + b.y) / 2)
        };
        let link = Rect::new(x - 14, y - 8, 28, 16);
        l.panel(link, p.accent, 8.0, Some(p.canvas));
        l.text(link, "+", SMALL_BOLD, p.on_accent, Align::Center);
    }
    if let Some(switch) = switch {
        let empty = CustomSlotView::default();
        detail(
            ui,
            l,
            custom,
            edit,
            page,
            switch,
            selected.unwrap_or(&empty),
        );
    }
}

/// The gesture of the COMBO tab: the combo that shows, else the switch's first combo, else
/// its first neighbour.
fn combo_tab(edit: CustomEdit, page: Option<&CustomPageView>) -> Gesture {
    if let Gesture::Combo(_) = edit.gesture {
        return edit.gesture;
    }
    let mut neighbours = (0..CUSTOM_SLOTS as u8).filter(|&other| adjacent(edit.slot, other));
    let first = neighbours.clone().next();
    let existing = page.and_then(|page| neighbours.find(|&n| page.combo(edit.slot, n).is_some()));
    existing.or(first).map_or(Gesture::Press, Gesture::Combo)
}

/// "C, H or Down": the neighbours of `slot` but `except`.
fn neighbour_names(slot: u8, except: u8) -> String {
    let names: Vec<&str> = (0..CUSTOM_SLOTS as u8)
        .filter(|&other| other != except && adjacent(slot, other))
        .map(|other| SLOT_NAMES[usize::from(other)])
        .collect();
    match names.split_last() {
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// The editor's page and slot, within the mode's pages, a combo with a neighbour, and an open
/// action that the binding still has (or the next new one).
fn clamped(edit: CustomEdit, custom: &CustomModeView) -> CustomEdit {
    let slot = edit.slot.min(CUSTOM_SLOTS as u8 - 1);
    let edit = CustomEdit {
        page: edit.page.min(custom.pages.saturating_sub(1)),
        slot,
        gesture: match edit.gesture {
            Gesture::Combo(partner) if !adjacent(slot, partner) => Gesture::Press,
            gesture => gesture,
        },
        ..edit
    };
    let actions = (custom.binding(edit.scope, edit.page, edit.slot, edit.gesture))
        .map_or(&[][..], |binding| &binding.actions);
    let action = edit
        .action
        .filter(|&index| usize::from(index) <= actions.len().min(MAX_ACTIONS - 1));
    let fields = action
        .and_then(|index| actions.get(usize::from(index)))
        .map_or(0, |action| action.fields.len());
    CustomEdit {
        action,
        values: edit.values && fields > 2,
        ..edit
    }
}

/// The selected press, double press or combo: the Label and Colour row (the colour: the press
/// only), or what a waiting copy or move needs, or the last refusal; then its actions, one row
/// each, and Add action.
fn detail(
    ui: &UiState,
    l: &mut Layout,
    custom: &CustomModeView,
    edit: CustomEdit,
    page: &CustomPageView,
    switch: &CustomSlotView,
    slot: &CustomSlotView,
) {
    let p = l.palette;
    l.panel(DETAIL, p.panel, 8.0, None);
    let first = Rect::new(DETAIL.x, DETAIL.y, DETAIL.width, DETAIL_ROW);
    if let Some(status) = status(custom, edit) {
        let (text, color) = status;
        let text = fit(&text, MEDIUM, (first.width - 32) as f32);
        l.text(first.inset(16, 0), text, MEDIUM, color(&p), Align::Left);
    } else {
        let bounds = Rect::new(first.x, first.y, 376, first.height);
        l.control(
            bounds,
            Control::Custom(CustomControl::Label),
            slot.action.is_some(),
            "LABEL",
            Look::Area,
        );
        l.text(
            Rect::new(bounds.x + 16, bounds.y, 80, bounds.height),
            "Label",
            MEDIUM,
            p.text,
            Align::Left,
        );
        let value = slot.custom_label.as_deref().unwrap_or("Auto");
        l.text(
            Rect::new(bounds.x + 96, bounds.y, bounds.width - 112, bounds.height),
            fit(value, MEDIUM, (bounds.width - 112) as f32),
            MEDIUM,
            if slot.custom_label.is_some() {
                p.text
            } else {
                p.muted
            },
            Align::Right,
        );
        if edit.gesture == Gesture::Press {
            swatches(l, first, slot, &custom.colours);
        }
    }
    divider(l, first);
    let rows = slot.actions.len() + usize::from(slot.actions.len() < MAX_ACTIONS);
    let content = rows as u32 * ACTION_ROW;
    let offset = ui.offset(ListId::CustomActions);
    let hint = hint(custom, edit, page, switch, slot);
    l.scroll(ListId::CustomActions, ACTIONS, content, offset, |l, top| {
        for (index, action) in slot.actions.iter().enumerate() {
            let bounds = Rect::new(
                ACTIONS.x,
                top + (index as u32 * ACTION_ROW) as i32,
                ACTIONS.width,
                ACTION_ROW,
            );
            action_row(ui, l, bounds, index as u8, action);
            divider(l, bounds);
        }
        if slot.actions.len() < MAX_ACTIONS {
            let index = slot.actions.len();
            let bounds = Rect::new(
                ACTIONS.x,
                top + (index as u32 * ACTION_ROW) as i32,
                ACTIONS.width,
                ACTION_ROW,
            );
            let open = Control::Custom(CustomControl::OpenAction(index as u8));
            l.control(bounds, open, true, "ADD ACTION", Look::Area);
            let mark = Rect::new(bounds.x + 16, bounds.y + 10, 40, 40);
            glyph(l, mark, Icon::Plus, IconSize::Medium, p.accent);
            lines(
                l,
                bounds,
                (bounds.x + 70, bounds.right() - 48),
                ("Add action", &hint),
                p.accent,
            );
        }
    });
    super::scrollbar(l, ACTIONS, content, offset, 2);
}

/// One action of the list: its mark, name and detail with the mode's values.
fn action_row(ui: &UiState, l: &mut Layout, bounds: Rect, index: u8, action: &CustomActionView) {
    let p = l.palette;
    let open = Control::Custom(CustomControl::OpenAction(index));
    l.control(bounds, open, true, action.label.clone(), Look::Area);
    let mark = Rect::new(bounds.x + 16, bounds.y + 10, 40, 40);
    action_mark(ui, l, Some(&action.action), mark);
    let detail = match action.summary.as_str() {
        "" => action.detail.clone(),
        summary => format!("{} · {summary}", action.detail),
    };
    let color = if action.available { p.text } else { p.muted };
    lines(
        l,
        bounds,
        (bounds.x + 70, bounds.right() - 48),
        (&action.label, &detail),
        color,
    );
    chevron(l, bounds);
}

/// A block action shows its block tile, another action its icon.
fn action_mark(ui: &UiState, l: &mut Layout, action: Option<&SlotAction>, mark: Rect) {
    let p = l.palette;
    match action {
        Some(SlotAction::Block { position, .. }) => block_tile(l, &ui.snapshot, *position, mark),
        Some(action) => glyph(l, mark, action_icon(action), IconSize::Medium, p.text),
        None => {}
    }
}

/// The action editor: the Mappings detail page's Action panel for one action of the selected
/// binding, under the switch's name. `action` is `None` for a new action.
fn action_page(
    ui: &UiState,
    l: &mut Layout,
    custom: &CustomModeView,
    edit: CustomEdit,
    title: &str,
    action: Option<&CustomActionView>,
) {
    let p = l.palette;
    crate::widgets::page_header(l, title, Control::Back, Icon::Check);
    l.control(
        Rect::new(620, 8, 66, 44),
        Control::Custom(CustomControl::RemoveAction),
        action.is_some(),
        "REMOVE",
        Look::IconButton {
            icon: Icon::Trash,
            fill: p.surface,
        },
    );
    l.push(Rect::new(685, 18, 1, 24), "", Look::Fill(p.section));
    caption(l, 56, "Action");
    let midi = action.is_some_and(|action| action.action == SlotAction::SendMidi);
    let third = midi || action.is_some_and(|action| !action.fields.is_empty());
    let rows = if third { 3 } else { 2 };
    let panel = Rect::new(8, EDITOR_Y, 784, rows * EDITOR_ROW);
    l.panel(panel, p.panel, 8.0, None);
    let row = Rect::new(panel.x, panel.y, panel.width, EDITOR_ROW);
    l.control(
        row,
        Control::Custom(CustomControl::Actions),
        true,
        "ACTION",
        Look::Area,
    );
    let mark = Rect::new(row.x + 16, row.y + 12, 40, 40);
    action_mark(ui, l, action.map(|action| &action.action), mark);
    let (text, detail, color) = match action {
        Some(action) => (
            action.label.as_str(),
            action.detail.as_str(),
            if action.available { p.text } else { p.muted },
        ),
        None => (
            "Choose an action",
            "Scene, preset, block, MIDI, system, page or command",
            p.accent,
        ),
    };
    lines(
        l,
        row,
        (row.x + 70, row.right() - 48),
        (text, detail),
        color,
    );
    chevron(l, row);
    divider(l, row);
    let mode = Rect::new(panel.x, row.bottom(), panel.width, EDITOR_ROW);
    let empty = ModeRows {
        mode: None,
        modes: &[],
        fields: &[],
        summary: "",
        response: None,
    };
    let rows = action.map_or(empty, ModeRows::from);
    mode_row(l, rows, mode);
    let third = Rect::new(panel.x, mode.bottom(), panel.width, EDITOR_ROW);
    if midi {
        divider(l, mode);
        let messages = Control::Custom(CustomControl::Messages);
        l.control(third, messages, true, "EDIT MESSAGES", Look::Area);
        label(l, third, "Edit messages");
        chevron(l, third);
    } else if !rows.fields.is_empty() {
        divider(l, mode);
        values_row(l, rows, third);
    }
    let footer = Rect::new(16, panel.bottom() + 6, 768, 24);
    let (note, color) = match &custom.notice {
        Some(notice) => (notice.clone(), p.warning),
        None => {
            let saved = match edit.scope {
                LayoutScope::Global => "Saved for all presets.",
                LayoutScope::Preset => "Saved in this preset.",
            };
            let order = match (edit.action, ui.snapshot.custom.as_ref()) {
                (Some(index), Some(_)) if index > 0 => {
                    format!(" Runs after action {index}.")
                }
                _ => String::new(),
            };
            (format!("{saved}{order}"), p.faint)
        }
    };
    let note = fit(&note, CAPTION, footer.width as f32);
    l.text(footer, note, CAPTION, color, Align::Left);
}

/// What a waiting copy or move needs, else the last refusal, with its colour.
fn status(custom: &CustomModeView, edit: CustomEdit) -> Option<(String, fn(&Palette) -> Rgb)> {
    let waiting = match edit.pending {
        Some(CustomPending::Copy { page, slot }) => Some(format!(
            "Tap the switch that gets a copy of {} (page {})",
            SLOT_NAMES[usize::from(slot)],
            page + 1
        )),
        Some(CustomPending::Move { page, slot }) => Some(format!(
            "Tap the switch that swaps with {} (page {})",
            SLOT_NAMES[usize::from(slot)],
            page + 1
        )),
        Some(CustomPending::Page { page }) => Some(format!(
            "Tap the page tab that gets a copy of page {}",
            page + 1
        )),
        None => None,
    };
    match (waiting, &custom.notice) {
        (Some(waiting), _) => Some((waiting, |p| p.accent)),
        (None, Some(notice)) => Some((notice.clone(), |p| p.warning)),
        (None, None) => None,
    }
}

/// Why the press of `switch` waits, if it does.
fn wait_note(
    custom: &CustomModeView,
    edit: CustomEdit,
    page: &CustomPageView,
    switch: &CustomSlotView,
) -> Option<String> {
    let name = SLOT_NAMES[usize::from(edit.slot)];
    let ms = |window: std::time::Duration| window.as_millis();
    if edit.gesture != Gesture::Press {
        return None;
    }
    if switch.double.is_some() {
        Some(format!(
            "{name} also has a double press: this press waits {} ms",
            ms(custom.double_press)
        ))
    } else if (page.combos.iter()).any(|combo| combo.switches.contains(&edit.slot)) {
        Some(format!(
            "{name} is in a combo: this press waits {} ms",
            ms(custom.combo_press)
        ))
    } else {
        None
    }
}

/// Auto and the eight LED colours, right of the Label.
fn swatches(l: &mut Layout, row: Rect, slot: &CustomSlotView, colours: &[[u8; 3]; 8]) {
    let p = l.palette;
    let enabled = slot.action.is_some();
    let choices = std::iter::once(None).chain((0..colours.len() as u8).map(Some));
    for (index, swatch) in choices.enumerate() {
        let bounds = Rect::new(row.x + 392 + index as i32 * 42, row.y + 9, 40, 40);
        let chosen = slot.swatch == swatch;
        l.control(
            bounds,
            Control::Custom(CustomControl::Colour(swatch)),
            enabled,
            swatch.map_or("AUTO".to_owned(), |swatch| format!("COLOUR {}", swatch + 1)),
            Look::Area,
        );
        if chosen {
            l.panel(bounds, p.accent, 20.0, None);
            l.panel(bounds.inset(3, 3), p.panel, 17.0, None);
        }
        let dot = bounds.inset(8, 8);
        match swatch {
            Some(swatch) => l.panel(dot, rgb(colours[usize::from(swatch)]), 12.0, None),
            None => l.text(bounds, "A", BODY_BOLD, p.muted, Align::Center),
        }
    }
}

/// The Add action row's line: when the binding runs, or what to do first.
fn hint(
    custom: &CustomModeView,
    edit: CustomEdit,
    page: &CustomPageView,
    switch: &CustomSlotView,
    slot: &CustomSlotView,
) -> String {
    let wait = wait_note(custom, edit, page, switch);
    let playing = match (custom.preset_slots.is_some(), edit.scope) {
        (true, LayoutScope::Global) => "This preset plays its own layout. ",
        _ => "",
    };
    let name = SLOT_NAMES[usize::from(edit.slot)];
    let ran = match edit.gesture {
        Gesture::Press => match &wait {
            Some(wait) => format!("{wait}."),
            None => "Runs on each press of the switch.".to_owned(),
        },
        Gesture::Double => format!(
            "Runs when {name} is pressed twice within {} ms.",
            custom.double_press.as_millis()
        ),
        Gesture::Combo(partner) => {
            let [first, second] = [edit.slot.min(partner), edit.slot.max(partner)];
            format!(
                "Runs when {} and {} are pressed together. Tap {} for another pair.",
                SLOT_NAMES[usize::from(first)],
                SLOT_NAMES[usize::from(second)],
                neighbour_names(edit.slot, partner)
            )
        }
    };
    match (
        slot.action.is_some() || edit.gesture != Gesture::Press,
        custom.active,
    ) {
        (true, _) => format!("{playing}{ran}"),
        (false, _) if !playing.is_empty() => format!("{playing}{ran}"),
        (false, true) => "Scene, preset, block, MIDI, system, page or command".to_owned(),
        (false, false) => "Off: the mode button or Down + Tap turns it on.".to_owned(),
    }
}

fn action_icon(action: &SlotAction) -> Icon {
    match action {
        SlotAction::System(action) => system_icon(*action),
        SlotAction::Block { .. } => Icon::Footswitch,
        SlotAction::LoadPreset | SlotAction::PresetSlot(_) => Icon::SaveArrow,
        SlotAction::SendMidi => Icon::Midi,
        SlotAction::Command { .. } => Icon::Plugin,
    }
}

impl UiState {
    pub(crate) fn custom_control(&mut self, control: CustomControl) -> Vec<UiCommand> {
        use CustomControl as C;
        let Some(custom) = self.snapshot.custom.clone() else {
            return Vec::new();
        };
        let edit = clamped(self.custom_edit, &custom);
        let CustomEdit {
            scope,
            page,
            slot,
            gesture,
            ..
        } = edit;
        let send = |edit| {
            vec![UiCommand::Custom(CustomCommand::Slot {
                scope,
                page,
                slot,
                gesture,
                edit,
            })]
        };
        let selected = custom
            .binding(scope, page, slot, gesture)
            .cloned()
            .unwrap_or_default();
        let open = edit
            .action
            .filter(|&index| usize::from(index) < selected.actions.len());
        match control {
            C::Tap(slot) => vec![UiCommand::Custom(CustomCommand::Tap { slot })],
            C::NextPage => vec![UiCommand::Custom(CustomCommand::Page(PageStep::Next))],
            C::Edit => {
                self.custom_edit = CustomEdit {
                    scope: match custom.preset_slots {
                        Some(_) => LayoutScope::Preset,
                        None => LayoutScope::Global,
                    },
                    page: custom.page,
                    ..CustomEdit::default()
                };
                self.navigate(Screen::CustomMode)
            }
            C::Scope(scope) => {
                self.custom_edit = CustomEdit {
                    scope,
                    pending: None,
                    ..edit
                };
                Vec::new()
            }
            C::Tab(to) => {
                self.custom_edit = clamped(CustomEdit { page: to, ..edit }, &custom);
                match edit.pending {
                    Some(CustomPending::Page { page: from }) => {
                        self.custom_edit.pending = None;
                        if from == to {
                            return Vec::new();
                        }
                        vec![UiCommand::Custom(CustomCommand::CopyPage {
                            scope,
                            from,
                            to,
                        })]
                    }
                    _ => Vec::new(),
                }
            }
            C::Gesture(gesture) => {
                self.custom_edit = clamped(
                    CustomEdit {
                        gesture,
                        pending: None,
                        ..edit
                    },
                    &custom,
                );
                Vec::new()
            }
            // While a combo shows, a neighbour becomes the partner.
            C::Select(to)
                if edit.pending.is_none()
                    && matches!(gesture, Gesture::Combo(_))
                    && adjacent(slot, to) =>
            {
                self.custom_edit.gesture = Gesture::Combo(to);
                Vec::new()
            }
            // Another switch: its press, or its double press while DOUBLE shows.
            C::Select(to) => {
                let gesture = match gesture {
                    Gesture::Combo(_) => Gesture::Press,
                    gesture => gesture,
                };
                self.custom_edit = clamped(
                    CustomEdit {
                        slot: to,
                        gesture,
                        pending: None,
                        ..edit
                    },
                    &custom,
                );
                let (from, edit) = match edit.pending {
                    Some(CustomPending::Copy { page: p, slot: s }) => {
                        ((p, s), SlotEdit::CopyTo { page, slot: to })
                    }
                    Some(CustomPending::Move { page: p, slot: s }) => {
                        ((p, s), SlotEdit::MoveTo { page, slot: to })
                    }
                    _ => return Vec::new(),
                };
                if from == (page, to) {
                    return Vec::new();
                }
                vec![UiCommand::Custom(CustomCommand::Slot {
                    scope,
                    page: from.0,
                    slot: from.1,
                    gesture: Gesture::Press,
                    edit,
                })]
            }
            C::Copy | C::Move | C::CopyPage => {
                let wanted = match control {
                    C::Copy => CustomPending::Copy { page, slot },
                    C::Move => CustomPending::Move { page, slot },
                    _ => CustomPending::Page { page },
                };
                let same = edit.pending.is_some_and(|pending| {
                    std::mem::discriminant(&pending) == std::mem::discriminant(&wanted)
                });
                self.custom_edit.pending = (!same).then_some(wanted);
                Vec::new()
            }
            C::PresetLayout(own) => {
                self.custom_edit.pending = None;
                vec![UiCommand::Custom(CustomCommand::PresetLayout(own))]
            }
            // A new action opens its editor with the action menu over it.
            C::OpenAction(index) => {
                self.custom_edit.action = Some(index);
                self.custom_edit.values = false;
                if usize::from(index) >= selected.actions.len() {
                    self.reset_scroll(ListId::MappingActions);
                    self.overlay = Some(Overlay::MappingActions(ActionMenu::Root));
                }
                Vec::new()
            }
            C::Actions => {
                self.reset_scroll(ListId::MappingActions);
                self.overlay = Some(Overlay::MappingActions(ActionMenu::Root));
                Vec::new()
            }
            C::RemoveAction => match open {
                Some(index) => {
                    self.custom_edit.action = None;
                    self.custom_edit.values = false;
                    send(SlotEdit::Action {
                        index,
                        edit: ActionEdit::Remove,
                    })
                }
                None => Vec::new(),
            },
            C::Label if selected.action.is_some() => {
                let current = selected.custom_label.unwrap_or_default();
                self.open_keyboard(TextTarget::SlotLabel(scope, page, slot, gesture), current);
                Vec::new()
            }
            C::Colour(swatch)
                if gesture == Gesture::Press
                    && selected.action.is_some()
                    && swatch != selected.swatch =>
            {
                send(SlotEdit::SetColour(swatch))
            }
            C::Messages
                if open.is_some_and(|index| {
                    selected.actions[usize::from(index)].action == SlotAction::SendMidi
                }) =>
            {
                let target = MidiOutTarget::Slot {
                    scope,
                    page,
                    slot,
                    gesture,
                    action: open.unwrap_or_default(),
                };
                self.midi_edit = Some((target, 0));
                self.navigate(Screen::MidiOut)
            }
            C::Clear => send(SlotEdit::Clear),
            C::Label | C::Colour(_) | C::Messages => Vec::new(),
        }
    }

    /// The open action's Mode dropdown, Values row and steppers (the Mappings widgets).
    pub(crate) fn custom_mode_control(&mut self, control: MappingControl) -> Vec<UiCommand> {
        let Some(custom) = self.snapshot.custom.as_ref() else {
            return Vec::new();
        };
        let edit = clamped(self.custom_edit, custom);
        let Some(index) = edit.action else {
            return Vec::new();
        };
        let Some(action) = (custom.binding(edit.scope, edit.page, edit.slot, edit.gesture))
            .and_then(|binding| binding.actions.get(usize::from(index)))
        else {
            return Vec::new();
        };
        let send = |action_edit| {
            vec![UiCommand::Custom(CustomCommand::Slot {
                scope: edit.scope,
                page: edit.page,
                slot: edit.slot,
                gesture: edit.gesture,
                edit: SlotEdit::Action {
                    index,
                    edit: action_edit,
                },
            })]
        };
        match control {
            MappingControl::Choice(MappingChoice::Mode) if !action.modes.is_empty() => {
                let kind = action.mode.as_ref().map(|mode| mode.kind);
                let current = action.modes.iter().position(|mode| Some(mode.kind) == kind);
                let purpose = ChoicePurpose::Mapping(MappingChoice::Mode);
                self.open_choice(purpose, current.unwrap_or(0));
                Vec::new()
            }
            MappingControl::Field(field, ticks)
                if action.fields.iter().any(|shown| shown.id == field) =>
            {
                send(ActionEdit::Field {
                    field,
                    ticks: i32::from(ticks),
                })
            }
            MappingControl::Values => {
                self.custom_edit.values = action.fields.len() > 2;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The open action's mode menu: its labels and the current one.
    pub(crate) fn custom_modes(&self) -> Option<(Vec<&str>, Option<usize>)> {
        let action = self.custom_action()?;
        let kind = action.mode.as_ref().map(|mode| mode.kind);
        let current = action.modes.iter().position(|mode| Some(mode.kind) == kind);
        Some((
            action
                .modes
                .iter()
                .map(|mode| mode.label.as_str())
                .collect(),
            current,
        ))
    }

    /// A pick from the open action's mode menu.
    pub(crate) fn choose_custom_mode(&self, index: usize) -> Vec<UiCommand> {
        let (Some(custom), Some(action)) = (self.snapshot.custom.as_ref(), self.custom_action())
        else {
            return Vec::new();
        };
        let edit = clamped(self.custom_edit, custom);
        let current = action.mode.as_ref().map(|mode| mode.kind);
        match (action.modes.get(index), edit.action) {
            (Some(mode), Some(at)) if Some(mode.kind) != current => {
                vec![UiCommand::Custom(CustomCommand::Slot {
                    scope: edit.scope,
                    page: edit.page,
                    slot: edit.slot,
                    gesture: edit.gesture,
                    edit: SlotEdit::Action {
                        index: at,
                        edit: ActionEdit::Mode(mode.kind),
                    },
                })]
            }
            _ => Vec::new(),
        }
    }

    /// The action whose editor is open.
    fn custom_action(&self) -> Option<&CustomActionView> {
        let custom = self.snapshot.custom.as_ref()?;
        let edit = clamped(self.custom_edit, custom);
        let binding = custom.binding(edit.scope, edit.page, edit.slot, edit.gesture)?;
        binding.actions.get(usize::from(edit.action?))
    }

    /// Row `index` of the action menu over the slot editor. Opening a setlist also makes it the
    /// browsed one, so its presets come with the next snapshot.
    pub(crate) fn custom_pick(&mut self, index: u16) -> Vec<UiCommand> {
        let (Some(Overlay::MappingActions(menu)), Some(custom)) =
            (&self.overlay, self.snapshot.custom.as_ref())
        else {
            return Vec::new();
        };
        let menu = *menu;
        let CustomEdit {
            scope,
            page,
            slot,
            gesture,
            ..
        } = clamped(self.custom_edit, custom);
        let actions = (custom.binding(scope, page, slot, gesture))
            .map_or(&[][..], |binding| &binding.actions);
        let at = (self.custom_edit.action).unwrap_or(actions.len().min(MAX_ACTIONS) as u8);
        let current = actions
            .get(usize::from(at))
            .map(|action| action.action.clone());
        let (next, commands) = match entries(self, menu).into_iter().nth(usize::from(index)) {
            Some(Entry::Open {
                menu: ActionMenu::Setlist(id),
                ..
            }) => (ActionMenu::Setlist(id), vec![UiCommand::OpenSetlist { id }]),
            Some(Entry::Open { menu, .. }) => (menu, Vec::new()),
            Some(Entry::Back(..)) => (ActionMenu::Root, Vec::new()),
            Some(Entry::Set { action, .. }) => {
                self.overlay = None;
                // "Load" takes the loaded preset again, so it is never the same action.
                if current.as_ref() == Some(&action) && action != SlotAction::LoadPreset {
                    return Vec::new();
                }
                self.custom_edit.action = Some(at);
                return vec![UiCommand::Custom(CustomCommand::Slot {
                    scope,
                    page,
                    slot,
                    gesture,
                    edit: SlotEdit::Action {
                        index: at,
                        edit: ActionEdit::Set(action),
                    },
                })];
            }
            Some(Entry::Header(_)) | None => return Vec::new(),
        };
        self.reset_scroll(ListId::MappingActions);
        self.overlay = Some(Overlay::MappingActions(next));
        commands
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::screens::mappings::tests::{pick, sample};
    use crate::tests::activate;

    fn mode(kind: ModeKind, label: &str) -> ModeView {
        ModeView {
            kind,
            label: label.into(),
            detail: "Detail".into(),
        }
    }

    /// Two pages; page 2's A toggles the Green Drive and sets its Drive.
    pub(crate) fn view(active: bool) -> CustomModeView {
        let mut page = <[CustomSlotView; CUSTOM_SLOTS]>::default();
        let drive = GridPosition::new(0, 1).unwrap();
        let bypass = SlotAction::Block {
            position: drive,
            target: BlockTarget::Bypass,
        };
        page[0] = CustomSlotView {
            action: Some(bypass.clone()),
            actions: vec![
                CustomActionView {
                    action: bypass,
                    label: "Green Drive".into(),
                    detail: "Bypass · Toggle".into(),
                    available: true,
                    mode: Some(mode(ModeKind::Toggle, "Toggle")),
                    modes: vec![
                        mode(ModeKind::Toggle, "Toggle"),
                        mode(ModeKind::Momentary, "Momentary"),
                        mode(ModeKind::Set, "Set"),
                    ],
                    fields: Vec::new(),
                    summary: String::new(),
                    messages: Vec::new(),
                },
                CustomActionView {
                    action: SlotAction::Block {
                        position: drive,
                        target: BlockTarget::Parameter(2),
                    },
                    label: "Green Drive".into(),
                    detail: "Drive · Set".into(),
                    available: true,
                    mode: Some(mode(ModeKind::Set, "Set")),
                    modes: vec![mode(ModeKind::Set, "Set"), mode(ModeKind::Toggle, "Toggle")],
                    fields: vec![ValueField {
                        id: FieldId::Value,
                        label: "Value".into(),
                        text: "6.5".into(),
                    }],
                    summary: "6.5".into(),
                    messages: Vec::new(),
                },
            ],
            label: "Green Drive".into(),
            detail: "Bypass · Toggle · +1".into(),
            colour: [0x45, 0xf8, 0x62],
            available: true,
            ..CustomSlotView::default()
        };
        let page = CustomPageView {
            slots: page,
            combos: Vec::new(),
        };
        CustomModeView {
            name: "Custom".into(),
            extension: "Custom Footswitch Mode".into(),
            pages: 2,
            page: 0,
            active,
            slots: vec![Default::default(), page],
            preset_slots: None,
            preset: "Studio Crunch".into(),
            commands: Vec::new(),
            colours: [[255; 3]; 8],
            notice: None,
            double_press: DOUBLE_PRESS,
            combo_press: COMBO_PRESS,
        }
    }

    fn open(screen: Screen, active: bool) -> UiState {
        let mut ui = UiState::new();
        ui.set_snapshot(UiSnapshot {
            mappings: sample(),
            custom: Some(view(active)),
            footswitch_mode: Some(if active {
                FootswitchMode::Custom
            } else {
                FootswitchMode::Stomp
            }),
            ..UiSnapshot::default()
        })
        .expect("valid snapshot");
        ui.navigate(screen);
        ui
    }

    fn custom(control: CustomControl) -> Control {
        Control::Custom(control)
    }

    fn sent(command: CustomCommand) -> Vec<UiCommand> {
        vec![UiCommand::Custom(command)]
    }

    fn global(page: u8, slot: u8, edit: SlotEdit) -> Vec<UiCommand> {
        sent(CustomCommand::Slot {
            scope: LayoutScope::Global,
            page,
            slot,
            gesture: Gesture::Press,
            edit,
        })
    }

    fn set(index: u8, action: SlotAction) -> SlotEdit {
        SlotEdit::Action {
            index,
            edit: ActionEdit::Set(action),
        }
    }

    /// Add action opens the action editor with the shared menu over it; the pick is the new
    /// action, and the editor stays on it. An action's editor has the Mappings Mode dropdown
    /// and value steppers, and its trash button removes it.
    #[test]
    fn the_editor_adds_and_edits_actions_with_the_mappings_rows() {
        let mut ui = open(Screen::CustomMode, false);
        activate(&mut ui, custom(CustomControl::Select(8)));
        activate(&mut ui, custom(CustomControl::OpenAction(0)));
        pick(&mut ui, "Page");
        let next = SlotAction::System(SystemAction::CustomPage(PageStep::Next));
        assert_eq!(pick(&mut ui, "Next page"), global(0, 8, set(0, next)));
        activate(&mut ui, custom(CustomControl::Actions));
        pick(&mut ui, "Preset");
        assert_eq!(
            pick(&mut ui, "Load Studio Crunch"),
            global(0, 8, set(0, SlotAction::LoadPreset))
        );
        ui.back();
        activate(&mut ui, custom(CustomControl::Tab(1)));
        activate(&mut ui, custom(CustomControl::Select(0)));
        // The second action: its mode and its value.
        activate(&mut ui, custom(CustomControl::OpenAction(1)));
        let map = |control| Control::Mapping(control);
        activate(&mut ui, map(MappingControl::Choice(MappingChoice::Mode)));
        let action = |edit| global(1, 0, SlotEdit::Action { index: 1, edit });
        assert_eq!(
            activate(&mut ui, Control::Choose(1)),
            action(ActionEdit::Mode(ModeKind::Toggle))
        );
        assert_eq!(
            activate(&mut ui, map(MappingControl::Field(FieldId::Value, 1))),
            action(ActionEdit::Field {
                field: FieldId::Value,
                ticks: 1,
            })
        );
        assert_eq!(
            activate(&mut ui, custom(CustomControl::RemoveAction)),
            action(ActionEdit::Remove)
        );
        assert_eq!(ui.custom_edit.action, None, "the editor closes");
        // A third action goes after the two: Add action scrolls into view under them.
        let add = custom(CustomControl::OpenAction(2));
        let shown = |ui: &UiState| ui.hit_regions().iter().any(|region| region.control == add);
        assert!(!shown(&ui));
        ui.handle_input(InputEvent::PointerDown { x: 400, y: 460 });
        ui.handle_input(InputEvent::PointerMove { x: 400, y: 360 });
        ui.handle_input(InputEvent::PointerUp { x: 400, y: 360 });
        assert!(shown(&ui));
        activate(&mut ui, add);
        assert_eq!(
            pick(&mut ui, "Tuner"),
            global(1, 0, set(2, SlotAction::System(SystemAction::Tuner)))
        );
    }

    /// COMBO opens on the switch's first neighbour; a neighbour's tile becomes the partner, and
    /// another tile selects that switch's press. The edits name the action that shows.
    #[test]
    fn combo_edits_take_a_neighbour_as_the_partner() {
        let mut ui = open(Screen::CustomMode, false);
        let edit = |slot, gesture, edit| {
            sent(CustomCommand::Slot {
                scope: LayoutScope::Global,
                page: 0,
                slot,
                gesture,
                edit,
            })
        };
        let next = set(
            0,
            SlotAction::System(SystemAction::CustomPage(PageStep::Next)),
        );
        let choose = |ui: &mut UiState| {
            activate(ui, custom(CustomControl::OpenAction(0)));
            pick(ui, "Page");
            let sent = pick(ui, "Next page");
            ui.back();
            sent
        };
        activate(&mut ui, custom(CustomControl::Select(3)));
        activate(&mut ui, custom(CustomControl::Gesture(Gesture::Double)));
        assert_eq!(choose(&mut ui), edit(3, Gesture::Double, next.clone()));
        activate(&mut ui, custom(CustomControl::Gesture(Gesture::Combo(2))));
        activate(&mut ui, custom(CustomControl::Select(7)));
        assert_eq!(choose(&mut ui), edit(3, Gesture::Combo(7), next.clone()));
        // G is not next to D (only next to H): it is selected, on its press.
        activate(&mut ui, custom(CustomControl::Select(6)));
        assert_eq!(choose(&mut ui), edit(6, Gesture::Press, next));
    }

    /// COPY and MOVE wait for the target switch (on any page); COPY PAGE waits for the target
    /// page tab; tapping the button again or Back cancels.
    #[test]
    fn copy_move_and_copy_page_take_their_target_from_the_next_tap() {
        let mut ui = open(Screen::CustomMode, false);
        activate(&mut ui, custom(CustomControl::Tab(1)));
        activate(&mut ui, custom(CustomControl::Copy));
        activate(&mut ui, custom(CustomControl::Tab(0)));
        assert_eq!(
            activate(&mut ui, custom(CustomControl::Select(9))),
            global(1, 0, SlotEdit::CopyTo { page: 0, slot: 9 })
        );
        activate(&mut ui, custom(CustomControl::Move));
        assert_eq!(
            activate(&mut ui, custom(CustomControl::Select(3))),
            global(0, 9, SlotEdit::MoveTo { page: 0, slot: 3 })
        );
        activate(&mut ui, custom(CustomControl::Move));
        activate(&mut ui, custom(CustomControl::Move));
        assert!(activate(&mut ui, custom(CustomControl::Select(4))).is_empty());
        activate(&mut ui, custom(CustomControl::Move));
        ui.back();
        assert_eq!(
            ui.screen(),
            Screen::CustomMode,
            "Back cancels the move first"
        );
        assert!(activate(&mut ui, custom(CustomControl::Select(5))).is_empty());
        activate(&mut ui, custom(CustomControl::CopyPage));
        assert_eq!(
            activate(&mut ui, custom(CustomControl::Tab(1))),
            sent(CustomCommand::CopyPage {
                scope: LayoutScope::Global,
                from: 0,
                to: 1,
            })
        );
    }

    /// THIS PRESET without a layout offers one; with one, its edits name that layout.
    #[test]
    fn this_preset_gets_and_edits_its_own_layout() {
        let mut ui = open(Screen::CustomMode, false);
        activate(&mut ui, custom(CustomControl::Scope(LayoutScope::Preset)));
        assert_eq!(
            activate(&mut ui, custom(CustomControl::PresetLayout(true))),
            sent(CustomCommand::PresetLayout(true))
        );
        let mut own = view(false);
        own.preset_slots = Some(own.slots.clone());
        let snapshot = UiSnapshot {
            custom: Some(own),
            ..ui.snapshot().clone()
        };
        ui.set_snapshot(snapshot).expect("valid snapshot");
        activate(&mut ui, custom(CustomControl::Tab(1)));
        assert_eq!(
            activate(&mut ui, custom(CustomControl::Clear)),
            sent(CustomCommand::Slot {
                scope: LayoutScope::Preset,
                page: 1,
                slot: 0,
                gesture: Gesture::Press,
                edit: SlotEdit::Clear,
            })
        );
        assert_eq!(
            activate(&mut ui, custom(CustomControl::PresetLayout(false))),
            sent(CustomCommand::PresetLayout(false))
        );
    }

    /// "Choose a preset" lists the setlists; opening one browses it, and its filled slots load
    /// by slot.
    #[test]
    fn a_slot_chooses_any_preset_of_a_setlist() {
        let mut ui = open(Screen::CustomMode, false);
        let mut snapshot = ui.snapshot().clone();
        snapshot.setlists = vec![SetlistItem {
            id: 3,
            title: "Live Set".into(),
        }];
        ui.set_snapshot(snapshot).expect("valid snapshot");
        activate(&mut ui, custom(CustomControl::OpenAction(0)));
        pick(&mut ui, "Preset");
        pick(&mut ui, "Choose a preset");
        assert_eq!(
            pick(&mut ui, "Live Set"),
            vec![UiCommand::OpenSetlist { id: 3 }]
        );
        let mut snapshot = ui.snapshot().clone();
        snapshot.active_setlist = Some(3);
        let slot = |id, title: &str| DirectorySlot {
            title: title.into(),
            id,
            key: id + 1,
            empty: title.is_empty(),
            author: String::new(),
            date: String::new(),
        };
        std::sync::Arc::make_mut(&mut snapshot.directory).slots =
            vec![slot(0, ""), slot(9, "Crunch")];
        ui.set_snapshot(snapshot).expect("valid snapshot");
        assert_eq!(
            pick(&mut ui, "2B Crunch"),
            global(0, 0, set(0, SlotAction::PresetSlot(9)))
        );
    }

    /// A Mappings trigger can turn the mode on, turn its pages and press its slots; a slot's
    /// own menu has no slot presses.
    #[test]
    fn mappings_offer_the_custom_mode_and_its_slots() {
        let mut ui = open(Screen::Mappings, false);
        activate(&mut ui, Control::Mapping(MappingControl::Open(2)));
        activate(&mut ui, Control::Mapping(MappingControl::Actions));
        pick(&mut ui, "Custom");
        assert_eq!(
            pick(&mut ui, "Press Tap"),
            vec![UiCommand::Mapping(MappingCommand::SetAction {
                scope: MappingScope::Preset,
                id: MappingId::Tetrad(0),
                action: MappingAction::System(SystemAction::CustomSlot(9)),
            })]
        );
        let mut editor = open(Screen::CustomMode, false);
        editor.custom_edit.slot = 3;
        editor.reset_scroll(ListId::MappingActions);
        editor.overlay = Some(Overlay::MappingActions(ActionMenu::Custom));
        let labels: Vec<_> = (entries(&editor, ActionMenu::Custom).iter())
            .filter_map(|entry| match entry {
                Entry::Set { label, .. } => Some(label.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["Next page", "Previous page", "Page 1", "Page 2"]);
    }

    /// A switch with only a double press is not an empty tile in Gig View: it names the double
    /// press.
    #[test]
    fn gig_view_names_a_double_press_without_a_press() {
        let mut only_double = view(true);
        only_double.slots[0].slots[5].double = Some(Box::new(CustomSlotView {
            action: Some(SlotAction::System(SystemAction::Tuner)),
            label: "Tuner".into(),
            ..CustomSlotView::default()
        }));
        let mut ui = open(Screen::Gig, true);
        let snapshot = UiSnapshot {
            custom: Some(only_double),
            ..ui.snapshot().clone()
        };
        ui.set_snapshot(snapshot).expect("valid snapshot");
        let named = (ui.layout().items.iter())
            .any(|item| matches!(item.look, Look::Text { .. }) && item.label == "Double: Tuner");
        assert!(named);
    }

    /// While the mode is on, Gig View shows the ten slots of the playing layout's current page;
    /// a tile taps its slot and the page button turns the page.
    #[test]
    fn gig_view_taps_the_slots_of_the_current_page() {
        let mut ui = open(Screen::Gig, true);
        assert!(
            !ui.hit_regions()
                .iter()
                .any(|r| r.control == custom(CustomControl::Tap(0)) && r.enabled)
        );
        let mut on_page_two = view(true);
        on_page_two.page = 1;
        let snapshot = UiSnapshot {
            custom: Some(on_page_two),
            ..ui.snapshot().clone()
        };
        ui.set_snapshot(snapshot).expect("valid snapshot");
        assert_eq!(
            activate(&mut ui, custom(CustomControl::Tap(0))),
            sent(CustomCommand::Tap { slot: 0 })
        );
        assert_eq!(
            activate(&mut ui, custom(CustomControl::NextPage)),
            sent(CustomCommand::Page(PageStep::Next))
        );
    }
}

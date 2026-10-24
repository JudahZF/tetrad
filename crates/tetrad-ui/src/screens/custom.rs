//! Owned Custom footswitch mode screens (Tetrad, `crate::custom_mode`): the slot editor
//! ([`Screen::CustomMode`]) and the Gig View page while the mode is on. Both lay the ten
//! switches out as the unit has them: A–D and Down on the top row, E–H and Tap below.
//!
//! The editor shows page tabs, the ten tiles of the page and the selected slot: its action
//! (the Mappings action menu, `super::mappings::entries`), label, colour and the setting of
//! its action (a parameter's value, a bypass's Momentary switch, the MIDI messages). Each
//! control sends a [`CustomCommand`] and the page shows the next snapshot.

use tetrad_assets::{Icon, IconSize, Palette, Rgb};

use super::mappings::{Entry, block_tile, chevron, divider, entries, glyph, lines, system_icon};
use crate::layout::{
    BODY, BODY_BOLD, ButtonStyle, CAPTION, Layout, ListId, Look, MEDIUM, MEDIUM_BOLD, SMALL_BOLD,
    XLARGE_BOLD,
};
use crate::render::{Align, fit, wrap};
use crate::state::{ActionMenu, CustomEdit, CustomPending, Overlay, TextTarget};
use crate::*;

/// Editor tiles: five 150 px columns on a 158 px pitch.
const EDIT_TILE: (u32, u32) = (150, 86);
const EDIT_TOP: i32 = 108;
const DETAIL: Rect = Rect::new(8, 296, 784, 176);
const DETAIL_ROW: u32 = 58;
/// Gig View tiles: as the stock tiles, five across.
const GIG_TILE: (u32, u32) = (150, 194);
const VALUE_STEP: i32 = 5;

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
    if !assigned {
        l.text(bounds, "-", MEDIUM, p.faint, Align::Center);
        return;
    }
    let color = if slot.available { ink } else { p.muted };
    if large {
        l.text(
            Rect::new(bounds.x + 12, bounds.y + 32, bounds.width - 24, 22),
            fit(&slot.detail, CAPTION, (bounds.width - 24) as f32),
            CAPTION,
            marks,
            Align::Left,
        );
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
    for (index, slot) in page.iter().enumerate() {
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
    let selected = layout
        .get(usize::from(edit.page))
        .map(|page| &page[usize::from(edit.slot)]);
    let assigned = selected.is_some_and(|slot| slot.action.is_some());
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
    let clearable =
        selected.is_some_and(|slot| slot.action.is_some() || slot.custom_label.is_some());
    header(
        l,
        620,
        CustomControl::Clear,
        "CLEAR",
        Icon::Trash,
        false,
        clearable,
    );
    for page in 0..custom.pages {
        let bounds = Rect::new(8 + i32::from(page) * 104, 58, 96, 42);
        let current = page == edit.page;
        let look = Look::Panel {
            fill: if current { p.panel } else { p.surface },
            radius: 8.0,
            border: None,
        };
        let text = format!("PAGE {}", page + 1);
        let control = Control::Custom(CustomControl::Tab(page));
        l.control(bounds, control, true, text.clone(), look);
        let ink = if current { p.accent } else { p.muted };
        l.text(bounds, text, BODY_BOLD, ink, Align::Center);
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
    for (index, slot) in page.iter().enumerate() {
        let bounds = tile_bounds(index, EDIT_TOP, EDIT_TILE, 94);
        let current = index == usize::from(edit.slot);
        tile(l, bounds, slot, SLOT_NAMES[index], false, current);
        let control = Control::Custom(CustomControl::Select(index as u8));
        l.control(bounds, control, true, SLOT_NAMES[index], Look::Area);
    }
    if let Some(slot) = selected {
        detail(ui, l, custom, edit, slot);
    }
}

/// The editor's page and slot, within the mode's pages.
fn clamped(edit: CustomEdit, custom: &CustomModeView) -> CustomEdit {
    CustomEdit {
        page: edit.page.min(custom.pages.saturating_sub(1)),
        slot: edit.slot.min(CUSTOM_SLOTS as u8 - 1),
        ..edit
    }
}

/// The selected slot: Action, then Label and Colour, then the action's setting.
fn detail(
    ui: &UiState,
    l: &mut Layout,
    custom: &CustomModeView,
    edit: CustomEdit,
    slot: &CustomSlotView,
) {
    let p = l.palette;
    l.panel(DETAIL, p.panel, 8.0, None);
    let action = Rect::new(DETAIL.x, DETAIL.y, DETAIL.width, 60);
    l.control(
        action,
        Control::Custom(CustomControl::Actions),
        true,
        "ACTION",
        Look::Area,
    );
    let mark = Rect::new(action.x + 16, action.y + 10, 40, 40);
    match &slot.action {
        Some(SlotAction::Block { position, .. }) => block_tile(l, &ui.snapshot, *position, mark),
        Some(action) => glyph(l, mark, action_icon(action), IconSize::Medium, p.text),
        None => {}
    }
    let name = SLOT_NAMES[usize::from(edit.slot)];
    let (text, detail, color) = match &slot.action {
        None => (
            format!("Switch {name}: choose an action"),
            "Scene, preset, block, MIDI, system, page or command".to_owned(),
            p.accent,
        ),
        Some(_) => (slot.label.clone(), slot.detail.clone(), p.text),
    };
    lines(
        l,
        action,
        (action.x + 70, action.right() - 48),
        (&text, &detail),
        color,
    );
    chevron(l, action);
    divider(l, action);
    let second = Rect::new(DETAIL.x, action.bottom(), DETAIL.width, DETAIL_ROW);
    let label = Rect::new(second.x, second.y, 376, second.height);
    l.control(
        label,
        Control::Custom(CustomControl::Label),
        slot.action.is_some(),
        "LABEL",
        Look::Area,
    );
    l.text(
        Rect::new(label.x + 16, label.y, 80, label.height),
        "Label",
        MEDIUM,
        p.text,
        Align::Left,
    );
    let value = slot.custom_label.as_deref().unwrap_or("Auto");
    l.text(
        Rect::new(label.x + 96, label.y, label.width - 112, label.height),
        fit(value, MEDIUM, (label.width - 112) as f32),
        MEDIUM,
        if slot.custom_label.is_some() {
            p.text
        } else {
            p.muted
        },
        Align::Right,
    );
    swatches(l, second, slot, &custom.colours);
    divider(l, second);
    let third = Rect::new(DETAIL.x, second.bottom(), DETAIL.width, DETAIL_ROW);
    setting(l, third, custom, edit, slot);
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

/// The last row: what a waiting copy or move needs, the last refusal, or the setting of the
/// action.
fn setting(
    l: &mut Layout,
    row: Rect,
    custom: &CustomModeView,
    edit: CustomEdit,
    slot: &CustomSlotView,
) {
    let p = l.palette;
    let title = |l: &mut Layout, text: &str| {
        l.text(
            Rect::new(row.x + 16, row.y, 300, row.height),
            text,
            MEDIUM,
            p.text,
            Align::Left,
        );
    };
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
    if let Some(waiting) = waiting {
        let text = fit(&waiting, MEDIUM, (row.width - 32) as f32);
        l.text(row.inset(16, 0), text, MEDIUM, p.accent, Align::Left);
        return;
    }
    if let Some(notice) = &custom.notice {
        let notice = fit(notice, MEDIUM, (row.width - 32) as f32);
        l.text(row.inset(16, 0), notice, MEDIUM, p.warning, Align::Left);
        return;
    }
    if let Some(percent) = slot.value {
        title(l, "Value");
        super::stepper(
            l,
            Rect::new(row.right() - 228, row.y + 7, 212, 44),
            Control::Custom(CustomControl::Value(-1)),
            Control::Custom(CustomControl::Value(1)),
            format!("{percent} %"),
            true,
        );
    } else if let Some(momentary) = slot.momentary {
        title(l, "Momentary");
        let note = if momentary {
            "The block changes only while the switch is down"
        } else {
            "Each press toggles the block"
        };
        l.text(
            Rect::new(row.x + 180, row.y, 440, row.height),
            note,
            CAPTION,
            p.muted,
            Align::Left,
        );
        let bounds = Rect::new(row.right() - 96, row.y + 9, 80, 40);
        l.control(
            bounds,
            Control::Custom(CustomControl::Momentary),
            true,
            "MOMENTARY",
            Look::Switch {
                on: Some(momentary),
            },
        );
    } else if slot.action == Some(SlotAction::SendMidi) {
        l.control(
            row,
            Control::Custom(CustomControl::Messages),
            true,
            "EDIT MESSAGES",
            Look::Area,
        );
        title(l, "Edit messages");
        chevron(l, row);
    } else {
        let playing = match (custom.preset_slots.is_some(), edit.scope) {
            (true, LayoutScope::Global) => "This preset plays its own layout. ",
            _ => "",
        };
        let hint = match (slot.action.is_some(), custom.active) {
            (true, _) => format!("{playing}Runs on each press of the switch."),
            (false, true) => format!("{playing}Tap a switch above to edit it."),
            (false, false) => format!("{playing}Off: the mode button or Down + Tap turns it on."),
        };
        let hint = fit(&hint, CAPTION, (row.width - 32) as f32);
        l.text(row.inset(16, 0), hint, CAPTION, p.faint, Align::Left);
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
            scope, page, slot, ..
        } = edit;
        let send = |edit| {
            vec![UiCommand::Custom(CustomCommand::Slot {
                scope,
                page,
                slot,
                edit,
            })]
        };
        let selected = custom
            .slot(scope, page, usize::from(slot))
            .cloned()
            .unwrap_or_default();
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
            C::Select(to) => {
                self.custom_edit = clamped(
                    CustomEdit {
                        slot: to,
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
            C::Actions => {
                self.reset_scroll(ListId::MappingActions);
                self.overlay = Some(Overlay::MappingActions(ActionMenu::Root));
                Vec::new()
            }
            C::Label if selected.action.is_some() => {
                let current = selected.custom_label.unwrap_or_default();
                self.open_keyboard(TextTarget::SlotLabel(scope, page, slot), current);
                Vec::new()
            }
            C::Colour(swatch) if selected.action.is_some() && swatch != selected.swatch => {
                send(SlotEdit::SetColour(swatch))
            }
            C::Value(steps) => match selected.value {
                Some(value) => {
                    let next =
                        (i32::from(value) + i32::from(steps) * VALUE_STEP).clamp(0, 100) as u8;
                    if next == value {
                        return Vec::new();
                    }
                    send(SlotEdit::SetValue(next))
                }
                None => Vec::new(),
            },
            C::Momentary => match selected.momentary {
                Some(momentary) => send(SlotEdit::SetMomentary(!momentary)),
                None => Vec::new(),
            },
            C::Messages if selected.action == Some(SlotAction::SendMidi) => {
                self.midi_edit = Some((MidiOutTarget::Slot { scope, page, slot }, 0));
                self.navigate(Screen::MidiOut)
            }
            C::Clear => send(SlotEdit::Clear),
            C::Label | C::Colour(_) | C::Messages => Vec::new(),
        }
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
            scope, page, slot, ..
        } = clamped(self.custom_edit, custom);
        let current = custom
            .slot(scope, page, usize::from(slot))
            .and_then(|slot| slot.action.clone());
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
                return vec![UiCommand::Custom(CustomCommand::Slot {
                    scope,
                    page,
                    slot,
                    edit: SlotEdit::SetAction(action),
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
mod tests {
    use super::*;
    use crate::screens::mappings::tests::{pick, sample};
    use crate::tests::activate;

    /// Two pages; page 2's A is a bypass of the Green Drive.
    fn view(active: bool) -> CustomModeView {
        let mut page = <[CustomSlotView; CUSTOM_SLOTS]>::default();
        page[0] = CustomSlotView {
            action: Some(SlotAction::Block {
                position: GridPosition::new(0, 1).unwrap(),
                target: BlockTarget::Bypass,
            }),
            label: "Green Drive".into(),
            detail: "Bypass".into(),
            colour: [0x45, 0xf8, 0x62],
            available: true,
            momentary: Some(false),
            ..CustomSlotView::default()
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
            edit,
        })
    }

    /// The editor sets the selected slot of the selected page: an action from the shared menu
    /// (a page change, the loaded preset), then a bypass's Momentary switch.
    #[test]
    fn the_editor_edits_the_selected_slot_through_the_action_menu() {
        let mut ui = open(Screen::CustomMode, false);
        activate(&mut ui, custom(CustomControl::Select(8)));
        activate(&mut ui, custom(CustomControl::Actions));
        pick(&mut ui, "Page");
        assert_eq!(
            pick(&mut ui, "Next page"),
            global(
                0,
                8,
                SlotEdit::SetAction(SlotAction::System(SystemAction::CustomPage(PageStep::Next)))
            )
        );
        activate(&mut ui, custom(CustomControl::Actions));
        pick(&mut ui, "Preset");
        assert_eq!(
            pick(&mut ui, "Load Studio Crunch"),
            global(0, 8, SlotEdit::SetAction(SlotAction::LoadPreset))
        );
        activate(&mut ui, custom(CustomControl::Tab(1)));
        activate(&mut ui, custom(CustomControl::Select(0)));
        assert_eq!(
            activate(&mut ui, custom(CustomControl::Momentary)),
            global(1, 0, SlotEdit::SetMomentary(true))
        );
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
        activate(&mut ui, custom(CustomControl::Actions));
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
            global(0, 0, SlotEdit::SetAction(SlotAction::PresetSlot(9)))
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

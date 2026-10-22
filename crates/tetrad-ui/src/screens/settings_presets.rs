//! Global EQ and I/O Virtual Device Presets (manual M553, M593-594): one stock
//! `GlobalEQPresetPopup` (`0x227a84`, base `OptionsPopup` `0x688f3c`) for model 4004 and
//! 31000. 31000.
//!
//! - Button (`0x688f3c`, `0x227790`): 44 × 44 at (8, 8), a 16 px bundle mark at (14, 14):
//!   grey without a match, tinted when the page values match a preset (Global EQ `#0a74e0`,
//!   I/O `#45f862`). The mark is an owned drawing.
//! - Popup: a `#5F5F5F` veil at 0.7 (`Look::Veil`), the button copy in white with a dark mark,
//!   and the table (`0x22a1d8`): 300 wide, 60 px rows, `#121212`, radius 10, IBM Plex Sans
//!   Regular 18 px. Row 0 is "Save Current Parameters as..." with `save.png` (disabled while
//!   the values match a preset), then one row per preset with a ⋮ (`dotsN70.png`, white while
//!   its menu is open, `0x229170`). The matched row is green `#45f862` (`0x229e1c`).
//! - Row ⋮ menu (`0x2289a0`): 240 wide at x 312, 60 px rows, `#121212`, radius 6, text at
//!   x 16. Global EQ: Set as Default, Edit Name, Overwrite, Delete; I/O has no Set as Default.
//!   Overwrite and Delete close the popup and ask (`0x39793c`, `0x397cb4`).
//!
//! Inferred: the table position under the button, the menu row aligned with its preset row,
//! factory rows before user rows, the "Default" caption, and a load closing the popup.

use tetrad_assets::{Icon, IconSize, Palette, Rgb, block};

use crate::layout::{CAPTION, GREY_VEIL, Layout, ListId, Look, MEDIUM};
use crate::render::{Align, HEIGHT};
use crate::state::{Confirm, Overlay, TextTarget};
use crate::*;

/// The header button of both pages.
pub(super) const BUTTON: Rect = Rect::new(8, 8, 44, 44);
const TABLE_X: i32 = 8;
const TABLE_Y: i32 = 60;
const TABLE_WIDTH: u32 = 300;
const ROW: u32 = 60;
const MENU_X: i32 = 312;
const MENU_WIDTH: u32 = 240;

/// The tint of a matched page (`0x227790`).
fn tint(p: &Palette, page: SettingsPresetPage) -> Rgb {
    match page {
        SettingsPresetPage::GlobalEq => block::EQ,
        SettingsPresetPage::Io => p.accent,
    }
}

/// Owned 16 px bundle mark: two stacked cards.
fn bundle(l: &mut Layout, at: (i32, i32), color: Rgb, fill: Rgb) {
    let (x, y) = at;
    l.panel(Rect::new(x + 5, y + 1, 10, 10), color, 2.0, None);
    l.panel(Rect::new(x + 2, y + 4, 11, 11), fill, 2.0, None);
    l.panel(Rect::new(x + 3, y + 5, 9, 9), color, 2.0, None);
}

/// The 44 × 44 preset button at (8, 8) of the Global EQ and I/O pages.
pub(super) fn button(ui: &UiState, l: &mut Layout, page: SettingsPresetPage) {
    let p = l.palette;
    l.panel(BUTTON, p.scrollbar, 8.0, None);
    l.control(
        BUTTON,
        Control::SettingsPreset(SettingsPresetControl::Open(page)),
        true,
        "PRESETS",
        Look::Area,
    );
    let matched = ui.snapshot.settings_preset_match.get(page).is_some();
    let color = if matched { tint(&p, page) } else { p.muted };
    bundle(l, (BUTTON.x + 14, BUTTON.y + 14), color, p.scrollbar);
}

/// Presets of the page in list order: factory rows, then user rows (I).
fn presets(ui: &UiState, page: SettingsPresetPage) -> Vec<&DevicePresetItem> {
    let of = |factory| {
        let model = page.model_id();
        let list = ui.snapshot.device_presets.iter();
        list.filter(move |p| p.model_id == model && p.factory == factory)
    };
    of(true).chain(of(false)).collect()
}

pub(super) fn layout(ui: &UiState, l: &mut Layout, page: SettingsPresetPage, menu: Option<u64>) {
    let p = l.palette;
    l.veil((p.tone(GREY_VEIL.0), GREY_VEIL.1));
    let presets = presets(ui, page);
    let matched = ui.snapshot.settings_preset_match.get(page);
    let height = ((presets.len() as u32 + 1) * ROW).min(HEIGHT - TABLE_Y as u32 - 8);
    let table = Rect::new(TABLE_X, TABLE_Y, TABLE_WIDTH, height);
    // A press outside the table hides the popup (`onMousePressOutside` → `hideDialog`).
    l.popover(table);
    l.panel(BUTTON, p.text, 8.0, None);
    bundle(l, (BUTTON.x + 14, BUTTON.y + 14), p.background, p.text);
    l.panel(table, p.background, 10.0, None);
    let control = Control::SettingsPreset;
    l.scroll(
        ListId::SettingsPresets,
        table,
        (presets.len() as u32 + 1) * ROW,
        ui.offset(ListId::SettingsPresets),
        |l, top| {
            let save = Rect::new(table.x, top, table.width, ROW);
            let can_save = matched.is_none();
            l.control(
                save,
                control(SettingsPresetControl::SaveAs),
                can_save,
                "Save Current Parameters as...",
                Look::Area,
            );
            let color = if can_save { p.text } else { p.faint };
            l.push(
                Rect::new(save.x + 16, save.y + 18, 24, 24),
                "",
                Look::Glyph {
                    icon: Icon::Save,
                    size: IconSize::Medium,
                    color,
                },
            );
            let text = Rect::new(save.x + 52, save.y, save.width - 60, ROW);
            l.text(
                text,
                "Save Current Parameters as...",
                MEDIUM,
                color,
                Align::Left,
            );
            for (index, preset) in presets.iter().enumerate() {
                let y = top + (index as u32 + 1) as i32 * ROW as i32;
                let row = Rect::new(table.x, y, table.width, ROW);
                preset_row(l, row, preset, matched == Some(preset.id), menu);
            }
        },
    );
    if let Some(id) = menu
        && let Some(preset) = presets.iter().find(|preset| preset.id == id)
        && let Some(trigger) = l.anchor(control(SettingsPresetControl::Menu(id)))
    {
        context_menu(l, page, preset, trigger.y, matched.is_some());
    }
}

fn preset_row(
    l: &mut Layout,
    row: Rect,
    preset: &DevicePresetItem,
    matched: bool,
    menu: Option<u64>,
) {
    let p = l.palette;
    let (body, dots) = row.take_right(ROW, 0);
    let control = Control::SettingsPreset;
    l.control(
        body,
        control(SettingsPresetControl::Load(preset.id)),
        true,
        preset.name.clone(),
        Look::Area,
    );
    l.control(
        dots,
        control(SettingsPresetControl::Menu(preset.id)),
        true,
        "MORE",
        Look::Area,
    );
    let color = if matched { p.accent } else { p.text };
    let text = Rect::new(row.x + 16, row.y, body.width - 16, ROW);
    if preset.default {
        l.text(
            Rect::new(text.x, row.y + 8, text.width, 24),
            preset.name.clone(),
            MEDIUM,
            color,
            Align::Left,
        );
        l.text(
            Rect::new(text.x, row.y + 33, text.width, 18),
            "Default",
            CAPTION,
            p.muted,
            Align::Left,
        );
    } else {
        l.text(text, preset.name.clone(), MEDIUM, color, Align::Left);
    }
    let dot = if menu == Some(preset.id) {
        p.text
    } else {
        p.faint
    };
    let (x, y) = dots.center();
    for dy in [-7, 0, 7] {
        l.push(
            Rect::new(x - 2, y + dy - 2, 4, 4),
            "",
            Look::Dot { color: dot },
        );
    }
}

fn context_menu(
    l: &mut Layout,
    page: SettingsPresetPage,
    preset: &DevicePresetItem,
    row_y: i32,
    matched: bool,
) {
    let p = l.palette;
    let user = !preset.factory;
    let mut items = Vec::new();
    if page == SettingsPresetPage::GlobalEq {
        items.push((
            DevicePresetMenu::SetDefault,
            "Set as Default",
            !preset.default,
        ));
    }
    items.extend([
        (DevicePresetMenu::EditName, "Edit Name", user),
        // Manual: disabled while the current values match an existing preset.
        (DevicePresetMenu::Overwrite, "Overwrite", user && !matched),
        (DevicePresetMenu::Delete, "Delete", user),
    ]);
    let height = items.len() as u32 * ROW;
    let y = row_y.min(HEIGHT as i32 - 8 - height as i32).max(8);
    let panel = Rect::new(MENU_X, y, MENU_WIDTH, height);
    l.popover(panel);
    l.panel(panel, p.background, 6.0, None);
    for (index, (item, label, enabled)) in items.into_iter().enumerate() {
        let row = Rect::new(panel.x, y + (index as u32 * ROW) as i32, MENU_WIDTH, ROW);
        l.control(
            row,
            Control::SettingsPreset(SettingsPresetControl::MenuItem(item)),
            enabled,
            label,
            Look::Area,
        );
        let color = if enabled { p.text } else { p.faint };
        l.text(row.inset(16, 0), label, MEDIUM, color, Align::Left);
    }
}

impl UiState {
    pub(crate) fn settings_preset_control(
        &mut self,
        control: SettingsPresetControl,
    ) -> Vec<UiCommand> {
        if let SettingsPresetControl::Open(page) = control {
            self.overlay = Some(Overlay::SettingsPresets { page, menu: None });
            self.reset_scroll(ListId::SettingsPresets);
            return Vec::new();
        }
        let Some(Overlay::SettingsPresets { page, menu }) = self.overlay else {
            return Vec::new();
        };
        let command = |request| vec![UiCommand::SettingsPreset { page, request }];
        match control {
            SettingsPresetControl::Open(_) => Vec::new(),
            SettingsPresetControl::SaveAs => {
                self.overlay = None;
                let target = TextTarget::SettingsPresetName { page, preset: None };
                self.open_keyboard(target, String::new());
                Vec::new()
            }
            SettingsPresetControl::Load(preset) => {
                self.overlay = None;
                command(SettingsPresetRequest::Load(preset))
            }
            SettingsPresetControl::Menu(preset) => {
                let menu = (menu != Some(preset)).then_some(preset);
                self.overlay = Some(Overlay::SettingsPresets { page, menu });
                Vec::new()
            }
            SettingsPresetControl::MenuItem(item) => {
                let Some(preset) = menu else {
                    return Vec::new();
                };
                self.overlay = None;
                match item {
                    DevicePresetMenu::SetDefault => {
                        command(SettingsPresetRequest::SetDefault(preset))
                    }
                    DevicePresetMenu::EditName => {
                        let name = (self.snapshot.device_presets.iter())
                            .find(|item| item.id == preset)
                            .map(|item| item.name.clone())
                            .unwrap_or_default();
                        let target = TextTarget::SettingsPresetName {
                            page,
                            preset: Some(preset),
                        };
                        self.open_keyboard(target, name);
                        Vec::new()
                    }
                    DevicePresetMenu::Overwrite | DevicePresetMenu::Delete => {
                        self.overlay = Some(Overlay::Confirm(Confirm::SettingsPreset {
                            page,
                            preset,
                            delete: item == DevicePresetMenu::Delete,
                        }));
                        Vec::new()
                    }
                }
            }
        }
    }
}

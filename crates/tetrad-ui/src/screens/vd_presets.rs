//! Virtual Device Presets pane (spec `w4-ui-spec-grid` §5, shots 093, 040):
//! FACTORY / USER tabs right of the device list, 60 px rows with a ⋮ menu
//! (Set as Default, Edit Name, Overwrite, Delete). A row tap auditions the
//! preset; ✓ keeps it and ✕ (or Back) reverts. Overwrite and Delete ask first
//! (`0x39793c`, `0x397cb4`; drawn in `overlay.rs`).

use tetrad_assets::{ACTION, BACKGROUND, FAINT, Icon, MUTED, PANEL, SURFACE, TEXT};

use super::picker::DRAWER;
use crate::layout::{BODY, CAPTION, Layout, ListId, Look};
use crate::render::{Align, HEIGHT, WIDTH};
use crate::state::{Confirm, Overlay, PresetPane, TextTarget};
use crate::widgets::{CLOSE_BUTTON, popover_menu};
use crate::*;

const PANE_X: i32 = DRAWER as i32 + 2;
const ROW: u32 = 60;
const MENU: u32 = 48;

pub(super) fn layout(ui: &UiState, l: &mut Layout, pane: PresetPane) {
    let s = &ui.snapshot;
    l.push(
        Rect::new(DRAWER as i32, 0, WIDTH - DRAWER, HEIGHT),
        "",
        Look::Fill(BACKGROUND),
    );
    for (index, (user, label)) in [(false, "FACTORY"), (true, "USER")].into_iter().enumerate() {
        let tab = Rect::new(PANE_X + index as i32 * 104, 8, 100, 60);
        let selected = pane.user == user;
        // The tab panel runs 8 px into the list; its touch area stops at y 60.
        l.control(
            Rect::new(tab.x, tab.y, tab.width, 52),
            Control::Picker(PickerControl::PresetTab { user }),
            true,
            label,
            Look::Area,
        );
        l.panel(tab, if selected { PANEL } else { SURFACE }, 8.0, None);
        l.text(
            Rect::new(tab.x, tab.y, tab.width, 52),
            label,
            BODY,
            if selected { TEXT } else { MUTED },
            Align::Center,
        );
    }
    // ✓ appears once a preset is auditioned; ✕ then shrinks beside it (093 vs 040).
    let close = if pane.auditioned {
        l.control(
            Rect::new(696, 8, 98, 44),
            Control::Picker(PickerControl::KeepPreset),
            true,
            "KEEP",
            Look::IconButton {
                icon: Icon::Check,
                fill: ACTION,
            },
        );
        Rect::new(622, 8, 66, 44)
    } else {
        CLOSE_BUTTON
    };
    l.control(
        close,
        Control::Picker(PickerControl::RevertPreset),
        true,
        "CLOSE",
        Look::IconButton {
            icon: Icon::Close,
            fill: SURFACE,
        },
    );

    let list = Rect::new(PANE_X, 60, WIDTH - PANE_X as u32, HEIGHT - 60);
    l.push(list, "", Look::Fill(PANEL));
    let presets = pane_presets(ui, pane);
    l.scroll(
        ListId::PickerPresets,
        list,
        presets.len() as u32 * ROW,
        ui.offset(ListId::PickerPresets),
        |l, top| {
            for (index, preset) in presets.iter().enumerate() {
                let row = Rect::new(list.x, top + index as i32 * ROW as i32, list.width, ROW);
                preset_row(ui, l, row, preset);
            }
        },
    );
    if presets.is_empty() {
        l.text(
            Rect::new(list.x, list.y + 40, list.width, 30),
            if pane.user {
                "No user presets for this device"
            } else {
                "No factory presets for this device"
            },
            BODY,
            FAINT,
            Align::Center,
        );
    }
    if let Some(id) = pane.menu
        && let Some(preset) = presets.iter().find(|preset| preset.id == id)
        && let Some(trigger) = l.anchor(Control::Picker(PickerControl::PresetMenu(id)))
    {
        let anchor = Rect::new(trigger.x, trigger.y - 26, trigger.width, trigger.height);
        let item = |menu| Control::Picker(PickerControl::PresetMenuItem(menu));
        let user = !preset.factory;
        // Overwrite is pointless while the block already matches the preset.
        let matches = s.device_preset == Some(id);
        popover_menu(
            l,
            anchor,
            &[
                (
                    item(DevicePresetMenu::SetDefault),
                    "Set as Default",
                    !preset.default,
                ),
                (item(DevicePresetMenu::EditName), "Edit Name", user),
                (
                    item(DevicePresetMenu::Overwrite),
                    "Overwrite",
                    user && !matches,
                ),
                (item(DevicePresetMenu::Delete), "Delete", user),
            ],
            None,
        );
    }
}

fn preset_row(ui: &UiState, l: &mut Layout, row: Rect, preset: &DevicePresetItem) {
    let (body, menu) = row.take_right(MENU, 0);
    if ui.snapshot.device_preset == Some(preset.id) {
        l.push(row, "", Look::Fill(SURFACE));
    }
    l.control(
        body,
        Control::Picker(PickerControl::Preset(preset.id)),
        true,
        preset.name.clone(),
        Look::Area,
    );
    l.control(
        menu,
        Control::Picker(PickerControl::PresetMenu(preset.id)),
        true,
        "MORE",
        Look::Area,
    );
    let text = Rect::new(row.x + 16, row.y, body.width - 24, row.height);
    // The edited current preset shows its name in italic (R11 rule, as in grid.rs).
    let name_font = BODY.italic(ui.snapshot.dirty && ui.snapshot.device_preset == Some(preset.id));
    if preset.default {
        l.text(
            Rect::new(text.x, row.y + 8, text.width, 24),
            preset.name.clone(),
            name_font,
            TEXT,
            Align::Left,
        );
        l.text(
            Rect::new(text.x, row.y + 33, text.width, 18),
            "Default",
            CAPTION,
            MUTED,
            Align::Left,
        );
    } else {
        l.text(text, preset.name.clone(), name_font, TEXT, Align::Left);
    }
    let (x, y) = menu.center();
    for dy in [-7, 0, 7] {
        l.push(
            Rect::new(x - 2, y + dy - 2, 4, 4),
            "",
            Look::Dot { color: FAINT },
        );
    }
}

/// Rows of the open tab, in list order.
fn pane_presets(ui: &UiState, pane: PresetPane) -> Vec<&DevicePresetItem> {
    ui.snapshot
        .device_presets
        .iter()
        .filter(|preset| preset.model_id == pane.model_id && preset.factory != pane.user)
        .collect()
}

impl UiState {
    /// BANK UP encoder (manual §4): in the Virtual Device Preset list it auditions the preset
    /// `delta` rows away (wrapping, I); in the block editor it cycles the device one model per
    /// event, by the sign only (`0x4f5f38`, review r15 R7; `ModelEditor` / picker
    /// `onEncoderUpdate`, connected at `0x391fb4`).
    pub(crate) fn bank_encoder(&mut self, delta: i32) -> Vec<UiCommand> {
        if delta == 0 || self.keyboard.is_some() || self.overlay.is_some() {
            return Vec::new();
        }
        if let Some(pane) = self.picker.presets {
            let ids: Vec<u64> = pane_presets(self, pane).iter().map(|p| p.id).collect();
            let Some(len) = i32::try_from(ids.len()).ok().filter(|len| *len > 0) else {
                return Vec::new();
            };
            // From no audition, the first turn lands on the first (or last) row.
            let current = ids
                .iter()
                .position(|id| self.snapshot.device_preset == Some(*id))
                .map_or(if delta > 0 { -1 } else { 0 }, |row| row as i32);
            let next = ids[(current + delta).rem_euclid(len) as usize];
            return self.preset_control(PickerControl::Preset(next));
        }
        match self.editing_block() {
            Some(position) if self.draft.is_none() => {
                vec![UiCommand::CycleModel {
                    position,
                    step: delta.signum(),
                }]
            }
            _ => Vec::new(),
        }
    }

    /// Presets pane controls (`PickerControl` variants from `PresetTab` on).
    pub(crate) fn preset_control(&mut self, control: PickerControl) -> Vec<UiCommand> {
        let Some(pane) = self.picker.presets.as_mut() else {
            return Vec::new();
        };
        let request = match control {
            PickerControl::PresetTab { user } => {
                pane.user = user;
                pane.menu = None;
                self.reset_scroll(ListId::PickerPresets);
                return Vec::new();
            }
            PickerControl::Preset(preset) => {
                pane.menu = None;
                pane.auditioned = true;
                DevicePresetRequest::Audition {
                    model_id: pane.model_id,
                    preset,
                }
            }
            PickerControl::PresetMenu(preset) => {
                pane.menu = Some(preset);
                return Vec::new();
            }
            PickerControl::PresetMenuItem(item) => {
                let Some(preset) = pane.menu.take() else {
                    return Vec::new();
                };
                match item {
                    DevicePresetMenu::SetDefault => DevicePresetRequest::SetDefault(preset),
                    DevicePresetMenu::Overwrite | DevicePresetMenu::Delete => {
                        self.overlay = Some(Overlay::Confirm(Confirm::DevicePreset {
                            preset,
                            delete: item == DevicePresetMenu::Delete,
                        }));
                        return Vec::new();
                    }
                    DevicePresetMenu::EditName => {
                        let name = self
                            .snapshot
                            .device_presets
                            .iter()
                            .find(|item| item.id == preset)
                            .map(|item| item.name.clone())
                            .unwrap_or_default();
                        self.open_keyboard(TextTarget::DevicePresetName(preset), name);
                        return Vec::new();
                    }
                }
            }
            PickerControl::KeepPreset => {
                self.picker.presets = None;
                let mut commands = self.device_preset_request(DevicePresetRequest::Keep);
                commands.extend(self.pop());
                return commands;
            }
            PickerControl::RevertPreset => return self.close_presets(),
            // Category, model, search and paste controls stay in `actions.rs`.
            _ => return Vec::new(),
        };
        self.device_preset_request(request)
    }

    /// Close the presets pane and revert an audition.
    pub(crate) fn close_presets(&mut self) -> Vec<UiCommand> {
        match self.picker.presets.take() {
            Some(pane) if pane.auditioned => {
                self.device_preset_request(DevicePresetRequest::Revert)
            }
            _ => Vec::new(),
        }
    }

    /// The request for the picker's target slot (none without a target).
    pub(crate) fn device_preset_request(&self, request: DevicePresetRequest) -> Vec<UiCommand> {
        self.picker
            .target
            .map(|position| UiCommand::DevicePreset { position, request })
            .into_iter()
            .collect()
    }
}

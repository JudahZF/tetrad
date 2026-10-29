//! Virtual Device List (spec `w4-ui-spec-grid` §3, shots 076, 090, 093): a
//! 400 px drawer over the dimmed Grid. Step 1 lists the 19 stock categories as
//! 78 px icon rows. A category tap collapses them to a 100 px icon rail beside
//! the GUITAR / BASS tabs and a 60 px device list; the stack button of a row
//! opens its Virtual Device Presets (`vd_presets.rs`). The Extensions row, name
//! search and clipboard paste rows are owned additions (I). Glyphs are owned vectors.

use tetrad_assets::{Icon, IconSize, Rgb};

use super::directory::ListQuery;
use crate::layout::{BODY, CAPTION, GREY_VEIL, Layout, ListId, Look};
use crate::render::{Align, HEIGHT, WIDTH};
use crate::state::PresetPane;
use crate::*;

/// Drawer width; the Grid stays visible right of it.
pub(super) const DRAWER: u32 = 400;
const CATEGORY_ROW: u32 = 78;
const TILE: u32 = 64;
const TILE_X: i32 = 18;
/// Top padding of the category column (first tile at y 13, shot 076).
const LIST_PAD: i32 = 6;
const RAIL: u32 = 100;
pub(super) const LIST_X: i32 = 104;
const TABS: i32 = 60;
const MODEL_ROW: u32 = 60;
const TAB: (u32, u32) = (100, 52);
/// Preset-stack button fill (`#373737`, shot 093 samples `#303430` at 16 bpp).
const STACK: Rgb = Rgb(0x37, 0x37, 0x37);

pub(super) fn layout(ui: &UiState, l: &mut Layout) {
    backdrop(ui, l);
    let p = l.palette;
    l.push(drawer(ui), "", Look::Fill(p.surface));
    let picker = &ui.picker;
    if picker.group.is_none() && picker.query.is_empty() {
        categories(ui, l);
    } else if ui.capture_browser() {
        rail(ui, l);
        super::capture_browser::layout(ui, l);
    } else {
        rail(ui, l);
        devices(ui, l);
    }
    if let Some(pane) = picker.presets {
        super::vd_presets::layout(ui, l, pane);
    }
}

/// The drawer: step 1 (the categories) opens on the right with the Grid left of it (shot
/// sn-2, 4.1.0); step 2 keeps the rail on the left (shots 090, sn-4). I: stock may pick the
/// side by the target slot; sn-2 shows only a column 4 slot.
fn drawer(ui: &UiState) -> Rect {
    let step1 = ui.picker.group.is_none() && ui.picker.query.is_empty();
    let x = if step1 { (WIDTH - DRAWER) as i32 } else { 0 };
    Rect::new(x, 0, DRAWER, HEIGHT)
}

/// The Grid under a scrim. A tap beside the drawer closes the picker; in add mode the
/// target slot shows its black "+" tile (shots 076, sn-2).
fn backdrop(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(p.canvas));
    super::main_bar(ui, l);
    super::grid::layout(ui, l);
    let target = ui
        .picker
        .target
        .filter(|_| !ui.picker.replace)
        .and_then(|target| l.anchor(Control::SelectCell(target)));
    l.veil((p.tone(GREY_VEIL.0), GREY_VEIL.1));
    // The capture browser covers the Grid (shot sn-4).
    if ui.picker.presets.is_some() || ui.capture_browser() {
        return;
    }
    let drawer = drawer(ui);
    let beside = match drawer.x {
        0 => Rect::new(drawer.right(), 0, WIDTH - DRAWER, HEIGHT),
        _ => Rect::new(0, 0, drawer.x as u32, HEIGHT),
    };
    l.control(beside, Control::Back, true, "CLOSE", Look::Dismiss);
    if let Some(slot) = target.filter(|slot| slot.right() <= drawer.x || slot.x >= drawer.right()) {
        l.push(
            slot.inset(3, 3),
            "+",
            Look::IconButton {
                icon: Icon::Plus,
                fill: p.canvas,
            },
        );
    }
}

/// Category rows: the stock groups, then Extensions when an extension block is installed.
fn groups(ui: &UiState) -> Vec<DeviceGroup> {
    let mut groups = DeviceGroup::ALL.to_vec();
    if ui.snapshot.models.iter().any(|model| model.extension) {
        groups.push(DeviceGroup::Extensions);
    }
    groups
}

/// Extra rows after the categories: paste (with a clipboard, add mode) and search.
fn extras(ui: &UiState) -> Vec<(Control, Icon, &'static str)> {
    let mut rows = Vec::new();
    if ui.snapshot.clipboard && !ui.picker.replace {
        rows.push((
            Control::Picker(PickerControl::Paste),
            Icon::Paste,
            "Paste device",
        ));
    }
    rows.push((
        Control::Picker(PickerControl::Search),
        Icon::Search,
        "Search",
    ));
    rows
}

fn row_y(top: i32, index: usize) -> i32 {
    top + LIST_PAD + index as i32 * CATEGORY_ROW as i32
}

/// Step 1: icon tile and name per category (no "New" pill on Plugins in 4.1.0, shot sn-2).
fn categories(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let drawer = drawer(ui);
    let groups = groups(ui);
    let extras = extras(ui);
    let rows = groups.len() + extras.len();
    let content = rows as u32 * CATEGORY_ROW + 2 * LIST_PAD as u32;
    let offset = ui.offset(ListId::PickerCategories);
    l.scroll(
        ListId::PickerCategories,
        drawer,
        content,
        offset,
        |l, top| {
            for (index, &group) in groups.iter().enumerate() {
                let row = Rect::new(drawer.x, row_y(top, index), DRAWER, CATEGORY_ROW);
                l.control(
                    row,
                    Control::Picker(PickerControl::Group(group)),
                    true,
                    group.label(),
                    Look::Area,
                );
                let plugins = matches!(group, DeviceGroup::Plugins);
                let fill = if plugins { p.accent_fill } else { p.canvas };
                tile(
                    l,
                    tile_rect(row),
                    Some(group.hue(&p)),
                    fill,
                    Mark::Group(group),
                );
                l.text(
                    Rect::new(row.x + 102, row.y, 280, CATEGORY_ROW),
                    group.label(),
                    BODY,
                    p.text,
                    Align::Left,
                );
            }
            for (index, (control, icon, label)) in extras.into_iter().enumerate() {
                let row = Rect::new(
                    drawer.x,
                    row_y(top, groups.len() + index),
                    DRAWER,
                    CATEGORY_ROW,
                );
                l.control(row, control, true, label, Look::Area);
                l.push(
                    tile_rect(row),
                    "",
                    Look::IconButton {
                        icon,
                        fill: p.surface,
                    },
                );
                l.text(
                    Rect::new(row.x + 102, row.y, 280, CATEGORY_ROW),
                    label,
                    BODY,
                    p.muted,
                    Align::Left,
                );
            }
        },
    );
    super::scrollbar(l, drawer, content, offset, 6);
}

fn tile_rect(row: Rect) -> Rect {
    Rect::new(row.x + TILE_X, row.y + 7, TILE, TILE)
}

/// Category tile with a 2 px border in `hue` over `fill`; without a hue only the
/// glyph shows (unselected rail icons, shot 090).
fn tile(l: &mut Layout, bounds: Rect, hue: Option<Rgb>, fill: Rgb, mark: Mark) {
    let text = l.palette.text;
    if let Some(hue) = hue {
        l.panel(bounds, hue, 10.0, None);
        l.panel(bounds.inset(2, 2), fill, 8.0, None);
    }
    glyph(l, mark, bounds.inset(12, 12), text);
}

/// Step 2 rail: one icon per category; the open one keeps its tile.
fn rail(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let groups = groups(ui);
    let extras = extras(ui);
    let open = ui.picker.group.filter(|_| ui.picker.query.is_empty());
    let rows = groups.len() + extras.len();
    l.scroll(
        ListId::PickerCategories,
        Rect::new(0, 0, RAIL, HEIGHT),
        rows as u32 * CATEGORY_ROW + 2 * LIST_PAD as u32,
        ui.offset(ListId::PickerCategories),
        |l, top| {
            for (index, &group) in groups.iter().enumerate() {
                let row = Rect::new(0, row_y(top, index), RAIL, CATEGORY_ROW);
                l.control(
                    row,
                    Control::Picker(PickerControl::Group(group)),
                    true,
                    group.label(),
                    Look::Area,
                );
                let selected = open == Some(group);
                tile(
                    l,
                    tile_rect(row),
                    selected.then(|| group.hue(&p)),
                    p.canvas,
                    Mark::Group(group),
                );
            }
            for (index, (control, icon, label)) in extras.into_iter().enumerate() {
                let row = Rect::new(0, row_y(top, groups.len() + index), RAIL, CATEGORY_ROW);
                l.control(
                    row,
                    control,
                    true,
                    label,
                    Look::IconButton {
                        icon,
                        fill: p.surface,
                    },
                );
            }
        },
    );
}

/// Models listed for the open group and tab, or the search hits, by name. Extensions
/// lists the extension blocks of every category. Unlicensed plugin models are left out.
fn shown_models(ui: &UiState) -> Vec<&ModelEntry> {
    let picker = &ui.picker;
    let all = &ui.snapshot.models;
    let query = picker.query.to_lowercase();
    let unlicensed = &ui.snapshot.unlicensed_plugins;
    let pinned = &ui.snapshot.pinned_models;
    let sort = || {
        let mut models: Vec<(u16, &ModelEntry)> = (0u16..)
            .zip(all.iter())
            .filter(|(_, model)| {
                !model
                    .plugin
                    .is_some_and(|plugin| unlicensed.contains(&plugin))
            })
            .filter(|(_, model)| {
                let place = DeviceGroup::of(model.category);
                if query.is_empty() && picker.group == Some(DeviceGroup::Extensions) {
                    model.extension && place.is_some()
                } else if query.is_empty() {
                    place.is_some_and(|(group, instrument)| {
                        Some(group) == picker.group
                            && (!group.has_bass() || instrument == picker.instrument)
                    })
                } else {
                    place.is_some() && model.name.to_lowercase().contains(&query)
                }
            })
            .collect();
        // Pinned models first, in pin order (manual "BROWSING & LOADING VIRTUAL DEVICES").
        models.sort_by_cached_key(|(_, model)| {
            let pin = pinned.iter().position(|id| *id == model.id);
            (pin.is_none(), pin, model.name.to_lowercase())
        });
        models.into_iter().map(|(index, _)| index).collect()
    };
    let order = ListQuery::Models {
        group: picker.group,
        instrument: picker.instrument,
        query: query.clone(),
        pinned: pinned.clone(),
        unlicensed: unlicensed.clone(),
    };
    (ui.list_cache.rows(&ui.snapshot, order, sort).iter())
        .map(|&index| &all[usize::from(index)])
        .collect()
}

/// Model in the block being replaced.
fn current_model(ui: &UiState) -> Option<&ModelEntry> {
    let picker = &ui.picker;
    let id = picker
        .target
        .filter(|_| picker.replace)
        .and_then(|position| ui.snapshot.blocks[position.index()].model_id)?;
    ui.snapshot.models.iter().find(|model| model.id == id)
}

/// Tab bar and device list (shots 090, 093).
fn devices(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let picker = &ui.picker;
    let current = current_model(ui);
    l.push(
        Rect::new(RAIL as i32, 0, DRAWER - RAIL, HEIGHT),
        "",
        Look::Fill(p.background),
    );
    let tab = |index: i32| Rect::new(LIST_X + index * (TAB.0 as i32 + 4), 8, TAB.0, TAB.1 + 8);
    // Tabs are drawn 8 px deep into the list so the list fill squares off
    // their lower corners.
    let mut labels: Vec<(Rect, String, Rgb, bool)> = Vec::new();
    match picker.group {
        _ if !picker.query.is_empty() => {
            let bounds = Rect::new(LIST_X, 8, 236, TAB.1 + 8);
            l.panel(bounds, p.panel, 8.0, None);
            labels.push((bounds, format!("\"{}\"", picker.query), p.text, false));
            l.icon_button(
                Rect::new(348, 8, 44, 44),
                Control::Picker(PickerControl::ClearSearch),
                true,
                "CLEAR",
                Icon::Close,
            );
        }
        Some(group) if group.has_bass() => {
            let current_tab = current
                .and_then(|model| DeviceGroup::of(model.category))
                .filter(|(current, _)| *current == group)
                .map(|(_, instrument)| instrument);
            for (index, (instrument, label)) in
                [(Instrument::Guitar, "GUITAR"), (Instrument::Bass, "BASS")]
                    .into_iter()
                    .enumerate()
            {
                let bounds = tab(index as i32);
                let selected = picker.instrument == instrument;
                l.control(
                    Rect::new(bounds.x, bounds.y, bounds.width, TAB.1),
                    Control::Picker(PickerControl::Instrument(instrument)),
                    true,
                    label,
                    Look::Area,
                );
                l.panel(
                    bounds,
                    if selected { p.panel } else { p.surface },
                    8.0,
                    None,
                );
                labels.push((
                    bounds,
                    label.into(),
                    if selected { p.text } else { p.muted },
                    current_tab == Some(instrument),
                ));
            }
        }
        Some(group) => {
            let bounds = Rect::new(LIST_X, 8, 236, TAB.1 + 8);
            l.panel(bounds, p.panel, 8.0, None);
            labels.push((bounds, group.label().to_uppercase(), p.text, false));
        }
        None => {}
    }
    let list = Rect::new(LIST_X, TABS, DRAWER - LIST_X as u32, HEIGHT - TABS as u32);
    l.push(list, "", Look::Fill(p.panel));
    for (bounds, text, color, dot) in labels {
        let text_box = Rect::new(bounds.x, bounds.y, bounds.width, TAB.1);
        if dot {
            // A yellow dot marks the tab of the current device (shot 093).
            l.push(
                Rect::new(bounds.x + 8, bounds.y + 23, 6, 6),
                "",
                Look::Dot { color: p.warning },
            );
        }
        l.text(text_box, text, BODY, color, Align::Center);
    }

    let models = shown_models(ui);
    let searching = !picker.query.is_empty();
    // Search hits and extension blocks mix categories, so each row names its category.
    let captions = searching || picker.group == Some(DeviceGroup::Extensions);
    l.scroll(
        ListId::PickerModels,
        list,
        models.len() as u32 * MODEL_ROW,
        ui.offset(ListId::PickerModels),
        |l, top| {
            let shown = l.shown_rows(top, MODEL_ROW, models.len());
            let rows = models.iter().enumerate().take(shown.end).skip(shown.start);
            for (index, model) in rows {
                let row = Rect::new(
                    list.x,
                    top + index as i32 * MODEL_ROW as i32,
                    list.width,
                    MODEL_ROW,
                );
                model_row(
                    ui,
                    l,
                    row,
                    model,
                    current.map(|m| m.id) == Some(model.id),
                    captions,
                );
            }
        },
    );
    if models.is_empty() {
        let text = match picker.group {
            _ if searching => "No results",
            Some(DeviceGroup::Plugins) => "No plugins installed",
            Some(DeviceGroup::Extensions) => "No extension blocks installed",
            _ if ui.snapshot.models.is_empty() => "No models in the application catalog",
            _ => "No devices",
        };
        l.text(
            Rect::new(list.x, list.y + 40, list.width, 30),
            text,
            BODY,
            p.faint,
            Align::Center,
        );
    }
}

fn model_row(
    ui: &UiState,
    l: &mut Layout,
    row: Rect,
    model: &ModelEntry,
    current: bool,
    caption: bool,
) {
    let p = l.palette;
    // Virtual Device Presets do not exist for Neural Capture blocks (manual).
    let presets = DeviceGroup::of(model.category)
        .is_some_and(|(group, _)| group != DeviceGroup::NeuralCapture);
    let open = ui.picker.presets.map(|pane| pane.model_id) == Some(model.id);
    if current || open {
        l.push(row, "", Look::Fill(p.surface));
    }
    let (body, stack) = row.take_right(48, 0);
    l.control(
        if presets { body } else { row },
        Control::Picker(PickerControl::Model(model.id)),
        true,
        model.name.clone(),
        Look::Area,
    );
    let color = if current { p.accent } else { p.text };
    // A pinned row (`setPinned` `0x38bc68`): the 16 px pin at (16, 22) and the name from
    // x 40. Owned pin drawing; the manual draws it green.
    let pinned = ui.snapshot.pinned_models.contains(&model.id);
    if pinned {
        glyph(
            l,
            Mark::Pin,
            Rect::new(row.x + 16, row.y + 22, 16, 16),
            p.accent,
        );
    }
    let text_x = row.x + if pinned { 40 } else { 16 };
    if caption {
        l.text(
            Rect::new(text_x, row.y + 8, (body.right() - text_x - 8) as u32, 24),
            model.name.clone(),
            BODY,
            color,
            Align::Left,
        );
        l.text(
            Rect::new(text_x, row.y + 32, (body.right() - text_x - 8) as u32, 18),
            model.category.label(),
            CAPTION,
            p.muted,
            Align::Left,
        );
    } else {
        l.text(
            Rect::new(
                text_x,
                row.y,
                (body.right() - text_x - 8) as u32,
                row.height,
            ),
            model.name.clone(),
            BODY,
            color,
            Align::Left,
        );
    }
    // A locked plugin model shows the stock lock (`plugin_lock.png`); its place in the row
    // is inferred (no stock shot), right of the name.
    if ui.snapshot.locked_models.contains(&model.id) {
        let size = IconSize::Small.px();
        l.push(
            Rect::new(
                body.right() - 28,
                row.y + (row.height - size) as i32 / 2,
                size,
                size,
            ),
            "",
            Look::Glyph {
                icon: Icon::Lock,
                size: IconSize::Small,
                color: p.muted,
            },
        );
    }
    if presets {
        let button = Rect::new(stack.x, row.y + 10, 40, 40);
        l.control(
            button,
            Control::Picker(PickerControl::Presets(model.id)),
            true,
            "PRESETS",
            Look::Area,
        );
        let white = current || open;
        l.panel(
            button,
            if white { p.text } else { p.tone(STACK) },
            6.0,
            None,
        );
        glyph(
            l,
            Mark::Stack,
            button.inset(11, 11),
            if white { p.canvas } else { p.muted },
        );
    }
}

impl UiState {
    /// New Virtual Device List controls (the original ones stay in `actions.rs`).
    pub(crate) fn device_list(&mut self, control: PickerControl) -> Vec<UiCommand> {
        match control {
            PickerControl::Group(group) => {
                let picker = &mut self.picker;
                let open = picker.group == Some(group) && picker.query.is_empty();
                picker.query.clear();
                picker.presets = None;
                // Tapping the open category collapses the list (manual).
                picker.group = (!open).then_some(group);
                if !open {
                    picker.instrument = current_tab(&self.snapshot, picker, group);
                }
                self.reset_scroll(ListId::PickerModels);
                if self.capture_browser() {
                    self.open_capture_browser();
                }
            }
            PickerControl::CaptureFolder(_)
            | PickerControl::CaptureBack
            | PickerControl::CaptureRefresh
            | PickerControl::Capture(_)
            | PickerControl::CaptureJump(_)
            | PickerControl::Close => return self.capture_browser_control(control),
            PickerControl::Instrument(instrument) => {
                self.picker.instrument = instrument;
                self.reset_scroll(ListId::PickerModels);
            }
            PickerControl::Presets(model_id) => {
                if self.picker.presets.map(|pane| pane.model_id) == Some(model_id) {
                    return Vec::new();
                }
                let commands = self.close_presets();
                self.picker.presets = Some(PresetPane {
                    model_id,
                    user: false,
                    menu: None,
                    auditioned: false,
                });
                self.reset_scroll(ListId::PickerPresets);
                return commands;
            }
            other => return self.preset_control(other),
        }
        Vec::new()
    }

    /// Back on the picker closes a preset menu, then the presets pane
    /// (reverting an audition); on the expression screen it reverts and closes.
    pub(crate) fn picker_back(&mut self) -> Option<Vec<UiCommand>> {
        match self.screen() {
            Screen::ModelPicker => {
                let pane = self.picker.presets.as_mut()?;
                if pane.menu.take().is_some() {
                    return Some(Vec::new());
                }
                Some(self.close_presets())
            }
            Screen::Expression => Some(self.close_expression(false)),
            _ => None,
        }
    }
}

/// Tab of the current device when it is in `group`, else GUITAR.
fn current_tab(
    snapshot: &UiSnapshot,
    picker: &crate::state::Picker,
    group: DeviceGroup,
) -> Instrument {
    picker
        .target
        .filter(|_| picker.replace)
        .and_then(|position| snapshot.blocks[position.index()].category)
        .and_then(DeviceGroup::of)
        .filter(|(current, _)| *current == group)
        .map_or(Instrument::Guitar, |(_, instrument)| instrument)
}

/// Owned vector glyphs in a 10 × 10 unit box (category tiles, buttons).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mark {
    Group(DeviceGroup),
    /// Virtual Device Presets stack.
    Stack,
    /// Library shelf (Directory sidebar).
    Library,
    /// Impulse responses (Directory category).
    Impulse,
    /// A pinned Virtual Device (`icons/16/pin.png`).
    Pin,
}

#[derive(Clone, Copy)]
enum Stroke {
    Line(f32, f32, f32, f32),
    /// Open or closed polyline.
    Poly(&'static [(f32, f32)]),
}

use Stroke::{Line, Poly};

impl Mark {
    fn strokes(self) -> &'static [Stroke] {
        match self {
            // Category marks are the shared `tetrad_assets` icons (see `glyph`).
            Self::Group(_) | Self::Impulse => &[],
            Self::Stack => &[
                Poly(&[(5.0, 0.8), (9.2, 3.0), (5.0, 5.2), (0.8, 3.0), (5.0, 0.8)]),
                Poly(&[(0.8, 5.2), (5.0, 7.4), (9.2, 5.2)]),
                Poly(&[(0.8, 7.2), (5.0, 9.4), (9.2, 7.2)]),
            ],
            Self::Pin => &[
                Poly(&[(3.0, 0.8), (7.0, 0.8)]),
                Poly(&[
                    (3.8, 0.8),
                    (3.8, 4.4),
                    (2.0, 6.2),
                    (8.0, 6.2),
                    (6.2, 4.4),
                    (6.2, 0.8),
                ]),
                Line(5.0, 6.2, 5.0, 9.6),
            ],
            Self::Library => &[
                Line(1.2, 1.0, 1.2, 9.0),
                Line(3.4, 1.0, 3.4, 9.0),
                Line(5.6, 1.0, 5.6, 9.0),
                Line(6.9, 1.4, 9.2, 8.8),
            ],
        }
    }
}

/// Owned category icon (`tetrad_assets`, shared with the Grid block tiles).
fn group_icon(group: DeviceGroup) -> Icon {
    match group {
        DeviceGroup::Plugins => Icon::Plugin,
        DeviceGroup::Amp => Icon::Amp,
        DeviceGroup::NeuralCapture => Icon::NeuralCapture,
        DeviceGroup::Cab => Icon::Cab,
        DeviceGroup::Overdrive => Icon::Overdrive,
        DeviceGroup::Delay => Icon::Delay,
        DeviceGroup::Reverb => Icon::Reverb,
        DeviceGroup::Compressor => Icon::Compressor,
        DeviceGroup::Pitch => Icon::Pitch,
        DeviceGroup::Modulation => Icon::Modulation,
        DeviceGroup::Morph => Icon::Morph,
        DeviceGroup::Synth => Icon::Synth,
        DeviceGroup::Filter => Icon::Filter,
        DeviceGroup::Eq => Icon::Eq,
        DeviceGroup::IrLoader => Icon::IrLoader,
        DeviceGroup::Wah => Icon::Wah,
        DeviceGroup::FxLoop => Icon::FxLoop,
        DeviceGroup::Looper => Icon::Looper,
        DeviceGroup::Utility => Icon::Utility,
        // The Settings → Extensions glyph.
        DeviceGroup::Extensions => Icon::Plugin,
    }
}

/// Draw `mark` scaled into `bounds` with 2 px strokes. Category marks and
/// the IR mark use the shared icon set at the largest size that fits.
pub(super) fn glyph(l: &mut Layout, mark: Mark, bounds: Rect, color: Rgb) {
    const WIDTH: f32 = 2.0;
    let icon = match mark {
        Mark::Group(group) => Some(group_icon(group)),
        Mark::Impulse => Some(Icon::IrLoader),
        _ => None,
    };
    if let Some(icon) = icon {
        let side = bounds.width.min(bounds.height);
        let size = [IconSize::Tile, IconSize::Large, IconSize::Medium]
            .into_iter()
            .find(|size| size.px() <= side)
            .unwrap_or(IconSize::Small);
        l.push(bounds, "", Look::Glyph { icon, size, color });
        return;
    }
    let scale = bounds.width.min(bounds.height) as f32 / 10.0;
    let at = |(x, y): (f32, f32)| (bounds.x as f32 + x * scale, bounds.y as f32 + y * scale);
    let polyline = |l: &mut Layout, points: &[(f32, f32)]| {
        for pair in points.windows(2) {
            l.line(at(pair[0]), at(pair[1]), WIDTH, color);
        }
    };
    for stroke in mark.strokes() {
        match *stroke {
            Line(x0, y0, x1, y1) => polyline(l, &[(x0, y0), (x1, y1)]),
            Poly(points) => polyline(l, points),
        }
    }
}

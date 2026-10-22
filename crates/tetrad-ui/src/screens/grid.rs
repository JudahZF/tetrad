//! The Grid (manual §4, spec `w4-ui-spec-grid` §2, shot 013): the preset
//! title header, the 4 × 8 block grid with row I/O tiles and Splitter/Mixer
//! cables, the scene dropdown (shot 025) and the docked block editor.
//!
//! Grid geometry is stock: rows at y 8/102/196/290 inside a region that
//! starts at y 96, row height 86, 70×70 slots at the x positions below
//! (`_INIT`, rodata `0x11dfde0`, `.data 0x153f728`) holding 64×64 tiles.

use tetrad_assets::{Icon, IconSize, Palette, Rgb, TextSize, Weight};

use crate::draw::{PORT_LINE_PITCH, PORT_LINE_TOP, PORT_RADIUS, TAB_IDLE};
use crate::layout::{BODY, BODY_BOLD, CAPTION, Layout, Look, MEDIUM, MEDIUM_BOLD};
use crate::render::{Align, Font, HEIGHT, WIDTH, fit};
use crate::state::{HEADER, INHIBIT_FLASH_MS};
use crate::widgets::CARD;
use crate::*;

/// Header preset title shared by the Grid and Gig View: slot id upright, then
/// the name, white italic while unsaved (stock `0x00341104`: pen
/// `Qt::white`, `QFont::setStyle(italic)` when modified; manual shot 004).
///
/// `grid` selects the stock Grid navbar rules (`0x00341104`; `0x005ae644` puts the item at
/// (0, 0), 598 × 96):
/// the slot letter in its gig colour (shot 025); the letter and the name start at the
/// `QFontMetrics::width` of the parts before them (whole pixels), the name 15 px after the
/// letter; and the name is never elided: while its ink box + 1 is wider than the item width −
/// x − 10, the pixel size becomes `int(size × 0.9)` ([`TITLE_STEPS`]). The item ends at
/// `text_box.right()`.
pub(super) fn preset_title(l: &mut Layout, s: &UiSnapshot, text_box: Rect, font: Font, grid: bool) {
    let p = l.palette;
    let part_box = |x: i32| {
        Rect::new(
            x,
            text_box.y,
            (text_box.right() - x).max(0) as u32,
            text_box.height,
        )
    };
    let mut x = text_box.x;
    if let Some(slot) = s.preset_slot {
        let label = slot.label();
        let (number, letter) = label.split_at(label.len() - 1);
        let letter_ink = if grid {
            super::gig::SLOT_COLOURS[usize::from(slot.slot % 8)]
        } else {
            p.text
        };
        for (part, color) in [(number, p.text), (letter, letter_ink)] {
            l.text(part_box(x), part, font, color, Align::Left);
            let width = font.width(part);
            x += if grid { width.round() } else { width.ceil() } as i32;
        }
        x += if grid {
            15
        } else {
            font.width(" ").ceil() as i32
        };
    }
    // Stock `0x005abcb0`: an edited preset gets a trailing "*" (removed on save)
    // and its dirty state turns the title italic (`0x00340a84`, manual 004).
    let star = if s.dirty { "*" } else { "" };
    let name = s.preset_title.strip_suffix('*').unwrap_or(&s.preset_title);
    if grid {
        let room = (text_box.right() - x - 10) as f32;
        let full = format!("{name}{star}");
        let fits = |font: Font| ink_width(font, &full) + 1.0 <= room;
        let steps = TITLE_STEPS.map(|size| Font::new(size, font.weight).italic(s.dirty));
        // Owned: a name too long at the last step is elided there.
        let font = steps
            .into_iter()
            .find(|&font| fits(font))
            .unwrap_or(steps[steps.len() - 1]);
        let mut text = fit(name, font, room - font.width(star));
        text.push_str(star);
        l.text(part_box(x), text, font, p.text, Align::Left);
        return;
    }
    let font = font.italic(s.dirty);
    let mut text = fit(
        name,
        font,
        (text_box.right() - x - 4).max(0) as f32 - font.width(star),
    );
    text.push_str(star);
    l.text(part_box(x), text, font, p.text, Align::Left);
}

/// Width from the first inked column to the last, as Qt's `QFontMetrics::boundingRect`.
fn ink_width(font: Font, text: &str) -> f32 {
    let (mut pen, mut left, mut right) = (0.0, f32::MAX, f32::MIN);
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        let mask = &tetrad_assets::glyph(ch, font.size, font.weight).mask;
        if mask.width > 0 {
            left = left.min(pen + mask.left as f32);
            right = right.max(pen + (mask.left + mask.width as i32) as f32);
        }
        pen += tetrad_assets::pen_advance(ch, chars.peek().copied(), font.size, font.weight);
    }
    (right - left).max(0.0)
}

const ROW_Y: [i32; GRID_ROWS] = [8, 102, 196, 290];
const ROW_HEIGHT: u32 = 86;
const SLOT_X: [i32; GRID_COLUMNS] = [66, 152, 236, 322, 408, 494, 580, 666];
const SLOT: u32 = 70;
/// Row I/O tiles: 44 × 76, centred on the row. Stock QC shots `sn-0`, `st0`, `sl-2` and
/// `gd-s1` put them at x 8–51 and 750–793 (shot 013 of the manual reads 748 for the output).
const PORT_X: [i32; 2] = [8, 750];
const PORT: (u32, u32) = (44, 76);
/// Space between a cable and the I/O tile it meets (`sn-0`, `gd-s1`: the cable runs x 54 to
/// 747 between tiles that end at 51 and start at 750). At a block the cable stops at the
/// 70 px slot, 3 px from the 64 px tile (x 65 | tile 69–132 | 136).
const PORT_GAP: i32 = 2;
/// Block editor top while a block is selected: the stock editor covers the
/// bottom half (y ≈ 197–480, spec `w4-ui-spec-grid` §4).
pub(super) const PANEL_TOP: i32 = 196;
/// Stock veil over the header and Grid while a block editor is open (`b4-stock1`: the title's
/// `#ffffff` reads `#848684`, the block ring `#ff7100` reads `#843800`). Black at 123/255
/// keeps 132/255 of each channel.
const EDITOR_VEIL: u8 = 123;
/// Preset title: 70 px, weight 75 (`0x00341104`, `0x34125c`).
const TITLE: Font = Font::new(TextSize::Px70, Weight::Bold);
/// The stock title sizes: 70 px × 0.9, truncated, per step (`0x341460`: `pixelSize() * 0.9`).
/// Owned: the steps end at 32 px.
const TITLE_STEPS: [TextSize; 8] = [
    TextSize::Px70,
    TextSize::Px63,
    TextSize::Px56,
    TextSize::Px50,
    TextSize::Px45,
    TextSize::Px40,
    TextSize::Px36,
    TextSize::Px32,
];
/// Header mode label ("STOMP", "PRESET"): 22 px Bold, cap 15. "STOMP" on `st-mp1c` inks
/// x 693–765 and has the Bold stem width (T stem 3.3 px; Medium gives 2.6 px).
const MODE: Font = Font::new(TextSize::Px22, Weight::Bold);
/// Scene letter badge in the header: 24 px Bold (`st-mp1c`: "H" ink 14 × 17 px, 3.5 px stems).
const BADGE_LETTER: Font = Font::new(TextSize::Px24, Weight::Bold);
/// Header icon buttons, right to left: ⋮, save, scene badge, undo. Stock
/// centres x 765/715/668/620 (shot 013), y 24 (4.1.0 on the QC, 2026-10-16).
const ICON_X: [i32; 4] = [743, 693, 646, 598];
const ICON_Y: i32 = 2;
const ICON: u32 = 44;
/// Scene dropdown: x 454–686 from y 44, 52 px rows (shot 025, pixel sample). The
/// device shows RGB565, so stock `#313431` reads back as 49/52/49; radius 10 fits the
/// panel corner.
const SCENE_MENU: Rect = Rect::new(454, 44, 232, SCENE_COUNT as u32 * 52);
const SCENE_MENU_FILL: Rgb = Rgb(0x31, 0x34, 0x31);
const SCENE_MENU_RADIUS: f32 = 10.0;
/// Dropdown scene letter: stock cap is 16 px (shots 025, `st5b`); Px23 gives 16 px.
const SCENE_LETTER: Font = Font::new(TextSize::Px23, Weight::Medium);
/// Row port clip light (`InputPortItem` paint `0x5a177c`, `0x5a1a8c`): the tile filled
/// `#510909` with a 2 px `0xff2727` (danger) border on its edge.
const CLIP_FILL: Rgb = Rgb(0x51, 0x09, 0x09);
/// Input Gate meter reduction (`0x5a43e8`: `QColor(0xff7000)`; reads `#ff7100` on the QC).
const GATE_REDUCTION: Rgb = Rgb(0xff, 0x70, 0x00);
/// The Add to Grid gray block (`icons/grid/gray_block.png`; I: its gray).
const GRAY_BLOCK: Rgb = Rgb(0x5a, 0x5a, 0x5a);

/// Drop target while a block is dragged: the header area (stock TrashCan
/// "Drop to delete", `0x11faa58`; placement inferred).
pub(crate) fn trash_zone() -> Rect {
    Rect::new(8, 8, WIDTH - 16, (HEADER - 16) as u32)
}

pub(crate) fn scene_label(scene: SceneIndex, view: &SceneView) -> String {
    if view.label.is_empty() {
        scene.letter().to_string()
    } else {
        format!("{}: {}", scene.letter(), view.label)
    }
}

/// Display title without the letter prefix (the badge shows the letter).
pub(crate) fn scene_title(scene: SceneIndex, view: &SceneView) -> String {
    if view.label.is_empty() {
        format!("Scene {}", scene.letter())
    } else {
        view.label.clone()
    }
}

/// The stock scene letter badge (shots 013, 025, 127): yellow, grey in the header while the
/// scene dropdown is open.
pub(crate) fn scene_badge(l: &mut Layout, bounds: Rect, scene: SceneIndex, font: Font, fill: Rgb) {
    let p = l.palette;
    l.panel(bounds, fill, 4.0, None);
    l.text(
        bounds,
        scene.letter().to_string(),
        font,
        p.on_accent,
        Align::Center,
    );
}

/// What a hold on an empty slot inserts: the pair's Splitter before the slot
/// when it has none, else its Mixer after the slot. Stock allows one of each
/// per row pair ("will not fit", `0x00a37f54`); the choice order is inferred.
pub(crate) fn junction_at(s: &UiSnapshot, position: GridPosition) -> Option<UiCommand> {
    if s.occupied(position) {
        return None;
    }
    let pair = if position.row() < 2 {
        RowPair::Upper
    } else {
        RowPair::Lower
    };
    let view = s.pairs[pair.index()];
    let column = position.column();
    match (view.split, view.mix) {
        (None, mix) if mix.is_none_or(|mix| column <= mix) => Some(UiCommand::SetSplit {
            pair,
            column: Some(column),
        }),
        (Some(split), None) if column >= split => Some(UiCommand::SetMix {
            pair,
            column: Some(column),
        }),
        _ => None,
    }
}

/// A row with an input: its port is set (a row fed from above shows "Prev. Row"), or it is
/// the lower row of a pair whose Splitter feeds it.
pub(crate) fn row_has_input(s: &UiSnapshot, row: u8) -> bool {
    let split = s.pairs[usize::from(row / 2)].split.is_some();
    s.rows[usize::from(row)].input != InputPort::Empty || (row % 2 == 1 && split)
}

pub(super) fn layout(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let s = &ui.snapshot;
    // The Grid sits on black (the canvas role); the bank preview banner above
    // the grid region stays visible.
    if ui.screen() == Screen::Grid {
        let top = if s.bank_preview_active { HEADER } else { 0 };
        l.push(
            Rect::new(0, top, WIDTH, (HEIGHT as i32 - top) as u32),
            "",
            Look::Fill(p.canvas),
        );
        // Owned (O): marks the Tetrad UI apart from stock ZenUI, in the free strip under the
        // last grid row (y 467–479 on the QC). Overlays and the block editor cover it.
        l.text(
            Rect::new(0, 467, WIDTH, 13),
            concat!("Tetrad ", env!("CARGO_PKG_VERSION")),
            CAPTION,
            p.faint,
            Align::Center,
        );
    }
    let dragging = ui.dragging();
    if dragging.is_some() {
        let active = trash_zone().contains(ui.pointer.0, ui.pointer.1);
        l.push(trash_zone(), "Drop to delete", Look::DropZone { active });
    } else if ui.screen() == Screen::Grid {
        // Screens drawn over the Grid (the picker) add the header themselves.
        header(ui, l);
    }

    let editing = ui.editing_block();
    let site = ui.editing_site();
    let editor_row = editing
        .map(GridPosition::row)
        .or(site.map(|site| site.row().get()));
    let bottom = if editor_row.is_some() {
        PANEL_TOP
    } else {
        HEIGHT as i32
    };
    // Keep the selected row visible above the panel (stock SceneGraph scroll).
    let shift = editor_row.map_or(0, |row| {
        let row_bottom = HEADER + ROW_Y[usize::from(row)] + ROW_HEIGHT as i32;
        (row_bottom - (bottom - 6)).max(0)
    });
    let viewport = Rect::new(0, HEADER, WIDTH, (bottom - HEADER) as u32);
    l.clipped(viewport, |l| grid(ui, l, HEADER - shift));

    if let Some(position) = editing {
        // The header and Grid sit under the veil; the editor panel below is drawn after it.
        l.push(
            Rect::new(0, 0, WIDTH, PANEL_TOP as u32),
            "",
            Look::Veil {
                color: p.canvas,
                alpha: EDITOR_VEIL,
            },
        );
        parameter_panel(ui, l, position);
    } else if let Some(site) = site {
        site_panel(ui, l, site);
    }
    if let Some(from) = dragging {
        let (x, y) = ui.pointer;
        l.push(
            Rect::new(x - SLOT as i32 / 2, y - SLOT as i32 / 2, SLOT, SLOT),
            s.cell(from).label.clone(),
            block_look(&p, s, from, false, false, true),
        );
    }
    if ui.scene_menu {
        scene_menu(ui, l);
    }
    if let (true, Some(cells)) = (ui.add_to_grid.is_some(), &s.add_to_grid) {
        add_to_grid(l, HEADER - shift, cells);
    }
}

/// The 70 px tile of a cell when the grid's row 0 starts at `top`.
fn slot_bounds(top: i32, position: GridPosition) -> Rect {
    let center = top + ROW_Y[usize::from(position.row())] + ROW_HEIGHT as i32 / 2;
    let x = SLOT_X[usize::from(position.column())];
    Rect::new(x, center - SLOT as i32 / 2, SLOT, SLOT)
}

/// Add to Grid (`zenUI::AddToGridOverlay` `0x7c6780`, paint `0x7c6378`, press `0x7c65e0`): black
/// at opacity 0.5 over the screen; per cell, [`CellFit::Add`] a gray block with a plus,
/// [`CellFit::Replace`] the change mark, [`CellFit::NoFit`] nothing. No text. The marks are Tetrad
/// drawings (no stock images); I: their places in the cell (stock state 2 is about +31/+26).
/// A press in a cell selects it whatever its state; a press above row 0 or beside columns 0
/// and 7 cancels; a press between or below the cells does nothing.
fn add_to_grid(l: &mut Layout, top: i32, cells: &[[CellFit; GRID_COLUMNS]; GRID_ROWS]) {
    let p = l.palette;
    l.veil((p.tone(Rgb(0, 0, 0)), 128));
    for position in GridPosition::all() {
        let bounds = slot_bounds(top, position);
        let fit = cells[usize::from(position.row())][usize::from(position.column())];
        let (mark, size, label) = match fit {
            CellFit::Add => {
                let block = Look::Panel {
                    fill: p.tone(GRAY_BLOCK),
                    radius: 8.0,
                    border: None,
                };
                l.push(bounds, "", block);
                (Some(Icon::Plus), IconSize::Medium, "add")
            }
            // `icons/32/change_black.png`: a black mark; I: on a white disc so it reads over
            // the dimmed block.
            CellFit::Replace => {
                let disc = Rect::new(bounds.x + 13, bounds.y + 13, 44, 44);
                l.push(disc, "", Look::Dot { color: p.text });
                (Some(Icon::Swap), IconSize::Large, "replace")
            }
            CellFit::NoFit => (None, IconSize::Medium, "no fit"),
        };
        if let Some(icon) = mark {
            let px = size.px();
            let at = Rect::new(
                bounds.x + (SLOT - px) as i32 / 2,
                bounds.y + (SLOT - px) as i32 / 2,
                px,
                px,
            );
            let color = if fit == CellFit::Replace {
                p.canvas
            } else {
                p.text
            };
            l.push(at, "", Look::Glyph { icon, size, color });
        }
        l.control(
            bounds,
            Control::AddToGrid(Some(position)),
            true,
            label,
            Look::Area,
        );
    }
    let first = slot_bounds(top, GridPosition::all().next().expect("32 cells"));
    let right = SLOT_X[GRID_COLUMNS - 1] + SLOT as i32;
    let below = (HEIGHT as i32 - first.y) as u32;
    for zone in [
        Rect::new(0, 0, WIDTH, first.y.max(0) as u32),
        Rect::new(0, first.y, first.x as u32, below),
        Rect::new(right, first.y, (WIDTH as i32 - right) as u32, below),
    ] {
        l.control(zone, Control::AddToGrid(None), true, "", Look::Area);
    }
}

/// Stock header (shot 013): "1A Brit 2203" title (tap → Directory), the
/// icon cluster undo / scene badge / save / ⋮, and the mode label below it.
pub(super) fn header(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let s = &ui.snapshot;
    let preview = s.bank_preview_active;
    let [menu, save, scenes, undo] = ICON_X.map(|x| Rect::new(x, ICON_Y, ICON, ICON));
    l.control(
        menu,
        Control::OpenMainMenu,
        true,
        "MENU",
        Look::IconButton {
            icon: Icon::More,
            fill: p.canvas,
        },
    );
    // An available update: a 10 px green dot at the ⋮ button's top-right (seen on the QC).
    if s.updater.available_version.is_some() {
        l.push(
            Rect::new(menu.right() - 9, 0, 10, 10),
            "",
            Look::Dot { color: p.accent },
        );
    }
    // Stock save button (`0x7e7de8`, `0x7e7e8c`): `saved.png` (025, a disk with a green
    // tick) for 2 s after a save; `save_as.png` (013, a disk with an arrow) when the preset
    // cannot be saved in place; else `save.png`. Owned: no file means "save as" (cloud and
    // plugin folders are not modelled).
    let saved = ui.now < ui.saved_until;
    let icon = match (saved, s.preset_file) {
        (true, _) => Icon::SaveCheck,
        (false, false) => Icon::SaveArrow,
        (false, true) => Icon::Save,
    };
    l.control(
        save,
        Control::SavePreset,
        s.preset_id.is_some(),
        "SAVE",
        Look::IconButton {
            icon,
            fill: p.canvas,
        },
    );
    if saved {
        l.push(
            save,
            "",
            Look::Glyph {
                icon: Icon::SaveBadge,
                size: IconSize::Medium,
                color: p.accent,
            },
        );
    }
    l.control(
        scenes,
        Control::SceneMenu,
        !preview,
        scene_label(
            s.selected_scene,
            &s.scenes[usize::from(s.selected_scene.get())],
        ),
        Look::Area,
    );
    // While the dropdown is open stock greys the badge (`st5b.png`: `#636163`, black letter).
    let fill = if ui.scene_menu { p.faint } else { p.warning };
    let badge = Rect::new(scenes.x + 10, scenes.y + 10, 24, 24);
    scene_badge(l, badge, s.selected_scene, BADGE_LETTER, fill);

    // Stock hides undo and redo without history (`0x7e7eec`, `0x7e7f18`).
    // Redo has no place in the stock cluster; it sits left of undo (I).
    let mut left = undo.x;
    for (enabled, control, label, icon) in [
        (s.can_undo, Control::Undo, "UNDO", Icon::Undo),
        (s.can_redo, Control::Redo, "REDO", Icon::Redo),
    ] {
        if enabled {
            let bounds = Rect::new(left, ICON_Y, ICON, ICON);
            l.control(
                bounds,
                control,
                true,
                label,
                Look::IconButton {
                    icon,
                    fill: p.canvas,
                },
            );
            left -= ICON as i32 + 4;
        }
    }
    // Gig View opens by the stock swipe up from the Grid bottom or by footswitch; the
    // header has no button for it, so the title keeps its width (review R28).
    // `left` is now 8 px left of the leftmost header button; the mode row under the
    // undo slot bounds it when undo is hidden.
    left = (left + ICON as i32 - 4).min(undo.x - 8);
    if ui.show_cpu {
        let cpu = s
            .cpu_percent
            .map_or("CPU --%".into(), |cpu| format!("CPU {cpu}%"));
        let width = BODY_BOLD.width(&cpu).ceil() as u32 + 4;
        left -= width as i32;
        l.text(
            Rect::new(left, ICON_Y, width, ICON),
            cpu,
            BODY_BOLD,
            p.muted,
            Align::Right,
        );
        left -= 8;
    }
    // Bank preview shows its own banner over the title and mode rows.
    if preview {
        return;
    }

    let title = Rect::new(8, 8, (left - 8).max(0) as u32, (HEADER - 16) as u32);
    l.control(
        title,
        Control::Navigate(Screen::Presets),
        true,
        if s.preset_title.is_empty() {
            "- -".to_owned()
        } else {
            s.preset_title.clone()
        },
        Look::Area,
    );
    // Stock pen x 12 and baseline y 77 (`sn-0`, `stock-home`: "1" ink from x 15, tops at y 27).
    // The stock title item ends at the undo button (x 598); here at the leftmost header item.
    let text_box = Rect::new(title.x + 4, title.y + 9, (left + 8 - 12).max(0) as u32, 72);
    preset_title(l, s, text_box, TITLE, true);

    // Mode label: tap cycles, hold opens Modes Configuration (manual "Modes").
    let mode = s.footswitch_mode.map_or_else(
        || format!("MODE {}", s.mode_id),
        |mode| mode.label().to_owned(),
    );
    // Stock `updateMode` (`0x674ed0`): with a Preset MIDI Out message stored
    // (`0x0032da4c`), every mode but Preset shows `icons/mainMenuPopup/midi.png` in place of
    // its own glyph (`sn-0`: a 22 px DIN disc at x 657–678, y 61–81).
    let icon = match s.footswitch_mode {
        Some(FootswitchMode::Preset) | None => Icon::ModePreset,
        Some(_) if s.preset_midi.stored => Icon::Midi,
        Some(FootswitchMode::Scene) => Icon::ModeScene,
        Some(FootswitchMode::Stomp) => Icon::Footswitch,
        Some(FootswitchMode::Hybrid) => Icon::Gig,
    };
    // Stock (`st-mp1c`): "STOMP" pen x 692 to 766, ink x 693–765, y 64–79.
    let area = Rect::new(600, 52, 192, 40);
    l.control(area, Control::CycleMode, true, mode.clone(), Look::Area);
    let text_width = MODE.width(&mode).ceil() as i32;
    let text = Rect::new(
        area.right() - 26 - text_width,
        area.y + 1,
        text_width as u32,
        40,
    );
    l.text(text, mode, MODE, p.text, Align::Left);
    // The 24 px glyph box starts 36 px left of the text pen (`st-rb`, `st-mp1c`: box x 656).
    l.push(
        Rect::new(text.x - 36, area.y + 8, 24, 24),
        "",
        Look::Glyph {
            icon,
            size: IconSize::Medium,
            color: p.text,
        },
    );
    inhibit_status(ui, l, undo.x);
}

/// M406 header status (`NavMenuBlock` button `+0x158`, 55 × 58 at (0, 48) of the 192 px
/// block at x 596; Tetrad's icon cluster sits 2 px right, so it starts at the undo slot). Its
/// 24 px icon is at (10, 10). `globalEqFlash` (`0x7e8194`) steps every 500 ms through
/// `EQ_disabled`, `EQ_enabled`, then, with both modules off, `in_gate_disabled` and
/// `in_gate_enabled`; with one off it alternates that module's two icons. A tapped status
/// stays on `EQ_disabled` (`0x7e8494`). Owned redraws (I): the EQ sliders, and an "In" tile
/// with the gate bar; an off icon is grey with a red slash.
fn inhibit_status(ui: &UiState, l: &mut Layout, x: i32) {
    let p = l.palette;
    let Some(status) = ui.inhibit_status else {
        return;
    };
    let bounds = Rect::new(x, 48, 55, 58);
    // (EQ icon, module on) per flash step.
    let steps: &[(bool, bool)] = match (status.global_eq, status.input_gate) {
        (true, true) => &[(true, false), (true, true), (false, false), (false, true)],
        (true, false) => &[(true, false), (true, true)],
        _ => &[(false, false), (false, true)],
    };
    let step = ui.now.saturating_sub(status.since) / INHIBIT_FLASH_MS;
    let (eq, on) = if status.steady {
        (true, false)
    } else {
        steps[step as usize % steps.len()]
    };
    let icon = Rect::new(bounds.x + 10, bounds.y + 10, 24, 24);
    let color = if on { p.text } else { p.muted };
    if eq {
        l.push(
            icon,
            "",
            Look::Glyph {
                icon: Icon::Eq,
                size: IconSize::Medium,
                color,
            },
        );
    } else {
        l.panel(icon, p.surface, 4.0, None);
        l.text(icon, "In", BODY_BOLD, color, Align::Center);
        if on {
            l.push(
                Rect::new(icon.x + 4, icon.y + 2, 16, 2),
                "",
                Look::Fill(p.tone(GATE_REDUCTION)),
            );
        }
    }
    if !on {
        let (x0, y0) = (icon.x as f32, icon.y as f32);
        l.line((x0 + 22.0, y0 + 2.0), (x0 + 2.0, y0 + 22.0), 2.0, p.danger);
    }
    l.control(
        bounds,
        Control::GlobalEqStatus,
        true,
        "GLOBAL EQ STATUS",
        Look::Area,
    );
}

/// Stock two-word I/O tile text ("In 1", "Prev. Row", "Multi Out"); empty
/// for an unassigned port, which shows "+".
fn input_text(port: InputPort) -> &'static str {
    match port {
        InputPort::Empty => "",
        InputPort::Input1 => "In 1",
        InputPort::Input2 => "In 2",
        InputPort::Input1And2 => "In 1/2",
        InputPort::Return1 => "Ret 1",
        InputPort::Return2 => "Ret 2",
        InputPort::Return1And2 => "Ret 1/2",
        InputPort::PrevRow => "Prev. Row",
        InputPort::UsbIn5 => "USB 5",
        InputPort::UsbIn6 => "USB 6",
        InputPort::UsbIn7 => "USB 7",
        InputPort::UsbIn8 => "USB 8",
        InputPort::UsbIn5And6 => "USB 5/6",
        InputPort::UsbIn7And8 => "USB 7/8",
        InputPort::SidechainBuffer => "Side Chain",
    }
}

fn output_text(port: OutputPort) -> &'static str {
    match port {
        OutputPort::Empty => "",
        OutputPort::Xlr1And2 => "Out 1/2",
        OutputPort::Output3And4 => "Out 3/4",
        OutputPort::Send1And2 => "Send 1/2",
        OutputPort::Xlr1 => "Out 1",
        OutputPort::Xlr2 => "Out 2",
        OutputPort::Output3 => "Out 3",
        OutputPort::Output4 => "Out 4",
        OutputPort::Send1 => "Send 1",
        OutputPort::Send2 => "Send 2",
        OutputPort::UsbOut5 => "USB 5",
        OutputPort::UsbOut6 => "USB 6",
        OutputPort::UsbOut7 => "USB 7",
        OutputPort::UsbOut8 => "USB 8",
        OutputPort::UsbOut5And6 => "USB 5/6",
        OutputPort::UsbOut7And8 => "USB 7/8",
        OutputPort::NextRow3 => "Row 3",
        OutputPort::NextRow4 => "Row 4",
        OutputPort::NextRow3And4 => "Row 3/4",
        OutputPort::MultipleOuts => "Multi Out",
        OutputPort::UsbOut3 => "USB 3",
        OutputPort::UsbOut4 => "USB 4",
        OutputPort::UsbOut3And4 => "USB 3/4",
    }
}

fn block_look(
    p: &Palette,
    s: &UiSnapshot,
    position: GridPosition,
    selected: bool,
    drop_target: bool,
    ghost: bool,
) -> Look {
    // Stock picks icon and colour by model id as well as category (`0x2eb99c`, `0x2eb380`).
    let (info, capture) = (s.blocks[position.index()], s.captures[position.index()]);
    Look::Block {
        hue: s
            .occupied(position)
            .then(|| info.hue(capture).unwrap_or(p.muted)),
        fill: info.fill(capture),
        icon: info.icon(capture),
        bypassed: s.cell(position).bypassed,
        selected,
        drop_target,
        ghost,
        capture_two: info.model_id == Some(CAPTURE_TWO_MODEL),
    }
}

/// Neural Capture 2.0 (`0x36b1`).
const CAPTURE_TWO_MODEL: u32 = 14_001;

/// Centre x of the cable gap left of column `gap` (8: right of column 7), where a junction
/// cable turns down. Stock keeps one point per gap (`splitMixControlPointPos` `0x5c5868`).
/// Measured: gaps 0 and 8 at x 61 and 740 (`sl-2.png`), gaps 5 and 7 at x 486 and 658
/// (`stock-home-20261015.png`); I: the other inner gaps also sit 8 px left of their slot.
fn gap_x(gap: u8) -> f32 {
    match gap {
        0 => (SLOT_X[0] - 5) as f32,
        1..=7 => (SLOT_X[usize::from(gap)] - 8) as f32,
        _ => (SLOT_X[7] + SLOT as i32 + 4) as f32,
    }
}

/// Where the Splitter cable meets path B and where the Mixer cable leaves it: x 66 and 736
/// (`stock-home-20261015.png`, `sl-2.png`). Stock takes both from path B's row item, not
/// from the junction columns (`0x5c4074`).
const PATH_B_ENDS: (f32, f32) = (66.0, 736.0);
/// Bend radius of a junction cable that turns at an inner gap: twice the 4.0 of
/// `convertSplitLineToPath` `0x5c1620` (`stock-home-20261015.png`: 8 px turns).
const BEND: f32 = 8.0;
/// The port of path B (`output` false: its input) that the pair's Splitter (Mixer) replaces.
/// Stock draws it at opacity 0.2 with the label of the stored port, or "+" when none is
/// stored ([`RowView::replaced_output`]; `st-mp1g.png` row 4: a faded "Multi Out").
fn faded_port(s: &UiSnapshot, row: usize, output: bool) -> bool {
    let (view, pair) = (s.rows[row], s.pairs[row / 2]);
    row % 2 == 1
        && match output {
            false => view.input == InputPort::Empty && pair.split.is_some(),
            true => view.output == OutputPort::Empty && pair.mix.is_some(),
        }
}

/// Horizontal cable extent of a row, or `None` when it carries no signal.
/// An assigned row runs between its I/O tiles (spec §2: "Drawn only on assigned rows").
/// Path B of a pair without its own ports runs from its first to its last column, whatever
/// the junction columns: stock draws the split cable from the Splitter to path B's column
/// 0 and the mix cable from its column 7 to the Mixer (`0x5c4074`).
fn cable(s: &UiSnapshot, row: usize) -> Option<(f32, f32)> {
    let view = s.rows[row];
    let start = if view.input != InputPort::Empty {
        (PORT_X[0] + PORT.0 as i32 + PORT_GAP) as f32
    } else if faded_port(s, row, false) {
        PATH_B_ENDS.0
    } else {
        return None;
    };
    let end = if faded_port(s, row, true) {
        PATH_B_ENDS.1
    } else {
        (PORT_X[1] - PORT_GAP) as f32
    };
    Some((start, end))
}

/// The cable of `row` in runs: cut at the slot of each active block (`sn-0`, `gd-s1`: 3 px
/// either side of the tile). A bypassed block cuts nothing: stock draws the cable over its
/// tile (`sl-1`: the cable runs through the ring and the glyph), as [`grid`] does after the
/// tiles.
fn cable_runs(s: &UiSnapshot, row: usize) -> Vec<(f32, f32)> {
    let Some((start, end)) = cable(s, row) else {
        return Vec::new();
    };
    let mut runs = Vec::new();
    let mut from = start;
    for (column, &slot) in SLOT_X.iter().enumerate() {
        let position = GridPosition {
            row: row as u8,
            column: column as u8,
        };
        if !s.occupied(position) || s.cell(position).bypassed {
            continue;
        }
        let slot = slot as f32;
        if slot > from {
            runs.push((from, slot.min(end)));
        }
        from = from.max(slot + SLOT as f32);
    }
    if end > from {
        runs.push((from, end));
    }
    runs
}

/// The corners of a junction cable from path A (y `upper`) down to path B (y `lower`) and
/// its bend radius, as stock `convertSplitLineToPath` `0x5c1620` (Splitter) and its mirror
/// `0x5c2018` (Mixer) draw them:
/// - At column 0 (Splitter) or 7 (Mixer) the cable drops straight down at the outer gap and
///   bends into path B's end, with a radius of half its x span (`0x5c4074` passes the edge
///   flag; `sl-2.png`: 5 px left, 4 px right).
/// - At an inner gap it leaves path A 8 px before the gap, drops to mid-way between the rows,
///   runs back to path B's end and drops again 8 px outside it (`stock-home-20261015.png`).
fn junction_path(
    site: EditorSite,
    column: u8,
    (upper, lower): (f32, f32),
) -> (Vec<(f32, f32)>, f32) {
    let (gap, edge, end, outwards) = match site {
        EditorSite::Splitter(_) => (gap_x(column), column == 0, PATH_B_ENDS.0, -1.0),
        _ => (gap_x(column + 1), column == 7, PATH_B_ENDS.1, 1.0),
    };
    if edge {
        let corners = vec![
            (2.0 * gap - end, upper),
            (gap, upper),
            (gap, lower),
            (end, lower),
        ];
        return (corners, (end - gap).abs());
    }
    let (start, outer, mid) = (
        gap + outwards * BEND,
        end + outwards * BEND,
        (upper + lower) / 2.0,
    );
    let corners = vec![
        (start, upper),
        (gap, upper),
        (gap, mid),
        (outer, mid),
        (outer, lower),
        (end, lower),
    ];
    (corners, BEND)
}

/// A 2 px cable through `corners` (each leg horizontal or vertical), every inner corner
/// rounded to a quarter circle of `radius`.
fn rounded_cable(l: &mut Layout, corners: &[(f32, f32)], radius: f32) {
    let p = l.palette;
    let unit = |(ax, ay): (f32, f32), (bx, by): (f32, f32)| {
        let length = (bx - ax).abs() + (by - ay).abs();
        ((bx - ax) / length, (by - ay) / length, length)
    };
    let last = corners.len() - 2;
    for (index, leg) in corners.windows(2).enumerate() {
        let (dx, dy, length) = unit(leg[0], leg[1]);
        let head = if index > 0 { radius } else { 0.0 };
        let tail = if index < last { radius } else { 0.0 };
        if length > head + tail {
            let from = (leg[0].0 + dx * head, leg[0].1 + dy * head);
            let to = (leg[1].0 - dx * tail, leg[1].1 - dy * tail);
            l.line(from, to, 2.0, p.cable);
        }
    }
    for turn in corners.windows(3) {
        let ((ix, iy, _), (ox, oy, _)) = (unit(turn[0], turn[1]), unit(turn[1], turn[2]));
        let (cx, cy) = (
            turn[1].0 + radius * (ox - ix),
            turn[1].1 + radius * (oy - iy),
        );
        // Seen from the centre the arc runs from `-outgoing` to `incoming`; the renderer
        // sweeps clockwise (y down), so a counter-clockwise turn starts at the other end.
        let angle = |x: f32, y: f32| y.atan2(x).to_degrees().rem_euclid(360.0);
        let start = match ix * oy - iy * ox > 0.0 {
            true => angle(-ox, -oy),
            false => angle(ix, iy),
        };
        let size = (2.0 * radius) as i32 + 2;
        let at = Rect::new(
            cx as i32 - size / 2,
            cy as i32 - size / 2,
            size as u32,
            size as u32,
        );
        let look = Look::Arc {
            width: 2.0,
            start,
            sweep: 90.0,
            color: p.cable,
        };
        l.push(at, "", look);
    }
}

/// The Splitter and Mixer cables of a pair ([`junction_path`]). Stock shows the S and M
/// tokens only in the split point edit mode (its `SplitControlPointGrid` starts hidden,
/// `0x595b6c` → `0x5c5728`); a tap on a cable opens the junction editor
/// (`showSplitterMixerEditorSignal`).
fn junctions(l: &mut Layout, pair: PairView, rows: RowPair, (upper, lower): (f32, f32)) {
    let cables = [
        (pair.split, EditorSite::Splitter(rows)),
        (pair.mix, EditorSite::Mixer(rows)),
    ];
    for (column, site) in cables {
        let Some(column) = column else {
            continue;
        };
        let (corners, radius) = junction_path(site, column, (upper, lower));
        rounded_cable(l, &corners, radius);
        // The tap area: the drop at the junction's gap.
        let x = corners[1].0;
        let half = SLOT as f32 / 2.0;
        let area = Rect::new(
            x as i32 - 10,
            (upper + half) as i32,
            20,
            (lower - upper - 2.0 * half) as u32,
        );
        l.control(area, Control::Site(site), true, "", Look::Area);
    }
}

/// The grid region alone (no header, editor or scene menu), for the offscreen preset render
/// ([`UiState::preset_grid`]).
pub(crate) fn offscreen(ui: &UiState) -> Layout {
    let mut l = Layout::new(ui.snapshot.appearance.palette);
    let viewport = Rect::new(0, HEADER, WIDTH, (HEIGHT as i32 - HEADER) as u32);
    l.clipped(viewport, |l| grid(ui, l, HEADER));
    l
}

fn grid(ui: &UiState, l: &mut Layout, top: i32) {
    let p = l.palette;
    let s = &ui.snapshot;
    let enabled = !s.bank_preview_active;
    let center = |row: usize| top + ROW_Y[row] + ROW_HEIGHT as i32 / 2;
    // Cables first, cut at the active blocks; the cable over a bypassed tile is drawn after
    // the tiles.
    for row in 0..GRID_ROWS {
        let y = center(row) as f32;
        for (start, end) in cable_runs(s, row) {
            l.line((start, y), (end, y), 2.0, p.cable);
        }
    }
    for (index, pair) in s.pairs.iter().enumerate() {
        let rows = [RowPair::Upper, RowPair::Lower][index];
        let (upper, lower) = (center(index * 2) as f32, center(index * 2 + 1) as f32);
        junctions(l, *pair, rows, (upper, lower));
    }
    for row in 0..GRID_ROWS as u8 {
        let view = s.rows[usize::from(row)];
        let y = center(usize::from(row)) - PORT.1 as i32 / 2;
        let clips = [s.grid_clips.inputs, s.grid_clips.outputs].map(|lit| lit[usize::from(row)]);
        for ((x, output, text, replaced), clipping) in [
            (
                PORT_X[0],
                false,
                input_text(view.input),
                input_text(view.replaced_input),
            ),
            (
                PORT_X[1],
                true,
                output_text(view.output),
                output_text(view.replaced_output),
            ),
        ]
        .into_iter()
        .zip(clips)
        {
            let bounds = Rect::new(x, y, PORT.0, PORT.1);
            if faded_port(s, usize::from(row), output) {
                l.push(bounds, replaced, Look::Port { faded: true });
                continue;
            }
            l.control(
                bounds,
                Control::Port {
                    row: Row(row),
                    output,
                },
                enabled,
                text,
                Look::Port { faded: false },
            );
            if clipping {
                clip_light(l, bounds, text);
            }
            if let (false, Some(meter)) = (output, s.gate_meters[usize::from(row)]) {
                gate_meter(l, bounds, meter);
            }
        }
        if s.sidechain.inputs[usize::from(row)] {
            let right = PORT_X[0] + PORT.0 as i32;
            let center = center(usize::from(row));
            sidechain_mark(l, right - 6, center, Icon::SidechainSource, false);
        }
    }
    let slot = |position: GridPosition| slot_bounds(top, position);
    let hover = ui.dragging().and_then(|from| {
        GridPosition::all().find(|position| {
            *position != from && slot(*position).contains(ui.pointer.0, ui.pointer.1)
        })
    });
    for position in GridPosition::all() {
        let label = if s.occupied(position) {
            s.cell(position).label.clone()
        } else {
            "+".into()
        };
        l.control(
            slot(position),
            Control::SelectCell(position),
            enabled,
            label,
            block_look(
                &p,
                s,
                position,
                s.selected_block == Some(position),
                hover == Some(position),
                false,
            ),
        );
        if s.occupied(position) && s.cell(position).bypassed {
            let (row, at) = (usize::from(position.row()), slot(position));
            let y = center(row) as f32;
            for (start, end) in cable_runs(s, row) {
                let (start, end) = (start.max(at.x as f32), end.min(at.right() as f32));
                if end > start {
                    l.line((start, y), (end, y), 2.0, p.cable);
                }
            }
        }
        // Stock `0x005a6004`: `plugin_lock.png` (24 px) at (4, 54) of an unlicensed
        // plugin block; bypassed it sits on a black silhouette at 20 % opacity. That item
        // is 82 px with the 64 px tile at (9, 9): its marks are symmetric about x 41 and
        // the sidechain marks sit on the tile edges at mid-height (shot 026). So the lock
        // overhangs the tile's lower left corner.
        let locked = s.blocks[position.index()]
            .model_id
            .is_some_and(|id| s.locked_models.contains(&id));
        if locked && s.occupied(position) {
            let at = slot(position);
            let lock = Rect::new(at.x - 5, at.y + 45, 24, 24);
            let color = if s.cell(position).bypassed {
                p.text.mix(p.canvas, 204)
            } else {
                p.text
            };
            l.push(
                lock,
                "",
                Look::Glyph {
                    icon: Icon::Lock,
                    size: IconSize::Medium,
                    color,
                },
            );
        }
        let (at, dim) = (slot(position), s.cell(position).bypassed);
        let index = position.index();
        if s.occupied(position) {
            plugin_tag(l, at, s.blocks[index], s.captures[index], dim);
        }
        // `capture_type.png` (24 px) at item (4, 54). Bypassed, stock paints a black 24 px
        // disc, then the badge at opacity 0.2. The 565 shot reads white as 47 (`st-mp1a.png`):
        // I: Qt's RGB16 blend keeps 5 bits of alpha, so 0.2 acts as 6/32.
        if s.occupied(position) && s.blocks[index].capture_badge(s.captures[index]) {
            let color = if dim {
                p.text.mix(p.canvas, 208)
            } else {
                p.text
            };
            capture_badge(l, at, color);
        }
        // The sidechain marks sit at item (3, 31) and (60, 31): tile x − 6 and + 51.
        let center = at.y + SLOT as i32 / 2;
        if s.sidechain.sinks[position.index()] {
            sidechain_mark(l, at.x - 1, center, Icon::SidechainSink, dim);
        }
        if s.sidechain.sources[position.index()] {
            sidechain_mark(l, at.x + 56, center, Icon::SidechainSource, dim);
        }
    }
}

/// Capture type badge of a block whose 70 px box is `at` (`st5a.png`; Gig View shot 103 draws
/// the same place): stock `capture_type.png` is an opaque 24 px black disc with the stripes at
/// (2, 2)–(21, 21). So Tetrad draws a 24 px black disc at (−2, +48), which hides the tile outline
/// under it (`st-rb.png`), and the owned redraw [`Icon::CaptureType`] in the same box.
pub(super) fn capture_badge(l: &mut Layout, at: Rect, color: Rgb) {
    let p = l.palette;
    let size = IconSize::Medium;
    let badge = Rect::new(at.x - 2, at.y + 48, size.px(), size.px());
    l.push(badge, "", Look::Dot { color: p.canvas });
    let icon = Icon::CaptureType;
    l.push(badge, "", Look::Glyph { icon, size, color });
}

/// Plugin tag text: stock `QFont("IBM Plex Sans")`, pixel size 11, weight 75 (`0x5a68dc`).
const TAG: Font = Font::new(TextSize::Px11, Weight::Bold);

/// Plugin tag of a block whose 70 px box is `at` (`0x005a6004`; text and colours set by
/// `0x59e194`): [`BlockInfo::plugin_tag`] centred in [`TAG`], ink
/// [`ModelCategory::tag_ink`], on a radius 5 rect in the block colour at item (25, 2, 32, 16),
/// so box (19, −4) (`3d-worship-v5`: "NLY" rect x 513–544, y 296–311). Dim (bypassed), stock
/// paints the rect black and then the tag at 20 %; black text stays at 100 %.
fn plugin_tag(
    l: &mut Layout,
    at: Rect,
    block: BlockInfo,
    capture: Option<CaptureDevice>,
    dim: bool,
) {
    let p = l.palette;
    let (Some(text), Some(hue), Some(category)) =
        (block.plugin_tag(), block.hue(capture), block.category)
    else {
        return;
    };
    let ink = category.tag_ink();
    let (fill, ink) = if dim {
        let fill = hue.mix(p.canvas, 204);
        (
            fill,
            if ink == p.canvas {
                ink
            } else {
                ink.mix(fill, 204)
            },
        )
    } else {
        (hue, ink)
    };
    let tag = Rect::new(at.x + 19, at.y - 4, 32, 16);
    l.panel(tag, fill, 5.0, None);
    l.text(tag, text, TAG, ink, Align::Center);
}

/// A stock sidechain mark (`0x005a6004`: `icons/24/sidechain_source.png` or
/// `sidechain_sink.png` in a 20 px box at 20 % opacity on a bypassed block) with its 16 px
/// glyph at `x` and the row `center`. The image's black backing shows as a 2 px rim that cuts
/// the cable (`st5a.png`); at 20 % only the grey square shows, the cut-outs let the cable through.
fn sidechain_mark(l: &mut Layout, x: i32, center: i32, icon: Icon, dim: bool) {
    let p = l.palette;
    let size = IconSize::Small;
    let px = size.px();
    let glyph = Rect::new(x, center - px as i32 / 2, px, px);
    if !dim {
        let backing = Look::Panel {
            fill: p.canvas,
            radius: 6.0,
            border: None,
        };
        let rim = Rect::new(glyph.x - 2, glyph.y - 2, px + 4, px + 4);
        l.push(rim, "", backing);
    }
    let color = if dim {
        p.text.mix(p.canvas, 204)
    } else {
        p.text
    };
    l.push(glyph, "", Look::Glyph { icon, size, color });
}

/// The clip light over a port tile; the port label is drawn again on top, as `Look::Port`
/// lays it out (stock paints the text after the fill).
fn clip_light(l: &mut Layout, bounds: Rect, text: &str) {
    let p = l.palette;
    // `0x5a177c` strokes the tile path itself (item rect inset 3, radius 14) with a 2 px pen
    // on its centre line: the border runs 1 px out of the tile and 1 px into it.
    let panel = |fill, radius| Look::Panel {
        fill,
        radius,
        border: None,
    };
    l.push(bounds.grow(1, 1), "", panel(p.danger, PORT_RADIUS + 1.0));
    l.push(
        bounds.inset(1, 1),
        "",
        panel(p.tone(CLIP_FILL), PORT_RADIUS - 1.0),
    );
    let (first, second) = text.split_once(' ').unwrap_or((text, ""));
    for (line, text) in [first, second].into_iter().enumerate() {
        let y = bounds.y + PORT_LINE_TOP + line as i32 * PORT_LINE_PITCH;
        let row = Rect::new(bounds.x, y, bounds.width, 22);
        l.text(row, text, BODY, p.text, Align::Center);
    }
}

/// The Input Gate meter over a row input port (`InputPortItem` `0x5a43e8`; children at item
/// (13, 10), which is port (10, 7) on `stock-home-20261015.png`). A black 24 × 4 bar with
/// radius 3; over it the gain reduction in orange `0xff7000` from the open px to the end, also
/// radius 3; and a 1 px orange mark at the open edge while it is more than 1 px from either
/// end (`0x5a163c`). Stock paints these child items over the port and its clip light.
fn gate_meter(l: &mut Layout, port: Rect, meter: GateMeter) {
    let p = l.palette;
    let (width, open) = (u32::from(GateMeter::WIDTH), u32::from(meter.open()));
    let bar = Rect::new(port.x + 10, port.y + 7, width, 4);
    let panel = |fill| Look::Panel {
        fill,
        radius: 3.0,
        border: None,
    };
    l.push(bar, "", panel(p.canvas));
    let edge = bar.x + open as i32;
    if open < width {
        let reduction = Rect::new(edge, bar.y, width - open, bar.height);
        l.push(reduction, "", panel(p.tone(GATE_REDUCTION)));
    }
    if open > 1 && open + 2 < width {
        let mark = Rect::new(edge, bar.y, 1, bar.height);
        l.push(mark, "", Look::Fill(p.tone(GATE_REDUCTION)));
    }
}

/// The active row's fill. Stock clips it to the panel's rounded corners on the
/// first and last rows. Those rows are drawn as a rounded panel that overhangs
/// the row by the radius, and the overhang is painted back in the panel colour.
/// Middle rows stay square.
fn active_row(l: &mut Layout, row: Rect, scene: SceneIndex) {
    let p = l.palette;
    let fill = p.canvas.mix(p.panel, 55);
    let scene_menu_fill = p.tone(SCENE_MENU_FILL);
    let radius = SCENE_MENU_RADIUS as u32;
    let index = usize::from(scene.get());
    if index == 0 {
        let body = Rect::new(row.x, row.y, row.width, row.height + radius);
        l.panel(body, fill, SCENE_MENU_RADIUS, None);
        let overhang = Rect::new(row.x, row.bottom(), row.width, radius);
        l.push(overhang, "", Look::Fill(scene_menu_fill));
    } else if index == SCENE_COUNT - 1 {
        let top = row.y - radius as i32;
        let body = Rect::new(row.x, top, row.width, row.height + radius);
        l.panel(body, fill, SCENE_MENU_RADIUS, None);
        let overhang = Rect::new(row.x, top, row.width, radius);
        l.push(overhang, "", Look::Fill(scene_menu_fill));
    } else {
        l.push(row, "", Look::Fill(fill));
    }
}

/// Stock scene dropdown (shot 025, `st5b.png`): yellow letter badge and scene name per row,
/// the active scene in green on a darker row. A pick or a tap outside closes it. No pencil:
/// scene names are edited from Gig View (EDIT SCENE).
fn scene_menu(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let s = &ui.snapshot;
    l.popover(SCENE_MENU);
    l.panel(SCENE_MENU, p.tone(SCENE_MENU_FILL), SCENE_MENU_RADIUS, None);
    let row_height = SCENE_MENU.height / SCENE_COUNT as u32;
    for scene in SceneIndex::all() {
        let row = Rect::new(
            SCENE_MENU.x,
            SCENE_MENU.y + i32::from(scene.get()) * row_height as i32,
            SCENE_MENU.width,
            row_height,
        );
        let view = &s.scenes[usize::from(scene.get())];
        let active = scene == s.selected_scene;
        if active {
            active_row(l, row, scene);
        }
        l.control(
            row,
            Control::SelectScene(scene),
            !s.bank_preview_active,
            scene_label(scene, view),
            Look::Area,
        );
        let badge = Rect::new(row.x + 16, row.y + (row.height as i32 - 24) / 2, 24, 24);
        l.panel(badge, p.warning, 4.0, None);
        // Stock's 16 px cap (`st5b`) sits 1 px left of and 1 px below centred Px23 text.
        let letter = Rect::new(badge.x - 1, badge.y + 1, badge.width, badge.height);
        let text = scene.letter().to_string();
        l.text(letter, text, SCENE_LETTER, p.on_accent, Align::Center);
        // Stock sets the name 1 px lower than the centre line (its "+" glyph
        // tops at y 168 in row C; Tetrad at 167).
        l.text(
            Rect::new(
                row.x + 48,
                row.y + 1,
                row.width.saturating_sub(58),
                row.height,
            ),
            scene_title(scene, view),
            BODY,
            if active { p.accent } else { p.text },
            Align::Left,
        );
    }
}

/// Stock block editor header (shot 127): ⋮, the category caption over the
/// model name in the block colour (tap → replace device), the ◀ A ▶ scene
/// selector, power bypass and ✓; then the 5 × 2 parameter card.
/// Lane control and junction editor (manual "INPUT & OUTPUT BLOCKS", "SPLITTER & MIXER
/// BLOCKS"): the docked panel with the element's name, CLOSE and its knobs. Owned header:
/// no stock shot of these editors.
fn site_panel(ui: &UiState, l: &mut Layout, site: EditorSite) {
    let p = l.palette;
    let s = &ui.snapshot;
    let top = PANEL_TOP;
    l.push(
        Rect::new(0, top, WIDTH, HEIGHT - top as u32),
        "",
        Look::Fill(p.background),
    );
    let header = top + 8;
    let hue = match site {
        EditorSite::Splitter(_) => ModelCategory::Splitter.hue(),
        EditorSite::Mixer(_) => ModelCategory::Mixer.hue(),
        EditorSite::InputGate(_) | EditorSite::LaneOutput(_) => p.text,
    };
    l.text(
        Rect::new(16, header + 1, 400, 18),
        format!("ROW {}", site.row().get() + 1),
        CAPTION,
        p.muted,
        Align::Left,
    );
    l.text(
        Rect::new(16, header + 19, 400, 24),
        site.title(),
        MEDIUM,
        hue,
        Align::Left,
    );
    let tabs = tab_bar(ui, l, 718, header);
    if matches!(site, EditorSite::Splitter(_) | EditorSite::Mixer(_)) {
        quick_buttons(l, tabs - 8, header);
    }
    l.control(
        Rect::new(726, header, 66, 44),
        Control::Param(ParamControl::Close),
        true,
        "CLOSE",
        Look::IconButton {
            icon: Icon::Check,
            fill: p.panel,
        },
    );
    let card = Rect::new(
        8,
        header + 52,
        WIDTH - 16,
        (HEIGHT as i32 - 8 - header - 52) as u32,
    );
    if s.parameters.is_empty() {
        l.panel(card, p.panel, 8.0, None);
        l.text(card, "No parameters", BODY, p.faint, Align::Center);
        return;
    }
    let first = (ui.param_page * KNOB_COUNT).min(s.parameters.len());
    crate::widgets::parameter_panel(l, card, &s.parameters[first..], hue, &[], ui.focus);
}

/// Stock editor model name (`0x8277e4`, `b4-stock1`): the first of 20, 18 and 16 px
/// (`0x12038c8`) at which the name fits, in a 26 px row 20 px below the header top. With a
/// Virtual Device Preset match (`+0x13c`, set by `0x4b7eb4`) [`Icon::PresetMatch`] follows
/// the name, 5 px into the row. Stock measures the name with a placeholder ("!//" at 20 px)
/// and ends the glyph where the placeholder ends; on `b4-stock1` that is 4 px after the
/// name's advance. Tetrad uses that 4 px gap at every size (I: Qt metrics differ from Tetrad's).
fn model_name(l: &mut Layout, name_box: Rect, name: &str, hue: Rgb, matched: bool) {
    const GAP: f32 = 4.0;
    let mark = if matched { GAP + 16.0 } else { 0.0 };
    let font = [TextSize::Px20, TextSize::Px18, TextSize::Px16]
        .into_iter()
        .map(|size| Font::new(size, Weight::Regular))
        .find(|font| font.width(name) + mark <= name_box.width as f32)
        .unwrap_or(BODY);
    let row = Rect::new(name_box.x, name_box.y + 20, name_box.width, 26);
    // Qt centres the font height, not the cap height: the stock baseline is 1 px lower.
    let text = Rect::new(row.x, row.y + 1, row.width, row.height);
    l.text(text, name, font, hue, Align::Left);
    if matched {
        let x = name_box.x + (font.width(name) + GAP).round() as i32;
        l.push(
            Rect::new(x.min(name_box.right() - 16), row.y + 5, 16, 16),
            "",
            Look::Glyph {
                icon: Icon::PresetMatch,
                size: IconSize::Small,
                color: hue,
            },
        );
    }
}

/// `quick1.png` / `quick2.png` (`0x3dfefc`): 114 px buttons left of the editor's right-hand
/// controls, quick1 first; the icon band is 114 × 37 at y 11. The route marks are owned
/// drawings: one lane forks into two (quick1), and the two rejoin (quick2).
fn quick_buttons(l: &mut Layout, right: i32, header: i32) {
    let p = l.palette;
    const WIDTH: i32 = 114;
    for (index, quick) in [QuickSplit::Split, QuickSplit::SplitAndMix]
        .into_iter()
        .enumerate()
    {
        let x = right - (2 - index as i32) * (WIDTH + 4) + 4;
        let button = Rect::new(x, header, WIDTH as u32, 44);
        l.panel(button, p.surface, 8.0, None);
        l.control(
            button,
            Control::QuickSplit(quick),
            true,
            if quick == QuickSplit::Split {
                "QUICK SPLIT"
            } else {
                "QUICK SPLIT MIX"
            },
            Look::Area,
        );
        let (y, top, bottom) = (
            header as f32 + 22.0,
            header as f32 + 13.0,
            header as f32 + 31.0,
        );
        let (left, fork, join, end) = (
            x as f32 + 22.0,
            x as f32 + 40.0,
            x as f32 + 74.0,
            x as f32 + 92.0,
        );
        l.line((left, y), (fork, y), 2.0, p.text);
        for rail in [top, bottom] {
            l.line((fork, y), (fork + 8.0, rail), 2.0, p.text);
            match quick {
                QuickSplit::Split => l.line((fork + 8.0, rail), (end, rail), 2.0, p.text),
                QuickSplit::SplitAndMix => {
                    l.line((fork + 8.0, rail), (join - 8.0, rail), 2.0, p.text);
                    l.line((join - 8.0, rail), (join, y), 2.0, p.text);
                }
            }
        }
        if quick == QuickSplit::SplitAndMix {
            l.line((join, y), (end, y), 2.0, p.text);
        }
    }
}

fn parameter_panel(ui: &UiState, l: &mut Layout, position: GridPosition) {
    let p = l.palette;
    let s = &ui.snapshot;
    // Pattern Tremolo and cab editors fill the screen: the header row, then an 800 × 420
    // body (`0x3ae61c`, `0x643e44`; header row height I).
    let editor = ui.model_editor();
    let full_screen = editor.is_some_and(super::model_editor::ModelEditor::full_screen);
    let top = if full_screen { 0 } else { PANEL_TOP };
    l.push(
        Rect::new(0, top, WIDTH, HEIGHT - top as u32),
        "",
        Look::Fill(p.background),
    );
    let cell = s.cell(position);
    let category = s.blocks[position.index()].category;
    let hue = category.map_or(p.muted, ModelCategory::hue);
    let header = top + 8;
    // Stock editor keys (MENU, footswitch, scenes, bypass) are `TAB_IDLE`; ✓ stays panel
    // (`b4-stock1`).
    l.control(
        Rect::new(8, header, 44, 44),
        Control::Param(ParamControl::Menu),
        true,
        "MENU",
        Look::IconButton {
            icon: Icon::More,
            fill: p.tone(TAB_IDLE),
        },
    );
    // FOOTSWITCH ASSIGNMENT (shot 058) while any tile is a stomp: footswitch glyph and
    // the tile letter, or "?" unassigned. Stock (`b4-stock1`) has no divider here.
    let stomp_button =
        (0..FOOTSWITCH_COUNT).any(|tile| ui.tile_mode(tile) == FootswitchMode::Stomp);
    let left_of_scenes = if stomp_button { 422 } else { 494 };
    let tabs_right = left_of_scenes - 8;
    let name_right = tab_bar(ui, l, tabs_right, header);
    let name_box = Rect::new(60, header, (name_right - 60) as u32, 44);
    let name = if cell.label.is_empty() {
        "-".to_owned()
    } else {
        cell.label.clone()
    };
    // Stock title area: a 200 × 60 `Button` from the editor top, right of ⋮ (`0x4ba858`). A
    // tap anywhere in it, the preset glyph included, opens the device list
    // (`onTitleAreaClicked` `0x4be624` → `0x4bdb8c` → `0x391fb4`). The page tabs clip it.
    l.control(
        Rect::new(name_box.x, top, 200.min(name_box.width), 60),
        Control::Param(ParamControl::ChangeModel),
        true,
        name.clone(),
        Look::Area,
    );
    if let Some(category) = category {
        l.text(
            Rect::new(name_box.x, header + 1, name_box.width, 18),
            category.label().to_uppercase(),
            CAPTION,
            p.muted,
            Align::Left,
        );
    }
    // A capture's title takes the colour of its device type (shot sn-7: a compressor capture
    // in green), as its Grid block does.
    let title_hue = match category {
        Some(ModelCategory::NeuralCapture) => (s.blocks[position.index()])
            .hue(s.captures[position.index()])
            .unwrap_or(hue),
        _ => hue,
    };
    model_name(l, name_box, &name, title_hue, s.editor_preset.is_some());
    if stomp_button {
        let button = Rect::new(428, header, 66, 44);
        l.panel(button, p.tone(TAB_IDLE), 8.0, None);
        let assigned = s.selected_stomp.map(|tile| char::from(b'A' + tile));
        l.control(
            button,
            Control::Param(ParamControl::Stomp),
            true,
            "FOOTSWITCH ASSIGNMENT",
            Look::Area,
        );
        l.push(
            Rect::new(button.x + 6, button.y, 30, 44),
            "",
            Look::Glyph {
                icon: Icon::Footswitch,
                size: IconSize::Medium,
                color: p.text,
            },
        );
        l.text(
            Rect::new(button.x + 36, button.y, 24, 44),
            assigned.map_or_else(|| "?".to_owned(), String::from),
            MEDIUM_BOLD,
            if assigned.is_some() { p.text } else { p.muted },
            Align::Center,
        );
    }
    let scenes = Rect::new(502, header, 132, 44);
    l.panel(scenes, p.tone(TAB_IDLE), 8.0, None);
    super::scene_stepper(ui, l, scenes, 24);
    // Stock 2 px divider (`b4-stock1`, 22 px tall) between the scene selector and bypass.
    l.push(Rect::new(642, header + 11, 2, 22), "", Look::Fill(p.panel));
    let bypass = Rect::new(652, header, 66, 44);
    l.panel(bypass, p.tone(TAB_IDLE), 8.0, None);
    l.control(
        bypass,
        Control::Param(ParamControl::Bypass),
        true,
        if cell.bypassed { "BYPASSED" } else { "ON" },
        Look::Area,
    );
    if cell.bypassed {
        bypassed_glyph(l, bypass, title_hue);
    } else {
        l.push(
            bypass,
            "",
            Look::Glyph {
                icon: Icon::Power,
                size: IconSize::Medium,
                color: p.text,
            },
        );
    }
    // Stock ✓ is a 20 px glyph (16 × 12 measured), not the 24 px button glyph.
    let close = Rect::new(726, header, 66, 44);
    l.panel(close, p.panel, 8.0, None);
    l.control(
        close,
        Control::Param(ParamControl::Close),
        true,
        "CLOSE",
        Look::Area,
    );
    l.push(
        close,
        "",
        Look::Glyph {
            icon: Icon::Check,
            size: IconSize::Compact,
            color: p.text,
        },
    );
    if let Some(editor) = editor {
        super::model_editor::body(ui, l, editor, hue);
        return;
    }
    let card = Rect::new(
        8,
        header + 52,
        WIDTH - 16,
        (HEIGHT as i32 - 8 - header - 52) as u32,
    );
    if s.parameters.is_empty() {
        l.panel(card, p.panel, 8.0, None);
        l.text(
            card,
            "No parameters from the application",
            BODY,
            p.faint,
            Align::Center,
        );
        return;
    }
    let first = (ui.param_page * KNOB_COUNT).min(s.parameters.len());
    crate::widgets::parameter_panel(
        l,
        card,
        &s.parameters[first..],
        hue,
        &s.expression,
        ui.focus,
    );
    // Stock `0x4b5918`: a bypassed block's page tabs and parameter cells are drawn at
    // opacity 0.3 (`0x3e99999a`), the header keys at full colour (`sl-1`). Qt paints each
    // item at 0.3 over the one under it in its RGB565 surface, so the cell content goes over
    // the already faded card ([`Look::Fade`]); `sl-1` reads exactly on the card, knob face,
    // needle and arc. The tabs sit on the page: a veil fits them.
    if cell.bypassed {
        let tabs = Rect::new(name_right, header, (tabs_right - name_right) as u32, 52);
        let veil = Look::Veil {
            color: p.tone(BYPASS_VEIL_COLOUR),
            alpha: BYPASS_VEIL,
        };
        l.push(tabs, "", veil);
        let fade = Look::Fade {
            card: p.tone(CARD),
            faded: p.tone(BYPASS_CARD),
            keep: BYPASS_KEEP,
        };
        l.push(card, "", fade);
    }
}

/// A bypassed block's card as `sl-1` reads it on the device (RGB565, `#101410`).
const BYPASS_CARD: Rgb = Rgb(0x10, 0x14, 0x10);
/// Qt's 0.3 opacity as its blend alpha, `int(0.3 · 255)` (fitted to `sl-1`: knob face
/// `#101810`, needle `#525952`, arc `#522c08`).
const BYPASS_KEEP: u8 = 76;

/// Colour of the veil over a bypassed block's dimmed parameters. Fitted, not sampled: with
/// [`BYPASS_VEIL`] it reads back as `sl-1` on the device (card `#101410`, white text `#525552`,
/// divider `#101010`). Stock blends in RGB565, so no 8-bit veil of the page colour is exact.
const BYPASS_VEIL_COLOUR: Rgb = Rgb(0x0f, 0x0f, 0x0f);
/// Share of [`BYPASS_VEIL_COLOUR`] in a bypassed block's dimmed parameters: 182/255, so each item
/// keeps 73/255 of its own colour. The green is RGB565 truncation: green keeps 6 bits and red and
/// blue keep 5, so a grey near 23 reads back as red 16 and green 20.
const BYPASS_VEIL: u8 = 182;

/// The stock bypass key glyph of a bypassed block (`0x006869ec`, the `+0x1e8` pixmap,
/// `sl-1` at 673, 214): a 2 px rounded square (18 × 18, radius 4) in the block colour, its
/// sides cut at y 10–13 of the 24 px box, crossed by a 2 px white bar the box's width.
fn bypassed_glyph(l: &mut Layout, key: Rect, hue: Rgb) {
    let p = l.palette;
    let (x, y) = (
        key.x + (key.width as i32 - 24) / 2,
        key.y + (key.height as i32 - 24) / 2,
    );
    l.panel(Rect::new(x + 3, y + 3, 18, 18), hue, 4.0, None);
    let idle = p.tone(TAB_IDLE);
    l.panel(Rect::new(x + 5, y + 5, 14, 14), idle, 2.0, None);
    l.push(Rect::new(x + 3, y + 10, 18, 4), "", Look::Fill(idle));
    l.push(Rect::new(x, y + 11, 24, 2), "", Look::Fill(p.text));
}

/// The editor's `zenUI::TabBar` (`0x4ba858`, paint `0x347d10`), right-aligned to `right`:
/// tabs "1", "2", … (generic editors have no page names, `0x6117e8`), each
/// max(66, text + 16) wide with 4 px gaps. The selected tab is the card colour and runs down
/// into the card; the others are 44 px `#1e1e1e` with grey text. Hidden below two pages
/// (`0x4b8268`). Returns the left edge of the bar (`right` when hidden).
fn tab_bar(ui: &UiState, l: &mut Layout, right: i32, header: i32) -> i32 {
    let p = l.palette;
    let pages = ui.last_param_page() + 1;
    if pages < 2 {
        return right;
    }
    let labels: Vec<String> = (1..=pages).map(|page| page.to_string()).collect();
    let widths: Vec<u32> = labels
        .iter()
        .map(|label| (BODY.width(label).ceil() as u32 + 16).max(66))
        .collect();
    let total = widths.iter().sum::<u32>() as i32 + 4 * (pages as i32 - 1);
    let mut x = right - total;
    let left = x;
    for (page, (label, width)) in labels.into_iter().zip(widths).enumerate() {
        let selected = page == ui.param_page;
        let tab = Rect::new(x, header, width, 44);
        if selected {
            // Top corners rounded, square into the card below.
            let card = p.tone(CARD);
            l.panel(Rect::new(x, header, width, 52), card, 8.0, None);
            l.push(Rect::new(x, header + 30, width, 22), "", Look::Fill(card));
        } else {
            l.panel(tab, p.surface, 8.0, None);
        }
        l.control(
            tab,
            Control::Param(ParamControl::Page(page as u8)),
            true,
            format!("PAGE {label}"),
            Look::Area,
        );
        l.text(
            tab,
            label,
            BODY,
            if selected { p.text } else { p.muted },
            Align::Center,
        );
        x += width as i32 + 4;
    }
    left
}

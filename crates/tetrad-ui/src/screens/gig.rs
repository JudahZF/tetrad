//! Gig View (manual §3, spec `w4-ui-spec-quickstart` §12): a slim header (preset title, mode
//! icon button, scene badge, ✓), the live tuner bar and eight 190×194 footswitch tiles from
//! y 76. A–D is the upper row and E–H the lower row in every mode; a hybrid mode gives its
//! first kind the upper row (stock `0x678304`, `0x65dbcc`). Stock row pitch is 210 (202 when
//! app flag `+0x19c` is set); Tetrad uses 202 so the lower row ends at y 472.
//!
//! Renderer limits: stock icons are bitmaps; Tetrad draws the shared owned icons (edit, swap,
//! copy, mode marks, block glyphs). The scene watermark is the largest face (96 px) instead of
//! ~190 px.

use tetrad_assets::{Icon, IconSize, Palette, Rgb, SWATCHES, TUNER_IN, TextSize, Weight};

use super::grid::{capture_badge, preset_title, scene_title};
use crate::layout::{Layout, Look, SMALL_BOLD, XLARGE_BOLD};
use crate::render::{Align, Font, wrap};
use crate::*;

const TILE_WIDTH: u32 = 190;
const TILE_HEIGHT: u32 = 194;
/// Gig title (32 px Regular, tokens §3).
const TITLE: Font = Font::new(TextSize::Px32, Weight::Regular);
/// Preset id "1A" and the stomp switch letter (40 px Regular).
const SLOT_ID: Font = Font::new(TextSize::Px40, Weight::Regular);
const WATERMARK: Font = Font::new(TextSize::Px96, Weight::Regular);
/// Preset id letter colours A–H (shot 063): green, blue, orange, magenta, light blue, violet,
/// yellow, red, taken from the stock swatch palette.
pub(super) const SLOT_COLOURS: [Rgb; FOOTSWITCH_COUNT] = [
    SWATCHES[8],
    SWATCHES[3],
    SWATCHES[4],
    SWATCHES[6],
    SWATCHES[7],
    SWATCHES[1],
    SWATCHES[2],
    SWATCHES[0],
];
/// Tuner tolerance shared with the Tuner screen.
const IN_TUNE_CENTS: f32 = 3.0;
/// Width of the sliding pitch segment in the live tuner bar.
const TUNER_SEGMENT: u32 = 171;

pub(super) fn layout(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    header(ui, l);
    tuner_bar(&ui.snapshot.tuner, l);
    for (tile, switch) in Footswitch::TILES.into_iter().enumerate() {
        let bounds = Rect::new(
            8 + (tile % 4) as i32 * 198,
            76 + (tile / 4) as i32 * 202,
            TILE_WIDTH,
            TILE_HEIGHT,
        );
        match ui.tile_mode(tile) {
            FootswitchMode::Scene => scene_tile(ui, l, bounds, tile),
            FootswitchMode::Stomp => stomp_tile(ui, l, bounds, tile, switch),
            FootswitchMode::Preset => preset_tile(ui, l, bounds, tile),
            FootswitchMode::Hybrid => {
                l.panel(bounds, p.surface, 8.0, None);
                l.control(bounds, Control::Tile(switch), false, "-", Look::Area);
            }
        }
    }
}

fn header(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let s = &ui.snapshot;
    // Stock 105/019: the mode button ends at x 633; hybrid widens it to 98 px.
    let mode = match s.footswitch_mode {
        Some(FootswitchMode::Hybrid) => Rect::new(536, 8, 98, 44),
        _ => Rect::new(568, 8, 66, 44),
    };
    preset_title(
        l,
        s,
        Rect::new(16, 8, (mode.x - 28) as u32, 44),
        TITLE,
        false,
    );
    l.panel(mode, p.surface, 8.0, None);
    let label = s.footswitch_mode.map_or_else(
        || format!("MODE {}", s.mode_id),
        |mode| mode.label().to_owned(),
    );
    l.control(mode, Control::CycleMode, true, label, Look::Area);
    match s.footswitch_mode {
        Some(FootswitchMode::Hybrid) => {
            mode_glyph(l, s.hybrid.upper, mode.x + 13, mode.y + 9);
            // A thin 16 px grey "+" (019: x 577..592, 2 px strokes, `#777777`).
            let plus = p.muted.mix(p.surface, 64);
            let (cx, cy) = ((mode.x + 49) as f32, (mode.y + 21) as f32);
            l.line((cx - 8.0, cy), (cx + 8.0, cy), 2.0, plus);
            l.line((cx, cy - 8.0), (cx, cy + 8.0), 2.0, plus);
            mode_glyph(l, s.hybrid.lower, mode.x + 61, mode.y + 9);
        }
        Some(single) => mode_glyph(l, single, mode.x + 21, mode.y + 9),
        None => {}
    }
    // Scene badge (display only; the scenes dropdown is the Grid's).
    let badge = Rect::new(652, 18, 24, 24);
    l.panel(badge, p.warning, 4.0, None);
    l.text(
        badge,
        s.selected_scene.letter().to_string(),
        SMALL_BOLD,
        p.on_accent,
        Align::Center,
    );
    l.control(
        Rect::new(694, 8, 98, 44),
        Control::Navigate(Screen::Grid),
        true,
        "CLOSE",
        Look::IconButton {
            icon: Icon::Check,
            fill: p.panel,
        },
    );
}

/// Live tuner bar `8,60,784,8`: the track alone without a note; with one, a white segment
/// slides with the detune, and the whole bar turns green in tune (spec §5).
fn tuner_bar(tuner: &TunerView, l: &mut Layout) {
    let p = l.palette;
    let bar = Rect::new(8, 60, 784, 8);
    let in_tune = tuner.note.is_some() && tuner.cents.abs() <= IN_TUNE_CENTS;
    l.panel(bar, if in_tune { TUNER_IN } else { p.surface }, 4.0, None);
    if tuner.note.is_some() && !in_tune {
        let travel = (bar.width - TUNER_SEGMENT) as f32;
        let x = bar.x + ((tuner.cents.clamp(-50.0, 50.0) / 100.0 + 0.5) * travel) as i32;
        let segment = Rect::new(x, bar.y, TUNER_SEGMENT, bar.height);
        l.panel(segment, p.text, 4.0, None);
    }
}

/// Scene tile: EDIT SCENE, SWAP SCENE and COPY SCENE over a faded scene letter and the
/// centred scene name. The playing scene fills with its colour; the others are tinted.
fn scene_tile(ui: &UiState, l: &mut Layout, bounds: Rect, tile: usize) {
    let p = l.palette;
    let s = &ui.snapshot;
    let scene = SceneIndex(tile as u8);
    let view = &s.scenes[tile];
    let active = s.selected_scene == scene;
    let enabled = !s.bank_preview_active;
    // A scene colour is data: on it, the ink is stock's, so it reads as stock draws it.
    let (hue, on_hue) = match scene_hue(view.raw_color) {
        Some(hue) => (hue, Palette::STOCK.on_accent),
        None => (p.accent, p.on_accent),
    };
    let fill = if active { hue } else { hue.mix(p.canvas, 214) };
    l.panel(bounds, fill, 8.0, None);
    // The tool row sits above the scene's own touch area.
    l.control(
        Rect::new(bounds.x, bounds.y + 52, bounds.width, bounds.height - 52),
        Control::SelectScene(scene),
        enabled,
        scene_title(scene, view),
        Look::Area,
    );
    l.text(
        Rect::new(bounds.x, bounds.y + 40, bounds.width, 120),
        scene.letter().to_string(),
        WATERMARK,
        if active {
            hue.mix(p.canvas, 40)
        } else {
            hue.mix(p.surface, 150)
        },
        Align::Center,
    );
    let ink = if active { on_hue } else { p.text };
    centred_name(l, bounds, &view.label, ink);
    let marks = if active { on_hue.mix(hue, 60) } else { p.muted };
    let tools = [
        (SceneTool::Edit, Icon::Edit, "EDIT SCENE"),
        (SceneTool::Swap, Icon::Swap, "SWAP SCENE"),
        (SceneTool::Copy, Icon::Copy, "COPY SCENE"),
    ];
    for (index, (tool, icon, label)) in tools.into_iter().enumerate() {
        // The playing scene's EDIT is the scene dialog's own entry (same draft).
        let control = if active && tool == SceneTool::Edit {
            Control::EditScenes
        } else {
            Control::SceneTool { scene, tool }
        };
        // Icon centres at +32, +96, +160 (spec §12).
        let x = bounds.x + 8 + index as i32 * 64;
        l.control(
            Rect::new(x, bounds.y + 4, 48, 44),
            control,
            enabled,
            label,
            Look::Area,
        );
        let mark = Rect::new(x + 12, bounds.y + 14, 24, 24);
        glyph(l, mark, icon, IconSize::Medium, marks);
    }
}

/// Glyph of a stomp, drawn at 32 px as stock (shot 103: tile G 31×31 px of ink, tile C
/// 28×26). A switch on several blocks shows `multiple_stomps.png` (`Icon::MultipleDevices`).
/// Otherwise it shows the first block's Grid icon (model and capture type, `BlockInfo::icon`),
/// so both screens show the same glyph. A stomp without a grid position falls back to its
/// category.
fn stomp_glyph(s: &UiSnapshot, stomp: &StompView) -> Option<Icon> {
    if stomp.blocks.len() > 1 {
        return Some(Icon::MultipleDevices);
    }
    match stomp.position {
        Some(position) => s.blocks[position.index()].icon(s.captures[position.index()]),
        None => stomp.category.map(ModelCategory::icon),
    }
}

/// Colour of a stomp: the first block's Grid colour (`BlockInfo::hue`; stock tints a pedal
/// capture orange, manual shot 103), else its category's.
fn stomp_hue(s: &UiSnapshot, stomp: &StompView) -> Option<Rgb> {
    match stomp.position {
        Some(position) => s.blocks[position.index()].hue(s.captures[position.index()]),
        None => stomp.category.map(ModelCategory::hue),
    }
}

/// Stomp tile: block icon box (struck through when bypassed; a tap opens the first
/// block's editor), EDIT STOMP (name, Latching/Momentary, blocks), switch letter and the block name at the bottom. An engaged stomp fills
/// with its block colour ([`stomp_hue`]). The capture badge of the first block sits on the icon
/// box ([`capture_badge`]).
fn stomp_tile(ui: &UiState, l: &mut Layout, bounds: Rect, tile: usize, switch: Footswitch) {
    let p = l.palette;
    let s = &ui.snapshot;
    let stomp = &s.stomps[tile];
    let assigned = !stomp.label.is_empty() || stomp.category.is_some();
    // A block colour is data: on it, the ink is stock's, so it reads as stock draws it.
    let (hue, on_hue) = match stomp_hue(s, stomp) {
        Some(hue) => (hue, Palette::STOCK.on_accent),
        None => (p.muted, p.on_accent),
    };
    let engaged = assigned && stomp.engaged;
    let fill = match (assigned, engaged) {
        (true, true) => hue,
        (true, false) => hue.mix(p.surface, 228),
        (false, _) => p.surface,
    };
    l.panel(bounds, fill, 8.0, None);
    l.control(
        bounds,
        Control::Tile(switch),
        assigned && !s.bank_preview_active,
        stomp.label.clone(),
        Look::Area,
    );
    let ink = if engaged { on_hue } else { p.text };
    let marks = if engaged {
        on_hue.mix(hue, 60)
    } else {
        p.muted
    };
    if assigned {
        // Stock 019: a 70 px box with a white glyph. Engaged it is black with a 2 px ring
        // in the block colour 3 px inside (`0x66159c`; black for a Looper, `0x66252c`);
        // bypassed it darkens the tile and holds a 64 px tile with a 2 px border
        // at 40 % hue on a 3 % hue fill, struck through in grey.
        let icon = Rect::new(bounds.x + 12, bounds.y + 12, 70, 70);
        if engaged {
            l.panel(icon, p.canvas, 14.0, None);
            if stomp.category != Some(ModelCategory::Looper) {
                l.panel(icon.inset(3, 3), hue, 14.0, None);
                l.panel(icon.inset(5, 5), p.canvas, 12.0, None);
            }
        } else {
            let tile = icon.inset(3, 3);
            l.panel(icon, fill.mix(p.canvas, 153), 14.0, None);
            l.panel(tile, hue.mix(p.canvas, 153), 12.0, None);
            l.panel(tile.inset(2, 2), hue.mix(p.canvas, 247), 10.0, None);
        }
        if let Some(glyph_icon) = stomp_glyph(s, stomp) {
            glyph(l, icon, glyph_icon, IconSize::Large, p.text);
        }
        if let Some(position) = stomp.position {
            let index = position.index();
            if s.blocks[index].capture_badge(s.captures[index]) {
                capture_badge(l, icon, p.text);
            }
        }
        if !engaged {
            let y = (icon.y + 35) as f32;
            let from = (icon.x as f32 - 3.0, y);
            l.line(
                from,
                (icon.right() as f32 + 3.0, y),
                2.0,
                p.text.mix(p.canvas, 60),
            );
        }
        // Manual §3 PARAMETER EDITOR SHORTCUT: the block icon opens its editor.
        if let Some(position) = stomp.position {
            l.control(
                icon,
                Control::EditBlock(position),
                !s.bank_preview_active,
                "PARAMETER EDITOR",
                Look::Area,
            );
        }
    }
    let edit = Rect::new(bounds.x + 92, bounds.y + 6, 48, 44);
    l.control(
        edit,
        Control::Stomp(StompEdit::Open(tile as u8)),
        !s.bank_preview_active,
        "EDIT STOMP",
        Look::Area,
    );
    let mark = Rect::new(edit.x + 12, edit.y + 12, 24, 24);
    glyph(l, mark, Icon::Edit, IconSize::Medium, marks);
    l.text(
        Rect::new(bounds.right() - 48, bounds.y + 8, 40, 48),
        char::from(b'A' + tile as u8).to_string(),
        SLOT_ID,
        marks,
        Align::Right,
    );
    let name = if stomp.multiple {
        "Multiple devices".to_owned()
    } else if assigned {
        stomp.label.clone()
    } else {
        "-".to_owned()
    };
    let lines = wrap(&name, XLARGE_BOLD, (bounds.width - 32) as f32)
        .len()
        .min(2) as i32;
    let line = XLARGE_BOLD.size.line_px() as i32;
    l.push(
        Rect::new(
            bounds.x + 16,
            bounds.bottom() - 12 - lines * line,
            bounds.width - 24,
            (lines * line) as u32,
        ),
        name,
        Look::Paragraph {
            font: XLARGE_BOLD,
            color: if assigned { ink } else { p.muted },
            align: Align::Left,
        },
    );
}

/// Preset tile: "1A" (bank digit, letter in its slot colour) over the preset name. The
/// loaded preset fills green. A hybrid preset half shows the bank's first four presets.
fn preset_tile(ui: &UiState, l: &mut Layout, bounds: Rect, tile: usize) {
    let p = l.palette;
    let s = &ui.snapshot;
    let preset = s.bank.presets.get(ui.preset_offset(tile));
    let active = preset.is_some_and(|preset| s.preset_id == Some(preset.id))
        || s.bank.highlighted == Some(ui.preset_offset(tile) as u8);
    l.panel(bounds, if active { p.accent } else { p.surface }, 8.0, None);
    if let Some(preset) = preset {
        l.control(
            bounds,
            Control::SelectPreset(preset.id),
            true,
            preset.title.clone(),
            Look::Area,
        );
    }
    let ink = if active { p.on_accent } else { p.text };
    let bank = (u32::from(s.bank.index) + 1).to_string();
    let id = Rect::new(bounds.x + 16, bounds.y + 6, 120, 52);
    l.text(id, bank.clone(), SLOT_ID, ink, Align::Left);
    l.text(
        Rect::new(id.x + SLOT_ID.width(&bank) as i32, id.y, 60, id.height),
        char::from(b'A' + tile as u8).to_string(),
        SLOT_ID,
        if active {
            p.on_accent
        } else {
            SLOT_COLOURS[tile]
        },
        Align::Left,
    );
    l.push(
        Rect::new(bounds.x + 16, bounds.y + 64, bounds.width - 24, 124),
        preset.map_or_else(|| "-".to_owned(), |preset| preset.title.clone()),
        Look::Paragraph {
            font: XLARGE_BOLD,
            color: if preset.is_some() { ink } else { p.muted },
            align: Align::Left,
        },
    );
}

/// Scene name: bold, centred, up to two lines around y +100.
fn centred_name(l: &mut Layout, bounds: Rect, name: &str, color: Rgb) {
    let width = bounds.width - 20;
    let lines = wrap(name, XLARGE_BOLD, width as f32).len().clamp(1, 2) as i32;
    let line = XLARGE_BOLD.size.line_px() as i32;
    l.push(
        Rect::new(
            bounds.x + 10,
            bounds.y + 100 - lines * line / 2,
            width,
            (lines * line) as u32,
        ),
        name,
        Look::Paragraph {
            font: XLARGE_BOLD,
            color,
            align: Align::Center,
        },
    );
}

/// Stored scene colour, `0xAARRGGBB` as tetrad-preset writes it (`DEFAULT_SCENE_COLORS`); a
/// zero alpha means no colour.
fn scene_hue(raw: u32) -> Option<Rgb> {
    let [alpha, red, green, blue] = raw.to_be_bytes();
    (alpha != 0).then_some(Rgb(red, green, blue))
}

fn mode_glyph(l: &mut Layout, mode: FootswitchMode, x: i32, y: i32) {
    let p = l.palette;
    let icon = match mode {
        FootswitchMode::Preset => Icon::ModePreset,
        FootswitchMode::Scene => Icon::ModeScene,
        FootswitchMode::Stomp | FootswitchMode::Hybrid => Icon::Footswitch,
    };
    glyph(l, Rect::new(x, y, 24, 24), icon, IconSize::Medium, p.text);
}

fn glyph(l: &mut Layout, bounds: Rect, icon: Icon, size: IconSize, color: Rgb) {
    l.push(bounds, "", Look::Glyph { icon, size, color });
}

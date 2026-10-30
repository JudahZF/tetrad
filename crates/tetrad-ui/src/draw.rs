//! Paints display-list items with the canvas palette. Purely visual: no state
//! changes.

use std::sync::OnceLock;

use tetrad_assets::{Icon, IconSize, Palette, Rgb, TextSize, Weight};

use crate::layout::{
    BODY, BODY_BOLD, ButtonStyle, CAPTION, Item, LARGE_BOLD, Look, MEDIUM, SMALL_BOLD, TileMark,
};
use crate::render::{Align, Canvas, FULL, Font, RingPixel, cached_ring, fit, ring};
use crate::{Control, ParameterKind, Rect, SettingControl, SettingId};

/// Stock radius of cards, buttons and tiles.
pub(crate) const RADIUS: f32 = 8.0;
/// Block tile corner radius on the pen's centre line: stock `0x427db4` strokes the tile with
/// `drawRoundedRect(13, 13)` on the slot rect inset by 3 + pen / 2 (C). So the outer edge of a
/// `pen` px outline has radius `13 + pen / 2`: 14 for the 2 px ring, 16 for the 6 px ring.
const TILE_RADIUS: f32 = 13.0;

/// Outer radius of a `pen` px tile ring whose outer edge is the 64 px tile.
fn tile_ring_radius(pen: f32) -> f32 {
    TILE_RADIUS + pen / 2.0
}

/// Grid row I/O tile radius: the row item gives both port items 14.0 (`0x694c90`, vtable
/// `+0x4c`); the corners of `st-mp1a.png` fit 14.
pub(crate) const PORT_RADIUS: f32 = 14.0;
/// Port label lines: 22 px rows from tile y 16, 21 px apart. Stock cap tops at tile y 21 and
/// 42 (`st-mp1a` … `st-mp1h`: "In" y 130, "1" y 151 on a tile at y 109).
pub(crate) const PORT_LINE_TOP: i32 = 16;
pub(crate) const PORT_LINE_PITCH: i32 = 21;
/// A faded port (opacity 0.2) as the RGB565 screen shows it. Text: white × 0.2 reads
/// `#313031` (`st-mp1g.png`). The muted "+" pixmap reads `#181818` (`sl-2.png`): Qt blends
/// pixmaps into RGB16 with the opacity cut to 6/32.
const FADED_PORT_TEXT: Rgb = Rgb(0x33, 0x33, 0x33);
const FADED_PORT_PLUS: Rgb = Rgb(0x18, 0x18, 0x18);

/// A port label on two lines, split at its first space (stock "In" / "1", "Multi" / "Out").
fn port_label(canvas: &mut Canvas, b: Rect, label: &str, color: Rgb) {
    let (first, second) = label.split_once(' ').unwrap_or((label, ""));
    for (line, text) in [first, second].into_iter().enumerate() {
        let y = b.y + PORT_LINE_TOP + line as i32 * PORT_LINE_PITCH;
        let row = Rect::new(b.x, y, b.width, 22);
        canvas.text(row, text, BODY, color, Align::Center);
    }
}

/// Unselected tab fill: Global EQ band tabs (shot 009), Expression tabs (068), the Capture
/// save instrument chips and folder pill (030). Darker and greener than the stock surface `#1e1e1e`.
pub(crate) const TAB_IDLE: Rgb = Rgb(0x18, 0x1c, 0x18);
/// Fill of a selected tab (068 Expression, 030 metadata indicator; every
/// `ButtonStyle::Tab`). The shared accent fill `#17291a` stays on the picker Plugins
/// tile (076) and the other accent fills.
pub(crate) const TAB_ACTIVE: Rgb = Rgb(0x10, 0x28, 0x18);

/// Stock button labels are Regular: 18 px in 44 px buttons, 14 px in small
/// ones (shots 095 CONNECTION DIAGRAM, 068 ASSIGN, 008 MUTE).
fn button_font(bounds: Rect) -> Font {
    if bounds.height >= 44 { MEDIUM } else { CAPTION }
}

/// Knob track (r 28.5–32.5) and value arc (r 30.5–34.5): stock draws the arc
/// 2 px outside the track, so the track shows inside it (shots 008, 009).
fn knob_rings() -> &'static (Vec<RingPixel>, Vec<RingPixel>) {
    static RINGS: OnceLock<(Vec<RingPixel>, Vec<RingPixel>)> = OnceLock::new();
    RINGS.get_or_init(|| (ring(30.5, 4.0), ring(32.5, 4.0)))
}

/// Whether `item` draws in its pressed state.
/// A [`Look::Dismiss`] area draws nothing, so its press is never shown (and never damages the
/// frame: it covers the whole screen around a popover).
pub(crate) fn is_pressed(item: &Item, pressed: Option<Control>) -> bool {
    item.enabled
        && item.control.is_some()
        && item.control == pressed
        && !matches!(item.look, Look::Dismiss)
}

pub(crate) fn item(canvas: &mut Canvas, item: &Item, pressed: Option<Control>) {
    paint_item(canvas, item, is_pressed(item, pressed));
}

pub(crate) fn paint_item(canvas: &mut Canvas, item: &Item, pressed: bool) {
    let draw = |canvas: &mut Canvas| paint(canvas, item, pressed);
    match item.clip {
        Some(clip) => canvas.clipped(clip, draw),
        None => draw(canvas),
    }
}

/// Pixels an item can touch: its bounds plus the widest overhang (selection
/// outlines grow 3 px, text clips 4 px past its box), inside its viewport and
/// the frame. A scrim dims the whole frame.
pub(crate) fn extent(item: &Item) -> Option<Rect> {
    let rect = match item.look {
        Look::Scrim => FULL,
        _ => item.bounds.grow(4, 4),
    };
    let rect = match item.clip {
        Some(clip) => rect.intersect(clip)?,
        None => rect,
    };
    rect.intersect(FULL)
}

/// Repaint `rect` from `items` (each with its pressed state), pixel-equal to clearing it and
/// painting every item. Opaque `Fill` items hide what lies under them: the paint starts at
/// the last fills that cover `rect` ([`opaque_cover`]; no clear then), and a large item is
/// clipped to the part that later fills leave open ([`uncovered`]).
pub(crate) fn paint_region<T>(
    canvas: &mut Canvas,
    items: &[T],
    rect: Rect,
    item_of: impl Fn(&T) -> (&Item, bool),
) {
    /// Items from this area up are worth the scan of the later items.
    const LARGE: u64 = 24_000;
    let p = canvas.palette;
    canvas.clipped(rect, |canvas| {
        let cover = opaque_cover(items.iter().map(|entry| item_of(entry).0), rect);
        if cover.is_none() {
            canvas.fill(rect, p.background);
        }
        let start = cover.unwrap_or(0);
        for (index, entry) in items.iter().enumerate().skip(start) {
            let (item, pressed) = item_of(entry);
            let Some(area) = extent(item).and_then(|extent| extent.intersect(rect)) else {
                continue;
            };
            if u64::from(area.width) * u64::from(area.height) < LARGE {
                paint_item(canvas, item, pressed);
                continue;
            }
            let later = items[index + 1..].iter().map(|entry| item_of(entry).0);
            if let Some(open) = uncovered(later, area) {
                canvas.clipped(open, |canvas| paint_item(canvas, item, pressed));
            }
        }
    });
}

/// An index from which `Fill` items alone paint every pixel of `rect` (the Grid's black fill;
/// the capture browser's rail and panel together). Items before it never show inside `rect`,
/// nor does the frame clear, so a redraw of `rect` can start there. `None`: clear and paint
/// every item.
pub(crate) fn opaque_cover<'a>(
    items: impl DoubleEndedIterator<Item = &'a Item> + ExactSizeIterator,
    rect: Rect,
) -> Option<usize> {
    let mut open = rect;
    for (index, item) in items.enumerate().rev() {
        open = match shrink(open, item) {
            Some(open) => open,
            None => return Some(index),
        };
    }
    None
}

/// The part of `rect` that the `Fill` items of `later` leave open (one rectangle, maybe
/// larger than the true open part); `None` when they cover all of it.
pub(crate) fn uncovered<'a>(
    later: impl DoubleEndedIterator<Item = &'a Item>,
    rect: Rect,
) -> Option<Rect> {
    later.rev().try_fold(rect, shrink)
}

/// `open` less `item` when it is a `Fill` that covers all of `open` (`None`) or a whole side
/// of it; otherwise `open` unchanged. The open part so stays one rectangle (cheap, and never
/// wrong: a fill left out only keeps more of `open`).
fn shrink(open: Rect, item: &Item) -> Option<Rect> {
    let (Look::Fill(_), Some(fill)) = (&item.look, item.visible()) else {
        return Some(open);
    };
    let Some(hit) = open.intersect(fill) else {
        return Some(open);
    };
    let (x0, y0, x1, y1) = (open.x, open.y, open.right(), open.bottom());
    let spans_rows = hit.y == y0 && hit.bottom() == y1;
    let spans_columns = hit.x == x0 && hit.right() == x1;
    Some(match (spans_rows, spans_columns) {
        (true, true) => return None,
        (true, false) if hit.x == x0 => {
            Rect::new(hit.right(), y0, (x1 - hit.right()) as u32, open.height)
        }
        (true, false) if hit.right() == x1 => Rect::new(x0, y0, (hit.x - x0) as u32, open.height),
        (false, true) if hit.y == y0 => {
            Rect::new(x0, hit.bottom(), open.width, (y1 - hit.bottom()) as u32)
        }
        (false, true) if hit.bottom() == y1 => Rect::new(x0, y0, open.width, (hit.y - y0) as u32),
        _ => open,
    })
}

/// A [`Look::HexNut`]: per row of the two apothems, a 1 px fill whose half width shrinks
/// from the circumradius by `|y + 0.5| / √3`, rounded.
fn hex_nut(canvas: &mut Canvas, b: Rect, color: Rgb) {
    let (radius, apothem) = (b.width as i32 / 2, b.height as i32 / 2);
    let (cx, cy) = (b.x + radius, b.y + apothem);
    for y in -apothem..apothem {
        let half = (radius as f32 - (y as f32 + 0.5).abs() / 3f32.sqrt()).round() as i32;
        canvas.fill(Rect::new(cx - half, cy + y, half as u32 * 2, 1), color);
    }
}

/// Text colour for enabled or disabled content.
fn ink(p: &Palette, enabled: bool) -> Rgb {
    if enabled { p.text } else { p.faint }
}

fn paint(canvas: &mut Canvas, item: &Item, pressed: bool) {
    let p = canvas.palette;
    let b = item.bounds;
    let label = item.label.as_str();
    let enabled = item.enabled;
    match &item.look {
        Look::Fill(color) => canvas.fill(b, *color),
        Look::Panel {
            fill,
            radius,
            border,
        } => {
            canvas.round_rect(b, *radius, *fill);
            if let Some(border) = border {
                canvas.round_outline(b, *radius, 1.0, *border);
            }
        }
        Look::Line {
            from,
            to,
            width,
            color,
        } => canvas.line(from.0, from.1, to.0, to.1, *width, *color),
        Look::Arc {
            width,
            start,
            sweep,
            color,
        } => {
            let center = (b.x + b.width as i32 / 2, b.y + b.height as i32 / 2);
            let pixels = cached_ring(b.width as f32 / 2.0 - width / 2.0, *width);
            canvas.arc(center, &pixels, *start, *sweep, *color);
        }
        Look::Dot { color } => {
            let (cx, cy) = b.center();
            canvas.circle(cx as f32, cy as f32, b.width as f32 / 2.0, *color);
        }
        Look::HexNut { color } => hex_nut(canvas, b, *color),
        Look::Text { font, color, align } => canvas.text(b, label, *font, *color, *align),
        Look::Paragraph { font, color, align } => {
            canvas.paragraph(b, label, *font, *color, *align);
        }
        Look::Button { style, icon, font } => {
            let font = font.unwrap_or_else(|| button_font(b));
            button(canvas, b, (label, font), *style, *icon, enabled, pressed);
        }
        Look::IconButton { icon, fill } => {
            canvas.round_rect(
                b,
                RADIUS,
                if pressed { fill.mix(p.text, 40) } else { *fill },
            );
            canvas.icon_in(b, *icon, IconSize::Medium, ink(&p, enabled));
        }
        Look::Dropdown { value } => dropdown(canvas, b, value, None, enabled, pressed),
        Look::ListRow {
            icon,
            value,
            selected,
            divider,
            menu,
            font,
        } => list_row(
            canvas,
            b,
            (label, *font),
            *icon,
            value,
            (*selected, *divider, *menu),
            enabled,
            pressed,
        ),
        Look::Row {
            hue,
            icon,
            value,
            chevron,
            selected,
        } => {
            let fill = if *selected || pressed {
                p.surface
            } else {
                p.panel
            };
            canvas.round_rect(b, RADIUS, fill);
            let text = match (*selected, enabled) {
                (_, false) => p.faint,
                (true, true) => p.accent,
                (false, true) => p.text,
            };
            let mut x = b.x + 14;
            if let Some(hue) = hue {
                canvas.circle(
                    x as f32 + 5.0,
                    b.y as f32 + b.height as f32 / 2.0,
                    5.0,
                    *hue,
                );
                x += 20;
            }
            if let Some(icon) = icon {
                let size = IconSize::Medium;
                canvas.icon(
                    *icon,
                    size,
                    x,
                    b.y + (b.height as i32 - size.px() as i32) / 2,
                    text,
                );
                x += size.px() as i32 + 14;
            }
            let mut right = b.right() - 12;
            if *chevron {
                let size = IconSize::Small;
                canvas.icon(
                    Icon::Forward,
                    size,
                    right - size.px() as i32,
                    b.y + (b.height as i32 - size.px() as i32) / 2,
                    p.faint,
                );
                right -= size.px() as i32 + 8;
            }
            let value_width = trailing_value(canvas, b, right, value, enabled);
            let text_width = (right - value_width - 12 - x).max(0) as u32;
            canvas.text(
                Rect::new(x, b.y, text_width, b.height),
                label,
                BODY,
                text,
                Align::Left,
            );
        }
        Look::Switch { on } => switch(canvas, b, *on, enabled),
        Look::Block {
            hue,
            fill,
            icon,
            bypassed,
            selected,
            drop_target,
            ghost,
            capture_two,
        } => block(
            canvas,
            b,
            label,
            *hue,
            *fill,
            *icon,
            *bypassed,
            *selected,
            *drop_target,
            *ghost,
            *capture_two,
            pressed,
        ),
        Look::Port { faded } => {
            // At opacity 0.2 the `#121212` tile is black on the RGB565 screen; only the
            // label or "+" shows (`st-mp1g.png`, `sl-2.png`).
            let (plus, text) = match faded {
                false => {
                    let fill = if pressed { p.surface } else { p.background };
                    canvas.round_rect(b, PORT_RADIUS, fill);
                    (p.muted, p.text)
                }
                true => (p.tone(FADED_PORT_PLUS), p.tone(FADED_PORT_TEXT)),
            };
            if label.is_empty() {
                // A 22 × 22 "+" of 2 px strokes, centred (`st-mp1a.png` x 19–40, y 230–251).
                let (cx, cy) = (b.x + b.width as i32 / 2, b.y + b.height as i32 / 2);
                canvas.fill(Rect::new(cx - 11, cy - 1, 22, 2), plus);
                canvas.fill(Rect::new(cx - 1, cy - 11, 2, 22), plus);
            } else {
                port_label(canvas, b, label, text);
            }
        }
        Look::Glyph { icon, size, color } => canvas.icon_in(b, *icon, *size, *color),
        Look::Knob {
            value,
            kind,
            color,
            focused,
        } => {
            // Stock 009: a Global EQ dial and value sit 2 px further below the label than
            // in the shared knob cell.
            let dial_dy = match item.control {
                Some(Control::Setting(SettingControl::Open(
                    SettingId::EqGain(_)
                    | SettingId::EqFreq(_)
                    | SettingId::EqQ(_)
                    | SettingId::EqOutput,
                ))) => 2,
                _ => 0,
            };
            knob(
                canvas, b, label, value, *kind, *color, *focused, pressed, enabled, dial_dy,
            )
        }
        Look::Progress { fraction, color } => {
            canvas.round_rect(b, b.height as f32 / 2.0, p.background);
            let filled = (b.width as f32 * fraction.clamp(0.0, 1.0)).round() as u32;
            if filled > 0 {
                canvas.round_rect(
                    Rect {
                        width: filled.max(b.height),
                        ..b
                    },
                    b.height as f32 / 2.0,
                    *color,
                );
            }
        }
        Look::Meter { level } => {
            canvas.round_rect(b, 2.0, p.background);
            let level = level.clamp(0.0, 1.0);
            let height = (b.height as f32 * level).round() as u32;
            let color = if level > 0.95 {
                p.danger
            } else if level > 0.75 {
                p.warning
            } else {
                p.accent
            };
            // A wide meter fills left to right (capture calibration, manual §6).
            let fill = if b.width > b.height {
                Rect::new(b.x, b.y, (b.width as f32 * level).round() as u32, b.height)
            } else {
                Rect::new(b.x, b.bottom() - height as i32, b.width, height)
            };
            canvas.round_rect(fill, 2.0, color);
        }
        // Stock `WiFi/N bars.png` traced on the QC (2026-10-16): six 2 px bars 4 px apart,
        // 3..18 px tall, bottoms at y 21 of the 24 px icon; lit white, the rest #1e1e1e.
        // I: index 0 (`disconnected.png`) is drawn as six unlit bars.
        Look::Signal { bars } => {
            for bar in 0..6u8 {
                let height = 3 * (u32::from(bar) + 1);
                canvas.fill(
                    Rect::new(
                        b.x + 3 + i32::from(bar) * 4,
                        b.y + 21 - height as i32,
                        2,
                        height,
                    ),
                    if bar < *bars { p.text } else { p.surface },
                );
            }
        }
        Look::Key { icon, accent } => {
            let fill = match (pressed, accent) {
                (true, _) => p.panel.mix(p.text, 60),
                (false, true) => p.accent,
                (false, false) => p.panel,
            };
            canvas.round_rect(b, 6.0, fill);
            let color = if *accent && !pressed {
                p.on_accent
            } else {
                ink(&p, enabled)
            };
            match icon {
                Some(icon) => canvas.icon_in(b, *icon, IconSize::Medium, color),
                None => canvas.text(
                    b,
                    label,
                    if label.chars().count() == 1 {
                        LARGE_BOLD
                    } else {
                        BODY_BOLD
                    },
                    color,
                    Align::Center,
                ),
            }
        }
        Look::DropZone { active } => {
            let fill = if *active {
                p.danger.mix(p.canvas, 140)
            } else {
                p.surface
            };
            canvas.round_rect(b, RADIUS, fill);
            canvas.round_outline(b, RADIUS, 1.5, p.danger);
            let size = IconSize::Medium;
            canvas.icon(
                Icon::Trash,
                size,
                b.x + b.width as i32 / 2 - 84,
                b.y + (b.height as i32 - size.px() as i32) / 2,
                p.danger,
            );
            canvas.text(b, label, BODY_BOLD, p.text, Align::Center);
        }
        Look::Area => {
            if pressed {
                canvas.round_rect(b, RADIUS, p.surface);
            }
        }
        Look::Dismiss => {}
        Look::Scrim => canvas.dim(96),
        Look::Veil { color, alpha } => canvas.tint(b, *color, *alpha),
        Look::Fade { card, faded, keep } => canvas.fade(b, *card, *faded, *keep),
        Look::Toast => {
            canvas.round_rect(b, b.height as f32 / 2.0, p.section);
            canvas.text(b.inset(16, 0), label, BODY, p.text, Align::Center);
        }
        Look::ActionTile {
            letter,
            title,
            mark,
            hue,
            active,
        } => {
            let fill = match (*active, pressed) {
                (_, true) => p.surface,
                (true, false) => hue.mix(p.canvas, 150),
                (false, false) => p.panel,
            };
            canvas.round_rect(b, RADIUS, fill);
            let ink = ink(&p, enabled);
            // Shot 086: 16 px Regular title, 40 px Regular letter 14 px from the right edge.
            canvas.text(
                Rect::new(b.x + 10, b.y + 3, b.width - 60, 26),
                title,
                BODY,
                ink,
                Align::Left,
            );
            let letter = letter.to_string();
            let x = b.right() as f32 - 14.0 - TILE_LETTER.width(&letter);
            canvas.text_at(x, b.y + 41, &letter, TILE_LETTER, ink);
            let color = if enabled { *hue } else { p.faint };
            let centre = (b.x + b.width as i32 / 2, b.y + 57);
            match mark {
                TileMark::Icon(icon) => tile_glyph(canvas, centre, *icon, color, fill),
                TileMark::Text(text) => {
                    let width = TILE_MARK.width(text);
                    let x = centre.0 as f32 - width / 2.0;
                    canvas.text_at(x, centre.1 + 15, text, TILE_MARK, color);
                }
            }
        }
    }
}

/// Looper X tile letter (tokens §3).
const TILE_LETTER: Font = Font::new(TextSize::Px40, Weight::Regular);
/// Looper X text marks ("1/2", the "1" of DUPLICATE): cap 28 px (shot 086).
const TILE_MARK: Font = Font::new(TextSize::Px40, Weight::Bold);

/// Filled convex polygon, anti-aliased by its signed distance (points clockwise, y down).
fn fill_polygon(canvas: &mut Canvas, points: &[(f32, f32)], color: Rgb) {
    let (mut left, mut top) = (f32::MAX, f32::MAX);
    let (mut right, mut bottom) = (f32::MIN, f32::MIN);
    for &(x, y) in points {
        (left, right) = (left.min(x), right.max(x));
        (top, bottom) = (top.min(y), bottom.max(y));
    }
    // Outward unit normal and offset of each edge.
    let edges: Vec<(f32, f32, f32)> = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .map(|(&(x0, y0), &(x1, y1))| {
            let length = (x1 - x0).hypot(y1 - y0);
            let (nx, ny) = ((y1 - y0) / length, (x0 - x1) / length);
            (nx, ny, nx * x0 + ny * y0)
        })
        .collect();
    for y in top.floor() as i32..bottom.ceil() as i32 {
        for x in left.floor() as i32..right.ceil() as i32 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let distance = edges
                .iter()
                .map(|(nx, ny, d)| nx * px + ny * py - d)
                .fold(f32::MIN, f32::max);
            let alpha = ((0.5 - distance).clamp(0.0, 1.0) * 255.0).round() as u8;
            if alpha > 0 {
                canvas.tint(Rect::new(x, y, 1, 1), color, alpha);
            }
        }
    }
}

/// Undo/redo ring of shot 086: radius 18.5, 8 px wide.
fn undo_ring() -> &'static [RingPixel] {
    static RING: OnceLock<Vec<RingPixel>> = OnceLock::new();
    RING.get_or_init(|| ring(18.5, 8.0))
}

/// Owned large Looper X tile glyphs measured on shot 086, centred on `(cx, cy)`. `fill` is
/// the tile fill, used to cut gaps. Other icons draw as the 32 px line icon.
fn tile_glyph(canvas: &mut Canvas, (cx, cy): (i32, i32), icon: Icon, color: Rgb, fill: Rgb) {
    let p = canvas.palette;
    let (x, y) = (cx as f32, cy as f32);
    match icon {
        Icon::Record => canvas.circle(x, y, 23.0, color),
        Icon::Play => fill_polygon(
            canvas,
            &[(x - 18.0, y - 20.5), (x + 17.5, y), (x - 18.0, y + 20.5)],
            color,
        ),
        Icon::Stop => canvas.round_rect(Rect::new(cx - 19, cy - 19, 38, 38), 3.0, color),
        // REVERSE ◀◀: two equal triangles 27 px apart, the right one cut 6 px clear of the left.
        Icon::Back => {
            fill_polygon(
                canvas,
                &[(x - 30.0, y), (x + 4.0, y - 22.5), (x + 4.0, y + 22.5)],
                color,
            );
            let cut = 22.5 * 14.0 / 34.0;
            fill_polygon(
                canvas,
                &[
                    (x + 11.0, y - cut),
                    (x + 31.0, y - 22.5),
                    (x + 31.0, y + 22.5),
                    (x + 11.0, y + cut),
                ],
                color,
            );
        }
        // ↺ from 9 o'clock round to 12 o'clock with a left-pointing head; REDO mirrors it.
        // The ring centre sits 2 px below the glyph centre.
        Icon::Undo | Icon::Redo => {
            let mirror = if icon == Icon::Redo { -1.0 } else { 1.0 };
            let start = if icon == Icon::Redo { 0.0 } else { 276.0 };
            canvas.arc((cx, cy + 2), undo_ring(), start, 264.0, color);
            let head = [(-12.5, -17.5), (3.0, -26.5), (3.0, -8.5)];
            let mut head = head.map(|(dx, dy)| (x + mirror * dx, y + dy));
            if mirror < 0.0 {
                head.reverse();
            }
            fill_polygon(canvas, &head, color);
        }
        // ONE SHOT: a 62 × 42 stadium, 8 px wide, open at the bottom left with a head there.
        Icon::Refresh => {
            canvas.round_outline(Rect::new(cx - 31, cy - 21, 62, 42), 21.0, 8.0, color);
            canvas.fill(Rect::new(cx - 12, cy + 10, 18, 12), fill);
            fill_polygon(
                canvas,
                &[(x - 8.0, y + 17.0), (x + 6.0, y + 8.0), (x + 6.0, y + 26.0)],
                color,
            );
        }
        // DUPLICATE: a disc with a dark "x", then a bold "1".
        Icon::Copy => {
            canvas.circle(x - 15.5, y, 16.0, color);
            for (dx0, dx1) in [(-21.5, -9.5), (-9.5, -21.5)] {
                canvas.line(x + dx0, y - 7.0, x + dx1, y + 7.0, 4.5, fill);
            }
            canvas.text_at(x + 8.0, cy + 15, "1", TILE_MARK, color);
        }
        // PUNCH IN: a boxing glove (body, thumb, cuff).
        Icon::Pedal => {
            canvas.round_rect(Rect::new(cx - 10, cy - 23, 26, 33), 9.0, color);
            canvas.round_rect(Rect::new(cx - 18, cy - 9, 8, 17), 4.0, color);
            canvas.line(x - 9.5, y - 6.0, x - 9.5, y + 7.0, 1.0, fill);
            canvas.fill(Rect::new(cx - 8, cy + 14, 22, 8), color);
            canvas.line(
                x - 3.0,
                y - 17.0,
                x + 9.0,
                y - 17.0,
                2.0,
                color.mix(p.text, 70),
            );
        }
        icon => canvas.icon_in(
            Rect::new(cx - 20, cy - 20, 40, 40),
            icon,
            IconSize::Large,
            color,
        ),
    }
}

/// Right-aligned muted value ending at `right`; returns its width.
fn trailing_value(canvas: &mut Canvas, b: Rect, right: i32, value: &str, enabled: bool) -> i32 {
    let p = canvas.palette;
    if value.is_empty() {
        return 0;
    }
    let width = (BODY.width(value).ceil() as i32).min((b.width / 2) as i32);
    canvas.text(
        Rect::new(right - width, b.y, width as u32, b.height),
        value,
        BODY,
        if enabled { p.muted } else { p.faint },
        Align::Right,
    );
    width
}

fn button(
    canvas: &mut Canvas,
    b: Rect,
    (label, font): (&str, Font),
    style: ButtonStyle,
    icon: Option<Icon>,
    enabled: bool,
    pressed: bool,
) {
    let p = canvas.palette;
    let selected_tab = style == ButtonStyle::Tab { selected: true };
    let (fill, text) = match (style, enabled) {
        (ButtonStyle::Ghost | ButtonStyle::Tab { selected: false }, false) => (None, p.faint),
        (ButtonStyle::Inset, false) => (Some(p.canvas), p.faint),
        (_, false) => (Some(p.surface), p.faint),
        (ButtonStyle::Primary, true) => (Some(p.action), p.text),
        (ButtonStyle::Secondary, true) => (Some(p.section), p.text),
        (ButtonStyle::Inset, true) => (Some(p.canvas), p.text),
        (ButtonStyle::Danger, true) => (Some(p.section), p.danger),
        (ButtonStyle::Ghost, true) => (None, p.text),
        (ButtonStyle::Tab { selected: true }, true) => (Some(p.tone(TAB_ACTIVE)), p.text),
        (ButtonStyle::Tab { selected: false }, true) => (Some(p.surface), p.muted),
    };
    let fill = match (fill, pressed) {
        (Some(color), true) => Some(color.mix(p.text, 40)),
        (None, true) => Some(p.surface),
        (fill, false) => fill,
    };
    if let Some(fill) = fill {
        canvas.round_rect(b, RADIUS, fill);
    }
    if selected_tab && enabled {
        canvas.round_outline(b, RADIUS, 1.0, p.accent);
    }
    // Narrow buttons cannot fit an icon and a label: show the icon only.
    match (
        icon,
        label.chars().count() <= 1 || style == ButtonStyle::Ghost || b.width < 80,
    ) {
        (Some(icon), true) => {
            let size = if b.height >= 40 {
                IconSize::Medium
            } else {
                IconSize::Small
            };
            canvas.icon_in(b, icon, size, text);
        }
        (Some(icon), false) => {
            let size = IconSize::Medium;
            let gap = size.px() as i32 + 8;
            let width = font.width(&fit(label, font, b.width as f32 - 48.0)) as i32 + gap;
            let x = b.x + (b.width as i32 - width) / 2;
            canvas.icon(
                icon,
                size,
                x,
                b.y + (b.height as i32 - size.px() as i32) / 2,
                text,
            );
            canvas.text(
                Rect::new(
                    x + gap,
                    b.y,
                    b.width.saturating_sub((x + gap - b.x) as u32 + 6),
                    b.height,
                ),
                label,
                font,
                text,
                Align::Left,
            );
        }
        (None, _) => canvas.text(b.inset(8, 0), label, font, text, Align::Center),
    }
}

/// Stock dropdown field: `#121212` box, value left, ▼ right (shot 008). Settings panes draw
/// their own black field (091). With a leading `icon` it is the compact black field with
/// 14 px text and a grey ▼ (Global EQ TYPE, shot 009).
fn dropdown(
    canvas: &mut Canvas,
    b: Rect,
    value: &str,
    icon: Option<Icon>,
    enabled: bool,
    pressed: bool,
) {
    let p = canvas.palette;
    let fill = if icon.is_some() {
        p.canvas
    } else {
        p.background
    };
    canvas.round_rect(b, 4.0, if pressed { fill.mix(p.text, 30) } else { fill });
    let arrow = Rect::new(b.right() - 24, b.y + (b.height as i32 - 7) / 2, 12, 7);
    let mut x = b.x + 8;
    let font = match icon {
        Some(icon) => {
            canvas.triangle_down(arrow, if enabled { p.muted } else { p.faint });
            let size = IconSize::Medium;
            canvas.icon(
                icon,
                size,
                x,
                b.y + (b.height as i32 - size.px() as i32) / 2,
                ink(&p, enabled),
            );
            x += size.px() as i32 + 6;
            CAPTION
        }
        None => {
            canvas.triangle_down(arrow, ink(&p, enabled));
            MEDIUM
        }
    };
    canvas.text(
        Rect::new(x, b.y, (arrow.x - x - 6).max(0) as u32, b.height),
        value,
        font,
        ink(&p, enabled),
        Align::Left,
    );
}

/// Flat stock list row: icon at +16, label at +54 (or +16), value right,
/// then the grey ⋮ in the last 48 px when `menu` is set.
#[allow(clippy::too_many_arguments)]
fn list_row(
    canvas: &mut Canvas,
    b: Rect,
    (label, font): (&str, Font),
    icon: Option<Icon>,
    value: &str,
    (selected, divider, menu): (bool, bool, bool),
    enabled: bool,
    pressed: bool,
) {
    let p = canvas.palette;
    if selected || pressed {
        canvas.fill(b, if pressed { p.row_selected } else { p.surface });
    }
    let text = match (selected, enabled) {
        (_, false) => p.faint,
        (true, true) => p.accent,
        (false, true) => p.text,
    };
    let mut x = b.x + 16;
    if let Some(icon) = icon {
        let size = IconSize::Medium;
        canvas.icon(
            icon,
            size,
            x,
            b.y + (b.height as i32 - size.px() as i32) / 2,
            // Stock keeps the icon white on the selected (green) row.
            ink(&p, enabled),
        );
        x += 38;
    }
    let mut right = b.right() - 16;
    if menu {
        let menu = Rect::new(b.right() - 48, b.y, 48, b.height);
        canvas.icon_in(menu, Icon::More, IconSize::Medium, p.faint);
        right = menu.x;
    }
    let value_width = trailing_value(canvas, b, right, value, enabled);
    let gap = if value_width > 0 { 12 } else { 0 };
    canvas.text(
        Rect::new(
            x,
            b.y,
            (right - value_width - gap - x).max(0) as u32,
            b.height,
        ),
        label,
        font,
        text,
        Align::Left,
    );
    if divider {
        canvas.fill(Rect::new(b.x, b.bottom() - 1, b.width, 1), p.divider);
    }
}

/// Stock two-state switch: a vertical black slot with a green ring at the
/// active end and the option names to its right, active white (shot 091).
fn switch(canvas: &mut Canvas, b: Rect, on: Option<bool>, enabled: bool) {
    const TRACK: (u32, u32) = (22, 46);
    let track = Rect::new(
        b.x,
        b.y + (b.height as i32 - TRACK.1 as i32) / 2,
        TRACK.0,
        TRACK.1,
    );
    // Disabled, stock dims the ring and tints the slot green (068 LATCH, 102 PHANTOM).
    const DISABLED_RING: Rgb = Rgb(0x20, 0x70, 0x28);
    const DISABLED_SLOT: Rgb = Rgb(0x10, 0x1c, 0x10);
    let p = canvas.palette;
    let track_fill = if enabled {
        p.canvas
    } else {
        p.tone(DISABLED_SLOT)
    };
    canvas.round_rect(track, TRACK.0 as f32 / 2.0, track_fill);
    let rows = [(true, "On", track.y), (false, "Off", track.y + 23)];
    for (value, text, y) in rows {
        let active = on == Some(value);
        canvas.text(
            Rect::new(track.right() + 16, y, b.width.saturating_sub(38), 23),
            text,
            BODY,
            match (active, enabled) {
                (true, true) => p.text,
                (false, false) => p.section,
                _ => p.faint,
            },
            Align::Left,
        );
        if active {
            let (cx, cy) = (track.x as f32 + 11.0, y as f32 + 11.5);
            canvas.circle(
                cx,
                cy,
                9.5,
                if enabled {
                    p.accent
                } else {
                    p.tone(DISABLED_RING)
                },
            );
            canvas.circle(cx, cy, 6.5, track_fill);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn block(
    canvas: &mut Canvas,
    b: Rect,
    label: &str,
    hue: Option<Rgb>,
    fill: Rgb,
    icon: Option<Icon>,
    bypassed: bool,
    selected: bool,
    drop_target: bool,
    ghost: bool,
    capture_two: bool,
    pressed: bool,
) {
    let p = canvas.palette;
    // The slot is 70 × 70; the visible tile is 64 × 64 inside it (shot 013).
    let tile = b.inset(3, 3);
    let Some(hue) = hue else {
        // Empty slots draw nothing; only the tapped (or drop) target shows
        // the stock black "+" tile (shot 076).
        if pressed || drop_target {
            canvas.round_rect(tile, tile_ring_radius(2.0), p.background);
            if drop_target {
                canvas.round_outline(tile, tile_ring_radius(2.0), 2.0, p.accent);
            }
            canvas.icon_in(
                tile,
                Icon::Plus,
                IconSize::Large,
                if drop_target { p.accent } else { p.text },
            );
        }
        return;
    };
    // Active: 2 px category outline on `fill`, white glyph. Bypassed: outline
    // and glyph at about 19 % with no fill, so the cable shows through
    // (shot 013; opacity inferred; I: a bypassed plugin block is not measured).
    let (outline, ink) = if bypassed {
        (hue.mix(p.canvas, 206), p.text.mix(p.canvas, 206))
    } else {
        (hue, p.text)
    };
    // Stock fills the stroked path itself: the 62 px rect on the pen's centre line, radius 13.
    let body = tile.inset(1, 1);
    if pressed {
        canvas.round_rect(body, TILE_RADIUS, p.surface);
    } else if !bypassed {
        canvas.round_rect(body, TILE_RADIUS, fill);
    }
    // Stock (`b4-stock1`): the selected tile's ring is 6 px wide, in the block's colour.
    let ring = if selected { 6.0 } else { 2.0 };
    canvas.round_outline(tile, tile_ring_radius(ring), ring, outline);
    // Stock `0x427db4` (item byte `+0x1f0`, set by `0x6966d4` for model 14001): a second
    // 2 px rounded square 5 px inside the tile, at opacity 0.6 (`nc-s1`: `#299639` at tile
    // y 5–6 and 57–58 of a green capture), `drawRoundedRect(10, 10)` on the pen's centre line.
    if capture_two {
        canvas.round_outline(tile.inset(5, 5), 11.0, 2.0, outline.mix(p.canvas, 102));
    }
    match icon {
        // Stock draws the 32 px device glyph unscaled, centred in the tile (QC screenshots,
        // 2026-10-16: `utility.png` ink at tile + 16).
        Some(icon) => canvas.icon_in(tile, icon, IconSize::Large, ink),
        None => {
            let lines = crate::render::wrap(label, SMALL_BOLD, (tile.width - 8) as f32);
            let shown = lines.len().min(3);
            let line_height = 17;
            let top = tile.y + (tile.height as i32 - shown as i32 * line_height) / 2;
            for (index, line) in lines.iter().take(shown).enumerate() {
                canvas.text(
                    Rect::new(
                        tile.x + 4,
                        top + index as i32 * line_height,
                        tile.width - 8,
                        line_height as u32,
                    ),
                    line,
                    SMALL_BOLD,
                    ink,
                    Align::Center,
                );
            }
        }
    }
    if drop_target {
        canvas.round_outline(tile.grow(3, 3), tile_ring_radius(2.0) + 3.0, 2.0, p.accent);
    }
    if ghost {
        canvas.round_outline(tile, tile_ring_radius(2.0), 2.0, p.accent);
    }
}

/// A trailing unit ("0.0 dB", "120 BPM") is drawn small and grey, as stock.
fn split_unit(value: &str) -> (&str, Option<&str>) {
    match value.rsplit_once(' ') {
        Some((number, unit))
            if !unit.is_empty()
                && unit.len() <= 4
                && unit.chars().all(|ch| ch.is_alphabetic() || ch == '%')
                && number.parse::<f32>().is_ok() =>
        {
            (number, Some(unit))
        }
        _ => (value, None),
    }
}

/// Stock parameter cell (shots 127, 008): label top-left, value bottom-left,
/// a 58 px knob on the right with a 270° arc in `color` from the lower left,
/// a black face and a white pointer. `dial_dy` moves the dial and value only.
#[allow(clippy::too_many_arguments)]
fn knob(
    canvas: &mut Canvas,
    b: Rect,
    label: &str,
    value: &str,
    kind: ParameterKind,
    color: Rgb,
    focused: bool,
    pressed: bool,
    enabled: bool,
    dial_dy: i32,
) {
    let p = canvas.palette;
    if focused || pressed {
        canvas.fill(b, p.panel.mix(p.text, if pressed { 24 } else { 12 }));
    }
    let ink = ink(&p, enabled);
    canvas.text(
        Rect::new(b.x + 10, b.y + 6, b.width.saturating_sub(20), 28),
        label,
        BODY,
        ink,
        Align::Left,
    );
    match kind {
        ParameterKind::Continuous { position }
        | ParameterKind::Centred { position }
        | ParameterKind::Int { position } => {
            // Stock 008: centre 41.5 px left of the cell edge and 71.5 px
            // below its top. The ring cache is centred on a pixel corner.
            let top = b.y + dial_dy;
            let (cx, cy) = (b.right() as f32 - 41.5, top as f32 + 71.5);
            let center = (b.right() - 42, top + 71);
            let (track, value_arc) = knob_rings();
            canvas.arc(center, track, 135.0, 270.0, p.row_selected);
            // Stock `0x4d0c44`: a centred arc starts at the top with span
            // sweep/2·(2·pos − 1); otherwise it starts at the minimum.
            let pointer = 135.0 + 270.0 * position.clamp(0.0, 1.0);
            let (start, end) = match kind {
                ParameterKind::Centred { .. } => (pointer.min(270.0), pointer.max(270.0)),
                _ => (135.0, pointer),
            };
            // Disabled arcs keep a dim `color` (R38: GAIN/Q `#104468`).
            let arc = if enabled {
                color
            } else {
                p.row_selected.mix(color, 112)
            };
            canvas.arc(center, value_arc, start, end - start, arc);
            // Stock face: the track grey inside a 3 px black ring (r 21.5–24.5);
            // disabled, the ring is dim. The pointer runs from r 10 to r 24.
            let rim = if enabled {
                p.canvas
            } else {
                p.row_selected.mix(p.canvas, 64)
            };
            canvas.circle(cx, cy, 24.5, rim);
            canvas.circle(cx, cy, 21.5, p.row_selected);
            let (dx, dy) = (pointer.to_radians().cos(), pointer.to_radians().sin());
            canvas.line(
                cx + dx * 10.0,
                cy + dy * 10.0,
                cx + dx * 24.0,
                cy + dy * 24.0,
                3.0,
                ink,
            );
            // Value baseline 88 px below the cell top, 12 px in (008, 009).
            let (x, baseline) = (b.x + 12, top + 88);
            let (number, unit) = split_unit(value);
            let end = canvas.text_at(
                x as f32,
                baseline,
                &fit(number, BODY, (center.0 - 34 - x) as f32),
                BODY,
                ink,
            );
            if let Some(unit) = unit {
                canvas.text_at(end + 4.0, baseline, unit, CAPTION, p.muted);
            }
        }
        ParameterKind::Toggle { on } => {
            switch(
                canvas,
                Rect::new(b.x + 16, b.y + 38, b.width.saturating_sub(32), 52),
                Some(on),
                enabled,
            );
        }
        ParameterKind::Choice { .. }
        | ParameterKind::ComboBox { .. }
        | ParameterKind::IconChoice { .. }
        | ParameterKind::Media(_) => {
            let icon = match kind {
                ParameterKind::IconChoice { icon } => Some(icon),
                _ => None,
            };
            dropdown(
                canvas,
                Rect::new(b.x + 10, b.bottom() - 48, b.width.saturating_sub(20), 36),
                value,
                icon,
                enabled,
                false,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{opaque_cover, split_unit, uncovered};
    use crate::Rect;
    use crate::layout::{Layout, Look};
    use tetrad_assets::Palette;

    /// A redraw starts at the fills that cover the rectangle together (side by side, or one
    /// over all of it), and an item shows only where later fills leave it open; a fill with
    /// the open part around it does not count.
    #[test]
    fn opaque_cover_finds_fills_that_cover_the_rectangle() {
        let full = Rect::new(0, 0, 800, 480);
        let mut l = Layout::new(Palette::STOCK);
        l.push(full, "", Look::Fill(Palette::STOCK.panel));
        l.push(
            Rect::new(0, 0, 400, 480),
            "",
            Look::Fill(Palette::STOCK.panel),
        );
        l.push(
            Rect::new(100, 0, 700, 480),
            "",
            Look::Fill(Palette::STOCK.panel),
        );
        l.push(
            Rect::new(300, 200, 100, 50),
            "",
            Look::Fill(Palette::STOCK.text),
        );
        l.push(
            Rect::new(0, 0, 800, 480),
            "",
            Look::Veil {
                color: Palette::STOCK.text,
                alpha: 9,
            },
        );
        assert_eq!(opaque_cover(l.items.iter(), full), Some(1));
        assert_eq!(
            opaque_cover(l.items.iter(), Rect::new(310, 210, 10, 10)),
            Some(3)
        );
        assert_eq!(opaque_cover(l.items[3..].iter(), full), None);
        assert_eq!(
            opaque_cover(l.items[2..].iter(), Rect::new(50, 0, 100, 10)),
            None
        );
        // The full fill shows only left of the later fill that holds the right side.
        let open = Rect::new(0, 0, 100, 480);
        assert_eq!(uncovered(l.items[2..].iter(), full), Some(open));
        assert_eq!(uncovered(l.items[1..].iter(), full), None);
    }

    #[test]
    fn only_a_short_alphabetic_suffix_after_a_number_is_a_unit() {
        assert_eq!(split_unit("0.0 dB"), ("0.0", Some("dB")));
        assert_eq!(split_unit("120 BPM"), ("120", Some("BPM")));
        assert_eq!(split_unit("MODERN"), ("MODERN", None));
        assert_eq!(split_unit("USB IN"), ("USB IN", None));
        assert_eq!(split_unit("1/4 NOTE"), ("1/4 NOTE", None));
    }
}

//! Settings → Appearance (owned, Tetrad): one row per theme with its swatches. A tap
//! applies the theme at once; a theme that failed its checks shows why and cannot be picked.

use tetrad_assets::{Icon, IconSize, Palette};

use crate::layout::{BODY, CAPTION, Layout, ListId, Look};
use crate::render::Align;
use crate::{Control, Rect, ThemeRow, UiState};

const ROW: u32 = 60;
const SWATCH: u32 = 22;
const SWATCH_GAP: u32 = 8;
const NOTE: u32 = 40;

pub(crate) fn layout(ui: &UiState, l: &mut Layout, body: Rect) {
    let p = l.palette;
    let themes = &ui.snapshot.appearance.themes;
    let area = Rect::new(body.x, body.y, body.width + 8, body.height - NOTE);
    let offset = ui.offset(ListId::Themes);
    l.scroll(
        ListId::Themes,
        area,
        themes.len() as u32 * ROW,
        offset,
        |l, y| {
            for (index, theme) in themes.iter().enumerate() {
                let row = Rect::new(area.x, y + (index as u32 * ROW) as i32, area.width, ROW);
                theme_row(l, &p, row, index as u16, theme);
            }
        },
    );
    l.paragraph(
        Rect::new(
            body.x,
            body.bottom() - NOTE as i32 + 8,
            body.width,
            NOTE - 8,
        ),
        "Add themes as JSON files in /media/p4/tetrad/themes, or install them with an extension.",
        CAPTION,
        p.muted,
    );
}

fn theme_row(l: &mut Layout, p: &Palette, row: Rect, index: u16, theme: &ThemeRow) {
    let usable = theme.palette.is_ok();
    if theme.selected {
        l.push(row, "", Look::Fill(p.row_selected));
    }
    l.control(
        row,
        Control::Theme(index),
        usable && !theme.selected,
        theme.name.clone(),
        Look::Area,
    );
    let check = Rect::new(row.right() - 48, row.y, 32, row.height);
    if theme.selected {
        l.push(
            check,
            "",
            Look::Glyph {
                icon: Icon::Check,
                size: IconSize::Small,
                color: p.accent,
            },
        );
    }
    let mut right = check.x - 8;
    if let Ok(colours) = &theme.palette {
        // Background, card, accent and action: the colours a theme is recognised by.
        for fill in [
            colours.action,
            colours.accent,
            colours.panel,
            colours.background,
        ]
        .iter()
        {
            right -= SWATCH as i32;
            let dot = Rect::new(
                right,
                row.y + (row.height - SWATCH) as i32 / 2,
                SWATCH,
                SWATCH,
            );
            l.panel(dot, *fill, SWATCH as f32 / 2.0, Some(p.section));
            right -= SWATCH_GAP as i32;
        }
    }
    let x = row.x + 16;
    let width = (right - x).max(0) as u32;
    let (name_ink, meta, meta_ink) = match &theme.palette {
        Ok(_) => (p.text, theme.source.clone(), p.muted),
        Err(reason) => (p.faint, format!("{} · {reason}", theme.source), p.danger),
    };
    l.text(
        Rect::new(x, row.y + 8, width, 22),
        theme.name.clone(),
        BODY,
        if theme.selected { p.accent } else { name_ink },
        Align::Left,
    );
    l.text(
        Rect::new(x, row.y + 32, width, 18),
        meta,
        CAPTION,
        meta_ink,
        Align::Left,
    );
    l.push(
        Rect::new(row.x, row.bottom() - 1, row.width, 1),
        "",
        Look::Fill(p.divider),
    );
}

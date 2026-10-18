//! Device Storage (manual §10 System: "User Presets, User Captures, and Impulse Responses")
//! and Account › Backups (manual §10 screenshot: "Cloud Backups n/5" header, UTC note, one
//! row per backup with a ⋮ menu, NEW CLOUD BACKUP under the list).

use tetrad_assets::{
    ACCENT, BACKGROUND, DIVIDER, FAINT, Icon, IconSize, MUTED, TEXT, WARNING, block,
};

use crate::layout::{BODY, ButtonStyle, CAPTION, LARGE, Layout, ListId, Look, MEDIUM};
use crate::render::Align;
use crate::*;

/// "You can store up to 5 backups on Cortex Cloud." (stock text).
pub(super) const BACKUP_LIMIT: usize = 5;
const BACKUP_ROW: u32 = 60;
const STORAGE_ROW: u32 = 92;
const QUARANTINE_ROW: u32 = 52;

/// Usage at or above 90 % warns (I: stock threshold not recovered).
fn nearly_full(usage: StorageUse) -> bool {
    usage.maximum > 0 && u64::from(usage.current) * 10 >= u64::from(usage.maximum) * 9
}

pub(super) fn layout(ui: &UiState, l: &mut Layout, area: Rect) {
    let rows = &ui.snapshot.quarantine;
    if rows.is_empty() {
        usage(ui, l, area);
        return;
    }
    // O: with quarantined captures the page scrolls, so the list gets the room it needs.
    let content = 3 * STORAGE_ROW + 32 + rows.len() as u32 * QUARANTINE_ROW;
    l.scroll(
        ListId::Quarantine,
        area,
        content,
        ui.offset(ListId::Quarantine),
        |l, y| {
            let page = Rect::new(area.x, y, area.width, content);
            usage(ui, l, page);
            quarantine(l, page, rows);
        },
    );
}

fn usage(ui: &UiState, l: &mut Layout, area: Rect) {
    let storage = &ui.snapshot.storage;
    // Stock bar colours per row (`0x65a8bc`).
    for (index, (label, usage, color)) in [
        ("My Presets", storage.presets, block::FILTER),
        ("My Captures", storage.captures, block::EQ),
        (
            "My Impulse Responses",
            storage.impulse_responses,
            block::CAB,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let row = Rect::new(
            area.x,
            area.y + index as i32 * STORAGE_ROW as i32,
            area.width,
            STORAGE_ROW,
        );
        let fraction = if usage.maximum == 0 {
            0.0
        } else {
            (usage.current as f32 / usage.maximum as f32).min(1.0)
        };
        l.text(
            Rect::new(row.x, row.y, row.width - 140, 24),
            label,
            MEDIUM,
            TEXT,
            Align::Left,
        );
        l.text(
            Rect::new(row.right() - 140, row.y, 140, 24),
            // An unknown limit (0) shows the count alone.
            if usage.maximum == 0 {
                usage.current.to_string()
            } else {
                format!("{} / {}", usage.current, usage.maximum)
            },
            BODY,
            MUTED,
            Align::Right,
        );
        let full = nearly_full(usage);
        l.push(
            Rect::new(row.x, row.y + 32, row.width, 16),
            "",
            Look::Progress {
                fraction,
                color: if full { WARNING } else { color },
            },
        );
        if full {
            l.text(
                Rect::new(row.x, row.y + 54, row.width, 20),
                "Storage usage for this item category is nearing its limit.",
                CAPTION,
                WARNING,
                Align::Left,
            );
        }
        if index < 2 {
            l.push(
                Rect::new(row.x, row.bottom() - 9, row.width, 1),
                "",
                Look::Fill(DIVIDER),
            );
        }
    }
}

/// O (stock deletes corrupt captures; review r23 K5): under the bars, the captures the
/// corruption check moved to the quarantine, each with RESTORE.
fn quarantine(l: &mut Layout, area: Rect, rows: &[QuarantineRow]) {
    let top = area.y + 3 * STORAGE_ROW as i32;
    l.text(
        Rect::new(area.x, top, area.width, 24),
        "Quarantined Captures",
        MEDIUM,
        TEXT,
        Align::Left,
    );
    for (index, item) in rows.iter().enumerate() {
        let row = Rect::new(
            area.x,
            top + 32 + index as i32 * QUARANTINE_ROW as i32,
            area.width,
            QUARANTINE_ROW,
        );
        let text = row.width - 136;
        l.text(
            Rect::new(row.x, row.y + 2, text, 22),
            item.id.clone(),
            BODY,
            TEXT,
            Align::Left,
        );
        let store = if item.downloads {
            "Downloads"
        } else {
            "My Captures"
        };
        l.text(
            Rect::new(row.x, row.y + 24, text, 18),
            format!("{store} · {}", item.reason),
            CAPTION,
            MUTED,
            Align::Left,
        );
        l.button(
            Rect::new(row.right() - 124, row.y + 4, 124, 40),
            Control::RestoreCapture(index as u16),
            true,
            "RESTORE",
            ButtonStyle::Secondary,
        );
    }
}

/// The whole page panel: the header replaces the page name, as in the manual screenshot.
pub(super) fn backups(ui: &UiState, l: &mut Layout, pane: Rect) {
    let s = &ui.snapshot;
    let linked = s.account.username.is_some();
    let items = &s.backups.items;
    // Shot 117: "Cloud Backups" (`0xa3a558`) then a grey count, UTC note right-aligned.
    let title = "Cloud Backups";
    let title_width = LARGE.width(title).ceil() as i32;
    l.text(
        Rect::new(pane.x, pane.y, title_width as u32 + 4, 32),
        title,
        LARGE,
        TEXT,
        Align::Left,
    );
    l.text(
        Rect::new(pane.x + title_width + 8, pane.y, 60, 32),
        format!("{}/{BACKUP_LIMIT}", items.len()),
        LARGE,
        MUTED,
        Align::Left,
    );
    l.text(
        Rect::new(pane.right() - 220, pane.y, 220, 32),
        "All timestamps are UTC",
        CAPTION,
        MUTED,
        Align::Right,
    );
    let mut top = pane.y + 47;
    if !linked {
        l.paragraph(
            Rect::new(pane.x, top, pane.width, 40),
            "You are not logged in to Cortex Cloud",
            MEDIUM,
            MUTED,
        );
        return;
    }
    if let Some((text, progress)) = &s.backups.busy {
        l.text(
            Rect::new(pane.x, top, pane.width, 20),
            text.clone(),
            BODY,
            TEXT,
            Align::Left,
        );
        if let Some(fraction) = progress {
            l.push(
                Rect::new(pane.x, top + 24, pane.width, 8),
                "",
                Look::Progress {
                    fraction: *fraction,
                    color: ACCENT,
                },
            );
        }
        top += 40;
    }
    let content = items.len() as u32 * BACKUP_ROW;
    let viewport = Rect::new(
        pane.x,
        top,
        pane.width,
        content.min((pane.bottom() - 82 - top).max(0) as u32),
    );
    let enabled = s.backups.busy.is_none();
    l.scroll(
        ListId::Backups,
        viewport,
        content,
        ui.offset(ListId::Backups),
        |l, y| {
            for (index, item) in items.iter().enumerate() {
                let row = Rect::new(
                    viewport.x,
                    y + index as i32 * BACKUP_ROW as i32,
                    viewport.width,
                    BACKUP_ROW,
                );
                backup_row(l, row, item, enabled);
            }
        },
    );
    if items.is_empty() {
        l.text(
            Rect::new(pane.x, top + 8, pane.width, 24),
            "No Cloud Backups",
            BODY,
            FAINT,
            Align::Left,
        );
    }
    // Stock 117 labels this 44 px button in 16 px (the shared button uses 18 px).
    let create = Rect::new(pane.x, viewport.bottom().max(top + 40) + 22, 219, 45);
    let can_create = enabled && items.len() < BACKUP_LIMIT;
    l.panel(create, BACKGROUND, 8.0, None);
    l.control(
        create,
        Control::Backup(BackupControl::Create),
        can_create,
        "NEW CLOUD BACKUP",
        Look::Area,
    );
    l.text(
        create,
        "NEW CLOUD BACKUP",
        BODY,
        if can_create { TEXT } else { FAINT },
        Align::Center,
    );
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Stock date form, e.g. "August 29th, 2025, 17:06". Other text is shown as given.
pub(super) fn stock_timestamp(text: &str) -> String {
    let formatted = text.split_once(' ').and_then(|(date, time)| {
        let [year, month, day] = date.split('-').collect::<Vec<_>>()[..] else {
            return None;
        };
        let (year, month, day): (u32, usize, u32) =
            (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
        let name = MONTHS.get(month.checked_sub(1)?)?;
        Some(format!("{name} {day}{}, {year}, {time}", ordinal(day)))
    });
    formatted.unwrap_or_else(|| text.to_owned())
}

fn ordinal(day: u32) -> &'static str {
    match (day % 10, day % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    }
}

/// Shot 117 row: name, grey timestamp line, grey ⋮ dots, divider.
fn backup_row(l: &mut Layout, row: Rect, item: &BackupItem, enabled: bool) {
    l.control(
        row,
        Control::Backup(BackupControl::Item(item.id)),
        enabled,
        item.title.clone(),
        Look::Area,
    );
    l.text(
        Rect::new(row.x, row.y + 6, row.width - 48, 24),
        item.title.clone(),
        BODY,
        TEXT,
        Align::Left,
    );
    // Stock 117 marks are about 16 px wide.
    let mark = |l: &mut Layout, x: i32, icon: Icon| {
        l.push(
            Rect::new(x, row.y + 31, 18, 18),
            "",
            Look::Glyph {
                icon,
                size: IconSize::Compact,
                color: MUTED,
            },
        );
    };
    mark(l, row.x, Icon::CloudUpload);
    l.text(
        Rect::new(row.x + 22, row.y + 31, 200, 20),
        stock_timestamp(&item.detail),
        CAPTION,
        MUTED,
        Align::Left,
    );
    if let Some(downloaded) = &item.downloaded {
        mark(l, row.x + 229, Icon::Download);
        l.text(
            Rect::new(row.x + 251, row.y + 31, 200, 20),
            stock_timestamp(downloaded),
            CAPTION,
            MUTED,
            Align::Left,
        );
    }
    for y in [18, 26, 34] {
        l.push(
            Rect::new(row.right() - 14, row.y + y, 4, 4),
            "",
            Look::Dot { color: FAINT },
        );
    }
    l.push(
        Rect::new(row.x, row.bottom() - 1, row.width, 1),
        "",
        Look::Fill(DIVIDER),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The warning starts at 90 % and never divides by an unknown maximum.
    #[test]
    fn storage_warning_threshold() {
        let use_ = |current, maximum| StorageUse { current, maximum };
        assert!(!nearly_full(use_(0, 0)));
        assert!(!nearly_full(use_(89, 100)));
        assert!(nearly_full(use_(90, 100)));
        assert!(nearly_full(use_(u32::MAX, u32::MAX)));
    }

    /// Shot 117 timestamps use the stock date form; firmware text passes through.
    #[test]
    fn backup_timestamps_use_stock_date_form() {
        assert_eq!(
            stock_timestamp("2025-08-29 17:06"),
            "August 29th, 2025, 17:06"
        );
        assert_eq!(
            stock_timestamp("2023-08-11 15:38"),
            "August 11th, 2023, 15:38"
        );
        assert_eq!(
            stock_timestamp("2024-11-22 19:22"),
            "November 22nd, 2024, 19:22"
        );
        assert_eq!(stock_timestamp("3.1.0 beta"), "3.1.0 beta");
    }
}

//! IR / Neural Capture picker for one media parameter of the selected block,
//! in the Directory frame (spec `w4-ui-spec-directory` §1–2, shots 107, 047):
//! the Directory header row (drop-down ▼, REFRESH or UPLOAD, icon buttons, ✓),
//! a sidebar tree with counts ("IRs Library" ▸ Factory IRs, My IRs; "Captures
//! Library" ▸ My Captures) and 60 px two-line rows. Cloud rows and user
//! subfolders are not drawn. The picker is the Directory filtered to the media
//! type (I): MULTI SELECT, FILTER, SORT and SEARCH are the Directory controls and
//! pop-ups over this list. The "Factory IRs" label is owned (I).

use tetrad_assets::Icon;

use super::directory::{MediaDetails, media_sort_text};
use super::directory_glyph::Glyph;
use super::directory_lists::check_box;
use super::picker::{Mark, glyph};
use super::{directory_lists, presets};
use crate::layout::{BODY, ButtonStyle, CAPTION, Layout, ListId, Look};
use crate::render::{Align, HEIGHT, WIDTH};
use crate::widgets::CLOSE_BUTTON;
use crate::*;

const ROW: u32 = 60;
const SIDEBAR: Rect = Rect::new(8, 60, 256, HEIGHT - 68);
const CONTENT: Rect = Rect::new(272, 60, WIDTH - 280, HEIGHT - 68);
/// Tree line colour (`#505050`, shot 047).
const TREE: tetrad_assets::Rgb = tetrad_assets::Rgb(0x50, 0x50, 0x50);

pub(super) fn layout(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(p.background));
    l.control(
        CLOSE_BUTTON,
        Control::Back,
        true,
        "CLOSE",
        Look::IconButton {
            icon: Icon::Check,
            fill: p.surface,
        },
    );
    let Some(pick) = ui.media else {
        return;
    };
    let s = &ui.snapshot;
    let dir = &ui.directory;
    let (title, library, mark) = match pick.kind {
        MediaKind::Ir => ("Impulse Responses", "IRs Library", Mark::Impulse),
        MediaKind::Capture => (
            "Neural Captures",
            "Captures Library",
            Mark::Group(DeviceGroup::NeuralCapture),
        ),
    };
    // The Directory header row (R24, shot 107); Multi Select swaps in its header.
    if dir.multi.is_some() {
        presets::multi_header(ui, l);
    } else {
        header(ui, l, pick.kind, title, mark);
    }

    let current = s
        .parameters
        .iter()
        .find(|parameter| parameter.index == pick.parameter)
        .map(|parameter| parameter.value.as_str());
    let folders: Vec<MediaFolder> = MediaFolder::ALL
        .into_iter()
        .filter(|folder| folder.kind() == pick.kind)
        .collect();
    let count = |folder: Option<MediaFolder>| {
        s.media
            .iter()
            .filter(|entry| match folder {
                Some(folder) => entry.folder == folder,
                None => entry.folder.kind() == pick.kind,
            })
            .count()
    };

    l.panel(SIDEBAR, p.panel, 8.0, None);
    l.scroll(
        ListId::MediaFolders,
        SIDEBAR,
        (folders.len() as u32 + 1) * ROW,
        ui.offset(ListId::MediaFolders),
        |l, top| {
            let rows = std::iter::once((None, library))
                .chain(folders.iter().map(|folder| (Some(*folder), folder.label())));
            for (index, (folder, label)) in rows.enumerate() {
                let row = Rect::new(
                    SIDEBAR.x,
                    top + index as i32 * ROW as i32,
                    SIDEBAR.width,
                    ROW,
                );
                let selected = pick.folder == folder;
                if selected {
                    l.push(row, "", Look::Fill(p.surface));
                }
                l.control(
                    row,
                    Control::Media(folder.map_or(MediaControl::Library, MediaControl::Folder)),
                    true,
                    label,
                    Look::Area,
                );
                let child = folder.is_some();
                // Stock label x: 62 for the library, 78 for folders (shots 107, 047).
                let (icon, text_x) = if child {
                    (Rect::new(row.x + 26, row.y + 10, 40, 40), row.x + 70)
                } else {
                    (Rect::new(row.x + 14, row.y + 18, 24, 24), row.x + 54)
                };
                if child {
                    // Tree line from the library row with an elbow to each child.
                    let (x, middle) = (row.x + 17, row.y + ROW as i32 / 2);
                    let last = index == folders.len();
                    l.push(
                        Rect::new(x, row.y, 1, if last { ROW / 2 } else { ROW }),
                        "",
                        Look::Fill(p.tone(TREE)),
                    );
                    l.push(Rect::new(x, middle, 12, 1), "", Look::Fill(p.tone(TREE)));
                    l.push(
                        icon,
                        "",
                        Look::Button {
                            style: ButtonStyle::Ghost,
                            icon: Some(Icon::Folder),
                            font: None,
                        },
                    );
                } else {
                    glyph(
                        l,
                        Mark::Library,
                        icon,
                        if selected { p.accent } else { p.text },
                    );
                }
                l.text(
                    Rect::new(text_x, row.y, (row.right() - 48 - text_x) as u32, ROW),
                    label,
                    BODY,
                    if selected { p.accent } else { p.text },
                    Align::Left,
                );
                l.text(
                    Rect::new(row.right() - 52, row.y, 40, ROW),
                    count(folder).to_string(),
                    BODY,
                    p.muted,
                    Align::Right,
                );
            }
        },
    );

    // Directory rules: Made by Me, the search query, then the sort role.
    let query = dir
        .search
        .is_some()
        .then(|| ui.library_query.to_lowercase());
    let details = MediaDetails::new(s);
    let mut items: Vec<_> = s
        .media
        .iter()
        .enumerate()
        .filter(|(_, entry)| match pick.folder {
            Some(folder) => entry.folder == folder,
            None => entry.folder.kind() == pick.kind,
        })
        .filter(|(_, entry)| !dir.made_by_me || entry.folder == MediaFolder::MyCaptures)
        .filter(|(_, entry)| {
            query
                .as_ref()
                .is_none_or(|query| entry.name.to_lowercase().contains(query))
        })
        .map(|item| (item, details.roles(item.1)))
        .collect();
    items.sort_by(|a, b| dir.media_sort.order(&a.1, &b.1));
    let items: Vec<(usize, &MediaEntry)> = items.into_iter().map(|(item, _)| item).collect();
    l.panel(CONTENT, p.panel, 8.0, None);
    l.scroll(
        ListId::MediaItems,
        CONTENT,
        items.len() as u32 * ROW,
        ui.offset(ListId::MediaItems),
        |l, top| {
            for (row_index, (index, entry)) in items.iter().enumerate() {
                let row = Rect::new(
                    CONTENT.x,
                    top + row_index as i32 * ROW as i32,
                    CONTENT.width,
                    ROW,
                );
                // `validate` bounds the list by `MAX_MEDIA_ITEMS`, so the index fits.
                let index = *index as u16;
                let checked = dir
                    .multi
                    .as_ref()
                    .map(|checked| checked.contains(&u64::from(index)));
                l.control(
                    row,
                    match checked {
                        Some(_) => Control::Library(LibraryControl::Check(u64::from(index))),
                        None => Control::Media(MediaControl::Item(index)),
                    },
                    true,
                    entry.name.clone(),
                    Look::Area,
                );
                let selected = current == Some(entry.name.as_str());
                let text_width = CONTENT.width - 90;
                l.text(
                    Rect::new(row.x + 16, row.y + 8, text_width, 24),
                    entry.name.clone(),
                    BODY,
                    if selected { p.accent } else { p.text },
                    Align::Left,
                );
                let sort_text = media_sort_text(ui, &details, entry);
                l.text(
                    Rect::new(row.x + 16, row.y + 33, text_width, 18),
                    if sort_text == entry.name || sort_text.is_empty() {
                        entry.folder.label()
                    } else {
                        sort_text
                    },
                    CAPTION,
                    p.muted,
                    Align::Left,
                );
                if let Some(checked) = checked {
                    check_box(l, Rect::new(row.right() - 48, row.y + 8, 40, 44), checked);
                }
                // Sort-key initial (shot 047).
                l.text(
                    Rect::new(row.right() - 70, row.y, 30, ROW),
                    sort_text
                        .chars()
                        .next()
                        .map(|initial| initial.to_uppercase().to_string())
                        .unwrap_or_default(),
                    BODY,
                    p.muted,
                    Align::Center,
                );
                if row_index + 1 < items.len() {
                    l.push(
                        Rect::new(row.x + 16, row.bottom() - 1, row.width - 32, 1),
                        "",
                        Look::Fill(p.surface),
                    );
                }
            }
        },
    );
    if items.is_empty() {
        let text = match pick.kind {
            MediaKind::Ir => "No IRs in this folder",
            MediaKind::Capture => "No Neural Captures available",
        };
        l.text(
            Rect::new(CONTENT.x, CONTENT.y + 40, CONTENT.width, 30),
            text,
            BODY,
            p.faint,
            Align::Center,
        );
    }
    presets::popup(ui, l);
}

/// The Directory header for the picked media type: the drop-down selects the library
/// root, then REFRESH (IRs) or UPLOAD (captures), the icon buttons and ✓.
fn header(ui: &UiState, l: &mut Layout, kind: MediaKind, title: &str, mark: Mark) {
    let p = l.palette;
    let dir = &ui.directory;
    presets::dropdown(
        l,
        Control::Media(MediaControl::Library),
        |l, area| {
            let (cx, cy) = area.center();
            glyph(l, mark, Rect::new(cx - 11, cy - 11, 22, 22), p.text);
        },
        title,
    );
    match kind {
        MediaKind::Ir => l.control(
            presets::WIDE,
            Control::Library(LibraryControl::Refresh),
            true,
            "REFRESH",
            Look::IconButton {
                icon: Icon::Sync,
                fill: p.surface,
            },
        ),
        MediaKind::Capture => directory_lists::glyph_button(
            l,
            presets::WIDE,
            Control::Library(LibraryControl::Upload),
            true,
            "UPLOAD",
            Glyph::CloudUp,
            p.surface,
        ),
    }
    presets::divider(l, 379);
    let button = |control, label, mark, active: bool| presets::HeaderButton {
        control: Control::Library(control),
        label,
        mark,
        enabled: true,
        fill: if active { p.accent_fill } else { p.surface },
    };
    let mut buttons = vec![button(
        LibraryControl::MultiSelect,
        "MULTI SELECT",
        Some(Glyph::MultiSelect),
        false,
    )];
    if kind == MediaKind::Capture {
        buttons.push(button(
            LibraryControl::Filter,
            "FILTER",
            Some(Glyph::Filter),
            dir.made_by_me,
        ));
    }
    buttons.extend([
        button(LibraryControl::Sort, "SORT", Some(Glyph::Sort), false),
        button(LibraryControl::Search, "SEARCH", None, dir.search.is_some()),
    ]);
    presets::icon_buttons(l, &buttons);
    presets::divider(l, 685);
}

//! Directory panels other than the Bank View: sidebars, the flat item lists with subtitle,
//! ⋮ and alphabet strip (captures, IRs, List View, favorites, search), and the shared
//! row pieces (spec `w4-ui-spec-directory` §1–2, shots 033, 047, 107, 024).

use tetrad_assets::{Icon, Rgb};

use super::directory::{
    Category, DATE_POSITIONS, GAIN_POSITIONS, ITEM_ROW, JumpBar, MediaDetails, SortKey,
    directory_favorites, jump_bar, media_rows, media_sort_text, month_label, preset_rows,
    preset_sort_text, search_rows, slot_label,
};
use super::directory_glyph::{Glyph, glyph};
use crate::layout::{BODY, ButtonStyle, CAPTION, Layout, ListId, Look, SMALL_BOLD};
use crate::render::Align;
use crate::*;

pub(super) const SIDEBAR: Rect = Rect::new(8, 60, 256, 412);
/// Content panel of every category but Presets (Bank View).
pub(super) const CONTENT: Rect = Rect::new(272, 60, 520, 412);
/// Search results take the full width (shot 024).
const RESULTS: Rect = Rect::new(8, 60, 784, 412);
pub(super) const SIDE_ROW: u32 = 60;
/// Alphabet strip at the right end of a list panel (x ≈772).
const STRIP: u32 = 36;
const SECTION_BAR: u32 = 44;
const MENU: u32 = 48;

/// Leading mark of a sidebar row.
#[derive(Clone, Copy)]
pub(super) enum Mark {
    Icon(Icon),
    Glyph(Glyph),
    /// Setlist folder with its index (0 = Factory Presets, shot 054).
    Setlist(usize),
    /// Default saving folder: folder with a ↓ badge (shots 047, 107).
    SavingFolder,
}

pub(super) struct SideRow {
    pub control: Control,
    pub enabled: bool,
    pub label: String,
    pub mark: Mark,
    pub selected: bool,
    /// Right-aligned muted count.
    pub value: String,
    pub menu: Option<Control>,
    /// Dimmed but enabled ("New Setlist", shot 054).
    pub dim: bool,
    /// Child of a library row: indented, with a tree elbow.
    pub child: bool,
}

impl SideRow {
    pub fn new(control: Control, label: impl Into<String>, mark: Mark) -> Self {
        Self {
            control,
            enabled: true,
            label: label.into(),
            mark,
            selected: false,
            value: String::new(),
            menu: None,
            dim: false,
            child: false,
        }
    }
}

/// Three ⋮ dots centred in `area`.
pub(super) fn kebab(l: &mut Layout, area: Rect, color: Rgb) {
    let (cx, cy) = area.center();
    for dy in [-7, 0, 7] {
        l.push(
            Rect::new(cx - 2, cy + dy - 2, 4, 4),
            "",
            Look::Dot { color },
        );
    }
}

pub(super) fn sidebar_row(l: &mut Layout, b: Rect, row: SideRow) {
    let p = l.palette;
    if row.selected {
        l.push(b, "", Look::Fill(p.surface));
    }
    let (body, menu) = match row.menu {
        Some(_) => b.take_right(MENU, 0),
        None => (b, Rect::new(b.right(), b.y, 0, 0)),
    };
    l.control(
        body,
        row.control,
        row.enabled,
        row.label.clone(),
        Look::Area,
    );
    if let Some(control) = row.menu {
        l.control(menu, control, row.enabled, "MORE", Look::Area);
        kebab(l, menu, p.faint);
    }
    let indent = if row.child { 16 } else { 0 };
    let mark = Rect::new(b.x + 16 + indent, b.y, 24, b.height);
    let ink = if row.enabled && !row.dim {
        p.text
    } else {
        p.faint
    };
    // Ghost buttons draw only their icon (white); disabled rows use faint glyphs.
    let icon = |l: &mut Layout, icon| {
        l.push(
            mark,
            "",
            Look::Button {
                style: ButtonStyle::Ghost,
                icon: Some(icon),
                font: None,
            },
        );
    };
    match row.mark {
        Mark::Icon(mark_icon) => icon(l, mark_icon),
        Mark::Glyph(mark_glyph) => glyph(l, mark, mark_glyph, ink),
        Mark::Setlist(index) => super::directory_glyph::numbered_folder(l, mark, index, ink),
        Mark::SavingFolder => {
            icon(l, Icon::Folder);
            let badge = Rect::new(mark.right() - 8, b.y + 32, 14, 14);
            l.panel(badge, p.text, 3.0, None);
            let x = badge.x as f32 + 7.0;
            let (top, tip) = (badge.y as f32 + 3.0, badge.y as f32 + 10.5);
            l.line((x, top), (x, tip), 1.6, p.canvas);
            l.line((x - 3.0, tip - 3.0), (x, tip), 1.6, p.canvas);
            l.line((x + 3.0, tip - 3.0), (x, tip), 1.6, p.canvas);
        }
    }
    let label_x = mark.right() + 14;
    let value_width = if row.value.is_empty() {
        0
    } else {
        BODY.width(&row.value).ceil() as u32 + 4
    };
    let right = if row.menu.is_some() {
        menu.x
    } else {
        b.right() - 13
    };
    if value_width > 0 {
        l.text(
            Rect::new(right - value_width as i32, b.y, value_width, b.height),
            row.value,
            BODY,
            p.muted,
            Align::Right,
        );
    }
    let color = match (row.selected, ink == p.text) {
        (_, false) => p.faint,
        (true, true) => p.accent,
        (false, true) => p.text,
    };
    l.text(
        Rect::new(
            label_x,
            b.y,
            (right - label_x - value_width as i32 - 8).max(0) as u32,
            b.height,
        ),
        row.label,
        BODY,
        color,
        Align::Left,
    );
}

/// Tree lines from a library row down to each child row's elbow (shots 047, 107), in the
/// side panel at `left`.
pub(super) fn tree(l: &mut Layout, left: i32, top: i32, children: usize) {
    let p = l.palette;
    if children == 0 {
        return;
    }
    let x = left as f32 + 17.5;
    let last = top + (children as i32 - 1) * SIDE_ROW as i32 + SIDE_ROW as i32 / 2;
    l.line((x, top as f32), (x, last as f32), 1.0, p.faint);
    for child in 0..children {
        let y = (top + child as i32 * SIDE_ROW as i32 + SIDE_ROW as i32 / 2) as f32;
        l.line((x, y), (x + 10.0, y), 1.0, p.faint);
    }
}

/// One flat list item: title, muted subtitle, the sort-key initial and ⋮ (or a Multi
/// Select check box), 1 px divider inset 16 px.
pub(super) struct ItemRow<'a> {
    pub control: Control,
    pub title: &'a str,
    pub sort_text: &'a str,
    pub subtitle: String,
    pub selected: bool,
    pub menu: Option<Control>,
    /// Multi Select: whether the row is checked.
    pub checked: Option<bool>,
}

pub(super) fn item_row(l: &mut Layout, b: Rect, row: ItemRow) {
    let p = l.palette;
    let (body, menu) = b.take_right(MENU, 0);
    if row.selected {
        l.push(b, "", Look::Fill(p.surface));
    }
    l.control(
        if row.menu.is_some() && row.checked.is_none() {
            body
        } else {
            b
        },
        row.control,
        true,
        row.title,
        Look::Area,
    );
    let text_x = b.x + 16;
    let width = (menu.x - 40 - text_x).max(0) as u32;
    l.text(
        Rect::new(text_x, b.y + 8, width, 22),
        row.title,
        BODY,
        if row.selected { p.accent } else { p.text },
        Align::Left,
    );
    l.text(
        Rect::new(text_x, b.y + 32, width, 20),
        row.subtitle,
        CAPTION,
        p.muted,
        Align::Left,
    );
    l.text(
        Rect::new(menu.x - 36, b.y, 24, b.height),
        // The sort key's first character as shown ("4", "B"); the strip maps digits to `#`.
        row.sort_text
            .trim_start()
            .chars()
            .next()
            .map(|first| first.to_uppercase().to_string())
            .unwrap_or_default(),
        BODY,
        p.faint,
        Align::Center,
    );
    match (row.checked, row.menu) {
        (Some(checked), _) => check_box(l, menu, checked),
        (None, Some(control)) => {
            l.control(menu, control, true, "MORE", Look::Area);
            kebab(l, menu, p.faint);
        }
        (None, None) => {}
    }
    l.push(
        Rect::new(b.x + 16, b.bottom() - 1, b.width - 16, 1),
        "",
        Look::Fill(p.surface),
    );
}

/// Multi Select check box (I: no stock screenshot).
pub(super) fn check_box(l: &mut Layout, area: Rect, checked: bool) {
    let p = l.palette;
    let (cx, cy) = area.center();
    let box_ = Rect::new(cx - 12, cy - 12, 24, 24);
    if checked {
        l.push(
            box_,
            "",
            Look::IconButton {
                icon: Icon::Check,
                fill: p.accent,
            },
        );
    } else {
        l.panel(box_, p.canvas, 4.0, Some(p.faint));
    }
}

/// `# • A • I • R • Z` strip (shots 033, 047): 27 bands, a tap jumps the list (I).
fn directory_strip(l: &mut Layout, panel: Rect) {
    let strip = Rect::new(panel.right() - STRIP as i32, panel.y, STRIP, panel.height);
    index_strip(l, strip, 12, |key| {
        Control::Library(LibraryControl::Index(key))
    });
}

/// The 27 bands of an alphabet strip (`#`, A–Z) with the stock marks, the first and last
/// mark centred `inset` px from the strip ends; a band sends `control(key)`.
pub(super) fn index_strip(
    l: &mut Layout,
    strip: Rect,
    inset: u32,
    control: impl Fn(u8) -> Control,
) {
    let p = l.palette;
    let keys: Vec<u8> = std::iter::once(b'#').chain(b'A'..=b'Z').collect();
    let band = strip.height as f32 / keys.len() as f32;
    for (index, key) in keys.iter().enumerate() {
        let y = strip.y + (index as f32 * band) as i32;
        let next = strip.y + ((index + 1) as f32 * band) as i32;
        l.control(
            Rect::new(strip.x, y, strip.width, (next - y) as u32),
            control(*key),
            true,
            char::from(*key).to_string(),
            Look::Area,
        );
    }
    let marks = ["#", "•", "A", "•", "I", "•", "R", "•", "Z"];
    let pitch = (strip.height - 2 * inset) / (marks.len() as u32 - 1);
    for (index, mark) in marks.iter().enumerate() {
        l.text(
            Rect::new(
                strip.x,
                strip.y + (inset - 8 + index as u32 * pitch) as i32,
                strip.width,
                16,
            ),
            *mark,
            CAPTION,
            p.muted,
            Align::Center,
        );
    }
}

/// The jump bar of a capture/IR list by its sort (`JumpScrollbarContainer`): the letters, or
/// the Date (9 bands over the 200 positions), Device Type (one band per code) or Gain ("0" to
/// "10") bar. Owned text labels (I: stock draws device icons; date labels not recovered).
fn media_strip(ui: &UiState, details: &MediaDetails, l: &mut Layout, kind: MediaKind) {
    let p = l.palette;
    /// Owned short device labels in `CAPTURE_DEVICE_TYPES` order.
    const DEVICES: [&str; 9] = ["Def", "Amp", "Cmb", "A+C", "Cab", "Ped", "Fuz", "Cmp", "OD"];
    let bands: Vec<(u8, String)> = match jump_bar(ui, details, kind) {
        JumpBar::Letters => return directory_strip(l, CONTENT),
        JumpBar::Date(bar) => (0..9u32)
            .map(|band| {
                let position = band * DATE_POSITIONS / 8;
                let label = match bar.date_at(position) {
                    Some(day) if band % 2 == 0 => month_label(day),
                    _ => "•".into(),
                };
                (position as u8, label)
            })
            .collect(),
        JumpBar::Device => (0u8..).zip(DEVICES.map(String::from)).collect(),
        JumpBar::Gain => (0..GAIN_POSITIONS)
            .map(|gain| (gain, gain.to_string()))
            .collect(),
    };
    let strip = Rect::new(
        CONTENT.right() - STRIP as i32,
        CONTENT.y,
        STRIP,
        CONTENT.height,
    );
    let band = strip.height / bands.len() as u32;
    for (index, (position, label)) in bands.into_iter().enumerate() {
        let at = Rect::new(
            strip.x,
            strip.y + (index as u32 * band) as i32,
            strip.width,
            band,
        );
        l.control(
            at,
            Control::Library(LibraryControl::Jump(position)),
            true,
            label.clone(),
            Look::Area,
        );
        l.text(at, label, CAPTION, p.muted, Align::Center);
    }
}

fn empty(l: &mut Layout, panel: Rect, text: &str) {
    let p = l.palette;
    l.text(
        Rect::new(panel.x, panel.y + 40, panel.width, 30),
        text,
        BODY,
        p.faint,
        Align::Center,
    );
}

fn checked(ui: &UiState, id: u64) -> Option<bool> {
    ui.directory
        .multi
        .as_ref()
        .map(|checked| checked.contains(&id))
}

/// Captures or IRs: Downloads and Cloud (drawn enabled as stock), the library with its
/// folders, and the item list of the open folder or cloud folder.
pub(super) fn media(ui: &UiState, l: &mut Layout, category: Category) {
    let p = l.palette;
    let kind = category.media().expect("media category");
    let dir = &ui.directory;
    let folders: &[MediaFolder] = match kind {
        MediaKind::Capture => &[MediaFolder::MyCaptures],
        MediaKind::Ir => &[MediaFolder::FactoryIrs, MediaFolder::MyIrs],
    };
    let user = match kind {
        MediaKind::Capture => MediaFolder::MyCaptures,
        MediaKind::Ir => MediaFolder::MyIrs,
    };
    let count = ui
        .snapshot
        .media
        .iter()
        .filter(|entry| entry.folder.kind() == kind)
        .count();
    let (library, cloud_label) = match kind {
        MediaKind::Capture => ("Captures Library", "Cloud Captures"),
        MediaKind::Ir => ("IRs Library", "Cloud IRs"),
    };
    l.panel(SIDEBAR, p.panel, 8.0, None);
    let mut rows = Vec::new();
    if kind == MediaKind::Capture {
        rows.push(SideRow {
            selected: dir.cloud == Some(CloudScope::Downloads),
            ..SideRow::new(
                Control::Library(LibraryControl::Downloads),
                "Downloads",
                Mark::Glyph(Glyph::Download),
            )
        });
    }
    rows.push(SideRow {
        selected: dir.cloud == Some(CloudScope::Cloud),
        ..SideRow::new(
            Control::Library(LibraryControl::Cloud),
            cloud_label,
            Mark::Glyph(Glyph::Cloud),
        )
    });
    rows.push(SideRow {
        selected: dir.cloud.is_none() && dir.folder.is_none() && dir.database_folder.is_none(),
        value: count.to_string(),
        ..SideRow::new(
            Control::Library(LibraryControl::Library),
            library,
            Mark::Glyph(Glyph::Library),
        )
    });
    let first_child = rows.len();
    for folder in folders {
        if *folder == user
            && ui
                .snapshot
                .directory
                .folders
                .iter()
                .any(|folder| folder.kind == kind && folder.root)
        {
            continue;
        }
        rows.push(SideRow {
            selected: dir.cloud.is_none() && dir.folder == Some(*folder),
            menu: (*folder == user)
                .then_some(Control::Library(LibraryControl::FolderMenu(*folder))),
            child: true,
            ..SideRow::new(
                Control::Library(LibraryControl::Folder(*folder)),
                folder.label(),
                if *folder == user {
                    Mark::SavingFolder
                } else {
                    Mark::Icon(Icon::Folder)
                },
            )
        });
    }
    for folder in ui
        .snapshot
        .directory
        .folders
        .iter()
        .filter(|folder| folder.kind == kind)
    {
        rows.push(SideRow {
            selected: dir.cloud.is_none() && dir.database_folder == Some(folder.id),
            menu: (!folder.read_only).then_some(Control::Library(
                LibraryControl::DatabaseFolderMenu(folder.id),
            )),
            child: true,
            ..SideRow::new(
                Control::Library(LibraryControl::DatabaseFolder(folder.id)),
                &folder.name,
                if folder.root {
                    Mark::SavingFolder
                } else {
                    Mark::Icon(Icon::Folder)
                },
            )
        });
    }
    rows.push(SideRow {
        enabled: ui
            .snapshot
            .directory
            .folders
            .iter()
            .any(|folder| folder.kind == kind && folder.root && !folder.read_only),
        child: true,
        ..SideRow::new(
            Control::Library(LibraryControl::NewFolder),
            "New Folder",
            Mark::Glyph(Glyph::FolderPlus),
        )
    });
    let children = rows.len() - first_child;
    l.scroll(
        ListId::Setlists,
        SIDEBAR,
        rows.len() as u32 * SIDE_ROW,
        ui.offset(ListId::Setlists),
        |l, top| {
            for (index, row) in rows.into_iter().enumerate() {
                let y = top + (index as u32 * SIDE_ROW) as i32;
                sidebar_row(l, Rect::new(SIDEBAR.x, y, SIDEBAR.width, SIDE_ROW), row);
            }
            tree(
                l,
                SIDEBAR.x,
                top + (first_child as u32 * SIDE_ROW) as i32,
                children,
            );
        },
    );
    if dir.cloud.is_some() {
        cloud(ui, l);
        return;
    }
    let details = MediaDetails::new(&ui.snapshot);
    let items = media_rows(ui, &details, kind);
    l.panel(CONTENT, p.panel, 8.0, None);
    if items.is_empty() {
        empty(l, CONTENT, "Empty Folder");
        return;
    }
    let list = Rect::new(CONTENT.x, CONTENT.y, CONTENT.width - STRIP, CONTENT.height);
    l.scroll(
        ListId::DirectoryItems,
        list,
        items.len() as u32 * ITEM_ROW,
        ui.offset(ListId::DirectoryItems),
        |l, top| {
            for (position, (index, entry)) in items.iter().enumerate() {
                let y = top + (position as u32 * ITEM_ROW) as i32;
                item_row(
                    l,
                    Rect::new(list.x, y, list.width, ITEM_ROW),
                    ItemRow {
                        control: match dir.multi {
                            Some(_) => Control::Library(LibraryControl::Check(u64::from(*index))),
                            None => Control::Library(LibraryControl::Media(*index)),
                        },
                        title: &entry.name,
                        sort_text: media_sort_text(ui, &details, entry),
                        subtitle: (details.get(&entry.value))
                            .map(|item| {
                                if ui.directory.media_sort.key == super::directory::SortKey::Date {
                                    item.date.clone()
                                } else {
                                    item.author.clone()
                                }
                            })
                            .filter(|text| !text.is_empty())
                            .unwrap_or_else(|| entry.folder.label().to_owned()),
                        selected: dir.selected_media == Some(*index),
                        menu: Some(Control::Library(LibraryControl::MediaMenu(*index))),
                        checked: checked(ui, u64::from(*index)),
                    },
                );
            }
        },
    );
    media_strip(ui, &details, l, kind);
}

/// The open cloud folder: product rows (name, author) in the order the cloud lists them,
/// 25 per page. A page request shows a busy ring under the rows (stock `BusyIndicatorItem`
/// in `FolderTable`; I: its look). An empty folder shows "Empty Folder"; a failed first
/// page shows its reason (O).
pub(super) fn cloud(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    l.panel(CONTENT, p.panel, 8.0, None);
    let folder = ui.open_cloud_folder();
    let rows = folder.map_or(&[][..], |folder| &folder.rows);
    // No folder: no page was asked, or the request was refused before Services (a bring-up
    // without `--services`, review r28 A5). Services creates the folder when it asks.
    let busy = folder.is_some_and(|folder| folder.fetching);
    if rows.is_empty() {
        match folder.and_then(|folder| folder.error.as_deref()) {
            _ if busy => busy_ring(l, Rect::new(CONTENT.x, CONTENT.y + 24, CONTENT.width, 48)),
            Some(error) => l.paragraph(
                Rect::new(CONTENT.x + 24, CONTENT.y + 40, CONTENT.width - 48, 80),
                error,
                BODY,
                p.faint,
            ),
            None => empty(l, CONTENT, "Empty Folder"),
        }
        return;
    }
    let tail = if busy { ITEM_ROW } else { 0 };
    let checked = ui.directory.multi.as_ref();
    l.scroll(
        ListId::CloudItems,
        CONTENT,
        rows.len() as u32 * ITEM_ROW + tail,
        ui.offset(ListId::CloudItems),
        |l, top| {
            for (index, row) in rows.iter().enumerate() {
                let y = top + (index as u32 * ITEM_ROW) as i32;
                item_row(
                    l,
                    Rect::new(CONTENT.x, y, CONTENT.width, ITEM_ROW),
                    ItemRow {
                        control: Control::Library(match checked {
                            Some(_) => LibraryControl::Check(index as u64),
                            None => LibraryControl::CloudItem(index as u16),
                        }),
                        title: &row.name,
                        sort_text: "",
                        subtitle: row.author.clone(),
                        selected: false,
                        menu: None,
                        checked: checked.map(|checked| checked.contains(&(index as u64))),
                    },
                );
            }
            if busy {
                let y = top + (rows.len() as u32 * ITEM_ROW) as i32;
                busy_ring(l, Rect::new(CONTENT.x, y, CONTENT.width, ITEM_ROW));
            }
        },
    );
}

fn busy_ring(l: &mut Layout, area: Rect) {
    let p = l.palette;
    let (cx, cy) = area.center();
    l.push(
        Rect::new(cx - 14, cy - 14, 28, 28),
        "",
        Look::Arc {
            width: 3.0,
            start: 0.0,
            sweep: 270.0,
            color: p.accent,
        },
    );
}

/// Presets List View (any sort but Banks, manual §5): one flat list by name.
pub(super) fn preset_list(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let rows = preset_rows(ui);
    let size = super::directory::bank_size(&ui.snapshot);
    l.panel(CONTENT, p.panel, 8.0, None);
    if rows.is_empty() {
        empty(l, CONTENT, "Empty Folder");
        return;
    }
    let list = Rect::new(CONTENT.x, CONTENT.y, CONTENT.width - STRIP, CONTENT.height);
    let multi = ui.directory.multi.is_some();
    l.scroll(
        ListId::DirectoryItems,
        list,
        rows.len() as u32 * ITEM_ROW,
        ui.offset(ListId::DirectoryItems),
        |l, top| {
            for (position, preset) in rows.iter().enumerate() {
                let y = top + (position as u32 * ITEM_ROW) as i32;
                item_row(
                    l,
                    Rect::new(list.x, y, list.width, ITEM_ROW),
                    ItemRow {
                        control: if multi {
                            Control::Library(LibraryControl::Check(preset.id))
                        } else {
                            Control::SelectPreset(preset.id)
                        },
                        title: &preset.title,
                        sort_text: preset_sort_text(ui, preset),
                        subtitle: slot_label(preset.id, size),
                        selected: ui.snapshot.preset_id == Some(preset.id)
                            || ui.directory.revealed == Some(preset.id),
                        menu: Some(Control::PresetMenu(preset.id)),
                        checked: checked(ui, preset.id),
                    },
                );
            }
        },
    );
    directory_strip(l, CONTENT);
}

/// "Favorites and Recent" (shot 033), projected from all six stock list files.
pub(super) fn favorites(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let dir = &ui.directory;
    l.panel(SIDEBAR, p.panel, 8.0, None);
    let kinds = [
        (ItemKind::Preset, "Presets", Glyph::Presets),
        (ItemKind::Capture, "Neural Captures", Glyph::Captures),
        (ItemKind::Ir, "Impulse Responses", Glyph::Irs),
    ];
    for (index, (kind, label, mark)) in kinds.into_iter().enumerate() {
        sidebar_row(
            l,
            Rect::new(
                SIDEBAR.x,
                SIDEBAR.y + (index as u32 * SIDE_ROW) as i32,
                SIDEBAR.width,
                SIDE_ROW,
            ),
            SideRow {
                selected: dir.favorites == kind,
                ..SideRow::new(
                    Control::Library(LibraryControl::Kind(kind)),
                    label,
                    Mark::Glyph(mark),
                )
            },
        );
    }
    l.panel(CONTENT, p.panel, 8.0, None);
    let rows = directory_favorites(ui);
    if rows.is_empty() {
        empty(
            l,
            CONTENT,
            if dir.recent {
                "No recent items"
            } else {
                "No favorites yet"
            },
        );
        return;
    }
    let list = Rect::new(CONTENT.x, CONTENT.y, CONTENT.width - STRIP, CONTENT.height);
    l.scroll(
        ListId::Favorites,
        list,
        rows.len() as u32 * ITEM_ROW,
        ui.offset(ListId::Favorites),
        |l, top| {
            for (position, preset) in rows.iter().enumerate() {
                let y = top + (position as u32 * ITEM_ROW) as i32;
                let row = Rect::new(list.x, y, list.width, ITEM_ROW);
                l.control(
                    Rect::new(row.x, row.y, 330, ITEM_ROW),
                    Control::Library(LibraryControl::Favorite(position as u64)),
                    true,
                    preset.name.clone(),
                    Look::Area,
                );
                let subtitle = if dir.media_sort.key == super::directory::SortKey::Date {
                    &preset.date
                } else {
                    &preset.author
                };
                // Stock 033 centres a name without a second line in the row.
                let name = if subtitle.is_empty() {
                    Rect::new(row.x + 16, row.y, 286, ITEM_ROW)
                } else {
                    Rect::new(row.x + 16, row.y + 8, 286, 22)
                };
                l.text(name, preset.name.clone(), BODY, p.text, Align::Left);
                if !subtitle.is_empty() {
                    l.text(
                        Rect::new(row.x + 16, row.y + 32, 286, 20),
                        subtitle,
                        CAPTION,
                        p.muted,
                        Align::Left,
                    );
                }
                // Stock 033 shows no sort initial in favorites rows (only the strip).
                // The app reports an explicit unsupported audition until DSP save/restore is available.
                for (x, control, enabled, label, mark) in
                    [
                        (
                            608,
                            LibraryControl::Audition(position as u64),
                            true,
                            "AUDITION",
                            Glyph::Binoculars,
                        ),
                        (
                            680,
                            LibraryControl::Unfavorite(position as u64),
                            !dir.recent
                                || ui.snapshot.directory.favorites.iter().any(|item| {
                                    item.kind == preset.kind && item.value == preset.value
                                }),
                            "REMOVE FROM FAVORITES",
                            Glyph::BrokenHeart,
                        ),
                    ]
                {
                    glyph_button(
                        l,
                        Rect::new(x, y + 8, 66, 44),
                        Control::Library(control),
                        enabled,
                        label,
                        mark,
                        p.surface,
                    );
                }
                l.push(
                    Rect::new(row.x + 16, row.bottom() - 1, row.width - 16, 1),
                    "",
                    Look::Fill(p.surface),
                );
            }
        },
    );
    directory_strip(l, CONTENT);
}

/// Plugin roots; an open plugin shows its folders (Main Artist, Artists, Neural DSP,
/// User) and the presets of the selected folder. A preset row auditions it.
pub(super) fn plugins(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    l.panel(SIDEBAR, p.panel, 8.0, None);
    let open = ui
        .directory
        .plugin_folder
        .and_then(|index| Some(index).zip(ui.snapshot.directory.plugins.get(index as usize)));
    if let Some((index, folder)) = open {
        let rows = folder.children.len() as u32 + 1;
        l.scroll(
            ListId::Setlists,
            SIDEBAR,
            rows * SIDE_ROW,
            ui.offset(ListId::Setlists),
            |l, top| {
                sidebar_row(
                    l,
                    Rect::new(SIDEBAR.x, top, SIDEBAR.width, SIDE_ROW),
                    SideRow {
                        selected: ui.directory.plugin_child.is_none(),
                        ..SideRow::new(
                            Control::Library(LibraryControl::PluginFolder(index)),
                            &folder.name,
                            Mark::Glyph(Glyph::Plugins),
                        )
                    },
                );
                for (position, name) in folder.children.iter().enumerate() {
                    sidebar_row(
                        l,
                        Rect::new(
                            SIDEBAR.x,
                            top + ((position as u32 + 1) * SIDE_ROW) as i32,
                            SIDEBAR.width,
                            SIDE_ROW,
                        ),
                        SideRow {
                            child: true,
                            selected: ui.directory.plugin_child == Some(position as u16),
                            ..SideRow::new(
                                Control::Library(LibraryControl::PluginChild(position as u16)),
                                name,
                                Mark::Icon(Icon::Folder),
                            )
                        },
                    );
                }
                tree(l, SIDEBAR.x, top + SIDE_ROW as i32, folder.children.len());
            },
        );
        let items = super::directory::plugin_presets(ui);
        l.panel(CONTENT, p.panel, 8.0, None);
        if items.is_empty() {
            empty(l, CONTENT, "Empty Folder");
            return;
        }
        let list = Rect::new(CONTENT.x, CONTENT.y, CONTENT.width - STRIP, CONTENT.height);
        l.scroll(
            ListId::DirectoryItems,
            list,
            items.len() as u32 * ITEM_ROW,
            ui.offset(ListId::DirectoryItems),
            |l, top| {
                for (position, (index, item)) in items.iter().enumerate() {
                    let y = top + (position as u32 * ITEM_ROW) as i32;
                    item_row(
                        l,
                        Rect::new(list.x, y, list.width, ITEM_ROW),
                        ItemRow {
                            control: Control::Library(LibraryControl::PluginPreset(*index)),
                            title: &item.name,
                            sort_text: match ui.directory.media_sort.key {
                                SortKey::Author => &item.author,
                                SortKey::Instrument => item.instrument.label(),
                                SortKey::Date => &item.date,
                                _ => &item.name,
                            },
                            // Author or preferred instrument (spec §1); the folder
                            // when the sidecar has neither.
                            subtitle: Some(match ui.directory.media_sort.key {
                                SortKey::Instrument => item.instrument.label(),
                                SortKey::Date => &item.date,
                                _ => &item.author,
                            })
                            .filter(|text| !text.is_empty())
                            .unwrap_or(&item.folder)
                            .to_owned(),
                            selected: false,
                            menu: None,
                            checked: None,
                        },
                    );
                }
            },
        );
        directory_strip(l, CONTENT);
        return;
    }
    // Unlicensed plugins are left out; a row keeps its folder's index in `plugins`.
    let unlicensed = &ui.snapshot.unlicensed_plugins;
    let folders: Vec<_> = (ui.snapshot.directory.plugins.iter().enumerate())
        .filter(|(_, folder)| {
            !folder
                .plugin
                .is_some_and(|plugin| unlicensed.contains(&plugin))
        })
        .collect();
    if folders.is_empty() {
        empty(l, SIDEBAR, "No plugins installed");
    } else {
        l.scroll(
            ListId::Setlists,
            SIDEBAR,
            folders.len() as u32 * SIDE_ROW,
            ui.offset(ListId::Setlists),
            |l, top| {
                for (row, &(index, folder)) in folders.iter().enumerate() {
                    sidebar_row(
                        l,
                        Rect::new(
                            SIDEBAR.x,
                            top + (row as u32 * SIDE_ROW) as i32,
                            SIDEBAR.width,
                            SIDE_ROW,
                        ),
                        SideRow {
                            value: folder.presets.len().to_string(),
                            ..SideRow::new(
                                Control::Library(LibraryControl::PluginFolder(index as u16)),
                                &folder.name,
                                Mark::Icon(Icon::Folder),
                            )
                        },
                    );
                }
            },
        );
    }
    l.panel(CONTENT, p.panel, 8.0, None);
    glyph(
        l,
        Rect::new(CONTENT.x, CONTENT.y, CONTENT.width, CONTENT.height),
        Glyph::Plugins,
        p.faint,
    );
}

/// Search (shot 024): recent searches before a query, then the matches of the selected
/// type under one section bar. A preset result loads the preset; a capture/IR result opens
/// its folder with the row highlighted.
pub(super) fn search(ui: &UiState, l: &mut Layout, kind: ItemKind) {
    let p = l.palette;
    l.panel(RESULTS, p.panel, 8.0, None);
    let bar = Rect::new(RESULTS.x, RESULTS.y, RESULTS.width, SECTION_BAR);
    let section = |l: &mut Layout, title: &str| {
        l.panel(bar, p.section, 8.0, None);
        l.push(
            Rect::new(bar.x, bar.y + 8, bar.width, bar.height - 8),
            "",
            Look::Fill(p.section),
        );
        l.text(
            Rect::new(bar.x + 16, bar.y, bar.width - 64, bar.height),
            title,
            SMALL_BOLD,
            p.muted,
            Align::Left,
        );
        l.push(
            Rect::new(bar.right() - 40, bar.y + 10, 24, 24),
            "",
            Look::IconButton {
                icon: Icon::Down,
                fill: p.section,
            },
        );
    };
    let body = Rect::new(
        RESULTS.x,
        bar.bottom(),
        RESULTS.width,
        RESULTS.height - SECTION_BAR,
    );
    if ui.library_query.is_empty() {
        section(l, "RECENTLY SEARCHED");
        let recent = &ui.directory.recent_searches;
        if recent.is_empty() {
            empty(l, body, "No recent searches");
        }
        for (index, query) in recent.iter().enumerate() {
            crate::widgets::list_row(
                l,
                Rect::new(
                    body.x,
                    body.y + (index as u32 * SIDE_ROW) as i32,
                    body.width,
                    SIDE_ROW,
                ),
                Control::Library(LibraryControl::RecentSearch(index as u8)),
                true,
                query.clone(),
                Some(Icon::Search),
                false,
                None,
            );
        }
        return;
    }
    section(l, "DEVICE DIRECTORIES");
    let rows = search_rows(ui, kind);
    if rows.is_empty() {
        let text = match kind {
            ItemKind::Preset => "No matching Presets found.",
            ItemKind::Capture => "No matching Neural Captures found.",
            ItemKind::Ir => "No matching Impulse Responses found.",
        };
        empty(l, body, text);
        return;
    }
    let size = super::directory::bank_size(&ui.snapshot);
    let setlist = ui
        .snapshot
        .active_setlist
        .and_then(|id| ui.snapshot.setlists.iter().find(|s| s.id == id))
        .map_or("", |setlist| setlist.title.as_str());
    l.scroll(
        ListId::PresetResults,
        body,
        rows.len() as u32 * ITEM_ROW,
        ui.offset(ListId::PresetResults),
        |l, top| {
            for (position, (id, name)) in rows.iter().enumerate() {
                let y = top + (position as u32 * ITEM_ROW) as i32;
                let (control, subtitle, menu, selected) = match kind {
                    ItemKind::Preset => match ui.snapshot.directory.presets.get(*id as usize) {
                        Some(preset) => (
                            Control::Library(LibraryControl::RevealPreset(*id as u32)),
                            format!(
                                "{} · {}",
                                setlist_title(ui, preset.setlist),
                                slot_label(preset.slot, size)
                            ),
                            Control::Library(LibraryControl::RevealPreset(*id as u32)),
                            false,
                        ),
                        None => (
                            Control::SelectPreset(*id),
                            format!("{setlist} · {}", slot_label(*id, size)),
                            Control::PresetMenu(*id),
                            ui.snapshot.preset_id == Some(*id),
                        ),
                    },
                    _ => {
                        let index = *id as u16;
                        (
                            Control::Library(LibraryControl::Reveal(index)),
                            ui.snapshot.media[usize::from(index)]
                                .folder
                                .label()
                                .to_owned(),
                            Control::Library(LibraryControl::MediaMenu(index)),
                            false,
                        )
                    }
                };
                item_row(
                    l,
                    Rect::new(body.x, y, body.width, ITEM_ROW),
                    ItemRow {
                        control,
                        title: name,
                        sort_text: name,
                        subtitle,
                        selected,
                        menu: Some(menu),
                        checked: None,
                    },
                );
            }
        },
    );
}

/// A 66×44 header or row button with an owned glyph.
pub(super) fn glyph_button(
    l: &mut Layout,
    b: Rect,
    control: Control,
    enabled: bool,
    label: &str,
    mark: Glyph,
    fill: Rgb,
) {
    let p = l.palette;
    l.panel(b, fill, 8.0, None);
    l.control(b, control, enabled, label, Look::Area);
    glyph(l, b, mark, if enabled { p.text } else { p.faint });
}

fn setlist_title(ui: &UiState, id: u64) -> &str {
    let setlists = &ui.snapshot.setlists;
    setlists
        .iter()
        .find(|setlist| setlist.id == id)
        .map_or("", |setlist| setlist.title.as_str())
}

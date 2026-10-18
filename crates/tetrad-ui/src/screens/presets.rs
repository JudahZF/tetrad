//! The Directory (manual §5, spec `w4-ui-spec-directory`): its own header (category
//! drop-down, per-category icon buttons, ✓), then the category body. Presets show the
//! setlists, the Bank View (two columns of 52×52 bank buttons beside the bank's presets)
//! or, with any other sort, the List View. Multi Select, sort, filter, search and the ⋮
//! menus open as Directory pop-ups. State and actions: `directory.rs`.

use tetrad_assets::{
    ACCENT, DANGER, FAINT, Icon, IconSize, MUTED, ON_ACCENT, PANEL, Rgb, SURFACE, TEXT,
};

use super::directory::{Category, DirectoryDelete, Popup, bank_size, slot_label, sort_keys};
use super::directory_glyph::{Glyph, glyph};
use super::directory_lists::{
    self as lists, Mark, SIDE_ROW, SIDEBAR, SideRow, check_box, glyph_button, sidebar_row,
};
use crate::layout::{
    BODY, BODY_BOLD, ButtonStyle, CAPTION, LARGE_BOLD, Layout, ListId, Look, MEDIUM,
};
use crate::render::{Align, HEIGHT};
use crate::*;

const ROW: u32 = 51;
const BANK: u32 = 52;
const BANK_PITCH: i32 = 60;
/// Bank column (x 272 and 332) and the preset list of the Bank View (shot 054).
const BANKS: Rect = Rect::new(272, 60, 112, 412);
const LIST: Rect = Rect::new(393, 60, 399, 412);
const DROPDOWN: Rect = Rect::new(8, 8, 256, 44);
pub(super) const WIDE: Rect = Rect::new(272, 8, 98, 44);
/// Right-aligned 66×44 header icon buttons, pitch 74, ending at SEARCH (x 610).
const ICON_RIGHT: i32 = 610;
const ICON_PITCH: i32 = 74;

pub(super) fn layout(ui: &UiState, l: &mut Layout) {
    if let Some(edit) = &ui.capture.edit {
        super::capture::edit_page(l, edit);
        return;
    }
    let dir = &ui.directory;
    match (dir.search, dir.multi.is_some()) {
        (Some(kind), _) => {
            search_header(ui, l, kind);
            lists::search(ui, l, kind);
        }
        (None, multi) => {
            if dir.download.as_ref().is_some_and(|pick| !pick.chosen) {
                setlist_pick_header(l);
            } else if multi {
                multi_header(ui, l);
            } else {
                header(ui, l);
            }
            match dir.category {
                Category::Favorites => lists::favorites(ui, l),
                Category::Presets => {
                    setlists(ui, l);
                    if dir.cloud.is_some() {
                        lists::cloud(ui, l);
                    } else if dir.bank_view() {
                        banks(ui, l);
                        presets(ui, l);
                    } else {
                        lists::preset_list(ui, l);
                    }
                }
                Category::Captures | Category::Irs => lists::media(ui, l, dir.category),
                Category::Plugins => lists::plugins(ui, l),
            }
        }
    }
    popup(ui, l);
}

fn category_mark(category: Category) -> Glyph {
    match category {
        Category::Favorites => Glyph::Heart,
        Category::Presets => Glyph::Presets,
        Category::Captures => Glyph::Captures,
        Category::Irs => Glyph::Irs,
        Category::Plugins => Glyph::Plugins,
    }
}

/// 1 px muted header divider (x ≈379 and ≈685, y 18–42).
pub(super) fn divider(l: &mut Layout, x: i32) {
    l.push(Rect::new(x, 18, 1, 24), "", Look::Fill(FAINT));
}

fn done(l: &mut Layout) {
    divider(l, 685);
    l.control(
        crate::widgets::CLOSE_BUTTON,
        Control::Library(LibraryControl::Done),
        true,
        "DONE",
        Look::IconButton {
            icon: Icon::Check,
            fill: PANEL,
        },
    );
}

/// Category drop-down: mark, label and ▼ (shots 054, 107). The media picker shares it
/// and draws its own mark into the 24 px box.
pub(super) fn dropdown(
    l: &mut Layout,
    control: Control,
    mark: impl FnOnce(&mut Layout, Rect),
    label: &str,
) {
    l.panel(DROPDOWN, PANEL, 8.0, None);
    l.control(DROPDOWN, control, true, label, Look::Area);
    mark(l, Rect::new(24, DROPDOWN.y, 24, DROPDOWN.height));
    l.text(
        Rect::new(62, DROPDOWN.y, 170, DROPDOWN.height),
        label,
        BODY,
        TEXT,
        Align::Left,
    );
    glyph(
        l,
        Rect::new(236, DROPDOWN.y, 24, DROPDOWN.height),
        Glyph::Caret,
        TEXT,
    );
}

/// One header icon button: a Directory glyph, or the search icon (`None`).
pub(super) struct HeaderButton<'a> {
    pub control: Control,
    pub label: &'a str,
    pub mark: Option<Glyph>,
    pub enabled: bool,
    pub fill: tetrad_assets::Rgb,
}

/// The 66×44 icon buttons, right-aligned to end at SEARCH (x 610, pitch 74).
pub(super) fn icon_buttons(l: &mut Layout, buttons: &[HeaderButton]) {
    let first = ICON_RIGHT - (buttons.len() as i32 - 1) * ICON_PITCH;
    for (index, button) in buttons.iter().enumerate() {
        let b = Rect::new(first + index as i32 * ICON_PITCH, 8, 66, 44);
        match button.mark {
            Some(mark) => glyph_button(
                l,
                b,
                button.control,
                button.enabled,
                button.label,
                mark,
                button.fill,
            ),
            None => l.control(
                b,
                button.control,
                button.enabled,
                button.label,
                Look::IconButton {
                    icon: Icon::Search,
                    fill: button.fill,
                },
            ),
        }
    }
}

fn header(ui: &UiState, l: &mut Layout) {
    let dir = &ui.directory;
    let category = dir.category;
    dropdown(
        l,
        Control::Library(LibraryControl::Category),
        |l, area| glyph(l, area, category_mark(category), TEXT),
        category.label(),
    );
    // Wide slot: Favorites tabs, UPLOAD or REFRESH (cloud features, not built).
    match category {
        Category::Favorites => {
            for (x, width, recent, mark, label) in [
                (272, 84, false, Glyph::Heart, "FAVORITES"),
                (362, 82, true, Glyph::Clock, "RECENT"),
            ] {
                let selected = dir.recent == recent;
                // The selected tab joins the panel below.
                let tab = Rect::new(x, 8, width, if selected { 60 } else { 44 });
                l.panel(tab, if selected { PANEL } else { SURFACE }, 8.0, None);
                let face = Rect::new(x, 8, width, 44);
                l.control(
                    face,
                    Control::Library(LibraryControl::Recent(recent)),
                    true,
                    label,
                    Look::Area,
                );
                glyph(l, face, mark, TEXT);
            }
        }
        Category::Captures => {
            glyph_button(
                l,
                WIDE,
                Control::Library(LibraryControl::Upload),
                true,
                "UPLOAD",
                Glyph::CloudUp,
                SURFACE,
            );
            divider(l, 379);
        }
        Category::Irs | Category::Plugins => {
            l.control(
                WIDE,
                Control::Library(LibraryControl::Refresh),
                true,
                "REFRESH",
                Look::IconButton {
                    icon: Icon::Sync,
                    fill: SURFACE,
                },
            );
            divider(l, 379);
        }
        Category::Presets => {}
    }
    let buttons: &[(LibraryControl, &str, Option<Glyph>)] = match category {
        Category::Captures => &[
            (
                LibraryControl::MultiSelect,
                "MULTI SELECT",
                Some(Glyph::MultiSelect),
            ),
            (LibraryControl::Filter, "FILTER", Some(Glyph::Filter)),
            (LibraryControl::Sort, "SORT", Some(Glyph::Sort)),
            (LibraryControl::Search, "SEARCH", None),
        ],
        // An open plugin sorts its presets (Preferred Instrument, I: placement).
        Category::Plugins if dir.plugin_folder.is_some() => &[
            (LibraryControl::Sort, "SORT", Some(Glyph::Sort)),
            (LibraryControl::Search, "SEARCH", None),
        ],
        Category::Plugins => &[(LibraryControl::Search, "SEARCH", None)],
        _ => &[
            (
                LibraryControl::MultiSelect,
                "MULTI SELECT",
                Some(Glyph::MultiSelect),
            ),
            (LibraryControl::Sort, "SORT", Some(Glyph::Sort)),
            (LibraryControl::Search, "SEARCH", None),
        ],
    };
    let buttons: Vec<HeaderButton> = buttons
        .iter()
        .map(|&(control, label, mark)| HeaderButton {
            control: Control::Library(control),
            label,
            mark,
            // Favorites have no bulk action or sort key the projection supports (gap).
            enabled: category != Category::Favorites
                || !matches!(control, LibraryControl::MultiSelect | LibraryControl::Sort),
            fill: if control == LibraryControl::Filter
                && (dir.made_by_me || !dir.device_types.is_empty() || dir.version.stock().is_some())
            {
                tetrad_assets::ACCENT_FILL
            } else {
                SURFACE
            },
        })
        .collect();
    icon_buttons(l, &buttons);
    done(l);
}

/// Multi Select header (I: no stock screenshot): ✕ and the count, then the bulk actions.
/// Copy and move (hold COPY) take the checked presets to a setlist, or link the checked
/// captures/IRs into a folder; delete removes them.
/// PASTE is live with a clipboard of the open kind and a place to paste into: an open
/// setlist for presets, an open writable folder for captures and IRs.
fn paste_ready(ui: &UiState) -> bool {
    use super::directory::Clipboard;
    let dir = &ui.directory;
    match (&dir.clipboard, dir.category.media()) {
        (Some(Clipboard::Presets { ids, .. }), None) => {
            !ids.is_empty() && ui.snapshot.active_setlist.is_some()
        }
        (Some(Clipboard::Media(ids)), Some(_)) => {
            let writable = |id| {
                (ui.snapshot.directory.folders.iter())
                    .any(|folder| folder.id == id && !folder.read_only && !folder.root)
            };
            !ids.is_empty() && dir.database_folder.is_some_and(writable)
        }
        _ => false,
    }
}

pub(super) fn multi_header(ui: &UiState, l: &mut Layout) {
    if let Some(paste) = ui.pasting() {
        return paste_bar(ui, l, paste);
    }
    let dir = &ui.directory;
    let count = dir.multi.as_ref().map_or(0, |checked| checked.len());
    let presets = dir.category == Category::Presets;
    l.control(
        Rect::new(8, 8, 66, 44),
        Control::Library(LibraryControl::MultiSelect),
        true,
        "CANCEL",
        Look::IconButton {
            icon: Icon::Close,
            fill: SURFACE,
        },
    );
    l.text(
        Rect::new(86, 8, 178, 44),
        format!("{count} selected"),
        BODY,
        MUTED,
        Align::Left,
    );
    // In a cloud folder only DOWNLOAD acts on the checked rows.
    let cloud = dir.cloud.is_some();
    let some = count > 0 && !cloud;
    let download = count > 0
        && cloud
        && matches!(
            dir.category,
            Category::Captures | Category::Irs | Category::Presets
        );
    // Captures and IRs copy or move as folder links (manual "FOLDERS & NESTED NAVIGATION").
    let media = dir.category.media().is_some();
    let links = some && media && !super::directory::link_targets(ui).is_empty();
    let actions: [(BulkAction, &str, Result<Icon, Glyph>, bool); 8] = [
        (
            BulkAction::Copy,
            "COPY",
            Ok(Icon::Copy),
            (presets && some) || links,
        ),
        (
            BulkAction::Paste,
            "PASTE",
            Ok(Icon::Paste),
            !cloud && paste_ready(ui),
        ),
        (
            BulkAction::Move,
            "MOVE",
            Ok(Icon::Forward),
            (presets && some) || links,
        ),
        (
            BulkAction::Favorite,
            "FAVORITE",
            Err(Glyph::Heart),
            (presets || media) && some,
        ),
        // re-r44 §1: a preset or capture folder (`0x77e6b0`; stock has no IR upload) and a
        // usable cloud (`0x7166d0`). The paste bar replaces this row while keys are copied.
        (
            BulkAction::Upload,
            "UPLOAD",
            Err(Glyph::CloudUp),
            some && (presets || dir.category == Category::Captures) && ui.snapshot.cloud_ready(),
        ),
        (
            BulkAction::Download,
            "DOWNLOAD",
            Ok(Icon::Download),
            download,
        ),
        (
            BulkAction::Edit,
            "EDIT",
            Ok(Icon::Pencil),
            (presets || media) && some,
        ),
        (
            BulkAction::Delete,
            "DELETE",
            Ok(Icon::Trash),
            (presets || media) && some,
        ),
    ];
    for (index, (action, label, mark, enabled)) in actions.into_iter().enumerate() {
        let b = Rect::new(272 + index as i32 * 52, 8, 48, 44);
        let control = Control::Library(LibraryControl::Bulk(action));
        match mark {
            Ok(icon) => l.control(
                b,
                control,
                enabled,
                label,
                Look::IconButton {
                    icon,
                    fill: SURFACE,
                },
            ),
            Err(mark) => glyph_button(l, b, control, enabled, label, mark, SURFACE),
        }
    }
    done(l);
}

/// The Multi Select bar while a PASTE waits for its slots (status `+0x210`, `0x7831a8`): the
/// mode line, then "%1 Presets collected" (green) or "%1 Presets will be overwritten" (red
/// `#ff2727`) for the occupied target slots; PASTE pastes. I: the bar layout and the manual
/// mode line (stock sets a line only for the consecutive mode).
fn paste_bar(ui: &UiState, l: &mut Layout, paste: &super::directory::PasteSlots) {
    l.control(
        Rect::new(8, 8, 66, 44),
        Control::Library(LibraryControl::MultiSelect),
        true,
        "CANCEL",
        Look::IconButton {
            icon: Icon::Close,
            fill: SURFACE,
        },
    );
    let count = ui.pasted_count();
    let targets = paste.targets(count);
    let verb = ui.paste_verb();
    let overwritten = targets.iter().filter(|slot| ui.slot_filled(**slot)).count();
    let mode = match paste {
        super::directory::PasteSlots::Manual(_) => "Choose each slot manually",
        super::directory::PasteSlots::Consecutive(_) => {
            &format!("{verb} consecutively from the first chosen slot onwards")
        }
    };
    l.text(Rect::new(86, 6, 500, 24), mode, BODY, TEXT, Align::Left);
    let download = ui.directory.download.is_some();
    // I: a download shows the PASTE line; DOWNLOAD then asks before it overwrites.
    let (status, color) = match overwritten {
        0 => (format!("{count} Presets collected"), ACCENT),
        n => (format!("{n} Presets will be overwritten"), DANGER),
    };
    l.text(
        Rect::new(86, 30, 500, 22),
        status,
        CAPTION,
        color,
        Align::Left,
    );
    l.button(
        Rect::new(590, 8, 88, 44),
        Control::Library(LibraryControl::Bulk(BulkAction::Paste)),
        !targets.is_empty(),
        if download { "DOWNLOAD" } else { "PASTE" },
        ButtonStyle::Primary,
    );
    done(l);
}

/// DOWNLOAD of cloud presets, Directory state `0x1b`: the header "Choose destination
/// Setlist:" while a setlist row tap picks the target. I: the header layout (CANCEL as the
/// Multi Select one).
fn setlist_pick_header(l: &mut Layout) {
    l.control(
        Rect::new(8, 8, 66, 44),
        Control::Back,
        true,
        "CANCEL",
        Look::IconButton {
            icon: Icon::Close,
            fill: SURFACE,
        },
    );
    l.text(
        Rect::new(86, 8, 400, 44),
        "Choose destination Setlist:",
        BODY_BOLD,
        TEXT,
        Align::Left,
    );
}

/// Search header (shot 024): the query field, three counted type tabs, SORT and ✓.
fn search_header(ui: &UiState, l: &mut Layout, kind: ItemKind) {
    let field = Rect::new(8, 8, 184, 44);
    l.panel(field, SURFACE, 8.0, None);
    l.control(
        Rect::new(field.x, field.y, field.width - 40, field.height),
        Control::Library(LibraryControl::Search),
        true,
        ui.library_query.clone(),
        Look::Area,
    );
    l.push(
        Rect::new(field.x + 4, field.y + 2, 40, 40),
        "",
        Look::IconButton {
            icon: Icon::Search,
            fill: SURFACE,
        },
    );
    l.text(
        Rect::new(field.x + 44, field.y, field.width - 88, field.height),
        ui.library_query.clone(),
        BODY,
        TEXT,
        Align::Left,
    );
    l.control(
        Rect::new(field.right() - 40, field.y, 40, field.height),
        Control::Library(LibraryControl::ClearSearch),
        true,
        "CLEAR",
        Look::IconButton {
            icon: Icon::Close,
            fill: SURFACE,
        },
    );
    divider(l, 200);
    let typed = !ui.library_query.is_empty();
    for (index, (tab, mark)) in [
        (ItemKind::Preset, Glyph::Presets),
        (ItemKind::Capture, Glyph::Captures),
        (ItemKind::Ir, Glyph::Irs),
    ]
    .into_iter()
    .enumerate()
    {
        let x = 210 + index as i32 * 104;
        let selected = tab == kind;
        let face = Rect::new(x, 8, 100, 44);
        // The selected tab joins the section bar below it.
        l.panel(
            Rect::new(x, 8, 100, if selected { 60 } else { 44 }),
            if selected {
                tetrad_assets::SECTION
            } else {
                SURFACE
            },
            8.0,
            None,
        );
        let count = lists_count(ui, tab, typed);
        l.control(
            face,
            Control::Library(LibraryControl::Kind(tab)),
            true,
            count.clone(),
            Look::Area,
        );
        let color = if selected { TEXT } else { MUTED };
        glyph(l, Rect::new(x + 10, 8, 32, 44), mark, color);
        l.text(
            Rect::new(x + 44, 8, 52, 44),
            count,
            BODY,
            color,
            Align::Left,
        );
    }
    glyph_button(
        l,
        Rect::new(ICON_RIGHT, 8, 66, 44),
        Control::Library(LibraryControl::Sort),
        typed,
        "SORT",
        Glyph::Sort,
        SURFACE,
    );
    done(l);
}

/// "(15)" tab count; empty before a query.
fn lists_count(ui: &UiState, kind: ItemKind, typed: bool) -> String {
    if typed {
        format!("({})", super::directory::search_rows(ui, kind).len())
    } else {
        String::new()
    }
}

/// Downloads and Cloud Presets (enabled as stock), the setlists with their index and ⋮,
/// then the dimmed "New Setlist" row (shot 054).
fn setlists(ui: &UiState, l: &mut Layout) {
    let s = &ui.snapshot;
    l.panel(SIDEBAR, PANEL, 8.0, None);
    let cloud = ui.directory.cloud;
    let mut rows = vec![
        SideRow {
            selected: cloud == Some(CloudScope::Downloads),
            ..SideRow::new(
                Control::Library(LibraryControl::Downloads),
                "Downloads",
                Mark::Glyph(Glyph::Download),
            )
        },
        SideRow {
            selected: cloud == Some(CloudScope::Cloud),
            ..SideRow::new(
                Control::Library(LibraryControl::Cloud),
                "Cloud Presets",
                Mark::Glyph(Glyph::Cloud),
            )
        },
    ];
    for (index, setlist) in s.setlists.iter().enumerate() {
        rows.push(SideRow {
            selected: cloud.is_none() && s.active_setlist == Some(setlist.id),
            // Factory Presets (index 0) has no menu.
            menu: (index > 0).then_some(Control::Library(LibraryControl::SetlistMenu(setlist.id))),
            ..SideRow::new(
                Control::Library(LibraryControl::Setlist(setlist.id)),
                setlist.title.clone(),
                Mark::Setlist(index),
            )
        });
    }
    rows.push(SideRow {
        dim: true,
        ..SideRow::new(
            Control::Library(LibraryControl::NewSetlist),
            "New Setlist",
            Mark::Glyph(Glyph::FolderPlus),
        )
    });
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
        },
    );
}

/// Two columns of 52×52 bank number buttons; the open bank has no fill and a green number.
fn banks(ui: &UiState, l: &mut Layout) {
    let s = &ui.snapshot;
    let size = bank_size(s);
    let pages = s.presets.len().div_ceil(size);
    let open = ui.directory.bank.min(ui.last_bank());
    l.scroll(
        ListId::Banks,
        BANKS,
        pages.div_ceil(2) as u32 * BANK_PITCH as u32,
        ui.offset(ListId::Banks),
        |l, top| {
            for page in 0..pages {
                let b = Rect::new(
                    BANKS.x + (page % 2) as i32 * BANK_PITCH,
                    top + (page / 2) as i32 * BANK_PITCH,
                    BANK,
                    BANK,
                );
                let selected = page == open;
                if !selected {
                    l.panel(b, PANEL, 8.0, None);
                }
                let number = (s.presets[page * size].id / size as u64 + 1).to_string();
                l.control(
                    b,
                    Control::Library(LibraryControl::Bank(page as u16)),
                    true,
                    number.clone(),
                    Look::Area,
                );
                l.text(
                    b,
                    number,
                    BODY,
                    if selected { ACCENT } else { TEXT },
                    Align::Center,
                );
            }
        },
    );
}

/// The open bank's presets: flat "1A Name" rows with ⋮; the loaded preset is green.
fn presets(ui: &UiState, l: &mut Layout) {
    let s = &ui.snapshot;
    l.panel(LIST, PANEL, 8.0, None);
    if s.presets.is_empty() {
        l.text(
            Rect::new(LIST.x, LIST.y + 40, LIST.width, 30),
            "Empty Folder",
            BODY,
            FAINT,
            Align::Center,
        );
        return;
    }
    let size = bank_size(s);
    let open = ui.directory.bank.min(ui.last_bank());
    let checked = ui.directory.multi.as_ref();
    for (offset, preset) in s.presets.iter().skip(open * size).take(size).enumerate() {
        let b = Rect::new(
            LIST.x,
            LIST.y + (offset as u32 * ROW) as i32,
            LIST.width,
            ROW,
        );
        let label = format!("{} {}", slot_label(preset.id, size), preset.title);
        let selected = s.preset_id == Some(preset.id);
        match checked {
            Some(checked) => {
                crate::widgets::list_row(
                    l,
                    b,
                    Control::Library(LibraryControl::Check(preset.id)),
                    true,
                    label,
                    None,
                    selected,
                    None,
                );
                check_box(l, b.take_right(48, 0).1, checked.contains(&preset.id));
            }
            None => crate::widgets::list_row(
                l,
                b,
                Control::SelectPreset(preset.id),
                true,
                label,
                None,
                selected,
                Some(Control::PresetMenu(preset.id)),
            ),
        }
    }
}

/// The open Directory pop-up, as a popover at its trigger (or a centred dialog).
pub(super) fn popup(ui: &UiState, l: &mut Layout) {
    let dir = &ui.directory;
    let Some(popup) = dir.popup else {
        return;
    };
    let s = &ui.snapshot;
    let library = |control| Control::Library(control);
    let trigger = match popup {
        Popup::Delete(target) => return confirm_directory_delete(ui, l, target),
        Popup::MoveConflict { .. } => return confirm_move(l),
        Popup::DatabaseFolder(id) => library(LibraryControl::DatabaseFolderMenu(id)),
        Popup::MovePreset(id) => Control::PresetMenu(id),
        Popup::Category => library(LibraryControl::Category),
        Popup::Sort => library(LibraryControl::Sort),
        Popup::Filter => library(LibraryControl::Filter),
        Popup::Preset(id) => Control::PresetMenu(id),
        Popup::Setlist(id) => library(LibraryControl::SetlistMenu(id)),
        Popup::Folder(folder) => library(LibraryControl::FolderMenu(folder)),
        Popup::Media(index) => library(LibraryControl::MediaMenu(index)),
        Popup::Target { .. } => library(LibraryControl::Bulk(BulkAction::Copy)),
        Popup::ConfirmDelete => return confirm_delete(ui, l),
        Popup::Paste(choice) => return paste_dialog(ui, l, choice),
    };
    let Some(anchor) = l.anchor(trigger) else {
        return;
    };
    let offset = ui.offset(ListId::Choice);
    let item = |action, label, enabled| (library(LibraryControl::Item(action)), label, enabled);
    match popup {
        Popup::Category => {
            let labels: Vec<&str> = Category::ALL.iter().map(|c| c.label()).collect();
            let current = Category::ALL.iter().position(|c| *c == dir.category);
            crate::widgets::dropdown_list(l, anchor, &labels, current, offset);
        }
        Popup::Sort => {
            let keys = sort_keys(dir);
            let labels: Vec<&str> = keys.iter().map(|key| key.label()).collect();
            let sort = if dir.preset_view() {
                dir.preset_sort
            } else {
                dir.media_sort
            };
            let current = keys.iter().position(|key| *key == sort.key);
            crate::widgets::dropdown_list(l, anchor, &labels, current, offset);
        }
        Popup::MovePreset(_) => {
            let rows: Vec<_> = if s.directory.slots.is_empty() {
                s.presets
                    .iter()
                    .map(|preset| (preset.id, preset.title.as_str()))
                    .collect()
            } else {
                s.directory
                    .slots
                    .iter()
                    .map(|slot| (slot.id, slot.title.as_str()))
                    .collect()
            };
            let labels: Vec<String> = rows
                .iter()
                .map(|(id, name)| format!("{} {name}", slot_label(*id, bank_size(s))))
                .collect();
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            crate::widgets::dropdown_list(l, anchor, &labels, None, offset);
        }
        Popup::Target { .. } if dir.category.media().is_some() => {
            let folders = super::directory::link_targets(ui);
            let labels: Vec<&str> = folders.iter().map(|f| f.name.as_str()).collect();
            crate::widgets::dropdown_list(l, anchor, &labels, None, offset);
        }
        Popup::Target { .. } => {
            let labels: Vec<&str> = s.setlists.iter().map(|s| s.title.as_str()).collect();
            crate::widgets::dropdown_list(l, anchor, &labels, None, offset);
        }
        Popup::Filter => filter_menu(ui, l),
        // The application favorites only the loaded, saved preset (Grid ⋮, manual §5).
        Popup::Preset(id) => {
            let favorite = s.preset_id == Some(id) && s.preset_file;
            crate::widgets::popover_menu(
                l,
                anchor,
                &[
                    item(ItemAction::Load, "Load", true),
                    item(ItemAction::EditName, "Edit Name", true),
                    item(ItemAction::Duplicate, "Duplicate", true),
                    item(
                        ItemAction::Favorite,
                        if favorite && s.favorite {
                            "In your favorites"
                        } else {
                            "Add to Favorites"
                        },
                        favorite,
                    ),
                    item(ItemAction::Move, "Move", true),
                    item(ItemAction::Upload, "Upload", true),
                    item(ItemAction::Delete, "Delete Preset", true),
                ],
                None,
            );
        }
        // Default setlists and library roots remain protected.
        Popup::Setlist(id) => crate::widgets::popover_menu(
            l,
            anchor,
            &[
                item(
                    ItemAction::EditName,
                    "Edit Name",
                    s.setlists.iter().any(|setlist| {
                        setlist.id == id
                            && setlist.title != "My Presets"
                            && setlist.title != "Factory Presets"
                    }),
                ),
                item(
                    ItemAction::Delete,
                    "Delete",
                    s.setlists.iter().any(|setlist| {
                        setlist.id == id
                            && setlist.title != "My Presets"
                            && setlist.title != "Factory Presets"
                    }),
                ),
            ],
            None,
        ),
        Popup::DatabaseFolder(id) => {
            let folder = s.directory.folders.iter().find(|folder| folder.id == id);
            let writable = folder.is_some_and(|folder| !folder.read_only);
            let editable = folder.is_some_and(|folder| !folder.read_only && !folder.root);
            // Manual M817-821 order; the current destination reads "Saving destination".
            let destination = s.directory.saving_destinations.contains(&id);
            crate::widgets::popover_menu(
                l,
                anchor,
                &[
                    item(
                        ItemAction::Duplicate,
                        "Duplicate",
                        super::directory::can_duplicate(&s.directory, id),
                    ),
                    item(ItemAction::NewSubfolder, "New Subfolder", writable),
                    item(ItemAction::EditName, "Edit Name", editable),
                    item(ItemAction::Delete, "Delete", editable),
                    item(
                        ItemAction::SavingDestination,
                        if destination {
                            "Saving destination"
                        } else {
                            "Set as saving destination"
                        },
                        writable,
                    ),
                ],
                destination.then_some(4),
            );
        }
        Popup::Folder(_) => crate::widgets::popover_menu(
            l,
            anchor,
            &[
                item(ItemAction::Duplicate, "Duplicate", false),
                item(ItemAction::NewSubfolder, "New Subfolder", false),
                item(ItemAction::EditName, "Edit Name", false),
                item(ItemAction::Delete, "Delete", false),
                item(
                    ItemAction::SavingDestination,
                    "Set as Saving Destination",
                    false,
                ),
            ],
            None,
        ),
        Popup::Media(index) => {
            let entry = s.media.get(index as usize);
            let writable = entry.is_some_and(|entry| {
                s.directory
                    .media
                    .iter()
                    .any(|item| item.value == entry.value && !item.read_only)
            });
            let favorite = entry.is_some_and(|entry| {
                s.directory
                    .favorites
                    .iter()
                    .any(|item| item.value == entry.value)
            });
            crate::widgets::popover_menu(
                l,
                anchor,
                &[
                    item(
                        ItemAction::Favorite,
                        if favorite {
                            "Remove from favorites"
                        } else {
                            "Add to Favorites"
                        },
                        entry.is_some_and(|entry| {
                            s.directory
                                .media
                                .iter()
                                .any(|item| item.value == entry.value)
                        }),
                    ),
                    item(ItemAction::EditName, "Edit Name", writable),
                    item(ItemAction::Move, "Move", false),
                    // Uploads take user captures (manual "UPLOAD": Presets and Captures).
                    item(
                        ItemAction::Upload,
                        "Upload",
                        writable && entry.is_some_and(|e| e.folder.kind() == MediaKind::Capture),
                    ),
                    item(ItemAction::Delete, "Delete", writable),
                ],
                None,
            );
        }
        Popup::Delete(_) | Popup::MoveConflict { .. } => unreachable!("dialog handled above"),
        Popup::ConfirmDelete | Popup::Paste(_) => unreachable!("dialog handled above"),
    }
}

/// Stock `PastingPopup` text colour.
const PASTE_ROW_TEXT: Rgb = Rgb(0xdd, 0xdd, 0xdd);

/// "Choose pasting option" (`PastingPopup` `0x3a4a28`, text `0x3a5a88` case 0): 472 × 435 at
/// (164, 22), the question at (48, 74), three 365 × 45 option rows at x 53, y 159 / 212 / 265
/// (`#1e1e1e`, text `#dddddd`, `check_black.png` on the checked row), CANCEL and PASTE
/// 157 × 45 at (75, 358) and (240, 358). PASTE reads CONTINUE unless the third row is checked
/// (`0x3a5728`). I: the panel fill and the checked-row look.
fn paste_dialog(ui: &UiState, l: &mut Layout, choice: PasteChoice) {
    let download = ui.directory.download.is_some();
    let at = |x: i32, y: i32, w: u32, h: u32| Rect::new(164 + x, 22 + y, w, h);
    l.overlay();
    l.panel(at(0, 0, 472, 435), PANEL, 8.0, None);
    l.text(
        at(24, 20, 424, 32),
        if download {
            "Choose downloading option"
        } else {
            "Choose pasting option"
        },
        LARGE_BOLD,
        TEXT,
        Align::Center,
    );
    let setlist = (ui.snapshot.setlists.iter())
        .find(|setlist| Some(setlist.id) == ui.snapshot.active_setlist)
        .map_or("", |setlist| setlist.title.as_str());
    let count = ui.pasted_count();
    let verb = ui.paste_verb();
    let lower = verb.to_lowercase();
    // Stock rich text bolds `<b>%4</b>`: the setlist name is its own bold line here (I).
    let line = BODY.size.line_px();
    l.push(
        at(48, 74, 376, 2 * line),
        format!(
            "Please select how you would like to {lower} these {count} presets into the banks in"
        ),
        Look::Paragraph {
            font: BODY,
            color: TEXT,
            align: Align::Center,
        },
    );
    l.text(
        at(48, 74 + 2 * line as i32, 376, line),
        format!("{setlist}:"),
        BODY_BOLD,
        TEXT,
        Align::Center,
    );
    let fits = ui.empty_slots_fit();
    for (row, (option, label, enabled)) in [
        (
            PasteChoice::Manual,
            "Choose each slot manually".into(),
            true,
        ),
        (
            PasteChoice::FromChosen,
            format!("{verb} consecutively from the first chosen slot onwards"),
            true,
        ),
        (
            PasteChoice::FromEmpty,
            format!("{verb} consecutively from the first empty slot onwards"),
            fits,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let b = at(53, 159 + row as i32 * 53, 365, 45);
        let checked = option == choice;
        let (fill, ink) = match (checked, enabled) {
            (true, _) => (TEXT, ON_ACCENT),
            (false, true) => (SURFACE, PASTE_ROW_TEXT),
            (false, false) => (SURFACE, FAINT),
        };
        l.panel(b, fill, 4.0, None);
        let control = Control::Library(LibraryControl::PasteChoice(option));
        l.control(b, control, enabled, label.clone(), Look::Area);
        let lines = if label.len() > 40 { 2 } else { 1 };
        let height = lines * CAPTION.size.line_px();
        l.push(
            Rect::new(
                b.x + 16,
                b.y + (45 - height as i32) / 2,
                b.width - 56,
                height,
            ),
            label,
            Look::Paragraph {
                font: CAPTION,
                color: ink,
                align: Align::Left,
            },
        );
        if checked {
            l.push(
                Rect::new(b.right() - 36, b.y + 12, 20, 20),
                "",
                Look::Glyph {
                    icon: Icon::Check,
                    size: IconSize::Small,
                    color: ON_ACCENT,
                },
            );
        }
    }
    l.button(
        at(75, 358, 157, 45),
        Control::Back,
        true,
        "CANCEL",
        ButtonStyle::Secondary,
    );
    l.button(
        at(240, 358, 157, 45),
        Control::Library(LibraryControl::ConfirmPaste),
        true,
        match choice {
            PasteChoice::FromEmpty if download => "DOWNLOAD",
            PasteChoice::FromEmpty => "PASTE",
            _ => "CONTINUE",
        },
        ButtonStyle::Primary,
    );
}

/// The capture filter (`FilterMenu` `0x7f929c`, rows 231 × 52): at (65, 57) "Made by Me", the
/// grey "Filters" header and the versions; at (300, 57) the device types (the x of the
/// `DEVICE_TYPES` form, `FilterMenuTable` `0x748f68`), scrolled. Both in one pop-up (I).
fn filter_menu(ui: &UiState, l: &mut Layout) {
    const ROW: u32 = 52;
    const WIDTH: u32 = 231;
    let dir = &ui.directory;
    let library = |control| Control::Library(control);
    let row = |l: &mut Layout, at: Rect, control, label: &str, checked: Option<bool>, selected| {
        crate::widgets::list_row(l, at, control, true, label, None, selected, None);
        if let Some(checked) = checked {
            check_box(l, at.take_right(48, 0).1, checked);
        }
    };
    let versions = Rect::new(65, 57, WIDTH, (2 + CaptureVersion::ALL.len() as u32) * ROW);
    let content = CAPTURE_DEVICE_TYPES.len() as u32 * ROW;
    let types = Rect::new(300, 57, WIDTH, content.min(HEIGHT - 57 - 8));
    // One layer for both panels: a tap outside them closes the pop-up.
    l.popover(Rect::new(
        versions.x,
        versions.y,
        (types.right() - versions.x) as u32,
        types.height,
    ));
    l.panel(versions, tetrad_assets::CANVAS, 8.0, None);
    let at =
        |panel: Rect, index: u32| Rect::new(panel.x, panel.y + (index * ROW) as i32, WIDTH, ROW);
    row(
        l,
        at(versions, 0),
        library(LibraryControl::MadeByMe),
        "Made by Me",
        Some(dir.made_by_me),
        false,
    );
    l.text(
        at(versions, 1).inset(16, 0),
        "Filters",
        BODY,
        MUTED,
        Align::Left,
    );
    for (index, version) in CaptureVersion::ALL.into_iter().enumerate() {
        row(
            l,
            at(versions, 2 + index as u32),
            library(LibraryControl::Version(version)),
            version.label(),
            None,
            dir.version == version,
        );
    }
    l.panel(types, tetrad_assets::CANVAS, 8.0, None);
    l.scroll(
        ListId::Choice,
        types,
        content,
        ui.offset(ListId::Choice),
        |l, top| {
            for (code, label) in CAPTURE_DEVICE_TYPES.iter().enumerate() {
                let code = code as u8;
                row(
                    l,
                    Rect::new(types.x, top + i32::from(code) * ROW as i32, WIDTH, ROW),
                    library(LibraryControl::TypeFilter(code)),
                    label,
                    Some(dir.device_types.contains(&code)),
                    false,
                );
            }
        },
    );
}

/// Capture/IR bulk delete (`0x83d208`). In the library root the items are deleted from the
/// library and every folder; in a folder REMOVE unlinks them and DELETE also deletes them
/// from the library (button labels owned, I). Factory items are omitted.
fn confirm_media_delete(ui: &UiState, l: &mut Layout, kind: MediaKind) {
    let checked = ui.directory.multi.clone().unwrap_or_default();
    let items = super::directory::checked_media(&ui.snapshot, &checked);
    let count = items.len();
    let noun = match kind {
        MediaKind::Capture => "Neural Capture",
        MediaKind::Ir => "Impulse Response",
    };
    let folders = &ui.snapshot.directory.folders;
    let library = folders
        .iter()
        .find(|f| f.kind == kind && f.root)
        .map_or("library", |f| f.name.as_str());
    let folder = ui
        .directory
        .database_folder
        .and_then(|id| folders.iter().find(|f| f.id == id));
    let one = count == 1;
    let s = if one { "" } else { "s" };
    // Only the plural strings carry `%4` (singular `0x01204f00`, `0x01204fd8`: review r15 R25).
    let omitted = if !one && items.iter().any(|item| item.read_only) {
        "\nFactory Captures will be omitted."
    } else {
        ""
    };
    let (title, body) = match folder {
        None => (
            format!("Are you sure you want to delete {count} {noun}{s}?"),
            format!(
                "This will delete the selected {noun}{s} from the {library} and any folders that \
                 contain {}.\n\nThis action is permanent.{omitted}",
                if one { "it" } else { "them" }
            ),
        ),
        Some(folder) => (
            format!("Are you sure you want to remove {count} {noun}{s}?"),
            format!(
                "This will remove the selected {noun}{s} from {}, but {} will still remain in \
                 the {library} and any other folders.\n\nDo you want to delete the selected \
                 {noun}{s} from the {library} as well?{omitted}",
                folder.name,
                if one { "it" } else { "they" }
            ),
        ),
    };
    l.overlay();
    let panel = Rect::new(100, 90, 600, 300);
    l.panel(panel, PANEL, 8.0, None);
    l.paragraph(
        Rect::new(panel.x + 24, panel.y + 16, panel.width - 48, 52),
        title,
        LARGE_BOLD,
        TEXT,
    );
    l.paragraph(
        Rect::new(panel.x + 24, panel.y + 72, panel.width - 48, 140),
        body,
        MEDIUM,
        MUTED,
    );
    let row = panel.bottom() - 68;
    l.button(
        Rect::new(panel.x + 24, row, 160, 48),
        Control::Back,
        true,
        "CANCEL",
        ButtonStyle::Secondary,
    );
    let delete = Control::Library(LibraryControl::ConfirmBulkDelete);
    let deletable = items.iter().any(|item| !item.read_only);
    if folder.is_some() {
        l.button(
            Rect::new(panel.right() - 332, row, 150, 48),
            Control::Library(LibraryControl::ConfirmBulkRemove),
            count > 0,
            "REMOVE",
            ButtonStyle::Primary,
        );
    }
    l.button(
        Rect::new(panel.right() - 174, row, 150, 48),
        delete,
        deletable,
        "DELETE",
        ButtonStyle::Danger,
    );
}

/// Bulk delete confirm: "Are you sure you want to remove %1 Preset%2?" (`0x11fb428`).
fn confirm_delete(ui: &UiState, l: &mut Layout) {
    if let Some(kind) = ui.directory.category.media() {
        return confirm_media_delete(ui, l, kind);
    }
    let count = ui
        .directory
        .multi
        .as_ref()
        .map_or(0, |checked| checked.len());
    l.overlay();
    let panel = Rect::new(160, 120, 480, 240);
    l.panel(panel, PANEL, 8.0, None);
    l.text(
        Rect::new(panel.x + 24, panel.y + 16, panel.width - 48, 28),
        "Delete Presets",
        LARGE_BOLD,
        TEXT,
        Align::Left,
    );
    l.paragraph(
        Rect::new(panel.x + 24, panel.y + 60, panel.width - 48, 90),
        format!(
            "Are you sure you want to remove {count} Preset{}?",
            if count == 1 { "" } else { "s" }
        ),
        MEDIUM,
        MUTED,
    );
    l.button(
        Rect::new(panel.x + 24, panel.bottom() - 68, 200, 48),
        Control::Back,
        true,
        "CANCEL",
        ButtonStyle::Secondary,
    );
    l.button(
        Rect::new(panel.right() - 224, panel.bottom() - 68, 200, 48),
        Control::Library(LibraryControl::ConfirmBulkDelete),
        true,
        "DELETE",
        ButtonStyle::Danger,
    );
}

/// Move onto an occupied slot (`0x1e8290`).
fn confirm_move(l: &mut Layout) {
    l.overlay();
    let panel = Rect::new(120, 110, 560, 260);
    l.panel(panel, PANEL, 8.0, None);
    l.text(
        Rect::new(144, 126, 512, 28),
        "You've dropped on an existing preset",
        LARGE_BOLD,
        TEXT,
        Align::Left,
    );
    l.paragraph(
        Rect::new(144, 166, 512, 110),
        "Swap - presets will change places with each other.\nOverwrite - will delete previous \
         and leave the one you just dragged over.",
        MEDIUM,
        MUTED,
    );
    let library = |overwrite| Control::Library(LibraryControl::ConfirmMove { overwrite });
    l.button(
        Rect::new(144, 294, 160, 48),
        Control::Back,
        true,
        "CANCEL",
        ButtonStyle::Secondary,
    );
    l.button(
        Rect::new(320, 294, 160, 48),
        library(false),
        true,
        "SWAP",
        ButtonStyle::Primary,
    );
    l.button(
        Rect::new(496, 294, 160, 48),
        library(true),
        true,
        "OVERWRITE",
        ButtonStyle::Danger,
    );
}

/// Folder delete (`0x83c6c0`): DELETE FOLDER keeps the items in the library;
/// "DELETE FOLDER + <ITEMS>" removes them too. Setlists and items use one DELETE.
fn confirm_directory_delete(ui: &UiState, l: &mut Layout, target: DirectoryDelete) {
    l.overlay();
    let panel = Rect::new(120, 110, 560, 260);
    l.panel(panel, PANEL, 8.0, None);
    let folder = match target {
        DirectoryDelete::Folder(id) => ui.snapshot.directory.folders.iter().find(|f| f.id == id),
        _ => None,
    };
    // The three-button form only when the folder links library items.
    let linked = |id| {
        ui.snapshot
            .directory
            .media
            .iter()
            .any(|item| item.folders.contains(&id))
    };
    let items = folder
        .filter(|folder| linked(folder.id))
        .map(|folder| match folder.kind {
            MediaKind::Capture => "capture",
            MediaKind::Ir => "IR",
        });
    let body = match (target, folder, items) {
        (DirectoryDelete::Setlist(_), ..) => "Delete this setlist and all its presets?".to_owned(),
        (DirectoryDelete::Folder(_), Some(folder), Some(items)) => format!(
            "This will delete the {} folder.\nDo you also want to remove {items}s it contains \
             from your library?",
            folder.name
        ),
        (DirectoryDelete::Folder(_), ..) => "Are you sure you want to delete this folder?".into(),
        (DirectoryDelete::Media(_), ..) => {
            "Delete this item from the library and all folders?".to_owned()
        }
    };
    l.paragraph(Rect::new(144, 134, 512, 100), body, MEDIUM, TEXT);
    l.button(
        Rect::new(144, 294, 130, 48),
        Control::Back,
        true,
        "CANCEL",
        ButtonStyle::Secondary,
    );
    let confirm = |remove_products| {
        Control::Library(LibraryControl::ConfirmDirectoryDelete { remove_products })
    };
    match items {
        Some(items) => {
            l.button(
                Rect::new(282, 294, 180, 48),
                confirm(false),
                true,
                "DELETE FOLDER",
                ButtonStyle::Danger,
            );
            l.button(
                Rect::new(470, 294, 186, 48),
                confirm(true),
                true,
                format!("DELETE FOLDER + {}S", items.to_uppercase()),
                ButtonStyle::Danger,
            );
        }
        None => l.button(
            Rect::new(282, 294, 180, 48),
            confirm(false),
            true,
            "DELETE",
            ButtonStyle::Danger,
        ),
    }
}

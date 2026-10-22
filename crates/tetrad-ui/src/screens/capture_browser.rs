//! Add device capture browser: the Neural Capture group of the Virtual Device List
//! (`zenUI::VirtualDevicesCaptures`, ctor `0x841f60`; shots sn-4, sn-5, 2026-10-16). Right of
//! the category rail: the header row ("Add device", ← inside a library folder, FILTER, SORT,
//! SEARCH, ✕), the folder table (`FolderDirTable_VirtualDevicesCaptures`, 60 px rows:
//! Favorites, Recent, Downloads, Captures Library with its count and its folder tree) and the
//! capture list (`0x775aec`, 430 px) with the `# • A • I • R • Z` strip. A row shows the name,
//! the date (role 0x25, `dd MMM yyyy`, `0x74c420`) and the name's initial (role 0x21, the
//! "number column": "4" for "4-Comp Custom 1"). A row tap adds or replaces the block
//! (`captureClickedSlot` → `onCaptureClicked` `0x386024`). FILTER, SORT and SEARCH are the
//! Directory controls and pop-ups over this list (I, as the media picker). Geometry and
//! colours are measured on the stock shots.

use tetrad_assets::{Icon, IconSize};

use super::directory::{Roles, natural_cmp, stock_date};
use super::directory_glyph::Glyph;
use super::directory_lists::{Mark, SIDE_ROW, SideRow, index_strip, sidebar_row, tree};
use super::presets;
use crate::draw::RADIUS;
use crate::layout::{BODY, ButtonStyle, CAPTION, Layout, ListId, Look};
use crate::render::{Align, HEIGHT, WIDTH};
use crate::widgets::CLOSE_BUTTON;
use crate::*;

/// The panel right of the 100 px rail and its 2 px gap.
const PANEL_X: i32 = 102;
const FOLDERS: Rect = Rect::new(104, 60, 256, HEIGHT - 60);
const LIST: Rect = Rect::new(362, 60, 430, HEIGHT - 60);
/// The strip at the list's right end (x 765..792).
const STRIP: u32 = 27;
const ROW: u32 = 60;
/// The ← button left of FILTER (shot sn-5: x 356..454).
const BACK: Rect = Rect::new(356, 8, 98, 44);
/// REFRESH (94 × 60 in the VDC ctor) in the same place, ending at the ← button's right edge
/// (I: its x; `0x385b88` anchors it in the header row).
const REFRESH: Rect = Rect::new(360, 8, 94, 44);
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

pub(super) fn layout(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    l.push(
        Rect::new(PANEL_X, 0, WIDTH - PANEL_X as u32, HEIGHT),
        "",
        Look::Fill(p.background),
    );
    header(ui, l);
    folders(ui, l);
    list(ui, l);
    presets::popup(ui, l);
}

fn header(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let dir = &ui.directory;
    l.text(
        Rect::new(120, 8, 220, 44),
        "Add device",
        BODY,
        p.muted,
        Align::Left,
    );
    // `updateRefreshButtonCombo` (`0x83f1b4`), only in Downloads: offline `no_signal.png`,
    // online and signed out `not_logged_in.png` (grey art with a red cross), else
    // `updater.png`. Online is the stock cloud gate (`0x669580`): Wi-Fi internet or Cortex
    // Control online. The button reads the list again in every state (I).
    if ui.picker.captures.folder == CaptureFolder::Downloads {
        let s = &ui.snapshot;
        let online = s.wifi.status == WifiStatus::InternetConnected || s.control_online;
        let refresh = Control::Picker(PickerControl::CaptureRefresh);
        let blocked = match (online, s.account.username.is_some()) {
            (false, _) => Some(Icon::NoSignal),
            (true, false) => Some(Icon::NotLoggedIn),
            (true, true) => None,
        };
        match blocked {
            None => {
                let look = Look::IconButton {
                    icon: Icon::Sync,
                    fill: p.surface,
                };
                l.control(REFRESH, refresh, true, "REFRESH", look);
            }
            Some(icon) => {
                l.panel(REFRESH, p.surface, RADIUS, None);
                l.control(REFRESH, refresh, true, "REFRESH", Look::Area);
                for (icon, color) in [(icon, p.muted), (Icon::CornerCross, p.danger)] {
                    let size = IconSize::Medium;
                    l.push(REFRESH, "", Look::Glyph { icon, size, color });
                }
            }
        }
    }
    if ui.picker.captures.entered.is_some() {
        l.control(
            BACK,
            Control::Picker(PickerControl::CaptureBack),
            true,
            "BACK",
            Look::IconButton {
                icon: Icon::Back,
                fill: p.surface,
            },
        );
    }
    let button = |control, label, mark, active: bool| presets::HeaderButton {
        control: Control::Library(control),
        label,
        mark,
        enabled: true,
        fill: if active { p.accent_fill } else { p.surface },
    };
    let filtered = dir.made_by_me || !dir.device_types.is_empty() || dir.version.stock().is_some();
    presets::icon_buttons(
        l,
        &[
            button(
                LibraryControl::Filter,
                "FILTER",
                Some(Glyph::Filter),
                filtered,
            ),
            button(LibraryControl::Sort, "SORT", Some(Glyph::Sort), false),
            button(LibraryControl::Search, "SEARCH", None, dir.search.is_some()),
        ],
    );
    presets::divider(l, 685);
    l.control(
        CLOSE_BUTTON,
        Control::Picker(PickerControl::Close),
        true,
        "CLOSE",
        Look::IconButton {
            icon: Icon::Close,
            fill: p.surface,
        },
    );
}

/// The folders below `parent` (`None`: the library root), by name (shot sn-5: Amp, Combo
/// Amp, Compressor, Fuzz, Overdrive).
fn children(library: &CaptureLibrary, parent: Option<u16>) -> Vec<u16> {
    let mut children: Vec<u16> = (0u16..)
        .zip(&library.folders)
        .filter(|(_, folder)| folder.parent == parent)
        .map(|(index, _)| index)
        .collect();
    let name = |index: &u16| library.folders[usize::from(*index)].name.as_str();
    children.sort_by(|a, b| natural_cmp(name(a), name(b)));
    children
}

fn folders(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let library = &*ui.snapshot.capture_library;
    let browse = ui.picker.captures;
    let selected = |folder| browse.folder == folder;
    let mut rows = Vec::new();
    // The root rows; inside a folder its two-line head row comes first instead.
    if browse.entered.is_none() {
        for (folder, label, glyph) in [
            (CaptureFolder::Favorites, "Favorites", Glyph::Heart),
            (CaptureFolder::Recent, "Recent", Glyph::Clock),
            (CaptureFolder::Downloads, "Downloads", Glyph::Download),
        ] {
            rows.push(SideRow {
                selected: selected(folder),
                ..SideRow::new(
                    Control::Picker(PickerControl::CaptureFolder(folder)),
                    label,
                    Mark::Glyph(glyph),
                )
            });
        }
        let count = (library.captures.iter())
            .filter(|capture| capture.origin != CaptureOrigin::Download)
            .count();
        rows.push(SideRow {
            selected: selected(CaptureFolder::Library),
            value: count.to_string(),
            ..SideRow::new(
                Control::Picker(PickerControl::CaptureFolder(CaptureFolder::Library)),
                "Captures Library",
                Mark::Glyph(Glyph::Library),
            )
        });
    }
    let head = usize::from(browse.entered.is_some());
    let first_child = head + rows.len();
    for index in children(library, browse.entered) {
        let folder = CaptureFolder::Folder(index);
        rows.push(SideRow {
            selected: selected(folder),
            child: true,
            ..SideRow::new(
                Control::Picker(PickerControl::CaptureFolder(folder)),
                &library.folders[usize::from(index)].name,
                Mark::Icon(Icon::Folder),
            )
        });
    }
    let total = head + rows.len();
    l.push(FOLDERS, "", Look::Fill(p.panel));
    l.scroll(
        ListId::MediaFolders,
        FOLDERS,
        total as u32 * SIDE_ROW,
        ui.offset(ListId::MediaFolders),
        |l, top| {
            let row = |index: usize| {
                let y = top + (index as u32 * SIDE_ROW) as i32;
                Rect::new(FOLDERS.x, y, FOLDERS.width, SIDE_ROW)
            };
            if let Some(entered) = browse.entered {
                let folder = CaptureFolder::Folder(entered);
                head_row(l, row(0), library, entered, selected(folder));
            }
            for (index, side) in rows.into_iter().enumerate() {
                sidebar_row(l, row(head + index), side);
            }
            tree(l, FOLDERS.x, row(first_child).y, total - first_child);
        },
    );
}

/// The entered folder (shot sn-5): folder icon, the parent's name small and grey ("Captures
/// Library" at the top level), then the folder name.
fn head_row(l: &mut Layout, b: Rect, library: &CaptureLibrary, entered: u16, selected: bool) {
    let p = l.palette;
    let folder = &library.folders[usize::from(entered)];
    if selected {
        l.push(b, "", Look::Fill(p.surface));
    }
    l.control(
        b,
        Control::Picker(PickerControl::CaptureFolder(CaptureFolder::Folder(entered))),
        true,
        folder.name.clone(),
        Look::Area,
    );
    l.push(
        Rect::new(b.x + 16, b.y, 24, b.height),
        "",
        Look::Button {
            style: ButtonStyle::Ghost,
            icon: Some(Icon::Folder),
            font: None,
        },
    );
    let parent = folder.parent.map_or("Captures Library", |parent| {
        library.folders[usize::from(parent)].name.as_str()
    });
    let width = b.width - 70;
    l.text(
        Rect::new(b.x + 54, b.y + 8, width, 20),
        parent,
        CAPTION,
        p.muted,
        Align::Left,
    );
    l.text(
        Rect::new(b.x + 54, b.y + 27, width, 22),
        folder.name.clone(),
        BODY,
        if selected { p.accent } else { p.text },
        Align::Left,
    );
}

/// The capture rows of the open folder after the Directory filter, search and sort, as
/// indices into [`CaptureLibrary::captures`]. Favorites is the Directory capture list
/// ([`crate::DirectoryView`]); Recent its raw capture Recents keys, so a factory capture
/// shows too. Both keep the listed captures only.
pub(crate) fn rows(ui: &UiState) -> Vec<u16> {
    let library = &*ui.snapshot.capture_library;
    let dir = &ui.directory;
    let captures = (0u16..).zip(&library.captures);
    let mut rows: Vec<(u16, &LibraryCapture)> = match ui.picker.captures.folder {
        CaptureFolder::Favorites => (ui.snapshot.directory.favorites.iter())
            .filter(|item| item.kind == ItemKind::Capture)
            .filter_map(|item| (captures.clone()).find(|(_, capture)| capture.value == item.value))
            .collect(),
        CaptureFolder::Recent => (ui.snapshot.directory.capture_recents.iter())
            .filter_map(|key| (captures.clone()).find(|(_, capture)| capture.value == *key))
            .collect(),
        CaptureFolder::Downloads => captures
            .filter(|(_, capture)| capture.origin == CaptureOrigin::Download)
            .collect(),
        CaptureFolder::Library => captures
            .filter(|(_, capture)| capture.origin != CaptureOrigin::Download)
            .collect(),
        CaptureFolder::Folder(folder) => captures
            .filter(|(_, capture)| capture.folders.contains(&folder))
            .collect(),
    };
    let query = (dir.search.is_some()).then(|| ui.library_query.to_lowercase());
    let version = dir.version.stock().map(|version| version == "2");
    rows.retain(|(_, capture)| {
        let device = capture.device.map(|device| device as u8);
        (!dir.made_by_me || capture.origin == CaptureOrigin::User)
            && (dir.device_types.is_empty()
                || device.is_some_and(|code| dir.device_types.contains(&code)))
            && version.is_none_or(|nc2| capture.nc2 == nc2)
            && (query.as_ref()).is_none_or(|query| capture.name.to_lowercase().contains(query))
    });
    let mut keyed: Vec<(u16, Roles)> = rows
        .into_iter()
        .map(|(index, capture)| {
            let roles = Roles {
                name: &capture.name,
                author: "",
                date: stock_date(&capture.date),
                instrument: PreferredInstrument::NotSet,
                device: capture.device.map(|device| device as u8),
                gain: None,
                id: &capture.value,
                pinned: false,
            };
            (index, roles)
        })
        .collect();
    keyed.sort_by(|a, b| dir.media_sort.order(&a.1, &b.1));
    keyed.into_iter().map(|(index, _)| index).collect()
}

/// The strip key of a name: its upper-case initial, `#` for anything but A–Z.
fn strip_key(name: &str) -> u8 {
    match name.trim_start().chars().next() {
        Some(c) if c.is_ascii_alphabetic() => c.to_ascii_uppercase() as u8,
        _ => b'#',
    }
}

/// Role 0x25: `dd MMM yyyy`, "--" for a date that does not parse (`0x74c420`).
fn date_text(date: &str) -> String {
    match stock_date(date) {
        Some([year, month, day, ..]) => {
            format!("{day:02} {} {year}", MONTHS[usize::from(month - 1)])
        }
        None => "--".into(),
    }
}

fn list(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let library = &*ui.snapshot.capture_library;
    let rows = rows(ui);
    l.push(LIST, "", Look::Fill(p.panel));
    if rows.is_empty() {
        l.text(
            Rect::new(LIST.x, LIST.y + 40, LIST.width, 30),
            "Empty Folder",
            BODY,
            p.faint,
            Align::Center,
        );
        return;
    }
    let viewport = Rect::new(LIST.x, LIST.y, LIST.width - STRIP, LIST.height);
    l.scroll(
        ListId::MediaItems,
        viewport,
        rows.len() as u32 * ROW,
        ui.offset(ListId::MediaItems),
        |l, top| {
            // Only the rows in view: the library holds about 2,500 captures.
            let first = ((viewport.y - top) / ROW as i32).max(0) as usize;
            let shown = (viewport.height / ROW) as usize + 2;
            for (at, &index) in rows.iter().enumerate().skip(first).take(shown) {
                let capture = &library.captures[usize::from(index)];
                let y = top + (at as u32 * ROW) as i32;
                capture_row(
                    l,
                    Rect::new(viewport.x, y, viewport.width, ROW),
                    index,
                    capture,
                );
            }
        },
    );
    let strip = Rect::new(LIST.right() - STRIP as i32, LIST.y, STRIP, LIST.height);
    // Shot sn-4: marks from y 94 to 446, 44 px apart.
    index_strip(l, strip, 34, |key| {
        Control::Picker(PickerControl::CaptureJump(key))
    });
}

/// Name, date, the initial (shot sn-4: x 378, 745) and a divider inset 16 px.
fn capture_row(l: &mut Layout, b: Rect, index: u16, capture: &LibraryCapture) {
    let p = l.palette;
    l.control(
        b,
        Control::Picker(PickerControl::Capture(index)),
        true,
        capture.name.clone(),
        Look::Area,
    );
    let text_x = b.x + 16;
    l.text(
        Rect::new(text_x, b.y + 8, 340, 22),
        capture.name.clone(),
        BODY,
        p.text,
        Align::Left,
    );
    l.text(
        Rect::new(text_x, b.y + 32, 340, 20),
        date_text(&capture.date),
        CAPTION,
        p.muted,
        Align::Left,
    );
    let initial = (capture.name.trim_start().chars().next())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    l.text(
        Rect::new(b.right() - 36, b.y, 24, ROW),
        initial,
        BODY,
        p.muted,
        Align::Center,
    );
    l.push(
        Rect::new(text_x, b.bottom() - 1, b.width - 16, 1),
        "",
        Look::Fill(p.surface),
    );
}

impl UiState {
    /// The device list shows the Neural Capture group: its capture browser. The application
    /// reads [`crate::UiSnapshot::capture_library`] while it shows.
    pub fn capture_browser(&self) -> bool {
        self.screen() == Screen::ModelPicker
            && self.picker.group == Some(DeviceGroup::NeuralCapture)
            && self.picker.query.is_empty()
    }

    /// The browser opens on the Captures Library with the Directory pop-ups, Multi Select and
    /// search closed.
    pub(crate) fn open_capture_browser(&mut self) {
        self.picker.captures = Default::default();
        self.directory.open_picker(MediaKind::Capture);
        self.reset_scroll(ListId::MediaFolders);
        self.reset_scroll(ListId::MediaItems);
    }

    pub(crate) fn capture_browser_control(&mut self, control: PickerControl) -> Vec<UiCommand> {
        let library = std::sync::Arc::clone(&self.snapshot.capture_library);
        let browse = &mut self.picker.captures;
        match control {
            PickerControl::CaptureFolder(folder) => {
                browse.folder = folder;
                if let CaptureFolder::Folder(index) = folder
                    && !children(&library, Some(index)).is_empty()
                {
                    browse.entered = Some(index);
                    self.reset_scroll(ListId::MediaFolders);
                }
                self.reset_scroll(ListId::MediaItems);
            }
            PickerControl::CaptureBack => {
                let parent = (browse.entered)
                    .and_then(|index| library.folders.get(usize::from(index)))
                    .and_then(|folder| folder.parent);
                browse.entered = parent;
                browse.folder = parent.map_or(CaptureFolder::Library, CaptureFolder::Folder);
                self.reset_scroll(ListId::MediaFolders);
                self.reset_scroll(ListId::MediaItems);
            }
            PickerControl::Capture(index) => {
                let (Some(position), Some(capture)) =
                    (self.picker.target, library.captures.get(usize::from(index)))
                else {
                    return Vec::new();
                };
                let mut commands = vec![UiCommand::AddCapture {
                    position,
                    key: capture.value.clone(),
                }];
                commands.extend(self.pop());
                return commands;
            }
            PickerControl::CaptureJump(key) => {
                let keys: Vec<u8> = (rows(self).iter())
                    .map(|&index| strip_key(&library.captures[usize::from(index)].name))
                    .collect();
                let descending = self.directory.media_sort.descending;
                let row = (keys.iter())
                    .position(|&row| if descending { row <= key } else { row >= key })
                    .unwrap_or(keys.len().saturating_sub(1));
                (self.scroll).insert(ListId::MediaItems, (row as u32 * ROW) as i32);
            }
            PickerControl::CaptureRefresh => return vec![UiCommand::RefreshCaptures],
            PickerControl::Close => return self.pop(),
            _ => {}
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use tetrad_assets::Palette;

    use super::*;

    /// Role 0x25 prints `dd MMM yyyy` ("29 Apr 2025", shot sn-4) or "--"; the strip maps a
    /// leading digit to `#`.
    #[test]
    fn rows_show_the_stock_date_and_strip_keys() {
        assert_eq!(date_text("2025-04-29T10:00:00Z"), "29 Apr 2025");
        assert_eq!(date_text(""), "--");
        assert_eq!(strip_key("4-Comp Custom 1"), b'#');
        assert_eq!(strip_key("amp"), b'A');
    }

    fn capture(name: &str, origin: CaptureOrigin, folders: Vec<u16>) -> LibraryCapture {
        LibraryCapture {
            value: format!("key-{name}"),
            name: name.into(),
            date: "2025-04-29T10:00:00Z".into(),
            device: Some(CaptureDevice::Compressor),
            nc2: origin == CaptureOrigin::Factory,
            origin,
            folders,
        }
    }

    /// The B7 shapes: Factory Captures V1 / V2 (V2 ▸ Compressor), My Captures; one factory,
    /// one user and one downloaded capture. Row 1 has an input.
    fn browser() -> UiState {
        let folder = |name: &str, parent| LibraryFolder {
            name: name.into(),
            parent,
        };
        let mut s = UiSnapshot {
            capture_library: std::sync::Arc::new(CaptureLibrary {
                folders: vec![
                    folder("Factory Captures V1", None),
                    folder("Factory Captures V2", None),
                    folder("Compressor", Some(1)),
                    folder("My Captures", None),
                ],
                captures: vec![
                    capture("65 AC100 H 1", CaptureOrigin::User, vec![3]),
                    capture("4-Comp Custom 1", CaptureOrigin::Factory, vec![1, 2]),
                    capture("Cloud Tweed", CaptureOrigin::Download, Vec::new()),
                ],
            }),
            ..UiSnapshot::default()
        };
        s.rows[0].input = InputPort::Input1;
        let mut ui = UiState::default();
        ui.set_snapshot(s).unwrap();
        let position = GridPosition::new(0, 4).unwrap();
        ui.handle_input(InputEvent::Activate(Control::SelectCell(position)));
        let group = PickerControl::Group(DeviceGroup::NeuralCapture);
        ui.handle_input(InputEvent::Activate(Control::Picker(group)));
        ui
    }

    /// Shot sn-2: the category drawer opens on the right over the dimmed Grid, the target
    /// slot left of it shows the black "+", and Plugins has no "New" pill.
    #[test]
    fn the_category_drawer_opens_on_the_right() {
        let mut s = UiSnapshot::default();
        s.rows[0].input = InputPort::Input1;
        let mut ui = UiState::default();
        ui.set_snapshot(s).unwrap();
        let position = GridPosition::new(0, 3).unwrap();
        ui.handle_input(InputEvent::Activate(Control::SelectCell(position)));
        let group = Control::Picker(PickerControl::Group(DeviceGroup::Plugins));
        let row = bounds(&ui, group).unwrap();
        assert_eq!((row.x, row.width), (400, 400));
        let layout = ui.layout();
        assert!(!(layout.items.iter()).any(|item| item.label == "New"));
        let plus = (layout.items.iter())
            .find(|item| item.label == "+" && matches!(item.look, Look::IconButton { .. }));
        assert!(plus.is_some_and(|item| item.bounds.right() <= 400));
        assert_eq!(bounds(&ui, Control::Back).map(|area| area.x), Some(0));
    }

    fn bounds(ui: &UiState, control: Control) -> Option<Rect> {
        (ui.hit_regions().into_iter())
            .find(|region| region.control == control)
            .map(|region| region.bounds)
    }

    fn names(ui: &UiState) -> Vec<String> {
        let library = &ui.snapshot.capture_library;
        (rows(ui).iter())
            .map(|&index| library.captures[usize::from(index)].name.clone())
            .collect()
    }

    /// Shot sn-4 geometry and colours: the `#121212` header and gaps, the `#2e2e2e` folder
    /// and list panels, the selected Captures Library row in `#1e1e1e` with its count, the
    /// header buttons at the stock places, rows sorted by name with the date and initial.
    #[test]
    fn the_browser_matches_the_stock_layout() {
        let ui = browser();
        assert!(ui.capture_browser());
        let folder = |folder| Control::Picker(PickerControl::CaptureFolder(folder));
        let library = bounds(&ui, folder(CaptureFolder::Library)).unwrap();
        assert_eq!(library, Rect::new(104, 240, 256, 60));
        assert_eq!(
            bounds(&ui, folder(CaptureFolder::Favorites)),
            Some(Rect::new(104, 60, 256, 60))
        );
        assert!(
            bounds(&ui, folder(CaptureFolder::Folder(2))).is_none(),
            "nested"
        );
        assert!(bounds(&ui, Control::Picker(PickerControl::CaptureBack)).is_none());
        assert_eq!(
            bounds(&ui, Control::Library(LibraryControl::Filter)),
            Some(Rect::new(462, 8, 66, 44))
        );
        assert_eq!(
            bounds(&ui, Control::Picker(PickerControl::Close)),
            Some(Rect::new(694, 8, 98, 44))
        );
        assert_eq!(names(&ui), ["4-Comp Custom 1", "65 AC100 H 1"]);
        let row = bounds(&ui, Control::Picker(PickerControl::Capture(1))).unwrap();
        assert_eq!(row, Rect::new(362, 60, 403, 60));
        let layout = ui.layout();
        let text = |label: &str| {
            (layout.items.iter())
                .find(|item| item.label == label && matches!(item.look, Look::Text { .. }))
                .map(|item| item.bounds)
        };
        assert_eq!(text("29 Apr 2025").map(|at| at.x), Some(378));
        assert!(text("2").is_some(), "the library count");
        let frame = ui.render();
        for (x, y, want) in [
            (400, 30, Palette::STOCK.background),
            (103, 300, Palette::STOCK.background),
            (361, 200, Palette::STOCK.background),
            (150, 450, Palette::STOCK.panel),
            (500, 450, Palette::STOCK.panel),
            (330, 270, Palette::STOCK.surface),
        ] {
            assert_eq!(frame.pixel(x, y), want, "({x}, {y})");
        }
    }

    /// Shot sn-5 (`onFolderTapped`): a folder with subfolders is entered; its head row names
    /// the parent, ← goes back; Downloads lists only downloaded captures; a row adds the
    /// capture block at the target and closes the device list.
    #[test]
    fn folders_enter_and_a_row_adds_the_capture() {
        let mut ui = browser();
        let folder = |folder| Control::Picker(PickerControl::CaptureFolder(folder));
        ui.handle_input(InputEvent::Activate(folder(CaptureFolder::Folder(1))));
        assert!(bounds(&ui, Control::Picker(PickerControl::CaptureBack)).is_some());
        assert_eq!(
            bounds(&ui, folder(CaptureFolder::Folder(2))),
            Some(Rect::new(104, 120, 256, 60))
        );
        let layout = ui.layout();
        assert!((layout.items.iter()).any(|item| item.label == "Captures Library"));
        assert_eq!(names(&ui), ["4-Comp Custom 1"]);
        let back = Control::Picker(PickerControl::CaptureBack);
        ui.handle_input(InputEvent::Activate(back));
        assert_eq!(ui.picker.captures, Default::default());
        ui.handle_input(InputEvent::Activate(folder(CaptureFolder::Downloads)));
        assert_eq!(names(&ui), ["Cloud Tweed"]);
        let commands = ui.handle_input(InputEvent::Activate(Control::Picker(
            PickerControl::Capture(2),
        )));
        assert_eq!(
            commands.first(),
            Some(&UiCommand::AddCapture {
                position: GridPosition::new(0, 4).unwrap(),
                key: "key-Cloud Tweed".into(),
            })
        );
        assert_eq!(ui.screen(), Screen::Grid);
    }

    /// `updateRefreshButtonCombo` (`0x83f1b4`): REFRESH shows only in Downloads and reads the
    /// list again (`0x83ff00`). Recent lists the capture Recents keys newest first, a factory
    /// capture included (`0x59dd78` adds what the library root holds).
    #[test]
    fn downloads_has_refresh_and_recent_lists_factory_captures() {
        let mut ui = browser();
        let refresh = Control::Picker(PickerControl::CaptureRefresh);
        let folder = |folder| Control::Picker(PickerControl::CaptureFolder(folder));
        assert!(bounds(&ui, refresh).is_none());
        ui.handle_input(InputEvent::Activate(folder(CaptureFolder::Downloads)));
        assert_eq!(bounds(&ui, refresh), Some(REFRESH));
        assert_eq!(
            ui.handle_input(InputEvent::Activate(refresh)),
            [UiCommand::RefreshCaptures]
        );
        // `0x83f1b4`: offline no_signal, online and signed out not_logged_in, else updater;
        // the key reads again in each state.
        let glyphs = |ui: &UiState| -> Vec<Icon> {
            (ui.layout().items.iter())
                .filter(|item| item.bounds == REFRESH)
                .filter_map(|item| match item.look {
                    Look::IconButton { icon, .. } | Look::Glyph { icon, .. } => Some(icon),
                    _ => None,
                })
                .collect()
        };
        let state = |ui: &mut UiState, online: bool, user: Option<&str>| {
            let mut s = ui.snapshot().clone();
            s.control_online = online;
            s.account.username = user.map(String::from);
            ui.set_snapshot(s).unwrap();
        };
        state(&mut ui, false, Some("player"));
        assert_eq!(glyphs(&ui), [Icon::NoSignal, Icon::CornerCross]);
        state(&mut ui, true, None);
        assert_eq!(glyphs(&ui), [Icon::NotLoggedIn, Icon::CornerCross]);
        assert_eq!(bounds(&ui, refresh), Some(REFRESH));
        state(&mut ui, true, Some("player"));
        assert_eq!(glyphs(&ui), [Icon::Sync]);
        let mut s = ui.snapshot().clone();
        std::sync::Arc::make_mut(&mut s.directory).capture_recents =
            ["key-4-Comp Custom 1", "gone", "key-65 AC100 H 1"]
                .map(String::from)
                .into();
        ui.set_snapshot(s).unwrap();
        ui.handle_input(InputEvent::Activate(folder(CaptureFolder::Recent)));
        assert!(bounds(&ui, refresh).is_none());
        let mut listed = names(&ui);
        listed.sort();
        assert_eq!(listed, ["4-Comp Custom 1", "65 AC100 H 1"]);
    }
}

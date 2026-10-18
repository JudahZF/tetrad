//! Directory state and actions (manual §5, spec `w4-ui-spec-directory` §1–4). The layout
//! is in `presets.rs` and `directory_lists.rs`. Pop-ups are local to the Directory: they
//! open as popover layers of the screen, and a tap outside (or Back) closes them.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};

use crate::layout::ListId;
use crate::state::{Confirm, DirectoryNameEdit, Overlay, TextTarget};
use crate::*;

/// Owned bound for the session's recent searches (stock keeps a persistent list, I).
const RECENT_SEARCHES: usize = 5;
/// The presets Downloads folder key (`"cloud-%1-%2"`, `0x716bac`: type 0, Downloads 1).
const DOWNLOADS_PRESETS: &str = "cloud-0-1";
/// Row pitch of the flat lists with a subtitle (captures, IRs, List View, search).
pub(crate) const ITEM_ROW: u32 = 60;

/// Category drop-down entries in stock order (shot 120).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Category {
    Favorites,
    #[default]
    Presets,
    Captures,
    Irs,
    Plugins,
}

impl Category {
    pub const ALL: [Self; 5] = [
        Self::Favorites,
        Self::Presets,
        Self::Captures,
        Self::Irs,
        Self::Plugins,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Favorites => "Favorites and Recent",
            Self::Presets => "Presets",
            Self::Captures => "Neural Captures",
            Self::Irs => "Impulse Responses",
            Self::Plugins => "Plugin Presets",
        }
    }

    /// The item kind of a category with cloud folders.
    pub fn cloud_kind(self) -> Option<ItemKind> {
        match self {
            Self::Presets => Some(ItemKind::Preset),
            Self::Captures => Some(ItemKind::Capture),
            Self::Irs => Some(ItemKind::Ir),
            Self::Favorites | Self::Plugins => None,
        }
    }

    /// Library of a capture or IR category.
    pub fn media(self) -> Option<MediaKind> {
        match self {
            Self::Captures => Some(MediaKind::Capture),
            Self::Irs => Some(MediaKind::Ir),
            _ => None,
        }
    }
}

/// Stock sort roles (`0x7c8b78`): name (0x21), author (0x23), preferred instrument
/// (0x24) and date added (0x25).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SortKey {
    /// Presets only: Bank View.
    #[default]
    Banks,
    Name,
    Author,
    /// Plugin presets only.
    Instrument,
    Date,
    /// Captures only (roles 0x26, 0x27; the sort has a Device or Gain jump bar, I).
    DeviceType,
    Gain,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::Banks => "Banks",
            Self::Name => "Name",
            Self::Author => "Author",
            Self::Instrument => "Preferred Instrument",
            Self::Date => "Date Added",
            Self::DeviceType => "Device Type",
            Self::Gain => "Gain",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Sort {
    pub key: SortKey,
    pub descending: bool,
}

/// The sort roles of one row.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Roles<'a> {
    pub name: &'a str,
    pub author: &'a str,
    /// The sidecar date, parsed once per row ([`stock_date`]).
    pub date: Option<[u16; 6]>,
    pub instrument: PreferredInstrument,
    /// Capture device type code and gain ([`CaptureMeta`]).
    pub device: Option<u8>,
    pub gain: Option<u8>,
    /// Tie-break of byte-equal names under the Name sort (stock: the item key).
    pub id: &'a str,
    /// A capture pinned before this session: stock item `+0x49` set and `+0x4a` clear.
    pub pinned: bool,
}

impl Sort {
    /// Stock `lessThan` (`0x755258`). First, pinned captures go on top in both directions.
    /// Author, Instrument and Date fall back to the name when equal; only Name (role 0x21)
    /// falls back to the key for byte-equal names. Descending swaps the operands, so ties
    /// keep their order (Qt's stable sort).
    pub fn order(self, a: &Roles, b: &Roles) -> Ordering {
        if a.pinned != b.pinned {
            return b.pinned.cmp(&a.pinned);
        }
        let (a, b) = if self.descending { (b, a) } else { (a, b) };
        let by_name = || natural_cmp(a.name, b.name);
        match self.key {
            SortKey::Author if a.author != b.author => natural_cmp(a.author, b.author),
            SortKey::Author => by_name(),
            SortKey::Instrument => a.instrument.cmp(&b.instrument).then_with(by_name),
            SortKey::Date => a.date.cmp(&b.date).then_with(by_name),
            SortKey::DeviceType => a.device.cmp(&b.device).then_with(by_name),
            SortKey::Gain => a.gain.cmp(&b.gain).then_with(by_name),
            SortKey::Banks | SortKey::Name if a.name == b.name => a.id.cmp(b.id),
            SortKey::Banks | SortKey::Name => by_name(),
        }
    }
}

/// The sidecar `date` as stock keeps it: parsed with `yyyy-MM-ddThh:mm:ssZ` (`0x7efc38`, format
/// `0x1561d24`) and stored only when after 1970-01-01T00:00:00Z (`0x7b58fc`). Anything else is
/// `None`: Qt 4.8 orders that default `QDateTime` before every valid date, and two of them tie.
pub(crate) fn stock_date(text: &str) -> Option<[u16; 6]> {
    let bytes = text.as_bytes();
    if bytes.len() != 20 || [4, 7, 10, 13, 16, 19].map(|i| bytes[i]) != *b"--T::Z" {
        return None;
    }
    let field = |at: usize, len: usize| {
        let digits = &bytes[at..at + len];
        digits.iter().all(u8::is_ascii_digit).then(|| {
            digits
                .iter()
                .fold(0, |value, digit| value * 10 + u16::from(digit - b'0'))
        })
    };
    let date = [
        field(0, 4)?,
        field(5, 2)?,
        field(8, 2)?,
        field(11, 2)?,
        field(14, 2)?,
        field(17, 2)?,
    ];
    let [year, month, day, hour, minute, second] = date;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => return None,
    };
    let valid = (1..=days).contains(&day) && hour < 24 && minute < 60 && second < 60;
    (valid && date > [1970, 1, 1, 0, 0, 0]).then_some(date)
}

/// Date bar positions (`0x00807138`).
pub(crate) const DATE_POSITIONS: u32 = 200;
/// Gain bar labels "0" to "10" (`0x008069e4`).
pub(crate) const GAIN_POSITIONS: u8 = 11;

/// The jump bar of a capture/IR list by its sort role (`JumpScrollbarContainer`): the
/// letter bar for Name and Author (`0x805420`), else the Date (`0x807138`), Device
/// (`0x8050cc`) or Gain (`0x8069e4`) bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JumpBar {
    Letters,
    Date(DateBar),
    Device,
    Gain,
}

/// `0x0080733c`: from the oldest date (2021-01-01 when no row has one) to the newest, in
/// days since 1970-01-01. tetrad-ui has no clock: without dates the bar spans zero days (stock
/// uses today).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DateBar {
    pub origin: i64,
    pub span: i64,
    /// The origin was defaulted: position 0 has no date.
    pub defaulted: bool,
}

impl DateBar {
    /// `0x008072b4`: `origin + lroundf(position / 200 * span)`.
    pub fn date_at(self, position: u32) -> Option<i64> {
        if self.defaulted && position == 0 {
            return None;
        }
        let offset = position as f32 / DATE_POSITIONS as f32 * self.span as f32;
        Some(self.origin + offset.round() as i64)
    }
}

pub(crate) fn jump_bar(ui: &UiState, details: &MediaDetails, kind: MediaKind) -> JumpBar {
    match ui.directory.media_sort.key {
        SortKey::DeviceType => JumpBar::Device,
        SortKey::Gain => JumpBar::Gain,
        SortKey::Date => {
            let days: Vec<i64> = media_rows(ui, details, kind)
                .iter()
                .filter_map(|(_, entry)| details.roles(entry).date.map(stock_days))
                .collect();
            let (oldest, newest) = (days.iter().min(), days.iter().max());
            let origin = oldest.copied().unwrap_or(18_628);
            JumpBar::Date(DateBar {
                origin,
                span: newest.map_or(0, |newest| newest - origin),
                defaulted: oldest.is_none(),
            })
        }
        _ => JumpBar::Letters,
    }
}

/// Days since 1970-01-01 of a stock date (days from civil, Howard Hinnant).
pub(crate) fn stock_days(date: [u16; 6]) -> i64 {
    let [year, month, day, ..] = date.map(i64::from);
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year - 719_468
}

/// "MM/YY" of a day number (civil from days); the stock date bar labels are not recovered (I).
pub(crate) fn month_label(days: i64) -> String {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{month:02}/{:02}", year.rem_euclid(100))
}

/// Stock name order (`0x542628`): ASCII case-insensitive, a run of spaces counts
/// as one, digit runs compare by value. Digit runs after a `.` on both sides
/// compare as fractions (digit by digit). Bytes, not characters, as stock.
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let skip = |s: &[u8], mut i: usize| {
        while i > 0 && s.get(i) == Some(&b' ') && s[i - 1] == b' ' {
            i += 1;
        }
        i
    };
    let run = |s: &[u8], i: usize| i + s[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    let (mut i, mut j) = (0, 0);
    loop {
        (i, j) = (skip(a, i), skip(b, j));
        let (x, y) = match (a.get(i), b.get(j)) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => (*x, *y),
        };
        let order = if x.is_ascii_digit() && y.is_ascii_digit() {
            let (ea, eb) = (run(a, i), run(b, j));
            let fraction = i > 0 && a[i - 1] == b'.' && j > 0 && b[j - 1] == b'.';
            let (da, db) = (&a[i..ea], &b[j..eb]);
            (i, j) = (ea, eb);
            if fraction {
                let trim = |d: &[u8]| d.len() - d.iter().rev().take_while(|c| **c == b'0').count();
                da[..trim(da)].cmp(&db[..trim(db)])
            } else {
                let trim = |d: &'_ [u8]| d.iter().take_while(|c| **c == b'0').count();
                let (da, db) = (&da[trim(da)..], &db[trim(db)..]);
                da.len().cmp(&db.len()).then_with(|| da.cmp(db))
            }
        } else {
            (i, j) = (i + 1, j + 1);
            x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase())
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

/// Why the cloud cannot be used: the MessageDialogs of `0x5b0958` (title, text, button), in
/// its order: Cortex Control first, then Wi-Fi, then the account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloudGate {
    /// Cortex Control connected and online (`0x6e0444`, `0x6e045c`), but no cloud: signed
    /// out, or no cloud route (no key exchange, or bring-up without `--control-cloud`).
    ControlLoginFailed,
    /// No internet (`0x669580` false) and Cortex Control connected but offline.
    NoControlInternet,
    /// No Wi-Fi association (`0x8961f8` false).
    NoWifi,
    /// Associated, but the internet check failed (`0x669580` false).
    NoInternet,
    /// Online, no account (`0x2ff7a8` false).
    LoggedOut,
}

impl CloudGate {
    pub fn of(snapshot: &UiSnapshot) -> Option<Self> {
        if snapshot.cloud_ready() {
            None
        } else if snapshot.control_link == ControlLinkState::Online {
            Some(Self::ControlLoginFailed)
        } else if snapshot.wifi.status == WifiStatus::InternetConnected {
            Some(Self::LoggedOut)
        } else if snapshot.control_link == ControlLinkState::Offline {
            Some(Self::NoControlInternet)
        } else if snapshot
            .wifi
            .networks
            .iter()
            .any(|network| network.connected)
        {
            Some(Self::NoInternet)
        } else {
            Some(Self::NoWifi)
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::ControlLoginFailed => "Unknown Error",
            Self::NoControlInternet => "No Control Internet Connection",
            Self::NoWifi => "No Wi-Fi connection",
            Self::NoInternet => "No Internet Connection",
            Self::LoggedOut => "You are not logged in to Cortex Cloud",
        }
    }

    pub fn body(self) -> &'static str {
        match self {
            Self::ControlLoginFailed => "Login via Cortex Control failed.",
            Self::NoControlInternet => "Your Internet connection is offline. Check your network.",
            Self::NoWifi => {
                "You need to be connected to the Internet via Wi-Fi to use the Cloud service."
            }
            Self::NoInternet => {
                "Your Internet connection is offline. Check your network and try reconnecting \
                 to Wi-Fi. You need to be connected to the Internet via Wi-Fi to use the Cloud \
                 service."
            }
            Self::LoggedOut => {
                "Go to Settings / My Account and log in to your Neural DSP account to use the \
                 Cloud service."
            }
        }
    }

    /// The button and the page it opens; OK only closes (I: the callbacks `0x5aa930`,
    /// `0x5aa990`, `0x5aac14` are not decoded; their labels name the pages).
    pub fn button(self) -> (&'static str, Option<SettingsPage>) {
        match self {
            Self::ControlLoginFailed | Self::NoControlInternet => ("OK", None),
            Self::NoWifi | Self::NoInternet => ("WIFI SETTINGS", Some(SettingsPage::Connection)),
            Self::LoggedOut => ("LOG IN", Some(SettingsPage::MyAccount)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Popup {
    Category,
    DatabaseFolder(i64),
    MovePreset(u64),
    /// "You've dropped on an existing preset": SWAP / OVERWRITE / CANCEL (`0x1e8290`).
    MoveConflict {
        from: u64,
        to: u64,
    },
    Delete(DirectoryDelete),
    Sort,
    Filter,
    Preset(u64),
    Setlist(u64),
    Folder(MediaFolder),
    Media(u16),
    /// Destination setlist for the Multi Select copy (`remove` = move).
    Target {
        remove: bool,
    },
    /// "Are you sure you want to remove %1 Preset%2?" (`0x11fb428`).
    ConfirmDelete,
    /// "Choose pasting option" (`PastingPopup` `0x3a4a28`, set up by `0x3a5a88` case 0) with
    /// its checked row.
    Paste(PasteChoice),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectoryDelete {
    Setlist(u64),
    Folder(i64),
    Media(u16),
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Directory {
    pub category: Category,
    /// Page of the Bank View (`bank_size` presets each).
    pub bank: usize,
    /// The open cloud folder (DOWNLOADS or CLOUD) of the category; it replaces the local
    /// list until another sidebar row or category opens.
    pub cloud: Option<CloudScope>,
    /// Captures/IRs sidebar: a folder, or the whole library.
    pub folder: Option<MediaFolder>,
    pub database_folder: Option<i64>,
    pub plugin_folder: Option<u16>,
    /// Sidebar folder of the open plugin (`None`: presets at the plugin root).
    pub plugin_child: Option<u16>,
    pub media_sort: Sort,
    /// Favorites and Recent sidebar row.
    pub favorites: ItemKind,
    /// Favorites and Recent: the 🕒 tab.
    pub recent: bool,
    pub preset_sort: Sort,
    pub media_descending: bool,
    /// Capture filter "Made by Me" (user captures only).
    pub made_by_me: bool,
    /// Capture filter `DEVICE_TYPES`: checked device codes; none checked shows every type.
    pub device_types: BTreeSet<u8>,
    /// Capture filter `VERSIONS`.
    pub version: CaptureVersion,
    /// Highlighted capture/IR row (index into `UiSnapshot::media`).
    pub selected_media: Option<u16>,
    /// Multi Select mode and its checked rows (preset ids or media indices).
    pub multi: Option<BTreeSet<u64>>,
    /// Search mode and its type tab.
    pub search: Option<ItemKind>,
    /// Slot of the open setlist that a search result revealed (highlighted).
    pub revealed: Option<u64>,
    /// Newest first.
    pub recent_searches: Vec<String>,
    pub popup: Option<Popup>,
    /// Multi Select COPY keeps the selection for PASTE (`multiselectPasteSlot` `0x786d38`).
    pub clipboard: Option<Clipboard>,
    /// A PASTE that waits for its slots (`0x795b18` modes 0 and 1); only while Multi Select
    /// is on.
    pub paste: Option<PasteSlots>,
    /// Multi Select EDIT: the items still to rename, one keyboard each (I: stock
    /// `BulkRenameDialog` `0x2bc390` steps the items with NEXT and SAVE).
    pub rename_queue: Vec<BulkRename>,
    /// Multi Select DOWNLOAD of cloud presets (`0x794768`): the product ids, while the
    /// destination setlist (Directory state `0x1b`) and then the slots are chosen.
    pub download: Option<PresetDownload>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PresetDownload {
    pub ids: Vec<String>,
    /// The destination setlist is chosen and open; the downloading option follows.
    pub chosen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Clipboard {
    /// Slots of the setlist open at COPY, by its name: PASTE copies from it, not from the
    /// setlist open at PASTE (review r21 H2), and a setlist create, delete or move between
    /// COPY and PASTE does not move the source (r23 K11; stock pastes items, `0x786d38`).
    Presets { setlist: String, ids: Vec<u64> },
    /// Library ids of captures or IRs.
    Media(Vec<String>),
}

/// The slots a waiting PASTE has (Multiselection `+0x6c`, `+0x30`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PasteSlots {
    /// "Choose each slot manually": the tapped slots, one per copied preset, in tap order.
    Manual(Vec<u64>),
    /// "Paste consecutively from the first chosen slot onwards": the tapped first slot.
    Consecutive(Option<u64>),
}

/// Slots of a setlist (manual "Setlists & Folders": up to 256 Presets; `0x848e18` stops at
/// `0x100`).
pub(crate) const SETLIST_SLOTS: u64 = 256;

impl PasteSlots {
    /// The target slots of `count` copied presets, in paste order.
    pub fn targets(&self, count: usize) -> Vec<u64> {
        match self {
            Self::Manual(slots) => slots.clone(),
            Self::Consecutive(Some(first)) => {
                (*first..(first + count as u64).min(SETLIST_SLOTS)).collect()
            }
            Self::Consecutive(None) => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BulkRename {
    Preset(u64),
    Media { kind: MediaKind, id: String },
}

impl Directory {
    pub fn preset_view(&self) -> bool {
        self.category == Category::Presets && self.search.is_none()
    }

    pub fn bank_view(&self) -> bool {
        self.preset_view() && self.preset_sort.key == SortKey::Banks
    }

    /// The media picker is the Directory filtered to `kind` (I): it opens on that category
    /// with Multi Select, search and pop-ups closed.
    pub fn open_picker(&mut self, kind: MediaKind) {
        self.category = match kind {
            MediaKind::Capture => Category::Captures,
            MediaKind::Ir => Category::Irs,
        };
        self.multi = None;
        self.search = None;
        self.popup = None;
    }

    /// Back closes the innermost Directory layer: pop-up, Multi Select, then search.
    /// Returns false when none is open.
    pub fn close_layer(&mut self) -> bool {
        if let Some(popup) = self.popup.take() {
            // O: CANCEL of the downloading option ends the download pick.
            if matches!(popup, Popup::Paste(_)) {
                self.download = None;
            }
            return true;
        }
        if self.download.take().is_some() {
            self.paste = None;
            self.multi = None;
            return true;
        }
        if self.multi.take().is_some() {
            return true;
        }
        self.search.take().is_some()
    }
}

/// Library rows of the checked media indices (Multi Select in a capture/IR category).
pub(crate) fn checked_media<'a>(
    snapshot: &'a UiSnapshot,
    checked: &BTreeSet<u64>,
) -> Vec<&'a DirectoryMedia> {
    let details = MediaDetails::new(snapshot);
    checked
        .iter()
        .filter_map(|index| snapshot.media.get(*index as usize))
        .filter_map(|entry| details.get(&entry.value))
        .collect()
}

/// Folders a Multi Select copy or move can link into: writable user folders of the open
/// library other than the open one (the library root holds every item already).
pub(crate) fn link_targets(ui: &UiState) -> Vec<&DirectoryFolder> {
    let dir = &ui.directory;
    let Some(kind) = dir.category.media() else {
        return Vec::new();
    };
    ui.snapshot
        .directory
        .folders
        .iter()
        .filter(|folder| {
            folder.kind == kind
                && !folder.root
                && !folder.read_only
                && Some(folder.id) != dir.database_folder
        })
        .collect()
}

/// Duplicate gate (`0x0076692c`): not a root, and the writable folders of the library plus
/// the copied subtree stay below 31 (the 30-folder limit, M815).
pub(crate) fn can_duplicate(view: &DirectoryView, id: i64) -> bool {
    let Some(folder) = view.folders.iter().find(|folder| folder.id == id) else {
        return false;
    };
    if folder.root || folder.read_only {
        return false;
    }
    let writable =
        |candidate: &&DirectoryFolder| candidate.kind == folder.kind && !candidate.read_only;
    let mut subtree = vec![id];
    let mut next = 0;
    while let Some(parent) = subtree.get(next).copied() {
        subtree.extend(
            view.folders
                .iter()
                .filter(|child| child.parent == parent && !child.root && child.id != parent)
                .filter(writable)
                .map(|child| child.id),
        );
        next += 1;
        if subtree.len() > view.folders.len() {
            return false;
        }
    }
    view.folders.iter().filter(writable).count() + subtree.len() < 31
}

/// Presets per bank as the application reports it ([`UiSnapshot::bank_size`]).
pub(crate) fn bank_size(snapshot: &UiSnapshot) -> usize {
    usize::from(snapshot.bank_size.max(1))
}

/// "1A" style slot label for a preset id (setlist slot) with this bank size.
pub(crate) fn slot_label(id: u64, size: usize) -> String {
    let size = size as u64;
    format!("{}{}", id / size + 1, char::from(b'A' + (id % size) as u8))
}

/// Alphabet strip key of a name: its upper-case first letter, or `#`.
pub(crate) fn initial(name: &str) -> u8 {
    match name.trim_start().bytes().next() {
        Some(byte) if byte.is_ascii_alphabetic() => byte.to_ascii_uppercase(),
        _ => b'#',
    }
}

/// List View rows: occupied slots of the open setlist, by the active sort role.
pub(crate) fn preset_rows(ui: &UiState) -> Vec<&PresetItem> {
    let mut rows: Vec<(&PresetItem, Roles)> = ui
        .snapshot
        .presets
        .iter()
        .filter(|preset| {
            !ui.snapshot
                .directory
                .slots
                .iter()
                .any(|slot| slot.id == preset.id && slot.empty)
        })
        .map(|preset| {
            let slot = ui
                .snapshot
                .directory
                .slots
                .iter()
                .find(|slot| slot.id == preset.id);
            let roles = Roles {
                name: &preset.title,
                author: slot.map_or("", |slot| &slot.author),
                date: slot.and_then(|slot| stock_date(&slot.date)),
                instrument: PreferredInstrument::NotSet,
                device: None,
                gain: None,
                id: "",
                pinned: false,
            };
            (preset, roles)
        })
        .collect();
    let sort = ui.directory.preset_sort;
    rows.sort_by(|a, b| sort.order(&a.1, &b.1));
    rows.into_iter().map(|(preset, _)| preset).collect()
}

/// `DirectoryView::media` by item key, built once per list or layout instead of one `find`
/// per row (health r34 #9 d). Keys are unique (one row per product); the first row of a key
/// wins, as `find` did.
pub(crate) struct MediaDetails<'a> {
    items: HashMap<&'a str, &'a DirectoryMedia>,
    pinned: HashSet<&'a str>,
}

impl<'a> MediaDetails<'a> {
    pub fn new(snapshot: &'a UiSnapshot) -> Self {
        let media = &snapshot.directory.media;
        let mut items = HashMap::with_capacity(media.len());
        for item in media {
            items.entry(item.value.as_str()).or_insert(item);
        }
        let pinned = snapshot.directory.pinned_captures.iter();
        Self {
            items,
            pinned: pinned.map(String::as_str).collect(),
        }
    }

    pub fn get(&self, value: &str) -> Option<&'a DirectoryMedia> {
        self.items.get(value).copied()
    }

    /// The sort roles of one capture or IR row.
    pub fn roles(&self, entry: &'a MediaEntry) -> Roles<'a> {
        let detail = self.get(&entry.value);
        let meta = detail.and_then(|item| item.capture.as_ref());
        Roles {
            name: &entry.name,
            author: detail.map_or("", |item| &item.author),
            date: detail.and_then(|item| stock_date(&item.date)),
            instrument: PreferredInstrument::NotSet,
            device: meta.and_then(|meta| meta.device),
            gain: meta.and_then(|meta| meta.gain),
            id: &entry.value,
            pinned: entry.folder.kind() == MediaKind::Capture
                && self.pinned.contains(entry.value.as_str()),
        }
    }
}

/// Capture or IR rows of the open folder, using SQLite membership and the active sort role.
pub(crate) fn media_rows<'a>(
    ui: &'a UiState,
    details: &MediaDetails<'a>,
    kind: MediaKind,
) -> Vec<(u16, &'a MediaEntry)> {
    let dir = &ui.directory;
    let mut rows: Vec<((u16, &MediaEntry), Roles)> = ui
        .snapshot
        .media
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            entry.folder.kind() == kind
                && dir.folder.is_none_or(|folder| folder == entry.folder)
                && dir.database_folder.is_none_or(|folder| {
                    (details.get(&entry.value)).is_some_and(|item| item.folders.contains(&folder))
                })
                && (!dir.made_by_me || entry.folder == MediaFolder::MyCaptures)
                && (kind != MediaKind::Capture || capture_filter(ui, details, &entry.value))
        })
        .map(|(index, entry)| ((index as u16, entry), details.roles(entry)))
        .collect();
    rows.sort_by(|a, b| dir.media_sort.order(&a.1, &b.1));
    rows.into_iter().map(|(row, _)| row).collect()
}

impl UiState {
    /// The Captures Library root rows in the BANK UP capture walk order (`0x808250`,
    /// r23-device §2 steps 3 and 4): read-only (factory) captures first, by device code and
    /// then natural name (`0x542628`); then the other rows in the Directory media sort, pinned
    /// first (`lessThan` `0x755258`). No VERSIONS filter: the caller keeps the block's version.
    /// Stock saves the view filter and sets only VERSIONS for the walk, so the Device Type
    /// and Made by Me filters do not apply. The list holds no folder rows (Tetrad keeps folders
    /// in the side tree). I: Tetrad keeps one media sort for every view (stock: one per view).
    pub fn capture_cycle(&self) -> Vec<CaptureCycleRow<'_>> {
        let details = MediaDetails::new(&self.snapshot);
        let mut rows: Vec<_> = (self.snapshot.media.iter())
            .filter(|entry| entry.folder == MediaFolder::MyCaptures)
            .map(|entry| (entry, details.get(&entry.value), details.roles(entry)))
            .collect();
        rows.sort_by(|a, b| self.directory.media_sort.order(&a.2, &b.2));
        let (mut factory, user): (Vec<_>, Vec<_>) = (rows.into_iter())
            .partition(|(_, detail, _)| detail.is_some_and(|item| item.read_only));
        factory.sort_by(|a, b| {
            (a.2.device.cmp(&b.2.device)).then_with(|| natural_cmp(a.2.name, b.2.name))
        });
        (factory.into_iter().chain(user))
            .map(|(entry, detail, _)| CaptureCycleRow {
                value: &entry.value,
                name: &entry.name,
                version: detail
                    .and_then(|item| item.capture.as_ref())
                    .map_or("", |meta| &meta.version),
            })
            .collect()
    }
}

/// The device type and version groups of the capture filter. Inferred, as
/// `tetrad_library::stock::CaptureFilter`: groups combine with AND, values in a group with OR. A
/// capture without metadata passes only while both groups are open.
fn capture_filter(ui: &UiState, details: &MediaDetails, value: &str) -> bool {
    let dir = &ui.directory;
    let version = dir.version.stock();
    if dir.device_types.is_empty() && version.is_none() {
        return true;
    }
    (details.get(value))
        .and_then(|item| item.capture.as_ref())
        .is_some_and(|meta| {
            (dir.device_types.is_empty()
                || meta
                    .device
                    .is_some_and(|code| dir.device_types.contains(&code)))
                && version.is_none_or(|version| meta.version == version)
        })
}

/// Search matches of one type: preset ids or media indices with their names.
pub(crate) fn search_rows<'a>(ui: &'a UiState, kind: ItemKind) -> Vec<(u64, &'a str)> {
    let query = ui.library_query.to_lowercase();
    // Preset rows are indices into `DirectoryView::presets` (every setlist) when the
    // application sends the index; else slots of the open setlist.
    let index = &ui.snapshot.directory.presets;
    let mut rows: Vec<(u64, &str)> = match kind {
        ItemKind::Preset if !index.is_empty() => (0..)
            .zip(index)
            .map(|(row, preset)| (row, preset.title.as_str()))
            .collect(),
        ItemKind::Preset => ui
            .snapshot
            .presets
            .iter()
            .filter(|preset| {
                !ui.snapshot
                    .directory
                    .slots
                    .iter()
                    .any(|slot| slot.id == preset.id && slot.empty)
            })
            .map(|preset| (preset.id, preset.title.as_str()))
            .collect(),
        ItemKind::Capture | ItemKind::Ir => {
            let media = if kind == ItemKind::Capture {
                MediaKind::Capture
            } else {
                MediaKind::Ir
            };
            ui.snapshot
                .media
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.folder.kind() == media)
                .map(|(index, entry)| (index as u64, entry.name.as_str()))
                .collect()
        }
    };
    rows.retain(|(_, name)| name.to_lowercase().contains(&query));
    let details = MediaDetails::new(&ui.snapshot);
    let roles = |row: &(u64, &'a str)| match kind {
        ItemKind::Preset => {
            let slot = (index.is_empty())
                .then(|| {
                    let slots = &ui.snapshot.directory.slots;
                    slots.iter().find(|slot| slot.id == row.0)
                })
                .flatten();
            Roles {
                name: row.1,
                author: slot.map_or("", |slot| &slot.author),
                date: slot.and_then(|slot| stock_date(&slot.date)),
                instrument: PreferredInstrument::NotSet,
                device: None,
                gain: None,
                id: "",
                pinned: false,
            }
        }
        _ => details.roles(&ui.snapshot.media[row.0 as usize]),
    };
    let mut rows: Vec<_> = rows.into_iter().map(|row| (row, roles(&row))).collect();
    rows.sort_by(|a, b| ui.directory.media_sort.order(&a.1, &b.1));
    rows.into_iter().map(|(row, _)| row).collect()
}

impl UiState {
    pub(crate) fn last_bank(&self) -> usize {
        self.snapshot.presets.len().saturating_sub(1) / bank_size(&self.snapshot)
    }

    pub(crate) fn library(&mut self, control: LibraryControl) -> Vec<UiCommand> {
        use LibraryControl as L;
        match control {
            L::Check(id) if self.pasting().is_some() => {
                self.paste_slot(id);
                return Vec::new();
            }
            L::PasteChoice(choice) => {
                let fits = choice != PasteChoice::FromEmpty || self.empty_slots_fit();
                if let Some(Popup::Paste(checked)) = &mut self.directory.popup
                    && fits
                {
                    *checked = choice;
                }
                return Vec::new();
            }
            L::ConfirmPaste => return self.confirm_paste(),
            _ => {}
        }
        let dir = &mut self.directory;
        match control {
            // "Choose destination Setlist:" (`0x78f628`): the tap opens the setlist and the
            // downloading option (`0x781e58` mode 2 → `0x3a5a88`).
            L::Setlist(id) if dir.download.as_ref().is_some_and(|pick| !pick.chosen) => {
                if let Some(pick) = &mut dir.download {
                    pick.chosen = true;
                }
                dir.cloud = None;
                dir.bank = 0;
                dir.revealed = None;
                self.reset_scroll(ListId::Banks);
                self.reset_scroll(ListId::DirectoryItems);
                self.open_popup(Popup::Paste(PasteChoice::Manual));
                return vec![UiCommand::OpenSetlist { id }];
            }
            L::Setlist(id) => {
                dir.cloud = None;
                dir.bank = 0;
                dir.revealed = None;
                self.reset_scroll(ListId::Banks);
                self.reset_scroll(ListId::DirectoryItems);
                return vec![UiCommand::OpenSetlist { id }];
            }
            L::Search => {
                if dir.search.is_none() {
                    dir.search = Some(match dir.category {
                        Category::Captures => ItemKind::Capture,
                        Category::Irs => ItemKind::Ir,
                        _ => ItemKind::Preset,
                    });
                    dir.multi = None;
                }
                let query = self.library_query.clone();
                self.open_keyboard(TextTarget::LibrarySearch, query);
            }
            L::ClearSearch => {
                self.library_query.clear();
                dir.search = None;
            }
            L::NewSetlist => self.open_keyboard(TextTarget::SetlistName, String::new()),
            L::Category => self.open_popup(Popup::Category),
            L::Bank(page) => self.directory.bank = usize::from(page).min(self.last_bank()),
            L::Favorite(index) => {
                if let Some(item) = directory_favorites(self).get(index as usize) {
                    if item.kind != ItemKind::Preset {
                        if let Some(index) = self
                            .snapshot
                            .media
                            .iter()
                            .position(|entry| entry.value == item.value)
                        {
                            return self.library(L::Reveal(index as u16));
                        }
                        return Vec::new();
                    }
                    if !item.value.is_empty() {
                        return vec![UiCommand::Directory(DirectoryRequest::LoadItem {
                            kind: item.kind,
                            folder: item.folder.clone(),
                            id: item.value.clone(),
                        })];
                    }
                }
                let index = directory_favorites(self)
                    .get(index as usize)
                    .and_then(|item| item.legacy_index)
                    .unwrap_or(index);
                return vec![UiCommand::LoadFavorite { index }];
            }
            L::Unfavorite(index) => {
                if let Some(item) = directory_favorites(self).get(index as usize)
                    && item.kind != ItemKind::Preset
                {
                    return vec![UiCommand::Directory(DirectoryRequest::FavoriteMedia {
                        kind: if item.kind == ItemKind::Capture {
                            MediaKind::Capture
                        } else {
                            MediaKind::Ir
                        },
                        folder: item.folder.clone(),
                        id: item.value.clone(),
                        favorite: false,
                    })];
                }
                if let Some(item) = directory_favorites(self).get(index as usize) {
                    if !item.value.is_empty() {
                        return vec![UiCommand::Directory(DirectoryRequest::FavoritePreset {
                            path: item.value.clone(),
                            favorite: false,
                        })];
                    }
                    return vec![UiCommand::RemoveFavorite {
                        index: item.legacy_index.unwrap_or(index),
                    }];
                }
                return Vec::new();
            }
            L::Audition(index) => {
                if let Some(item) = directory_favorites(self).get(index as usize) {
                    return vec![UiCommand::Directory(DirectoryRequest::Audition {
                        kind: item.kind,
                        folder: item.folder.clone(),
                        id: item.value.clone(),
                    })];
                }
            }
            L::DatabaseFolder(id) => {
                dir.cloud = None;
                dir.folder = None;
                dir.database_folder = Some(id);
                dir.selected_media = None;
                self.reset_scroll(ListId::DirectoryItems);
            }
            L::DatabaseFolderMenu(id) => self.open_popup(Popup::DatabaseFolder(id)),
            L::PluginFolder(index) => {
                dir.plugin_folder = Some(index);
                dir.plugin_child = None;
                self.reset_scroll(ListId::DirectoryItems);
            }
            L::PluginChild(index) => {
                dir.plugin_child = Some(index);
                self.reset_scroll(ListId::DirectoryItems);
            }
            // Stock auditions Downloads and Plugin presets (`0x7c2974`): load without a slot.
            L::PluginPreset(index) => {
                let plugin = dir
                    .plugin_folder
                    .and_then(|folder| self.snapshot.directory.plugins.get(usize::from(folder)));
                if let Some(plugin) = plugin
                    && let Some(item) = plugin.presets.get(usize::from(index))
                {
                    return vec![UiCommand::Directory(DirectoryRequest::Audition {
                        kind: ItemKind::Preset,
                        folder: plugin.path.clone(),
                        id: item.value.clone(),
                    })];
                }
            }
            L::ConfirmMove { overwrite } => {
                if let Some(Popup::MoveConflict { from, to }) = dir.popup.take() {
                    return self.move_preset(from, to, overwrite);
                }
            }
            L::NewFolder => {
                if let Some(kind) = dir.category.media() {
                    let parent = dir.database_folder.or_else(|| {
                        self.snapshot
                            .directory
                            .folders
                            .iter()
                            .find(|folder| folder.kind == kind && folder.root)
                            .map(|folder| folder.id)
                    });
                    if let Some(parent) = parent {
                        self.open_keyboard(
                            TextTarget::DirectoryName(DirectoryNameEdit::NewFolder {
                                kind,
                                parent,
                            }),
                            String::new(),
                        );
                    }
                }
            }
            L::ConfirmDirectoryDelete { remove_products } => {
                return self.confirm_directory_delete(remove_products);
            }
            L::Done => {
                dir.popup = None;
                dir.multi = None;
                dir.search = None;
                return self.pop();
            }
            L::MultiSelect => {
                dir.paste = None;
                dir.multi = match dir.multi {
                    Some(_) => None,
                    None => Some(BTreeSet::new()),
                };
            }
            L::Sort => self.open_popup(Popup::Sort),
            L::Filter => self.open_popup(Popup::Filter),
            L::MadeByMe => {
                dir.made_by_me = !dir.made_by_me;
                dir.popup = None;
                self.reset_scroll(ListId::DirectoryItems);
            }
            L::Library | L::Folder(_) => {
                dir.cloud = None;
                dir.database_folder = None;
                dir.folder = match control {
                    L::Folder(folder) => Some(folder),
                    _ => None,
                };
                dir.selected_media = None;
                self.reset_scroll(ListId::DirectoryItems);
            }
            L::Kind(kind) => {
                match &mut dir.search {
                    Some(tab) => *tab = kind,
                    None => dir.favorites = kind,
                }
                self.reset_scroll(ListId::Favorites);
                self.reset_scroll(ListId::PresetResults);
            }
            L::Recent(recent) => {
                dir.recent = recent;
                self.reset_scroll(ListId::Favorites);
            }
            L::SetlistMenu(id) => self.open_popup(Popup::Setlist(id)),
            L::FolderMenu(folder) => self.open_popup(Popup::Folder(folder)),
            L::Media(index) => {
                dir.selected_media = Some(index);
                let Some(entry) = self.snapshot.media.get(usize::from(index)) else {
                    return Vec::new();
                };
                let value = entry.value.clone();
                return self.start_add_to_grid(match entry.folder.kind() {
                    MediaKind::Capture => GridMedia::Capture(value),
                    MediaKind::Ir => GridMedia::Ir(value),
                });
            }
            L::Reveal(index) => {
                let Some(entry) = self.snapshot.media.get(usize::from(index)) else {
                    return Vec::new();
                };
                let kind = entry.folder.kind();
                dir.search = None;
                dir.category = match kind {
                    MediaKind::Capture => Category::Captures,
                    MediaKind::Ir => Category::Irs,
                };
                dir.database_folder = None;
                dir.folder = Some(entry.folder);
                dir.made_by_me = false;
                dir.selected_media = Some(index);
                let details = MediaDetails::new(&self.snapshot);
                let row = media_rows(self, &details, kind)
                    .iter()
                    .position(|(row, _)| *row == index)
                    .unwrap_or(0);
                self.scroll
                    .insert(ListId::DirectoryItems, (row as u32 * ITEM_ROW) as i32);
            }
            // Manual "SEARCHING ITEMS": a result opens its setlist with the preset shown.
            L::RevealPreset(row) => {
                let Some(preset) = self.snapshot.directory.presets.get(row as usize) else {
                    return Vec::new();
                };
                let (setlist, slot) = (preset.setlist, preset.slot);
                dir.search = None;
                dir.category = Category::Presets;
                dir.revealed = Some(slot);
                dir.bank = slot as usize / bank_size(&self.snapshot);
                self.scroll
                    .insert(ListId::DirectoryItems, (slot as u32 * ITEM_ROW) as i32);
                return vec![UiCommand::OpenSetlist { id: setlist }];
            }
            L::MediaMenu(index) => self.open_popup(Popup::Media(index)),
            L::RecentSearch(index) => {
                if let Some(query) = dir.recent_searches.get(usize::from(index)) {
                    self.library_query = query.clone();
                    self.reset_scroll(ListId::PresetResults);
                }
            }
            L::Index(letter) => self.jump(letter),
            L::Jump(position) => self.jump_media(position),
            L::TypeFilter(code) => {
                if !dir.device_types.remove(&code) {
                    dir.device_types.insert(code);
                }
                self.reset_scroll(ListId::DirectoryItems);
            }
            L::Version(version) => {
                dir.version = version;
                self.reset_scroll(ListId::DirectoryItems);
            }
            L::Check(id) => {
                if let Some(checked) = &mut dir.multi
                    && !checked.remove(&id)
                {
                    checked.insert(id);
                }
            }
            L::Bulk(BulkAction::Copy) => {
                let checked = dir.multi.clone().unwrap_or_default();
                dir.clipboard = match dir.category.media() {
                    Some(_) => Some(Clipboard::Media(
                        checked_media(&self.snapshot, &checked)
                            .into_iter()
                            .map(|item| item.value.clone())
                            .collect(),
                    )),
                    None => self
                        .snapshot
                        .active_setlist_name()
                        .map(|setlist| Clipboard::Presets {
                            setlist: setlist.to_owned(),
                            ids: checked.into_iter().collect(),
                        }),
                };
                self.open_popup(Popup::Target { remove: false });
            }
            L::Bulk(BulkAction::Paste) => return self.bulk_paste(),
            L::Bulk(BulkAction::Download) => return self.bulk_download(),
            L::Bulk(BulkAction::Favorite) => return self.bulk_favorite(),
            L::Bulk(BulkAction::Upload) => {
                let checked = dir.multi.take().unwrap_or_default();
                // Stock has no IR upload (re-r44 §1): an IR folder builds no item.
                let items: Vec<UploadItem> = match dir.category.media() {
                    Some(MediaKind::Capture) => checked_media(&self.snapshot, &checked)
                        .into_iter()
                        .filter(|item| !item.read_only)
                        .map(|item| UploadItem::Capture(item.value.clone()))
                        .collect(),
                    Some(MediaKind::Ir) => Vec::new(),
                    None => checked.into_iter().map(UploadItem::Slot).collect(),
                };
                return if items.is_empty() {
                    Vec::new()
                } else {
                    vec![UiCommand::Upload { items }]
                };
            }
            L::Bulk(BulkAction::Edit) => {
                let checked = dir.multi.take().unwrap_or_default();
                dir.rename_queue = match dir.category.media() {
                    Some(kind) => checked_media(&self.snapshot, &checked)
                        .into_iter()
                        .filter(|item| !item.read_only)
                        .map(|item| BulkRename::Media {
                            kind,
                            id: item.value.clone(),
                        })
                        .collect(),
                    None => checked.into_iter().map(BulkRename::Preset).collect(),
                };
                self.next_bulk_rename();
            }
            L::Bulk(BulkAction::Move) => self.open_popup(Popup::Target { remove: true }),
            L::Bulk(BulkAction::Delete) => self.open_popup(Popup::ConfirmDelete),
            L::ConfirmBulkDelete => {
                dir.popup = None;
                let ids = dir.multi.take().unwrap_or_default();
                let Some(kind) = dir.category.media() else {
                    return ids
                        .into_iter()
                        .map(|id| UiCommand::DeletePreset { id })
                        .collect();
                };
                // Factory items are omitted ("Factory Captures will be omitted.").
                return checked_media(&self.snapshot, &ids)
                    .into_iter()
                    .filter(|item| !item.read_only)
                    .map(|item| {
                        UiCommand::Directory(DirectoryRequest::DeleteMedia {
                            kind,
                            id: item.value.clone(),
                        })
                    })
                    .collect();
            }
            L::ConfirmBulkRemove => {
                dir.popup = None;
                let ids = dir.multi.take().unwrap_or_default();
                let Some(folder) = dir.database_folder else {
                    return Vec::new();
                };
                return vec![UiCommand::LinkMedia {
                    folder,
                    ids: checked_media(&self.snapshot, &ids)
                        .into_iter()
                        .map(|item| item.value.clone())
                        .collect(),
                    linked: false,
                }];
            }
            L::Item(action) => return self.item_action(action),
            L::Downloads => return self.open_cloud(CloudScope::Downloads),
            L::Cloud => return self.open_cloud(CloudScope::Cloud),
            L::Refresh if dir.cloud.is_some() => return self.cloud_page(true),
            L::CloudItem(index) => return self.cloud_item(index),
            // Upload Mode and the IR/plugin REFRESH outside a cloud folder are not built: the
            // stock offline notice (`0x11e23e0`).
            L::Upload | L::Refresh => self.show_toast(
                "You need to be connected to the Internet via Wi-Fi to use the Cloud service.",
            ),
            L::PasteChoice(_) | L::ConfirmPaste => {}
        }
        Vec::new()
    }

    /// PASTE (`0x786d38`). Presets: the pasting option dialog opens (`0x3a5a88` case 0); a
    /// waiting PASTE pastes into its slots ("Pasting %1 presets, please wait.", `0x786ac4`).
    /// Captures and IRs link into the open user folder. The clipboard stays.
    fn bulk_paste(&mut self) -> Vec<UiCommand> {
        if self.pasting().is_some() {
            return self.commit_paste();
        }
        let dir = &mut self.directory;
        match (&dir.clipboard, dir.category.media()) {
            (Some(Clipboard::Presets { .. }), None) => {
                if self.snapshot.active_setlist.is_some() {
                    self.open_popup(Popup::Paste(PasteChoice::Manual));
                }
                Vec::new()
            }
            (Some(Clipboard::Media(ids)), Some(_)) => {
                let commands = dir.database_folder.map(|folder| UiCommand::LinkMedia {
                    folder,
                    ids: ids.clone(),
                    linked: true,
                });
                dir.multi = None;
                commands.into_iter().collect()
            }
            _ => Vec::new(),
        }
    }

    /// DOWNLOAD (`0x794910`) of the checked rows of a cloud folder. Captures and IRs: each
    /// product not yet on the unit downloads into its destination (`0x76be70(root, 2 | 1)`,
    /// "Downloading %1, please wait." `0x7867c4`: %1 "Neural Captures" or "IRs"), then Multi
    /// Select ends. Presets (`0x794768`): the setlist list asks "Choose destination Setlist:"
    /// (state `0x1b`), then the downloading option.
    fn bulk_download(&mut self) -> Vec<UiCommand> {
        let checked = self.directory.multi.take().unwrap_or_default();
        let Some(folder) = self.open_cloud_folder() else {
            return Vec::new();
        };
        if self.directory.category == Category::Presets {
            let ids: Vec<String> = (folder.rows.iter().enumerate())
                .filter(|(index, _)| checked.contains(&(*index as u64)))
                .map(|(_, row)| row.id.clone())
                .collect();
            if !ids.is_empty() {
                self.directory.download = Some(PresetDownload { ids, chosen: false });
            }
            return Vec::new();
        }
        let noun = match self.directory.category {
            Category::Captures => "Neural Captures",
            Category::Irs => "IRs",
            _ => return Vec::new(),
        };
        let ids: Vec<String> = (folder.rows.iter().enumerate())
            .filter(|(index, row)| checked.contains(&(*index as u64)) && !row.stored)
            .map(|(_, row)| row.id.clone())
            .collect();
        if ids.is_empty() {
            return Vec::new();
        }
        self.show_toast(format!("Downloading {noun}, please wait."));
        vec![UiCommand::DownloadProducts { ids }]
    }

    /// The waiting PASTE, while Multi Select is on.
    pub(crate) fn pasting(&self) -> Option<&PasteSlots> {
        let dir = &self.directory;
        dir.paste.as_ref().filter(|_| dir.multi.is_some())
    }

    /// The copied preset slots, when the clipboard holds presets.
    pub(crate) fn copied_presets(&self) -> &[u64] {
        match &self.directory.clipboard {
            Some(Clipboard::Presets { ids, .. }) => ids,
            _ => &[],
        }
    }

    /// How many presets the waiting PASTE or DOWNLOAD places.
    pub(crate) fn pasted_count(&self) -> usize {
        match &self.directory.download {
            Some(pick) => pick.ids.len(),
            None => self.copied_presets().len(),
        }
    }

    /// The verb of the option texts (`0x781e58`: mode 0 Paste, 2 Download).
    pub(crate) fn paste_verb(&self) -> &'static str {
        if self.directory.download.is_some() {
            "Download"
        } else {
            "Paste"
        }
    }

    /// Slot `slot` of the open setlist holds a preset. The device lists every slot of the
    /// open setlist in `directory.slots` with its `empty` flag (an empty slot is also a
    /// "Empty" row of `presets`); without that list every listed preset is a filled slot.
    pub(crate) fn slot_filled(&self, slot: u64) -> bool {
        let slots = &self.snapshot.directory.slots;
        if slots.is_empty() {
            return self.snapshot.presets.iter().any(|preset| preset.id == slot);
        }
        slots.iter().any(|row| row.id == slot && !row.empty)
    }

    /// The open setlist has an empty slot for each copied preset: the stock enable test of
    /// the third row (`0x846768` → `0x3a61d4`) counts the target's empty-key slots and
    /// compares them with the items to place.
    pub(crate) fn empty_slots_fit(&self) -> bool {
        let used = (0..SETLIST_SLOTS)
            .filter(|slot| self.slot_filled(*slot))
            .count();
        SETLIST_SLOTS as usize - used >= self.pasted_count()
    }

    /// PASTE / CONTINUE of the pasting option dialog (`0x795b18`): the first empty slots now
    /// (mode 2), or Multi Select waits for slot taps (modes 0 and 1).
    fn confirm_paste(&mut self) -> Vec<UiCommand> {
        let Some(Popup::Paste(choice)) = self.directory.popup.take() else {
            return Vec::new();
        };
        let dir = &mut self.directory;
        dir.paste = match choice {
            PasteChoice::Manual => Some(PasteSlots::Manual(Vec::new())),
            PasteChoice::FromChosen => Some(PasteSlots::Consecutive(None)),
            PasteChoice::FromEmpty if dir.download.is_some() => {
                return self.download_presets(PasteTarget::FirstEmpty);
            }
            PasteChoice::FromEmpty => {
                let Some(Clipboard::Presets { setlist: from, ids }) = &dir.clipboard else {
                    return Vec::new();
                };
                let command =
                    (self.snapshot.active_setlist).map(|setlist| UiCommand::CopyPresets {
                        ids: ids.clone(),
                        from: from.clone(),
                        setlist,
                        remove: false,
                        to: PasteTarget::FirstEmpty,
                    });
                dir.multi = None;
                return command.into_iter().collect();
            }
        };
        dir.multi = Some(BTreeSet::new());
        Vec::new()
    }

    /// A slot tap while a PASTE waits: manual mode adds the slot (or drops a tapped one);
    /// consecutive mode sets the first slot (`0x848e18`). The rows show the target slots.
    fn paste_slot(&mut self, slot: u64) {
        let count = self.pasted_count();
        let Some(paste) = &mut self.directory.paste else {
            return;
        };
        match paste {
            PasteSlots::Manual(slots) => match slots.iter().position(|s| *s == slot) {
                Some(index) => {
                    slots.remove(index);
                }
                None if slots.len() < count => slots.push(slot),
                None => {}
            },
            PasteSlots::Consecutive(first) => *first = Some(slot),
        }
        let targets = paste.targets(count);
        self.directory.multi = Some(targets.into_iter().collect());
    }

    /// PASTE of a waiting PASTE: copied preset `n` goes to target slot `n` of the open
    /// setlist; the paste ends and Multi Select closes (`0x786d38` → `0x782684`).
    fn commit_paste(&mut self) -> Vec<UiCommand> {
        let targets =
            (self.pasting()).map_or_else(Vec::new, |paste| paste.targets(self.pasted_count()));
        if self.directory.download.is_some() {
            return if targets.is_empty() {
                Vec::new()
            } else {
                self.download_presets(PasteTarget::Slots(targets))
            };
        }
        let dir = &self.directory;
        let (Some(Clipboard::Presets { setlist: from, ids }), Some(setlist)) =
            (&dir.clipboard, self.snapshot.active_setlist)
        else {
            return Vec::new();
        };
        if targets.is_empty() {
            return Vec::new();
        }
        // A consecutive paste stops at slot 256 (`0x848e18`); O: the rest is named, not dropped
        // without a word (review r29 B7).
        let left_out = ids.len().saturating_sub(targets.len());
        let command = UiCommand::CopyPresets {
            ids: ids[..targets.len()].to_vec(),
            from: from.clone(),
            setlist,
            remove: false,
            to: PasteTarget::Slots(targets),
        };
        self.directory.paste = None;
        self.directory.multi = None;
        if left_out > 0 {
            // Review r30 C9 (O text): "1 preset", "2 presets".
            let presets = if left_out == 1 { "preset" } else { "presets" };
            self.show_toast(format!("{left_out} {presets} did not fit in the setlist"));
        }
        vec![command]
    }

    /// The end of a DOWNLOAD pick (`onMultiselectionPasteSelected` `0x795b18` in mode 2):
    /// the products in order into the open setlist at `to`; Multi Select closes. The device
    /// checks every target before the first download.
    ///
    /// An occupied target slot opens "Overwrite Existing Presets?" first. Stock asks at the
    /// store of the first occupied item (`0x1dcf2c` → `0x46485c(name, 1)`) and keeps that
    /// answer for the rest of the run (`DAT_0154af3c`, C); Tetrad asks once before the first
    /// download (I: the same result while the slots do not change).
    fn download_presets(&mut self, to: PasteTarget) -> Vec<UiCommand> {
        let dir = &mut self.directory;
        let (Some(pick), Some(setlist)) = (dir.download.take(), self.snapshot.active_setlist)
        else {
            return Vec::new();
        };
        dir.paste = None;
        dir.multi = None;
        let mut ids = pick.ids;
        if let PasteTarget::Slots(slots) = &to {
            ids.truncate(slots.len());
            if slots.iter().any(|slot| self.slot_filled(*slot)) {
                let slots = slots.clone();
                self.overlay = Some(Overlay::DownloadOverwrite {
                    ids,
                    setlist,
                    slots,
                });
                return Vec::new();
            }
        }
        vec![UiCommand::DownloadPresets {
            ids,
            setlist,
            to,
            overwrite: false,
        }]
    }

    /// "Overwrite Existing Presets?" answered: OVERWRITE (`0x1d8a04`: the store as for an
    /// empty slot) sends every product with `overwrite`; SKIP (`0x1da63c`: that item ends
    /// unstored) and every other close send only the products whose slot is empty (O: never
    /// an overwrite without the explicit OVERWRITE tap).
    pub(crate) fn answer_download_overwrite(
        &mut self,
        ids: Vec<String>,
        setlist: u64,
        slots: Vec<u64>,
        overwrite: bool,
    ) -> Vec<UiCommand> {
        let (ids, slots) = if overwrite {
            (ids, slots)
        } else {
            ids.into_iter()
                .zip(slots)
                .filter(|(_, slot)| !self.slot_filled(*slot))
                .unzip()
        };
        if ids.is_empty() {
            return Vec::new();
        }
        vec![UiCommand::DownloadPresets {
            ids,
            setlist,
            to: PasteTarget::Slots(slots),
            overwrite,
        }]
    }

    /// FAVORITE (`0x783aa0`, "Adding to Favorites, please wait."): adds every checked item,
    /// then leaves Multi Select.
    fn bulk_favorite(&mut self) -> Vec<UiCommand> {
        let checked = self.directory.multi.take().unwrap_or_default();
        let Some(kind) = self.directory.category.media() else {
            return vec![UiCommand::FavoritePresets {
                ids: checked.into_iter().collect(),
            }];
        };
        let folder = match kind {
            MediaKind::Capture => "local_nc_root",
            MediaKind::Ir => "local_ir_root",
        };
        checked_media(&self.snapshot, &checked)
            .into_iter()
            .map(|item| {
                UiCommand::Directory(DirectoryRequest::FavoriteMedia {
                    kind,
                    folder: folder.to_owned(),
                    id: item.value.clone(),
                    favorite: true,
                })
            })
            .collect()
    }

    /// Opens the keyboard for the next item of a Multi Select EDIT.
    pub(crate) fn next_bulk_rename(&mut self) {
        if self.directory.rename_queue.is_empty() {
            return;
        }
        let (target, name) = match self.directory.rename_queue.remove(0) {
            BulkRename::Preset(id) => (
                TextTarget::PresetRename(id),
                (self.snapshot.presets.iter())
                    .find(|preset| preset.id == id)
                    .map(|preset| preset.title.clone())
                    .unwrap_or_default(),
            ),
            BulkRename::Media { kind, id } => {
                let name = (self.snapshot.media.iter())
                    .find(|entry| entry.value == id)
                    .map(|entry| entry.name.clone())
                    .unwrap_or_default();
                (
                    TextTarget::DirectoryName(DirectoryNameEdit::Media { kind, id }),
                    name,
                )
            }
        };
        self.open_keyboard(target, name);
    }

    fn move_preset(&self, from: u64, to: u64, overwrite: bool) -> Vec<UiCommand> {
        let setlist = self
            .snapshot
            .setlists
            .iter()
            .find(|s| Some(s.id) == self.snapshot.active_setlist);
        setlist
            .map(|setlist| {
                UiCommand::Directory(DirectoryRequest::MovePreset {
                    setlist: setlist.title.clone(),
                    from: from as usize,
                    to: to as usize,
                    overwrite,
                })
            })
            .into_iter()
            .collect()
    }

    /// A preset row's ⋮ (or a long press on the row).
    pub(crate) fn preset_menu(&mut self, id: u64) {
        self.open_popup(Popup::Preset(id));
    }

    /// DOWNLOADS or CLOUD of the category. Without internet or account the stock dialog
    /// `0x5b0958` opens instead (I: the folder rows are drawn enabled, and the tap is the
    /// trigger; `updateCloudFoldersEnabled` `0x78ff0c` follows the same state). Each open asks
    /// the first page again (I: `fetchTop` is not traced).
    fn open_cloud(&mut self, scope: CloudScope) -> Vec<UiCommand> {
        if let Some(gate) = CloudGate::of(&self.snapshot) {
            self.overlay = Some(Overlay::CloudUnavailable(gate));
            return Vec::new();
        }
        let dir = &mut self.directory;
        dir.cloud = Some(scope);
        dir.multi = None;
        dir.selected_media = None;
        self.reset_scroll(ListId::CloudItems);
        self.cloud_page(true)
    }

    /// A cloud row tap. A CLOUD row (`cloudDownloadClicked` `0x79992c`: preset `0x719f90`,
    /// capture `0x1d9954`, IR `0x799134`) and a DOWNLOADS row
    /// not on this unit (`downloadsItemClicked` `0x78d204`) download the product. A stored
    /// DOWNLOADS preset closes the Directory and loads with no slot (`0x5b37f0`, index -1).
    /// A stored capture starts Add to Grid (`captureSelected` → `startAddNeuralCaptureMode`
    /// `0x593894`) with its product id; the device finds its hash (`0x214b84`).
    fn cloud_item(&mut self, index: u16) -> Vec<UiCommand> {
        let (Some(folder), Some(kind)) = (
            self.open_cloud_folder(),
            self.directory.category.cloud_kind(),
        ) else {
            return Vec::new();
        };
        let Some(row) = folder.rows.get(usize::from(index)) else {
            return Vec::new();
        };
        let id = row.id.clone();
        match (folder.scope == CloudScope::Downloads && row.stored, kind) {
            // A cloud IR goes into the IR saving destination (`0x799134`; the device store
            // `store_cloud_ir`).
            (false, _) | (_, ItemKind::Ir) => vec![UiCommand::DownloadProduct { id }],
            (true, ItemKind::Preset) => {
                let mut commands = self.pop();
                commands.push(UiCommand::Directory(DirectoryRequest::Audition {
                    kind,
                    folder: DOWNLOADS_PRESETS.to_owned(),
                    id,
                }));
                commands
            }
            (true, _) => self.start_add_to_grid(GridMedia::Capture(id)),
        }
    }

    /// A capture or IR tap in the Directory (`captureClicked` `0x77f860`, `irClicked`
    /// `0x790d28`): the Directory closes and the Grid enters Add to Grid mode (`0x593894`,
    /// `0x593950`). The media picker of a block editor has its own rows, so it never starts
    /// the mode (as stock `0x598b10` for an open IR Loader editor, I).
    fn start_add_to_grid(&mut self, media: GridMedia) -> Vec<UiCommand> {
        let mut commands = self.navigate(Screen::Grid);
        self.add_to_grid = Some(media.clone());
        commands.push(UiCommand::StartAddToGrid(media));
        commands
    }

    pub(crate) fn open_cloud_folder(&self) -> Option<&CloudFolderView> {
        let kind = self.directory.category.cloud_kind()?;
        (self.snapshot).cloud_folder(kind, self.directory.cloud?)
    }

    /// A page of the open cloud folder: the first (`refresh`), or the next one while the
    /// last page was full and none is out (fetchMore `0x431178`).
    fn cloud_page(&self, refresh: bool) -> Vec<UiCommand> {
        let dir = &self.directory;
        let (Some(kind), Some(scope)) = (dir.category.cloud_kind(), dir.cloud) else {
            return Vec::new();
        };
        let wanted = refresh
            || (self.open_cloud_folder()).is_some_and(|folder| folder.more && !folder.fetching);
        (wanted && self.snapshot.cloud_ready())
            .then_some(UiCommand::BrowseCloud {
                kind,
                scope,
                refresh,
            })
            .into_iter()
            .collect()
    }

    /// A scroll of `list` reached its end: the open cloud folder asks its next page (Qt
    /// `canFetchMore`/`fetchMore` as the view reaches the last row).
    pub(crate) fn scrolled_to_end(&self, list: ListId) -> Vec<UiCommand> {
        if list == ListId::CloudItems && self.screen() == Screen::Presets {
            self.cloud_page(false)
        } else {
            Vec::new()
        }
    }

    fn open_popup(&mut self, popup: Popup) {
        self.reset_scroll(ListId::Choice);
        self.directory.popup = Some(popup);
    }

    /// Health r34 #1 (O): a popup, confirm or rename that acts on one list row keeps that
    /// row's key across a new snapshot (`old` → `self.snapshot`). A capture or IR follows its
    /// product id and a setlist its name to their new positions; a preset slot stays only
    /// while the browsed setlist, the slot's title and its preset key are unchanged. When the item is gone,
    /// the popup closes and nothing is sent, so a later confirm cannot act on another item.
    pub(crate) fn follow_rows(&mut self, old: &UiSnapshot) {
        let s = &self.snapshot;
        let media = |index: u16| {
            let value = &old.media.get(usize::from(index))?.value;
            let at = s.media.iter().position(|entry| entry.value == *value)?;
            u16::try_from(at).ok()
        };
        let setlist = |id: u64| {
            let title = &old.setlists.iter().find(|item| item.id == id)?.title;
            Some(s.setlists.iter().find(|item| item.title == *title)?.id)
        };
        let browsed = |snapshot: &UiSnapshot| {
            let id = snapshot.active_setlist?;
            Some(
                snapshot
                    .setlists
                    .iter()
                    .find(|item| item.id == id)?
                    .title
                    .clone(),
            )
        };
        // Review r35 G9 (O): a slot row is its title and its preset `key`, so a new preset
        // with the same title (a same-name OVERWRITE, a delete and a paste) is another row.
        let row = |snapshot: &UiSnapshot, slot: u64| {
            let slots = &snapshot.directory.slots;
            match slots.iter().find(|row| row.id == slot) {
                Some(row) => Some((row.title.clone(), row.key)),
                None if slots.is_empty() => (snapshot.presets.iter())
                    .find(|preset| preset.id == slot)
                    .map(|preset| (preset.title.clone(), 0)),
                None => None,
            }
        };
        let same_setlist = browsed(s) == browsed(old);
        let slot_kept = |slot: u64| same_setlist && row(s, slot) == row(old, slot);
        let presets = self.directory.category.media().is_none();
        // Checked slots of the browsed setlist (not cloud rows, not PASTE targets).
        let dir = &self.directory;
        let slot_checks =
            dir.category == Category::Presets && dir.cloud.is_none() && dir.paste.is_none();
        let checked_kept = || (self.directory.multi.iter().flatten()).all(|&slot| slot_kept(slot));
        let popup = match self.directory.popup {
            Some(Popup::Media(index)) => media(index).map(Popup::Media),
            Some(Popup::Delete(DirectoryDelete::Media(index))) => {
                media(index).map(|at| Popup::Delete(DirectoryDelete::Media(at)))
            }
            Some(Popup::Setlist(id)) => setlist(id).map(Popup::Setlist),
            Some(Popup::Delete(DirectoryDelete::Setlist(id))) => {
                setlist(id).map(|at| Popup::Delete(DirectoryDelete::Setlist(at)))
            }
            Some(Popup::Preset(slot) | Popup::MovePreset(slot)) if !slot_kept(slot) => None,
            Some(Popup::MoveConflict { from, to }) if !(slot_kept(from) && slot_kept(to)) => None,
            Some(Popup::ConfirmDelete) if presets && !checked_kept() => None,
            Some(Popup::Target { .. }) if slot_checks && !checked_kept() => None,
            other => other,
        };
        // Review r35 G2 (O): checked slots follow their rows at every snapshot, not only under
        // an open confirm. A slot whose title changed drops out; a new browsed setlist clears
        // every check, so a later DELETE or MOVE acts only on slots the user saw checked.
        if slot_checks && let Some(checked) = &mut self.directory.multi {
            checked.retain(|&slot| slot_kept(slot));
        }
        let overlay = match self.overlay.take() {
            Some(Overlay::Confirm(Confirm::DeletePreset(slot))) if !slot_kept(slot) => None,
            Some(Overlay::DownloadOverwrite {
                ids,
                setlist: id,
                slots,
            }) => setlist(id).map(|setlist| Overlay::DownloadOverwrite {
                ids,
                setlist,
                slots,
            }),
            other => other,
        };
        let renamed = (self.keyboard.as_ref()).map(|keyboard| &keyboard.target);
        if let Some(TextTarget::PresetRename(slot)) = renamed
            && !slot_kept(*slot)
        {
            self.keyboard = None;
            self.directory.rename_queue.clear();
        }
        self.directory.popup = popup;
        self.overlay = overlay;
    }

    /// Hold COPY (500 ms) in Multi Select: move the selection (manual §5).
    pub(crate) fn hold_copy(&mut self) -> bool {
        let movable = self
            .directory
            .multi
            .as_ref()
            .is_some_and(|checked| !checked.is_empty());
        if movable {
            self.open_popup(Popup::Target { remove: true });
        }
        movable
    }

    /// A row of a dropdown list opened by the Directory.
    pub(crate) fn directory_choose(&mut self, index: u16) -> Vec<UiCommand> {
        let index = usize::from(index);
        let dir = &mut self.directory;
        match dir.popup.take() {
            Some(Popup::Category) => {
                if let Some(category) = Category::ALL.get(index) {
                    dir.category = *category;
                    dir.cloud = None;
                    dir.folder = None;
                    dir.database_folder = None;
                    dir.selected_media = None;
                    dir.multi = None;
                    dir.made_by_me = false;
                    dir.plugin_folder = None;
                    dir.plugin_child = None;
                    self.reset_scroll(ListId::Setlists);
                    self.reset_scroll(ListId::DirectoryItems);
                    self.reset_scroll(ListId::Favorites);
                }
            }
            Some(Popup::Sort) => {
                let presets = dir.preset_view();
                let keys = sort_keys(dir);
                if let Some(key) = keys.get(index).copied() {
                    let sort = if presets {
                        &mut dir.preset_sort
                    } else {
                        &mut dir.media_sort
                    };
                    sort.descending = sort.key == key && !sort.descending;
                    sort.key = key;
                    dir.media_descending = dir.media_sort.descending;
                }
                self.reset_scroll(ListId::DirectoryItems);
            }
            Some(Popup::MovePreset(from)) => {
                let slots = &self.snapshot.directory.slots;
                let (target, occupied) = if slots.is_empty() {
                    let target = self.snapshot.presets.get(index).map(|preset| preset.id);
                    (target, true)
                } else {
                    let slot = slots.get(index);
                    (
                        slot.map(|slot| slot.id),
                        slot.is_some_and(|slot| !slot.empty),
                    )
                };
                match target {
                    Some(to) if occupied && to != from => {
                        self.open_popup(Popup::MoveConflict { from, to });
                    }
                    Some(to) => return self.move_preset(from, to, false),
                    None => {}
                }
            }
            Some(Popup::Target { remove }) if dir.category.media().is_some() => {
                let checked = dir.multi.take().unwrap_or_default();
                let from = dir.database_folder;
                let Some(to) = link_targets(self).get(index).map(|folder| folder.id) else {
                    return Vec::new();
                };
                let ids: Vec<String> = checked_media(&self.snapshot, &checked)
                    .into_iter()
                    .map(|item| item.value.clone())
                    .collect();
                if ids.is_empty() {
                    return Vec::new();
                }
                // Move from a folder: link into the target, then unlink from the open folder.
                let unlink = from.filter(|_| remove).map(|folder| UiCommand::LinkMedia {
                    folder,
                    ids: ids.clone(),
                    linked: false,
                });
                return std::iter::once(UiCommand::LinkMedia {
                    folder: to,
                    ids,
                    linked: true,
                })
                .chain(unlink)
                .collect();
            }
            Some(Popup::Target { remove }) => {
                let ids: Vec<u64> = dir.multi.take().unwrap_or_default().into_iter().collect();
                if let Some(setlist) = self.snapshot.setlists.get(index)
                    && let Some(from) = self.snapshot.active_setlist_name()
                    && !ids.is_empty()
                {
                    return vec![UiCommand::CopyPresets {
                        ids,
                        from: from.to_owned(),
                        setlist: setlist.id,
                        remove,
                        to: PasteTarget::FirstEmpty,
                    }];
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn item_action(&mut self, action: ItemAction) -> Vec<UiCommand> {
        let popup = self.directory.popup;
        if !matches!(popup, Some(Popup::Preset(_))) {
            return self.directory_item_action(action);
        }
        let Some(Popup::Preset(id)) = self.directory.popup.take() else {
            return Vec::new();
        };
        let snapshot = &self.snapshot;
        match action {
            ItemAction::Load => {
                return vec![if snapshot.bank_preview_active {
                    UiCommand::PreviewPreset { id }
                } else {
                    UiCommand::RequestPreset { id }
                }];
            }
            ItemAction::EditName => {
                let title = snapshot
                    .presets
                    .iter()
                    .find(|preset| preset.id == id)
                    .map(|preset| preset.title.clone())
                    .unwrap_or_default();
                self.open_keyboard(TextTarget::PresetRename(id), title);
            }
            ItemAction::Duplicate => return vec![UiCommand::DuplicatePreset { id }],
            ItemAction::Favorite => {
                return vec![UiCommand::SetFavorite {
                    id,
                    favorite: !snapshot.favorite,
                }];
            }
            ItemAction::Delete => self.overlay = Some(Overlay::Confirm(Confirm::DeletePreset(id))),
            ItemAction::Move => self.open_popup(Popup::MovePreset(id)),
            ItemAction::Upload => {
                return vec![UiCommand::Upload {
                    items: vec![UploadItem::Slot(id)],
                }];
            }
            ItemAction::NewSubfolder | ItemAction::SavingDestination => {}
        }
        Vec::new()
    }

    /// Alphabet strip: scroll the open list to the first row at or past `letter`.
    fn jump(&mut self, letter: u8) {
        let dir = &self.directory;
        let (list, keys, descending): (ListId, Vec<u8>, bool) = match dir.category {
            Category::Favorites => (
                ListId::Favorites,
                directory_favorites(self)
                    .iter()
                    .map(|item| {
                        initial(match dir.media_sort.key {
                            SortKey::Author => &item.author,
                            SortKey::Date => &item.date,
                            _ => &item.name,
                        })
                    })
                    .collect(),
                dir.media_sort.descending,
            ),
            Category::Presets => (
                ListId::DirectoryItems,
                preset_rows(self)
                    .iter()
                    .map(|p| initial(preset_sort_text(self, p)))
                    .collect(),
                dir.preset_sort.descending,
            ),
            category => match category.media() {
                Some(kind) => (
                    ListId::DirectoryItems,
                    {
                        let details = MediaDetails::new(&self.snapshot);
                        media_rows(self, &details, kind)
                            .iter()
                            .map(|(_, entry)| initial(media_sort_text(self, &details, entry)))
                            .collect()
                    },
                    dir.media_descending,
                ),
                None => return,
            },
        };
        let row = keys
            .iter()
            .position(|key| {
                if descending {
                    *key <= letter
                } else {
                    *key >= letter
                }
            })
            .unwrap_or(keys.len().saturating_sub(1));
        self.scroll.insert(list, (row as u32 * ITEM_ROW) as i32);
    }

    /// Date, Device Type or Gain bar: scroll the capture/IR list to the first row at or past
    /// the key of `position` (the table client mapping is not recovered, I).
    fn jump_media(&mut self, position: u8) {
        let Some(kind) = self.directory.category.media() else {
            return;
        };
        let details = MediaDetails::new(&self.snapshot);
        let bar = jump_bar(self, &details, kind);
        let target = match bar {
            JumpBar::Letters => return,
            JumpBar::Date(dates) => dates.date_at(u32::from(position)).unwrap_or(i64::MIN),
            JumpBar::Device | JumpBar::Gain => i64::from(position),
        };
        let keys: Vec<i64> = media_rows(self, &details, kind)
            .iter()
            .map(|(_, entry)| {
                let roles = details.roles(entry);
                let key = match bar {
                    JumpBar::Date(_) => roles.date.map(stock_days),
                    JumpBar::Device => roles.device.map(i64::from),
                    _ => roles.gain.map(i64::from),
                };
                key.unwrap_or(i64::MIN)
            })
            .collect();
        let descending = self.directory.media_sort.descending;
        let row = keys
            .iter()
            .position(|key| {
                if descending {
                    *key <= target
                } else {
                    *key >= target
                }
            })
            .unwrap_or(keys.len().saturating_sub(1));
        self.scroll
            .insert(ListId::DirectoryItems, (row as u32 * ITEM_ROW) as i32);
    }

    /// Search commit: remember the query (newest first, no duplicates).
    pub(crate) fn commit_search(&mut self, query: String) {
        let recent = &mut self.directory.recent_searches;
        if !query.is_empty() {
            recent.retain(|old| *old != query);
            recent.insert(0, query.clone());
            recent.truncate(RECENT_SEARCHES);
        }
        self.library_query = query;
        self.reset_scroll(ListId::PresetResults);
    }
}

/// Sort options of the open view, in stock role order.
pub(crate) fn sort_keys(dir: &Directory) -> &'static [SortKey] {
    if dir.preset_view() {
        &[
            SortKey::Banks,
            SortKey::Name,
            SortKey::Author,
            SortKey::Date,
        ]
    } else if dir.category == Category::Plugins && dir.search.is_none() {
        &[
            SortKey::Name,
            SortKey::Author,
            SortKey::Instrument,
            SortKey::Date,
        ]
    } else if dir.category == Category::Captures && dir.search.is_none() {
        &[
            SortKey::Name,
            SortKey::Author,
            SortKey::Date,
            SortKey::DeviceType,
            SortKey::Gain,
        ]
    } else {
        &[SortKey::Name, SortKey::Author, SortKey::Date]
    }
}

pub(super) fn preset_sort_text<'a>(ui: &'a UiState, preset: &'a PresetItem) -> &'a str {
    let detail = ui
        .snapshot
        .directory
        .slots
        .iter()
        .find(|slot| slot.id == preset.id);
    match ui.directory.preset_sort.key {
        SortKey::Author => detail.map_or("", |slot| &slot.author),
        SortKey::Date => detail.map_or("", |slot| &slot.date),
        _ => &preset.title,
    }
}

pub(super) fn media_sort_text<'a>(
    ui: &UiState,
    details: &MediaDetails<'a>,
    entry: &'a MediaEntry,
) -> &'a str {
    let detail = details.get(&entry.value);
    match ui.directory.media_sort.key {
        SortKey::Author => detail.map_or("", |item| &item.author),
        SortKey::Date => detail.map_or("", |item| &item.date),
        _ => &entry.name,
    }
}

fn item_roles(item: &DirectoryItem) -> Roles<'_> {
    Roles {
        name: &item.name,
        author: &item.author,
        date: stock_date(&item.date),
        instrument: item.instrument,
        device: None,
        gain: None,
        id: &item.value,
        pinned: false,
    }
}

/// Presets of the open plugin's selected folder (and its subfolders), sorted.
pub(crate) fn plugin_presets(ui: &UiState) -> Vec<(u16, &DirectoryItem)> {
    let dir = &ui.directory;
    let Some(plugin) = dir
        .plugin_folder
        .and_then(|index| ui.snapshot.directory.plugins.get(usize::from(index)))
    else {
        return Vec::new();
    };
    let child = dir
        .plugin_child
        .and_then(|index| plugin.children.get(usize::from(index)));
    let mut rows: Vec<(u16, &DirectoryItem)> = plugin
        .presets
        .iter()
        .enumerate()
        .filter(|(_, item)| match child {
            Some(child) => item
                .folder
                .strip_prefix(child.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/')),
            None => item.folder.is_empty(),
        })
        .map(|(index, item)| (index as u16, item))
        .collect();
    rows.sort_by(|a, b| dir.media_sort.order(&item_roles(a.1), &item_roles(b.1)));
    rows
}

pub(super) fn directory_favorites(ui: &UiState) -> Vec<DirectoryItem> {
    let dir = &ui.directory;
    let source = if dir.recent {
        &ui.snapshot.directory.recents
    } else {
        &ui.snapshot.directory.favorites
    };
    let mut rows: Vec<_> = source
        .iter()
        .filter(|item| item.kind == dir.favorites)
        .cloned()
        .collect();
    if rows.is_empty() && !dir.recent && dir.favorites == ItemKind::Preset {
        rows = ui
            .snapshot
            .favorite_presets
            .iter()
            .map(|preset| DirectoryItem {
                kind: ItemKind::Preset,
                name: preset.title.clone(),
                folder: String::new(),
                value: String::new(),
                author: String::new(),
                date: String::new(),
                instrument: PreferredInstrument::NotSet,
                legacy_index: Some(preset.id),
            })
            .collect();
    }
    // Stock recent lists retain newest-first unless the user chooses a sort.
    if !dir.recent || dir.media_sort.key != SortKey::Banks {
        rows.sort_by(|a, b| dir.media_sort.order(&item_roles(a), &item_roles(b)));
    }
    rows
}

impl UiState {
    fn directory_item_action(&mut self, action: ItemAction) -> Vec<UiCommand> {
        let popup = self.directory.popup.take();
        match popup {
            Some(Popup::Setlist(id)) => {
                let Some(setlist) = self
                    .snapshot
                    .setlists
                    .iter()
                    .find(|setlist| setlist.id == id)
                else {
                    return Vec::new();
                };
                if setlist.title == "My Presets" || setlist.title == "Factory Presets" {
                    return Vec::new();
                }
                let name = setlist.title.clone();
                match action {
                    ItemAction::EditName => self.open_keyboard(
                        TextTarget::DirectoryName(DirectoryNameEdit::Setlist(name.clone())),
                        name,
                    ),
                    ItemAction::Delete => {
                        self.open_popup(Popup::Delete(DirectoryDelete::Setlist(id)))
                    }
                    _ => {}
                }
            }
            Some(Popup::DatabaseFolder(id)) => {
                let Some(folder) = self
                    .snapshot
                    .directory
                    .folders
                    .iter()
                    .find(|folder| folder.id == id)
                else {
                    return Vec::new();
                };
                if folder.read_only {
                    return Vec::new();
                }
                let (name, kind, root) = (folder.name.clone(), folder.kind, folder.root);
                match action {
                    ItemAction::EditName if !root => self.open_keyboard(
                        TextTarget::DirectoryName(DirectoryNameEdit::Folder(id)),
                        name,
                    ),
                    ItemAction::Delete if !root => {
                        self.open_popup(Popup::Delete(DirectoryDelete::Folder(id)))
                    }
                    ItemAction::NewSubfolder => self.open_keyboard(
                        TextTarget::DirectoryName(DirectoryNameEdit::NewFolder {
                            kind,
                            parent: id,
                        }),
                        String::new(),
                    ),
                    ItemAction::Duplicate if can_duplicate(&self.snapshot.directory, id) => {
                        return vec![UiCommand::DuplicateMediaFolder { folder: id }];
                    }
                    ItemAction::SavingDestination
                        if !self.snapshot.directory.saving_destinations.contains(&id) =>
                    {
                        return vec![UiCommand::SetSavingDestination { folder: id }];
                    }
                    _ => {}
                }
            }
            Some(Popup::Media(index)) => {
                let Some(entry) = self.snapshot.media.get(index as usize) else {
                    return Vec::new();
                };
                let (kind, id, name) =
                    (entry.folder.kind(), entry.value.clone(), entry.name.clone());
                let writable = self
                    .snapshot
                    .directory
                    .media
                    .iter()
                    .any(|item| item.value == id && !item.read_only);
                match action {
                    // `onEditClicked` `0x78e49c`: a capture opens its save page in edit mode.
                    ItemAction::EditName if writable && kind == MediaKind::Capture => {
                        let meta = (self.snapshot.directory.media.iter())
                            .find(|item| item.value == id)
                            .and_then(|item| item.capture.as_ref());
                        let device = meta.and_then(|meta| meta.device);
                        let instrument = meta.and_then(|meta| meta.instrument);
                        self.directory.popup = None;
                        self.capture.edit = Some(crate::screens::CaptureEdit {
                            id,
                            name,
                            device: device.and_then(CaptureDevice::from_code),
                            instrument: instrument.and_then(CaptureInstrument::from_code),
                        });
                    }
                    ItemAction::EditName if writable => self.open_keyboard(
                        TextTarget::DirectoryName(DirectoryNameEdit::Media { kind, id }),
                        name,
                    ),
                    ItemAction::Delete if writable => {
                        self.open_popup(Popup::Delete(DirectoryDelete::Media(index)))
                    }
                    ItemAction::Upload if writable && kind == MediaKind::Capture => {
                        self.directory.popup = None;
                        return vec![UiCommand::Upload {
                            items: vec![UploadItem::Capture(id)],
                        }];
                    }
                    ItemAction::Favorite => {
                        let item_kind = if kind == MediaKind::Capture {
                            ItemKind::Capture
                        } else {
                            ItemKind::Ir
                        };
                        let existing = self
                            .snapshot
                            .directory
                            .favorites
                            .iter()
                            .find(|item| item.kind == item_kind && item.value == id);
                        let folder =
                            existing.map(|item| item.folder.clone()).unwrap_or_else(|| {
                                if kind == MediaKind::Capture {
                                    "local_nc_root"
                                } else {
                                    "local_ir_root"
                                }
                                .to_owned()
                            });
                        return vec![UiCommand::Directory(DirectoryRequest::FavoriteMedia {
                            kind,
                            folder,
                            id,
                            favorite: existing.is_none(),
                        })];
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn confirm_directory_delete(&mut self, remove_products: bool) -> Vec<UiCommand> {
        let Some(Popup::Delete(target)) = self.directory.popup.take() else {
            return Vec::new();
        };
        let request = match target {
            DirectoryDelete::Setlist(id) => {
                let Some(setlist) = self
                    .snapshot
                    .setlists
                    .iter()
                    .find(|setlist| setlist.id == id)
                else {
                    return Vec::new();
                };
                DirectoryRequest::DeleteSetlist {
                    name: setlist.title.clone(),
                }
            }
            DirectoryDelete::Folder(id) => DirectoryRequest::DeleteFolder {
                id,
                remove_products,
            },
            DirectoryDelete::Media(index) => {
                let Some(entry) = self.snapshot.media.get(index as usize) else {
                    return Vec::new();
                };
                DirectoryRequest::DeleteMedia {
                    kind: entry.folder.kind(),
                    id: entry.value.clone(),
                }
            }
        };
        vec![UiCommand::Directory(request)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles<'a>(name: &'a str, author: &'a str, date: &'a str, id: &'a str) -> Roles<'a> {
        Roles {
            name,
            author,
            date: stock_date(date),
            instrument: PreferredInstrument::NotSet,
            device: None,
            gain: None,
            id,
            pinned: false,
        }
    }

    const fn sort(key: SortKey, descending: bool) -> Sort {
        Sort { key, descending }
    }

    /// Review r11 N13: only Name (role 0x21) uses the key; other roles tie on equal names.
    #[test]
    fn only_the_name_sort_breaks_equal_names_by_key() {
        let (a, b) = (roles("Same", "X", "", "b"), roles("Same", "X", "", "a"));
        assert_eq!(sort(SortKey::Name, false).order(&a, &b), Ordering::Greater);
        assert_eq!(sort(SortKey::Author, false).order(&a, &b), Ordering::Equal);
    }

    /// Review r11 N14: pinned captures (`+0x49` set, `+0x4a` clear) lead in both directions.
    #[test]
    fn pinned_captures_lead_in_both_directions() {
        let plain = roles("A", "", "", "1");
        let pinned = Roles {
            pinned: true,
            ..roles("Z", "", "", "2")
        };
        for descending in [false, true] {
            let sort = sort(SortKey::Name, descending);
            assert_eq!(sort.order(&pinned, &plain), Ordering::Less);
        }
    }

    /// Review r11 N15: dates compare as parsed `QDateTime`s; an invalid or pre-1970 date
    /// sorts before every valid one and equal dates fall back to the name.
    #[test]
    fn dates_sort_as_parsed_stock_dates() {
        let date = sort(SortKey::Date, false);
        let newer = roles("A", "", "2024-02-29T10:00:00Z", "");
        let older = roles("B", "", "2023-12-31T23:59:59Z", "");
        assert_eq!(date.order(&older, &newer), Ordering::Less);
        for invalid in [
            "2023-02-29T10:00:00Z",
            "2024-1-01T10:00:00Z",
            "1970-01-01T00:00:00Z",
        ] {
            assert_eq!(
                date.order(&roles("A", "", invalid, ""), &older),
                Ordering::Less
            );
        }
        let (a, b) = (roles("b", "", "junk", ""), roles("a", "", "", ""));
        assert_eq!(date.order(&a, &b), Ordering::Greater);
    }

    /// Health r34 #9 (d): the per-layout id map reads each row's details by key, not by
    /// position: the Date sort, the folder filter and a pinned capture follow the keyed row.
    #[test]
    fn media_rows_read_details_by_key() {
        use crate::{MediaFolder, UiSnapshot};
        let entry = |value: &str| MediaEntry {
            folder: MediaFolder::MyCaptures,
            name: value.to_uppercase(),
            value: value.into(),
        };
        let detail = |value: &str, date: &str, folders: Vec<i64>| DirectoryMedia {
            value: value.into(),
            author: String::new(),
            date: date.into(),
            read_only: false,
            folders,
            capture: None,
        };
        let mut snapshot = UiSnapshot {
            media: vec![entry("a"), entry("b"), entry("c"), entry("d")].into(),
            ..UiSnapshot::default()
        };
        std::sync::Arc::make_mut(&mut snapshot.directory).media = vec![
            detail("d", "2022-01-01T00:00:00Z", vec![1]),
            detail("c", "2020-01-01T00:00:00Z", vec![2]),
            detail("b", "2021-01-01T00:00:00Z", vec![1]),
            detail("a", "2023-01-01T00:00:00Z", vec![1]),
        ];
        std::sync::Arc::make_mut(&mut snapshot.directory).pinned_captures = vec!["d".into()];
        let mut ui = UiState::default();
        ui.set_snapshot(snapshot).unwrap();
        ui.directory.database_folder = Some(1);
        ui.directory.media_sort.key = SortKey::Date;
        let details = MediaDetails::new(&ui.snapshot);
        let rows = media_rows(&ui, &details, MediaKind::Capture);
        let keys: Vec<&str> = rows.iter().map(|(_, entry)| entry.value.as_str()).collect();
        assert_eq!(keys, ["d", "b", "a"]);
    }

    /// Health r34 #9 (d): host timing of one capture list layout with 2000 dated captures
    /// in a folder view, by Name and by Date. Timing only:
    /// `cargo test -p tetrad-ui --release directory_layout_timing -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing only"]
    fn directory_layout_timing() {
        use crate::{MediaFolder, Screen, UiSnapshot};
        const ITEMS: usize = 2000;
        const ROUNDS: u32 = 20;
        let mut snapshot = UiSnapshot {
            media: (0..ITEMS)
                .map(|index| MediaEntry {
                    folder: MediaFolder::MyCaptures,
                    name: format!("Capture {}", (index * 7919) % ITEMS),
                    value: format!("key-{index}"),
                })
                .collect::<Vec<_>>()
                .into(),
            ..UiSnapshot::default()
        };
        std::sync::Arc::make_mut(&mut snapshot.directory).media = (0..ITEMS)
            .map(|index| DirectoryMedia {
                value: format!("key-{index}"),
                author: format!("Author {}", index % 17),
                date: format!("2023-{:02}-{:02}T10:00:00Z", index % 12 + 1, index % 28 + 1),
                read_only: false,
                folders: vec![1],
                capture: None,
            })
            .collect();
        let mut ui = UiState::default();
        ui.set_snapshot(snapshot).unwrap();
        ui.show(Screen::Presets);
        ui.directory.category = Category::Captures;
        ui.directory.database_folder = Some(1);
        for key in [SortKey::Name, SortKey::Date] {
            ui.directory.media_sort.key = key;
            let start = std::time::Instant::now();
            for _ in 0..ROUNDS {
                std::hint::black_box(ui.layout());
            }
            eprintln!("layout {key:?}: {:?} per frame", start.elapsed() / ROUNDS);
        }
    }
}

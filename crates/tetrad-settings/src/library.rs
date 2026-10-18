//! Library UI state in ZenjackConfig.ini: `[TableSettings]` (sort and
//! filter state, last folders, download folders; main load `0x00653d18`),
//! `[LastPreset]`, `[BackupDownloadTimes]` and the read-only legacy
//! `[SetlistCreationTime]`.

use std::collections::BTreeMap;

use crate::typed::{KeyReader, KeyWriter, LoadIssueKind, SettingError, stock_enum};
use crate::{QtDateTime, Variant};

stock_enum!(
    /// Table column role (`0x007c8b78`).
    TableRole {
        Name = 0x21,
        /// "Index" in some tables.
        Banks = 0x22,
        Author = 0x23,
        PreferredInstrument = 0x24,
        DateAdded = 0x25,
        DeviceType = 0x26,
        Gain = 0x27,
        ModelData = 0x28,
    }
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortOrder {
    pub role: TableRole,
    pub ascending: bool,
}

impl SortOrder {
    const fn by(role: TableRole) -> Self {
        Self {
            role,
            ascending: true,
        }
    }
}

/// `captureFilter`: the filter menu `QVariantMap` (keys `0x007f4c1c`).
/// `None` fields are absent from the map. Default: empty map.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CaptureFilter {
    /// `DEVICE_TYPES` (`QList<int>`).
    pub device_types: Option<Vec<i32>>,
    /// `OWNED_BY_ME` ("Made by Me").
    pub owned_by_me: Option<bool>,
    /// `VERSIONS`; value type not recovered, kept as decoded.
    pub versions: Option<Variant>,
}

impl CaptureFilter {
    fn from_variant(v: &Variant) -> Option<Self> {
        let Variant::Map(entries) = v else {
            return None;
        };
        let mut filter = Self::default();
        for (key, value) in entries {
            match key.as_str() {
                "DEVICE_TYPES" => filter.device_types = value.to_int_list(),
                "OWNED_BY_ME" => filter.owned_by_me = Some(value.to_bool()),
                "VERSIONS" => filter.versions = Some(value.clone()),
                _ => {}
            }
        }
        Some(filter)
    }

    fn to_variant(&self) -> Variant {
        let mut entries = Vec::new();
        if let Some(types) = &self.device_types {
            entries.push(("DEVICE_TYPES".to_owned(), Variant::IntList(types.clone())));
        }
        if let Some(owned) = self.owned_by_me {
            entries.push(("OWNED_BY_ME".to_owned(), Variant::Bool(owned)));
        }
        if let Some(versions) = &self.versions {
            entries.push(("VERSIONS".to_owned(), versions.clone()));
        }
        Variant::map(entries)
    }
}

/// Library table state.
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryTables {
    pub presets: SortOrder,
    pub preset_reference: TableRole,
    /// Only `Name` or `PreferredInstrument` ("Invalid sort role").
    pub plugins: SortOrder,
    /// Only `Author` or `PreferredInstrument` ("Invalid reference role").
    pub plugin_reference: TableRole,
    pub captures: SortOrder,
    pub capture_reference: TableRole,
    pub capture_filter: CaptureFilter,
    /// The IR reference role is fixed at `Name` and not stored.
    pub irs: SortOrder,
    /// Favourites reference roles are fixed at `DateAdded`.
    pub favorite_presets: SortOrder,
    pub favorite_captures: SortOrder,
    pub favorite_irs: SortOrder,
    /// `folderSelectorMode`; meaning not recovered. Default 1.
    pub folder_selector_mode: i32,
    /// `lastTableFolder/<n>`: folder key by table type (1 = IR, 2 = NC,
    /// 0..=4 valid; `0x00651ac8`, save `0x0065560c`).
    pub last_folders: BTreeMap<u8, String>,
    /// `downloadDestinationFolderNC` / `IR`; `None` uses the database
    /// default folder of the type (`0x0064ba04`, save `0x0064ee28`).
    pub download_destination_nc: Option<String>,
    pub download_destination_ir: Option<String>,
}

impl Default for LibraryTables {
    fn default() -> Self {
        Self {
            presets: SortOrder::by(TableRole::Banks),
            preset_reference: TableRole::Author,
            plugins: SortOrder::by(TableRole::Name),
            plugin_reference: TableRole::PreferredInstrument,
            captures: SortOrder::by(TableRole::Name),
            capture_reference: TableRole::Author,
            capture_filter: CaptureFilter::default(),
            irs: SortOrder::by(TableRole::Name),
            favorite_presets: SortOrder::by(TableRole::Banks),
            favorite_captures: SortOrder::by(TableRole::Name),
            favorite_irs: SortOrder::by(TableRole::Name),
            folder_selector_mode: 1,
            last_folders: BTreeMap::new(),
            download_destination_nc: None,
            download_destination_ir: None,
        }
    }
}

const SORTS: [&str; 7] = [
    "preset",
    "plugin",
    "capture",
    "ir",
    "favoritesPreset",
    "favoritesCapture",
    "favoritesIR",
];

impl LibraryTables {
    fn sorts_mut(&mut self) -> [&mut SortOrder; 7] {
        [
            &mut self.presets,
            &mut self.plugins,
            &mut self.captures,
            &mut self.irs,
            &mut self.favorite_presets,
            &mut self.favorite_captures,
            &mut self.favorite_irs,
        ]
    }

    fn sorts(&self) -> [&SortOrder; 7] {
        [
            &self.presets,
            &self.plugins,
            &self.captures,
            &self.irs,
            &self.favorite_presets,
            &self.favorite_captures,
            &self.favorite_irs,
        ]
    }

    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let mut t = Self::default();
        let role = |r: &mut KeyReader, key: &str, default: TableRole, allowed: &[TableRole]| {
            let key = format!("TableSettings/{key}");
            let raw = r.int(&key, default.to_stock());
            let parsed = TableRole::from_stock(raw).and_then(|role| {
                if allowed.is_empty() || allowed.contains(&role) {
                    Ok(role)
                } else {
                    Err(SettingError::UnknownValue {
                        name: "TableRole",
                        value: raw.to_string(),
                    })
                }
            });
            r.check(&key, parsed, default)
        };
        for (prefix, sort) in SORTS.into_iter().zip(t.sorts_mut()) {
            let allowed: &[TableRole] = if prefix == "plugin" {
                &[TableRole::Name, TableRole::PreferredInstrument]
            } else {
                &[]
            };
            sort.role = role(r, &format!("{prefix}SortRole"), sort.role, allowed);
            sort.ascending = r.bool(&format!("TableSettings/{prefix}SortAscending"), true);
        }
        t.preset_reference = role(r, "presetReferenceRole", t.preset_reference, &[]);
        t.plugin_reference = role(
            r,
            "pluginReferenceRole",
            t.plugin_reference,
            &[TableRole::Author, TableRole::PreferredInstrument],
        );
        t.capture_reference = role(r, "captureReferenceRole", t.capture_reference, &[]);
        t.folder_selector_mode = r.int("TableSettings/folderSelectorMode", 1);
        if let Some(v) = r.variant("TableSettings/captureFilter") {
            match CaptureFilter::from_variant(&v) {
                Some(filter) => t.capture_filter = filter,
                None if v == Variant::Invalid => {}
                None => r.issue("TableSettings/captureFilter", LoadIssueKind::NotConvertible),
            }
        }
        for key in r.file.child_keys("TableSettings/lastTableFolder") {
            let full = format!("TableSettings/lastTableFolder/{key}");
            match key.parse::<u8>() {
                Ok(n) if n <= 4 => {
                    let folder = r.string(&full, "");
                    t.last_folders.insert(n, folder);
                }
                // "Invalid child in lastTableFolder".
                _ => r.issue(&full, LoadIssueKind::NotConvertible),
            }
        }
        let destination = |r: &mut KeyReader, key: &str| {
            let key = format!("TableSettings/{key}");
            r.file.contains(&key).then(|| r.string(&key, ""))
        };
        t.download_destination_nc = destination(r, "downloadDestinationFolderNC");
        t.download_destination_ir = destination(r, "downloadDestinationFolderIR");
        t
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        let role = |r: &TableRole| Variant::UInt(r.to_stock() as u32);
        for ((prefix, new), old) in SORTS.into_iter().zip(self.sorts()).zip(old.sorts()) {
            w.put(
                &format!("TableSettings/{prefix}SortRole"),
                &new.role,
                &old.role,
                role,
            );
            w.bool(
                &format!("TableSettings/{prefix}SortAscending"),
                new.ascending,
                old.ascending,
            );
        }
        w.put(
            "TableSettings/presetReferenceRole",
            &self.preset_reference,
            &old.preset_reference,
            role,
        );
        w.put(
            "TableSettings/pluginReferenceRole",
            &self.plugin_reference,
            &old.plugin_reference,
            role,
        );
        w.put(
            "TableSettings/captureReferenceRole",
            &self.capture_reference,
            &old.capture_reference,
            role,
        );
        w.int(
            "TableSettings/folderSelectorMode",
            self.folder_selector_mode,
            old.folder_selector_mode,
        );
        w.put(
            "TableSettings/captureFilter",
            &self.capture_filter,
            &old.capture_filter,
            CaptureFilter::to_variant,
        );
        for (n, folder) in &self.last_folders {
            let before = old.last_folders.get(n).map_or("", String::as_str);
            if !old.last_folders.contains_key(n) || before != folder {
                w.file.set(
                    &format!("TableSettings/lastTableFolder/{n}"),
                    &Variant::String(folder.clone()),
                );
            }
        }
        for n in old
            .last_folders
            .keys()
            .filter(|n| !self.last_folders.contains_key(n))
        {
            w.file.remove(&format!("TableSettings/lastTableFolder/{n}"));
        }
        for (key, new, old) in [
            (
                "downloadDestinationFolderNC",
                &self.download_destination_nc,
                &old.download_destination_nc,
            ),
            (
                "downloadDestinationFolderIR",
                &self.download_destination_ir,
                &old.download_destination_ir,
            ),
        ] {
            if new != old {
                let key = format!("TableSettings/{key}");
                match new {
                    Some(folder) => w.file.set(&key, &Variant::String(folder.clone())),
                    None => w.file.remove(&key),
                }
            }
        }
    }
}

/// `[LastPreset]`: where the last loaded preset came from. Load
/// `0x0064f024` (boot), save `0x0064cea0` on change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LastPreset {
    /// `setlist` folder key; `None` (key absent) means the default setlist.
    pub setlist: Option<String>,
    /// `presetIndex`: slot in the setlist; bank = index / 8.
    pub preset_index: i32,
    /// `key`: id of the loaded preset, to check the slot still holds it.
    pub key: String,
}

impl LastPreset {
    pub fn bank(&self) -> i32 {
        self.preset_index >> 3
    }

    pub(crate) fn load(r: &mut KeyReader) -> Self {
        Self {
            setlist: r
                .file
                .contains("LastPreset/setlist")
                .then(|| r.string("LastPreset/setlist", "")),
            preset_index: r.int("LastPreset/presetIndex", 0),
            key: r.string("LastPreset/key", ""),
        }
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        if self.setlist != old.setlist {
            match &self.setlist {
                Some(s) => w
                    .file
                    .set("LastPreset/setlist", &Variant::String(s.clone())),
                None => w.file.remove("LastPreset/setlist"),
            }
        }
        w.int(
            "LastPreset/presetIndex",
            self.preset_index,
            old.preset_index,
        );
        w.string("LastPreset/key", &self.key, &old.key);
    }
}

/// `[BackupDownloadTimes]`: cloud backup id to download time (`QDateTime`).
/// Read all `0x00303a38`, write all `0x0030367c`, remove `0x002ff7f0`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackupDownloadTimes(pub BTreeMap<String, QtDateTime>);

impl BackupDownloadTimes {
    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let mut times = BTreeMap::new();
        for id in r.file.child_keys("BackupDownloadTimes") {
            let key = format!("BackupDownloadTimes/{id}");
            match r.variant(&key) {
                Some(Variant::DateTime(t)) => {
                    times.insert(id, t);
                }
                Some(_) => r.issue(&key, LoadIssueKind::NotConvertible),
                None => {}
            }
        }
        Self(times)
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        for (id, time) in self.0.iter().filter(|(id, t)| old.0.get(*id) != Some(*t)) {
            w.file.set(
                &format!("BackupDownloadTimes/{id}"),
                &Variant::DateTime(*time),
            );
        }
        for id in old.0.keys().filter(|id| !self.0.contains_key(*id)) {
            w.file.remove(&format!("BackupDownloadTimes/{id}"));
        }
    }
}

/// `[SetlistCreationTime]`: setlist id to creation time (`qulonglong`). Read
/// by the setlist loader `0x007c77ec`; 4.1.0 has no writer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetlistCreationTimes(pub BTreeMap<String, u64>);

impl SetlistCreationTimes {
    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let mut times = BTreeMap::new();
        for id in r.file.child_keys("SetlistCreationTime") {
            let key = format!("SetlistCreationTime/{id}");
            let text = r.string(&key, "");
            match text.trim().parse() {
                Ok(t) => {
                    times.insert(id, t);
                }
                Err(_) => r.issue(&key, LoadIssueKind::NotConvertible),
            }
        }
        Self(times)
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        for (id, time) in self.0.iter().filter(|(id, t)| old.0.get(*id) != Some(*t)) {
            w.file.set(
                &format!("SetlistCreationTime/{id}"),
                &Variant::ULongLong(*time),
            );
        }
        for id in old.0.keys().filter(|id| !self.0.contains_key(*id)) {
            w.file.remove(&format!("SetlistCreationTime/{id}"));
        }
    }
}

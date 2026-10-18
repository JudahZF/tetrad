//! PermanentSettings.ini: survives both settings reset and factory reset.
//! Singleton `0x00208b24`, load `0x0020640c`.

use std::collections::BTreeMap;

use crate::Variant;
use crate::typed::{KeyReader, KeyWriter, LoadIssueKind, SettingError};

/// Dotted numeric version order ("3.10.0" > "3.9.0"). Stock compares
/// version strings; the exact comparison is inferred.
pub(crate) fn version_less(a: &str, b: &str) -> bool {
    let parts = |v: &str| {
        v.split('.')
            .map(|p| p.trim().parse::<u32>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parts(a) < parts(b)
}

/// `[General]` `atmaPowerOnModeSettingsUpdateCount` (diagnostic count of
/// applied power-on-mode changes, save `0x002051b0`) and `[PopupFlags]`
/// `modelPresetInfoViewed` (one-way flag, save `0x002052cc`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PermanentFlags {
    pub atma_power_on_update_count: i32,
    pub model_preset_info_viewed: bool,
}

impl PermanentFlags {
    pub(crate) fn load(r: &mut KeyReader) -> Self {
        Self {
            atma_power_on_update_count: r.int("atmaPowerOnModeSettingsUpdateCount", 0),
            model_preset_info_viewed: r.bool("PopupFlags/modelPresetInfoViewed", false),
        }
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        w.int(
            "atmaPowerOnModeSettingsUpdateCount",
            self.atma_power_on_update_count,
            old.atma_power_on_update_count,
        );
        w.bool(
            "PopupFlags/modelPresetInfoViewed",
            self.model_preset_info_viewed,
            old.model_preset_info_viewed,
        );
    }
}

/// `[General]` `cloudDomain`: stored as the `CloudDomain` enum key without
/// its `en` prefix (load `0x0040c2e0`, save `0x0040cb50`). The stock
/// read default "default" matches no key and selects `Default`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CloudDomain {
    #[default]
    Default,
    Alternative,
}

impl CloudDomain {
    pub const fn stock_name(self) -> &'static str {
        match self {
            Self::Default => "DefaultCloudDomain",
            Self::Alternative => "AlternativeCloudDomain",
        }
    }

    pub(crate) fn load(r: &mut KeyReader) -> Self {
        match r.string("cloudDomain", "default").as_str() {
            "AlternativeCloudDomain" => Self::Alternative,
            "DefaultCloudDomain" | "default" => Self::Default,
            other => {
                let e = SettingError::UnknownValue {
                    name: "cloudDomain",
                    value: other.to_owned(),
                };
                r.issue("cloudDomain", LoadIssueKind::Invalid(e));
                Self::Default
            }
        }
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        w.string("cloudDomain", self.stock_name(), old.stock_name());
    }
}

/// `[NewModels]`: firmware version to model ids that still show a "NEW"
/// badge. Stored as decimal string lists. Load `0x00205730`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewModels(pub BTreeMap<String, Vec<i32>>);

impl NewModels {
    /// Built-in seed (`0x0020640c`), in stock order.
    pub const SEED: [(&'static str, &'static [i32]); 5] = [
        (
            "3.0.0",
            &[
                16008, 16009, 16011, 16012, 16013, 6015, 4005, 18010, 18013, 16007, 24004, 5018,
                5019, 5020, 5021,
            ],
        ),
        ("3.1.0", &[18014, 7036, 18015, 9019, 1166, 1167]),
        (
            "3.3.0",
            &[
                1176, 7039, 7040, 42, 9021, 18016, 26002, 30001, 26003, 26004, 8026, 8027, 12063,
                32063, 12064, 32064, 12065, 32065, 12066, 32066, 12067, 32067, 12068, 32068, 12069,
                32069, 12070, 32070, 12071, 32071, 12072, 32072, 12073, 32073, 12074, 32074, 12075,
                32075, 12076, 32076, 12077, 32077, 21008, 33008, 21009, 33009,
            ],
        ),
        ("4.0.0", &[16016, 8028, 8029, 8030]),
        (
            "4.1.0",
            &[5025, 6026, 6031, 4008, 8039, 26005, 26006, 18012],
        ),
    ];
    /// Badges expire after this many boots (`boot_count.txt`).
    pub const EXPIRY_BOOTS: u32 = 5;

    pub fn is_new(&self, model_id: i32) -> bool {
        model_id != 0 && self.0.values().any(|ids| ids.contains(&model_id))
    }

    /// `0x00205c00`: drops versions below 3.0.0 and adds seed versions the
    /// file lacks. Existing lists are kept.
    pub fn seed(&mut self) {
        self.0.retain(|version, _| !version_less(version, "3.0.0"));
        for (version, ids) in Self::SEED {
            self.0
                .entry(version.to_owned())
                .or_insert_with(|| ids.to_vec());
        }
    }

    /// `0x00208b9c`: the user saw the model.
    pub fn mark_seen(&mut self, model_id: i32) {
        for ids in self.0.values_mut() {
            ids.retain(|&id| id != model_id);
        }
    }

    /// `0x002053ec`: every list becomes `["0"]`; keys stay so they are not
    /// seeded again.
    pub fn expire(&mut self) {
        for ids in self.0.values_mut() {
            *ids = vec![0];
        }
    }

    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let mut lists = BTreeMap::new();
        for version in r.file.child_keys("NewModels") {
            // Stock reads `toStringList()` and converts each item, so a
            // single "0" is the list [0].
            let key = format!("NewModels/{version}");
            let items = r
                .variant(&key)
                .map(|v| v.to_string_list())
                .unwrap_or_default();
            let ids: Option<Vec<i32>> = items.iter().map(|s| s.trim().parse().ok()).collect();
            match ids {
                Some(ids) => {
                    lists.insert(version, ids);
                }
                None => r.issue(&key, LoadIssueKind::NotConvertible),
            }
        }
        Self(lists)
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        for (version, ids) in &self.0 {
            w.put(
                &format!("NewModels/{version}"),
                &Some(ids),
                &old.0.get(version),
                |ids| {
                    Variant::StringList(
                        ids.iter()
                            .flat_map(|l| l.iter())
                            .map(i32::to_string)
                            .collect(),
                    )
                },
            );
        }
        for version in old.0.keys().filter(|v| !self.0.contains_key(*v)) {
            w.file.remove(&format!("NewModels/{version}"));
        }
    }
}

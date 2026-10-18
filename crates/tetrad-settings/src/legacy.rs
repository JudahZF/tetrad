//! Pre-database per-model default groups in ZenjackConfig.ini.
//!
//! When the database version is not 300, stock (`0x006a4140`) scans every
//! group not in [`EXCLUDED_GROUPS`]. A group with a `*_exp_trim_min` key is
//! parsed (`0x006a2edc`), imported into the model-preset store and
//! removed. Other groups are logged and kept.

use std::collections::BTreeMap;

use crate::{SettingsFile, Variant};

pub const EXCLUDED_GROUPS: [&str; 16] = [
    "LastPreset",
    "TunerInput",
    "Tuner",
    "TunerMute",
    "TempoSettings",
    "ConnectorsSettings",
    "NeuralCaptureDialogSettings",
    "Mode",
    "MidiSettings",
    "General",
    "ExpressionsSettings",
    "SetlistCreationTime",
    "GlobalEq",
    "MetronomeSettings",
    "Atma",
    "TableSettings",
];

/// One parameter of a legacy group. Keys are `<id>`, `<id>_exp`,
/// `<id>_exp_trim_min`, `<id>_exp_trim_max` and `<id>_exp_bypass_*`.
#[derive(Clone, Debug, PartialEq)]
pub struct LegacyParam {
    pub value: Option<Variant>,
    pub expression: Option<i32>,
    pub trim_min: Option<f32>,
    pub trim_max: Option<f32>,
    pub bypass_type: i32,
    pub bypass_inverted: bool,
    pub bypass_latch_emulation: bool,
    pub bypass_delay_ms: i32,
}

impl Default for LegacyParam {
    fn default() -> Self {
        Self {
            value: None,
            expression: None,
            trim_min: None,
            trim_max: None,
            bypass_type: 2,
            bypass_inverted: false,
            bypass_latch_emulation: false,
            bypass_delay_ms: 600,
        }
    }
}

/// A legacy group: the group name and its params by id.
#[derive(Clone, Debug, PartialEq)]
pub struct LegacyModelDefaults {
    pub group: String,
    pub params: BTreeMap<i32, LegacyParam>,
}

/// Parses the legacy groups of a ZenjackConfig file without changing it.
pub fn find_legacy_model_defaults(file: &SettingsFile) -> Vec<LegacyModelDefaults> {
    let mut found = Vec::new();
    for group in file.child_groups("") {
        if EXCLUDED_GROUPS.contains(&group.as_str()) {
            continue;
        }
        let keys = file.child_keys(&group);
        if !keys.iter().any(|k| k.ends_with("_exp_trim_min")) {
            continue;
        }
        let mut params: BTreeMap<i32, LegacyParam> = BTreeMap::new();
        for key in keys {
            let (id, field) = key.split_once('_').unwrap_or((&key, ""));
            let Ok(id) = id.parse::<i32>() else { continue };
            let Some(Ok(value)) = file.value(&format!("{group}/{key}")) else {
                continue;
            };
            let param = params.entry(id).or_default();
            match field {
                "" => param.value = Some(value),
                "exp" => param.expression = value.to_int(),
                "exp_trim_min" => param.trim_min = value.to_f32(),
                "exp_trim_max" => param.trim_max = value.to_f32(),
                "exp_bypass_type" => param.bypass_type = value.to_int().unwrap_or(2),
                "exp_bypass_inverted" => param.bypass_inverted = value.to_bool(),
                "exp_bypass_latch_emulation" => param.bypass_latch_emulation = value.to_bool(),
                "exp_bypass_delay_ms" => param.bypass_delay_ms = value.to_int().unwrap_or(600),
                _ => {}
            }
        }
        found.push(LegacyModelDefaults { group, params });
    }
    found
}

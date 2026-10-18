//! Import of legacy per-model defaults from `ZenjackConfig.ini` into
//! `zenui.db` (stock `0x006a4140` → `0x006a2edc`).
//!
//! Contract: at boot, when the user database schema major is not 300, the
//! caller reads the groups with
//! `tetrad_settings::device::DeviceSettings::legacy_model_defaults` and passes them
//! here. After a successful import, it calls `remove_legacy_model_defaults`.
//! A writable [`StockDb`] is always at 410.1, so the version condition holds
//! whenever this can run. Stock removes each group before its database
//! writes and ignores write errors; Tetrad writes all groups in one
//! transaction, and the caller saves the settings file after it.

use tetrad_settings::legacy::{LegacyModelDefaults, LegacyParam};
use tetrad_storage::stock::{ExpSwitch, ParamEntry, StockDb};

use crate::Result;

/// The group-level expression switch stock starts from (`0x006a2edc`).
const DEFAULT_EXP_SWITCH: ExpSwitch = ExpSwitch {
    switch_type: 2,
    inverted: false,
    latch_emulation: false,
    delay_ms: 600,
};

/// Writes every group as `default_param` rows plus one `default_exp_switch`
/// row (`StockWrite::import_legacy_defaults`).
pub fn import_legacy_model_defaults(
    db: &mut StockDb,
    groups: &[LegacyModelDefaults],
) -> Result<()> {
    Ok(db.transaction(|tx| {
        for group in groups {
            let (params, exp_switch) = legacy_rows(group);
            tx.import_legacy_defaults(&group.group, &params, exp_switch)?;
        }
        Ok(())
    })?)
}

/// The database rows of one group. A missing value is `""` and a missing
/// expression or trim is 0, as in the zeroed stock map node.
///
/// Stock keeps one expression switch per group; the last `*_exp_bypass_*`
/// key read wins. tetrad-settings reports the bypass keys per param, with
/// defaults for absent keys, so Tetrad takes the switch of the highest param
/// id whose bypass values are not all default (inferred; equal to stock when
/// one param carries the bypass keys).
fn legacy_rows(group: &LegacyModelDefaults) -> (Vec<ParamEntry>, ExpSwitch) {
    let switch = |param: &LegacyParam| ExpSwitch {
        switch_type: i64::from(param.bypass_type),
        inverted: param.bypass_inverted,
        latch_emulation: param.bypass_latch_emulation,
        delay_ms: i64::from(param.bypass_delay_ms),
    };
    let params = group
        .params
        .iter()
        .map(|(&index, param)| ParamEntry {
            index: i64::from(index),
            value: param
                .value
                .as_ref()
                .and_then(|value| value.to_string_value())
                .unwrap_or_default(),
            exp: i64::from(param.expression.unwrap_or(0)),
            exp_trim_max: f64::from(param.trim_max.unwrap_or(0.0)),
            exp_trim_min: f64::from(param.trim_min.unwrap_or(0.0)),
        })
        .collect();
    let exp_switch = group
        .params
        .values()
        .rev()
        .map(switch)
        .find(|exp_switch| *exp_switch != DEFAULT_EXP_SWITCH)
        .unwrap_or(DEFAULT_EXP_SWITCH);
    (params, exp_switch)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tetrad_settings::Variant;

    use super::*;

    #[test]
    fn legacy_group_maps_to_stock_rows_and_one_exp_switch() {
        let group = LegacyModelDefaults {
            group: "10012001".into(),
            params: BTreeMap::from([
                (
                    3,
                    LegacyParam {
                        value: Some(Variant::String("0.5".into())),
                        expression: Some(1),
                        trim_min: Some(0.25),
                        trim_max: Some(0.75),
                        ..LegacyParam::default()
                    },
                ),
                (
                    7,
                    LegacyParam {
                        bypass_type: 1,
                        bypass_delay_ms: 300,
                        ..LegacyParam::default()
                    },
                ),
                (9, LegacyParam::default()),
            ]),
        };
        let (params, exp_switch) = legacy_rows(&group);
        assert_eq!(
            params[0],
            ParamEntry {
                index: 3,
                value: "0.5".into(),
                exp: 1,
                exp_trim_max: 0.75,
                exp_trim_min: 0.25,
            }
        );
        assert_eq!((params[1].value.as_str(), params[1].exp), ("", 0));
        assert_eq!(params.len(), 3);
        assert_eq!(
            exp_switch,
            ExpSwitch {
                switch_type: 1,
                delay_ms: 300,
                ..DEFAULT_EXP_SWITCH
            }
        );
    }
}

//! Global EQ ("Parametric-5", model 4004): the I/O page's GLOBAL EQ screen over `[GlobalEq]` and
//! the DSP `0x3c` OutputEqualizerParam writes (`0x5150a0`).
//!
//! - Values are stored normalized, as the stock saver writes them (`0x2fa580`): every
//!   index of the model list, MUTE and the block BYPASS (the ON button) included. Missing
//!   ones come from the boot loader `0x2fbf08`: with the factory default model preset, the
//!   catalog defaults and BYPASS 1.0 (OFF); with a user default (`0x1f57b8`), its rows over
//!   a fresh block (`0x1f7e1c` → `0x1f7b10`), BYPASS 0 (ON, the fresh block value; I).
//!   Without the catalog the page has no values, so tetrad-ui keeps it disabled.
//! - One edit (`0x2f9fa8`) goes to each output pair the curve is assigned to, while ON.
//! - ON (`0x2fb8f4`), an assignment (`0x2fbce0`) and the boot push (`0x2fc9f4`) send
//!   parameters `0..=25` and then the per-output tail (`0x2faeac`): band BYPASS flags and
//!   OUTPUT when ON and assigned, else every BYPASS 0 and the OUTPUT default.

//!
//! Global EQ and I/O Virtual Device Presets (`GlobalEQPresetPopup` `0x227a84`) store these values as `model_presets` rows of models 4004
//! and 31000: `preset_rows`, `io_preset_rows`, `load_preset`, `load_io_preset`.

use std::collections::BTreeMap;

use tetrad_catalog::{ModelSpec, ParamSpec, ParameterReference};
use tetrad_dsp_protocol::controls::ControlRequest;
use tetrad_engine::SceneIndex;
use tetrad_settings::device::Hardware;
use tetrad_settings::io::{ConnectorId, EqParam, GLOBAL_EQ_MODEL};
use tetrad_storage::stock::{ParamEntry, StockDb};
use tetrad_ui::{EqBand, OutputJack, SettingId, SettingValue};

use super::actions::{ActionError, update_settings};
use super::{DspCommand, Effect, State};

/// OUT 1/2 and OUT 3/4: the `0x3c` output selector and the ASSIGN flag index.
const OUTPUTS: [u8; 2] = [0, 1];

/// The model 4004 parameter behind a tetrad-ui Global EQ setting (`None`: EqEnabled or not
/// an EQ setting).
fn param(id: SettingId) -> Option<EqParam> {
    let band = |band: EqBand| band.get();
    Some(match id {
        SettingId::EqType(b) => EqParam::Type(band(b)),
        SettingId::EqGain(b) => EqParam::Gain(band(b)),
        SettingId::EqFreq(b) => EqParam::Freq(band(b)),
        SettingId::EqQ(b) => EqParam::Q(band(b)),
        SettingId::EqBypass(b) => EqParam::Bypass(band(b)),
        SettingId::EqOutput => EqParam::Output,
        SettingId::EqAssign(jack) => EqParam::Assign(output(jack)),
        _ => return None,
    })
}

fn output(jack: OutputJack) -> u8 {
    match jack {
        OutputJack::Out12 => 0,
        OutputJack::Out34 => 1,
    }
}

pub(super) fn model(state: &State) -> Option<&ModelSpec> {
    state.specs.as_ref()?.model(GLOBAL_EQ_MODEL)
}

/// The block BYPASS entry, the last of the stored list: the XML parameters, MUTE, BYPASS
/// (`0x2ecc70` = count − 1).
pub(super) fn bypass_index(spec: &ModelSpec) -> u16 {
    spec.parameters.len() as u16 + 1
}

/// The ON button is the stored BYPASS (`0x2ecd4c`: above 0.5 is OFF). A fresh device has
/// none: OFF with the factory default model preset of `0x2fbf08` (review r11 N1).
pub(super) fn bypassed(state: &State) -> bool {
    model(state)
        .and_then(|spec| stored(state, spec, bypass_index(spec)))
        .is_none_or(|value| value > 0.5)
}

/// Every index of the stored list, as the saver `0x2fa580` writes them (review r11 N9).
pub(super) fn every_index(state: &State, spec: &ModelSpec) -> BTreeMap<u16, f32> {
    (0..=bypass_index(spec))
        .filter_map(|index| Some((index, stored(state, spec, index)?)))
        .collect()
}

/// Stored normalized value, else the boot value of `0x2fbf08`: the user default model preset
/// block ([`State::global_eq_default`]), or the catalog default with MUTE 0 and BYPASS 1.
pub(super) fn stored(state: &State, spec: &ModelSpec, param: u16) -> Option<f32> {
    let saved = &state.settings.settings().global_eq.params;
    if let Some(&value) = saved.get(&param) {
        return Some(value);
    }
    if let Some(default) = &state.global_eq_default {
        return default.get(&param).copied();
    }
    match bypass_index(spec).checked_sub(param)? {
        0 => Some(1.0),
        1 => Some(0.0),
        _ => Some(spec.parameter(u32::from(param))?.default_normalized()),
    }
}

/// Boot `0x2fbf08`: the model 4004 values of the user default model preset, or `None` for the
/// factory default. `0x1f57b8` reads the zenui.db `default_model_presets` row; `0x1f7b10` puts
/// its rows (zenui.db, or factory.db when `isFactory`, `0x1f78f4`) over a fresh catalog block.
/// MUTE and BYPASS keep the fresh block values (0; I). The result stays in memory, as stock
/// keeps it in the live block; the saver writes it with the next edit (`0x2fa580`).
pub fn user_default(
    state: &State,
    factory: impl FnOnce() -> tetrad_storage::Result<StockDb>,
) -> tetrad_storage::Result<Option<BTreeMap<u16, f32>>> {
    let (Some(specs), Some(user)) = (
        state.specs.as_ref(),
        state
            .library
            .as_ref()
            .and_then(|library| library.database.as_ref()),
    ) else {
        return Ok(None);
    };
    let Some(default) = (user.view().default_model_presets()?)
        .into_iter()
        .find(|default| default.model == i64::from(GLOBAL_EQ_MODEL))
    else {
        return Ok(None);
    };
    let rows = if default.factory {
        factory()?.view().model_preset_params(default.preset_id)?
    } else {
        user.view().model_preset_params(default.preset_id)?
    };
    let Some(block) = super::preset::model_preset_block(specs, GLOBAL_EQ_MODEL, None, &rows) else {
        return Ok(None);
    };
    let scene = SceneIndex::new(0).expect("scene 0");
    let values = block.params.iter().map(|param| param.at(scene));
    let mute_and_bypass = [0.0, 0.0];
    Ok(Some((0u16..).zip(values.chain(mute_and_bypass)).collect()))
}

/// tetrad-ui lists TYPE as HI PASS, LO PASS, HI SHELF, LO SHELF, PEAK; stock steps are PEAK,
/// HI PASS, LO PASS, HI SHELF, LO SHELF (`0x639f34`).
const fn stock_type(choice: u16) -> u16 {
    (choice + 1) % 5
}

/// Fixed-point tetrad-ui decimals per parameter (settings `SettingKind::Number`).
fn scale(param: EqParam) -> f32 {
    match param {
        EqParam::Gain(_) | EqParam::Output => 10.0,
        EqParam::Q(_) => 100.0,
        _ => 1.0,
    }
}

/// Normalized → tetrad-ui value.
fn view(param: EqParam, spec: &ParamSpec, normalized: f32) -> Option<SettingValue> {
    Some(match param {
        EqParam::Type(_) => {
            let step = (normalized * 4.0).round().clamp(0.0, 4.0) as u16;
            SettingValue::Choice((step + 4) % 5)
        }
        EqParam::Bypass(_) => SettingValue::Bool(normalized < 0.5),
        EqParam::Assign(_) => SettingValue::Bool(normalized > 0.0),
        _ => {
            let display = spec.scale.to_display(normalized).ok()?;
            SettingValue::Number((display * scale(param)).round() as i32)
        }
    })
}

/// tetrad-ui value → normalized, snapped to the parameter steps (`0x5cc304`).
fn normalized(param: EqParam, spec: &ParamSpec, value: &SettingValue) -> Option<f32> {
    match (param, value) {
        (EqParam::Type(_), SettingValue::Choice(choice)) if *choice < 5 => {
            Some(f32::from(stock_type(*choice)) / 4.0)
        }
        (EqParam::Bypass(_), SettingValue::Bool(bypassed)) => Some(f32::from(u8::from(!bypassed))),
        (EqParam::Assign(_), SettingValue::Bool(on)) => Some(f32::from(u8::from(*on))),
        (_, SettingValue::Number(number)) => {
            let value = spec
                .normalize(*number as f32 / scale(param))?
                .clamp(0.0, 1.0);
            let step = spec.scale.step_index(value).ok();
            Some(match step {
                Some(step) if spec.scale.steps() > 0 => {
                    let last = i64::from(spec.scale.steps() - 1);
                    spec.scale.step_value(i64::from(step).min(last)).ok()?
                }
                _ => value,
            })
        }
        _ => None,
    }
}

fn send(output: u8, parameter: u16, value: f32) -> Effect {
    Effect::Dsp(DspCommand::Control(
        ControlRequest::OutputEqualizerParameter {
            output,
            parameter: i32::from(parameter),
            value_bits: value.to_bits(),
        },
    ))
}

/// The value a double tap restores (manual "Global EQ MENU": the current default preset):
/// the user default model preset block ([`State::global_eq_default`]), else the factory
/// default, the catalog default (`0x2fbf08`). `None` for other ids.
pub(super) fn default_setting(state: &State, id: SettingId) -> Option<SettingValue> {
    let param = param(id)?;
    let spec = model(state)?.parameter(u32::from(param.index()))?;
    let normalized = match &state.global_eq_default {
        Some(default) => *default.get(&param.index())?,
        None => spec.default_normalized(),
    };
    view(param, spec, normalized)
}

/// The Global EQ values for the snapshot; empty without the catalog model.
pub(super) fn settings(state: &State) -> Vec<(SettingId, SettingValue)> {
    let Some(spec) = model(state) else {
        return Vec::new();
    };
    let bands = (0..tetrad_settings::io::GLOBAL_EQ_BANDS)
        .filter_map(EqBand::new)
        .flat_map(|band| {
            [
                SettingId::EqType(band),
                SettingId::EqGain(band),
                SettingId::EqFreq(band),
                SettingId::EqQ(band),
                SettingId::EqBypass(band),
            ]
        });
    let jacks = [OutputJack::Out12, OutputJack::Out34].map(SettingId::EqAssign);
    let mut values: Vec<_> = bands
        .chain([SettingId::EqOutput])
        .chain(jacks)
        .filter_map(|id| {
            let param = param(id)?;
            let normalized = stored(state, spec, param.index())?;
            let value = view(param, spec.parameter(u32::from(param.index()))?, normalized)?;
            Some((id, value))
        })
        .collect();
    values.push((SettingId::EqEnabled, SettingValue::Bool(!bypassed(state))));
    values
}

/// A tetrad-ui Global EQ edit: store it, then the stock DSP writes. `None` for other ids.
pub(super) fn change(
    state: &mut State,
    id: SettingId,
    value: &SettingValue,
) -> Option<Result<Vec<Effect>, ActionError>> {
    let param = param(id);
    if param.is_none() && id != SettingId::EqEnabled {
        return None;
    }
    let Some(spec) = model(state) else {
        return Some(Err(ActionError::Unavailable("the Global EQ model (4004)")));
    };
    let (index, normalized) = match (param, value) {
        (None, SettingValue::Bool(on)) => (bypass_index(spec), f32::from(u8::from(!on))),
        (Some(param), value) => match spec
            .parameter(u32::from(param.index()))
            .and_then(|spec| normalized(param, spec, value))
        {
            Some(normalized) => (param.index(), normalized),
            None => return Some(Err(ActionError::Unsupported("Global EQ value"))),
        },
        (None, _) => return Some(Err(ActionError::Unsupported("GLOBAL EQ value"))),
    };
    let mut all = every_index(state, spec);
    all.insert(index, normalized);
    let mut effects = update_settings(state, |settings| settings.global_eq.params = all);
    let Some(param) = param else {
        effects.extend(push(state));
        return Some(Ok(effects));
    };
    effects.extend(match param {
        EqParam::Assign(output) => assigned(state, output),
        _ if bypassed(state) => Vec::new(),
        _ => OUTPUTS
            .into_iter()
            .filter(|&output| assigned_to(state, output))
            .map(|output| send(output, param.index(), normalized))
            .collect(),
    });
    Some(Ok(effects))
}

fn assigned_to(state: &State, output: u8) -> bool {
    model(state)
        .and_then(|spec| stored(state, spec, EqParam::Assign(output).index()))
        .is_some_and(|value| value > 0.0)
}

/// `0x2fbce0`: unassigned → the neutral tail; assigned → every value, then the tail.
fn assigned(state: &State, output: u8) -> Vec<Effect> {
    let mut effects = Vec::new();
    if assigned_to(state, output) {
        effects.extend(values(state, output));
    }
    effects.extend(tail(state, output));
    effects
}

/// `0x2fc9f4` without the reload: both outputs get every value and their tail. Boot and
/// the ON button run it.
pub(super) fn push(state: &State) -> Vec<Effect> {
    OUTPUTS
        .into_iter()
        .flat_map(|output| {
            let mut effects = values(state, output);
            effects.extend(tail(state, output));
            effects
        })
        .collect()
}

fn values(state: &State, output: u8) -> Vec<Effect> {
    let Some(spec) = model(state) else {
        return Vec::new();
    };
    (0..EqParam::DSP_PARAMS)
        .filter_map(|param| Some(send(output, param, stored(state, spec, param)?)))
        .collect()
}

/// `0x2faeac`: ON and assigned → band BYPASS as 1.0 / 0.0 and the stored OUTPUT; else
/// BYPASS 0 for every band and the catalog OUTPUT default.
fn tail(state: &State, output: u8) -> Vec<Effect> {
    let Some(spec) = model(state) else {
        return Vec::new();
    };
    let live = !bypassed(state) && assigned_to(state, output);
    let bands = (0..tetrad_settings::io::GLOBAL_EQ_BANDS).filter_map(|band| {
        let index = EqParam::Bypass(band).index();
        let on = live && stored(state, spec, index)? >= 0.5;
        Some(send(output, index, f32::from(u8::from(on))))
    });
    let index = EqParam::Output.index();
    let level = spec.parameter(u32::from(index)).map(|param| {
        let value = if live {
            stored(state, spec, index).unwrap_or_else(|| param.default_normalized())
        } else {
            param.default_normalized()
        };
        send(output, index, value)
    });
    bands.chain(level).collect()
}

/// Model of the I/O Settings presets ("IOSettings", hidden).
pub const IO_PRESET_MODEL: u32 = 31_000;
/// IOPreset parameters `0x2e`, `0x2f`: OUTPUT 1/2 LINK and OUTPUT 3/4 LINK (`0x361c3c`,
/// `0x362d18`).
const IO_LINKS: [i64; 2] = [0x2e, 0x2f];
/// EXP 1 MIN, EXP 1 MAX, EXP 2 MIN, EXP 2 MAX (ModelRepo order after the links; the stock
/// index table `0xa2e19c` is not read: I).
const IO_CALIBRATION: [i64; 4] = [48, 49, 50, 51];

fn row(index: i64, value: String) -> ParamEntry {
    ParamEntry {
        index,
        value,
        exp: 0,
        exp_trim_max: 1.0,
        exp_trim_min: 0.0,
    }
}

/// Global EQ preset rows (`create` / `overwrite` of `0x227a84` over the live 4004 block,
/// `0x2f9ce8`): the stored values of the XML parameters and MUTE. ON (BYPASS) is not a row.
pub(super) fn preset_rows(state: &State) -> Option<Vec<ParamEntry>> {
    let spec = model(state)?;
    let mute = spec.parameters.len() as u16;
    Some(
        (0..=mute)
            .filter_map(|index| Some(row(index.into(), stored(state, spec, index)?.to_string())))
            .collect(),
    )
}

/// Global EQ preset load (`onLoadGlobalEQPreset`): the preset values of the XML parameters go
/// to `[GlobalEq]`, then every value to the DSP (`0x2fc9f4` without the reload). MUTE and ON
/// keep their values (I). Rows outside the catalog or outside 0..=1 are left out.
pub(super) fn load_preset(
    state: &mut State,
    rows: &[ParamEntry],
) -> Result<Vec<Effect>, ActionError> {
    let spec = model(state).ok_or(ActionError::Unavailable("the Global EQ model (4004)"))?;
    let mut all = every_index(state, spec);
    let count = spec.parameters.len();
    for row in rows {
        let Some(index) = u16::try_from(row.index)
            .ok()
            .filter(|i| usize::from(*i) < count)
        else {
            continue;
        };
        if let Ok(value) = row.value.parse::<f32>()
            && (0.0..=1.0).contains(&value)
        {
            all.insert(index, value);
        }
    }
    let mut effects = update_settings(state, |settings| settings.global_eq.params = all);
    effects.extend(push(state));
    Ok(effects)
}

/// The connector a model 31000 parameter replaces (`replaces` = connector id; the stock maps
/// ids below `0x6ae`, not `0x57a`/`0x5de`, `0x361c3c`).
fn connector(param: &ParamSpec) -> Option<ConnectorId> {
    match param.replaces.as_slice() {
        [ParameterReference::Index(id)] => ConnectorId::from_id(u16::try_from(*id).ok()?),
        _ => None,
    }
}

/// Connectors an I/O preset stores on the QC scale (`0x15399b8`: ids 100, 200, 500, 600).
const QC_SCALED: [u16; 4] = [100, 200, 500, 600];

/// `0x47faac` → `0x47f9b0`: I/O presets keep [`QC_SCALED`] values on the QC range −12..=60 dB;
/// the mini (hardware types 2 and 3, `0x496238`) has −12..=30 dB. A value goes to dB on the
/// `from` range (`0x64b8f8`, skew 1: linear), is clamped to the `to` range and normalised
/// again. Save converts this hardware → QC (`0x361c3c`), load QC → this hardware (`0x362d18`).
fn rescale(id: ConnectorId, value: f32, from: Hardware, to: Hardware) -> f32 {
    let top = |hardware| match hardware {
        Hardware::QuadCortex => 60.0,
        Hardware::Atma => 30.0,
    };
    if from == to || !QC_SCALED.contains(&id.id()) {
        return value;
    }
    let db = -12.0 + value * (top(from) + 12.0);
    (db.clamp(-12.0, top(to)) + 12.0) / (top(to) + 12.0)
}

/// I/O preset rows (`0x361c3c`): each live connector of this hardware at its IOPreset index
/// ([`rescale`] to the QC range), the two LINK flags (`0x2e`, `0x2f`) and the four EXP
/// calibration ints.
pub(super) fn io_preset_rows(state: &State) -> Option<Vec<ParamEntry>> {
    let spec = state.specs.as_ref()?.model(IO_PRESET_MODEL)?;
    let settings = state.settings.settings();
    let hardware = state.io.hardware();
    let mut rows: Vec<ParamEntry> = spec
        .parameters
        .iter()
        .filter_map(|param| {
            let id = connector(param)?;
            id.spec(hardware)?;
            let value = rescale(id, state.io.value(id), hardware, Hardware::QuadCortex);
            Some(row(param.index.into(), value.to_string()))
        })
        .collect();
    let links = [settings.io.out12_paired, settings.io.out34_paired];
    rows.extend(
        IO_LINKS
            .into_iter()
            .zip(links)
            .map(|(index, on)| row(index, u8::from(on).to_string())),
    );
    let [one, two] = settings.expression.pedals;
    let calibration = [one.min, one.max, two.min, two.max];
    rows.extend(
        IO_CALIBRATION
            .into_iter()
            .zip(calibration)
            .map(|(index, value)| row(index, value.to_string())),
    );
    Some(rows)
}

/// A model 31000 preset resolved for this hardware (`0x362d18`): the LINK rows (above 0.5
/// pairs the outputs), each connector row in this hardware's range (`rescale` from the QC
/// range), and the rows for the EXP calibration.
#[derive(Debug, Clone, PartialEq)]
pub struct IoPreset {
    links: [Option<bool>; 2],
    connectors: Vec<(ConnectorId, f32)>,
    rows: Vec<ParamEntry>,
}

/// Resolves the rows of an I/O preset before its load starts (refused without model 31000).
pub(super) fn io_preset(state: &State, rows: Vec<ParamEntry>) -> Result<IoPreset, ActionError> {
    let spec = (state.specs.as_ref())
        .and_then(|specs| specs.model(IO_PRESET_MODEL))
        .ok_or(ActionError::Unavailable("the I/O Settings model (31000)"))?;
    let hardware = state.io.hardware();
    let value = |row: &ParamEntry| {
        row.value
            .parse::<f32>()
            .ok()
            .filter(|value| (0.0..=1.0).contains(value))
    };
    let mut links = [None; 2];
    let mut connectors = Vec::new();
    for row in &rows {
        let Some(value) = value(row) else { continue };
        if let Some(link) = IO_LINKS.iter().position(|&index| index == row.index) {
            links[link] = Some(value > 0.5);
            continue;
        }
        let param = usize::try_from(row.index)
            .ok()
            .and_then(|index| spec.parameters.get(index));
        if let Some(id) = param
            .and_then(connector)
            .filter(|id| id.spec(hardware).is_some())
        {
            connectors.push((id, rescale(id, value, Hardware::QuadCortex, hardware)));
        }
    }
    Ok(IoPreset {
        links,
        connectors,
        rows,
    })
}

/// I/O preset load (`0x362d18`), run after the input mute settled
/// ([`super::jacks::settle_io_loads`]): the links and connectors go to `ConnectorsSettings`,
/// then the I/O reload (`0x360da8`, `jacks::reload`: live values from the file, every
/// connector in table order, both pairs, one Control IOSettings push), then the EXP
/// calibration rows (`0x3634e8` → `0x320fdc`, `jacks::load_calibration`). A 48 V row goes
/// through the same apply path, so bring-up refuses it without `--allow-phantom`.
pub(super) fn load_io_preset(state: &mut State, preset: IoPreset) -> Vec<Effect> {
    let IoPreset {
        links,
        connectors,
        rows,
    } = preset;
    let mut effects = update_settings(state, |settings| {
        let io = &mut settings.io;
        if let Some(on) = links[0] {
            io.out12_paired = on;
        }
        if let Some(on) = links[1] {
            io.out34_paired = on;
        }
        for (id, value) in connectors {
            io.params.entry(id.id()).or_default().value = Some(value);
        }
    });
    effects.extend(super::jacks::reload(state));
    effects.extend(super::jacks::load_calibration(state, &rows));
    effects
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `0x47f9b0`: a QC-scaled connector keeps its dB across hardware (0 dB is 12/72 on the
    /// QC and 12/42 on the mini), a QC value above the mini top clamps to it, and the same
    /// hardware or another connector keeps the value.
    #[test]
    fn io_preset_values_rescale_between_qc_and_mini() {
        let (qc, mini) = (Hardware::QuadCortex, Hardware::Atma);
        let gain = ConnectorId::from_id(100).unwrap();
        let close = |a: f32, b: f32| (a - b).abs() < 1e-6;
        assert!(close(rescale(gain, 12.0 / 72.0, qc, mini), 12.0 / 42.0));
        assert!(close(rescale(gain, 12.0 / 42.0, mini, qc), 12.0 / 72.0));
        assert_eq!(rescale(gain, 1.0, qc, mini), 1.0);
        assert_eq!(rescale(gain, 0.3, qc, qc), 0.3);
        let other = (101..200).find_map(ConnectorId::from_id).unwrap();
        assert_eq!(rescale(other, 0.3, qc, mini), 0.3);
    }
}

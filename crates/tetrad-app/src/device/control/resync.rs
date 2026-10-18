//! Inputs of the DefaultParameters (19) and ModelPreset (71) resync families, which tetrad-control
//! serializes with `block::serialize` (`0x67cb3c(.., 1, 0, 0)`): the `default_param` keys, the
//! capture default block (`0x704530`) and the record list (`0x1f6280`).
//!
//! Blocks are in stored form: the catalog parameters at scene 0 (strings as text), then MUTE;
//! `bypass_expression[0]` is BYPASS and `expression_bypass_info` holds the switches in the
//! stock loader positions (`0x681360`).

use std::collections::BTreeMap;

use tetrad_catalog::{ModelSpec, ParameterType, Specs};
use tetrad_control::block::{CaptureDefault, ModelPresetRecord, TypedBlock, ValueCase};
use tetrad_control::codecs::CortexMessageType;
use tetrad_control::device::{DeviceError as ModelError, DeviceResult};
use tetrad_core::performance::expression::{BypassSwitchType, DEFAULT_BYPASS_DELAY_MS};
use tetrad_engine::Instance;
use tetrad_preset::{
    ExpressionBypassInfo, Model, OUTPUT_CONTROL_ID, Param, ParamValue, SCENES, SceneValue,
};
use tetrad_storage::stock::{ExpSwitch, ParamEntry, StockDb, StockView};

use super::super::{Effect, State, Subsystem, preset};
use super::grid::SPECIAL_FACTORY_PRESET;
use super::{CAPTURE_MODELS, StateModel, split_capture_key};

const DEFAULTS: CortexMessageType = CortexMessageType::DefaultParameters;
const PRESETS: CortexMessageType = CortexMessageType::ModelPreset;
/// Capture parameter 5 (`file_name`): the capture key and name.
const FILE_NAME: usize = 5;
/// The name of the special entries (`0xa18280`, read with `fromAscii`, so Latin-1). The bytes
/// are UTF-8 `®` (`C2 AE`); `toAscii` writes them back unchanged, as this text does.
const SPECIAL_NAME: &str = "Neural DSP® Default";
/// Hidden models with a special entry after the catalog pass (`0x1f61e8`).
const EXTRA_SPECIAL_MODELS: [u32; 2] = [4004, 31000];
/// The switch of a new runtime block at `+0xbc`, `+0xc8` and `+0xd4` (ctor `0x2ef15c`): HeelToe,
/// 600 ms. `0x6678d4` writes the same at `+0xbc`.
const NEW_BLOCK_SWITCH: ExpSwitch = ExpSwitch {
    switch_type: 2,
    inverted: false,
    latch_emulation: false,
    delay_ms: 600,
};

fn refused(message_type: CortexMessageType, what: impl ToString) -> ModelError {
    ModelError::Refused(message_type, what.to_string())
}

/// `zenui.db`. Stock queries a database that did not open and gets no rows.
fn user_database<'a>(model: &'a StateModel<'_>) -> Option<&'a StockDb> {
    model.state.library.as_ref()?.database.as_ref()
}

fn specs<'a>(
    model: &'a StateModel<'_>,
    message_type: CortexMessageType,
) -> DeviceResult<&'a Specs> {
    model
        .state
        .specs
        .as_ref()
        .ok_or_else(|| refused(message_type, "the catalog is not loaded"))
}

/// `0x6678b0` → `0x6a2390`: `SELECT DISTINCT model_id FROM default_param`.
pub(super) fn default_param_ids(model: &StateModel<'_>) -> DeviceResult<Vec<String>> {
    let Some(database) = user_database(model) else {
        return Ok(Vec::new());
    };
    database
        .view()
        .default_param_models()
        .map_err(|error| refused(DEFAULTS, error))
}

/// `0x15d890` per key: the user capture store only (`0x765520` `vt+0x40` by key, C; the
/// `vt+0x5c` name lookup after it is not built, I), so a Downloads or factory capture gives
/// no entry (review r32 E3). Then its default block `0x704530` ([`preset::capture_record`]):
/// model 14001 when the capture version is "2" (`0x6f6a40`, `+0x40`), else 14000; parameter
/// 5 = record key + name in all eight scenes (`0x6f7018`); the user default rows
/// (`0x66824c`). The entry id is the key (inferred: capture `+4`). `0x6f734c` (block `+0xe0`)
/// is not serialized. A capture whose entry cannot be built is skipped with a notice (O): one
/// bad row never fails the whole resync.
pub(super) fn capture_default(
    model: &mut StateModel<'_>,
    key: &str,
) -> DeviceResult<Option<CaptureDefault>> {
    Ok(user_capture_default(model, key)?.unwrap_or_else(|reason| {
        let log = format!("DefaultParameters: capture {key:?} skipped: {reason}");
        model.effects.push(Effect::Notice(Subsystem::Control, log));
        None
    }))
}

/// [`capture_default`]: the outer error fails the resync (no cipher, no catalog); the inner
/// one is this capture's reason to be skipped.
fn user_capture_default(
    model: &StateModel<'_>,
    key: &str,
) -> DeviceResult<Result<Option<CaptureDefault>, String>> {
    use tetrad_library::stock::{CaptureSource, find_capture};
    let Some(database) = user_database(model) else {
        return Ok(Ok(None));
    };
    let view = database.view();
    let cipher = model
        .cipher
        .ok_or_else(|| refused(DEFAULTS, "the device media cipher is not set"))?;
    let specs = specs(model, DEFAULTS)?;
    let build = || -> Result<Option<CaptureDefault>, String> {
        match find_capture(&view, None, key).map_err(|error| error.to_string())? {
            Some(CaptureSource::User(_)) => {}
            _ => return Ok(None),
        }
        let Some(record) =
            preset::capture_record(&view, cipher, key).map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        let id = record.model;
        let spec = (specs.model(id)).ok_or_else(|| format!("model {id} is not in the catalog"))?;
        let mut block = stored(&tetrad_engine::new_instance(spec));
        *block
            .params
            .get_mut(FILE_NAME)
            .ok_or_else(|| format!("model {id} has no parameter {FILE_NAME}"))? =
            Param::new(ParamValue::String(record.file_name));
        block.expression_bypass_info = vec![switch_entry(NEW_BLOCK_SWITCH)];
        apply_user_defaults(&view, &mut block, spec).map_err(|error| error.to_string())?;
        Ok(Some(CaptureDefault {
            id: key.to_owned(),
            block: TypedBlock {
                model: block,
                cases: cases(spec),
            },
        }))
    };
    Ok(build())
}

/// `0x667acc(.., "", 0)`: the `default_param` rows of the block key (capture: parameter 5 cut
/// to 64 UTF-16 units, else the model id; `0x66768c`), by position in the result (index order
/// of `SELECT *`, inferred). All rows apply when they cover every runtime parameter (with
/// MUTE and BYPASS); else at most the catalog parameters. Each value goes to all eight scenes
/// (text for `string`, else `toFloat`, 0 when not a number) except the sidechain source and a
/// capture's parameter 5; EXP code and range go on every row but MUTE. Then the
/// `default_exp_switch` row replaces the `+0xbc` switch. Without rows the block is reset
/// (`0x6678d4`): it already holds the catalog defaults.
fn apply_user_defaults(
    view: &StockView<'_>,
    block: &mut Model,
    spec: &ModelSpec,
) -> tetrad_storage::Result<()> {
    let capture = CAPTURE_MODELS.contains(&spec.id);
    let capture_key = block
        .params
        .get(FILE_NAME)
        .and_then(|param| param.value(0))
        .and_then(|value| match value {
            ParamValue::String(text) if capture && !text.is_empty() => {
                Some(split_capture_key(text).map_or(text.as_str(), |(key, _)| key))
            }
            _ => None,
        });
    let key = capture_key.map_or_else(|| spec.id.to_string(), str::to_owned);
    let rows = view.default_params(&key)?;
    let runtime = block.params.len() + 1;
    let (mute, bypass) = (runtime - 2, runtime - 1);
    let limit = if rows.len() == runtime {
        runtime
    } else {
        rows.len().min(runtime).saturating_sub(2)
    };
    let sidechain = spec
        .sidechain
        .as_ref()
        .map(|sidechain| sidechain.source_param as usize);
    for (index, row) in rows.iter().enumerate().take(limit) {
        if Some(index) == sidechain {
            continue;
        }
        let (code, min, max) = (
            i32::try_from(row.exp).unwrap_or_default(),
            row.exp_trim_min as f32,
            row.exp_trim_max as f32,
        );
        if index == bypass {
            if let Some(expression) = block.bypass_expression.first_mut() {
                (
                    expression.expression,
                    expression.expression_min,
                    expression.expression_max,
                ) = (code, min, max);
            }
            continue;
        }
        let param = &mut block.params[index];
        if !(capture && index == FILE_NAME) {
            let value = match spec.parameter(index as u32) {
                Some(parameter) if parameter.kind == ParameterType::String => {
                    ParamValue::String(row.value.clone())
                }
                _ => ParamValue::Float(row.value.trim().parse().unwrap_or(0.0)),
            };
            param.values = vec![
                SceneValue {
                    value: Some(value),
                    ..SceneValue::default()
                };
                SCENES
            ];
        }
        if index != mute {
            (param.expression, param.expression_min, param.expression_max) =
                (Some(code), Some(min), Some(max));
        }
    }
    if let Some(switch) = view.default_exp_switch(&key)? {
        block.expression_bypass_info = vec![switch_entry(switch)];
    }
    Ok(())
}

/// `0x1f6280`: factory model presets (`0x8330cc`), then user ones (`0x6a603c`), each by id;
/// `is_default` when the user `default_model_presets` row of the hash names the same id and
/// database (`0x6a1954`). Then one special entry (`0x1f6038`: `SpecialFactoryModelPresetID`,
/// factory, "Neural DSP® Default") per model of [`special_models`], default when the user has
/// no default for the hash. A default entry carries its `0x1f7e74` block.
pub(super) fn model_preset_records(model: &StateModel<'_>) -> DeviceResult<Vec<ModelPresetRecord>> {
    let specs = specs(model, PRESETS)?;
    let (mut records, special) = model_preset_list(model.state, model.factory)?;
    for (hash, is_default) in special {
        let default_block = is_default
            .then(|| preset_block(specs, hash, &[], NEW_BLOCK_SWITCH))
            .transpose()?;
        records.push(special_record(hash, default_block));
    }
    Ok(records)
}

/// The special entry of `hash` ([`model_preset_records`]).
pub(super) fn special_record(hash: u32, default_block: Option<TypedBlock>) -> ModelPresetRecord {
    ModelPresetRecord {
        id: SPECIAL_FACTORY_PRESET.to_owned(),
        is_factory: true,
        hash,
        name: SPECIAL_NAME.to_owned(),
        default_block,
    }
}

/// A special entry of [`model_preset_list`]: its model hash and whether it is the default.
pub(super) type SpecialEntry = (u32, bool);

/// [`model_preset_records`] without the special blocks: the stored records, then the
/// `(hash, is_default)` of each special entry. The ModelPreset READ list sends no special
/// block, and the 602 blocks of the stock catalog cost about 1.6 ms on the host, so about
/// 0.2 s on the QC (A3).
pub(super) fn model_preset_list(
    state: &State,
    factory: Option<&StockDb>,
) -> DeviceResult<(Vec<ModelPresetRecord>, Vec<SpecialEntry>)> {
    let specs =
        (state.specs.as_ref()).ok_or_else(|| refused(PRESETS, "the catalog is not loaded"))?;
    let read = |error: tetrad_storage::Error| refused(PRESETS, error);
    let user = state
        .library
        .as_ref()
        .and_then(|library| library.database.as_ref());
    let mut defaults = BTreeMap::new();
    if let Some(user) = user {
        for row in user.view().default_model_presets().map_err(read)? {
            if let Ok(hash) = u32::try_from(row.model) {
                defaults.insert(hash, (row.preset_id, row.factory));
            }
        }
    }
    let mut records = Vec::new();
    for (database, is_factory) in [(factory, true), (user, false)] {
        let Some(database) = database else {
            continue;
        };
        let view = database.view();
        for preset in view.model_presets().map_err(read)? {
            // Stock reads the `hash` column as an unsigned word.
            let hash = preset.model as u32;
            let default_block = (defaults.get(&hash) == Some(&(preset.id, is_factory)))
                .then(|| {
                    let rows = view.model_preset_params(preset.id).map_err(read)?;
                    preset_block(specs, hash, &rows, preset.exp_switch)
                })
                .transpose()?;
            records.push(ModelPresetRecord {
                id: preset.id.to_string(),
                is_factory,
                hash,
                name: preset.name,
                default_block,
            });
        }
    }
    let special = special_models(specs)
        .map(|hash| (hash, !defaults.contains_key(&hash)))
        .collect();
    Ok((records, special))
}

/// The special entry models in `0x1f6280` order: the catalog map `+0x30` (`0x5518d4`: per
/// category in id order, each model that is not hidden and not a clone; inferred: `+0xa8`
/// is the hidden flag), the clone set `+0xc0` (`0x550864`, id order) without hidden clones,
/// then 4004 and 31000. Clones come only from the set: the category pass skips blocks with a
/// base (`+0x10`). The S7 capture (C) has no entry for a hidden category (Splitter, Mixer,
/// internal routing and control, NC internal, `Utility_Deprecated`) nor for the hidden clones
/// 12014 and 12116: 602 entries on CorOS 4.1.0 (I: the map `+0x30` holds only visible
/// categories).
fn special_models(specs: &Specs) -> impl Iterator<Item = u32> + '_ {
    let listed = (specs.categories.values())
        .filter(|category| !category.hidden)
        .flat_map(|category| &category.model_ids)
        .copied()
        .filter(|&id| {
            specs
                .model(id)
                .is_some_and(|spec| !spec.hidden && spec.clones.is_empty())
        });
    let clones = (specs.models.values())
        .filter(|spec| !spec.clones.is_empty() && !spec.hidden)
        .map(|spec| spec.id);
    listed.chain(clones).chain(EXTRA_SPECIAL_MODELS)
}

/// `0x1f7e74`: a new catalog block of `hash` (`0x550030`) with the preset rows and switch
/// (`0x1f7b10`; the special id gives no rows and the reset switch, `0x6678d4`). Model 23000
/// keeps the ctor switches at `+0xc8`/`+0xd4`: `0x1f7b10` writes `+0xbc`, which has no
/// stored form.
fn preset_block(
    specs: &Specs,
    hash: u32,
    rows: &[ParamEntry],
    switch: ExpSwitch,
) -> DeviceResult<TypedBlock> {
    let missing = || refused(PRESETS, format!("model {hash} is not in the catalog"));
    let spec = specs.model(hash).ok_or_else(missing)?;
    let instance = preset::model_preset_block(specs, hash, None, rows).ok_or_else(missing)?;
    let mut block = stored(&instance);
    preset::model_preset_expression(&mut block, spec.parameters.len(), rows, switch)
        .map_err(|error| refused(PRESETS, error))?;
    if hash == OUTPUT_CONTROL_ID {
        block.expression_bypass_info = vec![switch_entry(NEW_BLOCK_SWITCH); 2];
    }
    Ok(TypedBlock {
        model: block,
        cases: cases(spec),
    })
}

/// The stored form of a new block at scene 0: every catalog parameter (string parameters as
/// text, empty when unset), then MUTE.
fn stored(instance: &Instance) -> Model {
    let text = |index: usize| {
        let param = instance
            .texts
            .iter()
            .find(|text| text.index as usize == index)?;
        let text = param.scenes.iter().flatten().next().cloned();
        Some(ParamValue::String(text.unwrap_or_default()))
    };
    let scene = tetrad_engine::SceneIndex::new(0).expect("scene 0 exists");
    let values = instance
        .params
        .iter()
        .enumerate()
        .map(|(index, value)| text(index).unwrap_or(ParamValue::Float(value.at(scene))))
        .chain([ParamValue::Float(f32::from(u8::from(
            instance.mute.at(scene),
        )))]);
    Model::block(instance.model.0, values)
}

/// The 12-byte switch `{type, invert, latch, delay}` as a stored entry (`0x8ab170` fields).
fn switch_entry(switch: ExpSwitch) -> ExpressionBypassInfo {
    ExpressionBypassInfo {
        kind: u32::try_from(switch.switch_type).unwrap_or(BypassSwitchType::HeelToe as u32),
        invert: switch.inverted,
        delay_ms: u32::try_from(switch.delay_ms).unwrap_or(DEFAULT_BYPASS_DELAY_MS),
        latch_emulation: switch.latch_emulation,
        unknown: Default::default(),
    }
}

/// `0x67cb3c` value cases from the catalog types (`param +0x8`): `int` → int, `string` →
/// string, any other type float. MUTE has no catalog entry and stays float.
fn cases(spec: &ModelSpec) -> Vec<ValueCase> {
    spec.parameters
        .iter()
        .map(|parameter| match parameter.kind {
            ParameterType::Int => ValueCase::Int,
            ParameterType::String => ValueCase::String,
            _ => ValueCase::Float,
        })
        .collect()
}

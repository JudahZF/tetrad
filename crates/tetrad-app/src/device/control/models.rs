//! ModelPreset (71), receiver `0x118f54`, over `zenui.db` and `factory.db`.

use tetrad_control::codecs::{
    ConfirmationAction, CortexMessageType, EnumValue, MessageAction, ModelPreset, ModelPresetID,
    ModelPresetMessage,
};
use tetrad_control::device::{DeviceError as ModelError, DeviceResult, GridEcho};
use tetrad_control::link::ConfirmationRequest;

use tetrad_catalog::ParameterType;
use tetrad_engine::{Column, Row, SceneIndex, Site, Slot};
use tetrad_preset::ParamValue;
use tetrad_settings::io::GLOBAL_EQ_MODEL;
use tetrad_storage::stock::{ExpSwitch, ParamEntry, StockDb};

use super::super::actions::{Action, DevicePresetEdit};
use super::super::global_eq::{self, IO_PRESET_MODEL};
use super::super::ui::picker::{default_switch, live_rows, preset_rows};
use super::super::{Effect, State, preset};
use super::grid::{self, DefaultValue};
use super::{StateModel, resync};

const PRESETS: CortexMessageType = CortexMessageType::ModelPreset;
const DEFAULTS: CortexMessageType = CortexMessageType::DefaultParameters;
/// Looper X: `0x15d74c` skips its parameters 0..=7.
const LOOPER_X: u32 = 27_000;

fn refused(what: impl ToString) -> ModelError {
    ModelError::Refused(PRESETS, what.to_string())
}

/// A stored user or factory preset id (`ModelPresetID.value`, the database row id).
fn preset_id(id: &ModelPresetID) -> DeviceResult<u64> {
    let value = id.value.as_deref().unwrap_or_default();
    value
        .parse()
        .map_err(|_| refused(format!("model preset id {value:?}")))
}

/// `0x118f54`. READ with no row, column, hash or capture version sends the whole list
/// (`0x1d037c(msg, 0)`: action word `+0x30` = 1, UPDATE; S7 capture, C): the resync records,
/// but a default entry with the special id carries no block. Stock builds the list in each
/// READ; Tetrad builds it once per `State::model_preset_revision` (`State::model_preset_list`,
/// A3, owned). UPDATE renames (`0x1f6d78`) and sets a default (`0x1f5a14`) per entry; DELETE
/// deletes each id (`0x1f6cc8`). Each change is pushed (`0x11cf1c` UPDATE with the entry,
/// `0x11dd58` DELETE with the id). CREATE stores a grid block ([`create`]). Not modelled: the
/// `loaded_*` subscriptions (`0x116838`), READ by row, column, hash or capture version, the
/// delete of a preset that a rename collides with, and the UPDATE push of the old default.
pub(super) fn model_preset(
    model: &mut StateModel<'_>,
    request: &ModelPresetMessage,
) -> DeviceResult<Vec<ModelPresetMessage>> {
    let loaded = [
        request.loaded_row,
        request.loaded_column,
        request.loaded_hash,
        request.loaded_capture_version,
    ]
    .iter()
    .any(Option::is_some);
    let action = match request.action.unwrap_or(MessageAction::Create.into()) {
        EnumValue::Known(action) => action,
        EnumValue::Unknown(_) => return Ok(Vec::new()),
    };
    match action {
        MessageAction::Read if !loaded => Ok(vec![read_list(model.state, model.factory)?.clone()]),
        MessageAction::Update => {
            for entry in &request.presets {
                update_entry(model, entry)?;
            }
            Ok(Vec::new())
        }
        // Stock drops the watcher; Tetrad keeps none.
        MessageAction::Delete if loaded => Ok(Vec::new()),
        MessageAction::Delete => {
            for entry in &request.presets {
                let id = entry.id.clone().unwrap_or_default();
                if id.is_factory == Some(true) {
                    return Err(refused("factory model presets are read-only"));
                }
                edit(model, DevicePresetEdit::Delete(preset_id(&id)?))?;
                push(
                    model,
                    MessageAction::Delete,
                    ModelPreset {
                        id: Some(id),
                        ..Default::default()
                    },
                );
            }
            Ok(Vec::new())
        }
        MessageAction::Create if !loaded => {
            create(model, request)?;
            Ok(Vec::new())
        }
        _ => Err(ModelError::Unsupported(PRESETS)),
    }
}

/// The READ list reply ([`State::model_preset_list`]), built when the model presets changed
/// since the last build: the resync records, then each special entry without its block.
pub(in crate::device) fn read_list<'a>(
    state: &'a mut State,
    factory: Option<&StockDb>,
) -> DeviceResult<&'a ModelPresetMessage> {
    let revision = state.model_preset_revision;
    let built = (state.model_preset_list.as_ref()).is_some_and(|(at, _)| *at == revision);
    if !built {
        state.model_preset_list = Some((revision, build_list(state, factory)?));
    }
    let (_, list) = state.model_preset_list.as_ref().expect("built above");
    Ok(list)
}

fn build_list(state: &State, factory: Option<&StockDb>) -> DeviceResult<ModelPresetMessage> {
    let (records, special) = resync::model_preset_list(state, factory)?;
    let special = (special.into_iter()).map(|(hash, is_default)| {
        let mut entry =
            tetrad_control::block::model_preset_entry(&resync::special_record(hash, None))?;
        entry.is_default = Some(is_default);
        Ok(entry)
    });
    let presets = (records.iter())
        .map(tetrad_control::block::model_preset_entry)
        .chain(special)
        .collect::<Result<_, tetrad_control::block::BlockError>>()
        .map_err(refused)?;
    Ok(ModelPresetMessage {
        action: Some(MessageAction::Update.into()),
        presets,
        ..Default::default()
    })
}

/// User presets of one model that `0x1f867c` allows (`0x836268(model) < 0x20`).
const PRESET_LIMIT: usize = 32;

/// CREATE (`0x118f54` case 0 → `0x1f8b9c`), named by `presets[0].name`. The source is
/// `create_from_hash`: 4004 the Global EQ (`0x2f9ce8`), 31000 the I/O settings (`0x362498`),
/// else the grid block at `create_from_row`/`create_from_column` (the live rows with the
/// stored EXP data, `ui::picker::live_rows`). A user preset of the same model and name is
/// overwritten (`0x1f88f8`, UPDATE push `0x11cf1c`). A new one (`0x1f867c`) is not stored
/// when the model has 32 user presets or a stored preset has the same values
/// ([`same_values`]): the failed store (`0x119068`) sends the "Preset Conflict"
/// Confirmation ([`preset_conflict`]) and nothing else. Else it is stored, made the default
/// when `presets[0].is_default` (`0x1f5a14`) and pushed (CREATE `0x11d950`). The pushed entry
/// is the id, hash and name (I: no block). Not modelled: the catalog flag test `0x2ea254`.
fn create(model: &mut StateModel<'_>, request: &ModelPresetMessage) -> DeviceResult<()> {
    let first = request.presets.first();
    let name = (first.and_then(|entry| entry.name.clone()))
        .ok_or_else(|| refused("CREATE without a name"))?;
    let (hash, switch, params) = source(model, request)?;
    // The user preset of this model and name, if any (stock compares names per model).
    let stored = |model: &StateModel<'_>| -> DeviceResult<Vec<tetrad_storage::stock::ModelPreset>> {
        let database = (model.state.library.as_ref())
            .and_then(|library| library.database.as_ref())
            .ok_or_else(|| refused("zenui.db is not open"))?;
        let presets = database.view().model_presets().map_err(refused)?;
        Ok((presets.into_iter())
            .filter(|preset| preset.model == i64::from(hash))
            .collect())
    };
    let named = |presets: Vec<tetrad_storage::stock::ModelPreset>| {
        (presets.into_iter().rev())
            .find(|preset| preset.name == name)
            .map(|preset| preset.id)
    };
    let action = match named(stored(model)?) {
        Some(preset) => {
            let preset = u64::try_from(preset).map_err(refused)?;
            edit(
                model,
                DevicePresetEdit::Overwrite {
                    preset,
                    switch,
                    params,
                },
            )?;
            MessageAction::Update
        }
        None => {
            if stored(model)?.len() >= PRESET_LIMIT || same_values(model, hash, &params)? {
                model.confirmation = Some(preset_conflict(hash));
                return Ok(());
            }
            let create = Action::CreateDevicePreset {
                model: hash,
                name: name.clone(),
                switch,
                params,
            };
            model.act(PRESETS, create)?;
            MessageAction::Create
        }
    };
    let Some(id) = named(stored(model)?) else {
        return Ok(());
    };
    if action == MessageAction::Create && first.and_then(|entry| entry.is_default) == Some(true) {
        let preset = u64::try_from(id).map_err(refused)?;
        let default = DevicePresetEdit::SetDefault {
            model: hash,
            preset,
            factory: false,
        };
        edit(model, default)?;
    }
    let entry = ModelPreset {
        id: Some(ModelPresetID {
            value: Some(id.to_string()),
            is_factory: Some(false),
            hash: Some(hash),
        }),
        name: Some(name),
        ..Default::default()
    };
    push(model, action, entry);
    Ok(())
}

/// `0x397ff8(model, 1)`: the "Preset Conflict" dialog sent to Cortex Control as a
/// Confirmation (47) through `0xfa48c` (one Ok action, "GOT IT"; both callbacks are empty,
/// `0x3970a4`, `0x3970bc`). `%1` is the kind of preset (`0x1f5b58`).
fn preset_conflict(hash: u32) -> ConfirmationRequest {
    let kind = match hash {
        GLOBAL_EQ_MODEL => "Global EQ Preset",
        IO_PRESET_MODEL => "I/O Settings Preset",
        _ => "Virtual Device Preset",
    };
    ConfirmationRequest {
        title: "Preset Conflict".into(),
        message: format!(
            "The {kind} could not be saved because its parameters are already stored in an \
             existing Preset."
        ),
        actions: vec![ConfirmationAction::Ok],
        ok_title: "GOT IT".into(),
        cancel_title: String::new(),
    }
}

/// The model, switch and rows a CREATE stores. The settings sources take the default switch
/// (as the device Virtual Device Presets, `ui::picker`).
fn source(
    model: &StateModel<'_>,
    request: &ModelPresetMessage,
) -> DeviceResult<(u32, ExpSwitch, Vec<ParamEntry>)> {
    let settings = |hash: u32, rows: Option<Vec<ParamEntry>>| {
        let rows = rows.ok_or_else(|| refused(format!("model {hash} is not in the catalog")))?;
        Ok((hash, default_switch(), rows))
    };
    match (request.create_from_hash).and_then(|hash| u32::try_from(hash).ok()) {
        Some(GLOBAL_EQ_MODEL) => settings(GLOBAL_EQ_MODEL, global_eq::preset_rows(model.state)),
        Some(IO_PRESET_MODEL) => settings(IO_PRESET_MODEL, global_eq::io_preset_rows(model.state)),
        _ => {
            let (row, column) = (request.create_from_row, request.create_from_column);
            let slot = (row.zip(column))
                .and_then(|(row, column)| grid_slot(row, column))
                .ok_or_else(|| refused("CREATE outside the grid"))?;
            let current =
                (model.state.current.as_ref()).ok_or_else(|| refused("no preset is loaded"))?;
            let scene = current.deployment.scene();
            let layout = current.deployment.layout();
            let live =
                (layout.instance(Site::Slot(slot))).ok_or_else(|| refused("no block there"))?;
            let (switch, params) = live_rows(current, slot, live, scene).map_err(refused)?;
            Ok((live.model.0, switch, params))
        }
    }
}

/// `0x1f7f50`: a user or factory preset of `hash` whose block has the values of `rows`. Both
/// sides become catalog blocks with their rows (`0x1f7e74`) and compare as the rows they
/// give at scene 0 (I: stock compares the cached block values; EXP data is not compared).
fn same_values(model: &StateModel<'_>, hash: u32, rows: &[ParamEntry]) -> DeviceResult<bool> {
    let specs = (model.state.specs.as_ref()).ok_or_else(|| refused("no catalog"))?;
    let scene = SceneIndex::new(0).expect("scene 0");
    let values = |rows: &[ParamEntry]| {
        preset::model_preset_block(specs, hash, None, rows).map(|block| {
            let (_, rows) = preset_rows(&block, None, scene);
            rows.into_iter().map(|row| row.value).collect::<Vec<_>>()
        })
    };
    let Some(new) = values(rows) else {
        return Ok(false);
    };
    let user = (model.state.library.as_ref()).and_then(|library| library.database.as_ref());
    for database in user.into_iter().chain(model.factory) {
        let view = database.view();
        for preset in view.model_presets().map_err(refused)? {
            if preset.model != i64::from(hash) {
                continue;
            }
            let stored = view.model_preset_params(preset.id).map_err(refused)?;
            if values(&stored).as_ref() == Some(&new) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn update_entry(model: &mut StateModel<'_>, entry: &ModelPreset) -> DeviceResult<()> {
    let id = entry.id.clone().unwrap_or_default();
    let factory = id.is_factory == Some(true);
    if let Some(name) = &entry.name {
        if factory {
            return Err(refused("factory model presets are read-only"));
        }
        let preset = preset_id(&id)?;
        edit(
            model,
            DevicePresetEdit::Rename {
                preset,
                name: name.clone(),
            },
        )?;
        push(
            model,
            MessageAction::Update,
            ModelPreset {
                id: Some(id.clone()),
                name: Some(name.clone()),
                ..Default::default()
            },
        );
    }
    if entry.is_default == Some(true) {
        let hash = id
            .hash
            .ok_or_else(|| refused("model preset without a hash"))?;
        edit(
            model,
            DevicePresetEdit::SetDefault {
                model: hash,
                preset: preset_id(&id)?,
                factory,
            },
        )?;
        push(
            model,
            MessageAction::Update,
            ModelPreset {
                id: Some(id),
                is_default: Some(true),
                ..Default::default()
            },
        );
    }
    Ok(())
}

fn edit(model: &mut StateModel<'_>, edit: DevicePresetEdit) -> DeviceResult<()> {
    model.act(PRESETS, Action::DevicePreset(edit))
}

/// The ModelPreset sender pushes (flags 0).
fn push(model: &mut StateModel<'_>, action: MessageAction, entry: ModelPreset) {
    let message = ModelPresetMessage {
        action: Some(action.into()),
        presets: vec![entry],
        ..Default::default()
    };
    model
        .effects
        .push(Effect::ControlNotify(Box::new(message.into())));
}

/// A grid slot from Control row and column values; `None` outside the grid.
fn grid_slot(row: i32, column: i32) -> Option<Slot> {
    let position = |value: i32| u8::try_from(value).ok();
    Some(Slot {
        row: Row::new(position(row)?).ok()?,
        column: Column::new(position(column)?).ok()?,
    })
}

/// DefaultParameters (19) CREATE `0x6684c8`: the block at row/column stores its current-scene
/// rows and EXP switch as its model's defaults (`REPLACE INTO default_param`,
/// `default_exp_switch`), the same write as "Set Parameters as Defaults". An empty slot or a
/// position outside the grid stores nothing; the receiver replies either way.
pub(super) fn save_defaults(model: &mut StateModel<'_>, row: i32, column: i32) -> DeviceResult<()> {
    let Some(slot) = grid_slot(row, column) else {
        return Ok(());
    };
    let Some(current) = model.state.current.as_ref() else {
        return Ok(());
    };
    let scene = current.deployment.scene();
    let Some(live) = current.deployment.layout().instance(Site::Slot(slot)) else {
        return Ok(());
    };
    let key = live.model.0.to_string();
    let (switch, params) = live_rows(current, slot, live, scene).map_err(refused)?;
    (model.effects).push(Effect::Action(Action::SetDefaultParams {
        key,
        switch,
        params,
    }));
    Ok(())
}

/// DefaultParameters UPDATE `0x15eb0c` → `0x66824c` → `0x15d74c`. A copy of the block at
/// row/column takes its model's `default_param` rows (`0x667acc` with `param_4` = 0: no
/// default-preset fallback), else the catalog defaults (`0x6678d4`: EXP code 0; a capture
/// keeps parameter 5). Then each XML parameter below 100 goes to the live block at the
/// current scene ([`grid::write_defaults`]); Looper X (27000) skips 0..=7. Stock drops the
/// copy, so its other scenes and its switch are not applied. Not modelled: the branch for a
/// block editor that shows the same block (`0x4bd4ec`) and the capture default key
/// (`0x66768c`). Stock reads a capture's rows by its own capture key, so they hold that key;
/// Tetrad keys by model id ([`save_defaults`]), so a capture always keeps its live parameter
/// 5, the capture key (review r19 F6). An empty slot does nothing.
pub(super) fn reset_defaults(
    model: &mut StateModel<'_>,
    row: i32,
    column: i32,
) -> DeviceResult<GridEcho> {
    let refused = |what: &str| ModelError::Refused(DEFAULTS, what.to_owned());
    let (Some(slot), Some(current)) = (grid_slot(row, column), model.state.current.as_ref()) else {
        return Ok(GridEcho::default());
    };
    let scene = current.deployment.scene();
    let Some(live) = current.deployment.layout().instance(Site::Slot(slot)) else {
        return Ok(GridEcho::default());
    };
    let id = live.model.0;
    let specs = (model.state.specs.as_ref()).ok_or_else(|| refused("no catalog"))?;
    let spec = specs
        .model(id)
        .ok_or_else(|| refused("the block is not in the catalog"))?;
    let rows = match (model.state.library.as_ref()).and_then(|l| l.database.as_ref()) {
        Some(database) => (database.view().default_params(&id.to_string()))
            .map_err(|error| refused(&error.to_string()))?,
        None => Vec::new(),
    };
    let target = if rows.is_empty() {
        tetrad_engine::new_instance(spec)
    } else {
        preset::model_preset_block(specs, id, Some(live), &rows)
            .ok_or_else(|| refused("the block is not in the catalog"))?
    };
    let capture = preset::CAPTURE_MODELS.contains(&id);
    let (_, entries) = preset_rows(&target, None, scene);
    let values = (spec.parameters.iter().zip(entries))
        .filter(|(param, _)| param.index < 100 && !(id == LOOPER_X && param.index < 8))
        .filter(|(param, _)| !(capture && param.index == 5))
        .map(|(param, entry)| {
            let value = match param.kind {
                ParameterType::String => ParamValue::String(entry.value),
                _ => ParamValue::Float(entry.value.parse().unwrap_or_default()),
            };
            let link = (rows.iter())
                .find(|row| row.index == i64::from(param.index) && row.exp != 0)
                .map(|row| {
                    (
                        row.exp as i32,
                        row.exp_trim_min as f32,
                        row.exp_trim_max as f32,
                    )
                });
            DefaultValue {
                index: param.index,
                value,
                link,
            }
        })
        .collect();
    grid::write_defaults(model, slot, values)
}

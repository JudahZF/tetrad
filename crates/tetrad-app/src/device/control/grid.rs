//! Grid (1) UPDATE and DELETE (`0x257408`) and GridMove (12, `0x26411c`) from Cortex Control.
//!
//! - One UPDATE reaches one sub-handler, picked by the first non-empty field of its
//!   BinaryPreset; a chain picks again by its first non-empty field.
//! - Edits stock records an undo frame for run in `StateModel::undoable`: value edits and
//!   the junction bypass through the 500 ms timer `0x17004c` ([`UndoPath::ControlTimer`]),
//!   structural edits, ports, split and mix, block bypass, expression links and switches
//!   directly (`0x648f1c`, [`UndoPath::Direct`]). The frame is kept only when the edit
//!   commits. Tags, tempo, stomp, notify and scene-mode edits record none, as in stock.
//! - Owned: a non-finite parameter value refuses the whole UPDATE.
//! - Echoes follow the stock senders: lane, input, bypass, junction, stomp and notify
//!   pushes go out inside the receiver; block and junction parameter values wait 100 ms
//!   (`0x25b248` suspends the Grid sender around those edits).
//! - Owned: while a capture preset runs stock edits that preset (`+0x758`); tetrad-app holds
//!   only the user preset, so every Grid edit is refused.

use tetrad_control::codecs::{
    ControlMessage, CortexMessageType, GridMessage, GridMoveElement, GridMoveMessage,
    MessageAction, ModelPresetID, RawMessageBytes,
};
use tetrad_control::device::{DelayedGrid, DeviceError, DeviceResult, GridEcho, GridMoved};
use tetrad_core::performance::expression::{BypassInfo, BypassSwitchType, Pedal};
use tetrad_engine::{Column, DropSide, InputPort, OutputPort, Row, Site, Slot};
use tetrad_preset::{
    BypassRow, Chain, ColBypass, ExpressionBypassInfo, MapField, Model, Param, ParamValue, Preset,
    SceneBypass, SceneValue, SplitControlPoints, StompAssignment, StompRole,
};

use super::StateModel;
use super::status::nonzero;
use crate::device::actions::{self, Action, LayoutEdit};
use crate::device::preset::{self, PresetEdit, UndoPath};
use crate::device::services::PluginStatus;
use crate::device::{Effect, State};

const GRID: CortexMessageType = CortexMessageType::Grid;
/// Pair junction models (`0x17b084`): the mixer and the combined splitter.
const MIXER: u32 = 11_000;
const COMBINED_SPLITTER: u32 = 10_004;
/// `0x5227c4`: stomps 0..=10 take a momentary flag.
const MOMENTARY_STOMPS: u32 = 11;
/// `0x254ed0`: assignments to stomps 0..=7.
const ASSIGNABLE_STOMPS: u32 = 8;
/// `0x514598`: a stomp holding a Looper X is never momentary.
const LOOPER_X: u32 = 27_000;
/// `0x5227c4`: category 26 model 26001 follows the momentary flag in parameter 1.
const MOMENTARY_MODEL: u32 = 26_001;
/// `0x5225c8`: category 27 blocks take no MUTE edit (`0x1723bc` gives them no MUTE switch).
const MUTELESS_CATEGORY: u32 = 27;
/// `0xa14260`: the ModelPresetID of a model's catalog defaults (`0x1f7b10` → `0x6678d4`).
pub(super) const SPECIAL_FACTORY_PRESET: &str = "SpecialFactoryModelPresetID";

fn refused(what: impl Into<String>) -> DeviceError {
    DeviceError::Refused(GRID, what.into())
}

fn decode(bytes: Option<&RawMessageBytes>) -> DeviceResult<Preset> {
    let bytes = bytes.map_or(&[][..], |raw| raw.bytes.as_slice());
    Preset::decode(bytes, tetrad_preset::Limits::default())
        .map_err(|error| refused(error.to_string()))
}

fn guard(model: &StateModel<'_>) -> DeviceResult<()> {
    if model.capture_running {
        return Err(refused("a grid edit while a capture preset runs"));
    }
    Ok(())
}

fn row_of(raw: Option<u32>) -> Option<Row> {
    raw.and_then(|raw| u8::try_from(raw).ok())
        .and_then(|raw| Row::new(raw).ok())
}

fn column_of(raw: Option<u32>) -> Option<Column> {
    raw.and_then(|raw| u8::try_from(raw).ok())
        .and_then(|raw| Column::new(raw).ok())
}

/// The first value of a parameter message.
fn first_value(param: &Param) -> Option<&ParamValue> {
    param.values.first()?.value.as_ref()
}

fn has_switch(model: &Model) -> bool {
    !model.bypass_expression.is_empty() || !model.expression_bypass_info.is_empty()
}

/// `0x257408` UPDATE with a preset.
pub(super) fn update(model: &mut StateModel<'_>, message: &GridMessage) -> DeviceResult<GridEcho> {
    guard(model)?;
    let preset = decode(message.preset.as_ref())?;
    let non_finite = |value: &SceneValue| matches!(value.value, Some(ParamValue::Float(value)) if !value.is_finite());
    if preset::param_values(&preset).any(non_finite) {
        return Err(refused("a non-finite parameter value"));
    }
    let mut echo = GridEcho::default();
    if let Some(chain) = preset.chains.first() {
        chain_update(model, message, &preset, chain, &mut echo)?;
    } else if let Some(record) = preset.bypass.first() {
        block_bypass(model, record, &mut echo)?;
    } else if !preset.tags.is_empty() {
        model.act(
            GRID,
            Action::PresetEdit(PresetEdit::Tags(preset.tags.clone())),
        )?;
    } else if let Some(tempo) = preset.tempo_program_data.first() {
        tempo_update(model, tempo, &mut echo)?;
    } else if !preset.stomp_mode_assignments.is_empty() {
        stomp_assignments(model, &preset.stomp_mode_assignments, Some(&mut echo))?;
    } else if !preset.stomp_labels.is_empty() {
        // `0x254d10` → `0x65d37c(.., 1)` per entry; the echo is `0x25fdbc`.
        for (stomp, label) in preset.stomp_labels.iter() {
            let label = label.clone();
            let edit = PresetEdit::StompLabel {
                stomp,
                label: label.clone(),
            };
            model.act(GRID, Action::PresetEdit(edit))?;
            let mut stomp_labels = MapField::default();
            stomp_labels.insert(stomp, label);
            echo.now.push(grid_update(Preset {
                stomp_labels,
                ..Default::default()
            }));
        }
    } else if !preset.single_stomp_labels.is_empty() {
        // `0x254d60` → `0x65d37c(.., 0)`; the echo is `0x260130`.
        for (stomp, label) in preset.single_stomp_labels.iter() {
            let label = label.clone();
            let action = Action::StompLabel {
                stomp,
                label: label.clone(),
            };
            model.act(GRID, action)?;
            let mut single_stomp_labels = MapField::default();
            single_stomp_labels.insert(stomp, label);
            echo.now.push(grid_update(Preset {
                single_stomp_labels,
                ..Default::default()
            }));
        }
    } else {
        // `0x254db0` → `0x65d4b8` → `0x5227c4` (not on ATMA); the echo is `0x26055c`.
        for (stomp, &momentary) in preset.stomp_is_momentary.iter() {
            if stomp >= MOMENTARY_STOMPS || model.state.platform.is_atma() {
                continue;
            }
            let momentary = momentary_blocks(model, stomp, momentary, &mut echo)?;
            let edit = PresetEdit::Momentary { stomp, momentary };
            model.act(GRID, Action::PresetEdit(edit))?;
            let mut stomp_is_momentary = MapField::default();
            stomp_is_momentary.insert(stomp, momentary);
            echo.now.push(grid_update(Preset {
                stomp_is_momentary,
                ..Default::default()
            }));
        }
    }
    Ok(echo)
}

/// `0x257408` DELETE with a preset: a chain removes its first block (`0x1724b4`), else the
/// stomp assignments go (`0x254ed0(.., 0)`). No echo.
pub(super) fn delete(model: &mut StateModel<'_>, message: &GridMessage) -> DeviceResult<()> {
    guard(model)?;
    let preset = decode(message.preset.as_ref())?;
    let Some(chain) = preset.chains.first() else {
        return stomp_assignments(model, &preset.stomp_mode_assignments, None);
    };
    let row = row_of(chain.row);
    let column = chain
        .models
        .first()
        .and_then(|block| column_of(block.column));
    let (Some(row), Some(column)) = (row, column) else {
        return Ok(());
    };
    let slot = Slot { row, column };
    if stored_block(model, slot).is_none() {
        return Ok(());
    }
    model.undoable(UndoPath::Direct, |model| {
        model.act(GRID, Action::EditLayout(LayoutEdit::Remove(slot)))
    })
}

/// GridMove `0x26411c`: the check `0x26321c` (every hash of `grid` matches the slot, every
/// source slot holds a block, one move is the drop, the other moves stay in their row),
/// then the first drop moves the block (`0x5beb78` → the row view's drop signal). The other
/// moves are drag previews: `0x5beca8(row view, from_col, to_col)` marks where a block of the
/// drop's row will go (C); the drop makes room the way they point ([`preview_side`]). A
/// refused check sends the current preset (`0x2778d4(.., 0)`); a drop sends no RecallPreset
/// (no caller of `0x277468` under `Chain::dropSlot` `0x59f620`, C). The B5 bug: Tetrad sent the
/// working preset after each drop, and its `default_scene` (the stored scene, which a scene
/// change does not touch, as stock `+0x1520`) moved Cortex Control off the playing scene. A
/// drop leaves its GridMove echo in `model.drop_echo` ([`DropEcho`]).
pub(super) fn move_grid(
    model: &mut StateModel<'_>,
    message: &GridMoveMessage,
) -> DeviceResult<GridMoved> {
    guard(model)?;
    let slot = |row: Option<u32>, column: Option<u32>| {
        Some(Slot {
            row: row_of(Some(row.unwrap_or(0)))?,
            column: column_of(Some(column.unwrap_or(0)))?,
        })
    };
    let hash = |model: &StateModel<'_>, slot: Slot| block_hash(model.state, slot);
    let grid_matches = message.grid.iter().all(|grid| {
        grid.rows.iter().enumerate().all(|(row, ids)| {
            ids.model_ids.iter().enumerate().all(|(column, &id)| {
                let at = slot(u32::try_from(row).ok(), u32::try_from(column).ok());
                at.is_some_and(|at| hash(model, at) == id)
            })
        })
    });
    let moves = &message.moves;
    let drop = moves.iter().find(|step| step.is_drop.unwrap_or(false));
    let valid = grid_matches
        && drop.is_some()
        && moves.iter().all(|step| {
            slot(step.from_row, step.from_col).is_some_and(|from| hash(model, from) != 0)
                && (step.is_drop.unwrap_or(false) || step.from_row == step.to_row)
        });
    let Some(drop) = drop.filter(|_| valid) else {
        return Ok(GridMoved::Refused);
    };
    let (Some(from), Some(to)) = (
        slot(drop.from_row, drop.from_col),
        slot(drop.to_row, drop.to_col),
    ) else {
        return Ok(GridMoved::Refused);
    };
    let side = preview_side(moves);
    let echo = DropEcho::plan(model.state, from, to, side);
    model.undoable(UndoPath::Direct, |model| {
        model.act(GRID, Action::DropBlock { from, to, side })
    })?;
    model.drop_echo = echo;
    Ok(GridMoved::Dropped)
}

/// The half of the target a Control drop takes ([`DropSide`]): the first preview that moves
/// a block right makes room to the right (the left half, `0x5c09c8`), one that moves it left
/// makes room to the left (the right half, `0x5c0ee0`). I: stock drops onto the previews the
/// request set; with none the left half (an empty target needs no room).
fn preview_side(moves: &[GridMoveElement]) -> DropSide {
    let column = |column: Option<u32>| column.unwrap_or(0);
    (moves.iter())
        .filter(|step| !step.is_drop.unwrap_or(false))
        .find_map(
            |step| match column(step.to_col).cmp(&column(step.from_col)) {
                std::cmp::Ordering::Greater => Some(DropSide::Left),
                std::cmp::Ordering::Less => Some(DropSide::Right),
                std::cmp::Ordering::Equal => None,
            },
        )
        .unwrap_or_default()
}

/// The GridMove (12) echo of a drop, on the touch screen or from Cortex Control:
/// `Chain::dropSlot` `0x59f620` records the undo frame (`0x648f1c`), then `0x59ea40` →
/// `0x59e194` builds the move list and `0x266290` sends it (C): `moves` only, no action, no
/// grid, request ID 0 (Tetrad: the request's ID inside a Control request, as every push there,
/// `0x6e01d8`). Cortex Control draws the move only when the echo arrives (QC log
/// `cc-p-stock-gm-tail.log`: UndoRedo, GridMove echo, PresetDirty). [`super::Control::sync`]
/// sends it after the frame lists, and only when the move is live ([`Self::echo`]).
#[derive(Debug, Clone, PartialEq)]
pub(in crate::device) struct DropEcho {
    moves: Vec<GridMoveElement>,
    from: Slot,
    to: Slot,
    /// The hash of the dropped block.
    moved: u32,
}

impl DropEcho {
    /// The move list of a drop of the block at `from` on the `side` half of `to`, from the
    /// layout before the drop (C, `0x59e194`): each block the drop shifts in the target row
    /// (from its old to its new column, `is_drop` unset), then the drop (from the source
    /// slot to the target, `is_drop` set). The shifted blocks go from the far end: highest
    /// new column first when they move right, lowest first when they move left. `None` for a
    /// drop stock refuses.
    pub(in crate::device) fn plan(
        state: &State,
        from: Slot,
        to: Slot,
        side: DropSide,
    ) -> Option<Self> {
        let current = state.current.as_ref()?;
        let mut moves = current.deployment.layout().plan_drop(from, to, side)?.moves;
        let drop = moves.pop()?;
        if (moves.first()).is_some_and(|(old, new)| new.column.index() > old.column.index()) {
            moves.reverse();
        }
        let element = |(from, to): (Slot, Slot), is_drop: bool| GridMoveElement {
            from_row: nonzero(from.row.index() as u32),
            from_col: nonzero(from.column.index() as u32),
            to_row: nonzero(to.row.index() as u32),
            to_col: nonzero(to.column.index() as u32),
            is_drop: is_drop.then_some(true),
        };
        Some(Self {
            moves: (moves.into_iter().map(|shift| element(shift, false)))
                .chain([element(drop, true)])
                .collect(),
            from,
            to,
            moved: block_hash(state, from),
        })
    }

    /// The echo when the working preset holds the dropped block at `to` and no longer at
    /// `from` (O: a deploy that failed kept the old preset, so Control keeps its old grid).
    pub(super) fn echo(self, state: &State) -> Option<GridMoveMessage> {
        let live = block_hash(state, self.to) == self.moved
            && (self.from == self.to || block_hash(state, self.from) != self.moved);
        live.then(|| GridMoveMessage {
            moves: self.moves,
            ..Default::default()
        })
    }
}

fn stored_block<'s>(model: &'s StateModel<'_>, slot: Slot) -> Option<&'s Model> {
    state_block(model.state, slot)
}

fn state_block(state: &State, slot: Slot) -> Option<&Model> {
    let at = tetrad_preset::Slot {
        row: slot.row.index(),
        column: slot.column.index(),
    };
    state.current.as_ref()?.preset.block(at).ok().flatten()
}

/// The stored block hash at `slot`; 0 for an empty slot.
fn block_hash(state: &State, slot: Slot) -> u32 {
    state_block(state, slot).map_or(0, |block| block.hash.unwrap_or(0))
}

/// `0x25657c`: the first chain, rows 0..=3.
fn chain_update(
    model: &mut StateModel<'_>,
    message: &GridMessage,
    preset: &Preset,
    chain: &Chain,
    echo: &mut GridEcho,
) -> DeviceResult<()> {
    let Some(row) = row_of(chain.row) else {
        return Ok(());
    };
    if let Some(block) = chain.models.first() {
        return block_update(model, message, preset, row, block, echo);
    }
    if chain.in_portid.is_some() || chain.out_portid.is_some() {
        return ports(model, row, chain);
    }
    if let Some(lane) = chain.output_control.first() {
        return lane_update(model, row, lane, echo);
    }
    if let Some(input) = chain.input_control.first() {
        return input_update(model, row, input, echo);
    }
    // Junctions sit on the master rows 0 and 2.
    if row.index() % 2 == 1 {
        return Ok(());
    }
    let pair = row.pair();
    if let Some(points) = chain.split_control_points.first() {
        return junction_points(model, row, points, echo);
    }
    if let Some(bypass) = chain.split_bypass.first() {
        return junction_bypass(model, row, bypass.bypass, echo);
    }
    if let Some(mixer) = chain.mixer.first() {
        if let Some(param) = mixer.params.first() {
            return junction_param(model, Site::Mixer(pair), param, echo);
        }
        if has_switch(mixer) {
            // `0x17b46c`: the mixer bypass (its last parameter, `0x2ecc70`).
            return site_switch(model, Site::Mixer(pair), None, mixer);
        }
        return Ok(());
    }
    if let Some(param) = chain
        .combined_splitter
        .first()
        .and_then(|m| m.params.first())
    {
        return junction_param(model, Site::Splitter(pair), param, echo);
    }
    Ok(())
}

/// `0x1783b0`: a block model with a column 0..=7.
fn block_update(
    model: &mut StateModel<'_>,
    message: &GridMessage,
    preset: &Preset,
    row: Row,
    block: &Model,
    echo: &mut GridEcho,
) -> DeviceResult<()> {
    let Some(column) = column_of(block.column) else {
        return Ok(());
    };
    let slot = Slot { row, column };
    if let Some(hash) = block.hash {
        return set_block(model, message, preset, slot, hash, block);
    }
    if let Some(param) = block.params.first() {
        return block_param(model, slot, param, echo);
    }
    if has_switch(block) {
        return expression_switch(model, slot, block);
    }
    if block.notify_flag == Some(true) {
        return notify(model, slot, echo);
    }
    Ok(())
}

/// `0x17422c` → `0x172f24`: a model without parameters takes the catalog defaults (add,
/// change or reset); one with parameters loads like a stored block with its `ColBypass`.
/// Stock adds through the row view; no echo was found on that path.
fn set_block(
    model: &mut StateModel<'_>,
    message: &GridMessage,
    preset: &Preset,
    slot: Slot,
    hash: u32,
    block: &Model,
) -> DeviceResult<()> {
    if let Some(id) = &message.model_preset_to_load {
        return set_model_preset(model, slot, hash, id);
    }
    let edit = if block.params.is_empty() {
        default_block(model, slot, hash)
    } else {
        let bypass = preset
            .bypass
            .first()
            .and_then(|row| row.col_bypass.first())
            .cloned();
        let state = &*model.state;
        let (Some(specs), Some(current)) = (&state.specs, &*state.current) else {
            return Err(refused("no preset is loaded"));
        };
        let instance = preset::imported_block(current, specs, slot, block.clone(), bypass)
            .map_err(|error| refused(error.to_string()))?;
        LayoutEdit::Paste {
            slot,
            block: Box::new(instance),
        }
    };
    model.undoable(UndoPath::Direct, |model| {
        model.act(GRID, Action::EditLayout(edit))
    })
}

/// The catalog block of `hash` at `slot`: add, reset (same model) or change.
fn default_block(model: &StateModel<'_>, slot: Slot, hash: u32) -> LayoutEdit {
    match stored_block(model, slot).and_then(|stored| stored.hash) {
        None => LayoutEdit::Add { slot, model: hash },
        Some(stored) if stored == hash => LayoutEdit::Reset(slot),
        Some(_) => LayoutEdit::Replace { slot, model: hash },
    }
}

/// `0x17422c` with a ModelPresetID (`0x1cf838`: id text, factory flag, model hash): the live
/// block of that model, else a fresh one (`0x1f7e74`), takes the preset rows (`0x1f7b10`,
/// database by the factory flag, `0x1f78f4`) and loads like a stored block. The preset's EXP
/// links, ranges and switch are stored once the paste is committed (a guarded write). The
/// special id is the catalog block (`0x6678d4`).
fn set_model_preset(
    model: &mut StateModel<'_>,
    slot: Slot,
    hash: u32,
    id: &ModelPresetID,
) -> DeviceResult<()> {
    let hash = id.hash.unwrap_or(hash);
    let value = id.value.as_deref().unwrap_or_default();
    if value == SPECIAL_FACTORY_PRESET {
        let edit = default_block(model, slot, hash);
        return model.undoable(UndoPath::Direct, |model| {
            model.act(GRID, Action::EditLayout(edit))
        });
    }
    let database = if id.is_factory == Some(true) {
        model.factory
    } else {
        let library = model.state.library.as_ref();
        library.and_then(|library| library.database.as_ref())
    };
    let database = database.ok_or_else(|| refused("the preset database is not open"))?;
    let row = value
        .parse::<i64>()
        .map_err(|_| refused(format!("Virtual Device Preset id {value:?}")))?;
    let view = database.view();
    let read = |error: tetrad_storage::Error| refused(error.to_string());
    let switch = view.model_preset(row).map_err(read)?;
    let switch = switch.ok_or_else(|| refused("an unknown Virtual Device Preset"))?;
    let rows = view.model_preset_params(row).map_err(read)?;
    let state = &*model.state;
    let (Some(specs), Some(current)) = (&state.specs, &*state.current) else {
        return Err(refused("no preset is loaded"));
    };
    let live = current.deployment.layout().instance(Site::Slot(slot));
    let block = preset::model_preset_block(specs, hash, live, &rows)
        .ok_or_else(|| refused(format!("model {hash} is not in the catalog")))?;
    let rejected = preset::rejected_model_preset_rows(specs, hash, &rows);
    if rejected > 0 {
        model.effects.push(Effect::Notice(
            crate::device::Subsystem::Library,
            format!("Virtual Device Preset {row}: {rejected} invalid values left out"),
        ));
    }
    let (mut stored, bypass) = preset::pasted_record(current, specs, slot, &block)
        .map_err(|error| refused(error.to_string()))?;
    let parameters = specs.model(hash).map_or(0, |spec| spec.parameters.len());
    preset::model_preset_expression(&mut stored, parameters, &rows, switch.exp_switch)
        .map_err(|error| refused(error.to_string()))?;
    let paste = LayoutEdit::Paste {
        slot,
        block: Box::new(block),
    };
    model.undoable(UndoPath::Direct, |model| {
        model.act(GRID, Action::EditLayout(paste))
    })?;
    model.effects.push(Effect::Action(Action::RestoreBlock {
        slot,
        model: Box::new(stored),
        bypass: Box::new(bypass),
    }));
    Ok(())
}

/// The loaded preset's scene and the stored value of one parameter in it.
fn stored_value(model: &StateModel<'_>, site: Site, index: u32) -> Option<ParamValue> {
    let current = model.state.current.as_ref()?;
    let scene = current.deployment.scene().index();
    preset::stored_value(&current.preset, site, index, scene).cloned()
}

fn as_number(value: &ParamValue) -> Option<f32> {
    match *value {
        ParamValue::Float(value) => Some(value),
        ParamValue::Int(value) => Some(value as f32),
        ParamValue::String(_) => None,
    }
}

/// One value edit through the shared actions. Inferred: an integer value is stored as a
/// float, as the UI stores every number.
fn set_value(
    model: &mut StateModel<'_>,
    site: Site,
    index: u32,
    value: ParamValue,
) -> DeviceResult<()> {
    let action = match value {
        ParamValue::String(text) => Action::SetText { site, index, text },
        number => Action::SetParameter {
            site,
            index,
            value: as_number(&number).unwrap_or_default(),
        },
    };
    model.act(GRID, action)
}

/// `0x1727a0` on a grid block: a value (unchanged values do nothing; the kind must match,
/// `0x16ffb4`), else an expression link (`0x171b00`), else the scene mode (`0x1700b8`).
fn block_param(
    model: &mut StateModel<'_>,
    slot: Slot,
    param: &Param,
    echo: &mut GridEcho,
) -> DeviceResult<()> {
    let Some(index) = param.index else {
        return Ok(());
    };
    let site = Site::Slot(slot);
    if let Some(value) = first_value(param) {
        // `0x16ff3c`: the index must be one of the block's parameters.
        let Some(stored) = stored_value(model, site, index) else {
            return Ok(());
        };
        let mute = model
            .state
            .current
            .as_ref()
            .and_then(|current| current.deployment.layout().instance(site))
            .is_some_and(|block| block.params.len() == index as usize);
        let text = |value: &ParamValue| matches!(value, ParamValue::String(_));
        let unchanged = match (as_number(&stored), as_number(value)) {
            (Some(stored), Some(value)) => stored == value,
            _ => stored == *value,
        };
        if text(&stored) != text(value) || unchanged {
            return Ok(());
        }
        // `0x172868`: the timer frame comes before the MUTE test (`0x2ec444`).
        return model.undoable(UndoPath::ControlTimer, |model| {
            if mute {
                let muted = as_number(value).is_some_and(|value| value > 0.5);
                return block_mute(model, slot, index, muted, echo);
            }
            let run = model
                .state
                .current
                .as_ref()
                .map(|current| preset::step_run(current, site, index))
                .unwrap_or_default();
            for index in std::iter::once(index).chain(run) {
                set_value(model, site, index, value.clone())?;
                if let Some(value) = stored_value(model, site, index) {
                    echo.delayed.push(block_value_echo(slot, index, value));
                }
            }
            Ok(())
        });
    }
    if let Some(code) = param.expression {
        return expression_link(model, slot, index, code, param);
    }
    if let Some(enabled) = param.scene_mode {
        return scene_mode(model, site, index, enabled, echo);
    }
    Ok(())
}

/// `0x1727a0` at the MUTE index (`0x2ec444`): a value that differs from MUTE in scene 0
/// (`0x5133c0`) toggles it (`0x5225c8` → `0x521dbc` → `0x51e4d8`). `0x5225c8` does nothing,
/// echo included, on category 27 (`0x2ea1c4`) and on a licence-locked block (`0x2e02d0` →
/// block `+0xfc`, set by `0x521c84` when a plugin block deploys forced-bypassed): a ModelRepo
/// `plugin_id` whose licence is not active, the rule of the deploy and the save. The echo is
/// MUTE at the current scene, at once.
fn block_mute(
    model: &mut StateModel<'_>,
    slot: Slot,
    index: u32,
    muted: bool,
    echo: &mut GridEcho,
) -> DeviceResult<()> {
    let site = Site::Slot(slot);
    let state = &*model.state;
    let spec = state
        .current
        .as_ref()
        .and_then(|current| current.deployment.layout().instance(site))
        .zip(state.specs.as_ref())
        .and_then(|(block, specs)| specs.model(block.model.0));
    let muteless = spec.is_some_and(|spec| spec.category_id == MUTELESS_CATEGORY);
    let locked = (spec.and_then(|spec| spec.plugin_id))
        .is_some_and(|plugin| state.licences.status(plugin) != PluginStatus::Active);
    let first = state
        .current
        .as_ref()
        .and_then(|current| preset::stored_parameter(&current.preset, site, index, 0));
    let was = first.is_some_and(|value| value > 0.5);
    if muteless || locked || was == muted {
        return Ok(());
    }
    let value = f32::from(u8::from(!was));
    model.act(GRID, Action::SetParameter { site, index, value })?;
    if let Some(value) = stored_value(model, site, index) {
        echo.now
            .push(grid_update(site_preset(site, one_param(index, value))));
    }
    Ok(())
}

/// `0x171b00` → `0x170110`: the link code (0 none, 1 EXP1, else EXP2) and, when both are
/// set, the range. An unchanged code without a range records nothing.
fn expression_link(
    model: &mut StateModel<'_>,
    slot: Slot,
    index: u32,
    code: i32,
    param: &Param,
) -> DeviceResult<()> {
    let stored = stored_block(model, slot)
        .and_then(|block| block.params.get(index as usize))
        .and_then(|param| param.expression)
        .unwrap_or(0);
    let range = param.expression_min.zip(param.expression_max);
    if stored == code && range.is_none() {
        return Ok(());
    }
    // `0x171cb4`: a direct frame.
    model.undoable(UndoPath::Direct, |model| {
        let linked = Pedal::from_assignment(code);
        if let Some(pedal) = linked.or(Pedal::from_assignment(stored)) {
            let action = Action::LinkExpression {
                slot,
                parameter: Some(index),
                pedal,
                linked: linked.is_some(),
            };
            model.act(GRID, action)?;
        }
        if let Some((min, max)) = range {
            let edit = PresetEdit::ExpressionRange {
                slot,
                index,
                min,
                max,
            };
            model.act(GRID, Action::PresetEdit(edit))?;
        }
        Ok(())
    })
}

/// The block value echo `0x25d51c`, held 100 ms.
fn block_value_echo(slot: Slot, index: u32, value: ParamValue) -> DelayedGrid {
    let block = Model {
        column: Some(slot.column.index() as u32),
        params: vec![one_param(index, value)],
        ..Default::default()
    };
    DelayedGrid {
        target: (slot.row.index() as u32, slot.column.index() as u32, index),
        message: grid_message(chain_preset(
            slot.row,
            Chain {
                models: vec![block],
                ..Default::default()
            },
        )),
    }
}

/// One parameter of a DefaultParameters reset: its value at the current scene and the EXP
/// link of its default row (code, min, max), when the code is not 0.
pub(super) struct DefaultValue {
    pub index: u32,
    pub value: ParamValue,
    pub link: Option<(i32, f32, f32)>,
}

/// The writes of `0x15d74c` on the block at `slot`, in parameter order: the EXP link first
/// (`0x15c910`, as [`expression_link`]), then the value through `0x523644`. On model 26001
/// with a PRIMARY stomp (`0x2e1f20`), parameter 1 sets that stomp's momentary flag
/// (`0x5227c4`: [`momentary_blocks`], no `0x26055c` echo); every other value is written and
/// echoed (`0x51e4d8`, `0x25d51c`), unchanged ones too. The values open no undo frame (as
/// stock); a link keeps the direct frame of [`expression_link`] (I).
pub(super) fn write_defaults(
    model: &mut StateModel<'_>,
    slot: Slot,
    values: Vec<DefaultValue>,
) -> DeviceResult<GridEcho> {
    let mut echo = GridEcho::default();
    let site = Site::Slot(slot);
    let (row, column) = (slot.row.index() as u32, slot.column.index() as u32);
    let primary = model.state.current.as_ref().and_then(|current| {
        let assigned = current.preset.stomp_mode_assignments.iter();
        assigned
            .filter(|entry| (entry.row, entry.column) == (row, column))
            .find(|entry| entry.role.unwrap_or(StompRole::Primary) == StompRole::Primary)
            .map(|entry| entry.stomp_index)
    });
    let momentary_block = (model.state.current.as_ref())
        .and_then(|current| current.deployment.layout().instance(site))
        .is_some_and(|block| block.model.0 == MOMENTARY_MODEL);
    for DefaultValue { index, value, link } in values {
        if let Some((code, min, max)) = link {
            let param = Param {
                expression_min: Some(min),
                expression_max: Some(max),
                ..Default::default()
            };
            expression_link(model, slot, index, code, &param)?;
        }
        match primary {
            Some(stomp) if momentary_block && index == 1 && stomp < MOMENTARY_STOMPS => {
                let pressed = as_number(&value).is_some_and(|value| value > 0.5);
                let momentary = momentary_blocks(model, stomp, pressed, &mut echo)?;
                let edit = PresetEdit::Momentary { stomp, momentary };
                model.act(GRID, Action::PresetEdit(edit))?;
            }
            _ => {
                set_value(model, site, index, value)?;
                if let Some(value) = stored_value(model, site, index) {
                    echo.delayed.push(block_value_echo(slot, index, value));
                }
            }
        }
    }
    Ok(echo)
}

/// `0x5227c4` before the flag is stored: a stomp with a Looper X block (`0x514598`) is never
/// momentary, and each assigned model 26001 takes the flag as parameter 1 (`0x51e4d8`, echo
/// `0x25d51c`). Returns the flag to store.
fn momentary_blocks(
    model: &mut StateModel<'_>,
    stomp: u32,
    momentary: bool,
    echo: &mut GridEcho,
) -> DeviceResult<bool> {
    let Some(current) = model.state.current.as_ref() else {
        return Ok(momentary);
    };
    let layout = current.deployment.layout();
    let blocks: Vec<(Slot, u32)> = actions::stomp_slots(&current.preset, stomp)
        .filter_map(|slot| Some((slot, layout.instance(Site::Slot(slot))?.model.0)))
        .collect();
    let momentary = momentary && !blocks.iter().any(|&(_, id)| id == LOOPER_X);
    for (slot, _) in blocks.into_iter().filter(|&(_, id)| id == MOMENTARY_MODEL) {
        let site = Site::Slot(slot);
        let value = if momentary { 1.0 } else { 0.0 };
        model.act(
            GRID,
            Action::SetParameter {
                site,
                index: 1,
                value,
            },
        )?;
        if let Some(value) = stored_value(model, site, 1) {
            echo.delayed.push(block_value_echo(slot, 1, value));
        }
    }
    Ok(momentary)
}

/// `0x171b00` → `0x170110` on a lane, input gate or pair junction parameter: the link code
/// and, when both are set, the range (else the stored one), after a direct undo frame
/// (`0x171cb4`). An unchanged code without a range records nothing. No echo.
fn site_link(
    model: &mut StateModel<'_>,
    site: Site,
    index: u32,
    code: i32,
    param: &Param,
) -> DeviceResult<()> {
    let current = model.state.current.as_ref();
    let Some(stored) =
        current.and_then(|current| preset::stored_param(&current.preset, site, index))
    else {
        return Ok(());
    };
    let range = param.expression_min.zip(param.expression_max);
    if stored.expression.unwrap_or(0) == code && range.is_none() {
        return Ok(());
    }
    let edit = PresetEdit::SiteExpression {
        site,
        index,
        code,
        range,
    };
    model.undoable(UndoPath::Direct, |model| {
        model.act(GRID, Action::PresetEdit(edit))
    })
}

/// `0x1723bc` → `0x171e78`: the bypass link (`bypass_expression`) and the pedal switch
/// (`expression_bypass_info`) after a direct undo frame (`0x17212c`). An unchanged link
/// without a switch records nothing.
fn expression_switch(model: &mut StateModel<'_>, slot: Slot, block: &Model) -> DeviceResult<()> {
    let stored = stored_block(model, slot)
        .and_then(|block| block.bypass_expression.first())
        .map_or(0, |link| link.expression);
    let link = block.bypass_expression.first().map(|link| link.expression);
    let info = block.expression_bypass_info.first();
    if link.is_none_or(|code| code == stored) && info.is_none() {
        return Ok(());
    }
    let info = info.map(switch_info).transpose()?;
    model.undoable(UndoPath::Direct, |model| {
        if let Some(code) = link {
            let linked = Pedal::from_assignment(code);
            if let Some(pedal) = linked.or(Pedal::from_assignment(stored)) {
                let action = Action::LinkExpression {
                    slot,
                    parameter: None,
                    pedal,
                    linked: linked.is_some(),
                };
                model.act(GRID, action)?;
            }
        }
        if let Some(info) = info {
            model.act(GRID, Action::ExpressionSwitch { slot, info })?;
        }
        Ok(())
    })
}

fn switch_info(info: &ExpressionBypassInfo) -> DeviceResult<BypassInfo> {
    let kind = BypassSwitchType::from_raw(info.kind)
        .ok_or_else(|| refused(format!("bypass switch type {}", info.kind)))?;
    Ok(BypassInfo {
        kind,
        invert: info.invert,
        delay_ms: info.delay_ms,
        latch_emulation: info.latch_emulation,
    })
}

/// `0x171e78` on a lane, input gate or mixer: the link code (`bypass_expression[0]`) with
/// the stored range (`0x170110`) and the switch settings (`expression_bypass_info[0]`,
/// `0x51c4f8`) on the parameter `index`, or on the mixer bypass. A missing stored model does
/// nothing. A changed code or any settings take a direct undo frame (`0x17212c`); the
/// overrides then refresh (`0x3233b0`). No echo.
fn site_switch(
    model: &mut StateModel<'_>,
    site: Site,
    index: Option<u32>,
    message: &Model,
) -> DeviceResult<()> {
    let stored = model.state.current.as_ref().and_then(|current| {
        let stored = preset::stored_site_model(&current.preset, site)?;
        Some(match index {
            Some(index) => stored.params.get(index as usize)?.expression.unwrap_or(0),
            None => stored
                .bypass_expression
                .first()
                .map_or(0, |link| link.expression),
        })
    });
    let Some(stored) = stored else {
        return Ok(());
    };
    let code = message
        .bypass_expression
        .first()
        .map(|link| link.expression);
    let info = message
        .expression_bypass_info
        .first()
        .map(switch_info)
        .transpose()?;
    if code.is_none_or(|code| code == stored) && info.is_none() {
        return Ok(());
    }
    let edit = PresetEdit::SiteSwitch {
        site,
        index,
        code,
        info,
    };
    model.undoable(UndoPath::Direct, |model| {
        model.act(GRID, Action::PresetEdit(edit))
    })
}

/// `0x1700b8`: scene mode on (`0x517f54`) or off (`0x524770`) of a parameter and the ones
/// linked to it (`preset::scene_mode_group`). No undo frame. Each changed index pushes its
/// flag (`0x25f1b4`); off also echoes the value it keeps: block `0x25d51c` and junction
/// `0x25e55c` held 100 ms, lane `0x25ea1c` and gate `0x25edcc` at once.
fn scene_mode(
    model: &mut StateModel<'_>,
    site: Site,
    index: u32,
    enabled: bool,
    echo: &mut GridEcho,
) -> DeviceResult<()> {
    let indices = match (&*model.state.current, &model.state.specs) {
        (Some(current), Some(specs)) => {
            preset::scene_mode_group(current, specs, site, index, enabled)
        }
        _ => vec![index],
    };
    let edit = PresetEdit::SceneMode {
        site,
        index,
        enabled,
    };
    model.act(GRID, Action::PresetEdit(edit))?;
    for index in indices {
        let flag = Param {
            index: Some(index),
            scene_mode: Some(enabled),
            ..Default::default()
        };
        echo.now.push(grid_update(site_preset(site, flag)));
        if enabled {
            continue;
        }
        let Some(value) = stored_value(model, site, index) else {
            continue;
        };
        match site {
            Site::Slot(slot) => echo.delayed.push(block_value_echo(slot, index, value)),
            Site::Mixer(_) | Site::Splitter(_) => {
                echo.delayed.push(junction_echo(site, index, value));
            }
            _ => echo
                .now
                .push(grid_update(site_preset(site, one_param(index, value)))),
        }
    }
    Ok(())
}

/// A Grid push of one parameter message on a site's model (`0x25f1b4` layout): a block by
/// column, a lane, an input gate, a combined splitter or a mixer.
pub(super) fn site_preset(site: Site, param: Param) -> Preset {
    let model = Model {
        params: vec![param],
        ..Default::default()
    };
    let (row, chain) = match site {
        Site::Slot(slot) => (
            slot.row,
            Chain {
                models: vec![Model {
                    column: Some(slot.column.index() as u32),
                    ..model
                }],
                ..Default::default()
            },
        ),
        Site::OutputControl(row) => (
            row,
            Chain {
                output_control: vec![model],
                ..Default::default()
            },
        ),
        Site::InputControl(row) => (
            row,
            Chain {
                input_control: vec![model],
                ..Default::default()
            },
        ),
        Site::Splitter(pair) => (
            pair.master(),
            Chain {
                combined_splitter: vec![model],
                ..Default::default()
            },
        ),
        Site::Mixer(pair) => (
            pair.master(),
            Chain {
                mixer: vec![model],
                ..Default::default()
            },
        ),
    };
    chain_preset(row, chain)
}

/// The junction value echo `0x25e55c`, held 100 ms. Inferred: the model holds the one
/// parameter (`0x25aef4` is not read).
fn junction_echo(site: Site, index: u32, value: ParamValue) -> DelayedGrid {
    let id = match site {
        Site::Mixer(_) => MIXER,
        _ => COMBINED_SPLITTER,
    };
    let preset = site_preset(site, one_param(index, value));
    let row = preset.chains[0].row.unwrap_or_default();
    DelayedGrid {
        target: (row, id, index),
        message: grid_message(preset),
    }
}

/// `setExpParameterValue` `0x520074` → `0x25f060`: an expression write echoes its one
/// parameter at once (block `0x25d51c`, junction `0x25e55c`, lane `0x25ea1c`, gate
/// `0x25edcc`; only Control receivers set the hold flag `+0x44`). Stock sends nothing for an
/// index of 100 or more.
pub(in crate::device) fn expression_echo(
    site: Site,
    index: u32,
    value: f32,
) -> Option<ControlMessage> {
    let param = one_param(index, ParamValue::Float(value));
    (index < 100).then(|| grid_update(site_preset(site, param)))
}

/// `0x2e1edc` reads the slot's notification flag; `0x514fd8` sets it and echoes
/// `0x25fa00` (the model with `notify_flag`). Stock shows it in the UI instead when the
/// block editor holds the slot; that view is not modelled.
fn notify(model: &mut StateModel<'_>, slot: Slot, echo: &mut GridEcho) -> DeviceResult<()> {
    let (row, column) = (slot.row.index() as u32, slot.column.index() as u32);
    let pending = model.state.current.as_ref().is_some_and(|current| {
        current
            .preset
            .model_update_notifications
            .iter()
            .any(|stored| (stored.row, stored.column) == (row, column))
    });
    if pending {
        return Ok(());
    }
    model.act(GRID, Action::PresetEdit(PresetEdit::Notify(slot)))?;
    let block = Model {
        column: Some(column),
        notify_flag: Some(true),
        ..Default::default()
    };
    echo.now.push(grid_update(chain_preset(
        slot.row,
        Chain {
            models: vec![block],
            ..Default::default()
        },
    )));
    Ok(())
}

/// `0x59d4b4` (input below 15) and `0x59ccc4` (output up to 22), each with a direct undo
/// frame (`0x59d4e0`, `0x59ccec`; one here), in one compile. No echo.
fn ports(model: &mut StateModel<'_>, row: Row, chain: &Chain) -> DeviceResult<()> {
    let mut edits = Vec::new();
    if let Some(raw) = chain.in_portid.filter(|&raw| raw < 0xf) {
        let port = InputPort::from_raw(raw).ok_or_else(|| refused(format!("input {raw}")))?;
        edits.push(LayoutEdit::Input { row, port });
    }
    if let Some(raw) = chain.out_portid.filter(|&raw| raw <= 0x16) {
        let port = OutputPort::from_raw(raw).ok_or_else(|| refused(format!("output {raw}")))?;
        edits.push(LayoutEdit::Output { row, port });
    }
    if edits.is_empty() {
        return Ok(());
    }
    model.undoable(UndoPath::Direct, |model| {
        model.act(GRID, Action::EditLayouts(edits))
    })
}

/// `0x727ad4`, then `0x727e1c`: lane VOLUME, PAN, MUTE and SOLO (0..=3) as floats with a
/// timer undo frame and the lane echo `0x25ea1c` at once. MUTE and SOLO store `0.5 < value`
/// as 1.0 or 0.0 (`0x520498`, `0x520540`) and do nothing on a row that routes to the next
/// row (outputs `0x10..=0x12`).
fn lane_update(
    model: &mut StateModel<'_>,
    row: Row,
    lane: &Model,
    echo: &mut GridEcho,
) -> DeviceResult<()> {
    if let Some(param) = lane.params.first() {
        let index = param.index.unwrap_or(0);
        match first_value(param) {
            Some(&ParamValue::Float(value)) if index <= 3 => {
                let next_row = model.state.current.as_ref().is_some_and(|current| {
                    let output = current.deployment.layout().lane(row).output as u32;
                    (0x10..=0x12).contains(&output)
                });
                if index < 2 || !next_row {
                    let site = Site::OutputControl(row);
                    let value = match index {
                        0 | 1 => value,
                        _ => f32::from(u8::from(value > 0.5)),
                    };
                    model.undoable(UndoPath::ControlTimer, |model| {
                        model.act(GRID, Action::SetParameter { site, index, value })
                    })?;
                    echo.now.push(super::lane_echo(row, index, value));
                }
            }
            _ if index < 2
                && let Some(code) = param.expression =>
            {
                return site_link(model, Site::OutputControl(row), index, code, param);
            }
            _ => {
                if let Some(enabled) = param.scene_mode {
                    scene_mode(model, Site::OutputControl(row), index, enabled, echo)?;
                }
            }
        }
    }
    if has_switch(lane) {
        // `0x727e1c`: column 0 is MUTE (2), column 1 SOLO (3).
        let index = match lane.column.unwrap_or(0) {
            0 => 2,
            1 => 3,
            _ => return Ok(()),
        };
        return site_switch(model, Site::OutputControl(row), Some(index), lane);
    }
    Ok(())
}

/// `0x727794`, then `0x727994`: the input gate (model 28000). A float value sets it with a
/// timer undo frame and the echo `0x25edcc` at once; an index the gate does not store does
/// nothing (owned). Stock skips the row while `0x582504(+0xb910, row)` holds it (inferred:
/// the capture lock).
fn input_update(
    model: &mut StateModel<'_>,
    row: Row,
    input: &Model,
    echo: &mut GridEcho,
) -> DeviceResult<()> {
    if model.capture_lock {
        return Ok(());
    }
    let site = Site::InputControl(row);
    if let Some(param) = input.params.first() {
        let index = param.index.unwrap_or(0);
        match first_value(param) {
            Some(&ParamValue::Float(value)) => {
                if stored_value(model, site, index).is_none() {
                    return Ok(());
                }
                model.undoable(UndoPath::ControlTimer, |model| {
                    model.act(GRID, Action::SetParameter { site, index, value })
                })?;
                let value = stored_value(model, site, index).unwrap_or(ParamValue::Float(value));
                let gate = Model {
                    params: vec![one_param(index, value)],
                    ..Default::default()
                };
                echo.now.push(grid_update(chain_preset(
                    row,
                    Chain {
                        input_control: vec![gate],
                        ..Default::default()
                    },
                )));
            }
            _ if let Some(code) = param.expression => {
                return site_link(model, site, index, code, param);
            }
            _ => {
                if let Some(enabled) = param.scene_mode {
                    scene_mode(model, site, index, enabled, echo)?;
                }
            }
        }
    }
    if has_switch(input) {
        // `0x727994`: parameter 1 of 28000.
        return site_switch(model, site, Some(1), input);
    }
    Ok(())
}

/// `0x2561d4`: split and mix columns (-1 none, else 0..=7, split not after mix). Stock
/// test-compiles the copy (`0x514894`) and records the direct undo frame (`0x2563fc`) only
/// when it fits; here the frame is kept when the deploy commits. The echo is `0x25e3f8`.
fn junction_points(
    model: &mut StateModel<'_>,
    row: Row,
    points: &SplitControlPoints,
    echo: &mut GridEcho,
) -> DeviceResult<()> {
    let (split, mix) = (points.split, points.mix);
    let column = |raw: i32| u8::try_from(raw).ok().and_then(|raw| Column::new(raw).ok());
    let valid = (-1..8).contains(&split)
        && (-1..8).contains(&mix)
        && (split == -1 || mix == -1 || split <= mix);
    if !valid {
        return Ok(());
    }
    let pair = row.pair();
    let edits = vec![
        LayoutEdit::Split {
            pair,
            column: column(split),
        },
        LayoutEdit::Mix {
            pair,
            column: column(mix),
        },
    ];
    model.undoable(UndoPath::Direct, |model| {
        model.act(GRID, Action::EditLayouts(edits))
    })?;
    echo.now.push(grid_update(chain_preset(
        row,
        Chain {
            split_control_points: vec![SplitControlPoints {
                split,
                mix,
                ..Default::default()
            }],
            ..Default::default()
        },
    )));
    Ok(())
}

/// `0x17b384`: the pair's junction bypass (`splitBypass`, read by `0x514378`, set by
/// `0x51dc80` on the mixer). A changed value takes a timer undo frame (`0x17b3c8`); the
/// echo is `0x25e74c`.
fn junction_bypass(
    model: &mut StateModel<'_>,
    row: Row,
    bypassed: bool,
    echo: &mut GridEcho,
) -> DeviceResult<()> {
    let site = Site::Mixer(row.pair());
    let stored = model.state.current.as_ref().and_then(|current| {
        let mixer = current.deployment.layout().instance(site)?;
        Some(mixer.bypass.at(current.deployment.scene()))
    });
    if stored.is_none_or(|stored| stored == bypassed) {
        return Ok(());
    }
    model.undoable(UndoPath::ControlTimer, |model| {
        model.act(GRID, Action::SetBypass { site, bypassed })
    })?;
    echo.now.push(grid_update(chain_preset(
        row,
        Chain {
            split_bypass: vec![SceneBypass {
                bypass: bypassed,
                ..Default::default()
            }],
            ..Default::default()
        },
    )));
    Ok(())
}

/// `0x17b084`: a mixer (11000) or combined splitter (10004) parameter. A value takes the
/// undo timer (`0x17b168`) and waits for the echo `0x25e55c`; else the link or the scene
/// mode.
fn junction_param(
    model: &mut StateModel<'_>,
    site: Site,
    param: &Param,
    echo: &mut GridEcho,
) -> DeviceResult<()> {
    let Some(index) = param.index else {
        return Ok(());
    };
    if let Some(value) = first_value(param).and_then(as_number) {
        if stored_value(model, site, index).is_none() {
            return Ok(());
        }
        model.undoable(UndoPath::ControlTimer, |model| {
            model.act(GRID, Action::SetParameter { site, index, value })
        })?;
        let value = stored_value(model, site, index).unwrap_or(ParamValue::Float(value));
        echo.delayed.push(junction_echo(site, index, value));
        return Ok(());
    }
    if let Some(code) = param.expression {
        return site_link(model, site, index, code, param);
    }
    if let Some(enabled) = param.scene_mode {
        return scene_mode(model, site, index, enabled, echo);
    }
    Ok(())
}

/// `0x254910`: `bypass[0]` row and `colBypass[0]` column; a changed first scene value
/// toggles the block (`0x521514`) after a direct undo frame (`0x254a70`). The echo is
/// `0x25d364`. Stock first
/// checks that a capture block's capture is in the library; that check is not modelled.
fn block_bypass(
    model: &mut StateModel<'_>,
    record: &BypassRow,
    echo: &mut GridEcho,
) -> DeviceResult<()> {
    let Some(row) = row_of(record.row) else {
        return Ok(());
    };
    let Some(column_record) = record.col_bypass.first() else {
        return Ok(());
    };
    let (Some(column), Some(scene)) = (
        column_of(column_record.column),
        column_record.scene_bypass.first(),
    ) else {
        return Ok(());
    };
    let site = Site::Slot(Slot { row, column });
    let stored = model.state.current.as_ref().and_then(|current| {
        let block = current.deployment.layout().instance(site)?;
        Some(block.bypass.at(current.deployment.scene()))
    });
    if stored.is_none_or(|stored| stored == scene.bypass) {
        return Ok(());
    }
    let bypassed = scene.bypass;
    model.undoable(UndoPath::Direct, |model| {
        model.act(GRID, Action::SetBypass { site, bypassed })
    })?;
    echo.now.push(bypass_echo(Slot { row, column }, bypassed));
    Ok(())
}

/// The block bypass push `0x25d364(row, column, bypass)`: one `bypass` row with one column.
pub(super) fn bypass_echo(slot: Slot, bypassed: bool) -> ControlMessage {
    grid_update(Preset {
        bypass: vec![BypassRow {
            row: Some(slot.row.index() as u32),
            col_bypass: vec![ColBypass {
                column: Some(slot.column.index() as u32),
                scene_bypass: vec![SceneBypass {
                    bypass: bypassed,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    })
}

/// `0x256d7c`: `tempoProgramData[0].params[0]`. Index 1 (enTYPE) belongs to TempoMessage;
/// a float value goes to the tempo controller (`0x382540`); without a value the scene mode
/// changes ([`tempo_scene_mode`]).
fn tempo_update(
    model: &mut StateModel<'_>,
    tempo: &Model,
    echo: &mut GridEcho,
) -> DeviceResult<()> {
    let Some(param) = tempo.params.first() else {
        return Ok(());
    };
    let Some(index) = param.index.filter(|&index| index != 1) else {
        return Ok(());
    };
    match param.values.first() {
        Some(value) => match value.value {
            Some(ParamValue::Float(value)) => model.act(
                GRID,
                Action::SetTempoParameter {
                    index,
                    value,
                    remote: true,
                },
            ),
            _ => Ok(()),
        },
        None => match param.scene_mode {
            Some(enabled) => tempo_scene_mode(model, index, enabled, echo),
            None => Ok(()),
        },
    }
}

/// `0x1700b8` row -1 → `0x517f54` (on) / `0x524770` (off) on the tempo model, without an
/// undo frame. Each changed index pushes its flag (`0x25f5ec`); off also pushes the value it
/// keeps (`0x25f3f0`). Both go out inside the receiver. The DSP keeps its value: off keeps
/// the current scene's value.
fn tempo_scene_mode(
    model: &mut StateModel<'_>,
    index: u32,
    enabled: bool,
    echo: &mut GridEcho,
) -> DeviceResult<()> {
    model.act(
        GRID,
        Action::PresetEdit(PresetEdit::TempoSceneMode { index, enabled }),
    )?;
    let Some(current) = model.state.current.as_ref() else {
        return Ok(());
    };
    let scene = current.deployment.scene().index();
    let Some(tempo) = current.preset.tempo_program_data.first() else {
        return Ok(());
    };
    let push = |param: Param| {
        grid_update(Preset {
            tempo_program_data: vec![Model {
                params: vec![param],
                ..Default::default()
            }],
            ..Default::default()
        })
    };
    for index in preset::tempo_scene_mode_indices(index) {
        let Some(stored) = tempo.params.get(index as usize) else {
            continue;
        };
        echo.now.push(push(Param {
            index: Some(index),
            scene_mode: Some(enabled),
            ..Default::default()
        }));
        if !enabled && let Some(value) = stored.value(scene) {
            echo.now.push(push(one_param(index, value.clone())));
        }
    }
    Ok(())
}

/// `0x254ed0`: every assignment with a slot in the grid. UPDATE assigns stomps 0..=7
/// (`0x523224`, echo `0x25e2b0`); DELETE (no echo) clears the slot (`0x522b64`).
fn stomp_assignments(
    model: &mut StateModel<'_>,
    assignments: &[StompAssignment],
    mut echo: Option<&mut GridEcho>,
) -> DeviceResult<()> {
    for entry in assignments {
        let (Some(row), Some(column)) = (row_of(Some(entry.row)), column_of(Some(entry.column)))
        else {
            continue;
        };
        let slot = Slot { row, column };
        let Some(echo) = echo.as_deref_mut() else {
            let edit = PresetEdit::Stomp {
                slot,
                assignment: None,
            };
            model.act(GRID, Action::PresetEdit(edit))?;
            continue;
        };
        if entry.stomp_index >= ASSIGNABLE_STOMPS {
            continue;
        }
        let role = entry.role.unwrap_or(StompRole::Primary);
        let edit = PresetEdit::Stomp {
            slot,
            assignment: Some((entry.stomp_index, role)),
        };
        model.act(GRID, Action::PresetEdit(edit))?;
        echo.now.push(grid_update(Preset {
            stomp_mode_assignments: vec![StompAssignment {
                row: entry.row,
                column: entry.column,
                stomp_index: entry.stomp_index,
                role: Some(role),
                ..Default::default()
            }],
            ..Default::default()
        }));
    }
    Ok(())
}

pub(super) fn one_param(index: u32, value: ParamValue) -> Param {
    Param {
        index: Some(index),
        values: vec![SceneValue {
            value: Some(value),
            ..Default::default()
        }],
        ..Default::default()
    }
}

pub(super) fn chain_preset(row: Row, chain: Chain) -> Preset {
    Preset {
        chains: vec![Chain {
            row: Some(row.index() as u32),
            ..chain
        }],
        ..Default::default()
    }
}

/// A Grid sender push (`0x25c5dc`): action UPDATE and the partial preset.
fn grid_message(preset: Preset) -> GridMessage {
    GridMessage {
        action: Some(MessageAction::Update.into()),
        preset: Some(RawMessageBytes {
            bytes: preset.to_bytes(),
        }),
        ..Default::default()
    }
}

pub(super) fn grid_update(preset: Preset) -> ControlMessage {
    ControlMessage::Grid(grid_message(preset))
}

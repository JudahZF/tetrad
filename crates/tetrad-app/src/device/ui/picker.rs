//! Virtual Device Presets and expression assignment on the bridge (tetrad-ui picker screens).
//!
//! Presets are the `model_presets` rows of `zenui.db` and of the read-only `factory.db`
//! (`0x008328a4`; the stock `hash` column is the model ID). A factory row id carries
//! [`FACTORY_ID`] in the view (stock `ModelPresetID` has a factory flag, `0x1f4584`).
//! An audition edits the block live (stock builder `0x1f7b10`): the same model takes
//! parameter edits (scene modes stay), another model is pasted; then the preset's EXP links,
//! ranges and bypass switch are stored. The bridge keeps the block from before the first
//! audition until ✓ (Keep) or ✕ (Revert); a revert writes the original stored block back
//! (EXP links and ranges, switch, scene bypass). Without an audition the list marks the
//! preset the block matches ([`Picker::block_match`], stock `0x1f7f50`), which the editor
//! header also shows. Expression links edit the stored preset at once; the bridge keeps the
//! links from before the first edit for Revert.

use tetrad_catalog::{ModelSpec, ParamSpec, ParameterType, Specs};
use tetrad_core::conversion::qt_fuzzy_compare;
use tetrad_core::performance::expression::{BypassInfo, BypassSwitchType, DEFAULT_BYPASS_DELAY_MS};
use tetrad_engine::{Instance, SceneIndex, Site, Slot};
use tetrad_storage::stock::{ExpSwitch, ParamEntry, StockDb};
use tetrad_ui::{
    DevicePresetItem, DevicePresetRequest, ExpressionLink, ExpressionPedal, ExpressionRangeView,
    ExpressionRequest, ExpressionSwitch, ExpressionSwitchView, ExpressionTarget, GridPosition,
    MAX_DEVICE_PRESETS, MAX_LIST_ITEMS, PresetMatch, SettingsPresetPage, SettingsPresetRequest,
    UiSnapshot,
};

use std::collections::BTreeMap;
use std::sync::Arc;

use super::super::actions::{Action, DevicePresetEdit, LayoutEdit};
use super::super::{State, Subsystem};
use super::super::{global_eq, preset};
use super::{Request, notice, project};

/// View id bit of a `factory.db` preset; user ids are SQLite rowids (below 2^63).
pub(super) const FACTORY_ID: u64 = 1 << 63;

/// Owned: a stored value text matches a live value within this (stock writes `QString::number`
/// text, six significant digits).
const MATCH_TOLERANCE: f32 = 1e-5;

#[derive(Default)]
pub(super) struct Picker {
    /// User then factory presets, at most [`MAX_DEVICE_PRESETS`]; the snapshot shares them
    /// ([`Picker::listed`]). Read again after a model preset write ([`Picker::refresh`]).
    presets: Option<Arc<Vec<DevicePresetItem>>>,
    /// [`State::model_preset_revision`] at the last read.
    read: u64,
    /// The last read failed: the next open tries again.
    failed: bool,
    /// The match candidates of one model ([`Picker::block_match`]); dropped when the list is
    /// read again. Stock caches them per model too (`0x1f9198`).
    candidates: Option<Candidates>,
    /// `factory.db`, opened read-only at boot.
    factory: Option<StockDb>,
    /// The picker shows: the list marks the preset the block matches.
    open: bool,
    /// Pinned models in pin order (`pinned_models` rows that are model ids; the other rows
    /// are capture keys). Read with the presets; a pin updates it at once.
    pinned: Vec<u32>,
    audition: Option<Audition>,
    /// The block's links and switch before the first expression edit (Revert).
    expression: Option<(
        GridPosition,
        Vec<ExpressionLink>,
        ExpressionSwitchView,
        Vec<ExpressionRangeView>,
    )>,
    /// The model of a device save refused for equal values, until the bridge shows the
    /// Preset Conflict dialog ([`Picker::take_conflict`]).
    conflict: Option<u32>,
    /// The Add device capture browser list.
    pub captures: super::captures::Captures,
}

impl std::fmt::Debug for Picker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picker")
            .field("presets", &self.presets)
            .field("factory", &self.factory.is_some())
            .field("audition", &self.audition)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Audition {
    position: GridPosition,
    preset: u64,
    /// The block before the first audition; `None` for an empty slot (add mode).
    original: Option<Box<Instance>>,
    /// Its stored block and bypass record, written back by a revert.
    stored: Option<(Box<tetrad_preset::Model>, Box<tetrad_preset::ColBypass>)>,
}

/// The presets of `model` as the match reads them, in stock map order.
#[derive(Debug)]
struct Candidates {
    model: u32,
    presets: Vec<(PresetMatch, Vec<ParamEntry>)>,
}

impl Picker {
    pub fn presets(&self) -> &[DevicePresetItem] {
        self.presets.as_deref().map_or(&[], Vec::as_slice)
    }

    /// The presets the snapshot lists: the same `Arc` until the next read.
    pub fn listed(&self) -> Arc<Vec<DevicePresetItem>> {
        self.presets.clone().unwrap_or_default()
    }

    pub fn pinned(&self) -> &[u32] {
        &self.pinned
    }

    /// Mirrors a pin request until the next read.
    pub fn pin(&mut self, model: u32, pinned: bool) {
        self.pinned.retain(|id| *id != model);
        if pinned {
            self.pinned.push(model);
        }
    }

    /// `factory.db` (read-only): factory captures for the capture block build (`0x704530`).
    pub fn factory(&self) -> Option<&StockDb> {
        self.factory.as_ref()
    }

    /// Keeps the capture browser list current ([`super::captures::Captures::refresh`]): it is
    /// read from `zenui.db` and this `factory.db` after each new media list.
    pub fn refresh_captures(
        &mut self,
        state: &State,
        cipher: Option<&tetrad_media::factory::DeviceCipher>,
        media: &Arc<Vec<tetrad_ui::MediaEntry>>,
    ) -> Vec<String> {
        let user = state.library.as_ref().and_then(|l| l.database.as_ref());
        let path = |db: Option<&StockDb>| db.and_then(StockDb::path).map(ToOwned::to_owned);
        let sources = super::captures::Sources {
            user: path(user),
            factory: path(self.factory.as_ref()),
        };
        (self.captures).refresh(media, sources, cipher)
    }

    pub fn set_factory(&mut self, factory: StockDb) {
        self.factory = Some(factory);
        self.presets = None;
        self.candidates = None;
    }

    /// The preset auditioned in the block at `position`, else (picker shown) the listed preset
    /// the block matches ([`Self::block_match`]).
    pub fn selected_preset(
        &self,
        position: Option<GridPosition>,
        matched: Option<PresetMatch>,
    ) -> Option<u64> {
        if let Some(audition) = &self.audition {
            return (Some(audition.position) == position).then_some(audition.preset);
        }
        match matched {
            Some(PresetMatch::Preset(id)) if self.open => Some(id),
            _ => None,
        }
    }

    /// `0x1f7f50`: the first user or factory preset of the block's model, or else its catalog
    /// defaults, whose every parameter the block holds in the current scene. The order is the
    /// stock map's (`0x1f9668`): the id as text, then user before factory, so the catalog
    /// default entry (`SpecialFactoryModelPresetID`) comes last. Per parameter (`0x5ce580`):
    /// a rotary switch compares step indices, a string parameter its text, any other value
    /// within Qt's relative 1e-5 ([`qt_fuzzy_compare`]); a row the preset lacks counts as the
    /// catalog default (`0x1f78f4`). The XML parameters must also have the same EXP link and
    /// range (`+0x9c`, `+0xa0`, `+0xa4`). Then MUTE and BYPASS (the scene bypass as 0/1; I:
    /// stock re-checks on each bypass change, `0x48de18`) compare by value only. The sidechain
    /// source parameter (`+0xec`) is skipped. Not compared (owned): the EXP link of BYPASS and
    /// the bypass switch (`+0xbc`), which stock checks only when both sides link BYPASS.
    pub fn block_match(&mut self, state: &State, position: GridPosition) -> Option<PresetMatch> {
        let current = state.current.as_ref()?;
        let slot = project::slot_of(position)?;
        let live = current.deployment.layout().instance(Site::Slot(slot))?;
        let spec = state.specs.as_ref()?.model(live.model.0)?;
        let scene = current.deployment.scene();
        let stored = current.preset.block(stored_slot(slot)).ok().flatten();
        let (_, mut rows) = preset_rows(live, stored, scene);
        let hidden = spec.parameters.len() as i64;
        rows.extend(
            [live.mute.at(scene), live.bypass.at(scene)]
                .into_iter()
                .zip(hidden..)
                .map(|(on, index)| flag_row(index, on)),
        );
        let live = all_parameters(spec, &rows);
        if self.candidates.as_ref().is_none_or(|c| c.model != spec.id) {
            self.candidates = Some(self.candidates(state, spec));
        }
        let sidechain = spec
            .sidechain
            .as_ref()
            .map(|sidechain| sidechain.source_param);
        let candidates = self.candidates.as_ref()?;
        (candidates.presets.iter())
            .find(|(_, preset)| {
                (live.iter().zip(preset)).all(|(live, preset)| {
                    let index = u32::try_from(live.index).ok();
                    index == sidechain
                        || same_parameter(
                            index.and_then(|index| spec.parameters.get(index as usize)),
                            live,
                            preset,
                        )
                })
            })
            .map(|(id, _)| *id)
    }

    /// The listed presets of `spec` (a row read error leaves that preset out) and the
    /// catalog default entry, in stock map order.
    fn candidates(&self, state: &State, spec: &ModelSpec) -> Candidates {
        const SPECIAL: &str = "SpecialFactoryModelPresetID";
        let mut presets: Vec<_> = (self.presets().iter())
            .filter(|item| item.model_id == spec.id)
            .filter_map(|item| {
                let rows = self.rows(state, item.id).ok()?;
                let key = ((item.id & !FACTORY_ID).to_string(), item.factory);
                Some((
                    key,
                    PresetMatch::Preset(item.id),
                    all_parameters(spec, &rows),
                ))
            })
            .collect();
        let defaults = all_parameters(spec, &[]);
        presets.push((
            (SPECIAL.into(), true),
            PresetMatch::CatalogDefault,
            defaults,
        ));
        presets.sort_by(|a, b| a.0.cmp(&b.0));
        Candidates {
            model: spec.id,
            presets: (presets.into_iter())
                .map(|(_, id, rows)| (id, rows))
                .collect(),
        }
    }

    /// The database of a listed preset and its row id there.
    fn database<'a>(&'a self, state: &'a State, preset: u64) -> Result<(&'a StockDb, i64), String> {
        let (database, id) = if preset & FACTORY_ID != 0 {
            (self.factory.as_ref(), preset & !FACTORY_ID)
        } else {
            let user = state.library.as_ref().and_then(|l| l.database.as_ref());
            (user, preset)
        };
        let database = database.ok_or("the preset database is not open")?;
        Ok((
            database,
            i64::try_from(id).map_err(|error| error.to_string())?,
        ))
    }

    /// The parameter rows of a listed preset.
    fn rows(&self, state: &State, preset: u64) -> Result<Vec<ParamEntry>, String> {
        let (database, id) = self.database(state, preset)?;
        database
            .view()
            .model_preset_params(id)
            .map_err(|error| error.to_string())
    }

    /// The bypass switch of a listed preset.
    fn switch(&self, state: &State, preset: u64) -> Result<ExpSwitch, String> {
        let (database, id) = self.database(state, preset)?;
        let preset = database.view().model_preset(id);
        let preset = preset.map_err(|error| error.to_string())?;
        Ok(preset.ok_or("unknown device preset")?.exp_switch)
    }

    /// Reads the presets on first use and after each model preset write
    /// ([`State::model_preset_revision`]; from this bridge, Cortex Control or a restore): user
    /// rows, then factory rows. An open alone reads nothing: on the QC a read at each open cost
    /// about 40 ms of the I/O page open (r53). The match candidates are read again with
    /// them, so the editor header follows a created, deleted or changed preset at once, as
    /// stock re-checks on `modelPresetCreated/Deleted/Updated` (`0x48c2f0`). A default is a user
    /// `default_model_presets` row; its `isFactory` picks the database. A failed read is
    /// tried again at the next open or write.
    pub fn refresh(&mut self, state: &State, open: bool) -> Result<(), tetrad_storage::Error> {
        let retry = open && !self.open && self.failed;
        self.open = open;
        if self.presets.is_some() && !retry && self.read == state.model_preset_revision {
            return Ok(());
        }
        self.read = state.model_preset_revision;
        self.failed = true;
        self.presets = Some(Arc::default());
        self.candidates = None;
        let user = state.library.as_ref().and_then(|l| l.database.as_ref());
        self.pinned = match user {
            Some(database) => (database.view().pinned_models()?.iter())
                .filter_map(|key| key.parse().ok())
                .collect(),
            None => Vec::new(),
        };
        let defaults = match user {
            Some(database) => database.view().default_model_presets()?,
            None => Vec::new(),
        };
        let mut presets = Vec::new();
        for (database, factory) in [(user, false), (self.factory.as_ref(), true)] {
            let Some(database) = database else {
                continue;
            };
            let rows = database.view().model_presets()?;
            presets.extend(
                rows.into_iter()
                    // An over-long stored name would make the whole snapshot invalid.
                    .filter(|preset| preset.name.len() <= tetrad_ui::MAX_TEXT_BYTES)
                    .filter_map(|preset| {
                        let id = u64::try_from(preset.id)
                            .ok()
                            .filter(|id| id & FACTORY_ID == 0)?;
                        Some(DevicePresetItem {
                            id: if factory { id | FACTORY_ID } else { id },
                            model_id: u32::try_from(preset.model).ok()?,
                            default: defaults.iter().any(|default| {
                                default.factory == factory
                                    && default.model == preset.model
                                    && default.preset_id == preset.id
                            }),
                            name: preset.name,
                            factory,
                        })
                    }),
            );
        }
        // A bound for memory; stock has none. Over it the last factory rows drop out.
        presets.truncate(MAX_DEVICE_PRESETS);
        self.presets = Some(presets.into());
        self.failed = false;
        Ok(())
    }

    /// Global EQ and I/O Virtual Device Presets (`0x227a84`): the page values are the rows
    /// of model 4004 or 31000 in `zenui.db`, through the same writes as block presets.
    pub fn settings_preset(
        &mut self,
        page: SettingsPresetPage,
        request: SettingsPresetRequest,
        state: &State,
    ) -> Vec<Request> {
        match self.settings_requests(page, request, state) {
            Ok(requests) => requests,
            Err(detail) => vec![notice(Subsystem::Ui, detail)],
        }
    }

    fn settings_requests(
        &mut self,
        page: SettingsPresetPage,
        request: SettingsPresetRequest,
        state: &State,
    ) -> Result<Vec<Request>, String> {
        use SettingsPresetRequest as R;
        let model = page.model_id();
        let current = || settings_rows(page, state).ok_or("the page model is not in the catalog");
        let edit = |edit| Ok(vec![Request::Action(Action::DevicePreset(edit))]);
        match request {
            R::Load(preset) => Ok(vec![Request::Action(Action::LoadSettingsPreset {
                model,
                rows: self.rows(state, preset)?,
            })]),
            R::Create { .. } if self.conflicts(state, model, &current()?, None) => {
                self.conflict = Some(model);
                Ok(Vec::new())
            }
            R::Overwrite(preset) if self.conflicts(state, model, &current()?, Some(preset)) => {
                self.conflict = Some(model);
                Ok(Vec::new())
            }
            R::Create { name } => Ok(vec![Request::Action(Action::CreateDevicePreset {
                model,
                name,
                switch: default_switch(),
                params: current()?,
            })]),
            // M593: "Set as Default" is not available for I/O Settings presets.
            R::SetDefault(_) if page == SettingsPresetPage::Io => {
                Err("I/O Settings presets have no default".into())
            }
            R::SetDefault(preset) => edit(DevicePresetEdit::SetDefault {
                model,
                preset: preset & !FACTORY_ID,
                factory: preset & FACTORY_ID != 0,
            }),
            R::Rename { preset, .. } | R::Overwrite(preset) | R::Delete(preset)
                if preset & FACTORY_ID != 0 =>
            {
                Err("factory presets are read-only".into())
            }
            R::Rename { preset, name } => edit(DevicePresetEdit::Rename { preset, name }),
            R::Overwrite(preset) => edit(DevicePresetEdit::Overwrite {
                preset,
                switch: default_switch(),
                params: current()?,
            }),
            R::Delete(preset) => edit(DevicePresetEdit::Delete(preset)),
            R::RestoreDefault(id) => {
                let value = super::super::global_eq::default_setting(state, id)
                    .ok_or("the Global EQ value has no default")?;
                Ok(vec![Request::Action(Action::ChangeSetting { id, value })])
            }
        }
    }

    /// The first listed preset of each settings page whose every row equals the page value
    /// (`0x1f8600`; rows the page lacks, as mini-less connectors, are skipped).
    pub fn settings_match(&self, state: &State, page: SettingsPresetPage) -> Option<u64> {
        self.matching(state, page.model_id(), &settings_rows(page, state)?)
    }

    /// A device save of `rows` fails: another stored preset of `model` (not `target`, the
    /// overwritten one) has these values (`0x1f8b9c`/`0x1f88f8` → `0x1f7f50`). Stock then opens
    /// Preset Conflict (`0x397ff8(model, 0)`). The 32-preset limit and the catalog flag
    /// stay with the library writer.
    fn conflicts(
        &mut self,
        state: &State,
        model: u32,
        rows: &[ParamEntry],
        target: Option<u64>,
    ) -> bool {
        // The cached list may predate the last save; a failed read leaves the check to the
        // library writer.
        self.presets = None;
        self.refresh(state, self.open).is_ok()
            && (self.matching(state, model, rows)).is_some_and(|preset| Some(preset) != target)
    }

    /// The model was refused by [`Self::conflicts`] since the last call.
    pub(super) fn take_conflict(&mut self) -> Option<u32> {
        self.conflict.take()
    }

    fn matching(&self, state: &State, model: u32, rows: &[ParamEntry]) -> Option<u64> {
        let current: BTreeMap<i64, f32> = (rows.iter())
            .filter_map(|row| Some((row.index, row.value.parse().ok()?)))
            .collect();
        self.presets()
            .iter()
            .filter(|item| item.model_id == model)
            .find(|item| {
                self.rows(state, item.id).is_ok_and(|rows| {
                    rows.iter().all(|row| {
                        let Some(live) = current.get(&row.index) else {
                            return true;
                        };
                        row.value
                            .parse::<f32>()
                            .is_ok_and(|stored| (live - stored).abs() <= MATCH_TOLERANCE)
                    })
                })
            })
            .map(|item| item.id)
    }

    pub fn device_preset(
        &mut self,
        position: GridPosition,
        request: DevicePresetRequest,
        state: &State,
    ) -> Vec<Request> {
        match self.preset_requests(position, request, state) {
            Ok(requests) => requests,
            Err(detail) => vec![notice(Subsystem::Ui, detail)],
        }
    }

    fn preset_requests(
        &mut self,
        position: GridPosition,
        request: DevicePresetRequest,
        state: &State,
    ) -> Result<Vec<Request>, String> {
        use DevicePresetRequest as R;
        let slot = project::slot_of(position).ok_or("no grid slot")?;
        let (current, specs) = state
            .current
            .as_ref()
            .zip(state.specs.as_ref())
            .ok_or("no loaded preset")?;
        let layout = current.deployment.layout();
        let scene = current.deployment.scene();
        let live = project::instance(layout, position);
        let edit = |edit| Ok(vec![Request::Action(Action::DevicePreset(edit))]);
        match request {
            R::Audition { model_id, preset } => {
                let rows = self.rows(state, preset)?;
                let switch = self.switch(state, preset)?;
                let (original, stored) = match self.audition.take() {
                    Some(audition) if audition.position == position => {
                        (audition.original, audition.stored)
                    }
                    _ => (
                        live.cloned().map(Box::new),
                        stored_record(&current.preset, slot),
                    ),
                };
                let mut requests =
                    load_rows(state, slot, model_id, original.as_deref(), &rows, switch)?;
                // As the Cortex Control path (review-w6 F12): skipped rows are reported.
                let rejected = preset::rejected_model_preset_rows(specs, model_id, &rows);
                if rejected > 0 {
                    requests.push(notice(
                        Subsystem::Library,
                        format!(
                            "Virtual Device Preset {preset}: {rejected} invalid values left out"
                        ),
                    ));
                }
                self.audition = Some(Audition {
                    position,
                    preset,
                    original,
                    stored,
                });
                Ok(requests)
            }
            R::Keep => {
                self.audition = None;
                Ok(Vec::new())
            }
            R::Revert => Ok(match self.audition.take() {
                Some(audition) if audition.position == position => {
                    let original = audition.original.as_deref();
                    let mut requests = restore(slot, live, original, specs, scene);
                    // The audition stored the preset's links and switch (and a paste a fresh
                    // model): write the original stored block back.
                    if original.is_some()
                        && let Some((model, bypass)) = audition.stored
                    {
                        requests.push(Request::Action(Action::RestoreBlock {
                            slot,
                            model,
                            bypass,
                        }));
                    }
                    requests
                }
                _ => Vec::new(),
            }),
            R::SetDefault(preset) => {
                let model = self
                    .presets()
                    .iter()
                    .find(|item| item.id == preset)
                    .ok_or("unknown device preset")?
                    .model_id;
                edit(DevicePresetEdit::SetDefault {
                    model,
                    preset: preset & !FACTORY_ID,
                    factory: preset & FACTORY_ID != 0,
                })
            }
            R::Rename { preset, .. } | R::Overwrite(preset) | R::Delete(preset)
                if preset & FACTORY_ID != 0 =>
            {
                Err("factory presets are read-only".into())
            }
            R::Rename { preset, name } => edit(DevicePresetEdit::Rename { preset, name }),
            R::Delete(preset) => edit(DevicePresetEdit::Delete(preset)),
            R::Create { name } => {
                let live = live.ok_or("no block to store")?;
                let (switch, params) = live_rows(current, slot, live, scene)?;
                if self.conflicts(state, live.model.0, &params, None) {
                    self.conflict = Some(live.model.0);
                    return Ok(Vec::new());
                }
                Ok(vec![Request::Action(Action::CreateDevicePreset {
                    model: live.model.0,
                    name,
                    switch,
                    params,
                })])
            }
            // The block key is the model id (captures have no presets, manual "Unsupported
            // Items"; resync `apply_user_defaults` reads the same key).
            R::SetDefaults => {
                let live = live.ok_or("no block to store")?;
                let (switch, params) = live_rows(current, slot, live, scene)?;
                Ok(vec![Request::Action(Action::SetDefaultParams {
                    key: live.model.0.to_string(),
                    switch,
                    params,
                })])
            }
            R::RestoreDefault { parameter } => {
                let live = live.ok_or("no block")?;
                let model = live.model.0;
                let rows = self
                    .default_rows(state, model)?
                    .map(|(rows, _)| rows)
                    .unwrap_or_default();
                let defaults = preset::model_preset_block(specs, model, None, &rows)
                    .ok_or_else(|| format!("model {model} is not in the catalog"))?;
                let value = defaults
                    .params
                    .get(usize::from(parameter))
                    .ok_or("no such parameter")?
                    .at(scene);
                Ok(vec![Request::Action(Action::SetParameter {
                    site: Site::Slot(slot),
                    index: u32::from(parameter),
                    value,
                })])
            }
            R::Overwrite(preset) => {
                let live = live.ok_or("no block to store")?;
                let (switch, params) = live_rows(current, slot, live, scene)?;
                if self.conflicts(state, live.model.0, &params, Some(preset)) {
                    self.conflict = Some(live.model.0);
                    return Ok(Vec::new());
                }
                edit(DevicePresetEdit::Overwrite {
                    preset,
                    switch,
                    params,
                })
            }
        }
    }

    /// A new block of `model` with the user default parameters (`default_param`, as
    /// `0x667acc` reads them), else the model's default Virtual Device Preset (manual: "also
    /// applied automatically when loading the Virtual Device onto The Grid"). `None` without
    /// defaults: a plain add with the catalog values.
    pub fn add_block(
        &mut self,
        position: GridPosition,
        model: u32,
        state: &State,
    ) -> Option<Vec<Request>> {
        let slot = project::slot_of(position)?;
        let result = self.default_rows(state, model).and_then(|rows| {
            rows.map(|(rows, switch)| load_rows(state, slot, model, None, &rows, switch))
                .transpose()
        });
        match result {
            Ok(requests) => requests,
            Err(detail) => Some(vec![notice(Subsystem::Library, detail)]),
        }
    }

    /// The defaults a block of `model` takes: `default_param` rows with their switch, else
    /// the rows of the model's default preset.
    fn default_rows(
        &self,
        state: &State,
        model: u32,
    ) -> Result<Option<(Vec<ParamEntry>, ExpSwitch)>, String> {
        let key = model.to_string();
        if let Some(database) = state.library.as_ref().and_then(|l| l.database.as_ref()) {
            let view = database.view();
            let rows = view
                .default_params(&key)
                .map_err(|error| error.to_string())?;
            if !rows.is_empty() {
                let switch = view
                    .default_exp_switch(&key)
                    .map_err(|error| error.to_string())?
                    .unwrap_or_else(default_switch);
                return Ok(Some((rows, switch)));
            }
        }
        let Some(preset) = self
            .presets()
            .iter()
            .find(|item| item.model_id == model && item.default)
        else {
            return Ok(None);
        };
        Ok(Some((
            self.rows(state, preset.id)?,
            self.switch(state, preset.id)?,
        )))
    }

    pub fn expression(
        &mut self,
        position: GridPosition,
        request: ExpressionRequest,
        snapshot: &UiSnapshot,
    ) -> Vec<Request> {
        use ExpressionRequest as R;
        let Some(slot) = project::slot_of(position) else {
            return Vec::new();
        };
        if self
            .expression
            .as_ref()
            .is_none_or(|(at, ..)| *at != position)
        {
            self.expression = Some((
                position,
                snapshot.expression.clone(),
                snapshot.expression_switch,
                snapshot.expression_ranges.clone(),
            ));
        }
        let switch = snapshot.expression_switch;
        let set = |view| vec![switch_request(slot, view)];
        match request {
            R::Link {
                pedal,
                target,
                linked,
            } => vec![link(slot, ExpressionLink { pedal, target }, linked)],
            R::SwitchOn(switch_on) => set(ExpressionSwitchView {
                switch_on,
                ..switch
            }),
            R::InvertRange(inverted) => set(ExpressionSwitchView { inverted, ..switch }),
            R::SwitchDelay(delay_ms) => set(ExpressionSwitchView { delay_ms, ..switch }),
            R::LatchEmulation(latch) => set(ExpressionSwitchView { latch, ..switch }),
            R::Range {
                parameter,
                min,
                max,
            } => vec![range_request(
                slot,
                ExpressionRangeView {
                    parameter,
                    min,
                    max,
                },
            )],
            R::Save => {
                self.expression = None;
                Vec::new()
            }
            R::Revert => {
                let Some((_, links, before, ranges)) = self.expression.take() else {
                    return Vec::new();
                };
                let mut requests: Vec<_> = snapshot
                    .expression
                    .iter()
                    .filter(|now| !links.contains(now))
                    .map(|now| link(slot, *now, false))
                    .chain(links.iter().map(|before| link(slot, *before, true)))
                    .collect();
                if before != switch {
                    requests.push(switch_request(slot, before));
                }
                // Ranges after the links: a relink keeps the stored range (`0x170110`).
                requests.extend(
                    ranges
                        .into_iter()
                        .filter(|range| !snapshot.expression_ranges.contains(range))
                        .map(|range| range_request(slot, range)),
                );
                requests
            }
        }
    }
}

fn stored_slot(slot: Slot) -> tetrad_preset::Slot {
    tetrad_preset::Slot {
        row: slot.row.index(),
        column: slot.column.index(),
    }
}

/// The stored block at `slot` and its bypass record.
fn stored_record(
    preset: &tetrad_preset::Preset,
    slot: Slot,
) -> Option<(Box<tetrad_preset::Model>, Box<tetrad_preset::ColBypass>)> {
    let model = preset.block(stored_slot(slot)).ok().flatten()?;
    let bypass = preset
        .bypass
        .get(slot.row.index())?
        .col_bypass
        .get(slot.column.index())?;
    Some((Box::new(model.clone()), Box::new(bypass.clone())))
}

/// Stored links and switch of the block at `position` (`Param.expression`,
/// `bypass_expression`, `expression_bypass_info`; no info shows the dialog default).
pub(super) fn expression_view(
    preset: &tetrad_preset::Preset,
    position: GridPosition,
) -> (
    Vec<ExpressionLink>,
    ExpressionSwitchView,
    Vec<ExpressionRangeView>,
) {
    let stored =
        project::slot_of(position).and_then(|slot| preset.block(stored_slot(slot)).ok().flatten());
    stored.map_or_else(Default::default, |model| {
        let (links, switch, _) = model_expression(model);
        let percent = |value: Option<f32>, default: f32| {
            (value.unwrap_or(default).clamp(0.0, 1.0) * 100.0).round() as u8
        };
        let ranges = links
            .iter()
            .filter_map(|link| match link.target {
                ExpressionTarget::Parameter(index) => {
                    let param = model.params.get(usize::from(index))?;
                    Some(ExpressionRangeView {
                        parameter: index,
                        min: percent(param.expression_min, 0.0),
                        max: percent(param.expression_max, 1.0),
                    })
                }
                ExpressionTarget::Bypass => None,
            })
            .collect();
        (links, switch, ranges)
    })
}

/// MIN RANGE / MAX RANGE in percent as the stored link range 0..=1 (`0x170110`).
fn range_request(slot: Slot, range: ExpressionRangeView) -> Request {
    Request::Action(Action::PresetEdit(preset::PresetEdit::ExpressionRange {
        slot,
        index: u32::from(range.parameter),
        min: f32::from(range.min) / 100.0,
        max: f32::from(range.max) / 100.0,
    }))
}

/// A stored block's links and switch, and whether it stores switch info.
fn model_expression(
    model: &tetrad_preset::Model,
) -> (Vec<ExpressionLink>, ExpressionSwitchView, bool) {
    let pedal = |code: i32| {
        use tetrad_core::performance::expression::Pedal;
        Pedal::from_assignment(code).map(|pedal| match pedal {
            Pedal::Exp1 => ExpressionPedal::Exp1,
            Pedal::Exp2 => ExpressionPedal::Exp2,
        })
    };
    let links = (0u16..)
        .zip(&model.params)
        .filter_map(|(index, param)| {
            Some(ExpressionLink {
                pedal: pedal(param.expression?)?,
                target: ExpressionTarget::Parameter(index),
            })
        })
        .chain(model.bypass_expression.first().and_then(|bypass| {
            Some(ExpressionLink {
                pedal: pedal(bypass.expression)?,
                target: ExpressionTarget::Bypass,
            })
        }))
        .take(MAX_LIST_ITEMS)
        .collect();
    let info = model.expression_bypass_info.first();
    let switch = ExpressionSwitchView {
        switch_on: match info.and_then(|info| BypassSwitchType::from_raw(info.kind)) {
            Some(BypassSwitchType::Stop) => ExpressionSwitch::Stop,
            Some(BypassSwitchType::Switch) => ExpressionSwitch::Switch,
            Some(BypassSwitchType::HeelToe) | None => ExpressionSwitch::HeelToe,
        },
        inverted: info.is_some_and(|info| info.invert),
        delay_ms: info
            .map_or(DEFAULT_BYPASS_DELAY_MS, |info| info.delay_ms)
            .min(u32::from(tetrad_ui::MAX_SWITCH_DELAY_MS)) as u16,
        latch: info.is_some_and(|info| info.latch_emulation),
    };
    (links, switch, info.is_some())
}

fn link(slot: Slot, link: ExpressionLink, linked: bool) -> Request {
    use tetrad_core::performance::expression::Pedal;
    Request::Action(Action::LinkExpression {
        slot,
        parameter: match link.target {
            ExpressionTarget::Parameter(index) => Some(u32::from(index)),
            ExpressionTarget::Bypass => None,
        },
        pedal: match link.pedal {
            ExpressionPedal::Exp1 => Pedal::Exp1,
            ExpressionPedal::Exp2 => Pedal::Exp2,
        },
        linked,
    })
}

fn switch_request(slot: Slot, view: ExpressionSwitchView) -> Request {
    Request::Action(Action::ExpressionSwitch {
        slot,
        info: BypassInfo {
            kind: match view.switch_on {
                ExpressionSwitch::HeelToe => BypassSwitchType::HeelToe,
                ExpressionSwitch::Switch => BypassSwitchType::Switch,
                ExpressionSwitch::Stop => BypassSwitchType::Stop,
            },
            invert: view.inverted,
            delay_ms: u32::from(view.delay_ms),
            latch_emulation: view.latch,
        },
    })
}

/// Requests that turn the live block into `target`: parameter edits for the same model,
/// else a paste (or a removal for an empty target). A same-model target that differs in
/// another scene (a scene-mode value; stock audition writes all eight scenes, `0x1f7b10`)
/// is pasted too: the edits reach the current scene only.
fn restore(
    slot: Slot,
    live: Option<&Instance>,
    target: Option<&Instance>,
    specs: &Specs,
    scene: SceneIndex,
) -> Vec<Request> {
    let layout = |edit| vec![Request::Action(Action::EditLayout(edit))];
    let other_scenes_match = |live: &Instance, target: &Instance| {
        let scene_mode = |index: usize| live.scene_mode.get(index) == Some(&true);
        (0..tetrad_preset::SCENES as u8)
            .filter_map(|other| SceneIndex::new(other).ok())
            .filter(|&other| other != scene)
            .all(|other| {
                let mut params = live.params.iter().zip(&target.params).enumerate();
                params.all(|(index, (a, b))| !scene_mode(index) || a.at(other) == b.at(other))
                    && target.texts.iter().all(|text| {
                        !scene_mode(text.index as usize)
                            || text_at(live, text.index, other)
                                == text_at(target, text.index, other)
                    })
            })
    };
    let (live, target) = match (live, target) {
        (Some(live), Some(target))
            if live.model == target.model && other_scenes_match(live, target) =>
        {
            (live, target)
        }
        (_, Some(target)) => {
            return layout(LayoutEdit::Paste {
                slot,
                block: Box::new(target.clone()),
            });
        }
        (Some(_), None) => return layout(LayoutEdit::Remove(slot)),
        (None, None) => return Vec::new(),
    };
    let site = Site::Slot(slot);
    let Some(spec) = specs.model(live.model.0) else {
        return Vec::new();
    };
    spec.parameters
        .iter()
        .filter_map(|param| {
            let index = param.index;
            if param.kind == ParameterType::String {
                let text = text_at(target, index, scene)?;
                (text_at(live, index, scene) != Some(text)).then(|| Action::SetText {
                    site,
                    index,
                    text: text.to_owned(),
                })
            } else {
                let value = target.params.get(index as usize)?.at(scene);
                let old = live.params.get(index as usize)?.at(scene);
                (value != old).then_some(Action::SetParameter { site, index, value })
            }
        })
        .map(Request::Action)
        .collect()
}

/// A string parameter at `scene`, else its first stored scene (`preset::stored_values`).
fn text_at(block: &Instance, index: u32, scene: SceneIndex) -> Option<&str> {
    let param = block.texts.iter().find(|text| text.index == index)?;
    param
        .scenes
        .get(scene.index())
        .and_then(Option::as_deref)
        .or_else(|| param.scenes.iter().flatten().next().map(String::as_str))
}

/// The rows a user preset stores for a block (`update_model_preset`): values at `scene`
/// with each parameter's EXP link and range, and the bypass switch settings.
/// The requests that load preset `rows` into the block at `slot` (`0x1f7b10`): the values
/// over `original` (same model) or a fresh `model` (a paste), then the EXP rows and `switch`
/// on the stored block the values land in (a guarded write either way).
fn load_rows(
    state: &State,
    slot: Slot,
    model: u32,
    original: Option<&Instance>,
    rows: &[ParamEntry],
    switch: ExpSwitch,
) -> Result<Vec<Request>, String> {
    let (current, specs) = state
        .current
        .as_ref()
        .zip(state.specs.as_ref())
        .ok_or("no loaded preset")?;
    let layout = current.deployment.layout();
    let scene = current.deployment.scene();
    let live = layout.instance(Site::Slot(slot));
    let target = preset::model_preset_block(specs, model, original, rows)
        .ok_or_else(|| format!("model {model} is not in the catalog"))?;
    let mut requests = restore(slot, live, Some(&target), specs, scene);
    let pasted = requests.iter().any(|request| {
        matches!(
            request,
            Request::Action(Action::EditLayout(LayoutEdit::Paste { .. }))
        )
    });
    let (mut stored, bypass) = if pasted {
        preset::pasted_record(current, specs, slot, &target).map_err(|error| error.to_string())?
    } else {
        let (stored, bypass) = stored_record(&current.preset, slot).ok_or("no stored block")?;
        (*stored, *bypass)
    };
    let parameters = specs.model(model).map_or(0, |spec| spec.parameters.len());
    preset::model_preset_expression(&mut stored, parameters, rows, switch)
        .map_err(|error| error.to_string())?;
    let write = Request::Action(Action::RestoreBlock {
        slot,
        model: Box::new(stored),
        bypass: Box::new(bypass),
    });
    if pasted {
        requests.push(write);
    } else {
        requests.insert(0, write);
    }
    Ok(requests)
}

/// The live block's rows with the stored EXP data (Overwrite, Save As, Set as Defaults). Shared
/// with Cortex Control ModelPreset and DefaultParameters CREATE (r16-control MR 7).
pub(in crate::device) fn live_rows(
    current: &super::super::Current,
    slot: Slot,
    live: &Instance,
    scene: SceneIndex,
) -> Result<(ExpSwitch, Vec<ParamEntry>), String> {
    let stored = current
        .preset
        .block(stored_slot(slot))
        .map_err(|error| error.to_string())?;
    Ok(preset_rows(live, stored, scene))
}

/// The current rows of a settings page (Global EQ or I/O preset layout).
fn settings_rows(page: SettingsPresetPage, state: &State) -> Option<Vec<ParamEntry>> {
    match page {
        SettingsPresetPage::GlobalEq => global_eq::preset_rows(state),
        SettingsPresetPage::Io => global_eq::io_preset_rows(state),
    }
}

/// The stock bypass switch of a block without stored switch settings.
pub(in crate::device) fn default_switch() -> ExpSwitch {
    let default = BypassInfo::default();
    ExpSwitch {
        switch_type: i64::from(default.kind as u32),
        inverted: default.invert,
        latch_emulation: default.latch_emulation,
        delay_ms: i64::from(default.delay_ms),
    }
}

/// An unlinked MUTE or BYPASS row: 1 when set.
fn flag_row(index: i64, on: bool) -> ParamEntry {
    ParamEntry {
        index,
        value: if on { "1" } else { "0" }.into(),
        exp: 0,
        exp_trim_max: 1.0,
        exp_trim_min: 0.0,
    }
}

/// Every parameter of `spec` and then MUTE and BYPASS, in index order: `rows` over the catalog
/// defaults, unlinked and off (`0x1f78f4` fills a preset's missing rows the same way).
fn all_parameters(spec: &ModelSpec, rows: &[ParamEntry]) -> Vec<ParamEntry> {
    let scene = SceneIndex::new(0).expect("scene 0");
    let (_, mut all) = preset_rows(&tetrad_engine::new_instance(spec), None, scene);
    let hidden = all.len() as i64;
    all.extend([flag_row(hidden, false), flag_row(hidden + 1, false)]);
    for row in rows {
        if let Some(entry) = usize::try_from(row.index).ok().and_then(|i| all.get_mut(i)) {
            *entry = row.clone();
        }
    }
    all
}

/// One parameter of the stock match (`0x1f7f50`, `0x5ce580`); `param` is `None` for MUTE and
/// BYPASS. Values parse like `QString::toFloat` (0 on bad text); ranges compare as floats.
fn same_parameter(param: Option<&ParamSpec>, live: &ParamEntry, preset: &ParamEntry) -> bool {
    let number = |row: &ParamEntry| row.value.parse::<f32>().unwrap_or(0.0);
    let linked_alike = || {
        live.exp == preset.exp
            && live.exp_trim_max as f32 == preset.exp_trim_max as f32
            && live.exp_trim_min as f32 == preset.exp_trim_min as f32
    };
    let value = match param {
        Some(param) if param.kind == ParameterType::String => live.value == preset.value,
        Some(param) if param.kind == ParameterType::RotarySwitch => {
            let step = |row| param.scale.step_index(number(row)).ok();
            step(live) == step(preset)
        }
        _ => qt_fuzzy_compare(number(live), number(preset)),
    };
    value && (param.is_none() || linked_alike())
}

/// The bypass switch and parameter rows of `live` (display text at `scene`) with the EXP links
/// and ranges of its stored block.
pub(in crate::device) fn preset_rows(
    live: &Instance,
    stored: Option<&tetrad_preset::Model>,
    scene: SceneIndex,
) -> (ExpSwitch, Vec<ParamEntry>) {
    let info = stored.and_then(|model| model.expression_bypass_info.first());
    let default = BypassInfo::default();
    let switch = ExpSwitch {
        switch_type: i64::from(info.map_or(default.kind as u32, |info| info.kind)),
        inverted: info.is_some_and(|info| info.invert),
        latch_emulation: info.is_some_and(|info| info.latch_emulation),
        delay_ms: i64::from(info.map_or(default.delay_ms, |info| info.delay_ms)),
    };
    let params = (0..)
        .zip(&live.params)
        .map(|(index, value)| {
            let param = stored.and_then(|model| model.params.get(index as usize));
            ParamEntry {
                index,
                value: text_at(live, index as u32, scene)
                    .map_or_else(|| value.at(scene).to_string(), str::to_owned),
                exp: i64::from(param.and_then(|param| param.expression).unwrap_or(0)),
                exp_trim_max: f64::from(param.and_then(|p| p.expression_max).unwrap_or(1.0)),
                exp_trim_min: f64::from(param.and_then(|p| p.expression_min).unwrap_or(0.0)),
            }
        })
        .collect();
    (switch, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tetrad_preset::ParamValue;

    /// `0x1f7b10`: an audition stores each row's EXP link and range on the catalog
    /// parameters (the MUTE row is skipped) and the preset's bypass switch.
    #[test]
    fn audition_stores_the_preset_exp_rows_and_switch() {
        let mut model = tetrad_preset::Model::block(1, [0.5, 0.0].map(ParamValue::Float));
        let row = |index, exp| ParamEntry {
            index,
            value: "0.5".into(),
            exp,
            exp_trim_max: 0.75,
            exp_trim_min: 0.25,
        };
        let switch = ExpSwitch {
            switch_type: BypassSwitchType::Stop as i64,
            inverted: true,
            latch_emulation: false,
            delay_ms: 300,
        };
        preset::model_preset_expression(&mut model, 1, &[row(0, 2), row(1, 1)], switch).unwrap();
        let exp = |index: usize| {
            let param = &model.params[index];
            (param.expression, param.expression_min, param.expression_max)
        };
        assert_eq!(exp(0), (Some(2), Some(0.25), Some(0.75)));
        assert_eq!(exp(1), (Some(0), Some(0.0), Some(1.0)));
        let info = &model.expression_bypass_info[..];
        assert_eq!(
            info.iter()
                .map(|info| (info.kind, info.invert, info.delay_ms))
                .collect::<Vec<_>>(),
            [(BypassSwitchType::Stop as u32, true, 300)]
        );
    }
}

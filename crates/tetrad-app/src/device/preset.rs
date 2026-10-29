//! Preset path (owner: 2B-dsp): catalog from the DSP key, the last-preset lookup in the stock
//! library, tetrad-preset migration, the tetrad-engine compile into a [`Deployment`], live edits
//! that keep the preset and the deployment together, and the stock save.
//!
//! Cabinet models (categories 12/21) store the parameter list of the model they clone plus
//! MUTE (22 values for 2 own XML parameters); tetrad-catalog resolves clones (`0x5507c4`,
//! d-engine §1), so [`SpecsCatalog`] describes them like any other model.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use tetrad_catalog::{Catalog, Specs, ids};
use tetrad_core::Version;
use tetrad_core::performance::expression::{
    BypassInfo, BypassSwitchType, DEFAULT_BYPASS_DELAY_MS, Pedal,
};
use tetrad_core::performance::stomp::{NON_MOMENTARY_MODEL, auto_assign_stomp};
use tetrad_core::performance::{
    AssignmentType, BypassOverwriteMode, ChangeSource, Footswitch, StompAssignment, StompIndex,
};
use tetrad_dsp_host::responses::KeyMaterial;
use tetrad_engine::{
    AbsentScalars, CompileOptions, Deployment, EditError, GateUpdate, Globals, ImportedLayout,
    Instance, Layout, ModelId, Row, SceneIndex, Site, Slot, StoredMasks, Update, import_layout,
    new_instance,
};
use tetrad_library::sidecar::{Creator, Instrument, PresetSave, Sidecar, utc_date};
use tetrad_library::stock::{
    ItemList, ReadOnlyFolder, Setlist, StaleSearchIndex, StockLibrary, StockSave,
};
use tetrad_preset::{
    CatalogModel, CatalogParam, ColBypass, MigrationReport, Model, ModelCatalog, Param, ParamValue,
    Preset, PresetDocument, SaveStamp, SceneValue, SplitterType, StompRole, TEMPO_MODEL_ID,
    community,
};
use tetrad_services::cloud::session::Claims;
use tetrad_settings::tuner::TempoDefaults;
use tetrad_storage::media::Collection;
use tetrad_storage::stock::{ExpSwitch, ParamEntry, StockDb};
use tetrad_ui::PreferredInstrument;

use super::actions::LayoutEdit;
use super::services::{Licences, PluginStatus, PresetFile};
use super::{Current, PresetRef};

/// Looper category: momentary save inversion (`0x51898c`) and MUTE skip it.
const LOOPER_CATEGORY: u32 = 27;
/// Presets below this modified version run the stock parameter import again on load.
const IMPORT_GATE_END: Version = Version {
    major: 3,
    minor: 0,
    patch: 0,
};

#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    #[error("not supported yet: {0}")]
    Unsupported(&'static str),
    #[error("{0} is not UTF-8")]
    Text(&'static str),
    #[error(transparent)]
    Catalog(#[from] tetrad_catalog::CatalogError),
    #[error(transparent)]
    Spec(#[from] tetrad_catalog::SpecError),
    #[error(transparent)]
    Preset(#[from] tetrad_preset::Error),
    #[error(transparent)]
    Library(#[from] tetrad_library::Error),
    #[error(transparent)]
    Import(#[from] tetrad_engine::ImportError),
    #[error(transparent)]
    Deploy(#[from] tetrad_engine::DeployError),
    #[error("setlist {0:?} is not in the library")]
    Setlist(String),
    /// Stock "%1 is full" (`0xa17508`).
    #[error("{0} is full")]
    SetlistFull(String),
}

/// The resolved catalog and the decrypted `ModelRepo.xml` it came from (Cortex Control sends
/// the plain bytes).
pub struct LoadedCatalog {
    pub specs: Specs,
    pub plain: Vec<u8>,
}

/// `ModelRepo.xml` is encrypted with the `0x24` key (VA `0x52913c`, the NDSP file scheme);
/// `moduleReport.xml` arrives decrypted from the platform. Decrypts once.
pub fn load_catalog(
    model_repo: &[u8],
    module_report: &[u8],
    repo_key: &KeyMaterial,
) -> Result<LoadedCatalog, PresetError> {
    let plain = qc_format::crypto::crypt(model_repo, repo_key.as_bytes());
    let models = std::str::from_utf8(&plain).map_err(|_| PresetError::Text("ModelRepo.xml"))?;
    let report =
        std::str::from_utf8(module_report).map_err(|_| PresetError::Text("moduleReport.xml"))?;
    let specs = Specs::resolve(&Catalog::parse(models, report)?)?;
    Ok(LoadedCatalog { specs, plain })
}

/// Device 25000 (tempo) parameter defaults for `DeviceSettings::migrate_on_boot`.
pub fn tempo_defaults(specs: &Specs) -> Option<TempoDefaults> {
    let tempo = specs.models.get(&TEMPO_MODEL_ID)?;
    let params = tempo
        .parameters
        .iter()
        .map(|param| Some((u16::try_from(param.index).ok()?, param.default_normalized())))
        .collect::<Option<_>>()?;
    Some(TempoDefaults { params })
}

/// tetrad-catalog as the tetrad-preset model catalog, for any catalog (tools, tests). It is never
/// complete: a model it lacks stays in the preset unchanged.
pub struct SpecsCatalog<'a>(pub &'a Specs);

impl ModelCatalog for SpecsCatalog<'_> {
    fn model(&self, model_id: u32) -> Option<CatalogModel> {
        let spec = self.0.models.get(&model_id)?;
        let params = spec.parameters.iter().map(|param| CatalogParam {
            default: match &param.default {
                Some(tetrad_catalog::ParamDefault::Text(text)) => ParamValue::String(text.clone()),
                _ => ParamValue::Float(param.default_normalized()),
            },
            expression_assignable: param.exp_assignable.unwrap_or(true),
        });
        // MUTE follows the XML parameters; BYPASS is not stored (d-engine §2).
        let mute = CatalogParam {
            default: ParamValue::Float(0.0),
            expression_assignable: false,
        };
        Some(CatalogModel {
            params: params.chain([mute]).collect(),
            sidechain_parameter: spec
                .sidechain
                .as_ref()
                .map(|sidechain| sidechain.source_param as usize),
        })
    }

    /// `0x6e59f8` → `0x6e4148`: TYPE = step size × selector index (`0x5cc9e0`) of the 10004
    /// TYPE parameter. Legacy 10002/10000/10001 select 0/1/2.
    fn splitter_type(&self, selector: SplitterType) -> Option<f32> {
        let index = match selector {
            SplitterType::Type0 => 0.0,
            SplitterType::Type1 => 1.0,
            SplitterType::Type2 => 2.0,
        };
        let scale = &self.0.models.get(&ids::SPLITTER)?.parameters.first()?.scale;
        Some(scale.step_size() * index)
    }

    /// `0x0064b8f8(bpm, min, max, skew, 1)` with the 25000 TEMPO parameter law.
    fn tempo_value(&self, bpm: u32) -> Option<f32> {
        let law = self
            .0
            .models
            .get(&TEMPO_MODEL_ID)?
            .parameters
            .first()?
            .scale
            .law;
        Some(law.to_normalized(bpm as f32))
    }
}

/// The device catalog: the `ModelRepo.xml` the DSP key decrypted at boot, which is what
/// `State::specs` holds. It is the full stock catalog, so unknown grid models are emptied as
/// stock does (`0x680678` → `0x680bf4`).
pub struct DeviceCatalog<'a>(pub &'a Specs);

impl ModelCatalog for DeviceCatalog<'_> {
    fn model(&self, model_id: u32) -> Option<CatalogModel> {
        SpecsCatalog(self.0).model(model_id)
    }

    fn splitter_type(&self, selector: SplitterType) -> Option<f32> {
        SpecsCatalog(self.0).splitter_type(selector)
    }

    fn tempo_value(&self, bpm: u32) -> Option<f32> {
        SpecsCatalog(self.0).tempo_value(bpm)
    }

    fn is_complete(&self) -> bool {
        true
    }
}

/// A setlist by key. Inferred: the key is the setlist directory path (or its name), and no
/// key means the first setlist in `user_preset_folders` order.
pub(super) fn setlist(library: &StockLibrary, key: Option<&str>) -> Result<Setlist, PresetError> {
    let setlists = library.setlists()?;
    match key {
        Some(key) => setlists
            .into_iter()
            .find(|setlist| setlist.path == key || setlist.name == key)
            .ok_or_else(|| PresetError::Setlist(key.to_owned())),
        None => setlists
            .into_iter()
            .next()
            .ok_or(PresetError::Unsupported("a library without setlists")),
    }
}

/// The `.pb` device path of one setlist slot (`LastPreset` at boot). `None` is an empty slot.
/// `StockLibrary::slots` applies the safe part of the stock setlist repair (`0x70dbc0`).
pub fn last_preset_path(
    library: &StockLibrary,
    setlist_key: Option<&str>,
    index: usize,
) -> Result<Option<String>, PresetError> {
    let setlist = setlist(library, setlist_key)?;
    let slots = library.slots(&setlist.name)?;
    Ok(slots.get(index).cloned().flatten())
}

/// The slot of setlist `setlist_key` (`None`: the first) that names `path`.
pub fn slot_of(
    library: &StockLibrary,
    setlist_key: Option<&str>,
    path: &str,
) -> Result<Option<usize>, PresetError> {
    let setlist = setlist(library, setlist_key)?;
    let slots = library.slots(&setlist.name)?;
    Ok(slots.iter().position(|slot| slot.as_deref() == Some(path)))
}

/// A preset ready for the DSP.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub preset: Preset,
    pub migration: MigrationReport,
    /// What the import filled in, padded or sanitized.
    pub imported: ImportedLayout,
    pub deployment: Deployment,
    /// A full [`Update::Redeploy`] for the first preset; later loads follow the stock
    /// reuse-or-redeploy rule (`Deployment::load`).
    pub update: Update,
}

/// (O) Issue #17: the models of plugins with no licence at all load no DSP code. Their blocks
/// stay on the grid with the lock, and a new licence reloads the preset (`licence_repush`).
/// The DSP never acknowledges the run-list clear of an unlicensed Petrucci Wah, in stock too,
/// and the unit restarts. A licensed plugin waiting for its activation still loads.
pub fn dormant_models(specs: &Specs, licences: &Licences) -> BTreeSet<ModelId> {
    (specs.models.values())
        .filter(|model| {
            (model.plugin_id)
                .is_some_and(|plugin| licences.status(plugin) == PluginStatus::Unlicensed)
        })
        .map(|model| ModelId(model.id))
        .collect()
}

/// Stock load path: migrate (`0x681d44`), import, route, fit (trying the stored recompile
/// masks first) and compile against the `previous` deployment, in the preset's default
/// scene. Inferred: both global features (input gate, Global EQ) are requested; the fit may
/// inhibit them. The blocks of `dormant` models load no DSP code ([`dormant_models`]).
pub fn prepare(
    mut preset: Preset,
    specs: &Specs,
    dynamic_delay_compensation: bool,
    previous: Option<&Deployment>,
    dormant: BTreeSet<ModelId>,
) -> Result<Prepared, PresetError> {
    // Before the migration, which would empty a slot with an unknown model ID.
    restore_community(&mut preset, specs);
    let migration = preset.migrate(&DeviceCatalog(specs))?;
    let document = PresetDocument::parse(&preset.to_bytes(), tetrad_preset::Limits::default())?;
    let imported = import_layout(&document.summary()?, specs, AbsentScalars::Proto3Default)?;
    let scene = preset
        .default_scene
        .and_then(|scene| u8::try_from(scene).ok())
        .and_then(|scene| SceneIndex::new(scene).ok())
        .unwrap_or(SceneIndex::new(0).expect("scene 0"));
    let options = CompileOptions {
        globals: Globals {
            input_gate: true,
            global_eq: true,
        },
        ddc: dynamic_delay_compensation,
    };
    let layout = imported.layout.clone();
    let stored = StoredMasks::from_preset(&preset);
    let (deployment, update) = match previous {
        None => Deployment::new_stored(layout, specs, options, scene, stored, dormant)?,
        Some(previous) => {
            // A failed compile keeps the previous preset (`0x585464`). The new preset plays
            // its default scene; the load pushes every value, so the scene's own push is not
            // needed.
            let mut deployment = previous.clone();
            let _ = deployment.select_scene(scene);
            let update = deployment.load_stored(specs, layout, stored, dormant)?;
            (deployment, update)
        }
    };
    Ok(Prepared {
        preset,
        migration,
        imported,
        deployment,
        update,
    })
}

/// (O) Puts the installed community blocks of `preset` back in their slots
/// (`tetrad_preset::community`). A block that is not installed keeps its stock placeholder,
/// and so does every block of a record that does not decode: the preset still loads.
pub fn restore_community(preset: &mut Preset, specs: &Specs) -> Vec<community::Missing> {
    community::restore(preset, |key| specs.community_id(key)).unwrap_or_default()
}

/// (O) `preset` as it leaves Tetrad (a preset file, a full Cortex Control preset): each
/// community block becomes its stock placeholder at its defaults, and a record keeps the block
/// for the next load. Stock CorOS loads and plays the placeholder.
pub fn exported(preset: &Preset, specs: &Specs) -> Result<Preset, PresetError> {
    let mut exported = preset.clone();
    community::stash(&mut exported, |model| {
        let community = specs.model(model)?.community.as_ref()?;
        let placeholder = specs.model(community.placeholder)?;
        let defaults = tetrad_engine::new_instance(placeholder);
        let scene = SceneIndex::new(0).expect("scene 0");
        Some(community::Stash {
            key: community.key.clone(),
            placeholder: Model::block(placeholder.id, stored_values(&defaults, scene)),
        })
    })?;
    Ok(exported)
}

/// Reads one library preset, or the stock new-preset shape for an empty slot (inferred).
pub fn read(
    library: &StockLibrary,
    path: Option<&str>,
    specs: &Specs,
) -> Result<Preset, PresetError> {
    match path {
        Some(path) => Ok(Preset::try_from(&library.load(path)?.document)?),
        None => Ok(Preset::new_default(
            tetrad_firmware::running().version,
            &DeviceCatalog(specs),
        )?),
    }
}

/// The stored model of a site: grid block, splitter (`combined_splitter` first, as the load
/// reads it), mixer or lane control.
fn stored_model(preset: &mut Preset, site: Site) -> Result<&mut Model, PresetError> {
    let missing = PresetError::Unsupported("a site without a stored model");
    let model = match site {
        Site::Slot(slot) => {
            return Ok(preset.block_mut(tetrad_preset::Slot {
                row: slot.row.index(),
                column: slot.column.index(),
            })?);
        }
        Site::Splitter(pair) => preset
            .chains
            .get_mut(pair.master().index())
            .and_then(|chain| {
                if chain.combined_splitter.is_empty() {
                    chain.splitter.first_mut()
                } else {
                    chain.combined_splitter.first_mut()
                }
            }),
        Site::Mixer(pair) => preset
            .chains
            .get_mut(pair.master().index())
            .and_then(|chain| chain.mixer.first_mut()),
        Site::OutputControl(row) => preset
            .chains
            .get_mut(row.index())
            .and_then(|chain| chain.output_control.first_mut()),
        Site::InputControl(row) => preset
            .chains
            .get_mut(row.index())
            .and_then(|chain| chain.input_control.first_mut()),
    };
    model.ok_or(missing)
}

/// A live parameter edit (`0x51e4d8`): the deployment gives the DSP ops; the preset keeps
/// the value for the save. Both follow the stored scene-mode rule. A grid block's MUTE (the
/// index after its XML parameters) is on or off (above 0.5). Nothing changes when either
/// side refuses.
pub fn set_parameter(
    current: &mut Current,
    specs: &Specs,
    site: Site,
    index: u32,
    value: f32,
) -> Result<Update, PresetError> {
    let scene = current.deployment.scene().index();
    let mute = match (site, current.deployment.layout().instance(site)) {
        (Site::Slot(slot), Some(block)) if block.params.len() == index as usize => Some(slot),
        _ => None,
    };
    // Only the edited parameter is copied: it is stored after both sides took the edit.
    let stored = stored_model(&mut current.preset, site)?.param_mut(index as usize)?;
    let mut param = stored.clone();
    let update = match mute {
        Some(slot) => {
            let muted = value > 0.5;
            let scene_mode = param.scene_mode == Some(true);
            param.set_value(scene, ParamValue::Float(f32::from(u8::from(muted))))?;
            current
                .deployment
                .set_mute(specs, slot, muted, scene_mode)?
        }
        None => {
            param.set_value(scene, ParamValue::Float(value))?;
            current
                .deployment
                .set_parameter(specs, site, index, value)?
        }
    };
    *stored = param;
    Ok(update)
}

/// A driver scene-hook store (`0x526bd4` → `0x2e0830(…, 0)` → `0x2e8160`): only the
/// recalled scene's slot, in or out of scene mode. The hook already sent the DSP write; the
/// deployment follows only in scene mode (inferred: out of scene mode it keeps the shared
/// value).
pub fn store_scene_slot(
    current: &mut Current,
    specs: &Specs,
    site: Site,
    index: u32,
    value: f32,
) -> Result<(), PresetError> {
    let scene = current.deployment.scene().index();
    let mut preset = current.preset.clone();
    let param = stored_model(&mut preset, site)?.param_mut(index as usize)?;
    if param.scene_mode == Some(true) {
        param.set_value(scene, ParamValue::Float(value))?;
        current
            .deployment
            .set_parameter(specs, site, index, value)?;
    } else {
        let slot = param.values.get_mut(scene).ok_or(PresetError::Unsupported(
            "a parameter without eight scene values",
        ))?;
        *slot = tetrad_preset::SceneValue {
            value: Some(ParamValue::Float(value)),
            ..tetrad_preset::SceneValue::default()
        };
    }
    current.preset = preset;
    Ok(())
}

/// Expression uses the normal live ops without writing either scene store (`0x520074`).
pub fn set_live_parameter(
    current: &Current,
    specs: &Specs,
    site: Site,
    index: u32,
    value: f32,
) -> Result<Update, PresetError> {
    let mut deployment = current.deployment.clone();
    Ok(deployment.set_parameter(specs, site, index, value)?)
}

pub fn set_live_bypass(
    current: &Current,
    specs: &Specs,
    site: Site,
    bypassed: bool,
) -> Result<Update, PresetError> {
    let mut deployment = current.deployment.clone();
    Ok(deployment.set_bypass(specs, site, bypassed)?)
}

/// Expression gate edits use the normal gate path without changing scene storage.
pub fn set_live_input_gate(
    current: &Current,
    specs: &Specs,
    row: Row,
    index: u32,
    value: f32,
) -> Result<GateUpdate, PresetError> {
    let mut deployment = current.deployment.clone();
    Ok(deployment.set_input_gate(specs, row, index, value)?)
}

pub fn stored_parameter(preset: &Preset, site: Site, index: u32, scene: usize) -> Option<f32> {
    match stored_value(preset, site, index, scene)? {
        ParamValue::Float(value) => Some(*value),
        _ => None,
    }
}

/// The stored value of one parameter in one scene, of any kind.
pub fn stored_value(preset: &Preset, site: Site, index: u32, scene: usize) -> Option<&ParamValue> {
    stored_param(preset, site, index)?.value(scene)
}

/// One stored parameter of a grid block, lane, input gate or pair junction.
pub fn stored_param(preset: &Preset, site: Site, index: u32) -> Option<&tetrad_preset::Param> {
    stored_site_model(preset, site)?.params.get(index as usize)
}

/// The stored model of a site, read-only (see `stored_model`).
pub fn stored_site_model(preset: &Preset, site: Site) -> Option<&Model> {
    match site {
        Site::Slot(slot) => preset
            .chains
            .get(slot.row.index())?
            .models
            .get(slot.column.index()),
        Site::InputControl(row) => preset.chains.get(row.index())?.input_control.first(),
        Site::OutputControl(row) => preset.chains.get(row.index())?.output_control.first(),
        Site::Splitter(pair) => {
            let chain = preset.chains.get(pair.master().index())?;
            chain
                .combined_splitter
                .first()
                .or_else(|| chain.splitter.first())
        }
        Site::Mixer(pair) => preset.chains.get(pair.master().index())?.mixer.first(),
    }
}

/// A live input gate edit (model 28000, `0x51e4d8` case 0x6d60): the deployment gives the
/// router ops and gate messages (`Dsp::apply_gate`); the preset keeps the value.
pub fn set_input_gate(
    current: &mut Current,
    specs: &Specs,
    row: Row,
    index: u32,
    value: f32,
) -> Result<GateUpdate, PresetError> {
    let scene = current.deployment.scene().index();
    let stored =
        stored_model(&mut current.preset, Site::InputControl(row))?.param_mut(index as usize)?;
    let mut param = stored.clone();
    param.set_value(scene, ParamValue::Float(value))?;
    let update = current
        .deployment
        .set_input_gate(specs, row, index, value)?;
    *stored = param;
    Ok(update)
}

/// A live string edit (cab `ir selector`, IR Loader `IR PATH`, capture `file_name`): the
/// deployment gives a text op that reloads the block's IR or capture (`Dsp::apply`); the
/// preset keeps the text.
pub fn set_text(
    current: &mut Current,
    specs: &Specs,
    site: Site,
    index: u32,
    text: &str,
) -> Result<Update, PresetError> {
    let scene = current.deployment.scene().index();
    let mut preset = current.preset.clone();
    stored_model(&mut preset, site)?
        .param_mut(index as usize)?
        .set_value(scene, ParamValue::String(text.to_owned()))?;
    let update = current.deployment.set_text(specs, site, index, text)?;
    current.preset = preset;
    Ok(update)
}

/// A layout edit that is compiled but not committed: deploy [`Self::deployment`] with
/// [`Self::update`], then [`Self::commit`] only when the deploy succeeds, so a refused
/// deploy (`WontFit`) leaves `State` on the layout the DSP still runs.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutChange {
    pub update: Update,
    preset: Box<Preset>,
    deployment: Box<Deployment>,
    /// The undo frame of the edit, kept when the deploy commits.
    undo: Option<PendingFrame>,
}

impl LayoutChange {
    pub fn preset(&self) -> &Preset {
        &self.preset
    }

    /// Records `frame` with [`History::commit`] once this change commits.
    pub fn record_undo(&mut self, frame: PendingFrame) {
        self.undo = Some(frame);
    }

    /// The frame for [`History::commit`] after the deploy commits.
    pub fn take_undo(&mut self) -> Option<PendingFrame> {
        self.undo.take()
    }

    pub fn deployment(&self) -> &Deployment {
        &self.deployment
    }

    pub fn commit(self, current: &mut Current) {
        current.deployment = *self.deployment;
        current.preset = *self.preset;
    }
}

/// A structural grid edit (`Deployment::edit`, core reuse; a failed compile keeps the
/// previous preset, `0x585464`) mirrored into the stored preset so a save keeps it. Added,
/// replaced, reset and pasted blocks are stored from the engine instance at the current scene
/// (a pasted block's scene-mode values collapse to that scene, owned). Ports go to
/// `Chain.in_portid`/`out_portid`, split and mix points to the master chain's
/// `SplitControlPoints` (−1 = none; setter `0x51a090` → `0x2e2fb4`). A point placed on a pair
/// without its junction model gives it the new-preset default ([`Preset::default_junction`]).
/// Nothing changes when either side refuses.
pub fn prepare_layout_edit(
    current: &Current,
    specs: &Specs,
    edit: LayoutEdit,
) -> Result<LayoutChange, PresetError> {
    prepare_layout_edits(current, specs, vec![edit])
}

/// Several structural edits in one compile, in order (a Cortex Control Grid UPDATE sets a
/// row's two ports or a pair's split and mix points at once). See [`prepare_layout_edit`].
/// New junctions take the catalog default ([`prepare_junction_edits`] for user defaults).
pub fn prepare_layout_edits(
    current: &Current,
    specs: &Specs,
    edits: Vec<LayoutEdit>,
) -> Result<LayoutChange, PresetError> {
    prepare_junction_edits(current, specs, edits, &JunctionDefaults::default())
}

/// User default blocks of a new splitter (10004) and mixer (11000). Stock builds a junction
/// through the model factory `0x519cc0`, whose `0x668354` applies the user default model
/// preset (`0x1f46c4` → `0x1f7e1c`: `0x1f57b8` row, `0x1f7b10` values over a fresh block).
#[derive(Debug, Clone, Default)]
pub struct JunctionDefaults {
    pub splitter: Option<Instance>,
    pub mixer: Option<Instance>,
}

impl JunctionDefaults {
    /// The `default_model_presets` rows of both models in `zenui.db`. A factory default gives
    /// none: `factory.db` 4.1.0 has no model presets of 10004 or 11000.
    pub fn read(specs: &Specs, database: &StockDb) -> tetrad_storage::Result<Self> {
        let view = database.view();
        let defaults = view.default_model_presets()?;
        let block = |model: u32| -> tetrad_storage::Result<Option<Instance>> {
            let Some(default) = defaults
                .iter()
                .find(|default| default.model == i64::from(model) && !default.factory)
            else {
                return Ok(None);
            };
            let rows = view.model_preset_params(default.preset_id)?;
            Ok(model_preset_block(specs, model, None, &rows))
        };
        Ok(Self {
            splitter: block(ids::SPLITTER)?,
            mixer: block(ids::MIXER)?,
        })
    }

    fn get(&self, mixer: bool) -> Option<&Instance> {
        if mixer { &self.mixer } else { &self.splitter }.as_ref()
    }
}

/// [`prepare_layout_edits`] where a split or mix point on a pair without its junction takes
/// the user default of `defaults`, else the catalog default.
pub fn prepare_junction_edits(
    current: &Current,
    specs: &Specs,
    edits: Vec<LayoutEdit>,
    defaults: &JunctionDefaults,
) -> Result<LayoutChange, PresetError> {
    let catalog = DeviceCatalog(specs);
    let scene = current.deployment.scene();
    let mut preset = current.preset.clone();
    let mut refresh = Vec::new();
    for edit in &edits {
        match edit {
            LayoutEdit::Add { slot, .. }
            | LayoutEdit::Replace { slot, .. }
            | LayoutEdit::Reset(slot)
            | LayoutEdit::Paste { slot, .. } => refresh.push(*slot),
            LayoutEdit::Move { from, to } => {
                preset.move_block(stored_slot(*from), stored_slot(*to), &catalog)?;
            }
            LayoutEdit::Moves(moves) => {
                let moves: Vec<_> = (moves.iter())
                    .map(|&(from, to)| (stored_slot(from), stored_slot(to)))
                    .collect();
                preset.move_blocks(&moves, &catalog)?;
            }
            LayoutEdit::Remove(slot) => {
                preset.remove_block(stored_slot(*slot), &catalog)?;
            }
            LayoutEdit::Input { row, port } => {
                chain_mut(&mut preset, row.index())?.in_portid = Some(*port as u32);
            }
            LayoutEdit::Output { row, port } => {
                chain_mut(&mut preset, row.index())?.out_portid = Some(*port as u32);
            }
            LayoutEdit::Split { pair, column } | LayoutEdit::Mix { pair, column } => {
                let mix = matches!(edit, LayoutEdit::Mix { .. });
                if column.is_some()
                    && preset.default_junction(pair.master().index(), mix, &catalog)?
                    && let Some(block) = defaults.get(mix)
                {
                    store_junction(&mut preset, *pair, mix, block, scene)?;
                }
                let point = column.map_or(-1, |column| column.index() as i32);
                let points = split_points(&mut preset, *pair)?;
                if mix {
                    points.mix = point;
                } else {
                    points.split = point;
                }
            }
        }
    }
    let mut deployment = current.deployment.clone();
    let update = deployment.edit(specs, |layout| {
        edits
            .into_iter()
            .try_for_each(|edit| edit_engine(layout, specs, edit, defaults))
    })?;
    for slot in refresh {
        let instance = deployment
            .layout()
            .instance(Site::Slot(slot))
            .ok_or(PresetError::Unsupported("an edited slot without a block"))?;
        store_instance(&mut preset, &catalog, slot, instance, deployment.scene())?;
    }
    Ok(LayoutChange {
        update,
        preset: Box::new(preset),
        deployment: Box::new(deployment),
        undo: None,
    })
}

/// The stored values of a junction that [`Preset::default_junction`] just made, from its user
/// default block at `scene` (each default value fills all eight scenes).
fn store_junction(
    preset: &mut Preset,
    pair: tetrad_engine::Pair,
    mixer: bool,
    block: &Instance,
    scene: SceneIndex,
) -> Result<(), PresetError> {
    let chain = chain_mut(preset, pair.master().index())?;
    let models = if mixer {
        &mut chain.mixer
    } else {
        &mut chain.combined_splitter
    };
    let model =
        (models.first_mut()).ok_or(PresetError::Unsupported("a pair without its junction"))?;
    model.params = stored_values(block, scene)
        .into_iter()
        .map(tetrad_preset::Param::new)
        .collect();
    Ok(())
}

fn chain_mut(preset: &mut Preset, row: usize) -> Result<&mut tetrad_preset::Chain, PresetError> {
    preset
        .chains
        .get_mut(row)
        .ok_or(PresetError::Unsupported("a preset without that chain"))
}

/// The pair's `SplitControlPoints` on its master chain; a missing entry starts as (−1, −1).
fn split_points(
    preset: &mut Preset,
    pair: tetrad_engine::Pair,
) -> Result<&mut tetrad_preset::SplitControlPoints, PresetError> {
    let points = &mut chain_mut(preset, pair.master().index())?.split_control_points;
    if points.is_empty() {
        points.push(tetrad_preset::SplitControlPoints {
            split: -1,
            mix: -1,
            ..Default::default()
        });
    }
    Ok(&mut points[0])
}

/// The edits of a grid drop of the block at `from` on `to` ([`Layout::plan_drop`]): the moves,
/// then the pair's split and mix points when the drop changes them (empty otherwise). `None`
/// when stock refuses the drop.
pub fn drop_edits(
    layout: &Layout,
    from: Slot,
    to: Slot,
    side: tetrad_engine::DropSide,
) -> Option<(LayoutEdit, Vec<LayoutEdit>)> {
    let drop = layout.plan_drop(from, to, side)?;
    let points = drop.points.map_or_else(Vec::new, |points| {
        vec![
            LayoutEdit::Split {
                pair: points.pair,
                column: points.split,
            },
            LayoutEdit::Mix {
                pair: points.pair,
                column: points.mix,
            },
        ]
    });
    Some((LayoutEdit::Moves(drop.moves), points))
}

/// Stores an added, replaced or pasted instance: a fresh stored model of its values at
/// `scene` (an existing slot keeps its bypass record) and its bypass at `scene`.
fn store_instance(
    preset: &mut Preset,
    catalog: &DeviceCatalog<'_>,
    slot: Slot,
    instance: &Instance,
    scene: SceneIndex,
) -> Result<(), PresetError> {
    let model = Model::block(instance.model.0, stored_values(instance, scene));
    let at = stored_slot(slot);
    if preset.block(at)?.is_some() {
        preset.replace_model(at, model)?;
    } else {
        preset.insert_block(at, model, catalog)?;
    }
    preset
        .block_bypass_mut(at)?
        .set_bypassed(scene.index(), instance.bypass.at(scene))?;
    Ok(())
}

/// The stored block and bypass record a [`LayoutEdit::Paste`] of `block` at `slot` writes.
pub fn pasted_record(
    current: &Current,
    specs: &Specs,
    slot: Slot,
    block: &Instance,
) -> Result<(Model, tetrad_preset::ColBypass), PresetError> {
    let mut preset = current.preset.clone();
    let scene = current.deployment.scene();
    store_instance(&mut preset, &DeviceCatalog(specs), slot, block, scene)?;
    let at = stored_slot(slot);
    let model = preset.block(at)?.cloned();
    let model = model.ok_or(PresetError::Unsupported("a pasted slot without a block"))?;
    Ok((model, preset.block_bypass_mut(at)?.clone()))
}

/// UTF-16 units of the capture key at the start of a capture parameter 5 text.
pub const CAPTURE_KEY_UNITS: usize = 64;
/// Neural Capture models (`0x7c2da4` tests `hash - 14000 < 2`).
pub const CAPTURE_MODELS: [u32; 2] = [14_000, 14_001];

/// The capture key of a capture parameter 5 text (key, then name; `0x6f7018`): its first 64
/// UTF-16 units (`QString::left(64)`, `0x517cac`). Owned: a surrogate pair is never split.
pub fn capture_key(value: &str) -> &str {
    let mut units = 0;
    let end = value
        .char_indices()
        .find(|(_, c)| {
            units += c.len_utf16();
            units > CAPTURE_KEY_UNITS
        })
        .map_or(value.len(), |(at, _)| at);
    &value[..end]
}

/// Capture parameter 5 (`file_name`): the capture key, then its name.
const CAPTURE_FILE: u32 = 5;
/// The IR Loader model (`startAddIRLoaderMode` `0x598b10` → `0x597ec8(29001)`).
pub const IR_LOADER: u32 = 29_001;

/// A library or Downloads capture as Add to Grid and the DefaultParameters resync read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureRecord {
    /// The key parameter 5 starts with: the product id, or the `hash` of a Downloads row.
    pub key: String,
    /// 14001 when the metadata `version` is "2" (`0x6f6a40`, record `+0x40`), else 14000.
    pub model: u32,
    /// Parameter 5 in every scene: key, then name (`0x6f7018`).
    pub file_name: String,
    /// The metadata `deviceType` (`0x6f734c`, record `+0x60`): stock writes it into the new
    /// block's model word `[0x38]` (`0x2ed0e0`) before the capture loads.
    pub device: Option<tetrad_ui::CaptureDevice>,
}

/// The capture of `key` (`0x6f6a40`, `0x6f7018`): user products, then Downloads by `id` or
/// `hash` (`0x214b84` turns a Downloads key into its hash). The DefaultParameters resync reads
/// these two stores only (O: the stock resync result for a factory key is not traced); the
/// Add device and Add to Grid builds also read `factory.db` ([`capture_block`]). A Downloads
/// row without a hash is refused (O; stock would use "").
pub fn capture_record(
    view: &tetrad_storage::stock::StockView<'_>,
    cipher: &tetrad_media::factory::DeviceCipher,
    key: &str,
) -> Result<Option<CaptureRecord>, PresetError> {
    use tetrad_library::stock::{CaptureSource, find_capture};
    let (key, metadata) = match find_capture(view, None, key)? {
        Some(CaptureSource::User(product)) => (product.id, product.metadata),
        Some(CaptureSource::Download(download)) => (
            download
                .hash
                .filter(|hash| !hash.is_empty())
                .ok_or(PresetError::Unsupported(
                    "a Downloads capture without a hash",
                ))?,
            download.metadata,
        ),
        Some(CaptureSource::Factory(_)) | None => return Ok(None),
    };
    capture_record_of(key, metadata, cipher).map(Some)
}

/// The record of a stored capture: the model by metadata `version` (`0x6f6a40`) and
/// parameter 5 = key + name (`0x6f7018`).
fn capture_record_of(
    key: String,
    metadata: Option<Vec<u8>>,
    cipher: &tetrad_media::factory::DeviceCipher,
) -> Result<CaptureRecord, PresetError> {
    use tetrad_capture::stored::{CaptureMetadata, RecordVersion};
    let mut plain = metadata.unwrap_or_default();
    cipher.apply(&mut plain);
    let metadata = CaptureMetadata::from_json(&plain)
        .map_err(|_| PresetError::Unsupported("capture metadata that does not read"))?;
    let device = (tetrad_library::stock::media_query::CaptureFacts::from_metadata(&key, &plain)
        .ok())
    .and_then(|facts| facts.device)
    .and_then(|code| tetrad_ui::CaptureDevice::from_code(u8::try_from(code).ok()?));
    let model = match metadata.version {
        RecordVersion::Nc2 => 14_001,
        RecordVersion::V1 => 14_000,
    };
    let file_name = format!("{key}{}", metadata.name);
    Ok(CaptureRecord {
        key,
        model,
        file_name,
        device,
    })
}

/// The capture block of `key` (`0x704530`): a new block of the record's model with the
/// `default_param` rows of its key (`0x66824c`), then parameter 5 = key + name in all eight
/// scenes. The model follows the record, not the block it replaces: a version "1" capture
/// picked for a 14001 block gives a 14000 block (Add device `onCaptureClicked` `0x386024` →
/// `0x704530` → `0x98b9f0`). The record comes from the Captures Library root (`0x765520`,
/// which holds the `factory.db` captures: 2104 = 2062 factory + 42 user rows on the B7 unit)
/// or Downloads (`0x7657f8`), as `0x6f5e20` checks. The same build as BANK UP
/// (`cycle_capture`); I: the user default model preset of the model factory (`0x519cc0` →
/// `0x668354`) is not applied. The record comes back for its `deviceType`.
pub fn capture_block(
    view: &tetrad_storage::stock::StockView<'_>,
    factory: Option<&tetrad_storage::stock::StockView<'_>>,
    specs: &Specs,
    cipher: &tetrad_media::factory::DeviceCipher,
    key: &str,
) -> Result<Option<(Instance, CaptureRecord)>, PresetError> {
    let record = match capture_record(view, cipher, key)? {
        Some(record) => record,
        None => {
            let product = match factory {
                Some(factory) => factory.product(key).map_err(tetrad_library::Error::from)?,
                None => None,
            };
            let capture = tetrad_storage::stock::ProductType::Capture;
            match product.filter(|product| product.product_type == capture) {
                Some(product) => capture_record_of(product.id, product.metadata, cipher)?,
                None => return Ok(None),
            }
        }
    };
    // User `default_param` rows only: `factory.db` has no such table.
    let rows = view
        .default_params(&record.key)
        .map_err(tetrad_library::Error::from)?;
    let mut block = model_preset_block(specs, record.model, None, &rows).ok_or(
        PresetError::Unsupported("a capture model outside the catalog"),
    )?;
    if !set_text_everywhere(&mut block, CAPTURE_FILE, &record.file_name) {
        return Err(PresetError::Unsupported(
            "a capture model without parameter 5",
        ));
    }
    Ok(Some((block, record)))
}

/// The Add to Grid IR block (`0x596c50` IR branch): the catalog defaults of 29001
/// (`0x519cc0`), parameter 2 = the IR key and parameter 22 = its name in all eight scenes
/// (tetrad-media `IR_PATH_PARAMS[0]`, `IR_NAME_PARAMS[0]`).
pub fn ir_block(specs: &Specs, key: &str, name: &str) -> Result<Instance, PresetError> {
    use tetrad_media::block::{IR_NAME_PARAMS, IR_PATH_PARAMS};
    let spec = (specs.model(IR_LOADER)).ok_or(PresetError::Unsupported("the IR Loader model"))?;
    let mut block = new_instance(spec);
    if !(set_text_everywhere(&mut block, IR_PATH_PARAMS[0], key)
        && set_text_everywhere(&mut block, IR_NAME_PARAMS[0], name))
    {
        return Err(PresetError::Unsupported(
            "an IR Loader without its IR parameters",
        ));
    }
    Ok(block)
}

/// A string parameter's text in all eight scenes (the `0x2e8160` loop over scenes 0..8).
fn set_text_everywhere(block: &mut Instance, index: u32, text: &str) -> bool {
    let Some(param) = block.texts.iter_mut().find(|param| param.index == index) else {
        return false;
    };
    param.scenes = vec![Some(text.to_owned()); tetrad_preset::SCENES];
    true
}

/// Review r30 C6: the one instrument text map is tetrad-library's ([`Instrument::text`]).
pub fn preferred_instrument(instrument: Instrument) -> PreferredInstrument {
    match instrument {
        Instrument::NotSet => PreferredInstrument::NotSet,
        Instrument::Guitar => PreferredInstrument::Guitar,
        Instrument::Bass => PreferredInstrument::Bass,
        Instrument::Synth => PreferredInstrument::Synth,
        Instrument::Vocal => PreferredInstrument::Vocal,
        Instrument::Other => PreferredInstrument::Other,
    }
}

/// The reverse of [`preferred_instrument`].
pub fn sidecar_instrument(instrument: PreferredInstrument) -> Instrument {
    match instrument {
        PreferredInstrument::NotSet => Instrument::NotSet,
        PreferredInstrument::Guitar => Instrument::Guitar,
        PreferredInstrument::Bass => Instrument::Bass,
        PreferredInstrument::Synth => Instrument::Synth,
        PreferredInstrument::Vocal => Instrument::Vocal,
        PreferredInstrument::Other => Instrument::Other,
    }
}

/// The block a Virtual Device Preset gives (`0x1f7b10`; `0x1f7e74` for another model): the
/// stored rows over the original block (same model) or the catalog defaults. Each value goes
/// to all eight scenes (`0x2e8160`), so a scene-mode parameter keeps its flag with one value.
/// Stock skips the sidechain source parameter (`+0xec`) and, on a capture (14000/14001),
/// parameter 5 in the scenes: a scene-mode value there stays. Values are normalized text;
/// string parameters (`ir selector`) are text. Rows past the XML parameters (MUTE) are
/// skipped, and so are the rows [`rejected_model_preset_rows`] counts.
pub fn model_preset_block(
    specs: &Specs,
    model_id: u32,
    original: Option<&Instance>,
    rows: &[ParamEntry],
) -> Option<Instance> {
    let spec = specs.model(model_id)?;
    let mut block = original
        .filter(|block| block.model.0 == model_id)
        .cloned()
        .unwrap_or_else(|| tetrad_engine::new_instance(spec));
    let sidechain = spec
        .sidechain
        .as_ref()
        .map(|sidechain| sidechain.source_param);
    let capture = CAPTURE_MODELS.contains(&model_id);
    for row in rows {
        let Some(param) = u32::try_from(row.index)
            .ok()
            .and_then(|index| spec.parameters.get(index as usize))
        else {
            continue;
        };
        let scene_mode = block.scene_mode.get(param.index as usize) == Some(&true);
        if Some(param.index) == sidechain || (capture && param.index == 5 && scene_mode) {
            continue;
        }
        let Some(value) = row_value(param, row) else {
            continue;
        };
        for scene in
            (0..tetrad_preset::SCENES as u8).filter_map(|scene| SceneIndex::new(scene).ok())
        {
            match value {
                RowValue::Text(text) => {
                    block.set_text(param.index, scene, text);
                }
                RowValue::Value(value) => {
                    block.set_param(param.index, scene, value);
                }
            }
        }
    }
    Some(block)
}

/// Rows of a Virtual Device Preset for a catalog parameter that [`model_preset_block`] leaves
/// out: a value that is not a finite number in `0.0..=1.0`. Owned safety: stock parses any
/// float (`0x1f7b10`), so a corrupt user row in `zenui.db` would reach the DSP.
pub fn rejected_model_preset_rows(specs: &Specs, model_id: u32, rows: &[ParamEntry]) -> usize {
    let Some(spec) = specs.model(model_id) else {
        return 0;
    };
    rows.iter()
        .filter(|row| {
            usize::try_from(row.index)
                .ok()
                .and_then(|index| spec.parameters.get(index))
                .is_some_and(|param| row_value(param, row).is_none())
        })
        .count()
}

enum RowValue<'a> {
    Text(&'a str),
    Value(f32),
}

fn row_value<'a>(param: &tetrad_catalog::ParamSpec, row: &'a ParamEntry) -> Option<RowValue<'a>> {
    if param.kind == tetrad_catalog::ParameterType::String {
        return Some(RowValue::Text(&row.value));
    }
    let value = row.value.parse::<f32>().ok()?;
    (0.0..=1.0)
        .contains(&value)
        .then_some(RowValue::Value(value))
}

/// Stock `0x1f7b10` after each row value: the row's EXP link (`+0x9c`) and range (`+0xa0`
/// min, `+0xa4` max) on every catalog parameter (not MUTE), then the preset's bypass switch
/// (`0x2ed0a8`, block `+0xbc`).
pub fn model_preset_expression(
    model: &mut tetrad_preset::Model,
    parameters: usize,
    rows: &[ParamEntry],
    switch: ExpSwitch,
) -> Result<(), PresetError> {
    for row in rows {
        let param = usize::try_from(row.index)
            .ok()
            .filter(|&index| index < parameters)
            .and_then(|index| model.params.get_mut(index));
        if let Some(param) = param {
            let code = i32::try_from(row.exp).unwrap_or_default();
            param.set_expression(code, row.exp_trim_min as f32, row.exp_trim_max as f32)?;
        }
    }
    let info = tetrad_preset::ExpressionBypassInfo {
        kind: u32::try_from(switch.switch_type).unwrap_or(BypassSwitchType::HeelToe as u32),
        invert: switch.inverted,
        delay_ms: u32::try_from(switch.delay_ms).unwrap_or(DEFAULT_BYPASS_DELAY_MS),
        latch_emulation: switch.latch_emulation,
        unknown: Default::default(),
    };
    match model.expression_bypass_info.first_mut() {
        Some(first) => {
            *first = tetrad_preset::ExpressionBypassInfo {
                unknown: std::mem::take(&mut first.unknown),
                ..info
            }
        }
        None => model.expression_bypass_info.push(info),
    }
    Ok(())
}

/// The stored parameter list of an engine instance at `scene`, in the shape the import reads
/// (`SpecsCatalog::model`): every XML parameter, string parameters (cab and IR Loader
/// `ir selector`) as text from `texts`, then MUTE. Scene values collapse to `scene`.
fn stored_values(instance: &Instance, scene: SceneIndex) -> Vec<ParamValue> {
    let text = |index: usize| {
        let param = instance
            .texts
            .iter()
            .find(|text| text.index as usize == index)?;
        let at_scene = param.scenes.get(scene.index()).cloned().flatten();
        let text = at_scene.or_else(|| param.scenes.iter().flatten().next().cloned());
        Some(ParamValue::String(text.unwrap_or_default()))
    };
    instance
        .params
        .iter()
        .enumerate()
        .map(|(index, value)| text(index).unwrap_or(ParamValue::Float(value.at(scene))))
        .chain([ParamValue::Float(f32::from(u8::from(
            instance.mute.at(scene),
        )))])
        .collect()
}

fn edit_engine(
    layout: &mut Layout,
    specs: &Specs,
    edit: LayoutEdit,
    defaults: &JunctionDefaults,
) -> Result<(), EditError> {
    match edit {
        LayoutEdit::Add { slot, model } => layout.add_block(slot, ModelId(model), specs),
        LayoutEdit::Replace { slot, model } => {
            layout.replace_model(Site::Slot(slot), ModelId(model), specs)
        }
        LayoutEdit::Move { from, to } => layout.move_blocks(&[(from, to)]),
        LayoutEdit::Moves(moves) => layout.move_blocks(&moves),
        LayoutEdit::Remove(slot) => layout.remove_block(slot).map(drop),
        LayoutEdit::Reset(slot) => {
            let site = Site::Slot(slot);
            let model = layout.instance(site).ok_or(EditError::Empty(site))?.model;
            let spec = specs.model(model.0).ok_or(EditError::UnknownModel(model))?;
            layout.set_instance(site, Some(new_instance(spec)));
            Ok(())
        }
        LayoutEdit::Paste { slot, block } => {
            layout.set_instance(Site::Slot(slot), Some(*block));
            Ok(())
        }
        LayoutEdit::Input { row, port } => {
            layout.lane_mut(row).input = port;
            Ok(())
        }
        LayoutEdit::Output { row, port } => {
            layout.lane_mut(row).output = port;
            Ok(())
        }
        // A point on a pair without its junction takes the user default, else the
        // catalog default, as the stored preset does in `prepare_junction_edits`.
        LayoutEdit::Split { pair, column } => {
            let junction = layout.pair_mut(pair);
            if column.is_some() && junction.splitter.is_none() {
                junction.splitter = Some(new_junction(specs, ids::SPLITTER, defaults)?);
            }
            junction.split = column;
            Ok(())
        }
        LayoutEdit::Mix { pair, column } => {
            let junction = layout.pair_mut(pair);
            if column.is_some() && junction.mixer.is_none() {
                junction.mixer = Some(new_junction(specs, ids::MIXER, defaults)?);
            }
            junction.merge = column;
            Ok(())
        }
    }
}

fn new_junction(
    specs: &Specs,
    model: u32,
    defaults: &JunctionDefaults,
) -> Result<Instance, EditError> {
    if let Some(block) = defaults.get(model == ids::MIXER) {
        return Ok(block.clone());
    }
    let spec = specs
        .model(model)
        .ok_or(EditError::UnknownModel(ModelId(model)))?;
    Ok(new_instance(spec))
}

/// A live bypass toggle of a grid block (`0x521514`) or of a pair junction (`0x51dc80`, the
/// mixer), in the deployment and the preset. Inferred: the junction bypass is stored in the
/// master chain's `mixBypass`, the field the load reads for the mixer; Cortex Control sends
/// and echoes it as `splitBypass`. A junction bypass outside scene mode takes every scene.
pub fn set_bypass(
    current: &mut Current,
    specs: &Specs,
    site: Site,
    bypassed: bool,
) -> Result<Update, PresetError> {
    let scene = current.deployment.scene().index();
    // Only the edited bypass record is copied: it is stored after the deployment took the edit.
    match site {
        Site::Slot(slot) => {
            let stored = current.preset.block_bypass_mut(stored_slot(slot))?;
            let mut record = stored.clone();
            record.set_bypassed(scene, bypassed)?;
            let update = current.deployment.set_bypass(specs, site, bypassed)?;
            *stored = record;
            Ok(update)
        }
        Site::Mixer(pair) => {
            let scene_mode = current
                .deployment
                .layout()
                .instance(site)
                .is_some_and(|mixer| mixer.bypass_scene_mode);
            let stored = &mut current
                .preset
                .chains
                .get_mut(pair.master().index())
                .ok_or(PresetError::Unsupported("a pair without its master chain"))?
                .mix_bypass;
            let mut record = stored.clone();
            record.resize(tetrad_preset::SCENES, tetrad_preset::SceneBypass::default());
            for (index, entry) in record.iter_mut().enumerate() {
                if !scene_mode || index == scene {
                    entry.bypass = bypassed;
                }
            }
            let update = current.deployment.set_bypass(specs, site, bypassed)?;
            *stored = record;
            Ok(update)
        }
        _ => Err(PresetError::Unsupported(
            "bypass of lane controls and splitters",
        )),
    }
}

/// The committed bypass records (`0x2e8160` copies `+0x40`/`+0x60`) of grid blocks whose
/// stored record took writes that do not commit ([`BypassOverwriteMode::commits`]).
/// [`Current::preset`] holds the stored record (what a save writes, stock serializes the
/// stored copy); a scene change puts the committed one back ([`Self::revert`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BypassCommits(BTreeMap<Slot, ColBypass>);

impl BypassCommits {
    /// A preset load or a restore commits everything.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// A grid block bypass write (`0x521514` / `0x521620` → `0x2e8160`) under the stock commit
    /// rule: the stored record and the deployment change as [`set_bypass`]; a write that does
    /// not commit keeps the committed record, a committing one updates it too.
    pub fn write(
        &mut self,
        current: &mut Current,
        specs: &Specs,
        slot: Slot,
        bypassed: bool,
        source: ChangeSource,
        mode: BypassOverwriteMode,
    ) -> Result<Update, PresetError> {
        let at = stored_slot(slot);
        let before = current.preset.block_bypass_mut(at)?.clone();
        let scene = current.deployment.scene().index();
        let update = set_bypass(current, specs, Site::Slot(slot), bypassed)?;
        if mode.commits(source) {
            if let Some(record) = self.0.get_mut(&slot) {
                record.set_bypassed(scene, bypassed)?;
            }
        } else {
            self.0.entry(slot).or_insert(before);
        }
        let stored = current.preset.block_bypass_mut(at)?;
        if self.0.get(&slot).is_some_and(|record| record == stored) {
            self.0.remove(&slot);
        }
        Ok(update)
    }

    /// Scene change (`0x2e0e38(preset, 0)`, after the scene is selected): each block takes its
    /// committed record back in the preset and the deployment, unless its bypass is
    /// expression-assigned (`+0x9c`, `0x5cc964`). The deployment ops are dropped: the scene
    /// recall pushes every value.
    /// A slot without a block drops its copy; a failed block does not stop the others (the
    /// first error is returned).
    pub fn revert(&mut self, current: &mut Current, specs: &Specs) -> Result<(), PresetError> {
        let mut result = Ok(());
        for slot in self.0.keys().copied().collect::<Vec<_>>() {
            let at = stored_slot(slot);
            let Ok(Some(model)) = current.preset.block(at) else {
                self.0.remove(&slot);
                continue;
            };
            let assigned = (model.bypass_expression.first())
                .is_some_and(|link| Pedal::from_assignment(link.expression).is_some());
            if assigned {
                continue;
            }
            let Some(record) = self.0.remove(&slot) else {
                continue;
            };
            let restored =
                restore_live(&mut current.deployment, specs, slot, &record).and_then(|()| {
                    *current.preset.block_bypass_mut(at)? = record;
                    Ok(())
                });
            if result.is_ok() {
                result = restored;
            }
        }
        result
    }

    /// The committed copies live in each block model (`0x2e85fc`, `0x2e86bc`) and move or die
    /// with it: moves carry them with the blocks; any other block edit of a slot drops its
    /// copy.
    pub fn follow(&mut self, edit: &LayoutEdit) {
        let mut carry = |moves: &[(Slot, Slot)]| {
            let moving: Vec<_> = (moves.iter())
                .map(|&(from, to)| (to, self.0.remove(&from)))
                .collect();
            for (to, record) in moving {
                if let Some(record) = record {
                    self.0.insert(to, record);
                }
            }
        };
        match *edit {
            LayoutEdit::Move { from, to } => carry(&[(from, to)]),
            LayoutEdit::Moves(ref moves) => carry(moves),
            LayoutEdit::Add { slot, .. }
            | LayoutEdit::Replace { slot, .. }
            | LayoutEdit::Remove(slot)
            | LayoutEdit::Reset(slot)
            | LayoutEdit::Paste { slot, .. } => {
                self.0.remove(&slot);
            }
            LayoutEdit::Input { .. }
            | LayoutEdit::Output { .. }
            | LayoutEdit::Split { .. }
            | LayoutEdit::Mix { .. } => {}
        }
    }

    /// A restored block (undo of a delete or replace) takes its stored record as committed.
    pub fn forget(&mut self, slot: Slot) {
        self.0.remove(&slot);
    }
}

/// Puts a stored bypass record into the deployment, scene by scene where they differ, then
/// selects the playing scene again.
fn restore_live(
    deployment: &mut Deployment,
    specs: &Specs,
    slot: Slot,
    record: &ColBypass,
) -> Result<(), PresetError> {
    let playing = deployment.scene();
    let site = Site::Slot(slot);
    let mut result = Ok(());
    for index in 0..tetrad_engine::SCENES {
        let scene = SceneIndex::new(index as u8).expect("scene in range");
        let live = deployment
            .layout()
            .instance(site)
            .map(|block| block.bypass.at(scene));
        let Some(bypassed) = record
            .bypassed(index)
            .filter(|&stored| live != Some(stored))
        else {
            continue;
        };
        let _ = deployment.select_scene(scene);
        if let Err(error) = deployment.set_bypass(specs, site, bypassed) {
            result = Err(error.into());
            break;
        }
    }
    let _ = deployment.select_scene(playing);
    result
}

/// Runs `save` on `current` with the save-copy rule of `0x51898c` (QC only: the mini passes
/// `held = 0`, gate `0x499a60`). Each stomp of the stored momentary map (`+0x1560`) whose
/// footswitch is in the HAL held mask (`0x509b4c +0xc`, stomp → HAL `0xa4e1e8`) inverts the
/// stored bypass of every assigned block, any role (`0x2e2c44`): every scene, or only the
/// playing scene when the bypass is in scene mode (`+0xcc`). Category 27 and `locked` models
/// (plugin licence lock, `0x2e02d0` → `+0xfc`) are skipped. A block under two held stomps is
/// inverted twice, as stock. (O) The blocks in `also` (the held momentary bypasses of the
/// Custom footswitch mode) are inverted in the same way. The live records come back after the
/// save.
pub fn save_resting<T>(
    held: u16,
    also: &[Slot],
    specs: &Specs,
    locked: impl Fn(u32) -> bool,
    current: &mut Current,
    save: impl FnOnce(&mut Current) -> T,
) -> T {
    let scene = current.deployment.scene().index();
    let held_stomps: Vec<u32> = (current.preset.stomp_is_momentary.iter())
        .filter(|&(stomp, &momentary)| {
            momentary
                && StompIndex::new(stomp)
                    .is_some_and(|stomp| held & Footswitch::of_stomp(stomp).physical().mask() != 0)
        })
        .map(|(stomp, _)| stomp)
        .collect();
    let slots: Vec<Slot> = (held_stomps.iter())
        .flat_map(|&stomp| super::actions::stomp_slots(&current.preset, stomp))
        .chain(also.iter().copied())
        .collect();
    let mut live = BTreeMap::new();
    for slot in slots {
        let at = stored_slot(slot);
        let Some(model) = current.preset.block(at).ok().flatten().and_then(|m| m.hash) else {
            continue;
        };
        let category = specs.model(model).map(|spec| spec.category_id);
        if category == Some(LOOPER_CATEGORY) || locked(model) {
            continue;
        }
        let Ok(stored) = current.preset.block_bypass_mut(at) else {
            continue;
        };
        live.entry(at).or_insert_with(|| stored.clone());
        let scenes = &mut stored.scene_bypass;
        let inverted = if stored.scene_mode.unwrap_or(false) {
            scenes.get_mut(scene..=scene).unwrap_or_default()
        } else {
            &mut scenes[..]
        };
        for entry in inverted {
            entry.bypass = !entry.bypass;
        }
    }
    let result = save(current);
    for (at, record) in live {
        if let Ok(stored) = current.preset.block_bypass_mut(at) {
            *stored = record;
        }
    }
    result
}

impl LayoutChange {
    /// Stock model setter `0x522d70` after a block add or model change at `slot`: a block
    /// without a PRIMARY assignment takes the first stomp 0..=7 without any assignment when
    /// "Stomp Mode Bypass Assignment" is on (`0x510d70`); a 27000 block turns its stomp's
    /// momentary flag off (`0x5227c4`); a SECONDARY assignment goes when the model has no
    /// secondary stomp parameter (`0x522974`). Returns the auto-assigned stomp.
    pub fn assign_new_block(
        &mut self,
        specs: &Specs,
        slot: Slot,
        auto_assign: bool,
    ) -> Result<Option<u32>, PresetError> {
        let model = self
            .deployment
            .layout()
            .instance(Site::Slot(slot))
            .ok_or(PresetError::Unsupported("an edited slot without a block"))?
            .model
            .0;
        assign_new_block(&mut self.preset, specs, slot, model, auto_assign)
    }
}

/// `0x522d70` tests PRIMARY (`0x2e1f20`) and SECONDARY (`0x2e1fcc`) apart, so a block with only
/// a SECONDARY assignment still takes a PRIMARY one (review r14 P-7); it is inserted beside the
/// SECONDARY, as `Preset::assign_stomp` keeps one assignment per slot.
fn assign_new_block(
    preset: &mut Preset,
    specs: &Specs,
    slot: Slot,
    model: u32,
    auto_assign: bool,
) -> Result<Option<u32>, PresetError> {
    let (row, column) = (slot.row.index() as u32, slot.column.index() as u32);
    let primary_role = |role: Option<StompRole>| matches!(role, None | Some(StompRole::Primary));
    let ours = |preset: &Preset, primary: bool| {
        (preset.stomp_mode_assignments.iter())
            .find(|a| (a.row, a.column) == (row, column) && primary_role(a.role) == primary)
            .map(|assignment| assignment.stomp_index)
    };
    let (mut primary, secondary) = (ours(preset, true), ours(preset, false));
    let mut assigned = None;
    if primary.is_none() && auto_assign {
        let taken: Vec<StompAssignment> = (preset.stomp_mode_assignments.iter())
            .filter_map(|assignment| {
                Some(StompAssignment {
                    row: 0,
                    column: 0,
                    stomp: StompIndex::new(assignment.stomp_index)?,
                    kind: AssignmentType::Primary,
                })
            })
            .collect();
        if let Some(stomp) = auto_assign_stomp(&taken) {
            let stomp = u32::from(stomp.get());
            let position = (preset.stomp_mode_assignments)
                .partition_point(|a| (a.row, a.column) <= (row, column));
            preset.stomp_mode_assignments.insert(
                position,
                tetrad_preset::StompAssignment {
                    row,
                    column,
                    stomp_index: stomp,
                    role: Some(StompRole::Primary),
                    ..Default::default()
                },
            );
            (primary, assigned) = (Some(stomp), Some(stomp));
        }
    }
    if let Some(stomp) = primary
        && model == NON_MOMENTARY_MODEL
    {
        preset.stomp_is_momentary.insert(stomp, false);
    }
    let keeps_secondary = specs
        .model(model)
        .is_some_and(|spec| spec.secondary_stomp_parameter.is_some());
    if secondary.is_some() && !keeps_secondary {
        (preset.stomp_mode_assignments)
            .retain(|a| (a.row, a.column) != (row, column) || primary_role(a.role));
    }
    Ok(assigned)
}

/// The model `step` places from `model` in the Virtual Device list order of its category
/// (BANK UP encoder device cycling, manual "BANK UP knob"). Inferred order: visible grid
/// models of the same category by model id, wrapping. `None` for an unknown model.
pub fn cycled_model(specs: &Specs, model: u32, step: i32) -> Option<u32> {
    let category = specs.model(model)?.category_id;
    let mut models: Vec<u32> = specs
        .models
        .values()
        .filter(|spec| {
            spec.category_id == category
                && spec.role == tetrad_catalog::ModelRole::Block
                && !spec.hidden
                && !spec.internal
        })
        .map(|spec| spec.id)
        .collect();
    models.sort_unstable();
    let position = models.iter().position(|&id| id == model)? as i64;
    let len = models.len() as i64;
    Some(models[(position + i64::from(step)).rem_euclid(len) as usize])
}

/// Stored-preset edits of Cortex Control Grid UPDATE (`0x257408`) that no other action
/// covers. Only a scene-mode change touches the deployment.
#[derive(Debug, Clone, PartialEq)]
pub enum PresetEdit {
    /// `tags` (`0x2551f0` → `0x2e2318`).
    Tags(Vec<String>),
    /// A grid block's footswitch (`0x523224`), or none (`0x522b64`).
    Stomp {
        slot: Slot,
        assignment: Option<(u32, StompRole)>,
    },
    /// `stomp_labels` (`0x514474` flag 1 → `0x2e3224`). `Action::StompLabel` writes
    /// `single_stomp_labels` (flag 0).
    StompLabel { stomp: u32, label: String },
    /// `stomp_is_momentary` (`0x5227c4` → `0x2e33e4`).
    Momentary { stomp: u32, momentary: bool },
    /// A parameter's scene mode: on (`0x517f54`), or off with the current scene's value in
    /// every scene (`0x524770`).
    SceneMode {
        site: Site,
        index: u32,
        enabled: bool,
    },
    /// The range of a block parameter's expression link (`0x170110`).
    ExpressionRange {
        slot: Slot,
        index: u32,
        min: f32,
        max: f32,
    },
    /// Owned (Tetrad): the per-preset mappings as stored JSON
    /// (`tetrad_preset::model::TETRAD_MAPPINGS_FIELD`), or none.
    Mappings(Option<Vec<u8>>),
    /// Owned (Tetrad): the preset's own Custom footswitch layout as stored JSON
    /// (`tetrad_preset::model::TETRAD_FOOTSWITCH_FIELD`), or none.
    CustomLayout(Option<Vec<u8>>),
    /// A model update notification on a grid block (`0x514fd8` → `0x2e1e70`, slot `+0xe4`).
    /// Inferred: stored as `model_update_notifications`.
    Notify(Slot),
    /// A parameter's expression link code (0 none, 1 EXP1, else EXP2) on a lane, input gate
    /// or pair junction (`0x171b00` → `0x170110`), with a new range or the stored one.
    SiteExpression {
        site: Site,
        index: u32,
        code: i32,
        range: Option<(f32, f32)>,
    },
    /// A tempo parameter's scene mode (`0x517f54` / `0x524770` with model 25000): off keeps
    /// the current scene's value in every scene. Index 6 also sets 10..=22.
    TempoSceneMode { index: u32, enabled: bool },
    /// A pedal switch (`0x171e78`) on a lane MUTE or SOLO, input gate parameter 1, or a mixer
    /// bypass (`index` none, `bypass_expression`): the link code keeps the stored range
    /// (`0x170110`); the settings go to the parameter's switch entry (`0x51c4f8`).
    SiteSwitch {
        site: Site,
        index: Option<u32>,
        code: Option<i32>,
        info: Option<BypassInfo>,
    },
}

/// A pedal switch as stored (`ExpressionBypassInfo`).
pub(super) fn switch_info(info: BypassInfo) -> tetrad_preset::ExpressionBypassInfo {
    tetrad_preset::ExpressionBypassInfo {
        kind: info.kind as u32,
        invert: info.invert,
        delay_ms: info.delay_ms,
        latch_emulation: info.latch_emulation,
        unknown: Default::default(),
    }
}

/// `0x517f54` / `0x524770`: tempo index 6 carries indices 10..=22 with it.
pub fn tempo_scene_mode_indices(index: u32) -> impl Iterator<Item = u32> {
    let group = if index == 6 { 10..23 } else { 0..0 };
    std::iter::once(index).chain(group)
}

/// Applies a [`PresetEdit`]. A scene-mode change returns the recompiled layout to deploy;
/// nothing changes when either side refuses.
pub fn apply_edit(
    current: &mut Current,
    specs: &Specs,
    edit: PresetEdit,
) -> Result<Option<LayoutChange>, PresetError> {
    let preset = &mut current.preset;
    match edit {
        PresetEdit::Tags(tags) => preset.tags = tags,
        PresetEdit::Mappings(json) => preset
            .unknown
            .set_bytes_field(tetrad_preset::model::TETRAD_MAPPINGS_FIELD, json)?,
        PresetEdit::CustomLayout(json) => preset
            .unknown
            .set_bytes_field(tetrad_preset::model::TETRAD_FOOTSWITCH_FIELD, json)?,
        PresetEdit::Stomp { slot, assignment } => match assignment {
            Some((stomp, role)) => preset.assign_stomp(stored_slot(slot), stomp, role)?,
            None => drop(preset.unassign_stomp(stored_slot(slot))),
        },
        PresetEdit::StompLabel { stomp, label } => preset.stomp_labels.insert(stomp, label),
        PresetEdit::Momentary { stomp, momentary } => {
            preset.stomp_is_momentary.insert(stomp, momentary)
        }
        PresetEdit::ExpressionRange {
            slot,
            index,
            min,
            max,
        } => {
            let param = preset
                .block_mut(stored_slot(slot))?
                .param_mut(index as usize)?;
            param.set_expression(param.expression.unwrap_or(0), min, max)?;
        }
        PresetEdit::Notify(slot) => {
            let (row, column) = (slot.row.index() as u32, slot.column.index() as u32);
            let notifications = &mut preset.model_update_notifications;
            if !notifications
                .iter()
                .any(|stored| (stored.row, stored.column) == (row, column))
            {
                notifications.push(tetrad_preset::GridRef {
                    row,
                    column,
                    ..Default::default()
                });
            }
        }
        PresetEdit::SceneMode {
            site,
            index,
            enabled,
        } => {
            let indices = scene_mode_group(current, specs, site, index, enabled);
            return scene_mode(current, specs, site, &indices, enabled).map(Some);
        }
        PresetEdit::SiteExpression {
            site,
            index,
            code,
            range,
        } => {
            let param = stored_model(preset, site)?.param_mut(index as usize)?;
            let stored = (
                param.expression_min.unwrap_or(0.0),
                param.expression_max.unwrap_or(1.0),
            );
            let (min, max) = range.unwrap_or(stored);
            param.set_expression(code, min, max)?;
        }
        PresetEdit::SiteSwitch {
            site,
            index,
            code,
            info,
        } => {
            let entry = match index {
                Some(index) => super::features::switch_entry(site, index)
                    .ok_or(PresetError::Unsupported("a switch on this parameter"))?,
                None => 0,
            };
            let model = stored_model(preset, site)?;
            match (index, code) {
                (Some(index), Some(code)) => {
                    let param = model.param_mut(index as usize)?;
                    let min = param.expression_min.unwrap_or(0.0);
                    let max = param.expression_max.unwrap_or(1.0);
                    param.set_expression(code, min, max)?;
                }
                (None, Some(code)) => match model.bypass_expression.first_mut() {
                    Some(link) => link.expression = code,
                    None => model.bypass_expression.push(tetrad_preset::Expression {
                        expression: code,
                        expression_max: 1.0,
                        ..Default::default()
                    }),
                },
                (_, None) => {}
            }
            if let Some(info) = info {
                let stored = &mut model.expression_bypass_info;
                if stored.len() <= entry {
                    stored.resize(entry + 1, switch_info(BypassInfo::default()));
                }
                stored[entry] = tetrad_preset::ExpressionBypassInfo {
                    unknown: std::mem::take(&mut stored[entry].unknown),
                    ..switch_info(info)
                };
            }
        }
        PresetEdit::TempoSceneMode { index, enabled } => {
            let scene = current.deployment.scene().index();
            let tempo = preset
                .tempo_program_data
                .first_mut()
                .ok_or(PresetError::Unsupported("a preset without tempo data"))?;
            tempo
                .param_mut(index as usize)?
                .set_scene_mode(enabled, scene)?;
            for index in tempo_scene_mode_indices(index).skip(1) {
                if let Some(param) = tempo.params.get_mut(index as usize) {
                    param.set_scene_mode(enabled, scene)?;
                }
            }
        }
    }
    Ok(None)
}

/// Pattern Tremolo (`0x493e64`): STEP 1..=16 are parameters 11..=26.
const PATTERN_TREMOLO: u32 = 7040;
const STEPS: std::ops::RangeInclusive<u32> = 11..=26;
/// STEP n CONNECTED (27..=41) joins step n to step n + 1.
const STEP_CONNECTED: u32 = 27;

/// The parameters one scene-mode change sets, in stock order: on `0x517f54` (block
/// `0x513fc8`), off `0x524770` (block `0x522674`). A block takes its catalog
/// `linkedSceneMode` list (`0x5ce568`), except Looper X (19 with 20) and Pattern Tremolo steps
/// (the connected run at the current scene, `0x49390c`/`0x493a9c`). Off only: 18007 (7 with 20,
/// 11 with 21), 29001..=29008 (22 then 2, 23 then 10) and 3008 (42 then 15, 43 then 23) pair
/// instead. Other sites change one parameter. Linked indices past the parameters are left
/// out.
pub fn scene_mode_group(
    current: &Current,
    specs: &Specs,
    site: Site,
    index: u32,
    enabled: bool,
) -> Vec<u32> {
    let instance = current.deployment.layout().instance(site);
    let (Site::Slot(_), Some(instance)) = (site, instance) else {
        return vec![index];
    };
    let model = instance.model.0;
    let paired = |first, second| vec![first, second];
    let group = match (model, index) {
        (18_007, 7 | 20) if !enabled => paired(7, 20),
        (18_007, 11 | 21) if !enabled => paired(11, 21),
        (29_001..=29_008, 2) if !enabled => paired(22, 2),
        (29_001..=29_008, 10) if !enabled => paired(23, 10),
        (3_008, 15) if !enabled => paired(42, 15),
        (3_008, 23) if !enabled => paired(43, 23),
        (27_000, 19) => paired(19, 20),
        (27_000, 20) => paired(20, 19),
        (27_000, _) => vec![index],
        (PATTERN_TREMOLO, step) if STEPS.contains(&step) => std::iter::once(step)
            .chain(connected_steps(instance, current.deployment.scene(), step))
            .collect(),
        _ => {
            let linked = specs
                .model(model)
                .and_then(|spec| spec.parameter(index))
                .map(|param| param.linked_scene_mode.clone());
            std::iter::once(index)
                .chain(linked.unwrap_or_default())
                .collect()
        }
    };
    let count = instance.params.len() as u32;
    let mut indices = Vec::new();
    for other in group {
        if (other == index || other < count) && !indices.contains(&other) {
            indices.push(other);
        }
    }
    indices
}

/// Pattern Tremolo (`0x493e64`): the other steps of the connected run of `step` at `scene`
/// (`0x49390c`/`0x493a9c`).
fn connected_steps(instance: &Instance, scene: SceneIndex, step: u32) -> impl Iterator<Item = u32> {
    let connected = |step: u32| {
        let flag = instance
            .params
            .get((STEP_CONNECTED + step - STEPS.start()) as usize);
        flag.is_some_and(|flag| flag.at(scene) > 0.5)
    };
    let first = (*STEPS.start()..step)
        .rev()
        .take_while(|&before| connected(before))
        .last()
        .unwrap_or(step);
    let last = (step..*STEPS.end())
        .take_while(|&at| connected(at))
        .last()
        .map_or(step, |at| at + 1);
    (first..=last).filter(move |&other| other != step)
}

/// A value edit on a Pattern Tremolo STEP also writes the other steps of its connected run:
/// Cortex Control (`0x1727a0` → `0x493e64`, then `0x523644` per step) and the step editor
/// (`0x3afc94`) both do. Empty for any other block or parameter (review r11 N4).
pub fn step_run(current: &Current, site: Site, index: u32) -> Vec<u32> {
    match current.deployment.layout().instance(site) {
        Some(instance) if instance.model.0 == PATTERN_TREMOLO && STEPS.contains(&index) => {
            connected_steps(instance, current.deployment.scene(), index).collect()
        }
        _ => Vec::new(),
    }
}

/// The stored flags and the engine instance's (`Instance::scene_mode`) of `indices`; turning
/// them off collapses each value to the current scene in both. A grid block's MUTE (index
/// after the XML parameters) has only the stored flag (`0x513fc8`). A junction's BYPASS index
/// couples the splitter and the mixer (`0x517f54`, `0x524770` → `0x2e3938`/`0x2e37c4`); its
/// flag lives in the engine instances, and turning it off collapses `split_bypass` and
/// `mix_bypass` (inferred: the stored lists carry no flag, the load reads differing values).
fn scene_mode(
    current: &Current,
    specs: &Specs,
    site: Site,
    indices: &[u32],
    enabled: bool,
) -> Result<LayoutChange, PresetError> {
    let scene = current.deployment.scene();
    let instance = current.deployment.layout().instance(site);
    let param_count = instance.map_or(0, |instance| instance.params.len() as u32);
    let mute = matches!(site, Site::Slot(_)).then_some(param_count);
    let junction = match site {
        Site::Splitter(pair) | Site::Mixer(pair) => {
            instance.map(|instance| (pair, instance.bypass_index()))
        }
        _ => None,
    };
    let bypass_index = junction.map(|(_, index)| index);
    if indices
        .iter()
        .any(|&index| index >= param_count && Some(index) != mute && Some(index) != bypass_index)
    {
        return Err(PresetError::Unsupported(
            "scene mode of a missing parameter",
        ));
    }
    let mut preset = current.preset.clone();
    for &index in indices {
        match junction {
            Some((pair, bypass)) if index == bypass => {
                if !enabled {
                    let chain = chain_mut(&mut preset, pair.master().index())?;
                    for list in [&mut chain.split_bypass, &mut chain.mix_bypass] {
                        if let Some(value) = list.get(scene.index()).map(|entry| entry.bypass) {
                            list.iter_mut().for_each(|entry| entry.bypass = value);
                        }
                    }
                }
            }
            _ => stored_model(&mut preset, site)?
                .param_mut(index as usize)?
                .set_scene_mode(enabled, scene.index())?,
        }
    }
    let mut deployment = current.deployment.clone();
    let update = deployment.edit(specs, |layout| {
        if let Some((pair, bypass)) = junction.filter(|(_, bypass)| indices.contains(bypass)) {
            for site in [Site::Splitter(pair), Site::Mixer(pair)] {
                if let Some(junction) = layout.instance_mut(site) {
                    junction.bypass_scene_mode = enabled;
                    if !enabled {
                        junction.bypass = tetrad_engine::Scened::Fixed(junction.bypass.at(scene));
                    }
                }
            }
            if indices == [bypass] {
                return Ok(());
            }
        }
        let instance = layout.instance_mut(site).ok_or(EditError::Empty(site))?;
        for &index in indices {
            if Some(index) == mute {
                if !enabled {
                    instance.mute = tetrad_engine::Scened::Fixed(instance.mute.at(scene));
                }
                continue;
            }
            if Some(index) == bypass_index {
                continue;
            }
            let at = index as usize;
            if let Some(flag) = instance.scene_mode.get_mut(at) {
                *flag = enabled;
            }
            if !enabled {
                instance.params[at] = tetrad_engine::Scened::Fixed(instance.params[at].at(scene));
                if let Some(text) = instance.texts.iter_mut().find(|text| text.index == index) {
                    let value = text.scenes.get(scene.index()).cloned().flatten();
                    text.scenes
                        .iter_mut()
                        .for_each(|stored| *stored = value.clone());
                }
            }
        }
        Ok(())
    })?;
    Ok(LayoutChange {
        update,
        preset: Box::new(preset),
        deployment: Box::new(deployment),
        undo: None,
    })
}

/// The engine instance a stored model loads as at `slot`: the loaded preset with that slot
/// replaced, through the load import. `0x17422c` builds a Control block from its model
/// message (`0x680614`) and its `ColBypass`, or a default one (not bypassed).
pub fn imported_block(
    current: &Current,
    specs: &Specs,
    slot: Slot,
    model: Model,
    bypass: Option<tetrad_preset::ColBypass>,
) -> Result<Instance, PresetError> {
    let mut preset = current.preset.clone();
    let at = stored_slot(slot);
    if preset.block(at)?.is_some() {
        preset.replace_model(at, model)?;
    } else {
        preset.insert_block(at, model, &DeviceCatalog(specs))?;
    }
    let record = preset.block_bypass_mut(at)?;
    match bypass {
        Some(bypass) => *record = bypass,
        None => {
            record.scene_mode = Some(false);
            record
                .scene_bypass
                .iter_mut()
                .for_each(|scene| scene.bypass = false);
        }
    }
    let document = PresetDocument::parse(&preset.to_bytes(), tetrad_preset::Limits::default())?;
    let imported = import_layout(&document.summary()?, specs, AbsentScalars::Proto3Default)?;
    imported
        .layout
        .instance(Site::Slot(slot))
        .cloned()
        .ok_or(PresetError::Unsupported("a block the import leaves out"))
}

fn stored_slot(slot: Slot) -> tetrad_preset::Slot {
    tetrad_preset::Slot {
        row: slot.row.index(),
        column: slot.column.index(),
    }
}

/// Who saves, under which name and when.
pub struct SaveRequest<'a> {
    /// Slot for a preset without a file (the empty-slot shape).
    pub slot: &'a PresetRef,
    pub name: &'a str,
    pub now: SystemTime,
    /// The author `0x5b7438` stamps when the stored author is empty or the loaded preset's
    /// folder is read-only ([`SaveAccount::name`]).
    pub account_name: &'a str,
    /// [`SaveAccount::id`].
    pub account_id: &'a str,
    /// The sidecar `creator` (`0x7c4db8` → `0x497ce0`).
    pub creator: Creator,
}

/// The account a save stamps as the author (`0x5b7438`). Logged out, both are empty (logout
/// writes an empty `ui_username`, `0x2ff6e0`, and clears the token store `+0x14`, `0x44d590`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SaveAccount {
    /// The display name: `ui_username` (`0x2ff44c`, account `+0x10`), not the sign-in
    /// `username` (`+0xc`).
    pub name: String,
    /// The access token's `sub` (`0x2ff3f8` → `0x44d2fc`, token store `+0x14`, set by the JWT
    /// parse `0x44ed80`); empty when the token does not parse.
    pub id: String,
}

impl SaveAccount {
    pub fn of(settings: &tetrad_settings::device::Settings) -> Self {
        Self {
            name: settings.account.ui_username.clone(),
            id: Claims::parse(settings.tokens.access_token.expose())
                .map(|claims| claims.user_id)
                .unwrap_or_default(),
        }
    }
}

/// A save that is on disk.
#[derive(Debug)]
pub struct SavedPreset {
    pub path: String,
    /// False when the file already held these bytes.
    pub written: bool,
    /// The library's search index could not follow; a warning, not a failure.
    pub search_index: Option<StaleSearchIndex>,
}

/// The name a save stamps (`0x5b7438` → `0x51898c` keeps the stored name): the requested
/// name (Save As, Edit Details), else the loaded preset's own name. Never the file stem:
/// stock files whose name differs from their stem (a `_<n>` suffix from
/// [`StockLibrary::store`]) keep their name (review-w2c-io R2). `None`: a preset without a
/// name needs one from the keyboard.
pub fn save_name(current: &Current, requested: Option<String>) -> Option<String> {
    requested.or_else(|| current.preset.name.clone().filter(|name| !name.is_empty()))
}

/// Stock save (`0x5b7438` → `0x51898c` → `0x6817dc`) through `Preset::stamp_save` and the
/// stock library: an existing file is rewritten in place, a new preset goes to its empty
/// slot. The modified version becomes the version of the running build
/// (`tetrad_firmware::running`) when the preset differs from its migrated file (stock stamps at
/// the first edit, `0x50ec54`). Refused (owned, e1-preset gap
/// 8): unmigrated presets, and kept unknown blocks with a stored modified version below
/// 3.0.0, which a 4.1.0 stamp would make stock read as modern.
pub fn save(
    library: &mut StockLibrary,
    current: &mut Current,
    specs: &Specs,
    request: &SaveRequest,
) -> Result<SavedPreset, PresetError> {
    save_with(
        library,
        current,
        specs,
        request,
        PreferredInstrument::NotSet,
    )
}

/// [`save`] whose new file (a preset without a file) has `instrument` in its sidecar: the
/// Save As PREFERRED INSTRUMENT (`presetSaveSignal` → `0x5b7438`). A rewrite sets the sidecar
/// instrument unless `instrument` is `NotSet` (Edit Details).
pub fn save_with(
    library: &mut StockLibrary,
    current: &mut Current,
    specs: &Specs,
    request: &SaveRequest,
    instrument: PreferredInstrument,
) -> Result<SavedPreset, PresetError> {
    let target = match current.path {
        Some(_) => None,
        None => Some((
            setlist(library, request.slot.setlist.as_deref())?,
            usize::try_from(request.slot.index).unwrap_or(usize::MAX),
        )),
    };
    write(library, current, specs, request, target, false, instrument)
}

/// Save As (`0x5ac350` → `0x5b7438`) of a preset into a setlist (`None`: the first one):
/// a new file in `slot`, or in the setlist's first empty slot (`0x7c3d94`), named
/// `request.name` with a `_<n>` file suffix when that file exists (`0x699f18`). An occupied
/// `slot` is overwritten ([`StockLibrary::replace`]) only after every check that can refuse the save; the
/// loaded preset's own slot is refused (its file is the save source). The copy becomes the loaded preset (stock loads it, `0x5b2f40`); the
/// source file stays. Inferred: stock lets the user pick any slot with the first free one
/// preselected.
pub fn save_as(
    library: &mut StockLibrary,
    current: &mut Current,
    specs: &Specs,
    request: &SaveRequest,
    setlist_key: Option<&str>,
    slot: Option<usize>,
) -> Result<(SavedPreset, PresetRef), PresetError> {
    let instrument = PreferredInstrument::NotSet;
    save_as_with(
        library,
        current,
        specs,
        request,
        (setlist_key, slot),
        instrument,
    )
}

/// [`save_as`] into `(setlist, slot)` with `instrument` in the new sidecar (`0x58fe80`).
pub fn save_as_with(
    library: &mut StockLibrary,
    current: &mut Current,
    specs: &Specs,
    request: &SaveRequest,
    (setlist_key, slot): (Option<&str>, Option<usize>),
    instrument: PreferredInstrument,
) -> Result<(SavedPreset, PresetRef), PresetError> {
    let setlist = setlist(library, setlist_key)?;
    if let Some(index) = slot
        && current.path.is_some()
        && library.slots(&setlist.name)?.get(index).cloned().flatten() == current.path
    {
        return Err(PresetError::Unsupported(
            "a Save As over the loaded preset's own slot",
        ));
    }
    let index = target_slot(library, &setlist.name, slot)?;
    let target = PresetRef {
        setlist: Some(setlist.path.clone()),
        index: index as u32,
    };
    let saved = write(
        library,
        current,
        specs,
        request,
        Some((setlist, index)),
        slot.is_some(),
        instrument,
    )?;
    Ok((saved, target))
}

/// The slot a copy or Save As writes: `slot`, else the first empty one (`0x7c3d94`; none is
/// [`PresetError::SetlistFull`]).
fn target_slot(
    library: &StockLibrary,
    setlist: &str,
    slot: Option<usize>,
) -> Result<usize, PresetError> {
    let Some(index) = slot else {
        return first_free_slot(library, setlist)?
            .ok_or_else(|| PresetError::SetlistFull(setlist.to_owned()));
    };
    library
        .slots(setlist)?
        .get(index)
        .ok_or(tetrad_library::Error::SlotRange(index))?;
    Ok(index)
}

/// Edit Details of the loaded preset (`0x5b91dc` → `0x5b7438`) after its save: the file
/// follows the new name and the slot stays (`0x711aa4`). Refused when `slot` does not hold
/// the loaded file.
pub fn follow_name(
    library: &mut StockLibrary,
    current: &mut Current,
    slot: &PresetRef,
    name: &str,
) -> Result<Option<StaleSearchIndex>, PresetError> {
    let Some(path) = current.path.clone() else {
        return Ok(None);
    };
    let setlist = setlist(library, slot.setlist.as_deref())?;
    if path == format!("{}/{name}.pb", setlist.path) {
        return Ok(None);
    }
    let index = slot.index as usize;
    if library.slots(&setlist.name)?.get(index).cloned().flatten() != Some(path) {
        return Err(PresetError::Unsupported(
            "a rename of a preset outside its slot",
        ));
    }
    let renamed = library.rename_preset(&setlist.name, index, name)?;
    current.path = Some(renamed.value);
    Ok(renamed.search_index)
}

/// Copy a preset of setlist `from` into slot `to_slot` of setlist `to`, or into its first
/// empty slot (stock Copy, then "Paste in the first empty slot", `0x7c3d94`; the Directory
/// Multi Select copy; Cortex Control COPY and MOVE with a target index). An occupied target
/// is overwritten (`target_slot`). The copy keeps the stored name (never the file stem) and
/// sidecar; its file takes a free `<name>.pb` and its hash follows the new path (`PresetId`).
/// The copy is built before the target changes; [`StockLibrary::replace`] writes the new
/// files before it drops the old preset, so a failed write keeps the target (review r20 G4).
/// Cortex Control MOVE and COPY reach `0x1e8290` with the force flag set (`0x24b254`:
/// `param_6 != 2`), which skips the SWAP/OVERWRITE dialog and runs the drop `0x1e68d0`, as
/// its OVERWRITE choice does (`0x1e8248`).
/// The paste name rule is not recovered (I). The source slot itself is refused (stock does
/// nothing there, `0x1e8a70`).
pub fn copy_into(
    library: &mut StockLibrary,
    from: &str,
    slot: usize,
    to: &str,
    to_slot: Option<usize>,
) -> Result<Copied, PresetError> {
    if from == to && to_slot == Some(slot) {
        return Err(PresetError::Unsupported("a copy onto its own slot"));
    }
    let source = copy_source(library, from, slot)?;
    copy_source_into(library, source, to, to_slot)
}

/// [`copy_into`] of a source [`copy_source`] already read (review r30 C8: a PASTE reads each
/// source once).
pub(crate) fn copy_source_into(
    library: &mut StockLibrary,
    source: CopySource,
    to: &str,
    to_slot: Option<usize>,
) -> Result<Copied, PresetError> {
    let CopySource {
        sidecar,
        mut preset,
        name,
    } = source;
    let target = target_slot(library, to, to_slot)?;
    let document = PresetDocument::parse(&preset.to_bytes(), tetrad_preset::Limits::default())?;
    let stored = library.replace(to, target, &name, &document, &sidecar)?;
    // The target is gone after `replace`: a failed hash rewrite is reported with the copy,
    // not as an error (review r21 H6).
    preset.hash = Some(tetrad_core::PresetId::from_identifier(&stored.value).0);
    let hash_error = rewrite(library, &stored.value, &preset, None).err();
    Ok(Copied {
        slot: target,
        path: stored.value,
        search_index: stored.search_index,
        hash_error,
    })
}

/// The source of a [`copy_into`]: the sidecar, the preset and its stored name (never the file
/// stem). The stored document bytes are not kept.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CopySource {
    sidecar: Sidecar,
    preset: Preset,
    name: String,
}

/// Reads the [`CopySource`] of `slot`. Refused: an empty slot, an unreadable file, a preset
/// without a stored name.
pub(crate) fn copy_source(
    library: &StockLibrary,
    from: &str,
    slot: usize,
) -> Result<CopySource, PresetError> {
    let source = library
        .slots(from)?
        .get(slot)
        .cloned()
        .flatten()
        .ok_or(tetrad_library::Error::SlotEmpty(slot))?;
    let stock = library.load(&source)?;
    let preset = Preset::try_from(&stock.document)?;
    let name = preset
        .name
        .clone()
        .filter(|name| !name.is_empty())
        .or_else(|| Some(stock.sidecar.as_ref()?.text("name")?.to_owned()))
        .ok_or(PresetError::Unsupported("a preset without a stored name"))?;
    let sidecar = stock.sidecar.unwrap_or_default();
    Ok(CopySource {
        sidecar,
        preset,
        name,
    })
}

/// A copy that [`copy_into`] wrote.
#[derive(Debug)]
pub struct Copied {
    pub slot: usize,
    pub path: String,
    pub search_index: Option<StaleSearchIndex>,
    /// The copy is in its slot, but its hash rewrite failed: it keeps the source hash.
    pub hash_error: Option<PresetError>,
}

/// The first setlist slot that names `path` (stock "Jump to file", library action 11).
pub fn locate(library: &StockLibrary, path: &str) -> Result<Option<PresetRef>, PresetError> {
    for setlist in library.setlists()? {
        let slots = library.slots(&setlist.name)?;
        if let Some(index) = slots.iter().position(|slot| slot.as_deref() == Some(path)) {
            return Ok(Some(PresetRef {
                setlist: Some(setlist.path),
                index: index as u32,
            }));
        }
    }
    Ok(None)
}

/// Stamps and writes `current`: in place (`new_slot: None`, the preset has a file) or as a
/// new file in an empty slot (with `overwrite`, an occupied one: [`StockLibrary::replace`]).
fn write(
    library: &mut StockLibrary,
    current: &mut Current,
    specs: &Specs,
    request: &SaveRequest,
    new_slot: Option<(Setlist, usize)>,
    overwrite: bool,
    instrument: PreferredInstrument,
) -> Result<SavedPreset, PresetError> {
    let migration = &current.migration;
    if !migration.unknown_blocks.is_empty()
        && !migration.unknown_blocks_emptied
        && migration.versions.modified < IMPORT_GATE_END
    {
        return Err(PresetError::Unsupported(
            "kept unknown blocks with a stored modified version below 3.0.0",
        ));
    }
    // The sidecar writer formats the stock UTC date (`yyyy-MM-ddThh:mm:ssZ`, the same text
    // as the converter's `%FT%TZ`, `0x006695c4`).
    let date = utc_date(request.now);
    let (changed, stored_sidecar) = match &current.path {
        Some(path) => {
            let stock = library.load(path)?;
            let mut stored = Preset::try_from(&stock.document)?;
            restore_community(&mut stored, specs);
            stored.migrate(&DeviceCatalog(specs))?;
            let changed =
                stored != current.preset || request.name != stored.name.as_deref().unwrap_or("");
            (changed, stock.sidecar)
        }
        None => (true, None),
    };
    let stamp = |path: &str, preset: &mut Preset| {
        preset.stamp_save(&SaveStamp {
            date: &date,
            selected_scene: current.deployment.scene().index(),
            account_name: request.account_name,
            account_id: request.account_id,
            path,
            name: request.name,
            changed_in: changed.then_some(tetrad_firmware::running().version),
        })
    };
    let mut preset = current.preset.clone();
    // `0x5b7438`: a preset loaded from a read-only folder (`0x7c28b4` → `0x7054f0`) always
    // takes the account as its author (`0x2df870`, `0x2df8a8`), as an empty author does in
    // `stamp_save`; outside a plugin folder (`0x7c2a0c`) its tags go too (`0x2e2318`).
    if let Some(folder) = (current.path.as_deref()).and_then(|path| library.read_only_folder(path))
    {
        preset.author_name = None;
        preset.author_id = None;
        if folder == ReadOnlyFolder::FactoryLibrary {
            preset.tags.clear();
        }
    }
    let saved_sidecar = |preset: &Preset, instrument, coros_version| {
        Sidecar::saved_preset(&PresetSave {
            name: request.name,
            author: preset.author_name.as_deref().unwrap_or_default(),
            author_id: preset.author_id.as_deref().unwrap_or_default(),
            creator: request.creator,
            now: request.now,
            coros_version,
            instrument,
            tags: &preset.tags,
        })
    };
    let (path, written, search_index) = match (new_slot, current.path.clone()) {
        (None, Some(path)) => {
            stamp(&path, &mut preset)?;
            // A plain Save keeps the slot's instrument (`0x5b9078` passes record `+0x5c`).
            let instrument = match instrument {
                PreferredInstrument::NotSet => {
                    (stored_sidecar.as_ref()).map_or(Instrument::NotSet, Sidecar::instrument)
                }
                instrument => sidecar_instrument(instrument),
            };
            // I: an unchanged preset keeps its stored `corosVersion` (record `+0x20` is the
            // preset's `+0x16ac`, a field tetrad-preset does not map).
            let coros_version = (stored_sidecar.as_ref())
                .and_then(|sidecar| sidecar.text("corosVersion"))
                .filter(|_| !changed)
                .unwrap_or(tetrad_firmware::running().version);
            let sidecar = saved_sidecar(&preset, instrument, coros_version);
            let written = rewrite(library, &path, &exported(&preset, specs)?, Some(sidecar))?;
            (path, written, None)
        }
        (Some((setlist, slot)), _) => {
            let expected = format!("{}/{}.pb", setlist.path, request.name);
            stamp(&expected, &mut preset)?;
            let sidecar = saved_sidecar(
                &preset,
                sidecar_instrument(instrument),
                tetrad_firmware::running().version,
            );
            let document = PresetDocument::parse(
                &exported(&preset, specs)?.to_bytes(),
                tetrad_preset::Limits::default(),
            )?;
            let stored = match overwrite {
                true => library.replace(&setlist.name, slot, request.name, &document, &sidecar)?,
                false => library.store(&setlist.name, slot, request.name, &document, &sidecar)?,
            };
            // A taken file name gets a `_<n>` suffix; the hash follows the real path.
            if stored.value != expected {
                stamp(&stored.value, &mut preset)?;
                rewrite(library, &stored.value, &exported(&preset, specs)?, None)?;
            }
            (stored.value, true, stored.search_index)
        }
        (None, None) => return Err(PresetError::Unsupported("a new preset without a slot")),
    };
    current.path = Some(path.clone());
    current.preset = preset;
    Ok(SavedPreset {
        path,
        written,
        search_index,
    })
}

/// Replaces a stored preset's document and, with `sidecar`, its whole `.json` sidecar (a
/// save: [`Sidecar::saved_preset`]); `None` keeps the stored sidecar. Unchanged bytes write
/// nothing.
fn rewrite(
    library: &mut StockLibrary,
    path: &str,
    preset: &Preset,
    sidecar: Option<Sidecar>,
) -> Result<bool, PresetError> {
    let mut stock = library.load(path)?;
    stock.document.modify(|stored| {
        *stored = preset.clone();
        Ok(())
    })?;
    if sidecar.is_some() {
        stock.sidecar = sidecar;
    }
    Ok(library.save(&mut stock)? == StockSave::Written)
}

/// Stock undo and redo stacks hold at most 20 frames each (`0x00648f1c`, `0x649120`).
pub const HISTORY_DEPTH: usize = 20;
/// An undo timer path records only when its single-shot 500 ms timer is idle, then
/// restarts it (`0x59b408`, `0x170074`).
pub const HISTORY_MERGE: Duration = Duration::from_millis(500);

/// A whole-preset undo frame (`UndoRedoPresetFrame`, `0x00647e04`). The layout is kept
/// beside the stored preset because port, split and mix edits are deployed only.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub(super) preset: Preset,
    layout: Layout,
}

impl Snapshot {
    pub fn of(current: &Current) -> Self {
        Self {
            preset: current.preset.clone(),
            layout: current.deployment.layout().clone(),
        }
    }

    /// The live preset equals this frame, as the stock test `0x2e2490` compares the preset:
    /// float param values use `0x5ce580`, so two NaN are equal and others match within a
    /// relative 1e-5 (its step rounding for stepped parameters needs the catalog and is not
    /// applied). The deployed layout must match too, except its parameter values and
    /// bypasses, which the stored preset holds (a momentary stomp held through a save keeps
    /// its live bypass in the layout only: review r15 R18, P-10).
    pub fn matches(&self, current: &Current) -> bool {
        same_preset(&self.preset, &current.preset)
            && same_routing(&self.layout, current.deployment.layout())
    }
}

/// Two layouts equal apart from their stored parameter values and bypasses.
fn same_routing(a: &Layout, b: &Layout) -> bool {
    if a == b {
        return true;
    }
    let without_values = |layout: &Layout| {
        let mut layout = layout.clone();
        let sites: Vec<Site> = layout.sites().map(|(site, _)| site).collect();
        for site in sites {
            if let Some(instance) = layout.instance_mut(site) {
                instance.params.fill(tetrad_engine::Scened::Fixed(0.0));
                instance.bypass = tetrad_engine::Scened::Fixed(false);
            }
        }
        layout
    };
    without_values(a) == without_values(b)
}

fn same_preset(a: &Preset, b: &Preset) -> bool {
    fn stock_equal(x: &SceneValue, y: &SceneValue) -> bool {
        match (&x.value, &y.value) {
            (Some(ParamValue::Float(x)), Some(ParamValue::Float(y))) => {
                (x.is_nan() && y.is_nan()) || (x - y).abs() <= x.abs().min(y.abs()) * 1e-5
            }
            _ => false,
        }
    }
    if a == b {
        return true;
    }
    let differ_only_in_value = |x: &SceneValue, y: &SceneValue| x != y && stock_equal(x, y);
    // The pairs of `param_values`, model by model: where the shapes differ, so do the presets.
    let pairs = |x: &Param, y: &Param| {
        (x.values.iter().zip(&y.values)).any(|(x, y)| differ_only_in_value(x, y))
    };
    if !models(a)
        .zip(models(b))
        .any(|(x, y)| x.params.iter().zip(&y.params).any(|(x, y)| pairs(x, y)))
    {
        return false;
    }
    let (mut a, mut b) = (a.clone(), b.clone());
    for (x, y) in param_values_mut(&mut a).zip(param_values_mut(&mut b)) {
        if differ_only_in_value(x, y) {
            x.value = Some(ParamValue::Float(0.0));
            y.value = x.value.clone();
        }
    }
    a == b
}

/// Every model parameter scene value in stored order: per chain the blocks, splitters,
/// mixers, lane controls, then the tempo models. A shape difference anywhere makes the
/// presets unequal, so two walks stay aligned wherever it matters.
pub(super) fn param_values(preset: &Preset) -> impl Iterator<Item = &SceneValue> {
    models(preset)
        .flat_map(|model| &model.params)
        .flat_map(|param| &param.values)
}

/// The models of [`param_values`], in its order.
fn models(preset: &Preset) -> impl Iterator<Item = &Model> {
    preset
        .chains
        .iter()
        .flat_map(|chain| {
            [
                &chain.models,
                &chain.splitter,
                &chain.mixer,
                &chain.output_control,
                &chain.combined_splitter,
                &chain.input_control,
            ]
        })
        .flatten()
        .chain(&preset.tempo_program_data)
}

fn param_values_mut(preset: &mut Preset) -> impl Iterator<Item = &mut SceneValue> {
    preset
        .chains
        .iter_mut()
        .flat_map(|chain| {
            [
                &mut chain.models,
                &mut chain.splitter,
                &mut chain.mixer,
                &mut chain.output_control,
                &mut chain.combined_splitter,
                &mut chain.input_control,
            ]
        })
        .flatten()
        .chain(&mut preset.tempo_program_data)
        .flat_map(|model| &mut model.params)
        .flat_map(|param| &mut param.values)
}

/// The stock path that pushes an undo frame (`0x648f1c`, type 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoPath {
    /// The Grid view's 500 ms timer (`+0x1810`): only the UI value setters `0x59b3ac`
    /// (block value `0x523644`), `0x59b434` (splitter `0x5201a4`) and `0x59b4bc` (mixer
    /// `0x5202a8`). Every other UI edit is [`Self::Direct`].
    View,
    /// The Cortex Control Grid timer (`0x17004c`): Control value edits (block, lane, input
    /// gate and junction values, junction bypass).
    ControlTimer,
    /// A direct call: Control block set, remove and move, ports, split and mix, block bypass,
    /// expression links and switches (`0x171cb4`, `0x17212c`, `0x254a70`, `0x2563fc`),
    /// RecallPreset (`0x5b22d4`); UI structural edits (`0x59b714`, `0x59b95c`, `0x59ccc4`,
    /// `0x59d2e8`, `0x59d4b4`, `0x59ec78`, `0x59ee7c`, `0x59f620`, `0x5acfb8`, drag
    /// `0x5c8354`), bypass `0x5ad868`, MUTE `0x5ad95c`, scene texts `0x6795c0`/`0x679664`.
    Direct,
}

/// An undo frame taken before an edit ([`History::begin`]); [`History::commit`] keeps it
/// once the edit commits. Stock edits cannot be refused after the frame; Tetrad edits can.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingFrame {
    frame: Option<Box<Snapshot>>,
    owner: LoadId,
    path: UndoPath,
    at: Duration,
}

/// The identity of one loaded preset (review r39 K1, K2; O). Every load gives a new one; the
/// undo of a load gives back the one it replaced. A preset frame applies only to the preset it
/// was recorded on, and a slot frame takes the loaded preset to its file only when the slot
/// held that preset. Stock frames have no owner: stock never drops a load frame, so its preset
/// frames always sit under their own load frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoadId(u64);

/// One slot of a library swap frame: the setlist, the slot and the preset file the slot
/// names after the swap (`0x648a24` keeps the full record; Tetrad keeps its path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapSide {
    pub setlist: String,
    pub slot: usize,
    pub path: Option<String>,
}

/// One setlist slot of a file delete (type 1) or copy/move (type 3) frame (`0x648ab0`): what
/// the slot held, which an undo puts back, and the file the slot names now.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotState {
    pub setlist: String,
    pub slot: usize,
    /// The file the slot names now. Owned: a step writes nothing when the slot changed since
    /// (stock `0x647fa8` has no check and deletes whatever is there).
    pub expect: Option<String>,
    /// The preset the slot held (`None`: empty), read in full before the operation.
    pub(crate) held: Option<Box<Held>>,
}

/// A preset a [`SlotState`] puts back: its copy source, its file then and whether the preset
/// favorites named the file (`+0x1758`; the restore re-adds it).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Held {
    pub(crate) source: CopySource,
    pub(crate) path: String,
    pub(crate) favorite: bool,
    /// The loaded preset, when this file was its file (review r39 K1).
    pub(crate) loaded: Option<LoadId>,
    /// The stamp of the file then (review r41 L1); `None` when it does not resolve.
    pub(crate) file: Option<PresetFile>,
}

/// The loaded preset's file and identity, which [`SlotState::read`] and [`SlotState::held`]
/// mark in a slot that holds that file.
pub(crate) type LoadedFile<'a> = Option<(&'a str, LoadId)>;

impl SlotState {
    /// Slot `slot` of `setlist` as it is now; `expect` is set by the caller after the
    /// operation. Refused: a slot out of range, an unreadable preset or one without a name.
    pub(crate) fn read(
        library: &StockLibrary,
        setlist: &str,
        slot: usize,
        loaded: LoadedFile<'_>,
    ) -> Result<Self, PresetError> {
        let path = library
            .slots(setlist)?
            .get(slot)
            .ok_or(tetrad_library::Error::SlotRange(slot))?
            .clone();
        let held = match path {
            Some(path) => {
                let source = copy_source(library, setlist, slot)?;
                Some(Self::held(library, source, path, loaded)?)
            }
            None => None,
        };
        Ok(Self {
            setlist: setlist.to_owned(),
            slot,
            expect: None,
            held,
        })
    }

    /// A slot that held `source` from file `path` (a source already read for the copy).
    pub(crate) fn held(
        library: &StockLibrary,
        source: CopySource,
        path: String,
        loaded: LoadedFile<'_>,
    ) -> Result<Box<Held>, PresetError> {
        let favorites = library.items(ItemList::Favorites(Collection::Presets))?;
        let favorite = favorites.iter().any(|entry| entry.item == path);
        let loaded = loaded.and_then(|(file, id)| (file == path).then_some(id));
        let file = library.media().resolve(&path).ok().map(PresetFile::read);
        Ok(Box::new(Held {
            source,
            path,
            favorite,
            loaded,
            file,
        }))
    }
}

/// What a library frame (type 1 or 3) records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotOperation {
    /// Type 1, `UndoRedoFileSystemFrame`: a setlist preset delete (`0x1d916c`).
    Delete,
    /// Type 3, `UndoRedoCopyOrMoveFrame`: a copy or move into a setlist slot (`0x1de9f8`).
    CopyOrMove,
}

/// A preset load frame (type 5, `UndoRedoPresetLoadFrame`, `0x647e6c`): the selection and the
/// working preset before the load.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadState {
    /// The slot that names the file (`+0x16d0`, `+0x16d4`); `None` for a file no setlist slot
    /// names (an audition of a plugin folder or Downloads preset).
    pub slot: Option<PresetRef>,
    /// Its file (`+0x16d8`); `None` for a new preset in an empty slot.
    pub path: Option<String>,
    /// The stamp of that file (review r39 K4, O; [`History::refile`]); `None` when it does
    /// not read.
    pub file: Option<PresetFile>,
    /// The working preset, with its unsaved edits.
    pub working: Snapshot,
    /// That preset's identity ([`History::loaded`]).
    pub owner: LoadId,
}

/// An undo or redo frame (`UndoRedoController` frame types, `0x6476c8`). Type 4 (Zenjack
/// settings) has no stock caller (re-r38 §4) and is not modelled.
#[derive(Debug, Clone, PartialEq)]
pub enum UndoFrame {
    /// Type 0, `UndoRedoPresetFrame` (`0x647e04`), of the preset `owner`.
    Preset { frame: Box<Snapshot>, owner: LoadId },
    /// Type 2, the swap frame (`0x648a24`): both slots as they are after the swap.
    LibrarySwap([SwapSide; 2]),
    /// Types 1 and 3: the setlist slots the operation changed. A move lists the target, then
    /// the source. `working` is the working preset when the frame was pushed (`0x648ab0`
    /// `+0x90`) and `owner` its identity: a step that puts a preset back into the selected
    /// slot plays it there (`0x5b2ce8`).
    Slots {
        operation: SlotOperation,
        slots: Vec<SlotState>,
        working: Option<Box<Snapshot>>,
        owner: LoadId,
    },
    /// Type 5.
    PresetLoad(Box<LoadState>),
}

impl UndoFrame {
    /// The stock frame type that Cortex Control UndoRedo lists (`0x6476c8`).
    pub fn stock_type(&self) -> i32 {
        match self {
            Self::Preset { .. } => 0,
            Self::Slots {
                operation: SlotOperation::Delete,
                ..
            } => 1,
            Self::LibrarySwap(_) => 2,
            Self::Slots {
                operation: SlotOperation::CopyOrMove,
                ..
            } => 3,
            Self::PresetLoad(_) => 5,
        }
    }
}

/// Preset and library undo/redo (`UndoRedoController`, `0x006477b4`): one stack of at most
/// [`HISTORY_DEPTH`] frames of every type. Preset frames are pushed before an edit; library
/// frames after a good write; a load frame by each load. Not modelled: the history-off
/// windows (model browser preview, self test). Owned (review r39 K2): the newest frame of a
/// stack is never a preset frame of another preset than the loaded one ([`LoadId`]); such
/// frames drop when a step or a dropped frame leaves them on top.
#[derive(Debug, Clone, Default)]
pub struct History {
    undo: Vec<UndoFrame>,
    redo: Vec<UndoFrame>,
    /// The last edit on each timer path.
    view_edit: Option<Duration>,
    control_edit: Option<Duration>,
    loaded: LoadId,
    /// The newest identity a load got.
    issued: LoadId,
}

impl History {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Undo and redo frame counts.
    pub fn depths(&self) -> (usize, usize) {
        (self.undo.len(), self.redo.len())
    }

    /// The stock type of each undo and redo frame, oldest first ([`UndoFrame::stock_type`]).
    pub fn frame_types(&self) -> (Vec<i32>, Vec<i32>) {
        let types = |stack: &[UndoFrame]| stack.iter().map(UndoFrame::stock_type).collect();
        (types(&self.undo), types(&self.redo))
    }

    /// The newest frame of the undo (or redo) stack.
    pub fn newest(&self, redo: bool) -> Option<&UndoFrame> {
        if redo {
            self.redo.last()
        } else {
            self.undo.last()
        }
    }

    /// The identity of the loaded preset.
    pub fn loaded(&self) -> LoadId {
        self.loaded
    }

    /// Clear all (`0x00647ac0`): after a backup restore reload (`0x3fd3e8`) and a factory
    /// restore (`0x4dea3c`). A load clears nothing (C): it pushes a load frame.
    pub fn clear_all(&mut self) {
        *self = Self::default();
    }

    /// A setlist delete drops the frames of that setlist (`0x76c170` → `0x647c84`). Owned: a
    /// setlist rename drops them too (stock frames hold the folder, Tetrad frames its name).
    /// Load frames name the setlist directory; their step checks the slot instead.
    pub fn forget_setlist(&mut self, setlist: &str) {
        let names = |frame: &UndoFrame| match frame {
            UndoFrame::LibrarySwap(sides) => sides.iter().any(|side| side.setlist == setlist),
            UndoFrame::Slots { slots, .. } => slots.iter().any(|slot| slot.setlist == setlist),
            UndoFrame::Preset { .. } | UndoFrame::PresetLoad(_) => false,
        };
        for stack in [&mut self.undo, &mut self.redo] {
            stack.retain(|frame| !names(frame));
        }
        self.drop_strays();
    }

    /// A UI edit: [`Self::begin`] and [`Self::commit`] at once.
    pub fn record(&mut self, current: &Current, now: Duration, path: UndoPath) {
        let frame = self.begin(current, now, path);
        self.commit(frame);
    }

    /// Before an edit at device time `now`: the frame `path` pushes. A timer path records
    /// only when its timer is idle (no edit on that path in the last 500 ms); no path
    /// pushes a frame equal to the newest one (`0x00647944`, tolerant `0x2e29a8`).
    pub fn begin(&self, current: &Current, now: Duration, path: UndoPath) -> PendingFrame {
        let last = match path {
            UndoPath::View => self.view_edit,
            UndoPath::ControlTimer => self.control_edit,
            UndoPath::Direct => None,
        };
        let idle = last.is_none_or(|last| now.saturating_sub(last) >= HISTORY_MERGE);
        let fresh = !matches!(self.undo.last(), Some(UndoFrame::Preset { frame, .. }) if frame.matches(current));
        PendingFrame {
            frame: (idle && fresh).then(|| Box::new(Snapshot::of(current))),
            owner: self.loaded,
            path,
            at: now,
        }
    }

    /// After the edit committed: a timer path restarts its timer; a frame drops the oldest
    /// past [`HISTORY_DEPTH`] and clears the redo stack. A frame begun before a load belongs
    /// to the old preset and is not kept (review r39 K2).
    pub fn commit(&mut self, pending: PendingFrame) {
        match pending.path {
            UndoPath::View => self.view_edit = Some(pending.at),
            UndoPath::ControlTimer => self.control_edit = Some(pending.at),
            UndoPath::Direct => {}
        }
        if let Some(frame) = pending.frame.filter(|_| pending.owner == self.loaded) {
            let owner = pending.owner;
            self.push_new(UndoFrame::Preset { frame, owner });
        }
    }

    /// After a good library swap: its frame (`0x648f1c(…, 2, …)`, which clears redo).
    pub fn record_swap(&mut self, sides: [SwapSide; 2]) {
        self.push_new(UndoFrame::LibrarySwap(sides));
    }

    /// After a good setlist delete, copy or move: its frame (`0x648f1c(…, 1 or 3, …)`) with
    /// the working preset `current`.
    pub fn record_slots(
        &mut self,
        operation: SlotOperation,
        slots: Vec<SlotState>,
        current: Option<&Current>,
    ) {
        let frame = UndoFrame::Slots {
            operation,
            slots,
            working: current.map(|current| Box::new(Snapshot::of(current))),
            owner: self.loaded,
        };
        self.push_new(frame);
    }

    /// A slot step played the frame's working preset in the selected slot (`0x5b2ce8`): it is
    /// the preset `owner` again, so its preset frames apply. Both timers restart idle.
    pub fn resume(&mut self, owner: LoadId) {
        self.loaded = owner;
        self.view_edit = None;
        self.control_edit = None;
        self.drop_strays();
    }

    /// A preset load: `before`, the frame of what was loaded before (`0x648f1c(…, 5, …)`),
    /// and a new identity for the preset loaded now. `None` is a load that pushes no frame
    /// (the boot load; a MIDI program change, `0x5b5bd8` → `0x5b2f40`): the old preset's
    /// frames on top drop. Owned: both timers restart idle, as after an undo.
    pub fn record_load(&mut self, before: Option<LoadState>) {
        if let Some(before) = before {
            self.push_new(UndoFrame::PresetLoad(Box::new(before)));
        }
        self.issued.0 += 1;
        self.loaded = self.issued;
        self.view_edit = None;
        self.control_edit = None;
        self.drop_strays();
    }

    fn push_new(&mut self, frame: UndoFrame) {
        push(&mut self.undo, frame);
        self.redo.clear();
    }

    /// Undo (`0x649330`) or redo (`0x649b7c`) of a preset frame: the newest frame of one
    /// stack, with the current preset pushed to the other. `None` when the stack is empty or
    /// its newest frame is another type ([`Self::newest`]). Owned: both timers restart idle.
    pub fn step(&mut self, current: &Current, redo: bool) -> Option<Snapshot> {
        self.drop_strays();
        let owner = self.loaded;
        let (from, to) = self.stacks(redo);
        let frame = match from.pop()? {
            UndoFrame::Preset { frame, .. } => *frame,
            other => {
                from.push(other);
                return None;
            }
        };
        let current = Box::new(Snapshot::of(current));
        push(
            to,
            UndoFrame::Preset {
                frame: current,
                owner,
            },
        );
        self.view_edit = None;
        self.control_edit = None;
        Some(frame)
    }

    /// The newest frame of the undo (or redo) stack when it is a library swap.
    pub fn newest_swap(&self, redo: bool) -> Option<&[SwapSide; 2]> {
        match self.newest(redo) {
            Some(UndoFrame::LibrarySwap(sides)) => Some(sides),
            _ => None,
        }
    }

    /// Ends the undo or redo of the [`Self::newest_swap`] frame: it leaves its stack, and
    /// `done` (the slots after the swap back) goes onto the other one (`0x649330` case 2).
    /// `None` drops the frame (a stale frame, owned).
    pub fn step_swap(&mut self, redo: bool, done: Option<[SwapSide; 2]>) {
        if matches!(self.newest(redo), Some(UndoFrame::LibrarySwap(_))) {
            self.step_library(redo, None, done.map(UndoFrame::LibrarySwap));
        }
    }

    /// Ends the undo or redo of the newest library frame (types 1, 2, 3): it leaves its
    /// stack; `keep` (the part not applied) goes back onto it and `reverse` (the reverse
    /// frame, `0x21e42c`) onto the other one. Both `None` drop the frame (owned).
    pub fn step_library(
        &mut self,
        redo: bool,
        keep: Option<UndoFrame>,
        reverse: Option<UndoFrame>,
    ) {
        let (from, to) = self.stacks(redo);
        if !matches!(
            from.last(),
            Some(UndoFrame::LibrarySwap(_) | UndoFrame::Slots { .. })
        ) {
            return;
        }
        from.pop();
        from.extend(keep);
        if let Some(reverse) = reverse {
            push(to, reverse);
        }
        self.drop_strays();
    }

    /// Ends the undo or redo of the newest load frame (type 5, `0x649330` case 5): `reverse`
    /// (the selection before the step) goes onto the other stack and the frame's preset is the
    /// loaded one again. `None` drops the frame (owned: a changed file, a failed load), and
    /// with it the frames of its preset on top (review r39 K2). Both timers restart idle.
    pub fn step_load(&mut self, redo: bool, reverse: Option<LoadState>) {
        let (from, to) = self.stacks(redo);
        let Some(UndoFrame::PresetLoad(frame)) = from.last() else {
            return;
        };
        let owner = frame.owner;
        from.pop();
        if let Some(reverse) = reverse {
            push(to, UndoFrame::PresetLoad(Box::new(reverse)));
            self.loaded = owner;
        }
        self.view_edit = None;
        self.control_edit = None;
        self.drop_strays();
    }

    /// Tetrad rewrote the file `from` for the preset it holds (review r41 L1, O): a save of the
    /// loaded preset in place, or a library step that put a deleted preset back at its own
    /// path. Load frames of `from` take the stamp `to` (the same path), so their step still
    /// loads, as stock does. Any other change of the file still refuses (K4).
    pub fn refile(&mut self, from: &PresetFile, to: PresetFile) {
        if from.host != to.host || !to.unchanged() {
            return;
        }
        for frame in self.undo.iter_mut().chain(&mut self.redo) {
            if let UndoFrame::PresetLoad(state) = frame
                && state.file.as_ref() == Some(from)
            {
                state.file = Some(to.clone());
            }
        }
    }

    /// A load step whose working preset could not be put back (review r39 K7, O): it waits on
    /// top of the stack the load frame came from, as a preset frame of the loaded preset, so
    /// the next step puts it back and its unsaved edits are not lost.
    pub fn keep_working(&mut self, redo: bool, working: Snapshot) {
        let owner = self.loaded;
        let (from, _) = self.stacks(redo);
        let frame = Box::new(working);
        push(from, UndoFrame::Preset { frame, owner });
    }

    /// Drops the preset frames of another preset than the loaded one from the top of both
    /// stacks (review r39 K2): a step would put that preset's content into the loaded one.
    fn drop_strays(&mut self) {
        let loaded = self.loaded;
        for stack in [&mut self.undo, &mut self.redo] {
            while matches!(stack.last(), Some(UndoFrame::Preset { owner, .. }) if *owner != loaded)
            {
                stack.pop();
            }
        }
    }

    fn stacks(&mut self, redo: bool) -> (&mut Vec<UndoFrame>, &mut Vec<UndoFrame>) {
        if redo {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        }
    }

    /// A save renames the preset in every preset frame (`0x00647878`).
    pub fn rename(&mut self, name: &str) {
        for frame in self.undo.iter_mut().chain(&mut self.redo) {
            if let UndoFrame::Preset { frame, .. } = frame {
                frame.preset.name = Some(name.to_owned());
            }
        }
    }
}

fn push(stack: &mut Vec<UndoFrame>, frame: UndoFrame) {
    stack.push(frame);
    if stack.len() > HISTORY_DEPTH {
        stack.remove(0);
    }
}

/// What restoring a whole preset needs: nothing on the DSP when only stored texts differ.
#[derive(Debug)]
pub enum Restore {
    Preset(Box<Preset>),
    Layout(LayoutChange),
}

impl Restore {
    /// The stock restore (`0x00527d80`): a frame equal to the live preset changes nothing;
    /// otherwise the whole preset is taken and pushed. Inferred: tetrad-engine has no
    /// value-only replace, so any layout difference is one recompile with core reuse.
    pub fn prepare(current: &Current, specs: &Specs, frame: Snapshot) -> Result<Self, PresetError> {
        let Snapshot { preset, layout } = frame;
        if layout == *current.deployment.layout() {
            return Ok(Self::Preset(Box::new(preset)));
        }
        let mut deployment = current.deployment.clone();
        let update = deployment.edit(specs, |live| {
            *live = layout;
            Ok(())
        })?;
        Ok(Self::Layout(LayoutChange {
            update,
            preset: Box::new(preset),
            deployment: Box::new(deployment),
            undo: None,
        }))
    }
}

/// Stock scene copy or swap (`0x0051a2a8` → `0x0050e274`) in the stored preset and the
/// deployed layout: per-scene values, bypass, label and color. The caller then recalls
/// `second` (the copy target, `0x5899d8` forced).
pub fn copy_scene(
    current: &Current,
    specs: &Specs,
    first: SceneIndex,
    second: SceneIndex,
    swap: bool,
) -> Result<Restore, PresetError> {
    if first == second {
        return Err(PresetError::Unsupported("a scene copy onto itself"));
    }
    let (a, b) = (first.index(), second.index());
    let mut preset = current.preset.clone();
    if swap {
        preset.swap_scenes(a, b)?;
    } else {
        preset.copy_scene(a, b)?;
    }
    let mut layout = current.deployment.layout().clone();
    let sites: Vec<Site> = layout.sites().map(|(site, _)| site).collect();
    for site in sites {
        let Some(instance) = layout.instance_mut(site) else {
            continue;
        };
        for param in &mut instance.params {
            scene_values(param, a, b, swap);
        }
        scene_values(&mut instance.bypass, a, b, swap);
        scene_values(&mut instance.mute, a, b, swap);
        for text in &mut instance.texts {
            if text.scenes.len() > a.max(b) {
                if swap {
                    text.scenes.swap(a, b);
                } else {
                    text.scenes[b] = text.scenes[a].clone();
                }
            }
        }
    }
    Restore::prepare(current, specs, Snapshot { preset, layout })
}

fn scene_values<T: Copy>(value: &mut tetrad_engine::Scened<T>, a: usize, b: usize, swap: bool) {
    if let tetrad_engine::Scened::PerScene(values) = value {
        if swap {
            values.swap(a, b);
        } else {
            values[b] = values[a];
        }
    }
}

/// `0x007c3d94`: the first empty slot of a setlist, scanning from slot 0.
pub fn first_free_slot(
    library: &StockLibrary,
    setlist: &str,
) -> Result<Option<usize>, PresetError> {
    Ok(library.slots(setlist)?.iter().position(Option::is_none))
}

/// `0x007c3fd0` (Create New): the first empty slot of the first user setlist that has
/// one. `None` is the stock "Setlists full" case (`0x11e1b1c`).
pub fn global_free_slot(library: &StockLibrary) -> Result<Option<PresetRef>, PresetError> {
    for setlist in library.setlists()? {
        if let Some(index) = first_free_slot(library, &setlist.name)? {
            return Ok(Some(PresetRef {
                setlist: Some(setlist.path),
                index: index as u32,
            }));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODELS: &str = "<Models>\
<Category id='10' name='Splitter' hidden='true'><Model id='10004' name='Splitter'>\
<Parameter name='TYPE' type='switch' min='0' max='2' steps='3' defaultValue='0'/></Model></Category>\
<Category id='25' name='Tempo' hidden='true'><Model id='25000' name='Tempo'>\
<Parameter name='TEMPO' type='float' min='40' max='240' defaultValue='120'/></Model></Category>\
<Category id='12' name='Cab'><Model id='12000' name='Base Cab'>\
<Parameter name='A' type='float' defaultValue='0'/><Parameter name='B' type='float' defaultValue='0'/>\
<Parameter name='C' type='float' defaultValue='0'/></Model>\
<Model id='12001' name='Clone Cab' clones='12000'>\
<Parameter name='ir selector' type='string' defaultValue='IR' replaces='0'/></Model></Category>\
</Models>";

    fn specs() -> Specs {
        Specs::resolve(&Catalog::parse(MODELS, "<ModuleReport/>").unwrap()).unwrap()
    }

    /// Legacy splitters and tempo migrate through the catalog (`0x6e4148`, `0x64b8f8` mode
    /// 1), and cab clones describe their base list plus MUTE.
    #[test]
    fn catalog_maps_splitter_type_tempo_and_cab_clones() {
        let specs = specs();
        let catalog = SpecsCatalog(&specs);
        assert_eq!(catalog.splitter_type(SplitterType::Type0), Some(0.0));
        assert_eq!(catalog.splitter_type(SplitterType::Type2), Some(1.0));
        assert_eq!(catalog.tempo_value(140), Some(0.5));
        let cab = catalog.model(12001).unwrap();
        assert_eq!(cab.params.len(), 4, "three base parameters and MUTE");
        assert_eq!(cab.params[0].default, ParamValue::String("IR".into()));
        assert!(!catalog.is_complete() && DeviceCatalog(&specs).is_complete());
    }

    /// `0x1f7b10` writes each Virtual Device Preset value into all eight scenes (`0x2e8160`):
    /// a scene-mode parameter keeps its flag and holds one value everywhere.
    #[test]
    fn model_preset_rows_fill_every_scene() {
        let specs = specs();
        let scene = |index| SceneIndex::new(index).unwrap();
        let mut original = tetrad_engine::new_instance(specs.model(12000).unwrap());
        original.scene_mode[0] = true;
        original.set_param(0, scene(3), 0.9);
        let row = ParamEntry {
            index: 0,
            value: "0.25".into(),
            exp: 0,
            exp_trim_max: 1.0,
            exp_trim_min: 0.0,
        };
        let block = model_preset_block(&specs, 12000, Some(&original), &[row]).unwrap();
        assert!(block.scene_mode[0]);
        assert!((0..8).all(|index| block.params[0].at(scene(index)) == 0.25));
    }

    /// Owned safety (review-w6 F12): a user Virtual Device Preset row that is not a finite
    /// value in 0..=1 never reaches `set_param`; the block keeps the original value.
    #[test]
    fn model_preset_rows_outside_0_to_1_are_left_out() {
        let specs = specs();
        let scene = SceneIndex::new(0).unwrap();
        let mut original = tetrad_engine::new_instance(specs.model(12000).unwrap());
        original.set_param(1, scene, 0.5);
        let rows: Vec<_> = ["NaN", "inf", "1.5", "-0.1", "0.75"]
            .into_iter()
            .map(|value| ParamEntry {
                index: if value == "0.75" { 0 } else { 1 },
                value: value.into(),
                exp: 0,
                exp_trim_max: 1.0,
                exp_trim_min: 0.0,
            })
            .collect();
        let block = model_preset_block(&specs, 12000, Some(&original), &rows).unwrap();
        assert_eq!(block.params[1].at(scene), 0.5);
        assert_eq!(block.params[0].at(scene), 0.75);
        assert_eq!(rejected_model_preset_rows(&specs, 12000, &rows), 4);
    }
    /// (O) A community block leaves Tetrad as its stock placeholder at its defaults, with a
    /// record. The next load puts the same block back; a catalog without the block keeps the
    /// placeholder and reports the block as missing.
    #[test]
    fn community_blocks_leave_as_placeholders_and_come_back() {
        const GAIN: &str = "<Models><Category id='16' name='Utility'>\
<Model id='16005' name='Gain' blob='gainblock'>\
<Parameter name='LEVEL' type='float' min='-60' max='12' defaultValue='0'/></Model>\
</Category></Models>";
        const ID: u32 = 812;
        let mut specs = specs();
        let gain = Specs::resolve(&Catalog::parse(GAIN, "<ModuleReport/>").unwrap()).unwrap();
        specs.models.extend(gain.models);
        let block = qcdsp::examples::saturator()
            .unwrap()
            .renamed("tx_test", "Saturator", qcdsp::Category::GuitarOverdrive)
            .unwrap();
        let resources = qcdsp::device::Resources {
            code_bytes: 4096,
            state_bytes: 512,
            cycles_per_call: 9000,
            network_cycles_per_call: 0,
        };
        let report =
            qcdsp::device::module_element(&block, "qcdsp_tx_test", ID, "Saturator", &resources)
                .unwrap();
        let community = tetrad_catalog::community::CommunitySpec {
            key: "acme/blocks:acme.drive/sat".into(),
            placeholder: 16_005,
        };
        let spec = tetrad_catalog::community::spec(&block, ID, "qcdsp_tx_test", &report, community)
            .unwrap();
        let scene = SceneIndex::new(0).unwrap();
        let mut stored = stored_values(&tetrad_engine::new_instance(&spec), scene);
        stored[0] = ParamValue::Float(0.75);
        specs.models.insert(ID, spec);
        let chain = tetrad_preset::Chain {
            models: vec![Model::empty_slot(); 8],
            ..Default::default()
        };
        let mut preset = Preset {
            chains: vec![chain; 4],
            ..Preset::default()
        };
        preset.chains[1].models[3] = Model::block(ID, stored);
        let original = preset.chains[1].models[3].clone();

        let saved = exported(&preset, &specs).unwrap();
        let placeholder = &saved.chains[1].models[3];
        assert_eq!(placeholder.hash, Some(16_005));
        assert_eq!(
            placeholder.params[0].value(0),
            Some(&ParamValue::Float(60.0 / 72.0))
        );
        let mut loaded =
            Preset::decode(&saved.to_bytes(), tetrad_preset::Limits::default()).unwrap();
        assert!(restore_community(&mut loaded, &specs).is_empty());
        assert_eq!(loaded.chains[1].models[3], original);

        let mut without = specs.clone();
        without.remove_community();
        let mut loaded =
            Preset::decode(&saved.to_bytes(), tetrad_preset::Limits::default()).unwrap();
        let missing = restore_community(&mut loaded, &without);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].key, "acme/blocks:acme.drive/sat");
        assert_eq!(loaded.chains[1].models[3].hash, Some(16_005));
    }
}

//! Resolved model metadata for the compiler, UI and control layers.
//!
//! [`Specs::resolve`] turns the preserved XML view into typed, numeric specs:
//! parameter laws come from `tetrad_core::conversion` (the one owner of the stock
//! symbol table and value laws), categories become [`ModelRole`]s and module
//! reports become [`ModuleStats`]. Raw attributes stay on [`Catalog`].

use std::collections::BTreeMap;

use serde::Serialize;
use tetrad_core::conversion::{
    ConversionError, DefaultValue, ParameterScale, XmlParameter, metadata_float,
};

use crate::{Catalog, Hidden, ParameterReference, ParameterType};

/// What a model is for, from its stock category (`ModelRepo.xml` category IDs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// A user-placeable grid block.
    Block,
    /// Category 10: a pair split point (`Splitter AB`, crossover, balance, …).
    Splitter,
    /// Category 11: a pair mix point.
    Mixer,
    /// Category 22: compiler-inserted routing (`ZenLinkRx/Tx/Fwd/Delay`).
    Routing,
    /// Category 23: per-row output control (volume, pan, mute, solo).
    LaneOutput,
    /// Category 28: per-row input gate control.
    InputGate,
    /// Category 25: tempo/metronome control.
    Tempo,
    /// Category 20: neural capture recorder/trainer/refiner.
    CaptureInternal,
    /// Category 31: global I/O settings.
    IoSettings,
    /// Categories 15 and 19: no placeable model.
    Reserved,
}

impl ModelRole {
    pub fn from_category(category: u32) -> Self {
        match category {
            10 => Self::Splitter,
            11 => Self::Mixer,
            15 | 19 => Self::Reserved,
            20 => Self::CaptureInternal,
            22 => Self::Routing,
            23 => Self::LaneOutput,
            25 => Self::Tempo,
            28 => Self::InputGate,
            31 => Self::IoSettings,
            _ => Self::Block,
        }
    }
}

/// Stock model IDs the compiler and importer refer to directly.
pub mod ids {
    pub const SPLITTER_AB: u32 = 10_000;
    /// The combined splitter (TYPE selects Balance, A/B or Crossover).
    pub const SPLITTER: u32 = 10_004;
    pub const MIXER: u32 = 11_000;
    pub const ZEN_LINK_RX: u32 = 22_000;
    pub const ZEN_LINK_TX: u32 = 22_001;
    pub const ZEN_LINK_FWD: u32 = 22_002;
    pub const ZEN_LINK_DELAY: u32 = 22_003;
    pub const LANE_OUTPUT_CONTROL: u32 = 23_000;
    pub const TEMPO_CONTROL: u32 = 25_000;
    pub const INPUT_GATE_CONTROL: u32 = 28_000;
    pub const IO_SETTINGS: u32 = 31_000;
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ParamDefault {
    Number(f32),
    Text(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ParamRange {
    pub min: f32,
    pub max: f32,
}

impl ParamRange {
    pub fn contains(self, value: f32) -> bool {
        (self.min..=self.max).contains(&value)
    }
}

/// End labels such as `min_string="OFF"` or the `L`/`C`/`R` pan labels.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RangeLabels {
    pub min: Option<String>,
    pub mid: Option<String>,
    pub max: Option<String>,
}

/// One parameter. `index` is the XML position, which is also the DSP and preset
/// parameter index (no stock XML entry stores an explicit `index`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParamSpec {
    pub index: u32,
    pub name: String,
    pub kind: ParameterType,
    /// Stored `min`/`max` (both present), resolved like stock (`0x4e9ed8`).
    pub range: Option<ParamRange>,
    /// Stored default: a display number, or text for string parameters. For
    /// piecewise parameters stock stores the normalized value here.
    pub default: Option<ParamDefault>,
    /// Stored skew; the scale uses 1 when it is absent (stock default).
    pub skew: Option<f32>,
    pub steps: Option<u32>,
    pub step_names: Vec<String>,
    pub units: Option<String>,
    pub labels: RangeLabels,
    pub tooltip: Option<String>,
    pub hidden: bool,
    /// Step names are rebuilt from the grid (sidechain source selectors).
    pub dynamic: bool,
    pub exp_assignable: Option<bool>,
    pub show_as_integer: bool,
    pub display_pos: Option<u32>,
    pub linked_scene_mode: Vec<u32>,
    /// Base index (clones) or old parameter (model updates) this one replaces.
    pub replaces: Vec<ParameterReference>,
    pub toggle_on: Vec<u32>,
    pub toggle_off: Vec<u32>,
    pub toggle_step: Vec<u32>,
    /// The stock conversion state (`zenUI::ParameterInfo`): power or piecewise
    /// law, steps and normalized default.
    #[serde(skip)]
    pub scale: ParameterScale,
}

impl ParamSpec {
    /// Display default: the stored default through the scale (absent defaults
    /// are normalized 0, so this is the range minimum).
    pub fn default_number(&self) -> Option<f32> {
        match self.scale.default {
            DefaultValue::Normalized(value) => self.scale.to_display(value).ok(),
            DefaultValue::Text(_) => None,
        }
    }

    /// Display value → stored normalized value (`tetrad_core` `setPhysical`). `None`
    /// where a piecewise law has no segment for the value (stock yields 0.0).
    pub fn normalize(&self, display: f32) -> Option<f32> {
        self.scale.to_normalized(display).ok()
    }

    /// The stored (normalized) default a new instance starts with. Piecewise
    /// parameters keep their stored default unchanged; NaN passes through.
    pub fn default_normalized(&self) -> f32 {
        match self.scale.default {
            DefaultValue::Normalized(value) => value,
            DefaultValue::Text(_) => 0.0,
        }
    }
}

/// `sidechain="n"` names the source selector parameter; `sidechain_in="m"` names
/// the parameter that picks the sidechain input channel (Doubler).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SidechainSpec {
    pub source_param: u32,
    pub input_param: Option<u32>,
}

/// Stock `zenDSPCompiler::ModuleStats` (0x2c bytes; ctor `0x5f9180`, accumulate
/// `0x5f90d0`). Loader `0x8826a4` builds it from `moduleReport.xml`, whose values
/// already include the ModelRepo `<Padding>` (report writer `0x3136a4`), so pads
/// are never added again. Units are the report's: CPU in percent of one SHARC
/// core, memory sizes as reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct ModuleStats {
    pub cpu: f32,
    pub dm_heap: u32,
    pub pm_heap: u32,
    pub sd_heap: u32,
    /// `dm` attribute.
    pub dmda: u32,
    /// `pm` attribute.
    pub pmda: u32,
    /// `sw + nw`: code memory, shared by later instances when `reuse_pmco`.
    pub pmco: u32,
    /// `dma` attribute.
    pub edma: f32,
    /// FIR cab engine instances; a core holds at most one.
    pub singletons: u32,
    /// Module latency in samples (`Latency`).
    pub latency: u32,
    pub reuse_pmco: bool,
    pub has_sidechain: bool,
}

impl ModuleStats {
    /// CPU above this is clamped ("Limiting cpu measurement", `0x120b82c`).
    pub const CPU_CLAMP: f32 = 90.0;
    /// Models whose CPU comes from the `max` of "Stimulus High" (Freeze, Rabea).
    pub const MAX_CPU_MODELS: [u32; 2] = [26_001, 6_030];

    /// Resource sum, as `0x5f90d0` accumulates one instance into a core.
    /// Code memory is skipped when `shared_code` (same model already on the core
    /// and `reuse_pmco`).
    pub fn add(&mut self, other: &Self, shared_code: bool) {
        self.cpu += other.cpu;
        self.dm_heap += other.dm_heap;
        self.pm_heap += other.pm_heap;
        self.sd_heap += other.sd_heap;
        self.dmda += other.dmda;
        self.pmda += other.pmda;
        if !shared_code {
            self.pmco += other.pmco;
        }
        self.edma += other.edma;
        self.singletons += other.singletons;
    }

    /// True when every field is within `budget` (`0x5f9350`; singletons > 1 fail).
    pub fn fits(&self, budget: &Self) -> bool {
        self.cpu <= budget.cpu
            && self.dm_heap <= budget.dm_heap
            && self.pm_heap <= budget.pm_heap
            && self.sd_heap <= budget.sd_heap
            && self.dmda <= budget.dmda
            && self.pmda <= budget.pmda
            && self.pmco <= budget.pmco
            && self.edma <= budget.edma
            && self.singletons <= budget.singletons
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelSpec {
    pub id: u32,
    pub name: String,
    pub category_id: u32,
    pub role: ModelRole,
    /// Non-empty blob name; `None` for kernel-resident and plugin-only models.
    pub blob: Option<String>,
    pub plugin_id: Option<u32>,
    pub hidden: bool,
    pub internal: bool,
    pub replaces: Vec<u32>,
    pub clones: Vec<u32>,
    pub sidechain: Option<SidechainSpec>,
    pub secondary_stomp_parameter: Option<u32>,
    pub parameters: Vec<ParamSpec>,
    /// `None` when the module report has no row for this model and its clone base has none.
    pub stats: Option<ModuleStats>,
    /// The module report has a row for this model; a clone without one takes `stats` from its
    /// base.
    pub report_row: bool,
    /// ModelRepo `reuse_pmco` (absent means true, `0x4e9ed8`).
    pub reuse_pmco: bool,
}

impl ModelSpec {
    pub fn parameter(&self, index: u32) -> Option<&ParamSpec> {
        self.parameters.get(index as usize)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CategorySpec {
    pub id: u32,
    pub name: String,
    pub hidden: bool,
    pub model_ids: Vec<u32>,
}

/// Kernel overhead per SHARC core from `moduleReport.xml` `<Kernel core=…>`.
/// Budgets subtract `cpu` and the three heaps; `cpu_avg` and `isr` are unused.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct KernelCost {
    pub core: u32,
    pub cpu: f32,
    pub dm_heap: u32,
    pub pm_heap: u32,
    pub sd_heap: u32,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SpecError {
    #[error("model {model} parameter {index}: {source}")]
    Conversion {
        model: u32,
        index: usize,
        source: ConversionError,
    },
    #[error("model {model} names sidechain parameter {index}, but it has {count} parameters")]
    SidechainParam {
        model: u32,
        index: u32,
        count: usize,
    },
    #[error("model {model} clones {base}, which is missing or itself a clone")]
    CloneBase { model: u32, base: u32 },
    #[error("model {model} parameter {index} replaces no base parameter index")]
    CloneParameter { model: u32, index: u32 },
    #[error("module {model} report has no \"Stimulus High\" test row")]
    MissingCpuTest { model: u32 },
}

/// Typed, resolved catalog. Built once from a parsed [`Catalog`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct Specs {
    pub categories: BTreeMap<u32, CategorySpec>,
    pub models: BTreeMap<u32, ModelSpec>,
    pub kernels: BTreeMap<u32, KernelCost>,
}

impl Specs {
    pub fn resolve(catalog: &Catalog) -> Result<Self, SpecError> {
        let categories = catalog
            .categories
            .values()
            .map(|category| {
                let spec = CategorySpec {
                    id: category.id,
                    name: category.name.clone(),
                    hidden: is_hidden(category.hidden.as_ref()),
                    model_ids: category.model_ids.clone(),
                };
                (category.id, spec)
            })
            .collect();
        let models = catalog
            .models
            .values()
            .map(|model| {
                // Stock never resets `xValues` between parameters (`0x4e9ed8`).
                let mut x_values = None;
                let parameters = model
                    .parameters
                    .iter()
                    .map(|parameter| {
                        if let Some(x) = parameter.attributes.get("xValues") {
                            x_values = Some(x.as_str());
                        }
                        param_spec(model.id, parameter, x_values)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let sidechain = model.sidechain.map(|source_param| SidechainSpec {
                    source_param,
                    input_param: model.sidechain_in,
                });
                let stats = catalog
                    .modules
                    .get(&model.id)
                    .map(|module| module_stats(model, module))
                    .transpose()?
                    .or_else(|| fixed_stats(model.id));
                let spec = ModelSpec {
                    id: model.id,
                    name: model.name.clone(),
                    category_id: model.category_id,
                    role: ModelRole::from_category(model.category_id),
                    blob: model.blob.clone().filter(|blob| !blob.is_empty()),
                    plugin_id: model.plugin_id,
                    hidden: is_hidden(model.hidden.as_ref()),
                    internal: model.internal == Some(true),
                    replaces: model.replaces.clone().unwrap_or_default(),
                    clones: model.clones.clone().unwrap_or_default(),
                    sidechain,
                    secondary_stomp_parameter: model.secondary_stomp_parameter,
                    parameters,
                    stats,
                    report_row: catalog.modules.contains_key(&model.id),
                    reuse_pmco: model.reuse_pmco != Some(false),
                };
                Ok((model.id, spec))
            })
            .collect::<Result<BTreeMap<_, _>, SpecError>>()?;
        let models = inherit_clones(models)?;
        let kernels = catalog
            .kernels
            .values()
            .map(|kernel| {
                let r = &kernel.resources;
                let cost = KernelCost {
                    core: kernel.core,
                    cpu: r.cpu.unwrap_or_default() as f32,
                    dm_heap: word(r.dm_heap),
                    pm_heap: word(r.pm_heap),
                    sd_heap: word(r.sd_heap),
                };
                (kernel.core, cost)
            })
            .collect();
        Ok(Self {
            categories,
            models,
            kernels,
        })
    }

    pub fn model(&self, id: u32) -> Option<&ModelSpec> {
        self.models.get(&id)
    }

    /// The model that a stored ID resolves to after stock `replaces` links:
    /// a model listing `old` in `replaces` supersedes it.
    pub fn replacement(&self, id: u32) -> Option<&ModelSpec> {
        self.models
            .values()
            .find(|model| model.replaces.contains(&id))
    }
}

/// Clones (`clones="base"`, `0x5507c4`) run their base model: the effective
/// parameter list is the base list, an own parameter with `replaces="i"`
/// overrides base index `i`, and other own parameters are appended. Blob,
/// stats and sidechain fall back to the base. Stored presets follow the
/// effective list (cabs store 21/31 base entries plus MUTE).
fn inherit_clones(own: BTreeMap<u32, ModelSpec>) -> Result<BTreeMap<u32, ModelSpec>, SpecError> {
    let mut models = own.clone();
    for (id, model) in &mut models {
        let Some(&base_id) = model.clones.first() else {
            continue;
        };
        let base = own
            .get(&base_id)
            .filter(|base| base.clones.is_empty())
            .ok_or(SpecError::CloneBase {
                model: *id,
                base: base_id,
            })?;
        let mut parameters = base.parameters.clone();
        for parameter in &model.parameters {
            match parameter.replaces.as_slice() {
                [ParameterReference::Index(index)] if (*index as usize) < parameters.len() => {
                    parameters[*index as usize] = ParamSpec {
                        index: *index,
                        ..parameter.clone()
                    };
                }
                [] => parameters.push(ParamSpec {
                    index: parameters.len() as u32,
                    ..parameter.clone()
                }),
                _ => {
                    return Err(SpecError::CloneParameter {
                        model: *id,
                        index: parameter.index,
                    });
                }
            }
        }
        model.parameters = parameters;
        model.blob = model.blob.take().or_else(|| base.blob.clone());
        model.stats = model.stats.or(base.stats);
        model.sidechain = model.sidechain.or(base.sidechain);
    }
    for model in models.values() {
        let indices = model
            .sidechain
            .iter()
            .flat_map(|s| std::iter::once(s.source_param).chain(s.input_param));
        for index in indices {
            if index as usize >= model.parameters.len() {
                return Err(SpecError::SidechainParam {
                    model: model.id,
                    index,
                    count: model.parameters.len(),
                });
            }
        }
    }
    Ok(models)
}

fn is_hidden(hidden: Option<&Hidden>) -> bool {
    matches!(hidden, Some(Hidden::Boolean(true)))
}

fn word(value: Option<u64>) -> u32 {
    value.map_or(0, |value| u32::try_from(value).unwrap_or(u32::MAX))
}

fn module_stats(model: &crate::Model, module: &crate::Module) -> Result<ModuleStats, SpecError> {
    let test = module
        .tests
        .iter()
        .find(|test| test.name == "Stimulus High")
        .ok_or(SpecError::MissingCpuTest { model: model.id })?;
    let cpu = if ModuleStats::MAX_CPU_MODELS.contains(&model.id) {
        test.max
    } else {
        test.avg
    };
    let r = &module.resources;
    Ok(ModuleStats {
        cpu: (cpu.unwrap_or_default() as f32).min(ModuleStats::CPU_CLAMP),
        dm_heap: word(r.dm_heap),
        pm_heap: word(r.pm_heap),
        sd_heap: word(r.sd_heap),
        dmda: word(r.dm),
        pmda: word(r.pm),
        pmco: word(r.sw).saturating_add(word(r.nw)),
        edma: r.dma.unwrap_or_default() as f32,
        singletons: singletons(model.id, model.category_id),
        latency: word(r.latency),
        reuse_pmco: model.reuse_pmco != Some(false),
        has_sidechain: model.sidechain.is_some(),
    })
}

/// FIR cab engines (`0x8826a4`): mono guitar cabs except the core variants
/// 12014/12114/12116, mono bass cabs and capture-internal models.
fn singletons(id: u32, category: u32) -> u32 {
    let fir = match category {
        12 => !matches!(id, 12_014 | 12_114 | 12_116),
        20 | 21 => true,
        _ => false,
    };
    u32::from(fir)
}

/// Rows `0x8826a4` adds after loading: NC_Recorder and NC_Refiner cost nothing,
/// NC_Trainer costs 70% CPU.
fn fixed_stats(id: u32) -> Option<ModuleStats> {
    let cpu = match id {
        20_000 | 20_002 => 0.0,
        20_001 => 70.0,
        _ => return None,
    };
    Some(ModuleStats {
        cpu,
        singletons: 1,
        reuse_pmco: true,
        ..ModuleStats::default()
    })
}

fn param_spec(
    model: u32,
    p: &crate::Parameter,
    x_values: Option<&str>,
) -> Result<ParamSpec, SpecError> {
    let index = p.position;
    let attribute = |key: &str| p.attributes.get(key).map(String::as_str);
    let scale = ParameterScale::from_xml(&XmlParameter {
        type_name: attribute("type"),
        steps: attribute("steps"),
        skew: attribute("skew"),
        min: attribute("min"),
        max: attribute("max"),
        default_value: attribute("defaultValue"),
        min_string: attribute("min_string"),
        mid_string: attribute("mid_string"),
        max_string: attribute("max_string"),
        units: attribute("units"),
        step_names: attribute("stepNames"),
        x_values,
        y_values: attribute("yValues"),
        show_as_integer: attribute("showAsInteger"),
        exp_assignable: attribute("expAssignable"),
        disabled: attribute("disabled"),
    })
    .map_err(|source| SpecError::Conversion {
        model,
        index,
        source,
    })?;
    let law = scale.law;
    let range = (attribute("min").is_some() && attribute("max").is_some()).then_some(ParamRange {
        min: law.min,
        max: law.max,
    });
    // An unknown default macro warns and falls back to min (`0x4e9ed8`).
    let default = attribute("defaultValue").map(|text| match &p.parameter_type {
        ParameterType::String | ParameterType::Unknown(_) => ParamDefault::Text(text.to_owned()),
        _ => ParamDefault::Number(metadata_float(text, law.min)),
    });
    let owned = |key: &str| attribute(key).map(str::to_owned);
    Ok(ParamSpec {
        index: index as u32,
        name: p.name.clone(),
        kind: p.parameter_type.clone(),
        range,
        default,
        skew: attribute("skew").map(|_| law.skew),
        steps: p.steps,
        step_names: p.step_names.clone().unwrap_or_default(),
        units: p.units.clone().filter(|units| !units.is_empty()),
        labels: RangeLabels {
            min: owned("min_string"),
            mid: owned("mid_string"),
            max: owned("max_string"),
        },
        tooltip: owned("tooltip"),
        hidden: is_hidden(p.hidden.as_ref()),
        dynamic: p.dynamic == Some(true),
        exp_assignable: p.exp_assignable,
        show_as_integer: p.show_as_integer == Some(true),
        display_pos: p.display_pos,
        linked_scene_mode: p.linked_scene_mode.clone().unwrap_or_default(),
        replaces: p.replaces.clone().unwrap_or_default(),
        toggle_on: p.toggle_on.clone().unwrap_or_default(),
        toggle_off: p.toggle_off.clone().unwrap_or_default(),
        toggle_step: p.toggle_step.clone().unwrap_or_default(),
        scale,
    })
}

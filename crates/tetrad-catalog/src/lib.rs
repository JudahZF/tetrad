//! Bounded, offline model metadata and DSP resource reports.
//!
//! An early lexical attribute cap bounds `qc_format::metadata` tokenization;
//! XML structure and entity decoding come from that dependency. Typed
//! fields describe metadata, not recovered DSP or plugin behavior. Every entry
//! retains its attributes and original XML so opaque fields remain available.

mod blobs;
pub mod owned;
mod spec;
mod xml;

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

pub use blobs::{BlobInspection, BlobReport, BlobStatus, validate_blob};
pub use spec::{
    CategorySpec, KernelCost, ModelRole, ModelSpec, ModuleStats, ParamDefault, ParamRange,
    ParamSpec, RangeLabels, SidechainSpec, SpecError, Specs, ids,
};

pub type Result<T> = std::result::Result<T, CatalogError>;
pub type Attributes = BTreeMap<String, String>;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error(transparent)]
    Format(#[from] qc_format::FormatError),
    #[error("catalog schema: {0}")]
    Schema(String),
    #[error("catalog limit exceeded: {0}")]
    Limit(&'static str),
    #[error("blob directory: {0}")]
    Io(#[from] std::io::Error),
}

/// ID maps are authoritative. Separate order lists retain XML document order.
#[derive(Debug)]
pub struct Catalog {
    pub categories: BTreeMap<u32, Category>,
    pub models: BTreeMap<u32, Model>,
    pub modules: BTreeMap<u32, Module>,
    pub kernels: BTreeMap<u32, Kernel>,
    pub category_order: Vec<u32>,
    pub model_order: Vec<u32>,
    pub module_order: Vec<u32>,
    pub kernel_order: Vec<u32>,
    /// Full source documents also preserve root attributes and opaque elements.
    pub model_repo_xml: String,
    pub module_report_xml: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Category {
    pub id: u32,
    pub name: String,
    pub hidden: Option<Hidden>,
    pub model_ids: Vec<u32>,
    pub attributes: Attributes,
    pub raw_xml: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Model {
    pub id: u32,
    pub category_id: u32,
    pub name: String,
    /// None and Some("") are distinct metadata states. Neither requires a blob.
    pub blob: Option<String>,
    pub plugin_id: Option<u32>,
    pub sku: Option<String>,
    pub style: Option<String>,
    pub tm: Option<String>,
    pub hidden: Option<Hidden>,
    pub internal: Option<bool>,
    pub skip_self_test: Option<bool>,
    pub reuse_pmco: Option<bool>,
    pub replaces: Option<Vec<u32>>,
    pub clones: Option<Vec<u32>>,
    pub sidechain: Option<u32>,
    pub sidechain_in: Option<u32>,
    pub secondary_stomp_parameter: Option<u32>,
    pub padding: Option<Resources>,
    /// Ordered by XML position; explicit indices do not reorder parameters.
    pub parameters: Vec<Parameter>,
    pub attributes: Attributes,
    pub raw_xml: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Parameter {
    pub position: usize,
    pub index: Option<u32>,
    pub name: String,
    pub parameter_type: ParameterType,
    pub default_value: Option<ParameterValue>,
    /// Bounds can contain stock constants such as MIN_EQ_DB, not just numbers.
    pub min: Option<ParameterValue>,
    pub max: Option<ParameterValue>,
    pub skew: Option<ParameterValue>,
    pub steps: Option<u32>,
    pub step_names: Option<Vec<String>>,
    pub x_values: Option<Vec<f64>>,
    pub y_values: Option<Vec<f64>>,
    pub hidden: Option<Hidden>,
    pub display_pos: Option<u32>,
    pub replaces: Option<Vec<ParameterReference>>,
    /// Ordered direct parameter positions; duplicates are not removed.
    pub linked_scene_mode: Option<Vec<u32>>,
    pub toggle_on: Option<Vec<u32>>,
    pub toggle_off: Option<Vec<u32>>,
    pub toggle_step: Option<Vec<u32>>,
    pub dynamic: Option<bool>,
    pub exp_assignable: Option<bool>,
    pub show_as_integer: Option<bool>,
    pub units: Option<String>,
    pub self_test_value: Option<String>,
    pub attributes: Attributes,
    pub raw_xml: String,
}

/// Stock parameter types. ZenUI's type map (`_INIT_402`, VA `0x9932c`) assigns
/// float 0, fader 1, int 2, switch 3, rotarySwitch 4, meter 5, stereoMeter 6,
/// grMeter 7, stereoGrMeter 8, string 9, toggleButton 10, comboBox 11,
/// floatWithLed 12 and empty 13.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterType {
    Float,
    FloatWithLed,
    Int,
    String,
    Switch,
    RotarySwitch,
    ComboBox,
    ToggleButton,
    Fader,
    Meter,
    StereoMeter,
    GrMeter,
    StereoGrMeter,
    Empty,
    Unknown(String),
}

impl ParameterType {
    /// Display-only parameters that report DSP state instead of taking input.
    pub fn is_meter(&self) -> bool {
        matches!(
            self,
            Self::Meter | Self::StereoMeter | Self::GrMeter | Self::StereoGrMeter
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ParameterValue {
    Number(f64),
    Text(String),
    /// A symbolic stock constant; resolving it requires later runtime evidence.
    Symbol(String),
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ParameterReference {
    Index(u32),
    /// A literal metadata name; no replacement behavior is inferred.
    Name(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Hidden {
    Boolean(bool),
    /// For example the stock literal `atma`; no condition behavior is assumed.
    Opaque(String),
}

/// Optional resource quantities retain the report's units without inference.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Resources {
    pub dm_heap: Option<u64>,
    pub pm_heap: Option<u64>,
    pub sd_heap: Option<u64>,
    pub dm: Option<u64>,
    pub pm: Option<u64>,
    pub sw: Option<u64>,
    pub nw: Option<u64>,
    pub dma: Option<f64>,
    pub cpu: Option<f64>,
    pub cpu_avg: Option<f64>,
    pub cpu_pad: Option<f64>,
    pub isr: Option<f64>,
    pub latency: Option<u64>,
    pub dm_heap_pad: Option<u64>,
    pub pm_heap_pad: Option<u64>,
    pub sd_heap_pad: Option<u64>,
    pub dm_pad: Option<u64>,
    pub pm_pad: Option<u64>,
    pub sw_pad: Option<u64>,
    pub nw_pad: Option<u64>,
    pub attributes: Attributes,
    pub raw_xml: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Module {
    pub id: u32,
    pub category_id: Option<u32>,
    pub name: String,
    pub blob_name: Option<String>,
    pub resources: Resources,
    pub tests: Vec<ResourceTest>,
    pub attributes: Attributes,
    pub raw_xml: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceTest {
    pub name: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub avg: Option<f64>,
    pub pk_l: Option<f64>,
    pub pk_r: Option<f64>,
    pub attributes: Attributes,
    pub raw_xml: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Kernel {
    pub core: u32,
    pub resources: Resources,
    pub attributes: Attributes,
    pub raw_xml: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogSummary {
    pub categories: usize,
    pub models: usize,
    pub parameters: usize,
    pub modules: usize,
    pub kernels: usize,
    pub blob_attributes: usize,
    pub models_with_blobs: usize,
    pub unique_model_blobs: usize,
    pub plugin_models: usize,
    pub matched_modules: usize,
    pub unmatched_modules: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    UnmatchedModule,
    MissingModuleReport,
    BlobNameMismatch,
    CategoryMismatch,
    UnsafeBlobName,
    AttributeTypo,
    OpaqueHidden,
    UnknownParameterType,
    InvalidParameterRange,
    PiecewiseMappingMismatch,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagnosticCode,
    pub model_id: Option<u32>,
    pub module_id: Option<u32>,
    pub parameter_position: Option<usize>,
    pub message: String,
}

impl Catalog {
    pub fn parse(model_repo: &str, module_report: &str) -> Result<Self> {
        xml::parse(model_repo, module_report)
    }

    /// Lookup uses the catalog's ID map, including models with shared blob names.
    pub fn model(&self, id: u32) -> Option<&Model> {
        self.models.get(&id)
    }

    pub fn summary(&self) -> CatalogSummary {
        let blob_names: std::collections::BTreeSet<_> = self
            .models
            .values()
            .filter_map(|m| m.blob.as_deref().filter(|s| !s.is_empty()))
            .collect();
        let matched_modules = self
            .modules
            .keys()
            .filter(|id| self.models.contains_key(id))
            .count();
        CatalogSummary {
            categories: self.categories.len(),
            models: self.models.len(),
            parameters: self.models.values().map(|m| m.parameters.len()).sum(),
            modules: self.modules.len(),
            kernels: self.kernels.len(),
            blob_attributes: self.models.values().filter(|m| m.blob.is_some()).count(),
            models_with_blobs: self
                .models
                .values()
                .filter(|m| m.blob.as_ref().is_some_and(|s| !s.is_empty()))
                .count(),
            unique_model_blobs: blob_names.len(),
            plugin_models: self
                .models
                .values()
                .filter(|m| m.plugin_id.is_some())
                .count(),
            matched_modules,
            unmatched_modules: self.modules.len() - matched_modules,
        }
    }

    /// Missing optional plugins/defaults/ranges are not corruption diagnostics.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for module in self.modules.values() {
            let model = self.models.get(&module.id);
            let mut emit = |severity, code, message| {
                out.push(Diagnostic {
                    severity,
                    code,
                    model_id: model.map(|m| m.id),
                    module_id: Some(module.id),
                    parameter_position: None,
                    message,
                })
            };
            if let Some(model) = model {
                if model.blob != module.blob_name {
                    emit(
                        Severity::Warning,
                        DiagnosticCode::BlobNameMismatch,
                        "model/report blob names differ for the same ID".into(),
                    );
                }
                if module.category_id.is_some_and(|id| id != model.category_id) {
                    emit(
                        Severity::Warning,
                        DiagnosticCode::CategoryMismatch,
                        "model/report category IDs differ".into(),
                    );
                }
            } else {
                emit(
                    Severity::Warning,
                    DiagnosticCode::UnmatchedModule,
                    "report ID has no catalog model".into(),
                );
            }
            if module.blob_name.as_deref().is_some_and(unsafe_blob_name) {
                emit(
                    Severity::Error,
                    DiagnosticCode::UnsafeBlobName,
                    "report blob name cannot be used as a local filename".into(),
                );
            }
        }
        for model in self.models.values() {
            let mut emit = |severity, code, position, message| {
                out.push(Diagnostic {
                    severity,
                    code,
                    model_id: Some(model.id),
                    module_id: None,
                    parameter_position: position,
                    message,
                })
            };
            if let Some(name) = model.blob.as_deref().filter(|s| !s.is_empty()) {
                if unsafe_blob_name(name) {
                    emit(
                        Severity::Error,
                        DiagnosticCode::UnsafeBlobName,
                        None,
                        "model blob name cannot be used as a local filename".into(),
                    );
                }
                if !self.modules.contains_key(&model.id) {
                    emit(
                        Severity::Warning,
                        DiagnosticCode::MissingModuleReport,
                        None,
                        "model has a blob but no resource report".into(),
                    );
                }
            }
            if matches!(model.hidden, Some(Hidden::Opaque(_))) {
                emit(
                    Severity::Info,
                    DiagnosticCode::OpaqueHidden,
                    None,
                    "opaque model hidden value preserved".into(),
                );
            }
            for parameter in &model.parameters {
                let position = Some(parameter.position);
                if parameter.attributes.contains_key("isplayPos") {
                    emit(
                        Severity::Warning,
                        DiagnosticCode::AttributeTypo,
                        position,
                        "literal isplayPos attribute preserved; displayPos was not inferred".into(),
                    );
                }
                if matches!(parameter.hidden, Some(Hidden::Opaque(_))) {
                    emit(
                        Severity::Info,
                        DiagnosticCode::OpaqueHidden,
                        position,
                        "opaque parameter hidden value preserved".into(),
                    );
                }
                if matches!(parameter.parameter_type, ParameterType::Unknown(_)) {
                    emit(
                        Severity::Info,
                        DiagnosticCode::UnknownParameterType,
                        position,
                        "unknown parameter type preserved".into(),
                    );
                }
                if matches!((&parameter.min, &parameter.max), (Some(ParameterValue::Number(min)), Some(ParameterValue::Number(max))) if min > max)
                {
                    emit(
                        Severity::Warning,
                        DiagnosticCode::InvalidParameterRange,
                        position,
                        "parameter minimum exceeds maximum".into(),
                    );
                }
                match (&parameter.x_values, &parameter.y_values) {
                    (None, None) => {}
                    (Some(x), Some(y))
                        if x.len() == y.len()
                            && x.len() >= 2
                            && x.windows(2).all(|w| w[0] < w[1]) => {}
                    _ => emit(
                        Severity::Warning,
                        DiagnosticCode::PiecewiseMappingMismatch,
                        position,
                        "piecewise coordinates require matching lists and increasing x values"
                            .into(),
                    ),
                }
            }
        }
        out
    }

    /// Inspects every decrypted `.raw` file and all model/report references.
    /// Each file is bounded and opened without following symlinks. Linux uses
    /// an anchored directory descriptor; other platforms fail closed.
    pub fn inspect_blobs(&self, blob_root: &Path) -> Result<BlobReport> {
        blobs::inspect(self, blob_root)
    }
}

fn unsafe_blob_name(name: &str) -> bool {
    !name.is_empty() && qc_format::blob::validate_blob_name(name).is_err()
}

#[cfg(test)]
mod tests;

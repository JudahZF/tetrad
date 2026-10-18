//! Owned models: the `tetrad-blocks` SHARC blocks that can stand in for a stock model (slice
//! `w4-f-wire`).
//!
//! An owned block deploys under the stock ID it replaces, through the stock blob path. The
//! replacement is opt-in: [`replace`] changes only the models the caller names, and nothing
//! calls it by default.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use tetrad_blocks::blocks::{gain, gate, geq9, lohicut, volume};
use tetrad_blocks::{BlockError, BuiltModule, HostReference, ParameterSpec};

use crate::{ModuleStats, Specs};

/// Modeled steps one core has per 32-sample callback: 450 MHz / (48 kHz / 32). Inferred: the
/// clock is the SC589 maximum and a modeled step is not a measured cycle (roadmap H6 measures
/// the real scale).
pub const CORE_STEPS_PER_BLOCK: u64 = 300_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OwnedModel {
    Volume,
    Gain,
    LowHighCut,
    SimpleGate,
    Graphic9,
}

/// A generic operation on the concrete block type of an [`OwnedModel`].
pub trait BlockVisitor {
    type Output;
    fn visit<B: HostReference>(self, block: &B) -> Self::Output;
}

impl OwnedModel {
    pub const ALL: [Self; 5] = [
        Self::Volume,
        Self::Gain,
        Self::LowHighCut,
        Self::SimpleGate,
        Self::Graphic9,
    ];

    /// The command-line name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Volume => "volume",
            Self::Gain => "gain",
            Self::LowHighCut => "lohicut",
            Self::SimpleGate => "gate",
            Self::Graphic9 => "geq9",
        }
    }

    /// The stock model this block replaces; also its deployment ID.
    pub const fn stock_id(self) -> u32 {
        match self {
            Self::Volume => volume::STOCK_MODEL_ID,
            Self::Gain => gain::STOCK_MODEL_ID,
            Self::LowHighCut => lohicut::STOCK_MODEL_ID,
            Self::SimpleGate => gate::STOCK_MODEL_ID,
            Self::Graphic9 => geq9::STOCK_MODEL_ID,
        }
    }

    pub fn from_stock_id(id: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|model| model.stock_id() == id)
    }

    pub fn visit<V: BlockVisitor>(self, visitor: V) -> V::Output {
        match self {
            Self::Volume => visitor.visit(&volume::Volume),
            Self::Gain => visitor.visit(&gain::Gain),
            Self::LowHighCut => visitor.visit(&lohicut::LowHighCut),
            Self::SimpleGate => visitor.visit(&gate::SimpleGate),
            Self::Graphic9 => visitor.visit(&geq9::Graphic9),
        }
    }

    /// Builds the plaintext blob at the link bases, with its fit stats.
    pub fn build(self) -> Result<OwnedEntry, BlockError> {
        struct Build(OwnedModel);
        impl BlockVisitor for Build {
            type Output = Result<OwnedEntry, BlockError>;
            fn visit<B: HostReference>(self, block: &B) -> Self::Output {
                let module = BuiltModule::build(block)?;
                Ok(OwnedEntry {
                    model: self.0,
                    parameters: block.parameters(),
                    stats: ModuleStats::from(&module.stats),
                    module,
                })
            }
        }
        self.visit(Build(self))
    }
}

impl fmt::Display for OwnedModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unknown owned block {0:?}; known: volume, gain, lohicut, gate, geq9")]
pub struct UnknownOwnedModel(String);

impl FromStr for OwnedModel {
    type Err = UnknownOwnedModel;
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|model| model.name() == name)
            .ok_or_else(|| UnknownOwnedModel(name.to_owned()))
    }
}

/// One registry row: an owned block, built.
#[derive(Debug, Clone)]
pub struct OwnedEntry {
    pub model: OwnedModel,
    /// Runtime index, name and value law of each parameter.
    pub parameters: &'static [ParameterSpec],
    /// The blob for `BlobLoad`; `module.stats` are the tetrad-blocks stats.
    pub module: BuiltModule,
    /// `module.stats` in fit terms.
    pub stats: ModuleStats,
}

/// Every owned block, built.
pub fn registry() -> Result<Vec<OwnedEntry>, BlockError> {
    OwnedModel::ALL.into_iter().map(OwnedModel::build).collect()
}

/// Owned block stats in moduleReport terms (inferred mapping): `dm` = static state bytes,
/// `sw + nw` = VISA + ISA bytes, CPU = modeled steps over [`CORE_STEPS_PER_BLOCK`] (not
/// clamped). No heaps, PM data, DMA or latency; each instance needs its own code copy.
impl From<&tetrad_blocks::ModuleStats> for ModuleStats {
    fn from(stats: &tetrad_blocks::ModuleStats) -> Self {
        Self {
            cpu: (stats.cycles_per_stereo_block as f64 * 100.0 / CORE_STEPS_PER_BLOCK as f64)
                as f32,
            dmda: stats.dm_bytes,
            pmco: stats.visa_bytes.saturating_add(stats.isa_bytes),
            reuse_pmco: stats.reuse_pmco,
            ..Self::default()
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OwnedError {
    #[error("owned {0}: the catalog has no stock model {id}", id = .0.stock_id())]
    MissingModel(OwnedModel),
    /// The runtime index and name at one parameter position differ (`None`: no parameter).
    #[error("owned {model}: parameter {position} is {stock:?} in the catalog, {owned:?} owned")]
    Parameter {
        model: OwnedModel,
        position: usize,
        stock: Option<(u32, String)>,
        owned: Option<(u32, &'static str)>,
    },
    /// A shared blob name would let the planner share one load with another model.
    #[error("owned {model}: stock blob {blob:?} is shared with model {other}")]
    SharedBlob {
        model: OwnedModel,
        blob: String,
        other: u32,
    },
    #[error("owned {model}: {source}")]
    Build {
        model: OwnedModel,
        #[source]
        source: BlockError,
    },
}

/// Replaces each named stock model with its owned block. The spec keeps its parameters and
/// blob name and takes the owned stats with each resource floored at the stock figure (see
/// `charged`), without code reuse (static state), so fit charges every instance and the
/// planner gives each one its own load. The parameters must match by position, runtime index
/// and name. Every model is checked first: `specs` changes only on success. Returns the built
/// blobs for the deploy.
pub fn replace(
    specs: &mut Specs,
    models: &BTreeSet<OwnedModel>,
) -> Result<Vec<OwnedEntry>, OwnedError> {
    let mut entries = Vec::with_capacity(models.len());
    for &model in models {
        let id = model.stock_id();
        let spec = specs
            .models
            .get(&id)
            .ok_or(OwnedError::MissingModel(model))?;
        let entry = model
            .build()
            .map_err(|source| OwnedError::Build { model, source })?;
        for position in 0..spec.parameters.len().max(entry.parameters.len()) {
            let stock = spec.parameters.get(position);
            let owned = entry.parameters.get(position);
            if stock.map(|p| (p.index, p.name.as_str())) != owned.map(|p| (p.runtime_index, p.name))
            {
                return Err(OwnedError::Parameter {
                    model,
                    position,
                    stock: stock.map(|p| (p.index, p.name.clone())),
                    owned: owned.map(|p| (p.runtime_index, p.name)),
                });
            }
        }
        if let Some(blob) = &spec.blob
            && let Some(other) = specs
                .models
                .values()
                .find(|other| other.id != id && other.blob.as_ref() == Some(blob))
        {
            return Err(OwnedError::SharedBlob {
                model,
                blob: blob.clone(),
                other: other.id,
            });
        }
        entries.push(entry);
    }
    for entry in &entries {
        if let Some(spec) = specs.models.get_mut(&entry.model.stock_id()) {
            spec.stats = Some(ModuleStats {
                has_sidechain: spec.sidechain.is_some(),
                ..charged(&entry.stats, spec.stats.as_ref())
            });
            spec.reuse_pmco = false;
        }
    }
    Ok(entries)
}

/// The stats fit charges for an owned block: each resource is at least the stock figure
/// (review-w4-dsp F5). The stock CPU includes kernel per-slot work that the modeled steps
/// leave out (until proof H6), and off DSP core 0 the stock blob loads in place of the owned
/// one (F2), so the fit must hold for both. Latency stays the owned figure.
fn charged(owned: &ModuleStats, stock: Option<&ModuleStats>) -> ModuleStats {
    let Some(stock) = stock else {
        return *owned;
    };
    ModuleStats {
        cpu: owned.cpu.max(stock.cpu),
        dm_heap: owned.dm_heap.max(stock.dm_heap),
        pm_heap: owned.pm_heap.max(stock.pm_heap),
        sd_heap: owned.sd_heap.max(stock.sd_heap),
        dmda: owned.dmda.max(stock.dmda),
        pmda: owned.pmda.max(stock.pmda),
        pmco: owned.pmco.max(stock.pmco),
        edma: owned.edma.max(stock.edma),
        singletons: owned.singletons.max(stock.singletons),
        ..*owned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Catalog;

    /// A synthetic catalog with one stock Volume (16006) whose parameters are `parameters`,
    /// at 5 % CPU and 4096 DM bytes: far above the owned figures.
    fn volume_specs(parameters: [&str; 2]) -> Specs {
        let [first, second] = parameters.map(|name| {
            format!("<Parameter name='{name}' type='float' min='0' max='1' defaultValue='0'/>")
        });
        let models = format!(
            "<Models><Category id='16' name='Utility'><Model id='16006' name='Volume' \
             blob='volume'>{first}{second}</Model></Category></Models>"
        );
        let report = "<ModuleReport><Module id='16006' name='Volume' blob_name='volume' \
             category='16' dm_heap='0' pm_heap='0' sd_heap='0' dma='0' dm='4096' pm='0' \
             sw='8' nw='0' Latency='0'><Test name='Stimulus High' avg='5'/></Module>\
             </ModuleReport>";
        Specs::resolve(&Catalog::parse(&models, report).unwrap()).unwrap()
    }

    /// F5 (review-w4-dsp): an owned block is charged at least the stock CPU and memory of
    /// the model it replaces, because the stock blob loads in its place off DSP core 0.
    #[test]
    fn replace_floors_the_charge_at_the_stock_figures() {
        let mut specs = volume_specs(["LEVEL", "CURVE"]);
        let models = BTreeSet::from([OwnedModel::Volume]);
        let entry = replace(&mut specs, &models).unwrap().remove(0);
        let charged = specs.models[&16_006].stats.unwrap();
        assert!(entry.stats.cpu < 5.0 && entry.stats.dmda < 4096);
        assert_eq!((charged.cpu, charged.dmda), (5.0, 4096));
        assert_eq!(charged.pmco, entry.stats.pmco.max(8));
        assert!(!charged.reuse_pmco);
    }

    /// F8 (review-w4-dsp): a catalog that reorders the parameters is refused even though the
    /// count matches, and the specs stay stock.
    #[test]
    fn replace_refuses_reordered_parameters() {
        let mut specs = volume_specs(["CURVE", "LEVEL"]);
        let before = specs.models[&16_006].stats;
        let models = BTreeSet::from([OwnedModel::Volume]);
        let error = replace(&mut specs, &models).unwrap_err();
        assert!(matches!(
            error,
            OwnedError::Parameter {
                position: 0,
                owned: Some((0, "LEVEL")),
                ..
            }
        ));
        assert_eq!(specs.models[&16_006].stats, before);
    }

    /// Item 1 (`w4-f-wire`): the registry lists every owned block once, under its stock ID,
    /// and its fit stats are the tetrad-blocks `ModuleStats` (sizes, cycle bound, no reuse).
    #[test]
    fn registry_stats_match_module_stats() {
        let registry = registry().unwrap();
        let ids: Vec<_> = registry
            .iter()
            .map(|entry| entry.model.stock_id())
            .collect();
        assert_eq!(ids, [16_006, 16_005, 4_003, 16_003, 4_002]);
        for entry in &registry {
            let built = &entry.module.stats;
            assert_eq!(entry.module.model_id, entry.model.stock_id());
            assert_eq!(entry.stats.dmda, built.dm_bytes);
            assert_eq!(entry.stats.pmco, built.visa_bytes + built.isa_bytes);
            let steps = f64::from(entry.stats.cpu) * CORE_STEPS_PER_BLOCK as f64 / 100.0;
            assert!((steps - built.cycles_per_stereo_block as f64).abs() < 0.5);
            assert!(!entry.stats.reuse_pmco && !built.reuse_pmco);
            assert!(!entry.parameters.is_empty());
            assert_eq!(entry.model.name().parse(), Ok(entry.model));
        }
    }
}

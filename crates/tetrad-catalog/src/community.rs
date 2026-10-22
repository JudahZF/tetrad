//! Community blocks (O): qcdsp IR graphs from extension packages. Tetrad compiles each one on
//! the unit against the stock Volume framework (`qcdsp::device::build`). This module gives the
//! result a model spec, so the engine, the picker and the block editor treat it like a stock
//! model.
//!
//! A community block has a local model ID at the top of its category's thousand band
//! ([`FIRST_LOCAL`]`..=999`): the picker takes the category from `id / 1000`, and the stock IDs
//! of CorOS 4.1 stay below 200 in every band. Presets never store a local ID. A saved preset
//! holds the block's [`CommunitySpec::placeholder`], a stock model, and a record with its
//! [`CommunitySpec::key`] (`tetrad_preset::community`).

use qc_format::metadata::escape;
use serde::Serialize;

use crate::{Catalog, CatalogError, ModelSpec, SpecError, Specs};

/// The first local ID offset in a category band.
pub const FIRST_LOCAL: u32 = 800;
/// The stock Gain block at its default (0 dB, centre): the placeholder of a block whose
/// manifest names none.
pub const DEFAULT_PLACEHOLDER: u32 = 16_005;
/// Value steps of a community parameter knob.
const STEPS: u32 = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommunitySpec {
    /// `<installation key>/<block id>`, plus `#<variant>` for a block built from an asset.
    pub key: String,
    /// The stock model that a saved preset holds in this block's slot.
    pub placeholder: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum CommunityError {
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error(transparent)]
    Format(#[from] qc_format::FormatError),
    #[error("community model {0} did not resolve")]
    Unresolved(u32),
}

/// The local ID of `key` in `category`: a stable hash of the key in the community range,
/// probing upwards (and wrapping) past the IDs that `taken` reports. `None` when the range is
/// full. The same set of keys always gives the same IDs, so mappings to a community block
/// stay valid after a restart.
pub fn local_id(category: u32, key: &str, taken: impl Fn(u32) -> bool) -> Option<u32> {
    let span = 1000 - FIRST_LOCAL;
    let base = category.checked_mul(1000)?.checked_add(FIRST_LOCAL)?;
    // FNV-1a: small, stable across builds and platforms.
    let hash = key.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    (0..span)
        .map(|probe| base + (hash.wrapping_add(probe)) % span)
        .find(|&id| !taken(id))
}

/// True for an ID in a community range.
pub fn is_local(id: u32) -> bool {
    id % 1000 >= FIRST_LOCAL
}

/// The spec of a built block: `block` after `qcdsp::Block::renamed` (its name and category
/// are the model's), `module_element` is `qcdsp::device::DeviceBlock::module_element`.
/// Parameters keep the IR's DSP units: the editor shows them as stored, and the setup sends
/// `(value - min) / (max - min)`, which the generated set-parameter callback maps back.
pub fn spec(
    block: &qcdsp::Block,
    model_id: u32,
    blob_name: &str,
    module_element: &str,
    community: CommunitySpec,
) -> Result<ModelSpec, CommunityError> {
    let mut model = format!(
        r#"<Models><Category id="{}" name="Community"><Model blob="{}" id="{model_id}" name="{}">"#,
        block.category().id(),
        escape(blob_name)?,
        escape(block.name())?,
    );
    for parameter in block.parameters() {
        model.push_str(&format!(
            r#"<Parameter defaultValue="{}" max="{}" min="{}" name="{}" steps="{STEPS}" type="float"/>"#,
            parameter.default,
            parameter.max,
            parameter.min,
            escape(&parameter.name)?,
        ));
    }
    model.push_str("</Model></Category></Models>");
    let report = format!("<ModuleReport>{module_element}</ModuleReport>");
    let specs = Specs::resolve(&Catalog::parse(&model, &report)?)?;
    let mut spec = specs
        .models
        .into_values()
        .next()
        .ok_or(CommunityError::Unresolved(model_id))?;
    // Every instance gets its own load: blocks of one key can differ after an update.
    spec.reuse_pmco = false;
    if let Some(stats) = spec.stats.as_mut() {
        stats.reuse_pmco = false;
    }
    spec.community = Some(community);
    Ok(spec)
}

impl Specs {
    /// The local ID of an installed community block.
    pub fn community_id(&self, key: &str) -> Option<u32> {
        self.models
            .values()
            .find(|model| model.community.as_ref().is_some_and(|c| c.key == key))
            .map(|model| model.id)
    }

    /// Removes every community model; returns how many there were.
    pub fn remove_community(&mut self) -> usize {
        let before = self.models.len();
        self.models.retain(|_, model| model.community.is_none());
        before - self.models.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ids_are_stable_in_band_and_skip_taken_ids() {
        let id = local_id(1, "src~nam/amp", |_| false).unwrap();
        assert_eq!(id / 1000, 1);
        assert!(is_local(id));
        assert_eq!(local_id(1, "src~nam/amp", |_| false), Some(id));
        let next = local_id(1, "src~nam/amp", |taken| taken == id).unwrap();
        assert_ne!(next, id);
        assert_eq!(next / 1000, 1);
        assert_eq!(local_id(1, "x", |_| true), None);
    }

    /// A built block becomes a normal block spec: its category and range, the IR's parameters
    /// in DSP units, the stats of qcdsp's module report entry and its own blob.
    #[test]
    fn a_saturator_resolves_like_a_stock_block() {
        let block = qcdsp::examples::saturator()
            .unwrap()
            .renamed("tx_test", "Saturator", qcdsp::Category::GuitarOverdrive)
            .unwrap();
        let (layout, generated) = qcdsp::device::generate(&block).unwrap();
        let resources = qcdsp::device::Resources {
            code_bytes: 4096,
            state_bytes: layout.bytes(),
            cycles_per_call: generated.cycles_per_call,
        };
        let report =
            qcdsp::device::module_element(&block, "qcdsp_tx_test", 812, "Saturator", &resources)
                .unwrap();
        let community = CommunitySpec {
            key: "src~ext/sat".into(),
            placeholder: DEFAULT_PLACEHOLDER,
        };
        let spec = spec(&block, 812, "qcdsp_tx_test", &report, community.clone()).unwrap();
        assert_eq!(spec.id, 812);
        assert_eq!(spec.category_id, 0);
        assert_eq!(spec.role, crate::ModelRole::Block);
        assert_eq!(spec.blob.as_deref(), Some("qcdsp_tx_test"));
        let names: Vec<_> = spec.parameters.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Drive", "Bias", "Level"]);
        let drive = spec.parameters[0].range.unwrap();
        assert_eq!((drive.min, drive.max), (1.0, 40.0));
        let stats = spec.stats.unwrap();
        assert!(stats.cpu > 0.0 && !stats.reuse_pmco);
        assert_eq!(spec.community, Some(community));
        let mut specs = Specs::default();
        specs.models.insert(spec.id, spec);
        assert_eq!(specs.community_id("src~ext/sat"), Some(812));
        assert_eq!(specs.remove_community(), 1);
    }
}

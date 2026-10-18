//! Owned blocks through the stock `BlobLoad` path (slice `w4-f-wire`).
//!
//! Off by default: [`RunOptions::owned`] is empty, and only a bring-up run with
//! [`RunOptions::allow_owned`] accepts it (`run` has no owned flag until proof H1 passes).
//! [`install`] builds the named blocks once and patches the specs
//! (`tetrad_catalog::owned::replace`), so fit charges the owned stats and the planner gives every
//! instance its own load. A DSP core 0 load then sends the plaintext owned blob for those model
//! IDs instead of the decrypted stock blob: no blob key and no diff. The owned ABI was decoded
//! from the core-0 kernel (`ZPUKernel_Core1`) only, so a mapped model on another core stays
//! stock (roadmap proof H5).

use std::collections::BTreeMap;

use qc_format::blob::ModuleBlob;
use tetrad_catalog::Specs;
use tetrad_catalog::owned::{self, OwnedError, OwnedModel};
use tetrad_dsp_host::session::BlobLoad;
use tetrad_dsp_protocol::DspCore;

use super::bringup::RunOptions;

/// The only core that runs owned blocks.
pub const OWNED_CORE: DspCore = DspCore::Core0;

/// Owned blobs by the stock model ID they replace. Empty: every load is stock.
#[derive(Debug, Clone, Default)]
pub struct OwnedBlobs(BTreeMap<u32, (OwnedModel, ModuleBlob)>);

/// What a planned load of a mapped model becomes on one core.
#[derive(Debug, Clone)]
pub enum OwnedLoad {
    /// [`OWNED_CORE`]: the owned blob.
    Owned(OwnedModel, BlobLoad),
    /// Another core: the stock blob loads.
    Stock(OwnedModel),
}

impl OwnedBlobs {
    /// The load of `model` on `core`, or `None` when the model is not mapped.
    pub fn load(&self, core: DspCore, model: u32) -> Option<OwnedLoad> {
        let (owned, blob) = self.0.get(&model)?;
        Some(if core == OWNED_CORE {
            OwnedLoad::Owned(
                *owned,
                BlobLoad {
                    model_id: model,
                    blob: Some(blob.clone()),
                },
            )
        } else {
            OwnedLoad::Stock(*owned)
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("owned blocks need a bring-up run with --allow-owned; the stock blobs stay")]
    Refused,
    #[error(transparent)]
    Owned(#[from] OwnedError),
}

/// Applies the opt-in stock-ID map to `specs`. An empty map changes nothing. On an error
/// `specs` is unchanged and every model stays stock.
pub fn install(options: &RunOptions, specs: &mut Specs) -> Result<OwnedBlobs, InstallError> {
    if options.owned.is_empty() {
        return Ok(OwnedBlobs::default());
    }
    if !(options.is_bringup() && options.allow_owned) {
        return Err(InstallError::Refused);
    }
    let entries = owned::replace(specs, &options.owned)?;
    Ok(OwnedBlobs(
        entries
            .into_iter()
            .map(|entry| (entry.model.stock_id(), (entry.model, entry.module.blob)))
            .collect(),
    ))
}

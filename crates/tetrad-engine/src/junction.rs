//! Splitter and mixer models of a row pair.
//!
//! Stock does not push junction values to a model slot; splitter and mixer
//! take no slot. Their values are converted at send time and written to the
//! split router (`+0x5a4`), the merge router (`+0x5c4`) and the two 10003
//! crossover heads (`+0x674`) of [`crate::RouterSet`]. The engine emits
//! normalized ops ([`crate::PlanOp::PushJunctions`],
//! [`crate::PlanOp::Junction`]); the laws and the router mapping live with
//! the sender (tetrad-dsp-host `preblock::Junction`).

use tetrad_catalog::{Specs, ids};

use crate::plan::CompileError;
use crate::{Instance, Layout, Pair, SceneIndex, Site};

/// Splitter TYPE steps of model 10004.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitterType {
    Balance,
    AB,
    Crossover,
}

impl SplitterType {
    pub fn from_step(step: i32) -> Option<Self> {
        Some(match step {
            0 => Self::Balance,
            1 => Self::AB,
            2 => Self::Crossover,
            _ => return None,
        })
    }
}

/// The splitter's TYPE step in a scene (`lroundf(v / step)`).
pub(crate) fn splitter_type(
    specs: &Specs,
    splitter: &Instance,
    scene: SceneIndex,
) -> Option<SplitterType> {
    let scale = &specs.model(splitter.model.0)?.parameter(0)?.scale;
    let step = scale.step_index(splitter.params.first()?.at(scene)).ok()?;
    SplitterType::from_step(step)
}

/// Only the stock splitter (10004) and mixer (11000) have a recovered router
/// mapping (`0x582278`, `0x581f98`).
pub(crate) fn check_models(layout: &Layout) -> Result<(), CompileError> {
    for pair in Pair::ALL {
        let junctions = layout.pair(pair);
        let sites = [
            (Site::Splitter(pair), ids::SPLITTER),
            (Site::Mixer(pair), ids::MIXER),
        ];
        for ((site, expected), instance) in sites
            .into_iter()
            .zip([&junctions.splitter, &junctions.mixer])
        {
            if let Some(instance) = instance
                .as_ref()
                .filter(|instance| instance.model.0 != expected)
            {
                return Err(CompileError::Junction {
                    site,
                    model: instance.model,
                });
            }
        }
    }
    Ok(())
}

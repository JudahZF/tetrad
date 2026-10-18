//! PCOMSkewsDriver: constructor VA `0x567668` (no writes), `+0x30`
//! `0x56939c` with per-model tails `0x567ad0` (8038), `0x567bb4` (8039) and
//! `0x567cb4` (16008, 16013); every other hook is the base one.
//!
//! Each model rescales a few values on the ARM ("skews") and suppresses the
//! DSP indices those values drive. Stock asserts on an unknown model.

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{DspWrite, ModelIdentity, ParamSetMode, SetupError};
use super::catalog::{CatalogParameter, to_display as display, to_normalized};
use super::{Driver, DriverKind};

/// `0x55b47c`-style rescale: display in one range, normalized in another.
fn rescale(value: f32, from: (f32, f32, f32), to: (f32, f32, f32)) -> f32 {
    to_normalized(display(value, from.0, from.1, from.2), to.0, to.1, to.2)
}

/// OUTPUT law of the Gojira and blend models: ModelRepo range −60..12 dB,
/// skew 3.8018.
const OUTPUT: (f32, f32, f32) = (-60.0, 12.0, 3.801_8);
/// SOURCE steps are sent as `index / 44` (`0x568464`).
const SOURCE_SCALE: f32 = 44.0;
/// Blend models (`0x567cb4`): SOURCE is index 6, the sidechain routing
/// parameter.
const BLEND_MODELS: [u32; 2] = [16008, 16013];
const SOURCE: usize = 6;

#[derive(Debug)]
pub(super) struct PcomSkews {
    target: DeploymentTarget,
    model_id: u32,
    /// Blend models: the info of the block's own SOURCE parameter, which
    /// stock reads through the preset manager on each update (`0x2e5f58`,
    /// parameter `+0x690`, copy at `0x568450`). It is dynamic (ModelRepo
    /// `dynamic="true"`): its steps are Off, Follow Input, 12 inputs and the
    /// preceding blocks (tetrad-engine `sidechain_steps`), so the caller passes
    /// the per-block info. Only a grid change alters it, and a grid change
    /// rebuilds the block, so the info read at construction is current.
    source: Option<CatalogParameter>,
}

impl PcomSkews {
    pub(super) fn new(
        identity: &ModelIdentity,
        catalog: &[CatalogParameter],
    ) -> Result<Self, SetupError> {
        let model_id = identity.model_id;
        let source = BLEND_MODELS.contains(&model_id).then(|| {
            catalog
                .get(SOURCE)
                .cloned()
                .unwrap_or(CatalogParameter::DEFAULT)
        });
        Ok(Self {
            target: identity.target,
            model_id,
            source,
        })
    }
}

impl Driver for PcomSkews {
    fn kind(&self) -> DriverKind {
        DriverKind::PcomSkews
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let target = self.target;
        let mut send = |index, value| out.push(DspWrite::parameter(target, index, value));
        match (self.model_id, index) {
            (1163, 0) => send(0, display(value, 0.0, 1.0, 0.285)),
            (1163, 4) => send(4, display(value, 0.0, 1.0, 0.365_5)),
            (1164, 6) => send(6, display(value, 0.0, 1.0, 0.662)),
            (1164, 7) => {
                let shown = display(value, OUTPUT.0, OUTPUT.1, OUTPUT.2) - 8.0;
                send(7, to_normalized(shown, OUTPUT.0, OUTPUT.1, OUTPUT.2));
            }
            (6033, 0) => send(0, to_normalized(value, 0.0, 1.0, 1.4)),
            (8025, 4) | (8038, 6 | 7) | (16012, 5) => {}
            // Both also let the raw value through (stock returns 1).
            (8025, 7) => {
                send(
                    4,
                    rescale(value, (2000.0, 6000.0, 0.5), (800.0, 12000.0, 0.5)),
                );
                return true;
            }
            (8025, 1) => {
                send(1, value * 0.9);
                return true;
            }
            (8038, 10) => send(6, rescale(value, (20.0, 500.0, 0.6), (20.0, 500.0, 0.6))),
            (8038, 11) => send(
                7,
                rescale(value, (1000.0, 16000.0, 0.39), (1000.0, 20000.0, 0.39)),
            ),
            (8039, 6) => send(6, rescale(value, (20.0, 480.0, 0.6), (20.0, 500.0, 0.6))),
            (8039, 7) => send(
                7,
                rescale(value, (1500.0, 20000.0, 0.39), (1000.0, 20000.0, 0.39)),
            ),
            (16008 | 16013, 6) => {
                let source = self.source.as_ref().unwrap_or(&CatalogParameter::DEFAULT);
                send(6, source.step_index(value) as f32 / SOURCE_SCALE);
            }
            (16008 | 16013, 7) => {
                let shown = display(value, OUTPUT.0, OUTPUT.1, OUTPUT.2);
                send(
                    7,
                    if value > 0.001 {
                        10_f32.powf(shown * 0.05)
                    } else {
                        0.0
                    },
                );
            }
            (16012, 1) => send(1, rescale(value, (3.0, 20.0, 1.0), (3.0, 60.0, 1.0))),
            (18011, 0 | 1) | (18017, 0 | 2) => send(index, value.sqrt()),
            (18011, 2) | (18017, 1) => send(index, (value * 0.5).sqrt()),
            (18017, 3) => return true,
            // The octavers send nothing for their other indices.
            (18011 | 18017, _) => {}
            _ => return true,
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use tetrad_dsp_protocol::{DspCore, ModuleSlot};

    use super::*;

    fn driver(model_id: u32, catalog: &[CatalogParameter]) -> PcomSkews {
        let identity = ModelIdentity {
            target: DeploymentTarget {
                core: DspCore::Core0,
                slot: ModuleSlot::try_from(3).unwrap(),
            },
            model_id,
            category: 16,
            fx_loop_send: None,
        };
        PcomSkews::new(&identity, catalog).unwrap()
    }

    fn sent(driver: &mut PcomSkews, index: u32, value: f32) -> (bool, Vec<(u32, f32)>) {
        let mut out = Vec::new();
        let pass = driver.update_parameter(index, value, ParamSetMode::Push, &mut out);
        let writes = out
            .into_iter()
            .map(|write| match write {
                DspWrite::Parameter {
                    index, value_bits, ..
                } => (index, f32::from_bits(value_bits)),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        (pass, writes)
    }

    /// SOURCE is sent as its step index over 44 (`0x568464`). The step
    /// comes from the block's own dynamic list (review c-dsp3b4a F8): with one
    /// preceding block (15 steps) the last step is index 14, not 44.
    #[test]
    fn blend_source_is_quantized_to_its_steps() {
        let steps = |steps: i32| CatalogParameter {
            min: 0.0,
            max: (steps - 1) as f32,
            steps,
            step: 1.0 / (steps - 1) as f32,
            ..CatalogParameter::DEFAULT
        };
        let mut blend = driver(16013, &vec![steps(45); 8]);
        assert_eq!(sent(&mut blend, 6, 0.1), (false, vec![(6, 4.0 / 44.0)]));
        assert_eq!(sent(&mut blend, 7, 0.0005), (false, vec![(7, 0.0)]));
        let mut placed = driver(16013, &vec![steps(15); 8]);
        assert_eq!(sent(&mut placed, 6, 1.0), (false, vec![(6, 14.0 / 44.0)]));
    }

    /// Stock lets 8025's raw values through after its own writes, and the
    /// octavers swallow indices they do not convert.
    #[test]
    fn pass_through_and_suppression_follow_stock_returns() {
        let mut reverb = driver(8025, &[]);
        assert_eq!(sent(&mut reverb, 1, 0.5), (true, vec![(1, 0.45)]));
        assert_eq!(sent(&mut reverb, 4, 0.5), (false, vec![]));
        let mut octaver = driver(18011, &[]);
        assert_eq!(sent(&mut octaver, 2, 0.5), (false, vec![(2, 0.5)]));
        assert_eq!(sent(&mut octaver, 3, 0.5), (false, vec![]));
        assert_eq!(sent(&mut driver(18017, &[]), 3, 0.5), (true, vec![]));
    }
}

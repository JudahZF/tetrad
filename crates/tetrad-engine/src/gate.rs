//! The global Input Gate module and the lane I/O writes of the push
//! (`0x5205e8`).
//!
//! When the fit places the gate, every lane input that is not a next-row
//! input passes it: the gate receives the port (`InputGateSource` 0x2f,
//! `0x514e58`) and the lane input router reads the gate's output for the row
//! (channel 10, `0x581920`). Gate parameters go to the gate core as
//! `InputGateParameter` (0x31, `0x5187fc`), never as a ParameterChange. INPUT
//! GAIN is the lane input router's return gain (`0x581a78`).

use tetrad_catalog::{Specs, ids};
use tetrad_core::conversion::PowerLaw;

use crate::plan::{Channel, DeploymentPlan, Placement, RouterSet, RouterValue, RouterWrite};
use crate::routers::Built;
use crate::{DspCore, InputPort, Layout, ROWS, Row, SceneIndex, Scened};

/// Input trim of `0x581a78` (`0x3fa609ab`, +2.26 dB), the inverse of the
/// output trim; a lower row fed from the upper pair gets 1.
pub const INPUT_TRIM: f32 = 1.297_17;

/// INPUT GAIN law of model 28000 parameter 3: −24..24 dB, linear
/// (`0x5205e8` → `0x64b8f8` mode 0).
const INPUT_GAIN_DB: PowerLaw = PowerLaw {
    min: -24.0,
    max: 24.0,
    skew: 1.0,
};

/// Gate parameters the push sends: NOISE REDUCTION, BYPASS, GAIN REDUCTION.
pub const GATE_PARAMS: u32 = 3;
/// The INPUT GAIN parameter of model 28000.
pub const INPUT_GAIN: u32 = 3;

/// The Input Gate module of a deployment.
#[derive(Debug, Clone, PartialEq)]
pub struct GatePlan {
    pub core: DspCore,
    pub rows: [GateRow; ROWS],
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateRow {
    /// The port route the gate reads; `None` for a next-row input, which
    /// does not pass the gate.
    pub source: Option<(Channel, u8)>,
    /// Stored (normalized) NOISE REDUCTION, BYPASS and GAIN REDUCTION.
    pub params: [Scened<f32>; GATE_PARAMS as usize],
    /// The row's input trim, sent as `InputGateInputGain`.
    pub trim: f32,
}

/// One stock Input Gate message (`tetrad-dsp-protocol` `ControlRequest`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GateMessage {
    /// `InputGateEnabled` (0x30): the gate core + 1, or 0 without a gate.
    Enabled { core: DspCore, value: u32 },
    /// `InputGateSource` (0x2f).
    Source {
        core: DspCore,
        row: Row,
        channel: Channel,
        index: u8,
    },
    /// `InputGateParameter` (0x31) with the stored value.
    Parameter {
        core: DspCore,
        row: Row,
        index: u32,
        value: f32,
    },
    /// `InputGateInputGain` (0x32).
    InputGain { core: DspCore, row: Row, value: f32 },
}

impl GatePlan {
    pub(crate) fn new(layout: &Layout, specs: &Specs, core: DspCore, fed: [bool; ROWS]) -> Self {
        let defaults = specs.model(ids::INPUT_GATE_CONTROL);
        let rows = std::array::from_fn(|row| {
            let row_id = Row::new(row as u8).expect("row in range");
            let port = crate::plan::effective_input(layout, row_id);
            let control = layout.lane(row_id).input_control.as_ref();
            let params = std::array::from_fn(|index| {
                control
                    .and_then(|control| control.params.get(index).copied())
                    .unwrap_or_else(|| {
                        let default = defaults
                            .and_then(|spec| spec.parameter(index as u32))
                            .map_or(0.0, |param| param.default_normalized());
                        Scened::Fixed(default)
                    })
            });
            GateRow {
                source: (port != InputPort::PrevRow).then(|| Channel::input_route(port, row_id)),
                params,
                trim: input_trim(fed[row]),
            }
        });
        Self { core, rows }
    }
}

pub(crate) fn input_trim(fed: bool) -> f32 {
    if fed { 1.0 } else { INPUT_TRIM }
}

/// `0x581a78`: the lane input router's return gain for a stored INPUT GAIN.
/// The dB value takes the clamped, fused law (`0x64b958`); `0x581c08` gives 0
/// at or below −100 dB and lets NaN pass. With the gate the router takes the
/// plain gain and the gate the trim.
pub(crate) fn input_gain(normalized: f32, trim: f32, gate: bool) -> f32 {
    let db = INPUT_GAIN_DB.to_display(normalized);
    let gain = if db <= -100.0 {
        0.0
    } else {
        10f32.powf(db * 0.05)
    };
    if gate { gain } else { trim * gain }
}

/// The stored INPUT GAIN of a row; a lane without an input control has the
/// stock default 0 dB.
pub(crate) fn stored_input_gain(layout: &Layout, row: Row, scene: SceneIndex) -> f32 {
    layout
        .lane(row)
        .input_control
        .as_ref()
        .and_then(|control| control.params.get(INPUT_GAIN as usize))
        .map_or(0.5, |value| value.at(scene))
}

/// The return-gain parameter of a lane input router: 9 on 22000, 7 on 22002.
pub(crate) fn input_gain_index(model: u32) -> Option<u32> {
    match model {
        ids::ZEN_LINK_RX => Some(9),
        ids::ZEN_LINK_FWD => Some(7),
        _ => None,
    }
}

impl DeploymentPlan {
    /// The Input Gate messages of a push in stock order: [`Self::gate_enables`]
    /// without the capture lock, then per row the source (`0x581920`), the
    /// trim (`0x581a78`) and parameters 0..=2 (`0x5205e8`).
    pub fn gate_messages(&self, scene: SceneIndex) -> Vec<GateMessage> {
        let mut messages = self.gate_enables(false).to_vec();
        let Some(gate) = &self.gate else {
            return messages;
        };
        let core = gate.core;
        for (index, row) in gate.rows.iter().enumerate() {
            let row_id = Row::new(index as u8).expect("row in range");
            if let Some((channel, source)) = row.source {
                messages.push(GateMessage::Source {
                    core,
                    row: row_id,
                    channel,
                    index: source,
                });
            }
            messages.push(GateMessage::InputGain {
                core,
                row: row_id,
                value: row.trim,
            });
            messages.extend((0..GATE_PARAMS).map(|param| GateMessage::Parameter {
                core,
                row: row_id,
                index: param,
                value: row.params[param as usize].at(scene),
            }));
        }
        messages
    }

    /// `InputGateEnabled` of a deploy (`0x5184fc`): to the other SOC1 core,
    /// then the gate core. The value is the gate core + 1, or 0 without a
    /// gate (`+0x3408`) or under the capture lock (`+0x3409`, set while NC
    /// Recorder, NC2 or the DSP self-test runs: `0x582db8`).
    pub fn gate_enables(&self, capture_lock: bool) -> [GateMessage; 2] {
        let core = self.gate.as_ref().map_or(DspCore::ALL[0], |gate| gate.core);
        let value = match &self.gate {
            Some(gate) if !capture_lock => gate.core.index() as u32 + 1,
            _ => 0,
        };
        let other = DspCore::ALL[1 - core.index()];
        [
            GateMessage::Enabled { core: other, value },
            GateMessage::Enabled { core, value },
        ]
    }

    /// `OutputEqualizerEnable` (0x3b) of a deploy (`0x5856e8`, `0x58585c`):
    /// `+0x3411 ? 0 : !+0x3410`, with `+0x3410` = the fit dropped the global
    /// EQ (inferred from fit byte `+9`) and `+0x3411` the capture lock
    /// (`0x582d08`).
    pub fn output_eq_enabled(&self, capture_lock: bool) -> bool {
        !(capture_lock || self.inhibited.global_eq)
    }
}

/// `0x581c98` and `0x581920` per row: the lane output destination (22001
/// parameters 0/1, 22002 parameters 3/4) and the lane input source (0/1),
/// which reads the gate's row output when the gate takes the port.
pub(crate) fn lane_routes(
    layout: &Layout,
    routers: &RouterSet,
    gate: Option<&GatePlan>,
    built: &Built,
) -> Vec<RouterWrite> {
    let mut writes = Vec::new();
    let mut pair = |at: Placement, indices: [u32; 2], (channel, index): (Channel, u8)| {
        writes.extend(
            indices
                .into_iter()
                .zip([channel as i32, i32::from(index)])
                .map(|(index, value)| RouterWrite {
                    at,
                    index,
                    value: RouterValue::Int(value),
                }),
        );
    };
    for row in 0..ROWS {
        let row_id = Row::new(row as u8).expect("row in range");
        if let Some(at) = routers.lane_output[row] {
            let route = Channel::output_route(layout.lane(row_id).output);
            match built.model(at) {
                Some(ids::ZEN_LINK_TX) => pair(at, [0, 1], route),
                Some(ids::ZEN_LINK_FWD) => pair(at, [3, 4], route),
                _ => {}
            }
        }
        if let Some(at) = routers.lane_input[row]
            && built.model(at).and_then(input_gain_index).is_some()
        {
            let route = match gate.and_then(|gate| gate.rows[row].source) {
                Some(_) => (Channel::InputGate, row as u8),
                None => Channel::input_route(crate::plan::effective_input(layout, row_id), row_id),
            };
            pair(at, [0, 1], route);
        }
    }
    writes
}

#[cfg(test)]
mod tests {
    use std::hint::black_box;

    use super::*;

    /// `0x64b8f8` mode 0 clamps and rounds once (`vfma`, `0x64b958`); the
    /// unfused `min + (max − min)·u` differs in the last bit for this value.
    #[test]
    fn input_gain_uses_the_clamped_fused_law() {
        let u = black_box(f32::from_bits(0x3b0e_de55));
        let (min, max) = black_box((-24.0_f32, 24.0_f32));
        let unfused = min + (max - min) * u;
        assert_eq!(INPUT_GAIN_DB.to_display(u), -23.895_36);
        assert_eq!(unfused, -23.895_359);
        assert_ne!(input_gain(u, 1.0, true), 10f32.powf(unfused * 0.05));
        assert_eq!(
            input_gain(black_box(1.5), 1.0, true),
            input_gain(1.0, 1.0, true)
        );
    }
}

//! Lane I/O push step (VA `0x5205e8`), the last push-all step after the grid blocks: per lane (grid
//! row) the output port, the input port, the input gain, VOLUME (or the MUTE / SOLO paths), PAN and
//! the input gate.
//!
//! The lane object at controller `+0xb910` keeps state between pushes
//! ([`LaneIo`]). Router floats go through the bulk queue (`0x517010`); router
//! ints (`0x511318`), the input gate requests `0x2f`, `0x31` and `0x32`
//! (wait 0, 100 ms) and the tuner path `0x34` (wait 1, 100 ms) go out at once.

use std::collections::{BTreeMap, BTreeSet};

use tetrad_dsp_protocol::DeploymentTarget;
use tetrad_dsp_protocol::controls::ControlRequest;
use tetrad_dsp_protocol::tuner_looper::TunerLooperRequest;

use super::super::DspWrite;
use super::catalog::to_display;
use super::gain_db;
use super::sidechain::tuner_overlaps;

/// Lane input router (ModelRepo "receive" router).
pub const RECEIVE_ROUTER: u32 = 22000;
/// Lane output router (ModelRepo "send" router).
pub const SEND_ROUTER: u32 = 22001;
/// One router that is both the lane input and output.
pub const SEND_RECEIVE_ROUTER: u32 = 22002;

/// Lane outputs that feed lane 3 (16), lane 4 (17) or both (18) instead of a
/// port (`output − 16 < 3`).
const FEED_OUTPUTS: std::ops::RangeInclusive<u32> = 16..=18;
/// Input id for "fed by a lane above" (`0x581920`); no gate source is set.
const FED_INPUT: u32 = 7;
/// Router input selector written when the gate owns the input (`0x581920`).
const GATE_INPUT_SELECTOR: u32 = 10;
/// Input gain trim of lanes fed by a port (literal `0x581c00`).
const PORT_INPUT_TRIM: f32 = 1.297_17;
/// Output scale of lanes routed to a port (literal `0x581f88`).
const PORT_OUTPUT_SCALE: f32 = 0.7709;

/// A lane router the compiler placed: input router at compiler `+0x5e4`,
/// output router at `+0x604` (8 bytes per lane, VAs `0x87a18c`, `0x87a1ac`;
/// `None` when the "placed" byte `+5` is clear). `model_id` is the compiler's
/// model table entry (`+0x37c`, VA `0x87a0cc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneRouter {
    pub target: DeploymentTarget,
    pub model_id: u32,
}

/// One lane as the preset stores it (preset `+0x24`, 0x218 bytes per lane)
/// with the current scene's values, plus its compiler routers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lane {
    /// Input source id (lane `+0`).
    pub input: u32,
    /// Output destination id (lane `+4`).
    pub output: u32,
    /// Model 23000 VOLUME (index 0) and PAN (1), expression values
    /// substituted (VA `0x3221e8`).
    pub volume: f32,
    pub pan: f32,
    /// VOLUME in the scene, without the expression (`0x511fbc` →
    /// `0x2ea164`): SOLO saves it and falls back to it, scene recall sends it.
    pub scene_volume: f32,
    /// Model 23000 MUTE (2) and SOLO (3), scene values (no expression).
    pub mute: f32,
    pub solo: f32,
    /// Model 28000 (input gate) values 0..=3, expression values substituted.
    /// Index 3 is the input gain (normalized, −24..24 dB linear).
    pub gate: [f32; 4],
    /// Stock `0x5147cc`: lanes 1 and 3 only, when the row above has a
    /// splitter and no mixer (pair map `+0x14e4`), this lane has an output,
    /// and the pair's block in map `+0x14fc` has its last value above 0.5
    /// (`0x514378`). Path B is then silent.
    pub path_b_muted: bool,
    pub input_router: Option<LaneRouter>,
    pub output_router: Option<LaneRouter>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaneControl {
    Volume,
    Pan,
}

/// The controller's tuner state that the lane I/O push reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tuner {
    /// The tuner engine runs (`+0x88d8`): on from boot, off only on the
    /// Neural Capture screen.
    pub engine: bool,
    /// The tuner input port (`+0x88dc`).
    pub port: u32,
    /// The tuner mutes the lanes that read its port (`+0x88e0`).
    pub muted: bool,
}

/// The stock lane I/O object (controller `+0xb910`) and the controller state
/// the push reads. Keep one per DSP session.
#[derive(Debug, Clone, PartialEq)]
pub struct LaneIo {
    /// Destination of the input gate requests: controller `+0xed1c`, the
    /// compiler's gate core. `None` (stock −1) sends no gate request.
    pub gate_destination: Option<u8>,
    /// `+0x3408 | +0x3409`: the input gate and global EQ are off (set by
    /// `0x514f5c`, `0x582e18`, `0x581398`). The router then takes the input
    /// selector and the trimmed gain itself.
    pub gate_off: bool,
    /// `+0x228`.
    outputs: [u32; 4],
    /// `+0x238`.
    inputs: [u32; 4],
    /// `+0x3414`: linear input gain per lane.
    input_gain: [f32; 4],
    /// `+0x3424`: input muted (tuner).
    input_muted: [bool; 4],
    /// `+0x342c` while `+0x3430` is set (`0x5860a4`): the tuner port that
    /// cuts sidechains fed from it.
    tuner_cut: Option<u32>,
    /// Controller `+0xb890`: volumes saved by SOLO, keyed by (lane, output).
    /// Only lane edits (SOLO off) remove entries, so pushes keep them.
    solo_saved: BTreeMap<(usize, u32), f32>,
    /// Controller `+0xb88c`: port lanes muted by MUTE, keyed by (lane,
    /// output). SOLO does not re-send their saved volume. A lane that is
    /// unmuted drops its keys (stock does this on the MUTE-off edit).
    muted_ports: BTreeSet<(usize, u32)>,
}

impl LaneIo {
    /// A fresh lane object. The starting input gain is owned (unity); the
    /// push overwrites it before the value matters.
    pub fn new(gate_destination: Option<u8>) -> Self {
        Self {
            gate_destination,
            gate_off: false,
            outputs: [0; 4],
            inputs: [0; 4],
            input_gain: [1.0; 4],
            input_muted: [false; 4],
            tuner_cut: None,
            solo_saved: BTreeMap::new(),
            muted_ports: BTreeSet::new(),
        }
    }

    /// Stock lane I/O push (VA `0x5205e8`), after every grid block and
    /// before the bulk flush. With the tuner engine on, the lane input mutes
    /// stay and the push ends with the tuner path (`0x87590c`) and the tuner
    /// mute (`0x519dfc`). Stock then always runs the sidechain routes of the
    /// grid blocks (`0x585f04` → `0x5843a4`), which are not part of this step.
    pub fn push(&mut self, lanes: &[Lane; 4], tuner: Tuner, out: &mut Vec<DspWrite>) {
        for (index, lane) in lanes.iter().enumerate() {
            self.set_output(index, lane, out);
            self.set_input(index, lane, out);
            if !tuner.engine {
                // `0x581c78`: clear the tuner mute.
                self.input_muted[index] = false;
                self.apply_input_gain(index, lanes, out);
            }
            if lane.mute > 0.5 {
                // `0x520498` → edit `0x51e4d8` (23000, 2, 1.0): VOLUME with 0.
                self.level(lanes, index, LaneControl::Volume, 0.0, out);
                if !FEED_OUTPUTS.contains(&lane.output) {
                    self.muted_ports.insert((index, lane.output));
                }
            } else {
                self.muted_ports.retain(|&(muted, _)| muted != index);
                if lane.solo > 0.5 {
                    self.solo(index, lanes, out);
                } else {
                    self.level(lanes, index, LaneControl::Volume, lane.volume, out);
                }
            }
            self.level(lanes, index, LaneControl::Pan, lane.pan, out);
            let decibels = to_display(lane.gate[3], -24.0, 24.0, 1.0);
            self.set_input_gain(index, decibels, lanes, out);
            if let Some(destination) = self.gate_destination.filter(|&core| core <= 1) {
                for (parameter, &value) in (0..).zip(&lane.gate[..3]) {
                    // `0x5187fc`; parameter 1 also signals the UI.
                    out.push(DspWrite::Control(ControlRequest::InputGateParameter {
                        destination,
                        gate: index as u8,
                        parameter,
                        value_bits: value.to_bits(),
                    }));
                }
            }
        }
        if tuner.engine {
            // `0x87590c`: the low byte of the port word.
            out.push(DspWrite::Tuner(TunerLooperRequest::GlobalTunerInputPath {
                port: tuner.port as u8,
            }));
            self.mute_tuner_input(lanes, tuner, out);
        }
    }

    /// Tuner mute (`0x519dfc` without its sidechain step): every lane whose
    /// input overlaps the tuner port (`0x582a90`) takes the tuner mute, the
    /// others are unmuted, and each lane re-sends its input gain
    /// (`0x581c78`). The lane object keeps the port and mute for the
    /// sidechain cut (`0x5860a4`); [`Self::tuner_cut`] reads it.
    pub fn mute_tuner_input(&mut self, lanes: &[Lane; 4], tuner: Tuner, out: &mut Vec<DspWrite>) {
        for (index, lane) in lanes.iter().enumerate() {
            self.input_muted[index] = tuner.muted && tuner_overlaps(tuner.port, lane.input);
            self.apply_input_gain(index, lanes, out);
        }
        self.tuner_cut = tuner.muted.then_some(tuner.port);
    }

    /// The tuner port that cuts sidechains (`+0x342c` while `+0x3430` is
    /// set); `SidechainContext::tuner_input`.
    pub fn tuner_cut(&self) -> Option<u32> {
        self.tuner_cut
    }

    /// A live LaneOutputControl edit of lane `index` (`0x51e4d8` case
    /// 0x59d8, model 23000): VOLUME and PAN re-send the lane (`0x581de0`).
    /// MUTE and SOLO re-send every lane's VOLUME with the silence test
    /// (`0x2e1ab0`) and keep the MUTE map; the stock MUTE and SOLO edit paths
    /// (`0x520498`, `0x520540`, `0x510e4c`) with their saved volumes are
    /// inferred from the push.
    pub fn edit(
        &mut self,
        lanes: &[Lane; 4],
        index: usize,
        parameter: u32,
        out: &mut Vec<DspWrite>,
    ) {
        let lane = &lanes[index];
        match parameter {
            0 => self.level(lanes, index, LaneControl::Volume, lane.volume, out),
            1 => self.level(lanes, index, LaneControl::Pan, lane.pan, out),
            2 | 3 => {
                if lane.mute > 0.5 && !FEED_OUTPUTS.contains(&lane.output) {
                    self.muted_ports.insert((index, lane.output));
                } else if lane.mute <= 0.5 {
                    self.muted_ports.retain(|&(muted, _)| muted != index);
                }
                for (other, lane) in lanes.iter().enumerate() {
                    self.level(lanes, other, LaneControl::Volume, lane.volume, out);
                }
            }
            _ => {}
        }
    }

    /// The lane rows of scene recall (`0x527248` after the blocks): per lane
    /// VOLUME with the scene value (`0x581de0`; saved SOLO volumes take it,
    /// `0x5193f8`), PAN, the input gain (`0x581c08`) and the gate parameters
    /// 0..=2 (`0x5187fc`). Ports, MUTE and SOLO paths and the tuner tail are
    /// not part of a recall.
    pub fn recall(&mut self, lanes: &[Lane; 4], out: &mut Vec<DspWrite>) {
        for (index, lane) in lanes.iter().enumerate() {
            self.level(lanes, index, LaneControl::Volume, lane.scene_volume, out);
            if let Some(saved) = self.solo_saved.get_mut(&(index, lane.output)) {
                *saved = lane.scene_volume;
            }
            self.level(lanes, index, LaneControl::Pan, lane.pan, out);
            let decibels = to_display(lane.gate[3], -24.0, 24.0, 1.0);
            self.set_input_gain(index, decibels, lanes, out);
            if let Some(destination) = self.gate_destination {
                for (parameter, &value) in (0..).zip(&lane.gate[..3]) {
                    out.push(DspWrite::Control(ControlRequest::InputGateParameter {
                        destination,
                        gate: index as u8,
                        parameter,
                        value_bits: value.to_bits(),
                    }));
                }
            }
        }
    }

    /// `0x581c98`: store the output; the output router gets (class, port) as
    /// `0x03` 0/1 (22001) or 3/4 (22002).
    fn set_output(&mut self, index: usize, lane: &Lane, out: &mut Vec<DspWrite>) {
        self.outputs[index] = lane.output;
        let Some(router) = lane.output_router else {
            return;
        };
        let class = output_class(lane.output);
        let port = if matches!(class, 1 | 5 | 6) {
            lane.output
        } else {
            0
        };
        let first = match router.model_id {
            SEND_ROUTER => 0,
            SEND_RECEIVE_ROUTER => 3,
            _ => return,
        };
        int(out, router.target, first, class);
        int(out, router.target, first + 1, port);
    }

    /// Lane 3 (4) reads a feed when lane 1 or 2 outputs 16 (17) or 18.
    fn fed(&self, index: usize) -> bool {
        let feed = if index == 2 { 16 } else { 17 };
        index > 1 && self.outputs[..2].iter().any(|&o| o == feed || o == 18)
    }

    /// `0x581920`: store the input; lanes 3 and 4 fed from above use input 7.
    /// With the gate on and a port input, `0x2f` selects the gate source and
    /// the router reads the gate (10, lane); otherwise the router takes
    /// (class, port) itself. Router: 22000 or 22002, `0x03` 0 and 1.
    fn set_input(&mut self, index: usize, lane: &Lane, out: &mut Vec<DspWrite>) {
        self.inputs[index] = lane.input;
        let input = if self.fed(index) {
            FED_INPUT
        } else {
            lane.input
        };
        let class = input_class(input);
        let port = match class {
            1 | 7 => input,
            5 => (index as u32).wrapping_sub(2),
            _ => 0,
        };
        let (selector, source) = if !self.gate_off && input != FED_INPUT {
            // `0x514e58` sends nothing while `+0xed1c` < 0; the router still
            // reads the gate.
            if let Some(destination) = self.gate_destination {
                out.push(DspWrite::Control(ControlRequest::InputGateSource {
                    destination,
                    gate: index as u8,
                    word: class,
                    source: port,
                }));
            }
            (GATE_INPUT_SELECTOR, index as u32)
        } else {
            (class, port)
        };
        if let Some(router) = lane.input_router
            && matches!(router.model_id, RECEIVE_ROUTER | SEND_RECEIVE_ROUTER)
        {
            int(out, router.target, 0, selector);
            int(out, router.target, 1, source);
        }
    }

    /// `0x581c08`: dB → linear (`powf(10, dB·0.05)`, 0 at or below −100 dB;
    /// NaN passes `bhi`), then `0x581a78`.
    fn set_input_gain(
        &mut self,
        index: usize,
        decibels: f32,
        lanes: &[Lane; 4],
        out: &mut Vec<DspWrite>,
    ) {
        self.input_gain[index] = if decibels <= -100.0 {
            0.0
        } else {
            10_f32.powf(decibels * 0.05)
        };
        self.apply_input_gain(index, lanes, out);
    }

    /// `0x581a78`: trim 1.29717 for port-fed lanes (1.0 for fed lanes 3/4),
    /// 0 when muted. Gate on: `0x32` carries the trim and the router the
    /// gain; gate off: the router gets trim × gain. Router: 22000 index 9,
    /// 22002 index 7 (bulk queue).
    fn apply_input_gain(&self, index: usize, lanes: &[Lane; 4], out: &mut Vec<DspWrite>) {
        let mut trim = if self.fed(index) {
            1.0
        } else {
            PORT_INPUT_TRIM
        };
        if self.input_muted[index] {
            trim = 0.0;
        }
        let gain = self.input_gain[index];
        let level = if self.gate_off {
            trim * gain
        } else {
            if let Some(destination) = self.gate_destination {
                out.push(DspWrite::Control(ControlRequest::InputGateInputGain {
                    destination,
                    gate: index as u8,
                    value_bits: trim.to_bits(),
                }));
            }
            gain
        };
        let Some(router) = lanes[index].input_router else {
            return;
        };
        let parameter = match router.model_id {
            RECEIVE_ROUTER => 9,
            SEND_RECEIVE_ROUTER => 7,
            _ => return,
        };
        out.push(DspWrite::parameter(router.target, parameter, level));
    }

    /// `0x581de0`. VOLUME: lanes to a port scale by 0.7709 and are silent
    /// when muted or soloed out (`0x2e1ab0`); feed lanes pass unity and
    /// ignore mute and solo. PAN goes out unchanged. Router 22001: 3/4,
    /// 22002: 5/6 (bulk queue).
    fn level(
        &self,
        lanes: &[Lane; 4],
        index: usize,
        control: LaneControl,
        value: f32,
        out: &mut Vec<DspWrite>,
    ) {
        let lane = &lanes[index];
        let Some(router) = lane.output_router else {
            return;
        };
        let (mut level, scale) = if FEED_OUTPUTS.contains(&lane.output) {
            (if lane.path_b_muted { 0.0 } else { 1.0 }, 1.0)
        } else {
            (0.0, PORT_OUTPUT_SCALE)
        };
        if control == LaneControl::Volume && !lane.path_b_muted && !silenced(lanes, index) {
            level = scale
                * if value == 0.0 {
                    0.0
                } else {
                    gain_db(value, 52.0)
                };
        }
        let (parameter, value) = match (router.model_id, control) {
            (SEND_ROUTER, LaneControl::Volume) => (3, level),
            (SEND_ROUTER, LaneControl::Pan) => (4, value),
            (SEND_RECEIVE_ROUTER, LaneControl::Volume) => (5, level),
            (SEND_RECEIVE_ROUTER, LaneControl::Pan) => (6, value),
            _ => return,
        };
        out.push(DspWrite::parameter(router.target, parameter, value));
    }

    /// SOLO on (`0x520540` → edit `0x51e4d8` (23000, 3, 1.0)) for lane
    /// `soloed`. Unless that lane feeds a lane, every lane with an output
    /// re-sends its saved volume (if any, and not a muted port) and saves the
    /// scene volume. Then `0x510e4c` re-sends every port lane's VOLUME
    /// (muted lanes 0; saved volume, else the scene volume).
    fn solo(&mut self, soloed: usize, lanes: &[Lane; 4], out: &mut Vec<DspWrite>) {
        if !FEED_OUTPUTS.contains(&lanes[soloed].output) {
            for (index, lane) in lanes
                .iter()
                .enumerate()
                .filter(|(_, lane)| lane.output != 0)
            {
                let key = (index, lane.output);
                if let Some(&saved) = self.solo_saved.get(&key)
                    && !self.muted_ports.contains(&key)
                {
                    self.level(lanes, index, LaneControl::Volume, saved, out);
                }
                self.solo_saved.insert(key, lane.scene_volume);
            }
        }
        for (index, lane) in lanes
            .iter()
            .enumerate()
            .filter(|(_, lane)| lane.output != 0 && !FEED_OUTPUTS.contains(&lane.output))
        {
            let value = if lane.mute.abs() > 1e-5 {
                0.0
            } else {
                self.solo_saved
                    .get(&(index, lane.output))
                    .copied()
                    .unwrap_or(lane.scene_volume)
            };
            self.level(lanes, index, LaneControl::Volume, value, out);
        }
    }
}

/// `0x2e1ab0`: an unsoloed lane is silent while another lane is soloed; any
/// lane is silent while muted.
fn silenced(lanes: &[Lane; 4], index: usize) -> bool {
    let soloed_out = lanes[index].solo.abs() <= 1e-5
        && lanes
            .iter()
            .enumerate()
            .any(|(other, lane)| other != index && lane.solo > 0.0);
    soloed_out || lanes[index].mute > 0.0
}

/// `0x588aa8`: output port class.
fn output_class(output: u32) -> u32 {
    match output {
        1..=9 | 19 => 1,
        16..=18 => 5,
        10..=15 | 20..=22 => 6,
        _ => 0,
    }
}

/// `0x588c44`: input port class.
fn input_class(input: u32) -> u32 {
    match input {
        1..=6 => 1,
        7 => 5,
        16 => 6,
        8..=13 => 7,
        14 => 12,
        _ => 0,
    }
}

fn int(out: &mut Vec<DspWrite>, target: DeploymentTarget, index: u32, value: u32) {
    out.push(DspWrite::IntParameter {
        target,
        index,
        value,
    });
}

#[cfg(test)]
mod tests {
    use tetrad_dsp_protocol::{DspCore, ModuleSlot};

    use super::*;

    fn target(core: DspCore, slot: u32) -> DeploymentTarget {
        DeploymentTarget {
            core,
            slot: ModuleSlot::try_from(slot).unwrap(),
        }
    }

    fn lane(row: u32) -> Lane {
        Lane {
            input: 1,
            output: 1,
            volume: 0.769_230_8,
            pan: 0.5,
            scene_volume: 0.769_230_8,
            mute: 0.0,
            solo: 0.0,
            gate: [0.1, 0.0, 0.3, 0.5],
            path_b_muted: false,
            input_router: Some(LaneRouter {
                target: target(DspCore::Core0, row),
                model_id: RECEIVE_ROUTER,
            }),
            output_router: Some(LaneRouter {
                target: target(DspCore::Core1, row),
                model_id: SEND_ROUTER,
            }),
        }
    }

    fn gate_writes(out: &[DspWrite], lane: u8) -> Vec<ControlRequest> {
        out.iter()
            .filter_map(|write| match *write {
                DspWrite::Control(request) => Some(request),
                _ => None,
            })
            .filter(|request| match *request {
                ControlRequest::InputGateSource { gate, .. }
                | ControlRequest::InputGateInputGain { gate, .. }
                | ControlRequest::InputGateParameter { gate, .. } => gate == lane,
                _ => false,
            })
            .collect()
    }

    /// Volume writes (parameter 3) to the output router in slot `row`.
    fn volume_writes(out: &[DspWrite], row: u32) -> Vec<f32> {
        let output = target(DspCore::Core1, row);
        out.iter()
            .filter_map(|write| match *write {
                DspWrite::Parameter {
                    target,
                    index: 3,
                    value_bits,
                } if target == output => Some(f32::from_bits(value_bits)),
                _ => None,
            })
            .collect()
    }

    /// One port lane, gate on core 1: the stock write order of `0x5205e8`
    /// with the fused volume law, the 1.29717 trim on `0x32` (the router
    /// gets the gain), then the gate parameters 0..=2 as `0x31`.
    #[test]
    fn port_lane_writes_follow_stock_order() {
        let mut lanes = [lane(0), lane(1), lane(2), lane(3)];
        for other in &mut lanes[1..] {
            other.input_router = None;
            other.output_router = None;
        }
        let mut io = LaneIo::new(Some(1));
        let mut out = Vec::new();
        io.push(&lanes, Tuner::default(), &mut out);
        let input = target(DspCore::Core0, 0);
        let output = target(DspCore::Core1, 0);
        let int = |target, index, value| DspWrite::IntParameter {
            target,
            index,
            value,
        };
        let trim = DspWrite::Control(ControlRequest::InputGateInputGain {
            destination: 1,
            gate: 0,
            value_bits: PORT_INPUT_TRIM.to_bits(),
        });
        let parameter = |parameter, value: f32| {
            DspWrite::Control(ControlRequest::InputGateParameter {
                destination: 1,
                gate: 0,
                parameter,
                value_bits: value.to_bits(),
            })
        };
        let volume = 0.7709 * 10_f32.powf(0.769_230_8_f32.mul_add(52.0, -40.0) * 0.05);
        let expected = [
            int(output, 0, 1),
            int(output, 1, 1),
            DspWrite::Control(ControlRequest::InputGateSource {
                destination: 1,
                gate: 0,
                word: 1,
                source: 1,
            }),
            int(input, 0, GATE_INPUT_SELECTOR),
            int(input, 1, 0),
            trim.clone(),
            DspWrite::parameter(input, 9, 1.0),
            DspWrite::parameter(output, 3, volume),
            DspWrite::parameter(output, 4, 0.5),
            trim,
            // GAIN 0.5 → 0 dB → 1.0.
            DspWrite::parameter(input, 9, 1.0),
            parameter(0, 0.1),
            parameter(1, 0.0),
            parameter(2, 0.3),
        ];
        assert_eq!(out[..expected.len()], expected);
        // Lanes without routers still get their gate requests.
        assert_eq!(gate_writes(&out, 3).len(), 6);
    }

    /// Lane 1 feeding lane 3 (output 16): lane 3 reads input 7 with no
    /// `0x2f` and a unity trim, and lane 1 passes its volume unscaled.
    #[test]
    fn feed_lanes_bypass_the_gate_and_port_scaling() {
        let mut lanes = [lane(0), lane(1), lane(2), lane(3)];
        lanes[0].output = 16;
        lanes[0].output_router = Some(LaneRouter {
            target: target(DspCore::Core1, 0),
            model_id: SEND_RECEIVE_ROUTER,
        });
        let mut io = LaneIo::new(Some(0));
        let mut out = Vec::new();
        io.push(&lanes, Tuner::default(), &mut out);
        let feeder = target(DspCore::Core1, 0);
        let int = |target, index, value| DspWrite::IntParameter {
            target,
            index,
            value,
        };
        assert_eq!(out[..2], [int(feeder, 3, 5), int(feeder, 4, 16)]);
        let volume = 10_f32.powf(0.769_230_8_f32.mul_add(52.0, -40.0) * 0.05);
        assert!(out.contains(&DspWrite::parameter(feeder, 5, volume)));
        let fed = target(DspCore::Core0, 2);
        let at = out
            .iter()
            .position(|write| *write == int(fed, 0, 5))
            .unwrap();
        assert_eq!(out[at + 1], int(fed, 1, 0));
        let gate = gate_writes(&out, 2);
        assert!(
            !gate
                .iter()
                .any(|request| matches!(request, ControlRequest::InputGateSource { .. }))
        );
        assert_eq!(
            gate[0],
            ControlRequest::InputGateInputGain {
                destination: 0,
                gate: 2,
                value_bits: 1.0_f32.to_bits(),
            }
        );
    }

    /// MUTE silences a port lane (VOLUME 0); SOLO on lane 1 silences the
    /// other port lanes and re-sends its own gain through `0x510e4c`. No
    /// gate core: no gate requests.
    #[test]
    fn mute_and_solo_silence_port_lanes() {
        let mut lanes = [lane(0), lane(1), lane(2), lane(3)];
        lanes[0].mute = 1.0;
        lanes[1].solo = 1.0;
        let mut io = LaneIo::new(None);
        let mut out = Vec::new();
        io.push(&lanes, Tuner::default(), &mut out);
        let solo_gain = 0.7709 * 10_f32.powf(0.769_230_8_f32.mul_add(52.0, -40.0) * 0.05);
        assert_eq!(volume_writes(&out, 0), [0.0, 0.0]);
        assert_eq!(volume_writes(&out, 1), [solo_gain]);
        assert_eq!(volume_writes(&out, 2), [0.0, 0.0]);
        assert_eq!(volume_writes(&out, 3), [0.0, 0.0]);
        assert!(
            !out.iter()
                .any(|write| matches!(write, DspWrite::Control(_)))
        );
    }

    /// Tuner T3 (`0x5205e8` with `+0x88d8` set): no per-lane unmute; the
    /// push ends with `0x34` and the tuner mute, which silences only the lanes
    /// whose input overlaps the tuner port (input 3 holds port 1).
    #[test]
    fn tuner_engine_ends_the_push_with_the_tuner_path() {
        let mut lanes = [lane(0), lane(1), lane(2), lane(3)];
        lanes[1].input = 3;
        lanes[2].input = 4;
        let mut io = LaneIo::new(Some(1));
        let mut out = Vec::new();
        let tuner = Tuner {
            engine: true,
            port: 1,
            muted: true,
        };
        io.push(&lanes, tuner, &mut out);
        let path = out
            .iter()
            .position(|write| {
                *write == DspWrite::Tuner(TunerLooperRequest::GlobalTunerInputPath { port: 1 })
            })
            .unwrap();
        let trims: Vec<(u8, f32)> = out[path..]
            .iter()
            .filter_map(|write| match *write {
                DspWrite::Control(ControlRequest::InputGateInputGain {
                    gate, value_bits, ..
                }) => Some((gate, f32::from_bits(value_bits))),
                _ => None,
            })
            .collect();
        let trim = PORT_INPUT_TRIM;
        assert_eq!(trims, [(0, 0.0), (1, 0.0), (2, trim), (3, 0.0)]);
        assert_eq!(gate_writes(&out[..path], 0).len(), 5, "no unmute step");
        assert_eq!(io.tuner_cut(), Some(1));
    }

    /// Scene recall rows (`0x527248`): VOLUME with the scene value, PAN, the
    /// input gain and the gate parameters; no port words.
    #[test]
    fn recall_sends_lane_values_without_ports() {
        let mut lanes = [lane(0), lane(1), lane(2), lane(3)];
        lanes[0].volume = 0.1;
        let mut io = LaneIo::new(Some(1));
        io.push(&lanes, Tuner::default(), &mut Vec::new());
        let mut out = Vec::new();
        io.recall(&lanes, &mut out);
        assert!(
            !out.iter()
                .any(|write| matches!(write, DspWrite::IntParameter { .. }))
        );
        let volume = 0.7709 * 10_f32.powf(0.769_230_8_f32.mul_add(52.0, -40.0) * 0.05);
        let output = target(DspCore::Core1, 0);
        assert_eq!(
            out[..2],
            [
                DspWrite::parameter(output, 3, volume),
                DspWrite::parameter(output, 4, 0.5)
            ]
        );
        assert_eq!(gate_writes(&out, 0).len(), 4, "trim and three parameters");
    }

    /// Review F3: lanes 1 and 2 soloed, lane 1 feeds lane 3. From the second
    /// push on, SOLO re-sends the saved VOLUME of lane 1: its scene value, not
    /// the expression value (review c-dsp3b4a F4).
    #[test]
    fn solo_resends_saved_volume_of_feeding_lane() {
        let mut lanes = [lane(0), lane(1), lane(2), lane(3)];
        lanes[0].output = 16;
        lanes[0].scene_volume = 0.5;
        lanes[0].solo = 1.0;
        lanes[1].solo = 1.0;
        let mut io = LaneIo::new(None);
        let mut first = Vec::new();
        io.push(&lanes, Tuner::default(), &mut first);
        let mut second = Vec::new();
        io.push(&lanes, Tuner::default(), &mut second);
        let saved_gain = 10_f32.powf(0.5_f32.mul_add(52.0, -40.0) * 0.05);
        assert!(volume_writes(&first, 0).is_empty());
        assert_eq!(volume_writes(&second, 0), [saved_gain]);
    }

    /// Review F3: SOLO does not re-send the saved VOLUME of a muted port lane
    /// (stock mute map `+0xb88c`). Without that skip the lane gets a third
    /// zero write.
    #[test]
    fn solo_skips_resend_for_muted_port_lane() {
        let mut lanes = [lane(0), lane(1), lane(2), lane(3)];
        lanes[1].solo = 1.0;
        let mut io = LaneIo::new(None);
        io.push(&lanes, Tuner::default(), &mut Vec::new());
        lanes[0].mute = 1.0;
        let mut out = Vec::new();
        io.push(&lanes, Tuner::default(), &mut out);
        assert_eq!(volume_writes(&out, 0), [0.0, 0.0]);
    }
}

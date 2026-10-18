//! I/O page and Grid meters (owner: dev-c1, dev-d2).
//!
//! Stock registers the I/O page meter set with the meter service (`0x4add58`) while the page
//! shows, and the Grid set (`0x596108`) at each Grid view update. Every 100 ms (`QTimer::start(100)`, `0x4acabc`) the service sends one
//! `MeterRequest` (`0x0e`) per word of its list (`0xa492b0`), slot 0: core 0 for
//! `0x4000..=0x4003`, core 1 for `0x4021..=0x4023`, core 4 for `0x4024` (`0x4acacc`). Replies
//! are dB.

use std::collections::BTreeMap;
use std::time::Duration;

use tetrad_catalog::ParameterType;
use tetrad_dsp_protocol::requests::Request;
use tetrad_dsp_protocol::responses::{BulkMeter, DiagnosticsBlock};
use tetrad_dsp_protocol::tuner_looper::MeterRecord;
use tetrad_dsp_protocol::{CoreId, DspCore};
use tetrad_engine::{InputPort, Lane, OutputPort, Row, Site, Slot};
use tetrad_ui::IoMeters;

use super::{Current, DspCommand, Effect, State};

/// The meter service poll period (`0x4acabc`, `0x4ad9b0`).
pub const POLL: Duration = Duration::from_millis(100);

/// The meter service word list in send order (`0xa492b0`); the I/O page set holds all eight
/// (`0xa2e430`).
const WORDS: [u32; 8] = [
    0x4000, 0x4001, 0x4002, 0x4003, 0x4023, 0x4022, 0x4021, 0x4024,
];

/// The lane output levels of the Grid set (`0x4021`).
const LANES: u32 = 0x4021;

/// A Grid port clip light stays on this long after its last reading at or above 0 dB
/// (`QTimer::start(1000)` in `0x5a41d4`; `clippingTimerSlot` `0x5a4240` clears it).
const CLIP_HOLD: Duration = Duration::from_millis(1000);

fn request(word: u32) -> Effect {
    let core = match word {
        0x4021..=0x4023 => CoreId::Dsp(DspCore::Core1),
        0x4024 => CoreId::Control,
        _ => CoreId::Dsp(DspCore::Core0),
    };
    Effect::Dsp(DspCommand::Request(Request::MeterRequest {
        core,
        slot: 0,
        word,
    }))
}

/// One poll: the eight requests of the I/O page set.
pub fn requests() -> impl Iterator<Item = Effect> {
    WORDS.into_iter().map(request)
}

/// The input meter words of a row input (`0x595590`, `0x596108`): IN 1, IN 2, RET 1, RET 2
/// and their pairs; other inputs have no clip light.
fn input_words(port: InputPort) -> &'static [u32] {
    match port {
        InputPort::Input1 => &[0x4000],
        InputPort::Input2 => &[0x4001],
        InputPort::Input1And2 => &[0x4000, 0x4001],
        InputPort::Return1 => &[0x4002],
        InputPort::Return2 => &[0x4003],
        InputPort::Return1And2 => &[0x4002, 0x4003],
        _ => &[],
    }
}

/// One poll of the Grid set (`0x596108`): the input words the four rows use, then `0x4021`,
/// in the service order.
pub fn grid_requests(lanes: &[Lane]) -> impl Iterator<Item = Effect> + use<> {
    let used: Vec<u32> = (lanes.iter())
        .flat_map(|lane| input_words(lane.input))
        .copied()
        .collect();
    WORDS
        .into_iter()
        .filter(move |word| *word == LANES || used.contains(word))
        .map(request)
}

/// The row meter words (Cortex Control grid meters, `0x4df0f4`): row 0..=3.
pub const ROW_METERS: std::ops::RangeInclusive<u32> = 0x401c..=0x401f;

/// One row meter poll (`0x4df0f4`): `MeterRequest` slot 1 to the gate core (`+0xed1c`), one
/// per row. Stock sends none while a capture preset runs (`+0x88d0`) or when the gate core
/// is not 0 or 1. I: every row is polled; stock skips a row without a subscriber or whose
/// gate check `0x515140` fails.
pub fn row_requests(gate_core: Option<tetrad_engine::DspCore>) -> impl Iterator<Item = Effect> {
    let core = gate_core
        .filter(|core| core.index() <= 1)
        .and_then(|core| DspCore::try_from(core.index() as u8).ok());
    core.into_iter().flat_map(|core| {
        ROW_METERS.map(move |word| {
            Effect::Dsp(DspCommand::Request(Request::MeterRequest {
                core: CoreId::Dsp(core),
                slot: 1,
                word,
            }))
        })
    })
}

/// A row meter reply (`0x4df394`: column 1, word `0x401c + row`): the raw `f32` for the
/// Control grid meter sender. Returns whether it was used.
pub fn on_row_meter(rows: &mut [Option<f32>; 4], record: &MeterRecord) -> bool {
    let Some(row) = u32::try_from(record.index)
        .ok()
        .filter(|word| record.column == 1 && ROW_METERS.contains(word))
        .map(|word| (word - ROW_METERS.start()) as usize)
    else {
        return false;
    };
    rows[row] = Some(f32::from_bits(record.value_bits));
    true
}

/// Whether `row`'s Input Gate is on (`0x515140`, `0x2f7ed4`): the gate inhibit flags
/// (`+0x3794`/`+0x3795`) are clear and its BYPASS (28000 parameter 1) is not above 0 at the
/// current scene.
pub fn gate_on(state: &State, row: Row) -> bool {
    let Some(current) = &*state.current else {
        return false;
    };
    let deployment = &current.deployment;
    let bypass = (deployment.layout().lane(row).input_control.as_ref())
        .and_then(|control| control.params.get(1))
        .map(|value| value.at(deployment.scene()))
        .or_else(|| {
            let spec = (state.specs.as_ref())?.model(tetrad_catalog::ids::INPUT_GATE_CONTROL)?;
            Some(spec.parameter(1)?.default_normalized())
        })
        .unwrap_or(0.0);
    !deployment.plan().inhibited.input_gate && bypass <= 0.0
}

/// The row meter as `0x4df394` passes it on (Control sender lambdas `0x10fe9c`, Grid
/// `0x5a163c`): the input gate GAIN REDUCTION (28000 parameter 2, a `grMeter`) as
/// `1 − |v / floor|`, with `v = max(floor, dB)` and a reading above −0.1 dB taken as 0;
/// `floor` is that parameter's minimum (`0x4df710`: −40 dB; −40 too when the catalog has no
/// negative minimum). S7 capture: silent (−49.81 dB) sends 0.0 (C).
pub fn row_meter_level(db: f32, specs: Option<&tetrad_catalog::Specs>) -> f32 {
    let floor = (specs.and_then(|specs| specs.model(tetrad_preset::INPUT_CONTROL_ID)))
        .and_then(|model| model.parameter(2)?.range)
        .map(|range| range.min)
        .filter(|&min| min < 0.0)
        .unwrap_or(-40.0);
    let reading = if db <= -0.1 { db } else { 0.0 };
    1.0 - (reading.max(floor) / floor).abs()
}

/// The Grid Input Gate meters (`InputPortItem` `0x5a3868`). A row shows one while it has an
/// input port (`+0x18dc` not 0), no upper row feeds it (`0x6baf54`) and its gate is on
/// ([`gate_on`]). The meter reads the last row meter ([`row_meter_level`]). Before the first
/// reply stock shows the whole bar (`0x5a43e8`: the orange child is full width).
pub fn gate_meters(state: &State) -> [Option<tetrad_ui::GateMeter>; 4] {
    let Some(current) = &*state.current else {
        return [None; 4];
    };
    let layout = current.deployment.layout();
    let fed = tetrad_engine::fed_rows(layout);
    std::array::from_fn(|index| {
        let row = Row::new(index as u8).ok()?;
        let shown = layout.lane(row).input != InputPort::Empty && !fed[index];
        (shown && gate_on(state, row))
            .then(|| gate_meter(state.row_meters[index], state.specs.as_ref()))
    })
}

/// A Grid Input Gate meter for a row meter reading in dB; no reading is the whole bar.
pub fn gate_meter(db: Option<f32>, specs: Option<&tetrad_catalog::Specs>) -> tetrad_ui::GateMeter {
    tetrad_ui::GateMeter::from_level(db.map_or(0.0, |db| row_meter_level(db, specs)))
}

/// The meter parameters of a model (GridModelMeterService `0x4aa490`: count `0x492f58`, index
/// `0x493064(model, i)`). In the stock catalog each one is a `grMeter`; the second meter of
/// 6013 and 6014 is a `floatWithLed`.
fn model_meter_params(model: u32) -> &'static [u32] {
    match model {
        3008 => &[41],
        5001 | 5004 | 5005 | 5007 | 5012..=5015 | 5025 | 5029 => &[6],
        5017 | 5022 | 5023 | 5030 => &[8],
        5018..=5021 => &[7],
        5024 => &[4],
        5026 | 5027 => &[2],
        6001 | 6009 | 6011 => &[19],
        6004 => &[22],
        6005 => &[17],
        6007 => &[20],
        6008 | 6010 => &[21],
        6012 => &[23],
        6013 => &[17, 9],
        6014 => &[19, 11],
        8030 => &[11],
        _ => &[],
    }
}

/// One model meter poll (`meterRequestTimerSlot` `0x4a9844`, 100 ms, `0x4aa8e8`): for each
/// subscribed grid block whose model has meters and that is not bypassed at the current scene
/// (`0x51334c`), one `MeterRequest` per meter parameter (word = the parameter index) to the
/// block's core and slot (`0x581464`). The caller skips it while a capture preset runs
/// (`+0x758`).
pub fn model_requests(current: &Current, subscribed: impl Fn(Slot) -> bool) -> Vec<Effect> {
    let deployment = &current.deployment;
    let scene = deployment.scene();
    (deployment.plan().blocks.iter())
        .filter(|&(&slot, _)| subscribed(slot))
        .filter_map(|(&slot, placement)| {
            let instance = deployment.layout().instance(Site::Slot(slot))?;
            let core = DspCore::try_from(placement.core.index() as u8).ok()?;
            let slot = u8::try_from(placement.slot.index()).ok()?;
            let words = model_meter_params(instance.model.0);
            (!instance.bypass.at(scene)).then(|| {
                words.iter().map(move |&word| {
                    Effect::Dsp(DspCommand::Request(Request::MeterRequest {
                        core: CoreId::Dsp(core),
                        slot,
                        word,
                    }))
                })
            })
        })
        .flatten()
        .collect()
}

/// The Parallax editor poll (`0x635fa0`; 100 ms timer started in `0x636740`): one
/// `MeterRequest` for GAIN REDUCTION (word 41) to the block's core and slot (`0x581464`),
/// bypassed or not. The reply goes through [`on_model_meter`], the conversion of `0x636054`.
/// Other editors poll nothing (I: no other editor meter timer was found).
pub fn editor_requests(current: &Current, slot: Slot) -> Vec<Effect> {
    const PARALLAX: u32 = 3008;
    const GAIN_REDUCTION: u32 = 41;
    let deployment = &current.deployment;
    let block = deployment.layout().instance(Site::Slot(slot));
    let placement = deployment.plan().blocks.get(&slot);
    let (Some(block), Some(placement)) = (block, placement) else {
        return Vec::new();
    };
    let (Ok(core), Ok(column)) = (
        DspCore::try_from(placement.core.index() as u8),
        u8::try_from(placement.slot.index()),
    ) else {
        return Vec::new();
    };
    (block.model.0 == PARALLAX)
        .then_some(Effect::Dsp(DspCommand::Request(Request::MeterRequest {
            core: CoreId::Dsp(core),
            slot: column,
            word: GAIN_REDUCTION,
        })))
        .into_iter()
        .collect()
}

/// The last model meter values per grid slot for the Control sender (the `0x110f60` maps):
/// the slot's model and a value per meter parameter.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelMeters(BTreeMap<Slot, (u32, BTreeMap<u32, f32>)>);

impl ModelMeters {
    /// The slots with values, each with its values by parameter index.
    pub fn cells(&self) -> impl Iterator<Item = (Slot, &BTreeMap<u32, f32>)> {
        self.0.iter().map(|(&slot, (_, values))| (slot, values))
    }

    /// GridModelMeter DELETE All clears every map (`0x110a70`).
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

/// A model meter reply (`dspMeterResponseSlot` `0x4a9a64`): the grid block at the replying
/// core and slot (`0x510be4`) whose model lists `record.index` as a meter parameter. A
/// `floatWithLed` value is kept as it is; any other is linear: `20·log10(v)` dB (the integer
/// `min` for `v ≤ 0`), 0 dB above −0.1 dB, clamped to the integer `min..max`, then
/// `1 − |dB / (max − min)|`. Returns whether it was used. Owned: a new model in the slot
/// drops the old values (stock keeps them until DELETE All).
pub fn on_model_meter(state: &mut State, record: &MeterRecord) -> bool {
    let (Some(current), Some(specs)) = (&*state.current, &state.specs) else {
        return false;
    };
    let deployment = &current.deployment;
    let Some((&slot, _)) = deployment.plan().blocks.iter().find(|(_, placement)| {
        placement.core.index() as i32 == record.core
            && placement.slot.index() as i32 == record.column
    }) else {
        return false;
    };
    let Some(model) = (deployment.layout().instance(Site::Slot(slot))).map(|i| i.model.0) else {
        return false;
    };
    let Ok(index) = u32::try_from(record.index) else {
        return false;
    };
    let Some(param) = (model_meter_params(model).contains(&index))
        .then(|| specs.model(model)?.parameter(index))
        .flatten()
    else {
        return false;
    };
    let value = f32::from_bits(record.value_bits);
    let value = if param.kind == ParameterType::FloatWithLed {
        value
    } else {
        let (min, max) = param
            .range
            .map_or((0.0, 1.0), |range| (range.min.trunc(), range.max.trunc()));
        let db = if value > 0.0 {
            20.0 * value.log10()
        } else {
            min
        };
        let db = if db <= -0.1 { db } else { 0.0 };
        1.0 - (db.max(min).min(max) / (max - min)).abs()
    };
    let cell = state.model_meters.0.entry(slot).or_default();
    if cell.0 != model {
        *cell = (model, BTreeMap::new());
    }
    cell.1.insert(index, value);
    true
}

/// Grid port clip lights: the time each one goes out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GridClips {
    inputs: [Option<Duration>; 4],
    outputs: [Option<Duration>; 4],
}

impl GridClips {
    pub fn view(&self, now: Duration) -> tetrad_ui::GridClips {
        let lit = |until: &[Option<Duration>; 4]| until.map(|until| until.is_some_and(|t| now < t));
        tetrad_ui::GridClips {
            inputs: lit(&self.inputs),
            outputs: lit(&self.outputs),
        }
    }
}

/// After a single reply ([`on_meter`]): each row whose input reads that word lights its input
/// port when one of its input words reads at least 0 dB (`0x595590`).
pub fn grid_input_clips(state: &mut State, record: &MeterRecord, now: Duration) {
    let (Some(current), Ok(word)) = (&*state.current, u32::try_from(record.index)) else {
        return;
    };
    let meters = &state.io_meters;
    let db = |word: u32| match word {
        0x4000 | 0x4001 => meters.inputs[(word - 0x4000) as usize],
        _ => meters.returns[(word - 0x4002) as usize],
    };
    for (lane, until) in
        (current.deployment.layout().lanes.iter()).zip(&mut state.grid_clips.inputs)
    {
        let words = input_words(lane.input);
        if words.contains(&word) && words.iter().any(|&word| db(word) >= 0.0) {
            *until = Some(now + CLIP_HOLD);
        }
    }
}

/// `0x4021` (8 floats: a left/right pair per lane). Grid (`0x5937cc`): a lane pair at or above
/// 0 dB lights the row output port. I/O (`0x493464`): each pair, by the row's output port, is
/// the louder value of the HP feed meter of OUT 1..4, SEND 1, 2. Returns whether it was used.
pub fn on_lanes(state: &mut State, bulk: &BulkMeter, now: Duration) -> bool {
    let (Some(current), LANES, 8) = (&*state.current, bulk.second, bulk.values().len()) else {
        return false;
    };
    let values: Vec<f32> = bulk.values().iter().copied().map(f32::from_bits).collect();
    let mut feed = [IoMeters::FLOOR; 6];
    for (row, lane) in current.deployment.layout().lanes.iter().enumerate() {
        let [left, right] =
            [values[2 * row], values[2 * row + 1]].map(|db| db.max(IoMeters::FLOOR));
        if left >= 0.0 || right >= 0.0 {
            state.grid_clips.outputs[row] = Some(now + CLIP_HOLD);
        }
        let mut fold = |jack: usize, db: f32| feed[jack] = feed[jack].max(db);
        match lane.output {
            OutputPort::Xlr1And2 | OutputPort::MultipleOuts => {
                fold(0, left);
                fold(1, right);
            }
            OutputPort::Output3And4 => {
                fold(2, left);
                fold(3, right);
            }
            OutputPort::Send1And2 => {
                fold(4, left);
                fold(5, right);
            }
            OutputPort::Xlr1 => fold(0, left),
            OutputPort::Xlr2 => fold(1, left),
            OutputPort::Output3 => fold(2, left),
            OutputPort::Output4 => fold(3, left),
            OutputPort::Send1 => fold(4, left),
            OutputPort::Send2 => fold(5, left),
            _ => {}
        }
    }
    state.io_meters.hp_feed = feed;
    true
}

/// The DSP CPU monitor (`0x364ba0`): the last DSP Diagnostics `process_cpu` (word 5, an `f32`)
/// of each of the four cores. Stock keeps four values from 0 (`+0x148`, ctor `0x3672a8`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuLoad {
    cores: [f32; 4],
    reported: bool,
}

impl CpuLoad {
    pub fn record(&mut self, core: DspCore, block: &DiagnosticsBlock) {
        self.cores[core as usize] = f32::from_bits(block.words[5]);
        self.reported = true;
    }

    /// `+0x1c8` (`totalCpuUsageSignal`; Control CPULoad `cpu_total_load`): the mean of the four
    /// cores, a core that has not reported counting 0. `None` before the first reply.
    pub fn total(&self) -> Option<f32> {
        self.reported
            .then(|| self.cores.iter().sum::<f32>() / self.cores.len() as f32)
    }

    /// The UI CPU monitor value in whole percent. I: word 5 is a percent (the debug view prints
    /// it beside heap percents); the stock label format is not recovered.
    pub fn percent(&self) -> Option<u8> {
        self.total()
            .map(|total| total.round().clamp(0.0, 100.0) as u8)
    }
}

/// A single meter reply (`0x354468`): words `0x4000..=0x4003` are IN 1, IN 2, RET 1, RET 2.
/// Returns whether it was an I/O meter.
pub fn on_meter(meters: &mut IoMeters, record: &MeterRecord) -> bool {
    let db = f32::from_bits(record.value_bits);
    match record.index {
        0x4000 => meters.inputs[0] = db,
        0x4001 => meters.inputs[1] = db,
        0x4002 => meters.returns[0] = db,
        0x4003 => meters.returns[1] = db,
        _ => return false,
    }
    true
}

/// A bulk meter reply (`0x3562a0`; `first` is the core, `second` the word). Core 4 answers
/// `0x4024` with 16 floats, the other cores `0x4021..=0x4023` with 8. Returns whether it was
/// used.
///
/// - `0x4023` (`0x4937a8`): OUT 1..4, SEND 1, 2, HP left, right.
/// - `0x4022` (`0x4936cc`, `0x4c9af8`): limiter reduction of OUT 1..4, SEND 1, 2, kept
///   negated and at most 0 dB. Value 6 is a limiter flag (`+0x40`) and is not kept.
/// - `0x4024` (`0x49324c`): pairs 0..3 feed ids 1745..1748, pairs 4..7 ids 1741..1744.
/// - `0x4021`: see [`on_lanes`]; it needs the layout.
pub fn on_bulk(meters: &mut IoMeters, bulk: &BulkMeter) -> bool {
    let values: Vec<f32> = bulk.values().iter().copied().map(f32::from_bits).collect();
    let pair = |index: usize| [values[2 * index], values[2 * index + 1]];
    match (bulk.first, bulk.second, values.len()) {
        (4, 0x4024, 16) => {
            meters.usb_out = std::array::from_fn(pair);
            meters.usb_in = std::array::from_fn(|index| pair(index + 4));
        }
        (4, ..) => return false,
        (_, 0x4023, 8) => {
            meters.outputs.copy_from_slice(&values[..4]);
            meters.sends.copy_from_slice(&values[4..6]);
            meters.headphones.copy_from_slice(&values[6..]);
        }
        (_, 0x4022, 8) => {
            let limiters = meters.limiters.iter_mut().chain(&mut meters.send_limiters);
            for (limiter, reduction) in limiters.zip(&values) {
                *limiter = (-reduction).min(0.0);
            }
        }
        _ => return false,
    }
    true
}

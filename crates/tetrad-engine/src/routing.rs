//! Whole-layout routing validation and the resolved routing graph.
//!
//! Recovered rules: points −1..=7 per pair (`0x1cb228`); slave blocks run in column
//! order after the split whatever their column (`0x5ed844`); upper rows feed lower
//! rows through `NEXT_ROW_3`
//! (row index 2), `NEXT_ROW_4` (row 3) or `NEXT_ROW_3_4` (both) (`0x2e186c`).
//! Sidechain selectors list Off, Follow Input, twelve external inputs, then every
//! occupied block that precedes the sink in source order (`0x52e390`); the stored
//! value is normalised over that list. Owned strictness: split ≤ merge, junction models present when the
//! junction is, and shadowed slave ports are errors.
//!
//! Cycles cannot form: lanes run top to bottom, next-row links only point down, and
//! sidechain sources only name earlier blocks. Validation rejects every edge that
//! would break those three properties.

use tetrad_catalog::{ModelRole, ParameterType, Specs};

use crate::{
    COLUMNS, Column, InputPort, Instance, Layout, ModelId, OutputPort, Pair, ROWS, Row, SCENES,
    SceneIndex, Scened, Site, Slot,
};

/// What feeds a lane at its first column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneSource {
    Port(InputPort),
    /// The master lane of the pair, copied at the split column.
    Split(Pair),
    /// Upper-row lanes whose `NEXT_ROW_*` output targets this lane, summed.
    NextRow {
        upper: bool,
        lower: bool,
    },
    Silence,
}

/// Where a lane's signal goes after its last column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneSink {
    Port(OutputPort),
    /// Summed into the master lane at the merge column.
    Merge(Pair),
    /// Feeds lower-pair lanes (`NEXT_ROW_*`).
    NextRow {
        upper: bool,
        lower: bool,
    },
    Discard,
}

/// A decoded sidechain selector value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidechainSource {
    Off,
    FollowInput,
    /// One of the twelve external inputs, in stock step order (In 1 … USB 7/8).
    External(InputPort),
    Block(Slot),
}

/// Stock external sidechain steps 2..=13 (`stepNames` of every `SOURCE` parameter).
pub const SIDECHAIN_EXTERNAL: [InputPort; 12] = [
    InputPort::Input1,
    InputPort::Input2,
    InputPort::Input1And2,
    InputPort::Return1,
    InputPort::Return2,
    InputPort::Return1And2,
    InputPort::UsbIn5,
    InputPort::UsbIn6,
    InputPort::UsbIn7,
    InputPort::UsbIn8,
    InputPort::UsbIn5And6,
    InputPort::UsbIn7And8,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidechainRoute {
    pub sink: Slot,
    pub scenes: [SidechainSource; SCENES],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingGraph {
    pub sources: [LaneSource; ROWS],
    pub sinks: [LaneSink; ROWS],
    /// Occupied slots in stock source order (see [`source_order`]).
    pub order: Vec<Slot>,
    pub sidechains: Vec<SidechainRoute>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RouteError {
    #[error("{pair:?} pair splits at column {split} after it mixes at column {merge}")]
    SplitAfterMerge {
        pair: Pair,
        split: usize,
        merge: usize,
    },
    #[error("{0:?} pair has a split point but no splitter model")]
    SplitterMissing(Pair),
    #[error("{0:?} pair has a mix point but no mixer model")]
    MixerMissing(Pair),
    #[error("row {} input {port:?} is shadowed by the pair's split", .row.index())]
    ShadowedInput { row: Row, port: InputPort },
    #[error("row {} output {port:?} is shadowed by the pair's mix", .row.index())]
    ShadowedOutput { row: Row, port: OutputPort },
    #[error("row {} output {port:?}: only rows 1 and 2 can feed the next row", .row.index())]
    NextRowFromLowerPair { row: Row, port: OutputPort },
    #[error("row {} reads PREV_ROW, but no upper row feeds it", .0.index())]
    PrevRowUnfed(Row),
    #[error("row {} uses {port:?}, which has no recovered routing", .row.index())]
    UnsupportedInput { row: Row, port: InputPort },
    #[error("{site:?} model {model:?} is not in the catalog")]
    UnknownModel { site: Site, model: ModelId },
    #[error("{site:?} holds a {actual:?} model; it needs a {expected:?} model")]
    Role {
        site: Site,
        expected: ModelRole,
        actual: ModelRole,
    },
    #[error("{site:?} has {actual} parameters; model {model:?} has {expected}")]
    ParamCount {
        site: Site,
        model: ModelId,
        expected: usize,
        actual: usize,
    },
    #[error("{site:?} parameter {index} is {value} in scene {scene}, outside 0..=1")]
    ParamRange {
        site: Site,
        index: u32,
        scene: usize,
        value: f32,
    },
    #[error("{site:?} sets the sidechain sink flag, but model {model:?} has no sidechain input")]
    SidechainFlags { site: Site, model: ModelId },
    #[error("{sink:?} sidechain step {step} in scene {scene} is outside its {steps} sources")]
    SidechainStep {
        sink: Slot,
        scene: usize,
        step: u32,
        steps: u32,
    },
}

/// Validates the whole layout against the catalog and resolves its routing.
pub fn route(layout: &Layout, specs: &Specs) -> Result<RoutingGraph, RouteError> {
    for (site, instance) in layout.sites() {
        check_instance(site, instance, specs)?;
    }
    let mut sources = [LaneSource::Silence; ROWS];
    let mut sinks = [LaneSink::Discard; ROWS];
    for pair in Pair::ALL {
        check_pair(layout, pair, &mut sources, &mut sinks)?;
    }
    link_rows(layout, &mut sources, &mut sinks)?;
    let order = source_order(layout);
    let sidechains = sidechains(layout, specs, &order)?;
    Ok(RoutingGraph {
        sources,
        sinks,
        order,
        sidechains,
    })
}

fn check_instance(site: Site, instance: &Instance, specs: &Specs) -> Result<(), RouteError> {
    let model = instance.model;
    let spec = specs
        .model(model.0)
        .ok_or(RouteError::UnknownModel { site, model })?;
    let expected = match site {
        Site::InputControl(_) => ModelRole::InputGate,
        Site::OutputControl(_) => ModelRole::LaneOutput,
        Site::Slot(_) => ModelRole::Block,
        Site::Splitter(_) => ModelRole::Splitter,
        Site::Mixer(_) => ModelRole::Mixer,
    };
    if spec.role != expected {
        return Err(RouteError::Role {
            site,
            expected,
            actual: spec.role,
        });
    }
    if instance.params.len() != spec.parameters.len() {
        return Err(RouteError::ParamCount {
            site,
            model,
            expected: spec.parameters.len(),
            actual: instance.params.len(),
        });
    }
    if instance.sidechain.sink && spec.sidechain.is_none() {
        return Err(RouteError::SidechainFlags { site, model });
    }
    // Stored values are normalized; meters and strings are never DSP inputs.
    for (parameter, value) in spec.parameters.iter().zip(&instance.params) {
        if parameter.kind.is_meter() || parameter.kind == ParameterType::String {
            continue;
        }
        for scene in 0..SCENES {
            let value = value.at(SceneIndex(scene as u8));
            if !(0.0..=1.0).contains(&value) {
                return Err(RouteError::ParamRange {
                    site,
                    index: parameter.index,
                    scene,
                    value,
                });
            }
        }
    }
    Ok(())
}

fn check_pair(
    layout: &Layout,
    pair: Pair,
    sources: &mut [LaneSource; ROWS],
    sinks: &mut [LaneSink; ROWS],
) -> Result<(), RouteError> {
    let junctions = layout.pair(pair);
    let (master, slave) = (pair.master(), pair.slave());
    let split = junctions.split.map(Column::index);
    let merge = junctions.merge.map(Column::index);
    if let (Some(split), Some(merge)) = (split, merge)
        && split > merge
    {
        return Err(RouteError::SplitAfterMerge { pair, split, merge });
    }
    if split.is_some() && junctions.splitter.is_none() {
        return Err(RouteError::SplitterMissing(pair));
    }
    if merge.is_some() && junctions.mixer.is_none() {
        return Err(RouteError::MixerMissing(pair));
    }
    let slave_lane = layout.lane(slave);
    if split.is_some() && slave_lane.input != InputPort::Empty {
        return Err(RouteError::ShadowedInput {
            row: slave,
            port: slave_lane.input,
        });
    }
    if merge.is_some() && slave_lane.output != OutputPort::Empty {
        return Err(RouteError::ShadowedOutput {
            row: slave,
            port: slave_lane.output,
        });
    }
    for row in [master, slave] {
        sources[row.index()] = LaneSource::Port(layout.lane(row).input);
        sinks[row.index()] = LaneSink::Port(layout.lane(row).output);
    }
    if split.is_some() {
        sources[slave.index()] = LaneSource::Split(pair);
    }
    if merge.is_some() {
        sinks[slave.index()] = LaneSink::Merge(pair);
    }
    Ok(())
}

/// Port legality and `NEXT_ROW_*` → `PREV_ROW` links (`0x2e186c`).
fn link_rows(
    layout: &Layout,
    sources: &mut [LaneSource; ROWS],
    sinks: &mut [LaneSink; ROWS],
) -> Result<(), RouteError> {
    let mut feeds = [[false; 2]; 2];
    for row in (0..ROWS as u8).map(Row) {
        let output = layout.lane(row).output;
        if matches!(sinks[row.index()], LaneSink::Merge(_)) {
            continue;
        }
        match output {
            port if port.is_next_row() && row.pair() == Pair::Lower => {
                return Err(RouteError::NextRowFromLowerPair { row, port });
            }
            port if port.is_next_row() => {
                let upper = matches!(port, OutputPort::NextRow3 | OutputPort::NextRow3And4);
                let lower = matches!(port, OutputPort::NextRow4 | OutputPort::NextRow3And4);
                feeds[0][row.index()] = upper;
                feeds[1][row.index()] = lower;
                sinks[row.index()] = LaneSink::NextRow { upper, lower };
            }
            _ => {}
        }
    }
    // A fed lower row takes the upper row's signal; `0x2e186c` never reads its
    // own stored input then. A split slave keeps its master's signal.
    for (target, fed_by) in feeds.iter().enumerate() {
        let row = Row(2 + target as u8);
        if fed_by.contains(&true) && matches!(sources[row.index()], LaneSource::Port(_)) {
            sources[row.index()] = LaneSource::NextRow {
                upper: fed_by[0],
                lower: fed_by[1],
            };
        }
    }
    for row in (0..ROWS as u8).map(Row) {
        match sources[row.index()] {
            LaneSource::Port(InputPort::PrevRow) => return Err(RouteError::PrevRowUnfed(row)),
            LaneSource::Port(port @ InputPort::SidechainBuffer) => {
                return Err(RouteError::UnsupportedInput { row, port });
            }
            LaneSource::Port(InputPort::Empty) => sources[row.index()] = LaneSource::Silence,
            _ => {}
        }
    }
    Ok(())
}

/// Stock source order for sidechain selectors (`0x52e390`): a master row after
/// the rows above it; a slave row after its master through the mix column.
/// Slots are listed in the order a selector enumerates them.
pub fn source_order(layout: &Layout) -> Vec<Slot> {
    let mut order = Vec::new();
    let mut push = |row: Row, columns: std::ops::Range<usize>| {
        for column in columns {
            if layout.lane(row).slots[column].is_some() {
                order.push(Slot {
                    row,
                    column: Column(column as u8),
                });
            }
        }
    };
    for pair in Pair::ALL {
        let mix_end = layout.pair(pair).merge.map_or(COLUMNS, |c| c.index() + 1);
        push(pair.master(), 0..mix_end);
        push(pair.slave(), 0..COLUMNS);
        push(pair.master(), mix_end..COLUMNS);
    }
    order
}

/// Sources a selector at `sink` may name, row-major: complete rows above,
/// for a slave sink its master only through the mix column, then the sink
/// row before the sink (`0x52e390`, `tetrad-core::SourceLayout`).
fn preceding(layout: &Layout, sink: Slot) -> Vec<Slot> {
    let last = sink.row.index();
    let mix_end = layout
        .pair(sink.row.pair())
        .merge
        .map_or(COLUMNS, |column| column.index() + 1);
    (0..=last)
        .flat_map(|row| {
            let end = if row == last {
                sink.column.index()
            } else if last % 2 == 1 && row == last - 1 {
                mix_end
            } else {
                COLUMNS
            };
            (0..end)
                .filter(move |&column| layout.lanes[row].slots[column].is_some())
                .map(move |column| Slot {
                    row: Row(row as u8),
                    column: Column(column as u8),
                })
        })
        .collect()
}

/// Number of steps in a sink's selector: Off, Follow Input, 12 externals, then
/// the preceding blocks. Stored values are `step / (steps − 1)`.
pub fn sidechain_steps(layout: &Layout, sink: Slot) -> u32 {
    14 + preceding(layout, sink).len() as u32
}

/// (O) `stripped` is `full` without some grid blocks (dormant models): each sidechain selector
/// of `stripped` is re-encoded for its shorter source list, so it names the same source. A
/// selector on a removed block becomes Off.
pub(crate) fn keep_sidechain_sources(full: &Layout, stripped: &mut Layout, specs: &Specs) {
    let slots = (0..ROWS).flat_map(|row| {
        (0..COLUMNS).map(move |column| Slot {
            row: Row(row as u8),
            column: Column(column as u8),
        })
    });
    for slot in slots {
        let Some(instance) = stripped.instance(Site::Slot(slot)) else {
            continue;
        };
        let Some((sidechain, param)) = specs.model(instance.model.0).and_then(|spec| {
            let sidechain = spec.sidechain?;
            Some((sidechain, spec.parameter(sidechain.source_param)?))
        }) else {
            continue;
        };
        let (before, after) = (preceding(full, slot), preceding(stripped, slot));
        if before.len() == after.len() {
            continue;
        }
        let (mut old, mut new) = (param.scale.clone(), param.scale.clone());
        old.set_steps(14 + before.len() as i32);
        new.set_steps(14 + after.len() as i32);
        let keep = |value: f32| {
            let step = old.step_index(value).unwrap_or(0);
            let step = match usize::try_from(step - 14) {
                Ok(index) => (before.get(index))
                    .and_then(|source| after.iter().position(|kept| kept == source))
                    .map_or(0, |index| 14 + index as i64),
                Err(_) => i64::from(step),
            };
            new.step_value(step).unwrap_or(0.0)
        };
        let instance = stripped
            .instance_mut(Site::Slot(slot))
            .expect("checked above");
        let selector = &mut instance.params[sidechain.source_param as usize];
        *selector = match *selector {
            Scened::Fixed(value) => Scened::Fixed(keep(value)),
            Scened::PerScene(values) => Scened::PerScene(values.map(keep)),
        };
    }
}

impl Layout {
    /// The blocks a selector at `sink` lists after its 14 fixed steps, in step order
    /// (`0x52ebf4` emits them row-major from the `0x52e390` mask).
    pub fn sidechain_sources(&self, sink: Slot) -> Vec<Slot> {
        preceding(self, sink)
    }
}

fn sidechains(
    layout: &Layout,
    specs: &Specs,
    order: &[Slot],
) -> Result<Vec<SidechainRoute>, RouteError> {
    let mut routes = Vec::new();
    for &slot in order {
        let instance = layout.instance(Site::Slot(slot)).expect("ordered slot");
        let Some(sidechain) = specs
            .model(instance.model.0)
            .and_then(|spec| spec.sidechain)
        else {
            continue;
        };
        let sources = preceding(layout, slot);
        let steps = 14 + sources.len() as u32;
        let selector = instance.params[sidechain.source_param as usize];
        // The grid sets the step count (`setSteps`); the step is
        // `lroundf(v / step_size)` (`0x5cc994`, `tetrad_core::conversion`).
        let mut scale = specs
            .model(instance.model.0)
            .and_then(|spec| spec.parameter(sidechain.source_param))
            .map(|param| param.scale.clone())
            .expect("validated selector");
        scale.set_steps(steps as i32);
        let mut scenes = [SidechainSource::Off; SCENES];
        for (scene, source) in scenes.iter_mut().enumerate() {
            let value = selector.at(SceneIndex(scene as u8));
            let step = scale
                .step_index(value)
                .ok()
                .and_then(|step| u32::try_from(step).ok())
                .unwrap_or(u32::MAX);
            *source = match step {
                0 => SidechainSource::Off,
                1 => SidechainSource::FollowInput,
                2..=13 => SidechainSource::External(SIDECHAIN_EXTERNAL[step as usize - 2]),
                _ => match sources.get(step as usize - 14) {
                    Some(&block) => SidechainSource::Block(block),
                    None => {
                        return Err(RouteError::SidechainStep {
                            sink: slot,
                            scene,
                            step,
                            steps,
                        });
                    }
                },
            };
        }
        routes.push(SidechainRoute { sink: slot, scenes });
    }
    Ok(routes)
}

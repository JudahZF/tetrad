//! Typed DSP deployment plan: what each SHARC core runs, in run-list order.
//!
//! The router builder ([`crate::routers`]) ports the stock layout builder
//! `0x87fc2c`; the alignment ([`crate::align`]) ports `0x87c11c`/`0x87c958`;
//! the input gate ([`crate::gate`]) ports `0x5184fc`, `0x581920`, `0x581a78` and
//! `0x5187fc`.

use std::collections::BTreeMap;

use tetrad_catalog::{ModuleStats, Specs};

use crate::fit::{Assignment, Fit, Globals, Inhibited, StoredMasks, fit, fit_stored};
use crate::gate::GatePlan;
use crate::layout::collapse;
use crate::routing::{RouteError, RoutingGraph, SidechainSource, route};
use crate::{
    DspCore, FitError, InputPort, Layout, ModelId, OutputPort, Pair, ROWS, Row, SCENES, SceneIndex,
    Scened, Site, Slot, TextParam,
};

/// Run-list slots per core (`[4][30]` arrays, 30-bit start masks in `0x89ece0`).
pub const SLOTS_PER_CORE: usize = 30;
/// Latency each cross-core hop adds, in samples.
pub const HOP_LATENCY: u32 = 32;
/// Fixed latency of the FIR cab engines 12000 and 12100 (`0x87fc2c`).
pub const CAB_LATENCY: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotIndex(pub(crate) u8);

impl SlotIndex {
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// Where one deployed node runs (the stock 6-byte handle: core, slot, valid).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Placement {
    pub core: DspCore,
    pub slot: SlotIndex,
}

/// Router channel types: parameter 0 of 22000/22001 and parameters 0/3 of 22002
/// (`0x588c44` inputs, `0x588aa8` outputs, `0x87dc8c`, `0x87ebc4`, `0x581920`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Channel {
    /// An empty port.
    Unrouted = 0,
    Physical = 1,
    SlaveLink = 2,
    DualCore = 3,
    /// A copy between two buffers of one core (split, merge).
    Local = 4,
    NextRow = 5,
    /// USB outputs, and input port value 16.
    Usb = 6,
    UsbInput = 7,
    Looper = 8,
    /// The Input Gate module's output for a row (`0x581920`).
    InputGate = 10,
    Sidechain = 12,
}

impl Channel {
    pub fn of_input(port: InputPort) -> Option<Self> {
        Some(match port as u8 {
            1..=6 => Self::Physical,
            7 => Self::NextRow,
            8..=13 => Self::UsbInput,
            14 => Self::Sidechain,
            _ => return None,
        })
    }

    pub fn of_output(port: OutputPort) -> Option<Self> {
        Some(match port as u8 {
            1..=9 | 19 => Self::Physical,
            16..=18 => Self::NextRow,
            10..=15 | 20..=22 => Self::Usb,
            _ => return None,
        })
    }

    /// Channel type and index of a lane input (`0x588c44`, `0x588b5c`): ports
    /// keep their number, the next-row channel is the row minus two.
    pub fn input_route(port: InputPort, row: Row) -> (Self, u8) {
        match Self::of_input(port) {
            Some(channel @ (Self::Physical | Self::UsbInput)) => (channel, port as u8),
            Some(Self::NextRow) => (Self::NextRow, (row.index() as u8).saturating_sub(2)),
            Some(channel) => (channel, 0),
            None => (Self::Unrouted, 0),
        }
    }

    /// Channel type and index of a lane output (`0x588aa8`, `0x588b20`).
    pub fn output_route(port: OutputPort) -> (Self, u8) {
        match Self::of_output(port) {
            Some(channel) => (channel, port as u8),
            None => (Self::Unrouted, 0),
        }
    }
}

/// What a router serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Endpoint {
    /// Lane input router (`+0x5e4`).
    LaneInput(Row),
    /// Lane output router (`+0x604`).
    LaneOutput(Row),
    /// An empty row's port-to-port forward on core 1: input, output and
    /// output delay at once.
    LaneThrough(Row),
    /// Core-1 delay before a lane output (`+0x654`).
    OutputDelay(Row),
    /// The receiving end of a cross-core channel.
    Link(usize),
    /// Split router: the master buffer copied to the B side (`+0x5a4`).
    Split(Pair),
    /// Merge router: B mixed into the master buffer (`+0x5c4`).
    Merge(Pair),
    /// Merge alignment delays (`+0xbfc` master, `+0xc0c` slave).
    MergeDelay { pair: Pair, slave: bool },
    /// Sidechain sink router of a block (`+0x7b4`).
    Sidechain(Slot),
    /// SOC2 sidechain source forward on core 1 (`+0x6b4`).
    SidechainSource(Slot),
    /// FX loop send `index` of a block (`+0x624`).
    FxSend { slot: Slot, index: u8 },
    /// FX loop return `index` of a block (`+0x634`; delay `+0x644`).
    FxReturn { slot: Slot, index: u8 },
    /// Looper channel `index` of a block (`+0x694` send, `+0x6a4` return).
    Looper { slot: Slot, index: u8 },
}

/// What one slot runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Node {
    /// A grid block model.
    Model(Site),
    /// The 10003 crossover head of a split path (`+0x674`).
    PathHead { pair: Pair, slave: bool },
    /// ZenLinkRx (22000).
    Receive(Endpoint),
    /// ZenLinkTx (22001).
    Send(Endpoint),
    /// ZenLinkFwd (22002), always on core 1.
    Forward(Endpoint),
    /// ZenLinkDelay (22003).
    Delay(Endpoint),
}

/// A router parameter value with its stock message: `Int` goes out as
/// IntParameterChange (0x03), `Float` as ParameterChange (0x02) (`0x582e80`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RouterValue {
    Int(i32),
    Float(f32),
}

/// One router parameter write.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouterWrite {
    pub at: Placement,
    pub index: u32,
    pub value: RouterValue,
}

/// One run-list slot.
#[derive(Debug, Clone, PartialEq)]
pub struct Deployed {
    pub placement: Placement,
    pub node: Node,
    /// The model id in the run list (`+0x37c`): 22000..=22003 for routers;
    /// for blocks the fit's model, except that the FIR cab engines 12000 and
    /// 12100 run the block's own cab model (`0x87fc2c`).
    pub model: ModelId,
    /// Block values per scene: stored normalized XML values, then MUTE and
    /// BYPASS (0.0/1.0). Empty for routers and path heads, which only take
    /// router and junction writes.
    pub values: Vec<Scened<f32>>,
    pub bypass: Scened<bool>,
    /// Parameter indices never pushed: meters, strings and the bypass pair.
    pub skipped: Vec<u32>,
    pub texts: Vec<TextParam>,
    /// Module latency in samples as the alignment charges it.
    pub latency: u32,
    /// Start record layout word (`+0x19c`, read by `0x87a314`): the slot's
    /// audio buffer on its core (fit `0x5fb108`); 0 for core-1 forwards.
    pub layout: u16,
    /// Index into [`CorePlan::blobs`] (`+0xc58`); `None` for routers.
    pub blob: Option<usize>,
    /// Router setup writes in stock order (the `+0xc40` map that `0x582e80`
    /// flushes before the value push); empty for models.
    pub setup: Vec<RouterWrite>,
}

/// One blob a core loads (`+0x1c` names, `+0x28` model ids). Same-name models
/// share one load when their stats allow code reuse (`0x87b904`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobLoad {
    pub model: ModelId,
    /// ModelRepo blob name; `None` when the model has none.
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CorePlan {
    pub core: DspCore,
    pub slots: Vec<Deployed>,
    /// Blob loads in stock list order; `0x879100` loads them round-robin
    /// over the cores and `0x5e62a0` packs them in this order.
    pub blobs: Vec<BlobLoad>,
    pub usage: ModuleStats,
    pub budget: ModuleStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LinkKind {
    /// Same SOC (`Channel::DualCore`).
    DualCore,
    /// Across SOCs (`Channel::SlaveLink`).
    SlaveLink,
}

/// A cross-core audio channel: send router → receive router.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    pub kind: LinkKind,
    pub from: Placement,
    pub to: Placement,
    /// Send-side channel from the stock counters (`+0x56c`..`+0x594`).
    pub channel: u8,
}

/// `0x5843a4`: a grid source on the sink's core uses the same-core route (0x49),
/// otherwise the sidechain buffer (0x48).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidechainPath {
    SameCore,
    Buffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidechainScene {
    pub source: SidechainSource,
    /// Source placement and path for grid sources.
    pub route: Option<(Placement, SidechainPath)>,
    /// Channel: the sink's per-SOC channel, +2 across SOCs.
    pub channel: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidechainPlan {
    pub sink: Placement,
    pub site: Slot,
    /// The sink's receive router; `None` for 16008, 16013, 18012 and 30001.
    pub router: Option<Placement>,
    /// The SOC2 source forward on core 1 (`+0x6b4`); `None` on SOC1.
    pub source_router: Option<Placement>,
    /// The sink's per-SOC channel (`+0x934`), before the +2 across SOCs.
    pub channel: u8,
    /// The SOC2 SlaveLink receive channel (`+0x8b4`); `None` on SOC1.
    pub slave_channel: Option<u32>,
    pub scenes: [SidechainScene; SCENES],
}

/// Sidechain sinks without a receive router (no cost C in `0x5fb108`); their
/// router flag is not toggled by bypass (`0x517228`).
pub const ROUTERLESS_SIDECHAIN: [u32; 4] = [16_008, 16_013, 18_012, 30_001];

/// Split/mix alignment of one pair per scene (`0x87c11c`): the delay added on the
/// shorter side so both paths reach the mixer together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PairDelay {
    pub master: u32,
    pub slave: u32,
}

/// The stock per-row and per-pair router handle tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RouterSet {
    /// `+0x5e4`: the router that receives the lane input.
    pub lane_input: [Option<Placement>; ROWS],
    /// `+0x604`: the router that sends the lane output (gain, pan).
    pub lane_output: [Option<Placement>; ROWS],
    /// `+0x654`: the router that delays the lane output.
    pub output_delay: [Option<Placement>; ROWS],
    /// `+0x5a4`, by pair.
    pub split: [Option<Placement>; 2],
    /// `+0x5c4`, by pair.
    pub merge: [Option<Placement>; 2],
    /// `+0xbfc` / `+0xc0c`, by pair: master then slave.
    pub merge_delay: [[Option<Placement>; 2]; 2],
    /// `+0x674`: the 10003 head of each row.
    pub heads: [Option<Placement>; ROWS],
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeploymentPlan {
    pub cores: [CorePlan; 4],
    pub links: Vec<Link>,
    pub sidechains: Vec<SidechainPlan>,
    /// Per pair, per scene merge alignment delays.
    pub delays: [[PairDelay; SCENES]; 2],
    /// Total latency of each lane per scene (`+0x1b0c`).
    pub lane_latency: [[u32; SCENES]; ROWS],
    pub gate_core: Option<DspCore>,
    pub inhibited: Inhibited,
    /// Dynamic delay compensation: bypassed blocks drop their latency.
    pub ddc: bool,
    /// The fit's core assignment; an edit may keep it ([`compile_keeping`]).
    pub assignment: Assignment,
    pub routers: RouterSet,
    /// The placement whose values and bypass a block's edits target (the
    /// `+0xe38` map): the model slot, or the FX loop / looper router.
    pub blocks: BTreeMap<Slot, Placement>,
    /// Per scene alignment writes (`0x87c11c`, `0x87c958`), all `Int`.
    pub delay_writes: [Vec<RouterWrite>; SCENES],
    /// Lane destination and source rewrites of the push (`0x581c98`,
    /// `0x581920`), all `Int`, row by row.
    pub lane_routes: Vec<RouterWrite>,
    /// The Input Gate module; `None` when the fit placed no gate.
    pub gate: Option<GatePlan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompileOptions {
    pub globals: Globals,
    pub ddc: bool,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CompileError {
    #[error(transparent)]
    Route(#[from] RouteError),
    #[error(transparent)]
    Fit(#[from] FitError),
    #[error("core {} needs {count} run-list slots; it has 30", .core.index())]
    Slots { core: DspCore, count: usize },
    #[error("{kind:?} links on {soc:?} SOC need {count} channels; the limit is {limit}")]
    Links {
        kind: LinkKind,
        soc: Pair,
        count: usize,
        limit: usize,
    },
    #[error("{0:?} SOC needs a third sidechain channel")]
    SidechainChannels(Pair),
    #[error("{site:?} model {model:?} has no recovered junction mapping")]
    Junction { site: Site, model: ModelId },
}

impl DeploymentPlan {
    pub fn slot(&self, placement: Placement) -> Option<&Deployed> {
        self.cores[placement.core.index()]
            .slots
            .get(placement.slot.index())
    }

    pub fn placement(&self, node: Node) -> Option<Placement> {
        self.deployed()
            .find(|slot| slot.node == node)
            .map(|slot| slot.placement)
    }

    pub fn deployed(&self) -> impl Iterator<Item = &Deployed> {
        self.cores.iter().flat_map(|core| &core.slots)
    }

    /// The placement a block's values and bypass go to.
    pub fn block(&self, slot: Slot) -> Option<Placement> {
        self.blocks.get(&slot).copied()
    }

    /// `0x87d0d8`: the reuse push zeroes the delay parameter of the ten
    /// alignment routers before the new alignment, so a delay that the new
    /// alignment does not write ends at 0. Stock order: per pair the master
    /// and slave merge delays and the pair's two output delays, then the
    /// lane inputs of rows 3 and 4.
    pub fn delay_resets(&self) -> Vec<RouterWrite> {
        let routers = &self.routers;
        let pairs = (0..2).flat_map(|pair| {
            let [master, slave] = routers.merge_delay[pair];
            [
                master,
                slave,
                routers.output_delay[2 * pair],
                routers.output_delay[2 * pair + 1],
            ]
        });
        pairs
            .chain([routers.lane_input[2], routers.lane_input[3]])
            .flatten()
            .filter_map(|at| {
                let index = crate::align::delay_index(self.slot(at)?.model.0)?;
                Some(RouterWrite {
                    at,
                    index,
                    value: RouterValue::Int(0),
                })
            })
            .collect()
    }
}

/// Validates, fits and plans a layout: the stock full-compile result.
pub fn compile(
    layout: &Layout,
    specs: &Specs,
    options: CompileOptions,
) -> Result<DeploymentPlan, CompileError> {
    compile_keeping(layout, specs, options, None)
}

/// [`compile`] for an incremental edit: a row pair whose items did not change
/// keeps its previous core assignment when it still fits (`0x5efa08`).
pub fn compile_keeping(
    layout: &Layout,
    specs: &Specs,
    options: CompileOptions,
    previous: Option<&Assignment>,
) -> Result<DeploymentPlan, CompileError> {
    let graph = route(layout, specs)?;
    let fit = fit(layout, specs, options.globals, previous)?;
    plan(layout, specs, options, &graph, &fit)
}

/// [`compile`] with the core masks a preset stores ([`StoredMasks`]), as the
/// stock preset load does (`0x681d44` → `0x5f3940`).
pub fn compile_stored(
    layout: &Layout,
    specs: &Specs,
    options: CompileOptions,
    stored: StoredMasks,
) -> Result<DeploymentPlan, CompileError> {
    let graph = route(layout, specs)?;
    let fit = fit_stored(layout, specs, options.globals, stored)?;
    plan(layout, specs, options, &graph, &fit)
}

fn plan(
    layout: &Layout,
    specs: &Specs,
    options: CompileOptions,
    graph: &RoutingGraph,
    fit: &Fit,
) -> Result<DeploymentPlan, CompileError> {
    let built = crate::routers::build(layout, specs, graph, fit)?;
    crate::junction::check_models(layout)?;
    let alignment = crate::align::align(layout, &built, fit.gate_core, options.ddc);
    let gate = fit
        .gate_core
        .map(|core| GatePlan::new(layout, specs, core, fed_rows(layout)));
    let lane_routes = crate::gate::lane_routes(layout, &built.routers, gate.as_ref(), &built);
    let mut slots = built.slots.into_iter();
    let mut blobs = built.blobs.into_iter();
    let cores = DspCore::ALL.map(|core| CorePlan {
        core,
        slots: slots.next().unwrap_or_default(),
        blobs: blobs.next().unwrap_or_default(),
        usage: fit.usage[core.index()],
        budget: fit.budgets[core.index()],
    });
    Ok(DeploymentPlan {
        cores,
        links: built.links,
        sidechains: built.sidechains,
        delays: alignment.pairs,
        lane_latency: alignment.lanes,
        gate_core: fit.gate_core,
        inhibited: fit.inhibited,
        ddc: options.ddc,
        assignment: fit.assignment(),
        routers: built.routers,
        blocks: built.blocks,
        delay_writes: alignment.writes,
        lane_routes,
        gate,
    })
}

/// `0x582504` per row: rows 2 and 3 take the next-row output of row 0 or 1
/// (`NEXT_ROW_3`, `NEXT_ROW_4`, `NEXT_ROW_3_4`).
pub fn fed_rows(layout: &Layout) -> [bool; ROWS] {
    let upper = [0, 1].map(|row| layout.lanes[row].output);
    let feeds = |wanted| {
        upper
            .iter()
            .any(|&port| port == wanted || port == OutputPort::NextRow3And4)
    };
    [
        false,
        false,
        feeds(OutputPort::NextRow3),
        feeds(OutputPort::NextRow4),
    ]
}

/// The port a row reads (`0x87fc2c` `local_38`): `PREV_ROW` when an upper row
/// feeds it, else the stored input.
pub(crate) fn effective_input(layout: &Layout, row: Row) -> InputPort {
    if fed_rows(layout)[row.index()] {
        InputPort::PrevRow
    } else {
        layout.lane(row).input
    }
}

pub(crate) fn per_scene<T: Copy + PartialEq>(value: impl Fn(SceneIndex) -> T) -> Scened<T> {
    collapse(std::array::from_fn(|scene| value(SceneIndex(scene as u8))))
}

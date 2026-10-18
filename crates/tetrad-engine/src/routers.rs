//! Port of the stock layout builder `0x87fc2c`: run-list slots, routers,
//! cross-core channels and the latency tables the alignment reads.
//!
//! Per pair, items run in dependency order. Each item first gets its inputs
//! (lane input routers `0x87f99c`, links `0x87dc8c`), then its own slots: a
//! model (`0x87b904`), a 10003 head, split routers (`0x87ebc4`), merge routers
//! (`0x87ddd0`), FX loop routers (`0x87f780`) or looper routers (`0x87d958`).
//! Splitter and mixer models take no slot. Then each unmerged row gets its
//! output (`0x87e668`), or a core-1 forward when the row is empty.

use std::collections::BTreeMap;

use tetrad_catalog::{Specs, ids};

use crate::SplitterType;
use crate::fit::{Fit, Group, ItemInput, ItemNode, PATH_HEAD};
use crate::junction::splitter_type;
use crate::plan::{
    BlobLoad, Channel, CompileError, Deployed, Endpoint, HOP_LATENCY, Link, LinkKind, Node,
    Placement, ROUTERLESS_SIDECHAIN, RouterSet, RouterValue, RouterWrite, SLOTS_PER_CORE,
    SidechainPath, SidechainPlan, SidechainScene, SlotIndex, effective_input, per_scene,
};
use crate::routing::{RoutingGraph, SidechainSource};
use crate::{
    COLUMNS, Column, DspCore, InputPort, Layout, ModelId, OutputPort, Pair, ROWS, Row, SceneIndex,
    Scened, Site, Slot,
};

/// Fixed latency of an FX send leaving from core 1, and from another core
/// (`0x87f30c` returns 0x59 and 0x79).
const FX_SEND_LATENCY: [u32; 2] = [89, 121];
/// Sidechain receive return gain (`0x87d568`, `0x3fa609f8`).
const SIDECHAIN_RETURN_GAIN: f32 = 1.297_179_2;

/// Latency terms the builder records (`+0xa34`..`+0xbf4`).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LatencyTables {
    /// `+0xbbc`: hops before a row's first item.
    pub input: [u32; ROWS],
    /// `+0xbcc`: hops after a row's last item.
    pub output: [u32; ROWS],
    /// `+0xa34`: hops into a cell.
    pub hop: [[u32; COLUMNS]; ROWS],
    /// `+0xab4`: module latency of a cell (dropped when bypassed under DDC).
    pub module: [[u32; COLUMNS]; ROWS],
    /// `+0xbdc`: the merge's master-input hop plus a cross-core splitter input.
    pub merge: [u32; 2],
    /// `+0xbf4`: a cross-core splitter input.
    pub split: [u32; 2],
}

pub(crate) struct Built {
    pub slots: [Vec<Deployed>; 4],
    pub blobs: [Vec<BlobLoad>; 4],
    pub links: Vec<Link>,
    pub sidechains: Vec<SidechainPlan>,
    pub routers: RouterSet,
    pub blocks: BTreeMap<Slot, Placement>,
    pub latency: LatencyTables,
}

impl Built {
    pub fn model(&self, at: Placement) -> Option<u32> {
        self.slots[at.core.index()]
            .get(at.slot.index())
            .map(|slot| slot.model.0)
    }
}

pub(crate) fn build(
    layout: &Layout,
    specs: &Specs,
    graph: &RoutingGraph,
    fit: &Fit,
) -> Result<Built, CompileError> {
    let mut builder = Builder {
        layout,
        specs,
        slots: Default::default(),
        blobs: Default::default(),
        links: Vec::new(),
        dual: [[0; 2]; 2],
        slave_send: [0; 2],
        slave_receive: [0; 2],
        sidechain_counters: [0; 2],
        sidechains: BTreeMap::new(),
        routers: RouterSet::default(),
        blocks: BTreeMap::new(),
        latency: LatencyTables::default(),
    };
    for (group, cores) in fit.groups.iter().zip(&fit.cores) {
        builder.group(group, cores)?;
    }
    for (core, slots) in builder.slots.iter().enumerate() {
        if slots.len() > SLOTS_PER_CORE {
            return Err(CompileError::Slots {
                core: DspCore::ALL[core],
                count: slots.len(),
            });
        }
    }
    builder.check_channels()?;
    let sidechains = builder.sidechain_plans(graph);
    Ok(Built {
        slots: builder.slots,
        blobs: builder.blobs,
        links: builder.links,
        sidechains,
        routers: builder.routers,
        blocks: builder.blocks,
        latency: builder.latency,
    })
}

/// The `0x87e90c` nodes of one sidechain sink.
#[derive(Debug, Clone, Copy)]
struct SidechainNodes {
    channel: u8,
    slave_channel: Option<u32>,
    /// `+0x7b4`.
    router: Option<Placement>,
    /// `+0x6b4` on SOC2.
    source_router: Option<Placement>,
}

struct Builder<'a> {
    layout: &'a Layout,
    specs: &'a Specs,
    slots: [Vec<Deployed>; 4],
    blobs: [Vec<BlobLoad>; 4],
    links: Vec<Link>,
    /// Dual Core send/receive counters per SOC (`+0x58c`, `+0x594`).
    dual: [[u32; 2]; 2],
    /// SlaveLink send counters by sending SOC (`+0x56c`, `+0x574`).
    slave_send: [u32; 2],
    /// SlaveLink receive counters by receiving SOC (`+0x578`, `+0x570`).
    slave_receive: [u32; 2],
    /// Sidechain channels per SOC (`+0x59c`).
    sidechain_counters: [u8; 2],
    sidechains: BTreeMap<Slot, SidechainNodes>,
    routers: RouterSet,
    blocks: BTreeMap<Slot, Placement>,
    latency: LatencyTables,
}

fn int(index: u32, value: i64) -> (u32, RouterValue) {
    (index, RouterValue::Int(value as i32))
}

fn float(index: u32, value: f32) -> (u32, RouterValue) {
    (index, RouterValue::Float(value))
}

impl LinkKind {
    fn channel(self) -> Channel {
        match self {
            Self::DualCore => Channel::DualCore,
            Self::SlaveLink => Channel::SlaveLink,
        }
    }
}

impl Builder<'_> {
    fn group(&mut self, group: &Group, cores: &[DspCore]) -> Result<(), CompileError> {
        let order = dependency_order(group);
        let buffers = buffers(group, cores, &order);
        let pair = group.pair;
        for &index in &order {
            let item = &group.items[index];
            let core = cores[index];
            let (row, column) = item_cell(item.node, pair, self.layout);
            let own = buffers[index][0];
            let stats_model = item.stats_model.0;
            // Module latency (`0x87fc2c` head): the FIR engines add 32 and run
            // the block's own model; the core cab engines add nothing.
            let block_model = item
                .node
                .site()
                .and_then(|site| self.layout.instance(site))
                .map(|instance| instance.model);
            let (model, latency) = match stats_model {
                12_000 | 12_100 => (block_model.unwrap_or(item.stats_model), CAB_ENGINE),
                12_014 | 12_114 | 12_116 | ids::MIXER => (item.stats_model, 0),
                _ => (item.stats_model, item.stats.latency),
            };
            self.latency.module[row.index()][column] += latency;
            if stats_model == ids::MIXER {
                self.mixer(group, cores, &buffers, index, row);
                continue;
            }
            for input in &item.inputs {
                match *input {
                    ItemInput::Lane(lane) => {
                        let hop = self.lane_input(lane, core, own);
                        self.latency.input[lane.index()] += hop;
                    }
                    ItemInput::Item(source) => {
                        let source_item = &group.items[source];
                        // The split router serves the slave head (`0x87ebc4`).
                        let split_b = source_item.stats_model.0 == ids::SPLITTER
                            && buffers[source].len() > 1
                            && row.index() % 2 == 1;
                        if split_b || cores[source] == core {
                            continue;
                        }
                        let hop = self.link((cores[source], buffers[source][0]), (core, own));
                        if stats_model == ids::SPLITTER {
                            self.latency.input[row.index() + 1] += hop;
                            self.latency.merge[pair as usize] += hop;
                            self.latency.split[pair as usize] += hop;
                        } else {
                            self.latency.hop[row.index()][column] += hop;
                        }
                    }
                }
            }
            match stats_model {
                PATH_HEAD => {
                    let slave = row.index() % 2 == 1;
                    let at = self.model(core, Node::PathHead { pair, slave }, model, own);
                    // `0x5821a4`: heads run only for a crossover splitter.
                    let splitter = self.layout.pair(pair).splitter.as_ref();
                    let bypass = per_scene(|scene| {
                        splitter.and_then(|s| splitter_type(self.specs, s, scene))
                            != Some(SplitterType::Crossover)
                    });
                    let deployed = &mut self.slots[core.index()][at.slot.index()];
                    deployed.latency = latency;
                    deployed.bypass = bypass;
                    self.routers.heads[row.index()] = Some(at);
                }
                ids::SPLITTER => {
                    // The slave head is the splitter's second consumer.
                    let b = consumers(group, index)
                        .nth(1)
                        .expect("a split pair has a slave head");
                    let hop = self.split(pair, core, own, cores[b], buffers[b][0]);
                    self.latency.input[row.index() + 1] += hop;
                }
                13_000..=13_008 => {
                    let slot = item_slot(item.node);
                    let hop = self.fx_loop(stats_model, core, slot, own);
                    self.latency.hop[row.index()][column] += hop;
                    self.handle_values(slot);
                }
                27_000 | 27_002 => {
                    let slot = item_slot(item.node);
                    self.looper(stats_model, core, slot, own);
                    self.handle_values(slot);
                }
                _ => {
                    let slot = item_slot(item.node);
                    let sidechain = block_model
                        .and_then(|model| self.specs.model(model.0))
                        .is_some_and(|spec| spec.sidechain.is_some());
                    if sidechain {
                        self.sidechain(core, own, slot, stats_model)?;
                    }
                    let at = self.model(core, Node::Model(Site::Slot(slot)), model, own);
                    self.block_values(at, slot, latency);
                    self.blocks.insert(slot, at);
                }
            }
        }
        for output in &group.outputs {
            match *output {
                ItemInput::Lane(row) => self.lane_through(row),
                ItemInput::Item(index) => {
                    let (row, _) = item_cell(group.items[index].node, pair, self.layout);
                    self.lane_output(row, cores[index], buffers[index][0]);
                }
            }
        }
        Ok(())
    }

    fn push(&mut self, core: DspCore, node: Node, model: u32, layout: u16) -> Placement {
        let slots = &mut self.slots[core.index()];
        let placement = Placement {
            core,
            slot: SlotIndex(slots.len() as u8),
        };
        slots.push(Deployed {
            placement,
            node,
            model: ModelId(model),
            values: Vec::new(),
            bypass: Scened::Fixed(false),
            skipped: Vec::new(),
            texts: Vec::new(),
            latency: 0,
            layout,
            blob: None,
            setup: Vec::new(),
        });
        placement
    }

    fn router(
        &mut self,
        core: DspCore,
        node: Node,
        model: u32,
        layout: u16,
        writes: &[(u32, RouterValue)],
    ) -> Placement {
        let at = self.push(core, node, model, layout);
        self.slots[core.index()][at.slot.index()].setup = writes
            .iter()
            .map(|&(index, value)| RouterWrite { at, index, value })
            .collect();
        at
    }

    /// ZenLinkRx (`0x87d3b4`): return gain 1, source, stereo, and the thru
    /// gain 1 when `thru`.
    fn receive(
        &mut self,
        core: DspCore,
        endpoint: Endpoint,
        layout: u16,
        (channel, index): (Channel, u32),
        stereo: bool,
        thru: bool,
    ) -> Placement {
        let mut writes = vec![
            float(9, 1.0),
            int(0, channel as i64),
            int(1, index.into()),
            int(2, stereo.into()),
        ];
        if thru {
            writes.push(float(7, 1.0));
        }
        let node = Node::Receive(endpoint);
        self.router(core, node, ids::ZEN_LINK_RX, layout, &writes)
    }

    /// ZenLinkTx (`0x87d778`): thru and output gain 1, destination, stereo,
    /// send mode and the row meter when given.
    fn send(
        &mut self,
        core: DspCore,
        endpoint: Endpoint,
        layout: u16,
        (channel, index): (Channel, u32),
        send_mode: bool,
        meter: Option<Row>,
    ) -> Placement {
        let mut writes = vec![
            float(5, 1.0),
            float(3, 1.0),
            int(0, channel as i64),
            int(1, index.into()),
            int(2, 1),
            int(7, send_mode.into()),
        ];
        if let Some(row) = meter {
            writes.push(int(9, row.index() as i64 * 2));
        }
        let node = Node::Send(endpoint);
        self.router(core, node, ids::ZEN_LINK_TX, layout, &writes)
    }

    /// ZenLinkFwd on core 1 (`0x87e428`), layout word 0.
    fn forward(
        &mut self,
        endpoint: Endpoint,
        (from, from_index): (Channel, u32),
        (to, to_index): (Channel, u32),
        stereo: bool,
        send_mode: bool,
        meter: Option<Row>,
    ) -> Placement {
        let mut writes = vec![
            float(5, 1.0),
            int(0, from as i64),
            int(1, from_index.into()),
            int(3, to as i64),
            int(4, to_index.into()),
            int(2, stereo.into()),
            int(8, send_mode.into()),
            int(9, 0),
            int(12, 1),
        ];
        if let Some(row) = meter {
            writes.push(int(11, row.index() as i64 * 2));
        }
        let node = Node::Forward(endpoint);
        self.router(DspCore::IO, node, ids::ZEN_LINK_FWD, 0, &writes)
    }

    /// ZenLinkDelay (`0x87d260`).
    fn delay(&mut self, core: DspCore, endpoint: Endpoint, layout: u16, samples: u32) -> Placement {
        let writes = [int(0, samples.into())];
        self.router(
            core,
            Node::Delay(endpoint),
            ids::ZEN_LINK_DELAY,
            layout,
            &writes,
        )
    }

    /// A model slot (`0x87b904`). Same-name blobs share one load on a core
    /// when the model's stats allow code reuse.
    fn model(&mut self, core: DspCore, node: Node, model: ModelId, layout: u16) -> Placement {
        let spec = self.specs.model(model.0);
        let name = spec.and_then(|spec| spec.blob.clone());
        let reuse = spec
            .and_then(|spec| spec.stats)
            .is_some_and(|stats| stats.reuse_pmco);
        let blobs = &mut self.blobs[core.index()];
        let blob = match blobs.iter().position(|blob| blob.name == name) {
            Some(index) if reuse => index,
            _ => {
                blobs.push(BlobLoad { model, name });
                blobs.len() - 1
            }
        };
        let at = self.push(core, node, model.0, layout);
        self.slots[core.index()][at.slot.index()].blob = Some(blob);
        at
    }

    /// FX loop and looper blocks push their values to their handle router,
    /// where the stock drivers turn them into gains (`0x5174a8`).
    fn handle_values(&mut self, slot: Slot) {
        if let Some(&at) = self.blocks.get(&slot) {
            self.block_values(at, slot, 0);
        }
    }

    fn block_values(&mut self, at: Placement, slot: Slot, latency: u32) {
        let Some(instance) = self.layout.instance(Site::Slot(slot)) else {
            return;
        };
        let scenes: [Vec<f32>; crate::SCENES] =
            std::array::from_fn(|scene| instance.dsp_values(SceneIndex(scene as u8)));
        let values = (0..scenes[0].len())
            .map(|index| crate::layout::collapse(std::array::from_fn(|s| scenes[s][index])))
            .collect();
        let xml = instance.params.len() as u32;
        let mut skipped: Vec<u32> = self
            .specs
            .model(instance.model.0)
            .map(|spec| {
                spec.parameters
                    .iter()
                    .filter(|p| {
                        p.kind.is_meter() || p.kind == tetrad_catalog::ParameterType::String
                    })
                    .map(|p| p.index)
                    .collect()
            })
            .unwrap_or_default();
        skipped.extend([xml, xml + 1]);
        let deployed = &mut self.slots[at.core.index()][at.slot.index()];
        deployed.values = values;
        deployed.skipped = skipped;
        deployed.bypass = instance.bypass;
        deployed.texts = instance.texts.clone();
        deployed.latency = latency;
    }

    /// Advances the stock channel counters for a route `from` → `to`.
    fn channel(&mut self, from: DspCore, to: DspCore, width: u32) -> (LinkKind, u32, u32) {
        let (from_soc, to_soc) = (from.soc() as usize, to.soc() as usize);
        let advance = |counter: &mut u32| {
            let channel = *counter;
            *counter += width;
            channel
        };
        if from_soc == to_soc {
            let send = advance(&mut self.dual[from_soc][0]);
            let receive = advance(&mut self.dual[to_soc][1]);
            (LinkKind::DualCore, send, receive)
        } else {
            let send = advance(&mut self.slave_send[from_soc]);
            let receive = advance(&mut self.slave_receive[to_soc]);
            (LinkKind::SlaveLink, send, receive)
        }
    }

    fn record(&mut self, kind: LinkKind, from: Placement, to: Placement, channel: u32) {
        self.links.push(Link {
            kind,
            from,
            to,
            channel: channel as u8,
        });
    }

    /// `0x87dc8c`: a stereo send on the producer core, a receive on the
    /// consumer core.
    fn link(
        &mut self,
        (from, from_layout): (DspCore, u16),
        (to, to_layout): (DspCore, u16),
    ) -> u32 {
        let (kind, send, receive) = self.channel(from, to, 2);
        let id = self.links.len();
        let endpoint = Endpoint::Link(id);
        let tx = self.send(
            from,
            endpoint,
            from_layout,
            (kind.channel(), send),
            false,
            None,
        );
        let rx = self.receive(
            to,
            endpoint,
            to_layout,
            (kind.channel(), receive),
            true,
            false,
        );
        self.record(kind, tx, rx, send);
        HOP_LATENCY
    }

    /// `0x87f99c`: SOC1 cores receive the port directly; SOC2 cores through a
    /// core-1 forward and a SlaveLink (+32 samples).
    fn lane_input(&mut self, row: Row, core: DspCore, layout: u16) -> u32 {
        let route = Channel::input_route(effective_input(self.layout, row), row);
        let route = (route.0, route.1.into());
        let endpoint = Endpoint::LaneInput(row);
        if core.soc() == Pair::Upper {
            let at = self.receive(core, endpoint, layout, route, true, false);
            self.routers.lane_input[row.index()] = Some(at);
            return 0;
        }
        let (kind, send, receive) = self.channel(DspCore::IO, core, 2);
        let fwd = self.forward(endpoint, route, (kind.channel(), send), true, false, None);
        self.routers.lane_input[row.index()] = Some(fwd);
        let rx = self.receive(
            core,
            endpoint,
            layout,
            (kind.channel(), receive),
            true,
            false,
        );
        self.record(kind, fwd, rx, send);
        HOP_LATENCY
    }

    /// `0x87e668`: a core-1 row output is a delay and a send; another core
    /// sends to a core-1 forward (+32 samples on the row output).
    fn lane_output(&mut self, row: Row, core: DspCore, layout: u16) {
        let port = self.layout.lane(row).output;
        let (channel, index) = Channel::output_route(port);
        let route = (channel, u32::from(index));
        if core == DspCore::IO {
            let delay = self.delay(core, Endpoint::OutputDelay(row), layout, 0);
            self.routers.output_delay[row.index()] = Some(delay);
            let endpoint = Endpoint::LaneOutput(row);
            let tx = self.send(core, endpoint, layout, route, false, Some(row));
            self.routers.lane_output[row.index()] = Some(tx);
            return;
        }
        let (kind, send, receive) = self.channel(core, DspCore::IO, 2);
        let id = self.links.len();
        let tx = self.send(
            core,
            Endpoint::Link(id),
            layout,
            (kind.channel(), send),
            false,
            None,
        );
        self.latency.output[row.index()] += HOP_LATENCY;
        let from = (kind.channel(), receive);
        let fwd = self.forward(
            Endpoint::LaneOutput(row),
            from,
            route,
            true,
            false,
            Some(row),
        );
        self.record(kind, tx, fwd, send);
        self.routers.lane_output[row.index()] = Some(fwd);
        self.routers.output_delay[row.index()] = Some(fwd);
    }

    /// An empty row (`0x87fc2c` tail): one core-1 forward from its port to its
    /// port, which is also its input, output and output-delay router.
    fn lane_through(&mut self, row: Row) {
        let input = Channel::input_route(effective_input(self.layout, row), row);
        let output = Channel::output_route(self.layout.lane(row).output);
        let at = self.forward(
            Endpoint::LaneThrough(row),
            (input.0, input.1.into()),
            (output.0, output.1.into()),
            true,
            false,
            Some(row),
        );
        let routers = &mut self.routers;
        routers.lane_input[row.index()] = Some(at);
        routers.lane_output[row.index()] = Some(at);
        routers.output_delay[row.index()] = Some(at);
    }

    /// `0x87ebc4`: the split router copies the splitter's buffer to the slave
    /// head, locally or over a channel (+32 samples on the slave input).
    fn split(
        &mut self,
        pair: Pair,
        core: DspCore,
        layout: u16,
        b_core: DspCore,
        b_layout: u16,
    ) -> u32 {
        let endpoint = Endpoint::Split(pair);
        if core == b_core {
            let route = (Channel::Local, b_layout.into());
            let at = self.send(core, endpoint, layout, route, false, None);
            self.routers.split[pair as usize] = Some(at);
            return 0;
        }
        let (kind, send, receive) = self.channel(core, b_core, 2);
        let at = self.send(core, endpoint, layout, (kind.channel(), send), false, None);
        self.routers.split[pair as usize] = Some(at);
        let id = self.links.len();
        let rx = self.receive(
            b_core,
            Endpoint::Link(id),
            b_layout,
            (kind.channel(), receive),
            true,
            false,
        );
        self.record(kind, at, rx, send);
        HOP_LATENCY
    }

    /// The mixer branch of `0x87fc2c` and `0x87ddd0`: lane inputs of an empty
    /// side, the master-input route, both merge delays and the merge router.
    fn mixer(
        &mut self,
        group: &Group,
        cores: &[DspCore],
        buffers: &[Vec<u16>],
        index: usize,
        row: Row,
    ) {
        let pair = group.pair;
        let item = &group.items[index];
        let core = cores[index];
        let (own, b_own) = (buffers[index][0], buffers[index][1]);
        let side =
            |builder: &mut Self, input: ItemInput, lane_layout: u16, second: bool| match input {
                ItemInput::Lane(lane) => {
                    let hop = builder.lane_input(lane, core, lane_layout);
                    builder.latency.input[lane.index()] += hop;
                    (core, lane_layout)
                }
                ItemInput::Item(source) => {
                    let own = &buffers[source];
                    let buffer = if second && own.len() > 1 {
                        own[1]
                    } else {
                        own[0]
                    };
                    (cores[source], buffer)
                }
            };
        let (a_core, a_layout) = side(self, item.inputs[0], own, false);
        let (b_core, b_layout) = side(self, item.inputs[1], b_own, true);
        let mut hop = 0;
        if a_core == core {
            if a_layout != own {
                let route = (Channel::Local, own.into());
                self.send(core, Endpoint::Merge(pair), a_layout, route, true, None);
            }
        } else {
            let (kind, send, receive) = self.channel(a_core, core, 2);
            let id = self.links.len();
            let endpoint = Endpoint::Link(id);
            let tx = self.send(
                a_core,
                endpoint,
                a_layout,
                (kind.channel(), send),
                true,
                None,
            );
            let rx = self.receive(core, endpoint, own, (kind.channel(), receive), true, false);
            self.record(kind, tx, rx, send);
            hop = HOP_LATENCY;
        }
        let master_delay = Endpoint::MergeDelay { pair, slave: false };
        let slave_delay = Endpoint::MergeDelay { pair, slave: true };
        self.routers.merge_delay[pair as usize][0] = Some(self.delay(core, master_delay, own, 0));
        let merge = if b_core == core {
            self.routers.merge_delay[pair as usize][1] =
                Some(self.delay(core, slave_delay, b_own, 0));
            let route = (Channel::Local, b_own.into());
            self.receive(core, Endpoint::Merge(pair), own, route, true, true)
        } else {
            self.routers.merge_delay[pair as usize][1] =
                Some(self.delay(b_core, slave_delay, b_layout, 0));
            let (kind, send, receive) = self.channel(b_core, core, 2);
            let id = self.links.len();
            let route = (kind.channel(), send);
            let tx = self.send(b_core, Endpoint::Link(id), b_layout, route, false, None);
            let route = (kind.channel(), receive);
            let rx = self.receive(core, Endpoint::Merge(pair), own, route, true, true);
            self.record(kind, tx, rx, send);
            self.latency.hop[row.index() + 1][COLUMNS - 1] += HOP_LATENCY;
            rx
        };
        self.routers.merge[pair as usize] = Some(merge);
        self.latency.merge[pair as usize] += hop;
    }

    /// `0x87e90c`: a per-SOC sidechain channel; SOC1 sinks get a receive
    /// router, SOC2 sinks a core-1 source forward and a mono SlaveLink.
    fn sidechain(
        &mut self,
        core: DspCore,
        layout: u16,
        slot: Slot,
        model: u32,
    ) -> Result<(), CompileError> {
        let soc = core.soc();
        let counter = &mut self.sidechain_counters[soc as usize];
        if *counter >= 2 {
            return Err(CompileError::SidechainChannels(soc));
        }
        let channel = *counter;
        *counter += 1;
        let routerless = ROUTERLESS_SIDECHAIN.contains(&model);
        let (route, stereo, source, slave_channel) = if soc == Pair::Upper {
            ((Channel::Physical, 0), true, None, None)
        } else {
            let (kind, send, receive) = self.channel(DspCore::IO, core, 1);
            let source = self.forward(
                Endpoint::SidechainSource(slot),
                (Channel::Physical, 0),
                (kind.channel(), send),
                false,
                false,
                None,
            );
            (
                (kind.channel(), receive),
                false,
                Some(source),
                Some(receive),
            )
        };
        let router = (!routerless).then(|| {
            // `0x87d568`: the receive plus SidechainMode 1 and return gain 1.2972.
            let writes = [
                float(9, 1.0),
                int(0, route.0 as i64),
                int(1, route.1.into()),
                int(2, stereo.into()),
                float(7, 1.0),
                int(4, 1),
                float(9, SIDECHAIN_RETURN_GAIN),
            ];
            let node = Node::Receive(Endpoint::Sidechain(slot));
            self.router(core, node, ids::ZEN_LINK_RX, layout, &writes)
        });
        self.sidechains.insert(
            slot,
            SidechainNodes {
                channel,
                slave_channel,
                router,
                source_router: source,
            },
        );
        Ok(())
    }

    /// `0x87f780`: FX loop models are routers only. A lone send or return adds
    /// no cell latency; a loop adds its return latency.
    fn fx_loop(&mut self, model: u32, core: DspCore, slot: Slot, layout: u16) -> u32 {
        use crate::{InputPort as In, OutputPort as Out};
        let (send, ret) = match model {
            13_000 => (Some((0, Out::Send1)), None),
            13_001 => (Some((1, Out::Send2)), None),
            13_002 => (None, Some((0, In::Return1))),
            13_003 => (None, Some((1, In::Return2))),
            13_004 => (Some((0, Out::Send1)), Some((0, In::Return1))),
            13_005 => (Some((1, Out::Send2)), Some((1, In::Return2))),
            13_006 => (Some((0, Out::Send1And2)), None),
            13_007 => (None, Some((0, In::Return1And2))),
            _ => (Some((0, Out::Send1And2)), Some((0, In::Return1And2))),
        };
        let latency = send.map(|(index, port)| self.fx_send(core, slot, index, layout, port));
        let Some((index, port)) = ret else { return 0 };
        let returned = self.fx_return(core, slot, index, layout, port, latency.unwrap_or(0));
        if latency.is_some() { returned } else { 0 }
    }

    /// `0x87f30c`: the send leaves from core 1 directly, or over a channel to
    /// a core-1 forward. Returns the send latency.
    fn fx_send(
        &mut self,
        core: DspCore,
        slot: Slot,
        index: u8,
        layout: u16,
        port: OutputPort,
    ) -> u32 {
        let endpoint = Endpoint::FxSend { slot, index };
        let out = (Channel::Physical, port as u32);
        if core == DspCore::IO {
            let at = self.send(core, endpoint, layout, out, true, None);
            self.blocks.insert(slot, at);
            return FX_SEND_LATENCY[0];
        }
        let (kind, send, receive) = self.channel(core, DspCore::IO, 2);
        let tx = self.send(core, endpoint, layout, (kind.channel(), send), false, None);
        self.blocks.insert(slot, tx);
        let fwd = self.forward(endpoint, (kind.channel(), receive), out, true, true, None);
        self.record(kind, tx, fwd, send);
        FX_SEND_LATENCY[1]
    }

    /// `0x87ef3c`: a delay by the send latency and the return; SOC2 returns
    /// come through a core-1 forward (+32 samples).
    fn fx_return(
        &mut self,
        core: DspCore,
        slot: Slot,
        index: u8,
        layout: u16,
        port: InputPort,
        latency: u32,
    ) -> u32 {
        let stereo = port == InputPort::Return1And2;
        let endpoint = Endpoint::FxReturn { slot, index };
        let input = (Channel::Physical, port as u32);
        if core.soc() == Pair::Upper {
            self.delay(core, endpoint, layout, latency);
            let at = self.receive(core, endpoint, layout, input, stereo, true);
            self.blocks.insert(slot, at);
            return latency;
        }
        let (kind, send, receive) = self.channel(DspCore::IO, core, 1 + u32::from(stereo));
        let fwd = self.forward(endpoint, input, (kind.channel(), send), stereo, false, None);
        self.delay(core, endpoint, layout, latency + HOP_LATENCY);
        let route = (kind.channel(), receive);
        let at = self.receive(core, endpoint, layout, route, stereo, true);
        self.record(kind, fwd, at, send);
        self.blocks.insert(slot, at);
        latency + HOP_LATENCY
    }

    /// `0x87d958`: one (27000) or two (27002) looper send/receive pairs.
    fn looper(&mut self, model: u32, core: DspCore, slot: Slot, layout: u16) {
        let channels = if model == 27_000 { 1 } else { 2 };
        for index in 0..channels {
            let endpoint = Endpoint::Looper { slot, index };
            let route = (Channel::Looper, u32::from(index));
            let tx = self.send(core, endpoint, layout, route, false, None);
            self.blocks.insert(slot, tx);
            let rx = self.receive(core, endpoint, layout, route, true, true);
            self.blocks.insert(slot, rx);
        }
    }

    /// Plan assertion behind the fit's own count (`0x87a39c`): Dual Core
    /// channels SOC1 ≤ 24, SOC2 ≤ 12.
    fn check_channels(&self) -> Result<(), CompileError> {
        for (soc, limit) in [(Pair::Upper, 24), (Pair::Lower, 12)] {
            let count = self.dual[soc as usize][0] as usize;
            if count > limit {
                return Err(CompileError::Links {
                    kind: LinkKind::DualCore,
                    soc,
                    count,
                    limit,
                });
            }
        }
        Ok(())
    }

    /// Per sink: placements, router and the per-scene source route
    /// (`0x5843a4`): the build-time channel, +2 across SOCs.
    fn sidechain_plans(&self, graph: &RoutingGraph) -> Vec<SidechainPlan> {
        graph
            .sidechains
            .iter()
            .filter_map(|route| {
                let sink = *self.blocks.get(&route.sink)?;
                let nodes = *self.sidechains.get(&route.sink)?;
                let base = nodes.channel;
                let soc = route.sink.row.pair();
                let scenes = route.scenes.map(|source| {
                    let (route, channel) = match source {
                        SidechainSource::Block(slot) => {
                            let placement = self.blocks.get(&slot).copied();
                            let same_half = slot.row.pair() == soc;
                            let path =
                                if same_half && placement.is_some_and(|p| p.core == sink.core) {
                                    SidechainPath::SameCore
                                } else {
                                    SidechainPath::Buffer
                                };
                            let channel = if same_half { base } else { base + 2 };
                            (placement.map(|p| (p, path)), channel)
                        }
                        _ => (None, base),
                    };
                    SidechainScene {
                        source,
                        route,
                        channel,
                    }
                });
                Some(SidechainPlan {
                    sink,
                    site: route.sink,
                    router: nodes.router,
                    source_router: nodes.source_router,
                    channel: base,
                    slave_channel: nodes.slave_channel,
                    scenes,
                })
            })
            .collect()
    }
}

/// Module latency of the FIR cab engines 12000/12100 (`0x87fc2c`).
const CAB_ENGINE: u32 = crate::plan::CAB_LATENCY;

/// Per-item audio buffers on the item's core (fit `0x5fb108`): a lane input or
/// an input from another core takes the core's next buffer; an input on the
/// same core shares the producer's buffer (a splitter's second buffer for a
/// slave-row consumer); a splitter adds its B buffer. Buffers count from 1.
fn buffers(group: &Group, cores: &[DspCore], order: &[usize]) -> Vec<Vec<u16>> {
    let mut next = [0u16; 4];
    let mut buffers = vec![Vec::new(); group.items.len()];
    for &index in order {
        let item = &group.items[index];
        let core = cores[index].index();
        let slave_row = item_row(item.node, group.pair).index() % 2;
        for input in &item.inputs {
            let shared = match *input {
                ItemInput::Item(source) if cores[source].index() == core => {
                    let own: &Vec<u16> = &buffers[source];
                    Some(if own.len() < 2 {
                        own[0]
                    } else {
                        own[slave_row]
                    })
                }
                _ => None,
            };
            let buffer = shared.unwrap_or_else(|| {
                next[core] += 1;
                next[core]
            });
            buffers[index].push(buffer);
        }
        if item.stats_model.0 == ids::SPLITTER {
            next[core] += 1;
            buffers[index].push(next[core]);
        }
    }
    buffers
}

/// Items that take `index` as an input, in dependency order (`0x5fafa0`).
fn consumers(group: &Group, index: usize) -> impl Iterator<Item = usize> + '_ {
    dependency_order(group)
        .into_iter()
        .filter(move |&consumer| {
            group.items[consumer]
                .inputs
                .contains(&ItemInput::Item(index))
        })
}

fn item_row(node: ItemNode, pair: Pair) -> Row {
    match node {
        ItemNode::Block(slot) => slot.row,
        ItemNode::PathHead { slave: true, .. } => pair.slave(),
        _ => pair.master(),
    }
}

/// The stock item's row and column bytes (`0x5ed844`): junctions and the
/// master head sit at their junction column, the slave head at column 0.
fn item_cell(node: ItemNode, pair: Pair, layout: &Layout) -> (Row, usize) {
    let junction = |column: Option<Column>| column.map_or(0, Column::index);
    let pair_layout = layout.pair(pair);
    match node {
        ItemNode::Block(slot) => (slot.row, slot.column.index()),
        ItemNode::PathHead { slave: true, .. } => (pair.slave(), 0),
        ItemNode::PathHead { .. } | ItemNode::Splitter(_) => {
            (pair.master(), junction(pair_layout.split))
        }
        ItemNode::Mixer(_) => (pair.master(), junction(pair_layout.merge)),
    }
}

fn item_slot(node: ItemNode) -> Slot {
    match node {
        ItemNode::Block(slot) => slot,
        _ => unreachable!("only grid blocks reach the model dispatch"),
    }
}

/// Item indices with each item's inputs placed first (`0x5fd284`): the mixer and
/// the master tail move after the slave row they wait for.
pub(crate) fn dependency_order(group: &Group) -> Vec<usize> {
    fn visit(group: &Group, index: usize, done: &mut Vec<bool>, out: &mut Vec<usize>) {
        if done[index] {
            return;
        }
        done[index] = true;
        for input in &group.items[index].inputs {
            if let ItemInput::Item(source) = *input {
                visit(group, source, done, out);
            }
        }
        out.push(index);
    }
    let mut done = vec![false; group.items.len()];
    let mut out = Vec::with_capacity(group.items.len());
    for index in 0..group.items.len() {
        visit(group, index, &mut done, &mut out);
    }
    out
}

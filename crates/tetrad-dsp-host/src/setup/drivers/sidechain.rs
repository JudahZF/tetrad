//! Sidechain route step (VA `0x585f04`), the tail of the lane I/O push (`0x5205e8`): run it right
//! after [`super::lane_io::LaneIo::push`], inside the same bulk window. For every grid block with a
//! sidechain source it runs the stock route (`0x5843a4`, mode 0) in row-major order.
//!
//! Router ints (`0x511318`) and the route requests `0x48`/`0x49` (`0x5156b8`)
//! go out at once; router floats go through the bulk queue (`0x517010`).

use std::time::Duration;

use tetrad_dsp_protocol::requests::Request;
use tetrad_dsp_protocol::{DeploymentTarget, DspCore};

use super::super::DspWrite;
use super::lane_io::{Lane, LaneRouter, RECEIVE_ROUTER, SEND_RECEIVE_ROUTER};

/// Return gain (router 9) of a sink fed by an input, not a block (literal
/// `0x582d04`).
const INPUT_RETURN_GAIN: f32 = 1.297_17;
/// Route level (router 8) of a sink without a `sidechain_in` parameter.
const DEFAULT_LEVEL: f32 = 0.5;
/// `usleep(25000)` after a sink router is silenced outside bulk mode.
const SILENCE_SETTLE: Duration = Duration::from_millis(25);
/// Lane input id of the sidechain buffer (block sources, `0x588bac`).
const SIDECHAIN_INPUT: u32 = 14;

/// One grid cell: row 0..4, column 0..8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GridCell {
    row: u8,
    column: u8,
}

impl GridCell {
    pub fn new(row: u8, column: u8) -> Option<Self> {
        (row < 4 && column < 8).then_some(Self { row, column })
    }

    pub fn row(self) -> u8 {
        self.row
    }

    pub fn column(self) -> u8 {
        self.column
    }

    /// SOC1 holds rows 0 and 1 (`0x5843a4`: block steps below 30).
    fn soc1(self) -> bool {
        self.row < 2
    }
}

/// The decoded sidechain source parameter of a sink (`0x585b2c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidechainSource {
    /// Step 0.
    Off,
    /// Step 1: the input of the lane the sink's signal comes from (`0x2e186c`).
    FollowInput,
    /// Steps 2..=13: a lane input id (table `0x11df000`: 1..=6, 8..=13).
    Input(u32),
    /// Steps 14..=45: a grid block, with its placement when the compiler
    /// placed it (map `+0xe3c`, VA `0x879ff0`).
    Block {
        cell: GridCell,
        placement: Option<DeploymentTarget>,
    },
}

/// One sidechain sink: a grid block whose model has a sidechain source
/// parameter (model `+0xe8 == 1`) and whose compiler nodes are placed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SidechainSink {
    pub cell: GridCell,
    /// The sink block's own placement (VA `0x879ff0`).
    pub placement: DeploymentTarget,
    /// Compiler `+0x6b4` (VA `0x87a2ac`): on SOC1 the same node as
    /// `router`; on SOC2 the core-1 source forward (22002).
    pub source_router: LaneRouter,
    /// Compiler `+0x7b4` (VA `0x87a2d0`): the sink's receive router (22000),
    /// or the model itself for 16008, 16013, 18012 and 30001.
    pub router: LaneRouter,
    /// Compiler `+0x934`: the sink's per-SOC sidechain channel.
    pub channel: u32,
    /// Compiler `+0x8b4`: the SlaveLink channel of a SOC2 sink (0 on SOC1).
    pub slave_channel: u32,
    pub source: SidechainSource,
    /// The `sidechain_in` parameter (model `+0xf0`) at the current scene;
    /// `None` when the model has none.
    pub level: Option<f32>,
    /// `0x2ecd4c`: the block's forced flag (`+0xfc`) or BYPASS above 0.5 at
    /// the current scene.
    pub bypassed: bool,
}

/// One pair's junction columns (preset pair map `+0x14e4`: `+0x14` split,
/// `+0x18` merge; stock −1 = none).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PairJunction {
    pub split: Option<u8>,
    pub merge: Option<u8>,
}

/// Preset and controller state the route reads.
#[derive(Debug, Clone, Copy)]
pub struct SidechainContext<'a> {
    /// The lanes as for [`super::lane_io::LaneIo::push`]; only `input` and
    /// `output` are read (Follow Input).
    pub lanes: &'a [Lane; 4],
    /// Upper pair (rows 0, 1) and lower pair (rows 2, 3).
    pub pairs: [PairJunction; 2],
    /// Lane object `+0x342c` while `+0x3430` is set (`0x5860a4`, from
    /// controller `+0x88dc`/`+0x88e0`): the tuner's input port while the
    /// tuner mutes. A sink fed from an overlapping input is cut.
    pub tuner_input: Option<u32>,
    /// Controller `+0xb8a8`: bulk mode is on (every push and recall).
    pub bulk: bool,
}

/// The parts of the route `0x5843a4` runs (argument 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteMode {
    /// 0: source then sink router. The push (`0x585f04`) uses it.
    Full,
    /// 1: source records, route requests and the source router only. The
    /// first call of a source edit (VA `0x51e4d8`) uses it: the sidechain
    /// flag `0x2ea600` is 1 on that path (`0x51e7ac`) and reaches argument 5
    /// unchanged (`0x51f85c`).
    Source,
    /// 2: the sink router only. A `sidechain_in` edit (VA `0x51ee94`) and
    /// the second call of a source edit (VA `0x51f904`) use it.
    Sink,
}

/// Router channel kind (router parameter 0) of a lane input (`0x588c44`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum ChannelKind {
    Unrouted = 0,
    Physical = 1,
    SlaveLink = 2,
    NextRow = 5,
    Usb = 6,
    UsbInput = 7,
    Sidechain = 12,
}

impl ChannelKind {
    fn of_input(input: u32) -> Self {
        match input {
            1..=6 => Self::Physical,
            7 => Self::NextRow,
            16 => Self::Usb,
            8..=13 => Self::UsbInput,
            SIDECHAIN_INPUT => Self::Sidechain,
            _ => Self::Unrouted,
        }
    }
}

/// The sidechain route records (DSP controller `+0xee64`, `[4][8]` of
/// `{u8 block, i32 row, i32 column}`, VA `0x52dfb0`). Keep one per DSP
/// session, next to the [`super::lane_io::LaneIo`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sidechains {
    sources: [[Option<GridCell>; 8]; 4],
}

impl Sidechains {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stock `0x585f04`: every sink in row-major order with [`RouteMode::Full`].
    pub fn push(
        &mut self,
        sinks: &[SidechainSink],
        context: &SidechainContext<'_>,
        out: &mut Vec<DspWrite>,
    ) {
        let mut order: Vec<_> = sinks.iter().collect();
        order.sort_by_key(|sink| sink.cell);
        for sink in order {
            self.route(sink, RouteMode::Full, context, out);
        }
    }

    /// Stock `0x5843a4` for one sink.
    pub fn route(
        &mut self,
        sink: &SidechainSink,
        mode: RouteMode,
        context: &SidechainContext<'_>,
        out: &mut Vec<DspWrite>,
    ) {
        let cell = sink.cell;
        let input = match sink.source {
            SidechainSource::Off => 0,
            SidechainSource::FollowInput => {
                context.lanes[followed_lane(context, cell.row, cell.column)].input
            }
            SidechainSource::Input(input) => input,
            SidechainSource::Block { .. } => SIDECHAIN_INPUT,
        };
        let kind = ChannelKind::of_input(input);
        let block = kind == ChannelKind::Sidechain;
        let source = match sink.source {
            SidechainSource::Block { cell, placement } => Some((cell, placement)),
            _ => None,
        };
        // `0x588b5c`: inputs name their port; a fed lane its row − 2.
        let mut channel = match kind {
            ChannelKind::Sidechain => sink.channel,
            ChannelKind::Physical | ChannelKind::UsbInput => input,
            ChannelKind::NextRow => u32::from(cell.row).wrapping_sub(2),
            _ => 0,
        };
        let same_soc = source.is_some_and(|(from, _)| from.soc1() == cell.soc1());
        let level = sink.level.unwrap_or(DEFAULT_LEVEL);
        let cut = context
            .tuner_input
            .is_some_and(|port| tuner_overlaps(port, input));
        let router = |kind, channel, input_fed| RouterSetting {
            kind,
            channel,
            input_fed,
            level,
            cut,
        };
        if matches!(mode, RouteMode::Full | RouteMode::Source) {
            if block {
                if !same_soc {
                    channel += 2;
                }
                // Owned: a Follow Input lane fed by the sidechain buffer has
                // no source cell; stock reads a garbage cell there.
                if let Some((from, placement)) = source {
                    let old = self.sources[cell.row as usize][cell.column as usize].replace(from);
                    if old.is_some_and(|old| old != from) && !context.bulk {
                        router(ChannelKind::Unrouted, 0, false).write(sink.router, out);
                        out.push(DspWrite::SyncAllCores {
                            settle: SILENCE_SETTLE,
                        });
                    }
                    if let Some(placement) = placement {
                        // `0x52e948`: same SOC and core as the sink, or the
                        // source router's core.
                        let same_core = (same_soc && placement.core == sink.placement.core)
                            || sink.source_router.target.core == placement.core;
                        route(placement, channel, same_core, out);
                    }
                }
            } else {
                self.sources[cell.row as usize][cell.column as usize] = None;
            }
            let forward = sink.source_router;
            if matches!(forward.model_id, RECEIVE_ROUTER | SEND_RECEIVE_ROUTER) {
                int(out, forward.target, 0, kind as u32);
                int(out, forward.target, 1, channel);
            }
        }
        if matches!(mode, RouteMode::Full | RouteMode::Sink) {
            let setting = if !same_soc && sink.router.target.core as u8 > 1 {
                router(ChannelKind::SlaveLink, sink.slave_channel, !block)
            } else {
                router(kind, channel, !block)
            };
            setting.write(sink.router, out);
            if sink.router.model_id == RECEIVE_ROUTER {
                int(out, sink.router.target, 5, u32::from(!sink.bypassed));
            }
        }
    }
}

/// The arguments of `0x582bd4`, which sets a sink router.
struct RouterSetting {
    kind: ChannelKind,
    channel: u32,
    input_fed: bool,
    level: f32,
    cut: bool,
}

impl RouterSetting {
    /// `0x03` 2 = stereo (SOC1 cores and routed), 0 = kind, 1 = channel
    /// (both 0 when cut); a receive router then gets `0x02` 7 = 1.0,
    /// 9 = return gain, 8 = level.
    fn write(&self, router: LaneRouter, out: &mut Vec<DspWrite>) {
        let target = router.target;
        let stereo = (target.core as u8) < 2 && self.kind != ChannelKind::Unrouted;
        let (kind, channel) = if self.cut {
            (ChannelKind::Unrouted, 0)
        } else {
            (self.kind, self.channel)
        };
        int(out, target, 2, u32::from(stereo));
        int(out, target, 0, kind as u32);
        int(out, target, 1, channel);
        if router.model_id != RECEIVE_ROUTER {
            return;
        }
        let gain = if self.input_fed {
            INPUT_RETURN_GAIN
        } else {
            1.0
        };
        out.push(DspWrite::parameter(target, 7, 1.0));
        out.push(DspWrite::parameter(target, 9, gain));
        out.push(DspWrite::parameter(target, 8, self.level));
    }
}

/// `0x5156b8`: `0x48`, or `0x49` for a same-core source, to cores 0..=3.
fn route(source: DeploymentTarget, channel: u32, same_core: bool, out: &mut Vec<DspWrite>) {
    for core in [
        DspCore::Core0,
        DspCore::Core1,
        DspCore::Core2,
        DspCore::Core3,
    ] {
        out.push(DspWrite::Request(Request::RouteToSidechainBuffer {
            core,
            same_core,
            // Stock passes the channel as a byte.
            channel: channel as u8,
            source_slot: u32::from(source.slot.index()),
            source_core: source.core as u32,
        }));
    }
}

fn int(out: &mut Vec<DspWrite>, target: DeploymentTarget, index: u32, value: u32) {
    out.push(DspWrite::IntParameter {
        target,
        index,
        value,
    });
}

/// `0x582a90`: the tuner's input port overlaps a lane input (a stereo pair
/// holds both of its mono ports, and each mono port its pair).
pub(super) fn tuner_overlaps(port: u32, input: u32) -> bool {
    match port {
        1 | 2 => input == port || input == 3,
        3 => matches!(input, 1..=3),
        4 | 5 => input == port || input == 6,
        6 => matches!(input, 4..=6),
        8 | 9 => input == port || input == 12,
        12 => matches!(input, 8 | 9 | 12),
        _ => input == port,
    }
}

/// `0x2e186c`: the lane whose input a Follow Input sink at (`row`,
/// `column`) follows. Rows 2 and 3 fed by an upper lane's `NEXT_ROW`
/// output follow that lane from column 7.
fn followed_lane(context: &SidechainContext<'_>, mut row: u8, mut column: u8) -> usize {
    let lanes = context.lanes;
    let after_merge = |pair: PairJunction, column: u8| pair.merge.is_some_and(|m| m < column);
    loop {
        let pair = context.pairs[usize::from(row / 2)];
        let lane = match row {
            0 => return usize::from(after_merge(pair, column) && lanes[0].input == 0),
            1 => return usize::from(pair.split.is_none()),
            2 if !(after_merge(pair, column) && lanes[2].input == 0) => 2,
            _ if pair.split.is_some() => 2,
            _ => 3,
        };
        let feed = if lane == 2 { 16 } else { 17 };
        let feeds = |output: u32| output == feed || output == 18;
        row = match (feeds(lanes[0].output), feeds(lanes[1].output)) {
            (true, _) => 0,
            (_, true) => 1,
            _ => return lane,
        };
        column = 7;
    }
}

#[cfg(test)]
mod tests;

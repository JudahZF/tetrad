//! Latency alignment: ports of `0x87c11c` (split/merge) and `0x87c958` (row
//! outputs and lower-row inputs) over the builder's latency tables.
//!
//! A cell's latency is its hops plus its module latency, which DDC drops for a
//! bypassed block (`0x87a0f0`). Sidechain compensation (`0x87b3cc`, the
//! `+0x9b4` term and the sink delays) is not ported: it is 0 here.

use tetrad_catalog::ids;

use crate::plan::{
    HOP_LATENCY, PairDelay, Placement, RouterSet, RouterValue, RouterWrite, fed_rows,
};
use crate::routers::Built;
use crate::{
    COLUMNS, Column, DspCore, Layout, OutputPort, Pair, ROWS, SCENES, SceneIndex, Site, Slot,
};

pub(crate) struct Alignment {
    pub pairs: [[PairDelay; SCENES]; 2],
    /// Row totals (`+0x1b0c`) per scene.
    pub lanes: [[u32; SCENES]; ROWS],
    pub writes: [Vec<RouterWrite>; SCENES],
}

/// `0x87bf9c`: the delay parameter of a router model (3 on 22000 and the
/// routerless sidechain models, 0 on 22003, 9 on 22002).
pub(crate) fn delay_index(model: u32) -> Option<u32> {
    match model {
        ids::ZEN_LINK_RX | 16_008 | 16_013 | 18_012 | 30_001 => Some(3),
        ids::ZEN_LINK_DELAY => Some(0),
        ids::ZEN_LINK_FWD => Some(9),
        _ => None,
    }
}

pub(crate) fn align(layout: &Layout, built: &Built, gate: Option<DspCore>, ddc: bool) -> Alignment {
    let mut alignment = Alignment {
        pairs: [[PairDelay::default(); SCENES]; 2],
        lanes: [[0; SCENES]; ROWS],
        writes: Default::default(),
    };
    for scene in 0..SCENES {
        let scene_index = SceneIndex::new(scene as u8).expect("scene in range");
        let mut run = Run {
            layout,
            built,
            routers: &built.routers,
            writes: Vec::new(),
        };
        let cells = run.cells(scene_index, ddc);
        let (pairs, totals) = run.pairs(&cells, gate);
        run.outputs(&totals);
        alignment.writes[scene] = run.writes;
        for (pair, delay) in pairs.into_iter().enumerate() {
            alignment.pairs[pair][scene] = delay;
        }
        for (row, total) in totals.iter().enumerate() {
            alignment.lanes[row][scene] = *total;
        }
    }
    alignment
}

struct Run<'a> {
    layout: &'a Layout,
    built: &'a Built,
    routers: &'a RouterSet,
    writes: Vec<RouterWrite>,
}

impl Run<'_> {
    /// `0x87a0f0` per cell, with `0x879e08`'s bypass flags (global cab/IR
    /// bypass not modelled).
    fn cells(&self, scene: SceneIndex, ddc: bool) -> [[u32; COLUMNS]; ROWS] {
        let table = &self.built.latency;
        std::array::from_fn(|row| {
            std::array::from_fn(|column| {
                let slot = Slot {
                    row: crate::Row::new(row as u8).expect("row in range"),
                    column: Column::new(column as u8).expect("column in range"),
                };
                let bypassed = self
                    .layout
                    .instance(Site::Slot(slot))
                    .is_some_and(|instance| instance.bypass.at(scene));
                let module = if ddc && bypassed {
                    0
                } else {
                    table.module[row][column]
                };
                table.hop[row][column] + module
            })
        })
    }

    fn delay(&mut self, at: Option<Placement>, samples: u32) {
        let Some(at) = at else { return };
        let Some(index) = self.built.model(at).and_then(delay_index) else {
            return;
        };
        self.writes.push(RouterWrite {
            at,
            index,
            value: RouterValue::Int(samples as i32),
        });
    }

    /// `0x87c11c`: per pair, the gate hops on lane inputs, the merge delays and
    /// the row totals. Returns the pair delays and the totals.
    fn pairs(
        &mut self,
        cells: &[[u32; COLUMNS]; ROWS],
        gate: Option<DspCore>,
    ) -> ([PairDelay; 2], [u32; ROWS]) {
        let table = &self.built.latency;
        let fed = fed_rows(self.layout);
        let mut input = table.input;
        let mut delays = [PairDelay::default(); 2];
        let mut totals = [0u32; ROWS];
        let mut cumulative = [[0u32; COLUMNS]; ROWS];
        for pair in Pair::ALL {
            let p = pair as usize;
            let (m, s) = (pair.master().index(), pair.slave().index());
            let junctions = self.layout.pair(pair);
            let split = junctions.split.map(Column::index);
            let merge = junctions.merge.map(Column::index);
            // A lane input that does not pass the gate core is one hop later.
            let off_gate = |core: Option<DspCore>| {
                gate.is_some_and(|gate| core.is_some_and(|core| core != gate))
            };
            let lane_core = |row: usize| self.routers.lane_input[row].map(|at| at.core);
            let (master_core, slave_core) = match pair {
                Pair::Upper => (lane_core(m), lane_core(s)),
                Pair::Lower => (
                    (!fed[m]).then_some(DspCore::IO),
                    (!fed[s]).then_some(DspCore::IO),
                ),
            };
            if off_gate(master_core) {
                input[m] += HOP_LATENCY;
            }
            if split.is_none() && off_gate(slave_core) {
                input[s] += HOP_LATENCY;
            }
            let mut align = [0u32; 2];
            let merge_term = table.merge[p];
            let master_until = |end: usize| (0..=end).map(|c| cells[m][c]).sum::<u32>();
            let slave_row = input[s] + cells[s].iter().sum::<u32>();
            // The master path into the mixer: from the row start (merge only) or
            // from the split column, plus the merge hop term (`+0x2f7`).
            let merged = match (split, merge) {
                (None, Some(merge)) => Some(merge_term + input[m] + master_until(merge)),
                (Some(split), Some(merge)) => {
                    Some(merge_term + (split..=merge).map(|c| cells[m][c]).sum::<u32>())
                }
                (Some(split), None) => {
                    align[1] = input[m] + (0..split).map(|c| cells[m][c]).sum::<u32>();
                    None
                }
                (None, None) => None,
            };
            if let Some(master) = merged {
                let [master_delay, slave_delay] = self.routers.merge_delay[p];
                // A tie delays the master by 0.
                if master <= slave_row {
                    let diff = slave_row - master;
                    self.delay(master_delay, diff);
                    self.delay(slave_delay, 0);
                    align[0] = diff;
                    delays[p].master = diff;
                } else {
                    let diff = master - slave_row;
                    self.delay(slave_delay, diff);
                    self.delay(master_delay, 0);
                    align[1] = diff;
                    delays[p].slave = diff;
                }
            }
            for (side, row) in [m, s].into_iter().enumerate() {
                let mut total = align[side] + table.output[row] + input[row];
                let mut running = input[row];
                if side == 0 {
                    total += merge_term;
                } else if let Some(split) = split {
                    running += if split == 0 {
                        input[m]
                    } else {
                        cumulative[m][split - 1]
                    };
                }
                for column in 0..COLUMNS {
                    running += cells[row][column];
                    total += cells[row][column];
                    if side == 0 {
                        if merge.is_some_and(|merge| merge + 1 == column) {
                            running += align[0] + merge_term;
                        }
                        if split == Some(column) {
                            running += table.split[p];
                        }
                    }
                    cumulative[row][column] = running;
                }
                totals[row] = total;
            }
        }
        (delays, totals)
    }

    /// `0x87c958` without its sidechain part: output delays align every row
    /// output; when an upper row feeds a lower row, lower inputs that are not
    /// fed wait for the upper pair.
    fn outputs(&mut self, totals: &[u32; ROWS]) {
        let layout = self.layout;
        let routers = self.routers;
        let next_row = |row: usize| {
            matches!(
                layout.lanes[row].output,
                OutputPort::NextRow3 | OutputPort::NextRow4 | OutputPort::NextRow3And4
            )
        };
        let feeds = [next_row(0), next_row(1)];
        let merges = Pair::ALL.map(|pair| layout.pair(pair).merge.is_some());
        let fed = fed_rows(layout);
        let output = |row: usize| routers.output_delay[row];
        let input = |row: usize| routers.lane_input[row];
        if !feeds.contains(&true) {
            let max = totals.iter().copied().max().unwrap_or(0);
            for row in 0..ROWS {
                if row % 2 == 0 || !merges[row / 2] {
                    self.delay(output(row), max - totals[row]);
                }
            }
            for row in [2, 3] {
                if input(row) != output(row) {
                    self.delay(input(row), 0);
                }
            }
            return;
        }
        let side_max = |pair: usize| {
            let (m, s) = (pair * 2, pair * 2 + 1);
            let max = if merges[pair] {
                totals[m]
            } else {
                totals[m].max(totals[s])
            };
            let delays = if merges[pair] {
                [0, 0]
            } else {
                [max - totals[m], max - totals[s]]
            };
            (max, delays)
        };
        let (upper, mut upper_delays) = side_max(0);
        let (lower, lower_delays) = side_max(1);
        let all_feed = feeds[0] && feeds[1];
        let mut through = [false; 2];
        for (index, row) in [2, 3].into_iter().enumerate() {
            let upper_row = index;
            if !all_feed && (index == 0 || !merges[0]) && !feeds[upper_row] {
                upper_delays[upper_row] += lower;
            }
            if fed[row] {
                self.delay(input(row), 0);
            } else {
                through[index] = input(row) == output(row);
                let extra = if through[index] {
                    lower_delays[index]
                } else {
                    0
                };
                self.delay(input(row), upper + extra);
            }
        }
        for (row, delay) in upper_delays.into_iter().enumerate() {
            self.delay(output(row), delay);
        }
        for (index, row) in [2, 3].into_iter().enumerate() {
            if !through[index] {
                self.delay(output(row), lower_delays[index]);
            }
        }
    }
}

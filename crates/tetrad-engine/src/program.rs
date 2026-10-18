use crate::{
    Block, BlockKind, COLUMNS, Column, Detached, Error, GainDb, Grid, LaneGains, ModelId, Pair,
    PortId, ROWS, Row, SceneIndex, Scened, Slot,
};
use std::collections::BTreeMap;

/// One scheduled operation. Lane buffers are indexed by grid row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    /// Fills a lane from an input port, or with silence when `port` is `None`.
    Load {
        row: Row,
        port: Option<PortId>,
    },
    /// Copies the master lane to the slave lane, then applies the lane gains.
    Split {
        pair: Pair,
        gains: Scened<LaneGains>,
    },
    Process {
        slot: Slot,
        block: Block,
    },
    /// Sums the gained slave lane into the gained master lane.
    Merge {
        pair: Pair,
        gains: Scened<LaneGains>,
    },
    /// Adds a lane to an output port; lanes sharing a port are summed.
    Store {
        row: Row,
        port: PortId,
    },
}

/// The scene-dependent value of one step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepValue {
    Block {
        bypassed: bool,
        gain: Option<GainDb>,
    },
    Junction(LaneGains),
}

impl Step {
    pub fn value(&self, scene: SceneIndex) -> Option<StepValue> {
        match self {
            Self::Load { .. } | Self::Store { .. } => None,
            Self::Split { gains, .. } | Self::Merge { gains, .. } => {
                Some(StepValue::Junction(gains.at(scene)))
            }
            Self::Process { block, .. } => Some(StepValue::Block {
                bypassed: block.bypass.at(scene),
                gain: match block.kind {
                    BlockKind::Gain(gain) => Some(gain.at(scene)),
                    BlockKind::Model(_) => None,
                },
            }),
        }
    }
}

/// A scene transition entry: step index and its value in the target scene.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Change {
    pub step: usize,
    pub value: StepValue,
}

/// How the reference renderer treats opaque catalog models that are not bypassed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpaquePolicy {
    Reject,
    /// Treats opaque models as identity. For demos only; this is not model audio.
    PassThrough,
}

/// Validated, deterministically ordered schedule. Topology is fixed at compile time;
/// scenes only change step values, so a scene change never recompiles.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    steps: Vec<Step>,
    detached: Vec<Detached>,
}

impl Grid {
    /// Validates routing and produces the schedule.
    ///
    /// Per pair: load the master lane (and the slave lane when nothing splits into
    /// it), process master blocks through the merge column with the split before its
    /// column, process every slave block in column order, merge, process the
    /// remaining master blocks, then store. Upper pair before lower pair. Slave
    /// columns are not bounded by the split/mix columns: the stock spec builder
    /// (`0x5ed844`) walks slave columns 0..7 after the split.
    pub fn compile(&self) -> Result<Program, Error> {
        let mut steps = Vec::new();
        for pair in Pair::ALL {
            self.schedule_pair(pair, &mut steps)?;
        }
        Ok(Program {
            steps,
            detached: self.detached.clone(),
        })
    }

    fn schedule_pair(&self, pair: Pair, steps: &mut Vec<Step>) -> Result<(), Error> {
        let route = self.route(pair);
        let (master, slave) = (pair.master(), pair.slave());
        let split = route.split.map(|junction| junction.column.index());
        let merge = route.merge.map(|junction| junction.column.index());
        if let (Some(split), Some(merge)) = (split, merge)
            && split > merge
        {
            return Err(Error::SplitAfterMerge { pair, split, merge });
        }
        let (master_ports, slave_ports) = (self.ports(master), self.ports(slave));
        if split.is_some() && slave_ports.input.is_some() {
            return Err(Error::SlaveInputShadowed(pair));
        }
        if merge.is_some() && slave_ports.output.is_some() {
            return Err(Error::SlaveOutputShadowed(pair));
        }

        steps.push(Step::Load {
            row: master,
            port: master_ports.input,
        });
        if split.is_none() {
            steps.push(Step::Load {
                row: slave,
                port: slave_ports.input,
            });
        }
        let master_end = merge.map_or(COLUMNS, |merge| merge + 1);
        for column in 0..master_end {
            if let Some(junction) = route.split.filter(|_| split == Some(column)) {
                steps.push(Step::Split {
                    pair,
                    gains: junction.gains,
                });
            }
            self.push_block(master, column, steps);
        }
        for column in 0..COLUMNS {
            self.push_block(slave, column, steps);
        }
        if let Some(junction) = route.merge {
            steps.push(Step::Merge {
                pair,
                gains: junction.gains,
            });
        }
        for column in master_end..COLUMNS {
            self.push_block(master, column, steps);
        }
        if let Some(port) = master_ports.output {
            steps.push(Step::Store { row: master, port });
        }
        if let Some(port) = slave_ports.output {
            steps.push(Step::Store { row: slave, port });
        }
        Ok(())
    }

    fn push_block(&self, row: Row, column: usize, steps: &mut Vec<Step>) {
        if let Some(block) = self.blocks[row.index()][column] {
            steps.push(Step::Process {
                slot: slot(row, column),
                block,
            });
        }
    }
}

fn slot(row: Row, column: usize) -> Slot {
    Slot {
        row,
        column: Column(column as u8),
    }
}

impl Program {
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Models outside the schedule; `render` with `OpaquePolicy::Reject` refuses them.
    pub fn detached(&self) -> &[Detached] {
        &self.detached
    }

    /// Value updates needed to move from one scene to another, in schedule order.
    /// Steps whose value is unchanged are omitted.
    pub fn transition(&self, from: SceneIndex, to: SceneIndex) -> Vec<Change> {
        self.steps
            .iter()
            .enumerate()
            .filter_map(|(step, entry)| {
                let value = entry.value(to)?;
                (entry.value(from)? != value).then_some(Change { step, value })
            })
            .collect()
    }

    /// Owned render ceiling: 2^20 frames (about 21.8 s at 48 kHz, 16 MiB of lanes).
    pub const MAX_FRAMES: usize = 1 << 20;

    /// Offline reference render in f32 with instant (unsmoothed) scene values.
    /// Missing input ports read as silence; every stored port appears in the result.
    /// Under `OpaquePolicy::Reject` any detached model fails first; `PassThrough`
    /// skips detached models.
    ///
    /// The sound domain is finite samples. Every input buffer must hold `frames` finite
    /// samples, and a render whose stored output is not finite (overflow) fails. The
    /// frame ceiling and inputs are checked before any buffer is allocated. Inputs are
    /// borrowed, so a failure never changes them.
    pub fn render(
        &self,
        scene: SceneIndex,
        inputs: &BTreeMap<PortId, Vec<f32>>,
        frames: usize,
        policy: OpaquePolicy,
    ) -> Result<BTreeMap<PortId, Vec<f32>>, Error> {
        if let (OpaquePolicy::Reject, Some(&Detached { stage, model })) =
            (policy, self.detached.first())
        {
            return Err(Error::DetachedModel { stage, model });
        }
        if frames > Self::MAX_FRAMES {
            return Err(Error::FrameLimit {
                frames,
                max: Self::MAX_FRAMES,
            });
        }
        for (&port, samples) in inputs {
            if samples.len() != frames {
                return Err(Error::FrameMismatch {
                    port,
                    expected: frames,
                    actual: samples.len(),
                });
            }
            if let Some(frame) = samples.iter().position(|x| !x.is_finite()) {
                return Err(Error::NonFiniteInput { port, frame });
            }
        }
        let mut lanes: [Vec<f32>; ROWS] = std::array::from_fn(|_| vec![0.0; frames]);
        let mut outputs = BTreeMap::new();
        for step in &self.steps {
            match *step {
                Step::Load { row, port } => {
                    let lane = &mut lanes[row.index()];
                    match port.and_then(|port| inputs.get(&port)) {
                        Some(samples) => lane.copy_from_slice(samples),
                        None => lane.fill(0.0),
                    }
                }
                Step::Split { pair, gains } => {
                    let gains = gains.at(scene);
                    let [master, slave] = pair_lanes(&mut lanes, pair);
                    for (m, s) in master.iter_mut().zip(slave.iter_mut()) {
                        *s = *m * gains.lower.linear();
                        *m *= gains.upper.linear();
                    }
                }
                Step::Process { slot, block } => {
                    if block.bypass.at(scene) {
                        continue;
                    }
                    match block.kind {
                        BlockKind::Gain(gain) => {
                            let gain = gain.at(scene).linear();
                            lanes[slot.row.index()].iter_mut().for_each(|x| *x *= gain);
                        }
                        BlockKind::Model(model) => reject_opaque(policy, slot, model)?,
                    }
                }
                Step::Merge { pair, gains } => {
                    let gains = gains.at(scene);
                    let [master, slave] = pair_lanes(&mut lanes, pair);
                    for (m, s) in master.iter_mut().zip(slave.iter_mut()) {
                        *m = *m * gains.upper.linear() + *s * gains.lower.linear();
                        *s = 0.0;
                    }
                }
                Step::Store { row, port } => {
                    let output = outputs.entry(port).or_insert_with(|| vec![0.0; frames]);
                    for (out, x) in output.iter_mut().zip(&lanes[row.index()]) {
                        *out += x;
                    }
                }
            }
        }
        for (&port, samples) in &outputs {
            if let Some(frame) = samples.iter().position(|x| !x.is_finite()) {
                return Err(Error::NonFiniteOutput { port, frame });
            }
        }
        Ok(outputs)
    }
}

fn pair_lanes(lanes: &mut [Vec<f32>; ROWS], pair: Pair) -> [&mut Vec<f32>; 2] {
    let (head, tail) = lanes.split_at_mut(pair.slave().index());
    [&mut head[pair.master().index()], &mut tail[0]]
}

fn reject_opaque(policy: OpaquePolicy, slot: Slot, model: ModelId) -> Result<(), Error> {
    match policy {
        OpaquePolicy::Reject => Err(Error::OpaqueModel { slot, model }),
        OpaquePolicy::PassThrough => Ok(()),
    }
}

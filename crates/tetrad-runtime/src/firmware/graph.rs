//! Per-frame block graph: topological schedule and buffer allocation.
//!
//! Nodes are stock module slots (`0..30`, as in `ModuleSlot` of
//! `tetrad-dsp-protocol`). Each node has one mono output and a kernel-defined
//! number of inputs. The planner orders nodes so every source runs first and
//! reuses a buffer only after its last reader in the frame. Graph outputs keep
//! their buffers until the end of the frame. Kernels never alias input and
//! output: the gain leaf allows exact aliasing, other kernels may not.
//!
//! The planner is generic over the node kernel. [`KernelKind`] is the default;
//! [`ResidentKernel`] adds owned tetrad-blocks bodies (adapter B).

use std::collections::BTreeMap;
use std::fmt::Debug;

/// Stock module slot count per core (`ModuleSlot` accepts `0..30`).
pub const SLOT_COUNT: u8 = 30;
/// TDM slots per SPORT frame (`audio_port::TDM_SLOTS`).
pub const HARDWARE_CHANNELS: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotId(u8);

impl SlotId {
    pub const fn new(slot: u8) -> Result<Self, GraphError> {
        if slot < SLOT_COUNT {
            Ok(Self(slot))
        } else {
            Err(GraphError::Slot(slot))
        }
    }
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// One TDM channel of the receive or transmit SPORT frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Channel(u8);

impl Channel {
    pub const fn new(channel: u8) -> Result<Self, GraphError> {
        if channel < HARDWARE_CHANNELS {
            Ok(Self(channel))
        } else {
            Err(GraphError::Channel(channel))
        }
    }
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Owned kernels. Each has a fixed input count and one output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KernelKind {
    /// `out = in * gain`; parameter 0 is the linear gain.
    Gain,
    /// `out = a + b`; no parameters. Bypass is undefined for two inputs.
    Mix,
}

impl KernelKind {
    pub const fn inputs(self) -> usize {
        match self {
            Self::Gain => 1,
            Self::Mix => 2,
        }
    }
    pub const fn parameters(self) -> usize {
        match self {
            Self::Gain => 1,
            Self::Mix => 0,
        }
    }
    /// Bypass copies the single input to the output.
    pub const fn bypassable(self) -> bool {
        self.inputs() == 1
    }
}

/// What a node runs. The planner only needs the input count; the image also
/// asks which built-in kernel it is (`None`: an owned block, adapter B).
pub trait Kernel: Copy + Eq + Debug {
    fn inputs(self) -> usize;
    fn builtin(self) -> Option<KernelKind>;
}

impl Kernel for KernelKind {
    fn inputs(self) -> usize {
        KernelKind::inputs(self)
    }
    fn builtin(self) -> Option<KernelKind> {
        Some(self)
    }
}

/// Path (b) node kernels: the built-in kernels or the owned block that
/// `code::emit_block_kernels` assigns to the node's slot (adapter B). Owned
/// blocks run as mono nodes: one input, processed in place in the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResidentKernel {
    Builtin(KernelKind),
    Block,
}

impl Kernel for ResidentKernel {
    fn inputs(self) -> usize {
        match self {
            Self::Builtin(kind) => kind.inputs(),
            Self::Block => 1,
        }
    }
    fn builtin(self) -> Option<KernelKind> {
        match self {
            Self::Builtin(kind) => Some(kind),
            Self::Block => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    Input(Channel),
    Node(SlotId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeSpec<K = KernelKind> {
    pub slot: SlotId,
    pub kernel: K,
    pub inputs: Vec<Source>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphSpec<K = KernelKind> {
    pub nodes: Vec<NodeSpec<K>>,
    pub outputs: Vec<(Channel, Source)>,
}

impl<K> Default for GraphSpec<K> {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            outputs: Vec::new(),
        }
    }
}

/// Index into the resident buffer pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BufferId(u16);

impl BufferId {
    pub const fn get(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedNode<K = KernelKind> {
    pub slot: SlotId,
    pub kernel: K,
    pub inputs: Vec<BufferId>,
    pub output: BufferId,
}

/// The validated per-frame schedule. Step order is execution order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FramePlan<K = KernelKind> {
    pub inputs: Vec<(Channel, BufferId)>,
    pub steps: Vec<PlannedNode<K>>,
    pub outputs: Vec<(Channel, BufferId)>,
    pub buffer_count: u16,
}

impl<K> FramePlan<K> {
    pub fn step(&self, slot: SlotId) -> Option<&PlannedNode<K>> {
        self.steps.iter().find(|step| step.slot == slot)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
    #[error("module slot {0} is outside 0..{SLOT_COUNT}")]
    Slot(u8),
    #[error("hardware channel {0} is outside 0..{HARDWARE_CHANNELS}")]
    Channel(u8),
    #[error("module slot {0:?} is used twice")]
    DuplicateSlot(SlotId),
    #[error("the kernel in slot {slot:?} needs {expected} inputs, got {actual}")]
    InputCount {
        slot: SlotId,
        expected: usize,
        actual: usize,
    },
    #[error("slot {slot:?} reads missing slot {source_slot:?}")]
    MissingSource { slot: SlotId, source_slot: SlotId },
    #[error("output channel {0:?} reads a missing slot")]
    MissingOutputSource(Channel),
    #[error("output channel {0:?} is assigned twice")]
    DuplicateOutput(Channel),
    #[error("the graph contains a cycle through slot {0:?}")]
    Cycle(SlotId),
}

/// Kahn order with ascending slot tie-break, so equal graphs give equal plans.
pub fn plan<K: Kernel>(spec: &GraphSpec<K>) -> Result<FramePlan<K>, GraphError> {
    let mut nodes = BTreeMap::new();
    for node in &spec.nodes {
        if nodes.insert(node.slot, node).is_some() {
            return Err(GraphError::DuplicateSlot(node.slot));
        }
        if node.inputs.len() != node.kernel.inputs() {
            return Err(GraphError::InputCount {
                slot: node.slot,
                expected: node.kernel.inputs(),
                actual: node.inputs.len(),
            });
        }
    }
    for node in &spec.nodes {
        for source in &node.inputs {
            if let Source::Node(source_slot) = *source
                && !nodes.contains_key(&source_slot)
            {
                return Err(GraphError::MissingSource {
                    slot: node.slot,
                    source_slot,
                });
            }
        }
    }
    let mut seen_outputs = Vec::new();
    for &(channel, source) in &spec.outputs {
        if seen_outputs.contains(&channel) {
            return Err(GraphError::DuplicateOutput(channel));
        }
        seen_outputs.push(channel);
        if let Source::Node(slot) = source
            && !nodes.contains_key(&slot)
        {
            return Err(GraphError::MissingOutputSource(channel));
        }
    }

    let order = topological_order(&nodes)?;

    // Reads per source: node inputs plus graph outputs (which never release).
    let mut remaining: BTreeMap<Source, usize> = BTreeMap::new();
    for node in nodes.values() {
        for source in &node.inputs {
            *remaining.entry(*source).or_default() += 1;
        }
    }
    let pinned: Vec<Source> = spec.outputs.iter().map(|&(_, source)| source).collect();

    let mut pool = Pool::default();
    let mut assigned: BTreeMap<Source, BufferId> = BTreeMap::new();
    let mut inputs = Vec::new();
    let mut channels: Vec<Channel> = remaining
        .keys()
        .chain(pinned.iter())
        .filter_map(|source| match source {
            Source::Input(channel) => Some(*channel),
            Source::Node(_) => None,
        })
        .collect();
    channels.sort();
    channels.dedup();
    for channel in channels {
        let buffer = pool.take();
        assigned.insert(Source::Input(channel), buffer);
        inputs.push((channel, buffer));
    }

    let mut steps = Vec::with_capacity(order.len());
    for slot in order {
        let node = nodes[&slot];
        let input_buffers: Vec<BufferId> = node.inputs.iter().map(|s| assigned[s]).collect();
        let output = pool.take();
        assigned.insert(Source::Node(slot), output);
        for source in &node.inputs {
            let count = remaining.get_mut(source).expect("counted source");
            *count -= 1;
            if *count == 0 && !pinned.contains(source) {
                pool.release(assigned[source]);
            }
        }
        // An unread, unpinned output is dead after this step.
        if !remaining.contains_key(&Source::Node(slot)) && !pinned.contains(&Source::Node(slot)) {
            pool.release(output);
        }
        steps.push(PlannedNode {
            slot,
            kernel: node.kernel,
            inputs: input_buffers,
            output,
        });
    }
    let outputs = spec
        .outputs
        .iter()
        .map(|&(channel, source)| (channel, assigned[&source]))
        .collect();
    Ok(FramePlan {
        inputs,
        steps,
        outputs,
        buffer_count: pool.high_water,
    })
}

fn topological_order<K>(nodes: &BTreeMap<SlotId, &NodeSpec<K>>) -> Result<Vec<SlotId>, GraphError> {
    let mut pending: BTreeMap<SlotId, usize> = nodes
        .iter()
        .map(|(&slot, node)| {
            let count = node
                .inputs
                .iter()
                .filter(|s| matches!(s, Source::Node(_)))
                .count();
            (slot, count)
        })
        .collect();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(slot) = pending
        .iter()
        .find_map(|(&slot, &count)| (count == 0).then_some(slot))
    {
        pending.remove(&slot);
        order.push(slot);
        for (reader, node) in nodes {
            let reads = node
                .inputs
                .iter()
                .filter(|s| **s == Source::Node(slot))
                .count();
            if let Some(count) = pending.get_mut(reader) {
                *count -= reads;
            }
        }
    }
    match pending.keys().next() {
        Some(&slot) => Err(GraphError::Cycle(slot)),
        None => Ok(order),
    }
}

/// Lowest-index-first free list, so the plan is deterministic.
#[derive(Debug, Default)]
struct Pool {
    free: Vec<BufferId>,
    high_water: u16,
}

impl Pool {
    fn take(&mut self) -> BufferId {
        self.free.sort_by(|a, b| b.cmp(a));
        self.free.pop().unwrap_or_else(|| {
            let buffer = BufferId(self.high_water);
            self.high_water += 1;
            buffer
        })
    }
    fn release(&mut self, buffer: BufferId) {
        self.free.push(buffer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(n: u8) -> SlotId {
        SlotId::new(n).unwrap()
    }
    fn channel(n: u8) -> Channel {
        Channel::new(n).unwrap()
    }
    fn gain(n: u8, source: Source) -> NodeSpec {
        NodeSpec {
            slot: slot(n),
            kernel: KernelKind::Gain,
            inputs: vec![source],
        }
    }

    #[test]
    fn chain_reuses_buffers_after_their_last_reader() {
        // in0 -> 5 -> 2 -> 9 -> out0. Slot numbers are not execution order.
        let spec = GraphSpec {
            nodes: vec![
                gain(9, Source::Node(slot(2))),
                gain(2, Source::Node(slot(5))),
                gain(5, Source::Input(channel(0))),
            ],
            outputs: vec![(channel(0), Source::Node(slot(9)))],
        };
        let plan = plan(&spec).unwrap();
        let order: Vec<u8> = plan.steps.iter().map(|s| s.slot.get()).collect();
        assert_eq!(order, [5, 2, 9]);
        // Two buffers alternate: no node ever reads and writes the same one.
        assert_eq!(plan.buffer_count, 2);
        for step in &plan.steps {
            assert!(!step.inputs.contains(&step.output));
        }
        assert_eq!(plan.outputs, [(channel(0), plan.steps[2].output)]);
    }

    #[test]
    fn split_and_mix_keep_both_branches_alive_until_the_merge() {
        let spec = GraphSpec {
            nodes: vec![
                gain(0, Source::Input(channel(1))),
                gain(1, Source::Input(channel(1))),
                NodeSpec {
                    slot: slot(2),
                    kernel: KernelKind::Mix,
                    inputs: vec![Source::Node(slot(0)), Source::Node(slot(1))],
                },
            ],
            outputs: vec![
                (channel(0), Source::Node(slot(2))),
                (channel(1), Source::Input(channel(1))),
            ],
        };
        let plan = plan(&spec).unwrap();
        let mix = plan.step(slot(2)).unwrap();
        assert_ne!(mix.inputs[0], mix.inputs[1]);
        assert!(!mix.inputs.contains(&mix.output));
        // The dry input is pinned as an output, so the merge cannot reuse it.
        let dry = plan.inputs[0].1;
        assert_ne!(mix.output, dry);
        assert_eq!(plan.outputs[1], (channel(1), dry));
    }

    #[test]
    fn invalid_graphs_are_rejected_with_the_offending_slot() {
        let cycle = GraphSpec {
            nodes: vec![
                gain(3, Source::Node(slot(4))),
                gain(4, Source::Node(slot(3))),
            ],
            outputs: vec![],
        };
        assert_eq!(plan(&cycle), Err(GraphError::Cycle(slot(3))));
        let missing = GraphSpec {
            nodes: vec![gain(3, Source::Node(slot(7)))],
            outputs: vec![],
        };
        assert_eq!(
            plan(&missing),
            Err(GraphError::MissingSource {
                slot: slot(3),
                source_slot: slot(7)
            })
        );
        let wrong_arity = GraphSpec {
            nodes: vec![NodeSpec {
                slot: slot(1),
                kernel: KernelKind::Mix,
                inputs: vec![Source::Input(channel(0))],
            }],
            outputs: vec![],
        };
        assert!(matches!(
            plan(&wrong_arity),
            Err(GraphError::InputCount { expected: 2, .. })
        ));
        assert_eq!(SlotId::new(30), Err(GraphError::Slot(30)));
    }

    #[test]
    fn owned_blocks_are_one_input_nodes_beside_builtin_kernels() {
        let node = |n, kernel, inputs| NodeSpec {
            slot: slot(n),
            kernel,
            inputs,
        };
        let spec = GraphSpec {
            nodes: vec![
                node(6, ResidentKernel::Block, vec![Source::Node(slot(2))]),
                node(
                    2,
                    ResidentKernel::Builtin(KernelKind::Gain),
                    vec![Source::Input(channel(0))],
                ),
            ],
            outputs: vec![(channel(0), Source::Node(slot(6)))],
        };
        let order: Vec<u8> = plan(&spec)
            .unwrap()
            .steps
            .iter()
            .map(|s| s.slot.get())
            .collect();
        assert_eq!(order, [2, 6]);
        let stereo = GraphSpec {
            nodes: vec![node(
                1,
                ResidentKernel::Block,
                vec![Source::Input(channel(0)), Source::Input(channel(1))],
            )],
            outputs: vec![],
        };
        assert_eq!(
            plan(&stereo),
            Err(GraphError::InputCount {
                slot: slot(1),
                expected: 1,
                actual: 2
            })
        );
    }
}

use crate::{COLUMNS, Column, GainDb, ModelId, Pair, PortId, ROWS, Row, Scened, Slot};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlockKind {
    /// Owned gain stage; the only block the reference renderer processes.
    Gain(Scened<GainDb>),
    /// Opaque catalog model, scheduled but not processed by this crate.
    Model(ModelId),
}

/// One grid block. A bypassed block passes its input unchanged (owned policy).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Block {
    pub kind: BlockKind,
    pub bypass: Scened<bool>,
}

impl Block {
    pub fn gain(gain: GainDb) -> Self {
        Self {
            kind: BlockKind::Gain(Scened::Fixed(gain)),
            bypass: Scened::Fixed(false),
        }
    }

    pub fn model(model: ModelId) -> Self {
        Self {
            kind: BlockKind::Model(model),
            bypass: Scened::Fixed(false),
        }
    }
}

/// Master and slave lane levels at a split or merge (owned policy; stock splitter and
/// mixer parameters are scene-capable but their meaning is not recovered).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneGains {
    pub upper: GainDb,
    pub lower: GainDb,
}

impl LaneGains {
    pub const UNITY: Self = Self {
        upper: GainDb::UNITY,
        lower: GainDb::UNITY,
    };
}

/// A split or merge point. Points are preset-level topology, not scene values; only
/// the gains vary by scene.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Junction {
    pub column: Column,
    pub gains: Scened<LaneGains>,
}

impl Junction {
    pub fn at(column: Column) -> Self {
        Self {
            column,
            gains: Scened::Fixed(LaneGains::UNITY),
        }
    }
}

/// Stock `SplitControlPoints { split, mix }` with −1 as "absent" maps to `None`.
///
/// Split feeds the slave lane from the master lane before the split column's blocks.
/// Merge sums the slave lane into the master lane after the merge column's blocks.
/// Without a split the slave lane reads its own input; without a merge it drives its
/// own output.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PairRoute {
    pub split: Option<Junction>,
    pub merge: Option<Junction>,
}

/// Where a detached model sits in the stock preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DetachedStage {
    InputControl(Row),
    OutputControl(Row),
    Splitter(Pair),
    Mixer(Pair),
}

/// A stored model that processes audio in stock but has no recovered schedule
/// position or owned processor. It is kept so that `OpaquePolicy::Reject` refuses to
/// render a grid that would silently skip it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Detached {
    pub stage: DetachedStage,
    pub model: ModelId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LanePorts {
    pub input: Option<PortId>,
    pub output: Option<PortId>,
}

/// Editable owned grid. Edits never fail; `Grid::compile` validates the whole layout.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Grid {
    pub(crate) blocks: [[Option<Block>; COLUMNS]; ROWS],
    pub(crate) ports: [LanePorts; ROWS],
    pub(crate) routes: [PairRoute; 2],
    pub(crate) detached: Vec<Detached>,
}

impl Grid {
    pub fn new() -> Self {
        Self::default()
    }

    /// Places or clears a block and returns the previous occupant.
    pub fn set_block(&mut self, slot: Slot, block: Option<Block>) -> Option<Block> {
        std::mem::replace(
            &mut self.blocks[slot.row.index()][slot.column.index()],
            block,
        )
    }

    pub fn block(&self, slot: Slot) -> Option<&Block> {
        self.blocks[slot.row.index()][slot.column.index()].as_ref()
    }

    pub fn block_mut(&mut self, slot: Slot) -> Option<&mut Block> {
        self.blocks[slot.row.index()][slot.column.index()].as_mut()
    }

    pub fn set_ports(&mut self, row: Row, ports: LanePorts) {
        self.ports[row.index()] = ports;
    }

    pub fn ports(&self, row: Row) -> LanePorts {
        self.ports[row.index()]
    }

    pub fn set_route(&mut self, pair: Pair, route: PairRoute) {
        self.routes[pair as usize] = route;
    }

    pub fn route(&self, pair: Pair) -> PairRoute {
        self.routes[pair as usize]
    }

    /// Detached models, sorted by stage. Only the preset importer creates them.
    pub fn detached(&self) -> &[Detached] {
        &self.detached
    }
}

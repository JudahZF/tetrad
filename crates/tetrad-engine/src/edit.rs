//! Layout edits: add, remove, move and replace models. Stock recompiles after
//! every structural edit (`addModelToChain` `0x522d70` → `0x5122c0`); these only
//! change the layout, and [`crate::Deployment::edit`] recompiles.

use tetrad_catalog::{ModelSpec, ParameterReference, ParameterType, Specs};

use crate::{
    COLUMNS, Column, InputPort, Instance, Layout, ModelId, OutputPort, Pair, Scened, Site, Slot,
    TextParam,
};

/// The half of an occupied slot a dragged block is dropped on: the stock `SlotItem` drop
/// rects `+0x1e0` (left half) and `+0x1f0` (right half), each 15 px larger outwards
/// (`0x5a6fe4`). The drag move handler sends the left one as `dragEnterSlotWithModelLeft`
/// (`0x5a92a4`), the right one as `dragEnterSlotWithModelRight` (`0x5a9abc`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DropSide {
    /// The hovered block and its right neighbours make room to the right first
    /// (`ChainItemContainer` `0x5c09c8`).
    #[default]
    Left,
    /// The hovered block and its left neighbours make room to the left first (`0x5c0ee0`).
    Right,
}

/// What a grid drag and drop does to the layout (stock `Chain::dropSlot` `0x59f620`), from
/// [`Layout::plan_drop`]. Stock removes the dropped block, moves the blocks that make room
/// (`0x59e194`), then places the dropped block; [`Layout::move_blocks`] does all at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridDrop {
    /// Moves that happen at once: the blocks that make room, then the dropped block.
    pub moves: Vec<(Slot, Slot)>,
    /// New split and mix points of a pair, when the drop changes them (`0x59f1cc` →
    /// `setSplitMixPoints` `0x51a090`).
    pub points: Option<PairPoints>,
}

/// Split and mix points of a pair (`None` = stock −1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairPoints {
    pub pair: Pair,
    pub split: Option<Column>,
    pub mix: Option<Column>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum EditError {
    #[error("{0:?} is empty")]
    Empty(Site),
    #[error("{0:?} is occupied")]
    Occupied(Slot),
    #[error("model {0:?} is not in the catalog")]
    UnknownModel(ModelId),
    #[error("{site:?} cannot hold model {model:?}")]
    Role { site: Site, model: ModelId },
}

/// A fresh instance with normalized catalog defaults (string parameters keep
/// their default text).
pub fn new_instance(spec: &ModelSpec) -> Instance {
    let mut instance = Instance::new(
        ModelId(spec.id),
        spec.parameters
            .iter()
            .map(|p| Scened::Fixed(p.default_normalized()))
            .collect(),
    );
    instance.texts = spec
        .parameters
        .iter()
        .filter(|p| p.kind == ParameterType::String)
        .map(|p| TextParam {
            index: p.index,
            scenes: vec![match &p.default {
                Some(tetrad_catalog::ParamDefault::Text(text)) => Some(text.clone()),
                _ => None,
            }],
        })
        .collect();
    instance
}

impl Layout {
    pub fn add_block(
        &mut self,
        slot: Slot,
        model: ModelId,
        specs: &Specs,
    ) -> Result<(), EditError> {
        if self.instance(Site::Slot(slot)).is_some() {
            return Err(EditError::Occupied(slot));
        }
        let spec = block_spec(specs, Site::Slot(slot), model)?;
        self.set_instance(Site::Slot(slot), Some(new_instance(spec)));
        Ok(())
    }

    pub fn remove_block(&mut self, slot: Slot) -> Result<Instance, EditError> {
        self.set_instance(Site::Slot(slot), None)
            .ok_or(EditError::Empty(Site::Slot(slot)))
    }

    /// What a drag of the block at `from` dropped on `to` does (stock `Chain::dropSlot`
    /// `0x59f620`), or `None` when stock refuses the drop:
    ///
    /// - Another row must have an empty slot (`SlotItem` drop `0x5aa738`).
    /// - An occupied target makes room ([`DropSide`]): the blocks from the target up to the
    ///   nearest free slot on the first side move one column towards it, else on the other
    ///   side (`0x5c0440`). The source slot of a drop in its own row is free. The dropped
    ///   block takes the target column.
    /// - Path A to path B of a pair (row 1 → 2, row 3 → 4) makes a parallel path
    ///   (`0x59f1cc`): an absent split goes to column 0 when row B has no input and no
    ///   upper row feeds it (`0x6baf54`: `NEXT_ROW_4` or `NEXT_ROW_3_4` for row 4); an
    ///   absent mix goes to column 7 when row B has no output.
    /// - The last block of path B dropped on path A clears both points (`0x59baf8`).
    pub fn plan_drop(&self, from: Slot, to: Slot, side: DropSide) -> Option<GridDrop> {
        if from == to || self.instance(Site::Slot(from)).is_none() {
            return None;
        }
        let lane = &self.lanes[to.row.index()];
        let same_row = from.row == to.row;
        let free = |column: usize| {
            lane.slots[column].is_none() || (same_row && column == from.column.index())
        };
        if !(0..COLUMNS).any(free) {
            return None;
        }
        let target = to.column.index();
        let mut moves = if free(target) {
            Vec::new()
        } else {
            let right = || (target + 1..COLUMNS).find(|&column| free(column));
            let left = || (0..target).rev().find(|&column| free(column));
            let slot = |column: usize| Slot {
                row: to.row,
                column: Column(column as u8),
            };
            match (side, right(), left()) {
                (DropSide::Left, Some(gap), _) | (DropSide::Right, Some(gap), None) => (target
                    ..gap)
                    .map(|column| (slot(column), slot(column + 1)))
                    .collect(),
                (_, _, Some(gap)) => (gap + 1..=target)
                    .map(|column| (slot(column), slot(column - 1)))
                    .collect(),
                (_, None, None) => return None,
            }
        };
        moves.push((from, to));
        Some(GridDrop {
            moves,
            points: self.drop_points(from, to),
        })
    }

    /// The split and mix points a drop between the rows of a pair sets ([`Layout::plan_drop`]).
    fn drop_points(&self, from: Slot, to: Slot) -> Option<PairPoints> {
        let pair = from.row.pair();
        if to.row.pair() != pair || from.row == to.row {
            return None;
        }
        let junctions = self.pair(pair);
        let (split, mix) = (junctions.split, junctions.merge);
        let slave = self.lane(pair.slave());
        let points = if from.row == pair.master() {
            let fed = pair == Pair::Lower
                && (self.lanes[..2].iter()).any(|lane| {
                    matches!(lane.output, OutputPort::NextRow4 | OutputPort::NextRow3And4)
                });
            let open = slave.input == InputPort::Empty && !fed;
            PairPoints {
                pair,
                split: split.or(open.then_some(Column(0))),
                mix: mix.or((slave.output == OutputPort::Empty).then_some(Column(7))),
            }
        } else {
            let alone = (slave.slots.iter().enumerate())
                .all(|(column, slot)| slot.is_none() || column == from.column.index());
            if !alone {
                return None;
            }
            PairPoints {
                pair,
                split: None,
                mix: None,
            }
        };
        (points.split != split || points.mix != mix).then_some(points)
    }

    /// Moves blocks at once (a [`GridDrop`]). Each source must hold a block; each target must
    /// be empty or a source, and no two moves share a target. Nothing changes on an error.
    pub fn move_blocks(&mut self, moves: &[(Slot, Slot)]) -> Result<(), EditError> {
        for &(from, to) in moves {
            if self.instance(Site::Slot(from)).is_none() {
                return Err(EditError::Empty(Site::Slot(from)));
            }
            let vacated = moves.iter().any(|&(source, _)| source == to);
            let shared = moves.iter().filter(|&&(_, target)| target == to).count() > 1;
            if shared || (self.instance(Site::Slot(to)).is_some() && !vacated) {
                return Err(EditError::Occupied(to));
            }
        }
        let moving: Vec<(Slot, Option<Instance>)> = (moves.iter())
            .map(|&(from, to)| (to, self.set_instance(Site::Slot(from), None)))
            .collect();
        for (to, instance) in moving {
            self.set_instance(Site::Slot(to), instance);
        }
        Ok(())
    }

    /// Moves a block; an occupied target swaps with the source.
    pub fn move_block(&mut self, from: Slot, to: Slot) -> Result<(), EditError> {
        let moving = self
            .set_instance(Site::Slot(from), None)
            .ok_or(EditError::Empty(Site::Slot(from)))?;
        let displaced = self.set_instance(Site::Slot(to), Some(moving));
        self.set_instance(Site::Slot(from), displaced);
        Ok(())
    }

    /// Replaces the model at `site`, keeping bypass, mute and sidechain flags.
    /// A new parameter takes the old value of the parameter it names in
    /// `replaces` (index or name), else of the old parameter with the same name,
    /// else its default. Inferred from the ModelRepo `replaces` lists; the stock
    /// "Model update" mapping routine is not recovered.
    pub fn replace_model(
        &mut self,
        site: Site,
        model: ModelId,
        specs: &Specs,
    ) -> Result<(), EditError> {
        let old = self.instance(site).ok_or(EditError::Empty(site))?;
        let old_spec = specs
            .model(old.model.0)
            .ok_or(EditError::UnknownModel(old.model))?;
        let spec = block_spec(specs, site, model)?;
        let mut instance = new_instance(spec);
        for (index, parameter) in spec.parameters.iter().enumerate() {
            let source = parameter
                .replaces
                .iter()
                .find_map(|reference| match reference {
                    ParameterReference::Index(index) => Some(*index as usize),
                    ParameterReference::Name(name) => {
                        old_spec.parameters.iter().position(|p| &p.name == name)
                    }
                })
                .or_else(|| {
                    old_spec
                        .parameters
                        .iter()
                        .position(|p| p.name == parameter.name)
                })
                .filter(|&source| old_spec.parameters[source].kind == parameter.kind);
            if let Some(source) = source.filter(|&source| source < old.params.len()) {
                instance.params[index] = old.params[source];
                instance.scene_mode[index] = old.scene_mode.get(source).copied().unwrap_or(false);
            }
        }
        instance.bypass = old.bypass;
        instance.bypass_scene_mode = old.bypass_scene_mode;
        instance.mute = old.mute;
        instance.sidechain = old.sidechain;
        self.set_instance(site, Some(instance));
        Ok(())
    }
}

fn block_spec(specs: &Specs, site: Site, model: ModelId) -> Result<&ModelSpec, EditError> {
    let spec = specs.model(model.0).ok_or(EditError::UnknownModel(model))?;
    let expected = match site {
        Site::Slot(_) => tetrad_catalog::ModelRole::Block,
        Site::Splitter(_) => tetrad_catalog::ModelRole::Splitter,
        Site::Mixer(_) => tetrad_catalog::ModelRole::Mixer,
        Site::InputControl(_) => tetrad_catalog::ModelRole::InputGate,
        Site::OutputControl(_) => tetrad_catalog::ModelRole::LaneOutput,
    };
    if spec.role != expected {
        return Err(EditError::Role { site, model });
    }
    Ok(spec)
}

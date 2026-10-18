//! Sidechain follow on grid edits. Stock records each sink's selected option
//! before an insert, remove or move (`0x0052f23c`) and selects the same
//! option again after the graph refresh (`0x0052fecc`).
//!
//! The option list of a sink's SOURCE parameter is rebuilt from the grid
//! (`0x005319fc`): the first 14 XML step names, then one entry per source
//! block before the sink in row-major order, with step ID `R<row+1> C<col+1>`
//! (`0x0052ebf4`). The stored value is `index · 1/(options − 1)`. Stock finds a
//! block option again by its step ID (moves rename it, `0x0052e0f4`) and a
//! fixed option by its label; a source that is gone selects option 0.

use crate::{
    COLUMNS, Error, ParamValue, ROWS, Result, SCENES, Slot,
    migrate::ModelCatalog,
    model::{Preset, SceneValue},
};

/// Fixed options before the block sources. `0x005319fc` pads or cuts the XML
/// step names to 14; the 14 stock labels are distinct, so a fixed index stays.
const FIXED_OPTIONS: usize = 14;

/// Grid occupancy and the pair mixer columns: the inputs of the source mask.
struct Grid {
    ids: [[u32; COLUMNS]; ROWS],
    mixers: [Option<usize>; 2],
}

impl Grid {
    fn of(preset: &Preset) -> Result<Self> {
        if preset.chains.len() != ROWS
            || preset
                .chains
                .iter()
                .any(|chain| chain.models.len() != COLUMNS)
        {
            return Err(Error::Layout(
                "sidechain follow needs four chains of eight slots",
            ));
        }
        let mixer = |row: usize| {
            preset.chains[row]
                .split_control_points
                .first()
                .and_then(|points| usize::try_from(points.mix).ok())
        };
        Ok(Self {
            ids: std::array::from_fn(|row| {
                std::array::from_fn(|column| preset.chains[row].models[column].hash.unwrap_or(0))
            }),
            mixers: [mixer(0), mixer(2)],
        })
    }

    fn id(&self, slot: Slot) -> u32 {
        self.ids[slot.row][slot.column]
    }

    /// Source blocks of a sink in option order (mask of `0x0052ebf4`; the
    /// count is `tetrad_core::SourceLayout::preceding_sources`). A lower row reads
    /// all earlier rows, its upper row up to the mixer column (all when the
    /// mixer is negative) and its own row before the sink.
    fn sources(&self, sink: Slot) -> Vec<Slot> {
        let (r, c) = (sink.row, sink.column);
        let precedes = |row: usize, column: usize| {
            if r % 2 == 0 {
                row < r || (row == r && column < c)
            } else {
                row + 1 < r
                    || (row + 1 == r && self.mixers[r / 2].is_none_or(|mix| column <= mix))
                    || (row == r && column < c)
            }
        };
        (0..ROWS)
            .flat_map(|row| (0..COLUMNS).map(move |column| Slot { row, column }))
            .filter(|slot| precedes(slot.row, slot.column) && self.id(*slot) != 0)
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Choice {
    Fixed(usize),
    Source(Slot),
}

fn step(sources: usize) -> f32 {
    1.0 / (FIXED_OPTIONS + sources - 1) as f32
}

/// `0x005cc5d8`: `lroundf(value / max(step, 1e-8))`. An index outside the
/// list gives `None`; stock records nothing for it, and the value is kept.
fn decode(value: f32, sources: &[Slot]) -> Option<Choice> {
    let index = (value / step(sources.len()).max(1e-8)).round();
    if !(0.0..(FIXED_OPTIONS + sources.len()) as f32).contains(&index) {
        return None;
    }
    let index = index as usize;
    Some(match index.checked_sub(FIXED_OPTIONS) {
        None => Choice::Fixed(index),
        Some(source) => Choice::Source(sources[source]),
    })
}

/// `0x005cc304`: the stored value of an option index.
fn encode(choice: Choice, sources: &[Slot]) -> f32 {
    let index = match choice {
        Choice::Fixed(index) => index,
        Choice::Source(slot) => sources
            .iter()
            .position(|source| *source == slot)
            .map_or(0, |position| FIXED_OPTIONS + position),
    };
    step(sources.len()) * index as f32
}

impl Preset {
    /// Runs a grid edit on a copy, then writes every sink's recorded option
    /// again for the new grid, as stock does. `moved` maps moved blocks' old
    /// slots to their new slots. Inserted, removed and replaced slots are skipped.
    /// A block the catalog does not know is refused when its source list
    /// changes (it may be a sink). The preset changes only on success.
    pub(crate) fn follow_sidechains<T>(
        &mut self,
        catalog: &impl ModelCatalog,
        moved: &[(Slot, Slot)],
        edit: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        if !self.is_loaded_form() {
            return Err(Error::Unsupported(
                "grid edits need a migrated preset (sidechain values use the loaded encoding)",
            ));
        }
        let before = Grid::of(self)?;
        let mut next = self.clone();
        let output = edit(&mut next)?;
        let after = Grid::of(&next)?;
        let follow = |slot: Slot| {
            (moved.iter())
                .find(|(from, _)| *from == slot)
                .map_or(slot, |(_, to)| *to)
        };
        for old in (0..ROWS).flat_map(|row| (0..COLUMNS).map(move |column| Slot { row, column })) {
            let (id, new) = (before.id(old), follow(old));
            if id == 0 || after.id(new) != id {
                continue;
            }
            let old_sources: Vec<_> = before.sources(old).into_iter().map(follow).collect();
            let new_sources = after.sources(new);
            let Some(metadata) = catalog.model(id) else {
                if old_sources != new_sources {
                    return Err(Error::UnknownModel(id));
                }
                continue;
            };
            let Some(position) = metadata.sidechain_parameter else {
                continue;
            };
            let param = next.chains[new.row].models[new.column].param_mut(position)?;
            for scene in param.values.iter_mut().take(SCENES) {
                let Some(ParamValue::Float(stored)) = scene.value else {
                    continue;
                };
                let Some(choice) = decode(stored, &old_sources) else {
                    continue;
                };
                let value = encode(choice, &new_sources);
                if value.to_bits() != stored.to_bits() {
                    *scene = SceneValue {
                        value: Some(ParamValue::Float(value)),
                        ..SceneValue::default()
                    };
                }
            }
        }
        *self = next;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mask counts what the recovered import pass counts.
    #[test]
    fn source_mask_matches_the_import_source_count() {
        let mut ids = [[0; COLUMNS]; ROWS];
        for (n, slot) in [(0, 1), (0, 5), (1, 0), (1, 6), (2, 2), (3, 0), (3, 7)]
            .into_iter()
            .enumerate()
        {
            ids[slot.0][slot.1] = n as u32 + 1;
        }
        for mixers in [[Some(3), Some(1)], [None, None]] {
            let grid = Grid { ids, mixers };
            let layout = tetrad_core::SourceLayout::new(ids, mixers).unwrap();
            for row in 0..ROWS {
                for column in 0..COLUMNS {
                    let sources = grid.sources(Slot { row, column });
                    assert_eq!(
                        sources.len() as u32,
                        layout.preceding_sources(row, column).unwrap()
                    );
                    assert!(sources.windows(2).all(|pair| pair[0] < pair[1]));
                }
            }
        }
    }
}

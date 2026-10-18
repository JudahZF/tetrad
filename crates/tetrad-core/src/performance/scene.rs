//! Per-scene values with a scene-mode flag.
//!
//! Evidence: general setter `0x51e4d8` (scene mode writes the selected scene, else scenes 0..7),
//! disable `0x524770 → 0x522674` (copy the default-scene value to all eight scenes), enable
//! `0x517f54 → 0x513fc8` (flag only), and column bypass import `0x67c5d4` (stored flag OR any scene
//! differs from scene 0). Block bypass lives in the model's last parameter as 0.0/1.0/2.0
//! (`0x2ecca4`).

use crate::GRID_ROWS;
use crate::workflow::{BlockAddress, SceneIndex};

pub const SCENES: usize = 8;
/// Model whose parameter 12 is sent after its other parameters on recall, and
/// only when that parameter is 0.0.
pub const LATE_PARAMETER_MODEL: (u32, usize) = (27000, 12);
/// Model that receives no parameter sends on recall.
pub const NO_PARAMETER_MODEL: u32 = 25000;
/// Categories whose engaged blocks switch on before the rest of recall.
const RECALL_FIRST_PASS_CATEGORIES: [u32; 4] = [12, 21, 32, 33];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scened<T> {
    pub scene_mode: bool,
    pub values: [T; SCENES],
}

impl<T: Clone + PartialEq> Scened<T> {
    /// Scene mode off: every scene holds the same value.
    pub fn shared(value: T) -> Self {
        Self {
            scene_mode: false,
            values: std::array::from_fn(|_| value.clone()),
        }
    }

    /// Bypass/split/mix import rule: the stored flag (absent = false) OR any
    /// stored scene differing from scene 0. Ordinary parameter import is not
    /// covered by this evidence.
    pub fn imported_with_differences(stored_flag: Option<bool>, values: [T; SCENES]) -> Self {
        let differs = values.iter().any(|value| *value != values[0]);
        Self {
            scene_mode: stored_flag.unwrap_or(false) || differs,
            values,
        }
    }

    pub fn value(&self, scene: SceneIndex) -> &T {
        &self.values[usize::from(scene.get())]
    }

    /// Editor/touchscreen write through the general setter.
    pub fn edit(&mut self, selected: SceneIndex, value: T) {
        if self.scene_mode {
            self.values[usize::from(selected.get())] = value;
        } else {
            self.values.fill(value);
        }
    }

    pub fn enable_scene_mode(&mut self) {
        self.scene_mode = true;
    }

    /// The preset default scene (not the selected scene) supplies the value.
    pub fn disable_scene_mode(&mut self, default_scene: SceneIndex) {
        self.scene_mode = false;
        let value = self.value(default_scene).clone();
        self.values.fill(value);
    }
}

/// Bypass value sent in a block's bypass parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BypassValue {
    /// Engaged (0.0).
    Off,
    /// Bypassed (1.0).
    On,
    /// Muted bypass (2.0), used for scene-bypassed blocks when MUTE is on.
    Mute,
}

impl BypassValue {
    pub fn parameter(self) -> f32 {
        match self {
            Self::Off => 0.0,
            Self::On => 1.0,
            Self::Mute => 2.0,
        }
    }
}

/// One grid block for the recall plan, read after the scene change has
/// reverted uncommitted bypass (`0x5b244c` → `0x2e0e38`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecallBlock {
    pub block: BlockAddress,
    pub model_id: u32,
    pub category: u32,
    /// New-scene bypass, before the forced state.
    pub bypassed: bool,
    /// Forced state (`0x41111c`): bypasses the block whatever the scene says.
    pub globally_bypassed: bool,
    /// MUTE setting: a scene-bypassed, non-forced block sends [`BypassValue::Mute`].
    pub mute: bool,
    /// The bypass parameter is driven by an expression pedal: recall keeps the
    /// previous bypass and sends none.
    pub expression_bypass: bool,
    /// Parameter 12 of model 27000 is 0.0 in the new scene.
    pub late_parameter_zero: bool,
    pub parameter_count: usize,
}

impl RecallBlock {
    fn bypass_value(&self) -> BypassValue {
        match (self.bypassed, self.globally_bypassed, self.mute) {
            (_, true, _) | (true, false, false) => BypassValue::On,
            (true, false, true) => BypassValue::Mute,
            (false, false, _) => BypassValue::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallStep {
    Bypass {
        block: BlockAddress,
        value: BypassValue,
    },
    /// Flush the queued DSP messages (`0x511084`, `0x511188`, `0x512c74`).
    Flush,
    /// Row I/O after each even grid row (zero-based).
    RowIo {
        row: u8,
    },
    Parameter {
        block: BlockAddress,
        index: usize,
    },
}

/// DSP sends after a scene change (`0x527248`). First, engaged blocks of the
/// recall categories switch on, then a flush. Then, in grid order, each block
/// gets its bypass (skipped for expression-driven bypass) and its parameters
/// in index order, with row I/O after each even row. `skip` removes parameters
/// driven by an active expression pedal (`0x322178`). Held momentary stomps
/// (`0x518fe8`) come before this plan; the 23000/28000 groups come after it.
pub fn recall_plan(
    blocks: &[RecallBlock],
    skip: impl Fn(BlockAddress, usize) -> bool,
) -> Vec<RecallStep> {
    let mut ordered = blocks.to_vec();
    ordered.sort_by_key(|b| (b.block.row, b.block.column));
    let mut steps: Vec<RecallStep> = ordered
        .iter()
        .filter_map(|b| {
            let value = b.bypass_value();
            (!b.expression_bypass
                && value != BypassValue::Off
                && RECALL_FIRST_PASS_CATEGORIES.contains(&b.category))
            .then_some(RecallStep::Bypass {
                block: b.block,
                value,
            })
        })
        .collect();
    steps.push(RecallStep::Flush);
    for row in 0..GRID_ROWS as u8 {
        for b in ordered.iter().filter(|b| b.block.row == row) {
            if !b.expression_bypass {
                steps.push(RecallStep::Bypass {
                    block: b.block,
                    value: b.bypass_value(),
                });
            }
            steps.extend(
                parameter_order(b)
                    .filter(|&index| !skip(b.block, index))
                    .map(|index| RecallStep::Parameter {
                        block: b.block,
                        index,
                    }),
            );
        }
        if row % 2 == 0 {
            steps.push(RecallStep::RowIo { row });
        }
    }
    steps
}

/// Parameter indices in send order. Model 25000 sends none; parameter 12 of
/// model 27000 moves last only when it is 0.0.
fn parameter_order(block: &RecallBlock) -> impl Iterator<Item = usize> {
    let count = if block.model_id == NO_PARAMETER_MODEL {
        0
    } else {
        block.parameter_count
    };
    let (late_model, late_index) = LATE_PARAMETER_MODEL;
    let late = (block.model_id == late_model && block.late_parameter_zero && late_index < count)
        .then_some(late_index);
    (0..count)
        .filter(move |&index| Some(index) != late)
        .chain(late)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(index: i32) -> SceneIndex {
        SceneIndex::new(index).unwrap()
    }

    fn recall_block(
        row: u8,
        column: u8,
        model_id: u32,
        category: u32,
        parameter_count: usize,
    ) -> RecallBlock {
        RecallBlock {
            block: BlockAddress { row, column },
            model_id,
            category,
            bypassed: false,
            globally_bypassed: false,
            mute: false,
            expression_bypass: false,
            late_parameter_zero: false,
            parameter_count,
        }
    }

    fn parameters_of(steps: &[RecallStep], block: BlockAddress) -> Vec<usize> {
        steps
            .iter()
            .filter_map(|step| match step {
                RecallStep::Parameter { block: b, index } if *b == block => Some(*index),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn edits_follow_scene_mode_and_disable_copies_the_default_scene() {
        let mut gain = Scened::shared(0.5f32);
        gain.edit(scene(3), 0.7);
        assert_eq!(gain.values, [0.7; SCENES]);
        gain.enable_scene_mode();
        gain.edit(scene(3), 0.9);
        assert_eq!(*gain.value(scene(3)), 0.9);
        assert_eq!(*gain.value(scene(0)), 0.7);
        gain.disable_scene_mode(scene(3));
        assert_eq!(gain.values, [0.9; SCENES]);
        assert!(!gain.scene_mode);
    }

    #[test]
    fn bypass_import_sets_scene_mode_from_flag_or_differences() {
        let same = [false; SCENES];
        assert!(!Scened::imported_with_differences(None, same).scene_mode);
        assert!(Scened::imported_with_differences(Some(true), same).scene_mode);
        let mut differ = same;
        differ[5] = true;
        assert!(Scened::imported_with_differences(Some(false), differ).scene_mode);
    }

    #[test]
    fn bypass_value_is_off_on_or_mute_from_scene_forced_state_and_mute() {
        let mut block = recall_block(0, 0, 100, 1, 0);
        assert_eq!(block.bypass_value(), BypassValue::Off);
        block.mute = true;
        assert_eq!(block.bypass_value(), BypassValue::Off);
        block.bypassed = true;
        assert_eq!(block.bypass_value(), BypassValue::Mute);
        block.globally_bypassed = true;
        assert_eq!(block.bypass_value(), BypassValue::On);
        assert_eq!(BypassValue::Off.parameter(), 0.0);
        assert_eq!(BypassValue::On.parameter().to_bits(), 0x3f80_0000);
        assert_eq!(BypassValue::Mute.parameter(), 2.0);
    }

    #[test]
    fn recall_turns_on_engaged_blocks_of_the_recall_categories_before_the_flush() {
        let mut lead = recall_block(0, 1, 100, 33, 1);
        lead.bypassed = true;
        let mut other = recall_block(0, 0, 100, 2, 1);
        other.bypassed = true;
        let steps = recall_plan(&[lead, other], |_, _| false);
        assert_eq!(
            steps[0],
            RecallStep::Bypass {
                block: lead.block,
                value: BypassValue::On,
            }
        );
        assert_eq!(steps[1], RecallStep::Flush);
        assert_eq!(
            steps[2],
            RecallStep::Bypass {
                block: other.block,
                value: BypassValue::On,
            }
        );
    }

    #[test]
    fn expression_bypass_blocks_keep_their_bypass_and_send_none() {
        let mut pedal = recall_block(0, 0, 100, 33, 2);
        pedal.bypassed = true;
        pedal.expression_bypass = true;
        let steps = recall_plan(&[pedal], |_, _| false);
        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, RecallStep::Bypass { .. }))
        );
        assert_eq!(parameters_of(&steps, pedal.block), [0, 1]);
    }

    #[test]
    fn row_io_follows_each_even_row_inside_the_loop() {
        let at = |row| BlockAddress { row, column: 0 };
        let blocks = [
            recall_block(0, 0, 100, 2, 1),
            recall_block(1, 0, 100, 2, 1),
            recall_block(2, 0, 100, 2, 1),
        ];
        let steps = recall_plan(&blocks, |_, _| false);
        assert_eq!(
            steps,
            [
                RecallStep::Flush,
                RecallStep::Bypass {
                    block: at(0),
                    value: BypassValue::Off,
                },
                RecallStep::Parameter {
                    block: at(0),
                    index: 0,
                },
                RecallStep::RowIo { row: 0 },
                RecallStep::Bypass {
                    block: at(1),
                    value: BypassValue::Off,
                },
                RecallStep::Parameter {
                    block: at(1),
                    index: 0,
                },
                RecallStep::Bypass {
                    block: at(2),
                    value: BypassValue::Off,
                },
                RecallStep::Parameter {
                    block: at(2),
                    index: 0,
                },
                RecallStep::RowIo { row: 2 },
            ]
        );
    }

    #[test]
    fn model_27000_moves_parameter_12_last_only_when_it_is_zero() {
        let mut block = recall_block(0, 0, 27000, 26, 14);
        let natural: Vec<usize> = (0..14).collect();
        let plan = recall_plan(&[block], |_, _| false);
        assert_eq!(parameters_of(&plan, block.block), natural);
        block.late_parameter_zero = true;
        let mut moved: Vec<usize> = (0..14).filter(|&i| i != 12).collect();
        moved.push(12);
        let plan = recall_plan(&[block], |_, _| false);
        assert_eq!(parameters_of(&plan, block.block), moved);
        let skipped = recall_plan(&[block], |_, index| index == 0);
        assert_eq!(parameters_of(&skipped, block.block), moved[1..].to_vec());
    }

    #[test]
    fn model_25000_sends_its_bypass_but_no_parameters() {
        let mut block = recall_block(0, 0, 25000, 2, 5);
        block.bypassed = true;
        let steps = recall_plan(&[block], |_, _| false);
        assert!(steps.contains(&RecallStep::Bypass {
            block: block.block,
            value: BypassValue::On,
        }));
        assert!(
            steps
                .iter()
                .all(|step| !matches!(step, RecallStep::Parameter { .. }))
        );
    }
}

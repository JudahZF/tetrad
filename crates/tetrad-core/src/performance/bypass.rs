//! Block bypass writes, the bypass-overwrite setting and stomp toggles.
//!
//! Write chain: toggle `0x521514` / set `0x521620` → `0x521170` → general
//! setter `0x51e4d8` (scene fanout) → model setter `0x2e8160`. A model keeps
//! live (`+0x28`) and stored (`+0x34`) values plus committed bypass copies
//! (`+0x40`, `+0x60`). A scene change (`0x5b244c` → `0x2e0e38(preset, 0)`) and
//! a preset load (`0x5280d8` → `0x2e0e38(preset, 1)`) revert live/stored bypass
//! to the committed copies; a save commits them all (`0x5b7438` → `0x51327c` →
//! `0x2e0df8`). Stomp press `0x5239d0`, momentary release `0x523f80`.

use crate::workflow::{BlockAddress, SceneIndex};

use super::scene::SCENES;

/// `ParamChangeSource` values seen on bypass writes. MIDI is not traced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ChangeSource {
    Touch = 0,
    Expression = 1,
    Footswitch = 2,
    Momentary = 3,
}

/// Setting `bypassSaveMode` (`0x3b72a4`/`0x3b7c40`); GeneralSettings field 9.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum BypassOverwriteMode {
    #[default]
    Always = 0,
    /// Footswitch (stomp-mode) changes stay temporary.
    NonStomp = 1,
    Never = 2,
}

impl BypassOverwriteMode {
    /// Whether a write from `source` updates the committed copies (`0x2e8160`): momentary
    /// writes never do, footswitch writes not under NonStomp, nothing under Never.
    pub fn commits(self, source: ChangeSource) -> bool {
        !matches!(
            (source, self),
            (ChangeSource::Momentary, _)
                | (_, Self::Never)
                | (ChangeSource::Footswitch, Self::NonStomp)
        )
    }
}

/// One block's bypass across eight scenes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockBypass {
    pub scene_mode: bool,
    live: [bool; SCENES],
    stored: [bool; SCENES],
    committed_live: [bool; SCENES],
    committed_stored: [bool; SCENES],
}

impl BlockBypass {
    pub fn new(scene_mode: bool, bypassed: [bool; SCENES]) -> Self {
        Self {
            scene_mode,
            live: bypassed,
            stored: bypassed,
            committed_live: bypassed,
            committed_stored: bypassed,
        }
    }

    /// The value sent to the DSP (`isBypassed`: live value > 0.5).
    pub fn is_bypassed(&self, scene: SceneIndex) -> bool {
        self.live[usize::from(scene.get())]
    }

    /// The preset value without expression overrides.
    pub fn stored(&self, scene: SceneIndex) -> bool {
        self.stored[usize::from(scene.get())]
    }

    /// The values a save serializes.
    pub fn committed(&self) -> [bool; SCENES] {
        self.committed_stored
    }

    /// `0x51e4d8` fanout (all scenes unless scene mode, and always for
    /// expression), then the `0x2e8160` bypass branch for each scene.
    pub fn write(
        &mut self,
        scene: SceneIndex,
        bypassed: bool,
        source: ChangeSource,
        mode: BypassOverwriteMode,
    ) {
        let selected = usize::from(scene.get());
        let all = !self.scene_mode || source == ChangeSource::Expression;
        for index in (0..SCENES).filter(|&i| all || i == selected) {
            if source != ChangeSource::Expression {
                self.stored[index] = bypassed;
            }
            self.live[index] = bypassed;
            if mode.commits(source) {
                if source != ChangeSource::Expression {
                    self.committed_stored[index] = bypassed;
                }
                self.committed_live[index] = bypassed;
            }
        }
    }

    /// Scene change (`0x2e0e38(preset, 0)`): uncommitted bypass is discarded
    /// unless the bypass parameter is expression-assigned.
    pub fn revert_on_scene_change(&mut self, expression_assigned: bool) {
        if !expression_assigned {
            self.revert_committed();
        }
    }

    /// Reverts live and stored bypass to the committed copies. A preset load
    /// (`0x2e0e38(preset, 1)`) always calls this.
    pub fn revert_committed(&mut self) {
        self.live = self.committed_live;
        self.stored = self.committed_stored;
    }
}

/// Block categories that are switched off first when a stomp engages
/// several blocks (`0x5239d0`: category < 4, 5 or 14).
pub const FIRST_PASS_CATEGORIES: [u32; 6] = [0, 1, 2, 3, 5, 14];

/// A block assigned to the pressed stomp, as read at press time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AssignedBlock {
    pub block: BlockAddress,
    pub secondary: bool,
    pub category: u32,
    pub bypassed: bool,
    /// Current-scene value of the model's `secondary_stomp_parameter`.
    pub secondary_value: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StompWrite {
    Bypass {
        block: BlockAddress,
        bypassed: bool,
        source: ChangeSource,
    },
    /// `usleep(10000)` after the first pass changed something.
    Settle,
    /// Secondary parameter toggle (`0x5238f8`): a saved touch-source write.
    Secondary { block: BlockAddress, value: f32 },
}

fn secondary_toggle(block: &AssignedBlock) -> Option<StompWrite> {
    block.secondary_value.map(|value| StompWrite::Secondary {
        block: block.block,
        value: if value <= 0.5 { 1.0 } else { 0.0 },
    })
}

/// Stomp press. Pass 1 turns engaged first-pass blocks off (no assignment
/// type check in stock); pass 2 toggles the rest: secondary parameter for
/// SECONDARY, bypass for PRIMARY. Momentary stomps use source 3.
pub fn stomp_press(blocks: &[AssignedBlock], momentary: bool) -> Vec<StompWrite> {
    let source = if momentary {
        ChangeSource::Momentary
    } else {
        ChangeSource::Footswitch
    };
    let first_pass = |b: &AssignedBlock| FIRST_PASS_CATEGORIES.contains(&b.category) && !b.bypassed;
    let mut writes: Vec<StompWrite> = blocks
        .iter()
        .filter(|b| first_pass(b))
        .map(|b| StompWrite::Bypass {
            block: b.block,
            bypassed: true,
            source,
        })
        .collect();
    if !writes.is_empty() {
        writes.push(StompWrite::Settle);
    }
    writes.extend(blocks.iter().filter(|b| !first_pass(b)).filter_map(|b| {
        if b.secondary {
            secondary_toggle(b)
        } else {
            Some(StompWrite::Bypass {
                block: b.block,
                bypassed: !b.bypassed,
                source,
            })
        }
    }));
    writes
}

/// Momentary release: toggle every block back with source 3. Stock skips
/// this in scene copy mode or after a preset load during the hold.
pub fn momentary_release(blocks: &[AssignedBlock]) -> Vec<StompWrite> {
    blocks
        .iter()
        .filter_map(|b| {
            if b.secondary {
                secondary_toggle(b)
            } else {
                Some(StompWrite::Bypass {
                    block: b.block,
                    bypassed: !b.bypassed,
                    source: ChangeSource::Momentary,
                })
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(i: i32) -> SceneIndex {
        SceneIndex::new(i).unwrap()
    }

    #[test]
    fn overwrite_modes_decide_what_survives_a_scene_change() {
        for (mode, survives) in [
            (BypassOverwriteMode::Always, true),
            (BypassOverwriteMode::NonStomp, false),
            (BypassOverwriteMode::Never, false),
        ] {
            let mut block = BlockBypass::new(true, [false; SCENES]);
            block.write(scene(2), true, ChangeSource::Footswitch, mode);
            assert!(block.is_bypassed(scene(2)) && !block.is_bypassed(scene(3)));
            block.revert_on_scene_change(false);
            assert_eq!(block.is_bypassed(scene(2)), survives, "{mode:?}");
        }
        // Touch writes commit under NonStomp; momentary never commits.
        let mut block = BlockBypass::new(false, [false; SCENES]);
        block.write(
            scene(0),
            true,
            ChangeSource::Touch,
            BypassOverwriteMode::NonStomp,
        );
        block.write(
            scene(0),
            false,
            ChangeSource::Momentary,
            BypassOverwriteMode::Always,
        );
        assert!(!block.is_bypassed(scene(5)));
        block.revert_committed();
        assert_eq!(block.committed(), [true; SCENES]);
        // Expression writes live values in every scene, not the stored ones.
        let mut block = BlockBypass::new(true, [false; SCENES]);
        block.write(
            scene(1),
            true,
            ChangeSource::Expression,
            BypassOverwriteMode::Always,
        );
        assert!(block.is_bypassed(scene(7)) && !block.stored(scene(7)));
    }

    #[test]
    fn scene_change_keeps_expression_assigned_bypass_while_save_reverts() {
        let mut block = BlockBypass::new(false, [false; SCENES]);
        block.write(
            scene(0),
            true,
            ChangeSource::Footswitch,
            BypassOverwriteMode::Never,
        );
        block.revert_on_scene_change(true);
        assert!(block.is_bypassed(scene(0)));
        block.revert_on_scene_change(false);
        assert!(!block.is_bypassed(scene(0)));
        block.write(
            scene(0),
            true,
            ChangeSource::Footswitch,
            BypassOverwriteMode::Never,
        );
        block.revert_committed();
        assert!(!block.is_bypassed(scene(0)));
    }

    #[test]
    fn stomp_press_switches_first_pass_blocks_off_before_toggling_the_rest() {
        let at = |column| BlockAddress { row: 0, column };
        let block = |column, category, bypassed, secondary| AssignedBlock {
            block: at(column),
            secondary,
            category,
            bypassed,
            secondary_value: Some(0.2),
        };
        let blocks = [
            block(0, 7, true, false),
            block(1, 2, false, false),
            block(2, 9, false, true),
        ];
        let footswitch = ChangeSource::Footswitch;
        assert_eq!(
            stomp_press(&blocks, false),
            [
                StompWrite::Bypass {
                    block: at(1),
                    bypassed: true,
                    source: footswitch
                },
                StompWrite::Settle,
                StompWrite::Bypass {
                    block: at(0),
                    bypassed: false,
                    source: footswitch
                },
                StompWrite::Secondary {
                    block: at(2),
                    value: 1.0
                },
            ]
        );
        let released = momentary_release(&blocks[..1]);
        assert_eq!(
            released,
            [StompWrite::Bypass {
                block: at(0),
                bypassed: false,
                source: ChangeSource::Momentary
            }]
        );
    }
}

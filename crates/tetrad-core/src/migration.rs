use crate::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct MigrationFlags {
    pub side_chain_follow_exists: bool,
    pub zsk_7182_applied: bool,
}

/// Positional occupancy for the reviewed 4×8 import path only. Mixer `None`
/// means the recovered negative-mixer case, not an inferred routing topology.
#[derive(Debug, Clone)]
pub struct SourceLayout {
    models: [[u32; 8]; 4],
    mixer_columns: [Option<usize>; 2],
}

impl SourceLayout {
    pub fn new(models: [[u32; 8]; 4], mixer_columns: [Option<usize>; 2]) -> Result<Self> {
        for column in mixer_columns.into_iter().flatten() {
            if column >= 8 {
                return Err(Error::Mixer(column));
            }
        }
        Ok(Self {
            models,
            mixer_columns,
        })
    }

    pub fn preceding_sources(&self, row: usize, column: usize) -> Result<u32> {
        self.check_position(row, column)?;
        let count = |slots: &[u32]| slots.iter().filter(|&&id| id != 0).count() as u32;
        if row.is_multiple_of(2) {
            Ok(self.models[..row]
                .iter()
                .map(|slots| count(slots))
                .sum::<u32>()
                + count(&self.models[row][..column]))
        } else {
            let paired = row - 1;
            let upper_end = self.mixer_columns[row / 2].map_or(8, |mixer| mixer + 1);
            Ok(self.models[..paired]
                .iter()
                .map(|slots| count(slots))
                .sum::<u32>()
                + count(&self.models[paired][..upper_end])
                + count(&self.models[row][..column]))
        }
    }

    /// Runs recovered passes in order on one sidechain parameter. Callers must
    /// select the XML-designated parameter. This does not mark the whole preset
    /// migrated: flags may change only after all relevant models are processed.
    pub fn migrate_scenes(
        &self,
        row: usize,
        column: usize,
        scenes: [f32; 8],
        flags: MigrationFlags,
    ) -> Result<[f32; 8]> {
        let sources = self.preceding_sources(row, column)?;
        let pass_two = !flags.zsk_7182_applied
            && !row.is_multiple_of(2)
            && self.mixer_columns[row / 2].is_some()
            && self.models[row - 1][0] != 0;
        let mut migrated = scenes;
        for value in &mut migrated {
            if !flags.side_chain_follow_exists {
                *value = migrate_value(*value, sources, false)?;
            }
            if pass_two {
                *value = migrate_value(*value, sources, true)?;
            }
        }
        Ok(migrated)
    }

    fn check_position(&self, row: usize, column: usize) -> Result<()> {
        if row >= 4 || column >= 8 {
            return Err(Error::Position { row, column });
        }
        Ok(())
    }
}

fn migrate_value(value: f32, sources: u32, second: bool) -> Result<f32> {
    if !value.is_finite() {
        return Err(Error::NonFinite);
    }
    let decoded = (value * (sources + 12) as f32).trunc();
    if !decoded.is_finite() || decoded >= i32::MAX as f32 || decoded < i32::MIN as f32 {
        return Err(Error::SidechainRange);
    }
    let index = decoded as i32;
    if index <= 0 {
        return Ok(value);
    }
    // Keep separate f32 operations; pass one multiplies a rounded reciprocal.
    Ok(if second {
        index as f32 / (sources + 13) as f32
    } else {
        (1.0_f32 / (sources + 13) as f32) * (index + 1) as f32
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_positional_sources_with_mixer_inclusive_and_empty_slots() {
        let mut models = [[0; 8]; 4];
        models[0] = [1, 0, 2, 3, 0, 4, 0, 0];
        models[1] = [5, 0, 6, 0, 0, 0, 0, 0];
        models[2] = [7, 8, 0, 9, 0, 0, 0, 0];
        let layout = SourceLayout::new(models, [Some(2), None]).unwrap();
        assert_eq!(layout.preceding_sources(0, 3).unwrap(), 2);
        assert_eq!(layout.preceding_sources(1, 3).unwrap(), 4);
        assert_eq!(layout.preceding_sources(2, 3).unwrap(), 8);
        assert_eq!(layout.preceding_sources(3, 0).unwrap(), 9);
        assert!(layout.preceding_sources(4, 0).is_err());
        assert!(layout.preceding_sources(0, 8).is_err());
        assert!(SourceLayout::new(models, [Some(8), None]).is_err());
    }

    #[test]
    fn flags_and_qualifying_odd_rows_keep_float32_pass_order() {
        let mut models = [[0; 8]; 4];
        models[0][..4].fill(1);
        let layout = SourceLayout::new(models, [Some(3), None]).unwrap();
        let scenes = [0.5; 8];
        let first = layout
            .migrate_scenes(
                1,
                0,
                scenes,
                MigrationFlags {
                    side_chain_follow_exists: false,
                    zsk_7182_applied: true,
                },
            )
            .unwrap();
        let both = layout
            .migrate_scenes(
                1,
                0,
                scenes,
                MigrationFlags {
                    side_chain_follow_exists: false,
                    zsk_7182_applied: false,
                },
            )
            .unwrap();
        let second = layout
            .migrate_scenes(
                1,
                0,
                scenes,
                MigrationFlags {
                    side_chain_follow_exists: true,
                    zsk_7182_applied: false,
                },
            )
            .unwrap();
        assert_eq!(first[0].to_bits(), 0x3f07_8788);
        assert_eq!(both[0].to_bits(), 0x3ef0_f0f1);
        assert_eq!(second[0].to_bits(), 0x3ef0_f0f1);
        assert_eq!(
            layout
                .migrate_scenes(
                    1,
                    0,
                    scenes,
                    MigrationFlags {
                        side_chain_follow_exists: true,
                        zsk_7182_applied: true
                    }
                )
                .unwrap(),
            scenes
        );
        let small = layout
            .migrate_scenes(
                1,
                0,
                [0.0625; 8],
                MigrationFlags {
                    side_chain_follow_exists: false,
                    zsk_7182_applied: false,
                },
            )
            .unwrap();
        assert_eq!(small[0].to_bits(), 0x3d70_f0f1);
    }

    #[test]
    fn skipped_passes_preserve_bits_and_failed_passes_leave_inputs_unchanged() {
        let flags = MigrationFlags {
            side_chain_follow_exists: true,
            zsk_7182_applied: false,
        };
        let mut models = [[0; 8]; 4];
        models[0][1] = 1;
        let layout = SourceLayout::new(models, [Some(3), None]).unwrap();
        let nan = f32::from_bits(0x7fc0_1234);
        assert_eq!(
            layout.migrate_scenes(1, 0, [nan; 8], flags).unwrap()[0].to_bits(),
            nan.to_bits()
        );
        let zero = SourceLayout::new([[0; 8]; 4], [None; 2]).unwrap();
        let scenes = [0.0625, -0.0, -0.5, 0.0, 0.01, 0.01, 0.01, 0.01];
        let migrated = zero
            .migrate_scenes(
                0,
                0,
                scenes,
                MigrationFlags {
                    side_chain_follow_exists: false,
                    zsk_7182_applied: false,
                },
            )
            .unwrap();
        for (before, after) in scenes.into_iter().zip(migrated) {
            assert_eq!(before.to_bits(), after.to_bits());
        }
        assert_eq!(
            zero.migrate_scenes(
                0,
                0,
                [nan; 8],
                MigrationFlags {
                    side_chain_follow_exists: false,
                    zsk_7182_applied: false
                }
            ),
            Err(Error::NonFinite)
        );
    }
}

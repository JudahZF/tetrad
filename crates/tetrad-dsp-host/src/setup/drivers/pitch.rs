//! Pitch drivers. Each converts or replaces values on the ARM and keeps the
//! base hooks otherwise.
//!
//! | Driver | Models | Constructor | `+0x30` |
//! | --- | --- | --- | --- |
//! | TransposeDriver | 18001, 18010 | 0x566fb4 | 0x566da4 |
//! | MishaLaserDriver | 18018 | 0x55f4a4 | 0x55f74c, preset 0x55f52c |
//! | MishaOctaverDriver | 18019 | 0x55f8a8 | 0x55f820 |
//! | GojiraWOWDriver | 18009 | 0x566d24 | 0x566b48 |

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{DspWrite, ModelIdentity, ParamSetMode, StockPayload, StockTables, TableError};
use super::catalog::{to_display, to_normalized};
use super::reload::{payload, words};
use super::{Driver, DriverKind};

#[derive(Debug)]
pub(super) struct Transpose {
    target: DeploymentTarget,
    /// 18010 Transpose; else 18001 Pitch Shifter.
    transpose: bool,
    /// `+0x24`, `+0x28`, `+0x2c` (100.0 at construction).
    coarse: f32,
    fine: f32,
    mix: f32,
}

impl Transpose {
    pub(super) fn new(identity: &ModelIdentity) -> Self {
        Self {
            target: identity.target,
            transpose: identity.model_id == 18010,
            coarse: 0.0,
            fine: 0.0,
            mix: 100.0,
        }
    }
}

impl Driver for Transpose {
    fn kind(&self) -> DriverKind {
        DriverKind::Transpose
    }

    /// 18001 mutes MIX while COARSE and FINE both round to zero; 18010 maps
    /// SEMITONES (±12) onto COARSE (±24) and MIX onto "shifting".
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let target = self.target;
        let mut send = |index, value| out.push(DspWrite::parameter(target, index, value));
        if self.transpose {
            return match index {
                0 | 1 => false,
                3 => {
                    let semitones = to_display(value, -12.0, 12.0, 1.0).round();
                    send(1, to_normalized(semitones, -24.0, 24.0, 1.0));
                    send(0, if semitones != 0.0 { 1.0 } else { 0.0 });
                    false
                }
                _ => true,
            };
        }
        match index {
            0 => self.mix = value,
            1 => self.coarse = value,
            2 => self.fine = value,
            _ => return false,
        }
        let coarse = to_display(self.coarse, -24.0, 24.0, 1.0).round();
        let fine = to_display(self.fine, -100.0, 100.0, 1.0).round();
        // NaN counts as shifted (`vcmp`/`bne`).
        let silent = fine == 0.0 && coarse == 0.0;
        send(0, if silent { 0.0 } else { self.mix });
        index != 0
    }
}

/// 19 Laser presets of 0x28 bytes (`0xa6aff8`): pitch mode word, ring-track
/// byte, then ring heel/toe 1 (0..2000), ring heel/toe 2 (0.25..4) and pitch
/// heel/toe 1 and 2 (±24) as f32.
const LASER_PRESETS: StockPayload = payload!(0x00a6_aff8 => PRESET_BYTES * PRESETS as u32);
const PRESET_BYTES: u32 = 0x28;
const PRESETS: usize = 19;

pub(super) fn payloads() -> impl Iterator<Item = StockPayload> {
    std::iter::once(LASER_PRESETS)
}

/// One Laser preset as stock sends it: indices 0, 2, then 4..=11.
#[derive(Clone, Copy, PartialEq)]
struct Preset([f32; 10]);

/// Stock-derived values; `Debug` never prints them.
impl std::fmt::Debug for Preset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<stock preset>")
    }
}

impl Preset {
    fn read(bytes: &[u8]) -> Self {
        let word: Vec<u32> = words(bytes).collect();
        let on = |set: bool| if set { 1.0 } else { 0.0 };
        let float = |at: usize| f32::from_bits(word[at]);
        let mut values = [0.0; 10];
        values[0] = on(word[0] != 0);
        values[1] = on(bytes[4] != 0);
        for slot in 0..2 {
            values[2 + slot] = to_normalized(float(2 + slot), 0.0, 2000.0, 1.0);
            values[4 + slot] = to_normalized(float(4 + slot), 0.25, 4.0, 1.0);
        }
        for slot in 0..4 {
            values[6 + slot] = to_normalized(float(6 + slot), -24.0, 24.0, 1.0);
        }
        Self(values)
    }
}

const PEDAL: u32 = 3;
const PRESET: u32 = 15;
const TRIGGER: u32 = 16;

#[derive(Debug)]
pub(super) struct Laser {
    target: DeploymentTarget,
    /// `+0x24`.
    pedal: f32,
    /// `+0x28`: TRIGGER holds PEDAL at 1.0.
    triggered: bool,
    presets: Box<[Preset]>,
}

impl Laser {
    pub(super) fn new(
        identity: &ModelIdentity,
        tables: &impl StockTables,
    ) -> Result<Self, TableError> {
        let bytes = tables.read(LASER_PRESETS)?;
        if bytes.len() != LASER_PRESETS.len() {
            return Err(TableError::Malformed);
        }
        Ok(Self {
            target: identity.target,
            pedal: 0.0,
            triggered: false,
            presets: bytes
                .chunks_exact(PRESET_BYTES as usize)
                .map(Preset::read)
                .collect(),
        })
    }
}

impl Driver for Laser {
    fn kind(&self) -> DriverKind {
        DriverKind::MishaLaser
    }

    /// PRESET owns the hidden M, RING TRACK and heel/toe values; nothing
    /// else reaches them.
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            1 | 12..=14 => true,
            PEDAL => {
                self.pedal = value;
                !self.triggered
            }
            PRESET => {
                // `lroundf(v·18)`; stock does not bound the row (owned clamp).
                let row = ((value * 18.0).round() as i32).clamp(0, PRESETS as i32 - 1);
                let preset = self.presets[row as usize];
                let indices = [0, 2, 4, 5, 6, 7, 8, 9, 10, 11];
                out.extend(
                    indices
                        .into_iter()
                        .zip(preset.0)
                        .map(|(index, value)| DspWrite::parameter(self.target, index, value)),
                );
                false
            }
            TRIGGER => {
                self.triggered = value != 0.0;
                let pedal = if self.triggered { 1.0 } else { self.pedal };
                out.push(DspWrite::parameter(self.target, PEDAL, pedal));
                false
            }
            _ => false,
        }
    }
}

/// MishaOctaverDriver: every value goes out as its square root; OCT −1
/// also by 1/√2 (`0x3f3504f3`).
#[derive(Debug)]
pub(super) struct Octaver(DeploymentTarget);

impl Octaver {
    pub(super) fn new(identity: &ModelIdentity) -> Self {
        Self(identity.target)
    }
}

impl Driver for Octaver {
    fn kind(&self) -> DriverKind {
        DriverKind::MishaOctaver
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let root = value.sqrt();
        let value = if index == 2 {
            root * std::f32::consts::FRAC_1_SQRT_2
        } else {
            root
        };
        out.push(DspWrite::parameter(self.0, index, value));
        false
    }
}

const FATSO_MIX: u32 = 0;
const WOW_MODE: u32 = 11;

/// GojiraWOWDriver: MODE (Blade 2, Blade 1, FATSO) writes the hidden pitch,
/// blend and level values; FATSO MIX passes and is kept for FATSO.
#[derive(Debug)]
pub(super) struct GojiraWow {
    target: DeploymentTarget,
    /// `+0x24`, 1.0 at construction.
    mix: f32,
}

impl GojiraWow {
    pub(super) fn new(identity: &ModelIdentity) -> Self {
        Self {
            target: identity.target,
            mix: 1.0,
        }
    }
}

impl Driver for GojiraWow {
    fn kind(&self) -> DriverKind {
        DriverKind::GojiraWow
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            FATSO_MIX => {
                self.mix = value;
                true
            }
            WOW_MODE => {
                // `vcvt.u32`: truncates; negative and NaN give 0.
                let writes = match to_display(value, 0.0, 2.0, 1.0) as u32 {
                    0 => [(7, 1.0), (2, 0.5), (3, 1.0), (6, 0.0), (0, 1.0)],
                    1 => [(7, 1.0), (2, 0.5), (3, 0.75), (6, 0.0), (0, 1.0)],
                    2 => [(7, 1.0), (2, 0.25), (3, 0.75), (6, 0.0), (0, self.mix)],
                    _ => return false,
                };
                out.extend(
                    writes
                        .into_iter()
                        .map(|(index, value)| DspWrite::parameter(self.target, index, value)),
                );
                false
            }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::amps::tests::{Indexed, identity};
    use super::*;

    fn values(writes: &[DspWrite]) -> Vec<(u32, f32)> {
        writes
            .iter()
            .map(|write| match *write {
                DspWrite::Parameter {
                    index, value_bits, ..
                } => (index, f32::from_bits(value_bits)),
                ref other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    fn sent(driver: &mut dyn Driver, index: u32, value: f32) -> (bool, Vec<(u32, f32)>) {
        let mut out = Vec::new();
        let pass = driver.update_parameter(index, value, ParamSetMode::Live, &mut out);
        (pass, values(&out))
    }

    /// `0x566da4`: 18001 sends MIX only while shifted; 18010 rounds
    /// SEMITONES and re-expresses it on the ±24 COARSE range.
    #[test]
    fn transpose_laws() {
        let mut shifter = Transpose::new(&identity(18001));
        // FINE starts at normalized 0 (−100 cents) and MIX at 100.0.
        assert_eq!(sent(&mut shifter, 1, 0.5), (true, vec![(0, 100.0)]));
        assert_eq!(sent(&mut shifter, 2, 0.5), (true, vec![(0, 0.0)]));
        assert_eq!(sent(&mut shifter, 0, 0.7), (false, vec![(0, 0.0)]));
        assert_eq!(sent(&mut shifter, 1, 0.75), (true, vec![(0, 0.7)]));
        let mut transpose = Transpose::new(&identity(18010));
        assert_eq!(
            sent(&mut transpose, 3, 0.75),
            (false, vec![(1, 30.0 / 48.0), (0, 1.0)])
        );
        assert_eq!(sent(&mut transpose, 1, 0.75), (false, vec![]));
        assert_eq!(sent(&mut transpose, 2, 0.75), (true, vec![]));
    }

    /// `0x55f74c`/`0x55f52c` with index rows (word `i` = `i/128`): PRESET 0.5
    /// is row 9 (words 90..=99) and writes ten hidden values in word order;
    /// TRIGGER holds PEDAL at 1.0.
    #[test]
    fn laser_preset_and_trigger() {
        let mut laser = Laser::new(&identity(18018), &Indexed).unwrap();
        let (pass, writes) = sent(&mut laser, PRESET, 0.5);
        assert!(!pass);
        let word = |i: u16| f32::from(i) / 128.0;
        let expected = [
            (0, 1.0),
            (2, 0.0),
            (4, word(92) / 2000.0),
            (5, word(93) / 2000.0),
            (6, (word(94) - 0.25) / 3.75),
            (7, (word(95) - 0.25) / 3.75),
            (8, (word(96) + 24.0) / 48.0),
            (9, (word(97) + 24.0) / 48.0),
            (10, (word(98) + 24.0) / 48.0),
            (11, (word(99) + 24.0) / 48.0),
        ];
        assert_eq!(writes, expected);
        assert_eq!(sent(&mut laser, PEDAL, 0.3), (true, vec![]));
        assert_eq!(sent(&mut laser, TRIGGER, 1.0), (false, vec![(PEDAL, 1.0)]));
        assert_eq!(sent(&mut laser, PEDAL, 0.4), (false, vec![]));
        assert_eq!(sent(&mut laser, TRIGGER, 0.0), (false, vec![(PEDAL, 0.4)]));
        assert_eq!(sent(&mut laser, 4, 0.4), (false, vec![]));
    }

    /// `0x55f820` and `0x566b48`.
    #[test]
    fn octaver_and_wow_laws() {
        let mut octaver = Octaver::new(&identity(18019));
        assert_eq!(sent(&mut octaver, 0, 0.25), (false, vec![(0, 0.5)]));
        assert_eq!(
            sent(&mut octaver, 2, 0.25),
            (false, vec![(2, 0.5 * std::f32::consts::FRAC_1_SQRT_2)])
        );
        let mut wow = GojiraWow::new(&identity(18009));
        assert_eq!(sent(&mut wow, FATSO_MIX, 0.3), (true, vec![]));
        assert_eq!(
            sent(&mut wow, WOW_MODE, 1.0),
            (
                false,
                vec![(7, 1.0), (2, 0.25), (3, 0.75), (6, 0.0), (0, 0.3)]
            )
        );
        assert_eq!(sent(&mut wow, WOW_MODE, 0.6).1[2], (3, 0.75), "Blade 1");
        assert_eq!(sent(&mut wow, WOW_MODE, 0.4).1[2], (3, 1.0), "Blade 2");
        assert_eq!(sent(&mut wow, 5, 0.4), (true, vec![]));
    }
}

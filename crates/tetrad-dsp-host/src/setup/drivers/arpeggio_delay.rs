//! ArpeggioDelayDriver: constructor `0xed3e8`, parameter hook `0xed128`.
//! SCALE selects semitones or intervals for three taps. The factory keeps
//! both conversion ranges at 0..24 and installs no conversion callbacks.

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{DspWrite, ModelIdentity, ParamSetMode};
use super::catalog::{to_display, to_normalized};
use super::{Driver, DriverKind};

const TAP_PARAMETERS: [u32; 3] = [7, 14, 21];

#[derive(Debug)]
pub(super) struct ArpeggioDelay {
    target: DeploymentTarget,
    chromatic: bool,
    intervals: [f32; 3],
    semitones: [f32; 3],
}

impl ArpeggioDelay {
    pub(super) fn new(identity: &ModelIdentity) -> Self {
        Self {
            target: identity.target,
            chromatic: false,
            intervals: [0.5; 3],
            semitones: [0.5; 3],
        }
    }
}

impl Driver for ArpeggioDelay {
    fn kind(&self) -> DriverKind {
        DriverKind::ArpeggioDelay
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            3 => {
                // `roundf` (`0xed1dc`), then a fuzzy zero test that only
                // ±0 passes; NaN fails it.
                self.chromatic = (value * 4.0).round() == 0.0;
                let taps = if self.chromatic {
                    &self.semitones
                } else {
                    &self.intervals
                };
                for (index, &value) in TAP_PARAMETERS.into_iter().zip(taps) {
                    out.push(DspWrite::parameter(self.target, index, value));
                }
                true
            }
            7 | 14 | 21 => {
                let tap = (index / 7 - 1) as usize;
                let shown = to_display(value, 0.0, 24.0, 1.0);
                let value = to_normalized(shown, 0.0, 24.0, 1.0);
                self.intervals[tap] = value;
                if !self.chromatic {
                    out.push(DspWrite::parameter(self.target, index, value));
                }
                false
            }
            6 | 13 | 20 => {
                let tap = ((index + 1) / 7 - 1) as usize;
                // `0xed360` stores tap 3 into +0x78 (tap 2); +0x7c keeps
                // its constructor value. Preserve this stock quirk.
                self.semitones[tap.min(1)] = value;
                if self.chromatic {
                    out.push(DspWrite::parameter(self.target, TAP_PARAMETERS[tap], value));
                }
                false
            }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::tests::{delay_hook, delay_identity};
    use ParamSetMode::{Live, Push, Recall};

    /// SCALE sends the cached semitones (chromatic) or intervals. Input 20
    /// overwrites tap 2's semitone cache (`0xed360`); tap 3's stays 0.5 but
    /// a chromatic input 20 still writes index 21.
    #[test]
    fn arpeggio_scale_and_tap_laws_match_stock() {
        let mut driver = ArpeggioDelay::new(&delay_identity(6031));
        let mut hook = |index, value, mode| delay_hook(&mut driver, index, value, mode);
        for (index, value) in [(6, 0.25), (13, 0.75), (20, 1.0)] {
            assert_eq!(hook(index, value, Push), (false, vec![]));
        }
        let semitones = vec![(7, 0.25), (14, 1.0), (21, 0.5)];
        assert_eq!(hook(3, 0.0, Recall), (true, semitones));
        assert_eq!(hook(20, 0.125, Live), (false, vec![(21, 0.125)]));
        // Intervals are cached through display/normalize 0..24, so 2.0 clamps.
        for (index, value) in [(7, 0.75), (14, 0.25), (21, 2.0)] {
            assert_eq!(hook(index, value, Recall), (false, vec![]));
        }
        // roundf(4 · 0.125) = 1: half rounds away from zero, not chromatic.
        let intervals = vec![(7, 0.75), (14, 0.25), (21, 1.0)];
        assert_eq!(hook(3, 0.125, Live), (true, intervals));
        assert_eq!(hook(6, 0.5, Live), (false, vec![]));
        assert_eq!(hook(14, 0.5, Live), (false, vec![(14, 0.5)]));
    }
}

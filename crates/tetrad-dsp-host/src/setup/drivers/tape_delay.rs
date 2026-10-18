//! TapeDelayDriver: constructor `0x55c2a4`, notification range `0x55c330`,
//! parameter hook `0x55bf18`. Tempo is handled by the DSP's global `0x16`.

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{DspWrite, ModelIdentity, ParamSetMode};
use super::catalog::{to_display, to_normalized};
use super::{Driver, DriverKind};

#[derive(Debug)]
pub(super) struct TapeDelay {
    target: DeploymentTarget,
    model_id: u32,
    notification_end: u8,
}

impl TapeDelay {
    pub(super) fn new(identity: &ModelIdentity, notification_end: u8) -> Self {
        Self {
            target: identity.target,
            model_id: identity.model_id,
            notification_end,
        }
    }
}

impl Driver for TapeDelay {
    fn kind(&self) -> DriverKind {
        DriverKind::TapeDelay {
            range_end: self.notification_end,
        }
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        if index == 0 || index == u32::from(self.notification_end) {
            let end = index != 0;
            let marker = match (mode, end) {
                (ParamSetMode::Live, _) => return false,
                (ParamSetMode::Recall, false) => 0.0,
                (ParamSetMode::Recall, true) => 1.0,
                (ParamSetMode::Push, false) => 2.0,
                (ParamSetMode::Push, true) => 3.0,
            };
            out.push(DspWrite::parameter(self.target, index, marker));
            return false;
        }
        let mut send = |index, value| out.push(DspWrite::parameter(self.target, index, value));
        match (self.model_id, index) {
            (6018 | 6019, 3 | 4) | (6020 | 6021, 3 | 4) => {}
            (6018 | 6019, 27) => {
                let squared = value * value;
                let high = to_display(squared, 100.0, 250.0, 1.0);
                let low = to_display(squared, 1500.0, 15000.0, 1.0);
                send(3, to_normalized(high, 80.0, 800.0, 0.6));
                send(4, to_normalized(low, 800.0, 15000.0, 0.6));
            }
            (6019, 8) => {
                let time = to_display(value, 7.0, 3000.0, 0.6);
                send(8, to_normalized(time, 7.0, 6000.0, 0.6));
            }
            (6020 | 6021, 24) => {
                let high = to_display(value, 300.0, 800.0, 1.0);
                let low = to_display(value, 1000.0, 2500.0, 1.0);
                send(3, to_normalized(high, 80.0, 800.0, 0.6));
                send(4, to_normalized(low, 800.0, 6000.0, 0.6));
            }
            (6020 | 6021, 7) => {
                let time = to_display(value, 50.0, 1000.0, 0.6);
                send(7, to_normalized(time, 7.0, 6000.0, 0.6));
            }
            (6020 | 6021, 2) => send(2, value * 0.947_368),
            _ => return true,
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::tests::{delay_hook, delay_identity};
    use ParamSetMode::{Live, Push, Recall};

    /// Markers depend on the mode and ignore the value; live edits and the
    /// replaced filters send nothing (`0x55bf88`–`0x55c078`).
    #[test]
    fn tape_markers_follow_the_mode() {
        let mut gojira = TapeDelay::new(&delay_identity(6019), 26);
        for (mode, start, end) in [(Push, 2.0, 3.0), (Recall, 0.0, 1.0)] {
            assert_eq!(
                delay_hook(&mut gojira, 0, 0.8, mode),
                (false, vec![(0, start)])
            );
            assert_eq!(
                delay_hook(&mut gojira, 26, 0.2, mode),
                (false, vec![(26, end)])
            );
        }
        for (index, mode) in [(0, Live), (26, Live), (3, Push), (4, Recall)] {
            assert_eq!(delay_hook(&mut gojira, index, 0.5, mode), (false, vec![]));
        }
    }

    /// Plugin controls map into the shared tape kernel ranges. Interior
    /// values make every range, skew, the TONE square and the feedback
    /// scale count. Only 6019 remaps its time.
    #[test]
    fn tape_plugin_laws_match_stock() {
        let law = |u, [min, max, skew, to_min, to_max]: [f32; 5]| {
            to_normalized(to_display(u, min, max, skew), to_min, to_max, 0.6)
        };
        let u = 0.3;
        let mut gojira = TapeDelay::new(&delay_identity(6019), 26);
        // TONE squares 0.5 to 0.25 first.
        let tone = vec![
            (3, law(0.25, [100.0, 250.0, 1.0, 80.0, 800.0])),
            (4, law(0.25, [1500.0, 15000.0, 1.0, 800.0, 15000.0])),
        ];
        assert_eq!(delay_hook(&mut gojira, 27, 0.5, Live), (false, tone));
        let time = vec![(8, law(u, [7.0, 3000.0, 0.6, 7.0, 6000.0]))];
        assert_eq!(delay_hook(&mut gojira, 8, u, Live), (false, time));
        let mut plini = TapeDelay::new(&delay_identity(6018), 26);
        assert_eq!(delay_hook(&mut plini, 8, u, Live), (true, vec![]));

        let mut nolly = TapeDelay::new(&delay_identity(6021), 23);
        let tone = vec![
            (3, law(u, [300.0, 800.0, 1.0, 80.0, 800.0])),
            (4, law(u, [1000.0, 2500.0, 1.0, 800.0, 6000.0])),
        ];
        assert_eq!(delay_hook(&mut nolly, 24, u, Recall), (false, tone));
        let time = vec![(7, law(u, [50.0, 1000.0, 0.6, 7.0, 6000.0]))];
        assert_eq!(delay_hook(&mut nolly, 7, u, Recall), (false, time));
        // The stock literal word at `0x55c268`.
        let feedback = vec![(2, u * f32::from_bits(0x3f72_86b6))];
        assert_eq!(delay_hook(&mut nolly, 2, u, Recall), (false, feedback));
        assert_eq!(delay_hook(&mut nolly, 3, u, Recall), (false, vec![]));
    }
}

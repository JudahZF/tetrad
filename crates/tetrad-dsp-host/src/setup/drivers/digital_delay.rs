//! DigitalDelayDriver: constructor `0x55ab20`, parameter hook `0x55a64c`.
//! SYNC and NOTE reach the DSP unchanged; BPM arrives through global `0x16`.

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{DspWrite, ModelIdentity, ParamSetMode};
use super::catalog::{to_display, to_normalized};
use super::{Driver, DriverKind};

#[derive(Debug)]
pub(super) struct DigitalDelay {
    target: DeploymentTarget,
    model_id: u32,
    modulation: f32,
}

impl DigitalDelay {
    pub(super) fn new(identity: &ModelIdentity) -> Self {
        Self {
            target: identity.target,
            model_id: identity.model_id,
            modulation: 0.0,
        }
    }
}

impl Driver for DigitalDelay {
    fn kind(&self) -> DriverKind {
        DriverKind::DigitalDelay
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let mut send = |index, value| out.push(DspWrite::parameter(self.target, index, value));
        match (self.model_id, index) {
            (6005 | 6011, 2) => {
                let high = to_display(value, 20.0, 800.0, 0.6);
                send(2, to_normalized(high, 20.0, 2000.0, 0.6));
            }
            (6005 | 6011, 3) => {
                let low = to_display(value, 1000.0, 16000.0, 0.6);
                send(3, to_normalized(low, 800.0, 16000.0, 0.6));
            }
            (6022 | 6023, 2 | 3) | (6024, 10) | (6032, 4) => {}
            (6022 | 6023, 21) => {
                // `bpl` (VAs `0x55a76c`, `0x55a984`): NaN takes the upper branch.
                let (high, low) = if value < 0.75 {
                    (
                        80.0,
                        (value / 0.75).powf(1.666_666_6).mul_add(9200.0, 800.0),
                    )
                } else {
                    let upper = (value - 0.75) * 4.0;
                    ((upper * upper).mul_add(1920.0, 80.0), 10000.0)
                };
                send(2, to_normalized(high, 20.0, 2000.0, 0.6));
                send(3, to_normalized(low, 800.0, 16000.0, 0.6));
            }
            (6022, 7) => {
                let time = to_display(value, 16.0, 1500.0, 0.6);
                send(7, to_normalized(time, 7.0, 6000.0, 0.6));
            }
            (6024, 7) => {
                let time = to_display(value, 21.0, 3000.0, 1.0);
                send(7, to_normalized(time, 7.0, 6000.0, 0.6));
            }
            (6024, 4) => {
                send(10, self.modulation * 0.7);
                return true;
            }
            (6024, 21) => {
                self.modulation = value;
                send(10, value * 0.7);
                send(8, value + 0.1);
                send(9, value * 0.45);
            }
            (6032, 7) => {
                let time = to_display(value, 7.0, 3000.0, 0.6);
                send(7, to_normalized(time, 7.0, 6000.0, 0.6));
            }
            (6032, 21) => {
                let depth = to_display(value, 0.0, 1.0, 0.7) * 25.0;
                send(12, to_normalized(depth, 0.0, 60.0, 0.6));
                send(18, to_display(value, 0.2, 0.8, 0.7));
            }
            (6032, 22) => {
                let mode = value + value;
                send(21, if mode == 1.0 { 1.0 } else { 0.0 });
                send(4, if mode == 2.0 { 1.0 } else { 0.0 });
            }
            _ => return true,
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::tests::{delay_hook, delay_identity};

    fn sent(model: u32, index: u32, value: f32) -> (bool, Vec<(u32, f32)>) {
        let mut driver = DigitalDelay::new(&delay_identity(model));
        delay_hook(&mut driver, index, value, ParamSetMode::Live)
    }

    /// `0x55a64c` laws at interior values, so every range, skew and factor
    /// counts. Only 6022 remaps its time; 6023 keeps the catalog range.
    #[test]
    fn digital_controls_match_stock_laws() {
        let u = 0.3;
        // display(min, max, skew), then normalized(to_min, to_max, 0.6).
        for (model, index, [min, max, skew, to_min, to_max]) in [
            (6005, 2, [20.0, 800.0, 0.6, 20.0, 2000.0]),
            (6011, 3, [1000.0, 16000.0, 0.6, 800.0, 16000.0]),
            (6022, 7, [16.0, 1500.0, 0.6, 7.0, 6000.0]),
            (6024, 7, [21.0, 3000.0, 1.0, 7.0, 6000.0]),
            (6032, 7, [7.0, 3000.0, 0.6, 7.0, 6000.0]),
        ] {
            let law = to_normalized(to_display(u, min, max, skew), to_min, to_max, 0.6);
            assert_eq!(sent(model, index, u), (false, vec![(index, law)]));
        }
        let tone = |high, low| {
            vec![
                (2, to_normalized(high, 20.0, 2000.0, 0.6)),
                (3, to_normalized(low, 800.0, 16000.0, 0.6)),
            ]
        };
        // 0.375/0.75 = 0.5 below the split; (0.875 − 0.75)·4 = 0.5 above it.
        let low = 0.5_f32.powf(1.666_666_6).mul_add(9200.0, 800.0);
        let modulation = vec![(10, u * 0.7), (8, u + 0.1), (9, u * 0.45)];
        let ducker = to_normalized(to_display(u, 0.0, 1.0, 0.7) * 25.0, 0.0, 60.0, 0.6);
        let ducker = vec![(12, ducker), (18, to_display(u, 0.2, 0.8, 0.7))];
        for (model, index, value, pass, writes) in [
            (6023, 7, u, true, vec![]),
            (6022, 3, u, false, vec![]),
            (6023, 21, 0.375, false, tone(80.0, low)),
            (6022, 21, 0.875, false, tone(560.0, 10000.0)),
            (6024, 21, u, false, modulation),
            (6024, 10, u, false, vec![]),
            (6032, 21, u, false, ducker),
            (6032, 4, u, false, vec![]),
            (6032, 22, 0.0, false, vec![(21, 0.0), (4, 0.0)]),
            (6032, 22, 0.5, false, vec![(21, 1.0), (4, 0.0)]),
            (6032, 22, 1.0, false, vec![(21, 0.0), (4, 1.0)]),
        ] {
            let expected = (pass, writes);
            assert_eq!(sent(model, index, value), expected, "{model}/{index}");
        }
        // NaN fails `< 0.75` like the stock `bpl`: the upper tone branch.
        let (pass, writes) = sent(6023, 21, f32::NAN);
        assert!(!pass && writes[0].1.is_nan());
        assert_eq!(writes[1], (3, to_normalized(10000.0, 800.0, 16000.0, 0.6)));
    }

    /// 6024 PING PONG re-sends WIDTH from the cached MOD (0 at construction).
    #[test]
    fn nolly_ping_pong_resends_width_from_mod() {
        let mut driver = DigitalDelay::new(&delay_identity(6024));
        let live = ParamSetMode::Live;
        assert_eq!(
            delay_hook(&mut driver, 4, 1.0, live),
            (true, vec![(10, 0.0)])
        );
        delay_hook(&mut driver, 21, 0.5, live);
        assert_eq!(
            delay_hook(&mut driver, 4, 0.0, live),
            (true, vec![(10, 0.35)])
        );
    }
}

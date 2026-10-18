//! Stateful amp drivers. VictoryKrakenDriver: constructor VA `0x57cb74`,
//! `+0x30` `0x57caf4`, timer slot `parameterChangeTimerSlot` `0x57cd28`
//! (metacall `0x9b20f8`); every other hook is the base one.
//!
//! The channel switch (index 9) never reaches the DSP directly: the driver
//! raises index 11, waits 20 ms on a single-shot QTimer, then uploads the
//! channel's tables as buffer 0 and sends the switch.

use std::time::Duration;

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::tables::{Segment, StockPayload};
use super::super::{DspWrite, ModelIdentity, ParamSetMode};
use super::{Driver, DriverKind};

/// Buffer 1 of models 1157 and 1185, sent by the constructor.
const KRAKEN_BUFFER1: StockPayload = StockPayload::new(&[
    Segment {
        va: 0x00f5_7c50,
        len: 0x3e44,
    },
    Segment {
        va: 0x00f5_ba94,
        len: 0x900,
    },
]);
/// Buffer 0 per switch state (literal pool `0x57ce14`).
const KRAKEN_ON: StockPayload = StockPayload::new(&[
    Segment {
        va: 0x00f5_c394,
        len: 0x5ea4,
    },
    Segment {
        va: 0x00f6_2238,
        len: 0xa00,
    },
]);
const KRAKEN_OFF: StockPayload = StockPayload::new(&[
    Segment {
        va: 0x00f6_2c38,
        len: 0x5ea4,
    },
    Segment {
        va: 0x00f6_8adc,
        len: 0xa00,
    },
]);
/// Constructor fields `+0x2c` and `+0x30`.
const KRAKEN_SWITCH: u32 = 9;
const KRAKEN_RELOAD: u32 = 11;
/// `QTimer::start(20)` at `0x57cb50`.
const KRAKEN_DELAY: Duration = Duration::from_millis(20);

/// Every amp table, for [`super::stock_payloads`].
pub(super) fn payloads() -> impl Iterator<Item = StockPayload> {
    [KRAKEN_BUFFER1, KRAKEN_ON, KRAKEN_OFF].into_iter()
}

#[derive(Debug)]
pub(super) struct VictoryKraken {
    target: DeploymentTarget,
    /// `+0x28`, starts false.
    switch: bool,
    timer: Option<Duration>,
}

impl VictoryKraken {
    pub(super) fn new(identity: &ModelIdentity, out: &mut Vec<DspWrite>) -> Self {
        if matches!(identity.model_id, 1157 | 1185) {
            out.push(DspWrite::StockData {
                target: identity.target,
                buffer: 1,
                payload: KRAKEN_BUFFER1,
            });
        }
        Self {
            target: identity.target,
            switch: false,
            timer: None,
        }
    }
}

impl Driver for VictoryKraken {
    fn kind(&self) -> DriverKind {
        DriverKind::VictoryKraken
    }

    /// The push (mode 2) always restarts the switch sequence; edits only on
    /// a change.
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        if index != KRAKEN_SWITCH {
            return true;
        }
        let switch = value > 0.5;
        if mode == ParamSetMode::Push || switch != self.switch {
            self.switch = switch;
            out.push(DspWrite::parameter(self.target, KRAKEN_RELOAD, 1.0));
            self.timer = Some(KRAKEN_DELAY);
        }
        false
    }

    fn take_timer(&mut self) -> Option<Duration> {
        self.timer.take()
    }

    fn timer_elapsed(&mut self, out: &mut Vec<DspWrite>) {
        out.push(DspWrite::StockData {
            target: self.target,
            buffer: 0,
            payload: if self.switch { KRAKEN_ON } else { KRAKEN_OFF },
        });
        let switch = if self.switch { 1.0 } else { 0.0 };
        out.push(DspWrite::parameter(self.target, KRAKEN_SWITCH, switch));
    }
}

#[cfg(test)]
mod tests {
    use tetrad_dsp_protocol::{DspCore, ModuleSlot};

    use super::super::super::{BlockBypass, SetupValue};
    use super::*;
    use crate::setup::test_factory::build;

    /// Push restarts the switch sequence; the slot uploads the channel table
    /// and only then sends index 9; an unchanged live edit does nothing.
    #[test]
    fn kraken_switch_waits_for_its_timer() {
        let identity = ModelIdentity {
            target: DeploymentTarget {
                core: DspCore::Core0,
                slot: ModuleSlot::try_from(8).unwrap(),
            },
            model_id: 1157,
            category: 1,
            fx_loop_send: None,
        };
        let target = identity.target;
        let mut out = Vec::new();
        let mut block = build(identity, &mut out).unwrap();
        assert_eq!(
            out,
            [DspWrite::StockData {
                target,
                buffer: 1,
                payload: KRAKEN_BUFFER1,
            }]
        );
        out.clear();
        let mut values = vec![SetupValue::Float(0.5_f32.to_bits()); 12];
        values[9] = SetupValue::Float(1.0_f32.to_bits());
        block
            .push(&values, BlockBypass::default(), None, &mut out)
            .unwrap();
        assert!(
            !out.iter()
                .any(|write| matches!(write, DspWrite::Parameter { index: 9, .. }))
        );
        assert!(out.contains(&DspWrite::parameter(target, 11, 1.0)));
        assert_eq!(block.take_timer(), Some(KRAKEN_DELAY));
        assert_eq!(block.take_timer(), None);
        out.clear();
        block.timer_elapsed(&mut out);
        assert_eq!(
            out,
            [
                DspWrite::StockData {
                    target,
                    buffer: 0,
                    payload: KRAKEN_ON,
                },
                DspWrite::parameter(target, 9, 1.0),
            ]
        );
        out.clear();
        block
            .edit(
                9,
                SetupValue::Float(0.9_f32.to_bits()),
                ParamSetMode::Live,
                &mut out,
            )
            .unwrap();
        assert!(out.is_empty());
        assert_eq!(block.take_timer(), None);
    }
}

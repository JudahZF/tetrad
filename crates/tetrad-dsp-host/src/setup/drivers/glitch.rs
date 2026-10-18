//! GlitchDriver (26005): constructor `0xed6f4`, `+0x30` `0xed65c`, timer
//! slot `0xed7f0`. A MODE change outside the push mutes MIX, then sends MODE
//! and the latest MIX back, 50 ms apart.

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{DspWrite, ModelIdentity, ParamSetMode};
use super::reload::Timer;
use super::{Driver, DriverKind};

const MODE: u32 = 0;
const MIX: u32 = 3;
/// `QTimer::start(50)` at 0xed6e4 and 0xed830.
const STEP_MS: u64 = 50;

/// Stock `+0x2c`: 0, 1, 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    SendMode,
    SendMix,
    Idle,
}

#[derive(Debug)]
pub(super) struct Glitch {
    target: DeploymentTarget,
    /// `+0x24`, 50.0 at construction.
    mix: f32,
    /// `+0x28`, 3.0 at construction.
    mode: f32,
    stage: Stage,
    timer: Timer,
}

impl Glitch {
    pub(super) fn new(identity: &ModelIdentity) -> Self {
        Self {
            target: identity.target,
            mix: 50.0,
            mode: 3.0,
            stage: Stage::Idle,
            timer: Timer::default(),
        }
    }
}

impl Driver for Glitch {
    fn kind(&self) -> DriverKind {
        DriverKind::Glitch
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            // MIX is held back while a MODE change is in flight.
            MIX => {
                self.mix = value;
                self.stage == Stage::Idle
            }
            MODE => {
                let changed = mode != ParamSetMode::Push && value != self.mode;
                self.mode = value;
                if changed {
                    self.stage = Stage::SendMode;
                    out.push(DspWrite::parameter(self.target, MIX, 0.0));
                    self.timer.start(STEP_MS);
                }
                !changed
            }
            _ => true,
        }
    }

    fn take_timer(&mut self) -> Option<std::time::Duration> {
        self.timer.pending.take()
    }

    fn timer_elapsed(&mut self, out: &mut Vec<DspWrite>) {
        self.timer.elapsed();
        match self.stage {
            Stage::SendMode => {
                out.push(DspWrite::parameter(self.target, MODE, self.mode));
                self.stage = Stage::SendMix;
                self.timer.start(STEP_MS);
            }
            Stage::SendMix => {
                out.push(DspWrite::parameter(self.target, MIX, self.mix));
                self.stage = Stage::Idle;
            }
            Stage::Idle => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::amps::tests::identity;
    use super::*;

    /// 0xed65c/0xed7f0: a live MODE change mutes MIX, then MODE, then the
    /// MIX stored meanwhile, 50 ms apart; the push passes MODE unchanged.
    #[test]
    fn live_mode_change_mutes_then_replays_mode_and_mix() {
        let mut glitch = Glitch::new(&identity(26005));
        let at = identity(26005).target;
        let mut out = Vec::new();
        assert!(glitch.update_parameter(MODE, 0.5, ParamSetMode::Push, &mut out));
        assert!(glitch.update_parameter(MODE, 0.5, ParamSetMode::Live, &mut out));
        assert!(out.is_empty() && glitch.take_timer().is_none());
        assert!(!glitch.update_parameter(MODE, 1.0, ParamSetMode::Recall, &mut out));
        assert!(!glitch.update_parameter(MIX, 0.8, ParamSetMode::Live, &mut out));
        assert_eq!(glitch.take_timer(), Some(Duration::from_millis(50)));
        glitch.timer_elapsed(&mut out);
        assert_eq!(glitch.take_timer(), Some(Duration::from_millis(50)));
        glitch.timer_elapsed(&mut out);
        assert_eq!(
            out,
            [
                DspWrite::parameter(at, MIX, 0.0),
                DspWrite::parameter(at, MODE, 1.0),
                DspWrite::parameter(at, MIX, 0.8),
            ]
        );
        assert!(glitch.update_parameter(MIX, 0.2, ParamSetMode::Live, &mut out));
    }
}

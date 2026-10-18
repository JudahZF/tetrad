//! SoloRevDDriver (1170): constructor 0x562f10, `+0x30` 0x562c88, `+0x48`
//! 0x5637c8, timer slot 0x5634e4, buffer 2 0x5633e0.

use super::{
    Driver, DriverKind, DspWrite, ModelIdentity, ParamSetMode, RELOAD_DELAY, StockPayload, Timer,
    payload,
};
use tetrad_dsp_protocol::DeploymentTarget;

const INITIAL: StockPayload = payload!(0xbaf920 => 0x12c4, 0xbb0be4 => 0x2a0);
const RHYTHM: [StockPayload; 2] = [payload!(0xbb0e84 => 0x2844), payload!(0xbb36c8 => 0x2844)];
const LEAD: StockPayload = payload!(0xbb5f0c => 0x2844);
const CHANNELS: [StockPayload; 3] = [
    payload!(0xbb8750 => 0x104),
    payload!(0xbb8858 => 0x104),
    payload!(0xbb8960 => 0x104),
];

pub(super) fn payloads() -> impl Iterator<Item = StockPayload> {
    [INITIAL, RHYTHM[0], RHYTHM[1], LEAD]
        .into_iter()
        .chain(CHANNELS)
}

#[derive(Debug)]
pub(in super::super) struct SoloRevD {
    target: DeploymentTarget,
    channel: usize,
    lead: bool,
    rhythm: bool,
    bright: bool,
    bright_dirty: bool,
    channel_dirty: bool,
    controls: [f32; 4],
    edited: bool,
    timer: Timer,
}

impl SoloRevD {
    pub(in super::super) fn new(identity: &ModelIdentity, out: &mut Vec<DspWrite>) -> Self {
        out.push(DspWrite::parameter(identity.target, 10, 1.0));
        out.push(DspWrite::parameter(identity.target, 8, 0.0));
        out.push(DspWrite::StockData {
            target: identity.target,
            buffer: 0,
            payload: INITIAL,
        });
        Self {
            target: identity.target,
            channel: 0,
            lead: false,
            rhythm: true,
            bright: false,
            bright_dirty: true,
            channel_dirty: true,
            controls: [0.0; 4],
            edited: false,
            timer: Timer::default(),
        }
    }
    fn schedule(&mut self, mode: ParamSetMode, out: &mut Vec<DspWrite>) {
        if mode != ParamSetMode::Push && !self.timer.active {
            out.push(DspWrite::parameter(self.target, 10, 1.0));
            self.edited = true;
            self.timer.start(RELOAD_DELAY);
        }
    }
    fn flush(&mut self, out: &mut Vec<DspWrite>) {
        self.channel = if self.lead {
            2
        } else {
            usize::from(self.rhythm)
        };
        if (self.channel == 1 && (self.channel_dirty || self.bright_dirty))
            || (self.channel == 2 && self.channel_dirty)
        {
            out.push(DspWrite::StockData {
                target: self.target,
                buffer: 1,
                payload: if self.channel == 2 {
                    LEAD
                } else {
                    RHYTHM[usize::from(self.bright)]
                },
            });
        }
        if self.channel_dirty {
            out.push(DspWrite::parameter(
                self.target,
                8,
                self.channel as f32 * 0.5,
            ));
            out.push(DspWrite::StockData {
                target: self.target,
                buffer: 2,
                payload: CHANNELS[self.channel],
            });
            out.push(DspWrite::IntParameter {
                target: self.target,
                index: 15,
                value: [24, 3, 0][self.channel],
            });
        }
        let offset = if self.channel == 2 { 2 } else { 0 };
        out.push(DspWrite::parameter(self.target, 0, self.controls[offset]));
        out.push(DspWrite::parameter(
            self.target,
            4,
            self.controls[offset + 1],
        ));
        if self.channel != 2 {
            out.push(DspWrite::parameter(
                self.target,
                9,
                f32::from(u8::from(self.bright)),
            ));
        }
        out.push(DspWrite::parameter(
            self.target,
            10,
            if self.edited { 0.0 } else { 0.1 },
        ));
        self.bright_dirty = false;
        self.channel_dirty = false;
    }
}

impl Driver for SoloRevD {
    fn kind(&self) -> DriverKind {
        DriverKind::SoloRevD
    }
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            0 | 4 => {
                self.controls[usize::from(index == 4)] = value;
                return self.channel != 2;
            }
            8 | 10 => {}
            9 => {
                let state = value > 0.5;
                if state == self.bright {
                    return false;
                }
                self.bright = state;
                self.bright_dirty = true;
                if self.channel_dirty || self.channel != 2 {
                    self.schedule(mode, out);
                }
            }
            11 => {
                let state = value > 0.5;
                if state == self.lead {
                    return false;
                }
                self.lead = state;
                self.channel_dirty = true;
                self.schedule(mode, out);
            }
            12 => {
                let state = value > 0.5;
                if state == self.rhythm {
                    return false;
                }
                self.rhythm = state;
                self.channel_dirty |= !self.lead;
                if self.channel_dirty || (self.channel != 2 && self.bright_dirty) {
                    self.schedule(mode, out);
                }
            }
            13 | 14 => {
                if self.channel == 2 {
                    out.push(DspWrite::parameter(
                        self.target,
                        if index == 13 { 0 } else { 4 },
                        value,
                    ));
                }
                self.controls[if index == 13 { 2 } else { 3 }] = value;
            }
            _ => return true,
        }
        false
    }
    fn parameters_pushed(&mut self, out: &mut Vec<DspWrite>) {
        if !self.timer.active && (self.channel_dirty || (self.channel != 2 && self.bright_dirty)) {
            self.flush(out);
        }
    }
    fn take_timer(&mut self) -> Option<std::time::Duration> {
        self.timer.pending.take()
    }
    fn timer_elapsed(&mut self, out: &mut Vec<DspWrite>) {
        self.timer.elapsed();
        self.flush(out);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::identity;
    use super::*;
    #[test]
    fn solo_lead_recall_selects_tables_and_cached_controls() {
        let identity = identity(1170);
        let target = identity.target;
        let mut out = Vec::new();
        let mut driver = SoloRevD::new(&identity, &mut out);
        assert_eq!(out.len(), 3);
        out.clear();
        driver.update_parameter(13, 0.7, ParamSetMode::Push, &mut out);
        driver.update_parameter(14, 0.8, ParamSetMode::Push, &mut out);
        driver.parameters_pushed(&mut out);
        assert!(out.contains(&DspWrite::parameter(target, 10, 0.1)));
        out.clear();
        assert!(!driver.update_parameter(11, 1.0, ParamSetMode::Recall, &mut out));
        assert_eq!(out, [DspWrite::parameter(target, 10, 1.0)]);
        assert_eq!(
            driver.take_timer(),
            Some(std::time::Duration::from_millis(20))
        );
        driver.update_parameter(9, 1.0, ParamSetMode::Recall, &mut out);
        assert_eq!(driver.take_timer(), None);
        out.clear();
        driver.timer_elapsed(&mut out);
        assert_eq!(
            out[0],
            DspWrite::StockData {
                target,
                buffer: 1,
                payload: LEAD
            }
        );
        assert!(out.contains(&DspWrite::parameter(target, 0, 0.7)));
        assert!(out.contains(&DspWrite::parameter(target, 4, 0.8)));
        assert!(out.contains(&DspWrite::IntParameter {
            target,
            index: 15,
            value: 0
        }));
        assert_eq!(out.last(), Some(&DspWrite::parameter(target, 10, 0.0)));
    }
}

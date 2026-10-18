//! JP2CDriver (1140, 1141, 1179, 1180): constructor 0x5609b0, `+0x30`
//! 0x560428, `+0x48` 0x56086c, timer slot 0x560764, buffer 0 0x560594.

use super::{
    Driver, DriverKind, DspWrite, ModelIdentity, ParamSetMode, RELOAD_DELAY, SetupError,
    StockPayload, Timer, payload,
};
use tetrad_dsp_protocol::DeploymentTarget;

/// Buffer 0 per variant and layout `+0x24·2 + +0x25`: four segments.
const CHANNELS: [[StockPayload; 4]; 2] = [
    [
        payload!(0xa967c4 => 0x5ea4, 0xa95ec4 => 0x900, 0x153d82c => 0x1ac4, 0xab64f8 => 0x940),
        payload!(0xaa370c => 0x5ea4, 0xaa2e0c => 0x900, 0x153d82c => 0x1ac4, 0xab64f8 => 0x940),
        payload!(0xa90020 => 0x5ea4, 0xa8f720 => 0x900, 0x153d82c => 0x1ac4, 0xab64f8 => 0x940),
        payload!(0xa9cf68 => 0x5ea4, 0xa9c668 => 0x900, 0x153d82c => 0x1ac4, 0xab64f8 => 0x940),
    ],
    [
        payload!(0xab0654 => 0x5ea4, 0xaafd54 => 0x900, 0x153d82c => 0x1ac4, 0xab64f8 => 0x940),
        payload!(0xa830d8 => 0x5ea4, 0xa827d8 => 0x900, 0x153d82c => 0x1ac4, 0xab64f8 => 0x940),
        payload!(0xaa9eb0 => 0x5ea4, 0xaa95b0 => 0x900, 0x153d82c => 0x1ac4, 0xab64f8 => 0x940),
        payload!(0xa8987c => 0x5ea4, 0xa88f7c => 0x900, 0x153d82c => 0x1ac4, 0xab64f8 => 0x940),
    ],
];

pub(super) fn payloads() -> impl Iterator<Item = StockPayload> {
    CHANNELS.into_iter().flatten()
}

/// Constructor fields per model: variant `+0x34` (table family), reload
/// `+0x28`, then the parameter index of each switch byte: `+0x30` → `+0x24`,
/// `+0x2c` → `+0x25`, and the extra switch `+0x26` (1179 and 1180, which
/// also clear the reload after every timer slot and push).
fn fields(model_id: u32) -> Option<(usize, u32, [Option<u32>; 3])> {
    Some(match model_id {
        1140 => (0, 15, [Some(2), Some(1), None]),
        1141 => (1, 15, [Some(2), Some(1), None]),
        1179 => (0, 10, [Some(1), None, Some(5)]),
        1180 => (1, 8, [None, None, Some(3)]),
        _ => return None,
    })
}

#[derive(Debug)]
pub(in super::super) struct Jp2c {
    target: DeploymentTarget,
    variant: usize,
    reload: u32,
    indices: [Option<u32>; 3],
    /// Switch bytes `+0x24..=+0x26` (`v > 0.5`, NaN off).
    switches: [bool; 3],
    /// `+0x27`.
    dirty: bool,
    /// `+0x38`: the mode of the last switch change until the upload.
    mode: Option<ParamSetMode>,
    timer: Timer,
}

impl Jp2c {
    pub(in super::super) fn new(
        identity: &ModelIdentity,
        out: &mut Vec<DspWrite>,
    ) -> Result<Self, SetupError> {
        let model_id = identity.model_id;
        let (variant, reload, indices) = fields(model_id).ok_or(SetupError::UnsupportedDriver {
            model_id,
            driver: DriverKind::Jp2c,
        })?;
        let mut driver = Self {
            target: identity.target,
            variant,
            reload,
            indices,
            switches: [false; 3],
            dirty: true,
            mode: None,
            timer: Timer::default(),
        };
        driver.flush(out);
        Ok(driver)
    }

    /// The timer slot body (0x560764), also run by the constructor and the
    /// push branch of `+0x48`.
    fn flush(&mut self, out: &mut Vec<DspWrite>) {
        let send = |index: Option<u32>, on: bool, out: &mut Vec<DspWrite>| {
            if let Some(index) = index {
                out.push(DspWrite::parameter(
                    self.target,
                    index,
                    f32::from(u8::from(on)),
                ));
            }
        };
        if self.dirty {
            let layout = usize::from(self.switches[0]) * 2 + usize::from(self.switches[1]);
            out.push(DspWrite::StockData {
                target: self.target,
                buffer: 0,
                payload: CHANNELS[self.variant][layout],
            });
            send(self.indices[1], self.switches[1], out);
            send(self.indices[0], self.switches[0], out);
            self.mode = None;
            self.dirty = false;
        }
        if self.indices[2].is_some() {
            send(self.indices[2], self.switches[2], out);
            out.push(DspWrite::parameter(self.target, self.reload, 0.0));
        }
    }
}

impl Driver for Jp2c {
    fn kind(&self) -> DriverKind {
        DriverKind::Jp2c
    }

    /// A switch change marks dirty in every mode; live edits hold the
    /// reload and (re)start the timer without an `isActive` test. An
    /// unchanged switch passes its raw value.
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        if index == self.reload {
            return false;
        }
        let Some(switch) = self.indices.iter().position(|&i| i == Some(index)) else {
            return true;
        };
        let on = value > 0.5;
        if self.switches[switch] == on {
            return true;
        }
        self.switches[switch] = on;
        self.dirty = true;
        self.mode = Some(mode);
        if mode == ParamSetMode::Live {
            out.push(DspWrite::parameter(self.target, self.reload, 1.0));
            self.timer.start(RELOAD_DELAY);
        }
        false
    }

    /// Recall holds the reload and starts the timer; push uploads at once.
    fn parameters_pushed(&mut self, out: &mut Vec<DspWrite>) {
        match self.mode {
            Some(ParamSetMode::Recall) => {
                out.push(DspWrite::parameter(self.target, self.reload, 1.0));
                self.timer.start(RELOAD_DELAY);
            }
            Some(ParamSetMode::Push) => self.flush(out),
            _ => {}
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
    fn jp2c_push_and_recall_use_different_reload_paths() {
        let identity = identity(1179);
        let target = identity.target;
        let mut out = Vec::new();
        let mut driver = Jp2c::new(&identity, &mut out).unwrap();
        assert_eq!(out.len(), 4);
        out.clear();
        assert!(!driver.update_parameter(1, 1.0, ParamSetMode::Push, &mut out));
        driver.parameters_pushed(&mut out);
        assert_eq!(
            out[0],
            DspWrite::StockData {
                target,
                buffer: 0,
                payload: CHANNELS[0][2]
            }
        );
        assert_eq!(driver.take_timer(), None);
        out.clear();
        assert!(!driver.update_parameter(1, 0.0, ParamSetMode::Recall, &mut out));
        assert!(out.is_empty());
        driver.parameters_pushed(&mut out);
        assert_eq!(out, [DspWrite::parameter(target, 10, 1.0)]);
        assert_eq!(
            driver.take_timer(),
            Some(std::time::Duration::from_millis(20))
        );
        out.clear();
        driver.timer_elapsed(&mut out);
        assert_eq!(
            out[0],
            DspWrite::StockData {
                target,
                buffer: 0,
                payload: CHANNELS[0][0]
            }
        );
        assert_eq!(out.last(), Some(&DspWrite::parameter(target, 10, 0.0)));
        assert!(driver.update_parameter(1, 0.0, ParamSetMode::Live, &mut out));
    }
}

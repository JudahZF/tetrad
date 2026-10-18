//! Drive pedals that swap a stock circuit table when one switch changes.
//! Outside the push a change first raises the model's MUTE TRIG, then a
//! single-shot timer uploads the new table.
//!
//! | Driver | Model | Constructor | `+0x30` | Upload / slot | Delay |
//! | --- | --- | --- | --- | --- | --- |
//! | ODR1Driver | 26 | 0x55f028 | 0x55eeec | inline / 0x55f124 | 50 ms |
//! | KeeleyKatanaDriver | 45 | 0x55f920 | 0x55fbfc | 0x55fb0c / 0x55fa1c | 10 ms |
//! | RabeaFuzzDriver | 47 | 0x562878 | 0x5626c8 | `+0x48` 0x562758 / 0x5629ac | 10 ms |

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{DspWrite, ModelIdentity, ParamSetMode, StockPayload};
use super::reload::{Timer, payload};
use super::{Driver, DriverKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pedal {
    Odr1,
    Katana,
}

impl Pedal {
    /// The switch, its MUTE TRIG index and the reload delay.
    const fn layout(self) -> (u32, u32, u64) {
        match self {
            Self::Odr1 => (3, 4, 50),
            Self::Katana => (1, 2, 10),
        }
    }

    /// Buffer 0 per switch state: `table + head` first, then the head.
    const fn table(self, on: bool) -> StockPayload {
        match (self, on) {
            (Self::Odr1, false) => payload!(0x00a5_5ef8 => 0x3e44, 0x00a5_55b8 => 0x940),
            (Self::Odr1, true) => payload!(0x00a5_a67c => 0x3e44, 0x00a5_9d3c => 0x940),
            (Self::Katana, false) => payload!(0x00a6_b680 => 0xac4, 0x00a6_b480 => 0x200),
            (Self::Katana, true) => payload!(0x00a6_c348 => 0xac4, 0x00a6_c148 => 0x200),
        }
    }
}

pub(super) const RABEA: [[StockPayload; 3]; 2] = [
    [
        payload!(0x00ba_f808 => 0x3c),
        payload!(0x00ba_3180 => 0x5ea4),
        payload!(0x00ba_2800 => 0x980),
    ],
    [
        payload!(0x00ba_f844 => 0x3c),
        payload!(0x00ba_9964 => 0x5ea4),
        payload!(0x00ba_9024 => 0x940),
    ],
];

pub(super) fn payloads() -> impl Iterator<Item = StockPayload> {
    [Pedal::Odr1, Pedal::Katana]
        .into_iter()
        .flat_map(|pedal| [pedal.table(false), pedal.table(true)])
        .chain(RABEA.into_iter().flatten())
}

/// ODR1Driver and KeeleyKatanaDriver: the constructor raises MUTE TRIG; the
/// switch (`v > 0.5`) never passes. An unchanged switch uploads again only
/// in the push; a changed one uploads at once in the push, else after the
/// timer. Katana also sends BOOST as 0.0/1.0 after its table.
#[derive(Debug)]
pub(super) struct SwitchedDrive {
    target: DeploymentTarget,
    pedal: Pedal,
    /// `+0x25`.
    on: bool,
    timer: Timer,
}

impl SwitchedDrive {
    pub(super) fn new(identity: &ModelIdentity, pedal: Pedal, out: &mut Vec<DspWrite>) -> Self {
        let (_, mute, _) = pedal.layout();
        out.push(DspWrite::parameter(identity.target, mute, 1.0));
        Self {
            target: identity.target,
            pedal,
            on: false,
            timer: Timer::default(),
        }
    }

    fn upload(&self, out: &mut Vec<DspWrite>) {
        out.push(DspWrite::StockData {
            target: self.target,
            buffer: 0,
            payload: self.pedal.table(self.on),
        });
        if self.pedal == Pedal::Katana {
            let (switch, _, _) = self.pedal.layout();
            let on = if self.on { 1.0 } else { 0.0 };
            out.push(DspWrite::parameter(self.target, switch, on));
        }
    }
}

impl Driver for SwitchedDrive {
    fn kind(&self) -> DriverKind {
        match self.pedal {
            Pedal::Odr1 => DriverKind::Odr1,
            Pedal::Katana => DriverKind::KeeleyKatana,
        }
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let (switch, mute, delay) = self.pedal.layout();
        if index != switch {
            return true;
        }
        let on = value > 0.5;
        let changed = on != self.on;
        self.on = on;
        match mode {
            ParamSetMode::Push => self.upload(out),
            ParamSetMode::Live | ParamSetMode::Recall if changed => {
                out.push(DspWrite::parameter(self.target, mute, 1.0));
                self.timer.start(delay);
            }
            _ => {}
        }
        false
    }

    fn take_timer(&mut self) -> Option<std::time::Duration> {
        self.timer.pending.take()
    }

    /// The slot uploads without a pending test.
    fn timer_elapsed(&mut self, out: &mut Vec<DspWrite>) {
        self.timer.elapsed();
        self.upload(out);
    }
}

const RABEA_MODE: u32 = 0;
const RABEA_MUTE: u32 = 5;

/// RabeaFuzzDriver: MODE (`vcvt.s32`) selects one of two three-buffer sets.
/// `+0x48` and the timer slot upload when the timer is idle and a change is
/// pending, then send MODE and clear MUTE TRIG.
#[derive(Debug)]
pub(super) struct Rabea {
    target: DeploymentTarget,
    /// `+0x28`.
    mode: i32,
    /// `+0x2c`, set at construction.
    dirty: bool,
    timer: Timer,
}

impl Rabea {
    pub(super) fn new(identity: &ModelIdentity) -> Self {
        Self {
            target: identity.target,
            mode: 0,
            dirty: true,
            timer: Timer::default(),
        }
    }

    fn reload(&mut self, out: &mut Vec<DspWrite>) {
        if self.timer.active || !self.dirty {
            return;
        }
        // Stock indexes with the raw MODE; 0 and 1 are the only sets (owned
        // clamp).
        let set = &RABEA[usize::from(self.mode > 0)];
        out.extend(
            (0..)
                .zip(set)
                .map(|(buffer, &payload)| DspWrite::StockData {
                    target: self.target,
                    buffer,
                    payload,
                }),
        );
        out.push(DspWrite::parameter(
            self.target,
            RABEA_MODE,
            self.mode as f32,
        ));
        out.push(DspWrite::parameter(self.target, RABEA_MUTE, 0.0));
        self.dirty = false;
    }
}

impl Driver for Rabea {
    fn kind(&self) -> DriverKind {
        DriverKind::RabeaFuzz
    }

    /// A pushed MODE change passes unchanged without marking a reload.
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            RABEA_MODE => {
                let selected = value as i32;
                if selected == self.mode {
                    return false;
                }
                self.mode = selected;
                if mode == ParamSetMode::Push {
                    return true;
                }
                self.dirty = true;
                if !self.timer.active {
                    out.push(DspWrite::parameter(self.target, RABEA_MUTE, 1.0));
                    self.timer.start(10);
                }
                false
            }
            RABEA_MUTE => false,
            _ => true,
        }
    }

    fn parameters_pushed(&mut self, out: &mut Vec<DspWrite>) {
        self.reload(out);
    }

    fn take_timer(&mut self) -> Option<std::time::Duration> {
        self.timer.pending.take()
    }

    fn timer_elapsed(&mut self, out: &mut Vec<DspWrite>) {
        self.timer.elapsed();
        self.reload(out);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::amps::tests::identity;
    use super::*;

    /// `0x55eeec`/`0x55f124`: MUTE TRIG at construction; the push uploads
    /// the selected table; a live change mutes and uploads after 50 ms.
    #[test]
    fn odr1_bass_cut_swaps_tables() {
        let at = identity(26).target;
        let mut out = Vec::new();
        let mut odr = SwitchedDrive::new(&identity(26), Pedal::Odr1, &mut out);
        assert_eq!(out, [DspWrite::parameter(at, 4, 1.0)]);
        out.clear();
        assert!(odr.update_parameter(0, 0.5, ParamSetMode::Push, &mut out));
        assert!(!odr.update_parameter(3, 0.0, ParamSetMode::Push, &mut out));
        assert!(!odr.update_parameter(3, 0.0, ParamSetMode::Live, &mut out));
        assert!(!odr.update_parameter(3, 1.0, ParamSetMode::Live, &mut out));
        assert_eq!(odr.take_timer(), Some(Duration::from_millis(50)));
        odr.timer_elapsed(&mut out);
        let data = |on| DspWrite::StockData {
            target: at,
            buffer: 0,
            payload: Pedal::Odr1.table(on),
        };
        assert_eq!(
            out,
            [data(false), DspWrite::parameter(at, 4, 1.0), data(true)]
        );
        assert_eq!(Pedal::Odr1.table(true).len(), 0x4784);
    }

    /// `0x55fb0c`: Katana resends BOOST after its table.
    #[test]
    fn katana_boost_resends_the_switch() {
        let at = identity(45).target;
        let mut out = Vec::new();
        let mut katana = SwitchedDrive::new(&identity(45), Pedal::Katana, &mut out);
        out.clear();
        katana.update_parameter(1, 0.9, ParamSetMode::Push, &mut out);
        assert_eq!(out[1], DspWrite::parameter(at, 1, 1.0));
        assert_eq!(Pedal::Katana.table(true).len(), 0xcc4);
        out.clear();
        katana.update_parameter(1, 0.1, ParamSetMode::Recall, &mut out);
        assert_eq!(out, [DspWrite::parameter(at, 2, 1.0)]);
        assert_eq!(katana.take_timer(), Some(Duration::from_millis(10)));
    }

    /// `0x5626c8`/`0x562758`: the push lets a changed MODE through and
    /// uploads once; a live change mutes, and the timer reloads.
    #[test]
    fn rabea_mode_reload() {
        let at = identity(47).target;
        let mut rabea = Rabea::new(&identity(47));
        let mut out = Vec::new();
        assert!(rabea.update_parameter(0, 1.0, ParamSetMode::Push, &mut out));
        assert!(!rabea.update_parameter(0, 1.0, ParamSetMode::Push, &mut out));
        rabea.parameters_pushed(&mut out);
        assert_eq!(
            out[3..],
            [
                DspWrite::parameter(at, 0, 1.0),
                DspWrite::parameter(at, 5, 0.0)
            ]
        );
        out.clear();
        assert!(!rabea.update_parameter(0, 0.0, ParamSetMode::Live, &mut out));
        rabea.parameters_pushed(&mut out);
        assert_eq!(out, [DspWrite::parameter(at, 5, 1.0)]);
        assert_eq!(rabea.take_timer(), Some(Duration::from_millis(10)));
        rabea.timer_elapsed(&mut out);
        assert!(
            matches!(out[1], DspWrite::StockData { buffer: 0, payload, .. } if payload == RABEA[0][0])
        );
    }
}

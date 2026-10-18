//! FreezeDriver (26001): constructor 0x55ae84, vtable 0x1261ac8 with `+0x30`
//! 0x55ad6c, `+0x38` 0x55abec, footswitch press `+0x40` 0x55acd8 and release
//! `+0x44` 0x55ac5c. ON/OFF writes go through the controller (`0x52382c`,
//! index 0): the value is stored in the preset and re-enters `+0x30` in mode
//! 0, whose pass sends the `0x02`. Each `+0x30` ON/OFF also signals a UI
//! refresh (0x5114c8); the app redraws from the stored value.

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{BlockBypass, DspWrite, LinkedEdit, ModelIdentity, ParamSetMode};
use super::{Driver, DriverKind};

const ON_OFF: u32 = 0;
const MOMENTARY: u32 = 1;

#[derive(Debug)]
pub(super) struct Freeze {
    target: DeploymentTarget,
    /// 0x499a60: the mini ignores MOMENTARY.
    mini: bool,
    /// MOMENTARY (`+0x24`).
    momentary: bool,
    /// ON/OFF (`+0x28`).
    frozen: bool,
    /// `+0x29`.
    bypassed: bool,
    /// The stomp is held (`+0x2a`).
    pressed: bool,
    edits: Vec<LinkedEdit>,
}

impl Freeze {
    /// No DSP writes; everything starts off.
    pub(super) fn new(identity: &ModelIdentity, mini: bool) -> Self {
        Self {
            target: identity.target,
            mini,
            momentary: false,
            frozen: false,
            bypassed: false,
            pressed: false,
            edits: Vec::new(),
        }
    }

    /// `0x52382c(value, …, ON_OFF)`: the preset store and the `+0x30`
    /// re-entry (mode 0).
    fn write_on_off(&mut self, value: f32, out: &mut Vec<DspWrite>) {
        if self.update_parameter(ON_OFF, value, ParamSetMode::Live, out) {
            out.push(DspWrite::parameter(self.target, ON_OFF, value));
        }
        self.edits.push(LinkedEdit {
            index: ON_OFF,
            value,
            echo: true,
        });
    }
}

impl Driver for Freeze {
    fn kind(&self) -> DriverKind {
        DriverKind::Freeze
    }

    /// `+0x30`; it never reads the mode. ON/OFF passes unless bypassed.
    /// MOMENTARY on, while latched and not held, turns the freeze off
    /// first; while bypassed it is refused and not kept (`0x55ae38`).
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            // `vcmpe`: NaN is off.
            ON_OFF => {
                self.frozen = value >= 0.5;
                !self.bypassed
            }
            MOMENTARY if self.mini => false,
            MOMENTARY if value >= 0.5 => {
                if !self.momentary && !self.pressed {
                    if self.bypassed {
                        return false;
                    }
                    self.write_on_off(0.0, out);
                }
                self.momentary = true;
                true
            }
            MOMENTARY => {
                self.momentary = false;
                true
            }
            _ => true,
        }
    }

    /// `+0x38` (through the base `+0x3c`): while frozen, bypass sends ON/OFF
    /// 0.0 and engaging again sends 1.0. The `0x0c` always goes out.
    fn bypass(&mut self, bypass: BlockBypass, out: &mut Vec<DspWrite>) -> bool {
        if self.frozen {
            if bypass.bypassed {
                out.push(DspWrite::parameter(self.target, ON_OFF, 0.0));
            } else if self.bypassed {
                out.push(DspWrite::parameter(self.target, ON_OFF, 1.0));
            }
        }
        self.bypassed = bypass.bypassed;
        true
    }

    /// `+0x40`: momentary freezes, latched toggles; nothing while bypassed.
    fn footswitch_pressed(&mut self, out: &mut Vec<DspWrite>) {
        if !self.bypassed {
            self.pressed = true;
            let on = self.momentary || !self.frozen;
            self.write_on_off(f32::from(u8::from(on)), out);
        }
    }

    /// `+0x44`: a momentary freeze ends; nothing while bypassed.
    fn footswitch_released(&mut self, out: &mut Vec<DspWrite>) {
        if !self.bypassed {
            self.pressed = false;
            if self.momentary && self.frozen {
                self.write_on_off(0.0, out);
            }
        }
    }

    fn take_linked_edits(&mut self, out: &mut Vec<LinkedEdit>) {
        out.append(&mut self.edits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::drivers::amps::tests::identity;

    /// Latched: each press toggles ON/OFF through the controller (a stored,
    /// echoed write and its `0x02`); the release does nothing. Momentary:
    /// MOMENTARY on turns a latched freeze off, then press freezes and
    /// release thaws. Bypass mutes a freeze and engaging restores it; a
    /// bypassed block ignores the footswitch. The mini refuses MOMENTARY.
    #[test]
    fn freeze_footswitch_momentary_and_bypass_follow_stock() {
        let target = identity(26001).target;
        let p = |value| DspWrite::parameter(target, ON_OFF, value);
        let edit = |value| LinkedEdit {
            index: ON_OFF,
            value,
            echo: true,
        };
        let mut freeze = Freeze::new(&identity(26001), false);
        let (mut out, mut edits) = (Vec::new(), Vec::new());
        freeze.footswitch_pressed(&mut out);
        freeze.footswitch_released(&mut out);
        freeze.take_linked_edits(&mut edits);
        assert_eq!(
            (out.as_slice(), edits.as_slice()),
            (&[p(1.0)][..], &[edit(1.0)][..])
        );
        out.clear();
        assert!(freeze.update_parameter(MOMENTARY, 1.0, ParamSetMode::Live, &mut out));
        assert_eq!(out, [p(0.0)]);
        out.clear();
        freeze.footswitch_pressed(&mut out);
        freeze.footswitch_released(&mut out);
        assert_eq!(out, [p(1.0), p(0.0)]);
        out.clear();
        freeze.footswitch_pressed(&mut out);
        let bypassed = BlockBypass {
            bypassed: true,
            muted: false,
        };
        assert!(freeze.bypass(bypassed, &mut out));
        freeze.footswitch_released(&mut out);
        assert!(freeze.bypass(BlockBypass::default(), &mut out));
        assert_eq!(out, [p(1.0), p(0.0), p(1.0)]);
        out.clear();
        freeze.bypass(bypassed, &mut out);
        assert!(!freeze.update_parameter(ON_OFF, 1.0, ParamSetMode::Live, &mut out));
        let mut mini = Freeze::new(&identity(26001), true);
        assert!(!mini.update_parameter(MOMENTARY, 1.0, ParamSetMode::Live, &mut out));
        assert!(!mini.momentary);
    }
}

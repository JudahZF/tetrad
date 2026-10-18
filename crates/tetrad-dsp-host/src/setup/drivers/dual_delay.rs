//! DualDelayDriver (`0x57c4bc`, hook `0x57c3b0`) and DualReverseDelayDriver
//! (`0x57c664`, hook `0x57c558`) share the feedback-link contract.
//!
//! Stock never writes the counterpart feedback itself. It edits it through
//! `0x52382c` → `0x523644` → `0x51e4d8`: the preset value is stored, this hook
//! runs again in mode 0 and stores its cache, and its `0x02` goes out before
//! the caller's own. Then come the Control echo (`0x25d51c`) and
//! `updateControlPositionSignal` (`0x9ab358`). The driver flattens that
//! re-entry into the same DSP writes and cache updates and records each
//! write as a [`LinkedEdit`]; the app stores it and sends the echo. A NaN
//! feedback while linked recurses forever in stock (NaN is never equal);
//! here it stops after one counterpart write.

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{DspWrite, LinkedEdit, ModelIdentity, ParamSetMode};
use super::{Driver, DriverKind};

const LEFT: u32 = 4;
const RIGHT: u32 = 9;

#[derive(Debug)]
pub(super) struct DualDelay {
    target: DeploymentTarget,
    reverse: bool,
    linked: bool,
    left: f32,
    right: f32,
    edits: Vec<LinkedEdit>,
}

impl DualDelay {
    pub(super) fn new(identity: &ModelIdentity, reverse: bool) -> Self {
        Self {
            target: identity.target,
            reverse,
            linked: false,
            left: 0.0,
            right: 0.0,
            edits: Vec::new(),
        }
    }

    /// One `0x52382c` counterpart write: its `0x02` and the preset store.
    fn write(&mut self, index: u32, value: f32, out: &mut Vec<DspWrite>) {
        out.push(DspWrite::parameter(self.target, index, value));
        self.edits.push(LinkedEdit {
            index,
            value,
            echo: true,
        });
    }
}

impl Driver for DualDelay {
    fn kind(&self) -> DriverKind {
        if self.reverse {
            DriverKind::DualReverseDelay
        } else {
            DriverKind::DualDelay
        }
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let link = if self.reverse { 24 } else { 14 };
        match index {
            LEFT => {
                self.left = value;
                if self.linked && self.left != self.right {
                    self.right = value;
                    self.write(RIGHT, value, out);
                }
            }
            RIGHT => {
                self.right = value;
                if self.linked && self.left != self.right {
                    self.left = value;
                    self.write(LEFT, value, out);
                }
            }
            index if index == link => {
                // `vcmp #0.0`: any nonzero value, NaN included, links.
                self.linked = value != 0.0;
                if self.linked {
                    self.right = self.left;
                } else {
                    self.write(LEFT, self.left, out);
                }
                self.write(RIGHT, self.right, out);
            }
            _ => {}
        }
        true
    }

    fn take_linked_edits(&mut self, out: &mut Vec<LinkedEdit>) {
        out.append(&mut self.edits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::tests::{delay_hook, delay_identity};
    use ParamSetMode::{Live, Push, Recall};

    /// The two classes differ only in the LINK FBACK index. LINK re-sends
    /// the cached feedback in every mode, so a push with LINK off sends
    /// FEEDBACK L and R twice.
    #[test]
    fn feedback_link_matches_stock() {
        for (model, link) in [(6012, 14), (6014, 24)] {
            let mut driver = DualDelay::new(&delay_identity(model), model == 6014);
            let mut hook = |index, value, mode| delay_hook(&mut driver, index, value, mode);
            assert_eq!(hook(4, 0.25, Push), (true, vec![]));
            assert_eq!(hook(9, 0.75, Push), (true, vec![]));
            assert_eq!(hook(link, 0.0, Push), (true, vec![(4, 0.25), (9, 0.75)]));
            assert_eq!(hook(link, 0.1, Recall), (true, vec![(9, 0.25)]));
            assert_eq!(hook(9, 0.5, Live), (true, vec![(4, 0.5)]));
            assert_eq!(hook(4, 0.5, Live), (true, vec![]));
            assert_eq!(hook(4, 0.75, Recall), (true, vec![(9, 0.75)]));
            // Each counterpart write is also a stored, echoed preset write.
            let mut edits = Vec::new();
            driver.take_linked_edits(&mut edits);
            let stored: Vec<(u32, f32)> =
                edits.iter().map(|edit| (edit.index, edit.value)).collect();
            assert_eq!(
                stored,
                [(4, 0.25), (9, 0.75), (9, 0.25), (4, 0.5), (9, 0.75)]
            );
            assert!(edits.iter().all(|edit| edit.echo));
        }
    }
}

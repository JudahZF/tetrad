//! The Control overwrite form (`0xfa48c`, re-r38 §3): an overwrite question of an upload that
//! Cortex Control started goes only to Control, as Confirmation (47); the device screen does
//! not show it (C: `0x463d2c` with the Control flag set).
//!
//! C: the message has a 64-bit `confirmation_id` (`0xf9844`), `title`, `msg`, `actions` =
//! [Ok, Cancel] (`DAT_00a432f0`), `ok_title` "OK" and `cancel_title` "SKIP" (batch) or
//! "Cancel" (single). The answer `0xf986c`: Ok runs the ok closure (`0x4635f0`: overwrite);
//! Cancel, an absent or an unknown action runs the cancel closure (`0x463700`: skip). A
//! disconnect (`0xf9994`) cancels every open question. No timeout.
//!
//! O: stock returns without a callback when Control is not connected (`0x6e0444`), so the
//! upload waits forever; Tetrad answers Cancel at once, also when the link does not send the
//! message.

use std::time::Duration;

use tetrad_control::codecs::ConfirmationAction;
use tetrad_control::link::ConfirmationRequest;
use tetrad_services::cloud::services::ProductKind;

use super::super::{Effect, State};
use super::{AccountRequest, Control, ControlError};

/// An overwrite question of a Control-started upload (`Services::upload_prompt` with
/// `from_control`), as Control shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadAsk {
    /// [`ProductKind::Preset`] (`0x46485c`) or [`ProductKind::NeuralCapture`] (`0x465178`).
    pub kind: ProductKind,
    /// The question's serial (`UploadQuestion::serial`): the answer names it (review r39 K3).
    pub question: u64,
    pub title: String,
    pub text: String,
    /// A batch question: the cancel button is "SKIP".
    pub batch: bool,
}

/// The question Control was asked and its `confirmation_id` (`None`: not sent, answered
/// Cancel at once).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Asked {
    ask: UploadAsk,
    id: Option<u64>,
}

impl Asked {
    /// The open `confirmation_id` and the question its answer is for.
    pub(super) fn open(&self) -> Option<(u64, u64)> {
        Some((self.id?, self.ask.question))
    }
}

impl Control {
    /// The Control-started upload question that waits now, read after each device step. A
    /// new question goes out as Confirmation (47); its answer becomes
    /// [`AccountRequest::UploadAnswer`]. Not connected, or not sent: the answer is Cancel at
    /// once (O). The same question is asked once; `None` forgets the asked one (a later
    /// answer to it does nothing).
    pub fn upload_question(
        &mut self,
        ask: Option<UploadAsk>,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        if self.asked.as_ref().map(|asked| &asked.ask) == ask.as_ref() {
            return Ok(Vec::new());
        }
        let Some(ask) = ask else {
            self.asked = None;
            return Ok(Vec::new());
        };
        let mut id = None;
        let mut effects = Vec::new();
        if self.connected() {
            let request = ConfirmationRequest {
                title: ask.title.clone(),
                message: ask.text.clone(),
                actions: vec![ConfirmationAction::Ok, ConfirmationAction::Cancel],
                ok_title: "OK".into(),
                cancel_title: if ask.batch { "SKIP" } else { "Cancel" }.into(),
            };
            effects = self.with_state(state, now, |link, model, stages| {
                let (sent, step) = link.request_confirmation(request, now, model, stages)?;
                id = sent;
                Ok(step)
            })?;
        }
        if id.is_none() {
            let answer = AccountRequest::UploadAnswer {
                question: ask.question,
                overwrite: false,
            };
            effects.push(Effect::ControlAccount(answer));
        }
        self.asked = Some(Asked { ask, id });
        Ok(effects)
    }
}

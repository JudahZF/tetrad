//! BulkOperation (57) pushes of a Multi Select run (re-r44 §2; A9 r52): the batch upload, a
//! multi-key cloud DELETE and the multi-key downloads, from the services run view
//! (`Services::bulk_run`) read after each device step. Stock pushes go out with
//! `0xee2ec(ctl, 0, msg, 57, &id)`: no request ID; nothing while Control is not connected
//! (`0x6e0444`); a link that may not send only logs them (`Notice::NotSent`).
//!
//! - Start `0xf1cec(ctl, text, 1, 1, source, target)`: CREATE with the overlay text, blocking,
//!   cancellable, progress 0.0, the source and target folders and `operation_type` (the
//!   source folder's type). It first stores the folders in the 57 receiver (`0xed8b4`),
//!   connected or not, for READ. Callers (C): upload `0x785dcc`, DELETE `0x785b74` (source the
//!   cloud folder, no target), capture or IR download `0x786264` (source the cloud folder,
//!   target the saving destination `0x76be70`), preset download `0x786ac4` (source the cloud
//!   presets folder `0x765738`, target the setlist).
//! - Progress `0x77fef0`: on each item start that passes [`pushes_progress`], UPDATE with only
//!   `progress` (`handled / total`, 1-based: the item now starting).
//! - Finished `0xf29b8` (done closures: upload `0x7839a4`, DELETE `0x783950`, downloads
//!   `0x799020` and `0x7839f8`): DELETE with only `finished`; a cancelled run ends the same
//!   way.
//! - O: the run ended when the view is gone or has a new serial. Its `BatchEnd` (same serial,
//!   [`Control::set_batch_end`]) with `success` means every item started: the item starts not
//!   read yet (an item that started and ended inside one device step) push their UPDATE first.
//!   I: a cancelled end adds none (an item that starts in the step of the cancel is missed).
//! - I: an upload with no source folder (Control 46 of user captures, no `from_folder`) sends
//!   no `source_folder`. O: the cloud folder `key` is the kind text and its `name` the type
//!   noun (stock reads `CloudProductFolder +0x44`, not traced).

use std::time::Duration;

use tetrad_control::codecs::{BulkOperationMessage, FolderInfo, MessageAction};
use tetrad_control::link::Notification;
use tetrad_media::factory::DeviceCipher;
use tetrad_services::cloud::services::ProductKind;

use tetrad_storage::stock::ProductType;

use super::super::services::{BatchEnd, BatchOperation, BatchView};
use super::super::{Effect, State};
use super::{Control, ControlError, files};

/// The run Control was told about.
pub(super) struct BatchSeen {
    serial: u64,
    /// The last item start read.
    handled: usize,
    total: usize,
    /// What READ adds while the batch runs (`0xed8d0` action 3): the start fields with the
    /// bar value (`0x81bcdc`) of the last progress that passed the rule.
    pub(super) read: BulkOperationMessage,
}

/// `0x77fef0`: `step = total / 200`; the bar and the UPDATE push take the item start when
/// `step < 2` (total ≤ 399) or `handled % step == step >> 1`.
fn pushes_progress(handled: usize, total: usize) -> bool {
    let step = total / 200;
    step < 2 || handled % step == step / 2
}

impl BatchSeen {
    /// The UPDATE pushes of the item starts after the last one read, up to `handled`.
    fn progress(&mut self, handled: usize, pushes: &mut Vec<BulkOperationMessage>) {
        for handled in self.handled + 1..=handled {
            if pushes_progress(handled, self.total) {
                let progress = handled as f32 / self.total as f32;
                self.read.progress = Some(progress);
                pushes.push(BulkOperationMessage {
                    action: Some(MessageAction::Update.into()),
                    progress: Some(progress),
                    ..Default::default()
                });
            }
        }
        self.handled = self.handled.max(handled);
    }
}

/// The type noun `0x7ece20`: 0 "Presets", 1 "IRs", 2 "Neural Captures".
fn noun(kind: ProductKind) -> &'static str {
    match kind {
        ProductKind::Preset => "Presets",
        ProductKind::ImpulseResponse => "IRs",
        ProductKind::NeuralCapture => "Neural Captures",
    }
}

/// The cloud folder of `kind` (`0x765780`), `is_cloud` (O: key the kind text, name the type
/// noun; stock reads `CloudProductFolder +0x44`, I).
fn cloud_folder(kind: ProductKind) -> FolderInfo {
    FolderInfo {
        key: Some(kind.as_str().to_owned()),
        name: Some(noun(kind).to_owned()),
        is_factory: Some(false),
        is_downloads: Some(false),
        is_user_default: Some(false),
        is_cloud: Some(true),
        is_favorites: Some(false),
        is_plugin: Some(false),
        ..Default::default()
    }
}

/// The start push `0xf1cec(ctl, text, 1, 1, source, target)` of a run; `operation_type` is the
/// source folder type (vt+0x30), the run's kind. Texts (C): upload `0x785dcc` "Uploading %1,
/// please wait." (%1 "Presets" for type 0, else "Neural Captures"); DELETE `0x785b74`
/// "Removing %1, please wait." (%1 the noun `0x7ece20`); capture or IR download `0x786264`
/// "Downloading %1, please wait." (%1 "Neural Captures" for a type 2 target, else "IRs");
/// preset download `0x786ac4` "%1 presets, please wait." with %1 "Downloading".
fn start(view: &BatchView, state: &State, cipher: Option<&DeviceCipher>) -> BulkOperationMessage {
    let kind = view.kind;
    let (text, source, target) = match &view.operation {
        BatchOperation::Upload => {
            let noun = match kind {
                ProductKind::Preset => "Presets",
                _ => "Neural Captures",
            };
            let source =
                (view.source.as_deref()).and_then(|key| files::folder_info(state, cipher, key));
            let text = format!("Uploading {noun}, please wait.");
            (text, source, Some(cloud_folder(kind)))
        }
        BatchOperation::Delete => {
            let text = format!("Removing {}, please wait.", noun(kind));
            (text, Some(cloud_folder(kind)), None)
        }
        BatchOperation::Download { setlist: None } => {
            let (noun, media) = match kind {
                ProductKind::NeuralCapture => ("Neural Captures", ProductType::Capture),
                _ => ("IRs", ProductType::Ir),
            };
            let target = files::saving_destination_info(state, cipher, media);
            let text = format!("Downloading {noun}, please wait.");
            (text, Some(cloud_folder(kind)), target)
        }
        BatchOperation::Download {
            setlist: Some(name),
        } => {
            let target = files::setlist_info(state, cipher, name);
            let text = "Downloading presets, please wait.".to_owned();
            (text, Some(cloud_folder(kind)), target)
        }
    };
    let operation_type = match kind {
        ProductKind::Preset => 0,
        ProductKind::ImpulseResponse => 1,
        ProductKind::NeuralCapture => 2,
    };
    BulkOperationMessage {
        action: Some(MessageAction::Create.into()),
        progress_message: Some(text),
        blocking: Some(true),
        cancellable: Some(true),
        progress: Some(0.0),
        operation_type: Some(operation_type),
        source_folder: source,
        destination_folder: target,
        ..Default::default()
    }
}

/// The BulkOperation (57) pushes around a folder DELETE (`0x77df68`, C; File DELETE of a
/// media or plugin folder, `0x250090` case 2): before the delete CREATE "Removing, please
/// wait." with `0xf1cec(ctl, text, 1, 0, folder, 0)` (blocking, not cancellable, progress 0,
/// the folder as source, its type as `operation_type`), after it DELETE `finished`
/// (`0xf29b8`), whatever the delete did. Stock runs all three in one call, so no READ sees
/// the run.
pub(super) fn folder_removal(folder: FolderInfo, file_type: i32) -> [Effect; 2] {
    let start = BulkOperationMessage {
        action: Some(MessageAction::Create.into()),
        progress_message: Some("Removing, please wait.".to_owned()),
        blocking: Some(true),
        cancellable: Some(false),
        progress: Some(0.0),
        operation_type: Some(file_type),
        source_folder: Some(folder),
        ..Default::default()
    };
    let finished = BulkOperationMessage {
        action: Some(MessageAction::Delete.into()),
        finished: Some(true),
        ..Default::default()
    };
    [start, finished].map(|message| Effect::ControlNotify(Box::new(message.into())))
}

impl Control {
    /// The end of the last run (`Services::last_batch_end`), read before
    /// [`Self::bulk_run`] in each device step.
    pub fn set_batch_end(&mut self, end: Option<BatchEnd>) {
        self.batch_end = end;
    }

    /// The running run (`Services::bulk_run`), read after each device step: a new run pushes
    /// CREATE, each new item start that passes the step rule UPDATE, the end of the run
    /// DELETE `finished`. Nothing goes out while Control is not connected; READ still reports
    /// a running run after a connect (`0xed8b4` stores it first).
    pub fn bulk_run(
        &mut self,
        view: Option<BatchView>,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, ControlError> {
        let mut pushes = Vec::new();
        if let Some(mut seen) =
            (self.batch).take_if(|seen| view.as_ref().map(|v| v.serial) != Some(seen.serial))
        {
            if (self.batch_end).is_some_and(|end| end.serial == seen.serial && end.success) {
                let total = seen.total;
                seen.progress(total, &mut pushes);
            }
            pushes.push(BulkOperationMessage {
                action: Some(MessageAction::Delete.into()),
                finished: Some(true),
                ..Default::default()
            });
        }
        if let Some(view) = view {
            let seen = self.batch.get_or_insert_with(|| {
                let start = start(&view, state, self.media_cipher.as_ref());
                pushes.push(start.clone());
                BatchSeen {
                    serial: view.serial,
                    handled: 0,
                    total: view.total,
                    read: BulkOperationMessage {
                        action: None,
                        ..start
                    },
                }
            });
            seen.progress(view.handled, &mut pushes);
        }
        if !self.connected() {
            return Ok(Vec::new());
        }
        let mut effects = Vec::new();
        for push in pushes {
            let push = Notification::Update(push.into());
            effects.extend(self.with_state(state, now, |link, model, stages| {
                link.notify(push, now, model, stages)
            })?);
        }
        Ok(effects)
    }
}

#[cfg(test)]
mod tests {
    use super::pushes_progress;

    /// `0x77fef0`: every item start below 400 items; from 400 on one in `total / 200`, the one
    /// whose remainder is half the step (re-r44 §2 step 2; its "3, 8, 13" example is wrong).
    #[test]
    fn progress_pushes_follow_the_stock_step_rule() {
        assert!((1..=399).all(|handled| pushes_progress(handled, 399)));
        let pushed = |total| -> Vec<usize> {
            (1..=13)
                .filter(|&handled| pushes_progress(handled, total))
                .collect()
        };
        assert_eq!(pushed(1000), [2, 7, 12]);
        assert_eq!(pushed(400), [1, 3, 5, 7, 9, 11, 13]);
    }
}

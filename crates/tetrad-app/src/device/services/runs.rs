//! Multi Select runs as Cortex Control BulkOperation (57) sees them ([`Services::bulk_run`]):
//! the batch upload, the multi-key cloud DELETE and the multi-key downloads.
//!
//! Stock runs each in `Multiselection` (`0x8472dc`, C): one run at a time (`+0x55`, read by
//! `0x843c78`), one item after the other, the stop flag `+0x7c` checked before each item, a
//! progress signal at each item start (`0x9f2b48(handled, total)`, 1-based, → `0x77fef0`), and
//! `actionsFinished` (`0x9f2b9c(.., !stop, fromControl)`) with the done closure at the end.
//! Loops: DELETE `0x84be40`, capture and IR download `0x845370`, preset download `0x84f030`,
//! upload `0x847934` / `0x848308` (the batch, [`super::uploads`]).

use super::*;

/// What a run does: its BulkOperation (57) text and folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchOperation {
    /// `0x785dcc`: [`BatchView::source`] into the cloud folder.
    Upload,
    /// Captures or IRs (`0x786264`, Control; `0x7867c4`, Directory) into the saving
    /// destination, or presets (`0x786ac4`) into the user setlist named `setlist`.
    Download { setlist: Option<String> },
    /// `0x785b74`: products out of the cloud folder.
    Delete,
}

/// The running run ([`Services::bulk_run`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchView {
    /// The [`BatchEnd::serial`] this run gets when it ends: the same for the whole run.
    pub serial: u64,
    pub operation: BatchOperation,
    /// `operation_type` (vt+0x30 of the source folder): 0 presets, 1 IRs, 2 captures.
    pub kind: ProductKind,
    /// The started items, the one in flight included (1-based: the loops count the item now
    /// starting).
    pub handled: usize,
    pub total: usize,
    /// An upload's selection folder (`0x843cb4`, C for Multi Select): for presets the folder
    /// of the first preset file, a setlist folder key (I: a batch is one folder's selection).
    /// `None` for captures (I: the user store) and for every other operation.
    pub source: Option<String>,
}

/// The end of a run ([`Services::last_batch_end`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchEnd {
    /// Counts the runs, from 1: the serial of the run that ended.
    pub serial: u64,
    pub kind: ProductKind,
    /// No stop flag (C: success → `0x75bc20`, else `0x75c018`). O: a run that a dropped or
    /// failed request ended is no success either.
    pub success: bool,
    pub from_control: bool,
}

/// The progress of a run other than the batch upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RunState {
    pub(super) serial: u64,
    pub(super) total: usize,
    pub(super) from_control: bool,
    /// The stop flag `+0x7c`.
    pub(super) cancel: bool,
}

/// A Cortex Control multi-key DELETE (`0x785b74` → `0x84c614` → `0x84be40`, C): one
/// `DELETE user/products/<id>` at a time; the reply, good or not, starts the next.
#[derive(Debug)]
pub(super) struct DeleteRun {
    pub(super) run: RunState,
    pub(super) kind: ProductKind,
    /// The ids not started yet, in request order.
    pub(super) waiting: VecDeque<String>,
    /// The id whose request is out.
    pub(super) current: Option<String>,
    pub(super) handled: usize,
}

impl Services {
    /// The running run for the Cortex Control BulkOperation (57) pushes and READ: the batch
    /// upload, a multi-key DELETE, a capture or IR download batch, or a preset download of
    /// more than one item. `None`: no run. Only one runs at a time ([`Self::bulk_running`]).
    pub fn bulk_run(&self) -> Option<BatchView> {
        if let Some(view) = self.upload_batch() {
            return Some(view);
        }
        let view = |run: &RunState, operation, kind, handled: usize| BatchView {
            serial: run.serial,
            operation,
            kind,
            handled: handled.min(run.total),
            total: run.total,
            source: None,
        };
        if let Some(delete) = &self.delete_run {
            return Some(view(
                &delete.run,
                BatchOperation::Delete,
                delete.kind,
                delete.handled,
            ));
        }
        if let Some(batch) = &self.download_batch
            && let Some(run) = &batch.run
        {
            let out = (batch.ids.iter())
                .filter(|id| self.downloads_out.contains(id))
                .count();
            // The queued downloads run one after the other on the worker: the item now
            // starting is the first one not ended (O; stock starts each after the last).
            let ended = run.total.saturating_sub(out + batch.dropped);
            let handled = (ended + 1).min(run.total - batch.dropped);
            let operation = BatchOperation::Download { setlist: None };
            return Some(view(run, operation, batch.kind, handled));
        }
        let presets = self.preset_downloads.as_ref()?;
        let run = presets.run.as_ref()?;
        let operation = BatchOperation::Download {
            setlist: Some(presets.setlist.clone()),
        };
        let handled = presets.total - presets.waiting.len();
        Some(view(run, operation, ProductKind::Preset, handled))
    }

    /// A run is active (`+0x55`): a new multi-key run is refused.
    pub fn bulk_running(&self) -> bool {
        self.bulk_run().is_some()
    }

    /// Control BulkOperation (57) DELETE (`0xed8d0`) or the device Cancel (`0x843f2c`) set
    /// the stop flag `+0x7c` of the running run: the item in flight completes and the run ends
    /// before the next one with `success: false`. Downloads still queued for the worker are
    /// dropped (Tetrad queues a download batch at once). `false`: no run.
    pub fn cancel_bulk_run(&mut self) -> bool {
        if self.cancel_upload_batch() {
            return true;
        }
        if let Some(delete) = &mut self.delete_run {
            delete.run.cancel = true;
            return true;
        }
        if let Some(presets) = &mut self.preset_downloads
            && let Some(run) = &mut presets.run
        {
            run.cancel = true;
            return true;
        }
        let Some(batch) = &mut self.download_batch else {
            return false;
        };
        let Some(run) = &mut batch.run else {
            return false;
        };
        run.cancel = true;
        let queued = |call: &CloudCall| match call {
            CloudCall::Download { product_id, origin } => {
                matches!(origin, DownloadOrigin::Cloud | DownloadOrigin::Queue)
                    && batch.ids.contains(product_id)
            }
            _ => false,
        };
        let mut dropped = Vec::new();
        self.cloud_queue
            .retain(|pending| match pending.calls.as_slice() {
                [call @ CloudCall::Download { product_id, .. }] if queued(call) => {
                    dropped.push(product_id.clone());
                    false
                }
                _ => true,
            });
        batch.dropped += dropped.len();
        for id in &dropped {
            self.end_download(id);
        }
        true
    }

    /// The next run's serial ([`BatchView::serial`]).
    pub(super) fn next_run(&mut self, total: usize, from_control: bool) -> RunState {
        self.run_serial += 1;
        RunState {
            serial: self.run_serial,
            total,
            from_control,
            cancel: false,
        }
    }

    /// Records the end of `run` ([`Self::last_batch_end`]): `success` unless it was stopped.
    pub(super) fn end_run(&mut self, run: RunState, kind: ProductKind, success: bool) {
        self.batch_end = Some(BatchEnd {
            serial: run.serial,
            kind,
            success: success && !run.cancel,
            from_control: run.from_control,
        });
    }

    /// The next DELETE of the run, or its end (stop flag set or no id left; `0x84be40`).
    pub(super) fn next_delete(
        &mut self,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Vec<Effect> {
        let Some(delete) = &mut self.delete_run else {
            return Vec::new();
        };
        delete.current = None;
        match delete.waiting.pop_front().filter(|_| !delete.run.cancel) {
            Some(cloud_id) => {
                delete.handled += 1;
                delete.current = Some(cloud_id.clone());
                let call = CloudCall::DeleteProduct {
                    kind: delete.kind,
                    cloud_id,
                };
                self.cloud_job(settings, vec![call], None, now)
            }
            None => {
                self.stop_delete_run(true);
                Vec::new()
            }
        }
    }

    /// Ends the DELETE run now; the ids not started stay in the cloud.
    pub(super) fn stop_delete_run(&mut self, success: bool) {
        if let Some(delete) = self.delete_run.take() {
            self.end_run(delete.run, delete.kind, success);
        }
    }

    /// The reply to the DELETE of `cloud_id` (good or not, `0x84c4ec` / `0x84c580`): the next
    /// one starts when it was the run's.
    pub(super) fn delete_ended(
        &mut self,
        cloud_id: &str,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Vec<Effect> {
        let current = (self.delete_run.as_ref())
            .is_some_and(|delete| delete.current.as_deref() == Some(cloud_id));
        match current {
            true => self.next_delete(settings, now),
            false => Vec::new(),
        }
    }

    /// A download batch whose last download ended (a reply, a dropped call, a lost job, a
    /// stop): its run ends. The batch itself stays for the name-taken text of its stores
    /// ([`Self::capture_name_taken`]).
    pub(super) fn check_download_run(&mut self) {
        let Some(batch) = &mut self.download_batch else {
            return;
        };
        let out = &self.downloads_out;
        if batch.ids.iter().any(|id| out.contains(id)) {
            return;
        }
        if let Some(run) = batch.run.take() {
            let kind = batch.kind;
            self.end_run(run, kind, true);
        }
    }
}

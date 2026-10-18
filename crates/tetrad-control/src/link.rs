//! One Cortex Control link: report reassembly, stock receive stages, session
//! rules, the per-type dispatcher and outbound encoding. No transport: report
//! bodies arrive from and leave to the caller. Wave 2 carries them in the DSP
//! HID frames (`tetrad_dsp_protocol::hid`, type 0x27 out); the incoming queue
//! that holds reports until the DSP is ready (`0x6e0c10`) belongs there too.

use std::time::Duration;

use crate::codecs::{CodecLimits, CortexMessageType, EnumValue, MessageAction, SuspendReason};
use crate::device::DeviceModel;
use crate::dispatch::{self, Context};
use crate::outbox::Outbox;
use crate::pipeline::{MAX_MESSAGE_TYPE, PipelineConfig, PipelineError, SendLimits, Stages};
use crate::session::{Session, SessionError, SessionEvent};
use crate::{Limits, REPORT_BYTES, Reassembler};

pub use crate::dispatch::{
    BACKUP_CHUNK_BYTES, ConfirmationRequest, DispatchState, HeldGrid, MAX_BACKUP_BYTES, NotSent,
    Notice, Notification, Periodic, SUSPENDED_RECEIVE, SUSPENDED_SEND, Suspension, Timers,
    UndoFrames,
};

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error(transparent)]
    Report(#[from] crate::Error),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Pipeline(#[from] PipelineError),
}

#[derive(Debug, Clone, Copy)]
pub struct LinkLimits {
    pub reports: Limits,
    /// Inbound and outbound protobuf payload bound (owned policy). Presets,
    /// IRs and ModelRepo are larger than the 16 KiB codec default.
    pub payload: CodecLimits,
}

impl Default for LinkLimits {
    fn default() -> Self {
        Self {
            reports: Limits::default(),
            payload: CodecLimits {
                max_payload_bytes: 8 * 1024 * 1024,
                max_fields: 1 << 20,
                max_depth: 32,
            },
        }
    }
}

/// Everything one call produced, in order. `reports` are complete 128-byte
/// report bodies for the HID transport.
#[derive(Debug, Default, PartialEq)]
pub struct Step {
    pub reports: Vec<[u8; REPORT_BYTES]>,
    pub sent: Vec<CortexMessageType>,
    pub session: Vec<SessionEvent>,
    pub notices: Vec<Notice>,
    /// A receiver ran for a decoded request: its `request_id` (zero when absent). The
    /// device sends what it applies after the step inside this request
    /// ([`ControlLink::notify_for`]).
    pub request: Option<u64>,
    /// The message a receive step completed, for diagnostics (the device's `SLOW` pass line).
    pub received: Option<Received>,
}

/// The type and action of a received message. Displays as `File Read`; a message that did
/// not decode shows its type only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received {
    pub message_type: u32,
    pub action: Option<EnumValue<MessageAction>>,
}

impl std::fmt::Display for Received {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match CortexMessageType::try_from(self.message_type) {
            Ok(known) => write!(f, "{known:?}")?,
            Err(_) => write!(f, "type {}", self.message_type)?,
        }
        match self.action {
            Some(EnumValue::Known(action)) => write!(f, " {action:?}"),
            Some(EnumValue::Unknown(value)) => write!(f, " action {value}"),
            None => Ok(()),
        }
    }
}

pub struct ControlLink {
    reassembler: Reassembler,
    pipeline: PipelineConfig,
    session: Session,
    state: DispatchState,
    limits: LinkLimits,
}

impl ControlLink {
    pub fn new(session: Session, limits: LinkLimits) -> Self {
        Self {
            reassembler: Reassembler::new(limits.reports),
            pipeline: PipelineConfig::new(limits.payload.max_payload_bytes),
            session,
            state: DispatchState::default(),
            limits,
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn set_peer_accepts_compression(&mut self, accepts: bool) {
        self.pipeline.peer_accepts_compression = accepts;
    }

    pub fn state(&self) -> &DispatchState {
        &self.state
    }

    /// Periodic sender intervals (owned configuration).
    pub fn timers_mut(&mut self) -> &mut Timers {
        &mut self.state.timers
    }

    /// Accepts one 128-byte report body. A completed message runs through the
    /// receive stages and its receiver. Framing errors reset partial input
    /// and return `Err`; payload problems are notices, as stock logs them.
    pub fn receive_report(
        &mut self,
        report: &[u8],
        now: Duration,
        model: &mut dyn DeviceModel,
        stages: Stages<'_>,
    ) -> Result<Step, LinkError> {
        self.session.advance(now)?;
        let Some(envelope) = self.reassembler.push(report)? else {
            return Ok(Step::default());
        };
        let message_type = envelope.message_type;
        let received = self.pipeline.receive(envelope, stages);
        Ok(self.run(now, stages, model, |ctx, model| {
            ctx.step.received = Some(Received {
                message_type,
                action: None,
            });
            if message_type <= MAX_MESSAGE_TYPE && message_type != 32 {
                ctx.session.renew(&mut ctx.step.session);
            }
            match received {
                Ok(Ok(inbound)) => dispatch::dispatch(ctx, model, inbound),
                Ok(Err(dropped)) => ctx.step.notices.push(Notice::Dropped(dropped)),
                Err(error) => ctx.step.notices.push(Notice::Stage {
                    message_type,
                    error,
                }),
            }
        }))
    }

    /// Timer service: keep-alive expiry and the subscribed periodic senders.
    pub fn tick(
        &mut self,
        now: Duration,
        model: &mut dyn DeviceModel,
        stages: Stages<'_>,
    ) -> Result<Step, LinkError> {
        self.session.advance(now)?;
        Ok(self.run(now, stages, model, |ctx, model| {
            ctx.session.expire(&mut ctx.step.session);
            dispatch::tick(ctx, model);
        }))
    }

    /// USB unplug runs the keep-alive expiry path (`0x6e2b8c` → `0x6e294c`).
    pub fn usb_unplugged(
        &mut self,
        now: Duration,
        model: &mut dyn DeviceModel,
        stages: Stages<'_>,
    ) -> Result<Step, LinkError> {
        self.session.advance(now)?;
        Ok(self.run(now, stages, model, |ctx, _| {
            ctx.session.drop_keep_alive(&mut ctx.step.session);
        }))
    }

    /// Sends one unsolicited device update. Stock senders check `connected`
    /// first; a disconnected link sends nothing.
    pub fn notify(
        &mut self,
        notification: Notification,
        now: Duration,
        model: &mut dyn DeviceModel,
        stages: Stages<'_>,
    ) -> Result<Step, LinkError> {
        self.session.advance(now)?;
        Ok(self.run(now, stages, model, |ctx, _| {
            dispatch::notify(ctx, notification)
        }))
    }

    /// [`Self::notify`] inside the context of request `request_id`: every message echoes the
    /// ID as if its receiver sent it (`0x6e01d8` stamps all types but 47 and 29–31). The
    /// device uses it for a deferred reply ([`crate::device::Reply::Deferred`]) and for the
    /// pushes the request caused.
    pub fn notify_for(
        &mut self,
        request_id: u64,
        notification: Notification,
        now: Duration,
        model: &mut dyn DeviceModel,
        stages: Stages<'_>,
    ) -> Result<Step, LinkError> {
        self.session.advance(now)?;
        Ok(self.run(now, stages, model, |ctx, _| {
            ctx.outbox.current_request_id = request_id;
            dispatch::notify(ctx, notification);
            ctx.outbox.current_request_id = 0;
        }))
    }

    /// The due time of the earliest held Grid echo.
    pub fn next_grid_echo(&self) -> Option<Duration> {
        self.state.grid_echoes.iter().map(|held| held.due).min()
    }

    /// Sends the held Grid echoes that are due at `now`; [`Self::tick`] does it too. The
    /// stock timer fires 100 ms after the edit (`0x262b1c` → `QTimer::start`, `0x262cd0`),
    /// so a device calls this at [`Self::next_grid_echo`].
    pub fn flush_grid_echoes(
        &mut self,
        now: Duration,
        model: &mut dyn DeviceModel,
        stages: Stages<'_>,
    ) -> Result<Step, LinkError> {
        self.session.advance(now)?;
        Ok(self.run(now, stages, model, |ctx, _| {
            dispatch::flush_grid_echoes(ctx)
        }))
    }

    /// Opens a device dialog in Control. Returns the `confirmation_id`, or
    /// `None` when it was not sent; the answer reaches
    /// `DeviceModel::confirmation_answered`.
    pub fn request_confirmation(
        &mut self,
        request: ConfirmationRequest,
        now: Duration,
        model: &mut dyn DeviceModel,
        stages: Stages<'_>,
    ) -> Result<(Option<u64>, Step), LinkError> {
        self.session.advance(now)?;
        let mut id = None;
        let step = self.run(now, stages, model, |ctx, _| {
            id = dispatch::confirm(ctx, request);
        });
        Ok((id, step))
    }

    /// Ends a pending Normal-mode Version exchange. `xxtea_key` encrypts
    /// `key_material` into the reply's `comms_version`.
    pub fn complete_version(
        &mut self,
        xxtea_key: &[u8; 16],
        key_material: &[u8],
        now: Duration,
        model: &mut dyn DeviceModel,
        stages: Stages<'_>,
    ) -> Result<Step, LinkError> {
        self.session.advance(now)?;
        Ok(self.run(now, stages, model, |ctx, model| {
            dispatch::complete_version(ctx, model, xxtea_key, key_material)
        }))
    }

    /// The stock full state push (`0x29a4f8`) after factory restore, settings
    /// reset or resume. `files` adds the library listing first.
    pub fn resync(
        &mut self,
        files: bool,
        now: Duration,
        model: &mut dyn DeviceModel,
        stages: Stages<'_>,
    ) -> Result<Step, LinkError> {
        self.session.advance(now)?;
        Ok(self.run(now, stages, model, |ctx, model| {
            dispatch::resync(ctx, model, files, false)
        }))
    }

    /// The factory-restore resync (`0x4de79c` → `0x29a4f8(.., 1, 1)`): the listing, and
    /// CloudLogin DELETE last.
    pub fn factory_resync(
        &mut self,
        now: Duration,
        model: &mut dyn DeviceModel,
        stages: Stages<'_>,
    ) -> Result<Step, LinkError> {
        self.session.advance(now)?;
        Ok(self.run(now, stages, model, |ctx, model| {
            dispatch::resync(ctx, model, true, true)
        }))
    }

    /// Device-side suspension (`0x3fbb90`): sends SuspendConnection{true},
    /// then a Download suspension blocks all but the backup and forward
    /// types in both directions.
    pub fn suspend(
        &mut self,
        kind: Suspension,
        reason: SuspendReason,
        now: Duration,
        model: &mut dyn DeviceModel,
        stages: Stages<'_>,
    ) -> Result<Step, LinkError> {
        self.session.advance(now)?;
        let step = self.run(now, stages, model, |ctx, _| {
            dispatch::send_suspend(ctx, true, reason)
        });
        if kind == Suspension::Download {
            let all = 0..=MAX_MESSAGE_TYPE;
            self.pipeline.blocked_inbound = all
                .clone()
                .filter(|t| !SUSPENDED_RECEIVE.contains(t))
                .collect();
            self.pipeline.blocked_outbound = all.filter(|t| !SUSPENDED_SEND.contains(t)).collect();
            self.state.download_suspended = true;
            self.session.pause_keep_alive();
        }
        Ok(step)
    }

    /// Resume (`0x3fbb90` → idle, `0x3f553c`): after a Download suspension
    /// clear both blocked sets and run the full resync with the listing;
    /// then send SuspendConnection{false}.
    pub fn resume(
        &mut self,
        reason: SuspendReason,
        now: Duration,
        model: &mut dyn DeviceModel,
        stages: Stages<'_>,
    ) -> Result<Step, LinkError> {
        self.session.advance(now)?;
        let download = std::mem::take(&mut self.state.download_suspended);
        if download {
            self.pipeline.blocked_inbound.clear();
            self.pipeline.blocked_outbound.clear();
            self.session.resume_keep_alive();
        }
        Ok(self.run(now, stages, model, |ctx, model| {
            if download {
                dispatch::resync(ctx, model, true, false);
            }
            dispatch::send_suspend(ctx, false, reason);
        }))
    }

    /// Queues a device backup for Control in 150 000-byte LocalBackup
    /// UPDATE chunks, one per backup timer period (`0x17ddfc`); the last
    /// carries `is_last_chunk = true`.
    pub fn send_local_backup(&mut self, backup_json: &str) {
        dispatch::queue_backup(&mut self.state, backup_json);
    }

    /// Ends input. An unfinished multi-report message is an error.
    pub fn finish(&self) -> Result<(), LinkError> {
        Ok(self.reassembler.finish()?)
    }

    /// Runs one operation with a fresh outbox, then the connection edges it
    /// caused (connect hook or stock teardown).
    fn run(
        &mut self,
        now: Duration,
        stages: Stages<'_>,
        model: &mut dyn DeviceModel,
        operation: impl FnOnce(&mut Context<'_, '_>, &mut dyn DeviceModel),
    ) -> Step {
        let mut step = Step::default();
        let mut outbox = Outbox::new(
            &self.pipeline,
            stages,
            SendLimits {
                payload: self.limits.payload,
                reports: self.limits.reports,
            },
        );
        let mut ctx = Context {
            session: &mut self.session,
            state: &mut self.state,
            outbox: &mut outbox,
            step: &mut step,
            limits: self.limits.payload,
            now,
            edges: 0,
        };
        operation(&mut ctx, model);
        dispatch::connection_edges(&mut ctx, model);
        let (reports, sent) = outbox.finish();
        step.reports = reports;
        step.sent = sent;
        step
    }
}

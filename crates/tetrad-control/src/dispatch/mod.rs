//! Stock per-type receiver behavior (dispatcher `0x0028e8ac`) and the
//! device-side senders around it.
//!
//! Shared stock rules:
//! - A parse failure is logged; nothing is sent (no GenericError).
//! - Type 0 has no case and is ignored.
//! - The request's `request_id` (zero when absent) is the current request
//!   context while its receiver runs; every message sent synchronously in
//!   that window echoes it, except Confirmation (47) and forwards 29–31.
//!   Deferred replies (timers, posted events, async renders) do not.
//! - Normal sends need `connected` and an enabled sender; production
//!   automation mode disables the ordinary senders (`0x6e192c`).
//! - State replies carry action UPDATE.

mod receivers;

use std::collections::BTreeSet;
use std::time::Duration;

use crate::block::{self, BlockError};
use crate::codecs::{
    CodecError, CodecLimits, CompilerInhibitedModulesMessage, ConfirmationAction,
    ConfirmationMessage, ConnectionMessage, ControlMessage, CortexMessageType,
    DefaultParametersMessage, DefaultParametersModel, EnumValue, GenericErrorCode,
    GenericErrorMessage, GridUpdateType, IOMeterMessage, MessageAction, ModeMessage,
    ModelPresetMessage, PinnedModelsMessage, PresetDirtyMessage, RecentFavoriteType,
    RecentsFavoritesMessage, SceneMessage, TunerMessage, UndoRedoMessage, VersionMessage,
};
use crate::device::{
    DelayedGrid, DeviceError, DeviceModel, DeviceResult, GRID_ECHO_DELAY, GridEcho, MeterTarget,
    mask_version,
};
use crate::link::Step;
use crate::outbox::Outbox;
use crate::pipeline::{Dropped, Inbound, PipelineError};
use crate::session::{Session, SessionError, SessionEvent};

/// Non-fatal outcomes. Stock logs these and keeps the link open.
#[derive(Debug, PartialEq)]
pub enum Notice {
    Dropped(Dropped),
    Stage {
        message_type: u32,
        error: PipelineError,
    },
    Parse {
        message_type: u32,
        error: CodecError,
    },
    /// Type 0: the dispatcher has no case.
    NoReceiver(u32),
    /// The stock receiver has no branch for this action or value.
    Ignored {
        message_type: CortexMessageType,
        action: Option<i32>,
    },
    /// A normal-path send was skipped, as stock logs "dropped message".
    NotSent {
        message_type: CortexMessageType,
        reason: NotSent,
    },
    /// The last outgoing LocalBackup chunk was sent; the app may resume.
    LocalBackupSent,
    Device(DeviceError),
    Session(SessionError),
    Send(PipelineError),
    /// A resync block the stock serializer form cannot express (`block::serialize`).
    Block(BlockError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotSent {
    Disconnected,
    /// Production automation mode disables the ordinary senders.
    SenderDisabled,
}

/// A receiver failure. Messages already sent stay sent, as in stock.
#[derive(Debug)]
pub(crate) enum Fault {
    Device(DeviceError),
    Send(PipelineError),
    Session(SessionError),
    Block(BlockError),
}

impl From<BlockError> for Fault {
    fn from(error: BlockError) -> Self {
        Self::Block(error)
    }
}

impl From<DeviceError> for Fault {
    fn from(error: DeviceError) -> Self {
        Self::Device(error)
    }
}

impl From<PipelineError> for Fault {
    fn from(error: PipelineError) -> Self {
        Self::Send(error)
    }
}

impl From<SessionError> for Fault {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

impl From<Fault> for Notice {
    fn from(fault: Fault) -> Self {
        match fault {
            Fault::Device(error) => Self::Device(error),
            Fault::Send(error) => Self::Send(error),
            Fault::Session(error) => Self::Session(error),
            Fault::Block(error) => Self::Block(error),
        }
    }
}

pub(crate) type Handled = Result<(), Fault>;

pub(crate) struct Context<'s, 'o> {
    pub session: &'s mut Session,
    pub state: &'s mut DispatchState,
    pub outbox: &'s mut Outbox<'o>,
    pub step: &'s mut Step,
    pub limits: CodecLimits,
    pub now: Duration,
    /// Session events already handled by `connection_edges`.
    pub edges: usize,
}

impl Context<'_, '_> {
    /// Normal path (`0x29acd0`): connected and sender enabled.
    fn send(&mut self, message: impl Into<ControlMessage>) -> Handled {
        let message = message.into();
        match self.blocked_reason() {
            Some(reason) => self.step.notices.push(Notice::NotSent {
                message_type: message.message_type(),
                reason,
            }),
            None => self.outbox.send(message)?,
        }
        Ok(())
    }

    /// Normal path with options other than the type's stock flags.
    fn send_options(
        &mut self,
        message: impl Into<ControlMessage>,
        options: crate::pipeline::SendOptions,
    ) -> Handled {
        let message = message.into();
        match self.blocked_reason() {
            Some(reason) => self.step.notices.push(Notice::NotSent {
                message_type: message.message_type(),
                reason,
            }),
            None => self.outbox.send_with(message, options, 0)?,
        }
        Ok(())
    }

    /// Production-automation senders stay enabled in PA mode; the transport
    /// still needs a connection.
    fn send_pa(&mut self, message: impl Into<ControlMessage>) -> Handled {
        let message = message.into();
        if self.session.connected() {
            self.outbox.send(message)?;
        } else {
            self.step.notices.push(Notice::NotSent {
                message_type: message.message_type(),
                reason: NotSent::Disconnected,
            });
        }
        Ok(())
    }

    /// Normal path with an explicit request ID (a deferred reply).
    fn send_as(&mut self, message: impl Into<ControlMessage>, request_id: u64) -> Handled {
        let message = message.into();
        match self.blocked_reason() {
            Some(reason) => self.step.notices.push(Notice::NotSent {
                message_type: message.message_type(),
                reason,
            }),
            None => {
                let options = crate::pipeline::stock_send_options(message.message_type());
                self.outbox.send_with(message, options, request_id)?;
            }
        }
        Ok(())
    }

    fn send_scene(&mut self, message: SceneMessage) -> Handled {
        match self.blocked_reason() {
            Some(reason) => self.step.notices.push(Notice::NotSent {
                message_type: CortexMessageType::Scene,
                reason,
            }),
            None => self.outbox.send_scene(message)?,
        }
        Ok(())
    }

    fn blocked_reason(&self) -> Option<NotSent> {
        if !self.session.connected() {
            Some(NotSent::Disconnected)
        } else if self.state.production_automation {
            Some(NotSent::SenderDisabled)
        } else {
            None
        }
    }

    fn ignored(
        &mut self,
        message_type: CortexMessageType,
        action: Option<EnumValue<MessageAction>>,
    ) -> Handled {
        self.step.notices.push(Notice::Ignored {
            message_type,
            action: action.map(EnumValue::number),
        });
        Ok(())
    }

    fn report(&mut self, result: Handled) {
        if let Err(fault) = result {
            self.step.notices.push(fault.into());
        }
    }
}

/// Stock IOMeter push period (`0x6e0ae0`).
pub const IO_METER_INTERVAL: Duration = Duration::from_millis(100);
/// Stock CPULoad push period (`0x6e0128`).
pub const CPU_LOAD_INTERVAL: Duration = Duration::from_millis(1000);
/// Stock Diagnostics push period (`0x6dfdd0`).
pub const DIAGNOSTICS_INTERVAL: Duration = Duration::from_millis(1000);
/// Stock GridModelMeter push period (`0x110bcc`).
pub const GRID_METER_INTERVAL: Duration = Duration::from_millis(100);
/// Stock LocalBackup chunk period (`0x17e204`, `0x17e5d8`).
pub const LOCAL_BACKUP_INTERVAL: Duration = Duration::from_millis(1000);

/// One periodic stock sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Periodic {
    pub interval: Duration,
    pub enabled: bool,
    next: Option<Duration>,
}

impl Periodic {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            enabled: false,
            next: None,
        }
    }

    /// Due once per interval while enabled; a timer with no start is due at once.
    fn due(&mut self, now: Duration) -> bool {
        if !self.enabled || self.interval.is_zero() {
            self.next = None;
            return false;
        }
        let next = *self.next.get_or_insert(now);
        if now < next {
            return false;
        }
        self.next = now.checked_add(self.interval);
        true
    }

    /// Starts the timer; its first send is one interval after `now` (`QTimer::start`).
    pub(crate) fn start(&mut self, now: Duration) {
        self.enabled = true;
        self.next = now.checked_add(self.interval);
    }

    fn stop(&mut self) {
        *self = Self::new(self.interval);
    }
}

/// Stock timers, started and stopped by CREATE/DELETE requests: IOMeter (5)
/// `sendIOMeters` (+0x248), CPULoad (26) `sendCPULoad` (+0x2b4), Diagnostics
/// (7) `sendDiagnostics` (+0x10), GridModelMeter (37) `sendGridModelMeters`
/// (+0x3b0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timers {
    pub io_meters: Periodic,
    pub cpu_load: Periodic,
    pub diagnostics: Periodic,
    pub grid_meters: Periodic,
    /// LocalBackup send timer (`0x17ddfc`): one chunk per period.
    pub local_backup: Periodic,
}

impl Default for Timers {
    fn default() -> Self {
        Self {
            io_meters: Periodic::new(IO_METER_INTERVAL),
            cpu_load: Periodic::new(CPU_LOAD_INTERVAL),
            diagnostics: Periodic::new(DIAGNOSTICS_INTERVAL),
            grid_meters: Periodic::new(GRID_METER_INTERVAL),
            local_backup: Periodic::new(LOCAL_BACKUP_INTERVAL),
        }
    }
}

/// Receiver state that stock keeps between messages.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DispatchState {
    /// GridModelMeter subscriptions; the push timer runs while non-empty.
    pub grid_meters: BTreeSet<MeterTarget>,
    /// Tuner meter subscription (`enable_meter`).
    pub tuner_meter: bool,
    /// Control's ShowTuner flag (`USBHIDController + 0x3cc`).
    pub tuner_shown: bool,
    pub timers: Timers,
    /// Last IOMeter block sent; stock sends only changed blocks (`0x6e0040`).
    pub io_meter_cache: Option<IOMeterMessage>,
    /// Open device dialogs by `confirmation_id` (`0xf9acc`).
    pub confirmations: BTreeSet<u64>,
    /// Post-increment counter at receiver `+0x20` (`0xf9844`).
    pub next_confirmation: u64,
    /// Device-sent HTTP forwards awaiting Control's response, by type and
    /// forward ID.
    pub forwards: BTreeSet<(CortexMessageType, u64)>,
    /// A Normal-mode Version READ waiting for its key exchange.
    pub pending_version: Option<VersionMessage>,
    /// Production automation mode (`USBHIDController + 0x40c`).
    pub production_automation: bool,
    /// Incoming LocalBackup chunks, joined until `is_last_chunk`.
    pub backup_in: String,
    /// Outgoing LocalBackup chunks, one per backup timer period.
    pub backup_out: std::collections::VecDeque<String>,
    /// A Download suspension (`0x3fbb90` state 1) blocks traffic.
    pub download_suspended: bool,
    /// UndoRedo sender cache (`0x1be1a8`, `+0x10` and `+0x1c`): the lists last sent. `None`
    /// until the first send (the ctor force flag `+0xd`).
    pub undo_frames: Option<UndoFrames>,
    /// Grid echoes the DelayedMessageHandler holds (`0x262b1c`), oldest first.
    pub grid_echoes: Vec<HeldGrid>,
}

/// One held Grid echo: due time and the request ID current when it was queued
/// (`0x6e01d8`, has-bit 4 of the stored message).
#[derive(Debug, Clone, PartialEq)]
pub struct HeldGrid {
    pub due: Duration,
    pub request_id: u64,
    pub echo: DelayedGrid,
}

/// UndoRedo frame type lists, oldest first: one `0x6476c8` type per frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UndoFrames {
    pub undo: Vec<i32>,
    pub redo: Vec<i32>,
}

/// Stock LocalBackup send chunk (`0x17ddfc`).
pub const BACKUP_CHUNK_BYTES: usize = 150_000;
/// Owned bound for an incoming backup.
pub const MAX_BACKUP_BYTES: usize = 64 * 1024 * 1024;

/// While a Download suspension is active only these types pass
/// (`0x3fbb90`): receive 29, 30, 32, 40, 41; send 29, 30, 40, 41, 56.
pub const SUSPENDED_RECEIVE: [u32; 5] = [29, 30, 32, 40, 41];
pub const SUSPENDED_SEND: [u32; 5] = [29, 30, 40, 41, 56];

/// Device-side suspension kinds of `0x3fbb90`. `Notice` (backup create,
/// rename or delete, factory reset, settings reset) only sends
/// SuspendConnection; `Download` also blocks every other type both ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suspension {
    Notice,
    Download,
}

/// SuspendConnection sender `0x138118`: field 2 suspend, field 3 reason.
pub(crate) fn send_suspend(
    ctx: &mut Context<'_, '_>,
    suspend: bool,
    reason: crate::codecs::SuspendReason,
) {
    let result = ctx.send(crate::codecs::SuspendConnectionMessage {
        suspend: Some(suspend),
        reason: Some(reason.into()),
        ..Default::default()
    });
    ctx.report(result);
}

/// Splits a backup into stock chunks at UTF-8 boundaries and starts the
/// backup timer. Chunk boundaries inside a multi-byte character move back
/// (owned; stock splits file bytes).
pub(crate) fn queue_backup(state: &mut DispatchState, mut backup_json: &str) {
    while !backup_json.is_empty() {
        let mut end = backup_json.len().min(BACKUP_CHUNK_BYTES);
        while !backup_json.is_char_boundary(end) {
            end -= 1;
        }
        let (chunk, rest) = backup_json.split_at(end);
        state.backup_out.push_back(chunk.to_owned());
        backup_json = rest;
    }
    state.timers.local_backup.enabled = !state.backup_out.is_empty();
}

/// Unsolicited device updates: stock `<Name>MessageSender` calls outside any
/// receiver, so no request ID is set unless one is given.
#[derive(Debug, Clone, PartialEq)]
pub enum Notification {
    /// A state change pushed as built (stock senders use action UPDATE), for
    /// example Mode, GeneralSettings, Grid, Looper or a finished Screenshot.
    Update(ControlMessage),
    /// Stock pushes PresetDirty only when the value changes (`0x1a6dfc`).
    PresetDirty(bool),
    /// The UndoRedo frame lists (`0x1be1a8`); lists equal to the last sent ones are not
    /// sent.
    UndoFrames(UndoFrames),
    /// Scene selection changed on the device (`0x27c3fc`).
    Scene(u32),
    /// Detected tuner pitch in Hz (`0x284bf8`); zero means no pitch. Sent
    /// only while Control is subscribed with `enable_meter`.
    TunerMeter(f32),
    /// GenericError sender `0x105d94`: CloudLoginFailed and the two excess
    /// download warnings; CloudLoginFailed carries the login request ID.
    GenericError {
        code: GenericErrorCode,
        request_id: u64,
    },
    /// A device-side HTTP forward (29, 30, 31 or 61). The forward ID in field
    /// 3 is registered so Control's response can be matched.
    Forward(ControlMessage),
}

/// A device dialog shown in Cortex Control (`0xfa48c`), for example
/// "Preset Conflict" or "Out of resources".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmationRequest {
    pub title: String,
    pub message: String,
    pub actions: Vec<ConfirmationAction>,
    /// Sent only when non-empty.
    pub ok_title: String,
    /// Sent only when non-empty.
    pub cancel_title: String,
}

pub(crate) fn notify(ctx: &mut Context<'_, '_>, notification: Notification) {
    let result = match notification {
        Notification::Update(message) => ctx.send(message),
        Notification::PresetDirty(dirty) => ctx.send(PresetDirtyMessage {
            action: Some(MessageAction::Update.into()),
            is_dirty: Some(dirty),
            ..Default::default()
        }),
        Notification::UndoFrames(frames) => send_undo_frames(ctx, frames),
        Notification::Scene(scene) => ctx.send_scene(SceneMessage {
            action: Some(MessageAction::Update.into()),
            selected_scene: Some(scene),
            ..Default::default()
        }),
        Notification::TunerMeter(hz) if ctx.state.tuner_meter && hz.is_finite() && hz >= 0.0 => ctx
            .send(TunerMessage {
                action: Some(MessageAction::Update.into()),
                meter: Some(hz),
                ..Default::default()
            }),
        Notification::TunerMeter(_) => Ok(()),
        Notification::GenericError { code, request_id } => {
            let message = GenericErrorMessage {
                error_code: Some(code.into()),
                ..Default::default()
            };
            match ctx.blocked_reason() {
                Some(reason) => {
                    ctx.step.notices.push(Notice::NotSent {
                        message_type: CortexMessageType::GenericError,
                        reason,
                    });
                    Ok(())
                }
                None => ctx
                    .outbox
                    .send_with(
                        ControlMessage::from(message),
                        Default::default(),
                        request_id,
                    )
                    .map_err(Fault::from),
            }
        }
        Notification::Forward(message) => forward(ctx, message),
    };
    ctx.report(result);
}

/// A Grid UPDATE's echoes: the receiver's own sends now, the held ones queued with the
/// current request ID; a held echo replaces a pending one for the same target.
pub(crate) fn grid_echo(ctx: &mut Context<'_, '_>, echo: GridEcho) -> Handled {
    for message in echo.now {
        ctx.send(message)?;
    }
    let request_id = ctx.outbox.current_request_id;
    for held in echo.delayed {
        let echoes = &mut ctx.state.grid_echoes;
        echoes.retain(|pending| pending.echo.target != held.target);
        echoes.push(HeldGrid {
            due: ctx.now + GRID_ECHO_DELAY,
            request_id,
            echo: held,
        });
    }
    Ok(())
}

/// `0x1be1a8`: skips lists equal to the cache unless nothing was sent yet; the cache moves
/// only when the message goes out.
fn send_undo_frames(ctx: &mut Context<'_, '_>, frames: UndoFrames) -> Handled {
    if ctx.state.undo_frames.as_ref() == Some(&frames) {
        return Ok(());
    }
    let sent = ctx.outbox.sent_count();
    ctx.send(UndoRedoMessage {
        action: Some(MessageAction::Update.into()),
        undo_frames: frames.undo.clone(),
        redo_frames: frames.redo.clone(),
        ..Default::default()
    })?;
    if ctx.outbox.sent_count() > sent {
        ctx.state.undo_frames = Some(frames);
    }
    Ok(())
}

fn forward(ctx: &mut Context<'_, '_>, message: ControlMessage) -> Handled {
    use crate::codecs::ForwardMessage;
    let id = match &message {
        ControlMessage::ProductForward(m) => m.forward_request_id(),
        ControlMessage::BackupsForward(m) => m.forward_request_id(),
        ControlMessage::LogsForward(m) => m.forward_request_id(),
        ControlMessage::UpdaterForward(m) => m.forward_request_id(),
        other => {
            return Err(
                DeviceError::Refused(other.message_type(), "not an HTTP forward".into()).into(),
            );
        }
    };
    let message_type = message.message_type();
    let sent = ctx.outbox.sent_count();
    ctx.send(message)?;
    if ctx.outbox.sent_count() > sent {
        ctx.state.forwards.insert((message_type, id.unwrap_or(0)));
    }
    Ok(())
}

/// Sends a device dialog and registers its ID; `None` when not sent.
pub(crate) fn confirm(ctx: &mut Context<'_, '_>, request: ConfirmationRequest) -> Option<u64> {
    if let Some(reason) = ctx.blocked_reason() {
        ctx.step.notices.push(Notice::NotSent {
            message_type: CortexMessageType::Confirmation,
            reason,
        });
        return None;
    }
    let id = ctx.state.next_confirmation;
    ctx.state.next_confirmation = id.wrapping_add(1);
    let non_empty = |text: String| (!text.is_empty()).then_some(text);
    let message = ConfirmationMessage {
        confirmation_id: Some(id),
        title: Some(request.title),
        msg: Some(request.message),
        actions: request.actions.into_iter().map(EnumValue::Known).collect(),
        ok_title: non_empty(request.ok_title),
        cancel_title: non_empty(request.cancel_title),
        ..Default::default()
    };
    match ctx.send(message) {
        Ok(()) => {
            ctx.state.confirmations.insert(id);
            Some(id)
        }
        Err(fault) => {
            ctx.step.notices.push(fault.into());
            None
        }
    }
}

/// Completes a pending Normal-mode Version exchange (`0x2a532c`).
pub(crate) fn complete_version(
    ctx: &mut Context<'_, '_>,
    model: &mut dyn DeviceModel,
    xxtea_key: &[u8; 16],
    key_material: &[u8],
) {
    let Some(request) = ctx.state.pending_version.take() else {
        return;
    };
    let result = receivers::complete_version(ctx, model, &request, xxtea_key, key_material);
    ctx.report(result);
}

/// Device-side resync `0x29a4f8`, run after factory restore (`0x4de79c`: listing and CloudLogin),
/// settings reset (`0x4decdc`: neither) and suspend/resume (`0x3f553c`, `0x3fbb90`: listing only).
/// Stock order: File, CloudProduct (46), RecentsFavorites (20, six sends), RecentSearches,
/// IOSettings, GlobalEQ, GeneralSettings, RecallPreset, UndoRedo (deduplicated), Mode, the mode
/// list, SetlistPosition, Scene, PinnedModels, 42 (two sends), DefaultParameters (19), ModelPreset
/// (71), GlobalTempo, Version (custom name), CloudLogin (18, `param_3`). 19 and 71 are a DELETE and
/// then one message with every entry (`0x15d890`, `0x1d037c`); their blocks go through
/// `block::serialize`. Unsupported families are skipped with a notice; 19 and 71 send nothing
/// unless all their inputs are available.
pub(crate) fn resync(
    ctx: &mut Context<'_, '_>,
    model: &mut dyn DeviceModel,
    files: bool,
    cloud_logout: bool,
) {
    let update = || Some(MessageAction::Update.into());
    if files {
        let result = model
            .file_listing()
            .map_err(Fault::from)
            .and_then(|replies| replies.into_iter().try_for_each(|reply| ctx.send(reply)));
        ctx.report(result);
    }
    fn state<M: Into<ControlMessage>>(
        message: DeviceResult<M>,
        slot: impl FnOnce(&mut M) -> &mut Option<EnumValue<MessageAction>>,
    ) -> DeviceResult<Vec<ControlMessage>> {
        message.map(|m| vec![receivers::updated(m, slot).into()])
    }
    fn send<E: Into<Fault>>(ctx: &mut Context<'_, '_>, messages: Result<Vec<ControlMessage>, E>) {
        let result = messages
            .map_err(Into::into)
            .and_then(|messages| messages.into_iter().try_for_each(|m| ctx.send(m)));
        ctx.report(result);
    }
    // `0xf5470`, sent whenever connected: the model's CloudProduct READ of the transfer state
    // (DELETE with an empty state, then UPDATE with the products in transfer); a model without
    // it sends the DELETE only.
    let transfers = crate::codecs::CloudProductMessage {
        action: Some(MessageAction::Read.into()),
        cloud_transfer_state: Some(Default::default()),
        ..Default::default()
    };
    let replies = model.cloud_product(&transfers).unwrap_or_else(|_| {
        vec![crate::codecs::CloudProductMessage {
            action: Some(MessageAction::Delete.into()),
            ..transfers
        }]
    });
    let cloud = (replies.into_iter()).try_for_each(|reply| ctx.send(reply));
    ctx.report(cloud);
    // `0x1abc68` (recents) and `0x1abcb4` (favorites) for presets, captures, then IRs: the
    // stored list as UPDATE. Inferred: the READ reply (`0x1aa02c`) holds the same items.
    for item_type in [
        RecentFavoriteType::Preset,
        RecentFavoriteType::NeuralCapture,
        RecentFavoriteType::ImpulseResponse,
    ] {
        for is_favorites in [false, true] {
            let request = RecentsFavoritesMessage {
                action: Some(MessageAction::Read.into()),
                is_favorites: Some(is_favorites),
                item_type: Some(item_type.into()),
                ..Default::default()
            };
            let list = model.recents_favorites(&request).map(|reply| {
                reply
                    .into_iter()
                    .map(|reply| {
                        RecentsFavoritesMessage {
                            action: update(),
                            is_favorites: Some(is_favorites),
                            item_type: Some(item_type.into()),
                            ..reply
                        }
                        .into()
                    })
                    .collect()
            });
            send(ctx, list);
        }
    }
    let searches = model.recent_searches().map(|searches| {
        vec![
            crate::codecs::RecentSearchesMessage {
                action: update(),
                searches,
                ..Default::default()
            }
            .into(),
        ]
    });
    send(ctx, searches);
    // An empty request is the full dump (`0x268a14`).
    send(
        ctx,
        state(model.io_settings(&Default::default()), |m| &mut m.action),
    );
    send(ctx, state(model.global_eq(), |m| &mut m.action));
    send(ctx, state(model.general_settings(), |m| &mut m.action));
    send(ctx, state(model.current_preset(), |m| &mut m.action));
    let frames = model.undo_redo().map_err(Fault::from).and_then(|m| {
        let frames = UndoFrames {
            undo: m.undo_frames,
            redo: m.redo_frames,
        };
        send_undo_frames(ctx, frames)
    });
    ctx.report(frames);
    // `0x2734e8` sends the mode (and the ATMA page), then `0x273854` the list.
    let modes = model.mode().map(|mut mode| {
        let available_modes = Some(mode.available_modes.take().unwrap_or_default());
        vec![
            receivers::updated(mode, |m| &mut m.action).into(),
            ModeMessage {
                action: update(),
                available_modes,
                ..Default::default()
            }
            .into(),
        ]
    });
    send(ctx, modes);
    send(ctx, state(model.setlist_position(), |m| &mut m.action));
    let scene = model.scene().map_err(Fault::from).and_then(|scene| {
        ctx.send_scene(SceneMessage {
            action: update(),
            selected_scene: Some(scene),
            ..Default::default()
        })
    });
    ctx.report(scene);
    let pinned = model.pinned_models().map(|(models, captures)| {
        vec![
            PinnedModelsMessage {
                action: update(),
                models,
                captures,
                ..Default::default()
            }
            .into(),
        ]
    });
    send(ctx, pinned);
    // `0x15c3f0`: `global_gate` alone (`0x15bd68`), then `global_eq` alone.
    let inhibited = model.compiler_inhibited_modules().map(|modules| {
        let message = |global_gate, global_eq| {
            CompilerInhibitedModulesMessage {
                action: update(),
                global_gate,
                global_eq,
                ..Default::default()
            }
            .into()
        };
        vec![
            message(modules.global_gate, None),
            message(None, modules.global_eq),
        ]
    });
    send(ctx, inhibited);
    // `0x162f4c`: DELETE, then `0x15d890`, whose message keeps action 0 (not on the wire).
    let defaults = default_parameters(model).map(|all_defaults| {
        vec![
            DefaultParametersMessage {
                action: Some(MessageAction::Delete.into()),
                ..Default::default()
            }
            .into(),
            DefaultParametersMessage {
                all_defaults,
                ..Default::default()
            }
            .into(),
        ]
    });
    send(ctx, defaults);
    // `0x11d324`: DELETE, then `0x1d037c(.., 1)` with action UPDATE.
    let presets = model
        .model_preset_records()
        .map_err(Fault::from)
        .and_then(|records| {
            let presets = records
                .iter()
                .map(block::model_preset_entry)
                .collect::<Result<_, _>>()?;
            Ok(vec![
                ModelPresetMessage {
                    action: Some(MessageAction::Delete.into()),
                    ..Default::default()
                }
                .into(),
                ModelPresetMessage {
                    action: update(),
                    presets,
                    ..Default::default()
                }
                .into(),
            ])
        });
    send(ctx, presets);
    send(ctx, state(model.global_tempo(), |m| &mut m.action));
    // `0x13cec8`: the custom name only (`0x64fc30`).
    let version = model.version_info().map(|info| {
        vec![
            VersionMessage {
                action: update(),
                custom_name: Some(info.custom_name.unwrap_or_default()),
                ..Default::default()
            }
            .into(),
        ]
    });
    send(ctx, version);
    // `0x23d688`: DELETE only.
    if cloud_logout {
        let logout = ctx.send(crate::codecs::CloudLoginMessage {
            action: Some(MessageAction::Delete.into()),
            ..Default::default()
        });
        ctx.report(logout);
    }
}

/// `0x15d890`: one entry per kept key whose library capture exists, in key order.
pub(super) fn default_parameters(
    model: &mut dyn DeviceModel,
) -> Result<Vec<DefaultParametersModel>, Fault> {
    let mut entries = Vec::new();
    for key in block::default_parameter_keys(model.default_param_ids()?) {
        if let Some(capture) = model.capture_default(&key)? {
            entries.push(block::default_parameters_entry(&capture)?);
        }
    }
    Ok(entries)
}

/// Periodic senders run only while connected. IOMeter sends only a changed
/// block; the others send every period.
/// The held Grid echoes that are due, each with the request ID of its edit (`0x262b1c`).
pub(crate) fn flush_grid_echoes(ctx: &mut Context<'_, '_>) {
    let now = ctx.now;
    let (due, held): (Vec<_>, Vec<_>) = std::mem::take(&mut ctx.state.grid_echoes)
        .into_iter()
        .partition(|held| held.due <= now);
    ctx.state.grid_echoes = held;
    for held in due {
        let result = ctx.send_as(held.echo.message, held.request_id);
        ctx.report(result);
    }
}

/// Owned: a periodic sender whose model answers `Unsupported` stops after that one notice,
/// so an unsupported subscription does not report every period. A later CREATE starts it
/// again.
fn periodic(ctx: &mut Context<'_, '_>, timer: fn(&mut Timers) -> &mut Periodic, result: Handled) {
    if let Err(Fault::Device(DeviceError::Unsupported(_))) = &result {
        timer(&mut ctx.state.timers).stop();
    }
    ctx.report(result);
}

pub(crate) fn tick(ctx: &mut Context<'_, '_>, model: &mut dyn DeviceModel) {
    let now = ctx.now;
    flush_grid_echoes(ctx);
    if !ctx.session.connected() {
        return;
    }
    let timers = &mut ctx.state.timers;
    let due = [
        timers.io_meters.due(now),
        timers.cpu_load.due(now),
        timers.diagnostics.due(now),
        timers.grid_meters.due(now),
    ];
    if due[0] {
        let result = model.io_meters().map_err(Fault::from).and_then(|meters| {
            if ctx.state.io_meter_cache.as_ref() == Some(&meters) {
                return Ok(());
            }
            ctx.state.io_meter_cache = Some(meters.clone());
            ctx.send(meters)
        });
        periodic(ctx, |timers| &mut timers.io_meters, result);
    }
    if due[1] {
        let result = model
            .cpu_load()
            .map_err(Fault::from)
            .and_then(|m| ctx.send(m));
        periodic(ctx, |timers| &mut timers.cpu_load, result);
    }
    if due[2] {
        let result = model
            .diagnostics()
            .map_err(Fault::from)
            .and_then(|m| ctx.send(m));
        periodic(ctx, |timers| &mut timers.diagnostics, result);
    }
    if ctx.state.timers.local_backup.due(now)
        && let Some(chunk) = ctx.state.backup_out.pop_front()
    {
        let last = ctx.state.backup_out.is_empty();
        ctx.state.timers.local_backup.enabled = !last;
        let sent = ctx.outbox.sent_count();
        let result = ctx.send(crate::codecs::LocalBackupMessage {
            action: Some(MessageAction::Update.into()),
            backup_json: Some(chunk),
            is_last_chunk: Some(last),
            ..Default::default()
        });
        if last && ctx.outbox.sent_count() > sent {
            ctx.step.notices.push(Notice::LocalBackupSent);
        }
        ctx.report(result);
    }
    if due[3] {
        let targets: Vec<_> = ctx.state.grid_meters.iter().copied().collect();
        let result = model
            .grid_model_meter_updates(&targets)
            .map_err(Fault::from)
            .and_then(|messages| {
                messages.into_iter().try_for_each(|mut message| {
                    message.update_type = Some(GridUpdateType::MeterUpdate.into());
                    message.action = Some(MessageAction::Update.into());
                    ctx.send(message)
                })
            });
        periodic(ctx, |timers| &mut timers.grid_meters, result);
    }
}

/// Connection edges from this step's session events. Teardown (`0x6e1d74`):
/// production automation off, transmit flush, Connection(false) on the
/// priority path, pending dialogs cancelled, receiver state and timers
/// reset, the app's own teardown, and Control's ShowTuner cleared.
pub(crate) fn connection_edges(ctx: &mut Context<'_, '_>, model: &mut dyn DeviceModel) {
    let edges: Vec<bool> = ctx.step.session[ctx.edges..]
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Connected => Some(true),
            SessionEvent::Disconnected => Some(false),
            _ => None,
        })
        .collect();
    ctx.edges = ctx.step.session.len();
    for connected in edges {
        if connected {
            let result = model.connection_changed(true).map_err(Fault::from);
            ctx.report(result);
            continue;
        }
        if std::mem::take(&mut ctx.state.production_automation) {
            let result = model.set_production_automation(false).map_err(Fault::from);
            ctx.report(result);
        }
        ctx.step.session.push(SessionEvent::FlushTransmit);
        let result = ctx
            .outbox
            .send_direct(ControlMessage::from(ConnectionMessage {
                connected: Some(false),
                ..Default::default()
            }))
            .map_err(Fault::from);
        ctx.report(result);
        for id in std::mem::take(&mut ctx.state.confirmations) {
            let result = model
                .confirmation_answered(id, ConfirmationAction::Cancel)
                .map_err(Fault::from);
            ctx.report(result);
        }
        let result = model.connection_changed(false).map_err(Fault::from);
        ctx.report(result);
        if std::mem::take(&mut ctx.state.tuner_shown) {
            let result = model.show_tuner(false).map_err(Fault::from);
            ctx.report(result);
        }
        let state = &mut ctx.state;
        state.grid_meters.clear();
        state.tuner_meter = false;
        state.io_meter_cache = None;
        state.forwards.clear();
        state.pending_version = None;
        state.backup_in.clear();
        state.backup_out.clear();
        state.grid_echoes.clear();
        for timer in [
            &mut state.timers.io_meters,
            &mut state.timers.cpu_load,
            &mut state.timers.diagnostics,
            &mut state.timers.grid_meters,
            &mut state.timers.local_backup,
        ] {
            timer.stop();
        }
    }
    ctx.edges = ctx.step.session.len();
}

pub(crate) fn dispatch(ctx: &mut Context<'_, '_>, model: &mut dyn DeviceModel, inbound: Inbound) {
    let message_type = inbound.message_type;
    let Ok(known) = CortexMessageType::try_from(message_type) else {
        return ctx.step.notices.push(Notice::NoReceiver(message_type));
    };
    match known {
        CortexMessageType::Undefined | CortexMessageType::NumberOfMessageTypes => {
            ctx.step.notices.push(Notice::NoReceiver(message_type));
        }
        CortexMessageType::Scene => receivers::scene(ctx, model, &inbound.payload),
        _ => {
            let envelope = crate::Envelope {
                message_type,
                transform: 0,
                decompress: 0,
                opaque: [0; 2],
                payload: inbound.payload,
            };
            match ControlMessage::decode(&envelope, ctx.limits) {
                Ok(decoded) => {
                    let message = decoded.message;
                    if let Some(received) = &mut ctx.step.received {
                        received.action = message.action();
                    }
                    ctx.outbox.current_request_id = message.request_id().unwrap_or(0);
                    ctx.step.request = Some(ctx.outbox.current_request_id);
                    let result = receivers::receive(ctx, model, message);
                    ctx.outbox.current_request_id = 0;
                    ctx.report(result);
                }
                Err(error) => ctx.step.notices.push(Notice::Parse {
                    message_type,
                    error,
                }),
            }
        }
    }
}

/// The Version reply body for a Normal or production-automation request.
pub(crate) fn version_reply(info: VersionMessage, request: &VersionMessage) -> VersionMessage {
    VersionMessage {
        action: Some(MessageAction::Update.into()),
        request_id: request.request_id,
        ..mask_version(info, request)
    }
}

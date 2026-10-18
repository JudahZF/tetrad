//! One receiver per message type. Each comment names the stock receiver.

use crate::codecs::{
    ControlMessage, CortexMessageType as T, DefaultParametersMessage, EnableCaptureOutMessage,
    EnumValue, ForwardMessage, GlobalEQMessage, GridMessage, GridModelMeterMessage,
    MasterVolumeMessage, MessageAction, ModelPresetID, ModelRepoMessage, NewModelsMessage,
    PinnedModelsMessage, PresetDirtyMessage, ProtoEnum, RecentSearchesMessage,
    ResetCommsBuffersMessage, SceneColorMessage, SceneCopyMessage, SceneLabelMessage, SceneMessage,
    ShowGigViewMessage, UndoRedoMessage, VersionMessage,
};
use crate::device::{
    DeviceModel, GridMoved, IoCommand, MeterTarget, ModeCommand, Reply, TunerCommand, UndoRedoStep,
    VersionExchange, confirmation_response, control_version_is_older, mask_version,
};
use crate::scene;
use crate::session::SessionEvent;
use crate::xxtea;

use super::{Context, Fault, Handled, Notice, version_reply};

/// Known action, or `None` for an unknown number. Absent decodes as CREATE
/// (0), the generated default.
fn action(action: Option<EnumValue<MessageAction>>) -> Option<MessageAction> {
    match action.unwrap_or(EnumValue::Known(MessageAction::Create)) {
        EnumValue::Known(action) => Some(action),
        EnumValue::Unknown(_) => None,
    }
}

/// Sets action UPDATE on a model-built state reply.
pub(super) fn updated<M>(
    mut message: M,
    slot: impl FnOnce(&mut M) -> &mut Option<EnumValue<MessageAction>>,
) -> M {
    *slot(&mut message) = Some(MessageAction::Update.into());
    message
}

fn send_all<M: Into<ControlMessage>>(ctx: &mut Context<'_, '_>, replies: Vec<M>) -> Handled {
    replies.into_iter().try_for_each(|reply| ctx.send(reply))
}

/// Scene indices are 0–7 for SceneLabel and SceneColor (`index < 8`).
fn scene_index(index: Option<i32>) -> Option<i32> {
    index.or(Some(0)).filter(|index| (0..8).contains(index))
}

const UPDATE: Option<MessageAction> = Some(MessageAction::Update);
const READ: Option<MessageAction> = Some(MessageAction::Read);
const CREATE: Option<MessageAction> = Some(MessageAction::Create);
const DELETE: Option<MessageAction> = Some(MessageAction::Delete);

pub(super) fn receive(
    ctx: &mut Context<'_, '_>,
    model: &mut dyn DeviceModel,
    message: ControlMessage,
) -> Handled {
    use ControlMessage as M;
    match message {
        // `0x257408` switches on `action`: UPDATE with a preset applies it,
        // DELETE with a preset removes it, others are ignored. No reply. The
        // production-automation receiver `0x2585ec` echoes a preset hash
        // below 4 for UPDATE or READ.
        M::Grid(message) => {
            let action = action(message.action);
            if ctx.state.production_automation {
                let hash = message.model_preset_to_load.as_ref().and_then(|id| id.hash);
                return match (action, hash) {
                    // The PA sender stays enabled; the transport still needs
                    // a connection.
                    (_, Some(_)) if !ctx.session.connected() => {
                        ctx.ignored(T::Grid, message.action)
                    }
                    (Some(MessageAction::Update | MessageAction::Read), Some(hash)) if hash < 4 => {
                        ctx.send_pa(GridMessage {
                            action: Some(MessageAction::Update.into()),
                            update_type: message.update_type,
                            model_preset_to_load: Some(ModelPresetID {
                                hash: Some(hash),
                                ..Default::default()
                            }),
                            ..Default::default()
                        })
                    }
                    _ => ctx.ignored(T::Grid, message.action),
                };
            }
            match (action, &message.preset) {
                (UPDATE, Some(_)) => {
                    let echo = model.update_grid(&message)?;
                    super::grid_echo(ctx, echo)
                }
                (DELETE, Some(_)) => Ok(model.delete_grid_preset(&message)?),
                _ => ctx.ignored(T::Grid, message.action),
            }
        }
        // `0x27f64c`: UPDATE sets, READ replies.
        M::SetlistPosition(message) => match action(message.action) {
            UPDATE => Ok(model.set_setlist_position(&message)?),
            READ => ctx.send(updated(model.setlist_position()?, |m| &mut m.action)),
            _ => ctx.ignored(T::SetlistPosition, message.action),
        },
        // `0x26acfc`: UPDATE applies one change, READ replies (`0x269ab4`).
        M::IOSettings(message) => match action(message.action) {
            UPDATE => match IoCommand::from_update(&message) {
                Some(command) => Ok(model.update_io_settings(command)?),
                None => Ok(()),
            },
            READ => ctx.send(updated(model.io_settings(&message)?, |m| &mut m.action)),
            _ => ctx.ignored(T::IOSettings, message.action),
        },
        // `0x250090` switches on `action`; the READ pushes are compressible (flags 2).
        M::File(message) => send_all(ctx, model.file(&message)?),
        // `0x111cb0`: CREATE starts `sendIOMeters`, DELETE stops it.
        M::IOMeter(message) => match action(message.action) {
            CREATE => {
                ctx.state.timers.io_meters.start(ctx.now);
                ctx.state.io_meter_cache = None;
                Ok(())
            }
            DELETE => {
                ctx.state.timers.io_meters.stop();
                Ok(())
            }
            _ => ctx.ignored(T::IOMeter, message.action),
        },
        // `0x284e30`: one UPDATE branch in priority order; READ replies.
        M::Tuner(message) => match action(message.action) {
            UPDATE => {
                let command = if let Some(port) = message.input_port_id {
                    TunerCommand::InputPort(port)
                } else if let Some(offset) = message.frequency {
                    TunerCommand::ReferenceOffset(offset)
                } else if let Some(mute) = message.mute {
                    TunerCommand::Mute(mute)
                } else if let Some(enable) = message.enable_meter {
                    TunerCommand::EnableMeter(enable)
                } else {
                    return Ok(());
                };
                model.update_tuner(command)?;
                if let TunerCommand::EnableMeter(enable) = command {
                    ctx.state.tuner_meter = enable;
                }
                Ok(())
            }
            READ => ctx.send(updated(model.tuner()?, |m| &mut m.action)),
            _ => ctx.ignored(T::Tuner, message.action),
        },
        // `0x23e530`: CREATE starts `sendDiagnostics`, DELETE stops it.
        M::Diagnostics(message) => match action(message.action) {
            CREATE => {
                ctx.state.timers.diagnostics.start(ctx.now);
                Ok(())
            }
            DELETE => {
                ctx.state.timers.diagnostics.stop();
                Ok(())
            }
            _ => ctx.ignored(T::Diagnostics, message.action),
        },
        // `0x1912d4`: UPDATE only (`0x1912f0` returns for any other action; no READ).
        M::MIDISettings(message) => match action(message.action) {
            UPDATE => Ok(model.update_midi_settings(&message)?),
            _ => ctx.ignored(T::MIDISettings, message.action),
        },
        // `0x163274`: UPDATE applies each present field; READ sends all.
        M::GeneralSettings(message) => match action(message.action) {
            UPDATE => Ok(model.update_general_settings(&message)?),
            READ => ctx.send(updated(model.general_settings()?, |m| &mut m.action)),
            _ => ctx.ignored(T::GeneralSettings, message.action),
        },
        M::Version(message) => version(ctx, model, message),
        // `0x273f3c`: not gated by connection; no reply.
        M::ProductionAutomationMode(message) => {
            let enable = message.enable.unwrap_or(false);
            model.set_production_automation(enable)?;
            ctx.state.production_automation = enable;
            Ok(())
        }
        // `0x26411c`: a refused move pushes the current preset (`0x2778d4(.., 0)`).
        M::GridMove(message) => match model.move_grid(&message)? {
            GridMoved::Refused => ctx.send(updated(model.current_preset()?, |m| &mut m.action)),
            GridMoved::Dropped => Ok(()),
        },
        // `0x271aa0`: UPDATE takes the first present form (`ModeCommand`); READ replies.
        M::Mode(message) => match action(message.action) {
            UPDATE => {
                let command = match (message.mode, message.available_modes, message.atma_page) {
                    (Some(mode @ 0..9), ..) => ModeCommand::Mode(mode),
                    (Some(_), ..) => return ctx.ignored(T::Mode, message.action),
                    (None, Some(modes), _) if !modes.modes.is_empty() => {
                        ModeCommand::AvailableModes(modes.modes)
                    }
                    (None, _, Some(page)) => ModeCommand::AtmaPage(page),
                    _ => return ctx.ignored(T::Mode, message.action),
                };
                Ok(model.set_mode(command)?)
            }
            READ => ctx.send(updated(model.mode()?, |m| &mut m.action)),
            _ => ctx.ignored(T::Mode, message.action),
        },
        // `0x274238`: CREATE (the proto3 default, absent on the wire) loads
        // like UPDATE; both send the working preset back. READ only replies.
        M::RecallPreset(message) => match action(message.action) {
            UPDATE | CREATE => match model.recall_preset(&message)? {
                Reply::Now => ctx.send(updated(model.current_preset()?, |m| &mut m.action)),
                Reply::Deferred => Ok(()),
            },
            READ => ctx.send(updated(model.current_preset()?, |m| &mut m.action)),
            _ => ctx.ignored(T::RecallPreset, message.action),
        },
        // `0x240108`: no action field; replies with the applied value.
        M::EnableCaptureOut(message) => {
            let enable = message.enable.unwrap_or(false);
            model.enable_capture_out(enable)?;
            ctx.send(EnableCaptureOutMessage {
                enable: Some(enable),
                ..Default::default()
            })
        }
        // Normal `0x1962c8` applies `volume` only; READ replies. Stock registers a second
        // receiver for the type, `0x1969b4` (the knob calibration), so a READ gets a second
        // reply from it: UPDATE with only `calibrate` (`0x1fc68c`, written as a presence
        // field; S7 capture `[46]`, `[48]`). Tetrad runs no calibration, so it is false.
        M::MasterVolume(message) => match action(message.action) {
            UPDATE => match message.volume {
                Some(volume) => Ok(model.set_master_volume(volume)?),
                None => Ok(()),
            },
            READ => {
                ctx.send(updated(model.master_volume()?, |m| &mut m.action))?;
                ctx.send(MasterVolumeMessage {
                    action: Some(MessageAction::Update.into()),
                    calibrate: Some(false),
                    ..Default::default()
                })
            }
            _ => ctx.ignored(T::MasterVolume, message.action),
        },
        M::CloudLogin(message) => send_all(ctx, model.cloud_login(&message)?),
        // `0x15eb0c`: CREATE stores the block at row/column as defaults and replies UPDATE
        // with row and column only; UPDATE resets the block to its defaults (`0x15d74c`, no
        // reply; the parameter echoes are held as Grid echoes); READ sends the full list as the resync does (`0x15d890`, action 0); DELETE
        // has no arm.
        M::DefaultParameters(message) => match action(message.action) {
            CREATE => {
                let (row, column) = (message.row.unwrap_or(0), message.column.unwrap_or(0));
                model.save_default_parameters(row, column)?;
                ctx.send(DefaultParametersMessage {
                    action: Some(MessageAction::Update.into()),
                    row: Some(row),
                    column: Some(column),
                    ..Default::default()
                })
            }
            UPDATE => {
                let echo = model.apply_default_parameters(&message)?;
                super::grid_echo(ctx, echo)
            }
            READ => {
                let all_defaults = super::default_parameters(model)?;
                ctx.send(DefaultParametersMessage {
                    all_defaults,
                    ..Default::default()
                })
            }
            _ => ctx.ignored(T::DefaultParameters, message.action),
        },
        // `0x1aa02c`: compound branches; the model returns any reply.
        M::RecentsFavorites(message) => match model.recents_favorites(&message)? {
            Some(reply) => ctx.send(updated(reply, |m| &mut m.action)),
            None => Ok(()),
        },
        // `0x1ba218`: UPDATE steps (undo first); READ sends frame stacks.
        M::UndoRedo(message) => match action(message.action) {
            UPDATE => {
                let step = if message.undo.is_some() {
                    UndoRedoStep::Undo
                } else if message.redo.is_some() {
                    UndoRedoStep::Redo
                } else {
                    return Ok(());
                };
                Ok(model.undo_redo_step(step)?)
            }
            // `0x1ba218` READ leaves the action at 0 (CREATE, implicit): stock `[27]` is an
            // empty body when both stacks are empty.
            READ => ctx.send(UndoRedoMessage {
                action: None,
                ..model.undo_redo()?
            }),
            _ => ctx.ignored(T::UndoRedo, message.action),
        },
        // `0x1b048c`: UPDATE only and only when the indices differ.
        M::SceneCopy(message) => {
            let (from, to) = (
                message.from_index.unwrap_or(0),
                message.to_index.unwrap_or(0),
            );
            if action(message.action) != UPDATE || from == to {
                return ctx.ignored(T::SceneCopy, message.action);
            }
            let swap = message.is_swap.unwrap_or(false);
            model.copy_scene(from, to, swap)?;
            ctx.send(SceneCopyMessage {
                action: Some(MessageAction::Update.into()),
                from_index: Some(from),
                to_index: Some(to),
                is_swap: Some(swap),
                ..Default::default()
            })
        }
        // `0x1b4534`: UPDATE and READ both reply with the label.
        M::SceneLabel(message) => {
            let (Some(index), Some(update @ (MessageAction::Update | MessageAction::Read))) =
                (scene_index(message.index), action(message.action))
            else {
                return ctx.ignored(T::SceneLabel, message.action);
            };
            if update == MessageAction::Update {
                model.set_scene_label(index, message.label.as_deref().unwrap_or_default())?;
            }
            let label = model.scene_label(index)?;
            ctx.send(SceneLabelMessage {
                action: Some(MessageAction::Update.into()),
                index: Some(index),
                label: Some(label),
                ..Default::default()
            })
        }
        // `0x1b8244`: UPDATE shows or hides (no reply); READ replies.
        M::ShowGigView(message) => match action(message.action) {
            UPDATE => Ok(model.show_gig_view(message.show.unwrap_or(false))?),
            READ => {
                let show = model.gig_view_shown()?;
                ctx.send(ShowGigViewMessage {
                    action: Some(MessageAction::Update.into()),
                    show: Some(show),
                    ..Default::default()
                })
            }
            _ => ctx.ignored(T::ShowGigView, message.action),
        },
        // `0x1b6058`: READ renders asynchronously; the PNG follows with the request's ID.
        M::Screenshot(message) => match action(message.action) {
            READ => Ok(model.request_screenshot(&message)?),
            _ => ctx.ignored(T::Screenshot, message.action),
        },
        // `0x14a76c`: CREATE starts `sendCPULoad`, DELETE stops it.
        M::CPULoad(message) => match action(message.action) {
            CREATE => {
                ctx.send(model.cpu_load()?)?;
                ctx.state.timers.cpu_load.start(ctx.now);
                Ok(())
            }
            DELETE => {
                ctx.state.timers.cpu_load.stop();
                Ok(())
            }
            _ => ctx.ignored(T::CPULoad, message.action),
        },
        // `0x1ba180` → `0x6e0330`: UPDATE only; absent `show` is false.
        M::ShowTuner(message) => match action(message.action) {
            UPDATE => {
                let show = message.show.unwrap_or(false);
                model.show_tuner(show)?;
                ctx.state.tuner_shown = show;
                Ok(())
            }
            _ => ctx.ignored(T::ShowTuner, message.action),
        },
        // `0x18bad8`: READ copies the cached DSP looper status.
        M::Looper(message) => match action(message.action) {
            READ => ctx.send(updated(model.looper()?, |m| &mut m.action)),
            _ => ctx.ignored(T::Looper, message.action),
        },
        M::ProductForward(ref m) => match forward_last(ctx, T::ProductForward, m, |_| true) {
            Some(last) => Ok(model.forward_response(message, last)?),
            None => Ok(()),
        },
        M::BackupsForward(ref m) => {
            match forward_last(ctx, T::BackupsForward, m, |r| {
                r.is_last_chunk.unwrap_or(false)
            }) {
                Some(last) => Ok(model.forward_response(message, last)?),
                None => Ok(()),
            }
        }
        M::LogsForward(ref m) => match forward_last(ctx, T::LogsForward, m, |_| true) {
            Some(last) => Ok(model.forward_response(message, last)?),
            None => Ok(()),
        },
        // `0x1beba4`: a response without `payload` (an error) is also the last one.
        M::UpdaterForward(ref m) => match forward_last(ctx, T::UpdaterForward, m, |r| {
            r.is_last_chunk.unwrap_or(false) || r.payload.is_none()
        }) {
            Some(last) => Ok(model.forward_response(message, last)?),
            None => Ok(()),
        },
        // `0x17c7f4`: UPDATE only. The timeout applies only on a CorOS
        // version with a '-' suffix; `is_online` defaults to false.
        M::KeepAlive(message) => {
            if action(message.action) != UPDATE {
                return ctx.ignored(T::KeepAlive, message.action);
            }
            if let Some(seconds) = message.timeout
                && ctx.session.honors_wire_timeout()
            {
                ctx.session.set_timeout_seconds(seconds)?;
            }
            ctx.session.renew(&mut ctx.step.session);
            ctx.session
                .set_online(message.is_online.unwrap_or(false), &mut ctx.step.session);
            Ok(())
        }
        // `0x109b60`: only the first Param, which needs an index and a float
        // first value; READ `0x10aa1c`.
        M::GlobalTempo(message) => match action(message.action) {
            UPDATE => match message.first_indexed_float() {
                Some((index, value)) => Ok(model.set_global_tempo_param(index, value)?),
                None => ctx.ignored(T::GlobalTempo, message.action),
            },
            READ => ctx.send(updated(model.global_tempo()?, |m| &mut m.action)),
            _ => ctx.ignored(T::GlobalTempo, message.action),
        },
        // `0x1a5dc0`: READ only; Control cannot set the dirty flag.
        M::PresetDirty(message) => match action(message.action) {
            READ => {
                let dirty = model.preset_dirty()?;
                ctx.send(PresetDirtyMessage {
                    action: Some(MessageAction::Update.into()),
                    is_dirty: Some(dirty),
                    ..Default::default()
                })
            }
            _ => ctx.ignored(T::PresetDirty, message.action),
        },
        // `0x19c434`: READ only.
        M::ModuleStats(message) => match action(message.action) {
            READ => ctx.send(updated(model.module_stats()?, |m| &mut m.action)),
            _ => ctx.ignored(T::ModuleStats, message.action),
        },
        M::NeuralCapture(message) => send_all(ctx, model.neural_capture(&message)?),
        M::GridModelMeter(message) => grid_model_meter(ctx, message),
        M::GlobalEQ(message) => global_eq(ctx, model, message),
        // `0x1a7140`: DELETE clears, UPDATE stores, READ replies.
        M::RecentSearches(message) => match action(message.action) {
            DELETE => Ok(model.clear_recent_searches()?),
            UPDATE => Ok(model.store_recent_searches(&message.searches)?),
            READ => {
                let searches = model.recent_searches()?;
                ctx.send(RecentSearchesMessage {
                    action: Some(MessageAction::Update.into()),
                    searches,
                    ..Default::default()
                })
            }
            _ => ctx.ignored(T::RecentSearches, message.action),
        },
        // `0x17d104`: UPDATE with `can_apply_backup` opens a receive; other UPDATE
        // chunks join until one without `is_last_chunk = false`, then apply.
        M::LocalBackup(message) => match (action(message.action), &message.backup_json) {
            (UPDATE, Some(chunk)) if message.can_apply_backup.is_none() => {
                let total = ctx.state.backup_in.len().saturating_add(chunk.len());
                if total > super::MAX_BACKUP_BYTES {
                    ctx.state.backup_in.clear();
                    return Err(crate::device::DeviceError::Refused(
                        T::LocalBackup,
                        format!("backup exceeds {} bytes", super::MAX_BACKUP_BYTES),
                    )
                    .into());
                }
                ctx.state.backup_in.push_str(chunk);
                if message.is_last_chunk == Some(false) {
                    return Ok(());
                }
                let backup = std::mem::take(&mut ctx.state.backup_in);
                send_all(ctx, model.apply_local_backup(backup)?)
            }
            _ => send_all(ctx, model.local_backup(&message)?),
        },
        M::CloudBackup(message) => send_all(ctx, model.cloud_backup(&message)?),
        // `0x15ac7c`: READ only.
        M::CompilerInhibitedModules(message) => match action(message.action) {
            READ => ctx.send(updated(model.compiler_inhibited_modules()?, |m| {
                &mut m.action
            })),
            _ => ctx.ignored(T::CompilerInhibitedModules, message.action),
        },
        // `0x1ba1b0`: UPDATE only; no reply.
        M::SystemTimeSync(message) => match action(message.action) {
            UPDATE => Ok(model.set_system_time(message.ms_since_epoch.unwrap_or(0))?),
            _ => ctx.ignored(T::SystemTimeSync, message.action),
        },
        // `0x114cd4`: CREATE only.
        M::Logs(message) => match action(message.action) {
            CREATE => Ok(model.upload_logs(message.correlation_id.as_deref())?),
            _ => ctx.ignored(T::Logs, message.action),
        },
        // `0x1326a8`: UPDATE only.
        M::ProcessDownloadsQueue(message) => match action(message.action) {
            UPDATE => Ok(model.process_downloads_queue(message.queue_type)?),
            _ => ctx.ignored(T::ProcessDownloadsQueue, message.action),
        },
        M::CloudProduct(message) => send_all(ctx, model.cloud_product(&message)?),
        // `0xf986c`: look up the dialog; Ok runs ok, anything else cancel.
        M::Confirmation(message) => {
            let id = message.confirmation_id.unwrap_or(0);
            if !ctx.state.confirmations.remove(&id) {
                return ctx.ignored(T::Confirmation, None);
            }
            Ok(model.confirmation_answered(id, confirmation_response(message.response))?)
        }
        // `0x13577c`: UPDATE and READ both reply with the color.
        M::SceneColor(message) => {
            let (Some(index), Some(update @ (MessageAction::Update | MessageAction::Read))) =
                (scene_index(message.index), action(message.action))
            else {
                return ctx.ignored(T::SceneColor, message.action);
            };
            if update == MessageAction::Update {
                model.set_scene_color(index, message.color.unwrap_or(0))?;
            }
            let color = model.scene_color(index)?;
            ctx.send(SceneColorMessage {
                action: Some(MessageAction::Update.into()),
                index: Some(index),
                color: Some(color),
                ..Default::default()
            })
        }
        // `0xfe880` → `0x6e291c`; absent `connected` is false. No reply.
        M::Connection(message) => {
            ctx.session
                .set_connection(message.connected.unwrap_or(false), &mut ctx.step.session);
            Ok(())
        }
        // `0x1267b8`: DELETE clears the new flag per model; READ replies.
        M::NewModels(message) => match action(message.action) {
            DELETE => message
                .models
                .iter()
                .try_for_each(|id| model.clear_new_model(*id))
                .map_err(Fault::from),
            READ => {
                let models = model.new_models()?;
                ctx.send(NewModelsMessage {
                    action: Some(MessageAction::Update.into()),
                    models,
                    ..Default::default()
                })
            }
            _ => ctx.ignored(T::NewModels, message.action),
        },
        // `0x11e180`: READ only, on the priority path without the request ID.
        // A sent payload is the only writer of the "version" handshake flag.
        M::ModelRepo(message) => {
            if action(message.action) != READ {
                return ctx.ignored(T::ModelRepo, message.action);
            }
            let payload = model.model_repo()?;
            if payload.is_empty() {
                return Ok(());
            }
            ctx.outbox
                .send_direct(ControlMessage::from(ModelRepoMessage {
                    action: Some(MessageAction::Update.into()),
                    model_repo_payload: Some(payload),
                    ..Default::default()
                }))?;
            ctx.session.set_version(true, &mut ctx.step.session);
            Ok(())
        }
        // `0x1354d8`: a live session is torn down; otherwise transmit is
        // flushed. The same `session_id` is always echoed (`0x29df70`).
        M::ResetCommsBuffers(message) => {
            if ctx.session.connected() {
                // Stock tears down inside `0x6e291c`, before the echo.
                ctx.session.set_connection(false, &mut ctx.step.session);
                super::connection_edges(ctx, model);
            } else {
                ctx.step.session.push(SessionEvent::FlushTransmit);
            }
            ctx.outbox
                .send_direct(ControlMessage::from(ResetCommsBuffersMessage {
                    session_id: message.session_id.filter(|id| !id.is_empty()),
                    ..Default::default()
                }))?;
            Ok(())
        }
        // `0x1380d8`: only an explicit `suspend = false` resumes.
        M::SuspendConnection(message) => match message.suspend {
            Some(false) => Ok(model.resume_suspended()?),
            _ => ctx.ignored(T::SuspendConnection, None),
        },
        // `0x12ba94`: CREATE pins, DELETE unpins, READ replies.
        M::PinnedModels(message) => match action(message.action) {
            Some(pin @ (MessageAction::Create | MessageAction::Delete)) => Ok(model.pin_models(
                pin == MessageAction::Create,
                &message.models,
                &message.captures,
            )?),
            READ => {
                let (models, captures) = model.pinned_models()?;
                ctx.send(PinnedModelsMessage {
                    action: Some(MessageAction::Update.into()),
                    models,
                    captures,
                    ..Default::default()
                })
            }
            _ => ctx.ignored(T::PinnedModels, message.action),
        },
        // `0x109ab4`: UPDATE only; no reply.
        M::GigViewButton(message) => match action(message.action) {
            UPDATE => Ok(model.gig_view_button(
                message.button_index.unwrap_or(0),
                message.pressed.unwrap_or(false),
            )?),
            _ => ctx.ignored(T::GigViewButton, message.action),
        },
        // `0x105d80` is an empty stub.
        M::GenericError(_) => ctx.ignored(T::GenericError, None),
        // `0xed8d0`: DELETE sets the multiselection abort flag when busy, READ replies.
        M::BulkOperation(message) => match action(message.action) {
            DELETE => Ok(model.cancel_bulk_operation()?),
            READ => ctx.send(updated(model.bulk_operation()?, |m| &mut m.action)),
            _ => ctx.ignored(T::BulkOperation, message.action),
        },
        // `0x113094`: DELETE clears the "new" mark of each `new_plugin_ids` entry (low 32
        // bits, `0x113298`), READ replies (sent transformed), UPDATE refreshes.
        M::License(message) => match action(message.action) {
            DELETE => message
                .new_plugin_ids
                .iter()
                .try_for_each(|&id| model.acknowledge_new_plugin(id as u32))
                .map_err(Fault::from),
            READ => ctx.send(updated(model.licenses()?, |m| &mut m.action)),
            UPDATE => Ok(model.refresh_licenses()?),
            _ => ctx.ignored(T::License, message.action),
        },
        // `0x130d34`: UPDATE only.
        M::PresetSpeedTest(message) => match action(message.action) {
            UPDATE => Ok(model.preset_speed_test(&message)?),
            _ => ctx.ignored(T::PresetSpeedTest, message.action),
        },
        // `0x1391b4`: READ sends state, UPDATE runs `command`.
        M::Updater(message) => match action(message.action) {
            READ | UPDATE => match model.updater(&message)? {
                Some(reply) => ctx.send(updated(reply, |m| &mut m.action)),
                None => Ok(()),
            },
            _ => ctx.ignored(T::Updater, message.action),
        },
        M::GainCalibration(message) => send_all(ctx, model.gain_calibration(&message)?),
        M::NeuralCapture2(message) => send_all(ctx, model.neural_capture2(&message)?),
        // `0x136e20`: UPDATE only; replies with the result.
        M::Serialization(message) => match action(message.action) {
            UPDATE => ctx.send(updated(model.serialization(&message)?, |m| &mut m.action)),
            _ => ctx.ignored(T::Serialization, message.action),
        },
        M::TestFarm(message) => send_all(ctx, model.test_farm(&message)?),
        // Normal `0x13285c` is empty; PA `0x132884` acts on UPDATE and echoes.
        M::ProductionTest(message) => {
            if !ctx.state.production_automation || action(message.action) != UPDATE {
                return ctx.ignored(T::ProductionTest, message.action);
            }
            model.production_test(&message)?;
            ctx.send_pa(message)
        }
        M::LoadAutomatedTestPreset(message) => Ok(model.load_automated_test_preset(&message)?),
        M::SetTestPresetInputOutputPorts(message) => Ok(model.set_test_preset_ports(&message)?),
        M::SetTestPresetSplitMixPoints(message) => Ok(model.set_test_preset_split_mix(&message)?),
        // `0x1cf248`: CREATE only.
        M::GenerateTestPreset(message) => match action(message.action) {
            CREATE => Ok(model.generate_test_preset(&message)?),
            _ => ctx.ignored(T::GenerateTestPreset, message.action),
        },
        // `0x118f54`: only the READ list (no row, column, hash or capture version) is
        // compressible (flag 2).
        M::ModelPreset(message) => {
            let list = action(message.action) == READ
                && [
                    message.loaded_row,
                    message.loaded_column,
                    message.loaded_hash,
                    message.loaded_capture_version,
                ]
                .iter()
                .all(Option::is_none);
            let replies = model.model_preset(&message)?;
            if !list {
                return send_all(ctx, replies);
            }
            let options = crate::pipeline::SendOptions {
                compressible: true,
                ..Default::default()
            };
            replies
                .into_iter()
                .try_for_each(|reply| ctx.send_options(reply, options))
        }
        M::RemoteControl(message) => send_all(ctx, model.remote_control(&message)?),
    }
}

/// Forward responses (`0x155a6c`, `0x147dc8`, `0x1893ec`, `0x1beba4`):
/// UPDATE only, matched by forward ID, response half required. Backups and
/// Updater responses stay open until `is_last_chunk`. `Some(last)` for a
/// response to an open forward (a last one closes it); the caller then moves
/// the message to the model. Others are ignored here.
fn forward_last<F: ForwardMessage>(
    ctx: &mut Context<'_, '_>,
    message_type: T,
    forward: &F,
    last: impl FnOnce(&F::Response) -> bool,
) -> Option<bool> {
    let key = (message_type, forward.forward_request_id().unwrap_or(0));
    let open = action(forward.action()) == UPDATE && ctx.state.forwards.contains(&key);
    let Some(response) = forward.response().filter(|_| open) else {
        _ = ctx.ignored(message_type, forward.action());
        return None;
    };
    let last = last(response);
    if last {
        ctx.state.forwards.remove(&key);
    }
    Some(last)
}

/// Version (10) `0x294368`. Production automation runs `0x2a52c8`: an
/// immediate reply. Normal `0x2a59d0`: UPDATE with `custom_name` renames;
/// other UPDATEs check the Control version; READ starts the key exchange.
/// All Version sends use the priority path.
fn version(
    ctx: &mut Context<'_, '_>,
    model: &mut dyn DeviceModel,
    request: VersionMessage,
) -> Handled {
    if ctx.state.production_automation {
        return send_version(ctx, model, &request);
    }
    match action(request.action) {
        UPDATE => {
            if let Some(name) = &request.custom_name {
                return Ok(model.set_device_name(name)?);
            }
            let version = request
                .cortex_control_version
                .as_deref()
                .unwrap_or_default();
            if control_version_is_older(version) {
                ctx.outbox.send_direct(ControlMessage::from(
                    crate::codecs::GenericErrorMessage {
                        request_id: request.request_id,
                        error_code: Some(crate::codecs::GenericErrorCode::VersionIsOlder.into()),
                    },
                ))?;
            } else {
                ctx.outbox
                    .send_direct(ControlMessage::from(VersionMessage {
                        action: Some(MessageAction::Update.into()),
                        request_id: request.request_id,
                        cortex_control_version_valid: Some(true),
                        ..Default::default()
                    }))?;
            }
            Ok(())
        }
        READ => match model.begin_version_exchange(&request)? {
            VersionExchange::Ready => send_version(ctx, model, &request),
            VersionExchange::Pending => {
                ctx.state.pending_version = Some(request);
                Ok(())
            }
        },
        _ => ctx.ignored(T::Version, request.action),
    }
}

pub(super) fn send_version(
    ctx: &mut Context<'_, '_>,
    model: &mut dyn DeviceModel,
    request: &VersionMessage,
) -> Handled {
    let reply = version_reply(model.version_info()?, request);
    ctx.outbox.send_direct(ControlMessage::from(reply))?;
    Ok(())
}

/// Key-exchange reply (`0x2a532c`, `0x13dc54`): `comms_version` carries the
/// XXTEA-encrypted key material, the reply has no request ID, and a Version
/// READ follows.
pub(super) fn complete_version(
    ctx: &mut Context<'_, '_>,
    model: &mut dyn DeviceModel,
    request: &VersionMessage,
    xxtea_key: &[u8; 16],
    key_material: &[u8],
) -> Handled {
    let reply = VersionMessage {
        action: Some(MessageAction::Update.into()),
        request_id: None,
        comms_version: Some(xxtea::encrypt(xxtea_key, key_material)),
        ..mask_version(model.version_info()?, request)
    };
    ctx.outbox.send_direct(ControlMessage::from(reply))?;
    ctx.outbox
        .send_direct(ControlMessage::from(VersionMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        }))?;
    Ok(())
}

/// GlobalEQ (38) `0x16d6c4`: UPDATE applies (a preset load takes priority;
/// parameter index below 28); any other action sends state.
fn global_eq(
    ctx: &mut Context<'_, '_>,
    model: &mut dyn DeviceModel,
    message: GlobalEQMessage,
) -> Handled {
    if action(message.action) != UPDATE {
        return ctx.send(updated(model.global_eq()?, |m| &mut m.action));
    }
    if let Some(preset) = &message.model_preset_to_load {
        return Ok(model.load_global_eq_preset(preset)?);
    }
    for parameter in &message.parameters {
        if parameter.parameter_index.unwrap_or(0) < GLOBAL_EQ_PARAMETERS {
            model.set_global_eq_parameter(parameter)?;
        }
    }
    if let Some(bypassed) = message.bypassed {
        model.set_global_eq_bypass(bypassed)?;
    }
    Ok(())
}

/// Stock bound `< 0x1c` in `0x16d6c4`.
const GLOBAL_EQ_PARAMETERS: i32 = 28;

/// GridModelMeter (37) `0x110a70`: CREATE subscribes and starts the 100 ms timer when it is
/// not running, even for a target out of range; DELETE unsubscribes, and only DELETE All
/// (row and column -1) stops the timer. Absent row or column reads as 0, as the generated
/// getter does.
fn grid_model_meter(ctx: &mut Context<'_, '_>, message: GridModelMeterMessage) -> Handled {
    let row = message.row.unwrap_or(0);
    let column = message.column.unwrap_or(0);
    let target = match (row, column) {
        (-1, -1) => Some(MeterTarget::All),
        (0..=3, -1) => Some(MeterTarget::Row(row as u8)),
        (0..=3, 0..=7) => Some(MeterTarget::Model {
            row: row as u8,
            column: column as u8,
        }),
        _ => None,
    };
    let state = &mut *ctx.state;
    match (action(message.action), target) {
        (CREATE, target) => {
            state.grid_meters.extend(target);
            if !state.timers.grid_meters.enabled {
                state.timers.grid_meters.start(ctx.now);
            }
        }
        (DELETE, Some(MeterTarget::All)) => {
            state.grid_meters.clear();
            state.timers.grid_meters.stop();
        }
        (DELETE, Some(target)) => {
            state.grid_meters.remove(&target);
        }
        (DELETE, None) => {}
        _ => return ctx.ignored(T::GridModelMeter, message.action),
    }
    Ok(())
}

/// Scene (13) `0x2787d8`: UPDATE selects (absent means 0, above 7 is
/// refused) with non-forced `setScene` `0x5899d8` and no reply of its own (a
/// changed scene comes back as the device's `Notification::Scene` push, in this
/// request's context); READ reads `0x589a30` and replies.
pub(super) fn scene(ctx: &mut Context<'_, '_>, model: &mut dyn DeviceModel, payload: &[u8]) {
    let limits = scene::SceneLimits {
        max_payload_bytes: ctx.limits.max_payload_bytes,
        max_fields: ctx.limits.max_fields,
        max_group_depth: ctx.limits.max_depth,
    };
    let request = match scene::SceneRequest::decode(payload, limits) {
        Ok(request) => request,
        Err(error) => {
            return ctx.step.notices.push(Notice::Parse {
                message_type: T::Scene as u32,
                error: error.into(),
            });
        }
    };
    ctx.outbox.current_request_id = request.request_id.unwrap_or(0);
    ctx.step.request = Some(ctx.outbox.current_request_id);
    let result = match request.action.unwrap_or(0) {
        1 => match request.selected_scene.unwrap_or(0) {
            scene @ 0..=7 => model.select_scene(scene).map_err(Fault::from),
            _ => ctx.ignored(T::Scene, Some(MessageAction::Update.into())),
        },
        3 => model.scene().map_err(Fault::from).and_then(|scene| {
            ctx.send_scene(SceneMessage {
                action: Some(MessageAction::Update.into()),
                selected_scene: Some(scene),
                ..Default::default()
            })
        }),
        other => {
            let action =
                i32::try_from(other).map_or(EnumValue::Unknown(-1), MessageAction::from_number);
            ctx.ignored(T::Scene, Some(action))
        }
    };
    ctx.outbox.current_request_id = 0;
    ctx.report(result);
}

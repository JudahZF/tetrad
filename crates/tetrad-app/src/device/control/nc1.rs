//! NeuralCapture (36) between Cortex Control and the V1 capture session. Senders `0x1a1e94`
//! (state), `0x1a2ba0` (progress), `0x1a2518` (error id), `0x1a13c4` (MicSetup input level
//! as parameter 1 or 6), `0x1a285c` (A/B), `0x3d3f18` (save info); receiver `0x19ee00`.

use tetrad_capture::orchestration::Command;
use tetrad_capture::session::{RemoteUpdate, SaveInfo};
use tetrad_control::codecs::{
    ControlMessage, CortexMessageType, MessageAction, NeuralCaptureMessage, NeuralCaptureParameter,
    NeuralCaptureSaveInfo,
};
use tetrad_control::device::{DeviceError as ModelError, DeviceResult};

use super::super::capture::NcRemote;
use super::super::{Effect, Subsystem};

/// A V1 session push as the stock senders build it: action UPDATE and one field. Sent only
/// while Control is connected (`0x6e0444`, the `Control::notify` gate).
pub fn nc1_message(remote: RemoteUpdate) -> ControlMessage {
    let mut message = NeuralCaptureMessage {
        action: Some(MessageAction::Update.into()),
        ..Default::default()
    };
    match remote {
        RemoteUpdate::State(state) => message.state = Some(state),
        RemoteUpdate::Progress(progress) => message.progress = Some(progress),
        RemoteUpdate::ErrorId(id) => message.error_id = Some(id),
        RemoteUpdate::Parameter { index, value } => {
            message.parameters = vec![NeuralCaptureParameter {
                index: Some(index),
                value: Some(value),
            }];
        }
        RemoteUpdate::ToggleAbModel(model) => message.toggle_ab_model = Some(model),
        RemoteUpdate::SaveInfo(info) => {
            message.save_info = Some(NeuralCaptureSaveInfo {
                name: Some(info.name),
                device: Some(info.device),
                instrument: Some(info.instrument),
            });
        }
    }
    message.into()
}

/// Inbound `0x19ee00`: UPDATE only, no reply. The first present field in stock order acts:
/// `state` → `0x3cf6a8` (remote state); `toggle_ab_model` → `0x3cf4dc`; `save_info` →
/// `0x3d405c` (the save of the device dialog); `show_dialog_fail_reason` → the device message
/// "Failed to open NC dialog" (a notice here, O). Not built: `parameters` (`0x3cf25c`),
/// `show_dialog` (`0x3cf4fc` and the page change), `try_to_show_dialog` (`0x19df08` and its
/// "navigate to the grid page" reply) and the A/B model fields (`0x3b9fbc`, `0x521170`).
/// Stock also closes the open on-screen keyboard and, for a state other than 6, the dialog's
/// open child item; the Tetrad UI owns those views.
pub(super) fn neural_capture(request: &NeuralCaptureMessage) -> DeviceResult<Option<Effect>> {
    if request.action != Some(MessageAction::Update.into()) {
        return Ok(None);
    }
    let unsupported = || Err(ModelError::Unsupported(CortexMessageType::NeuralCapture));
    if let Some(state) = request.state {
        let state = u8::try_from(state).map_err(|_| {
            ModelError::Refused(CortexMessageType::NeuralCapture, format!("state {state}"))
        })?;
        return Ok(Some(Effect::CaptureRemote(NcRemote::Command(
            Command::RemoteState(state),
        ))));
    }
    if !request.parameters.is_empty() || request.show_dialog.is_some() {
        return unsupported();
    }
    if let Some(model) = request.toggle_ab_model {
        return Ok(Some(Effect::CaptureRemote(NcRemote::Command(
            Command::ToggleAb(model),
        ))));
    }
    if let Some(info) = &request.save_info {
        return Ok(Some(Effect::CaptureRemote(NcRemote::Save(SaveInfo {
            name: info.name.clone().unwrap_or_default(),
            device: info.device.unwrap_or(0),
            instrument: info.instrument.unwrap_or(0),
        }))));
    }
    if let Some(reason) = &request.show_dialog_fail_reason {
        let text = format!("Failed to open NC dialog: {reason}");
        return Ok(Some(Effect::Notice(Subsystem::Capture, text)));
    }
    if request.try_to_show_dialog.is_some()
        || request.model_ab.is_some()
        || request.model_ab_bypass.is_some()
        || request.model_ab_preset.is_some()
    {
        return unsupported();
    }
    Ok(None)
}

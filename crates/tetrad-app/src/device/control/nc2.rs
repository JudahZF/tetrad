//! NeuralCapture2 (63) between Cortex Control and the NC2 controller (inbound `0x1201bc`, senders
//! `0x122f04`..`0x1256dc`).

use tetrad_capture::session::SaveInfo;
use tetrad_capture::v2::{Nc2Input, Nc2Remote, Nc2Request, Nc2Update};
use tetrad_control::codecs::{
    ControlMessage, CortexMessageType, MessageAction, NeuralCapture2Message,
    NeuralCaptureParameter, NeuralCaptureSaveInfo,
};
use tetrad_control::device::{DeviceError as ModelError, DeviceResult};
use tetrad_engine::{Column, Row, Site, Slot};

use super::super::{Effect, State};

/// The NC2 cab block, Grid (0,3) (`0x51334c(.., 0, 3)`).
fn cab_bypassed(state: &State) -> bool {
    let (Some(current), Ok(row), Ok(column)) = (&*state.current, Row::new(0), Column::new(3))
    else {
        return false;
    };
    let scene = current.deployment.scene();
    (current.deployment.layout())
        .instance(Site::Slot(Slot { row, column }))
        .is_some_and(|block| block.bypass.at(scene))
}

/// Inbound `0x1201bc`: CREATE (action 0) saves the auditioned capture (`0x1fef08`); UPDATE
/// takes the first present field ([`Nc2Update::request`]); other actions do nothing. There
/// is no reply. `open_dialog` opens the NC2 dialog (`showDialog` `0x3d791c` → `0x203af0`): the
/// controller starts its session ([`Nc2Input::Open`]). The cab model changes belong to the Grid
/// (`0x200428`) and are refused.
pub(super) fn neural_capture2(
    request: &NeuralCapture2Message,
    state: &State,
) -> DeviceResult<Option<Effect>> {
    let action = request.action.unwrap_or(MessageAction::Create.into());
    if action == MessageAction::Create.into() {
        return Ok(Some(Effect::Nc2(Nc2Input::Save)));
    }
    if action != MessageAction::Update.into() {
        return Ok(None);
    }
    let update = Nc2Update {
        state: request.state,
        open_dialog: request.open_dialog == Some(true),
        job_id: request.job_id.clone(),
        parameters: (request.parameters.iter())
            .map(|parameter| (parameter.index.unwrap_or(0), parameter.value.unwrap_or(0.0)))
            .collect(),
        cabsim: request.cabsim.is_some(),
        cabsim_model_preset: request.cabsim_model_preset.is_some(),
        cabsim_bypass: request.cabsim_bypass,
        save_info: request.save_info.as_ref().map(|info| SaveInfo {
            name: info.name.clone().unwrap_or_default(),
            device: info.device.unwrap_or(0),
            instrument: info.instrument.unwrap_or(0),
        }),
        start_from_scratch: request.start_from_scratch == Some(true),
    };
    let input = match update.request() {
        None => return Ok(None),
        Some(Nc2Request::Input(input)) => input,
        Some(Nc2Request::CabsimBypass(bypass)) => Nc2Input::CabsimBypass {
            bypass,
            current: cab_bypassed(state),
        },
        Some(Nc2Request::OpenDialog) => Nc2Input::Open,
        Some(Nc2Request::Cabsim | Nc2Request::CabsimModelPreset) => {
            return Err(ModelError::Unsupported(CortexMessageType::NeuralCapture2));
        }
    };
    Ok(Some(Effect::Nc2(input)))
}

/// An NC2 controller push as the stock senders build it: action UPDATE and one field
/// (state `0x123244`, error `0x123d18`, parameter echo, save info `0x1256dc`, cab bypass
/// `0x123588`, reset `0x122f04` = `start_from_scratch`).
pub fn nc2_message(remote: Nc2Remote) -> ControlMessage {
    let mut message = NeuralCapture2Message {
        action: Some(MessageAction::Update.into()),
        ..Default::default()
    };
    match remote {
        Nc2Remote::State(state) => message.state = Some(state as i32),
        Nc2Remote::Error { id, text } => {
            message.error_id = Some(id);
            message.error_string = Some(text);
        }
        Nc2Remote::Parameter { index, value } => {
            message.parameters = vec![NeuralCaptureParameter {
                index: Some(index),
                value: Some(value),
            }];
        }
        Nc2Remote::SaveInfo(info) => {
            message.save_info = Some(NeuralCaptureSaveInfo {
                name: Some(info.name),
                device: Some(info.device),
                instrument: Some(info.instrument),
            });
        }
        Nc2Remote::CabsimBypass(bypass) => message.cabsim_bypass = Some(bypass),
        Nc2Remote::Reset => message.start_from_scratch = Some(true),
    }
    message.into()
}

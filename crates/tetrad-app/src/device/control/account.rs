//! License (58) and CloudLogin (18), both transform types: they go out only once the comms
//! transform is installed.

use tetrad_control::codecs::{
    AuthorizationToken, CloudLoginMessage, CortexMessageType, LicenseItem, LicenseMessage,
    LicenseUpdateStatus, MessageAction,
};
use tetrad_control::device::{DeviceError as ModelError, DeviceResult};

use tetrad_services::cloud::session::Tokens;
use tetrad_services::exec::Secret;

use super::super::services::{ControlLogin, Handoff, PluginStatus};
use super::super::{Effect, State};
use super::{AccountRequest, StateModel};

/// `supported_plugin_ids` (`0x114154` reads the 12-entry table `0xa0bb28`; the licence
/// manager's own table `0xa17988` holds the same ids in the same order).
const SUPPORTED_PLUGINS: [u32; 12] = [3, 10, 1, 11, 4, 5, 9, 12, 13, 21, 15, 20];

/// License READ `0x114154` and the state push `0x1147f0`: one item per licensed id with only
/// `plugin_id`, the unacknowledged ids that are licensed, and the supported table. Inferred:
/// `is_enabled` (`+0x38`, bit 62 of the last DSP answer) reads true when a licensed plugin is
/// active, as the device keeps the flag only inside the licence state. `is_deactivated`
/// (`+0x39`, bit 63) and `update_status` (`0x1ed180`: Updating while a visible refresh runs,
/// Failed only in the failure push `0x1144ac`) need services state (r16 MR 2): always false
/// and Idle here.
pub(super) fn licences(state: &State) -> LicenseMessage {
    let licensed: Vec<u32> = (SUPPORTED_PLUGINS.into_iter())
        .filter(|&id| state.licences.is_licensed(id))
        .collect();
    let new = &state.settings.settings().licenses.new;
    LicenseMessage {
        action: Some(MessageAction::Update.into()),
        licenses: licensed
            .iter()
            .map(|&id| LicenseItem {
                plugin_id: Some(u64::from(id)),
                ..Default::default()
            })
            .collect(),
        is_enabled: Some(
            (licensed.iter()).any(|&id| state.licences.status(id) == PluginStatus::Active),
        ),
        new_plugin_ids: (new.iter())
            .filter_map(|&id| u32::try_from(id).ok())
            .filter(|id| licensed.contains(id))
            .map(u64::from)
            .collect(),
        supported_plugin_ids: SUPPORTED_PLUGINS.map(u64::from).to_vec(),
        // Has-bits 4 and 8 are always set. Bit 63 of the refresh result (`0x1f1aa4`) and the
        // licence manager busy flags (`0x1ed180`) are not kept: false and Idle.
        is_deactivated: Some(false),
        update_status: Some(LicenseUpdateStatus::Idle.into()),
        ..Default::default()
    }
}

/// License DELETE `0x1f170c(ctrl, id, 1)`: a licensed id leaves the `new` set, which is saved
/// (`PluginLicenses.ini`, `0x1f123c`). The state push `0x1147f0` follows from the
/// [`Control::sync`](super::Control::sync) diff, inside the request. No HTTP call.
pub(super) fn acknowledge_new_plugin(model: &mut StateModel<'_>, plugin: u32) -> DeviceResult<()> {
    if !model.state.licences.is_licensed(plugin) {
        return Ok(());
    }
    model.settings(|settings| {
        settings
            .licenses
            .new
            .retain(|&id| u32::try_from(id) != Ok(plugin));
    });
    Ok(())
}

/// CloudLogin `0x239828`. DELETE runs the logout `0x3080b4` (unpair, then sign out) with no
/// reply. READ `0x23943c`: logged out (`0x44d32c`) replies DELETE; logged in replies the
/// username and tokens (`0x23d9b8`: action 0, `expires_in` 0). Inferred: the stored tokens are
/// sent as they are (stock first runs the ensure-fresh refresh `0x44e190` and replies DELETE
/// when it fails). CREATE with both tokens hands them to the services bridge
/// (`loginFromControl` `0x307e18`: store the tokens, pair, fetch the profile); a failure
/// answers GenericError CloudLoginFailed later (`0x238ce4`). Without both tokens it does
/// nothing (I).
pub(super) fn cloud_login(
    model: &mut StateModel<'_>,
    request: &CloudLoginMessage,
) -> DeviceResult<Vec<CloudLoginMessage>> {
    let action = request.action.unwrap_or(MessageAction::Create.into());
    if action == MessageAction::Delete.into() {
        model
            .effects
            .push(Effect::ControlAccount(AccountRequest::SignOut));
        return Ok(Vec::new());
    }
    if action == MessageAction::Create.into() {
        let token = request.token.as_ref();
        if let (Some(access), Some(refresh)) = (
            token.and_then(|token| token.access_token.as_deref()),
            token.and_then(|token| token.refresh_token.as_deref()),
        ) {
            let login = ControlLogin {
                username: request.username.clone(),
                tokens: Tokens {
                    access_token: Secret::new(access),
                    refresh_token: Secret::new(refresh),
                },
                request_id: request.request_id,
            };
            let request = AccountRequest::Login(Handoff::new(login));
            model.effects.push(Effect::ControlAccount(request));
        }
        return Ok(Vec::new());
    }
    if action != MessageAction::Read.into() {
        return Err(ModelError::Unsupported(CortexMessageType::CloudLogin));
    }
    let settings = model.state.settings.settings();
    let tokens = &settings.tokens;
    if settings.account.is_logged_out() || tokens.access_token.is_empty() {
        return Ok(vec![CloudLoginMessage {
            action: Some(MessageAction::Delete.into()),
            ..Default::default()
        }]);
    }
    Ok(vec![CloudLoginMessage {
        action: Some(MessageAction::Create.into()),
        username: Some(settings.account.username.clone()),
        token: Some(AuthorizationToken {
            access_token: Some(tokens.access_token.expose().to_owned()),
            refresh_token: Some(tokens.refresh_token.expose().to_owned()),
            expires_in: Some(0),
        }),
        ..Default::default()
    }])
}

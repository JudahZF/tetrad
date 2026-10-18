//! CloudProduct (46) on the Control side (receiver `0xf491c`).
//!
//! By action: DELETE (2) `0xf330c`, READ (3) `0xf5470`, UPLOAD (6) `0xf3060`, DOWNLOAD (7)
//! `0xf3f24`. Each of DELETE, UPLOAD and DOWNLOAD returns without a reply when
//! `product_keys` is empty or `product_type` is absent (has-bit `0x10`). One key runs the
//! single-item path; more keys run the Multi Select worker (`0x785b74`, `0x785dcc`,
//! `0x786264`, `0x786ac4`). Tetrad builds the multi-key DELETE, UPLOAD (the batch, re-r38
//! §2) and DOWNLOAD (r47-control); each is a Services run with its BulkOperation (57) pushes
//! (`Control::bulk_run`).

use std::collections::{BTreeMap, BTreeSet};

use tetrad_control::codecs::{
    CloudProductMessage, CloudTransferItemState, CloudTransferState, CloudTransferStateItem,
    ControlMessage, CortexMessageType, EnumValue, MessageAction,
};
use tetrad_control::device::{DeviceError as ModelError, DeviceResult};
use tetrad_services::cloud::services::ProductKind;
use tetrad_storage::media::SETLIST_SLOTS;

use tetrad_ui::UploadItem;

use super::super::services::UploadState;

use super::super::{Effect, Subsystem};
use super::{AccountRequest, StateModel, files};

const CLOUD: CortexMessageType = CortexMessageType::CloudProduct;

fn refused(what: &str) -> ModelError {
    ModelError::Refused(CLOUD, what.to_owned())
}

/// A multi-key receiver returns with nothing (C, `0x785b74`, `0x785dcc`, `0x786264`,
/// `0x786ac4` with fromControl 1) while Directory Multi Select is on (`0x843c60`, `+0x54`) or
/// a run is active (`0x843c78`, `+0x55`: the run Control last saw, `StateModel::bulk`).
/// `+0x3a8` (a library view flag) is not modelled.
fn runs_block(model: &StateModel<'_>) -> bool {
    model.multi_select || model.bulk.is_some()
}

/// The cloud folder of `product_type` (`0x765780`): 0 presets, 1 IRs, 2 captures; any other
/// type has none.
fn kind(product_type: i32) -> Option<ProductKind> {
    match product_type {
        0 => Some(ProductKind::Preset),
        1 => Some(ProductKind::ImpulseResponse),
        2 => Some(ProductKind::NeuralCapture),
        _ => None,
    }
}

pub(super) fn cloud_product(
    model: &mut StateModel<'_>,
    request: &CloudProductMessage,
) -> DeviceResult<Vec<CloudProductMessage>> {
    type Run = fn(&mut StateModel<'_>, &CloudProductMessage, i32) -> DeviceResult<()>;
    let run: Run = match request.action {
        Some(EnumValue::Known(MessageAction::Read)) => return read(model, request),
        Some(EnumValue::Known(MessageAction::Delete)) => delete,
        Some(EnumValue::Known(MessageAction::Upload)) => upload,
        Some(EnumValue::Known(MessageAction::Download)) => download,
        _ => return Err(ModelError::Unsupported(CLOUD)),
    };
    let (Some(product_type), false) = (request.product_type, request.product_keys.is_empty())
    else {
        return Ok(Vec::new());
    };
    run(model, request, product_type)?;
    Ok(Vec::new())
}

/// READ with `cloud_transfer_state` (`0xf5470`, sent whenever connected; also the resync):
/// DELETE with an empty transfer state, then UPDATE with every product of the transfer state
/// map `0x7db468` ([`transfer_state`]). The device builds the stored products over several
/// passes ([`files::synced`]); the resync builds them at once.
fn read(
    model: &mut StateModel<'_>,
    request: &CloudProductMessage,
) -> DeviceResult<Vec<CloudProductMessage>> {
    if request.cloud_transfer_state.is_none() {
        return Err(ModelError::Unsupported(CLOUD));
    }
    if model.state.library.is_none() {
        let (uploads, in_flight) = (model.upload_states, model.downloads_in_flight);
        return Ok(transfer_state(BTreeSet::new(), uploads, in_flight).into());
    }
    let spread = files::synced(model)?;
    if model.spreads {
        model.spread = Some(spread);
        return Ok(Vec::new());
    }
    (spread.run(model.state, model.cipher)?.into_iter())
        .map(|message| match message {
            ControlMessage::CloudProduct(message) => Ok(message),
            _ => Err(refused("the transfer state is no CloudProduct message")),
        })
        .collect()
}

/// The READ reply: DELETE with an empty transfer state, then UPDATE with the map in key order
/// (a map; S7 capture, C): the stored products with a cloud id as Downloaded (`synced`), the
/// upload states by local key (`Services::upload_states`: NotUploaded, Uploading, Uploaded)
/// and the downloads in flight as Downloading (r30-services §4). One key holds its last
/// state (`0x7db73c` overwrites; O: in this order). I: the UPDATE goes out also when the map is
/// empty.
pub(super) fn transfer_state(
    synced: BTreeSet<String>,
    uploads: &[(String, UploadState)],
    in_flight: &[String],
) -> [CloudProductMessage; 2] {
    let mut states: BTreeMap<String, CloudTransferItemState> = (synced.into_iter())
        .map(|key| (key, CloudTransferItemState::Downloaded))
        .collect();
    for (key, state) in uploads {
        let state = match state {
            UploadState::NotUploaded => CloudTransferItemState::NotUploaded,
            UploadState::Uploading => CloudTransferItemState::Uploading,
            UploadState::Uploaded => CloudTransferItemState::Uploaded,
        };
        states.insert(key.clone(), state);
    }
    for id in in_flight {
        states.insert(id.clone(), CloudTransferItemState::Downloading);
    }
    let items = (states.into_iter())
        .map(|(id, state)| CloudTransferStateItem {
            id: Some(id),
            state: Some(state.into()),
        })
        .collect();
    let message = |action: MessageAction, items| CloudProductMessage {
        action: Some(action.into()),
        cloud_transfer_state: Some(CloudTransferState { items }),
        ..Default::default()
    };
    [
        message(MessageAction::Delete, Vec::new()),
        message(MessageAction::Update, items),
    ]
}

/// DELETE `0xf330c`: one key → `0x1d916c(lib, cloud folder, key)` → the cloud folder remove
/// (`vt+0x4c`; I: `DELETE user/products/<id>`). The key `"folder"` (`DAT_015615dc`) names the
/// folder itself: stock logs "will not remove cloud or readonly folder" (`0x76c170`) and
/// sends nothing (C, review r32 E1). An empty key does nothing (O; stock would remove "", I).
/// More keys: the Multi Select worker `0x785b74(.., fromControl 1)` returns with nothing while
/// [`runs_block`], else runs `0x84c614` → `0x84be40`: each key in order (`vt+0x4c`
/// `0x7180bc`), the next after the reply, good or not, and asks nothing (C, re-r34 §6), with
/// the BulkOperation (57) pushes "Removing %1, please wait." (`0xf1cec(.., 1, 1, cloud
/// folder, 0)`), progress, and DELETE `finished` from the done closure `0x783950`
/// (`Control::bulk_run`).
///
/// The products leave the user's cloud account, so only an explicit Control DELETE gets here
/// and exactly the given keys go to Services in one request
/// ([`AccountRequest::CloudProductDelete`]: one key, or the run), logged. Owned: apart from
/// the one-key cases above, every key must be a plain product id (not empty, not `"folder"`,
/// not `.` or `..`, no `/`, no repeat), else nothing is sent. Bring-up refuses the request
/// without `--services` and `--persist`.
fn delete(
    model: &mut StateModel<'_>,
    request: &CloudProductMessage,
    product_type: i32,
) -> DeviceResult<()> {
    let Some(kind) = kind(product_type) else {
        return Ok(());
    };
    let keys = request.product_keys.as_slice();
    match keys {
        [id] if id.is_empty() => return Ok(()),
        [id] if id == "folder" => {
            let log = "will not remove cloud or readonly folder".to_owned();
            model.effects.push(Effect::Notice(Subsystem::Control, log));
            return Ok(());
        }
        _ => {}
    }
    let plain = |(at, id): (usize, &String)| {
        !(id.is_empty()
            || id == "folder"
            || id == "."
            || id == ".."
            || id.contains('/')
            || keys[..at].contains(id))
    };
    if !keys.iter().enumerate().all(plain) {
        return Err(refused(
            "a cloud delete with a key that is not a product id",
        ));
    }
    if keys.len() > 1 && runs_block(model) {
        return Ok(());
    }
    let delete = AccountRequest::CloudProductDelete {
        kind,
        ids: keys.to_vec(),
    };
    model.queue(CLOUD, Effect::ControlAccount(delete))?;
    let log = format!(
        "Cortex Control deletes cloud {} {}",
        kind.as_str(),
        keys.join(", ")
    );
    model.effects.push(Effect::Notice(Subsystem::Control, log));
    Ok(())
}

/// UPLOAD `0xf3060`: type 1 (IR) does nothing. Type 2 takes the user capture store
/// (`0x765520`); other types the folder of `from_folder` (`0x76bc68`; none: nothing). One key
/// → `0x1e8a70(lib, from, key, cloud folder, −1, …)`, whose cloud-target branch is the
/// Directory upload (`0x1d81d0` presets, `0x1d8278` captures). More keys → the batch
/// `0x785dcc(.., fromControl 1)` (re-r38 §2, C): signed out (`0x716b80`) or a running Multi
/// Select ([`runs_block`]) → nothing, no text; else the single uploads one by one.
/// Tetrad: presets of a user setlist ([`AccountRequest::CloudProductUpload`]) or user
/// captures ([`AccountRequest::CloudCaptureUpload`]); more keys are
/// [`AccountRequest::CloudBatchUpload`]. Other preset folders, presets that are not in the
/// folder and unknown types are refused. O: an unknown capture key ends with the upload
/// result dialog. The BulkOperation (57) start, progress and finished pushes of the batch
/// (`0xf1cec`, `0x77fef0`, `0xf29b8`) are `Control::bulk_upload`.
fn upload(
    model: &mut StateModel<'_>,
    request: &CloudProductMessage,
    product_type: i32,
) -> DeviceResult<()> {
    let keys = request.product_keys.as_slice();
    let items: Vec<_> = match product_type {
        1 => return Ok(()),
        2 => (keys.iter().cloned()).map(UploadItem::Capture).collect(),
        0 => {
            let Some(from) = request.from_folder.as_deref() else {
                return Ok(());
            };
            let Some((_, slots)) = files::setlist_folder(model, from)? else {
                return Err(refused(
                    "a cloud upload from a folder other than a user setlist",
                ));
            };
            if !keys
                .iter()
                .all(|key| slots.iter().flatten().any(|slot| slot == key))
            {
                return Err(refused(
                    "a cloud upload of a preset that is not in the folder",
                ));
            }
            (keys.iter().cloned()).map(UploadItem::Preset).collect()
        }
        _ => return Err(ModelError::Unsupported(CLOUD)),
    };
    let upload = match items.as_slice() {
        [UploadItem::Capture(key)] => AccountRequest::CloudCaptureUpload { key: key.clone() },
        [UploadItem::Preset(path)] => AccountRequest::CloudProductUpload { path: path.clone() },
        _ => {
            let signed_out = model.state.settings.settings().account.is_logged_out();
            if signed_out || runs_block(model) {
                return Ok(());
            }
            AccountRequest::CloudBatchUpload { items }
        }
    };
    model.queue(CLOUD, Effect::ControlAccount(upload))
}

/// DOWNLOAD `0xf3f24`. Types 1 and 2: the folder of `to_folder` or the saving destination of
/// the type (`0x76be70`); one key → `0x1e8a70` (IR `0x799134`, capture `0x1d9954`); more
/// keys → `0x786264` → `0x847620`, the Multi Select download of each key into that folder. Any
/// other type is a preset: it needs `to_folder` (has-bit 2) and `to_indices`, else nothing;
/// one key → `0x1e8a70(lib, cloud presets, key, folder, to_indices[0], …)` → `0x1dd61c`
/// stores into that slot. More keys → `0x786ac4` with one index (`0xf3878` → `0x84eac4`: the
/// consecutive slots from it up to the setlist end, `DAT_01205790` = 256 = [`SETLIST_SLOTS`];
/// keys past it are dropped) or with one
/// index per key (`0xf3cc0` → `0x84e68c`); other index counts only log. Both workers do
/// nothing for a Control request while [`runs_block`].
///
/// Tetrad: capture or IR keys with no `to_folder` go into the saving destination
/// ([`AccountRequest::CloudProductDownload`]; more keys one batch,
/// [`AccountRequest::CloudProductDownloads`]); presets go
/// into user setlist slots ([`AccountRequest::CloudPresetDownload`], every slot checked by
/// `Device::download_presets` before the first request). Refused (O): a capture or IR into a
/// folder, a preset into another folder or a negative slot, and a preset index list that
/// fits neither form. A multi-key download is a Services run with its BulkOperation (57)
/// pushes (`0x786264`, `0x786ac4` → `0xf1cec`; `Control::bulk_run`).
fn download(
    model: &mut StateModel<'_>,
    request: &CloudProductMessage,
    product_type: i32,
) -> DeviceResult<()> {
    let keys = request.product_keys.as_slice();
    let to_folder = request.to_folder.as_deref();
    let many = keys.len() > 1;
    if let Some(kind @ (ProductKind::ImpulseResponse | ProductKind::NeuralCapture)) =
        kind(product_type)
    {
        if to_folder.is_some_and(|folder| !folder.is_empty()) {
            return Err(refused("a cloud download into a folder"));
        }
        let download = match keys {
            [id] => AccountRequest::CloudProductDownload {
                kind,
                id: id.clone(),
            },
            _ if runs_block(model) => return Ok(()),
            ids => AccountRequest::CloudProductDownloads {
                kind,
                ids: ids.to_vec(),
            },
        };
        return model.queue(CLOUD, Effect::ControlAccount(download));
    }
    let (Some(to_folder), [first, ..]) = (to_folder, request.to_indices.as_slice()) else {
        return Err(refused(
            "a preset download without to_folder and to_indices",
        ));
    };
    let Some((setlist, _)) = files::setlist_folder(model, to_folder)? else {
        return Err(refused(
            "a preset download into a folder other than a user setlist",
        ));
    };
    let slot = |index: &i32| {
        usize::try_from(*index).map_err(|_| refused("a preset download into a negative slot"))
    };
    let slots: Vec<usize> = match request.to_indices.as_slice() {
        _ if !many => vec![slot(first)?],
        [start] => (slot(start)?..SETLIST_SLOTS).take(keys.len()).collect(),
        indices if indices.len() == keys.len() => {
            indices.iter().map(slot).collect::<Result<_, _>>()?
        }
        _ => {
            return Err(refused(
                "a preset download with neither one index nor one index per key",
            ));
        }
    };
    if slots.is_empty() {
        return Err(refused("a preset download past the last slot"));
    }
    if many && runs_block(model) {
        return Ok(());
    }
    if slots.len() < keys.len() {
        let log = format!(
            "{} cloud presets past slot {SETLIST_SLOTS} are not downloaded",
            keys.len() - slots.len()
        );
        model.effects.push(Effect::Notice(Subsystem::Control, log));
    }
    let items = keys.iter().cloned().zip(slots).collect();
    let download = AccountRequest::CloudPresetDownload { setlist, items };
    model.queue(CLOUD, Effect::ControlAccount(download))
}

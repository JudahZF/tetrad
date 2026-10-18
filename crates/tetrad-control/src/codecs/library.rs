//! Library, cloud and capture envelopes: files, bulk operations, setlists,
//! recents/favorites, cloud login/products/backups, local backups, logs,
//! downloads queue, module stats, neural capture, default parameters and
//! model presets. Payload observations only: no storage, cloud, HTTP, login
//! or capture action is taken here.

use std::fmt;

use super::proto::{Decoder, Encoder, Message};
use super::{
    CloudBackupErrorCode, CloudTransferItemState, CodecLimits, CodecResult, Decoded, EnumValue,
    MessageAction, Model, ModelPresetID, RecentFavoriteType, WireField,
};

proto_message!(
    /// `hash` is implicit presence; the rest are `proto3_optional`.
    ProductDependency {
    1 => hash: scalar String,
    2 => missing: scalar i32,
    3 => visible: scalar i32,
    4 => published: scalar i32,
    5 => cloud_id: scalar String,
    6 => name: scalar String,
    7 => product_type: scalar String,
});

proto_message!(
    /// Library product metadata. Descriptor `extraMetadata` is
    /// `extra_metadata`; field 21 is a signed int64.
    ProductData {
    1 => key: scalar String,
    2 => index: scalar i32,
    3 => name: scalar String,
    4 => author: scalar String,
    5 => author_id: scalar String,
    6 => coros_version: scalar String,
    7 => tags: repeated_scalar String,
    8 => date: scalar String,
    9 => gain: scalar i32,
    10 => cloud_id: scalar String,
    11 => description: scalar String,
    12 => shared_by: scalar String,
    13 => factory_dependencies: repeated ProductDependency,
    14 => product_dependencies: repeated ProductDependency,
    15 => instrument: scalar i32,
    16 => device: scalar i32,
    17 => extra_metadata: scalar Vec<u8>,
    18 => old_key: scalar String,
    19 => is_readonly: scalar bool,
    20 => version: scalar String,
    21 => date_ms_since_epoch: scalar i64,
});

proto_message!(
    /// The descriptor declares `files` (7) after field 10; output is numeric.
    FolderInfo {
    1 => key: scalar String,
    2 => parent_key: scalar String,
    3 => name: scalar String,
    4 => is_factory: scalar bool,
    5 => is_downloads: scalar bool,
    6 => is_user_default: scalar bool,
    7 => files: repeated ProductData,
    8 => is_cloud: scalar bool,
    9 => is_favorites: scalar bool,
    10 => is_plugin: scalar bool,
});

proto_message!(
    /// Envelope type 4. Descriptor `type` is `file_type`, the product type: 0 presets,
    /// 1 IRs, 2 captures (`0x250090`). `preset_payload` is raw `BinaryPreset` bytes.
    FileMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => file_type: scalar i32,
    4 => folder: message FolderInfo,
    5 => to_folder: message FolderInfo,
    6 => short_listing: scalar bool,
    7 => ir_payload: scalar Vec<u8>,
    8 => preset_payload: raw_message,
    9 => total_bulk_create_count: scalar i32,
    10 => delete_from_library: scalar bool,
    11 => omit_factory_content: scalar bool,
    12 => user_content_estimate: scalar i32,
});

proto_message!(
    /// Envelope type 57. Descriptor `type` is `operation_type`.
    BulkOperationMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => progress_message: scalar String,
    4 => blocking: scalar bool,
    5 => cancellable: scalar bool,
    6 => progress: scalar f32,
    7 => finished: scalar bool,
    8 => is_multiselection_active: scalar bool,
    9 => operation_type: scalar i32,
    10 => source_folder: message FolderInfo,
    11 => destination_folder: message FolderInfo,
});

proto_message!(
    /// Envelope type 2.
    SetlistPositionMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => folder_key: scalar String,
    4 => position: scalar u32,
    5 => is_factory: scalar bool,
    6 => is_downloads: scalar bool,
    7 => key_in_downloads: scalar String,
    8 => is_plugin: scalar bool,
    9 => key_in_plugin_folder: scalar String,
});

proto_message!(RecentsFavoritesItem {
    1 => name: scalar String,
    2 => folder_key: scalar String,
    3 => folder_name: scalar String,
    4 => is_factory: scalar bool,
    5 => is_plugin: scalar bool,
    6 => product_key: scalar String,
});

proto_message!(
    /// Envelope type 20. Descriptor `type` is `item_type`.
    RecentsFavoritesMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => is_favorites: scalar bool,
    4 => items: repeated RecentsFavoritesItem,
    5 => item_type: scalar EnumValue<RecentFavoriteType>,
});

/// Cloud credentials carried by `CloudLoginMessage`. `Debug` prints only
/// lengths so tokens never reach logs by accident; no credential is stored,
/// refreshed or sent by this crate.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct AuthorizationToken {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i32>,
}

impl fmt::Debug for AuthorizationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorizationToken")
            .field(
                "access_token_len",
                &self.access_token.as_ref().map(String::len),
            )
            .field(
                "refresh_token_len",
                &self.refresh_token.as_ref().map(String::len),
            )
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

impl Message for AuthorizationToken {
    fn merge(
        &mut self,
        fields: &[WireField<'_>],
        _decoder: &mut Decoder,
        _depth: usize,
    ) -> CodecResult<()> {
        for field in fields {
            match field.number {
                1 => merge_field!(scalar, self.access_token, field, _decoder, _depth),
                2 => merge_field!(scalar, self.refresh_token, field, _decoder, _depth),
                3 => merge_field!(scalar, self.expires_in, field, _decoder, _depth),
                _ => {}
            }
        }
        Ok(())
    }

    fn write(&self, encoder: &mut Encoder) -> CodecResult<()> {
        write_field!(scalar, self.access_token, 1, encoder);
        write_field!(scalar, self.refresh_token, 2, encoder);
        write_field!(scalar, self.expires_in, 3, encoder);
        Ok(())
    }
}

message_api!(AuthorizationToken);

proto_message!(
    /// Envelope type 18. Decoding a login payload is not a login.
    CloudLoginMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => username: scalar String,
    4 => token: message AuthorizationToken,
});

proto_message!(DefaultParametersModel {
    1 => id: scalar String,
    2 => model: message Model,
});

proto_message!(
    /// Envelope type 19.
    DefaultParametersMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => row: scalar i32,
    4 => column: scalar i32,
    5 => all_defaults: repeated DefaultParametersModel,
});

proto_message!(
    /// Envelope type 40. Backup JSON is carried as a string and not parsed.
    LocalBackupMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => backup_json: scalar String,
    4 => can_apply_backup: scalar bool,
    5 => applied_backup: scalar bool,
    6 => is_last_chunk: scalar bool,
});

proto_message!(CloudBackupItem {
    1 => user_id: scalar String,
    2 => client_type: scalar String,
    3 => client_version: scalar String,
    4 => name: scalar String,
    5 => timestamp: scalar String,
    6 => access_url: scalar String,
    7 => key: scalar String,
    8 => download_timestamp: scalar String,
});

proto_message!(
    /// Envelope type 41.
    CloudBackupMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => backups: repeated CloudBackupItem,
    4 => error_code: scalar EnumValue<CloudBackupErrorCode>,
    5 => cloud_error_code: scalar i32,
    6 => installation_progress: scalar i32,
    7 => is_busy: scalar bool,
});

proto_message!(
    /// Envelope type 44.
    LogsMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => correlation_id: scalar String,
});

proto_message!(
    /// Envelope type 45. Descriptor `type` is `queue_type`.
    ProcessDownloadsQueueMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => queue_type: scalar i32,
});

proto_message!(CloudTransferStateItem {
    1 => id: scalar String,
    2 => state: scalar EnumValue<CloudTransferItemState>,
});

proto_message!(CloudTransferState {
    1 => items: repeated CloudTransferStateItem,
});

proto_message!(
    /// Envelope type 46. Descriptor `type` is `product_type`; `to_indices`
    /// is a packed repeated int32.
    CloudProductMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => product_type: scalar i32,
    4 => from_folder: scalar String,
    5 => to_folder: scalar String,
    6 => to_indices: packed i32,
    7 => product_keys: repeated_scalar String,
    8 => cloud_transfer_state: message CloudTransferState,
});

proto_message!(ModuleStatsItem {
    1 => id: scalar u32,
    2 => cpu_percent: scalar f32,
    3 => dm_heap_bytes: scalar u32,
    4 => pm_heap_bytes: scalar u32,
    5 => sd_heap_bytes: scalar u32,
    6 => dmda_bytes: scalar u32,
    7 => pmda_bytes: scalar u32,
    8 => pmco_bytes: scalar u32,
    9 => edma_percent: scalar f32,
    10 => singletons: scalar i32,
    11 => latency: scalar i32,
    12 => reuse_pmco: scalar bool,
    13 => has_sidechain: scalar bool,
});

proto_message!(
    /// Both lists are packed repeated uint32.
    ModuleStatsSizeRelationPair {
    1 => smaller_models: packed u32,
    2 => larger_models: packed u32,
});

proto_message!(ModuleStatsSizeRelation {
    1 => id: scalar u32,
    2 => relations: message ModuleStatsSizeRelationPair,
});

proto_message!(ModuleStatsValidModelId {
    1 => id: scalar u32,
    2 => valid: scalar bool,
});

proto_message!(
    /// Envelope type 35.
    ModuleStatsMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => stats: repeated ModuleStatsItem,
    4 => size_relations: repeated ModuleStatsSizeRelation,
    5 => valid_id: repeated ModuleStatsValidModelId,
    6 => core_stats: repeated ModuleStatsItem,
});

proto_message!(NeuralCaptureParameter {
    1 => index: scalar i32,
    2 => value: scalar f32,
});

proto_message!(NeuralCaptureSaveInfo {
    1 => name: scalar String,
    2 => device: scalar i32,
    3 => instrument: scalar i32,
});

proto_message!(
    /// Envelope type 36. State and error numbering is not recovered.
    NeuralCaptureMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => try_to_show_dialog: scalar bool,
    4 => show_dialog_fail_reason: scalar String,
    5 => show_dialog: scalar bool,
    6 => state: scalar i32,
    7 => parameters: repeated NeuralCaptureParameter,
    8 => progress: scalar i32,
    9 => toggle_ab_model: scalar bool,
    10 => save_info: message NeuralCaptureSaveInfo,
    11 => error_id: scalar i32,
    12 => model_ab_bypass: scalar bool,
    13 => model_ab: message Model,
    14 => model_ab_preset: message ModelPresetID,
});

proto_message!(
    /// Envelope type 63. The descriptor declares field 13 before 9; output
    /// is numeric order.
    NeuralCapture2Message {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => state: scalar i32,
    4 => job_id: scalar String,
    5 => save_info: message NeuralCaptureSaveInfo,
    6 => parameters: repeated NeuralCaptureParameter,
    7 => cabsim_bypass: scalar bool,
    8 => cabsim: message Model,
    9 => error_id: scalar i32,
    10 => error_string: scalar String,
    11 => open_dialog: scalar bool,
    12 => start_from_scratch: scalar bool,
    13 => cabsim_model_preset: message ModelPresetID,
});

proto_message!(ModelPreset {
    1 => id: message ModelPresetID,
    2 => name: scalar String,
    3 => is_default: scalar bool,
    4 => model: message Model,
});

proto_message!(
    /// Envelope type 71. Hashes are signed int32 here, unlike `Model.hash`.
    ModelPresetMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => presets: repeated ModelPreset,
    4 => create_from_row: scalar i32,
    5 => create_from_column: scalar i32,
    6 => create_from_hash: scalar i32,
    7 => loaded_row: scalar i32,
    8 => loaded_column: scalar i32,
    9 => loaded_hash: scalar i32,
    10 => loaded_capture_version: scalar i32,
    11 => loaded_preset_id: message ModelPresetID,
});

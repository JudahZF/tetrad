//! Runtime-critical envelopes: mode, preset recall, undo/redo, CPU load,
//! confirmation dialogs, comms-buffer reset, licenses, grid and grid moves,
//! screenshots and UI meters. Observations only; nothing here changes mode,
//! loads a preset, undoes, confirms or resets anything.

use super::{
    CodecLimits, CodecResult, ConfirmationAction, Decoded, EnumValue, GridUpdateType,
    LicenseUpdateStatus, MessageAction, ModelPresetID, RecallPresetReason,
};

proto_message!(
    /// `modes` is a packed repeated uint32; mode numbering is not recovered.
    AvailableModes {
    1 => modes: packed u32,
});

proto_message!(
    /// Envelope type 14. Fields 2 through 5 are `proto3_optional`.
    ModeMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => mode: scalar u32,
    4 => available_modes: message AvailableModes,
    5 => atma_page: scalar u32,
});

proto_message!(
    /// Envelope type 15. `preset` is the Preset.proto `BinaryPreset`, kept as
    /// exact bytes for `tetrad_preset::PresetDocument::parse`; this crate does not
    /// load it.
    RecallPresetMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => preset: raw_message,
    4 => reason: scalar EnumValue<RecallPresetReason>,
});

proto_message!(
    /// Envelope type 1. `preset` is raw `BinaryPreset` bytes as in
    /// `RecallPresetMessage`. Fields 3 through 6 are `proto3_optional`.
    GridMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => preset: raw_message,
    4 => update_type: scalar EnumValue<GridUpdateType>,
    5 => model_preset_to_load: message ModelPresetID,
    6 => control_is_auditioning: scalar bool,
});

proto_message!(GridMoveElement {
    1 => from_row: scalar u32,
    2 => from_col: scalar u32,
    3 => to_row: scalar u32,
    4 => to_col: scalar u32,
    5 => is_drop: scalar bool,
});

proto_message!(
    /// Descriptor `modelIds` is `model_ids`; packed repeated uint32.
    GridMoveRow {
    1 => model_ids: packed u32,
});

proto_message!(GridMoveGrid {
    1 => rows: repeated GridMoveRow,
});

proto_message!(
    /// Envelope type 12. No action field; `move` is `moves` in Rust.
    GridMoveMessage {
    1 => request_id: scalar u64,
    2 => moves: repeated GridMoveElement,
    3 => grid: message GridMoveGrid,
});

proto_message!(
    /// Envelope type 21. Frame lists are packed repeated int32; frame
    /// numbering belongs to the stock undo stack and is not recovered.
    UndoRedoMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => undo_frames: packed i32,
    4 => redo_frames: packed i32,
    5 => undo: scalar bool,
    6 => redo: scalar bool,
});

proto_message!(CPUColumnLoad {
    1 => cpu_load: scalar f32,
    2 => is_on_core2: scalar bool,
});

proto_message!(CPUChainLoad {
    1 => columns: repeated CPUColumnLoad,
});

proto_message!(
    /// Envelope type 26. Chains and columns keep wire order.
    CPULoadMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => cpu_total_load: scalar f32,
    4 => chains: repeated CPUChainLoad,
});

proto_message!(
    /// Envelope type 47. No action field; `confirmation_id` is proto3 implicit
    /// presence. `actions` is a packed repeated enum. `response` is the
    /// answered action; this crate neither shows nor answers dialogs.
    ConfirmationMessage {
    1 => request_id: scalar u64,
    2 => confirmation_id: scalar u64,
    3 => title: scalar String,
    4 => msg: scalar String,
    5 => actions: packed EnumValue<ConfirmationAction>,
    6 => response: scalar EnumValue<ConfirmationAction>,
    7 => ok_title: scalar String,
    8 => cancel_title: scalar String,
});

proto_message!(
    /// Envelope type 52. No action field. `Envelope::plain_payload` already
    /// admits the stock decompression bypass for this type; no buffer reset
    /// or session policy is performed here.
    ResetCommsBuffersMessage {
    1 => request_id: scalar u64,
    2 => session_id: scalar String,
});

proto_message!(
    /// `plugin_id` is proto3 implicit presence; the strings are optional.
    LicenseItem {
    1 => id: scalar String,
    2 => name: scalar String,
    3 => plugin_id: scalar u64,
    4 => license_type: scalar String,
    5 => expiration_date: scalar String,
});

proto_message!(
    /// Envelope type 58. Plugin ID lists are packed repeated uint64. This is
    /// a payload observation, not a license check or activation.
    LicenseMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => licenses: repeated LicenseItem,
    4 => is_enabled: scalar bool,
    5 => is_deactivated: scalar bool,
    6 => update_status: scalar EnumValue<LicenseUpdateStatus>,
    7 => new_plugin_ids: packed u64,
    8 => supported_plugin_ids: packed u64,
});

proto_message!(
    /// Envelope type 25. `png` is opaque bytes; no image decoding.
    ScreenshotMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => folder_name: scalar String,
    4 => is_factory: scalar bool,
    5 => index: scalar i32,
    6 => png: scalar Vec<u8>,
});

proto_message!(
    /// Envelope type 37.
    GridModelMeterMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => row: scalar i32,
    4 => column: scalar i32,
});

proto_message!(
    /// Envelope type 50. `models` is a packed repeated uint32 of model hashes.
    NewModelsMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => models: packed u32,
});

proto_message!(
    /// Envelope type 54.
    PinnedModelsMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => models: packed u32,
    4 => captures: repeated_scalar String,
});

proto_message!(
    /// Envelope type 51. The repository payload is opaque bytes.
    ModelRepoMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => model_repo_payload: scalar Vec<u8>,
});

proto_message!(GlobalEQParameter {
    1 => parameter_index: scalar i32,
    2 => value: scalar f32,
});

proto_message!(
    /// Envelope type 38. Parameter index numbering is not recovered here.
    GlobalEQMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => parameters: repeated GlobalEQParameter,
    4 => bypassed: scalar bool,
    5 => model_preset_to_load: message ModelPresetID,
});

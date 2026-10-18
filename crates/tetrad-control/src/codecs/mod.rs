//! Offline Cortex payload codecs, not action dispatch or audio state policy.
//!
//! `Decoded::original_payload` and `Decoded::fields` retain wire order, duplicates,
//! unknowns and noncanonical encodings. Typed messages are observations: every
//! scalar is `Option`, including proto3 implicit fields, so wire absence differs
//! from an explicit default. `encode` creates a new known-field payload; it is not
//! a lossless edit of a decoded payload. Use the original bytes for exact replay.

mod proto;
mod types;

pub use proto::{
    CodecError, CodecLimits, CodecResult, Decoded, RawMessageBytes, WireField, WireValue,
};
pub use types::{
    AtmaPowerOnMode, BackupsForwardRequestType, CloudBackupErrorCode, CloudDomain,
    CloudTransferItemState, ConfirmationAction, CortexMessageType, EnumValue, GainCalInputPortId,
    GainCalOutputPortId, GainCalibrationOutput, GenericErrorCode, GridUpdateType, HttpOperation,
    LicenseUpdateStatus, LogsForwardRequestType, LooperUpdateType, MessageAction, MidiClockOut,
    PowerOptions, ProtoEnum, RecallPresetReason, RecentFavoriteType, RemoteControlMouseType,
    SceneBlockBypass, SerializationResult, StorageInfoType, SuspendReason, TestFarmTestState,
    UnknownEnumValue, UpdaterCommand, UpdaterForwardRequestType, UpdaterState, UpdaterStatus,
    VersionDeviceType,
};

use crate::Envelope;
use proto::{Decoder, Encoder, Message, Scalar};

// Schema verified from ProductionAutomation.proto embedded at file 0x120dac8 and
// its dependency Preset.proto embedded at file 0x1207920 (both CorOS 4.1.0 ZenUI).
macro_rules! message_api {
    ($name:ident) => {
        impl $name {
            pub fn decode(payload: &[u8], limits: CodecLimits) -> CodecResult<Decoded<'_, Self>> {
                $crate::codecs::proto::decode(payload, limits)
            }

            pub fn encode(&self, limits: CodecLimits) -> CodecResult<Vec<u8>> {
                $crate::codecs::proto::encode(self, limits)
            }
        }
    };
}

macro_rules! field_type {
    (scalar $ty:ty) => { Option<$ty> };
    (repeated_scalar $ty:ty) => { Vec<$ty> };
    (packed $ty:ty) => { Vec<$ty> };
    (message $ty:ty) => { Option<$ty> };
    (raw_message) => { Option<$crate::codecs::RawMessageBytes> };
    (repeated $ty:ty) => { Vec<$ty> };
}

/// Singular scalars keep the last correct-wire occurrence. Repeated scalars
/// append per correct-wire occurrence (length-delimited `String`/`Vec<u8>`
/// only). Packed numerics accept packed and unpacked occurrences. Singular
/// messages merge every occurrence in wire order; raw messages concatenate
/// their exact bytes. Repeated messages append per occurrence.
macro_rules! merge_field {
    (scalar, $slot:expr, $field:expr, $decoder:expr, $depth:expr) => {
        if let Some(value) = $crate::codecs::proto::Scalar::read($field)? {
            $slot = Some(value);
        }
    };
    (repeated_scalar, $slot:expr, $field:expr, $decoder:expr, $depth:expr) => {
        if let Some(value) = $crate::codecs::proto::Scalar::read($field)? {
            $slot.push(value);
        }
    };
    (packed, $slot:expr, $field:expr, $decoder:expr, $depth:expr) => {
        $decoder.packed($field, &mut $slot)?
    };
    (message, $slot:expr, $field:expr, $decoder:expr, $depth:expr) => {
        if let $crate::codecs::WireValue::Bytes(bytes) = $field.value {
            let nested = $decoder.scan(bytes, $depth + 1)?;
            $crate::codecs::proto::Message::merge(
                $slot.get_or_insert_default(),
                &nested,
                $decoder,
                $depth + 1,
            )?;
        }
    };
    (raw_message, $slot:expr, $field:expr, $decoder:expr, $depth:expr) => {
        if let $crate::codecs::WireValue::Bytes(bytes) = $field.value {
            $decoder.scan(bytes, $depth + 1)?;
            $slot.get_or_insert_default().bytes.extend_from_slice(bytes);
        }
    };
    (repeated, $slot:expr, $field:expr, $decoder:expr, $depth:expr) => {
        if let $crate::codecs::WireValue::Bytes(bytes) = $field.value {
            let nested = $decoder.scan(bytes, $depth + 1)?;
            let mut item = Default::default();
            $crate::codecs::proto::Message::merge(&mut item, &nested, $decoder, $depth + 1)?;
            $slot.push(item);
        }
    };
}

macro_rules! write_field {
    (scalar, $slot:expr, $number:literal, $encoder:expr) => {
        if let Some(value) = &$slot {
            $crate::codecs::proto::Scalar::write(value, $number, $encoder)?;
        }
    };
    (repeated_scalar, $slot:expr, $number:literal, $encoder:expr) => {
        for value in &$slot {
            $crate::codecs::proto::Scalar::write(value, $number, $encoder)?;
        }
    };
    (packed, $slot:expr, $number:literal, $encoder:expr) => {
        $encoder.packed($number, &$slot)?;
    };
    (message, $slot:expr, $number:literal, $encoder:expr) => {
        if let Some(message) = &$slot {
            $encoder.message($number, message)?;
        }
    };
    (raw_message, $slot:expr, $number:literal, $encoder:expr) => {
        if let Some(raw) = &$slot {
            $encoder.raw_message($number, raw)?;
        }
    };
    (repeated, $slot:expr, $number:literal, $encoder:expr) => {
        for item in &$slot {
            $encoder.message($number, item)?;
        }
    };
}

/// Field kinds: `scalar T` (Option<T>), `repeated_scalar T` (Vec<T>, strings/bytes),
/// `packed T` (Vec<T>, numerics/bools/enums), `message M` (Option<M>),
/// `raw_message` (Option<RawMessageBytes>), `repeated M` (Vec<M>).
/// List fields in field-number order; `encode` writes them in that order.
macro_rules! proto_message {
    ($(#[$meta:meta])* $name:ident { $($number:literal => $field:ident: $kind:ident $($ty:ty)?),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq)]
        pub struct $name {
            $(pub $field: field_type!($kind $($ty)?),)*
        }

        impl $crate::codecs::proto::Message for $name {
            // Underscore names: scalar-only messages never touch the decoder.
            fn merge(
                &mut self,
                fields: &[$crate::codecs::WireField<'_>],
                _decoder: &mut $crate::codecs::proto::Decoder,
                _depth: usize,
            ) -> CodecResult<()> {
                for field in fields {
                    match field.number {
                        $($number => merge_field!($kind, self.$field, field, _decoder, _depth),)*
                        _ => {}
                    }
                }
                Ok(())
            }

            fn write(&self, encoder: &mut $crate::codecs::proto::Encoder) -> CodecResult<()> {
                $(write_field!($kind, self.$field, $number, encoder);)*
                Ok(())
            }
        }

        message_api!($name);
    };
}

mod library;
mod production;
mod runtime;
mod settings;
mod shared;

pub use library::*;
pub use production::*;
pub use runtime::*;
pub use settings::*;
pub use shared::*;

proto_message!(TunerMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => input_port_id: scalar i32,
    4 => frequency: scalar f32,
    5 => mute: scalar bool,
    6 => enable_meter: scalar bool,
    7 => meter: scalar f32,
});

proto_message!(
    /// Envelope type 13, outbound only. Inbound Scene uses `scene::SceneRequest`
    /// and stays outside `ControlMessage`.
    SceneMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => selected_scene: scalar u32,
});

proto_message!(ShowTunerMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => show: scalar bool,
});

proto_message!(
    /// Numeric state/transition/action values are observations, not recovered enums.
    /// Flag-like fields are uint32 in the descriptor, not protobuf bools.
    LooperStatus {
    1 => state: scalar u32,
    2 => progress: scalar f32,
    3 => undo_progress: scalar f32,
    4 => duplicate_cycle: scalar u32,
    5 => num_duplicate_cycles: scalar u32,
    6 => one_shot_stopped: scalar u32,
    7 => redo_available: scalar u32,
    8 => loop_length: scalar u32,
    9 => free_samples: scalar u32,
    10 => in_reverse: scalar u32,
    11 => one_shot: scalar u32,
    12 => half_speed: scalar u32,
    13 => fixed_duplicate_cycles: scalar u32,
    14 => armed: scalar u32,
    15 => waiting_for_cycle: scalar u32,
    16 => undo_count: scalar u32,
    17 => max_write_displacement: scalar f32,
    18 => min_write_displacement: scalar f32,
    19 => events_waiting_for_quantize: scalar u32,
    20 => current_clock: scalar u32,
    21 => transition: scalar u32,
    22 => action: scalar u32,
});

proto_message!(LooperMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => status: message LooperStatus,
    4 => state: scalar u32,
    5 => one_shot_play: scalar bool,
    6 => sync_start_waiting: scalar bool,
    7 => quantize_enabled: scalar bool,
    8 => update_type: scalar EnumValue<LooperUpdateType>,
});

proto_message!(KeepAliveMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => is_online: scalar bool,
    4 => timeout: scalar f32,
});

proto_message!(ConnectionMessage {
    1 => request_id: scalar u64,
    2 => connected: scalar bool,
});

proto_message!(SuspendConnectionMessage {
    1 => request_id: scalar u64,
    2 => suspend: scalar bool,
    3 => reason: scalar EnumValue<SuspendReason>,
});

proto_message!(
    /// Version 10 envelopes bypass stock decompression; `plain_payload` already
    /// admits them. Descriptor declares 11 and 14 last; wire order is numeric.
    VersionMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => linux_kernel_version: scalar String,
    4 => zenos_git_hash: scalar String,
    5 => zenwireless_fw_version: scalar String,
    6 => uboot_version: scalar String,
    7 => app_fw_version: scalar String,
    8 => bootloader_fw_version: scalar String,
    9 => device_serial_number: scalar String,
    10 => comms_version: scalar Vec<u8>,
    11 => cortex_control_version: scalar String,
    12 => device_type: scalar EnumValue<VersionDeviceType>,
    13 => is_ess: scalar bool,
    14 => cortex_control_version_valid: scalar bool,
    15 => custom_name: scalar String,
    16 => mac_address: scalar String,
    17 => zencoder_fw_app: scalar String,
    18 => zencoder_fw_bootloader: scalar String,
    19 => cloud_endpoint: scalar String,
});

proto_message!(
    /// Payload observation only. No update check, download, install or reboot.
    UpdaterMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => command: scalar EnumValue<UpdaterCommand>,
    4 => status: scalar EnumValue<UpdaterStatus>,
    5 => state: scalar EnumValue<UpdaterState>,
    6 => new_version_id: scalar String,
    7 => changelog: scalar String,
    8 => download_progress: scalar f32,
    9 => installation_progress: scalar f32,
});

proto_message!(
    /// No action field in the descriptor.
    GenericErrorMessage {
    1 => request_id: scalar u64,
    2 => error_code: scalar EnumValue<GenericErrorCode>,
});

proto_message!(ProductForwardHeaderPair {
    1 => name: scalar String,
    2 => value: scalar String,
});

proto_message!(ProductForwardRequest {
    1 => operation: scalar EnumValue<HttpOperation>,
    2 => url: scalar String,
    3 => timeout: scalar i32,
    4 => is_multipart: scalar bool,
    5 => payload: scalar Vec<u8>,
    6 => headers: repeated ProductForwardHeaderPair,
});

proto_message!(ProductForwardResponse {
    1 => payload: scalar Vec<u8>,
    2 => error_code: scalar i32,
});

proto_message!(ProductForwardMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => cloud_request_id: scalar u64,
    4 => request: message ProductForwardRequest,
    5 => response: message ProductForwardResponse,
});

proto_message!(BackupsForwardRequest {
    1 => request_type: scalar EnumValue<BackupsForwardRequestType>,
    2 => url: scalar String,
    3 => payload: scalar Vec<u8>,
    4 => is_last_chunk: scalar bool,
    5 => timeout: scalar i32,
    6 => headers: repeated ProductForwardHeaderPair,
});

proto_message!(BackupsForwardResponse {
    1 => payload: scalar Vec<u8>,
    2 => is_last_chunk: scalar bool,
    3 => error_code: scalar i32,
});

proto_message!(BackupsForwardMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => backups_request_id: scalar u64,
    4 => request: message BackupsForwardRequest,
    5 => response: message BackupsForwardResponse,
});

proto_message!(LogsForwardRequest {
    1 => request_type: scalar EnumValue<LogsForwardRequestType>,
    2 => url: scalar String,
    3 => payload: scalar Vec<u8>,
    4 => is_last_chunk: scalar bool,
    5 => timeout: scalar i32,
    6 => headers: repeated ProductForwardHeaderPair,
});

proto_message!(LogsForwardResponse {
    1 => payload: scalar Vec<u8>,
    2 => error_code: scalar i32,
});

proto_message!(LogsForwardMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => logs_request_id: scalar u64,
    4 => request: message LogsForwardRequest,
    5 => response: message LogsForwardResponse,
});

proto_message!(
    /// `payload` is a descriptor `string`, unlike the other forward requests.
    UpdaterForwardRequest {
    1 => request_type: scalar EnumValue<UpdaterForwardRequestType>,
    2 => url: scalar String,
    3 => timeout: scalar i32,
    4 => payload: scalar String,
    5 => headers: repeated ProductForwardHeaderPair,
});

proto_message!(UpdaterForwardResponse {
    1 => payload: scalar Vec<u8>,
    2 => is_last_chunk: scalar bool,
    3 => error_code: scalar i32,
});

proto_message!(UpdaterForwardMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => updater_request_id: scalar u64,
    4 => request: message UpdaterForwardRequest,
    5 => response: message UpdaterForwardResponse,
});

/// `ParamValue.value` oneof from Preset.proto: `int_value` 1, `float_value` 2,
/// `string_value` 3. This is the wire message, not `tetrad_preset::ParamValue`.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValueKind {
    /// Signed int32 varint; negatives use the ten-byte form.
    Int(i32),
    /// Exact fixed32 bits, including NaN and negative zero.
    Float(f32),
    String(String),
}

/// One `Param.param_values` element. Every correct-wire occurrence of fields
/// 1, 2 or 3 replaces the whole oneof, so the last one in wire order wins and
/// an earlier different case is cleared, as in the stock C++ parser. An empty
/// element decodes as `value: None` and re-encodes as zero bytes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParamValue {
    pub value: Option<ParamValueKind>,
}

impl ParamValue {
    pub fn int(value: i32) -> Self {
        Self {
            value: Some(ParamValueKind::Int(value)),
        }
    }

    pub fn float(value: f32) -> Self {
        Self {
            value: Some(ParamValueKind::Float(value)),
        }
    }

    pub fn string(value: impl Into<String>) -> Self {
        Self {
            value: Some(ParamValueKind::String(value.into())),
        }
    }

    /// `Some` only for the float case; an int case is not converted.
    pub fn as_float(&self) -> Option<f32> {
        match self.value {
            Some(ParamValueKind::Float(value)) => Some(value),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i32> {
        match self.value {
            Some(ParamValueKind::Int(value)) => Some(value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.value {
            Some(ParamValueKind::String(value)) => Some(value),
            _ => None,
        }
    }
}

impl Message for ParamValue {
    fn merge(
        &mut self,
        fields: &[WireField<'_>],
        _decoder: &mut Decoder,
        _depth: usize,
    ) -> CodecResult<()> {
        for field in fields {
            let case = match field.number {
                1 => i32::read(field)?.map(ParamValueKind::Int),
                2 => f32::read(field)?.map(ParamValueKind::Float),
                3 => String::read(field)?.map(ParamValueKind::String),
                _ => None,
            };
            if case.is_some() {
                self.value = case;
            }
        }
        Ok(())
    }

    fn write(&self, encoder: &mut Encoder) -> CodecResult<()> {
        match &self.value {
            Some(ParamValueKind::Int(value)) => value.write(1, encoder),
            Some(ParamValueKind::Float(value)) => value.write(2, encoder),
            Some(ParamValueKind::String(value)) => value.write(3, encoder),
            None => Ok(()),
        }
    }
}

message_api!(ParamValue);

proto_message!(
    /// Preset.proto `Param`, shared by presets and `GlobalTempoMessage.params`.
    /// Fields 1, 2, 3, 4, 6 and 7 are `proto3_optional`; the repeated string
    /// lists append per occurrence and keep order, including empty strings.
    /// Index numbering for tempo/metronome parameters is not recovered here.
    Param {
    1 => expression: scalar i32,
    2 => expression_min: scalar f32,
    3 => expression_max: scalar f32,
    4 => scene_mode: scalar bool,
    5 => param_values: repeated ParamValue,
    6 => index: scalar u32,
    7 => is_led: scalar bool,
    8 => dynamic_steps: repeated_scalar String,
    9 => dynamic_icons: repeated_scalar String,
    10 => dynamic_metadata: repeated_scalar String,
});

impl Param {
    /// First `param_values` element, which is the only value the stock
    /// GlobalTempo UPDATE receiver reads.
    pub fn first_value(&self) -> Option<&ParamValue> {
        self.param_values.first()
    }
}

proto_message!(
    /// Flag-like fields are uint32 in the descriptor, not protobuf bools.
    /// Fields 6 and 7 are signed; the descriptor marks none as optional.
    MetronomeStatusUpdate {
    1 => is_enabled: scalar u32,
    2 => preroll_enabled: scalar u32,
    3 => current_beat: scalar u32,
    4 => current_bar: scalar u32,
    5 => current_tick: scalar u32,
    6 => count_in_beats_remaining: scalar i32,
    7 => count_in_bars_remaining: scalar i32,
});

proto_message!(
    /// Envelope type 33. `params` keeps every repeated element in wire order;
    /// `metronome_status` merges duplicate occurrences like other singular
    /// messages. No metronome, tempo or DSP action is taken by this crate.
    GlobalTempoMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => params: repeated Param,
    4 => metronome_status: message MetronomeStatusUpdate,
});

impl GlobalTempoMessage {
    /// Observation of the stock UPDATE receiver gate (`0x109b60`): it reads
    /// only the first `Param`, requires its `index`, and requires the first
    /// `param_values` element to be the float case. Returns `(index, value)`
    /// when that shape is present. This accessor does not apply the value;
    /// the session/application owner decides what an index means.
    pub fn first_indexed_float(&self) -> Option<(u32, f32)> {
        let param = self.params.first()?;
        Some((param.index?, param.first_value()?.as_float()?))
    }
}

// Scalar-only envelopes with a dispatcher case (`0x0028e8ac`) and a named
// receiver class. `enable`, `show`, `pressed`, `is_dirty`, scene indices and
// `ms_since_epoch` are proto3 implicit presence; `Option` still records absence.

proto_message!(
    /// Envelope type 16. No action field in the descriptor.
    EnableCaptureOutMessage {
    1 => request_id: scalar u64,
    2 => enable: scalar bool,
});

proto_message!(MasterVolumeMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => volume: scalar f32,
    4 => engaged: scalar bool,
    5 => calibrate: scalar bool,
});

proto_message!(SceneCopyMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => from_index: scalar i32,
    4 => to_index: scalar i32,
    5 => is_swap: scalar bool,
});

proto_message!(SceneLabelMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => index: scalar i32,
    4 => label: scalar String,
});

proto_message!(ShowGigViewMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => show: scalar bool,
});

proto_message!(PresetDirtyMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => is_dirty: scalar bool,
});

proto_message!(RecentSearchesMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => searches: repeated_scalar String,
});

proto_message!(CompilerInhibitedModulesMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => global_gate: scalar bool,
    4 => global_eq: scalar bool,
});

proto_message!(SystemTimeSyncMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => ms_since_epoch: scalar u64,
});

proto_message!(
    /// `color` is a uint32 in the descriptor; its packing is not recovered.
    SceneColorMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => index: scalar i32,
    4 => color: scalar u32,
});

proto_message!(GigViewButtonMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => button_index: scalar u64,
    4 => pressed: scalar bool,
});

/// Shared shape of the four HTTP forwarding envelopes: a control request ID,
/// a per-service forwarding correlation ID (field 3), and optional request and
/// response halves. Accessors only; no HTTP client, queue or cloud policy.
pub trait ForwardMessage {
    type Request;
    type Response;

    fn action(&self) -> Option<EnumValue<MessageAction>>;
    fn request_id(&self) -> Option<u64>;
    /// Field 3: cloud/backups/logs/updater request ID, proto3 implicit presence.
    fn forward_request_id(&self) -> Option<u64>;
    fn request(&self) -> Option<&Self::Request>;
    fn response(&self) -> Option<&Self::Response>;
    fn request_headers(&self) -> &[ProductForwardHeaderPair];
}

macro_rules! forward_message {
    ($name:ident, $request:ident, $response:ident, $forward_id:ident) => {
        impl ForwardMessage for $name {
            type Request = $request;
            type Response = $response;

            fn action(&self) -> Option<EnumValue<MessageAction>> {
                self.action
            }

            fn request_id(&self) -> Option<u64> {
                self.request_id
            }

            fn forward_request_id(&self) -> Option<u64> {
                self.$forward_id
            }

            fn request(&self) -> Option<&$request> {
                self.request.as_ref()
            }

            fn response(&self) -> Option<&$response> {
                self.response.as_ref()
            }

            fn request_headers(&self) -> &[ProductForwardHeaderPair] {
                self.request
                    .as_ref()
                    .map_or(&[], |request| request.headers.as_slice())
            }
        }
    };
}

forward_message!(
    ProductForwardMessage,
    ProductForwardRequest,
    ProductForwardResponse,
    cloud_request_id
);
forward_message!(
    BackupsForwardMessage,
    BackupsForwardRequest,
    BackupsForwardResponse,
    backups_request_id
);
forward_message!(
    LogsForwardMessage,
    LogsForwardRequest,
    LogsForwardResponse,
    logs_request_id
);
forward_message!(
    UpdaterForwardMessage,
    UpdaterForwardRequest,
    UpdaterForwardResponse,
    updater_request_id
);

/// The `action` (field 1) of a top-level message; `no_action` marks the few whose field 1 is
/// `request_id`.
macro_rules! message_action {
    ($message:ident) => {
        $message.action
    };
    ($message:ident, no_action) => {
        None
    };
}

macro_rules! control_messages {
    ($($variant:ident($message:ident $(, $no_action:ident)?)),* $(,)?) => {
        /// Typed payload selection only. No receiver policy or state mutation.
        /// Variant sizes differ widely (GeneralSettings versus KeepAlive); one
        /// value is decoded per envelope and matched, not stored in bulk, so
        /// the uniform unboxed API is kept.
        #[allow(clippy::large_enum_variant)]
        #[derive(Debug, Clone, PartialEq)]
        pub enum ControlMessage {
            $($variant($message),)*
        }

        impl ControlMessage {
            /// Applies the unchanged `Envelope::plain_payload` guards first, then
            /// selects the codec by envelope type. Scene (13) keeps its separate
            /// API; Undefined (0) and NumberOfMessageTypes (73) have no payload
            /// schema, so all three are `UnsupportedMessageType`.
            pub fn decode(envelope: &Envelope, limits: CodecLimits) -> CodecResult<Decoded<'_, Self>> {
                let payload = envelope.plain_payload().map_err(CodecError::Envelope)?;
                let message_type = CortexMessageType::try_from(envelope.message_type)
                    .map_err(|_| CodecError::UnsupportedMessageType(envelope.message_type))?;
                match message_type {
                    $(CortexMessageType::$variant => {
                        Ok($message::decode(payload, limits)?.map(Self::$variant))
                    })*
                    _ => Err(CodecError::UnsupportedMessageType(envelope.message_type)),
                }
            }

            pub fn message_type(&self) -> CortexMessageType {
                match self {
                    $(Self::$variant(_) => CortexMessageType::$variant,)*
                }
            }

            pub fn encode(&self, limits: CodecLimits) -> CodecResult<Vec<u8>> {
                match self {
                    $(Self::$variant(message) => message.encode(limits),)*
                }
            }

            /// Every envelope schema has `request_id` (field 1 or 2).
            pub fn request_id(&self) -> Option<u64> {
                match self {
                    $(Self::$variant(message) => message.request_id,)*
                }
            }

            /// The message action; `None` when absent or when the type has none (GridMove,
            /// Connection and six others).
            pub fn action(&self) -> Option<EnumValue<MessageAction>> {
                match self {
                    $(Self::$variant(_message) => message_action!(_message $(, $no_action)?),)*
                }
            }

            pub fn request_id_mut(&mut self) -> &mut Option<u64> {
                match self {
                    $(Self::$variant(message) => &mut message.request_id,)*
                }
            }
        }

        $(impl From<$message> for ControlMessage {
            fn from(message: $message) -> Self {
                Self::$variant(message)
            }
        })*
    };
}

control_messages!(
    Grid(GridMessage),
    SetlistPosition(SetlistPositionMessage),
    IOSettings(IOSettingsMessage),
    File(FileMessage),
    IOMeter(IOMeterMessage),
    Tuner(TunerMessage),
    Diagnostics(DiagnosticsMessage),
    MIDISettings(MIDISettingsMessage),
    GeneralSettings(GeneralSettingsMessage),
    Version(VersionMessage),
    ProductionAutomationMode(ProductionAutomationModeMessage, no_action),
    GridMove(GridMoveMessage, no_action),
    Mode(ModeMessage),
    RecallPreset(RecallPresetMessage),
    EnableCaptureOut(EnableCaptureOutMessage, no_action),
    MasterVolume(MasterVolumeMessage),
    CloudLogin(CloudLoginMessage),
    DefaultParameters(DefaultParametersMessage),
    RecentsFavorites(RecentsFavoritesMessage),
    UndoRedo(UndoRedoMessage),
    SceneCopy(SceneCopyMessage),
    SceneLabel(SceneLabelMessage),
    ShowGigView(ShowGigViewMessage),
    Screenshot(ScreenshotMessage),
    CPULoad(CPULoadMessage),
    ShowTuner(ShowTunerMessage),
    Looper(LooperMessage),
    ProductForward(ProductForwardMessage),
    BackupsForward(BackupsForwardMessage),
    LogsForward(LogsForwardMessage),
    KeepAlive(KeepAliveMessage),
    GlobalTempo(GlobalTempoMessage),
    PresetDirty(PresetDirtyMessage),
    ModuleStats(ModuleStatsMessage),
    NeuralCapture(NeuralCaptureMessage),
    GridModelMeter(GridModelMeterMessage),
    GlobalEQ(GlobalEQMessage),
    RecentSearches(RecentSearchesMessage),
    LocalBackup(LocalBackupMessage),
    CloudBackup(CloudBackupMessage),
    CompilerInhibitedModules(CompilerInhibitedModulesMessage),
    SystemTimeSync(SystemTimeSyncMessage),
    Logs(LogsMessage),
    ProcessDownloadsQueue(ProcessDownloadsQueueMessage),
    CloudProduct(CloudProductMessage),
    Confirmation(ConfirmationMessage, no_action),
    SceneColor(SceneColorMessage),
    Connection(ConnectionMessage, no_action),
    NewModels(NewModelsMessage),
    ModelRepo(ModelRepoMessage),
    ResetCommsBuffers(ResetCommsBuffersMessage, no_action),
    SuspendConnection(SuspendConnectionMessage, no_action),
    PinnedModels(PinnedModelsMessage),
    GigViewButton(GigViewButtonMessage),
    GenericError(GenericErrorMessage, no_action),
    BulkOperation(BulkOperationMessage),
    License(LicenseMessage),
    PresetSpeedTest(PresetSpeedTestMessage),
    Updater(UpdaterMessage),
    UpdaterForward(UpdaterForwardMessage),
    GainCalibration(GainCalibrationMessage),
    NeuralCapture2(NeuralCapture2Message),
    Serialization(SerializationMessage),
    TestFarm(TestFarmMessage),
    ProductionTest(ProductionTestMessage),
    LoadAutomatedTestPreset(LoadAutomatedTestPresetMessage),
    SetTestPresetInputOutputPorts(SetTestPresetInputOutputPortsMessage),
    SetTestPresetSplitMixPoints(SetTestPresetSplitMixPointsMessage),
    GenerateTestPreset(GenerateTestPresetMessage),
    ModelPreset(ModelPresetMessage),
    RemoteControl(RemoteControlMessage),
);

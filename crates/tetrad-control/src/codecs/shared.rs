//! Nested schemas shared by several envelopes: Preset.proto `Model` and its
//! parts, `MidiMessageInfo`, and the ProductionAutomation `ModelPresetID`
//! reference. `BinaryPreset` itself stays raw; see `RawMessageBytes`.

use super::{CodecLimits, CodecResult, Decoded, Param};

proto_message!(
    /// Preset.proto `Expression`, used by `Model.bypass_expression`. Unlike
    /// `Param`, none of these fields are `proto3_optional`.
    Expression {
    1 => expression: scalar i32,
    2 => expression_min: scalar f32,
    3 => expression_max: scalar f32,
});

proto_message!(
    /// Preset.proto `ExpressionBypassInfo`; descriptor `type` is `bypass_type`.
    ExpressionBypassInfo {
    1 => bypass_type: scalar u32,
    2 => invert: scalar bool,
    3 => delay_ms: scalar u32,
    4 => latch_emulation: scalar bool,
});

proto_message!(
    /// Preset.proto `Model`: one grid block with its parameters. Carried by
    /// DefaultParameters, NeuralCapture, NeuralCapture2 and ModelPreset.
    /// Fields 1 and 5 through 8 are `proto3_optional`.
    Model {
    1 => hash: scalar u32,
    2 => params: repeated Param,
    3 => bypass_expression: repeated Expression,
    4 => expression_bypass_info: repeated ExpressionBypassInfo,
    5 => column: scalar u32,
    6 => notify_flag: scalar bool,
    7 => sidechain_sink_flag: scalar bool,
    8 => sidechain_source_flag: scalar bool,
});

proto_message!(
    /// Preset.proto `MidiMessageInfo`; descriptor `type` is `message_type`.
    /// Numeric meanings of type and params are not recovered here.
    MidiMessageInfo {
    1 => message_type: scalar u32,
    2 => channel: scalar u32,
    3 => param1: scalar u32,
    4 => param2: scalar u32,
    5 => param3: scalar u32,
});

proto_message!(
    /// Reference to a stored model preset. All fields are `proto3_optional`.
    ModelPresetID {
    1 => value: scalar String,
    2 => is_factory: scalar bool,
    3 => hash: scalar u32,
});

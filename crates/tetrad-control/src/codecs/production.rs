//! Production, test-farm and remote-control envelopes. Observations only;
//! no LED, serialization, test preset or remote input is driven here.

use super::{
    CodecLimits, CodecResult, Decoded, EnumValue, GainCalInputPortId, GainCalOutputPortId,
    MessageAction, RemoteControlMouseType, SerializationResult, TestFarmTestState,
};

proto_message!(
    /// Envelope type 11. No action field.
    ProductionAutomationModeMessage {
    1 => request_id: scalar u64,
    2 => enable: scalar bool,
});

proto_message!(
    /// Envelope type 59.
    PresetSpeedTestMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => setlist_name: scalar String,
    4 => num_laps: scalar i32,
    5 => randomize: scalar bool,
    6 => csv_file: scalar String,
});

proto_message!(
    /// Envelope type 64.
    SerializationMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => serial_number: scalar String,
    4 => display_type: scalar String,
    5 => result: scalar EnumValue<SerializationResult>,
});

proto_message!(
    /// Envelope type 65. `test_id` is implicit presence.
    TestFarmMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => test_id: scalar String,
    4 => state: scalar EnumValue<TestFarmTestState>,
});

proto_message!(
    /// Envelope type 66. `led_color` packing is not recovered.
    ProductionTestMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => led_color: scalar u32,
    4 => enable_tempo_led: scalar bool,
});

proto_message!(
    /// Envelope type 67. Only the common action/request pair.
    LoadAutomatedTestPresetMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
});

proto_message!(
    /// Envelope type 68. Port lists are packed repeated enums; row lists are
    /// packed repeated uint32.
    SetTestPresetInputOutputPortsMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => input_ports: packed EnumValue<GainCalInputPortId>,
    4 => output_ports: packed EnumValue<GainCalOutputPortId>,
    5 => input_port_rows: packed u32,
    6 => output_port_rows: packed u32,
});

proto_message!(
    /// Envelope type 69. All four lists are packed; `hard_pan_mixer` is a
    /// packed repeated bool.
    SetTestPresetSplitMixPointsMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => split_mix_rows: packed u32,
    4 => split_points: packed i32,
    5 => mix_points: packed i32,
    6 => hard_pan_mixer: packed bool,
});

proto_message!(
    /// Envelope type 70.
    GenerateTestPresetMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => latency_test_mode: scalar u32,
    4 => grid_density: scalar u32,
});

proto_message!(
    /// Descriptor `type` is `mouse_type`.
    RemoteControlMouse {
    1 => x: scalar f32,
    2 => y: scalar f32,
    3 => mouse_type: scalar EnumValue<RemoteControlMouseType>,
    4 => to_x: scalar f32,
    5 => to_y: scalar f32,
});

proto_message!(RemoteControlScreenshot {
    1 => payload: scalar Vec<u8>,
    2 => x: scalar u32,
    3 => y: scalar u32,
    4 => w: scalar u32,
    5 => h: scalar u32,
});

proto_message!(RemoteControlGraphicsTree {
    1 => payload: scalar String,
});

proto_message!(
    /// Envelope type 72. Mouse, screenshot and graphics-tree halves are all
    /// optional singular messages. No input is injected or screen captured.
    RemoteControlMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => mouse: message RemoteControlMouse,
    4 => screenshot: message RemoteControlScreenshot,
    5 => graphics_tree: message RemoteControlGraphicsTree,
});

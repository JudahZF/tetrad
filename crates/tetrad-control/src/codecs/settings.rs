//! I/O settings and meters, diagnostics, MIDI settings, general settings and
//! gain calibration. Observations only; no HAL, port, power or reset action.

use super::{
    AtmaPowerOnMode, CloudDomain, CodecLimits, CodecResult, Decoded, EnumValue, GainCalInputPortId,
    GainCalOutputPortId, GainCalibrationOutput, MessageAction, MidiClockOut, MidiMessageInfo,
    ModelPresetID, PowerOptions, SceneBlockBypass, StorageInfoType,
};

proto_message!(HeadphonesFeedLevel {
    1 => level: scalar f32,
    2 => output_port_id: scalar u32,
});

proto_message!(
    /// `input_port_id` is implicit presence; the rest are `proto3_optional`.
    /// Z-mode, type and ground lift are floats in the descriptor.
    InputPortSettings {
    1 => input_port_id: scalar u32,
    2 => level: scalar f32,
    3 => input_zmode: scalar f32,
    4 => input_type: scalar f32,
    5 => ground_lift: scalar f32,
    6 => plugged: scalar bool,
});

proto_message!(OutputPortSettings {
    1 => output_port_id: scalar u32,
    2 => level: scalar f32,
    3 => ground_lift: scalar f32,
    4 => mute: scalar bool,
    5 => plugged: scalar bool,
});

proto_message!(HeadphonesSettings {
    1 => level: scalar f32,
    2 => hp_feed: repeated HeadphonesFeedLevel,
    3 => plugged: scalar bool,
});

proto_message!(USBPortSettings {
    1 => level: scalar f32,
    2 => hp_select: scalar f32,
    3 => plugged: scalar bool,
    4 => dry_wet: scalar f32,
});

proto_message!(
    /// `exp_port_id` is a signed int32.
    ExpPortSettings {
    1 => exp_port_id: scalar i32,
    2 => plugged: scalar bool,
    3 => level: scalar f32,
    4 => calibrating: scalar bool,
});

proto_message!(MIDIPortSettings {
    1 => midi_thru: scalar f32,
});

proto_message!(PortSettings {
    1 => in_port: repeated InputPortSettings,
    2 => out_port: repeated OutputPortSettings,
    3 => hp_port: message HeadphonesSettings,
    4 => usb_port: message USBPortSettings,
    5 => midi_port: message MIDIPortSettings,
    6 => exp_port: repeated ExpPortSettings,
});

proto_message!(
    /// Envelope type 3.
    IOSettingsMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => settings: message PortSettings,
    4 => xlr1_2_linked: scalar bool,
    5 => out3_4_linked: scalar bool,
    6 => preset_to_load: message ModelPresetID,
});

proto_message!(
    /// Envelope type 5. Every meter is an optional float; field 19 is the
    /// headphone limiter flag. Meter scaling is not recovered here.
    IOMeterMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => input_1: scalar f32,
    4 => input_2: scalar f32,
    5 => return_1: scalar f32,
    6 => return_2: scalar f32,
    7 => xlr_1: scalar f32,
    8 => xlr_1_limiter: scalar f32,
    9 => xlr_2: scalar f32,
    10 => xlr_2_limiter: scalar f32,
    11 => out_3: scalar f32,
    12 => out_3_limiter: scalar f32,
    13 => out_4: scalar f32,
    14 => out_4_limiter: scalar f32,
    15 => send_1: scalar f32,
    16 => send_2: scalar f32,
    17 => hp_l: scalar f32,
    18 => hp_r: scalar f32,
    19 => hp_limiter_active: scalar bool,
    20 => usb_output_1l: scalar f32,
    21 => usb_output_1r: scalar f32,
    22 => usb_output_2l: scalar f32,
    23 => usb_output_2r: scalar f32,
    24 => usb_output_3l: scalar f32,
    25 => usb_output_3r: scalar f32,
    26 => usb_output_4l: scalar f32,
    27 => usb_output_4r: scalar f32,
    28 => usb_input_1l: scalar f32,
    29 => usb_input_1r: scalar f32,
    30 => usb_input_2l: scalar f32,
    31 => usb_input_2r: scalar f32,
    32 => usb_input_3l: scalar f32,
    33 => usb_input_3r: scalar f32,
    34 => usb_input_4l: scalar f32,
    35 => usb_input_4r: scalar f32,
    36 => grid_xlr_1: scalar f32,
    37 => grid_xlr_2: scalar f32,
    38 => grid_out_3: scalar f32,
    39 => grid_out_4: scalar f32,
    40 => grid_send_1: scalar f32,
    41 => grid_send_2: scalar f32,
});

proto_message!(
    /// Per-core DSP comms counters; none are optional.
    DSPCommsDiagnosticsMessage {
    1 => status1: scalar u32,
    2 => status2: scalar u32,
    3 => status3: scalar u32,
    4 => status4: scalar u32,
    5 => process_cpu: scalar f32,
    6 => internal_heap_usage: scalar f32,
    7 => external_heap_usage: scalar f32,
    8 => internal_pm_heap_usage: scalar f32,
    9 => process_cpu_max: scalar f32,
    10 => ext_dma_usage: scalar f32,
    11 => error_flags: scalar u32,
});

proto_message!(
    /// Descriptor names are CamelCase (`USBConnected`, `inReadIdx`); Rust uses
    /// snake_case with the same field numbers.
    SOC2ARMCommsDiagnosticsMessage {
    1 => usb_connected: scalar u32,
    2 => in_read_idx: scalar u32,
    3 => in_write_idx: scalar u32,
    4 => out_read_idx: scalar u32,
    5 => out_write_idx: scalar u32,
    6 => usb_audio_rx_frame: scalar u64,
    7 => usb_audio_tx_frame: scalar u64,
    8 => usb_audio_tx_frame_skipped: scalar u32,
    9 => usb_audio_tx_frame_aborted: scalar u32,
    10 => usb_audio_out_rd_wr_distance_average: scalar u32,
    11 => usb_audio_rd_wr_min_num_samples: scalar u32,
    12 => usb_audio_rd_wr_max_num_samples: scalar u32,
    13 => usb_audio_plus_one_sample_count: scalar u32,
    14 => usb_audio_minus_one_sample_count: scalar u32,
    15 => usb_audio_feedback_state: scalar u32,
    16 => usb_audio_streaming_ep_enabled: scalar u32,
    17 => usb_audio_streaming_ep_disabled: scalar u32,
    18 => usb_midi_out_count: scalar u64,
    19 => usb_midi_in_count: scalar u64,
    20 => usb_hid_in_count: scalar u64,
    21 => usb_hid_out_count: scalar u64,
    22 => usb_hid_in_dropped_count: scalar u32,
    23 => usb_hid_out_dropped_count: scalar u32,
    24 => usb_audio_in_rd_wr_distance_average: scalar u32,
    25 => usb_audio_out_gaps_detected: scalar u32,
    26 => usb_audio_out_gap_resets: scalar u32,
    27 => usb_audio_out_buffer_resets: scalar u32,
});

proto_message!(
    /// Envelope type 7. Descriptor names `SOC1Core1Diagnostics` and so on
    /// are snake_case here.
    DiagnosticsMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => soc1_core1_diagnostics: message DSPCommsDiagnosticsMessage,
    4 => soc1_core2_diagnostics: message DSPCommsDiagnosticsMessage,
    5 => soc2_core1_diagnostics: message DSPCommsDiagnosticsMessage,
    6 => soc2_core2_diagnostics: message DSPCommsDiagnosticsMessage,
    7 => soc2_arm_core_diagnostics: message SOC2ARMCommsDiagnosticsMessage,
    8 => soc1_to_soc2_dropped_count: scalar u64,
});

proto_message!(
    /// `msg` is `messages` in Rust; elements are Preset.proto `MidiMessageInfo`.
    GeneralMIDIMessage {
    1 => source: scalar u32,
    2 => messages: repeated MidiMessageInfo,
});

proto_message!(GeneralMIDIMessages {
    1 => messages: repeated GeneralMIDIMessage,
});

proto_message!(
    /// Envelope type 8.
    MIDISettingsMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => preset_load_messages: message GeneralMIDIMessages,
    4 => general_midi_messages: message GeneralMIDIMessages,
});

proto_message!(GlobalBypassRows {
    1 => row1: scalar bool,
    2 => row2: scalar bool,
    3 => row3: scalar bool,
    4 => row4: scalar bool,
});

proto_message!(MasterVolumeAssignmentOptions {
    1 => out12: scalar bool,
    2 => out34: scalar bool,
    3 => send12: scalar bool,
    4 => headphones: scalar bool,
});

proto_message!(
    /// Descriptor `type` is `folder_type`, the product type: 1 IRs, 2 captures (`0x16c964`
    /// table `0xa0fff8`).
    DownloadDestinationFolder {
    1 => key: scalar String,
    2 => folder_type: scalar i32,
});

proto_message!(StorageInfoEntity {
    1 => entity_type: scalar EnumValue<StorageInfoType>,
    2 => size_maximum: scalar i32,
    3 => size_current: scalar i32,
});

proto_message!(
    /// Sizes are signed int64 in the descriptor.
    StorageInfo {
    1 => total_storage_size_kb: scalar i64,
    2 => free_storage_size_kb: scalar i64,
    3 => storage_entities: repeated StorageInfoEntity,
});

proto_message!(
    /// Envelope type 9. Field 20 is absent from the descriptor. Power,
    /// reset and factory-reset fields are observed only; nothing is executed.
    /// `looper_stomp_assignments` is a packed repeated int32.
    GeneralSettingsMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => reset_wifi_networks: scalar bool,
    4 => screen_brightness: scalar i32,
    5 => led_brightness: scalar i32,
    6 => lock_screen_and_volume_knob: scalar bool,
    7 => global_bypass_cab: message GlobalBypassRows,
    8 => global_bypass_ir: message GlobalBypassRows,
    9 => scene_block_bypass: scalar EnumValue<SceneBlockBypass>,
    10 => midi_over_usb: scalar bool,
    11 => midi_channel: scalar i32,
    12 => ignore_duplicate_pc: scalar bool,
    13 => power_option: scalar EnumValue<PowerOptions>,
    14 => power_button_sensitivity: scalar i32,
    15 => reset_settings: scalar bool,
    16 => factory_reset: scalar bool,
    17 => available_disk_space: scalar f32,
    18 => total_disk_space: scalar f32,
    19 => internal_midi_clock_enabled: scalar bool,
    21 => master_volume_assignment: message MasterVolumeAssignmentOptions,
    22 => stomp_mode_auto_assign: scalar bool,
    23 => swap_tempo_tuner_access: scalar bool,
    24 => midi_clock_out: scalar EnumValue<MidiClockOut>,
    25 => download_destination_folders: repeated DownloadDestinationFolder,
    26 => disable_internet_connection_check: scalar bool,
    27 => enable_dynamic_delay_compensation: scalar bool,
    28 => dimmed_led_brightness: scalar i32,
    29 => enable_preset_dimmed: scalar bool,
    30 => enable_scene_dimmed: scalar bool,
    31 => enable_stomp_dimmed: scalar bool,
    32 => atma_power_mode: scalar EnumValue<AtmaPowerOnMode>,
    33 => looper_stomp_assignments: packed i32,
    34 => midi_clock_in_enabled: scalar bool,
    35 => atma_looper_lock_mode: scalar bool,
    36 => storage_info: message StorageInfo,
    37 => cloud_domain: scalar EnumValue<CloudDomain>,
    38 => gig_view_stomp_access_enabled: scalar bool,
    39 => hold_timing: scalar i32,
    40 => external_midi_clock_tempo: scalar f32,
    41 => external_midi_clock_out_of_range: scalar bool,
});

proto_message!(GainCalParameter {
    1 => index: scalar EnumValue<GainCalibrationOutput>,
    2 => gain_db: scalar f32,
});

proto_message!(GainCalInputPortParameter {
    1 => port_id: scalar EnumValue<GainCalInputPortId>,
    2 => level: scalar f32,
});

proto_message!(GainCalOutputPortParameter {
    1 => port_id: scalar EnumValue<GainCalOutputPortId>,
    2 => level: scalar f32,
});

proto_message!(GainCalHeadphonesFeed {
    1 => level: scalar f32,
    2 => output_port_id: scalar EnumValue<GainCalOutputPortId>,
});

proto_message!(GainCalHeadphonesParameter {
    1 => level: scalar f32,
    2 => feed: message GainCalHeadphonesFeed,
});

proto_message!(
    /// Envelope type 62. Calibration values are observed, never applied.
    GainCalibrationMessage {
    1 => action: scalar EnumValue<MessageAction>,
    2 => request_id: scalar u64,
    3 => gains: repeated GainCalParameter,
    4 => load_input_preset: scalar bool,
    5 => load_output_preset: scalar bool,
    6 => in_port: repeated GainCalInputPortParameter,
    7 => out_port: repeated GainCalOutputPortParameter,
    8 => hp_port: repeated GainCalHeadphonesParameter,
});

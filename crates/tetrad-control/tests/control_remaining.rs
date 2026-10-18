//! Remaining control families: runtime-critical envelopes, packed repeated
//! numerics, raw BinaryPreset bytes, int64, wide/out-of-order field numbers,
//! deep nesting and whole-table selection. Synthetic wire vectors only; no
//! stock payloads, descriptor bytes or device traffic.

use tetrad_control::codecs::{
    AuthorizationToken, AvailableModes, CPUChainLoad, CPUColumnLoad, CPULoadMessage, CloudDomain,
    CloudLoginMessage, CodecError, CodecLimits, ConfirmationAction, ConfirmationMessage,
    ControlMessage, CortexMessageType, EnumValue, GainCalHeadphonesFeed,
    GainCalHeadphonesParameter, GainCalOutputPortId, GainCalParameter, GainCalibrationMessage,
    GainCalibrationOutput, GeneralMIDIMessage, GeneralMIDIMessages, GeneralSettingsMessage,
    IOMeterMessage, LicenseItem, LicenseMessage, LicenseUpdateStatus, MIDISettingsMessage,
    MasterVolumeAssignmentOptions, MessageAction, MidiMessageInfo, ModeMessage, Model, ModelPreset,
    ModelPresetMessage, Param, ParamValue, PowerOptions, ProtoEnum, RawMessageBytes,
    RecallPresetMessage, RecallPresetReason, ResetCommsBuffersMessage, SceneBlockBypass,
    SetTestPresetInputOutputPortsMessage, SetTestPresetSplitMixPointsMessage, StorageInfo,
    StorageInfoEntity, StorageInfoType, UndoRedoMessage, WireValue,
};
use tetrad_control::scene::SceneWireError;
use tetrad_control::{Envelope, Error};

fn limits() -> CodecLimits {
    CodecLimits::default()
}

fn envelope(message_type: u32, payload: Vec<u8>) -> Envelope {
    Envelope {
        message_type,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload,
    }
}

fn check(message_type: u32, wire: &[u8], expected: ControlMessage) {
    let envelope = envelope(message_type, wire.to_vec());
    let decoded = ControlMessage::decode(&envelope, limits()).unwrap();
    assert_eq!(decoded.message, expected, "type {message_type}");
    assert_eq!(
        expected.message_type(),
        CortexMessageType::try_from(message_type).unwrap()
    );
    assert_eq!(
        expected.encode(limits()).unwrap(),
        wire,
        "type {message_type}"
    );
    assert_eq!(decoded.original_payload(), wire);
}

const NEG_ONE: [u8; 10] = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];

/// One hand-encoded vector per runtime-critical family (14, 15, 21, 26, 47,
/// 52, 58): descriptor numbers, wire types, packed lists, nesting and exact
/// re-encoding through `ControlMessage`.
#[test]
fn runtime_critical_vectors_match_descriptor_wire_and_roundtrip() {
    let update = Some(EnumValue::from(MessageAction::Update));
    check(
        14,
        &[
            0x08, 0x01, 0x10, 0x07, 0x18, 0x02, 0x22, 0x04, 0x0a, 0x02, 0x01, 0x02, 0x28, 0x01,
        ],
        ControlMessage::Mode(ModeMessage {
            action: update,
            request_id: Some(7),
            mode: Some(2),
            available_modes: Some(AvailableModes { modes: vec![1, 2] }),
            atma_page: Some(1),
        }),
    );

    let preset = b"\x12\x04Lead\x18\x05\x38\x02"; // BinaryPreset name, hash, default_scene
    let mut recall = vec![0x08, 0x01, 0x1a, preset.len() as u8];
    recall.extend_from_slice(preset);
    recall.extend_from_slice(&[0x20, 0x02]);
    check(
        15,
        &recall,
        ControlMessage::RecallPreset(RecallPresetMessage {
            action: update,
            request_id: None,
            preset: Some(RawMessageBytes {
                bytes: preset.to_vec(),
            }),
            reason: Some(RecallPresetReason::Save.into()),
        }),
    );

    let mut undo = vec![0x08, 0x01, 0x1a, 0x0c, 0x01];
    undo.extend_from_slice(&NEG_ONE); // packed int32 -1 uses the ten-byte form
    undo.extend_from_slice(&[0x03, 0x22, 0x01, 0x02, 0x28, 0x01, 0x30, 0x00]);
    check(
        21,
        &undo,
        ControlMessage::UndoRedo(UndoRedoMessage {
            action: update,
            request_id: None,
            undo_frames: vec![1, -1, 3],
            redo_frames: vec![2],
            undo: Some(true),
            redo: Some(false),
        }),
    );

    check(
        26,
        &[
            0x08, 0x03, 0x1d, 0x00, 0x00, 0x48, 0x42, // READ, total 50.0
            0x22, 0x0d, 0x0a, 0x07, 0x0d, 0x00, 0x00, 0x80, 0x3f, 0x10, 0x01, 0x0a, 0x02, 0x10,
            0x00, // chain: column 1.0/core2, column core1 only
            0x22, 0x00, // empty chain kept
        ],
        ControlMessage::CPULoad(CPULoadMessage {
            action: Some(MessageAction::Read.into()),
            request_id: None,
            cpu_total_load: Some(50.0),
            chains: vec![
                CPUChainLoad {
                    columns: vec![
                        CPUColumnLoad {
                            cpu_load: Some(1.0),
                            is_on_core2: Some(true),
                        },
                        CPUColumnLoad {
                            cpu_load: None,
                            is_on_core2: Some(false),
                        },
                    ],
                },
                CPUChainLoad::default(),
            ],
        }),
    );

    check(
        47,
        b"\x08\x05\x10\xac\x02\x1a\x05Title\x22\x03msg\x2a\x02\x00\x01\x30\x01\x3a\x02OK\x42\x06Cancel",
        ControlMessage::Confirmation(ConfirmationMessage {
            request_id: Some(5),
            confirmation_id: Some(300),
            title: Some("Title".into()),
            msg: Some("msg".into()),
            actions: vec![
                ConfirmationAction::Ok.into(),
                ConfirmationAction::Cancel.into(),
            ],
            response: Some(ConfirmationAction::Cancel.into()),
            ok_title: Some("OK".into()),
            cancel_title: Some("Cancel".into()),
        }),
    );

    let reset = ControlMessage::ResetCommsBuffers(ResetCommsBuffersMessage {
        request_id: Some(9),
        session_id: Some("abc".into()),
    });
    check(52, b"\x08\x09\x12\x03abc", reset.clone());
    // Type 52 keeps the stock decompression bypass that `plain_payload` admits.
    let mut compressed = envelope(52, b"\x08\x09\x12\x03abc".to_vec());
    compressed.decompress = 1;
    assert_eq!(
        ControlMessage::decode(&compressed, limits())
            .unwrap()
            .message,
        reset
    );
    compressed.message_type = 58;
    assert_eq!(
        ControlMessage::decode(&compressed, limits()),
        Err(CodecError::Envelope(Error::UnsupportedCompression))
    );

    let mut license = vec![
        0x08, 0x03, 0x1a, 0x19, 0x0a, 0x02, b'i', b'd', 0x12, 0x01, b'n', 0x18,
    ];
    license.extend_from_slice(&NEG_ONE); // plugin_id u64::MAX
    license.extend_from_slice(b"\x22\x03pro\x2a\x00");
    license.extend_from_slice(&[
        0x20, 0x01, 0x28, 0x00, 0x30, 0x02, 0x3a, 0x03, 0x01, 0x80, 0x01,
    ]);
    check(
        58,
        &license,
        ControlMessage::License(LicenseMessage {
            action: Some(MessageAction::Read.into()),
            request_id: None,
            licenses: vec![LicenseItem {
                id: Some("id".into()),
                name: Some("n".into()),
                plugin_id: Some(u64::MAX),
                license_type: Some("pro".into()),
                expiration_date: Some(String::new()),
            }],
            is_enabled: Some(true),
            is_deactivated: Some(false),
            update_status: Some(LicenseUpdateStatus::Failed.into()),
            new_plugin_ids: vec![1, 128],
            supported_plugin_ids: vec![],
        }),
    );
}

#[test]
fn packed_fields_accept_unpacked_and_mixed_occurrences_and_bound_budgets() {
    let wire = [
        0x18, 0x01, // split_mix_rows unpacked element 1
        0x1a, 0x02, 0x02, 0x03, // packed 2, 3 appends
        0x1d, 0x00, 0x00, 0x00, 0x00, // wrong wire type: retained raw only
        0x22, 0x00, // empty packed split_points: zero elements
        0x32, 0x03, 0x01, 0x00, 0x01, // hard_pan_mixer packed
        0x30, 0x02, // unpacked bool, nonzero is true
    ];
    let decoded = SetTestPresetSplitMixPointsMessage::decode(&wire, limits()).unwrap();
    let message = &decoded.message;
    assert_eq!(message.split_mix_rows, [1, 2, 3]);
    assert_eq!(message.split_points, Vec::<i32>::new());
    assert_eq!(message.hard_pan_mixer, [true, false, true, true]);
    assert_eq!(decoded.fields().len(), 6);
    assert_eq!(decoded.fields()[2].value, WireValue::Fixed32(0));
    // Re-encoding always packs and omits empty lists.
    let bytes = message.encode(limits()).unwrap();
    assert_eq!(
        bytes,
        [
            0x1a, 0x03, 0x01, 0x02, 0x03, 0x32, 0x04, 0x01, 0x00, 0x01, 0x01
        ]
    );
    assert_eq!(
        SetTestPresetSplitMixPointsMessage::decode(&bytes, limits())
            .unwrap()
            .message,
        *message
    );

    // Packed enums stay open; negatives use the ten-byte form inside the body.
    let mut ports = vec![0x1a, 0x0c, 0x01, 0x63];
    ports.extend_from_slice(&NEG_ONE);
    let decoded = SetTestPresetInputOutputPortsMessage::decode(&ports, limits()).unwrap();
    assert_eq!(decoded.message.input_ports.len(), 3);
    assert_eq!(decoded.message.input_ports[0].number(), 1);
    assert_eq!(decoded.message.input_ports[1], EnumValue::Unknown(99));
    assert_eq!(decoded.message.input_ports[2], EnumValue::Unknown(-1));
    assert_eq!(decoded.message.encode(limits()).unwrap(), ports);

    // Errors inside a packed body report the offset inside that body.
    assert_eq!(
        SetTestPresetSplitMixPointsMessage::decode(&[0x1a, 0x01, 0x80], limits()),
        Err(CodecError::Wire {
            offset: 1,
            problem: SceneWireError::Truncated
        })
    );
    assert_eq!(
        SetTestPresetSplitMixPointsMessage::decode(
            &[0x1a, 0x05, 0x80, 0x80, 0x80, 0x80, 0x10],
            limits()
        ),
        Err(CodecError::Integer {
            field: 3,
            value: 1 << 32
        })
    );
    // Each packed element costs one field unit on both paths: key + 3 = 4.
    let three = [0x1a, 0x03, 0x01, 0x02, 0x03];
    let few = CodecLimits {
        max_fields: 3,
        ..limits()
    };
    assert_eq!(
        SetTestPresetSplitMixPointsMessage::decode(&three, few),
        Err(CodecError::FieldLimit(3))
    );
    let message = SetTestPresetSplitMixPointsMessage {
        split_mix_rows: vec![1, 2, 3],
        ..Default::default()
    };
    assert_eq!(message.encode(few), Err(CodecError::FieldLimit(3)));
    let exact = CodecLimits {
        max_fields: 4,
        ..limits()
    };
    assert_eq!(
        SetTestPresetSplitMixPointsMessage::decode(&three, exact)
            .unwrap()
            .message,
        message
    );
    assert_eq!(message.encode(exact).unwrap(), three);
}

#[test]
fn raw_preset_bytes_concatenate_validate_top_level_and_hand_off_to_tetrad_preset() {
    // Two `preset` occurrences merge by concatenation, as protobuf requires.
    let wire = b"\x1a\x06\x12\x04Lead\x1a\x02\x18\x05";
    let decoded = RecallPresetMessage::decode(wire, limits()).unwrap();
    let preset = decoded.message.preset.as_ref().unwrap();
    assert_eq!(preset.bytes, b"\x12\x04Lead\x18\x05");
    assert_eq!(decoded.fields().len(), 2);
    assert_eq!(format!("{preset:?}"), "RawMessageBytes(8 bytes)");
    let bytes = decoded.message.encode(limits()).unwrap();
    assert_eq!(bytes, b"\x1a\x08\x12\x04Lead\x18\x05");
    // The owner crate parses the retained bytes; this crate never loads them.
    let document =
        tetrad_preset::PresetDocument::parse(&preset.bytes, tetrad_preset::Limits::default())
            .unwrap();
    let summary = document.summary().unwrap();
    assert_eq!(summary.name.as_deref(), Some("Lead"));
    assert_eq!(summary.hash, Some(5));

    // Top-level wire validity and the nesting budget still apply to raw bytes.
    assert_eq!(
        RecallPresetMessage::decode(&[0x1a, 0x02, 0x0f, 0x00], limits()),
        Err(CodecError::Wire {
            offset: 1,
            problem: SceneWireError::WireType(7)
        })
    );
    let flat = CodecLimits {
        max_depth: 0,
        ..limits()
    };
    assert_eq!(
        RecallPresetMessage::decode(wire, flat),
        Err(CodecError::DepthLimit(0))
    );
    assert_eq!(
        RecallPresetMessage::decode(&[0x1a, 0x00], limits())
            .unwrap()
            .message
            .preset,
        Some(RawMessageBytes::default())
    );
}

#[test]
fn cloud_login_token_debug_redacts_and_duplicate_tokens_merge() {
    let wire = b"\x1a\x02me\x22\x0a\x0a\x08secret-a\x22\x0d\x12\x08secret-r\x18\xd0\x0f";
    let decoded = CloudLoginMessage::decode(wire, limits()).unwrap();
    let message = &decoded.message;
    assert_eq!(message.username.as_deref(), Some("me"));
    assert_eq!(
        message.token,
        Some(AuthorizationToken {
            access_token: Some("secret-a".into()),
            refresh_token: Some("secret-r".into()),
            expires_in: Some(2000),
        })
    );
    let shown = format!("{message:?}");
    assert!(!shown.contains("secret"), "{shown}");
    assert!(shown.contains("access_token_len: Some(8)"), "{shown}");
    assert_eq!(
        message.encode(limits()).unwrap(),
        b"\x1a\x02me\x22\x17\x0a\x08secret-a\x12\x08secret-r\x18\xd0\x0f"
    );
}

#[test]
fn wide_and_out_of_order_field_numbers_use_multibyte_keys() {
    let mut wire = vec![0x08, 0x01, 0x48, 0x02, 0x68, 0x01];
    wire.extend_from_slice(&[0xa0, 0x01, 0x05]); // field 20: absent from descriptor
    wire.extend_from_slice(&[0xaa, 0x01, 0x02, 0x08, 0x01]); // 21 master_volume_assignment
    wire.extend_from_slice(&[0x8a, 0x02, 0x03, 0x00, 0x01, 0x02]); // 33 packed int32
    wire.extend_from_slice(&[0xa2, 0x02, 0x17, 0x08]); // 36 storage_info
    wire.extend_from_slice(&NEG_ONE); // total_storage_size_kb int64 -1
    wire.extend_from_slice(&[
        0x10, 0x80, 0x80, 0x01, 0x1a, 0x06, 0x08, 0x01, 0x10, 0x0a, 0x18, 0x05,
    ]);
    wire.extend_from_slice(&[0xa8, 0x02, 0x01]); // 37 cloud_domain
    wire.extend_from_slice(&[0xc5, 0x02, 0x00, 0x00, 0xf0, 0x42]); // 40 tempo 120.0
    wire.extend_from_slice(&[0xc8, 0x02, 0x01]); // 41 out_of_range
    let decoded = GeneralSettingsMessage::decode(&wire, limits()).unwrap();
    let expected = GeneralSettingsMessage {
        action: Some(MessageAction::Update.into()),
        scene_block_bypass: Some(SceneBlockBypass::NeverOverwrite.into()),
        power_option: Some(PowerOptions::Reboot.into()),
        master_volume_assignment: Some(MasterVolumeAssignmentOptions {
            out12: Some(true),
            ..Default::default()
        }),
        looper_stomp_assignments: vec![0, 1, 2],
        storage_info: Some(StorageInfo {
            total_storage_size_kb: Some(-1),
            free_storage_size_kb: Some(16384),
            storage_entities: vec![StorageInfoEntity {
                entity_type: Some(StorageInfoType::NeuralCaptures.into()),
                size_maximum: Some(10),
                size_current: Some(5),
            }],
        }),
        cloud_domain: Some(CloudDomain::AlternativeDomain.into()),
        external_midi_clock_tempo: Some(120.0),
        external_midi_clock_out_of_range: Some(true),
        ..Default::default()
    };
    assert_eq!(decoded.message, expected);
    assert_eq!(decoded.fields().len(), 10);
    assert_eq!(decoded.fields()[3].number, 20);
    // The unknown field survives only in the original bytes.
    let bytes = decoded.message.encode(limits()).unwrap();
    assert_ne!(bytes, wire);
    assert_eq!(
        GeneralSettingsMessage::decode(&bytes, limits())
            .unwrap()
            .message,
        expected
    );

    let meter = [
        0x1d, 0x00, 0x00, 0x80, 0xbf, 0x98, 0x01, 0x01, 0xcd, 0x02, 0x00, 0x00, 0x80, 0x3f,
    ];
    check(
        5,
        &meter,
        ControlMessage::IOMeter(IOMeterMessage {
            input_1: Some(-1.0),
            hp_limiter_active: Some(true),
            grid_send_2: Some(1.0),
            ..Default::default()
        }),
    );
}

#[test]
fn nested_families_merge_duplicates_and_deepest_chain_hits_depth_limit() {
    // MIDISettings -> GeneralMIDIMessages -> GeneralMIDIMessage -> MidiMessageInfo.
    let wire = [
        0x1a, 0x0b, 0x0a, 0x09, 0x08, 0x01, 0x12, 0x05, 0x08, 0xb0, 0x01, 0x10, 0x01, // first
        0x1a, 0x04, 0x0a, 0x02, 0x08, 0x02, // duplicate occurrence appends a message
    ];
    let decoded = MIDISettingsMessage::decode(&wire, limits()).unwrap();
    assert_eq!(
        decoded.message.preset_load_messages,
        Some(GeneralMIDIMessages {
            messages: vec![
                GeneralMIDIMessage {
                    source: Some(1),
                    messages: vec![MidiMessageInfo {
                        message_type: Some(176),
                        channel: Some(1),
                        ..Default::default()
                    }],
                },
                GeneralMIDIMessage {
                    source: Some(2),
                    messages: vec![],
                },
            ],
        })
    );
    assert_eq!(
        MIDISettingsMessage::decode(
            &wire,
            CodecLimits {
                max_depth: 2,
                ..limits()
            }
        ),
        Err(CodecError::DepthLimit(2))
    );

    let calibration = [
        0x1a, 0x07, 0x08, 0x07, 0x15, 0x00, 0x00, 0xc0, 0xc0, // HP_R_GAIN -6 dB
        0x42, 0x0e, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x12, 0x07, 0x0d, 0x00, 0x00, 0x80, 0x3f, 0x10,
        0x04, // hp_port level 0.0, feed level 1.0 -> XLR_1
    ];
    check(
        62,
        &calibration,
        ControlMessage::GainCalibration(GainCalibrationMessage {
            gains: vec![GainCalParameter {
                index: Some(GainCalibrationOutput::HpRGain.into()),
                gain_db: Some(-6.0),
            }],
            hp_port: vec![GainCalHeadphonesParameter {
                level: Some(0.0),
                feed: Some(GainCalHeadphonesFeed {
                    level: Some(1.0),
                    output_port_id: Some(GainCalOutputPortId::Xlr1.into()),
                }),
            }],
            ..Default::default()
        }),
    );

    // ModelPreset -> ModelPreset -> Model -> Param -> ParamValue scans at depth 4.
    let deep = ModelPresetMessage {
        presets: vec![ModelPreset {
            model: Some(Model {
                hash: Some(0xdead),
                params: vec![Param {
                    index: Some(0),
                    param_values: vec![ParamValue::float(0.5)],
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    let bytes = deep.encode(limits()).unwrap();
    for depth in [3, 4] {
        let shallow = CodecLimits {
            max_depth: depth,
            ..limits()
        };
        let result = ModelPresetMessage::decode(&bytes, shallow).map(|decoded| decoded.message);
        if depth == 3 {
            assert_eq!(result, Err(CodecError::DepthLimit(3)));
            assert_eq!(deep.encode(shallow), Err(CodecError::DepthLimit(3)));
        } else {
            assert_eq!(result.unwrap(), deep);
        }
    }
}

/// Every descriptor type except Undefined, Scene and NumberOfMessageTypes
/// selects a codec; an empty payload is the default message and re-encodes
/// to zero bytes.
#[test]
fn whole_type_table_selects_codecs_and_refuses_the_three_schemaless_types() {
    for number in 0u32..=74 {
        let envelope = envelope(number, vec![]);
        let result = ControlMessage::decode(&envelope, limits());
        if matches!(number, 0 | 13 | 73 | 74) {
            assert_eq!(result, Err(CodecError::UnsupportedMessageType(number)));
            continue;
        }
        let decoded = result.unwrap_or_else(|error| panic!("type {number}: {error}"));
        assert_eq!(decoded.message.message_type().number() as u32, number);
        assert_eq!(decoded.message.encode(limits()).unwrap(), b"");
    }
}

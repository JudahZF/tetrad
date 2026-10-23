use tetrad_control::codecs::{
    CodecError, CodecLimits, ConnectionMessage, ControlMessage, CortexMessageType, EnumValue,
    KeepAliveMessage, LooperMessage, LooperStatus, LooperUpdateType, MessageAction, ProtoEnum,
    ShowTunerMessage, SuspendConnectionMessage, SuspendReason, TunerMessage, WireValue,
};
use tetrad_control::scene::SceneWireError;
use tetrad_control::{Envelope, Error, Limits, Reassembler};

#[test]
fn complete_type_table_and_open_enums_are_explicit() {
    for number in 0u32..=73 {
        assert_eq!(
            CortexMessageType::try_from(number).unwrap().number() as u32,
            number
        );
    }
    for number in [74, u32::MAX] {
        assert_eq!(CortexMessageType::try_from(number), Err(number));
    }
    assert_eq!(
        CortexMessageType::try_from(0u32),
        Ok(CortexMessageType::Undefined)
    );
    assert_eq!(
        CortexMessageType::try_from(73u32),
        Ok(CortexMessageType::NumberOfMessageTypes)
    );
    for number in 0..=8 {
        assert_eq!(MessageAction::from_number(number).number(), number);
    }
    assert_eq!(MessageAction::from_number(9), EnumValue::Unknown(9));
    assert_eq!(MessageAction::from_number(-1), EnumValue::Unknown(-1));
    assert_eq!(LooperUpdateType::from_number(2), EnumValue::Unknown(2));
    assert_eq!(SuspendReason::from_number(3), EnumValue::Unknown(3));
}

#[test]
fn all_payloads_roundtrip_through_existing_report_transport() {
    let messages = [
        ControlMessage::Tuner(TunerMessage {
            action: Some(MessageAction::Update.into()),
            request_id: Some(u64::MAX),
            input_port_id: Some(-1),
            frequency: Some(-0.0),
            mute: Some(false),
            enable_meter: Some(true),
            meter: Some(1.25),
        }),
        ControlMessage::ShowTuner(ShowTunerMessage {
            action: Some(MessageAction::Create.into()),
            request_id: Some(0),
            show: Some(false),
        }),
        ControlMessage::Looper(LooperMessage {
            action: Some(MessageAction::Read.into()),
            request_id: Some(42),
            status: Some(LooperStatus {
                state: Some(0),
                ..Default::default()
            }),
            state: Some(u32::MAX),
            one_shot_play: Some(false),
            sync_start_waiting: Some(true),
            quantize_enabled: Some(false),
            update_type: Some(LooperUpdateType::Meter.into()),
        }),
        ControlMessage::KeepAlive(KeepAliveMessage {
            action: Some(MessageAction::Read.into()),
            request_id: Some(0),
            is_online: Some(false),
            timeout: Some(0.0),
        }),
        ControlMessage::Connection(ConnectionMessage {
            request_id: Some(u64::MAX),
            connected: Some(false),
        }),
        ControlMessage::SuspendConnection(SuspendConnectionMessage {
            request_id: Some(0),
            suspend: Some(false),
            reason: Some(SuspendReason::Backup.into()),
        }),
    ];
    for message in messages {
        let envelope = Envelope {
            message_type: message.message_type().number() as u32,
            transform: 0,
            decompress: 0,
            opaque: [0xa5, 0x5a],
            payload: message.encode(CodecLimits::default()).unwrap(),
        };
        let reports = envelope.reports(Limits::default()).unwrap();
        let mut reassembler = Reassembler::new(Limits::default());
        let mut restored = None;
        for report in &reports {
            restored = reassembler.push(report).unwrap();
        }
        let restored = restored.unwrap();
        reassembler.finish().unwrap();
        let decoded = ControlMessage::decode(&restored, CodecLimits::default()).unwrap();
        assert_eq!(decoded.message, message);
        assert_eq!(decoded.original_payload(), envelope.payload);
        assert_eq!(restored.reports(Limits::default()).unwrap(), reports);
    }
}

#[test]
fn absent_and_explicit_defaults_have_distinct_outbound_encodings() {
    let limits = CodecLimits::default();
    assert_eq!(
        TunerMessage::decode(&[], limits).unwrap().message,
        TunerMessage::default()
    );
    assert_eq!(
        ShowTunerMessage::default().encode(limits).unwrap(),
        [0_u8; 0]
    );
    assert_eq!(
        KeepAliveMessage::default().encode(limits).unwrap(),
        [0_u8; 0]
    );
    assert_eq!(
        ConnectionMessage::default().encode(limits).unwrap(),
        [0_u8; 0]
    );
    assert_eq!(
        SuspendConnectionMessage::default().encode(limits).unwrap(),
        [0_u8; 0]
    );
    assert_eq!(LooperMessage::default().encode(limits).unwrap(), [0_u8; 0]);
    let show = ShowTunerMessage {
        action: Some(MessageAction::Create.into()),
        request_id: Some(0),
        show: Some(false),
    };
    assert_eq!(show.encode(limits).unwrap(), [8, 0, 16, 0, 24, 0]);
    let keep_alive = KeepAliveMessage {
        is_online: Some(false),
        timeout: Some(0.0),
        ..Default::default()
    };
    assert_eq!(keep_alive.encode(limits).unwrap(), [24, 0, 37, 0, 0, 0, 0]);
    let connection = ConnectionMessage {
        request_id: Some(0),
        connected: Some(false),
    };
    assert_eq!(connection.encode(limits).unwrap(), [8, 0, 16, 0]);
    let suspend = SuspendConnectionMessage {
        request_id: Some(0),
        suspend: Some(false),
        reason: Some(SuspendReason::Backup.into()),
    };
    assert_eq!(suspend.encode(limits).unwrap(), [8, 0, 16, 0, 24, 0]);
    let looper = LooperMessage {
        status: Some(LooperStatus::default()),
        ..Default::default()
    };
    assert_eq!(looper.encode(limits).unwrap(), [26, 0]);
    assert_eq!(
        LooperMessage::decode(&[26, 0], limits)
            .unwrap()
            .message
            .status,
        Some(LooperStatus::default())
    );
}

#[test]
fn tuner_field_numbers_signed_int32_and_float_bits_match_synthetic_wire() {
    let limits = CodecLimits::default();
    let mut payload = vec![8, 1, 16, 42, 24];
    payload.extend_from_slice(&[0xff; 9]);
    payload.push(1);
    payload.push(37);
    payload.extend_from_slice(&0x8000_0000u32.to_le_bytes());
    payload.extend_from_slice(&[40, 0, 48, 1, 61]);
    payload.extend_from_slice(&0x7fc0_1234u32.to_le_bytes());
    let decoded = TunerMessage::decode(&payload, limits).unwrap();
    assert_eq!(decoded.message.action, Some(MessageAction::Update.into()));
    assert_eq!(decoded.message.request_id, Some(42));
    assert_eq!(decoded.message.input_port_id, Some(-1));
    assert_eq!(decoded.message.frequency.unwrap().to_bits(), 0x8000_0000);
    assert_eq!(decoded.message.mute, Some(false));
    assert_eq!(decoded.message.enable_meter, Some(true));
    assert_eq!(decoded.message.meter.unwrap().to_bits(), 0x7fc0_1234);
    assert_eq!(decoded.message.encode(limits).unwrap(), payload);
    // Protobuf int32 also accepts the low-32-bit, five-byte negative form.
    let short_negative = [24, 0xff, 0xff, 0xff, 0xff, 0x0f];
    assert_eq!(
        TunerMessage::decode(&short_negative, limits)
            .unwrap()
            .message
            .input_port_id,
        Some(-1)
    );
}

#[test]
fn wire_observations_keep_duplicates_unknowns_groups_and_wrong_wire_types() {
    let payload = [
        8, 1, 8, 0x83, 0, // last action READ, overlong varint retained
        40, 2, 40, 0, // last bool false, noncanonical true retained
        0x3a, 1, 9, // meter with wrong wire type
        0x4b, 24, 1, 0x4c, // unknown group: does not supply input_port_id
        0x51, 1, 0, 0, 0, 0, 0, 0, 0, // unknown fixed64
        0x5d, 4, 3, 2, 1, // unknown fixed32
    ];
    let decoded = TunerMessage::decode(&payload, CodecLimits::default()).unwrap();
    assert_eq!(decoded.original_payload(), payload);
    assert_eq!(
        decoded
            .fields()
            .iter()
            .flat_map(|field| field.raw.iter().copied())
            .collect::<Vec<_>>(),
        payload
    );
    assert_eq!(decoded.fields().len(), 8);
    assert_eq!(decoded.fields()[5].value, WireValue::Group);
    assert_eq!(decoded.fields()[6].value, WireValue::Fixed64(1));
    assert_eq!(decoded.message.action, Some(MessageAction::Read.into()));
    assert_eq!(decoded.message.mute, Some(false));
    assert_eq!(decoded.message.meter, None);
    assert_eq!(decoded.message.input_port_id, None);
    // New typed output is explicitly separate from exact replay.
    assert_eq!(
        decoded.message.encode(CodecLimits::default()).unwrap(),
        [8, 3, 40, 0]
    );
}

#[test]
fn duplicate_looper_status_messages_merge_including_explicit_zero() {
    let payload = [26, 4, 8, 9, 32, 2, 26, 5, 8, 0, 0x98, 1, 7, 26, 0];
    let decoded = LooperMessage::decode(&payload, CodecLimits::default()).unwrap();
    let status = decoded.message.status.as_ref().unwrap();
    assert_eq!(status.state, Some(0));
    assert_eq!(status.duplicate_cycle, Some(2));
    assert_eq!(status.events_waiting_for_quantize, Some(7));
    assert_eq!(decoded.original_payload(), payload);
    let canonical = [26, 7, 8, 0, 32, 2, 0x98, 1, 7];
    assert_eq!(
        decoded.message.encode(CodecLimits::default()).unwrap(),
        canonical
    );
}

#[test]
fn looper_status_all_22_fields_use_descriptor_types_not_flag_guesses() {
    let mut payload = Vec::new();
    for number in 1u8..=22 {
        let float = matches!(number, 2 | 3 | 17 | 18);
        let key = u32::from(number) * 8 + if float { 5 } else { 0 };
        if key >= 128 {
            payload.extend_from_slice(&[(key as u8 & 0x7f) | 0x80, 1]);
        } else {
            payload.push(key as u8);
        }
        if float {
            payload.extend_from_slice(&(f32::from(number) + 0.5).to_le_bytes());
        } else {
            payload.push(number);
        }
    }
    let status = LooperStatus::decode(&payload, CodecLimits::default())
        .unwrap()
        .message;
    assert_eq!(
        status,
        LooperStatus {
            state: Some(1),
            progress: Some(2.5),
            undo_progress: Some(3.5),
            duplicate_cycle: Some(4),
            num_duplicate_cycles: Some(5),
            one_shot_stopped: Some(6),
            redo_available: Some(7),
            loop_length: Some(8),
            free_samples: Some(9),
            in_reverse: Some(10),
            one_shot: Some(11),
            half_speed: Some(12),
            fixed_duplicate_cycles: Some(13),
            armed: Some(14),
            waiting_for_cycle: Some(15),
            undo_count: Some(16),
            max_write_displacement: Some(17.5),
            min_write_displacement: Some(18.5),
            events_waiting_for_quantize: Some(19),
            current_clock: Some(20),
            transition: Some(21),
            action: Some(22),
        }
    );
    assert_eq!(status.encode(CodecLimits::default()).unwrap(), payload);
}

#[test]
fn unknown_enum_values_remain_numeric_on_decode_and_encode() {
    let limits = CodecLimits::default();
    let positive = ShowTunerMessage::decode(&[8, 99], limits).unwrap().message;
    assert_eq!(positive.action, Some(EnumValue::Unknown(99)));
    assert_eq!(positive.encode(limits).unwrap(), [8, 99]);
    let mut negative = vec![24];
    negative.extend_from_slice(&[0xff; 9]);
    negative.push(1);
    let suspend = SuspendConnectionMessage::decode(&negative, limits)
        .unwrap()
        .message;
    assert_eq!(suspend.reason, Some(EnumValue::Unknown(-1)));
    assert_eq!(suspend.encode(limits).unwrap(), negative);
    let looper = LooperMessage::decode(&[64, 99], limits).unwrap().message;
    assert_eq!(looper.update_type, Some(EnumValue::Unknown(99)));
    assert_eq!(looper.encode(limits).unwrap(), [64, 99]);
}

#[test]
fn malformed_fields_and_uint32_overflow_are_rejected() {
    let malformed: &[(&[u8], SceneWireError)] = &[
        (&[0], SceneWireError::FieldNumber),
        (&[14], SceneWireError::WireType(6)),
        (&[8], SceneWireError::Truncated),
        (&[0x80], SceneWireError::Truncated),
        (&[37, 0, 0, 0], SceneWireError::Truncated),
        (&[0x51, 0], SceneWireError::Truncated),
        (&[0x4a, 3, 1], SceneWireError::Truncated),
        (&[0x4c], SceneWireError::UnexpectedEndGroup),
        (&[0x4b, 0x54], SceneWireError::MismatchedEndGroup),
        (&[0x4b], SceneWireError::Truncated),
        (
            &[8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 2],
            SceneWireError::VarintOverflow,
        ),
        (&[0x80, 0x80, 0x80, 0x80, 0x10], SceneWireError::FieldNumber),
    ];
    for (payload, expected) in malformed {
        assert!(
            matches!(TunerMessage::decode(payload, CodecLimits::default()), Err(CodecError::Wire { problem, .. }) if problem == *expected),
            "{payload:?}"
        );
    }
    assert!(matches!(
        LooperMessage::decode(&[26, 1, 8], CodecLimits::default()),
        Err(CodecError::Wire {
            problem: SceneWireError::Truncated,
            ..
        })
    ));
    assert_eq!(
        LooperMessage::decode(&[32, 0x80, 0x80, 0x80, 0x80, 0x10], CodecLimits::default()),
        Err(CodecError::Integer {
            field: 4,
            value: 1u64 << 32
        })
    );
}

#[test]
fn byte_field_and_combined_nesting_limits_apply_to_read_and_write() {
    let show = ShowTunerMessage {
        action: Some(MessageAction::Read.into()),
        request_id: Some(42),
        show: Some(false),
    };
    let bytes = [8, 3, 16, 42, 24, 0];
    let limits = CodecLimits {
        max_payload_bytes: 5,
        ..Default::default()
    };
    assert_eq!(show.encode(limits), Err(CodecError::PayloadLimit(5)));
    assert_eq!(
        ShowTunerMessage::decode(&bytes, limits),
        Err(CodecError::PayloadLimit(5))
    );
    let limits = CodecLimits {
        max_fields: 2,
        ..Default::default()
    };
    assert_eq!(show.encode(limits), Err(CodecError::FieldLimit(2)));
    assert_eq!(
        ShowTunerMessage::decode(&bytes, limits),
        Err(CodecError::FieldLimit(2))
    );
    let nested = LooperMessage {
        status: Some(LooperStatus {
            state: Some(0),
            ..Default::default()
        }),
        ..Default::default()
    };
    let limits = CodecLimits {
        max_fields: 1,
        ..Default::default()
    };
    assert_eq!(nested.encode(limits), Err(CodecError::FieldLimit(1)));
    assert_eq!(
        LooperMessage::decode(&[26, 2, 8, 0], limits),
        Err(CodecError::FieldLimit(1))
    );
    let limits = CodecLimits {
        max_depth: 0,
        ..Default::default()
    };
    assert_eq!(nested.encode(limits), Err(CodecError::DepthLimit(0)));
    assert_eq!(
        LooperMessage::decode(&[26, 0], limits),
        Err(CodecError::DepthLimit(0))
    );
    let limits = CodecLimits {
        max_depth: 65,
        ..Default::default()
    };
    assert_eq!(
        ShowTunerMessage::decode(&[], limits),
        Err(CodecError::DepthConfiguration(65))
    );
    assert_eq!(show.encode(limits), Err(CodecError::DepthConfiguration(65)));
    // Known status nesting + unknown group nesting share one depth budget.
    let payload = [26, 2, 0x4b, 0x4c];
    let limits = CodecLimits {
        max_depth: 1,
        ..Default::default()
    };
    assert_eq!(
        LooperMessage::decode(&payload, limits),
        Err(CodecError::DepthLimit(1))
    );
    let limits = CodecLimits {
        max_depth: 2,
        max_fields: 3,
        ..Default::default()
    };
    LooperMessage::decode(&payload, limits).unwrap();
    let limits = CodecLimits {
        max_fields: 2,
        ..limits
    };
    assert_eq!(
        LooperMessage::decode(&payload, limits),
        Err(CodecError::FieldLimit(2))
    );
    // Duplicate nested messages cannot each reset the field budget.
    let payload = [26, 2, 8, 1, 26, 2, 8, 2];
    let limits = CodecLimits {
        max_fields: 3,
        ..Default::default()
    };
    assert_eq!(
        LooperMessage::decode(&payload, limits),
        Err(CodecError::FieldLimit(3))
    );
}

#[test]
fn typed_selection_refuses_unsupported_types_and_preserves_envelope_guards() {
    let mut envelope = Envelope {
        message_type: 6,
        transform: 1,
        decompress: 0,
        opaque: [0; 2],
        payload: vec![],
    };
    assert_eq!(
        ControlMessage::decode(&envelope, CodecLimits::default()),
        Err(CodecError::Envelope(Error::UnsupportedTransform(1)))
    );
    envelope.transform = 0;
    envelope.decompress = 1;
    assert_eq!(
        ControlMessage::decode(&envelope, CodecLimits::default()),
        Err(CodecError::Envelope(Error::UnsupportedCompression))
    );
    envelope.decompress = 0;
    // Type 52 gained a codec in the remaining-families slice; see control_remaining.rs.
    for message_type in [0, 13, 73, 74, u32::MAX] {
        envelope.message_type = message_type;
        assert_eq!(
            ControlMessage::decode(&envelope, CodecLimits::default()),
            Err(CodecError::UnsupportedMessageType(message_type))
        );
    }
}

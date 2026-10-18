//! GlobalTempo (33), MetronomeStatusUpdate and the shared Param/ParamValue
//! codecs. Synthetic wire vectors only; no stock payloads, descriptor bytes or
//! device traffic.

use tetrad_control::codecs::{
    CodecError, CodecLimits, ControlMessage, CortexMessageType, GlobalTempoMessage, MessageAction,
    MetronomeStatusUpdate, Param, ParamValue, ParamValueKind,
};
use tetrad_control::{Envelope, Error};

fn limits() -> CodecLimits {
    CodecLimits::default()
}

/// Hand-encoded GlobalTempoMessage with every Param and MetronomeStatusUpdate
/// field number and wire type from the two embedded descriptors.
#[test]
fn global_tempo_wire_vector_matches_descriptor_numbers_and_types() {
    let mut param = vec![0x08, 0x02]; // 1 expression = 2
    param.extend_from_slice(&[0x15, 0x00, 0x00, 0x80, 0xbf]); // 2 expression_min -1.0
    param.extend_from_slice(&[0x1d, 0x00, 0x00, 0x00, 0x40]); // 3 expression_max 2.0
    param.extend_from_slice(&[0x20, 0x01]); // 4 scene_mode true
    param.extend_from_slice(&[0x2a, 0x05, 0x15, 0x00, 0x00, 0xf0, 0x42]); // 5 float 120.0
    param.extend_from_slice(&[0x2a, 0x02, 0x08, 0x04]); // 5 int 4
    param.extend_from_slice(&[0x30, 0x03]); // 6 index 3
    param.extend_from_slice(&[0x38, 0x00]); // 7 is_led false
    param.extend_from_slice(b"\x42\x03" /* 8 */);
    param.extend_from_slice(b"1/4");
    param.extend_from_slice(b"\x42\x00"); // 8 empty string kept
    param.extend_from_slice(b"\x4a\x04" /* 9 */);
    param.extend_from_slice(b"icon");
    param.extend_from_slice(b"\x52\x02" /* 10 */);
    param.extend_from_slice(b"md");

    let mut status = vec![0x08, 0x01, 0x10, 0x00, 0x18, 0x02, 0x20, 0x05, 0x28, 0x7f];
    status.extend_from_slice(&[
        0x30, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
    ]); // 6 = -1
    status.extend_from_slice(&[0x38, 0x02]); // 7 = 2

    let mut wire = vec![0x08, 0x01, 0x10, 0x07]; // action UPDATE, request_id 7
    wire.push(0x1a);
    wire.push(param.len() as u8);
    wire.extend_from_slice(&param);
    wire.push(0x22);
    wire.push(status.len() as u8);
    wire.extend_from_slice(&status);

    let decoded = GlobalTempoMessage::decode(&wire, limits()).unwrap();
    let message = &decoded.message;
    assert_eq!(message.action, Some(MessageAction::Update.into()));
    assert_eq!(message.request_id, Some(7));
    assert_eq!(message.params.len(), 1);
    let param = &message.params[0];
    assert_eq!(param.expression, Some(2));
    assert_eq!(param.expression_min, Some(-1.0));
    assert_eq!(param.expression_max, Some(2.0));
    assert_eq!(param.scene_mode, Some(true));
    assert_eq!(
        param.param_values,
        vec![ParamValue::float(120.0), ParamValue::int(4)]
    );
    assert_eq!(param.index, Some(3));
    assert_eq!(param.is_led, Some(false));
    assert_eq!(param.dynamic_steps, vec!["1/4".to_owned(), String::new()]);
    assert_eq!(param.dynamic_icons, vec!["icon".to_owned()]);
    assert_eq!(param.dynamic_metadata, vec!["md".to_owned()]);
    assert_eq!(
        message.metronome_status,
        Some(MetronomeStatusUpdate {
            is_enabled: Some(1),
            preroll_enabled: Some(0),
            current_beat: Some(2),
            current_bar: Some(5),
            current_tick: Some(127),
            count_in_beats_remaining: Some(-1),
            count_in_bars_remaining: Some(2),
        })
    );
    assert_eq!(message.first_indexed_float(), Some((3, 120.0)));
    assert_eq!(decoded.original_payload(), wire.as_slice());
    assert_eq!(message.encode(limits()).unwrap(), wire);

    assert_eq!(
        GlobalTempoMessage::decode(&[], limits()).unwrap().message,
        GlobalTempoMessage::default()
    );
    assert_eq!(GlobalTempoMessage::default().encode(limits()).unwrap(), b"");
}

#[test]
fn param_value_oneof_keeps_last_case_and_clears_earlier_cases() {
    let int_then_float = [0x08, 0x05, 0x15, 0x00, 0x00, 0x80, 0x3f];
    assert_eq!(
        ParamValue::decode(&int_then_float, limits())
            .unwrap()
            .message,
        ParamValue::float(1.0)
    );
    let float_then_int = [0x15, 0x00, 0x00, 0x80, 0x3f, 0x08, 0x05];
    assert_eq!(
        ParamValue::decode(&float_then_int, limits())
            .unwrap()
            .message,
        ParamValue::int(5)
    );
    let int_then_string = b"\x08\x05\x1a\x03abc";
    let decoded = ParamValue::decode(int_then_string, limits()).unwrap();
    assert_eq!(decoded.message, ParamValue::string("abc"));
    assert_eq!(decoded.message.as_str(), Some("abc"));
    assert_eq!(decoded.message.as_int(), None);
    assert_eq!(decoded.message.as_float(), None);
    assert_eq!(decoded.fields().len(), 2);
    // Wrong wire type for a oneof member is retained raw and does not clear.
    let int_then_bad_float = [0x08, 0x05, 0x10, 0x01];
    let decoded = ParamValue::decode(&int_then_bad_float, limits()).unwrap();
    assert_eq!(decoded.message, ParamValue::int(5));
    assert_eq!(decoded.fields().len(), 2);
    assert_eq!(
        ParamValue::decode(b"\x1a\x01\xff", limits()),
        Err(CodecError::Utf8(3))
    );

    let empty = ParamValue::decode(&[], limits()).unwrap().message;
    assert_eq!(empty, ParamValue { value: None });
    assert_eq!(empty.encode(limits()).unwrap(), b"");
    let holder = Param {
        param_values: vec![ParamValue::default(), ParamValue::int(-1)],
        ..Default::default()
    };
    let bytes = holder.encode(limits()).unwrap();
    assert_eq!(
        bytes,
        [
            0x2a, 0x00, 0x2a, 0x0b, 0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0x01
        ]
    );
    assert_eq!(Param::decode(&bytes, limits()).unwrap().message, holder);
}

#[test]
fn repeated_params_and_strings_append_and_status_duplicates_merge() {
    let mut wire = Vec::new();
    wire.extend_from_slice(&[0x1a, 0x02, 0x30, 0x01]); // params[0] index 1
    wire.extend_from_slice(&[0x22, 0x02, 0x08, 0x01]); // status is_enabled 1
    wire.extend_from_slice(b"\x1a\x06\x42\x01b\x42\x01a"); // params[1] steps b, a
    wire.extend_from_slice(&[0x1a, 0x00]); // params[2] empty
    wire.extend_from_slice(&[0x22, 0x04, 0x08, 0x00, 0x18, 0x03]); // status again
    wire.extend_from_slice(&[0x1a, 0x03, 0x42, 0x01, 0xff]); // params[3] invalid UTF-8
    assert_eq!(
        GlobalTempoMessage::decode(&wire, limits()),
        Err(CodecError::Utf8(8))
    );
    wire.truncate(wire.len() - 5);
    let decoded = GlobalTempoMessage::decode(&wire, limits()).unwrap();
    let message = &decoded.message;
    assert_eq!(message.params.len(), 3);
    assert_eq!(message.params[0].index, Some(1));
    assert_eq!(message.params[1].dynamic_steps, ["b", "a"]);
    assert_eq!(message.params[2], Param::default());
    assert_eq!(
        message.metronome_status,
        Some(MetronomeStatusUpdate {
            is_enabled: Some(0),
            current_beat: Some(3),
            ..Default::default()
        })
    );
    assert_eq!(decoded.fields().len(), 5);
    assert_eq!(decoded.original_payload(), wire.as_slice());
    // Re-encoding merges the duplicate status into one occurrence; it is not replay.
    let bytes = message.encode(limits()).unwrap();
    assert_ne!(bytes, wire);
    assert_eq!(
        GlobalTempoMessage::decode(&bytes, limits())
            .unwrap()
            .message,
        *message
    );
}

#[test]
fn first_indexed_float_mirrors_receiver_gate_without_converting_cases() {
    let mut message = GlobalTempoMessage::default();
    assert_eq!(message.first_indexed_float(), None);
    message.params.push(Param {
        param_values: vec![ParamValue::float(90.0)],
        ..Default::default()
    });
    assert_eq!(message.first_indexed_float(), None, "index required");
    message.params[0].index = Some(0);
    assert_eq!(message.first_indexed_float(), Some((0, 90.0)));
    message.params[0]
        .param_values
        .insert(0, ParamValue::int(90));
    assert_eq!(
        message.first_indexed_float(),
        None,
        "first value must be float"
    );
    message.params[0].param_values.clear();
    assert_eq!(message.first_indexed_float(), None);
    // Only the first Param is consulted.
    message.params.push(Param {
        index: Some(5),
        param_values: vec![ParamValue::float(1.0)],
        ..Default::default()
    });
    assert_eq!(message.first_indexed_float(), None);
}

#[test]
fn signed_values_and_float_bits_survive_roundtrip() {
    let message = GlobalTempoMessage {
        action: Some(MessageAction::Read.into()),
        request_id: Some(u64::MAX),
        params: vec![Param {
            expression: Some(i32::MIN),
            expression_min: Some(f32::NAN),
            expression_max: Some(-0.0),
            scene_mode: Some(false),
            param_values: vec![ParamValue::float(f32::INFINITY), ParamValue::int(i32::MAX)],
            index: Some(u32::MAX),
            is_led: Some(true),
            dynamic_steps: vec![],
            dynamic_icons: vec!["ñ".into()],
            dynamic_metadata: vec![String::new(), String::new()],
        }],
        metronome_status: Some(MetronomeStatusUpdate {
            is_enabled: Some(u32::MAX),
            count_in_beats_remaining: Some(i32::MIN),
            count_in_bars_remaining: Some(-7),
            ..Default::default()
        }),
    };
    let bytes = message.encode(limits()).unwrap();
    let decoded = GlobalTempoMessage::decode(&bytes, limits())
        .unwrap()
        .message;
    let param = &decoded.params[0];
    assert_eq!(param.expression, Some(i32::MIN));
    assert_eq!(
        param.expression_min.map(f32::to_bits),
        Some(f32::NAN.to_bits())
    );
    assert_eq!(
        param.expression_max.map(f32::to_bits),
        Some((-0.0f32).to_bits())
    );
    assert_eq!(param.param_values[0].as_float(), Some(f32::INFINITY));
    assert_eq!(param.param_values[1].as_int(), Some(i32::MAX));
    assert_eq!(param.dynamic_metadata.len(), 2);
    assert_eq!(
        decoded
            .metronome_status
            .as_ref()
            .unwrap()
            .count_in_beats_remaining,
        Some(i32::MIN)
    );
    assert_eq!(
        decoded.metronome_status.as_ref().unwrap().is_enabled,
        Some(u32::MAX)
    );
    // Equality on everything except the NaN float.
    let mut expected = message.clone();
    expected.params[0].expression_min = None;
    let mut actual = decoded.clone();
    actual.params[0].expression_min = None;
    assert_eq!(actual, expected);
    assert_eq!(
        MetronomeStatusUpdate::decode(&[0x08, 0x80, 0x80, 0x80, 0x80, 0x10], limits()),
        Err(CodecError::Integer {
            field: 1,
            value: 1 << 32
        })
    );
}

#[test]
fn nested_depth_field_and_payload_limits_apply_to_tempo_messages() {
    let message = GlobalTempoMessage {
        params: vec![Param {
            index: Some(1),
            param_values: vec![ParamValue::float(1.0)],
            ..Default::default()
        }],
        ..Default::default()
    };
    let bytes = message.encode(limits()).unwrap();
    // GlobalTempo -> Param -> ParamValue is depth 2 for the ParamValue scan.
    let shallow = CodecLimits {
        max_depth: 1,
        ..limits()
    };
    assert_eq!(
        GlobalTempoMessage::decode(&bytes, shallow),
        Err(CodecError::DepthLimit(1))
    );
    assert_eq!(message.encode(shallow), Err(CodecError::DepthLimit(1)));
    let deep_enough = CodecLimits {
        max_depth: 2,
        ..limits()
    };
    assert_eq!(
        GlobalTempoMessage::decode(&bytes, deep_enough)
            .unwrap()
            .message,
        message
    );
    // Fields: params(1) + index(2) + param_values(3) + float_value(4).
    let few = CodecLimits {
        max_fields: 3,
        ..limits()
    };
    assert_eq!(
        GlobalTempoMessage::decode(&bytes, few),
        Err(CodecError::FieldLimit(3))
    );
    assert_eq!(message.encode(few), Err(CodecError::FieldLimit(3)));
    let exact = CodecLimits {
        max_fields: 4,
        ..limits()
    };
    assert!(GlobalTempoMessage::decode(&bytes, exact).is_ok());
    let small = CodecLimits {
        max_payload_bytes: bytes.len() - 1,
        ..limits()
    };
    assert_eq!(
        GlobalTempoMessage::decode(&bytes, small),
        Err(CodecError::PayloadLimit(bytes.len() - 1))
    );
    assert_eq!(
        message.encode(small),
        Err(CodecError::PayloadLimit(bytes.len() - 1))
    );
    // Truncated nested length is a wire error, not a panic.
    let mut truncated = bytes.clone();
    truncated.pop();
    assert!(matches!(
        GlobalTempoMessage::decode(&truncated, limits()),
        Err(CodecError::Wire { .. })
    ));
}

#[test]
fn control_message_selects_global_tempo_by_envelope_type_33() {
    let message = GlobalTempoMessage {
        action: Some(MessageAction::Read.into()),
        request_id: Some(1),
        ..Default::default()
    };
    let control = ControlMessage::GlobalTempo(message.clone());
    assert_eq!(control.message_type(), CortexMessageType::GlobalTempo);
    let mut envelope = Envelope {
        message_type: 33,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload: control.encode(limits()).unwrap(),
    };
    let decoded = ControlMessage::decode(&envelope, limits()).unwrap();
    assert_eq!(decoded.message, control);
    envelope.decompress = 1;
    assert_eq!(
        ControlMessage::decode(&envelope, limits()),
        Err(CodecError::Envelope(Error::UnsupportedCompression))
    );
    assert!(matches!(ParamValueKind::Int(1), ParamValueKind::Int(_)));
}

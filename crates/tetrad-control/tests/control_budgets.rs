//! Regression vectors for three budget findings of a code review: raw-message encode
//! validation, packed encode byte bounds, and unpacked element charges.
//! Synthetic wire vectors only.

use tetrad_control::codecs::{
    AvailableModes, CodecError, CodecLimits, ConfirmationMessage, ModeMessage, NewModelsMessage,
    RawMessageBytes, RecallPresetMessage, SetTestPresetSplitMixPointsMessage, UndoRedoMessage,
};
use tetrad_control::scene::SceneWireError;

fn limits(max_payload_bytes: usize, max_fields: usize, max_depth: usize) -> CodecLimits {
    CodecLimits {
        max_payload_bytes,
        max_fields,
        max_depth,
    }
}

fn fields(max_fields: usize) -> CodecLimits {
    CodecLimits {
        max_fields,
        ..CodecLimits::default()
    }
}

fn recall(raw: &[u8]) -> RecallPresetMessage {
    RecallPresetMessage {
        preset: Some(RawMessageBytes {
            bytes: raw.to_vec(),
        }),
        ..Default::default()
    }
}

/// Encode applies the same shallow scan as decode: depth, top-level wire
/// validity and top-level field cost, while the raw bytes stay exact.
#[test]
fn raw_message_encode_matches_decode_depth_wire_and_field_budget() {
    // An empty raw message still opens a nested level.
    let flat = limits(16, 8, 0);
    assert_eq!(recall(&[]).encode(flat), Err(CodecError::DepthLimit(0)));
    assert_eq!(
        RecallPresetMessage::decode(&[0x1a, 0x00], flat),
        Err(CodecError::DepthLimit(0))
    );

    // Wire type 7 is invalid at the raw message's top level.
    let malformed = || CodecError::Wire {
        offset: 1,
        problem: SceneWireError::WireType(7),
    };
    assert_eq!(
        recall(&[0x0f]).encode(CodecLimits::default()),
        Err(malformed())
    );
    assert_eq!(
        RecallPresetMessage::decode(&[0x1a, 0x01, 0x0f], CodecLimits::default()),
        Err(malformed())
    );

    // Outer preset key + two raw keys = 3 units on both paths.
    let raw = [0x08, 0x01, 0x10, 0x02];
    let wire = [0x1a, 0x04, 0x08, 0x01, 0x10, 0x02];
    let two = limits(16, 2, 1);
    assert_eq!(recall(&raw).encode(two), Err(CodecError::FieldLimit(2)));
    assert_eq!(
        RecallPresetMessage::decode(&wire, two),
        Err(CodecError::FieldLimit(2))
    );
    let three = limits(16, 3, 1);
    assert_eq!(recall(&raw).encode(three).unwrap(), wire);
    assert_eq!(
        RecallPresetMessage::decode(&wire, three).unwrap().message,
        recall(&raw)
    );

    // Deeper length-delimited contents stay opaque: field 1 holds a body
    // that is not valid protobuf, and only its outer key is charged.
    let opaque = [0x0a, 0x01, 0x0f];
    assert_eq!(
        recall(&opaque).encode(limits(16, 2, 1)).unwrap(),
        [0x1a, 0x03, 0x0a, 0x01, 0x0f]
    );

    // The byte cap is checked before the scan: 1 key + 1 prefix + 1 byte.
    assert_eq!(
        recall(&[0x0f]).encode(limits(2, 8, 1)),
        Err(CodecError::PayloadLimit(2))
    );
}

/// Every correct-wire unpacked element costs its key plus one element unit,
/// exactly as a packed element does, for numerics, bools, enums and nested lists.
#[test]
fn unpacked_numeric_bool_enum_and_nested_elements_pay_key_plus_element() {
    // Two unpacked uint32 models: 2 keys + 2 elements = 4.
    let unpacked = [0x18, 0x01, 0x18, 0x02];
    assert_eq!(
        NewModelsMessage::decode(&unpacked, fields(3)),
        Err(CodecError::FieldLimit(3))
    );
    let message = NewModelsMessage::decode(&unpacked, fields(4))
        .unwrap()
        .message;
    assert_eq!(message.models, [1, 2]);
    // The packed form costs 1 key + 2 elements = 3, so typed re-encoding of an
    // accepted unpacked payload never needs more than its decode budget.
    let packed = [0x1a, 0x02, 0x01, 0x02];
    assert_eq!(message.encode(fields(3)).unwrap(), packed);
    assert_eq!(
        NewModelsMessage::decode(&packed, fields(3))
            .unwrap()
            .message,
        message
    );

    // One unpacked bool or enum costs 2.
    let bool_wire = [0x30, 0x01]; // hard_pan_mixer
    assert_eq!(
        SetTestPresetSplitMixPointsMessage::decode(&bool_wire, fields(1)),
        Err(CodecError::FieldLimit(1))
    );
    assert_eq!(
        SetTestPresetSplitMixPointsMessage::decode(&bool_wire, fields(2))
            .unwrap()
            .message
            .hard_pan_mixer,
        [true]
    );
    let enum_wire = [0x28, 0x01]; // actions
    assert_eq!(
        ConfirmationMessage::decode(&enum_wire, fields(1)),
        Err(CodecError::FieldLimit(1))
    );
    assert_eq!(
        ConfirmationMessage::decode(&enum_wire, fields(2))
            .unwrap()
            .message
            .actions
            .len(),
        1
    );

    // Nested: available_modes key + 2 unpacked keys + 2 elements = 5.
    let nested = [0x22, 0x04, 0x08, 0x01, 0x08, 0x02];
    assert_eq!(
        ModeMessage::decode(&nested, fields(4)),
        Err(CodecError::FieldLimit(4))
    );
    assert_eq!(
        ModeMessage::decode(&nested, fields(5))
            .unwrap()
            .message
            .available_modes,
        Some(AvailableModes { modes: vec![1, 2] })
    );
}

/// The packed encoder checks the exact key + length prefix + body total
/// against the byte cap, including inside nested messages.
#[test]
fn packed_encode_checks_exact_total_and_rejects_impossible_lists_early() {
    let undo = |undo_frames: Vec<i32>| UndoRedoMessage {
        undo_frames,
        ..Default::default()
    };

    // Review counterexample: 100 000 ten-byte elements under a tiny cap. The
    // field budget allows the list, so only the byte cap can reject it.
    let huge = undo(vec![-1; 100_000]);
    assert_eq!(
        huge.encode(limits(1, 100_001, 0)),
        Err(CodecError::PayloadLimit(1))
    );
    assert_eq!(
        huge.encode(limits(64, 100_001, 0)),
        Err(CodecError::PayloadLimit(64))
    );
    // Few enough elements to pass the count check; sizing stops on overflow.
    assert_eq!(
        undo(vec![-1; 10]).encode(limits(64, 11, 0)),
        Err(CodecError::PayloadLimit(64))
    );

    // -1 packs as ten bytes: 1 key + 1 prefix + 10 = 12.
    let one = undo(vec![-1]);
    assert_eq!(
        one.encode(limits(11, 2, 0)),
        Err(CodecError::PayloadLimit(11))
    );
    assert_eq!(one.encode(limits(12, 2, 0)).unwrap().len(), 12);

    // A 128-byte body needs a two-byte prefix: 1 + 2 + 128 = 131.
    let zeros = undo(vec![0; 128]);
    assert_eq!(
        zeros.encode(limits(130, 129, 0)),
        Err(CodecError::PayloadLimit(130))
    );
    let bytes = zeros.encode(limits(131, 129, 0)).unwrap();
    assert_eq!(bytes.len(), 131);
    assert_eq!(&bytes[..3], [0x1a, 0x80, 0x01]);

    // Nested room includes the parent's key and minimum prefix: 6 bytes total.
    let mode = ModeMessage {
        available_modes: Some(AvailableModes { modes: vec![1, 2] }),
        ..Default::default()
    };
    assert_eq!(
        mode.encode(limits(5, 4, 1)),
        Err(CodecError::PayloadLimit(5))
    );
    assert_eq!(
        mode.encode(limits(6, 4, 1)).unwrap(),
        [0x22, 0x04, 0x0a, 0x02, 0x01, 0x02]
    );
}

/// A nested body is written in place after a one-byte length placeholder (A3); a body of 128
/// bytes or more moves up for its two-byte prefix. The bytes are the plain protobuf form, and
/// the payload budget still counts every byte: the exact size fits, one byte less does not.
#[test]
fn nested_bodies_get_exact_prefixes_and_budget() {
    use tetrad_control::codecs::{FileMessage, FolderInfo};
    for name_bytes in [0, 125, 126, 300] {
        let message = FileMessage {
            folder: Some(FolderInfo {
                name: Some("n".repeat(name_bytes)),
                ..Default::default()
            }),
            ..Default::default()
        };
        let bytes = message.encode(CodecLimits::default()).unwrap();
        // Field 3 (name) inside the folder: key, length, text.
        let name_prefix = if name_bytes < 128 { 1 } else { 2 };
        let body = 1 + name_prefix + name_bytes;
        let body_prefix = if body < 128 { 1 } else { 2 };
        assert_eq!(bytes.len(), 1 + body_prefix + body, "name {name_bytes}");
        let decoded = FileMessage::decode(&bytes, CodecLimits::default()).unwrap();
        assert_eq!(decoded.message, message);
        let exact = limits(bytes.len(), 64, 8);
        assert_eq!(message.encode(exact).as_ref(), Ok(&bytes));
        let short = limits(bytes.len() - 1, 64, 8);
        assert_eq!(
            message.encode(short),
            Err(CodecError::PayloadLimit(bytes.len() - 1))
        );
    }
}

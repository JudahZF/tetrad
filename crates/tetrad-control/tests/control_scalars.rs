//! Scalar-only control envelopes added with the tempo slice. Synthetic wire
//! vectors only; no stock payloads, descriptor bytes or device traffic.

use tetrad_control::Envelope;
use tetrad_control::codecs::{
    CodecError, CodecLimits, CompilerInhibitedModulesMessage, ControlMessage, CortexMessageType,
    EnableCaptureOutMessage, EnumValue, GigViewButtonMessage, MasterVolumeMessage, MessageAction,
    PresetDirtyMessage, RecentSearchesMessage, SceneColorMessage, SceneCopyMessage,
    SceneLabelMessage, ShowGigViewMessage, SystemTimeSyncMessage,
};

fn limits() -> CodecLimits {
    CodecLimits::default()
}

/// One hand-encoded vector per message: descriptor field numbers, wire types
/// and signedness, exact re-encoding, and envelope-type selection.
#[test]
fn scalar_messages_match_descriptor_wire_and_select_by_envelope_type() {
    let update = Some(EnumValue::from(MessageAction::Update));
    let cases: Vec<(u32, Vec<u8>, ControlMessage)> = vec![
        (
            16,
            vec![0x08, 0x09, 0x10, 0x01],
            ControlMessage::EnableCaptureOut(EnableCaptureOutMessage {
                request_id: Some(9),
                enable: Some(true),
            }),
        ),
        (
            17,
            vec![
                0x08, 0x01, 0x10, 0x02, 0x1d, 0x00, 0x00, 0x80, 0xbf, 0x20, 0x00, 0x28, 0x01,
            ],
            ControlMessage::MasterVolume(MasterVolumeMessage {
                action: update,
                request_id: Some(2),
                volume: Some(-1.0),
                engaged: Some(false),
                calibrate: Some(true),
            }),
        ),
        (
            22,
            vec![
                0x08, 0x01, 0x18, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01, 0x20,
                0x07, 0x28, 0x01,
            ],
            ControlMessage::SceneCopy(SceneCopyMessage {
                action: update,
                request_id: None,
                from_index: Some(-1),
                to_index: Some(7),
                is_swap: Some(true),
            }),
        ),
        (
            23,
            b"\x08\x01\x18\x03\x22\x05Verse".to_vec(),
            ControlMessage::SceneLabel(SceneLabelMessage {
                action: update,
                request_id: None,
                index: Some(3),
                label: Some("Verse".into()),
            }),
        ),
        (
            24,
            vec![0x08, 0x01, 0x18, 0x00],
            ControlMessage::ShowGigView(ShowGigViewMessage {
                action: update,
                request_id: None,
                show: Some(false),
            }),
        ),
        (
            34,
            vec![0x08, 0x01, 0x10, 0x05, 0x18, 0x01],
            ControlMessage::PresetDirty(PresetDirtyMessage {
                action: update,
                request_id: Some(5),
                is_dirty: Some(true),
            }),
        ),
        (
            39,
            b"\x08\x03\x1a\x02ab\x1a\x00\x1a\x01c".to_vec(),
            ControlMessage::RecentSearches(RecentSearchesMessage {
                action: Some(MessageAction::Read.into()),
                request_id: None,
                searches: vec!["ab".into(), String::new(), "c".into()],
            }),
        ),
        (
            42,
            vec![0x08, 0x01, 0x18, 0x01, 0x20, 0x00],
            ControlMessage::CompilerInhibitedModules(CompilerInhibitedModulesMessage {
                action: update,
                request_id: None,
                global_gate: Some(true),
                global_eq: Some(false),
            }),
        ),
        (
            43,
            vec![
                0x08, 0x01, 0x18, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
            ],
            ControlMessage::SystemTimeSync(SystemTimeSyncMessage {
                action: update,
                request_id: None,
                ms_since_epoch: Some(u64::MAX),
            }),
        ),
        (
            48,
            vec![0x08, 0x01, 0x18, 0x02, 0x20, 0xff, 0xff, 0xff, 0xff, 0x0f],
            ControlMessage::SceneColor(SceneColorMessage {
                action: update,
                request_id: None,
                index: Some(2),
                color: Some(u32::MAX),
            }),
        ),
        (
            55,
            vec![0x08, 0x01, 0x18, 0x04, 0x20, 0x01],
            ControlMessage::GigViewButton(GigViewButtonMessage {
                action: update,
                request_id: None,
                button_index: Some(4),
                pressed: Some(true),
            }),
        ),
    ];
    for (message_type, wire, expected) in cases {
        let envelope = Envelope {
            message_type,
            transform: 0,
            decompress: 0,
            opaque: [0; 2],
            payload: wire.clone(),
        };
        let decoded = ControlMessage::decode(&envelope, limits()).unwrap();
        assert_eq!(decoded.message, expected, "type {message_type}");
        assert_eq!(
            CortexMessageType::try_from(message_type).unwrap(),
            expected.message_type()
        );
        assert_eq!(
            expected.encode(limits()).unwrap(),
            wire,
            "type {message_type}"
        );
        assert_eq!(decoded.original_payload(), wire.as_slice());
    }
    // Shared rules still hold: strings must be UTF-8, uint32 is bounded.
    assert_eq!(
        SceneLabelMessage::decode(b"\x22\x01\xff", limits()),
        Err(CodecError::Utf8(4))
    );
    assert_eq!(
        SceneColorMessage::decode(&[0x20, 0x80, 0x80, 0x80, 0x80, 0x10], limits()),
        Err(CodecError::Integer {
            field: 4,
            value: 1 << 32
        })
    );
}

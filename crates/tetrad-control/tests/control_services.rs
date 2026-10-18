//! Version, Updater, GenericError and HTTP-forwarding codecs. Synthetic wire
//! vectors only; no stock payloads, descriptor bytes or device traffic.

use tetrad_control::codecs::{
    BackupsForwardMessage, BackupsForwardRequest, BackupsForwardRequestType,
    BackupsForwardResponse, CodecError, CodecLimits, ControlMessage, CortexMessageType, EnumValue,
    ForwardMessage, GenericErrorCode, GenericErrorMessage, HttpOperation, LogsForwardMessage,
    LogsForwardRequest, LogsForwardRequestType, LogsForwardResponse, MessageAction,
    ProductForwardHeaderPair, ProductForwardMessage, ProductForwardRequest, ProductForwardResponse,
    ProtoEnum, UpdaterCommand, UpdaterForwardMessage, UpdaterForwardRequest,
    UpdaterForwardRequestType, UpdaterMessage, UpdaterState, UpdaterStatus, VersionDeviceType,
    VersionMessage, WireValue,
};
use tetrad_control::{Envelope, Error, Limits, Reassembler};

fn header(name: &str, value: &str) -> ProductForwardHeaderPair {
    ProductForwardHeaderPair {
        name: Some(name.to_owned()),
        value: Some(value.to_owned()),
    }
}

/// Hand-encoded VersionMessage: every field number and wire type from the
/// descriptor, including the out-of-declaration-order fields 11 and 14.
#[test]
fn version_wire_vector_matches_descriptor_numbers_and_types() {
    let mut wire = vec![0x08, 0x03, 0x10, 0x80, 0x01]; // action READ, request_id 128
    wire.extend_from_slice(b"\x1a\x05" /* 3 */);
    wire.extend_from_slice(b"5.4.0");
    wire.extend_from_slice(b"\x52\x02\x01\x02"); // 10 comms_version bytes
    wire.extend_from_slice(b"\x5a\x03" /* 11 */);
    wire.extend_from_slice(b"4.1");
    wire.extend_from_slice(&[0x60, 0x01]); // 12 device_type ATMA
    wire.extend_from_slice(&[0x68, 0x01]); // 13 is_ess true
    wire.extend_from_slice(&[0x70, 0x00]); // 14 cortex_control_version_valid false
    wire.extend_from_slice(b"\x9a\x01\x04" /* 19 */);
    wire.extend_from_slice(b"http");
    let decoded = VersionMessage::decode(&wire, CodecLimits::default()).unwrap();
    let message = &decoded.message;
    assert_eq!(message.action, Some(MessageAction::Read.into()));
    assert_eq!(message.request_id, Some(128));
    assert_eq!(message.linux_kernel_version.as_deref(), Some("5.4.0"));
    assert_eq!(message.zenos_git_hash, None);
    assert_eq!(message.comms_version.as_deref(), Some(&[1u8, 2][..]));
    assert_eq!(message.cortex_control_version.as_deref(), Some("4.1"));
    assert_eq!(message.device_type, Some(VersionDeviceType::Atma.into()));
    assert_eq!(message.is_ess, Some(true));
    assert_eq!(message.cortex_control_version_valid, Some(false));
    assert_eq!(message.cloud_endpoint.as_deref(), Some("http"));
    assert_eq!(decoded.original_payload(), wire.as_slice());
    // Numeric field order on output puts 11 before 12 and 14 before 15.
    assert_eq!(message.encode(CodecLimits::default()).unwrap(), wire);

    let full = VersionMessage {
        action: Some(EnumValue::Unknown(9)),
        request_id: Some(u64::MAX),
        linux_kernel_version: Some(String::new()),
        zenos_git_hash: Some("g".repeat(300)),
        zenwireless_fw_version: Some("w".into()),
        uboot_version: Some("u".into()),
        app_fw_version: Some("a".into()),
        bootloader_fw_version: Some("b".into()),
        device_serial_number: Some("s".into()),
        comms_version: Some(vec![]),
        cortex_control_version: Some("c".into()),
        device_type: Some(EnumValue::Unknown(-1)),
        is_ess: Some(false),
        cortex_control_version_valid: Some(true),
        custom_name: Some("ñ".into()),
        mac_address: Some("m".into()),
        zencoder_fw_app: Some("z".into()),
        zencoder_fw_bootloader: Some("y".into()),
        cloud_endpoint: Some("e".into()),
    };
    let bytes = full.encode(CodecLimits::default()).unwrap();
    assert_eq!(
        VersionMessage::decode(&bytes, CodecLimits::default())
            .unwrap()
            .message,
        full
    );
    assert_eq!(
        VersionMessage::decode(&[], CodecLimits::default())
            .unwrap()
            .message,
        VersionMessage::default()
    );
}

#[test]
fn version_envelopes_keep_the_decompression_bypass_but_not_transforms() {
    let payload = VersionMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    }
    .encode(CodecLimits::default())
    .unwrap();
    let mut envelope = Envelope {
        message_type: 10,
        transform: 0,
        decompress: 1,
        opaque: [7, 9],
        payload,
    };
    let decoded = ControlMessage::decode(&envelope, CodecLimits::default()).unwrap();
    assert!(matches!(decoded.message, ControlMessage::Version(_)));
    assert_eq!(decoded.message.message_type(), CortexMessageType::Version);
    envelope.transform = 2;
    assert_eq!(
        ControlMessage::decode(&envelope, CodecLimits::default()),
        Err(CodecError::Envelope(Error::UnsupportedTransform(2)))
    );
    envelope.transform = 0;
    envelope.message_type = 60;
    assert_eq!(
        ControlMessage::decode(&envelope, CodecLimits::default()),
        Err(CodecError::Envelope(Error::UnsupportedCompression))
    );
}

#[test]
fn string_fields_reject_invalid_utf8_but_bytes_fields_accept_it() {
    let bad_string = [0x1a, 0x02, 0xff, 0xfe]; // VersionMessage.linux_kernel_version
    assert_eq!(
        VersionMessage::decode(&bad_string, CodecLimits::default()),
        Err(CodecError::Utf8(3))
    );
    let bad_bytes = [0x52, 0x02, 0xff, 0xfe]; // VersionMessage.comms_version
    let decoded = VersionMessage::decode(&bad_bytes, CodecLimits::default()).unwrap();
    assert_eq!(decoded.message.comms_version, Some(vec![0xff, 0xfe]));
    // A wrong wire type for a string is retained raw and supplies no value.
    let wrong_wire = [0x18, 0x05];
    let decoded = VersionMessage::decode(&wrong_wire, CodecLimits::default()).unwrap();
    assert_eq!(decoded.message.linux_kernel_version, None);
    assert_eq!(decoded.fields()[0].value, WireValue::Varint(5));
    // Nested string errors report the local field number of the nested message.
    let nested = [0x22, 0x04, 0x12, 0x02, 0xc0, 0x00]; // ProductForward.request.url
    assert_eq!(
        ProductForwardMessage::decode(&nested, CodecLimits::default()),
        Err(CodecError::Utf8(2))
    );
}

#[test]
fn updater_and_generic_error_use_descriptor_enums_and_retain_unknowns() {
    let wire = [
        0x08, 0x01, // action UPDATE
        0x18, 0x01, // command CHECK_AND_DOWNLOAD
        0x20, 0x03, // status NO_NEW_VERSION_AVAILABLE
        0x28, 0x06, // state: unknown 6
        0x32, 0x05, b'4', b'.', b'1', b'.', b'0', // new_version_id
        0x45, 0x00, 0x00, 0x00, 0x3f, // download_progress 0.5
        0x4d, 0x00, 0x00, 0xc0, 0x7f, // installation_progress NaN
    ];
    let decoded = UpdaterMessage::decode(&wire, CodecLimits::default()).unwrap();
    let message = &decoded.message;
    assert_eq!(message.request_id, None);
    assert_eq!(
        message.command,
        Some(UpdaterCommand::CheckAndDownload.into())
    );
    assert_eq!(
        message.status,
        Some(UpdaterStatus::NoNewVersionAvailable.into())
    );
    assert_eq!(message.state, Some(EnumValue::Unknown(6)));
    assert_eq!(message.new_version_id.as_deref(), Some("4.1.0"));
    assert_eq!(message.changelog, None);
    assert_eq!(message.download_progress, Some(0.5));
    assert!(message.installation_progress.unwrap().is_nan());
    assert_eq!(message.encode(CodecLimits::default()).unwrap(), wire);
    assert_eq!(UpdaterState::try_from(5), Ok(UpdaterState::Failed));
    assert_eq!(
        UpdaterCommand::try_from(4),
        Ok(UpdaterCommand::CancelDownload)
    );

    // GenericError has no action field: field 1 is the request ID.
    let wire = [0x08, 0x2a, 0x10, 0x02];
    let decoded = GenericErrorMessage::decode(&wire, CodecLimits::default()).unwrap();
    assert_eq!(
        decoded.message,
        GenericErrorMessage {
            request_id: Some(42),
            error_code: Some(GenericErrorCode::CloudLoginFailed.into()),
        }
    );
    assert_eq!(
        decoded.message.encode(CodecLimits::default()).unwrap(),
        wire
    );
    assert_eq!(
        GenericErrorMessage::decode(&[0x10, 0x05], CodecLimits::default())
            .unwrap()
            .message
            .error_code,
        Some(EnumValue::Unknown(5))
    );
    assert_eq!(
        GenericErrorCode::try_from(4),
        Ok(GenericErrorCode::ExcessDownloadsCapturesWarningTriggered)
    );
}

#[test]
fn forward_wire_vector_keeps_repeated_headers_and_merges_duplicate_halves() {
    let mut wire = vec![0x08, 0x00, 0x10, 0x07, 0x18, 0x09]; // CREATE, request 7, cloud 9
    wire.extend_from_slice(&[0x22, 0x1a]); // request, 26 bytes
    wire.extend_from_slice(&[0x08, 0x02]); // operation POST
    wire.extend_from_slice(b"\x12\x03/v1");
    wire.extend_from_slice(&[0x18, 0x1e]); // timeout 30
    wire.extend_from_slice(&[0x20, 0x01]); // is_multipart true
    wire.extend_from_slice(b"\x2a\x00"); // payload: explicit empty
    wire.extend_from_slice(b"\x32\x06\x0a\x01a\x12\x01b"); // header a=b
    wire.extend_from_slice(b"\x32\x03\x0a\x01c"); // header c, no value
    wire.extend_from_slice(&[0x2a, 0x04, 0x0a, 0x02, 0xff, 0x00]); // response payload
    wire.extend_from_slice(&[0x2a, 0x02, 0x10, 0x00]); // second response: error_code 0
    let decoded = ProductForwardMessage::decode(&wire, CodecLimits::default()).unwrap();
    let message = &decoded.message;
    assert_eq!(message.forward_request_id(), Some(9));
    assert_eq!(message.request_id(), Some(7));
    let request = message.request().unwrap();
    assert_eq!(request.operation, Some(HttpOperation::Post.into()));
    assert_eq!(request.url.as_deref(), Some("/v1"));
    assert_eq!(request.timeout, Some(30));
    assert_eq!(request.is_multipart, Some(true));
    assert_eq!(request.payload, Some(vec![]));
    assert_eq!(
        message.request_headers(),
        [
            header("a", "b"),
            ProductForwardHeaderPair {
                name: Some("c".into()),
                value: None,
            }
        ]
    );
    assert_eq!(
        message.response(),
        Some(&ProductForwardResponse {
            payload: Some(vec![0xff, 0x00]),
            error_code: Some(0),
        })
    );
    assert_eq!(decoded.original_payload(), wire.as_slice());
    assert_eq!(decoded.fields().len(), 6);
    // Re-encoding merges the two response occurrences into one known-field message.
    let encoded = message.encode(CodecLimits::default()).unwrap();
    assert_ne!(encoded, wire);
    assert_eq!(
        ProductForwardMessage::decode(&encoded, CodecLimits::default())
            .unwrap()
            .message,
        *message
    );
    // Negative timeouts use the ten-byte int32 form and come back unchanged.
    let negative = ProductForwardRequest {
        timeout: Some(-1),
        ..Default::default()
    }
    .encode(CodecLimits::default())
    .unwrap();
    assert_eq!(negative.len(), 11);
    assert_eq!(
        ProductForwardRequest::decode(&negative, CodecLimits::default())
            .unwrap()
            .message
            .timeout,
        Some(-1)
    );
}

#[test]
fn all_four_forward_families_roundtrip_with_their_own_schemas() {
    let messages = [
        ControlMessage::BackupsForward(BackupsForwardMessage {
            action: Some(MessageAction::Upload.into()),
            request_id: Some(1),
            backups_request_id: Some(0),
            request: Some(BackupsForwardRequest {
                request_type: Some(BackupsForwardRequestType::UploadStatus.into()),
                url: None,
                payload: Some(vec![1, 2, 3]),
                is_last_chunk: Some(false),
                timeout: Some(0),
                headers: vec![],
            }),
            response: Some(BackupsForwardResponse {
                payload: None,
                is_last_chunk: Some(true),
                error_code: Some(-404),
            }),
        }),
        ControlMessage::LogsForward(LogsForwardMessage {
            action: None,
            request_id: None,
            logs_request_id: Some(u64::MAX),
            request: Some(LogsForwardRequest {
                request_type: Some(LogsForwardRequestType::UploadCoredump.into()),
                url: Some("u".into()),
                payload: None,
                is_last_chunk: None,
                timeout: None,
                headers: vec![header("x", "y"); 3],
            }),
            response: Some(LogsForwardResponse {
                payload: Some(vec![]),
                error_code: None,
            }),
        }),
        ControlMessage::UpdaterForward(UpdaterForwardMessage {
            action: Some(MessageAction::Download.into()),
            request_id: Some(2),
            updater_request_id: None,
            request: Some(UpdaterForwardRequest {
                request_type: Some(UpdaterForwardRequestType::DownloadUpdate.into()),
                url: Some(String::new()),
                timeout: Some(i32::MAX),
                payload: Some("json".into()),
                headers: vec![],
            }),
            response: None,
        }),
        ControlMessage::ProductForward(ProductForwardMessage {
            action: Some(MessageAction::Read.into()),
            request_id: None,
            cloud_request_id: Some(3),
            request: None,
            response: Some(ProductForwardResponse::default()),
        }),
    ];
    let expected_types = [
        CortexMessageType::BackupsForward,
        CortexMessageType::LogsForward,
        CortexMessageType::UpdaterForward,
        CortexMessageType::ProductForward,
    ];
    for (message, expected) in messages.iter().zip(expected_types) {
        assert_eq!(message.message_type(), expected);
        let envelope = Envelope {
            message_type: expected.number() as u32,
            transform: 0,
            decompress: 0,
            opaque: [0; 2],
            payload: message.encode(CodecLimits::default()).unwrap(),
        };
        let mut reassembler = Reassembler::new(Limits::default());
        let mut received = None;
        for report in envelope.reports(Limits::default()).unwrap() {
            if let Some(envelope) = reassembler.push(&report).unwrap() {
                received = Some(envelope);
            }
        }
        let received = received.unwrap();
        assert_eq!(received, envelope);
        assert_eq!(
            ControlMessage::decode(&received, CodecLimits::default())
                .unwrap()
                .message,
            *message
        );
    }
    // Empty forward halves and absent halves encode differently.
    let absent = ProductForwardMessage::default()
        .encode(CodecLimits::default())
        .unwrap();
    let present = ProductForwardMessage {
        request: Some(ProductForwardRequest::default()),
        ..Default::default()
    }
    .encode(CodecLimits::default())
    .unwrap();
    assert_eq!(absent, Vec::<u8>::new());
    assert_eq!(present, vec![0x22, 0x00]);
}

#[test]
fn forward_limits_bound_headers_bytes_and_nesting_on_read_and_write() {
    let limits = CodecLimits {
        max_payload_bytes: 64,
        max_fields: 8,
        max_depth: 2,
    };
    let many_headers = ProductForwardMessage {
        request: Some(ProductForwardRequest {
            headers: vec![header("a", "b"); 3],
            ..Default::default()
        }),
        ..Default::default()
    };
    // 1 request + 3 headers + 6 name/value fields = 10 > 8 fields.
    assert_eq!(many_headers.encode(limits), Err(CodecError::FieldLimit(8)));
    let bytes = many_headers.encode(CodecLimits::default()).unwrap();
    assert_eq!(
        ProductForwardMessage::decode(&bytes, limits),
        Err(CodecError::FieldLimit(8))
    );
    // Message > request > header is depth 2; a limit of 1 refuses the header.
    let shallow = CodecLimits {
        max_depth: 1,
        ..CodecLimits::default()
    };
    assert_eq!(
        ProductForwardMessage::decode(&bytes, shallow),
        Err(CodecError::DepthLimit(1))
    );
    assert_eq!(many_headers.encode(shallow), Err(CodecError::DepthLimit(1)));
    let big = UpdaterMessage {
        changelog: Some("c".repeat(70)),
        ..Default::default()
    };
    assert_eq!(big.encode(limits), Err(CodecError::PayloadLimit(64)));
    let big_bytes = big.encode(CodecLimits::default()).unwrap();
    assert_eq!(
        UpdaterMessage::decode(&big_bytes, limits),
        Err(CodecError::PayloadLimit(64))
    );
    // Truncated nested length is a wire error inside the request payload.
    assert!(matches!(
        ProductForwardMessage::decode(&[0x22, 0x05, 0x12, 0x09, b'a'], CodecLimits::default()),
        Err(CodecError::Wire { .. })
    ));
}

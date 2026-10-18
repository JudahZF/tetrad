//! Stock receive/send stage contracts (`0x290ae8`, sender template `0xee2ec`).
//! Synthetic stages only: the transform here is a test XOR, not the stock cipher.

use tetrad_control::codecs::{
    CodecLimits, ConfirmationMessage, ControlMessage, LogsMessage, ModelRepoMessage,
    PresetDirtyMessage, ProductForwardMessage, VersionMessage,
};
use tetrad_control::pipeline::{
    COMPRESS_ABOVE_BYTES, Compression, Dropped, PipelineConfig, PipelineError, RequestStamp,
    SendLimits, SendOptions, StageFailure, Stages, Transform,
};
use tetrad_control::{Envelope, Limits, Reassembler};

/// Marks inflate/deflate so the test can see order and bypasses.
struct Tagged;

impl Compression for Tagged {
    fn inflate(&self, input: &[u8], max_output: usize) -> Result<Vec<u8>, StageFailure> {
        let body = input.strip_prefix(b"Z").ok_or(StageFailure::Malformed)?;
        if body.len() > max_output {
            return Err(StageFailure::Limit(max_output));
        }
        Ok(body.to_vec())
    }

    fn deflate(&self, input: &[u8]) -> Result<Vec<u8>, StageFailure> {
        Ok([b"Z", input].concat())
    }
}

struct Xor;

impl Transform for Xor {
    fn decode(&self, input: &[u8]) -> Result<Vec<u8>, StageFailure> {
        Ok(input.iter().map(|byte| byte ^ 0x5a).collect())
    }

    fn encode(&self, input: &[u8]) -> Result<Vec<u8>, StageFailure> {
        self.decode(input)
    }
}

fn stages() -> Stages<'static> {
    Stages {
        compression: Some(&Tagged),
        transform: Some(&Xor),
    }
}

fn envelope(message_type: u32, transform: u8, decompress: u8, payload: &[u8]) -> Envelope {
    Envelope {
        message_type,
        transform,
        decompress,
        opaque: [0; 2],
        payload: payload.to_vec(),
    }
}

#[test]
fn request_id_echo_skips_confirmation_and_three_forwards() {
    let current = RequestStamp {
        explicit: 0,
        current: 0x1_0000_0002,
    };
    for message_type in [1, 28, 32, 46, 48, 61, 72] {
        assert_eq!(current.resolve(message_type), Some(0x1_0000_0002));
    }
    for message_type in [29, 30, 31, 47] {
        assert_eq!(current.resolve(message_type), None);
    }
    let explicit = RequestStamp {
        explicit: 9,
        current: 0,
    };
    assert_eq!(explicit.resolve(47), Some(9));
    assert_eq!(RequestStamp::default().resolve(34), None);

    // The stamp reaches the encoded payload; an absent ID stays absent.
    let config = PipelineConfig::new(1024);
    let send = |message: ControlMessage, stamp| {
        let reports = config
            .send(
                message,
                SendOptions::default(),
                stamp,
                SendLimits::default(),
                Stages::default(),
            )
            .unwrap()
            .unwrap();
        Reassembler::new(Limits::default())
            .push(&reports[0])
            .unwrap()
            .unwrap()
    };
    let dirty = send(PresetDirtyMessage::default().into(), current);
    assert_eq!(dirty.message_type, 34);
    assert_eq!(dirty.payload, [0x10, 0x82, 0x80, 0x80, 0x80, 0x10]);
    let confirmation = send(ConfirmationMessage::default().into(), current);
    assert!(confirmation.payload.is_empty());
    let forward = send(ProductForwardMessage::default().into(), current);
    assert!(forward.payload.is_empty());
}

#[test]
fn receive_drops_unknown_and_blocked_then_decodes_transform_before_inflate() {
    let mut config = PipelineConfig::new(64);
    config.blocked_inbound.insert(44);
    assert_eq!(
        config.receive(envelope(73, 0, 0, &[]), stages()),
        Ok(Err(Dropped::UnknownType(73)))
    );
    assert!(matches!(
        config.receive(envelope(44, 0, 0, &[]), stages()),
        Ok(Err(Dropped::Blocked(_)))
    ));

    // Transform first, then inflate: XOR("Z" + body).
    let wire: Vec<u8> = b"Zab".iter().map(|byte| byte ^ 0x5a).collect();
    let inbound = config
        .receive(envelope(34, 1, 1, &wire), stages())
        .unwrap()
        .unwrap();
    assert_eq!(inbound.payload, b"ab");

    // Version (10) and ResetCommsBuffers (52) skip inflate even when flagged.
    for message_type in [10, 52] {
        let inbound = config
            .receive(envelope(message_type, 0, 1, b"Zab"), stages())
            .unwrap()
            .unwrap();
        assert_eq!(inbound.payload, b"Zab");
    }

    // Empty payloads skip both stages; missing stages refuse explicitly.
    assert!(
        config
            .receive(envelope(34, 1, 1, &[]), Stages::default())
            .is_ok()
    );
    assert_eq!(
        config.receive(envelope(34, 0, 1, b"Z"), Stages::default()),
        Err(PipelineError::CompressionUnavailable(34))
    );
    assert_eq!(
        config.receive(envelope(34, 1, 0, b"x"), Stages::default()),
        Err(PipelineError::TransformUnavailable(34))
    );
    assert_eq!(
        config.receive(envelope(34, 0, 1, &[b'Z'; 66]), stages()),
        Err(PipelineError::Compression {
            message_type: 34,
            failure: StageFailure::Limit(64)
        })
    );
}

#[test]
fn send_deflates_only_above_ten_kib_for_a_capable_peer_then_transforms() {
    let limits = SendLimits {
        payload: CodecLimits {
            max_payload_bytes: 1 << 20,
            ..CodecLimits::default()
        },
        reports: Limits::default(),
    };
    // ModelRepo payload field: key + 2-byte length + bytes.
    let repo = |bytes: usize| -> ControlMessage {
        ModelRepoMessage {
            model_repo_payload: Some(vec![7; bytes]),
            ..Default::default()
        }
        .into()
    };
    let options = SendOptions {
        transform: true,
        compressible: true,
    };
    let mut config = PipelineConfig::new(1 << 20);
    config.blocked_outbound.insert(44);
    let decode = |reports: Vec<[u8; 128]>| {
        let mut reassembler = Reassembler::new(Limits::default());
        reports
            .iter()
            .find_map(|report| reassembler.push(report).unwrap())
            .unwrap()
    };
    let at = COMPRESS_ABOVE_BYTES - 3;
    let send = |config: &PipelineConfig, message| {
        config
            .send(message, options, RequestStamp::default(), limits, stages())
            .unwrap()
            .unwrap()
    };

    // Peer flag off: never deflated, still transformed.
    config.peer_accepts_compression = false;
    let plain = decode(send(&config, repo(at + 1)));
    assert_eq!((plain.transform, plain.decompress), (1, 0));
    config.peer_accepts_compression = true;
    let exact = decode(send(&config, repo(at)));
    assert_eq!(exact.payload.len(), COMPRESS_ABOVE_BYTES);
    assert_eq!((exact.transform, exact.decompress), (1, 0));
    let above = decode(send(&config, repo(at + 1)));
    assert_eq!((above.transform, above.decompress), (1, 1));
    assert_eq!(above.payload[0], b'Z' ^ 0x5a);

    // The trailer flags round-trip through the receive stages.
    let back = config.receive(above, stages()).unwrap().unwrap();
    assert_eq!(
        ModelRepoMessage::decode(&back.payload, limits.payload)
            .unwrap()
            .message
            .model_repo_payload
            .map(|bytes| bytes.len()),
        Some(at + 1)
    );

    // An empty payload is never transformed; blocked types send nothing.
    let empty = decode(send(&config, VersionMessage::default().into()));
    assert_eq!((empty.transform, empty.decompress), (0, 0));
    assert_eq!(
        config.send(
            ControlMessage::from(LogsMessage::default()),
            options,
            RequestStamp::default(),
            limits,
            stages()
        ),
        Ok(None)
    );
}

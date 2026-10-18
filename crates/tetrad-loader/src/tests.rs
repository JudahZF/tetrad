use super::*;
use intent::*;

/// Invented byte arrays only. No stock firmware is embedded.
fn header(signature: Signature, flags: u16, address: u32, count: u32, argument: u32) -> [u8; 16] {
    let mut raw = [0; 16];
    raw[..2].copy_from_slice(&flags.to_le_bytes());
    raw[3] = signature as u8;
    raw[4..8].copy_from_slice(&address.to_le_bytes());
    raw[8..12].copy_from_slice(&count.to_le_bytes());
    raw[12..].copy_from_slice(&argument.to_le_bytes());
    raw[2] = raw.iter().fold(0, |xor, byte| xor ^ byte);
    raw
}

#[test]
fn borrowed_literal_fill_unknown_flags_and_byte_exact_roundtrip() {
    let literal = header(Signature::Core1, 0x2001, 0x1234, 3, 0x87654321);
    let fill = header(Signature::Core1, 0x8101, 0x4567, 1024, 0xdeadbeef);
    let mut bytes = literal.to_vec();
    bytes.extend_from_slice(&[5, 6, 7]);
    bytes.extend_from_slice(&fill);
    let parsed = RawLdr::parse(&bytes, Limits::default()).unwrap();
    assert_eq!(parsed.as_bytes(), bytes);
    assert_eq!(parsed.blocks().len(), 2);
    assert_eq!(parsed.total_declared_bytes(), 1027);
    let first = parsed.blocks()[0];
    assert_eq!(first.offset, 0);
    assert_eq!(first.header.to_bytes(), literal);
    assert_eq!(first.header.flags(), 0x2001);
    assert_eq!(first.header.target_end_exclusive().unwrap(), 0x1237);
    match first.data {
        BlockData::Literal(payload) => {
            assert_eq!(payload, &[5, 6, 7]);
            assert_eq!(payload.as_ptr(), bytes[16..].as_ptr());
        }
        _ => panic!("literal expected"),
    }
    let second = parsed.blocks()[1];
    assert_eq!(second.offset, 19);
    assert_eq!(second.header.to_bytes(), fill);
    assert_eq!(
        second.data,
        BlockData::Fill {
            byte_count: 1024,
            argument: 0xdeadbeef
        }
    );
    let reconstructed: Vec<u8> = parsed
        .blocks()
        .iter()
        .flat_map(|block| {
            let mut bytes = block.header.to_bytes().to_vec();
            if let BlockData::Literal(payload) = block.data {
                bytes.extend_from_slice(payload);
            }
            bytes
        })
        .collect();
    assert_eq!(reconstructed, bytes);
    parsed.validate_signature(Signature::Core1).unwrap();
    assert_eq!(
        parsed.validate_signature(Signature::Core2),
        Err(Error::CoreMismatch)
    );
}

#[test]
fn fill_has_no_payload_and_final_flag_does_not_hide_trailing_bytes() {
    let fill = header(Signature::Core2, 0x8101, 2, 100, 1);
    let zero = header(Signature::Core2, 1, 200, 0, 0);
    let mut bytes = fill.to_vec();
    bytes.extend_from_slice(&zero);
    assert_eq!(
        RawLdr::parse(&bytes, Limits::default())
            .unwrap()
            .blocks()
            .len(),
        2
    );
    bytes.push(0);
    assert!(matches!(
        RawLdr::parse(&bytes, Limits::default()),
        Err(Error::Truncated {
            part: "header",
            offset: 32
        })
    ));
}

#[test]
fn all_header_and_payload_truncations_fail() {
    let mut bytes = header(Signature::Core1, 1, 0, 7, 0).to_vec();
    bytes.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7]);
    for end in 0..bytes.len() {
        assert!(
            RawLdr::parse(&bytes[..end], Limits::default()).is_err(),
            "accepted truncation {end}"
        );
    }
    RawLdr::parse(&bytes, Limits::default()).unwrap();
}

#[test]
fn checksum_signature_and_raw_vs_wrapper_are_explicit_errors() {
    let valid = header(Signature::Core1, 1, 0, 0, 0);
    for byte in 0..16 {
        let mut corrupt = valid;
        corrupt[byte] ^= 1;
        assert!(Header::decode(corrupt).is_err());
        assert!(RawLdr::parse(&corrupt, Limits::default()).is_err());
    }
    let mut unsupported = valid;
    unsupported[3] = 0xad;
    unsupported[2] ^= 1;
    assert!(matches!(
        Header::decode(unsupported),
        Err(Error::Signature { value: 0xad, .. })
    ));
    assert_eq!(
        RawLdr::parse(b"BLw\x01invented", Limits::default()).unwrap_err(),
        Error::UnsupportedWrapper
    );
    assert_eq!(
        RawLdr::parse(&[], Limits::default()).unwrap_err(),
        Error::Empty
    );
}

#[test]
fn count_address_arithmetic_and_all_bounds_are_checked() {
    let overflow = header(Signature::Core1, 0x101, u32::MAX, 1, 0);
    assert_eq!(
        RawLdr::parse(&overflow, Limits::default()).unwrap_err(),
        Error::Overflow("LDR target interval")
    );
    let huge = header(Signature::Core1, 1, 0, u32::MAX, 0);
    let generous = Limits {
        max_block_bytes: u32::MAX,
        max_total_declared_bytes: u64::MAX,
        ..Limits::default()
    };
    assert!(matches!(
        RawLdr::parse(&huge, generous),
        Err(Error::Truncated {
            part: "payload",
            ..
        }) | Err(Error::Overflow("LDR payload offset"))
    ));
    let fill = header(Signature::Core2, 0x101, 0, 1024, 0);
    for limits in [
        Limits {
            max_stream_bytes: 15,
            ..Limits::default()
        },
        Limits {
            max_blocks: 0,
            ..Limits::default()
        },
        Limits {
            max_block_bytes: 1023,
            ..Limits::default()
        },
        Limits {
            max_total_declared_bytes: 1023,
            ..Limits::default()
        },
    ] {
        assert!(matches!(RawLdr::parse(&fill, limits), Err(Error::Limit(_))));
    }
    let mut two = fill.to_vec();
    two.extend_from_slice(&fill);
    assert_eq!(
        RawLdr::parse(
            &two,
            Limits {
                max_blocks: 1,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::Limit("LDR blocks")
    );
    assert_eq!(
        RawLdr::parse(
            &two,
            Limits {
                max_total_declared_bytes: 2047,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::Limit("LDR total declared bytes")
    );
    RawLdr::parse(
        &two,
        Limits {
            max_stream_bytes: 32,
            max_blocks: 2,
            max_block_bytes: 1024,
            max_total_declared_bytes: 2048,
        },
    )
    .unwrap();
}

#[test]
fn scalar_core_control_and_core2_before_core1_are_inert_data() {
    let bytes1 = header(Signature::Core1, 1, 0, 0, 0);
    let bytes2 = header(Signature::Core2, 1, 0, 0, 0);
    let core1 = RawLdr::parse(&bytes1, Limits::default()).unwrap();
    let core2 = RawLdr::parse(&bytes2, Limits::default()).unwrap();
    let plan = plan_core_pair(&core1, 0x12345678, &core2, 0x87654321).unwrap();
    let [second, first] = plan.loads;
    assert_eq!(second.core, LoaderCore::Core2);
    assert_eq!(first.core, LoaderCore::Core1);
    assert_eq!(
        second.stop,
        ScalarRequest {
            request: 0x6201,
            argument: 2
        }
    );
    assert_eq!(
        second.start,
        ScalarRequest {
            request: 0x6200,
            argument: 2
        }
    );
    assert_eq!(
        second.set_entry,
        ScalarRequest {
            request: 0x6d12,
            argument: 0x87654321
        }
    );
    assert_eq!(
        first.set_entry,
        ScalarRequest {
            request: 0x6d11,
            argument: 0x12345678
        }
    );
    assert_eq!(
        first.stop,
        ScalarRequest {
            request: 0x6201,
            argument: 1
        }
    );
    assert_eq!(
        first.start,
        ScalarRequest {
            request: 0x6200,
            argument: 1
        }
    );
    assert!(matches!(
        plan_core_pair(&core2, 0, &core1, 0),
        Err(Error::CoreMismatch)
    ));
}

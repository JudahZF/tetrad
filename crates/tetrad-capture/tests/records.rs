use tetrad_capture::records::{Error, Limits, MAX_RECORDS, RecordKind, parse_records_prefix};

fn opaque(tag: u32) -> Vec<u8> {
    let mut bytes = vec![0xa5; 32];
    bytes[..4].copy_from_slice(&tag.to_le_bytes());
    bytes
}

fn affine(inputs: u32, outputs: u32) -> Vec<u8> {
    let mut bytes = vec![0xab; 84 + 4 * inputs as usize * outputs as usize + 4 * outputs as usize];
    for (offset, value) in [(0, 4), (4, 4), (32, inputs), (44, outputs), (60, 2)] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[test]
fn skipped_records_keep_order_ordinals_opaque_bytes_and_untouched_remainder() {
    let five = opaque(5);
    let four = affine(4, 1);
    let six = opaque(6);
    let multi = affine(4, 3);
    let mut input = [
        five.as_slice(),
        four.as_slice(),
        six.as_slice(),
        multi.as_slice(),
    ]
    .concat();
    input.extend_from_slice(&[0x7f; 16]);
    let before = input.clone();
    let parsed = parse_records_prefix(&input, 4, 100, Limits::default()).unwrap();
    assert_eq!(
        parsed.records.iter().map(|r| r.ordinal).collect::<Vec<_>>(),
        [100, 101, 102, 103]
    );
    assert_eq!(
        parsed.records.iter().map(|r| r.offset).collect::<Vec<_>>(),
        [0, 32, 136, 168]
    );
    assert_eq!(parsed.consumed, 312);
    assert_eq!(parsed.next_ordinal, Some(104));
    assert_eq!(parsed.remainder, &[0x7f; 16]);
    match &parsed.records[0].kind {
        RecordKind::Opaque5(raw) => assert_eq!(raw.as_slice(), five),
        other => panic!("expected opaque5, got {other:?}"),
    }
    match &parsed.records[1].kind {
        RecordKind::Affine(record) => {
            assert_eq!(record.as_bytes(), four);
            assert_eq!(record.dimensions(), (4, 1));
        }
        other => panic!("expected affine, got {other:?}"),
    }
    match &parsed.records[2].kind {
        RecordKind::Opaque6(raw) => assert_eq!(raw.as_slice(), six),
        other => panic!("expected opaque6, got {other:?}"),
    }
    match &parsed.records[3].kind {
        RecordKind::Affine(record) => assert_eq!(record.dimensions(), (4, 3)),
        other => panic!("expected multi-output record inspection, got {other:?}"),
    }
    assert_eq!(input, before);
}

#[test]
fn every_truncated_mixed_prefix_fails_without_a_partial_result_or_input_change() {
    let input = [opaque(5), affine(4, 1), opaque(6)].concat();
    let before = input.clone();
    for end in 0..input.len() {
        let expected = match end {
            0..32 => Error::Truncated {
                ordinal: 8,
                offset: 0,
            },
            32..36 => Error::Truncated {
                ordinal: 9,
                offset: 32,
            },
            36..136 => Error::Affine {
                ordinal: 9,
                offset: 32,
                source: tetrad_capture::Error::Truncated,
            },
            _ => Error::Truncated {
                ordinal: 10,
                offset: 136,
            },
        };
        assert_eq!(
            parse_records_prefix(&input[..end], 3, 8, Limits::default()).unwrap_err(),
            expected,
            "end={end}"
        );
    }
    let parsed = parse_records_prefix(&input, 3, 8, Limits::default()).unwrap();
    assert_eq!(parsed.consumed, input.len());
    assert!(parsed.remainder.is_empty());
    assert_eq!(input, before);
}

#[test]
fn unsupported_tag_and_affine_format_errors_keep_exact_ordinal_and_offset() {
    let input = [opaque(5), opaque(38)].concat();
    assert_eq!(
        parse_records_prefix(&input, 2, 44, Limits::default()).unwrap_err(),
        Error::UnsupportedTag {
            ordinal: 45,
            offset: 32,
            tag: 38
        }
    );
    let mut four = affine(4, 1);
    four[4..8].copy_from_slice(&77_u32.to_le_bytes());
    let input = [opaque(6), four].concat();
    assert_eq!(
        parse_records_prefix(&input, 2, 9, Limits::default()).unwrap_err(),
        Error::Affine {
            ordinal: 10,
            offset: 32,
            source: tetrad_capture::Error::Format {
                selector: 77,
                format: 2
            }
        }
    );
}

#[test]
fn prefix_byte_limits_cover_consumed_bytes_not_the_remainder_and_cannot_be_raised() {
    let mut input = opaque(5);
    input.extend_from_slice(&[0x99; 64]);
    let limits = Limits {
        max_prefix_bytes: 32,
        ..Limits::default()
    };
    let parsed = parse_records_prefix(&input, 1, 0, limits).unwrap();
    assert_eq!(parsed.remainder.len(), 64);
    assert_eq!(
        parse_records_prefix(
            &input,
            1,
            0,
            Limits {
                max_prefix_bytes: 31,
                ..limits
            }
        )
        .unwrap_err(),
        Error::PrefixByteLimit
    );
    let mut too_big = affine(1, 1);
    too_big[32..36].copy_from_slice(&16_384_u32.to_le_bytes());
    too_big[44..48].copy_from_slice(&256_u32.to_le_bytes());
    let raised = Limits {
        max_prefix_bytes: usize::MAX,
        affine: tetrad_capture::affine::Limits {
            max_record_bytes: usize::MAX,
            ..limits.affine
        },
        ..limits
    };
    assert_eq!(
        parse_records_prefix(&too_big, 1, 0, raised).unwrap_err(),
        Error::Affine {
            ordinal: 0,
            offset: 0,
            source: tetrad_capture::Error::Limit
        }
    );
    let cumulative = [affine(4, 1), affine(7, 1), opaque(6)].concat();
    for (budget, expected) in [
        (
            219,
            Error::Affine {
                ordinal: 10,
                offset: 104,
                source: tetrad_capture::Error::Limit,
            },
        ),
        (220, Error::PrefixByteLimit),
    ] {
        assert_eq!(
            parse_records_prefix(
                &cumulative,
                3,
                9,
                Limits {
                    max_prefix_bytes: budget,
                    ..Limits::default()
                }
            )
            .unwrap_err(),
            expected,
        );
    }
    let exact = parse_records_prefix(
        &cumulative,
        3,
        9,
        Limits {
            max_prefix_bytes: 252,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(exact.consumed, 252);
    assert_eq!(exact.next_ordinal, Some(12));
    assert!(exact.remainder.is_empty());
    let mixed = [affine(4, 1), opaque(6)].concat();
    assert_eq!(
        parse_records_prefix(
            &mixed,
            2,
            7,
            Limits {
                max_prefix_bytes: 135,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::PrefixByteLimit
    );
}

#[test]
fn record_capacity_and_caller_lower_limits_are_checked_before_parsing() {
    let input = opaque(5).repeat(MAX_RECORDS);
    let parsed = parse_records_prefix(&input, MAX_RECORDS, 0, Limits::default()).unwrap();
    assert_eq!(parsed.records.len(), MAX_RECORDS);
    assert_eq!(
        parsed.records.last().unwrap().ordinal,
        (MAX_RECORDS - 1) as u32
    );
    assert_eq!(
        parse_records_prefix(
            &[],
            MAX_RECORDS + 1,
            0,
            Limits {
                max_records: usize::MAX,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::RecordLimit
    );
    assert_eq!(
        parse_records_prefix(
            &input,
            2,
            0,
            Limits {
                max_records: 1,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::RecordLimit
    );
}

#[test]
fn ordinal_exhaustion_and_zero_count_have_explicit_continuation_results() {
    let input = opaque(6);
    let parsed = parse_records_prefix(&input, 1, u32::MAX, Limits::default()).unwrap();
    assert_eq!(parsed.records[0].ordinal, u32::MAX);
    assert_eq!(parsed.next_ordinal, None);
    assert_eq!(
        parse_records_prefix(&input, 2, u32::MAX, Limits::default()).unwrap_err(),
        Error::OrdinalOverflow
    );
    let parsed = parse_records_prefix(
        &input,
        0,
        u32::MAX,
        Limits {
            max_records: 0,
            max_prefix_bytes: 0,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(parsed.records.is_empty());
    assert_eq!(parsed.next_ordinal, Some(u32::MAX));
    assert_eq!(parsed.remainder, input);
}

#[test]
fn affine_tensor_bits_and_opaque_intervals_are_preserved_without_numeric_execution() {
    let mut input = affine(4, 1);
    let bits = [0x7fc0_1234_u32, 0x8000_0000, 0x7f80_0000, 0x0000_0001];
    for (word, value) in input[76..92].chunks_exact_mut(4).zip(bits) {
        word.copy_from_slice(&value.to_le_bytes());
    }
    input[100..104].copy_from_slice(&0x7f80_1234_u32.to_le_bytes());
    let parsed = parse_records_prefix(&input, 1, 500, Limits::default()).unwrap();
    match &parsed.records[0].kind {
        RecordKind::Affine(record) => {
            assert_eq!(record.as_bytes(), input);
            assert_eq!(record.weights().map(f32::to_bits).collect::<Vec<_>>(), bits);
            assert_eq!(record.biases().next().unwrap().to_bits(), 0x7f80_1234);
            assert_eq!(&record.as_bytes()[92..100], &[0xab; 8]);
        }
        other => panic!("expected affine, got {other:?}"),
    }
}

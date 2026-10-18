use tetrad_preset::{Error, Limits, Preset, PresetDocument, Severity};

fn varint(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    while value > 127 {
        bytes.push(value as u8 | 128);
        value >>= 7;
    }
    bytes.push(value as u8);
    bytes
}

fn scalar(number: u32, value: u64) -> Vec<u8> {
    let mut bytes = varint(u64::from(number) << 3);
    bytes.extend(varint(value));
    bytes
}

fn bytes(number: u32, value: &[u8]) -> Vec<u8> {
    let mut bytes = varint(u64::from(number) << 3 | 2);
    bytes.extend(varint(value.len() as u64));
    bytes.extend_from_slice(value);
    bytes
}

fn float(number: u32, value: f32) -> Vec<u8> {
    let mut bytes = varint(u64::from(number) << 3 | 5);
    bytes.extend(value.to_le_bytes());
    bytes
}

fn decode(data: &[u8]) -> Preset {
    Preset::decode(data, Limits::default()).unwrap()
}

#[test]
fn typed_codec_follows_stock_parser_and_serializer_rules() {
    let split = [scalar(1, u64::MAX), scalar(2, 0)].concat();
    let expression = [float(2, -0.0), float(3, f32::NAN)].concat();
    let model = [bytes(3, &expression), scalar(1, 7)].concat();
    let chain = [bytes(6, &split), bytes(5, &model)].concat();
    let input = [
        bytes(2, b"a"),
        scalar(99, 5),      // Unknown field.
        scalar(2, 7),       // Known number, foreign wire type: unknown to stock.
        bytes(2, b"b"),     // Last occurrence wins.
        scalar(3, 1 << 32), // uint32 truncates to 0 but stays present.
        scalar(14, 5),      // Unpacked repeated scalars save packed.
        scalar(14, 6),
        // Map entries keep stored order (stock order is hash-seeded).
        bytes(32, &[scalar(1, 2), bytes(2, b"x")].concat()),
        bytes(32, &scalar(1, 1)), // Missing map value reads as "".
        bytes(11, &chain),
    ]
    .concat();
    let preset = decode(&input);
    assert_eq!(preset.name.as_deref(), Some("b"));
    assert_eq!(preset.hash, Some(0));
    assert_eq!(preset.scene_tempo, [5, 6]);
    assert_eq!(preset.chains[0].split_control_points[0].split, -1);

    // Implicit floats: -0.0 is not written, NaN is (`0x008aa928`).
    let expected_model = [scalar(1, 7), bytes(3, &float(3, f32::NAN))].concat();
    let expected_chain = [bytes(5, &expected_model), bytes(6, &scalar(1, u64::MAX))].concat();
    let expected = [
        bytes(2, b"b"),
        scalar(3, 0),
        bytes(11, &expected_chain),
        bytes(14, &[5, 6]),
        bytes(32, &[scalar(1, 2), bytes(2, b"x")].concat()),
        bytes(32, &[scalar(1, 1), bytes(2, b"")].concat()),
        scalar(99, 5),
        scalar(2, 7),
    ]
    .concat();
    assert_eq!(preset.to_bytes(), expected);
    assert_eq!(decode(&expected).to_bytes(), expected);
}

#[test]
fn typed_decode_refuses_inputs_the_stock_parser_rejects() {
    for input in [
        bytes(2, &[0xff]),  // Invalid UTF-8 string.
        bytes(14, &[0x80]), // Truncated packed varint.
        bytes(11, &bytes(5, &bytes(2, &bytes(5, &bytes(3, &[0xc3]))))),
    ] {
        let document = PresetDocument::parse(&input, Limits::default()).unwrap();
        assert!(matches!(
            Preset::try_from(&document),
            Err(Error::InvalidKnownField { .. })
        ));
    }
}

#[test]
fn modify_keeps_untouched_bytes_and_is_transactional() {
    // Noncanonical input: duplicate name and an overlong hash varint.
    let input = [bytes(2, b"a"), bytes(2, b"b"), vec![0x18, 0x81, 0x00]].concat();
    let mut document = PresetDocument::parse(&input, Limits::default()).unwrap();
    document.modify(|_| Ok(())).unwrap();
    assert_eq!(document.to_bytes(), input);

    let refused = document.modify(|preset| {
        preset.name = Some("lost".into());
        Err::<(), _>(Error::Unsupported("synthetic"))
    });
    assert!(refused.is_err());
    assert_eq!(document.to_bytes(), input);

    let mut small = PresetDocument::parse(
        &input,
        Limits {
            max_bytes: input.len(),
            ..Limits::default()
        },
    )
    .unwrap();
    let grown = small.modify(|preset| {
        preset.name = Some("a much longer name".into());
        Ok(())
    });
    assert!(matches!(grown, Err(Error::Limit { .. })));
    assert_eq!(small.to_bytes(), input);

    document
        .modify(|preset| {
            preset.name = Some("c".into());
            Ok(())
        })
        .unwrap();
    assert_eq!(document.to_bytes(), [bytes(2, b"c"), scalar(3, 1)].concat());
}

#[test]
fn validation_accepts_only_the_stock_default_nan_parameter_pattern() {
    let value = |bits: u32| {
        let input = bytes(
            11,
            &bytes(5, &bytes(2, &bytes(5, &float(2, f32::from_bits(bits))))),
        );
        PresetDocument::parse(&input, Limits::default())
            .unwrap()
            .validate()
            .unwrap()
            .into_iter()
            .find(|d| d.path.ends_with("float_value[0]"))
            .unwrap()
    };
    let stock = value(0x7fc0_0000);
    assert_eq!(
        (stock.severity, stock.code),
        (Severity::Warning, "stock_default_nan")
    );
    for bits in [0x7fc0_0123, 0xffc0_0000, 0x7f80_0000] {
        let diagnostic = value(bits);
        assert_eq!(
            (diagnostic.severity, diagnostic.code),
            (Severity::Error, "non_finite_float")
        );
    }
    let volume = PresetDocument::parse(&float(5, f32::from_bits(0x7fc0_0000)), Limits::default())
        .unwrap()
        .validate()
        .unwrap();
    assert!(
        volume
            .iter()
            .any(|d| d.code == "non_finite_float" && d.severity == Severity::Error)
    );
}

#[test]
fn validation_flags_inputs_where_the_stock_loader_leaves_its_bounds() {
    let codes = |data: &[u8]| {
        PresetDocument::parse(data, Limits::default())
            .unwrap()
            .validate()
            .unwrap()
            .into_iter()
            .map(|d| (d.code, d.severity))
            .collect::<Vec<_>>()
    };
    let colors = bytes(31, &[1; 9]);
    assert!(codes(&colors).contains(&("stock_scene_overflow", Severity::Error)));
    let values: Vec<u8> = (0..9).flat_map(|_| bytes(5, &float(2, 0.5))).collect();
    let param = bytes(11, &bytes(5, &bytes(2, &values)));
    assert!(codes(&param).contains(&("stock_scene_overflow", Severity::Error)));
    let empty_value = bytes(11, &bytes(5, &bytes(2, &bytes(5, &[]))));
    assert!(codes(&empty_value).contains(&("stock_empty_value", Severity::Warning)));
    assert!(codes(&bytes(22, b"2.0.0")).contains(&("stock_missing_version", Severity::Error)));
    assert!(
        codes(&bytes(26, b"text"))
            .contains(&("stock_description_without_cloud_id", Severity::Error))
    );
    assert!(codes(&[]).contains(&("stock_short_scene_labels", Severity::Warning)));
}

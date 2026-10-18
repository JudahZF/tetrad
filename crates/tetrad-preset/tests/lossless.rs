use tetrad_preset::{
    Error, Limits, ParamValue, PresetDocument, Severity,
    schema::{self, Kind},
};

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

fn preset(value: &[u8]) -> Vec<u8> {
    // Positional chain/model/parameter/scene lists without coordinates or IDs.
    bytes(11, &bytes(5, &bytes(2, &bytes(5, value))))
}

fn parse(data: &[u8]) -> PresetDocument {
    PresetDocument::parse(data, Limits::default()).unwrap()
}

#[test]
fn edit_budget_accounts_for_shrinking_ancestor_prefixes() {
    let data = [
        0x5a, 0x8c, 0, 0x2a, 0x89, 0, 0x12, 0x86, 0, 0x2a, 0x83, 0, 0x88, 0, 1,
    ];
    let mut document = PresetDocument::parse(
        &data,
        Limits {
            max_bytes: data.len(),
            ..Limits::default()
        },
    )
    .unwrap();
    document
        .set_parameter(0, 0, 0, 0, ParamValue::Int(128))
        .unwrap();
    let expected = [
        0x5a, 0x8c, 0, 0x2a, 0x89, 0, 0x12, 0x86, 0, 0x2a, 4, 0x88, 0, 0x80, 1,
    ];
    assert_eq!(document.to_bytes(), expected);
    let saved = document.to_bytes();
    assert!(matches!(
        document.set_parameter(0, 0, 0, 0, ParamValue::String("too large".into())),
        Err(Error::Limit { .. })
    ));
    assert_eq!(document.to_bytes(), saved);
}

#[test]
fn no_op_preserves_order_duplicates_defaults_packed_maps_and_groups() {
    let mut data = bytes(2, b"earlier");
    // Overlong tag, length and scalar value encodings remain byte-identical.
    data.extend([0x92, 0, 0x81, 0, b'z']);
    data.extend([24, 0x80, 0]);
    data.extend(scalar(36, 0));
    data.extend(bytes(14, &[1, 2]));
    data.extend(scalar(14, 3));
    data.extend(bytes(32, &[8, 0, 18, 0]));
    data.extend([
        0xa3, 6, 8, 0x81, 0, 0xab, 6, 0x15, 1, 2, 3, 4, 0xac, 6, 0xa4, 6,
    ]);
    data.extend(bytes(100, &[0, 255, 128]));
    let mut doc = parse(&data);
    let summary = doc.summary().unwrap();
    assert_eq!(summary.name.as_deref(), Some("z"));
    assert_eq!(summary.hash, Some(0));
    assert_eq!(summary.side_chain_follow_exists, Some(false));
    assert_eq!(summary.scene_count, 3);
    assert_eq!(summary.scenes[2].tempo, Some(3));
    assert_eq!(
        summary
            .presence
            .iter()
            .find(|p| p.number == 2)
            .unwrap()
            .occurrences,
        2
    );
    assert_eq!(summary.unknown_field_count, 5);
    doc.rename("z").unwrap();
    assert_eq!(doc.to_bytes(), data);
}

#[test]
fn edits_change_only_selected_occurrence_and_required_ancestor_lengths() {
    let mut root = bytes(2, b"first");
    root.extend(bytes(100, b"opaque"));
    root.extend(bytes(2, b"last"));
    root.extend(bytes(15, b"A"));
    root.extend(bytes(15, b"B"));
    let mut doc = parse(&root);
    doc.rename("next").unwrap();
    doc.set_scene_label(1, "longer").unwrap();
    let expected = [
        bytes(2, b"first"),
        bytes(100, b"opaque"),
        bytes(2, b"next"),
        bytes(15, b"A"),
        bytes(15, b"longer"),
    ]
    .concat();
    assert_eq!(doc.to_bytes(), expected);

    // Same-size child edit must preserve every overlong ancestor length prefix.
    let nested = vec![
        0x5a, 0x8d, 0, 0x2a, 0x8a, 0, 0x12, 0x87, 0, 0x2a, 0x84, 0, 0x88, 0, 0x80, 0,
    ];
    let mut doc = parse(&nested);
    doc.set_parameter(0, 0, 0, 0, ParamValue::Int(1)).unwrap();
    // Overlong leaf varint is replaced, so all ancestor lengths must shrink.
    assert_eq!(
        doc.summary().unwrap().chains[0].models[0].params[0].values,
        vec![Some(ParamValue::Int(1))]
    );
    assert_eq!(doc.to_bytes(), preset(&[0x88, 0, 1]));
    let original = [
        0x5a, 0x8c, 0, 0x2a, 0x89, 0, 0x12, 0x86, 0, 0x2a, 0x83, 0, 0x88, 0, 1,
    ];
    let mut doc = parse(&original);
    doc.set_parameter(0, 0, 0, 0, ParamValue::Int(2)).unwrap();
    let mut expected = original;
    expected[14] = 2;
    assert_eq!(doc.to_bytes(), expected);

    // Cross 7-bit length-prefix boundaries at the leaf and its ancestors.
    for length in [1, 119, 120, 127, 128, 255] {
        let unknown = bytes(101, b"opaque");
        let old = preset(&[unknown.clone(), bytes(3, b"old")].concat());
        let new_value = "x".repeat(length);
        let mut doc = parse(&old);
        doc.set_parameter(0, 0, 0, 0, ParamValue::String(new_value.clone()))
            .unwrap();
        assert_eq!(
            doc.to_bytes(),
            preset(&[unknown, bytes(3, new_value.as_bytes())].concat())
        );
    }
}

#[test]
fn oneof_edits_preserve_prior_alternatives_unknowns_and_empty_presence() {
    let value = [scalar(1, 7), bytes(101, b"opaque"), float(2, 0.5)].concat();
    let mut doc = parse(&preset(&value));
    doc.set_parameter(0, 0, 0, 0, ParamValue::String("text".into()))
        .unwrap();
    let expected_value = [scalar(1, 7), bytes(101, b"opaque"), bytes(3, b"text")].concat();
    assert_eq!(doc.to_bytes(), preset(&expected_value));
    assert_eq!(
        doc.summary().unwrap().chains[0].models[0].params[0].values,
        vec![Some(ParamValue::String("text".into()))]
    );
    assert_eq!(doc.summary().unwrap().unknown_field_count, 1);
    assert!(
        doc.validate()
            .unwrap()
            .iter()
            .any(|d| d.code == "oneof_overrides" && d.severity == Severity::Info)
    );
    doc.set_parameter(0, 0, 0, 0, ParamValue::Int(-1)).unwrap();
    assert_eq!(
        doc.to_bytes(),
        preset(&[scalar(1, 7), bytes(101, b"opaque"), scalar(1, u64::MAX)].concat())
    );

    let mut empty = parse(&preset(&bytes(101, b"unknown")));
    assert_eq!(
        empty.summary().unwrap().chains[0].models[0].params[0].values,
        vec![None]
    );
    empty
        .set_parameter(0, 0, 0, 0, ParamValue::Float(0.0))
        .unwrap();
    assert_eq!(
        empty.to_bytes(),
        preset(&[bytes(101, b"unknown"), float(2, 0.0)].concat())
    );
}

#[test]
fn equal_values_and_signed_zero_have_defined_edit_behavior() {
    let original = preset(&[8, 0x80, 0]);
    let mut doc = parse(&original);
    doc.set_parameter(0, 0, 0, 0, ParamValue::Int(0)).unwrap();
    assert_eq!(doc.to_bytes(), original);
    let original = preset(&float(2, -0.0));
    let mut doc = parse(&original);
    doc.set_parameter(0, 0, 0, 0, ParamValue::Float(-0.0))
        .unwrap();
    assert_eq!(doc.to_bytes(), original);
    doc.set_parameter(0, 0, 0, 0, ParamValue::Float(0.0))
        .unwrap();
    assert_eq!(doc.to_bytes(), preset(&float(2, 0.0)));
}

#[test]
fn wire_errors_and_resource_bounds_fail_before_editing() {
    for data in [
        vec![0],
        vec![8, 128],
        vec![18, 2, 1],
        vec![14],
        vec![15],
        vec![12],
        vec![11, 20],
        vec![11],
        bytes(11, &[0]),
    ] {
        assert!(PresetDocument::parse(&data, Limits::default()).is_err());
    }
    let overflow = [vec![8], vec![255; 9], vec![2]].concat();
    assert!(matches!(
        PresetDocument::parse(&overflow, Limits::default()),
        Err(Error::VarintOverflow { .. })
    ));
    let tag_overflow = varint(1_u64 << 32);
    assert!(matches!(
        PresetDocument::parse(&tag_overflow, Limits::default()),
        Err(Error::InvalidFieldNumber { .. })
    ));
    assert!(
        PresetDocument::parse(
            &bytes(11, &[]),
            Limits {
                max_depth: 0,
                ..Limits::default()
            }
        )
        .is_err()
    );
    assert!(
        PresetDocument::parse(
            &[11, 19, 20, 12],
            Limits {
                max_depth: 1,
                ..Limits::default()
            }
        )
        .is_err()
    );
    assert!(
        PresetDocument::parse(
            &[8, 0, 16, 0],
            Limits {
                max_fields: 1,
                ..Limits::default()
            }
        )
        .is_err()
    );
    assert!(
        PresetDocument::parse(
            &[8, 0],
            Limits {
                max_bytes: 1,
                ..Limits::default()
            }
        )
        .is_err()
    );
    assert!(
        PresetDocument::parse(
            &bytes(14, &[0, 1, 2]),
            Limits {
                max_fields: 3,
                ..Limits::default()
            }
        )
        .is_err()
    );
    assert!(
        PresetDocument::parse(
            &bytes(14, &[0, 1, 2]),
            Limits {
                max_fields: 4,
                ..Limits::default()
            }
        )
        .is_ok()
    );
}

#[test]
fn diagnostics_separate_schema_errors_from_preserved_wire() {
    let data = [
        scalar(2, 0),
        scalar(3, 1_u64 << 40),
        bytes(14, &[255]),
        float(5, f32::INFINITY),
        bytes(15, &[255]),
        scalar(7, 8),
        bytes(22, b"1"),
        scalar(36, 2),
    ]
    .concat();
    let doc = parse(&data);
    assert_eq!(doc.to_bytes(), data);
    assert!(doc.summary().is_err());
    let diagnostics = doc.validate().unwrap();
    for code in [
        "known_wire_type",
        "uint32_overflow",
        "invalid_packed_varint",
        "non_finite_float",
        "invalid_utf8",
        "default_scene_range",
    ] {
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == code && d.severity == Severity::Error),
            "missing {code}"
        );
    }
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == "partial_version" && d.severity == Severity::Warning)
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == "noncanonical_bool" && d.severity == Severity::Warning)
    );
    let full_versions = [
        bytes(22, b"1.0.3"),
        bytes(23, b"4.1.0"),
        bytes(24, b"1.0.3"),
    ]
    .concat();
    assert!(
        !parse(&full_versions)
            .validate()
            .unwrap()
            .iter()
            .any(|d| d.code == "partial_version")
    );
}

#[test]
fn failed_edits_are_atomic_and_do_not_extend_positional_lists() {
    let original = preset(&scalar(1, 0));
    let mut doc = PresetDocument::parse(
        &original,
        Limits {
            max_bytes: original.len(),
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(
        doc.set_parameter(0, 0, 0, 0, ParamValue::String("too large".into()))
            .is_err()
    );
    assert!(
        doc.set_parameter(0, 0, 0, 0, ParamValue::Float(f32::NAN))
            .is_err()
    );
    assert!(doc.set_parameter(1, 0, 0, 0, ParamValue::Int(2)).is_err());
    assert!(doc.set_parameter(0, 0, 0, 1, ParamValue::Int(2)).is_err());
    assert!(doc.set_scene_label(0, "new").is_err());
    assert_eq!(doc.to_bytes(), original);
    let original = scalar(100, 0);
    let mut doc = PresetDocument::parse(
        &original,
        Limits {
            max_fields: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(doc.rename("new").is_err());
    assert_eq!(doc.to_bytes(), original);
    let mut doc = parse(&[]);
    doc.rename("").unwrap();
    assert_eq!(doc.to_bytes(), [18, 0]);
    assert_eq!(doc.summary().unwrap().name.as_deref(), Some(""));
}

#[test]
fn scene_color_edits_preserve_equal_encodings_tags_zero_and_unrelated_fields() {
    let prefix = [bytes(100, b"opaque"), bytes(15, b"label")].concat();
    let color = [0xf8, 0x81, 0, 0x81, 0]; // Overlong field-31 tag and value 1.
    let suffix = [scalar(31, 9), bytes(14, &[0x80, 0])].concat();
    let original = [prefix.clone(), color.to_vec(), suffix.clone()].concat();
    let mut doc = parse(&original);
    doc.set_scene_color(0, 1).unwrap();
    assert_eq!(doc.to_bytes(), original);
    doc.set_scene_color(1, 9).unwrap();
    assert_eq!(doc.to_bytes(), original);
    doc.set_scene_color(0, 0).unwrap();
    let expected = [prefix, vec![0xf8, 0x81, 0, 0], suffix].concat();
    assert_eq!(doc.to_bytes(), expected);
    assert_eq!(doc.summary().unwrap().scenes[0].color, Some(0));
    doc.set_scene_color(0, 0).unwrap();
    assert_eq!(doc.to_bytes(), expected);
    doc.set_scene_color(1, u32::MAX).unwrap();
    assert_eq!(doc.summary().unwrap().scenes[1].color, Some(u32::MAX));
    let saved = doc.to_bytes();
    assert!(matches!(
        doc.set_scene_color(2, 0),
        Err(Error::Index {
            kind: "scene color",
            index: 2,
            len: 2
        })
    ));
    assert_eq!(doc.to_bytes(), saved);
    let mut empty = parse(&[]);
    assert!(matches!(
        empty.set_scene_color(0, 0),
        Err(Error::Index { len: 0, .. })
    ));
    assert_eq!(empty.to_bytes(), Vec::<u8>::new());
}

#[test]
fn scene_color_editor_refuses_packed_mixed_invalid_and_over_budget_without_writes() {
    for original in [
        bytes(31, &[1, 2]),
        [scalar(31, 1), bytes(31, &[2, 3]), scalar(31, 4)].concat(),
        [scalar(31, 1), bytes(31, &[])].concat(),
    ] {
        let mut doc = parse(&original);
        for (index, color) in [(0, 1), (0, 0), (1, 8), (99, 8)] {
            assert!(matches!(
                doc.set_scene_color(index, color),
                Err(Error::InvalidKnownField {
                    message: "BinaryPreset",
                    field: 31,
                    reason: "packed scene colors are unsupported by the positional color editor",
                })
            ));
            assert_eq!(doc.to_bytes(), original);
        }
    }
    for (original, reason) in [
        (scalar(31, u64::from(u32::MAX) + 1), "uint32 overflow"),
        (float(31, 1.0), "wrong wire type"),
    ] {
        let mut doc = parse(&original);
        assert!(matches!(doc.set_scene_color(0, 0),
            Err(Error::InvalidKnownField { message: "BinaryPreset", field: 31, reason: got })
                if got == reason));
        assert_eq!(doc.to_bytes(), original);
    }
    let original = [bytes(100, b"opaque"), scalar(31, 0)].concat();
    let mut doc = PresetDocument::parse(
        &original,
        Limits {
            max_bytes: original.len(),
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        doc.set_scene_color(0, 128),
        Err(Error::Limit { kind: "bytes", .. })
    ));
    assert_eq!(doc.to_bytes(), original);
    doc.set_scene_color(0, 127).unwrap();
    assert_eq!(
        doc.to_bytes(),
        [bytes(100, b"opaque"), scalar(31, 127)].concat()
    );
}

#[test]
fn schema_is_complete_and_summary_diagnostics_are_serializable() {
    let all = schema::messages();
    assert_eq!(all.len(), 18);
    let root = all.iter().find(|m| m.name == "BinaryPreset").unwrap();
    assert_eq!(
        root.fields.iter().map(|f| f.number).collect::<Vec<_>>(),
        (2..=37).collect::<Vec<_>>()
    );
    for message in all {
        for field in message.fields {
            if let Kind::Message(reference) = field.kind {
                assert!(all.iter().any(|m| m.name == reference));
            }
        }
    }
    for name in [
        "BinaryPreset.StompLabelsEntry",
        "BinaryPreset.SingleStompLabelsEntry",
        "BinaryPreset.StompIsMomentaryEntry",
    ] {
        assert_eq!(all.iter().find(|m| m.name == name).unwrap().fields.len(), 2);
    }
    let model = all.iter().find(|m| m.name == "Model").unwrap();
    assert!(
        model
            .fields
            .iter()
            .filter(|f| (6..=8).contains(&f.number))
            .all(|f| f.optional)
    );
    fn serializable<T: serde::Serialize>(_: &T) {}
    serializable(&parse(&[]).summary().unwrap());
    serializable(&parse(&[]).validate().unwrap());
}

#[test]
fn split_points_preserve_sentinels_absence_duplicates_and_exceptional_integers() {
    let duplicate = [scalar(1, 0), bytes(100, b"opaque"), scalar(1, u64::MAX)].concat();
    let points = [
        bytes(6, &[]),
        bytes(6, &scalar(1, 0)),
        bytes(6, &scalar(2, u64::MAX)),
        bytes(6, &duplicate),
        bytes(6, &[scalar(1, u64::MAX), scalar(2, u64::MAX)].concat()),
        bytes(
            6,
            &[
                scalar(1, i64::from(i32::MIN) as u64),
                scalar(2, i32::MAX as u64),
            ]
            .concat(),
        ),
        bytes(6, &[scalar(1, u64::from(u32::MAX)), scalar(2, 8)].concat()),
        bytes(6, &scalar(1, (1_u64 << 32) + 7)),
    ]
    .concat();
    let original = [bytes(11, &points), bytes(11, &[])].concat();
    let doc = parse(&original);
    let summary = doc.summary().unwrap();
    let points = &summary.chains[0].split_control_points;
    assert_eq!(points.len(), 8);
    for (position, point) in points.iter().enumerate() {
        assert_eq!(point.position, position);
    }
    assert_eq!(points[0].split, None);
    assert_eq!(points[0].mix, None);
    assert!(points[0].presence.is_empty());
    assert_eq!(points[1].split, Some(0));
    assert_eq!(points[1].mix, None);
    assert!(!points[1].presence[0].optional);
    assert_eq!(points[2].split, None);
    assert_eq!(points[2].mix, Some(-1));
    assert_eq!(points[3].split, Some(-1));
    assert_eq!(points[3].presence[0].occurrences, 2);
    assert_eq!(points[3].presence[1].name, None);
    assert_eq!(points[3].unknown_field_count, 1);
    assert_eq!(points[4].split, Some(-1));
    assert_eq!(points[4].mix, Some(-1));
    assert_eq!(points[5].split, Some(i32::MIN));
    assert_eq!(points[5].mix, Some(i32::MAX));
    assert_eq!(points[6].split, Some(-1));
    assert_eq!(points[6].mix, Some(8));
    assert_eq!(points[7].split, Some(7));
    assert!(summary.chains[1].split_control_points.is_empty());
    assert_eq!(summary.scene_count, 0); // Points are not a scene list.
    assert_eq!(summary.unknown_field_count, 1);
    assert!(doc.validate().unwrap().iter().any(|d| {
        d.code == "int32_truncation" && d.path.contains("split_control_points[7].split")
    }));
    assert_eq!(doc.to_bytes(), original);
}

#[test]
fn column_bypass_preserves_sparse_scene_data_and_stored_positions_not_coordinates() {
    let duplicate_bool = [scalar(1, 1), bytes(100, b"opaque"), scalar(1, 0)].concat();
    let column = [
        scalar(2, 7),
        bytes(1, &[]),
        scalar(3, 1),
        bytes(1, &scalar(1, 0)),
        bytes(1, &scalar(1, 1)),
        scalar(3, 0),
        bytes(1, &duplicate_bool),
        scalar(100, 9),
        scalar(2, 0),
    ]
    .concat();
    let row = [
        scalar(2, 3),
        bytes(1, &column),
        bytes(1, &[]),
        bytes(1, &[scalar(2, u64::from(u32::MAX)), scalar(3, 2)].concat()),
        scalar(2, 0),
    ]
    .concat();
    let original = [bytes(18, &row), bytes(18, &[])].concat();
    let doc = parse(&original);
    let summary = doc.summary().unwrap();
    assert_eq!(summary.bypass.len(), 2);
    let row = &summary.bypass[0];
    assert_eq!(row.position, 0);
    assert_eq!(row.row, Some(0));
    assert_eq!(row.presence[0].occurrences, 2);
    assert_eq!(row.unknown_field_count, 2);
    assert_eq!(row.col_bypass.len(), 3);
    let column = &row.col_bypass[0];
    assert_eq!(column.position, 0);
    assert_eq!(column.column, Some(0));
    assert_eq!(column.scene_mode, Some(false));
    assert_eq!(column.presence[0].occurrences, 2);
    assert_eq!(column.presence[2].occurrences, 2);
    assert_eq!(column.unknown_field_count, 2);
    assert_eq!(column.scene_bypass.len(), 4);
    for (position, scene) in column.scene_bypass.iter().enumerate() {
        assert_eq!(scene.position, position);
    }
    assert_eq!(column.scene_bypass[0].bypass, None);
    assert!(column.scene_bypass[0].presence.is_empty());
    assert_eq!(column.scene_bypass[1].bypass, Some(false));
    assert!(!column.scene_bypass[1].presence[0].optional);
    assert_eq!(column.scene_bypass[2].bypass, Some(true));
    assert_eq!(column.scene_bypass[3].bypass, Some(false));
    assert_eq!(column.scene_bypass[3].presence[0].occurrences, 2);
    assert_eq!(column.scene_bypass[3].unknown_field_count, 1);
    let empty_column = &row.col_bypass[1];
    assert_eq!(empty_column.position, 1);
    assert_eq!(empty_column.column, None);
    assert_eq!(empty_column.scene_mode, None);
    assert!(empty_column.scene_bypass.is_empty());
    assert_eq!(row.col_bypass[2].position, 2);
    assert_eq!(row.col_bypass[2].column, Some(u32::MAX));
    assert_eq!(row.col_bypass[2].scene_mode, Some(true));
    assert_eq!(summary.bypass[1].position, 1);
    assert_eq!(summary.bypass[1].row, None);
    assert!(summary.bypass[1].col_bypass.is_empty());
    assert_eq!(summary.scene_count, 4);
    assert!(
        summary
            .scenes
            .iter()
            .all(|s| { s.label.is_none() && s.tempo.is_none() && s.color.is_none() })
    );
    assert_eq!(summary.scene_value_count, 0);
    assert_eq!(summary.unknown_field_count, 2);
    let diagnostics = doc.validate().unwrap();
    assert!(diagnostics.iter().any(|d| d.code == "noncanonical_bool"));
    assert!(diagnostics.iter().any(|d| d.code == "coordinate_range"));
    assert_eq!(doc.to_bytes(), original);
}

#[test]
fn split_mix_bypass_keeps_empty_messages_noncanonical_bools_and_unknown_groups() {
    let unknown_group = [0xa3, 6, 8, 1, 0xa4, 6];
    let chain = [
        bytes(10, &[]),
        bytes(11, &scalar(1, 1)),
        bytes(10, &scalar(1, 0)),
        bytes(11, &[scalar(1, 0), scalar(1, 2)].concat()),
        bytes(10, &unknown_group),
    ]
    .concat();
    let original = [bytes(11, &chain), bytes(11, &[])].concat();
    let doc = parse(&original);
    let summary = doc.summary().unwrap();
    let chain = &summary.chains[0];
    assert_eq!(chain.split_bypass.len(), 3);
    assert_eq!(chain.mix_bypass.len(), 2);
    assert_eq!(chain.split_bypass[0].position, 0);
    assert_eq!(chain.split_bypass[0].bypass, None);
    assert_eq!(chain.split_bypass[1].position, 1);
    assert_eq!(chain.split_bypass[1].bypass, Some(false));
    assert_eq!(chain.split_bypass[2].position, 2);
    assert_eq!(chain.split_bypass[2].bypass, None);
    assert_eq!(chain.split_bypass[2].presence[0].wire_types, vec![3]);
    assert_eq!(chain.split_bypass[2].unknown_field_count, 2);
    assert_eq!(chain.mix_bypass[0].bypass, Some(true));
    assert_eq!(chain.mix_bypass[1].bypass, Some(true));
    assert_eq!(chain.mix_bypass[1].presence[0].occurrences, 2);
    assert!(summary.chains[1].split_bypass.is_empty());
    assert!(summary.chains[1].mix_bypass.is_empty());
    assert_eq!(summary.scene_count, 3);
    assert_eq!(summary.scene_value_count, 0);
    assert_eq!(summary.unknown_field_count, 2);
    assert!(
        doc.validate().unwrap().iter().any(|d| {
            d.code == "noncanonical_bool" && d.path.contains("mixBypass[1].bypass[1]")
        })
    );
    assert_eq!(doc.to_bytes(), original);
}

#[test]
fn routing_summary_errors_leave_invalid_wire_preserved_for_diagnostics() {
    for (original, message, field, reason) in [
        (bytes(11, &scalar(6, 0)), "Chain", 6, "wrong wire type"),
        (
            bytes(11, &bytes(6, &float(1, 0.0))),
            "SplitControlPoints",
            1,
            "wrong wire type",
        ),
        (bytes(11, &scalar(10, 0)), "Chain", 10, "wrong wire type"),
        (
            bytes(11, &bytes(11, &float(1, 0.0))),
            "SceneBypass",
            1,
            "wrong wire type",
        ),
        (scalar(18, 0), "BinaryPreset", 18, "wrong wire type"),
        (bytes(18, &scalar(1, 0)), "Bypass", 1, "wrong wire type"),
        (
            bytes(18, &scalar(2, 1_u64 << 32)),
            "Bypass",
            2,
            "uint32 overflow",
        ),
        (
            bytes(18, &bytes(1, &scalar(2, 1_u64 << 32))),
            "ColBypass",
            2,
            "uint32 overflow",
        ),
        (
            bytes(18, &bytes(1, &float(3, 0.0))),
            "ColBypass",
            3,
            "wrong wire type",
        ),
        (
            bytes(18, &bytes(1, &bytes(1, &float(1, 0.0)))),
            "SceneBypass",
            1,
            "wrong wire type",
        ),
    ] {
        let doc = parse(&original);
        assert!(matches!(doc.summary(),
            Err(Error::InvalidKnownField { message: m, field: f, reason: r })
                if m == message && f == field && r == reason));
        assert!(doc.validate().unwrap().iter().any(|d| {
            d.severity == Severity::Error && matches!(d.code, "known_wire_type" | "uint32_overflow")
        }));
        assert_eq!(doc.to_bytes(), original);
    }
}

#[test]
fn routing_summary_allocations_follow_field_budgets_not_coordinate_values() {
    // Each outer row/column/scene message consumes one field, even if empty.
    let original = bytes(
        18,
        &[
            scalar(2, u64::from(u32::MAX)),
            bytes(
                1,
                &[
                    scalar(2, u64::from(u32::MAX)),
                    bytes(1, &[]),
                    bytes(1, &[]),
                    bytes(1, &[]),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    assert!(matches!(
        PresetDocument::parse(
            &original,
            Limits {
                max_fields: 6,
                ..Limits::default()
            }
        ),
        Err(Error::Limit {
            kind: "fields",
            limit: 6
        })
    ));
    let doc = PresetDocument::parse(
        &original,
        Limits {
            max_fields: 7,
            ..Limits::default()
        },
    )
    .unwrap();
    let summary = doc.summary().unwrap();
    assert_eq!(summary.wire_field_count, 7);
    assert_eq!(summary.bypass.len(), 1);
    assert_eq!(summary.bypass[0].col_bypass.len(), 1);
    assert_eq!(summary.bypass[0].col_bypass[0].scene_bypass.len(), 3);
    assert_eq!(summary.scene_count, 3);
    assert_eq!(doc.to_bytes(), original);
    assert!(matches!(
        PresetDocument::parse(
            &original,
            Limits {
                max_depth: 2,
                ..Limits::default()
            }
        ),
        Err(Error::Limit {
            kind: "depth",
            limit: 2
        })
    ));
    let original = bytes(
        11,
        &[bytes(6, &[]), bytes(10, &[]), bytes(11, &[])].concat(),
    );
    assert!(matches!(
        PresetDocument::parse(
            &original,
            Limits {
                max_fields: 3,
                ..Limits::default()
            }
        ),
        Err(Error::Limit {
            kind: "fields",
            limit: 3
        })
    ));
    let summary = PresetDocument::parse(
        &original,
        Limits {
            max_fields: 4,
            ..Limits::default()
        },
    )
    .unwrap()
    .summary()
    .unwrap();
    assert_eq!(summary.chains[0].split_control_points.len(), 1);
    assert_eq!(summary.chains[0].split_bypass.len(), 1);
    assert_eq!(summary.chains[0].mix_bypass.len(), 1);
    assert_eq!(summary.scene_count, 1);
}

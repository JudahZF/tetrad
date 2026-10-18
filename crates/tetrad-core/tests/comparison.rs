use tetrad_core::comparison::numeric::*;
use tetrad_core::comparison::*;

const POLICY: ComparisonPolicy = ComparisonPolicy::DefaultQtHandlerOrdinaryIeee;
fn float(value: f32) -> F32 {
    F32::from_bits(value.to_bits()).unwrap()
}
fn double(value: f64) -> F64 {
    F64::from_bits(value.to_bits()).unwrap()
}
fn parameter(fields: ParameterFields) -> Parameter {
    Parameter::new(fields).unwrap()
}
fn empty_model() -> Model {
    Model::new(ModelFields::default()).unwrap()
}
fn model_with(fields: ParameterFields) -> Model {
    Model::new(ModelFields {
        descriptors28: vec![parameter(fields)],
        second52: vec![vec![Variant::Int32(7)]; SCENES],
        ..ModelFields::default()
    })
    .unwrap()
}
fn fixture() -> PresetFields {
    let model = empty_model();
    PresetFields {
        grid0: vec![vec![model.clone(), model.clone()], vec![model.clone()]],
        graph12: vec![
            GraphNode {
                opaque_word16: 3,
                ..GraphNode::default()
            },
            GraphNode {
                opaque_word16: 8,
                ..GraphNode::default()
            },
        ],
        paired36: vec![PairedRecord {
            opaque_word0: 4,
            opaque_word4: 5,
            model8: model.clone(),
            model272: model.clone(),
        }],
        strings48: vec![Utf16::from_literal("a\0b").unwrap(), Utf16::null()],
        map5344: TraversalMap::from_entries(vec![MapEntry {
            key16: 7,
            value: WordMapValue::default(),
        }])
        .unwrap(),
        model_map5368: TraversalMap::from_entries(vec![MapEntry {
            key16: 9,
            value: ModelPair {
                model20: model.clone(),
                model284: model.clone(),
            },
        }])
        .unwrap(),
        opaque_float5396: float(1.0),
        opaque_float5400: float(2.0),
        default_scene5408: 0,
        assignments64: [Assignment::default(); ASSIGNMENTS],
        assignment_groups544: [[Assignment::default(); ASSIGNMENTS]; ASSIGNMENT_GROUPS],
        scene_names5412: vec![Utf16::from_literal("Scene").unwrap()],
        group_labels5424: TraversalMap::from_entries(vec![MapEntry {
            key16: 1,
            value: Utf16::from_literal("Group").unwrap(),
        }])
        .unwrap(),
        individual_labels5448: TraversalMap::from_entries(vec![MapEntry {
            key16: 2,
            value: Utf16::from_literal("Stomp").unwrap(),
        }])
        .unwrap(),
        momentary5472: TraversalMap::from_entries(vec![MapEntry { key16: 4, value: 0 }]).unwrap(),
        colors5496: vec![1, 2, 1],
        expression5508: model,
        opaque_string5800: Utf16::default(),
        opaque_string5820: Utf16::default(),
        opaque_byte5825: 0,
        omitted: PresetOmissions::default(),
    }
}
fn snapshot(fields: PresetFields) -> AdmittedSnapshot {
    AdmittedSnapshot::new(fields).unwrap()
}
fn mismatch(result: Result<Comparison, Unsupported>, expected: &str) {
    match result.unwrap() {
        Comparison::Mismatch { path } => assert_eq!(path.to_string(), expected),
        Comparison::Equal => panic!("expected mismatch at {expected}"),
    }
}

#[test]
fn full_snapshot_comparison_is_pure_and_omissions_are_owned() {
    let lhs = snapshot(fixture());
    let before = format!("{lhs:?}");
    let mut rhs = lhs.fields().clone();
    rhs.omitted.identity5404 = 99;
    rhs.omitted.name_key5392 = Utf16::from_literal("different storage key").unwrap();
    rhs.omitted.selected_scene = Some(7);
    rhs.omitted.byte5824 = 8;
    rhs.omitted.string5816 = Utf16::from_literal("excluded").unwrap();
    rhs.assignments64[0].omitted_timestamp24 = u64::MAX;
    rhs.assignment_groups544[9][11].omitted_timestamp24 = 7;
    let mut m = rhs.grid0[0][0].fields().clone();
    m.omitted.opaque_byte252 = 1;
    m.omitted.primary40 = Some(std::array::from_fn(|_| {
        vec![Variant::UnsupportedTypeId(999)]
    }));
    m.omitted.primary_cache64 = Some([float(f32::NAN); SCENES]);
    rhs.grid0[0][0] = Model::new(m).unwrap();
    assert_eq!(
        compare(&lhs, &snapshot(rhs), POLICY).unwrap(),
        Comparison::Equal
    );
    assert_eq!(format!("{lhs:?}"), before);
    let mut rhs = lhs.fields().clone();
    rhs.default_scene5408 = 1;
    mismatch(
        compare(&lhs, &snapshot(rhs), POLICY),
        "preset.default_scene5408",
    );
}

#[test]
fn ordered_grid_rows_models_and_duplicates_are_not_sets() {
    let mut f = fixture();
    let m = ModelFields {
        opaque_word0: 2,
        ..ModelFields::default()
    };
    f.grid0[0][1] = Model::new(m).unwrap();
    let lhs = snapshot(f.clone());
    f.grid0[0].swap(0, 1);
    mismatch(
        compare(&lhs, &snapshot(f), POLICY),
        "preset.grid0[0][0].opaque_word0",
    );
    let lhs = snapshot(fixture());
    let mut rhs = fixture();
    rhs.grid0.pop();
    mismatch(compare(&lhs, &snapshot(rhs), POLICY), "preset.grid0.length");
    let mut rhs = fixture();
    rhs.grid0[0].pop();
    mismatch(
        compare(&lhs, &snapshot(rhs), POLICY),
        "preset.grid0[0].length",
    );
}

#[test]
fn graph_payload_order_and_membership_duplicates_are_retained() {
    let mut f = fixture();
    f.graph12.push(f.graph12[0]);
    let lhs = snapshot(f.clone());
    f.graph12[0].opaque_word28 = 7;
    f.graph12[0].opaque_word16 = 1;
    f.graph12[0].opaque_word20 = 2;
    f.graph12[0].opaque_word24 = 3;
    mismatch(
        compare(&lhs, &snapshot(f), POLICY),
        "preset.graph12[0].opaque_word24",
    );
    let mut f = fixture();
    f.graph12.swap(0, 1);
    mismatch(
        compare(&snapshot(fixture()), &snapshot(f), POLICY),
        "preset.graph12[0].opaque_word16",
    );
}

#[test]
fn maps_include_keys_and_preserve_captured_traversal() {
    let lhs = snapshot(fixture());
    let mut rhs = fixture();
    rhs.group_labels5424 = TraversalMap::from_entries(vec![MapEntry {
        key16: 5,
        value: Utf16::from_literal("Group").unwrap(),
    }])
    .unwrap();
    mismatch(
        compare(&lhs, &snapshot(rhs), POLICY),
        "preset.group_labels5424[0].key16",
    );
    let entries = vec![
        MapEntry { key16: 5, value: 1 },
        MapEntry { key16: 5, value: 2 },
    ];
    assert!(matches!(
        TraversalMap::from_entries(entries).unwrap_err().reason,
        AdmissionReason::DuplicateMapKey(5)
    ));
    let mut a = fixture();
    a.momentary5472 = TraversalMap::from_entries(vec![
        MapEntry {
            key16: 0xffff_ffff,
            value: 1,
        },
        MapEntry { key16: 0, value: 1 },
    ])
    .unwrap();
    let mut b = a.clone();
    b.momentary5472 = TraversalMap::from_entries(vec![
        MapEntry { key16: 0, value: 1 },
        MapEntry {
            key16: 0xffff_ffff,
            value: 1,
        },
    ])
    .unwrap();
    mismatch(
        compare(&snapshot(a), &snapshot(b), POLICY),
        "preset.momentary5472[0].key16",
    );
}

#[test]
fn assignments_use_byte_then_four_words_and_omit_timestamp() {
    let lhs = snapshot(fixture());
    let mut rhs = fixture();
    rhs.assignments64[0].opaque_byte0 = 2;
    rhs.assignments64[0].opaque_word4 = 3;
    mismatch(
        compare(&lhs, &snapshot(rhs), POLICY),
        "preset.assignments64[0].opaque_byte0",
    );
    let mut rhs = fixture();
    rhs.assignment_groups544[9][11].opaque_word16 = 3;
    mismatch(
        compare(&lhs, &snapshot(rhs), POLICY),
        "preset.assignment_groups544[9][11].opaque_word16",
    );
}

#[test]
fn paired_records_check_words_and_each_model_in_order() {
    let lhs = snapshot(fixture());
    let mut rhs = fixture();
    rhs.paired36[0].opaque_word4 = 99;
    let m = ModelFields {
        opaque_word0: 10,
        ..ModelFields::default()
    };
    rhs.paired36[0].model8 = Model::new(m.clone()).unwrap();
    mismatch(
        compare(&lhs, &snapshot(rhs), POLICY),
        "preset.paired36[0].opaque_word4",
    );
    let mut rhs = fixture();
    rhs.paired36[0].model272 = Model::new(m).unwrap();
    mismatch(
        compare(&lhs, &snapshot(rhs), POLICY),
        "preset.paired36[0].model272.opaque_word0",
    );
}

#[test]
fn descriptor_ignore_is_directional_and_does_not_suppress_values_or_cache() {
    let a = ParameterFields {
        packed260: [0, 1, 0, 0],
        opaque_word0: 8,
        ..ParameterFields::default()
    };
    let b = ParameterFields::default();
    let lhs = model_with(a);
    let rhs = model_with(b);
    assert_eq!(
        compare_model(&lhs, &rhs, POLICY).unwrap(),
        Comparison::Equal
    );
    mismatch(
        compare_model(&rhs, &lhs, POLICY),
        "model.descriptors28[0].opaque_word0",
    );
    let mut b = rhs.fields().clone();
    b.second52[0][0] = Variant::Int32(8);
    mismatch(
        compare_model(&lhs, &Model::new(b).unwrap(), POLICY),
        "model.second52[0][0]",
    );
    let mut b = rhs.fields().clone();
    b.second_cache96[7] = float(1.0);
    mismatch(
        compare_model(&lhs, &Model::new(b).unwrap(), POLICY),
        "model.second_cache96[7]",
    );
}

#[test]
fn descriptor_ignore_cannot_turn_unsupported_into_equal() {
    let a = ParameterFields {
        packed260: [0, 1, 0, 0],
        opaque_variant64: Variant::UnsupportedTypeId(777),
        ..ParameterFields::default()
    };
    let lhs = model_with(a.clone());
    let rhs = model_with(a);
    let error = compare_model(&lhs, &rhs, POLICY).unwrap_err();
    assert_eq!(
        error.reason,
        UnsupportedReason::VariantType { type_id: 777 }
    );
    assert_eq!(
        error.path.to_string(),
        "model.descriptors28[0].opaque_variant64.lhs"
    );
}

#[test]
fn lhs_descriptor_dispatch_is_used_before_rhs_metadata() {
    let lhs = model_with(ParameterFields::default());
    let rhs = model_with(ParameterFields {
        type8: 4,
        ..ParameterFields::default()
    });
    mismatch(
        compare_model(&lhs, &rhs, POLICY),
        "model.descriptors28[0].type8",
    );
    assert_eq!(
        compare_model(&rhs, &lhs, POLICY).unwrap_err().reason,
        UnsupportedReason::Type4Stepping
    );
}

#[test]
fn expression_precheck_precedes_full_model_and_ignores_no_restrictions() {
    let a = ParameterFields {
        packed260: [0, 1, 0, 0],
        expression_float160: float(1.0),
        ..ParameterFields::default()
    };
    let b = ParameterFields {
        expression_float160: float(2.0),
        ..a.clone()
    };
    assert_eq!(
        compare_model(&model_with(a.clone()), &model_with(b.clone()), POLICY).unwrap(),
        Comparison::Equal
    );
    let mut f = fixture();
    f.expression5508 = model_with(a);
    let lhs = snapshot(f.clone());
    f.expression5508 = model_with(b);
    mismatch(
        compare(&lhs, &snapshot(f), POLICY),
        "preset.expression5508.expression_precheck[0].expression_float160",
    );
}

#[test]
fn expression_precheck_mismatch_prevents_reached_type4_or_bank_mismatch() {
    let a = ParameterFields {
        type8: 4,
        suppress_current156: 1,
        ..ParameterFields::default()
    };
    let b = ParameterFields {
        suppress_current156: 2,
        ..a.clone()
    };
    let mut f = fixture();
    f.expression5508 = model_with(a);
    let lhs = snapshot(f.clone());
    f.expression5508 = model_with(b);
    mismatch(
        compare(&lhs, &snapshot(f), POLICY),
        "preset.expression5508.expression_precheck[0].suppress_current156",
    );
}

#[test]
fn missing_rhs_expression_position_is_skipped_then_descriptor_length_decides() {
    let mut a = fixture();
    a.expression5508 = Model::new(ModelFields {
        descriptors28: vec![parameter(ParameterFields::default())],
        ..ModelFields::default()
    })
    .unwrap();
    mismatch(
        compare(&snapshot(a), &snapshot(fixture()), POLICY),
        "preset.expression5508.descriptors28.length",
    );
}

#[test]
fn all_eight_second_banks_and_caches_precede_descriptor_length() {
    let a = Model::new(ModelFields {
        descriptors28: vec![parameter(ParameterFields::default())],
        ..ModelFields::default()
    })
    .unwrap();
    let mut b = ModelFields::default();
    b.second_cache96[7] = float(1.0);
    mismatch(
        compare_model(&a, &Model::new(b).unwrap(), POLICY),
        "model.second_cache96[7]",
    );
    let mut b = a.fields().clone();
    b.second52[7].push(Variant::Int32(0));
    mismatch(
        compare_model(&a, &Model::new(b).unwrap(), POLICY),
        "model.second52[7].length",
    );
}

#[test]
fn current_preparation_is_early_but_false_result_is_checked_after_structural_fields() {
    let a = ParameterFields::default();
    let b = ParameterFields {
        current16: Variant::Int32(1),
        opaque_word0: 2,
        ..a.clone()
    };
    mismatch(
        compare_parameter(&parameter(a.clone()), &parameter(b), POLICY),
        "parameter.opaque_word0",
    );
    let a = ParameterFields {
        current16: Variant::UnsupportedTypeId(999),
        ..a
    };
    let b = ParameterFields {
        opaque_word0: 2,
        ..a.clone()
    };
    let error = compare_parameter(&parameter(a), &parameter(b), POLICY).unwrap_err();
    assert_eq!(error.path.to_string(), "parameter.current16.lhs");
}

#[test]
fn suppress_current_is_lhs_only_and_does_not_suppress_default() {
    let a = ParameterFields {
        suppress_current156: 1,
        current16: Variant::UnsupportedTypeId(999),
        ..ParameterFields::default()
    };
    let b = ParameterFields {
        current16: Variant::Int32(0),
        ..a.clone()
    };
    assert_eq!(
        compare_parameter(&parameter(a.clone()), &parameter(b.clone()), POLICY).unwrap(),
        Comparison::Equal
    );
    let a = ParameterFields {
        default32: Variant::UnsupportedTypeId(888),
        ..a
    };
    assert_eq!(
        compare_parameter(&parameter(a), &parameter(b), POLICY)
            .unwrap_err()
            .reason,
        UnsupportedReason::VariantType { type_id: 888 }
    );
    let lhs = ParameterFields {
        suppress_current156: 0,
        current16: Variant::UnsupportedTypeId(999),
        ..ParameterFields::default()
    };
    let rhs = ParameterFields {
        suppress_current156: 1,
        ..lhs.clone()
    };
    assert!(compare_parameter(&parameter(lhs), &parameter(rhs), POLICY).is_err());
}

#[test]
fn corrected_descriptor256_precedes_both_later_vectors_and_unknown_variant() {
    let a = ParameterFields::default();
    let b = ParameterFields {
        opaque_word256: 1,
        opaque_vector232: vec![9],
        linked_scene_mode244: vec![9],
        opaque_variant64: Variant::UnsupportedTypeId(999),
        ..a.clone()
    };
    mismatch(
        compare_parameter(&parameter(a), &parameter(b), POLICY),
        "parameter.opaque_word256",
    );
    let a = ParameterFields::default();
    let b = ParameterFields {
        opaque_vector220: vec![1],
        opaque_word256: 1,
        ..a.clone()
    };
    mismatch(
        compare_parameter(&parameter(a), &parameter(b), POLICY),
        "parameter.opaque_vector220.length",
    );
}

#[test]
fn earlier_preset_mismatch_hides_later_unsupported_expression_fields() {
    let lhs = snapshot(fixture());
    let mut b = fixture();
    b.default_scene5408 = 1;
    b.expression5508 = model_with(ParameterFields {
        type8: 4,
        ..ParameterFields::default()
    });
    mismatch(
        compare(&lhs, &snapshot(b), POLICY),
        "preset.default_scene5408",
    );
}

#[test]
fn packed261_262_are_compared_but_260_263_and_vectors104_116_128_are_excluded() {
    let a = ParameterFields::default();
    let mut b = a.clone();
    b.packed260 = [9, 0, 0, 8];
    b.omitted.vector104 = vec![1];
    b.omitted.vector116 = vec![2];
    b.omitted.vector128 = vec![3];
    assert_eq!(
        compare_parameter(&parameter(a.clone()), &parameter(b.clone()), POLICY).unwrap(),
        Comparison::Equal
    );
    b.packed260[2] = 1;
    mismatch(
        compare_parameter(&parameter(a), &parameter(b), POLICY),
        "parameter.packed261_262",
    );
}

#[test]
fn model_tail_word240_is_checked_before_word236() {
    let a = empty_model();
    let b = ModelFields {
        opaque_word240: 1,
        opaque_word236: 2,
        ..ModelFields::default()
    };
    mismatch(
        compare_model(&a, &Model::new(b).unwrap(), POLICY),
        "model.opaque_word240",
    );
}

#[test]
fn point_coordinates_use_raw_exact_x_before_y() {
    let a = ParameterFields {
        points264: vec![Point {
            x: float(-0.0),
            y: float(1.0),
        }],
        ..ParameterFields::default()
    };
    let b = ParameterFields {
        points264: vec![Point {
            x: float(0.0),
            y: float(1.0),
        }],
        ..a.clone()
    };
    assert_eq!(
        compare_parameter(&parameter(a.clone()), &parameter(b), POLICY).unwrap(),
        Comparison::Equal
    );
    let b = ParameterFields {
        points264: vec![Point {
            x: float(f32::NAN),
            y: float(2.0),
        }],
        ..a.clone()
    };
    mismatch(
        compare_parameter(&parameter(a), &parameter(b), POLICY),
        "parameter.points264[0].x",
    );
}

#[test]
fn utf16_null_empty_nul_surrogates_and_no_normalization() {
    let v = |t| Variant::String(t);
    assert_eq!(
        compare_variant(&v(Utf16::null()), &v(Utf16::default()), POLICY).unwrap(),
        Comparison::Equal
    );
    let a = Utf16::from_literal("a\0b").unwrap();
    let b = Utf16::from_literal("a").unwrap();
    mismatch(compare_variant(&v(a), &v(b), POLICY), "variant");
    assert_eq!(
        compare_variant(
            &v(Utf16::from_literal("😀").unwrap()),
            &v(Utf16::from_units(vec![0xd83d, 0xde00]).unwrap()),
            POLICY
        )
        .unwrap(),
        Comparison::Equal
    );
    mismatch(
        compare_variant(
            &v(Utf16::from_literal("é").unwrap()),
            &v(Utf16::from_literal("e\u{301}").unwrap()),
            POLICY,
        ),
        "variant",
    );
    assert_eq!(
        compare_variant(
            &v(Utf16::from_units(vec![0xd800]).unwrap()),
            &v(Utf16::from_units(vec![0xd800]).unwrap()),
            POLICY
        )
        .unwrap(),
        Comparison::Equal
    );
}

#[test]
fn four_numeric_rules_distinguish_nan_zero_tolerance_and_infinity() {
    let nan = F32::from_bits(0x7fc0_0101).unwrap();
    let nan2 = F32::from_bits(0xffc0_0202).unwrap();
    assert!(!plain_relative_f32(nan, nan2));
    assert!(same_float_f32(nan, nan2));
    assert!(!raw_exact_f32(nan, nan));
    assert!(!same_float_f32(nan, float(1.0)));
    assert!(same_double_f64(
        F64::from_bits(0x7ff8_0000_0000_0101).unwrap(),
        F64::from_bits(0xfff8_0000_0000_0202).unwrap()
    ));
    for (a, b) in [
        (float(0.0), float(-0.0)),
        (float(2.0), F32::from_bits(0x4000_0001).unwrap()),
    ] {
        assert!(plain_relative_f32(a, b));
    }
    assert!(!raw_exact_f32(
        float(2.0),
        F32::from_bits(0x4000_0001).unwrap()
    ));
    assert!(raw_exact_f32(float(0.0), float(-0.0)));
    assert!(raw_exact_f64(double(0.0), double(-0.0)));
    assert!(!plain_relative_f32(float(0.0), F32::from_bits(1).unwrap()));
    assert!(!plain_relative_f32(
        float(f32::INFINITY),
        float(f32::INFINITY)
    ));
    assert!(plain_relative_f32(
        float(f32::INFINITY),
        float(f32::NEG_INFINITY)
    ));
    assert!(same_double_f64(
        double(f64::INFINITY),
        double(f64::NEG_INFINITY)
    ));
    assert!(!same_double_f64(
        double(f64::INFINITY),
        double(f64::INFINITY)
    ));
    assert!(raw_exact_f32(float(f32::INFINITY), float(f32::INFINITY)));
    assert!(!plain_relative_f32(float(f32::INFINITY), float(2.0)));
    assert!(same_double_f64(double(2.0), double(2.00000000000001)));
    assert!(!same_double_f64(double(2.0), double(2.000000001)));
}

#[test]
fn variant64_direct_qt_nan_rule_differs_from_current_and_default() {
    let nan = Variant::Float32(float(f32::NAN));
    let a = ParameterFields {
        current16: nan.clone(),
        default32: nan.clone(),
        ..ParameterFields::default()
    };
    assert_eq!(
        compare_parameter(&parameter(a.clone()), &parameter(a.clone()), POLICY).unwrap(),
        Comparison::Equal
    );
    let a = ParameterFields {
        opaque_variant64: nan,
        ..a
    };
    mismatch(
        compare_parameter(&parameter(a.clone()), &parameter(a), POLICY),
        "parameter.opaque_variant64",
    );
}

#[test]
fn cross_numeric_to_real_is_f32_and_int_uint_extend_correctly() {
    assert_eq!(
        compare_variant(&Variant::Int32(7), &Variant::UInt32(7), POLICY).unwrap(),
        Comparison::Equal
    );
    mismatch(
        compare_variant(&Variant::Int32(-1), &Variant::UInt32(u32::MAX), POLICY),
        "variant",
    );
    assert_eq!(
        compare_variant(
            &Variant::Float32(float(16_777_216.0)),
            &Variant::Int32(16_777_217),
            POLICY
        )
        .unwrap(),
        Comparison::Equal
    );
    assert_eq!(
        compare_variant(
            &Variant::UInt32(u32::MAX),
            &Variant::Float32(float(4_294_967_296.0)),
            POLICY
        )
        .unwrap(),
        Comparison::Equal
    );
    assert_eq!(
        compare_variant(
            &Variant::Double64(double(2.0000000001)),
            &Variant::Float32(float(2.0)),
            POLICY
        )
        .unwrap(),
        Comparison::Equal
    );
    mismatch(
        compare_variant(
            &Variant::Double64(double(f64::NAN)),
            &Variant::Float32(float(f32::NAN)),
            POLICY,
        ),
        "variant",
    );
    assert_eq!(
        compare_variant(
            &Variant::Double64(double(f64::MAX)),
            &Variant::Float32(float(1.0)),
            POLICY
        )
        .unwrap_err()
        .reason,
        UnsupportedReason::CrossRealOverflow
    );
}

#[test]
fn unsupported_string_conversion_keeps_rhs_to_lhs_direction() {
    let i = Variant::Int32(7);
    let s = Variant::String(Utf16::from_literal("007").unwrap());
    assert_eq!(
        compare_variant(&i, &s, POLICY).unwrap_err().reason,
        UnsupportedReason::RhsToLhsConversion {
            lhs_type: 2,
            rhs_type: 10
        }
    );
    assert_eq!(
        compare_variant(&s, &i, POLICY).unwrap_err().reason,
        UnsupportedReason::RhsToLhsConversion {
            lhs_type: 10,
            rhs_type: 2
        }
    );
}

#[test]
fn custom_live_handler_unknown_types_and_numeric_environment_are_refusals() {
    let a = snapshot(fixture());
    assert_eq!(
        compare(&a, &a, ComparisonPolicy::CustomOrLiveHandler)
            .unwrap_err()
            .reason,
        UnsupportedReason::HandlerPolicy
    );
    assert_eq!(
        compare(&a, &a, ComparisonPolicy::UnresolvedNumericEnvironment)
            .unwrap_err()
            .reason,
        UnsupportedReason::NumericEnvironment
    );
    for id in [0, 1, 4, 5, 136, 9999] {
        assert_eq!(
            compare_variant(
                &Variant::UnsupportedTypeId(id),
                &Variant::UnsupportedTypeId(id),
                POLICY
            )
            .unwrap_err()
            .reason,
            UnsupportedReason::VariantType { type_id: id }
        );
    }
    assert_eq!(
        F32::from_bits(0x7f80_0001).unwrap_err().reason,
        AdmissionReason::SignalingNaN
    );
    assert_eq!(
        F64::from_bits(0x7ff0_0000_0000_0001).unwrap_err().reason,
        AdmissionReason::SignalingNaN
    );
}

#[test]
fn type4_is_refused_including_nonfinite_overflow_and_zero_steps() {
    for count in [-1, 0, 1, i32::MAX] {
        let a = ParameterFields {
            type8: 4,
            step_count84: count,
            step144: float(f32::INFINITY),
            current16: Variant::Float32(float(f32::NAN)),
            ..ParameterFields::default()
        };
        assert_eq!(
            compare_parameter(&parameter(a.clone()), &parameter(a), POLICY)
                .unwrap_err()
                .reason,
            UnsupportedReason::Type4Stepping
        );
    }
    // Suppression bypasses current type4, but default32 still uses ordinary rule.
    let a = ParameterFields {
        type8: 4,
        suppress_current156: 1,
        default32: Variant::Float32(float(f32::NAN)),
        ..ParameterFields::default()
    };
    assert_eq!(
        compare_parameter(&parameter(a.clone()), &parameter(a), POLICY).unwrap(),
        Comparison::Equal
    );
}

#[test]
fn missing_banks_and_descriptor_bounds_are_typed_admission_refusals() {
    let mut f = ModelFields::default();
    f.second52.pop();
    assert!(matches!(
        Model::new(f).unwrap_err().reason,
        AdmissionReason::SceneBanks {
            expected: 8,
            actual: 7
        }
    ));
    let mut f = ModelFields::default();
    f.second52[0].push(Variant::Int32(0));
    assert!(matches!(
        Model::new(f).unwrap_err().reason,
        AdmissionReason::BankExceedsDescriptors {
            values: 1,
            descriptors: 0
        }
    ));
    let f = ModelFields {
        descriptors28: vec![parameter(ParameterFields::default()); MAX_PARAMETERS + 1],
        ..ModelFields::default()
    };
    assert!(matches!(
        Model::new(f).unwrap_err().reason,
        AdmissionReason::Limit {
            maximum: MAX_PARAMETERS,
            actual: 257
        }
    ));
}

#[test]
fn owned_local_resource_edges_are_checked() {
    assert!(Utf16::from_units(vec![0; MAX_UTF16_UNITS]).is_ok());
    assert!(Utf16::from_units(vec![0; MAX_UTF16_UNITS + 1]).is_err());
    let p = ParameterFields {
        opaque_bytes4: vec![0; MAX_BYTES],
        opaque_vector208: vec![0; MAX_ITEMS],
        ..ParameterFields::default()
    };
    assert!(Parameter::new(p.clone()).is_ok());
    assert!(
        Parameter::new(ParameterFields {
            opaque_bytes4: vec![0; MAX_BYTES + 1],
            ..p.clone()
        })
        .is_err()
    );
    assert!(
        Parameter::new(ParameterFields {
            opaque_vector208: vec![0; MAX_ITEMS + 1],
            ..p
        })
        .is_err()
    );
    let mut f = fixture();
    f.grid0 = vec![vec![]; MAX_ROWS + 1];
    assert!(AdmittedSnapshot::new(f).is_err());
    let mut f = fixture();
    f.grid0[0] = vec![empty_model(); MAX_ROW_MODELS + 1];
    assert!(AdmittedSnapshot::new(f).is_err());
    let entries = (0..=MAX_ITEMS)
        .map(|i| MapEntry {
            key16: i as u32,
            value: 0,
        })
        .collect();
    assert!(TraversalMap::from_entries(entries).is_err());
    let mut f = fixture();
    f.default_scene5408 = 8;
    assert!(matches!(
        AdmittedSnapshot::new(f).unwrap_err().reason,
        AdmissionReason::InvalidDefaultScene(8)
    ));
}

#[test]
fn aggregate_budget_rejects_many_individually_valid_objects() {
    let text = Utf16::from_units(vec![0; MAX_UTF16_UNITS]).unwrap();
    let mut f = fixture();
    f.strings48 = vec![text.clone(); MAX_TOTAL_UNITS / MAX_UTF16_UNITS + 1];
    assert!(matches!(
        AdmittedSnapshot::new(f).unwrap_err().reason,
        AdmissionReason::Limit {
            maximum: MAX_TOTAL_UNITS,
            ..
        }
    ));
    let mut f = fixture();
    f.model_map5368 = TraversalMap::from_entries(
        (0..MAX_MODELS)
            .map(|i| MapEntry {
                key16: i as u32,
                value: ModelPair {
                    model20: empty_model(),
                    model284: empty_model(),
                },
            })
            .collect(),
    )
    .unwrap();
    assert!(matches!(
        AdmittedSnapshot::new(f).unwrap_err().reason,
        AdmissionReason::Limit {
            maximum: MAX_MODELS,
            ..
        }
    ));
    let p = ParameterFields {
        opaque_vector208: vec![0; MAX_ITEMS],
        opaque_vector220: vec![0; MAX_ITEMS],
        opaque_vector232: vec![0; MAX_ITEMS],
        linked_scene_mode244: vec![0; MAX_ITEMS],
        ..ParameterFields::default()
    };
    let m = Model::new(ModelFields {
        descriptors28: vec![parameter(p); 128],
        ..ModelFields::default()
    })
    .unwrap();
    let mut f = fixture();
    f.grid0[0] = vec![m; 2];
    assert!(matches!(
        AdmittedSnapshot::new(f).unwrap_err().reason,
        AdmissionReason::Limit {
            maximum: MAX_SCALARS,
            ..
        }
    ));
}

#[test]
fn string_vectors_labels_colors_momentary_and_final_opaque_fields_are_semantic() {
    let lhs = snapshot(fixture());
    let mut b = fixture();
    b.strings48[0] = Utf16::from_literal("a\0c").unwrap();
    mismatch(
        compare(&lhs, &snapshot(b), POLICY),
        "preset.strings48[0][2]",
    );
    let mut b = fixture();
    b.scene_names5412[0] = Utf16::from_literal("scene").unwrap();
    mismatch(
        compare(&lhs, &snapshot(b), POLICY),
        "preset.scene_names5412[0][0]",
    );
    let mut b = fixture();
    b.individual_labels5448 = TraversalMap::from_entries(vec![MapEntry {
        key16: 2,
        value: Utf16::from_literal("stomp").unwrap(),
    }])
    .unwrap();
    mismatch(
        compare(&lhs, &snapshot(b), POLICY),
        "preset.individual_labels5448[0].value[0]",
    );
    let mut b = fixture();
    b.momentary5472 = TraversalMap::from_entries(vec![MapEntry { key16: 4, value: 1 }]).unwrap();
    mismatch(
        compare(&lhs, &snapshot(b), POLICY),
        "preset.momentary5472[0].value",
    );
    let mut b = fixture();
    b.colors5496.swap(0, 1);
    mismatch(compare(&lhs, &snapshot(b), POLICY), "preset.colors5496[0]");
    let mut b = fixture();
    b.opaque_string5800 = Utf16::from_literal("x").unwrap();
    b.opaque_string5820 = Utf16::from_literal("y").unwrap();
    b.opaque_byte5825 = 1;
    mismatch(
        compare(&lhs, &snapshot(b), POLICY),
        "preset.opaque_string5800.length",
    );
    let mut b = fixture();
    b.opaque_string5820 = Utf16::from_literal("y").unwrap();
    b.opaque_byte5825 = 1;
    mismatch(
        compare(&lhs, &snapshot(b), POLICY),
        "preset.opaque_string5820.length",
    );
    let mut b = fixture();
    b.opaque_byte5825 = 1;
    mismatch(
        compare(&lhs, &snapshot(b), POLICY),
        "preset.opaque_byte5825",
    );
}

#[test]
fn key_comparison_precedes_unsupported_map_model() {
    let lhs = snapshot(fixture());
    let mut rhs = fixture();
    let value = ModelPair {
        model20: model_with(ParameterFields {
            type8: 4,
            ..ParameterFields::default()
        }),
        model284: empty_model(),
    };
    rhs.model_map5368 = TraversalMap::from_entries(vec![MapEntry { key16: 99, value }]).unwrap();
    mismatch(
        compare(&lhs, &snapshot(rhs), POLICY),
        "preset.model_map5368[0].key16",
    );
}

#[test]
fn ignored_earlier_descriptor_mismatch_does_not_inspect_later_unknown_descriptor_field() {
    let lhs = model_with(ParameterFields {
        opaque_word0: 1,
        packed260: [0, 1, 0, 0],
        opaque_variant64: Variant::UnsupportedTypeId(999),
        ..ParameterFields::default()
    });
    let rhs = model_with(ParameterFields::default());
    assert_eq!(
        compare_model(&lhs, &rhs, POLICY).unwrap(),
        Comparison::Equal
    );
}

#[test]
fn parameter_and_scene_value_aggregate_limits_are_independent() {
    let descriptors = vec![parameter(ParameterFields::default()); MAX_PARAMETERS];
    let sparse = Model::new(ModelFields {
        descriptors28: descriptors.clone(),
        ..ModelFields::default()
    })
    .unwrap();
    let mut f = fixture();
    f.paired36 = vec![
        PairedRecord {
            opaque_word0: 0,
            opaque_word4: 0,
            model8: sparse.clone(),
            model272: sparse
        };
        33
    ];
    assert!(matches!(
        AdmittedSnapshot::new(f).unwrap_err().reason,
        AdmissionReason::Limit {
            maximum: MAX_PARAMETERS_TOTAL,
            ..
        }
    ));
    let dense = Model::new(ModelFields {
        descriptors28: descriptors,
        second52: vec![vec![Variant::Int32(0); MAX_PARAMETERS]; SCENES],
        ..ModelFields::default()
    })
    .unwrap();
    let mut f = fixture();
    f.paired36 = vec![
        PairedRecord {
            opaque_word0: 0,
            opaque_word4: 0,
            model8: dense.clone(),
            model272: dense
        };
        33
    ];
    assert!(matches!(
        AdmittedSnapshot::new(f).unwrap_err().reason,
        AdmissionReason::Limit {
            maximum: MAX_SCENE_VALUES,
            ..
        }
    ));
}

use tetrad_core::comparison::*;

mod preset_fixture {
    use tetrad_core::comparison::*;
    fn float(value: f32) -> F32 {
        F32::from_bits(value.to_bits()).unwrap()
    }
    fn empty_model() -> Model {
        Model::new(ModelFields::default()).unwrap()
    }
    pub(super) fn fixture() -> PresetFields {
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
            momentary5472: TraversalMap::from_entries(vec![MapEntry { key16: 4, value: 0 }])
                .unwrap(),
            colors5496: vec![1, 2, 1],
            expression5508: model,
            opaque_string5800: Utf16::default(),
            opaque_string5820: Utf16::default(),
            opaque_byte5825: 0,
            omitted: PresetOmissions::default(),
        }
    }
}
const LEGACY: ComparisonPolicy = ComparisonPolicy::DefaultQtHandlerOrdinaryIeee;
const FINITE: ComparisonPolicy = ComparisonPolicy::AssertedFiniteType4DefaultQtHelpersOrdinaryIeee;

fn parameter(fields: ParameterFields) -> Parameter {
    Parameter::new(fields).unwrap()
}
fn invalid_fields() -> ParameterFields {
    ParameterFields {
        opaque_variant64: Variant::ConstructorInvalid,
        ..ParameterFields::default()
    }
}
fn mismatch(result: Result<Comparison, Unsupported>, expected: &str) {
    let Comparison::Mismatch { path } = result.unwrap() else {
        panic!("expected mismatch at {expected}")
    };
    assert_eq!(path.to_string(), expected);
}
fn refuse(result: Result<Comparison, Unsupported>, expected: &str, reason: UnsupportedReason) {
    let error = result.unwrap_err();
    assert_eq!(error.path.to_string(), expected);
    assert_eq!(error.reason, reason);
}
fn outside(side: OperandSide, context: VariantContext) -> UnsupportedReason {
    UnsupportedReason::ConstructorInvalidOutside64 { side, context }
}
fn model(fields: ParameterFields, value: Option<Variant>) -> Model {
    let mut second52 = vec![vec![]; SCENES];
    if let Some(value) = value {
        second52[0] = vec![value];
    }
    Model::new(ModelFields {
        descriptors28: vec![parameter(fields)],
        second52,
        ..ModelFields::default()
    })
    .unwrap()
}
fn f(bits: u32) -> Variant {
    Variant::Float32(F32::from_bits(bits).unwrap())
}
fn d(bits: u64) -> Variant {
    Variant::Double64(F64::from_bits(bits).unwrap())
}

#[test]
fn proved_constructor_copy_is_payload_free_and_does_not_change_synthetic_defaults() {
    let unit = Variant::ConstructorInvalid;
    assert_eq!(unit.type_id(), 0);
    assert_eq!(format!("{unit:?}"), "ConstructorInvalid");
    let p = parameter(invalid_fields());
    let before = format!("{p:?}");
    let copied = p.clone();
    assert_eq!(
        compare_parameter(&p, &copied, LEGACY).unwrap(),
        Comparison::Equal
    );
    assert_eq!(format!("{p:?}"), before);
    let defaults = ParameterFields::default();
    for value in [
        &defaults.current16,
        &defaults.default32,
        &defaults.opaque_variant64,
    ] {
        assert!(matches!(value, Variant::Int32(0)));
    }
}

#[test]
fn equal_invalid_visits_byte80_packed_bytes_and_points() {
    let lhs = parameter(invalid_fields());
    for which in 0..3 {
        let mut rhs = lhs.fields().clone();
        let path = match which {
            0 => {
                rhs.opaque_byte80 = 1;
                "parameter.opaque_byte80"
            }
            1 => {
                rhs.packed260[2] = 1;
                "parameter.packed261_262"
            }
            _ => {
                rhs.points264.push(Point::default());
                "parameter.points264.length"
            }
        };
        mismatch(compare_parameter(&lhs, &parameter(rhs), LEGACY), path);
    }
    let mut fields = invalid_fields();
    fields.points264.push(Point::default());
    let lhs = parameter(fields.clone());
    fields.points264[0].x = F32::from_bits(0x3f80_0000).unwrap();
    mismatch(
        compare_parameter(&lhs, &parameter(fields), LEGACY),
        "parameter.points264[0].x",
    );
}

#[test]
fn ten_directed_known_cross_pairs_do_not_convert_payloads() {
    let values = [
        Variant::Int32(i32::MIN),
        Variant::UInt32(u32::MAX),
        f(0x3f80_0000),
        d(0x3ff0_0000_0000_0000),
        Variant::String(Utf16::from_literal("a\0\u{fffd}").unwrap()),
        f(0x7fc0_1234),
        f(0x7f80_0000),
        f(0xff80_0000),
        f(0x8000_0000),
        d(0x7ff8_0000_0000_1234),
        d(0x7ff0_0000_0000_0000),
        d(0x7fef_ffff_ffff_ffff),
        d(0x8000_0000_0000_0000),
        Variant::String(Utf16::null()),
        Variant::String(Utf16::from_units(vec![]).unwrap()),
    ];
    for value in values {
        for (lhs, rhs) in [
            (Variant::ConstructorInvalid, value.clone()),
            (value, Variant::ConstructorInvalid),
        ] {
            let lhs = parameter(ParameterFields {
                opaque_variant64: lhs,
                ..ParameterFields::default()
            });
            let rhs = parameter(ParameterFields {
                opaque_variant64: rhs,
                opaque_byte80: 1,
                ..ParameterFields::default()
            });
            for policy in [LEGACY, FINITE] {
                mismatch(
                    compare_parameter(&lhs, &rhs, policy),
                    "parameter.opaque_variant64",
                );
            }
        }
    }
    let lhs = parameter(ParameterFields {
        opaque_variant64: Variant::String(Utf16::null()),
        ..ParameterFields::default()
    });
    let rhs = parameter(ParameterFields {
        opaque_variant64: Variant::String(Utf16::from_units(vec![]).unwrap()),
        ..ParameterFields::default()
    });
    assert_eq!(
        compare_parameter(&lhs, &rhs, LEGACY).unwrap(),
        Comparison::Equal
    );
}

#[test]
fn unknown_markers_including_known_ids_precede_invalid_decisions() {
    for id in [0, 1, 2, 3, 6, 10, 135, 136, 0x3fff_ffff] {
        for (lhs, rhs, side) in [
            (
                Variant::UnsupportedTypeId(id),
                Variant::ConstructorInvalid,
                "lhs",
            ),
            (
                Variant::ConstructorInvalid,
                Variant::UnsupportedTypeId(id),
                "rhs",
            ),
            (
                Variant::UnsupportedTypeId(id),
                Variant::UnsupportedTypeId(999),
                "lhs",
            ),
        ] {
            let lhs = parameter(ParameterFields {
                opaque_variant64: lhs,
                ..ParameterFields::default()
            });
            let rhs = parameter(ParameterFields {
                opaque_variant64: rhs,
                ..ParameterFields::default()
            });
            refuse(
                compare_parameter(&lhs, &rhs, LEGACY),
                &format!("parameter.opaque_variant64.{side}"),
                UnsupportedReason::VariantType { type_id: id },
            );
        }
    }
}

#[test]
fn public_policy_refuses_before_invalid_or_unknown_operands() {
    for (policy, reason) in [
        (
            ComparisonPolicy::CustomOrLiveHandler,
            UnsupportedReason::HandlerPolicy,
        ),
        (
            ComparisonPolicy::UnresolvedNumericEnvironment,
            UnsupportedReason::NumericEnvironment,
        ),
    ] {
        let lhs = parameter(invalid_fields());
        let rhs = parameter(ParameterFields {
            opaque_variant64: Variant::UnsupportedTypeId(0),
            ..ParameterFields::default()
        });
        refuse(
            compare_parameter(&lhs, &rhs, policy),
            "policy",
            reason.clone(),
        );
        refuse(
            compare_variant(
                &Variant::ConstructorInvalid,
                &Variant::UnsupportedTypeId(2),
                policy,
            ),
            "policy",
            reason.clone(),
        );
        let m = model(invalid_fields(), Some(Variant::ConstructorInvalid));
        refuse(compare_model(&m, &m, policy), "policy", reason.clone());
        let s = AdmittedSnapshot::new(preset_fixture::fixture()).unwrap();
        refuse(compare(&s, &s, policy), "policy", reason);
    }
}

#[test]
fn standalone_variant_cannot_admit_invalid_and_keeps_unknown_priority() {
    for policy in [LEGACY, FINITE] {
        for (lhs, rhs, side) in [
            (
                Variant::ConstructorInvalid,
                Variant::ConstructorInvalid,
                OperandSide::Lhs,
            ),
            (
                Variant::ConstructorInvalid,
                f(0x7fc0_1234),
                OperandSide::Lhs,
            ),
            (
                Variant::Int32(0),
                Variant::ConstructorInvalid,
                OperandSide::Rhs,
            ),
        ] {
            let path = if side == OperandSide::Lhs {
                "variant.lhs"
            } else {
                "variant.rhs"
            };
            refuse(
                compare_variant(&lhs, &rhs, policy),
                path,
                outside(side, VariantContext::PublicVariant),
            );
        }
        for id in [0, 2, 135] {
            refuse(
                compare_variant(
                    &Variant::ConstructorInvalid,
                    &Variant::UnsupportedTypeId(id),
                    policy,
                ),
                "variant.rhs",
                UnsupportedReason::VariantType { type_id: id },
            );
            refuse(
                compare_variant(
                    &Variant::UnsupportedTypeId(id),
                    &Variant::ConstructorInvalid,
                    policy,
                ),
                "variant.lhs",
                UnsupportedReason::VariantType { type_id: id },
            );
        }
    }
}

#[test]
fn current_preparation_refuses_before_structure_and_suppression_hides_it() {
    for side in [OperandSide::Lhs, OperandSide::Rhs] {
        let mut lhs = invalid_fields();
        let mut rhs = lhs.clone();
        rhs.opaque_word0 = 1;
        if side == OperandSide::Lhs {
            lhs.current16 = Variant::ConstructorInvalid;
        } else {
            rhs.current16 = Variant::ConstructorInvalid;
        }
        let path = if side == OperandSide::Lhs {
            "parameter.current16.lhs"
        } else {
            "parameter.current16.rhs"
        };
        refuse(
            compare_parameter(&parameter(lhs.clone()), &parameter(rhs.clone()), LEGACY),
            path,
            outside(side, VariantContext::Current16),
        );
        lhs.suppress_current156 = 1;
        mismatch(
            compare_parameter(&parameter(lhs), &parameter(rhs), LEGACY),
            "parameter.opaque_word0",
        );
    }
    let fields = ParameterFields {
        suppress_current156: 1,
        current16: Variant::ConstructorInvalid,
        ..invalid_fields()
    };
    let lhs = parameter(fields.clone());
    assert_eq!(
        compare_parameter(&lhs, &parameter(fields), LEGACY).unwrap(),
        Comparison::Equal
    );
}

#[test]
fn default32_refuses_when_reached_and_is_hidden_by_earlier_structure() {
    for side in [OperandSide::Lhs, OperandSide::Rhs] {
        let mut lhs = invalid_fields();
        let mut rhs = lhs.clone();
        if side == OperandSide::Lhs {
            lhs.default32 = Variant::ConstructorInvalid;
        } else {
            rhs.default32 = Variant::ConstructorInvalid;
        }
        let path = if side == OperandSide::Lhs {
            "parameter.default32.lhs"
        } else {
            "parameter.default32.rhs"
        };
        refuse(
            compare_parameter(&parameter(lhs.clone()), &parameter(rhs.clone()), FINITE),
            path,
            outside(side, VariantContext::Default32),
        );
        rhs.opaque_word0 = 1;
        mismatch(
            compare_parameter(&parameter(lhs), &parameter(rhs), FINITE),
            "parameter.opaque_word0",
        );
    }
}

#[test]
fn earlier_descriptor_fields_hide_unknown64_and_keep_corrected_tail_order() {
    for which in 0..10 {
        let mut lhs = invalid_fields();
        lhs.opaque_variant64 = Variant::UnsupportedTypeId(0);
        let mut rhs = lhs.clone();
        let path = match which {
            0 => {
                rhs.opaque_word0 = 1;
                "opaque_word0"
            }
            1 => {
                rhs.opaque_bytes4.push(1);
                "opaque_bytes4.length"
            }
            2 => {
                rhs.type8 = 1;
                "type8"
            }
            3 => {
                rhs.current16 = Variant::Int32(1);
                "current16"
            }
            4 => {
                rhs.opaque_vector208.push(1);
                "opaque_vector208.length"
            }
            5 => {
                rhs.opaque_vector220.push(1);
                "opaque_vector220.length"
            }
            6 => {
                rhs.opaque_word256 = 1;
                "opaque_word256"
            }
            7 => {
                rhs.opaque_vector232.push(1);
                "opaque_vector232.length"
            }
            8 => {
                rhs.linked_scene_mode244.push(1);
                "linked_scene_mode244.length"
            }
            _ => {
                rhs.opaque_string88 = Utf16::from_literal("x").unwrap();
                "opaque_string88.length"
            }
        };
        mismatch(
            compare_parameter(&parameter(lhs), &parameter(rhs), LEGACY),
            &format!("parameter.{path}"),
        );
    }
}

#[test]
fn bank_invalid_refuses_before_cache_and_bank_length_can_hide_it() {
    for side in [OperandSide::Lhs, OperandSide::Rhs] {
        let lhs = model(
            invalid_fields(),
            Some(if side == OperandSide::Lhs {
                Variant::ConstructorInvalid
            } else {
                Variant::Int32(0)
            }),
        );
        let mut rhs = model(
            invalid_fields(),
            Some(if side == OperandSide::Rhs {
                Variant::ConstructorInvalid
            } else {
                Variant::Int32(0)
            }),
        )
        .fields()
        .clone();
        rhs.second_cache96[0] = F32::from_bits(0x3f80_0000).unwrap();
        let path = if side == OperandSide::Lhs {
            "model.second52[0][0].lhs"
        } else {
            "model.second52[0][0].rhs"
        };
        refuse(
            compare_model(&lhs, &Model::new(rhs.clone()).unwrap(), LEGACY),
            path,
            outside(side, VariantContext::SceneBank),
        );
        rhs.second52[0].clear();
        mismatch(
            compare_model(&lhs, &Model::new(rhs).unwrap(), LEGACY),
            "model.second52[0].length",
        );
    }
    for scene in 0..SCENES {
        let mut fields = model(invalid_fields(), None).fields().clone();
        fields.second52[scene] = vec![Variant::ConstructorInvalid];
        let lhs = Model::new(fields.clone()).unwrap();
        if scene > 0 {
            fields.second_cache96[scene - 1] = F32::from_bits(0x3f80_0000).unwrap();
            mismatch(
                compare_model(&lhs, &Model::new(fields).unwrap(), LEGACY),
                &format!("model.second_cache96[{}]", scene - 1),
            );
        } else {
            refuse(
                compare_model(&lhs, &lhs, LEGACY),
                "model.second52[0][0].lhs",
                outside(OperandSide::Lhs, VariantContext::SceneBank),
            );
        }
    }
}

#[test]
fn ignore_discards_only_decided_descriptor_mismatch_and_not_bank_or_refusal() {
    let mut lhs_fields = invalid_fields();
    lhs_fields.packed260[1] = 1;
    lhs_fields.points264.push(Point {
        x: F32::from_bits(0x7fc0_1234).unwrap(),
        y: F32::default(),
    });
    let lhs = model(lhs_fields.clone(), None);
    let rhs = model(
        ParameterFields {
            opaque_variant64: Variant::Int32(0),
            ..lhs_fields.clone()
        },
        None,
    );
    assert_eq!(
        compare_model(&lhs, &rhs, LEGACY).unwrap(),
        Comparison::Equal
    );
    let rhs = model(
        ParameterFields {
            opaque_variant64: Variant::UnsupportedTypeId(0),
            ..lhs_fields.clone()
        },
        None,
    );
    refuse(
        compare_model(&lhs, &rhs, LEGACY),
        "model.descriptors28[0].opaque_variant64.rhs",
        UnsupportedReason::VariantType { type_id: 0 },
    );
    let lhs = model(lhs_fields.clone(), Some(Variant::Int32(0)));
    let rhs = model(lhs_fields.clone(), Some(Variant::Int32(1)));
    mismatch(compare_model(&lhs, &rhs, LEGACY), "model.second52[0][0]");
    let lhs = model(lhs_fields.clone(), Some(Variant::ConstructorInvalid));
    refuse(
        compare_model(&lhs, &lhs, LEGACY),
        "model.second52[0][0].lhs",
        outside(OperandSide::Lhs, VariantContext::SceneBank),
    );
    let mut rhs = lhs.fields().clone();
    rhs.second52[0].clear();
    mismatch(
        compare_model(&lhs, &Model::new(rhs).unwrap(), LEGACY),
        "model.second52[0].length",
    );
}

#[test]
fn type4_legacy_dispatch_and_finite_conversion_preserve_order() {
    let fields = ParameterFields {
        type8: 4,
        current16: Variant::ConstructorInvalid,
        step_count84: 0,
        ..invalid_fields()
    };
    let lhs = parameter(fields.clone());
    refuse(
        compare_parameter(&lhs, &lhs, LEGACY),
        "parameter.current16",
        UnsupportedReason::Type4Stepping,
    );
    refuse(
        compare_parameter(&lhs, &lhs, FINITE),
        "parameter.current16.lhs",
        UnsupportedReason::FiniteType4 {
            side: OperandSide::Lhs,
            reason: Type4Refusal::ConstructorInvalid,
        },
    );
    let lhs = parameter(ParameterFields {
        current16: Variant::Int32(0),
        ..fields.clone()
    });
    let rhs = parameter(fields.clone());
    refuse(
        compare_parameter(&lhs, &rhs, FINITE),
        "parameter.current16.lhs",
        UnsupportedReason::FiniteType4 {
            side: OperandSide::Lhs,
            reason: Type4Refusal::NonPositiveStepCount { count: 0 },
        },
    );
    let valid = ParameterFields {
        step_count84: 1,
        step144: F32::from_bits(0x3f80_0000).unwrap(),
        current16: Variant::Int32(0),
        ..fields
    };
    let lhs = parameter(valid.clone());
    let rhs = parameter(ParameterFields {
        current16: Variant::ConstructorInvalid,
        ..valid.clone()
    });
    refuse(
        compare_parameter(&lhs, &rhs, FINITE),
        "parameter.current16.rhs",
        UnsupportedReason::FiniteType4 {
            side: OperandSide::Rhs,
            reason: Type4Refusal::ConstructorInvalid,
        },
    );
    let bank = model(valid.clone(), Some(Variant::ConstructorInvalid));
    refuse(
        compare_model(&bank, &bank, LEGACY),
        "model.second52[0][0]",
        UnsupportedReason::Type4Stepping,
    );
    refuse(
        compare_model(&bank, &bank, FINITE),
        "model.second52[0][0].lhs",
        UnsupportedReason::FiniteType4 {
            side: OperandSide::Lhs,
            reason: Type4Refusal::ConstructorInvalid,
        },
    );
    let lhs = parameter(ParameterFields {
        default32: Variant::ConstructorInvalid,
        ..valid
    });
    refuse(
        compare_parameter(&lhs, &lhs, FINITE),
        "parameter.default32.lhs",
        outside(OperandSide::Lhs, VariantContext::Default32),
    );
}

#[test]
fn all_model_carriers_reach64_and_off64_refusals_with_policy_threaded() {
    for carrier in 0..6 {
        let mut fields = preset_fixture::fixture();
        let unit_model = model(invalid_fields(), None);
        match carrier {
            0 => fields.grid0[0][0] = unit_model,
            1 => fields.paired36[0].model8 = unit_model,
            2 => fields.paired36[0].model272 = unit_model,
            3 | 4 => {
                let entry = &fields.model_map5368.entries()[0];
                let mut value = entry.value.clone();
                if carrier == 3 {
                    value.model20 = unit_model;
                } else {
                    value.model284 = unit_model;
                }
                fields.model_map5368 = TraversalMap::from_entries(vec![MapEntry {
                    key16: entry.key16,
                    value,
                }])
                .unwrap();
            }
            _ => fields.expression5508 = unit_model,
        }
        let lhs = AdmittedSnapshot::new(fields.clone()).unwrap();
        let before = format!("{lhs:?}");
        for policy in [LEGACY, FINITE] {
            assert_eq!(
                compare(&lhs, &lhs.clone(), policy).unwrap(),
                Comparison::Equal
            );
        }
        let bad = model(invalid_fields(), Some(Variant::ConstructorInvalid));
        match carrier {
            0 => fields.grid0[0][0] = bad,
            1 => fields.paired36[0].model8 = bad,
            2 => fields.paired36[0].model272 = bad,
            3 | 4 => {
                let entry = &fields.model_map5368.entries()[0];
                let mut value = entry.value.clone();
                if carrier == 3 {
                    value.model20 = bad;
                } else {
                    value.model284 = bad;
                }
                fields.model_map5368 = TraversalMap::from_entries(vec![MapEntry {
                    key16: entry.key16,
                    value,
                }])
                .unwrap();
            }
            _ => fields.expression5508 = bad,
        }
        let bad = AdmittedSnapshot::new(fields).unwrap();
        let root = [
            "preset.grid0[0][0]",
            "preset.paired36[0].model8",
            "preset.paired36[0].model272",
            "preset.model_map5368[0].model20",
            "preset.model_map5368[0].model284",
            "preset.expression5508",
        ][carrier];
        for policy in [LEGACY, FINITE] {
            refuse(
                compare(&bad, &bad, policy),
                &format!("{root}.second52[0][0].lhs"),
                outside(OperandSide::Lhs, VariantContext::SceneBank),
            );
        }
        assert_eq!(format!("{lhs:?}"), before);
    }
}

#[test]
fn expression_precheck_and_early_cache_mismatch_hide_later64_and_bank_invalid() {
    let mut fields = preset_fixture::fixture();
    fields.expression5508 = model(
        ParameterFields {
            opaque_variant64: Variant::UnsupportedTypeId(0),
            ..invalid_fields()
        },
        Some(Variant::ConstructorInvalid),
    );
    let lhs = AdmittedSnapshot::new(fields.clone()).unwrap();
    let mut expression = fields.expression5508.fields().clone();
    let mut descriptor = expression.descriptors28[0].fields().clone();
    descriptor.expression_float160 = F32::from_bits(0x3f80_0000).unwrap();
    expression.descriptors28[0] = parameter(descriptor);
    fields.expression5508 = Model::new(expression).unwrap();
    let rhs = AdmittedSnapshot::new(fields).unwrap();
    mismatch(
        compare(&lhs, &rhs, LEGACY),
        "preset.expression5508.expression_precheck[0].expression_float160",
    );
    let lhs = model(
        ParameterFields {
            opaque_variant64: Variant::UnsupportedTypeId(0),
            ..invalid_fields()
        },
        None,
    );
    let mut rhs = lhs.fields().clone();
    rhs.second_cache96[0] = F32::from_bits(0x3f80_0000).unwrap();
    mismatch(
        compare_model(&lhs, &Model::new(rhs).unwrap(), LEGACY),
        "model.second_cache96[0]",
    );
}

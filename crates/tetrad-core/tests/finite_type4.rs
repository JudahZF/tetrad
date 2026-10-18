use tetrad_core::comparison::*;

const FINITE: ComparisonPolicy = ComparisonPolicy::AssertedFiniteType4DefaultQtHelpersOrdinaryIeee;
const LEGACY: ComparisonPolicy = ComparisonPolicy::DefaultQtHandlerOrdinaryIeee;
const EPS: u32 = 0x322b_cc77;

fn f(bits: u32) -> F32 {
    F32::from_bits(bits).unwrap()
}
fn v(bits: u32) -> Variant {
    Variant::Float32(f(bits))
}
fn d(bits: u64) -> Variant {
    Variant::Double64(F64::from_bits(bits).unwrap())
}
fn fields(value: Variant, step: u32, count: i32) -> ParameterFields {
    ParameterFields {
        type8: 4,
        current16: value,
        step_count84: count,
        step144: f(step),
        ..ParameterFields::default()
    }
}
fn parameter(fields: ParameterFields) -> Parameter {
    Parameter::new(fields).unwrap()
}
fn pair(lhs: Variant, rhs: Variant, step: u32, count: i32) -> Result<Comparison, Unsupported> {
    compare_parameter(
        &parameter(fields(lhs, step, count)),
        &parameter(fields(rhs, step, count)),
        FINITE,
    )
}
fn refusal(
    result: Result<Comparison, Unsupported>,
    side: OperandSide,
    reason: Type4Refusal,
    path: &str,
) {
    let error = result.unwrap_err();
    assert_eq!(
        error.reason,
        UnsupportedReason::FiniteType4 { side, reason }
    );
    assert_eq!(error.path.to_string(), path);
}
fn mismatch(result: Result<Comparison, Unsupported>, expected: &str) {
    assert_eq!(
        match result.unwrap() {
            Comparison::Mismatch { path } => path.to_string(),
            Comparison::Equal => panic!("expected mismatch at {expected}"),
        },
        expected
    );
}
fn bank(descriptor: ParameterFields, values: Vec<Variant>) -> Model {
    let mut second52 = vec![vec![]; SCENES];
    second52[0] = values;
    Model::new(ModelFields {
        descriptors28: vec![parameter(descriptor)],
        second52,
        ..ModelFields::default()
    })
    .unwrap()
}

#[test]
fn half_neighbors_both_signs_and_signed_zero() {
    for (bits, index) in [
        (0x3eff_ffff, 0),
        (0x3f00_0000, 1),
        (0x3f00_0001, 1),
        (0xbeff_ffff, 0),
        (0xbf00_0000, -1),
        (0xbf00_0001, -1),
        (0x3fbf_ffff, 1),
        (0x3fc0_0000, 2),
        (0x3fc0_0001, 2),
        (0xbfbf_ffff, -1),
        (0xbfc0_0000, -2),
        (0xbfc0_0001, -2),
        (0, 0),
        (0x8000_0000, 0),
        (1, 0),
        (0x8000_0001, 0),
    ] {
        assert_eq!(
            pair(v(bits), Variant::Int32(index), 0x3f80_0000, 1).unwrap(),
            Comparison::Equal
        );
    }
    assert_eq!(v(0x8000_0000).type_id(), 135);
    let p = parameter(fields(v(0x8000_0000), 0x3f80_0000, 1));
    let Variant::Float32(value) = p.fields().current16 else {
        panic!()
    };
    assert_eq!(value.bits(), 0x8000_0000);
}

#[test]
fn subnormal_step_and_epsilon_neighbors_select_the_floor() {
    // EPS/2 is exactly representable. Below/equal EPS yields a half tie;
    // its next larger neighbor yields a quotient below that tie.
    for step in [1, 0x007f_ffff, EPS - 1, EPS] {
        assert_eq!(
            pair(v(EPS - 0x0080_0000), v(EPS), step, 1).unwrap(),
            Comparison::Equal
        );
    }
    mismatch(
        pair(v(EPS - 0x0080_0000), v(EPS), EPS + 1, 1),
        "parameter.current16",
    );
}

#[test]
fn count_enables_without_capping_the_index() {
    for count in [1, 2, i32::MAX - 1, i32::MAX] {
        assert_eq!(
            pair(v(0x42c8_0000), Variant::Int32(100), 0x3f80_0000, count).unwrap(),
            Comparison::Equal
        );
        mismatch(
            pair(Variant::Int32(100), Variant::Int32(1), 0x3f80_0000, count),
            "parameter.current16",
        );
    }
    for count in [0, -1, i32::MIN] {
        refusal(
            pair(Variant::Int32(0), Variant::Int32(0), 0x7fc0_0001, count),
            OperandSide::Lhs,
            Type4Refusal::NonPositiveStepCount { count },
            "parameter.current16.lhs",
        );
    }
}

#[test]
fn count_precedes_step_and_each_conversion_precedes_count() {
    for bad in [
        Variant::UnsupportedTypeId(777),
        Variant::String(Utf16::null()),
    ] {
        let type_id = bad.type_id();
        refusal(
            pair(bad, Variant::Int32(0), 0, 0),
            OperandSide::Lhs,
            Type4Refusal::Variant { type_id },
            "parameter.current16.lhs",
        );
    }
    refusal(
        pair(d(0x47ef_ffff_e000_0001), Variant::Int32(0), 0, 0),
        OperandSide::Lhs,
        Type4Refusal::ToFloatOverflow,
        "parameter.current16.lhs",
    );
    // Lhs metadata refusal is complete before the rhs alternative is inspected.
    refusal(
        pair(Variant::Int32(0), Variant::UnsupportedTypeId(777), 0, 0),
        OperandSide::Lhs,
        Type4Refusal::NonPositiveStepCount { count: 0 },
        "parameter.current16.lhs",
    );
    refusal(
        pair(
            Variant::Int32(0),
            Variant::UnsupportedTypeId(777),
            0x3f80_0000,
            1,
        ),
        OperandSide::Rhs,
        Type4Refusal::Variant { type_id: 777 },
        "parameter.current16.rhs",
    );
}

#[test]
fn nonpositive_and_nonfinite_steps_are_deliberate_refusals() {
    for bits in [0, 0x8000_0000, 0xbf80_0000, 0x8000_0001] {
        refusal(
            pair(Variant::Int32(0), Variant::Int32(0), bits, 1),
            OperandSide::Lhs,
            Type4Refusal::NonPositiveStep { bits },
            "parameter.current16.lhs",
        );
    }
    for bits in [0x7f80_0000, 0xff80_0000, 0x7fc0_0001, 0xffc0_0001] {
        refusal(
            pair(Variant::Int32(0), Variant::Int32(0), bits, 1),
            OperandSide::Lhs,
            Type4Refusal::NonFiniteStep { bits },
            "parameter.current16.lhs",
        );
    }
}

#[test]
fn nonfinite_alternatives_refuse_before_unused_metadata() {
    for value in [
        v(0x7f80_0000),
        v(0xff80_0000),
        v(0x7fc0_0001),
        v(0xffc0_0001),
        d(0x7ff0_0000_0000_0000),
        d(0xfff0_0000_0000_0000),
        d(0x7ff8_0000_0000_0001),
    ] {
        refusal(
            pair(value.clone(), Variant::Int32(0), 0, 0),
            OperandSide::Lhs,
            Type4Refusal::NonFiniteValue,
            "parameter.current16.lhs",
        );
        refusal(
            pair(Variant::Int32(0), value, 0x3f80_0000, 1),
            OperandSide::Rhs,
            Type4Refusal::NonFiniteValue,
            "parameter.current16.rhs",
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
fn int_uint_and_double_to_float_precision_is_distinct_from_direct_qt() {
    for (lhs, rhs, step) in [
        (Variant::Int32(16_777_217), v(0x4b80_0000), 0x3f80_0000),
        (Variant::Int32(16_777_219), v(0x4b80_0002), 0x3f80_0000),
        (Variant::Int32(i32::MAX), v(0x4f00_0000), 0x4000_0000),
        (Variant::Int32(i32::MIN), v(0xcf00_0000), 0x4000_0000),
        (Variant::UInt32(u32::MAX), v(0x4f80_0000), 0x4080_0000),
        (d(0x3ff0_0000_1000_0000), v(0x3f80_0000), 0x3f80_0000),
        (d(0x3ff0_0000_3000_0000), v(0x3f80_0002), 0x3f80_0000),
        (d(0x8000_0000_0000_0000), v(0), 0x3f80_0000),
        (d(1), v(0), 0x3f80_0000),
        (d(0x47ef_ffff_e000_0000), v(0x7f7f_ffff), 0x7f7f_ffff),
    ] {
        assert_eq!(pair(lhs, rhs, step, 1).unwrap(), Comparison::Equal);
    }
    for bits in [
        0x47ef_ffff_e000_0001,
        0xc7ef_ffff_e000_0001,
        0x7fef_ffff_ffff_ffff,
    ] {
        refusal(
            pair(d(bits), Variant::Int32(0), 0x3f80_0000, 1),
            OperandSide::Lhs,
            Type4Refusal::ToFloatOverflow,
            "parameter.current16.lhs",
        );
    }
    // Crossing a half boundary makes nearest-even narrowing observable.
    assert_eq!(
        pair(d(0x3fdfffff_f0000000), v(0x3f00_0000), 0x3f80_0000, 1).unwrap(),
        Comparison::Equal
    );
    mismatch(
        pair(d(0x3fdfffff_efffffff), v(0x3f00_0000), 0x3f80_0000, 1),
        "parameter.current16",
    );
}

#[test]
fn binary32_division_witness_rounds_to_index_two() {
    let step = 0x3f80_0001;
    assert_eq!(
        pair(v(0x3fc0_0001), v(step + 0x0080_0000), step, 1).unwrap(),
        Comparison::Equal
    );
    mismatch(
        pair(v(0x3fc0_0001), v(step), step, 1),
        "parameter.current16",
    );
}

#[test]
fn safe_quotient_bound_and_negative_endpoint_refusal() {
    for (bits, index) in [(0x4eff_ffff, 2_147_483_520), (0xceff_ffff, -2_147_483_520)] {
        assert_eq!(
            pair(v(bits), Variant::Int32(index), 0x3f80_0000, 1).unwrap(),
            Comparison::Equal
        );
    }
    for quotient_bits in [0x4f00_0000, 0xcf00_0000] {
        refusal(
            pair(v(quotient_bits), Variant::Int32(0), 0x3f80_0000, 1),
            OperandSide::Lhs,
            Type4Refusal::QuantizedLong32Overflow { quotient_bits },
            "parameter.current16.lhs",
        );
        refusal(
            pair(Variant::Int32(0), v(quotient_bits), 0x3f80_0000, 1),
            OperandSide::Rhs,
            Type4Refusal::QuantizedLong32Overflow { quotient_bits },
            "parameter.current16.rhs",
        );
    }
    for (bits, quotient_bits) in [(0x7f7f_ffff, 0x7f80_0000), (0xff7f_ffff, 0xff80_0000)] {
        refusal(
            pair(v(bits), Variant::Int32(0), EPS, 1),
            OperandSide::Lhs,
            Type4Refusal::DivisionOverflow { quotient_bits },
            "parameter.current16.lhs",
        );
        refusal(
            pair(Variant::Int32(0), v(bits), EPS, 1),
            OperandSide::Rhs,
            Type4Refusal::DivisionOverflow { quotient_bits },
            "parameter.current16.rhs",
        );
    }
}

#[test]
fn lhs_processing_finishes_before_rhs_conversion() {
    refusal(
        pair(v(0x7f7f_ffff), Variant::UnsupportedTypeId(999), EPS, 1),
        OperandSide::Lhs,
        Type4Refusal::DivisionOverflow {
            quotient_bits: 0x7f80_0000,
        },
        "parameter.current16.lhs",
    );
    refusal(
        pair(
            v(0x4f00_0000),
            Variant::UnsupportedTypeId(999),
            0x3f80_0000,
            1,
        ),
        OperandSide::Lhs,
        Type4Refusal::QuantizedLong32Overflow {
            quotient_bits: 0x4f00_0000,
        },
        "parameter.current16.lhs",
    );
}

#[test]
fn lhs_metadata_controls_both_operands_and_dispatch_precedes_alternatives() {
    let mut lhs = fields(Variant::Int32(0), 0x3f80_0000, 1);
    lhs.suppress_current156 = 1;
    lhs.packed260[1] = 1;
    let rhs = ParameterFields {
        type8: 2,
        step_count84: 0,
        step144: f(0x7fc0_0001),
        ..lhs.clone()
    };
    let a = bank(lhs, vec![v(0x3fc0_0000)]);
    let b = bank(rhs, vec![Variant::Int32(2)]);
    assert_eq!(compare_model(&a, &b, FINITE).unwrap(), Comparison::Equal);
    mismatch(compare_model(&b, &a, FINITE), "model.second52[0][0]");
    let mut rhs = b.fields().clone();
    rhs.second52[0][0] = Variant::String(Utf16::null());
    refusal(
        compare_model(&a, &Model::new(rhs).unwrap(), FINITE),
        OperandSide::Rhs,
        Type4Refusal::Variant { type_id: 10 },
        "model.second52[0][0].rhs",
    );
    assert_eq!(
        compare_model(&a, &b, LEGACY).unwrap_err().reason,
        UnsupportedReason::Type4Stepping
    );
}

#[test]
fn prepared_false_waits_for_word_bytes_and_type_but_refusal_is_early() {
    let lhs = fields(Variant::Int32(0), 0x3f80_0000, 1);
    for (rhs, path) in [
        (
            ParameterFields {
                opaque_word0: 1,
                current16: Variant::Int32(1),
                ..lhs.clone()
            },
            "parameter.opaque_word0",
        ),
        (
            ParameterFields {
                opaque_bytes4: vec![1],
                current16: Variant::Int32(1),
                ..lhs.clone()
            },
            "parameter.opaque_bytes4.length",
        ),
        (
            ParameterFields {
                type8: 2,
                current16: Variant::Int32(1),
                ..lhs.clone()
            },
            "parameter.type8",
        ),
    ] {
        mismatch(
            compare_parameter(&parameter(lhs.clone()), &parameter(rhs), FINITE),
            path,
        );
    }
    let lhs = ParameterFields {
        current16: Variant::String(Utf16::null()),
        ..lhs
    };
    let rhs = ParameterFields {
        opaque_word0: 1,
        ..lhs.clone()
    };
    refusal(
        compare_parameter(&parameter(lhs), &parameter(rhs), FINITE),
        OperandSide::Lhs,
        Type4Refusal::Variant { type_id: 10 },
        "parameter.current16.lhs",
    );
}

#[test]
fn suppression_skips_invalid_unused_current_but_reached_bank_refuses() {
    let lhs = ParameterFields {
        type8: 4,
        suppress_current156: 1,
        current16: Variant::UnsupportedTypeId(777),
        default32: v(0x7fc0_0001),
        ..ParameterFields::default()
    };
    let p = parameter(lhs.clone());
    assert_eq!(
        compare_parameter(&p, &p, FINITE).unwrap(),
        Comparison::Equal
    );
    let m = bank(lhs.clone(), vec![Variant::Int32(0)]);
    refusal(
        compare_model(&m, &m, FINITE),
        OperandSide::Lhs,
        Type4Refusal::NonPositiveStepCount { count: 0 },
        "model.second52[0][0].lhs",
    );
    let lhs = ParameterFields {
        suppress_current156: 0,
        ..lhs
    };
    refusal(
        compare_parameter(&parameter(lhs), &p, FINITE),
        OperandSide::Lhs,
        Type4Refusal::Variant { type_id: 777 },
        "parameter.current16.lhs",
    );
}

#[test]
fn ignore_cannot_erase_reached_current_or_bank_refusal() {
    let lhs = ParameterFields {
        packed260: [0, 1, 0, 0],
        ..fields(Variant::UnsupportedTypeId(999), 0x3f80_0000, 1)
    };
    let rhs = ParameterFields {
        opaque_word0: 1,
        ..lhs.clone()
    };
    refusal(
        compare_model(&bank(lhs.clone(), vec![]), &bank(rhs, vec![]), FINITE),
        OperandSide::Lhs,
        Type4Refusal::Variant { type_id: 999 },
        "model.descriptors28[0].current16.lhs",
    );
    let lhs = ParameterFields {
        suppress_current156: 1,
        ..lhs
    };
    let m = bank(lhs, vec![Variant::UnsupportedTypeId(999)]);
    refusal(
        compare_model(&m, &m, FINITE),
        OperandSide::Lhs,
        Type4Refusal::Variant { type_id: 999 },
        "model.second52[0][0].lhs",
    );
}

#[test]
fn bank_values_precede_cache_then_length_and_cache_short_circuit_later_operations() {
    let descriptor = fields(Variant::Int32(0), 0x3f80_0000, 1);
    let a = bank(descriptor.clone(), vec![Variant::Int32(0)]);
    let mut b = a.fields().clone();
    b.second52[0][0] = Variant::UnsupportedTypeId(999);
    b.second_cache96[0] = f(0x3f80_0000);
    refusal(
        compare_model(&a, &Model::new(b).unwrap(), FINITE),
        OperandSide::Rhs,
        Type4Refusal::Variant { type_id: 999 },
        "model.second52[0][0].rhs",
    );
    let mut b = a.fields().clone();
    b.second52[0].clear();
    mismatch(
        compare_model(&a, &Model::new(b).unwrap(), FINITE),
        "model.second52[0].length",
    );
    let invalid = fields(Variant::UnsupportedTypeId(999), 0, 0);
    let a = bank(invalid, vec![]);
    let mut b = a.fields().clone();
    b.second_cache96[0] = f(0x3f80_0000);
    b.second52[1].push(Variant::UnsupportedTypeId(999));
    mismatch(
        compare_model(&a, &Model::new(b).unwrap(), FINITE),
        "model.second_cache96[0]",
    );
}

#[test]
fn early_expression_mismatch_short_circuits_invalid_type4_bank() {
    // Reuse the exact existing preset fixture; only this test's expression changes.
    let mut a = baseline_fixture::fixture();
    let descriptor = fields(Variant::UnsupportedTypeId(999), 0, 0);
    a.expression5508 = bank(descriptor.clone(), vec![Variant::UnsupportedTypeId(999)]);
    let mut b = a.clone();
    let descriptor = ParameterFields {
        expression_float160: f(0x3f80_0000),
        ..descriptor
    };
    b.expression5508 = bank(descriptor, vec![Variant::UnsupportedTypeId(999)]);
    mismatch(
        compare(
            &AdmittedSnapshot::new(a).unwrap(),
            &AdmittedSnapshot::new(b).unwrap(),
            FINITE,
        ),
        "preset.expression5508.expression_precheck[0].expression_float160",
    );
}

#[test]
fn defaults_remain_unstepped_and_variant64_remains_direct() {
    let lhs = ParameterFields {
        default32: v(0x3fc0_0000),
        ..fields(Variant::Int32(0), 0x3f80_0000, 1)
    };
    let rhs = ParameterFields {
        default32: v(0x4000_0000),
        ..lhs.clone()
    };
    mismatch(
        compare_parameter(&parameter(lhs.clone()), &parameter(rhs), FINITE),
        "parameter.default32",
    );
    let nan_default = ParameterFields {
        default32: v(0x7fc0_0001),
        ..lhs
    };
    let p = parameter(nan_default.clone());
    assert_eq!(
        compare_parameter(&p, &p, FINITE).unwrap(),
        Comparison::Equal
    );
    let p = parameter(ParameterFields {
        opaque_variant64: v(0x7fc0_0001),
        ..nan_default
    });
    mismatch(
        compare_parameter(&p, &p, FINITE),
        "parameter.opaque_variant64",
    );
}

#[test]
fn opt_in_does_not_detect_or_resolve_runtime_policy() {
    let p = parameter(fields(Variant::Int32(0), 0x3f80_0000, 1));
    assert_eq!(
        compare_parameter(&p, &p, LEGACY).unwrap_err().reason,
        UnsupportedReason::Type4Stepping
    );
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
        let error = compare_parameter(&p, &p, policy).unwrap_err();
        assert_eq!(error.reason, reason);
        assert_eq!(error.path.to_string(), "policy");
    }
}

// Share only the existing fixture constructor, without duplicating its tests.
mod baseline_fixture {
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

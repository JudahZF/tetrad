use tetrad_core::comparison::*;

const ORDINARY: ComparisonPolicy = ComparisonPolicy::DefaultQtHandlerOrdinaryIeee;
const FINITE: ComparisonPolicy = ComparisonPolicy::AssertedFiniteType4DefaultQtHelpersOrdinaryIeee;

#[test]
fn constructor_and_admission_are_separate_from_storage() {
    let mut arena = ComparisonArena::default();
    let mut rejected = support::fields();
    rejected.default_scene5408 = SCENES as u32;
    let error = AdmittedSnapshot::new(rejected).unwrap_err();
    assert_eq!(error.path.to_string(), "preset.default_scene5408");
    assert_eq!(error.reason, AdmissionReason::InvalidDefaultScene(8));
    let handle = arena
        .insert(AdmittedSnapshot::new(support::fields()).unwrap())
        .unwrap();
    assert_eq!(arena.get(&handle).unwrap().fields().default_scene5408, 0);
    // Unsupported markers are retainable; they gain no comparator admission.
    let unknown = arena
        .insert(support::with_parameter(ParameterFields {
            opaque_variant64: Variant::UnsupportedTypeId(0),
            ..ParameterFields::default()
        }))
        .unwrap();
    match arena.compare(&unknown, &unknown, ORDINARY).unwrap_err() {
        CompareError::Unsupported(error) => {
            assert_eq!(
                error.path.to_string(),
                "preset.grid0[0][0].descriptors28[0].opaque_variant64.lhs"
            );
            assert_eq!(error.reason, UnsupportedReason::VariantType { type_id: 0 });
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn exact_capacity_failure_preserves_every_retained_identity_and_value() {
    let mut arena = ComparisonArena::default();
    let mut handles = Vec::new();
    for index in 0..MAX_CAPTURES {
        let mut preset = support::fields();
        preset.opaque_byte5825 = u8::try_from(index).unwrap();
        let handle = arena
            .insert(AdmittedSnapshot::new(preset).unwrap())
            .unwrap();
        for previous in &handles {
            assert_ne!(&handle, previous);
        }
        handles.push(handle);
    }
    for _ in 0..2 {
        assert_eq!(
            arena.insert(AdmittedSnapshot::new(support::fields()).unwrap()),
            Err(InsertError::CapacityReached)
        );
        for (index, handle) in handles.iter().enumerate() {
            assert_eq!(
                arena.get(handle).unwrap().fields().opaque_byte5825,
                u8::try_from(index).unwrap()
            );
            assert_eq!(
                arena.compare(handle, handle, ORDINARY).unwrap(),
                Comparison::Equal
            );
        }
    }
}

#[test]
fn clones_and_new_captures_do_not_replace_retained_content() {
    let mut arena = ComparisonArena::default();
    let mut fields = support::fields();
    fields.strings48 = vec![Utf16::from_literal("retained").unwrap()];
    let original = AdmittedSnapshot::new(fields).unwrap();
    let separate_copy = original.clone();
    let first = arena.insert(original).unwrap();
    let same_content = arena.insert(separate_copy).unwrap();
    assert_ne!(first, same_content);
    let retained_handle = first.clone();
    assert_eq!(first, retained_handle);
    assert!(std::ptr::eq(
        arena.get(&first).unwrap(),
        arena.get(&retained_handle).unwrap()
    ));
    let mut edited = arena.get(&first).unwrap().fields().clone();
    edited.strings48[0] = Utf16::from_literal("edited").unwrap();
    let next = arena
        .insert(AdmittedSnapshot::new(edited).unwrap())
        .unwrap();
    assert_eq!(
        arena.get(&retained_handle).unwrap().fields().strings48[0].units(),
        "retained".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(
        arena.compare(&first, &same_content, ORDINARY).unwrap(),
        Comparison::Equal
    );
    assert!(matches!(
        arena.compare(&first, &next, ORDINARY),
        Ok(Comparison::Mismatch { .. })
    ));
    // Moving ownership preserves the witness; it does not create a second arena.
    let moved = arena;
    assert!(moved.get(&retained_handle).is_ok());
}

#[test]
fn same_slot_in_two_arenas_refuses_in_both_directions_and_both_operand_roles() {
    let mut a = ComparisonArena::default();
    let mut b = ComparisonArena::default();
    let ah = a
        .insert(AdmittedSnapshot::new(support::fields()).unwrap())
        .unwrap();
    let bh = b
        .insert(AdmittedSnapshot::new(support::fields()).unwrap())
        .unwrap();
    assert_ne!(ah, bh);
    assert_eq!(a.get(&bh).unwrap_err(), HandleError::ForeignArena);
    assert_eq!(b.get(&ah).unwrap_err(), HandleError::ForeignArena);
    for (arena, own, foreign) in [(&a, &ah, &bh), (&b, &bh, &ah)] {
        assert_eq!(
            arena.compare(foreign, own, ORDINARY),
            Err(CompareError::LhsHandle(HandleError::ForeignArena))
        );
        assert_eq!(
            arena.compare(own, foreign, ORDINARY),
            Err(CompareError::RhsHandle(HandleError::ForeignArena))
        );
        assert_eq!(
            arena.compare(foreign, foreign, ComparisonPolicy::CustomOrLiveHandler),
            Err(CompareError::LhsHandle(HandleError::ForeignArena))
        );
    }
}

#[test]
fn retained_witness_after_drop_cannot_resolve_in_new_arena() {
    let old = {
        let mut arena = ComparisonArena::default();
        arena
            .insert(AdmittedSnapshot::new(support::fields()).unwrap())
            .unwrap()
    };
    let mut new = ComparisonArena::default();
    let fresh = new
        .insert(AdmittedSnapshot::new(support::fields()).unwrap())
        .unwrap();
    assert_ne!(old, fresh);
    assert_eq!(new.get(&old).unwrap_err(), HandleError::ForeignArena);
    assert_eq!(
        new.compare(&fresh, &old, ORDINARY),
        Err(CompareError::RhsHandle(HandleError::ForeignArena))
    );
}

#[test]
fn actual_direction_and_nontransitive_comparison_are_not_identity() {
    let mut arena = ComparisonArena::default();
    let ignore = arena
        .insert(support::with_parameter(ParameterFields {
            opaque_word0: 8,
            packed260: [0, 1, 0, 0],
            ..ParameterFields::default()
        }))
        .unwrap();
    let plain = arena
        .insert(support::with_parameter(ParameterFields::default()))
        .unwrap();
    assert_eq!(
        arena.compare(&ignore, &plain, ORDINARY).unwrap(),
        Comparison::Equal
    );
    match arena.compare(&plain, &ignore, ORDINARY).unwrap() {
        Comparison::Mismatch { path } => assert_eq!(
            path.to_string(),
            "preset.grid0[0][0].descriptors28[0].opaque_word0"
        ),
        other => panic!("unexpected result: {other:?}"),
    }
    let mut handles = Vec::new();
    // Adjacent differences are within 1e-5; the end-to-end difference exceeds it.
    for value in [1.0_f32, 1.0000075, 1.000015] {
        let mut fields = support::fields();
        fields.opaque_float5396 = F32::from_bits(value.to_bits()).unwrap();
        handles.push(
            arena
                .insert(AdmittedSnapshot::new(fields).unwrap())
                .unwrap(),
        );
    }
    assert_eq!(
        arena.compare(&handles[0], &handles[1], ORDINARY).unwrap(),
        Comparison::Equal
    );
    assert_eq!(
        arena.compare(&handles[1], &handles[2], ORDINARY).unwrap(),
        Comparison::Equal
    );
    assert!(matches!(
        arena.compare(&handles[0], &handles[2], ORDINARY),
        Ok(Comparison::Mismatch { .. })
    ));
}

#[test]
fn policy_and_exact_unsupported_direction_survive_resolution() {
    let mut arena = ComparisonArena::default();
    let int = arena
        .insert(support::with_parameter(ParameterFields {
            current16: Variant::Int32(7),
            ..ParameterFields::default()
        }))
        .unwrap();
    let string = arena
        .insert(support::with_parameter(ParameterFields {
            current16: Variant::String(Utf16::from_literal("007").unwrap()),
            ..ParameterFields::default()
        }))
        .unwrap();
    for (lhs, rhs, lhs_type, rhs_type) in [(&int, &string, 2, 10), (&string, &int, 10, 2)] {
        let expected =
            compare(arena.get(lhs).unwrap(), arena.get(rhs).unwrap(), ORDINARY).unwrap_err();
        assert_eq!(
            expected.path.to_string(),
            "preset.grid0[0][0].descriptors28[0].current16"
        );
        assert_eq!(
            expected.reason,
            UnsupportedReason::RhsToLhsConversion { lhs_type, rhs_type }
        );
        assert_eq!(
            arena.compare(lhs, rhs, ORDINARY),
            Err(CompareError::Unsupported(expected))
        );
    }
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
        match arena.compare(&int, &int, policy).unwrap_err() {
            CompareError::Unsupported(error) => {
                assert_eq!(error.path.to_string(), "policy");
                assert_eq!(error.reason, reason);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}

#[test]
fn finite_type4_requires_opt_in_and_preserves_typed_refusal() {
    let mut arena = ComparisonArena::default();
    let handle = arena
        .insert(support::with_parameter(ParameterFields {
            type8: 4,
            current16: Variant::Int32(2),
            step_count84: 1,
            step144: F32::from_bits(1.0_f32.to_bits()).unwrap(),
            ..ParameterFields::default()
        }))
        .unwrap();
    match arena.compare(&handle, &handle, ORDINARY).unwrap_err() {
        CompareError::Unsupported(error) => {
            assert_eq!(
                error.path.to_string(),
                "preset.grid0[0][0].descriptors28[0].current16"
            );
            assert_eq!(error.reason, UnsupportedReason::Type4Stepping);
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(
        arena.compare(&handle, &handle, FINITE).unwrap(),
        Comparison::Equal
    );
    let refused = arena
        .insert(support::with_parameter(ParameterFields {
            type8: 4,
            current16: Variant::ConstructorInvalid,
            ..ParameterFields::default()
        }))
        .unwrap();
    match arena.compare(&refused, &handle, FINITE).unwrap_err() {
        CompareError::Unsupported(error) => {
            assert_eq!(
                error.path.to_string(),
                "preset.grid0[0][0].descriptors28[0].current16.lhs"
            );
            assert_eq!(
                error.reason,
                UnsupportedReason::FiniteType4 {
                    side: OperandSide::Lhs,
                    reason: Type4Refusal::ConstructorInvalid
                }
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn reached_constructor_invalid_remains_limited_to_field64() {
    let mut arena = ComparisonArena::default();
    let unit = arena
        .insert(support::with_parameter(ParameterFields {
            opaque_variant64: Variant::ConstructorInvalid,
            ..ParameterFields::default()
        }))
        .unwrap();
    let known = arena
        .insert(support::with_parameter(ParameterFields::default()))
        .unwrap();
    assert_eq!(
        arena.compare(&unit, &unit, ORDINARY).unwrap(),
        Comparison::Equal
    );
    assert!(matches!(
        arena.compare(&unit, &known, ORDINARY),
        Ok(Comparison::Mismatch { .. })
    ));
    for (parameter, context, field) in [
        (
            ParameterFields {
                current16: Variant::ConstructorInvalid,
                ..ParameterFields::default()
            },
            VariantContext::Current16,
            "current16",
        ),
        (
            ParameterFields {
                default32: Variant::ConstructorInvalid,
                ..ParameterFields::default()
            },
            VariantContext::Default32,
            "default32",
        ),
    ] {
        let outside = arena.insert(support::with_parameter(parameter)).unwrap();
        for (lhs, rhs, side, name) in [
            (&outside, &known, OperandSide::Lhs, "lhs"),
            (&known, &outside, OperandSide::Rhs, "rhs"),
        ] {
            match arena.compare(lhs, rhs, ORDINARY).unwrap_err() {
                CompareError::Unsupported(error) => {
                    assert_eq!(
                        error.path.to_string(),
                        format!("preset.grid0[0][0].descriptors28[0].{field}.{name}")
                    );
                    assert_eq!(
                        error.reason,
                        UnsupportedReason::ConstructorInvalidOutside64 { side, context }
                    );
                }
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }
}

mod support {
    use tetrad_core::comparison::*;

    /// Invented complete local comparison fixture; no firmware initialization claim.
    pub fn fields() -> PresetFields {
        PresetFields {
            grid0: Vec::new(),
            graph12: Vec::new(),
            paired36: Vec::new(),
            strings48: Vec::new(),
            map5344: TraversalMap::default(),
            model_map5368: TraversalMap::default(),
            opaque_float5396: F32::default(),
            opaque_float5400: F32::default(),
            default_scene5408: 0,
            assignments64: [Assignment::default(); ASSIGNMENTS],
            assignment_groups544: [[Assignment::default(); ASSIGNMENTS]; ASSIGNMENT_GROUPS],
            scene_names5412: Vec::new(),
            group_labels5424: TraversalMap::default(),
            individual_labels5448: TraversalMap::default(),
            momentary5472: TraversalMap::default(),
            colors5496: Vec::new(),
            expression5508: Model::new(ModelFields::default()).unwrap(),
            opaque_string5800: Utf16::default(),
            opaque_string5820: Utf16::default(),
            opaque_byte5825: 0,
            omitted: PresetOmissions::default(),
        }
    }

    pub fn with_parameter(parameter: ParameterFields) -> AdmittedSnapshot {
        let mut preset = fields();
        preset.grid0 = vec![vec![
            Model::new(ModelFields {
                descriptors28: vec![Parameter::new(parameter).unwrap()],
                ..ModelFields::default()
            })
            .unwrap(),
        ]];
        AdmittedSnapshot::new(preset).unwrap()
    }
}

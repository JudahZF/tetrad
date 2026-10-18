use crate::*;

const ORDINARY: ResolvedParameterMetadata = ResolvedParameterMetadata { sidechain: false };

fn version(major: u32, minor: u32) -> Version {
    Version {
        major,
        minor,
        patch: 0,
    }
}

fn initialized() -> RuntimeParameter {
    RuntimeParameter {
        scenes: std::array::from_fn(|_| ParameterValue::Float(-0.25)),
        controls: ParameterControls {
            expression_assignment: 7,
            expression_min: -0.0,
            expression_max: f32::from_bits(0x7fc0_4567),
            scene_mode: true,
        },
    }
}

fn stored(count: usize) -> Vec<StoredParameterScenes> {
    (0..count)
        .map(|position| StoredParameterScenes {
            scenes: std::array::from_fn(|_| {
                Some(ParameterValue::Int(i32::try_from(position).unwrap()))
            }),
        })
        .collect()
}

#[test]
fn added_parameter_uses_unsigned_count_gap_and_version_threshold() {
    // T=6; gap one is ordinary, gap two suppresses candidate M-1.
    for (v, modern) in [
        (version(1, 9), false),
        (version(2, 0), true),
        (version(3, 0), true),
    ] {
        for (m, suppress) in [(6, false), (5, false), (4, modern), (7, false)] {
            let plan = ParameterImportPlan::new(42, 4, m, v).unwrap();
            let p = (m - 1).min(5);
            let expected = if suppress {
                SceneValueSource::Defaults(DefaultReason::AddedParameter)
            } else {
                SceneValueSource::Stored { position: p }
            };
            assert_eq!(plan.scene_source(p).unwrap(), expected);
            let output = plan
                .import_parameter(p, &initialized(), &stored(m), ORDINARY)
                .unwrap();
            assert_eq!(
                output.scenes[0],
                if suppress {
                    ParameterValue::Float(-0.25)
                } else {
                    ParameterValue::Int(i32::try_from(p).unwrap())
                }
            );
        }
    }
    // M>T is accepted safely; the verified subtraction wraps, not indexing.
    let plan = ParameterImportPlan::new(42, 4, 8, version(2, 0)).unwrap();
    for p in 0..6 {
        assert_eq!(
            plan.scene_source(p).unwrap(),
            SceneValueSource::Stored { position: p }
        );
    }
}

#[test]
fn special_models_swap_only_mute_scenes_when_counts_match() {
    for id in [5011, 5016] {
        for v in [version(1, 9), version(2, 0), version(3, 0)] {
            let plan = ParameterImportPlan::new(id, 4, 6, v).unwrap();
            assert_eq!(
                plan.scene_source(4).unwrap(),
                SceneValueSource::Stored { position: 5 }
            );
            assert_eq!(
                plan.scene_source(5).unwrap(),
                SceneValueSource::Stored { position: 5 }
            );
            let output = plan
                .import_parameter(4, &initialized(), &stored(6), ORDINARY)
                .unwrap();
            assert_eq!(output.scenes[0], ParameterValue::Int(5));
            // Caller supplied earlier destination-position controls survive.
            assert_eq!(output.controls.expression_assignment, 7);
            assert_eq!(
                output.controls.expression_min.to_bits(),
                (-0.0_f32).to_bits()
            );
            assert_eq!(output.controls.expression_max.to_bits(), 0x7fc0_4567);
            assert!(output.controls.scene_mode);
        }
        let plan = ParameterImportPlan::new(id, 4, 5, version(2, 0)).unwrap();
        assert_eq!(
            plan.scene_source(4).unwrap(),
            SceneValueSource::Stored { position: 4 }
        );
    }
}

#[test]
fn legacy_input_gain_resets_and_disables_fallback_until_three() {
    for v in [
        version(1, 9),
        version(2, 0),
        Version {
            major: 2,
            minor: 99,
            patch: 99,
        },
    ] {
        let plan = ParameterImportPlan::new(28000, 5, 7, v).unwrap();
        assert_eq!(
            plan.scene_source(3).unwrap(),
            SceneValueSource::Defaults(DefaultReason::LegacyInputGain)
        );
        assert_eq!(
            plan.scene_source(2).unwrap(),
            SceneValueSource::Stored { position: 2 }
        );
        let missing = ParameterImportPlan::new(28000, 5, 3, v).unwrap();
        assert_eq!(
            missing.scene_source(5).unwrap(),
            SceneValueSource::Defaults(DefaultReason::MissingPosition)
        );
    }
    let current = ParameterImportPlan::new(28000, 5, 7, version(3, 0)).unwrap();
    assert_eq!(
        current.scene_source(3).unwrap(),
        SceneValueSource::Stored { position: 3 }
    );
    let missing = ParameterImportPlan::new(28000, 5, 3, version(3, 0)).unwrap();
    assert_eq!(
        missing.scene_source(5).unwrap(),
        SceneValueSource::LastStoredFloats { position: 2 }
    );
    // For T=5, p=3 is also MUTE. Legacy guard still wins there.
    let overlapping = ParameterImportPlan::new(28000, 3, 5, version(2, 0)).unwrap();
    assert_eq!(
        overlapping.scene_source(3).unwrap(),
        SceneValueSource::Defaults(DefaultReason::LegacyInputGain)
    );
}

#[test]
fn ordinary_scenes_preserve_alternatives_float_bits_and_missing_defaults() {
    let nan = f32::from_bits(0x7fa1_2345);
    let values = [
        Some(ParameterValue::Int(i32::MIN)),
        Some(ParameterValue::Float(nan)),
        Some(ParameterValue::String("literal value".into())),
        None,
        Some(ParameterValue::Float(-0.0)),
        Some(ParameterValue::Float(f32::INFINITY)),
        Some(ParameterValue::Float(f32::NEG_INFINITY)),
        Some(ParameterValue::Float(2.0)),
    ];
    let plan = ParameterImportPlan::new(42, 0, 1, version(1, 9)).unwrap();
    let output = plan
        .import_parameter(
            0,
            &initialized(),
            &[StoredParameterScenes { scenes: values }],
            ORDINARY,
        )
        .unwrap();
    assert_eq!(output.scenes[0], ParameterValue::Int(i32::MIN));
    assert_eq!(float_bits(&output.scenes[1]), nan.to_bits());
    assert_eq!(
        output.scenes[2],
        ParameterValue::String("literal value".into())
    );
    assert_eq!(output.scenes[3], ParameterValue::Float(-0.25));
    assert_eq!(float_bits(&output.scenes[4]), (-0.0_f32).to_bits());
    assert_eq!(float_bits(&output.scenes[5]), f32::INFINITY.to_bits());
    assert_eq!(float_bits(&output.scenes[6]), f32::NEG_INFINITY.to_bits());
    assert_eq!(output.scenes[7], ParameterValue::Float(2.0));
}

fn float_bits(value: &ParameterValue) -> u32 {
    match value {
        ParameterValue::Float(value) => value.to_bits(),
        _ => panic!("expected float alternative"),
    }
}

#[test]
fn sidechain_clamps_only_available_float_upper_values() {
    let nan = f32::from_bits(0xffa1_4321);
    let values = [
        ParameterValue::Float(2.0),
        ParameterValue::Float(-2.0),
        ParameterValue::Float(nan),
        ParameterValue::Float(-0.0),
        ParameterValue::Float(f32::INFINITY),
        ParameterValue::Float(f32::NEG_INFINITY),
        ParameterValue::Int(2),
        ParameterValue::String("2".into()),
    ];
    let source = StoredParameterScenes {
        scenes: values.map(Some),
    };
    let output = ParameterImportPlan::new(42, 0, 1, version(1, 9))
        .unwrap()
        .import_parameter(
            0,
            &initialized(),
            &[source],
            ResolvedParameterMetadata { sidechain: true },
        )
        .unwrap();
    assert_eq!(output.scenes[0], ParameterValue::Float(1.0));
    assert_eq!(output.scenes[1], ParameterValue::Float(-2.0));
    assert_eq!(float_bits(&output.scenes[2]), nan.to_bits());
    assert_eq!(float_bits(&output.scenes[3]), (-0.0_f32).to_bits());
    assert_eq!(output.scenes[4], ParameterValue::Float(1.0));
    assert_eq!(float_bits(&output.scenes[5]), f32::NEG_INFINITY.to_bits());
    assert_eq!(output.scenes[6], ParameterValue::Int(2));
    assert_eq!(output.scenes[7], ParameterValue::String("2".into()));
}

#[test]
fn missing_mute_fallback_filters_present_variants_without_sidechain_clamp() {
    let nan = f32::from_bits(0x7fc0_1234);
    let mut source = stored(4);
    source[3].scenes = [
        Some(ParameterValue::Float(nan)),
        Some(ParameterValue::Int(8)),
        Some(ParameterValue::String("x".into())),
        None,
        Some(ParameterValue::Float(2.0)),
        Some(ParameterValue::Float(-2.0)),
        Some(ParameterValue::Float(-0.0)),
        None,
    ];
    for v in [version(2, 0), version(3, 0)] {
        let plan = ParameterImportPlan::new(42, 4, 4, v).unwrap();
        let output = plan
            .import_parameter(
                4,
                &initialized(),
                &source,
                ResolvedParameterMetadata { sidechain: true },
            )
            .unwrap();
        assert_eq!(float_bits(&output.scenes[0]), nan.to_bits());
        assert_eq!(output.scenes[1], ParameterValue::Float(0.0));
        assert_eq!(output.scenes[2], ParameterValue::Float(0.0));
        assert_eq!(output.scenes[3], ParameterValue::Float(-0.25));
        assert_eq!(output.scenes[4], ParameterValue::Float(2.0));
        assert_eq!(output.scenes[5], ParameterValue::Float(-2.0));
        assert_eq!(float_bits(&output.scenes[6]), (-0.0_f32).to_bits());
        assert_eq!(output.scenes[7], ParameterValue::Float(-0.25));
        assert_eq!(
            plan.scene_source(5).unwrap(),
            SceneValueSource::Defaults(DefaultReason::MissingPosition)
        );
    }
    let old = ParameterImportPlan::new(42, 4, 4, version(1, 9)).unwrap();
    assert_eq!(
        old.scene_source(4).unwrap(),
        SceneValueSource::Defaults(DefaultReason::MissingPosition)
    );
}

#[test]
fn malformed_counts_positions_and_empty_fallback_are_explicit_errors() {
    for xml_count in [usize::MAX, usize::try_from(i32::MAX).unwrap()] {
        assert!(matches!(
            ParameterImportPlan::new(42, xml_count, 0, version(2, 0)),
            Err(Error::ParameterCount(_))
        ));
    }
    assert!(matches!(
        ParameterImportPlan::new(42, 0, usize::MAX, version(2, 0)),
        Err(Error::ParameterCount(_))
    ));
    let empty = ParameterImportPlan::new(42, 0, 0, version(2, 0)).unwrap();
    assert_eq!(
        empty.scene_source(0),
        Err(Error::MissingLastParameter { destination: 0 })
    );
    assert_eq!(
        empty.scene_source(1).unwrap(),
        SceneValueSource::Defaults(DefaultReason::MissingPosition)
    );
    assert_eq!(
        empty.scene_source(2),
        Err(Error::ParameterPosition {
            position: 2,
            count: 2
        })
    );
    assert!(empty.scene_source(usize::MAX).is_err());
    assert_eq!(
        empty
            .import_parameter(1, &initialized(), &stored(1), ORDINARY)
            .unwrap_err(),
        Error::StoredParameterCount {
            expected: 0,
            actual: 1
        }
    );
    let old = ParameterImportPlan::new(42, 0, 0, version(1, 9)).unwrap();
    assert_eq!(
        old.scene_source(0).unwrap(),
        SceneValueSource::Defaults(DefaultReason::MissingPosition)
    );
    assert_eq!(
        Version::from_fields(Some(2), None, Some(0)),
        Err(Error::PartialVersion)
    );
}

#[test]
fn splitter_maps_copy_scenes_controls_and_use_external_type_encoding() {
    let mut source = vec![initialized(); 3];
    for (i, parameter) in source.iter_mut().enumerate() {
        parameter.scenes =
            std::array::from_fn(|scene| ParameterValue::String(format!("{i}/{scene}")));
        parameter.controls.expression_assignment = i32::try_from(i).unwrap();
        parameter.controls.scene_mode = i == 1;
    }
    source[0].scenes[7] = ParameterValue::Float(f32::from_bits(0x7fc0_6789));
    let defaults = vec![initialized(); 9];
    for (id, selector, expected) in [
        (10000, SplitterType::Type1, &[(0, 3), (1, 4), (2, 1)][..]),
        (10001, SplitterType::Type2, &[(0, 5), (1, 6), (2, 1)][..]),
        (10002, SplitterType::Type0, &[(0, 2), (1, 1)][..]),
    ] {
        let plan = LegacySplitterPlan::new(id).unwrap();
        assert_eq!(plan.selector(), selector);
        assert_eq!(plan.mappings(), expected);
        // Deliberately not a raw selector, to detect invented TYPE encoding.
        let output = plan
            .apply(
                &source,
                &defaults,
                ResolvedSplitterType {
                    selector,
                    encoded_value: 0.375,
                },
            )
            .unwrap();
        assert!(
            output[0]
                .scenes
                .iter()
                .all(|v| *v == ParameterValue::Float(0.375))
        );
        for &(from, to) in expected {
            assert_eq!(output[to].scenes[0], source[from].scenes[0]);
            assert_eq!(output[to].scenes[6], source[from].scenes[6]);
            assert_eq!(
                output[to].controls.expression_assignment,
                source[from].controls.expression_assignment
            );
            assert_eq!(
                output[to].controls.scene_mode,
                source[from].controls.scene_mode
            );
            assert_eq!(
                output[to].controls.expression_min.to_bits(),
                source[from].controls.expression_min.to_bits()
            );
            assert_eq!(
                output[to].controls.expression_max.to_bits(),
                source[from].controls.expression_max.to_bits()
            );
        }
        assert_eq!(float_bits(&output[expected[0].1].scenes[7]), 0x7fc0_6789);
        for (p, parameter) in output.iter().enumerate().skip(1) {
            if !expected.iter().any(|&(_, to)| to == p) {
                assert_eq!(parameter.scenes, defaults[p].scenes);
            }
        }
    }
}

#[test]
fn splitter_missing_sources_targets_and_wrong_encoding_fail_without_mutation() {
    let plan = LegacySplitterPlan::new(10001).unwrap();
    let encoding = ResolvedSplitterType {
        selector: plan.selector(),
        encoded_value: 0.5,
    };
    let source = vec![initialized(); 3];
    let target = vec![initialized(); 9];
    assert_eq!(
        plan.apply(&source[..2], &target, encoding).unwrap_err(),
        Error::SourceParameterPosition {
            position: 2,
            count: 2
        }
    );
    assert_eq!(
        plan.apply(&source, &target[..6], encoding).unwrap_err(),
        Error::ParameterPosition {
            position: 6,
            count: 6
        }
    );
    assert!(plan.apply(&source, &[], encoding).is_err());
    assert!(matches!(
        plan.apply(
            &source,
            &target,
            ResolvedSplitterType {
                selector: SplitterType::Type0,
                encoded_value: 0.0,
            }
        ),
        Err(Error::SplitterTypeEncoding { .. })
    ));
    assert!(matches!(
        LegacySplitterPlan::new(10004),
        Err(Error::UnsupportedSplitter(10004))
    ));
    assert_eq!(target[0].scenes[0], ParameterValue::Float(-0.25));
    assert_eq!(source[0].controls.expression_max.to_bits(), 0x7fc0_4567);
}

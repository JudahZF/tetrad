use super::*;

fn address(chain: i32, parameter: i32) -> ParameterAddress {
    ParameterAddress {
        chain: ChainIndex::new(chain).unwrap(),
        column: BankSlot::new(0).unwrap(),
        parameter: ParameterIndex::new(parameter).unwrap(),
    }
}

fn float(bits: u32) -> LiveParameterValue {
    LiveParameterValue::Float(FloatBits(bits))
}

fn parameter(position: i32) -> WorkingParameter {
    WorkingParameter {
        position: ParameterIndex::new(position).unwrap(),
        primary: std::array::from_fn(|index| LiveParameterValue::Int(index as i32)),
        second: std::array::from_fn(|index| LiveParameterValue::UInt(index as u32 + 10)),
        scene_mode: true,
        metadata: ParameterMetadata {
            route: ParameterRoute::Ordinary,
            binding_valid: true,
            visual_target_exists: true,
            model_block_needs_repaint: true,
            control: ParameterControlFacts {
                session_supported: true,
                kind: ParameterControlKind::Float,
            },
        },
        linked_scene_mode: Vec::new(),
        dependencies: Vec::new(),
    }
}

fn model(chain: i32, parameters: Vec<WorkingParameter>) -> WorkingParameterModel {
    WorkingParameterModel {
        chain: ChainIndex::new(chain).unwrap(),
        column: BankSlot::new(0).unwrap(),
        facts: ParameterModelFacts {
            model_id: 1000,
            category_id: 1,
            column_identity: 0,
            runtime_kind: Some(0),
            coupling: CouplingFacts::ResolvedAbsent,
            force: false,
        },
        parameters,
    }
}

fn base_core() -> ApplicationCore {
    ApplicationCore::new(WorkflowInit {
        working: SceneStorage::new(Vec::new()).unwrap(),
        alternate: None,
        baseline: BaselineId(7),
        mode: ModeId::new(0).unwrap(),
        previous_mode: ModeId::new(0).unwrap(),
        page: 0,
        ui_dirty: false,
        last_notified_dirty: false,
        position: BankPosition {
            bank: CallerValidatedBank(0),
            slot: BankSlot::new(0).unwrap(),
        },
        title: UiText::default(),
        requested: None,
        loaded: None,
    })
}

fn core(models: Vec<WorkingParameterModel>) -> ApplicationCore {
    let mut core = base_core();
    act(
        &mut core,
        ParameterAction::InstallWorkingParameters(
            WorkingParameters::new(SceneIndex::new(2).unwrap(), models).unwrap(),
        ),
        0,
    );
    core
}

fn act(core: &mut ApplicationCore, action: ParameterAction, now: u64) -> Vec<Effect> {
    core.dispatch(Action::Parameter(action), Millis(now))
        .unwrap()
}

fn edit(
    target: ParameterAddress,
    requested: LiveParameterValue,
    predicate: bool,
) -> ParameterAction {
    ParameterAction::EditorParameterEdit {
        target,
        requested,
        float_dispatch: FloatDispatchPredicate::Resolved(predicate),
    }
}

fn stored(core: &ApplicationCore, target: ParameterAddress) -> &WorkingParameter {
    core.state()
        .working_parameters
        .as_ref()
        .unwrap()
        .parameter(target)
        .unwrap()
}

fn parameter_effects(effects: &[Effect]) -> Vec<&ParameterEffect> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Parameter(effect) => Some(effect),
            _ => None,
        })
        .collect()
}

fn comparison(core: &ApplicationCore, differs: bool) -> SemanticComparison {
    SemanticComparison {
        effective: core.state().effective_stamp(),
        baseline: core.state().baseline,
        differs,
    }
}

#[test]
fn exact_variants_banks_selected_scene_and_default_scene_share_the_core() {
    let target = address(0, 0);
    let text = UiText::from_units(vec![65, 0, 0xd800, 0xffff]).unwrap();
    for value in [
        float(0x8000_0000),
        float(0x7fc1_2345),
        float(0x7f80_0000),
        LiveParameterValue::Int(i32::MIN),
        LiveParameterValue::UInt(u32::MAX),
        LiveParameterValue::String(text),
    ] {
        let mut core = core(vec![model(0, vec![parameter(0)])]);
        core.dispatch(
            Action::SelectScene {
                scene: SceneIndex::new(6).unwrap(),
                force: false,
            },
            Millis(1),
        )
        .unwrap();
        let initial = stored(&core, target).clone();
        let stamp = core.state().effective_stamp();
        let effects = act(&mut core, edit(target, value.clone(), false), 1);
        let parameter = stored(&core, target);
        for index in 0..8 {
            assert_eq!(
                parameter.primary[index],
                if index == 6 {
                    &value
                } else {
                    &initial.primary[index]
                }
                .clone()
            );
            assert_eq!(
                parameter.second[index],
                if index == 6 {
                    &value
                } else {
                    &initial.second[index]
                }
                .clone()
            );
        }
        assert!(core.state().effective_stamp().revision > stamp.revision);
        assert_eq!(
            core.state()
                .working_parameters
                .as_ref()
                .unwrap()
                .default_scene()
                .get(),
            2
        );
        assert_eq!(core.state().scene.get(), 6);
        assert!(core.state().dirty_deadline.is_none());
        assert!(!core.state().ui_dirty);
        assert!(
            matches!(parameter_effects(&effects)[0], ParameterEffect::StoredSceneWritten {
            scene, bank: ParameterBank::Second, .. } if scene.get() == 6)
        );
        assert!(
            matches!(parameter_effects(&effects)[1], ParameterEffect::StoredSceneWritten {
            scene, bank: ParameterBank::Primary, .. } if scene.get() == 6)
        );

        // Low-level source1 writes primary only and schedules no UI/dirty effects.
        let effects = act(
            &mut core,
            ParameterAction::SetStoredParameterScene {
                target,
                scene: SceneIndex::new(4).unwrap(),
                value: value.clone(),
                source: ParameterWriteSource::PrimaryOnly,
            },
            2,
        );
        assert!(effects.is_empty());
        assert_eq!(stored(&core, target).primary[4], value);
        assert_eq!(stored(&core, target).second[4], initial.second[4]);
        act(
            &mut core,
            ParameterAction::DisableParameterSceneMode {
                target,
                float_dispatch: vec![FloatDispatchPredicate::Unresolved],
            },
            3,
        ); // Default scene is Int2, so no Float predicate is needed.
        assert_eq!(
            stored(&core, target).primary,
            std::array::from_fn(|_| LiveParameterValue::Int(2))
        );
        let effects = act(&mut core, edit(target, value.clone(), false), 4);
        assert_eq!(
            stored(&core, target).primary,
            std::array::from_fn(|_| value.clone())
        );
        assert_eq!(stored(&core, target).second, stored(&core, target).primary);
        let writes: Vec<_> = parameter_effects(&effects)
            .into_iter()
            .filter_map(|effect| match effect {
                ParameterEffect::StoredSceneWritten { scene, bank, .. } => {
                    Some((scene.get(), *bank))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            writes,
            (0..8)
                .flat_map(|index| [
                    (index, ParameterBank::Second),
                    (index, ParameterBank::Primary)
                ])
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(
        FloatBits(0x7fc1_2345).finite_dsp_value(),
        Err(WorkflowError::NonFiniteDspValue)
    );
    assert_eq!(
        FloatBits(0x7f80_0000).finite_dsp_value(),
        Err(WorkflowError::NonFiniteDspValue)
    );
    assert_eq!(
        FloatBits(0x8000_0000).finite_dsp_value().unwrap().to_bits(),
        0x8000_0000
    );
}

#[test]
fn float_predicate_repaint_control_and_tail_keep_original_request_identity() {
    let target = address(0, 0);
    let requested = FloatBits(1.4_f32.to_bits());
    for (binding, visual, predicate, session) in [
        (true, true, false, true),
        (true, true, true, true),
        (true, false, false, true),
        (false, true, false, false),
    ] {
        let mut p = parameter(0);
        p.metadata.binding_valid = binding;
        p.metadata.visual_target_exists = visual;
        p.metadata.control.session_supported = session;
        let mut core = core(vec![model(0, vec![p])]);
        let effects = act(
            &mut core,
            edit(target, LiveParameterValue::Float(requested), predicate),
            0,
        );
        let tail = &parameter_effects(&effects)[2..];
        let mut expected = Vec::new();
        if binding {
            if visual {
                expected.push(ParameterEffect::VisualFloat48 { target, requested });
            }
            expected.push(ParameterEffect::ModelBlockNeedsRepaint { target });
            expected.push(ParameterEffect::FloatDispatchEvaluated {
                target,
                predicate: !visual || predicate,
            });
            if !visual || predicate {
                expected.push(ParameterEffect::GenericFloatSend { target, requested });
            }
        }
        if session {
            let ParameterEffect::StoredCurrentSceneControl {
                observation,
                metadata_kind,
                stored,
            } = tail[expected.len()]
            else {
                panic!("control order")
            };
            assert_eq!(*metadata_kind, ParameterControlKind::Float);
            assert_eq!(*stored, LiveParameterValue::Float(requested));
            assert_eq!(observation.primary[0], *stored);
            assert_eq!(observation.second[0], *stored);
            assert_eq!(observation.effective, core.state().effective_stamp());
            expected.push((*tail[expected.len()]).clone());
        }
        if visual {
            expected.push(ParameterEffect::VisualFloat96 { target, requested });
        }
        expected.push(ParameterEffect::EditTimerStopped {
            chain: target.chain,
        });
        expected.push(ParameterEffect::EditTimerRestarted {
            chain: target.chain,
            timer: core.state().parameter_edit_timers[0].unwrap(),
        });
        assert_eq!(tail, expected.iter().collect::<Vec<_>>());
        assert_eq!(
            stored(&core, target).primary[0],
            LiveParameterValue::Float(requested)
        );
        assert!(
            effects
                .iter()
                .all(|effect| matches!(effect.adapter_support(), AdapterSupport::Unsupported(_)))
        );
    }
    // Missing visual target supplies true even when the +48 factor is unresolved.
    let mut p = parameter(0);
    p.metadata.visual_target_exists = false;
    let mut core = core(vec![model(0, vec![p])]);
    let effects = act(
        &mut core,
        ParameterAction::EditorParameterEdit {
            target,
            requested: float(0x7f80_0000),
            float_dispatch: FloatDispatchPredicate::Unresolved,
        },
        0,
    );
    assert!(parameter_effects(&effects).iter().any(|effect| matches!(
        effect,
        ParameterEffect::GenericFloatSend {
            requested: FloatBits(0x7f80_0000),
            ..
        }
    )));
}

#[test]
fn uint_string_and_int_have_distinct_generic_phases_and_control_metadata() {
    let target = address(0, 0);
    let text = UiText::from_units(vec![0xd800, 0, 65]).unwrap();
    for (value, kind) in [
        (
            LiveParameterValue::UInt(u32::MAX),
            ParameterControlKind::Int,
        ),
        (
            LiveParameterValue::String(text.clone()),
            ParameterControlKind::String,
        ),
        (LiveParameterValue::Int(-123), ParameterControlKind::Int),
    ] {
        let mut p = parameter(0);
        p.metadata.control.kind = kind;
        let mut core = core(vec![model(0, vec![p])]);
        let effects = act(&mut core, edit(target, value.clone(), true), 0);
        let tail = &parameter_effects(&effects)[2..];
        match &value {
            LiveParameterValue::UInt(_) => assert_eq!(
                tail[0],
                &ParameterEffect::UIntHelper {
                    target,
                    requested: u32::MAX
                }
            ),
            LiveParameterValue::String(_) => assert_eq!(
                tail[0],
                &ParameterEffect::VisualString52 {
                    target,
                    requested: text.clone()
                }
            ),
            LiveParameterValue::Int(_) => assert!(matches!(
                tail[0],
                ParameterEffect::StoredCurrentSceneControl { .. }
            )),
            LiveParameterValue::Float(_) => unreachable!(),
        }
        assert!(!tail.iter().any(|effect| matches!(
            effect,
            ParameterEffect::GenericFloatSend { .. }
                | ParameterEffect::VisualFloat96 { .. }
                | ParameterEffect::VisualFloat48 { .. }
                | ParameterEffect::ModelBlockNeedsRepaint { .. }
        )));
        assert!(tail.iter().any(|effect| matches!(effect,
            ParameterEffect::StoredCurrentSceneControl { metadata_kind, stored, .. }
                if *metadata_kind == kind && *stored == value)));
    }
}

#[test]
fn enable_preserves_banks_and_direct_link_order_duplicates_and_same_flag_effects() {
    let a = address(0, 0);
    let b = address(0, 1);
    let c = address(0, 2);
    let mut p = parameter(0);
    p.linked_scene_mode = vec![b, b];
    p.dependencies = vec![c];
    let mut q = parameter(1);
    q.linked_scene_mode = vec![c];
    let mut r = parameter(2);
    r.scene_mode = false;
    let mut core = core(vec![model(0, vec![p, q, r])]);
    let banks = core.state().working_parameters.clone();
    for now in [1, 2] {
        let effects = act(
            &mut core,
            ParameterAction::EnableParameterSceneMode { target: a },
            now,
        );
        let targets: Vec<_> = parameter_effects(&effects)
            .into_iter()
            .filter_map(|effect| match effect {
                ParameterEffect::SceneModeSignal(observation) => Some(observation.target),
                _ => None,
            })
            .collect();
        assert_eq!(targets, vec![a, b, b]);
        assert_eq!(effects.len(), 9);
        for chunk in effects.chunks_exact(3) {
            assert!(matches!(
                chunk[0],
                Effect::Parameter(ParameterEffect::SceneModeSignal(_))
            ));
            assert_eq!(
                chunk[1],
                Effect::DirtyTimerRestarted {
                    deadline: Millis(now + 100)
                }
            );
            assert!(matches!(
                chunk[2],
                Effect::Parameter(ParameterEffect::SceneModeControl { enabled: true, .. })
            ));
        }
        assert!(!stored(&core, c).scene_mode);
        for target in [a, b, c] {
            let original = banks.as_ref().unwrap().parameter(target).unwrap();
            assert_eq!(stored(&core, target).primary, original.primary);
            assert_eq!(stored(&core, target).second, original.second);
        }
        assert_eq!(core.state().parameter_edit_timers, [None; 4]);
        assert!(!core.state().ui_dirty);
    }
}

#[test]
fn disable_snapshots_each_own_default_and_repeats_general_setter_without_outer_tail() {
    let a = address(0, 0);
    let b = address(0, 1);
    let mut p = parameter(0);
    p.primary[2] = float(0x7fc1_2345);
    p.linked_scene_mode = vec![b, b];
    let mut q = parameter(1);
    q.primary[2] = LiveParameterValue::UInt(u32::MAX);
    let mut core = core(vec![model(0, vec![p, q])]);
    core.dispatch(
        Action::SelectScene {
            scene: SceneIndex::new(6).unwrap(),
            force: false,
        },
        Millis(0),
    )
    .unwrap();
    let values = [
        float(0x7fc1_2345),
        LiveParameterValue::UInt(u32::MAX),
        LiveParameterValue::UInt(u32::MAX),
    ];
    let effects = act(
        &mut core,
        ParameterAction::DisableParameterSceneMode {
            target: a,
            float_dispatch: vec![FloatDispatchPredicate::Resolved(false); 3],
        },
        5,
    );
    assert!(effects.len() <= MAX_EFFECTS_PER_ACTION);
    let signals: Vec<_> = parameter_effects(&effects)
        .into_iter()
        .filter_map(|effect| match effect {
            ParameterEffect::SceneModeSignal(observation) => Some(observation),
            _ => None,
        })
        .collect();
    assert_eq!(
        signals
            .iter()
            .map(|observation| observation.target)
            .collect::<Vec<_>>(),
        [a, b, b]
    );
    for (observation, value) in signals.iter().zip(&values) {
        assert!(!observation.scene_mode);
        assert_eq!(observation.selected_scene.get(), 6);
        assert_eq!(observation.default_scene.get(), 2);
        assert_eq!(observation.primary, std::array::from_fn(|_| value.clone()));
        assert_eq!(observation.second, observation.primary);
    }
    assert!(signals[0].effective.revision < signals[1].effective.revision);
    assert!(signals[1].effective.revision < signals[2].effective.revision);
    let markers = parameter_effects(&effects);
    assert_eq!(
        markers
            .iter()
            .filter(|effect| matches!(effect, ParameterEffect::StoredSceneWritten { .. }))
            .count(),
        96
    );
    assert_eq!(
        markers
            .iter()
            .filter(|effect| matches!(effect, ParameterEffect::UIntHelper { .. }))
            .count(),
        2
    );
    assert!(!markers.iter().any(|effect| matches!(
        effect,
        ParameterEffect::VisualFloat96 { .. } | ParameterEffect::EditTimerRestarted { .. }
    )));
    for (index, effect) in markers.iter().enumerate() {
        if matches!(effect, ParameterEffect::SceneModeSignal(_)) {
            assert!(matches!(
                markers[index + 1],
                ParameterEffect::SceneModeControl { enabled: false, .. }
            ));
            assert!(matches!(
                markers[index + 2],
                ParameterEffect::StoredCurrentSceneControl { .. }
            ));
            assert!(matches!(
                markers[index + 3],
                ParameterEffect::StoredSceneWritten {
                    bank: ParameterBank::Second,
                    ..
                }
            ));
        }
    }
    assert_eq!(core.state().dirty_deadline, Some(Millis(105)));
    assert!(!core.state().ui_dirty);
    // Already-false disable still collapses, signals and dispatches all turns.
    let repeated = act(
        &mut core,
        ParameterAction::DisableParameterSceneMode {
            target: a,
            float_dispatch: vec![FloatDispatchPredicate::Resolved(false); 3],
        },
        6,
    );
    assert_eq!(repeated.len(), effects.len());
}

#[test]
fn four_chain_timers_restart_then_schedule_the_same_dirty_timer_without_latching() {
    let mut core = core(
        (0..4)
            .map(|chain| model(chain, vec![parameter(0)]))
            .collect(),
    );
    let mut timers = Vec::new();
    for chain in 0..4 {
        act(
            &mut core,
            edit(address(chain, 0), LiveParameterValue::Int(9), true),
            chain as u64 * 10,
        );
        timers.push(core.state().parameter_edit_timers[chain as usize].unwrap());
    }
    assert!(core.state().dirty_deadline.is_none());
    let old = timers[0];
    act(
        &mut core,
        edit(address(0, 0), LiveParameterValue::Int(9), true),
        100,
    );
    let same_clock = core.state().parameter_edit_timers[0].unwrap();
    act(
        &mut core,
        edit(address(0, 0), LiveParameterValue::Int(0), true),
        100,
    ); // Revert.
    let latest = core.state().parameter_edit_timers[0].unwrap();
    assert_eq!(latest.deadline, same_clock.deadline);
    assert_ne!(latest.generation, same_clock.generation);
    assert!(
        act(
            &mut core,
            ParameterAction::ParameterEditTimeout {
                chain: address(0, 0).chain,
                timer: old
            },
            500
        )
        .is_empty()
    );
    for chain in 1..4 {
        let deadline = timers[chain as usize].deadline.0;
        let effects = act(
            &mut core,
            ParameterAction::ParameterEditTimeout {
                chain: address(chain, 0).chain,
                timer: timers[chain as usize],
            },
            deadline,
        );
        assert_eq!(
            effects,
            vec![
                Effect::Parameter(ParameterEffect::ChainDirty {
                    chain: address(chain, 0).chain
                }),
                Effect::DirtyTimerRestarted {
                    deadline: Millis(deadline + 100)
                }
            ]
        );
        assert_eq!(core.state().dirty_deadline, Some(Millis(deadline + 100)));
    }
    assert!(
        act(
            &mut core,
            ParameterAction::ParameterEditTimeout {
                chain: address(0, 0).chain,
                timer: same_clock
            },
            600
        )
        .is_empty()
    );
    act(
        &mut core,
        ParameterAction::ParameterEditTimeout {
            chain: address(0, 0).chain,
            timer: latest,
        },
        600,
    );
    assert_eq!(core.state().dirty_deadline, Some(Millis(700)));
    let clean = comparison(&core, false);
    assert!(
        core.dispatch(Action::DirtyTimeout(clean), Millis(630))
            .unwrap()
            .is_empty()
    );
    let evaluated = core
        .dispatch(Action::DirtyTimeout(clean), Millis(700))
        .unwrap();
    assert_eq!(
        evaluated.last(),
        Some(&Effect::DirtyNotificationCacheStored(false))
    );
    assert!(!core.state().ui_dirty);
    assert!(!core.state().last_notified_dirty);
    assert!(
        !evaluated
            .iter()
            .any(|effect| matches!(effect, Effect::Control(_) | Effect::Observer(_)))
    );
    assert_eq!(core.state().parameter_edit_timers, [None; 4]);

    act(
        &mut core,
        edit(address(0, 0), LiveParameterValue::Int(1), true),
        710,
    );
    let edit_timer = core.state().parameter_edit_timers[0].unwrap();
    // Scene-mode signals use the existing dirty timer before chain timeout.
    act(
        &mut core,
        ParameterAction::EnableParameterSceneMode {
            target: address(1, 0),
        },
        720,
    );
    let dirty = comparison(&core, true);
    let effects = core
        .dispatch(Action::DirtyTimeout(dirty), Millis(820))
        .unwrap();
    assert!(matches!(
        effects[2],
        Effect::Observer(ObserverEvent::DirtyChanged(DirtyObservation {
            ui_dirty: true,
            last_notified_dirty: false,
            ..
        }))
    ));
    assert_eq!(core.state().parameter_edit_timers[0], Some(edit_timer));
    act(
        &mut core,
        edit(address(0, 0), LiveParameterValue::Int(0), true),
        830,
    );
    let timer = core.state().parameter_edit_timers[0].unwrap();
    act(
        &mut core,
        ParameterAction::ParameterEditTimeout {
            chain: address(0, 0).chain,
            timer,
        },
        1330,
    );
    let clean = comparison(&core, false);
    let effects = core
        .dispatch(Action::DirtyTimeout(clean), Millis(1430))
        .unwrap();
    assert!(matches!(
        effects[2],
        Effect::Observer(ObserverEvent::DirtyChanged(DirtyObservation {
            ui_dirty: false,
            last_notified_dirty: true,
            ..
        }))
    ));
    assert_eq!(
        effects.last(),
        Some(&Effect::DirtyNotificationCacheStored(false))
    );
    assert!(!core.state().ui_dirty);
}

#[test]
fn linked_unsupported_policies_are_fully_audited_before_any_mutation() {
    let a = address(0, 0);
    let b = address(1, 0);
    let c = address(2, 0);
    let mut p = parameter(0);
    p.linked_scene_mode = vec![b, c];
    let normal = model(0, vec![p]);
    let linked = model(1, vec![parameter(0)]);
    let bad = model(2, vec![parameter(0)]);
    let mut variants = Vec::new();
    for route in [
        ParameterRoute::Unresolved,
        ParameterRoute::Mute,
        ParameterRoute::Bypass,
        ParameterRoute::Momentary,
        ParameterRoute::Selector,
        ParameterRoute::Coupled,
    ] {
        let mut variant = bad.clone();
        variant.parameters[0].metadata.route = route;
        variants.push(variant);
    }
    for facts in [
        ParameterModelFacts {
            force: true,
            ..bad.facts
        },
        ParameterModelFacts {
            coupling: CouplingFacts::Unresolved,
            ..bad.facts
        },
        ParameterModelFacts {
            coupling: CouplingFacts::Present,
            ..bad.facts
        },
        ParameterModelFacts {
            runtime_kind: None,
            ..bad.facts
        },
        ParameterModelFacts {
            runtime_kind: Some(1),
            ..bad.facts
        },
        ParameterModelFacts {
            category_id: 26,
            ..bad.facts
        },
        ParameterModelFacts {
            model_id: 27000,
            ..bad.facts
        },
        ParameterModelFacts {
            model_id: 7040,
            ..bad.facts
        },
    ] {
        let mut variant = bad.clone();
        variant.facts = facts;
        variants.push(variant);
    }
    for column_identity in [11000, 23000, 25000, 28000, 10004] {
        let mut variant = bad.clone();
        variant.facts.column_identity = column_identity;
        variants.push(variant);
    }
    for variant in variants {
        let mut core = core(vec![normal.clone(), linked.clone(), variant]);
        for action in [
            ParameterAction::EnableParameterSceneMode { target: a },
            ParameterAction::DisableParameterSceneMode {
                target: a,
                float_dispatch: vec![FloatDispatchPredicate::Unresolved; 3],
            },
        ] {
            let before = core.clone();
            assert!(matches!(
                core.dispatch(Action::Parameter(action), Millis(1)),
                Err(WorkflowError::UnsupportedParameterPolicy(_))
            ));
            assert_eq!(core, before);
        }
    }
    // Disable-only known model routes do not become ordinary from a known ID.
    for model_id in [18007, 29001, 29008, 3008] {
        let mut bad = bad.clone();
        bad.facts.model_id = model_id;
        let mut core = core(vec![normal.clone(), linked.clone(), bad]);
        let before = core.clone();
        assert_eq!(
            core.dispatch(
                Action::Parameter(ParameterAction::DisableParameterSceneMode {
                    target: a,
                    float_dispatch: vec![FloatDispatchPredicate::Unresolved; 3],
                }),
                Millis(1)
            ),
            Err(unsupported(UnsupportedParameterPolicy::SpecialModel))
        );
        assert_eq!(core, before);
    }
    // Editor model/position routes need an explicit supported slice too.
    for (model_id, position) in [(26001, 1), (27000, 19), (27000, 20)] {
        let mut model = model(0, vec![parameter(position)]);
        model.facts.model_id = model_id;
        let mut core = core(vec![model]);
        let before = core.clone();
        assert_eq!(
            core.dispatch(
                Action::Parameter(edit(address(0, position), float(0), true)),
                Millis(1)
            ),
            Err(unsupported(UnsupportedParameterPolicy::SpecialModel))
        );
        assert_eq!(core, before);
    }
}

#[test]
fn malformed_addresses_predicates_clock_revision_timer_and_effect_errors_are_atomic() {
    for index in [-1, 100, i32::MAX] {
        assert_eq!(
            ParameterIndex::new(index),
            Err(WorkflowError::InvalidParameterIndex)
        );
    }
    for index in [-1, 4, i32::MAX] {
        assert_eq!(
            ChainIndex::new(index),
            Err(WorkflowError::InvalidParameterIndex)
        );
    }
    let target = address(0, 0);
    let mut core = core(vec![model(0, vec![parameter(0)])]);
    for (action, now, expected) in [
        (
            edit(address(0, 1), float(0), true),
            1,
            WorkflowError::ParameterAddress,
        ),
        (
            ParameterAction::EditorParameterEdit {
                target,
                requested: float(0),
                float_dispatch: FloatDispatchPredicate::Unresolved,
            },
            1,
            WorkflowError::UnresolvedFloatDispatch,
        ),
        (
            ParameterAction::DisableParameterSceneMode {
                target,
                float_dispatch: vec![],
            },
            1,
            WorkflowError::ParameterDispatchCount,
        ),
        (
            edit(target, float(0), true),
            u64::MAX - 499,
            WorkflowError::Overflow,
        ),
        (
            ParameterAction::EnableParameterSceneMode { target },
            u64::MAX - 99,
            WorkflowError::Overflow,
        ),
    ] {
        let before = core.clone();
        assert_eq!(
            core.dispatch(Action::Parameter(action), Millis(now)),
            Err(expected)
        );
        assert_eq!(core, before);
    }
    act(&mut core, edit(target, float(0), true), 20);
    let before = core.clone();
    assert_eq!(
        core.dispatch(Action::Parameter(edit(target, float(0), true)), Millis(19)),
        Err(WorkflowError::ClockRegression)
    );
    assert_eq!(core, before);
    let stale = comparison(&core, true);
    act(&mut core, edit(target, float(0), true), 20);
    let before = core.clone();
    assert_eq!(
        core.dispatch(Action::ForceDirtyEvaluation(stale), Millis(20)),
        Err(WorkflowError::StaleComparison)
    );
    assert_eq!(core, before);
    core.state.working_revision = u64::MAX;
    let before = core.clone();
    assert_eq!(
        core.dispatch(Action::Parameter(edit(target, float(0), true)), Millis(20)),
        Err(WorkflowError::Overflow)
    );
    assert_eq!(core, before);
    core.state.working_revision = 0;
    core.state.parameter_timer_generations[0] = u64::MAX;
    let before = core.clone();
    assert_eq!(
        core.dispatch(Action::Parameter(edit(target, float(0), true)), Millis(20)),
        Err(WorkflowError::Overflow)
    );
    assert_eq!(core, before);
    core.state.parameter_timer_generations[0] = 0;
    let timer = ParameterEditTimer {
        deadline: Millis(u64::MAX - 99),
        generation: 9,
    };
    core.state.parameter_edit_timers[0] = Some(timer);
    let before = core.clone();
    assert_eq!(
        core.dispatch(
            Action::Parameter(ParameterAction::ParameterEditTimeout {
                chain: target.chain,
                timer
            }),
            Millis(u64::MAX - 99)
        ),
        Err(WorkflowError::Overflow)
    );
    assert_eq!(core, before);
    core.state.alternate = Some(SceneStorage::new(Vec::new()).unwrap());
    let before = core.clone();
    assert_eq!(
        core.dispatch(Action::Parameter(edit(target, float(0), true)), Millis(20)),
        Err(unsupported(UnsupportedParameterPolicy::AlternateContext))
    );
    assert_eq!(core, before);

    let mut p = parameter(0);
    p.linked_scene_mode = vec![target; 3];
    let mut core = super::tests::core(vec![model(0, vec![p])]);
    let before = core.clone();
    assert_eq!(
        core.dispatch(
            Action::Parameter(ParameterAction::DisableParameterSceneMode {
                target,
                float_dispatch: vec![FloatDispatchPredicate::Unresolved; 4],
            }),
            Millis(1)
        ),
        Err(WorkflowError::ResourceLimit)
    );
    assert_eq!(core, before);
    // A late linked Float factor is audited before any flag/bank/clock changes.
    let mut p = parameter(0);
    p.linked_scene_mode = vec![address(0, 1)];
    let mut q = parameter(1);
    q.primary[2] = float(0);
    let mut core = super::tests::core(vec![model(0, vec![p, q])]);
    let before = core.clone();
    assert_eq!(
        core.dispatch(
            Action::Parameter(ParameterAction::DisableParameterSceneMode {
                target,
                float_dispatch: vec![
                    FloatDispatchPredicate::Resolved(true),
                    FloatDispatchPredicate::Unresolved
                ],
            }),
            Millis(1)
        ),
        Err(WorkflowError::UnresolvedFloatDispatch)
    );
    assert_eq!(core, before);
}

#[test]
fn installation_and_text_resources_are_bounded_without_changing_init_contracts() {
    let target = address(0, 0);
    let mut p = parameter(0);
    p.linked_scene_mode = vec![target; MAX_PARAMETER_LINKS + 1];
    assert_eq!(
        WorkingParameters::new(SceneIndex(0), vec![model(0, vec![p])]),
        Err(WorkflowError::ResourceLimit)
    );
    let mut p = parameter(0);
    p.linked_scene_mode = vec![address(0, 1)];
    assert_eq!(
        WorkingParameters::new(SceneIndex(0), vec![model(0, vec![p])]),
        Err(WorkflowError::ParameterAddress)
    );
    assert_eq!(
        WorkingParameters::new(
            SceneIndex(0),
            vec![model(0, vec![parameter(0), parameter(0)])]
        ),
        Err(WorkflowError::ParameterAddress)
    );
    assert_eq!(
        WorkingParameters::new(SceneIndex(0), vec![model(0, vec![]), model(0, vec![])]),
        Err(WorkflowError::ParameterAddress)
    );
    assert_eq!(
        WorkingParameters::new(
            SceneIndex(0),
            (0..4)
                .map(|chain| model(chain, (0..65).map(parameter).collect()))
                .collect()
        ),
        Err(WorkflowError::ResourceLimit)
    );

    let text = LiveParameterValue::String(UiText::from_units(vec![65; MAX_TEXT_UNITS]).unwrap());
    let mut p = parameter(0);
    p.primary = std::array::from_fn(|_| text.clone());
    p.second = p.primary.clone();
    let full = model(0, vec![p]);
    let mut q = parameter(1);
    q.primary[0] = LiveParameterValue::String(UiText::from_literal("x").unwrap());
    let mut oversized = full.clone();
    oversized.parameters.push(q);
    assert_eq!(
        WorkingParameters::new(SceneIndex(0), vec![oversized]),
        Err(WorkflowError::ResourceLimit)
    );
    let mut installed = full.clone();
    installed.parameters.push(parameter(1));
    let mut core = core(vec![installed]);
    let before = core.clone();
    assert_eq!(
        core.dispatch(
            Action::Parameter(edit(address(0, 1), text, true)),
            Millis(1)
        ),
        Err(WorkflowError::ResourceLimit)
    );
    assert_eq!(core, before);
    assert_eq!(
        core.dispatch(
            Action::Parameter(ParameterAction::InstallWorkingParameters(
                WorkingParameters::new(SceneIndex(0), vec![full]).unwrap()
            )),
            Millis(1)
        ),
        Err(WorkflowError::ParametersAlreadyInstalled)
    );
    assert_eq!(core, before);
    let mut empty = base_core();
    let before = empty.clone();
    assert_eq!(
        empty.dispatch(Action::Parameter(edit(target, float(0), true)), Millis(1)),
        Err(WorkflowError::ParametersNotInstalled)
    );
    assert_eq!(empty, before);
    assert!(
        act(
            &mut core,
            ParameterAction::SetDefaultScene(SceneIndex::new(7).unwrap()),
            2
        )
        .is_empty()
    );
    assert_eq!(
        core.state()
            .working_parameters
            .as_ref()
            .unwrap()
            .default_scene()
            .get(),
        7
    );
    assert_eq!(core.state().scene.get(), 0);
    assert!(core.state().dirty_deadline.is_none());
}

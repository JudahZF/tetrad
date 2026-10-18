use super::*;

// Invented arena handles and facts test owned behavior, never stock parity.
const TARGET: StorageTarget = StorageTarget {
    folder: 9,
    index: 2,
};
const OTHER: StorageTarget = StorageTarget {
    folder: 10,
    index: 4,
};
const RESOLVED: CallbackPolicy = CallbackPolicy::CallerResolvedNonReentrant(Provenance(77));

fn snapshot(handle: u64, default: i32) -> FullSnapshot {
    FullSnapshot {
        handle: SnapshotHandle(handle),
        provenance: Provenance(77),
        default_scene: SceneIndex::new(default).unwrap(),
        storage: Some(StorageIdentity {
            identity: 8192,
            key: handle,
        }),
    }
}

fn scenes(label: &str) -> SceneStorage {
    SceneStorage::new(vec![SceneEntry {
        label: UiText::from_literal(label).unwrap(),
        raw_color: 1,
    }])
    .unwrap()
}

fn address() -> ParameterAddress {
    ParameterAddress {
        chain: ChainIndex::new(0).unwrap(),
        column: BankSlot::new(0).unwrap(),
        parameter: ParameterIndex::new(0).unwrap(),
    }
}

fn parameters(default: i32) -> WorkingParameters {
    WorkingParameters::new(
        SceneIndex::new(default).unwrap(),
        vec![WorkingParameterModel {
            chain: ChainIndex::new(0).unwrap(),
            column: BankSlot::new(0).unwrap(),
            facts: ParameterModelFacts {
                model_id: 1000,
                category_id: 1,
                column_identity: 0,
                runtime_kind: Some(0),
                coupling: CouplingFacts::ResolvedAbsent,
                force: false,
            },
            parameters: vec![WorkingParameter {
                position: ParameterIndex::new(0).unwrap(),
                primary: std::array::from_fn(|_| LiveParameterValue::Int(1)),
                second: std::array::from_fn(|_| LiveParameterValue::Int(2)),
                scene_mode: true,
                metadata: ParameterMetadata {
                    route: ParameterRoute::Ordinary,
                    binding_valid: true,
                    visual_target_exists: true,
                    model_block_needs_repaint: true,
                    control: ParameterControlFacts {
                        session_supported: true,
                        kind: ParameterControlKind::Int,
                    },
                },
                linked_scene_mode: vec![],
                dependencies: vec![],
            }],
        }],
    )
    .unwrap()
}

fn core(alternate: bool) -> ApplicationCore {
    let mut core = ApplicationCore::new(WorkflowInit {
        working: scenes("working"),
        alternate: alternate.then(|| scenes("alternate")),
        baseline: BaselineId(40),
        mode: ModeId::new(0).unwrap(),
        previous_mode: ModeId::new(0).unwrap(),
        page: 0,
        ui_dirty: true,
        last_notified_dirty: true,
        position: BankPosition {
            bank: CallerValidatedBank(0),
            slot: BankSlot::new(0).unwrap(),
        },
        title: UiText::default(),
        requested: None,
        loaded: None,
    });
    if !alternate {
        core.dispatch(
            Action::Parameter(ParameterAction::InstallWorkingParameters(parameters(2))),
            Millis(0),
        )
        .unwrap();
    }
    core.dispatch_lifecycle(
        LifecycleAction::Attach {
            working: snapshot(1, 2),
            alternate: alternate.then(|| snapshot(2, 6)),
            baseline: snapshot(3, 1),
            current_target: TARGET,
        },
        Millis(0),
    )
    .unwrap();
    core
}

fn load(core: &ApplicationCore) -> LoadInputs {
    let normalized = WorkingProjection {
        scenes: scenes("new working"),
        parameters: Some(parameters(3)),
    };
    let mut reset = normalized.clone();
    reset
        .parameters
        .as_mut()
        .unwrap()
        .lifecycle_reset_ordinary_banks();
    let alternate = core.lifecycle().unwrap().alternate;
    LoadInputs {
        selected_target: TARGET,
        baseline_input_r1: snapshot(10, 7),
        working_input_r2: snapshot(11, 3),
        facts: Some(LoadFacts {
            normalized_from: snapshot(11, 3),
            normalized_working: snapshot(12, 3),
            normalized_projection: normalized,
            reset_from: alternate.map_or(snapshot(12, 3), |bound| bound.snapshot),
            reset_effective: alternate.map_or(
                FullSnapshot {
                    handle: SnapshotHandle(13),
                    ..snapshot(12, 3)
                },
                |bound| FullSnapshot {
                    handle: SnapshotHandle(14),
                    ..bound.snapshot
                },
            ),
            reset_working_projection: alternate.is_none().then_some(reset),
            normalization_evidence: Provenance(77),
            reset_evidence: Provenance(78),
            callbacks: RESOLVED,
        }),
    }
}

fn prepare(core: &ApplicationCore, handle: u64) -> PreparedCandidate {
    PreparedCandidate {
        copied_from: core.lifecycle().unwrap().effective(),
        selected: core.state().scene,
        candidate: FullSnapshot {
            default_scene: core.state().scene,
            ..snapshot(handle, 0)
        },
        preparation_evidence: Provenance(79),
        callbacks: RESOLVED,
    }
}

fn begin(core: &mut ApplicationCore, target: StorageTarget, now: u64) -> SaveToken {
    let prepared = prepare(core, 20);
    core.dispatch_lifecycle(
        LifecycleAction::BeginSave {
            target,
            prepared: Some(prepared),
        },
        Millis(now),
    )
    .unwrap();
    core.lifecycle().unwrap().in_flight.unwrap().token
}

fn success(core: &ApplicationCore) -> StorageReport {
    let lifecycle = core.lifecycle().unwrap();
    let storage = Some(StorageIdentity {
        identity: 9000,
        key: 100,
    });
    let effective = FullSnapshot {
        handle: SnapshotHandle(31),
        default_scene: core.state().scene,
        storage: if lifecycle.alternate.is_some() {
            lifecycle.effective().snapshot.storage
        } else {
            storage
        },
        ..lifecycle.effective().snapshot
    };
    StorageReport::ReportedSuccess(ReportedSavedSnapshot {
        target: lifecycle.in_flight.unwrap().target,
        saved: FullSnapshot {
            handle: SnapshotHandle(30),
            default_scene: core.state().scene,
            storage,
            ..snapshot(30, 0)
        },
        effective_after_default: effective,
        working_after_identity: if lifecycle.alternate.is_some() {
            FullSnapshot {
                handle: SnapshotHandle(32),
                storage,
                ..lifecycle.working.snapshot
            }
        } else {
            effective
        },
        callbacks: RESOLVED,
    })
}

#[test]
fn distinct_r1_baseline_r2_install_and_bank_reset_use_existing_stores() {
    let mut core = core(false);
    let old_comparison = SemanticComparison {
        effective: core.state().effective_stamp(),
        baseline: core.state().baseline,
        differs: false,
    };
    let inputs = load(&core);
    let effects = core
        .dispatch_lifecycle(LifecycleAction::Load(Box::new(inputs)), Millis(10))
        .unwrap();
    assert_eq!(
        core.lifecycle().unwrap().baseline_snapshot.handle,
        SnapshotHandle(10)
    );
    assert_eq!(
        core.lifecycle().unwrap().working.snapshot.handle,
        SnapshotHandle(13)
    );
    assert_eq!(core.state().baseline, BaselineId(41));
    assert_eq!(core.state().scene.get(), 3);
    assert_eq!(core.state().working, scenes("new working"));
    let parameter = core
        .state()
        .working_parameters
        .as_ref()
        .unwrap()
        .parameter(address())
        .unwrap();
    assert_eq!(parameter.primary[0], LiveParameterValue::Int(2));
    assert_eq!(parameter.second[0], LiveParameterValue::Int(2));
    let baseline_pos = effects
        .iter()
        .position(|effect| matches!(effect, LifecycleEffect::BaselineCopied { .. }))
        .unwrap();
    let selected_pos = effects
        .iter()
        .position(|effect| {
            matches!(
                effect,
                LifecycleEffect::Core(Effect::Observer(ObserverEvent::SceneSelected { .. }))
            )
        })
        .unwrap();
    let install_pos = effects
        .iter()
        .position(|effect| matches!(effect, LifecycleEffect::ForcedWorkingInstalled { .. }))
        .unwrap();
    assert!(baseline_pos < selected_pos && selected_pos < install_pos);
    assert!(core.state().ui_dirty && core.state().last_notified_dirty);
    assert_eq!(core.state().dirty_deadline, Some(Millis(110)));
    assert_eq!(
        core.dispatch(Action::ForceDirtyEvaluation(old_comparison), Millis(10)),
        Err(WorkflowError::StaleComparison)
    );
}

#[test]
fn load_preserves_alternate_context_and_resets_effective_without_replacing_baseline() {
    let mut core = core(true);
    let alternate_scenes = core.state().alternate.clone();
    let alternate_stamp = core.state().effective_stamp();
    let inputs = load(&core);
    core.dispatch_lifecycle(LifecycleAction::Load(Box::new(inputs)), Millis(1))
        .unwrap();
    assert_eq!(core.state().alternate, alternate_scenes);
    assert_eq!(
        core.state().effective_stamp().context,
        PresetContext::Alternate
    );
    assert!(core.state().effective_stamp().revision > alternate_stamp.revision);
    assert_eq!(
        core.lifecycle().unwrap().working.snapshot.handle,
        SnapshotHandle(12)
    );
    assert_eq!(
        core.lifecycle().unwrap().effective().snapshot.handle,
        SnapshotHandle(14)
    );
    assert_eq!(
        core.lifecycle().unwrap().baseline_snapshot.handle,
        SnapshotHandle(10)
    );
    assert_eq!(core.state().scene.get(), 3);
    assert_eq!(
        core.lifecycle()
            .unwrap()
            .effective()
            .snapshot
            .default_scene
            .get(),
        6
    );
    let parameter = core
        .state()
        .working_parameters
        .as_ref()
        .unwrap()
        .parameter(address())
        .unwrap();
    assert_eq!(parameter.primary[0], LiveParameterValue::Int(1));
    assert_eq!(parameter.second[0], LiveParameterValue::Int(2));
}

#[test]
fn failure_has_no_success_writes_or_dirty_clear_and_accepts_intervening_edit() {
    let mut core = core(true);
    let token = begin(&mut core, TARGET, 1);
    core.dispatch(
        Action::SetSceneLabel {
            index: 0,
            label: UiText::from_literal("edited").unwrap(),
        },
        Millis(2),
    )
    .unwrap();
    let before = core.clone();
    let effects = core
        .dispatch_lifecycle(
            LifecycleAction::StorageResult {
                token,
                report: StorageReport::ReportedFailure {
                    code: std::num::NonZeroU32::new(7).unwrap(),
                },
            },
            Millis(3),
        )
        .unwrap();
    assert!(matches!(
        effects.as_slice(),
        [LifecycleEffect::StorageFailureObserved { .. }]
    ));
    assert_eq!(core.state().working, before.state().working);
    assert_eq!(core.state().alternate, before.state().alternate);
    assert_eq!(core.state().baseline, before.state().baseline);
    assert_eq!(
        core.lifecycle().unwrap().working,
        before.lifecycle().unwrap().working
    );
    assert_eq!(
        core.lifecycle().unwrap().baseline_snapshot,
        before.lifecycle().unwrap().baseline_snapshot
    );
    assert_eq!(core.state().dirty_deadline, before.state().dirty_deadline);
    assert!(core.state().ui_dirty && core.lifecycle().unwrap().in_flight.is_none());
}

#[test]
fn current_target_uses_actual_saved_snapshot_updates_working_identity_and_effective_default() {
    let mut core = core(true);
    core.dispatch(
        Action::SelectScene {
            scene: SceneIndex::new(5).unwrap(),
            force: false,
        },
        Millis(1),
    )
    .unwrap();
    let token = begin(&mut core, TARGET, 2);
    let report = success(&core);
    let effects = core
        .dispatch_lifecycle(LifecycleAction::StorageResult { token, report }, Millis(3))
        .unwrap();
    let lifecycle = core.lifecycle().unwrap();
    assert_eq!(lifecycle.baseline_snapshot.handle, SnapshotHandle(30)); // actual saved != candidate20
    assert_eq!(lifecycle.working.snapshot.storage.unwrap().identity, 9000);
    assert_eq!(lifecycle.working.snapshot.default_scene.get(), 2);
    assert_eq!(lifecycle.effective().snapshot.default_scene.get(), 5);
    assert_eq!(
        lifecycle.effective().snapshot.storage.unwrap().identity,
        8192
    );
    assert!(effects.iter().any(|effect| matches!(effect, LifecycleEffect::BaselineCopied { snapshot, .. } if snapshot.handle == SnapshotHandle(30))));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, LifecycleEffect::OrdinaryLoaderRequired { .. }))
    );
    assert!(core.state().ui_dirty && core.state().last_notified_dirty);
    assert_eq!(core.state().dirty_deadline, Some(Millis(103)));
    let comparison = SemanticComparison {
        effective: core.state().effective_stamp(),
        baseline: core.state().baseline,
        differs: true,
    };
    core.dispatch(Action::DirtyTimeout(comparison), Millis(103))
        .unwrap();
    assert!(core.state().ui_dirty); // successful save does not imply semantic equality
}

#[test]
fn save_as_emits_loader_intent_without_claiming_loader_or_baseline_success() {
    let mut core = core(false);
    let baseline = core.state().baseline;
    let token = begin(&mut core, OTHER, 1);
    let report = success(&core);
    let effects = core
        .dispatch_lifecycle(LifecycleAction::StorageResult { token, report }, Millis(2))
        .unwrap();
    assert!(effects.iter().any(|effect| matches!(effect, LifecycleEffect::OrdinaryLoaderRequired { target: OTHER, saved } if saved.handle == SnapshotHandle(30))));
    assert_eq!(core.state().baseline, baseline);
    assert_eq!(core.lifecycle().unwrap().current_target, TARGET);
    assert_eq!(core.state().dirty_deadline, None);
    assert!(core.state().ui_dirty);
    assert_eq!(
        core.state()
            .working_parameters
            .as_ref()
            .unwrap()
            .default_scene(),
        core.state().scene
    );
    let parameter = core
        .state()
        .working_parameters
        .as_ref()
        .unwrap()
        .parameter(address())
        .unwrap();
    assert_eq!(parameter.primary[0], LiveParameterValue::Int(1));
    assert_eq!(parameter.second[0], LiveParameterValue::Int(2)); // save never clears banks
}

#[test]
fn edit_during_save_refuses_success_atomically_and_requires_explicit_abandonment() {
    let mut core = core(false);
    let token = begin(&mut core, TARGET, 1);
    let report = success(&core);
    core.dispatch(
        Action::Parameter(ParameterAction::EditorParameterEdit {
            target: address(),
            requested: LiveParameterValue::Int(9),
            float_dispatch: FloatDispatchPredicate::Resolved(true),
        }),
        Millis(2),
    )
    .unwrap();
    let before = core.clone();
    assert_eq!(
        core.dispatch_lifecycle(LifecycleAction::StorageResult { token, report }, Millis(3)),
        Err(LifecycleError::Stale)
    );
    assert_eq!(core, before);
    assert!(core.lifecycle().unwrap().in_flight.is_some());
    core.dispatch_lifecycle(LifecycleAction::AbandonSave(token), Millis(3))
        .unwrap();
    assert_eq!(
        core.state()
            .working_parameters
            .as_ref()
            .unwrap()
            .parameter(address())
            .unwrap()
            .primary[0],
        LiveParameterValue::Int(9)
    );
    assert_eq!(
        core.lifecycle().unwrap().baseline_snapshot.handle,
        SnapshotHandle(3)
    );
    assert!(core.state().ui_dirty);
}

#[test]
fn reentrant_requests_unresolved_callbacks_and_snapshot_staleness_are_refusals() {
    let mut core = core(false);
    let prepared = prepare(&core, 20);
    let token = begin(&mut core, TARGET, 1);
    let before = core.clone();
    assert_eq!(
        core.dispatch_lifecycle(
            LifecycleAction::BeginSave {
                target: TARGET,
                prepared: Some(prepared)
            },
            Millis(2)
        ),
        Err(LifecycleError::ReentrantSave)
    );
    let inputs = load(&core);
    assert_eq!(
        core.dispatch_lifecycle(LifecycleAction::Load(Box::new(inputs)), Millis(2)),
        Err(LifecycleError::LoadDuringSave)
    );
    let StorageReport::ReportedSuccess(mut report) = success(&core) else {
        unreachable!()
    };
    report.callbacks = CallbackPolicy::Unresolved;
    assert_eq!(
        core.dispatch_lifecycle(
            LifecycleAction::StorageResult {
                token,
                report: StorageReport::ReportedSuccess(report)
            },
            Millis(2)
        ),
        Err(LifecycleError::Unresolved(RequiredFact::CallbackReentrancy))
    );
    assert_eq!(core, before);
    core.dispatch_lifecycle(LifecycleAction::AbandonSave(token), Millis(2))
        .unwrap();
    core.dispatch(Action::InvalidateEffectiveComparison, Millis(3))
        .unwrap();
    let before = core.clone();
    assert_eq!(
        core.dispatch_lifecycle(
            LifecycleAction::BeginSave {
                target: TARGET,
                prepared: Some(prepared)
            },
            Millis(4)
        ),
        Err(LifecycleError::Unresolved(
            RequiredFact::FullSnapshotCapture
        ))
    );
    assert_eq!(core, before);
    let current = BoundSnapshot {
        stamp: core.state().effective_stamp(),
        snapshot: snapshot(90, 2),
    };
    core.dispatch_lifecycle(LifecycleAction::BindSnapshot(current), Millis(4))
        .unwrap();
    begin(&mut core, TARGET, 5);
}

#[test]
fn replacing_bound_handle_or_selected_scene_during_save_makes_result_stale() {
    for replace_handle in [true, false] {
        let mut core = core(false);
        let token = begin(&mut core, TARGET, 1);
        let report = success(&core);
        if replace_handle {
            core.dispatch_lifecycle(
                LifecycleAction::BindSnapshot(BoundSnapshot {
                    stamp: core.state().effective_stamp(),
                    snapshot: snapshot(91, 2),
                }),
                Millis(2),
            )
            .unwrap();
        } else {
            core.dispatch(
                Action::SelectScene {
                    scene: SceneIndex::new(7).unwrap(),
                    force: false,
                },
                Millis(2),
            )
            .unwrap();
        }
        let before = core.clone();
        assert_eq!(
            core.dispatch_lifecycle(LifecycleAction::StorageResult { token, report }, Millis(3)),
            Err(LifecycleError::Stale)
        );
        assert_eq!(core, before);
    }
}

#[test]
fn save300_generation_deadline_and_chain500_share_only_existing_dirty100() {
    let mut core = core(false);
    core.dispatch(
        Action::Parameter(ParameterAction::EditorParameterEdit {
            target: address(),
            requested: LiveParameterValue::Int(9),
            float_dispatch: FloatDispatchPredicate::Resolved(true),
        }),
        Millis(10),
    )
    .unwrap();
    let chain = core.state().parameter_edit_timers[0].unwrap();
    assert_eq!(chain.deadline, Millis(510));
    core.dispatch_lifecycle(
        LifecycleAction::ScheduleSave(SaveTimerPolicy::CallerObservedZero(Provenance(77))),
        Millis(10),
    )
    .unwrap();
    let old = core.lifecycle().unwrap().save_timer.unwrap();
    assert_eq!(old.deadline, Millis(310));
    core.dispatch_lifecycle(
        LifecycleAction::ScheduleSave(SaveTimerPolicy::CallerObservedZero(Provenance(77))),
        Millis(10),
    )
    .unwrap();
    let latest = core.lifecycle().unwrap().save_timer.unwrap();
    assert_ne!(old.generation, latest.generation);
    assert!(
        core.dispatch_lifecycle(LifecycleAction::SaveTimeout(old), Millis(310))
            .unwrap()
            .is_empty()
    );
    // Latest is still pending; no implicit snapshot or storage operation occurred.
    assert_eq!(core.lifecycle().unwrap().save_timer, Some(latest));
    let effects = core
        .dispatch_lifecycle(LifecycleAction::SaveTimeout(latest), Millis(310))
        .unwrap();
    assert!(matches!(
        effects.as_slice(),
        [LifecycleEffect::SaveRequested { .. }]
    ));
    assert_eq!(core.state().parameter_edit_timers[0], Some(chain));
    assert!(core.lifecycle().unwrap().in_flight.is_none());
    core.dispatch(
        Action::Parameter(ParameterAction::ParameterEditTimeout {
            chain: ChainIndex::new(0).unwrap(),
            timer: chain,
        }),
        Millis(510),
    )
    .unwrap();
    assert_eq!(core.state().dirty_deadline, Some(Millis(610)));
    assert!(core.state().ui_dirty);
}

#[test]
fn early_and_baseline_stale_save_timers_do_not_mutate_baseline_or_dirty() {
    let mut core = core(false);
    core.dispatch_lifecycle(
        LifecycleAction::ScheduleSave(SaveTimerPolicy::CallerObservedZero(Provenance(77))),
        Millis(1),
    )
    .unwrap();
    let timer = core.lifecycle().unwrap().save_timer.unwrap();
    assert!(
        core.dispatch_lifecycle(LifecycleAction::SaveTimeout(timer), Millis(300))
            .unwrap()
            .is_empty()
    );
    let inputs = load(&core);
    core.dispatch_lifecycle(LifecycleAction::Load(Box::new(inputs)), Millis(300))
        .unwrap();
    let before = core.clone();
    assert_eq!(
        core.dispatch_lifecycle(LifecycleAction::SaveTimeout(timer), Millis(301)),
        Err(LifecycleError::Stale)
    );
    assert_eq!(core, before);
    assert_eq!(core.state().dirty_deadline, Some(Millis(400)));
}

#[test]
fn unresolved_and_contradictory_loads_clock_and_overflow_roll_back_owned_state() {
    let mut core = core(false);
    for mode in 0..4 {
        let mut inputs = load(&core);
        match mode {
            0 => inputs.facts = None,
            1 => inputs.facts.as_mut().unwrap().callbacks = CallbackPolicy::Unresolved,
            2 => inputs.facts.as_mut().unwrap().reset_working_projection = None,
            _ => {
                inputs.facts.as_mut().unwrap().reset_working_projection =
                    Some(inputs.facts.as_ref().unwrap().normalized_projection.clone())
            }
        }
        let before = core.clone();
        assert!(
            core.dispatch_lifecycle(LifecycleAction::Load(Box::new(inputs)), Millis(5))
                .is_err()
        );
        assert_eq!(core, before);
    }
    core.dispatch(Action::ScheduleDirty, Millis(5)).unwrap();
    let before = core.clone();
    assert_eq!(
        core.dispatch_lifecycle(
            LifecycleAction::BeginSave {
                target: TARGET,
                prepared: None
            },
            Millis(4)
        ),
        Err(LifecycleError::Core(WorkflowError::ClockRegression))
    );
    assert_eq!(core, before);
    assert_eq!(
        core.dispatch_lifecycle(
            LifecycleAction::BeginSave {
                target: TARGET,
                prepared: None
            },
            Millis(5)
        ),
        Err(LifecycleError::Unresolved(RequiredFact::SavePreparation))
    );
    assert_eq!(core, before);
    core.lifecycle.as_mut().unwrap().timer_generation = u64::MAX;
    let before = core.clone();
    assert_eq!(
        core.dispatch_lifecycle(
            LifecycleAction::ScheduleSave(SaveTimerPolicy::CallerObservedZero(Provenance(77))),
            Millis(5)
        ),
        Err(LifecycleError::Core(WorkflowError::Overflow))
    );
    assert_eq!(core, before);
    core.state.baseline = BaselineId(u64::MAX);
    let before = core.clone();
    let inputs = load(&core);
    assert_eq!(
        core.dispatch_lifecycle(LifecycleAction::Load(Box::new(inputs)), Millis(5)),
        Err(LifecycleError::Core(WorkflowError::Overflow))
    );
    assert_eq!(core, before);
}

#[test]
fn storage_receipt_must_name_requested_target_and_consistent_success_metadata() {
    let mut core = core(false);
    let token = begin(&mut core, TARGET, 1);
    let StorageReport::ReportedSuccess(valid) = success(&core) else {
        unreachable!()
    };
    for mode in 0..3 {
        let mut reported = valid;
        match mode {
            0 => reported.target = OTHER,
            1 => reported.saved.storage = None,
            _ => reported.effective_after_default.default_scene = SceneIndex::new(7).unwrap(),
        }
        let before = core.clone();
        assert_eq!(
            core.dispatch_lifecycle(
                LifecycleAction::StorageResult {
                    token,
                    report: StorageReport::ReportedSuccess(reported)
                },
                Millis(2)
            ),
            Err(LifecycleError::InconsistentFacts)
        );
        assert_eq!(core, before);
    }
    let mut stale_token = token;
    stale_token.generation += 1;
    let before = core.clone();
    assert_eq!(
        core.dispatch_lifecycle(
            LifecycleAction::StorageResult {
                token: stale_token,
                report: StorageReport::ReportedSuccess(valid)
            },
            Millis(2)
        ),
        Err(LifecycleError::Stale)
    );
    assert_eq!(core, before);
    core.dispatch_lifecycle(
        LifecycleAction::StorageResult {
            token,
            report: StorageReport::ReportedSuccess(valid),
        },
        Millis(2),
    )
    .unwrap();
    assert_eq!(core.lifecycle().unwrap().baseline_snapshot, valid.saved);
    assert!(core.state().ui_dirty);
}

mod regressions;

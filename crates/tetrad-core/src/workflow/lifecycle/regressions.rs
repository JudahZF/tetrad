//! Durable regressions for L01-01/L01-02. All full captures are synthetic facts.
use super::*;

fn refuse_atomically(app: &mut ApplicationCore, action: LifecycleAction, now: u64) {
    let before = app.clone();
    assert_eq!(
        app.dispatch_lifecycle(action, Millis(now)),
        Err(LifecycleError::InconsistentFacts)
    );
    assert_eq!(*app, before); // Includes clock, revisions, baseline, timers and flight.
}

#[test]
fn attach_rejects_two_metadata_values_for_one_immutable_arena_handle() {
    // Independent replay of the review's Attach counterexample.
    let mut app = core(true);
    app.lifecycle = None;
    let working = snapshot(1, 2);
    let baseline = FullSnapshot {
        default_scene: SceneIndex::new(7).unwrap(),
        ..working
    };
    refuse_atomically(
        &mut app,
        LifecycleAction::Attach {
            working,
            alternate: Some(snapshot(2, 6)),
            baseline,
            current_target: TARGET,
        },
        1,
    );
}

#[test]
fn bind_rejects_metadata_conflicting_with_retained_baseline_handle() {
    // Independent replay: default stays 6 in alternate, so this tests aliases,
    // independently of the still-current defaultScene rule.
    let mut app = core(true);
    let baseline = app.lifecycle().unwrap().baseline_snapshot;
    let snapshot = FullSnapshot {
        default_scene: SceneIndex::new(6).unwrap(),
        ..baseline
    };
    let stamp = app.state().effective_stamp();
    refuse_atomically(
        &mut app,
        LifecycleAction::BindSnapshot(BoundSnapshot { stamp, snapshot }),
        1,
    );
}

#[test]
fn load_rejects_normalized_working_conflicting_with_r1_baseline_handle() {
    // Independent replay of the review's Load counterexample.
    let mut app = core(true);
    let mut inputs = load(&app);
    inputs.facts.as_mut().unwrap().normalized_working.handle = inputs.baseline_input_r1.handle;
    refuse_atomically(&mut app, LifecycleAction::Load(Box::new(inputs)), 1);
}

#[test]
fn begin_save_rejects_candidate_redefining_its_immutable_source_handle() {
    // Independent replay of the review's BeginSave counterexample.
    let mut app = core(false);
    let source = app.lifecycle().unwrap().effective();
    let prepared = PreparedCandidate {
        copied_from: source,
        selected: app.state().scene,
        candidate: FullSnapshot {
            default_scene: app.state().scene,
            ..source.snapshot
        },
        preparation_evidence: Provenance(79),
        callbacks: RESOLVED,
    };
    refuse_atomically(
        &mut app,
        LifecycleAction::BeginSave {
            target: TARGET,
            prepared: Some(prepared),
        },
        1,
    );
}

#[test]
fn storage_result_rejects_saved_and_working_with_conflicting_handle_metadata() {
    // Independent replay of the review's StorageResult counterexample.
    let mut app = core(true);
    let token = begin(&mut app, TARGET, 1);
    let StorageReport::ReportedSuccess(mut report) = success(&app) else {
        unreachable!()
    };
    report.working_after_identity.handle = report.saved.handle;
    report.working_after_identity.provenance = report.saved.provenance;
    refuse_atomically(
        &mut app,
        LifecycleAction::StorageResult {
            token,
            report: StorageReport::ReportedSuccess(report),
        },
        2,
    );
    assert!(app.lifecycle().unwrap().in_flight.is_some());
}

#[test]
fn same_revision_alternate_default_change_refuses_and_preserves_old_comparison_facts() {
    // Independent replay of L01-02. Initial equality is a complete caller fact.
    let mut app = core(true);
    let old = app.lifecycle().unwrap().effective();
    let working = app.lifecycle().unwrap().working.snapshot;
    app.lifecycle = None;
    app.dispatch_lifecycle(
        LifecycleAction::Attach {
            working,
            alternate: Some(old.snapshot),
            baseline: old.snapshot,
            current_target: TARGET,
        },
        Millis(0),
    )
    .unwrap();
    let comparison = SemanticComparison {
        effective: old.stamp,
        baseline: app.state().baseline,
        differs: false,
    };
    let new = BoundSnapshot {
        stamp: old.stamp,
        snapshot: FullSnapshot {
            handle: SnapshotHandle(100),
            default_scene: SceneIndex::new(7).unwrap(),
            ..old.snapshot
        },
    };
    refuse_atomically(&mut app, LifecycleAction::BindSnapshot(new), 1);
    assert_eq!(app.lifecycle().unwrap().effective(), old);
    // Refusal leaves the old complete comparison valid; no change was accepted.
    app.dispatch(Action::ForceDirtyEvaluation(comparison), Millis(2))
        .unwrap();
    assert!(!app.state().ui_dirty);
}

#[test]
fn handle_namespace_is_single_arena_and_all_metadata_must_agree() {
    for changed_field in 0..4 {
        let mut app = core(true);
        app.lifecycle = None;
        let working = snapshot(1, 2);
        let mut alternate = working;
        match changed_field {
            0 => alternate.provenance = Provenance(999),
            1 => alternate.default_scene = SceneIndex::new(7).unwrap(),
            2 => alternate.storage = None,
            _ => alternate.storage.as_mut().unwrap().key += 1,
        }
        refuse_atomically(
            &mut app,
            LifecycleAction::Attach {
                working,
                alternate: Some(alternate),
                baseline: snapshot(3, 1),
                current_target: TARGET,
            },
            1,
        );
    }
    let mut app = core(true);
    app.lifecycle = None;
    let same = snapshot(1, 2);
    app.dispatch_lifecycle(
        LifecycleAction::Attach {
            working: same,
            alternate: Some(same),
            baseline: same,
            current_target: TARGET,
        },
        Millis(1),
    )
    .unwrap(); // Identical aliases are valid, without asserting semantic equality.
}

#[test]
fn load_checks_r1_r2_transformation_inputs_outputs_and_retained_contexts() {
    for route in 0..7 {
        let mut app = core(true);
        let mut inputs = load(&app);
        let facts = inputs.facts.as_mut().unwrap();
        match route {
            0 => inputs.baseline_input_r1.handle = inputs.working_input_r2.handle,
            1 => {
                inputs.working_input_r2.handle = app.lifecycle().unwrap().working.snapshot.handle;
                facts.normalized_from = inputs.working_input_r2;
            }
            2 => facts.normalized_from.provenance = Provenance(999),
            3 => facts.normalized_working.handle = inputs.baseline_input_r1.handle,
            4 => facts.reset_from.provenance = Provenance(999),
            5 => facts.reset_effective.handle = app.lifecycle().unwrap().working.snapshot.handle,
            _ => {
                inputs.baseline_input_r1.handle = app.lifecycle().unwrap().baseline_snapshot.handle;
            }
        }
        refuse_atomically(&mut app, LifecycleAction::Load(Box::new(inputs)), 1);
    }
    // Working-effective reset output must also agree with its normalized source
    // if the caller reuses the same object. Its projection remains a valid reset.
    let mut app = core(false);
    let mut inputs = load(&app);
    let facts = inputs.facts.as_mut().unwrap();
    facts.reset_effective.handle = facts.normalized_working.handle;
    facts.reset_effective.provenance = Provenance(999);
    refuse_atomically(&mut app, LifecycleAction::Load(Box::new(inputs)), 1);
}

#[test]
fn bind_checks_old_binding_other_context_and_inflight_capture_and_candidate() {
    for route in 0..4 {
        let mut app = core(true);
        app.dispatch(
            Action::SelectScene {
                scene: SceneIndex::new(6).unwrap(),
                force: false,
            },
            Millis(1),
        )
        .unwrap();
        begin(&mut app, TARGET, 2);
        let state = app.lifecycle().unwrap();
        let other = match route {
            0 => state.effective().snapshot,
            1 => state.working.snapshot,
            2 => state.in_flight.unwrap().candidate,
            _ => state.in_flight.unwrap().token.captured.effective_snapshot,
        };
        if route == 3 {
            // Old handle2 survives only in the flight token after this recapture.
            let mut current = state.effective();
            current.snapshot.handle = SnapshotHandle(99);
            app.dispatch_lifecycle(LifecycleAction::BindSnapshot(current), Millis(3))
                .unwrap();
        }
        let bound = BoundSnapshot {
            stamp: app.state().effective_stamp(),
            snapshot: FullSnapshot {
                default_scene: SceneIndex::new(6).unwrap(),
                provenance: Provenance(999),
                ..other
            },
        };
        refuse_atomically(&mut app, LifecycleAction::BindSnapshot(bound), 4);
    }
}

#[test]
fn begin_save_checks_prepared_source_and_candidate_against_retained_metadata() {
    for source in [true, false] {
        let mut app = core(false);
        let mut prepared = prepare(&app, 20);
        if source {
            prepared.copied_from.snapshot.provenance = Provenance(999);
        } else {
            prepared.candidate.handle = app.lifecycle().unwrap().baseline_snapshot.handle;
        }
        refuse_atomically(
            &mut app,
            LifecycleAction::BeginSave {
                target: TARGET,
                prepared: Some(prepared),
            },
            1,
        );
    }
}

#[test]
fn storage_checks_saved_working_effective_against_retained_baseline_and_candidate() {
    for route in 0..6 {
        let mut app = core(true);
        let token = begin(&mut app, TARGET, 1);
        let state = app.lifecycle().unwrap();
        let StorageReport::ReportedSuccess(mut reported) = success(&app) else {
            unreachable!()
        };
        match route {
            0 => reported.saved.handle = state.in_flight.unwrap().candidate.handle,
            1 => reported.saved.handle = state.baseline_snapshot.handle,
            2 => reported.working_after_identity.handle = state.working.snapshot.handle,
            3 => reported.effective_after_default.handle = state.effective().snapshot.handle,
            4 => reported.effective_after_default.handle = reported.saved.handle,
            _ => reported.working_after_identity.handle = reported.effective_after_default.handle,
        }
        refuse_atomically(
            &mut app,
            LifecycleAction::StorageResult {
                token,
                report: StorageReport::ReportedSuccess(reported),
            },
            2,
        );
    }
}

#[test]
fn identical_metadata_reuse_is_accepted_through_bind_load_prepare_and_storage() {
    let mut app = core(false);
    let bound = app.lifecycle().unwrap().working;
    app.dispatch_lifecycle(LifecycleAction::BindSnapshot(bound), Millis(1))
        .unwrap();
    let mut inputs = load(&app);
    let facts = inputs.facts.as_mut().unwrap();
    // Synthetic caller facts describe no-op normalization/reset, so the same
    // immutable object can be reused. Metadata never proves hidden equality.
    facts
        .normalized_projection
        .parameters
        .as_mut()
        .unwrap()
        .lifecycle_reset_ordinary_banks();
    facts.reset_working_projection = Some(facts.normalized_projection.clone());
    inputs.baseline_input_r1 = inputs.working_input_r2;
    facts.normalized_working = inputs.working_input_r2;
    facts.reset_from = facts.normalized_working;
    facts.reset_effective = facts.normalized_working;
    app.dispatch_lifecycle(LifecycleAction::Load(Box::new(inputs)), Millis(2))
        .unwrap();
    let source = app.lifecycle().unwrap().effective();
    let mut prepared = prepare(&app, 20);
    prepared.candidate = source.snapshot;
    app.dispatch_lifecycle(
        LifecycleAction::BeginSave {
            target: TARGET,
            prepared: Some(prepared),
        },
        Millis(3),
    )
    .unwrap();
    let token = app.lifecycle().unwrap().in_flight.unwrap().token;
    let StorageReport::ReportedSuccess(mut reported) = success(&app) else {
        unreachable!()
    };
    reported.working_after_identity = reported.saved;
    reported.effective_after_default = reported.saved;
    app.dispatch_lifecycle(
        LifecycleAction::StorageResult {
            token,
            report: StorageReport::ReportedSuccess(reported),
        },
        Millis(4),
    )
    .unwrap();
    assert_eq!(app.lifecycle().unwrap().baseline_snapshot, reported.saved);
}

#[test]
fn working_without_parameter_projection_refuses_same_revision_default_change() {
    for alternate in [true, false] {
        let mut app = core(alternate);
        // Synthetic construction of the admitted no-parameter projection case.
        app.state.working_parameters = None;
        let old = app.lifecycle().unwrap().working;
        let changed = BoundSnapshot {
            snapshot: FullSnapshot {
                handle: SnapshotHandle(100),
                default_scene: SceneIndex::new(7).unwrap(),
                ..old.snapshot
            },
            ..old
        };
        refuse_atomically(&mut app, LifecycleAction::BindSnapshot(changed), 1);
    }
}

#[test]
fn harmless_new_handle_recapture_preserves_revision_and_comparison_stamp() {
    for alternate in [true, false] {
        let mut app = core(alternate);
        let old = app.lifecycle().unwrap().effective();
        let comparison = SemanticComparison {
            effective: old.stamp,
            baseline: app.state().baseline,
            differs: true,
        };
        let current = BoundSnapshot {
            snapshot: FullSnapshot {
                handle: SnapshotHandle(100),
                ..old.snapshot
            },
            ..old
        };
        app.dispatch_lifecycle(LifecycleAction::BindSnapshot(current), Millis(1))
            .unwrap();
        assert_eq!(app.state().effective_stamp(), old.stamp);
        app.dispatch(Action::ForceDirtyEvaluation(comparison), Millis(2))
            .unwrap();
        assert!(app.state().ui_dirty); // Caller comparison, never handle equality.
    }
}

#[test]
fn ordinary_revision_advance_and_explicit_invalidation_allow_changed_default_recapture() {
    for alternate in [true, false] {
        let mut app = core(alternate);
        let old = app.lifecycle().unwrap().effective();
        let comparison = SemanticComparison {
            effective: old.stamp,
            baseline: app.state().baseline,
            differs: false,
        };
        let action = if alternate {
            Action::InvalidateEffectiveComparison
        } else {
            Action::Parameter(ParameterAction::SetDefaultScene(
                SceneIndex::new(7).unwrap(),
            ))
        };
        app.dispatch(action, Millis(1)).unwrap();
        let current = BoundSnapshot {
            stamp: app.state().effective_stamp(),
            snapshot: FullSnapshot {
                handle: SnapshotHandle(100),
                default_scene: SceneIndex::new(7).unwrap(),
                ..old.snapshot
            },
        };
        app.dispatch_lifecycle(LifecycleAction::BindSnapshot(current), Millis(2))
            .unwrap();
        assert!(current.stamp.revision > old.stamp.revision);
        let before = app.clone();
        assert_eq!(
            app.dispatch(Action::ForceDirtyEvaluation(comparison), Millis(3)),
            Err(WorkflowError::StaleComparison)
        );
        assert_eq!(app, before);
        assert!(app.save_stamp().is_ok());
    }
}

#[test]
fn invalidation_does_not_bypass_working_parameter_default_projection_check() {
    let mut app = core(false);
    let old = app.lifecycle().unwrap().working;
    app.dispatch(Action::InvalidateEffectiveComparison, Millis(1))
        .unwrap();
    let current = BoundSnapshot {
        stamp: app.state().effective_stamp(),
        snapshot: FullSnapshot {
            handle: SnapshotHandle(100),
            default_scene: SceneIndex::new(7).unwrap(),
            ..old.snapshot
        },
    };
    refuse_atomically(&mut app, LifecycleAction::BindSnapshot(current), 2);
    app.state.working_parameters = None; // Exercise the no-projection boundary.
    app.dispatch_lifecycle(LifecycleAction::BindSnapshot(current), Millis(2))
        .unwrap();
    assert_eq!(app.lifecycle().unwrap().working, current);
}

#[test]
fn working_effective_success_fields_describe_the_same_final_capture() {
    let mut app = core(false);
    let token = begin(&mut app, TARGET, 1);
    let StorageReport::ReportedSuccess(reported) = success(&app) else {
        unreachable!()
    };
    assert_eq!(
        reported.working_after_identity,
        reported.effective_after_default
    );
    assert_ne!(
        reported.working_after_identity.default_scene,
        app.lifecycle().unwrap().working.snapshot.default_scene
    );
    app.dispatch_lifecycle(
        LifecycleAction::StorageResult {
            token,
            report: StorageReport::ReportedSuccess(reported),
        },
        Millis(2),
    )
    .unwrap();
    assert_eq!(
        app.lifecycle().unwrap().working.snapshot,
        reported.effective_after_default
    );
    assert_eq!(
        app.state()
            .working_parameters
            .as_ref()
            .unwrap()
            .default_scene(),
        app.state().scene
    );
}

#[test]
fn selection_away_and_back_is_checked_by_current_value_not_history() {
    let mut app = core(false);
    let token = begin(&mut app, TARGET, 1);
    let reported = success(&app);
    let selected = app.state().scene;
    app.dispatch(
        Action::SelectScene {
            scene: SceneIndex::new(7).unwrap(),
            force: false,
        },
        Millis(2),
    )
    .unwrap();
    app.dispatch(
        Action::SelectScene {
            scene: selected,
            force: false,
        },
        Millis(3),
    )
    .unwrap();
    app.dispatch_lifecycle(
        LifecycleAction::StorageResult {
            token,
            report: reported,
        },
        Millis(4),
    )
    .unwrap();
    assert!(app.lifecycle().unwrap().in_flight.is_none());
}

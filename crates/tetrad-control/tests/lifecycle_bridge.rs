//! The ordinary wire bridge shares lifecycle state in ApplicationCore.
use tetrad_control::{
    Envelope,
    application::dispatch_application_scene,
    scene::{SCENE_MESSAGE_TYPE, SceneLimits},
};
use tetrad_core::workflow::*;

#[test]
fn wire_selection_after_lifecycle_install_is_the_same_save_candidate_selection() {
    let scenes = SceneStorage::new(vec![]).unwrap();
    let mut core = ApplicationCore::new(WorkflowInit {
        working: scenes.clone(),
        alternate: None,
        baseline: BaselineId(5),
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
    let full = FullSnapshot {
        handle: SnapshotHandle(1),
        provenance: Provenance(1),
        default_scene: SceneIndex::new(3).unwrap(),
        storage: Some(StorageIdentity {
            identity: 9000,
            key: 1,
        }),
    };
    let target = StorageTarget {
        folder: 1,
        index: 2,
    };
    core.dispatch_lifecycle(
        LifecycleAction::Attach {
            working: full,
            alternate: None,
            baseline: full,
            current_target: target,
        },
        Millis(0),
    )
    .unwrap();
    let projection = WorkingProjection {
        scenes,
        parameters: None,
    };
    let normalized = FullSnapshot {
        handle: SnapshotHandle(2),
        ..full
    };
    let reset = FullSnapshot {
        handle: SnapshotHandle(3),
        ..full
    };
    core.dispatch_lifecycle(
        LifecycleAction::Load(Box::new(LoadInputs {
            selected_target: target,
            baseline_input_r1: full,
            working_input_r2: full,
            facts: Some(LoadFacts {
                normalized_from: full,
                normalized_working: normalized,
                normalized_projection: projection.clone(),
                reset_from: normalized,
                reset_effective: reset,
                reset_working_projection: Some(projection),
                normalization_evidence: Provenance(1),
                reset_evidence: Provenance(1),
                callbacks: CallbackPolicy::CallerResolvedNonReentrant(Provenance(1)),
            }),
        })),
        Millis(1),
    )
    .unwrap();
    let envelope = Envelope {
        message_type: SCENE_MESSAGE_TYPE,
        transform: 0,
        decompress: 0,
        opaque: [0, 0],
        payload: vec![8, 1, 24, 6],
    };
    let outcome =
        dispatch_application_scene(&mut core, &envelope, SceneLimits::default(), Millis(2))
            .unwrap();
    assert_eq!(outcome.selected_scene, 6);
    assert_eq!(core.state().previous_scene.get(), 3);
    assert_eq!(
        core.lifecycle()
            .unwrap()
            .effective()
            .snapshot
            .default_scene
            .get(),
        3
    );
    let prepared = PreparedCandidate {
        copied_from: core.lifecycle().unwrap().effective(),
        selected: core.state().scene,
        candidate: FullSnapshot {
            handle: SnapshotHandle(4),
            default_scene: core.state().scene,
            ..full
        },
        preparation_evidence: Provenance(1),
        callbacks: CallbackPolicy::CallerResolvedNonReentrant(Provenance(1)),
    };
    let before = core.clone();
    assert_eq!(
        core.dispatch_lifecycle(
            LifecycleAction::BeginSave {
                target,
                prepared: Some(prepared)
            },
            Millis(1)
        ),
        Err(LifecycleError::Core(WorkflowError::ClockRegression))
    );
    assert_eq!(core, before);
    core.dispatch_lifecycle(
        LifecycleAction::BeginSave {
            target,
            prepared: Some(prepared),
        },
        Millis(2),
    )
    .unwrap();
    assert_eq!(
        core.lifecycle()
            .unwrap()
            .in_flight
            .unwrap()
            .candidate
            .default_scene
            .get(),
        6
    );
    assert_eq!(core.state().dirty_deadline, Some(Millis(101)));
    assert!(core.state().ui_dirty);
}

use super::*;

fn text(value: &str) -> UiText {
    UiText::from_literal(value).unwrap()
}

fn storage(count: usize, prefix: &str) -> SceneStorage {
    SceneStorage::new(
        (0..count)
            .map(|index| SceneEntry {
                label: text(&format!("{prefix}{index}")),
                raw_color: index as u32 + 1,
            })
            .collect(),
    )
    .unwrap()
}

fn core() -> ApplicationCore {
    ApplicationCore::new(WorkflowInit {
        working: storage(9, "working"),
        alternate: None,
        baseline: BaselineId(7),
        mode: ModeId(0),
        previous_mode: ModeId(0),
        page: 12,
        ui_dirty: false,
        last_notified_dirty: false,
        position: BankPosition {
            bank: CallerValidatedBank(2),
            slot: BankSlot::new(3).unwrap(),
        },
        title: text("begin"),
        requested: Some(PresetIndex(19)),
        loaded: Some(PresetIndex(5)),
    })
}

fn comparison(core: &ApplicationCore, differs: bool) -> SemanticComparison {
    SemanticComparison {
        effective: core.state().effective_stamp(),
        baseline: core.state().baseline,
        differs,
    }
}

fn send(core: &mut ApplicationCore, action: Action) -> Vec<Effect> {
    core.dispatch(action, Millis(0)).unwrap()
}

#[test]
fn scene_force_same_and_front_guard_preserve_previous_selection() {
    let mut core = core();
    let one = SceneIndex::new(1).unwrap();
    assert!(send(&mut core, Action::FrontSelectScene(SceneIndex(0))).is_empty());
    assert_eq!(
        send(
            &mut core,
            Action::SelectScene {
                scene: one,
                force: false
            }
        ),
        vec![
            Effect::Backend(BackendRequest::PrepareSceneSelection),
            Effect::Observer(ObserverEvent::SceneSelected {
                current: one,
                previous: SceneIndex(0)
            }),
        ]
    );
    assert!(
        send(
            &mut core,
            Action::SelectScene {
                scene: one,
                force: false
            }
        )
        .is_empty()
    );
    let forced = send(
        &mut core,
        Action::SelectScene {
            scene: one,
            force: true,
        },
    );
    assert!(
        matches!(forced[1], Effect::Observer(ObserverEvent::SceneSelected { current, previous }) if current == one && previous == one)
    );
    send(
        &mut core,
        Action::BeginBank {
            special_folder: false,
        },
    );
    assert!(send(&mut core, Action::FrontSelectScene(SceneIndex(2))).is_empty());
    assert_eq!(core.state().scene, one);
    send(
        &mut core,
        Action::SelectScene {
            scene: SceneIndex(2),
            force: false,
        },
    );
    assert_eq!(core.state().previous_scene, one);
    assert_eq!(SceneIndex::new(-1), Err(WorkflowError::InvalidSlot));
    assert_eq!(SceneIndex::new(8), Err(WorkflowError::InvalidSlot));
}

#[test]
fn numeric_mode_fallback_cycle_and_visual_actions_have_distinct_contracts() {
    let mut core = core();
    let effects = send(&mut core, Action::ModeShortRelease);
    assert_eq!(
        effects,
        vec![
            Effect::Backend(BackendRequest::PrepareModeChange),
            Effect::Persistence(PersistenceRequest::ModeSetting(ModeId(1))),
            Effect::Observer(ObserverEvent::ModeUpdated {
                current: ModeId(1),
                previous: ModeId(0)
            }),
            Effect::Observer(ObserverEvent::PageUpdated(12)),
            Effect::Control(ControlRequest::Mode(ModeId(1))),
        ]
    );
    send(&mut core, Action::ModeShortRelease);
    send(&mut core, Action::ModeShortRelease);
    assert_eq!(core.state().mode, ModeId(0));
    send(
        &mut core,
        Action::SetAllowedModes(AllowedModes::new(vec![ModeId(9), ModeId(4)]).unwrap()),
    );
    send(&mut core, Action::ModeShortRelease); // Absent current selects first.
    assert_eq!(core.state().mode, ModeId(9));
    send(
        &mut core,
        Action::SetMode {
            requested: 4,
            persist: false,
        },
    );
    let invalid = send(
        &mut core,
        Action::SetMode {
            requested: -1,
            persist: false,
        },
    );
    assert_eq!(core.state().mode, ModeId(9));
    assert!(
        !invalid
            .iter()
            .any(|effect| matches!(effect, Effect::Persistence(_)))
    );
    send(
        &mut core,
        Action::SetMode {
            requested: 4,
            persist: false,
        },
    );
    send(
        &mut core,
        Action::SetMode {
            requested: 100,
            persist: true,
        },
    );
    assert_eq!(core.state().mode, ModeId(9));
    send(
        &mut core,
        Action::SetMode {
            requested: 4,
            persist: false,
        },
    );
    send(
        &mut core,
        Action::SetMode {
            requested: 1,
            persist: true,
        },
    ); // Valid but disallowed.
    assert_eq!(core.state().mode, ModeId(9));
    assert!(
        send(
            &mut core,
            Action::SetMode {
                requested: 9,
                persist: true
            }
        )
        .is_empty()
    );
    assert_eq!(
        send(
            &mut core,
            Action::ModeVisualPress {
                control_color: 0xdead_beef
            }
        ),
        vec![Effect::Renderer(RendererRequest::ModeButtonColor(
            0xdead_beef
        ))]
    );
    assert_eq!(
        send(&mut core, Action::ModeVisualRelease),
        vec![Effect::Renderer(RendererRequest::ModeButtonColor(
            0x001e_1e1e
        ))]
    );
    assert_eq!(core.state().mode, ModeId(9));
    send(
        &mut core,
        Action::SetAllowedModes(AllowedModes::new(vec![]).unwrap()),
    );
    assert!(
        send(
            &mut core,
            Action::SetMode {
                requested: 0,
                persist: true
            }
        )
        .is_empty()
    );
    let before = core.clone();
    assert_eq!(
        core.dispatch(Action::ModeShortRelease, Millis(1)),
        Err(WorkflowError::EmptyModeCycle)
    );
    assert_eq!(core, before);
}

#[test]
fn scene_dialog_is_a_draft_and_confirms_each_label_then_nonzero_color() {
    let mut core = core();
    let colors = [0, 11, 0, 13, 14, 15, 16, u32::MAX];
    send(
        &mut core,
        Action::OpenSceneDialog {
            observed_colors: colors,
        },
    );
    let original = core.state().working.clone();
    let literal = UiText::from_units(vec![32, 32, 0xd800, 9, 32]).unwrap();
    send(&mut core, Action::SetDraftLabelLiteral(literal.clone()));
    send(&mut core, Action::SetDraftColor(0));
    assert_eq!(core.state().working, original);
    assert!(core.state().dirty_deadline.is_none());
    assert_eq!(
        send(&mut core, Action::CloseSceneDialog),
        vec![Effect::Renderer(RendererRequest::HideSceneDialog)]
    );
    assert_eq!(core.state().working, original);
    send(
        &mut core,
        Action::OpenSceneDialog {
            observed_colors: colors,
        },
    );
    send(&mut core, Action::SetDraftLabelLiteral(literal.clone()));
    let effects = send(&mut core, Action::ConfirmSceneDialog);
    let submissions: Vec<_> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Control(ControlRequest::SceneLabel { index, .. }) => Some((*index, false)),
            Effect::Control(ControlRequest::SceneColor { index, .. }) => Some((*index, true)),
            _ => None,
        })
        .collect();
    assert_eq!(
        submissions,
        vec![
            (0, false),
            (1, false),
            (1, true),
            (2, false),
            (3, false),
            (3, true),
            (4, false),
            (4, true),
            (5, false),
            (5, true),
            (6, false),
            (6, true),
            (7, false),
            (7, true),
        ]
    );
    assert_eq!(core.state().working.entries()[0].label, literal);
    assert_eq!(core.state().working.entries()[0].raw_color, 1); // Zero skips color.
    assert_eq!(core.state().working.entries()[7].raw_color, u32::MAX);
    assert_eq!(core.state().working.entries()[8], original.entries()[8]);
    assert_eq!(
        effects.last(),
        Some(&Effect::Renderer(RendererRequest::HideSceneDialog))
    );
    assert!(effects.len() <= MAX_EFFECTS_PER_ACTION);
    assert!(core.state().dialog.is_none());
    assert_eq!(core.state().dirty_deadline, Some(Millis(100)));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Persistence(_)))
    );
}

#[test]
fn alternate_labels_and_comparison_stay_distinct_from_working_colors() {
    let mut core = core();
    core.state.alternate = Some(storage(8, "alternate"));
    let working_label = core.state().working.entries()[0].label.clone();
    let stamp = core.state().effective_stamp();
    send(
        &mut core,
        Action::SetSceneColor {
            index: 0,
            raw_color: 0,
        },
    );
    assert_eq!(core.state().effective_stamp(), stamp);
    assert_eq!(core.state().working.entries()[0].raw_color, 0);
    assert_eq!(
        core.state().alternate.as_ref().unwrap().entries()[0].raw_color,
        1
    );
    let effects = send(
        &mut core,
        Action::SetSceneLabel {
            index: 0,
            label: text("  literal\t "),
        },
    );
    assert!(matches!(
        &effects[0],
        Effect::Control(ControlRequest::SceneLabel {
            context: PresetContext::Alternate,
            ..
        })
    ));
    assert_eq!(core.state().working.entries()[0].label, working_label);
    assert_eq!(
        core.state().effective_scenes().entries()[0].label,
        text("  literal\t ")
    );
    send(
        &mut core,
        Action::OpenSceneDialog {
            observed_colors: [0; 8],
        },
    );
    assert_eq!(
        core.state().dialog.as_ref().unwrap().labels[0],
        text("  literal\t ")
    );
    let before = core.clone();
    assert_eq!(
        core.dispatch(
            Action::ForceDirtyEvaluation(SemanticComparison {
                effective: stamp,
                baseline: BaselineId(7),
                differs: false
            }),
            Millis(1)
        ),
        Err(WorkflowError::StaleComparison)
    );
    assert_eq!(core, before);
    let differs = comparison(&core, true);
    send(&mut core, Action::ForceDirtyEvaluation(differs));
    assert!(core.state().ui_dirty);
}

#[test]
fn dirty_timer_restarts_reverts_and_observes_before_notification_cache_store() {
    let mut core = core();
    assert_eq!(
        core.dispatch(Action::ScheduleDirty, Millis(10)).unwrap(),
        vec![Effect::DirtyTimerRestarted {
            deadline: Millis(110)
        }]
    );
    core.dispatch(Action::ScheduleDirty, Millis(70)).unwrap();
    let dirty = comparison(&core, true);
    assert!(
        core.dispatch(Action::DirtyTimeout(dirty), Millis(110))
            .unwrap()
            .is_empty()
    );
    assert!(!core.state().ui_dirty);
    let effects = core
        .dispatch(Action::DirtyTimeout(dirty), Millis(170))
        .unwrap();
    let observation = DirtyObservation {
        effective: dirty.effective,
        ui_dirty: true,
        last_notified_dirty: false,
    };
    assert_eq!(
        effects,
        vec![
            Effect::DirtyTimerStopped,
            Effect::Renderer(RendererRequest::RefreshDirtyTitle(observation)),
            Effect::Observer(ObserverEvent::DirtyChanged(observation)),
            Effect::Control(ControlRequest::Dirty(observation)),
            Effect::DirtyNotificationCacheStored(true),
        ]
    );
    assert!(core.state().last_notified_dirty);
    assert!(core.state().dirty_deadline.is_none());
    assert!(
        core.dispatch(Action::DirtyTimeout(dirty), Millis(171))
            .unwrap()
            .is_empty()
    );
    core.dispatch(Action::ScheduleDirty, Millis(200)).unwrap();
    let clean = comparison(&core, false);
    let reverted = core
        .dispatch(Action::DirtyTimeout(clean), Millis(300))
        .unwrap();
    assert!(matches!(
        reverted[2],
        Effect::Observer(ObserverEvent::DirtyChanged(DirtyObservation {
            ui_dirty: false,
            last_notified_dirty: true,
            ..
        }))
    ));
    assert_eq!(
        reverted.last(),
        Some(&Effect::DirtyNotificationCacheStored(false))
    );
    assert!(!core.state().ui_dirty);
    assert!(!core.state().last_notified_dirty);
    let equal = core
        .dispatch(Action::ForceDirtyEvaluation(clean), Millis(301))
        .unwrap();
    assert!(
        !equal
            .iter()
            .any(|effect| matches!(effect, Effect::Observer(_) | Effect::Control(_)))
    );
    let mut wrong_baseline = clean;
    wrong_baseline.baseline = BaselineId(8);
    let before = core.clone();
    assert_eq!(
        core.dispatch(Action::ForceDirtyEvaluation(wrong_baseline), Millis(302)),
        Err(WorkflowError::StaleComparison)
    );
    assert_eq!(core, before);
    core.dispatch(Action::InvalidateEffectiveComparison, Millis(302))
        .unwrap();
    assert_eq!(
        core.dispatch(Action::ForceDirtyEvaluation(clean), Millis(302)),
        Err(WorkflowError::StaleComparison)
    );
}

#[test]
fn bank_preview_observes_old_title_and_stops_before_cancel_restoration() {
    let mut core = core();
    assert!(send(&mut core, Action::CancelBank).is_empty());
    assert!(
        send(
            &mut core,
            Action::BeginBank {
                special_folder: true
            }
        )
        .is_empty()
    );
    let dirty = comparison(&core, true);
    send(&mut core, Action::ForceDirtyEvaluation(dirty));
    let started = send(
        &mut core,
        Action::BeginBank {
            special_folder: false,
        },
    );
    assert!(matches!(
        started.last(),
        Some(Effect::Observer(ObserverEvent::BankStarted(
            BankObservation { active: true, .. }
        )))
    ));
    assert!(
        send(
            &mut core,
            Action::BeginBank {
                special_folder: false
            }
        )
        .is_empty()
    );
    send(&mut core, Action::SetPreviewBank(CallerValidatedBank(4)));
    let effects = send(
        &mut core,
        Action::SelectPreviewSlot {
            slot: BankSlot(6),
            resolved_title: UiText::default(),
            empty_title_fallback: text("fallback"),
        },
    );
    assert!(
        matches!(&effects[0], Effect::Observer(ObserverEvent::PresetChanged(observation)) if observation.title == text("begin") && observation.requested == Some(PresetIndex(38)) && observation.loaded == Some(PresetIndex(5)))
    );
    assert_eq!(
        effects[1],
        Effect::Renderer(RendererRequest::PreviewTitle(text("fallback")))
    );
    let again = send(
        &mut core,
        Action::SelectPreviewSlot {
            slot: BankSlot(6),
            resolved_title: text("preview"),
            empty_title_fallback: text("fallback"),
        },
    );
    assert!(matches!(
        again[0],
        Effect::Observer(ObserverEvent::PresetChanged(_))
    )); // No same-slot guard.
    let stopped = send(&mut core, Action::CancelBank);
    assert!(
        matches!(&stopped[2], Effect::Observer(ObserverEvent::BankStopped(observation)) if !observation.active && observation.position.bank == CallerValidatedBank(4) && observation.title == text("preview") && observation.requested == Some(PresetIndex(38)))
    );
    assert!(
        matches!(&stopped[3], Effect::PreviewRestored(observation) if !observation.active && observation.position.bank == CallerValidatedBank(2) && observation.title == text("begin") && observation.requested == Some(PresetIndex(19)))
    );
    assert_eq!(core.state().bank.loaded, Some(PresetIndex(5)));
    assert!(core.state().ui_dirty);
    send(
        &mut core,
        Action::BeginBank {
            special_folder: false,
        },
    );
    send(
        &mut core,
        Action::SelectPreviewSlot {
            slot: BankSlot(7),
            resolved_title: text("retained"),
            empty_title_fallback: text("fallback"),
        },
    );
    let ended = send(&mut core, Action::EndBank);
    assert!(
        ended
            .iter()
            .any(|effect| matches!(effect, Effect::PreviewEnded(_)))
    );
    assert_eq!(core.state().bank.position.slot, BankSlot(7));
    assert_eq!(core.state().bank.requested, Some(PresetIndex(23)));
    assert_eq!(core.state().bank.loaded, Some(PresetIndex(5)));
    assert!(matches!(
        send(&mut core, Action::EndBank)[2],
        Effect::Observer(ObserverEvent::BankStopped(_))
    ));
    send(
        &mut core,
        Action::RecordLoadedIdentity(Some(PresetIndex(23))),
    );
    assert_eq!(core.state().bank.loaded, Some(PresetIndex(23)));
    assert!(
        !ended
            .iter()
            .any(|effect| matches!(effect, Effect::Persistence(_)))
    );
}

#[test]
fn malformed_bounds_time_revision_and_bank_overflow_are_atomic() {
    let mut core = core();
    for action in [
        Action::SetSceneLabel {
            index: 9,
            label: text("invalid"),
        },
        Action::SetSceneColor {
            index: usize::MAX,
            raw_color: 5,
        },
        Action::ConfirmSceneDialog,
    ] {
        let before = core.clone();
        assert!(core.dispatch(action, Millis(1)).is_err());
        assert_eq!(core, before);
    }
    let before = core.clone();
    assert_eq!(
        core.dispatch(
            Action::SetSceneLabel {
                index: 0,
                label: text("overflow")
            },
            Millis(u64::MAX - 99)
        ),
        Err(WorkflowError::Overflow)
    );
    assert_eq!(core, before);
    core.dispatch(Action::ScheduleDirty, Millis(5)).unwrap();
    let before = core.clone();
    assert_eq!(
        core.dispatch(Action::EndBank, Millis(4)),
        Err(WorkflowError::ClockRegression)
    );
    assert_eq!(core, before);
    core.state.working_revision = u64::MAX;
    let before = core.clone();
    assert_eq!(
        core.dispatch(
            Action::SetSceneColor {
                index: 0,
                raw_color: 0
            },
            Millis(5)
        ),
        Err(WorkflowError::Overflow)
    );
    assert_eq!(core, before);

    let mut core = super::tests::core();
    send(
        &mut core,
        Action::SetPreviewBank(CallerValidatedBank(u64::MAX)),
    );
    let before = core.clone();
    assert_eq!(
        core.dispatch(
            Action::SelectPreviewSlot {
                slot: BankSlot(0),
                resolved_title: text("overflow"),
                empty_title_fallback: UiText::default()
            },
            Millis(1)
        ),
        Err(WorkflowError::Overflow)
    );
    assert_eq!(core, before);
    send(
        &mut core,
        Action::BeginBank {
            special_folder: false,
        },
    );
    send(&mut core, Action::SetPreviewBank(CallerValidatedBank(0)));
    let before = core.clone();
    assert_eq!(
        core.dispatch(Action::CancelBank, Millis(1)),
        Err(WorkflowError::Overflow)
    );
    assert_eq!(core, before);
    assert!(core.state().bank.active);
    let max_valid = BankPosition {
        bank: CallerValidatedBank(u64::MAX / 8),
        slot: BankSlot(7),
    };
    assert_eq!(max_valid.absolute_index().unwrap(), PresetIndex(u64::MAX));
}

#[test]
fn owned_limits_context_cardinality_and_unresolved_adapters_are_explicit() {
    assert_eq!(
        UiText::from_units(vec![0; MAX_TEXT_UNITS + 1]),
        Err(WorkflowError::ResourceLimit)
    );
    assert_eq!(
        SceneStorage::new(vec![
            SceneEntry {
                label: UiText::default(),
                raw_color: 0
            };
            MAX_SCENE_STORAGE_SLOTS + 1
        ]),
        Err(WorkflowError::ResourceLimit)
    );
    assert_eq!(
        AllowedModes::new(vec![ModeId(0); MAX_ALLOWED_MODES + 1]),
        Err(WorkflowError::ResourceLimit)
    );
    assert_eq!(ModeId::new(10), Err(WorkflowError::InvalidMode));
    assert_eq!(BankSlot::new(-1), Err(WorkflowError::InvalidSlot));
    let mut core = core();
    core.state.alternate = Some(storage(7, "short"));
    let before = core.clone();
    assert_eq!(
        core.dispatch(
            Action::OpenSceneDialog {
                observed_colors: [1; 8]
            },
            Millis(1)
        ),
        Err(WorkflowError::StorageIndex { index: 7, count: 7 })
    );
    assert_eq!(core, before);
    core.state.alternate = Some(storage(8, "alternate"));
    core.state.working = storage(1, "short");
    send(
        &mut core,
        Action::OpenSceneDialog {
            observed_colors: [1; 8],
        },
    );
    let before = core.clone();
    assert_eq!(
        core.dispatch(Action::ConfirmSceneDialog, Millis(1)),
        Err(WorkflowError::StorageIndex { index: 1, count: 1 })
    );
    assert_eq!(core, before); // Prior callback intents and edits are discarded.
    assert_eq!(
        core.dispatch(Action::NormalizeDraftKeyboard(text(" a\t b ")), Millis(1)),
        Err(WorkflowError::UnsupportedAdapter(
            Adapter::KeyboardNormalization
        ))
    );
    assert_eq!(core, before);
    assert_eq!(
        Effect::Persistence(PersistenceRequest::ModeSetting(ModeId(1))).adapter_support(),
        AdapterSupport::Unsupported(Adapter::Persistence)
    );
}

#[test]
fn pinned_qt48_simplification_classifies_every_utf16_unit() {
    let spaces = [
        0x0009, 0x000a, 0x000b, 0x000c, 0x000d, 0x0020, 0x00a0, 0x1680, 0x180e, 0x2000, 0x2001,
        0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a, 0x2028, 0x2029,
        0x202f, 0x205f, 0x3000,
    ];
    for unit in 0..=u16::MAX {
        let output = UiText::from_units(vec![unit])
            .unwrap()
            .simplified_pinned_qt48();
        if spaces.contains(&unit) {
            assert!(output.units().is_empty(), "unit {unit:04x}");
        } else {
            assert_eq!(output.units(), [unit], "unit {unit:04x}");
        }
    }
}

#[test]
fn pinned_qt48_simplification_preserves_units_and_existing_text_limits() {
    let input = vec![
        0x0020, 0x180e, 0xd800, 0x00a0, 0x0009, 0x2007, 0xdc00, 0x0085, 0x200b, 0x0000, 0xfeff,
        0x3000,
    ];
    let output = UiText::from_units(input).unwrap().simplified_pinned_qt48();
    assert_eq!(
        output.units(),
        [0xd800, 0x0020, 0xdc00, 0x0085, 0x200b, 0, 0xfeff]
    );
    assert_eq!(output.clone().simplified_pinned_qt48(), output);
    assert_eq!(
        UiText::default().simplified_pinned_qt48(),
        UiText::default()
    );
    let all_spaces = UiText::from_units(vec![0x180e; MAX_TEXT_UNITS]).unwrap();
    assert_eq!(all_spaces.simplified_pinned_qt48(), UiText::default());
    let full = UiText::from_units(vec![0xd800; MAX_TEXT_UNITS]).unwrap();
    assert_eq!(full.clone().simplified_pinned_qt48(), full);
    assert_eq!(
        UiText::from_units(vec![0x0020; MAX_TEXT_UNITS + 1]),
        Err(WorkflowError::ResourceLimit)
    );
}

#[test]
fn pinned_keyboard_action_changes_only_selected_draft_and_errors_are_atomic() {
    let mut core = core();
    let before = core.clone();
    assert_eq!(
        core.dispatch(
            Action::NormalizeDraftKeyboardPinnedQt48(text(" a ")),
            Millis(0)
        ),
        Err(WorkflowError::NoDialog)
    );
    assert_eq!(core, before);
    send(
        &mut core,
        Action::OpenSceneDialog {
            observed_colors: [0; 8],
        },
    );
    send(&mut core, Action::SelectDraftScene(SceneIndex(6)));
    let literal = UiText::from_units(vec![
        0x180e, 0xd800, 0x00a0, 9, 0xdc00, 0x0085, 0, 0xfeff, 0x3000,
    ])
    .unwrap();
    let normalized = UiText::from_units(vec![0xd800, 0x0020, 0xdc00, 0x0085, 0, 0xfeff]).unwrap();
    let mut expected = core.clone();
    expected.state.dialog.as_mut().unwrap().labels[6] = normalized.clone();
    assert!(
        send(
            &mut core,
            Action::NormalizeDraftKeyboardPinnedQt48(literal.clone())
        )
        .is_empty()
    );
    assert_eq!(core, expected);
    assert!(send(&mut core, Action::SetDraftLabelLiteral(literal.clone())).is_empty());
    assert_eq!(core.state.dialog.as_ref().unwrap().labels[6], literal);
    assert!(
        core.dispatch(
            Action::NormalizeDraftKeyboardPinnedQt48(literal),
            Millis(10)
        )
        .unwrap()
        .is_empty()
    );
    let before = core.clone();
    assert_eq!(
        core.dispatch(
            Action::NormalizeDraftKeyboardPinnedQt48(text("other")),
            Millis(9)
        ),
        Err(WorkflowError::ClockRegression)
    );
    assert_eq!(core, before);
    let working = core.state.working.clone();
    core.dispatch(Action::ConfirmSceneDialog, Millis(10))
        .unwrap();
    assert!(core.state.dialog.is_none());
    for index in 0..working.entries().len() {
        let expected = if index == 6 {
            &normalized
        } else {
            &working.entries()[index].label
        };
        assert_eq!(&core.state.working.entries()[index].label, expected);
        assert_eq!(
            core.state.working.entries()[index].raw_color,
            working.entries()[index].raw_color
        );
    }
}

#[test]
fn stomp_rename_orders_label_momentary_dirty_and_refresh_effects() {
    use crate::performance::stomp::StompIndex;
    let stomp = StompIndex::new(4).unwrap();
    let rename = |label: &str, momentary: bool, current: bool| StompRename {
        stomp,
        label: text(label),
        momentary,
        assigned_count: 2,
        generated_default: text("DRIVE"),
        current_momentary: current,
        atma: false,
        non_momentary_model_assigned: false,
        mirror_blocks: vec![BlockAddress { row: 1, column: 2 }],
    };
    let mut core = core();
    let effects = core
        .dispatch(
            Action::Stomp(StompAction::Rename(rename("LEAD", true, false))),
            Millis(10),
        )
        .unwrap();
    assert_eq!(
        effects,
        [
            Effect::Control(ControlRequest::StompLabel {
                stomp: 4,
                grouped: true
            }),
            Effect::Stomp(StompEffect::MirrorMomentary {
                block: BlockAddress { row: 1, column: 2 },
                momentary: true,
            }),
            Effect::Control(ControlRequest::StompMomentary(4)),
            Effect::DirtyTimerRestarted {
                deadline: Millis(110)
            },
            Effect::Renderer(RendererRequest::RefreshStomps),
            Effect::Renderer(RendererRequest::RefreshTitle),
        ]
    );
    assert_eq!(
        core.state().stomps.grouped_labels.get(&stomp),
        Some(&text("LEAD"))
    );
    assert_eq!(core.state().stomps.momentary.get(&stomp), Some(&true));
    // The generated default clears the stored label; same values only refresh.
    core.dispatch(
        Action::Stomp(StompAction::Rename(rename("DRIVE", true, true))),
        Millis(20),
    )
    .unwrap();
    assert!(core.state().stomps.grouped_labels.is_empty());
    let effects = core
        .dispatch(
            Action::Stomp(StompAction::Rename(rename("", true, true))),
            Millis(30),
        )
        .unwrap();
    assert_eq!(
        effects,
        [
            Effect::Renderer(RendererRequest::RefreshStomps),
            Effect::Renderer(RendererRequest::RefreshTitle),
        ]
    );
}

use tetrad_control::application::{ApplicationSceneOutcome, dispatch_application_scene};
use tetrad_control::preset_working::{
    CaptureAdmission, Omission, OmissionAck, PresetDocumentAssertion, WorkingPresetError,
    WorkingShell, project_scene_prefix,
};
use tetrad_control::scene::{SCENE_MESSAGE_TYPE, SceneLimits};
use tetrad_control::{Envelope, Error};
use tetrad_core::workflow::{
    Action, AdapterSupport, ApplicationCore, BankPosition, BankSlot, BaselineId,
    CallerValidatedBank, LifecycleError, MAX_SCENE_STORAGE_SLOTS, MAX_TEXT_UNITS, Millis, ModeId,
    PresetIndex, SceneStorage, UiText, WorkflowError, WorkflowInit,
};
use tetrad_preset::{Limits, PresetDocument};
use tetrad_storage::{Library, Mutation, Product, ProductData};

fn limits() -> Limits {
    Limits {
        max_bytes: 128 * 1024,
        max_depth: 8,
        max_fields: 2048,
    }
}

fn shell() -> WorkingShell {
    WorkingShell {
        baseline: BaselineId(17),
        mode: ModeId::new(2).unwrap(),
        previous_mode: ModeId::new(1).unwrap(),
        page: 9,
        ui_dirty: true,
        last_notified_dirty: false,
        position: BankPosition {
            bank: CallerValidatedBank(3),
            slot: BankSlot::new(2).unwrap(),
        },
        title: UiText::from_literal("Caller title, not document name").unwrap(),
        requested: Some(PresetIndex(26)),
        loaded: Some(PresetIndex(99)),
    }
}

fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 128 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn uint(field: u32, value: u32, out: &mut Vec<u8>) {
    varint(u64::from(field) << 3, out);
    varint(u64::from(value), out);
}

fn blob(field: u32, bytes: &[u8], out: &mut Vec<u8>) {
    varint((u64::from(field) << 3) | 2, out);
    varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

// Entirely synthetic schema-shaped bytes, not a complete preset producer.
fn document(labels: &[&str], colors: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::new();
    blob(2, b"Synthetic document name", &mut bytes);
    uint(3, 0xfeed_beef, &mut bytes);
    uint(7, 4, &mut bytes);
    for tempo in [120, 121, 122, 123] {
        uint(14, tempo, &mut bytes);
    }
    for label in labels {
        blob(15, label.as_bytes(), &mut bytes);
    }
    for &color in colors {
        uint(31, color, &mut bytes);
    }
    let mut param = Vec::new();
    for value in 0..6 {
        let mut data = Vec::new();
        uint(1, value, &mut data);
        blob(5, &data, &mut param);
    }
    let mut model = Vec::new();
    blob(2, &param, &mut model);
    let mut chain = Vec::new();
    blob(5, &model, &mut chain);
    blob(11, &chain, &mut bytes);
    blob(100, b"opaque unknown retained bytes", &mut bytes);
    bytes
}

fn envelope(payload: &[u8]) -> Envelope {
    Envelope {
        message_type: SCENE_MESSAGE_TYPE,
        transform: 0,
        decompress: 0,
        opaque: [0xa5, 0x5a],
        payload: payload.to_vec(),
    }
}

#[test]
fn owned_library_bytes_install_prefix_and_route_existing_scene_actions_without_loading() {
    let bytes = document(&["  Literal label 😀  ", "Second"], &[0x1234_5678, 0]);
    let products = [
        Product {
            id: "synthetic-a".into(),
            product_type: -914,
            read_only: false,
            data: ProductData {
                raw_data: Some(bytes.clone()),
                metadata: Some(vec![0xff, 0x80, 0, 7]),
            },
        },
        Product {
            id: "synthetic-b".into(),
            product_type: 0,
            read_only: true,
            data: ProductData {
                raw_data: Some(bytes.clone()),
                metadata: Some(vec![9, 8]),
            },
        },
        Product {
            id: "absent-blob".into(),
            product_type: 13,
            read_only: false,
            data: ProductData {
                raw_data: None,
                metadata: None,
            },
        },
        Product {
            id: "empty-blob".into(),
            product_type: 13,
            read_only: false,
            data: ProductData {
                raw_data: Some(Vec::new()),
                metadata: Some(Vec::new()),
            },
        },
    ];
    let mut library = Library::in_memory(tetrad_storage::Limits::default()).unwrap();
    library
        .apply(
            &products
                .iter()
                .map(Mutation::InsertProduct)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    for product in &products[..2] {
        let row = library.product(&product.id).unwrap().unwrap();
        let source = row.data.raw_data.as_deref().unwrap();
        let capture = project_scene_prefix(source, PresetDocumentAssertion, limits()).unwrap();
        assert_eq!(
            capture.admission(),
            CaptureAdmission::SceneLabelColorPrefixOnly
        );
        assert_eq!(capture.pairs().len(), 2);
        assert_eq!(capture.pairs()[1].raw_color, 0);
        assert_eq!(
            capture.pairs()[0].label.units(),
            "  Literal label 😀  ".encode_utf16().collect::<Vec<_>>()
        );
        assert!(capture.omissions().is_empty());
        let summary = capture.document().summary();
        assert_eq!(summary.name.as_deref(), Some("Synthetic document name"));
        assert_eq!(summary.hash, Some(0xfeed_beef));
        assert_eq!(summary.default_scene, Some(4));
        assert_eq!(summary.scene_count, 6);
        assert_eq!(
            summary.scenes.iter().filter(|s| s.tempo.is_some()).count(),
            4
        );
        assert_eq!(
            (
                summary.chain_count,
                summary.parameter_count,
                summary.scene_value_count
            ),
            (1, 1, 6)
        );
        assert_eq!(summary.unknown_field_count, 1);
        let caller = shell();
        let mut installed = capture
            .install(OmissionAck::RequireNoOmissions, caller.clone())
            .unwrap();
        let state = installed.core.state();
        assert_eq!((state.scene.get(), state.previous_scene.get()), (0, 0));
        assert_eq!(state.working.entries().len(), 2);
        assert_eq!(state.working.entries()[1].raw_color, 0);
        assert_eq!(
            (state.baseline, state.mode, state.previous_mode, state.page),
            (
                caller.baseline,
                caller.mode,
                caller.previous_mode,
                caller.page
            )
        );
        assert_eq!((state.ui_dirty, state.last_notified_dirty), (true, false));
        assert_eq!(state.bank.position, caller.position);
        assert_eq!(state.bank.title, caller.title);
        assert_eq!(state.bank.requested, caller.requested);
        assert_eq!(state.bank.loaded, Some(PresetIndex(99)));
        assert!(state.alternate.is_none());
        assert!(state.dialog.is_none());
        assert!(state.working_parameters.is_none());
        assert_eq!(state.parameter_edit_timers, [None; 4]);
        assert!(state.dirty_deadline.is_none());
        assert!(installed.core.lifecycle().is_none());
        assert!(matches!(
            installed.core.save_stamp(),
            Err(LifecycleError::NotAttached)
        ));
        let outcome: ApplicationSceneOutcome = dispatch_application_scene(
            &mut installed.core,
            &envelope(&[8, 1, 16, 42, 24, 1]),
            SceneLimits::default(),
            Millis(10),
        )
        .unwrap();
        assert_eq!((outcome.selected_scene, outcome.request_id), (1, Some(42)));
        assert!(!outcome.effects.is_empty());
        assert!(
            outcome
                .effects
                .iter()
                .all(|effect| matches!(effect.adapter_support(), AdapterSupport::Unsupported(_)))
        );
        let before_read = installed.core.clone();
        let read = dispatch_application_scene(
            &mut installed.core,
            &envelope(&[8, 3]),
            SceneLimits::default(),
            Millis(0),
        )
        .unwrap();
        assert_eq!(read.selected_scene, 1);
        assert!(read.effects.is_empty());
        assert_eq!(installed.core, before_read);
        assert!(installed.core.state().dirty_deadline.is_none());
        assert_eq!(
            installed.admission(),
            CaptureAdmission::SceneLabelColorPrefixOnly
        );
        assert_eq!(installed.document().bytes(), source);
        assert_eq!(
            library.product(&product.id).unwrap().as_ref(),
            Some(product)
        );
    }
    let absent = library.product("absent-blob").unwrap().unwrap();
    let empty = library.product("empty-blob").unwrap().unwrap();
    assert_eq!(absent.data.raw_data, None);
    assert_eq!(empty.data.raw_data.as_deref(), Some(&[][..]));
    let capture = project_scene_prefix(
        empty.data.raw_data.as_deref().unwrap(),
        PresetDocumentAssertion,
        limits(),
    )
    .unwrap();
    assert!(capture.pairs().is_empty());
    assert!(capture.omissions().is_empty());
    assert_eq!(capture.document().summary().scene_count, 0);
    assert_eq!(capture.document().bytes(), &[]);
    // Generic empty protobuf acceptance never upgrades this admission.
    let installed = capture
        .install(OmissionAck::RequireNoOmissions, shell())
        .unwrap();
    assert_eq!(
        installed.admission(),
        CaptureAdmission::SceneLabelColorPrefixOnly
    );
    assert!(installed.core.state().working.entries().is_empty());
    for product in &products {
        assert_eq!(
            library.product(&product.id).unwrap().as_ref(),
            Some(product)
        );
    }
}

#[test]
fn both_tail_directions_require_exact_acknowledgement_and_ignore_summary_padding() {
    let cases = [
        (
            document(&["paired", "tail-1", "tail-2"], &[0]),
            vec![
                Omission::LabelWithoutColor { index: 1 },
                Omission::LabelWithoutColor { index: 2 },
            ],
        ),
        (
            document(&["paired"], &[0, 22, 33]),
            vec![
                Omission::ColorWithoutLabel { index: 1 },
                Omission::ColorWithoutLabel { index: 2 },
            ],
        ),
    ];
    for (bytes, expected) in cases {
        let project = || project_scene_prefix(&bytes, PresetDocumentAssertion, limits()).unwrap();
        let capture = project();
        assert_eq!(capture.omissions(), expected);
        assert_eq!(capture.pairs().len(), 1);
        assert_eq!(capture.pairs()[0].raw_color, 0);
        assert_eq!(capture.document().summary().scene_count, 6);
        assert!(matches!(
            capture.install(OmissionAck::RequireNoOmissions, shell()),
            Err(WorkingPresetError::UnacknowledgedOmissions { count: 2 })
        ));
        for actual in [0, 1, 3] {
            assert!(
                matches!(project().install(OmissionAck::ExplicitTail { count: actual }, shell()), Err(WorkingPresetError::OmissionCountMismatch { expected: 2, actual: got }) if got == actual)
            );
        }
        let installed = project()
            .install(OmissionAck::ExplicitTail { count: 2 }, shell())
            .unwrap();
        assert_eq!(installed.omissions(), expected);
        assert_eq!(installed.core.state().working.entries().len(), 1);
        assert_eq!(installed.core.state().working.entries()[0].raw_color, 0);
        assert_eq!(installed.document().bytes(), bytes);
        assert_eq!(
            PresetDocument::parse(installed.document().bytes(), limits())
                .unwrap()
                .to_bytes(),
            bytes
        );
        assert_eq!(installed.document().summary().unknown_field_count, 1);
    }
    // Working bounds apply only to paired content; a retained tail is opaque here.
    let long = "😀".repeat(MAX_TEXT_UNITS);
    let bytes = document(&["paired", &long], &[0]);
    let capture = project_scene_prefix(&bytes, PresetDocumentAssertion, limits()).unwrap();
    assert_eq!(
        capture.omissions(),
        &[Omission::LabelWithoutColor { index: 1 }]
    );
    assert_eq!(capture.document().bytes(), bytes);
    let equal = document(&[""], &[0]);
    let project = || project_scene_prefix(&equal, PresetDocumentAssertion, limits()).unwrap();
    assert!(
        project()
            .install(OmissionAck::ExplicitTail { count: 0 }, shell())
            .is_ok()
    );
    assert!(matches!(
        project().install(OmissionAck::ExplicitTail { count: 1 }, shell()),
        Err(WorkingPresetError::OmissionCountMismatch {
            expected: 0,
            actual: 1
        })
    ));
}

#[test]
fn malformed_and_resource_limited_projection_refuses_and_scene_errors_roll_back_whole_core() {
    assert!(matches!(
        project_scene_prefix(&[0x7a, 3, b'a'], PresetDocumentAssertion, limits()),
        Err(WorkingPresetError::Preset(
            tetrad_preset::Error::Truncated { .. }
        ))
    ));
    let bytes = document(&["paired"], &[0]);
    for bound in [
        Limits {
            max_bytes: bytes.len() - 1,
            ..limits()
        },
        Limits {
            max_fields: 1,
            ..limits()
        },
        Limits {
            max_depth: 0,
            ..limits()
        },
    ] {
        assert!(matches!(
            project_scene_prefix(&bytes, PresetDocumentAssertion, bound),
            Err(WorkingPresetError::Preset(
                tetrad_preset::Error::Limit { .. }
            ))
        ));
    }
    for units in [MAX_TEXT_UNITS, MAX_TEXT_UNITS + 2] {
        let label = "😀".repeat(units / 2);
        let bytes = document(&[&label], &[0]);
        let result = project_scene_prefix(&bytes, PresetDocumentAssertion, limits());
        if units == MAX_TEXT_UNITS {
            assert_eq!(result.unwrap().pairs()[0].label.units().len(), units);
        } else {
            assert!(matches!(
                result,
                Err(WorkingPresetError::Workflow(WorkflowError::ResourceLimit))
            ));
        }
    }
    for count in [MAX_SCENE_STORAGE_SLOTS, MAX_SCENE_STORAGE_SLOTS + 1] {
        let bytes = document(&vec!["x"; count], &vec![0; count]);
        let result = project_scene_prefix(&bytes, PresetDocumentAssertion, limits());
        if count == MAX_SCENE_STORAGE_SLOTS {
            let installed = result
                .unwrap()
                .install(OmissionAck::RequireNoOmissions, shell())
                .unwrap();
            assert_eq!(installed.core.state().working.entries().len(), count);
        } else {
            assert!(matches!(
                result,
                Err(WorkingPresetError::Workflow(WorkflowError::ResourceLimit))
            ));
        }
    }
    let mut installed = project_scene_prefix(&bytes, PresetDocumentAssertion, limits())
        .unwrap()
        .install(OmissionAck::RequireNoOmissions, shell())
        .unwrap();
    dispatch_application_scene(
        &mut installed.core,
        &envelope(&[8, 1, 24, 1]),
        SceneLimits::default(),
        Millis(10),
    )
    .unwrap();
    let before = installed.core.clone();
    assert_eq!(
        dispatch_application_scene(
            &mut installed.core,
            &envelope(&[8, 1, 24, 8]),
            SceneLimits::default(),
            Millis(11)
        ),
        Err(Error::Scene(8))
    );
    assert_eq!(installed.core, before);
    assert_eq!(
        dispatch_application_scene(
            &mut installed.core,
            &envelope(&[8, 1, 24, 0]),
            SceneLimits::default(),
            Millis(9)
        ),
        Err(Error::Workflow(WorkflowError::ClockRegression))
    );
    assert_eq!(installed.core, before);
    assert_eq!(installed.document().bytes(), bytes);
}

#[test]
fn scene_field_export_replaces_owned_library_bytes_without_exporting_tails_or_padding() {
    for (labels, colors, expected_labels, expected_colors) in [
        (
            vec!["original", "second", "unpaired label"],
            vec![7, 8],
            vec!["  音 😀\t  ", "", "unpaired label"],
            vec![0, 8],
        ),
        (
            vec!["original", "second"],
            vec![7, 8, 99],
            vec!["  音 😀\t  ", ""],
            vec![0, 8, 99],
        ),
    ] {
        let mut original = document(&labels, &colors);
        let mut expected = document(&expected_labels, &expected_colors);
        let mut unchanged = Vec::new();
        for field in [22, 23, 24] {
            blob(field, b"opaque-version-string", &mut unchanged);
        }
        uint(36, 0, &mut unchanged);
        uint(37, 1, &mut unchanged);
        // Unknown overlong tag/value and explicit defaults remain exact.
        unchanged.extend([0xa0, 0x86, 0, 0x80, 0]);
        original.extend_from_slice(&unchanged);
        expected.extend_from_slice(&unchanged);
        for metadata in [None, Some(Vec::new()), Some(vec![0xff, 0x80, 0, 7])] {
            let product = Product {
                id: "synthetic-export".into(),
                product_type: -914,
                read_only: false,
                data: ProductData {
                    raw_data: Some(original.clone()),
                    metadata,
                },
            };
            let mut library = Library::in_memory(tetrad_storage::Limits::default()).unwrap();
            library.apply(&[Mutation::InsertProduct(&product)]).unwrap();
            let row = library.product(&product.id).unwrap().unwrap();
            let mut installed = project_scene_prefix(
                row.data.raw_data.as_deref().unwrap(),
                PresetDocumentAssertion,
                limits(),
            )
            .unwrap()
            .install(OmissionAck::ExplicitTail { count: 1 }, shell())
            .unwrap();
            let retained = format!("{:?}", installed.document());
            let omissions = installed.omissions().to_vec();
            assert_eq!(installed.export_scene_fields(limits()).unwrap(), original);
            for action in [
                Action::SetSceneLabel {
                    index: 0,
                    label: UiText::from_literal("  音 😀\t  ").unwrap(),
                },
                Action::SetSceneLabel {
                    index: 1,
                    label: UiText::from_literal("").unwrap(),
                },
                Action::SetSceneColor {
                    index: 0,
                    raw_color: 0,
                },
            ] {
                installed.core.dispatch(action, Millis(20)).unwrap();
            }
            let core = installed.core.clone();
            let exported = installed.export_scene_fields(limits()).unwrap();
            assert_eq!(exported, expected); // Exact all-field bytes, not only a summary.
            let parsed = PresetDocument::parse(&exported, limits()).unwrap();
            assert_eq!(parsed.to_bytes(), exported);
            let summary = parsed.summary().unwrap();
            assert_eq!(summary.scenes[0].label.as_deref(), Some("  音 😀\t  "));
            assert_eq!(summary.scenes[1].label.as_deref(), Some(""));
            assert_eq!(summary.scenes[0].color, Some(0));
            assert_eq!(summary.scene_count, 6);
            assert_eq!(summary.scenes[5].label, None);
            assert_eq!(summary.scenes[5].color, None);
            assert_eq!(installed.core.state().working.entries().len(), 2);
            assert_eq!(library.product(&product.id).unwrap(), Some(product.clone()));
            let replacement = ProductData {
                raw_data: Some(exported),
                metadata: row.data.metadata.clone(),
            };
            library
                .apply(&[Mutation::ReplaceProductData {
                    id: &row.id,
                    data: &replacement,
                }])
                .unwrap();
            assert_eq!(
                library.product(&product.id).unwrap(),
                Some(Product {
                    data: replacement,
                    ..product
                })
            );
            assert_eq!(installed.core, core);
            assert_eq!(format!("{:?}", installed.document()), retained);
            assert_eq!(installed.document().bytes(), original);
            assert_eq!(installed.omissions(), omissions);
            assert_eq!(
                installed.admission(),
                CaptureAdmission::SceneLabelColorPrefixOnly
            );
            assert!(installed.core.state().working_parameters.is_none());
            assert!(installed.core.lifecycle().is_none());
            assert!(matches!(
                installed.core.save_stamp(),
                Err(LifecycleError::NotAttached)
            ));
        }
    }
}

#[test]
fn packed_colors_survive_noop_and_label_exports_but_any_color_change_refuses() {
    for mixed in [false, true] {
        let mut original = document(&["first", "second"], &[]);
        if mixed {
            uint(31, 1, &mut original);
            blob(31, &[0x82, 0], &mut original);
        } else {
            blob(31, &[0x81, 0, 0x82, 0], &mut original);
        }
        let mut installed = project_scene_prefix(&original, PresetDocumentAssertion, limits())
            .unwrap()
            .install(OmissionAck::RequireNoOmissions, shell())
            .unwrap();
        assert_eq!(installed.export_scene_fields(limits()).unwrap(), original);
        installed
            .core
            .dispatch(
                Action::SetSceneLabel {
                    index: 0,
                    label: UiText::from_literal("label-only 😀").unwrap(),
                },
                Millis(1),
            )
            .unwrap();
        let mut expected = document(&["label-only 😀", "second"], &[]);
        if mixed {
            uint(31, 1, &mut expected);
            blob(31, &[0x82, 0], &mut expected);
        } else {
            blob(31, &[0x81, 0, 0x82, 0], &mut expected);
        }
        let before = installed.core.clone();
        assert_eq!(installed.export_scene_fields(limits()).unwrap(), expected);
        assert_eq!(installed.core, before);
        for index in [0, 1] {
            installed
                .core
                .dispatch(
                    Action::SetSceneColor {
                        index,
                        raw_color: 0,
                    },
                    Millis(2),
                )
                .unwrap();
            let core = installed.core.clone();
            assert!(matches!(
                installed.export_scene_fields(limits()),
                Err(WorkingPresetError::Preset(
                    tetrad_preset::Error::InvalidKnownField {
                        field: 31,
                        reason: "packed scene colors are unsupported by the positional color editor",
                        ..
                    }
                ))
            ));
            assert_eq!(installed.core, core);
            assert_eq!(installed.document().bytes(), original);
            assert!(installed.omissions().is_empty());
            assert_eq!(
                installed.document().summary().scenes[0].label.as_deref(),
                Some("first")
            );
        }
    }
    let mut original = document(&["first"], &[]);
    original.extend([0xf8, 0x81, 0, 0x81, 0]);
    let installed = project_scene_prefix(&original, PresetDocumentAssertion, limits())
        .unwrap()
        .install(OmissionAck::RequireNoOmissions, shell())
        .unwrap();
    assert_eq!(installed.export_scene_fields(limits()).unwrap(), original);
    let empty = project_scene_prefix(&[], PresetDocumentAssertion, limits())
        .unwrap()
        .install(OmissionAck::RequireNoOmissions, shell())
        .unwrap();
    assert_eq!(
        empty.export_scene_fields(limits()).unwrap(),
        Vec::<u8>::new()
    );
}

#[test]
fn export_refusals_preserve_replaced_core_retained_observations_and_library() {
    let original = document(&["first", "second"], &[1, 2]);
    let product = Product {
        id: "synthetic-refusal".into(),
        product_type: 17,
        read_only: false,
        data: ProductData {
            raw_data: Some(original.clone()),
            metadata: None,
        },
    };
    let mut library = Library::in_memory(tetrad_storage::Limits::default()).unwrap();
    library.apply(&[Mutation::InsertProduct(&product)]).unwrap();
    let mut installed = project_scene_prefix(&original, PresetDocumentAssertion, limits())
        .unwrap()
        .install(OmissionAck::RequireNoOmissions, shell())
        .unwrap();
    let retained = format!("{:?}", installed.document());
    let entries = installed.core.state().working.entries().to_vec();
    // Public core replacement is not full-document admission.
    for (working, alternate, error_kind) in [
        (
            entries.clone(),
            Some(SceneStorage::new(entries.clone()).unwrap()),
            "alternate",
        ),
        (entries[..1].to_vec(), None, "count-short"),
        (
            [entries.clone(), vec![entries[0].clone()]].concat(),
            None,
            "count-long",
        ),
    ] {
        let caller = shell();
        installed.core = ApplicationCore::new(WorkflowInit {
            working: SceneStorage::new(working).unwrap(),
            alternate,
            baseline: caller.baseline,
            mode: caller.mode,
            previous_mode: caller.previous_mode,
            page: caller.page,
            ui_dirty: caller.ui_dirty,
            last_notified_dirty: caller.last_notified_dirty,
            position: caller.position,
            title: caller.title,
            requested: caller.requested,
            loaded: caller.loaded,
        });
        let core = installed.core.clone();
        let result = installed.export_scene_fields(limits());
        match error_kind {
            "alternate" => assert!(matches!(result, Err(WorkingPresetError::AlternateStorage))),
            "count-short" => assert!(matches!(
                result,
                Err(WorkingPresetError::WorkingSceneCountMismatch {
                    expected: 2,
                    actual: 1
                })
            )),
            _ => assert!(matches!(
                result,
                Err(WorkingPresetError::WorkingSceneCountMismatch {
                    expected: 2,
                    actual: 3
                })
            )),
        }
        assert_eq!(installed.core, core);
        assert_eq!(format!("{:?}", installed.document()), retained);
        assert!(installed.omissions().is_empty());
        assert_eq!(
            installed.admission(),
            CaptureAdmission::SceneLabelColorPrefixOnly
        );
        assert_eq!(library.product(&product.id).unwrap(), Some(product.clone()));
    }
    let mut installed = project_scene_prefix(&original, PresetDocumentAssertion, limits())
        .unwrap()
        .install(OmissionAck::RequireNoOmissions, shell())
        .unwrap();
    // Index 0 would edit successfully; conversion must check every label.
    installed
        .core
        .dispatch(
            Action::SetSceneLabel {
                index: 0,
                label: UiText::from_literal("changed").unwrap(),
            },
            Millis(1),
        )
        .unwrap();
    for surrogate in [0xd800, 0xdc00] {
        installed
            .core
            .dispatch(
                Action::SetSceneLabel {
                    index: 1,
                    label: UiText::from_units(vec![surrogate]).unwrap(),
                },
                Millis(2),
            )
            .unwrap();
        let core = installed.core.clone();
        assert!(matches!(
            installed.export_scene_fields(limits()),
            Err(WorkingPresetError::InvalidSceneLabel { index: 1 })
        ));
        assert_eq!(installed.core, core);
        assert_eq!(format!("{:?}", installed.document()), retained);
        assert_eq!(library.product(&product.id).unwrap(), Some(product.clone()));
    }
    installed
        .core
        .dispatch(
            Action::SetSceneLabel {
                index: 1,
                label: UiText::from_literal("second").unwrap(),
            },
            Millis(3),
        )
        .unwrap();
    for bound in [
        Limits {
            max_bytes: original.len() - 1,
            ..limits()
        },
        Limits {
            max_fields: 1,
            ..limits()
        },
        Limits {
            max_depth: 0,
            ..limits()
        },
        Limits {
            max_bytes: original.len(),
            ..limits()
        }, // Parse succeeds; label growth fails.
    ] {
        let core = installed.core.clone();
        assert!(matches!(
            installed.export_scene_fields(bound),
            Err(WorkingPresetError::Preset(
                tetrad_preset::Error::Limit { .. }
            ))
        ));
        assert_eq!(installed.core, core);
        assert_eq!(format!("{:?}", installed.document()), retained);
        assert_eq!(library.product(&product.id).unwrap(), Some(product.clone()));
    }
    // With an exact input budget, label succeeds privately before color growth fails.
    installed
        .core
        .dispatch(
            Action::SetSceneLabel {
                index: 0,
                label: UiText::from_literal("other").unwrap(),
            },
            Millis(4),
        )
        .unwrap();
    installed
        .core
        .dispatch(
            Action::SetSceneColor {
                index: 1,
                raw_color: 128,
            },
            Millis(4),
        )
        .unwrap();
    let core = installed.core.clone();
    assert!(matches!(
        installed.export_scene_fields(Limits {
            max_bytes: original.len(),
            ..limits()
        }),
        Err(WorkingPresetError::Preset(tetrad_preset::Error::Limit {
            kind: "bytes",
            ..
        }))
    ));
    assert_eq!(installed.core, core);
    assert_eq!(format!("{:?}", installed.document()), retained);
    assert_eq!(library.product(&product.id).unwrap(), Some(product));
    assert!(installed.core.state().working_parameters.is_none());
    assert!(installed.core.lifecycle().is_none());
}

#[test]
fn exported_bytes_keep_storage_permissions_missing_errors_and_batch_rollback_separate() {
    let original = document(&["first"], &[7]);
    let products = [false, true].map(|read_only| Product {
        id: if read_only {
            "synthetic-read-only"
        } else {
            "synthetic-writable"
        }
        .into(),
        product_type: -7,
        read_only,
        data: ProductData {
            raw_data: Some(original.clone()),
            metadata: Some(Vec::new()),
        },
    });
    let mut library = Library::in_memory(tetrad_storage::Limits::default()).unwrap();
    library
        .apply(
            &products
                .iter()
                .map(Mutation::InsertProduct)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let mut installed = project_scene_prefix(&original, PresetDocumentAssertion, limits())
        .unwrap()
        .install(OmissionAck::RequireNoOmissions, shell())
        .unwrap();
    installed
        .core
        .dispatch(
            Action::SetSceneColor {
                index: 0,
                raw_color: 0,
            },
            Millis(1),
        )
        .unwrap();
    let core = installed.core.clone();
    let replacement = ProductData {
        raw_data: Some(installed.export_scene_fields(limits()).unwrap()),
        metadata: library
            .product(&products[1].id)
            .unwrap()
            .unwrap()
            .data
            .metadata,
    };
    assert!(matches!(
        library.apply(&[Mutation::ReplaceProductData {
            id: &products[1].id,
            data: &replacement,
        }]),
        Err(tetrad_storage::Error::ReadOnly)
    ));
    assert!(matches!(library.apply(&[
        Mutation::ReplaceProductData { id: &products[0].id, data: &replacement },
        Mutation::ReplaceProductData { id: "synthetic-missing", data: &replacement },
    ]), Err(tetrad_storage::Error::ProductMissing(id)) if id == "synthetic-missing"));
    for product in &products {
        assert_eq!(
            library.product(&product.id).unwrap().as_ref(),
            Some(product)
        );
    }
    assert_eq!(installed.core, core);
    assert_eq!(installed.document().bytes(), original);
    assert!(matches!(
        installed.core.save_stamp(),
        Err(LifecycleError::NotAttached)
    ));
}

#[test]
fn independent_working_edits_never_change_retained_document_or_other_instance() {
    let mut bytes = document(&["original", "second"], &[0, u32::MAX]);
    let original = bytes.clone();
    let project = || {
        project_scene_prefix(&bytes, PresetDocumentAssertion, limits())
            .unwrap()
            .install(OmissionAck::RequireNoOmissions, shell())
            .unwrap()
    };
    let mut first = project();
    let second = project();
    let second_core = second.core.clone();
    bytes.fill(0); // The capture owns bytes; it does not borrow caller storage.
    for action in [
        Action::SetSceneLabel {
            index: 0,
            label: UiText::from_literal("working edit").unwrap(),
        },
        Action::SetSceneColor {
            index: 0,
            raw_color: 0xff00_0012,
        },
    ] {
        let effects = first.core.dispatch(action, Millis(20)).unwrap();
        assert!(
            effects
                .iter()
                .all(|effect| matches!(effect.adapter_support(), AdapterSupport::Unsupported(_)))
        );
    }
    assert_eq!(
        first.core.state().working.entries()[0].label,
        UiText::from_literal("working edit").unwrap()
    );
    assert_eq!(
        first.core.state().working.entries()[0].raw_color,
        0xff00_0012
    );
    assert!(first.core.state().dirty_deadline.is_some());
    assert_eq!(
        first.document().summary().scenes[0].label.as_deref(),
        Some("original")
    );
    assert_eq!(first.document().summary().scenes[0].color, Some(0));
    assert_eq!(first.document().bytes(), original);
    assert_eq!(second.document().bytes(), original);
    assert_eq!(second.core, second_core);
    assert!(second.core.state().dirty_deadline.is_none());
    assert!(first.core.lifecycle().is_none());
    assert!(first.core.state().working_parameters.is_none());
    assert!(matches!(
        first.core.save_stamp(),
        Err(LifecycleError::NotAttached)
    ));
}

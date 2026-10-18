use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "tetrad-app-test-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("test directory: {error}"),
            }
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(arguments: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tetrad"))
        .args(arguments)
        .output()
        .unwrap()
}

fn json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn fixture(path: &Path) -> Vec<u8> {
    let bytes = vec![
        0x12, 4, b'T', b'e', b's', b't', 0x7a, 1, b'A', 0x7a, 1, b'B', 0xf8, 1, 1, 0xf8, 1, 2,
        0xa0, 6, 7,
    ];
    std::fs::write(path, &bytes).unwrap();
    bytes
}

#[test]
fn renders_fixed_frame_and_never_overwrites_output_or_source() {
    let dir = TestDir::new();
    let preset = dir.path("source.pb");
    let bytes = fixture(&preset);
    let frame = dir.path("frame.ppm");
    let args = [
        "render".as_ref(),
        "--preset".as_ref(),
        preset.as_os_str(),
        "--scene".as_ref(),
        "2".as_ref(),
        "--output".as_ref(),
        frame.as_os_str(),
    ];
    let result = json(&run(&args));
    assert_eq!(result["width"], 800);
    assert_eq!(result["height"], 480);
    assert_eq!(result["dsp_loaded"], false);
    let rendered = std::fs::read(&frame).unwrap();
    let header = b"P6\n800 480\n255\n";
    assert!(rendered.starts_with(header));
    assert_eq!(rendered.len(), header.len() + 800 * 480 * 3);
    assert!(!run(&args).status.success());
    assert_eq!(std::fs::read(&frame).unwrap(), rendered);
    assert!(
        !run(&["render".as_ref(), "--output".as_ref(), preset.as_os_str()])
            .status
            .success()
    );
    let alias = dir.path("source-alias.pb");
    std::fs::hard_link(&preset, &alias).unwrap();
    assert!(
        !run(&["render".as_ref(), "--output".as_ref(), alias.as_os_str()])
            .status
            .success()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        assert_eq!(
            std::fs::metadata(&frame).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let link = dir.path("source-link.pb");
        symlink(&preset, &link).unwrap();
        assert!(
            !run(&["render".as_ref(), "--output".as_ref(), link.as_os_str()])
                .status
                .success()
        );
    }
    assert_eq!(std::fs::read(&preset).unwrap(), bytes);
}

#[test]
fn scene_dialog_export_preserves_unknowns_and_refuses_before_creating_output() {
    let dir = TestDir::new();
    let input = dir.path("source.pb");
    let original = fixture(&input);
    let output = dir.path("edited.pb");
    let result = json(&run(&[
        "scene-edit".as_ref(),
        "--input".as_ref(),
        input.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
        "--scene".as_ref(),
        "2".as_ref(),
        "--label".as_ref(),
        "Bridge".as_ref(),
        "--color".as_ref(),
        "3".as_ref(),
    ]));
    assert_eq!(result["changed"], true);
    let edited = std::fs::read(&output).unwrap();
    assert!(edited.ends_with(&[0xa0, 6, 7]));
    let summary = tetrad_preset::PresetDocument::parse(&edited, Default::default())
        .unwrap()
        .summary()
        .unwrap();
    assert_eq!(summary.scenes[1].label.as_deref(), Some("Bridge"));
    assert_eq!(summary.scenes[1].color, Some(3));
    let refused = dir.path("refused.pb");
    assert!(
        !run(&[
            "scene-edit".as_ref(),
            "--input".as_ref(),
            input.as_os_str(),
            "--output".as_ref(),
            refused.as_os_str(),
            "--scene".as_ref(),
            "8".as_ref(),
            "--label".as_ref(),
            "Absent".as_ref()
        ])
        .status
        .success()
    );
    assert!(!refused.exists());
    assert!(
        !run(&[
            "scene-edit".as_ref(),
            "--input".as_ref(),
            input.as_os_str(),
            "--output".as_ref(),
            refused.as_os_str(),
            "--scene".as_ref(),
            "2".as_ref(),
            "--label".as_ref(),
            "B".as_ref(),
            "--color".as_ref(),
            "0".as_ref()
        ])
        .status
        .success()
    );
    assert!(!refused.exists());
    assert_eq!(std::fs::read(&input).unwrap(), original);
}

#[test]
fn color_only_edit_keeps_unsimplified_label_bytes() {
    let dir = TestDir::new();
    let input = dir.path("spaced.pb");
    let output = dir.path("color.pb");
    let original = [0x7a, 4, b'A', b' ', b' ', b'B', 0xf8, 1, 1, 0xa0, 6, 7];
    std::fs::write(&input, original).unwrap();
    let result = json(&run(&[
        "scene-edit".as_ref(),
        "--input".as_ref(),
        input.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
        "--scene".as_ref(),
        "1".as_ref(),
        "--color".as_ref(),
        "5".as_ref(),
    ]));
    assert_eq!(result["changed"], true);
    let mut expected = original;
    expected[8] = 5;
    assert_eq!(std::fs::read(output).unwrap(), expected);
    assert_eq!(std::fs::read(&input).unwrap(), original);
    let refused = dir.path("no-edit.pb");
    let invocation = run(&[
        "scene-edit".as_ref(),
        "--input".as_ref(),
        input.as_os_str(),
        "--output".as_ref(),
        refused.as_os_str(),
        "--scene".as_ref(),
        "1".as_ref(),
    ]);
    assert_eq!(invocation.status.code(), Some(2));
    assert!(!refused.exists());
}

#[cfg(unix)]
#[test]
fn output_json_accepts_native_non_utf8_filenames_for_every_export() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let dir = TestDir::new();
    let input = dir.path("source.pb");
    fixture(&input);
    let frame = dir.0.join(OsString::from_vec(b"frame\xff.ppm".to_vec()));
    let rendered = json(&run(&[
        "render".as_ref(),
        "--output".as_ref(),
        frame.as_os_str(),
    ]));
    assert!(rendered["output"].as_str().unwrap().contains('\u{fffd}'));
    assert!(
        std::fs::read(frame)
            .unwrap()
            .starts_with(b"P6\n800 480\n255\n")
    );
    let edited = dir.0.join(OsString::from_vec(b"edited\xfe.pb".to_vec()));
    json(&run(&[
        "scene-edit".as_ref(),
        "--input".as_ref(),
        input.as_os_str(),
        "--output".as_ref(),
        edited.as_os_str(),
        "--scene".as_ref(),
        "1".as_ref(),
        "--label".as_ref(),
        "Changed".as_ref(),
    ]));
    assert!(std::fs::read(edited).unwrap().ends_with(&[0xa0, 6, 7]));
    let record = dir.path("record.f32");
    let bytes: Vec<_> = [0.25_f32, -0.25]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    std::fs::write(&record, &bytes).unwrap();
    let tape = dir.0.join(OsString::from_vec(b"tape\xfd.f32".to_vec()));
    json(&run(&[
        "loop".as_ref(),
        "--record".as_ref(),
        record.as_os_str(),
        "--output".as_ref(),
        tape.as_os_str(),
    ]));
    assert_eq!(std::fs::read(tape).unwrap(), bytes);
}

#[test]
fn library_scene_save_uses_real_owned_storage_and_preserves_product_facts() {
    use tetrad_storage::{Folder, FolderId, Library, Limits, Mutation, Product, ProductData};
    let dir = TestDir::new();
    let database = dir.path("owned.sqlite");
    let original = fixture(&dir.path("source.pb"));
    let mut storage = Library::create_owned(&database, Limits::default()).unwrap();
    let product = Product {
        id: "preset".into(),
        product_type: 42,
        read_only: false,
        data: ProductData {
            raw_data: Some(original.clone()),
            metadata: Some(vec![0, 255, 1]),
        },
    };
    let folder = Folder {
        id: FolderId::try_from(1).unwrap(),
        product_type: 42,
        folder_type: 7,
        name: "Owned".into(),
        parent: None,
        read_only: false,
    };
    storage
        .apply(&[
            Mutation::InsertProduct(&product),
            Mutation::CreateFolder(&folder),
            Mutation::Link {
                folder: folder.id,
                product: &product.id,
            },
        ])
        .unwrap();
    drop(storage);
    let args = [
        "library-scene-edit".as_ref(),
        "--database".as_ref(),
        database.as_os_str(),
        "--id".as_ref(),
        "preset".as_ref(),
        "--preset-type".as_ref(),
        "42".as_ref(),
        "--scene".as_ref(),
        "2".as_ref(),
        "--label".as_ref(),
        "Bridge".as_ref(),
    ];
    assert_eq!(json(&run(&args))["changed"], true);
    assert_eq!(json(&run(&args))["changed"], false);
    let mut reopened = Library::open_owned(&database, Limits::default(), false).unwrap();
    let saved = reopened.product("preset").unwrap().unwrap();
    assert_eq!(saved.data.metadata, product.data.metadata);
    assert_eq!(saved.product_type, product.product_type);
    assert_eq!(saved.read_only, product.read_only);
    assert_eq!(reopened.folder_products(folder.id, 10).unwrap(), ["preset"]);
    let bytes = saved.data.raw_data.as_ref().unwrap();
    assert!(bytes.ends_with(&[0xa0, 6, 7]));
    let summary = tetrad_preset::PresetDocument::parse(bytes, Default::default())
        .unwrap()
        .summary()
        .unwrap();
    assert_eq!(summary.scenes[1].label.as_deref(), Some("Bridge"));
    let readonly = Product {
        id: "readonly".into(),
        read_only: true,
        ..product
    };
    reopened
        .apply(&[Mutation::InsertProduct(&readonly)])
        .unwrap();
    drop(reopened);
    assert!(
        !run(&[
            "library-scene-edit".as_ref(),
            "--database".as_ref(),
            database.as_os_str(),
            "--id".as_ref(),
            "readonly".as_ref(),
            "--preset-type".as_ref(),
            "42".as_ref(),
            "--scene".as_ref(),
            "1".as_ref(),
            "--label".as_ref(),
            "Refused".as_ref()
        ])
        .status
        .success()
    );
    let reopened = Library::open_owned(&database, Limits::default(), true).unwrap();
    assert_eq!(reopened.product("readonly").unwrap().unwrap(), readonly);
    assert_eq!(reopened.product("preset").unwrap().unwrap(), saved);
}

fn grid_fixture() -> Vec<u8> {
    fn varint(mut value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        while value >= 128 {
            bytes.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        bytes.push(value as u8);
        bytes
    }
    fn scalar(field: u32, value: u64) -> Vec<u8> {
        [varint(u64::from(field) << 3), varint(value)].concat()
    }
    fn message(field: u32, payload: &[u8]) -> Vec<u8> {
        [
            varint((u64::from(field) << 3) | 2),
            varint(payload.len() as u64),
            payload.to_vec(),
        ]
        .concat()
    }
    let mut preset = message(2, b"Owned grid");
    for row in 0..4 {
        let mut chain = [scalar(1, 10 + row), scalar(2, 20 + row), scalar(14, row)].concat();
        for column in 0..8 {
            let hash = if row == 0 && column == 0 { 7 } else { 0 };
            chain.extend(message(5, &[scalar(1, hash), scalar(5, column)].concat()));
        }
        if row % 2 == 0 {
            // Signed int32 -1 uses the canonical ten-byte varint.
            chain.extend(message(
                6,
                &[scalar(1, u64::MAX), scalar(2, u64::MAX)].concat(),
            ));
            chain.extend(message(7, &scalar(1, 30)));
            chain.extend(message(8, &scalar(1, 40)));
        }
        chain.extend(message(9, &scalar(1, 20)));
        chain.extend(message(13, &scalar(1, 10)));
        preset.extend(message(11, &chain));
        let mut bypass = scalar(2, row);
        for column in 0..8 {
            let mut col = [scalar(2, column), scalar(3, 0)].concat();
            for scene in 0..8 {
                col.extend(message(
                    1,
                    &scalar(1, u64::from(row == 0 && column == 0 && scene == 1)),
                ));
            }
            bypass.extend(message(1, &col));
        }
        preset.extend(message(18, &bypass));
    }
    preset
}

#[test]
fn explicit_grid_projection_composes_wire_scene_ui_and_cli_without_loading_dsp() {
    use tetrad_app::session::OfflineSession;
    use tetrad_engine::{AbsentScalars, BypassPolarity, ImportPolicy};
    use tetrad_ui::SceneIndex;
    let bytes = grid_fixture();
    let policy = ImportPolicy {
        absent: AbsentScalars::Proto3Default,
        bypass: BypassPolarity::StoredTrueBypasses,
    };
    for (scene, bypassed) in [(0, false), (1, true)] {
        let session = OfflineSession::from_bytes(&bytes, SceneIndex::new(scene).unwrap()).unwrap();
        assert!(
            session
                .snapshot()
                .unwrap()
                .grid
                .iter()
                .all(|cell| cell.label.is_empty())
        );
        let view = session.grid_snapshot(policy).unwrap();
        assert_eq!(view.grid[0].label, "MODEL 7");
        assert_eq!(view.grid[0].bypassed, bypassed);
        assert!(view.grid[1..].iter().all(|cell| cell.label.is_empty()));
        assert_eq!(
            view.status,
            "OWNED GRID / 0 DEFAULTS / 12 DETACHED / NO DSP"
        );
        assert_eq!(view.preset_id, None);
        assert!(!view.dirty);
        assert!(session.grid_snapshot(ImportPolicy::STRICT).is_err());
        assert_eq!(session.export(), bytes);
    }
    let dir = TestDir::new();
    let input = dir.path("grid.pb");
    std::fs::write(&input, &bytes).unwrap();
    let output = dir.path("grid.ppm");
    let result = json(&run(&[
        "render".as_ref(),
        "--preset".as_ref(),
        input.as_os_str(),
        "--scene".as_ref(),
        "2".as_ref(),
        "--grid-policy".as_ref(),
        "owned-defaults".as_ref(),
        "--output".as_ref(),
        output.as_os_str(),
    ]));
    assert_eq!(result["grid_projected"], true);
    assert_eq!(result["dsp_loaded"], false);
    assert_eq!(std::fs::read(output).unwrap().len(), 1_152_015);
    let refused = dir.path("strict.ppm");
    let result = run(&[
        "render".as_ref(),
        "--preset".as_ref(),
        input.as_os_str(),
        "--grid-policy".as_ref(),
        "strict".as_ref(),
        "--output".as_ref(),
        refused.as_os_str(),
    ]);
    assert_eq!(result.status.code(), Some(1));
    assert!(!refused.exists());
    let missing_input = run(&[
        "render".as_ref(),
        "--grid-policy".as_ref(),
        "owned-defaults".as_ref(),
        "--output".as_ref(),
        refused.as_os_str(),
    ]);
    assert_eq!(missing_input.status.code(), Some(2));
    assert!(!refused.exists());
    assert_eq!(std::fs::read(input).unwrap(), bytes);
}

#[test]
fn replay_cli_connects_pointer_dialog_backend_and_frame_without_writing_source() {
    use tetrad_app::session::OfflineSession;
    use tetrad_ui::{Control, InputEvent, SceneIndex, UiState};
    let dir = TestDir::new();
    let input = dir.path("source.pb");
    let bytes = fixture(&input);
    let session = OfflineSession::from_bytes(&bytes, SceneIndex::new(0).unwrap()).unwrap();
    let mut ui = UiState::new();
    ui.set_snapshot(session.snapshot().unwrap()).unwrap();
    let mut events = Vec::new();
    // The trace taps what a mirror UI shows; the mirror follows each tap.
    let click = |ui: &mut UiState, events: &mut Vec<serde_json::Value>, control: Control| {
        let bounds = ui
            .hit_regions()
            .into_iter()
            .find(|r| r.control == control && r.enabled)
            .unwrap()
            .bounds;
        let x = bounds.x + bounds.width as i32 / 2;
        let y = bounds.y + bounds.height as i32 / 2;
        events.push(serde_json::json!({"event":"pointer_down", "x":x, "y":y}));
        events.push(serde_json::json!({"event":"pointer_up", "x":x, "y":y}));
        ui.handle_input(InputEvent::Activate(control));
    };
    let scene = SceneIndex::new(1).unwrap();
    click(&mut ui, &mut events, Control::SceneMenu);
    click(&mut ui, &mut events, Control::SelectScene(scene));
    let mut selected = ui.snapshot().clone();
    selected.selected_scene = scene;
    ui.set_snapshot(selected).unwrap();
    // Stock edits scene names from Gig View (EDIT SCENE): a swipe up from the Grid bottom edge.
    let swipe = [
        InputEvent::PointerDown { x: 400, y: 470 },
        InputEvent::PointerMove { x: 400, y: 300 },
        InputEvent::PointerUp { x: 400, y: 300 },
    ];
    for event in swipe {
        let (kind, x, y) = match event {
            InputEvent::PointerDown { x, y } => ("pointer_down", x, y),
            InputEvent::PointerMove { x, y } => ("pointer_move", x, y),
            InputEvent::PointerUp { x, y } => ("pointer_up", x, y),
            _ => unreachable!(),
        };
        events.push(serde_json::json!({"event":kind, "x":x, "y":y}));
        ui.handle_input(event);
    }
    assert_eq!(ui.screen(), tetrad_ui::Screen::Gig);
    click(&mut ui, &mut events, Control::EditScenes);
    events.extend([
        serde_json::json!({"event":"text", "value":"Bridge"}),
        serde_json::json!({"event":"color", "value":3}),
        serde_json::json!({"event":"activate", "control":{"name":"confirm_scenes"}}),
        serde_json::json!({"event":"quit"}),
    ]);
    let trace = dir.path("edit.jsonl");
    std::fs::write(
        &trace,
        events
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let output = dir.path("replayed.ppm");
    let result = json(&run(&[
        "replay".as_ref(),
        "--preset".as_ref(),
        input.as_os_str(),
        "--trace".as_ref(),
        trace.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
    ]));
    assert_eq!(result["events"], 13);
    assert_eq!(result["stop"], "quit");
    assert_eq!(result["changed"], true);
    assert_eq!(result["selected_scene"], 2);
    assert_eq!(result["selected_label"], "Bridge");
    assert_eq!(result["dsp_loaded"], false);
    assert_eq!(result["source_written"], false);
    assert_eq!(std::fs::read(output).unwrap().len(), 1_152_015);
    assert_eq!(std::fs::read(input).unwrap(), bytes);
}

#[test]
fn replay_exports_terminal_pointer_cancellation_and_refuses_bad_or_limited_traces() {
    let dir = TestDir::new();
    let input = dir.path("source.pb");
    let bytes = fixture(&input);
    let trace = dir.path("input.jsonl");
    let output = dir.path("released.ppm");
    let baseline = dir.path("baseline.ppm");
    json(&run(&[
        "render".as_ref(),
        "--preset".as_ref(),
        input.as_os_str(),
        "--output".as_ref(),
        baseline.as_os_str(),
    ]));
    std::fs::write(&trace, b"{\"event\":\"pointer_down\",\"x\":600,\"y\":80}\n").unwrap();
    let result = json(&run(&[
        "replay".as_ref(),
        "--preset".as_ref(),
        input.as_os_str(),
        "--trace".as_ref(),
        trace.as_os_str(),
        "--max-events".as_ref(),
        "1".as_ref(),
        "--output".as_ref(),
        output.as_os_str(),
    ]));
    assert_eq!(result["stop"], "input_exhausted");
    assert_eq!(
        std::fs::read(output).unwrap(),
        std::fs::read(baseline).unwrap()
    );
    let refused = dir.path("refused.ppm");
    for text in [
        "{\"event\":\"quit\",\"unexpected\":1}",
        "{\"event\":\"activate\",\"control\":{\"name\":\"cycle_mode\"}}",
    ] {
        std::fs::write(&trace, text).unwrap();
        assert_eq!(
            run(&[
                "replay".as_ref(),
                "--preset".as_ref(),
                input.as_os_str(),
                "--trace".as_ref(),
                trace.as_os_str(),
                "--output".as_ref(),
                refused.as_os_str(),
            ])
            .status
            .code(),
            Some(1)
        );
        assert!(!refused.exists());
    }
    std::fs::write(&trace, b"{\"event\":\"back\"}\n{\"event\":\"quit\"}\n").unwrap();
    assert_eq!(
        run(&[
            "replay".as_ref(),
            "--preset".as_ref(),
            input.as_os_str(),
            "--trace".as_ref(),
            trace.as_os_str(),
            "--max-events".as_ref(),
            "1".as_ref(),
            "--output".as_ref(),
            refused.as_os_str(),
        ])
        .status
        .code(),
        Some(1)
    );
    assert!(!refused.exists());
    assert_eq!(std::fs::read(input).unwrap(), bytes);
}

#[test]
fn replay_accepts_exact_event_limits_and_counts_only_through_quit() {
    let dir = TestDir::new();
    let input = dir.path("source.pb");
    let bytes = fixture(&input);
    let trace = dir.path("input.jsonl");
    for count in [1, 2, tetrad_app::replay::MAX_TRACE_EVENTS] {
        std::fs::write(&trace, "{\"event\":\"back\"}\n".repeat(count)).unwrap();
        let output = dir.path(&format!("exact-{count}.ppm"));
        let count_arg = count.to_string();
        let mut args = vec![
            "replay".as_ref(),
            "--preset".as_ref(),
            input.as_os_str(),
            "--trace".as_ref(),
            trace.as_os_str(),
            "--output".as_ref(),
            output.as_os_str(),
        ];
        if count != tetrad_app::replay::MAX_TRACE_EVENTS {
            args.push("--max-events".as_ref());
            args.push(count_arg.as_ref());
        }
        let result = json(&run(&args));
        assert_eq!(result["events"], count);
        assert_eq!(result["stop"], "input_exhausted");
        assert_eq!(result["changed"], false);
        assert_eq!(std::fs::read(output).unwrap().len(), 1_152_015);
    }
    std::fs::write(&trace, b"{\"event\":\"quit\"}\n{\"event\":\"back\"}\n").unwrap();
    let output = dir.path("quit.ppm");
    let result = json(&run(&[
        "replay".as_ref(),
        "--preset".as_ref(),
        input.as_os_str(),
        "--trace".as_ref(),
        trace.as_os_str(),
        "--max-events".as_ref(),
        "1".as_ref(),
        "--output".as_ref(),
        output.as_os_str(),
    ]));
    assert_eq!(result["events"], 1);
    assert_eq!(result["stop"], "quit");
    assert_eq!(std::fs::read(input).unwrap(), bytes);
}

#[test]
fn library_replay_drives_pointer_selection_and_local_edits_without_database_writes() {
    use tetrad_storage::{Library, Limits, Mutation, Product, ProductData};
    use tetrad_ui::{Control, PresetItem, SceneIndex, Screen, UiState};
    let dir = TestDir::new();
    let bytes = fixture(&dir.path("source.pb"));
    let database = dir.path("owned.sqlite");
    let products = ["first", "second"].map(|id| Product {
        id: id.into(),
        product_type: 42,
        read_only: false,
        data: ProductData {
            raw_data: Some(bytes.clone()),
            metadata: Some(vec![0, 255, 9]),
        },
    });
    let mut storage = Library::create_owned(&database, Limits::default()).unwrap();
    storage
        .apply(&[
            Mutation::InsertProduct(&products[0]),
            Mutation::InsertProduct(&products[1]),
        ])
        .unwrap();
    drop(storage);
    let original_database = std::fs::read(&database).unwrap();
    let bindings = dir.path("bindings.json");
    std::fs::write(
        &bindings,
        br#"[{"ui_handle":11,"product_id":"first","title":"First"},{"ui_handle":22,"product_id":"second","title":"Second"}]"#,
    )
    .unwrap();
    let mut snapshot =
        tetrad_app::session::OfflineSession::from_bytes(&bytes, SceneIndex::new(0).unwrap())
            .unwrap()
            .snapshot()
            .unwrap();
    snapshot.presets = vec![
        PresetItem {
            id: 11,
            title: "First".into(),
        },
        PresetItem {
            id: 22,
            title: "Second".into(),
        },
    ];
    let mut ui = UiState::new();
    ui.set_snapshot(snapshot).unwrap();
    ui.handle_input(tetrad_ui::InputEvent::Activate(Control::Navigate(
        Screen::Presets,
    )));
    let bounds = ui
        .hit_regions()
        .into_iter()
        .find(|region| region.control == Control::SelectPreset(22) && region.enabled)
        .unwrap()
        .bounds;
    let x = bounds.x + bounds.width as i32 / 2;
    let y = bounds.y + bounds.height as i32 / 2;
    let events = [
        serde_json::json!({"event":"activate","control":{"name":"navigate","screen":"presets"}}),
        serde_json::json!({"event":"pointer_down","x":x,"y":y}),
        serde_json::json!({"event":"pointer_up","x":x,"y":y}),
        // The Directory has its own header (manual §5), so leave it before navigating.
        serde_json::json!({"event":"back"}),
        // Gig View opens with the stock swipe up from the Grid bottom (it has no header button).
        serde_json::json!({"event":"pointer_down","x":400,"y":470}),
        serde_json::json!({"event":"pointer_move","x":400,"y":300}),
        serde_json::json!({"event":"pointer_up","x":400,"y":300}),
        serde_json::json!({"event":"activate","control":{"name":"select_scene","scene":2}}),
        serde_json::json!({"event":"activate","control":{"name":"edit_scenes"}}),
        serde_json::json!({"event":"text","value":"Bridge"}),
        serde_json::json!({"event":"activate","control":{"name":"confirm_scenes"}}),
        serde_json::json!({"event":"quit"}),
    ];
    let trace = dir.path("events.jsonl");
    std::fs::write(
        &trace,
        events
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let output = dir.path("library.ppm");
    let args = [
        "library-replay".as_ref(),
        "--database".as_ref(),
        database.as_os_str(),
        "--bindings".as_ref(),
        bindings.as_os_str(),
        "--initial-handle".as_ref(),
        "11".as_ref(),
        "--preset-type".as_ref(),
        "42".as_ref(),
        "--trace".as_ref(),
        trace.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
    ];
    let result = json(&run(&args));
    assert_eq!(result["events"], 12);
    assert_eq!(result["selected_handle"], 22);
    assert_eq!(result["verified_product_id"], "second");
    assert_eq!(result["selected_scene"], 2);
    assert_eq!(result["selected_label"], "Bridge");
    assert_eq!(result["changed"], true);
    assert_eq!(result["preset_id"], serde_json::Value::Null);
    assert_eq!(result["dsp_loaded"], false);
    assert_eq!(result["database_written"], false);
    assert_eq!(std::fs::read(&output).unwrap().len(), 1_152_015);
    assert!(!run(&args).status.success());
    let reopened = Library::open_owned(&database, Limits::default(), true).unwrap();
    for product in products {
        assert_eq!(reopened.product(&product.id).unwrap().unwrap(), product);
    }
    assert_eq!(std::fs::read(&database).unwrap(), original_database);
}

#[test]
fn library_replay_refuses_bad_bindings_limits_and_dirty_switches_before_output() {
    use tetrad_storage::{Library, Limits, Mutation, Product, ProductData};
    let dir = TestDir::new();
    let bytes = fixture(&dir.path("source.pb"));
    let database = dir.path("owned.sqlite");
    let products = ["first", "second"].map(|id| Product {
        id: id.into(),
        product_type: 42,
        read_only: false,
        data: ProductData {
            raw_data: Some(bytes.clone()),
            metadata: None,
        },
    });
    let mut storage = Library::create_owned(&database, Limits::default()).unwrap();
    storage
        .apply(&[
            Mutation::InsertProduct(&products[0]),
            Mutation::InsertProduct(&products[1]),
        ])
        .unwrap();
    drop(storage);
    let original_database = std::fs::read(&database).unwrap();
    let bindings = dir.path("bindings.json");
    let trace = dir.path("events.jsonl");
    let output = dir.path("refused.ppm");
    std::fs::write(&trace, b"{\"event\":\"quit\"}\n").unwrap();
    let args = [
        "library-replay".as_ref(),
        "--database".as_ref(),
        database.as_os_str(),
        "--bindings".as_ref(),
        bindings.as_os_str(),
        "--initial-handle".as_ref(),
        "11".as_ref(),
        "--preset-type".as_ref(),
        "42".as_ref(),
        "--trace".as_ref(),
        trace.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
    ];
    for manifest in [
        br#"[{"ui_handle":11,"ui_handle":22,"product_id":"first","title":"First"}]"#.as_slice(),
        br#"[{"ui_handle":11,"product_id":"first","title":"First","extra":1}]"#,
        br#"[{"ui_handle":1.5,"product_id":"first","title":"First"}]"#,
    ] {
        std::fs::write(&bindings, manifest).unwrap();
        let result = run(&args);
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("binding manifest"));
        assert!(!output.exists());
    }
    std::fs::write(&bindings, vec![b' '; 1024 * 1024 + 1]).unwrap();
    assert_eq!(run(&args).status.code(), Some(1));
    assert!(!output.exists());
    let valid = br#"[{"ui_handle":11,"product_id":"first","title":"First"},{"ui_handle":22,"product_id":"second","title":"Second"}]"#;
    std::fs::write(&bindings, valid).unwrap();
    std::fs::write(
        &trace,
        concat!(
            // Gig View (swipe up from the Grid bottom) → EDIT SCENE, then back to the Grid.
            "{\"event\":\"pointer_down\",\"x\":400,\"y\":470}\n",
            "{\"event\":\"pointer_move\",\"x\":400,\"y\":300}\n",
            "{\"event\":\"pointer_up\",\"x\":400,\"y\":300}\n",
            "{\"event\":\"activate\",\"control\":{\"name\":\"edit_scenes\"}}\n",
            "{\"event\":\"text\",\"value\":\"Local edit\"}\n",
            "{\"event\":\"activate\",\"control\":{\"name\":\"confirm_scenes\"}}\n",
            "{\"event\":\"back\"}\n",
            "{\"event\":\"activate\",\"control\":{\"name\":\"navigate\",\"screen\":\"presets\"}}\n",
            "{\"event\":\"activate\",\"control\":{\"name\":\"select_preset\",\"id\":22}}\n"
        ),
    )
    .unwrap();
    let result = run(&args);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("discard local wire edits"));
    assert!(!output.exists());
    std::fs::write(&trace, b"{\"event\":\"back\"}\n{\"event\":\"quit\"}\n").unwrap();
    let mut limited = args.to_vec();
    limited.push("--max-events".as_ref());
    limited.push("1".as_ref());
    let result = run(&limited);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("exceeds its 1-event limit"));
    assert!(!output.exists());
    assert_eq!(std::fs::read(&database).unwrap(), original_database);
}

#[test]
fn owned_audio_commands_detect_a4_and_export_exact_stereo_overdub() {
    let dir = TestDir::new();
    let tone = dir.path("tone.f32");
    let samples: Vec<u8> = (0..4096)
        .flat_map(|index| {
            (0.5 * (std::f32::consts::TAU * 440.0 * index as f32 / 48_000.0).sin()).to_le_bytes()
        })
        .collect();
    std::fs::write(&tone, &samples).unwrap();
    let tuning = json(&run(&[
        "tune".as_ref(),
        "--input".as_ref(),
        tone.as_os_str(),
        "--sample-rate".as_ref(),
        "48000".as_ref(),
    ]));
    assert_eq!(tuning["result"]["note"], "A4");
    assert!(tuning["result"]["cents"].as_f64().unwrap().abs() < 1.0);
    let record = dir.path("record.f32");
    let overdub = dir.path("overdub.f32");
    let output = dir.path("loop.f32");
    let tape = [0.5_f32, -0.5, 0.25, -0.25];
    let added = [0.125_f32, 0.125, 0.0625, 0.0625];
    std::fs::write(
        &record,
        tape.into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    std::fs::write(
        &overdub,
        added
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let looping = json(&run(&[
        "loop".as_ref(),
        "--record".as_ref(),
        record.as_os_str(),
        "--overdub".as_ref(),
        overdub.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
    ]));
    assert_eq!(looping["frames"], 2);
    let expected: Vec<_> = tape
        .into_iter()
        .zip(added)
        .flat_map(|(a, b)| (a + b).to_le_bytes())
        .collect();
    assert_eq!(std::fs::read(output).unwrap(), expected);
    assert_eq!(std::fs::read(tone).unwrap(), samples);
}

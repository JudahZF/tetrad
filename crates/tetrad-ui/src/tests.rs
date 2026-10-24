use std::io::{self, Write};

use crate::*;
use tetrad_assets::Palette;

fn center(ui: &UiState, control: Control) -> (i32, i32) {
    let bounds = ui
        .hit_regions()
        .into_iter()
        .find(|region| region.control == control)
        .unwrap_or_else(|| panic!("{control:?} is not visible"))
        .bounds;
    bounds.center()
}

pub(crate) fn activate(ui: &mut UiState, control: Control) -> Vec<UiCommand> {
    // The ⋮ menu shows seven rows (stock 4.1.0): scroll it, as a user does, until the item shows.
    for _ in 0..3 {
        let shown = (ui.hit_regions().iter()).any(|region| region.control == control);
        let menu = Some(crate::state::Overlay::Menu(crate::state::MenuKind::Main));
        if shown || !matches!(control, Control::Menu(_)) || ui.overlay != menu {
            break;
        }
        ui.handle_input(InputEvent::PointerDown { x: 600, y: 400 });
        ui.handle_input(InputEvent::PointerMove { x: 600, y: 160 });
        ui.handle_input(InputEvent::PointerUp { x: 600, y: 160 });
    }
    ui.handle_input(InputEvent::Activate(control))
}

fn tap(ui: &mut UiState, x: i32, y: i32) -> Vec<UiCommand> {
    ui.handle_input(InputEvent::PointerDown { x, y });
    ui.handle_input(InputEvent::PointerUp { x, y })
}

fn drag(ui: &mut UiState, from: (i32, i32), to: (i32, i32)) -> Vec<UiCommand> {
    ui.handle_input(InputEvent::PointerDown {
        x: from.0,
        y: from.1,
    });
    let mut commands = ui.handle_input(InputEvent::PointerMove { x: to.0, y: to.1 });
    commands.extend(ui.handle_input(InputEvent::PointerUp { x: to.0, y: to.1 }));
    commands
}

/// The stock way into Gig View: a swipe up from the Grid bottom edge (manual §2).
fn swipe_up_to_gig(ui: &mut UiState) {
    drag(ui, (400, 470), (400, 300));
    assert_eq!(ui.screen(), Screen::Gig);
}

/// Opens the scene editor as stock does: Gig View → EDIT SCENE on the playing scene (the Grid
/// scene dropdown has no pencil, `st5b.png`).
fn edit_scenes(ui: &mut UiState) -> Vec<UiCommand> {
    if ui.screen() != Screen::Gig {
        swipe_up_to_gig(ui);
    }
    activate(ui, Control::EditScenes)
}

fn scene(index: u8) -> SceneIndex {
    SceneIndex::new(index).unwrap()
}

pub(crate) fn cell(row: u8, column: u8) -> GridPosition {
    GridPosition::new(row, column).unwrap()
}

/// Small synthetic projection with blocks, parameters, settings and lists.
pub(crate) fn rich() -> UiSnapshot {
    let mut s = UiSnapshot {
        preset_id: Some(7),
        preset_title: "SYNTHETIC".into(),
        preset_file: true,
        presets: (0..10)
            .map(|id| PresetItem {
                id,
                title: format!("PRESET {id}"),
            })
            .collect(),
        models: vec![
            ModelEntry {
                id: 1001,
                name: "AMP ONE".into(),
                category: ModelCategory::GuitarAmp,
            },
            ModelEntry {
                id: 6002,
                name: "ECHO TWO".into(),
                category: ModelCategory::Delay,
            },
        ]
        .into(),
        // More than one page of KNOB_COUNT (10) cells.
        parameters: (0..12)
            .map(|index| ParameterView {
                entry: None,
                scene: false,
                index: index * 2,
                label: format!("P{index}"),
                value: index.to_string(),
                kind: if index == 3 {
                    ParameterKind::Toggle { on: false }
                } else {
                    ParameterKind::Continuous { position: 0.5 }
                },
            })
            .collect(),
        ..UiSnapshot::default()
    };
    s.grid[0] = GridCell {
        label: "AMP ONE".into(),
        bypassed: false,
    };
    s.blocks[0].category = Some(ModelCategory::GuitarAmp);
    s.stomps[1].label = "ECHO".into();
    s.settings
        .insert(SettingId::MidiThru, SettingValue::Bool(false));
    s.settings
        .insert(SettingId::MidiClockOut, SettingValue::Choice(0));
    s.settings
        .insert(SettingId::TunerReference, SettingValue::Number(480));
    s.wifi.networks.push(WifiNetwork {
        ssid: "LOCKED".into(),
        signal: 4,
        secured: true,
        ..WifiNetwork::default()
    });
    s
}

fn ui_with(snapshot: UiSnapshot) -> UiState {
    let mut ui = UiState::new();
    ui.set_snapshot(snapshot).unwrap();
    ui
}

#[test]
fn pointer_capture_requires_matching_release_and_application_acknowledgement() {
    let mut ui = UiState::new();
    activate(&mut ui, Control::SceneMenu);
    let target = Control::SelectScene(scene(3));
    let (x, y) = center(&ui, target);
    let baseline = ui.render();
    assert!(ui.handle_input(InputEvent::PointerUp { x, y }).is_empty());
    ui.handle_input(InputEvent::PointerDown { x, y });
    assert_ne!(ui.render(), baseline);
    ui.handle_input(InputEvent::PointerMove { x: -1, y });
    assert_eq!(ui.render(), baseline);
    assert!(
        ui.handle_input(InputEvent::PointerUp { x: -1, y })
            .is_empty()
    );
    assert_eq!(
        tap(&mut ui, x, y),
        vec![UiCommand::SelectScene { scene: scene(3) }]
    );
    assert_eq!(ui.snapshot().selected_scene.get(), 0);
    // A pick closes the dropdown, as stock (seen on the QC, 2026-10-16).
    assert!(!ui.scene_menu);
    let mut snapshot = ui.snapshot().clone();
    snapshot.selected_scene = scene(3);
    ui.set_snapshot(snapshot).unwrap();
    assert_ne!(ui.render(), baseline);
    // A touch survives a snapshot that keeps its target, and is cancelled
    // when the target goes away (bank preview disables the scene strip).
    activate(&mut ui, Control::SceneMenu);
    ui.handle_input(InputEvent::PointerDown { x, y });
    ui.set_snapshot(ui.snapshot().clone()).unwrap();
    assert_eq!(
        ui.handle_input(InputEvent::PointerUp { x, y }),
        vec![UiCommand::SelectScene { scene: scene(3) }]
    );
    activate(&mut ui, Control::SceneMenu);
    ui.handle_input(InputEvent::PointerDown { x, y });
    let mut snapshot = ui.snapshot().clone();
    snapshot.bank_preview_active = true;
    ui.set_snapshot(snapshot).unwrap();
    assert!(ui.handle_input(InputEvent::PointerUp { x, y }).is_empty());
}

/// Review R1: live data (tuner, beat, meters) must not drop a touch.
#[test]
fn live_updates_keep_knob_drags_and_long_presses() {
    let mut ui = ui_with(UiSnapshot {
        selected_block: Some(cell(0, 0)),
        ..rich()
    });
    let set = |value| UiCommand::SetPosition {
        position: cell(0, 0),
        parameter: 0,
        value,
    };
    let (x, y) = center(&ui, Control::Param(ParamControl::Knob(KnobIndex(0))));
    ui.handle_input(InputEvent::PointerDown { x, y });
    assert_eq!(
        ui.handle_input(InputEvent::PointerMove { x, y: y - 17 }),
        vec![set(5850)]
    );
    let mut snapshot = ui.snapshot().clone();
    snapshot.tuner.cents += 1.0;
    snapshot.tempo.beat = Some(2);
    ui.set_snapshot(snapshot).unwrap();
    let mut live = ui.snapshot().live();
    live.input_levels = [0.5, 0.25];
    ui.update_live(live).unwrap();
    assert_eq!(
        ui.handle_input(InputEvent::PointerMove { x, y: y - 40 }),
        vec![set(7000)]
    );
    // A new selection moves the knobs to another block: the drag ends.
    let mut snapshot = ui.snapshot().clone();
    snapshot.grid[1].label = "OTHER".into();
    snapshot.selected_block = Some(cell(0, 1));
    ui.set_snapshot(snapshot).unwrap();
    assert!(
        ui.handle_input(InputEvent::PointerMove { x, y: y - 80 })
            .is_empty()
    );
    ui.handle_input(InputEvent::PointerUp { x, y: y - 80 });

    let mut ui = ui_with(rich());
    let (x, y) = center(&ui, Control::SelectCell(cell(0, 0)));
    ui.handle_input(InputEvent::PointerDown { x, y });
    let mut live = ui.snapshot().live();
    live.tempo.beat = Some(1);
    ui.update_live(live).unwrap();
    // A 750 ms hold (`LONG_PRESS_MS`); the test held 600 ms over the old 500 ms.
    ui.handle_input(InputEvent::Tick { now_ms: 750 });
    assert!(ui.overlay_open(), "long press survives a beat update");
    live.tuner.cents = f32::NAN;
    assert_eq!(ui.update_live(live), Err(UiError::InvalidNumber));
}

/// Every screen, menu and dialog: controls stay inside the frame, never
/// overlap within the interactive layer, keep a usable touch size and hit
/// at their half-open corners.
#[test]
fn every_screen_has_disjoint_in_frame_touch_targets() {
    let mut snapshot = rich();
    snapshot.selected_block = Some(cell(0, 0));
    let base = ui_with(snapshot);
    let open = |setup: &dyn Fn(&mut UiState)| {
        let mut ui = base.clone();
        setup(&mut ui);
        ui
    };
    let mut states = vec![base.clone()];
    for screen in [
        Screen::Gig,
        Screen::Presets,
        Screen::Tuner,
        Screen::Tempo,
        Screen::Looper,
        Screen::Capture,
        Screen::Io(IoTab::Input1),
        Screen::Io(IoTab::Expression),
        Screen::Settings(SettingsPage::Connection),
        Screen::Settings(SettingsPage::Updates),
        Screen::Settings(SettingsPage::Backups),
        Screen::Settings(SettingsPage::Midi),
        Screen::Settings(SettingsPage::DeviceInformation),
        Screen::Settings(SettingsPage::Brightness),
        Screen::Settings(SettingsPage::GlobalBypass),
        Screen::Settings(SettingsPage::SceneBypass),
        Screen::Settings(SettingsPage::FactoryReset),
        Screen::Io(IoTab::Usb),
        Screen::Io(IoTab::Return),
        Screen::GlobalEq(EqTab::Out),
        Screen::MidiOut,
        Screen::Recovery,
        Screen::ModelPicker,
    ] {
        states.push(open(&|ui| {
            ui.navigate(screen);
        }));
    }
    states.push(open(&|ui| {
        activate(ui, Control::SelectCell(cell(0, 0)));
        activate(ui, Control::Picker(PickerControl::Presets(1001)));
    }));
    states.push(open(&|ui| {
        ui.open_expression(cell(0, 0));
    }));
    states.push(open(&|ui| {
        activate(ui, Control::OpenMainMenu);
    }));
    // Extensions list, an extension's page, Repositories and the trust question.
    let extensions = |ui: &mut UiState, control: Option<ExtensionControl>, trust: bool| {
        let mut extensions = crate::screens::extensions::tests::sample();
        extensions.status = Some("Refreshing".into());
        extensions.busy = true;
        extensions.pending_trust = trust.then(|| PendingTrust {
            repository: "night-owl/tools".into(),
            fingerprint: "3f2a 9c01".into(),
        });
        ui.navigate(Screen::Settings(SettingsPage::Extensions));
        let snapshot = UiSnapshot {
            extensions,
            ..ui.snapshot().clone()
        };
        ui.set_snapshot(snapshot).expect("valid snapshot");
        if let Some(control) = control {
            activate(ui, Control::Extension(control));
        }
    };
    states.push(open(&|ui| extensions(ui, None, false)));
    states.push(open(&|ui| {
        extensions(ui, Some(ExtensionControl::Open(0)), false)
    }));
    states.push(open(&|ui| {
        extensions(ui, Some(ExtensionControl::Sources), true)
    }));
    // Mappings list, a detail page with its stepper and LEARN, and the action menu levels.
    let mappings = |ui: &mut UiState, controls: &[MappingControl]| {
        let snapshot = UiSnapshot {
            mappings: crate::screens::mappings::tests::sample(),
            ..ui.snapshot().clone()
        };
        ui.set_snapshot(snapshot).expect("valid snapshot");
        ui.navigate(Screen::Mappings);
        for control in controls {
            activate(ui, Control::Mapping(*control));
        }
    };
    states.push(open(&|ui| mappings(ui, &[])));
    states.push(open(&|ui| mappings(ui, &[MappingControl::Open(2)])));
    states.push(open(&|ui| mappings(ui, &[MappingControl::Open(3)])));
    states.push(open(&|ui| {
        mappings(ui, &[MappingControl::Open(1), MappingControl::Actions])
    }));
    states.push(open(&|ui| {
        let preset = MappingControl::Pick(1);
        mappings(
            ui,
            &[MappingControl::Open(1), MappingControl::Actions, preset],
        )
    }));
    states.push(open(&|ui| {
        activate(ui, Control::SceneMenu);
    }));
    states.push(open(&|ui| {
        // The docked block editor covers the Grid bottom edge: open by footswitch.
        ui.show(Screen::Gig);
        activate(ui, Control::EditScenes);
    }));
    states.push(open(&|ui| {
        activate(ui, Control::OpenMainMenu);
        activate(ui, Control::Menu(MenuAction::SaveAs));
    }));
    states.push(open(&|ui| {
        activate(
            ui,
            Control::Port {
                row: Row(1),
                output: false,
            },
        );
    }));
    for ui in &states {
        let regions: Vec<_> = ui.hit_regions().into_iter().filter(|r| r.enabled).collect();
        assert!(!regions.is_empty(), "{:?}", ui.stack());
        for (index, region) in regions.iter().enumerate() {
            let Rect {
                x,
                y,
                width,
                height,
            } = region.bounds;
            assert!(
                x >= 0 && y >= 0 && region.bounds.right() <= 800 && region.bounds.bottom() <= 480,
                "{region:?}"
            );
            assert_eq!(ui.hit_test(x, y), Some(region.control), "{region:?}");
            assert_eq!(
                ui.hit_test(x + width as i32 - 1, y + height as i32 - 1),
                Some(region.control)
            );
            for other in &regions[index + 1..] {
                assert!(
                    region.bounds.intersect(other.bounds).is_none(),
                    "{region:?} overlaps {other:?} on {:?}",
                    ui.stack()
                );
            }
        }
        // Review R10: every enabled control is at least 40 px each way
        // before a scroll viewport clips it.
        let layout = ui.layout();
        for item in &layout.items[layout.layer..] {
            if item.enabled && item.control.is_some() && item.visible().is_some() {
                let Rect { width, height, .. } = item.bounds;
                assert!(
                    width.min(height) >= 40,
                    "{:?} {width}x{height} '{}' on {:?}",
                    item.control,
                    item.label,
                    ui.stack()
                );
            }
        }
        assert_eq!(ui.render().rgb().len(), 800 * 480 * 3);
    }
    for point in [(-1, 0), (0, -1), (800, 0), (0, 480), (i32::MAX, i32::MIN)] {
        assert_eq!(base.hit_test(point.0, point.1), None);
    }
}

/// Recovered stock labels and order: main menu (`0x3badd4`) and settings
/// menu (`0x791ac`).
#[test]
fn menus_use_stock_labels_in_stock_order() {
    let mut ui = ui_with(rich());
    activate(&mut ui, Control::OpenMainMenu);
    // The list scrolls (seven rows show): collect the labels across three scrolls.
    let mut labels: Vec<String> = Vec::new();
    for _ in 0..4 {
        for region in ui.hit_regions() {
            if matches!(region.control, Control::Menu(_)) && !labels.contains(&region.label) {
                labels.push(region.label);
            }
        }
        drag(&mut ui, (600, 400), (600, 160));
    }
    // Stock 4.1.0 has no I/O Settings or Looper X row (seen on the QC, 2026-10-15). Tetrad
    // shows Mappings in place of Preset MIDI Out.
    assert_eq!(
        labels,
        [
            "Create New",
            "Save as...",
            "Edit Details",
            "Copy Scene A",
            "Swap Scene A",
            "Mappings",
            // The runtime label (`0x003bb9a0`, `0xa35818`), not the populate default.
            "Add to favorites",
            "Delete Preset",
            // Stock 4.1.0 QUAD CORTEX section (seen on the QC, 2026-10-15).
            "New Neural Capture",
            "Modes Configuration",
            "Tempo",
            "CPU Monitor",
            "Settings",
        ]
    );
    activate(&mut ui, Control::Menu(MenuAction::Settings));
    let pages: Vec<String> = ui
        .hit_regions()
        .into_iter()
        .filter(|r| matches!(r.control, Control::Setting(SettingControl::Page(_))))
        .map(|r| r.label)
        .collect();
    assert_eq!(pages, ["My Account", "Backups"]);
    // Manual §10: the dropdown switches groups; each opens on its first page.
    activate(&mut ui, Control::Setting(SettingControl::Groups));
    activate(&mut ui, Control::Choose(1));
    assert_eq!(ui.screen(), Screen::Settings(SettingsPage::Connection));
    let pages: Vec<String> = ui
        .hit_regions()
        .into_iter()
        .filter(|r| matches!(r.control, Control::Setting(SettingControl::Page(_))))
        .map(|r| r.label)
        .collect();
    // Owned: the stock "Updates" page names CorOS, and Tetrad Updates follows it.
    assert_eq!(
        pages[..3],
        ["Connection", "CorOS Updates", "Tetrad Updates"]
    );
}

/// Manual §4: a tap on the Looper X block opens its full-screen view (the block is selected,
/// so Params opens its editor), as stock; the ⋮ menu has no Looper X row.
#[test]
fn a_looper_block_tap_opens_looper_x() {
    let mut snapshot = rich();
    snapshot.grid[1] = GridCell {
        label: "LOOPER X".into(),
        bypassed: false,
    };
    snapshot.blocks[1].category = Some(ModelCategory::Looper);
    let mut ui = ui_with(snapshot);
    let commands = activate(&mut ui, Control::SelectCell(cell(0, 1)));
    assert_eq!(
        commands.first(),
        Some(&UiCommand::InspectCell {
            position: cell(0, 1)
        })
    );
    assert_eq!(ui.screen(), Screen::Looper);
}

/// Tetrad: Mappings takes the place of stock Preset MIDI Out in the ⋮ menu.
#[test]
fn main_menu_mappings_opens_the_mappings_page() {
    let mut ui = ui_with(rich());
    activate(&mut ui, Control::OpenMainMenu);
    let regions = ui.hit_regions();
    let item =
        (regions.iter()).find(|region| region.control == Control::Menu(MenuAction::Mappings));
    assert_eq!(item.map(|region| region.label.as_str()), Some("Mappings"));
    activate(&mut ui, Control::Menu(MenuAction::Mappings));
    assert_eq!(ui.screen(), Screen::Mappings);
    assert!(!ui.overlay_open());
}

#[test]
fn navigation_stack_back_order_and_tuner_visibility_requests() {
    let mut ui = UiState::new();
    swipe_up_to_gig(&mut ui);
    // Gig View has no main bar (manual §3); the Directory opens over it from the app.
    ui.navigate(Screen::Presets);
    assert_eq!(ui.stack(), [Screen::Grid, Screen::Gig, Screen::Presets]);
    ui.handle_input(InputEvent::Back);
    assert_eq!(ui.screen(), Screen::Gig);
    // The tuner opens over Gig View from its footswitch shortcut.
    assert_eq!(
        ui.show(Screen::Tuner),
        vec![UiCommand::ShowTuner { show: true }]
    );
    assert_eq!(
        ui.handle_input(InputEvent::Back),
        vec![UiCommand::ShowTuner { show: false }]
    );
    ui.handle_input(InputEvent::Back);
    assert_eq!(ui.stack(), [Screen::Grid]);
    assert!(ui.handle_input(InputEvent::Back).is_empty());
    activate(&mut ui, Control::OpenMainMenu);
    activate(&mut ui, Control::Menu(MenuAction::Settings));
    activate(&mut ui, Control::Setting(SettingControl::Groups));
    activate(&mut ui, Control::Choose(1));
    activate(
        &mut ui,
        Control::Setting(SettingControl::Page(SettingsPage::Updates)),
    );
    assert_eq!(
        ui.stack(),
        [Screen::Grid, Screen::Settings(SettingsPage::Updates)]
    );
    // A selected block closes before the stack pops.
    let mut ui = ui_with(UiSnapshot {
        selected_block: Some(cell(0, 0)),
        ..rich()
    });
    assert_eq!(
        ui.handle_input(InputEvent::Back),
        vec![UiCommand::ClearBlockSelection]
    );
}

#[test]
fn scene_draft_is_modal_and_close_discards_edits() {
    let mut ui = UiState::new();
    let original = ui.snapshot().clone();
    edit_scenes(&mut ui);
    assert!(ui.scene_dialog_open());
    assert!(activate(&mut ui, Control::Navigate(Screen::Presets)).is_empty());
    assert_eq!(
        ui.screen(),
        Screen::Gig,
        "the dialog keeps Gig View under it"
    );
    let region = ui.hit_regions()[0].bounds;
    assert!(ui.hit_test(region.x - 1, region.y - 1).is_none());
    ui.handle_input(InputEvent::TextChanged("  NEW\u{00a0}NAME  ".into()));
    ui.handle_input(InputEvent::DraftColor(0xfeed));
    assert_eq!(ui.snapshot(), &original);
    assert!(activate(&mut ui, Control::CloseDialog).is_empty());
    assert!(!ui.scene_dialog_open());
    edit_scenes(&mut ui);
    assert_eq!(activate(&mut ui, Control::ConfirmScenes).len(), 8);
    assert_eq!(ui.snapshot(), &original);
}

#[test]
fn scene_confirmation_emits_all_labels_then_each_nonzero_color_in_scene_order() {
    let mut snapshot = UiSnapshot::default();
    snapshot.scenes[1].raw_color = 0x12345678;
    snapshot.scenes[2].label = "KEEP literal".into();
    let mut ui = ui_with(snapshot.clone());
    edit_scenes(&mut ui);
    ui.handle_input(InputEvent::TextChanged(
        "\u{180e} Verse\u{2009}\u{3000}é\u{0085}x  ".into(),
    ));
    ui.handle_input(InputEvent::DraftColor(0x87654321));
    let commands = activate(&mut ui, Control::ConfirmScenes);
    assert_eq!(commands.len(), 10);
    assert_eq!(
        commands[..5],
        [
            UiCommand::SetSceneLabel {
                scene: scene(0),
                label: "Verse é\u{0085}x".into()
            },
            UiCommand::SetSceneColor {
                scene: scene(0),
                raw_color: 0x87654321
            },
            UiCommand::SetSceneLabel {
                scene: scene(1),
                label: String::new()
            },
            UiCommand::SetSceneColor {
                scene: scene(1),
                raw_color: 0x12345678
            },
            UiCommand::SetSceneLabel {
                scene: scene(2),
                label: "KEEP literal".into()
            },
        ]
    );
    assert_eq!(ui.snapshot(), &snapshot);
    assert!(!ui.scene_dialog_open());
}

#[test]
fn scene_confirmation_is_disabled_during_bank_preview_and_stale_drafts_are_rejected() {
    let mut ui = UiState::new();
    edit_scenes(&mut ui);
    ui.handle_input(InputEvent::TextChanged("Pending".into()));
    let mut snapshot = ui.snapshot().clone();
    snapshot.bank_preview_active = true;
    ui.set_snapshot(snapshot.clone()).unwrap();
    assert!(activate(&mut ui, Control::ConfirmScenes).is_empty());
    assert!(ui.scene_dialog_open());
    snapshot.bank_preview_active = false;
    ui.set_snapshot(snapshot.clone()).unwrap();
    assert!(matches!(
        activate(&mut ui, Control::ConfirmScenes).first(),
        Some(UiCommand::SetSceneLabel { label, .. }) if label == "Pending"
    ));
    edit_scenes(&mut ui);
    snapshot.scenes[0].label = "EXTERNAL CHANGE".into();
    ui.set_snapshot(snapshot).unwrap();
    assert_eq!(
        activate(&mut ui, Control::ConfirmScenes),
        vec![UiCommand::Rejected(UiError::StaleSceneDraft)]
    );
}

#[test]
fn preset_and_bank_commands_do_not_conflate_request_preview_and_loaded_state() {
    let mut snapshot = UiSnapshot {
        presets: vec![PresetItem {
            id: 42,
            title: "OWNED".into(),
        }],
        ..UiSnapshot::default()
    };
    let mut ui = ui_with(snapshot.clone());
    activate(&mut ui, Control::Navigate(Screen::Presets));
    assert_eq!(
        activate(&mut ui, Control::SelectPreset(42)),
        vec![UiCommand::RequestPreset { id: 42 }]
    );
    // The Directory has no touch preview (manual §5); the footswitch overlay drives it.
    assert!(activate(&mut ui, Control::BeginBankPreview).is_empty());
    snapshot.bank_preview_active = true;
    ui.set_snapshot(snapshot.clone()).unwrap();
    assert!(activate(&mut ui, Control::BeginBankPreview).is_empty());
    assert_eq!(
        activate(&mut ui, Control::SelectPreset(42)),
        vec![UiCommand::PreviewPreset { id: 42 }]
    );
    // The preview banner belongs to the Grid header.
    ui.navigate(Screen::Grid);
    assert_eq!(
        activate(&mut ui, Control::EndBankPreview),
        vec![UiCommand::EndBankPreview]
    );
    assert_eq!(ui.snapshot(), &snapshot);
    assert_eq!(
        ui.handle_input(InputEvent::Back),
        vec![UiCommand::CancelBankPreview]
    );
    swipe_up_to_gig(&mut ui);
    assert!(activate(&mut ui, Control::SelectScene(scene(1))).is_empty());
    assert!(activate(&mut ui, Control::EditScenes).is_empty());
}

#[test]
fn preset_banks_use_opaque_ids_clamp_after_shrink_and_search_filters_locally() {
    let mut snapshot = UiSnapshot {
        presets: (0..17)
            .map(|index| PresetItem {
                id: 100 + index * 19,
                title: format!("PRESET {index}"),
            })
            .collect(),
        ..UiSnapshot::default()
    };
    let mut ui = ui_with(snapshot.clone());
    activate(&mut ui, Control::Navigate(Screen::Presets));
    assert!(activate(&mut ui, Control::SelectPreset(252)).is_empty());
    activate(&mut ui, Control::Library(LibraryControl::Bank(1)));
    assert_eq!(
        activate(&mut ui, Control::SelectPreset(252)),
        vec![UiCommand::RequestPreset { id: 252 }]
    );
    activate(&mut ui, Control::Library(LibraryControl::Search));
    for key in "16".chars() {
        activate(&mut ui, Control::Key(Key::Layer(KeyLayer::Digits)));
        assert!(activate(&mut ui, Control::Key(Key::Char(key))).is_empty());
    }
    assert!(activate(&mut ui, Control::Key(Key::Done)).is_empty());
    let shown: Vec<Control> = ui.hit_regions().into_iter().map(|r| r.control).collect();
    assert!(shown.contains(&Control::SelectPreset(404)));
    assert!(!shown.contains(&Control::SelectPreset(100)));
    snapshot.presets.truncate(1);
    ui.set_snapshot(snapshot).unwrap();
    activate(&mut ui, Control::Library(LibraryControl::ClearSearch));
    assert_eq!(
        activate(&mut ui, Control::SelectPreset(100)),
        vec![UiCommand::RequestPreset { id: 100 }]
    );
}

/// A media parameter knob opens the IR picker on the folder of the current
/// item, lists only IR folders, edits nothing on a turn, and a tap commits the
/// exact stored text (factory selector form) and returns to the grid.
#[test]
fn media_picker_commits_the_stored_text_of_the_tapped_item() {
    let mut snapshot = rich();
    snapshot.selected_block = Some(cell(0, 0));
    snapshot.parameters[0] = ParameterView {
        entry: None,
        scene: false,
        index: 1,
        label: "MIC A".into(),
        value: "Mine".into(),
        kind: ParameterKind::Media(MediaKind::Ir),
    };
    let entry = |folder, name: &str, value: &str| MediaEntry {
        folder,
        name: name.into(),
        value: value.into(),
    };
    snapshot.media = vec![
        entry(
            MediaFolder::FactoryIrs,
            "TAG Cab One Mic Two",
            "TAG_Cab One_Mic Two",
        ),
        entry(MediaFolder::MyIrs, "Mine", "CIR_1a2b"),
        entry(MediaFolder::MyCaptures, "Amp", "ab"),
    ]
    .into();
    let mut ui = ui_with(snapshot);
    let knob = KnobIndex::new(0).unwrap();
    assert!(
        ui.handle_input(InputEvent::Knob { knob, delta: 1 })
            .is_empty()
    );
    assert!(activate(&mut ui, Control::Param(ParamControl::Knob(knob))).is_empty());
    assert_eq!(ui.screen(), Screen::MediaPicker);
    let shown: Vec<Control> = ui.hit_regions().into_iter().map(|r| r.control).collect();
    assert!(shown.contains(&Control::Media(MediaControl::Item(1))));
    assert!(!shown.contains(&Control::Media(MediaControl::Item(0))));
    assert!(!shown.contains(&Control::Media(MediaControl::Folder(
        MediaFolder::MyCaptures
    ))));

    activate(
        &mut ui,
        Control::Media(MediaControl::Folder(MediaFolder::FactoryIrs)),
    );
    let (x, y) = center(&ui, Control::Media(MediaControl::Item(0)));
    assert_eq!(
        tap(&mut ui, x, y),
        vec![UiCommand::SelectText {
            position: cell(0, 0),
            parameter: 1,
            text: "TAG_Cab One_Mic Two".into(),
        }]
    );
    assert_eq!(ui.screen(), Screen::Grid);
}

/// The media picker is the Directory filtered to the media type: SORT, SEARCH and MULTI
/// SELECT are the Directory controls over the picker rows, and Back closes them first.
#[test]
fn media_picker_sorts_searches_and_checks_rows_as_the_directory() {
    let mut snapshot = rich();
    snapshot.selected_block = Some(cell(0, 0));
    snapshot.parameters[0] = ParameterView {
        entry: None,
        scene: false,
        index: 1,
        label: "MIC A".into(),
        value: "Mine".into(),
        kind: ParameterKind::Media(MediaKind::Ir),
    };
    let entry = |name: &str, value: &str| MediaEntry {
        folder: MediaFolder::MyIrs,
        name: name.into(),
        value: value.into(),
    };
    snapshot.media = vec![entry("Mine", "CIR_1"), entry("Air", "CIR_2")].into();
    let mut ui = ui_with(snapshot);
    activate(
        &mut ui,
        Control::Param(ParamControl::Knob(KnobIndex::new(0).unwrap())),
    );
    assert_eq!(ui.screen(), Screen::MediaPicker);
    let rows = |ui: &UiState| {
        ui.hit_regions()
            .into_iter()
            .filter_map(|r| match r.control {
                Control::Media(MediaControl::Item(i)) => Some(i),
                Control::Library(LibraryControl::Check(i)) => Some(i as u16),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(rows(&ui), [1, 0]);
    // SORT ▸ Name twice: descending.
    for _ in 0..2 {
        activate(&mut ui, Control::Library(LibraryControl::Sort));
        activate(&mut ui, Control::Choose(0));
    }
    assert_eq!(rows(&ui), [0, 1]);
    activate(&mut ui, Control::Library(LibraryControl::Search));
    activate(&mut ui, Control::Key(Key::Char('a')));
    activate(&mut ui, Control::Key(Key::Done));
    assert_eq!(rows(&ui), [1]);
    activate(&mut ui, Control::Library(LibraryControl::MultiSelect));
    activate(&mut ui, Control::Library(LibraryControl::Check(1)));
    assert!(
        ui.hit_regions()
            .iter()
            .any(|r| r.control == Control::Library(LibraryControl::Check(1)))
    );
    // Back closes Multi Select, then the search, then the picker.
    activate(&mut ui, Control::Back);
    activate(&mut ui, Control::Back);
    assert_eq!((ui.screen(), rows(&ui)), (Screen::MediaPicker, vec![0, 1]));
    activate(&mut ui, Control::Back);
    assert_ne!(ui.screen(), Screen::MediaPicker);
}

#[test]
fn grid_touch_opens_picker_selects_blocks_and_drags_move_or_delete() {
    let mut snapshot = rich();
    // Stock `slotReleasedSlot` (`0x59a9c0`): an empty slot of a row without input does
    // nothing (shot sn-1); with an input it opens the device list.
    let mut ui = ui_with(snapshot.clone());
    activate(&mut ui, Control::SelectCell(cell(1, 3)));
    assert_eq!(ui.screen(), Screen::Grid, "row 2 has no input");
    snapshot.rows[1].input = InputPort::Input1;
    let mut ui = ui_with(snapshot);
    // Empty slot: picker in add mode; choosing returns to the grid.
    activate(&mut ui, Control::SelectCell(cell(1, 3)));
    assert_eq!(ui.screen(), Screen::ModelPicker);
    activate(
        &mut ui,
        Control::Picker(PickerControl::Group(DeviceGroup::Amp)),
    );
    assert!(
        ui.hit_regions()
            .iter()
            .all(|r| r.control != Control::Picker(PickerControl::Model(6002)))
    );
    assert_eq!(
        activate(&mut ui, Control::Picker(PickerControl::Model(1001))),
        vec![UiCommand::AddBlock {
            position: cell(1, 3),
            model_id: 1001
        }]
    );
    assert_eq!(ui.screen(), Screen::Grid);
    // Occupied: inspect request, then replace once the app selects it.
    assert_eq!(
        activate(&mut ui, Control::SelectCell(cell(0, 0))),
        vec![UiCommand::InspectCell {
            position: cell(0, 0)
        }]
    );
    let from = center(&ui, Control::SelectCell(cell(0, 0)));
    let to = center(&ui, Control::SelectCell(cell(2, 5)));
    // The half under the finger (`SlotItem` `0x5a6fe4`: split at width / 2, left first).
    for (dx, side) in [
        (-30, DropSide::Left),
        (0, DropSide::Left),
        (1, DropSide::Right),
    ] {
        assert_eq!(
            drag(&mut ui, from, (to.0 + dx, to.1)),
            vec![UiCommand::MoveBlock {
                from: cell(0, 0),
                to: cell(2, 5),
                side
            }],
            "dx {dx}"
        );
    }
    assert_eq!(
        drag(&mut ui, from, (400, 74)),
        vec![UiCommand::RemoveBlock {
            position: cell(0, 0)
        }]
    );
    // Dragging an empty slot is not a move.
    let empty = center(&ui, Control::SelectCell(cell(3, 0)));
    assert!(drag(&mut ui, empty, to).is_empty());
    // Split/mix routing of the row pair; an absent point (stock −1) starts at column 1.
    activate(
        &mut ui,
        Control::Port {
            row: Row(1),
            output: false,
        },
    );
    activate(&mut ui, Control::Menu(MenuAction::Routing));
    assert_eq!(
        activate(&mut ui, Control::Routing(RoutingControl::Split(1))),
        vec![UiCommand::SetSplit {
            pair: RowPair::Upper,
            column: Some(0)
        }]
    );
}

#[test]
fn long_press_and_swipe_need_the_application_clock_and_suppress_the_tap() {
    let mut ui = ui_with(rich());
    let (x, y) = center(&ui, Control::SelectCell(cell(0, 0)));
    ui.handle_input(InputEvent::Tick { now_ms: 1_000 });
    ui.handle_input(InputEvent::PointerDown { x, y });
    // The hold is 750 ms (`LONG_PRESS_MS`); the test used 500 ms before the stock value.
    ui.handle_input(InputEvent::Tick { now_ms: 1_749 });
    assert!(!ui.overlay_open());
    ui.handle_input(InputEvent::Tick { now_ms: 1_750 });
    assert!(ui.overlay_open());
    assert!(ui.handle_input(InputEvent::PointerUp { x, y }).is_empty());
    assert_eq!(
        activate(&mut ui, Control::Menu(MenuAction::ToggleBypass)),
        vec![UiCommand::SetBlockBypass {
            position: cell(0, 0),
            bypassed: true
        }]
    );
    assert!(drag(&mut ui, (200, 20), (200, 140)).is_empty());
    assert_eq!(ui.screen(), Screen::Io(IoTab::Input1));
}

#[test]
fn scroll_lists_drag_without_activating_and_clamp_to_content() {
    let mut ui = UiState::new();
    activate(&mut ui, Control::OpenMainMenu);
    activate(&mut ui, Control::Menu(MenuAction::Settings));
    activate(&mut ui, Control::Setting(SettingControl::Groups));
    // Device: its 8th page (MIDI) starts below the 411 px menu.
    activate(&mut ui, Control::Choose(2));
    let page = Control::Setting(SettingControl::Page(SettingsPage::Midi));
    assert!(ui.hit_regions().iter().all(|r| r.control != page));
    let start = center(
        &ui,
        Control::Setting(SettingControl::Page(SettingsPage::SceneBypass)),
    );
    for _ in 0..4 {
        assert!(drag(&mut ui, start, (start.0, 60)).is_empty());
    }
    assert_eq!(ui.screen(), Screen::Settings(SettingsPage::GlobalBypass));
    let midi = center(&ui, page);
    assert!(midi.1 < 480);
    tap(&mut ui, midi.0, midi.1);
    assert_eq!(ui.screen(), Screen::Settings(SettingsPage::Midi));
}

#[test]
fn keyboard_enforces_name_rules_and_routes_each_target() {
    let mut ui = ui_with(rich());
    activate(&mut ui, Control::OpenMainMenu);
    activate(&mut ui, Control::Menu(MenuAction::SaveAs));
    assert!(ui.keyboard_open());
    ui.handle_input(InputEvent::TextChanged("abc".into()));
    assert!(activate(&mut ui, Control::Key(Key::Done)).is_empty());
    assert!(
        ui.keyboard_open(),
        "names shorter than 4 characters are refused"
    );
    activate(&mut ui, Control::Key(Key::Shift));
    activate(&mut ui, Control::Key(Key::Char('d')));
    activate(&mut ui, Control::Key(Key::Char('e')));
    assert_eq!(
        activate(&mut ui, Control::Key(Key::Done)),
        vec![UiCommand::SavePresetAs {
            name: "AbcDe".into(),
            setlist: None,
            instrument: PreferredInstrument::NotSet,
        }],
        "preset names start with a capital (`0x58e46c`)"
    );
    // Restricted name keys outside the stock set are disabled.
    activate(&mut ui, Control::OpenMainMenu);
    activate(&mut ui, Control::Menu(MenuAction::SaveAs));
    activate(&mut ui, Control::Key(Key::Layer(KeyLayer::Digits)));
    assert!(
        ui.hit_regions()
            .iter()
            .any(|r| r.control == Control::Key(Key::Char('.')) && !r.enabled)
    );
    assert!(activate(&mut ui, Control::Key(Key::Layer(KeyLayer::Symbols))).is_empty());
    activate(&mut ui, Control::Key(Key::Cancel));
    // Wi-Fi: a secured unsaved network asks for its passphrase.
    ui.navigate(Screen::Settings(SettingsPage::Connection));
    assert!(activate(&mut ui, Control::Wifi(WifiControl::Network(0))).is_empty());
    ui.handle_input(InputEvent::TextChanged("short".into()));
    assert!(activate(&mut ui, Control::Key(Key::Done)).is_empty());
    ui.handle_input(InputEvent::TextChanged("long enough".into()));
    assert_eq!(
        activate(&mut ui, Control::Key(Key::Done)),
        vec![UiCommand::WifiConnect {
            ssid: "LOCKED".into(),
            password: Some("long enough".into())
        }]
    );
}

/// Review R5/R8: pasted text obeys the target's character set and limit,
/// and passwords never appear in `Debug` output.
#[test]
fn pasted_text_is_filtered_and_secrets_are_redacted() {
    let mut ui = ui_with(rich());
    activate(&mut ui, Control::OpenMainMenu);
    activate(&mut ui, Control::Menu(MenuAction::SaveAs));
    assert_eq!(
        ui.handle_input(InputEvent::TextChanged(
            "bad/name.with*chars and far too long".into()
        )),
        vec![UiCommand::Rejected(UiError::InvalidName)]
    );
    assert_eq!(
        activate(&mut ui, Control::Key(Key::Done)),
        vec![UiCommand::SavePresetAs {
            name: "Badnamewithchars and".into(),
            setlist: None,
            instrument: PreferredInstrument::NotSet,
        }]
    );
    ui.navigate(Screen::Settings(SettingsPage::Connection));
    activate(&mut ui, Control::Wifi(WifiControl::Network(0)));
    ui.handle_input(InputEvent::TextChanged("hunter2hunter2".into()));
    assert!(!format!("{ui:?}").contains("hunter2"));
    let commands = activate(&mut ui, Control::Key(Key::Done));
    let shown = format!("{commands:?}");
    assert!(
        shown.contains("LOCKED") && !shown.contains("hunter2"),
        "{shown}"
    );
}

/// Connection links (stock `0x5d3818`, `0x5d3b38`): Internet Check opens a popup whose one
/// button flips the check; Domain Settings replaces the page, its radio buttons pick the
/// cloud domain and ✓ returns to the page.
#[test]
fn connection_links_open_the_check_popup_and_domain_settings() {
    let mut snapshot = rich();
    snapshot
        .settings
        .insert(SettingId::InternetCheck, SettingValue::Bool(true));
    snapshot
        .settings
        .insert(SettingId::CloudDomain, SettingValue::Choice(0));
    let mut ui = ui_with(snapshot);
    ui.navigate(Screen::Settings(SettingsPage::Connection));
    assert!(activate(&mut ui, Control::Wifi(WifiControl::InternetCheck)).is_empty());
    assert_eq!(
        activate(&mut ui, Control::Dialog(DialogButton::Primary)),
        vec![UiCommand::ChangeSetting {
            id: SettingId::InternetCheck,
            value: SettingValue::Bool(false),
        }]
    );
    activate(&mut ui, Control::Wifi(WifiControl::DomainSettings));
    let alternative = Control::Setting(SettingControl::Set(SettingId::CloudDomain, 1));
    assert_eq!(
        activate(&mut ui, alternative),
        vec![UiCommand::ChangeSetting {
            id: SettingId::CloudDomain,
            value: SettingValue::Choice(1),
        }]
    );
    activate(&mut ui, Control::Back);
    assert_eq!(ui.screen(), Screen::Settings(SettingsPage::Connection));
    center(&ui, Control::Wifi(WifiControl::DomainSettings));
}

/// Stock network taps (`0x5d4834`, `0x5d5758`): the joined network disconnects, a saved one
/// connects, the forget icon of a saved WPA network forgets it at once; a failure opens its
/// stock popup naming the network (`0x5d5404`).
#[test]
fn wifi_cells_follow_stock_taps_and_failures_pop_up() {
    let network = |ssid: &str, connected: bool| WifiNetwork {
        ssid: ssid.into(),
        saved: true,
        secured: true,
        connected,
        ..WifiNetwork::default()
    };
    let mut snapshot = rich();
    snapshot.wifi.networks = vec![network("HOME", true), network("STUDIO", false)];
    let mut ui = ui_with(snapshot.clone());
    ui.navigate(Screen::Settings(SettingsPage::Connection));
    assert_eq!(
        activate(&mut ui, Control::Wifi(WifiControl::Network(0))),
        vec![UiCommand::WifiDisconnect]
    );
    assert_eq!(
        activate(&mut ui, Control::Wifi(WifiControl::Network(1))),
        vec![UiCommand::WifiConnect {
            ssid: "STUDIO".into(),
            password: None
        }]
    );
    assert_eq!(
        activate(&mut ui, Control::Wifi(WifiControl::Forget(1))),
        vec![UiCommand::WifiForget {
            ssid: "STUDIO".into()
        }]
    );
    snapshot.wifi.alert = Some(WifiAlert::ConnectionError);
    snapshot.wifi.joining = Some("STUDIO".into());
    ui.set_snapshot(snapshot).unwrap();
    assert!(
        ui.layout()
            .items
            .iter()
            .any(|item| item.label == "Failed to reach STUDIO.")
    );
}

/// Review R4: partial redraws match a full render pixel for pixel, and a
/// live change repaints only a small area.
#[test]
fn renderer_repaints_only_changes_and_matches_a_full_render() {
    let mut snapshot = rich();
    snapshot.selected_block = Some(cell(0, 0));
    snapshot.tuner.note = Some(Note {
        pitch_class: 9,
        octave: 2,
    });
    let mut ui = ui_with(snapshot);
    let mut renderer = Renderer::new();
    assert_eq!(renderer.render(&ui), vec![Rect::new(0, 0, 800, 480)]);
    assert!(renderer.render(&ui).is_empty());
    type Step = Box<dyn Fn(&mut UiState)>;
    let mut steps: Vec<Step> = vec![
        Box::new(|ui| {
            let mut s = ui.snapshot().clone();
            s.parameters[1].kind = ParameterKind::Continuous { position: 0.9 };
            s.parameters[1].value = "9".into();
            ui.set_snapshot(s).unwrap();
        }),
        Box::new(|ui| {
            let (x, y) = center(ui, Control::SelectCell(cell(0, 3)));
            ui.handle_input(InputEvent::PointerDown { x, y });
        }),
        Box::new(|ui| {
            ui.handle_input(InputEvent::CancelPointer);
            activate(ui, Control::OpenMainMenu);
        }),
        Box::new(|ui| {
            ui.handle_input(InputEvent::Back);
            ui.show(Screen::Tuner);
        }),
        Box::new(|ui| {
            let mut live = ui.snapshot().live();
            live.tuner.cents = -20.0;
            ui.update_live(live).unwrap();
            ui.show_toast("Saved");
        }),
    ];
    for (index, step) in steps.iter_mut().enumerate() {
        step(&mut ui);
        let rects = renderer.render(&ui);
        assert!(!rects.is_empty(), "step {index}");
        assert_eq!(renderer.frame(), &ui.render(), "step {index}");
        if index == 0 {
            let area: u32 = rects.iter().map(|r| r.width * r.height).sum();
            assert!(area < 800 * 480 / 8, "knob change repainted {rects:?}");
        }
    }
}

#[test]
fn parameter_knobs_follow_the_page() {
    let mut ui = ui_with(UiSnapshot {
        selected_block: Some(cell(0, 0)),
        ..rich()
    });
    let knob = |index| KnobIndex::new(index).unwrap();
    let nudge = |parameter, steps| UiCommand::NudgeParameter {
        position: cell(0, 0),
        parameter,
        steps,
    };
    assert_eq!(
        ui.handle_input(InputEvent::Knob {
            knob: knob(2),
            delta: -3
        }),
        vec![nudge(4, -3)]
    );
    assert_eq!(
        activate(&mut ui, Control::Param(ParamControl::Knob(knob(3)))),
        vec![nudge(6, 1)]
    );
    activate(&mut ui, Control::Param(ParamControl::Page(1)));
    assert_eq!(
        ui.handle_input(InputEvent::Knob {
            knob: knob(1),
            delta: 1
        }),
        vec![nudge(22, 1)]
    );
    assert!(
        ui.handle_input(InputEvent::Knob {
            knob: knob(2),
            delta: 1
        })
        .is_empty()
    );
    assert_eq!(KnobIndex::new(10), Err(UiError::InvalidKnob));
}

/// Stock knob drag law (`ParameterControl::mouseMoveEvent` `0x4c9e20`): the normalized value
/// moves 1/200 per pixel from the first pixel, clamps at the ends and keeps no overshoot. On
/// the QC (2026-10-16) VOLUME 0–10 moved +0.5, +1.0, +2.0, +4.0 for 10, 20, 40, 80 px up.
/// Toggles, Int and ComboBox knobs do not drag (stock mask `0x1013`).
#[test]
fn knob_drag_follows_the_stock_law() {
    let mut snapshot = UiSnapshot {
        selected_block: Some(cell(0, 0)),
        ..rich()
    };
    snapshot.parameters[0].kind = ParameterKind::Continuous { position: 0.1 };
    let mut ui = ui_with(snapshot);
    let set = |parameter, value| UiCommand::SetPosition {
        position: cell(0, 0),
        parameter,
        value,
    };
    let knob =
        |ui: &UiState, index| center(ui, Control::Param(ParamControl::Knob(KnobIndex(index))));
    let to = |ui: &mut UiState, x, y| ui.handle_input(InputEvent::PointerMove { x, y });

    let (x, y) = knob(&ui, 0);
    ui.handle_input(InputEvent::PointerDown { x, y });
    assert_eq!(to(&mut ui, x, y - 1), [set(0, 1050)], "no slop");
    for (px, value) in [(10, 1500), (20, 2000), (40, 3000), (80, 5000)] {
        assert_eq!(to(&mut ui, x, y - px), [set(0, value)]);
    }
    assert!(
        to(&mut ui, x + 30, y - 80).is_empty(),
        "sideways moves nothing"
    );
    assert_eq!(to(&mut ui, x, y - 300), [set(0, 10_000)]);
    assert!(to(&mut ui, x, y - 400).is_empty(), "held at the end");
    assert_eq!(
        to(&mut ui, x, y - 390),
        [set(0, 9500)],
        "overshoot is not kept"
    );
    assert!(
        ui.handle_input(InputEvent::PointerUp { x, y: y - 390 })
            .is_empty()
    );

    let (x, y) = knob(&ui, 3);
    ui.handle_input(InputEvent::PointerDown { x, y });
    assert!(to(&mut ui, x, y - 40).is_empty(), "a toggle does not drag");
    ui.handle_input(InputEvent::PointerUp { x, y: y - 40 });
    let mut s = ui.snapshot().clone();
    s.parameters[0].kind = ParameterKind::Int { position: 0.5 };
    s.parameters[1].kind = ParameterKind::ComboBox { position: 0.0 };
    let mut ui = ui_with(s);
    for index in [0, 1] {
        let (x, y) = knob(&ui, index);
        ui.handle_input(InputEvent::PointerDown { x, y });
        assert!(
            to(&mut ui, x, y - 40).is_empty(),
            "knob {index} does not drag"
        );
        ui.handle_input(InputEvent::PointerUp { x, y: y - 40 });
    }

    // A full-screen editor knob takes the same law.
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::Modulation, 7040);
    s.parameters = vec![continuous(0, "RATE", 0.5)];
    let mut ui = ui_with(s);
    let (x, y) = center(&ui, Control::Editor(EditorControl::Knob(0)));
    assert_eq!(
        drag(&mut ui, (x, y), (x, y + 40)),
        [UiCommand::SetPosition {
            position,
            parameter: 0,
            value: 3000,
        }]
    );
}

/// Stock RotarySwitch drag (`0x4c9e20`, `0x5cc994`, `0x5cc9e0`): the sum rounds to a step; on
/// the same step the anchor stays, on a new one the value is the step and the anchor moves.
/// A 5-position switch (step 0.25) changes every 100 · 0.25 = 25 px, up to the end.
#[test]
fn choice_knob_drag_changes_one_step_per_100_step_px() {
    let mut snapshot = UiSnapshot {
        selected_block: Some(cell(0, 0)),
        ..rich()
    };
    snapshot.parameters[1].kind = ParameterKind::Choice {
        position: 0.0,
        steps: 5,
    };
    let mut ui = ui_with(snapshot);
    let (x, y) = center(&ui, Control::Param(ParamControl::Knob(KnobIndex(1))));
    ui.handle_input(InputEvent::PointerDown { x, y });
    let mut to = |dy| ui.handle_input(InputEvent::PointerMove { x, y: y - dy });
    let set = |value| {
        vec![UiCommand::SetPosition {
            position: cell(0, 0),
            parameter: 2,
            value,
        }]
    };
    assert!(to(24).is_empty(), "travel adds up below the step");
    assert_eq!(to(25), set(2500));
    assert!(to(49).is_empty());
    assert_eq!(to(50), set(5000), "the next step 25 px later, not 75");
    assert_eq!(to(75), set(7500));
    assert_eq!(to(100), set(10_000));
    assert!(to(140).is_empty(), "held at the end");
    assert_eq!(to(114), set(7500), "travel past the end is lost");
}

/// Review R2/R3: shortcuts are tetrad-core's. The UI forwards every edge,
/// opens screens on request and drops the release of a press it consumed.
#[test]
fn footswitches_forward_edges_and_consume_only_ui_local_presses() {
    let mut snapshot = rich();
    snapshot.footswitch_mode = Some(FootswitchMode::Stomp);
    let mut ui = ui_with(snapshot);
    let press = |ui: &mut UiState, switch, pressed| {
        ui.handle_input(InputEvent::Footswitch { switch, pressed })
    };
    let edge = |switch, pressed, target| {
        vec![UiCommand::Footswitch {
            switch,
            pressed,
            target,
        }]
    };
    assert_eq!(
        press(&mut ui, Footswitch::B, true),
        edge(Footswitch::B, true, FootswitchTarget::Stomp(Footswitch::B))
    );
    assert_eq!(
        press(&mut ui, Footswitch::C, true),
        edge(Footswitch::C, true, FootswitchTarget::Unassigned)
    );
    press(&mut ui, Footswitch::B, false);
    press(&mut ui, Footswitch::C, false);
    // No UI-owned holds, chords or double taps.
    ui.handle_input(InputEvent::Tick { now_ms: 10_000 });
    press(&mut ui, Footswitch::D, true);
    assert!(
        ui.handle_input(InputEvent::Tick { now_ms: 12_000 })
            .is_empty()
    );
    press(&mut ui, Footswitch::A, true);
    press(&mut ui, Footswitch::C, true);
    assert_eq!(ui.screen(), Screen::Grid);
    for switch in [Footswitch::A, Footswitch::C, Footswitch::D] {
        press(&mut ui, switch, false);
    }
    assert_eq!(
        press(&mut ui, Footswitch::Tempo, true),
        edge(Footswitch::Tempo, true, FootswitchTarget::Unassigned)
    );
    press(&mut ui, Footswitch::Tempo, false);
    // The application opens the tuner after tetrad-core's hold TEMPO. TEMPO
    // and A stay with tetrad-core; BANK UP closes the tuner (manual), and its
    // release does not reach the application.
    activate(&mut ui, Control::OpenMainMenu);
    assert_eq!(
        ui.show(Screen::Tuner),
        vec![UiCommand::ShowTuner { show: true }]
    );
    assert!(!ui.overlay_open());
    for switch in [Footswitch::Tempo, Footswitch::A] {
        assert_eq!(press(&mut ui, switch, true).len(), 1);
        press(&mut ui, switch, false);
    }
    assert_eq!(
        press(&mut ui, Footswitch::BankUp, true),
        vec![UiCommand::ShowTuner { show: false }]
    );
    assert!(press(&mut ui, Footswitch::BankUp, false).is_empty());
    ui.show(Screen::Tempo);
    assert!(ui.hide(Screen::Tuner).is_empty());
    ui.hide(Screen::Tempo);
    assert_eq!(ui.stack(), [Screen::Grid]);
    // B+D closes the looper view on a mini only (`0x6153e4`).
    ui.navigate(Screen::Looper);
    let chord = |ui: &mut UiState| {
        assert_eq!(press(ui, Footswitch::B, true).len(), 1);
        press(ui, Footswitch::D, true);
        let screen = ui.screen();
        press(ui, Footswitch::B, false);
        press(ui, Footswitch::D, false);
        screen
    };
    assert_eq!(chord(&mut ui), Screen::Looper);
    let mut snapshot = ui.snapshot().clone();
    snapshot.looper.mini = true;
    ui.set_snapshot(snapshot).unwrap();
    assert_eq!(chord(&mut ui), Screen::Grid);
}

/// Copy / Swap Scene (`0x5ad9d4`): the dialog puts the device in footswitch scene copy mode
/// and closes only when the device leaves it (`0x20eec4`): a press on the source scene keeps
/// both (review r15 R1). CANCEL, another screen (R2) and a preset load (R6) end it; an
/// on-screen destination copies and ends it.
#[test]
fn scene_copy_dialog_drives_the_device_scene_copy_mode() {
    let mut snapshot = rich();
    let mut ui = ui_with(snapshot.clone());
    let press = |ui: &mut UiState, switch| {
        let mut commands = ui.handle_input(InputEvent::Footswitch {
            switch,
            pressed: true,
        });
        commands.extend(ui.handle_input(InputEvent::Footswitch {
            switch,
            pressed: false,
        }));
        commands
    };
    let open = |ui: &mut UiState, action| {
        activate(ui, Control::OpenMainMenu);
        activate(ui, Control::Menu(action))
    };
    let dialog = |ui: &UiState| shows(ui, Control::SceneTarget(scene(5)));
    assert_eq!(
        open(&mut ui, MenuAction::CopyScene),
        [UiCommand::BeginSceneCopy {
            source: scene(0),
            swap: false,
        }]
    );
    snapshot.scene_copy = true;
    ui.set_snapshot(snapshot.clone()).unwrap();
    // The source scene: the device keeps the mode, so the dialog stays.
    assert!(press(&mut ui, Footswitch::A).is_empty());
    assert!(dialog(&ui));
    // Another scene: the device copies and leaves the mode; its snapshot closes the dialog.
    assert!(press(&mut ui, Footswitch::F).is_empty());
    assert!(dialog(&ui));
    snapshot.scene_copy = false;
    ui.set_snapshot(snapshot.clone()).unwrap();
    assert!(!dialog(&ui));

    open(&mut ui, MenuAction::SwapScene);
    assert_eq!(ui.handle_input(InputEvent::Back), [UiCommand::EndSceneCopy]);

    open(&mut ui, MenuAction::CopyScene);
    assert_eq!(
        activate(&mut ui, Control::SceneTarget(scene(5))),
        [
            UiCommand::CopyScene {
                from: scene(0),
                to: scene(5)
            },
            UiCommand::EndSceneCopy
        ]
    );

    open(&mut ui, MenuAction::CopyScene);
    snapshot.preset_id = Some(8);
    ui.set_snapshot(snapshot).unwrap();
    assert!(!dialog(&ui), "a preset load closes the dialog");

    open(&mut ui, MenuAction::CopyScene);
    assert!(ui.show(Screen::Tuner).contains(&UiCommand::EndSceneCopy));
}

/// BANK UP encoder (manual §4): the block editor cycles the device one model per event,
/// whatever the acceleration (`0x4f5f38`, review r15 R7); the Virtual Device Preset list
/// auditions the next row of the open tab, wrapping.
#[test]
fn bank_up_encoder_cycles_the_device_or_auditions_the_next_vd_preset() {
    let mut snapshot = rich();
    snapshot.blocks[0].model_id = Some(1001);
    snapshot.selected_block = Some(cell(0, 0));
    snapshot.device_presets = [1, 2, 3]
        .map(|id| DevicePresetItem {
            id,
            model_id: 1001,
            name: format!("F{id}"),
            factory: true,
            default: false,
        })
        .to_vec()
        .into();
    let mut ui = ui_with(snapshot.clone());
    let turn = |ui: &mut UiState, delta| ui.handle_input(InputEvent::BankEncoder { delta });
    assert_eq!(
        turn(&mut ui, -2),
        [UiCommand::CycleModel {
            position: cell(0, 0),
            step: -1
        }]
    );
    activate(&mut ui, Control::SelectCell(cell(0, 0)));
    activate(&mut ui, Control::Picker(PickerControl::Presets(1001)));
    let audition = |preset| {
        vec![UiCommand::DevicePreset {
            position: cell(0, 0),
            request: DevicePresetRequest::Audition {
                model_id: 1001,
                preset,
            },
        }]
    };
    assert_eq!(turn(&mut ui, 1), audition(1));
    snapshot.device_preset = Some(1);
    ui.set_snapshot(snapshot).unwrap();
    assert_eq!(turn(&mut ui, -1), audition(3));
}

#[test]
fn settings_rows_request_changes_and_unknown_values_stay_disabled() {
    let mut ui = ui_with(rich());
    ui.navigate(Screen::Settings(SettingsPage::Midi));
    assert_eq!(
        activate(
            &mut ui,
            Control::Setting(SettingControl::Open(SettingId::MidiThru))
        ),
        vec![UiCommand::ChangeSetting {
            id: SettingId::MidiThru,
            value: SettingValue::Bool(true)
        }]
    );
    assert!(
        activate(
            &mut ui,
            Control::Setting(SettingControl::Open(SettingId::MidiChannel))
        )
        .is_empty()
    );
    activate(
        &mut ui,
        Control::Setting(SettingControl::Open(SettingId::MidiClockOut)),
    );
    assert!(ui.overlay_open());
    assert_eq!(
        activate(&mut ui, Control::Choose(3)),
        vec![UiCommand::ChangeSetting {
            id: SettingId::MidiClockOut,
            value: SettingValue::Choice(3)
        }]
    );
    ui.navigate(Screen::Tuner);
    assert_eq!(
        ui.handle_input(InputEvent::Knob {
            knob: KnobIndex(0),
            delta: 1
        }),
        vec![UiCommand::ChangeSetting {
            id: SettingId::TunerReference,
            value: SettingValue::Number(480)
        }]
    );
    activate(&mut ui, Control::Back);
    ui.navigate(Screen::Settings(SettingsPage::FactoryReset));
    assert!(activate(&mut ui, Control::Action(DeviceAction::FactoryReset)).is_empty());
    assert_eq!(
        activate(&mut ui, Control::Dialog(DialogButton::Primary)),
        vec![UiCommand::Device {
            action: DeviceAction::FactoryReset
        }]
    );
}

#[test]
fn toasts_expire_on_the_application_clock() {
    let mut ui = UiState::new();
    ui.handle_input(InputEvent::Tick { now_ms: 5_000 });
    ui.show_toast("Saved");
    let shown = ui.render();
    ui.handle_input(InputEvent::Tick { now_ms: 7_499 });
    assert_eq!(ui.render(), shown);
    ui.handle_input(InputEvent::Tick { now_ms: 7_500 });
    assert_ne!(ui.render(), shown);
    assert_eq!(ui.render(), UiState::new().render());
}

#[test]
fn stomp_rename_simplifies_and_uppercases_only_an_ascii_first_letter() {
    assert_eq!(crate::state::stomp_name("  big   muff "), "Big muff");
    assert_eq!(crate::state::stomp_name("élan"), "élan");
    let mut snapshot = rich();
    snapshot.footswitch_mode = Some(FootswitchMode::Stomp);
    let mut ui = ui_with(snapshot);
    swipe_up_to_gig(&mut ui);
    let (x, y) = center(&ui, Control::Tile(Footswitch::B));
    ui.handle_input(InputEvent::PointerDown { x, y });
    // A 750 ms hold (`LONG_PRESS_MS`); the test held 600 ms over the old 500 ms.
    ui.handle_input(InputEvent::Tick {
        now_ms: crate::state::LONG_PRESS_MS,
    });
    ui.handle_input(InputEvent::PointerUp { x, y });
    ui.handle_input(InputEvent::TextChanged("wide  echo".into()));
    assert_eq!(
        activate(&mut ui, Control::Key(Key::Done)),
        vec![UiCommand::RenameStomp {
            switch: Footswitch::B,
            label: "Wide echo".into()
        }]
    );
}

#[test]
fn snapshot_limits_preserve_last_valid_state() {
    let mut ui = UiState::new();
    let original = ui.snapshot().clone();
    for (invalid, error) in [
        (
            UiSnapshot {
                preset_title: "a".repeat(MAX_TEXT_BYTES + 1),
                ..original.clone()
            },
            UiError::TextTooLong,
        ),
        (
            UiSnapshot {
                presets: vec![
                    PresetItem {
                        id: 1,
                        title: "ONE".into()
                    };
                    2
                ],
                ..original.clone()
            },
            UiError::DuplicatePreset,
        ),
        (
            UiSnapshot {
                presets: (0..=MAX_PRESETS as u64)
                    .map(|id| PresetItem {
                        id,
                        title: String::new(),
                    })
                    .collect(),
                ..original.clone()
            },
            UiError::TooManyPresets,
        ),
        (
            UiSnapshot {
                tempo: TempoView {
                    bpm: f32::NAN,
                    ..TempoView::default()
                },
                ..original.clone()
            },
            UiError::InvalidNumber,
        ),
    ] {
        assert_eq!(ui.set_snapshot(invalid), Err(error));
        assert_eq!(ui.snapshot(), &original);
    }
    edit_scenes(&mut ui);
    assert_eq!(
        ui.handle_input(InputEvent::TextChanged("x".repeat(MAX_TEXT_BYTES + 1))),
        vec![UiCommand::Rejected(UiError::TextTooLong)]
    );
    assert_eq!(
        activate(&mut ui, Control::ConfirmScenes)[0],
        UiCommand::SetSceneLabel {
            scene: scene(0),
            label: String::new()
        }
    );
}

/// Review r11 N6: a corrupt recents file or sidecar is clamped, not a rejected snapshot.
#[test]
fn oversized_directory_files_are_clamped_not_rejected() {
    let mut ui = UiState::new();
    let mut snapshot = ui.snapshot().clone();
    let item = |kind, value: String| DirectoryItem {
        legacy_index: None,
        kind,
        folder: String::new(),
        value,
        name: "é".repeat(MAX_TEXT_BYTES),
        author: String::new(),
        date: String::new(),
        instrument: PreferredInstrument::NotSet,
    };
    std::sync::Arc::make_mut(&mut snapshot.directory).recents = (0..70)
        .map(|index| item(ItemKind::Preset, format!("{index}.json")))
        .chain([item(ItemKind::Capture, "x".repeat(MAX_TEXT_BYTES + 1))])
        .chain([item(ItemKind::Ir, "ir.wav".into())])
        .collect();
    assert_eq!(ui.set_snapshot(snapshot), Ok(()));
    let recents = &ui.snapshot().directory.recents;
    assert_eq!(recents.len(), 65);
    assert_eq!(recents[63].value, "63.json");
    assert_eq!(recents[64].kind, ItemKind::Ir);
    assert_eq!(recents[0].name.len(), MAX_TEXT_BYTES);
}

#[test]
fn ppm_is_exact_rgb_and_propagates_writer_failures() {
    let frame = UiState::new().render();
    assert_eq!((frame.width(), frame.height()), (800, 480));
    assert_eq!(frame.rgb().len(), 1_152_000);
    let mut encoded = Vec::new();
    frame.write_ppm(&mut encoded).unwrap();
    let header = b"P6\n800 480\n255\n";
    assert_eq!(&encoded[..header.len()], header);
    assert_eq!(&encoded[header.len()..], frame.rgb());
    struct FailingWriter;
    impl Write for FailingWriter {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert_eq!(
        frame.write_ppm(FailingWriter).unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
}

/// Save flows (`0x5ac544`, `0x5ac350`, `0x5b91dc`): Save of a preset without a file opens
/// "Name your Preset"; Save as... of a filed preset asks for the destination setlist; Edit
/// Details saves the loaded preset under the new name; names start with a capital.
#[test]
fn save_save_as_and_edit_details_flows() {
    let typed = |ui: &mut UiState, text: &str| {
        ui.handle_input(InputEvent::TextChanged(text.into()));
        activate(ui, Control::Key(Key::Done))
    };
    let mut ui = ui_with(UiSnapshot {
        preset_id: Some(3),
        preset_file: false,
        ..UiSnapshot::default()
    });
    assert!(activate(&mut ui, Control::SavePreset).is_empty());
    assert_eq!(
        typed(&mut ui, "new tone"),
        vec![UiCommand::SavePresetAs {
            name: "New tone".into(),
            setlist: None,
            instrument: PreferredInstrument::NotSet,
        }]
    );

    let mut snapshot = rich();
    snapshot.setlists = ["My Presets", "Live", "Studio"]
        .into_iter()
        .enumerate()
        .map(|(id, title)| SetlistItem {
            id: id as u64,
            title: title.into(),
        })
        .collect();
    snapshot.active_setlist = Some(0);
    snapshot.preset_instrument = PreferredInstrument::Bass;
    let mut ui = ui_with(snapshot);
    assert_eq!(
        activate(&mut ui, Control::SavePreset),
        vec![UiCommand::SavePreset]
    );
    activate(&mut ui, Control::OpenMainMenu);
    activate(&mut ui, Control::Menu(MenuAction::SaveAs));
    assert!(
        typed(&mut ui, "Lead copy").is_empty(),
        "the setlist comes next"
    );
    assert_eq!(
        activate(&mut ui, Control::Choose(2)),
        vec![UiCommand::SavePresetAs {
            name: "Lead copy".into(),
            setlist: Some(2),
            instrument: PreferredInstrument::Bass,
        }],
        "the instrument starts on the stored one (`0x58ed3c`)"
    );
    activate(&mut ui, Control::OpenMainMenu);
    activate(&mut ui, Control::Menu(MenuAction::EditDetails));
    assert_eq!(
        typed(&mut ui, "Renamed"),
        vec![UiCommand::EditPresetDetails {
            name: "Renamed".into(),
            instrument: PreferredInstrument::Bass,
        }],
        "Edit Details keeps the stored instrument unless the page changes it"
    );
}

/// PREFERRED INSTRUMENT (`PresetMetadataDialog` `0x7e79b8`): the metadata button opens the
/// page over the name keyboard; BACK keeps the name and the pick; SAVE saves as Done does,
/// with the picked instrument: Save As through `presetSaveSignal`, Edit Details through
/// `modifyPresetMetadataSignal` (`0x58fe80`, review gap I3).
#[test]
fn preset_save_metadata_page_picks_the_preferred_instrument() {
    let metadata = |control| Control::PresetMetadata(control);
    let mut snapshot = rich();
    snapshot.preset_file = true;
    snapshot.setlists = vec![SetlistItem {
        id: 4,
        title: "Live".into(),
    }];
    snapshot.active_setlist = Some(4);
    let mut ui = ui_with(snapshot);
    activate(&mut ui, Control::OpenMainMenu);
    activate(&mut ui, Control::Menu(MenuAction::EditDetails));
    ui.handle_input(InputEvent::TextChanged("clean tone".into()));
    activate(&mut ui, metadata(PresetMetadataControl::Open));
    let guitar = PreferredInstrument::Guitar;
    activate(&mut ui, metadata(PresetMetadataControl::Instrument(guitar)));
    assert_eq!(
        activate(&mut ui, metadata(PresetMetadataControl::Save)),
        vec![UiCommand::EditPresetDetails {
            name: "Clean tone".into(),
            instrument: guitar,
        }]
    );

    activate(&mut ui, Control::OpenMainMenu);
    activate(&mut ui, Control::Menu(MenuAction::SaveAs));
    ui.handle_input(InputEvent::TextChanged("solo tone".into()));
    activate(&mut ui, metadata(PresetMetadataControl::Open));
    let vocal = metadata(PresetMetadataControl::Instrument(
        PreferredInstrument::Vocal,
    ));
    activate(&mut ui, vocal);
    activate(&mut ui, metadata(PresetMetadataControl::Back));
    assert!(
        activate(&mut ui, vocal).is_empty() && !ui.keyboard.as_ref().unwrap().metadata_page,
        "the picker is not on the keys page"
    );
    activate(&mut ui, metadata(PresetMetadataControl::Open));
    assert_eq!(
        activate(&mut ui, metadata(PresetMetadataControl::Save)),
        vec![UiCommand::SavePresetAs {
            name: "Solo tone".into(),
            setlist: Some(4),
            instrument: PreferredInstrument::Vocal,
        }]
    );
    assert!(ui.keyboard.is_none());
}

/// K6 (re-r44 §4): the preset save keyboard has the stock `NewDialogWithHeader` (close, divider,
/// folder, name, metadata, forward; each x = previous right + 8) and the "KeyboardTextfield"
/// at (40, 97), 720 wide on `#121212`, with no title and no "Characters left" row. Forward
/// is `#1e1e1e` and off while the name is not 4..20 characters (`0x590c34`).
#[test]
fn preset_save_keyboard_has_the_stock_header_and_field() {
    let mut ui = ui_with(rich());
    activate(&mut ui, Control::OpenMainMenu);
    activate(&mut ui, Control::Menu(MenuAction::SaveAs));
    ui.handle_input(InputEvent::TextChanged("ab".into()));
    let layout = ui.layout();
    let item = |label: &str| {
        (layout.items.iter().rev())
            .find(|item| item.label == label && item.control.is_some())
            .unwrap_or_else(|| panic!("no {label} control"))
    };
    let panel_at = |bounds: Rect| {
        (layout.items.iter()).find_map(|item| match item.look {
            crate::layout::Look::Panel { fill, border, .. } if item.bounds == bounds => {
                Some((fill, border))
            }
            _ => None,
        })
    };
    assert_eq!(item("CLOSE").bounds, Rect::new(8, 0, 98, 60));
    assert_eq!(item("CLOSE").control, Some(Control::Key(Key::Cancel)));
    assert_eq!(
        panel_at(Rect::new(114, 19, 2, 21)),
        Some((Palette::STOCK.panel, None))
    );
    assert!(panel_at(Rect::new(124, 8, 179, 44)).is_some(), "folder");
    assert_eq!(item("NAME").bounds, Rect::new(311, 8, 76, 44));
    assert_eq!(
        panel_at(Rect::new(311, 8, 76, 44)),
        Some((Palette::STOCK.accent_fill, Some(Palette::STOCK.accent)))
    );
    assert_eq!(item("METADATA").bounds, Rect::new(395, 8, 66, 44));
    let forward = item("FORWARD");
    assert_eq!(forward.bounds, Rect::new(694, 0, 98, 60));
    assert_eq!(forward.control, Some(Control::Key(Key::Done)));
    assert!(!forward.enabled, "a 2-character name cannot go forward");
    assert_eq!(
        panel_at(Rect::new(40, 97, 720, 74)),
        Some((Palette::STOCK.background, None))
    );
    assert!(
        (layout.items.iter()).all(|item| !item.label.starts_with("Characters left")
            && item.label != "Name your Preset"
            && item.label != "CANCEL"),
        "no title, counter or CANCEL"
    );

    ui.handle_input(InputEvent::TextChanged("abcd".into()));
    let layout = ui.layout();
    let forward = (layout.items.iter()).find(|item| item.label == "FORWARD");
    assert!(forward.is_some_and(|item| item.enabled));
}

/// The Directory (manual §5): a row's ⋮ opens its menu (Edit Name, Duplicate), banks page
/// the list, and the favorites category (drop-down entry 0) loads or removes favorites.
#[test]
fn directory_menus_banks_and_favorites() {
    let mut snapshot = rich();
    snapshot.favorite_presets = vec![PresetItem {
        id: 0,
        title: "FAV".into(),
    }];
    let mut ui = ui_with(snapshot);
    activate(&mut ui, Control::Navigate(Screen::Presets));
    activate(&mut ui, Control::PresetMenu(2));
    assert_eq!(
        activate(
            &mut ui,
            Control::Library(LibraryControl::Item(ItemAction::Duplicate))
        ),
        vec![UiCommand::DuplicatePreset { id: 2 }]
    );
    activate(&mut ui, Control::Library(LibraryControl::Bank(1)));
    assert_eq!(
        activate(&mut ui, Control::SelectPreset(9)),
        vec![UiCommand::RequestPreset { id: 9 }]
    );
    activate(&mut ui, Control::Library(LibraryControl::Category));
    activate(&mut ui, Control::Choose(0));
    assert_eq!(
        activate(&mut ui, Control::Library(LibraryControl::Favorite(0))),
        vec![UiCommand::LoadFavorite { index: 0 }]
    );
    assert_eq!(
        activate(&mut ui, Control::Library(LibraryControl::Unfavorite(0))),
        vec![UiCommand::RemoveFavorite { index: 0 }]
    );
}

/// Banks hold the application's bank size: 8, or 4 when the mode cycle has a hybrid with
/// PRESET (`0x58b968`, tetrad-core `BankSize`).
#[test]
fn directory_banks_follow_the_bank_size_of_the_mode_cycle() {
    let mut snapshot = rich();
    let mut ui = ui_with(snapshot.clone());
    activate(&mut ui, Control::Navigate(Screen::Presets));
    let banks = |ui: &UiState| {
        ui.hit_regions()
            .iter()
            .filter(|r| matches!(r.control, Control::Library(LibraryControl::Bank(_))))
            .count()
    };
    assert_eq!(banks(&ui), 2);
    snapshot.allowed_modes = vec![FootswitchMode::Preset, FootswitchMode::Hybrid];
    snapshot.hybrid = HybridRows {
        upper: FootswitchMode::Preset,
        lower: FootswitchMode::Scene,
    };
    snapshot.bank_size = 4;
    ui.set_snapshot(snapshot).unwrap();
    assert_eq!(banks(&ui), 3);
    activate(&mut ui, Control::Library(LibraryControl::Bank(1)));
    let row = ui
        .hit_regions()
        .into_iter()
        .find(|r| r.control == Control::SelectPreset(4))
        .expect("bank 2 shows slot 4");
    assert_eq!(row.label, "2A PRESET 4");
}

/// Multi Select (manual §5): checked presets copy into a setlist, hold COPY (500 ms) moves
/// them, and DELETE asks once for all of them.
#[test]
fn directory_multi_select_copies_moves_and_deletes() {
    let mut snapshot = rich();
    snapshot.setlists = ["Factory Presets", "My Presets"]
        .into_iter()
        .enumerate()
        .map(|(id, title)| SetlistItem {
            id: id as u64,
            title: title.into(),
        })
        .collect();
    snapshot.active_setlist = Some(0);
    let mut ui = ui_with(snapshot);
    let library = Control::Library;
    activate(&mut ui, Control::Navigate(Screen::Presets));
    activate(&mut ui, library(LibraryControl::MultiSelect));
    // Rows toggle their check box instead of loading.
    assert!(activate(&mut ui, Control::SelectPreset(1)).is_empty());
    for id in [1, 3] {
        activate(&mut ui, library(LibraryControl::Check(id)));
    }
    activate(&mut ui, library(LibraryControl::Bulk(BulkAction::Copy)));
    assert_eq!(
        activate(&mut ui, Control::Choose(1)),
        vec![UiCommand::CopyPresets {
            ids: vec![1, 3],
            from: "Factory Presets".into(),
            setlist: 1,
            remove: false,
            to: PasteTarget::FirstEmpty,
        }]
    );
    activate(&mut ui, library(LibraryControl::MultiSelect));
    activate(&mut ui, library(LibraryControl::Check(2)));
    let (x, y) = center(&ui, library(LibraryControl::Bulk(BulkAction::Copy)));
    ui.handle_input(InputEvent::Tick { now_ms: 1_000 });
    ui.handle_input(InputEvent::PointerDown { x, y });
    // A 750 ms hold (`LONG_PRESS_MS`); the test used 500 ms before the stock value.
    ui.handle_input(InputEvent::Tick { now_ms: 1_750 });
    assert!(ui.handle_input(InputEvent::PointerUp { x, y }).is_empty());
    assert_eq!(
        activate(&mut ui, Control::Choose(1)),
        vec![UiCommand::CopyPresets {
            ids: vec![2],
            from: "Factory Presets".into(),
            setlist: 1,
            remove: true,
            to: PasteTarget::FirstEmpty,
        }]
    );
    activate(&mut ui, library(LibraryControl::MultiSelect));
    for id in [4, 5] {
        activate(&mut ui, library(LibraryControl::Check(id)));
    }
    activate(&mut ui, library(LibraryControl::Bulk(BulkAction::Delete)));
    assert_eq!(
        activate(&mut ui, library(LibraryControl::ConfirmBulkDelete)),
        vec![
            UiCommand::DeletePreset { id: 4 },
            UiCommand::DeletePreset { id: 5 }
        ]
    );
}

/// Directory categories and search (shots 047, 024): captures sort by name with an
/// alphabet strip; search counts each type, remembers the query, and a capture result
/// opens its folder; Back closes the pop-up, then search, then the Directory.
#[test]
fn directory_categories_search_and_back() {
    let mut snapshot = rich();
    snapshot.media = vec![
        ("Zeta", MediaFolder::MyCaptures),
        ("Alpha", MediaFolder::MyCaptures),
        ("Mid", MediaFolder::MyCaptures),
        ("Alpha Room", MediaFolder::MyIrs),
    ]
    .into_iter()
    .map(|(name, folder)| MediaEntry {
        folder,
        name: name.into(),
        value: name.into(),
    })
    .collect::<Vec<_>>()
    .into();
    let mut ui = ui_with(snapshot);
    let library = Control::Library;
    activate(&mut ui, Control::Navigate(Screen::Presets));
    activate(&mut ui, library(LibraryControl::Category));
    activate(&mut ui, Control::Choose(2));
    let rows: Vec<Control> = ui
        .hit_regions()
        .into_iter()
        .map(|r| r.control)
        .filter(|c| matches!(c, Control::Library(LibraryControl::Media(_))))
        .collect();
    assert_eq!(
        rows,
        [1, 2, 0].map(|index| library(LibraryControl::Media(index)))
    );
    activate(&mut ui, library(LibraryControl::Index(b'Z')));
    assert_eq!(ui.offset(crate::layout::ListId::DirectoryItems), 2 * 60);

    activate(&mut ui, library(LibraryControl::Search));
    for key in "al".chars() {
        activate(&mut ui, Control::Key(Key::Char(key)));
    }
    activate(&mut ui, Control::Key(Key::Done));
    let tab = |ui: &UiState, kind| {
        ui.hit_regions()
            .into_iter()
            .find(|r| r.control == library(LibraryControl::Kind(kind)))
            .map(|r| r.label)
    };
    assert_eq!(tab(&ui, ItemKind::Capture).as_deref(), Some("(1)"));
    assert_eq!(tab(&ui, ItemKind::Ir).as_deref(), Some("(1)"));
    assert_eq!(tab(&ui, ItemKind::Preset).as_deref(), Some("(0)"));
    activate(&mut ui, library(LibraryControl::Reveal(1)));
    assert!(tab(&ui, ItemKind::Capture).is_none(), "search closed");
    assert_eq!(ui.directory.selected_media, Some(1));

    // A new search lists the recent query before typing.
    activate(&mut ui, library(LibraryControl::Search));
    activate(&mut ui, Control::Key(Key::Cancel));
    activate(&mut ui, library(LibraryControl::ClearSearch));
    activate(&mut ui, library(LibraryControl::Search));
    activate(&mut ui, Control::Key(Key::Cancel));
    activate(&mut ui, library(LibraryControl::RecentSearch(0)));
    assert_eq!(ui.library_query, "al");
    activate(&mut ui, library(LibraryControl::Sort));
    assert!(ui.handle_input(InputEvent::Back).is_empty());
    assert!(ui.handle_input(InputEvent::Back).is_empty());
    assert_eq!(ui.screen(), Screen::Presets);
    assert!(tab(&ui, ItemKind::Capture).is_none(), "search closed");
    ui.handle_input(InputEvent::Back);
    assert_eq!(ui.screen(), Screen::Grid);
}

/// Gig View scene tools (`0x664954`) copy or swap from the tile's own scene, and the nav
/// bar shows UNDO/REDO only with history (`0x7e7eec`).
#[test]
fn gig_scene_tools_and_undo_visibility() {
    let mut snapshot = rich();
    snapshot.footswitch_mode = Some(FootswitchMode::Scene);
    let mut ui = ui_with(snapshot.clone());
    let shown = |ui: &UiState, control| ui.hit_regions().iter().any(|r| r.control == control);
    assert!(!shown(&ui, Control::Undo) && !shown(&ui, Control::Redo));
    swipe_up_to_gig(&mut ui);
    activate(
        &mut ui,
        Control::SceneTool {
            scene: scene(2),
            tool: SceneTool::Swap,
        },
    );
    assert_eq!(
        activate(&mut ui, Control::SceneTarget(scene(5))),
        vec![
            UiCommand::SwapScene {
                first: scene(2),
                second: scene(5),
            },
            UiCommand::EndSceneCopy
        ]
    );
    snapshot.can_undo = true;
    ui.set_snapshot(snapshot).unwrap();
    ui.navigate(Screen::Grid);
    assert_eq!(activate(&mut ui, Control::Undo), vec![UiCommand::Undo]);
}

/// Looper X: eight tiles A–H in stock order (`0x6129fc`) with one UNDO/REDO tile.
#[test]
fn looper_has_the_eight_stock_tiles() {
    let mut snapshot = rich();
    snapshot.looper.state = LooperState::Playing;
    snapshot.looper.redo_available = true;
    let mut ui = ui_with(snapshot);
    ui.navigate(Screen::Looper);
    let tiles: Vec<(LooperAction, String)> = ui
        .hit_regions()
        .into_iter()
        .filter_map(|region| match region.control {
            Control::Looper(action) => Some((action, region.label)),
            _ => None,
        })
        .collect();
    let titles: Vec<&str> = tiles.iter().map(|(_, title)| title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "DUPLICATE",
            "ONE SHOT",
            "HALF SPEED",
            "PUNCH IN",
            "OVERDUB",
            "PLAYING",
            "REVERSE",
            "REDO"
        ]
    );
    assert!(
        tiles
            .iter()
            .map(|(action, _)| *action)
            .eq(LooperAction::TILES)
    );
}

/// Looper X tiles: stock draws every tile in normal ink with no loop (shot 086; the device
/// drops the presses its gates refuse); REDO shows whenever redo exists; an undo in progress
/// leaves only UNDO.
#[test]
fn looper_tiles_follow_the_stock_gates() {
    let tiles = |looper: LooperView| {
        let mut snapshot = rich();
        snapshot.looper = looper;
        let mut ui = ui_with(snapshot);
        ui.navigate(Screen::Looper);
        ui.hit_regions()
            .into_iter()
            .filter_map(|region| match region.control {
                Control::Looper(action) => Some((action, region.label, region.enabled)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let stopped = LooperView {
        state: LooperState::Stopped,
        redo_available: true,
        ..LooperView::default()
    };
    let empty = tiles(stopped);
    assert!(empty.iter().all(|(_, _, on)| *on));
    assert_eq!(empty[7].1, "REDO");
    let undoing = tiles(LooperView {
        loop_seconds: Some(4.0),
        undo_in_progress: true,
        ..stopped
    });
    assert!(
        undoing
            .iter()
            .all(|(action, _, on)| *on == (*action == LooperAction::UndoRedo))
    );
}

/// The mini lock button flips Looper editor `+0x187`; a QC has no lock button.
#[test]
fn looper_lock_button_is_mini_only() {
    let mut snapshot = rich();
    let mut ui = ui_with(snapshot.clone());
    ui.navigate(Screen::Looper);
    assert!(activate(&mut ui, Control::LooperLock).is_empty());
    snapshot.looper.mini = true;
    snapshot.looper.editor_view = true;
    ui.set_snapshot(snapshot).unwrap();
    assert_eq!(
        activate(&mut ui, Control::LooperLock),
        vec![UiCommand::LooperView { shown: false }]
    );
}

/// Tempo panel cell n is encoder n: TEMPO nudges the BPM, VOLUME steps its setting.
#[test]
fn tempo_encoders_follow_the_panel_cells() {
    let mut snapshot = rich();
    snapshot
        .settings
        .insert(SettingId::MetronomeLevel, SettingValue::Number(-60));
    let mut ui = ui_with(snapshot);
    ui.navigate(Screen::Tempo);
    let mut turn = |knob| {
        ui.handle_input(InputEvent::Knob {
            knob: KnobIndex(knob),
            delta: 1,
        })
    };
    assert_eq!(turn(0), vec![UiCommand::NudgeTempo { steps: 1 }]);
    assert!(matches!(
        turn(3).as_slice(),
        [UiCommand::ChangeSetting {
            id: SettingId::MetronomeLevel,
            ..
        }]
    ));
    assert!(turn(5).is_empty());
}

/// A settings knob cell takes the stock knob drag law on its catalog position, rounded to
/// the setting's step: Tempo PAN (−100..100, linear) moves 1 per px; Global EQ FREQ
/// (20..20 000 Hz, skew 0.3) moves along its skewed scale.
#[test]
fn settings_knob_drag_follows_the_stock_law() {
    let band = EqBand::new(0).unwrap();
    let mut snapshot = rich();
    snapshot
        .settings
        .insert(SettingId::MetronomePan, SettingValue::Number(0));
    snapshot
        .settings
        .insert(SettingId::EqFreq(band), SettingValue::Number(1000));
    let mut ui = ui_with(snapshot);
    let up = |ui: &mut UiState, id| {
        let from = center(ui, Control::Setting(SettingControl::Open(id)));
        drag(ui, from, (from.0, from.1 - 16))
    };
    ui.navigate(Screen::Tempo);
    assert_eq!(
        up(&mut ui, SettingId::MetronomePan),
        vec![UiCommand::ChangeSetting {
            id: SettingId::MetronomePan,
            value: SettingValue::Number(16),
        }]
    );
    ui.navigate(Screen::GlobalEq(EqTab::Band(band)));
    assert_eq!(
        up(&mut ui, SettingId::EqFreq(band)),
        vec![UiCommand::ChangeSetting {
            id: SettingId::EqFreq(band),
            value: SettingValue::Number(1808),
        }]
    );
}

/// The Tempo page TEMPO knob follows a vertical drag, one BPM per pixel up (stock moved
/// 120 → 180 BPM for a 60 px drag on the QC, 2026-10-15); a tap on it changes nothing.
#[test]
fn tempo_knob_drag_nudges_the_tempo() {
    let mut ui = ui_with(rich());
    ui.navigate(Screen::Tempo);
    let from = center(&ui, Control::Tempo(TempoControl::Knob));
    assert_eq!(
        drag(&mut ui, from, (from.0, from.1 - 60)),
        vec![UiCommand::NudgeTempo { steps: 60 }]
    );
    assert!(tap(&mut ui, from.0, from.1).is_empty());
}

/// Stock's Tempo header has the ◀◀ A ▶▶ scene stepper left of ✓ (seen on the QC 2026-10-15).
#[test]
fn tempo_page_steps_scenes() {
    let mut ui = ui_with(rich());
    ui.navigate(Screen::Tempo);
    let current = ui.snapshot.selected_scene.get();
    let next = scene((current + 1) % SCENE_COUNT as u8);
    let (x, y) = center(&ui, Control::SelectScene(next));
    assert!(x > 600 && y < 60);
    assert_eq!(
        tap(&mut ui, x, y),
        vec![UiCommand::SelectScene { scene: next }]
    );
}

/// MIDI Clock Out on: stock paints the BPM red and appends " | MIDI Clock Out" to the caption
/// (`0x380924`..`0x3809d0`, seen on the QC 2026-10-15); off, the BPM stays the accent green.
#[test]
fn tempo_page_marks_midi_clock_out() {
    let color_of = |ui: &UiState, text: &str| {
        ui.layout().items.iter().find_map(|item| match item.look {
            crate::layout::Look::Text { color, .. } if item.label == text => Some(color),
            _ => None,
        })
    };
    let mut snapshot = rich();
    snapshot.tempo.bpm = 120.0;
    snapshot
        .settings
        .insert(SettingId::MidiClockOut, SettingValue::Choice(0));
    let mut ui = ui_with(snapshot.clone());
    ui.navigate(Screen::Tempo);
    assert_eq!(color_of(&ui, "120"), Some(Palette::STOCK.accent));
    assert!(
        !ui.layout()
            .items
            .iter()
            .any(|item| item.label.ends_with("| MIDI Clock Out"))
    );
    snapshot
        .settings
        .insert(SettingId::MidiClockOut, SettingValue::Choice(1));
    ui.set_snapshot(snapshot).unwrap();
    assert_eq!(color_of(&ui, "120"), Some(Palette::STOCK.danger));
    assert!(
        ui.layout()
            .items
            .iter()
            .any(|item| item.label.ends_with(" | MIDI Clock Out"))
    );
}

/// Stock 4.1.0 ⋮ menu (seen on the QC, 2026-10-15): a scrolling list on the right with FILE
/// and QUAD CORTEX headers; the first rows show at once, Settings only after a scroll.
#[test]
fn main_menu_is_a_scrolling_list_with_sections() {
    let mut ui = ui_with(rich());
    let shown = |ui: &UiState, action| {
        ui.hit_regions()
            .iter()
            .any(|region| region.enabled && region.control == Control::Menu(action))
    };
    ui.apply(Control::OpenMainMenu);
    assert!(ui.layout().items.iter().any(|item| item.label == "FILE"));
    assert!(shown(&ui, MenuAction::CreateNew));
    assert!(!shown(&ui, MenuAction::Settings));
    drag(&mut ui, (600, 400), (600, 40));
    assert!(
        ui.layout()
            .items
            .iter()
            .any(|item| item.label == "QUAD CORTEX")
    );
    drag(&mut ui, (600, 400), (600, 40));
    assert!(shown(&ui, MenuAction::Settings));
}

/// An available update: stock puts a green dot on ⋮ and on the menu's Settings row, on the
/// Settings group button outside System and on System's Updates row (seen on the QC,
/// 2026-10-15/16); no update, no dots.
#[test]
fn available_update_dots_the_menu_and_settings() {
    let green_dots = |ui: &UiState| {
        ui.layout()
            .items
            .iter()
            .filter(|item| {
                item.visible().is_some()
                    && matches!(item.look, crate::layout::Look::Dot { color } if color == Palette::STOCK.accent)
            })
            .count()
    };
    let mut snapshot = rich();
    let mut ui = ui_with(snapshot.clone());
    ui.apply(Control::OpenMainMenu);
    drag(&mut ui, (600, 400), (600, 40));
    drag(&mut ui, (600, 400), (600, 40));
    assert_eq!(green_dots(&ui), 0);
    snapshot.updater.available_version = Some("4.1.1".into());
    ui.set_snapshot(snapshot.clone()).unwrap();
    assert_eq!(green_dots(&ui), 2);
    for page in [SettingsPage::MyAccount, SettingsPage::Connection] {
        let mut ui = ui_with(snapshot.clone());
        ui.navigate(Screen::Settings(page));
        assert_eq!(green_dots(&ui), 1, "{page:?}");
    }
}

/// Gig rows (`0x678304`, `0x65dbcc`): a hybrid's first kind owns A–D (upper row), the second
/// E–H; a preset half addresses the bank's first four presets. Swipe up from the Grid's bottom
/// edge opens Gig View (manual §2).
#[test]
fn hybrid_first_kind_owns_a_to_d_and_swipe_up_opens_gig_view() {
    let mut snapshot = rich();
    snapshot.footswitch_mode = Some(FootswitchMode::Hybrid);
    snapshot.hybrid = HybridRows {
        upper: FootswitchMode::Stomp,
        lower: FootswitchMode::Preset,
    };
    snapshot.bank.presets = snapshot.presets[..8].to_vec();
    let mut ui = ui_with(snapshot);
    let target = |ui: &mut UiState, switch| match ui
        .handle_input(InputEvent::Footswitch {
            switch,
            pressed: true,
        })
        .as_slice()
    {
        [UiCommand::Footswitch { target, .. }] => *target,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        target(&mut ui, Footswitch::B),
        FootswitchTarget::Stomp(Footswitch::B)
    );
    assert_eq!(target(&mut ui, Footswitch::F), FootswitchTarget::Preset(1));
    assert!(drag(&mut ui, (400, 470), (400, 300)).is_empty());
    assert_eq!(ui.screen(), Screen::Gig);
    let a = center(&ui, Control::Tile(Footswitch::B)).1;
    let f = center(&ui, Control::SelectPreset(1)).1;
    assert!(a < f, "A–D tiles are above E–H");
}

/// The Grid scene badge opens the stock dropdown (manual "SCENES DROPDOWN"). A pick
/// closes it, as stock 4.1.0 does on the QC (2026-10-16; the manual says it stays
/// open); a tap outside or Back closes it too.
#[test]
fn scene_badge_opens_a_dropdown_that_a_pick_or_dismiss_closes() {
    let mut ui = ui_with(rich());
    let scene_rows = |ui: &UiState| {
        ui.hit_regions()
            .iter()
            .filter(|region| matches!(region.control, Control::SelectScene(_)))
            .count()
    };
    assert_eq!(scene_rows(&ui), 0, "no permanent scene strip");
    activate(&mut ui, Control::SceneMenu);
    assert_eq!(scene_rows(&ui), SCENE_COUNT);
    let (x, y) = center(&ui, Control::SelectScene(scene(2)));
    assert_eq!(
        tap(&mut ui, x, y),
        vec![UiCommand::SelectScene { scene: scene(2) }]
    );
    assert_eq!(scene_rows(&ui), 0, "a pick closes");
    activate(&mut ui, Control::SceneMenu);
    assert!(tap(&mut ui, 100, 300).is_empty(), "outside closes");
    assert_eq!(scene_rows(&ui), 0);
    activate(&mut ui, Control::SceneMenu);
    ui.handle_input(InputEvent::Back);
    assert_eq!(scene_rows(&ui), 0);
}

/// Stock 4.1.0 on the QC (`st5b.png`, 2026-10-16): the open scene dropdown has no pencil
/// (scene names are edited from Gig View), a tap anywhere on the active row picks it, and the
/// header badge turns grey (`#636163`) with no green frame around it.
/// The dropdown scene letter as stock draws it (`st5b`, row B): ink rows 115–130 (a 16 px cap)
/// and columns 476–487.
#[test]
fn scene_dropdown_letters_match_the_stock_cap() {
    let mut ui = ui_with(rich());
    activate(&mut ui, Control::SceneMenu);
    let frame = ui.render();
    // Inside the yellow badge only.
    let ink: Vec<(u32, u32)> = (474..490)
        .flat_map(|x| (112..134).map(move |y| (x, y)))
        .filter(|&(x, y)| {
            let p = frame.pixel(x, y);
            u32::from(p.0) + u32::from(p.1) + u32::from(p.2) < 200
        })
        .collect();
    let rows = (ink.iter().map(|p| p.1).min(), ink.iter().map(|p| p.1).max());
    let columns = (ink.iter().map(|p| p.0).min(), ink.iter().map(|p| p.0).max());
    assert_eq!(
        (rows, columns),
        ((Some(115), Some(130)), (Some(476), Some(487)))
    );
}

#[test]
fn the_open_scene_dropdown_has_no_pencil_and_greys_the_header_badge() {
    let mut ui = ui_with(rich());
    let menu = ui
        .hit_regions()
        .into_iter()
        .find(|region| region.control == Control::SceneMenu)
        .unwrap()
        .bounds;
    // Badge at +10 in the 44 px button; (2, 2) inside it is fill, left of it is the button.
    let (badge, frame) = ((menu.x + 12, menu.y + 12), (menu.x + 5, menu.y + 22));
    let pixel = |ui: &UiState, (x, y): (i32, i32)| ui.render().pixel(x as u32, y as u32);
    assert_eq!(pixel(&ui, badge), Palette::STOCK.warning);
    activate(&mut ui, Control::SceneMenu);
    assert_eq!(pixel(&ui, badge), Palette::STOCK.faint);
    assert_eq!(pixel(&ui, frame), Palette::STOCK.canvas);
    assert!(
        ui.hit_regions()
            .iter()
            .all(|region| region.control != Control::EditScenes)
    );
    let active = (ui.hit_regions().into_iter())
        .find(|region| region.control == Control::SelectScene(scene(0)))
        .unwrap()
        .bounds;
    let right = (active.right() - 4, active.y + active.height as i32 / 2);
    assert_eq!(
        tap(&mut ui, right.0, right.1),
        vec![UiCommand::SelectScene { scene: scene(0) }]
    );
}

/// Hold on an empty slot inserts the pair's Splitter, then its Mixer, and
/// nothing once both exist (manual "SPLITTER & MIXER BLOCKS").
#[test]
fn holding_an_empty_slot_inserts_split_then_mix() {
    let mut s = rich();
    s.selected_block = None;
    let mut ui = ui_with(s);
    let mut now = 0;
    let mut hold = |ui: &mut UiState, position| {
        let (x, y) = center(ui, Control::SelectCell(position));
        now += 1_000;
        ui.handle_input(InputEvent::Tick { now_ms: now });
        ui.handle_input(InputEvent::PointerDown { x, y });
        let commands = ui.handle_input(InputEvent::Tick {
            now_ms: now + crate::state::LONG_PRESS_MS,
        });
        assert!(ui.handle_input(InputEvent::PointerUp { x, y }).is_empty());
        commands
    };
    assert_eq!(
        hold(&mut ui, cell(3, 2)),
        vec![UiCommand::SetSplit {
            pair: RowPair::Lower,
            column: Some(2)
        }]
    );
    let mut s = ui.snapshot().clone();
    s.pairs[1].split = Some(2);
    ui.set_snapshot(s).unwrap();
    assert_eq!(
        hold(&mut ui, cell(2, 6)),
        vec![UiCommand::SetMix {
            pair: RowPair::Lower,
            column: Some(6)
        }]
    );
    let mut s = ui.snapshot().clone();
    s.pairs[1].mix = Some(6);
    ui.set_snapshot(s).unwrap();
    assert!(hold(&mut ui, cell(2, 4)).is_empty());
}

fn shows(ui: &UiState, control: Control) -> bool {
    ui.hit_regions()
        .iter()
        .any(|region| region.control == control)
}

/// Virtual Device List (shots 076/090): the 19 stock categories first, then a
/// rail with GUITAR / BASS tabs that split one group; tapping the open group
/// collapses it, and a tap on the dimmed Grid closes the drawer.
#[test]
fn device_list_groups_models_by_category_and_instrument_tab() {
    let mut snapshot = rich();
    {
        let mut models = snapshot.models.to_vec();
        models.push(ModelEntry {
            id: 2001,
            name: "BASS ONE".into(),
            category: ModelCategory::BassAmp,
        });
        snapshot.models = models.into();
    }
    snapshot.rows[1].input = InputPort::Input1;
    let mut ui = ui_with(snapshot);
    activate(&mut ui, Control::SelectCell(cell(1, 3)));
    // Every pickable category has a group; Grid-only ones have none.
    assert!(
        ModelCategory::PICKER
            .into_iter()
            .all(|category| DeviceGroup::of(category).is_some())
    );
    assert_eq!(DeviceGroup::of(ModelCategory::Splitter), None);
    assert!(shows(
        &ui,
        Control::Picker(PickerControl::Group(DeviceGroup::Plugins))
    ));
    assert!(!shows(&ui, Control::Picker(PickerControl::Model(1001))));

    let amp = Control::Picker(PickerControl::Group(DeviceGroup::Amp));
    activate(&mut ui, amp);
    assert!(shows(&ui, Control::Picker(PickerControl::Model(1001))));
    assert!(!shows(&ui, Control::Picker(PickerControl::Model(2001))));
    activate(
        &mut ui,
        Control::Picker(PickerControl::Instrument(Instrument::Bass)),
    );
    assert!(shows(&ui, Control::Picker(PickerControl::Model(2001))));
    assert!(!shows(&ui, Control::Picker(PickerControl::Model(1001))));
    activate(&mut ui, amp);
    assert!(!shows(&ui, Control::Picker(PickerControl::Model(2001))));

    // Step 1 is a right-hand drawer (shot sn-2): a tap on the Grid left of it closes it.
    assert!(tap(&mut ui, 200, 300).is_empty());
    assert_eq!(ui.screen(), Screen::Grid);
}

/// Virtual Device Presets (shots 093/040): a row tap auditions, Back reverts
/// and closes the pane, ✓ keeps and returns to the Grid; factory rows offer
/// only "Set as Default".
#[test]
fn device_presets_audition_keep_revert_and_guard_factory_rows() {
    let mut snapshot = rich();
    snapshot.blocks[0].model_id = Some(1001);
    snapshot.selected_block = Some(cell(0, 0));
    let preset = |id, name: &str, factory| DevicePresetItem {
        id,
        model_id: 1001,
        name: name.into(),
        factory,
        default: id == 1,
    };
    snapshot.device_presets = [preset(1, "DEFAULT", true), preset(9, "MINE", false)]
        .to_vec()
        .into();
    let mut ui = ui_with(snapshot);
    let picker = |control| Control::Picker(control);
    let request = |request| {
        vec![UiCommand::DevicePreset {
            position: cell(0, 0),
            request,
        }]
    };
    // Replacing opens the current device's group with its stack button.
    activate(&mut ui, Control::SelectCell(cell(0, 0)));
    activate(&mut ui, picker(PickerControl::Presets(1001)));
    assert_eq!(
        activate(&mut ui, picker(PickerControl::Preset(1))),
        request(DevicePresetRequest::Audition {
            model_id: 1001,
            preset: 1
        })
    );
    activate(&mut ui, picker(PickerControl::PresetMenu(1)));
    let overwrite = picker(PickerControl::PresetMenuItem(DevicePresetMenu::Overwrite));
    assert!(activate(&mut ui, overwrite).is_empty());
    assert_eq!(ui.handle_input(InputEvent::Back), Vec::new());
    assert_eq!(
        ui.handle_input(InputEvent::Back),
        request(DevicePresetRequest::Revert)
    );
    assert_eq!(ui.screen(), Screen::ModelPicker);
    assert!(!shows(&ui, picker(PickerControl::Preset(1))));

    activate(&mut ui, picker(PickerControl::Presets(1001)));
    activate(&mut ui, picker(PickerControl::PresetTab { user: true }));
    // Overwrite and Delete ask first with the stock texts (`0x39793c`, `0x397cb4`).
    let primary = Control::Dialog(DialogButton::Primary);
    let says = |ui: &UiState, text: &str| ui.layout().items.iter().any(|i| i.label == text);
    for (item, title, text, edit) in [
        (
            DevicePresetMenu::Overwrite,
            "Virtual Device Preset Overwrite",
            "Are you sure you want to overwrite the Virtual Device Preset 'MINE' with the \
             current parameter settings?",
            DevicePresetRequest::Overwrite(9),
        ),
        (
            DevicePresetMenu::Delete,
            "MINE",
            "Are you sure you want to delete this Virtual Device Preset?",
            DevicePresetRequest::Delete(9),
        ),
    ] {
        activate(&mut ui, picker(PickerControl::PresetMenu(9)));
        assert!(activate(&mut ui, picker(PickerControl::PresetMenuItem(item))).is_empty());
        assert!(says(&ui, title) && says(&ui, text), "{item:?}");
        assert_eq!(activate(&mut ui, primary), request(edit));
    }
    activate(&mut ui, picker(PickerControl::Preset(9)));
    assert_eq!(
        activate(&mut ui, picker(PickerControl::KeepPreset)),
        request(DevicePresetRequest::Keep)
    );
    assert_eq!(ui.screen(), Screen::Grid);
}

/// Expression assignment (shot 068): ASSIGN links to the shown pedal and
/// UNASSIGN removes it; the bypass row shows only while BYPASS is linked;
/// Back reverts and leaves.
#[test]
fn expression_assign_toggles_links_per_pedal_and_back_reverts() {
    let mut snapshot = rich();
    snapshot.selected_block = Some(cell(0, 0));
    snapshot.expression = vec![ExpressionLink {
        pedal: ExpressionPedal::Exp2,
        target: ExpressionTarget::Parameter(0),
    }];
    let mut ui = ui_with(snapshot);
    ui.open_expression(cell(0, 0));
    assert_eq!(ui.screen(), Screen::Expression);
    let expression = |request| {
        vec![UiCommand::Expression {
            position: cell(0, 0),
            request,
        }]
    };
    let assign = |target| Control::Expression(ExpressionControl::Assign(target));
    let link = |pedal, linked| ExpressionRequest::Link {
        pedal,
        target: ExpressionTarget::Parameter(0),
        linked,
    };
    let first = assign(ExpressionTarget::Parameter(0));
    assert_eq!(
        activate(&mut ui, first),
        expression(link(ExpressionPedal::Exp1, true))
    );
    activate(
        &mut ui,
        Control::Expression(ExpressionControl::Pedal(ExpressionPedal::Exp2)),
    );
    assert_eq!(
        activate(&mut ui, first),
        expression(link(ExpressionPedal::Exp2, false))
    );

    let switch_on = Control::Expression(ExpressionControl::SwitchOn);
    assert!(!shows(&ui, switch_on));
    let mut snapshot = ui.snapshot().clone();
    snapshot.expression.push(ExpressionLink {
        pedal: ExpressionPedal::Exp2,
        target: ExpressionTarget::Bypass,
    });
    ui.set_snapshot(snapshot).unwrap();
    assert_eq!(
        activate(&mut ui, switch_on),
        expression(ExpressionRequest::SwitchOn(ExpressionSwitch::Switch))
    );

    assert_eq!(
        ui.handle_input(InputEvent::Back),
        expression(ExpressionRequest::Revert)
    );
    assert_eq!(ui.screen(), Screen::Grid);
}

/// The media picker library row lists every item of the picked kind.
#[test]
fn media_library_row_lists_all_items_of_the_kind() {
    let mut snapshot = rich();
    snapshot.selected_block = Some(cell(0, 0));
    snapshot.parameters[0].kind = ParameterKind::Media(MediaKind::Ir);
    let entry = |folder, name: &str| MediaEntry {
        folder,
        name: name.into(),
        value: name.into(),
    };
    snapshot.media = vec![
        entry(MediaFolder::FactoryIrs, "F"),
        entry(MediaFolder::MyIrs, "M"),
        entry(MediaFolder::MyCaptures, "C"),
    ]
    .into();
    let mut ui = ui_with(snapshot);
    activate(
        &mut ui,
        Control::Param(ParamControl::Knob(KnobIndex::new(0).unwrap())),
    );
    activate(&mut ui, Control::Media(MediaControl::Library));
    let item = |index| Control::Media(MediaControl::Item(index));
    assert!(shows(&ui, item(0)) && shows(&ui, item(1)));
    assert!(!shows(&ui, item(2)));
}

/// Power & Locking (`0x7d5690`, slot `0x6a852c`): a full-size POWER press shows the
/// popup (again) and restarts a 3 s guard (`0x6a862c`), an open menu blocks it, LOCK drops touch until a 1 s hold of
/// repeated presses (a 350 ms gap cancels), Be Right Back blanks the screen until a press.
#[test]
fn power_overlay_locks_touch_until_a_power_hold() {
    let mut ui = ui_with(rich());
    let power = |ui: &mut UiState| ui.handle_input(InputEvent::PowerButton { pressed: true });
    let at = |ui: &mut UiState, now_ms| ui.handle_input(InputEvent::Tick { now_ms });
    activate(&mut ui, Control::OpenMainMenu);
    power(&mut ui);
    assert!(!shows(&ui, Control::Power(PowerControl::Lock)));
    ui.handle_input(InputEvent::Back);
    power(&mut ui);
    assert!(shows(&ui, Control::Power(PowerControl::BeRightBack)));
    at(&mut ui, 2_999);
    power(&mut ui);
    assert!(ui.overlay_open(), "the 3 s guard ignores the press");
    at(&mut ui, 3_000);
    power(&mut ui);
    assert!(
        ui.overlay_open(),
        "review r11 N16: full-size does not toggle"
    );
    assert_eq!(
        activate(&mut ui, Control::Power(PowerControl::Lock)),
        vec![UiCommand::SetLocked { locked: true }]
    );
    assert!(ui.locked() && !ui.overlay_open());
    let (x, y) = center(&ui, Control::SelectCell(cell(0, 0)));
    assert!(tap(&mut ui, x, y).is_empty());
    at(&mut ui, 10_000);
    power(&mut ui);
    assert!(at(&mut ui, 10_350).is_empty());
    for now_ms in [10_400, 10_700, 11_000, 11_300] {
        at(&mut ui, now_ms);
        assert!(power(&mut ui).is_empty());
    }
    assert!(at(&mut ui, 11_399).is_empty() && ui.locked());
    assert_eq!(
        at(&mut ui, 11_400),
        vec![UiCommand::SetLocked { locked: false }]
    );
    assert!(!tap(&mut ui, x, y).is_empty());

    at(&mut ui, 15_000);
    power(&mut ui);
    assert_eq!(
        activate(&mut ui, Control::Power(PowerControl::Shutdown)),
        vec![UiCommand::Device {
            action: DeviceAction::Shutdown
        }]
    );
    at(&mut ui, 18_000);
    power(&mut ui);
    assert_eq!(
        activate(&mut ui, Control::Power(PowerControl::BeRightBack)),
        vec![UiCommand::BeRightBack { active: true }]
    );
    assert!(ui.away());
    assert!(ui.render().rgb().iter().all(|byte| *byte == 0));
    at(&mut ui, 21_000);
    assert_eq!(
        power(&mut ui),
        vec![UiCommand::BeRightBack { active: false }]
    );
}

/// Mini POWER (`0x6a852c`, `0x7d4844`): the slot acts on the release and toggles the
/// popup (`0x6a862c`), one press unlocks, and the Master Volume overlay stays 2 s
/// (`0x500024`).
#[test]
fn mini_power_acts_on_release_and_one_press_unlocks() {
    let mut ui = ui_with(UiSnapshot {
        mini: true,
        ..rich()
    });
    let power = |ui: &mut UiState, pressed| ui.handle_input(InputEvent::PowerButton { pressed });
    power(&mut ui, true);
    assert!(!ui.overlay_open());
    power(&mut ui, false);
    power(&mut ui, true);
    power(&mut ui, false);
    assert!(!ui.overlay_open(), "the mini toggles the popup");
    power(&mut ui, true);
    power(&mut ui, false);
    activate(&mut ui, Control::Power(PowerControl::Lock));
    assert!(power(&mut ui, true).is_empty());
    assert_eq!(
        power(&mut ui, false),
        vec![UiCommand::SetLocked { locked: false }]
    );
    ui.handle_input(InputEvent::Tick { now_ms: 1_000 });
    ui.handle_input(InputEvent::MasterVolume { percent: 40 });
    ui.handle_input(InputEvent::Tick { now_ms: 2_999 });
    assert!(ui.overlay_open());
    ui.handle_input(InputEvent::Tick { now_ms: 3_000 });
    assert!(!ui.overlay_open());
}

/// Master Volume overlay: a turn shows the level; tiles toggle the output assignment; it
/// closes 1 s (full-size, `0x500024`) after the last turn or touch and stays shut while
/// locked.
#[test]
fn master_volume_overlay_assigns_outputs_and_times_out() {
    let mut snapshot = rich();
    snapshot
        .settings
        .insert(SettingId::MasterVolumeOut34, SettingValue::Bool(false));
    let mut ui = ui_with(snapshot);
    ui.handle_input(InputEvent::Tick { now_ms: 1_000 });
    ui.handle_input(InputEvent::MasterVolume { percent: 75 });
    assert!(ui.overlay_open());
    let tile = Control::Setting(SettingControl::Open(SettingId::MasterVolumeOut34));
    ui.handle_input(InputEvent::Tick { now_ms: 1_900 });
    let (x, y) = center(&ui, tile);
    assert_eq!(
        tap(&mut ui, x, y),
        vec![UiCommand::ChangeSetting {
            id: SettingId::MasterVolumeOut34,
            value: SettingValue::Bool(true),
        }]
    );
    ui.handle_input(InputEvent::Tick { now_ms: 2_800 });
    assert!(ui.overlay_open(), "a touch restarts the timeout");
    ui.handle_input(InputEvent::Tick { now_ms: 2_900 });
    assert!(!ui.overlay_open());

    ui.handle_input(InputEvent::PowerButton { pressed: true });
    activate(&mut ui, Control::Power(PowerControl::Lock));
    ui.handle_input(InputEvent::MasterVolume { percent: 20 });
    assert!(!ui.overlay_open());
}

/// Modes configuration ⓘ (`0x2d4f00`): the MIDI recall help opens over the page and
/// Back returns to the page.
#[test]
fn modes_info_opens_midi_help_over_the_page() {
    let mut ui = ui_with(rich());
    activate(&mut ui, Control::OpenMainMenu);
    activate(&mut ui, Control::Menu(MenuAction::ModesConfiguration));
    let slot = Control::Mode(ModeControl::Slot(0));
    activate(&mut ui, Control::Mode(ModeControl::Info));
    assert!(!shows(&ui, slot));
    let layout = ui.layout();
    assert!(
        layout.items[layout.layer..]
            .iter()
            .any(|item| item.label.contains("CC#47 value 2"))
    );
    ui.handle_input(InputEvent::Back);
    assert!(shows(&ui, slot));
}

/// Modes configuration (manual §3): drag onto a slot merges a hybrid, drag into the row
/// reorders, drag to the top right removes (one slot stays), hold splits a hybrid, the
/// right edge swaps its rows, and the add menu appends a missing mode.
#[test]
fn modes_configuration_drags_merge_reorder_remove_and_split() {
    use FootswitchMode::{Hybrid, Preset, Scene, Stomp};
    let cycle = |modes: Vec<FootswitchMode>, upper, lower| UiCommand::SetModeCycle {
        modes,
        hybrid: HybridRows { upper, lower },
    };
    let mut ui = ui_with(rich());
    activate(&mut ui, Control::OpenMainMenu);
    activate(&mut ui, Control::Menu(MenuAction::ModesConfiguration));
    assert_eq!(ui.led_state(), (false, false, true, false));
    let slot = |ui: &UiState, index| center(ui, Control::Mode(ModeControl::Slot(index)));
    let rows = ui.snapshot().hybrid;
    let (preset, scene, stomp) = (slot(&ui, 0), slot(&ui, 1), slot(&ui, 2));
    assert_eq!(
        drag(&mut ui, preset, scene),
        vec![cycle(vec![Hybrid, Stomp], Scene, Preset)]
    );
    assert_eq!(
        drag(&mut ui, stomp, (preset.0 - 100, preset.1)),
        vec![cycle(vec![Stomp, Preset, Scene], rows.upper, rows.lower)]
    );
    assert_eq!(
        drag(&mut ui, preset, (700, 60)),
        vec![cycle(vec![Scene, Stomp], rows.upper, rows.lower)]
    );

    let mut hybrid = rich();
    hybrid.allowed_modes = vec![Hybrid, Preset];
    hybrid.hybrid = HybridRows {
        upper: Scene,
        lower: Stomp,
    };
    ui.set_snapshot(hybrid).unwrap();
    assert_eq!(
        activate(&mut ui, Control::Mode(ModeControl::SwapRows)),
        vec![cycle(vec![Hybrid, Preset], Stomp, Scene)]
    );
    let (x, y) = slot(&ui, 0);
    ui.handle_input(InputEvent::Tick { now_ms: 1_000 });
    ui.handle_input(InputEvent::PointerDown { x: x - 40, y });
    // A 750 ms hold (`LONG_PRESS_MS`); the test used 500 ms before the stock value.
    assert_eq!(
        ui.handle_input(InputEvent::Tick { now_ms: 1_750 }),
        vec![cycle(vec![Scene, Stomp, Preset], Scene, Stomp)]
    );
    assert!(
        ui.handle_input(InputEvent::PointerUp { x: x - 40, y })
            .is_empty()
    );

    let mut single = rich();
    single.allowed_modes = vec![Preset];
    ui.set_snapshot(single).unwrap();
    let only = slot(&ui, 0);
    assert!(drag(&mut ui, only, (700, 60)).is_empty());
    activate(&mut ui, Control::Mode(ModeControl::Add));
    assert_eq!(
        activate(&mut ui, Control::Mode(ModeControl::Insert(Stomp))),
        vec![cycle(vec![Preset, Stomp], rows.upper, rows.lower)]
    );
    assert!(ui.led_state().2);
}

/// Capture ✕ (`0x003cd75c`): the stock quit text for the wizard state, anchored at the ✕;
/// OK aborts and closes the screen, a tap outside keeps the capture.
#[test]
fn capture_quit_confirm_uses_the_stock_text_in_place() {
    let mut snapshot = rich();
    snapshot.capture.step = CaptureStep::Training;
    let mut ui = ui_with(snapshot);
    ui.show(Screen::Capture);
    let close = Control::Capture(CaptureRequest::Abort);
    let (x, y) = center(&ui, close);
    tap(&mut ui, x, y);
    let ok = ui
        .hit_regions()
        .into_iter()
        .find(|region| region.control == Control::Dialog(DialogButton::Primary))
        .unwrap();
    assert_eq!(ok.label, "OK");
    assert!(
        ok.bounds.y > y && ok.bounds.y < y + 200,
        "card opens under the ✕"
    );
    assert!(tap(&mut ui, 40, 400).is_empty());
    assert_eq!(ui.screen(), Screen::Capture);
    tap(&mut ui, x, y);
    assert_eq!(
        activate(&mut ui, Control::Dialog(DialogButton::Primary))[0],
        UiCommand::Capture {
            request: CaptureRequest::Abort
        }
    );
    assert_ne!(ui.screen(), Screen::Capture);
}

#[test]
fn directory_keeps_a_named_empty_preset_and_uses_real_occupancy_for_list_and_search() {
    let mut snapshot = rich();
    snapshot.presets = vec![
        PresetItem {
            id: 0,
            title: "Empty".into(),
        },
        PresetItem {
            id: 1,
            title: "Empty".into(),
        },
    ];
    std::sync::Arc::make_mut(&mut snapshot.directory).slots = vec![
        DirectorySlot {
            title: "Empty".into(),
            id: 0,
            key: 0,
            empty: false,
            author: "Player".into(),
            date: String::new(),
        },
        DirectorySlot {
            title: "Empty".into(),
            id: 1,
            key: 0,
            empty: true,
            author: String::new(),
            date: String::new(),
        },
    ];
    let mut ui = ui_with(snapshot);
    activate(&mut ui, Control::Navigate(Screen::Presets));
    activate(&mut ui, Control::Library(LibraryControl::Sort));
    activate(&mut ui, Control::Choose(1));
    let rows = ui.hit_regions();
    assert!(
        rows.iter()
            .any(|row| row.control == Control::SelectPreset(0))
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.control == Control::SelectPreset(1))
    );
    activate(&mut ui, Control::Library(LibraryControl::Search));
    for ch in "Empty".chars() {
        activate(&mut ui, Control::Key(Key::Char(ch)));
    }
    activate(&mut ui, Control::Key(Key::Done));
    let tab = ui
        .hit_regions()
        .into_iter()
        .find(|row| row.control == Control::Library(LibraryControl::Kind(ItemKind::Preset)))
        .unwrap();
    assert_eq!(tab.label, "(1)");
}

/// Stock `lessThan` (`0x755258`, `0x542628`): natural case-insensitive names, an equal
/// author falls back to the name, and a second tap flips the order.
#[test]
fn directory_sorts_follow_the_stock_natural_order_and_role_fallbacks() {
    let mut snapshot = rich();
    let rows = [("Preset 10", "B"), ("preset 9", "B"), ("Alpha", "C")];
    snapshot.presets = rows
        .iter()
        .zip(0..)
        .map(|((title, _), id)| PresetItem {
            id,
            title: (*title).into(),
        })
        .collect();
    std::sync::Arc::make_mut(&mut snapshot.directory).slots = rows
        .iter()
        .zip(0..)
        .map(|((title, author), id)| DirectorySlot {
            title: (*title).into(),
            id,
            key: 0,
            empty: false,
            author: (*author).into(),
            date: String::new(),
        })
        .collect();
    let mut ui = ui_with(snapshot);
    activate(&mut ui, Control::Navigate(Screen::Presets));
    let order = |ui: &UiState| {
        let mut ids: Vec<u64> = (0..3).collect();
        ids.sort_by_key(|id| center(ui, Control::SelectPreset(*id)).1);
        ids
    };
    let sort = |ui: &mut UiState, choice| {
        activate(ui, Control::Library(LibraryControl::Sort));
        activate(ui, Control::Choose(choice));
    };
    sort(&mut ui, 1);
    assert_eq!(order(&ui), [2, 1, 0], "Name: 9 before 10, case ignored");
    sort(&mut ui, 2);
    assert_eq!(
        order(&ui),
        [1, 0, 2],
        "Author: equal authors fall back to the name"
    );
    sort(&mut ui, 2);
    assert_eq!(order(&ui), [2, 0, 1], "a second tap flips the whole order");
}

/// Preset ⋮ Copy and Move open one page with every writable setlist and its slots: an empty
/// slot takes the preset at once, an occupied one asks first. SWAP is only for a move inside
/// the browsed setlist (the Directory move); the rest is a one-preset PASTE.
#[test]
fn directory_preset_copy_and_move_choose_any_setlist_and_slot() {
    let mut snapshot = rich();
    snapshot.setlists = ["Factory Presets", "My Presets", "Tour"]
        .into_iter()
        .enumerate()
        .map(|(id, title)| SetlistItem {
            id: id as u64,
            title: title.into(),
        })
        .collect();
    snapshot.active_setlist = Some(1);
    // My Presets holds slots 0 and 1, Tour slot 1.
    std::sync::Arc::make_mut(&mut snapshot.directory).presets = [(1, 0), (1, 1), (2, 1)]
        .into_iter()
        .map(|(setlist, slot)| DirectoryPreset {
            setlist,
            slot,
            title: format!("PRESET {slot}"),
        })
        .collect();
    let mut ui = ui_with(snapshot);
    activate(&mut ui, Control::Navigate(Screen::Presets));
    let library = Control::Library;
    let open = |ui: &mut UiState, action| {
        activate(ui, Control::PresetMenu(0));
        activate(ui, library(LibraryControl::Item(action)))
    };
    let paste = |remove, setlist, slot| {
        vec![UiCommand::CopyPresets {
            ids: vec![0],
            from: "My Presets".into(),
            setlist,
            remove,
            to: PasteTarget::Slots(vec![slot]),
        }]
    };
    let slots = |ui: &UiState| {
        (ui.hit_regions().into_iter())
            .filter(|region| {
                matches!(
                    region.control,
                    Control::Library(LibraryControl::TargetSlot(_))
                )
            })
            .count()
    };

    // Copy to an empty slot of another setlist; Factory Presets is not offered.
    open(&mut ui, ItemAction::Copy);
    assert!(slots(&ui) > 0, "the page shows the slots");
    assert!(!(ui.hit_regions().iter()).any(|region| region.label == "Factory Presets"));
    activate(&mut ui, library(LibraryControl::TargetSetlist(2)));
    assert_eq!(
        activate(&mut ui, library(LibraryControl::TargetSlot(3))),
        paste(false, 2, 3)
    );
    assert_eq!(slots(&ui), 0, "the page closes");

    // Move onto an occupied slot of another setlist: no SWAP, OVERWRITE pastes.
    open(&mut ui, ItemAction::Move);
    activate(&mut ui, library(LibraryControl::TargetSetlist(2)));
    assert!(activate(&mut ui, library(LibraryControl::TargetSlot(1))).is_empty());
    let answer = |overwrite| library(LibraryControl::ConfirmMove { overwrite });
    assert!(activate(&mut ui, answer(false)).is_empty());
    assert_eq!(activate(&mut ui, answer(true)), paste(true, 2, 1));

    // Move inside the browsed setlist: the own slot is not a target, an occupied slot asks
    // SWAP or OVERWRITE, an empty one moves.
    let mv = |to, overwrite| {
        vec![UiCommand::Directory(DirectoryRequest::MovePreset {
            setlist: "My Presets".into(),
            from: 0,
            to,
            overwrite,
        })]
    };
    open(&mut ui, ItemAction::Move);
    assert!(activate(&mut ui, library(LibraryControl::TargetSlot(0))).is_empty());
    assert!(activate(&mut ui, library(LibraryControl::TargetSlot(1))).is_empty());
    assert_eq!(activate(&mut ui, answer(false)), mv(1, false));
    open(&mut ui, ItemAction::Move);
    assert_eq!(
        activate(&mut ui, library(LibraryControl::TargetSlot(5))),
        mv(5, false)
    );

    // Back closes the question first, then the page.
    open(&mut ui, ItemAction::Copy);
    activate(&mut ui, library(LibraryControl::TargetSlot(1)));
    activate(&mut ui, Control::Back);
    assert!(slots(&ui) > 0);
    activate(&mut ui, Control::Back);
    assert_eq!(slots(&ui), 0);
}

/// Plugin folders list their presets per sidebar folder; a row auditions the preset.
#[test]
fn directory_plugin_preset_rows_audition_the_selected_folder() {
    let mut snapshot = rich();
    let item = |folder: &str, name: &str| DirectoryItem {
        legacy_index: None,
        kind: ItemKind::Preset,
        folder: folder.into(),
        value: format!("/opt/neuraldsp/Plugins/Owned/{folder}/{name}.pb"),
        name: name.into(),
        author: String::new(),
        date: String::new(),
        instrument: PreferredInstrument::NotSet,
    };
    std::sync::Arc::make_mut(&mut snapshot.directory).plugins = vec![DirectoryPlugin {
        name: "Owned".into(),
        path: "/opt/neuraldsp/Plugins/Owned".into(),
        read_only: true,
        children: vec!["Artists".into()],
        presets: vec![item("", "Default"), item("Artists/Someone", "Lead")],
    }];
    let mut ui = ui_with(snapshot);
    activate(&mut ui, Control::Navigate(Screen::Presets));
    activate(&mut ui, Control::Library(LibraryControl::Category));
    activate(&mut ui, Control::Choose(4));
    activate(&mut ui, Control::Library(LibraryControl::PluginFolder(0)));
    let rows = |ui: &UiState| {
        ui.hit_regions()
            .into_iter()
            .filter_map(|row| match row.control {
                Control::Library(LibraryControl::PluginPreset(index)) => Some(index),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(rows(&ui), [0]);
    activate(&mut ui, Control::Library(LibraryControl::PluginChild(0)));
    assert_eq!(rows(&ui), [1]);
    assert_eq!(
        activate(&mut ui, Control::Library(LibraryControl::PluginPreset(1))),
        vec![UiCommand::Directory(DirectoryRequest::Audition {
            kind: ItemKind::Preset,
            folder: "/opt/neuraldsp/Plugins/Owned".into(),
            id: "/opt/neuraldsp/Plugins/Owned/Artists/Someone/Lead.pb".into(),
        })]
    );
}

/// Plugin presets sort by the sidecar `instrument_type` code (`0x755258` role 0x24:
/// guitar 1 before bass 2), names breaking ties.
#[test]
fn directory_plugin_presets_sort_by_preferred_instrument_code() {
    let mut snapshot = rich();
    let item = |name: &str, instrument| DirectoryItem {
        legacy_index: None,
        kind: ItemKind::Preset,
        folder: String::new(),
        value: format!("/opt/neuraldsp/Plugins/Owned/{name}.pb"),
        name: name.into(),
        author: String::new(),
        date: String::new(),
        instrument,
    };
    std::sync::Arc::make_mut(&mut snapshot.directory).plugins = vec![DirectoryPlugin {
        name: "Owned".into(),
        path: "/opt/neuraldsp/Plugins/Owned".into(),
        read_only: true,
        children: Vec::new(),
        presets: vec![
            item("Alpha", PreferredInstrument::Bass),
            item("Beta", PreferredInstrument::Guitar),
            item("Gamma", PreferredInstrument::Bass),
        ],
    }];
    let mut ui = ui_with(snapshot);
    activate(&mut ui, Control::Navigate(Screen::Presets));
    activate(&mut ui, Control::Library(LibraryControl::Category));
    activate(&mut ui, Control::Choose(4));
    activate(&mut ui, Control::Library(LibraryControl::PluginFolder(0)));
    activate(&mut ui, Control::Library(LibraryControl::Sort));
    // Name, Author, Preferred Instrument, Date Added.
    activate(&mut ui, Control::Choose(2));
    let rows: Vec<u16> = ui
        .hit_regions()
        .into_iter()
        .filter_map(|r| match r.control {
            Control::Library(LibraryControl::PluginPreset(i)) => Some(i),
            _ => None,
        })
        .collect();
    assert_eq!(rows, [1, 0, 2]);
}

#[test]
fn directory_media_favorite_and_folder_delete_emit_stable_database_requests() {
    let mut snapshot = rich();
    snapshot.media = vec![MediaEntry {
        folder: MediaFolder::MyCaptures,
        name: "Owned Capture".into(),
        value: "owned-id".into(),
    }]
    .into();
    std::sync::Arc::make_mut(&mut snapshot.directory).media = vec![DirectoryMedia {
        value: "owned-id".into(),
        author: "Player".into(),
        date: String::new(),
        read_only: false,
        folders: vec![2],
        capture: None,
    }];
    std::sync::Arc::make_mut(&mut snapshot.directory).folders = vec![
        DirectoryFolder {
            id: 1,
            kind: MediaKind::Capture,
            name: "My Captures".into(),
            parent: 0,
            read_only: false,
            root: true,
        },
        DirectoryFolder {
            id: 2,
            kind: MediaKind::Capture,
            name: "Live".into(),
            parent: 1,
            read_only: false,
            root: false,
        },
    ];
    let mut ui = ui_with(snapshot);
    activate(&mut ui, Control::Navigate(Screen::Presets));
    activate(&mut ui, Control::Library(LibraryControl::Category));
    activate(&mut ui, Control::Choose(2));
    activate(&mut ui, Control::Library(LibraryControl::MediaMenu(0)));
    assert_eq!(
        activate(
            &mut ui,
            Control::Library(LibraryControl::Item(ItemAction::Favorite))
        ),
        vec![UiCommand::Directory(DirectoryRequest::FavoriteMedia {
            kind: MediaKind::Capture,
            folder: "local_nc_root".into(),
            id: "owned-id".into(),
            favorite: true
        })]
    );
    activate(
        &mut ui,
        Control::Library(LibraryControl::DatabaseFolderMenu(2)),
    );
    activate(
        &mut ui,
        Control::Library(LibraryControl::Item(ItemAction::Delete)),
    );
    assert_eq!(
        activate(
            &mut ui,
            Control::Library(LibraryControl::ConfirmDirectoryDelete {
                remove_products: false
            })
        ),
        vec![UiCommand::Directory(DirectoryRequest::DeleteFolder {
            id: 2,
            remove_products: false
        })]
    );
}

/// Capture/IR Multi Select (manual "BULK ACTIONS", "FOLDERS & NESTED NAVIGATION"): MOVE out
/// of a folder links into the target and unlinks from the open folder; DELETE in a folder can
/// only unlink (REMOVE); DELETE in the library root omits factory items (`0x83d208`).
#[test]
fn directory_multi_select_links_moves_and_deletes_captures() {
    let mut snapshot = rich();
    let entry = |name: &str, value: &str| MediaEntry {
        folder: MediaFolder::MyCaptures,
        name: name.into(),
        value: value.into(),
    };
    snapshot.media = vec![entry("Owned", "owned-id"), entry("Factory", "factory-id")].into();
    let details = |value: &str, read_only| DirectoryMedia {
        value: value.into(),
        author: String::new(),
        date: String::new(),
        read_only,
        folders: vec![2],
        capture: None,
    };
    std::sync::Arc::make_mut(&mut snapshot.directory).media =
        vec![details("owned-id", false), details("factory-id", true)];
    let folder = |id, name: &str, root| DirectoryFolder {
        id,
        kind: MediaKind::Capture,
        name: name.into(),
        parent: if root { 0 } else { 1 },
        read_only: false,
        root,
    };
    std::sync::Arc::make_mut(&mut snapshot.directory).folders = vec![
        folder(1, "My Captures", true),
        folder(2, "Live", false),
        folder(3, "Studio", false),
    ];
    let mut ui = ui_with(snapshot);
    let library = |control| Control::Library(control);
    activate(&mut ui, Control::Navigate(Screen::Presets));
    activate(&mut ui, library(LibraryControl::Category));
    activate(&mut ui, Control::Choose(2));
    let check = |ui: &mut UiState, rows: &[u64]| {
        activate(ui, library(LibraryControl::MultiSelect));
        for row in rows {
            activate(ui, library(LibraryControl::Check(*row)));
        }
    };
    let link = |folder, linked| UiCommand::LinkMedia {
        folder,
        ids: vec!["owned-id".into()],
        linked,
    };
    activate(&mut ui, library(LibraryControl::DatabaseFolder(2)));
    check(&mut ui, &[0]);
    activate(&mut ui, library(LibraryControl::Bulk(BulkAction::Move)));
    assert_eq!(
        activate(&mut ui, Control::Choose(0)),
        [link(3, true), link(2, false)]
    );
    check(&mut ui, &[0]);
    activate(&mut ui, library(LibraryControl::Bulk(BulkAction::Delete)));
    assert_eq!(
        activate(&mut ui, library(LibraryControl::ConfirmBulkRemove)),
        [link(2, false)]
    );

    activate(&mut ui, library(LibraryControl::Library));
    check(&mut ui, &[0, 1]);
    activate(&mut ui, library(LibraryControl::Bulk(BulkAction::Delete)));
    assert!(!shows(&ui, library(LibraryControl::ConfirmBulkRemove)));
    assert_eq!(
        activate(&mut ui, library(LibraryControl::ConfirmBulkDelete)),
        [UiCommand::Directory(DirectoryRequest::DeleteMedia {
            kind: MediaKind::Capture,
            id: "owned-id".into(),
        })]
    );
}

/// Capture folders "Live" (with the subfolder "Set") and "Studio" under the root; captures
/// a/b/c with device types 1/4/1, gains 7/2/9 and versions 2/1/1.
fn capture_folders() -> UiState {
    let mut snapshot = rich();
    let entry = |name: &str| MediaEntry {
        folder: MediaFolder::MyCaptures,
        name: name.into(),
        value: name.into(),
    };
    snapshot.media = vec![entry("a"), entry("b"), entry("c")].into();
    let folder = |id, name: &str, parent, root| DirectoryFolder {
        id,
        kind: MediaKind::Capture,
        name: name.into(),
        parent,
        read_only: false,
        root,
    };
    std::sync::Arc::make_mut(&mut snapshot.directory).folders = vec![
        folder(1, "My Captures", 0, true),
        folder(2, "Live", 1, false),
        folder(3, "Studio", 1, false),
        folder(4, "Set", 2, false),
    ];
    std::sync::Arc::make_mut(&mut snapshot.directory).saving_destinations = vec![3];
    std::sync::Arc::make_mut(&mut snapshot.directory).media =
        [("a", 1, 7, "2"), ("b", 4, 2, "1"), ("c", 1, 9, "1")]
            .map(|(key, device, gain, version)| DirectoryMedia {
                value: key.into(),
                author: String::new(),
                date: String::new(),
                read_only: false,
                folders: Vec::new(),
                capture: Some(CaptureMeta {
                    device: Some(device),
                    gain: Some(gain),
                    version: version.into(),
                    instrument: None,
                }),
            })
            .into();
    let mut ui = ui_with(snapshot);
    activate(&mut ui, Control::Navigate(Screen::Presets));
    activate(&mut ui, Control::Library(LibraryControl::Category));
    activate(&mut ui, Control::Choose(2));
    ui
}

/// Folder ⋮ menu (M817-821): DUPLICATE (gate `0x76692c`: not the root, under 31 folders with
/// the copy) and SET AS SAVING DESTINATION; the current destination's row does nothing.
#[test]
fn folder_menu_duplicates_and_sets_the_saving_destination() {
    let mut ui = capture_folders();
    let library = |control| Control::Library(control);
    let item = |action| library(LibraryControl::Item(action));
    activate(&mut ui, library(LibraryControl::DatabaseFolderMenu(2)));
    assert_eq!(
        activate(&mut ui, item(ItemAction::Duplicate)),
        [UiCommand::DuplicateMediaFolder { folder: 2 }]
    );
    activate(&mut ui, library(LibraryControl::DatabaseFolderMenu(2)));
    assert_eq!(
        activate(&mut ui, item(ItemAction::SavingDestination)),
        [UiCommand::SetSavingDestination { folder: 2 }]
    );
    activate(&mut ui, library(LibraryControl::DatabaseFolderMenu(3)));
    assert!(activate(&mut ui, item(ItemAction::SavingDestination)).is_empty());
    let duplicate = |ui: &mut UiState, folder| {
        activate(ui, library(LibraryControl::DatabaseFolderMenu(folder)));
        let enabled = (ui.hit_regions().iter())
            .any(|region| region.control == item(ItemAction::Duplicate) && region.enabled);
        activate(ui, Control::Back);
        enabled
    };
    assert!(!duplicate(&mut ui, 1), "the root");
    // 29 writable folders: "Studio" (1) still fits, "Live" with "Set" (2) does not.
    let mut snapshot = ui.snapshot().clone();
    std::sync::Arc::make_mut(&mut snapshot.directory)
        .folders
        .extend((10..35).map(|id| DirectoryFolder {
            id,
            kind: MediaKind::Capture,
            name: format!("F{id}"),
            parent: 1,
            read_only: false,
            root: false,
        }));
    ui.set_snapshot(snapshot).unwrap();
    assert!(duplicate(&mut ui, 3));
    assert!(!duplicate(&mut ui, 2));
}

/// Capture filter (M797, `DEVICE_TYPES` / `VERSIONS`): checked types OR, AND the version; the
/// Gain sort's jump bar scrolls to the first row with that gain or more (I).
#[test]
fn capture_filter_and_gain_jump_bar_follow_the_metadata() {
    let mut ui = capture_folders();
    let library = |control| Control::Library(control);
    let rows = |ui: &UiState| -> Vec<u16> {
        (0..3)
            .filter(|index| shows(ui, library(LibraryControl::Media(*index))))
            .collect()
    };
    // The rows take input again once the filter pop-up closes.
    let filter = |ui: &mut UiState, control| {
        activate(ui, library(LibraryControl::Filter));
        activate(ui, library(control));
        activate(ui, Control::Back);
        rows(ui)
    };
    assert_eq!(filter(&mut ui, LibraryControl::TypeFilter(1)), [0, 2]);
    assert_eq!(
        filter(&mut ui, LibraryControl::Version(CaptureVersion::V1)),
        [2]
    );
    assert_eq!(filter(&mut ui, LibraryControl::TypeFilter(4)), [1, 2]);
    filter(&mut ui, LibraryControl::TypeFilter(1));
    filter(&mut ui, LibraryControl::TypeFilter(4));
    assert_eq!(
        filter(&mut ui, LibraryControl::Version(CaptureVersion::All)),
        [0, 1, 2]
    );

    activate(&mut ui, library(LibraryControl::Sort));
    activate(&mut ui, Control::Choose(4));
    assert!(shows(&ui, library(LibraryControl::Jump(10))));
    // Gain order b (2), a (7), c (9): gain 8 jumps to row 2.
    activate(&mut ui, library(LibraryControl::Jump(8)));
    assert_eq!(
        ui.offset(crate::layout::ListId::DirectoryItems),
        2 * 60,
        "two 60 px rows"
    );
}

/// Control Screenshot (25, offscreen renderer `0x691040`): the stored preset's grid, 800 × 384
/// from y 96, transparent where nothing is drawn and equal to the Grid screen where opaque.
#[test]
fn preset_grid_is_the_grid_area_on_a_transparent_fill() {
    let mut snapshot = rich();
    snapshot.selected_block = Some(GridPosition::new(0, 2).unwrap());
    let rgba = UiState::preset_grid(snapshot.clone()).unwrap();
    assert_eq!(
        rgba.len(),
        (PRESET_GRID_WIDTH * PRESET_GRID_HEIGHT * 4) as usize
    );
    snapshot.selected_block = None;
    let screen = ui_with(snapshot).render();
    let (mut clear, mut opaque) = (0, 0);
    for (index, pixel) in rgba.chunks_exact(4).enumerate() {
        let (x, y) = (index as u32 % WIDTH, index as u32 / WIDTH + 96);
        match pixel[3] {
            0 => clear += 1,
            255 => {
                opaque += 1;
                let drawn = screen.pixel(x, y);
                assert_eq!(&pixel[..3], &[drawn.0, drawn.1, drawn.2], "({x}, {y})");
            }
            _ => {}
        }
    }
    assert!(
        clear > opaque && opaque > 1000,
        "{clear} clear, {opaque} opaque"
    );
}

/// Tempo beat marks (`0x0037e89c`, tempo parameter 10 + beat): 1 fills the dot black, 2 and
/// 3 put a small dot 24 px above or below the beat centre (shot 008).
#[test]
fn tempo_beat_accents_draw_the_stock_marks() {
    let mut snapshot = rich();
    snapshot.tempo.beats_per_bar = 4;
    snapshot.tempo.accents[..3].copy_from_slice(&[
        BeatAccent::Above,
        BeatAccent::Black,
        BeatAccent::Below,
    ]);
    let mut ui = ui_with(snapshot);
    ui.navigate(Screen::Tempo);
    let frame = ui.render();
    // Beat centres x = 208 + 128·i, y 213; marks at y 189 and 237.
    let at = |x, y| frame.pixel(x, y);
    assert_eq!(at(208, 189), Palette::STOCK.section, "mark above beat 1");
    assert_eq!(at(208, 237), Palette::STOCK.canvas);
    assert_eq!(at(336, 213), Palette::STOCK.canvas, "black beat 2");
    assert_eq!(at(464, 237), Palette::STOCK.section, "mark below beat 3");
    assert_eq!(at(592, 213), Palette::STOCK.section, "plain beat 4");
    assert_eq!(at(592, 189), Palette::STOCK.canvas);
}

/// Looper X after "Reassign": tile n shows the action stored for footswitch n, and picking a
/// tile in the swap dialog names its footswitch letter.
#[test]
fn looper_tiles_follow_the_stored_assignments() {
    let mut snapshot = rich();
    snapshot.looper.tiles.0.swap(0, 4);
    let mut ui = ui_with(snapshot);
    ui.navigate(Screen::Looper);
    let actions: Vec<LooperAction> = ui
        .hit_regions()
        .into_iter()
        .filter_map(|region| match region.control {
            Control::Looper(action) => Some(action),
            _ => None,
        })
        .collect();
    let mut expected = LooperAction::TILES;
    expected.swap(0, 4);
    assert_eq!(actions, expected);
    activate(&mut ui, Control::LooperMenu);
    activate(&mut ui, Control::Menu(MenuAction::ReassignLooperActions));
    assert_eq!(
        activate(&mut ui, Control::Looper(LooperAction::Record)),
        [UiCommand::LooperAssign { tile: Some(0) }]
    );
}

/// Stock `0x005a6004`: only an unlicensed plugin block gets the lock, at (4, 54) of its slot.
#[test]
fn grid_draws_a_lock_on_an_unlicensed_plugin_block_only() {
    let locks = |ui: &UiState| {
        ui.layout()
            .items
            .into_iter()
            .filter(|item| {
                matches!(
                    item.look,
                    crate::layout::Look::Glyph {
                        icon: tetrad_assets::Icon::Lock,
                        ..
                    }
                )
            })
            .map(|item| item.bounds)
            .collect::<Vec<_>>()
    };
    let mut snapshot = rich();
    snapshot.blocks[0].model_id = Some(1001);
    assert!(locks(&ui_with(snapshot.clone())).is_empty());
    snapshot.locked_models.insert(1001);
    let ui = ui_with(snapshot);
    let slot = ui
        .hit_regions()
        .into_iter()
        .find(|region| region.control == Control::SelectCell(cell(0, 0)))
        .unwrap()
        .bounds;
    // Stock (4, 54, 24, 24) in the 82 px block item, whose tile sits at (9, 9).
    assert_eq!(locks(&ui), [Rect::new(slot.x - 5, slot.y + 45, 24, 24)]);
}

/// Stock `0x005a6004` and `0x002eb624` (`3d-worship-v5`): a plugin block gets its tag
/// ("NLY" for plugin 5, white on the amp red) at (19, −4, 32, 16) of its slot and a tile fill
/// of its colour at 30 % (`#4c0c0c`, `#4a0c08` on the 565 screen); a plain block gets neither.
#[test]
fn grid_draws_the_plugin_tag_and_fill_on_a_plugin_block_only() {
    let mut snapshot = rich();
    for (index, plugin) in [Some(5), None].into_iter().enumerate() {
        snapshot.grid[index].label = "AMP".into();
        snapshot.blocks[index] = BlockInfo {
            category: Some(ModelCategory::GuitarAmp),
            model_id: Some(1171),
            plugin,
        };
    }
    let ui = ui_with(snapshot);
    let items = ui.layout().items;
    let tile = |column| {
        let item = items
            .iter()
            .find(|item| item.control == Some(Control::SelectCell(cell(0, column))))
            .unwrap();
        match item.look {
            crate::layout::Look::Block { fill, .. } => (item.bounds, fill),
            _ => panic!("not a block"),
        }
    };
    let tags: Vec<_> = items
        .iter()
        .filter(|item| matches!(item.look, crate::layout::Look::Text { .. }))
        .filter(|item| item.label == "NLY")
        .map(|item| (item.bounds, item.look.clone()))
        .collect();
    let (slot, fill) = tile(0);
    let tag = Rect::new(slot.x + 19, slot.y - 4, 32, 16);
    assert_eq!(fill, tetrad_assets::Rgb(0x4c, 0x0c, 0x0c));
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].0, tag);
    assert!(matches!(
        tags[0].1,
        crate::layout::Look::Text { color, .. } if color == Palette::STOCK.text
    ));
    assert!(items.iter().any(|item| item.bounds == tag
        && matches!(
            item.look,
            crate::layout::Look::Panel {
                fill: tetrad_assets::block::AMP,
                ..
            }
        )));
    assert_eq!(tile(1).1, Palette::STOCK.canvas);
}

/// Census #2: a new device notice opens the MessageDialog once; while another modal shows
/// it waits, and OK closes it. The same notice id never opens again.
#[test]
fn device_notice_opens_the_message_dialog_once() {
    let mut ui = ui_with(rich());
    ui.overlay = Some(crate::state::Overlay::Modes);
    let mut snapshot = ui.snapshot().clone();
    snapshot.notice = Some(NoticeView {
        id: 1,
        title: "Preset".into(),
        body: "refused".into(),
        button: "OK",
    });
    ui.set_snapshot(snapshot.clone()).unwrap();
    assert_eq!(ui.overlay, Some(crate::state::Overlay::Modes));
    activate(&mut ui, Control::Back);
    ui.handle_input(InputEvent::Tick { now_ms: 10 });
    assert!(matches!(
        ui.overlay,
        Some(crate::state::Overlay::Notice { ref body, .. }) if body == "refused"
    ));
    activate(&mut ui, Control::Dialog(DialogButton::Primary));
    ui.set_snapshot(snapshot).unwrap();
    assert!(!ui.overlay_open());
}

/// `0x46485c`: a Directory upload whose name is on the account asks "Preset already exists"
/// with the stock text. OVERWRITE answers `true`; CANCEL and Back answer `false`; the dialog
/// closes when the device no longer asks.
#[test]
fn the_upload_overwrite_dialog_answers_the_device() {
    let mut ui = ui_with(rich());
    let ask = |ui: &mut UiState, name: Option<&str>| {
        let mut snapshot = ui.snapshot().clone();
        snapshot.upload_overwrite = name.map(str::to_owned);
        ui.set_snapshot(snapshot).unwrap();
    };
    let says = |ui: &UiState, text: &str| ui.layout().items.iter().any(|i| i.label == text);
    ask(&mut ui, Some("Lead"));
    assert!(says(&ui, "Preset already exists"));
    assert!(says(&ui, "Preset Lead already exists. Overwrite?"));
    assert!(says(&ui, "OVERWRITE") && says(&ui, "CANCEL"));
    assert_eq!(
        activate(&mut ui, Control::Dialog(DialogButton::Primary)),
        vec![UiCommand::UploadOverwrite { overwrite: true }]
    );
    let no = vec![UiCommand::UploadOverwrite { overwrite: false }];
    ask(&mut ui, None);
    ask(&mut ui, Some("Lead"));
    assert_eq!(activate(&mut ui, Control::Dialog(DialogButton::Cancel)), no);
    ask(&mut ui, None);
    ask(&mut ui, Some("Lead"));
    assert_eq!(ui.handle_input(InputEvent::Back), no);
    ask(&mut ui, None);
    ask(&mut ui, Some("Lead"));
    ask(&mut ui, None);
    assert!(!ui.overlay_open());
}

/// re-r34 §1, §2 (`0x465178`, C texts): a capture download or upload that waits asks "Neural
/// Capture already exists". Only OVERWRITE answers `true`; CANCEL and Back
/// answer `false` once. The questions are level-triggered, one at a time, and close when the
/// device no longer asks.
#[test]
fn the_capture_overwrite_dialogs_answer_the_device() {
    let mut ui = ui_with(rich());
    let ask = |ui: &mut UiState, download: bool, upload: bool| {
        let mut snapshot = ui.snapshot().clone();
        snapshot.capture_overwrite = download.then(|| CaptureQuestion {
            name: "Cloud Lead".into(),
            existing: "My Lead".into(),
        });
        snapshot.capture_upload_overwrite = upload.then(|| "Crunch".into());
        ui.set_snapshot(snapshot).unwrap();
    };
    let says = |ui: &UiState, text: &str| ui.layout().items.iter().any(|i| i.label == text);
    let download_text = "Neural Capture Cloud Lead already exists on your Quad Cortex (My Lead). \
                         Would you like to overwrite it?";
    let upload_text =
        "Neural Capture Crunch already exists in your account. Would you like to overwrite it?";
    let download = |overwrite| vec![UiCommand::CaptureOverwrite { overwrite }];
    let upload = |overwrite| vec![UiCommand::CaptureUploadOverwrite { overwrite }];

    ask(&mut ui, true, true);
    assert!(says(&ui, "Neural Capture already exists") && says(&ui, upload_text));
    assert!(says(&ui, "OVERWRITE") && says(&ui, "CANCEL"));
    assert_eq!(
        activate(&mut ui, Control::Dialog(DialogButton::Primary)),
        upload(true)
    );
    ui.handle_input(InputEvent::Tick { now_ms: 10 });
    assert!(says(&ui, download_text), "the next question shows");
    assert_eq!(ui.handle_input(InputEvent::Back), download(false));
    ui.handle_input(InputEvent::Tick { now_ms: 20 });
    assert!(!ui.overlay_open(), "answered questions do not come back");

    ask(&mut ui, false, false);
    ask(&mut ui, true, false);
    assert_eq!(
        activate(&mut ui, Control::Dialog(DialogButton::Cancel)),
        download(false)
    );
    ask(&mut ui, false, false);
    ask(&mut ui, false, true);
    assert_eq!(
        activate(&mut ui, Control::Dialog(DialogButton::Cancel)),
        upload(false)
    );
    ask(&mut ui, false, false);
    ask(&mut ui, true, false);
    ask(&mut ui, false, false);
    assert!(
        !ui.overlay_open(),
        "the dialog closes when the device stops asking"
    );
}

/// Review r31 D2: the upload status shows while the device uploads and goes with the result.
#[test]
fn the_upload_status_shows_until_the_result() {
    let mut ui = ui_with(rich());
    let shows = |ui: &UiState, text: &str| (ui.layout().items.iter()).any(|i| i.label == text);
    let mut snapshot = ui.snapshot().clone();
    snapshot.uploading = Some("Presets");
    ui.set_snapshot(snapshot.clone()).unwrap();
    assert!(shows(&ui, "Uploading Presets, please wait."));
    // Review r36 H4: a capture upload names its own kind.
    snapshot.uploading = Some("Neural Captures");
    ui.set_snapshot(snapshot.clone()).unwrap();
    assert!(shows(&ui, "Uploading Neural Captures, please wait."));
    snapshot.uploading = None;
    ui.set_snapshot(snapshot).unwrap();
    assert!(!(ui.layout().items.iter()).any(|i| i.label.starts_with("Uploading")));
}

/// re-r38 §2 (`0x785dcc`, `0x81b74c`, `0x77fef0`, `0x463d2c`): a batch upload shows the stock
/// progress dialog (no status pill) with the bar at handled / total, and its Cancel stops the
/// batch (`0x780390`). A batch question has the batch text and OVERWRITE / SKIP.
#[test]
fn a_batch_upload_shows_the_progress_dialog_and_batch_questions() {
    let mut ui = ui_with(rich());
    let says = |ui: &UiState, text: &str| ui.layout().items.iter().any(|i| i.label == text);
    let mut snapshot = ui.snapshot().clone();
    snapshot.uploading = Some("Neural Captures");
    snapshot.upload_progress = Some(UploadProgress {
        noun: "Neural Captures",
        handled: 2,
        total: 4,
    });
    ui.set_snapshot(snapshot.clone()).unwrap();
    let layout = ui.layout();
    assert!(says(&ui, "Uploading Neural Captures, please wait."));
    assert!(!(layout.items.iter()).any(|i| i.look == crate::layout::Look::Toast));
    let bar = Rect::new(178 + 54, 171 + 90, 168, 8);
    assert!(layout.items.iter().any(|i| i.bounds == bar), "half the bar");
    assert_eq!(tap(&mut ui, 400, 313), vec![UiCommand::CancelUpload]);

    snapshot.upload_batch = true;
    snapshot.capture_upload_overwrite = Some("Crunch".into());
    ui.set_snapshot(snapshot.clone()).unwrap();
    assert!(says(
        &ui,
        "Some of the neural captures you selected already exist in your account. Would you \
         like to overwrite them?"
    ));
    assert!(says(&ui, "OVERWRITE") && says(&ui, "SKIP"));
    assert_eq!(
        activate(&mut ui, Control::Dialog(DialogButton::Cancel)),
        vec![UiCommand::CaptureUploadOverwrite { overwrite: false }]
    );
    snapshot.capture_upload_overwrite = None;
    snapshot.upload_overwrite = Some("Lead".into());
    ui.set_snapshot(snapshot).unwrap();
    assert!(says(&ui, "Overwrite Existing Presets?"));
    assert!(says(
        &ui,
        "Some of the Presets you are trying to upload already exist in your account."
    ));
}

/// Review r31 D1: a device dialog that opens over the upload question does not drop it. The
/// question shows again when that dialog closes, and it never covers an open dialog.
#[test]
fn a_device_dialog_does_not_drop_the_upload_question() {
    let mut ui = ui_with(rich());
    let asks = |ui: &UiState| {
        ui.layout()
            .items
            .iter()
            .any(|i| i.label == "Preset already exists")
    };
    ui.show_excessive_downloads(ExcessDownloads::Captures);
    let mut snapshot = ui.snapshot().clone();
    snapshot.upload_overwrite = Some("Lead".into());
    ui.set_snapshot(snapshot).unwrap();
    assert!(!asks(&ui), "the open dialog stays");
    ui.handle_input(InputEvent::Back);
    assert!(asks(&ui), "the question shows when the dialog closes");
    ui.show_excessive_downloads(ExcessDownloads::Captures);
    ui.handle_input(InputEvent::Back);
    assert!(asks(&ui));
    assert_eq!(
        activate(&mut ui, Control::Dialog(DialogButton::Cancel)),
        vec![UiCommand::UploadOverwrite { overwrite: false }]
    );
    ui.handle_input(InputEvent::Tick { now_ms: 10 });
    assert!(!asks(&ui), "an answered question does not come back");
}

/// Manual "SCENE ASSIGNMENTS": holding a parameter knob toggles its scene assignment; the
/// release does not also act as a tap.
#[test]
fn holding_a_parameter_toggles_its_scene_assignment() {
    let mut snapshot = UiSnapshot {
        selected_block: Some(cell(0, 0)),
        ..rich()
    };
    snapshot.parameters[1].scene = true;
    let mut ui = ui_with(snapshot);
    for (knob, enabled) in [(0, true), (1, false)] {
        let (x, y) = center(&ui, Control::Param(ParamControl::Knob(KnobIndex(knob))));
        let now = u64::from(knob) * 2000;
        ui.handle_input(InputEvent::Tick { now_ms: now });
        ui.handle_input(InputEvent::PointerDown { x, y });
        let held = ui.handle_input(InputEvent::Tick {
            now_ms: now + crate::state::LONG_PRESS_MS,
        });
        assert_eq!(
            held,
            vec![UiCommand::SetSceneMode {
                position: cell(0, 0),
                parameter: u16::from(knob) * 2,
                enabled,
            }]
        );
        assert!(ui.handle_input(InputEvent::PointerUp { x, y }).is_empty());
    }
}

/// Stock value arc (`0x004c1e20`, shots h-650 and h-850): a scene-assigned knob draws white
/// and an expression-linked knob green; expression wins, as stock tests it first. The sample
/// sits on the arc at 199° (radius 32.3), inside the 135°–270° sweep of a 0.5 knob.
#[test]
fn knob_arc_colour_follows_scene_and_expression_assignment() {
    let mut snapshot = UiSnapshot {
        selected_block: Some(cell(0, 0)),
        ..rich()
    };
    snapshot.blocks[0].category = Some(ModelCategory::GuitarAmp);
    snapshot.parameters = vec![continuous(4, "VOLUME", 0.5)];
    let arc = |snapshot: UiSnapshot| {
        let ui = ui_with(snapshot);
        let cell = ui
            .hit_regions()
            .into_iter()
            .find(|region| region.control == Control::Param(ParamControl::Knob(KnobIndex(0))))
            .expect("knob 0 is visible")
            .bounds;
        // The knob centre is 42 px left of the cell's right edge and 71 px below its top.
        let (cx, cy) = (cell.right() - 42, cell.y + 71);
        ui.render().pixel((cx - 31) as u32, (cy - 11) as u32)
    };
    assert_eq!(arc(snapshot.clone()), ModelCategory::GuitarAmp.hue());
    snapshot.parameters[0].scene = true;
    assert_eq!(arc(snapshot.clone()), Palette::STOCK.text);
    snapshot.expression = vec![ExpressionLink {
        pedal: ExpressionPedal::Exp1,
        target: ExpressionTarget::Parameter(4),
    }];
    assert_eq!(arc(snapshot), Palette::STOCK.accent);
}

/// Census #5 (`0x3b5424`): in Stomp mode the editor's footswitch button opens "Assign
/// footswitch"; a footswitch press assigns the block; UNASSIGN shows only when assigned.
#[test]
fn footswitch_assignment_dialog_assigns_and_unassigns() {
    let snapshot = UiSnapshot {
        selected_block: Some(cell(0, 0)),
        footswitch_mode: Some(FootswitchMode::Stomp),
        ..rich()
    };
    let mut ui = ui_with(snapshot.clone());
    activate(&mut ui, Control::Param(ParamControl::Stomp));
    assert!(ui.assign_mode(), "the device blocks footswitch actions");
    assert!(
        !ui.hit_regions()
            .iter()
            .any(|region| region.control == Control::Dialog(DialogButton::Primary)),
        "no UNASSIGN without an assignment"
    );
    let pressed = ui.handle_input(InputEvent::Footswitch {
        switch: Footswitch::C,
        pressed: true,
    });
    assert_eq!(
        pressed,
        vec![UiCommand::AssignStomp {
            position: cell(0, 0),
            tile: Some(2),
        }]
    );
    assert!(!ui.overlay_open() && !ui.assign_mode());

    // Review r14 U-1: a preset load or a changed block closes the dialog.
    for change in [
        |s: &mut UiSnapshot| s.preset_id = Some(8),
        |s: &mut UiSnapshot| s.blocks[0].model_id = Some(6002),
    ] {
        activate(&mut ui, Control::Param(ParamControl::Stomp));
        let mut next = snapshot.clone();
        change(&mut next);
        ui.set_snapshot(next).unwrap();
        assert!(!ui.overlay_open());
        ui.set_snapshot(snapshot.clone()).unwrap();
    }

    let mut ui = ui_with(UiSnapshot {
        selected_stomp: Some(2),
        ..snapshot
    });
    activate(&mut ui, Control::Param(ParamControl::Stomp));
    assert_eq!(
        activate(&mut ui, Control::Dialog(DialogButton::Primary)),
        vec![UiCommand::AssignStomp {
            position: cell(0, 0),
            tile: None,
        }]
    );
}

/// Gig View EDIT STOMP: a block chip toggles its assignment to the stomp; Momentary sets
/// the stored flag.
#[test]
fn gig_edit_stomp_assigns_blocks_and_momentary() {
    let mut snapshot = UiSnapshot {
        footswitch_mode: Some(FootswitchMode::Stomp),
        ..rich()
    };
    snapshot.stomps[1].blocks = vec![cell(0, 0)];
    let mut ui = ui_with(snapshot);
    swipe_up_to_gig(&mut ui);
    let (x, y) = center(&ui, Control::Stomp(StompEdit::Open(1)));
    tap(&mut ui, x, y);
    let chip = Control::Stomp(StompEdit::Block {
        tile: 1,
        position: cell(0, 0),
    });
    center(&ui, chip);
    assert_eq!(
        activate(&mut ui, chip),
        vec![UiCommand::AssignStomp {
            position: cell(0, 0),
            tile: None,
        }]
    );
    assert_eq!(
        activate(
            &mut ui,
            Control::Stomp(StompEdit::Momentary {
                tile: 1,
                momentary: true,
            })
        ),
        vec![UiCommand::SetMomentary {
            tile: 1,
            momentary: true,
        }]
    );
}

/// Census #15 (`0x34d9b8`): a linked parameter cell shows MIN RANGE / MAX RANGE; the encoder
/// under column 1 steps MIN by 1 % and clamps at 0.
#[test]
fn expression_range_knobs_step_the_selected_link() {
    let mut snapshot = UiSnapshot {
        selected_block: Some(cell(0, 0)),
        ..rich()
    };
    snapshot.expression = vec![ExpressionLink {
        pedal: ExpressionPedal::Exp1,
        target: ExpressionTarget::Parameter(2),
    }];
    snapshot.expression_ranges = vec![ExpressionRangeView {
        parameter: 2,
        min: 1,
        max: 90,
    }];
    let mut ui = ui_with(snapshot);
    ui.navigate(Screen::Expression);
    activate(&mut ui, Control::Expression(ExpressionControl::Select(2)));
    center(
        &ui,
        Control::Expression(ExpressionControl::Range { max: true }),
    );
    let range = |min, max| {
        vec![UiCommand::Expression {
            position: cell(0, 0),
            request: ExpressionRequest::Range {
                parameter: 2,
                min,
                max,
            },
        }]
    };
    let turn = InputEvent::Knob {
        knob: KnobIndex(1),
        delta: -3,
    };
    assert_eq!(ui.handle_input(turn), range(0, 90));
    assert_eq!(
        activate(
            &mut ui,
            Control::Expression(ExpressionControl::Range { max: true })
        ),
        range(1, 100)
    );
}

/// Manual "Virtual Device Presets": two taps on one continuous knob restore its default;
/// taps on two knobs do not.
#[test]
fn double_tap_on_a_parameter_restores_its_default() {
    let mut ui = ui_with(UiSnapshot {
        selected_block: Some(cell(0, 0)),
        ..rich()
    });
    let knob = |index| Control::Param(ParamControl::Knob(KnobIndex(index)));
    assert!(activate(&mut ui, knob(0)).is_empty());
    assert!(activate(&mut ui, knob(1)).is_empty());
    ui.handle_input(InputEvent::Tick { now_ms: 100 });
    assert_eq!(
        activate(&mut ui, knob(1)),
        vec![UiCommand::DevicePreset {
            position: cell(0, 0),
            request: DevicePresetRequest::RestoreDefault { parameter: 2 },
        }]
    );
}

/// Manual "INPUT & OUTPUT BLOCKS": holding an input tile opens INPUT GATE CONTROL; an output
/// routed to another row has no editor. The site editor's knobs nudge the site.
#[test]
fn holding_a_port_opens_its_lane_editor() {
    let mut snapshot = rich();
    snapshot.rows[1].output = OutputPort::NextRow3;
    let mut ui = ui_with(snapshot);
    let hold = |ui: &mut UiState, control, now| {
        let (x, y) = center(ui, control);
        ui.handle_input(InputEvent::Tick { now_ms: now });
        ui.handle_input(InputEvent::PointerDown { x, y });
        let held = ui.handle_input(InputEvent::Tick {
            now_ms: now + crate::state::LONG_PRESS_MS,
        });
        ui.handle_input(InputEvent::PointerUp { x, y });
        held
    };
    let row = |index| Row::new(index).unwrap();
    let port = |index, output| Control::Port {
        row: row(index),
        output,
    };
    assert_eq!(
        hold(&mut ui, port(0, false), 0),
        vec![UiCommand::InspectSite {
            site: EditorSite::InputGate(row(0)),
        }]
    );
    assert!(hold(&mut ui, port(1, true), 2000).is_empty());

    let mut snapshot = ui.snapshot().clone();
    snapshot.selected_site = Some(EditorSite::LaneOutput(row(0)));
    ui.set_snapshot(snapshot).unwrap();
    ui.overlay = None;
    assert_eq!(
        ui.handle_input(InputEvent::Knob {
            knob: KnobIndex(0),
            delta: 1,
        }),
        vec![UiCommand::NudgeSite {
            site: EditorSite::LaneOutput(row(0)),
            parameter: 0,
            steps: 1,
        }]
    );
}

/// Census #19 (manual "SEARCHING ITEMS"): preset search spans every setlist; a result opens
/// its setlist with the preset highlighted instead of loading it.
#[test]
fn preset_search_spans_setlists_and_reveals_the_result() {
    let mut snapshot = rich();
    snapshot.setlists = vec![
        SetlistItem {
            id: 0,
            title: "LIVE".into(),
        },
        SetlistItem {
            id: 1,
            title: "STUDIO".into(),
        },
    ];
    std::sync::Arc::make_mut(&mut snapshot.directory).presets = vec![
        DirectoryPreset {
            setlist: 0,
            slot: 0,
            title: "Clean".into(),
        },
        DirectoryPreset {
            setlist: 1,
            slot: 9,
            title: "Crunch Lead".into(),
        },
    ];
    let mut ui = ui_with(snapshot);
    ui.navigate(Screen::Presets);
    activate(&mut ui, Control::Library(LibraryControl::Search));
    ui.handle_input(InputEvent::TextChanged("lead".into()));
    activate(&mut ui, Control::Key(Key::Done));
    let result = Control::Library(LibraryControl::RevealPreset(1));
    let label = ui
        .hit_regions()
        .into_iter()
        .find(|region| region.control == result)
        .map(|region| region.label);
    assert_eq!(label.as_deref(), Some("Crunch Lead"));
    assert_eq!(
        activate(&mut ui, result),
        vec![UiCommand::OpenSetlist { id: 1 }]
    );
    assert_eq!(ui.directory.revealed, Some(9));
    assert_eq!(ui.directory.search, None);
}

/// M528 (`0x4c62d4`, `0x64b774`): the value text opens the numeric keyboard; ✓ accepts only a
/// number inside the range; the request carries the typed text for the application.
#[test]
fn numeric_keyboard_enters_a_value_in_range() {
    let mut snapshot = UiSnapshot {
        selected_block: Some(cell(0, 0)),
        ..rich()
    };
    snapshot.parameters[0].kind = ParameterKind::Continuous { position: 0.5 };
    snapshot.parameters[0].entry = Some(NumericRange {
        value: 5.0,
        min: 0.0,
        max: 10.0,
        integer: false,
    });
    let mut ui = ui_with(snapshot);
    let (x, y) = center(&ui, Control::Param(ParamControl::Value(KnobIndex(0))));
    assert!(tap(&mut ui, x, y).is_empty());
    let key = |ui: &mut UiState, key| activate(ui, Control::Numeric(key));
    // The first key replaces the start text "5.00"; 12 is out of range.
    for digit in [1, 2] {
        key(&mut ui, NumericKey::Digit(digit));
    }
    assert!(key(&mut ui, NumericKey::Done).is_empty());
    assert!(
        key(&mut ui, NumericKey::Minus).is_empty(),
        "no sign key above 0"
    );
    for code in [
        NumericKey::Backspace,
        NumericKey::Point,
        NumericKey::Digit(5),
    ] {
        key(&mut ui, code);
    }
    assert_eq!(
        key(&mut ui, NumericKey::Done),
        vec![UiCommand::EnterParameter {
            target: ParameterTarget::Block(cell(0, 0)),
            parameter: 0,
            text: "1.5".into(),
        }]
    );
    assert!(ui.numeric.is_none());
}

/// M433 (`0x38cb90`, `0x38ce7c`): holding a Virtual Device row pins it, a second hold unpins
/// it; pinned models list first in their category.
#[test]
fn holding_a_device_row_pins_it_to_the_top() {
    let mut snapshot = rich();
    {
        let mut models = snapshot.models.to_vec();
        models.push(ModelEntry {
            id: 1003,
            name: "AAA AMP".into(),
            category: ModelCategory::GuitarAmp,
        });
        snapshot.models = models.into();
    }
    snapshot.rows[3].input = InputPort::Input1;
    let mut ui = ui_with(snapshot.clone());
    activate(&mut ui, Control::SelectCell(cell(3, 7)));
    activate(
        &mut ui,
        Control::Picker(PickerControl::Group(DeviceGroup::Amp)),
    );
    let row = |ui: &UiState, id| center(ui, Control::Picker(PickerControl::Model(id)));
    assert!(row(&ui, 1003).1 < row(&ui, 1001).1, "by name");
    let hold = |ui: &mut UiState, now: u64| {
        let (x, y) = row(ui, 1001);
        ui.handle_input(InputEvent::Tick { now_ms: now });
        ui.handle_input(InputEvent::PointerDown { x, y });
        let held = ui.handle_input(InputEvent::Tick {
            now_ms: now + crate::state::LONG_PRESS_MS,
        });
        assert!(ui.handle_input(InputEvent::PointerUp { x, y }).is_empty());
        held
    };
    assert_eq!(
        hold(&mut ui, 1000),
        vec![UiCommand::PinModel {
            model: 1001,
            pinned: true
        }]
    );
    snapshot.pinned_models = vec![1001];
    ui.set_snapshot(snapshot).unwrap();
    assert!(row(&ui, 1001).1 < row(&ui, 1003).1, "pinned first");
    assert_eq!(
        hold(&mut ui, 3000),
        vec![UiCommand::PinModel {
            model: 1001,
            pinned: false
        }]
    );
}

/// M1190 (`DeviceNameDialog` `0x40efe4`, `0x64fd2c`): the pencil opens the name keyboard
/// with the current name; an empty name saves the stock default.
#[test]
fn device_name_edit_saves_the_default_when_empty() {
    let mut snapshot = rich();
    snapshot.about.device_name = Some("Stage Left".into());
    let mut ui = ui_with(snapshot);
    ui.navigate(Screen::Settings(SettingsPage::DeviceInformation));
    activate(&mut ui, Control::Setting(SettingControl::EditDeviceName));
    assert_eq!(ui.keyboard.as_ref().unwrap().text, "Stage Left");
    ui.handle_input(InputEvent::TextChanged(String::new()));
    assert_eq!(
        activate(&mut ui, Control::Key(Key::Done)),
        vec![UiCommand::SetDeviceName {
            name: "Neural DSP Quad Cortex".into()
        }]
    );
}

/// Support pages: a licence row asks for its stock file (`pcutils` reads `pciutils`, `0x6c9434`)
/// and Back drops it; Footswitch Statistics counts presses, releases and turns (`0x68b7d0`).
#[test]
fn support_pages_request_licences_and_count_switches() {
    let mut ui = UiState::new();
    ui.navigate(Screen::Settings(SettingsPage::Licenses));
    assert_eq!(crate::screens::licence_file("pcutils"), "pciutils");
    assert_eq!(
        activate(
            &mut ui,
            Control::Setting(SettingControl::Licence("binutils"))
        ),
        vec![UiCommand::ShowLicence {
            package: Some("binutils".into())
        }]
    );
    assert_eq!(
        ui.handle_input(InputEvent::Back),
        vec![UiCommand::ShowLicence { package: None }]
    );
    assert_eq!(ui.screen(), Screen::Settings(SettingsPage::Licenses));
    for pressed in [true, false] {
        ui.handle_input(InputEvent::Footswitch {
            switch: Footswitch::BankDown,
            pressed,
        });
    }
    ui.handle_input(InputEvent::BankEncoder { delta: -2 });
    let stats = ui.switch_stats;
    assert_eq!((stats[4].pressed, stats[4].released), (1, 1));
    assert_eq!((stats[10].left, stats[10].right), (2, 0));
}

/// re-r44 §3: the UI cloud gate `0x669580` is Wi-Fi internet **or** Cortex Control connected
/// and online (the cloud calls then go over Control); `0x7166d0` also needs an account.
#[test]
fn cloud_is_ready_over_cortex_control_without_wifi() {
    let mut snapshot = UiSnapshot::default();
    snapshot.account.username = Some("player@example.com".into());
    assert!(!snapshot.cloud_ready(), "no Wi-Fi, no Control");
    snapshot.control_online = true;
    assert!(snapshot.cloud_ready());
    snapshot.account.username = None;
    assert!(!snapshot.cloud_ready(), "signed out");
}

/// Review r45 M6 and r47 N3: the cloud dialog `0x5b0958` (C) asks Cortex Control first, by
/// its link state (connected `0x6e0444`, KeepAlive online `0x6e045c`), not by the cloud route.
/// Connected and online with no cloud (for example a bring-up run without `--control-cloud`):
/// "Login via Cortex Control failed."; no Wi-Fi internet and Control connected but offline:
/// "No Control Internet Connection". Both have only OK.
#[test]
fn cloud_gate_names_the_cortex_control_cases() {
    use crate::screens::CloudGate;
    let dialog = |snapshot: &UiSnapshot| {
        let gate = CloudGate::of(snapshot).expect("cloud not ready");
        (gate.title(), gate.body(), gate.button())
    };
    let mut snapshot = UiSnapshot::default();
    assert_eq!(CloudGate::of(&snapshot), Some(CloudGate::NoWifi));
    snapshot.control_link = ControlLinkState::Offline;
    let offline = "Your Internet connection is offline. Check your network.";
    assert_eq!(
        dialog(&snapshot),
        ("No Control Internet Connection", offline, ("OK", None))
    );
    snapshot.wifi.status = WifiStatus::InternetConnected;
    assert_eq!(CloudGate::of(&snapshot), Some(CloudGate::LoggedOut));
    snapshot.wifi.status = WifiStatus::Idle;
    snapshot.control_link = ControlLinkState::Online;
    assert!(!snapshot.control_online, "no cloud route");
    let failed = "Login via Cortex Control failed.";
    assert_eq!(dialog(&snapshot), ("Unknown Error", failed, ("OK", None)));
}

/// re-r44 §1: stock 4.1.0 has no IR upload. Multi Select UPLOAD shows only in a preset or
/// capture folder (`0x77e6b0`) while the cloud is usable (`0x7166d0`, `0x782f14`); Tetrad draws
/// the row, so the button is disabled (O). Replaces review r21 H7 (`UploadItem::Ir`).
#[test]
fn multi_select_upload_is_off_in_ir_folders() {
    let mut snapshot = rich();
    snapshot.wifi.status = WifiStatus::InternetConnected;
    snapshot.account.username = Some("player@example.com".into());
    let entry = |folder, value: &str| MediaEntry {
        folder,
        name: value.into(),
        value: value.into(),
    };
    let details = |value: &str| DirectoryMedia {
        value: value.into(),
        author: String::new(),
        date: String::new(),
        read_only: false,
        folders: Vec::new(),
        capture: None,
    };
    snapshot.media = vec![
        entry(MediaFolder::MyIrs, "ir"),
        entry(MediaFolder::MyCaptures, "nc"),
    ]
    .into();
    std::sync::Arc::make_mut(&mut snapshot.directory).media = vec![details("ir"), details("nc")];
    let library = Control::Library;
    let upload = library(LibraryControl::Bulk(BulkAction::Upload));
    let upload_on = |ui: &UiState| {
        (ui.hit_regions().into_iter())
            .find(|region| region.control == upload)
            .is_some_and(|region| region.enabled)
    };
    // `Check` takes the `snapshot.media` index.
    let checked_in = |choice, row| {
        let mut ui = ui_with(snapshot.clone());
        activate(&mut ui, Control::Navigate(Screen::Presets));
        activate(&mut ui, library(LibraryControl::Category));
        activate(&mut ui, Control::Choose(choice));
        activate(&mut ui, library(LibraryControl::MultiSelect));
        activate(&mut ui, library(LibraryControl::Check(row)));
        ui
    };
    let mut irs = checked_in(3, 0);
    assert!(!upload_on(&irs) && activate(&mut irs, upload).is_empty());
    let mut captures = checked_in(2, 1);
    assert!(upload_on(&captures));
    captures.snapshot.account.username = None;
    assert!(!upload_on(&captures), "signed out");
    captures.snapshot.account.username = Some("player@example.com".into());
    assert_eq!(
        activate(&mut captures, upload),
        [UiCommand::Upload {
            items: vec![UploadItem::Capture("nc".into())],
        }]
    );
}

/// Review r15 R4: checked captures follow their product id when the list changes under
/// Multi Select (a capture saved meanwhile), so DELETE removes the checked capture only.
#[test]
fn checked_captures_follow_their_product_when_the_list_changes() {
    let mut snapshot = rich();
    let entry = |value: &str| MediaEntry {
        folder: MediaFolder::MyCaptures,
        name: value.into(),
        value: value.into(),
    };
    let details = |value: &str| DirectoryMedia {
        value: value.into(),
        author: String::new(),
        date: String::new(),
        read_only: false,
        folders: Vec::new(),
        capture: None,
    };
    snapshot.media = vec![entry("a"), entry("b")].into();
    std::sync::Arc::make_mut(&mut snapshot.directory).media = ["a", "b", "new"].map(details).into();
    let mut ui = ui_with(snapshot.clone());
    let library = Control::Library;
    activate(&mut ui, Control::Navigate(Screen::Presets));
    activate(&mut ui, library(LibraryControl::Category));
    activate(&mut ui, Control::Choose(2));
    activate(&mut ui, library(LibraryControl::MultiSelect));
    activate(&mut ui, library(LibraryControl::Check(1)));
    {
        let mut media = snapshot.media.to_vec();
        media.insert(0, entry("new"));
        snapshot.media = media.into();
    }
    ui.set_snapshot(snapshot).unwrap();
    activate(&mut ui, library(LibraryControl::Bulk(BulkAction::Delete)));
    assert_eq!(
        activate(&mut ui, library(LibraryControl::ConfirmBulkDelete)),
        [UiCommand::Directory(DirectoryRequest::DeleteMedia {
            kind: MediaKind::Capture,
            id: "b".into(),
        })]
    );
}

/// Health r34 #1: a Delete confirm acts on the item it was opened on. A list refresh moves a
/// capture or a setlist to another position, so the confirm follows its key; when the item is
/// gone (or a preset slot now names another preset) the confirm closes and sends nothing.
#[test]
fn row_confirms_keep_their_item_when_the_list_changes() {
    use crate::screens::Category;
    let library = Control::Library;
    let delete = library(LibraryControl::Item(ItemAction::Delete));
    let confirm = library(LibraryControl::ConfirmDirectoryDelete {
        remove_products: false,
    });
    let mut snapshot = media_snapshot(&[
        (MediaFolder::MyCaptures, "A", "k1"),
        (MediaFolder::MyCaptures, "B", "k2"),
    ]);
    let setlists = |titles: &[&str]| {
        (titles.iter().enumerate())
            .map(|(id, title)| SetlistItem {
                id: id as u64,
                title: (*title).into(),
            })
            .collect()
    };
    snapshot.setlists = setlists(&["My Presets", "A", "B"]);
    snapshot.active_setlist = Some(1);
    let mut ui = ui_with(snapshot.clone());
    let mut refresh = |ui: &mut UiState, edit: &dyn Fn(&mut UiSnapshot)| {
        edit(&mut snapshot);
        ui.set_snapshot(snapshot.clone()).unwrap();
    };

    open_directory(&mut ui, Category::Captures);
    activate(&mut ui, library(LibraryControl::MediaMenu(1)));
    activate(&mut ui, delete);
    let added = MediaEntry {
        folder: MediaFolder::MyCaptures,
        name: "New".into(),
        value: "k0".into(),
    };
    refresh(&mut ui, &|s| {
        let mut media = s.media.to_vec();
        media.insert(0, added.clone());
        s.media = media.into();
    });
    assert_eq!(
        activate(&mut ui, confirm),
        [UiCommand::Directory(DirectoryRequest::DeleteMedia {
            kind: MediaKind::Capture,
            id: "k2".into(),
        })]
    );
    activate(&mut ui, library(LibraryControl::MediaMenu(2)));
    activate(&mut ui, delete);
    let other = MediaEntry {
        value: "k3".into(),
        ..added.clone()
    };
    refresh(&mut ui, &|s| {
        let mut media = s.media.to_vec();
        media[2] = other.clone();
        s.media = media.into();
    });
    assert!(
        activate(&mut ui, confirm).is_empty(),
        "k2 is gone; k3 has its row"
    );

    open_directory(&mut ui, Category::Presets);
    activate(&mut ui, library(LibraryControl::SetlistMenu(2)));
    activate(&mut ui, delete);
    refresh(&mut ui, &|s| {
        s.setlists = setlists(&["My Presets", "A", "AA", "B"])
    });
    assert_eq!(
        activate(&mut ui, confirm),
        [UiCommand::Directory(DirectoryRequest::DeleteSetlist {
            name: "B".into()
        })]
    );

    activate(&mut ui, Control::PresetMenu(2));
    activate(&mut ui, delete);
    refresh(&mut ui, &|s| s.presets[2].title = "SWAPPED IN".into());
    assert!(activate(&mut ui, Control::Dialog(DialogButton::Primary)).is_empty());
}

/// Review r15 R11: the QC mini Copy Scene title names the footswitch letter and the roman
/// page (`0x2b071c`, `0x1d361c`, `0x1d37fc`): scene F is "Scene B (Page II)".
#[test]
fn mini_scene_copy_title_uses_the_footswitch_letter_and_roman_page() {
    let mut ui = ui_with(UiSnapshot {
        mini: true,
        selected_scene: scene(5),
        ..rich()
    });
    activate(&mut ui, Control::OpenMainMenu);
    activate(&mut ui, Control::Menu(MenuAction::CopyScene));
    assert!(
        ui.layout()
            .items
            .iter()
            .any(|item| item.label == "Copy Scene B (Page II)")
    );
}

/// Review r15 R24: in Gig EDIT STOMP the device gives footswitch presses to the dialog; a
/// stomp footswitch moves the editor to its stomp (as the stock Assign dialog takes the
/// press) instead of being swallowed.
#[test]
fn edit_stomp_takes_a_footswitch_press_as_its_stomp() {
    let mut ui = ui_with(UiSnapshot {
        footswitch_mode: Some(FootswitchMode::Stomp),
        ..rich()
    });
    swipe_up_to_gig(&mut ui);
    let (x, y) = center(&ui, Control::Stomp(StompEdit::Open(1)));
    tap(&mut ui, x, y);
    assert!(ui.assign_mode());
    let press = InputEvent::Footswitch {
        switch: Footswitch::C,
        pressed: true,
    };
    assert!(ui.handle_input(press).is_empty());
    assert!(shows(&ui, Control::Stomp(StompEdit::Rename(2))));
    assert!(!shows(&ui, Control::Stomp(StompEdit::Rename(1))));
}

/// Review r15 R25: "Factory Captures will be omitted." is in the plural delete text only
/// (singular strings `0x01204f00`, `0x01204fd8` have no `%4`).
#[test]
fn factory_omitted_note_is_plural_only() {
    let mut snapshot = rich();
    let entry = |value: &str| MediaEntry {
        folder: MediaFolder::MyCaptures,
        name: value.into(),
        value: value.into(),
    };
    let details = |value: &str, read_only| DirectoryMedia {
        value: value.into(),
        author: String::new(),
        date: String::new(),
        read_only,
        folders: Vec::new(),
        capture: None,
    };
    snapshot.media = vec![entry("owned"), entry("factory")].into();
    std::sync::Arc::make_mut(&mut snapshot.directory).media =
        vec![details("owned", false), details("factory", true)];
    let mut ui = ui_with(snapshot);
    let library = Control::Library;
    activate(&mut ui, Control::Navigate(Screen::Presets));
    activate(&mut ui, library(LibraryControl::Category));
    activate(&mut ui, Control::Choose(2));
    let note = |ui: &UiState| {
        ui.layout()
            .items
            .iter()
            .any(|item| item.label.contains("will be omitted"))
    };
    for (rows, plural) in [(&[1][..], false), (&[0, 1][..], true)] {
        activate(&mut ui, library(LibraryControl::MultiSelect));
        for row in rows {
            activate(&mut ui, library(LibraryControl::Check(*row)));
        }
        activate(&mut ui, library(LibraryControl::Bulk(BulkAction::Delete)));
        assert_eq!(note(&ui), plural, "{rows:?}");
        ui.handle_input(InputEvent::Back);
        ui.handle_input(InputEvent::Back);
    }
}

/// Global EQ preset popup (`0x227a84`, `0x2289a0`): while the page matches a preset, Save
/// Current Parameters as... and Overwrite are disabled; factory rows cannot be renamed; a
/// user Delete asks first, then requests the delete.
#[test]
fn settings_preset_popup_guards_rows_and_confirms_delete() {
    let mut snapshot = rich();
    snapshot.device_presets = [(1, "Flat", true), (2, "Stage", false)]
        .map(|(id, name, factory)| DevicePresetItem {
            id,
            model_id: 4004,
            name: name.into(),
            factory,
            default: false,
        })
        .to_vec()
        .into();
    snapshot.settings_preset_match.global_eq = Some(2);
    let mut ui = ui_with(snapshot);
    ui.navigate(Screen::GlobalEq(EqTab::Band(EqBand::new(0).unwrap())));
    let preset = Control::SettingsPreset;
    let page = SettingsPresetPage::GlobalEq;
    activate(&mut ui, preset(SettingsPresetControl::Open(page)));
    let enabled = |ui: &UiState, control| {
        ui.hit_regions()
            .iter()
            .any(|region| region.control == control && region.enabled)
    };
    assert!(!enabled(&ui, preset(SettingsPresetControl::SaveAs)));
    let item = |menu| preset(SettingsPresetControl::MenuItem(menu));
    activate(&mut ui, preset(SettingsPresetControl::Menu(1)));
    assert!(enabled(&ui, item(DevicePresetMenu::SetDefault)));
    assert!(!enabled(&ui, item(DevicePresetMenu::EditName)));
    // A tap outside the menu closes only the menu.
    ui.handle_input(InputEvent::Back);
    activate(&mut ui, preset(SettingsPresetControl::Menu(2)));
    assert!(enabled(&ui, item(DevicePresetMenu::EditName)));
    assert!(!enabled(&ui, item(DevicePresetMenu::Overwrite)));
    assert!(activate(&mut ui, item(DevicePresetMenu::Delete)).is_empty());
    assert_eq!(
        activate(&mut ui, Control::Dialog(DialogButton::Primary)),
        [UiCommand::SettingsPreset {
            page,
            request: SettingsPresetRequest::Delete(2),
        }]
    );
}

/// NC2 A/B (`0x3d8094`, `0x3d8284`): in the Ready dialog encoder 0 steps CAPTURE → REFERENCE
/// on a positive turn (a turn toward the selected side does nothing), encoder 1 steps the
/// level knob and footswitch A toggles; mode 3 (`0x3d7dc0`) shows "Neural Capture Saved." with EXIT, which closes NC2.
#[test]
fn nc2_ab_follows_encoder_and_stomp_and_saved_mode_exits() {
    let mut snapshot = rich();
    snapshot.capture.nc2 = Some(Nc2Dialog::Ready);
    snapshot.capture.audition = AuditionSource::Capture;
    (snapshot.settings).insert(SettingId::HeadphoneLevel, SettingValue::Number(-35));
    let mut ui = ui_with(snapshot.clone());
    let turn = |ui: &mut UiState, delta| {
        ui.handle_input(InputEvent::Knob {
            knob: KnobIndex::new(0).unwrap(),
            delta,
        })
    };
    let audition = |source| {
        vec![UiCommand::Nc2 {
            request: Nc2Request::Audition(source),
        }]
    };
    assert!(turn(&mut ui, -1).is_empty());
    assert_eq!(turn(&mut ui, 2), audition(AuditionSource::Reference));
    let press = InputEvent::Footswitch {
        switch: Footswitch::A,
        pressed: true,
    };
    assert_eq!(ui.handle_input(press), audition(AuditionSource::Reference));
    let level = ui.handle_input(InputEvent::Knob {
        knob: KnobIndex::new(1).unwrap(),
        delta: 1,
    });
    let value = SettingValue::Number(-30);
    let id = SettingId::HeadphoneLevel;
    assert_eq!(level, [UiCommand::ChangeSetting { id, value }]);

    snapshot.capture.nc2 = Some(Nc2Dialog::Saved);
    ui.set_snapshot(snapshot).unwrap();
    assert!(
        ui.layout()
            .items
            .iter()
            .any(|i| i.label == "Neural Capture Saved.")
    );
    assert_eq!(
        activate(&mut ui, Control::Nc2(Nc2Request::Close)),
        [UiCommand::Nc2 {
            request: Nc2Request::Close
        }]
    );
}

fn editor_block(s: &mut UiSnapshot, category: ModelCategory, model: u32) -> GridPosition {
    let position = cell(1, 3);
    s.blocks[position.index()] = BlockInfo {
        category: Some(category),
        model_id: Some(model),
        plugin: None,
    };
    s.grid[position.index()].label = "Editor".into();
    s.selected_block = Some(position);
    position
}

fn continuous(index: u16, label: &str, position: f32) -> ParameterView {
    ParameterView {
        index,
        label: label.into(),
        value: label.into(),
        kind: ParameterKind::Continuous { position },
        entry: None,
        scene: false,
    }
}

/// Pattern Tremolo editor (`0x3ae61c`): a vertical fader drag steps its STEP (10 px per
/// step), STEPS + steps STEPS, a sideways drag over steps 1..=4 joins them by flipping only
/// the CONNECTED links that are off, and a tap on the info icon opens Step Groups
/// (`0x3adf84`).
#[test]
fn pattern_tremolo_faders_step_values_and_join_runs() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::Modulation, 7040);
    s.parameters = std::iter::once(continuous(10, "STEPS", 0.5))
        .chain((0..16).map(|step| continuous(11 + step, "STEP", 0.5)))
        .collect();
    s.step_connected[1] = true;
    let mut ui = ui_with(s);
    let nudge = |parameter, steps| UiCommand::NudgeParameter {
        position,
        parameter,
        steps,
    };
    let first = center(&ui, Control::Editor(EditorControl::Step(0)));
    assert_eq!(
        drag(&mut ui, first, (first.0, first.1 - 25)),
        [nudge(11, 2)]
    );
    assert_eq!(
        activate(&mut ui, Control::Editor(EditorControl::StepsPlus)),
        [nudge(10, 1)]
    );
    let fourth = center(&ui, Control::Editor(EditorControl::Step(3)));
    assert_eq!(drag(&mut ui, first, fourth), [nudge(27, 1), nudge(29, 1)]);
    let (x, y) = center(&ui, Control::Editor(EditorControl::StepGroupsInfo));
    assert!(tap(&mut ui, x, y).is_empty());
    assert!(ui.layout().items.iter().any(|i| i.label == "Step Groups"));
}

/// Cab editor (`0x643e44`): a tap at the outer lower left of mic A's graph sets POSITION and
/// DISTANCE to their maximum (`0x825c68`); the IR arrows step only through the mics of the
/// same cab; an IR the media list does not have shows the missing note (`0x8221f8`).
#[test]
fn cab_editor_places_mic_and_steps_mics_of_its_cab() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::GuitarCabMono, 12004);
    s.parameters = vec![
        ParameterView {
            kind: ParameterKind::Media(MediaKind::Ir),
            ..continuous(1, "Cab 1 Mic B", 0.0)
        },
        continuous(4, "DISTANCE", 0.2),
        continuous(5, "POSITION", 0.4),
        ParameterView {
            kind: ParameterKind::Media(MediaKind::Ir),
            ..continuous(9, "Gone IR", 0.0)
        },
    ];
    s.media = ["Cab 1_Mic A", "Cab 1_Mic B", "Cab 2_Mic A"]
        .map(|value| MediaEntry {
            folder: MediaFolder::FactoryIrs,
            name: value.replace('_', " "),
            value: value.into(),
        })
        .to_vec()
        .into();
    let mut ui = ui_with(s);
    let enter = |parameter, text: &str| UiCommand::EnterParameter {
        target: ParameterTarget::Block(position),
        parameter,
        text: text.into(),
    };
    assert_eq!(
        tap(&mut ui, 8 + 54 + 20, 60 + 74 + 260),
        [enter(5, "10.00"), enter(4, "10.00")]
    );
    let step = |forward| {
        Control::Editor(EditorControl::IrStep {
            mic: CabMic::A,
            forward,
        })
    };
    assert!(activate(&mut ui, step(true)).is_empty());
    assert_eq!(
        activate(&mut ui, step(false)),
        [UiCommand::SelectText {
            position,
            parameter: 1,
            text: "Cab 1_Mic A".into(),
        }]
    );
    let layout = ui.layout();
    let notes = (layout.items.iter()).filter(|i| i.label == "Missing IR file:");
    assert_eq!(notes.count(), 1);
    let graph = |mic| {
        (ui.hit_regions().iter()).any(|r| r.control == Control::Editor(EditorControl::Mic(mic)))
    };
    assert!(
        graph(CabMic::A) && !graph(CabMic::B),
        "the note replaces mic B's speaker"
    );
}

/// The restore overlay (`0x81b74c`, text `0x3fba64`) covers every view: it shows the stock
/// text, offers no control and a tap on the Grid below it does nothing.
#[test]
fn the_restore_overlay_shows_the_stock_text_and_takes_every_touch() {
    let mut ui = ui_with(rich());
    let (x, y) = center(&ui, Control::SceneMenu);
    let mut snapshot = rich();
    snapshot.restoring = true;
    ui.set_snapshot(snapshot).unwrap();
    let layout = ui.layout();
    assert!((layout.items.iter()).any(|item| item.label == "Downloading a backup, please wait."));
    assert!(ui.hit_regions().is_empty());
    let before = ui.render();
    assert!(tap(&mut ui, x, y).is_empty());
    assert_eq!(ui.render(), before);
}

/// The hardware encoders turn the knobs a model editor shows, by the stock tables:
/// Pattern Tremolo `0x3b38d0` (A STEPS, B scroller, E–H and TEMPO the page knobs) and cab
/// `0x646f4c` (A–D mic A, E–H mic B). A free encoder does nothing.
#[test]
fn model_editor_encoders_turn_the_knobs_on_screen() {
    let turn = |ui: &mut UiState, knob, delta| {
        ui.handle_input(InputEvent::Knob {
            knob: KnobIndex::new(knob).unwrap(),
            delta,
        })
    };
    let nudge = |position, parameter, steps| {
        vec![UiCommand::NudgeParameter {
            position,
            parameter,
            steps,
        }]
    };
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::Modulation, 7040);
    s.parameters = [(0, "SYNC"), (1, "RATE"), (5, "SMOOTH"), (10, "STEPS")]
        .map(|(index, label)| continuous(index, label, 0.5))
        .into();
    let mut ui = ui_with(s);
    assert_eq!(turn(&mut ui, 0, 1), nudge(position, 10, 1));
    assert_eq!(turn(&mut ui, 5, -2), nudge(position, 1, -2));
    assert_eq!(turn(&mut ui, 9, 1), nudge(position, 5, 1));
    assert!(turn(&mut ui, 1, 3).is_empty());
    assert_eq!(ui.step_offset, 1);
    assert!(turn(&mut ui, 2, 1).is_empty());

    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::GuitarCabMono, 12004);
    s.parameters = [(3, "PAN"), (13, "POSITION")]
        .map(|(index, label)| continuous(index, label, 0.5))
        .into();
    let mut ui = ui_with(s);
    assert_eq!(turn(&mut ui, 3, 1), nudge(position, 3, 1));
    assert_eq!(turn(&mut ui, 4, -1), nudge(position, 13, -1));
    assert!(turn(&mut ui, 8, 1).is_empty());
}

/// Census #7: the Power Functions row "Lock Touchscreen and Master Volume" locks at once, as
/// POWER → LOCK does.
#[test]
fn the_power_functions_lock_row_locks_the_device() {
    let mut snapshot = rich();
    (snapshot.settings).insert(SettingId::LockScreen, SettingValue::Bool(false));
    let mut ui = ui_with(snapshot);
    ui.show(Screen::Settings(SettingsPage::PowerFunctions));
    let row = Control::Setting(SettingControl::Open(SettingId::LockScreen));
    assert_eq!(
        activate(&mut ui, row),
        [UiCommand::SetLocked { locked: true }]
    );
    assert!(ui.locked());
}

/// Review r19 F8: in a full-screen editor one vertical drag on a toggle knob (SYNC) flips it
/// once, on release, not on every drag step.
#[test]
fn an_editor_toggle_flips_once_per_drag() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::Modulation, 7040);
    s.parameters = vec![ParameterView {
        kind: ParameterKind::Toggle { on: false },
        ..continuous(0, "SYNC", 0.0)
    }];
    let mut ui = ui_with(s);
    let from = center(&ui, Control::Editor(EditorControl::Knob(0)));
    ui.handle_input(InputEvent::PointerDown {
        x: from.0,
        y: from.1,
    });
    let mut commands = Vec::new();
    for dy in [10, 30, 50] {
        commands.extend(ui.handle_input(InputEvent::PointerMove {
            x: from.0,
            y: from.1 - dy,
        }));
    }
    commands.extend(ui.handle_input(InputEvent::PointerUp {
        x: from.0,
        y: from.1 - 50,
    }));
    let flips = (commands.iter()).filter(|c| matches!(c, UiCommand::NudgeParameter { position: p, parameter: 0, .. } if *p == position));
    assert_eq!(flips.count(), 1);
}

fn numeric(index: u16, label: &str, value: f32, min: f32, max: f32) -> ParameterView {
    ParameterView {
        entry: Some(NumericRange {
            value,
            min,
            max,
            integer: false,
        }),
        ..continuous(index, label, (value - min) / (max - min))
    }
}

fn knob_turn(ui: &mut UiState, knob: u8, delta: i32) -> Vec<UiCommand> {
    ui.handle_input(InputEvent::Knob {
        knob: KnobIndex::new(knob).unwrap(),
        delta,
    })
}

fn nudged(position: GridPosition, parameter: u16, steps: i32) -> Vec<UiCommand> {
    vec![UiCommand::NudgeParameter {
        position,
        parameter,
        steps,
    }]
}

/// `EQModelEditor` (`0x640440`): a node drag sets FREQ and, only while GAIN is live, GAIN
/// (`0x63b988`, `0x63b7d4`: no GAIN on HI PASS); a node double tap turns the band off
/// (`0x63d990`); encoders A TYPE, C FREQ, DOWN on (left) / off (right), B nothing on a pass
/// type (`0x63d788`); OUT DOWN turns OUTPUT (`0x63a984`); a type table cell steps TYPE to
/// cell / 4 (`0x63d32c`).
#[test]
fn eq_editor_nodes_encoders_and_type_table_follow_the_stock_slots() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::Eq, 4001);
    let band = |base: u16, kind: f32, hz: f32, on: bool| {
        [
            numeric(base, "GAIN", 0.0, -12.0, 12.0),
            numeric(base + 1, "FREQ", hz, 20.0, 20_000.0),
            numeric(base + 2, "Q", 0.707, 0.1, 10.0),
            continuous(base + 3, "TYPE", kind),
            ParameterView {
                kind: ParameterKind::Toggle { on },
                ..continuous(base + 4, "BYPASS", 0.0)
            },
        ]
    };
    s.parameters = [
        band(0, 0.0, 200.0, true),
        band(5, 0.25, 50.0, true),
        band(10, 0.0, 4_000.0, false),
    ]
    .concat();
    s.parameters.push(numeric(15, "OUTPUT", 0.0, -20.0, 20.0));
    let mut ui = ui_with(s);
    let enter = |parameter, text: &str| UiCommand::EnterParameter {
        target: ParameterTarget::Block(position),
        parameter,
        text: text.into(),
    };
    // 2 kHz is two decades right of 20 Hz (528 px); +6 dB is 54 px above y 185.
    let node = center(&ui, Control::Editor(EditorControl::EqNode(0)));
    assert_eq!(
        drag(&mut ui, node, (528, 131)),
        [enter(1, "2000"), enter(0, "6.0")]
    );
    let node = center(&ui, Control::Editor(EditorControl::EqNode(1)));
    assert_eq!(drag(&mut ui, node, (264, 131)), [enter(6, "200")]);
    assert_eq!(ui.eq_band, 1);
    let node = center(&ui, Control::Editor(EditorControl::EqNode(1)));
    assert!(tap(&mut ui, node.0, node.1).is_empty());
    assert_eq!(tap(&mut ui, node.0, node.1), nudged(position, 9, 1));
    // Band 2 (HI PASS, on).
    assert_eq!(knob_turn(&mut ui, 0, 1), nudged(position, 8, 1));
    assert!(knob_turn(&mut ui, 1, 1).is_empty());
    assert_eq!(knob_turn(&mut ui, 2, -3), nudged(position, 6, -3));
    assert!(knob_turn(&mut ui, 8, -1).is_empty());
    assert_eq!(knob_turn(&mut ui, 8, 1), nudged(position, 9, 1));
    // The off band 3 takes no TYPE turn.
    activate(&mut ui, Control::Editor(EditorControl::EqTab(2)));
    assert!(knob_turn(&mut ui, 0, 1).is_empty());
    activate(&mut ui, Control::Editor(EditorControl::EqTab(3)));
    assert_eq!(knob_turn(&mut ui, 8, 2), nudged(position, 15, 2));
    activate(&mut ui, Control::Editor(EditorControl::EqTab(1)));
    activate(&mut ui, Control::Editor(EditorControl::EqType));
    assert_eq!(
        activate(&mut ui, Control::Choose(4)),
        nudged(position, 8, 3)
    );
}

/// `GraphicEQModelEditor` (`0x2c6184`): page 1 encoders A–D, DOWN, E–H turn bands 1..=9 in
/// that order; page 2 A HPF, C OUTPUT, D nothing without Nolly. A fader touch sets its gain
/// (top +12 dB, middle 0 dB).
#[test]
fn graphic_eq_faders_and_encoders_follow_the_stock_order() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::Eq, 4005);
    s.parameters = std::iter::once(numeric(0, "HPF", 19.0, 19.0, 500.0))
        .chain((1..=9).map(|band| numeric(band, "BAND", 0.0, -12.0, 12.0)))
        .chain([
            numeric(10, "LPF", 20_001.0, 1_000.0, 20_001.0),
            numeric(12, "OUTPUT", 0.0, -20.0, 20.0),
        ])
        .collect();
    let mut ui = ui_with(s);
    assert_eq!(knob_turn(&mut ui, 8, 1), nudged(position, 5, 1));
    assert_eq!(knob_turn(&mut ui, 4, 1), nudged(position, 6, 1));
    let fader = center(&ui, Control::Editor(EditorControl::Fader(3)));
    let commands = drag(&mut ui, fader, (fader.0, 112));
    assert_eq!(
        commands.last(),
        Some(&UiCommand::EnterParameter {
            target: ParameterTarget::Block(position),
            parameter: 3,
            text: "12.0".into(),
        })
    );
    activate(&mut ui, Control::Param(ParamControl::Page(1)));
    assert_eq!(knob_turn(&mut ui, 0, 1), nudged(position, 0, 1));
    assert_eq!(knob_turn(&mut ui, 2, 1), nudged(position, 12, 1));
    assert!(knob_turn(&mut ui, 3, 1).is_empty());
}

/// `IRLoaderModelEditor` (`0x800008`): the channel power and phase buttons flip MUTE and
/// INVERT; encoders A–D and DOWN turn IR 1's knobs, E–H and TEMPO IR 2's (`0x8023e4`).
#[test]
fn ir_loader_buttons_and_encoders_address_their_channel() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::IrLoader, 29003);
    s.parameters = (0..16)
        .map(|index| match index % 8 {
            0 | 1 => ParameterView {
                kind: ParameterKind::Toggle { on: false },
                ..continuous(index, "SWITCH", 0.0)
            },
            2 => ParameterView {
                kind: ParameterKind::Media(MediaKind::Ir),
                ..continuous(index, "IR PATH", 0.0)
            },
            _ => continuous(index, "KNOB", 0.5),
        })
        .collect();
    let mut ui = ui_with(s);
    assert_eq!(
        activate(&mut ui, Control::Editor(EditorControl::IrPower(1))),
        nudged(position, 8, 1)
    );
    assert_eq!(
        activate(&mut ui, Control::Editor(EditorControl::IrPhase(0))),
        nudged(position, 1, 1)
    );
    assert_eq!(knob_turn(&mut ui, 8, 1), nudged(position, 7, 1));
    assert_eq!(knob_turn(&mut ui, 4, -1), nudged(position, 11, -1));
    assert_eq!(knob_turn(&mut ui, 9, 1), nudged(position, 15, 1));
}

/// Shot sn-7: the Neural Capture editor sits in the block panel under the Grid (header at
/// y 204, knob card at y 256) with no capture field. Its title is the block label (the capture
/// name) in the device type colour; a title tap opens the capture browser to replace the block,
/// and so does a capture media pick.
#[test]
fn the_capture_editor_title_is_the_capture_and_opens_the_browser() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::NeuralCapture, 14001);
    s.grid[position.index()].label = "4-Comp Custom 1".into();
    s.captures[position.index()] = Some(CaptureDevice::Compressor);
    s.parameters = (0..5)
        .map(|index| continuous(index, "GAIN", 0.5))
        .chain([ParameterView {
            kind: ParameterKind::Media(MediaKind::Capture),
            value: "4-Comp Custom 1".into(),
            ..continuous(5, "file_name", 0.0)
        }])
        .collect();
    let mut ui = ui_with(s);
    let title = Control::Param(ParamControl::ChangeModel);
    let area = (ui.hit_regions().into_iter())
        .find(|region| region.control == title)
        .map(|region| region.bounds);
    assert_eq!(area.map(|area| area.y), Some(196), "under the Grid");
    assert!(shows(&ui, Control::Editor(EditorControl::Knob(0))));
    let knob = (ui.hit_regions().into_iter())
        .find(|region| region.control == Control::Editor(EditorControl::Knob(0)))
        .map(|region| region.bounds.y);
    assert_eq!(knob, Some(256));
    let layout = ui.layout();
    let color = (layout.items.iter())
        .filter(|item| item.label == "4-Comp Custom 1")
        .find_map(|item| match item.look {
            crate::layout::Look::Text { color, .. } => Some(color),
            _ => None,
        });
    assert_eq!(color, Some(tetrad_assets::block::DYNAMICS));
    activate(&mut ui, title);
    assert!(ui.capture_browser());
    assert!(ui.picker.replace);
    assert_eq!(ui.picker.target, Some(position));
}

/// `NCModelEditor` (`0x3c6a00`): a stored capture the media list lacks shows the stock
/// missing text and DOWNLOAD; DOWNLOAD asks for the capture by its NC hash (`0x3c892c`); a
/// failed download shows the failure text without the button (`0x3c5aa0`).
#[test]
fn a_missing_capture_downloads_by_hash_and_shows_a_failure() {
    let mut s = rich();
    editor_block(&mut s, ModelCategory::NeuralCapture, 14001);
    s.parameters = vec![ParameterView {
        kind: ParameterKind::Media(MediaKind::Capture),
        value: "Lost Capture".into(),
        ..continuous(5, "file_name", 0.0)
    }];
    s.media = vec![MediaEntry {
        folder: MediaFolder::MyCaptures,
        name: "Other".into(),
        value: "Other".into(),
    }]
    .into();
    let mut ui = ui_with(s);
    let download = Control::Editor(EditorControl::CaptureDownload);
    let shows =
        |ui: &UiState, text: &str| (ui.layout().items.iter()).any(|i| i.label.contains(text));
    assert!(shows(&ui, "Lost Capture is missing. Please re-download it"));
    assert_eq!(
        activate(&mut ui, download),
        [UiCommand::DownloadCapture {
            hash: "Lost Capture".into()
        }]
    );
    ui.capture_download_failed("Lost Capture".into());
    assert!(shows(&ui, "Re-downloading is not possible"));
    assert!(
        !ui.hit_regions()
            .iter()
            .any(|region| region.control == download)
    );
}

/// Census #4: the Directory cloud folders. Offline, DOWNLOADS opens the stock dialog
/// `0x5b0958` (WIFI SETTINGS opens the Wi-Fi page). Online and logged in, a folder asks its
/// first page, lists its rows, a row tap downloads that product (`0x79992c`), and a scroll to
/// the end of a full page asks the next one (fetchMore `0x431178`).
#[test]
fn cloud_folders_page_download_and_need_the_cloud() {
    let mut ui = ui_with(rich());
    activate(&mut ui, Control::Navigate(Screen::Presets));
    let downloads = Control::Library(LibraryControl::Downloads);
    assert!(activate(&mut ui, downloads).is_empty());
    let dialog = ui
        .layout()
        .items
        .iter()
        .any(|i| i.label == "No Wi-Fi connection");
    assert!(dialog);
    activate(&mut ui, Control::Dialog(DialogButton::Primary));
    assert_eq!(ui.screen(), Screen::Settings(SettingsPage::Connection));

    let mut s = rich();
    s.wifi.status = WifiStatus::InternetConnected;
    s.account.username = Some("player@example.com".into());
    let mut ui = ui_with(s.clone());
    activate(&mut ui, Control::Navigate(Screen::Presets));
    let first = UiCommand::BrowseCloud {
        kind: ItemKind::Preset,
        scope: CloudScope::Downloads,
        refresh: true,
    };
    assert_eq!(activate(&mut ui, downloads), [first]);
    let shows = |ui: &UiState, text: &str| ui.layout().items.iter().any(|i| i.label == text);
    let mut folder = CloudFolderView {
        kind: ItemKind::Preset,
        scope: CloudScope::Downloads,
        rows: Vec::new(),
        more: false,
        fetching: false,
        error: None,
    };
    s.cloud = vec![folder.clone()].into();
    ui.set_snapshot(s.clone()).unwrap();
    assert!(shows(&ui, "Empty Folder"));
    folder.more = true;
    folder.rows = (0..26)
        .map(|n| CloudItem {
            id: format!("id-{n}"),
            name: format!("Tone {n}"),
            author: "someone".into(),
            stored: false,
        })
        .collect();
    s.cloud = vec![folder].into();
    ui.set_snapshot(s).unwrap();
    assert!(shows(&ui, "Tone 0"));
    assert_eq!(
        activate(&mut ui, Control::Library(LibraryControl::CloudItem(0))),
        [UiCommand::DownloadProduct { id: "id-0".into() }]
    );
    let row = center(&ui, Control::Library(LibraryControl::CloudItem(1)));
    let mut asked = Vec::new();
    for _ in 0..20 {
        asked.extend(drag(&mut ui, row, (row.0, 60)));
    }
    let next = UiCommand::BrowseCloud {
        kind: ItemKind::Preset,
        scope: CloudScope::Downloads,
        refresh: false,
    };
    assert_eq!(asked, [next]);
}

/// Census #8 (`PastingPopup` `0x3a4a28`, `0x795b18`, `0x848e18`): "Paste consecutively from
/// the first chosen slot onwards" waits for one slot tap and pastes the copied presets into
/// that slot and the next ones; "Choose each slot manually" pastes into the tapped slots in
/// tap order. The bar counts the occupied target slots as overwritten (`0x7831a8`).
#[test]
fn paste_modes_address_the_chosen_slots() {
    let library = Control::Library;
    let mut s = rich();
    s.setlists = vec![SetlistItem {
        id: 0,
        title: "Live".into(),
    }];
    s.active_setlist = Some(0);
    let mut ui = ui_with(s);
    activate(&mut ui, Control::Navigate(Screen::Presets));
    let shows = |ui: &UiState, text: &str| ui.layout().items.iter().any(|i| i.label == text);
    let copy = |ui: &mut UiState| {
        activate(ui, library(LibraryControl::MultiSelect));
        for id in [0, 1] {
            activate(ui, library(LibraryControl::Check(id)));
        }
        activate(ui, library(LibraryControl::Bulk(BulkAction::Copy)));
        ui.directory.popup = None;
    };
    let paste = library(LibraryControl::Bulk(BulkAction::Paste));
    let pasted = |slots: Vec<u64>| {
        vec![UiCommand::CopyPresets {
            ids: vec![0, 1],
            from: "Live".to_owned(),
            setlist: 0,
            remove: false,
            to: PasteTarget::Slots(slots),
        }]
    };

    copy(&mut ui);
    activate(&mut ui, paste);
    assert!(shows(&ui, "Choose pasting option") && shows(&ui, "CONTINUE"));
    activate(
        &mut ui,
        library(LibraryControl::PasteChoice(PasteChoice::FromChosen)),
    );
    activate(&mut ui, library(LibraryControl::ConfirmPaste));
    assert!(activate(&mut ui, paste).is_empty(), "no slot chosen yet");
    activate(&mut ui, library(LibraryControl::Check(1)));
    assert!(shows(&ui, "2 Presets will be overwritten"));
    assert_eq!(activate(&mut ui, paste), pasted(vec![1, 2]));
    assert!(ui.directory.multi.is_none() && ui.directory.paste.is_none());

    copy(&mut ui);
    activate(&mut ui, paste);
    activate(&mut ui, library(LibraryControl::ConfirmPaste));
    for slot in [9, 5, 9, 7] {
        activate(&mut ui, library(LibraryControl::Check(slot)));
    }
    assert_eq!(activate(&mut ui, paste), pasted(vec![5, 7]));

    // Review r29 B7: a consecutive paste from the last slot pastes one preset and says so.
    copy(&mut ui);
    activate(&mut ui, paste);
    activate(
        &mut ui,
        library(LibraryControl::PasteChoice(PasteChoice::FromChosen)),
    );
    activate(&mut ui, library(LibraryControl::ConfirmPaste));
    ui.directory.paste = Some(crate::screens::PasteSlots::Consecutive(Some(255)));
    assert_eq!(
        activate(&mut ui, paste),
        vec![UiCommand::CopyPresets {
            ids: vec![0],
            from: "Live".to_owned(),
            setlist: 0,
            remove: false,
            to: PasteTarget::Slots(vec![255]),
        }]
    );
    assert_eq!(
        ui.toast.as_ref().map(|toast| toast.text.as_str()),
        Some("1 preset did not fit in the setlist")
    );
}

/// Census #8 (`0x794910`, `0x786264`): Multi Select DOWNLOAD in a cloud capture folder
/// downloads each checked product not yet on the unit, then Multi Select ends; a new page
/// keeps the checks on their products.
#[test]
fn bulk_download_fetches_the_checked_cloud_captures_not_on_the_unit() {
    let mut s = rich();
    s.wifi.status = WifiStatus::InternetConnected;
    s.account.username = Some("player@example.com".into());
    let row = |id: &str, stored| CloudItem {
        id: id.into(),
        name: id.into(),
        author: "someone".into(),
        stored,
    };
    let mut folder = CloudFolderView {
        kind: ItemKind::Capture,
        scope: CloudScope::Downloads,
        rows: vec![row("a", false), row("b", true), row("c", false)],
        more: false,
        fetching: false,
        error: None,
    };
    s.cloud = vec![folder.clone()].into();
    let mut ui = ui_with(s.clone());
    let library = Control::Library;
    activate(&mut ui, Control::Navigate(Screen::Presets));
    activate(&mut ui, library(LibraryControl::Category));
    activate(&mut ui, Control::Choose(2));
    activate(&mut ui, library(LibraryControl::Downloads));
    activate(&mut ui, library(LibraryControl::MultiSelect));
    for index in [1, 2] {
        activate(&mut ui, library(LibraryControl::Check(index)));
    }
    folder.rows.insert(0, row("new", false));
    s.cloud = vec![folder].into();
    ui.set_snapshot(s).unwrap();
    assert_eq!(
        activate(&mut ui, library(LibraryControl::Bulk(BulkAction::Download))),
        [UiCommand::DownloadProducts {
            ids: vec!["c".into()]
        }]
    );
    assert!(ui.directory.multi.is_none());

    // Review r28 A6 (r29 B8): the cloud goes away with the folder open: the folder and Multi
    // Select close (`0x78ff0c` → `0x78f628`).
    activate(&mut ui, library(LibraryControl::MultiSelect));
    assert!(ui.directory.cloud.is_some() && ui.directory.multi.is_some());
    let mut offline = ui.snapshot().clone();
    offline.wifi.status = WifiStatus::NoNetwork;
    ui.set_snapshot(offline).unwrap();
    assert!(ui.directory.cloud.is_none() && ui.directory.multi.is_none());
}

/// Gap C1 (`0x79992c` → `0x799134`; `0x794910` type 1 → `0x7867c4`): a cloud IR row downloads
/// into the IR destination, and Multi Select DOWNLOAD fetches each checked IR not on the unit
/// with "Downloading IRs, please wait.".
#[test]
fn cloud_irs_download_one_by_one_or_in_bulk() {
    let mut s = rich();
    s.wifi.status = WifiStatus::InternetConnected;
    s.account.username = Some("player@example.com".into());
    let row = |id: &str, stored| CloudItem {
        id: id.into(),
        name: id.into(),
        author: "someone".into(),
        stored,
    };
    s.cloud = vec![CloudFolderView {
        kind: ItemKind::Ir,
        scope: CloudScope::Cloud,
        rows: vec![row("a", false), row("b", true), row("c", false)],
        more: false,
        fetching: false,
        error: None,
    }]
    .into();
    let mut ui = ui_with(s);
    let library = Control::Library;
    activate(&mut ui, Control::Navigate(Screen::Presets));
    activate(&mut ui, library(LibraryControl::Category));
    activate(&mut ui, Control::Choose(3));
    activate(&mut ui, library(LibraryControl::Cloud));
    assert_eq!(
        activate(&mut ui, library(LibraryControl::CloudItem(0))),
        [UiCommand::DownloadProduct { id: "a".into() }]
    );
    activate(&mut ui, library(LibraryControl::MultiSelect));
    for index in [1, 2] {
        activate(&mut ui, library(LibraryControl::Check(index)));
    }
    assert_eq!(
        activate(&mut ui, library(LibraryControl::Bulk(BulkAction::Download))),
        [UiCommand::DownloadProducts {
            ids: vec!["c".into()]
        }]
    );
    assert_eq!(
        ui.toast.as_ref().map(|toast| toast.text.as_str()),
        Some("Downloading IRs, please wait.")
    );
}

/// Census #14 (`BackupEditDialog`): CREATE BACKUP asks a name first (`0x3f8568`, empty
/// field), ⋮ "Edit name" renames with the current name filled in (`0x3f8660`); the name is
/// simplified and at most 30 characters (`0x3f41f8`).
#[test]
fn backup_create_and_edit_name_use_the_backup_name_dialog() {
    let mut s = rich();
    s.backups.items = vec![BackupItem {
        id: 7,
        title: "Before gig".into(),
        detail: String::new(),
        downloaded: None,
    }];
    s.account.username = Some("player@example.com".into());
    let mut ui = ui_with(s);
    ui.navigate(Screen::Settings(SettingsPage::Backups));
    let type_and_save = |ui: &mut UiState, text: &str| {
        ui.keyboard.as_mut().expect("backup name keyboard").text = text.into();
        activate(ui, Control::Key(Key::Done))
    };
    assert!(activate(&mut ui, Control::Backup(BackupControl::Create)).is_empty());
    assert_eq!(ui.keyboard.as_ref().unwrap().text, "");
    assert_eq!(
        type_and_save(&mut ui, "  Studio   rig "),
        [UiCommand::CreateBackup {
            name: "Studio rig".into()
        }]
    );
    activate(&mut ui, Control::Backup(BackupControl::Item(7)));
    activate(&mut ui, Control::Menu(MenuAction::EditBackupName));
    assert_eq!(ui.keyboard.as_ref().unwrap().text, "Before gig");
    assert_eq!(
        type_and_save(&mut ui, "After gig"),
        [UiCommand::RenameBackup {
            id: 7,
            name: "After gig".into()
        }]
    );
}

/// r28-ui MR 4 (`downloadsItemClicked` `0x78d204`): a DOWNLOADS preset stored on the unit
/// closes the Directory and loads from the presets Downloads folder; a row not stored, or any
/// CLOUD row, downloads.
#[test]
fn a_stored_downloads_preset_loads_instead_of_downloading() {
    let mut s = rich();
    s.wifi.status = WifiStatus::InternetConnected;
    s.account.username = Some("player@example.com".into());
    let row = |id: &str, stored| CloudItem {
        id: id.into(),
        name: id.into(),
        author: "someone".into(),
        stored,
    };
    s.cloud = vec![CloudFolderView {
        kind: ItemKind::Preset,
        scope: CloudScope::Downloads,
        rows: vec![row("queued", false), row("kept", true)],
        more: false,
        fetching: false,
        error: None,
    }]
    .into();
    let mut ui = ui_with(s);
    activate(&mut ui, Control::Navigate(Screen::Presets));
    activate(&mut ui, Control::Library(LibraryControl::Downloads));
    assert_eq!(
        activate(&mut ui, Control::Library(LibraryControl::CloudItem(0))),
        [UiCommand::DownloadProduct {
            id: "queued".into()
        }]
    );
    let load = activate(&mut ui, Control::Library(LibraryControl::CloudItem(1)));
    let audition = UiCommand::Directory(DirectoryRequest::Audition {
        kind: ItemKind::Preset,
        folder: "cloud-0-1".into(),
        id: "kept".into(),
    });
    assert_eq!(load.last(), Some(&audition));
    assert_ne!(ui.screen(), Screen::Presets);
}

/// Review r27 R2: a DOWNLOAD failure lasts only while its editor stays open on that block
/// (stock sets the missing text again at each editor update, `0x3c5d38`); closing and
/// reopening the editor, or a stored download, gives the user DOWNLOAD again.
#[test]
fn a_capture_download_failure_clears_when_the_editor_reopens() {
    let mut s = rich();
    editor_block(&mut s, ModelCategory::NeuralCapture, 14001);
    s.parameters = vec![ParameterView {
        kind: ParameterKind::Media(MediaKind::Capture),
        value: "Lost Capture".into(),
        ..continuous(5, "file_name", 0.0)
    }];
    s.media = vec![MediaEntry {
        folder: MediaFolder::MyCaptures,
        name: "Other".into(),
        value: "Other".into(),
    }]
    .into();
    let mut ui = ui_with(s.clone());
    let download = Control::Editor(EditorControl::CaptureDownload);
    let offered = |ui: &UiState| ui.hit_regions().iter().any(|r| r.control == download);
    assert!(offered(&ui));
    ui.capture_download_failed("Lost Capture".into());
    ui.set_snapshot(s.clone()).unwrap();
    assert!(!offered(&ui), "the same editor update keeps the failure");
    let closed = UiSnapshot {
        selected_block: None,
        ..s.clone()
    };
    ui.set_snapshot(closed).unwrap();
    ui.set_snapshot(s).unwrap();
    assert!(offered(&ui));
    // A stored capture of that hash also clears it (r28-services typed end).
    ui.capture_download_failed("Lost Capture".into());
    ui.capture_download_stored("Lost Capture");
    assert!(offered(&ui));
}

/// Review r20 G5: cab page 2 encoders use the stock table of the cab (`0x646f4c`): plain
/// and Parallax cabs `0x11e8e68`, Petrucci `0x11e8e98`, John Mayer `0x11e8ec8`, other
/// plugin cabs `0x11e8ef8`.
#[test]
fn cab_page_two_encoders_use_the_table_of_the_cab() {
    let cab = |model: u32, plugin: Option<u32>| {
        let mut s = rich();
        let position = editor_block(&mut s, ModelCategory::GuitarCabMono, model);
        s.blocks[position.index()].plugin = plugin;
        s.parameters = (16..=27)
            .map(|index| continuous(index, "KNOB", 0.5))
            .collect();
        let mut ui = ui_with(s);
        activate(&mut ui, Control::Param(ParamControl::Page(1)));
        (ui, position)
    };
    let (mut ui, position) = cab(12004, None);
    assert_eq!(knob_turn(&mut ui, 0, 1), nudged(position, 16, 1));
    assert!(knob_turn(&mut ui, 8, 1).is_empty());
    let (mut ui, position) = cab(21007, Some(4));
    assert_eq!(knob_turn(&mut ui, 0, 1), nudged(position, 16, 1));
    let (mut ui, position) = cab(12078, Some(13));
    assert_eq!(knob_turn(&mut ui, 0, 1), nudged(position, 19, 1));
    assert_eq!(knob_turn(&mut ui, 8, 1), nudged(position, 21, 1));
    assert_eq!(knob_turn(&mut ui, 9, 1), nudged(position, 22, 1));
    assert!(knob_turn(&mut ui, 6, 1).is_empty());
    let (mut ui, position) = cab(12079, Some(21));
    assert_eq!(knob_turn(&mut ui, 6, 1), nudged(position, 23, 1));
    let (mut ui, position) = cab(12053, Some(3));
    assert_eq!(knob_turn(&mut ui, 0, 1), nudged(position, 24, 1));
    assert!(knob_turn(&mut ui, 6, 1).is_empty());
}

/// Directory Multi Select (manual "BULK ACTIONS"): FAVORITE adds every checked preset
/// (`0x783aa0`), EDIT renames the checked presets one keyboard after another, and PASTE
/// copies the COPY selection into the open setlist (`0x786d38`, first empty slots); UPLOAD
/// names the checked slots (`0x725ba4`). Review r21 H2: PASTE in another setlist copies from
/// the setlist open at COPY. Review r23 K11: a setlist created between COPY and PASTE moves
/// every index; the source stays the setlist named at COPY.
#[test]
fn multi_select_favorite_edit_and_paste_use_the_library_requests() {
    let mut snapshot = rich();
    let setlists = |titles: &[&str]| -> Vec<SetlistItem> {
        (titles.iter().enumerate())
            .map(|(id, title)| SetlistItem {
                id: id as u64,
                title: (*title).into(),
            })
            .collect()
    };
    snapshot.setlists = setlists(&["Live", "Studio"]);
    snapshot.active_setlist = Some(0);
    let mut ui = ui_with(snapshot);
    ui.show(Screen::Presets);
    activate(&mut ui, Control::Library(LibraryControl::MultiSelect));
    let ids: Vec<u64> = ui
        .snapshot
        .presets
        .iter()
        .take(2)
        .map(|preset| preset.id)
        .collect();
    assert_eq!(ids.len(), 2);
    let check = |ui: &mut UiState| {
        for &id in &ids {
            activate(ui, Control::Library(LibraryControl::Check(id)));
        }
    };
    check(&mut ui);
    assert_eq!(
        activate(
            &mut ui,
            Control::Library(LibraryControl::Bulk(BulkAction::Favorite))
        ),
        [UiCommand::FavoritePresets { ids: ids.clone() }]
    );
    activate(&mut ui, Control::Library(LibraryControl::MultiSelect));
    check(&mut ui);
    activate(
        &mut ui,
        Control::Library(LibraryControl::Bulk(BulkAction::Edit)),
    );
    for &id in &ids {
        let keyboard = ui.keyboard.as_mut().expect("rename keyboard");
        assert_eq!(keyboard.target, crate::state::TextTarget::PresetRename(id));
        keyboard.text = "Renamed".into();
        let commands = activate(&mut ui, Control::Key(Key::Done));
        assert!(
            matches!(commands[..], [UiCommand::RenamePreset { id: renamed, .. }] if renamed == id)
        );
    }
    assert!(ui.keyboard.is_none());
    activate(&mut ui, Control::Library(LibraryControl::MultiSelect));
    check(&mut ui);
    activate(
        &mut ui,
        Control::Library(LibraryControl::Bulk(BulkAction::Copy)),
    );
    ui.directory.popup = None;
    ui.snapshot.setlists = setlists(&["Covers", "Live", "Studio"]);
    ui.snapshot.active_setlist = Some(2);
    let paste = Control::Library(LibraryControl::Bulk(BulkAction::Paste));
    assert!(
        activate(&mut ui, paste).is_empty(),
        "the pasting option dialog"
    );
    activate(
        &mut ui,
        Control::Library(LibraryControl::PasteChoice(PasteChoice::FromEmpty)),
    );
    assert_eq!(
        activate(&mut ui, Control::Library(LibraryControl::ConfirmPaste)),
        [UiCommand::CopyPresets {
            ids: ids.clone(),
            from: "Live".into(),
            setlist: 2,
            remove: false,
            to: PasteTarget::FirstEmpty,
        }]
    );
    // UPLOAD names the slots; tetrad-app sends their preset paths to services. It needs a
    // usable cloud (re-r44 §1).
    ui.snapshot.wifi.status = WifiStatus::InternetConnected;
    ui.snapshot.account.username = Some("player@example.com".into());
    activate(&mut ui, Control::Library(LibraryControl::MultiSelect));
    check(&mut ui);
    assert_eq!(
        activate(
            &mut ui,
            Control::Library(LibraryControl::Bulk(BulkAction::Upload))
        ),
        [UiCommand::Upload {
            items: ids.iter().map(|&id| UploadItem::Slot(id)).collect(),
        }]
    );
}

/// Arpeggio Delay (`0x2ad914`): E–H and TEMPO turn the five knobs of the tab, A–D nothing
/// (`0x2abc9c`); SYNC on and SCALE Chromatic swap TIME → SYNC NOTE and INTERVAL → SEMITONES
/// (`0x2a9104`); an off tap's knobs and point take no input (`0x2ac414`); a point drag sets
/// time (x / 711) and pitch (y / 159) (`0x2acb44`); a double tap flips ACTIVE (`0x2ac904`).
#[test]
fn arpeggio_delay_tabs_points_and_encoders_follow_the_stock_tables() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::Delay, 6031);
    let choice = |index, label: &str, position| ParameterView {
        kind: ParameterKind::Choice { position, steps: 5 },
        ..continuous(index, label, position)
    };
    let toggle = |index, label: &str, on| ParameterView {
        kind: ParameterKind::Toggle { on },
        ..continuous(index, label, 0.0)
    };
    s.parameters = vec![
        continuous(0, "MIX", 0.8),
        toggle(1, "SYNC", true),
        choice(3, "SCALE", 0.0),
        continuous(4, "GLISSANDO", 0.1),
    ];
    for (tap, on, note) in [(0u16, true, 0.5), (1, true, 0.25), (2, false, 0.75)] {
        let base = 5 + 7 * tap;
        s.parameters.extend([
            toggle(base, "ACTIVE", on),
            choice(base + 1, "SEMITONES", 0.5),
            choice(base + 2, "INTERVAL", 0.5),
            continuous(base + 3, "DELAY TIME", 0.2),
            choice(base + 4, "SYNC NOTE", note),
        ]);
    }
    let mut ui = ui_with(s);
    assert_eq!(knob_turn(&mut ui, 4, 1), nudged(position, 0, 1));
    assert_eq!(knob_turn(&mut ui, 9, -2), nudged(position, 4, -2));
    assert!(knob_turn(&mut ui, 0, 1).is_empty());
    activate(&mut ui, Control::Editor(EditorControl::Tab(1)));
    assert_eq!(knob_turn(&mut ui, 5, 1), nudged(position, 6, 1));
    assert_eq!(knob_turn(&mut ui, 6, 1), nudged(position, 9, 1));
    activate(&mut ui, Control::Editor(EditorControl::Tab(3)));
    assert_eq!(knob_turn(&mut ui, 4, 1), nudged(position, 19, 1));
    assert!(knob_turn(&mut ui, 5, 1).is_empty());
    // Tap 1 sits at time 0.5, pitch 0.5: x 52 + 355.5, y 60 + 199.5 − 79.5.
    let point = center(&ui, Control::Editor(EditorControl::Point(0)));
    assert_eq!(point, (408, 180));
    let set = |parameter, value| UiCommand::SetPosition {
        position,
        parameter,
        value,
    };
    assert_eq!(
        drag(&mut ui, point, (52 + 711, 180 - 159)),
        [set(9, POSITION_SCALE), set(6, POSITION_SCALE)]
    );
    assert_eq!(ui.editor_tab, 1);
    let point = center(&ui, Control::Editor(EditorControl::Point(1)));
    assert!(tap(&mut ui, point.0, point.1).is_empty());
    assert_eq!(tap(&mut ui, point.0, point.1), nudged(position, 12, 1));
    let point = center(&ui, Control::Editor(EditorControl::Point(2)));
    assert!(drag(&mut ui, point, (100, 100)).is_empty());
}

/// Cab IR mode (`CabsimPanel` `0x8203a4`): the IR name toggles it; the panel then shows the
/// IR table of its cab (a row loads that IR, `irTableCellSelected`) in place of the knobs
/// and mic graph, and the close button ends it (`0x8202b8`).
#[test]
fn cab_ir_mode_lists_the_cab_irs_and_closes() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::GuitarCabMono, 12004);
    s.parameters = vec![
        continuous(0, "bypass", 0.0),
        ParameterView {
            kind: ParameterKind::Media(MediaKind::Ir),
            value: "SYN Cab 1 Mic B".into(),
            ..continuous(1, "ir selector", 0.0)
        },
        continuous(4, "DISTANCE", 0.2),
    ];
    s.media = ["SYN_Cab 2_Mic A", "SYN_Cab 1_Mic A", "SYN_Cab 1_Mic B"]
        .map(|value| MediaEntry {
            folder: MediaFolder::FactoryIrs,
            name: value.replace('_', " "),
            value: value.into(),
        })
        .to_vec()
        .into();
    let mut ui = ui_with(s);
    let mic = Control::Editor(EditorControl::Mic(CabMic::A));
    assert!(
        ui.hit_test(center(&ui, mic).0, center(&ui, mic).1)
            .is_some()
    );
    activate(&mut ui, Control::Editor(EditorControl::IrList(CabMic::A)));
    assert!(!ui.hit_regions().iter().any(|r| r.control == mic));
    let row = |row| {
        Control::Editor(EditorControl::IrRow {
            mic: CabMic::A,
            row,
        })
    };
    assert!(!ui.hit_regions().iter().any(|r| r.control == row(2)));
    assert_eq!(
        activate(&mut ui, row(0)),
        [UiCommand::SelectText {
            position,
            parameter: 1,
            text: "SYN_Cab 1_Mic A".into(),
        }]
    );
    activate(&mut ui, Control::Editor(EditorControl::IrClose(CabMic::A)));
    assert!(ui.hit_regions().iter().any(|r| r.control == mic));
}

/// M406 (`0x5b030c`, `0x7e8494`, `0x376204`): a deploy that switches a module off shows
/// only the header status, never the popup by itself (stock showed no popup for "1C Main &
/// Bass V2", 2026-10-17). A tap opens the popup, which names what was switched off; CPU
/// MONITOR turns the CPU monitor on; OK closes and hides the status. Without a tap the status
/// goes after 10 s; nothing switched off hides it.
#[test]
fn a_cpu_inhibit_flashes_the_header_status_and_the_popup_opens_on_tap() {
    let mut ui = ui_with(rich());
    let status = |ui: &UiState| {
        ui.hit_regions()
            .iter()
            .any(|r| r.control == Control::GlobalEqStatus)
    };
    ui.set_global_eq_inhibited(false, false);
    assert!(!status(&ui));
    ui.set_global_eq_inhibited(true, true);
    assert!(status(&ui) && ui.overlay.is_none());
    activate(&mut ui, Control::GlobalEqStatus);
    assert!(
        ui.layout()
            .items
            .iter()
            .any(|i| i.label == "Global EQ and Input Gate disabled")
    );
    activate(&mut ui, Control::Dialog(DialogButton::Primary));
    assert!(ui.overlay.is_none() && ui.show_cpu && status(&ui));
    activate(&mut ui, Control::GlobalEqStatus);
    activate(&mut ui, Control::Dialog(DialogButton::Cancel));
    assert!(ui.overlay.is_none() && !status(&ui));

    ui.set_global_eq_inhibited(false, true);
    ui.handle_input(InputEvent::Tick { now_ms: 9_999 });
    assert!(status(&ui));
    ui.handle_input(InputEvent::Tick { now_ms: 10_000 });
    assert!(!status(&ui) && ui.overlay.is_none());
    ui.set_global_eq_inhibited(false, true);
    ui.set_global_eq_inhibited(false, false);
    assert!(!status(&ui));
}

/// Corrupt Neural Capture (`0x54663c`): the names join with ", " (`0xa10c40`); an empty list
/// shows nothing; SEND REPORT is disabled (no report service) and CANCEL closes.
#[test]
fn the_corrupt_captures_dialog_lists_the_names_and_cancels() {
    let mut ui = ui_with(rich());
    ui.show_corrupt_captures(Vec::new());
    assert_eq!(ui.overlay, None);
    ui.show_corrupt_captures(vec!["Plexi".into(), "Bass DI".into()]);
    assert!(
        ui.layout()
            .items
            .iter()
            .any(|i| i.label == "Plexi, Bass DI")
    );
    let report = ui
        .hit_regions()
        .into_iter()
        .find(|r| r.label == "SEND REPORT");
    assert!(report.is_some_and(|r| !r.enabled));
    activate(&mut ui, Control::Dialog(DialogButton::Cancel));
    assert_eq!(ui.overlay, None);
}

/// Synthetic parameters 0..56 for a stock editor: knobs at 0.5, `on` toggles on, `off`
/// toggles off.
fn synth_parameters(on: &[u16], off: &[u16]) -> Vec<ParameterView> {
    (0..56u16)
        .map(|index| {
            if on.contains(&index) || off.contains(&index) {
                ParameterView {
                    kind: ParameterKind::Toggle {
                        on: on.contains(&index),
                    },
                    ..continuous(index, "SWITCH", 0.0)
                }
            } else {
                continuous(index, "KNOB", 0.5)
            }
        })
        .collect()
}

fn set_parameter(ui: &mut UiState, index: u16, kind: ParameterKind) {
    let mut s = ui.snapshot.clone();
    s.parameters
        .iter_mut()
        .find(|p| p.index == index)
        .unwrap()
        .kind = kind;
    ui.set_snapshot(s).unwrap();
}

/// Minivoicer (`0x60b5ac`): slot = HAL + 10 · page, no HAL − 5; MODE Chromatic swaps V1
/// INTERVAL for V1 SEMIT; MIDI On empties that slot and dims V1 ACTIVE (`0x623220`).
#[test]
fn minivoicer_pages_follow_the_stock_slot_order_and_state_rules() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::Pitch, 18007);
    s.parameters = synth_parameters(&[5, 9], &[15]);
    let mut ui = ui_with(s);
    assert_eq!(knob_turn(&mut ui, 4, 1), nudged(position, 4, 1));
    assert_eq!(knob_turn(&mut ui, 3, 1), nudged(position, 7, 1));
    set_parameter(
        &mut ui,
        2,
        ParameterKind::Choice {
            position: 1.0,
            steps: 5,
        },
    );
    assert_eq!(knob_turn(&mut ui, 3, 1), nudged(position, 20, 1));
    set_parameter(&mut ui, 15, ParameterKind::Toggle { on: true });
    assert!(knob_turn(&mut ui, 3, 1).is_empty());
    assert!(knob_turn(&mut ui, 1, 1).is_empty());
    activate(&mut ui, Control::Param(ParamControl::Page(1)));
    assert_eq!(knob_turn(&mut ui, 8, -1), nudged(position, 14, -1));
}

/// Multivoicer (`0x625638`): Normal E–H turn the tab card (VOICE 2: ACTIVE, INTERVAL or
/// SEMITONES in Chromatic, LEVEL, PAN); Global uses HAL order and a MIDI channel blocks KEY;
/// a voice double tap flips ACTIVE (`0x625268`); the DI drag sets DI LEVEL (`0x624e28`).
#[test]
fn multivoicer_tabs_global_bank_and_points_follow_the_stock_tables() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::Pitch, 18012);
    s.parameters = synth_parameters(&[5, 11, 17, 23, 48], &[]);
    s.parameters[50] = continuous(50, "MIDI CH", 0.0);
    for (pan, position) in [(10, 0.1), (16, 0.4), (22, 0.7), (28, 0.9)] {
        s.parameters[pan] = continuous(pan as u16, "PAN", position);
    }
    let mut ui = ui_with(s);
    assert_eq!(knob_turn(&mut ui, 5, 1), nudged(position, 44, 1));
    activate(&mut ui, Control::Editor(EditorControl::Tab(2)));
    assert!(knob_turn(&mut ui, 0, 1).is_empty());
    assert_eq!(knob_turn(&mut ui, 5, 1), nudged(position, 13, 1));
    assert_eq!(knob_turn(&mut ui, 7, 1), nudged(position, 16, 1));
    set_parameter(
        &mut ui,
        2,
        ParameterKind::Choice {
            position: 0.0,
            steps: 5,
        },
    );
    assert_eq!(knob_turn(&mut ui, 5, 1), nudged(position, 41, 1));
    let voice = center(&ui, Control::Editor(EditorControl::Point(0)));
    assert!(tap(&mut ui, voice.0, voice.1).is_empty());
    assert_eq!(tap(&mut ui, voice.0, voice.1), nudged(position, 5, 1));
    assert_eq!(ui.editor_tab, 1);
    let di = center(&ui, Control::Editor(EditorControl::Point(4)));
    let top = 60 + 33;
    assert_eq!(
        drag(&mut ui, di, (di.0, top)),
        [UiCommand::SetPosition {
            position,
            parameter: 0,
            value: POSITION_SCALE,
        }]
    );
    activate(&mut ui, Control::Param(ParamControl::Page(1)));
    assert_eq!(knob_turn(&mut ui, 3, 1), nudged(position, 3, 1));
    set_parameter(
        &mut ui,
        2,
        ParameterKind::Choice {
            position: 0.5,
            steps: 5,
        },
    );
    assert_eq!(knob_turn(&mut ui, 0, 1), nudged(position, 1, 1));
    set_parameter(&mut ui, 50, ParameterKind::Continuous { position: 0.1 });
    assert!(knob_turn(&mut ui, 0, 1).is_empty());
}

/// Overlord Synth (`0x62cad8`): direct HAL tables per category; OSC 1 off dims its knobs
/// (`0x62dbbc`); a wave tap turns the oscillator on and writes (wave − 1) / 6 (`0x62dde8`).
#[test]
fn overlord_synth_categories_and_wave_buttons_follow_the_stock_tables() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::Synth, 30001);
    s.parameters = synth_parameters(&[14, 23, 38], &[9]);
    let mut ui = ui_with(s);
    assert_eq!(knob_turn(&mut ui, 9, 1), nudged(position, 51, 1));
    activate(&mut ui, Control::Editor(EditorControl::Tab(2)));
    assert!(knob_turn(&mut ui, 0, 1).is_empty());
    assert_eq!(knob_turn(&mut ui, 3, 1), nudged(position, 18, 1));
    let wave = Control::Editor(EditorControl::Wave { osc: 0, wave: 3 });
    assert_eq!(
        activate(&mut ui, wave),
        [
            UiCommand::NudgeParameter {
                position,
                parameter: 9,
                steps: 1,
            },
            UiCommand::SetPosition {
                position,
                parameter: 10,
                value: POSITION_SCALE / 3,
            },
        ]
    );
    activate(&mut ui, Control::Editor(EditorControl::Tab(3)));
    assert!(knob_turn(&mut ui, 7, 1).is_empty());
}

/// Parallax (`0x63507c`): direct HAL tables per page, E is the meter; unlike the synth, a
/// knob dimmed by its section switch still takes the encoder; the M point sets MID LEVEL
/// from the graph height (`0x635d8c`).
#[test]
fn parallax_pages_sections_and_bands_follow_the_stock_tables() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::GuitarOverdrive, 3008);
    s.parameters = synth_parameters(&[35, 36], &[34]);
    let mut ui = ui_with(s);
    assert!(knob_turn(&mut ui, 4, 1).is_empty());
    assert_eq!(knob_turn(&mut ui, 0, 1), nudged(position, 0, 1));
    assert_eq!(knob_turn(&mut ui, 2, 1), nudged(position, 3, 1));
    let mid = center(&ui, Control::Editor(EditorControl::Point(1)));
    assert_eq!(
        drag(&mut ui, mid, (mid.0, 60)),
        [UiCommand::SetPosition {
            position,
            parameter: 4,
            value: POSITION_SCALE,
        }]
    );
    assert_eq!(
        activate(&mut ui, Control::Editor(EditorControl::Toggle(34))),
        nudged(position, 34, 1)
    );
    activate(&mut ui, Control::Param(ParamControl::Page(2)));
    assert_eq!(knob_turn(&mut ui, 8, 1), nudged(position, 12, 1));
}

/// Overlord Synth graphs: the envelope dragger writes ENV. DCY/ATCK 32 from the graph x
/// (`0x4fd8e4`); the filter point writes only CUTOFF 25, skew 0.3 (`0x62ef40`), and nothing
/// while FILTER is off.
#[test]
fn overlord_synth_envelope_and_filter_graphs_write_the_stock_parameters() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::Synth, 30001);
    s.parameters = synth_parameters(&[23], &[]);
    let mut ui = ui_with(s);
    let dragger = center(&ui, Control::Editor(EditorControl::Point(0)));
    assert_eq!(
        drag(&mut ui, dragger, (10 + 782, dragger.1)),
        [UiCommand::SetPosition {
            position,
            parameter: 32,
            value: POSITION_SCALE,
        }]
    );
    activate(&mut ui, Control::Editor(EditorControl::Tab(3)));
    let point = center(&ui, Control::Editor(EditorControl::Point(1)));
    // 1 kHz sits at x = 8 + 775 · log₁₀₀₀(50); its value is (980 / 19980)^0.3 ≈ 0.404.
    let x = 8 + (775.0 * 50f32.log10() / 3.0).round() as i32;
    let writes = drag(&mut ui, point, (x, point.1));
    let [
        UiCommand::SetPosition {
            parameter: 25,
            value,
            ..
        },
    ] = writes[..]
    else {
        panic!("{writes:?}");
    };
    assert!(value.abs_diff(4041) <= 10, "{value}");
    set_parameter(&mut ui, 23, ParameterKind::Toggle { on: false });
    assert!(drag(&mut ui, point, (x + 40, point.1)).is_empty());
}

/// Parallax resize mode (`0x636e28`, `0x63507c`): B, C, D turn COMPRESSION, MID DRIVE and
/// HIGH DRIVE, other encoders nothing; the M point's gain uses the 348 px graph.
#[test]
fn parallax_resize_mode_routes_three_encoders_and_grows_the_graph() {
    let mut s = rich();
    let position = editor_block(&mut s, ModelCategory::GuitarOverdrive, 3008);
    s.parameters = synth_parameters(&[34, 35, 36], &[]);
    let mut ui = ui_with(s);
    activate(&mut ui, Control::Editor(EditorControl::Resize));
    assert!(knob_turn(&mut ui, 0, 1).is_empty());
    assert_eq!(knob_turn(&mut ui, 1, 1), nudged(position, 1, 1));
    assert_eq!(knob_turn(&mut ui, 3, 1), nudged(position, 5, 1));
    assert!(knob_turn(&mut ui, 5, 1).is_empty());
    let mid = center(&ui, Control::Editor(EditorControl::Point(1)));
    assert_eq!(
        drag(&mut ui, mid, (mid.0, 60 + 87)),
        [UiCommand::SetPosition {
            position,
            parameter: 4,
            value: POSITION_SCALE / 4 * 3,
        }]
    );
    activate(&mut ui, Control::Editor(EditorControl::Resize));
    assert_eq!(knob_turn(&mut ui, 0, 1), nudged(position, 0, 1));
}

/// Parallax GAIN REDUCTION (`0x636054`): the meter shows the device value the editor poll
/// returns, not the stored parameter 41; no reply yet is a full bar (no reduction).
#[test]
fn parallax_meter_draws_the_polled_gain_reduction() {
    let mut s = rich();
    editor_block(&mut s, ModelCategory::GuitarOverdrive, 3008);
    s.parameters = synth_parameters(&[34, 35, 36], &[]);
    let blue = |ui: &UiState| {
        (ui.layout().items.iter())
            .filter(|item| {
                matches!(item.look, crate::layout::Look::Panel { fill, .. } if fill == tetrad_assets::Rgb(0x87, 0xda, 0xff))
            })
            .map(|item| item.bounds.width)
            .collect::<Vec<_>>()
    };
    let mut ui = ui_with(s.clone());
    assert_eq!(blue(&ui), [133]);
    s.editor_meters.insert(41, 0.25);
    ui.set_snapshot(s).unwrap();
    assert_eq!(blue(&ui), [33]);
}

/// USB Statistics (`0x689de8`, `0x68a90c`): "---" before the first SOC2 ARM block, then the
/// device values in the stock colours, white by default.
#[test]
fn usb_statistics_show_the_device_values_in_the_stock_colours() {
    let mut ui = ui_with(rich());
    ui.show(Screen::Settings(SettingsPage::Diagnostics));
    ui.support = crate::state::SupportView::Diagnostics(DiagnosticsPage::Usb);
    let color = |ui: &UiState, text: &str| {
        (ui.layout().items.iter())
            .filter(|item| item.label == text)
            .map(|item| match item.look {
                crate::layout::Look::Text { color, .. } => color,
                _ => panic!("{text} is not text"),
            })
            .collect::<Vec<_>>()
    };
    let dashes = color(&ui, "---");
    assert!(!dashes.is_empty() && dashes.iter().all(|c| *c == Palette::STOCK.text));
    let mut s = ui.snapshot.clone();
    let mut values: [UsbValue; 21] = std::array::from_fn(|index| UsbValue {
        text: format!("v{index}"),
        tone: UsbTone::Plain,
    });
    values[0].text = "Yes".into();
    values[5].tone = UsbTone::OutOfRange;
    values[13].tone = UsbTone::Plus;
    s.usb_statistics = Some(values);
    ui.set_snapshot(s).unwrap();
    assert!(color(&ui, "---").is_empty());
    assert_eq!(color(&ui, "Yes"), [tetrad_assets::Rgb(0xff, 0xff, 0xff)]);
    assert_eq!(color(&ui, "v5"), [tetrad_assets::Rgb(0xff, 0x00, 0x00)]);
    assert_eq!(color(&ui, "v13"), [tetrad_assets::Rgb(0x00, 0xff, 0x00)]);
}

/// Minivoicer MIDI On (`0x61dfbc`, `0x61d994`): page 0 shows V1 / V2 NOTE from the driver's
/// voice notes as pitch class and `note / 12`, "-" for a silent voice, never the stored NOTE
/// DISPLAY 23 / 24; page 1 hides them.
#[test]
fn minivoicer_midi_notes_show_on_the_first_page_only() {
    let mut s = rich();
    editor_block(&mut s, ModelCategory::Pitch, 18007);
    s.parameters = synth_parameters(&[15], &[]);
    s.parameters[24] = continuous(24, "V2 NOTE DISPLAY", 61.0 / 128.0);
    s.voice_notes = [Some(60), None];
    let mut ui = ui_with(s);
    let shown = |ui: &UiState, text: &str| ui.layout().items.iter().any(|i| i.label == text);
    assert!(shown(&ui, "V1 NOTE") && shown(&ui, "C5") && shown(&ui, "-"));
    activate(&mut ui, Control::Param(ParamControl::Page(1)));
    assert!(!shown(&ui, "V1 NOTE"));
}

/// Review r23 K5 way back (O): Device Storage lists the quarantined captures; RESTORE sends
/// the row's id and store, and a row that is gone sends nothing.
#[test]
fn device_storage_restore_sends_the_quarantined_capture() {
    let mut s = rich();
    s.quarantine = vec![
        QuarantineRow {
            id: "cloud-2-9".into(),
            downloads: true,
            reason: "bad rawdata".into(),
        },
        QuarantineRow {
            id: "user-1".into(),
            downloads: false,
            reason: "bad metadata".into(),
        },
    ];
    let mut ui = ui_with(s);
    ui.navigate(Screen::Settings(SettingsPage::DeviceStorage));
    let restore = Control::RestoreCapture(0);
    assert!(ui.hit_regions().iter().any(|r| r.control == restore));
    assert_eq!(
        activate(&mut ui, restore),
        [UiCommand::RestoreQuarantinedCapture {
            id: "cloud-2-9".into(),
            downloads: true,
        }]
    );
    assert!(activate(&mut ui, Control::RestoreCapture(2)).is_empty());
}

/// BANK UP capture walk (`0x808250` steps 3–4): read-only captures first by device code and
/// natural name, then the others in the Directory sort with pinned first. The Device Type
/// filter and the version do not drop rows; the version comes with each row.
#[test]
fn the_capture_cycle_puts_factory_captures_first_then_the_directory_sort() {
    let mut s = rich();
    let capture = |value: &str, name: &str| MediaEntry {
        folder: MediaFolder::MyCaptures,
        name: name.into(),
        value: value.into(),
    };
    s.media = vec![
        capture("u-beta", "Beta"),
        capture("f-b", "Fac 10"),
        capture("u-alpha", "Alpha"),
        capture("f-a", "Fac 9"),
        capture("u-pin", "Zed"),
        capture("f-z", "Fac Z"),
        MediaEntry {
            folder: MediaFolder::MyIrs,
            name: "An IR".into(),
            value: "CIR_1".into(),
        },
    ]
    .into();
    let detail = |value: &str, read_only: bool, device: u8, version: &str| DirectoryMedia {
        value: value.into(),
        author: String::new(),
        date: String::new(),
        read_only,
        folders: Vec::new(),
        capture: Some(CaptureMeta {
            device: Some(device),
            gain: None,
            version: version.into(),
            instrument: None,
        }),
    };
    std::sync::Arc::make_mut(&mut s.directory).media = vec![
        detail("u-beta", false, 0, "1"),
        detail("u-alpha", false, 0, "2"),
        detail("u-pin", false, 0, "2"),
        detail("f-b", true, 3, "1"),
        detail("f-a", true, 3, "1"),
        detail("f-z", true, 1, "2"),
    ];
    std::sync::Arc::make_mut(&mut s.directory).pinned_captures = vec!["u-pin".into()];
    let mut ui = ui_with(s);
    ui.directory.device_types.insert(3);
    let rows = ui.capture_cycle();
    let values: Vec<&str> = rows.iter().map(|row| row.value).collect();
    assert_eq!(values, ["f-z", "f-a", "f-b", "u-pin", "u-alpha", "u-beta"]);
    assert_eq!((rows[0].name, rows[0].version), ("Fac Z", "2"));
}

/// The first boot after an update (`0x54491c`) shows the popup titled "CorOS: %1"
/// (`0x351c58`) over the release notes.
#[test]
fn update_notes_show_the_version_title_and_notes() {
    let mut ui = ui_with(rich());
    ui.show_update_notes("4.1.1", "Bug fixes".into());
    let layout = ui.layout();
    let says = |text: &str| layout.items.iter().any(|i| i.label == text);
    assert!(says("CorOS: 4.1.1") && says("Bug fixes"));
}

fn media_snapshot(entries: &[(MediaFolder, &str, &str)]) -> UiSnapshot {
    let mut snapshot = rich();
    snapshot.media = (entries.iter())
        .map(|&(folder, name, value)| MediaEntry {
            folder,
            name: name.into(),
            value: value.into(),
        })
        .collect::<Vec<_>>()
        .into();
    std::sync::Arc::make_mut(&mut snapshot.directory).media = (entries.iter())
        .map(|&(folder, _, value)| DirectoryMedia {
            value: value.into(),
            author: String::new(),
            date: String::new(),
            read_only: false,
            folders: Vec::new(),
            capture: (folder == MediaFolder::MyCaptures).then(|| CaptureMeta {
                device: Some(2),
                gain: None,
                version: "2".into(),
                instrument: None,
            }),
        })
        .collect();
    snapshot
}

fn open_directory(ui: &mut UiState, category: crate::screens::Category) {
    activate(ui, Control::Navigate(Screen::Presets));
    ui.directory.category = category;
}

/// re-r31 §1 (`0x7c65e0`): a Directory capture tap closes the Directory and starts Add to
/// Grid; a press in any cell selects it (also a cell where the block does not fit), a press
/// above row 0 cancels, a press between cells does nothing; the mode then still waits.
#[test]
fn add_to_grid_taps_follow_the_stock_overlay() {
    use crate::screens::Category;
    let snapshot = media_snapshot(&[(MediaFolder::MyCaptures, "Amp", "key")]);
    let mut ui = ui_with(snapshot);
    let media = GridMedia::Capture("key".into());
    let mut cells = [[CellFit::Add; GRID_COLUMNS]; GRID_ROWS];
    cells[0][1] = CellFit::Replace;
    cells[0][2] = CellFit::NoFit;
    let start = |ui: &mut UiState| {
        open_directory(ui, Category::Captures);
        let commands = activate(ui, Control::Library(LibraryControl::Media(0)));
        assert!(commands.contains(&UiCommand::StartAddToGrid(media.clone())));
        assert_eq!(ui.screen(), Screen::Grid);
        let mut snapshot = ui.snapshot().clone();
        snapshot.add_to_grid = Some(cells);
        ui.set_snapshot(snapshot).unwrap();
    };
    for column in [1, 2] {
        start(&mut ui);
        let (x, y) = center(&ui, Control::AddToGrid(Some(cell(0, column))));
        assert_eq!(
            tap(&mut ui, x, y),
            vec![UiCommand::AddToGrid {
                position: cell(0, column),
                media: media.clone(),
            }]
        );
    }
    start(&mut ui);
    let (x, y) = center(&ui, Control::AddToGrid(Some(cell(0, 0))));
    let (right, _) = center(&ui, Control::AddToGrid(Some(cell(0, 1))));
    assert!(tap(&mut ui, (x + right) / 2, y).is_empty(), "between cells");
    assert!(ui.hit_test(x, y) == Some(Control::AddToGrid(Some(cell(0, 0)))));
    assert_eq!(tap(&mut ui, x, 4), vec![UiCommand::CancelAddToGrid]);
    assert_ne!(
        ui.hit_test(x, y),
        Some(Control::AddToGrid(Some(cell(0, 0))))
    );
}

/// re-r31 §2: ⋮ Edit Name on a capture opens its save page in edit mode with the stored type;
/// SAVE checks the name as `0x6901c8` (simplified, ASCII first letter upper case, under 4 →
/// keyboard, a name of another capture → "Name already exists") and sends `EditCapture`.
#[test]
fn capture_edit_name_opens_the_save_page_with_the_stored_metadata() {
    use crate::screens::Category;
    let snapshot = media_snapshot(&[
        (MediaFolder::MyCaptures, "Amp Lead", "k1"),
        (MediaFolder::MyCaptures, "Other Cap", "k2"),
    ]);
    let mut ui = ui_with(snapshot);
    open_directory(&mut ui, Category::Captures);
    let row = (ui.snapshot().media.iter())
        .position(|entry| entry.value == "k1")
        .unwrap();
    activate(
        &mut ui,
        Control::Library(LibraryControl::MediaMenu(row as u16)),
    );
    activate(
        &mut ui,
        Control::Library(LibraryControl::Item(ItemAction::EditName)),
    );
    let shown = |ui: &UiState, text: &str| ui.layout().items.iter().any(|i| i.label == text);
    assert!(shown(&ui, "Combo Amp") && shown(&ui, "Amp Lead"));
    let save = Control::CaptureUi(CaptureControl::Save);
    let rename = |ui: &mut UiState, text: &str| {
        activate(ui, Control::CaptureUi(CaptureControl::Name));
        ui.handle_input(InputEvent::TextChanged(text.into()));
        activate(ui, Control::Key(Key::Done));
    };
    rename(&mut ui, "other   Cap");
    assert!(activate(&mut ui, save).is_empty());
    assert!(shown(&ui, "Name already exists"));
    activate(&mut ui, Control::Dialog(DialogButton::Primary));
    rename(&mut ui, "new  lead");
    activate(
        &mut ui,
        Control::CaptureUi(CaptureControl::Instrument(CaptureInstrument::Bass)),
    );
    assert_eq!(
        activate(&mut ui, save),
        vec![UiCommand::Directory(DirectoryRequest::EditCapture {
            id: "k1".into(),
            name: "New lead".into(),
            device: Some(CaptureDevice::ComboAmp),
            instrument: Some(CaptureInstrument::Bass),
        })]
    );
    assert!(!shown(&ui, "TYPE OF CAPTURE"), "the edit page closed");
}

/// The capture edit page starts from the stored `instrumentType`, so SAVE without a chip tap
/// sends it back unchanged.
#[test]
fn capture_edit_keeps_the_stored_instrument() {
    use crate::screens::Category;
    let mut snapshot = media_snapshot(&[(MediaFolder::MyCaptures, "Amp Lead", "k1")]);
    if let Some(meta) = std::sync::Arc::make_mut(&mut snapshot.directory).media[0]
        .capture
        .as_mut()
    {
        meta.instrument = Some(1);
    }
    let mut ui = ui_with(snapshot);
    open_directory(&mut ui, Category::Captures);
    activate(&mut ui, Control::Library(LibraryControl::MediaMenu(0)));
    activate(
        &mut ui,
        Control::Library(LibraryControl::Item(ItemAction::EditName)),
    );
    assert_eq!(
        activate(&mut ui, Control::CaptureUi(CaptureControl::Save)),
        vec![UiCommand::Directory(DirectoryRequest::EditCapture {
            id: "k1".into(),
            name: "Amp Lead".into(),
            device: Some(CaptureDevice::ComboAmp),
            instrument: Some(CaptureInstrument::Guitar),
        })]
    );
}

/// re-r31 §3 IRSaveDialog (`0x7e542c`, `0x7e503c`): renaming an IR to the name of another
/// user IR asks first; RENAME opens the keyboard again, OVERWRITE sends the overwrite.
#[test]
fn an_ir_rename_onto_another_ir_name_asks_overwrite_or_rename() {
    use crate::screens::Category;
    let snapshot = media_snapshot(&[
        (MediaFolder::MyIrs, "Room A", "CIR_1"),
        (MediaFolder::MyIrs, "Room B", "CIR_2"),
    ]);
    let mut ui = ui_with(snapshot);
    open_directory(&mut ui, Category::Irs);
    let row = (ui.snapshot().media.iter())
        .position(|entry| entry.value == "CIR_1")
        .unwrap();
    let rename = |ui: &mut UiState, text: &str| {
        activate(ui, Control::Library(LibraryControl::MediaMenu(row as u16)));
        activate(
            ui,
            Control::Library(LibraryControl::Item(ItemAction::EditName)),
        );
        ui.handle_input(InputEvent::TextChanged(text.into()));
        activate(ui, Control::Key(Key::Done))
    };
    let asks = |ui: &UiState| (ui.layout().items.iter()).any(|i| i.label == "OVERWRITE");
    assert!(rename(&mut ui, "Room  B").is_empty());
    assert!(asks(&ui));
    assert!(activate(&mut ui, Control::Dialog(DialogButton::Cancel)).is_empty());
    assert!(ui.keyboard.is_some(), "RENAME opens the keyboard again");
    activate(&mut ui, Control::Key(Key::Cancel));
    rename(&mut ui, "Room B");
    assert_eq!(
        activate(&mut ui, Control::Dialog(DialogButton::Primary)),
        vec![UiCommand::OverwriteIr {
            id: "CIR_1".into(),
            name: "Room B".into(),
            other: "CIR_2".into(),
        }]
    );
    assert_eq!(
        rename(&mut ui, "Room C"),
        vec![UiCommand::Directory(DirectoryRequest::RenameMedia {
            kind: MediaKind::Ir,
            id: "CIR_1".into(),
            name: "Room C".into(),
        })]
    );
}

/// re-r31 §5 (`0x794768`, `0x781e58` mode 2): DOWNLOAD of checked cloud presets asks the
/// destination setlist, then the downloading option, then sends `DownloadPresets`.
#[test]
fn bulk_preset_download_asks_setlist_then_downloading_option() {
    use crate::screens::Category;
    let mut snapshot = rich();
    snapshot.setlists = vec![SetlistItem {
        id: 4,
        title: "Live".into(),
    }];
    snapshot.cloud = vec![CloudFolderView {
        kind: ItemKind::Preset,
        scope: CloudScope::Cloud,
        rows: ["p1", "p2"]
            .map(|id| CloudItem {
                id: id.into(),
                name: id.into(),
                ..CloudItem::default()
            })
            .to_vec(),
        more: false,
        fetching: false,
        error: None,
    }]
    .into();
    let mut ui = ui_with(snapshot);
    open_directory(&mut ui, Category::Presets);
    ui.directory.cloud = Some(CloudScope::Cloud);
    ui.directory.multi = Some([0, 1].into());
    let shown = |ui: &UiState, text: &str| ui.layout().items.iter().any(|i| i.label == text);
    assert!(
        activate(
            &mut ui,
            Control::Library(LibraryControl::Bulk(BulkAction::Download))
        )
        .is_empty()
    );
    assert!(shown(&ui, "Choose destination Setlist:"));
    assert_eq!(
        activate(&mut ui, Control::Library(LibraryControl::Setlist(4))),
        vec![UiCommand::OpenSetlist { id: 4 }]
    );
    let mut snapshot = ui.snapshot().clone();
    snapshot.active_setlist = Some(4);
    ui.set_snapshot(snapshot).unwrap();
    assert!(shown(&ui, "Choose downloading option"));
    assert!(shown(
        &ui,
        "Download consecutively from the first empty slot onwards"
    ));
    activate(
        &mut ui,
        Control::Library(LibraryControl::PasteChoice(PasteChoice::FromEmpty)),
    );
    assert_eq!(
        activate(&mut ui, Control::Library(LibraryControl::ConfirmPaste)),
        vec![UiCommand::DownloadPresets {
            ids: vec!["p1".into(), "p2".into()],
            setlist: 4,
            to: PasteTarget::FirstEmpty,
            overwrite: false,
        }]
    );
}

/// `0x1dcf2c` → `0x46485c(name, 1)` (Multi Select run): a cloud preset download onto an
/// occupied slot asks "Overwrite Existing Presets?" first. OVERWRITE sends every product with
/// `overwrite`; SKIP sends only the products whose slot is empty. Empty targets ask nothing.
#[test]
fn a_preset_download_onto_an_occupied_slot_asks_overwrite_or_skip() {
    use crate::screens::Category;
    let mut snapshot = rich();
    snapshot.setlists = vec![SetlistItem {
        id: 4,
        title: "Live".into(),
    }];
    snapshot.active_setlist = Some(4);
    // As the device sends it: every slot is a row; `directory.slots` marks 5 and 6 empty.
    std::sync::Arc::make_mut(&mut snapshot.directory).slots = (snapshot.presets.iter())
        .map(|preset| DirectorySlot {
            id: preset.id,
            title: preset.title.clone(),
            empty: [5, 6].contains(&preset.id),
            ..DirectorySlot::default()
        })
        .collect();
    snapshot.cloud = vec![CloudFolderView {
        kind: ItemKind::Preset,
        scope: CloudScope::Cloud,
        rows: ["p1", "p2"]
            .map(|id| CloudItem {
                id: id.into(),
                name: id.into(),
                ..CloudItem::default()
            })
            .to_vec(),
        more: false,
        fetching: false,
        error: None,
    }]
    .into();
    let mut ui = ui_with(snapshot);
    let library = Control::Library;
    let download = library(LibraryControl::Bulk(BulkAction::Paste));
    // Slots 0..10 but 5 and 6 hold presets.
    let pick = |ui: &mut UiState, slots: [u64; 2]| {
        open_directory(ui, Category::Presets);
        ui.directory.cloud = Some(CloudScope::Cloud);
        ui.directory.multi = Some([0, 1].into());
        activate(ui, library(LibraryControl::Bulk(BulkAction::Download)));
        activate(ui, library(LibraryControl::Setlist(4)));
        ui.set_snapshot(ui.snapshot().clone()).unwrap();
        activate(ui, library(LibraryControl::ConfirmPaste));
        for slot in slots {
            activate(ui, library(LibraryControl::Check(slot)));
        }
        activate(ui, download)
    };
    let sent = |ids: &[&str], slots: Vec<u64>, overwrite| {
        vec![UiCommand::DownloadPresets {
            ids: ids.iter().map(|id| id.to_string()).collect(),
            setlist: 4,
            to: PasteTarget::Slots(slots),
            overwrite,
        }]
    };
    let shown = |ui: &UiState, text: &str| ui.layout().items.iter().any(|i| i.label == text);

    assert_eq!(
        pick(&mut ui, [5, 6]),
        sent(&["p1", "p2"], vec![5, 6], false)
    );

    assert!(pick(&mut ui, [3, 6]).is_empty());
    assert!(shown(&ui, "Overwrite Existing Presets?") && shown(&ui, "SKIP"));
    assert_eq!(
        activate(&mut ui, Control::Dialog(DialogButton::Cancel)),
        sent(&["p2"], vec![6], false)
    );

    assert!(pick(&mut ui, [3, 6]).is_empty());
    assert_eq!(
        activate(&mut ui, Control::Dialog(DialogButton::Primary)),
        sent(&["p1", "p2"], vec![3, 6], true)
    );
    assert!(ui.overlay.is_none());
}

/// Backup ⋮ "Update" (`0x3fd4b8` row 1 → `0x3f8730`): "Update Cloud Backup" names the backup
/// date and the product; only PROCEED sends `UpdateBackup`, CANCEL sends nothing.
#[test]
fn backup_update_asks_before_it_replaces_the_cloud_backup() {
    let mut s = rich();
    s.backups.items = vec![BackupItem {
        id: 7,
        title: "Before gig".into(),
        detail: "2026-10-01 18:00".into(),
        downloaded: None,
    }];
    s.account.username = Some("player@example.com".into());
    let mut ui = ui_with(s);
    ui.navigate(Screen::Settings(SettingsPage::Backups));
    let shown = |ui: &UiState, text: &str| ui.layout().items.iter().any(|i| i.label == text);
    for (button, sent) in [
        (DialogButton::Cancel, vec![]),
        (
            DialogButton::Primary,
            vec![UiCommand::UpdateBackup { id: 7 }],
        ),
    ] {
        activate(&mut ui, Control::Backup(BackupControl::Item(7)));
        assert!(activate(&mut ui, Control::Menu(MenuAction::UpdateBackup)).is_empty());
        assert!(shown(
            &ui,
            "This will replace the Cloud Backup made on October 1st, 2026, 18:00 with the current \
             files on your Quad Cortex"
        ));
        assert_eq!(activate(&mut ui, Control::Dialog(button)), sent);
    }
}

/// Review r35 G1: the Grid ⋮ Delete Preset confirm closes when the loaded preset changes —
/// a new load generation (another setlist's slot of the same number and title), another
/// slot or a rename — so a later tap deletes nothing.
#[test]
fn delete_loaded_preset_confirm_closes_when_the_loaded_preset_changes() {
    let changes: [fn(&mut UiSnapshot); 3] = [
        |s| s.preset_load += 1,
        |s| s.preset_id = Some(8),
        |s| s.preset_title = "OTHER".into(),
    ];
    let primary = Control::Dialog(DialogButton::Primary);
    for change in changes {
        let mut ui = ui_with(rich());
        activate(&mut ui, Control::OpenMainMenu);
        activate(&mut ui, Control::Menu(MenuAction::DeletePreset));
        assert!(shows(&ui, primary));
        let mut snapshot = ui.snapshot().clone();
        change(&mut snapshot);
        ui.set_snapshot(snapshot).unwrap();
        assert!(!shows(&ui, primary));
        assert!(activate(&mut ui, primary).is_empty());
    }
}

/// Review r35 G2: checked preset slots follow their rows before DELETE or MOVE opens. A new
/// browsed setlist clears the checks, so DELETE sends nothing; a checked slot that now holds
/// another preset drops out, so MOVE sends only the slot still showing its checked preset.
#[test]
fn checked_preset_slots_follow_their_rows_before_delete_or_move() {
    let library = Control::Library;
    let mut snapshot = rich();
    snapshot.setlists = ["My Presets", "Live"]
        .into_iter()
        .enumerate()
        .map(|(id, title)| SetlistItem {
            id: id as u64,
            title: title.into(),
        })
        .collect();
    snapshot.active_setlist = Some(0);
    let mut ui = ui_with(snapshot.clone());
    activate(&mut ui, Control::Navigate(Screen::Presets));
    let check = |ui: &mut UiState| {
        activate(ui, library(LibraryControl::MultiSelect));
        for id in [1, 3] {
            activate(ui, library(LibraryControl::Check(id)));
        }
    };
    check(&mut ui);
    let mut browsed = snapshot.clone();
    browsed.active_setlist = Some(1);
    ui.set_snapshot(browsed).unwrap();
    activate(&mut ui, library(LibraryControl::Bulk(BulkAction::Delete)));
    assert!(activate(&mut ui, library(LibraryControl::ConfirmBulkDelete)).is_empty());

    ui.set_snapshot(snapshot.clone()).unwrap();
    activate(&mut ui, library(LibraryControl::MultiSelect));
    check(&mut ui);
    snapshot.presets[3].title = "OTHER".into();
    ui.set_snapshot(snapshot).unwrap();
    activate(&mut ui, library(LibraryControl::Bulk(BulkAction::Move)));
    assert_eq!(
        activate(&mut ui, Control::Choose(1)),
        [UiCommand::CopyPresets {
            ids: vec![1],
            from: "My Presets".into(),
            setlist: 1,
            remove: true,
            to: PasteTarget::FirstEmpty,
        }]
    );
}

/// Review r35 G9: a slot that now holds another preset with the same title (a same-name
/// download OVERWRITE, a delete and a paste) is another row. Its open Delete confirm closes
/// and its Multi Select check drops, so DELETE does not remove the new preset.
#[test]
fn a_new_preset_with_the_same_title_closes_the_slot_confirm_and_check() {
    let library = Control::Library;
    let mut snapshot = rich();
    snapshot.setlists = vec![SetlistItem {
        id: 0,
        title: "My Presets".into(),
    }];
    snapshot.active_setlist = Some(0);
    std::sync::Arc::make_mut(&mut snapshot.directory).slots = (snapshot.presets.iter())
        .map(|preset| DirectorySlot {
            id: preset.id,
            key: 100 + preset.id,
            title: "Light".into(),
            ..DirectorySlot::default()
        })
        .collect();
    let mut ui = ui_with(snapshot.clone());
    activate(&mut ui, Control::Navigate(Screen::Presets));
    let mut replaced = snapshot.clone();
    std::sync::Arc::make_mut(&mut replaced.directory).slots[2].key = 7;

    activate(&mut ui, Control::PresetMenu(2));
    activate(&mut ui, library(LibraryControl::Item(ItemAction::Delete)));
    ui.set_snapshot(replaced.clone()).unwrap();
    assert!(activate(&mut ui, Control::Dialog(DialogButton::Primary)).is_empty());

    ui.set_snapshot(snapshot).unwrap();
    activate(&mut ui, library(LibraryControl::MultiSelect));
    for id in [1, 2] {
        activate(&mut ui, library(LibraryControl::Check(id)));
    }
    ui.set_snapshot(replaced).unwrap();
    activate(&mut ui, library(LibraryControl::Bulk(BulkAction::Delete)));
    assert_eq!(
        activate(&mut ui, library(LibraryControl::ConfirmBulkDelete)),
        [UiCommand::DeletePreset { id: 1 }]
    );
}

/// Snapshot with one overdrive block at `position`, selected (its editor is open).
fn selected_overdrive() -> UiSnapshot {
    let position = cell(0, 0);
    let mut s = UiSnapshot::default();
    s.grid[position.index()] = GridCell {
        label: "Freeman BOD".into(),
        bypassed: false,
    };
    s.blocks[position.index()].category = Some(ModelCategory::GuitarOverdrive);
    s.selected_block = Some(position);
    s
}

/// Stock block editor (`b4-stock1`, 2026-10-16): the header and the Grid behind an open editor
/// keep 132/255 of each channel, and the editor panel is not dimmed.
#[test]
fn open_block_editor_dims_the_header_and_grid_to_stock_level() {
    let open = ui_with(selected_overdrive()).render();
    let mut closed_snapshot = selected_overdrive();
    closed_snapshot.selected_block = None;
    let closed = ui_with(closed_snapshot).render();
    // USB 5 port tile (8..51, 109..184) and the header scene badge (656..680, 12..36).
    for (x, y) in [(20, 120), (658, 24)] {
        assert_eq!(
            open.pixel(x, y),
            closed.pixel(x, y).mix(Palette::STOCK.canvas, 123),
            "dimmed at ({x}, {y})"
        );
    }
    assert_eq!(
        open.pixel(730, 210),
        Palette::STOCK.panel,
        "the ✓ key is not dimmed"
    );
}

/// Colour the device shows: RGB565 truncation, expanded back to 8 bits per channel.
fn device(c: tetrad_assets::Rgb) -> tetrad_assets::Rgb {
    let (r, g, b) = (c.0 >> 3, c.1 >> 2, c.2 >> 3);
    tetrad_assets::Rgb(
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
    )
}

/// First and last column of each run of columns with ink (a channel above 128) inside `area`,
/// the same threshold the stock shots are read with.
fn ink_runs(frame: &Frame, area: Rect) -> Vec<(i32, i32)> {
    let mut runs: Vec<(i32, i32)> = Vec::new();
    for x in area.x..area.right() {
        let inked = (area.y..area.bottom()).any(|y| {
            let c = frame.pixel(x as u32, y as u32);
            c.0.max(c.1).max(c.2) > 128
        });
        if !inked {
            continue;
        }
        match runs.last_mut() {
            Some((_, last)) if *last + 1 == x => *last = x,
            _ => runs.push((x, x)),
        }
    }
    runs
}

/// Stock title (`sn-0` "1A Save", `stock-home` "3D Worship V5"): the glyph starts are 15, 56,
/// 120, 163, 200 and 239, and 13, 59, 120, 187, 228, 254 and 292. Qt draws 70 px text as outlines
/// at the fractional pen, and Tetrad puts each glyph at the rounded pen, so each start may
/// differ by 1.
#[test]
fn grid_title_glyph_starts_match_the_stock() {
    let title = |bank: u16, slot: u8, name: &str| {
        ui_with(UiSnapshot {
            preset_title: name.into(),
            preset_slot: Some(BankSlot { bank, slot }),
            ..UiSnapshot::default()
        })
        .render()
    };
    let area = Rect::new(0, 14, 560, 80);
    let check = |frame: &Frame, stock: &[i32]| {
        let starts: Vec<i32> = ink_runs(frame, area).iter().map(|run| run.0).collect();
        assert!(starts.len() >= stock.len(), "{starts:?}");
        for (got, want) in starts.iter().zip(stock) {
            assert!((got - want).abs() <= 1, "glyph start {got} for {stock:?}");
        }
    };
    check(&title(0, 0, "Save"), &[15, 56, 120, 163, 200, 239]);
    check(
        &title(2, 3, "Worship V5"),
        &[13, 59, 120, 187, 228, 254, 292],
    );
    // Stock `sn-0` ink rows 27 to 77 for "1A Save": the baseline sits at y 77.
    let frame = title(0, 0, "Save");
    let rows: Vec<i32> = (area.y..area.bottom())
        .filter(|&y| (area.x..area.right()).any(|x| frame.pixel(x as u32, y as u32).0 > 128))
        .collect();
    assert_eq!((rows[0], rows[rows.len() - 1]), (27, 77), "title rows");
}

/// Stock never elides the Grid title (`0x00341104`): while the name's ink + 1 is wider than
/// the 598 px item − pen x − 10, it sets the pixel size to `int(size × 0.9)`. "1C Main & Bass V2"
/// draws whole at 63 px (`st-mp1c`; Tetrad used to show "Main & Bass..."), a short name stays
/// at 70 px, and the slot id keeps 70 px.
#[test]
fn grid_title_shrinks_to_fit_and_is_not_elided() {
    let title = |name: &str| {
        let ui = ui_with(UiSnapshot {
            preset_title: name.into(),
            preset_slot: Some(BankSlot { bank: 0, slot: 2 }),
            can_undo: true,
            ..UiSnapshot::default()
        });
        ui.layout()
            .items
            .into_iter()
            .filter(|item| item.bounds.y < crate::state::HEADER && item.bounds.x < 598)
            .filter_map(|item| match item.look {
                crate::layout::Look::Text { font, .. } => Some((item.label, font.size)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let px70 = tetrad_assets::TextSize::Px70;
    assert_eq!(
        title("Main & Bass V2")[..3],
        [
            ("1".into(), px70),
            ("C".into(), px70),
            ("Main & Bass V2".into(), tetrad_assets::TextSize::Px63),
        ]
    );
    assert_eq!(title("Bass")[2], ("Bass".into(), px70));
}

/// Stock mode label (`st-mp1c`): "STOMP" is 22 px Bold with the 26.6 kern rule (T→O −1 px), ink
/// x 693–765 and y 64–79. Without the T→O kern it ends at 766.
#[test]
fn grid_mode_label_is_the_stock_22_px_bold() {
    let frame = ui_with(UiSnapshot {
        footswitch_mode: Some(FootswitchMode::Stomp),
        ..UiSnapshot::default()
    })
    .render();
    let area = Rect::new(686, 52, 106, 40);
    let runs = ink_runs(&frame, area);
    let (first, last) = (
        runs.first().expect("label ink").0,
        runs.last().expect("label ink").1,
    );
    assert_eq!((first, last), (693, 765), "label ink columns");
    let rows: Vec<i32> = (area.y..area.bottom())
        .filter(|&y| {
            (area.x..area.right()).any(|x| {
                let c = frame.pixel(x as u32, y as u32);
                c.0.max(c.1).max(c.2) > 128
            })
        })
        .collect();
    assert_eq!((rows[0], rows[rows.len() - 1]), (64, 79), "label rows");
}

/// Stock selection (`b4-stock1`): the selected tile's ring is 6 px wide (x 69..74) in the block's
/// own colour, and nothing white is drawn outside the tile.
#[test]
fn selected_tile_has_a_six_pixel_ring_in_its_hue() {
    let frame = ui_with(selected_overdrive()).render();
    let hue = ModelCategory::GuitarOverdrive
        .hue()
        .mix(Palette::STOCK.canvas, 123);
    for x in 69..=74 {
        assert_eq!(frame.pixel(x, 140), hue, "ring at x {x}");
    }
    assert_eq!(
        frame.pixel(75, 140),
        Palette::STOCK.canvas,
        "inside the ring"
    );
    assert_eq!(
        frame.pixel(68, 140),
        Palette::STOCK.canvas,
        "outside the tile"
    );
}

/// Stock editor keys (`b4-stock1`): ⋮, the scene selector, and bypass are `#181c18`, and the
/// scene letter badge is 24 px (x 556..579) centred between the chevrons.
#[test]
fn block_editor_keys_use_the_stock_fill_and_a_24_px_scene_badge() {
    let frame = ui_with(selected_overdrive()).render();
    let idle = tetrad_assets::Rgb(0x18, 0x1c, 0x18);
    assert_eq!(frame.pixel(12, 210), idle, "⋮ key");
    assert_eq!(frame.pixel(660, 210), idle, "bypass key");
    assert_eq!(
        frame.pixel(556, 226),
        Palette::STOCK.warning,
        "badge left edge"
    );
    assert_eq!(
        frame.pixel(579, 226),
        Palette::STOCK.warning,
        "badge right edge"
    );
    assert_eq!(frame.pixel(555, 226), idle, "left of the badge");
    assert_eq!(frame.pixel(580, 226), idle, "right of the badge");
}

/// Stock dropdown pixels (shot 025, RGB565 readback): the panel fill is 49/52/49, its
/// corner has a 10 px radius, and the active top row is clipped to that corner.
#[test]
fn scene_dropdown_panel_matches_stock_fill_and_corner() {
    let mut ui = ui_with(rich());
    let mut s = ui.snapshot().clone();
    s.selected_scene = scene(0);
    ui.set_snapshot(s).unwrap();
    activate(&mut ui, Control::SceneMenu);
    let frame = ui.render();
    let outside = frame.pixel(453, 44);
    assert_eq!(
        frame.pixel(650, 150),
        tetrad_assets::Rgb(0x31, 0x34, 0x31),
        "panel fill"
    );
    assert_eq!(
        frame.pixel(456, 46),
        outside,
        "a 10 px corner leaves (456, 46) clear"
    );
    assert_eq!(
        frame.pixel(455, 44),
        outside,
        "the active row is clipped to the corner"
    );
}

/// Every catalog category and sub-category takes its stock block glyph and colour
/// (`0x002eb99c`, `0x002eb380`). Tetrad
/// drew a wrench for every Utility and Tuner block and a refresh arrow for the Looper.
#[test]
fn block_glyphs_and_colours_follow_the_stock_device_table() {
    use tetrad_assets::{Icon as I, block};
    let info = |model: u32| BlockInfo {
        category: ModelCategory::of_model(model),
        model_id: Some(model),
        plugin: None,
    };
    let glyphs = [
        (1, I::Overdrive),
        (1001, I::Amp),
        (1184, I::NcAmpCab),
        (2010, I::Amp),
        (3000, I::Overdrive),
        (4000, I::Eq),
        (5001, I::Compressor),
        (6011, I::Delay),
        (7000, I::Modulation),
        (8000, I::Reverb),
        (9010, I::Wah),
        (10_000, I::InOut),
        (11_000, I::InOut),
        (12_000, I::Cab),
        (21_001, I::Cab),
        (32_000, I::Cab),
        (33_001, I::Cab),
        (13_000, I::FxSend),
        (13_001, I::FxSend),
        (13_006, I::FxSend),
        (13_002, I::FxReturn),
        (13_003, I::FxReturn),
        (13_007, I::FxReturn),
        (13_004, I::FxLoop),
        (13_005, I::FxLoop),
        (13_008, I::FxLoop),
        (14_000, I::NeuralCapture),
        (14_001, I::NeuralCapture),
        (15_000, I::Utility),
        (16_001, I::Utility),
        (16_006, I::Utility),
        (16_007, I::Utility),
        (16_008, I::Blend),
        (16_013, I::Blend),
        (16_011, I::Doubler),
        (16_012, I::Doubler),
        (18_010, I::Pitch),
        (18_012, I::Multivoicer),
        (24_001, I::Filter),
        (26_001, I::Freeze),
        (26_002, I::Morph),
        (26_003, I::BitCrusher),
        (26_004, I::BitCrusher),
        (26_006, I::Morph),
        (27_000, I::LooperReels),
        (27_002, I::LooperReels),
        (29_001, I::IrLoader),
        (30_001, I::Synth),
    ];
    for (model, icon) in glyphs {
        assert_eq!(info(model).icon(None), Some(icon), "model {model}");
    }
    // Capture `deviceType` codes 0..=8 (`0x2ebad0`).
    let captures = [
        I::NeuralCapture,
        I::Amp,
        I::ComboAmp,
        I::NcAmpCab,
        I::Cab,
        I::NcPedal,
        I::Fuzz,
        I::Compressor,
        I::Overdrive,
    ];
    for (code, icon) in (0..).zip(captures) {
        let device = CaptureDevice::from_code(code);
        assert_eq!(
            ModelCategory::NeuralCapture.model_icon(14_000, device),
            icon,
            "deviceType {code}"
        );
    }
    assert_eq!(info(26_001).hue(None), Some(block::FILTER));
    assert_eq!(info(26_003).hue(None), Some(block::DRIVE));
    assert_eq!(info(26_002).hue(None), Some(block::NEUTRAL));
    assert_eq!(info(3008).hue(None), Some(block::OTHER));
    assert_eq!(info(3000).hue(None), Some(block::DRIVE));
    let capture = |device| ModelCategory::NeuralCapture.model_hue(14_000, Some(device));
    assert_eq!(capture(CaptureDevice::Pedal), block::DRIVE);
    assert_eq!(capture(CaptureDevice::Cab), block::CAB);
    assert_eq!(capture(CaptureDevice::ComboAmp), block::AMP);
    assert_eq!(capture(CaptureDevice::Compressor), block::DYNAMICS);
    assert_eq!(capture(CaptureDevice::Default), block::NEUTRAL);
    assert_eq!(BlockInfo::default().icon(None), None);
}

/// Factory 1A "Brit 2203", row 1 block 7 (Adaptive Gate (S/C), 16007) on the QC: stock
/// draws the `utility.png` fan at 32 px with its box at tile + 16, (599, 131) on screen
/// (`st5a.png`, 2026-10-16). Tetrad drew a 40 px wrench there.
#[test]
fn a_utility_block_draws_the_stock_fan_at_32_px() {
    let position = cell(0, 6);
    let mut s = UiSnapshot::default();
    s.grid[position.index()].label = "Adaptive Gate (S/C)".into();
    s.blocks[position.index()] = BlockInfo {
        category: Some(ModelCategory::Utility),
        model_id: Some(16_007),
        plugin: None,
    };
    let ui = ui_with(s);
    let slot = (ui.hit_regions().into_iter())
        .find(|region| region.control == Control::SelectCell(position))
        .expect("the block is visible")
        .bounds;
    let (x, y) = (slot.x + 3 + 16, slot.y + 3 + 16);
    assert_eq!((x, y), (599, 131), "stock glyph box");
    let frame = ui.render();
    let at = |dx: i32, dy: i32| frame.pixel((x + dx) as u32, (y + dy) as u32);
    assert_eq!(at(5, 9), Palette::STOCK.text, "handle");
    assert_eq!(at(4, 27), Palette::STOCK.text, "ball");
    let blade = at(12, 12);
    assert!((100..180).contains(&blade.0), "grey blade {blade:?}");
    assert_eq!(
        at(22, 22),
        Palette::STOCK.canvas,
        "the wrench shaft is gone"
    );
}

/// The Grid picks the block glyph by model as stock does (`0x2eb99c`): a Doubler (16011)
/// draws `doubler.png`, not the Utility fan of its category.
#[test]
fn grid_blocks_use_the_model_glyph_not_only_the_category() {
    let position = cell(2, 1);
    let render = |model_id| {
        let mut s = UiSnapshot::default();
        s.grid[position.index()].label = "Block".into();
        s.blocks[position.index()] = BlockInfo {
            category: Some(ModelCategory::Utility),
            model_id: Some(model_id),
            plugin: None,
        };
        let ui = ui_with(s);
        let slot = (ui.hit_regions().into_iter())
            .find(|region| region.control == Control::SelectCell(position))
            .expect("the block is visible")
            .bounds;
        let frame = ui.render();
        (0..32)
            .flat_map(|dy| (0..32).map(move |dx| (dx, dy)))
            .map(|(dx, dy)| frame.pixel((slot.x + 19 + dx) as u32, (slot.y + 19 + dy) as u32))
            .collect::<Vec<_>>()
    };
    assert_ne!(
        render(16_011),
        render(16_007),
        "Doubler and the Utility fan differ"
    );
}

/// Factory 1A "Brit 2203" on the QC (`st5a.png`, 2026-10-16): stock draws ●▶ after "In 1"
/// (stored input flag), ▶● on the left edge of the Adaptive Gate (S/C) in row 1 column 7 (a
/// sink) and a grey ▶● on the bypassed Doubler in row 3 column 2 (`0x005a6004`, 20 %). The
/// pixels below are the stock ones: white square, black dot and ▶, a black rim over the cable.
#[test]
fn sidechain_marks_draw_as_stock_on_the_grid() {
    let mut s = UiSnapshot::default();
    s.rows[0] = RowView {
        input: InputPort::Input1,
        output: OutputPort::NextRow3,
        ..RowView::default()
    };
    s.rows[2] = RowView {
        input: InputPort::PrevRow,
        output: OutputPort::MultipleOuts,
        ..RowView::default()
    };
    for (position, bypassed) in [(cell(0, 6), false), (cell(2, 1), true)] {
        s.grid[position.index()] = GridCell {
            label: "Block".into(),
            bypassed,
        };
        s.blocks[position.index()].category = Some(ModelCategory::Utility);
        s.sidechain.sinks[position.index()] = true;
    }
    s.sidechain.inputs[0] = true;
    let frame = ui_with(s).render();
    let at = |x: u32, y: u32| frame.pixel(x, y);
    let (white, black) = (Palette::STOCK.text, Palette::STOCK.canvas);
    // "In 1" source, glyph box (46, 139): rim on the tile, square, dot, ▶; the cable from x 64.
    assert_eq!(
        [
            at(45, 146),
            at(53, 141),
            at(50, 147),
            at(55, 147),
            at(64, 147)
        ],
        [black, white, black, black, Palette::STOCK.cable]
    );
    // Row 1 column 7 sink, glyph box (579, 139): rim cuts the cable, ▶, square, dot.
    assert_eq!(
        [
            at(577, 147),
            at(582, 147),
            at(587, 147),
            at(590, 147),
            at(593, 147)
        ],
        [black, black, white, black, white]
    );
    // Bypassed row 3 column 2 sink, glyph box (151, 327): grey square, the cable through the ▶.
    assert_eq!(at(160, 330), white.mix(black, 204));
    assert_eq!(at(153, 334), Palette::STOCK.cable);
}

/// Stock `0x427db4` strokes a block tile with `drawRoundedRect(13, 13)` on the pen's centre
/// line, so a 2 px ring has an outer corner radius of 14 (`st-mp1a.png`, tile (69, 115)): the
/// left edge is black at tile row 8, partly lit from row 9 and fully lit from row 14.
#[test]
fn block_tile_corners_use_the_stock_radius() {
    let position = cell(0, 0);
    let mut s = UiSnapshot::default();
    s.grid[position.index()].label = "OD".into();
    s.blocks[position.index()].category = Some(ModelCategory::GuitarOverdrive);
    let ui = ui_with(s);
    let slot = (ui.hit_regions().into_iter())
        .find(|region| region.control == Control::SelectCell(position))
        .unwrap()
        .bounds;
    let frame = ui.render();
    let at = |dy: i32| frame.pixel(slot.x as u32 + 3, (slot.y + 3 + dy) as u32);
    assert_eq!(at(8), Palette::STOCK.canvas);
    assert_ne!(at(9), Palette::STOCK.canvas);
    assert_ne!(at(13), tetrad_assets::block::DRIVE);
    assert_eq!(at(14), tetrad_assets::block::DRIVE);
}

/// Factory 1A row 1 column 3 on the QC (`st5a.png`): a "pedal" capture draws `NC_pedal.png`
/// in the drive orange (`0x2eb99c`, `0x2eb380`) and the capture type badge, a 20 px disc at
/// tile (−5, 47) (`0x002ebf94` → `capture_type.png` at item (4, 54)); bypassed, the badge
/// is at opacity 0.2 (6/32 in Qt's RGB16 blend) on a black disc. Without a loaded type:
/// `NC.png`, grey outline, no badge.
#[test]
fn a_capture_block_shows_its_device_type_and_the_badge() {
    let position = cell(0, 2);
    let render = |capture: Option<CaptureDevice>, bypassed: bool| {
        let mut s = UiSnapshot::default();
        s.grid[position.index()] = GridCell {
            label: "Eltron 30".into(),
            bypassed,
        };
        s.blocks[position.index()] = BlockInfo {
            category: Some(ModelCategory::NeuralCapture),
            model_id: Some(14_000),
            plugin: None,
        };
        s.captures[position.index()] = capture;
        let ui = ui_with(s);
        let slot = (ui.hit_regions().into_iter())
            .find(|region| region.control == Control::SelectCell(position))
            .unwrap()
            .bounds;
        (ui.render(), slot.x + 3, slot.y + 3)
    };
    let (frame, x, y) = render(Some(CaptureDevice::Pedal), false);
    let at = |frame: &Frame, dx: i32, dy: i32| frame.pixel((x + dx) as u32, (y + dy) as u32);
    assert_eq!(at(&frame, 0, 32), tetrad_assets::block::DRIVE, "outline");
    assert_eq!(
        at(&frame, 31, 34),
        Palette::STOCK.text,
        "the pedal's big knob"
    );
    // Badge disc at tile (−5, 47): a bright stripe of its right half.
    assert_eq!(at(&frame, 9, 55), Palette::STOCK.text, "badge");
    let (dim, ..) = render(Some(CaptureDevice::Pedal), true);
    assert_eq!(
        at(&dim, 9, 55),
        Palette::STOCK.text.mix(Palette::STOCK.canvas, 208)
    );
    let (plain, ..) = render(None, false);
    assert_eq!(at(&plain, 0, 32), tetrad_assets::block::NEUTRAL);
    assert_eq!(at(&plain, 9, 55), Palette::STOCK.canvas, "no badge");
    assert_ne!(
        at(&plain, 31, 34),
        Palette::STOCK.text,
        "NC.png has no knob there"
    );
}

/// Gig View stomp icons follow the Grid (`BlockInfo::icon`, `0x002eb99c`): a Doubler (16011)
/// shows `doubler.png` where its category shows the Utility fan, and a pedal capture shows
/// `NC_pedal.png` where an unloaded Neural Capture shows `NC.png`.
#[test]
fn gig_stomp_icons_use_the_block_glyph_not_only_the_category() {
    let position = cell(0, 2);
    let render = |model_id, capture: Option<CaptureDevice>| {
        let mut snapshot = UiSnapshot {
            footswitch_mode: Some(FootswitchMode::Stomp),
            ..rich()
        };
        let category = ModelCategory::of_model(model_id);
        snapshot.blocks[position.index()] = BlockInfo {
            category,
            model_id: Some(model_id),
            plugin: None,
        };
        snapshot.captures[position.index()] = capture;
        snapshot.stomps[1] = StompView {
            label: "STOMP".into(),
            category,
            position: Some(position),
            blocks: vec![position],
            ..StompView::default()
        };
        let mut ui = ui_with(snapshot);
        swipe_up_to_gig(&mut ui);
        let frame = ui.render();
        // Tile B sits at (206, 76); its icon box is 70 px at tile + 12.
        (0..70_u32)
            .flat_map(|dy| (0..70_u32).map(move |dx| (dx, dy)))
            .map(|(dx, dy)| frame.pixel(218 + dx, 88 + dy))
            .collect::<Vec<_>>()
    };
    assert_ne!(
        render(16_011, None),
        render(16_007, None),
        "Doubler and the Utility fan differ"
    );
    assert_ne!(
        render(14_000, Some(CaptureDevice::Pedal)),
        render(14_000, None),
        "a pedal capture and an unloaded capture differ"
    );

    // An engaged stomp fills with the block colour (`BlockInfo::hue`): a pedal capture is
    // tinted like its Grid block, not with the Neural Capture category colour.
    let fill = |capture: Option<CaptureDevice>| {
        let category = ModelCategory::of_model(14_000);
        let mut snapshot = UiSnapshot {
            footswitch_mode: Some(FootswitchMode::Stomp),
            ..rich()
        };
        snapshot.blocks[position.index()] = BlockInfo {
            category,
            model_id: Some(14_000),
            plugin: None,
        };
        snapshot.captures[position.index()] = capture;
        snapshot.stomps[1] = StompView {
            label: "STOMP".into(),
            category,
            position: Some(position),
            blocks: vec![position],
            engaged: true,
            ..StompView::default()
        };
        let mut ui = ui_with(snapshot);
        swipe_up_to_gig(&mut ui);
        // Inside tile B, left of its icon box.
        ui.render().pixel(212, 120)
    };
    let pedal = BlockInfo {
        category: ModelCategory::of_model(14_000),
        model_id: Some(14_000),
        plugin: None,
    };
    assert_eq!(
        fill(Some(CaptureDevice::Pedal)),
        pedal.hue(Some(CaptureDevice::Pedal)).unwrap()
    );
    assert_ne!(fill(Some(CaptureDevice::Pedal)), fill(None));
}

/// Stock 103 (tile B): a Neural Capture with a device type shows the capture badge on its Gig
/// stomp. The 24 px black disc sits at (−2, +48) of the 70 px icon box (218, 88). The probe
/// (228, 158) is below the box (y 88..157), where only the disc's backing can paint.
#[test]
fn gig_stomp_shows_the_capture_type_badge() {
    let position = cell(0, 2);
    let render = |capture: Option<CaptureDevice>| {
        let mut snapshot = UiSnapshot {
            footswitch_mode: Some(FootswitchMode::Stomp),
            ..rich()
        };
        let category = ModelCategory::of_model(14_000);
        snapshot.blocks[position.index()] = BlockInfo {
            category,
            model_id: Some(14_000),
            plugin: None,
        };
        snapshot.captures[position.index()] = capture;
        snapshot.stomps[1] = StompView {
            label: "STOMP".into(),
            category,
            position: Some(position),
            blocks: vec![position],
            ..StompView::default()
        };
        let mut ui = ui_with(snapshot);
        swipe_up_to_gig(&mut ui);
        ui.render()
    };
    let badged = render(Some(CaptureDevice::Pedal));
    let plain = render(None);
    assert_eq!(badged.pixel(228, 158), Palette::STOCK.canvas, "badge disc");
    assert_ne!(
        plain.pixel(228, 158),
        Palette::STOCK.canvas,
        "no badge without a device type"
    );
    // The striped glyph is white in the disc, below the block glyph (which ends at y 143).
    let striped = |frame: &Frame| {
        (222..240).any(|x| (144..158).any(|y| frame.pixel(x, y) == Palette::STOCK.text))
    };
    assert!(striped(&badged), "striped glyph");
    assert!(!striped(&plain), "no glyph without a device type");
}

/// Stock "Multiple devices": a switch on several blocks shows `multiple_stomps.png` in place of
/// its first block's glyph. More blocks than two do not change it.
#[test]
fn gig_stomp_on_several_blocks_shows_the_multiple_devices_glyph() {
    let render = |blocks: Vec<GridPosition>| {
        let mut snapshot = UiSnapshot {
            footswitch_mode: Some(FootswitchMode::Stomp),
            ..rich()
        };
        let category = ModelCategory::of_model(16_011);
        snapshot.blocks[cell(0, 2).index()] = BlockInfo {
            category,
            model_id: Some(16_011),
            plugin: None,
        };
        snapshot.stomps[1] = StompView {
            label: "STOMP".into(),
            category,
            position: Some(cell(0, 2)),
            blocks,
            ..StompView::default()
        };
        let mut ui = ui_with(snapshot);
        swipe_up_to_gig(&mut ui);
        let frame = ui.render();
        // Tile B's 70 px icon box at (218, 88).
        (0..70_u32)
            .flat_map(|dy| (0..70_u32).map(move |dx| (dx, dy)))
            .map(|(dx, dy)| frame.pixel(218 + dx, 88 + dy))
            .collect::<Vec<_>>()
    };
    let one = render(vec![cell(0, 2)]);
    let two = render(vec![cell(0, 2), cell(0, 3)]);
    assert_ne!(one, two, "two blocks show the Multiple devices glyph");
    assert_eq!(two, render(vec![cell(0, 2), cell(0, 3), cell(0, 4)]));
}

/// Stock editor header (`b4-stock1`): "Freeman BOD" at 20 px (cap ink y 231..244), and with a
/// Virtual Device Preset match the 16 px layers glyph in the name colour after it (stock box
/// x 188, y 229; Tetrad's name is 1 px wider, so Tetrad's box is at x 189).
#[test]
fn block_editor_header_marks_a_preset_match_after_the_model_name() {
    let hue = ModelCategory::GuitarOverdrive.hue();
    let render = |matched: Option<PresetMatch>| {
        let mut s = selected_overdrive();
        s.editor_preset = matched;
        ui_with(s).render()
    };
    let plain = render(None);
    let marked = render(Some(PresetMatch::CatalogDefault));
    let ink_rows = (215..255)
        .filter(|&y| (62..186).any(|x| plain.pixel(x, y) == hue))
        .collect::<Vec<_>>();
    assert_eq!(ink_rows.first().zip(ink_rows.last()), Some((&231, &244)));
    let (x, y) = (189, 229);
    // Diamond row 3 is solid from column 2 to 13; chevron row 8 is solid at columns 2..=3
    // and open at column 7.
    for column in 2..=13 {
        assert_eq!(
            marked.pixel(x + column, y + 3),
            hue,
            "diamond column {column}"
        );
    }
    assert_eq!(marked.pixel(x + 2, y + 8), hue);
    assert_eq!(marked.pixel(x + 7, y + 8), plain.pixel(x + 7, y + 8));
    let glyph_box = (x - 1..x + 17).flat_map(|px| (y - 1..y + 17).map(move |py| (px, py)));
    assert!(glyph_box.clone().all(|(px, py)| plain.pixel(px, py) != hue));
    assert!(
        glyph_box
            .filter(|&(px, py)| marked.pixel(px, py) == hue)
            .count()
            > 60
    );
}

/// Stock title area (`0x4ba858`): one 200 × 60 button right of ⋮ from the editor top. A tap on
/// the preset glyph opens the device list on the block's group, as a tap on the name does
/// (`onTitleAreaClicked` `0x4be624`); stock has no glyph target.
#[test]
fn block_editor_title_area_is_the_stock_200_by_60_button() {
    let mut s = selected_overdrive();
    s.editor_preset = Some(PresetMatch::CatalogDefault);
    let ui = ui_with(s.clone());
    let title = Control::Param(ParamControl::ChangeModel);
    assert_eq!(ui.hit_test(197, 237), Some(title), "preset glyph");
    assert_eq!(ui.hit_test(60, 196), Some(title));
    assert_eq!(ui.hit_test(259, 255), Some(title));
    assert_ne!(ui.hit_test(260, 220), Some(title));
    assert_ne!(ui.hit_test(100, 256), Some(title));
    let mut ui = ui_with(s);
    tap(&mut ui, 197, 237);
    assert_eq!(ui.screen(), Screen::ModelPicker);
    assert_eq!(ui.picker.presets, None);
    assert_eq!(
        ui.picker.group,
        DeviceGroup::of(ModelCategory::GuitarOverdrive).map(|g| g.0)
    );
}

/// Hardware B1 (`sl-2.png`, stock 4.1.0): a parallel path of the upper pair (split 0, mix 7)
/// draws path B from column 0 to column 7, the two junction cables at x 60–61 and 739–740
/// with 4 px bends and no S/M tokens, and fades row 2's own `+` ports to `#181818` with no
/// tile. A tap on a cable opens the junction editor.
#[test]
fn a_parallel_path_draws_junction_cables_and_fades_path_b_ports() {
    let mut s = UiSnapshot::default();
    s.rows[0] = RowView {
        input: InputPort::UsbIn5,
        output: OutputPort::MultipleOuts,
        ..RowView::default()
    };
    for (position, category) in [
        (cell(0, 0), ModelCategory::GuitarOverdrive),
        (cell(1, 2), ModelCategory::GuitarOverdrive),
    ] {
        s.grid[position.index()].label = "OD".into();
        s.blocks[position.index()].category = Some(category);
    }
    s.pairs[0] = PairView {
        split: Some(0),
        mix: Some(7),
    };
    let ui = ui_with(s);
    let frame = ui.render();
    let cable = |x, y| frame.pixel(x, y) == Palette::STOCK.cable;
    // Junction cables between the rows (centres y 147 and 241).
    for y in [160, 194, 228] {
        assert!(cable(60, y) && cable(61, y), "split cable at y {y}");
        assert!(cable(739, y) && cable(740, y), "mix cable at y {y}");
        assert!(!cable(58, y) && !cable(63, y) && !cable(742, y));
    }
    // Path B: from its first column to its last; the bends leave no square corner.
    for x in [70, 200, 400, 734] {
        assert!(cable(x, 240) && cable(x, 241), "path B at x {x}");
    }
    assert!(!cable(61, 241) && !cable(740, 241), "bent corners");
    assert!(
        !cable(50, 241) && !cable(746, 241),
        "no cable into the faded ports"
    );
    // No S / M tokens on path A.
    assert!(cable(200, 147) && cable(700, 147));
    // Faded path B ports: the `+` alone; the live row 3 ports keep their tile.
    let faded = tetrad_assets::Rgb(0x18, 0x18, 0x18);
    assert_eq!(frame.pixel(30, 241), faded);
    assert_eq!(frame.pixel(770, 241), faded);
    assert_eq!(frame.pixel(12, 241), Palette::STOCK.canvas);
    assert_ne!(
        frame.pixel(12, 335),
        Palette::STOCK.canvas,
        "row 3 keeps its tile"
    );
    assert!(!shows(
        &ui,
        Control::Port {
            row: Row(1),
            output: false
        }
    ));
    assert!(shows(
        &ui,
        Control::Port {
            row: Row(2),
            output: false
        }
    ));
    assert_eq!(
        tap(&mut ui_with(ui.snapshot().clone()), 61, 194),
        vec![UiCommand::InspectSite {
            site: EditorSite::Splitter(RowPair::Upper)
        }]
    );
    // A stored port under the Mixer: its label at opacity 0.2 instead of the "+"
    // (`st-mp1g.png` row 4: a faded "Multi Out").
    let mut s = ui.snapshot().clone();
    s.rows[1].replaced_output = OutputPort::MultipleOuts;
    let frame = ui_with(s).render();
    let tile = (750..794).flat_map(|x| (203..279).map(move |y| (x, y)));
    let text = tetrad_assets::Rgb(0x33, 0x33, 0x33);
    assert!(tile.clone().any(|(x, y)| frame.pixel(x, y) == text));
    assert!((761..783).any(|x| frame.pixel(x, 240) != faded), "no +");
    assert!(
        tile.clone().all(|(x, y)| frame.pixel(x, y).0 <= 0x33),
        "no tile, dim text"
    );
}

/// The cable layout of stock "1D Royal Blood" (`st-rb.png`): rows 1–2 split at column 2 with
/// path B ending at its own `Row 3` port, rows 3–4 split at column 5 and mixed after column 6.
fn royal_blood_grid() -> UiSnapshot {
    let mut s = UiSnapshot::default();
    s.rows[0] = RowView {
        input: InputPort::Input1,
        output: OutputPort::MultipleOuts,
        ..RowView::default()
    };
    s.rows[1].output = OutputPort::NextRow3;
    s.rows[2] = RowView {
        input: InputPort::PrevRow,
        output: OutputPort::MultipleOuts,
        ..RowView::default()
    };
    for (row, column) in [
        (0, 0),
        (0, 5),
        (0, 6),
        (0, 7),
        (1, 1),
        (2, 5),
        (2, 6),
        (2, 7),
    ] {
        let position = cell(row, column);
        s.grid[position.index()].label = "OD".into();
        s.blocks[position.index()].category = Some(ModelCategory::GuitarOverdrive);
    }
    for column in [2, 5, 6] {
        let position = cell(3, column);
        s.grid[position.index()].label = "OD".into();
        s.blocks[position.index()].category = Some(ModelCategory::GuitarOverdrive);
    }
    s.pairs = [
        PairView {
            split: Some(2),
            mix: None,
        },
        PairView {
            split: Some(5),
            mix: Some(6),
        },
    ];
    s
}

/// Stock split cables from an inner gap (`st-rb.png`, `stock-home-20261015.png`, both "1D
/// Royal Blood" or "3D Worship V5" rows 3–4): the cable drops from the gap to mid-way between
/// the rows (y 194 or 382), runs back to x 58 and drops into path B at x 66; the mix cable
/// runs out to x 744 the same way. No straight joiner crosses between the rows.
#[test]
fn split_cables_from_inner_gaps_detour_to_path_b_ends() {
    let frame = ui_with(royal_blood_grid()).render();
    let cable = |x, y| frame.pixel(x, y) == Palette::STOCK.cable;
    let vertical = |x, ys: [u32; 3]| ys.into_iter().all(|y| cable(x - 1, y) && cable(x, y));
    let horizontal = |y, xs: [u32; 3]| xs.into_iter().all(|x| cable(x, y - 1) && cable(x, y));
    // Rows 1–2: split at gap 2 (x 228), no mix: row 2 keeps its own `Row 3` port.
    assert!(vertical(228, [158, 170, 184]), "drop from row 1");
    assert!(horizontal(194, [72, 140, 214]), "back to path B's start");
    assert!(vertical(58, [204, 220, 232]), "drop into row 2");
    assert!(horizontal(241, [68, 100, 140]), "row 2 from x 66");
    assert!(!cable(228, 220) && !cable(58, 241) && !cable(228, 194));
    assert!(!cable(744, 220), "no mix cable");
    // Rows 3–4: split at gap 5 (x 486), mix at gap 7 (x 658).
    assert!(vertical(486, [346, 360, 372]), "split drop from row 3");
    assert!(horizontal(382, [80, 300, 470]), "split run");
    assert!(vertical(58, [392, 410, 420]), "split drop into row 4");
    assert!(vertical(658, [346, 360, 372]), "mix drop from row 3");
    assert!(horizontal(382, [672, 700, 730]), "mix run");
    assert!(vertical(744, [392, 410, 420]), "mix drop from row 4");
    assert!(horizontal(429, [68, 650, 734]), "row 4 from x 66 to 736");
    for x in [486, 658] {
        assert!(
            !cable(x, 400) && !cable(x, 382),
            "no straight joiner at x {x}"
        );
    }
    for (x, y) in [(58, 382), (744, 382), (58, 429), (744, 429)] {
        assert!(!cable(x, y), "bent corner at ({x}, {y})");
    }
}

/// Stock cable gaps (`sn-0`, `gd-s1`, `sl-1`): 2 px between a cable and an I/O tile (tiles
/// end at x 51 and start at 750), 3 px either side of an active block's tile (cable to x 65,
/// tile 69–132, cable from 136). A bypassed block cuts nothing: the cable runs over its
/// ring and glyph.
#[test]
fn cables_stop_short_of_ports_and_active_blocks_and_cross_bypassed_ones() {
    let mut s = UiSnapshot::default();
    s.rows[0] = RowView {
        input: InputPort::UsbIn5,
        output: OutputPort::MultipleOuts,
        ..RowView::default()
    };
    for column in [0, 1] {
        let position = cell(0, column);
        s.grid[position.index()].label = "OD".into();
        s.blocks[position.index()].category = Some(ModelCategory::GuitarOverdrive);
    }
    s.grid[cell(0, 1).index()].bypassed = true;
    let frame = ui_with(s).render();
    let cable = |x| {
        frame.pixel(x, 147) == Palette::STOCK.cable && frame.pixel(x, 146) == Palette::STOCK.cable
    };
    let cut = |x| frame.pixel(x, 147) != Palette::STOCK.cable;
    for x in [54, 65, 136, 151, 155, 186, 218, 222, 747] {
        assert!(cable(x), "cable at x {x}");
    }
    for x in [52, 53, 66, 68, 133, 135, 748, 749] {
        assert!(cut(x), "gap at x {x}");
    }
    assert_eq!(frame.pixel(749, 120), Palette::STOCK.canvas);
    assert_ne!(
        frame.pixel(750, 120),
        Palette::STOCK.canvas,
        "Multi Out tile from x 750"
    );
    assert_ne!(frame.pixel(793, 120), Palette::STOCK.canvas);
    assert_eq!(frame.pixel(794, 120), Palette::STOCK.canvas);
}

/// Stock bypassed block editor (`0x4b5918`, `sl-1` against `b4-stock1`): the parameter cells
/// are drawn at opacity 0.3 (white text reads `#525552`, the `#ff7100` arc `#522c08`), the
/// bypass key shows the bypassed pixmap (`0x6869ec`: the ring in the block colour at x
/// 676–693, cut at y 224–227, a white bar at y 225–226 from x 673 to 696), and the header
/// keys keep full colour. Three parameters take one row: the card ends at y 362 and the
/// divider after the last cell (x 478) is the last one.
#[test]
fn a_bypassed_block_editor_dims_its_cells_and_shows_the_bypassed_glyph() {
    let near = |got: tetrad_assets::Rgb, stock: tetrad_assets::Rgb| {
        let channel = |a: u8, b: u8| a.abs_diff(b) <= 10;
        channel(got.0, stock.0) && channel(got.1, stock.1) && channel(got.2, stock.2)
    };
    let mut s = selected_overdrive();
    s.parameters = ["OVERDRIVE", "TONE", "LEVEL"]
        .into_iter()
        .enumerate()
        .map(|(index, label)| continuous(index as u16, label, 0.5))
        .collect();
    let active = ui_with(s.clone()).render();
    s.grid[0].bypassed = true;
    let bypassed = ui_with(s).render();
    let hue = ModelCategory::GuitarOverdrive.hue();
    let idle = tetrad_assets::Rgb(0x18, 0x1c, 0x18);
    // The arc at the left of the first knob (centre 122.5, 327.5, radius about 30).
    let arc = (80..105)
        .map(|x| (x, 328))
        .find(|&(x, y)| active.pixel(x, y) == hue)
        .expect("an active arc pixel");
    assert!(near(
        bypassed.pixel(arc.0, arc.1),
        tetrad_assets::Rgb(0x52, 0x2c, 0x08)
    ));
    let label = (18..60)
        .flat_map(|x| (268..282).map(move |y| (x, y)))
        .find(|&(x, y)| active.pixel(x, y) == Palette::STOCK.text)
        .expect("a label pixel");
    assert!(near(
        bypassed.pixel(label.0, label.1),
        tetrad_assets::Rgb(0x52, 0x55, 0x52)
    ));
    assert!(
        near(
            bypassed.pixel(60, 300),
            tetrad_assets::Rgb(0x10, 0x14, 0x10)
        ),
        "cell fill"
    );
    // The bypass key.
    assert_eq!(bypassed.pixel(677, 221), hue, "ring");
    assert_eq!(bypassed.pixel(685, 221), idle, "inside the ring");
    assert_eq!(bypassed.pixel(676, 224), idle, "the cut");
    for x in [673, 685, 696] {
        assert_eq!(bypassed.pixel(x, 225), Palette::STOCK.text, "bar at x {x}");
        assert_eq!(bypassed.pixel(x, 226), Palette::STOCK.text, "bar at x {x}");
    }
    assert_eq!(bypassed.pixel(672, 225), idle);
    assert_ne!(active.pixel(677, 221), hue, "active: the power glyph");
    assert_eq!(bypassed.pixel(660, 210), idle, "keys are not dimmed");
    assert_eq!(bypassed.pixel(730, 210), Palette::STOCK.panel);
    // One row of cells, dividers up to the last cell.
    let page = active.pixel(400, 400);
    assert_eq!(active.pixel(400, 362), crate::widgets::CARD);
    assert_eq!(active.pixel(400, 363), page);
    assert_eq!(active.pixel(478, 300), Palette::STOCK.divider);
    assert_eq!(active.pixel(635, 300), crate::widgets::CARD);
}

/// Stock bypassed card (`sl-1`, device readback): the card reads `#101410`, the divider and page
/// `#101010`. The 0.3 layer over the `#101010` page only turns green after RGB565 truncation (green
/// keeps 6 bits, red and blue keep 5). The cell content goes at 0.3 over the faded card.
#[test]
fn bypassed_card_reads_back_green_like_the_stock_565_panel() {
    let mut s = selected_overdrive();
    s.parameters = ["OVERDRIVE", "TONE", "LEVEL"]
        .into_iter()
        .enumerate()
        .map(|(index, label)| continuous(index as u16, label, 0.5))
        .collect();
    s.grid[0].bypassed = true;
    let frame = ui_with(s).render();
    let card = tetrad_assets::Rgb(0x10, 0x14, 0x10);
    let page = tetrad_assets::Rgb(0x10, 0x10, 0x10);
    assert_eq!(device(frame.pixel(60, 300)), card, "card");
    assert_eq!(
        device(frame.pixel(790, 300)),
        card,
        "card at the right edge"
    );
    assert_eq!(device(frame.pixel(164, 300)), page, "cell divider");
    assert_eq!(device(frame.pixel(400, 400)), page, "page below the card");
    // The brightest pixel of the "OVERDRIVE" label is its white stem.
    let text = (18..60)
        .flat_map(|x| (268..282).map(move |y| (x, y)))
        .map(|(x, y)| frame.pixel(x, y))
        .max_by_key(|c| c.0)
        .map(device)
        .expect("label ink");
    // Full white over the faded card, as stock's needle; stock's label stems peak one green
    // step lower (#525552) because its glyph stems are not fully covered.
    let white = tetrad_assets::Rgb(0x52, 0x59, 0x52);
    assert_eq!(text, white, "label");
    // The first knob as `sl-1` reads it: face, needle and arc each painted at 0.3 over the
    // faded card.
    assert_eq!(
        device(frame.pixel(122, 335)),
        tetrad_assets::Rgb(0x10, 0x18, 0x10),
        "face"
    );
    let knob: Vec<_> = (85..160)
        .flat_map(|x| (290..362).map(move |y| (x, y)))
        .map(|(x, y)| device(frame.pixel(x, y)))
        .collect();
    assert!(knob.contains(&white), "needle");
    assert!(knob.contains(&tetrad_assets::Rgb(0x52, 0x2c, 0x08)), "arc");
}

/// Stock card rows (`b4-stock1`, seven parameters): cells on a 157 px pitch, row 2 from y 365,
/// and in a part row no divider after the cell that follows the last one.
#[test]
fn block_editor_draws_a_cell_only_for_each_parameter() {
    let mut s = selected_overdrive();
    s.parameters = (0..7).map(|index| continuous(index, "P", 0.5)).collect();
    let frame = ui_with(s).render();
    for (x, y) in [
        (164, 300),
        (321, 300),
        (478, 300),
        (635, 300),
        (164, 410),
        (321, 410),
    ] {
        assert_eq!(
            frame.pixel(x, y),
            Palette::STOCK.divider,
            "divider at ({x}, {y})"
        );
    }
    for (x, y) in [(478, 410), (635, 410), (164, 360), (164, 270)] {
        assert_eq!(
            frame.pixel(x, y),
            crate::widgets::CARD,
            "card at ({x}, {y})"
        );
    }
    assert_eq!(frame.pixel(400, 364), Palette::STOCK.divider, "row gap");
    assert_eq!(frame.pixel(400, 365), crate::widgets::CARD, "row 2");
}

/// Stock `updateMode` (`0x674ed0`): a stored Preset MIDI Out message (`0x0032da4c`) puts
/// `icons/mainMenuPopup/midi.png` in the mode glyph's place in every mode but Preset (`sn-0`;
/// `st0` without messages keeps the footswitch, in the same 24 px box). The disc is 22 px
/// with pin holes (`sn-0`: x 657–678, y 61–81, top pin at y 63–65).
#[test]
fn mode_button_shows_the_midi_glyph_while_the_preset_has_midi_out() {
    let ui = |mode: FootswitchMode, stored: bool| {
        let mut s = UiSnapshot {
            footswitch_mode: Some(mode),
            ..UiSnapshot::default()
        };
        s.preset_midi.stored = stored;
        ui_with(s)
    };
    let glyph = |ui: &UiState| {
        ui.layout()
            .items
            .into_iter()
            .find_map(|item| match item.look {
                crate::layout::Look::Glyph { icon, .. } if item.bounds.y == 60 => {
                    Some((icon, item.bounds))
                }
                _ => None,
            })
    };
    let stomp = ui(FootswitchMode::Stomp, false);
    let midi = ui(FootswitchMode::Stomp, true);
    let (footswitch, box_) = glyph(&stomp).unwrap();
    assert_eq!(footswitch, tetrad_assets::Icon::Footswitch);
    assert_eq!(glyph(&midi), Some((tetrad_assets::Icon::Midi, box_)));
    assert_eq!(
        glyph(&ui(FootswitchMode::Scene, true)).map(|(icon, _)| icon),
        Some(tetrad_assets::Icon::Midi)
    );
    assert_eq!(
        glyph(&ui(FootswitchMode::Preset, true)).map(|(icon, _)| icon),
        Some(tetrad_assets::Icon::ModePreset)
    );
    let frame = midi.render();
    let (cx, cy) = (box_.x as u32 + 12, box_.y as u32 + 11);
    let white = |x: u32, y: u32| frame.pixel(x, y) == Palette::STOCK.text;
    assert!(white(cx, cy) && white(cx, cy + 6) && white(cx - 10, cy));
    assert!(!white(cx, cy - 7), "top pin hole");
    assert!(
        !white(cx - 7, cy + 1) && !white(cx + 7, cy + 1),
        "side pin holes"
    );
    assert!(!white(cx, cy + 10), "key notch");
    assert!(!white(cx - 12, cy) && !white(cx + 12, cy), "22 px disc");
}

/// Stock `0x427db4`: a Capture 2.0 block (model 14001, `0x6966d4`) has a second 2 px ring
/// 5 px inside its tile at opacity 0.6 (`nc-s1`: tile y 115–178, inner ring y 120–121 and
/// 172–173 at x 356); a 14000 capture has none. It is not a selection.
#[test]
fn capture_two_blocks_draw_the_stock_inner_ring() {
    let frame = |model| {
        let mut s = UiSnapshot::default();
        let position = cell(0, 3);
        s.grid[position.index()].label = "NC".into();
        s.blocks[position.index()].category = Some(ModelCategory::NeuralCapture);
        s.blocks[position.index()].model_id = Some(model);
        ui_with(s).render()
    };
    let two = frame(14_001);
    let ring = two.pixel(356, 116);
    let inner = ring.mix(Palette::STOCK.canvas, 102);
    for y in [120, 121, 172, 173] {
        assert_eq!(two.pixel(356, y), inner, "inner ring at y {y}");
    }
    for y in [118, 123, 170, 175] {
        assert_eq!(two.pixel(356, y), Palette::STOCK.canvas, "black at y {y}");
    }
    assert_eq!(frame(14_000).pixel(356, 120), Palette::STOCK.canvas);
}

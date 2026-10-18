//! The UI bridge on the shared simulated device (`common`): the first frame, a grid tap that
//! opens the parameter panel, a knob turn that edits the parameter through tetrad-core, a scene
//! tap, and layout edits that commit through a DSP redeploy.

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use common::{Log, Media, STOCK, Sent, Settings, Sim, light_preset, target};
use tetrad_app::device::actions::SceneClipboard;
use tetrad_app::device::leds::UiLeds;
use tetrad_app::device::platform::TouchSample;
use tetrad_app::device::preset::{SpecsCatalog, UndoPath, prepare};
use tetrad_app::device::ui::{Request, Ui, UiInput};
use tetrad_app::device::{Action, Current, Device, Effect, Event, State};
use tetrad_catalog::Specs;
use tetrad_core::performance::footswitch::BankOverlay;
use tetrad_dsp_host::session::Clock;
use tetrad_dsp_protocol::CoreId;
use tetrad_engine::{Column, Row, SceneIndex as EngineScene, Site, Slot as EngineSlot};
use tetrad_hal::platform::{Platform as HalPlatform, Product};
use tetrad_preset::{Model, ParamValue, Preset, Slot, StompRole};
use tetrad_settings::device::{DeviceSettings, Hardware};
use tetrad_storage::media::MediaRoot;
use tetrad_ui::{
    Control, ExpressionControl, ExpressionLink, ExpressionPedal, ExpressionTarget, GridPosition,
    InputEvent, KnobIndex, MediaControl, MediaEntry, MediaFolder, MediaKind, NumericKey,
    ParamControl, ParameterKind, PickerControl, SceneIndex, Screen, SettingsPage,
};

/// Boots the simulated device to its first deployed preset ("Light" in row 1, column 3).
fn boot() -> (Device<Sim>, Rc<RefCell<Log>>, Media) {
    common::boot(media())
}

/// The [`boot`] media, for tests that store `zenui.db` or `factory.db` rows before the boot.
fn media() -> Media {
    Media::new([("Light", light_preset(&STOCK.specs(), 2))])
}

/// A tap: press and release at one point, 50 ms apart.
fn tap(ui: &mut Ui, state: &State, at: TouchSample, now: Duration) -> Vec<Request> {
    let mut requests = ui.handle(UiInput::Touch(at), state, now);
    let release = TouchSample {
        pressed: false,
        ..at
    };
    requests.extend(ui.handle(
        UiInput::Touch(release),
        state,
        now + Duration::from_millis(50),
    ));
    requests
}

/// The stock ⋮ menu shows seven rows: a 360 px drag up on it brings the lower items on screen.
fn scroll_menu(ui: &mut Ui, state: &State, now: Duration) {
    let at = |y, pressed| TouchSample { x: 600, y, pressed };
    ui.handle(UiInput::Touch(at(400, true)), state, now);
    ui.handle(
        UiInput::Touch(at(40, true)),
        state,
        now + Duration::from_millis(50),
    );
    ui.handle(
        UiInput::Touch(at(40, false)),
        state,
        now + Duration::from_millis(100),
    );
}

/// [`scroll_menu`] through the device.
fn scroll_device_menu(device: &mut Device<Sim>) {
    for (y, pressed) in [(400, true), (40, true), (40, false)] {
        device.push(Event::UiInput(UiInput::Touch(TouchSample {
            x: 600,
            y,
            pressed,
        })));
        device.step();
    }
}

/// Device actions without the LED and editor view reports: the bridge sends
/// `Action::UiLeds` and `Action::EditorBlock` until the reducer stores them in `State`, and
/// most tests here do not reduce.
fn edits(requests: &[Request]) -> Vec<&Action> {
    requests
        .iter()
        .filter_map(|request| match request {
            Request::Action(Action::UiLeds(_) | Action::EditorBlock(_)) => None,
            Request::Action(Action::Undoable(_, action)) => Some(&**action),
            Request::Action(action) => Some(action),
            _ => None,
        })
        .collect()
}

/// The undo paths of the [`Action::Undoable`] requests.
fn undo_paths(requests: &[Request]) -> Vec<UndoPath> {
    (requests.iter())
        .filter_map(|request| match request {
            Request::Action(Action::Undoable(path, _)) => Some(*path),
            _ => None,
        })
        .collect()
}

/// Activates `control`, which must be on screen, and runs one device pass.
fn activate(device: &mut Device<Sim>, control: Control) {
    let ui = &device.bridges().ui;
    let shown = ui
        .view()
        .hit_regions()
        .iter()
        .any(|region| region.enabled && region.control == control);
    assert!(shown, "{control:?} is not on screen");
    device.push(Event::UiInput(UiInput::View(InputEvent::Activate(control))));
    device.step();
}

/// Start-up presents one full 800×480 RGB8 frame (splash 90 → 100, `0x545ac0`), built from
/// the deployed preset, and the grid shows its block under the catalog name.
#[test]
fn start_up_presents_the_projected_grid() {
    let (device, log, _media) = boot();
    assert_eq!(log.borrow().frames, [800 * 480 * 3]);
    let state = device.state();
    let mut ui = Ui::new();
    ui.build(state).unwrap();
    assert_eq!(ui.render(state).unwrap().len(), 800 * 480 * 3);
    let snapshot = ui.view().snapshot();
    let block = GridPosition::new(0, 2).unwrap();
    assert_eq!(snapshot.grid[2].label, "Light");
    assert_eq!(snapshot.blocks[2].model_id, Some(1));
    assert_eq!(
        snapshot.preset_slot.map(|slot| slot.label()),
        Some("1A".into())
    );
    assert_eq!(snapshot.presets[0].title, "Light");
    assert_eq!(snapshot.selected_block, None);
    target(&ui, Control::SelectCell(block));
}

/// I/O meters arrive about 12 times a second, signal or not; on the grid they cost no frame
/// unless a port clip light changes (the QC rendered 116 unchanged frames in 10 idle s).
#[test]
fn idle_meters_cost_no_frame_but_a_clip_light_does() {
    use tetrad_dsp_host::responses::Event as DspEvent;
    use tetrad_dsp_protocol::responses::MeterRecord;
    let (mut device, _log, _media) = boot();
    device.step();
    device.take_renders();
    let in1 = |db: f32| {
        Event::Dsp(
            CoreId::Control,
            DspEvent::Meter(MeterRecord {
                core: 4,
                column: 0,
                index: 0x4000,
                value_bits: db.to_bits(),
            }),
        )
    };
    for _ in 0..12 {
        device.push(in1(-60.0));
        device.step();
    }
    assert_eq!(device.take_renders(), 0);
    device.push(in1(0.0));
    device.step();
    assert_eq!(device.take_renders(), 1);
}

/// Grid tap → `InspectCell` → the bridge selects the block and supplies its parameters as
/// tetrad-core text; a knob step becomes a `0x5cca94` encoder edit at the block's site; a scene
/// tap becomes a scene recall. Neither touches `State` (that is the action reducer's job).
#[test]
fn grid_tap_knob_and_scene_become_device_edits() {
    let (device, _log, _media) = boot();
    let state = device.state();
    let mut ui = Ui::new();
    ui.build(state).unwrap();
    let block = GridPosition::new(0, 2).unwrap();
    let now = Duration::from_secs(5);

    let cell = target(&ui, Control::SelectCell(block));
    let requests = tap(&mut ui, state, cell, now);
    assert!(edits(&requests).is_empty(), "selection is UI-local");
    // The open block editor goes to the LED view (guard `0x5ad4c8`).
    let editor = UiLeds {
        editor: true,
        ..UiLeds::default()
    };
    assert!(requests.contains(&Request::Action(Action::UiLeds(editor))));
    assert!(requests.contains(&Request::Effect(Effect::Present)));
    ui.render(state).unwrap();
    let snapshot = ui.view().snapshot();
    assert_eq!(snapshot.selected_block, Some(block));
    let gain = &snapshot.parameters[0];
    assert_eq!(
        (gain.index, gain.label.as_str(), gain.value.as_str()),
        (0, "GAIN", "5.0")
    );
    assert_eq!(gain.kind, ParameterKind::Continuous { position: 0.5 });

    let knob = InputEvent::Knob {
        knob: KnobIndex::new(0).unwrap(),
        delta: 1,
    };
    let requests = ui.handle(UiInput::View(knob), state, now + Duration::from_secs(1));
    let site = Site::Slot(EngineSlot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    });
    // The knob edit is a stock undo source (`0x59b3ac`): it carries a frame.
    assert_eq!(undo_paths(&requests), [UndoPath::View]);
    match edits(&requests)[..] {
        [
            Action::SetParameter {
                site: at,
                index: 0,
                value,
            },
        ] => {
            assert_eq!(*at, site);
            assert!(
                (value - 0.51).abs() < 1e-4,
                "one encoder step of 101: {value}"
            );
        }
        ref other => panic!("expected one parameter edit, got {other:?}"),
    }

    // BANK UP in the block editor cycles the device as a model change (manual §4). "Light"
    // is the only Drive model here: the turn keeps the model and pushes no undo frame
    // (`0x4f4ddc` returns 0 before `0x5148b0`, review r15 R22).
    let requests = ui.handle(
        UiInput::View(InputEvent::BankEncoder { delta: -1 }),
        state,
        now + Duration::from_millis(1200),
    );
    assert_eq!(
        edits(&requests),
        [&Action::CycleModel {
            slot: EngineSlot {
                row: Row::new(0).unwrap(),
                column: Column::new(2).unwrap(),
            },
            step: -1
        }]
    );

    let scene = SceneIndex::new(1).unwrap();
    let badge = target(&ui, Control::SceneMenu);
    tap(&mut ui, state, badge, now + Duration::from_millis(1500));
    let scene_tile = target(&ui, Control::SelectScene(scene));
    let requests = tap(&mut ui, state, scene_tile, now + Duration::from_secs(2));
    assert_eq!(
        edits(&requests),
        [&Action::SelectScene(EngineScene::new(1).unwrap())]
    );
}

/// Review r11 N17: LOCK reaches the reducer (`Action::SetLocked`), which holds the knob and
/// tells Cortex Control (`0x16ad60`).
#[test]
fn lock_is_sent_to_cortex_control() {
    let (mut device, _log, _media) = boot();
    while !device.state().splash_dismissed {
        device.step();
    }
    let state = device.state();
    let mut ui = Ui::new();
    ui.build(state).unwrap();
    let now = Duration::from_secs(5);
    ui.handle(UiInput::PowerButton { pressed: true }, state, now);
    let lock = target(&ui, Control::Power(tetrad_ui::PowerControl::Lock));
    let requests = tap(&mut ui, state, lock, now + Duration::from_millis(100));
    assert!(
        edits(&requests).contains(&&Action::SetLocked(true)),
        "{requests:?}"
    );
}

/// One source of truth for screens: a device request opens the screen from any screen (no
/// main bar needed), a touch Back closes it, `State::screen` follows the UI each time, and the
/// next request opens it again.
#[test]
fn screen_requests_follow_the_ui() {
    let (mut device, _log, _media) = boot();
    let mut run = |event| {
        device.push(event);
        device.step();
        device.state().screen
    };
    let settings = Screen::Settings(SettingsPage::PowerFunctions);
    assert_eq!(run(Event::UiInput(UiInput::Show(settings))), settings);
    let tempo = || Event::Action(Action::ShowTempo(true));
    assert_eq!(run(tempo()), Screen::Tempo);
    assert_eq!(
        run(Event::UiInput(UiInput::View(InputEvent::Back))),
        settings
    );
    assert_eq!(run(tempo()), Screen::Tempo);
}

/// A device (footswitch) show of the Tempo popup is timed: `QTimer::start(4000)` at
/// `0x382844`, `timeout()` → `hideDialog()`. Each show restarts the timer; a touch open is
/// not timed.
#[test]
fn footswitch_tempo_popup_hides_after_four_seconds() {
    let (device, _log, _media) = boot();
    let state = device.state();
    let mut ui = Ui::new();
    ui.build(state).unwrap();
    let ms = Duration::from_millis;
    let tick = |ui: &mut Ui, now: Duration| {
        let now_ms = now.as_millis() as u64;
        ui.handle(UiInput::View(InputEvent::Tick { now_ms }), state, now);
        ui.view().screen()
    };
    ui.handle(UiInput::Show(Screen::Tempo), state, ms(1_000));
    ui.handle(UiInput::Show(Screen::Tempo), state, ms(3_000));
    assert_eq!(
        tick(&mut ui, ms(6_900)),
        Screen::Tempo,
        "the second show restarted it"
    );
    assert_ne!(tick(&mut ui, ms(7_000)), Screen::Tempo);
    let at = target(&ui, Control::OpenMainMenu);
    tap(&mut ui, state, at, ms(8_000));
    scroll_menu(&mut ui, state, ms(8_500));
    let at = target(&ui, Control::Menu(tetrad_ui::MenuAction::Tempo));
    tap(&mut ui, state, at, ms(9_000));
    assert_eq!(tick(&mut ui, ms(20_000)), Screen::Tempo, "touch opens stay");
}

/// `0x5b3924`, `0x3ef3e8`, `0x4e1558`: engine lifetime, screen writes and stale readings.
#[test]
fn tuner_engine_screen_and_meter_have_separate_lifetimes() {
    use tetrad_app::device::Timer;
    use tetrad_dsp_host::responses::Event as DspEvent;
    use tetrad_dsp_protocol::responses::MeterRecord;
    let (mut device, log, _media) = boot();
    assert!(device.state().tuner_engine);
    assert!(!device.state().tuner_muted);
    assert!(log.borrow().requests.iter().any(|sent| sent.kind() == 0x36));
    let mark = log.borrow().requests.len();
    device.push(Event::UiInput(UiInput::Show(Screen::Tuner)));
    device.step();
    device
        .platform()
        .clock
        .clone()
        .sleep(Duration::from_millis(1));
    assert!(device.state().tuner_engine);
    assert!(device.state().tuner_muted);
    let requests: Vec<_> = log.borrow().requests[mark..]
        .iter()
        .map(Sent::kind)
        .collect();
    let tuner: Vec<_> = requests
        .iter()
        .copied()
        .filter(|message| matches!(message, 0x44 | 0x37 | 0x34))
        .collect();
    assert!(
        tuner
            .windows(3)
            .any(|messages| messages == [0x44, 0x37, 0x34]),
        "{requests:x?}"
    );
    device.push(Event::Timer(Timer::TunerMeterPoll));
    device.step();
    device
        .platform()
        .clock
        .clone()
        .sleep(Duration::from_millis(1));
    assert!(
        log.borrow().requests[mark..]
            .iter()
            .any(|sent| sent.kind() == 0x38)
    );
    device.push(Event::Dsp(
        CoreId::Control,
        DspEvent::Meter(MeterRecord {
            core: 4,
            column: 0,
            index: 0x4020,
            value_bits: 440.0_f32.to_bits(),
        }),
    ));
    device.step();
    assert_eq!(device.state().tuner_hz, 440.0);
    let mut ui = Ui::new();
    ui.build(device.state()).unwrap();
    assert_eq!(ui.view().snapshot().tuner.frequency_hz, Some(440.0));
    device
        .platform()
        .clock
        .clone()
        .sleep(Duration::from_millis(499));
    device.step();
    assert_eq!(device.state().tuner_hz, 440.0);
    device
        .platform()
        .clock
        .clone()
        .sleep(Duration::from_millis(1));
    device.step();
    assert_eq!(device.state().tuner_hz, 0.0);
    // `0x3efc14`: a reference edit on the open tuner sends only `0x37`.
    let mark = log.borrow().requests.len();
    device.push(Event::Action(Action::ChangeSetting {
        id: tetrad_ui::SettingId::TunerReference,
        value: tetrad_ui::SettingValue::Number(442),
    }));
    device.step();
    let sent: Vec<_> = log.borrow().requests[mark..]
        .iter()
        .map(Sent::kind)
        .filter(|message| matches!(message, 0x34 | 0x37 | 0x44))
        .collect();
    assert_eq!(sent, [0x37]);
    device.push(Event::UiInput(UiInput::Hide(Screen::Tuner)));
    device.step();
    assert!(device.state().tuner_engine);
    assert!(!device.state().tuner_muted);
    device.push(Event::UiInput(UiInput::Show(Screen::Capture)));
    device.step();
    assert!(!device.state().tuner_engine);
    let mark = log.borrow().requests.len();
    device.push(Event::UiInput(UiInput::Hide(Screen::Capture)));
    device.step();
    assert!(device.state().tuner_engine);
    // `0x52098c(1)`: only `0x36` to destinations 4 and 0; the port is a boot step.
    let sent: Vec<_> = log.borrow().requests[mark..]
        .iter()
        .map(Sent::kind)
        .filter(|message| matches!(message, 0x34 | 0x36))
        .collect();
    assert_eq!(sent, [0x36, 0x36]);
}

/// The footswitch bank overlay (UP/DOWN hold) is the bank preview on screen, with its armed
/// slot; the touch preview writes that same overlay.
#[test]
fn bank_overlay_is_the_preview_on_screen() {
    let (mut device, _log, _media) = boot();
    let overlay = BankOverlay {
        bank: 3,
        highlighted: Some(2),
    };
    device.push(Event::Action(Action::BankOverlay(Some(overlay))));
    device.step();
    let snapshot = device.bridges().ui.view().snapshot();
    assert!(snapshot.bank_preview_active);
    assert_eq!(
        (snapshot.bank.index, snapshot.bank.highlighted),
        (3, Some(2))
    );

    let state = device.state();
    let mut ui = Ui::new();
    ui.build(state).unwrap();
    let cancel = InputEvent::Activate(Control::CancelBankPreview);
    let requests = ui.handle(UiInput::View(cancel), state, Duration::from_secs(5));
    assert_eq!(edits(&requests), [&Action::BankOverlay(None)]);
}

/// The IR picker on the bridge: a cab `ir selector` shows as a media parameter named by the
/// picker list; the cab editor's IR selector name opens the picker, and a tap on another
/// factory IR sends the live string edit
/// (`Action::SetText` → `Block::set_text`) with the exact stored selector text.
#[test]
fn ir_picker_tap_sends_the_live_text_edit() {
    let specs = STOCK.specs();
    let catalog = SpecsCatalog(&specs);
    let mut preset = Preset::new_default("4.1.0", &catalog).unwrap();
    let cab = Model::block(
        12000,
        [
            ParamValue::Float(0.5),
            ParamValue::String("SYN_Cab One_Mic A".into()),
            ParamValue::Float(0.0),
        ],
    );
    preset
        .insert_block(Slot { row: 0, column: 3 }, cab, &catalog)
        .unwrap();
    let state = loaded_state(specs, preset);
    let entry = |name: &str, value: &str| MediaEntry {
        folder: MediaFolder::FactoryIrs,
        name: name.into(),
        value: value.into(),
    };
    let mut ui = Ui::new();
    ui.set_media(vec![
        entry("SYN Cab One Mic A", "SYN_Cab One_Mic A"),
        entry("SYN Cab Two Mic B", "SYN_Cab Two_Mic B"),
    ]);
    let now = Duration::from_secs(5);
    let block = GridPosition::new(0, 3).unwrap();
    let cell = target(&ui, Control::SelectCell(block));
    tap(&mut ui, &state, cell, now);
    ui.render(&state).unwrap();
    let selector = &ui.view().snapshot().parameters[1];
    assert_eq!(
        (selector.index, selector.value.as_str(), selector.kind),
        (1, "SYN Cab One Mic A", ParameterKind::Media(MediaKind::Ir))
    );

    // The IR name turns IR mode on (`0x82049c`); its `ir.png` button opens the browser.
    let name = Control::Editor(tetrad_ui::EditorControl::IrList(tetrad_ui::CabMic::A));
    let name = target(&ui, name);
    tap(&mut ui, &state, name, now + Duration::from_millis(500));
    let browse = Control::Editor(tetrad_ui::EditorControl::IrBrowse(tetrad_ui::CabMic::A));
    let browse = target(&ui, browse);
    let requests = tap(&mut ui, &state, browse, now + Duration::from_secs(1));
    assert!(
        edits(&requests)
            .iter()
            .all(|action| matches!(action, Action::ScreenShown(_)))
    );
    assert_eq!(ui.view().screen(), Screen::MediaPicker);
    let item = target(&ui, Control::Media(MediaControl::Item(1)));
    let requests = tap(&mut ui, &state, item, now + Duration::from_secs(2));
    let site = Site::Slot(EngineSlot {
        row: Row::new(0).unwrap(),
        column: Column::new(3).unwrap(),
    });
    assert!(edits(&requests).contains(&&Action::SetText {
        site,
        index: 1,
        text: "SYN_Cab Two_Mic B".into(),
    }));
    assert_eq!(ui.view().screen(), Screen::Grid);
}

/// A QC state with `preset` loaded, no DSP and no library.
fn loaded_state(specs: Specs, preset: Preset) -> State {
    let prepared = prepare(preset, &specs, false, None).unwrap();
    let storage = Settings::default();
    let (settings, _) = DeviceSettings::load(&storage, Hardware::QuadCortex).unwrap();
    let mut state = State::new(Product::QuadCortex, HalPlatform::QcOriginal, settings);
    state.specs = Some(specs);
    *state.current.get_mut() = Some(Current {
        path: None,
        preset: prepared.preset,
        migration: prepared.migration,
        deployment: prepared.deployment,
    });
    state
}

/// Runs bridge requests through the action reducer at `now`, without a DSP. Layout deploys
/// commit only through a redeploy: test them on the simulated device (`common::boot`).
fn run(state: &mut State, requests: Vec<Request>, now: Duration) {
    fn apply(state: &mut State, action: Action) {
        for effect in tetrad_app::device::actions::reduce(action, state).unwrap() {
            match effect {
                Effect::DeployLayout(_) | Effect::RestoreLayout { .. } => {
                    panic!("a layout deploy needs the simulated device")
                }
                Effect::Action(action) => apply(state, action),
                _ => {}
            }
        }
    }
    apply(state, Action::Time(now));
    for request in requests {
        if let Request::Action(action) = request {
            apply(state, action);
        }
    }
}

/// The Tempo panel shows SOUND (tempo slot 8) and the read-only "BPM =" beat unit of the
/// time signature (`0xa478f0`, `features::bpm_note`).
#[test]
fn tempo_panel_projects_sound_and_bpm_note() {
    use tetrad_ui::{SettingId, SettingValue};
    let (mut device, _log, _media) = boot();
    // The shared fixture's slot 8 range is 0..=1.
    for (id, value) in [
        (SettingId::MetronomeSound, SettingValue::Choice(1)),
        (SettingId::TimeSignature, SettingValue::Choice(14)),
    ] {
        device.push(Event::Action(Action::ChangeSetting { id, value }));
        device.step();
    }
    let snapshot = device.bridges().ui.view().snapshot();
    assert_eq!(
        snapshot.setting(SettingId::MetronomeSound),
        Some(&SettingValue::Choice(1))
    );
    assert_eq!(
        snapshot.setting(SettingId::BpmNote),
        Some(&SettingValue::Choice(1)),
        "eighths"
    );
}

/// Stock undo (`0x649330`, whole-preset frames) and scene copy (`0x50e274`) end to end on the
/// device: UNDO is hidden without history, a knob turn records one frame, UNDO and REDO
/// restore whole snapshots through the DSP, and Copy Scene moves the source scene's values
/// and label, then plays the destination.
#[test]
fn undo_redo_and_scene_copy_through_the_bridge() {
    let specs = STOCK.specs();
    let catalog = SpecsCatalog(&specs);
    let mut preset = Preset::new_default("4.1.0", &catalog).unwrap();
    // A Send block: a cab would open the cab editor, whose encoder A turns mic A POSITION
    // (`0x11e8e38`), not parameter 0.
    let mut send = Model::block(13000, [ParamValue::Float(0.5), ParamValue::Float(0.0)]);
    send.params[0].set_scene_mode(true, 0).unwrap();
    send.params[0].set_value(1, ParamValue::Float(0.9)).unwrap();
    preset
        .insert_block(Slot { row: 0, column: 3 }, send, &catalog)
        .unwrap();
    preset.scene_labels = ["Verse", "Lead", "", "", "", "", "", ""]
        .map(String::from)
        .to_vec();
    let (mut device, _log, _media) = common::boot(Media::new([("Send", preset)]));
    let site = Site::Slot(EngineSlot {
        row: Row::new(0).unwrap(),
        column: Column::new(3).unwrap(),
    });
    let level = |device: &Device<Sim>, scene: u8| {
        let layout = device.state().current.as_ref().unwrap().deployment.layout();
        layout.instance(site).unwrap().params[0].at(EngineScene::new(scene).unwrap())
    };
    let shown = |device: &Device<Sim>, control| {
        let ui = &device.bridges().ui;
        ui.view().hit_regions().iter().any(|r| r.control == control)
    };
    assert!(!shown(&device, Control::Undo), "hidden without history");

    activate(
        &mut device,
        Control::SelectCell(GridPosition::new(0, 3).unwrap()),
    );
    let knob = InputEvent::Knob {
        knob: KnobIndex::new(0).unwrap(),
        delta: 4,
    };
    device.push(Event::UiInput(UiInput::View(knob)));
    device.step();
    let turned = level(&device, 0);
    assert_ne!(turned, 0.5);

    for (control, expected) in [(Control::Undo, 0.5), (Control::Redo, turned)] {
        activate(&mut device, control);
        assert_eq!(level(&device, 0), expected, "{control:?}");
    }

    // Scene B is the source; Copy Scene from the Grid menu onto F.
    device.push(Event::Action(Action::SelectScene(
        EngineScene::new(1).unwrap(),
    )));
    device.step();
    // The dialog puts the device in footswitch scene copy mode (`0x5ad9d4`); the on-screen
    // destination copies and ends it.
    activate(&mut device, Control::OpenMainMenu);
    activate(&mut device, Control::Menu(tetrad_ui::MenuAction::CopyScene));
    assert_eq!(
        device.state().perform.scene_clipboard,
        Some(SceneClipboard {
            source: Some(EngineScene::new(1).unwrap()),
            swap: false
        })
    );
    activate(
        &mut device,
        Control::SceneTarget(SceneIndex::new(5).unwrap()),
    );
    assert_eq!(device.state().perform.scene_clipboard, None);
    let current = device.state().current.as_ref().unwrap();
    assert_eq!(level(&device, 5), 0.9);
    assert_eq!(current.preset.scene_labels[5], "Lead");
    assert_eq!(
        current.deployment.scene().index(),
        5,
        "the copy target plays"
    );
}

/// Review r11 R1: only the Grid view value slots (`0x59b3ac`) merge in the 500 ms timer; a
/// bypass (`0x5ad868`) pushes its own frame, so a bypass right after a knob turn is undone
/// alone.
#[test]
fn a_bypass_right_after_a_knob_turn_gets_its_own_undo_frame() {
    let (mut device, _log, _media) = boot();
    activate(
        &mut device,
        Control::SelectCell(GridPosition::new(0, 2).unwrap()),
    );
    let knob = InputEvent::Knob {
        knob: KnobIndex::new(0).unwrap(),
        delta: 1,
    };
    device.push(Event::UiInput(UiInput::View(knob)));
    device.step();
    activate(&mut device, Control::Param(ParamControl::Bypass));
    assert_eq!(device.state().history.depths(), (2, 0));
}

/// Expression assignment (shot 068): ASSIGN links a parameter and the bypass to the shown
/// pedal in the stored preset at once (codes 1/2, `0x3242ec`; the bypass gets the dialog
/// default Heel-Toe 600 ms, `0x34d9b8`), and ✕ restores the links from before the first edit.
#[test]
fn expression_links_apply_live_and_revert_to_the_opening_state() {
    let specs = STOCK.specs();
    let catalog = SpecsCatalog(&specs);
    let mut preset = Preset::new_default("4.1.0", &catalog).unwrap();
    let light = Model::block(1, [ParamValue::Float(0.5), ParamValue::Float(0.0)]);
    preset
        .insert_block(Slot { row: 0, column: 2 }, light, &catalog)
        .unwrap();
    let mut state = loaded_state(specs, preset);
    let position = GridPosition::new(0, 2).unwrap();
    let mut ui = Ui::new();
    let _ = ui.render(&state);
    let mut now = Duration::from_secs(5);
    let cell = target(&ui, Control::SelectCell(position));
    tap(&mut ui, &state, cell, now);
    ui.handle(UiInput::Show(Screen::Expression), &state, now);
    let stored = |state: &State| {
        let preset = &state.current.as_ref().unwrap().preset;
        let block = preset.block(Slot { row: 0, column: 2 }).unwrap().unwrap();
        let info = block.expression_bypass_info.first();
        (
            block.params[0].expression,
            block.bypass_expression[0].expression,
            info.map(|info| (info.kind, info.delay_ms)),
        )
    };
    for target_ in [ExpressionTarget::Parameter(0), ExpressionTarget::Bypass] {
        now += Duration::from_secs(1);
        let control = Control::Expression(ExpressionControl::Assign(target_));
        let at = target(&ui, control);
        let requests = tap(&mut ui, &state, at, now);
        run(&mut state, requests, now);
        ui.render(&state).unwrap();
    }
    assert_eq!(stored(&state), (Some(1), 1, Some((2, 600))));
    let link = |target| ExpressionLink {
        pedal: ExpressionPedal::Exp1,
        target,
    };
    assert_eq!(
        ui.view().snapshot().expression,
        [
            link(ExpressionTarget::Parameter(0)),
            link(ExpressionTarget::Bypass)
        ]
    );

    let requests = ui.handle(UiInput::View(InputEvent::Back), &state, now);
    run(&mut state, requests, now);
    assert_eq!(stored(&state).0, Some(0));
    assert_eq!(stored(&state).1, 0);
    assert_eq!(ui.view().screen(), Screen::Grid);
}

/// Virtual Device Presets: a USER preset row from `zenui.db` auditions into the block (its
/// normalized value text becomes a live parameter edit) and shows as the block's preset;
/// ✕ restores the block as it was before the audition. A row outside 0..=1 is left out with
/// a notice, as on the Cortex Control path (r11-a leftover).
#[test]
fn device_preset_audition_edits_the_block_and_revert_restores_it() {
    let media = media();
    let db = rusqlite::Connection::open(media.0.join("zenui.db")).unwrap();
    db.execute_batch(
        "INSERT INTO model_presets VALUES (7, 'Bright', 1, 2, 'false', 'false', 600);
         INSERT INTO model_preset_parameters VALUES (7, 0, '0.8', 0, 1.0, 0.0);
         INSERT INTO model_presets VALUES (8, 'Broken', 1, 2, 'false', 'false', 600);
         INSERT INTO model_preset_parameters VALUES (8, 0, '1.5', 0, 1.0, 0.0);",
    )
    .unwrap();
    let (mut device, log, _media) = common::boot(media);
    let block = GridPosition::new(0, 2).unwrap();
    let site = Site::Slot(EngineSlot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    });
    let gain = |device: &Device<Sim>| {
        let current = device.state().current.as_ref().unwrap();
        let instance = current.deployment.layout().instance(site).unwrap();
        instance.params[0].at(current.deployment.scene())
    };
    // A second tap on the selected block opens the device list on its own group.
    for control in [
        Control::SelectCell(block),
        Control::SelectCell(block),
        Control::Picker(PickerControl::Presets(1)),
        Control::Picker(PickerControl::PresetTab { user: true }),
        Control::Picker(PickerControl::Preset(7)),
    ] {
        activate(&mut device, control);
    }
    assert_eq!(gain(&device), 0.8);
    assert_eq!(device.bridges().ui.view().snapshot().device_preset, Some(7));
    activate(&mut device, Control::Picker(PickerControl::Preset(8)));
    assert_eq!(gain(&device), 0.5, "the bad row is left out");
    assert!(
        (log.borrow().notices.iter()).any(|notice| notice.detail.contains("1 invalid values")),
        "{:?}",
        log.borrow().notices
    );
    // The notice answers the tap: the MessageDialog shows it until OK.
    let notice = |device: &Device<Sim>| device.bridges().ui.view().snapshot().notice.clone();
    let first = notice(&device).unwrap();
    assert!(first.body.contains("1 invalid values"));
    let ok = Control::Dialog(tetrad_ui::DialogButton::Primary);
    activate(&mut device, ok);
    // Review r14 U-2: the same text again is a new arrival and shows again.
    activate(&mut device, Control::Picker(PickerControl::Preset(8)));
    let again = notice(&device).unwrap();
    assert_ne!(again.id, first.id);
    assert_eq!(again.body, first.body);
    activate(&mut device, ok);
    activate(&mut device, Control::Picker(PickerControl::RevertPreset));
    assert_eq!(gain(&device), 0.5);
}

/// Factory Virtual Device Presets come from the read-only `factory.db` (`0x008328a4`);
/// a factory id never equals a user id. Without an audition the open list marks the
/// preset whose values the block holds (I).
#[test]
fn device_presets_list_factory_rows_and_mark_the_matching_one() {
    let media = media();
    let rows = |db: &str, value: &str| {
        rusqlite::Connection::open(media.0.join(db))
            .unwrap()
            .execute_batch(&format!(
                "INSERT INTO model_presets VALUES (7, 'P', 1, 2, 'false', 'false', 600);
                 INSERT INTO model_preset_parameters VALUES (7, 0, '{value}', 0, 1.0, 0.0);"
            ))
            .unwrap();
    };
    rows("factory/factory.db", "0.5");
    rows("zenui.db", "0.8");
    let (mut device, _log, _media) = common::boot(media);
    let block = GridPosition::new(0, 2).unwrap();
    for control in [
        Control::SelectCell(block),
        Control::SelectCell(block),
        Control::Picker(PickerControl::Presets(1)),
    ] {
        activate(&mut device, control);
    }
    let snapshot = device.bridges().ui.view().snapshot();
    let presets = &snapshot.device_presets;
    let factory = presets.iter().find(|preset| preset.factory).unwrap().id;
    assert_ne!(factory, 7);
    assert!(
        presets
            .iter()
            .any(|preset| !preset.factory && preset.id == 7)
    );
    assert_eq!(snapshot.device_preset, Some(factory));
}

/// `factory.db` (4.1.0) holds 2149 presets; the list once stopped at 1024, so the factory
/// presets of later models were missing. The list now takes up to `MAX_DEVICE_PRESETS`.
#[test]
fn device_list_keeps_factory_presets_past_the_first_1024_rows() {
    let media = media();
    rusqlite::Connection::open(media.0.join("factory/factory.db"))
        .unwrap()
        .execute_batch(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1500)
             INSERT INTO model_presets SELECT i, 'F' || i, 12000, 2, 'false', 'false', 600 FROM n;
             INSERT INTO model_presets VALUES (2000, 'Late', 1, 2, 'false', 'false', 600);",
        )
        .unwrap();
    let (mut device, _log, _media) = common::boot(media);
    let block = GridPosition::new(0, 2).unwrap();
    for control in [
        Control::SelectCell(block),
        Control::SelectCell(block),
        Control::Picker(PickerControl::Presets(1)),
    ] {
        activate(&mut device, control);
    }
    let snapshot = device.bridges().ui.view().snapshot();
    assert_eq!(snapshot.device_presets.len(), 1501);
    assert!(
        (snapshot.device_presets.iter())
            .any(|preset| preset.model_id == 1 && preset.name == "Late")
    );
}

/// Stock re-checks the editor's match when a model preset is created, deleted or updated
/// (`0x48c2f0`), Cortex Control writes included. Tetrad reads the presets again after each
/// write, so the header follows a Control CREATE and DELETE while the device list is closed.
#[test]
fn editor_preset_follows_cortex_control_model_preset_writes() {
    use tetrad_control::codecs::{MessageAction, ModelPreset, ModelPresetID, ModelPresetMessage};
    let (mut device, _log, _media) = boot();
    common::connect_control(&mut device);
    activate(
        &mut device,
        Control::SelectCell(GridPosition::new(0, 2).unwrap()),
    );
    let matched = |device: &Device<Sim>| device.bridges().ui.view().snapshot().editor_preset;
    assert_eq!(
        matched(&device),
        Some(tetrad_ui::PresetMatch::CatalogDefault)
    );
    common::host_send(
        &mut device,
        ModelPresetMessage {
            action: Some(MessageAction::Create.into()),
            create_from_row: Some(0),
            create_from_column: Some(2),
            create_from_hash: Some(1),
            presets: vec![ModelPreset {
                name: Some("Mine".into()),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    let Some(tetrad_ui::PresetMatch::Preset(id)) = matched(&device) else {
        panic!("the created preset matches: {:?}", matched(&device));
    };
    common::host_send(
        &mut device,
        ModelPresetMessage {
            action: Some(MessageAction::Delete.into()),
            presets: vec![ModelPreset {
                id: Some(ModelPresetID {
                    value: Some(id.to_string()),
                    hash: Some(1),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    assert_eq!(
        matched(&device),
        Some(tetrad_ui::PresetMatch::CatalogDefault)
    );
}

/// A cross-model audition pastes another model through a DSP redeploy; ✕ pastes the
/// original back and writes its stored block back: EXP links with their heel/toe ranges and
/// the scene bypass record (a paste alone stores a fresh model).
#[test]
fn cross_model_audition_revert_restores_the_stored_block() {
    let specs = STOCK.specs();
    let slot = Slot { row: 0, column: 2 };
    let mut preset = light_preset(&specs, 2);
    let light = preset.block_mut(slot).unwrap();
    light.params[0].expression = Some(1);
    light.params[0].expression_min = Some(0.2);
    light.params[0].expression_max = Some(0.7);
    preset
        .block_bypass_mut(slot)
        .unwrap()
        .set_bypassed(3, true)
        .unwrap();
    let before = (
        preset.block(slot).unwrap().cloned().unwrap(),
        preset.bypass[0].col_bypass[2].clone(),
    );
    let media = Media::new([("Light", preset)]);
    rusqlite::Connection::open(media.0.join("zenui.db"))
        .unwrap()
        .execute_batch(
            "INSERT INTO model_presets VALUES (8, 'Room', 12000, 2, 'false', 'false', 600);
             INSERT INTO model_preset_parameters VALUES (8, 0, '0.3', 0, 1.0, 0.0);",
        )
        .unwrap();
    let (mut device, log, _media) = common::boot(media);
    let stored = |device: &Device<Sim>| {
        let current = device.state().current.as_ref().unwrap();
        current.preset.block(slot).unwrap().cloned().unwrap()
    };
    let site = Site::Slot(EngineSlot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    });
    let live_model = |device: &Device<Sim>| {
        let current = device.state().current.as_ref().unwrap();
        current.deployment.layout().instance(site).unwrap().model.0
    };
    let block = GridPosition::new(0, 2).unwrap();
    for control in [
        Control::SelectCell(block),
        Control::SelectCell(block),
        Control::Picker(PickerControl::Group(tetrad_ui::DeviceGroup::Cab)),
        Control::Picker(PickerControl::Presets(12000)),
        Control::Picker(PickerControl::PresetTab { user: true }),
    ] {
        activate(&mut device, control);
    }
    // A redeploy clears the run list (`0x1e`), then starts the new layout (`0x1c`).
    let redeploys = |device: &mut Device<Sim>, control| {
        let mark = log.borrow().requests.len();
        activate(device, control);
        // The scripted DSP logs what it consumed on the next sleep.
        device
            .platform()
            .clock
            .clone()
            .sleep(Duration::from_millis(1));
        let kinds = log.borrow().kinds(mark);
        let clear = kinds.iter().position(|&kind| kind == 0x1e);
        let start = kinds.iter().rposition(|&kind| kind == 0x1c);
        assert!(clear.is_some() && clear < start, "{kinds:x?}");
    };

    redeploys(&mut device, Control::Picker(PickerControl::Preset(8)));
    assert_eq!(
        stored(&device).hash,
        Some(12000),
        "the audition paste commits"
    );
    assert_eq!(live_model(&device), 12000);

    redeploys(&mut device, Control::Picker(PickerControl::RevertPreset));
    assert_eq!(live_model(&device), 1);
    let preset = &device.state().current.as_ref().unwrap().preset;
    assert_eq!(
        (stored(&device), preset.bypass[0].col_bypass[2].clone()),
        before
    );
}

/// Review r11 N7: `0x527d80` sends the on-load Preset MIDI Out (`0x331c28`) before the
/// compare and the deploy, so external gear switches even when the deploy fails.
#[test]
fn on_load_midi_goes_out_before_the_deploy() {
    let specs = STOCK.specs();
    let mut second = light_preset(&specs, 3);
    second.midi_messages = vec![tetrad_preset::MidiMessage {
        kind: 3,
        channel: 1,
        param1: u32::MAX,
        param2: u32::MAX,
        param3: 9,
        ..Default::default()
    }];
    let (mut device, log, _media) = common::boot(Media::new([
        ("Light", light_preset(&specs, 2)),
        ("Second", second),
    ]));
    device
        .platform()
        .clock
        .hold
        .set(Some(|_, frame| frame[0] == 0x1e));
    let before = log.borrow().midi.len();
    device.push(Event::Action(Action::LoadPreset {
        target: tetrad_app::device::PresetRef {
            setlist: None,
            index: 1,
        },
        reload: true,
    }));
    device.step();
    assert_eq!(log.borrow().midi[before..], [vec![0xc0, 9]]);
}

/// Stock records a preset recent at the load request (`0x5b2f40` → `0x7c3820`), before the
/// push: a preset that fails to deploy (the DSP never reads the run-list clear `0x1e`) is
/// still the newest recent.
#[test]
fn preset_recent_is_recorded_at_the_load_request() {
    let specs = STOCK.specs();
    let (mut device, _log, media) = common::boot(Media::new([
        ("Light", light_preset(&specs, 2)),
        ("Second", light_preset(&specs, 3)),
    ]));
    device
        .platform()
        .clock
        .hold
        .set(Some(|_, frame| frame[0] == 0x1e));
    device.push(Event::Action(Action::LoadPreset {
        target: tetrad_app::device::PresetRef {
            setlist: None,
            index: 1,
        },
        reload: true,
    }));
    device.step();
    let path = |state: &State| state.current.as_ref().unwrap().path.clone();
    assert_eq!(
        path(device.state()).as_deref(),
        Some("/media/p4/Presets/My Presets/Light.pb"),
        "the failed deploy keeps the loaded preset"
    );
    let recents = tetrad_library::stock::StockLibrary::open(
        MediaRoot::new(&media.0),
        tetrad_library::stock::StockLimits::default(),
    )
    .unwrap()
    .items(tetrad_library::stock::ItemList::Recents(
        tetrad_storage::media::Collection::Presets,
    ))
    .unwrap();
    assert_eq!(recents[0].item, "/media/p4/Presets/My Presets/Second.pb");
}

/// Gig View is a tuner meter consumer only while LIVE TUNER is on (`liveTunerEnabledSlot`
/// `0x65debc`): the `0x38` poll runs on Gig View with the setting on, not with it off.
#[test]
fn gig_view_polls_the_tuner_meter_only_with_live_tuner() {
    use tetrad_app::device::Timer;
    let (mut device, log, _media) = boot();
    let polled = |device: &mut Device<Sim>| {
        let mark = log.borrow().requests.len();
        device.push(Event::Timer(Timer::TunerMeterPoll));
        device.step();
        device
            .platform()
            .clock
            .clone()
            .sleep(Duration::from_millis(1));
        log.borrow().requests[mark..]
            .iter()
            .any(|sent| sent.kind() == 0x38)
    };
    device.push(Event::UiInput(UiInput::Show(Screen::Gig)));
    device.step();
    assert!(!polled(&mut device), "LIVE TUNER is off by default");
    device.push(Event::Action(Action::ChangeSetting {
        id: tetrad_ui::SettingId::LiveTuner,
        value: tetrad_ui::SettingValue::Bool(true),
    }));
    device.step();
    assert!(polled(&mut device));
    device.push(Event::UiInput(UiInput::Hide(Screen::Gig)));
    device.step();
    assert!(!polled(&mut device));
}

/// Gig View EDIT STOMP opens the block editor of the switch's first block on the Grid.
#[test]
fn gig_stomp_edit_opens_the_block_editor() {
    let specs = STOCK.specs();
    let catalog = SpecsCatalog(&specs);
    let mut preset = Preset::new_default("4.1.0", &catalog).unwrap();
    let light = Model::block(1, [ParamValue::Float(0.5), ParamValue::Float(0.0)]);
    preset
        .insert_block(Slot { row: 0, column: 2 }, light, &catalog)
        .unwrap();
    preset
        .assign_stomp(Slot { row: 0, column: 2 }, 0, StompRole::Primary)
        .unwrap();
    let mut state = loaded_state(specs, preset);
    state.performance.footswitch.mode = tetrad_core::performance::FootswitchMode::Stomp;
    let block = GridPosition::new(0, 2).unwrap();
    let mut ui = Ui::new();
    let _ = ui.render(&state);
    let now = Duration::from_secs(5);
    ui.handle(UiInput::Show(Screen::Gig), &state, now);
    ui.render(&state).unwrap();
    assert_eq!(ui.view().snapshot().stomps[0].position, Some(block));
    let edit = target(&ui, Control::EditBlock(block));
    tap(&mut ui, &state, edit, now);
    ui.render(&state).unwrap();
    assert_eq!(ui.view().screen(), Screen::Grid);
    assert_eq!(ui.view().snapshot().selected_block, Some(block));
}

/// "Unsaved" is the PresetDirty flag (`0x5ad250`): it reads `State::clean`, so an expression
/// link edit marks the preset and the baseline a save sets clears the mark.
#[test]
fn unsaved_follows_state_clean_through_expression_edit_and_save() {
    use tetrad_app::device::preset::Snapshot;
    let specs = STOCK.specs();
    let catalog = SpecsCatalog(&specs);
    let mut preset = Preset::new_default("4.1.0", &catalog).unwrap();
    let light = Model::block(1, [ParamValue::Float(0.5), ParamValue::Float(0.0)]);
    let slot = Slot { row: 0, column: 2 };
    preset.insert_block(slot, light, &catalog).unwrap();
    let mut state = loaded_state(specs, preset);
    let baseline = |state: &mut State| {
        *state.clean.get_mut() = Some(Snapshot::of(state.current.as_ref().unwrap()));
    };
    baseline(&mut state);
    let mut ui = Ui::new();
    // The first pass also refreshes the library, which this state has not.
    let _ = ui.render(&state);
    assert!(!ui.view().snapshot().dirty);

    let link = Action::LinkExpression {
        slot: EngineSlot {
            row: Row::new(0).unwrap(),
            column: Column::new(2).unwrap(),
        },
        parameter: Some(0),
        pedal: tetrad_core::performance::expression::Pedal::Exp1,
        linked: true,
    };
    tetrad_app::device::actions::reduce(link, &mut state).unwrap();
    ui.render(&state).unwrap();
    assert!(ui.view().snapshot().dirty, "an expression edit is unsaved");

    baseline(&mut state);
    ui.render(&state).unwrap();
    assert!(!ui.view().snapshot().dirty, "a save clears the mark");
}

/// Health r34 #9: the PresetDirty mark ("Unsaved", `0x5ad250`) on the device changes exactly at
/// these steps: an edit sets it, an UNDO back to the loaded preset clears it, a save clears it
/// and a load clears it. Pinned through the device loop before the dirty check was memoized.
#[test]
fn unsaved_follows_edit_undo_save_and_load_on_the_device() {
    use tetrad_app::device::PresetRef;
    let mut light = light_preset(&STOCK.specs(), 0);
    light.name = Some("Light".into());
    let (mut device, _log, _media) = common::boot(Media::new([("Light", light)]));
    device.platform().wall_clock.set(Some(
        std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
    ));
    let run = |device: &mut Device<Sim>, event: Event| -> bool {
        device.push(event);
        (0..5).for_each(|_| _ = device.step());
        device.bridges().ui.view().snapshot().dirty
    };
    let edit = |label: &str| {
        Event::Action(Action::SceneLabel {
            scene: EngineScene::new(1).unwrap(),
            label: label.into(),
        })
    };
    let undo = Event::Action(Action::RecordUndo(UndoPath::Direct));
    assert!(!run(
        &mut device,
        Event::Timer(tetrad_app::device::Timer::UiTick)
    ));
    assert!(!run(&mut device, undo.clone()));
    assert!(run(&mut device, edit("Verse")), "an edit");
    assert!(
        !run(&mut device, Event::Action(Action::Undo)),
        "UNDO to the loaded preset"
    );
    assert!(run(&mut device, edit("Chorus")));
    let save = UiInput::View(InputEvent::Activate(Control::SavePreset));
    assert!(!run(&mut device, Event::UiInput(save)), "a save");
    assert!(run(&mut device, edit("Bridge")));
    let load = Action::LoadPreset {
        target: PresetRef {
            setlist: None,
            index: 0,
        },
        reload: true,
    };
    assert!(!run(&mut device, Event::Action(load)), "a load");
}

/// The Grid meter set (`0x596108`): only the input words the rows read, then `0x4021`. A
/// reading at or above 0 dB lights the row port for 1 s (`0x595590`, `0x5937cc`, `0x5a41d4`);
/// `0x4021` also folds each lane by its output port into the HP feed meters (`0x493464`), and
/// `0x4022` values 4, 5 are the SEND limiters (`0x4c9af8`).
#[test]
fn grid_meters_light_row_ports_and_fold_lanes_by_output() {
    use tetrad_app::device::meters;
    use tetrad_dsp_protocol::requests::Request as DspRequest;
    use tetrad_dsp_protocol::responses::BulkMeter;
    use tetrad_dsp_protocol::tuner_looper::MeterRecord;

    let specs = STOCK.specs();
    let mut preset = Preset::new_default("4.1.0", &SpecsCatalog(&specs)).unwrap();
    // Row 1 stays In 1 → Multi Out; row 3 reads RET 1/2 and feeds SEND 1.
    (preset.chains[2].in_portid, preset.chains[2].out_portid) = (Some(6), Some(8));
    let mut state = loaded_state(specs, preset);
    let lanes = &state.current.as_ref().unwrap().deployment.layout().lanes;
    let words: Vec<u32> = meters::grid_requests(lanes)
        .map(|effect| match effect {
            Effect::Dsp(tetrad_app::device::DspCommand::Request(DspRequest::MeterRequest {
                word,
                ..
            })) => word,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(words, [0x4000, 0x4002, 0x4003, 0x4021]);

    let at = Duration::from_secs(1);
    let record = MeterRecord {
        core: 0,
        column: 0,
        index: 0x4003,
        value_bits: 0.5f32.to_bits(),
    };
    assert!(meters::on_meter(&mut state.io_meters, &record));
    meters::grid_input_clips(&mut state, &record, at);
    let bulk = |second, values: [f32; 8]| BulkMeter {
        first: 1,
        second,
        count: 8,
        value_bits: std::array::from_fn(|i| values.get(i).copied().unwrap_or(0.0).to_bits()),
    };
    let lanes = bulk(0x4021, [-3.0, 0.0, -50.0, -50.0, -6.0, -9.0, -50.0, -50.0]);
    assert!(meters::on_lanes(&mut state, &lanes, at));
    assert!(meters::on_bulk(
        &mut state.io_meters,
        &bulk(0x4022, [0.0, 0.0, 0.0, 0.0, 2.0, 5.0, 0.0, 0.0])
    ));

    let mut ui = Ui::new();
    // No library: the first frame reports it once.
    assert!(ui.render(&state).is_err());
    let mut shown = |state: &mut State, now| {
        state.now = now;
        ui.render(state).unwrap();
        ui.view().snapshot().clone()
    };
    let lit = shown(&mut state, at + Duration::from_millis(900));
    let clips = lit.grid_clips;
    assert_eq!(clips.inputs, [false, false, true, false]);
    assert_eq!(clips.outputs, [true, false, false, false]);
    let floor = tetrad_ui::IoMeters::FLOOR;
    assert_eq!(
        lit.io_meters.hp_feed,
        [-3.0, 0.0, floor, floor, -6.0, floor]
    );
    assert_eq!(lit.io_meters.send_limiters, [-2.0, -5.0]);
    let out = shown(&mut state, at + Duration::from_millis(1000)).grid_clips;
    assert_eq!(out, tetrad_ui::GridClips::default());
}

/// The Grid Input Gate meter (`InputPortItem` `0x5a3868`, `0x5a163c`): a row with an input
/// port whose gate is on shows one, all reduction before the first reply. A row meter reply
/// (`0x4df394`) of −20 dB gain reduction opens half the 24 px bar. A bypassed gate, a row
/// without an input and a row an upper row feeds (`0x6baf54`) show none.
#[test]
fn grid_input_gate_meters_follow_the_row_meter_and_the_gate() {
    use tetrad_app::device::meters;
    use tetrad_dsp_protocol::tuner_looper::MeterRecord;
    use tetrad_ui::GateMeter;

    let specs = STOCK.specs();
    let mut preset = Preset::new_default("4.1.0", &SpecsCatalog(&specs)).unwrap();
    // Row 2 reads In 2 with its gate bypassed; row 3 reads RET 1/2.
    preset.chains[1].in_portid = Some(2);
    preset.chains[2].in_portid = Some(6);
    // The stored Input Gate parameters are in XML order: BYPASS is the second.
    for value in &mut preset.chains[1].input_control[0].params[1].values {
        value.value = Some(ParamValue::Float(1.0));
    }
    let mut state = loaded_state(specs.clone(), preset.clone());
    let full = Some(GateMeter::from_level(0.0));
    assert_eq!(full.map(GateMeter::open), Some(0));
    assert_eq!(meters::gate_meters(&state), [full, None, full, None]);
    let record = MeterRecord {
        core: 0,
        column: 1,
        index: 0x401c,
        value_bits: (-20.0_f32).to_bits(),
    };
    assert!(meters::on_row_meter(&mut state.row_meters, &record));
    let open = meters::gate_meters(&state).map(|meter| meter.map(GateMeter::open));
    assert_eq!(open, [Some(12), None, Some(0), None]);
    // Row 1 feeds row 3 (NEXT_ROW_3): row 3 shows "Prev. Row" and no meter.
    preset.chains[0].out_portid = Some(16);
    let state = loaded_state(specs, preset);
    assert_eq!(meters::gate_meters(&state), [full, None, None, None]);
}

/// Census #5 and #6 on the simulated device: in Stomp mode the editor's footswitch button
/// and a press of C store the stomp assignment; holding a parameter knob turns its scene
/// assignment on (a redeploy). The editor projects both back.
#[test]
fn editor_stomp_and_scene_assignments_reach_the_preset() {
    use tetrad_settings::controls::FootswitchMode as StoredMode;
    let (mut device, _log, _media) = boot();
    device.push(Event::Action(Action::SetMode(StoredMode::Stomp)));
    device.step();
    let block = GridPosition::new(0, 2).unwrap();
    activate(&mut device, Control::SelectCell(block));
    activate(&mut device, Control::Param(ParamControl::Stomp));
    device.push(Event::UiInput(UiInput::View(InputEvent::Footswitch {
        switch: tetrad_ui::Footswitch::C,
        pressed: true,
    })));
    device.step();
    let preset = &device.state().current.as_ref().unwrap().preset;
    assert!(
        preset
            .stomp_mode_assignments
            .iter()
            .any(|stomp| { (stomp.row, stomp.column, stomp.stomp_index) == (0, 2, 2) })
    );
    assert_eq!(
        device.bridges().ui.view().snapshot().selected_stomp,
        Some(2)
    );

    let knob = Control::Param(ParamControl::Knob(KnobIndex::new(0).unwrap()));
    let parameter = device.bridges().ui.view().snapshot().parameters[0].index;
    let press = target(&device.bridges().ui, knob);
    let mut clock = device.platform().clock.clone();
    device.push(Event::UiInput(UiInput::Touch(press)));
    device.step();
    // The stock hold is 750 ms (`LONG_PRESS_MS`); the test held 600 ms over the old 500 ms.
    clock.sleep(Duration::from_millis(750));
    device.push(Event::UiInput(UiInput::View(InputEvent::Tick {
        now_ms: 0,
    })));
    device.step();
    device.push(Event::UiInput(UiInput::Touch(TouchSample {
        pressed: false,
        ..press
    })));
    device.step();
    let site = Site::Slot(EngineSlot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    });
    let current = device.state().current.as_ref().unwrap();
    let block = current.deployment.layout().instance(site).unwrap();
    assert_eq!(block.scene_mode.get(usize::from(parameter)), Some(&true));
    assert!(device.bridges().ui.view().snapshot().parameters[0].scene);
}

/// Census #7 (manual "INPUT & OUTPUT BLOCKS"): holding the row 1 input tile opens INPUT GATE
/// CONTROL with the lane control's parameters, and a knob turn edits that lane control.
#[test]
fn input_gate_editor_edits_the_lane_control() {
    let specs = STOCK.specs();
    let catalog = SpecsCatalog(&specs);
    let preset = Preset::new_default("4.1.0", &catalog).unwrap();
    let mut state = loaded_state(specs, preset);
    let mut ui = Ui::new();
    let _ = ui.render(&state);
    let row = tetrad_ui::Row::new(0).unwrap();
    let now = Duration::from_secs(5);
    let press = target(&ui, Control::Port { row, output: false });
    ui.handle(UiInput::Touch(press), &state, now);
    // The stock hold is 750 ms (`LONG_PRESS_MS`); the test held 600 ms over the old 500 ms.
    let held = ui.handle(
        UiInput::View(InputEvent::Tick { now_ms: 0 }),
        &state,
        now + Duration::from_millis(750),
    );
    run(&mut state, held, now);
    let _ = ui.render(&state);
    let snapshot = ui.view().snapshot().clone();
    assert_eq!(
        snapshot.selected_site,
        Some(tetrad_ui::EditorSite::InputGate(row))
    );
    let parameter = snapshot.parameters.first().expect("gate parameters").index;
    let site = Site::InputControl(Row::new(0).unwrap());
    let value = |state: &State| {
        let layout = state.current.as_ref().unwrap().deployment.layout();
        layout.instance(site).unwrap().params[usize::from(parameter)]
            .at(EngineScene::new(0).unwrap())
    };
    let before = value(&state);
    let turn = InputEvent::Knob {
        knob: KnobIndex::new(0).unwrap(),
        delta: 5,
    };
    let requests = ui.handle(UiInput::View(turn), &state, now + Duration::from_secs(1));
    run(&mut state, requests, now + Duration::from_secs(1));
    assert_ne!(value(&state), before);
}

/// Census #12: "Save Current Parameters As…" stores a new user preset in `zenui.db`; a second
/// save of the same values stores nothing and shows Preset Conflict (`0x397ff8(model, 0)`).
/// "Set Parameters as Defaults" stores the `default_param` rows, which a new block of the
/// model then takes instead of the catalog values (a paste of the default block).
#[test]
fn device_preset_save_as_and_defaults_reach_zenui_db() {
    let (mut device, _log, media) = boot();
    let block = GridPosition::new(0, 2).unwrap();
    activate(&mut device, Control::SelectCell(block));
    let knob = InputEvent::Knob {
        knob: KnobIndex::new(0).unwrap(),
        delta: 3,
    };
    device.push(Event::UiInput(UiInput::View(knob)));
    device.step();
    for name in ["Warm", "Again"] {
        activate(&mut device, Control::Param(ParamControl::Menu));
        activate(
            &mut device,
            Control::Menu(tetrad_ui::MenuAction::SaveDevicePreset),
        );
        let text = InputEvent::TextChanged(name.into());
        device.push(Event::UiInput(UiInput::View(text)));
        device.step();
        activate(&mut device, Control::Key(tetrad_ui::Key::Done));
    }
    let notice = device
        .bridges()
        .ui
        .view()
        .snapshot()
        .notice
        .clone()
        .unwrap();
    assert_eq!(
        (notice.title.as_str(), notice.button),
        ("Preset Conflict", "GOT IT")
    );
    assert!(
        notice
            .body
            .starts_with("The Virtual Device Preset could not be saved")
    );
    activate(
        &mut device,
        Control::Dialog(tetrad_ui::DialogButton::Primary),
    );
    activate(&mut device, Control::Param(ParamControl::Menu));
    activate(
        &mut device,
        Control::Menu(tetrad_ui::MenuAction::SetDefaults),
    );
    let db = rusqlite::Connection::open(media.0.join("zenui.db")).unwrap();
    let count = |sql: &str| db.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap();
    assert_eq!(
        count("SELECT COUNT(*) FROM model_presets WHERE name = 'Warm' AND hash = 1"),
        1
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM model_presets WHERE name = 'Again'"),
        0
    );
    assert!(count("SELECT COUNT(*) FROM default_param WHERE model_id = '1'") > 0);

    let site = |column| {
        Site::Slot(EngineSlot {
            row: Row::new(0).unwrap(),
            column: Column::new(column).unwrap(),
        })
    };
    let gain = |device: &Device<Sim>, column| {
        let layout = device.state().current.as_ref().unwrap().deployment.layout();
        layout.instance(site(column)).unwrap().params[0].at(EngineScene::new(0).unwrap())
    };
    let edited = gain(&device, 2);
    activate(&mut device, Control::Param(ParamControl::Close));
    activate(
        &mut device,
        Control::SelectCell(GridPosition::new(0, 3).unwrap()),
    );
    let model = Control::Picker(PickerControl::Model(1));
    let groups: Vec<Control> = device
        .bridges()
        .ui
        .view()
        .hit_regions()
        .into_iter()
        .map(|region| region.control)
        .filter(|control| matches!(control, Control::Picker(PickerControl::Group(_))))
        .collect();
    for group in groups {
        let shown = |device: &Device<Sim>| {
            let regions = device.bridges().ui.view().hit_regions();
            regions.iter().any(|region| region.control == model)
        };
        if shown(&device) {
            break;
        }
        activate(&mut device, group);
    }
    activate(&mut device, model);
    assert_eq!(
        gain(&device, 3),
        edited,
        "the new block takes the user defaults"
    );
}

/// M528: a value typed on the numeric keyboard (`0x64b518`) becomes the parameter's display
/// value through the same write as a knob drag (`0x4c9ce8`).
#[test]
fn numeric_keyboard_sets_the_display_value() {
    let (mut device, _log, _media) = boot();
    activate(
        &mut device,
        Control::SelectCell(GridPosition::new(0, 2).unwrap()),
    );
    let snapshot = device.bridges().ui.view().snapshot().clone();
    let (slot, parameter, range) = snapshot
        .parameters
        .iter()
        .take(tetrad_ui::KNOB_COUNT)
        .enumerate()
        .find_map(|(slot, parameter)| {
            let range = parameter.entry?;
            (range.min >= 0.0 && range.max - range.min >= 2.0).then_some((
                slot,
                parameter.index,
                range,
            ))
        })
        .expect("a continuous parameter with a range");
    let typed = ((range.min + range.max) / 2.0).floor();
    activate(
        &mut device,
        Control::Param(ParamControl::Value(KnobIndex::new(slot as u8).unwrap())),
    );
    for digit in format!("{typed}").bytes().map(|byte| byte - b'0') {
        activate(&mut device, Control::Numeric(NumericKey::Digit(digit)));
    }
    activate(&mut device, Control::Numeric(NumericKey::Done));
    let after = device.bridges().ui.view().snapshot();
    let entry = (after.parameters.iter())
        .find(|view| view.index == parameter)
        .and_then(|view| view.entry)
        .unwrap();
    assert!(
        (entry.value - typed).abs() <= (range.max - range.min) * 1e-3,
        "{} after typing {typed}",
        entry.value
    );
}

/// M433 and M1190: a pin is a `pinned_models` row that the picker projects; the device name
/// is `General/customName` on the Device Information page.
#[test]
fn pins_and_device_name_reach_storage_and_the_view() {
    let (mut device, _log, _media) = boot();
    device.push(Event::Action(Action::PinModel {
        model: 1001,
        pinned: true,
    }));
    device.push(Event::Action(Action::SetDeviceName(Some(
        "Stage Left".into(),
    ))));
    device.step();
    let state = device.state();
    let database = state.library.as_ref().unwrap().database.as_ref().unwrap();
    assert_eq!(database.view().pinned_models().unwrap(), ["1001"]);
    assert_eq!(
        state.settings.settings().general.custom_name.as_deref(),
        Some("Stage Left")
    );
    // Row 1 has an input: an empty slot of a row without one opens nothing (shot sn-1).
    activate(
        &mut device,
        Control::SelectCell(GridPosition::new(0, 7).unwrap()),
    );
    let snapshot = device.bridges().ui.view().snapshot();
    assert_eq!(snapshot.pinned_models, [1001]);
    assert_eq!(snapshot.about.device_name.as_deref(), Some("Stage Left"));
}

/// M694-697: the Grid CPU Monitor shows the DSP Diagnostics load (`0x364ba0`), "--" before the
/// first reply.
#[test]
fn cpu_monitor_shows_the_dsp_diagnostics_load() {
    use tetrad_dsp_protocol::DspCore;
    use tetrad_dsp_protocol::responses::DiagnosticsBlock;
    let mut state = loaded_state(STOCK.specs(), light_preset(&STOCK.specs(), 2));
    let mut ui = Ui::new();
    // No library in this state: the build reports it, and the snapshot is still projected.
    let _ = ui.build(&state);
    assert_eq!(ui.view().snapshot().cpu_percent, None);
    let mut words = [0_u32; 16];
    words[5] = 82.0_f32.to_bits();
    state
        .cpu
        .record(DspCore::Core0, &DiagnosticsBlock { words });
    let _ = ui.build(&state);
    assert_eq!(ui.view().snapshot().cpu_percent, Some(21));
}

/// Folder ⋮ menu (M817-821) end to end: DUPLICATE adds "cabs 2" beside "cabs" and SET AS
/// SAVING DESTINATION stores the folder key; the view marks the new destination.
#[test]
fn folder_menu_duplicates_and_sets_the_saving_destination() {
    use tetrad_storage::stock::ProductType;
    use tetrad_ui::{ItemAction, LibraryControl};
    let (mut device, _log, _media) = boot();
    let folders = |device: &Device<Sim>| {
        let library = device.state().library.as_ref().unwrap();
        library.database.as_ref().unwrap().view().folders().unwrap()
    };
    let library = device.state().library.as_ref().unwrap();
    let view = library.database.as_ref().unwrap().view();
    let root = view.root_folder(ProductType::Ir).unwrap().unwrap();
    activate(&mut device, Control::Navigate(Screen::Presets));
    activate(&mut device, Control::Library(LibraryControl::Category));
    activate(&mut device, Control::Choose(3));
    activate(&mut device, Control::Library(LibraryControl::NewFolder));
    for key in "cabs".chars() {
        activate(&mut device, Control::Key(tetrad_ui::Key::Char(key)));
    }
    activate(&mut device, Control::Key(tetrad_ui::Key::Done));
    let cabs = folders(&device)
        .into_iter()
        .find(|folder| folder.name == "cabs")
        .unwrap()
        .id;
    for action in [ItemAction::Duplicate, ItemAction::SavingDestination] {
        activate(
            &mut device,
            Control::Library(LibraryControl::DatabaseFolderMenu(cabs)),
        );
        activate(&mut device, Control::Library(LibraryControl::Item(action)));
    }
    assert!(
        folders(&device)
            .iter()
            .any(|folder| folder.name == "cabs 2" && folder.parent == root)
    );
    let settings = device.state().settings.settings();
    assert_eq!(
        settings.library.download_destination_ir,
        Some(format!("{cabs}_q"))
    );
    device.step();
    let snapshot = device.bridges().ui.view().snapshot();
    assert!(snapshot.directory.saving_destinations.contains(&cabs));
}

/// Review r15 R2, R6: the device scene copy mode lives only with its dialog. A preset load
/// closes the dialog in the UI, and the bridge then ends the mode, so the next scene
/// footswitch does not copy inside the new preset with the old source.
#[test]
fn a_preset_load_ends_the_scene_copy_mode() {
    let specs = STOCK.specs();
    let (mut device, _log, _media) = common::boot(Media::new([
        ("Light", light_preset(&specs, 2)),
        ("Second", light_preset(&specs, 3)),
    ]));
    activate(&mut device, Control::OpenMainMenu);
    activate(&mut device, Control::Menu(tetrad_ui::MenuAction::CopyScene));
    assert!(device.state().perform.scene_clipboard.is_some());
    device.push(Event::Action(Action::LoadPreset {
        target: tetrad_app::device::PresetRef {
            setlist: None,
            index: 1,
        },
        reload: true,
    }));
    device.step();
    // Any UI pass syncs the view; hiding a closed Tuner changes nothing else.
    device.push(Event::UiInput(UiInput::Hide(Screen::Tuner)));
    device.step();
    assert!(!device.bridges().ui.view().scene_target_open());
    assert_eq!(device.state().perform.scene_clipboard, None);
}

/// A new preset whose catalog has the split path head 10003 with DSP stats (the shared
/// fixture lacks them, so a split cannot compile there).
fn split_state() -> State {
    // The split path head 10003 needs DSP stats, which the shared fixture lacks.
    const MODELS: &str = "<Models>\
<Category id='10' name='Splitter' hidden='true'><Model id='10004' name='Splitter'>\
<Parameter name='TYPE' type='switch' min='0' max='2' defaultValue='0'/></Model>\
<Model id='10003' name='LR Crossover' blob='lr' hidden='true'>\
<Parameter name='FREQ' type='float' min='20' max='20000' defaultValue='20'/></Model></Category>\
<Category id='11' name='Mixer' hidden='true'><Model id='11000' name='Mixer'>\
<Parameter name='LEVEL A' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/>\
</Model></Category>\
<Category id='23' name='Lane Output' hidden='true'><Model id='23000' name='Lane Output'>\
<Parameter name='VOLUME' type='float' min='0' max='1' defaultValue='0.76923'/></Model></Category>\
<Category id='25' name='Tempo' hidden='true'><Model id='25000' name='Tempo'>\
<Parameter name='TEMPO' type='float' min='40' max='240' defaultValue='120'/></Model></Category>\
<Category id='28' name='Input Gate' hidden='true'><Model id='28000' name='Input Gate'>\
<Parameter name='THRESHOLD' type='float' min='-96' max='0' defaultValue='-60'/></Model></Category>\
</Models>";
    const REPORT: &str = "<ModuleReport>\
<Kernel core='0' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='1' cpu='2' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='2' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='3' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Module id='10003' name='LR Crossover' blob_name='lr' category='10' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='1'/></Module>\
</ModuleReport>";
    let catalog = tetrad_catalog::Catalog::parse(MODELS, REPORT).unwrap();
    let specs = Specs::resolve(&catalog).unwrap();
    let preset = Preset::new_default("4.1.0", &SpecsCatalog(&specs)).unwrap();
    loaded_state(specs, preset)
}

/// Review r14 U-3 (r15 R18): a layout edit that removes the split point closes the Splitter
/// editor (the pair keeps its splitter instance without a split).
#[test]
fn removing_the_split_closes_the_splitter_editor() {
    use tetrad_app::device::actions::LayoutEdit;
    use tetrad_engine::Pair;
    use tetrad_ui::{EditorSite, RowPair};
    let mut state = split_state();
    let split = |state: &mut State, column| {
        let edit = LayoutEdit::Split {
            pair: Pair::Upper,
            column,
        };
        let (current, specs) = (
            state.current.get_mut().as_mut().unwrap(),
            state.specs.as_ref().unwrap(),
        );
        tetrad_app::device::preset::prepare_layout_edit(current, specs, edit)
            .unwrap()
            .commit(current);
    };
    split(&mut state, Some(Column::new(4).unwrap()));
    let mut ui = Ui::new();
    let _ = ui.build(&state);
    let site = EditorSite::Splitter(RowPair::Upper);
    let open = UiInput::View(InputEvent::Activate(Control::Site(site)));
    ui.handle(open, &state, Duration::from_secs(5));
    assert_eq!(ui.view().snapshot().selected_site, Some(site));
    split(&mut state, None);
    let _ = ui.build(&state);
    assert_eq!(ui.view().snapshot().selected_site, None);
}

/// The shared fixture with Global EQ 4004 (stock parameter order) and a synthetic IOSettings
/// 31000: IN 1 LEVEL (0, replaces 100), IN 1 GROUND LIFT (4, replaces 108), fillers, the two
/// LINK flags at 46/47 and the EXP calibration ints at 48..=51 (ModelRepo 4.1.0 layout).
fn settings_preset_state(test: &str) -> (State, std::path::PathBuf) {
    use tetrad_storage::stock::{DbLimits, StockDb};
    let band = |n: u8| {
        format!(
            "<Parameter name='{n} GAIN' type='float' min='MIN_EQ_DB' max='MAX_EQ_DB' \
             defaultValue='0' steps='241'/>\
             <Parameter name='{n} FREQ' type='float' min='20' max='20000' defaultValue='1000' \
             skew='0.3'/>\
             <Parameter name='{n} Q' type='float' min='0.1' max='10' defaultValue='0.707' \
             steps='100'/>\
             <Parameter name='{n} TYPE' type='float' min='0' max='1' defaultValue='0' steps='5'/>\
             <Parameter name='{n} BYPASS' type='switch' min='0' max='1' defaultValue='1' \
             steps='2'/>"
        )
    };
    let io = (0..46)
        .map(|index| match index {
            0 => "<Parameter name='IN 1 LEVEL' max='1.0' min='0.0' replaces='100' type='float'/>"
                .to_owned(),
            4 => "<Parameter name='IN 1 GROUND LIFT' max='1.0' min='0.0' replaces='108' \
                  type='float'/>"
                .to_owned(),
            _ => format!("<Parameter name='P{index}' max='1.0' min='0.0' type='float'/>"),
        })
        .collect::<String>();
    let extra = format!(
        "<Category id='40' name='Output EQ' hidden='true'><Model id='4004' name='Output Equalizer'>\
         {}<Parameter name='OUTPUT' type='float' min='-20' max='20' defaultValue='0' steps='241'/>\
         <Parameter name='ASSIGN_EQ_0' type='float' min='0' max='1' defaultValue='0' steps='2'/>\
         <Parameter name='ASSIGN_EQ_1' type='float' min='0' max='1' defaultValue='0' steps='2'/>\
         </Model></Category>\
         <Category id='41' name='IO' hidden='true'><Model id='31000' name='IOSettings' hidden='true'>\
         {io}<Parameter name='OUTPUT 1/2 LINK' max='1.0' min='0.0' type='float'/>\
         <Parameter name='OUTPUT 3/4 LINK' max='1.0' min='0.0' type='float'/>\
         <Parameter name='EXP 1 CALIBRATION MIN' max='255' min='0' type='int'/>\
         <Parameter name='EXP 1 CALIBRATION MAX' max='255' min='0' type='int'/>\
         <Parameter name='EXP 2 CALIBRATION MIN' max='255' min='0' type='int'/>\
         <Parameter name='EXP 2 CALIBRATION MAX' max='255' min='0' type='int'/>\
         </Model></Category></Models>",
        (1..=5).map(band).collect::<String>()
    );
    let models = common::MODELS.replace("</Models>", &extra);
    let specs =
        Specs::resolve(&tetrad_catalog::Catalog::parse(&models, common::REPORT).unwrap()).unwrap();
    let preset = Preset::new_default("4.1.0", &SpecsCatalog(&specs)).unwrap();
    let mut state = loaded_state(specs, preset);
    let root = std::env::temp_dir().join(format!("tetrad-app-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let db = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    state.library = Some(tetrad_app::device::Library {
        presets: tetrad_library::stock::StockLibrary::open(
            MediaRoot::new(&root),
            tetrad_library::stock::StockLimits::default(),
        )
        .unwrap(),
        database: Some(db),
    });
    (state, root)
}

/// Activates a visible control on the bridge and runs its requests through the reducer.
fn press(ui: &mut Ui, state: &mut State, control: Control) {
    assert!(
        ui.view()
            .hit_regions()
            .iter()
            .any(|region| region.enabled && region.control == control),
        "{control:?} is not on screen"
    );
    let now = state.now + Duration::from_secs(1);
    let requests = ui.handle(UiInput::View(InputEvent::Activate(control)), state, now);
    run(state, requests, now);
    let _ = ui.render(state);
}

fn name_preset(ui: &mut Ui, state: &mut State, name: &str) {
    let text = UiInput::View(InputEvent::TextChanged(name.into()));
    ui.handle(text, state, state.now);
    press(ui, state, Control::Key(tetrad_ui::Key::Done));
}

/// Global EQ presets (M553, M594; `0x227a84`): Save Current Parameters as... stores the
/// stored EQ values as model 4004 rows in `zenui.db`; the page then matches the preset (Save
/// disabled). After an edit, a row tap loads the preset back into `[GlobalEq]`. A double tap
/// on GAIN restores the default preset's value (M594; the factory default: the catalog's).
#[test]
fn global_eq_preset_saves_and_loads_through_zenui_db() {
    use tetrad_ui::SettingsPresetPage as Page;
    use tetrad_ui::{EqBand, EqTab, IoTab, SettingId, SettingValue, SettingsPresetControl as P};
    let (mut state, root) = settings_preset_state("eq-presets");
    let mut ui = Ui::new();
    let _ = ui.build(&state);
    let band = EqBand::new(0).unwrap();
    let gain = |state: &State| state.settings.settings().global_eq.params.get(&0).copied();
    run(
        &mut state,
        vec![Request::Action(Action::ChangeSetting {
            id: SettingId::EqGain(band),
            value: SettingValue::Number(30),
        })],
        Duration::from_secs(1),
    );
    let saved = gain(&state);
    ui.handle(UiInput::Show(Screen::Io(IoTab::Input1)), &state, state.now);
    press(
        &mut ui,
        &mut state,
        Control::Navigate(Screen::GlobalEq(EqTab::Band(band))),
    );
    press(
        &mut ui,
        &mut state,
        Control::SettingsPreset(P::Open(Page::GlobalEq)),
    );
    press(&mut ui, &mut state, Control::SettingsPreset(P::SaveAs));
    name_preset(&mut ui, &mut state, "Stage");
    let database = state.library.as_ref().unwrap().database.as_ref().unwrap();
    let stored = database.view().model_presets().unwrap();
    assert_eq!((stored.len(), stored[0].model), (1, 4004));
    let id = stored[0].id as u64;
    let snapshot = ui.view().snapshot();
    assert_eq!(snapshot.settings_preset_match.global_eq, Some(id));

    let now = state.now;
    run(
        &mut state,
        vec![Request::Action(Action::ChangeSetting {
            id: SettingId::EqGain(band),
            value: SettingValue::Number(-50),
        })],
        now,
    );
    let _ = ui.render(&state);
    assert_ne!(gain(&state), saved);
    assert_eq!(ui.view().snapshot().settings_preset_match.global_eq, None);
    press(
        &mut ui,
        &mut state,
        Control::SettingsPreset(P::Open(Page::GlobalEq)),
    );
    press(&mut ui, &mut state, Control::SettingsPreset(P::Load(id)));
    assert_eq!(gain(&state), saved);

    let cell = Control::Setting(tetrad_ui::SettingControl::Open(SettingId::EqGain(band)));
    let now = state.now + Duration::from_secs(1);
    for at in [now, now + Duration::from_millis(150)] {
        let requests = ui.handle(UiInput::View(InputEvent::Activate(cell)), &state, at);
        run(&mut state, requests, at);
    }
    let specs = state.specs.as_ref().unwrap();
    let default = specs.model(4004).unwrap().parameter(0).unwrap();
    assert_eq!(gain(&state), Some(default.default_normalized()));
    std::fs::remove_dir_all(&root).unwrap();
}

/// I/O presets (M593; `0x361c3c` save, `0x362d18` load): the rows hold each connector by its
/// `replaces` id, the LINK flags (46, 47) and the EXP calibration (48..=51); a load writes
/// links and connectors back and re-applies them under the input external mute (`0x3634e8`,
/// review r19 F2). I/O has no Set as Default.
#[test]
fn io_preset_rows_hold_connectors_links_and_calibration() {
    use tetrad_settings::io::ConnectorId;
    use tetrad_ui::{IoTab, SettingsPresetControl as P, SettingsPresetPage as Page};
    let (mut state, root) = settings_preset_state("io-presets");
    let mut ui = Ui::new();
    let _ = ui.build(&state);
    ui.handle(UiInput::Show(Screen::Io(IoTab::Input1)), &state, state.now);
    let _ = ui.render(&state);
    let set = |state: &mut State, value: f32, linked: bool| {
        let actions = [
            Action::SetConnector {
                id: ConnectorId::In1Level,
                value,
            },
            Action::PairOutputs {
                pair: tetrad_app::device::jacks::OutputPair::Out12,
                linked,
            },
        ];
        let now = state.now;
        run(state, actions.map(Request::Action).into(), now);
    };
    set(&mut state, 0.25, true);
    press(
        &mut ui,
        &mut state,
        Control::SettingsPreset(P::Open(Page::Io)),
    );
    press(&mut ui, &mut state, Control::SettingsPreset(P::SaveAs));
    name_preset(&mut ui, &mut state, "Gig");
    let database = state.library.as_ref().unwrap().database.as_ref().unwrap();
    let preset = database.view().model_presets().unwrap()[0].clone();
    assert_eq!(preset.model, 31_000);
    let rows = database.view().model_preset_params(preset.id).unwrap();
    let value = |index: i64| {
        rows.iter()
            .find(|row| row.index == index)
            .map(|r| r.value.as_str())
    };
    assert_eq!(
        (value(0), value(46), value(47), value(48), value(49)),
        (Some("0.25"), Some("1"), Some("0"), Some("0"), Some("255"))
    );
    assert!(
        value(1).is_none(),
        "a parameter without a connector has no row"
    );

    set(&mut state, 0.75, false);
    let _ = ui.render(&state);
    press(
        &mut ui,
        &mut state,
        Control::SettingsPreset(P::Open(Page::Io)),
    );
    let id = preset.id as u64;
    press(&mut ui, &mut state, Control::SettingsPreset(P::Menu(id)));
    assert!(!ui.view().hit_regions().iter().any(|region| region.control
        == Control::SettingsPreset(P::MenuItem(tetrad_ui::DevicePresetMenu::SetDefault))));
    ui.handle(UiInput::View(InputEvent::Back), &state, state.now);
    press(&mut ui, &mut state, Control::SettingsPreset(P::Load(id)));
    // The load runs on the 10 ms settle timer (`Timer::IoPresetSettle`).
    let _ = tetrad_app::device::jacks::settle_io_loads(&mut state);
    let io = &state.settings.settings().io;
    assert!(io.out12_paired);
    assert_eq!(io.params[&100].value, Some(0.25));
    assert_eq!(state.io.value(ConnectorId::In1Level), 0.25);

    use tetrad_app::device::jacks::InputChange;
    let database = state.library.as_ref().unwrap().database.as_ref().unwrap();
    let rows = database.view().model_preset_params(preset.id).unwrap();
    let load = Action::LoadSettingsPreset { model: 31000, rows };
    let effects = tetrad_app::device::actions::reduce(load, &mut state).unwrap();
    let mute = |on| Effect::ScheduleInput(InputChange::ExternalMute(on));
    assert_eq!(
        effects,
        [
            mute(true),
            Effect::StartTimer {
                timer: tetrad_app::device::Timer::IoPresetSettle,
                after: std::time::Duration::from_millis(10),
            },
            Effect::Present,
        ],
        "muted, then the writes wait for the settle"
    );
    let effects = tetrad_app::device::jacks::settle_io_loads(&mut state);
    let writes = |effect: &Effect| matches!(effect, Effect::Hal(_) | Effect::ScheduleInput(_));
    let last = effects.iter().rposition(writes).expect("jack writes");
    assert!(last > 0, "writes before the release");
    assert_eq!(effects[last], mute(false), "released after the last write");
    std::fs::remove_dir_all(&root).unwrap();
}

/// Splitter/Mixer quick buttons (`0x3e19e8` → `0x51a090(pair, 0, 7)`): quick2 sets the split
/// and mix points of the pair in one layout edit with one undo frame.
#[test]
fn quick_split_sets_both_points_in_one_edit() {
    use tetrad_app::device::actions::LayoutEdit;
    use tetrad_engine::Pair;
    use tetrad_ui::{EditorSite, QuickSplit, RowPair};
    let mut state = split_state();
    let current = state.current.get_mut().as_mut().unwrap();
    let split = LayoutEdit::Split {
        pair: Pair::Upper,
        column: Some(Column::new(4).unwrap()),
    };
    tetrad_app::device::preset::prepare_layout_edit(current, state.specs.as_ref().unwrap(), split)
        .unwrap()
        .commit(current);
    let mut ui = Ui::new();
    let _ = ui.build(&state);
    let now = Duration::from_secs(5);
    let site = Control::Site(EditorSite::Splitter(RowPair::Upper));
    ui.handle(UiInput::View(InputEvent::Activate(site)), &state, now);
    let _ = ui.render(&state);
    let quick = Control::QuickSplit(QuickSplit::SplitAndMix);
    let requests = ui.handle(UiInput::View(InputEvent::Activate(quick)), &state, now);
    let column = |c| Some(Column::new(c).unwrap());
    assert_eq!(undo_paths(&requests), [UndoPath::Direct]);
    assert_eq!(
        edits(&requests),
        [&Action::EditLayouts(vec![
            LayoutEdit::Split {
                pair: Pair::Upper,
                column: column(0),
            },
            LayoutEdit::Mix {
                pair: Pair::Upper,
                column: column(7),
            },
        ]),]
    );
}

/// Pattern Tremolo editor (`0x3ae61c`): the bridge projects STEP n CONNECTED (hidden 27..=41)
/// for the step faders, and a sideways drag from step 1 to step 3 writes CONNECTED 1 and 2,
/// which the next frame shows as one run.
#[test]
fn pattern_tremolo_join_drag_writes_connected_flags() {
    let params: String = (0..42)
        .map(|index| {
            let kind = if index >= 27 { "switch" } else { "float" };
            let hidden = if index >= 27 { " hidden='true'" } else { "" };
            format!(
                "<Parameter name='P{index}' type='{kind}' min='0' max='1' \
defaultValue='0'{hidden}/>"
            )
        })
        .collect();
    let models = common::MODELS.replace(
        "</Models>",
        &format!(
            "<Category id='7' name='Modulation'><Model id='7040' name='Pattern Tremolo' \
blob='light'>{params}</Model></Category></Models>"
        ),
    );
    let report = common::REPORT.replace(
        "</ModuleReport>",
        "<Module id='7040' name='Pattern' blob_name='light' category='7' dm_heap='100' \
pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='1'/></Module></ModuleReport>",
    );
    let specs = Specs::resolve(&tetrad_catalog::Catalog::parse(&models, &report).unwrap()).unwrap();
    let catalog = SpecsCatalog(&specs);
    let mut preset = Preset::new_default("4.1.0", &catalog).unwrap();
    let block = Model::block(7040, (0..42).map(|_| ParamValue::Float(0.0)));
    preset
        .insert_block(Slot { row: 0, column: 2 }, block, &catalog)
        .unwrap();
    let mut state = loaded_state(specs, preset);
    let mut ui = Ui::new();
    let _ = ui.render(&state);
    let now = Duration::from_secs(5);
    let cell = target(&ui, Control::SelectCell(GridPosition::new(0, 2).unwrap()));
    let requests = tap(&mut ui, &state, cell, now);
    run(&mut state, requests, now);
    let _ = ui.render(&state);
    assert_eq!(ui.view().snapshot().step_connected, [false; 15]);

    let step = |index| Control::Editor(tetrad_ui::EditorControl::Step(index));
    let (from, to) = (target(&ui, step(0)), target(&ui, step(2)));
    let later = now + Duration::from_secs(1);
    let mut requests = ui.handle(UiInput::Touch(from), &state, later);
    requests.extend(ui.handle(UiInput::Touch(to), &state, later));
    requests.extend(ui.handle(
        UiInput::Touch(TouchSample {
            pressed: false,
            ..to
        }),
        &state,
        later + Duration::from_millis(50),
    ));
    run(&mut state, requests, later);
    let _ = ui.render(&state);
    let mut joined = [false; 15];
    joined[..2].fill(true);
    assert_eq!(ui.view().snapshot().step_connected, joined);
}

/// r25-device MR 3: the bridge reports the block whose editor shows (`0x636740` starts the
/// Parallax meter poll) and `None` when another screen covers the Grid; the polled GAIN
/// REDUCTION reply reaches the editor's meter.
#[test]
fn the_block_editor_reports_its_block_and_shows_the_polled_meter() {
    use tetrad_app::device::meters;
    use tetrad_dsp_protocol::tuner_looper::MeterRecord;
    let params: String = (0..41)
        .map(|index| format!("<Parameter name='P{index}' type='float' defaultValue='0'/>"))
        .chain(["<Parameter name='GR' type='grMeter' min='-40' max='0' defaultValue='0'/>".into()])
        .collect();
    let models = common::MODELS.replace(
        "</Models>",
        &format!(
            "<Category id='3' name='Eq'><Model id='3008' name='Parallax' blob='light'>\
{params}</Model></Category></Models>"
        ),
    );
    let report = common::REPORT.replace(
        "</ModuleReport>",
        "<Module id='3008' name='Parallax' blob_name='light' category='3' dm_heap='100' \
pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module></ModuleReport>",
    );
    let specs = Specs::resolve(&tetrad_catalog::Catalog::parse(&models, &report).unwrap()).unwrap();
    let catalog = SpecsCatalog(&specs);
    let mut preset = Preset::new_default("4.1.0", &catalog).unwrap();
    let block = Model::block(3008, [ParamValue::Float(0.0)]);
    preset
        .insert_block(Slot { row: 0, column: 2 }, block, &catalog)
        .unwrap();
    let mut state = loaded_state(specs, preset);
    let mut ui = Ui::new();
    let _ = ui.render(&state);
    let now = Duration::from_secs(5);
    let cell = target(&ui, Control::SelectCell(GridPosition::new(0, 2).unwrap()));
    let requests = tap(&mut ui, &state, cell, now);
    let slot = EngineSlot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    };
    assert!(requests.contains(&Request::Action(Action::EditorBlock(Some(slot)))));
    run(&mut state, requests, now);
    assert_eq!(state.editor_block, Some(slot));

    let placement = state
        .current
        .as_ref()
        .unwrap()
        .deployment
        .plan()
        .block(slot)
        .unwrap();
    let record = MeterRecord {
        core: placement.core.index() as i32,
        column: placement.slot.index() as i32,
        index: 41,
        value_bits: 0.1_f32.to_bits(),
    };
    assert!(meters::on_model_meter(&mut state, &record));
    let _ = ui.render(&state);
    let level = ui.view().snapshot().editor_meters[&41];
    assert!((level - 0.5).abs() < 1e-6, "{level}");

    let later = now + Duration::from_secs(1);
    let requests = ui.handle(UiInput::Show(Screen::Tuner), &state, later);
    assert!(requests.contains(&Request::Action(Action::EditorBlock(None))));
}

/// USB Statistics (`0x68a90c`): a SOC2 ARM block reaches the page rows with the HID sent
/// count read at its arrival; "---" (`None`) before it.
#[test]
fn a_soc2_arm_block_fills_the_usb_statistics_rows() {
    use tetrad_dsp_host::responses::Event as DspEvent;
    use tetrad_dsp_protocol::responses::Soc2ArmDiagnostics;
    use tetrad_ui::UsbTone;
    let (mut device, _log, _media) = boot();
    let snapshot = |device: &Device<Sim>| device.bridges().ui.view().snapshot().clone();
    assert_eq!(snapshot(&device).usb_statistics, None);
    let mut record = [0_u8; 128];
    record[0] = 1;
    record[0x30..0x34].copy_from_slice(&0x400_u32.to_le_bytes());
    record[0x78..0x7c].copy_from_slice(&5_u32.to_le_bytes());
    let block = DspEvent::Soc2ArmDiagnostics(Soc2ArmDiagnostics(record));
    device.push(Event::Dsp(CoreId::Control, block));
    device.step();
    let rows = snapshot(&device).usb_statistics.expect("the rows");
    assert_eq!(rows[0].text, "Yes");
    assert_eq!(rows[3].text, "0/5");
    assert_eq!(
        (rows[5].text.as_str(), rows[5].tone),
        ("1024", UsbTone::OutOfRange)
    );
}

/// A graph drag (`UiCommand::SetPosition`, Arpeggio Delay point `0x2acb44`): the bridge snaps
/// a stepped parameter to its step (`0x5cc994`, `0x5cc9e0`) and writes only a changed step; a
/// float without `steps` takes the dragged value as is (r23-ui MR 1).
#[test]
fn a_graph_drag_snaps_stepped_parameters_and_skips_an_unchanged_step() {
    let specs = STOCK.specs();
    let catalog = SpecsCatalog(&specs);
    let mut preset = Preset::new_default("4.1.0", &catalog).unwrap();
    // Tap 1 is on; SYNC off and SCALE Chromatic put DELAY TIME (8) on x, SEMITONES (6) on y.
    let values = [0.5, 0.0, 0.0, 0.0, 0.0, 1.0, 0.5, 0.5, 0.5, 0.5];
    let block = Model::block(6031, values.map(ParamValue::Float));
    preset
        .insert_block(Slot { row: 0, column: 2 }, block, &catalog)
        .unwrap();
    let mut state = loaded_state(specs, preset);
    let mut ui = Ui::new();
    let _ = ui.render(&state);
    let mut now = Duration::from_secs(5);
    let cell = target(&ui, Control::SelectCell(GridPosition::new(0, 2).unwrap()));
    let requests = tap(&mut ui, &state, cell, now);
    run(&mut state, requests, now);
    let _ = ui.render(&state);
    let site = Site::Slot(EngineSlot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    });
    let point = Control::Editor(tetrad_ui::EditorControl::Point(0));
    let mut drag = |state: &mut State, (x, y): (u16, u16)| {
        now += Duration::from_secs(1);
        let from = target(&ui, point);
        let to = TouchSample {
            x,
            y,
            pressed: true,
        };
        let mut requests = ui.handle(UiInput::Touch(from), state, now);
        requests.extend(ui.handle(UiInput::Touch(to), state, now));
        let release = TouchSample {
            pressed: false,
            ..to
        };
        requests.extend(ui.handle(UiInput::Touch(release), state, now));
        let written: Vec<(u32, f32)> = edits(&requests)
            .into_iter()
            .filter_map(|action| match *action {
                Action::SetParameter {
                    site: at,
                    index,
                    value,
                } if at == site => Some((index, value)),
                _ => None,
            })
            .collect();
        run(state, requests, now);
        let _ = ui.render(state);
        written
    };
    // y 177 is 0.519 on the pitch axis: still step 12 of 25 (0.5), so only the time moves.
    let written = drag(&mut state, (230, 177));
    assert!(
        matches!(written[..], [(8, time)] if (time - 0.25).abs() < 1e-3),
        "{written:?}"
    );
    // The same x keeps the time; y 164 (0.601) snaps to step 14, 14 / 24.
    let written = drag(&mut state, (230, 164));
    assert!(
        matches!(written[..], [(6, pitch)] if (pitch - 14.0 / 24.0).abs() < 1e-6),
        "{written:?}"
    );
}

/// An I/O preset load ends in the I/O reload (`0x362d18` → `0x360da8`, `jacks::reload`),
/// not the settings-reset path: after the settle timer Cortex Control gets one full
/// IOSettings push (`0x17be68`), not the per-pair echoes.
#[test]
fn io_preset_load_runs_the_io_reload() {
    use tetrad_app::device::control::io::{pair_echo, push};
    use tetrad_app::device::jacks::OutputPair;
    use tetrad_storage::stock::ParamEntry;
    let (mut state, root) = settings_preset_state("io-reload");
    let row = |index: i64, value: &str| ParamEntry {
        index,
        value: value.into(),
        exp: 0,
        exp_trim_max: 1.0,
        exp_trim_min: 0.0,
    };
    let load = Action::LoadSettingsPreset {
        model: 31_000,
        rows: vec![row(0, "0.5"), row(46, "1"), row(47, "0")],
    };
    let _ = tetrad_app::device::actions::reduce(load, &mut state).unwrap();
    let effects = tetrad_app::device::jacks::settle_io_loads(&mut state);
    assert!(effects.contains(&push(&state)));
    assert!(!effects.contains(&pair_echo(OutputPair::Out12, true)));
    assert!(!effects.contains(&pair_echo(OutputPair::Out34, false)));
    std::fs::remove_dir_all(&root).unwrap();
}

/// Review r18 (data) MR 2 and r19 F3: during a Control restore's extract the stock loading
/// overlay shows (`0x3fba64`); a footswitch press or a MIDI program change does nothing
/// (neither reaches the library, which is off the device, so no notice appears) and the
/// POWER button does not shut down. The in-place reload (`0x3fcf64`) ends the overlay and
/// gives the library back, with no restart.
#[test]
fn a_restore_shows_the_overlay_and_drops_local_input() {
    use tetrad_app::device::platform::Input;
    use tetrad_app::device::services::{BackupDone, ServiceNotice};
    use tetrad_control::codecs::{LocalBackupMessage, MessageAction};
    use tetrad_hal::hal::Event as HalEvent;
    use tetrad_hal::node::Element;
    let (mut device, log, _media) = boot();
    common::connect_control(&mut device);
    device.platform().worker.set(true);
    let update = || Some(MessageAction::Update.into());
    let chunk = LocalBackupMessage {
        action: update(),
        backup_json: Some("{}".into()),
        is_last_chunk: Some(true),
        ..Default::default()
    };
    let apply = LocalBackupMessage {
        action: update(),
        can_apply_backup: Some(true),
        ..Default::default()
    };
    common::host_send(&mut device, apply);
    common::host_send(&mut device, chunk);
    let restoring = |device: &Device<Sim>| device.bridges().ui.view().snapshot().restoring;
    assert!(restoring(&device) && device.state().library.is_none());

    let notices = log.borrow().notices.len();
    let element = Element::all().next().unwrap();
    for value in [1, 0] {
        device.push(Event::Hal(HalEvent::Stomp { element, value }));
        device.step();
    }
    let at = device.platform().clock.now();
    for byte in [0xc0, 3] {
        device.push(Event::Midi { byte, at });
    }
    (0..3).for_each(|_| _ = device.step());
    assert_eq!(log.borrow().notices.len(), notices, "no library notice");
    let power = Input::Power(tetrad_app::device::PowerAction::PowerOff);
    (device.platform().inputs.borrow_mut()).push(power);
    assert_eq!(
        (0..3).find_map(|_| device.step()),
        None,
        "POWER waits for the restore"
    );

    let done = ServiceNotice::Backup(Ok(BackupDone::Restored(3)));
    (device.platform().inputs.borrow_mut()).push(Input::Service(done));
    assert_eq!((0..3).find_map(|_| device.step()), None, "no restart");
    assert!(!restoring(&device), "the overlay ends with the reload");
    assert!(device.state().library.is_some());
}

/// Excessive Downloads (`0x22cab0`, dialog `0x22bd50`): a checked "Don't show again" closes
/// with CANCEL into `dontShowExcessCaptureDownloads`, the flag the device check reads.
#[test]
fn excessive_downloads_dont_show_again_stores_the_flag() {
    use tetrad_storage::stock::DownloadKind;
    let (mut state, root) = settings_preset_state("excess-downloads");
    let mut ui = Ui::new();
    let _ = ui.build(&state);
    let now = state.now;
    let requests = ui.handle(
        UiInput::ExcessiveDownloads(DownloadKind::Capture),
        &state,
        now,
    );
    run(&mut state, requests, now);
    let _ = ui.render(&state);
    press(
        &mut ui,
        &mut state,
        Control::Dialog(tetrad_ui::DialogButton::Check),
    );
    press(
        &mut ui,
        &mut state,
        Control::Dialog(tetrad_ui::DialogButton::Cancel),
    );
    let general = &state.settings.settings().general;
    assert!(general.dont_show_excess_capture_downloads);
    assert!(!general.dont_show_excess_preset_downloads);
    std::fs::remove_dir_all(&root).unwrap();
}

/// Review r21 H2: Multi Select COPY in setlist A, then PASTE in setlist B, copies A's presets
/// into B (`0x786d38` pastes the copied items), not B's own slots of the same numbers.
#[test]
fn multi_select_paste_copies_from_the_setlist_open_at_copy() {
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_ui::{BulkAction, LibraryControl};
    let specs = STOCK.specs();
    let preset = Preset::new_default("4.1.0", &SpecsCatalog(&specs)).unwrap();
    let mut state = loaded_state(specs, preset);
    let root = std::env::temp_dir().join(format!("tetrad-app-paste-source-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut presets = StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap();
    let [a, b] = ["A", "B"].map(|name| presets.create_setlist(name).unwrap().name);
    let setlists = presets.setlists().unwrap();
    let index = |name: &str| setlists.iter().position(|s| s.name == name).unwrap() as u64;
    let (open_a, open_b) = (index(&a), index(&b));
    state.library = Some(tetrad_app::device::Library {
        presets,
        database: None,
    });
    let mut ui = Ui::new();
    let _ = ui.render(&state);
    let mut press = |control: Control| {
        let input = UiInput::View(InputEvent::Activate(control));
        let requests = ui.handle(input, &state, state.now);
        let _ = ui.render(&state);
        requests
    };
    let library = Control::Library;
    press(Control::Navigate(Screen::Presets));
    press(library(LibraryControl::Setlist(open_a)));
    press(library(LibraryControl::MultiSelect));
    press(library(LibraryControl::Check(0)));
    press(library(LibraryControl::Bulk(BulkAction::Copy)));
    press(Control::Back);
    press(library(LibraryControl::MultiSelect));
    press(library(LibraryControl::Setlist(open_b)));
    press(library(LibraryControl::MultiSelect));
    press(library(LibraryControl::Bulk(BulkAction::Paste)));
    press(library(LibraryControl::PasteChoice(
        tetrad_ui::PasteChoice::FromEmpty,
    )));
    let pasted: Vec<_> = press(library(LibraryControl::ConfirmPaste))
        .into_iter()
        .filter_map(|request| match request {
            Request::Action(action @ Action::CopyPreset { .. }) => Some(action),
            _ => None,
        })
        .collect();
    assert_eq!(
        pasted,
        [Action::CopyPreset {
            from: a,
            slot: 0,
            to: b,
            remove: false,
        }]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// r35-ui finding: Grid ⋮ Delete Preset deletes the loaded preset in its own setlist, not
/// the slot of the same number in the setlist the Directory shows.
#[test]
fn grid_delete_preset_deletes_the_loaded_preset_not_the_browsed_slot() {
    use tetrad_library::sidecar::Sidecar;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_ui::{DialogButton, LibraryControl, MenuAction};
    let specs = STOCK.specs();
    let media = Media::new([("Light", light_preset(&specs, 0))]);
    let open_b = {
        let mut presets =
            StockLibrary::open(MediaRoot::new(&media.0), StockLimits::default()).unwrap();
        let b = presets.create_setlist("B").unwrap().name;
        let bytes = light_preset(&specs, 0).to_bytes();
        let document =
            tetrad_preset::PresetDocument::parse(&bytes, tetrad_preset::Limits::default()).unwrap();
        let stored = (presets.store(&b, 0, "In B", &document, &Sidecar::default())).unwrap();
        assert!(stored.search_index.is_none());
        let setlists = presets.setlists().unwrap();
        setlists.iter().position(|s| s.name == b).unwrap() as u64
    };
    let (mut device, _log, media) = common::boot(media);
    let loaded = device.state().current.as_ref().and_then(|c| c.path.clone());
    assert!(loaded.is_some_and(|path| path.contains("My Presets")));
    activate(&mut device, Control::Navigate(Screen::Presets));
    activate(
        &mut device,
        Control::Library(LibraryControl::Setlist(open_b)),
    );
    // The hardware back key: the setlist layer, then the Directory.
    for _ in 0..2 {
        device.push(Event::UiInput(UiInput::View(InputEvent::Back)));
        device.step();
    }
    assert_eq!(device.bridges().ui.view().screen(), Screen::Grid);
    activate(&mut device, Control::OpenMainMenu);
    scroll_device_menu(&mut device);
    activate(&mut device, Control::Menu(MenuAction::DeletePreset));
    activate(&mut device, Control::Dialog(DialogButton::Primary));
    assert!(
        media.0.join("Presets/B/In B.pb").exists(),
        "B keeps its slot 0"
    );
    assert!(!media.0.join("Presets/My Presets/Light.pb").exists());
}

/// Review r29 B1: a consecutive PASTE in its own setlist whose first pair targets the loaded
/// preset's slot is refused whole. Slots 0 (A), 1 (B), 2 (C, loaded); copy 0 and 1, paste
/// from slot 1: no pair is written, so B keeps slot 1 (per-pair copies replaced B).
#[test]
fn a_refused_paste_pair_refuses_the_whole_paste() {
    use tetrad_library::sidecar::Sidecar;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_ui::{BulkAction, LibraryControl, PasteChoice};
    let specs = STOCK.specs();
    let preset = Preset::new_default("4.1.0", &SpecsCatalog(&specs)).unwrap();
    let mut state = loaded_state(specs, preset.clone());
    let root =
        std::env::temp_dir().join(format!("tetrad-app-paste-refused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut presets = StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap();
    let live = presets.create_setlist("Live").unwrap().name;
    for (slot, name) in ["A", "B", "C"].into_iter().enumerate() {
        let mut named = preset.clone();
        named.name = Some(name.into());
        let document = tetrad_preset::PresetDocument::parse(
            &named.to_bytes(),
            tetrad_preset::Limits::default(),
        )
        .unwrap();
        let stored = presets
            .store(&live, slot, name, &document, &Sidecar::default())
            .unwrap();
        assert!(stored.search_index.is_none());
    }
    let before = presets.slots(&live).unwrap();
    let open = presets
        .setlists()
        .unwrap()
        .iter()
        .position(|s| s.name == live)
        .unwrap() as u64;
    state.current.get_mut().as_mut().unwrap().path = before[2].clone();
    state.library = Some(tetrad_app::device::Library {
        presets,
        database: None,
    });
    let mut ui = Ui::new();
    let _ = ui.render(&state);
    let mut press = |control: Control| {
        let input = UiInput::View(InputEvent::Activate(control));
        let requests = ui.handle(input, &state, state.now);
        let _ = ui.render(&state);
        requests
    };
    let library = Control::Library;
    press(Control::Navigate(Screen::Presets));
    press(library(LibraryControl::Setlist(open)));
    press(library(LibraryControl::MultiSelect));
    press(library(LibraryControl::Check(0)));
    press(library(LibraryControl::Check(1)));
    press(library(LibraryControl::Bulk(BulkAction::Copy)));
    press(Control::Back);
    press(library(LibraryControl::MultiSelect));
    press(library(LibraryControl::MultiSelect));
    press(library(LibraryControl::Bulk(BulkAction::Paste)));
    press(library(LibraryControl::PasteChoice(
        PasteChoice::FromChosen,
    )));
    press(library(LibraryControl::ConfirmPaste));
    press(library(LibraryControl::Check(1)));
    let requests = press(library(LibraryControl::Bulk(BulkAction::Paste)));
    let mut refused = 0;
    for request in requests {
        // As `Device::execute`: a refused action is a notice and the next one runs.
        if let Request::Action(action) = request {
            refused +=
                usize::from(tetrad_app::device::actions::reduce(action, &mut state).is_err());
        }
    }
    let library = &state.library.as_ref().unwrap().presets;
    assert_eq!(refused, 1, "the paste is refused");
    assert_eq!(library.slots(&live).unwrap(), before, "nothing is written");
    let _ = std::fs::remove_dir_all(&root);
}

/// Review r29 B5: the request a stored DOWNLOADS preset row sends (r29-ui §3) loads the
/// product from `downloads_product_preset` through `load_preset_path` (`0x78d204` →
/// `0x5b37f0(app, folder, -1, key)`); its path names no file, so a Save of it is refused and
/// writes no file.
#[test]
fn a_stored_downloads_preset_loads_on_the_device_and_does_not_save() {
    use tetrad_storage::stock::{DbLimits, Download, DownloadKind, StockDb};
    let specs = STOCK.specs();
    let media = Media::new([("Light", light_preset(&specs, 0))]);
    let mut lead = light_preset(&specs, 1);
    lead.name = Some("Lead".into());
    let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
    db.transaction(|write| {
        write.put_download(
            DownloadKind::Preset,
            &Download {
                id: "p-1".into(),
                hash: None,
                raw_data: Some(lead.to_bytes()),
                metadata: Some(br#"{"name":"Lead"}"#.to_vec()),
            },
        )
    })
    .unwrap();
    drop(db);
    let (mut device, log, media) = common::boot(media);
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    device.platform().wall_clock.set(Some(now));
    device.push(Event::Action(Action::Directory(
        tetrad_ui::DirectoryRequest::Audition {
            kind: tetrad_ui::ItemKind::Preset,
            folder: "cloud-0-1".into(),
            id: "p-1".into(),
        },
    )));
    device.step();
    let current = device.state().current.as_ref().unwrap();
    assert_eq!(
        (current.preset.name.as_deref(), current.path.as_deref()),
        (Some("Lead"), Some("cloud-0-1/p-1"))
    );
    let files = || {
        std::fs::read_dir(media.0.join("Presets/My Presets"))
            .unwrap()
            .count()
    };
    let before = (files(), log.borrow().notices.len());
    activate(&mut device, Control::SavePreset);
    assert_eq!(files(), before.0, "no file is written");
    let notices = &log.borrow().notices[before.1..];
    assert!(
        notices
            .iter()
            .any(|notice| notice.detail.contains("cloud-0-1/p-1")),
        "the save is refused: {notices:?}"
    );
}

/// Census I3 (`PresetMetadataDialog` `0x7e79b8`): the Save As PREFERRED INSTRUMENT goes into
/// the new sidecar (`instrument_type`, `0x7ec4c4`), and a later load of that preset starts the
/// picker on it (`0x58ed3c`).
#[test]
fn save_as_writes_the_preferred_instrument_and_a_load_reads_it() {
    use tetrad_ui::{MenuAction, PreferredInstrument, PresetMetadataControl};
    let specs = STOCK.specs();
    let (mut device, _log, media) = common::boot(Media::new([("Light", light_preset(&specs, 0))]));
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    device.platform().wall_clock.set(Some(now));
    let metadata = Control::PresetMetadata;
    activate(&mut device, Control::OpenMainMenu);
    activate(&mut device, Control::Menu(MenuAction::SaveAs));
    device.push(Event::UiInput(UiInput::View(InputEvent::TextChanged(
        "bass tone".into(),
    ))));
    device.step();
    activate(&mut device, metadata(PresetMetadataControl::Open));
    let bass = PresetMetadataControl::Instrument(PreferredInstrument::Bass);
    activate(&mut device, metadata(bass));
    activate(&mut device, metadata(PresetMetadataControl::Save));
    let sidecar = std::fs::read_to_string(media.0.join("Presets/My Presets/Bass tone.json"));
    assert!(sidecar.unwrap().contains(r#""instrument_type": "bass""#));
    let instrument = |device: &mut Device<Sim>, index| {
        let target = tetrad_app::device::PresetRef {
            setlist: None,
            index,
        };
        device.push(Event::Action(Action::LoadPreset {
            target,
            reload: true,
        }));
        device.step();
        device.bridges().ui.view().snapshot().preset_instrument
    };
    assert_eq!(instrument(&mut device, 0), PreferredInstrument::NotSet);
    assert_eq!(instrument(&mut device, 1), PreferredInstrument::Bass);
}

/// Gap I3 (`modifyPresetMetadataSignal` → `0x5b91dc` → `0x5b7438` → `0x7c4db8` `+0x5c`): Edit
/// Details of the loaded preset puts the PREFERRED INSTRUMENT page's pick into its sidecar,
/// also when the name and the preset do not change.
#[test]
fn edit_details_saves_the_picked_instrument_into_the_sidecar() {
    use tetrad_ui::{MenuAction, PreferredInstrument, PresetMetadataControl};
    let specs = STOCK.specs();
    let media = Media::new([("Light", light_preset(&specs, 0))]);
    let file = media.0.join("Presets/My Presets/Light.json");
    let sidecar = serde_json::json!({
        "author": "", "authorId": "", "cloudId": "", "corosVersion": "4.1.0", "creator": "",
        "date": "2026-10-12T10:00:00Z", "description": "made up", "instrument_type": "bass",
        "name": "Light", "tags": [],
    });
    std::fs::write(&file, sidecar.to_string()).unwrap();
    let (mut device, _log, _media) = common::boot(media);
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    device.platform().wall_clock.set(Some(now));
    let metadata = Control::PresetMetadata;
    activate(&mut device, Control::OpenMainMenu);
    activate(&mut device, Control::Menu(MenuAction::EditDetails));
    device.push(Event::UiInput(UiInput::View(InputEvent::TextChanged(
        "light".into(),
    ))));
    device.step();
    activate(&mut device, metadata(PresetMetadataControl::Open));
    let guitar = PresetMetadataControl::Instrument(PreferredInstrument::Guitar);
    activate(&mut device, metadata(guitar));
    activate(&mut device, metadata(PresetMetadataControl::Save));
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(stored["instrument_type"], "guitar");
    assert_eq!(stored["name"], "Light");

    // Review r39 K6: the next Edit Details opens on the saved pick (record `+0x5c`,
    // `0x7c4db8`), so a save without a pick keeps it.
    activate(&mut device, Control::OpenMainMenu);
    activate(&mut device, Control::Menu(MenuAction::EditDetails));
    activate(&mut device, metadata(PresetMetadataControl::Open));
    activate(&mut device, metadata(PresetMetadataControl::Save));
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(stored["instrument_type"], "guitar");
}

/// Review r39 K8: Edit Details saves, then the file follows the new name. When the rename is
/// refused (a start-up `--preset` file that the `LastPreset` slot does not name), the save is
/// still done: the preset shows saved and a notice names the rename.
#[test]
fn edit_details_ends_its_save_when_the_file_rename_fails() {
    use tetrad_app::device::bringup::RunOptions;
    use tetrad_ui::{MenuAction, PresetMetadataControl};
    let specs = STOCK.specs();
    let second = format!("{}/Second.pb", common::SETLIST);
    let options = RunOptions {
        preset: Some(second.clone()),
        ..RunOptions::STOCK
    };
    let media = Media::new([
        ("Light", light_preset(&specs, 0)),
        ("Second", light_preset(&specs, 1)),
    ]);
    let (mut device, log, _media) = common::boot_with(media, options);
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    device.platform().wall_clock.set(Some(now));
    let notices = log.borrow().notices.len();
    activate(&mut device, Control::OpenMainMenu);
    activate(&mut device, Control::Menu(MenuAction::EditDetails));
    device.push(Event::UiInput(UiInput::View(InputEvent::TextChanged(
        "renamed".into(),
    ))));
    device.step();
    let metadata = Control::PresetMetadata;
    activate(&mut device, metadata(PresetMetadataControl::Open));
    activate(&mut device, metadata(PresetMetadataControl::Save));
    let state = device.state();
    let current = state.current.as_ref().unwrap();
    assert_eq!(current.path.as_deref(), Some(second.as_str()));
    assert_eq!(current.preset.name.as_deref(), Some("Renamed"));
    assert!(!state.preset_dirty(), "the save is done");
    let log = log.borrow();
    assert!(
        (log.notices[notices..].iter()).any(|notice| notice.detail.contains("rename")),
        "{:?}",
        &log.notices[notices..]
    );
}

/// `name` stored under that name, with its "Light" block in `column`.
fn named(specs: &Specs, name: &'static str, column: usize) -> (&'static str, Preset) {
    let mut preset = light_preset(specs, column);
    preset.name = Some(name.into());
    (name, preset)
}

/// The column of the first row-1 block of the preset stored at `path`.
fn block_column(device: &Device<Sim>, path: &str) -> Option<usize> {
    let presets = &device.state().library.as_ref().unwrap().presets;
    let preset = Preset::try_from(&presets.load(path).unwrap().document).unwrap();
    (0..8).find(|&column| preset.block(Slot { row: 0, column }).unwrap().is_some())
}

/// Review r39 K1 (P0): a slot UNDO follows the loaded preset only when the frame held that
/// preset, never by path. "Old" (slot 2, not loaded) is deleted; the loaded preset is renamed
/// "Old" and takes the freed `Old.pb`. The UNDO brings "Old" back in a file of its own and the
/// loaded preset keeps its file, so a SAVE writes only the loaded preset.
#[test]
fn a_slot_undo_keeps_the_loaded_preset_on_its_own_file() {
    let specs = STOCK.specs();
    let media = Media::new([
        named(&specs, "Light", 0),
        named(&specs, "Second", 1),
        named(&specs, "Old", 4),
    ]);
    let (mut device, _log, _media) = common::boot(media);
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    device.platform().wall_clock.set(Some(now));
    let act = |device: &mut Device<Sim>, action| {
        device.push(Event::Action(action));
        device.step();
    };
    let setlist = || "My Presets".to_owned();
    act(
        &mut device,
        Action::DeletePreset {
            setlist: setlist(),
            slot: 2,
        },
    );
    let rename = Action::RenamePreset {
        setlist: setlist(),
        slot: 0,
        name: "Old".into(),
    };
    act(&mut device, rename);
    let old = format!("{}/Old.pb", common::SETLIST);
    let loaded = |device: &Device<Sim>| device.state().current.as_ref().unwrap().path.clone();
    assert_eq!(loaded(&device), Some(old.clone()));

    act(&mut device, Action::Undo);
    let presets = &device.state().library.as_ref().unwrap().presets;
    let restored = format!("{}/Old_1.pb", common::SETLIST);
    assert_eq!(
        presets.slots("My Presets").unwrap()[2],
        Some(restored.clone())
    );
    assert_eq!(loaded(&device), Some(old.clone()));

    activate(&mut device, Control::SavePreset);
    assert_eq!(block_column(&device, &restored), Some(4));
    assert_eq!(block_column(&device, &old), Some(0));
}

/// Review r39 K4 (O): a load frame keeps the stamp of its file. "Second" loads, "Light" is
/// deleted and Second is saved as "Light" into the freed slot, so the slot names `Light.pb`
/// again, but another file. The load UNDO loads nothing, says so and the frame drops: it would
/// put Light's old working preset on Second's file.
#[test]
fn a_load_undo_refuses_a_file_replaced_since() {
    use tetrad_ui::{MenuAction, PresetMetadataControl};
    let specs = STOCK.specs();
    let media = Media::new([named(&specs, "Light", 0), named(&specs, "Second", 1)]);
    let (mut device, log, _media) = common::boot(media);
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    device.platform().wall_clock.set(Some(now));
    let act = |device: &mut Device<Sim>, action| {
        device.push(Event::Action(action));
        device.step();
    };
    let target = tetrad_app::device::PresetRef {
        setlist: None,
        index: 1,
    };
    act(
        &mut device,
        Action::LoadPreset {
            target,
            reload: true,
        },
    );
    let delete = Action::DeletePreset {
        setlist: "My Presets".into(),
        slot: 0,
    };
    act(&mut device, delete);
    activate(&mut device, Control::OpenMainMenu);
    activate(&mut device, Control::Menu(MenuAction::SaveAs));
    device.push(Event::UiInput(UiInput::View(InputEvent::TextChanged(
        "light".into(),
    ))));
    device.step();
    let metadata = Control::PresetMetadata;
    activate(&mut device, metadata(PresetMetadataControl::Open));
    activate(&mut device, metadata(PresetMetadataControl::Save));
    let light = format!("{}/Light.pb", common::SETLIST);
    let presets = &device.state().library.as_ref().unwrap().presets;
    assert_eq!(presets.slots("My Presets").unwrap()[0], Some(light.clone()));

    let notices = log.borrow().notices.len();
    act(&mut device, Action::Undo);
    act(&mut device, Action::Undo);
    let state = device.state();
    assert_eq!(state.history.frame_types(), (vec![], vec![]));
    let current = state.current.as_ref().unwrap();
    assert_eq!(current.path, Some(light.clone()));
    let block = current.preset.block(Slot { row: 0, column: 1 });
    assert!(block.unwrap().is_some(), "Second's content stays");
    let log = log.borrow();
    assert!(
        (log.notices[notices..].iter()).any(|notice| notice.detail.contains("load undo refused")),
        "{:?}",
        &log.notices[notices..]
    );
}

/// A Cortex Control RecallPreset UPDATE of `preset`: an edit of the loaded preset.
fn recall(preset: &Preset) -> tetrad_control::codecs::RecallPresetMessage {
    use tetrad_control::codecs::{MessageAction, RawMessageBytes, RecallPresetMessage};
    RecallPresetMessage {
        action: Some(MessageAction::Update.into()),
        request_id: Some(9),
        preset: Some(RawMessageBytes {
            bytes: preset.to_bytes(),
        }),
        ..Default::default()
    }
}

/// "Light" (slot 0, loaded, block in column 0), "Second" (slot 1, column 1) and "Old" (slot 2,
/// column 4), with Control connected and a wall clock for saves.
fn three_presets() -> (Device<Sim>, Rc<RefCell<Log>>, Media) {
    let specs = STOCK.specs();
    let media = Media::new([
        named(&specs, "Light", 0),
        named(&specs, "Second", 1),
        named(&specs, "Old", 4),
    ]);
    let (mut device, log, media) = common::boot(media);
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    device.platform().wall_clock.set(Some(now));
    common::connect_control(&mut device);
    (device, log, media)
}

/// The block column of the loaded preset and the load-undo refusals since `mark`.
fn after_load_undo(device: &Device<Sim>, log: &Rc<RefCell<Log>>, mark: usize) -> Option<usize> {
    let log = log.borrow();
    let refused = (log.notices[mark..].iter()).find(|n| n.detail.contains("load undo refused"));
    assert!(refused.is_none(), "{refused:?}");
    let preset = &device.state().current.as_ref().unwrap().preset;
    (0..8).find(|&column| preset.block(Slot { row: 0, column }).unwrap().is_some())
}

/// Review r41 L1 (O, C result): Tetrad's own SAVE of a preset does not make its older load frames
/// stale. Light loads twice around Second; the second Light is edited (column 5) and saved, so
/// `Light.pb` is a new file. Three UNDOs reach the first load frame: stock reloads Light's file
/// and puts back that frame's working preset (column 0); the file keeps the saved content.
#[test]
fn a_load_undo_reloads_a_file_tetrad_saved_since() {
    let (mut device, log, _media) = three_presets();
    let act = |device: &mut Device<Sim>, action| {
        device.push(Event::Action(action));
        device.step();
    };
    let load = |index| Action::LoadPreset {
        target: tetrad_app::device::PresetRef {
            setlist: None,
            index,
        },
        reload: true,
    };
    act(&mut device, load(1));
    act(&mut device, load(0));
    common::host_send(&mut device, recall(&named(&STOCK.specs(), "Light", 5).1));
    activate(&mut device, Control::SavePreset);
    let light = format!("{}/Light.pb", common::SETLIST);
    assert_eq!(block_column(&device, &light), Some(5));

    let mark = log.borrow().notices.len();
    (0..3).for_each(|_| act(&mut device, Action::Undo));
    assert_eq!(after_load_undo(&device, &log, mark), Some(0));
    let state = device.state();
    assert_eq!(state.current.as_ref().unwrap().path, Some(light.clone()));
    assert_eq!(state.history.frame_types(), (vec![], vec![0, 5, 5]));
    assert_eq!(block_column(&device, &light), Some(5), "no file is written");
}

/// Review r41 L1 (O, C result): a library UNDO that puts a deleted preset back at its own path
/// does not make that preset's load frame stale. Light is edited (column 5), Second loads, Light
/// (not loaded) is deleted and the delete is undone. The load UNDO loads Light with its edits.
#[test]
fn a_load_undo_reloads_a_file_a_library_undo_put_back() {
    let (mut device, log, _media) = three_presets();
    let act = |device: &mut Device<Sim>, action| {
        device.push(Event::Action(action));
        device.step();
    };
    common::host_send(&mut device, recall(&named(&STOCK.specs(), "Light", 5).1));
    let load = Action::LoadPreset {
        target: tetrad_app::device::PresetRef {
            setlist: None,
            index: 1,
        },
        reload: true,
    };
    act(&mut device, load);
    let delete = Action::DeletePreset {
        setlist: "My Presets".into(),
        slot: 0,
    };
    act(&mut device, delete);
    act(&mut device, Action::Undo);
    let light = format!("{}/Light.pb", common::SETLIST);
    let presets = &device.state().library.as_ref().unwrap().presets;
    assert_eq!(presets.slots("My Presets").unwrap()[0], Some(light.clone()));

    let mark = log.borrow().notices.len();
    act(&mut device, Action::Undo);
    assert_eq!(after_load_undo(&device, &log, mark), Some(5));
    let state = device.state();
    assert_eq!(state.current.as_ref().unwrap().path, Some(light.clone()));
    assert_eq!(state.history.frame_types(), (vec![0], vec![1, 5]));
    assert_eq!(block_column(&device, &light), Some(0), "no file is written");
}

/// Review r41 L2 (O): the loaded "Light" is deleted (the new-preset template plays in slot 0
/// with no file, `0x5b2c54`) and a copy of "Old" fills slot 0. SAVE asks for a name and saves
/// the template into the first empty slot of the setlist (as Save As, `0x7c3d94`), which
/// becomes the loaded slot; the copy stays.
#[test]
fn a_preset_without_a_file_saves_to_a_free_slot_when_its_slot_is_taken() {
    use tetrad_ui::PresetMetadataControl;
    let (mut device, _log, _media) = three_presets();
    let act = |device: &mut Device<Sim>, action| {
        device.push(Event::Action(action));
        device.step();
    };
    let my = || "My Presets".to_owned();
    act(
        &mut device,
        Action::DeletePreset {
            setlist: my(),
            slot: 0,
        },
    );
    let copy = Action::CopyPresetTo {
        from: my(),
        slot: 2,
        to: my(),
        to_slot: Some(0),
        remove: false,
    };
    act(&mut device, copy);
    let slots = |device: &Device<Sim>| {
        let presets = &device.state().library.as_ref().unwrap().presets;
        presets.slots("My Presets").unwrap()[..4].to_vec()
    };
    let refill = slots(&device)[0].clone().unwrap();

    activate(&mut device, Control::SavePreset);
    device.push(Event::UiInput(UiInput::View(InputEvent::TextChanged(
        "fresh".into(),
    ))));
    device.step();
    activate(
        &mut device,
        Control::PresetMetadata(PresetMetadataControl::Open),
    );
    activate(
        &mut device,
        Control::PresetMetadata(PresetMetadataControl::Save),
    );
    let fresh = format!("{}/Fresh.pb", common::SETLIST);
    let state = device.state();
    assert_eq!(state.current.as_ref().unwrap().path, Some(fresh.clone()));
    assert_eq!(state.settings.settings().last_preset.preset_index, 3);
    let slots = slots(&device);
    assert_eq!(
        (&slots[0], &slots[3]),
        (&Some(refill.clone()), &Some(fresh.clone()))
    );
    assert_eq!(
        block_column(&device, &refill),
        Some(4),
        "the copy is untouched"
    );
    assert_eq!(
        block_column(&device, &fresh),
        None,
        "the template has no block"
    );
}

/// A device whose boot corruption check (`0x702f70`) quarantines the user capture "broken".
fn quarantined_boot() -> (Device<Sim>, Media) {
    use tetrad_app::device::media::CAPTURE_CONVERSION_FINISHED;
    use tetrad_app::device::platform::{Platform as _, ServiceBackends as _};
    use tetrad_storage::stock::{
        CaptureEncryption, CaptureStore, DbLimits, ProductType, StockDb, StockProduct,
    };
    let specs = common::STOCK.specs();
    let media = Media::new([("Light", common::light_preset(&specs, 0))]);
    let secret = b"invented test secret";
    let cipher = || tetrad_media::factory::DeviceCipher::caller_secret(secret);
    // Review r25 Q7: a check that judges every row bad holds them all, so one sound row
    // sits next to the broken one.
    let (mut raw, mut metadata) = (
        common::sound_v1_capture(),
        br#"{"name":"Sound","version":"1"}"#.to_vec(),
    );
    cipher().apply(&mut raw);
    cipher().apply(&mut metadata);
    let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
    db.transaction(|write| {
        write.put_product(&StockProduct {
            id: "sound".into(),
            product_type: ProductType::Capture,
            read_only: false,
            raw_data: Some(raw),
            metadata: Some(metadata),
        })?;
        write.put_product(&StockProduct {
            id: "broken".into(),
            product_type: ProductType::Capture,
            read_only: false,
            raw_data: Some(vec![1, 2, 3]),
            metadata: Some(b"not encrypted JSON".to_vec()),
        })?;
        write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
        write.set_capture_encryption(CaptureStore::Downloads, CaptureEncryption::Local)
    })
    .unwrap();
    drop(db);
    let log = Rc::new(RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    sim.media_cipher = Some(cipher());
    sim.services().files().put(CAPTURE_CONVERSION_FINISHED, "");
    (Device::start(sim).unwrap(), media)
}

/// r23-media list → "Corrupt Neural Capture" (`0x54663c`): the capture the boot corruption
/// check (`0x702f70`) quarantined shows in the dialog after the first step, once.
#[test]
fn a_quarantined_capture_shows_the_corrupt_captures_dialog() {
    let (mut device, _media) = quarantined_boot();
    let dialog = |device: &Device<Sim>| {
        let ui = &device.bridges().ui;
        ui.view()
            .hit_regions()
            .iter()
            .any(|r| r.label == "SEND REPORT")
    };
    assert!(!dialog(&device));
    device.step();
    assert!(dialog(&device));
    assert!(device.take_corrupt_captures().is_empty());
}

/// Review r23 K5 way back (O): Device Storage lists the quarantined capture, and RESTORE puts
/// it back into its store (`restore_quarantined_capture`), so the row leaves the page.
#[test]
fn device_storage_restore_returns_a_quarantined_capture() {
    let (mut device, _media) = quarantined_boot();
    device.step();
    activate(
        &mut device,
        Control::Dialog(tetrad_ui::DialogButton::Cancel),
    );
    let storage = Screen::Settings(SettingsPage::DeviceStorage);
    device.push(Event::UiInput(UiInput::Show(storage)));
    device.step();
    let rows = |device: &Device<Sim>| device.bridges().ui.view().snapshot().quarantine.clone();
    assert_eq!(rows(&device).len(), 1);
    assert_eq!(rows(&device)[0].id, "broken");
    activate(&mut device, Control::RestoreCapture(0));
    device.step();
    assert!(rows(&device).is_empty());
}

/// Review r23 K2: a DSP crash (`dspCrashSignal` → `0x5149dc`) shows the DSPErrorDialog over
/// the Grid. A REBOOT tap (`0x514a6c`) hides it, sets the last preset to Factory Library
/// slot 0 and asks the DSP fault restart (`0x6a8914(…, 2)`).
#[test]
fn a_dsp_crash_shows_the_error_dialog_and_its_reboot_restarts() {
    use tetrad_app::device::{FACTORY_SETLIST, PowerAction};
    use tetrad_dsp_host::responses::{CrashCause, Event as DspEvent};
    use tetrad_dsp_protocol::DspCore;
    use tetrad_watchdog::power::RestartReason;
    let (mut device, _log, _media) = boot();
    let reboot = Control::DspError { reboot: true };
    let shown = |device: &Device<Sim>| {
        let ui = &device.bridges().ui;
        let regions = ui.view().hit_regions();
        regions.iter().any(|r| r.enabled && r.control == reboot)
    };
    assert!(!shown(&device));
    let core = CoreId::Dsp(DspCore::Core2);
    let crash = DspEvent::Crash(CrashCause::SendTimeouts { core });
    device.push(Event::Dsp(core, crash));
    device.step();
    assert!(shown(&device), "the crash shows the dialog");

    let press = target(&device.bridges().ui, reboot);
    device.push(Event::UiInput(UiInput::Touch(press)));
    device.step();
    device.push(Event::UiInput(UiInput::Touch(TouchSample {
        pressed: false,
        ..press
    })));
    let restart = RestartReason::DspFault { delay_seconds: 2 };
    assert_eq!(device.step(), Some(PowerAction::Restart(restart)));
    assert!(device.state().dsp_fault.is_some_and(|fault| !fault.dialog));
    let last = &device.state().settings.settings().last_preset;
    assert_eq!(
        (
            last.setlist.as_deref(),
            last.preset_index,
            last.key.as_str()
        ),
        (Some(FACTORY_SETLIST), 0, "")
    );
}

/// r27-ui (census #2): a services refusal that answers a user request shows in the
/// MessageDialog (owned rule, `UserJob`); before r27 it stayed in the log.
#[test]
fn a_services_refusal_of_a_user_request_shows_in_the_message_dialog() {
    let (mut device, log, _media) = boot();
    let page = Screen::Settings(SettingsPage::SendReport);
    device.push(Event::UiInput(UiInput::Show(page)));
    device.step();
    activate(
        &mut device,
        Control::Action(tetrad_ui::DeviceAction::SendReport),
    );
    assert!(
        (log.borrow().notices.iter())
            .any(|notice| notice.detail.contains("support reports are turned off")),
        "{:?}",
        log.borrow().notices
    );
    let notice = device
        .bridges()
        .ui
        .view()
        .snapshot()
        .notice
        .clone()
        .unwrap();
    assert_eq!((notice.title.as_str(), notice.button), ("Network", "OK"));
    assert!(
        notice.body.contains("support reports are turned off"),
        "{}",
        notice.body
    );
}

/// r27-ui MERGE REQUEST 1 (built by r28-ui): the first boot after an update reads the marker
/// `changelog_boot` (`0x54491c` → `0x4e4398`), shows "SEE WHAT'S NEW" once and removes the
/// marker with a sync (`0x4e43b4`), so the next boot shows nothing.
#[test]
fn the_first_boot_after_an_update_shows_the_release_notes_once() {
    use tetrad_app::device::platform::{Platform, ServiceBackends};
    use tetrad_services::updater::paths::CHANGELOG_BOOT;
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let log = Rc::new(RefCell::new(Log::default()));
    let mut sim = Sim::new(STOCK, &media, &log);
    // `MemFiles` clones share their files.
    let files = sim.services().files().clone();
    for marker in [
        tetrad_app::device::media::CAPTURE_CORRUPTION_CHECKED,
        tetrad_app::device::media::CAPTURE_CONVERSION_FINISHED,
    ] {
        files.put(marker, "");
    }
    files.put(CHANGELOG_BOOT, "4.1.1\nYour %1 got new amps.\n");
    let syncs = files.syncs();
    let device = Device::start(sim).unwrap();
    assert!(device.bridges().ui.view().overlay_open());
    assert_eq!(files.get(CHANGELOG_BOOT), None);
    assert!(files.syncs() > syncs);
}

/// [`STOCK`] with a capture model whose parameter 5 is the capture text (`file_name`) and the
/// IR Loader 29001 (parameters 2 and 22 are the IR key and name), for Add to Grid.
fn grid_media_fixture() -> common::Fixture {
    let capture = "<Model id='14001' name='Capture 2' blob='light'>\
<Parameter name='GAIN' type='float' min='0' max='1' defaultValue='0.5'/></Model></Category>";
    let mut params: String = (0..5)
        .map(|index| format!("<Parameter name='P{index}' type='float' defaultValue='0.5'/>"))
        .collect();
    params.push_str("<Parameter name='file_name' type='string' defaultValue=''/>");
    let mut ir: String = (0..23)
        .map(|index| match index {
            2 | 22 => format!("<Parameter name='S{index}' type='string' defaultValue=''/>"),
            _ => format!("<Parameter name='P{index}' type='float' defaultValue='0'/>"),
        })
        .collect();
    ir = format!(
        "<Category id='29' name='IR Loader'><Model id='29001' name='IR Loader' blob='light'>{ir}</Model></Category>"
    );
    let models = STOCK.models.replacen(
        capture,
        &format!("<Model id='14001' name='Capture 2' blob='light'>{params}</Model></Category>{ir}"),
        1,
    );
    assert_ne!(models, STOCK.models);
    let module = "<Module id='29001' name='IR Loader' blob_name='light' category='29' dm_heap='100' \
pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module></ModuleReport>";
    let report = STOCK.report.replacen("</ModuleReport>", module, 1);
    common::Fixture {
        models: Box::leak(models.into_boxed_str()),
        report: Box::leak(report.into_boxed_str()),
        ..STOCK
    }
}

/// A device on [`grid_media_fixture`] with "Light" in row 1, column 3, the user capture `k1`
/// ("Plexi", version "2") and the user IRs `CIR_7` ("Room") and `CIR_8` ("Hall").
fn grid_media_device() -> (Device<Sim>, Media) {
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    use tetrad_app::device::platform::{Platform as _, ServiceBackends as _};
    use tetrad_storage::stock::{
        CaptureEncryption, CaptureStore, DbLimits, ProductType, StockDb, StockProduct,
    };
    let fixture = grid_media_fixture();
    let media = Media::new([("Light", light_preset(&fixture.specs(), 2))]);
    let cipher = tetrad_media::factory::DeviceCipher::caller_secret(b"invented test secret");
    let ciphered = |json: &str| {
        let mut bytes = json.as_bytes().to_vec();
        cipher.apply(&mut bytes);
        bytes
    };
    let mut db =
        tetrad_storage::stock::StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false)
            .unwrap();
    let product = |id: &str, product_type, metadata| StockProduct {
        id: id.into(),
        product_type,
        read_only: false,
        raw_data: Some(vec![1; 8]),
        metadata: Some(metadata),
    };
    db.transaction(|write| {
        write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
        let capture = ciphered(r#"{"name":"Plexi","version":"2"}"#);
        write.put_product(&product("k1", ProductType::Capture, capture))?;
        for (id, name) in [("CIR_7", "Room"), ("CIR_8", "Hall")] {
            let metadata = ciphered(&format!(r#"{{"name":"{name}"}}"#));
            write.put_product(&product(id, ProductType::Ir, metadata))?;
        }
        Ok(())
    })
    .unwrap();
    drop::<StockDb>(db);
    let log = Rc::new(RefCell::new(Log::default()));
    let mut sim = Sim::new(fixture, &media, &log);
    sim.media_cipher = Some(cipher.clone());
    let files = sim.services().files().clone();
    for marker in [CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED] {
        files.put(marker, "");
    }
    (Device::start(sim).unwrap(), media)
}

/// Opens Directory category `category` (`Category::ALL` index) and taps the media row `value`.
fn tap_media_row(device: &mut Device<Sim>, category: u16, value: &str) {
    use tetrad_ui::LibraryControl;
    activate(device, Control::Navigate(Screen::Presets));
    activate(device, Control::Library(LibraryControl::Category));
    activate(device, Control::Choose(category));
    let snapshot = device.bridges().ui.view().snapshot();
    let row = (snapshot.media.iter())
        .position(|entry| entry.value == value)
        .unwrap();
    activate(device, Control::Library(LibraryControl::Media(row as u16)));
}

fn cell(row: u8, column: u8) -> GridPosition {
    GridPosition::new(row, column).unwrap()
}

fn grid_block(device: &Device<Sim>, row: u8, column: u8) -> Option<tetrad_engine::Instance> {
    let slot = EngineSlot {
        row: Row::new(row).unwrap(),
        column: Column::new(column).unwrap(),
    };
    let current = device.state().current.as_ref().unwrap();
    current
        .deployment
        .layout()
        .instance(Site::Slot(slot))
        .cloned()
}

fn text(block: &tetrad_engine::Instance, index: u32) -> Vec<Option<String>> {
    (block.texts.iter().find(|text| text.index == index))
        .map(|text| text.scenes.clone())
        .unwrap_or_default()
}

/// re-r31 §1 (`0x598bac`, `0x596c50`, `0x704530`): a capture tap starts Add to Grid; the cell
/// of "Light" shows Replace, an empty cell Add. The tap replaces the block with model 14001
/// (version "2"), parameter 5 = key + name in all eight scenes, as one undo step.
#[test]
fn add_to_grid_replaces_an_occupied_cell_with_the_capture_block() {
    use tetrad_ui::CellFit;
    let (mut device, _media) = grid_media_device();
    tap_media_row(&mut device, 2, "k1");
    let cells = device.bridges().ui.view().snapshot().add_to_grid.unwrap();
    assert_eq!((cells[0][2], cells[0][0]), (CellFit::Replace, CellFit::Add));
    activate(&mut device, Control::AddToGrid(Some(cell(0, 2))));
    let block = grid_block(&device, 0, 2).unwrap();
    assert_eq!(block.model.0, 14_001);
    assert_eq!(text(&block, 5), vec![Some("k1Plexi".to_owned()); 8]);
    activate(&mut device, Control::Undo);
    assert_eq!(grid_block(&device, 0, 2).unwrap().model.0, 1);
}

/// Review r32 E5 (O): a layout edit from another source while Add to Grid is open (here an
/// injected action, as Cortex Control sends one) makes the cell fits stale, so the mode ends.
#[test]
fn add_to_grid_ends_when_the_layout_changes_under_it() {
    use tetrad_app::device::actions::LayoutEdit;
    let (mut device, _media) = grid_media_device();
    tap_media_row(&mut device, 2, "k1");
    assert!(device.bridges().ui.view().snapshot().add_to_grid.is_some());
    let slot = EngineSlot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    };
    device.push(Event::Action(Action::EditLayout(LayoutEdit::Remove(slot))));
    (0..20).for_each(|_| _ = device.step());
    assert!(grid_block(&device, 0, 2).is_none(), "the edit committed");
    assert!(device.bridges().ui.view().snapshot().add_to_grid.is_none());
}

/// Review r32 E6 (`0x7e5a30`: remove, then rename): IR OVERWRITE renames only after a good
/// delete. Here the other IR is gone from `zenui.db` before OVERWRITE, so the delete fails
/// and the IR keeps its old name: two user IRs never share one name.
#[test]
fn ir_overwrite_renames_only_after_a_good_delete() {
    use tetrad_storage::stock::{DbLimits, StockDb};
    use tetrad_ui::{ItemAction, Key, LibraryControl};
    let (mut device, media) = grid_media_device();
    activate(&mut device, Control::Navigate(Screen::Presets));
    activate(&mut device, Control::Library(LibraryControl::Category));
    activate(&mut device, Control::Choose(3));
    let snapshot = device.bridges().ui.view().snapshot();
    let row = (snapshot.media.iter())
        .position(|entry| entry.value == "CIR_8")
        .unwrap();
    activate(
        &mut device,
        Control::Library(LibraryControl::MediaMenu(row as u16)),
    );
    activate(
        &mut device,
        Control::Library(LibraryControl::Item(ItemAction::EditName)),
    );
    device.push(Event::UiInput(UiInput::View(InputEvent::TextChanged(
        "Room".into(),
    ))));
    device.step();
    activate(&mut device, Control::Key(Key::Done));
    let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
    db.transaction(|write| write.delete_product("CIR_7"))
        .unwrap();
    drop(db);
    activate(
        &mut device,
        Control::Dialog(tetrad_ui::DialogButton::Primary),
    );
    let view = (device.state().library.as_ref())
        .and_then(|library| library.database.as_ref())
        .unwrap()
        .view();
    let mut metadata = view.product("CIR_8").unwrap().unwrap().metadata.unwrap();
    tetrad_media::factory::DeviceCipher::caller_secret(b"invented test secret")
        .apply(&mut metadata);
    let metadata = String::from_utf8(metadata).unwrap();
    assert!(metadata.contains("Hall"), "{metadata}");
}

/// re-r31 §1 IR branch (`0x598b10`, `0x596c50`): an IR tap places an IR Loader with parameter 2
/// = the IR key and parameter 22 = its name. §3 (O): OVERWRITE of an IR that the loaded preset
/// uses is refused, so the IR stays.
#[test]
fn add_to_grid_ir_block_sets_key_and_name() {
    use tetrad_ui::{ItemAction, Key, LibraryControl};
    let (mut device, _media) = grid_media_device();
    tap_media_row(&mut device, 3, "CIR_7");
    activate(&mut device, Control::AddToGrid(Some(cell(1, 0))));
    let block = grid_block(&device, 1, 0).unwrap();
    assert_eq!(block.model.0, 29_001);
    assert_eq!(text(&block, 2), vec![Some("CIR_7".to_owned()); 8]);
    assert_eq!(text(&block, 22), vec![Some("Room".to_owned()); 8]);

    activate(&mut device, Control::Navigate(Screen::Presets));
    let snapshot = device.bridges().ui.view().snapshot();
    let row = (snapshot.media.iter())
        .position(|entry| entry.value == "CIR_8")
        .unwrap();
    activate(
        &mut device,
        Control::Library(LibraryControl::MediaMenu(row as u16)),
    );
    activate(
        &mut device,
        Control::Library(LibraryControl::Item(ItemAction::EditName)),
    );
    device.push(Event::UiInput(UiInput::View(InputEvent::TextChanged(
        "Room".into(),
    ))));
    device.step();
    activate(&mut device, Control::Key(Key::Done));
    activate(
        &mut device,
        Control::Dialog(tetrad_ui::DialogButton::Primary),
    );
    let view = (device.state().library.as_ref())
        .and_then(|library| library.database.as_ref())
        .unwrap()
        .view();
    assert!(
        view.product("CIR_7").unwrap().is_some(),
        "the used IR stays"
    );
}

/// re-r34 §5.4: a library swap frame outlives a preset load, but a factory reset clears
/// every undo frame before it loads the factory preset (`0x4de79c` → `0x4dea3c` →
/// `0x5b37f0`, C), so UNDO cannot swap two slots of the wiped library.
#[test]
fn a_factory_reset_clears_the_library_swap_frames() {
    let specs = STOCK.specs();
    let media = Media::new([
        ("Light", light_preset(&specs, 2)),
        ("Crunch", light_preset(&specs, 2)),
        ("Lead", light_preset(&specs, 2)),
    ]);
    let (mut device, _log, _media) = common::boot(media);
    device.push(Event::Action(Action::SwapPresets {
        from: "My Presets".into(),
        slot: 1,
        to: "My Presets".into(),
        to_slot: 2,
    }));
    (0..20).for_each(|_| _ = device.step());
    assert!(device.state().history.can_undo(), "the swap is undoable");
    device.push(Event::Action(Action::Reset(
        tetrad_settings::device::ResetKind::Factory,
    )));
    (0..50).for_each(|_| _ = device.step());
    assert!(!device.state().history.can_undo());
}

/// Health r34 #9 (a): `Ui::render` presents the kept `tetrad_ui::Renderer` frame. After a grid
/// tap (block editor), Back and an unchanged pass, it equals a full render pixel for pixel.
#[test]
fn the_kept_frame_equals_a_full_render() {
    let (device, _log, _media) = boot();
    let state = device.state();
    let mut ui = Ui::new();
    ui.build(state).unwrap();
    let cell = target(&ui, Control::SelectCell(GridPosition::new(0, 2).unwrap()));
    let now = Duration::from_secs(5);
    let steps: [&dyn Fn(&mut Ui); 3] = [
        &|ui| _ = tap(ui, state, cell, now),
        &|ui| _ = ui.handle(UiInput::View(InputEvent::Back), state, now),
        &|_| {},
    ];
    ui.render(state).unwrap();
    for (index, step) in steps.iter().enumerate() {
        step(&mut ui);
        let frame = ui.render(state).unwrap().to_vec();
        assert_eq!(frame, ui.view().render().rgb(), "step {index}");
    }
}

/// The display gets only the rows that changed since the frame it took: every row that differs
/// lies in the reported rows; an unchanged pass hands out nothing, a failed present all rows.
#[test]
fn the_display_gets_the_changed_rows() {
    let (device, _log, _media) = boot();
    let state = device.state();
    let mut ui = Ui::new();
    ui.build(state).unwrap();
    let first = ui.render_for_display(state).unwrap().unwrap();
    assert_eq!(first.rows, 0..480);
    let mut shown = first.rgb.to_vec();
    let cell = target(&ui, Control::SelectCell(GridPosition::new(0, 2).unwrap()));
    let now = Duration::from_secs(5);
    let row = |rgb: &[u8], y: usize| rgb[y * 2400..(y + 1) * 2400].to_vec();
    // A press highlights one tile (a few rows); the release opens the editor.
    for pressed in [true, false] {
        let touch = TouchSample { pressed, ..cell };
        _ = ui.handle(UiInput::Touch(touch), state, now);
        let frame = ui.render_for_display(state).unwrap().unwrap();
        let (rgb, rows) = (frame.rgb, frame.rows);
        let changed: Vec<usize> = (0..480)
            .filter(|&y| row(rgb, y) != row(&shown, y))
            .collect();
        assert!(!changed.is_empty(), "{rows:?}");
        assert!(!pressed || rows.len() < 100, "{rows:?}");
        assert!(
            changed.iter().all(|&y| rows.contains(&(y as u32))),
            "{rows:?}"
        );
        shown = rgb.to_vec();
        assert!(ui.render_for_display(state).unwrap().is_none());
    }
    ui.unshown();
    assert_eq!(ui.render_for_display(state).unwrap().unwrap().rows, 0..480);
}

/// Health r34 #9: host timing of the device loop. An idle `Device::step` with Cortex Control
/// connected (views and Control pushes, no frame), then one frame of an unchanged view and
/// one of a view that alternates between the grid and the block editor, and one full redraw of
/// the view (the cost of a screen change). Timing only:
/// `cargo test -p tetrad-app --release --test device_ui loop_timing -- --ignored --nocapture`.
#[test]
#[ignore = "timing only"]
fn loop_timing() {
    use std::time::Instant;
    const STEPS: u32 = 20_000;
    const FRAMES: u32 = 200;
    let (mut device, _log, _media) = boot();
    common::connect_control(&mut device);
    let start = Instant::now();
    for _ in 0..STEPS {
        device.step();
    }
    eprintln!("idle step: {:?}", start.elapsed() / STEPS);
    let state = device.state();
    let mut ui = Ui::new();
    ui.build(state).unwrap();
    ui.render(state).unwrap();
    let start = Instant::now();
    for _ in 0..FRAMES {
        std::hint::black_box(ui.render(state).unwrap());
    }
    eprintln!("unchanged frame: {:?}", start.elapsed() / FRAMES);
    let start = Instant::now();
    for _ in 0..FRAMES {
        std::hint::black_box(ui.view().render());
    }
    eprintln!(
        "full frame (no damage reuse): {:?}",
        start.elapsed() / FRAMES
    );
    let cell = target(&ui, Control::SelectCell(GridPosition::new(0, 2).unwrap()));
    let mut now = Duration::from_secs(5);
    let start = Instant::now();
    for _ in 0..FRAMES {
        now += Duration::from_secs(1);
        tap(&mut ui, state, cell, now);
        std::hint::black_box(ui.render(state).unwrap());
    }
    eprintln!("tap + changed frame: {:?}", start.elapsed() / FRAMES);
}

/// Host timing of the I/O Settings passes the QC measured slow (2026-10-16): opening the page
/// from the Grid and a switch tap on it, each as whole `Device::step`s (input, views, frame),
/// and the frame alone (UI sync, layout, paint). Timing only:
/// `cargo test -p tetrad-app --release --test device_ui io_settings_timing -- --ignored
/// --nocapture`.
#[test]
#[ignore = "timing only"]
fn io_settings_timing() {
    use std::time::Instant;
    use tetrad_ui::{InputJack, IoTab, SettingControl, SettingId};
    const ROUNDS: u32 = 100;
    let (mut device, log, _media) = boot();
    let ground_lift = SettingId::InputGroundLift(InputJack::In1);
    // Steps from the input to the pass that presents its frame (at most five).
    let step = |device: &mut Device<Sim>, input: UiInput| {
        let frames = log.borrow().frames.len();
        device.push(Event::UiInput(input.clone()));
        let start = Instant::now();
        for _ in 0..5 {
            device.step();
            if log.borrow().frames.len() > frames {
                return start.elapsed();
            }
        }
        panic!("no frame: {input:?}");
    };
    let [mut open, mut toggle, mut close] = [Duration::ZERO; 3];
    for _ in 0..ROUNDS {
        open += step(&mut device, UiInput::Show(Screen::Io(IoTab::Input1)));
        let ground = (device.bridges().ui.view().hit_regions().into_iter())
            .map(|region| region.control)
            .find(|control| {
                matches!(control, Control::Setting(SettingControl::Set(id, _)) if *id == ground_lift)
            })
            .unwrap();
        toggle += step(&mut device, UiInput::View(InputEvent::Activate(ground)));
        close += step(&mut device, UiInput::View(InputEvent::Back));
    }
    eprintln!(
        "open I/O {:?}, switch tap {:?}, close {:?} (steps to the frame)",
        open / ROUNDS,
        toggle / ROUNDS,
        close / ROUNDS
    );
    let state = device.state();
    let mut ui = Ui::new();
    ui.build(state).unwrap();
    ui.handle(UiInput::Show(Screen::Io(IoTab::Input1)), state, state.now);
    let start = Instant::now();
    for _ in 0..ROUNDS {
        std::hint::black_box(ui.view().render());
    }
    eprintln!("I/O full render: {:?}", start.elapsed() / ROUNDS);
    ui.render(state).unwrap();
    let start = Instant::now();
    for _ in 0..ROUNDS {
        std::hint::black_box(ui.render(state).unwrap());
    }
    eprintln!(
        "I/O unchanged frame (sync, layout, compare): {:?}",
        start.elapsed() / ROUNDS
    );
}

/// Stock lists the read-only Factory Library first as "Factory Presets" (number 0); a slot of
/// it loads by the factory path. Tetrad left it out (seen on the QC, 2026-10-15).
#[test]
fn factory_presets_list_first_and_load_from_the_factory_library() {
    use tetrad_app::device::FACTORY_SETLIST;
    use tetrad_app::device::PresetRef;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::{SETLIST_SLOTS, encode_setlist};
    use tetrad_ui::LibraryControl;
    let specs = STOCK.specs();
    let preset = Preset::new_default("4.1.0", &SpecsCatalog(&specs)).unwrap();
    let mut state = loaded_state(specs, preset);
    let root = std::env::temp_dir().join(format!("tetrad-app-factory-list-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let library = root.join("factory/Factory Library");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut slots = vec![None; SETLIST_SLOTS];
    slots[0] = Some(format!("{FACTORY_SETLIST}/One.pb"));
    std::fs::write(library.join("setlist"), encode_setlist(&slots).unwrap()).unwrap();
    let mut presets = StockLibrary::open(MediaRoot::new(&root), StockLimits::default())
        .unwrap()
        .with_factory(MediaRoot::factory(root.join("factory")));
    presets.create_setlist("Mine").unwrap();
    state.library = Some(tetrad_app::device::Library {
        presets,
        database: None,
    });
    let mut ui = Ui::new();
    let _ = ui.render(&state);
    let mut press = |control: Control| {
        let input = UiInput::View(InputEvent::Activate(control));
        let requests = ui.handle(input, &state, state.now);
        let _ = ui.render(&state);
        requests
    };
    press(Control::Navigate(Screen::Presets));
    press(Control::Library(LibraryControl::Setlist(0)));
    let loads: Vec<_> = press(Control::SelectPreset(0))
        .into_iter()
        .filter_map(|request| match request {
            Request::Action(Action::LoadPreset { target, .. }) => Some(target),
            _ => None,
        })
        .collect();
    assert_eq!(
        loads,
        [PresetRef {
            setlist: Some(FACTORY_SETLIST.into()),
            index: 0,
        }]
    );
    let titles: Vec<_> = (ui.view().snapshot().setlists.iter())
        .map(|setlist| setlist.title.as_str())
        .collect();
    assert_eq!(titles, ["Factory Presets", "Mine"]);
    let _ = std::fs::remove_dir_all(&root);
}

/// "Light" in slot 0 of My Presets (loaded at boot) and of a second setlist "B"; returns the
/// boot and the name of B's directory.
fn light_in_two_setlists() -> (Device<Sim>, Rc<RefCell<Log>>, Media, String) {
    use tetrad_library::sidecar::Sidecar;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    let specs = STOCK.specs();
    let media = Media::new([("Light", light_preset(&specs, 0))]);
    let b = {
        let mut presets =
            StockLibrary::open(MediaRoot::new(&media.0), StockLimits::default()).unwrap();
        let b = presets.create_setlist("B").unwrap().name;
        let bytes = light_preset(&specs, 0).to_bytes();
        let document =
            tetrad_preset::PresetDocument::parse(&bytes, tetrad_preset::Limits::default()).unwrap();
        let stored = (presets.store(&b, 0, "Light", &document, &Sidecar::default())).unwrap();
        assert!(stored.search_index.is_none());
        b
    };
    let (device, log, media) = common::boot(media);
    (device, log, media, b)
}

/// Review r35 G1: Grid ⋮ Delete Preset is open on My Presets slot 0 when slot 0 of setlist B
/// (same number, same title) loads. The confirm closes, and a tap on its place deletes
/// neither preset.
#[test]
fn grid_delete_preset_deletes_nothing_after_another_setlist_loads_the_same_slot() {
    use tetrad_app::device::PresetRef;
    use tetrad_ui::{DialogButton, MenuAction};
    let (mut device, _log, media, b) = light_in_two_setlists();
    let primary = Control::Dialog(DialogButton::Primary);
    activate(&mut device, Control::OpenMainMenu);
    scroll_device_menu(&mut device);
    activate(&mut device, Control::Menu(MenuAction::DeletePreset));
    let press = target(&device.bridges().ui, primary);
    device.push(Event::Action(Action::LoadPreset {
        target: PresetRef {
            setlist: Some(b.clone()),
            index: 0,
        },
        reload: false,
    }));
    device.step();
    let path = device.state().current.as_ref().and_then(|c| c.path.clone());
    assert!(
        path.is_some_and(|path| path.contains(&b)),
        "B's slot 0 loaded"
    );
    let ui = &device.bridges().ui;
    assert!(
        !(ui.view().hit_regions().iter()).any(|region| region.control == primary),
        "the confirm closed"
    );
    for pressed in [true, false] {
        device.push(Event::UiInput(UiInput::Touch(TouchSample {
            pressed,
            ..press
        })));
        device.step();
    }
    assert!(media.0.join(format!("Presets/{b}/Light.pb")).exists());
    assert!(media.0.join("Presets/My Presets/Light.pb").exists());
}

/// Review r35 G9: the Directory Delete confirm of slot 0 stays across a list read without a
/// write, but closes when the slot is written over in place by another preset with the same
/// title (a same-name OVERWRITE keeps the path). DELETE then sends nothing.
#[test]
fn a_slot_confirm_closes_when_the_slot_is_written_over_with_the_same_title() {
    use tetrad_library::sidecar::Sidecar;
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_ui::{DialogButton, ItemAction, LibraryControl};
    let specs = STOCK.specs();
    let media = Media::new([("Light", light_preset(&specs, 0))]);
    let preset = Preset::new_default("4.1.0", &SpecsCatalog(&specs)).unwrap();
    let mut state = loaded_state(specs.clone(), preset);
    state.library = Some(tetrad_app::device::Library {
        presets: StockLibrary::open(MediaRoot::new(&media.0), StockLimits::default()).unwrap(),
        database: None,
    });
    let primary = Control::Dialog(DialogButton::Primary);
    let mut ui = Ui::new();
    ui.build(&state).unwrap();
    for control in [
        Control::Navigate(Screen::Presets),
        Control::PresetMenu(0),
        Control::Library(LibraryControl::Item(ItemAction::Delete)),
    ] {
        ui.handle(
            UiInput::View(InputEvent::Activate(control)),
            &state,
            state.now,
        );
    }
    let shown = |ui: &mut Ui, state: &State| {
        ui.build(state).unwrap();
        (ui.view().hit_regions().iter()).any(|region| region.control == primary)
    };
    assert!(
        shown(&mut ui, &state),
        "a read without a write keeps the confirm"
    );

    let presets = &mut state.library.as_mut().unwrap().presets;
    let before = presets.slots("My Presets").unwrap()[0].clone();
    let bytes = light_preset(&specs, 1).to_bytes();
    let document = tetrad_preset::PresetDocument::parse(&bytes, Default::default()).unwrap();
    let saved = presets.replace("My Presets", 0, "Light", &document, &Sidecar::default());
    assert_eq!(Some(saved.unwrap().value), before, "in place");
    assert!(!shown(&mut ui, &state), "the confirm closed");
    let requests = ui.handle(
        UiInput::View(InputEvent::Activate(primary)),
        &state,
        state.now,
    );
    assert!(
        !(requests.iter()).any(|r| matches!(r, Request::Action(Action::DeletePreset { .. }))),
        "{requests:?}"
    );
}

/// Review r35 G10: Grid ⋮ Delete Preset refuses a loaded preset that two slots name (here
/// slots 0 and 1 of My Presets): both slots and the file stay.
#[test]
fn grid_delete_preset_refuses_a_preset_two_slots_name() {
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_storage::media::{SETLIST_SLOTS, encode_setlist};
    use tetrad_ui::{DialogButton, MenuAction};
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 0))]);
    let library = || StockLibrary::open(MediaRoot::new(&media.0), StockLimits::default());
    let slots = library().unwrap().slots("My Presets").unwrap();
    let mut doubled = vec![None; SETLIST_SLOTS];
    doubled[..2].fill(slots[0].clone());
    let file = media.0.join("Presets/My Presets/setlist");
    std::fs::write(&file, encode_setlist(&doubled).unwrap()).unwrap();
    let (mut device, _log, media) = common::boot(media);
    activate(&mut device, Control::OpenMainMenu);
    scroll_device_menu(&mut device);
    activate(&mut device, Control::Menu(MenuAction::DeletePreset));
    activate(&mut device, Control::Dialog(DialogButton::Primary));
    let library = StockLibrary::open(MediaRoot::new(&media.0), StockLimits::default()).unwrap();
    let after = library.slots("My Presets").unwrap();
    assert_eq!(after[..2], doubled[..2]);
    assert!(media.0.join("Presets/My Presets/Light.pb").exists());
}

/// Review r33 F7: a Multi Select cloud preset download onto an occupied slot asks "Overwrite
/// Existing Presets?" (`0x46485c(name, 1)`). SKIP downloads only into the empty slot and the
/// occupied one keeps its preset; OVERWRITE (the device's `overwrite: true` arm) replaces it.
#[test]
fn a_cloud_preset_download_overwrites_or_skips_an_occupied_slot() {
    use tetrad_library::stock::{StockLibrary, StockLimits};
    use tetrad_services::http::Method;
    use tetrad_ui::{BulkAction, DialogButton, LibraryControl};
    let library = Control::Library;
    let specs = STOCK.specs();
    for (button, kept) in [
        (DialogButton::Cancel, "Other"),
        (DialogButton::Primary, "One"),
    ] {
        let (mut device, media) = common::boot_online(Media::new([
            ("Light", light_preset(&specs, 0)),
            ("Other", light_preset(&specs, 1)),
        ]));
        let cloud = device.platform().cloud.clone();
        let rows =
            r#"{"data":[{"productId":"p-1","type":"preset"},{"productId":"p-2","type":"preset"}]}"#;
        let mut routes = cloud.borrow_mut();
        routes.route(Method::Get, "/search/v1/for/preset", 200, rows);
        routes.preset("p-1", "One", &light_preset(&specs, 2));
        routes.preset("p-2", "Two", &light_preset(&specs, 3));
        drop(routes);
        activate(&mut device, Control::Navigate(Screen::Presets));
        activate(&mut device, library(LibraryControl::Cloud));
        activate(&mut device, library(LibraryControl::MultiSelect));
        for row in [0, 1] {
            activate(&mut device, library(LibraryControl::Check(row)));
        }
        activate(
            &mut device,
            library(LibraryControl::Bulk(BulkAction::Download)),
        );
        activate(&mut device, library(LibraryControl::Setlist(0)));
        activate(&mut device, library(LibraryControl::ConfirmPaste));
        // Slot 1 holds "Other", slot 2 is empty (slot 0 is the loaded preset).
        for slot in [1, 2] {
            activate(&mut device, library(LibraryControl::Check(slot)));
        }
        activate(
            &mut device,
            library(LibraryControl::Bulk(BulkAction::Paste)),
        );
        activate(&mut device, Control::Dialog(button));
        (0..5).for_each(|_| _ = device.step());
        let presets = StockLibrary::open(MediaRoot::new(&media.0), StockLimits::default());
        let slots = presets.unwrap().directory_slots("My Presets").unwrap();
        let names: Vec<&str> = slots[..3].iter().map(|slot| slot.name.as_str()).collect();
        assert_eq!(names, ["Light", kept, "Two"], "{button:?}");
    }
}

/// An online device ([`common::start_online`]) whose user capture store holds `count` sound V1
/// captures "Amp", "Amp 2", .. (tags `clean`) under the hashes `c…`, `d…`, .., with both
/// capture ciphers (made-up secrets). The cloud already holds each under the account as `c-1`
/// with the tag `lead`: an upload asks before it overwrites (status 8, `0x6f7630`). Returns
/// the device, its media, the cloud and the first hash.
fn capture_upload_device(count: u8) -> (Device<Sim>, Media, Rc<RefCell<common::Cloud>>, String) {
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    use tetrad_app::device::platform::{Platform as _, ServiceBackends as _};
    use tetrad_media::factory::DeviceCipher;
    use tetrad_services::http::Method;
    use tetrad_storage::stock::{
        CaptureEncryption, CaptureStore, DbLimits, ProductType, StockDb, StockProduct,
    };
    let hash = "c".repeat(64);
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 0))]);
    let cipher = DeviceCipher::caller_secret(b"invented test secret");
    let sealed = |plain: &[u8]| {
        let mut bytes = plain.to_vec();
        cipher.apply(&mut bytes);
        bytes
    };
    let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
    db.transaction(|write| {
        write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
        let root = write.boot_maintenance()?.0;
        for index in 0..count {
            let id = char::from(b'c' + index).to_string().repeat(64);
            let name = match index {
                0 => "Amp".to_owned(),
                _ => format!("Amp {}", index + 1),
            };
            let metadata = format!(r#"{{"name":"{name}","tags":["clean"]}}"#);
            write.put_product(&StockProduct {
                id: id.clone(),
                product_type: ProductType::Capture,
                read_only: false,
                raw_data: Some(sealed(&common::sound_v1_capture())),
                metadata: Some(sealed(metadata.as_bytes())),
            })?;
            write.link(root, &id)?;
        }
        Ok(())
    })
    .unwrap();
    drop(db);
    let log = Rc::new(RefCell::new(Log::default()));
    let mut sim = Sim::new(STOCK, &media, &log);
    sim.media_cipher = Some(cipher.clone());
    let files = sim.services().files().clone();
    for marker in [CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED] {
        files.put(marker, "");
    }
    let cloud = sim.cloud.clone();
    let mut device = common::start_online(sim);
    device.set_capture_seed(DeviceCipher::caller_secret(b"invented seed-only secret"));
    let mut routes = cloud.borrow_mut();
    let mine = r#"{"existing":[{"cloud_id":"c-1","author":true}]}"#;
    routes.route(Method::Get, "/products/exists?hash=", 200, mine);
    routes.route(
        Method::Get,
        "/user/products/exists?name=",
        200,
        r#"{"product_id":"c-1"}"#,
    );
    let record = r#"{"id":"c-1","type":"neural_capture","name":"Amp","tags":["lead"]}"#;
    routes.route(Method::Get, "/products/c-1", 200, record);
    routes.route(Method::Put, "/user/products/c-1", 200, "{}");
    drop(routes);
    (device, media, cloud, hash)
}

/// re-r34 §1 and r36-upload, end to end on the device: Directory capture ⋮ Upload of a capture
/// the account already holds with other tags asks "overwrite" on the device screen. CANCEL
/// (`0x1d5a84`) sends nothing more and writes nothing; OVERWRITE (`0x1e2924`) puts to the found
/// id and writes only the capture's `cloudUUID`.
#[test]
fn a_capture_upload_asks_then_overwrites_or_cancels() {
    use tetrad_services::http::Method;
    use tetrad_ui::{DialogButton, ItemAction, LibraryControl};
    let library = Control::Library;
    for (button, puts) in [(DialogButton::Cancel, 0), (DialogButton::Primary, 1)] {
        let (mut device, _media, cloud, hash) = capture_upload_device(1);
        activate(&mut device, Control::Navigate(Screen::Presets));
        activate(&mut device, library(LibraryControl::Category));
        activate(&mut device, Control::Choose(2));
        activate(&mut device, library(LibraryControl::MediaMenu(0)));
        activate(
            &mut device,
            library(LibraryControl::Item(ItemAction::Upload)),
        );
        let labels = |device: &Device<Sim>| -> Vec<String> {
            let regions = device.bridges().ui.view().hit_regions();
            regions.iter().map(|region| region.label.clone()).collect()
        };
        assert_eq!(labels(&device), ["CANCEL", "OVERWRITE"]);
        assert_eq!(cloud.borrow().requests.len(), 3, "only the check");
        activate(&mut device, Control::Dialog(button));
        (0..3).for_each(|_| _ = device.step());
        let cloud = cloud.borrow();
        assert_eq!(
            cloud.sent(Method::Put, "/user/products/c-1").len(),
            puts,
            "{button:?}"
        );
        assert!(cloud.sent(Method::Post, "/user/products").is_empty());
        let library = device.state().library.as_ref().unwrap();
        let db = library.database.as_ref().unwrap();
        let row = db.view().product(&hash).unwrap().unwrap();
        let mut metadata = row.metadata.unwrap();
        tetrad_media::factory::DeviceCipher::caller_secret(b"invented test secret")
            .apply(&mut metadata);
        let metadata: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
        let cloud_id = (puts == 1).then_some("c-1");
        assert_eq!(metadata["cloudUUID"].as_str(), cloud_id, "{button:?}");
    }
}

/// re-r38 §2 on the device: Multi Select UPLOAD of two captures the account already holds runs
/// one batch. The device shows the progress dialog (`0x785dcc`, item 1 of 2) under one batch
/// question with OVERWRITE / SKIP (`0x463d2c`); one OVERWRITE serves both items (two PUTs),
/// and good batch items open no result dialog (stock only moves the bar).
#[test]
fn a_batch_capture_upload_asks_once_and_shows_its_progress() {
    use tetrad_services::http::Method;
    use tetrad_ui::{BulkAction, DialogButton, LibraryControl, UploadProgress};
    let library = Control::Library;
    let (mut device, _media, cloud, _) = capture_upload_device(2);
    activate(&mut device, Control::Navigate(Screen::Presets));
    activate(&mut device, library(LibraryControl::Category));
    activate(&mut device, Control::Choose(2));
    activate(&mut device, library(LibraryControl::MultiSelect));
    for row in [0, 1] {
        activate(&mut device, library(LibraryControl::Check(row)));
    }
    activate(
        &mut device,
        library(LibraryControl::Bulk(BulkAction::Upload)),
    );
    (0..3).for_each(|_| _ = device.step());
    let snapshot = device.bridges().ui.view().snapshot().clone();
    assert_eq!(
        snapshot.upload_progress,
        Some(UploadProgress {
            noun: "Neural Captures",
            handled: 1,
            total: 2,
        })
    );
    let labels = |device: &Device<Sim>| -> Vec<String> {
        let regions = device.bridges().ui.view().hit_regions();
        regions.iter().map(|region| region.label.clone()).collect()
    };
    assert_eq!(labels(&device), ["SKIP", "OVERWRITE"]);
    activate(&mut device, Control::Dialog(DialogButton::Primary));
    (0..6).for_each(|_| _ = device.step());
    assert_eq!(
        cloud.borrow().sent(Method::Put, "/user/products/c-1").len(),
        2
    );
    let view = device.bridges().ui.view();
    assert_eq!(view.snapshot().upload_progress, None);
    assert!(!view.overlay_open(), "no result dialog");
}

/// re-r38 §3 (`0x463d2c` flag set): the overwrite question of a Cortex Control capture UPLOAD
/// (46, type 2) goes to Control only; the device screen does not show it.
#[test]
fn a_control_upload_question_stays_off_the_device_screen() {
    use tetrad_control::codecs::CloudProductMessage;
    let (mut device, _media, _cloud, hash) = capture_upload_device(1);
    common::connect_control(&mut device);
    common::host_send(
        &mut device,
        CloudProductMessage {
            action: Some(tetrad_control::codecs::MessageAction::Upload.into()),
            product_keys: vec![hash],
            product_type: Some(2),
            ..Default::default()
        },
    );
    let services = &device.bridges().services;
    assert_eq!(services.capture_upload_question(), Some("Amp"));
    let snapshot = device.bridges().ui.view().snapshot();
    assert_eq!(snapshot.capture_upload_overwrite, None);
    assert!(!device.bridges().ui.view().overlay_open());
}

/// re-r44 §3: with no Wi-Fi the cloud is usable while Cortex Control is connected and its
/// KeepAlive says online (`0x669580`); an offline KeepAlive takes it away.
#[test]
fn cloud_is_ready_over_cortex_control_without_wifi() {
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 2))]);
    let log = Rc::new(RefCell::new(Log::default()));
    let mut sim = Sim::new(STOCK, &media, &log);
    sim.sign_in();
    let mut device = Device::start(sim).unwrap();
    common::connect_control(&mut device);
    let ready = |device: &Device<Sim>| device.bridges().ui.view().snapshot().cloud_ready();
    assert!(!ready(&device), "connected, not online");
    common::set_control_online(&mut device, true);
    device.step();
    let snapshot = device.bridges().ui.view().snapshot();
    assert_ne!(
        snapshot.wifi.status,
        tetrad_ui::WifiStatus::InternetConnected
    );
    assert!(ready(&device));
    common::set_control_online(&mut device, false);
    device.step();
    assert!(!ready(&device), "Control offline");
}

/// Factory 1A "Brit 2203" on the QC (`st5a.png`): row 3 stores no input under row 1's "Row 3"
/// output, so stock shows "Prev. Row" there (`0x87fc2c`). An Adaptive Gate whose SOURCE picks a
/// block is a sink, that block a source (`0x0052e53c`); the stored input flag marks "In 1".
/// A scene with SOURCE Off shows no marks.
#[test]
fn the_grid_shows_prev_row_and_the_sidechain_marks_of_the_scene() {
    let specs = STOCK.specs();
    let catalog = SpecsCatalog(&specs);
    let mut preset = light_preset(&specs, 0);
    // THRESHOLD, SOURCE, MUTE. One preceding source: 15 steps; 1.0 is step 14, the Light block
    // ("R1 C1").
    let gate = Model::block(16_007, [0.5, 1.0, 0.0].map(ParamValue::Float));
    preset
        .insert_block(Slot { row: 0, column: 3 }, gate, &catalog)
        .unwrap();
    let source = &mut preset.chains[0].models[3].params[1];
    source.scene_mode = Some(true);
    source.values[1].value = Some(ParamValue::Float(0.0));
    preset.chains[0].out_portid = Some(tetrad_engine::OutputPort::NextRow3 as u32);
    preset.chains[2].in_portid = Some(0);
    preset.chains[0].input_control[0].sidechain_source_flag = Some(true);
    let (mut device, _log, _media) = common::boot(Media::new([("Gate", preset)]));

    let snapshot = device.bridges().ui.view().snapshot().clone();
    assert_eq!(snapshot.rows[2].input, tetrad_ui::InputPort::PrevRow);
    let marked = |marks: &[bool]| {
        (marks.iter().enumerate())
            .filter_map(|(cell, &on)| on.then_some(cell))
            .collect::<Vec<_>>()
    };
    assert_eq!(marked(&snapshot.sidechain.sinks), [3]);
    assert_eq!(marked(&snapshot.sidechain.sources), [0]);
    assert_eq!(snapshot.sidechain.inputs, [true, false, false, false]);

    device.push(Event::Action(Action::SelectScene(
        EngineScene::new(1).unwrap(),
    )));
    device.step();
    let marks = device.bridges().ui.view().snapshot().sidechain;
    assert_eq!(
        (marked(&marks.sinks), marked(&marks.sources)),
        (vec![], vec![])
    );
}

/// Factory 1A on the QC (`st5a.png`): the row 1 capture is a "pedal" capture (its `factory.db`
/// metadata `deviceType`), so stock draws `NC_pedal.png` in drive orange with the capture type
/// badge (`0x2eb99c`, `0x2eb380`, `0x2ebf94`). The media library reads the type when it loads
/// the capture for a deploy.
#[test]
fn loading_a_capture_reads_its_device_type() {
    use tetrad_app::device::media::MediaLibrary;
    use tetrad_storage::stock::{DbLimits, ProductType, StockDb, StockProduct};
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 0))]);
    let cipher = tetrad_media::factory::DeviceCipher::caller_secret(b"invented test secret");
    let sealed = |plain: &[u8]| {
        let mut bytes = plain.to_vec();
        cipher.apply(&mut bytes);
        bytes
    };
    let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
    db.transaction(|write| {
        for (id, metadata) in [
            (
                "k1",
                r#"{"name":"Pedal","deviceType":"pedal","version":"1"}"#,
            ),
            ("k2", r#"{"name":"Plain","version":"1"}"#),
        ] {
            write.put_product(&StockProduct {
                id: id.into(),
                product_type: ProductType::Capture,
                read_only: false,
                raw_data: Some(sealed(&common::sound_v1_capture())),
                metadata: Some(sealed(metadata.as_bytes())),
            })?;
        }
        Ok(())
    })
    .unwrap();
    let catalog = tetrad_media::catalog::FactoryIrCatalog::new(Vec::new()).encode();
    let mut library = MediaLibrary::new(&catalog, cipher.clone()).unwrap();
    let (_, failures) = library.resolve(["k1", "k2"], |_| Err("no file".into()), Some(&db));
    assert_eq!(failures, Vec::<String>::new());
    let devices: Vec<_> = (library.capture_devices().iter())
        .map(|(value, device)| (value.as_str(), *device))
        .collect();
    assert_eq!(devices, [("k1", tetrad_ui::CaptureDevice::Pedal)]);
}

/// The editor header's Virtual Device Preset match (`0x1f7f50`) for the "Light" block (GAIN 0.5,
/// the catalog default). Candidates go in stock map order, the id as text, then user before
/// factory: user "1" has the value but an EXP link, and user "10" is within Qt's relative 1e-5,
/// so "10" matches before factory "10", user "7" and the catalog default entry.
#[test]
fn editor_preset_is_the_first_preset_in_stock_order_that_the_block_holds() {
    let media = media();
    let insert = |db: &str, rows: &[(i64, &str, i64)]| {
        let db = rusqlite::Connection::open(media.0.join(db)).unwrap();
        for (id, value, exp) in rows {
            db.execute_batch(&format!(
                "INSERT INTO model_presets VALUES ({id}, 'P{id}', 1, 2, 'false', 'false', 600);
                 INSERT INTO model_preset_parameters VALUES ({id}, 0, '{value}', {exp}, 1.0, 0.0);"
            ))
            .unwrap();
        }
    };
    insert(
        "zenui.db",
        &[(1, "0.5", 1), (7, "0.5", 0), (10, "0.500001", 0)],
    );
    insert("factory/factory.db", &[(10, "0.5", 0)]);
    let (mut device, _log, _media) = common::boot(media);
    // The boot read lists the rows; the editor shows the match.
    let block = GridPosition::new(0, 2).unwrap();
    for _ in 0..2 {
        activate(&mut device, Control::SelectCell(block));
    }
    let snapshot = device.bridges().ui.view().snapshot();
    assert_eq!(
        snapshot.editor_preset,
        Some(tetrad_ui::PresetMatch::Preset(10))
    );
    assert_eq!(
        snapshot.device_preset,
        Some(10),
        "the open list marks it too"
    );
}

/// Without a stored preset the block at its catalog defaults matches the catalog default entry
/// ("Neural DSP® Default"); 1e-5 absolute off is not within Qt's relative 1e-5 of 0.5. The scene
/// bypass counts as the BYPASS parameter (I), so a bypassed block matches nothing.
#[test]
fn editor_preset_falls_back_to_the_catalog_defaults_and_counts_the_bypass() {
    let (mut device, _log, media) = boot();
    rusqlite::Connection::open(media.0.join("zenui.db"))
        .unwrap()
        .execute_batch(
            "INSERT INTO model_presets VALUES (3, 'Near', 1, 2, 'false', 'false', 600);
             INSERT INTO model_preset_parameters VALUES (3, 0, '0.50001', 0, 1.0, 0.0);",
        )
        .unwrap();
    activate(
        &mut device,
        Control::SelectCell(GridPosition::new(0, 2).unwrap()),
    );
    let matched = |device: &Device<Sim>| device.bridges().ui.view().snapshot().editor_preset;
    assert_eq!(
        matched(&device),
        Some(tetrad_ui::PresetMatch::CatalogDefault)
    );
    activate(&mut device, Control::Param(ParamControl::Bypass));
    assert_eq!(matched(&device), None);
    activate(&mut device, Control::Param(ParamControl::Bypass));
    assert_eq!(
        matched(&device),
        Some(tetrad_ui::PresetMatch::CatalogDefault)
    );
}

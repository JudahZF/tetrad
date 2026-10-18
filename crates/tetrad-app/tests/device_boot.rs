//! Boots the device on the shared simulated platform (`common`) through the stock start-up
//! order to the first deployed preset and the splash dismissal, then powers off.

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use common::{CODE, Log, Mark, Media, SOC2_BUFFER, STOCK, Sim, light_preset, target};
use tetrad_app::device::capture::{Swap, run_entry};
use tetrad_app::device::platform::{Marker, TouchSample};
use tetrad_app::device::preset::UndoFrame;
use tetrad_app::device::ui::UiInput;
use tetrad_app::device::{Action, Device, Event, PowerAction, PresetRef, SPLASH_POLL};
use tetrad_capture::entry::{EntryAction, Hardware};
use tetrad_dsp_host::session::Clock;
use tetrad_dsp_protocol::{CoreId, DspCore};
use tetrad_engine::{Column, Node, Row, Site, Slot as EngineSlot};
use tetrad_hal::hal::{Command as HalCommand, Event as HalEvent};
use tetrad_hal::node::Element;
use tetrad_hal::zenjack::{Control as JackControl, HeadphoneVolume, Setting, Value};
use tetrad_loader::PhysAddr;
use tetrad_settings::SettingsFileId;
use tetrad_settings::controls::FootswitchMode;
use tetrad_ui::{Control, GridPosition};
use tetrad_watchdog::power::PowerStep;
use tetrad_watchdog::splash::SplashCommand;

/// Slots 0 and 1 hold a one-block preset (the block in column 2, then 3).
fn media() -> Media {
    let specs = STOCK.specs();
    Media::new([
        ("Light", light_preset(&specs, 2)),
        ("Second", light_preset(&specs, 3)),
    ])
}

/// Runs one pass with `event` queued, then idles one pass so the scripted DSP consumes what
/// was sent.
fn drive(device: &mut Device<Sim>, event: Event) {
    device.push(event);
    assert!(device.step().is_none());
    assert!(device.step().is_none());
}

/// Review r30 C7: a QC with the ESS codec reads the U-Boot environment once per boot for both
/// readers (the master volume range and the output calibration; stock `0x47d994`).
#[test]
fn a_qc_boot_runs_fw_printenv_once() {
    let media = media();
    let log = Rc::new(RefCell::new(Log::default()));
    let mut sim = Sim::new(STOCK, &media, &log);
    sim.markers.push(Marker::EssEmulation);
    let device = Device::start(sim).unwrap();
    assert!(device.state().platform.has_ess_codec());
    let runs = (device.platform().commands().iter())
        .filter(|command| command.program == "fw_printenv")
        .count();
    assert_eq!(runs, 1);
}

/// Stock start-up order (o-lifecycle §4, a-boot §2, c-dsp §6): splash progress, SOC1 kernels
/// through the booter, handshake and SOC2 boot, key pulls, the catalog from the DSP key, DDC,
/// the last preset deployed inside a master mute with the 20 ms settle, splash 100 and the
/// DISMISS one second later. The running device then turns a grid touch into a selection
/// frame and a scene-mode footswitch into the stock scene recall. Power-off mutes first and
/// flushes settings before the storage teardown.
#[test]
fn simulated_platform_boots_to_first_preset_and_powers_off_in_stock_order() {
    let media = media();
    let log = Rc::new(RefCell::new(Log::default()));
    let sim = Sim::new(STOCK, &media, &log);
    let core_windows = sim.core_windows.clone();
    let (core1, core2) = (sim.firmware.core1.clone(), sim.firmware.core2.clone());
    let mut device = Device::start(sim).unwrap();

    // SOC1: both kernels streamed byte for byte, the QC argument placed, the marker set.
    let bus = device.platform().bus.clone().unwrap();
    let bus = bus.0.borrow();
    let [sim2, sim1] = [&bus.peer[0], &bus.peer[1]];
    assert_eq!(
        (sim1.received.as_slice(), sim2.received.as_slice()),
        (&core1[..], &core2[..])
    );
    assert_eq!(sim1.arguments.as_deref(), Some(&b"QC"[..]));
    assert_eq!(bus.memory.bytes(PhysAddr(SOC2_BUFFER), 40), vec![0x5a; 40]);
    assert_eq!(
        bus.memory.bytes(PhysAddr(SOC2_BUFFER + 48), 24),
        vec![0xc0; 24]
    );
    assert!(device.platform().markers.contains(&Marker::SharcsStarted));

    {
        let log = log.borrow();
        let kinds = log.kinds(0);
        assert_eq!(
            kinds[..6],
            [0x54, 0x55, 0x57, 0x12, 0x13, 0x14],
            "handshake, SOC2 boot"
        );
        assert_eq!(
            kinds[6], 0x23,
            "DSPController ctor tail: internal MIDI clock on"
        );
        assert_eq!(kinds[7], 0x4c, "dynamic delay compensation from settings");
        // Deploy: mute (cores 0, 1), 20 ms, start, sync, value push, unmute.
        let mute = log
            .requests
            .iter()
            .position(|r| r.kind() == 0x20 && r.word() == Some(1))
            .unwrap();
        let start = log.requests.iter().position(|r| r.kind() == 0x1c).unwrap();
        let unmute = log
            .requests
            .iter()
            .rposition(|r| r.kind() == 0x20 && r.word() == Some(0))
            .unwrap();
        assert!(mute < start && start < unmute);
        assert!(log.requests[start].at - log.requests[mute].at >= Duration::from_millis(20));
        assert_eq!(
            log.requests[start].ring,
            CoreId::Dsp(DspCore::Core1),
            "I/O hub core"
        );
        assert!(kinds[start..unmute].contains(&0x1d), "bulk parameter push");
        assert_eq!(log.splash, [30, 40, 50, 60, 70, 80, 90, 100]);
        assert!(log.midi.is_empty(), "no MIDI output during boot");

        // After the first deploy's unmute and before splash 70: the tempo `0x16`, the
        // metronome resend (`0x44`/`0x46`, `0x512c74`), the tuner engine `0x36` (4, then 0),
        // the tuner input path `0x34` and the lane mutes (`0x5b3924`). The first LED frame
        // goes out between splash 90 and 100.
        let at = |wanted: Mark| {
            let found = log.trace.iter().position(|&mark| mark == wanted);
            found.unwrap_or_else(|| panic!("{wanted:x?} in {:x?}", log.trace))
        };
        let boot = &log.trace[at(Mark::Splash(60))..at(Mark::Splash(70))];
        let kinds: Vec<u8> = boot
            .iter()
            .filter_map(|mark| match mark {
                Mark::Dsp(kind, _) => Some(*kind),
                _ => None,
            })
            .collect();
        let first = |kind: u8| kinds.iter().position(|&k| k == kind).unwrap();
        let unmuted = boot
            .iter()
            .rposition(|&mark| mark == Mark::Dsp(0x20, Some(0)))
            .unwrap();
        let unmuted = boot[..unmuted]
            .iter()
            .filter(|mark| matches!(mark, Mark::Dsp(..)))
            .count();
        let tempo = first(0x16);
        let tuner = first(0x36);
        let path = first(0x34);
        assert!(unmuted < tempo && tempo < tuner, "{kinds:x?}");
        assert!(kinds[tempo..tuner].contains(&0x46), "resend: {kinds:x?}");
        assert_eq!(kinds[tuner..path], [0x36, 0x36], "{kinds:x?}");
        assert!(kinds[path..].contains(&0x32), "lane mutes: {kinds:x?}");
        let led = log
            .trace
            .iter()
            .position(|mark| matches!(mark, Mark::Hal(HalCommand::ElementLed(_))))
            .unwrap();
        assert!(at(Mark::Splash(90)) < led && led < at(Mark::Splash(100)));
    }
    let state = device.state();
    let current = state.current.as_ref().expect("first preset deployed");
    // The block's relocated code landed at the start of the code pool of the core the plan
    // placed it on.
    let light = Site::Slot(EngineSlot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    });
    let core = current
        .deployment
        .plan()
        .placement(Node::Model(light))
        .expect("the block is deployed")
        .core;
    let window = core_windows
        .get(core.index())
        .expect("grid blocks run on cores 0 and 1");
    assert_eq!(window.0.borrow()[..4], CODE);
    assert_eq!(
        current.path.as_deref(),
        Some("/media/p4/Presets/My Presets/Light.pb")
    );
    assert!(
        state.library.as_ref().unwrap().database.is_some(),
        "zenui.db maintained"
    );
    // The UI bridge presents full 800×480 RGB8 frames once the device is up.
    {
        let log = log.borrow();
        assert!(!log.frames.is_empty(), "the UI presented a frame");
        assert!(log.frames.iter().all(|&len| len == 800 * 480 * 3));
    }

    let ready = device.platform().clock.now();
    while !device.state().splash_dismissed {
        assert!(device.step().is_none());
    }
    {
        let log = log.borrow();
        assert_eq!(log.splash.last(), Some(&SplashCommand::DISMISS.value()));
        assert!(device.platform().clock.now() >= ready + Duration::from_secs(1));
        assert!(
            log.settings_writes.contains(&SettingsFileId::Zenjack),
            "debounced flush"
        );
    }

    // B13 boot 761: the stock QWS server paints the splash area black when `zensplash` exits,
    // so once it is gone (two polls) the whole frame is written again.
    let frames = log.borrow().frames.len();
    let dismissed = device.platform().clock.now();
    while log.borrow().display_forgets == 0 {
        assert!(device.step().is_none());
    }
    assert!(device.platform().clock.now() >= dismissed + 2 * SPLASH_POLL);
    assert_eq!(log.borrow().frames.len(), frames + 1, "one full frame");

    // A tap on the grid block selects it (tetrad-ui `InspectCell`) and presents one new frame.
    let block = GridPosition::new(0, 2).unwrap();
    let at = target(&device.bridges().ui, Control::SelectCell(block));
    let frames = log.borrow().frames.len();
    for pressed in [true, false] {
        let sample = TouchSample { pressed, ..at };
        drive(&mut device, Event::UiInput(UiInput::Touch(sample)));
    }
    assert_eq!(
        device.bridges().ui.view().snapshot().selected_block,
        Some(block)
    );
    assert!(log.borrow().frames.len() > frames, "the selection is drawn");

    // Scene mode, then footswitch B: the reducer selects scene 2 and the DSP gets the stock
    // recall push, from the bulk parameter change (`0x1d`) to the bypass sync (`0x3e`).
    drive(
        &mut device,
        Event::Action(Action::SetMode(FootswitchMode::Scene)),
    );
    let recall = log.borrow().requests.len();
    let switch_b = Element::all().nth(1).unwrap();
    for value in [1, 0] {
        drive(
            &mut device,
            Event::Hal(HalEvent::Stomp {
                element: switch_b,
                value,
            }),
        );
    }
    let current = device.state().current.as_ref().unwrap();
    assert_eq!(current.deployment.scene().index(), 1);
    {
        let log = log.borrow();
        let kinds = log.kinds(recall);
        assert!(kinds.contains(&0x1d) && kinds.contains(&0x3e), "{kinds:x?}");
    }

    // Power-off is SHUTDOWN `0x6a87d4`: the mute `0x876470` (`0x20` (0,0) to cores 0 and 1),
    // headphones 0, then the teardown `0x66a208`. Nothing (no Wi-Fi step) comes first.
    let before = log.borrow().trace.len();
    device.power(PowerAction::PowerOff).unwrap();
    let log = log.borrow();
    let headphones = Setting::new(
        JackControl::HeadphoneVolume,
        Value::HeadphoneVolume(HeadphoneVolume::Muted),
    )
    .unwrap();
    // libzenhal `0x1b158` with (0.0, mute, no verify): word 100, no read-back.
    let mut expected = vec![
        Mark::Dsp(0x20, Some(0)),
        Mark::Dsp(0x20, Some(0)),
        Mark::Hal(HalCommand::Set(
            headphones,
            tetrad_hal::analog::Verification::Unverified,
        )),
    ];
    expected.extend(
        [
            PowerStep::StopLogPipelines,
            PowerStep::StopLogFollower,
            PowerStep::Sync,
            PowerStep::Unmount("/media/p4"),
            PowerStep::Sync,
            PowerStep::CheckFilesystem("/dev/mmcblk0p4"),
            PowerStep::Sync,
            PowerStep::HalShutdown,
        ]
        .map(Mark::Power),
    );
    assert_eq!(log.trace[before..], expected);
}

/// The DSPController ctor tail (`0x877a84`) sends `0x23` EnableInternalMidiClock 1 to core 4
/// once at boot, before the dynamic delay compensation (`0x4c`).
#[test]
fn boot_enables_the_internal_midi_clock_once_on_core_4() {
    let media = media();
    let log = Rc::new(RefCell::new(Log::default()));
    let sim = Sim::new(STOCK, &media, &log);
    let _device = Device::start(sim).unwrap();
    let log = log.borrow();
    let clocks: Vec<_> = (log.requests.iter())
        .filter(|sent| sent.kind() == 0x23)
        .collect();
    assert_eq!(clocks.len(), 1, "one enable per boot");
    assert_eq!(clocks[0].ring, CoreId::Control);
    assert_eq!(clocks[0].word(), Some(1));
    let clock = log.requests.iter().position(|sent| sent.kind() == 0x23);
    let delay = log.requests.iter().position(|sent| sent.kind() == 0x4c);
    assert!(clock.unwrap() < delay.unwrap());
}

/// Review w5 R10 (`0x5122c0`, `+0x88d0`): a preset load while a capture preset runs stores
/// the preset, `LastPreset` and the footswitch index but sends nothing to the DSP; the
/// capture revert then plays that preset (`0x527d80(user, 1, 0)`).
#[test]
fn preset_load_during_capture_is_stored_and_the_revert_plays_it() {
    let (mut device, log, _media) = common::boot(media());
    while !device.state().splash_dismissed {
        assert!(device.step().is_none());
    }
    let mut swap = Swap::default();
    run_entry(
        &[EntryAction::LoadEmpty],
        Hardware::QuadCortex,
        &mut swap,
        &mut device,
    )
    .unwrap();
    assert!(device.bridges().capture.capture_running());
    // The scripted DSP logs a request when it consumes it, one pass later.
    assert!(device.step().is_none());

    let before = log.borrow().requests.len();
    let second = PresetRef {
        setlist: None,
        index: 1,
    };
    drive(
        &mut device,
        Event::Action(Action::LoadPreset {
            target: second,
            reload: true,
        }),
    );
    assert_eq!(log.borrow().kinds(before), [0u8; 0], "no DSP push");
    let state = device.state();
    assert_eq!(
        state.current.as_ref().unwrap().path.as_deref(),
        Some("/media/p4/Presets/My Presets/Second.pb")
    );
    assert_eq!(state.settings.settings().last_preset.preset_index, 1);
    assert_eq!(state.performance.footswitch.current_preset, 1);

    let before = log.borrow().requests.len();
    run_entry(
        &[EntryAction::RevertNormal],
        Hardware::QuadCortex,
        &mut swap,
        &mut device,
    )
    .unwrap();
    assert!(!device.bridges().capture.capture_running());
    assert!(device.step().is_none());
    let log = log.borrow();
    let starts: Vec<_> = log.requests[before..]
        .iter()
        .filter(|request| request.kind() == 0x1c)
        .collect();
    assert!(!starts.is_empty(), "the revert starts the stored preset");
}

/// Review r15 R6: a preset load ends Copy/Swap Scene mode (`0x5b2f40` → `0x5898f4(0)`), so
/// the next scene footswitch in the new preset is a plain scene select.
#[test]
fn preset_load_ends_scene_copy_mode() {
    use tetrad_app::device::actions::SceneClipboard;
    let (mut device, _log, _media) = common::boot(media());
    while !device.state().splash_dismissed {
        assert!(device.step().is_none());
    }
    let clipboard = SceneClipboard {
        source: None,
        swap: false,
    };
    drive(
        &mut device,
        Event::Action(Action::SceneCopyMode(Some(clipboard))),
    );
    assert!(device.state().performance.footswitch.scene_copy);
    let target = PresetRef {
        setlist: None,
        index: 1,
    };
    drive(
        &mut device,
        Event::Action(Action::LoadPreset {
            target,
            reload: false,
        }),
    );
    let state = device.state();
    assert_eq!(state.performance.footswitch.current_preset, 1);
    assert_eq!(state.perform.scene_clipboard, None);
    assert!(!state.performance.footswitch.scene_copy);
}

/// Review r14 P-1 and P-10 (r15 R18): a save commits every block's bypass (`0x5b7438` →
/// `0x51327c`), so a later scene change keeps a stomp state that was not committed before
/// the save; the clean frame is the saved copy, so a momentary stomp held through the save
/// shows clean after its release.
#[test]
fn save_commits_the_bypass_and_keeps_the_saved_copy_clean() {
    use tetrad_app::device::actions::Action;
    use tetrad_app::device::platform::TouchSample;
    use tetrad_engine::SceneIndex;
    use tetrad_preset::{Model, ParamValue, StompAssignment};

    let specs = STOCK.specs();
    let catalog = tetrad_app::device::preset::SpecsCatalog(&specs);
    let mut stored = light_preset(&specs, 2);
    let block = Model::block(1, [ParamValue::Float(0.5), ParamValue::Float(0.0)]);
    let at = tetrad_preset::Slot { row: 0, column: 3 };
    stored.insert_block(at, block, &catalog).unwrap();
    for (column, stomp_index) in [(2, 0), (3, 1)] {
        stored.stomp_mode_assignments.push(StompAssignment {
            row: 0,
            column,
            stomp_index,
            ..StompAssignment::default()
        });
    }
    stored.stomp_is_momentary.insert(1, true);
    stored.name = Some("Light".into());
    let (mut device, _log, _media) = common::boot(Media::new([("Light", stored)]));
    while !device.state().splash_dismissed {
        assert!(device.step().is_none());
    }
    device.platform().wall_clock.set(Some(
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_760_000_000),
    ));
    drive(
        &mut device,
        Event::Action(Action::ChangeSetting {
            id: tetrad_ui::SettingId::SceneBypassBehavior,
            value: tetrad_ui::SettingValue::Choice(1),
        }),
    );
    drive(
        &mut device,
        Event::Action(Action::SetMode(FootswitchMode::Stomp)),
    );
    let mut switches = Element::all();
    let (a, b) = (switches.next().unwrap(), switches.next().unwrap());
    let stomp = |device: &mut Device<Sim>, element, value| {
        drive(device, Event::Hal(HalEvent::Stomp { element, value }));
    };
    let bypassed = |device: &Device<Sim>, column| {
        let current = device.state().current.as_ref().unwrap();
        let site = Site::Slot(EngineSlot {
            row: Row::new(0).unwrap(),
            column: Column::new(column).unwrap(),
        });
        let block = current.deployment.layout().instance(site).unwrap();
        block.bypass.at(current.deployment.scene())
    };
    stomp(&mut device, a, 1);
    stomp(&mut device, a, 0);
    assert!(bypassed(&device, 2));
    stomp(&mut device, b, 1);
    let save = common::target(&device.bridges().ui, Control::SavePreset);
    for pressed in [true, false] {
        let touch = TouchSample { pressed, ..save };
        drive(&mut device, Event::UiInput(UiInput::Touch(touch)));
    }
    stomp(&mut device, b, 0);
    let state = device.state();
    let (clean, current) = (state.clean.as_ref(), state.current.as_ref());
    assert!(clean.unwrap().matches(current.unwrap()), "P-10");
    let scene = SceneIndex::new(1).unwrap();
    drive(&mut device, Event::Action(Action::SelectScene(scene)));
    assert!(bypassed(&device, 2), "P-1");
}

/// Review r15 R18 and R15: the device copies `UiState::assign_mode` to
/// `State::footswitch_dialog` before a HAL footswitch and before a MIDI CC#35..=42 footswitch
/// (`Device::reduce`, `Effect::MidiFootswitch`). With "Assign footswitch" open, neither
/// toggles the stomp's block (`0x5898f4(1)`, `0x523f80`).
#[test]
fn assign_dialog_blocks_hal_and_midi_footswitches() {
    use tetrad_app::device::actions::Action;
    use tetrad_app::device::platform::TouchSample;
    use tetrad_preset::StompAssignment;
    use tetrad_ui::ParamControl;

    let specs = STOCK.specs();
    let mut stored = light_preset(&specs, 2);
    stored.stomp_mode_assignments.push(StompAssignment {
        row: 0,
        column: 2,
        stomp_index: 0,
        ..StompAssignment::default()
    });
    let (mut device, _log, _media) = common::boot(Media::new([("Light", stored)]));
    while !device.state().splash_dismissed {
        assert!(device.step().is_none());
    }
    drive(
        &mut device,
        Event::Action(Action::SetMode(FootswitchMode::Stomp)),
    );
    let open_dialog = |device: &mut Device<Sim>| {
        let block = GridPosition::new(0, 2).unwrap();
        let stomp = Control::Param(ParamControl::Stomp);
        if !device
            .bridges()
            .ui
            .view()
            .hit_regions()
            .iter()
            .any(|r| r.control == stomp)
        {
            let at = target(&device.bridges().ui, Control::SelectCell(block));
            for pressed in [true, false] {
                let touch = TouchSample { pressed, ..at };
                drive(device, Event::UiInput(UiInput::Touch(touch)));
            }
        }
        let at = target(&device.bridges().ui, stomp);
        for pressed in [true, false] {
            let touch = TouchSample { pressed, ..at };
            drive(device, Event::UiInput(UiInput::Touch(touch)));
        }
        assert!(device.bridges().ui.view().assign_mode());
    };
    open_dialog(&mut device);
    let bypassed = |device: &Device<Sim>| {
        let current = device.state().current.as_ref().unwrap();
        let site = Site::Slot(EngineSlot {
            row: Row::new(0).unwrap(),
            column: Column::new(2).unwrap(),
        });
        let block = current.deployment.layout().instance(site).unwrap();
        block.bypass.at(current.deployment.scene())
    };
    let before = bypassed(&device);
    // CC#35 (footswitch A) on channel 1, before any HAL event refreshes the flag.
    let at = device.platform().clock.now();
    for byte in [0xb0, 35, 127] {
        device.push(Event::Midi { byte, at });
    }
    (0..3).for_each(|_| _ = device.step());
    assert_eq!(bypassed(&device), before, "MIDI footswitch");
    // The press went to the dialog: it is the assignment, so the dialog closed.
    assert!(!device.bridges().ui.view().assign_mode());
    open_dialog(&mut device);
    let element = Element::all().next().unwrap();
    for value in [1, 0] {
        drive(&mut device, Event::Hal(HalEvent::Stomp { element, value }));
    }
    assert_eq!(bypassed(&device), before, "HAL footswitch");
}

/// Factory presets store no name in the file: the sidecar name shows, as stock shows "Brit
/// 2203" (seen on the QC, 2026-10-15).
#[test]
fn a_nameless_factory_preset_takes_its_sidecar_name() {
    use std::fs;
    use tetrad_app::device::FACTORY_SETLIST;
    use tetrad_storage::media::{SETLIST_SLOTS, encode_setlist};

    let media = media();
    let factory = media.0.join("factory/Factory Library");
    fs::create_dir_all(&factory).unwrap();
    let mut preset = light_preset(&STOCK.specs(), 3);
    preset.name = None;
    fs::write(factory.join("Brit.pb"), preset.to_bytes()).unwrap();
    fs::write(factory.join("Brit.json"), r#"{"name":"Brit 2203"}"#).unwrap();
    let mut slots = vec![None; SETLIST_SLOTS];
    slots[0] = Some(format!("{FACTORY_SETLIST}/Brit.pb"));
    fs::write(factory.join("setlist"), encode_setlist(&slots).unwrap()).unwrap();
    let (mut device, _log, _media) = common::boot(media);

    drive(
        &mut device,
        Event::Action(Action::LoadPreset {
            target: PresetRef {
                setlist: Some(FACTORY_SETLIST.into()),
                index: 0,
            },
            reload: true,
        }),
    );
    let current = device.state().current.as_ref().unwrap();
    assert_eq!(current.preset.name.as_deref(), Some("Brit 2203"));
}

/// Factory reset (`0x004de79c`): `zenui.db` is removed and made again at 410.1 with only the
/// user roots (`0x006a551c`, `0x006a576c`); the user files go (`0x00770334`): setlists,
/// favorites, recents and user plugin presets. `My Presets` and one folder per factory plugin
/// come back empty, then Factory Library preset 0 loads (`0x005b37f0`).
#[test]
fn factory_reset_wipes_the_user_library_and_loads_factory_preset_0() {
    use std::fs;
    use tetrad_settings::device::ResetKind;
    use tetrad_storage::media::{SETLIST_SLOTS, encode_setlist};
    use tetrad_storage::stock::{COROS_4_1_SCHEMA, DbLimits, StockDb};

    let media = media();
    let root = media.0.clone();
    let factory = root.join("factory/Factory Library");
    fs::create_dir_all(&factory).unwrap();
    fs::write(
        factory.join("One.pb"),
        light_preset(&STOCK.specs(), 3).to_bytes(),
    )
    .unwrap();
    let mut slots = vec![None; SETLIST_SLOTS];
    slots[0] = Some("/opt/neuraldsp/Factory Library/One.pb".to_owned());
    fs::write(factory.join("setlist"), encode_setlist(&slots).unwrap()).unwrap();
    fs::create_dir_all(root.join("factory/Plugins/Amp")).unwrap();
    fs::create_dir_all(root.join("Plugins/Amp/Mine")).unwrap();
    fs::write(root.join("Plugins/Amp/Mine/Tone.pb"), b"").unwrap();
    fs::write(root.join("recents-nc"), "local_nc_root\nkey\n").unwrap();
    rusqlite::Connection::open(root.join("zenui.db"))
        .unwrap()
        .execute("INSERT INTO pinned_models VALUES ('7')", [])
        .unwrap();
    let (mut device, log, _media) = common::boot(media);
    let notices = log.borrow().notices.len();

    drive(
        &mut device,
        Event::Action(Action::Reset(ResetKind::Factory)),
    );
    let current = device.state().current.as_ref().unwrap();
    assert_eq!(
        current.path.as_deref(),
        Some("/opt/neuraldsp/Factory Library/One.pb")
    );
    let entries = |dir: &str| fs::read_dir(root.join(dir)).unwrap().count();
    assert_eq!(entries("Presets"), 1);
    assert_eq!(entries("Presets/My Presets"), 0);
    assert_eq!(entries("Plugins/Amp"), 0);
    for file in ["user_preset_folders", "recents-nc", "favorites"] {
        assert!(!root.join(file).exists(), "{file}");
    }
    // `0x005b37f0` is the load path `0x005b2f40`: it records the factory preset as a recent.
    assert_eq!(
        fs::read_to_string(root.join("recents")).unwrap(),
        "/opt/neuraldsp/Factory Library\n/opt/neuraldsp/Factory Library/One.pb\n"
    );
    let database = StockDb::open(&root.join("zenui.db"), DbLimits::default(), true).unwrap();
    assert_eq!(database.version(), COROS_4_1_SCHEMA);
    assert!(database.view().pinned_models().unwrap().is_empty());
    assert_eq!(database.view().folders().unwrap().len(), 2);
    let log = log.borrow();
    let library = (log.notices[notices..].iter())
        .filter(|notice| notice.subsystem == tetrad_app::device::Subsystem::Library)
        .collect::<Vec<_>>();
    assert!(library.is_empty(), "{library:?}");
    // `0x005b37f0` calls `0x5b2f40` directly: no load frame of the wiped preset (review r39
    // K13, C).
    assert_eq!(device.state().history.frame_types(), (vec![], vec![]));
}

// --- r36-core characterization: preset load and Control recall (health-r34 §4) -----------
//
// These pin the load and recall paths before the round-36 helper split (`push_prepared`,
// `finish_load`). The DSP push lists are the observed order at round 35.

/// The DSP push of a one-block preset after the on-load MIDI Out: the deploy (run-list
/// clear `0x1e` … unmute `0x20`), then the metronome parameter `0x44`, the tempo `0x16` and
/// the metronome destination `0x46` (`0x527d80` → `0x875a48`, `0x512c74`).
const DEPLOY: &str = "19 1e 1e 1c 1c 0a 0a 0a 0a 30 3b 03 03 03 03 03 03 03 30 03 03 \
    03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 03 \
    03 03 03 03 03 03 03 2f 32 31 31 31 03 03 03 03 2f 03 03 32 31 31 31 03 03 2f 03 03 32 \
    31 31 31 03 03 2f 03 03 32 31 31 31 34 32 32 32 32 1d 1d 3d 0a 0a 0a 0a 03 03 03 03 1f \
    3e 1f 20 1f 1f 20 44 16 46";

/// Slot 1 (`Second`) sends Program Change 9 on channel 1 when it loads.
fn midi_media() -> Media {
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
    Media::new([("Light", light_preset(&specs, 2)), ("Second", second)])
}

/// DSP requests from `mark` on as (type, word at `+0x10`), without the Control link frames
/// (`0x27`).
fn pushes(log: &Log, mark: usize) -> Vec<(u8, Option<u32>)> {
    (log.requests[mark..].iter())
        .filter(|sent| sent.kind() != 0x27)
        .map(|sent| (sent.kind(), sent.word()))
        .collect()
}

/// A Cortex Control RecallPreset UPDATE of `preset` with request ID 9.
fn recall(preset: &tetrad_preset::Preset) -> tetrad_control::codecs::RecallPresetMessage {
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

fn kinds(pushes: &[(u8, Option<u32>)]) -> String {
    let kinds: Vec<_> = pushes
        .iter()
        .map(|(kind, _)| format!("{kind:02x}"))
        .collect();
    kinds.join(" ")
}

fn booted(media: Media) -> (Device<Sim>, Rc<RefCell<Log>>, Media) {
    let (mut device, log, media) = common::boot(media);
    while !device.state().splash_dismissed {
        assert!(device.step().is_none());
    }
    (device, log, media)
}

/// A preset load (`0x5b2f40`): the on-load MIDI Out (DSP `0x18`) goes before the deploy and
/// the tempo and metronome follow it. The load keeps the undo history and pushes a load frame
/// (type 5, re-r38 §4), makes the loaded preset clean, ends Copy/Swap Scene mode and writes
/// `LastPreset` and the footswitch index.
#[test]
fn preset_load_pushes_in_order_and_resets_the_working_state() {
    use tetrad_app::device::actions::SceneClipboard;
    let (mut device, log, _media) = booted(midi_media());
    common::connect_control(&mut device);
    common::host_send(&mut device, recall(&light_preset(&STOCK.specs(), 5)));
    assert_eq!(device.state().history.depths(), (1, 0));
    let clipboard = SceneClipboard {
        source: None,
        swap: false,
    };
    drive(
        &mut device,
        Event::Action(Action::SceneCopyMode(Some(clipboard))),
    );
    // Review r36 H7: Copy/Swap Scene is on before the load ends it.
    assert!(device.state().performance.footswitch.scene_copy);
    assert_eq!(device.state().perform.scene_clipboard, Some(clipboard));

    let (mark, midi) = (log.borrow().requests.len(), log.borrow().midi.len());
    drive(
        &mut device,
        Event::Action(Action::LoadPreset {
            target: PresetRef {
                setlist: None,
                index: 1,
            },
            reload: true,
        }),
    );
    (0..50).for_each(|_| _ = device.step());
    let log = log.borrow();
    assert_eq!(kinds(&pushes(&log, mark)), format!("20 18 20 {DEPLOY}"));
    assert_eq!(log.midi[midi..], [vec![0xc0, 9]]);
    let state = device.state();
    let current = state.current.as_ref().unwrap();
    assert_eq!(
        current.path.as_deref(),
        Some("/media/p4/Presets/My Presets/Second.pb")
    );
    assert_eq!(state.history.frame_types(), (vec![0, 5], vec![]));
    assert!(state.clean.as_ref().unwrap().matches(current));
    assert!(!state.performance.footswitch.scene_copy);
    assert_eq!(state.perform.scene_clipboard, None);
    let last = &state.settings.settings().last_preset;
    assert_eq!((last.setlist.as_deref(), last.preset_index), (None, 1));
    assert_eq!(state.performance.footswitch.current_preset, 1);
}

/// Re-r38 §4: UNDO of a load (type 5, `0x649330` → `0x5b2f40`) loads the old slot again with
/// the working preset it had, so its unsaved edits return and show as unsaved; the preset
/// frames before it stay (C). REDO loads the newer slot again. O: when the old slot names
/// another file since, nothing loads, a notice says so and the frame drops, and the old
/// preset's frames under it drop with it (review r39 K2): no later UNDO puts them into the
/// loaded preset.
#[test]
fn undo_after_a_load_reloads_the_old_preset_with_its_edits() {
    let (mut device, _log, _media) = booted(midi_media());
    common::connect_control(&mut device);
    let light = "/media/p4/Presets/My Presets/Light.pb";
    common::host_send(&mut device, recall(&light_preset(&STOCK.specs(), 5)));
    let edited = device.state().current.as_ref().unwrap().preset.clone();
    assert!(device.state().preset_dirty());
    let load = |index| {
        Event::Action(Action::LoadPreset {
            target: PresetRef {
                setlist: None,
                index,
            },
            reload: true,
        })
    };
    drive(&mut device, load(1));
    assert_eq!(device.state().history.frame_types(), (vec![0, 5], vec![]));

    drive(&mut device, Event::Action(Action::Undo));
    let state = device.state();
    let current = state.current.as_ref().unwrap();
    assert_eq!(current.path.as_deref(), Some(light));
    assert_eq!(current.preset, edited);
    assert!(state.preset_dirty(), "the edits show as unsaved");
    assert_eq!(state.settings.settings().last_preset.preset_index, 0);
    assert_eq!(state.history.frame_types(), (vec![0], vec![5]));

    drive(&mut device, Event::Action(Action::Redo));
    let state = device.state();
    let second = state.current.as_ref().unwrap().path.clone();
    assert_eq!(
        second.as_deref(),
        Some("/media/p4/Presets/My Presets/Second.pb")
    );
    assert!(!state.preset_dirty());
    assert_eq!(state.history.frame_types(), (vec![0, 5], vec![]));

    // A rename gives slot 0 another file and pushes no frame: the load frame is stale.
    let rename = Action::RenamePreset {
        setlist: "My Presets".into(),
        slot: 0,
        name: "Renamed".into(),
    };
    drive(&mut device, Event::Action(rename));
    drive(&mut device, Event::Action(Action::Undo));
    let state = device.state();
    let current = state.current.as_ref().unwrap();
    assert_eq!(current.path, second);
    assert_eq!(state.history.frame_types(), (vec![], vec![]));
    assert!(
        (state.notices.iter()).any(|notice| notice.detail.contains("load undo refused")),
        "{:?}",
        state.notices.iter().collect::<Vec<_>>()
    );
    let loaded = current.preset.clone();
    drive(&mut device, Event::Action(Action::Undo));
    assert_eq!(device.state().current.as_ref().unwrap().preset, loaded);
}

/// Review r39 K2 on the redo stack: a refused REDO of a load frame (its slot names another
/// file since) drops the frames of that preset under it, so the next REDO does not put them
/// into the loaded preset.
#[test]
fn a_refused_load_redo_drops_the_frames_of_its_preset() {
    let (mut device, _log, _media) = booted(midi_media());
    common::connect_control(&mut device);
    let load = Action::LoadPreset {
        target: PresetRef {
            setlist: None,
            index: 1,
        },
        reload: true,
    };
    drive(&mut device, Event::Action(load));
    common::host_send(&mut device, recall(&light_preset(&STOCK.specs(), 5)));
    drive(&mut device, Event::Action(Action::Undo));
    drive(&mut device, Event::Action(Action::Undo));
    let state = device.state();
    assert_eq!(state.history.frame_types(), (vec![], vec![0, 5]));
    let current = state.current.as_ref().unwrap();
    let light = (current.path.clone(), current.preset.clone());

    let rename = Action::RenamePreset {
        setlist: "My Presets".into(),
        slot: 1,
        name: "Renamed".into(),
    };
    drive(&mut device, Event::Action(rename));
    drive(&mut device, Event::Action(Action::Redo));
    assert_eq!(device.state().history.frame_types(), (vec![], vec![]));
    drive(&mut device, Event::Action(Action::Redo));
    let current = device.state().current.as_ref().unwrap();
    assert_eq!((current.path.clone(), current.preset.clone()), light);
}

/// Review r39 K4: the load frame of a swap reload. SWAP moves the clean loaded "Light" from
/// slot 0 to slot 1 and slot 0 reloads with "Second" (`0x971180`). The frame names the slot
/// that holds Light's file now, so the UNDO loads Light from slot 1: `LastPreset` and the
/// footswitch index follow it and do not stay on slot 0, which holds Second.
#[test]
fn a_swap_reload_undo_loads_the_old_preset_from_its_new_slot() {
    let (mut device, _log, _media) = booted(midi_media());
    let swap = Action::SwapPresets {
        from: "My Presets".into(),
        slot: 0,
        to: "My Presets".into(),
        to_slot: 1,
    };
    drive(&mut device, Event::Action(swap));
    let state = device.state();
    assert_eq!(
        state.current.as_ref().unwrap().path.as_deref(),
        Some("/media/p4/Presets/My Presets/Second.pb")
    );
    assert_eq!(state.history.frame_types(), (vec![2, 5], vec![]));

    drive(&mut device, Event::Action(Action::Undo));
    let state = device.state();
    assert_eq!(
        state.current.as_ref().unwrap().path.as_deref(),
        Some("/media/p4/Presets/My Presets/Light.pb")
    );
    let last = &state.settings.settings().last_preset;
    assert_eq!((last.setlist.as_deref(), last.preset_index), (None, 1));
    assert_eq!(state.performance.footswitch.current_preset, 1);
    assert_eq!(state.history.frame_types(), (vec![2], vec![5]));
}

/// "Light" (slot 0) and "Second" (slot 1) with a stored name, so library frames read them.
fn named_media() -> Media {
    let specs = STOCK.specs();
    let named = |name: &'static str, column| {
        let mut preset = light_preset(&specs, column);
        preset.name = Some(name.into());
        (name, preset)
    };
    Media::new([named("Light", 2), named("Second", 3)])
}

/// Review r39 K4 between setlists: SWAP moves the clean loaded "Light" into setlist "B" (a new
/// file there) and the old slot reloads with the other preset. No slot of the `LastPreset`
/// setlist names Light's file, so the load frame takes the slot of "B" that does; the UNDO
/// loads Light there and `LastPreset` follows it.
#[test]
fn a_swap_reload_between_setlists_undo_loads_from_the_other_setlist() {
    let (mut device, _log, _media) = booted(named_media());
    drive(
        &mut device,
        Event::Action(Action::CreateSetlist("B".into())),
    );
    let copy = Action::CopyPresetTo {
        from: "My Presets".into(),
        slot: 1,
        to: "B".into(),
        to_slot: Some(0),
        remove: false,
    };
    drive(&mut device, Event::Action(copy));
    let swap = Action::SwapPresets {
        from: "My Presets".into(),
        slot: 0,
        to: "B".into(),
        to_slot: 0,
    };
    drive(&mut device, Event::Action(swap));
    let state = device.state();
    assert_eq!(state.history.frame_types(), (vec![3, 2, 5], vec![]));
    let presets = &state.library.as_ref().unwrap().presets;
    let b = (presets.setlists().unwrap().into_iter())
        .find(|setlist| setlist.name == "B")
        .unwrap()
        .path;
    let light = presets.slots("B").unwrap()[0].clone();
    assert!(
        light
            .as_deref()
            .is_some_and(|light| light.ends_with("/B/Light.pb"))
    );

    drive(&mut device, Event::Action(Action::Undo));
    let state = device.state();
    assert_eq!(state.current.as_ref().unwrap().path, light);
    let last = &state.settings.settings().last_preset;
    assert_eq!((last.setlist.clone(), last.preset_index), (Some(b), 0));
    assert_eq!(state.history.frame_types(), (vec![3, 2], vec![5]));
}

/// "Light" (slot 0, loaded) and "Second" (slot 1), both with a stored name. Light is deleted,
/// so the new-preset template plays in slot 0 with no file (`0x5b2c54`); `edit` then runs on
/// it, and the empty slot 5 loads: frames `[1, 5]`.
fn deleted_then_empty_slot(edit: Option<Action>) -> (Device<Sim>, Media) {
    let (mut device, _log, media) = booted(named_media());
    let delete = Action::DeletePreset {
        setlist: "My Presets".into(),
        slot: 0,
    };
    drive(&mut device, Event::Action(delete));
    assert_eq!(device.state().current.as_ref().unwrap().path, None);
    if let Some(edit) = edit {
        drive(&mut device, Event::Action(edit));
    }
    let load = Action::LoadPreset {
        target: PresetRef {
            setlist: None,
            index: 5,
        },
        reload: true,
    };
    drive(&mut device, Event::Action(load));
    assert_eq!(device.state().history.frame_types(), (vec![1, 5], vec![]));
    (device, media)
}

/// The file slot 0 names.
fn slot_0(device: &Device<Sim>) -> Option<String> {
    let presets = &device.state().library.as_ref().unwrap().presets;
    presets.slots("My Presets").unwrap()[0].clone()
}

/// Review r39 K1: a restored file goes back only to the preset that was deleted, never to
/// another preset that has no file. Here the load UNDO back to Light fails on the DSP (`0x20`
/// is never read), so its frame drops and the empty-slot preset stays loaded. The delete UNDO
/// brings Light's file back; the loaded preset keeps no file, so no SAVE can write it there.
#[test]
fn a_restored_file_goes_back_only_to_the_deleted_preset() {
    let (mut device, _media) = deleted_then_empty_slot(None);
    let hold = &device.platform().clock.hold;
    hold.set(Some(|_, frame| frame[0] == 0x20));
    drive(&mut device, Event::Action(Action::Undo));
    device.platform().clock.hold.set(None);
    assert_eq!(device.state().history.frame_types(), (vec![1], vec![]));

    drive(&mut device, Event::Action(Action::Undo));
    let light = format!("{}/Light.pb", common::SETLIST);
    assert_eq!(slot_0(&device), Some(light));
    assert_eq!(device.state().current.as_ref().unwrap().path, None);
}

/// Review r39 K7 (O): when the load UNDO loads slot 0 but cannot put the working preset back
/// (the template with an added block; the redeploy's run-list clear `0x1e` is never read), the
/// working preset waits on top of the undo stack as a frame of the loaded preset, so its
/// edits are not lost: the next UNDO puts it back.
#[test]
fn a_load_undo_keeps_a_working_preset_it_cannot_put_back() {
    let add = tetrad_app::device::actions::LayoutEdit::Add {
        slot: EngineSlot {
            row: Row::new(0).unwrap(),
            column: Column::new(2).unwrap(),
        },
        model: 1,
    };
    let (mut device, _media) = deleted_then_empty_slot(Some(Action::EditLayout(add)));
    let Some(UndoFrame::PresetLoad(frame)) = device.state().history.newest(false).cloned() else {
        panic!("a load frame");
    };
    let hold = &device.platform().clock.hold;
    hold.set(Some(|_, frame| frame[0] == 0x1e));
    drive(&mut device, Event::Action(Action::Undo));
    device.platform().clock.hold.set(None);
    let state = device.state();
    assert_eq!(state.history.frame_types(), (vec![1, 0], vec![5]));
    let kept = UndoFrame::Preset {
        frame: Box::new(frame.working.clone()),
        owner: state.history.loaded(),
    };
    assert_eq!(state.history.newest(false), Some(&kept));
    assert!(!frame.working.matches(state.current.as_ref().unwrap()));
}

/// Review r39 K13 (C): a MIDI program change loads through `0x5b2f40` with no load frame
/// (`0x5b5bd8` case 3); only the `updateUndo` loads push one (`0x5b3810`, `0x5b38c8`). The
/// old preset's frames on top drop: they belong to no loaded preset.
#[test]
fn a_program_change_loads_with_no_load_frame() {
    let (mut device, _log, _media) = booted(midi_media());
    common::connect_control(&mut device);
    common::host_send(&mut device, recall(&light_preset(&STOCK.specs(), 5)));
    assert_eq!(device.state().history.frame_types(), (vec![0], vec![]));
    let program_change = Action::ProgramChange {
        target: PresetRef {
            setlist: None,
            index: 1,
        },
        reload: true,
    };
    drive(&mut device, Event::Action(program_change));
    let state = device.state();
    assert_eq!(
        state.current.as_ref().unwrap().path.as_deref(),
        Some("/media/p4/Presets/My Presets/Second.pb")
    );
    assert_eq!(state.history.frame_types(), (vec![], vec![]));
}

/// A Cortex Control RecallPreset (`0x5b22a8` → `0x527d80`) pushes as a load does, its
/// on-load MIDI Out first, but keeps the file path, the slot, the clean preset and the
/// history, and commits its own undo frame.
#[test]
fn control_recall_pushes_as_a_load_and_keeps_the_slot() {
    let specs = STOCK.specs();
    let (mut device, log, _media) = booted(midi_media());
    common::connect_control(&mut device);
    let clean = device.state().clean.clone();
    let mut recalled = light_preset(&specs, 5);
    recalled.name = Some("Recalled".into());
    recalled.midi_messages = vec![tetrad_preset::MidiMessage {
        kind: 3,
        channel: 1,
        param1: u32::MAX,
        param2: u32::MAX,
        param3: 7,
        ..Default::default()
    }];

    let (mark, midi) = (log.borrow().requests.len(), log.borrow().midi.len());
    common::host_send(&mut device, recall(&recalled));
    let log = log.borrow();
    // The trailing `0x0e` frames are idle meter polls of `host_send`'s steps.
    assert_eq!(
        kinds(&pushes(&log, mark)).trim_end_matches(" 0e"),
        format!("20 18 20 {DEPLOY}")
    );
    assert_eq!(log.midi[midi..], [vec![0xc0, 7]]);
    let state = device.state();
    let current = state.current.as_ref().unwrap();
    assert_eq!(current.preset.name.as_deref(), Some("Recalled"));
    assert_eq!(
        current.path.as_deref(),
        Some("/media/p4/Presets/My Presets/Light.pb")
    );
    assert_eq!(state.history.depths(), (1, 0));
    assert_eq!(*state.clean, clean);
    assert!(!state.clean.as_ref().unwrap().matches(current));
    let last = &state.settings.settings().last_preset;
    assert_eq!((last.setlist.as_deref(), last.preset_index), (None, 0));
    assert_eq!(state.performance.footswitch.current_preset, 0);
}

/// Review r11 N7 for a recall: the on-load MIDI Out goes before the deploy, so it goes out
/// when the deploy fails (the DSP never reads the run-list clear `0x1e`); the live preset
/// stays.
#[test]
fn control_recall_sends_its_on_load_midi_before_a_failed_deploy() {
    let specs = STOCK.specs();
    let (mut device, log, _media) = booted(midi_media());
    common::connect_control(&mut device);
    let live = device.state().current.as_ref().unwrap().preset.to_bytes();
    let mut recalled = light_preset(&specs, 5);
    recalled.midi_messages = vec![tetrad_preset::MidiMessage {
        kind: 3,
        channel: 1,
        param1: u32::MAX,
        param2: u32::MAX,
        param3: 7,
        ..Default::default()
    }];
    device
        .platform()
        .clock
        .hold
        .set(Some(|_, frame| frame[0] == 0x1e));
    let midi = log.borrow().midi.len();
    common::host_send(&mut device, recall(&recalled));
    device.platform().clock.hold.set(None);
    assert_eq!(log.borrow().midi[midi..], [vec![0xc0, 7]]);
    let state = device.state();
    assert_eq!(state.current.as_ref().unwrap().preset.to_bytes(), live);
    assert_eq!(state.history.depths(), (0, 0));
}

/// `0x5122c0` for a recall: while a capture preset runs, a Control recall pushes nothing to
/// the DSP (no deploy, tempo or metronome) but replaces the working preset, which the revert
/// then plays.
#[test]
fn control_recall_during_capture_replaces_the_preset_without_a_push() {
    let (mut device, log, _media) = booted(media());
    common::connect_control(&mut device);
    let mut swap = Swap::default();
    run_entry(
        &[EntryAction::LoadEmpty],
        Hardware::QuadCortex,
        &mut swap,
        &mut device,
    )
    .unwrap();
    assert!(device.bridges().capture.capture_running());
    // Idle steps let the scripted DSP read the capture preset's push.
    (0..50).for_each(|_| _ = device.step());
    let path = device.state().current.as_ref().unwrap().path.clone();
    let mut recalled = light_preset(&STOCK.specs(), 5);
    recalled.name = Some("Recalled".into());

    let mark = log.borrow().requests.len();
    common::host_send(&mut device, recall(&recalled));
    let pushed = pushes(&log.borrow(), mark);
    assert!(
        pushed.iter().all(|&(kind, _)| kind == 0x0e),
        "{}",
        kinds(&pushed)
    );
    let current = device.state().current.as_ref().unwrap();
    assert_eq!(current.preset.name.as_deref(), Some("Recalled"));
    assert_eq!(current.path, path);
}

/// A start-up preset file (`--preset`, `slot` `None`) loads without a slot: `LastPreset` and
/// the footswitch index keep their stored values.
#[test]
fn startup_preset_file_keeps_the_last_preset_slot() {
    use tetrad_app::device::bringup::RunOptions;
    let path = "/media/p4/Presets/My Presets/Second.pb";
    let options = RunOptions {
        preset: Some(path.into()),
        ..RunOptions::STOCK
    };
    let (device, _log, _media) = common::boot_with(media(), options);
    let state = device.state();
    let current = state.current.as_ref().unwrap();
    assert_eq!(current.path.as_deref(), Some(path));
    assert!(state.clean.as_ref().unwrap().matches(current));
    let last = &state.settings.settings().last_preset;
    assert_eq!((last.setlist.as_deref(), last.preset_index), (None, 0));
    assert_eq!(state.performance.footswitch.current_preset, 0);
}

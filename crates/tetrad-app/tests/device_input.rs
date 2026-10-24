//! Input bridge contracts: HAL edges through tetrad-core become device actions, UI input and
//! Preset MIDI Out; raw-input state (holds, chords, knobs, volume, pedals) stays here.

use std::time::Duration;

use tetrad_app::device::input::{Input, POWER_LONG_PRESS, VOLUME_REPEAT};
use tetrad_app::device::midi::Trigger;
use tetrad_app::device::ui::UiInput;
use tetrad_app::device::{Action, DspCommand, Effect, PowerAction, PresetRef, State, Timer};
use tetrad_core::performance::StompIndex;
use tetrad_core::performance::footswitch::BankOverlay;
use tetrad_dsp_protocol::requests::Request;
use tetrad_engine::SceneIndex;
use tetrad_hal::hal::Event as HalEvent;
use tetrad_hal::node::{Element, Pedal};
use tetrad_hal::platform::{Platform as HalPlatform, Product};
use tetrad_hal::zencoder::{VolumeEvent, VolumeKey};
use tetrad_settings::controls::FootswitchMode;
use tetrad_settings::device::{DeviceSettings, Hardware};
use tetrad_settings::{SettingsFileId, SettingsStorage};
use tetrad_ui::{DeviceAction, InputEvent, KnobIndex, Screen, UiCommand as ViewCommand};

const A: u8 = 0;
const B: u8 = 1;
const DOWN: u8 = 4;
const G: u8 = 7;
const H: u8 = 8;
const TEMPO: u8 = 9;
const UP: u8 = 10;
const ZERO: Duration = Duration::ZERO;

struct NoFiles;

impl SettingsStorage for NoFiles {
    fn read(&self, _: SettingsFileId) -> std::io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn write(&mut self, _: SettingsFileId, _: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
    fn remove(&mut self, _: SettingsFileId) -> std::io::Result<()> {
        Ok(())
    }
}

fn state(platform: HalPlatform, mode: FootswitchMode) -> State {
    let hardware = if platform.is_atma() {
        Hardware::Atma
    } else {
        Hardware::QuadCortex
    };
    let (mut settings, _) = DeviceSettings::load(&NoFiles, hardware).unwrap();
    settings.update(|settings| settings.mode.set_mode(mode).unwrap());
    State::new(Product::QuadCortex, platform, settings)
}

fn qc(mode: FootswitchMode) -> State {
    state(HalPlatform::QcOriginal, mode)
}

fn hal(input: &mut Input, state: &mut State, event: HalEvent, now: Duration) -> Vec<Effect> {
    input.on_hal(event, state, now).unwrap()
}

fn stomp(input: &mut Input, state: &mut State, index: u8, value: i32) -> Vec<Effect> {
    let element = Element::all().nth(usize::from(index)).unwrap();
    hal(input, state, HalEvent::Stomp { element, value }, ZERO)
}

fn actions(effects: &[Effect]) -> Vec<&Action> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Action(action) => Some(action),
            _ => None,
        })
        .collect()
}

/// Preset mode: a press loads its slot in the loaded setlist ("a footswitch always reloads");
/// the release does nothing. tetrad-ui still gets the raw edges.
#[test]
fn preset_press_loads_its_slot_and_release_is_quiet() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    let effects = stomp(&mut input, &mut state, A, 1);
    let load = Action::LoadPreset {
        target: PresetRef {
            setlist: None,
            index: 0,
        },
        reload: true,
    };
    assert_eq!(actions(&effects), [&load]);
    assert!(
        effects.contains(&Effect::UiInput(UiInput::View(InputEvent::Footswitch {
            switch: tetrad_ui::Footswitch::A,
            pressed: true,
        })))
    );
    assert!(actions(&stomp(&mut input, &mut state, A, 0)).is_empty());
}

#[test]
fn scene_press_selects_its_scene() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Scene));
    let effects = stomp(&mut input, &mut state, B, 1);
    assert_eq!(
        actions(&effects),
        [&Action::SelectScene(SceneIndex::new(1).unwrap())]
    );
}

/// Stomp mode on the full-size QC: the press toggles and sends the switch's Preset MIDI Out
/// (`0x00332064` gate: no tuner, no overlay); the release may undo a momentary stomp.
#[test]
fn stomp_press_toggles_and_sends_preset_midi_on_press() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Stomp));
    let stomp_a = StompIndex::new(0).unwrap();
    let effects = stomp(&mut input, &mut state, A, 1);
    assert_eq!(actions(&effects), [&Action::ToggleStomp(stomp_a)]);
    assert!(effects.contains(&Effect::PresetMidiOut(Trigger::Footswitch(0))));
    let effects = stomp(&mut input, &mut state, A, 0);
    assert_eq!(actions(&effects), [&Action::ReleaseStomp(stomp_a)]);
    assert!(!effects.contains(&Effect::PresetMidiOut(Trigger::Footswitch(0))));
}

/// DOWN + TEMPO cycles the mode once, cancels the hold, and both releases are quiet.
#[test]
fn down_tempo_chord_cycles_mode_once() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    assert!(actions(&stomp(&mut input, &mut state, DOWN, 1)).is_empty());
    let effects = stomp(&mut input, &mut state, TEMPO, 1);
    assert_eq!(actions(&effects), [&Action::CycleMode]);
    assert!(input.hold_deadline().is_none());
    assert!(actions(&stomp(&mut input, &mut state, DOWN, 0)).is_empty());
    assert!(actions(&stomp(&mut input, &mut state, TEMPO, 0)).is_empty());
}

#[test]
fn tempo_hold_shows_the_tuner_when_due() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    stomp(&mut input, &mut state, TEMPO, 1);
    let due = input.hold_deadline().expect("a TEMPO press starts a hold");
    assert!(
        input
            .on_hold_expired(&mut state, due - Duration::from_millis(1))
            .is_empty()
    );
    let effects = input.on_hold_expired(&mut state, due);
    assert_eq!(actions(&effects), [&Action::ShowTuner(true)]);
}

/// Custom mode: an UP hold opens the tuner, and only while the mode is on. A hold armed in
/// Custom that comes due after the mode ended does nothing, and so does its release.
#[test]
fn a_custom_up_hold_ends_with_the_mode() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Stomp));
    state.performance.footswitch.custom = true;
    stomp(&mut input, &mut state, UP, 1);
    let due = input.hold_deadline().expect("UP arms a hold in Custom");
    let effects = input.on_hold_expired(&mut state, due);
    assert_eq!(actions(&effects), [&Action::ShowTuner(true)]);
    assert!(actions(&stomp(&mut input, &mut state, UP, 0)).is_empty());

    stomp(&mut input, &mut state, UP, 1);
    let due = input.hold_deadline().unwrap();
    state.performance.footswitch.custom = false;
    assert!(input.on_hold_expired(&mut state, due).is_empty());
    assert!(actions(&stomp(&mut input, &mut state, UP, 0)).is_empty());
}

/// Stock gtest `testQCTunerAccess` (`0x43c288`): while the Tuner shows, one TEMPO click
/// closes it; the press is not a tap (`0x20db38` resets the count) and starts no hold.
#[test]
fn tempo_click_closes_the_tuner() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    state.screen = Screen::Tuner;
    let effects = stomp(&mut input, &mut state, TEMPO, 1);
    assert_eq!(actions(&effects), [&Action::ShowTuner(false)]);
    assert_eq!(state.performance.tap.count(), 0);
    assert!(input.hold_deadline().is_none());
    assert!(actions(&stomp(&mut input, &mut state, TEMPO, 0)).is_empty());
}

/// The third TEMPO tap with the popup open applies the last accepted zencoder tempo.
#[test]
fn third_tempo_tap_applies_the_driver_tempo() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    state.screen = Screen::Tempo;
    hal(&mut input, &mut state, HalEvent::Tempo(132), ZERO);
    let mut applied = Vec::new();
    for tap in 0..3 {
        let now = Duration::from_millis(500 * tap);
        let element = Element::all().nth(usize::from(TEMPO)).unwrap();
        let press = HalEvent::Stomp { element, value: 1 };
        applied.extend(hal(&mut input, &mut state, press, now));
        let release = HalEvent::Stomp { element, value: 0 };
        hal(&mut input, &mut state, release, now);
    }
    assert!(actions(&applied).contains(&&Action::SetTempo(132.0)));
}

/// Encoder steps (accelerated, `0x00508e68`) go to the editor knob in cell order A–H,
/// BANK DOWN, TEMPO (logical index); BANK UP has no knob.
#[test]
fn knob_steps_drive_the_editor_knob_of_the_same_letter() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    let mut knob = |index: u8| {
        let event = HalEvent::Knob {
            element: Element::all().nth(usize::from(index)).unwrap(),
            delta: 1,
        };
        hal(&mut input, &mut state, event, ZERO)
    };
    let step = |index| {
        [Effect::UiInput(UiInput::View(InputEvent::Knob {
            knob: KnobIndex::new(index).unwrap(),
            delta: 1,
        }))]
    };
    assert_eq!(knob(A), step(0));
    assert_eq!(knob(G), step(6));
    assert_eq!(knob(DOWN), step(8));
    assert_eq!(knob(TEMPO), step(9));
    // Logical 10 has no editor knob: the BANK UP encoder is its own action.
    assert_eq!(knob(UP), [Effect::Action(Action::BankEncoder(1))]);
}

/// `0x58b968`: a cycle with a hybrid-with-PRESET mode gives banks of four. The preset half
/// (E–H in mode 7) loads `bank·4 + slot`, and BANK UP steps banks of four.
#[test]
fn split_mode_cycle_gives_the_footswitches_banks_of_four() {
    let (mut settings, _) = DeviceSettings::load(&NoFiles, Hardware::QuadCortex).unwrap();
    settings.update(|settings| {
        let modes = vec![FootswitchMode::Preset, FootswitchMode::StompPreset];
        settings.mode.set_available(modes).unwrap();
        settings.mode.set_mode(FootswitchMode::StompPreset).unwrap();
    });
    let mut state = State::new(Product::QuadCortex, HalPlatform::QcOriginal, settings);
    assert_eq!(state.performance.footswitch.bank_size.get(), 4);
    state.performance.footswitch.current_preset = 9;
    let mut input = Input::new();
    let load = Action::LoadPreset {
        target: PresetRef {
            setlist: None,
            index: 11,
        },
        reload: true,
    };
    assert_eq!(actions(&stomp(&mut input, &mut state, H, 1)), [&load]);
    stomp(&mut input, &mut state, H, 0);
    stomp(&mut input, &mut state, UP, 1);
    let overlay = Action::BankOverlay(Some(BankOverlay {
        bank: 3,
        highlighted: None,
    }));
    assert_eq!(actions(&stomp(&mut input, &mut state, UP, 0)), [&overlay]);
}

/// `0x6a86c4`: the mini press follows the QC UI path and starts a 3000 ms shutdown timer.
#[test]
fn power_button_opens_ui_and_mini_hold_shuts_down_unless_released() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    assert_eq!(
        hal(&mut input, &mut state, HalEvent::PowerButton(0), ZERO),
        [Effect::UiInput(UiInput::PowerButton { pressed: true })]
    );
    let mut mini = state_mini();
    assert_eq!(
        hal(&mut input, &mut mini, HalEvent::PowerButton(1), ZERO),
        [
            Effect::UiInput(UiInput::PowerButton { pressed: true }),
            Effect::StartTimer {
                timer: Timer::PowerLongPress,
                after: POWER_LONG_PRESS
            },
        ]
    );
    assert_eq!(POWER_LONG_PRESS, Duration::from_millis(3000));
    assert_eq!(
        hal(&mut input, &mut mini, HalEvent::PowerButton(0), ZERO),
        [
            Effect::UiInput(UiInput::PowerButton { pressed: false }),
            Effect::StopTimer(Timer::PowerLongPress),
        ]
    );
    assert!(input.on_power_timer().is_empty());
    hal(&mut input, &mut mini, HalEvent::PowerButton(1), ZERO);
    assert_eq!(
        input.on_power_timer(),
        [Effect::Power(PowerAction::PowerOff)]
    );
}

fn state_mini() -> State {
    state(HalPlatform::Atma, FootswitchMode::Preset)
}

#[test]
fn volume_knob_uses_calibration_and_stock_deadband() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    let level = HalEvent::Volume(VolumeEvent::Level(242));
    let expected = Effect::Dsp(DspCommand::Request(Request::MasterVolumeChange {
        gain_bits: 1.0f32.to_bits(),
    }));
    assert_eq!(hal(&mut input, &mut state, level, ZERO)[0], expected);
    assert!(hal(&mut input, &mut state, level, ZERO).is_empty());
    // One ADC count is 1/240: quiet until the change reaches the 0.005 deadband.
    assert!(
        hal(
            &mut input,
            &mut state,
            HalEvent::Volume(VolumeEvent::Level(241)),
            ZERO
        )
        .is_empty()
    );
    assert_eq!(
        hal(
            &mut input,
            &mut state,
            HalEvent::Volume(VolumeEvent::Level(240)),
            ZERO
        )
        .len(),
        2,
        "the DSP level and the Control echo"
    );
}

/// The QC knob uses the factory range from the U-Boot environment (`0x1fd8d0` →
/// `0x1fcfb4`) for both the knob path and the boot read; a bad range falls back to 2..=242.
#[test]
fn volume_knob_uses_the_u_boot_factory_range() {
    use std::collections::BTreeMap;
    use tetrad_app::device::actions::master_volume;
    use tetrad_app::device::input::{load_volume_calibration, read_volume};
    use tetrad_hal::zencoder::{VolumeCalibration, VolumeCalibrationError};
    let env = |min: &str, max: &str| {
        BTreeMap::from([
            ("MASTER_VOLUME_MIN".to_owned(), min.to_owned()),
            ("MASTER_VOLUME_MAX".to_owned(), max.to_owned()),
        ])
    };
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    assert_eq!(
        load_volume_calibration(&mut state, &env("10", "250")),
        Ok(())
    );
    // (130 - 10) / 240 = 0.5; the default range would give 128 / 240.
    let knob = HalEvent::Volume(VolumeEvent::Level(130));
    assert_eq!(
        hal(&mut input, &mut state, knob, ZERO)[0],
        master_volume(0.5)
    );
    assert_eq!(read_volume(&mut state, Some(130)), master_volume(0.5));

    let invalid = load_volume_calibration(&mut state, &env("10", "300"));
    assert!(matches!(
        invalid,
        Err(VolumeCalibrationError::Invalid { .. })
    ));
    assert_eq!(
        state.master_volume.calibration,
        VolumeCalibration::default()
    );
    assert_eq!(read_volume(&mut state, Some(122)), master_volume(0.5));
}

/// The MasterVolume pushes of the knob path (`0x19b518` `engaged`, `0x19af74` echo).
fn volume_push(volume: Option<f32>, engaged: Option<bool>) -> Effect {
    use tetrad_control::codecs::{ControlMessage, MasterVolumeMessage, MessageAction};
    Effect::ControlNotify(Box::new(ControlMessage::MasterVolume(
        MasterVolumeMessage {
            action: Some(MessageAction::Update.into()),
            volume,
            engaged,
            ..Default::default()
        },
    )))
}

/// Cortex Control takes the QC Master Volume (`0x1fbb7c`); the knob then has to reach the
/// Control level before it drives the DSP again (`0x1fb86c`, `0x1fb980`). The wake from Be
/// Right Back reads the knob back in (`0x6a83e0` → `0x1fc518`).
#[test]
fn control_volume_makes_the_knob_pick_up_and_wake_reads_it_back() {
    use tetrad_app::device::actions::{self, master_volume};
    use tetrad_app::device::input::{VolumeOwner, control_volume, read_volume};
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    let knob = |raw| HalEvent::Volume(VolumeEvent::Level(raw));
    assert_eq!(
        hal(&mut input, &mut state, knob(122), ZERO),
        [master_volume(0.5), volume_push(Some(0.5), None)]
    );
    assert!(state.master_volume.engaged());

    assert_eq!(control_volume(&mut state, 0.25), [master_volume(0.25)]);
    assert_eq!(state.master_volume.owner, VolumeOwner::PickupFromAbove);
    assert!(!state.master_volume.engaged());
    assert!(hal(&mut input, &mut state, knob(112), ZERO).is_empty());
    assert_eq!(state.master_volume.level, 0.25);
    // The pickup tells Control `engaged` again, then the level (`0x1fb980`).
    assert_eq!(
        hal(&mut input, &mut state, knob(62), ZERO),
        [
            volume_push(None, Some(true)),
            master_volume(0.25),
            volume_push(Some(0.25), None)
        ]
    );
    assert!(state.master_volume.engaged());

    control_volume(&mut state, 0.9);
    let wake = actions::reduce(Action::Standby(false), &mut state).unwrap();
    assert!(wake.contains(&Effect::ReadVolume));
    assert_eq!(read_volume(&mut state, Some(122)), master_volume(0.5));
    assert_eq!(
        (state.master_volume.level, state.master_volume.engaged()),
        (0.5, true)
    );
}

/// Pedal moves send Preset MIDI Out and an expression action; the heel flag flips on each
/// fall to toe.
#[test]
fn expression_moves_send_midi_and_toggle_the_heel() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    let mut sweep = |value| {
        let event = HalEvent::Expression {
            pedal: Pedal::One,
            value,
        };
        let effects = hal(&mut input, &mut state, event, ZERO);
        for effect in &effects {
            if let Effect::Action(action) = effect {
                tetrad_app::device::actions::reduce(action.clone(), &mut state).unwrap();
            }
        }
        effects
    };
    let effects = sweep(255);
    assert_eq!(
        effects[0],
        Effect::PresetMidiOut(Trigger::Expression {
            pedal: Pedal::One,
            position: 1.0,
        })
    );
    sweep(0);
    assert!(input.expression_heel(&state, Pedal::One));
    assert!(!input.expression_heel(&state, Pedal::Two));
}

/// `CALIBRATE EXP 1` tracks the swept range (5 counts inside each extreme) and confirm
/// stores it in `ExpressionsSettings`.
#[test]
fn expression_calibration_stores_the_tracked_range() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    let start = ViewCommand::Device {
        action: DeviceAction::CalibrateExpression(1),
    };
    input.on_ui(&start, &mut state).unwrap();
    for value in [10, 200] {
        let event = HalEvent::Expression {
            pedal: Pedal::One,
            value,
        };
        hal(&mut input, &mut state, event, ZERO);
    }
    let save = ViewCommand::ExpressionCalibration {
        pedal: 1,
        save: true,
    };
    let effects = input.on_ui(&save, &mut state).unwrap();
    let range = state.settings.settings().expression.pedals[0];
    assert_eq!((range.min, range.max), (15, 195));
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::StartTimer { .. }))
    );
}

/// MIDI CC#35..=42 press and release footswitch A..H like the hardware (`0x0020fb78`), so
/// the result follows the mode: in preset mode CC#35 loads preset A of the bank.
#[test]
fn midi_footswitch_follows_the_mode() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    state.performance.footswitch.current_preset = 10;
    let effects = input.midi_footswitch(A, &mut state, ZERO).unwrap();
    let load = Action::LoadPreset {
        target: PresetRef {
            setlist: None,
            index: 8,
        },
        reload: true,
    };
    assert_eq!(actions(&effects), [&load]);
    assert_eq!(state.performance.held, 0, "the release follows the press");
}

/// Mini keys: 50 ms ticks, first step at 500 ms (f32 sum of 0.05 passes 0.5 on tick 10),
/// release step, blocked chord, saved gain, silent gain at 0 (`0x511bb0`).
#[test]
fn mini_volume_keys_repeat_release_and_block_both_keys() {
    let mut input = Input::new();
    let mut mini = state_mini();
    let key = |key, pressed| HalEvent::Volume(VolumeEvent::Key { key, pressed });
    let effects = hal(&mut input, &mut mini, key(VolumeKey::Up, true), ZERO);
    assert_eq!(
        effects,
        [Effect::StartTimer {
            timer: Timer::VolumeRepeat,
            after: VOLUME_REPEAT
        }]
    );
    for _ in 0..9 {
        assert!(actions(&input.on_volume_timer()).is_empty());
    }
    let mut step = |effects: Vec<Effect>| {
        let mut applied = Vec::new();
        for effect in effects {
            if let Effect::Action(action) = effect {
                applied.extend(tetrad_app::device::actions::reduce(action, &mut mini).unwrap());
            }
        }
        applied
    };
    let applied = step(input.on_volume_timer());
    let level = std::hint::black_box(0.30_f32);
    let gain = 10_f32.powf((level * 40.0 - 40.0) / 20.0);
    assert!(applied.contains(&Effect::Dsp(DspCommand::Request(
        Request::MasterVolumeChange {
            gain_bits: gain.to_bits()
        }
    ))));
    assert!(
        applied
            .iter()
            .any(|e| matches!(e, Effect::SettingsChanged(_)))
    );
    assert_eq!(mini.settings.settings().atma_volume.get(), 0.30);
    let repeat = input.on_volume_timer();
    for action in actions(&repeat) {
        tetrad_app::device::actions::reduce(action.clone(), &mut mini).unwrap();
    }
    let release = hal(&mut input, &mut mini, key(VolumeKey::Up, false), ZERO);
    for action in actions(&release) {
        tetrad_app::device::actions::reduce(action.clone(), &mut mini).unwrap();
    }
    assert_eq!(mini.settings.settings().atma_volume.get(), 0.40);
    assert!(release.contains(&Effect::StopTimer(Timer::VolumeRepeat)));
    hal(&mut input, &mut mini, key(VolumeKey::Down, true), ZERO);
    hal(&mut input, &mut mini, key(VolumeKey::Up, true), ZERO);
    for _ in 0..12 {
        assert!(actions(&input.on_volume_timer()).is_empty());
    }
    assert!(
        actions(&hal(
            &mut input,
            &mut mini,
            key(VolumeKey::Down, false),
            ZERO
        ))
        .is_empty()
    );
    assert!(actions(&input.on_volume_timer()).is_empty());
    assert!(actions(&hal(&mut input, &mut mini, key(VolumeKey::Up, false), ZERO)).is_empty());
    let mut qc = qc(FootswitchMode::Preset);
    assert!(hal(&mut input, &mut qc, key(VolumeKey::Up, true), ZERO).is_empty());
    let effects = tetrad_app::device::actions::reduce(Action::StepVolume(-1.0), &mut mini).unwrap();
    assert_eq!(mini.settings.settings().atma_volume.get(), 0.0);
    assert!(effects.contains(&Effect::Dsp(DspCommand::Request(
        Request::MasterVolumeChange {
            gain_bits: 0.0_f32.to_bits()
        }
    ))));
}

/// Recovery Mode (manual §12, inferred check): A + H held while the splash shows open the
/// Recovery screen; the completing press does nothing else and both releases are quiet.
/// After the splash the same chord is two normal presses.
#[test]
fn a_and_h_during_the_splash_open_recovery() {
    let mut state = qc(FootswitchMode::Scene);
    let mut input = Input::new();
    stomp(&mut input, &mut state, A, 1);
    let chord = stomp(&mut input, &mut state, H, 1);
    assert_eq!(
        chord,
        [Effect::UiInput(UiInput::Show(Screen::Recovery))],
        "{chord:?}"
    );
    assert!(actions(&stomp(&mut input, &mut state, H, 0)).is_empty());
    assert!(actions(&stomp(&mut input, &mut state, A, 0)).is_empty());

    state.splash_dismissed = true;
    stomp(&mut input, &mut state, A, 1);
    let late = stomp(&mut input, &mut state, H, 1);
    assert!(!late.contains(&Effect::UiInput(UiInput::Show(Screen::Recovery))));
}

/// Be Right Back (`0x6a82ac`) on an ESS board: 48 V off, headphones muted (word 100),
/// backlight 0, DSP `0x20` (1, 1); the wake turns 48 V on, restores the screen brightness
/// and HP LEVEL, unmutes, then re-reads the knob (`0x1fc518`). Every HAL write is unverified,
/// as stock. Both end with the Cortex Control notify `0x16b758` (GeneralSettings
/// `power_option` Standby or WakeUp).
#[test]
fn standby_mutes_outputs_and_blanks_the_backlight_then_restores() {
    use tetrad_control::codecs::{
        ControlMessage, GeneralSettingsMessage, MessageAction, PowerOptions,
    };
    use tetrad_hal::analog::Verification;
    use tetrad_hal::hal::Command;
    use tetrad_hal::requests::{Brightness, DisplayCommand};
    use tetrad_hal::zenjack::{Control, HeadphoneVolume, Input as Jack, InputType, Setting, Value};
    use tetrad_settings::io::ConnectorId;
    let notify = |option: PowerOptions| {
        Effect::ControlNotify(Box::new(ControlMessage::GeneralSettings(
            GeneralSettingsMessage {
                action: Some(MessageAction::Update.into()),
                power_option: Some(option.into()),
                ..Default::default()
            },
        )))
    };
    let reduce = |action, state: &mut State| {
        let mut effects = tetrad_app::device::actions::reduce(action, state).unwrap();
        effects.retain(|effect| *effect != Effect::Present);
        effects
    };
    let mut state = state(HalPlatform::QcEss, FootswitchMode::Preset);
    let phantom_on = Action::SetConnector {
        id: ConnectorId::In1Phantom,
        value: 1.0,
    };
    reduce(phantom_on, &mut state);
    let set = |control, value| {
        Effect::Hal(Command::Set(
            Setting::new(control, value).unwrap(),
            Verification::Unverified,
        ))
    };
    let jack = |kind| set(Control::InputType(Jack::One), Value::InputType(kind));
    let headphones = |volume| set(Control::HeadphoneVolume, Value::HeadphoneVolume(volume));
    let dark = DisplayCommand::Brightness(Brightness::try_from(0).unwrap());
    assert_eq!(
        reduce(Action::Standby(true), &mut state),
        [
            jack(InputType::Xlr),
            headphones(HeadphoneVolume::Muted),
            Effect::Hal(Command::Display(dark)),
            Effect::Dsp(DspCommand::MasterMute(3)),
            notify(PowerOptions::Standby),
        ]
    );
    let level = state.io.value(ConnectorId::HpLevel);
    assert_eq!(
        reduce(Action::Standby(false), &mut state),
        [
            jack(InputType::XlrPhantom),
            Effect::Backlight(state.settings.settings().display.brightness.get()),
            headphones(HeadphoneVolume::from_unit(level).unwrap()),
            Effect::Dsp(DspCommand::MasterMute(0)),
            Effect::ReadVolume,
            notify(PowerOptions::WakeUp),
        ]
    );
}

/// `0x1fc76c` emits `updatePotentiometerSignal(false)`: after the splash a mini key step
/// also shows the Master Volume overlay (`0x4fffb8`).
#[test]
fn mini_volume_step_shows_the_volume_overlay() {
    let mut mini = state_mini();
    mini.splash_dismissed = true;
    let effects = tetrad_app::device::actions::reduce(Action::StepVolume(0.05), &mut mini).unwrap();
    let overlay = UiInput::View(InputEvent::MasterVolume { percent: 30 });
    assert!(effects.contains(&Effect::UiInput(overlay)));
}

/// Lock Touchscreen and Master Volume (`0x1fb86c` forces owner 2): while locked the knob never
/// sets the level; after the unlock it picks the level up, as after a Cortex Control change.
#[test]
fn lock_holds_the_knob_until_it_picks_up_the_level() {
    use tetrad_app::device::actions::{master_volume, reduce};
    use tetrad_app::device::input::VolumeOwner;
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    let knob = |raw| HalEvent::Volume(VolumeEvent::Level(raw));
    hal(&mut input, &mut state, knob(122), ZERO);
    reduce(Action::SetLocked(true), &mut state).unwrap();
    assert!(hal(&mut input, &mut state, knob(240), ZERO).is_empty());
    assert_eq!(state.master_volume.level, 0.5);

    reduce(Action::SetLocked(false), &mut state).unwrap();
    assert!(hal(&mut input, &mut state, knob(200), ZERO).is_empty());
    assert_eq!(state.master_volume.owner, VolumeOwner::PickupFromAbove);
    assert_eq!(
        hal(&mut input, &mut state, knob(62), ZERO),
        [
            volume_push(None, Some(true)),
            master_volume(0.25),
            volume_push(Some(0.25), None)
        ]
    );
}

/// Copy / Swap Scene turns the assign mode on (ModeController `0x5898f4(1)`): a footswitch
/// without the dialog's scene meaning does nothing on press or release; TEMPO neither taps
/// nor starts a hold (r14-ui open gap 1).
#[test]
fn scene_copy_mode_blocks_every_other_footswitch_meaning() {
    use tetrad_app::device::actions::{SceneClipboard, reduce};
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Preset));
    let clipboard = SceneClipboard {
        source: None,
        swap: false,
    };
    reduce(Action::SceneCopyMode(Some(clipboard)), &mut state).unwrap();
    for switch in [A, UP, TEMPO] {
        assert!(actions(&stomp(&mut input, &mut state, switch, 1)).is_empty());
        assert_eq!(input.hold_deadline(), None);
        assert!(actions(&stomp(&mut input, &mut state, switch, 0)).is_empty());
    }
    reduce(Action::SceneCopyMode(None), &mut state).unwrap();
    assert!(!actions(&stomp(&mut input, &mut state, A, 1)).is_empty());
}

/// "Assign footswitch" and Gig EDIT STOMP block the footswitch actions (`0x5898f4` from
/// `0x4b569c`, `0x2c5710`): the press and its release reach only the UI dialog.
#[test]
fn assign_dialog_takes_the_footswitch_without_its_action() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Stomp));
    let edge = |pressed| {
        Effect::UiInput(UiInput::View(InputEvent::Footswitch {
            switch: tetrad_ui::Footswitch::A,
            pressed,
        }))
    };
    state.footswitch_dialog = true;
    assert_eq!(stomp(&mut input, &mut state, A, 1), [edge(true)]);
    // The press closed the dialog; the release still has no action.
    state.footswitch_dialog = false;
    assert_eq!(stomp(&mut input, &mut state, A, 0), [edge(false)]);
    assert!(!actions(&stomp(&mut input, &mut state, A, 1)).is_empty());
}

/// NC2 A/B dialog (r18-ui MR 4): footswitch A (`0x3d8284`) and encoder 0 (`0x3d8094`) reach
/// tetrad-ui, which maps them to `Nc2Request::Audition`. The modal dialog keeps the stomp action
/// from the user preset (inferred).
#[test]
fn nc2_dialog_takes_footswitch_a_and_encoder_0_without_their_action() {
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Stomp));
    state.views.capture.nc2 = Some(tetrad_ui::Nc2Dialog::Ready);
    for pressed in [true, false] {
        let edge = Effect::UiInput(UiInput::View(InputEvent::Footswitch {
            switch: tetrad_ui::Footswitch::A,
            pressed,
        }));
        assert_eq!(stomp(&mut input, &mut state, A, i32::from(pressed)), [edge]);
    }
    let element = Element::all().next().unwrap();
    let turned = hal(
        &mut input,
        &mut state,
        HalEvent::Knob { element, delta: 1 },
        ZERO,
    );
    assert!(
        matches!(
            turned[..],
            [Effect::UiInput(UiInput::View(InputEvent::Knob { knob, delta }))]
                if knob.get() == 0 && delta > 0
        ),
        "{turned:?}"
    );
}

/// Footswitch scene copy (`0x20eec4`): the first scene press picks the source (inferred), the
/// next scene takes the copy or swap from it (`0x51a2a8`, then forced) and the mode ends.
#[test]
fn scene_copy_mode_pastes_the_source_into_the_pressed_scene() {
    use tetrad_app::device::actions::{SceneClipboard, reduce};
    let (mut input, mut state) = (Input::new(), qc(FootswitchMode::Scene));
    let scene = |index| SceneIndex::new(index).unwrap();
    let mode = |source| Action::SceneCopyMode(Some(SceneClipboard { source, swap: true }));
    reduce(mode(None), &mut state).unwrap();
    let picked = stomp(&mut input, &mut state, B, 1);
    assert_eq!(actions(&picked), [&mode(Some(scene(1)))]);
    reduce(mode(Some(scene(1))), &mut state).unwrap();
    stomp(&mut input, &mut state, B, 0);

    let pasted = stomp(&mut input, &mut state, 2, 1);
    assert_eq!(
        actions(&pasted),
        [
            &Action::CopyScene {
                first: scene(1),
                second: scene(2),
                swap: true,
            },
            &Action::SceneCopyMode(None),
        ]
    );
}

/// Cortex Control Gig View buttons (`0x109ab4`, r16-control MR 5): a preset tile loads its
/// slot on the release without a forced reload (`0x65ffdc`); a stomp tile toggles on the
/// press and releases on the release, with the driver hooks and no Preset MIDI Out; tile 8
/// is out of range.
#[test]
fn gig_view_buttons_follow_the_tile_page() {
    let (mut input, state) = (Input::new(), qc(FootswitchMode::Preset));
    assert!(input.gig_view_button(2, true, &state, false).is_empty());
    let load = Action::LoadPreset {
        target: PresetRef {
            setlist: None,
            index: 2,
        },
        reload: false,
    };
    assert_eq!(
        actions(&input.gig_view_button(2, false, &state, false)),
        [&load]
    );
    assert!(input.gig_view_button(8, false, &state, false).is_empty());

    let state = qc(FootswitchMode::Stomp);
    let stomp = StompIndex::new(3).unwrap();
    assert_eq!(
        input.gig_view_button(3, true, &state, false),
        [
            Effect::Action(Action::ToggleStomp(stomp)),
            Effect::Dsp(DspCommand::Footswitch {
                stomp,
                pressed: true
            })
        ]
    );
    assert_eq!(
        actions(&input.gig_view_button(3, false, &state, false)),
        [&Action::ReleaseStomp(stomp)]
    );
}

/// Round-16 review note: a Gig View tile from Cortex Control obeys the gates of a physical
/// press. The update install mask, the Assign/EDIT STOMP dialog, the NC2 dialog and Copy/Swap
/// Scene mode each drop the press and the release.
#[test]
fn gig_view_buttons_respect_the_footswitch_gates() {
    let mut input = Input::new();
    let gates: [&dyn Fn(&mut State) -> bool; 4] = [
        &|_| true,
        &|state| {
            state.footswitch_dialog = true;
            false
        },
        &|state| {
            state.views.capture.nc2 = Some(tetrad_ui::Nc2Dialog::Ready);
            false
        },
        &|state| {
            state.performance.footswitch.scene_copy = true;
            false
        },
    ];
    for gate in gates {
        let mut state = qc(FootswitchMode::Stomp);
        let masked = gate(&mut state);
        for pressed in [true, false] {
            assert!(input.gig_view_button(3, pressed, &state, masked).is_empty());
        }
    }
}

//! Recovered jack scheduler timing on an in-memory HAL; no hardware or stock data.

use std::time::Duration;

use tetrad_app::device::jacks::{InputChange, InputScheduler};
use tetrad_app::device::{DspCommand, Effect, Timer};
use tetrad_hal::analog::{GainEndpoint, Verification};
use tetrad_hal::codec::Impedance;
use tetrad_hal::hal::{Command, Query, Reading, RecordingHal};
use tetrad_hal::platform::Platform;
use tetrad_hal::zenjack::{Control, GainPlan, Input, InputType, Setting, Value, ZENUI_VERIFY};

fn delay(ms: u64) -> Effect {
    Effect::StartTimer {
        timer: Timer::InputScheduler,
        after: Duration::from_millis(ms),
    }
}

/// An unverified write: every stock I/O write outside the boot restore.
fn set(control: Control, value: Value) -> Effect {
    write(control, value, Verification::Unverified)
}

fn write(control: Control, value: Value, verification: Verification) -> Effect {
    Effect::Hal(Command::Set(
        Setting::new(control, value).unwrap(),
        verification,
    ))
}

const UNVERIFIED: Verification = Verification::Unverified;

fn hal() -> RecordingHal {
    let mut hal = RecordingHal::default();
    for input in [Input::One, Input::Two] {
        hal.set_reading(
            Query::Setting(Control::InputType(input)),
            Reading::Value(Value::InputType(InputType::Instrument)),
        );
        hal.set_reading(
            Query::Setting(Control::InputImpedance(input)),
            Reading::Value(Value::Impedance(Impedance::try_from(8).unwrap())),
        );
    }
    hal
}

const MUTE: Effect = Effect::Dsp(DspCommand::InputTrims(None));

/// `0x7d7450` states 0 → 5 → 6 → 7: mute, 20 ms, zero impedances (QC) and switch, 500 ms,
/// restore, 25 ms, unmute. A request equal to the cache does nothing.
#[test]
fn combo_type_mutes_switches_restores_and_unmutes_with_stock_delays() {
    for platform in [Platform::QcOriginal, Platform::Atma] {
        let mut scheduler = InputScheduler::new(platform);
        let mut hal = hal();
        let change = InputChange::Type {
            input: Input::One,
            kind: InputType::XlrPhantom,
            verification: UNVERIFIED,
        };
        assert_eq!(
            scheduler.enqueue(change, &mut hal).unwrap(),
            [MUTE, delay(20)]
        );
        assert!(scheduler.enqueue(change, &mut hal).unwrap().is_empty());
        let zero = Value::Impedance(Impedance::try_from(0).unwrap());
        let saved = Value::Impedance(Impedance::try_from(8).unwrap());
        let mut switched = Vec::new();
        let mut restore = Vec::new();
        if platform.is_qc() {
            for input in [Input::One, Input::Two] {
                switched.push(set(Control::InputImpedance(input), zero));
                restore.push(set(Control::InputImpedance(input), saved));
            }
        }
        switched.extend([
            set(
                Control::InputType(Input::One),
                Value::InputType(InputType::XlrPhantom),
            ),
            delay(500),
        ]);
        restore.push(delay(25));
        assert_eq!(scheduler.on_timer(), switched);
        assert_eq!(scheduler.on_timer(), restore);
        assert_eq!(
            scheduler.on_timer(),
            [Effect::Dsp(DspCommand::InputTrims(Some([0.0; 4])))]
        );
        assert!(scheduler.enqueue(change, &mut hal).unwrap().is_empty());
    }
}

/// Gain first: mute, then the PGA write after 10 ms (QC) or 1 ms (mini) together with the
/// queued impedances, then 50 ms or 1 ms before the type switch (`0x7d65c4`, state 1 → 2).
/// An unchanged PGA step skips mute and write but still applies the new trim. Each write
/// keeps the verify flags of its own request (`0x7d6ec4` `+0x90`, `+0x91`).
#[test]
fn gain_writes_after_the_mute_then_impedance_then_type() {
    for platform in [Platform::QcOriginal, Platform::Atma] {
        let qc = platform.is_qc();
        let mut scheduler = InputScheduler::new(platform);
        let mut hal = hal();
        let plan = GainPlan::from_unit(platform, 0.5).unwrap();
        let gain = InputChange::Gain {
            endpoint: GainEndpoint::Input1,
            plan,
            verification: ZENUI_VERIFY,
        };
        let settle = delay(if qc { 10 } else { 1 });
        assert_eq!(
            scheduler.enqueue(gain, &mut hal).unwrap(),
            [MUTE, settle.clone()]
        );
        let impedance = Impedance::try_from(3).unwrap();
        for change in [
            InputChange::Impedance {
                input: Input::One,
                impedance,
                verification: UNVERIFIED,
            },
            InputChange::Type {
                input: Input::One,
                kind: InputType::Xlr,
                verification: UNVERIFIED,
            },
        ] {
            assert!(scheduler.enqueue(change, &mut hal).unwrap().is_empty());
        }
        let mut written = vec![write(
            Control::InputGain(Input::One),
            Value::Gain(plan.step),
            ZENUI_VERIFY,
        )];
        if qc {
            written.push(set(
                Control::InputImpedance(Input::One),
                Value::Impedance(impedance),
            ));
        }
        written.push(delay(if qc { 50 } else { 1 }));
        assert_eq!(scheduler.on_timer(), written);
        assert_eq!(scheduler.on_timer(), [MUTE, delay(20)]);
        scheduler.on_timer();
        scheduler.on_timer();
        let trims = [plan.trim, 0.0, 0.0, 0.0];
        assert_eq!(
            scheduler.on_timer(),
            [Effect::Dsp(DspCommand::InputTrims(Some(trims)))]
        );
        let same_step = GainPlan { trim: 0.5, ..plan };
        let change = InputChange::Gain {
            endpoint: GainEndpoint::Input1,
            plan: same_step,
            verification: UNVERIFIED,
        };
        assert_eq!(scheduler.enqueue(change, &mut hal).unwrap(), [settle]);
        assert_eq!(
            scheduler.on_timer(),
            [Effect::Dsp(DspCommand::InputTrims(Some([
                0.5, 0.0, 0.0, 0.0
            ])))]
        );
    }
}

/// QC impedance alone: mute, write after 1 ms, settle 25 ms, unmute (states 3 → 4).
#[test]
fn impedance_writes_once_after_the_mute_and_settles() {
    let mut scheduler = InputScheduler::new(Platform::QcOriginal);
    let mut hal = hal();
    let impedance = Impedance::try_from(2).unwrap();
    let change = InputChange::Impedance {
        input: Input::Two,
        impedance,
        verification: UNVERIFIED,
    };
    assert_eq!(
        scheduler.enqueue(change, &mut hal).unwrap(),
        [MUTE, delay(1)]
    );
    assert_eq!(
        scheduler.on_timer(),
        [
            set(
                Control::InputImpedance(Input::Two),
                Value::Impedance(impedance)
            ),
            delay(25)
        ]
    );
    assert_eq!(
        scheduler.on_timer(),
        [Effect::Dsp(DspCommand::InputTrims(Some([0.0; 4])))]
    );
}

fn state(platform: Platform) -> tetrad_app::device::State {
    use tetrad_settings::device::{DeviceSettings, Hardware};
    use tetrad_settings::{SettingsFileId, SettingsStorage};
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
    let (hardware, product) = if platform.is_atma() {
        (Hardware::Atma, tetrad_hal::platform::Product::Atma)
    } else {
        (
            Hardware::QuadCortex,
            tetrad_hal::platform::Product::QuadCortex,
        )
    };
    let (settings, _) = DeviceSettings::load(&NoFiles, hardware).unwrap();
    tetrad_app::device::State::new(product, platform, settings)
}

fn external(on: bool) -> Effect {
    Effect::ScheduleInput(InputChange::ExternalMute(on))
}

/// `0x7d6440` / `0x7d7378`: the external mute keeps every trim write at zero, including the
/// scheduler's own unmute; releasing it restores the trims.
#[test]
fn external_mute_holds_trims_at_zero_until_released() {
    let mut scheduler = InputScheduler::new(Platform::QcEss);
    let mut hal = hal();
    let mute = InputChange::ExternalMute(true);
    assert_eq!(scheduler.enqueue(mute, &mut hal).unwrap(), [MUTE]);
    let plan = GainPlan::from_unit(Platform::QcEss, 0.5).unwrap();
    let gain = InputChange::Gain {
        endpoint: GainEndpoint::Return1,
        plan,
        verification: UNVERIFIED,
    };
    scheduler.enqueue(gain, &mut hal).unwrap();
    scheduler.on_timer();
    assert_eq!(scheduler.on_timer(), [MUTE]);
    let release = InputChange::ExternalMute(false);
    assert_eq!(
        scheduler.enqueue(release, &mut hal).unwrap(),
        [Effect::Dsp(DspCommand::InputTrims(Some([
            0.0, 0.0, plan.trim, 0.0
        ])))]
    );
}

/// Boot restore `0x35c4ac` on an ESS QC with stock defaults: external mute and 10 ms, then
/// the table order (gains IN 1, IN 2, RET 1, RET 2; `0x06` SEND 1, 2, OUT 3, 4, OUT 1, 2
/// while unpaired; `0x11` feeds), a full 100 ms after the loop (`0x35f740`), unmute, then
/// save-all of every id.
#[test]
fn boot_restore_applies_in_table_order_under_a_100ms_mute_then_saves_all() {
    use tetrad_dsp_protocol::requests::Request;
    let mut state = state(Platform::QcEss);
    let effects = tetrad_app::device::jacks::restore(&mut state, ZENUI_VERIFY);
    assert_eq!(
        effects[..2],
        [external(true), Effect::Sleep(Duration::from_millis(10))]
    );
    let unmute = effects.iter().position(|e| *e == external(false)).unwrap();
    let slept: Duration = effects[..unmute]
        .iter()
        .filter_map(|e| match e {
            Effect::Sleep(d) => Some(*d),
            _ => None,
        })
        .sum();
    assert_eq!(slept, Duration::from_millis(110));
    assert_eq!(
        effects[unmute - 1],
        Effect::Sleep(Duration::from_millis(100))
    );
    let applied = &effects[2..unmute];
    let gains: Vec<_> = applied
        .iter()
        .filter_map(|e| match e {
            Effect::ScheduleInput(InputChange::Gain { endpoint, .. }) => Some(*endpoint),
            _ => None,
        })
        .collect();
    use GainEndpoint::*;
    assert_eq!(gains, [Input1, Input2, Return1, Return2]);
    let request = |e: &Effect| match e {
        Effect::Dsp(DspCommand::Request(request)) => Some(request.clone()),
        _ => None,
    };
    let outputs: Vec<_> = applied
        .iter()
        .filter_map(request)
        .filter_map(|r| match r {
            Request::SetOutputGain { output, gain_bits } => Some((output, gain_bits)),
            _ => None,
        })
        .collect();
    // Each OUT jack sends twice: its level, then its mute (`0x35a828` → `0x3574e8`).
    let unity = 1.0f32.to_bits();
    let expected: Vec<_> = [4, 5, 2, 2, 3, 3, 0, 0, 1, 1]
        .into_iter()
        .map(|output| (output, unity))
        .collect();
    assert_eq!(outputs, expected);
    let mixes: Vec<_> = applied
        .iter()
        .filter_map(request)
        .filter_map(|r| match r {
            Request::SetHeadphoneMixLevel { mix, .. } => Some(mix),
            _ => None,
        })
        .collect();
    assert_eq!(mixes, [4, 5, 6, 7, 8, 9, 1, 1, 2, 3]);
    assert!(matches!(effects[unmute + 1], Effect::SettingsChanged(_)));
    let stored = &state.settings.settings().io.params;
    assert_eq!(stored.len(), 48);
    assert!(stored.values().all(|param| param.value.is_some()));
}

/// I/O reload `0x360da8` (I/O preset load): the stored rows go live with no external mute,
/// no sleep and no save-all; the table applies first, then both pairs (`0x3608e8`); MIDI THRU
/// comes from the MIDI page (`0x3312c8`), not the stored row.
#[test]
fn io_reload_applies_the_stored_rows_then_both_pairs_without_mute_or_save() {
    use tetrad_app::device::Action;
    use tetrad_dsp_protocol::requests::Request;
    use tetrad_settings::io::ConnectorId;
    use tetrad_ui::{SettingId, SettingValue};
    let mut state = state(Platform::QcEss);
    state.settings.update(|settings| {
        let params = &mut settings.io.params;
        params.entry(ConnectorId::MidiThru.id()).or_default().value = Some(1.0);
        params
            .entry(ConnectorId::Send1Level.id())
            .or_default()
            .value = Some(0.0);
    });
    let effects = tetrad_app::device::jacks::reload(&mut state);
    assert!(!effects.iter().any(|e| matches!(
        e,
        Effect::Sleep(_)
            | Effect::SettingsChanged(_)
            | Effect::ScheduleInput(InputChange::ExternalMute(_))
    )));
    assert_eq!(state.io.value(ConnectorId::MidiThru), 0.0);
    assert!(effects.contains(&Effect::Action(Action::ChangeSetting {
        id: SettingId::MidiThru,
        value: SettingValue::Bool(false),
    })));
    let outputs: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Dsp(DspCommand::Request(Request::SetOutputGain { output, gain_bits })) => {
                Some((*output, f32::from_bits(*gain_bits)))
            }
            _ => None,
        })
        .collect();
    // SEND 1 (output 4) is the stored row. The table writes each OUT jack's level, then its
    // mute; the pairs (OUT 1/2, then OUT 3/4) write one frame per jack.
    assert_eq!(outputs[0], (4, 0.0));
    let order: Vec<_> = outputs.iter().map(|&(output, _)| output).collect();
    assert_eq!(order, [4, 5, 2, 2, 3, 3, 0, 0, 1, 1, 0, 1, 2, 3]);
}

/// §2.2: a non-ESS QC switches the combo jack at once (external mute, 10 ms, `SetInputType`,
/// 100 ms, unmute); an ESS QC queues it with TYPE ignored. Review w5 R2: a non-ESS QC refuses
/// PHANTOM on TS (`0x35f75c`) and TYPE → TS stores PHANTOM 0 (`0x359598`–`0x3595cc`), so a
/// later TYPE → Mic never turns 48 V on by itself.
#[test]
fn non_ess_qc_switches_the_type_directly_and_never_keeps_phantom_on_ts() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::reduce;
    use tetrad_settings::io::ConnectorId;
    use tetrad_ui::{InputJack, SettingId, SettingValue};
    let change = |id, value| Action::ChangeSetting { id, value };
    let phantom = change(SettingId::Phantom(InputJack::In1), SettingValue::Bool(true));
    let jack = |choice| {
        change(
            SettingId::InputType(InputJack::In1),
            SettingValue::Choice(choice),
        )
    };
    let kind = |kind| set(Control::InputType(Input::One), Value::InputType(kind));
    let mut qc = state(Platform::QcOriginal);
    let effects = reduce(phantom.clone(), &mut qc).unwrap();
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::Hal(_) | Effect::ScheduleInput(_)))
    );
    assert_eq!(qc.io.value(ConnectorId::In1Phantom), 0.0);
    let effects = reduce(jack(1), &mut qc).unwrap();
    assert_eq!(
        effects[..5],
        [
            external(true),
            Effect::Sleep(Duration::from_millis(10)),
            kind(InputType::Xlr),
            Effect::Sleep(Duration::from_millis(100)),
            external(false),
        ]
    );
    let effects = reduce(phantom.clone(), &mut qc).unwrap();
    assert!(effects.contains(&kind(InputType::XlrPhantom)));
    let effects = reduce(jack(0), &mut qc).unwrap();
    assert!(effects.contains(&kind(InputType::Instrument)));
    assert_eq!(qc.io.value(ConnectorId::In1Phantom), 0.0);
    let effects = reduce(jack(1), &mut qc).unwrap();
    assert!(effects.contains(&kind(InputType::Xlr)));

    let mut ess = state(Platform::QcEss);
    let effects = reduce(jack(1), &mut ess).unwrap();
    assert_eq!(
        effects[0],
        Effect::ScheduleInput(InputChange::Type {
            input: Input::One,
            kind: InputType::Xlr,
            verification: UNVERIFIED,
        })
    );
    let effects = reduce(phantom, &mut ess).unwrap();
    assert_eq!(
        effects[0],
        Effect::ScheduleInput(InputChange::Type {
            input: Input::One,
            kind: InputType::XlrPhantom,
            verification: UNVERIFIED,
        })
    );
}

/// Review w5 R13 and R2: a non-ESS restore writes each type inside the one restore mute
/// (`+0x2ac` skips the per-type wrapper, `0x3595fc`), and a stored PHANTOM on a TS jack is
/// stored back as 0.
#[test]
fn non_ess_restore_writes_types_inside_its_one_mute() {
    let mut state = state(Platform::QcOriginal);
    state.settings.update(|settings| {
        for (id, value) in [(104, 1.0), (202, 1.0), (204, 1.0)] {
            settings.io.params.entry(id).or_default().value = Some(value);
        }
    });
    let effects = tetrad_app::device::jacks::restore(&mut state, ZENUI_VERIFY);
    let mutes: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::ScheduleInput(InputChange::ExternalMute(_))))
        .collect();
    assert_eq!(mutes, [&external(true), &external(false)]);
    let kind = |input, kind| {
        write(
            Control::InputType(input),
            Value::InputType(kind),
            ZENUI_VERIFY,
        )
    };
    assert!(effects.contains(&kind(Input::One, InputType::Instrument)));
    assert!(effects.contains(&kind(Input::Two, InputType::XlrPhantom)));
    let stored = |id| {
        state
            .settings
            .settings()
            .io
            .params
            .get(&id)
            .and_then(|p| p.value)
    };
    assert_eq!(stored(104), Some(0.0));
    assert_eq!(stored(204), Some(1.0));
}

/// Every HAL setting write of `effects` with its flags; scheduler requests run through one
/// [`InputScheduler`] until its timer stops.
fn hal_sets(platform: Platform, effects: Vec<Effect>) -> Vec<(Control, Verification)> {
    let mut scheduler = InputScheduler::new(platform);
    let mut hal = hal();
    let mut pending = std::collections::VecDeque::from(effects);
    let mut sets = Vec::new();
    loop {
        let mut armed = false;
        while let Some(effect) = pending.pop_front() {
            match effect {
                Effect::Hal(Command::Set(setting, verification)) => {
                    sets.push((setting.control(), verification));
                }
                Effect::ScheduleInput(change) => {
                    pending.extend(scheduler.enqueue(change, &mut hal).unwrap());
                }
                Effect::StartTimer {
                    timer: Timer::InputScheduler,
                    ..
                } => armed = true,
                _ => {}
            }
        }
        if !armed {
            return sets;
        }
        pending.extend(scheduler.on_timer());
    }
}

/// r48 A3: stock verifies (write, 25 ms, read, 25 ms per write) only in the boot restore
/// (`0x35476c(v, id, 1, 1)` at `0x35e6d4`, `0x35b068`). An edit (`0x354c74`, `0x35b8e8`), the
/// reload (`0x360da8`) and the reset (`0x3615a0`) write with readback and commgap 0, also the
/// gains, impedances and types the scheduler writes later.
#[test]
fn only_the_boot_restore_verifies_its_io_writes() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::reduce;
    use tetrad_app::device::jacks;
    use tetrad_hal::zenjack::{Return, XlrOut};
    use tetrad_settings::io::ConnectorId::*;
    for platform in [Platform::QcOriginal, Platform::QcEss] {
        let mut state = state(platform);
        let boot = hal_sets(platform, jacks::restore(&mut state, ZENUI_VERIFY));
        assert!(boot.contains(&(Control::InputGain(Input::One), ZENUI_VERIFY)));
        assert!(
            boot.iter().all(|&(_, flags)| flags == ZENUI_VERIFY),
            "{boot:?}"
        );

        let mut effects = Vec::new();
        for (id, value) in [
            (In1Level, 0.9),
            (In2Impedance, 0.25),
            (In1Type, 1.0),
            (In1GroundLift, 1.0),
            (Ret1GroundLift, 1.0),
            (Out1GroundLift, 1.0),
            (HpLevel, 0.3),
        ] {
            effects.extend(reduce(Action::SetConnector { id, value }, &mut state).unwrap());
        }
        let edits = hal_sets(platform, effects);
        for control in [
            Control::InputGain(Input::One),
            Control::InputImpedance(Input::Two),
            Control::InputType(Input::One),
            Control::InputGroundLift(Input::One),
            Control::ReturnGroundLift(Return::One),
            Control::XlrOutGroundLift(XlrOut::One),
        ] {
            assert!(
                edits.contains(&(control, UNVERIFIED)),
                "{control:?}: {edits:?}"
            );
        }
        assert!(
            edits.iter().all(|&(_, flags)| flags == UNVERIFIED),
            "{edits:?}"
        );

        for effects in [jacks::reload(&mut state), jacks::reset(&mut state, false)] {
            let sets = hal_sets(platform, effects);
            assert!(!sets.is_empty());
            assert!(
                sets.iter().all(|&(_, flags)| flags == UNVERIFIED),
                "{sets:?}"
            );
        }
    }
}

/// Review w5 R3: the I/O MIDI THRU row (1301) and the MIDI page share `MidiSettings/Passthru`
/// (`0x330478`). The row goes through the MIDI setting, whose bridge switches the jack; a
/// MIDI page edit moves the row and restarts its save.
#[test]
fn midi_thru_row_and_midi_page_share_passthru() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::reduce;
    use tetrad_settings::device::SettingsEvent;
    use tetrad_settings::io::ConnectorId;
    use tetrad_ui::{SettingId, SettingValue};
    let mut state = state(Platform::QcOriginal);
    let thru = |on| Action::ChangeSetting {
        id: SettingId::MidiThru,
        value: SettingValue::Bool(on),
    };
    let row = Action::SetConnector {
        id: ConnectorId::MidiThru,
        value: 1.0,
    };
    let effects = reduce(row, &mut state).unwrap();
    assert!(effects.contains(&Effect::Action(thru(true))));
    assert!(!effects.iter().any(|e| matches!(e, Effect::Hal(_))));
    let effects = reduce(thru(true), &mut state).unwrap();
    assert!(state.settings.settings().midi.thru);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SettingsChanged(SettingsEvent::Midi(_))))
    );
    let effects = reduce(thru(false), &mut state).unwrap();
    assert_eq!(state.io.value(ConnectorId::MidiThru), 0.0);
    assert!(effects.contains(&Effect::StartTimer {
        timer: Timer::ConnectorsSave,
        after: Duration::from_millis(250),
    }));
}

/// A refused 48 V write (bring-up) leaves PHANTOM off live and in the stored file, and the
/// jack goes to XLR without 48 V, so no later save or restore brings the phantom back.
#[test]
fn refused_phantom_is_stored_off_and_the_jack_reapplies_without_it() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::reduce;
    use tetrad_settings::io::ConnectorId;
    let mut state = state(Platform::QcEss);
    let on = Action::SetConnector {
        id: ConnectorId::In1Phantom,
        value: 1.0,
    };
    reduce(on, &mut state).unwrap();
    let effects = tetrad_app::device::jacks::refuse_phantom(&mut state, Input::One);
    assert_eq!(state.io.value(ConnectorId::In1Phantom), 0.0);
    let stored = state.settings.settings().io.params.get(&104);
    assert_eq!(stored.and_then(|param| param.value), Some(0.0));
    assert!(effects.contains(&Effect::ScheduleInput(InputChange::Type {
        input: Input::One,
        kind: InputType::Xlr,
        verification: UNVERIFIED,
    })));
}

/// `0x35c95c`: an edit applies at once and (re)starts the 250 ms save; the stored value
/// changes only when the save runs.
#[test]
fn edits_apply_at_once_and_save_250ms_later() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::reduce;
    use tetrad_settings::io::ConnectorId;
    let mut state = state(Platform::QcOriginal);
    let save = Effect::StartTimer {
        timer: Timer::ConnectorsSave,
        after: Duration::from_millis(250),
    };
    let edit = Action::SetConnector {
        id: ConnectorId::Out12Mute,
        value: 1.0,
    };
    let effects = reduce(edit, &mut state).unwrap();
    assert!(effects.contains(&save));
    assert_eq!(state.io.value(ConnectorId::Out12Mute), 1.0);
    let stored = |state: &tetrad_app::device::State| {
        state
            .settings
            .settings()
            .io
            .params
            .get(&1052)
            .and_then(|p| p.value)
    };
    assert_eq!(stored(&state), None);
    let effects = tetrad_app::device::jacks::save(&mut state);
    assert_eq!(stored(&state), Some(1.0));
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::StartTimer {
            timer: Timer::SettingsFlush,
            ..
        }
    )));
}

/// The factory default I/O preset (`0x3615a0`): every connector row goes back to its catalog
/// default, both output pairs link, and both EXP calibrations go to 0..255 (`0x320fdc`).
#[test]
fn factory_io_preset_resets_rows_pairs_and_exp_calibration() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::reduce;
    use tetrad_settings::io::{ConnectorId, ExpressionRange};
    let mut state = state(Platform::QcEss);
    let default = state.io.value(ConnectorId::Send1Level);
    state.settings.update(|settings| {
        let row = settings.io.params.entry(ConnectorId::Send1Level.id());
        row.or_default().value = Some(if default > 0.5 { 0.0 } else { 1.0 });
        settings.io.out12_paired = false;
        settings.expression.pedals[1] = ExpressionRange { min: 20, max: 200 };
    });
    tetrad_app::device::jacks::reload(&mut state);
    reduce(Action::LoadFactoryIoPreset, &mut state).unwrap();
    tetrad_app::device::jacks::settle_io_loads(&mut state);
    let settings = state.settings.settings();
    assert_eq!(state.io.value(ConnectorId::Send1Level), default);
    assert!(settings.io.out12_paired && settings.io.out34_paired);
    assert_eq!(settings.expression.pedals, [ExpressionRange::default(); 2]);
}

/// An I/O preset load ends with its EXP calibration (`0x3634e8`): rows 48/49 are EXP 1 MIN
/// and MAX, 50/51 EXP 2, as raw counts; a pedal without both rows keeps its range.
#[test]
fn io_preset_load_takes_the_preset_exp_calibration() {
    use tetrad_settings::io::ExpressionRange;
    use tetrad_storage::stock::ParamEntry;
    let mut state = state(Platform::QcEss);
    let row = |index, value: &str| ParamEntry {
        index,
        value: value.into(),
        exp: 0,
        exp_trim_max: 1.0,
        exp_trim_min: 0.0,
    };
    let rows = [row(48, "12"), row(49, "240"), row(50, "30")];
    tetrad_app::device::jacks::load_calibration(&mut state, &rows);
    assert_eq!(
        state.settings.settings().expression.pedals,
        [
            ExpressionRange { min: 12, max: 240 },
            ExpressionRange::default()
        ]
    );
}

fn io_row(index: i64, value: &str) -> tetrad_storage::stock::ParamEntry {
    tetrad_storage::stock::ParamEntry {
        index,
        value: value.into(),
        exp: 0,
        exp_trim_max: 1.0,
        exp_trim_min: 0.0,
    }
}

/// Model 31000 with IN 1 TYPE (connector 102) at index 0 and the two LINK rows at 46, 47.
fn io_specs() -> tetrad_catalog::Specs {
    let filler: String = (1..46)
        .map(|index| format!("<Parameter name='P{index}' max='1.0' min='0.0' type='float'/>"))
        .collect();
    let models = format!(
        "<Models><Category id='41' name='IO' hidden='true'>\
         <Model id='31000' name='IOSettings' hidden='true'>\
         <Parameter name='IN 1 TYPE' max='1.0' min='0.0' replaces='102' type='float'/>{filler}\
         <Parameter name='OUTPUT 1/2 LINK' max='1.0' min='0.0' type='float'/>\
         <Parameter name='OUTPUT 3/4 LINK' max='1.0' min='0.0' type='float'/>\
         </Model></Category></Models>"
    );
    let report = "<ModuleReport>\
        <Kernel core='0' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
        <Kernel core='1' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
        <Kernel core='2' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
        <Kernel core='3' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
        </ModuleReport>";
    tetrad_catalog::Specs::resolve(&tetrad_catalog::Catalog::parse(&models, report).unwrap())
        .unwrap()
}

/// Review r20 G1 and G3: both I/O preset loads (`0x3634e8`), the stored one and the factory
/// default (`0x3615a0`), set the input external mute and write nothing until 10 ms later
/// (`usleep(10000)` at `0x36352c`); the mute releases once, after the last write. A non-ESS
/// TYPE write inside skips its own mute wrapper (`+0x2ac`, `0x363514`), whose unmute would
/// release the load's mute early.
#[test]
fn io_preset_loads_write_only_after_a_10ms_mute_settle() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::reduce;
    let mut state = state(Platform::QcOriginal);
    state.specs = Some(io_specs());
    let stored = Action::LoadSettingsPreset {
        model: 31_000,
        rows: vec![io_row(0, "1"), io_row(46, "0")],
    };
    for load in [Action::LoadFactoryIoPreset, stored] {
        let settle = Effect::StartTimer {
            timer: Timer::IoPresetSettle,
            after: Duration::from_millis(10),
        };
        let effects = reduce(load, &mut state).unwrap();
        assert_eq!(effects, [external(true), settle, Effect::Present]);
        let effects = tetrad_app::device::jacks::settle_io_loads(&mut state);
        let mutes: Vec<_> = effects
            .iter()
            .filter(|e| matches!(e, Effect::ScheduleInput(InputChange::ExternalMute(_))))
            .collect();
        assert_eq!(mutes, [&external(false)]);
        let write = |e: &Effect| matches!(e, Effect::Hal(_) | Effect::ScheduleInput(_));
        assert!(effects.iter().any(|e| matches!(e, Effect::Hal(_))));
        let last = effects.iter().rposition(write).unwrap();
        assert_eq!(
            effects[last],
            external(false),
            "released after the last write"
        );
    }
}

/// Review r20 G6: an I/O reset (factory I/O preset, `0x3615a0`) and an I/O reload (stored
/// I/O preset, `0x360da8`) send Cortex Control one full IOSettings push (`0x26ebc0`), not
/// per-port or pair echoes; after the reset it carries both pairs linked.
#[test]
fn io_reset_and_reload_push_the_full_io_settings_once() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::reduce;
    use tetrad_app::device::control::io::push;
    let mut state = state(Platform::QcEss);
    state
        .settings
        .update(|settings| settings.io.out12_paired = false);
    reduce(Action::LoadFactoryIoPreset, &mut state).unwrap();
    let notifies = |effects: Vec<Effect>| -> Vec<Effect> {
        effects
            .into_iter()
            .filter(|e| matches!(e, Effect::ControlNotify(_)))
            .collect()
    };
    let reset = notifies(tetrad_app::device::jacks::settle_io_loads(&mut state));
    assert!(state.settings.settings().io.out12_paired);
    assert_eq!(reset, [push(&state)]);
    let reload = notifies(tetrad_app::device::jacks::reload(&mut state));
    assert_eq!(reload, [push(&state)]);
}

/// Parent decision on the review r20 G3 timer: while an I/O preset load waits for its 10 ms
/// settle, `+0x2ac` holds, so a non-ESS TYPE edit in that time writes without its own mute
/// wrapper; the input mute stays on until the load releases it.
#[test]
fn a_type_edit_in_the_settle_window_keeps_the_load_mute() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::reduce;
    use tetrad_settings::io::ConnectorId;
    let mut state = state(Platform::QcOriginal);
    reduce(Action::LoadFactoryIoPreset, &mut state).unwrap();
    let edit = tetrad_app::device::jacks::set(&mut state, ConnectorId::In1Type, 1.0).unwrap();
    let mutes = |effects: &[Effect]| -> Vec<Effect> {
        effects
            .iter()
            .filter(|e| matches!(e, Effect::ScheduleInput(InputChange::ExternalMute(_))))
            .cloned()
            .collect()
    };
    assert!(mutes(&edit).is_empty(), "the edit keeps the load's mute");
    assert!(edit.iter().any(|e| matches!(e, Effect::Hal(_))));
    let load = tetrad_app::device::jacks::settle_io_loads(&mut state);
    assert_eq!(mutes(&load), [external(false)]);
}

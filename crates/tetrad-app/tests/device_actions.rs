//! The shared action reducer (`device::actions::reduce`) over a deployed synthetic preset:
//! the contracts every source (input, MIDI, UI, Cortex Control) relies on.

use tetrad_app::device::actions::{Action, LayoutEdit, reduce};
use tetrad_app::device::preset::{SpecsCatalog, UndoPath, prepare};
use tetrad_app::device::ui::UiInput;
use tetrad_app::device::{Current, DspCommand, Effect, Library, PresetRef, State, Subsystem};
use tetrad_catalog::{Catalog, Specs};
use tetrad_core::performance::{FootswitchMode as CoreMode, StompIndex};
use tetrad_engine::{Column, Row, SceneIndex, Site, Slot};
use tetrad_hal::platform::{Platform as HalPlatform, Product};
use tetrad_library::stock::{StockLibrary, StockLimits};
use tetrad_preset::{Model, ParamValue, Preset, Slot as PresetSlot, StompAssignment};
use tetrad_settings::controls::FootswitchMode;
use tetrad_settings::device::{DeviceSettings, Hardware};
use tetrad_settings::{SettingsFileId, SettingsStorage};
use tetrad_storage::media::MediaRoot;
use tetrad_ui::Screen;

/// Synthetic catalog (no stock data), as in `tests/device_boot.rs`.
const MODELS: &str = "<Models>\
<Category id='0' name='Drive'>\
<Model id='1' name='Light' blob='light' secondary_stomp_parameter='0'>\
<Parameter name='GAIN' type='float' min='0' max='10' defaultValue='5'/></Model>\
<Model id='2' name='Heavy' blob='heavy'>\
<Parameter name='GAIN' type='float' min='0' max='10' defaultValue='5'/></Model></Category>\
<Category id='14' name='Capture'><Model id='14000' name='Capture' blob='capture'>\
<Parameter name='GAIN' type='float' min='0' max='1' defaultValue='0.5'/></Model>\
<Model id='14001' name='Capture 2' blob='capture'>\
<Parameter name='GAIN' type='float' min='0' max='1' defaultValue='0.5'/>\
<Parameter name='B' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='C' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='D' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='E' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='file name' type='string' defaultValue=''/></Model></Category>\
<Category id='10' name='Splitter' hidden='true'><Model id='10004' name='Splitter'>\
<Parameter name='TYPE' type='switch' min='0' max='2' defaultValue='0'/></Model>\
<Model id='10003' name='LR Crossover' blob='lr' hidden='true'>\
<Parameter name='FREQ' type='float' min='20' max='20000' defaultValue='20'/></Model></Category>\
<Category id='11' name='Mixer' hidden='true'><Model id='11000' name='Mixer'>\
<Parameter name='LEVEL A' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/>\
</Model></Category>\
<Category id='23' name='Lane Output' hidden='true'><Model id='23000' name='Lane Output'>\
<Parameter name='VOLUME' type='float' min='0' max='1' defaultValue='0.76923'/>\
<Parameter name='PAN' type='float' min='0' max='1' defaultValue='0.5'/></Model></Category>\
<Category id='25' name='Tempo' hidden='true'><Model id='25000' name='Tempo'>\
<Parameter name='TEMPO' type='float' min='40' max='240' defaultValue='120'/>\
<Parameter name='SCOPE' type='switch' min='0' max='1' defaultValue='0'/>\
<Parameter name='LED' type='switch' min='0' max='1' defaultValue='1'/></Model></Category>\
<Category id='28' name='Input Gate' hidden='true'><Model id='28000' name='Input Gate'>\
<Parameter name='THRESHOLD' type='float' min='-96' max='0' defaultValue='-60'/></Model></Category>\
</Models>";

const REPORT: &str = "<ModuleReport>\
<Kernel core='0' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='1' cpu='2' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='2' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='3' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Module id='1' name='Light' blob_name='light' category='0' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='2' name='Heavy' blob_name='heavy' category='0' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='14000' name='Capture' blob_name='capture' category='14' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='14001' name='Capture 2' blob_name='capture' category='14' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='10003' name='LR Crossover' blob_name='lr' category='10' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='1'/></Module>\
</ModuleReport>";

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

/// "Light" at row 1, column 3, assigned to stomp A, deployed in scene 1.
fn state() -> State {
    state_with(|_, _| {})
}

/// [`state`] with more stored edits before the deploy.
fn state_with(edit: impl FnOnce(&mut Preset, &SpecsCatalog<'_>)) -> State {
    state_in(MODELS, REPORT, edit)
}

/// [`state_with`] over another catalog.
fn state_in(
    models: &str,
    report: &str,
    edit: impl FnOnce(&mut Preset, &SpecsCatalog<'_>),
) -> State {
    let specs = Specs::resolve(&Catalog::parse(models, report).unwrap()).unwrap();
    let catalog = SpecsCatalog(&specs);
    let mut preset = Preset::new_default(tetrad_firmware::running().version, &catalog).unwrap();
    let block = Model::block(1, [ParamValue::Float(0.5), ParamValue::Float(0.0)]);
    preset
        .insert_block(PresetSlot { row: 0, column: 2 }, block, &catalog)
        .unwrap();
    preset.stomp_mode_assignments.push(StompAssignment {
        row: 0,
        column: 2,
        stomp_index: 0,
        ..StompAssignment::default()
    });
    edit(&mut preset, &catalog);
    let prepared = prepare(preset, &specs, false, None, Default::default()).unwrap();
    let (settings, _) = DeviceSettings::load(&NoFiles, Hardware::QuadCortex).unwrap();
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

fn light() -> Site {
    Site::Slot(Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    })
}

fn bypassed(state: &State) -> bool {
    let deployment = &state.current.as_ref().unwrap().deployment;
    let block = deployment.layout().instance(light()).unwrap();
    block.bypass.at(deployment.scene())
}

/// Non-forced `setScene` (`0x5899d8`): a new scene moves the deployment and asks the DSP for
/// the stock recall, which carries the metronome resend (`0x527248` → `0x512c74`); the
/// active scene does nothing.
#[test]
fn scene_selection_recalls_once_and_ignores_the_active_scene() {
    let mut state = state();
    let scene = SceneIndex::new(3).unwrap();
    let effects = reduce(Action::SelectScene(scene), &mut state).unwrap();
    let recalls = effects
        .iter()
        .filter(|effect| matches!(effect, Effect::RecallScene(_)))
        .count();
    assert_eq!(recalls, 1, "{effects:?}");
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Dsp(_) | Effect::MetronomeResendPlan(_))),
        "the resend runs inside the recall: {effects:?}"
    );
    let current = state.current.as_ref().unwrap();
    assert_eq!(current.deployment.scene(), scene);
    assert!(
        reduce(Action::SelectScene(scene), &mut state)
            .unwrap()
            .is_empty()
    );
}

/// Review final-device D4: a settings reset sends what the DSP holds from the reset groups:
/// the tuner input path with the lane mutes, then the tempo `0x16` with the metronome resend.
#[test]
fn settings_reset_resends_the_tuner_path_and_the_tempo() {
    use tetrad_dsp_protocol::tuner_looper::TunerLooperRequest;
    use tetrad_settings::device::ResetKind;
    let mut state = state();
    state.settings.update(|settings| {
        settings.tuner.live_tuner = true;
        settings.tuner.mutes_outputs = true;
        settings.tempo.params.insert(0, 0.5);
    });
    let effects = reduce(Action::Reset(ResetKind::Settings), &mut state).unwrap();
    let position = |wanted: fn(&Effect) -> bool| {
        let found = effects.iter().position(wanted);
        found.unwrap_or_else(|| panic!("{effects:?}"))
    };
    let path = position(|effect| {
        matches!(
            effect,
            Effect::Dsp(DspCommand::TunerLooper(
                TunerLooperRequest::GlobalTunerInputPath { .. }
            ))
        )
    });
    let mutes = position(|effect| matches!(effect, Effect::Dsp(DspCommand::TunerMute)));
    let tempo = position(|effect| matches!(effect, Effect::Dsp(DspCommand::Tempo(_))));
    let resend = position(|effect| matches!(effect, Effect::Action(Action::MetronomeResend)));
    assert!(path < mutes && tempo < resend, "{effects:?}");
    assert!(!state.tuner_muted, "the reset live tuner is off");
}

/// Review w5 R1: a reset runs the I/O reset (`0x3615a0`). The live values, the DSP and the
/// stored file all take the defaults, so a pending `ConnectorsSave` cannot write an old value
/// back; the reset pairs OUT 1/2, so outputs 0 and 1 follow the pair level.
#[test]
fn settings_reset_resets_the_live_io_and_stores_the_defaults() {
    use tetrad_dsp_protocol::requests::Request;
    use tetrad_settings::device::ResetKind;
    use tetrad_settings::io::ConnectorId;
    let mut state = state();
    let edit = Action::SetConnector {
        id: ConnectorId::Out1Level,
        value: 0.2,
    };
    reduce(edit, &mut state).unwrap();
    let effects = reduce(Action::Reset(ResetKind::Settings), &mut state).unwrap();
    let unity = 40.0 / 52.0;
    assert_eq!(state.io.value(ConnectorId::Out1Level), unity);
    let stored = &state.settings.settings().io.params;
    assert_eq!(stored.get(&1000).and_then(|param| param.value), Some(unity));
    assert_eq!(stored.len(), 48);
    let gains: Vec<_> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Dsp(DspCommand::Request(Request::SetOutputGain { output, gain_bits })) => {
                Some((*output, f32::from_bits(*gain_bits)))
            }
            _ => None,
        })
        .collect();
    assert!(gains.contains(&(0, 1.0)), "{gains:?}");
    assert!(gains.iter().all(|&(_, gain)| gain == 1.0), "{gains:?}");
}

/// Review w5 R9: an undo whose frame changes the layout leaves the history alone; the
/// device moves the frame only when the deploy commits (`Effect::RestoreLayout`).
#[test]
fn layout_undo_keeps_its_frame_until_the_deploy_commits() {
    let mut state = state();
    reduce(Action::RecordUndo(UndoPath::View), &mut state).unwrap();
    let edit = Action::SetParameter {
        site: light(),
        index: 0,
        value: 0.9,
    };
    reduce(edit, &mut state).unwrap();
    let effects = reduce(Action::Undo, &mut state).unwrap();
    assert!(
        matches!(effects[0], Effect::RestoreLayout { redo: false, .. }),
        "{effects:?}"
    );
    assert!(state.history.can_undo() && !state.history.can_redo());
}

/// `0x527d80` runs the library pass `0x7c2da4` on every preset it takes, undo included
/// (review-w6 C10): a frame whose capture block (parameter 5 = 64-unit key + name) holds an
/// old name comes back with the library name in every scene.
#[test]
fn undo_restores_a_capture_block_with_its_library_name() {
    use tetrad_preset::{Param, SceneValue};
    let key = "k".repeat(64);
    let text = |name: &str| Some(ParamValue::String(format!("{key}{name}")));
    let mut state = state();
    let current = state.current.get_mut().as_mut().unwrap();
    // Stored only: the layout stays the same, so the restore takes the frame's preset.
    current.preset.chains[0].models.push(Model {
        hash: Some(14_000),
        params: (0..6)
            .map(|index| Param {
                values: vec![
                    SceneValue {
                        value: (index == 5).then(|| text("Old")).flatten(),
                        ..Default::default()
                    };
                    tetrad_preset::SCENES
                ],
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    });
    reduce(Action::RecordUndo(UndoPath::View), &mut state).unwrap();
    state.current.get_mut().as_mut().unwrap().preset.name = Some("Edited".into());
    state.capture_names = [(key.clone(), "New".to_owned())].into();

    reduce(Action::Undo, &mut state).unwrap();
    let preset = &state.current.as_ref().unwrap().preset;
    assert_ne!(preset.name.as_deref(), Some("Edited"));
    let capture = preset.chains[0].models.last().unwrap();
    assert!(
        capture.params[5]
            .values
            .iter()
            .all(|scene| scene.value == text("New"))
    );
}

/// Review final-device D5: MIDI queues the clock BPM after every byte and the Looper poll
/// repeats its status, so only a change presents a frame.
#[test]
fn unchanged_clock_bpm_and_looper_status_present_nothing() {
    use tetrad_dsp_protocol::responses::LooperStatus;
    let mut state = state();
    let status = LooperStatus::from_words([0; 22]);
    for action in [Action::ClockTempo(Some(120)), Action::LooperStatus(status)] {
        let first = reduce(action.clone(), &mut state).unwrap();
        assert!(
            first.iter().any(|e| matches!(e, Effect::Present)),
            "{action:?}"
        );
        assert!(reduce(action, &mut state).unwrap().is_empty());
    }
}

/// Review final-device D7: the IO page jack settings reach the jack scheduler with the
/// h-hal values: impedance index 0..=8, TS / XLR / XLR+48V, `dB = v·72 − 12` on the QC.
#[test]
fn io_page_jack_settings_schedule_the_jack_changes() {
    use tetrad_app::device::jacks::InputChange;
    use tetrad_hal::analog::GainEndpoint;
    use tetrad_hal::codec::Impedance;
    use tetrad_hal::hal::Command as HalCommand;
    use tetrad_hal::zenjack::{Control, GainPlan, Input, InputType, Value};
    use tetrad_ui::{InputJack, SettingId, SettingValue};
    let mut state = state();
    let mut change = |id, value| reduce(Action::ChangeSetting { id, value }, &mut state).unwrap();
    // QC (no ESS codec): TYPE is a direct write inside the external mute (`0x35b494`).
    let type_write = |effects: &[Effect]| {
        effects.iter().find_map(|effect| match effect {
            Effect::Hal(HalCommand::Set(setting, _)) => {
                match (setting.control(), setting.value()) {
                    (Control::InputType(input), Value::InputType(kind)) => Some((input, kind)),
                    _ => None,
                }
            }
            _ => None,
        })
    };
    // A non-ESS QC refuses PHANTOM on a TS jack (`0x35f75c`); TYPE Mic (XLR) comes first.
    let phantom = change(SettingId::Phantom(InputJack::In1), SettingValue::Bool(true));
    assert_eq!(type_write(&phantom), None);
    let mic = change(
        SettingId::InputType(InputJack::In1),
        SettingValue::Choice(1),
    );
    assert!(matches!(
        mic.first(),
        Some(Effect::ScheduleInput(InputChange::ExternalMute(true)))
    ));
    assert_eq!(type_write(&mic), Some((Input::One, InputType::Xlr)));
    let phantom = change(SettingId::Phantom(InputJack::In1), SettingValue::Bool(true));
    assert_eq!(
        type_write(&phantom),
        Some((Input::One, InputType::XlrPhantom))
    );
    let mut scheduled = |id, value| {
        change(id, value)
            .into_iter()
            .find_map(|effect| match effect {
                Effect::ScheduleInput(change) => Some(change),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(
        scheduled(
            SettingId::InputImpedance(InputJack::In2),
            SettingValue::Choice(8)
        ),
        InputChange::Impedance {
            input: Input::Two,
            impedance: Impedance::try_from(8).unwrap(),
            verification: tetrad_hal::analog::Verification::Unverified,
        }
    );
    assert_eq!(
        scheduled(
            SettingId::InputLevel(InputJack::In1),
            SettingValue::Number(240)
        ),
        InputChange::Gain {
            endpoint: GainEndpoint::Input1,
            plan: GainPlan::from_unit(HalPlatform::QcOriginal, 0.5).unwrap(),
            verification: tetrad_hal::analog::Verification::Unverified,
        }
    );
}

/// Stomp press `0x5239d0`: an engaged first-pass block (category 0) switches off, then the
/// 10 ms settle; each other block toggles its own bypass, and a SECONDARY block toggles its
/// secondary stomp parameter instead (a saved write). A latching stomp ignores its release.
#[test]
fn stomp_press_switches_first_pass_blocks_off_then_toggles_each_block() {
    use tetrad_preset::StompRole;
    let mut state = state_with(|preset, catalog| {
        for (column, role) in [(3, StompRole::Primary), (4, StompRole::Secondary)] {
            let at = PresetSlot { row: 0, column };
            let block = Model::block(1, [ParamValue::Float(0.5), ParamValue::Float(0.0)]);
            preset.insert_block(at, block, catalog).unwrap();
            preset
                .block_bypass_mut(at)
                .unwrap()
                .set_bypassed(0, true)
                .unwrap();
            preset.stomp_mode_assignments.push(StompAssignment {
                row: 0,
                column: column as u32,
                stomp_index: 0,
                role: Some(role),
                ..StompAssignment::default()
            });
        }
    });
    let stomp = StompIndex::new(0).unwrap();
    let effects = reduce(Action::ToggleStomp(stomp), &mut state).unwrap();
    assert!(
        matches!(
            &effects[..],
            [Effect::Deploy(_), Effect::Sleep(settle), Effect::Deploy(_), Effect::Deploy(_), Effect::Present]
                if *settle == std::time::Duration::from_millis(10)
        ),
        "{effects:?}"
    );
    let current = state.current.as_ref().unwrap();
    let scene = current.deployment.scene();
    let block = |column| {
        current
            .deployment
            .layout()
            .instance(Site::Slot(Slot {
                row: Row::new(0).unwrap(),
                column: Column::new(column).unwrap(),
            }))
            .unwrap()
    };
    let bypass: Vec<bool> = [2, 3, 4]
        .map(|column| block(column).bypass.at(scene))
        .into();
    assert_eq!(bypass, [true, false, true]);
    assert_eq!(block(4).params[0].at(scene), 1.0);
    assert!(
        reduce(Action::ReleaseStomp(stomp), &mut state)
            .unwrap()
            .is_empty()
    );
}

/// Scene Bypass Behaviour NonStomp (`bypassSaveMode` 1, `0x2e8160`): a footswitch toggle plays
/// at once and is gone after a scene change (`0x2e0e38(preset, 0)`), in the deployment and
/// the stored preset; a touch toggle commits and stays.
#[test]
fn nonstomp_footswitch_toggles_revert_on_scene_change_and_touch_toggles_stay() {
    use tetrad_settings::general::BypassSaveMode;
    let mut state = state();
    state
        .settings
        .update(|settings| settings.general.bypass_save_mode = BypassSaveMode::NonStomp);
    let stored = |state: &mut State| {
        let current = state.current.get_mut().as_mut().unwrap();
        let scene = current.deployment.scene().index();
        let record = current
            .preset
            .block_bypass_mut(PresetSlot { row: 0, column: 2 })
            .unwrap();
        record.bypassed(scene).unwrap()
    };
    reduce(Action::ToggleStomp(StompIndex::new(0).unwrap()), &mut state).unwrap();
    assert!(bypassed(&state) && stored(&mut state));
    reduce(Action::SelectScene(SceneIndex::new(2).unwrap()), &mut state).unwrap();
    assert!(!bypassed(&state) && !stored(&mut state));

    let touch = Action::SetBypass {
        site: light(),
        bypassed: true,
    };
    reduce(touch, &mut state).unwrap();
    reduce(Action::SelectScene(SceneIndex::new(3).unwrap()), &mut state).unwrap();
    assert!(bypassed(&state) && stored(&mut state));
}

/// Review r14 P-2: the committed copy lives in the block model (`0x2e85fc`), so it moves with
/// a moved block and dies with a removed one; a scene change then reverts the moved block and
/// raises no notice for the empty slot.
#[test]
fn bypass_commits_follow_moved_and_removed_blocks() {
    use tetrad_settings::general::BypassSaveMode;
    let mut state = state();
    state
        .settings
        .update(|settings| settings.general.bypass_save_mode = BypassSaveMode::NonStomp);
    let to = Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(5).unwrap(),
    };
    let Site::Slot(from) = light() else {
        unreachable!()
    };
    let edit = |state: &mut State, edit| {
        let effects = reduce(Action::EditLayout(edit), state).unwrap();
        let Some(Effect::DeployLayout(change)) = effects.into_iter().next() else {
            panic!("a layout change");
        };
        change.commit(state.current.get_mut().as_mut().unwrap());
    };
    let notices = |effects: &[Effect]| {
        effects
            .iter()
            .filter(|effect| {
                matches!(
                    effect,
                    Effect::Notice(tetrad_app::device::Subsystem::Preset, _)
                )
            })
            .count()
    };
    reduce(Action::ToggleStomp(StompIndex::new(0).unwrap()), &mut state).unwrap();
    edit(&mut state, LayoutEdit::Move { from, to });
    let effects = reduce(Action::SelectScene(SceneIndex::new(2).unwrap()), &mut state).unwrap();
    assert_eq!(notices(&effects), 0, "{effects:?}");
    let current = state.current.as_ref().unwrap();
    let moved = current
        .deployment
        .layout()
        .instance(Site::Slot(to))
        .unwrap();
    assert!(!moved.bypass.at(current.deployment.scene()));

    reduce(Action::ToggleStomp(StompIndex::new(0).unwrap()), &mut state).unwrap();
    edit(&mut state, LayoutEdit::Remove(to));
    let effects = reduce(Action::SelectScene(SceneIndex::new(3).unwrap()), &mut state).unwrap();
    assert_eq!(notices(&effects), 0, "{effects:?}");
}

fn slot(row: u8, column: u8) -> Slot {
    Slot {
        row: Row::new(row).unwrap(),
        column: Column::new(column).unwrap(),
    }
}

/// Runs a layout action and commits its deploy.
fn commit_layout(state: &mut State, action: Action) {
    let effects = reduce(action, state).unwrap();
    let change = effects.into_iter().find_map(|effect| match effect {
        Effect::DeployLayout(change) => Some(change),
        _ => None,
    });
    change
        .expect("a layout change")
        .commit(state.current.get_mut().as_mut().unwrap());
}

/// Hardware B1 (`sl-2.png`), stock `Chain::dropSlot` (`0x59f620` → `0x59f1cc`): a row-1
/// block dropped in empty row 2 makes a parallel path in one deploy: split at column 0, mix
/// at column 7, stored on chain 0 with the block, its record and its stomp. The last row-2
/// block dropped back on row 1 clears both points.
#[test]
fn a_drop_into_path_b_stores_and_deploys_a_parallel_path() {
    let mut state = state();
    let (from, to) = (slot(0, 2), slot(1, 4));
    commit_layout(
        &mut state,
        Action::EditLayout(LayoutEdit::Move { from, to }),
    );
    let current = state.current.as_ref().unwrap();
    let pair = current.deployment.layout().pair(tetrad_engine::Pair::Upper);
    assert_eq!(
        (pair.split, pair.merge),
        (Some(slot(0, 0).column), Some(slot(0, 7).column))
    );
    assert!(pair.splitter.is_some() && pair.mixer.is_some());
    let stored = &current.preset;
    let points = &stored.chains[0].split_control_points[0];
    assert_eq!((points.split, points.mix), (0, 7));
    let at = PresetSlot { row: 1, column: 4 };
    assert_eq!(stored.block(at).unwrap().unwrap().hash, Some(1));
    let stomp = &stored.stomp_mode_assignments[0];
    assert_eq!((stomp.row, stomp.column), (1, 4));

    let back = slot(0, 1);
    commit_layout(
        &mut state,
        Action::EditLayout(LayoutEdit::Move { from: to, to: back }),
    );
    let current = state.current.as_ref().unwrap();
    let pair = current.deployment.layout().pair(tetrad_engine::Pair::Upper);
    assert_eq!((pair.split, pair.merge), (None, None));
    let points = &current.preset.chains[0].split_control_points[0];
    assert_eq!((points.split, points.mix), (-1, -1));
}

/// A drop on a block makes room (`ChainItemContainer` `0x5c09c8`): in its own row the
/// source slot is the gap, so the two blocks trade places in the layout and the file. A drop
/// on its own slot changes nothing.
#[test]
fn a_drop_on_a_block_makes_room_in_its_row() {
    let mut state = state_with(|preset, catalog| {
        let heavy = Model::block(2, [ParamValue::Float(0.5), ParamValue::Float(0.0)]);
        let at = PresetSlot { row: 0, column: 3 };
        preset.insert_block(at, heavy, catalog).unwrap();
    });
    let (from, to) = (slot(0, 3), slot(0, 2));
    commit_layout(
        &mut state,
        Action::EditLayout(LayoutEdit::Move { from, to }),
    );
    let current = state.current.as_ref().unwrap();
    let model = |column| {
        let at = PresetSlot { row: 0, column };
        current.preset.block(at).unwrap().map(|block| block.hash)
    };
    assert_eq!((model(2), model(3)), (Some(Some(2)), Some(Some(1))));
    let layout = current.deployment.layout();
    assert_eq!(layout.instance(Site::Slot(to)).unwrap().model.0, 2);
    let stomp = &current.preset.stomp_mode_assignments[0];
    assert_eq!((stomp.row, stomp.column), (0, 3), "stomp A follows Light");

    let refused = Action::EditLayout(LayoutEdit::Move { from: to, to });
    let effects = reduce(refused, &mut state).unwrap();
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::DeployLayout(_)))
    );
}

/// The half of the target block decides where its row makes room (r50 grid drag, touch):
/// a row-3 block dropped on the left half of the Light at row 1 column 2 pushes it and the
/// block at column 3 right (`0x5c09c8`); on the right half the Light moves left to the free
/// column 1 (`0x5c0ee0`). The dropped block takes column 2 either way.
#[test]
fn a_drop_makes_room_on_the_side_of_the_half_under_the_finger() {
    use tetrad_engine::DropSide;
    let start = || {
        state_with(|preset, catalog| {
            for (row, column) in [(0, 3), (2, 0)] {
                let heavy = Model::block(2, [ParamValue::Float(0.5), ParamValue::Float(0.0)]);
                preset
                    .insert_block(PresetSlot { row, column }, heavy, catalog)
                    .unwrap();
            }
        })
    };
    let columns = |state: &State| {
        let layout = state.current.as_ref().unwrap().deployment.layout();
        (0..8)
            .map(|column| {
                layout
                    .instance(Site::Slot(slot(0, column)))
                    .map(|block| block.model.0)
            })
            .collect::<Vec<_>>()
    };
    let (from, to) = (slot(2, 0), slot(0, 2));
    let mut left = start();
    commit_layout(
        &mut left,
        Action::DropBlock {
            from,
            to,
            side: DropSide::Left,
        },
    );
    assert_eq!(
        columns(&left)[1..5],
        [None, Some(2), Some(1), Some(2)],
        "Light and its right neighbour move right"
    );
    let mut right = start();
    commit_layout(
        &mut right,
        Action::DropBlock {
            from,
            to,
            side: DropSide::Right,
        },
    );
    assert_eq!(
        columns(&right)[1..5],
        [Some(1), Some(2), Some(2), None],
        "Light moves left"
    );
}

/// Review r14 P-3: a preset load while a momentary stomp is held sets `+0x3ab` (`0x527d80`);
/// the release then changes nothing in the new preset and clears it (`0x523f80`), and a press
/// clears it too (`0x5239d0`).
#[test]
fn momentary_release_after_a_preset_load_does_nothing_once() {
    let mut state = state();
    let stomp = StompIndex::new(0).unwrap();
    state
        .current
        .get_mut()
        .as_mut()
        .unwrap()
        .preset
        .stomp_is_momentary
        .insert(0, true);
    reduce(Action::ToggleStomp(stomp), &mut state).unwrap();
    state.perform.preset_loaded = true;
    let before = bypassed(&state);
    assert!(
        reduce(Action::ReleaseStomp(stomp), &mut state)
            .unwrap()
            .is_empty()
    );
    assert_eq!(bypassed(&state), before);
    assert!(!state.perform.preset_loaded);
    assert!(
        !reduce(Action::ReleaseStomp(stomp), &mut state)
            .unwrap()
            .is_empty()
    );
}

/// Review r15 R13: only a release that skips a momentary toggle clears `+0x3ab`
/// (`0x523f80`). With momentary A and plain B held over a load, the release of B keeps the
/// flag, so the release of A still changes nothing in the new preset.
#[test]
fn preset_load_with_two_held_stomps_keeps_the_momentary_release_quiet() {
    let mut state = state();
    let (a, b) = (StompIndex::new(0).unwrap(), StompIndex::new(1).unwrap());
    let current = state.current.get_mut().as_mut().unwrap();
    current.preset.stomp_is_momentary.insert(0, true);
    reduce(Action::ToggleStomp(a), &mut state).unwrap();
    reduce(Action::ToggleStomp(b), &mut state).unwrap();
    state.perform.preset_loaded = true;
    let before = bypassed(&state);
    reduce(Action::ReleaseStomp(b), &mut state).unwrap();
    assert!(state.perform.preset_loaded);
    assert!(
        reduce(Action::ReleaseStomp(a), &mut state)
            .unwrap()
            .is_empty()
    );
    assert_eq!(bypassed(&state), before);
    assert!(!state.perform.preset_loaded);
}

/// Review r15 R14: a layout edit that fails to prepare leaves the bypass commits as they
/// were (they follow only an edit that changes the layout).
#[test]
fn failed_layout_edit_keeps_the_bypass_commits() {
    use tetrad_settings::general::BypassSaveMode;
    let mut state = state();
    state
        .settings
        .update(|settings| settings.general.bypass_save_mode = BypassSaveMode::NonStomp);
    reduce(Action::ToggleStomp(StompIndex::new(0).unwrap()), &mut state).unwrap();
    let commits = state.perform.bypass.clone();
    assert!(!commits.is_empty());
    let Site::Slot(slot) = light() else {
        unreachable!()
    };
    let unknown = LayoutEdit::Replace { slot, model: 999 };
    assert!(reduce(Action::EditLayout(unknown.clone()), &mut state).is_err());
    assert!(reduce(Action::EditLayouts(vec![unknown]), &mut state).is_err());
    assert_eq!(state.perform.bypass, commits);
}

/// "Stomp Mode Bypass Assignment" (`0x522d70` → `0x510d70`): an added block takes the first
/// stomp 0..=7 without assignments as PRIMARY.
#[test]
fn auto_assign_gives_an_added_block_the_first_free_stomp() {
    let mut state = state();
    state
        .settings
        .update(|settings| settings.general.stomp_mode_auto_assign = true);
    let slot = Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(5).unwrap(),
    };
    let effects = reduce(
        Action::EditLayout(LayoutEdit::Add { slot, model: 1 }),
        &mut state,
    )
    .unwrap();
    let Some(Effect::DeployLayout(change)) = effects.first() else {
        panic!("{effects:?}");
    };
    let assigned: Vec<_> = change
        .preset()
        .stomp_mode_assignments
        .iter()
        .map(|assignment| (assignment.column, assignment.stomp_index))
        .collect();
    assert_eq!(assigned, [(2, 0), (5, 1)]);
}

/// Review r14 P-7 (r15 R18): `0x522d70` tests PRIMARY and SECONDARY apart, so a block with
/// only a SECONDARY assignment takes the first free stomp as PRIMARY after a model change.
#[test]
fn auto_assign_gives_a_secondary_only_block_a_primary_stomp() {
    use tetrad_preset::StompRole;
    let mut state = state_with(|preset, catalog| {
        let block = Model::block(14000, [ParamValue::Float(0.5), ParamValue::Float(0.0)]);
        let at = PresetSlot { row: 0, column: 4 };
        preset.insert_block(at, block, catalog).unwrap();
        preset.stomp_mode_assignments.push(StompAssignment {
            row: 0,
            column: 4,
            stomp_index: 3,
            role: Some(StompRole::Secondary),
            ..StompAssignment::default()
        });
    });
    state
        .settings
        .update(|settings| settings.general.stomp_mode_auto_assign = true);
    let slot = Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(4).unwrap(),
    };
    let replace = Action::EditLayout(LayoutEdit::Replace { slot, model: 1 });
    let effects = reduce(replace, &mut state).unwrap();
    let Some(Effect::DeployLayout(change)) = effects.first() else {
        panic!("{effects:?}");
    };
    let mut assigned: Vec<_> = (change.preset().stomp_mode_assignments.iter())
        .filter(|assignment| assignment.column == 4)
        .map(|assignment| (assignment.stomp_index, assignment.role))
        .collect();
    assigned.sort_by_key(|&(stomp, _)| stomp);
    let primary = |role| matches!(role, None | Some(StompRole::Primary));
    assert_eq!(assigned.len(), 2, "{assigned:?}");
    assert!(assigned[0].0 == 1 && primary(assigned[0].1), "{assigned:?}");
    assert_eq!(assigned[1], (3, Some(StompRole::Secondary)));
}

/// Review r14 P-5, P-6 and P-8 (r15 R18): a press on a stomp without blocks writes nothing
/// and raises no notice (`0x5239d0`); BANK UP goes to the UI as a view input; an assignment
/// at column 4004 (`0x2fa254`, target not recovered) gives an explicit Preset notice.
#[test]
fn empty_stomp_bank_encoder_and_column_4004_follow_the_contract() {
    use tetrad_app::device::Subsystem;
    let mut state = state_with(|preset, _| {
        preset.stomp_mode_assignments.push(StompAssignment {
            row: 0,
            column: 0xfa4,
            stomp_index: 6,
            ..StompAssignment::default()
        });
    });
    let empty = reduce(Action::ToggleStomp(StompIndex::new(5).unwrap()), &mut state).unwrap();
    assert!(
        !empty
            .iter()
            .any(|effect| matches!(effect, Effect::Deploy(_) | Effect::Notice(..))),
        "{empty:?}"
    );
    let bank = reduce(Action::BankEncoder(-2), &mut state).unwrap();
    assert_eq!(
        bank[0],
        Effect::UiInput(UiInput::View(tetrad_ui::InputEvent::BankEncoder {
            delta: -2
        }))
    );
    let special = reduce(Action::ToggleStomp(StompIndex::new(6).unwrap()), &mut state).unwrap();
    assert!(
        special
            .iter()
            .any(|effect| matches!(effect, Effect::Notice(Subsystem::Preset, _))),
        "{special:?}"
    );
}

/// Scene mode on a block's MUTE keeps the stored flag (`0x513fc8`); on a junction BYPASS it
/// couples the splitter and the mixer (`0x517f54`).
#[test]
fn mute_and_junction_bypass_take_scene_mode() {
    use tetrad_app::device::preset::PresetEdit;
    use tetrad_engine::Pair;
    let mut state = state();
    let edit = |site, index| {
        Action::PresetEdit(PresetEdit::SceneMode {
            site,
            index,
            enabled: true,
        })
    };
    let change = |effects: Vec<Effect>| match effects.into_iter().next() {
        Some(Effect::DeployLayout(change)) => change,
        other => panic!("{other:?}"),
    };
    let mute = change(reduce(edit(light(), 1), &mut state).unwrap());
    let stored = &mute.preset().chains[0].models[2].params[1];
    assert_eq!(stored.scene_mode, Some(true));

    let mixer = Site::Mixer(Pair::Upper);
    let index = state
        .current
        .as_ref()
        .unwrap()
        .deployment
        .layout()
        .instance(mixer)
        .unwrap()
        .bypass_index();
    let junction = change(reduce(edit(mixer, index), &mut state).unwrap());
    let layout = junction.deployment().layout();
    for site in [Site::Splitter(Pair::Upper), mixer] {
        assert!(layout.instance(site).unwrap().bypass_scene_mode, "{site:?}");
    }
}

/// BANK UP device cycling ([`Action::CycleModel`]): the next model of the block's category
/// in id order, wrapping (inferred order).
#[test]
fn device_cycling_steps_through_the_category_and_wraps() {
    use tetrad_app::device::preset::cycled_model;
    let state = state();
    let specs = state.specs.as_ref().unwrap();
    assert_eq!(cycled_model(specs, 1, 1), Some(2));
    assert_eq!(cycled_model(specs, 1, 2), Some(1));
    assert_eq!(cycled_model(specs, 1, -1), Some(2));
}

/// Review r15 R7: BANK UP reads only the sign of the accelerated turn (`0x4f5f38`), so a
/// fast turn (+2) steps one model and does not skip back to the same one.
#[test]
fn device_cycling_takes_one_step_per_event() {
    let mut state = state();
    let slot = Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    };
    let effects = reduce(Action::CycleModel { slot, step: 2 }, &mut state).unwrap();
    let Some(Effect::DeployLayout(change)) = effects.into_iter().next() else {
        panic!("a model change");
    };
    let block = change.deployment().layout().instance(Site::Slot(slot));
    assert_eq!(block.map(|block| block.model.0), Some(2));
}

/// BANK UP on a Neural Capture block (`0x4f5f38` → `0x4f50dc`): the next capture of the
/// block's version (14001: "2") in the Directory order of the root that holds the key
/// (`0x6f5ce0`, `0x808250`); both ends wrap. The new block of the same model holds the
/// capture in all eight scenes. Without the Directory order it stays refused (review r15 R3).
#[test]
fn capture_cycling_follows_the_directory_order_of_the_version_and_wraps() {
    use tetrad_app::device::actions::{ActionError, CaptureCycle, CycleCapture, CycleMiss};
    let capture = |id: char, version: &str| CycleCapture {
        value: format!("{}{id}", id.to_string().repeat(64)),
        version: version.to_owned(),
    };
    let mut state = state_with(|preset, catalog| {
        let at = PresetSlot { row: 0, column: 4 };
        let block = Model::block(14001, [ParamValue::Float(0.5)]);
        preset.insert_block(at, block, catalog).unwrap();
    });
    let slot = Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(4).unwrap(),
    };
    let text = capture('b', "2").value;
    let site = Site::Slot(slot);
    reduce(
        Action::SetText {
            site,
            index: 5,
            text,
        },
        &mut state,
    )
    .unwrap();
    let refused = reduce(Action::CycleModel { slot, step: 1 }, &mut state);
    assert!(
        matches!(refused, Err(ActionError::Unavailable(_))),
        "{refused:?}"
    );

    let order = CaptureCycle {
        library: ['a', 'b', 'c', 'd']
            .map(|id| capture(id, if id == 'c' { "1" } else { "2" }))
            .to_vec(),
        downloads: vec![capture('e', "2")],
    };
    let next = |id: char, backwards| {
        let key = id.to_string().repeat(64);
        (order.step(&key, "2", backwards)).map(|row| row.value.chars().last().unwrap())
    };
    assert_eq!(next('b', false), Ok('d'), "a version 1 row is skipped");
    assert_eq!(next('d', false), Ok('a'));
    assert_eq!(next('a', true), Ok('d'));
    assert_eq!(next('e', false), Ok('e'), "the Downloads root walks alone");
    assert_eq!(next('c', false), Err(CycleMiss::NoNextKey));
    assert_eq!(next('z', false), Err(CycleMiss::NoFolder));

    reduce(Action::CaptureCycle(order), &mut state).unwrap();
    let effects = reduce(Action::CycleModel { slot, step: -2 }, &mut state).unwrap();
    let Some(Effect::DeployLayout(change)) = effects.into_iter().next() else {
        panic!("a block replace");
    };
    let block = change.deployment().layout().instance(site).unwrap();
    assert_eq!(block.model.0, 14001);
    let text = block.texts.iter().find(|text| text.index == 5).unwrap();
    assert_eq!(text.scenes, vec![Some(capture('a', "2").value); 8]);
}

/// The Parallax editor meter (`0x636740` 100 ms timer, `0x635fa0`, `0x636054`): while the
/// editor shows the block, each poll asks its core and slot for GAIN REDUCTION (word 41); the
/// linear reply is kept as `1 − |dB / 40|` for the meter. Closing the editor stops the poll.
#[test]
fn the_parallax_editor_polls_gain_reduction_into_the_model_meters() {
    use tetrad_app::device::{Timer, meters};
    use tetrad_dsp_protocol::requests::Request;
    use tetrad_dsp_protocol::tuner_looper::MeterRecord;
    let params: String = (0..41)
        .map(|index| format!("<Parameter name='P{index}' type='float' defaultValue='0'/>"))
        .chain(["<Parameter name='GR' type='grMeter' min='-40' max='0' defaultValue='0'/>".into()])
        .collect();
    let models = MODELS.replace(
        "</Models>",
        &format!("<Category id='3' name='Eq'><Model id='3008' name='Parallax' blob='light'>{params}</Model></Category></Models>"),
    );
    let report = REPORT.replace(
        "</ModuleReport>",
        "<Module id='3008' name='Parallax' blob_name='light' category='3' dm_heap='100' \
         pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
         <Test name='Stimulus High' avg='10'/></Module></ModuleReport>",
    );
    let mut state = state_in(&models, &report, |preset, catalog| {
        let at = PresetSlot { row: 0, column: 4 };
        let block = Model::block(3008, [ParamValue::Float(0.0)]);
        preset.insert_block(at, block, catalog).unwrap();
    });
    let slot = Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(4).unwrap(),
    };
    let opened = reduce(Action::EditorBlock(Some(slot)), &mut state).unwrap();
    let poll = Effect::StartTimer {
        timer: Timer::EditorMeters,
        after: meters::POLL,
    };
    assert_eq!(opened.first(), Some(&poll));
    let current = state.current.as_ref().unwrap();
    let placement = current.deployment.plan().block(slot).unwrap();
    let (core, column) = (placement.core.index(), placement.slot.index());
    let polls = meters::editor_requests(current, slot);
    let [Effect::Dsp(DspCommand::Request(Request::MeterRequest { slot: at, word, .. }))] =
        polls[..]
    else {
        panic!("one meter request: {polls:?}");
    };
    assert_eq!((usize::from(at), word), (column, 41));

    let record = MeterRecord {
        core: core as i32,
        column: column as i32,
        index: 41,
        value_bits: 0.1_f32.to_bits(),
    };
    assert!(meters::on_model_meter(&mut state, &record));
    let (_, values) = state.model_meters.cells().next().unwrap();
    assert!((values[&41] - 0.5).abs() < 1e-6, "{values:?}");
    let closed = reduce(Action::EditorBlock(None), &mut state).unwrap();
    assert_eq!(
        closed.first(),
        Some(&Effect::StopTimer(Timer::EditorMeters))
    );
}

/// MIDI CC#44 and the UI clamp to 40..=240; the value is stored and goes to the DSP tempo
/// path (drivers' tempo hooks, then `0x16`).
#[test]
fn tempo_is_clamped_stored_and_sent_to_the_drivers() {
    let mut state = state();
    let effects = reduce(Action::SetTempo(300.0), &mut state).unwrap();
    assert_eq!(tetrad_app::device::actions::tempo_bpm(&state), 240);
    assert_eq!(state.settings.settings().tempo.bpm(), None);
    assert!(effects.contains(&Effect::Dsp(DspCommand::Tempo(240))));
    let echo = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::ControlNotify(message) => match message.as_ref() {
                tetrad_control::codecs::ControlMessage::Grid(grid) => grid.preset.as_ref(),
                _ => None,
            },
            _ => None,
        })
        .expect("preset-scope tempo echo");
    let partial = Preset::decode(&echo.bytes, tetrad_preset::Limits::default()).unwrap();
    assert!(partial.chains.is_empty());
    let echoed = &partial.tempo_program_data[0].params[0];
    assert_eq!(echoed.index, Some(0));
    assert_eq!(echoed.values[0].value, Some(ParamValue::Float(1.0)));
    let current = state.current.get_mut().as_mut().unwrap();
    let param = &mut current.preset.tempo_program_data[0].params[0];
    assert!(
        param
            .values
            .iter()
            .all(|value| value.value == Some(ParamValue::Float(1.0)))
    );
    param.scene_mode = Some(true);
    reduce(Action::SelectScene(SceneIndex::new(3).unwrap()), &mut state).unwrap();
    reduce(Action::SetTempo(80.0), &mut state).unwrap();
    assert_eq!(tetrad_app::device::actions::tempo_bpm(&state), 80);
    let current = state.current.get_mut().as_mut().unwrap();
    assert_eq!(
        current.preset.tempo_program_data[0].params[0].value(0),
        Some(&ParamValue::Float(1.0))
    );
    current.preset.default_scene = Some(0);
    current.preset.tempo_program_data[0].params[0].scene_mode = Some(false);
    assert_eq!(
        tetrad_app::device::actions::tempo_bpm(&state),
        240,
        "non-scene mode reads the default scene"
    );
}

/// `0x556ed4` / `0x512c74`: 10 ms flush, ordered enable, and all 23 slots on recall.
#[test]
fn metronome_flush_and_resend_use_the_scoped_tempo_block() {
    use tetrad_app::device::{Timer, features};
    use tetrad_dsp_protocol::controls::ControlRequest;
    let mut state = state();
    let mut xml = MODELS.to_owned();
    let parameters: String = (3..23)
        .map(|index| match index {
            3 => {
                "<Parameter name='LEVEL' type='float' min='-60' max='0' defaultValue='-12'/>".into()
            }
            5 => "<Parameter name='PAN' type='float' min='-1' max='1' defaultValue='0'/>".into(),
            6 => "<Parameter name='SIGNATURE' type='switch' min='0' max='20' defaultValue='2'/>"
                .into(),
            7 => "<Parameter name='SUBDIVISION' type='switch' min='0' max='3' defaultValue='0'/>"
                .into(),
            9 => "<Parameter name='DESTINATION' type='switch' min='0' max='4' defaultValue='0'/>"
                .into(),
            _ => format!(
                "<Parameter name='P{index}' type='float' min='0' max='1' defaultValue='0'/>"
            ),
        })
        .collect();
    xml = xml.replace(
        "defaultValue='1'/></Model></Category>",
        &format!("defaultValue='1'/>{parameters}</Model></Category>"),
    );
    let specs = Specs::resolve(&Catalog::parse(&xml, REPORT).unwrap()).unwrap();
    let defaults: Vec<_> = specs.models[&25_000]
        .parameters
        .iter()
        .map(|param| ParamValue::Float(param.default_normalized()))
        .collect();
    state
        .current
        .get_mut()
        .as_mut()
        .unwrap()
        .preset
        .tempo_program_data = vec![Model::block(25_000, defaults)];
    state.specs = Some(specs);
    let effects = reduce(
        Action::SetTempoParameter {
            index: 4,
            value: 1.0,
            remote: false,
        },
        &mut state,
    )
    .unwrap();
    assert!(effects.contains(&Effect::StartTimer {
        timer: Timer::MetronomeFlush,
        after: features::METRONOME_FLUSH
    }));
    reduce(
        Action::SetTempoParameter {
            index: 3,
            value: 0.5,
            remote: false,
        },
        &mut state,
    )
    .unwrap();
    let flushed = reduce(Action::MetronomeFlush, &mut state).unwrap();
    let requests: Vec<_> = flushed
        .iter()
        .filter_map(|effect| {
            if let Effect::Dsp(DspCommand::Control(request)) = effect {
                Some(*request)
            } else {
                None
            }
        })
        .collect();
    assert!(matches!(
        requests.first(),
        Some(ControlRequest::MetronomeParameter {
            destination: 1,
            parameter: 3,
            ..
        })
    ));
    assert_eq!(
        requests.last(),
        Some(&ControlRequest::MetronomeParameter {
            destination: 4,
            parameter: 4,
            value_bits: 1.0_f32.to_bits()
        })
    );
    let resent = reduce(Action::MetronomeResend, &mut state).unwrap();
    let plan = resent
        .iter()
        .find_map(|effect| {
            if let Effect::MetronomeResendPlan(plan) = effect {
                Some(plan)
            } else {
                None
            }
        })
        .unwrap();
    assert!(
        plan.steps
            .iter()
            .any(|step| matches!(step, tetrad_app::tempo::ResendStep::Block(_)))
    );
    reduce(
        Action::MetronomeStatus(tetrad_dsp_protocol::responses::MetronomeStatus {
            words: [1, 0, 5, 2, 3, 0, 0],
        }),
        &mut state,
    )
    .unwrap();
    assert_eq!(state.metronome_status.unwrap().words[2], 5);
}

/// `setExpParameterValue` `0x520074` → `0x25f060`: each expression write to a block echoes
/// its one parameter to a connected Cortex Control at once (block sender `0x25d51c`), as a
/// Grid UPDATE the device drops while a capture preset runs; nothing goes out unconnected.
#[test]
fn expression_write_echoes_the_live_value_to_a_connected_control() {
    use tetrad_control::codecs::ControlMessage;
    use tetrad_hal::node::Pedal;
    let mut state = state();
    let block = (state.current.get_mut().as_mut().unwrap().preset)
        .block_mut(PresetSlot { row: 0, column: 2 })
        .unwrap();
    block.params[0].expression = Some(1);
    block.params[0].expression_min = Some(0.2);
    block.params[0].expression_max = Some(0.8);
    reduce(Action::PedalPlugs(0x80), &mut state).unwrap();
    let pedal = |state: &mut State, position| {
        let effects = reduce(
            Action::Expression {
                pedal: Pedal::One,
                position,
                from_midi: false,
            },
            state,
        )
        .unwrap();
        effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::GridEcho(message) => Some(*message),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert!(pedal(&mut state, 0.25).is_empty());
    state.control_connected = true;
    let [ControlMessage::Grid(echo)] = &pedal(&mut state, 0.5)[..] else {
        panic!("one Grid echo");
    };
    let bytes = &echo.preset.as_ref().unwrap().bytes;
    let preset = Preset::decode(bytes, tetrad_preset::Limits::default()).unwrap();
    let chain = &preset.chains[0];
    assert_eq!((chain.row, chain.models[0].column), (Some(0), Some(2)));
    let param = &chain.models[0].params[0];
    assert_eq!(param.index, Some(0));
    assert_eq!(param.values[0].value, Some(ParamValue::Float(0.5)));
}

/// `0x322cf0` / `0x6df618`: normalized live values and bypass delays never write scenes.
#[test]
fn expression_keeps_scene_storage_and_uses_each_bypass_delay() {
    use std::time::Duration;
    use tetrad_app::device::Timer;
    use tetrad_hal::node::Pedal;
    use tetrad_preset::{Expression, ExpressionBypassInfo};
    let mut state = state();
    let current = state.current.get_mut().as_mut().unwrap();
    let block = current
        .preset
        .block_mut(PresetSlot { row: 0, column: 2 })
        .unwrap();
    block.params[0].expression = Some(1);
    block.params[0].expression_min = Some(0.2);
    block.params[0].expression_max = Some(0.8);
    block.bypass_expression = vec![Expression {
        expression: 1,
        expression_min: 0.0,
        expression_max: 1.0,
        ..Default::default()
    }];
    block.expression_bypass_info.push(ExpressionBypassInfo {
        kind: 2,
        delay_ms: 30,
        ..Default::default()
    });
    let saved = current.preset.clone();
    let stored_layout = current.deployment.layout().clone();
    reduce(Action::PedalPlugs(0x80), &mut state).unwrap();
    let effects = reduce(
        Action::Expression {
            pedal: Pedal::One,
            position: 0.5,
            from_midi: false,
        },
        &mut state,
    )
    .unwrap();
    assert_eq!(state.expression.values[&(light(), 0)], 0.5);
    assert!(!state.expression.bypasses[&light()]);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Deploy(_)))
    );
    let effects = reduce(
        Action::Expression {
            pedal: Pedal::One,
            position: 0.0,
            from_midi: false,
        },
        &mut state,
    )
    .unwrap();
    assert!(effects.contains(&Effect::StartTimer {
        timer: Timer::ExpressionBypass,
        after: Duration::from_millis(31)
    }));
    reduce(Action::Time(Duration::from_millis(30)), &mut state).unwrap();
    reduce(Action::ExpressionPoll, &mut state).unwrap();
    assert!(!state.expression.bypasses[&light()]);
    reduce(Action::Time(Duration::from_millis(31)), &mut state).unwrap();
    reduce(Action::ExpressionPoll, &mut state).unwrap();
    assert!(state.expression.bypasses[&light()]);
    reduce(Action::SelectScene(SceneIndex::new(1).unwrap()), &mut state).unwrap();
    assert_eq!(state.expression.values[&(light(), 0)], 0.2);
    assert_eq!(state.current.as_ref().unwrap().preset, saved);
    assert_eq!(
        state.current.as_ref().unwrap().deployment.layout(),
        &stored_layout
    );
    let restore = reduce(Action::PedalPlugs(0), &mut state).unwrap();
    assert!(state.expression.values.is_empty());
    assert!(
        restore
            .iter()
            .any(|effect| matches!(effect, Effect::Deploy(_)))
    );
}

/// Review r11 N2: `0x3242ec` skips a pedal link on MUTE (the last stored parameter), so the
/// other links of the pedal still move.
#[test]
fn a_mute_link_is_skipped_and_the_other_links_still_move() {
    use tetrad_hal::node::Pedal;
    let mut state = state();
    let block = (state.current.get_mut().as_mut().unwrap().preset)
        .block_mut(PresetSlot { row: 0, column: 2 })
        .unwrap();
    let mute = block.params.len() - 1;
    block.params[0].expression = Some(1);
    block.params[mute].expression = Some(1);
    reduce(Action::PedalPlugs(0x80), &mut state).unwrap();
    let pedal = Action::Expression {
        pedal: Pedal::One,
        position: 0.5,
        from_midi: false,
    };
    reduce(pedal, &mut state).unwrap();
    assert_eq!(
        state.expression.values.keys().collect::<Vec<_>>(),
        [&(light(), 0)]
    );
}

/// Review r11 N8: an unknown stored switch type loads as the Heel-Toe default with a
/// notice, so the bypass link still follows the pedal.
#[test]
fn an_unknown_switch_type_loads_the_default_with_a_notice() {
    use tetrad_preset::{Expression, ExpressionBypassInfo};
    let mut state = state();
    let block = (state.current.get_mut().as_mut().unwrap().preset)
        .block_mut(PresetSlot { row: 0, column: 2 })
        .unwrap();
    block.bypass_expression = vec![Expression {
        expression: 1,
        expression_min: 0.0,
        expression_max: 1.0,
        ..Default::default()
    }];
    block.expression_bypass_info.push(ExpressionBypassInfo {
        kind: 7,
        ..Default::default()
    });
    let effects = reduce(Action::PedalPlugs(0x80), &mut state).unwrap();
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Notice(_, detail) if detail.contains("type 7"))),
        "{effects:?}"
    );
    assert!(state.expression.bypasses.contains_key(&light()));
}

/// The mode cycle walks the allowed list and wraps; the footswitch context follows the
/// stored mode.
#[test]
fn mode_cycle_wraps_the_allowed_list_and_moves_the_footswitch_mode() {
    let mut state = state();
    reduce(
        Action::SetAllowedModes(vec![FootswitchMode::Preset, FootswitchMode::Scene]),
        &mut state,
    )
    .unwrap();
    reduce(Action::SetMode(FootswitchMode::Scene), &mut state).unwrap();
    assert_eq!(state.performance.footswitch.mode, CoreMode::Scene);
    reduce(Action::CycleMode, &mut state).unwrap();
    assert_eq!(
        state.settings.settings().mode.mode(),
        FootswitchMode::Preset
    );
    assert_eq!(state.performance.footswitch.mode, CoreMode::Preset);
}

/// MIDI `ignore_if_current` skips the loaded slot; every other source reloads it.
#[test]
fn load_without_reload_skips_the_loaded_slot() {
    let mut state = state();
    let loaded = PresetRef {
        setlist: None,
        index: 0,
    };
    let quiet = Action::LoadPreset {
        target: loaded.clone(),
        reload: false,
    };
    assert!(reduce(quiet, &mut state).unwrap().is_empty());
    let forced = Action::LoadPreset {
        target: loaded.clone(),
        reload: true,
    };
    assert_eq!(
        reduce(forced, &mut state).unwrap(),
        [Effect::LoadPreset(loaded)]
    );
}

/// A setlist name and its directory path are one key (the stock `LastPreset/setlist` path
/// form): MIDI `ignore_if_current` by name skips the slot loaded by path, and a load stores
/// the path.
#[test]
fn setlist_names_load_as_their_directory_path() {
    let root = std::env::temp_dir().join(format!("tetrad-app-actions-key-{}", std::process::id()));
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut presets = StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap();
    presets.create_setlist("Live").unwrap();
    let path = presets.setlists().unwrap()[0].path.clone();
    let mut state = state();
    state.library = Some(Library {
        presets,
        database: None,
    });
    state
        .settings
        .update(|settings| settings.last_preset.setlist = Some(path.clone()));
    let by_name = |reload| Action::LoadPreset {
        target: PresetRef {
            setlist: Some("Live".into()),
            index: 0,
        },
        reload,
    };
    let quiet = reduce(by_name(false), &mut state).unwrap();
    let forced = reduce(by_name(true), &mut state).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    assert!(quiet.is_empty());
    let stored = PresetRef {
        setlist: Some(path),
        index: 0,
    };
    assert_eq!(forced, [Effect::LoadPreset(stored)]);
}

/// A device favourite toggle reaches Cortex Control through the list signals (`0x1aabe8`
/// added, `0x1abd00` removed): one-item RecentsFavorites CREATE, then DELETE.
#[test]
fn device_favourite_toggles_push_one_item_changes_to_control() {
    use tetrad_control::codecs::{ControlMessage, MessageAction};
    let root = std::env::temp_dir().join(format!("tetrad-app-actions-fav-{}", std::process::id()));
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let presets = StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap();
    let mut state = state();
    state.library = Some(Library {
        presets,
        database: None,
    });
    let path = "/media/p4/Presets/Live/Lead.pb".to_owned();
    let mut toggle = |favorite| {
        let effects = reduce(
            Action::SetFavorite {
                path: path.clone(),
                favorite,
            },
            &mut state,
        )
        .unwrap();
        effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::ControlNotify(message) => match *message {
                    ControlMessage::RecentsFavorites(push) => Some(push),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let added = toggle(true);
    let removed = toggle(false);
    std::fs::remove_dir_all(&root).unwrap();
    let [added] = &added[..] else {
        panic!("one CREATE: {added:?}");
    };
    assert_eq!(added.action, Some(MessageAction::Create.into()));
    assert_eq!(added.is_favorites, Some(true));
    assert_eq!(added.items[0].name.as_deref(), Some("Lead"));
    assert_eq!(added.items[0].product_key.as_deref(), Some(path.as_str()));
    let [removed] = &removed[..] else {
        panic!("one DELETE: {removed:?}");
    };
    assert_eq!(removed.action, Some(MessageAction::Delete.into()));
}

/// A screen request only asks the UI; `State::screen` follows the UI's report, and the tuner
/// screen's own DSP steps run on that report (the tuner engine `0x36` is not per screen).
#[test]
fn screen_requests_ask_the_ui_and_its_report_sets_state() {
    let mut state = state();
    let effects = reduce(Action::ShowTuner(true), &mut state).unwrap();
    assert_eq!(effects, [Effect::UiInput(UiInput::Show(Screen::Tuner))]);
    assert_eq!(state.screen, Screen::Grid);
    let effects = reduce(Action::ScreenShown(Screen::Tuner), &mut state).unwrap();
    assert_eq!(state.screen, Screen::Tuner);
    // `0x3f0710`: the metronome level goes to 0 first.
    assert_eq!(
        effects[0],
        Effect::Dsp(DspCommand::Control(
            tetrad_dsp_protocol::controls::ControlRequest::MetronomeParameter {
                destination: 1,
                parameter: 3,
                value_bits: 0,
            }
        ))
    );
    let effects = reduce(Action::ShowTuner(false), &mut state).unwrap();
    assert_eq!(effects, [Effect::UiInput(UiInput::Hide(Screen::Tuner))]);
}

/// Health r34 #9: only a write to the loaded preset moves its change count, so the 100 ms
/// PresetDirty check keeps its answer over screen changes and settings taps.
#[test]
fn ui_only_actions_keep_the_preset_change_count() {
    use tetrad_ui::{InputJack, SettingId, SettingValue};
    let mut state = state();
    let revision = state.current.revision();
    reduce(Action::ScreenShown(Screen::Tuner), &mut state).unwrap();
    let ground_lift = SettingId::InputGroundLift(InputJack::In1);
    let setting = Action::ChangeSetting {
        id: ground_lift,
        value: SettingValue::Bool(true),
    };
    reduce(setting, &mut state).unwrap();
    assert_eq!(state.current.revision(), revision);
    let edit = Action::SetParameter {
        site: light(),
        index: 0,
        value: 0.9,
    };
    reduce(edit, &mut state).unwrap();
    assert!(state.current.revision() > revision);
}

/// One LED frame per present pass: after the boot frame, a scene change writes only the
/// old and new scene LEDs (`leds::diff`, `0x50b548`), and the tempo LED stays quiet.
#[test]
fn scene_change_writes_only_the_changed_footswitch_leds() {
    use tetrad_app::device::leds::Leds;
    use tetrad_core::performance::leds::LOGICAL_TO_LED;
    use tetrad_hal::hal::Command;

    let mut state = state();
    state.performance.footswitch.mode = CoreMode::Scene;
    let mut leds = Leds::default();
    let boot = leds.update(&state);
    assert_eq!(boot.len(), 11 + 2, "every LED, the tempo colour and blink");
    let old = state.current.as_ref().unwrap().deployment.scene();
    let new = SceneIndex::new(if old.index() == 3 { 4 } else { 3 }).unwrap();
    reduce(Action::SelectScene(new), &mut state).unwrap();
    let mut written: Vec<u8> = leds
        .update(&state)
        .into_iter()
        .map(|effect| match effect {
            Effect::Hal(Command::ElementLed(led)) => led.index,
            other => panic!("unexpected LED effect {other:?}"),
        })
        .collect();
    written.sort_unstable();
    let mut expected = [LOGICAL_TO_LED[old.index()], LOGICAL_TO_LED[new.index()]];
    expected.sort_unstable();
    assert_eq!(written, expected);
}

/// Preset MIDI Out editor save (`0x3c1a7c`): one whole list goes into the loaded preset.
/// A footswitch list fills its field 33 source (slot 0 mirrored into legacy field 17), the
/// rest of the source is emptied, and the MIDI controller resets that source's CC Toggle
/// states. Nothing goes out on MIDI.
#[test]
fn preset_midi_out_edit_stores_the_list_and_resets_its_source() {
    use tetrad_ui::{Footswitch, MidiMessageKind, MidiOutMessage, MidiOutTarget};
    let mut state = state();
    let toggle = MidiOutMessage {
        kind: MidiMessageKind::CcToggle,
        channel: 2,
        values: [Some(64), Some(0), Some(127)],
    };
    let target = MidiOutTarget::Footswitch(Footswitch::B);
    let effects = reduce(
        Action::SetPresetMidi {
            target,
            messages: vec![toggle],
        },
        &mut state,
    )
    .unwrap();
    assert!(
        effects.contains(&Effect::PresetMidiSource(1)),
        "{effects:?}"
    );
    assert!(!effects.iter().any(|e| matches!(e, Effect::MidiOut(_))));
    let preset = &state.current.as_ref().unwrap().preset;
    let stored = &preset.midi_messages_general_v2[12];
    assert_eq!(
        (
            stored.kind,
            stored.channel,
            stored.param1,
            stored.param2,
            stored.param3
        ),
        (2, 2, 64, 0, 127)
    );
    assert_eq!(preset.midi_messages_general[1], *stored);
    assert!(
        preset.midi_messages_general_v2[13..24]
            .iter()
            .all(|m| m.kind == 0)
    );

    // An OFF PC bank is stored as `u32::MAX`; the on-load list has no source to reset.
    let program = MidiOutMessage {
        kind: MidiMessageKind::Pc,
        channel: 1,
        values: [None, Some(3), Some(9)],
    };
    let effects = reduce(
        Action::SetPresetMidi {
            target: MidiOutTarget::OnLoad,
            messages: vec![program],
        },
        &mut state,
    )
    .unwrap();
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::PresetMidiSource(_)))
    );
    let on_load = &state.current.as_ref().unwrap().preset.midi_messages[0];
    assert_eq!(
        (on_load.kind, on_load.param1, on_load.param3),
        (3, u32::MAX, 9)
    );
}

/// Boot `0x2fbf08` with a user default model preset for 4004 (`0x1f57b8`): its rows go over a
/// fresh block (`0x1f7e1c` → `0x1f7b10`), not BYPASS 1.0, so the EQ is ON (I: fresh block
/// value). Unsaved indices take the preset values; the saver writes them with the next edit.
#[test]
fn global_eq_boots_from_the_user_default_model_preset() {
    use tetrad_storage::stock::{DbLimits, ExpSwitch, ParamEntry, StockDb};
    use tetrad_ui::{EqBand, SettingId, SettingValue};
    let root = std::env::temp_dir().join(format!("tetrad-app-eq-default-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut db = StockDb::create(&root.join("zenui.db"), DbLimits::default()).unwrap();
    let row = |index, value: &str| ParamEntry {
        index,
        value: value.into(),
        exp: 0,
        exp_trim_max: 1.0,
        exp_trim_min: 0.0,
    };
    db.transaction(|tx| {
        let switch = ExpSwitch {
            switch_type: 0,
            inverted: false,
            latch_emulation: false,
            delay_ms: 600,
        };
        let rows = [row(0, "0.25"), row(26, "1")];
        let id = tx.create_model_preset("Bright", 4004, switch, &rows)?;
        tx.set_default_model_preset(4004, Some((id, false)))
    })
    .unwrap();
    let mut state = state();
    state.specs = Some(global_eq_specs());
    state.library = Some(tetrad_app::device::Library {
        presets: tetrad_library::stock::StockLibrary::open(
            tetrad_storage::media::MediaRoot::new(&root),
            tetrad_library::stock::StockLimits::default(),
        )
        .unwrap(),
        database: Some(db),
    });
    let no_factory = || -> tetrad_storage::Result<StockDb> { unreachable!("user row") };
    state.global_eq_default =
        tetrad_app::device::global_eq::user_default(&state, no_factory).unwrap();
    let band = EqBand::new(2).unwrap();
    let id = SettingId::EqGain(band);
    let value = SettingValue::Number(0);
    reduce(Action::ChangeSetting { id, value }, &mut state).unwrap();
    let saved = &state.settings.settings().global_eq.params;
    // Band 1 GAIN and ASSIGN_EQ_0 from the preset; BYPASS (29) 0 = ON.
    assert_eq!((saved[&0], saved[&26], saved[&29]), (0.25, 1.0, 0.0));
    std::fs::remove_dir_all(&root).unwrap();
}

/// Model 4004 in the stock parameter order: five bands of GAIN, FREQ, Q, TYPE, BYPASS, then
/// OUTPUT and the two ASSIGN flags (synthetic XML, ModelRepo 4.1.0 layout).
fn global_eq_specs() -> Specs {
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
    let eq = format!(
        "<Category id='40' name='Output EQ' hidden='true'><Model id='4004' name='Output Equalizer'>\
         {}<Parameter name='OUTPUT' type='float' min='-20' max='20' defaultValue='0' steps='241'/>\
         <Parameter name='ASSIGN_EQ_0' type='float' min='0' max='1' defaultValue='0' steps='2'/>\
         <Parameter name='ASSIGN_EQ_1' type='float' min='0' max='1' defaultValue='0' steps='2'/>\
         </Model></Category></Models>",
        (1..=5).map(band).collect::<String>()
    );
    let models = MODELS.replace("</Models>", &eq);
    Specs::resolve(&Catalog::parse(&models, REPORT).unwrap()).unwrap()
}

/// Global EQ edits: stored normalized with every index of the model list, MUTE and the
/// block BYPASS (the ON button, count − 1) included (`0x2fa580`, review r11 N1/N9); a fresh
/// device is OFF (`0x2fbf08`). Sent as `0x3c` only to the assigned outputs while ON
/// (`0x2f9fa8`). An assignment (`0x2fbce0`) sends parameters 0..=25, then the band BYPASS
/// flags and OUTPUT (`0x2faeac`). tetrad-ui TYPE index 4 (PEAK) is stock step 0.
#[test]
fn global_eq_edits_store_normalized_values_and_reach_assigned_outputs() {
    use tetrad_dsp_protocol::controls::ControlRequest;
    use tetrad_ui::{EqBand, OutputJack, SettingId, SettingValue};
    let sends = |effects: &[Effect]| -> Vec<(u8, i32, f32)> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Dsp(DspCommand::Control(ControlRequest::OutputEqualizerParameter {
                    output,
                    parameter,
                    value_bits,
                })) => Some((*output, *parameter, f32::from_bits(*value_bits))),
                _ => None,
            })
            .collect()
    };
    let mut state = state();
    state.specs = Some(global_eq_specs());
    let mut change = |id, value| reduce(Action::ChangeSetting { id, value }, &mut state).unwrap();
    let band = EqBand::new(1).unwrap();
    // +6.0 dB of −12..12 with 241 steps; no output is assigned yet.
    assert!(sends(&change(SettingId::EqGain(band), SettingValue::Number(60))).is_empty());
    let assigned = change(
        SettingId::EqAssign(OutputJack::Out34),
        SettingValue::Bool(true),
    );
    let sent = sends(&assigned);
    assert_eq!(sent.len(), 26 + 6, "{sent:?}");
    assert!(sent.iter().all(|&(output, ..)| output == 1));
    // Step 180 of 240: `step_size × index` in f32 (`0x5cc304`).
    let gain = |value: f32| (value - 0.75).abs() < 1e-6;
    assert!(
        sent.iter()
            .any(|&(_, index, value)| index == 5 && gain(value))
    );
    let peak = SettingValue::Choice(4);
    assert!(
        sends(&change(SettingId::EqType(band), peak.clone())).is_empty(),
        "OFF"
    );
    change(SettingId::EqEnabled, SettingValue::Bool(true));
    assert_eq!(sends(&change(SettingId::EqType(band), peak)), [(1, 8, 0.0)]);
    // 28 XML parameters, MUTE (28) and BYPASS (29, 0 = ON).
    let saved = &state.settings.settings().global_eq.params;
    assert_eq!(saved.len(), 30);
    assert!(gain(saved[&5]) && saved[&27] == 1.0 && saved[&28] == 0.0 && saved[&29] == 0.0);
}

/// Setlists `A` (slot 0 "One", the loaded preset) and `B` (slot 3 "Two") in a fresh library
/// under `root`, and the SWAP of those two slots.
fn swap_fixture(state: &mut State, root: &std::path::Path) -> (String, String, Action) {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut presets = StockLibrary::open(MediaRoot::new(root), StockLimits::default()).unwrap();
    let [a, b] = ["A", "B"].map(|name| presets.create_setlist(name).unwrap().name);
    let preset = state.current.as_ref().unwrap().preset.clone();
    for (setlist, slot, name) in [(&a, 0, "One"), (&b, 3, "Two")] {
        let mut named = preset.clone();
        named.name = Some(name.into());
        let document = tetrad_preset::PresetDocument::parse(
            &named.to_bytes(),
            tetrad_preset::Limits::default(),
        )
        .unwrap();
        let sidecar = tetrad_library::sidecar::Sidecar::default();
        let stored = presets
            .store(setlist, slot, name, &document, &sidecar)
            .unwrap();
        assert!(stored.search_index.is_none());
    }
    state.current.get_mut().as_mut().unwrap().path = presets.slots(&a).unwrap()[0].clone();
    state.library = Some(Library {
        presets,
        database: None,
    });
    let swap = Action::SwapPresets {
        from: a.clone(),
        slot: 0,
        to: b.clone(),
        to_slot: 3,
    };
    (a, b, swap)
}

/// The SWAP of slot 0 of `a` and slot 3 of `b`.
fn swap_again(a: &str, b: &str) -> Action {
    Action::SwapPresets {
        from: a.to_owned(),
        slot: 0,
        to: b.to_owned(),
        to_slot: 3,
    }
}

/// Task r30-ui 3, review r30 C1: a SWAP between two setlists (`StockLibrary::swap_between`,
/// `0x1e5a58`) puts each preset in the other setlist as a new file. The loaded preset's path
/// follows its new file, and stock reloads the loaded slot (`0x971180`), so the other preset
/// plays there. O: with unsaved edits, or no clean snapshot to compare (review r31 D7), the
/// loaded preset only follows its file.
#[test]
fn a_swap_between_setlists_reloads_the_loaded_slot() {
    let mut state = state();
    let root = std::env::temp_dir().join(format!("tetrad-app-swap-{}", std::process::id()));
    let (a, b, swap) = swap_fixture(&mut state, &root);
    let snapshot = tetrad_app::device::preset::Snapshot::of;
    *state.clean.get_mut() = Some(snapshot(state.current.as_ref().unwrap()));
    let effects = reduce(swap, &mut state).unwrap();
    let presets = &state.library.as_ref().unwrap().presets;
    let moved = presets.slots(&b).unwrap()[3].clone().unwrap();
    assert!(
        moved.contains("/B/") && moved.ends_with("One.pb"),
        "{moved}"
    );
    assert!(
        presets.slots(&a).unwrap()[0]
            .as_deref()
            .unwrap()
            .contains("/A/Two")
    );
    assert_eq!(
        state.current.as_ref().unwrap().path.as_deref(),
        Some(moved.as_str())
    );
    let setlist_a = presets.setlists().unwrap()[0].path.clone();
    let reload = Effect::LoadPreset(PresetRef {
        setlist: Some(setlist_a),
        index: 0,
    });
    assert!(effects.contains(&reload), "{effects:?}");

    // The swap back with an unsaved edit: no reload, the edit stays with its file.
    *state.clean.get_mut() = Some(snapshot(state.current.as_ref().unwrap()));
    state.current.get_mut().as_mut().unwrap().preset.name = Some("Edited".into());
    let back = Action::SwapPresets {
        from: b.clone(),
        slot: 3,
        to: a.clone(),
        to_slot: 0,
    };
    let reloads =
        |effects: &[Effect]| (effects.iter()).any(|effect| matches!(effect, Effect::LoadPreset(_)));
    let effects = reduce(back, &mut state).unwrap();
    assert!(!reloads(&effects), "{effects:?}");
    let presets = &state.library.as_ref().unwrap().presets;
    assert_eq!(
        state.current.as_ref().unwrap().path,
        presets.slots(&a).unwrap()[0]
    );

    // No clean snapshot: not known to be clean, so no reload either.
    *state.clean.get_mut() = None;
    let effects = reduce(swap_again(&a, &b), &mut state).unwrap();
    assert!(!reloads(&effects), "{effects:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Review r30 C2: a later step fails after both setlists changed (here the favorites list
/// cannot be read). The swap is done (`Committed::tail_error`): a Library notice names the
/// step, and the loaded preset takes the file its new slot names, so the next save does not
/// write into a file that no slot names.
#[test]
fn a_swap_that_fails_after_its_setlist_writes_keeps_the_loaded_path_in_a_slot() {
    let mut state = state();
    let root = std::env::temp_dir().join(format!("tetrad-app-swap-fail-{}", std::process::id()));
    let (_, b, swap) = swap_fixture(&mut state, &root);
    std::fs::create_dir_all(root.join("favorites")).unwrap();
    let effects = reduce(swap, &mut state).unwrap();
    assert!(
        (effects.iter()).any(|effect| matches!(
            effect,
            Effect::Notice(Subsystem::Library, detail) if detail.starts_with("swap done")
        )),
        "{effects:?}"
    );
    let presets = &state.library.as_ref().unwrap().presets;
    let moved = presets.slots(&b).unwrap()[3].clone();
    assert!(
        moved
            .as_deref()
            .is_some_and(|moved| moved.ends_with("/B/One.pb")),
        "{moved:?}"
    );
    assert_eq!(state.current.as_ref().unwrap().path, moved);
    let _ = std::fs::remove_dir_all(&root);
}

/// The preset name in each slot (`One`, `Two`, or `None`): the side of `a` first.
fn swap_names(state: &State, a: &str, b: &str) -> [Option<String>; 2] {
    let presets = &state.library.as_ref().unwrap().presets;
    [(a, 0), (b, 3)].map(|(setlist, slot)| {
        let path = presets.slots(setlist).unwrap()[slot].clone()?;
        presets
            .load(&path)
            .unwrap()
            .document
            .summary()
            .unwrap()
            .name
    })
}

/// Re-r34 §5: a SWAP pushes one type-2 frame (`0x648a24`) with both slots after the swap. UNDO
/// swaps them back (`0x649330` case 2 → `0x1e3824`) through [`Action::StepLibrarySwap`] (the
/// bring-up write gate), with no reload; the loaded preset's path follows its file, and Cortex
/// Control gets the swap push `0x2519e8`. REDO swaps once (O; stock `0x649b7c` swaps twice).
#[test]
fn undo_of_a_swap_between_setlists_restores_both_slots() {
    let mut state = state();
    let root = std::env::temp_dir().join(format!("tetrad-app-swap-undo-{}", std::process::id()));
    let (a, b, swap) = swap_fixture(&mut state, &root);
    let swapped = [Some("Two".to_owned()), Some("One".to_owned())];
    let restored = [Some("One".to_owned()), Some("Two".to_owned())];
    reduce(swap, &mut state).unwrap();
    assert_eq!(swap_names(&state, &a, &b), swapped);
    assert_eq!(state.history.frame_types(), (vec![2], vec![]));

    // A clean loaded preset: the action swap would reload its slot; the undo does not.
    *state.clean.get_mut() = Some(tetrad_app::device::preset::Snapshot::of(
        state.current.as_ref().unwrap(),
    ));
    let effects = reduce(Action::Undo, &mut state).unwrap();
    let step = Action::StepLibrarySwap { redo: false };
    assert_eq!(effects.first(), Some(&Effect::Action(step.clone())));
    let effects = reduce(step, &mut state).unwrap();
    assert_eq!(swap_names(&state, &a, &b), restored);
    assert_eq!(state.history.frame_types(), (vec![], vec![2]));
    let presets = &state.library.as_ref().unwrap().presets;
    assert_eq!(
        state.current.as_ref().unwrap().path,
        presets.slots(&a).unwrap()[0]
    );
    assert!(
        !(effects.iter()).any(|effect| matches!(effect, Effect::LoadPreset(_))),
        "{effects:?}"
    );
    assert!(
        (effects.iter()).any(|effect| matches!(effect, Effect::ControlNotify(_))),
        "the swap push: {effects:?}"
    );

    let effects = reduce(Action::Redo, &mut state).unwrap();
    let [Effect::Action(redo), ..] = &effects[..] else {
        panic!("{effects:?}");
    };
    reduce(redo.clone(), &mut state).unwrap();
    assert_eq!(swap_names(&state, &a, &b), swapped);
    assert_eq!(state.history.frame_types(), (vec![2], vec![]));
    let _ = std::fs::remove_dir_all(&root);
}

/// Review r35 G3: a swap that does not reload the loaded preset (unsaved edits, a swap UNDO or
/// REDO) moves its file to the other slot; `LastPreset` and the footswitch index follow the
/// file, so the LEDs and the next save name the slot that holds it.
#[test]
fn a_swap_without_a_reload_moves_the_loaded_slot_with_its_file() {
    let mut state = state();
    let root = std::env::temp_dir().join(format!("tetrad-app-swap-slot-{}", std::process::id()));
    let (a, b, swap) = swap_fixture(&mut state, &root);
    let setlist = |state: &State, name: &str| {
        let presets = &state.library.as_ref().unwrap().presets;
        let setlists = presets.setlists().unwrap();
        (setlists.into_iter())
            .find(|setlist| setlist.name == name)
            .unwrap()
            .path
    };
    let (path_a, path_b) = (setlist(&state, &a), setlist(&state, &b));
    state.settings.update(|settings| {
        settings.last_preset.setlist = Some(path_a.clone());
        settings.last_preset.preset_index = 0;
    });
    state.performance.footswitch.current_preset = 0;
    let slot = |state: &State| {
        let last = &state.settings.settings().last_preset;
        (
            last.setlist.clone(),
            last.preset_index,
            state.performance.footswitch.current_preset,
        )
    };
    // No clean snapshot: no reload, the loaded preset follows its file into `b` slot 3.
    *state.clean.get_mut() = None;
    reduce(swap, &mut state).unwrap();
    assert_eq!(slot(&state), (Some(path_b.clone()), 3, 3));

    reduce(Action::StepLibrarySwap { redo: false }, &mut state).unwrap();
    assert_eq!(slot(&state), (Some(path_a), 0, 0));
    reduce(Action::StepLibrarySwap { redo: true }, &mut state).unwrap();
    assert_eq!(slot(&state), (Some(path_b), 3, 3));
    let _ = std::fs::remove_dir_all(&root);
}

/// Review r36 H1/H2: a SWAP inside one setlist with the loaded preset on one side. Stock
/// reloads the loaded slot (`0x1e5a58` → `0x971180`), so a clean loaded preset gives way to the
/// other preset in the same slot: `LastPreset` and the footswitch index stay. With unsaved
/// edits (no clean snapshot here) and on UNDO the loaded preset follows its file instead.
#[test]
fn a_swap_inside_one_setlist_reloads_a_clean_loaded_slot() {
    let mut state = state();
    let root = std::env::temp_dir().join(format!("tetrad-app-swap-one-{}", std::process::id()));
    let (a, b, _) = swap_fixture(&mut state, &root);
    let presets = &mut state.library.as_mut().unwrap().presets;
    let two = presets.slots(&b).unwrap()[3].clone().unwrap();
    let document = presets.load(&two).unwrap().document;
    let sidecar = tetrad_library::sidecar::Sidecar::default();
    let _ = presets.store(&a, 3, "Two", &document, &sidecar).unwrap();
    let path_a = presets.setlists().unwrap()[0].path.clone();
    state.settings.update(|settings| {
        settings.last_preset.setlist = Some(path_a.clone());
        settings.last_preset.preset_index = 0;
    });
    state.performance.footswitch.current_preset = 0;
    let slot = |state: &State| {
        let last = &state.settings.settings().last_preset;
        (
            last.setlist.clone(),
            last.preset_index,
            state.performance.footswitch.current_preset,
        )
    };
    let reloads =
        |effects: &[Effect]| (effects.iter()).any(|effect| matches!(effect, Effect::LoadPreset(_)));
    let swap = Action::SwapPresets {
        from: a.clone(),
        slot: 0,
        to: a.clone(),
        to_slot: 3,
    };

    *state.clean.get_mut() = None;
    let effects = reduce(swap.clone(), &mut state).unwrap();
    assert!(!reloads(&effects), "{effects:?}");
    assert_eq!(
        slot(&state),
        (Some(path_a.clone()), 3, 3),
        "follows its file"
    );
    *state.clean.get_mut() = Some(tetrad_app::device::preset::Snapshot::of(
        state.current.as_ref().unwrap(),
    ));
    let effects = reduce(Action::StepLibrarySwap { redo: false }, &mut state).unwrap();
    assert!(!reloads(&effects), "UNDO: {effects:?}");
    assert_eq!(
        slot(&state),
        (Some(path_a.clone()), 0, 0),
        "UNDO follows its file"
    );

    let effects = reduce(swap, &mut state).unwrap();
    let reload = Effect::LoadPreset(PresetRef {
        setlist: Some(path_a.clone()),
        index: 0,
    });
    assert!(effects.contains(&reload), "{effects:?}");
    assert_eq!(slot(&state), (Some(path_a), 0, 0), "the slot stays");
    let _ = std::fs::remove_dir_all(&root);
}

/// Re-r34 §5: `History::clear_all` (backup restore, factory restore) drops the swap frame. O:
/// when a slot no longer names the file the swap left there, the undo writes nothing, says
/// so, and the frame drops.
#[test]
fn a_changed_slot_refuses_a_swap_undo() {
    let mut state = state();
    let root = std::env::temp_dir().join(format!("tetrad-app-swap-stale-{}", std::process::id()));
    let (a, b, swap) = swap_fixture(&mut state, &root);
    reduce(swap, &mut state).unwrap();
    assert_eq!(state.history.frame_types(), (vec![2], vec![]));

    // Another preset takes slot 0 of `a` after the swap.
    let presets = &mut state.library.as_mut().unwrap().presets;
    let two = presets.slots(&a).unwrap()[0].clone().unwrap();
    let document = presets.load(&two).unwrap().document;
    let _ = presets.delete(&a, 0).unwrap();
    let sidecar = tetrad_library::sidecar::Sidecar::default();
    let _ = presets.store(&a, 0, "Three", &document, &sidecar).unwrap();
    let before = swap_names(&state, &a, &b);
    let effects = reduce(Action::StepLibrarySwap { redo: false }, &mut state).unwrap();
    assert!(
        matches!(&effects[..], [Effect::Notice(Subsystem::Library, detail), ..] if detail.contains("refused")),
        "{effects:?}"
    );
    assert_eq!(swap_names(&state, &a, &b), before);
    assert!(!state.history.can_undo() && !state.history.can_redo());

    let swap = Action::SwapPresets {
        from: a.clone(),
        slot: 0,
        to: b.clone(),
        to_slot: 3,
    };
    reduce(swap, &mut state).unwrap();
    state.history.clear_all();
    assert!(!state.history.can_undo());
    let _ = std::fs::remove_dir_all(&root);
}

/// The file in slot `slot` of `setlist` and its stored preset name.
fn slot_at(state: &State, setlist: &str, slot: usize) -> Option<(String, Option<String>)> {
    let presets = &state.library.as_ref().unwrap().presets;
    let path = presets.slots(setlist).unwrap()[slot].clone()?;
    let name = presets
        .load(&path)
        .unwrap()
        .document
        .summary()
        .unwrap()
        .name;
    Some((path, name))
}

fn name_at(state: &State, setlist: &str, slot: usize) -> Option<String> {
    slot_at(state, setlist, slot).and_then(|(_, name)| name)
}

fn loaded_path(state: &State) -> Option<String> {
    state.current.as_ref().unwrap().path.clone()
}

/// UNDO with a library frame on top is [`Action::StepLibraryFrame`] (the bring-up write gate).
fn step_frame(state: &mut State, redo: bool) -> Vec<Effect> {
    let step = Action::StepLibraryFrame { redo };
    let undo = if redo { Action::Redo } else { Action::Undo };
    let effects = reduce(undo, state).unwrap();
    assert_eq!(effects.first(), Some(&Effect::Action(step.clone())));
    reduce(step, state).unwrap()
}

/// Re-r38 §4: a setlist delete pushes a type-1 frame (`0x1d916c`) with the preset read in
/// full. UNDO puts it back in its slot (`0x647fa8`) and into the favorites again; REDO deletes
/// it again. A delete of the loaded preset's file leaves it with no path (review r39 K5); when
/// that file comes back, the loaded preset follows it, also when another preset took the old
/// file name; when a REDO deletes it again, the loaded preset keeps no path (O: never a file
/// it does not hold).
#[test]
fn undo_of_a_setlist_delete_restores_the_preset_and_redo_deletes_it() {
    use tetrad_library::stock::ItemList;
    use tetrad_storage::media::Collection;
    let mut state = state();
    let root = std::env::temp_dir().join(format!("tetrad-app-delete-undo-{}", std::process::id()));
    let (a, b, _) = swap_fixture(&mut state, &root);
    let favorites = |state: &State| -> Vec<String> {
        let presets = &state.library.as_ref().unwrap().presets;
        let list = presets.items(ItemList::Favorites(Collection::Presets));
        list.unwrap().into_iter().map(|entry| entry.item).collect()
    };
    let (two, _) = slot_at(&state, &b, 3).unwrap();
    let favorite = Action::SetFavorite {
        path: two,
        favorite: true,
    };
    reduce(favorite, &mut state).unwrap();
    let delete = |setlist: &str, slot| Action::DeletePreset {
        setlist: setlist.to_owned(),
        slot,
    };
    reduce(delete(&b, 3), &mut state).unwrap();
    assert_eq!(slot_at(&state, &b, 3), None);
    assert!(favorites(&state).is_empty());
    assert_eq!(state.history.frame_types(), (vec![1], vec![]));

    step_frame(&mut state, false);
    assert_eq!(name_at(&state, &b, 3).as_deref(), Some("Two"));
    let (restored, _) = slot_at(&state, &b, 3).unwrap();
    assert_eq!(favorites(&state), [restored]);
    assert_eq!(state.history.frame_types(), (vec![], vec![1]));
    step_frame(&mut state, true);
    assert_eq!(slot_at(&state, &b, 3), None);
    assert_eq!(state.history.frame_types(), (vec![1], vec![]));

    let (one, _) = slot_at(&state, &a, 0).unwrap();
    assert_eq!(loaded_path(&state).as_deref(), Some(one.as_str()));
    reduce(delete(&a, 0), &mut state).unwrap();
    assert_eq!(loaded_path(&state), None);
    // Another preset named "One" takes the freed file name before the UNDO.
    let mut other = state.current.as_ref().unwrap().preset.clone();
    other.name = Some("One".into());
    let document =
        tetrad_preset::PresetDocument::parse(&other.to_bytes(), tetrad_preset::Limits::default())
            .unwrap();
    let presets = &mut state.library.as_mut().unwrap().presets;
    let sidecar = tetrad_library::sidecar::Sidecar::default();
    let taken = presets.store(&a, 1, "One", &document, &sidecar).unwrap();
    assert_eq!(taken.value, one);
    step_frame(&mut state, false);
    let (back, _) = slot_at(&state, &a, 0).unwrap();
    assert_ne!(back, one);
    assert_eq!(loaded_path(&state), Some(back));
    step_frame(&mut state, true);
    assert_eq!(slot_at(&state, &a, 0), None);
    assert_eq!(loaded_path(&state), None);
    let _ = std::fs::remove_dir_all(&root);
}

/// Re-r38 §4: a paste over a preset pushes a type-3 frame (`0x1de9f8`) that holds the target
/// slot as it was. UNDO removes the copy and brings the overwritten preset back; the source
/// stays. REDO pastes again.
#[test]
fn undo_of_a_paste_over_a_preset_brings_the_overwritten_preset_back() {
    let mut state = state();
    let root = std::env::temp_dir().join(format!("tetrad-app-paste-undo-{}", std::process::id()));
    let (a, b, _) = swap_fixture(&mut state, &root);
    let paste = Action::PastePresets {
        from: a.clone(),
        to: b.clone(),
        pairs: vec![(0, 3)],
        remove: false,
    };
    reduce(paste, &mut state).unwrap();
    assert_eq!(name_at(&state, &b, 3).as_deref(), Some("One"));
    assert_eq!(state.history.frame_types(), (vec![3], vec![]));

    step_frame(&mut state, false);
    assert_eq!(name_at(&state, &b, 3).as_deref(), Some("Two"));
    assert_eq!(name_at(&state, &a, 0).as_deref(), Some("One"));
    assert_eq!(state.history.frame_types(), (vec![], vec![3]));
    step_frame(&mut state, true);
    assert_eq!(name_at(&state, &b, 3).as_deref(), Some("One"));
    assert_eq!(state.history.frame_types(), (vec![3], vec![]));
    let _ = std::fs::remove_dir_all(&root);
}

/// Re-r38 §4: a move into a setlist slot pushes a type-3 frame with the target and the source
/// slot. UNDO brings the preset back into the source slot and empties the target; the loaded
/// preset moves with it both ways.
#[test]
fn undo_of_a_move_restores_the_source_slot() {
    let mut state = state();
    let root = std::env::temp_dir().join(format!("tetrad-app-move-undo-{}", std::process::id()));
    let (a, b, _) = swap_fixture(&mut state, &root);
    let moved = Action::CopyPresetTo {
        from: a.clone(),
        slot: 0,
        to: b.clone(),
        to_slot: Some(5),
        remove: true,
    };
    reduce(moved, &mut state).unwrap();
    assert_eq!(slot_at(&state, &a, 0), None);
    let (target, _) = slot_at(&state, &b, 5).unwrap();
    assert_eq!(loaded_path(&state), Some(target));
    assert_eq!(state.history.frame_types(), (vec![3], vec![]));

    step_frame(&mut state, false);
    assert_eq!(slot_at(&state, &b, 5), None);
    let (source, name) = slot_at(&state, &a, 0).unwrap();
    assert_eq!(name.as_deref(), Some("One"));
    assert_eq!(loaded_path(&state), Some(source));
    step_frame(&mut state, true);
    assert_eq!(slot_at(&state, &a, 0), None);
    let (target, _) = slot_at(&state, &b, 5).unwrap();
    assert_eq!(loaded_path(&state), Some(target));
    let _ = std::fs::remove_dir_all(&root);
}

/// Re-r38 §4, O: stock restores a slot with no check and deletes whatever is there. Tetrad
/// writes nothing when the slot changed since the operation, says so, and drops the frame.
#[test]
fn a_changed_slot_refuses_a_slot_undo() {
    let mut state = state();
    let root = std::env::temp_dir().join(format!("tetrad-app-slot-stale-{}", std::process::id()));
    let (_, b, _) = swap_fixture(&mut state, &root);
    let delete = Action::DeletePreset {
        setlist: b.clone(),
        slot: 3,
    };
    reduce(delete, &mut state).unwrap();
    // Another preset takes the emptied slot.
    let mut three = state.current.as_ref().unwrap().preset.clone();
    three.name = Some("Three".into());
    let document =
        tetrad_preset::PresetDocument::parse(&three.to_bytes(), tetrad_preset::Limits::default())
            .unwrap();
    let presets = &mut state.library.as_mut().unwrap().presets;
    let sidecar = tetrad_library::sidecar::Sidecar::default();
    let _ = presets.store(&b, 3, "Three", &document, &sidecar).unwrap();

    let effects = step_frame(&mut state, false);
    assert!(
        matches!(&effects[..], [Effect::Notice(Subsystem::Library, detail), ..] if detail.contains("refused")),
        "{effects:?}"
    );
    assert_eq!(name_at(&state, &b, 3).as_deref(), Some("Three"));
    assert!(!state.history.can_undo() && !state.history.can_redo());
    let _ = std::fs::remove_dir_all(&root);
}

/// Review r39 K7: the reverse frame of a slot step is read first (`0x21e42c`). When that read
/// fails (here the pasted copy is no longer a preset), nothing is written, a notice says so and
/// the frame drops, as for a changed slot, so it does not block every later UNDO.
#[test]
fn a_failed_reverse_read_drops_the_slot_frame() {
    let mut state = state();
    let root = std::env::temp_dir().join(format!("tetrad-app-slot-read-{}", std::process::id()));
    let (a, b, _) = swap_fixture(&mut state, &root);
    let paste = Action::PastePresets {
        from: a.clone(),
        to: b.clone(),
        pairs: vec![(0, 3)],
        remove: false,
    };
    reduce(paste, &mut state).unwrap();
    let (copy, _) = slot_at(&state, &b, 3).unwrap();
    let presets = &state.library.as_ref().unwrap().presets;
    std::fs::write(presets.media().resolve(&copy).unwrap(), b"not a preset").unwrap();

    let effects = step_frame(&mut state, false);
    assert!(
        matches!(&effects[..], [Effect::Notice(Subsystem::Library, detail), ..] if detail.contains("refused")),
        "{effects:?}"
    );
    let presets = &state.library.as_ref().unwrap().presets;
    assert_eq!(presets.slots(&b).unwrap()[3], Some(copy));
    assert_eq!(state.history.frame_types(), (vec![], vec![]));
    let _ = std::fs::remove_dir_all(&root);
}

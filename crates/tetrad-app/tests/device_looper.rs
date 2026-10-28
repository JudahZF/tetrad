//! Looper X and metronome contracts of the w5 pass: caller gates, tile and footswitch routing, the
//! tempo copy on a scope change and the time-signature accent reset.

use std::time::Duration;

use tetrad_app::device::actions::{Action, reduce};
use tetrad_app::device::features::{self, looper_context, set_tempo_parameter};
use tetrad_app::device::input::Input;
use tetrad_app::device::midi::ClockStatus;
use tetrad_app::device::preset::{SpecsCatalog, prepare};
use tetrad_app::device::{Current, Effect, State};
use tetrad_app::tempo::TempoParam;
use tetrad_catalog::{Catalog, Specs};
use tetrad_engine::{Column, Row, Site, Slot};
use tetrad_hal::hal::Event as HalEvent;
use tetrad_hal::node::Element;
use tetrad_hal::platform::{Platform as HalPlatform, Product};
use tetrad_midi::LooperButton;
use tetrad_preset::{Model, ParamValue, Preset, Slot as PresetSlot};
use tetrad_settings::device::{DeviceSettings, Hardware};
use tetrad_settings::midi::MidiClockOut;
use tetrad_settings::{SettingsFileId, SettingsStorage};
use tetrad_ui::Screen;

/// Synthetic catalog (no stock data): the 25000 slots with their catalog ranges (VOLUME,
/// PAN, SOUND, STEPSTATE0..12) and a 27000 Looper with 23 unit parameters.
fn models() -> String {
    let tempo = [
        ("TEMPO", 40, 240),
        ("SCOPE", 0, 1),
        ("LED", 0, 1),
        ("VOLUME", -60, 9),
        ("ENABLE", 0, 1),
        ("PAN", -1, 1),
        ("TIMESIG", 0, 20),
        ("SUBDIVISION", 0, 3),
        ("SOUND", 0, 5),
        ("ROUTING", 0, 4),
    ]
    .into_iter()
    .map(|(name, min, max)| (name.to_owned(), min, max))
    .chain((0..13).map(|step| (format!("STEPSTATE{step}"), 0, 3)))
    .map(|(name, min, max)| {
        format!(
            "<Parameter name='{name}' type='float' min='{min}' max='{max}' defaultValue='{min}'/>"
        )
    })
    .collect::<String>();
    let looper = (0..23)
        .map(|index| {
            format!("<Parameter name='P{index}' type='float' min='0' max='1' defaultValue='0'/>")
        })
        .collect::<String>();
    format!(
        "<Models>\
<Category id='10' name='Splitter' hidden='true'><Model id='10004' name='Splitter'>\
<Parameter name='TYPE' type='switch' min='0' max='2' defaultValue='0'/></Model></Category>\
<Category id='11' name='Mixer' hidden='true'><Model id='11000' name='Mixer'>\
<Parameter name='LEVEL A' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/>\
</Model></Category>\
<Category id='23' name='Lane Output' hidden='true'><Model id='23000' name='Lane Output'>\
<Parameter name='VOLUME' type='float' min='0' max='1' defaultValue='0.76923'/>\
<Parameter name='PAN' type='float' min='0' max='1' defaultValue='0.5'/></Model></Category>\
<Category id='25' name='Tempo' hidden='true'><Model id='25000' name='Tempo'>{tempo}</Model></Category>\
<Category id='27' name='Looper'><Model id='27000' name='Loop' blob='loop'>{looper}</Model></Category>\
<Category id='28' name='Input Gate' hidden='true'><Model id='28000' name='Input Gate'>\
<Parameter name='THRESHOLD' type='float' min='-96' max='0' defaultValue='-60'/></Model></Category>\
</Models>"
    )
}

const REPORT: &str = "<ModuleReport>\
<Kernel core='0' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='1' cpu='2' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='2' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='3' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Module id='27000' name='Loop' blob_name='loop' category='27' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='10'/></Module>\
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

fn looper_site() -> Site {
    Site::Slot(Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(2).unwrap(),
    })
}

/// A QC with a Looper X at row 1, column 3, and the global tempo scope.
fn state() -> State {
    let specs = Specs::resolve(&Catalog::parse(&models(), REPORT).unwrap()).unwrap();
    let catalog = SpecsCatalog(&specs);
    let mut preset = Preset::new_default(tetrad_firmware::running().version, &catalog).unwrap();
    preset
        .insert_block(
            PresetSlot { row: 0, column: 2 },
            Model::block(27000, std::iter::repeat_n(ParamValue::Float(0.0), 23)),
            &catalog,
        )
        .unwrap();
    let prepared = prepare(preset, &specs, false, None, Default::default()).unwrap();
    let (mut settings, _) = DeviceSettings::load(&NoFiles, Hardware::QuadCortex).unwrap();
    settings.update(|settings| {
        settings.tempo.params.insert(1, 1.0);
    });
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

fn stored(effects: &[Effect]) -> Vec<(u32, f32)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Action(Action::SetParameter { site, index, value }) => {
                assert_eq!(*site, looper_site());
                Some((*index, *value))
            }
            _ => None,
        })
        .collect()
}

/// P19 on (`0x55e93c`): 1 only when ClockInEn is on and ClockOutEn is Off.
#[test]
fn midi_clock_start_needs_clock_in_on_and_clock_out_off() {
    let mut state = state();
    for (clock_in, clock_out, sends) in [
        (true, MidiClockOut::Off, true),
        (true, MidiClockOut::DinOnly, false),
        (false, MidiClockOut::Off, false),
        (false, MidiClockOut::UsbAndDin, false),
    ] {
        state.settings.update(|settings| {
            settings.midi.clock_in = clock_in;
            settings.midi.clock_out = clock_out;
        });
        let start = looper_context(&state).midi_clock_start.unwrap();
        assert_eq!(
            start.enabled && !start.inhibited,
            sends,
            "{clock_in} {clock_out:?}"
        );
    }
}

/// `0x5138dc` reads the USB (`+0xfd`) and DIN (`+0xfe`) running flags: a running clock at
/// tempo 0 has no BPM override but still blocks the Looper clock reset.
#[test]
fn looper_clock_gates_follow_the_running_flags() {
    let mut state = state();
    state.metronome = Some(features::tempo_controller(&state).unwrap());
    state
        .metronome
        .as_mut()
        .unwrap()
        .set_driver(TempoParam::Enabled, 0.0)
        .unwrap();
    let usb = ClockStatus {
        usb_running: true,
        din_running: false,
        tempo: 0.0,
        ..ClockStatus::default()
    };
    reduce(Action::MidiClock(usb), &mut state).unwrap();
    assert_eq!(state.clock_bpm, None);
    let clock = looper_context(&state).clock.unwrap();
    assert!(clock.midi_gate_f514 && !clock.midi_gate_fbb4);
}

/// Tiles (`0x6129fc`, `0x612c60`): a passing edge is stored as P0–7 through the edit path;
/// only Record, Duplicate and Punch In release; an empty loop (`+0x186`) drops Play/Stop,
/// Reverse and an Undo with no redo, and when idle also Duplicate and Punch In (Inactive,
/// `0x614e48`); an undo in progress (`+0x185`) leaves only Undo/Redo.
#[test]
fn tiles_store_their_edges_behind_the_tile_gates() {
    let mut state = state();
    let tile = |state: &mut State, button, pressed| {
        stored(&reduce(Action::LooperTile { button, pressed }, state).unwrap())
    };
    assert_eq!(
        tile(&mut state, LooperButton::RecordOverdub, true),
        [(0, 1.0)]
    );
    assert_eq!(
        tile(&mut state, LooperButton::RecordOverdub, false),
        [(0, 0.0)]
    );
    assert!(tile(&mut state, LooperButton::OneShot, false).is_empty());
    for button in [
        LooperButton::PlayStop,
        LooperButton::Reverse,
        LooperButton::Undo,
        LooperButton::Duplicate,
        LooperButton::PunchIn,
    ] {
        assert!(tile(&mut state, button, true).is_empty(), "{button:?}");
    }
    state.looper_playback.incoming.loop_length = 480;
    assert_eq!(tile(&mut state, LooperButton::Reverse, true), [(7, 1.0)]);
    state.looper_playback.incoming.undo_progress_bits = 0.5_f32.to_bits();
    assert!(tile(&mut state, LooperButton::PunchIn, true).is_empty());
    assert_eq!(tile(&mut state, LooperButton::Undo, true), [(2, 1.0)]);
    // The store path accepts the button parameter of the Looper block.
    let effects = reduce(
        Action::SetParameter {
            site: looper_site(),
            index: 0,
            value: 1.0,
        },
        &mut state,
    )
    .unwrap();
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Deploy(_)))
    );
}

/// `0x613510`: with the Looper view set, a letter takes its assignment's tile (QC default:
/// switch A is Duplicate) and DOWN, TEMPO and UP do nothing; with the view clear, only DOWN
/// and UP are consumed and letters keep their normal meaning.
#[test]
fn looper_view_footswitches_drive_tiles() {
    let mut state = state();
    let mut input = Input::new();
    let mut stomp = |state: &mut State, index: usize, value| {
        let element = Element::all().nth(index).unwrap();
        input
            .on_hal(HalEvent::Stomp { element, value }, state, Duration::ZERO)
            .unwrap()
    };
    reduce(Action::ScreenShown(Screen::Looper), &mut state).unwrap();
    assert!(state.looper_view);
    let tile = |effects: &[Effect]| {
        effects.iter().find_map(|effect| match effect {
            Effect::Action(Action::LooperTile { button, pressed }) => Some((*button, *pressed)),
            _ => None,
        })
    };
    assert_eq!(
        tile(&stomp(&mut state, 0, 1)),
        Some((LooperButton::Duplicate, true))
    );
    for hal in [4, 9, 10] {
        assert!(stomp(&mut state, hal, 1).is_empty(), "HAL {hal}");
    }
    reduce(Action::LooperView(false), &mut state).unwrap();
    assert!(stomp(&mut state, 4, 1).is_empty());
    stomp(&mut state, 9, 1);
    assert_ne!(state.performance.held, 0, "TEMPO keeps its normal handling");
    assert_eq!(tile(&stomp(&mut state, 0, 1)), None);
}

/// Looper X ⋮ (`0x94d14`): Reassign arms the swap dialog (`0x37ab08`); the next letter takes
/// the picked tile's action, so the two `stompAssignments` swap, and neither edge reaches the
/// Looper tiles. Reset (`0x4b40a8`) restores the factory set (`0x11e9448`).
#[test]
fn looper_menu_reassigns_and_resets_footswitch_actions() {
    let mut state = state();
    let mut input = Input::new();
    let mut stomp = |state: &mut State, index: usize, value| {
        let element = Element::all().nth(index).unwrap();
        input
            .on_hal(HalEvent::Stomp { element, value }, state, Duration::ZERO)
            .unwrap()
    };
    let tiles = |effects: &[Effect]| {
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Action(Action::LooperTile { .. })))
    };
    let assignments = |state: &State| state.settings.settings().looper.stomp_assignments();
    reduce(Action::ScreenShown(Screen::Looper), &mut state).unwrap();
    let factory = assignments(&state);
    reduce(Action::LooperAssign(Some(0)), &mut state).unwrap();
    // HAL 5 is footswitch E.
    assert!(!tiles(&stomp(&mut state, 5, 1)));
    assert!(!tiles(&stomp(&mut state, 5, 0)));
    assert_eq!(state.looper_assign, None);
    let mut swapped = factory;
    swapped.swap(0, 4);
    assert_eq!(assignments(&state), swapped);
    reduce(Action::ResetLooperActions, &mut state).unwrap();
    assert_eq!(assignments(&state), factory);
}

/// `0x64c870`: a scope change to global copies the running MIDI clock tempo (normalized over
/// 40..240) into index 0; without a running clock the stored BPM stays.
#[test]
fn scope_change_to_global_copies_the_midi_clock_tempo() {
    for running in [false, true] {
        let mut state = state();
        state.settings.update(|settings| {
            settings.tempo.params.insert(1, 0.0);
            settings.tempo.params.insert(0, 0.4);
        });
        state.midi_clock = ClockStatus {
            usb_running: false,
            din_running: running,
            tempo: 90.0,
            ..ClockStatus::default()
        };
        set_tempo_parameter(&mut state, 1, 1.0, false).unwrap();
        let bpm = state.settings.settings().tempo.params[&0];
        assert_eq!(bpm, if running { 0.25 } else { 0.4 });
    }
}

/// `0x513410` index 6 → `0x49469c`: the new row's default accents are stored; between two
/// `x/4` rows only the added beats are written.
#[test]
fn time_signature_edit_resets_default_accents() {
    let mut state = state();
    let accent = |state: &State, beat: u16| state.settings.settings().tempo.params[&(10 + beat)];
    let signature = |state: &mut State, row: f32| {
        set_tempo_parameter(state, 6, row / 20.0, false).unwrap();
    };
    signature(&mut state, 13.0); // 6/8
    let levels: Vec<f32> = (0..13).map(|beat| accent(&state, beat) * 3.0).collect();
    assert_eq!(levels, [2., 0., 0., 3., 0., 0., 0., 0., 0., 0., 0., 0., 0.]);
    signature(&mut state, 2.0); // 4/4
    for index in [13, 15] {
        set_tempo_parameter(&mut state, index, 1.0, false).unwrap();
    }
    signature(&mut state, 3.0); // 5/4: beats 4.. only
    assert_eq!((accent(&state, 3), accent(&state, 5)), (1.0, 0.0));
}

/// Looper editor open (`0x614840`): a Looper X with parameters 20 and 18 not 0 asks the DSP
/// once for the metronome status (`0x875db0`, `0x45`); with them at 0 it does not.
#[test]
fn looper_editor_open_requests_the_metronome_status_once() {
    use tetrad_app::device::DspCommand;
    use tetrad_dsp_protocol::controls::ControlRequest;
    let mut state = state();
    let request = Effect::Dsp(DspCommand::Control(ControlRequest::MetronomeStatusRequest));
    let open = |state: &mut State| reduce(Action::ScreenShown(Screen::Looper), state).unwrap();
    assert!(!open(&mut state).contains(&request));
    reduce(Action::ScreenShown(Screen::Grid), &mut state).unwrap();
    for index in [18, 20] {
        let set = Action::SetParameter {
            site: looper_site(),
            index,
            value: 1.0,
        };
        reduce(set, &mut state).unwrap();
    }
    assert!(open(&mut state).contains(&request));
    assert!(
        !open(&mut state).contains(&request),
        "only when the editor opens"
    );
}

/// `sendAllSounds` (`0x558554`): the 18 stock files under `metronomesounds` make one `0x47`
/// bundle in stock order; an unreadable file is an empty sound and is named.
#[test]
fn metronome_sounds_bundle_the_stock_files_and_name_the_missing_ones() {
    use tetrad_media::metronome::STOCK_SOUND_FILES;
    let root = std::env::temp_dir().join(format!("tetrad-app-sounds-{}", std::process::id()));
    let folder = root.join("metronomesounds");
    std::fs::create_dir_all(&folder).unwrap();
    for (index, file) in STOCK_SOUND_FILES[..17].iter().enumerate() {
        std::fs::write(folder.join(file), [index as u8; 4]).unwrap();
    }
    let result = features::metronome_sounds(&root);
    let _ = std::fs::remove_dir_all(&root);
    let (bundle, missing) = result.unwrap();
    assert_eq!(missing, [STOCK_SOUND_FILES[17]]);
    assert_eq!(bundle.sounds()[16], [16; 4]);
    assert!(bundle.sounds()[17].is_empty());
}

/// Metronome SOUND writes tempo slot 8 (inferred); "BPM =" is read-only and follows the
/// time signature's beat unit (`0xa478f0`: quarters for the first 12 signatures).
#[test]
fn metronome_sound_writes_slot_8_and_bpm_note_follows_the_signature() {
    use tetrad_ui::{SettingId, SettingValue};
    let mut state = state();
    let set = |id, value| Action::ChangeSetting { id, value };
    reduce(
        set(SettingId::MetronomeSound, SettingValue::Choice(3)),
        &mut state,
    )
    .unwrap();
    let sound = features::scoped_value(&state, 8).unwrap();
    assert!((sound - 0.6).abs() < 1e-6, "{sound}");
    assert!(reduce(set(SettingId::BpmNote, SettingValue::Choice(1)), &mut state).is_err());
    reduce(
        set(SettingId::TimeSignature, SettingValue::Choice(2)),
        &mut state,
    )
    .unwrap();
    assert_eq!(features::bpm_note(&state), Some(0));
    reduce(
        set(SettingId::TimeSignature, SettingValue::Choice(14)),
        &mut state,
    )
    .unwrap();
    assert_eq!(features::bpm_note(&state), Some(1));
}

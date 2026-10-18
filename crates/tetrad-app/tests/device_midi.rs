//! MIDI bridge contracts: DIN program changes become preset-load actions in the right setlist, the
//! clock reaches the DSP as control frames, and the QC mini runs MIDI through the SHARC (in: DSP
//! `0x26`, out: tagged `0x18`).

mod common;

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use tetrad_app::device::midi::Midi;
use tetrad_app::device::{Action, DspCommand, Effect, FACTORY_SETLIST, Library, PresetRef, State};
use tetrad_dsp_host::responses::Event as DspEvent;
use tetrad_dsp_host::setup::DriverMidi;
use tetrad_dsp_protocol::controls::{ControlRequest, UsbMidiDelivery, UsbMidiEvent};
use tetrad_dsp_protocol::responses::{MidiClockTempo, MidiRecord, ResponseType};
use tetrad_hal::platform::{Platform as HalPlatform, Product};
use tetrad_library::stock::{StockLibrary, StockLimits};
use tetrad_settings::device::{DeviceSettings, Hardware};
use tetrad_settings::{SettingsFileId, SettingsStorage};
use tetrad_storage::media::MediaRoot;

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

/// A temp `/media/p4` with two setlists.
struct Media(PathBuf);

impl Media {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "tetrad-app-device-midi-{name}-{}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("Presets")).unwrap();
        Self(root)
    }

    fn library(&self) -> StockLibrary {
        let mut library =
            StockLibrary::open(MediaRoot::new(&self.0), StockLimits::default()).unwrap();
        library.create_setlist("Live").unwrap();
        library.create_setlist("Studio").unwrap();
        library
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn state(platform: HalPlatform, hardware: Hardware) -> State {
    let (settings, _) = DeviceSettings::load(&NoFiles, hardware).unwrap();
    State::new(Product::QuadCortex, platform, settings)
}

fn started(state: &State) -> Midi {
    let mut midi = Midi::new();
    midi.boot(state).unwrap();
    midi.preset_loaded(state).unwrap();
    midi
}

fn feed(midi: &mut Midi, state: &mut State, bytes: &[u8]) -> Vec<Effect> {
    (0u64..)
        .zip(bytes)
        .flat_map(|(at, &byte)| {
            midi.on_din_byte(byte, Duration::from_millis(at), state)
                .unwrap()
        })
        .collect()
}

fn loads(effects: Vec<Effect>) -> Vec<PresetRef> {
    effects
        .into_iter()
        .filter_map(|effect| match effect {
            Effect::Action(Action::ProgramChange { target, reload }) => {
                assert!(reload, "duplicate PCs are not ignored by default");
                Some(target)
            }
            _ => None,
        })
        .collect()
}

/// `0x007c2794`: CC#32 0 selects the Factory Library (library field `+0x24`), CC#32 N ≥ 1
/// user setlist N − 1 in library order, and a PC without CC#32 stays in the setlist of the
/// loaded preset (running status included). Keys are setlist directory paths.
#[test]
fn program_changes_load_in_the_selected_setlist() {
    let media = Media::new("pc");
    let mut state = state(HalPlatform::QcOriginal, Hardware::QuadCortex);
    let presets = media.library();
    let second = presets.setlists().unwrap()[1].path.clone();
    state.library = Some(Library {
        presets,
        database: None,
    });
    let mut midi = started(&state);
    let bytes = [
        0xB0, 0x20, 0x00, 0xC0, 0x05, 0xB0, 0x20, 0x02, 0xC0, 0x06, 0x07,
    ];
    assert_eq!(
        loads(feed(&mut midi, &mut state, &bytes)),
        [
            PresetRef {
                setlist: Some(FACTORY_SETLIST.into()),
                index: 5,
            },
            PresetRef {
                setlist: Some(second),
                index: 6,
            },
            PresetRef {
                setlist: None,
                index: 7,
            },
        ]
    );
}

/// USB MIDI on a QC arrives as DSP `0x07` records (SOC2, ring 4) and takes the same
/// program change path: CC#32 0 + PC 0 on the stored channel (2 here, as on the test QC)
/// loads Factory 1A.
#[test]
fn usb_program_change_loads_on_the_stored_channel() {
    let media = Media::new("usb-pc");
    let mut state = state(HalPlatform::QcOriginal, Hardware::QuadCortex);
    state.settings.update(|settings| {
        settings.midi.channel = tetrad_settings::midi::MidiChannel::new(2).unwrap();
    });
    state.library = Some(Library {
        presets: media.library(),
        database: None,
    });
    let mut midi = started(&state);
    let effects: Vec<_> = [[0x0b, 0xb1, 0x20, 0x00], [0x0c, 0xc1, 0x00, 0x00]]
        .into_iter()
        .flat_map(|event| {
            let event = record(ResponseType::MidiFromType07, event, 0);
            midi.on_dsp(&event, &mut state, Duration::ZERO).unwrap()
        })
        .collect();
    assert_eq!(
        loads(effects),
        [PresetRef {
            setlist: Some(FACTORY_SETLIST.into()),
            index: 0,
        }]
    );
}

/// Lane controls only: enough for the new-preset shape (`preset::read` with no path).
const MODELS: &str = "<Models>\
<Category id='10' name='Splitter' hidden='true'><Model id='10004' name='Splitter'>\
<Parameter name='TYPE' type='switch' min='0' max='2' defaultValue='0'/></Model></Category>\
<Category id='11' name='Mixer' hidden='true'><Model id='11000' name='Mixer'>\
<Parameter name='LEVEL A' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/>\
</Model></Category>\
<Category id='23' name='Lane Output' hidden='true'><Model id='23000' name='Lane Output'>\
<Parameter name='VOLUME' type='float' min='0' max='1' defaultValue='0.76923'/>\
<Parameter name='PAN' type='float' min='0' max='1' defaultValue='0.5'/></Model></Category>\
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
</ModuleReport>";

/// `0x5b5bd8`: while an auditioned plugin preset is loaded its folder refuses a program
/// change (the CC#32 latch stays); a CC#32 bank still selects a user setlist.
#[test]
fn program_change_is_dropped_while_a_plugin_preset_is_auditioned() {
    let media = Media::new("audition");
    let mut state = state(HalPlatform::QcOriginal, Hardware::QuadCortex);
    let presets = media.library();
    let second = presets.setlists().unwrap()[1].path.clone();
    let specs =
        tetrad_catalog::Specs::resolve(&tetrad_catalog::Catalog::parse(MODELS, REPORT).unwrap())
            .unwrap();
    let read = tetrad_app::device::preset::read(&presets, None, &specs).unwrap();
    let prepared = tetrad_app::device::preset::prepare(read, &specs, false, None).unwrap();
    *state.current.get_mut() = Some(tetrad_app::device::Current {
        path: Some("/media/p4/Plugins/Owned/Clean.pb".into()),
        preset: prepared.preset,
        migration: prepared.migration,
        deployment: prepared.deployment,
    });
    state.library = Some(Library {
        presets,
        database: None,
    });
    let mut midi = started(&state);
    assert!(loads(feed(&mut midi, &mut state, &[0xC0, 0x05])).is_empty());
    assert_eq!(
        loads(feed(&mut midi, &mut state, &[0xB0, 0x20, 0x02, 0xC0, 0x06])),
        [PresetRef {
            setlist: Some(second),
            index: 6,
        }]
    );
}

/// Without a library a program change is refused, not loaded.
#[test]
fn program_change_without_library_loads_nothing() {
    let mut state = state(HalPlatform::QcOriginal, Hardware::QuadCortex);
    let mut midi = started(&state);
    assert!(loads(feed(&mut midi, &mut state, &[0xC0, 0x05])).is_empty());
}

/// A DSP MIDI record: `code`, `status`, two data bytes and the word-2 SHARC stamp.
fn record(
    response_type: ResponseType,
    [code, status, data1, data2]: [u8; 4],
    stamp: u32,
) -> DspEvent {
    DspEvent::MidiReceived {
        record: MidiRecord {
            words: [
                u32::from(code) | u32::from(status) << 16 | u32::from(data1) << 24,
                u32::from(data2),
                stamp,
            ],
        },
        response_type,
    }
}

fn mini_records(midi: &mut Midi, state: &mut State, events: &[[u8; 4]]) -> Vec<Effect> {
    events
        .iter()
        .flat_map(|&event| {
            let event = record(ResponseType::MidiFromType26, event, 0);
            midi.on_dsp(&event, state, Duration::ZERO).unwrap()
        })
        .collect()
}

/// One on-load CC 7 = 100 on channel 1.
fn cc_on_load() -> tetrad_preset::Preset {
    tetrad_preset::Preset {
        midi_messages: vec![tetrad_preset::MidiMessage {
            kind: 1,
            channel: 1,
            param1: 7,
            param2: 100,
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// QC mini MIDI in is DSP `0x26`: the drain puts it in the DIN queue (`0x8a118c`), so it
/// passes with MIDI over USB off (a USB `0x07` program change does not) and keeps the CC#32
/// setlist latch.
#[test]
fn mini_midi_in_arrives_as_dsp_0x26() {
    let media = Media::new("mini-in");
    let mut state = state(HalPlatform::Atma, Hardware::Atma);
    state
        .settings
        .update(|settings| settings.midi.over_usb = false);
    let presets = media.library();
    let second = presets.setlists().unwrap()[1].path.clone();
    state.library = Some(Library {
        presets,
        database: None,
    });
    let mut midi = started(&state);
    let usb = record(ResponseType::MidiFromType07, [0x0c, 0xc0, 0x05, 0], 0);
    assert!(loads(midi.on_dsp(&usb, &mut state, Duration::ZERO).unwrap()).is_empty());
    let pc = [[0x0b, 0xb0, 0x20, 0x02], [0x0c, 0xc0, 0x06, 0]];
    assert_eq!(
        loads(mini_records(&mut midi, &mut state, &pc)),
        [PresetRef {
            setlist: Some(second),
            index: 6,
        }]
    );
}

/// The mini times DIN clock by the SHARC stamp in record word 2 at 500 MHz (`0x32ff40` →
/// `0x4f1288`), not by host time: 150 BPM stamps with one host time reach the tempo path.
#[test]
fn mini_midi_clock_follows_the_sharc_stamp() {
    let mut state = state(HalPlatform::Atma, Hardware::Atma);
    state
        .settings
        .update(|settings| settings.midi.clock_in = true);
    let mut midi = started(&state);
    let effects: Vec<Effect> = (0..60u32)
        .flat_map(|tick| {
            let event = record(
                ResponseType::MidiFromType26,
                [0x0f, 0xf8, 0, 0],
                tick * 8_333_333,
            );
            midi.on_dsp(&event, &mut state, Duration::ZERO).unwrap()
        })
        .collect();
    assert!(effects.contains(&Effect::Dsp(DspCommand::Tempo(150))));
}

/// Preset MIDI Out on the mini is one tagged `0x18` (`0x331340` → `0x876200`: selector 1,
/// word +4 = MIDI over USB) and never a DIN write.
#[test]
fn mini_preset_midi_out_is_a_tagged_0x18() {
    let state = state(HalPlatform::Atma, Hardware::Atma);
    let mut midi = started(&state);
    assert_eq!(
        midi.preset_loading(&cc_on_load(), &state).unwrap(),
        [Effect::Dsp(DspCommand::Control(
            ControlRequest::UsbMidiOut {
                delivery: UsbMidiDelivery::Tagged(1),
                event: UsbMidiEvent {
                    code: 0x0b,
                    status: 0xb0,
                    data: [7, 100],
                },
            }
        ))]
    );
}

/// CC#64 on the mini selects footswitch page II for values above 63 (`0x5b5bd8` →
/// `0x589c98`), stored as `Mode/atmaPage`.
#[test]
fn mini_cc64_sets_the_footswitch_page() {
    let mut state = state(HalPlatform::Atma, Hardware::Atma);
    let mut midi = started(&state);
    mini_records(&mut midi, &mut state, &[[0x0b, 0xb0, 0x40, 0x7f]]);
    assert_eq!(state.settings.settings().mode.atma_page, 1);
}

/// The QC path is unchanged: Preset MIDI Out goes to DIN, then `0x18` immediate, and CC#64
/// on DIN does not touch the mini page.
#[test]
fn qc_keeps_din_midi_and_ignores_cc64() {
    let mut state = state(HalPlatform::QcOriginal, Hardware::QuadCortex);
    let mut midi = started(&state);
    assert_eq!(
        midi.preset_loading(&cc_on_load(), &state).unwrap(),
        [
            Effect::MidiOut(vec![0xb0, 7, 100]),
            Effect::Dsp(DspCommand::Control(ControlRequest::UsbMidiOut {
                delivery: UsbMidiDelivery::Immediate,
                event: UsbMidiEvent {
                    code: 0x0b,
                    status: 0xb0,
                    data: [7, 100],
                },
            })),
        ]
    );
    feed(&mut midi, &mut state, &[0xB0, 0x40, 0x7f]);
    assert_eq!(state.settings.settings().mode.atma_page, 0);
}

/// A USB clock tempo (DSP `0x0e`) reaches the DSP tempo path (drivers, then `0x16`).
#[test]
fn usb_clock_tempo_reaches_the_dsp_tempo_path() {
    let mut state = state(HalPlatform::QcOriginal, Hardware::QuadCortex);
    let mut midi = started(&state);
    let tempo = DspEvent::MidiClockTempo(MidiClockTempo {
        int_arg: 120,
        uint_arg: 1,
    });
    let effects = midi
        .on_dsp(&tempo, &mut state, Duration::from_secs(1))
        .unwrap();
    assert!(effects.contains(&Effect::Dsp(DspCommand::Tempo(120))));
}

/// CC#60 writes the Looper editor view flag `+0x187` as `value == 0` (`0x55d1bc` case `0x3c`).
#[test]
fn cc60_sets_the_looper_view_flag() {
    let mut state = state(HalPlatform::QcOriginal, Hardware::QuadCortex);
    let mut midi = started(&state);
    let views: Vec<bool> = feed(&mut midi, &mut state, &[0xB0, 0x3C, 0x00, 0xB0, 0x3C, 0x05])
        .into_iter()
        .filter_map(|effect| match effect {
            Effect::Action(Action::LooperView(shown)) => Some(shown),
            _ => None,
        })
        .collect();
    assert_eq!(views, [true, false]);
}

/// Notes (`0x514270`, not filtered by the global channel) and CC 120/123 (`0x5141b8`, on the
/// global channel, 1 by default) go to the grid drivers with their one-based channel; other
/// CCs do not.
#[test]
fn notes_and_all_notes_off_reach_the_grid_drivers() {
    let mut state = state(HalPlatform::QcOriginal, Hardware::QuadCortex);
    let mut midi = started(&state);
    let bytes = [
        0x91, 0x3c, 0x64, 0x81, 0x3c, 0x00, 0xB0, 0x7b, 0x00, 0xB0, 0x07, 0x40,
    ];
    let routed: Vec<_> = feed(&mut midi, &mut state, &bytes)
        .into_iter()
        .filter_map(|effect| match effect {
            Effect::Dsp(DspCommand::BlockMidi { channel, message }) => Some((channel, message)),
            _ => None,
        })
        .collect();
    let note = |on| DriverMidi::Note {
        note: 0x3c,
        velocity: if on { 0x64 } else { 0 },
        on,
    };
    assert_eq!(
        routed,
        [
            (2, note(true)),
            (2, note(false)),
            (1, DriverMidi::Control(123))
        ]
    );
}

/// The tempo LED blink starts its phase on each MIDI clock beat: `SetTempo` (`0x513790`) goes
/// out again at the clock BPM on every 24th in-range DIN clock (`0x32ff40`, ticks 25 and 49
/// here) and on each USB `0xF8` while no DIN clock runs (`0x89ece0` → `0x89ba68`).
#[test]
fn midi_clock_beats_restart_the_tempo_led_blink() {
    use tetrad_hal::hal::Command;
    use tetrad_hal::zencoder::TempoConfig;
    let blinks = |effects: Vec<Effect>| -> Vec<TempoConfig> {
        effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Hal(Command::Tempo(blink)) => Some(blink),
                _ => None,
            })
            .collect()
    };
    let mut state = state(HalPlatform::QcOriginal, Hardware::QuadCortex);
    state
        .settings
        .update(|settings| settings.midi.clock_in = true);
    let mut midi = started(&state);
    // 150 BPM: one clock every 1/60 s.
    let beats: Vec<(u64, Vec<TempoConfig>)> = (1..=49)
        .map(|tick| {
            let at = Duration::from_micros(16_667 * tick);
            let effects = midi.on_din_byte(0xf8, at, &mut state).unwrap();
            (tick, blinks(effects))
        })
        .filter(|(_, blinks)| !blinks.is_empty())
        .collect();
    let beat = vec![TempoConfig::blink(150)];
    assert_eq!(beats, [(25, beat.clone()), (49, beat)]);

    let mut state = self::state(HalPlatform::QcOriginal, Hardware::QuadCortex);
    let mut midi = started(&state);
    let tick = DspEvent::MidiReceived {
        record: MidiRecord {
            words: [0x0f | (0xf8 << 16), 0, 0],
        },
        response_type: ResponseType::MidiFromType07,
    };
    let effects = midi.on_dsp(&tick, &mut state, Duration::ZERO).unwrap();
    assert_eq!(blinks(effects), [TempoConfig::blink(120)]);
}

/// Review s7 D7 (`0x89ece0` → `0x89ba68` → `0x557dbc`): a USB `0xF8` record asks the DSP for the
/// metronome status while the USB clock runs, also with the BPM known (`clock_bpm`). The gate
/// is `+0xfe` (DIN clock) and `+0xff` (out of range): an out-of-range clock asks nothing.
#[test]
fn usb_clock_ticks_request_the_metronome_status_while_the_bpm_is_known() {
    let requests = |effects: Vec<Effect>| {
        (effects.into_iter())
            .filter(|effect| {
                matches!(
                    effect,
                    Effect::Dsp(DspCommand::Control(ControlRequest::MetronomeStatusRequest))
                )
            })
            .count()
    };
    let tick = DspEvent::MidiReceived {
        record: MidiRecord {
            words: [0x0f | (0xf8 << 16), 0, 0],
        },
        response_type: ResponseType::MidiFromType07,
    };
    let tempo = |int_arg| {
        DspEvent::MidiClockTempo(MidiClockTempo {
            int_arg,
            uint_arg: 5,
        })
    };
    let mut state = state(HalPlatform::QcOriginal, Hardware::QuadCortex);
    let mut midi = started(&state);
    midi.on_dsp(&tempo(120), &mut state, Duration::ZERO)
        .unwrap();
    // What the reducer keeps while the clock runs (`Action::MidiClock` → `ClockTempo`); the
    // test does not run the effects.
    state.clock_bpm = Some(120);
    let running = midi.on_dsp(&tick, &mut state, Duration::ZERO).unwrap();
    assert_eq!(requests(running), 1);

    midi.on_dsp(&tempo(300), &mut state, Duration::ZERO)
        .unwrap();
    let out_of_range = midi.on_dsp(&tick, &mut state, Duration::ZERO).unwrap();
    assert_eq!(requests(out_of_range), 0);
}

/// Review r18 F11 (owned rule): an update install drops MIDI input. UART bytes were dropped
/// already; DSP `0x26` records (all DIN MIDI on the QC mini), USB `0x07` records and USB clock
/// tempo `0x0e` records (no DSP tempo `0x16`) are dropped too. A failed install gives MIDI back.
#[test]
fn an_update_install_drops_dsp_midi_records() {
    use tetrad_app::device::platform::Input;
    use tetrad_app::device::services::{JobError, ServiceNotice};
    use tetrad_app::device::ui::UiInput;
    use tetrad_app::device::{Device, Event};
    use tetrad_dsp_protocol::responses::encode;
    use tetrad_dsp_protocol::{CoreId, DspCore};
    use tetrad_ui::{Screen, SettingsPage};
    type Sim = Device<common::Sim>;
    let specs = common::STOCK.specs();
    let media = common::Media::new([
        ("Light", common::light_preset(&specs, 2)),
        ("Second", common::light_preset(&specs, 3)),
    ]);
    let (mut device, log, _media) = common::boot(media);
    let steps = |device: &mut Sim| (0..50).for_each(|_| _ = device.step());
    let queue = |device: &Sim, notice| {
        (device.platform().inputs.borrow_mut()).push(Input::Service(notice));
    };
    device.platform().worker.set(true);
    queue(&device, ServiceNotice::Download(Ok(())));
    device.push(Event::UiInput(UiInput::Show(Screen::Settings(
        SettingsPage::Updates,
    ))));
    steps(&mut device);
    common::tap_update_install(&mut device, steps);
    assert!(device.bridges().services.inputs_masked());
    // Program change 2 (slot index 1) on channel 1, as a `0x26` record on the SOC2 ring.
    let program_change = |device: &mut Sim| {
        let words = [0x0c_u32 | 0xc0 << 16 | 0x01 << 24, 0, 0];
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        let frame = encode(ResponseType::MidiFromType26, 0, &bytes);
        let rings = &device.platform().clock.rings;
        common::publish(
            &mut rings.0.borrow_mut(),
            CoreId::Dsp(DspCore::Core2),
            &frame,
        );
        steps(device);
    };
    // A USB clock tempo of 90 BPM: the number of DSP tempo (`0x16`) requests it causes.
    let clock_tempo = |device: &mut Sim| {
        let mark = log.borrow().requests.len();
        let words = [90_u32, 1];
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        let frame = encode(ResponseType::MidiClockTempo, 0, &bytes);
        let rings = &device.platform().clock.rings;
        common::publish(
            &mut rings.0.borrow_mut(),
            CoreId::Dsp(DspCore::Core2),
            &frame,
        );
        steps(device);
        log.borrow()
            .kinds(mark)
            .iter()
            .filter(|&&kind| kind == 0x16)
            .count()
    };
    let current = |device: &Sim| (device.state().current.as_ref()).and_then(|c| c.path.clone());
    let before = current(&device);
    program_change(&mut device);
    assert_eq!(current(&device), before);
    assert_eq!(clock_tempo(&mut device), 0);
    queue(
        &device,
        ServiceNotice::Install(Err(JobError::WorkerStopped)),
    );
    steps(&mut device);
    assert!(!device.bridges().services.inputs_masked());
    assert!(clock_tempo(&mut device) > 0);
    program_change(&mut device);
    assert!(
        current(&device).is_some_and(|path| path.ends_with("Second.pb")),
        "{:?}",
        current(&device)
    );
}

/// MIDI → Cortex Control: an out-of-range external clock sets GeneralSettings field 41 and
/// Control pushes it as a one-field UPDATE (`0x32fdbc` → `0x16aa1c`); an in-range tempo clears
/// it with a second push. No "not wired" notice follows a MIDI settings report.
#[test]
fn clock_out_of_range_reaches_control_as_field_41() {
    use tetrad_app::device::Subsystem;
    use tetrad_control::codecs::ControlMessage;
    use tetrad_dsp_protocol::responses::encode;
    use tetrad_dsp_protocol::{CoreId, DspCore};
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 2))]);
    let (mut device, log, _media) = common::boot(media);
    common::connect_control(&mut device);
    let mut usb_tempo = |bpm: u32| {
        let mark = log.borrow().requests.len();
        let bytes: Vec<u8> = [bpm, 2]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        let frame = encode(ResponseType::MidiClockTempo, 0, &bytes);
        let rings = &device.platform().clock.rings;
        common::publish(
            &mut rings.0.borrow_mut(),
            CoreId::Dsp(DspCore::Core2),
            &frame,
        );
        (0..50).for_each(|_| _ = device.step());
        (common::control_sent(&log.borrow(), mark).into_iter())
            .filter_map(|message| match message {
                ControlMessage::GeneralSettings(message) => {
                    message.external_midi_clock_out_of_range
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(usb_tempo(300), [true]);
    assert_eq!(usb_tempo(90), [false]);
    let notices = &log.borrow().notices;
    assert!(
        !notices
            .iter()
            .any(|notice| notice.subsystem == Subsystem::Midi)
    );
}

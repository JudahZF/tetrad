//! The capture bridge: V1 session wiring, the DSPController capture swap (`run_entry`, stock
//! `0x528498`) and capture records from the library with injected keys. All bytes are
//! invented; the keys are test secrets, never device keys.

mod common;

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use tetrad_app::device::DeviceError;
use tetrad_app::device::capture::Nc2Product;
use tetrad_app::device::capture::{
    CabBlock, Capture, CaptureBridge, CaptureError, CapturePreset, EntryDevice, HeldStomp,
    LANE_MUTE, LANE_SOLO, LaneState, OVERRIDE_SETTLE, Parts, StockCaptures, Swap, capture_preset,
    run_entry, swap_context,
};
use tetrad_app::device::control::nc2_message;
use tetrad_app::device::dsp::Context;
use tetrad_app::device::features::Expression;
use tetrad_app::device::media::MediaLibrary;
use tetrad_app::device::platform::{DspFile, TouchSample};
use tetrad_app::device::services::CloudRequest;
use tetrad_app::device::ui::UiInput;
use tetrad_app::device::{Device, DspCommand, Effect, Event, Timer};
use tetrad_capture::entry::{
    CapturePlacement, DspController, EntryAction, GridEdit, Hardware, LANE_VOLUME, NC2_CAB_SITE,
    NC2_ENABLE, NcPreset,
};
use tetrad_capture::orchestration::{CallerFact, Command, Gate, NcState};
use tetrad_capture::session::{
    CaptureLibrary, CaptureRecord, CaptureServices, CaptureStorage, INPUT_LEVEL_1, SaveInfo,
    ServiceError, SessionError, SharcMemory, TrainerSeed,
};
use tetrad_capture::v2::{
    Nc2Input, Nc2Remote, Nc2Route, Nc2Routing, Nc2State, POLL_RETRY_MS, SETUP_METER_MS, job_url,
};
use tetrad_catalog::Specs;
use tetrad_core::performance::StompIndex;
use tetrad_dsp_host::responses::Event as DspEvent;
use tetrad_dsp_host::setup::drivers::media::CaptureData;
use tetrad_dsp_protocol::requests::Request;
use tetrad_dsp_protocol::responses::MeterRecord;
use tetrad_dsp_protocol::{DeploymentTarget, DspCore, ModuleSlot};
use tetrad_engine::{CompileOptions, Deployment, Globals, Layout, Row, SceneIndex};
use tetrad_hal::hal::Command as HalCommand;
use tetrad_hal::zenjack::NcOutput;
use tetrad_media::catalog::FactoryIrCatalog;
use tetrad_media::factory::DeviceCipher;
use tetrad_settings::device::{Hardware as SettingsHardware, Settings};
use tetrad_storage::stock::{
    CaptureEncryption, CaptureStore, DbLimits, Download, DownloadKind, ProductType, StockDb,
    StockProduct,
};
use tetrad_ui::{CaptureRequest, Control, Nc2Dialog, Nc2Request, UiCommand as ViewCommand};

struct FakeMemory;

impl SharcMemory for FakeMemory {
    fn write(&mut self, _address: u32, _bytes: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn read(&mut self, _address: u32, _out: &mut [u8]) -> io::Result<()> {
        Ok(())
    }
}

struct FakeStorage;

impl CaptureStorage for FakeStorage {
    fn file_len(&mut self, _path: &Path) -> io::Result<u64> {
        Ok(4)
    }

    fn read(&mut self, _path: &Path, _out: &mut [u8]) -> io::Result<()> {
        Ok(())
    }

    fn write(&mut self, _path: &Path, _bytes: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

struct FakeServices;

impl CaptureServices for FakeServices {
    fn request(&mut self, gate: Gate) -> Result<Option<CallerFact>, ServiceError> {
        Ok(match gate {
            Gate::EntryServices(state) => Some(CallerFact::EntryServicesReady(state)),
            _ => None,
        })
    }
}

#[derive(Default)]
struct FakeLibrary {
    hashes: Vec<String>,
}

impl CaptureLibrary for FakeLibrary {
    fn contains(&mut self, hash: &str) -> io::Result<bool> {
        Ok(self.hashes.iter().any(|known| known == hash))
    }

    fn insert(&mut self, record: &CaptureRecord<'_>) -> io::Result<()> {
        self.hashes.push(record.hash.clone());
        Ok(())
    }

    fn update(&mut self, _hash: &str, _info: &SaveInfo) -> io::Result<()> {
        Ok(())
    }
}

/// Review-2 R4 and review r25 Q4: a V1 transfer at a DSP buffer address outside SHARC L2 and
/// the DDR below Linux RAM (`CAPTURE_WINDOWS`) is refused before the platform memory sees it,
/// write and read; a buffer inside a window passes. DDR above Linux RAM is refused too: on a
/// unit with less DDR, `0xa300_0000` can alias onto Linux RAM.
#[test]
fn capture_transfers_outside_the_sharc_windows_are_refused() {
    use tetrad_app::device::capture::{CaptureMemory, LINUX_RAM};
    #[derive(Default)]
    struct Recorded(Vec<(u32, usize)>);
    impl SharcMemory for Recorded {
        fn write(&mut self, address: u32, bytes: &[u8]) -> io::Result<()> {
            self.0.push((address, bytes.len()));
            Ok(())
        }
        fn read(&mut self, address: u32, out: &mut [u8]) -> io::Result<()> {
            self.0.push((address, out.len()));
            Ok(())
        }
    }
    let mut memory = CaptureMemory(Recorded::default());
    let (linux, linux_len) = LINUX_RAM;
    // Linux RAM, a range that runs into it, outside L2 and DDR, past the 32-bit end, DDR
    // above Linux RAM and its alias range. `0x8200_8000` is the kernel code start measured on
    // a rev5 QC (S0, 2026-10-15), below the rootfs device-tree RAM base.
    for (address, len) in [
        (0x8200_8000, 16),
        (linux + 0x10_0000, 16),
        (linux - 4, 8),
        (0x1000_0000, 4),
        (0xffff_fff0, 32),
        (linux + linux_len, 16),
        (0xa300_0000, 16),
    ] {
        let write = memory.write(address, &vec![0; len]).unwrap_err();
        let read = memory.read(address, &mut vec![0; len]).unwrap_err();
        assert_eq!(
            (write.kind(), read.kind()),
            (
                io::ErrorKind::PermissionDenied,
                io::ErrorKind::PermissionDenied
            )
        );
    }
    assert_eq!(memory.0.0, []);
    for address in [0x2008_0000, 0x8000_0000, linux - 16] {
        memory.write(address, &[0; 16]).unwrap();
    }
    assert_eq!(memory.0.0.len(), 3);
}

fn bridge() -> CaptureBridge<FakeMemory, FakeStorage, FakeServices> {
    CaptureBridge::with_parts(
        Some(Parts {
            memory: FakeMemory,
            storage: FakeStorage,
            services: FakeServices,
            seed: TrainerSeed::from_random_device(1),
        }),
        None,
    )
}

fn requests(effects: &[Effect]) -> Vec<Request> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Dsp(DspCommand::Request(request)) => Some(request.clone()),
            _ => None,
        })
        .collect()
}

fn meter_reply(index: u16, value: f32) -> DspEvent {
    DspEvent::Meter(MeterRecord {
        core: 0,
        column: 0,
        index: i32::from(index),
        value_bits: value.to_bits(),
    })
}

#[test]
fn v1_without_seed_is_unsupported() {
    let result = Capture::new().command(Command::Open);
    assert!(matches!(result, Err(CaptureError::Unsupported(_))));
}

#[test]
fn v1_meter_tick_before_setup_requests_nothing() {
    let effects = bridge().on_meter_tick().unwrap();
    assert!(requests(&effects).is_empty());
}

#[test]
fn v1_save_outside_refine_is_refused() {
    let info = SaveInfo {
        name: "test".into(),
        device: 0,
        instrument: 0,
    };
    let mut library = FakeLibrary::default();
    let saved = bridge().save(info, &mut library);
    assert!(matches!(
        saved,
        Err(CaptureError::Session(SessionError::Refused(_)))
    ));
    assert!(library.hashes.is_empty());
}

#[test]
fn v1_unrequested_meter_is_ignored() {
    let mut bridge = bridge();
    bridge.command(Command::Open).unwrap();
    let effects = bridge.on_reply(&meter_reply(INPUT_LEVEL_1, 0.25)).unwrap();
    assert!(effects.is_empty());
}

/// Idle → setup (`enter()` in tetrad-capture v2): the two meter-timer effects become the bridge's
/// meter interval, `SetNCOutSel(1)` becomes the HAL write, the routes and the remote state are
/// still notices.
/// Idle → Setup (`0x202fd8` case 1): the routes go to the device as NC2 entries (no V1
/// gate), `SetNCOutSel(1)` to the HAL, and the remote state push to Cortex Control as one
/// NeuralCapture2 UPDATE field (`0x123244`, `control::nc2_message`).
#[test]
fn v2_idle_to_setup_routes_and_pushes_the_state_to_control() {
    let mut bridge = Capture::with_parts(None, Some("cloud.example".into()));
    let effects = bridge.nc2(Nc2Input::SetState(1)).unwrap();
    let routes: Vec<_> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::CaptureEntry(entry) if entry.state.is_none() => Some(entry.plan.clone()),
            _ => None,
        })
        .collect();
    let route = |route| vec![EntryAction::Nc2Route(route)];
    assert_eq!(
        routes,
        [
            route(Nc2Route::Reset),
            route(Nc2Route::SetupPrepare),
            route(Nc2Route::Setup),
            route(Nc2Route::AuditionDut)
        ]
    );
    let nc_output = Effect::Hal(HalCommand::nc_output(NcOutput::Capture));
    assert_eq!(
        effects
            .iter()
            .filter(|effect| **effect == nc_output)
            .count(),
        1
    );
    let push = Effect::ControlNotify(Box::new(nc2_message(Nc2Remote::State(Nc2State::Setup))));
    assert_eq!(effects.last(), Some(&push));
    assert_eq!(
        bridge.meter_interval(),
        Some(Duration::from_millis(u64::from(SETUP_METER_MS)))
    );
}

/// The NC2 setup meter (`Nc2Input::Meter`, tetrad-capture `v2.rs`): input-level replies reach the
/// controller while its meter timer runs, without a V1 session, and show as wizard levels.
#[test]
fn v2_setup_input_meter_reaches_the_controller() {
    let mut bridge = Capture::with_parts(None, Some("cloud.example".into()));
    bridge.nc2(Nc2Input::SetState(1)).unwrap();
    bridge.on_reply(&meter_reply(INPUT_LEVEL_1, 0.5)).unwrap();
    let levels = bridge.view().input_levels;
    assert!(levels[0] > 0.0 && levels[1] == 0.0, "{levels:?}");
}

#[test]
fn v2_without_domain_is_unsupported() {
    let result = Capture::new().nc2(Nc2Input::SetState(1));
    assert!(matches!(result, Err(CaptureError::Unsupported(_))));
}

fn target(core: DspCore, slot: u32) -> DeploymentTarget {
    DeploymentTarget {
        core,
        slot: ModuleSlot::try_from(slot).unwrap(),
    }
}

/// The recorder placement a deploy reports: node 1 (player) on core 0 slot 5.
fn recorder_placement() -> CapturePlacement {
    CapturePlacement::new([((0, 1), target(DspCore::Core0, 5))])
}

/// One stock call of the capture swap.
#[derive(Debug, Clone, PartialEq)]
enum Call {
    StoreCab,
    MasterMute {
        mute: bool,
        flag: bool,
    },
    Metronome(bool),
    RestoreMetronome,
    LaneStates,
    Lane(usize, u32, f32),
    OutputGain(u8, f32),
    RestoreMonitorGain,
    Deploy(CapturePreset, bool),
    InputOverride(bool),
    ReloadUser,
    /// `reload_user` with the momentary release armed (`+0xf031`).
    ReleaseArmed,
    Release(u8),
    Nc2(EntryAction),
    Sleep(Duration),
    Gap,
}

/// Records the calls. The user preset has row 1 muted and row 2 soloed.
#[derive(Default)]
struct RecordingDevice {
    calls: Vec<Call>,
    /// `reload_user` fails (the user preset does not deploy).
    failing_reload: bool,
    /// The held-stomp map `+0xf034` the revert reads.
    held: Vec<HeldStomp>,
    /// An update install holds the master mute.
    install: bool,
}

impl EntryDevice for RecordingDevice {
    fn store_cab(&mut self) -> Result<(), DeviceError> {
        self.calls.push(Call::StoreCab);
        Ok(())
    }
    fn master_mute(&mut self, mute: bool, flag: bool) -> Result<(), DeviceError> {
        self.calls.push(Call::MasterMute { mute, flag });
        Ok(())
    }
    fn install_hold(&self) -> bool {
        self.install
    }
    fn metronome(&mut self, on: bool) -> Result<(), DeviceError> {
        self.calls.push(Call::Metronome(on));
        Ok(())
    }
    fn restore_metronome(&mut self) -> Result<(), DeviceError> {
        self.calls.push(Call::RestoreMetronome);
        Ok(())
    }
    fn lane_states(&mut self) -> Result<[LaneState; 4], DeviceError> {
        self.calls.push(Call::LaneStates);
        let mut lanes = [LaneState::default(); 4];
        lanes[1].mute = true;
        lanes[2].solo = true;
        Ok(lanes)
    }
    fn lane_edit(&mut self, row: Row, parameter: u32, value: f32) -> Result<(), DeviceError> {
        self.calls.push(Call::Lane(row.index(), parameter, value));
        Ok(())
    }
    fn output_gain(&mut self, output: u8, gain: f32) -> Result<(), DeviceError> {
        self.calls.push(Call::OutputGain(output, gain));
        Ok(())
    }
    fn restore_monitor_gain(&mut self) -> Result<(), DeviceError> {
        self.calls.push(Call::RestoreMonitorGain);
        Ok(())
    }
    fn deploy(
        &mut self,
        preset: CapturePreset,
        locked: bool,
    ) -> Result<CapturePlacement, DeviceError> {
        self.calls.push(Call::Deploy(preset, locked));
        Ok(recorder_placement())
    }
    fn input_override(&mut self, on: bool) -> Result<(), DeviceError> {
        self.calls.push(Call::InputOverride(on));
        Ok(())
    }
    fn reload_user(&mut self, release_armed: bool) -> Result<(), DeviceError> {
        if release_armed {
            self.calls.push(Call::ReleaseArmed);
        }
        self.calls.push(Call::ReloadUser);
        if self.failing_reload {
            return Err(DeviceError::Unavailable("the user preset"));
        }
        Ok(())
    }
    fn held_stomps(&mut self) -> Vec<HeldStomp> {
        self.held.clone()
    }
    fn release_stomp(&mut self, stomp: StompIndex) -> Result<(), DeviceError> {
        self.calls.push(Call::Release(stomp.get()));
        Ok(())
    }
    fn nc2(&mut self, action: EntryAction) -> Result<(), DeviceError> {
        self.calls.push(Call::Nc2(action));
        Ok(())
    }
    fn sleep(&mut self, duration: Duration) {
        self.calls.push(Call::Sleep(duration));
    }
    fn gap(&mut self, _step: &'static str) {
        self.calls.push(Call::Gap);
    }
}

/// A full V1 run through the stock state reactions (`0x528498`): open (0 → 1) swaps in the
/// locked NC_Recorder and clears lane MUTE/SOLO; setup unmutes; training and refining deploy
/// their presets; close (12 → 0) reverts in stock order and restores the saved lanes.
#[test]
fn entry_runs_the_capture_swap_in_stock_order() {
    use Call::*;
    let mut controller = DspController::default();
    let mut swap = Swap::default();
    let mut device = RecordingDevice::default();
    let mut placements = Vec::new();
    for state in [
        NcState::Initial,
        NcState::MicSetup,
        NcState::Calibrate,
        NcState::Training,
        NcState::AutoRefine,
        NcState::Inactive,
    ] {
        let plan = controller.enter(state);
        placements.push(run_entry(&plan, Hardware::QuadCortex, &mut swap, &mut device).unwrap());
    }
    assert_eq!(placements[0], Some(recorder_placement()));
    assert_eq!(placements[1], None);

    let lanes = |values: [(f32, f32); 4]| {
        (0..4).flat_map(move |row| {
            let (mute, solo) = values[row];
            [Lane(row, LANE_MUTE, mute), Lane(row, LANE_SOLO, solo)]
        })
    };
    let mut expected = vec![
        Metronome(false),
        LaneStates,
        OutputGain(4, 1.0),
        Deploy(CapturePreset::Nc(NcPreset::Recorder), true),
    ];
    expected.extend(lanes([(0.0, 0.0); 4]));
    expected.extend([
        InputOverride(true),
        // 1 → 6, 6 → 7: the recorder's cab is stored before each change.
        StoreCab,
        MasterMute {
            mute: false,
            flag: true,
        },
        StoreCab,
        // 7 → 11, 11 → 12 (the trainer stores no cab; the refiner unmutes without the flag).
        StoreCab,
        Deploy(CapturePreset::Nc(NcPreset::Trainer), true),
        MasterMute {
            mute: false,
            flag: true,
        },
        Deploy(CapturePreset::Nc(NcPreset::Refiner), true),
        MasterMute {
            mute: false,
            flag: false,
        },
        // 12 → 0.
        StoreCab,
        InputOverride(true),
        Sleep(OVERRIDE_SETTLE),
        ReloadUser,
        InputOverride(false),
        Sleep(OVERRIDE_SETTLE),
        RestoreMonitorGain,
    ]);
    expected.extend(lanes([(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (0.0, 0.0)]));
    expected.extend([
        MasterMute {
            mute: false,
            flag: false,
        },
        RestoreMetronome,
    ]);
    assert_eq!(device.calls, expected);
    assert_eq!((swap.active, swap.locked), (None, false));
}

/// Open emits one `CaptureEntry` and nothing else; the session waits at the entry gate. The
/// device's answer carries the recorder placement, so the player start and the state-1 tick
/// address the deployed node (core 0 slot 5).
#[test]
fn entry_gate_waits_for_the_device_and_uses_its_placement() {
    let mut bridge = CaptureBridge::with_entry(
        FakeMemory,
        FakeStorage,
        TrainerSeed::from_random_device(1),
        Hardware::QuadCortex,
        None,
    );
    let effects = bridge.command(Command::Open).unwrap();
    let entries: Vec<_> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::CaptureEntry(entry) => Some(entry),
            _ => None,
        })
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].state, Some(NcState::Initial));
    assert_eq!(
        entries[0].plan,
        DspController::default().enter(NcState::Initial)
    );
    assert!(requests(&effects).is_empty());

    let effects = bridge
        .entered(NcState::Initial, Ok(Some(recorder_placement())))
        .unwrap();
    assert!(requests(&effects).is_empty(), "waits for the player start");
    let started = DspEvent::ModelStart {
        core: 0,
        slot: 5,
        started: true,
    };
    let effects = bridge.on_reply(&started).unwrap();
    let sent = requests(&effects);
    assert!(
        sent.contains(&Request::ParameterChange {
            target: target(DspCore::Core0, 5),
            index: 2,
            value_bits: 1.0f32.to_bits(),
        }),
        "{sent:?}"
    );
}

fn entry_states(effects: &[Effect]) -> Vec<NcState> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::CaptureEntry(entry) => entry.state,
            _ => None,
        })
        .collect()
}

/// Review w5 R4: with the device entry services Close finishes its cleanup (`Quiesce` is
/// answered at once; the revert entry is already queued ahead of it), so the next Open
/// starts a new capture instead of `Busy`.
#[test]
fn close_then_open_starts_a_new_capture() {
    let mut bridge = CaptureBridge::with_entry(
        FakeMemory,
        FakeStorage,
        TrainerSeed::from_random_device(1),
        Hardware::QuadCortex,
        None,
    );
    bridge.command(Command::Open).unwrap();
    bridge
        .entered(NcState::Initial, Ok(Some(recorder_placement())))
        .unwrap();
    let closed = bridge.command(Command::Close).unwrap();
    assert_eq!(entry_states(&closed), [NcState::Inactive]);
    bridge.entered(NcState::Inactive, Ok(None)).unwrap();
    let reopened = bridge.command(Command::Open).unwrap();
    assert_eq!(entry_states(&reopened), [NcState::Initial]);
}

/// Review w5 R5: every stock revert call returns nothing (`0x528778`–`0x528c98`). A failed
/// user reload still releases the override, restores the monitor, the lanes, the unmute and
/// the metronome; the first error is reported.
#[test]
fn revert_runs_every_step_after_a_failed_reload() {
    use Call::*;
    let mut controller = DspController::default();
    let mut swap = Swap::default();
    let mut device = RecordingDevice {
        failing_reload: true,
        ..RecordingDevice::default()
    };
    let open = controller.enter(NcState::Initial);
    run_entry(&open, Hardware::QuadCortex, &mut swap, &mut device).unwrap();
    device.calls.clear();
    let close = controller.enter(NcState::Inactive);
    let error = run_entry(&close, Hardware::QuadCortex, &mut swap, &mut device).unwrap_err();
    assert!(matches!(error, DeviceError::Unavailable("the user preset")));
    let after: Vec<_> = device
        .calls
        .iter()
        .skip_while(|&call| *call != ReloadUser)
        .skip(1)
        .cloned()
        .collect();
    assert_eq!(
        after[..3],
        [
            InputOverride(false),
            Sleep(OVERRIDE_SETTLE),
            RestoreMonitorGain
        ]
    );
    assert!(after.contains(&Lane(1, LANE_MUTE, 1.0)));
    assert_eq!(
        after[after.len() - 2..],
        [
            MasterMute {
                mute: false,
                flag: false
            },
            RestoreMetronome
        ]
    );
    assert_eq!((swap.active, swap.locked), (None, false));
}

/// Review r15 R8: Cortex Control stays live during an update install, so a capture or NC2
/// flow can run. Its plan still runs, but no step changes the install mute: the setup
/// unmute, the NC2 unmute and the revert unmute do nothing.
#[test]
fn capture_flow_keeps_the_install_mute() {
    let mut controller = DspController::default();
    let mut swap = Swap::default();
    let mut device = RecordingDevice {
        install: true,
        ..RecordingDevice::default()
    };
    for state in [NcState::Initial, NcState::MicSetup, NcState::Inactive] {
        let plan = controller.enter(state);
        run_entry(&plan, Hardware::QuadCortex, &mut swap, &mut device).unwrap();
    }
    for plan in [&NC2_ENABLE[..], &[EntryAction::RevertNc2]] {
        run_entry(plan, Hardware::QuadCortex, &mut swap, &mut device).unwrap();
    }
    assert!(device.calls.contains(&Call::ReloadUser), "the flow ran");
    assert!(
        !device
            .calls
            .iter()
            .any(|call| matches!(call, Call::MasterMute { .. })),
        "{:?}",
        device.calls
    );
}

/// Review w5 R11: the revert takes the saved lanes. A later revert (after an entry that
/// failed before `SaveLaneStates`) writes no MUTE/SOLO of an old session.
#[test]
fn revert_restores_the_saved_lanes_once() {
    let mut swap = Swap::default();
    let mut device = RecordingDevice::default();
    let plan = [EntryAction::SaveLaneStates, EntryAction::RevertNormal];
    run_entry(&plan, Hardware::QuadCortex, &mut swap, &mut device).unwrap();
    assert!(device.calls.contains(&Call::Lane(1, LANE_MUTE, 1.0)));
    device.calls.clear();
    run_entry(
        &[EntryAction::RevertNormal],
        Hardware::QuadCortex,
        &mut swap,
        &mut device,
    )
    .unwrap();
    assert!(
        !device
            .calls
            .iter()
            .any(|call| matches!(call, Call::Lane(..)))
    );
}

/// Review w5 R6: a capture push reads no user expression and runs with the tuner engine off
/// (`+0x88d8` is 0 on the Neural Capture screen); the user reload keeps both.
#[test]
fn capture_pushes_take_no_user_expression_or_tuner_engine() {
    let specs = Specs::default();
    let options = CompileOptions {
        globals: Globals {
            input_gate: true,
            global_eq: true,
        },
        ddc: false,
    };
    let scene = SceneIndex::new(0).unwrap();
    let (deployment, _) = Deployment::new(Layout::default(), &specs, options, scene).unwrap();
    let settings = Settings::defaults(SettingsHardware::QuadCortex);
    let expression = Expression::default();
    let user = Context {
        expression: Some(&expression),
        tuner_active: true,
        tuner_muted: true,
        ..Context::new(&specs, &deployment, &settings)
    };
    let capture = swap_context(user, true, true, true);
    assert!(capture.expression.is_none());
    assert!(!capture.tuner_active);
    assert!(capture.tuner_muted && capture.capture_lock && capture.nopoll);
    let reload = swap_context(user, false, false, false);
    assert!(reload.expression.is_some() && reload.tuner_active);
}

/// Recovered stock DDL (ZenUI `0x00a453cc`, `0x00a4693c`, `0x00a45328`) at schema 410.1, as
/// in `device_boot.rs`.
const DDL: &str = "
CREATE TABLE default_param (model_id TEXT,param_id INT,value TEXT,exp INT,exp_trim_max REAL,exp_trim_min REAL,PRIMARY KEY (model_id, param_id));
CREATE TABLE default_exp_switch (model_id TEXT,exp_switch_type INT,exp_switch_inverted INT,exp_switch_latch_emulation INT,exp_switch_delay_ms INT,PRIMARY KEY (model_id));
CREATE TABLE product ( id TEXT, type INT, readonly INT, rawdata BLOB, metadata BLOB, PRIMARY KEY(id));
CREATE TABLE downloads_product_preset ( id TEXT, rawdata BLOB, metadata BLOB, PRIMARY KEY ( id ) );
CREATE TABLE downloads_product_nc ( id TEXT, hash TEXT, rawdata BLOB, metadata BLOB, PRIMARY KEY ( id ) );
CREATE INDEX downloads_product_nc_hash ON downloads_product_nc (hash);
CREATE TABLE folder ( id INTEGER PRIMARY KEY AUTOINCREMENT, pdType INT, folderType INT, name TEXT, parent INTEGER, readonly INT);
CREATE TABLE folder_content ( folderId INTEGER, productId TEXT, FOREIGN KEY(folderId) REFERENCES folder(id), PRIMARY KEY(folderId,productId));
CREATE TABLE nc_encryption ( nc_type TEXT, encryption INT, PRIMARY KEY ( nc_type ) );
CREATE TABLE pinned_models (hash TEXT);
CREATE TABLE model_presets ( id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, hash INT, expSwitchType INT, expSwitchInverted INT, expSwitchLatchEmulation INT, expSwitchDelayMs INT);
CREATE TABLE default_model_presets ( hash INT PRIMARY KEY, presetId INTEGER, isFactory INTEGER );
CREATE TABLE model_preset_parameters (presetId INT, paramIndex INT, value TEXT, exp INT, expTrimMax REAL, expTrimMin REAL, FOREIGN KEY (presetId) REFERENCES model_presets(id), PRIMARY KEY (presetId, paramIndex));
CREATE TABLE schema_version ( major INTEGER, minor INTEGER);
INSERT INTO schema_version VALUES (410, 1);
";

/// Stand-in for the device cipher (seed ‖ serial) and the DSP boot vector `0x27`.
fn device_cipher() -> DeviceCipher {
    DeviceCipher::caller_secret(b"invented device secret")
}
const NC2_VECTOR: &[u8] = b"invented boot vector";

fn encrypted(cipher: &DeviceCipher, plain: &[u8]) -> Vec<u8> {
    let mut bytes = plain.to_vec();
    cipher.apply(&mut bytes);
    bytes
}

/// Protobuf writer for the synthetic records.
#[derive(Default)]
struct Proto(Vec<u8>);

impl Proto {
    fn varint(mut self, field: u32, value: u64) -> Self {
        self.key(field, 0);
        self.raw(value);
        self
    }
    fn fixed(mut self, field: u32, bits: u32) -> Self {
        self.key(field, 5);
        self.0.extend(bits.to_le_bytes());
        self
    }
    fn bytes(mut self, field: u32, payload: &[u8]) -> Self {
        self.key(field, 2);
        self.raw(payload.len() as u64);
        self.0.extend(payload);
        self
    }
    fn floats(self, field: u32, values: &[f32]) -> Self {
        let packed: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.bytes(field, &packed)
    }
    fn key(&mut self, field: u32, wire: u64) {
        self.raw(u64::from(field) << 3 | wire);
    }
    fn raw(&mut self, mut value: u64) {
        while value >= 0x80 {
            self.0.push(value as u8 | 0x80);
            value >>= 7;
        }
        self.0.push(value as u8);
    }
}

/// `captureProto.Capture` with the stock 1→2→2→1 shape.
fn v1_capture() -> Vec<u8> {
    let sizes = [1u64, 2, 2, 1]
        .into_iter()
        .fold(Proto::default(), |proto, size| {
            let mut proto = proto;
            proto.raw(size);
            proto
        });
    let layer =
        |weights: &[f32], biases: &[f32]| Proto::default().floats(1, weights).floats(2, biases).0;
    Proto::default()
        .fixed(1, 48_000.0f32.to_bits())
        .varint(2, 512)
        .varint(3, 2048)
        .floats(4, &[0.25; 512])
        .floats(5, &[0.5; 2048])
        .fixed(6, 1.0f32.to_bits())
        .fixed(7, 1.0f32.to_bits())
        .varint(8, 3)
        .bytes(9, &sizes.0)
        .bytes(10, &layer(&[1.0, 2.0], &[0.0, 0.0]))
        .bytes(10, &layer(&[1.0, 0.0, 0.0, 1.0], &[0.0, 0.0]))
        .bytes(10, &layer(&[0.5, 0.5], &[0.0]))
        .fixed(11, 20.0f32.to_bits())
        .fixed(12, 0.7f32.to_bits())
        .0
}

/// `snng_obfuscate.ObfuscatedGraph` 0.1.0.
fn nc2_capture() -> Vec<u8> {
    let version = Proto::default().varint(2, 1).0;
    Proto::default()
        .bytes(1, &version)
        .fixed(3, 0x1234_5678)
        .bytes(4, b"opaque graph")
        .0
}

const V1_KEY: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const NC2_HASH: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";

/// A user V1 capture and a downloaded NC2 capture, both device-encrypted, in a fresh
/// `zenui.db` (store flag local), and its path.
fn capture_db(test: &str) -> (StockDb, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "tetrad-app-device-capture-{test}-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(DDL)
        .unwrap();
    let mut db = StockDb::open(&path, DbLimits::default(), false).unwrap();
    let device = device_cipher();
    let nc2 = DeviceCipher::caller_secret(NC2_VECTOR);
    db.transaction(|write| {
        write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
        write.put_product(&StockProduct {
            id: V1_KEY.into(),
            product_type: ProductType::Capture,
            read_only: false,
            raw_data: Some(encrypted(&device, &v1_capture())),
            metadata: Some(encrypted(&device, br#"{"name":"Plexi","version":"1"}"#)),
        })?;
        write.put_download(
            DownloadKind::Capture,
            &Download {
                id: "cloud-1".into(),
                hash: Some(NC2_HASH.into()),
                raw_data: Some(encrypted(&nc2, &nc2_capture())),
                metadata: Some(encrypted(&device, br#"{"name":"Cloud","version":"2"}"#)),
            },
        )
    })
    .unwrap();
    (db, path)
}

/// Capture keys are run-time inputs (`0x7ed2c8`): V1 records decrypt with the device cipher
/// and convert to runtime data (`0x6ec290`); NC2 records need the DSP boot vector `0x27`
/// and become identity ‖ data. "My Captures" lists user captures by decrypted name.
#[test]
fn capture_records_load_with_injected_keys() {
    let (db, path) = capture_db("records");
    let catalog = FactoryIrCatalog::new(Vec::new()).encode();
    let no_files = |file: DspFile<'_>| Err(format!("unexpected {file:?}"));
    let values = [V1_KEY, NC2_HASH];

    let mut library = MediaLibrary::new(&catalog, device_cipher()).unwrap();
    let (media, failures) = library.resolve(values, no_files, Some(&db));
    match media.captures.get(V1_KEY) {
        Some(CaptureData::V1(bytes)) => assert_eq!(bytes.len(), 0x2864),
        other => panic!("expected the V1 runtime data, got {other:?}"),
    }
    assert!(!media.captures.contains_key(NC2_HASH));
    assert!(
        failures.len() == 1 && failures[0].contains("0x27"),
        "{failures:?}"
    );

    let mut library = MediaLibrary::new(&catalog, device_cipher()).unwrap();
    library.set_nc2_key(NC2_VECTOR);
    let (media, failures) = library.resolve(values, no_files, Some(&db));
    assert!(failures.is_empty(), "{failures:?}");
    let mut blob = 0x1234_5678u32.to_le_bytes().to_vec();
    blob.extend(b"opaque graph");
    assert_eq!(
        media.captures.get(NC2_HASH),
        Some(&CaptureData::V2(blob.into()))
    );

    let (entries, failures) = library.entries(Some(&db));
    assert!(failures.is_empty(), "{failures:?}");
    let captures: Vec<_> = entries
        .iter()
        .filter(|entry| entry.folder == tetrad_ui::MediaFolder::MyCaptures)
        .map(|entry| (entry.name.as_str(), entry.value.as_str()))
        .collect();
    assert_eq!(captures, [("Plexi", V1_KEY)]);
    drop(db);
    let _ = std::fs::remove_file(path);
}

/// Royal Blood on the QC (2026-10-15): a capture the user stores lack resolves from the
/// read-only `factory.db` with the device cipher; without the factory store it does not.
#[test]
fn a_factory_capture_loads_from_factory_db() {
    let (db, path) = capture_db("factory");
    drop(db);
    let factory_path = path.with_extension("factory.db");
    std::fs::copy(&path, &factory_path).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("delete from product", [])
        .unwrap();
    let user = StockDb::open(&path, DbLimits::default(), false).unwrap();
    let catalog = FactoryIrCatalog::new(Vec::new()).encode();
    let no_files = |file: DspFile<'_>| Err(format!("unexpected {file:?}"));

    let mut library = MediaLibrary::new(&catalog, device_cipher()).unwrap();
    let (media, _) = library.resolve([V1_KEY], no_files, Some(&user));
    assert!(!media.captures.contains_key(V1_KEY));

    let mut library = MediaLibrary::new(&catalog, device_cipher()).unwrap();
    library.set_factory(StockDb::open_factory(&factory_path, DbLimits::default()).unwrap());
    let (media, failures) = library.resolve([V1_KEY], no_files, Some(&user));
    assert!(failures.is_empty(), "{failures:?}");
    assert!(matches!(
        media.captures.get(V1_KEY),
        Some(CaptureData::V1(_))
    ));
    drop(user);
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(factory_path);
}

/// Capture save into collection 2 (`0x76be70`): `0x6fa0c4` adds a device-encrypted type-2
/// record with the `0x7ee62c` case-2 metadata under "My Captures"; `0x6f824c` renames it.
/// The media library loads the record back as V1 runtime data.
#[test]
fn saved_capture_is_a_my_captures_record_the_media_library_loads() {
    let (mut db, path) = capture_db("save");
    let cipher = device_cipher();
    let model = v1_capture();
    let hash = "c3".repeat(32);
    let info = SaveInfo {
        name: "Tweed".into(),
        device: 3,
        instrument: 2,
    };
    let mut library = StockCaptures {
        db: &mut db,
        cipher: &cipher,
        platform: Hardware::QuadCortex.platform_name(),
        author: "player",
        now: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        destination: None,
    };
    assert!(!library.contains(&hash).unwrap());
    let record = CaptureRecord {
        info: &info,
        hash: hash.clone(),
        model: &model,
    };
    library.insert(&record).unwrap();
    assert!(library.contains(&hash).unwrap());
    let renamed = SaveInfo {
        name: "Tweed 2".into(),
        ..info.clone()
    };
    library.update(&hash, &renamed).unwrap();

    let product = db.view().product(&hash).unwrap().unwrap();
    let mut metadata = product.metadata.unwrap();
    cipher.apply(&mut metadata);
    let metadata: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
    for (key, value) in [
        ("name", "Tweed 2"),
        ("authorName", "player"),
        ("creator", "quad"),
        ("deviceType", "amp_and_cab"),
        ("instrumentType", "bass"),
        ("encryption", "Local"),
        ("version", "1"),
        ("date", "2023-11-14T22:13:20Z"),
        ("path", &format!("{hash}.cns")),
    ] {
        assert_eq!(metadata[key], value, "{key}");
    }

    let catalog = FactoryIrCatalog::new(Vec::new()).encode();
    let no_files = |file: DspFile<'_>| Err(format!("unexpected {file:?}"));
    let mut media = MediaLibrary::new(&catalog, device_cipher()).unwrap();
    let (resolved, failures) = media.resolve([hash.as_str()], no_files, Some(&db));
    assert!(failures.is_empty(), "{failures:?}");
    assert!(matches!(
        resolved.captures.get(&hash),
        Some(CaptureData::V1(_))
    ));
    let (entries, _) = media.entries(Some(&db));
    assert!(
        entries
            .iter()
            .any(|entry| entry.folder == tetrad_ui::MediaFolder::MyCaptures
                && entry.name == "Tweed 2"
                && entry.value == hash)
    );
    drop(db);
    let _ = std::fs::remove_file(path);
}

/// `0x3cea8c`: Back without an exported model waits for "Unsaved capture". The view shows
/// the prompt while the decision is held; CANCEL keeps the capture, OK discards it.
#[test]
fn back_waits_for_the_unsaved_capture_answer() {
    let mut bridge = CaptureBridge::with_entry(
        FakeMemory,
        FakeStorage,
        TrainerSeed::from_random_device(1),
        Hardware::QuadCortex,
        None,
    );
    bridge.command(Command::Open).unwrap();
    bridge
        .entered(NcState::Initial, Ok(Some(recorder_placement())))
        .unwrap();
    let answer = |allowed| ViewCommand::Capture {
        request: CaptureRequest::Discard(allowed),
    };
    bridge.command(Command::Back).unwrap();
    assert!(bridge.view().unsaved_prompt);
    assert!(bridge.on_ui(&answer(false)).unwrap().is_empty());
    assert!(!bridge.view().unsaved_prompt);

    bridge.command(Command::Back).unwrap();
    let discarded = bridge.on_ui(&answer(true)).unwrap();
    assert!(!bridge.view().unsaved_prompt);
    assert!(!discarded.is_empty(), "{discarded:?}");
}

/// The V1 dialog close (`0x3c9a04`, also run when both trainers disengage, gap G3): mute
/// `0x876470(1, 1)`, then state 0, whose revert unmutes. The capture output goes back to
/// normal only after state 6 switched it on (`0x3d2890`). STOP outside manual refine does
/// nothing (button `+0x164`, `0x3ceac8`).
#[test]
fn v1_close_mutes_then_leaves_the_capture_state() {
    let mut bridge = CaptureBridge::with_entry(
        FakeMemory,
        FakeStorage,
        TrainerSeed::from_random_device(1),
        Hardware::QuadCortex,
        None,
    );
    let request = |request| ViewCommand::Capture { request };
    bridge.command(Command::Open).unwrap();
    bridge
        .entered(NcState::Initial, Ok(Some(recorder_placement())))
        .unwrap();
    assert!(
        bridge
            .on_ui(&request(CaptureRequest::Stop))
            .unwrap()
            .is_empty()
    );

    let closed = bridge.on_ui(&request(CaptureRequest::Abort)).unwrap();
    assert_eq!(closed[0], Effect::Dsp(DspCommand::MasterMute(3)));
    assert!(!closed.contains(&Effect::Hal(HalCommand::nc_output(NcOutput::Normal))));
    assert_eq!(entry_states(&closed), [NcState::Inactive]);
}

/// NC2 job and product work goes to the cloud as effects: the job GET (`0x0020068c`), the
/// 1000 ms re-poll (`0x1fe630`) and the product download on success (`0x7061b0`).
#[test]
fn nc2_job_poll_and_download_are_cloud_requests() {
    let mut bridge = Capture::with_parts(None, Some("cloud.example".into()));
    bridge.nc2(Nc2Input::SetState(3)).unwrap();
    let job = Effect::Cloud(CloudRequest::Nc2Job(job_url("cloud.example", "job-1")));
    assert!(
        bridge
            .nc2(Nc2Input::SetJobId("job-1".into()))
            .unwrap()
            .contains(&job)
    );
    let pending = bridge
        .nc2(Nc2Input::JobReply(br#"{"status":"pending"}"#.to_vec()))
        .unwrap();
    assert_eq!(
        pending,
        [Effect::StartTimer {
            timer: Timer::Nc2Poll,
            after: Duration::from_millis(u64::from(POLL_RETRY_MS)),
        }]
    );
    assert_eq!(bridge.nc2(Nc2Input::PollDue).unwrap(), [job]);
    let done = bridge
        .nc2(Nc2Input::JobReply(
            br#"{"status":"success","productId":"p-1"}"#.to_vec(),
        ))
        .unwrap();
    assert_eq!(
        done,
        [Effect::Cloud(CloudRequest::Nc2Download("p-1".into()))]
    );
}

/// NC2 save (`0x1fef08`; record shape inferred): the payload stays under the NC2 key with
/// `version` "2", so the media library loads it back as the Capture 2 blob.
#[test]
fn nc2_save_stores_a_record_the_library_loads() {
    let (mut db, path) = capture_db("nc2-save");
    let cipher = device_cipher();
    let nc2 = DeviceCipher::caller_secret(NC2_VECTOR);
    let product = Nc2Product {
        product_id: "p-1".into(),
        payload: encrypted(&nc2, &nc2_capture()),
        blob: std::sync::Arc::from(&b"unused"[..]),
        info: SaveInfo {
            name: "Cloud Tweed".into(),
            device: 1,
            instrument: 1,
        },
    };
    StockCaptures {
        db: &mut db,
        cipher: &cipher,
        platform: Hardware::QuadCortex.platform_name(),
        author: "player",
        now: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        destination: None,
    }
    .insert_nc2(&product)
    .unwrap();

    let catalog = FactoryIrCatalog::new(Vec::new()).encode();
    let mut media = MediaLibrary::new(&catalog, device_cipher()).unwrap();
    media.set_nc2_key(NC2_VECTOR);
    let (entries, _) = media.entries(Some(&db));
    let saved = entries
        .iter()
        .find(|entry| entry.name == "Cloud Tweed")
        .expect("listed in My Captures");
    assert_eq!(saved.folder, tetrad_ui::MediaFolder::MyCaptures);
    let no_files = |file: DspFile<'_>| Err(format!("unexpected {file:?}"));
    let (resolved, failures) = media.resolve([saved.value.as_str()], no_files, Some(&db));
    assert!(failures.is_empty(), "{failures:?}");
    let mut blob = 0x1234_5678u32.to_le_bytes().to_vec();
    blob.extend(b"opaque graph");
    assert_eq!(
        resolved.captures.get(&saved.value),
        Some(&CaptureData::V2(blob.into()))
    );
    drop(db);
    let _ = std::fs::remove_file(path);
}

fn nc2_plans(effects: &[Effect]) -> Vec<Vec<EntryAction>> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::CaptureEntry(entry) if entry.state.is_none() => Some(entry.plan.clone()),
            _ => None,
        })
        .collect()
}

/// `showDialog` (`0x3d791c`) → NC2 open (`0x203af0`): `enableNC2(1)` first, then state 1
/// even when the controller is already there; `enableNC2` returns when `+0x75d` already
/// holds the value; the close (`0x203ca4`) ends with `enableNC2(0)`.
#[test]
fn nc2_open_enables_the_nc2_preset_once_and_close_reverts_it() {
    let mut bridge = Capture::with_parts(None, Some("cloud.example".into()));
    let opened = nc2_plans(&bridge.nc2(Nc2Input::Open).unwrap());
    assert_eq!(opened[0], NC2_ENABLE);
    assert_eq!(opened[1], [EntryAction::Nc2Route(Nc2Route::Reset)]);
    let again = nc2_plans(&bridge.nc2(Nc2Input::Open).unwrap());
    assert_eq!(again[0], [EntryAction::Nc2Route(Nc2Route::Reset)]);
    assert!(!again.iter().any(|plan| plan[..] == NC2_ENABLE));
    let closed = nc2_plans(&bridge.nc2(Nc2Input::Close).unwrap());
    assert_eq!(
        closed.last().map(Vec::as_slice),
        Some(&[EntryAction::RevertNc2][..])
    );
}

/// NC2 save (`0x1fef08`) ends with `newCaptureSaved()` (`0x9728ac`): the open dialog shows
/// "Neural Capture Saved." (`0x3d7fc8` → mode 3) until a state change (`0x3dcc2c`) or the close.
#[test]
fn nc2_save_shows_the_saved_mode_until_the_state_changes() {
    let mut bridge = Capture::with_parts(None, Some("cloud.example".into()));
    bridge.nc2(Nc2Input::Open).unwrap();
    let product = tetrad_capture::v2::Product {
        name: "Cloud Tweed".into(),
        device: 1,
        instrument: 1,
    };
    bridge.nc2(Nc2Input::Downloaded(Some(product))).unwrap();
    assert!(!bridge.nc2_saved());
    // The device holds no downloaded product here: the library write is a notice only.
    bridge.nc2(Nc2Input::Save).unwrap();
    assert!(bridge.nc2_saved());
    bridge.nc2(Nc2Input::SetState(1)).unwrap();
    assert!(!bridge.nc2_saved());
    bridge.nc2(Nc2Input::Close).unwrap();
    assert!(!bridge.nc2_saved());
}

/// `enableNC2` (`0x528de4`): on mutes with the flag, keeps the user lanes, holds the monitor
/// at unity, deploys the locked NC2 preset, clears lane MUTE/SOLO and unmutes without the
/// flag. Off reverts like state 0 but without the held-stomp steps and the metronome.
#[test]
fn nc2_enable_and_revert_follow_enable_nc2() {
    use Call::*;
    let mut swap = Swap::default();
    let mut device = RecordingDevice {
        held: vec![HeldStomp {
            stomp: StompIndex::new(0).unwrap(),
            down: true,
        }],
        ..RecordingDevice::default()
    };
    run_entry(&NC2_ENABLE, Hardware::QuadCortex, &mut swap, &mut device).unwrap();
    let mut expected = vec![
        MasterMute {
            mute: true,
            flag: true,
        },
        LaneStates,
        OutputGain(4, 1.0),
        Deploy(CapturePreset::Nc(NcPreset::Nc2), true),
    ];
    expected.extend((0..4).flat_map(|row| [Lane(row, LANE_MUTE, 0.0), Lane(row, LANE_SOLO, 0.0)]));
    expected.push(MasterMute {
        mute: false,
        flag: false,
    });
    assert_eq!(device.calls, expected);
    assert_eq!(swap.active, Some(CapturePreset::Nc(NcPreset::Nc2)));

    device.calls.clear();
    run_entry(
        &[EntryAction::RevertNc2],
        Hardware::QuadCortex,
        &mut swap,
        &mut device,
    )
    .unwrap();
    assert_eq!(
        device.calls[..6],
        [
            InputOverride(true),
            Sleep(OVERRIDE_SETTLE),
            ReloadUser,
            InputOverride(false),
            Sleep(OVERRIDE_SETTLE),
            RestoreMonitorGain
        ]
    );
    assert_eq!(
        device.calls.last(),
        Some(&MasterMute {
            mute: false,
            flag: false
        })
    );
    assert!(device.calls.contains(&Lane(1, LANE_MUTE, 1.0)));
    assert_eq!((swap.active, swap.locked), (None, false));
}

/// The state-0 revert (`0x528498` case 0): a held stomp whose switch is down arms the
/// momentary release across the reload (`+0xf031`); after the unmute the held stomps whose
/// switch is up are released (`0x524358` → `0x523f80`), then the metronome comes back.
#[test]
fn capture_revert_releases_held_stomps_whose_switch_is_up() {
    use Call::*;
    let held = |stomp, down| HeldStomp {
        stomp: StompIndex::new(stomp).unwrap(),
        down,
    };
    let mut swap = Swap::default();
    let mut device = RecordingDevice {
        held: vec![held(1, true), held(2, false)],
        ..RecordingDevice::default()
    };
    run_entry(
        &[EntryAction::RevertNormal],
        Hardware::QuadCortex,
        &mut swap,
        &mut device,
    )
    .unwrap();
    let reload = device
        .calls
        .iter()
        .position(|call| *call == ReloadUser)
        .unwrap();
    assert_eq!(device.calls[reload - 1], ReleaseArmed);
    assert_eq!(
        device.calls[device.calls.len() - 3..],
        [
            MasterMute {
                mute: false,
                flag: false
            },
            Release(2),
            RestoreMetronome
        ]
    );
}

/// Invented catalog with the NC2 preset models: player 14001, send 13000, cab 12001.
const NC2_MODELS: &str = "<Models>\
<Category id='10' name='Splitter' hidden='true'><Model id='10004' name='Splitter'>\
<Parameter name='TYPE' type='switch' min='0' max='2' defaultValue='0'/></Model></Category>\
<Category id='11' name='Mixer' hidden='true'><Model id='11000' name='Mixer'>\
<Parameter name='LEVEL A' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/>\
</Model></Category>\
<Category id='12' name='Cab'><Model id='12001' name='Cab'>\
<Parameter name='LEVEL' type='float' min='0' max='10' defaultValue='5'/></Model></Category>\
<Category id='13' name='FX Loop'><Model id='13000' name='Send'>\
<Parameter name='LEVEL' type='float' min='0' max='1' defaultValue='0.5'/></Model></Category>\
<Category id='14' name='Capture'><Model id='14001' name='Capture 2'>\
<Parameter name='GAIN' type='float' min='0' max='1' defaultValue='0.5'/></Model></Category>\
<Category id='23' name='Lane Output' hidden='true'><Model id='23000' name='Lane Output'>\
<Parameter name='VOLUME' type='float' min='0' max='1' defaultValue='0.76923'/></Model></Category>\
<Category id='25' name='Tempo' hidden='true'><Model id='25000' name='Tempo'>\
<Parameter name='TEMPO' type='float' min='40' max='240' defaultValue='120'/></Model></Category>\
<Category id='28' name='Input Gate' hidden='true'><Model id='28000' name='Input Gate'>\
<Parameter name='THRESHOLD' type='float' min='-96' max='0' defaultValue='-60'/></Model></Category>\
</Models>";
const NC2_REPORT: &str = "<ModuleReport>\
<Kernel core='0' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='1' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
</ModuleReport>";

/// `nc2Preset` (`0x51b7e8`): lane ports (1→0x13, 2→0, 1→8, 8→0xb), the player at (0, 0),
/// the FX loop send at (0, 1) bypassed in every scene, row-0 points {-1, 2} without a
/// splitter edit, and the stored NC2 cab at (0, 3).
#[test]
fn nc2_preset_matches_the_stock_builder() {
    let specs =
        Specs::resolve(&tetrad_catalog::Catalog::parse(NC2_MODELS, NC2_REPORT).unwrap()).unwrap();
    let cab = CabBlock {
        model: tetrad_preset::Model::block(12001, [tetrad_preset::ParamValue::Float(7.0)]),
        bypass: [false; 8],
    };
    let mut preset = capture_preset(
        CapturePreset::Nc(NcPreset::Nc2),
        Hardware::QuadCortex,
        &specs,
        Some(cab),
    )
    .unwrap();
    let ports: Vec<_> = preset
        .chains
        .iter()
        .map(|chain| (chain.in_portid, chain.out_portid))
        .collect();
    assert_eq!(
        ports,
        [
            (Some(1), Some(0x13)),
            (Some(2), Some(0)),
            (Some(1), Some(8)),
            (Some(8), Some(0xb))
        ]
    );
    let points = &preset.chains[0].split_control_points[0];
    assert_eq!((points.split, points.mix), (-1, 2));
    let slot = |(row, column): (u8, u8)| tetrad_preset::Slot {
        row: usize::from(row),
        column: usize::from(column),
    };
    let model = |preset: &tetrad_preset::Preset, site| {
        preset
            .block(slot(site))
            .unwrap()
            .and_then(|block| block.hash)
    };
    assert_eq!(model(&preset, (0, 0)), Some(14001));
    assert_eq!(model(&preset, (0, 1)), Some(13000));
    assert_eq!(model(&preset, NC2_CAB_SITE), Some(12001));
    let bypassed = |preset: &mut tetrad_preset::Preset, site| {
        let bypass = preset.block_bypass_mut(slot(site)).unwrap();
        (0..8)
            .map(|scene| bypass.bypassed(scene).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(bypassed(&mut preset, (0, 1)), [true; 8]);
    assert_eq!(bypassed(&mut preset, (0, 0)), [false; 8]);
    assert_eq!(bypassed(&mut preset, NC2_CAB_SITE), [false; 8]);
}

/// NC2 routes (`0x1fec1c`, `0x1fea60`, `0x1fed6c`): Setup takes lane 2 from IN 1 to lane 0
/// out 0x13 with the stored cab bypass; Recording takes IN 8 to out 10, bypasses the cab,
/// sets lane 2 to -2.26 dB and the row-0 mixer; the next audition runs Setup once first.
#[test]
fn nc2_routes_follow_the_stock_tables() {
    let mut routing = Nc2Routing::default();
    let hardware = Hardware::QuadCortex;
    let setup = routing.edits(Nc2Route::Setup, hardware, false);
    for edit in [
        GridEdit::InputPort { row: 2, port: 1 },
        GridEdit::OutputPort { row: 0, port: 0x13 },
        GridEdit::Bypass {
            site: NC2_CAB_SITE,
            bypassed: false,
        },
    ] {
        assert!(setup.contains(&edit), "{edit:?}");
    }
    assert_eq!(setup.len(), 11);
    let recording = routing.edits(Nc2Route::Recording, hardware, false);
    let volume = (37.74f32) / 52.0;
    for edit in [
        GridEdit::InputPort { row: 2, port: 8 },
        GridEdit::OutputPort { row: 0, port: 10 },
        GridEdit::Lane {
            row: 2,
            parameter: LANE_VOLUME,
            value: volume,
        },
        GridEdit::Mixer {
            row: 0,
            parameter: 0,
            value: 0.0,
        },
    ] {
        assert!(recording.contains(&edit), "{edit:?}");
    }
    let audition = routing.edits(Nc2Route::AuditionCapture, hardware, true);
    assert_eq!(audition[..11], routing_setup(true)[..]);
    assert_eq!(
        routing
            .edits(Nc2Route::AuditionCapture, hardware, true)
            .len(),
        2
    );
    assert_eq!(
        routing.edits(Nc2Route::Reset, hardware, true),
        [
            GridEdit::HeadphoneMix { mix: 1, level: 1.0 },
            GridEdit::HeadphoneMix { mix: 8, level: 0.0 }
        ]
    );
}

fn routing_setup(cab_bypass: bool) -> Vec<GridEdit> {
    Nc2Routing::default().edits(Nc2Route::Setup, Hardware::QuadCortex, cab_bypass)
}

/// The NC2 open's IO restore (`0x1ffe80`): each stored value goes to the device and is
/// echoed (impedance on its 1/8 step, `0x1ff190`); a later remote write of the same value
/// is deduplicated (`0x1ffbac`).
#[test]
fn nc2_io_restore_applies_echoes_and_dedups() {
    let mut bridge = Capture::with_parts(None, Some("cloud.example".into()));
    let restored = bridge
        .nc2(Nc2Input::RestoreIo(vec![(0, 0.5), (13, 0.9)]))
        .unwrap();
    assert_eq!(
        nc2_plans(&restored),
        [
            vec![EntryAction::Nc2Io {
                index: 0,
                value: 0.5
            }],
            vec![EntryAction::Nc2Io {
                index: 13,
                value: 0.9
            }]
        ]
    );
    let echo = |index, value| {
        Effect::ControlNotify(Box::new(nc2_message(Nc2Remote::Parameter { index, value })))
    };
    assert!(restored.contains(&echo(0, 0.5)));
    assert!(restored.contains(&echo(13, 0.875)));
    let repeat = bridge
        .nc2(Nc2Input::Parameter {
            index: 0,
            value: 0.5,
        })
        .unwrap();
    assert!(repeat.is_empty(), "{repeat:?}");
}

/// r16-media MR 6: a capture save lands in the saving destination
/// (`downloadDestinationFolderNC`, `saving_destination`) and in "My Captures" (`store_media`).
#[test]
fn capture_save_lands_in_the_saving_destination() {
    let (mut db, path) = capture_db("destination");
    let cipher = device_cipher();
    let model = v1_capture();
    let info = SaveInfo {
        name: "Bassman".into(),
        device: 1,
        instrument: 1,
    };
    let save = |db: &mut StockDb, hash: String, destination: Option<&str>| {
        StockCaptures {
            db,
            cipher: &cipher,
            platform: Hardware::QuadCortex.platform_name(),
            author: "player",
            now: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            destination,
        }
        .insert(&CaptureRecord {
            info: &info,
            hash,
            model: &model,
        })
        .unwrap();
    };
    save(&mut db, "d1".repeat(32), None);
    let root = db
        .view()
        .root_folder(ProductType::Capture)
        .unwrap()
        .unwrap();
    let folder =
        tetrad_library::stock::create_media_folder(&mut db, ProductType::Capture, root, "Amps")
            .unwrap();
    let key = (db.view().folders().unwrap().iter())
        .find(|candidate| candidate.id == folder)
        .and_then(tetrad_library::stock::media_folder_key)
        .unwrap();
    save(&mut db, "d2".repeat(32), Some(&key));
    let view = db.view();
    assert_eq!(view.folder_products(folder).unwrap(), ["d2".repeat(32)]);
    let in_root = view.folder_products(root).unwrap();
    assert!(in_root.contains(&"d1".repeat(32)) && in_root.contains(&"d2".repeat(32)));
    drop(db);
    let _ = std::fs::remove_file(path);
}

/// START FROM SCRATCH (`0x3cea8c`, button `+0x160`) in state 16 without a held model
/// (`+0x88`, `0x3331b8`) asks "Unsaved capture" before `setNcState(1)`.
#[test]
fn start_from_scratch_after_an_error_asks_before_restarting() {
    let mut bridge = CaptureBridge::with_entry(
        FakeMemory,
        FakeStorage,
        TrainerSeed::from_random_device(1),
        Hardware::QuadCortex,
        None,
    );
    bridge.command(Command::Open).unwrap();
    bridge
        .entered(NcState::Initial, Err(ServiceError::Failed))
        .unwrap();
    assert_eq!(bridge.view().step, tetrad_ui::CaptureStep::Failed);
    let restart = ViewCommand::Capture {
        request: CaptureRequest::StartFromScratch,
    };
    bridge.on_ui(&restart).unwrap();
    assert!(bridge.view().unsaved_prompt);
}

/// Review r15 R8 on the device (review r18 F10): during an update install Cortex Control can
/// still open NC2 (`enableNC2` `0x528de4` mutes, deploys, unmutes). The install mute holds:
/// no `0x20` unmutes and the last one is the held value. Without the `Device` wiring of
/// `EntryDevice::install_hold` the entry unmute sends 0.
#[test]
fn nc2_open_during_an_update_install_keeps_the_install_mute() {
    use tetrad_app::device::platform::Input;
    use tetrad_app::device::services::ServiceNotice;
    use tetrad_ui::{Screen, SettingsPage};
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let (mut device, log, _media) = common::boot(media);
    let steps = |device: &mut Device<common::Sim>| (0..10).for_each(|_| _ = device.step());
    common::connect_control(&mut device);
    device.platform().worker.set(true);
    (device.platform().inputs.borrow_mut()).push(Input::Service(ServiceNotice::Download(Ok(()))));
    device.push(Event::UiInput(UiInput::Show(Screen::Settings(
        SettingsPage::Updates,
    ))));
    steps(&mut device);
    common::tap_update_install(&mut device, steps);
    assert!(device.bridges().services.inputs_masked());

    let mark = log.borrow().requests.len();
    common::host_send(
        &mut device,
        tetrad_control::codecs::NeuralCapture2Message {
            action: Some(tetrad_control::codecs::MessageAction::Update.into()),
            open_dialog: Some(true),
            ..Default::default()
        },
    );
    steps(&mut device);
    assert!(
        device.bridges().capture.running().is_some(),
        "the NC2 preset runs"
    );
    const HELD: u32 = 1 | 1 << 1;
    let mutes: Vec<_> = (log.borrow().requests[mark..].iter())
        .filter(|sent| sent.kind() == 0x20)
        .filter_map(common::Sent::word)
        .collect();
    assert!(!mutes.contains(&0), "an unmute: {mutes:?}");
    assert_eq!(mutes.last(), Some(&HELD), "{mutes:?}");
}

/// NC2 open and close on the simulated QC: Cortex Control `open_dialog` (`0x1201bc` →
/// `showDialog` `0x3d791c` → `0x203af0`) pushes the NC2 preset (`0x51b7e8`) to the DSP with
/// the player, the bypassed FX loop send and the default capture cab; its routes post no
/// capture notice. The NC2 dialog shows; its ABORT THE PROCESS (`hideDialog` `0x3d7098` →
/// `0x203ca4`) reverts to the user preset and hides the dialog.
#[test]
fn nc2_dialog_opens_from_control_and_its_close_reverts_the_nc2_preset() {
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let (mut device, log, _media) = common::boot(media);
    common::connect_control(&mut device);
    let (mark, notices) = (log.borrow().requests.len(), log.borrow().notices.len());
    common::host_send(
        &mut device,
        tetrad_control::codecs::NeuralCapture2Message {
            action: Some(tetrad_control::codecs::MessageAction::Update.into()),
            open_dialog: Some(true),
            ..Default::default()
        },
    );
    // The sim has no cab tables and no impedance readings: those notices are Dsp and Input.
    let capture_notices = |from: usize| -> Vec<_> {
        (log.borrow().notices[from..].iter())
            .filter(|notice| notice.subsystem == tetrad_app::device::Subsystem::Capture)
            .cloned()
            .collect()
    };
    assert!(capture_notices(notices).is_empty());
    assert!(
        !log.borrow().kinds(mark).is_empty(),
        "the NC2 preset is pushed"
    );
    let running = device
        .bridges()
        .capture
        .running()
        .expect("the NC2 preset runs");
    let block = |(row, column): (u8, u8)| {
        let slot = tetrad_engine::Slot {
            row: tetrad_engine::Row::new(row).unwrap(),
            column: tetrad_engine::Column::new(column).unwrap(),
        };
        let block = running
            .layout()
            .instance(tetrad_engine::Site::Slot(slot))
            .unwrap();
        (block.model.0, block.bypass.at(running.scene()))
    };
    assert_eq!(block((0, 0)), (14001, false));
    assert_eq!(block((0, 1)), (13000, true));
    assert_eq!(block(NC2_CAB_SITE).0, 12001);

    let dialog = |device: &Device<_>| device.bridges().ui.view().snapshot().capture.nc2;
    assert_eq!(dialog(&device), Some(Nc2Dialog::InProgress));
    let (mark, notices) = (log.borrow().requests.len(), log.borrow().notices.len());
    let abort = common::target(&device.bridges().ui, Control::Nc2(Nc2Request::Close));
    for pressed in [true, false] {
        device.push(Event::UiInput(UiInput::Touch(TouchSample {
            pressed,
            ..abort
        })));
        (0..50).for_each(|_| _ = device.step());
    }
    assert!(capture_notices(notices).is_empty());
    assert!(device.bridges().capture.running().is_none(), "reverted");
    assert!(
        !log.borrow().kinds(mark).is_empty(),
        "the user preset is pushed"
    );
    assert_eq!(dialog(&device), None);
}

/// Census #11: a V1 session's remote updates go to Cortex Control as one NeuralCapture (36)
/// UPDATE field each (state sender `0x1a1e94`), not as notices.
#[test]
fn v1_remote_state_pushes_one_neural_capture_field() {
    use tetrad_app::device::control::nc1_message;
    use tetrad_capture::session::RemoteUpdate;
    let effects = bridge().command(Command::Open).unwrap();
    let push = |state: NcState| {
        Effect::ControlNotify(Box::new(nc1_message(RemoteUpdate::State(state as i32))))
    };
    assert!(effects.contains(&push(NcState::Initial)), "{effects:?}");
    assert!(!effects.iter().any(|effect| matches!(
        effect,
        Effect::Notice(_, text) if text.contains("no Cortex Control route")
    )));
}

/// Review 2 R5: a bring-up run without `--persist` refuses the NC2 save (Cortex Control
/// CREATE) before the controller moves on, as the V1 save. The run log says "refused"; the
/// refusal does not depend on the read-only `zenui.db` open.
#[test]
fn nc2_save_is_refused_in_bringup_without_persist() {
    use tetrad_app::device::bringup::{RunOptions, Stage};
    use tetrad_control::codecs::{MessageAction, NeuralCapture2Message};
    let root = std::env::temp_dir().join(format!("tetrad-app-nc2-refused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let log = std::rc::Rc::new(std::cell::RefCell::new(common::Log::default()));
    let sim = common::Sim::new(common::STOCK, &media, &log);
    let options = RunOptions::bringup(Stage::Full, Duration::from_secs(600), root.clone());
    let mut device = Device::start_with(sim, options).unwrap();
    common::connect_control(&mut device);
    let nc2 = |action: MessageAction, open: Option<bool>| NeuralCapture2Message {
        action: Some(action.into()),
        open_dialog: open,
        ..Default::default()
    };
    common::host_send(&mut device, nc2(MessageAction::Update, Some(true)));
    let notices = log.borrow().notices.len();
    common::host_send(&mut device, nc2(MessageAction::Create, None));
    (0..10).for_each(|_| _ = device.step());
    let details: Vec<String> = (log.borrow().notices[notices..].iter())
        .map(|notice| notice.detail.clone())
        .collect();
    assert_eq!(
        details,
        ["bring-up refused an NC2 save without --persist"],
        "the controller never sees the save"
    );
    drop(device);
    let _ = std::fs::remove_dir_all(&root);
}
